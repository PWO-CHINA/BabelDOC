import asyncio
import hashlib
import json
from functools import lru_cache
from pathlib import Path

import pymupdf
import pytest
from babeldoc import _rust_il
from babeldoc.format.pdf.document_il import il_version_1 as il
from babeldoc.rust_translation_service import RustTranslatorService
from test_cross_page_llm_direct_atomicity import DirectTranslator
from test_cross_page_llm_direct_atomicity import Monitor
from test_cross_page_llm_direct_atomicity import run_bridge


@lru_cache(maxsize=1)
def fixture_font():
    return pymupdf.Font(fontfile=str(Path.home() / ".cache/babeldoc/fonts/GoNotoKurrent-Regular.ttf"))


def source_characters(text, style, box):
    font = fixture_font()
    position = box[0]
    baseline = box[3] - 12
    characters = []
    for index, character in enumerate(text):
        advance = font.glyph_advance(ord(character)) * style.font_size
        word = text[index:].split(" ", 1)[0] if index == 0 or text[index - 1] == " " else ""
        word_width = sum(font.glyph_advance(ord(letter)) * style.font_size for letter in word)
        if word and position > box[0] and position + word_width > box[2]:
            position = box[0]
            baseline -= 12
        bounds = il.Box(x=position, y=baseline, x2=position + advance, y2=baseline + 10)
        characters.append(il.PdfCharacter(
            pdf_style=style, char_unicode=character, box=bounds,
            visual_bbox=il.VisualBbox(box=bounds), advance=advance,
            pdf_character_id=font.has_glyph(ord(character)), vertical=False,
        ))
        position += advance
    return characters


def document(regions, *, formula_only=(), figures=None):
    pages = []
    for page_index, entries in enumerate(regions):
        style = il.PdfStyle(font_id="regular", font_size=10.0)
        paragraphs = []
        for index, (text, label, box) in enumerate(entries):
            characters = source_characters(text, style, box)
            compositions = ([il.PdfParagraphComposition(pdf_formula=il.PdfFormula(
                pdf_character=characters, box=il.Box(
                    x=characters[0].box.x, y=characters[0].box.y,
                    x2=characters[-1].box.x2, y2=characters[0].box.y2,
                ), x_offset=0, y_offset=0,
            ))] if (page_index, index) in formula_only else [
                il.PdfParagraphComposition(pdf_character=character) for character in characters
            ])
            paragraphs.append(il.PdfParagraph(
                box=il.Box(x=box[0], y=box[1], x2=box[2], y2=box[3]),
                pdf_style=style, unicode=text, debug_id=f"p{page_index}-{index}",
                layout_label=label, pdf_paragraph_composition=compositions,
            ))
        pages.append(il.Page(
            page_number=page_index, cropbox=il.Cropbox(box=il.Box(x=0, y=0, x2=600, y2=800)),
            pdf_font=[il.PdfFont(font_id="regular", bold=False, italic=False, monospace=False, serif=False)],
            pdf_paragraph=paragraphs,
            pdf_figure=(figures or {}).get(page_index, []),
        ))
    return _rust_il.transfer_document(il.Document(page=pages, total_pages=len(pages)))


class SyntheticTranslator(DirectTranslator):
    def __init__(self, selection, flag=None, cancel_at=None):
        super().__init__()
        self.selection = selection
        self.flag = flag
        self.cancel_at = cancel_at
        self.boundary_requests = []
        self.metadata = []
        self.boundary_responses = []

    def llm_translate(self, prompt, rate_limit_params=None):
        params = rate_limit_params or {}
        if params.get("cross_boundary_request"):
            self.boundary_requests.append((prompt, params))
            if "Metadata: " in prompt:
                metadata = prompt.split("Metadata: ", 1)[1].split(" Formula markers and source characters: ", 1)[0]
                self.metadata.append(json.loads(metadata))
                response = self.selection(self.metadata[-1])
                stage = "selection"
            else:
                items = json.loads(prompt.split(" Input: ", 1)[1])
                response = {"outputs": [{"id": item["id"], "text": item["text"].replace("Alpha", "甲甲").replace("beta", "乙乙")
                            .replace("gamma", "丙丙").replace("delta", "丁丁")
                            .replace("epsilon", "戊戊").replace("zeta", "己己")} for item in items]}
                stage = "translation"
            if self.cancel_at == stage:
                self.flag.cancel()
            encoded = json.dumps(response, ensure_ascii=False)
            self.boundary_responses.append(encoded)
            return encoded
        if "## Here is the input:" in prompt:
            items = json.loads(prompt.rsplit("## Here is the input:", 1)[1].strip())
            return json.dumps([{"id": item["id"], "output": item["input"]} for item in items])
        return prompt.rsplit("Now translate the following text:", 1)[1].strip()


def select(*locations):
    def decision(metadata):
        candidates = metadata["left"] + metadata["right"]
        ranges = []
        for page, paragraph in locations:
            candidate = next(item for item in candidates if (item["page"], item["paragraph"]) == (page, paragraph))
            ranges.append({"page": page, "paragraph": paragraph, "text": candidate["text"]})
        positions = [index for index, candidate in enumerate(candidates) if (candidate["page"], candidate["paragraph"]) in locations]
        skipped = [{"page": candidate["page"], "paragraph": candidate["paragraph"], "reason": "non_body"}
                   for index, candidate in enumerate(candidates)
                   if min(positions) < index < max(positions) and (candidate["page"], candidate["paragraph"]) not in locations]
        return {"status": "selected", "ranges": ranges, "joins": ["space"] * (len(ranges) - 1), "skipped": skipped}
    return decision


LEFT = ("Alpha beta", "text", (50, 600, 250, 650))
RIGHT = ("gamma delta.", "text", (350, 600, 550, 650))


@pytest.mark.parametrize("regions,locations,kind", [
    ([[LEFT, RIGHT]], ((0, 0), (0, 1)), "column"),
    ([[LEFT], [("gamma delta.", "text", (50, 600, 250, 650))]], ((0, 0), (1, 0)), "page"),
    ([[LEFT, ("gamma delta", "text", (50, 500, 250, 550)),
       ("epsilon zeta.", "text", (350, 600, 550, 650))]], ((0, 0), (0, 1), (0, 2)), "column"),
])
def test_selected_boundary_requests_and_writeback(tmp_path, regions, locations, kind):
    native = document(regions)
    before = native.parser_artifact_hash()
    translator = SyntheticTranslator(select(*locations))
    counters = run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert len(translator.boundary_requests) == 2
    assert all(request[1]["request_json_mode"] is True for request in translator.boundary_requests)
    items = json.loads(translator.boundary_requests[1][0].split(" Input: ", 1)[1])
    assert len(items) == 1
    assert items[0]["text"] == " ".join(regions[page][index][0] for page, index in locations)
    assert translator.metadata[0]["kind"] == kind
    assert native.parser_artifact_hash() != before
    paragraphs = json.loads(native.parser_artifact_json())["document"]["page"]
    values = [paragraphs[page]["pdf_paragraph"][index]["unicode"] for page, index in locations]
    assert all(value for value in values)
    expected = "甲甲乙乙丙丙丁丁" + ("戊戊己己" if len(locations) == 3 else "") + "."
    assert "".join(values).replace(" ", "") == expected
    tracking = json.loads((tmp_path / "translate_tracking.json").read_text(encoding="utf-8"))
    trace = json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))["decisions"][0]
    assert trace["kind"] == kind and trace["result"] == "applied"
    candidate_positions = {(item["page"], item["paragraph"]) for side in ("left_candidates", "right_candidates") for item in trace[side]}
    assert set(locations) <= candidate_positions
    assert trace["joins"] == ["space"] * (len(locations) - 1)
    def digest(text):
        return hashlib.sha256(text.encode("utf-8")).hexdigest()
    for position, name in enumerate(("selection", "translation")):
        assert trace[f"{name}_request_sha256"] == digest(translator.boundary_requests[position][0])
        assert trace[f"{name}_response_sha256"] == digest(translator.boundary_responses[position])
    for item, (page, index) in zip(trace["ranges"], locations, strict=True):
        source = regions[page][index][0]
        assert (item["page"], item["paragraph"]) == (page, index)
        assert (item["start_byte"], item["end_byte"]) == (0, len(source.encode("utf-8")))
        assert item["source_text_sha256"] == digest(source)
    assert counters[0] == counters[2] == len(locations)


def test_caption_context_and_none_do_not_merge(tmp_path):
    caption = ("Figure caption", "caption", (50, 400, 250, 450))
    native = document([[LEFT, caption, RIGHT]])
    translator = SyntheticTranslator(lambda _: {"status": "none"})
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert len(translator.boundary_requests) == 1
    assert any(item["label"] == "caption" for item in translator.metadata[0]["context"][0]["paragraphs"])
    assert any(item["text"] == "Figure caption" for item in translator.metadata[0]["left"] + translator.metadata[0]["right"])
    assert all("text" not in item for item in translator.metadata[0]["context"][0]["paragraphs"])
    trace = json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))["decisions"][0]
    assert trace["result"] == "none"
    assert trace["selection_request_sha256"] == hashlib.sha256(translator.boundary_requests[0][0].encode()).hexdigest()
    assert trace["selection_response_sha256"] == hashlib.sha256(translator.boundary_responses[0].encode()).hexdigest()
    assert "translation_request_sha256" not in trace


def test_selected_utf8_byte_offsets(tmp_path):
    left = ("前句。 Alpha beta", "text", LEFT[2])
    right = ("gamma delta 后句。", "text", RIGHT[2])
    native = document([[left, right]])
    translator = SyntheticTranslator(lambda _: {"status": "selected", "ranges": [
        {"page": 0, "paragraph": 0, "text": "Alpha beta"},
        {"page": 0, "paragraph": 1, "text": "gamma delta"},
    ], "joins": ["space"]})
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    trace = json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))["decisions"][0]
    assert [(item["start_byte"], item["end_byte"]) for item in trace["ranges"]] == [(10, 20), (0, len(right[0].encode("utf-8")))]


def test_success_replaces_existing_tracking(tmp_path):
    tracking = tmp_path / "translate_tracking.json"
    tracking.write_bytes(b"existing tracking")
    native = document([[LEFT, RIGHT]])
    before = native.parser_artifact_hash()
    translator = SyntheticTranslator(select((0, 0), (0, 1)))
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert native.parser_artifact_hash() != before
    stored = json.loads(tracking.read_text(encoding="utf-8"))
    assert stored["page"]
    assert json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))["decisions"][0]["result"] == "applied"


@pytest.mark.parametrize("selected", [False, True])
def test_tracking_publish_failure_keeps_document(tmp_path, selected):
    tracking = tmp_path / "translate_tracking.json"
    tracking.mkdir()
    native = document([[LEFT, RIGHT]])
    before = native.parser_artifact_hash()
    translator = SyntheticTranslator(select((0, 0), (0, 1)) if selected else lambda _: {"status": "none"})
    with pytest.raises((OSError, RuntimeError)) as error:
        run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert "Boundary layout" not in str(error.value)
    assert not translator.boundary_requests
    assert tracking.is_dir()


@pytest.mark.parametrize("selected", [False, True])
def test_cancel_at_progress_end_keeps_existing_tracking(tmp_path, selected):
    tracking = tmp_path / "translate_tracking.json"
    tracking.write_bytes(b"existing tracking")
    native = document([[LEFT, RIGHT]])
    before = native.parser_artifact_hash()
    flag = _rust_il.CancelFlag()
    translator = SyntheticTranslator(select((0, 0), (0, 1)) if selected else lambda _: {"status": "none"})
    with pytest.raises(asyncio.CancelledError):
        run_bridge(native, tmp_path, RustTranslatorService(translator), flag, Monitor(flag.cancel))
    assert flag.is_cancelled()
    assert tracking.read_bytes() == b"existing tracking"


def test_default_off_skips_boundary_requests(tmp_path):
    native = document([[LEFT, RIGHT]])
    translator = SyntheticTranslator(select((0, 0), (0, 1)))
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor(), cross_page=None)
    assert translator.boundary_requests == []


@pytest.mark.parametrize("mode", ["invalid_translation", "cancel_selection", "cancel_translation"])
def test_failure_keeps_document_and_existing_tracking(tmp_path, mode):
    native = document([[LEFT, RIGHT]])
    before = native.parser_artifact_hash()
    tracking = tmp_path / "translate_tracking.json"
    tracking.write_bytes(b"existing tracking")
    flag = _rust_il.CancelFlag()
    decision = (lambda _: {"status": "uncertain"}) if mode == "uncertain" else select((0, 0), (0, 1))
    translator = SyntheticTranslator(decision, flag, mode.removeprefix("cancel_") if mode.startswith("cancel_") else None)
    if mode == "invalid_translation":
        original = translator.llm_translate
        def invalid_response(prompt, rate_limit_params=None):
            response = original(prompt, rate_limit_params)
            return "invalid" if (rate_limit_params or {}).get("cross_boundary_request") and " Input: " in prompt else response
        translator.llm_translate = invalid_response
    if mode.startswith("cancel_"):
        with pytest.raises(asyncio.CancelledError):
            run_bridge(native, tmp_path, RustTranslatorService(translator), flag, Monitor())
    else:
        run_bridge(native, tmp_path, RustTranslatorService(translator), flag, Monitor())
        report = json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))
        assert report["unresolved"] and report["applied_groups"] == 0
    values = json.loads(native.parser_artifact_json())["document"]["page"][0]["pdf_paragraph"]
    assert [value["unicode"] for value in values] == [LEFT[0], RIGHT[0]]


def test_uncertain_boundary_uses_ordinary_translation(tmp_path):
    native = document([[LEFT, RIGHT]])
    translator = SyntheticTranslator(lambda _: {"status": "uncertain"})
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert len(translator.boundary_requests) == 1
    tracking = json.loads((tmp_path / "translate_tracking.json").read_text(encoding="utf-8"))
    assert json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))["decisions"][0]["result"] == "uncertain"
    values = json.loads(native.parser_artifact_json())["document"]["page"][0]["pdf_paragraph"]
    assert [value["unicode"] for value in values] == [LEFT[0], RIGHT[0]]


@pytest.mark.parametrize("formula_only", [False, True])
def test_nontranslatable_page_item_does_not_block_body_boundary(tmp_path, formula_only):
    extra = ("x" if formula_only else "12", "footer", (50, 20, 250, 40))
    native = document([[LEFT, extra], [("gamma delta.", "text", LEFT[2])]], formula_only=((0, 1),) if formula_only else ())
    translator = SyntheticTranslator(select((0, 0), (1, 0)))
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert len(translator.boundary_requests) == 2
    metadata = translator.metadata[0]
    assert metadata["left"][0]["selectable"] is True
    assert metadata["left"][1]["selectable"] is False
    pages = json.loads(native.parser_artifact_json())["document"]["page"]
    assert pages[0]["pdf_paragraph"][1]["unicode"] == extra[0]
    assert "甲甲" in pages[0]["pdf_paragraph"][0]["unicode"]


def test_numeric_only_pages_skip_boundary_requests(tmp_path):
    numeric = ("12", "footer", LEFT[2])
    native = document([[numeric], [numeric]])
    translator = SyntheticTranslator(lambda _: {"status": "none"})
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert not translator.boundary_requests
    tracking = json.loads((tmp_path / "translate_tracking.json").read_text(encoding="utf-8"))
    assert not json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))["decisions"]


def test_standalone_body_number_participates_in_complete_sentence(tmp_path):
    number = ("120", "text", LEFT[2])
    tail = ("gamma delta.", "text", (50, 500, 250, 550))
    native = document([[LEFT], [number, tail]])
    translator = SyntheticTranslator(select((0, 0), (1, 0), (1, 1)))
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    items = json.loads(translator.boundary_requests[-1][0].split(" Input: ", 1)[1])
    assert [item["text"] for item in items] == ["Alpha beta 120 gamma delta."]
    pages = json.loads(native.parser_artifact_json())["document"]["page"]
    rendered = "".join(item["unicode"] for page in pages for item in page["pdf_paragraph"])
    assert rendered.replace(" ", "") == "甲甲乙乙120丙丙丁丁."


def test_unexplained_body_gap_is_reported_in_original_page_coordinates(tmp_path):
    native = document([[LEFT, ("Intervening body", "text", (50, 500, 250, 550)), RIGHT]])
    translator = SyntheticTranslator(lambda _: {"status": "selected", "ranges": [
        {"page": 0, "paragraph": 0, "text": LEFT[0]}, {"page": 0, "paragraph": 2, "text": RIGHT[0]},
    ], "joins": ["space"]})
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    report = json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))
    assert report["status"] == "needs_review"
    assert report["unresolved"][0]["locations"][0]["original_page"] == 1
    assert all("Metadata: " in prompt for prompt, _ in translator.boundary_requests)


@pytest.mark.parametrize("original,changed", [("−4.5%", "4.5%"), ("−4.5%", "−4.5"), ("+1.25e-3", "+1.25e-2")])
def test_numeric_damage_rejects_whole_boundary_without_partial_commit(tmp_path, original, changed):
    left = ("Alpha +1.25e-3 and", "text", LEFT[2])
    right = ("beta −4.5%.", "text", RIGHT[2])
    native = document([[left, right]])
    before = native.parser_artifact_hash()
    tracking = tmp_path / "translate_tracking.json"
    tracking.write_bytes(b"existing tracking")
    translator = SyntheticTranslator(select((0, 0), (0, 1)))
    translate = translator.llm_translate

    def damage(prompt, rate_limit_params=None):
        response = translate(prompt, rate_limit_params)
        return response.replace(original, changed) if " Input: " in prompt else response

    translator.llm_translate = damage
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    report = json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))
    assert report["unresolved"] and report["applied_groups"] == 0
    values = json.loads(native.parser_artifact_json())["document"]["page"][0]["pdf_paragraph"]
    assert [value["unicode"] for value in values] == [left[0], right[0]]


def test_column_then_page_share_one_paragraph_without_duplicate_translation(tmp_path):
    middle = ("gamma delta. Middle sentence. Alpha beta", "text", RIGHT[2])
    final = ("epsilon zeta.", "text", LEFT[2])
    native = document([[LEFT, middle], [final]])

    def decision(metadata):
        if metadata["kind"] == "column":
            ranges = [(0, 0, LEFT[0]), (0, 1, "gamma delta.")]
        else:
            ranges = [(0, 1, "Alpha beta"), (1, 0, final[0])]
        return {"status": "selected", "ranges": [
            {"page": page, "paragraph": paragraph, "text": text}
            for page, paragraph, text in ranges
        ], "joins": ["space"]}

    translator = SyntheticTranslator(decision)
    counters = run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert len(translator.boundary_requests) == 3
    items = json.loads(translator.boundary_requests[-1][0].split(" Input: ", 1)[1])
    assert [item["text"] for item in items[:2]] == ["Alpha beta gamma delta.", "Alpha beta epsilon zeta."]
    assert " Middle sentence. " in [item["text"] for item in items]
    pages = json.loads(native.parser_artifact_json())["document"]["page"]
    output = "".join(item["unicode"] for page in pages for item in page["pdf_paragraph"])
    assert output.replace(" ", "") == "甲甲乙乙丙丙丁丁.Middlesentence.甲甲乙乙戊戊己己."
    assert counters[0] == counters[2] == 3
    tracking = json.loads((tmp_path / "translate_tracking.json").read_text(encoding="utf-8"))
    traces = json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))["decisions"]
    assert len(traces) == 2
    assert traces[0]["translation_request_sha256"] == traces[1]["translation_request_sha256"]
