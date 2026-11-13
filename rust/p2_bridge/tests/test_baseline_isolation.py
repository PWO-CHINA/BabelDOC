import asyncio
import concurrent.futures
import hashlib
import json
import os
import socket
from pathlib import Path
from types import SimpleNamespace

import pymupdf
import pytest
from babeldoc import _rust_il as native
from babeldoc.assets import embedding_assets_metadata
from babeldoc.format.pdf import high_level
from babeldoc.format.pdf.split_manager import PageCountStrategy
from babeldoc.format.pdf.translation_config import TranslationConfig
from babeldoc.format.pdf.translation_config import WatermarkOutputMode
from babeldoc.rust_run_context import DocumentRunContext
from babeldoc.rust_run_context import PriorityServiceDispatcher
from test_cross_boundary_production import LEFT
from test_cross_boundary_production import RIGHT
from test_cross_boundary_production import SENTENCE
from test_cross_boundary_production import TRANSLATIONS
from test_cross_boundary_production import Layout
from test_cross_boundary_production import LocalWriter
from test_cross_boundary_production import Translator
from test_cross_boundary_production import compact
from test_cross_boundary_production import make_pdf


class OfflineTranslator(Translator):
    def __init__(self, mode):
        super().__init__("page")
        self.mode = mode
        self.baseline_requests = []
        self.boundary_requests = []

    def llm_translate(self, prompt, rate_limit_params=None):
        params = rate_limit_params or {}
        if not params.get("cross_boundary_request"):
            self.baseline_requests.append(prompt)
            if self.mode == "baseline_failure":
                raise ConnectionError("offline synthetic baseline failure")
        else:
            self.boundary_requests.append(prompt)
            if self.mode == "provider_failure" or (self.mode == "first_success" and len(self.boundary_requests) > 2):
                raise ConnectionError("offline synthetic provider failure")
        return super().llm_translate(prompt, rate_limit_params)


def create_context(config, font):
    family = {"normal": ["test"], "script": [], "fallback": ["test"], "base": ["test"]}
    mapper, styles = native.font_mapper_handle_with_source_styles(
        json.dumps(family), json.dumps({"test": str(font)}),
        json.dumps({"test": hashlib.sha3_256(font.read_bytes()).hexdigest()}), None,
    )
    shared = native.SharedContextCrossSplitPart([])
    config.shared_context_cross_split_part = shared
    context = DocumentRunContext(shared, mapper, PriorityServiceDispatcher(None, 1), PriorityServiceDispatcher(None, 1))
    context.font_family = family
    context.font_source_styles = styles
    return context


def run_async_production(tmp_path, monkeypatch, mode, **options):
    source_font = Path("C:/Windows/Fonts/arial.ttf")
    target_font = Path.home() / ".cache/babeldoc/fonts/GoNotoKurrent-Regular.ttf"
    if not source_font.is_file() or not target_font.is_file():
        pytest.skip("local test fonts unavailable")
    source = tmp_path / "source.pdf"
    make_pdf(source, "page", source_font)
    if options.pop("split", False):
        with pymupdf.open(source) as original, pymupdf.open() as combined:
            combined.insert_pdf(original)
            combined.insert_pdf(original)
            split_source = tmp_path / "split-source.pdf"
            combined.save(split_source)
        source = split_source
        options["split_strategy"] = PageCountStrategy(2)
        from babeldoc.format.pdf.document_il.backend.pdf_creater import PDFCreater
        monkeypatch.setattr(PDFCreater, "subset_fonts_in_subprocess", staticmethod(lambda pdf, *_args, **_kwargs: pdf))
        monkeypatch.setattr(PDFCreater, "save_pdf_with_timeout", staticmethod(lambda pdf, path, **_kwargs: pdf.save(path)))
    output = tmp_path / "output"
    output.mkdir()
    working = tmp_path / "working"
    working.mkdir()
    translator = options.pop("translator", OfflineTranslator(mode))
    config_options = {
        "no_dual": True, "min_text_length": 1, "pool_max_workers": 1, "skip_clean": True,
        "skip_scanned_detection": True, "auto_extract_glossary": False,
        "save_auto_extracted_glossary": False, "watermark_output_mode": WatermarkOutputMode.NoWatermark,
        "experimental_cross_page_context": mode != "off", "report_interval": 0.01,
    }
    config_options.update(options)
    config = TranslationConfig(
        translator=translator, input_file=source, lang_in="en", lang_out="Chinese",
        doc_layout_model=object(), output_dir=output, working_dir=working,
        **config_options,
    )
    metadata = {"font_name": "Arial Regular", "ascent": 0.9, "descent": -0.2, "encoding_length": 2}
    monkeypatch.setitem(embedding_assets_metadata.EMBEDDING_FONT_METADATA, "test", metadata)
    monkeypatch.setattr(high_level, "create_document_run_context", lambda cfg: create_context(cfg, target_font))
    monkeypatch.setattr(high_level, "LayoutService", lambda *_args: Layout())
    monkeypatch.setattr(high_level, "WriterFileService", LocalWriter)
    connect = socket.socket.connect

    def offline_connect(sock, address):
        if isinstance(address, tuple) and address[0] in ("127.0.0.1", "::1"):
            return connect(sock, address)
        pytest.fail("network access forbidden")

    monkeypatch.setattr(socket.socket, "connect", offline_connect)

    async def run():
        return [event async for event in high_level.async_translate(config)]

    return asyncio.run(run()), translator, config


@pytest.mark.parametrize("mode", ["off", "provider_failure", "success"])
def test_async_production_baseline_and_enhancement(tmp_path, monkeypatch, mode):
    events, translator, _config = run_async_production(tmp_path, monkeypatch, mode)
    assert events[-1]["type"] == "finish", events[-1]
    result = events[-1]["translate_result"]
    with pymupdf.open(result.mono_pdf_path) as pdf:
        assert len(pdf) == 2
        text = compact("".join(page.get_text() for page in pdf))
    assert translator.baseline_requests
    if mode == "success":
        assert TRANSLATIONS[SENTENCE] in text
        report = json.loads(result.cross_boundary_report_path.read_text(encoding="utf-8"))
        assert report["candidate_state"] == "applied"
        assert report["applied_groups"] == 1
    else:
        assert TRANSLATIONS[LEFT] in text and TRANSLATIONS[RIGHT] in text
        if mode == "provider_failure":
            report = json.loads(result.cross_boundary_report_path.read_text(encoding="utf-8"))
            assert report["status"] == "needs_review"
            assert report["applied_groups"] == 0


def test_async_baseline_error_is_terminal(tmp_path, monkeypatch):
    events, translator, config = run_async_production(tmp_path, monkeypatch, "baseline_failure")
    assert events[-1]["type"] == "error"
    assert isinstance(events[-1]["error"], ConnectionError)
    assert translator.baseline_requests and not translator.boundary_requests
    assert not list(Path(config.output_dir).glob("*.pdf"))


def test_rejected_enhancement_keeps_default_translation_requests_and_output(tmp_path, monkeypatch):
    observed = []
    for mode in ("off", "provider_failure"):
        directory = tmp_path / mode
        directory.mkdir()
        with monkeypatch.context() as scoped:
            events, translator, _config = run_async_production(directory, scoped, mode)
        assert events[-1]["type"] == "finish", events[-1]
        result = events[-1]["translate_result"]
        with pymupdf.open(result.mono_pdf_path) as pdf:
            text = compact("".join(page.get_text() for page in pdf))
        assert TRANSLATIONS[LEFT] in text and TRANSLATIONS[RIGHT] in text
        observed.append((translator.baseline_requests, text))
    assert observed[0] == observed[1]


@pytest.mark.parametrize("auto,error", [
    (False, RuntimeError("term probe must not run")),
    (True, RuntimeError("optional term probe failed")),
    (True, asyncio.CancelledError()),
    (True, concurrent.futures.CancelledError()),
])
def test_async_term_probe_is_optional_and_cancellation_propagates(tmp_path, monkeypatch, auto, error):
    term = object()
    calls = []
    original = high_level.TranslatorProbeService

    class Probe(original):
        def probe(self):
            if self.translator is term:
                calls.append("term")
                raise error
            return super().probe()

    monkeypatch.setattr(high_level, "TranslatorProbeService", Probe)
    events, translator, config = run_async_production(
        tmp_path, monkeypatch, "provider_failure", auto_extract_glossary=auto,
        term_extraction_translator=term,
    )
    assert calls == (["term"] if auto else [])
    if auto and not isinstance(error, RuntimeError):
        assert events[-1]["type"] == "error"
        expected = asyncio.CancelledError if isinstance(error, concurrent.futures.CancelledError) else type(error)
        assert isinstance(events[-1]["error"], expected)
        assert not translator.baseline_requests
        assert not list(Path(config.output_dir).glob("*.pdf"))
    else:
        assert events[-1]["type"] == "finish"
        assert translator.baseline_requests


@pytest.mark.parametrize("mode", ["success", "provider_failure", "first_success"])
def test_async_split_keeps_translated_baseline_per_part(tmp_path, monkeypatch, mode):
    events, _translator, _config = run_async_production(tmp_path, monkeypatch, mode, split=True)
    assert events[-1]["type"] == "finish", events[-1]
    result = events[-1]["translate_result"]
    with pymupdf.open(result.mono_pdf_path) as pdf:
        assert len(pdf) == 4
        first = compact("".join(page.get_text() for page in pdf[:2]))
        second = compact("".join(page.get_text() for page in pdf[2:]))
    if mode in ("success", "first_success"):
        assert TRANSLATIONS[SENTENCE] in first
    else:
        assert TRANSLATIONS[LEFT] in first and TRANSLATIONS[RIGHT] in first
    if mode == "success":
        assert TRANSLATIONS[SENTENCE] in second
    else:
        assert TRANSLATIONS[LEFT] in second and TRANSLATIONS[RIGHT] in second
    report = json.loads(result.cross_boundary_report_path.read_text(encoding="utf-8"))
    assert report["applied_groups"] == {"success": 2, "provider_failure": 0, "first_success": 1}[mode]


def test_async_both_watermark_variants_publish_all_candidates(tmp_path, monkeypatch):
    events, _translator, _config = run_async_production(
        tmp_path, monkeypatch, "success", no_dual=False,
        watermark_output_mode=WatermarkOutputMode.Both,
    )
    assert events[-1]["type"] == "finish", events[-1]
    result = events[-1]["translate_result"]
    paths = {result.mono_pdf_path, result.dual_pdf_path, result.no_watermark_mono_pdf_path, result.no_watermark_dual_pdf_path}
    assert len(paths) == 4
    for path in paths:
        with pymupdf.open(path) as pdf:
            text = "".join(page.get_text() for page in pdf)
        translated = "".join(character for character in text
                             if "\u4e00" <= character <= "\u9fff" or character in "。，")
        assert TRANSLATIONS[SENTENCE] in translated


def test_async_non_llm_keeps_ordinary_translation(tmp_path, monkeypatch):
    class NonLLM(OfflineTranslator):
        def do_llm_translate(self, text, rate_limit_params=None):
            raise NotImplementedError()

        def translate(self, text, rate_limit_params=None):
            self.baseline_requests.append(text)
            for source, translated in TRANSLATIONS.items():
                text = text.replace(source, translated)
            return text

    events, translator, _config = run_async_production(
        tmp_path, monkeypatch, "provider_failure", translator=NonLLM("provider_failure"),
    )
    assert events[-1]["type"] == "finish", events[-1]
    result = events[-1]["translate_result"]
    with pymupdf.open(result.mono_pdf_path) as pdf:
        text = compact("".join(page.get_text() for page in pdf))
    assert translator.baseline_requests and not translator.boundary_requests
    assert TRANSLATIONS[LEFT] in text and TRANSLATIONS[RIGHT] in text
    report = json.loads(result.cross_boundary_report_path.read_text(encoding="utf-8"))
    assert report["candidate_state"] == "not_supported"


@pytest.mark.parametrize("error", [
    ValueError("candidate invalid"),
    OSError("candidate IO failed"),
    asyncio.CancelledError(),
    concurrent.futures.CancelledError(),
])
def test_async_candidate_postprocessing_error_classification(tmp_path, monkeypatch, error):
    postprocess = high_level._postprocess_outputs

    def fail_candidate(result, config):
        if ".boundary-candidate-" in str(result.mono_pdf_path):
            raise error
        return postprocess(result, config)

    monkeypatch.setattr(high_level, "_postprocess_outputs", fail_candidate)
    events, _translator, config = run_async_production(tmp_path, monkeypatch, "success")
    if isinstance(error, ValueError):
        assert events[-1]["type"] == "finish", events[-1]
        result = events[-1]["translate_result"]
        report = json.loads(result.cross_boundary_report_path.read_text(encoding="utf-8"))
        assert report["candidate_state"] == "fallback"
        assert report["applied_groups"] == 0
        path = result.mono_pdf_path
    else:
        assert events[-1]["type"] == "error"
        expected = asyncio.CancelledError if isinstance(error, concurrent.futures.CancelledError) else type(error)
        assert isinstance(events[-1]["error"], expected)
        path = next(Path(config.output_dir).glob("*.mono.pdf"))
    with pymupdf.open(path) as pdf:
        text = compact("".join(page.get_text() for page in pdf))
    assert TRANSLATIONS[LEFT] in text and TRANSLATIONS[RIGHT] in text
    assert TRANSLATIONS[SENTENCE] not in text


def test_progress_end_cancellation_keeps_document_and_tracking(tmp_path):
    from test_cross_page_llm_direct_atomicity import DirectApplicationService
    from test_cross_page_llm_direct_atomicity import Monitor
    from test_cross_page_llm_direct_atomicity import make_direct_document
    from test_cross_page_llm_direct_atomicity import run_bridge

    document = make_direct_document()
    before = document.parser_artifact_hash()
    tracking = tmp_path / "translate_tracking.json"
    tracking.write_bytes(b"prior tracking")
    flag = native.CancelFlag()
    with pytest.raises(asyncio.CancelledError):
        run_bridge(document, tmp_path, DirectApplicationService(), flag, Monitor(flag.cancel), False)
    assert document.parser_artifact_hash() == before
    assert tracking.read_bytes() == b"prior tracking"
    assert not list(tmp_path.glob("translate_tracking.json.*.tmp"))


def test_async_report_reset_failure_preserves_translated_baseline(tmp_path, monkeypatch):
    original_unlink = Path.unlink

    def fail_report_reset(path, *args, **kwargs):
        if path.name == "cross_boundary_report.json":
            raise OSError("injected report reset failure")
        return original_unlink(path, *args, **kwargs)

    monkeypatch.setattr(Path, "unlink", fail_report_reset)
    events, translator, _config = run_async_production(tmp_path, monkeypatch, "provider_failure")
    assert events[-1]["type"] == "finish", events[-1]
    with pymupdf.open(events[-1]["translate_result"].mono_pdf_path) as pdf:
        text = compact("".join(page.get_text() for page in pdf))
    assert translator.baseline_requests
    assert TRANSLATIONS[LEFT] in text and TRANSLATIONS[RIGHT] in text


def test_candidate_requires_every_baseline_output(tmp_path):
    from babeldoc.format.pdf.translation_config import TranslateResult

    baseline = TranslateResult(tmp_path / "mono.pdf", tmp_path / "dual.pdf")
    report = {"candidate_state": "written", "candidate_outputs": {"mono": str(tmp_path / "candidate.pdf")}}
    with pytest.raises(ValueError, match="missing dual"):
        high_level._candidate_result(baseline, report)


def test_candidate_cannot_alias_baseline(tmp_path):
    from babeldoc.format.pdf.translation_config import TranslateResult

    baseline = TranslateResult(tmp_path / "mono.pdf", None)
    report = {"candidate_state": "written", "candidate_outputs": {"mono": str(baseline.mono_pdf_path)}}
    with pytest.raises(ValueError, match="aliases"):
        high_level._candidate_result(baseline, report)


def test_report_reset_failure_does_not_reuse_stale_report(tmp_path, monkeypatch):
    report_path = tmp_path / "cross_boundary_report.json"
    report_path.write_text(json.dumps({
        "version": 2, "status": "complete", "stage": "translation",
        "decisions": [], "unresolved": [], "candidate_state": "written",
        "candidate_outputs": {"mono": "stale.pdf"},
    }), encoding="utf-8")
    original_unlink = Path.unlink

    def fail_unlink(path, *args, **kwargs):
        if path == report_path:
            raise OSError("injected report reset error")
        return original_unlink(path, *args, **kwargs)

    monkeypatch.setattr(Path, "unlink", fail_unlink)
    config = SimpleNamespace(get_working_file_path=lambda _name: report_path)
    assert high_level._reset_boundary_report(config) == report_path
    report = high_level._load_part_boundary_report(config)
    assert report["candidate_state"] == "unknown"
    assert report["status"] == "needs_review"


@pytest.mark.parametrize("cancel", [False, True])
def test_publish_cleanup_error_does_not_mask_outcome(tmp_path, monkeypatch, cancel):
    from babeldoc.format.pdf.translation_config import TranslateResult

    baseline = TranslateResult(tmp_path / "mono.pdf", None)
    candidate = TranslateResult(tmp_path / "candidate.pdf", None)
    baseline.mono_pdf_path.write_bytes(b"baseline")
    candidate.mono_pdf_path.write_bytes(b"candidate")
    original_unlink = Path.unlink

    def fail_backup_unlink(path, *args, **kwargs):
        if ".baseline-" in path.name:
            raise OSError("injected cleanup failure")
        return original_unlink(path, *args, **kwargs)

    checks = 0

    def check():
        nonlocal checks
        checks += 1
        if cancel and checks == 3:
            raise asyncio.CancelledError()

    monkeypatch.setattr(Path, "unlink", fail_backup_unlink)
    monitor = SimpleNamespace(raise_if_cancelled=check)
    if cancel:
        with pytest.raises(asyncio.CancelledError):
            high_level._publish_candidate_outputs(monitor, baseline, candidate)
        assert baseline.mono_pdf_path.read_bytes() == b"baseline"
    else:
        high_level._publish_candidate_outputs(monitor, baseline, candidate)
        assert baseline.mono_pdf_path.read_bytes() == b"candidate"


def test_publish_rolls_back_when_second_replace_fails(tmp_path, monkeypatch):
    from babeldoc.format.pdf.translation_config import TranslateResult

    baseline = TranslateResult(tmp_path / "mono.pdf", tmp_path / "dual.pdf")
    candidate = TranslateResult(tmp_path / "candidate-mono.pdf", tmp_path / "candidate-dual.pdf")
    for name, content in ((baseline.mono_pdf_path, b"baseline mono"), (baseline.dual_pdf_path, b"baseline dual"),
                          (candidate.mono_pdf_path, b"candidate mono"), (candidate.dual_pdf_path, b"candidate dual")):
        name.write_bytes(content)
    replace = os.replace
    attempts = []

    def fail_second(source, target):
        if ".candidate-" in str(source):
            attempts.append(target)
            if len(attempts) == 2:
                raise OSError("injected publish failure")
        return replace(source, target)

    monkeypatch.setattr(os, "replace", fail_second)
    with pytest.raises(OSError, match="injected publish failure"):
        high_level._publish_candidate_outputs(SimpleNamespace(raise_if_cancelled=lambda: None), baseline, candidate)
    assert baseline.mono_pdf_path.read_bytes() == b"baseline mono"
    assert baseline.dual_pdf_path.read_bytes() == b"baseline dual"


def test_cancel_during_publish_restores_baseline(tmp_path):
    from babeldoc.format.pdf.translation_config import TranslateResult

    baseline = TranslateResult(tmp_path / "mono.pdf", tmp_path / "dual.pdf")
    candidate = TranslateResult(tmp_path / "candidate-mono.pdf", tmp_path / "candidate-dual.pdf")
    for name, content in ((baseline.mono_pdf_path, b"baseline mono"), (baseline.dual_pdf_path, b"baseline dual"),
                          (candidate.mono_pdf_path, b"candidate mono"), (candidate.dual_pdf_path, b"candidate dual")):
        name.write_bytes(content)
    checks = 0

    def cancel():
        nonlocal checks
        checks += 1
        if checks == 3:
            raise asyncio.CancelledError()

    with pytest.raises(asyncio.CancelledError):
        high_level._publish_candidate_outputs(SimpleNamespace(raise_if_cancelled=cancel), baseline, candidate)
    assert baseline.mono_pdf_path.read_bytes() == b"baseline mono"
    assert baseline.dual_pdf_path.read_bytes() == b"baseline dual"


@pytest.mark.parametrize("error", [OSError("toc write failed"), concurrent.futures.CancelledError(), asyncio.CancelledError()])
def test_postprocess_toc_propagates_io_and_cancellation(tmp_path, monkeypatch, error):
    monkeypatch.setattr(high_level, "fix_cmap", lambda *_args: None)
    monkeypatch.setattr(high_level, "add_metadata", lambda *_args: None)

    def fail_toc(*_args):
        raise error

    monkeypatch.setattr(high_level, "migrate_toc", fail_toc)
    config = SimpleNamespace(input_file=tmp_path / "source.pdf", deterministic_output=False)
    with pytest.raises(type(error)) as raised:
        high_level._postprocess_outputs(SimpleNamespace(), config)
    assert raised.value is error


def test_postprocess_toc_non_io_error_remains_optional(tmp_path, monkeypatch):
    monkeypatch.setattr(high_level, "fix_cmap", lambda *_args: None)
    monkeypatch.setattr(high_level, "add_metadata", lambda *_args: None)

    def fail_toc(*_args):
        raise ValueError("invalid optional toc")

    monkeypatch.setattr(high_level, "migrate_toc", fail_toc)
    config = SimpleNamespace(input_file=tmp_path / "source.pdf", deterministic_output=False)
    high_level._postprocess_outputs(SimpleNamespace(), config)


@pytest.mark.parametrize("cancel", [False, True])
@pytest.mark.parametrize("has_report", [False, True])
def test_publish_rollback_failure_exposes_recovery_state(tmp_path, monkeypatch, cancel, has_report):
    from babeldoc.format.pdf.translation_config import TranslateResult

    baseline = TranslateResult(tmp_path / "mono.pdf", tmp_path / "dual.pdf")
    candidate = TranslateResult(tmp_path / "candidate-mono.pdf", tmp_path / "candidate-dual.pdf")
    for path, content in ((baseline.mono_pdf_path, b"baseline mono"), (baseline.dual_pdf_path, b"baseline dual"),
                          (candidate.mono_pdf_path, b"candidate mono"), (candidate.dual_pdf_path, b"candidate dual")):
        path.write_bytes(content)
    original_error = asyncio.CancelledError() if cancel else OSError("injected publish failure")
    restore_error = OSError("injected restoration failure")
    replace = os.replace

    def fail_restore(source, target):
        if ".baseline-" in str(source):
            raise restore_error
        if not cancel and ".candidate-" in str(source) and Path(target) == baseline.dual_pdf_path:
            raise original_error
        return replace(source, target)

    checks = 0

    def check_cancel():
        nonlocal checks
        checks += 1
        if cancel and checks == 3:
            raise original_error

    monkeypatch.setattr(os, "replace", fail_restore)
    with pytest.raises(OSError, match="baseline output restoration") as raised:
        high_level._publish_candidate_outputs(SimpleNamespace(raise_if_cancelled=check_cancel), baseline, candidate)
    error = raised.value
    assert isinstance(error, high_level.CandidateOutputRollbackError)
    assert error.original_error is original_error
    assert error.__cause__ is original_error
    assert len(error.restore_failures) == 1
    target, backup, failure = error.restore_failures[0]
    assert target == baseline.mono_pdf_path
    assert backup.read_bytes() == b"baseline mono"
    assert failure is restore_error
    assert baseline.mono_pdf_path.read_bytes() == b"candidate mono"
    assert baseline.dual_pdf_path.read_bytes() == b"baseline dual"
    source_report = tmp_path / "work-report.json"
    output_report = tmp_path / "output-report.json"
    if has_report:
        source_report.write_text(json.dumps({
            "version": 2, "status": "complete", "stage": "translation", "candidate_state": "written",
            "decisions": [{"kind": "page", "result": "prepared"}], "unresolved": [],
            "prepared_groups": [{"group": "g1", "locations": [{"page": 0, "paragraph": 0}]}],
        }), encoding="utf-8")
    config = SimpleNamespace(input_file=tmp_path / "source.pdf", lang_out="zh",
                             get_working_file_path=lambda _name: source_report,
                             get_output_file_path=lambda _name: output_report)
    assert high_level._persist_cross_boundary_report(config, error=error) is has_report
    report = json.loads(output_report.read_text(encoding="utf-8"))
    assert report["status"] == "failed"
    assert report["candidate_state"] == "rollback_failed"
    assert "candidate was not published" not in json.dumps(report)
    recovery = report["publication_failure"]
    assert recovery["original_error_type"] == type(original_error).__name__
    assert recovery["restore_failures"][0]["target"] == str(target)
    assert recovery["restore_failures"][0]["backup"] == str(backup)
    assert recovery["restore_failures"][0]["error_type"] == "OSError"
    if has_report:
        assert report["decisions"][0]["result"] == "unknown"
