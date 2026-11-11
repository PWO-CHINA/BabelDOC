"""Local, single-attempt DeepSeek proxy for cross-page product validation."""

import argparse
import hashlib
import hmac
import http.client
import json
import os
import socketserver
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler
from http.server import HTTPServer
from pathlib import Path

import crosspage_offline_validation as validation

MODEL = "deepseek-flash"
MAX_OUTPUT_TOKENS = 8192
MAX_REQUEST_BYTES = 524288
INPUT_MARKER = "## Here is the input:"
CONTEXT_MARKER = "## Cross-page continuation"


class RejectedRequestError(Exception):
    pass


def request_items(prompt):
    if prompt.count(INPUT_MARKER) != 1:
        raise RejectedRequestError("unsupported prompt")
    try:
        items = json.loads(prompt.split(INPUT_MARKER, 1)[1].strip())
    except (TypeError, ValueError):
        raise RejectedRequestError("invalid items") from None
    if not isinstance(items, list) or not items:
        raise RejectedRequestError("invalid items")
    if any(
        not isinstance(item, dict)
        or type(item.get("id")) not in (int, str)
        or not isinstance(item.get("input"), str)
        for item in items
    ):
        raise RejectedRequestError("invalid items")
    identifiers = [item["id"] for item in items]
    if len(set(identifiers)) != len(identifiers):
        raise RejectedRequestError("duplicate IDs")
    return items


def synthetic_response(content):
    return {
        "id": "local-validation",
        "object": "chat.completion",
        "created": int(time.time()),
        "model": MODEL,
        "choices": [
            {
                "index": 0,
                "message": {"role": "assistant", "content": content},
                "finish_reason": "stop",
            }
        ],
        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0},
    }


def send_deepseek_once(prompt, api_key):
    body = json.dumps(
        {
            "model": MODEL,
            "messages": [{"role": "user", "content": prompt}],
            "stream": False,
            "max_tokens": MAX_OUTPUT_TOKENS,
            "thinking": {"type": "disabled"},
        },
        ensure_ascii=False,
    ).encode("utf-8")
    if len(body) > MAX_REQUEST_BYTES:
        raise RejectedRequestError("request too large")
    connection = http.client.HTTPSConnection("api.deepseek.com", timeout=120)
    try:
        connection.request(
            "POST",
            "/chat/completions",
            body=body,
            headers={
                "Authorization": f"Bearer {api_key}",
                "Content-Type": "application/json",
            },
        )
        response = connection.getresponse()
        payload = response.read(MAX_REQUEST_BYTES + 1)
        if response.status != 200 or len(payload) > MAX_REQUEST_BYTES:
            raise RejectedRequestError("provider response unavailable")
        try:
            data = json.loads(payload)
            content = data["choices"][0]["message"]["content"]
            if not isinstance(content, str):
                raise ValueError("invalid response")
        except (KeyError, IndexError, TypeError, ValueError):
            raise RejectedRequestError("provider response unavailable") from None
        return data
    finally:
        connection.close()


class SingleRequestProxy:
    def __init__(self, target, marker, api_key, transport=send_deepseek_once):
        if len(target) != 2 or any(
            not isinstance(anchor, str) or len(anchor) < 12 for anchor in target
        ):
            raise ValueError("A confirmed target with two anchors is required")
        self.target = target
        self.marker = marker
        self.api_key = api_key
        self.transport = transport
        self.lock = threading.Lock()
        self.counts = {
            "local_batches": 0,
            "local_fallbacks": 0,
            "target_first": 0,
            "target_retries_rejected": 0,
            "provider_attempts": 0,
        }
        self.usage = None

    def handle(self, body):
        if not isinstance(body, dict):
            raise RejectedRequestError("unsupported request")
        messages = body.get("messages")
        if (
            body.get("model") != MODEL
            or body.get("stream", False) is not False
            or not isinstance(messages, list)
            or len(messages) != 1
            or not isinstance(messages[0], dict)
            or messages[0].get("role") != "user"
            or not isinstance(messages[0].get("content"), str)
        ):
            raise RejectedRequestError("unsupported request")
        prompt = messages[0]["content"]
        if len(prompt.encode("utf-8")) > MAX_REQUEST_BYTES // 2:
            raise RejectedRequestError("request too large")
        if INPUT_MARKER not in prompt:
            marker = "Now translate the following text:"
            if marker not in prompt:
                if not prompt.startswith(
                    "You are a professional,authentic machine translation engine."
                ):
                    raise RejectedRequestError("unsupported prompt")
                marker = "Input:\n\n"
                if marker not in prompt:
                    raise RejectedRequestError("unsupported prompt")
            with self.lock:
                self.counts["local_fallbacks"] += 1
            return synthetic_response(
                validation.synthetic_text(prompt.rsplit(marker, 1)[1].strip(), "rot13")
            )
        items = request_items(prompt)
        target_count = sum(
            validation.normalized(item["input"]).count(self.target[0] + self.target[1])
            for item in items
        )
        if CONTEXT_MARKER in prompt.split(INPUT_MARKER, 1)[0] and target_count:
            if target_count != 1 or prompt.startswith(validation.RETRY_INSTRUCTION):
                with self.lock:
                    self.counts["target_retries_rejected"] += 1
                raise RejectedRequestError("target retry or ambiguity")
            with self.lock:
                self.counts["target_first"] += 1
                try:
                    with self.marker.open("x", encoding="utf-8") as marker:
                        json.dump(
                            {
                                "model": MODEL,
                                "prompt_sha256": hashlib.sha256(
                                    prompt.encode("utf-8")
                                ).hexdigest(),
                                "max_output_tokens": MAX_OUTPUT_TOKENS,
                            },
                            marker,
                        )
                except FileExistsError:
                    self.counts["target_retries_rejected"] += 1
                    raise RejectedRequestError(
                        "provider attempt already consumed"
                    ) from None
                self.counts["provider_attempts"] += 1
            result = self.transport(prompt, self.api_key)
            usage = result.get("usage")
            if isinstance(usage, dict):
                with self.lock:
                    self.usage = {
                        key: usage[key]
                        for key in (
                            "prompt_tokens",
                            "completion_tokens",
                            "total_tokens",
                            "prompt_cache_hit_tokens",
                            "prompt_cache_miss_tokens",
                        )
                        if type(usage.get(key)) is int and usage[key] >= 0
                    }
            return result
        with self.lock:
            self.counts["local_batches"] += 1
        outputs = [
            {
                "id": item["id"],
                "output": validation.synthetic_text(item["input"], "rot13"),
            }
            for item in items
        ]
        return synthetic_response(json.dumps(outputs, ensure_ascii=False))


class LoopbackServer(socketserver.ThreadingMixIn, HTTPServer):
    daemon_threads = True
    allow_reuse_address = False


def make_handler(proxy, local_key):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, format_string, *args):
            return

        def respond(self, status, response):
            payload = json.dumps(response, ensure_ascii=False).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def do_GET(self):
            if self.path != "/_validation/status" or not hmac.compare_digest(
                self.headers.get("Authorization", ""), f"Bearer {local_key}"
            ):
                self.respond(400, {"error": "Local validation request rejected"})
                return
            with proxy.lock:
                response = {
                    "counts": dict(proxy.counts),
                    "usage": proxy.usage,
                    "attempt_marker_exists": proxy.marker.exists(),
                }
            self.respond(200, response)

        def do_POST(self):
            try:
                if self.path != "/v1/chat/completions" or not hmac.compare_digest(
                    self.headers.get("Authorization", ""), f"Bearer {local_key}"
                ):
                    raise RejectedRequestError("unauthorized")
                length = int(self.headers.get("Content-Length", "0"))
                if length <= 0 or length > MAX_REQUEST_BYTES:
                    raise RejectedRequestError("invalid size")
                body = json.loads(self.rfile.read(length))
                response = proxy.handle(body)
                status = 200
            except Exception:
                response = {
                    "error": {
                        "message": "Local validation request rejected",
                        "type": "invalid_request_error",
                    }
                }
                status = 400
            self.respond(status, response)

    return Handler


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--suite-root", type=Path, required=True)
    parser.add_argument("--downloads", type=Path, required=True)
    parser.add_argument("--sample", required=True)
    parser.add_argument("--attempt-marker", type=Path, required=True)
    parser.add_argument("--port", type=int, required=True)
    args = parser.parse_args()
    samples = validation.load_manifest(args.manifest.resolve(strict=True))
    if args.sample not in samples or not samples[args.sample]["target"]:
        raise ValueError("Confirmed target required")
    source, target, _ = validation.selected_input(
        args.suite_root.resolve(strict=True),
        args.downloads.resolve(strict=True),
        samples[args.sample],
    )
    if validation.anchor_locations(source, target) != [[1, 0], [0, 1]]:
        raise ValueError("Target anchors do not uniquely match original pages")
    marker = args.attempt_marker.resolve()
    if (
        not marker.parent.is_dir()
        or marker.exists()
        or any((parent / ".git").exists() for parent in marker.parents)
    ):
        raise ValueError("A new marker outside Git worktrees is required")
    api_key = os.environ.pop("DEEPSEEK_API_KEY", None)
    local_key = os.environ.pop("CROSSPAGE_PROXY_LOCAL_KEY", None)
    if not api_key or not local_key:
        raise ValueError("Missing proxy credentials")
    proxy = SingleRequestProxy(target, marker, api_key)
    server = LoopbackServer(("127.0.0.1", args.port), make_handler(proxy, local_key))
    print(
        json.dumps({"listening": True, "port": server.server_port, "model": MODEL}),
        flush=True,
    )
    try:
        server.serve_forever()
    finally:
        server.server_close()
        print(json.dumps({"counts": proxy.counts, "usage": proxy.usage}), flush=True)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(json.dumps({"error_type": type(error).__name__}), file=sys.stderr)
        raise SystemExit(1) from None
