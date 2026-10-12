"""A free, local OpenAI-compatible chat endpoint for the omp smoke run (no tokens spent).

Request 1 answers with a bash tool call (`cat src/relay.rs`); every later request with plain text.
Usage: mock_model.py <port> <request-log>
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

LOG = sys.argv[2]
calls = 0


def chunk(delta, finish=None):
    choice = {"index": 0, "delta": delta, "finish_reason": finish}
    body = {"id": "c", "object": "chat.completion.chunk", "created": 0, "model": "mock-1", "choices": [choice]}
    return "data: " + json.dumps(body) + "\n\n"


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):  # model listing
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({"object": "list", "data": [{"id": "mock-1", "object": "model"}]}).encode())

    def do_POST(self):
        global calls
        body = self.rfile.read(int(self.headers.get("content-length", 0)))
        with open(LOG, "a") as f:
            f.write(self.path + " " + body.decode() + "\n")
        if "chat/completions" not in self.path:
            self.send_response(404)
            self.end_headers()
            return
        calls += 1
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        if calls == 1:
            call = {"index": 0, "id": "call_1", "type": "function",
                    "function": {"name": "bash", "arguments": json.dumps({"command": "cat src/relay.rs"})}}
            out = chunk({"role": "assistant", "tool_calls": [call]}) + chunk({}, "tool_calls")
        else:
            text = f"Done {calls}: the relay delivery now uses a lock-free queue head."
            out = chunk({"role": "assistant", "content": text}) + chunk({}, "stop")
        self.wfile.write((out + "data: [DONE]\n\n").encode())


HTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
