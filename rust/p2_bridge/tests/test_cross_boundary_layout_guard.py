import hashlib
import json
import os
from pathlib import Path
from types import SimpleNamespace

import pytest
from babeldoc import _rust_il
from babeldoc.format.pdf.document_il import il_version_1 as il
from babeldoc.rust_run_context import PriorityServiceDispatcher
from babeldoc.rust_translation_service import RustTranslatorService
from test_cross_boundary_integration import LEFT
from test_cross_boundary_integration import RIGHT
from test_cross_boundary_integration import SyntheticTranslator
from test_cross_boundary_integration import document
from test_cross_boundary_integration import select
from test_cross_boundary_integration import source_characters
from test_cross_page_llm_direct_atomicity import Monitor
from test_cross_page_llm_direct_atomicity import run_bridge


@pytest.mark.parametrize("regions,locations", [
    ([[LEFT, RIGHT]], ((0, 0), (0, 1))),
    ([[LEFT], [("gamma delta.", "text", (50, 600, 250, 650))]], ((0, 0), (1, 0))),
])
def test_translation_candidate_uses_local_preview_without_pdf_writer(tmp_path, regions, locations):
    native = document(regions)
    before = native.parser_artifact_hash()
    tracking = tmp_path / "translate_tracking.json"
    tracking.write_bytes(b"existing tracking")
    translator = SyntheticTranslator(select(*locations))

    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert len(translator.boundary_requests) == 2
    assert native.parser_artifact_hash() != before
    report = json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))
    assert report["scope"] == "translation_only"
    assert report["decisions"][0]["result"] == "applied"
    assert json.loads(tracking.read_text(encoding="utf-8"))["page"]


@pytest.mark.parametrize("overlapping_figure", [False, True])
def test_translation_preserves_source_boxes_for_normal_typesetting(tmp_path, overlapping_figure):
    font = Path(os.environ.get("BABELDOC_TEST_PRODUCTION_FONT", Path.home() / ".cache/babeldoc/fonts/GoNotoKurrent-Regular.ttf"))
    assert font.is_file(), "cached production test font unavailable"
    family = {"normal": ["regular"], "script": [], "fallback": ["regular"], "base": ["regular"]}
    digest = hashlib.sha3_256(font.read_bytes()).hexdigest()
    mapper, _styles = _rust_il.font_mapper_handle_with_source_styles(
        json.dumps(family), json.dumps({"regular": str(font)}), json.dumps({"regular": digest}), None,
    )
    config = SimpleNamespace(
        disable_rich_text_translate=False, min_text_length=1, pool_max_workers=1,
        lang_out="Chinese", debug=True, working_dir=str(tmp_path),
        should_translate_page=lambda _page: True, get_working_file_path=lambda name: str(tmp_path / name),
        experimental_cross_page_context=True,
    )
    if overlapping_figure:
        from test_cross_boundary_candidate_negatives import figure_document
        native = figure_document(include_body_anchor=False, figure_box=(50, 550, 250, 600))
        locations = ((0, 1), (0, 3))
    else:
        native = document([[LEFT, RIGHT]])
        locations = ((0, 0), (0, 1))
    before = native.parser_artifact_hash()
    source_pages = json.loads(native.parser_artifact_json())["document"]["page"]
    translator = SyntheticTranslator(select(*locations))
    service = RustTranslatorService(translator)
    first = PriorityServiceDispatcher(service, 1)
    second = PriorityServiceDispatcher(service, 1)
    try:
        native.translate_paragraphs_llm_only(
            config, _rust_il.SharedContextCrossSplitPart([]), mapper, first, second,
            service, _rust_il.CancelFlag(), Monitor(),
        )
    finally:
        first.close()
        second.close()
    assert native.parser_artifact_hash() != before
    pages = json.loads(native.parser_artifact_json())["document"]["page"]
    for page, paragraph in locations:
        assert pages[page]["pdf_paragraph"][paragraph]["box"] == source_pages[page]["pdf_paragraph"][paragraph]["box"]
    assert pages[0].get("pdf_figure") == source_pages[0].get("pdf_figure")
    assert (tmp_path / "translate_tracking.json").exists()


def test_formula_source_identity_survives_boundary_reconstruction(tmp_path):
    style = il.PdfStyle(font_id="regular", font_size=10.0)
    formula_characters = source_characters("x", style, (105, 600, 120, 650))
    formula = il.PdfFormula(pdf_character=formula_characters, box=formula_characters[0].box,
                            x_offset=0, y_offset=0, x_advance=formula_characters[0].advance)
    left = il.PdfParagraph(
        box=il.Box(x=50, y=600, x2=250, y2=650), pdf_style=style,
        unicode="Alpha betax", debug_id="left", layout_label="text",
        pdf_paragraph_composition=[
            *[il.PdfParagraphComposition(pdf_character=character)
              for character in source_characters("Alpha beta", style, (50, 600, 105, 650))],
            il.PdfParagraphComposition(pdf_formula=formula),
        ],
    )
    right = il.PdfParagraph(
        box=il.Box(x=350, y=600, x2=550, y2=650), pdf_style=style,
        unicode="gamma delta.", debug_id="right", layout_label="text",
        pdf_paragraph_composition=[il.PdfParagraphComposition(pdf_character=character)
                                   for character in source_characters("gamma delta.", style, (350, 600, 550, 650))],
    )
    page = il.Page(
        page_number=0, cropbox=il.Cropbox(box=il.Box(x=0, y=0, x2=600, y2=800)),
        pdf_font=[il.PdfFont(font_id="regular", bold=False, italic=False, monospace=False, serif=False)],
        pdf_paragraph=[left, right],
    )
    native = _rust_il.transfer_document(il.Document(page=[page], total_pages=1))
    before = native.parser_artifact_hash()
    source_formula = json.loads(native.parser_artifact_json())["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"][-1]["pdf_formula"]
    tracking = tmp_path / "translate_tracking.json"
    tracking.write_bytes(b"existing tracking")
    translator = SyntheticTranslator(select((0, 0), (0, 1)))
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert len(translator.boundary_requests) == 2
    assert native.parser_artifact_hash() != before
    paragraphs = json.loads(native.parser_artifact_json())["document"]["page"][0]["pdf_paragraph"]
    formulas = [part["pdf_formula"] for paragraph in paragraphs for part in paragraph["pdf_paragraph_composition"] if part.get("pdf_formula")]
    assert len(formulas) == 1
    assert formulas[0] == source_formula
    assert json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))["decisions"][0]["result"] == "applied"
