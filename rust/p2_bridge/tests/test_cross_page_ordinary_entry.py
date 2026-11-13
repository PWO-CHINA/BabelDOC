import json
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest
from babeldoc import _rust_il
from babeldoc.rust_run_context import PriorityServiceDispatcher
from babeldoc.rust_translation_service import RustTranslatorService
from test_cross_page_llm_direct_atomicity import DirectTranslator
from test_cross_page_llm_direct_atomicity import Monitor
from test_cross_page_llm_direct_atomicity import make_direct_document


def test_ordinary_bridge_retains_baseline_when_enhancement_not_supported(tmp_path):
    font = (
        Path("C:/Windows/Fonts/arial.ttf")
        if sys.platform == "win32"
        else Path("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf")
    )
    if not font.exists():
        pytest.skip("test font is unavailable")
    family = json.dumps({"normal": ["regular"], "script": [], "fallback": [], "base": ["regular"]})
    mapper = _rust_il.font_mapper_handle(family, json.dumps({"regular": str(font)}))
    document = make_direct_document()
    before = document.parser_artifact_hash()

    class OrdinaryTranslator(DirectTranslator):
        def do_llm_translate(self, text, rate_limit_params=None):
            raise NotImplementedError()

        def translate(self, text, rate_limit_params=None):
            return self.ordinary(text)

    config = SimpleNamespace(
        experimental_cross_page_context=True, disable_rich_text_translate=False,
        min_text_length=1, pool_max_workers=1, lang_out="Chinese", auto_extract_glossary=False,
        debug=True, working_dir=str(tmp_path), should_translate_page=lambda _page: True,
        get_working_file_path=lambda name: str(tmp_path / name),
    )
    service = RustTranslatorService(OrdinaryTranslator())
    dispatcher = PriorityServiceDispatcher(service, 1)
    try:
        document.translate_paragraphs(
            config,
            _rust_il.SharedContextCrossSplitPart([]),
            mapper,
            dispatcher,
            service,
            _rust_il.CancelFlag(),
            Monitor(),
        )
    finally:
        dispatcher.close()

    assert document.parser_artifact_hash() != before
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    assert "基础左页译文" in pages[0]["pdf_paragraph"][0]["unicode"]
    assert "基础右页译文" in pages[1]["pdf_paragraph"][0]["unicode"]
    report = json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))
    assert report["baseline_completed"] and report["candidate_state"] == "not_supported"
    assert report["applied_groups"] == 0 and report["unresolved"]
