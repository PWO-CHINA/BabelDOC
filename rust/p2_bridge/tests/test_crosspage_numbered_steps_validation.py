import json

import pytest
from babeldoc import _rust_il
from test_cross_page_llm_direct_atomicity import DirectApplicationService
from test_cross_page_llm_direct_atomicity import Monitor
from test_cross_page_llm_direct_atomicity import report_of
from test_cross_page_llm_direct_atomicity import run_bridge
from test_cross_page_sample_validation import document_with_caption
from test_crosspage_formula_boundary_validation import ReversibleTranslator
from test_crosspage_formula_boundary_validation import synthetic_substitution


@pytest.mark.parametrize("step_number", ["2", "10"])
@pytest.mark.parametrize("separator", [" ", "  "], ids=["single-space", "double-space"])
def test_numbered_step_is_not_a_cross_page_continuation(tmp_path, step_number, separator):
    document = document_with_caption(
        caption_label="text", caption_size=10, obstacle=False,
        left_text="Earlier measurements were complete. The first operation involved",
        right_text=f"{step_number}.{separator}Calibrate the measurement system carefully.",
    )
    service = DirectApplicationService()
    service.translator = ReversibleTranslator()
    service.translator.left_tail = "The first operation involved"
    service.translator.continuation = f"{step_number}.{separator}Calibrate the measurement system carefully."
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.requests
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    left = pages[0]["pdf_paragraph"][-1]["unicode"]
    right = pages[1]["pdf_paragraph"][0]["unicode"]
    assert f"{step_number}." not in left
    assert f"{step_number}." in right
    assert service.translator.cross_page_requests == 0


@pytest.mark.parametrize("step_number", ["2", "10"])
def test_preceding_decimal_is_not_merged_into_numbered_step(tmp_path, step_number):
    prior = "The reference level was -0.5."
    step_tail = "The second operation involved"
    right = "several independent checks before the sample was analyzed."
    document = document_with_caption(
        caption_label="text", caption_size=10, obstacle=False,
        left_text=f"{prior} {step_number}. {step_tail}", right_text=right,
    )
    service = DirectApplicationService()
    service.translator = ReversibleTranslator()
    service.translator.left_tail = step_tail
    service.translator.continuation = right
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.cross_page_requests >= 1
    for items in service.translator.translation_items:
        matches = [item["text"] for item in items if step_tail + " " + right in item["text"]]
        assert len(matches) == 1
        assert prior not in matches[0]
    report = report_of(tmp_path)
    assert report["baseline_completed"]
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    left = pages[0]["pdf_paragraph"][-1]["unicode"]
    right_output = pages[1]["pdf_paragraph"][0]["unicode"]
    assert synthetic_substitution(prior) in left
    assert synthetic_substitution(prior) not in right_output
    assert f"{step_number}." in left
    assert f"{step_number}." not in right_output
