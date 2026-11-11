import json
import re

from babeldoc import _rust_il
from babeldoc.format.pdf.document_il import il_version_1 as il
from test_cross_page_llm_direct_atomicity import DirectApplicationService
from test_cross_page_llm_direct_atomicity import Monitor
from test_cross_page_llm_direct_atomicity import run_bridge
from test_cross_page_sample_validation import document_with_caption
from test_cross_page_sample_validation import paragraph
from test_crosspage_formula_boundary_validation import ReversibleTranslator
from test_crosspage_formula_boundary_validation import synthetic_substitution

LEFT_TAIL = "The independent investigators reported that"
UPPER_CONTINUATION = "PX24 activity changed substantially in the observed samples."
LOWER_CONTINUATION = "several independent measurements changed substantially in the observed samples."
CAPTION_TAIL = "The synthetic figure notes the following"
FOLLOWING_SENTENCE = "Additional observations followed."


def translate(document, tmp_path):
    service = DirectApplicationService()
    service.translator = ReversibleTranslator()
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    return service.translator


def dedicated_inputs(translator):
    inputs = []
    for prompt in translator.requests:
        if "## Cross-page continuation" in prompt and "## Here is the input:" in prompt:
            items = json.loads(prompt.rsplit("## Here is the input:", 1)[1].strip())
            inputs.extend(re.sub(r"\{v\d+\}|</?b\d+>", "", item["input"]) for item in items)
    return inputs


def figure_below_boundary_document(body_tail=LEFT_TAIL):
    pages = [
        il.Page(
            page_number=0,
            cropbox=il.Cropbox(box=il.Box(x=0, y=0, x2=600, y2=800)),
            pdf_font=[il.PdfFont(font_id="regular", bold=False, italic=False, monospace=False, serif=False)],
            pdf_paragraph=[
                paragraph("An earlier body paragraph is complete.", (40, 350, 290, 700)),
                paragraph("Another body paragraph is complete.", (310, 360, 550, 700)),
                paragraph("Earlier findings were clear. " + body_tail, (310, 270, 550, 350)),
                paragraph("Figure 7. A synthetic chart with detailed notes.", (40, 50, 290, 135), label="figure_caption", size=7),
                paragraph(CAPTION_TAIL, (310, 50, 550, 135), label="text", size=7),
            ],
        ),
        il.Page(
            page_number=1,
            cropbox=il.Cropbox(box=il.Box(x=0, y=0, x2=600, y2=800)),
            pdf_font=[il.PdfFont(font_id="regular", bold=False, italic=False, monospace=False, serif=False)],
            pdf_paragraph=[
                paragraph(LOWER_CONTINUATION + " " + FOLLOWING_SENTENCE, (40, 550, 290, 680)),
                paragraph("A separate body paragraph is complete.", (310, 400, 550, 700)),
            ],
        ),
    ]
    return _rust_il.transfer_document(il.Document(page=pages, total_pages=2))


def test_alphanumeric_acronym_after_that_is_a_whole_sentence(tmp_path):
    control = document_with_caption(
        caption_label="text", caption_size=10, obstacle=False,
        left_text="Earlier findings were clear. " + LEFT_TAIL,
        right_text=LOWER_CONTINUATION,
    )
    control_dir = tmp_path / "control"
    control_dir.mkdir()
    assert translate(control, control_dir).cross_page_requests == 1

    candidate = document_with_caption(
        caption_label="text", caption_size=10, obstacle=False,
        left_text="Earlier findings were clear. " + LEFT_TAIL,
        right_text=UPPER_CONTINUATION + " " + FOLLOWING_SENTENCE,
    )
    candidate_dir = tmp_path / "candidate"
    candidate_dir.mkdir()
    translator = translate(candidate, candidate_dir)
    assert translator.cross_page_requests == 1
    assert any(LEFT_TAIL + " " + UPPER_CONTINUATION in value for value in dedicated_inputs(translator))
    pages = json.loads(candidate.parser_artifact_json())["document"]["page"]
    left = pages[0]["pdf_paragraph"][-1]["unicode"]
    right = pages[1]["pdf_paragraph"][0]["unicode"]
    assert synthetic_substitution(FOLLOWING_SENTENCE) not in left
    assert synthetic_substitution(FOLLOWING_SENTENCE) in right
    assert all(
        LEFT_TAIL not in prompt and UPPER_CONTINUATION not in prompt
        for prompt in translator.requests if "## Here is the input:" not in prompt
    )


def test_body_continuation_above_figure_is_not_paired_with_caption(tmp_path):
    document = figure_below_boundary_document()
    translator = translate(document, tmp_path)
    assert translator.cross_page_requests == 1
    inputs = dedicated_inputs(translator)
    assert any(LEFT_TAIL + " " + LOWER_CONTINUATION in value for value in inputs)
    assert all(CAPTION_TAIL not in value for value in inputs)
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    left = pages[0]["pdf_paragraph"][2]["unicode"]
    right = pages[1]["pdf_paragraph"][0]["unicode"]
    assert synthetic_substitution(FOLLOWING_SENTENCE) not in left
    assert synthetic_substitution(FOLLOWING_SENTENCE) in right
    assert all(
        LEFT_TAIL not in prompt and LOWER_CONTINUATION not in prompt
        for prompt in translator.requests if "## Here is the input:" not in prompt
    )


def test_caption_above_page_boundary_does_not_replace_complete_body(tmp_path):
    document = figure_below_boundary_document("This independent body paragraph is complete.")
    translator = translate(document, tmp_path)
    assert translator.requests
    assert translator.cross_page_requests == 0
