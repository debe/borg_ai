"""Zero-token signals per correction/rejection prompt.

1. authorship: did the human edit lines (Entire prompt_attributions) at the
   checkpoint of the correction turn or the next one, preferably in files the
   agent had just edited; or did the agent rework the same files right after.
2. nd: Notre Dame misalignment labels (misalignments.json), matched to the
   prompt via quoted USER evidence; symptom -> engineering-like / steering-like.
3. kw: English + German engineering vocabulary and code-shaped tokens.

Writes signals.parquet next to corrections.parquet.
"""
import json, re, sys
from pathlib import Path
import pandas as pd

D = Path.home() / ".local/share/borg/eval/swe-chat"
E = D / "engineering"

# ---------- 1. authorship ----------
def authorship(c, turns):
    s = pd.read_parquet(D / "sessions.parquet", columns=["session_id", "prompt_attributions",
                                                          "human_modified", "human_removed"])
    pa = {r.session_id: json.loads(r.prompt_attributions) for r in s.itertuples()
          if isinstance(r.prompt_attributions, str) and r.prompt_attributions.startswith("[")}
    sess_human = set(s.session_id[(s.human_modified.fillna(0) + s.human_removed.fillna(0)) > 0])
    # checkpoint k = k-th prompt turn whose agent edited files.  Only trusted when the
    # session has exactly as many attribution entries as edit turns (checked: ~30% of sessions).
    turns = turns.sort_values(["session_id", "turn_number"])
    cp_of, exact = {}, set()
    for sid, g in turns.groupby("session_id", sort=False):
        k = 0
        for tn, files in zip(g.turn_number, g.files):
            if files != "[]":
                k += 1
            cp_of[(sid, tn)] = k if files != "[]" else k + 1  # no edits -> human edits land in next checkpoint
        if sid in pa and len(pa[sid]) == k and k > 0:
            exact.add(sid)
    out = []
    for r in c.itertuples():
        fb = set(json.loads(r.files_before)); fa = set(json.loads(r.files_after))
        rework = len(fb & fa)
        h_lines = h_overlap = 0
        if r.session_id in exact:
            ents = {e.get("checkpoint_number"): e for e in pa[r.session_id]}
            k = cp_of.get((r.session_id, r.turn_number))
            for kk in (k, k + 1) if k else ():
                e = ents.get(kk)
                if not e:
                    continue
                h_lines += (e.get("user_lines_added") or 0) + (e.get("user_lines_removed") or 0)
                per = e.get("user_added_per_file") or {}
                h_overlap += sum(v for f, v in per.items() if any(f and (x.endswith(f) or f.endswith(x)) for x in fb))
        sh = r.session_id in sess_human
        score = 2 * (h_lines > 0) + 1 * (h_overlap > 0) + 1 * (rework > 0) + 0.5 * sh
        out.append(dict(attr_exact=r.session_id in exact, human_lines=h_lines, human_on_agent_files=h_overlap,
                        agent_rework_files=rework, session_human_edit=sh, s1=score))
    return pd.DataFrame(out, index=c.index)

# ---------- 2. Notre Dame labels ----------
ENG_S = {"S1": "wrong_project_diagnosis", "S3": "constraint_violation", "S5": "faulty_implementation"}
STEER_S = {"S2": "misread_intent", "S4": "overreach", "S6": "operational", "S7": "inaccurate_self_report"}
norm = lambda t: re.sub(r"\s+", " ", (t or "").lower()).strip()

def nd(c):
    m = [x for x in json.load(open(E / "nd/misalignments.json")) if x["source"] == "swe"]
    by_sess = {}
    for x in m:
        by_sess.setdefault(x["session_sha"], []).append(x)
    content = c.content.map(norm)
    res = []
    for i, r in zip(c.index, c.itertuples()):
        recs = by_sess.get(r.session_id, [])
        hit_e = hit_s = 0; forms = set(); matched = False
        for x in recs:
            frags = [f for q in x.get("evidence", []) if "USER" in q.get("turn", "")
                     for f in norm(q.get("quote")).split("...") if len(f.strip()) >= 15]
            prompt_hit = any(f.strip()[:80] in content[i] for f in frags)
            if not prompt_hit:
                continue
            matched = True
            e = [v for k, v in ENG_S.items() if x.get(k)]; s = [v for k, v in STEER_S.items() if x.get(k)]
            hit_e += bool(e); hit_s += bool(s); forms.update(e + s)
        res.append(dict(nd_session=bool(recs), nd_prompt=matched, nd_eng=hit_e, nd_steer=hit_s,
                        nd_forms=",".join(sorted(forms)),
                        s2=1.0 if hit_e and hit_e >= hit_s else (0.5 if hit_e else 0.0)))
    return pd.DataFrame(res, index=c.index), len(m), len(by_sess)

# ---------- 3. keyword prefilter ----------
EN = r"""architect\w*|design(ed)?\b|abstraction|coupl\w+|interface|\bapi\b|endpoint|signature|contract|schema|
migration|data ?model|foreign key|\bindex(es)?\b|normali[sz]\w+|race condition|deadlock|\block(s|ing)?\b|mutex|
atomic\w*|concurren\w+|thread\w*|\basync\b|await|goroutine|channel|transaction\w*|idempoten\w+|retry|retries|
cach(e|ing)|invalidat\w+|o\(n|complexity|performan\w+|\bslow\w*|latency|memory|leak\w*|allocat\w+|n\+1|\bquer(y|ies)\b|
\bbatch\w*|stream\w*|paginat\w+|algorithm\w*|data structure|hash ?map|dictionary|\bqueue\b|\bheap\b|recurs\w+|
\bsort\w*|binary search|dedup\w*|secur\w+|injection|saniti[sz]\w+|escap\w+|\bxss\b|csrf|\bauth\w*|secret|encrypt\w*|
permission|validat\w+|error handling|exception|try ?/? ?catch|\bpanic\b|unwrap|fallback|swallow\w*|\bnull\b|
undefined|edge case|corner case|off[- ]by[- ]one|boundary|overflow|precision|timezone|encoding|type safe\w*|
\bgeneric\w*|\benum\b|immutab\w+|mutab\w+|\bstate\b|side[- ]effect|pure function|dependency injection|singleton|
global variable|duplicat\w+|\bdry\b|separation of concerns|responsibilit\w+|\blayer\w*|\bmodule\b|inherit\w*|
composition|polymorph\w+|ownership|borrow\w*|lifetime|\bclone\b|pointer|resource|connection pool|timeout|
backpressure|ordering|consisten\w+|invariant|\bpars(e|er|ing)\b|seriali[sz]\w+|protocol|versioning|
backward[s]? compat\w*|breaking change|hard[- ]?cod\w+|magic number|refactor\w*|instead of|rather than|
better (way|approach)|wrong approach|over[- ]?engineer\w*|(?:un)?necessary complexity|single source of truth|
type\b|\btypes\b|null ?check|bounds|o\(1\)|o\(n\^?2\)|blocking|non[- ]blocking|event loop|callback|
promise|closure|scope|race|starv\w+|sharding|partition\w*|denormali[sz]\w+|join\b"""
DE = r"""architektur|entwurf|schnittstelle|datenmodell|nebenläufig\w*|sperr\w+|wettlauf\w*|speicher\w*|leistung|
langsam\w*|algorithm\w*|datenstruktur\w*|sicherheit\w*|fehlerbehandlung|ausnahme\w*|abhängigkeit\w*|kopplung|
zustand\w*|transaktion\w*|zwischenspeicher|abfrage\w*|tabelle\w*|validier\w+|grenzfall|randfall|rekursi\w+|
schicht\w*|doppelt\w*|hartcodiert|komplexität|effizien\w+|ineffizien\w+|laufzeit|asynchron|atomar|muster|
abstraktion|kapselung|vererbung|berechtigung\w*|verschlüssel\w+|statt\b|anstatt|besser(e|er|en)? (weg|ansatz|lösung)|
falsche[rn]? ansatz|überflüssig|umständlich|redundan\w+|typsicher\w*|eindeutig\w*"""
KW = re.compile("|".join(x.replace("\n", "") for x in (EN, DE)), re.I)
CODE = re.compile(r"`[^`]+`|\b[a-z]+[A-Z]\w+\b|\b\w+_\w+\b|\b\w+\(\)|\b[\w/]+\.(py|ts|tsx|js|rs|go|java|kt|rb|sql|swift|cs|cpp|h)\b")
STEER = re.compile(r"\b(commit|push|shorter|concise|explain|summar\w+|continue|go ahead|proceed|stop|run the tests?|"
                   r"don'?t ask|format\w*|markdown|bullet|readme|weiter|mach weiter|kürzer)\b", re.I)

def keywords(c):
    t = c.content.fillna("")
    kw = t.map(lambda x: len({m.group(0).lower() for m in KW.finditer(x[:3000])}))
    code = t.map(lambda x: len(CODE.findall(x[:3000])))
    steer = t.map(lambda x: len(STEER.findall(x[:3000])))
    words = t.str.split().map(len)
    s3 = kw.clip(upper=5) + 0.5 * code.clip(upper=4) - 0.5 * steer.clip(upper=3)
    keep = (words >= 6) & ((kw >= 1) | ((code >= 2) & (words >= 15)))
    return pd.DataFrame(dict(kw_hits=kw, code_tokens=code, steer_hits=steer, words=words, s3=s3, kw_pass=keep))

def main():
    c = pd.read_parquet(E / "corrections.parquet")
    turns = pd.read_parquet(E / "corrections_turns.parquet")
    a = authorship(c, turns)
    n, n_rec, n_sess = nd(c)
    k = keywords(c)
    out = pd.concat([c, a, n, k], axis=1)
    out.to_parquet(E / "signals.parquet")
    print("prompts", len(out), "sessions", out.session_id.nunique(), "repos", out.repo_id.nunique())
    print("S1 exact-mapped", int(out.attr_exact.sum()), "human edits at cp", int((out.human_lines > 0).sum()),
          "human on agent files", int((out.human_on_agent_files > 0).sum()),
          "agent rework", int((out.agent_rework_files > 0).sum()), "session human edits", int(out.session_human_edit.sum()),
          "s1>=1", int((out.s1 >= 1).sum()), "s1>=2", int((out.s1 >= 2).sum()))
    print("S2 nd records(swe)", n_rec, "nd sessions", n_sess, "prompts in nd sessions", int(out.nd_session.sum()),
          "prompt-matched", int(out.nd_prompt.sum()), "eng-like", int((out.nd_eng > 0).sum()),
          "steer-like only", int(((out.nd_steer > 0) & (out.nd_eng == 0)).sum()))
    print("S3 kw_pass", int(out.kw_pass.sum()))
    # Haiku candidates: keyword pass, or any authorship / ND engineering signal (non-trivial text)
    cand = out[out.kw_pass | ((out.words >= 6) & ((out.s1 >= 2) | (out.nd_eng > 0)))].copy()
    cand["id"] = cand.session_id + "#" + cand.turn_number.astype(str)
    cand["priority"] = (2 * (cand.s1 >= 2) + cand.s1 + 3 * (cand.nd_eng > 0) + cand.s3
                        + cand.words.clip(upper=200).pow(0.5) / 5)
    cand.to_parquet(E / "candidates.parquet")
    print("candidates", len(cand), "with s1>=2", int((cand.s1 >= 2).sum()), "with nd_eng", int((cand.nd_eng > 0).sum()))

if __name__ == "__main__":
    main()
