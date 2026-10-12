#!/usr/bin/env python3
"""Score the judged cases (pre-registered metrics, reports/swe-chat.md) -> run/summary.json and stdout.

Catch (strict) = some item of the arm judged yes; lenient = yes or partial. Arms: N, L, H, B-block (asked lessons),
B-all (asked + free). Paired differences by cluster bootstrap over repos (10,000 resamples, seed 20261012).
"""
import collections, json, math, random, sys

from common import OUT, SEED, jl

H5 = {"entireio/cli", "JanDeDobbeleer/oh-my-posh", "ebridges/wakil", "yash-learner/care_fe", "hxrshx/ehl-bayern-codewerk"}
ARMS = ["N", "L", "H5", "H", "B-block", "B-all"]
PAIRS = [("B-all", "L"), ("B-all", "N"), ("H5", "L"), ("B-all", "H5"), ("H5", "N"), ("H", "L"), ("B-all", "H"), ("L", "N"), ("B-block", "L"), ("B-block", "N"),
         ("B-block", "H")]


def wilson(k, n, z=1.96):
    if not n:
        return [None, None]
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return [round(100 * (c - h), 1), round(100 * (c + h), 1)]


def srcs(item, arm):
    s = set(item["arms"])
    if arm == "B-all":
        return bool(s & {"B-block", "B-free"})
    return ("H" if arm == "H5" else arm) in s


def arm_present(r, arm):
    if arm == "H5":
        return r["repo"] in H5 and "H" in r["arms_present"]
    return (arm.split("-")[0] if arm.startswith("B") else arm) in r["arms_present"]


def caught(r, arm, lenient=False):
    ok = {"yes", "partial"} if lenient else {"yes"}
    return any(srcs(x, arm) and r["verdicts"].get(str(x["n"]), r["verdicts"].get(x["n"])) in ok for x in r["items"])


def boot(rows, a, b, f, reps=10000):
    """Cluster bootstrap over repos of mean(f(a)) - mean(f(b)) on cases where both arms exist."""
    by = collections.defaultdict(list)
    for r in rows:
        if arm_present(r, a) and arm_present(r, b):
            by[r["repo"]].append((f(r, a), f(r, b)))
    repos = sorted(by)
    if not repos:
        return None
    pt = lambda rs: (sum(x for r_ in rs for x, _ in by[r_]) - sum(y for r_ in rs for _, y in by[r_])) / max(1, sum(len(by[r_]) for r_ in rs))
    rng = random.Random(SEED)
    ds = sorted(pt([rng.choice(repos) for _ in repos]) for _ in range(reps))
    n = sum(len(v) for v in by.values())
    return {"n": n, "repos": len(repos), "diff": round(100 * pt(repos), 1),
            "ci": [round(100 * ds[int(0.025 * reps)], 1), round(100 * ds[int(0.975 * reps) - 1], 1)]}


def main():
    cases = {c["id"]: c for c in jl(OUT / "cases.jsonl")}
    J = [dict(r, repo=cases[r["id"]]["repo"]) for r in jl(OUT / "judge.jsonl") if not r.get("error")]
    for r in J:
        r["verdicts"] = {str(k): v for k, v in r["verdicts"].items()}
    pos = [r for r in J if r["kind"] == "pos"]
    neg = [r for r in J if r["kind"] == "neg"]
    s = {"n_pos": len(pos), "n_neg": len(neg), "catch": {}, "catch_genuine": {}, "pairs": {}, "pairs_lenient": {},
         "pairs_genuine": {}, "items_per_case": {}, "false_alarms": {}}
    gen = [r for r in pos if r.get("genuine")]
    s["genuine_share"] = [len(gen), len(pos)]
    f_strict = lambda r, a: 1 if caught(r, a) else 0
    f_len = lambda r, a: 1 if caught(r, a, True) else 0
    for arm in ARMS:
        P = [r for r in pos if arm_present(r, arm)]
        G = [r for r in gen if arm_present(r, arm)]
        k, kl, kg = sum(map(lambda r: f_strict(r, arm), P)), sum(map(lambda r: f_len(r, arm), P)), sum(map(lambda r: f_strict(r, arm), G))
        s["catch"][arm] = {"n": len(P), "strict": k, "strict_pct": round(100 * k / max(1, len(P)), 1), "strict_ci": wilson(k, len(P)),
                           "lenient": kl, "lenient_pct": round(100 * kl / max(1, len(P)), 1), "lenient_ci": wilson(kl, len(P))}
        s["catch_genuine"][arm] = {"n": len(G), "strict": kg, "pct": round(100 * kg / max(1, len(G)), 1), "ci": wilson(kg, len(G))}
        s["items_per_case"][arm] = round(sum(sum(1 for x in r["items"] if srcs(x, arm)) for r in P) / max(1, len(P)), 2)
        Nn = [r for r in neg if arm_present(r, arm)]
        fa = [sum(1 for x in r["items"] if srcs(x, arm) and r["verdicts"].get(str(x["n"])) is True) for r in Nn]
        its = [sum(1 for x in r["items"] if srcs(x, arm)) for r in Nn]
        s["false_alarms"][arm] = {"n": len(Nn), "mean_false_alarms": round(sum(fa) / max(1, len(fa)), 2),
                                  "share_with_any": round(100 * sum(1 for x in fa if x) / max(1, len(fa)), 1),
                                  "ci_any": wilson(sum(1 for x in fa if x), len(fa)), "mean_items": round(sum(its) / max(1, len(its)), 2)}
    for a, b in PAIRS:
        s["pairs"][f"{a} - {b}"] = boot(pos, a, b, f_strict)
        s["pairs_lenient"][f"{a} - {b}"] = boot(pos, a, b, f_len)
        s["pairs_genuine"][f"{a} - {b}"] = boot(gen, a, b, f_strict)
    # examples: B caught (yes) and L missed, and the other way
    ex = {"B_not_L": [], "L_not_B": []}
    for r in pos:
        if not (arm_present(r, "B-all") and arm_present(r, "L")):
            continue
        b, l = caught(r, "B-all"), caught(r, "L")
        if b != l:
            key = "B_not_L" if b else "L_not_B"
            win = "B-all" if b else "L"
            ex[key].append({"id": r["id"], "repo": r["repo"], "prompt": cases[r["id"]]["prompt"][:600],
                            "hit_items": [x["text"] for x in r["items"] if srcs(x, win) and r["verdicts"].get(str(x["n"])) == "yes"]})
    s["examples"] = ex
    calls = jl(OUT / "claude-calls.jsonl")
    cost = collections.defaultdict(float)
    ncalls = collections.Counter()
    for c in calls:
        t = (c.get("tag") or "").split(":")
        key = t[0] if t[0] in ("learn", "judge", "stop", "setup") else f"arm {t[1]}" if t[0] == "arm" else t[0]
        cost[key] += c.get("cost") or 0
        ncalls[key] += 1
    s["cost_usd"] = {k: round(v, 3) for k, v in sorted(cost.items())}
    s["calls"] = dict(ncalls)
    s["cost_total"] = round(sum(cost.values()), 2)
    hook = jl(OUT / "borg_hook.jsonl")
    s["borg_stop"] = {"cases": len(hook), "blocked": sum(1 for h in hook if h.get("blocked")),
                      "with_free": sum(1 for h in hook if h.get("free")), "skipped_prefilter": sum(1 for h in hook if h.get("skip")),
                      "asked_items": sum(len(h.get("asked") or []) for h in hook), "free_items": sum(len(h.get("free") or []) for h in hook),
                      "errors": sum(1 for h in hook if h.get("error"))}
    (OUT / "summary.json").write_text(json.dumps(s, indent=2, ensure_ascii=False))
    print(json.dumps({k: v for k, v in s.items() if k != "examples"}, indent=1))
    if "-x" in sys.argv:
        print(json.dumps(ex, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    main()
