"""Offline production-writer source binding regression using synthetic PDFs."""

import hashlib
import json
import os
import threading
from pathlib import Path
from types import SimpleNamespace

import pymupdf
import pytest
from babeldoc import _rust_il as native
from babeldoc.rust_callback_bridge import CallbackRun
from babeldoc.rust_run_context import PriorityServiceDispatcher
from babeldoc.rust_writer_service import WriterFileService


class Stage:
    def __enter__(self):
        return self

    def advance(self, amount):
        pass

    def __exit__(self, *args):
        pass


class Monitor:
    def stage_start(self, name, total):
        return Stage()


class Layout:
    characters = False

    def begin(self, inputs):
        pass

    def next_page(self):
        return None

    def close(self):
        pass

    def close_process_pool(self):
        pass


class Probe:
    def __init__(self, callback=None):
        self.callback = callback

    def probe(self):
        if self.callback:
            self.callback()
        return False


class ObservedWriter(WriterFileService):
    def __init__(self, config, source, replacement, translated_replacement, debug_replacement, fail_debug):
        super().__init__(config, {}, str(source))
        self.source = source
        self.replacement = replacement
        self.translated_replacement = translated_replacement
        self.debug_replacement = debug_replacement
        self.fail_debug = fail_debug
        self.opens = []
        self.translated_opens = []
        self.generated_text = None
        self.debug_digest = None
        self.subset_applied = False

    def replace_paths(self):
        translated = Path(self.config.working_dir) / "translated.pdf"
        generated_digest = hashlib.sha256(translated.read_bytes()).hexdigest()
        with pymupdf.open(translated) as document:
            self.generated_text = [page.get_text() for page in document]
        self.translated_replacement.replace(translated)
        self.replacement.replace(self.source)
        return generated_digest

    def open_translated(self, path):
        self.translated_opens.append(("path", self.replace_paths(), None))
        return super().open_translated(path)

    def open_translated_bytes(self, data):
        generated_digest = self.replace_paths()
        self.translated_opens.append(("bytes", generated_digest, hashlib.sha256(data).hexdigest()))
        return super().open_translated_bytes(data)

    def open_original(self, path):
        self.opens.append(("path", None))
        return super().open_original(path)

    def open_original_bytes(self, data):
        self.opens.append(("bytes", hashlib.sha256(data).hexdigest()))
        if len(self.opens) == 2:
            debug_path = Path(self.config.working_dir) / "debug.pdf"
            self.debug_digest = hashlib.sha256(debug_path.read_bytes()).hexdigest()
            self.debug_replacement.replace(debug_path)
        if self.fail_debug and len(self.opens) == 2:
            raise ValueError("synthetic debug failure")
        return super().open_original_bytes(data)

    def subset_mono(self):
        original = self.translated
        super().subset_mono()
        self.subset_applied = self.translated is not original


def make_pdf(path, marker, count):
    with pymupdf.open() as document:
        for index in range(count):
            page = document.new_page(width=300, height=400)
            page.insert_text((40, 60), f"{marker} {index}")
        document.save(path)


@pytest.mark.parametrize("debug,fail_debug,mismatch,skip_clean", [
    (False, False, False, True), (True, False, False, True),
    (True, True, False, True), (False, False, True, True),
    (False, False, False, False),
])
def test_bound_attempt_reuses_source_after_replacement(tmp_path, debug, fail_debug, mismatch, skip_clean):
    font = Path(os.environ.get("BABELDOC_TEST_FONT", Path.home() / ".cache/babeldoc/fonts/GoNotoKurrent-Regular.ttf"))
    assert font.is_file(), "Set BABELDOC_TEST_FONT to a local test font"
    source = tmp_path / "source.pdf"
    replacement = tmp_path / "replacement.pdf"
    translated_replacement = tmp_path / "translated-replacement.pdf"
    debug_replacement = tmp_path / "debug-replacement.pdf"
    make_pdf(source, "PUBLIC ORIGINAL", 1)
    make_pdf(replacement, "PUBLIC REPLACEMENT", 2)
    make_pdf(translated_replacement, "PUBLIC TRANSLATED REPLACEMENT", 3)
    make_pdf(debug_replacement, "PUBLIC DEBUG REPLACEMENT", 4)
    original_digest = hashlib.sha256(source.read_bytes()).hexdigest()
    family = {"normal": ["test"], "script": [], "fallback": ["test"], "base": ["test"]}
    mapper, styles = native.font_mapper_handle_with_source_styles(
        json.dumps(family), json.dumps({"test": str(font)}),
        json.dumps({"test": hashlib.sha3_256(font.read_bytes()).hexdigest()}), None,
    )
    shared = native.SharedContextCrossSplitPart([])
    config = SimpleNamespace(
        input_file=str(source), shared_context_cross_split_part=SimpleNamespace(),
        table_model=None, debug=False, only_parse_generate_pdf=False,
        skip_scanned_detection=True, skip_translation=True, auto_extract_glossary=False,
        ocr_workaround=False, should_translate_page=lambda _page: True,
        disable_rich_text_translate=False, min_text_length=1, pool_max_workers=1,
        lang_out="English", working_dir=str(tmp_path),
        get_working_file_path=lambda name: str(tmp_path / name),
    )
    first = PriorityServiceDispatcher(None, 1)
    second = PriorityServiceDispatcher(None, 1)
    context = native.DocumentRunContext(shared, mapper, first, second)
    writer = ObservedWriter(config, source, replacement, translated_replacement, debug_replacement, fail_debug)
    writer_config = {
        "source": str(source), "translated": str(tmp_path / "translated.pdf"),
        "debug_original": str(tmp_path / "debug.pdf"), "mono": str(tmp_path / "mono.pdf"),
        "dual": str(tmp_path / "dual.pdf"), "debug": debug, "skip_clean": skip_clean,
        "use_alternating_pages_dual": True,
    }
    stages = {"paragraph": {}, "styles": {}, "typesetting": {"lang_out": "en", "font_source_styles": styles}}
    assets = [{"id": "test", "metadata": {"font_name": "Test", "ascent": 0.9, "descent": -0.2, "encoding_length": 2}}]
    replace_before_attempt = (lambda: replacement.replace(source)) if mismatch else None
    callbacks = {"layout": (Layout(), 1), "probe_translate": (Probe(replace_before_attempt), 1),
                 "probe_terms": (Probe(), 1), "writer": (writer, 1)}
    try:
        with CallbackRun(callbacks, threading.Event(), native, record_requests=False) as run:
            first.cancel_flag = second.cancel_flag = run.flag
            parsed = native.parse_prepared_pdf_product(
                str(source), json.dumps({"pages": [0], "enable_graphic_element_process": True,
                                         "ocr_workaround": False, "visual_roundtrip_mode": False}),
                mapper, shared, run.flag, Monitor(), capture_source_sha256=True,
            )
            def run_part():
                return parsed.run_part(context, config, run.executors, json.dumps(stages), "{}",
                                       json.dumps(writer_config), json.dumps(assets), run.flag, Monitor(), False, "{}")

            if mismatch:
                with pytest.raises(OSError, match="writer source SHA-256 mismatch"):
                    run_part()
                assert not writer.translated_opens and not writer.opens
                assert not (tmp_path / "mono.pdf").exists()
                assert not (tmp_path / "dual.pdf").exists()
                return
            run_part()
        assert hashlib.sha256(source.read_bytes()).hexdigest() != original_digest
        assert len(writer.translated_opens) == 1
        assert writer.subset_applied is not skip_clean
        kind, generated_digest, opened_digest = writer.translated_opens[0]
        assert kind == "bytes" and opened_digest == generated_digest
        assert hashlib.sha256((tmp_path / "translated.pdf").read_bytes()).hexdigest() != generated_digest
        assert writer.opens[0] == ("bytes", original_digest)
        assert all(kind == "bytes" for kind, _ in writer.opens)
        assert len(writer.opens) == (3 if fail_debug else 2 if debug else 1)
        if fail_debug:
            assert writer.opens[-1] == ("bytes", original_digest)
        if debug and not fail_debug:
            assert writer.opens[1][1] == writer.debug_digest
            assert hashlib.sha256((tmp_path / "debug.pdf").read_bytes()).hexdigest() != writer.debug_digest
        with pymupdf.open(tmp_path / "dual.pdf") as dual:
            assert len(dual) == 2
            assert "PUBLIC ORIGINAL" in dual[0].get_text()
            assert "PUBLIC REPLACEMENT" not in dual[0].get_text()
            assert "PUBLIC TRANSLATED REPLACEMENT" not in dual[1].get_text()
            assert dual[1].get_text() == writer.generated_text[0]
        with pymupdf.open(tmp_path / "mono.pdf") as mono:
            assert len(mono) == 1
            assert "PUBLIC TRANSLATED REPLACEMENT" not in mono[0].get_text()
            assert mono[0].get_text() == writer.generated_text[0]
        if debug:
            with pymupdf.open(tmp_path / "dual.pdf.decompressed.pdf") as dual:
                assert len(dual) == 2
    finally:
        writer.close()
        first.close()
        second.close()
