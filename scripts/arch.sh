#!/usr/bin/env bash
# The harness abstraction, measured (docs/harnesses.md): a before/after table of crates/borg/src at a
# base commit (default bcebe3c, the last commit before the harness trait; ARCH_BASE overrides) and the
# working tree, and the gates. Fails when a gate fails:
#
#   M1   harness-name branches outside src/harness/, the deferred
#        runner.rs and assimilate*.rs aside (reported apart)           0
#   M2   places listing harness names outside src/harness/ (runner.rs,
#        borg's model runners, aside)                                  0  (harness::ALL is the one)
#   M5   dependency cycles through harness::*                          0  (cargo-coupling)
#   M7   CCN of any function in src/harness/                          <= 10 (lizard)
#   M9   CCN of the hook's per-event functions (hook.rs)               <= 15 (lizard)
#   M10  functions in install.rs, tests aside                          < 40 (cargo-coupling)
#
# The bars are baselines: design-chosen when the harness trait landed (docs/harnesses.md), not tuned;
# 0 for M1, M2 and M5 is the definition of the abstraction, the CCN and size bars a ceiling on regress.
#
# M1 and M2 need nothing but grep and awk (crates/borg/tests/harness_arch.rs runs them in cargo test too).
# A missing tool (lizard, cargo-coupling, rust-code-analysis-cli) skips its rows with a note. 0 tokens.
#
# usage: scripts/arch.sh
set -uo pipefail
cd "$(dirname "$0")/.."

BASE=${ARCH_BASE:-bcebe3c}
W=$(mktemp -d "${TMPDIR:-/tmp}/borg-arch.XXXXXX")
trap 'rm -rf "$W"' EXIT
# The base as a whole tree: cargo-coupling resolves paths through the workspace (outside one it would
# bind `serde_json::Map` to a `Map` of borg's own, a false edge). A history without the base (a fresh
# clone after the release) has no before column: its cells read "-", the gates still run.
if git cat-file -e "$BASE^{commit}" 2>/dev/null; then
    git archive "$BASE" | tar -x -C "$W"
else
    echo "arch: base $BASE not in this history (set ARCH_BASE to a commit): before column skipped" >&2
    BASE="-"
fi
BEFORE=$W/crates/borg/src
AFTER=crates/borg/src
NAMES='claude|omp|opencode|codex|sim'
have() { command -v "$1" >/dev/null 2>&1; }
rows=()
row() { rows+=("$(printf '%-58s %10s %10s  %s' "$1" "$2" "$3" "$4")"); }
gate() { # gate NAME VALUE OK-EXPR (awk on v): pass or FAIL
    if awk -v v="$2" "BEGIN { exit !($3) }"; then echo pass; else echo FAIL; fi
}

VARIANTS='Claude|Omp|Opencode|Codex|Sim'
# M1: a comparison or match arm on a harness name, or on an enum variant named after a harness (any
# enum's). The deferred follow-ups (harness_arch.rs `M1_DEFERRED`) are counted apart (m1 DIR deferred).
m1() {
    grep -rnE "\"($NAMES)\"\\s*(=>|\\||==)|(==|!=)\\s*\"($NAMES)\"|\\b\\w+::($VARIANTS)\\s*=>|(==|!=)\\s*\\w+::($VARIANTS)\\b" "$1" --include=*.rs |
        grep -v '/harness/' | { if [ "${2:-}" = deferred ]; then grep -E '/(runner|assimilate|assimilate_dir)\.rs:'; else grep -vE '/(runner|assimilate|assimilate_dir)\.rs:'; fi; } | wc -l
}
# A cell of the before column: "-" without a base.
before() { if [ "$BASE" = - ]; then echo -; else "$@"; fi; }

# M2: files outside harness/ whose code (before its unit tests) maps a name (`=> "omp"`) or lists two names.
m2() {
    find "$1" -name '*.rs' -not -path '*/harness/*' -not -name runner.rs | sort | while read -r f; do
        awk -v n="$NAMES" '/^#\[cfg\(test\)\]/ { exit } {
            k = 0; split(n, ns, "|")
            for (i in ns) if (index($0, "\"" ns[i] "\"")) k++
            if (k >= 2 || $0 ~ ("=> *\"(" n ")\" *,? *$")) { print FILENAME; exit }
        }' "$f"
    done | wc -l
}

v1=$(before m1 "$BEFORE"); a1=$(m1 "$AFTER")
row "M1 harness-name branches outside harness/" "$v1" "$a1" "$(gate M1 "$a1" 'v == 0')"
row "  runner.rs and assimilate*.rs (deferred, not in M1)" "$(before m1 "$BEFORE" deferred)" "$(m1 "$AFTER" deferred)" "report"
v2=$(before m2 "$BEFORE"); a2=$(m2 "$AFTER")
row "M2 files listing harness names outside harness/" "$v2" "$a2" "$(gate M2 "$a2" 'v == 0')"

if have lizard; then
    # fn@file -> CCN, from lizard's CSV (nloc,ccn,token,param,length,location,file,function,...)
    liz() { lizard -l rust --csv "$@" 2>/dev/null; }
    ccn_of() { liz "$1" | awk -F, -v f="$2" '{ gsub(/"/, "", $8) } $8 ~ ("^" f "$") { if ($2 > m) m = $2 } END { print m + 0 }'; }
    vh=$(before ccn_of "$BEFORE/main.rs" 'claude_hook')
    ah=$(ccn_of "$AFTER/hook.rs" '(dispatch|on_[a-z_]+|stop_[a-z_]+)')
    row "M9 max CCN of the hook's per-event fns" "$vh" "$ah" "$(gate M9 "$ah" 'v <= 15')"
    a7=$(liz "$AFTER/harness" | awk -F, '{ if ($2 > m) m = $2 } END { print m + 0 }')
    row "M7 max CCN in harness/" "-" "$a7" "$(gate M7 "$a7" 'v <= 10')"
    warn() { lizard -l rust -w -C 15 "$1" 2>/dev/null | grep -c warning; }
    row "lizard: functions with CCN > 15 in crates/borg/src" "$(before warn "$BEFORE")" "$(warn "$AFTER")" "report"
    tot() { liz "$@" | awk -F, '{ n += $1; c += $2; k++ } END { printf "%d/%.1f/%d", n, c / k, k }'; }
    row "lizard: main+install(+hook+harness) fn NLOC/avg CCN/fns" "$(before tot "$BEFORE/main.rs" "$BEFORE/install.rs")" \
        "$(tot "$AFTER/main.rs" "$AFTER/install.rs" "$AFTER/hook.rs" "$AFTER/harness")" "report"
else
    row "M7, M9 (lizard not installed: pip install lizard)" "-" "-" "skipped"
fi

if have rust-code-analysis-cli && have python3; then
    cog() {
        rust-code-analysis-cli -m -O json -p "$1" 2>/dev/null | python3 -c '
import json, re, sys
pat = re.compile(sys.argv[1]); best = 0
def walk(n):
    global best
    if n.get("kind") == "function" and pat.fullmatch(n.get("name", "")): best = max(best, n["metrics"]["cognitive"]["sum"])
    for c in n.get("spaces", []): walk(c)
for line in sys.stdin.read().splitlines():
    if line.strip(): walk(json.loads(line))
print(int(best))' "$2"
    }
    row "rca: max cognitive of the hook's per-event fns" "$(before cog "$BEFORE/main.rs" claude_hook)" "$(cog "$AFTER/hook.rs" '(dispatch|on_[a-z_]+|stop_[a-z_]+)')" "report"
else
    row "rca (rust-code-analysis-cli not installed)" "-" "-" "skipped"
fi

if cargo coupling --help >/dev/null 2>&1 && have python3; then
    if [ "$BASE" != - ]; then
        (cd "$W" && cargo coupling --no-git --json crates/borg/src) > "$W/before.json" 2>/dev/null
        (cd "$W" && cargo coupling --no-git --exclude-tests --json crates/borg/src) > "$W/before-nt.json" 2>/dev/null
    fi
    cargo coupling --no-git --json "$AFTER" > "$W/after.json" 2>/dev/null
    cargo coupling --no-git --exclude-tests --json "$AFTER" > "$W/after-nt.json" 2>/dev/null
    q() {
        python3 - "$@" <<'EOF'
import json, re, sys
d = json.load(open(sys.argv[1])); what = sys.argv[2]
cyc = d["circular_dependencies"]
if what == "grade": print(d["summary"]["health_grade"])
elif what == "cycles": print(len(cyc))
elif what == "harness_cycles": print(sum(any("harness" in m for m in c) for c in cyc))
elif what.startswith("fns:"):
    mod = what[4:]
    n = [int(re.match(r"(\d+) functions", i["target"]).group(1)) for i in d["issues"] if i["issue_type"] == "God Module" and i["source"] == mod]
    if n: print(n[0])
    else: print(next((f"<30" for m in d["modules"] if m["name"] == mod), "-"))
EOF
    }
    a5=$(q "$W/after.json" harness_cycles)
    row "M5 dependency cycles through harness::*" "-" "$a5" "$(gate M5 "$a5" 'v == 0')"
    a10=$(q "$W/after-nt.json" fns:install)
    row "M10 functions in install.rs (tests aside)" "$(before q "$W/before-nt.json" fns:install)" "$a10" "$(gate M10 "${a10/<30/0}" 'v < 40')"
    row "cargo-coupling: functions in main.rs (tests aside)" "$(before q "$W/before-nt.json" fns:main)" "$(q "$W/after-nt.json" fns:main)" "report"
    row "cargo-coupling: functions in hook.rs (tests aside)" "-" "$(q "$W/after-nt.json" fns:hook)" "report"
    row "cargo-coupling: grade / cycle paths" "$(before q "$W/before.json" grade)/$(before q "$W/before.json" cycles)" "$(q "$W/after.json" grade)/$(q "$W/after.json" cycles)" "report"
else
    row "M5, M10 (cargo-coupling not installed: cargo install cargo-coupling)" "-" "-" "skipped"
fi

printf '%-58s %10s %10s  %s\n' "metric (crates/borg/src)" "$BASE" "now" "gate"
printf '%s\n' "${rows[@]}"
if printf '%s\n' "${rows[@]}" | grep -q ' FAIL$'; then
    echo "arch: a gate failed"
    exit 1
fi
