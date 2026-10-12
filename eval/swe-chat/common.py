"""Shared helpers for the SWE-chat anticipation eval (reports/swe-chat.md).

Data: ~/.local/share/borg/eval/swe-chat/ (sample.csv, prompt_labels.parquet, transcripts_compact/).
Outputs go to DATA / "run". Nothing here touches ~/.config/borg or ~/.local/share/borg/collectives.
"""
import datetime, json, os, pathlib, re, subprocess, time

HOME = pathlib.Path.home()
DATA = HOME / ".local/share/borg" / "eval" / "swe-chat"
OUT = DATA / "run"
ROOT = pathlib.Path(__file__).resolve().parents[2]
BORG = ROOT / "target/release/borg"
SEED = 20261012
CAP_USD = 45.0
COST_LOG = OUT / "claude-calls.jsonl"

FILLER = re.compile(r"^\s*(please\s+)?(continue|go on|go ahead|proceed|yes|yep|yeah|ok(ay)?|sure|do it|carry on|keep going|next)\b[\s.!,]*"
                    r"(please|then|thanks|thank you)?[\s.!]*$", re.I)


def jl(p):
    p = pathlib.Path(p)
    return [json.loads(l) for l in open(p)] if p.exists() else []


def ms(ts):
    return int(datetime.datetime.fromisoformat(str(ts).replace("Z", "+00:00")).timestamp() * 1000)


def cut(s, n, head=False):
    s = s or ""
    if len(s) <= n:
        return s
    return s[:n] + " …[cut]" if head else "[cut]… " + s[-n:]


def human(t):
    t = (t or "").strip()
    return bool(t) and not t.startswith("<") and not t.startswith("This session is being continued") \
        and not t.startswith("[Request interrupted")


def events(session):
    """Ordered events of a compact transcript: user prompts, agent text, tool calls (result inlined)."""
    out = []
    for line in open(DATA / "transcripts_compact" / f"{session}.jsonl"):
        v = json.loads(line)
        ts = v.get("ts")
        if v["type"] == "user":
            for c in v.get("content", []):
                if c.get("type") in (None, "text") and c.get("text"):
                    out.append({"kind": "user", "ts": ts, "text": c["text"]})
        elif v["type"] == "assistant":
            for c in v.get("content", []):
                if c.get("type") == "text" and c.get("text", "").strip():
                    out.append({"kind": "agent", "ts": ts, "text": c["text"]})
                elif c.get("type") == "tool_use":
                    res = c.get("result") or {}
                    out.append({"kind": "tool", "ts": ts, "name": c.get("name", ""), "input": c.get("input") or {},
                                "output": str(res.get("output", ""))[:4000], "status": res.get("status", "")})
    return out


def tool_line(e, n=300):
    inp = e["input"]
    arg = inp.get("command") or inp.get("file_path") or inp.get("pattern") or inp.get("description") or json.dumps(inp)[:200]
    out = cut(e["output"].strip(), n)
    return f"[tool {e['name']}] {cut(str(arg), 300, head=True)}" + (f"\n  -> {out}" if out else "")


def render(evs):
    parts = []
    for e in evs:
        if e["kind"] == "user":
            parts.append(f"USER: {e['text']}" if human(e["text"]) else f"[harness] {cut(e['text'], 200, head=True)}")
        elif e["kind"] == "agent":
            parts.append(f"AGENT: {e['text']}")
        else:
            parts.append(tool_line(e))
    return "\n\n".join(parts)


def context(evs, idx, limit=12000):
    """Transcript before event idx (the hidden prompt): from the user's previous prompt to the agent's last
    message, tail-cut to `limit` characters while always keeping the previous prompt (cut to 2k)."""
    prev = max((i for i in range(idx) if evs[i]["kind"] == "user" and human(evs[i]["text"])), default=None)
    if prev is None:
        return None
    head = f"USER: {cut(evs[prev]['text'], 2000, head=True)}"
    body = render(evs[prev + 1:idx])
    room = max(2000, limit - len(head))
    if len(body) > room:
        body = "[earlier turn activity cut]…\n" + body[-room:]
    return head + "\n\n" + body


def spent():
    return sum(r.get("cost") or 0 for r in jl(COST_LOG))


def claude(prompt, tag, schema=None, system=None, model="sonnet", timeout=300):
    """One `claude -p` call (no tools, no settings, no session), logged with its cost and a running total.
    Raises RuntimeError when the approved cap would be crossed."""
    total = spent()
    if total >= CAP_USD - 0.5:
        raise RuntimeError(f"budget cap reached: ${total:.2f}")
    cmd = ["claude", "-p", "--model", model, "--tools", "", "--output-format", "json", "--setting-sources", "",
           "--strict-mcp-config", "--no-session-persistence"]
    if schema:
        cmd += ["--json-schema", json.dumps(schema)]
    if system:
        cmd += ["--system-prompt", system]
    env = {k: v for k, v in os.environ.items() if not k.startswith(("BORG_", "CLAUDECODE", "CLAUDE_CODE"))}
    t0 = time.monotonic()
    err, res = None, {}
    try:
        r = subprocess.run(cmd, input=prompt, capture_output=True, text=True, timeout=timeout, env=env, cwd="/tmp")
        res = json.loads(r.stdout)
        if res.get("is_error"):
            err = str(res.get("result"))[:300]
    except Exception as e:  # noqa: BLE001
        err = repr(e)[:300]
    cost = res.get("total_cost_usd") or 0
    out = res.get("structured_output")
    if out is None and not err:
        try:
            out = json.loads(res.get("result", "")) if schema else res.get("result", "")
        except Exception:
            out, err = None, "unparsed"
    usage = res.get("usage") or {}
    rec = {"tag": tag, "model": model, "cost": cost, "error": err, "wall_s": round(time.monotonic() - t0, 1),
           "in": usage.get("input_tokens"), "cache_w": usage.get("cache_creation_input_tokens"),
           "cache_r": usage.get("cache_read_input_tokens"), "out": usage.get("output_tokens")}
    OUT.mkdir(parents=True, exist_ok=True)
    with open(COST_LOG, "a") as f:
        f.write(json.dumps({**rec, "running_total": round(total + cost, 4)}) + "\n")
    return {"out": out, "cost": cost, "error": err}
