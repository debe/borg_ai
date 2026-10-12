# Minimal OpenAI-compatible chat endpoint for the live test: logs every request as JSONL and
# answers with a fixed plan, so a real `opencode run` exercises the plugin for 0 tokens.
# The first turn of a task calls tools first: a read that works and one of a missing file (fails).
# usage: python3 mock_llm.py <port> <log.jsonl> <workdir>
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = sys.argv[2]
WORK = sys.argv[3]
REPLY = "Plan: I check closed.get() on the AtomicBoolean before deliver() in relay.rs."


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.send_response(404)
        self.end_headers()

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")
        with open(LOG, "a") as f:
            f.write(json.dumps({"path": self.path, "body": body}) + "\n")
        msgs = body.get("messages", [])
        calls = None
        if body.get("tools") and not any(m.get("role") == "tool" for m in msgs):
            calls = [{"index": i, "id": f"call_{i}", "type": "function",
                      "function": {"name": "read", "arguments": json.dumps({"filePath": f"{WORK}/{f}"})}}
                     for i, f in enumerate(["relay.rs", "missing.rs"])]
        base = {"id": "mock", "created": int(time.time()), "model": body.get("model", "m")}
        usage = {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        if not body.get("stream"):
            data = json.dumps({**base, "object": "chat.completion", "usage": usage, "choices": [
                {"index": 0, "message": {"role": "assistant", "content": REPLY}, "finish_reason": "stop"}]}).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        chunk = {**base, "object": "chat.completion.chunk"}
        delta = {"role": "assistant", "tool_calls": calls} if calls else {"role": "assistant", "content": REPLY}
        for c in ({**chunk, "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                  {**chunk, "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls" if calls else "stop"}], "usage": usage}):
            self.wfile.write(b"data: " + json.dumps(c).encode() + b"\n\n")
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
