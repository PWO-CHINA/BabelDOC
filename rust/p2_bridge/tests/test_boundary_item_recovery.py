import json

from babeldoc import _rust_il
from babeldoc.rust_translation_service import RustTranslatorService
from test_cross_boundary_integration import SyntheticTranslator
from test_cross_boundary_integration import document
from test_cross_boundary_integration import select
from test_cross_page_llm_direct_atomicity import Monitor
from test_cross_page_llm_direct_atomicity import run_bridge


def test_missing_item_is_retried_by_id_without_retranslating_valid_items(tmp_path):
    native = document([
        [("Earlier. Alpha beta", "text", (50, 500, 250, 550))],
        [("gamma delta. Later.", "text", (50, 500, 250, 550))],
    ])
    translator = SyntheticTranslator(lambda _: {"status": "selected", "ranges": [
        {"page": 0, "paragraph": 0, "text": "Alpha beta"},
        {"page": 1, "paragraph": 0, "text": "gamma delta."},
    ], "joins": ["space"]})
    original = translator.llm_translate
    calls = []

    def partial(prompt, rate_limit_params=None):
        response = original(prompt, rate_limit_params)
        if " Input: " not in prompt:
            return response
        inputs = json.loads(prompt.split(" Input: ", 1)[1])
        calls.append(inputs)
        outputs = json.loads(response)["outputs"]
        if len(calls) == 1:
            outputs = [outputs[-1], outputs[0]]
        return json.dumps({"outputs": outputs}, ensure_ascii=False)

    translator.llm_translate = partial
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert len(calls) == 2
    assert len(calls[0]) == 3 and len(calls[1]) == 1
    assert calls[1][0]["id"] == calls[0][1]["id"]
    assert calls[1][0]["text"] == calls[0][1]["text"]


def test_unidentified_array_never_guesses_the_missing_item(tmp_path):
    native = document([
        [("Earlier. Alpha beta", "text", (50, 500, 250, 550))],
        [("gamma delta. Later.", "text", (50, 500, 250, 550))],
    ])
    translator = SyntheticTranslator(lambda _: {"status": "selected", "ranges": [
        {"page": 0, "paragraph": 0, "text": "Alpha beta"},
        {"page": 1, "paragraph": 0, "text": "gamma delta."},
    ], "joins": ["space"]})
    original = translator.llm_translate
    calls = []

    def unidentified(prompt, rate_limit_params=None):
        response = original(prompt, rate_limit_params)
        if " Input: " not in prompt:
            return response
        inputs = json.loads(prompt.split(" Input: ", 1)[1])
        calls.append(inputs)
        return json.dumps({"outputs": [item["text"] for item in inputs]})

    translator.llm_translate = unidentified
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    assert [len(items) for items in calls] == [3, 3, 1, 1, 1]
    report = json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))
    assert report["unresolved"]
    assert not any(decision.get("result") == "applied" for decision in report["decisions"])


def test_failed_group_keeps_baseline_and_later_group_keeps_its_identity(tmp_path):
    left = ("Alpha beta", "text", (50, 500, 250, 550))
    right = ("gamma delta.", "text", (350, 500, 550, 550))
    native = document([[left, right], [left, right]])

    def decision(metadata):
        if metadata["kind"] == "page":
            return {"status": "none"}
        page = metadata["left"][0]["page"]
        return select((page, 0), (page, 1))(metadata)

    translator = SyntheticTranslator(decision)
    original = translator.llm_translate
    requests = []

    def fail_first_group(prompt, rate_limit_params=None):
        response = original(prompt, rate_limit_params)
        if not (rate_limit_params or {}).get("cross_boundary_request"):
            if "## Here is the input:" in prompt:
                outputs = json.loads(response)
                for output in outputs:
                    output["output"] = output["output"].replace("Alpha beta", "基础甲乙").replace("gamma delta.", "基础丙丁。")
                return json.dumps(outputs, ensure_ascii=False)
            return response.replace("Alpha beta", "基础甲乙").replace("gamma delta.", "基础丙丁。")
        if " Input: " not in prompt:
            return response
        inputs = json.loads(prompt.split(" Input: ", 1)[1])
        requests.append(inputs)
        if inputs[0]["id"].startswith("g0-"):
            return '{"outputs":[]}'
        return response

    translator.llm_translate = fail_first_group
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor())
    report = json.loads((tmp_path / "cross_boundary_report.json").read_text(encoding="utf-8"))
    decisions = {decision["group"]: decision for decision in report["decisions"] if "group" in decision}
    assert decisions[0]["result"] == "fallback"
    assert decisions[1]["result"] == "applied"
    assert report["applied_groups"] == 1
    assert report["prepared_groups"][0]["group"] == 1
    pages = json.loads(native.parser_artifact_json())["document"]["page"]
    assert [item["unicode"] for item in pages[0]["pdf_paragraph"]] == ["基础甲乙", "基础丙丁。"]
    assert "甲甲乙乙丙丙丁丁." == "".join(item["unicode"] for item in pages[1]["pdf_paragraph"]).replace(" ", "")
    assert sum(items[0]["id"].startswith("g0-") for items in requests) == 3
    assert sum(items[0]["id"].startswith("g1-") for items in requests) == 1
