"""Offline regressions for split-output completeness and cancellation."""

import asyncio
import concurrent.futures
from pathlib import Path
from types import SimpleNamespace

import babeldoc.format.pdf.result_merger as merger_module
import pymupdf
import pytest
from babeldoc.format.pdf.document_il.backend.pdf_creater import PDFCreater
from babeldoc.format.pdf.result_merger import ResultMerger
from babeldoc.format.pdf.translation_config import TranslateResult
from babeldoc.format.pdf.translation_config import WatermarkOutputMode


def config(tmp_path, **overrides):
    values = {
        "input_file": "source.pdf",
        "debug": False,
        "lang_out": "zh",
        "no_mono": False,
        "no_dual": False,
        "watermark_output_mode": WatermarkOutputMode.Both,
        "save_auto_extracted_glossary": False,
        "get_output_file_path": lambda name: tmp_path / name,
    }
    values.update(overrides)
    return SimpleNamespace(**values)


def result(name, distinct=False):
    value = TranslateResult(Path(f"{name}-mono.pdf"), Path(f"{name}-dual.pdf"))
    if distinct:
        value.no_watermark_mono_pdf_path = Path(f"{name}-plain-mono.pdf")
        value.no_watermark_dual_pdf_path = Path(f"{name}-plain-dual.pdf")
    return value


@pytest.mark.parametrize("tag", [
    "merged_mono", "merged_no_watermark_mono",
    "merged_dual", "merged_no_watermark_dual",
])
@pytest.mark.parametrize("error_type", [
    OSError, concurrent.futures.CancelledError, asyncio.CancelledError, KeyboardInterrupt,
])
def test_merge_propagates_failure_of_each_requested_output(tmp_path, tag, error_type):
    merger = ResultMerger(config(tmp_path))
    error = error_type("injected merge failure")
    calls = []

    def merge(paths, name, tag):
        calls.append((tag, paths))
        if tag == target_tag:
            raise error
        return tmp_path / name

    target_tag = tag
    merger._merge_pdfs = merge
    with pytest.raises(error_type) as caught:
        merger.merge_results({1: result("second"), 0: result("first", distinct=True)})
    assert caught.value is error
    assert calls[-1][0] == target_tag


@pytest.mark.parametrize("attribute", [
    "mono_pdf_path", "no_watermark_mono_pdf_path",
    "dual_pdf_path", "no_watermark_dual_pdf_path",
])
def test_merge_rejects_missing_part_output(tmp_path, attribute):
    first = result("first", distinct=True)
    second = result("second")
    setattr(second, attribute, None)
    merger = ResultMerger(config(tmp_path))
    def merge(_paths, name, **_kwargs):
        return tmp_path / name

    merger._merge_pdfs = merge

    with pytest.raises(OSError, match=f"missing {attribute} for part 1"):
        merger.merge_results({0: first, 1: second})


def test_merge_rejects_empty_output_from_merge_helper(tmp_path):
    merger = ResultMerger(config(tmp_path))
    def merge(_paths, _name, **_kwargs):
        return None

    merger._merge_pdfs = merge
    with pytest.raises(OSError, match="failed to merge mono_pdf_path"):
        merger.merge_results({0: result("first")})


def test_single_variant_preserves_alias_and_sorted_parts(tmp_path):
    merger = ResultMerger(config(tmp_path, no_dual=True, watermark_output_mode=WatermarkOutputMode.NoWatermark))
    calls = []

    def merge(paths, name, tag):
        calls.append((tag, paths))
        return tmp_path / name

    merger._merge_pdfs = merge
    merged = merger.merge_results({2: result("second"), 0: result("first"), 1: None})
    assert calls == [("merged_mono", [Path("first-mono.pdf"), Path("second-mono.pdf")])]
    assert merged.mono_pdf_path == merged.no_watermark_mono_pdf_path
    assert merged.dual_pdf_path is merged.no_watermark_dual_pdf_path is None


def test_only_parse_retains_single_variant(tmp_path):
    merger = ResultMerger(config(tmp_path, no_dual=True, only_parse_generate_pdf=True))
    calls = []
    merger._merge_pdfs = lambda _paths, name, tag: calls.append(tag) or tmp_path / name
    merged = merger.merge_results({0: result("first")})
    assert calls == ["merged_mono"]
    assert merged.mono_pdf_path == merged.no_watermark_mono_pdf_path


def test_dual_only_does_not_require_mono_parts(tmp_path):
    first = TranslateResult(None, Path("first-dual.pdf"))
    second = TranslateResult(None, Path("second-dual.pdf"))
    merger = ResultMerger(config(tmp_path, no_mono=True))
    def merge(_paths, name, **_kwargs):
        return tmp_path / name

    merger._merge_pdfs = merge
    merged = merger.merge_results({0: first, 1: second})
    assert merged.mono_pdf_path is merged.no_watermark_mono_pdf_path is None
    assert merged.dual_pdf_path != merged.no_watermark_dual_pdf_path


def test_no_present_parts_is_rejected(tmp_path):
    with pytest.raises(ValueError, match="No results to merge"):
        ResultMerger(config(tmp_path)).merge_results({0: None})


def test_both_mode_merges_identical_source_aliases_to_distinct_files(tmp_path, monkeypatch):
    paths = []
    for index in range(2):
        path = tmp_path / f"part-{index}.pdf"
        with pymupdf.open() as document:
            document.new_page().insert_text((30, 50), f"PART {index}")
            document.save(path)
        paths.append(path)

    def subset(document, _config, **_kwargs):
        return document

    def save(document, path, **_kwargs):
        document.save(path)

    monkeypatch.setattr(PDFCreater, "subset_fonts_in_subprocess", subset)
    monkeypatch.setattr(
        PDFCreater, "save_pdf_with_timeout",
        save,
    )
    merger = ResultMerger(config(tmp_path, no_dual=True))
    merged = merger.merge_results({1: TranslateResult(paths[1], None), 0: TranslateResult(paths[0], None)})

    assert merged.mono_pdf_path != merged.no_watermark_mono_pdf_path
    for path in (merged.mono_pdf_path, merged.no_watermark_mono_pdf_path):
        with pymupdf.open(path) as document:
            assert len(document) == 2
            assert [page.get_text().strip() for page in document] == ["PART 0", "PART 1"]


@pytest.mark.parametrize("operation", ["open_part", "insert", "subset", "save"])
@pytest.mark.parametrize("error_type", [
    OSError, concurrent.futures.CancelledError, asyncio.CancelledError,
])
def test_real_merge_pipeline_propagates_io_and_cancellation(tmp_path, monkeypatch, operation, error_type):
    error = error_type("injected merge pipeline failure")

    def fail():
        raise error

    class FakeDocument:
        def __init__(self, path=None):
            if path is not None and operation == "open_part":
                fail()

        def insert_pdf(self, _document):
            if operation == "insert":
                fail()

    def subset(document, _config, **_kwargs):
        if operation == "subset":
            fail()
        return document

    def save(_document, _path, **_kwargs):
        if operation == "save":
            fail()

    monkeypatch.setattr(merger_module, "Document", FakeDocument)
    monkeypatch.setattr(PDFCreater, "subset_fonts_in_subprocess", subset)
    monkeypatch.setattr(PDFCreater, "save_pdf_with_timeout", save)
    merger = ResultMerger(config(tmp_path, no_dual=True, watermark_output_mode=WatermarkOutputMode.NoWatermark))
    with pytest.raises(error_type) as caught:
        merger.merge_results({0: result("first")})
    assert caught.value is error
