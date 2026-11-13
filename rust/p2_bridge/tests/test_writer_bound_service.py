from types import SimpleNamespace

import pymupdf
import pytest
from babeldoc.rust_writer_service import WriterFileService


def pdf_bytes(page_count):
    document = pymupdf.open()
    for _ in range(page_count):
        document.new_page()
    result = document.tobytes()
    document.close()
    return result


def test_original_bytes_and_path_after_replacement(tmp_path):
    source = tmp_path / "source.pdf"
    original = pdf_bytes(1)
    source.write_bytes(original)
    service = WriterFileService(SimpleNamespace(), {}, str(source))
    try:
        source.write_bytes(pdf_bytes(2))
        service.open_original_bytes(original)
        assert len(service.original) == 1
        service.open_original(str(source))
        assert len(service.original) == 2
        with pytest.raises(pymupdf.FileDataError):
            service.open_original_bytes(b"invalid PDF")
        assert len(service.original) == 2
    finally:
        service.close()


def test_translated_bytes_and_path_after_replacement(tmp_path):
    translated = tmp_path / "translated.pdf"
    generated = pdf_bytes(1)
    translated.write_bytes(generated)
    service = WriterFileService(SimpleNamespace(), {}, str(translated))
    try:
        translated.write_bytes(pdf_bytes(2))
        service.open_translated_bytes(generated)
        assert len(service.translated) == 1
        service.open_translated(str(translated))
        assert len(service.translated) == 2
        with pytest.raises(pymupdf.FileDataError):
            service.open_translated_bytes(b"invalid PDF")
        assert len(service.translated) == 2
    finally:
        service.close()
