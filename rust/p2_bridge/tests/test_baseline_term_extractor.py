import asyncio
import concurrent.futures
import json
from pathlib import Path
from types import SimpleNamespace

import pytest
from babeldoc import _rust_il as native
from babeldoc.format.pdf.document_il import il_version_1 as il
from babeldoc.glossary import Glossary
from babeldoc.glossary import GlossaryEntry
from babeldoc.rust_run_context import PriorityServiceDispatcher
from babeldoc.rust_translation_service import RustTranslatorService


class Monitor:
    def stage_start(self, name, total):
        return self

    def __enter__(self):
        return self

    def advance(self, amount):
        pass

    def __exit__(self, *args):
        pass


class Service:
    def __init__(self, result, flag=None):
        self.result = result
        self.flag = flag

    def llm_translate(self, prompt, rate_limit_params=None):
        if self.flag is not None:
            self.flag.cancel()
        if isinstance(self.result, BaseException):
            raise self.result
        return self.result

    def usage_counters(self):
        return (0, 0, 0, 0)

    def record_term_extraction_usage(self, *args):
        pass


def document():
    text = "A cell develops within a complex biological environment."
    style = il.PdfStyle(font_id="regular", font_size=10.0)
    paragraph = il.PdfParagraph(
        unicode=text,
        debug_id="p0",
        layout_label="text",
        pdf_style=style,
        pdf_paragraph_composition=[il.PdfParagraphComposition(
            pdf_character=il.PdfCharacter(pdf_style=style, char_unicode=text)
        )],
    )
    return native.transfer_document(il.Document(page=[il.Page(page_number=0, pdf_paragraph=[paragraph])], total_pages=1))


def extract(result, flag=None, cancel_during=False, service=None, dispatcher_factory=None):
    flag = flag or native.CancelFlag()
    service = service or Service(result, flag if cancel_during else None)
    dispatcher = (dispatcher_factory or PriorityServiceDispatcher)(service, 1)
    shared = native.SharedContextCrossSplitPart([Glossary("user", [GlossaryEntry("cell", "细胞")])])
    config = SimpleNamespace(lang_out="Chinese", term_pool_max_workers=1, debug=False, working_dir=None)
    try:
        document().extract_terms(config, shared, dispatcher, service, flag, Monitor())
        return shared
    finally:
        dispatcher.close()


@pytest.mark.parametrize("result", [RuntimeError("offline injected extraction failure"), "invalid JSON", object()])
def test_extraction_failures_leave_user_glossary_available(result):
    shared = extract(result)
    assert [glossary.name for glossary in shared.get_glossaries_for_translation(True)] == ["user"]


@pytest.mark.parametrize("result", [asyncio.CancelledError(), concurrent.futures.CancelledError(), KeyboardInterrupt()])
def test_non_exception_cancellation_propagates(result):
    with pytest.raises(type(result)):
        extract(result)


def test_flag_cancellation_propagates_before_extraction():
    flag = native.CancelFlag()
    flag.cancel()
    with pytest.raises(asyncio.CancelledError):
        extract("[]", flag)


def test_flag_cancellation_during_extraction_propagates():
    with pytest.raises(asyncio.CancelledError):
        extract("[]", cancel_during=True)


class SubmissionFailure(PriorityServiceDispatcher):
    def submit(self, *args):
        raise RuntimeError("offline injected dispatcher failure")


def test_extraction_dispatch_failure_leaves_user_glossary_available():
    shared = extract("[]", dispatcher_factory=SubmissionFailure)
    assert [glossary.name for glossary in shared.get_glossaries_for_translation(True)] == ["user"]


@pytest.mark.parametrize("error", [asyncio.CancelledError(), concurrent.futures.CancelledError(), KeyboardInterrupt()])
def test_cancellation_during_dispatch_propagates(error):
    class CancelSubmission(PriorityServiceDispatcher):
        def submit(self, *args):
            raise error

    with pytest.raises(type(error)):
        extract("[]", dispatcher_factory=CancelSubmission)


class UsageFailure(Service):
    def __init__(self, result, method, error):
        super().__init__(result)
        self.method = method
        self.error = error

    def usage_counters(self):
        if self.method == "usage_counters":
            raise self.error
        return super().usage_counters()

    def record_term_extraction_usage(self, *args):
        if self.method == "record_term_extraction_usage":
            raise self.error


@pytest.mark.parametrize("method", ["usage_counters", "record_term_extraction_usage"])
def test_usage_failure_does_not_block_extraction(method):
    shared = extract(None, service=UsageFailure("[]", method, RuntimeError("offline usage failure")))
    assert [glossary.name for glossary in shared.get_glossaries_for_translation(True)] == ["user"]


@pytest.mark.parametrize("method", ["usage_counters", "record_term_extraction_usage"])
@pytest.mark.parametrize("error", [asyncio.CancelledError(), concurrent.futures.CancelledError()])
def test_usage_cancellation_propagates(method, error):
    with pytest.raises(type(error)):
        extract(None, service=UsageFailure("[]", method, error))


@pytest.mark.parametrize("error", [RuntimeError("offline monitor failure"), asyncio.CancelledError(), concurrent.futures.CancelledError()])
def test_stage_start_failure_is_optional_except_cancellation(error):
    class FailingMonitor(Monitor):
        def stage_start(self, name, total):
            raise error

    service = Service("[]")
    dispatcher = PriorityServiceDispatcher(service, 1)
    shared = native.SharedContextCrossSplitPart([Glossary("user", [GlossaryEntry("cell", "细胞")])])
    config = SimpleNamespace(lang_out="Chinese", term_pool_max_workers=1, debug=False, working_dir=None)
    try:
        if isinstance(error, (asyncio.CancelledError, concurrent.futures.CancelledError)):
            with pytest.raises(type(error)):
                document().extract_terms(config, shared, dispatcher, service, native.CancelFlag(), FailingMonitor())
        else:
            document().extract_terms(config, shared, dispatcher, service, native.CancelFlag(), FailingMonitor())
            assert [glossary.name for glossary in shared.get_glossaries_for_translation(True)] == ["user"]
    finally:
        dispatcher.close()


def test_valid_response_remains_available_without_replacing_user_terms():
    response = json.dumps([{"src": "cell", "tgt": "细胞"}], ensure_ascii=False)
    shared = extract(response)
    assert [(entry.source, entry.target) for entry in shared.auto_extracted_glossary.entries] == [("cell", "细胞")]
    assert shared.user_glossaries[0].name == "user"
    assert [glossary.name for glossary in shared.get_glossaries_for_translation(True)] == ["user"]


@pytest.mark.parametrize("term_failure,separate_pages", [(False, False), (True, False), (False, True)])
def test_explicit_auto_enabled_extraction_and_baseline_translation(tmp_path, term_failure, separate_pages):
    from test_cross_page_llm_direct_atomicity import DirectTranslator

    evidence = Path(__file__).resolve().parents[3] / ".baseline-priority-20261112/glossary-evidence.json"
    entry = (
        json.loads(evidence.read_text(encoding="utf-8"))["extraction_origins"][0]["entry"]
        if evidence.is_file() else {"src": "Development", "tgt": "《发育》"}
    )
    reference = "[21] A. Scientist, Development 2016, 143, 2862. The reference continues."
    body = "A driver behind technological development is collaboration between researchers."
    paragraphs = [(reference, "reference"), (body, "text")]
    page_paragraphs = [[paragraphs[0]], [paragraphs[1]]] if separate_pages else [paragraphs]
    styles = [il.PdfStyle(font_id="regular", font_size=10.0) for _ in page_paragraphs]
    doc = native.transfer_document(il.Document(page=[il.Page(
        page_number=page_index,
        cropbox=il.Cropbox(box=il.Box(x=0, y=0, x2=600, y2=800)),
        pdf_font=[il.PdfFont(font_id="regular", bold=False, italic=False, monospace=False, serif=False)],
        pdf_paragraph=[il.PdfParagraph(
            unicode=text, debug_id=f"p{page_index}-{index}", layout_label=label, pdf_style=styles[page_index],
            box=il.Box(x=60, y=600 - index * 80, x2=540, y2=650 - index * 80),
            pdf_paragraph_composition=[il.PdfParagraphComposition(
                pdf_character=il.PdfCharacter(pdf_style=styles[page_index], char_unicode=text)
            )],
        ) for index, (text, label) in enumerate(page)],
    ) for page_index, page in enumerate(page_paragraphs)], total_pages=len(page_paragraphs)))

    class Translator(DirectTranslator):
        def llm_translate(self, prompt, rate_limit_params=None):
            self.requests.append(prompt)
            if "Input Text:" in prompt:
                if term_failure:
                    raise RuntimeError("offline injected extraction failure")
                terms = [entry] if "Development 2016" in prompt else []
                return json.dumps(terms, ensure_ascii=False)
            if "Now translate the following text:" in prompt:
                text = prompt.rsplit("Now translate the following text:", 1)[1].strip()
                return "参考文献内容。" if "Development" in text else "技术发展依靠合作。"
            items = json.loads(prompt.rsplit("## Here is the input:", 1)[1].strip())
            return json.dumps([
                {"id": item["id"], "output": "参考文献内容。" if "Development" in item["input"] else "技术发展依靠合作。"}
                for item in items
            ], ensure_ascii=False)

    translator = Translator()
    service = RustTranslatorService(translator)
    font = Path("C:/Windows/Fonts/arial.ttf")
    if not font.is_file():
        pytest.skip("test font is unavailable")
    mapper = native.font_mapper_handle(
        json.dumps({"normal": ["regular"], "script": [], "fallback": [], "base": ["regular"]}),
        json.dumps({"regular": str(font)}),
    )
    shared = native.SharedContextCrossSplitPart([
        Glossary("user", [GlossaryEntry("collaboration", "合作")])
    ])
    config = SimpleNamespace(
        lang_out="Chinese", term_pool_max_workers=1, pool_max_workers=1,
        disable_rich_text_translate=False, min_text_length=1,
        auto_extract_glossary=True, experimental_cross_page_context=False,
        debug=False, working_dir=None, should_translate_page=lambda _page: True,
        get_working_file_path=lambda name: str(tmp_path / name),
    )
    flag = native.CancelFlag()
    first = PriorityServiceDispatcher(service, 1)
    second = PriorityServiceDispatcher(service, 1)
    try:
        doc.extract_terms(config, shared, first, service, flag, Monitor())
        if term_failure:
            assert shared.auto_extracted_glossary is None
        else:
            assert [(entry.source, entry.target) for entry in shared.auto_extracted_glossary.entries] == [
                (entry["src"], entry["tgt"])
            ]
        doc.translate_paragraphs_llm_only(config, shared, mapper, first, second, service, flag, Monitor())
    finally:
        first.close()
        second.close()
    requests = [prompt for prompt in translator.requests if "Input Text:" not in prompt]
    assert requests
    body_requests = [prompt for prompt in requests if "technological development" in prompt]
    assert body_requests and all("《发育》" not in prompt for prompt in body_requests)
    if separate_pages:
        assert any("optional suggestions" in prompt and "Development → 《发育》" in prompt for prompt in requests)
    else:
        assert all("《发育》" not in prompt for prompt in requests)
    assert any("| collaboration | 合作 |" in prompt for prompt in requests)
    pages = json.loads(doc.parser_artifact_json())["document"]["page"]
    assert [paragraph["unicode"] for page in pages for paragraph in page["pdf_paragraph"]] == [
        "参考文献内容。", "技术发展依靠合作。"
    ]
