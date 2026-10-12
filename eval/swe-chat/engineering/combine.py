"""Combine signals + Haiku labels into the engineering-correction test set.

engineering correction = Haiku 'engineering' AND (authorship s1>=2 OR ND engineering-like
OR long specific text).  Writes engineering_set.parquet and prints the stage counts,
per-agent/repo spread, replay coverage and the concept x repo cross-repo table.
"""
import json, re
from pathlib import Path
import pandas as pd

# pasted review reports, logs, findings tables: engineering text, but not the user's own correction
PASTE = re.compile(r"(^|\n)\s*#{2,} |\|\s*-{3}|\*\*(Issue|Finding|Verdict|Location|File|Rule|Severity)|Verify each finding|"
                   r"FINDING \d|\[P\d\]|^\s*(Findings|Summary|Review)\b|\d{4}-\d\d-\d\dT\d\d:\d\d|Traceback|panicked at|^<\w+>|"
                   r"Nitpick comments", re.I | re.M)

D = Path.home() / ".local/share/borg/eval/swe-chat"
E = D / "engineering"

def load():
    s = pd.read_parquet(E / "signals.parquet")
    s["id"] = s.session_id + "#" + s.turn_number.astype(str)
    labs = pd.DataFrame([json.loads(l) for l in (E / "labels.jsonl").open()]).drop_duplicates("id", keep="last")
    df = s.merge(labs, on="id", how="left")
    cm = pd.DataFrame([json.loads(l) for l in (E / "concept_map.jsonl").open()]) if (E / "concept_map.jsonl").exists() \
        else pd.DataFrame(columns=["category", "concept", "canon"])
    df["concept_l"] = df.concept.fillna("").str.strip().str.lower()
    df = df.merge(cm.drop_duplicates(["category", "concept"]).rename(columns={"concept": "concept_l"}),
                  on=["category", "concept_l"], how="left")
    df["canon"] = df.canon.fillna(df.concept_l.where(df.concept_l != ""))
    ts = pd.read_parquet(D / "prompt_labels.parquet", columns=["session_id", "turn_number", "timestamp"])
    df = df.merge(ts, on=["session_id", "turn_number"], how="left")
    ss = pd.read_parquet(D / "sessions.parquet", columns=["session_id", "user_id", "owner_id", "checkpoint_ids"])
    cp = pd.read_parquet(D / "checkpoints.parquet", columns=["checkpoint_pk", "commit_shas"])
    linked = set(cp.checkpoint_pk[cp.commit_shas.fillna("[]") != "[]"])
    ss["has_commit"] = ss.checkpoint_ids.fillna("[]").map(lambda v: any(c in linked for c in json.loads(v)))
    df = df.merge(ss.drop(columns="checkpoint_ids"), on="session_id", how="left")
    lg = pd.read_parquet(D / "session_logs.parquet", columns=["session_id", "transcript_path", "compact_transcript_path"])
    lg["has_transcript"] = lg.transcript_path.fillna("") != ""
    lg["has_compact"] = lg.compact_transcript_path.fillna("") != ""
    df = df.merge(lg[["session_id", "has_transcript", "has_compact"]], on="session_id", how="left")
    df["user"] = df.user_id.fillna("").where(df.user_id.fillna("") != "", df.owner_id)
    return df

def main():
    df = load()
    lab = df[df.label.notna()]
    df["long_specific"] = (df.words >= 30) & ((df.code_tokens >= 1) | (df.kw_hits >= 2))
    df["corroborated"] = (df.s1 >= 2) | (df.nd_eng > 0) | df.long_specific
    eng = df[(df.label == "engineering") & df.corroborated & df.has_transcript.fillna(False)].copy()
    eng.to_parquet(E / "engineering_set.parquet")

    print("== stages")
    print("correction+rejection prompts", len(df), "| sessions", df.session_id.nunique(), "| repos", df.repo_id.nunique())
    print("signal1 s1>0", int((df.s1 > 0).sum()), "s1>=2", int((df.s1 >= 2).sum()))
    print("signal2 nd prompt-matched", int(df.nd_prompt.sum()), "nd eng-like", int((df.nd_eng > 0).sum()))
    print("keyword pass", int(df.kw_pass.sum()))
    print("haiku labelled", len(lab), lab.label.value_counts().to_dict())
    print("engineering & corroborated", len(eng),
          "| via s1", int((eng.s1 >= 2).sum()), "via nd", int((eng.nd_eng > 0).sum()), "via long", int(eng.long_specific.sum()))
    print("== spread")
    print("agents", eng.agent.value_counts().to_dict())
    print("repos", eng.repo_id.nunique(), "users", eng.user.nunique(), "sessions", eng.session_id.nunique())
    print("with compact transcript", int(eng.has_compact.sum()), "| haiku-eng without transcript dropped",
          int(((df.label == "engineering") & df.corroborated & ~df.has_transcript.fillna(False)).sum()))
    print("with repo+commit for replay", int(eng.has_commit.sum()), "repos", eng[eng.has_commit].repo_id.nunique())
    print("top repos", eng.repo_id.value_counts().head(10).to_dict())
    print("categories", eng.category.value_counts().to_dict())

    print("== cross-repo")
    g = eng.groupby("canon").agg(repos=("repo_id", "nunique"), cases=("id", "size"), users=("user", "nunique"))
    multi = g[g.repos >= 2].sort_values(["repos", "cases"], ascending=False)
    print("canon concepts", len(g), "in >=2 repos", len(multi), "cases in those", int(multi.cases.sum()))
    eng = eng.sort_values("timestamp")
    seen = {}; prior = []
    for r in eng.itertuples():
        reps = seen.setdefault(r.canon, set())
        prior.append(bool(reps - {r.repo_id}))
        reps.add(r.repo_id)
    eng["has_prior_other_repo"] = prior
    print("cases whose concept appeared earlier in another repo", int(eng.has_prior_other_repo.sum()),
          "| of those with commit", int((eng.has_prior_other_repo & eng.has_commit).sum()))
    print(multi.head(25).to_string())
    eng.to_parquet(E / "engineering_set.parquet")
    multi.to_csv(E / "concept_repo.csv")

    # quality tier for the eventual ~150-case test: strong signal or long, code-specific text
    eng["strong"] = (eng.s1 >= 2) | (eng.nd_eng > 0)
    eng["specific"] = (eng.words >= 40) & (eng.code_tokens >= 2) & (eng.kw_hits >= 2)
    eng["paste"] = eng.content.map(lambda t: bool(PASTE.search(t[:4000])))
    print("== quality tier (paste = pasted review/log text)")
    print("strong|specific", int((eng.strong | eng.specific).sum()), "of which paste", int(((eng.strong | eng.specific) & eng.paste).sum()))
    q = eng[(eng.strong | eng.specific) & ~eng.paste].copy()
    seen = {}; prior = []
    for r in q.itertuples():
        reps = seen.setdefault(r.canon, set()); prior.append(bool(reps - {r.repo_id})); reps.add(r.repo_id)
    q["q_prior_other_repo"] = prior
    gq = q.groupby("canon").agg(repos=("repo_id", "nunique"), cases=("id", "size")).sort_values(["repos", "cases"], ascending=False)
    print("quality cases", len(q), "| strong", int(q.strong.sum()), "specific-only", int((~q.strong).sum()),
          "| repos", q.repo_id.nunique(), "users", q.user.nunique(), "with commit", int(q.has_commit.sum()),
          "| compact transcript", int(q.has_compact.sum()))
    print("quality concepts", len(gq), "in >=2 repos", int((gq.repos >= 2).sum()),
          "| cases whose concept appeared earlier in another repo (within tier)", int(q.q_prior_other_repo.sum()),
          "with commit", int((q.q_prior_other_repo & q.has_commit).sum()),
          "| excluding entireio/cli", int((q.q_prior_other_repo & (q.repo_id != "entireio/cli")).sum()))
    print(gq.head(15).to_string())
    q.to_parquet(E / "quality_set.parquet")
    gq[gq.repos >= 2].to_csv(E / "concept_repo_quality.csv")

if __name__ == "__main__":
    main()
