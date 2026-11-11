import json
import re

import pytest

from babeldoc import _rust_il
from babeldoc.format.pdf.document_il import il_version_1 as il

from test_cross_page_llm_direct_atomicity import (
    DirectApplicationService,
    DirectTranslator,
    LEFT_PREFIX,
    LEFT_TAIL,
    Monitor,
    RIGHT_CONTINUATION,
    TRANSLATED_FOLLOWING,
    TRANSLATED_PREFIX,
    TRANSLATED_SENTENCE,
    make_direct_document,
    run_bridge,
)


def paragraph(text, box, *, label="text", size=10.0):
    style = il.PdfStyle(font_id="regular", font_size=size)
    return il.PdfParagraph(
        box=il.Box(x=box[0], y=box[1], x2=box[2], y2=box[3]),
        pdf_style=style,
        unicode=text,
        debug_id=f"{box[0]}-{box[1]}",
        layout_label=label,
        pdf_paragraph_composition=[
            il.PdfParagraphComposition(
                pdf_character=il.PdfCharacter(pdf_style=style, char_unicode=text, xobj_id=0)
            )
        ],
    )


def document_with_caption(
    *, caption_label, next_page_number=1, caption_size=7, obstacle=True,
    left_text=LEFT_PREFIX + LEFT_TAIL, right_text=RIGHT_CONTINUATION, header_pages=(),
    left_style_parts=None, right_style_parts=None,
):
    caption = paragraph(
        left_text, (310, 60, 550, 145), label=caption_label, size=caption_size
    )
    right_paragraph = paragraph(right_text, (40, 650, 290, 720))

    def apply_styles(target, parts):
        assert "".join(parts) == target.unicode
        target.pdf_paragraph_composition = []
        for index, text in enumerate(parts):
            style = il.PdfStyle(
                font_id="regular", font_size=10.0,
                graphic_state=il.GraphicState(passthrough_per_char_instruction=f"{index} g"),
            )
            target.pdf_paragraph_composition.append(il.PdfParagraphComposition(
                pdf_same_style_characters=il.PdfSameStyleCharacters(
                    pdf_style=style,
                    pdf_character=[il.PdfCharacter(pdf_style=style, char_unicode=text, xobj_id=0)],
                ),
            ))

    if left_style_parts:
        apply_styles(caption, left_style_parts)
    if right_style_parts:
        apply_styles(right_paragraph, right_style_parts)
    pages = [
        il.Page(
            page_number=0,
            cropbox=il.Cropbox(box=il.Box(x=0, y=0, x2=600, y2=800)),
            pdf_font=[il.PdfFont(font_id="regular", bold=False, italic=False, monospace=False, serif=False)],
            pdf_paragraph=[
                *([paragraph("Review article", (40, 730, 135, 754), label="title")] if 0 in header_pages else []),
                paragraph("A prior body paragraph is complete.", (40, 300, 290, 700)),
                paragraph("A second body paragraph is complete.", (310, 300, 550, 700)),
                *([paragraph("Figure 3. A synthetic caption continues in the next column.", (40, 60, 290, 145), label="figure_caption", size=7)] if obstacle else []),
                caption,
            ],
        ),
        il.Page(
            page_number=next_page_number,
            cropbox=il.Cropbox(box=il.Box(x=0, y=0, x2=600, y2=800)),
            pdf_font=[il.PdfFont(font_id="regular", bold=False, italic=False, monospace=False, serif=False)],
            pdf_paragraph=[
                *([paragraph("Review article", (40, 730, 135, 754), label="title")] if 1 in header_pages else []),
                right_paragraph,
                paragraph("Another body paragraph is complete.", (310, 400, 550, 700)),
            ],
        ),
    ]
    return _rust_il.transfer_document(il.Document(page=pages, total_pages=2))


def run(document, tmp_path):
    service = DirectApplicationService()
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    return service.translator


def test_complete_input_and_original_page_writeback(tmp_path):
    document = make_direct_document()
    before = json.loads(document.parser_artifact_json())["document"]["page"]
    translator = run(document, tmp_path)
    assert translator.cross_page_requests == 1
    assert translator.fallback_requests == 0
    prompt = next(value for value in translator.requests if "## Cross-page continuation" in value)
    items = json.loads(prompt.rsplit("## Here is the input:", 1)[1].strip())
    assert any(LEFT_TAIL + " several independent experiments." in item["input"] for item in items)

    after = json.loads(document.parser_artifact_json())["document"]["page"]
    left, right = [page["pdf_paragraph"][0] for page in after]
    assert [page["page_number"] for page in after] == [0, 1]
    assert [page["pdf_paragraph"][0]["box"] for page in after] == [
        page["pdf_paragraph"][0]["box"] for page in before
    ]
    assert TRANSLATED_PREFIX in left["unicode"] and TRANSLATED_PREFIX not in right["unicode"]
    assert TRANSLATED_FOLLOWING in right["unicode"] and TRANSLATED_FOLLOWING not in left["unicode"]
    visible = re.sub(r"\{v\d+\}|</?b\d+>", "", left["unicode"] + right["unicode"])
    assert visible == TRANSLATED_PREFIX + TRANSLATED_SENTENCE + TRANSLATED_FOLLOWING
    assert all(
        any(part.get("pdf_formula") for part in paragraph["pdf_paragraph_composition"])
        for paragraph in (left, right)
    )


def test_two_column_body_control_is_a_cross_page_pair(tmp_path):
    document = document_with_caption(caption_label="text", caption_size=10, obstacle=False)
    translator = run(document, tmp_path)
    assert translator.requests
    assert translator.cross_page_requests == 1


def test_proper_name_after_preposition_is_a_continuation(tmp_path):
    document = document_with_caption(
        caption_label="text", caption_size=10, obstacle=False,
        left_text="Earlier findings were clear. The cohort included participants from",
        right_text="California and neighboring regions. Additional observations follow.",
    )
    translator = run(document, tmp_path)
    assert translator.requests
    assert translator.cross_page_requests == 1


def test_repeated_running_header_does_not_hide_body_continuation(tmp_path):
    document = document_with_caption(
        caption_label="text", caption_size=10, obstacle=False, header_pages=(0, 1),
    )
    translator = run(document, tmp_path)
    assert translator.requests
    assert translator.cross_page_requests == 1


def test_single_page_title_blocks_a_candidate(tmp_path):
    document = document_with_caption(
        caption_label="text", caption_size=10, obstacle=False, header_pages=(1,),
    )
    translator = run(document, tmp_path)
    assert translator.requests
    assert translator.cross_page_requests == 0


def test_multiple_guards_still_reject_a_caption_pair(tmp_path):
    document = document_with_caption(caption_label="text", next_page_number=2)
    translator = run(document, tmp_path)
    assert translator.requests
    assert translator.cross_page_requests == 0


def test_mixed_style_boundary_keeps_sentences_and_pages(tmp_path):
    document = make_direct_document()
    artifact = json.loads(document.parser_artifact_json())
    pages = artifact["document"]["page"]
    rich = pages[1]["pdf_paragraph"][0]["pdf_paragraph_composition"][0]
    assert rich["pdf_same_style_characters"] is not None
    translator = run(document, tmp_path)
    assert translator.cross_page_requests == 1
    after = json.loads(document.parser_artifact_json())["document"]["page"]
    left, right = [page["pdf_paragraph"][0] for page in after]
    assert TRANSLATED_PREFIX in left["unicode"] and TRANSLATED_FOLLOWING in right["unicode"]
    visible = re.sub(r"\{v\d+\}|</?b\d+>", "", left["unicode"] + right["unicode"])
    assert visible == TRANSLATED_PREFIX + TRANSLATED_SENTENCE + TRANSLATED_FOLLOWING


def test_isolated_continuation_is_translated_as_one_input(tmp_path):
    document = document_with_caption(
        caption_label="text", caption_size=10, obstacle=False,
        right_text="several independent experiments.",
    )
    translator = run(document, tmp_path)
    assert translator.cross_page_requests == 1
    prompt = next(value for value in translator.requests if "## Cross-page continuation" in value)
    items = json.loads(prompt.rsplit("## Here is the input:", 1)[1].strip())
    assert any(LEFT_TAIL + " several independent experiments." in item["input"] for item in items)

    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    left = pages[0]["pdf_paragraph"][-1]["unicode"]
    right = pages[1]["pdf_paragraph"][0]["unicode"]
    visible = re.sub(r"\{v\d+\}|</?b\d+>", "", left + right)
    assert visible == TRANSLATED_PREFIX + TRANSLATED_SENTENCE


def test_following_sentence_is_separate_and_stays_on_its_page(tmp_path):
    document = document_with_caption(caption_label="text", caption_size=10, obstacle=False)
    translator = run(document, tmp_path)
    prompt = next(value for value in translator.requests if "## Cross-page continuation" in value)
    items = json.loads(prompt.rsplit("## Here is the input:", 1)[1].strip())
    inputs = [re.sub(r"\{v\d+\}|</?b\d+>", "", item["input"]) for item in items]
    continuation = LEFT_TAIL + " several independent experiments."
    following = "Additional observations follow."
    assert any(continuation in value and following not in value for value in inputs)
    assert any(following in value and continuation not in value for value in inputs)

    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    left = pages[0]["pdf_paragraph"][-1]["unicode"]
    right = pages[1]["pdf_paragraph"][0]["unicode"]
    assert TRANSLATED_PREFIX in left and TRANSLATED_PREFIX not in right
    assert TRANSLATED_FOLLOWING in right and TRANSLATED_FOLLOWING not in left
    visible = re.sub(r"\{v\d+\}|</?b\d+>", "", left + right)
    assert visible == TRANSLATED_PREFIX + TRANSLATED_SENTENCE + TRANSLATED_FOLLOWING


def test_unproven_different_styles_do_not_reflow_across_pages(tmp_path):
    control = document_with_caption(caption_label="text", caption_size=10, obstacle=False)
    control_dir = tmp_path / "control"
    control_dir.mkdir()
    assert run(control, control_dir).cross_page_requests == 1

    styled = document_with_caption(
        caption_label="text", caption_size=10, obstacle=False,
        left_style_parts=(LEFT_PREFIX, LEFT_TAIL),
        right_style_parts=("several independent experiments.", " Additional observations follow."),
    )
    styled_dir = tmp_path / "styled"
    styled_dir.mkdir()
    translator = run(styled, styled_dir)
    assert translator.requests
    assert translator.cross_page_requests == 0
    pages = json.loads(styled.parser_artifact_json())["document"]["page"]
    left = pages[0]["pdf_paragraph"][-1]["unicode"]
    right = pages[1]["pdf_paragraph"][0]["unicode"]
    assert TRANSLATED_FOLLOWING not in left
    assert TRANSLATED_FOLLOWING in right


@pytest.mark.parametrize("split_at", [None, 1, 2], ids=["same-style", "boundary-before-period", "boundary-after-period"])
def test_style_boundary_around_continuation_end(tmp_path, split_at):
    left_tail = "The observation was repeated across"
    first_style = "several locations, including the preliminary result "
    second_style = "that was later revised. A separate sentence remains here."
    left_text = "Earlier evidence was clear. " + left_tail
    right_text = first_style + second_style
    right_parts = {
        None: (right_text,),
        1: (first_style, second_style),
        2: (first_style + "that was later revised.", " A separate sentence remains here."),
    }[split_at]

    class AmbiguousTranslator(DirectTranslator):
        def llm_translate(self, prompt, rate_limit_params=None):
            response = super().llm_translate(prompt, rate_limit_params=rate_limit_params)
            if "## Cross-page continuation" not in prompt or "## Here is the input:" not in prompt:
                return response
            items = json.loads(response)
            for item in items:
                item["output"] = item["output"].replace(
                    "several locations, including", "several locations. Including",
                )
            return json.dumps(items)

    def whole_inputs(translator, require_separate_following=False):
        result = []
        for prompt in translator.requests:
            if "## Cross-page continuation" not in prompt or "## Here is the input:" not in prompt:
                continue
            items = json.loads(prompt.rsplit("## Here is the input:", 1)[1].strip())
            texts = [re.sub(r"</?b\d+>|\{v\d+\}", "", item["input"]) for item in items]
            for index, text in enumerate(texts):
                if left_tail in text and first_style.strip() in text:
                    following = "A separate sentence remains here."
                    if not require_separate_following or (
                        following not in text
                        and any(following in other for other_index, other in enumerate(texts) if other_index != index)
                    ):
                        result.append(text)
        return result

    control = document_with_caption(
        caption_label="text", caption_size=10, obstacle=False,
        left_text=left_text, right_text=first_style + "that was later revised.",
        left_style_parts=(left_text,), right_style_parts=(first_style + "that was later revised.",),
    )
    control_service = DirectApplicationService()
    control_service.translator = AmbiguousTranslator()
    control_dir = tmp_path / "control"
    control_dir.mkdir()
    run_bridge(control, control_dir, control_service, _rust_il.CancelFlag(), Monitor())
    assert whole_inputs(control_service.translator)

    mixed = document_with_caption(
        caption_label="text", caption_size=10, obstacle=False,
        left_text=left_text, right_text=right_text,
        left_style_parts=(left_text,), right_style_parts=right_parts,
    )
    service = DirectApplicationService()
    service.translator = AmbiguousTranslator()
    mixed_dir = tmp_path / "mixed"
    mixed_dir.mkdir()
    run_bridge(mixed, mixed_dir, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.requests
    if split_at == 1:
        assert service.translator.cross_page_requests == 0
        assert not whole_inputs(service.translator)
    else:
        assert whole_inputs(service.translator, require_separate_following=True)
        dedicated_index = next(
            index for index, prompt in enumerate(service.translator.requests)
            if "## Cross-page continuation" in prompt
        )
        assert any(
            "## Here is the input:" not in prompt
            for prompt in service.translator.requests[dedicated_index + 1:]
        )
    pages = json.loads(mixed.parser_artifact_json())["document"]["page"]
    left_page = " ".join(paragraph["unicode"] for paragraph in pages[0]["pdf_paragraph"])
    right_page = " ".join(paragraph["unicode"] for paragraph in pages[1]["pdf_paragraph"])
    assert "A separate sentence remains here." not in left_page
    assert "A separate sentence remains here." in right_page
    assert "several locations. Including" not in left_page + right_page


@pytest.mark.parametrize(
    ("caption_label", "next_page_number"),
    [("figure_caption", 1), ("text", 1)],
    ids=["labelled-caption", "caption-mislabelled-body"],
)
def test_caption_and_body_are_not_a_cross_page_pair(tmp_path, caption_label, next_page_number):
    document = document_with_caption(caption_label=caption_label, next_page_number=next_page_number)
    translator = run(document, tmp_path)
    assert translator.requests
    assert translator.cross_page_requests == 0
