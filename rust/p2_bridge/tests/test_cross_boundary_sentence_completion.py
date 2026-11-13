import copy
import json

import pytest
from babeldoc import _rust_il
from babeldoc.format.pdf.document_il import il_version_1 as il
from babeldoc.rust_translation_service import RustTranslatorService
from test_cross_boundary_integration import SyntheticTranslator
from test_cross_boundary_integration import document
from test_cross_page_llm_direct_atomicity import Monitor
from test_cross_page_llm_direct_atomicity import run_bridge


def boundary_items(prompt):
    return [item["text"] for item in json.loads(prompt.split(" Input: ", 1)[1])]


@pytest.mark.parametrize(
    "label,text",
    [
        ("text", "severe damage and"),
        (None, "severe damage and"),
        ("text", "Fig. 1.2 suggests that treatment was"),
    ],
)
def test_model_cannot_skip_body_using_nonbody_reason(tmp_path, label, text):
    native = document(
        [
            [
                ("Earlier result. The treatment caused", "text", (50, 600, 250, 650)),
                (text, label, (50, 500, 250, 550)),
            ],
            [("death in patients. Later.", "text", (50, 600, 250, 650))],
        ]
    )
    translator = SyntheticTranslator(
        lambda _: {
            "status": "selected",
            "ranges": [
                {"page": 0, "paragraph": 0, "text": "The treatment caused"},
                {"page": 1, "paragraph": 0, "text": "death in patients."},
            ],
            "joins": ["space"],
            "skipped": [{"page": 0, "paragraph": 1, "reason": "figure_label"}],
        }
    )
    run_bridge(
        native,
        tmp_path,
        RustTranslatorService(translator),
        _rust_il.CancelFlag(),
        Monitor(),
    )
    assert all(" Input: " not in prompt for prompt, _ in translator.boundary_requests)
    report = json.loads(
        (tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8")
    )
    assert any("body content" in item["reason"] for item in report["unresolved"])


def test_multiple_captions_do_not_hide_body_target(tmp_path):
    native = document(
        [
            [
                ("Earlier. The treatment improved", "text", (50, 600, 250, 650)),
                ("A chart describes trends.", "caption", (50, 400, 250, 450)),
                ("Another panel shows counts.", "caption", (50, 300, 250, 350)),
            ],
            [("survival in patients. Later.", "text", (50, 600, 250, 650))],
        ]
    )
    translator = SyntheticTranslator(lambda _: {"status": "none"})
    run_bridge(
        native,
        tmp_path,
        RustTranslatorService(translator),
        _rust_il.CancelFlag(),
        Monitor(),
    )
    assert any(
        candidate["paragraph"] == 0 for candidate in translator.metadata[0]["left"]
    )


def test_sentence_expansion_skips_caption_and_includes_predicate(tmp_path):
    native = document(
        [
            [("Earlier. The study showed that", "text", (50, 600, 250, 650))],
            [
                ("the patient cohort", "text", (50, 650, 250, 700)),
                ("Caption regarding figure.", "caption", (50, 500, 250, 550)),
                (
                    "included 300 patients and improved survival. Later.",
                    "text",
                    (50, 400, 250, 450),
                ),
            ],
        ]
    )
    translator = SyntheticTranslator(
        lambda _: {
            "status": "selected",
            "ranges": [
                {"page": 0, "paragraph": 0, "text": "The study showed that"},
                {"page": 1, "paragraph": 0, "text": "the patient cohort"},
            ],
            "joins": ["space"],
        }
    )
    run_bridge(
        native,
        tmp_path,
        RustTranslatorService(translator),
        _rust_il.CancelFlag(),
        Monitor(),
    )
    items = boundary_items(
        next(
            prompt for prompt, _ in translator.boundary_requests if " Input: " in prompt
        )
    )
    assert (
        items[0]
        == "The study showed that the patient cohort included 300 patients and improved survival."
    )
    assert "Caption" not in items[0]


def test_spaced_numbered_reference_stays_with_sentence(tmp_path):
    native = document(
        [
            [("Earlier. The study improved", "text", (50, 600, 250, 650))],
            [("outcomes. [12] Later.", "text", (50, 600, 250, 650))],
        ]
    )
    translator = SyntheticTranslator(
        lambda _: {
            "status": "selected",
            "ranges": [
                {"page": 0, "paragraph": 0, "text": "The study improved"},
                {"page": 1, "paragraph": 0, "text": "outcomes."},
            ],
            "joins": ["space"],
        }
    )
    run_bridge(
        native,
        tmp_path,
        RustTranslatorService(translator),
        _rust_il.CancelFlag(),
        Monitor(),
    )
    items = boundary_items(
        next(
            prompt for prompt, _ in translator.boundary_requests if " Input: " in prompt
        )
    )
    assert items[0] == "The study improved outcomes. [12]"
    assert " Later." in items


def test_formula_at_start_of_sentence_stays_with_that_sentence(tmp_path):
    style = il.PdfStyle(font_id="regular", font_size=10.0)
    pages = []
    for page_index in range(2):
        if page_index == 0:
            text = "Earlier result. CD47 expression increased in"
            compositions = [
                il.PdfParagraphComposition(
                    pdf_character=il.PdfCharacter(
                        pdf_style=style, char_unicode="Earlier result. "
                    )
                ),
                il.PdfParagraphComposition(
                    pdf_formula=il.PdfFormula(
                        pdf_character=[
                            il.PdfCharacter(pdf_style=style, char_unicode="CD47")
                        ]
                    )
                ),
                il.PdfParagraphComposition(
                    pdf_character=il.PdfCharacter(
                        pdf_style=style, char_unicode=" expression increased in"
                    )
                ),
            ]
        else:
            text = "treated patients. Later."
            compositions = [
                il.PdfParagraphComposition(
                    pdf_character=il.PdfCharacter(pdf_style=style, char_unicode=text)
                )
            ]
        pages.append(
            copy.deepcopy(
                il.Page(
                    page_number=page_index,
                    cropbox=il.Cropbox(box=il.Box(x=0, y=0, x2=600, y2=800)),
                    pdf_font=[
                        il.PdfFont(
                            font_id="regular",
                            bold=False,
                            italic=False,
                            monospace=False,
                            serif=False,
                        )
                    ],
                    pdf_paragraph=[
                        il.PdfParagraph(
                            box=il.Box(x=50, y=600, x2=250, y2=650),
                            pdf_style=style,
                            unicode=text,
                            layout_label="text",
                            pdf_paragraph_composition=compositions,
                        )
                    ],
                )
            )
        )
    native = _rust_il.transfer_document(il.Document(page=pages, total_pages=2))
    translator = SyntheticTranslator(
        lambda _: {
            "status": "selected",
            "ranges": [
                {"page": 0, "paragraph": 0, "text": "expression increased in"},
                {"page": 1, "paragraph": 0, "text": "treated patients."},
            ],
            "joins": ["space"],
        }
    )
    run_bridge(
        native,
        tmp_path,
        RustTranslatorService(translator),
        _rust_il.CancelFlag(),
        Monitor(),
    )
    items = boundary_items(
        next(
            prompt for prompt, _ in translator.boundary_requests if " Input: " in prompt
        )
    )
    assert items[0].startswith("[[BABEL_")
    assert "expression increased in treated patients." in items[0]
    assert items[1] == "Earlier result. "


@pytest.mark.parametrize(
    "left,right,selected_left,selected_right,expected",
    [
        (
            "Earlier result. This article describes major discoveries, which owing to improved models has",
            "driven our understanding. Later work follows.",
            "which owing to improved models has",
            "driven our understanding.",
            "This article describes major discoveries, which owing to improved models has driven our understanding.",
        ),
        (
            "Earlier result. For example, in situ",
            "hybridization analyses of samples from 302 patients reveal distinct subgroups. Later work follows.",
            "For example, in situ",
            "hybridization analyses of samples from 302 patients",
            "For example, in situ hybridization analyses of samples from 302 patients reveal distinct subgroups.",
        ),
        (
            "Earlier result. In 2009, Majeti et al. reported that",
            "CD47 expression was a marker of poor prognosis. Later work follows.",
            "Majeti et al. reported that",
            "CD47 expression was a marker",
            "In 2009, Majeti et al. reported that CD47 expression was a marker of poor prognosis.",
        ),
    ],
)
def test_partial_model_ranges_expand_to_entire_sentence(
    tmp_path, left, right, selected_left, selected_right, expected
):
    native = document(
        [[(left, "text", (50, 500, 250, 550))], [(right, "text", (50, 500, 250, 550))]]
    )
    decision = {
        "status": "selected",
        "ranges": [
            {"page": 0, "paragraph": 0, "text": selected_left},
            {"page": 1, "paragraph": 0, "text": selected_right},
        ],
        "joins": ["space"],
    }
    translator = SyntheticTranslator(lambda _metadata: decision)
    run_bridge(
        native,
        tmp_path,
        RustTranslatorService(translator),
        _rust_il.CancelFlag(),
        Monitor(),
    )
    requests = [
        boundary_items(prompt)
        for prompt, _ in translator.boundary_requests
        if " Input: " in prompt
    ]
    assert len(requests) == 1
    assert requests[0][0] == expected
    assert "Earlier result. " in requests[0]
    assert " Later work follows." in requests[0]


def test_skip_reason_is_description_not_unpublished_enum(tmp_path):
    native = document(
        [
            [
                ("Earlier. The finding indicates", "text", (50, 500, 250, 550)),
                ("Figure axis text", "caption", (50, 100, 250, 140)),
            ],
            [("a complete effect. Later.", "text", (50, 500, 250, 550))],
        ]
    )
    translator = SyntheticTranslator(
        lambda _metadata: {
            "status": "selected",
            "ranges": [
                {"page": 0, "paragraph": 0, "text": "The finding indicates"},
                {"page": 1, "paragraph": 0, "text": "a complete effect."},
            ],
            "joins": ["space"],
            "skipped": [{"page": 0, "paragraph": 1, "reason": "figure_label"}],
        }
    )
    run_bridge(
        native,
        tmp_path,
        RustTranslatorService(translator),
        _rust_il.CancelFlag(),
        Monitor(),
    )
    requests = [
        boundary_items(prompt)
        for prompt, _ in translator.boundary_requests
        if " Input: " in prompt
    ]
    assert requests[0][0] == "The finding indicates a complete effect."
    assert (
        len(
            [
                prompt
                for prompt, _ in translator.boundary_requests
                if "Metadata: " in prompt
            ]
        )
        == 1
    )
