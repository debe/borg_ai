#!/usr/bin/env python3
"""A local stand-in for the OpenAI Responses API, for adapters/codex/smoke.sh: 0 tokens, no network.

usage: mock.py PORT_FILE SCENARIO.json LOG_DIR

Listens on a free port of 127.0.0.1 and writes it to PORT_FILE. Answers the n-th POST to /responses
with the n-th entry of SCENARIO (a list of lists of output items, streamed as SSE; past the end, the
last entry again) and saves every request body to LOG_DIR/<n>.json, so the smoke test can read what
reached the model.
"""
import itertools
import json
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

port_file, scenario_file, logdir = sys.argv[1:4]
scenario = json.load(open(scenario_file))
os.makedirs(logdir, exist_ok=True)
lock = threading.Lock()
seq = itertools.count()
answered = itertools.count()


def sse(ev):
    return f"event: {ev['type']}\ndata: {json.dumps(ev)}\n\n".encode()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def reply(self, code, body=b"", ctype="application/json"):
        self.send_response(code)
        self.send_header("content-type", ctype)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.reply(200, json.dumps({"models": [], "data": []}).encode())

    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("content-length", 0)))
        if self.headers.get("content-encoding") == "zstd":
            import subprocess
            raw = subprocess.run(["zstd", "-dc"], input=raw, capture_output=True).stdout
        try:
            req = json.loads(raw)
        except ValueError:
            req = {"raw": raw.decode("latin1")}
        with lock:
            n = next(seq)
        with open(os.path.join(logdir, f"{n:03d}.json"), "w") as f:
            json.dump({"path": self.path, "body": req}, f, indent=1)
        if not self.path.endswith("/responses"):
            return self.reply(404)
        with lock:
            i = next(answered)
        items = scenario[min(i, len(scenario) - 1)]
        rid = f"resp_{i}"
        out = [sse({"type": "response.created", "response": {"id": rid}})]
        for k, item in enumerate(items):
            item = dict(item, id=item.get("id", f"item_{i}_{k}"))
            out.append(sse({"type": "response.output_item.added", "output_index": k, "item": item}))
            out.append(sse({"type": "response.output_item.done", "output_index": k, "item": item}))
        usage = {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 5,
                 "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 15}
        out.append(sse({"type": "response.completed", "response": {"id": rid, "usage": usage}}))
        self.reply(200, b"".join(out), "text/event-stream")


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(port_file + ".tmp", "w") as f:
    f.write(str(server.server_address[1]))
os.replace(port_file + ".tmp", port_file)
server.serve_forever()
