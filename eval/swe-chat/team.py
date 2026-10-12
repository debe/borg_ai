#!/usr/bin/env python3
"""SWE-chat one-team test (reports/swe-chat.md, "One-team test"): one team's long history as one collective.

  python3 eval/swe-chat/team.py select     # team choice + the 200 newest sessions -> run/team/sample.csv (+ download)
  python3 eval/swe-chat/team.py prep       # cases, episodes, recurrence inputs (no model calls)
  python3 eval/swe-chat/team.py recur      # recurrence (no model calls) -> run/team/recur.json
  python3 eval/swe-chat/team.py learn      # borg learn on the one team store (thinned evenly over time, capped)
  python3 eval/swe-chat/team.py hook       # the real Stop path per case -> run/team/borg_hook.jsonl
  python3 eval/swe-chat/team.py arm N|L|F  # Sonnet arms -> run/team/arm_<X>.jsonl
  python3 eval/swe-chat/team.py judge      # blind pooled judge -> run/team/judge.jsonl
  python3 eval/swe-chat/team.py report     # -> run/team/summary.json

Paid calls: common.claude and borg's shim, both logging into run/team/claude-calls.jsonl (running total), hard cap $40
(refuses past $39.50).
"""
import argparse, collections, concurrent.futures as cf, json, math, pathlib, random, re, sys

import numpy as np
import pandas as pd

import common
from common import DATA, SEED, cut, events, human, jl, ms, context

TEAM = common.OUT / "team"
TEAM.mkdir(parents=True, exist_ok=True)
common.COST_LOG = TEAM / "claude-calls.jsonl"
common.CAP_USD = 40.0

import arms as A  # noqa: E402
import borg as B  # noqa: E402
import followup as FU  # noqa: E402  (tokens, bm25, Embedder; its module-level cost log is reset below)
import judge as J  # noqa: E402
import prep as P  # noqa: E402
import report as R  # noqa: E402

common.COST_LOG = TEAM / "claude-calls.jsonl"
common.CAP_USD = 40.0
B.OUT, B.COST_LOG, B.CAP_USD = TEAM, common.COST_LOG, common.CAP_USD
B.STORES = TEAM / "stores"
STORE = B.STORES / "entireio"
OWNER = "entireio"
N_SESS, N_LEARN, N_POS, N_NEG = 200, 150, 150, 50
LEARN_CAP_USD = 15.0
LOG_CAP = 25000
SCRATCH_SIZES = None


# ---------- select ----------

def compact_ids():
    from huggingface_hub import HfApi
    p = TEAM / "compact_ids.json"
    if not p.exists():
        ids = [f.path.split("/")[-1][:-6] for f in HfApi().list_repo_tree("SALT-NLP/SWE-chat", repo_type="dataset", path_in_repo="transcripts_compact")]
        p.write_text(json.dumps(ids))
    return set(json.loads(p.read_text()))


def cmd_select(a):
    c = compact_ids()
    s = pd.read_parquet(DATA / "sessions.parquet", columns=["session_id", "repo_id", "user_id", "agent", "created_at"])
    s = s[s.session_id.isin(c) & (s.agent == "Claude Code")].copy()
    s["owner"] = s.repo_id.str.split("/").str[0]
    by_owner = s.groupby("owner").agg(sessions=("session_id", "size"), users=("user_id", "nunique"), repos=("repo_id", "nunique"))
    by_repo = s.groupby("repo_id").agg(sessions=("session_id", "size"), users=("user_id", "nunique"))
    teams = by_owner[by_owner.users >= 2].sort_values("sessions", ascending=False)
    print("owners with >= 2 users (Claude Code, compact transcript):\n", teams.head(10))
    print("top repos:\n", by_repo.sort_values("sessions", ascending=False).head(6))
    t = s[s.owner == OWNER].sort_values(["created_at", "session_id"])
    t = t.tail(N_SESS).copy()
    t["split"] = ["learn"] * N_LEARN + ["test"] * (N_SESS - N_LEARN)
    t[["session_id", "repo_id", "user_id", "created_at", "split"]].to_csv(TEAM / "sample.csv", index=False)
    info = {"owners_ge2_users": teams.head(10).reset_index().to_dict("records"),
            "team_all": by_owner.loc[OWNER].to_dict(),
            "sample": {"span": [str(t.created_at.min()), str(t.created_at.max())],
                       "repos": t.repo_id.value_counts().to_dict(), "users": t.user_id.value_counts().to_dict(),
                       "null_user": int(t.user_id.isna().sum()),
                       "learn_users": int(t[t.split == "learn"].user_id.nunique()), "test_users": int(t[t.split == "test"].user_id.nunique()),
                       "test_users_seen_in_learn": len(set(t[t.split == "test"].user_id.dropna()) & set(t[t.split == "learn"].user_id.dropna())),
                       "learn_until": str(t[t.split == "learn"].created_at.max()), "test_from": str(t[t.split == "test"].created_at.min())}}
    (TEAM / "team.json").write_text(json.dumps(info, indent=1, default=str))
    print(json.dumps(info["sample"], indent=1, default=str))
    from huggingface_hub import hf_hub_download
    dest = DATA / "transcripts_compact"
    todo = [x for x in t.session_id if not (dest / f"{x}.jsonl").exists()]
    print("download", len(todo))
    for x in todo:
        hf_hub_download("SALT-NLP/SWE-chat", f"transcripts_compact/{x}.jsonl", repo_type="dataset", local_dir=str(DATA))
    print("done")


# ---------- prep ----------

def sample():
    return pd.read_csv(TEAM / "sample.csv")


def cmd_prep(a):
    smp = sample()
    users = dict(zip(smp.session_id, smp.user_id.fillna("")))
    labels = P.labelled(smp)
    pos, neg, eps = [], [], []
    for s in smp.itertuples():
        evs = events(s.session_id)
        m = P.match(evs, labels.get(s.session_id, []))
        push_idx = {j for j, lab, _ in m if lab in P.PUSH}
        task, tasks = "", {}
        for i, e in enumerate(evs):
            if e["kind"] == "user" and human(e["text"]):
                tasks[i] = task
                if i not in push_idx:
                    task = e["text"]
        for i, lab, turn in m:
            t = evs[i]["text"].strip()
            agent = next((evs[j]["text"] for j in range(i - 1, -1, -1) if evs[j]["kind"] == "agent"), "")
            if not agent or not P.ok_prompt(t):
                continue
            if s.split == "learn" and lab in P.PUSH:
                eps.append({"id": f"{s.session_id[-12:]}-{turn}", "ms": ms(evs[i]["ts"]), "repo": s.repo_id.split("/")[-1],
                            "session": s.session_id, "task": cut(tasks.get(i, ""), 1500, head=True), "agent": agent[-2000:],
                            "rejected_tool": None, "correction": cut(t, 1500, head=True), "engineer": users.get(s.session_id, "")})
            if s.split == "test" and (lab in P.PUSH or lab == "non_pushback"):
                ctx = context(evs, i)
                if not ctx:
                    continue
                row = {"session": s.session_id, "repo": s.repo_id, "user": users.get(s.session_id, ""), "turn": int(turn), "label": lab,
                       "ts": evs[i]["ts"], "prompt": t, "context": ctx, "idx": i}
                (pos if lab in P.PUSH else neg).append(row)
    rng = random.Random(SEED)
    pos.sort(key=lambda x: (x["session"], x["turn"]))
    neg.sort(key=lambda x: (x["session"], x["turn"]))
    rng.shuffle(pos)
    rng.shuffle(neg)
    chosen, negs = sorted(pos[:N_POS], key=lambda x: x["ts"]), sorted(neg[:N_NEG], key=lambda x: x["ts"])
    with open(TEAM / "cases.jsonl", "w") as f:
        for n, c in enumerate(chosen):
            f.write(json.dumps({"id": f"p{n:03d}", "kind": "pos", **c}) + "\n")
        for n, c in enumerate(negs):
            f.write(json.dumps({"id": f"n{n:03d}", "kind": "neg", **c}) + "\n")
    eps.sort(key=lambda e: e["ms"])
    with open(TEAM / "episodes_all.jsonl", "w") as f:
        for e in eps:
            f.write(json.dumps(e) + "\n")
    st = {"positives_available": len(pos), "positives": len(chosen), "negatives_available": len(neg), "negatives": len(negs),
          "test_sessions_with_cases": len({c["session"] for c in chosen + negs}), "episodes": len(eps),
          "episode_engineers": len({e["engineer"] for e in eps}), "labels": collections.Counter(c["label"] for c in chosen)}
    (TEAM / "prep.json").write_text(json.dumps(st, indent=1))
    print(st)


# ---------- recurrence ----------

def other_corrections():
    """Control pool: correction/rejection prompts (prep filters) of every non-team session in the earlier 159-session sample."""
    smp = pd.read_csv(DATA / "sample.csv")
    smp = smp[~smp.repo_id.str.startswith(OWNER + "/")]
    labels = P.labelled(smp)
    out = []
    for s in smp.itertuples():
        evs = events(s.session_id)
        for i, lab, turn in P.match(evs, labels.get(s.session_id, [])):
            t = evs[i]["text"].strip()
            agent = next((evs[j]["text"] for j in range(i - 1, -1, -1) if evs[j]["kind"] == "agent"), "")
            if lab in P.PUSH and agent and P.ok_prompt(t):
                out.append({"id": f"{s.session_id[-12:]}-{turn}", "repo": s.repo_id, "ms": ms(evs[i]["ts"]), "text": cut(t, 1500, head=True)})
    return out


def cmd_recur(a):
    cases = [c for c in jl(TEAM / "cases.jsonl") if c["kind"] == "pos"]
    learn = [{"id": e["id"], "repo": e["repo"], "ms": e["ms"], "user": e["engineer"], "session": e["session"], "text": e["correction"]}
             for e in jl(TEAM / "episodes_all.jsonl")]
    other = other_corrections()
    texts = [x["text"] for x in learn] + [x["text"] for x in other] + [cut(c["prompt"], 1500, head=True) for c in cases]
    toks = [FU.tokens(t) for t in texts]
    df = collections.Counter(t for d in toks for t in set(d))
    Nd = len(toks)
    vocab = {t: i for i, t in enumerate(df)}
    idf = np.array([math.log(Nd / df[t]) + 1 for t in vocab])

    def tfidf(d):
        v = np.zeros(len(vocab))
        for t, f in collections.Counter(d).items():
            v[vocab[t]] = (1 + math.log(f)) * idf[vocab[t]]
        return v / max(np.linalg.norm(v), 1e-12)

    nl, no = len(learn), len(other)
    T = np.array([tfidf(d) for d in toks])
    emb = FU.Embedder()
    V = np.array([emb(t) for t in texts])
    sims = {"tfidf": (T[nl + no:] @ T[:nl].T, T[nl + no:] @ T[nl:nl + no].T),
            "vector": (V[nl + no:] @ V[:nl].T, V[nl + no:] @ V[nl:nl + no].T)}
    rng = random.Random(SEED)
    res = {"n_team_corrections": nl, "n_control_corrections": no, "control_repos": len({x["repo"] for x in other}), "n_cases": len(cases),
           "views": {}}
    per_case = collections.defaultdict(dict)
    for metric, (MS, MO) in sims.items():
        best, ctrl = [], []
        for ci, c in enumerate(cases):
            Tc = ms(c["ts"])
            same = [j for j, x in enumerate(learn) if x["ms"] < Tc]
            if not same:
                best.append(None)
                continue
            j = max(same, key=lambda j: MS[ci, j])
            k = min(len(same), no)
            # size-matched control: best over k random control corrections (all control corrections are from other repos;
            # their dates are earlier or interleaved, so no date filter is applied to them)
            row = MO[ci]
            for _ in range(200):
                idx = rng.sample(range(no), k)
                ctrl.append(float(row[idx].max()))
            best.append((float(MS[ci, j]), j))
            same_user = [jj for jj in same if learn[jj]["user"] and learn[jj]["user"] == c["user"]]
            other_user = [jj for jj in same if jj not in set(same_user)]
            per_case[c["id"]][metric] = {"best": round(float(MS[ci, j]), 3), "earlier_id": learn[j]["id"], "earlier_user_same": learn[j]["user"] == c["user"] and bool(c["user"]),
                                         "best_same_user": round(float(max((MS[ci, x] for x in same_user), default=0)), 3),
                                         "best_other_user": round(float(max((MS[ci, x] for x in other_user), default=0)), 3), "k": len(same)}
        thr = float(np.percentile(ctrl, 95))
        above = [(b[0], ci, b[1]) for ci, b in enumerate(best) if b and b[0] > thr]
        k = len(above)
        res["views"][metric] = {
            "threshold_p95_size_matched": round(thr, 3), "k_capped_at_control_size": no,
            "median_best_same": round(float(np.median([b[0] for b in best if b])), 3), "median_ctrl": round(float(np.median(ctrl)), 3),
            "above": {"k": k, "n": len(cases), "pct": round(100 * k / len(cases), 1), "ci": R.wilson(k, len(cases))},
            "above_by_other_user": sum(1 for s, ci, j in above if learn[j]["user"] != cases[ci]["user"]),
            "list": [{"case": cases[ci]["id"], "sim": round(s, 3), "case_user": cases[ci]["user"], "earlier_user": learn[j]["user"],
                      "case_repo": cases[ci]["repo"], "earlier_repo": learn[j]["repo"], "case_text": cut(cases[ci]["prompt"], 400, head=True),
                      "earlier": cut(learn[j]["text"], 400, head=True), "earlier_id": learn[j]["id"]} for s, ci, j in sorted(above, reverse=True)],
        }
        for c in cases:
            if c["id"] in per_case and metric in per_case[c["id"]]:
                per_case[c["id"]][metric]["above"] = per_case[c["id"]][metric]["best"] > thr
    (TEAM / "recur.json").write_text(json.dumps(res, indent=1, ensure_ascii=False))
    with open(TEAM / "recur_cases.jsonl", "w") as f:
        for cid, v in per_case.items():
            f.write(json.dumps({"case": cid, **v}) + "\n")
    for m, v in res["views"].items():
        print(m, {x: v[x] for x in v if x != "list"})
    print({k: v for k, v in res.items() if k != "views"})


# ---------- learn ----------

def cmd_learn(a):
    eps = jl(TEAM / "episodes_all.jsonl")
    n = min(len(eps), a.n)
    # evenly over time: n episodes at evenly spaced ranks of the time order
    pick = sorted({round(i * (len(eps) - 1) / max(1, n - 1)) for i in range(n)}) if n < len(eps) else range(len(eps))
    chosen = [eps[i] for i in pick]
    STORE.mkdir(parents=True, exist_ok=True)
    have = {e["id"] for e in jl(STORE / "episodes.jsonl")}
    with open(STORE / "episodes.jsonl", "a") as f:
        for e in chosen:
            if e["id"] not in have:
                f.write(json.dumps(e) + "\n")
    print("episodes in store", len(have | {e["id"] for e in chosen}), "of", len(eps), flush=True)
    import shutil, subprocess, tempfile, time
    tmp = pathlib.Path(tempfile.mkdtemp(prefix="swe-team-learn-"))
    try:
        t0 = time.monotonic()
        r = subprocess.run([str(common.BORG), "learn"], capture_output=True, text=True, env=B.env_for(tmp, STORE, "learn:entireio"),
                           timeout=8 * 3600)
        rec = {"rc": r.returncode, "stdout": r.stdout[-500:], "stderr_tail": r.stderr[-1500:], "wall_s": round(time.monotonic() - t0),
               "episodes": len(chosen), "lessons": len({l["id"] for l in jl(STORE / "lessons.jsonl")})}
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    with open(TEAM / "borg_learn.jsonl", "a") as f:
        f.write(json.dumps(rec) + "\n")
    print(rec)


# ---------- hook ----------

def cmd_hook(a):
    path = TEAM / "borg_hook.jsonl"
    have = {r["id"] for r in jl(path) if not r.get("error")}
    todo = [c for c in jl(TEAM / "cases.jsonl") if c["id"] not in have and (not a.kind or c["kind"] == a.kind)
            and (not a.only or c["id"] in a.only.split(","))]
    with cf.ThreadPoolExecutor(a.workers) as ex, open(path, "a") as f:
        for r in ex.map(lambda c: B.run_hook(c, STORE), todo):
            f.write(json.dumps(r) + "\n")
            f.flush()
            print(r["id"], "err " + str(r.get("error")) if r.get("error") else f"blocked={r['blocked']} asked={len(r['asked'])} free={len(r['free'])} "
                  f"skip={r['skip']} cost={r['predict_cost']:.3f} lessons={r['lessons_before']}", f"total ${common.spent():.2f}", flush=True)


# ---------- arms ----------

def plain_log(T):
    """Team plain log: learn-session corrections dated before T, newest that fit LOG_CAP chars, printed oldest first."""
    lines = []
    for e in reversed(jl(TEAM / "episodes_all.jsonl")):
        if e["ms"] >= T:
            continue
        l = f"- [{pd.Timestamp(e['ms'], unit='ms'):%Y-%m-%d}] ({e['repo']}) {cut(re.sub(chr(10) + '+', ' / ', e['correction']), P.LOG_ITEM, head=True)}"
        if sum(len(x) + 1 for x in lines) + len(l) + 1 > LOG_CAP:
            break
        lines.append(l)
    return "\n".join(reversed(lines))


def arm_prompt(arm, c, hook):
    turn = f"## The agent's turn (transcript since the user's previous message; may be cut)\n\n{c['context']}"
    if arm == "N":
        return turn
    if arm == "L":
        return f"## Earlier corrections by users of this repository (oldest first)\n\n{plain_log(ms(c['ts'])) or '(none)'}\n\n{turn}"
    if arm == "F":
        h = hook.get(c["id"]) or {}
        ls = [l for l in (h.get("asked") or []) + (h.get("free") or []) if l.get("question")]
        return FU.f_prompt(c, ls), [l["id"] for l in ls]
    raise ValueError(arm)


def cmd_arm(a):
    path = TEAM / f"arm_{a.arm}.jsonl"
    have = {r["id"] for r in jl(path) if not r.get("error")}
    hook = {r["id"]: r for r in jl(TEAM / "borg_hook.jsonl") if not r.get("error")}
    todo = [c for c in jl(TEAM / "cases.jsonl") if c["id"] not in have and (not a.kind or c["kind"] == a.kind)]
    if a.arm == "F":
        todo = [c for c in todo if c["id"] in hook]
    todo.sort(key=lambda c: c["ts"])  # time order: the L prefix changes slowly (prompt-cache reuse)

    def job(c):
        p = arm_prompt(a.arm, c, hook)
        p, lids = p if isinstance(p, tuple) else (p, None)
        try:
            r = common.claude(p, f"arm:{a.arm}:{c['id']}", schema=A.SCHEMA, system=A.SYSTEM)
        except RuntimeError as e:
            return {"id": c["id"], "error": str(e)}
        out = r["out"] or {}
        return {"id": c["id"], "concerns": [x for x in (out.get("concerns") or []) if isinstance(x, str)][:5], "lessons": lids,
                "cost": r["cost"], "error": r["error"]}

    with cf.ThreadPoolExecutor(a.workers) as ex, open(path, "a") as f:
        for r in ex.map(job, todo):
            f.write(json.dumps(r) + "\n")
            f.flush()
            print(r["id"], "ERR " + str(r["error"]) if r.get("error") else len(r["concerns"]), f"total ${common.spent():.2f}", flush=True)


# ---------- judge ----------

def cmd_judge(a):
    arms = {x: {r["id"]: r for r in jl(TEAM / f"arm_{x}.jsonl") if not r.get("error")} for x in ("N", "L", "F")}
    arms["B"] = {r["id"]: r for r in jl(TEAM / "borg_hook.jsonl") if not r.get("error")}
    path = TEAM / "judge.jsonl"
    have = {r["id"] for r in jl(path) if not r.get("error")}
    todo = [c for c in jl(TEAM / "cases.jsonl") if c["id"] not in have and (not a.kind or c["kind"] == a.kind)
            and (not a.only or c["id"] in a.only.split(","))]

    def job(c):
        items = J.pooled(c, arms)
        present = sorted(x for x in arms if c["id"] in arms[x])
        if not items:
            return {"id": c["id"], "kind": c["kind"], "items": [], "verdicts": {}, "genuine": None, "arms_present": present, "cost": 0, "error": None}
        try:
            r = common.claude(J.prompt(c, items), f"judge:{c['id']}", schema=J.POS_SCHEMA if c["kind"] == "pos" else J.NEG_SCHEMA, system=J.SYSTEM)
        except RuntimeError as e:
            return {"id": c["id"], "error": str(e)}
        out = r["out"] or {}
        v = {int(x["n"]): (x.get("verdict") if c["kind"] == "pos" else x.get("false_alarm")) for x in out.get("items", [])}
        missing = [x["n"] for x in items if x["n"] not in v]
        return {"id": c["id"], "kind": c["kind"], "items": items, "verdicts": v, "genuine": out.get("genuine"), "arms_present": present,
                "cost": r["cost"], "error": r["error"] or (f"missing verdicts {missing}" if missing else None)}

    with cf.ThreadPoolExecutor(a.workers) as ex, open(path, "a") as f:
        for r in ex.map(job, todo):
            f.write(json.dumps(r) + "\n")
            f.flush()
            print(r["id"], "ERR " + str(r["error"]) if r.get("error") else f"{len(r['items'])} items", f"total ${common.spent():.2f}", flush=True)


# ---------- report ----------

def cmd_report(a):
    cases = {c["id"]: c for c in jl(TEAM / "cases.jsonl")}
    rec = {r["case"]: r for r in jl(TEAM / "recur_cases.jsonl")}
    repeats = set(json.loads((TEAM / "repeats.json").read_text())) if (TEAM / "repeats.json").exists() else set()
    rows = []
    for r in jl(TEAM / "judge.jsonl"):
        if r.get("error"):
            continue
        r = dict(r, repo=cases[r["id"]]["session"])  # cluster = test session
        r["verdicts"] = {str(k): v for k, v in r["verdicts"].items()}
        rows.append(r)
    pos = [r for r in rows if r["kind"] == "pos"]
    neg = [r for r in rows if r["kind"] == "neg"]
    above = {cid for cid, v in rec.items() if any(v.get(m, {}).get("above") for m in ("tfidf", "vector"))}
    subsets = {"all": pos, "genuine": [r for r in pos if r.get("genuine")], "above_threshold": [r for r in pos if r["id"] in above],
               "repeats_read": [r for r in pos if r["id"] in repeats]}
    bf = {r["id"] for r in pos if "F" in r["arms_present"] and "B" in r["arms_present"]}
    subsets["bf_subset"] = [r for r in pos if r["id"] in bf]
    subsets["bf_subset_non_repeats"] = [r for r in pos if r["id"] in bf and r["id"] not in repeats]
    subsets["bf_subset_genuine"] = [r for r in pos if r["id"] in bf and r.get("genuine")]
    f_strict = lambda r, x: 1 if R.caught(r, x) else 0
    f_len = lambda r, x: 1 if R.caught(r, x, True) else 0
    ARMS = ["N", "L", "F", "B-block", "B-all"]
    s = {"catch": {}, "pairs": {}, "false_alarms": {}}
    for name, P_ in subsets.items():
        s["catch"][name] = {}
        for arm in ARMS:
            X = [r for r in P_ if R.arm_present(r, arm)]
            k, kl = sum(f_strict(r, arm) for r in X), sum(f_len(r, arm) for r in X)
            s["catch"][name][arm] = {"n": len(X), "k": k, "pct": round(100 * k / max(1, len(X)), 1), "ci": R.wilson(k, len(X)),
                                     "lenient_k": kl, "lenient_pct": round(100 * kl / max(1, len(X)), 1),
                                     "items": round(sum(sum(1 for x in r["items"] if R.srcs(x, arm)) for r in X) / max(1, len(X)), 2)}
        s["pairs"][name] = {f"{x} - {y}": {"strict": R.boot(P_, x, y, f_strict), "lenient": R.boot(P_, x, y, f_len)}
                            for x, y in [("F", "L"), ("F", "N"), ("L", "N"), ("B-all", "F"), ("B-all", "L"), ("B-block", "F")]}
    for arm in ARMS:
        Nn = [r for r in neg if R.arm_present(r, arm)]
        fa = [sum(1 for x in r["items"] if R.srcs(x, arm) and r["verdicts"].get(str(x["n"])) is True) for r in Nn]
        its = [sum(1 for x in r["items"] if R.srcs(x, arm)) for r in Nn]
        s["false_alarms"][arm] = {"n": len(Nn), "items": round(sum(its) / max(1, len(its)), 2), "fa": round(sum(fa) / max(1, len(fa)), 2),
                                  "any_pct": round(100 * sum(1 for x in fa if x) / max(1, len(fa)), 1),
                                  "any_ci": R.wilson(sum(1 for x in fa if x), len(fa))}
    hook = [r for r in jl(TEAM / "borg_hook.jsonl") if not r.get("error")]
    s["stop"] = {"n": len(hook), "blocked": sum(r["blocked"] for r in hook), "skipped": sum(1 for r in hook if r["skip"]),
                 "asked": sum(len(r["asked"]) for r in hook), "free": sum(len(r["free"]) for r in hook),
                 "none_surfaced": sum(1 for r in hook if not r["asked"] and not r["free"]),
                 "stop_wall_p50": float(np.median([r["stop_wall_s"] for r in hook])) if hook else None}
    calls = jl(common.COST_LOG)
    s["cost"] = {k: {"usd": round(sum(c["cost"] or 0 for c in calls if (c.get("tag") or "").startswith(k)), 3),
                     "calls": sum(1 for c in calls if (c.get("tag") or "").startswith(k))}
                 for k in ("learn", "stop", "arm:N", "arm:L", "arm:F", "judge")}
    s["cost_total"] = round(sum(c["cost"] or 0 for c in calls), 3)
    (TEAM / "summary.json").write_text(json.dumps(s, indent=1, ensure_ascii=False))
    print(json.dumps(s, indent=1))


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["select", "prep", "recur", "learn", "hook", "arm", "judge", "report"])
    ap.add_argument("arm", nargs="?", default="")
    ap.add_argument("--workers", type=int, default=6)
    ap.add_argument("--kind", default="")
    ap.add_argument("--n", type=int, default=10 ** 6)
    ap.add_argument("--only", default="")
    a = ap.parse_args()
    {"select": cmd_select, "prep": cmd_prep, "recur": cmd_recur, "learn": cmd_learn, "hook": cmd_hook, "arm": cmd_arm,
     "judge": cmd_judge, "report": cmd_report}[a.cmd](a)
