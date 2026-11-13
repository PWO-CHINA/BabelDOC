import json
import re

import pytest
from babeldoc import _rust_il
from babeldoc.format.pdf.document_il import il_version_1 as il
from test_cross_page_llm_direct_atomicity import DirectApplicationService
from test_cross_page_llm_direct_atomicity import DirectTranslator
from test_cross_page_llm_direct_atomicity import Monitor
from test_cross_page_llm_direct_atomicity import report_of
from test_cross_page_llm_direct_atomicity import run_bridge
from test_cross_page_llm_direct_atomicity import source_characters
from test_crosspage_formula_boundary_validation import synthetic_substitution

PREFIX = "Earlier findings were clear. "
LEFT_TAIL = "The measured change was {left_number} units after"
RIGHT_CONTINUATION = "adjustment for {right_number} observations."
FOLLOWING = " Additional observations follow."


class SignedNumberTranslator(DirectTranslator):
    def __init__(self, expression, replacement, texts, *, valid_retry=False):
        super().__init__()
        self.expression = expression
        self.replacement = replacement
        self.valid_retry = valid_retry
        self.left_tail = texts[0].removeprefix(PREFIX)
        self.continuation = texts[1].removesuffix(FOLLOWING)
        self.dedicated_inputs = []
        self.dedicated_outputs = []
        self.dedicated_retries = []

    def substitute(self, text):
        return self.expression.join(synthetic_substitution(part) for part in text.split(self.expression))

    def ordinary(self, text):
        return self.substitute(text)

    def enhanced(self, text):
        return self.substitute(text)

    def llm_translate(self, prompt, rate_limit_params=None):
        response = super().llm_translate(prompt, rate_limit_params)
        if (rate_limit_params or {}).get("cross_boundary_request") and " Input: " in prompt:
            items = json.loads(prompt.split(" Input: ", 1)[1])
            outputs = json.loads(response)["outputs"]
            self.dedicated_inputs.append(items)
            attempt = re.search(r"Attempt: (\d+);", prompt)
            assert attempt is not None
            self.dedicated_retries.append(int(attempt.group(1)) > 0)
            if self.replacement is not None and (not self.valid_retry or self.cross_page_requests == 1):
                matches = [item for item in outputs if self.expression in item["text"]]
                assert len(matches) == 1 and matches[0]["text"].count(self.expression) == 1
                matches[0]["text"] = matches[0]["text"].replace(self.expression, self.replacement)
            self.dedicated_outputs.append(outputs)
            return json.dumps({"outputs": outputs}, ensure_ascii=False)
        return response


def signed_document(signed_page, expression, following):
    left_number, right_number = (expression, "15") if signed_page == "left" else ("15", expression)
    right_text = RIGHT_CONTINUATION.format(right_number=right_number) + (FOLLOWING if following else "")
    texts = (PREFIX + LEFT_TAIL.format(left_number=left_number), right_text)
    pages = []
    for index, text in enumerate(texts):
        style = il.PdfStyle(font_id="regular", font_size=10.0)
        bounds = (60, 100 if index == 0 else 650, 540, 150 if index == 0 else 700)
        paragraph = il.PdfParagraph(
            box=il.Box(x=bounds[0], y=bounds[1], x2=bounds[2], y2=bounds[3]),
            pdf_style=style,
            unicode=text,
            debug_id=f"signed-number-{index}",
            layout_label="text",
            pdf_paragraph_composition=[il.PdfParagraphComposition(pdf_character=character)
                                       for character in source_characters(text, style, bounds)],
        )
        pages.append(il.Page(
            page_number=index,
            cropbox=il.Cropbox(box=il.Box(x=0, y=0, x2=600, y2=800)),
            pdf_font=[il.PdfFont(font_id="regular", bold=False, italic=False, monospace=False, serif=False)],
            pdf_paragraph=[paragraph],
        ))
    return _rust_il.transfer_document(il.Document(page=pages, total_pages=2)), texts


def assert_dedicated_sentence(translator, texts, replacement, following, *, valid_retry=False):
    request_count = translator.cross_page_requests
    assert request_count == (2 if valid_retry else 1 if replacement is None else 3)
    assert len(translator.dedicated_inputs) == len(translator.dedicated_outputs) == request_count
    assert translator.dedicated_retries == [False] + [True] * (request_count - 1)
    sentence = texts[0].removeprefix(PREFIX) + " " + (texts[1].removesuffix(FOLLOWING) if following else texts[1])
    initial_inputs = translator.dedicated_inputs[0]
    matching = [item for item in initial_inputs if item["text"] == sentence]
    assert len(matching) == 1
    assert len(initial_inputs) == (3 if following else 2)
    if following:
        assert any(item["text"].strip() == FOLLOWING.strip() for item in initial_inputs)
    for index, (inputs, outputs) in enumerate(zip(translator.dedicated_inputs, translator.dedicated_outputs, strict=True)):
        if index:
            assert inputs == matching
        assert {item["id"] for item in outputs} == {item["id"] for item in inputs}
        output = next(item["text"] for item in outputs if item["id"] == matching[0]["id"])
        expected = translator.substitute(sentence)
        if replacement is not None and (not valid_retry or index == 0):
            expected = expected.replace(translator.expression, replacement)
        assert output == expected


def assert_pages_and_tracking(document, tmp_path, signed_page, expression, following, texts, translator):
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    left, right = [page["pdf_paragraph"][0]["unicode"] for page in pages]
    assert "".join((left + right).split()) == "".join(translator.substitute(" ".join(texts)).split())
    assert [expression in left, expression in right] == [signed_page == "left", signed_page == "right"]
    assert ["15" in left, "15" in right] == [signed_page == "right", signed_page == "left"]
    if following:
        assert synthetic_substitution(FOLLOWING.strip()) in right
        assert synthetic_substitution(FOLLOWING.strip()) not in left
    tracking = json.loads((tmp_path / "translate_tracking.json").read_text(encoding="utf-8"))
    assert len(tracking["cross_page"]) == 1


def ordinary_control(tmp_path, signed_page, expression, following):
    control_dir = tmp_path / "ordinary-control"
    control_dir.mkdir()
    document, texts = signed_document(signed_page, expression, following)
    service = DirectApplicationService()
    service.translator = SignedNumberTranslator(expression, None, texts)
    run_bridge(document, control_dir, service, _rust_il.CancelFlag(), Monitor(), cross_page=False)
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    tracking = json.loads((control_dir / "translate_tracking.json").read_text(encoding="utf-8"))
    return [page["pdf_paragraph"][0]["unicode"] for page in pages], tracking


@pytest.mark.parametrize("signed_page", ["left", "right"])
@pytest.mark.parametrize(
    "expression", ["-7", "+7", "−7", "-0.5", "7%", "1e-3"],
    ids=["negative", "positive", "unicode-minus", "negative-decimal", "percentage", "exponent"],
)
@pytest.mark.parametrize("following", [False, True], ids=["two-items", "three-items"])
def test_signed_numbers_are_preserved_on_original_pages(tmp_path, signed_page, expression, following):
    document, texts = signed_document(signed_page, expression, following)
    service = DirectApplicationService()
    service.translator = SignedNumberTranslator(expression, None, texts)
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert_dedicated_sentence(service.translator, texts, None, following)
    assert service.translator.fallback_requests == 0
    assert_pages_and_tracking(document, tmp_path, signed_page, expression, following, texts, service.translator)


@pytest.mark.parametrize("signed_page", ["left", "right"])
@pytest.mark.parametrize("following", [False, True], ids=["two-items", "three-items"])
@pytest.mark.parametrize(
    ("expression", "replacement"),
    [("-7", "7"), ("+7", "-7"), ("−7", "7"), ("-0.5", "0.5"), ("7%", "7"), ("1e-3", "1e3")],
    ids=["negative", "positive", "unicode-minus", "negative-decimal", "percentage", "exponent"],
)
def test_numeric_expression_recovered_on_retry(tmp_path, signed_page, following, expression, replacement):
    document, texts = signed_document(signed_page, expression, following)
    before = document.parser_artifact_hash()
    service = DirectApplicationService()
    service.translator = SignedNumberTranslator(expression, replacement, texts, valid_retry=True)
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert_dedicated_sentence(service.translator, texts, replacement, following, valid_retry=True)
    assert service.translator.fallback_requests == 0
    assert document.parser_artifact_hash() != before
    assert_pages_and_tracking(document, tmp_path, signed_page, expression, following, texts, service.translator)


@pytest.mark.parametrize("signed_page", ["left", "right"])
@pytest.mark.parametrize("following", [False, True], ids=["two-items", "three-items"])
@pytest.mark.parametrize(
    ("expression", "replacement"),
    [
        ("-7", "7"), ("-7", "+7"), ("+7", "-7"), ("−7", "7"), ("-0.5", "0.5"),
        ("7%", "7"), ("1e-3", "1e3"), ("1e-3", "1e+3"),
    ],
    ids=[
        "sign-removed", "sign-flipped", "positive-flipped", "unicode-minus-removed", "decimal-sign-removed",
        "percent-removed", "exponent-sign-removed", "exponent-sign-flipped",
    ],
)
def test_corrupted_signed_number_rejects_transaction(tmp_path, signed_page, expression, replacement, following):
    document, texts = signed_document(signed_page, expression, following)
    before = document.parser_artifact_hash()
    ordinary_pages, ordinary_tracking = ordinary_control(tmp_path, signed_page, expression, following)
    service = DirectApplicationService()
    service.translator = SignedNumberTranslator(expression, replacement, texts)
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert_dedicated_sentence(service.translator, texts, replacement, following)
    assert service.translator.fallback_requests == 0
    assert document.parser_artifact_hash() != before
    report = report_of(tmp_path)
    assert report["baseline_completed"] and report["applied_groups"] == 0
    assert "numeric expression or mathematical symbol changed" in str(report["unresolved"])
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    actual_pages = [page["pdf_paragraph"][0]["unicode"] for page in pages]
    assert actual_pages == ordinary_pages
    target_page = 0 if signed_page == "left" else 1
    assert expression in actual_pages[target_page]
    assert actual_pages[target_page] != ordinary_pages[target_page].replace(expression, replacement, 1)
    assert json.loads((tmp_path / "translate_tracking.json").read_text(encoding="utf-8")) == ordinary_tracking
    assert_pages_and_tracking(document, tmp_path, signed_page, expression, following, texts, service.translator)


@pytest.mark.parametrize(
    ("expression", "replacement"),
    [("-7", "7"), ("7%", "7"), ("1e-3", "1e3")],
    ids=["sign", "percentage", "exponent"],
)
def test_corrupted_number_preserves_existing_tracking(tmp_path, expression, replacement):
    document, texts = signed_document("left", expression, True)
    before = document.parser_artifact_hash()
    ordinary_pages, ordinary_tracking = ordinary_control(tmp_path, "left", expression, True)
    tracking = tmp_path / "translate_tracking.json"
    original_tracking = b'{"sentinel": true}\n'
    tracking.write_bytes(original_tracking)
    service = DirectApplicationService()
    service.translator = SignedNumberTranslator(expression, replacement, texts)

    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())

    assert_dedicated_sentence(service.translator, texts, replacement, True)
    assert service.translator.fallback_requests == 0
    assert document.parser_artifact_hash() != before
    report = report_of(tmp_path)
    assert report["baseline_completed"] and report["applied_groups"] == 0
    assert "numeric expression or mathematical symbol changed" in str(report["unresolved"])
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    actual_pages = [page["pdf_paragraph"][0]["unicode"] for page in pages]
    assert actual_pages == ordinary_pages
    assert expression in actual_pages[0]
    assert actual_pages[0] != ordinary_pages[0].replace(expression, replacement, 1)
    assert tracking.read_bytes() != original_tracking
    assert json.loads(tracking.read_text(encoding="utf-8")) == ordinary_tracking
    assert not list(tmp_path.glob("translate_tracking.json.*.tmp"))
