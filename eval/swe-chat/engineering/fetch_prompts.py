"""Read conversations.parquet (HF stream, or a temporary local copy; column-projected)
and keep only correction/rejection user prompts plus minimal context:
previous assistant text (tail) and the files the agent edited in the turn
before and the turn after the prompt.  Output: corrections.parquet, plus
corrections_turns.parquet = files the agent edited in every user-prompt turn."""
import sys, json, time, threading
import pyarrow.parquet as pq, pyarrow as pa
from concurrent.futures import ThreadPoolExecutor
from huggingface_hub import HfFileSystem

OUT = sys.argv[1]
URL = "datasets/SALT-NLP/SWE-chat/conversations.parquet"
LOCAL = sys.argv[2] if len(sys.argv) > 2 else None  # optional temporary local copy

def open_pf():
    if LOCAL:
        return pq.ParquetFile(LOCAL)
    return pq.ParquetFile(HfFileSystem().open(URL, "rb", block_size=8 << 20))
COLS = ["session_id", "repo_id", "turn_number", "turn_type", "prompt_pushback",
        "content", "tool_name", "file_path", "agent"]
EDIT = {"Edit", "Write", "MultiEdit", "write_file", "edit_file", "replace",
        "apply_patch", "str_replace_editor", "NotebookEdit", "edit", "write", "patch"}
KEEP = {"correction", "rejection"}
NW = 8

def work(ranges):
    pf = open_pf()
    out = []; turns = []
    sess = None; last_asst = ""; files_turn = set(); pending = []
    def close_turn():
        for r in pending:
            r["files_after"] = sorted(files_turn)
        pending.clear()
    for g in ranges:
        t = pf.read_row_group(g, columns=COLS).to_pylist()
        for r in t:
            if r["session_id"] != sess:
                if turns and turns[-1]["session_id"] == sess:
                    turns[-1]["files"] = json.dumps(sorted(files_turn))
                close_turn(); sess = r["session_id"]; last_asst = ""; files_turn = set()
            tt = r["turn_type"]
            if tt == "assistant_response" and r["content"]:
                last_asst = r["content"]
            elif tt == "tool_use" and r["file_path"] and r["tool_name"] in EDIT:
                files_turn.add(r["file_path"])
            elif tt == "user_prompt":
                prev_files = sorted(files_turn)
                if turns and turns[-1]["session_id"] == sess:
                    turns[-1]["files"] = json.dumps(prev_files)
                turns.append(dict(session_id=sess, turn_number=r["turn_number"], files="[]"))
                close_turn(); files_turn = set()
                if r["prompt_pushback"] in KEEP:
                    rec = dict(session_id=r["session_id"], repo_id=r["repo_id"],
                               turn_number=r["turn_number"], agent=r["agent"],
                               pushback=r["prompt_pushback"],
                               content=(r["content"] or "")[:6000],
                               prev_assistant=last_asst[-1200:],
                               files_before=prev_files, files_after=[], rg=g)
                    out.append(rec); pending.append(rec)
                last_asst = ""
        print(g, len(out), flush=True)
    close_turn()
    if turns: turns[-1]["files"] = json.dumps(sorted(files_turn))
    return out, turns

def main():
    n = open_pf().metadata.num_row_groups
    step = (n + NW - 1) // NW
    chunks = [range(i, min(n, i + step)) for i in range(0, n, step)]
    t = time.time()
    with ThreadPoolExecutor(NW) as ex:
        res = list(ex.map(work, chunks))
    rows = [r for part, _ in res for r in part]
    pq.write_table(pa.Table.from_pylist([t for _, ts in res for t in ts]), OUT.replace(".parquet", "_turns.parquet"))
    for r in rows:
        r["files_before"] = json.dumps(r["files_before"]); r["files_after"] = json.dumps(r["files_after"])
    pq.write_table(pa.Table.from_pylist(rows), OUT)
    print("done", len(rows), round(time.time() - t), "s")

if __name__ == "__main__":
    main()
