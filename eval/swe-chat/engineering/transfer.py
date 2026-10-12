#!/usr/bin/env python3
"""Cross-repo transfer test (reports/swe-chat-engineering-transfer.md): do engineering lessons learned EARLIER in
OTHER repos help anticipate an engineer's correction?

  python3 eval/swe-chat/engineering/transfer.py prep        # cases, contexts, memory pool (0 tokens)
  python3 eval/swe-chat/engineering/transfer.py episodes    # pool episodes for borg learn (0 tokens; needs pool_prompts)
  python3 eval/swe-chat/engineering/transfer.py learn       # the real `borg learn` on the pool, once (Sonnet, cached)
  python3 eval/swe-chat/engineering/transfer.py stores      # one leak-free store per case + vector index (0 tokens)
  python3 eval/swe-chat/engineering/transfer.py check       # 0-token checks: BM25-only Stop, gate on/off, L retrieval
  python3 eval/swe-chat/engineering/transfer.py hook        # the real Stop per case -> borg_hook.jsonl
  python3 eval/swe-chat/engineering/transfer.py arm N|L|F   # Sonnet arms -> arm_<X>.jsonl
  python3 eval/swe-chat/engineering/transfer.py judge       # blind pooled judge -> judge.jsonl
  python3 eval/swe-chat/engineering/transfer.py report      # -> summary.json

Data and every store: ~/.local/share/borg/eval/swe-chat/engineering/transfer/ (T). Paid calls: common.claude and
borg's logging shim, both into T/calls.jsonl with a running total; hard cap $15 (calls refused from $14.30 on, so
calls in flight cannot cross $15).
"""
import argparse, collections, concurrent.futures as cf, json, os, pathlib, random, re, shutil, subprocess, sys, tempfile

import pandas as pd

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

import common  # noqa: E402
from common import SEED, cut, human, jl, ms  # noqa: E402

E = common.HOME / ".local/share/borg/eval/swe-chat/engineering"
T = E / "transfer"
T.mkdir(parents=True, exist_ok=True)
(T / "tmp").mkdir(exist_ok=True)
tempfile.tempdir = str(T / "tmp")  # hook copies of stores live under the eval dir too
common.OUT = T
common.COST_LOG = T / "calls.jsonl"
common.CAP_USD = 14.8  # common.claude and the shim refuse from CAP - 0.5 = $14.30

import arms as A  # noqa: E402
import borg as B  # noqa: E402
import followup as FU  # noqa: E402
import judge as J  # noqa: E402
import report as R  # noqa: E402

common.COST_LOG = T / "calls.jsonl"
common.CAP_USD = 14.8
B.OUT, B.COST_LOG, B.CAP_USD = T, common.COST_LOG, common.CAP_USD
B.STORES = T / "stores"
GLOBAL = T / "pool_store"
N_DISTRACT = 48
NO_CTX = "(This message opens the session: no earlier agent activity is logged in it.)"
L_TOP, L_ITEM = 10, 400


def short(repo):
    """Repos sharing a name (forks, mirrors: osabiohq/osabio and marcus-sa/osabio) count as one repo."""
    return repo.split("/")[-1].lower()


def iso(t):
    return None if pd.isna(t) else pd.Timestamp(t).isoformat()


def tms(t):
    return None if t is None or pd.isna(t) else int(pd.Timestamp(t).timestamp() * 1000)


# ---------- prep ----------

def quality():
    """The 1,291 quality tier with a time interval [lo, hi] per correction: its own timestamp, else bounded by the
    session's nearest timestamped prompts before and after it (157 lack one)."""
    q = pd.read_parquet(E / "quality_set.parquet")
    ts = pd.read_parquet(T / "prompt_ts.parquet").dropna(subset=["timestamp"]).sort_values(["session_id", "turn_number"])
    by = {s: x for s, x in ts.groupby("session_id")}
    lo, hi = [], []
    for r in q.itertuples():
        if pd.notna(r.timestamp):
            lo.append(r.timestamp); hi.append(r.timestamp)
            continue
        x = by.get(r.session_id)
        lo.append(x[x.turn_number < r.turn_number].timestamp.max() if x is not None else pd.NaT)
        hi.append(x[x.turn_number > r.turn_number].timestamp.min() if x is not None else pd.NaT)
    q["lo"], q["hi"] = lo, hi
    q["lo_ms"] = q.lo.map(tms)
    q["hi_ms"] = q.hi.map(tms)
    return q


def session_events(rows):
    """Ordered events of one session from conversations rows: user prompts, agent text, tool calls with results."""
    rows = rows.sort_values("turn_number")
    res = {r.tool_call_id: str(r.content or "") for r in rows.itertuples() if r.turn_type == "tool_result" and r.tool_call_id}
    out = []
    for r in rows.itertuples():
        ts = iso(r.timestamp)
        if r.turn_type == "user_prompt" and r.content:
            out.append({"kind": "user", "ts": ts, "text": r.content, "turn": int(r.turn_number)})
        elif r.turn_type == "assistant_response" and str(r.content or "").strip():
            out.append({"kind": "agent", "ts": ts, "text": r.content, "turn": int(r.turn_number)})
        elif r.turn_type == "tool_use":
            try:
                inp = json.loads(r.tool_input_json) if r.tool_input_json else {}
            except Exception:
                inp = {}
            if not isinstance(inp, dict):
                inp = {"input": inp}
            out.append({"kind": "tool", "ts": ts, "name": r.tool_name or "", "input": inp, "turn": int(r.turn_number),
                        "output": res.get(r.tool_call_id, "")[:4000], "status": ""})
    return out


def cmd_prep(a):
    q = quality()
    test = q[q.q_prior_other_repo & q.has_commit].copy()
    rows = pd.read_parquet(T / "test_rows.parquet")
    evs_all = {}
    cases = []
    for r in test.sort_values(["lo", "id"]).itertuples():
        if r.session_id not in evs_all:
            evs_all[r.session_id] = session_events(rows[rows.session_id == r.session_id])
        evs = evs_all[r.session_id]
        idx = next((i for i, e in enumerate(evs) if e["kind"] == "user" and e["turn"] == r.turn_number), None)
        ctx = common.context(evs, idx) if idx is not None else None
        if ctx is None and idx is not None:  # no earlier human prompt: the agent's activity before the correction
            body = common.render(evs[max(0, idx - 40):idx])
            ctx = (("[cut]… " + body[-12000:]) if len(body) > 12000 else body) or NO_CTX
        lo = r.lo if pd.notna(r.lo) else r.hi
        src = q[(q.canon == r.canon) & (q.repo_id.map(short) != short(r.repo_id)) & (q.hi_ms < (r.lo_ms or -1))]
        cases.append({"id": f"t{len(cases):03d}", "qid": r.id, "kind": "pos", "session": r.session_id, "repo": r.repo_id,
                      "agent": r.agent, "user": r.user, "turn": int(r.turn_number), "idx": idx, "ts": iso(lo),
                      "lo_ms": r.lo_ms, "hi_ms": r.hi_ms, "prompt": r.content, "context": ctx or "",
                      "category": r.category, "canon": r.canon, "strong": bool(r.strong), "s1": float(r.s1 or 0),
                      "nd_eng": int(r.nd_eng), "sources": sorted(src.id)})
    # No timestamped row before the correction (11 OpenCode/Cursor sessions log none at all, 1 opens its session):
    # lower bound = the session's created_at minus 20 days (created_at - first prompt time is <= 18.5 days for 99% of
    # the 1,964 dated sessions of the 5,680 set). Flagged `time_inferred`; amendment 1 of the report.
    created = pd.read_parquet(common.DATA / "sessions.parquet", columns=["session_id", "created_at"]).set_index("session_id").created_at
    for c in cases:
        c["time_inferred"] = c["lo_ms"] is None or pd.isna(c["lo_ms"])
        if c["time_inferred"]:
            c["lo_ms"] = tms(created[c["session"]] - pd.Timedelta(days=20))
            c["ts"] = iso(pd.Timestamp(c["lo_ms"], unit="ms", tz="UTC"))
            if c["hi_ms"] is None or pd.isna(c["hi_ms"]):
                c["hi_ms"] = None
    order = [c["id"] for c in cases]
    random.Random(SEED).shuffle(order)
    rank = {cid: i for i, cid in enumerate(order)}
    for c in cases:
        c["order"] = rank[c["id"]]
    with open(T / "cases.jsonl", "w") as f:
        for c in cases:
            f.write(json.dumps(c, ensure_ascii=False) + "\n")
    with open(T / "events.jsonl", "w") as f:
        for s, evs in evs_all.items():
            f.write(json.dumps({"session": s, "events": evs}, ensure_ascii=False) + "\n")
    # memory pool: every concept source of a case + a seeded random draw of other quality-tier corrections
    srcs = sorted({s for c in cases for s in c["sources"]})
    rest = sorted(set(q[q.hi_ms.notna() & q.lo_ms.notna()].id) - set(srcs))
    distract = sorted(random.Random(SEED).sample(rest, N_DISTRACT))
    pool = q[q.id.isin(srcs + distract)].copy()
    pool["role"] = pool.id.map(lambda i: "source" if i in set(srcs) else "distractor")
    pool[["id", "session_id", "turn_number", "repo_id", "user", "lo_ms", "hi_ms", "content", "prev_assistant", "canon", "category",
          "role"]].to_parquet(T / "pool.parquet")
    st = {"cases": len(cases), "no_context": sum(1 for c in cases if c["context"] == NO_CTX), "no_idx": sum(1 for c in cases if c["idx"] is None),
          "cases_with_strict_source": sum(1 for c in cases if c["sources"]), "sources": len(srcs), "distractors": len(distract),
          "pool": len(pool), "pool_sessions": int(pool.session_id.nunique()), "pool_repos": int(pool.repo_id.nunique()),
          "context_chars_median": int(pd.Series([len(c["context"]) for c in cases]).median()),
          "time_inferred": sum(1 for c in cases if c["time_inferred"]),
          "entireio_cli": sum(1 for c in cases if c["repo"] == "entireio/cli"), "strong": sum(1 for c in cases if c["strong"]),
          "agents": collections.Counter(c["agent"] for c in cases)}
    (T / "prep.json").write_text(json.dumps(st, indent=1))
    print(json.dumps(st, indent=1))


# ---------- episodes ----------

def cmd_episodes(a):
    """Pool corrections as borg Episodes, as the earlier harness built them: task = the latest earlier prompt of the
    session that is not itself pushback, agent = the agent's text before the correction (tail), engineer = user."""
    pool = pd.read_parquet(T / "pool.parquet")
    pr = pd.read_parquet(T / "pool_prompts.parquet")
    lab = pd.read_parquet(common.DATA / "prompt_labels.parquet", columns=["session_id", "turn_number", "prompt_pushback"])
    pr = pr.merge(lab, on=["session_id", "turn_number"], how="left").sort_values(["session_id", "turn_number"])
    by = {s: x for s, x in pr.groupby("session_id")}
    eps = []
    for r in pool.itertuples():
        x = by.get(r.session_id)
        task = ""
        if x is not None:
            prev = x[(x.turn_number < r.turn_number) & ~x.prompt_pushback.isin(["correction", "rejection"])]
            prev = prev[prev.content.map(lambda t: human(t or ""))]
            if len(prev):
                task = prev.content.iloc[-1]
        eps.append({"id": r.id.replace("#", "-"), "ms": int(r.hi_ms), "repo": short(r.repo_id), "session": r.session_id,
                    "task": cut(task, 1500, head=True), "agent": (r.prev_assistant or "")[-2000:], "rejected_tool": None,
                    "correction": cut(r.content, 1500, head=True), "engineer": r.user or "", "qid": r.id, "repo_id": r.repo_id,
                    "lo_ms": int(r.lo_ms), "role": r.role})
    eps.sort(key=lambda e: (e["ms"], e["id"]))
    with open(T / "episodes_all.jsonl", "w") as f:
        for e in eps:
            f.write(json.dumps(e, ensure_ascii=False) + "\n")
    print("episodes", len(eps), "with task", sum(1 for e in eps if e["task"]), "with agent text", sum(1 for e in eps if e["agent"]))


# ---------- learn (paid, once) ----------

def cmd_learn(a):
    """The real `borg learn` over the whole pool in time order, one store: each episode is generalised once (the
    cache); per-case stores are cut from it (`stores`)."""
    GLOBAL.mkdir(parents=True, exist_ok=True)
    have = {e["id"] for e in jl(GLOBAL / "episodes.jsonl")}
    fields = ["id", "ms", "repo", "session", "task", "agent", "rejected_tool", "correction", "engineer"]
    with open(GLOBAL / "episodes.jsonl", "a") as f:
        for e in jl(T / "episodes_all.jsonl")[: a.n or None]:
            if e["id"] not in have:
                f.write(json.dumps({k: e[k] for k in fields}, ensure_ascii=False) + "\n")
    tmp = pathlib.Path(tempfile.mkdtemp(prefix="learn-"))
    try:
        r = subprocess.run([str(common.BORG), "learn"], capture_output=True, text=True, env=B.env_for(tmp, GLOBAL, "learn:pool"), timeout=6 * 3600)
        rec = {"rc": r.returncode, "stdout": r.stdout[-800:], "stderr_tail": r.stderr[-1500:],
               "lessons": len({l["id"] for l in jl(GLOBAL / "lessons.jsonl")})}
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    with open(T / "borg_learn.jsonl", "a") as f:
        f.write(json.dumps(rec) + "\n")
    print(rec, f"total ${common.spent():.2f}")


# ---------- per-case stores (0 tokens) ----------

def allowed_eps(c, eps):
    """Pool episodes a case may see: another repo (by name) and strictly earlier (the episode's latest possible
    time before the case's earliest possible time)."""
    return {e["id"] for e in eps if short(e["repo_id"]) != short(c["repo"]) and e["ms"] < (c["lo_ms"] or c["hi_ms"])}


def cmd_stores(a):
    eps = jl(T / "episodes_all.jsonl")
    versions = jl(GLOBAL / "lessons.jsonl")
    cases = jl(T / "cases.jsonl")
    if a.only:
        cases = [c for c in cases if c["id"] in a.only.split(",")]
    stats = []
    for c in cases:
        ok = allowed_eps(c, eps)
        last = {}
        for i, v in enumerate(versions):
            if set(v["episodes"]) <= ok:
                last[v["id"]] = i  # the latest version every one of whose episodes the case may see
        keep = sorted(last.values())
        home = B.STORES / c["id"]
        if home.exists():
            shutil.rmtree(home)
        home.mkdir(parents=True)
        with open(home / "lessons.jsonl", "w") as f:
            for i in keep:
                f.write(json.dumps(versions[i], ensure_ascii=False) + "\n")
        with open(home / "episodes.jsonl", "w") as f:
            for e in eps:
                if e["id"] in ok:
                    f.write(json.dumps({k: e[k] for k in ["id", "ms", "repo", "session", "task", "agent", "rejected_tool", "correction", "engineer"]},
                                       ensure_ascii=False) + "\n")
        tmp = pathlib.Path(tempfile.mkdtemp(prefix="vec-"))
        try:
            r = subprocess.run([str(common.BORG), "vectors", "--rebuild"], capture_output=True, text=True, env=B.env_for(tmp, home, "vectors"), timeout=600)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)
        src_eps = {s.replace("#", "-") for s in c["sources"]}
        src_lessons = sorted({versions[i]["id"] for i in keep if set(versions[i]["episodes"]) & src_eps})
        stats.append({"id": c["id"], "episodes": len(ok), "lessons": len(keep), "source_lessons": src_lessons,
                      "vectors_rc": r.returncode, "vectors_out": (r.stdout + r.stderr)[-200:]})
        print(c["id"], len(ok), len(keep), len(src_lessons), r.returncode, flush=True)
    with open(T / "stores.jsonl", "w" if not a.only else "a") as f:
        for s in stats:
            f.write(json.dumps(s) + "\n")


# ---------- hook replay ----------

EVS = {}


def events_of(session):
    if not EVS:
        for r in jl(T / "events.jsonl"):
            EVS[r["session"]] = r["events"]
    return EVS[session]


B.events = events_of


def run_case_hook(c, extra_env=None):
    """borg.run_hook on the case's leak-free store; extra_env for the 0-token checks."""
    if extra_env:
        old = B.env_for

        def env_for(tmp, home, tag, before=None):
            env = old(tmp, home, tag, before)
            env.update(extra_env)
            return env
        B.env_for = env_for
        try:
            return B.run_hook(c, B.STORES / c["id"])
        finally:
            B.env_for = old
    return B.run_hook(c, B.STORES / c["id"])


def todo_cases(path, a, need=None):
    have = {r["id"] for r in jl(path) if not r.get("error")}
    cs = sorted(jl(T / "cases.jsonl"), key=lambda c: c["order"])
    if a.n:
        cs = cs[: a.n]
    if a.only:
        cs = [c for c in cs if c["id"] in a.only.split(",")]
    if need is not None:
        cs = [c for c in cs if c["id"] in need]
    return [c for c in cs if c["id"] not in have]


def cmd_hook(a):
    path = T / "borg_hook.jsonl"
    todo = todo_cases(path, a)
    with cf.ThreadPoolExecutor(a.workers) as ex, open(path, "a") as f:
        for r in ex.map(run_case_hook, todo):
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
            f.flush()
            print(r["id"], "err " + str(r.get("error")) if r.get("error") else f"blocked={r['blocked']} asked={len(r['asked'])} free={len(r['free'])} "
                  f"skip={r['skip']} cost={r['predict_cost']:.3f} lessons={r['lessons_before']}", f"total ${common.spent():.2f}", flush=True)


# ---------- L retrieval (0 tokens) ----------

def plain_log(c, eps):
    """L: the raw earlier other-repo corrections of the pool, BM25 (Borg's tokeniser and BM25, ported) over the
    correction text with the turn context as query, top L_TOP, most relevant first."""
    ok = allowed_eps(c, eps)
    docs = [e for e in eps if e["id"] in ok]
    if not docs:
        return "", []
    sc = FU.bm25([FU.tokens(e["correction"]) for e in docs], c["context"])
    top = [i for i in sorted(range(len(docs)), key=lambda i: (-sc[i], docs[i]["id"]))[:L_TOP] if sc[i] > 0]
    lines = [f"- [{pd.Timestamp(docs[i]['ms'], unit='ms'):%Y-%m-%d}] ({docs[i]['repo_id']}) "
             f"{cut(re.sub(chr(10) + '+', ' / ', docs[i]['correction']), L_ITEM, head=True)}" for i in top]
    return "\n".join(lines), [docs[i]["id"] for i in top]


def cmd_check(a):
    """0 tokens: on a handful of cases, Borg's Stop with BORG_MATCH=bm25 (no model call), system gate on vs off, and
    whether the concept-source lesson is in the store / surfaced; L's retrieval and whether it finds a source."""
    eps = jl(T / "episodes_all.jsonl")
    st = {s["id"]: s for s in jl(T / "stores.jsonl")}
    cs = sorted(jl(T / "cases.jsonl"), key=lambda c: c["order"])[: a.n or 8]
    out = []
    for c in cs:
        on = run_case_hook(c, {"BORG_MATCH": "bm25"})
        off = run_case_hook(c, {"BORG_MATCH": "bm25", "BORG_SYSTEM_GATE": "off"})
        log, ids = plain_log(c, eps)
        src = set(st.get(c["id"], {}).get("source_lessons", []))
        srce = {s.replace("#", "-") for s in c["sources"]}
        ids_on = [x["id"] for x in on.get("asked", []) + on.get("free", [])]
        ids_off = [x["id"] for x in off.get("asked", []) + off.get("free", [])]
        r = {"id": c["id"], "repo": c["repo"], "store_lessons": st.get(c["id"], {}).get("lessons"), "source_lessons_in_store": len(src),
             "gate_on": ids_on, "gate_off": ids_off, "gate_same": ids_on == ids_off, "source_surfaced_bm25": bool(src & set(ids_on)),
             "L_items": len(ids), "L_source_in_top": bool(srce & set(ids)), "err": on.get("error") or off.get("error")}
        out.append(r)
        print(json.dumps(r))
    print("cost so far", common.spent())


# ---------- arms ----------

F_HEAD = ("## Lessons a memory of earlier corrections in other repositories selected for this turn (question → consequence)")
L_HEAD = "## Earlier corrections by users of other repositories, retrieved for this turn (most relevant first)"


def arm_prompt(arm, c, hook, eps):
    turn = f"## The agent's turn (transcript since the user's previous message; may be cut)\n\n{c['context']}"
    if arm == "N":
        return turn, None
    if arm == "L":
        log, ids = plain_log(c, eps)
        return f"{L_HEAD}\n\n{log or '(none)'}\n\n{turn}", ids
    if arm == "F":
        h = hook.get(c["id"]) or {}
        ls = [l for l in (h.get("asked") or []) + (h.get("free") or []) if l.get("question")]
        block = "\n".join(f"- {l['question']} → {l['consequence']}" for l in ls) or "(none)"
        return f"{F_HEAD}\n\n{block}\n\n{turn}", [l["id"] for l in ls]
    raise ValueError(arm)


def cmd_arm(a):
    path = T / f"arm_{a.arm}.jsonl"
    hook = {r["id"]: r for r in jl(T / "borg_hook.jsonl") if not r.get("error")}
    eps = jl(T / "episodes_all.jsonl")
    todo = todo_cases(path, a, need=set(hook) if a.arm == "F" else None)

    def job(c):
        p, ids = arm_prompt(a.arm, c, hook, eps)
        try:
            r = common.claude(p, f"arm:{a.arm}:{c['id']}", schema=A.SCHEMA, system=A.SYSTEM)
        except RuntimeError as e:
            return {"id": c["id"], "error": str(e)}
        out = r["out"] or {}
        return {"id": c["id"], "concerns": [x for x in (out.get("concerns") or []) if isinstance(x, str)][:5], "memory": ids,
                "cost": r["cost"], "error": r["error"]}

    with cf.ThreadPoolExecutor(a.workers) as ex, open(path, "a") as f:
        for r in ex.map(job, todo):
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
            f.flush()
            print(r["id"], "ERR " + str(r["error"]) if r.get("error") else len(r["concerns"]), f"total ${common.spent():.2f}", flush=True)


# ---------- judge ----------

ARMS_IN = ("N", "L", "F")


def load_arms():
    arms = {x: {r["id"]: r for r in jl(T / f"arm_{x}.jsonl") if not r.get("error")} for x in ARMS_IN}
    arms["B"] = {r["id"]: r for r in jl(T / "borg_hook.jsonl") if not r.get("error")}
    return arms


def cmd_judge(a):
    arms = load_arms()
    path = T / "judge.jsonl"
    ready = set.intersection(*(set(v) for v in arms.values()))  # judge a case only once every arm has run on it
    todo = todo_cases(path, a, need=ready)

    def job(c):
        items = J.pooled(c, arms)
        present = sorted(x for x in arms if c["id"] in arms[x])
        if not items:
            return {"id": c["id"], "kind": "pos", "items": [], "verdicts": {}, "genuine": None, "arms_present": present, "cost": 0, "error": None}
        try:
            r = common.claude(J.prompt(c, items), f"judge:{c['id']}", schema=J.POS_SCHEMA, system=J.SYSTEM)
        except RuntimeError as e:
            return {"id": c["id"], "error": str(e)}
        out = r["out"] or {}
        v = {int(x["n"]): x.get("verdict") for x in out.get("items", [])}
        missing = [x["n"] for x in items if x["n"] not in v]
        return {"id": c["id"], "kind": "pos", "items": items, "verdicts": v, "genuine": out.get("genuine"), "arms_present": present,
                "cost": r["cost"], "error": r["error"] or (f"missing verdicts {missing}" if missing else None)}

    with cf.ThreadPoolExecutor(a.workers) as ex, open(path, "a") as f:
        for r in ex.map(job, todo):
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
            f.flush()
            print(r["id"], "ERR " + str(r["error"]) if r.get("error") else f"{len(r['items'])} items", f"total ${common.spent():.2f}", flush=True)


# ---------- batches ----------

STOP_AT = 14.30


def cmd_run(a):
    """Batches of 10 cases in the pre-registered order: Stop -> F -> N -> L -> judge. Before a batch, stop if the spend
    plus 10 x the mean cost per finished case would pass STOP_AT (cost only, never results)."""
    import types
    while True:
        calls = jl(common.COST_LOG)
        spent = sum(c["cost"] or 0 for c in calls)
        per = [c for c in calls if not (c.get("tag") or "").startswith("learn")]
        done = {r["id"] for r in jl(T / "judge.jsonl") if not r.get("error")}
        mean = sum(c["cost"] or 0 for c in per) / len(done) if done else 0.16
        n = len(done) + 10
        if n > 149 + 9 or spent + 10 * mean > STOP_AT:
            print(f"stop: done {len(done)} spent ${spent:.2f} mean/case ${mean:.3f}", flush=True)
            return
        print(f"batch to n={min(n, 149)}: spent ${spent:.2f} mean/case ${mean:.3f}", flush=True)
        b = types.SimpleNamespace(n=min(n, 149), only="", workers=a.workers, arm="")
        cmd_hook(b)
        for arm in ("F", "N", "L"):
            b.arm = arm
            cmd_arm(b)
        cmd_judge(b)
        if len({r["id"] for r in jl(T / "judge.jsonl") if not r.get("error")}) <= len(done):
            print("no progress; stopping", flush=True)
            return


# ---------- report ----------

def cmd_report(a):
    cases = {c["id"]: c for c in jl(T / "cases.jsonl")}
    st = {s["id"]: s for s in jl(T / "stores.jsonl")}
    hook = {r["id"]: r for r in jl(T / "borg_hook.jsonl") if not r.get("error")}
    armrows = {x: {r["id"]: r for r in jl(T / f"arm_{x}.jsonl") if not r.get("error")} for x in ARMS_IN}
    rows = []
    for r in jl(T / "judge.jsonl"):
        if r.get("error"):
            continue
        r = dict(r, repo=cases[r["id"]]["repo"])
        r["verdicts"] = {str(k): v for k, v in r["verdicts"].items()}
        rows.append(r)
    f_strict = lambda r, x: 1 if R.caught(r, x) else 0
    f_len = lambda r, x: 1 if R.caught(r, x, True) else 0
    ARMS = ["N", "L", "F", "B-block", "B-all"]
    sig = lambda c: c["strong"]
    subsets = {"all": rows, "excl_entireio_cli": [r for r in rows if r["repo"] != "entireio/cli"],
               "signal_authorship_or_ND": [r for r in rows if sig(cases[r["id"]])],
               "no_signal": [r for r in rows if not sig(cases[r["id"]])],
               "genuine": [r for r in rows if r.get("genuine")],
               "strict_source": [r for r in rows if cases[r["id"]]["sources"]],
               "source_lesson_in_store": [r for r in rows if st.get(r["id"], {}).get("source_lessons")]}
    s = {"n": len(rows), "catch": {}, "pairs": {}}
    for name, P_ in subsets.items():
        s["catch"][name] = {}
        for arm in ARMS:
            X = [r for r in P_ if R.arm_present(r, arm)]
            k, kl = sum(f_strict(r, arm) for r in X), sum(f_len(r, arm) for r in X)
            s["catch"][name][arm] = {"n": len(X), "k": k, "pct": round(100 * k / max(1, len(X)), 1), "ci": R.wilson(k, len(X)),
                                     "lenient_k": kl, "lenient_pct": round(100 * kl / max(1, len(X)), 1),
                                     "items": round(sum(sum(1 for x in r["items"] if R.srcs(x, arm)) for r in X) / max(1, len(X)), 2)}
        s["pairs"][name] = {f"{x} - {y}": {"strict": R.boot(P_, x, y, f_strict), "lenient": R.boot(P_, x, y, f_len)}
                            for x, y in [("B-all", "N"), ("F", "N"), ("L", "N"), ("F", "L"), ("B-all", "L"), ("F", "B-all"), ("B-block", "N")]}
    # retrieval diagnostics (0 tokens): is a concept-source lesson in the store, surfaced by the Stop, used by F; is a
    # concept-source correction in L's top 10
    diag = collections.Counter()
    for r in rows:
        c, h = cases[r["id"]], hook.get(r["id"], {})
        src = set(st.get(r["id"], {}).get("source_lessons", []))
        surfaced = {x["id"] for x in h.get("asked", []) + h.get("free", [])}
        diag["cases"] += 1
        diag["source_lesson_in_store"] += bool(src)
        diag["source_lesson_surfaced"] += bool(src & surfaced)
        diag["source_lesson_asked"] += bool(src & {x["id"] for x in h.get("asked", [])})
        diag["L_source_in_top10"] += bool({x.replace("#", "-") for x in c["sources"]} & set(armrows["L"].get(r["id"], {}).get("memory") or []))
        diag["stop_blocked"] += bool(h.get("blocked"))
        diag["stop_skipped"] += bool(h.get("skip"))
        diag["stop_nothing"] += not surfaced
    s["retrieval"] = dict(diag)
    ex = {"hits": [], "misses": []}
    for r in rows:
        c = cases[r["id"]]
        for arm in ("B-all", "F", "L"):
            if R.caught(r, arm) and not R.caught(r, "N"):
                ex["hits"].append({"id": r["id"], "arm": arm, "repo": c["repo"], "canon": c["canon"], "correction": cut(c["prompt"], 500, head=True),
                                   "items": [x["text"] for x in r["items"] if R.srcs(x, arm) and r["verdicts"].get(str(x["n"])) == "yes"]})
        src = set(st.get(r["id"], {}).get("source_lessons", []))
        surfaced = {x["id"] for x in hook.get(r["id"], {}).get("asked", []) + hook.get(r["id"], {}).get("free", [])}
        if src & surfaced and not R.caught(r, "B-all") and not R.caught(r, "F"):
            ex["misses"].append({"id": r["id"], "repo": c["repo"], "canon": c["canon"], "correction": cut(c["prompt"], 500, head=True),
                                 "source_lessons": [x for x in hook[r["id"]]["asked"] + hook[r["id"]]["free"] if x["id"] in src]})
    s["examples"] = ex
    calls = jl(common.COST_LOG)
    keys = ("learn", "stop", "arm:N", "arm:L", "arm:F", "judge", "pilot")
    s["cost"] = {k: {"usd": round(sum(c["cost"] or 0 for c in calls if (c.get("tag") or "").startswith(k)), 3),
                     "calls": sum(1 for c in calls if (c.get("tag") or "").startswith(k))} for k in keys}
    s["cost_total"] = round(sum(c["cost"] or 0 for c in calls), 3)
    hk = list(hook.values())
    s["stop"] = {"n": len(hk), "blocked": sum(h["blocked"] for h in hk), "skipped": sum(1 for h in hk if h["skip"]),
                 "asked": sum(len(h["asked"]) for h in hk), "free": sum(len(h["free"]) for h in hk)}
    (T / "summary.json").write_text(json.dumps(s, indent=1, ensure_ascii=False))
    print(json.dumps({k: v for k, v in s.items() if k != "examples"}, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["prep", "episodes", "learn", "stores", "check", "hook", "arm", "judge", "run", "report"])
    ap.add_argument("arm", nargs="?", default="")
    ap.add_argument("--workers", type=int, default=6)
    ap.add_argument("--n", type=int, default=0)
    ap.add_argument("--only", default="")
    a = ap.parse_args()
    {"prep": cmd_prep, "episodes": cmd_episodes, "learn": cmd_learn, "stores": cmd_stores, "check": cmd_check, "hook": cmd_hook,
     "arm": cmd_arm, "judge": cmd_judge, "run": cmd_run, "report": cmd_report}[a.cmd](a)
