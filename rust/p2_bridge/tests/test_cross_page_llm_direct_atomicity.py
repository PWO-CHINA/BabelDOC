import asyncio
import json
import re
import sys
from functools import lru_cache
from pathlib import Path
from types import SimpleNamespace

import pymupdf
import pytest
from babeldoc import _rust_il
from babeldoc.format.pdf.document_il import il_version_1 as il
from babeldoc.rust_run_context import PriorityServiceDispatcher
from babeldoc.rust_translation_service import RustTranslatorService

LEFT_PREFIX = "Earlier findings were clear. "
LEFT_TAIL = "The results demonstrate a substantial improvement across"
RIGHT_CONTINUATION = "several independent experiments. Additional observations follow."
TRANSLATED_PREFIX = "此前的研究结论十分明确。"
TRANSLATED_SENTENCE = "增强实验结果多个独立实验得到改进。"
TRANSLATED_FOLLOWING = "后续还发现了其他现象。"
BASE_PREFIX = "基础前文。"
BASE_LEFT = "基础左页译文"
BASE_RIGHT = "基础右页译文"
BASE_FOLLOWING = "基础后文。"


class DirectTranslator:
    def __init__(self):
        self.requests = []
        self.page_requests = 0
        self.cross_page_requests = 0
        self.fallback_requests = 0
        self.boundary_requests = []
        self.translation_items = []
        self.left_tail = LEFT_TAIL
        self.continuation = RIGHT_CONTINUATION.partition(". ")[0]
        if ". " in RIGHT_CONTINUATION:
            self.continuation += "."

    def do_llm_translate(self, text, rate_limit_params=None):
        return None

    def get_formular_placeholder(self, identifier):
        token = f"{{v{identifier}}}"
        return token, rf"\{{v{identifier}\}}" if identifier == r"\d+" else re.escape(token)

    def get_rich_text_left_placeholder(self, identifier):
        return f"<b{identifier}>"

    def get_rich_text_right_placeholder(self, identifier):
        return f"</b{identifier}>"

    def ordinary(self, text):
        left = BASE_LEFT
        right = BASE_RIGHT + "。"
        match = re.fullmatch(r"The measured value reached (.+) during", self.left_tail)
        if match:
            left += match.group(1)
        elif self.left_tail.startswith("After analysis, 7"):
            left += "7份"
        if self.continuation.startswith("11 "):
            right = BASE_RIGHT + "11次。"
        elif "-0.5" in self.continuation:
            right = BASE_RIGHT + "-0.5。"
        return (text.replace(LEFT_PREFIX.strip(), BASE_PREFIX)
                .replace(self.left_tail, left)
                .replace(self.continuation, right)
                .replace("Additional observations follow.", BASE_FOLLOWING))

    def enhanced(self, text):
        left = "增强实验结果"
        right = "多个独立实验得到改进。"
        match = re.fullmatch(r"The measured value reached (.+) during", self.left_tail)
        if match:
            left = f"增强测量值为{match.group(1)}期间"
        if self.continuation.startswith("11 independent experiments"):
            right = "完成11次独立实验。"
        elif "-0.5" in self.continuation:
            right = "独立组测量值为-0.5。"
        elif self.continuation.startswith("11 samples"):
            left = "分析后7份来自回收的"
            right = "11份样本通过检查。"
        return (text.replace(LEFT_PREFIX.strip(), TRANSLATED_PREFIX)
                .replace(self.left_tail, left)
                .replace(self.continuation, right)
                .replace("Additional observations follow.", TRANSLATED_FOLLOWING))

    def llm_translate(self, prompt, rate_limit_params=None):
        self.requests.append(prompt)
        if (rate_limit_params or {}).get("cross_boundary_request"):
            self.boundary_requests.append((prompt, rate_limit_params))
            if "Metadata: " in prompt:
                metadata = json.loads(prompt.split("Metadata: ", 1)[1].split(" Formula markers and source characters: ", 1)[0])
                left = next((item for item in metadata["left"] if self.left_tail in item["text"]), None)
                right = next((item for item in metadata["right"] if self.continuation in item["text"]), None)
                if left is None or right is None:
                    return json.dumps({"status": "none"})
                preceding = left["text"][:left["text"].find(self.left_tail)]
                start = preceding.rfind("[[BABEL_F_") if preceding.rstrip().endswith("]]") else -1
                left_range = left["text"][start:] if start >= 0 else self.left_tail
                return json.dumps({"status": "selected", "ranges": [
                    {"page": left["page"], "paragraph": left["paragraph"], "text": left_range},
                    {"page": right["page"], "paragraph": right["paragraph"], "text": self.continuation},
                ], "joins": ["space"]}, ensure_ascii=False)
            self.cross_page_requests += 1
            items = json.loads(prompt.split(" Input: ", 1)[1])
            self.translation_items.append(items)
            return json.dumps({"outputs": [{"id": item["id"], "text": self.enhanced(item["text"])}
                                          for item in items]}, ensure_ascii=False)
        if "## Here is the input:" not in prompt:
            self.fallback_requests += 1
            return self.ordinary(prompt.rsplit("Now translate the following text:", 1)[1].strip())
        self.page_requests += 1
        items = json.loads(prompt.rsplit("## Here is the input:", 1)[1].strip())
        return json.dumps([{"id": item["id"], "output": self.ordinary(item["input"])}
                           for item in items], ensure_ascii=False)


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
            if BASE_LEFT in text:
                side = "left"
            elif BASE_RIGHT in text:
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


@lru_cache(maxsize=1)
def fixture_font():
    path = Path.home() / ".cache/babeldoc/fonts/GoNotoKurrent-Regular.ttf"
    return pymupdf.Font(fontfile=str(path))


def source_characters(text, style, bounds):
    font = fixture_font()
    position, baseline = bounds[0], bounds[3] - 12
    characters = []
    for index, character in enumerate(text):
        advance = font.glyph_advance(ord(character)) * style.font_size
        word = text[index:].split(" ", 1)[0] if index == 0 or text[index - 1] == " " else ""
        width = sum(font.glyph_advance(ord(letter)) * style.font_size for letter in word)
        if word and position > bounds[0] and position + width > bounds[2]:
            position, baseline = bounds[0], baseline - 12
        box = il.Box(x=position, y=baseline, x2=position + advance, y2=baseline + 10)
        characters.append(il.PdfCharacter(
            pdf_style=style, char_unicode=character, box=box, visual_bbox=il.VisualBbox(box=box),
            advance=advance, pdf_character_id=font.has_glyph(ord(character)), vertical=False,
        ))
        position += advance
    return characters


def make_direct_document(split_following=True, *, formula_before=False):
    pages = []
    continuation, separator, _following = RIGHT_CONTINUATION.partition(". ")
    continuation += "." if separator else ""
    for index in range(2):
        base_style = il.PdfStyle(font_id="regular", font_size=10.0)
        rich_style = il.PdfStyle(font_id="regular", font_size=10.0,
            graphic_state=il.GraphicState(passthrough_per_char_instruction="0 g"))
        bounds = (60, 100 if index == 0 else 650, 540, 150 if index == 0 else 700)
        text = (("x " + LEFT_PREFIX + LEFT_TAIL if formula_before else LEFT_PREFIX + "x " + LEFT_TAIL)
                if index == 0 else RIGHT_CONTINUATION + " y")
        characters = source_characters(text, base_style, bounds)
        formula_index = (0 if formula_before else len(LEFT_PREFIX)) if index == 0 else len(text) - 1
        formula_character = characters[formula_index]
        formula = il.PdfParagraphComposition(pdf_formula=il.PdfFormula(
            pdf_character=[formula_character], box=formula_character.box,
            x_offset=0, y_offset=0, x_advance=formula_character.advance,
        ))
        if index == 0:
            compositions = [il.PdfParagraphComposition(pdf_character=character)
                            for character in characters[:formula_index]] + [formula] + [
                il.PdfParagraphComposition(pdf_character=character) for character in characters[formula_index + 1:]
            ]
        else:
            rich_end = len(continuation) if split_following and separator else len(RIGHT_CONTINUATION)
            rich_characters = characters[:rich_end]
            for character in rich_characters:
                character.pdf_style = rich_style
            rich_box = il.Box(x=min(c.box.x for c in rich_characters), y=min(c.box.y for c in rich_characters),
                              x2=max(c.box.x2 for c in rich_characters), y2=max(c.box.y2 for c in rich_characters))
            compositions = [il.PdfParagraphComposition(pdf_same_style_characters=il.PdfSameStyleCharacters(
                box=rich_box, pdf_style=rich_style, pdf_character=rich_characters,
            ))] + [il.PdfParagraphComposition(pdf_character=character)
                   for character in characters[rich_end:formula_index]] + [formula]
        paragraph = il.PdfParagraph(
            box=il.Box(x=bounds[0], y=bounds[1], x2=bounds[2], y2=bounds[3]),
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
    font = Path.home() / ".cache/babeldoc/fonts/GoNotoKurrent-Regular.ttf"
    if not font.exists():
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


def pages_of(document):
    return json.loads(document.parser_artifact_json())["document"]["page"]


def page_texts(document):
    return [page["pdf_paragraph"][0]["unicode"] for page in pages_of(document)]


def report_of(tmp_path):
    return json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))


def assert_direct_request(service):
    translator = service.translator
    assert translator.page_requests > 0
    assert translator.cross_page_requests == 1
    assert len(translator.boundary_requests) == 2
    items = translator.translation_items[0]
    assert len(items) == 3
    assert all(set(item) == {"id", "text"} for item in items)
    assert len({item["id"] for item in items}) == 3
    assert translator.left_tail in items[0]["text"]
    assert translator.continuation in items[0]["text"]
    assert "Additional observations follow." not in items[0]["text"]
    assert any("Additional observations follow." in item["text"] for item in items[1:])
    return items


def assert_baseline(document, tmp_path):
    left, right = page_texts(document)
    assert BASE_PREFIX in left and BASE_LEFT in left
    assert BASE_RIGHT in right and BASE_FOLLOWING in right
    assert "增强" not in left + right
    assert TRANSLATED_FOLLOWING not in left + right
    report = report_of(tmp_path)
    assert report["baseline_completed"] is True
    assert report["applied_groups"] == 0
    assert any(decision["result"] == "fallback" for decision in report["decisions"])
    assert report["unresolved"]


def formula_texts(page):
    return ["".join(character["char_unicode"] for character in composition["pdf_formula"]["pdf_character"])
            for paragraph in page["pdf_paragraph"] for composition in paragraph["pdf_paragraph_composition"]
            if composition.get("pdf_formula")]


@pytest.mark.parametrize("split_following", [True, False])
def test_direct_whole_sentence_success_and_ownership(tmp_path, split_following):
    document = make_direct_document(split_following)
    before = document.parser_artifact_hash()
    service = DirectApplicationService()
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    items = assert_direct_request(service)
    assert "[[BABEL_" in items[0]["text"]
    assert report_of(tmp_path)["applied_groups"] == 1
    assert document.parser_artifact_hash() != before
    left, right = page_texts(document)
    assert TRANSLATED_PREFIX in left
    assert TRANSLATED_FOLLOWING not in left and TRANSLATED_FOLLOWING in right
    assert BASE_LEFT not in left and BASE_RIGHT not in right
    pages = pages_of(document)
    assert formula_texts(pages[0]) == ["x"]
    assert formula_texts(pages[1]) == ["y"]
    right_parts = pages[1]["pdf_paragraph"][0]["pdf_paragraph_composition"]
    assert any((part.get("pdf_same_style_unicode_characters") or {}).get("pdf_style", {})
               .get("graphic_state", {}).get("passthrough_per_char_instruction") == "0 g"
               for part in right_parts)
    assert (tmp_path / "translate_tracking.json").is_file()


def test_formula_before_sentence_does_not_hide_its_boundary(tmp_path):
    document = make_direct_document(formula_before=True)
    service = DirectApplicationService()
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    items = assert_direct_request(service)
    assert not re.search(r"\[\[BABEL_\d+_\d+_F\]\]", items[0]["text"])
    assert re.search(r"\[\[BABEL_\d+_\d+_F\]\]", items[1]["text"])
    assert report_of(tmp_path)["applied_groups"] == 1
    pages = pages_of(document)
    assert formula_texts(pages[0]) == ["x"] and formula_texts(pages[1]) == ["y"]
    left, right = page_texts(document)
    assert TRANSLATED_PREFIX in left and TRANSLATED_FOLLOWING not in left
    assert TRANSLATED_FOLLOWING in right


@pytest.mark.parametrize("selection", ["none", "uncertain"])
def test_ambiguous_boundary_keeps_translated_baseline(tmp_path, selection):
    document = make_direct_document()
    service = DirectApplicationService()
    translate = service.translator.llm_translate

    def undecided(prompt, rate_limit_params=None):
        response = translate(prompt, rate_limit_params)
        if (rate_limit_params or {}).get("cross_boundary_request") and "Metadata: " in prompt:
            return json.dumps({"status": selection})
        return response

    service.translator.llm_translate = undecided
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.cross_page_requests == 0
    report = report_of(tmp_path)
    assert report["applied_groups"] == 0
    assert report["decisions"][0]["result"] == selection
    left, right = page_texts(document)
    assert BASE_PREFIX in left and BASE_LEFT in left
    assert BASE_RIGHT in right and BASE_FOLLOWING in right
    assert BASE_FOLLOWING not in left


@pytest.mark.parametrize("damage", ["missing", "duplicate", "empty", "unidentified"])
def test_invalid_following_item_falls_back_for_both_pages(tmp_path, damage):
    document = make_direct_document(split_following=False)
    service = DirectApplicationService()
    translate = service.translator.llm_translate

    def damaged(prompt, rate_limit_params=None):
        response = translate(prompt, rate_limit_params)
        if not (rate_limit_params or {}).get("cross_boundary_request") or " Input: " not in prompt:
            return response
        outputs = json.loads(response)["outputs"]
        if damage == "unidentified":
            return json.dumps({"outputs": [item["text"] for item in outputs]})
        target = next((item for item in outputs if TRANSLATED_FOLLOWING in item["text"]), outputs[-1])
        if damage == "missing":
            outputs.remove(target)
        elif damage == "duplicate":
            outputs.append(dict(target))
        else:
            target["text"] = ""
        return json.dumps({"outputs": outputs}, ensure_ascii=False)

    service.translator.llm_translate = damaged
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.cross_page_requests >= 2
    assert_baseline(document, tmp_path)


def test_damaged_style_markers_fall_back_without_moving_following_sentence(tmp_path):
    document = make_direct_document(split_following=False)
    service = DirectApplicationService()
    translate = service.translator.llm_translate

    def damaged(prompt, rate_limit_params=None):
        response = translate(prompt, rate_limit_params)
        if (rate_limit_params or {}).get("cross_boundary_request") and " Input: " in prompt:
            outputs = json.loads(response)["outputs"]
            for item in outputs:
                item["text"] = re.sub(r"\[\[BABEL_\d+_\d+_R\]\]", "", item["text"])
            return json.dumps({"outputs": outputs}, ensure_ascii=False)
        return response

    service.translator.llm_translate = damaged
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.cross_page_requests >= 2
    assert_baseline(document, tmp_path)
    assert BASE_FOLLOWING not in page_texts(document)[0]


def test_ambiguous_translated_punctuation_cannot_move_following_sentence(tmp_path):
    document = make_direct_document(split_following=False)
    service = DirectApplicationService()
    translate = service.translator.llm_translate

    def ambiguous(prompt, rate_limit_params=None):
        response = translate(prompt, rate_limit_params)
        if (rate_limit_params or {}).get("cross_boundary_request") and " Input: " in prompt:
            outputs = json.loads(response)["outputs"]
            for item in outputs:
                item["text"] = item["text"].replace("增强实验结果", "研究结果。另有无句界内容")
            return json.dumps({"outputs": outputs}, ensure_ascii=False)
        return response

    service.translator.llm_translate = ambiguous
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    items = assert_direct_request(service)
    assert "Additional observations follow." not in items[0]["text"]
    assert report_of(tmp_path)["applied_groups"] == 1
    left, right = page_texts(document)
    assert TRANSLATED_FOLLOWING not in left and TRANSLATED_FOLLOWING in right


@pytest.mark.parametrize("split_following", [True, False])
def test_numeric_sentence_is_present_once_across_original_pages(tmp_path, monkeypatch, split_following):
    monkeypatch.setitem(globals(), "LEFT_TAIL", "After analysis, 7 of the recovered")
    monkeypatch.setitem(globals(), "RIGHT_CONTINUATION", "11 samples were checked successfully. Additional observations follow.")
    document = make_direct_document(split_following)
    service = DirectApplicationService()
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert_direct_request(service)
    assert report_of(tmp_path)["applied_groups"] == 1
    left, right = page_texts(document)
    assert (left + right).count("7") == 1 and (left + right).count("11") == 1
    assert TRANSLATED_FOLLOWING not in left and TRANSLATED_FOLLOWING in right


NUMERIC_DAMAGE = [
    ("-7", "7"), ("−7", "7"), ("–7", "7"), ("7", "–7"),
    ("≪7", "7"), ("7%", "7"), ("7٪", "7"), ("7", "7٪"),
    ("1e-3", "1e3"), ("1e-3", "1e-2"), ("1,000", "1.000"),
    ("-0.5", "0.5"), ("7 × 3", "7 ÷ 3"), ("7 +/- 3", "7 - 3"),
    ("7 $", "7"), ("7", "-7"), ("7-", "7"),
]


@pytest.mark.parametrize("damage_numeric", [False, True])
@pytest.mark.parametrize(("quantity", "translated_quantity"), NUMERIC_DAMAGE)
def test_numeric_expression_validation_is_atomic(tmp_path, monkeypatch, quantity, translated_quantity, damage_numeric):
    monkeypatch.setitem(globals(), "LEFT_TAIL", f"The measured value reached {quantity} during")
    monkeypatch.setitem(globals(), "RIGHT_CONTINUATION", "11 independent experiments. Additional observations follow.")
    document = make_direct_document()
    service = DirectApplicationService()
    translate = service.translator.llm_translate

    def damage_response(prompt, rate_limit_params=None):
        response = translate(prompt, rate_limit_params)
        if damage_numeric and (rate_limit_params or {}).get("cross_boundary_request") and " Input: " in prompt:
            outputs = json.loads(response)["outputs"]
            for item in outputs:
                item["text"] = item["text"].replace(quantity, translated_quantity)
            return json.dumps({"outputs": outputs}, ensure_ascii=False)
        return response

    service.translator.llm_translate = damage_response
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    if damage_numeric:
        assert service.translator.cross_page_requests >= 2
        assert_baseline(document, tmp_path)
        assert "numeric expression or mathematical symbol changed" in str(report_of(tmp_path)["unresolved"])
        assert quantity in page_texts(document)[0]
    else:
        assert service.translator.cross_page_requests == 1
        report = report_of(tmp_path)
        if quantity == "≪7" and not fixture_font().has_glyph(ord("≪")):
            item = next(item for item in service.translator.translation_items[0] if quantity in item["text"])
            batch = report["decisions"][0]["translation_batches"][0]
            assert item["id"] in batch["accepted_ids"]
            assert "TypesettingFailed" in report["unresolved"][0]["reason"]
            assert_baseline(document, tmp_path)
            return
        assert report["applied_groups"] == 1, json.dumps(report, ensure_ascii=False)
        left, right = page_texts(document)
        assert quantity in left + right and "11" in left + right
        assert TRANSLATED_FOLLOWING not in left and TRANSLATED_FOLLOWING in right


def test_missing_numeric_item_retains_multistyle_baseline(tmp_path, monkeypatch):
    monkeypatch.setitem(globals(), "LEFT_TAIL", "After analysis, 7 of the recovered")
    monkeypatch.setitem(globals(), "RIGHT_CONTINUATION", "11 samples were checked successfully. Additional observations follow.")
    document = make_direct_document(split_following=False)
    service = DirectApplicationService()
    translate = service.translator.llm_translate

    def damaged(prompt, rate_limit_params=None):
        response = translate(prompt, rate_limit_params)
        if (rate_limit_params or {}).get("cross_boundary_request") and " Input: " in prompt:
            outputs = json.loads(response)["outputs"]
            for item in outputs:
                item["text"] = item["text"].replace("11", "")
            return json.dumps({"outputs": outputs}, ensure_ascii=False)
        return response

    service.translator.llm_translate = damaged
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.cross_page_requests >= 2
    assert_baseline(document, tmp_path)
    assert "numeric expression or mathematical symbol changed" in str(report_of(tmp_path)["unresolved"])
    left, right = page_texts(document)
    assert "7" in left and "11" in right


def test_dotted_step_range_cannot_cut_a_decimal(tmp_path, monkeypatch):
    monkeypatch.setitem(globals(), "LEFT_TAIL", "The checklist includes the following step 1.2")
    monkeypatch.setitem(globals(), "RIGHT_CONTINUATION", "applies to sample collection. Additional observations follow.")
    document = make_direct_document()
    service = DirectApplicationService()
    translate = service.translator.llm_translate

    def truncated(prompt, rate_limit_params=None):
        response = translate(prompt, rate_limit_params)
        if (rate_limit_params or {}).get("cross_boundary_request") and "Metadata: " in prompt:
            decision = json.loads(response)
            decision["ranges"][0]["text"] = decision["ranges"][0]["text"][:-1]
            return json.dumps(decision, ensure_ascii=False)
        return response

    service.translator.llm_translate = truncated
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.cross_page_requests == 0
    assert len(service.translator.boundary_requests) == 2
    report = report_of(tmp_path)
    assert report["applied_groups"] == 0 and report["unresolved"]
    left, right = page_texts(document)
    assert BASE_LEFT in left and BASE_RIGHT in right and BASE_FOLLOWING in right


def test_numeric_expression_retry_commits_only_corrected_response(tmp_path, monkeypatch):
    monkeypatch.setitem(globals(), "LEFT_TAIL", "The measured value reached -7 during")
    monkeypatch.setitem(globals(), "RIGHT_CONTINUATION", "11 independent experiments. Additional observations follow.")
    document = make_direct_document()
    service = DirectApplicationService()
    translate = service.translator.llm_translate

    def damaged_first(prompt, rate_limit_params=None):
        response = translate(prompt, rate_limit_params)
        if (rate_limit_params or {}).get("cross_boundary_request") and " Input: " in prompt and service.translator.cross_page_requests == 1:
            outputs = json.loads(response)["outputs"]
            for item in outputs:
                item["text"] = item["text"].replace("-7", "7")
            return json.dumps({"outputs": outputs}, ensure_ascii=False)
        return response

    service.translator.llm_translate = damaged_first
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.translator.cross_page_requests == 2
    first, retry = service.translator.translation_items
    assert len(first) == 3 and len(retry) == 1
    bad = next(item for item in first if "-7" in item["text"])
    assert retry == [bad]
    assert report_of(tmp_path)["applied_groups"] == 1
    left, right = page_texts(document)
    assert "-7" in left + right and "11" in left + right
    assert TRANSLATED_FOLLOWING not in left and TRANSLATED_FOLLOWING in right


@pytest.mark.parametrize("following", [False, True])
@pytest.mark.parametrize("damaged", [False, True])
def test_signed_decimal_at_sentence_end_is_validated(tmp_path, monkeypatch, following, damaged):
    monkeypatch.setitem(globals(), "LEFT_TAIL", "The final measurement was reported for")
    monkeypatch.setitem(globals(), "RIGHT_CONTINUATION", "the independent group as -0.5." +
                        (" Additional observations follow." if following else ""))
    document = make_direct_document()
    service = DirectApplicationService()
    translate = service.translator.llm_translate

    def response(prompt, rate_limit_params=None):
        result = translate(prompt, rate_limit_params)
        if damaged and (rate_limit_params or {}).get("cross_boundary_request") and " Input: " in prompt:
            outputs = json.loads(result)["outputs"]
            for item in outputs:
                item["text"] = item["text"].replace("-0.5", "0.5")
            return json.dumps({"outputs": outputs}, ensure_ascii=False)
        return result

    service.translator.llm_translate = response
    run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    left, right = page_texts(document)
    assert "-0.5" in left + right
    report = report_of(tmp_path)
    if damaged:
        assert report["applied_groups"] == 0 and report["unresolved"]
        assert "numeric expression or mathematical symbol changed" in str(report["unresolved"])
        assert BASE_LEFT in left and BASE_RIGHT in right
        assert service.translator.cross_page_requests >= 2
    else:
        assert report["applied_groups"] == 1
        assert service.translator.cross_page_requests == 1
    if following:
        expected = BASE_FOLLOWING if damaged else TRANSLATED_FOLLOWING
        assert expected not in left and expected in right


@pytest.mark.parametrize("failure", ["cancel", "second_output"])
@pytest.mark.parametrize("split_following", [True, False])
def test_baseline_apply_failure_rolls_back(tmp_path, failure, split_following):
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

    document = make_direct_document(split_following)
    before = document.parser_artifact_hash()
    tracking = tmp_path / "translate_tracking.json"
    tracking.write_bytes(b"existing tracking")
    service = DirectApplicationService(fail)
    expected = asyncio.CancelledError if failure == "cancel" else RuntimeError
    with pytest.raises(expected) as error:
        run_bridge(document, tmp_path, service, flag, Monitor())
    assert fired == ["left" if failure == "cancel" else "right"]
    if failure == "cancel":
        assert flag.observation_delay() is not None
    else:
        assert str(error.value) == "injected direct second-output failure"
    assert document.parser_artifact_hash() == before
    assert tracking.read_bytes() == b"existing tracking"
    assert not service.translator.boundary_requests


@pytest.mark.parametrize("split_following", [True, False])
def test_baseline_tracking_publish_failure_rolls_back(tmp_path, split_following):
    document = make_direct_document(split_following)
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
    assert blocked == [True]
    assert document.parser_artifact_hash() == before
    assert not list(tmp_path.glob("translate_tracking.json.*.tmp"))
    assert not service.translator.boundary_requests


@pytest.mark.parametrize("split_following", [True, False])
def test_baseline_cancellation_at_commit_preserves_tracking(tmp_path, split_following):
    document = make_direct_document(split_following)
    before = document.parser_artifact_hash()
    tracking = tmp_path / "translate_tracking.json"
    tracking.write_bytes(b"existing tracking")
    flag = _rust_il.CancelFlag()
    service = DirectApplicationService()
    with pytest.raises(asyncio.CancelledError):
        run_bridge(document, tmp_path, service, flag, Monitor(flag.cancel))
    assert flag.observation_delay() is not None
    assert document.parser_artifact_hash() == before
    assert tracking.read_bytes() == b"existing tracking"
    assert not list(tmp_path.glob("translate_tracking.json.*.tmp"))
    assert not service.translator.boundary_requests


@pytest.mark.parametrize("stage", ["selection", "translation"])
def test_enhancement_cancellation_preserves_completed_baseline(tmp_path, stage):
    baseline_dir = tmp_path / "baseline"
    baseline_dir.mkdir()
    baseline = make_direct_document()
    run_bridge(baseline, baseline_dir, DirectApplicationService(), _rust_il.CancelFlag(), Monitor(), False)
    expected_hash = baseline.parser_artifact_hash()
    expected_tracking = (baseline_dir / "translate_tracking.json").read_bytes()
    document = make_direct_document()
    flag = _rust_il.CancelFlag()
    service = DirectApplicationService()
    translate = service.translator.llm_translate

    def cancel(prompt, rate_limit_params=None):
        response = translate(prompt, rate_limit_params)
        is_boundary = (rate_limit_params or {}).get("cross_boundary_request")
        if is_boundary and ((stage == "selection") == ("Metadata: " in prompt)):
            flag.cancel()
        return response

    service.translator.llm_translate = cancel
    with pytest.raises(asyncio.CancelledError):
        run_bridge(document, tmp_path, service, flag, Monitor())
    assert flag.observation_delay() is not None
    assert document.parser_artifact_hash() == expected_hash
    assert (tmp_path / "translate_tracking.json").read_bytes() == expected_tracking
    assert not list(tmp_path.glob("translate_tracking.json.*.tmp"))


def test_baseline_provider_failure_is_terminal(tmp_path):
    document = make_direct_document()
    before = document.parser_artifact_hash()
    tracking = tmp_path / "translate_tracking.json"
    tracking.write_bytes(b"existing tracking")
    service = DirectApplicationService()
    calls = []

    def fail(prompt, rate_limit_params=None):
        calls.append((prompt, rate_limit_params))
        raise ConnectionError("offline synthetic baseline failure")

    service.translator.llm_translate = fail
    with pytest.raises(ConnectionError, match="offline synthetic baseline failure"):
        run_bridge(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert calls and not any((params or {}).get("cross_boundary_request") for _, params in calls)
    assert document.parser_artifact_hash() == before
    assert tracking.read_bytes() == b"existing tracking"


def test_default_off_matches_explicit_false_in_production(tmp_path):
    results = []
    for name, setting in (("omitted", None), ("disabled", False)):
        directory = tmp_path / name
        directory.mkdir()
        document = make_direct_document()
        before = document.parser_artifact_hash()
        service = DirectApplicationService()
        run_bridge(document, directory, service, _rust_il.CancelFlag(), Monitor(), setting)
        assert not service.translator.boundary_requests
        assert service.translator.page_requests > 0
        assert document.parser_artifact_hash() != before
        assert BASE_LEFT in page_texts(document)[0] and BASE_RIGHT in page_texts(document)[1]
        results.append((document.parser_artifact_hash(), service.translator.requests))
    assert results[0] == results[1]


def test_cross_page_protocol_versions_match():
    from babeldoc.format.pdf.translation_config import (
        CROSS_PAGE_SENTENCE_PROTOCOL_VERSION,
    )

    assert type(CROSS_PAGE_SENTENCE_PROTOCOL_VERSION) is int
    assert CROSS_PAGE_SENTENCE_PROTOCOL_VERSION == 1
    assert type(_rust_il.CROSS_PAGE_SENTENCE_PROTOCOL_VERSION) is int
    assert _rust_il.CROSS_PAGE_SENTENCE_PROTOCOL_VERSION == CROSS_PAGE_SENTENCE_PROTOCOL_VERSION
