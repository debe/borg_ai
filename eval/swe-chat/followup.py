#!/usr/bin/env python3
"""SWE-chat follow-up (reports/swe-chat.md, "Follow-up"): recurrence of corrections (no model calls) and arm F
(Sonnet given the lessons Borg's Stop selected).

  python3 eval/swe-chat/followup.py recur              # -> run/followup/recur.json
  python3 eval/swe-chat/followup.py arm [--workers 6]  # arm F -> run/followup/arm_F.jsonl
  python3 eval/swe-chat/followup.py judge [--calib]    # blind judge of F (and N alone on 40 positives with --calib)
  python3 eval/swe-chat/followup.py report             # -> run/followup/summary.json

Paid calls go through common.claude with this follow-up's own cost log (cap $8, refuses past $7.50).
"""
import argparse, collections, concurrent.futures as cf, json, math, random, sys

import numpy as np
import pandas as pd

import arms as A
import common
import judge as J
import report as R
from common import DATA, OUT, SEED, cut, events, jl, ms

FU = OUT / "followup"
FU.mkdir(parents=True, exist_ok=True)
common.COST_LOG = FU / "claude-calls.jsonl"
common.CAP_USD = 8.0


# ---------- Borg's tokeniser and BM25 (crates/borg/src/conscience.rs `tokens`, `bm25`), ported ----------

def tokens(text):
    out = []
    raw_parts = []
    cur = []
    for ch in text:
        if ch.isalnum() or ch == "_":
            cur.append(ch)
        else:
            raw_parts.append("".join(cur)); cur = []
    raw_parts.append("".join(cur))
    for raw in raw_parts:
        if len(raw.encode()) < 3:
            continue
        out.append(raw.lower())
        word = ""
        for ch in raw:
            if ch.isupper() and word:
                if len(word.encode()) >= 3 and len(word.encode()) < len(raw.encode()):
                    out.append(word.lower())
                word = ""
            word += ch
        if len(word.encode()) >= 3 and len(word.encode()) < len(raw.encode()):
            out.append(word.lower())
    return out


def bm25(docs, query):
    k1, b = 1.2, 0.75
    n = max(1, len(docs))
    avg = sum(map(len, docs)) / n
    df = collections.Counter(t for d in docs for t in set(d))
    q = set(tokens(query))
    out = []
    for d in docs:
        tf = collections.Counter(d)
        s = 0.0
        for t in q:
            if t in tf:
                f = tf[t]
                idf = math.log(1 + (n - df[t] + 0.5) / (df[t] + 0.5))
                s += idf * f * (k1 + 1) / (f + k1 * (1 - b + b * len(d) / avg))
        out.append(s)
    return out


# ---------- 1. recurrence ----------

def learn_corrections():
    """Every learn-session correction/rejection prompt passing prep's filters (prep.py, before its 40-newest cap)."""
    import prep
    sample = pd.read_csv(DATA / "sample.csv")
    labels = prep.labelled(sample)
    out = []
    for s in sample.itertuples():
        if s.split != "learn":
            continue
        evs = events(s.session_id)
        for i, lab, turn in prep.match(evs, labels.get(s.session_id, [])):
            t = evs[i]["text"].strip()
            agent = next((evs[j]["text"] for j in range(i - 1, -1, -1) if evs[j]["kind"] == "agent"), "")
            if lab in prep.PUSH and agent and prep.ok_prompt(t):
                out.append({"id": f"{s.session_id[:8]}-{turn}", "repo": s.repo_id, "ms": ms(evs[i]["ts"]),
                            "text": cut(t, 1500, head=True)})
    return out


class Embedder:
    """Borg's Model2Vec embed (vectors.rs `embed`): mean of the token rows (no special tokens, unk skipped), normalised."""

    def __init__(self):
        from safetensors.numpy import load_file
        from tokenizers import Tokenizer
        d = common.HOME / ".local/share/borg/models/potion-multilingual-128M"
        self.tok = Tokenizer.from_file(str(d / "tokenizer.json"))
        self.E = load_file(str(d / "model.safetensors"))["embeddings"]
        self.unk = self.tok.token_to_id("[UNK]") if self.tok.token_to_id("[UNK]") is not None else self.tok.token_to_id("<unk>")

    def __call__(self, text):
        ids = [i for i in self.tok.encode(text, add_special_tokens=False).ids if i != self.unk and i < len(self.E)]
        v = self.E[ids].astype(np.float64).mean(0) if ids else np.zeros(self.E.shape[1])
        return v / max(np.linalg.norm(v), 1e-12)


def cmd_recur(a):
    cases = [c for c in jl(OUT / "cases.jsonl") if c["kind"] == "pos"]
    genuine = {r["id"] for r in jl(OUT / "judge.jsonl") if r.get("genuine")}
    learn = learn_corrections()
    newest40 = set()
    for f in (OUT / "episodes").glob("*.jsonl"):
        newest40 |= {e["id"] for e in jl(f)}
    texts = [x["text"] for x in learn] + [cut(c["prompt"], 1500, head=True) for c in cases]
    toks = [tokens(t) for t in texts]
    df = collections.Counter(t for d in toks for t in set(d))
    N = len(toks)
    vocab = {t: i for i, t in enumerate(df)}
    idf = np.array([math.log(N / df[t]) + 1 for t in vocab])

    def tfidf(d):
        v = np.zeros(len(vocab))
        for t, f in collections.Counter(d).items():
            v[vocab[t]] = (1 + math.log(f)) * idf[vocab[t]]
        return v / max(np.linalg.norm(v), 1e-12)

    TL = np.array([tfidf(d) for d in toks[:len(learn)]])
    TC = np.array([tfidf(d) for d in toks[len(learn):]])
    emb = Embedder()
    VL = np.array([emb(x["text"]) for x in learn])
    VC = np.array([emb(cut(c["prompt"], 1500, head=True)) for c in cases])
    S = {"tfidf": TC @ TL.T, "vector": VC @ VL.T}
    rng = random.Random(SEED)
    res = {"n_learn_corrections": len(learn), "n_cases": len(cases), "n_genuine": len(genuine & {c["id"] for c in cases}),
           "views": {}}
    per_case = []
    for view in ("all", "newest40"):
        for metric, M in S.items():
            best_same, ctrl, ctrl_all = [], [], []
            for ci, c in enumerate(cases):
                T = ms(c["ts"])
                same = [j for j, x in enumerate(learn) if x["repo"] == c["repo"] and x["ms"] < T and (view == "all" or x["id"] in newest40)]
                other = [j for j, x in enumerate(learn) if x["repo"] != c["repo"] and x["ms"] < T and (view == "all" or x["id"] in newest40)]
                if not same:
                    best_same.append((ci, None, None))
                    continue
                j = max(same, key=lambda j: M[ci, j])
                best_same.append((ci, M[ci, j], j))
                k = len(same)
                for _ in range(200):
                    ctrl.append(max(M[ci, jj] for jj in rng.sample(other, min(k, len(other)))))
                ctrl_all.append(max(M[ci, jj] for jj in other))
            thr = float(np.percentile(ctrl, 95))
            thr_all = float(np.percentile(ctrl_all, 95))

            def share(ids, t):
                xs = [(ci, s) for ci, s, _ in best_same if cases[ci]["id"] in ids]
                k = sum(1 for _, s in xs if s is not None and s > t)
                return {"k": k, "n": len(xs), "pct": round(100 * k / max(1, len(xs)), 1), "ci": R.wilson(k, len(xs))}

            allids = {c["id"] for c in cases}
            gids = allids & genuine
            ex = sorted([(s, ci, j) for ci, s, j in best_same if s is not None and s > thr], reverse=True)
            res["views"][f"{view}/{metric}"] = {
                "threshold_p95_size_matched": round(thr, 3), "threshold_p95_all_other": round(thr_all, 3),
                "cases_without_earlier": sum(1 for _, s, _ in best_same if s is None),
                "median_best_same": round(float(np.median([s for _, s, _ in best_same if s is not None])), 3),
                "median_ctrl": round(float(np.median(ctrl)), 3),
                "all": share(allids, thr), "genuine": share(gids, thr),
                "all_vs_all_other": share(allids, thr_all), "genuine_vs_all_other": share(gids, thr_all),
                "above": [{"case": cases[ci]["id"], "repo": cases[ci]["repo"], "sim": round(float(s), 3),
                           "genuine": cases[ci]["id"] in genuine, "case_text": cut(cases[ci]["prompt"], 300, head=True),
                           "earlier": cut(learn[j]["text"], 300, head=True), "earlier_id": learn[j]["id"]} for s, ci, j in ex],
            }
            if view == "all":
                for ci, s, j in best_same:
                    per_case.append({"case": cases[ci]["id"], "metric": metric, "best": None if s is None else round(float(s), 3),
                                     "earlier_id": None if j is None else learn[j]["id"]})
    (FU / "recur.json").write_text(json.dumps(res, indent=1, ensure_ascii=False))
    with open(FU / "recur_cases.jsonl", "w") as f:
        for r in per_case:
            f.write(json.dumps(r) + "\n")
    for k, v in res["views"].items():
        print(k, {x: v[x] for x in v if x != "above"}, "above:", len(v["above"]))


# ---------- 2. arm F ----------

def store_lessons(repo):
    from borg import lessons_by_id
    return lessons_by_id(OUT / "stores" / repo.replace("/", "__"))


def f_lessons(c, hook):
    """(source, lessons): Borg's surfaced lessons for the case, else BM25 top 15 of the store dated before it."""
    h = hook.get(c["id"]) or {}
    got = [l for l in (h.get("asked") or []) + (h.get("free") or []) if l.get("question")]
    if got:
        return "borg", got
    T = ms(c["ts"])
    ls = [l for l in store_lessons(c["repo"]).values() if l["origin_ms"] < T]
    ls.sort(key=lambda l: l["id"])
    sc = bm25([tokens(l["question"]) for l in ls], c["context"])
    order = sorted(range(len(ls)), key=lambda i: -sc[i])[:15]
    return "bm25", [{"id": ls[i]["id"], "question": ls[i]["question"], "consequence": ls[i]["consequence"]} for i in order]


def f_prompt(c, lessons):
    block = "\n".join(f"- {l['question']} → {l['consequence']}" for l in lessons) or "(none)"
    return ("## Lessons a memory of this repository's earlier corrections selected for this turn (question → consequence)\n\n"
            f"{block}\n\n## The agent's turn (transcript since the user's previous message; may be cut)\n\n{c['context']}")


def cmd_arm(a):
    hook = {r["id"]: r for r in jl(OUT / "borg_hook.jsonl") if not r.get("error")}
    path = FU / "arm_F.jsonl"
    have = {r["id"] for r in jl(path) if not r.get("error")}
    todo = [c for c in jl(OUT / "cases.jsonl") if c["id"] not in have and (not a.kind or c["kind"] == a.kind)][: a.limit or None]
    todo.sort(key=lambda c: (c["kind"] != "pos", c["repo"], c["id"]))

    def job(c):
        src, ls = f_lessons(c, hook)
        try:
            r = common.claude(f_prompt(c, ls), f"arm:F:{c['id']}", schema=A.SCHEMA, system=A.SYSTEM)
        except RuntimeError as e:
            return {"id": c["id"], "error": str(e)}
        out = r["out"] or {}
        return {"id": c["id"], "source": src, "lessons": [l["id"] for l in ls],
                "concerns": [x for x in (out.get("concerns") or []) if isinstance(x, str)][:5], "cost": r["cost"], "error": r["error"]}

    with cf.ThreadPoolExecutor(a.workers) as ex, open(path, "a") as f:
        for r in ex.map(job, todo):
            f.write(json.dumps(r) + "\n")
            f.flush()
            print(r["id"], "ERR " + str(r["error"]) if r.get("error") else f"{r['source']} {len(r['lessons'])} -> {len(r['concerns'])}",
                  f"total ${common.spent():.2f}", flush=True)


def cmd_judge(a):
    cases = {c["id"]: c for c in jl(OUT / "cases.jsonl")}
    if a.calib:
        src = {r["id"]: r for r in jl(OUT / "arm_N.jsonl") if not r.get("error")}
        ids = sorted(i for i in src if i.startswith("p"))
        ids = random.Random(SEED).sample(ids, 40)
        path, arm = FU / "judge_N_alone.jsonl", "N"
    else:
        src = {r["id"]: r for r in jl(FU / "arm_F.jsonl") if not r.get("error")}
        ids, path, arm = sorted(src), FU / "judge_F.jsonl", "F"
    have = {r["id"] for r in jl(path) if not r.get("error")}
    todo = [cases[i] for i in ids if i not in have]

    def job(c):
        items = J.pooled(c, {arm: src})
        if not items:
            return {"id": c["id"], "kind": c["kind"], "items": [], "verdicts": {}, "genuine": None, "cost": 0, "error": None}
        try:
            r = common.claude(J.prompt(c, items), f"judge{arm}:{c['id']}", schema=J.POS_SCHEMA if c["kind"] == "pos" else J.NEG_SCHEMA,
                              system=J.SYSTEM)
        except RuntimeError as e:
            return {"id": c["id"], "error": str(e)}
        out = r["out"] or {}
        v = {int(x["n"]): (x.get("verdict") if c["kind"] == "pos" else x.get("false_alarm")) for x in out.get("items", [])}
        missing = [x["n"] for x in items if x["n"] not in v]
        return {"id": c["id"], "kind": c["kind"], "items": items, "verdicts": v, "genuine": out.get("genuine"), "cost": r["cost"],
                "error": r["error"] or (f"missing verdicts {missing}" if missing else None)}

    with cf.ThreadPoolExecutor(a.workers) as ex, open(path, "a") as f:
        for r in ex.map(job, todo):
            f.write(json.dumps(r) + "\n")
            f.flush()
            print(r["id"], "ERR " + str(r["error"]) if r.get("error") else f"{len(r['items'])} items", f"total ${common.spent():.2f}", flush=True)


# ---------- report ----------

def merged(extra_path, arm):
    """Original judge rows with the separately judged items of `arm` appended (n offset 1000)."""
    cases = {c["id"]: c for c in jl(OUT / "cases.jsonl")}
    extra = {r["id"]: r for r in jl(extra_path) if not r.get("error")}
    rows = []
    for r in jl(OUT / "judge.jsonl"):
        if r.get("error"):
            continue
        r = dict(r, repo=cases[r["id"]]["repo"])
        r["verdicts"] = {str(k): v for k, v in r["verdicts"].items()}
        if r["id"] in extra:
            e = extra[r["id"]]
            r["items"] = r["items"] + [{"n": 1000 + x["n"], "text": x["text"], "arms": [f"{arm}*"]} for x in e["items"]]
            r["verdicts"].update({str(1000 + int(k)): v for k, v in e["verdicts"].items()})
            r["arms_present"] = r["arms_present"] + [f"{arm}*"]
        rows.append(r)
    return rows


def cmd_report(a):
    rows = merged(FU / "judge_F.jsonl", "F")
    src = {r["id"]: r["source"] for r in jl(FU / "arm_F.jsonl") if not r.get("error")}
    pos = [r for r in rows if r["kind"] == "pos"]
    neg = [r for r in rows if r["kind"] == "neg"]
    gen = [r for r in pos if r.get("genuine")]
    s = {"catch": {}, "pairs": {}, "pairs_genuine": {}, "pairs_lenient": {}, "false_alarms": {}, "by_source": {}}
    f_strict = lambda r, x: 1 if R.caught(r, x) else 0
    f_len = lambda r, x: 1 if R.caught(r, x, True) else 0
    for arm in ["F*", "N", "L", "H5", "H", "B-block", "B-all"]:
        P = [r for r in pos if R.arm_present(r, arm)]
        G = [r for r in gen if R.arm_present(r, arm)]
        k, kl, kg = sum(f_strict(r, arm) for r in P), sum(f_len(r, arm) for r in P), sum(f_strict(r, arm) for r in G)
        Nn = [r for r in neg if R.arm_present(r, arm)]
        fa = [sum(1 for x in r["items"] if R.srcs(x, arm) and r["verdicts"].get(str(x["n"])) is True) for r in Nn]
        its = [sum(1 for x in r["items"] if R.srcs(x, arm)) for r in Nn]
        s["catch"][arm] = {"n": len(P), "strict": k, "pct": round(100 * k / max(1, len(P)), 1), "ci": R.wilson(k, len(P)),
                           "lenient_pct": round(100 * kl / max(1, len(P)), 1), "genuine": f"{kg}/{len(G)}",
                           "genuine_pct": round(100 * kg / max(1, len(G)), 1),
                           "items_per_case": round(sum(sum(1 for x in r["items"] if R.srcs(x, arm)) for r in P) / max(1, len(P)), 2)}
        s["false_alarms"][arm] = {"n": len(Nn), "items": round(sum(its) / max(1, len(its)), 2), "fa": round(sum(fa) / max(1, len(fa)), 2),
                                  "any_pct": round(100 * sum(1 for x in fa if x) / max(1, len(fa)), 1)}
    for b in ["N", "L", "H5", "H", "B-all", "B-block"]:
        s["pairs"][f"F - {b}"] = R.boot(pos, "F*", b, f_strict)
        s["pairs_genuine"][f"F - {b}"] = R.boot(gen, "F*", b, f_strict)
        s["pairs_lenient"][f"F - {b}"] = R.boot(pos, "F*", b, f_len)
    for which in ("borg", "bm25"):
        P = [r for r in pos if src.get(r["id"]) == which]
        s["by_source"][which] = {"n": len(P), **{arm: sum(f_strict(r, arm) for r in P) for arm in ["F*", "N", "L", "B-all"]}}
        s["pairs"][f"F - B-all [{which}]"] = R.boot(P, "F*", "B-all", f_strict)
        s["pairs"][f"F - L [{which}]"] = R.boot(P, "F*", "L", f_strict)
    if (FU / "judge_N_alone.jsonl").exists():
        cal = merged(FU / "judge_N_alone.jsonl", "N")
        C = [r for r in cal if "N*" in r["arms_present"]]
        s["calibration_N_alone_vs_pooled"] = {"n": len(C), "pooled": sum(f_strict(r, "N") for r in C),
                                             "alone": sum(f_strict(r, "N*") for r in C),
                                             "pooled_lenient": sum(f_len(r, "N") for r in C), "alone_lenient": sum(f_len(r, "N*") for r in C)}
    ex = []
    for r in pos:
        if R.caught(r, "F*") and not R.caught(r, "B-all"):
            ex.append({"id": r["id"], "repo": r["repo"], "source": src.get(r["id"]), "also_N": R.caught(r, "N"), "also_L": R.caught(r, "L"),
                       "hit": [x["text"] for x in r["items"] if R.srcs(x, "F*") and r["verdicts"].get(str(x["n"])) == "yes"]})
    s["examples_F_not_B"] = ex
    calls = jl(common.COST_LOG)
    s["cost"] = {k: round(sum(c["cost"] or 0 for c in calls if c["tag"].startswith(k)), 3) for k in ("arm:F", "judgeF", "judgeN")}
    s["cost_total"] = round(sum(c["cost"] or 0 for c in calls), 3)
    (FU / "summary.json").write_text(json.dumps(s, indent=1, ensure_ascii=False))
    print(json.dumps({k: v for k, v in s.items() if k != "examples_F_not_B"}, indent=1))


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["recur", "arm", "judge", "report"])
    ap.add_argument("--workers", type=int, default=6)
    ap.add_argument("--kind", default="")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--calib", action="store_true")
    a = ap.parse_args()
    {"recur": cmd_recur, "arm": cmd_arm, "judge": cmd_judge, "report": cmd_report}[a.cmd](a)
