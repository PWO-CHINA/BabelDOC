"""Offline, end-to-end cross-boundary translation of public synthetic PDFs."""

import hashlib
import json
import os
import re
import threading
from pathlib import Path
from types import SimpleNamespace

import pymupdf
import pytest
from babeldoc import _rust_il as native
from babeldoc.rust_callback_bridge import CallbackRun
from babeldoc.rust_run_context import PriorityServiceDispatcher
from babeldoc.rust_translation_service import RustTranslatorService
from babeldoc.rust_writer_service import WriterFileService

PREFIX = "Earlier results hold."
LEFT = "The model learns from"
RIGHT = "data across boundaries."
SUFFIX = "Later results follow."
SENTENCE = f"{LEFT} {RIGHT}"
TRANSLATIONS = {
    PREFIX: "此前结果成立。",
    SENTENCE: "模型从跨边界数据中学习。",
    SUFFIX: "后续结果继续。",
    LEFT: "模型从中学习",
    RIGHT: "跨边界的数据。",
}


class Stage:
    def __enter__(self):
        return self

    def advance(self, amount):
        pass

    def __exit__(self, *args):
        pass


class Monitor:
    def __init__(self):
        self.stages = []

    def stage_start(self, name, total):
        self.stages.append(name)
        return Stage()


class Layout:
    characters = False

    def begin(self, inputs):
        pass

    def next_page(self):
        return None

    def close(self):
        pass

    def close_process_pool(self):
        pass


class Probe:
    def __init__(self, result):
        self.result = result

    def probe(self):
        return self.result


class LocalWriter(WriterFileService):
    def _save(self, pdf, path, garbage, clean, tag):
        pdf.save(path, garbage=garbage, clean=clean, deflate=True)


class Translator:
    def __init__(self, kind):
        self.kind = kind
        self.selections = []
        self.inputs = []

    def do_llm_translate(self, text, rate_limit_params=None):
        return None

    def get_formular_placeholder(self, identifier):
        token = f"{{v{identifier}}}"
        pattern = rf"\{{v{identifier}\}}" if identifier == r"\d+" else re.escape(token)
        return token, pattern

    def get_rich_text_left_placeholder(self, identifier):
        return f"<b{identifier}>"

    def get_rich_text_right_placeholder(self, identifier):
        return f"</b{identifier}>"

    def llm_translate(self, prompt, rate_limit_params=None):
        params = rate_limit_params or {}
        if not params.get("cross_boundary_request"):
            def ordinary(text):
                for source, translated in TRANSLATIONS.items():
                    text = text.replace(source, translated)
                return text
            if "## Here is the input:" in prompt:
                values = json.loads(prompt.rsplit("## Here is the input:", 1)[1].strip())
                return json.dumps([{"id": value["id"], "output": ordinary(value["input"])} for value in values], ensure_ascii=False)
            return ordinary(prompt.rsplit("Now translate the following text:", 1)[1].strip())
        assert params.get("request_json_mode")
        if "Metadata: " in prompt:
            metadata = json.loads(prompt.split("Metadata: ", 1)[1].split(" Formula markers and source characters: ", 1)[0])
            assert metadata["kind"] == self.kind
            left = next(item for item in metadata["left"] if LEFT in item["text"])
            right = next(item for item in metadata["right"] if RIGHT in item["text"])
            assert PREFIX in left["text"] and SUFFIX in right["text"]
            self.selections.append(metadata)
            response = {
                "status": "selected",
                "ranges": [
                    {"page": left["page"], "paragraph": left["paragraph"], "text": LEFT},
                    {"page": right["page"], "paragraph": right["paragraph"], "text": RIGHT},
                ],
                "joins": ["space"],
            }
        else:
            items = json.loads(prompt.split(" Input: ", 1)[1])
            assert [item["text"].strip() for item in items] == [SENTENCE, PREFIX, SUFFIX]
            self.inputs.append([item["text"] for item in items])
            response = {"outputs": [{"id": item["id"], "text": TRANSLATIONS[item["text"].strip()]} for item in items]}
        return json.dumps(response, ensure_ascii=False)


def make_pdf(path, kind, font):
    with pymupdf.open() as pdf:
        left_page = pdf.new_page(width=600, height=800)
        left_page.insert_font(fontname="arial", fontfile=str(font))
        left_page.insert_text((60, 680), f"{PREFIX} {LEFT}", fontname="arial", fontsize=11)
        if kind == "page":
            right_page = pdf.new_page(width=600, height=800)
            right_page.insert_font(fontname="arial", fontfile=str(font))
            right_position = (60, 100)
        else:
            right_page = left_page
            right_position = (340, 100)
        right_page.insert_text(right_position, f"{RIGHT} {SUFFIX}", fontname="arial", fontsize=11)
        pdf.save(path)


def compact(text):
    return "".join(text.split())


@pytest.mark.parametrize("kind", ["page", "column"])
def test_cross_boundary_production_pdf(tmp_path, kind):
    source_font = Path(os.environ.get("BABELDOC_TEST_SOURCE_FONT", "C:/Windows/Fonts/arial.ttf"))
    target_font = Path(os.environ.get("BABELDOC_TEST_FONT", Path.home() / ".cache/babeldoc/fonts/GoNotoKurrent-Regular.ttf"))
    assert source_font.is_file() and target_font.is_file()
    source = tmp_path / f"{kind}-source.pdf"
    output = tmp_path / f"{kind}-mono.pdf"
    make_pdf(source, kind, source_font)

    family = {"normal": ["test"], "script": [], "fallback": ["test"], "base": ["test"]}
    mapper, styles = native.font_mapper_handle_with_source_styles(
        json.dumps(family), json.dumps({"test": str(target_font)}),
        json.dumps({"test": hashlib.sha3_256(target_font.read_bytes()).hexdigest()}), None,
    )
    translator = Translator(kind)
    service = RustTranslatorService(translator)
    first = PriorityServiceDispatcher(service, 1)
    second = PriorityServiceDispatcher(service, 1)
    shared = native.SharedContextCrossSplitPart([])
    context = native.DocumentRunContext(shared, mapper, first, second)
    config = SimpleNamespace(
        input_file=str(source), shared_context_cross_split_part=SimpleNamespace(),
        table_model=None, debug=False, only_parse_generate_pdf=False,
        skip_scanned_detection=True, skip_translation=False, auto_extract_glossary=False,
        ocr_workaround=False, should_translate_page=lambda _page: True,
        disable_rich_text_translate=False, min_text_length=1, pool_max_workers=1,
        experimental_cross_page_context=True, lang_out="Chinese", working_dir=str(tmp_path),
        get_working_file_path=lambda name: str(tmp_path / name),
    )
    writer = LocalWriter(config, {}, str(source))
    monitor = Monitor()
    pages = [0, 1] if kind == "page" else [0]
    parser_config = {"pages": pages, "enable_graphic_element_process": True,
                     "ocr_workaround": False, "visual_roundtrip_mode": False}
    stages = {"paragraph": {}, "styles": {}, "typesetting": {
        "lang_out": "Chinese", "font_source_styles": styles,
        "progress_monitor_present": True, "debug_information_before_typesetting": False,
    }}
    assets = [{"id": "test", "metadata": {"font_name": "Arial Regular", "ascent": 0.9,
                                             "descent": -0.2, "encoding_length": 2}}]
    writer_config = {"source": str(source), "translated": str(tmp_path / "translated.pdf"),
                     "debug_original": str(tmp_path / "debug.pdf"), "mono": str(output),
                     "dual": None, "skip_clean": True}
    callbacks = {"layout": (Layout(), 1), "probe_translate": (Probe(True), 1),
                 "probe_terms": (Probe(False), 1), "translation": (service, 1), "writer": (writer, 1)}
    try:
        with CallbackRun(callbacks, threading.Event(), native, record_requests=False) as run:
            first.cancel_flag = second.cancel_flag = run.flag
            parsed = native.parse_prepared_pdf_product(
                str(source), json.dumps(parser_config), mapper, shared, run.flag, monitor,
                capture_source_sha256=True,
            )
            parsed.run_part(context, config, run.executors, json.dumps(stages), "{}",
                            json.dumps(writer_config), json.dumps(assets), run.flag, monitor, False, "{}")
    finally:
        writer.close()
        first.close()
        second.close()

    assert len(translator.selections) == len(translator.inputs) == 1
    from babeldoc.format.pdf.high_level import _candidate_result
    from babeldoc.format.pdf.high_level import _publish_candidate_outputs
    from babeldoc.format.pdf.high_level import _verify_outputs
    from babeldoc.format.pdf.translation_config import TranslateResult

    report = json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))
    assert report["candidate_state"] == "written"
    assert report["applied_groups"] == 0
    baseline = TranslateResult(output, None)
    with pymupdf.open(output) as pdf:
        baseline_text = compact("".join(page.get_text() for page in pdf))
    assert TRANSLATIONS[LEFT] in baseline_text
    candidate = _candidate_result(baseline, report)
    _verify_outputs(candidate, baseline)
    _publish_candidate_outputs(SimpleNamespace(raise_if_cancelled=lambda: None), baseline, candidate)
    for stage in ("Parse Page Layout", "Parse Paragraphs", "Typesetting", "Save PDF"):
        assert stage in monitor.stages
    with pymupdf.open(output) as pdf:
        assert len(pdf) == len(pages)
        for index, page in enumerate(pdf):
            page.get_pixmap(matrix=pymupdf.Matrix(2, 2)).save(str(tmp_path / f"{kind}-page-{index + 1}.png"))
        if kind == "page":
            left_text, right_text = (compact(page.get_text()) for page in pdf)
        else:
            page = pdf[0]
            left_text = compact(page.get_text(clip=pymupdf.Rect(0, 0, 300, 800)))
            right_text = compact(page.get_text(clip=pymupdf.Rect(300, 0, 600, 800)))
    assert left_text.startswith(TRANSLATIONS[PREFIX])
    assert right_text.endswith(TRANSLATIONS[SUFFIX])
    left_sentence = left_text.removeprefix(TRANSLATIONS[PREFIX])
    right_sentence = right_text.removesuffix(TRANSLATIONS[SUFFIX])
    assert left_sentence and right_sentence
    assert left_sentence + right_sentence == TRANSLATIONS[SENTENCE]
