import asyncio
import json
import re
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest
from babeldoc import _rust_il
from babeldoc.format.pdf.document_il import il_version_1 as il
from babeldoc.rust_run_context import PriorityServiceDispatcher
from babeldoc.rust_translation_service import RustTranslatorService

LEFT_PREFIX = "Earlier findings were clear. "
LEFT_TAIL = "The results demonstrate a substantial improvement across"
RIGHT_CONTINUATION = "several independent experiments. Additional observations follow."
TRANSLATED_PREFIX = "此前的研究结论十分明确。"
TRANSLATED_SENTENCE = "研究结果表明，在多个相互独立的实验中均取得了显著改进。"
TRANSLATED_FOLLOWING = "后续还发现了其他现象。"


class DirectTranslator:
    def __init__(self):
        self.requests = []
        self.page_requests = 0
        self.cross_page_requests = 0
        self.fallback_requests = 0

    def do_llm_translate(self, text, rate_limit_params=None):
        return None

    def get_formular_placeholder(self, identifier):
        token = f"{{v{identifier}}}"
        return token, rf"\{{v{identifier}\}}" if identifier == r"\d+" else re.escape(token)

    def get_rich_text_left_placeholder(self, identifier):
        return f"<b{identifier}>"

    def get_rich_text_right_placeholder(self, identifier):
        return f"</b{identifier}>"

    def llm_translate(self, prompt, rate_limit_params=None):
        self.requests.append(prompt)
        if "## Here is the input:" not in prompt:
            self.fallback_requests += 1
            return prompt.rsplit("Now translate the following text:", 1)[1].strip()
        if "## Cross-page continuation" in prompt:
            self.cross_page_requests += 1
        else:
            self.page_requests += 1
        items = json.loads(prompt.rsplit("## Here is the input:", 1)[1].strip())
        for item in items:
            item["output"] = item.pop("input").replace(LEFT_PREFIX, TRANSLATED_PREFIX).replace(
                LEFT_TAIL + " several independent experiments.", TRANSLATED_SENTENCE,
            ).replace(LEFT_TAIL, "研究结果表明，在多项实验中取得了显著改进").replace(
                "several independent experiments.", "多个相互独立的实验。",
            ).replace(" Additional observations follow.", TRANSLATED_FOLLOWING).replace(
                "Additional observations follow.", TRANSLATED_FOLLOWING,
            )
            item.pop("layout_label", None)
        return json.dumps(items)


class DirectApplicationService(RustTranslatorService):
    def __init__(self, on_output=lambda _side: None):
        super().__init__(DirectTranslator())
        self.on_output = on_output
        self.parse_patterns = set()
        self.application_outputs = []

    def regex_compile(self, pattern, flags=0):
        result = super().regex_compile(pattern, flags)
        if pattern in (r"(\{v1\})", r"(\{v3\})"):
            self.parse_patterns.add(result)
        return result

    def regex_finditer(self, pattern_id, text):
        if pattern_id in self.parse_patterns:
            if text.startswith("{v1}") and TRANSLATED_PREFIX in text:
                side = "left"
            elif "{v3}" in text and TRANSLATED_FOLLOWING in text:
                side = "right"
            else:
                side = None
            if side is not None:
                self.application_outputs.append(side)
                self.on_output(side)
        return super().regex_finditer(pattern_id, text)


class Monitor:
    def __init__(self, on_end=lambda: None):
        self.on_end = on_end

    def stage_start(self, name, total):
        return self

    def __enter__(self):
        return self

    def advance(self, amount):
        pass

    def __exit__(self, *args):
        if args[0] is None:
            self.on_end()


def make_direct_document(split_following=True):
    pages = []
    for index in range(2):
        base_style = il.PdfStyle(font_id="regular", font_size=10.0)
        formula = il.PdfParagraphComposition(pdf_formula=il.PdfFormula(pdf_character=[
            il.PdfCharacter(pdf_style=base_style, char_unicode="x"),
        ]))
        if index == 0:
            text = LEFT_PREFIX + LEFT_TAIL
            compositions = [formula, il.PdfParagraphComposition(pdf_character=il.PdfCharacter(
                pdf_style=base_style, char_unicode=text,
            ))]
        else:
            text = RIGHT_CONTINUATION
            rich_style = il.PdfStyle(
                font_id="regular", font_size=10.0,
                graphic_state=il.GraphicState(passthrough_per_char_instruction="0 g"),
            )
            continued, separator, following = text.partition(". ") if split_following else (text, "", "")
            continued = continued + ("." if separator else "")
            compositions = [il.PdfParagraphComposition(pdf_same_style_characters=il.PdfSameStyleCharacters(
                pdf_style=rich_style,
                pdf_character=[il.PdfCharacter(pdf_style=rich_style, char_unicode=continued)],
            ))]
            if separator:
                compositions.append(il.PdfParagraphComposition(pdf_character=il.PdfCharacter(
                    pdf_style=base_style, char_unicode=" " + following,
                )))
            compositions.append(formula)
        paragraph = il.PdfParagraph(
            box=il.Box(x=60, y=100 if index == 0 else 650, x2=540, y2=150 if index == 0 else 700),
            pdf_style=base_style, unicode=text, debug_id=f"p{index}", layout_label="text",
            pdf_paragraph_composition=compositions,
        )
        pages.append(il.Page(
            page_number=index, cropbox=il.Cropbox(box=il.Box(x=0, y=0, x2=600, y2=800)),
            pdf_font=[il.PdfFont(font_id="regular", bold=False, italic=False, monospace=False, serif=False)],
            pdf_paragraph=[paragraph],
        ))
    return _rust_il.transfer_document(il.Document(page=pages, total_pages=2))


def run_bridge(document, tmp_path, service, flag, monitor, cross_page=True):
    font = (Path("C:/Windows/Fonts/arial.ttf") if sys.platform == "win32"
            else Path("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"))
    if not font.exists():
        pytest.skip("test font is unavailable")
    mapper = _rust_il.font_mapper_handle(
        json.dumps({"normal": ["regular"], "script": [], "fallback": [], "base": ["regular"]}),
        json.dumps({"regular": str(font)}),
    )
    config = SimpleNamespace(
        disable_rich_text_translate=False, min_text_length=1,
        pool_max_workers=1, lang_out="Chinese", debug=True, working_dir=str(tmp_path),
        should_translate_page=lambda _page: True,
        get_working_file_path=lambda name: str(tmp_path / name),
    )
    if cross_page is not None:
        config.experimental_cross_page_context = cross_page
    shared = _rust_il.SharedContextCrossSplitPart([])
    first = PriorityServiceDispatcher(service, 1)
    second = PriorityServiceDispatcher(service, 1)
    try:
        return document.translate_paragraphs_llm_only(config, shared, mapper, first, second, service, flag, monitor)
    finally:
        first.close()
        second.close()


def assert_direct_request(service):
    assert service.translator.cross_page_requests == 1
    assert service.translator.fallback_requests == 0
    prompt = service.translator.requests[0]
    assert "second input starts with a complete sentence" in prompt
    items = json.loads(prompt.rsplit("## Here is the input:", 1)[1].strip())
    assert LEFT_TAIL not in items[0]["input"]
    assert LEFT_TAIL + " several independent experiments." in items[1]["input"]


def test_direct_whole_sentence_success(tmp_path):
    document = make_direct_document()
    before = document.parser_artifact_hash()
    service = DirectApplicationService()
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert_direct_request(service)
    assert service.application_outputs == ["left", "left", "right"]
    assert document.parser_artifact_hash() != before
    tracking = json.loads((tmp_path / "translate_tracking.json").read_text(encoding="utf-8"))
    assert len(tracking["cross_page"]) == 1


def test_same_style_following_sentence_has_its_own_request_item(tmp_path):
    document = make_direct_document(split_following=False)
    service = DirectApplicationService()
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert_direct_request(service)
    items = json.loads(service.translator.requests[0].rsplit("## Here is the input:", 1)[1].strip())
    assert len(items) == 3
    assert "Additional observations follow." not in items[1]["input"]
    assert "Additional observations follow." in items[2]["input"]
    assert service.application_outputs == ["left", "left", "right"]
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    left, right = [page["pdf_paragraph"][0]["unicode"] for page in pages]
    assert TRANSLATED_FOLLOWING not in left
    assert TRANSLATED_FOLLOWING in right


@pytest.mark.parametrize("damage", ["missing", "duplicate", "empty"])
def test_invalid_following_item_falls_back_for_both_pages(tmp_path, damage):
    document = make_direct_document(split_following=False)
    service = DirectApplicationService()
    translate = service.translator.llm_translate

    def damaged(prompt, rate_limit_params=None):
        response = translate(prompt, rate_limit_params)
        if "## Cross-page continuation" not in prompt:
            return response
        items = json.loads(response)
        if damage == "missing":
            items.pop()
        elif damage == "duplicate":
            items[2]["id"] = 1
        else:
            items[2]["output"] = ""
        return json.dumps(items)

    service.translator.llm_translate = damaged
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.cross_page_requests == 2
    assert service.translator.fallback_requests == 2
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    left, right = [page["pdf_paragraph"][0]["unicode"] for page in pages]
    assert "Additional observations follow." not in left
    assert "Additional observations follow." in right


def test_normalization_cannot_move_a_suffix_when_sentence_boundary_disappears(tmp_path):
    document = make_direct_document(split_following=False)
    service = DirectApplicationService()
    translate = service.translator.llm_translate

    def damaged(prompt, rate_limit_params=None):
        response = translate(prompt, rate_limit_params)
        if "## Cross-page continuation" not in prompt:
            return response
        items = json.loads(response)
        items[1]["output"] = items[1]["output"].replace(TRANSLATED_SENTENCE, "研究结果" + "。" * 20 + "另有无句界内容")
        return json.dumps(items)

    service.translator.llm_translate = damaged
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.cross_page_requests == 2
    assert service.translator.fallback_requests == 2
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    assert "Additional observations follow." not in pages[0]["pdf_paragraph"][0]["unicode"]
    assert "Additional observations follow." in pages[1]["pdf_paragraph"][0]["unicode"]


def test_direct_whole_sentence_keeps_the_next_sentence_on_its_page(tmp_path, monkeypatch):
    sentence = "经过充分分析，这些独立实验的结论最终到达 the end."
    following = " Another unrelated finding must stay entirely on the next page."
    monkeypatch.setitem(DirectTranslator.llm_translate.__globals__, "TRANSLATED_SENTENCE", sentence)
    monkeypatch.setitem(DirectTranslator.llm_translate.__globals__, "TRANSLATED_FOLLOWING", following)
    document = make_direct_document()
    service = DirectApplicationService()
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert_direct_request(service)
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    left = pages[0]["pdf_paragraph"][0]["unicode"]
    right = pages[1]["pdf_paragraph"][0]["unicode"]
    assert "Another unrelated finding" not in left
    assert following in right


def test_direct_ambiguous_sentence_boundary_falls_back(tmp_path, monkeypatch):
    monkeypatch.setitem(DirectTranslator.llm_translate.__globals__, "TRANSLATED_SENTENCE", "Dr.")
    monkeypatch.setitem(
        DirectTranslator.llm_translate.__globals__, "TRANSLATED_FOLLOWING",
        " Another unrelated finding must stay entirely on the next page.",
    )
    document = make_direct_document()
    service = DirectApplicationService()
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.cross_page_requests >= 1
    assert service.translator.fallback_requests >= 1
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    assert "Another unrelated finding" not in pages[0]["pdf_paragraph"][0]["unicode"]


def test_numeric_multistyle_missing_boundary_fails_without_mutation(tmp_path, monkeypatch):
    monkeypatch.setitem(
        DirectTranslator.llm_translate.__globals__, "LEFT_TAIL",
        "After analysis, 7 of the recovered",
    )
    monkeypatch.setitem(
        DirectTranslator.llm_translate.__globals__, "RIGHT_CONTINUATION",
        "11 samples were checked successfully. Additional observations follow.",
    )
    monkeypatch.setitem(
        DirectTranslator.llm_translate.__globals__, "TRANSLATED_SENTENCE",
        "A later finding appears first and the numeric sentence ends",
    )
    monkeypatch.setitem(
        DirectTranslator.llm_translate.__globals__, "TRANSLATED_FOLLOWING", "",
    )
    document = make_direct_document()
    before = document.parser_artifact_hash()
    service = DirectApplicationService()
    def malformed(prompt, rate_limit_params=None):
        service.translator.requests.append(prompt)
        service.translator.cross_page_requests += 1
        items = json.loads(prompt.rsplit("## Here is the input:", 1)[1].strip())
        for item in items:
            item["output"] = item.pop("input").replace(
                "After analysis, 7 of the recovered 11 samples were checked successfully.",
                "A later finding appears first and the numeric sentence ends",
            ).replace(" Additional observations follow.", "")
            item.pop("layout_label", None)
        return json.dumps(items)

    service.translator.llm_translate = malformed
    with pytest.raises(RuntimeError, match="跨页数值续句"):
        run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.cross_page_requests >= 1
    assert document.parser_artifact_hash() == before
    assert not (tmp_path / "translate_tracking.json").exists()


def test_numeric_sentence_is_present_once_across_original_pages(tmp_path, monkeypatch):
    monkeypatch.setitem(DirectTranslator.llm_translate.__globals__, "LEFT_TAIL", "After analysis, 7 of the recovered")
    monkeypatch.setitem(
        DirectTranslator.llm_translate.__globals__, "RIGHT_CONTINUATION",
        "11 samples were checked successfully. Additional observations follow.",
    )
    document = make_direct_document()
    service = DirectApplicationService()
    def translate(prompt, rate_limit_params=None):
        service.translator.requests.append(prompt)
        service.translator.cross_page_requests += 1
        items = json.loads(prompt.rsplit("## Here is the input:", 1)[1].strip())
        for item in items:
            output = item["input"].replace(LEFT_PREFIX, TRANSLATED_PREFIX).replace(
                "After analysis, 7 of the recovered 11 samples were checked successfully.",
                "分析后，7份来自回收的11份样本，通过了检查。",
            ).replace(" Additional observations follow.", TRANSLATED_FOLLOWING)
            item["output"] = output
            item.pop("input")
            item.pop("layout_label", None)
        return json.dumps(items)

    service.translator.llm_translate = translate
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.cross_page_requests == 1
    assert service.translator.fallback_requests == 0
    prompt = service.translator.requests[0]
    items = json.loads(prompt.rsplit("## Here is the input:", 1)[1].strip())
    assert any("After analysis, 7 of the recovered 11 samples were checked successfully." in item["input"] for item in items)
    pages = json.loads(document.parser_artifact_json())["document"]["page"]
    left = pages[0]["pdf_paragraph"][0]["unicode"]
    right = pages[1]["pdf_paragraph"][0]["unicode"]
    assert left.count("7") == 1 and "7" not in right
    assert right.count("11") == 1 and "11" not in left
    assert TRANSLATED_FOLLOWING in right
    assert TRANSLATED_FOLLOWING not in left


def test_numeric_sentence_with_a_dropped_number_fails_without_mutation(tmp_path, monkeypatch):
    monkeypatch.setitem(DirectTranslator.llm_translate.__globals__, "LEFT_TAIL", "After analysis, 7 of the recovered")
    monkeypatch.setitem(
        DirectTranslator.llm_translate.__globals__, "RIGHT_CONTINUATION",
        "11 samples were checked successfully. Additional observations follow.",
    )
    document = make_direct_document()
    before = document.parser_artifact_hash()
    service = DirectApplicationService()
    with pytest.raises(RuntimeError, match="跨页数值续句"):
        run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.cross_page_requests >= 1
    assert document.parser_artifact_hash() == before
    assert not (tmp_path / "translate_tracking.json").exists()


@pytest.mark.parametrize("split_following", [True, False])
@pytest.mark.parametrize("failure", ["cancel", "second_output"])
def test_direct_whole_sentence_apply_failure_rolls_back(tmp_path, failure, split_following):
    flag = _rust_il.CancelFlag()
    fired = []

    def fail(side):
        if fired:
            return
        if failure == "cancel" and side == "left":
            fired.append(side)
            flag.cancel()
        elif failure == "second_output" and side == "right":
            fired.append(side)
            raise RuntimeError("injected direct second-output failure")

    document = make_direct_document(split_following=split_following)
    before = document.parser_artifact_hash()
    service = DirectApplicationService(fail)
    expected = asyncio.CancelledError if failure == "cancel" else RuntimeError
    with pytest.raises(expected) as error:
        run_bridge(document, tmp_path, service, flag, Monitor())
    assert_direct_request(service)
    assert fired == ["left" if failure == "cancel" else "right"]
    assert service.application_outputs == (["left", "left"] if failure == "cancel" else ["left", "left", "right"])
    if failure == "cancel":
        assert flag.observation_delay() is not None
    else:
        assert str(error.value) == "injected direct second-output failure"
    assert not (tmp_path / "translate_tracking.json").exists()
    assert document.parser_artifact_hash() == before


@pytest.mark.parametrize("split_following", [True, False])
def test_direct_whole_sentence_tracking_publish_failure_rolls_back(tmp_path, split_following):
    document = make_direct_document(split_following=split_following)
    before = document.parser_artifact_hash()
    destination = tmp_path / "translate_tracking.json"
    service = DirectApplicationService()
    blocked = []

    def block_publish():
        assert list(tmp_path.glob("translate_tracking.json.*.tmp"))
        destination.mkdir()
        blocked.append(True)

    with pytest.raises(RuntimeError):
        run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor(block_publish))
    assert_direct_request(service)
    assert blocked == [True]
    assert document.parser_artifact_hash() == before
    assert not list(tmp_path.glob("translate_tracking.json.*.tmp"))


def test_default_off_matches_explicit_false_in_production(tmp_path):
    results = []
    for name, setting in (("omitted", None), ("disabled", False)):
        directory = tmp_path / name
        directory.mkdir()
        document = make_direct_document()
        before = document.parser_artifact_hash()
        service = DirectApplicationService()
        run_bridge(document, directory, service, _rust_il.CancelFlag(), Monitor(), setting)
        assert service.translator.cross_page_requests == 0
        assert service.translator.fallback_requests == 0
        assert service.translator.page_requests > 0
        assert all("## Cross-page continuation" not in prompt for prompt in service.translator.requests)
        after = document.parser_artifact_hash()
        assert after != before
        results.append((after, service.translator.requests))
    assert results[0] == results[1]


def test_cross_page_protocol_versions_match():
    from babeldoc.format.pdf.translation_config import CROSS_PAGE_SENTENCE_PROTOCOL_VERSION

    assert type(CROSS_PAGE_SENTENCE_PROTOCOL_VERSION) is int
    assert CROSS_PAGE_SENTENCE_PROTOCOL_VERSION == 1
    assert type(_rust_il.CROSS_PAGE_SENTENCE_PROTOCOL_VERSION) is int
    assert _rust_il.CROSS_PAGE_SENTENCE_PROTOCOL_VERSION == CROSS_PAGE_SENTENCE_PROTOCOL_VERSION
