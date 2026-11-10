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


LEFT = "The results demonstrate a substantial improvement across"
RIGHT = "several independent experiments. Additional observations follow."


class Translator:
    def __init__(self):
        self.cross_page_requests = 0
        self.fallback_requests = 0

    def do_llm_translate(self, text, rate_limit_params=None):
        return None

    def get_formular_placeholder(self, identifier):
        return f"{{v{identifier}}}", rf"\{{v{identifier}\}}" if identifier == r"\d+" else re.escape(f"{{v{identifier}}}")

    def get_rich_text_left_placeholder(self, identifier):
        return f"<b{identifier}>"

    def get_rich_text_right_placeholder(self, identifier):
        return f"</b{identifier}>"

    def llm_translate(self, prompt, rate_limit_params=None):
        marker = "## Here is the input:"
        if marker not in prompt:
            self.fallback_requests += 1
            return prompt.rsplit("Now translate the following text:", 1)[1].strip()
        assert "## Cross-page continuation" in prompt
        self.cross_page_requests += 1
        items = json.loads(prompt.rsplit(marker, 1)[1].strip())
        assert len(items) == 2
        return json.dumps([{"id": item["id"], "output": item["input"]} for item in items])


class ApplicationService(RustTranslatorService):
    def __init__(self, on_output=lambda side: None):
        super().__init__(Translator())
        self.on_output = on_output
        self.application_pattern = None
        self.application_outputs = []

    def regex_compile(self, pattern, flags=0):
        result = super().regex_compile(pattern, flags)
        if pattern == r"(\{v1\})":
            self.application_pattern = result
        return result

    def regex_finditer(self, pattern_id, text):
        if self.application_pattern == pattern_id and text in ("{v1}" + LEFT, "{v1}" + RIGHT):
            side = "left" if text.endswith(LEFT) else "right"
            self.application_outputs.append(side)
            self.on_output(side)
        return super().regex_finditer(pattern_id, text)


def make_document(with_formula=False):
    pages = []
    for index, text in enumerate((LEFT, RIGHT)):
        style = il.PdfStyle(font_id="regular", font_size=10.0)
        compositions = [il.PdfParagraphComposition(pdf_character=il.PdfCharacter(pdf_style=style, char_unicode=text))]
        if with_formula:
            compositions.insert(0, il.PdfParagraphComposition(pdf_formula=il.PdfFormula(pdf_character=[il.PdfCharacter(pdf_style=style, char_unicode="x")])))
        paragraph = il.PdfParagraph(
            box=il.Box(x=60, y=100 if index == 0 else 650, x2=540, y2=150 if index == 0 else 700),
            pdf_style=style,
            unicode=text,
            debug_id=f"p{index}",
            layout_label="text",
            pdf_paragraph_composition=compositions,
        )
        pages.append(il.Page(page_number=index, cropbox=il.Cropbox(box=il.Box(x=0, y=0, x2=600, y2=800)), pdf_paragraph=[paragraph]))
    return _rust_il.transfer_document(il.Document(page=pages, total_pages=2))


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


def run(document, tmp_path, service, flag, monitor):
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
        experimental_cross_page_context=True, pool_max_workers=1, lang_out="Chinese",
        debug=True, working_dir=str(tmp_path), should_translate_page=lambda page: True,
        get_working_file_path=lambda name: str(tmp_path / name),
    )
    shared = _rust_il.SharedContextCrossSplitPart([])
    first = PriorityServiceDispatcher(service, 1)
    second = PriorityServiceDispatcher(service, 1)
    try:
        return document.translate_paragraphs_llm_only(config, shared, mapper, first, second, service, flag, monitor)
    finally:
        first.close()
        second.close()


def test_success_control(tmp_path):
    document = make_document()
    before = document.parser_artifact_hash()
    service = RustTranslatorService(Translator())
    run(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert document.parser_artifact_hash() != before
    assert len(json.loads((tmp_path / "translate_tracking.json").read_text(encoding="utf-8"))["cross_page"]) == 1


def test_formula_application_control(tmp_path):
    document = make_document(True)
    before = document.parser_artifact_hash()
    service = ApplicationService()
    run(document, tmp_path, service, _rust_il.CancelFlag(), Monitor())
    assert service.application_outputs == ["left", "left", "right", "right"]
    assert service.translator.cross_page_requests >= 1
    assert service.translator.fallback_requests == 2
    assert len(json.loads((tmp_path / "translate_tracking.json").read_text(encoding="utf-8"))["cross_page"]) == 1
    assert document.parser_artifact_hash() != before


@pytest.mark.parametrize("failure", ["cancel", "second_output"])
def test_application_failure_rolls_back_document(tmp_path, failure):
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
            raise RuntimeError("injected failure in second output")

    document = make_document(True)
    before = document.parser_artifact_hash()
    service = ApplicationService(fail)
    expected = asyncio.CancelledError if failure == "cancel" else RuntimeError
    with pytest.raises(expected) as error:
        run(document, tmp_path, service, flag, Monitor())
    if failure == "second_output":
        assert str(error.value) == "injected failure in second output"
    else:
        assert flag.observation_delay() is not None
    assert fired == ["left" if failure == "cancel" else "right"]
    assert service.application_outputs == (["left", "left"] if failure == "cancel" else ["left", "left", "right"])
    assert service.translator.fallback_requests == 2
    assert service.translator.cross_page_requests >= 1
    assert not (tmp_path / "translate_tracking.json").exists()
    assert document.parser_artifact_hash() == before


def test_tracking_publish_failure_rolls_back_document(tmp_path):
    destination = tmp_path / "translate_tracking.json"
    fired = []

    def block_publish():
        assert list(destination.parent.glob("translate_tracking.json.*.tmp"))
        destination.mkdir()
        fired.append(True)

    document = make_document()
    before = document.parser_artifact_hash()
    service = RustTranslatorService(Translator())
    with pytest.raises(RuntimeError):
        run(document, tmp_path, service, _rust_il.CancelFlag(), Monitor(block_publish))
    assert fired
    assert service.translator.cross_page_requests == 1
    assert not list(destination.parent.glob("translate_tracking.json.*.tmp"))
    assert document.parser_artifact_hash() == before
