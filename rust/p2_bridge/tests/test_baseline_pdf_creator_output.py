"""Offline regressions for PDF output failure and cancellation."""

import asyncio
import concurrent.futures
from pathlib import Path
from types import SimpleNamespace

import pymupdf
import pytest
from babeldoc.format.pdf.document_il.backend import pdf_creater
from babeldoc.format.pdf.document_il.backend.pdf_creater import PDFCreater


class CleanSaveProcess:
    exitcode = 0

    def __init__(self, target, args):
        self.output = Path(args[1])

    def start(self):
        self.output.write_bytes(b"clean")

    def join(self, _timeout):
        pass

    def is_alive(self):
        return False


@pytest.mark.parametrize("error_type", [
    concurrent.futures.CancelledError, asyncio.CancelledError, KeyboardInterrupt, OSError,
])
def test_clean_save_copy_failure_preserves_cancel_and_io_fallback(tmp_path, monkeypatch, error_type):
    output = tmp_path / "output.pdf"
    calls = []
    error = error_type("injected copy failure")

    def save(path, **_kwargs):
        calls.append(Path(path))
        Path(path).write_bytes(b"fallback")

    def fail_copy(*_args):
        raise error

    monkeypatch.setattr(pdf_creater, "Process", CleanSaveProcess)
    monkeypatch.setattr("shutil.copy2", fail_copy)
    config = SimpleNamespace(get_working_file_path=lambda name: tmp_path / name)
    pdf = SimpleNamespace(save=save)

    if error_type is OSError:
        assert PDFCreater.save_pdf_with_timeout(pdf, str(output), config) is False
        assert output.read_bytes() == b"fallback"
        assert calls[-1] == output
    else:
        with pytest.raises(error_type) as caught:
            PDFCreater.save_pdf_with_timeout(pdf, str(output), config)
        assert caught.value is error
        assert output not in calls
        assert not output.exists()
    assert not list(tmp_path.glob("temp_save_*.pdf"))


def make_pdf(text):
    document = pymupdf.open()
    document.new_page(width=200, height=200).insert_text((25, 40), text)
    return document


@pytest.mark.parametrize("first", [False, True])
def test_side_by_side_contains_both_pages(first):
    creator = PDFCreater.__new__(PDFCreater)
    config = SimpleNamespace(dual_translate_first=first, input_file="source.pdf")
    with make_pdf("ORIGINAL") as original, make_pdf("TRANSLATED") as translated:
        dual = creator.create_side_by_side_dual_pdf(original, translated, "dual.pdf", config)
        try:
            words = {word[4]: word[0] for word in dual[0].get_text("words")}
            assert set(words) == {"ORIGINAL", "TRANSLATED"}
            assert (words["TRANSLATED"] < words["ORIGINAL"]) == first
        finally:
            dual.close()


@pytest.mark.parametrize("failed_call", [1, 2])
@pytest.mark.parametrize("error_type", [
    OSError, ValueError, concurrent.futures.CancelledError, asyncio.CancelledError,
])
def test_side_by_side_page_failure_is_not_silently_dropped(monkeypatch, failed_call, error_type):
    creator = PDFCreater.__new__(PDFCreater)
    config = SimpleNamespace(dual_translate_first=False, input_file="source.pdf")
    error = error_type("injected page rendering failure")
    real_show = pymupdf.Page.show_pdf_page
    calls = []

    def show(page, *args, **kwargs):
        calls.append(args[1])
        if len(calls) == failed_call:
            raise error
        return real_show(page, *args, **kwargs)

    monkeypatch.setattr(pymupdf.Page, "show_pdf_page", show)
    with make_pdf("ORIGINAL") as original, make_pdf("TRANSLATED") as translated:
        with pytest.raises(error_type) as caught:
            creator.create_side_by_side_dual_pdf(original, translated, "dual.pdf", config)
        assert caught.value is error
        assert len(calls) == failed_call
