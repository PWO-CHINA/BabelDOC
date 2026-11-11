import json

import pytest
from babeldoc import _rust_il
from babeldoc.format.pdf.document_il import il_version_1 as il
from test_cross_page_llm_direct_atomicity import LEFT_PREFIX
from test_cross_page_llm_direct_atomicity import LEFT_TAIL
from test_cross_page_llm_direct_atomicity import TRANSLATED_FOLLOWING
from test_cross_page_llm_direct_atomicity import TRANSLATED_PREFIX
from test_cross_page_llm_direct_atomicity import TRANSLATED_SENTENCE
from test_cross_page_llm_direct_atomicity import DirectApplicationService
from test_cross_page_llm_direct_atomicity import Monitor
from test_cross_page_llm_direct_atomicity import run_bridge


def paragraph(text, bounds, label="text", size=10.0):
    style = il.PdfStyle(font_id="regular", font_size=size)
    return il.PdfParagraph(
        box=il.Box(x=bounds[0], y=bounds[1], x2=bounds[2], y2=bounds[3]),
        pdf_style=style,
        unicode=text,
        debug_id=f"synthetic-{bounds[0]}-{bounds[1]}",
        layout_label=label,
        pdf_paragraph_composition=[
            il.PdfParagraphComposition(
                pdf_character=il.PdfCharacter(pdf_style=style, char_unicode=text, xobj_id=0)
            )
        ],
    )


def fixture(*, size=10.0, labelled_caption=False, neighbour_caption=False,
            caption_size=7.0, title=False, running_header=False, single_header=False, page_number=1,
            left_text=LEFT_PREFIX + LEFT_TAIL, right_text="several independent experiments.", styled=False):
    left = paragraph(left_text, (310, 60, 550, 145),
                     label="figure_caption" if labelled_caption else "text", size=size)
    left_items = [
        paragraph("An earlier paragraph contains a complete observation.", (40, 300, 290, 700)),
        paragraph("The following paragraph contains another observation.", (310, 300, 550, 700)),
        left,
    ]
    if neighbour_caption:
        left_items.append(paragraph("Figure 3. A synthetic caption continues.",
                                    (40, 60, 290, 145), label="figure_caption", size=caption_size))
    right_items = [
        paragraph(right_text, (40, 650, 290, 720)),
        paragraph("A separate paragraph appears in the next column.", (310, 400, 550, 700)),
    ]
    if title:
        right_items.append(paragraph("Methods", (40, 731, 135, 754), label="title"))
    if running_header:
        left_items.append(paragraph("Review article", (40, 730, 135, 754), label="title"))
        right_items.append(paragraph("Review article", (40, 730, 135, 754), label="title"))
    if single_header:
        right_items.append(paragraph("Review article", (40, 730, 135, 748), label="title"))
    if styled:
        for target in (left, right_items[0]):
            style = il.PdfStyle(font_id="regular", font_size=10.0,
                                graphic_state=il.GraphicState(passthrough_per_char_instruction="0 g"))
            text = target.unicode
            if target is left:
                prefix, tail = text.split("clear. ", 1)
                target.pdf_paragraph_composition = [
                    il.PdfParagraphComposition(pdf_character=il.PdfCharacter(pdf_style=target.pdf_style, char_unicode=prefix + "clear. ")),
                    il.PdfParagraphComposition(pdf_same_style_characters=il.PdfSameStyleCharacters(
                        pdf_style=style, pdf_character=[il.PdfCharacter(pdf_style=style, char_unicode=tail)]
                    )),
                ]
            else:
                target.pdf_paragraph_composition = [
                    il.PdfParagraphComposition(pdf_same_style_characters=il.PdfSameStyleCharacters(
                        pdf_style=style, pdf_character=[il.PdfCharacter(pdf_style=style, char_unicode=text)]
                    )),
                    il.PdfParagraphComposition(pdf_formula=il.PdfFormula(pdf_character=[
                        il.PdfCharacter(pdf_style=target.pdf_style, char_unicode="x")
                    ])),
                ]
    font = il.PdfFont(font_id="regular", bold=False, italic=False, monospace=False, serif=False)
    pages = [
        il.Page(page_number=number, cropbox=il.Cropbox(box=il.Box(x=0, y=0, x2=600, y2=800)),
                pdf_font=[font], pdf_paragraph=items)
        for number, items in [(0, left_items), (page_number, right_items)]
    ]
    return _rust_il.transfer_document(il.Document(page=pages, total_pages=2))


def test_two_column_body_is_translated_and_returns_to_original_pages(tmp_path):
    document = fixture()
    service = DirectApplicationService()
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.cross_page_requests == 1
    assert service.translator.fallback_requests == 0
    prompt = next(value for value in service.translator.requests if "## Cross-page continuation" in value)
    items = json.loads(prompt.rsplit("## Here is the input:", 1)[1].strip())
    assert any(LEFT_TAIL + " several independent experiments." in item["input"] for item in items)
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    left = pages[0]["pdf_paragraph"][2]["unicode"]
    right = pages[1]["pdf_paragraph"][0]["unicode"]
    assert left + right == TRANSLATED_PREFIX + TRANSLATED_SENTENCE
    assert TRANSLATED_PREFIX in left and right
    assert TRANSLATED_FOLLOWING not in left


@pytest.mark.parametrize("kwargs", [
    {"running_header": True},
    {"left_text": "Earlier findings were clear. The cohort included participants from",
     "right_text": "California and neighboring regions."},
])
def test_supported_boundaries_are_detected(tmp_path, kwargs):
    document = fixture(**kwargs)
    service = DirectApplicationService()
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.requests
    assert service.translator.cross_page_requests == 1


def test_unmovable_sentence_does_not_use_context_only_mode(tmp_path):
    document = fixture(left_text=LEFT_TAIL)
    service = DirectApplicationService()
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.requests
    assert service.translator.cross_page_requests == 0


def test_identity_sentence_retains_each_page_anchor(tmp_path):
    left_tail = "The synthetic left anchor begins a statement across"
    right_text = "the synthetic right anchor and ends here."
    document = fixture(left_text="Earlier findings were clear. " + left_tail, right_text=right_text)
    service = DirectApplicationService()
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.cross_page_requests == 1
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    left = pages[0]["pdf_paragraph"][2]["unicode"]
    right = pages[1]["pdf_paragraph"][0]["unicode"]
    assert "synthetic left anchor" in left
    assert "synthetic right anchor" in right


@pytest.mark.parametrize("right_text", ["several independent experiments.",
                                        "several independent experiments. Additional observations follow."])
def test_unproven_boundary_styles_do_not_use_reflow(tmp_path, right_text):
    document = fixture(right_text=right_text, styled=True)
    service = DirectApplicationService()
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.requests
    assert service.translator.cross_page_requests == 0


@pytest.mark.parametrize("kwargs", [
    {"labelled_caption": True},
    {"size": 7.0, "neighbour_caption": True},
    {"size": 7.0},
    {"neighbour_caption": True, "caption_size": 10.0},
    {"title": True},
    {"single_header": True},
    {"page_number": 2},
])
def test_ambiguous_boundary_stays_out_of_cross_page_request(tmp_path, kwargs):
    document = fixture(**kwargs)
    service = DirectApplicationService()
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.requests
    assert service.translator.cross_page_requests == 0
    tracking = json.loads((tmp_path / "translate_tracking.json").read_text(encoding="utf-8"))
    assert tracking["cross_page"] == []
