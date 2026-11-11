import importlib.util
import json
from pathlib import Path

import pytest
import pyzstd

SCRIPT = Path(__file__).resolve().parents[3] / "scripts" / "crosspage_offline_validation.py"
SPEC = importlib.util.spec_from_file_location("crosspage_offline_validation", SCRIPT)
validation = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(validation)


def paragraph(unicode, composition):
    return {"unicode": unicode, "pdf_paragraph_composition": composition}


def capture(path, pages):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(pyzstd.compress(json.dumps({"document": {"page": pages}}).encode()))


def local_sample_spec():
    return {"run": "synthetic-run", "sha256": "0" * 64, "pages": [1, 2], "blocks": None, "target": False}


def test_local_manifest_loads_and_checks_selected_input(tmp_path):
    suite = tmp_path / "suite"
    run = suite / "synthetic-run"
    downloads = tmp_path / "inputs"
    run.mkdir(parents=True)
    downloads.mkdir()
    source = downloads / "synthetic.pdf"
    source.write_bytes(b"synthetic input")
    (run / "summary.json").write_text(json.dumps({"source": source.name}), encoding="utf-8")
    spec = local_sample_spec()
    spec["sha256"] = validation.digest(source)
    manifest = tmp_path / "manifest.json"
    manifest.write_text(json.dumps({"version": 1, "samples": {"sample-a": spec}}), encoding="utf-8")

    samples = validation.load_manifest(manifest)
    selected, target, hashes = validation.selected_input(suite, downloads, samples["sample-a"])
    assert selected == source
    assert target is None
    assert hashes == {"historical_summary_sha256": validation.digest(run / "summary.json")}
    source.write_bytes(b"changed input")
    with pytest.raises(ValueError, match="Input hash does not match"):
        validation.selected_input(suite, downloads, samples["sample-a"])


@pytest.mark.parametrize(
    ("field", "value"),
    [("run", "../outside"), ("sha256", "bad"), ("pages", [True, 2]), ("blocks", [0, -1]), ("target", 1)],
)
def test_local_manifest_rejects_invalid_fields(tmp_path, field, value):
    spec = local_sample_spec()
    spec[field] = value
    manifest = tmp_path / "manifest.json"
    manifest.write_text(json.dumps({"version": 1, "samples": {"sample-a": spec}}), encoding="utf-8")
    with pytest.raises(ValueError, match="Invalid local sample manifest"):
        validation.load_manifest(manifest)


def test_local_manifest_rejects_unsafe_sample_name(tmp_path):
    manifest = tmp_path / "manifest.json"
    manifest.write_text(json.dumps({"version": 1, "samples": {"../outside": local_sample_spec()}}), encoding="utf-8")
    with pytest.raises(ValueError, match="Invalid local sample manifest"):
        validation.load_manifest(manifest)


def test_retained_output_requires_a_new_directory_outside_git(tmp_path):
    output = tmp_path / "private-output"
    with validation.output_directory_context(output) as directory:
        (Path(directory) / "local.txt").write_text("synthetic", encoding="utf-8")
    assert (output / "local.txt").is_file()
    with pytest.raises(FileExistsError):
        validation.output_directory_context(output)
    worktree = tmp_path / "worktree"
    worktree.mkdir()
    (worktree / ".git").write_text("synthetic worktree marker", encoding="utf-8")
    with pytest.raises(ValueError, match="outside Git worktrees"):
        validation.output_directory_context(worktree / "private-output")


@pytest.fixture
def replay_records(tmp_path):
    root = tmp_path / "suite"
    run = root / "synthetic-run"
    tracking_path = run / "work" / "document" / "translate_tracking.json"
    snapshot_path = run / "target-prompt-private.json"
    tracking_path.parent.mkdir(parents=True)
    source = "<b0>left anchor right anchor.</b0>"
    translated = "<b0>synthetic translation.</b0>"
    items = [{"id": 1, "input": source, "layout_label": "body"}, {"id": 2, "input": "Other."}]
    outputs = [{"id": 2, "output": "Different."}, {"id": 1, "output": translated}]
    tracker = {"input": "synthetic prompt", "output": json.dumps(outputs),
               "has_error": False, "fallback_to_translate": False}
    tracking = {"cross_page": [{"paragraph": [
        {"pdf_unicode": "left anchor", "llm_translate_trackers": [dict(tracker)]},
        {"pdf_unicode": "right anchor", "llm_translate_trackers": [dict(tracker)]},
    ]}]}
    snapshot = {"prompt": "synthetic prompt", "items": items}

    def save():
        tracking_path.write_text(json.dumps(tracking), encoding="utf-8")
        snapshot_path.write_text(json.dumps(snapshot), encoding="utf-8")

    save()
    return root, {"run": "synthetic-run"}, ("leftanchor", "rightanchor"), tracking, snapshot, save


def test_historical_replay_requires_exact_dedicated_item_and_layout(replay_records):
    root, spec, target, _, _, _ = replay_records
    replay, metadata = validation.historical_replay(root, spec, target)
    item = {"input": replay["input"], "layout_label": replay["layout_label"]}
    assert validation.replay_response_item(item, replay, True) == ("<b0>synthetic translation.</b0>", True)
    for changed, dedicated in ((item, False), ({**item, "input": item["input"] + " "}, True),
                               ({**item, "layout_label": "other"}, True)):
        output, matched = validation.replay_response_item(changed, replay, dedicated)
        assert not matched
        assert output == validation.synthetic_text(changed["input"], "rot13")
    assert all(len(value) == 64 for value in metadata.values())


@pytest.mark.parametrize("case", [
    "fallback", "error", "missing_tracker", "different_prompt", "different_response",
    "invalid_json", "duplicate_input_id", "duplicate_output_id", "missing_output_id",
    "boolean_id", "placeholder_mismatch", "unchanged_response", "ambiguous_target",
])
def test_historical_replay_rejects_unverifiable_records(replay_records, case):
    root, spec, target, tracking, snapshot, save = replay_records
    paragraphs = tracking["cross_page"][0]["paragraph"]
    trackers = [paragraph["llm_translate_trackers"][0] for paragraph in paragraphs]
    outputs = json.loads(trackers[0]["output"])
    if case == "fallback":
        trackers[0]["fallback_to_translate"] = True
    elif case == "error":
        trackers[0]["has_error"] = True
    elif case == "missing_tracker":
        paragraphs[0]["llm_translate_trackers"] = []
    elif case == "different_prompt":
        trackers[0]["input"] = "another prompt"
    elif case == "different_response":
        trackers[0]["output"] += " "
    elif case == "invalid_json":
        for tracker in trackers:
            tracker["output"] = "not JSON"
    else:
        if case == "duplicate_input_id":
            snapshot["items"][1]["id"] = 1
        elif case == "duplicate_output_id":
            outputs[0]["id"] = 1
        elif case == "missing_output_id":
            outputs[0]["id"] = 3
        elif case == "boolean_id":
            snapshot["items"][0]["id"] = True
        elif case == "placeholder_mismatch":
            outputs[1]["output"] = "<b1>synthetic translation.</b1>"
        elif case == "unchanged_response":
            outputs[1]["output"] = snapshot["items"][0]["input"]
        elif case == "ambiguous_target":
            snapshot["items"][1]["input"] = snapshot["items"][0]["input"]
        for tracker in trackers:
            tracker["output"] = json.dumps(outputs)
    save()
    with pytest.raises(ValueError, match="Historical replay"):
        validation.historical_replay(root, spec, target)


def test_reviewed_block_edge_anchors_are_unique_on_original_pages(tmp_path):
    import pymupdf

    suite = tmp_path / "suite"
    run = suite / "synthetic-run"
    downloads = tmp_path / "inputs"
    run.mkdir(parents=True)
    downloads.mkdir()
    source = downloads / "synthetic.pdf"
    with pymupdf.open() as document:
        document.new_page().insert_text((30, 40), "Earlier evidence was complete and the synthetic study showed that")
        document.new_page().insert_text((30, 40), "A different synthetic outcome appeared in the following measurements.")
        document.save(source)
    (run / "summary.json").write_text(json.dumps({"source": source.name}), encoding="utf-8")
    spec = local_sample_spec()
    spec.update(sha256=validation.digest(source), blocks=[0, 0], target=True, anchor_source="selected_block_edges")
    manifest = tmp_path / "manifest.json"
    manifest.write_text(json.dumps({"version": 1, "samples": {"sample-a": spec}}), encoding="utf-8")

    samples = validation.load_manifest(manifest)
    selected, target, _ = validation.selected_input(suite, downloads, samples["sample-a"])
    assert selected == source
    assert validation.anchor_locations(source, target) == [[1, 0], [0, 1]]

    with pymupdf.open(source) as document:
        duplicate = tmp_path / "duplicate.pdf"
        document[1].insert_text((30, 80), "Earlier evidence was complete and the synthetic study showed that")
        document.save(duplicate)
    spec["sha256"] = validation.digest(duplicate)
    (run / "summary.json").write_text(json.dumps({"source": duplicate.name}), encoding="utf-8")
    with pytest.raises(ValueError, match="anchors are not unique"):
        validation.selected_input(suite, tmp_path, spec)


@pytest.mark.parametrize(
    "changes",
    [{"anchor_source": "unknown"}, {"anchor_source": "selected_block_edges"},
     {"anchor_source": "selected_block_edges", "target": True}],
)
def test_block_edge_manifest_requires_a_reviewed_target_and_two_blocks(tmp_path, changes):
    spec = local_sample_spec()
    spec.update(changes)
    manifest = tmp_path / "manifest.json"
    manifest.write_text(json.dumps({"version": 1, "samples": {"sample-a": spec}}), encoding="utf-8")
    with pytest.raises(ValueError, match="Invalid local sample manifest"):
        validation.load_manifest(manifest)


def test_stage_evidence_distinguishes_text_composition_and_page(tmp_path):
    pages = [
        {"page_number": 0, "pdf_paragraph": [paragraph("first anchor", [
            {"pdf_same_style_characters": {"pdf_character": [
                {"char_unicode": "first"}, {"char_unicode": " anchor"},
            ]}},
        ])]},
        {"page_number": 1, "pdf_paragraph": [paragraph("second anchor", [
            {"pdf_character": {"char_unicode": "different text"}},
        ])]},
    ]
    for stage in ("translation", "typesetting"):
        capture(tmp_path / "document" / "part-0000" / f"{stage}.json.zst", pages)

    evidence = validation.stage_evidence(tmp_path, ("firstanchor", "secondanchor"))
    for result in evidence.values():
        assert result["page_numbers"] == [0, 1]
        assert result["anchor_occurrences_in_paragraph_unicode"] == [[1, 0], [0, 1]]
        assert result["anchor_occurrences_in_composition_characters"] == [[1, 0], [0, 0]]
        assert len(result["sha256"]) == 64
        assert "first anchor" not in json.dumps(result)


def test_stage_evidence_requires_both_unique_stage_files(tmp_path):
    capture(tmp_path / "document" / "part-0000" / "translation.json.zst", [])
    with pytest.raises(ValueError, match="exactly one capture"):
        validation.stage_evidence(tmp_path, None)


def test_failure_classification_does_not_emit_arbitrary_exception_text():
    known = "跨页数值续句无法校验占位符；请人工核对。"
    assert validation.classify_failure(RuntimeError(known)) == "unsafe_numeric_continuation"
    assert validation.classify_failure(RuntimeError(known + " private text")) == "unclassified"
    assert validation.classify_failure(ValueError(known)) == "unclassified"
    assert validation.classify_static_failure_message(known) == "placeholder_validation"
    assert validation.classify_static_failure_message(known + " private text") == "unclassified"


def test_target_fallback_categories_exclude_tracker_text(tmp_path):
    tracking = {"cross_page": [{"paragraph": [
        {"pdf_unicode": "left anchor", "llm_translate_trackers": [{
            "fallback_to_translate": True, "error_message": "Translation result is the same as input, fallback.",
        }]},
        {"pdf_unicode": "right anchor", "llm_translate_trackers": [{
            "fallback_to_translate": True, "error_message": "private response content",
        }]},
    ]}]}
    path = tmp_path / "work" / "document" / "translate_tracking.json"
    path.parent.mkdir(parents=True)
    path.write_text(json.dumps(tracking), encoding="utf-8")
    result = validation.fallback_evidence(tmp_path, ("leftanchor", "rightanchor"))
    assert result["target_slots"] == 2
    assert result["categories"]["same_as_input"] == 1
    assert result["categories"]["other"] == 1
    assert "private response content" not in json.dumps(result)


def test_rot13_control_preserves_tokens_and_is_reversible():
    source = "Alpha 123, 1e-3 and 2E+4, 1r3, 1R-9; <b2>Beta {v7} zeta!</b2> 中文 <style id='2'>More.</style>"
    changed = validation.synthetic_text(source, "rot13")
    assert changed == "Nycun 123, 1e-3 naq 2E+4, 1r3, 1R-9; <b2>Orgn {v7} mrgn!</b2> 中文 <style id='2'>Zber.</style>"
    assert validation.synthetic_text(changed, "rot13") == source
    assert validation.synthetic_text(source, "identity") == source
    with pytest.raises(ValueError, match="Unsupported"):
        validation.synthetic_text(source, "unexpected")


def test_sentence_boundary_skips_decimal_and_abbreviation():
    text = "continued in Fig. 2.5 across samples. Another follows."
    end = validation.sentence_boundary(text)
    assert text[:end].endswith("samples.")
    assert validation.sentence_boundary("no complete ending") is None


def test_device_color_instructions_require_exact_numeric_operands():
    assert validation.device_color_instructions_only("0.5 g 0 1 .2 RG 0 0 0 1 k")
    for instructions in ("", "1 2 g", "1 rg", "1 g q", "1e2 g", "g", "1"):
        assert not validation.device_color_instructions_only(instructions)


def test_context_sentence_rule_replay_distinguishes_guard_stages():
    continuation = "the independent study continues here. Another sentence."
    valid = validation.context_sentence_diagnostic("Earlier work is complete. These results rely on", continuation)
    assert valid == {"parts": "present", "extraction": "available"}
    rejected = validation.context_sentence_diagnostic("Earlier work is complete. These results rely on", "the unfinished next page")
    assert rejected == {"parts": "right_sentence_boundary_missing", "extraction": "not_attempted"}


def test_tracking_placeholder_classification_keeps_contents_private(tmp_path):
    tracking = {"page": [{"paragraph": [
        {"pdf_unicode": "left anchor", "input": "prefix {v1}", "placeholders": [
            {"type": "formula", "placeholder": "{v1}"}], "llm_translate_trackers": []},
        {"pdf_unicode": "right anchor", "input": "<b2>private</b2>", "placeholders": [
            {"type": "rich_text", "left_placeholder": "<b2>", "right_placeholder": "</b2>"}],
            "llm_translate_trackers": [{"fallback_to_translate": True}]},
    ]}]}
    path = tmp_path / "work" / "document" / "translate_tracking.json"
    path.parent.mkdir(parents=True)
    path.write_text(json.dumps(tracking), encoding="utf-8")
    result = validation.target_placeholder_evidence(tmp_path, None, ("leftanchor", "rightanchor"))
    assert [item["matching_tracking_paragraphs"] for item in result] == [1, 1]
    assert [item["first_placeholder_type"] for item in result] == ["formula", "rich_text"]
    assert all(item["input_tokens_match_record_count_and_order"] for item in result)
    assert result[1]["tracking_has_fallback"]
    assert "private" not in json.dumps(result)


def test_sentence_composition_mapping_requires_matching_character_streams():
    def group(text):
        return {"pdf_same_style_characters": {"pdf_character": [{"char_unicode": text}]}}

    left_text = "Earlier work is complete. These results rely on"
    right_text = "the independent study continues here. Another sentence."
    left = paragraph(left_text, [group(left_text)])
    right = paragraph(right_text, [group(right_text)])
    evidence = validation.composition_sentence_evidence(left, right)
    assert evidence["paragraph_and_compositions_match_ignoring_whitespace"] == [True, True]
    assert evidence["left_tail_after_all_formula_compositions"]
    assert evidence["right_sentence_and_end_in_first_rich_composition"]
    assert evidence["right_first_composition_has_content_after_sentence"]
    assert not evidence["right_later_compositions_have_content"]
    assert not evidence["left_sentence_tail_has_numeric"]
    assert not evidence["right_paragraph_has_numeric"]
    assert not evidence["sentence_overlaps_formula_composition"]
    right["unicode"] += "changed"
    assert validation.composition_sentence_evidence(left, right) == {
        "paragraph_and_compositions_match_ignoring_whitespace": [True, False]
    }
