import copy

from babeldoc import _rust_il
from babeldoc.format.pdf.document_il import il_version_1 as il
from babeldoc.rust_translation_service import RustTranslatorService
from test_cross_boundary_integration import SyntheticTranslator
from test_cross_page_llm_direct_atomicity import Monitor
from test_cross_page_llm_direct_atomicity import run_bridge


def candidate_text(tmp_path, text, compositions, fonts=None):
    style = il.PdfStyle(font_id="regular", font_size=10)
    pages = []
    for page_index, (value, parts) in enumerate(
        [
            (text, compositions),
            (
                "continues across the boundary.",
                [
                    il.PdfParagraphComposition(
                        pdf_character=il.PdfCharacter(
                            pdf_style=style,
                            char_unicode="continues across the boundary.",
                        )
                    )
                ],
            ),
        ]
    ):
        pages.append(
            copy.deepcopy(
                il.Page(
                    page_number=page_index,
                    cropbox=il.Cropbox(box=il.Box(x=0, y=0, x2=600, y2=800)),
                    pdf_font=fonts
                    or [
                        il.PdfFont(
                            font_id="regular",
                            bold=False,
                            italic=False,
                            serif=False,
                            monospace=False,
                        )
                    ],
                    pdf_paragraph=[
                        il.PdfParagraph(
                            box=il.Box(x=50, y=500, x2=250, y2=550),
                            pdf_style=style,
                            unicode=value,
                            debug_id=f"p{page_index}",
                            layout_label="text",
                            pdf_paragraph_composition=parts,
                        )
                    ],
                )
            )
        )
    native = _rust_il.transfer_document(il.Document(page=pages, total_pages=2))
    translator = SyntheticTranslator(lambda _: {"status": "none"})
    run_bridge(
        native,
        tmp_path,
        RustTranslatorService(translator),
        _rust_il.CancelFlag(),
        Monitor(),
    )
    return translator.metadata[0]["left"][0]


def test_scientific_superscripts_survive_multiple_compositions(tmp_path):
    style = il.PdfStyle(font_id="regular", font_size=10)
    parts = [
        il.PdfParagraphComposition(
            pdf_character=il.PdfCharacter(pdf_style=style, char_unicode=value)
        )
        for value in ["Value ", "10⁻³ continues"]
    ]
    candidate = candidate_text(tmp_path, "Value 10⁻³ continues", parts)
    assert candidate["selectable"] is True
    assert candidate["text"] == "Value 10⁻³ continues"


def test_plain_gene_name_without_geometry_is_not_rejected(tmp_path):
    style = il.PdfStyle(font_id="regular", font_size=10)
    parts = [
        il.PdfParagraphComposition(
            pdf_character=il.PdfCharacter(
                pdf_style=style, char_unicode="BRCA1 expression"
            )
        )
    ]
    candidate = candidate_text(tmp_path, "BRCA1 expression", parts)
    assert candidate["selectable"] is True
    assert candidate["text"] == "BRCA1 expression"


def test_style_markers_do_not_hide_geometric_word_space(tmp_path):
    normal = il.PdfStyle(font_id="regular", font_size=10)
    bold = il.PdfStyle(font_id="bold", font_size=10)
    parts = [
        il.PdfParagraphComposition(
            pdf_character=il.PdfCharacter(
                pdf_style=normal,
                char_unicode="The",
                box=il.Box(x=50, y=500, x2=65, y2=510),
            )
        ),
        il.PdfParagraphComposition(
            pdf_same_style_characters=il.PdfSameStyleCharacters(
                pdf_style=bold,
                pdf_character=[
                    il.PdfCharacter(
                        pdf_style=bold,
                        char_unicode="model",
                        box=il.Box(x=70, y=500, x2=95, y2=510),
                    ),
                ],
            )
        ),
    ]
    fonts = [
        il.PdfFont(
            font_id=name, bold=is_bold, italic=False, serif=False, monospace=False
        )
        for name, is_bold in [("regular", False), ("bold", True)]
    ]
    candidate = candidate_text(tmp_path, "The model", parts, fonts)
    assert candidate["selectable"] is True
    assert candidate["text"] == "The model"


def test_ordinary_style_markers_keep_geometric_word_space(tmp_path):
    import json
    import re

    from test_cross_boundary_integration import source_characters

    normal = il.PdfStyle(font_id="regular", font_size=10)
    bold = il.PdfStyle(font_id="bold", font_size=10)
    parts = [
        il.PdfParagraphComposition(pdf_same_style_characters=il.PdfSameStyleCharacters(
            pdf_style=bold, pdf_character=source_characters("at", bold, (50, 500, 62, 550)),
        )),
        il.PdfParagraphComposition(pdf_same_style_characters=il.PdfSameStyleCharacters(
            pdf_style=il.PdfStyle(font_id="link", font_size=10),
            pdf_character=source_characters("https", il.PdfStyle(font_id="link", font_size=10), (70, 500, 100, 550)),
        )),
    ]
    fonts = [il.PdfFont(font_id=name, bold=is_bold, italic=False, serif=False, monospace=False)
             for name, is_bold in [("regular", False), ("bold", True), ("link", True)]]
    first = il.PdfParagraph(
        box=il.Box(x=50, y=500, x2=250, y2=550), pdf_style=normal,
        unicode="at https", layout_label="text", debug_id="ordinary-style-space", pdf_paragraph_composition=parts,
    )
    native = _rust_il.transfer_document(il.Document(page=[il.Page(
        page_number=0, cropbox=il.Cropbox(box=il.Box(x=0, y=0, x2=600, y2=800)),
        pdf_font=fonts, pdf_paragraph=[first],
    )], total_pages=1))
    translator = SyntheticTranslator(lambda _: {"status": "none"})
    observed = []
    original = translator.llm_translate

    def record(prompt, rate_limit_params=None):
        if "## Here is the input:" in prompt:
            items = json.loads(prompt.rsplit("## Here is the input:", 1)[1])
            observed.extend(item["input"] for item in items)
        elif "Now translate the following text:" in prompt:
            observed.append(prompt.rsplit("Now translate the following text:", 1)[1].strip())
        return original(prompt, rate_limit_params)

    translator.llm_translate = record
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor(), False)
    assert any(re.sub(r"<[^>]*>", "", text) == "at https" for text in observed), observed


def test_ligature_whitespace_and_scientific_sources_remain_selectable(tmp_path):
    style = il.PdfStyle(font_id="regular", font_size=10)
    parts = [
        il.PdfParagraphComposition(pdf_character=il.PdfCharacter(pdf_style=style, char_unicode="An  e")),
        il.PdfParagraphComposition(pdf_same_style_characters=il.PdfSameStyleCharacters(
            pdf_style=style, pdf_character=[il.PdfCharacter(pdf_style=style, char_unicode="ﬀ")],
        )),
        il.PdfParagraphComposition(pdf_character=il.PdfCharacter(pdf_style=style, char_unicode="ect of ")),
        il.PdfParagraphComposition(pdf_formula=il.PdfFormula(pdf_character=[
            il.PdfCharacter(pdf_style=style, char_unicode="10⁻³"),
        ])),
        il.PdfParagraphComposition(pdf_character=il.PdfCharacter(pdf_style=style, char_unicode=" on H₂O")),
    ]
    candidate = candidate_text(tmp_path, "An effect of 10−3 on H2O", parts)
    assert candidate["selectable"] is True
    assert candidate["text"].startswith("An effect of [[BABEL_F_")
    assert candidate["text"].endswith(" on H₂O")


def test_literal_placeholder_in_unselected_candidate_is_local_failure(tmp_path):
    from test_cross_boundary_integration import LEFT
    from test_cross_boundary_integration import document
    from test_cross_boundary_integration import select

    native = document(
        [[LEFT, ("Literal {v1}", "caption", (50, 100, 250, 130))], [LEFT]]
    )
    translator = SyntheticTranslator(select((0, 0), (1, 0)))
    run_bridge(
        native,
        tmp_path,
        RustTranslatorService(translator),
        _rust_il.CancelFlag(),
        Monitor(),
    )
    candidate = next(
        item for item in translator.metadata[0]["left"] if item["paragraph"] == 1
    )
    assert candidate["selectable"] is False
    assert (tmp_path / "cross_boundary_report.json").is_file()
