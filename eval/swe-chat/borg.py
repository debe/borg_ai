#!/usr/bin/env python3
"""Borg arm: one store per repo learned through the real `borg learn`, then the real Stop hook per case.

  python3 eval/swe-chat/borg.py learn [--workers 8]   # episodes -> run/stores/<repo>/ via `borg learn`
  python3 eval/swe-chat/borg.py hook  [--workers 6]   # `borg hook claude` per case -> run/borg_hook.jsonl

Every borg process: BORG_HOME = an eval store (a temp copy for the hook), BORG_CONFIG = a missing temp file,
BORG_STATE = temp. Borg's own `claude` calls go through a shim on PATH that logs cost into run/claude-calls.jsonl
(running total) and refuses calls once the cap is reached.
"""
import argparse, concurrent.futures as cf, json, os, pathlib, re, shutil, subprocess, sys, tempfile, time

from common import BORG, CAP_USD, COST_LOG, OUT, events, human, jl, ms

STORES = OUT / "stores"


def shim_dir():
    d = OUT / "shim"
    d.mkdir(parents=True, exist_ok=True)
    real = shutil.which("claude", path=os.environ["PATH"].replace(str(d) + ":", ""))
    p = d / "claude"
    p.write_text(
        "#!/usr/bin/env python3\n"
        "import json, os, subprocess, sys, time, fcntl\n"
        f"LOG = {str(COST_LOG)!r}; CAP = {CAP_USD - 0.5!r}\n"
        "def total():\n"
        "    try: return sum((json.loads(l).get('cost') or 0) for l in open(LOG))\n"
        "    except FileNotFoundError: return 0\n"
        "if total() >= CAP:\n"
        "    sys.stderr.write('budget cap reached\\n'); sys.exit(1)\n"
        f"t0 = time.monotonic(); r = subprocess.run([{real!r}] + sys.argv[1:], capture_output=True)\n"
        "sys.stdout.buffer.write(r.stdout); sys.stderr.buffer.write(r.stderr)\n"
        "try: res = json.loads(r.stdout); cost = res.get('total_cost_usd') or 0; err = res.get('is_error')\n"
        "except Exception: cost, err = 0, 'unparsed'\n"
        "a = sys.argv[1:]; model = a[a.index('--model') + 1] if '--model' in a else None\n"
        "with open(LOG, 'a') as f:\n"
        "    fcntl.flock(f, fcntl.LOCK_EX)\n"
        "    f.write(json.dumps({'tag': os.environ.get('SWE_TAG'), 'model': model, 'cost': cost, 'error': err,\n"
        "        'wall_s': round(time.monotonic() - t0, 1), 'running_total': round(total() + cost, 4)}) + '\\n')\n"
        "sys.exit(r.returncode)\n")
    p.chmod(0o755)
    return d


def env_for(tmp, home, tag, before=None):
    env = {k: v for k, v in os.environ.items() if not k.startswith(("BORG_", "CLAUDECODE", "CLAUDE_CODE"))}
    env.update(BORG_HOME=str(home), BORG_STATE=str(tmp / "state"), BORG_CONFIG=str(tmp / "no-collectives.json"),
               PATH=f"{shim_dir()}:{os.environ['PATH']}", SWE_TAG=tag)
    if before is not None:
        env["BORG_BEFORE"] = str(before)
    assert not (tmp / "no-collectives.json").exists()
    assert str(home).startswith(str(OUT)) or str(home).startswith(tempfile.gettempdir())
    return env


def learn_repo(src):
    slug = src.stem
    home = STORES / slug
    home.mkdir(parents=True, exist_ok=True)
    have = {e["id"] for e in jl(home / "episodes.jsonl")}
    with open(home / "episodes.jsonl", "a") as f:
        for e in jl(src):
            if e["id"] not in have:
                f.write(json.dumps(e) + "\n")
    tmp = pathlib.Path(tempfile.mkdtemp(prefix="swe-learn-"))
    try:
        t0 = time.monotonic()
        r = subprocess.run([str(BORG), "learn"], capture_output=True, text=True, env=env_for(tmp, home, f"learn:{slug}"), timeout=4 * 3600)
        rec = {"repo": slug, "rc": r.returncode, "stdout": r.stdout[-500:], "stderr_tail": r.stderr[-800:],
               "wall_s": round(time.monotonic() - t0), "lessons": len({l["id"] for l in jl(home / "lessons.jsonl")})}
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    with open(OUT / "borg_learn.jsonl", "a") as f:
        f.write(json.dumps(rec) + "\n")
    print(slug, rec["stdout"].strip().splitlines()[-1:] if rec["stdout"] else rec["stderr_tail"][-200:], flush=True)
    return rec


def cmd_learn(a):
    srcs = sorted((OUT / "episodes").glob("*.jsonl"))
    with cf.ThreadPoolExecutor(a.workers) as ex:
        list(ex.map(learn_repo, srcs))


def hook_events(c):
    """The case's session up to the hidden prompt, as hook inputs: prompts and tool calls (last 200)."""
    evs = events(c["session"])[:c["idx"]]
    out = []
    for e in evs:
        if e["kind"] == "user" and human(e["text"]):
            out.append(("prompt", {"prompt": e["text"]}))
        elif e["kind"] == "tool":
            out.append(("post_tool", {"tool_name": e["name"], "tool_input": e["input"]}))
    prompts = [x for x in out if x[0] == "prompt"]
    tools = [x for x in out if x[0] == "post_tool"][-200:]
    keep = set(map(id, prompts + tools))
    last = next((e["text"] for e in reversed(evs) if e["kind"] == "agent"), "")
    return [x for x in out if id(x) in keep], last


def lessons_by_id(home):
    by = {}
    for l in jl(home / "lessons.jsonl"):
        if l["id"] not in by or by[l["id"]]["version"] <= l["version"]:
            by[l["id"]] = l
    return by


def run_hook(c, store=None):
    store = store or STORES / c["repo"].replace("/", "__")
    tmp = pathlib.Path(tempfile.mkdtemp(prefix="swe-hook-"))
    try:
        home = tmp / "store"
        shutil.copytree(store, home)
        T = ms(c["ts"])
        env = env_for(tmp, home, f"stop:{c['id']}", T)
        env.update(BORG_QUEEN="off", BORG_LEARN="off")
        session = "swe-" + c["id"]
        base = {"session_id": session, "cwd": f"/tmp/swe-chat/{c['repo'].split('/')[-1]}"}
        evs, last = hook_events(c)
        free_early = []

        def hook(event, extra):
            r = subprocess.run([str(BORG), "hook", "claude", event], input=json.dumps({**base, **extra}),
                               capture_output=True, text=True, env=env, timeout=900)
            return r.stdout

        for ev, extra in evs:
            o = hook(ev, extra)
            if o.strip():
                free_early.append(o.strip()[:4000])
        t0 = time.monotonic()
        out = hook("stop", {"last_assistant_message": last, "stop_hook_active": False})
        wall = round(time.monotonic() - t0, 2)
        sdir = tmp / "state"
        name = re.sub(r"[^A-Za-z0-9-]", "", session)
        ctx = sdir / f"{name}.ctx"
        lines = ctx.read_text().splitlines() if ctx.exists() else []
        tag = lambda p: [l[len(p):].strip() for l in lines if l.startswith(p)]
        asked = [x for x in ",".join(tag("#borg-asked ")).split(",") if x]
        queued_f = sdir / f"{name}.queued"
        queued = [x for x in ",".join(queued_f.read_text().split()).split(",") if x] if queued_f.exists() else []
        cost_f = sdir / f"{name}.cost"
        block = json.loads(out) if out.strip() else None
        ls = lessons_by_id(store)
        fmt = lambda i: {"id": i, "question": ls[i]["question"], "consequence": ls[i]["consequence"]} if i in ls else {"id": i}
        return {"id": c["id"], "blocked": bool(block), "reason": (block or {}).get("reason"), "asked": [fmt(i) for i in asked],
                "free": [fmt(i) for i in queued if i not in asked], "skip": tag("#borg-skip "),
                "predict_cost": sum(float(x) for x in tag("#borg-predict-cost ")), "stop_wall_s": wall,
                "early_outputs": free_early, "cost_log": cost_f.read_text()[-1500:] if cost_f.exists() else "",
                "lessons_before": sum(1 for l in ls.values() if l["origin_ms"] < T), "n_events": len(evs), "error": None}
    except Exception as e:  # noqa: BLE001
        return {"id": c["id"], "error": repr(e)[:400]}
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def cmd_hook(a):
    path = OUT / "borg_hook.jsonl"
    have = {r["id"] for r in jl(path) if not r.get("error")}
    todo = [c for c in jl(OUT / "cases.jsonl") if c["id"] not in have]
    if a.only:
        todo = [c for c in todo if c["id"] in a.only.split(",")]
    with cf.ThreadPoolExecutor(a.workers) as ex, open(path, "a") as f:
        for r in ex.map(run_hook, todo):
            f.write(json.dumps(r) + "\n")
            f.flush()
            print(r["id"], "err" if r.get("error") else f"blocked={r['blocked']} asked={len(r['asked'])} free={len(r['free'])} "
                  f"skip={r['skip']} cost={r['predict_cost']:.3f} lessons={r['lessons_before']}", flush=True)


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["learn", "hook"])
    ap.add_argument("--workers", type=int, default=6)
    ap.add_argument("--only", default="")
    a = ap.parse_args()
    {"learn": cmd_learn, "hook": cmd_hook}[a.cmd](a)
