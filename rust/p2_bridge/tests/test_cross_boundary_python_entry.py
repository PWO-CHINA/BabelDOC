import json
import logging
import shutil
from asyncio import CancelledError
from types import SimpleNamespace

import babeldoc
import pytest
from babeldoc.format.pdf import high_level
from babeldoc.format.pdf.translation_config import TranslateResult
from babeldoc.rust_translation_service import RustTranslatorService
from babeldoc.tools.executor.translator import ExecutorTranslator
from babeldoc.translator import cache as cache_module
from babeldoc.translator import translator as translator_module
from babeldoc.translator.translator import BaseTranslator
from babeldoc.translator.translator import OpenAITranslator


class LocalTranslator(BaseTranslator):
    name = "local-cross-test"

    def __init__(self, responses=None):
        super().__init__("en", "zh", False)
        self.responses = responses
        self.calls = []

    def do_translate(self, text, rate_limit_params=None):
        return self.do_llm_translate(text, rate_limit_params)

    def do_llm_translate(self, text, rate_limit_params=None):
        self.calls.append((text, rate_limit_params))
        if self.responses is not None:
            return next(self.responses)
        return (
            "json answer"
            if (rate_limit_params or {}).get("request_json_mode")
            else "plain answer"
        )


@pytest.fixture(autouse=True)
def no_rate_wait(monkeypatch, tmp_path):
    monkeypatch.setattr(translator_module._translate_rate_limiter, "wait", lambda: None)
    database = cache_module.SqliteDatabase(tmp_path / "translation-cache.sqlite")
    with database.bind_ctx([cache_module._TranslationCache]):
        database.create_tables([cache_module._TranslationCache])
        yield
    database.close()


@pytest.mark.parametrize("method", ["translate", "llm_translate"])
def test_boundary_callback_bypasses_both_cache_operations(method):
    translator = LocalTranslator(iter(["{bad json", '{"ok": true}']))

    class ForbiddenCache:
        def get(self, text):
            pytest.fail("boundary request read the cache")

        def set(self, text, value):
            pytest.fail("boundary request wrote the cache")

    translator.cache = ForbiddenCache()
    service = RustTranslatorService(translator)
    request = getattr(service, method)
    params = {"cross_boundary_request": True, "request_json_mode": True}
    assert request("same prompt", params) == "{bad json"
    assert request("same prompt", params) == '{"ok": true}'
    assert len(translator.calls) == 2


def test_boundary_request_bypasses_a_preexisting_invalid_entry():
    translator = LocalTranslator(iter(['{"ok": true}', '{"ok": true}']))
    prompt = "previously cached boundary prompt"
    translator.cache.set(prompt, "{bad json")
    params = {"cross_boundary_request": True, "request_json_mode": True}
    service = RustTranslatorService(translator)
    assert service.llm_translate(prompt, params) == '{"ok": true}'
    assert service.llm_translate(prompt, params) == '{"ok": true}'
    assert translator.cache.get(prompt) == "{bad json"
    assert len(translator.calls) == 2


def test_json_and_plain_requests_use_separate_cache_entries():
    translator = LocalTranslator()
    prompt = "same prompt with distinct contracts"
    assert translator.llm_translate(prompt) == "plain answer"
    assert (
        translator.llm_translate(prompt, rate_limit_params={"request_json_mode": True})
        == "json answer"
    )
    assert translator.llm_translate(prompt) == "plain answer"
    assert (
        translator.llm_translate(prompt, rate_limit_params={"request_json_mode": True})
        == "json answer"
    )
    assert len(translator.calls) == 2
    assert translator.translate_cache_call_count == 2
    assert "request_json_mode" not in translator.cache.params


def test_json_cache_uses_parameters_updated_during_request():
    translator = LocalTranslator()

    def respond(_text, _rate_limit_params):
        translator.add_cache_impact_parameters("initialized", True)
        return "initialized answer"

    translator.do_llm_translate = respond
    params = {"request_json_mode": True}
    assert (
        translator.llm_translate("initialized prompt", rate_limit_params=params)
        == "initialized answer"
    )
    assert (
        translator.llm_translate("initialized prompt", rate_limit_params=params)
        == "initialized answer"
    )
    assert translator.translate_cache_call_count == 1


def test_endpoint_cache_identity_omits_credentials(monkeypatch):
    def make_client(base_url, http_client, **_kwargs):
        http_client.close()
        return SimpleNamespace(base_url=base_url or "https://api.example/v1/")

    monkeypatch.setattr(translator_module.openai, "OpenAI", make_client)
    first = OpenAITranslator(
        "en",
        "zh",
        "model",
        "https://alice:password@first.example/v1/?token=secret&api-version=v1",
        "key-one",
    )
    same_endpoint = OpenAITranslator(
        "en",
        "zh",
        "model",
        "https://bob:other@first.example/v1/?token=another&api-version=v1",
        "key-two",
    )
    other_endpoint = OpenAITranslator(
        "en", "zh", "model", "https://second.example/v1/", "key-three"
    )
    other_version = OpenAITranslator(
        "en", "zh", "model", "https://first.example/v1/?api-version=v2", "key-four"
    )
    first_identity = first.cache.params["base_url_sha256"]
    assert first_identity == same_endpoint.cache.params["base_url_sha256"]
    assert first_identity != other_endpoint.cache.params["base_url_sha256"]
    assert first_identity != other_version.cache.params["base_url_sha256"]
    first.cache.set("endpoint prompt", "first endpoint answer")
    assert same_endpoint.cache.get("endpoint prompt") == "first endpoint answer"
    assert other_endpoint.cache.get("endpoint prompt") is None
    cache_parameters = json.dumps(first.cache.params)
    for credential in ("alice", "password", "secret", "key-one"):
        assert credential not in cache_parameters


def test_executor_cache_separates_endpoints_without_network():
    first = ExecutorTranslator("en", "zh", "model", "https://first.example", "key")
    second = ExecutorTranslator("en", "zh", "model", "https://second.example", "key")
    try:
        first.do_llm_translate = lambda _text, _params: "first answer"
        second.do_llm_translate = lambda _text, _params: "second answer"
        assert first.llm_translate("executor endpoint prompt") == "first answer"
        assert second.llm_translate("executor endpoint prompt") == "second answer"
        assert first.llm_translate("executor endpoint prompt") == "first answer"
        assert second.llm_translate("executor endpoint prompt") == "second answer"
        assert (
            first.translate_cache_call_count == second.translate_cache_call_count == 1
        )
    finally:
        first._client.close()
        second._client.close()


def test_cache_error_log_does_not_include_requests_or_responses(caplog):
    translator = LocalTranslator(iter(["private translated text"]))

    class FailingCache:
        def get(self, text):
            raise ValueError(text)

        def set(self, text, value):
            raise ValueError(value)

    translator.cache = FailingCache()
    caplog.set_level(logging.DEBUG, logger=translator_module.__name__)
    assert translator.llm_translate("private source text") == "private translated text"
    assert "private source text" not in caplog.text
    assert "private translated text" not in caplog.text
    assert "ValueError" in caplog.text


class FakeMonitor:
    def __init__(self):
        self.errors = []
        self.finished_result = None

    def translate_done(self, result):
        self.finished_result = result

    def translate_error(self, error):
        self.errors.append(error)

    def raise_if_cancelled(self):
        pass

    def on_finish(self):
        pass


class FakeMemoryMonitor:
    peak_memory_usage = 0

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass


def prepare_entry(monkeypatch, tmp_path, *, enabled=True, split=True, skip=False):
    working = tmp_path / "working"
    output = tmp_path / "output"
    working.mkdir()
    output.mkdir()
    config = SimpleNamespace(
        input_file=tmp_path / "synthetic.pdf",
        lang_out="zh",
        debug=False,
        capture_stages=False,
        only_parse_generate_pdf=False,
        skip_translation=skip,
        experimental_cross_page_context=enabled,
        no_mono=True,
        no_dual=True,
        split_strategy=object() if split else None,
        shared_context_cross_split_part=SimpleNamespace(),
        deterministic_output=False,
        get_working_file_path=lambda name: working / name,
        get_output_file_path=lambda name: output / name,
        cleanup_temp_files=lambda: shutil.rmtree(working),
    )
    rust = SimpleNamespace(
        acquire_logging_run=lambda: None,
        flush_logging_run=lambda: None,
        stop_logging_run=lambda: None,
    )
    monkeypatch.setattr(babeldoc, "_rust_il", rust, raising=False)
    monkeypatch.setattr(high_level, "Document", lambda _path: object())
    monkeypatch.setattr(high_level, "check_metadata", lambda _document: None)
    monkeypatch.setattr(high_level, "MemoryMonitor", FakeMemoryMonitor)
    monkeypatch.setattr(
        high_level,
        "create_document_run_context",
        lambda _config: SimpleNamespace(close=lambda: None),
    )
    monkeypatch.setattr(high_level, "fix_cmap", lambda *_args: None)
    monkeypatch.setattr(high_level, "add_metadata", lambda *_args: None)
    monkeypatch.setattr(high_level, "migrate_toc", lambda *_args: None)
    return config, working, output, FakeMonitor()


def sample_report():
    return {
        "version": 2,
        "status": "needs_review",
        "stage": "translation",
        "decisions": [{"kind": "page", "result": "uncertain"}],
        "unresolved": [
            {
                "kind": "page",
                "reason": "uncertain",
                "locations": [{"page": 0, "paragraph": 2}, {"page": 1, "paragraph": 0}],
            }
        ],
    }


@pytest.mark.parametrize("status", ["complete", "needs_review"])
def test_single_document_report_survives_cleanup(monkeypatch, tmp_path, caplog, status):
    config, working, output, monitor = prepare_entry(monkeypatch, tmp_path, split=False)
    report = sample_report()
    if status == "complete":
        report.update(status="complete", decisions=[], unresolved=[])
    stale_report = working / "cross_boundary_report.json"
    stale_report.write_text("stale", encoding="utf-8")

    def translate_single(_part_monitor, part_config, _run_context):
        assert part_config is config
        assert not stale_report.exists()
        stale_report.write_text(json.dumps(report), encoding="utf-8")
        return TranslateResult(None, None)

    monkeypatch.setattr(high_level, "_do_translate_single", translate_single)
    monkeypatch.setattr(
        high_level, "SplitManager", lambda _config: pytest.fail("split manager called")
    )
    caplog.set_level(logging.WARNING, logger=high_level.__name__)
    result = high_level.do_translate(monitor, config)
    assert not working.exists()
    assert result is monitor.finished_result
    assert (
        result.cross_boundary_report_path
        == output / "synthetic.zh.cross_boundary_report.json"
    )
    persisted_report = json.loads(
        result.cross_boundary_report_path.read_text(encoding="utf-8")
    )
    assert persisted_report == {**report, "stage": "output"}
    assert result.cross_boundary_report_status == status
    assert result.cross_boundary_report_stage == "output"
    assert result.cross_boundary_unresolved == report["unresolved"]
    assert result.cross_boundary_unresolved_count == len(report["unresolved"])
    assert f"{status}, {len(report['unresolved'])} unresolved" in str(result)


def test_disabled_mode_keeps_ordinary_split_strategy(monkeypatch, tmp_path):
    config, working, output, monitor = prepare_entry(
        monkeypatch, tmp_path, enabled=False
    )
    calls = []
    monkeypatch.setattr(
        high_level,
        "SplitManager",
        lambda _config: SimpleNamespace(
            determine_split_points=lambda config: calls.append(config) or []
        ),
    )
    monkeypatch.setattr(
        high_level, "_do_translate_single", lambda *_args: TranslateResult(None, None)
    )
    result = high_level.do_translate(monitor, config)
    assert calls == [config]
    assert result.cross_boundary_report_path is None
    assert result.cross_boundary_report_stage is None
    assert result.cross_boundary_unresolved_count is None
    assert result.cross_boundary_unresolved is None
    assert not list(output.iterdir())


def test_enabled_mode_keeps_ordinary_split_strategy(monkeypatch, tmp_path):
    config, _working, _output, monitor = prepare_entry(monkeypatch, tmp_path)
    calls = []
    monkeypatch.setattr(high_level, "SplitManager", lambda _config: SimpleNamespace(
        determine_split_points=lambda config: calls.append(config) or []
    ))
    monkeypatch.setattr(high_level, "_do_translate_single", lambda *_args: TranslateResult(None, None))
    result = high_level.do_translate(monitor, config)
    assert calls == [config]
    assert result.cross_boundary_report_status == "needs_review"


def test_failed_writer_preserves_existing_boundary_report(monkeypatch, tmp_path):
    config, working, output, monitor = prepare_entry(monkeypatch, tmp_path, split=False)
    report = sample_report()

    def fail_after_translation(*_args):
        (working / "cross_boundary_report.json").write_text(
            json.dumps(report), encoding="utf-8"
        )
        raise RuntimeError("writer failed")

    monkeypatch.setattr(high_level, "_do_translate_single", fail_after_translation)
    with pytest.raises(RuntimeError, match="writer failed"):
        high_level.do_translate(monitor, config)
    assert not working.exists()
    persisted = output / "synthetic.zh.cross_boundary_report.json"
    assert json.loads(persisted.read_text(encoding="utf-8")) == {
        **report,
        "status": "failed",
        "reason": "RuntimeError: writer failed",
        "error_type": "RuntimeError",
    }
    assert monitor.errors


def test_existing_rust_failure_reason_and_locations_are_preserved(
    monkeypatch, tmp_path
):
    config, working, output, monitor = prepare_entry(monkeypatch, tmp_path, split=False)
    report = sample_report()
    report.update(status="failed", reason="Rust failure at page 1 paragraph 2")

    def fail_translation(*_args):
        (working / "cross_boundary_report.json").write_text(
            json.dumps(report), encoding="utf-8"
        )
        raise RuntimeError("translation failed")

    monkeypatch.setattr(high_level, "_do_translate_single", fail_translation)
    with pytest.raises(RuntimeError, match="translation failed"):
        high_level.do_translate(monitor, config)
    persisted = output / "synthetic.zh.cross_boundary_report.json"
    assert json.loads(persisted.read_text(encoding="utf-8")) == {
        **report,
        "error_type": "RuntimeError",
    }


def test_rust_failed_report_marks_details_unavailable(monkeypatch, tmp_path):
    config, working, output, monitor = prepare_entry(monkeypatch, tmp_path, split=False)
    report = sample_report()
    report.update(status="failed", reason="boundary failed")

    def translate_single(*_args):
        (working / "cross_boundary_report.json").write_text(
            json.dumps(report), encoding="utf-8"
        )
        return TranslateResult(None, None)

    monkeypatch.setattr(high_level, "_do_translate_single", translate_single)
    high_level.do_translate(monitor, config)
    persisted = output / "synthetic.zh.cross_boundary_report.json"
    failed = json.loads(persisted.read_text(encoding="utf-8"))
    assert failed["status"] == "needs_review"
    assert failed["details_available"] is False


def test_report_persistence_failure_retains_working_directory(
    monkeypatch, tmp_path, caplog
):
    config, working, output, monitor = prepare_entry(monkeypatch, tmp_path, split=False)

    def translate_single(*_args):
        (working / "cross_boundary_report.json").write_text(
            json.dumps(sample_report()), encoding="utf-8"
        )
        return TranslateResult(None, None)

    def fail_to_persist(*_args, **_kwargs):
        raise OSError("output unavailable")

    monkeypatch.setattr(high_level, "_do_translate_single", translate_single)
    monkeypatch.setattr(high_level, "_persist_cross_boundary_report", fail_to_persist)
    caplog.set_level(logging.WARNING, logger=high_level.__name__)
    high_level.do_translate(monitor, config)
    assert (working / "cross_boundary_report.json").exists()
    assert not list(output.iterdir())
    assert "Could not persist cross-boundary report" in caplog.text


def test_missing_fresh_report_is_flagged_instead_of_reusing_stale(monkeypatch, tmp_path):
    config, working, output, monitor = prepare_entry(monkeypatch, tmp_path, split=False)
    (working / "cross_boundary_report.json").write_text(
        json.dumps(sample_report()), encoding="utf-8"
    )
    monkeypatch.setattr(
        high_level, "_do_translate_single", lambda *_args: TranslateResult(None, None)
    )
    high_level.do_translate(monitor, config)
    report = json.loads(
        (output / "synthetic.zh.cross_boundary_report.json").read_text(encoding="utf-8")
    )
    assert report["status"] == "needs_review"
    assert report["stage"] == "output"
    assert report["details_available"] is False
    assert report["unresolved"][0]["reason"] == "enhancement report unavailable"


@pytest.mark.parametrize("alteration", ["version", "locations", "status"])
def test_malformed_report_is_flagged(monkeypatch, tmp_path, alteration):
    config, working, output, monitor = prepare_entry(monkeypatch, tmp_path, split=False)
    report = sample_report()
    if alteration == "version":
        report["version"] = 1
    elif alteration == "locations":
        report["unresolved"][0]["locations"][0]["page"] = -1
    else:
        report["status"] = "complete"

    def translate_single(*_args):
        (working / "cross_boundary_report.json").write_text(
            json.dumps(report), encoding="utf-8"
        )
        return TranslateResult(None, None)

    monkeypatch.setattr(high_level, "_do_translate_single", translate_single)
    high_level.do_translate(monitor, config)
    persisted = output / "synthetic.zh.cross_boundary_report.json"
    failure_report = json.loads(persisted.read_text(encoding="utf-8"))
    assert failure_report["status"] == "needs_review"
    assert failure_report["details_available"] is False
    assert failure_report["decisions"] == []


def test_report_is_not_marked_output_before_postprocessing(monkeypatch, tmp_path):
    config, working, output, monitor = prepare_entry(monkeypatch, tmp_path, split=False)
    report = sample_report()

    def translate_single(*_args):
        (working / "cross_boundary_report.json").write_text(
            json.dumps(report), encoding="utf-8"
        )
        return TranslateResult(None, None)

    def fail_postprocessing(*_args):
        assert not list(output.iterdir())
        raise ValueError("output failed")

    monkeypatch.setattr(high_level, "_do_translate_single", translate_single)
    monkeypatch.setattr(high_level, "fix_cmap", fail_postprocessing)
    with pytest.raises(ValueError, match="output failed"):
        high_level.do_translate(monitor, config)
    persisted = output / "synthetic.zh.cross_boundary_report.json"
    failed = json.loads(persisted.read_text(encoding="utf-8"))
    assert failed["status"] == "failed"
    assert failed["stage"] == "translation"
    assert failed["reason"] == "ValueError: output failed"
    assert failed["unresolved"] == report["unresolved"]


@pytest.mark.parametrize("after_translation", [False, True])
def test_cancellation_is_propagated_and_reported(
    monkeypatch, tmp_path, after_translation
):
    config, working, output, monitor = prepare_entry(monkeypatch, tmp_path, split=False)
    report = sample_report()

    def cancel_translation(*_args):
        if after_translation:
            (working / "cross_boundary_report.json").write_text(
                json.dumps(report), encoding="utf-8"
            )
        raise CancelledError()

    monkeypatch.setattr(high_level, "_do_translate_single", cancel_translation)
    with pytest.raises(CancelledError):
        high_level.do_translate(monitor, config)
    persisted = output / "synthetic.zh.cross_boundary_report.json"
    failed = json.loads(persisted.read_text(encoding="utf-8"))
    assert failed["status"] == "failed"
    assert failed["stage"] == "translation"
    assert failed["reason"] == "CancelledError"
    assert failed["unresolved"] == (report["unresolved"] if after_translation else [])
    assert not monitor.errors


def test_cancellation_is_not_replaced_by_cleanup_failure(monkeypatch, tmp_path):
    config, working, output, monitor = prepare_entry(monkeypatch, tmp_path, split=False)

    def cancel_translation(*_args):
        raise CancelledError()

    def fail_cleanup():
        raise RuntimeError("cleanup failed")

    monkeypatch.setattr(high_level, "_do_translate_single", cancel_translation)
    monkeypatch.setattr(monitor, "on_finish", fail_cleanup)
    with pytest.raises(CancelledError):
        high_level.do_translate(monitor, config)
    persisted = output / "synthetic.zh.cross_boundary_report.json"
    assert (
        json.loads(persisted.read_text(encoding="utf-8"))["reason"] == "CancelledError"
    )


def test_failure_before_report_retains_short_reason_and_location(monkeypatch, tmp_path):
    config, working, output, monitor = prepare_entry(monkeypatch, tmp_path, split=False)
    detail = "Boundary failed at page 2 paragraph 3: invalid JSON"

    def fail_translation(*_args):
        raise RuntimeError(detail)

    monkeypatch.setattr(high_level, "_do_translate_single", fail_translation)
    with pytest.raises(RuntimeError, match="Boundary failed"):
        high_level.do_translate(monitor, config)
    persisted = output / "synthetic.zh.cross_boundary_report.json"
    failed = json.loads(persisted.read_text(encoding="utf-8"))
    assert failed["status"] == "failed"
    assert failed["reason"] == f"RuntimeError: {detail}"
    assert failed["details_available"] is False


def test_failure_after_output_records_completed_stage(monkeypatch, tmp_path):
    config, working, output, monitor = prepare_entry(monkeypatch, tmp_path, split=False)

    def translate_single(*_args):
        (working / "cross_boundary_report.json").write_text(
            json.dumps(sample_report()), encoding="utf-8"
        )
        return TranslateResult(None, None)

    def fail_notification(_result):
        raise RuntimeError("notification failed")

    monkeypatch.setattr(high_level, "_do_translate_single", translate_single)
    monkeypatch.setattr(monitor, "translate_done", fail_notification)
    with pytest.raises(RuntimeError, match="notification failed"):
        high_level.do_translate(monitor, config)
    persisted = output / "synthetic.zh.cross_boundary_report.json"
    failed = json.loads(persisted.read_text(encoding="utf-8"))
    assert failed["status"] == "failed"
    assert failed["stage"] == "output"
    assert failed["reason"] == "RuntimeError: notification failed"


@pytest.mark.parametrize("failure_point", ["context", "monitor"])
def test_cleanup_failure_updates_already_persisted_report(
    monkeypatch, tmp_path, failure_point
):
    config, working, output, monitor = prepare_entry(monkeypatch, tmp_path, split=False)
    report = sample_report()

    def translate_single(*_args):
        (working / "cross_boundary_report.json").write_text(
            json.dumps(report), encoding="utf-8"
        )
        return TranslateResult(None, None)

    def fail_cleanup():
        raise OSError("cleanup failed")

    monkeypatch.setattr(high_level, "_do_translate_single", translate_single)
    if failure_point == "context":
        monkeypatch.setattr(
            high_level,
            "create_document_run_context",
            lambda _config: SimpleNamespace(close=fail_cleanup),
        )
    else:
        monkeypatch.setattr(monitor, "on_finish", fail_cleanup)
    with pytest.raises(OSError, match="cleanup failed"):
        high_level.do_translate(monitor, config)
    persisted = output / "synthetic.zh.cross_boundary_report.json"
    failed = json.loads(persisted.read_text(encoding="utf-8"))
    assert failed["status"] == "failed"
    assert failed["stage"] == "output"
    assert failed["reason"] == "OSError: cleanup failed"
    assert failed["unresolved"] == report["unresolved"]


@pytest.mark.parametrize("mode", ["skip", "only_parse"])
def test_skipped_translation_keeps_split_policy_and_needs_no_report(
    monkeypatch, tmp_path, mode
):
    config, working, output, monitor = prepare_entry(monkeypatch, tmp_path, skip=True)
    config.only_parse_generate_pdf = mode == "only_parse"
    config.skip_translation = mode == "skip"
    split_calls = []
    monkeypatch.setattr(
        high_level,
        "SplitManager",
        lambda _config: SimpleNamespace(
            determine_split_points=lambda config: split_calls.append(config) or []
        ),
    )
    monkeypatch.setattr(
        high_level, "_do_translate_single", lambda *_args: TranslateResult(None, None)
    )
    result = high_level.do_translate(monitor, config)
    assert split_calls == [config]
    assert result.cross_boundary_report_path is None
    assert result.cross_boundary_unresolved_count is None
    assert not list(output.iterdir())
