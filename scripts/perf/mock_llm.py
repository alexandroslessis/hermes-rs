"""A mock OpenAI-compatible model server for performance runs: streams a markdown reply at a fixed
rate, so measurements don't depend on a real model or the GPU.

    python mock_llm.py [--port 47900] [--tps 80] [--tokens 1500]
"""

import argparse
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ARGS = None
LOCK = threading.Lock()
REQUESTS = 0

PARAGRAPH = (
    "Performance work starts with a measurement, not a guess. The backend streams tokens as they "
    "arrive, the page batches them into frames, and markdown is parsed incrementally. "
)
CODE = (
    "```rust\nfn fib(n: u64) -> u64 {\n    match n {\n        0 | 1 => n,\n"
    "        _ => fib(n - 1) + fib(n - 2),\n    }\n}\n```\n\n"
)


def reply_text(tokens):
    """About `tokens` four-character tokens of mixed markdown."""
    parts, size, section = [], 0, 1
    while size < tokens * 4:
        block = (
            f"## Section {section}\n\n{PARAGRAPH * 3}\n\n"
            f"- first point about item {section}\n- second point, with `inline code`\n"
            f"- third point and a [link](https://example.com/{section})\n\n{CODE}"
            f"| col a | col b |\n|---|---|\n| {section} | {section * 2} |\n\n"
        )
        parts.append(block)
        size += len(block)
        section += 1
    text = "".join(parts)
    return text[: tokens * 4]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def _json(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path.rstrip("/").endswith("/models"):
            return self._json(200, {"object": "list", "data": [
                {"id": "mock", "object": "model", "owned_by": "mock", "context_length": 131072}]})
        self._json(404, {"error": "not found"})

    def do_POST(self):
        global REQUESTS
        length = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(length) or b"{}")
        with LOCK:
            REQUESTS += 1
        created = int(time.time())
        if not body.get("stream"):
            return self._json(200, {
                "id": "mock", "object": "chat.completion", "created": created, "model": "mock",
                "choices": [{"index": 0, "finish_reason": "stop",
                             "message": {"role": "assistant", "content": "Mock session title"}}],
                "usage": {"prompt_tokens": 50, "completion_tokens": 4, "total_tokens": 54}})

        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()

        def send(obj):
            data = f"data: {obj if isinstance(obj, str) else json.dumps(obj)}\n\n".encode()
            self.wfile.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
            self.wfile.flush()

        # "please write file NAME": one write_file call (to exercise checkpoints), then a short
        # reply once its result is in.
        messages = body.get("messages") or []
        last = messages[-1] if messages else {}
        asked = last.get("content") if last.get("role") == "user" else None
        asked = asked if isinstance(asked, str) else " ".join(
            p.get("text", "") for p in (asked or []) if isinstance(p, dict))
        if asked and "please write file" in asked:
            name = asked.split("please write file", 1)[1].split()[0] if asked.split("please write file", 1)[1].split() else "note.txt"
            call = {"id": "mock", "object": "chat.completion.chunk", "created": created, "model": "mock",
                    "choices": [{"index": 0, "finish_reason": None, "delta": {"tool_calls": [{
                        "index": 0, "id": f"call_{REQUESTS}", "type": "function",
                        "function": {"name": "write_file", "arguments": json.dumps(
                            {"path": name, "content": f"written by the mock, request {REQUESTS}\n"})}}]}}]}
            send(call)
            send({"id": "mock", "object": "chat.completion.chunk", "created": created, "model": "mock",
                  "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
                  "usage": {"prompt_tokens": 3000, "completion_tokens": 20, "total_tokens": 3020}})
            send("[DONE]")
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()
            return
        # "please run slow": a terminal call that takes ten minutes; "please run big": one that
        # prints two million lines (to exercise Stop and huge outputs).
        if asked and ("please run slow" in asked or "please run big" in asked):
            command = ("Start-Sleep -Seconds 600" if "slow" in asked
                       else "1..2000000 | ForEach-Object { \"line $_ of a long listing\" }")
            send({"id": "mock", "object": "chat.completion.chunk", "created": created, "model": "mock",
                  "choices": [{"index": 0, "finish_reason": None, "delta": {"tool_calls": [{
                      "index": 0, "id": f"call_{REQUESTS}", "type": "function",
                      "function": {"name": "terminal", "arguments": json.dumps(
                          {"command": command, "timeout": 900})}}]}}]})
            send({"id": "mock", "object": "chat.completion.chunk", "created": created, "model": "mock",
                  "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]})
            send("[DONE]")
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()
            return
        # "please hang": a few words, then the stream stalls (an unresponsive provider).
        if asked and "please hang" in asked:
            send({"id": "mock", "object": "chat.completion.chunk", "created": created, "model": "mock",
                  "choices": [{"index": 0, "delta": {"content": "Thinking about"}, "finish_reason": None}]})
            time.sleep(3600)
            return
        text = "Wrote it." if last.get("role") == "tool" else reply_text(ARGS.tokens)
        interval = 1.0 / ARGS.tps
        start = time.perf_counter()
        for i in range(0, len(text), 4):
            send({"id": "mock", "object": "chat.completion.chunk", "created": created, "model": "mock",
                  "choices": [{"index": 0, "delta": {"content": text[i:i + 4]}, "finish_reason": None}]})
            # Keep the average rate exact (sleep to the schedule, not per chunk).
            delay = start + (i // 4 + 1) * interval - time.perf_counter()
            if delay > 0:
                time.sleep(delay)
        send({"id": "mock", "object": "chat.completion.chunk", "created": created, "model": "mock",
              "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
              "usage": {"prompt_tokens": 3000, "completion_tokens": len(text) // 4,
                        "total_tokens": 3000 + len(text) // 4}})
        send("[DONE]")
        self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=47900)
    parser.add_argument("--tps", type=float, default=80)
    parser.add_argument("--tokens", type=int, default=1500)
    ARGS = parser.parse_args()
    print(f"mock model on http://127.0.0.1:{ARGS.port}/v1 at {ARGS.tps} tok/s, {ARGS.tokens} tokens", flush=True)
    ThreadingHTTPServer(("127.0.0.1", ARGS.port), Handler).serve_forever()
