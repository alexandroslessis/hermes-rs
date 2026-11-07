"""A mock provider that always fails, so the error card can be tested in the app.

Run it, add a custom endpoint in Settings ▸ Providers pointing at it, make it
default, and send a message in a new session — the turn stops and the chat
shows the error card.

    uv run --no-project python scripts/mock-error-provider.py [--port 8901] [--kind quota]

Kinds:
  quota  429 "user daily request limit reached" — the card hides its Retry
         action (retrying reproduces a quota limit).
  server 500 — the backend retries a few times first (about 8 s), then the
         card offers Retry (a server error clears on its own).
  drop   closes the connection without replying — "Can't reach your model
         server".

Non-streaming requests always answer 200, so the endpoint's Test button and
the session titler succeed.
"""

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ARGS = None

BODIES = {
    "quota": (
        429,
        {"Retry-After": "19919"},
        {
            "error": {
                "message": "Rate limit exceeded: user daily request limit reached (800 of 800). "
                "Retry after 19919 seconds.",
                "metadata": {"retry_after_seconds": 19919},
            }
        },
    ),
    "server": (500, {}, {"error": {"message": "The model server is having problems right now."}}),
}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):  # keep the console quiet
        pass

    def _send(self, status, headers, payload):
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        for name, value in headers.items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        # The model list the Providers page and the session's model picker read.
        self._send(200, {}, {"object": "list", "data": [{"id": "mock-error", "object": "model"}]})

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b"{}"
        try:
            request = json.loads(raw or b"{}")
        except ValueError:
            request = {}

        if not request.get("stream"):
            # The endpoint's Test button and the session auto-titler: a healthy reply.
            self._send(200, {}, {
                "choices": [{
                    "index": 0,
                    "finish_reason": "stop",
                    "message": {"role": "assistant", "content": "Mock reply"},
                }]
            })
            return

        if ARGS.kind == "drop":
            # Close without replying: a transport failure, not a provider verdict.
            self.close_connection = True
            try:
                self.connection.close()
            except OSError:
                pass
            return

        status, headers, payload = BODIES[ARGS.kind]
        self._send(status, headers, payload)


def main():
    global ARGS
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8901)
    parser.add_argument("--kind", choices=["quota", "server", "drop"], default="quota")
    ARGS = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", ARGS.port), Handler)
    print(f"error mock ({ARGS.kind}) on http://127.0.0.1:{ARGS.port}/v1 — Ctrl+C to stop")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
