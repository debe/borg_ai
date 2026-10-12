"""Print the review sample as markdown: 25 quality-tier engineering corrections whose concept
recurs in another repo, and 10 keyword-heavy prompts Haiku rejected as steering."""
import json, re
from pathlib import Path
import pandas as pd

E = Path.home() / ".local/share/borg/eval/swe-chat/engineering"

def clip(t, n=260):
    t = re.sub(r"\s+", " ", t or "").replace("|", "\\|").strip()
    return t if len(t) <= n else t[:n].rstrip() + " ..."

def main():
    eng = pd.read_parquet(E / "quality_set.parquet")
    multi = set(pd.read_csv(E / "concept_repo_quality.csv").canon)
    g = eng[(eng.words.between(15, 160)) & eng.canon.isin(multi)].copy()
    g["rank"] = 3 * (g.s1 >= 2) + 3 * (g.nd_eng > 0) + g.kw_hits.clip(upper=4) + g.code_tokens.clip(upper=3)
    g = g.sample(frac=1, random_state=7).sort_values("rank", ascending=False)
    pick, per_repo, per_cat = [], {}, {}
    for r in g.itertuples():
        if per_repo.get(r.repo_id, 0) >= 1 or per_cat.get(r.category, 0) >= 4:
            continue
        pick.append(r); per_repo[r.repo_id] = 1; per_cat[r.category] = per_cat.get(r.category, 0) + 1
        if len(pick) == 25:
            break
    print("| # | repo | category / concept | signals | correction (trimmed) | agent had |")
    print("|---|---|---|---|---|---|")
    for i, r in enumerate(pick, 1):
        sig = ",".join(x for x, ok in (("human-edit", r.s1 >= 2), ("ND:" + (r.nd_forms or ""), r.nd_eng > 0),
                                        ("rework", r.agent_rework_files > 0), ("long", r.long_specific)) if ok)
        print(f"| {i} | {r.repo_id} | {r.category} / {clip(r.canon, 60)} | {sig} | {clip(r.content)} | {clip(r.agent_did, 120)} |")
    s = pd.read_parquet(E / "signals.parquet")
    s["id"] = s.session_id + "#" + s.turn_number.astype(str)
    labs = pd.DataFrame([json.loads(l) for l in (E / "labels.jsonl").open()]).drop_duplicates("id", keep="last")
    st = s.merge(labs, on="id")
    st = st[(st.label == "steering") & (st.kw_hits >= 2) & st.words.between(10, 120)]
    st = st.sample(frac=1, random_state=7).drop_duplicates("repo_id").head(10)
    print("\n| # | repo | correction (trimmed) | why steering (Haiku paraphrase) |")
    print("|---|---|---|---|")
    for i, r in enumerate(st.itertuples(), 1):
        print(f"| {i} | {r.repo_id} | {clip(r.content, 220)} | {clip(r.paraphrase, 140)} |")

if __name__ == "__main__":
    main()
