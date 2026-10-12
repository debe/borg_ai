#!/usr/bin/env python3
"""Sonnet anticipation arms N (no memory), L (plain log), H (Hindsight recall) -> run/arm_<X>.jsonl

  python3 eval/swe-chat/arms.py N|L|H [--workers 6] [--kind pos|neg]
"""
import argparse, concurrent.futures as cf, datetime, json, re

from common import OUT, claude, jl, ms

SYSTEM = ("You review a coding agent's turn before the user replies. List the concerns the user is most likely to raise "
          "about this turn in their next message: corrections, objections, missed requirements, things they will push "
          "back on. At most 5, most likely first, each one sentence and specific to this turn. If you expect no "
          "pushback, return an empty list.")
SCHEMA = {"type": "object", "properties": {"concerns": {"type": "array", "items": {"type": "string"}, "maxItems": 5}},
          "required": ["concerns"], "additionalProperties": False}
LINE = re.compile(r"^- \[(\d{4}-\d\d-\d\d)\] ")


def plain_log(repo, T):
    """The repo's plain log, only entries dated before the case (episode ms < T, matched by order)."""
    slug = repo.replace("/", "__")
    eps = jl(OUT / "episodes" / f"{slug}.jsonl")[-40:]
    lines = (OUT / "plainlog" / f"{slug}.txt").read_text().splitlines()
    eps = eps[-len(lines):] if lines else []
    keep = [l for l, e in zip(lines, eps) if e["ms"] < T]
    return "\n".join(keep)


def prompt(arm, c, hs):
    turn = f"## The agent's turn (transcript since the user's previous message; may be cut)\n\n{c['context']}"
    if arm == "N":
        return turn
    if arm == "L":
        log = plain_log(c["repo"], ms(c["ts"]))
        return (f"## Earlier corrections by users of this repository (oldest first)\n\n{log or '(none)'}\n\n{turn}")
    if arm == "H":
        return f"## Memory\n\n{hs[c['id']]['text']}\n\n{turn}"
    raise ValueError(arm)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("arm", choices=["N", "L", "H"])
    ap.add_argument("--workers", type=int, default=6)
    ap.add_argument("--kind", default="")
    a = ap.parse_args()
    path = OUT / f"arm_{a.arm}.jsonl"
    have = {r["id"] for r in jl(path) if not r.get("error")}
    hs = {r["id"]: r for r in jl(OUT / "hs_recall.jsonl")}
    todo = [c for c in jl(OUT / "cases.jsonl") if c["id"] not in have and (not a.kind or c["kind"] == a.kind)
            and (a.arm != "H" or c["id"] in hs)]
    # Same repo together (prompt-cache reuse of the shared prefix).
    todo.sort(key=lambda c: (c["repo"], c["id"]))

    def job(c):
        try:
            r = claude(prompt(a.arm, c, hs), f"arm:{a.arm}:{c['id']}", schema=SCHEMA, system=SYSTEM)
        except RuntimeError as e:
            return {"id": c["id"], "error": str(e), "stop": True}
        out = r["out"] or {}
        return {"id": c["id"], "concerns": [x for x in (out.get("concerns") or []) if isinstance(x, str)][:5],
                "cost": r["cost"], "error": r["error"]}

    with cf.ThreadPoolExecutor(a.workers) as ex, open(path, "a") as f:
        for r in ex.map(job, todo):
            f.write(json.dumps(r) + "\n")
            f.flush()
            print(r["id"], "ERR " + str(r["error"]) if r.get("error") else len(r["concerns"]), flush=True)


if __name__ == "__main__":
    main()
