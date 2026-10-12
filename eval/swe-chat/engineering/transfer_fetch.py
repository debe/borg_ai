"""Cross-repo transfer test, data fetch (0 tokens): from SWE-chat's conversations.parquet (HF, column-projected).

  1. prompt timestamps: every user_prompt turn of the 5,680-set sessions -> transfer/prompt_ts.parquet
  2. full rows of the test sessions (the 149 cases)                  -> transfer/test_rows.parquet
  `pool`: user prompts (text) of the memory pool's sessions          -> transfer/pool_prompts.parquet

Reads only the needed columns; nothing is written outside ~/.local/share/borg/eval/.
"""
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pandas as pd
import pyarrow as pa
import pyarrow.parquet as pq
from huggingface_hub import HfFileSystem

E = Path.home() / ".local/share/borg/eval/swe-chat/engineering"
T = E / "transfer"
URL = "datasets/SALT-NLP/SWE-chat/conversations.parquet"
IDX = ["session_id", "turn_number", "turn_type", "timestamp"]
FULL = ["session_id", "turn_number", "turn_type", "role", "content", "timestamp", "tool_name", "tool_call_id", "file_path",
        "command", "pattern", "tool_input_json", "agent", "content_block_index"]
NW = 12


def open_pf():
    return pq.ParquetFile(HfFileSystem().open(URL, "rb", block_size=4 << 20))


def test_cases():
    q = pd.read_parquet(E / "quality_set.parquet")
    return q[q.q_prior_other_repo & q.has_commit]


def scan(groups, sessions):
    pf = open_pf()
    out, hit = [], []
    for g in groups:
        t = pf.read_row_group(g, columns=IDX).to_pandas()
        t = t[t.session_id.isin(sessions)]
        if len(t):
            out.append(t[t.turn_type == "user_prompt"])
            hit.append(g)
    return (pd.concat(out) if out else pd.DataFrame(columns=IDX)), hit


def full(groups, sessions):
    pf = open_pf()
    out = []
    for g in groups:
        t = pf.read_row_group(g, columns=FULL).to_pandas()
        out.append(t[t.session_id.isin(sessions)])
    return pd.concat(out) if out else pd.DataFrame(columns=FULL)


def main():
    T.mkdir(parents=True, exist_ok=True)
    eng = pd.read_parquet(E / "engineering_set.parquet", columns=["session_id"])
    test = test_cases()
    sessions = set(eng.session_id)
    tsess = set(test.session_id)
    n = open_pf().metadata.num_row_groups
    chunks = [list(range(i, n, NW)) for i in range(NW)]
    with ThreadPoolExecutor(NW) as ex:
        res = list(ex.map(lambda c: scan(c, sessions), chunks))
    ts = pd.concat([r[0] for r in res])
    ts.to_parquet(T / "prompt_ts.parquet")
    print("prompt rows", len(ts), "sessions", ts.session_id.nunique(), flush=True)
    # row groups holding test sessions: rescan cheaply (session_id only)
    def where(c):
        pf = open_pf()
        return [g for g in c if tsess & set(pf.read_row_group(g, columns=["session_id"]).column(0).to_pylist())]
    hit = sorted(g for r in [r[1] for r in res] for g in r)
    with ThreadPoolExecutor(NW) as ex:
        tg = sorted(g for part in ex.map(where, [hit[i::NW] for i in range(NW)]) for g in part)
    print("row groups with test sessions", len(tg), flush=True)
    with ThreadPoolExecutor(NW) as ex:
        rows = pd.concat(ex.map(lambda c: full(c, tsess), [tg[i::NW] for i in range(NW)]))
    rows.to_parquet(T / "test_rows.parquet")
    print("test rows", len(rows), "sessions", rows.session_id.nunique(), "of", len(tsess))


def pool():
    """User prompts of the memory pool's sessions (episode tasks): session_id scan, then content of those row groups."""
    ps = set(pd.read_parquet(T / "pool.parquet").session_id)
    n = open_pf().metadata.num_row_groups

    def where(c):
        pf = open_pf()
        return [g for g in c if ps & set(pf.read_row_group(g, columns=["session_id"]).column(0).to_pylist())]

    with ThreadPoolExecutor(NW) as ex:
        gs = sorted(g for part in ex.map(where, [list(range(i, n, NW)) for i in range(NW)]) for g in part)
    print("row groups with pool sessions", len(gs), flush=True)

    def read(c):
        pf = open_pf()
        out = []
        for g in c:
            t = pf.read_row_group(g, columns=["session_id", "turn_number", "turn_type", "content"]).to_pandas()
            out.append(t[t.session_id.isin(ps) & (t.turn_type == "user_prompt")])
        return pd.concat(out) if out else pd.DataFrame()

    with ThreadPoolExecutor(NW) as ex:
        rows = pd.concat(ex.map(read, [gs[i::NW] for i in range(NW)]))
    rows.to_parquet(T / "pool_prompts.parquet")
    print("pool prompts", len(rows), "sessions", rows.session_id.nunique(), "of", len(ps))


if __name__ == "__main__":
    pool() if sys.argv[1:] == ["pool"] else main()
