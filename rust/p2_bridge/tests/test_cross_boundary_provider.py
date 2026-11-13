from types import SimpleNamespace

import pytest
from babeldoc.translator.translator import OpenAITranslator


def translator_with_response(finish_reason="stop", content="synthetic result"):
    calls = []

    def create(**kwargs):
        calls.append(kwargs)
        return SimpleNamespace(
            choices=[
                SimpleNamespace(
                    finish_reason=finish_reason,
                    message=SimpleNamespace(content=content),
                )
            ],
            usage=None,
        )

    translator = object.__new__(OpenAITranslator)
    translator.send_temperature = True
    translator.options = {"temperature": 0}
    translator.enable_json_mode_if_requested = True
    translator.send_dashscope_header = False
    translator.model = "configured-model"
    translator.extra_body = {"thinking": {"type": "disabled"}}
    translator.client = SimpleNamespace(
        chat=SimpleNamespace(completions=SimpleNamespace(create=create))
    )
    translator.update_token_count = lambda _response: None
    return translator, calls


def test_cross_boundary_request_uses_provider_limits():
    translator, calls = translator_with_response()
    assert (
        translator.do_llm_translate(
            "synthetic prompt",
            {"cross_boundary_request": True, "request_json_mode": True},
        )
        == "synthetic result"
    )
    assert "max_tokens" not in calls[0]
    assert calls[0]["model"] == "configured-model"
    assert calls[0]["extra_body"] == {"thinking": {"type": "disabled"}}
    assert calls[0]["response_format"] == {"type": "json_object"}


@pytest.mark.parametrize(
    "finish_reason,content",
    [
        ("length", "partial"),
        ("content_filter", "partial"),
        ("stop", ""),
        ("stop", " \n\t"),
    ],
)
def test_cross_boundary_rejects_incomplete_provider_response(finish_reason, content):
    translator, _ = translator_with_response(finish_reason, content)
    with pytest.raises(RuntimeError, match="response was incomplete"):
        translator.do_llm_translate(
            "synthetic prompt", {"cross_boundary_request": True}
        )


def test_ordinary_request_retains_existing_completion_limit():
    translator, calls = translator_with_response()
    assert translator.do_llm_translate("synthetic prompt") == "synthetic result"
    assert calls[0]["max_tokens"] == 2048


@pytest.mark.parametrize("finish_reason", ["stop", "length"])
def test_executor_boundary_uses_same_complete_response_contract(finish_reason):
    from babeldoc.tools.executor.translator import ExecutorTranslator

    calls = []
    translator = object.__new__(ExecutorTranslator)
    translator.model = "configured-model"
    translator.base_url = "https://offline.invalid"
    translator.api_key = "test-only"
    payload = {
        "choices": [{"finish_reason": finish_reason, "message": {"content": "{}"}}]
    }

    def post(*_args, **kwargs):
        calls.append(kwargs["json"])
        return SimpleNamespace(status_code=200, json=lambda: payload)

    translator._client = SimpleNamespace(post=post)
    translator._update_token_count = lambda _: None
    if finish_reason == "stop":
        assert (
            translator.do_llm_translate("input", {"cross_boundary_request": True})
            == "{}"
        )
    else:
        with pytest.raises(RuntimeError, match="incomplete"):
            translator.do_llm_translate("input", {"cross_boundary_request": True})
    assert "max_tokens" not in calls[0]
