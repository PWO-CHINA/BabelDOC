"""Offline writer callback cancellation and media-box regressions."""

import asyncio
import concurrent.futures
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
from test_writer_bound_e2e import Layout
from test_writer_bound_e2e import Monitor
from test_writer_bound_e2e import Probe
from test_writer_bound_e2e import make_pdf


@pytest.mark.parametrize("error_type", [
    OSError, concurrent.futures.CancelledError, asyncio.CancelledError, KeyboardInterrupt,
])
def test_restore_media_box_propagates_failures(error_type):
    service = WriterFileService(SimpleNamespace(), {12: {"MediaBox": "[0 0 10 10]"}}, "source.pdf")
    error = error_type("injected restoration failure")

    def fail(*_args):
        raise error

    service.translated = SimpleNamespace(xref_set_key=fail)
    with pytest.raises(error_type) as caught:
        service.restore_media_box()
    assert caught.value is error


def test_restore_media_box_restores_each_key():
    calls = []
    service = WriterFileService(SimpleNamespace(), {12: {"MediaBox": "[0 0 10 10]", "CropBox": "[0 0 5 5]"}}, "source.pdf")
    service.translated = SimpleNamespace(xref_set_key=lambda *args: calls.append(args))
    service.restore_media_box()
    assert calls == [(12, "MediaBox", "[0 0 10 10]"), (12, "CropBox", "[0 0 5 5]")]


def test_restore_media_box_updates_real_pdf():
    with pymupdf.open() as document:
        xref = document.new_page(width=100, height=100).xref
        boxes = {xref: {"MediaBox": "[0 0 200 300]", "CropBox": "[0 0 150 250]"}}
        service = WriterFileService(SimpleNamespace(), boxes, "source.pdf")
        service.translated = document
        service.restore_media_box()
        for name, expected in boxes[xref].items():
            assert document.xref_get_key(xref, name) == ("array", expected)


class InjectedWriter(WriterFileService):
    def __init__(self, config, source, point, error):
        super().__init__(config, {}, str(source))
        self.point = point
        self.error = error
        self.opens = 0
        self.original_opens = 0
        self.saves = 0
        self.restore_calls = 0
        self.closes = 0

    def open_translated_bytes(self, data):
        self.opens += 1
        return super().open_translated_bytes(data)

    def open_original_bytes(self, data):
        self.original_opens += 1
        if self.point == "debug" and self.original_opens == 2:
            raise self.error
        return super().open_original_bytes(data)

    def save_mono(self, path, garbage, clean):
        self.saves += 1
        if self.point == "save" and self.saves == 1:
            raise self.error
        return super().save_mono(path, garbage, clean)

    def restore_media_box(self):
        self.restore_calls += 1
        if self.point == "restore" and self.restore_calls == 1:
            self.mediabox_data = {12: {"MediaBox": "[0 0 10 10]"}}
            original = self.translated

            def fail(*_args):
                raise self.error

            self.translated = SimpleNamespace(xref_set_key=fail)
            try:
                return super().restore_media_box()
            finally:
                self.translated = original
                self.mediabox_data = {}
        return super().restore_media_box()

    def _save(self, pdf, path, garbage, clean, tag):
        pdf.save(path, garbage=garbage, clean=clean, deflate=True)

    def close(self):
        self.closes += 1
        super().close()


@pytest.mark.parametrize("point", ["save", "debug", "restore"])
@pytest.mark.parametrize("error_type", [
    concurrent.futures.CancelledError, asyncio.CancelledError, KeyboardInterrupt, ValueError,
])
def test_native_writer_does_not_retry_or_fallback_after_cancellation(tmp_path, point, error_type):
    font = Path(os.environ.get("BABELDOC_TEST_FONT", Path.home() / ".cache/babeldoc/fonts/GoNotoKurrent-Regular.ttf"))
    assert font.is_file(), "Set BABELDOC_TEST_FONT to a local test font"
    source = tmp_path / "source.pdf"
    make_pdf(source, "SOURCE", 1)
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
    error = error_type("injected writer failure")
    writer = InjectedWriter(config, source, point, error)
    writer_config = {
        "source": str(source), "translated": str(tmp_path / "translated.pdf"),
        "debug_original": str(tmp_path / "debug.pdf"), "mono": str(tmp_path / "mono.pdf"),
        "dual": str(tmp_path / "dual.pdf") if point == "debug" else None,
        "debug": point == "debug", "skip_clean": True, "use_alternating_pages_dual": True,
    }
    stages = {"paragraph": {}, "styles": {}, "typesetting": {"lang_out": "en", "font_source_styles": styles}}
    assets = [{"id": "test", "metadata": {"font_name": "Test", "ascent": 0.9, "descent": -0.2, "encoding_length": 2}}]
    callbacks = {"layout": (Layout(), 1), "probe_translate": (Probe(), 1),
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

            if error_type is ValueError:
                run_part()
                assert writer.opens == (1 if point == "debug" else 2)
                assert writer.saves == (2 if point == "save" else 1)
                assert writer.original_opens == (3 if point == "debug" else 0)
                assert writer.restore_calls == (2 if point == "restore" else 1 if point == "debug" else 2)
                assert (tmp_path / "mono.pdf").is_file()
                if point == "debug":
                    assert (tmp_path / "dual.pdf").is_file()
            else:
                with pytest.raises(error_type) as caught:
                    run_part()
                assert caught.value is error
                assert writer.opens == 1
                assert writer.saves == (0 if point == "restore" else 1)
                assert writer.restore_calls == 1
                assert writer.original_opens == (2 if point == "debug" else 0)
                assert not (tmp_path / "dual.pdf").exists()
            assert writer.closes == (2 if point != "debug" and error_type is ValueError else 1)
    finally:
        writer.close()
        first.close()
        second.close()
