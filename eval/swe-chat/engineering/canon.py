"""Merge Haiku's free-text engineering concepts into canonical principles, one call
per category, so the same idea from different repos lands on one key.

Output: concept_map.jsonl {category, concept, canon}; cost logged to calls.jsonl.
usage: canon.py [cap_usd] [--no-prior]   (--no-prior: retry chunks that came back empty)
"""
import json, subprocess, sys
from pathlib import Path

E = Path.home() / ".local/share/borg/eval/swe-chat/engineering"
CAP = float(sys.argv[1]) if len(sys.argv) > 1 else 5.0
CHUNK = 400

SYSTEM = """You merge engineering principles. Input: numbered list of short principles from code reviews (one category).
Group items that express the SAME reusable engineering idea (same fix would apply), and name each group with a canonical 3-8 word principle. Be strict: merge paraphrases, not merely related topics. Singletons are fine.
Reply with JSON lines only, one per input item: {"i": <number>, "canon": "<canonical principle>"}"""

def spent():
    q = E / "calls.jsonl"; t = 0.0; n = 0
    if q.exists():
        for line in q.open():
            d = json.loads(line); t = d["running_total"]; n = d["call"]
    return t, n

def main():
    labs = [json.loads(l) for l in (E / "labels.jsonl").open()]
    by = {}
    for l in labs:
        if l.get("label") == "engineering" and l.get("concept"):
            by.setdefault(l.get("category") or "other", set()).add(l["concept"].strip().lower())
    done = set()
    mp = E / "concept_map.jsonl"
    if mp.exists():
        done = {(d["category"], d["concept"]) for d in map(json.loads, mp.open())}
    total, ncall = spent()
    for cat, cs in sorted(by.items()):
        cs = sorted(c for c in cs if (cat, c) not in done)
        for k in range(0, len(cs), CHUNK):
            if total >= CAP - 0.15:
                print("cap reached"); return
            part = cs[k:k + CHUNK]
            # carry canon names already chosen for this category so later chunks reuse them
            prior = sorted({d["canon"] for d in map(json.loads, mp.open()) if d["category"] == cat}) if mp.exists() else []
            if "--no-prior" in sys.argv:  # some chunks return nothing when given the prior list
                prior = []
            msg = (("Existing canonical names (reuse when matching):\n" + "\n".join(prior[:300]) + "\n\n") if prior else "") + \
                  "\n".join(f"{i}. {c}" for i, c in enumerate(part))
            cmd = ["claude", "-p", "--model", "haiku", "--setting-sources", "project", "--strict-mcp-config",
                   "--tools", "", "--system-prompt", SYSTEM, "--no-session-persistence", "--output-format", "json"]
            p = subprocess.run(cmd, input=msg, capture_output=True, text=True, timeout=600, cwd=E / "haiku_cwd")
            d = json.loads(p.stdout); cost = d.get("total_cost_usd") or 0.0
            total += cost; ncall += 1
            got = 0
            with mp.open("a") as f:
                for line in (d.get("result") or "").splitlines():
                    line = line.strip().strip(",")
                    if not line.startswith("{"): continue
                    try: o = json.loads(line); c = part[int(o["i"])]
                    except Exception: continue
                    f.write(json.dumps(dict(category=cat, concept=c, canon=o["canon"].strip().lower())) + "\n"); got += 1
            with (E / "calls.jsonl").open("a") as f:
                f.write(json.dumps(dict(call=ncall, model="canon:" + cat, items=len(part), parsed=got,
                                        cost=cost, running_total=round(total, 6))) + "\n")
            print(cat, len(part), "parsed", got, f"cost {cost:.4f} total {total:.4f}", flush=True)

if __name__ == "__main__":
    main()
