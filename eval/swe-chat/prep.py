#!/usr/bin/env python3
"""Build the eval inputs (no model calls):

  cases.jsonl     150 correction/rejection prompts from test sessions (+ 50 non_pushback negatives)
  episodes/<repo>.jsonl  learn-session corrections as borg Episodes (the same set feeds the plain log)
  plainlog/<repo>.txt    the plain-log arm's memory: those corrections verbatim (each cut), oldest first
  hs_docs/<repo>.jsonl   learn sessions rendered for Hindsight (user prompts + agent text, as its plugin retains)

usage: python3 eval/swe-chat/prep.py [--stats]
"""
import collections, json, random, re, sys

import pandas as pd

from common import DATA, OUT, SEED, FILLER, events, context, human, cut, ms, render

PUSH = ("correction", "rejection")
PER_REPO, N_POS, N_NEG = 10, 150, 50
LOG_ITEM = 400      # chars per plain-log entry
LOG_CAP = 24000     # chars per repo plain log (newest kept if over)
EP_CAP = 40         # newest learn-session corrections per repo that Borg learns from and the plain log lists


def slug(repo):
    return repo.replace("/", "__")


def labelled(sample):
    p = pd.read_parquet(DATA / "prompt_labels.parquet")
    p = p[p.session_id.isin(sample.session_id)]
    rows = collections.defaultdict(list)
    for r in p.itertuples():
        if pd.isna(r.timestamp):
            continue
        rows[r.session_id].append((r.timestamp.isoformat().replace("+00:00", ""), r.prompt_pushback, r.turn_number))
    return rows


def match(evs, labels):
    """(event index, label) for each labelled prompt, matched by exact timestamp to a human user event."""
    by_ts = collections.defaultdict(list)
    for i, e in enumerate(evs):
        if e["kind"] == "user" and human(e["text"]):
            by_ts[e["ts"][:23]].append(i)
    # One label per timestamp: a prompt split into blocks (image + text) is labelled once, the rest None.
    best = {}
    for ts, lab, turn in labels:
        if lab is not None or ts[:23] not in best:
            best[ts[:23]] = (lab, turn) if lab is not None or ts[:23] not in best else best[ts[:23]]
    out = []
    for key, (lab, turn) in best.items():
        cand = [i for i in by_ts.get(key, []) if not evs[i]["text"].startswith("[Image: source")]
        if cand and lab is not None:
            out.append((cand[0], lab, turn))
    return out


def ok_prompt(t):
    w = t.split()
    return len(w) >= 5 and not FILLER.match(t) and not t.lstrip().startswith(("/", "#"))


def main():
    sample = pd.read_csv(DATA / "sample.csv")
    users = pd.read_parquet(DATA / "sessions.parquet", columns=["session_id", "user_id"]).set_index("session_id")["user_id"].to_dict()
    labels = labelled(sample)
    pos, neg, eps = collections.defaultdict(list), collections.defaultdict(list), collections.defaultdict(list)
    hs_docs = collections.defaultdict(list)
    unmatched = 0
    for s in sample.itertuples():
        evs = events(s.session_id)
        m = match(evs, labels.get(s.session_id, []))
        unmatched += len(labels.get(s.session_id, [])) - len(m)
        if s.split == "learn":
            text = render([e for e in evs if e["kind"] != "tool"])
            hs_docs[s.repo_id].append({"session": s.session_id, "ts": evs[0]["ts"] if evs else str(s.created_at),
                                       "chars": len(text), "content": text})
        # The task of an episode: the latest human prompt before it that was not itself pushback (as assimilate does).
        push_idx = {j for j, lab, _ in m if lab in PUSH}
        task, tasks = "", {}
        for i, e in enumerate(evs):
            if e["kind"] == "user" and human(e["text"]):
                tasks[i] = task
                if i not in push_idx:
                    task = e["text"]
        for i, lab, turn in m:
            t = evs[i]["text"].strip()
            agent = next((evs[j]["text"] for j in range(i - 1, -1, -1) if evs[j]["kind"] == "agent"), "")
            if not agent or not ok_prompt(t):
                continue
            if s.split == "learn" and lab in PUSH:
                eps[s.repo_id].append({"id": f"{s.session_id[:8]}-{turn}", "ms": ms(evs[i]["ts"]), "repo": s.repo_id.split("/")[-1],
                                       "session": s.session_id, "task": cut(tasks.get(i, ""), 1500, head=True),
                                       "agent": agent[-2000:], "rejected_tool": None, "correction": cut(t, 1500, head=True),
                                       "engineer": users.get(s.session_id) or ""})
            if s.split == "test":
                ctx = context(evs, i)
                if not ctx:
                    continue
                row = {"session": s.session_id, "repo": s.repo_id, "turn": int(turn), "label": lab, "ts": evs[i]["ts"],
                       "prompt": t, "context": ctx, "idx": i}
                (pos if lab in PUSH else neg if lab == "non_pushback" else {}).setdefault(s.repo_id, []).append(row) \
                    if lab in PUSH or lab == "non_pushback" else None
    rng = random.Random(SEED)
    chosen = []
    repos = sorted(pos)
    extra = []
    for r in repos:
        xs = sorted(pos[r], key=lambda x: (x["session"], x["turn"]))
        rng.shuffle(xs)
        chosen += xs[:PER_REPO]
        extra += xs[PER_REPO:]
    rng.shuffle(extra)
    chosen += extra[:max(0, N_POS - len(chosen))]
    negs_all = [x for r in sorted(neg) for x in sorted(neg[r], key=lambda x: (x["session"], x["turn"]))]
    # negatives: round-robin over repos so every repo contributes
    by = collections.defaultdict(list)
    for x in negs_all:
        by[x["repo"]].append(x)
    for r in by:
        rng.shuffle(by[r])
    negs, k = [], 0
    while len(negs) < N_NEG and any(by.values()):
        r = sorted(by)[k % len(by)]
        if by[r]:
            negs.append(by[r].pop())
        k += 1
    if "--stats" in sys.argv:
        print("unmatched labelled prompts:", unmatched)
        for r in sorted(set(pos) | set(eps)):
            print(f"{r:35s} test-pos {len(pos.get(r, [])):4d} chosen {sum(1 for c in chosen if c['repo'] == r):3d} "
                  f"neg {len(neg.get(r, [])):4d} learn-eps {len(eps.get(r, [])):4d} hs-chars {sum(d['chars'] for d in hs_docs.get(r, [])):9d}")
        print("cases", len(chosen), "negatives", len(negs), "episodes", sum(map(len, eps.values())),
              "hs chars", sum(d["chars"] for v in hs_docs.values() for d in v))
        return
    OUT.mkdir(parents=True, exist_ok=True)
    with open(OUT / "cases.jsonl", "w") as f:
        for n, c in enumerate(chosen):
            f.write(json.dumps({"id": f"p{n:03d}", "kind": "pos", **c}) + "\n")
        for n, c in enumerate(negs):
            f.write(json.dumps({"id": f"n{n:03d}", "kind": "neg", **c}) + "\n")
    for d in ("episodes", "plainlog", "hs_docs"):
        (OUT / d).mkdir(exist_ok=True)
    for r, xs in eps.items():
        xs.sort(key=lambda e: e["ms"])
        xs = xs[-EP_CAP:]
        with open(OUT / "episodes" / f"{slug(r)}.jsonl", "w") as f:
            for e in xs:
                f.write(json.dumps(e) + "\n")
        lines = [f"- [{pd.Timestamp(e['ms'], unit='ms'):%Y-%m-%d}] {cut(re.sub(chr(10) + '+', ' / ', e['correction']), LOG_ITEM, head=True)}" for e in xs]
        while sum(len(l) + 1 for l in lines) > LOG_CAP:
            lines.pop(0)
        (OUT / "plainlog" / f"{slug(r)}.txt").write_text("\n".join(lines))
    for r, ds in hs_docs.items():
        with open(OUT / "hs_docs" / f"{slug(r)}.jsonl", "w") as f:
            for d in sorted(ds, key=lambda d: d["ts"]):
                f.write(json.dumps(d) + "\n")
    print("cases", len(chosen), "negatives", len(negs), "episodes", sum(map(len, eps.values())))


if __name__ == "__main__":
    main()
