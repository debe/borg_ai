#!/bin/bash
# reports/regress.md: builds the scripted repo (make-repo.sh), replays its whole history with
# `borg regress --replay` (dry run) and scores the links against truth.tsv. Any pair not listed
# there is not a regret. Usage: run.sh [borg binary] [work dir].
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
borg=${1:-borg}
work=${2:-$(mktemp -d)}
"$here/make-repo.sh" "$work" 2>/dev/null
cd "$work/app"
run() {
  env -i PATH="$PATH" HOME="$work/home" BORG_HOME="$work/store" BORG_STATE="$work/state" \
    BORG_CONFIG="$work/collectives.json" GIT_CONFIG_GLOBAL=/dev/null "$borg" regress \
    --replay "$(git rev-list --max-parents=0 HEAD)..HEAD" "$@"
}
run --explain > "$work/replay.txt"
run --json > "$work/replay.json"
subjects=$(git log --format='%H%x09%s')
python3 -I - "$work/replay.json" "$here/truth.tsv" "$subjects" <<'PY'
import csv, json, sys
links = json.load(open(sys.argv[1]))["links"]
truth = {(r["fix"], r["decision"]): r["verdict"] for r in csv.DictReader(open(sys.argv[2]), delimiter="\t")}
subject = dict(l.split("\t", 1) for l in sys.argv[3].splitlines())
found = set()
for kind in ("fix", "revisited"):
    rows = [l for l in links if l["strength"] == kind]
    print(f"{kind} links: {len(rows)}")
    for l in rows:
        pair = (subject[l["fix"]], subject[l["commit"]])
        verdict = truth.get(pair, "not a regret")
        found.add(pair)
        print(f"  {'+' if verdict == 'regret' else '-'} {pair[0]!r} <- {pair[1]!r} [{','.join(l.get('signals', []))}]: {verdict}")
strong = [l for l in links if l["strength"] == "fix"]
right = sum(truth.get((subject[l["fix"]], subject[l["commit"]])) == "regret" for l in strong)
reach = [p for p, v in truth.items() if v == "regret"]
hit = sum(any((subject[l["fix"]], subject[l["commit"]]) == p for l in strong) for p in reach)
print(f"precision of fix links: {right}/{len(strong)}; reachable regrets found as fix links: {hit}/{len(reach)}")
for p, v in truth.items():
    if v != "regret":
        print(f"  out of reach by design: {p[0]!r} <- {p[1]!r} ({v}){' FOUND' if p in found else ''}")
PY
echo "store after the replay: $(find "$work/store" -type f | wc -l | tr -d ' ') file(s)"
