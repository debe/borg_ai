#!/usr/bin/env python3
"""Blind Sonnet judge: one call per case over the pooled, shuffled items of all arms -> run/judge.jsonl

  python3 eval/swe-chat/judge.py [--workers 6] [--kind pos|neg]

Arms pooled: N, L, H (where recall exists), B (asked + free lessons). Exact duplicate texts are merged; the
order is shuffled with a per-case seeded RNG; arm names never reach the judge. The key (item -> arms) is kept in
the output row for scoring.
"""
import argparse, concurrent.futures as cf, json, random

from common import OUT, SEED, claude, cut, jl

SYSTEM = "You grade predictions about a real user's next message to a coding agent. Be strict and literal; judge only from the text shown."
POS_SCHEMA = {"type": "object", "additionalProperties": False, "properties": {
    "genuine": {"type": "boolean"},
    "items": {"type": "array", "items": {"type": "object", "additionalProperties": False, "properties": {
        "n": {"type": "integer"}, "verdict": {"type": "string", "enum": ["yes", "partial", "no"]}},
        "required": ["n", "verdict"]}}}, "required": ["genuine", "items"]}
NEG_SCHEMA = {"type": "object", "additionalProperties": False, "properties": {
    "items": {"type": "array", "items": {"type": "object", "additionalProperties": False, "properties": {
        "n": {"type": "integer"}, "false_alarm": {"type": "boolean"}}, "required": ["n", "false_alarm"]}}},
    "required": ["items"]}


def borg_item(l):
    return f"{l.get('question', '')} ({l.get('consequence', '')})" if l.get("question") else None


def pooled(c, arms):
    items = {}
    for arm, rows in arms.items():
        r = rows.get(c["id"])
        if not r or r.get("error"):
            continue
        if arm == "B":
            texts = [(borg_item(l), "B-block") for l in r["asked"]] + [(borg_item(l), "B-free") for l in r["free"]]
        else:
            texts = [(t, arm) for t in r["concerns"]]
        for t, src in texts:
            if t and t.strip():
                items.setdefault(t.strip(), set()).add(src)
    keys = sorted(items)
    random.Random(f"{SEED}-{c['id']}").shuffle(keys)
    return [{"n": i + 1, "text": k, "arms": sorted(items[k])} for i, k in enumerate(keys)]


def prompt(c, items):
    listing = "\n".join(f"{x['n']}. {x['text']}" for x in items)
    head = ("Below is the end of a coding agent's turn, then the user's real next message, then numbered items that "
            "different reviewers wrote BEFORE seeing the user's message (predicted concerns, or lessons a memory "
            "surfaced).\n\n## End of the agent's turn (may be cut)\n{ctx}\n\n## The user's real next message\n{msg}\n\n"
            "## Items\n{items}\n\n").format(ctx=cut(c["context"], 6000), msg=cut(c["prompt"], 3000, head=True), items=listing)
    if c["kind"] == "pos":
        return head + (
            "For EVERY item give a verdict:\n"
            "- yes: had the agent been told this item before the user replied, it would have anticipated the specific "
            "point the user then made (the same correction, objection or requirement), so the user would not have "
            "needed to say it.\n- partial: same topic or direction, but it misses what the user actually wanted.\n"
            "- no: anything else (other topics, generic advice, things the user did not raise).\n"
            "Also genuine: true if the user's message pushes back on, corrects or rejects something in the agent's "
            "turn; false if it is a new request, a follow-up task or a continuation that does not push back.")
    return head + (
        "The user did not push back on this turn. For EVERY item set false_alarm: true if raising it would have been "
        "noise to this user at this point (a concern their actual reply shows they did not have, not about work done "
        "in this turn, or generic advice); false only if it is a substantive concern about this turn that a careful "
        "user could reasonably have wanted checked even though they moved on.")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--workers", type=int, default=6)
    ap.add_argument("--kind", default="")
    ap.add_argument("--only", default="")
    a = ap.parse_args()
    arms = {x: {r["id"]: r for r in jl(OUT / f"arm_{x}.jsonl") if not r.get("error")} for x in ("N", "L", "H")}
    arms["B"] = {r["id"]: r for r in jl(OUT / "borg_hook.jsonl") if not r.get("error")}
    path = OUT / "judge.jsonl"
    have = {r["id"] for r in jl(path) if not r.get("error")}
    todo = [c for c in jl(OUT / "cases.jsonl") if c["id"] not in have and (not a.kind or c["kind"] == a.kind)
            and (not a.only or c["id"] in a.only.split(","))]

    def job(c):
        items = pooled(c, arms)
        present = sorted(x for x in arms if c["id"] in arms[x])
        if not items:
            return {"id": c["id"], "kind": c["kind"], "items": [], "verdicts": {}, "genuine": None, "arms_present": present,
                    "cost": 0, "error": None}
        try:
            r = claude(prompt(c, items), f"judge:{c['id']}", schema=POS_SCHEMA if c["kind"] == "pos" else NEG_SCHEMA, system=SYSTEM)
        except RuntimeError as e:
            return {"id": c["id"], "error": str(e)}
        out = r["out"] or {}
        v = {int(x["n"]): (x.get("verdict") if c["kind"] == "pos" else x.get("false_alarm")) for x in out.get("items", [])}
        missing = [x["n"] for x in items if x["n"] not in v]
        return {"id": c["id"], "kind": c["kind"], "items": items, "verdicts": v, "genuine": out.get("genuine"),
                "arms_present": present, "missing": missing, "cost": r["cost"],
                "error": r["error"] or (f"missing verdicts {missing}" if missing else None)}

    with cf.ThreadPoolExecutor(a.workers) as ex, open(path, "a") as f:
        for r in ex.map(job, todo):
            f.write(json.dumps(r) + "\n")
            f.flush()
            print(r["id"], "ERR " + str(r["error"]) if r.get("error") else f"{len(r['items'])} items", flush=True)


if __name__ == "__main__":
    main()
