"""Haiku classification of prefiltered correction prompts: engineering | steering | unclear.

Input : candidates.parquet (id, content, prev_assistant, priority), highest priority first.
Output: labels.jsonl (one line per item), calls.jsonl (one line per model call with
        cost and running total).  Resumable.  Hard cap on total spend.

usage: classify.py <max_items> [cap_usd]
"""
import json, subprocess, sys, threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
import pandas as pd

E = Path.home() / ".local/share/borg/eval/swe-chat/engineering"
CWD = E / "haiku_cwd"
CAP = float(sys.argv[2]) if len(sys.argv) > 2 else 5.0
SAFETY = 0.15  # stop before the cap with headroom for in-flight calls
BATCH = 20
WORKERS = 12

SYSTEM = """You label user messages sent to an AI coding agent. Each item is a user message that pushes back on or corrects the agent, plus the tail of the agent's previous message.

label:
- engineering: the user corrects the TECHNICAL SUBSTANCE of the code or plan: design, architecture, concurrency/locking, data structures, algorithms, performance, API/interface design, data model/schema, logic correctness (a concrete bug in reasoning or code), security, error handling, or a better way to write a specific function. The correction must carry a reusable engineering idea.
- steering: process or communication: be shorter, explain, continue, run/skip tests, commit/push, formatting of answers, stop doing X, wrong file/task, just do what I said, UI copy/text wording, styling/colors, "it still doesn't work" without a technical idea, environment or tool usage.
- unclear: cannot tell.

For engineering items also give:
- category: one of concurrency, data_structure, algorithm, performance, api_design, data_model, architecture, logic, security, error_handling, function_impl, testing_design, state_management, types
- concept: the reusable engineering principle in 3-8 words, repo-agnostic (e.g. "batch queries instead of N+1", "hold lock across read-modify-write", "validate input at API boundary").
For every item give paraphrase (one line, English, what the user wants) and agent_did (one line: what the agent had done or proposed).

Reply with JSON lines only, one object per item, in input order:
{"id": ..., "label": "...", "category": "..." or null, "concept": "..." or null, "paraphrase": "...", "agent_did": "..."}"""

lock = threading.Lock()
state = {"total": 0.0, "calls": 0, "stop": False}

def load_done():
    done = set()
    p = E / "labels.jsonl"
    if p.exists():
        for line in p.open():
            done.add(json.loads(line)["id"])
    q = E / "calls.jsonl"
    if q.exists():
        for line in q.open():
            d = json.loads(line); state["total"] = d["running_total"]; state["calls"] = d["call"]
    return done

def run(batch):
    with lock:
        if state["stop"] or state["total"] >= CAP - SAFETY:
            state["stop"] = True
            return
    items = "\n\n".join(
        f"### id={r['id']}\nAGENT (tail): {r['prev_assistant'][-300:]!r}\nUSER: {r['content'][:900]!r}" for r in batch)
    cmd = ["claude", "-p", "--model", "haiku", "--setting-sources", "project", "--strict-mcp-config",
           "--tools", "", "--system-prompt", SYSTEM, "--no-session-persistence", "--output-format", "json"]
    try:
        p = subprocess.run(cmd, input=items, capture_output=True, text=True, timeout=300, cwd=CWD)
        d = json.loads(p.stdout)
    except Exception as ex:
        print("call failed", ex, file=sys.stderr); return
    cost = d.get("total_cost_usd") or 0.0
    got = []
    for line in (d.get("result") or "").splitlines():
        line = line.strip().strip(",")
        if line.startswith("{"):
            try: got.append(json.loads(line))
            except Exception: pass
    ids = {r["id"] for r in batch}
    got = [g for g in got if g.get("id") in ids]
    with lock:
        state["total"] += cost; state["calls"] += 1
        with (E / "calls.jsonl").open("a") as f:
            f.write(json.dumps(dict(call=state["calls"], model=next(iter(d.get("modelUsage", {"?": 0}))),
                                    items=len(batch), parsed=len(got), cost=cost,
                                    running_total=round(state["total"], 6))) + "\n")
        with (E / "labels.jsonl").open("a") as f:
            for g in got: f.write(json.dumps(g, ensure_ascii=False) + "\n")
        print(f"call {state['calls']} parsed {len(got)}/{len(batch)} cost {cost:.4f} total {state['total']:.4f}", flush=True)

def main():
    CWD.mkdir(exist_ok=True)
    n = int(sys.argv[1])
    done = load_done()
    c = pd.read_parquet(E / "candidates.parquet").sort_values("priority", ascending=False).head(n)
    todo = [r for r in c[["id", "content", "prev_assistant"]].to_dict("records") if r["id"] not in done]
    batches = [todo[i:i + BATCH] for i in range(0, len(todo), BATCH)]
    print("todo", len(todo), "batches", len(batches), "spent", round(state["total"], 4))
    with ThreadPoolExecutor(WORKERS) as ex:
        list(ex.map(run, batches))
    print("done, total", round(state["total"], 4), "stopped_at_cap" if state["stop"] else "")

if __name__ == "__main__":
    main()
