import codecs
import json
import re

import pytest
from babeldoc import _rust_il
from babeldoc.format.pdf.document_il import il_version_1 as il
from test_cross_page_llm_direct_atomicity import LEFT_TAIL
from test_cross_page_llm_direct_atomicity import RIGHT_CONTINUATION
from test_cross_page_llm_direct_atomicity import DirectApplicationService
from test_cross_page_llm_direct_atomicity import DirectTranslator
from test_cross_page_llm_direct_atomicity import Monitor
from test_cross_page_llm_direct_atomicity import run_bridge


def synthetic_substitution(text):
    parts = re.split(r"(<[^<>]*>|\{[^{}]*\})", text)
    return "".join(part if index % 2 else codecs.encode(part, "rot_13") for index, part in enumerate(parts))


class ReversibleTranslator(DirectTranslator):
    def llm_translate(self, prompt, rate_limit_params=None):
        self.requests.append(prompt)
        marker = "## Here is the input:"
        if marker not in prompt:
            self.fallback_requests += 1
            return synthetic_substitution(prompt.rsplit("Now translate the following text:", 1)[1].strip())
        if "## Cross-page continuation" in prompt:
            self.cross_page_requests += 1
        else:
            self.page_requests += 1
        items = json.loads(prompt.rsplit(marker, 1)[1].strip())
        return json.dumps([{"id": item["id"], "output": synthetic_substitution(item["input"])} for item in items])


def formula_boundary_document(*, overlapping_formula=False, split_continuation=False):
    left_style = il.PdfStyle(font_id="regular", font_size=10.0)
    right_style = il.PdfStyle(font_id="regular", font_size=10.0)
    rich_style = il.PdfStyle(
        font_id="regular", font_size=10.0,
        graphic_state=il.GraphicState(passthrough_per_char_instruction="0 g"),
    )

    def plain(text, style):
        return il.PdfParagraphComposition(pdf_character=il.PdfCharacter(pdf_style=style, char_unicode=text))

    def rich(text):
        return il.PdfParagraphComposition(pdf_same_style_characters=il.PdfSameStyleCharacters(
            pdf_style=rich_style,
            pdf_character=[il.PdfCharacter(pdf_style=rich_style, char_unicode=text)],
        ))

    def formula(text):
        return il.PdfParagraphComposition(pdf_formula=il.PdfFormula(pdf_character=[
            il.PdfCharacter(pdf_style=left_style, char_unicode=text),
        ]))

    if overlapping_formula:
        before = "Earlier findings were clear. The results demonstrate a "
        number, after = "x", " substantial improvement across"
    else:
        before = "Earlier findings were clear after "
        number, after = "23", ". " + LEFT_TAIL
    left_text = before + number + after
    right_text = RIGHT_CONTINUATION + " A final observation remains."
    if split_continuation or overlapping_formula:
        split = len("several independent ") if split_continuation else len(RIGHT_CONTINUATION)
        right_compositions = [rich(right_text[:split]), rich(right_text[split:])]
    else:
        right_compositions = [rich(RIGHT_CONTINUATION), plain(" A final observation remains.", right_style)]
    pages = []
    for index, (text, compositions) in enumerate((
        (left_text, [plain(before, left_style), formula(number), plain(after, left_style)]),
        (right_text, right_compositions),
    )):
        paragraph = il.PdfParagraph(
            box=il.Box(x=60, y=100 if index == 0 else 650, x2=540, y2=150 if index == 0 else 700),
            pdf_style=(left_style if index == 0 else right_style), unicode=text, debug_id=f"formula-boundary-{index}", layout_label="text",
            pdf_paragraph_composition=compositions,
        )
        pages.append(il.Page(
            page_number=index, cropbox=il.Cropbox(box=il.Box(x=0, y=0, x2=600, y2=800)),
            pdf_font=[il.PdfFont(font_id="regular", bold=False, italic=False, monospace=False, serif=False)],
            pdf_paragraph=[paragraph],
        ))
    return _rust_il.transfer_document(il.Document(page=pages, total_pages=2))


def execute(document, tmp_path):
    service = DirectApplicationService()
    service.translator = ReversibleTranslator()
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    return service.translator


def test_formula_before_prior_sentence_boundary_isolated_from_continuation(tmp_path):
    document = formula_boundary_document()
    translator = execute(document, tmp_path)
    assert translator.cross_page_requests == 1
    assert translator.fallback_requests == 0
    prompt = next(value for value in translator.requests if "## Cross-page continuation" in value)
    items = json.loads(prompt.rsplit("## Here is the input:", 1)[1].strip())
    inputs = [re.sub(r"\{v\d+\}|</?b\d+>", "", item["input"]) for item in items]
    sentence = LEFT_TAIL + " several independent experiments."
    assert len(items) == 3
    assert sum(sentence in value for value in inputs) == 1
    assert any("Additional observations follow." in value and sentence not in value for value in inputs)
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    left, right = [page["pdf_paragraph"][0] for page in pages]
    assert "Nqqvgvbany bofreingvbaf sbyybj." not in left["unicode"]
    assert "Nqqvgvbany bofreingvbaf sbyybj." in right["unicode"]
    assert any(part.get("pdf_formula") for part in left["pdf_paragraph_composition"])


@pytest.mark.parametrize("case", ["formula_in_sentence", "sentence_crosses_styles"])
def test_unsafe_formula_or_style_boundary_does_not_form_request(tmp_path, case):
    document = formula_boundary_document(
        overlapping_formula=case == "formula_in_sentence",
        split_continuation=case == "sentence_crosses_styles",
    )
    translator = execute(document, tmp_path)
    assert translator.cross_page_requests == 0
