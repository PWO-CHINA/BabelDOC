import json
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest
from babeldoc import _rust_il
from babeldoc.format.pdf.document_il.il_version_1 import Document


def test_ordinary_bridge_rejects_experiment_before_mutating_document():
    font = (
        Path("C:/Windows/Fonts/arial.ttf")
        if sys.platform == "win32"
        else Path("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf")
    )
    if not font.exists():
        pytest.skip("test font is unavailable")
    family = json.dumps({"normal": ["regular"], "script": [], "fallback": [], "base": ["regular"]})
    mapper = _rust_il.font_mapper_handle(family, json.dumps({"regular": str(font)}))
    document = _rust_il.transfer_document(Document(page=[], total_pages=0))
    before = document.parser_artifact_hash()

    with pytest.raises(ValueError, match="ordinary translators are not supported"):
        document.translate_paragraphs(
            SimpleNamespace(experimental_cross_page_context=True),
            None,
            mapper,
            None,
            None,
            _rust_il.CancelFlag(),
            None,
        )

    assert document.parser_artifact_hash() == before
