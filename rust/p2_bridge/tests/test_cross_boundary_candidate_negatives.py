import json

import pytest
from babeldoc import _rust_il
from babeldoc.format.pdf.document_il import il_version_1 as il
from babeldoc.rust_translation_service import RustTranslatorService
from test_cross_boundary_integration import SyntheticTranslator
from test_cross_boundary_integration import document
from test_cross_boundary_integration import select
from test_cross_boundary_integration import source_characters
from test_cross_page_llm_direct_atomicity import Monitor
from test_cross_page_llm_direct_atomicity import run_bridge

LEFT = ("Alpha beta 12.34", "caption", (50, 550, 250, 600))
RIGHT = ("gamma delta.", "text", (350, 550, 550, 600))
TITLE = ("A synthetic heading", "title", (50, 700, 250, 740))
CAPTION = ("Synthetic figure caption", "caption", (50, 180, 250, 210))


def figure_document(include_body_anchor=True, figure_box=(50, 250, 250, 450), left_label="caption"):
    entries = [TITLE, (LEFT[0], left_label, LEFT[2]), CAPTION, RIGHT]
    if include_body_anchor:
        entries.append(("Earlier body", "text", (50, 620, 250, 660)))
    return document([entries], figures={0: [il.PdfFigure(box=il.Box(
        x=figure_box[0], y=figure_box[1], x2=figure_box[2], y2=figure_box[3],
    ))]})


def test_context_and_none_with_figure_below_body(tmp_path):
    native = figure_document(left_label="text")
    translator = SyntheticTranslator(lambda _: {"status": "none"})
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert len(translator.boundary_requests) == 1
    metadata = translator.metadata[0]
    assert {(item["paragraph"], item["label"]) for item in metadata["left"] + metadata["right"]} >= {
        (1, "text"), (2, "caption"), (3, "text")
    }
    context = metadata["context"][0]
    assert len(context["figures"]) == 1
    assert context["figures"][0]["top"] < next(item for item in metadata["left"] if item["paragraph"] == 1)["bounds"]["bottom"]
    assert {item["label"] for item in context["paragraphs"]} >= {"caption", "text"}
    trace = json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))["decisions"][0]
    assert trace["result"] == "none"
    assert "translation_request_sha256" not in trace


@pytest.mark.parametrize("ranges", [
    [(0, 0, "Alpha beta 12.34"), (0, 99, "gamma delta")],
    [(0, 1, "gamma delta"), (0, 0, "Alpha beta 12.34")],
    [(0, 0, "Alpha beta 12.34"), (0, 0, "Alpha beta 12.34"), (0, 1, "gamma delta")],
    [(0, 0, "34"), (0, 1, "gamma delta")],
    [(0, 0, "beta 12.34"), (0, 1, "gamm")],
    [(0, 0, "pha beta 12.34"), (0, 1, "gamma delta")],
], ids=["unknown-object", "wrong-order", "duplicate", "partial-number", "partial-word-end", "partial-word-start"])
def test_invalid_selection_reports_unresolved_without_merging(tmp_path, ranges):
    native = document([[(LEFT[0], "text", LEFT[2]), RIGHT]])
    before = native.parser_artifact_hash()
    tracking = tmp_path / "translate_tracking.json"
    tracking.write_bytes(b"existing tracking")
    decision = {"status": "selected", "ranges": [
        {"page": page, "paragraph": paragraph, "text": value} for page, paragraph, value in ranges
    ], "joins": ["space"] * (len(ranges) - 1)}
    translator = SyntheticTranslator(lambda _: decision)
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert len(translator.boundary_requests) == 2
    assert all("Metadata: " in prompt for prompt, _ in translator.boundary_requests)
    report = json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))
    assert report["status"] == "needs_review" and report["unresolved"]
    assert all(decision["result"] != "selected" for decision in report["decisions"])


def test_caption_label_retains_ordinary_translation(tmp_path):
    native = figure_document(include_body_anchor=False)
    control = figure_document(include_body_anchor=False)
    control_dir = tmp_path / "ordinary-control"
    control_dir.mkdir()
    run_bridge(control, control_dir, RustTranslatorService(SyntheticTranslator(lambda _: {"status": "none"})),
               _rust_il.CancelFlag(), Monitor(), cross_page=False)
    translator = SyntheticTranslator(select((0, 1), (0, 3)))
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert not translator.boundary_requests
    assert json.loads(native.parser_artifact_json())["document"]["page"] == json.loads(control.parser_artifact_json())["document"]["page"]
    report = json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))
    assert report["baseline_completed"] and report["applied_groups"] == 0
    assert all(decision["result"] != "applied" for decision in report["decisions"])


def test_cross_page_body_is_not_hidden_by_right_side_caption(tmp_path):
    caption = ("Synthetic caption", "caption", (350, 180, 550, 210))
    left = ("Alpha beta", "text", (50, 550, 250, 600))
    right = ("gamma delta.", "text", (50, 550, 550, 600))
    native = document([[left, caption], [right]])
    before = native.parser_artifact_hash()
    tracking = tmp_path / "translate_tracking.json"
    tracking.write_bytes(b"existing tracking")
    select_body = select((0, 0), (1, 0))
    translator = SyntheticTranslator(lambda metadata: {"status": "none"} if metadata["kind"] == "column" else select_body(metadata))
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    page_metadata = next(metadata for metadata in translator.metadata if metadata["kind"] == "page")
    assert {(candidate["page"], candidate["paragraph"]) for candidate in page_metadata["left"]} == {(0, 0), (0, 1)}
    assert len(translator.boundary_requests) == 2
    assert all(metadata["kind"] == "page" for metadata in translator.metadata)
    assert native.parser_artifact_hash() != before
    assert json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))["decisions"][-1]["result"] == "applied"


def test_page_selection_may_skip_mislabelled_nonbody_in_intervening_column(tmp_path):
    left = ("Alpha beta", "text", (50, 550, 250, 600))
    earlier_right_column = ("Fig. 1. Synthetic plot", "text", (350, 180, 550, 210))
    right = ("gamma delta.", "text", (50, 550, 550, 600))
    native = document([[left, earlier_right_column], [right]])
    before = native.parser_artifact_hash()
    tracking = tmp_path / "translate_tracking.json"
    tracking.write_bytes(b"existing tracking")
    select_body = select((0, 0), (1, 0))
    translator = SyntheticTranslator(lambda metadata: {"status": "none"} if metadata["kind"] == "column" else select_body(metadata))
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert len(translator.boundary_requests) == 2
    assert all(metadata["kind"] == "page" for metadata in translator.metadata)
    assert "do not jump over an intervening body candidate" in translator.boundary_requests[0][0]
    assert native.parser_artifact_hash() != before
    assert json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))["decisions"][-1]["result"] == "applied"


def test_small_body_cannot_be_skipped_for_distant_aligned_caption(tmp_path):
    paragraphs = []
    for text, label, bounds, size in (
        ("Alpha beta", "text", (50, 600, 250, 650), 10),
        ("gamma", "text", (50, 400, 250, 450), 8),
        ("delta.", "text", (350, 600, 550, 650), 10),
        ("Synthetic caption", "caption", (350, 400, 550, 450), 8),
    ):
        style = il.PdfStyle(font_id="regular", font_size=size)
        paragraphs.append(il.PdfParagraph(
            box=il.Box(x=bounds[0], y=bounds[1], x2=bounds[2], y2=bounds[3]), pdf_style=style,
            unicode=text, layout_label=label,
            pdf_paragraph_composition=[il.PdfParagraphComposition(pdf_character=character)
                                       for character in source_characters(text, style, bounds)],
        ))
    page = il.Page(page_number=0, cropbox=il.Cropbox(box=il.Box(x=0, y=0, x2=600, y2=800)),
                   pdf_font=[il.PdfFont(font_id="regular", bold=False, italic=False, monospace=False, serif=False)],
                   pdf_paragraph=paragraphs)
    native = _rust_il.transfer_document(il.Document(page=[page], total_pages=1))
    translator = SyntheticTranslator(select((0, 0), (0, 2)))
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert translator.boundary_requests
    assert all("Metadata: " in prompt for prompt, _params in translator.boundary_requests)
    report = json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))
    assert report["baseline_completed"] and report["applied_groups"] == 0
    assert "cannot be skipped without layout evidence" in str(report["unresolved"])
    after = json.loads(native.parser_artifact_json())["document"]["page"][0]["pdf_paragraph"]
    assert after[1]["unicode"] == "gamma"
