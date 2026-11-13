import json

import pytest
from babeldoc import _rust_il
from babeldoc.rust_translation_service import RustTranslatorService
from test_cross_boundary_integration import SyntheticTranslator
from test_cross_boundary_integration import document
from test_cross_page_llm_direct_atomicity import Monitor
from test_cross_page_llm_direct_atomicity import run_bridge


@pytest.mark.parametrize("invalid_id", [-1, True, 0.5, "1", 2, None])
def test_invalid_baseline_id_retranslates_without_guessing(tmp_path, invalid_id):
    native = document([[
        ("Alpha beta.", "text", (50, 600, 250, 650)),
        ("Gamma delta.", "text", (50, 500, 250, 550)),
    ]])
    translator = SyntheticTranslator(lambda _: {"status": "none"})
    fallback_inputs = []

    def translate(prompt, rate_limit_params=None):
        del rate_limit_params
        if "## Here is the input:" in prompt:
            return json.dumps([{"id": 0, "output": "错误批次甲"}, {"id": invalid_id, "output": "错误批次乙"}])
        text = prompt.rsplit("Now translate the following text:", 1)[1].strip()
        fallback_inputs.append(text)
        return {"Alpha beta.": "甲乙基础译文。", "Gamma delta.": "丙丁基础译文。"}[text]

    translator.llm_translate = translate
    run_bridge(native, tmp_path, RustTranslatorService(translator), _rust_il.CancelFlag(), Monitor(), False)
    assert sorted(fallback_inputs) == ["Alpha beta.", "Gamma delta."]
    paragraphs = json.loads(native.parser_artifact_json())["document"]["page"][0]["pdf_paragraph"]
    assert [paragraph["unicode"] for paragraph in paragraphs] == ["甲乙基础译文。", "丙丁基础译文。"]
