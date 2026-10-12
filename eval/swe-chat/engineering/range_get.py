"""Parallel HTTP range download of one HF dataset file (temporary local copy)."""
import sys, os, threading, requests
from concurrent.futures import ThreadPoolExecutor
from huggingface_hub import get_token
url, out = sys.argv[1], sys.argv[2]
H = {"Authorization": f"Bearer {get_token()}"}
size = int(requests.head(url, headers=H, allow_redirects=True).headers["Content-Length"])
with open(out, "wb") as f: f.truncate(size)
CH = 32 << 20
done = [0]; lock = threading.Lock()
def get(off):
    end = min(size, off + CH) - 1
    for _ in range(5):
        try:
            r = requests.get(url, headers={**H, "Range": f"bytes={off}-{end}"}, timeout=120)
            assert len(r.content) == end - off + 1
            break
        except Exception: continue
    else: raise RuntimeError(off)
    with open(out, "r+b") as f: f.seek(off); f.write(r.content)
    with lock:
        done[0] += 1
        if done[0] % 10 == 0: print(done[0], "/", (size + CH - 1) // CH, flush=True)
with ThreadPoolExecutor(16) as ex: list(ex.map(get, range(0, size, CH)))
print("ok", size)
