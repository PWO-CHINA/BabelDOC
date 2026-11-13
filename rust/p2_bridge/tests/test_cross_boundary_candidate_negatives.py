import json

import pytest
from babeldoc import _rust_il
from babeldoc.format.pdf.document_il import il_version_1 as il
from babeldoc.rust_translation_service import RustTranslatorService
from test_cross_boundary_integration import SyntheticTranslator
from test_cross_boundary_integration import document
from test_cross_boundary_integration import select
from test_cross_page_llm_direct_atomicity import Monitor
from test_cross_page_llm_direct_atomicity import run_bridge

LEFT = ("Alpha beta 12.34", "caption", (50, 550, 250, 600))
RIGHT = ("gamma delta.", "text", (350, 550, 550, 600))
TITLE = ("A synthetic heading", "title", (50, 700, 250, 740))
CAPTION = ("Synthetic figure caption", "caption", (50, 180, 250, 210))


def figure_document(include_body_anchor=True, figure_box=(50, 250, 250, 450)):
    entries = [TITLE, LEFT, CAPTION, RIGHT]
    if include_body_anchor:
        entries.append(("Earlier body", "text", (50, 620, 250, 660)))
    return document([entries], figures={0: [il.PdfFigure(box=il.Box(
        x=figure_box[0], y=figure_box[1], x2=figure_box[2], y2=figure_box[3],
    ))]})


def test_context_and_none_with_figure_below_body(tmp_path):
    native = figure_document()
    translator = SyntheticTranslator(lambda _: {"status": "none"})
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert len(translator.boundary_requests) == 1
    metadata = translator.metadata[0]
    assert {(item["paragraph"], item["label"]) for item in metadata["left"] + metadata["right"]} >= {
        (1, "caption"), (2, "caption"), (3, "text")
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


def test_mislabelled_body_selection_translates(tmp_path):
    native = figure_document(include_body_anchor=False)
    before = native.parser_artifact_hash()
    tracking = tmp_path / "translate_tracking.json"
    tracking.write_bytes(b"existing tracking")
    translator = SyntheticTranslator(select((0, 1), (0, 3)))
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert len(translator.boundary_requests) == 2
    assert native.parser_artifact_hash() != before
    assert json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))["decisions"][0]["result"] == "applied"


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
    assert len(translator.boundary_requests) == 3
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
    assert len(translator.boundary_requests) == 3
    assert "do not jump over an intervening body candidate" in translator.boundary_requests[1][0]
    assert native.parser_artifact_hash() != before
    assert json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))["decisions"][-1]["result"] == "applied"
