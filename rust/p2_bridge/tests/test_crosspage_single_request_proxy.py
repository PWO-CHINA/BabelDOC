import concurrent.futures
import http.client
import importlib.util
import json
import sys
import threading
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "scripts"))
PROXY_SPEC = importlib.util.spec_from_file_location(
    "crosspage_single_request_proxy",
    Path(__file__).resolve().parents[3]
    / "scripts"
    / "crosspage_single_request_proxy.py",
)
proxy_module = importlib.util.module_from_spec(PROXY_SPEC)
PROXY_SPEC.loader.exec_module(proxy_module)


TARGET = ("leftanchorlong", "rightanchorlong")


def request(prompt):
    return {
        "model": proxy_module.MODEL,
        "messages": [{"role": "user", "content": prompt}],
    }


def batch(text, *, dedicated=True):
    prefix = "## Cross-page continuation\n" if dedicated else ""
    return prefix + "## Here is the input:\n\n" + json.dumps([{"id": 0, "input": text}])


def target_prompt():
    return batch("left anchor long right anchor long.")


def test_local_and_non_target_batches_never_use_provider(tmp_path):
    calls = []
    proxy = proxy_module.SingleRequestProxy(
        TARGET,
        tmp_path / "attempt",
        "private-key",
        lambda *arguments: calls.append(arguments),
    )
    for prompt in (
        batch("Unrelated text."),
        batch("left anchor long right anchor long.", dedicated=False),
        "Now translate the following text: Ordinary text.",
        "You are a professional,authentic machine translation engine.\nInput:\n\nPlain text.",
    ):
        response = proxy.handle(request(prompt))
        assert response["choices"][0]["message"]["content"]
    assert not calls
    assert not (tmp_path / "attempt").exists()
    assert proxy.counts["provider_attempts"] == 0


@pytest.mark.parametrize(
    "prompt",
    [
        proxy_module.validation.RETRY_INSTRUCTION + target_prompt(),
        batch(
            "left anchor long right anchor long. left anchor long right anchor long."
        ),
    ],
)
def test_retry_and_ambiguous_target_never_use_provider(tmp_path, prompt):
    calls = []
    proxy = proxy_module.SingleRequestProxy(
        TARGET,
        tmp_path / "attempt",
        "private-key",
        lambda *arguments: calls.append(arguments),
    )
    with pytest.raises(proxy_module.RejectedRequestError):
        proxy.handle(request(prompt))
    assert not calls
    assert not (tmp_path / "attempt").exists()


def test_concurrent_target_requests_consume_only_one_attempt(tmp_path):
    calls = []
    lock = threading.Lock()

    def transport(prompt, key):
        with lock:
            calls.append((prompt, key))
        return proxy_module.synthetic_response("synthetic translated response")

    marker = tmp_path / "attempt"
    proxy = proxy_module.SingleRequestProxy(TARGET, marker, "private-key", transport)

    def attempt():
        try:
            proxy.handle(request(target_prompt()))
            return "success"
        except proxy_module.RejectedRequestError:
            return "rejected"

    with concurrent.futures.ThreadPoolExecutor(max_workers=12) as executor:
        outcomes = list(executor.map(lambda _: attempt(), range(12)))
    assert outcomes.count("success") == 1
    assert len(calls) == proxy.counts["provider_attempts"] == 1
    assert marker.is_file()
    persisted = json.loads(marker.read_text(encoding="utf-8"))
    assert persisted["model"] == proxy_module.MODEL
    assert "private-key" not in json.dumps(persisted)
    restarted = proxy_module.SingleRequestProxy(
        TARGET, marker, "private-key", transport
    )
    with pytest.raises(proxy_module.RejectedRequestError):
        restarted.handle(request(target_prompt()))
    assert len(calls) == 1


def test_failed_provider_attempt_cannot_be_retried(tmp_path):
    calls = []

    def transport(*arguments):
        calls.append(arguments)
        raise OSError("unavailable")

    proxy = proxy_module.SingleRequestProxy(
        TARGET, tmp_path / "attempt", "private-key", transport
    )
    with pytest.raises(OSError):
        proxy.handle(request(target_prompt()))
    with pytest.raises(proxy_module.RejectedRequestError):
        proxy.handle(request(target_prompt()))
    assert len(calls) == proxy.counts["provider_attempts"] == 1


def test_marker_blocks_concurrent_separate_proxy_instances(tmp_path):
    calls = []
    marker = tmp_path / "attempt"

    def transport(*arguments):
        calls.append(arguments)
        return proxy_module.synthetic_response("ok")

    proxies = [
        proxy_module.SingleRequestProxy(TARGET, marker, "private-key", transport)
        for _ in range(2)
    ]

    def attempt(proxy):
        try:
            proxy.handle(request(target_prompt()))
            return True
        except proxy_module.RejectedRequestError:
            return False

    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
        outcomes = list(executor.map(attempt, proxies))
    assert sum(outcomes) == 1
    assert len(calls) == 1


def test_provider_transport_sends_one_fixed_request_without_redirect_or_retry(
    monkeypatch,
):
    requests = []

    class Connection:
        def __init__(self, host, timeout):
            assert host == "api.deepseek.com"

        def request(self, method, path, body, headers):
            requests.append((method, path, json.loads(body), headers))

        def getresponse(self):
            class Response:
                status = 200

                def read(self, size):
                    return json.dumps(
                        proxy_module.synthetic_response("synthetic result")
                    ).encode()

            return Response()

        def close(self):
            pass

    monkeypatch.setattr(http.client, "HTTPSConnection", Connection)
    response = proxy_module.send_deepseek_once("synthetic prompt", "private-key")
    assert response["choices"][0]["message"]["content"] == "synthetic result"
    assert len(requests) == 1
    method, path, body, _ = requests[0]
    assert (method, path) == ("POST", "/chat/completions")
    assert body == {
        "model": "deepseek-flash",
        "messages": [{"role": "user", "content": "synthetic prompt"}],
        "stream": False,
        "max_tokens": 8192,
        "thinking": {"type": "disabled"},
    }


@pytest.mark.parametrize("status", [301, 302, 307, 308, 429, 500])
def test_provider_redirect_or_error_never_triggers_a_second_post(monkeypatch, status):
    requests = []

    class Connection:
        def __init__(self, host, timeout):
            assert host == "api.deepseek.com"

        def request(self, method, path, body, headers):
            requests.append((method, path))

        def getresponse(self):
            class Response:
                def read(self, size):
                    return b"{}"

            response = Response()
            response.status = status
            return response

        def close(self):
            pass

    monkeypatch.setattr(http.client, "HTTPSConnection", Connection)
    with pytest.raises(proxy_module.RejectedRequestError):
        proxy_module.send_deepseek_once("synthetic prompt", "private-key")
    assert requests == [("POST", "/chat/completions")]


def test_loopback_proxy_rejects_auth_and_forwards_target_once(tmp_path):
    calls = []
    proxy = proxy_module.SingleRequestProxy(
        TARGET,
        tmp_path / "attempt",
        "private-key",
        lambda *arguments: (
            calls.append(arguments) or proxy_module.synthetic_response("ok")
        ),
    )
    server = proxy_module.LoopbackServer(
        ("127.0.0.1", 0), proxy_module.make_handler(proxy, "local-key")
    )
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        for key, expected in (
            ("wrong-key", 400),
            ("local-key", 200),
            ("local-key", 400),
        ):
            connection = http.client.HTTPConnection(
                "127.0.0.1", server.server_port, timeout=5
            )
            connection.request(
                "POST",
                "/v1/chat/completions",
                json.dumps(request(target_prompt())),
                {"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
            )
            response = connection.getresponse()
            assert response.status == expected
            response.read()
            connection.close()
        connection = http.client.HTTPConnection(
            "127.0.0.1", server.server_port, timeout=5
        )
        connection.request(
            "GET", "/_validation/status", headers={"Authorization": "Bearer local-key"}
        )
        response = connection.getresponse()
        status = json.loads(response.read())
        assert status["counts"]["provider_attempts"] == 1
        assert status["attempt_marker_exists"]
        connection.close()
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    assert len(calls) == 1


def test_openai_client_retries_cannot_resend_to_provider(tmp_path):
    openai = pytest.importorskip("openai")
    calls = []

    def transport(*arguments):
        calls.append(arguments)
        raise OSError("synthetic provider failure")

    proxy = proxy_module.SingleRequestProxy(
        TARGET, tmp_path / "attempt", "private-key", transport
    )
    server = proxy_module.LoopbackServer(
        ("127.0.0.1", 0), proxy_module.make_handler(proxy, "local-key")
    )
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        client = openai.OpenAI(
            api_key="local-key", base_url=f"http://127.0.0.1:{server.server_port}/v1"
        )
        for _ in range(2):
            with pytest.raises(openai.BadRequestError):
                client.chat.completions.create(**request(target_prompt()))
        client.close()
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    assert len(calls) == proxy.counts["provider_attempts"] == 1
