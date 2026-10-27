#!/usr/bin/env python3
"""A local stand-in for the OpenAI Responses API for adapters/codex/hive-smoke.sh: 0 tokens, no network.

usage: hive_mock.py PORT_FILE LOG_DIR BORG

Answers by what the request carries, since the codex TUI sends requests of its own besides the turn's:
a prompt "join the hive" runs `BORG hive join`, a hive wake ("Work arrived") does the task in the
drone's worktree, a tool's result or anything else gets a short message. Every request body and its
answer are saved to LOG_DIR/<n>.json.
"""
import itertools, json, os, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
port_file, logdir, borg = sys.argv[1:4]
os.makedirs(logdir, exist_ok=True)
seq = itertools.count()

def sse(ev):
    return f"event: {ev['type']}\ndata: {json.dumps(ev)}\n\n".encode()

def texts(item):
    c = item.get("content")
    if isinstance(c, str):
        return [c]
    return [x.get("text", "") for x in (c or []) if isinstance(x, dict)]

def answer(req):
    inp = req.get("input", [])
    last = inp[-1] if inp else {}
    if last.get("type") == "function_call_output":
        return [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Done with that step."}]}]
    lastuser = next((" ".join(texts(it)) for it in reversed(inp) if it.get("role") == "user"), "")
    def call(cmd):
        return [{"type": "function_call", "call_id": f"call_{next(seq)}", "name": "exec_command", "arguments": json.dumps({"cmd": cmd})}]
    if "join the hive" in lastuser:
        return call(f"{borg} hive join")
    if "Work arrived" in lastuser:
        return call("cd .worktrees/hive-i-* && touch ok && echo made ok")
    return [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Drone idle: waiting for hive work."}]}]

class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def reply(self, code, body=b"", ctype="application/json"):
        self.send_response(code); self.send_header("content-type", ctype); self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        self.reply(200, json.dumps({"models": [], "data": []}).encode())
    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("content-length", 0)))
        if self.headers.get("content-encoding") == "zstd":
            import subprocess
            raw = subprocess.run(["zstd", "-dc"], input=raw, capture_output=True).stdout
        try: req = json.loads(raw)
        except ValueError: req = {}
        n = next(seq)
        items = answer(req) if self.path.endswith("/responses") else []
        with open(os.path.join(logdir, f"{n:03d}.json"), "w") as f:
            json.dump({"path": self.path, "body": req, "answer": items}, f, indent=1)
        if not self.path.endswith("/responses"):
            return self.reply(404)
        rid = f"resp_{n}"
        out = [sse({"type": "response.created", "response": {"id": rid}})]
        for k, item in enumerate(items):
            item = dict(item, id=f"item_{n}_{k}")
            out.append(sse({"type": "response.output_item.added", "output_index": k, "item": item}))
            out.append(sse({"type": "response.output_item.done", "output_index": k, "item": item}))
        usage = {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 5, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 15}
        out.append(sse({"type": "response.completed", "response": {"id": rid, "usage": usage}}))
        self.reply(200, b"".join(out), "text/event-stream")

s = ThreadingHTTPServer(("127.0.0.1", 0), H)
open(port_file + ".tmp", "w").write(str(s.server_address[1])); os.replace(port_file + ".tmp", port_file)
s.serve_forever()
