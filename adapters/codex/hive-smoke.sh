#!/usr/bin/env bash
# A codex session as an interactive hive drone (phase g G3), the real codex TUI in tmux against a local
# mock of the Responses API (hive_mock.py) with borg's installed hooks: 0 tokens, no network, nothing
# outside a fresh temp dir (CODEX_HOME, HOME, borg's store all in it).
#
#  1. "join the hive": the model runs `borg hive join`, which binds the thread codex gives the command
#     (CODEX_THREAD_ID); the join's own hook ties the drone to codex's process; the turn's stop starts
#     the drone's waiter, detached.
#  2. A task opened from a terminal: the waiter claims it and runs `codex queue`; the idle TUI starts a
#     turn with it (its prompt takes the claim up), the mock's model does the task in the drone's
#     worktree, and the stop publishes the done.
#  3. `/quit`: SessionEnd leaves (registry and node) and stops the waiter.
#  4. In codex's workspace-write sandbox the join is refused (`.git` is read-only there) and says to run
#     it outside the sandbox.
#
# Steps 1 to 3 run codex with `-s danger-full-access`, as an engineer's approval of the escalated join
# would. Every run passes `--dangerously-bypass-hook-trust`: trusting borg's hooks is the engineer's
# step (/hooks in codex), never borg's.
#
# usage: adapters/codex/hive-smoke.sh   needs codex (tested with 0.153.2), tmux, python3, git, target/debug/borg
set -euo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd)
BORG=${BORG_TEST_BIN:-$REPO/target/debug/borg}
command -v codex > /dev/null || { echo "hive-smoke: codex is not installed" >&2; exit 1; }
command -v tmux > /dev/null || { echo "hive-smoke: tmux is not installed" >&2; exit 1; }
[ -x "$BORG" ] || { echo "hive-smoke: no borg binary at $BORG (cargo build -p borg-ai)" >&2; exit 1; }

W=$(mktemp -d "${TMPDIR:-/tmp}/borg-codex-hive.XXXXXX")
TM=borg-codex-hive-$$
MOCK=""
cleanup() {
    tmux kill-session -t "$TM" 2> /dev/null || true
    [ -n "$MOCK" ] && kill "$MOCK" 2> /dev/null || true
    # A waiter left by a failed step sees codex gone and leaves within a second.
    sleep 1
    [ -n "${KEEP:-}" ] && echo "hive-smoke: kept $W" || rm -rf "$W"
}
trap cleanup EXIT
fail() {
    echo "FAIL: $*" >&2
    KEEP=1
    exit 1
}
ok() { echo "ok   $*"; }
# until SECS WHAT CMD...: CMD succeeds within SECS, else fail.
until_ok() {
    local secs=$1 what=$2
    shift 2
    for _ in $(seq $((secs * 10))); do "$@" > /dev/null 2>&1 && return 0; sleep 0.1; done
    fail "$what within ${secs}s"
}

# ---------------------------------------------------------------- the world
REPOD=$W/harbor_orion
STORE=$W/store/collectives/corp
mkdir -p "$W/home/.codex" "$W/store" "$REPOD"
git -C "$REPOD" init -q -b main
echo orion > "$REPOD/README"
git -C "$REPOD" add .
git -C "$REPOD" -c user.email=e@x -c user.name=E commit -qm start
printf '{"default":"off","collectives":[{"name":"corp","paths":["%s"],"share":"local"}]}' "$REPOD" > "$W/collectives.json"
python3 -I "$REPO/adapters/codex/hive_mock.py" "$W/port" "$W/requests" "$BORG" > "$W/mock.log" 2>&1 &
MOCK=$!
until_ok 5 "the mock's start" test -s "$W/port"

export CODEX_HOME=$W/home/.codex HOME=$W/home OPENAI_API_KEY=sk-dummy-not-real GIT_CONFIG_NOSYSTEM=1
export BORG_HOME=$W/store BORG_CONFIG=$W/collectives.json BORG_MODE=hook BORG_MATCH=bm25 BORG_PREDICT_MODEL=off
export BORG_QUEEN=off BORG_LEARN=off BORG_VECTORS=off BORG_STOP_ATTENTION=off BORG_NOTIFY=off BORG_EARLY=off
unset OPENAI_BASE_URL BORG_STATE BORG_HARNESS CLAUDE_CODE_SESSION_ID PI_SESSION_ID CODEX_THREAD_ID
cat > "$CODEX_HOME/catalog.json" << 'EOF'
{"models": [{"slug": "mock-plain", "display_name": "mock-plain", "description": null, "base_instructions": "You are a scripted mock.", "supported_reasoning_levels": [],
  "shell_type": "unified_exec", "visibility": "list", "supported_in_api": true, "priority": 1, "availability_nux": null,
  "upgrade": null, "support_verbosity": false, "default_verbosity": null, "apply_patch_tool_type": "freeform",
  "truncation_policy": {"mode": "tokens", "limit": 10000}, "experimental_supported_tools": []}]}
EOF
# No update check: its dialog would take the keys the smoke types.
cat > "$CODEX_HOME/config.toml" << EOF
check_for_update_on_startup = false
model = "mock-plain"
model_provider = "mock"
model_catalog_json = "$CODEX_HOME/catalog.json"

[model_providers.mock]
name = "mock"
base_url = "http://127.0.0.1:$(cat "$W/port")/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0

[projects."$REPOD"]
trust_level = "trusted"
EOF
"$BORG" install codex --apply --home "$W/home" > "$W/install.out"
(cd "$REPOD" && "$BORG" hive init g1 --spec "Ship the parser" --oracle "ok=test -f ok" --budget 5) > "$W/init.out"

# The environment above, for the TUI: a tmux server already running keeps its own.
export -p > "$W/env.sh"
# tui SANDBOX: the codex TUI in tmux session $TM, in the repo.
tui() {
    tmux kill-session -t "$TM" 2> /dev/null || true
    tmux new-session -d -s "$TM" -x 220 -y 50 -c "$REPOD" "bash -c '. $W/env.sh; exec codex --dangerously-bypass-hook-trust -s $1 -a never'"
    sleep 4
}
say() {
    tmux send-keys -t "$TM" "$1"
    sleep 0.5
    tmux send-keys -t "$TM" Enter
}
entry() { tail -1 "$STORE/hive/drones.local" 2> /dev/null; }
field() { entry | python3 -I -c "import json,sys; print(json.load(sys.stdin)['$1'])"; }
waiter_runs() { pgrep -f "[h]ive wait --queue --session $1" > /dev/null; }
board() { cat "$STORE/hive/g1/tasks.jsonl"; }

# ---------------------------------------------------------------- 1. join
tui danger-full-access
say "join the hive"
until_ok 30 "the join" grep -q '"harness":"codex"' "$STORE/hive/drones.local"
S=$(field session)
until_ok 30 "the waiter the stop starts" waiter_runs "$S"
[ "$(cat "/proc/$(field pid)/comm" 2> /dev/null || ps -o comm= -p "$(field pid)")" = codex ] || fail "the drone's harness is codex's process: $(entry)"
ok "join: thread $S bound (CODEX_THREAD_ID), tied to codex's process, its stop started the waiter"

# ---------------------------------------------------------------- 2. a task wakes the idle session
t0=$(date +%s%N)
(cd "$REPOD" && "$BORG" hive open g1 --id t1 --title "do t1") > /dev/null
until_ok 60 "t1 done through the woken turn" sh -c "grep -q '\"ev\":\"done\",\"task\":\"t1\"' '$STORE/hive/g1/tasks.jsonl'"
ms=$((($(date +%s%N) - t0) / 1000000))
grep -q "queued: \[borg hive g1\] Work arrived." "$STORE"/state/*.hive-wait.log || fail "the waiter queued the task: $(cat "$STORE"/state/*.hive-wait.log)"
[ ! -e "$STORE/state/$S.hive-takeup" ] || fail "the woken turn took the claim up"
python3 -I - "$W/requests" << 'EOF' || fail "the queued message reached the model as the turn's prompt"
import json, os, sys
d = sys.argv[1]
reqs = [json.load(open(os.path.join(d, f)))["body"] for f in sorted(os.listdir(d))]
user = [c.get("text", "") for r in reqs for it in r.get("input", []) if it.get("role") == "user" for c in (it.get("content") or []) if isinstance(c, dict)]
assert any(t.startswith("[borg hive g1] Work arrived.") for t in user), user[-3:]
EOF
ok "wake: t1 claimed, queued with codex queue, taken up by the woken turn and done (${ms} ms from open to done)"

# ---------------------------------------------------------------- 3. /quit
say "/quit"
until_ok 15 "the leave at SessionEnd" sh -c "tail -1 '$STORE/hive/drones.local' | grep -q '\"by\":\"leave\"'"
until_ok 15 "the waiter's end" sh -c "! pgrep -f '[h]ive wait --queue --session $S'"
board | grep -q '"note":"leave"' || fail "the node left with its last drone: $(board | tail -3)"
ok "quit: the drone left (registry and node), its waiter ended"

# ---------------------------------------------------------------- 4. the sandbox
tui workspace-write
say "join the hive"
until_ok 30 "the refused join's output" sh -c "grep -l 'outside the sandbox' '$W'/requests/*.json"
[ "$(grep -c '"by":"join"' "$STORE/hive/drones.local")" -eq 1 ] || fail "no drone made in the sandbox: $(cat "$STORE/hive/drones.local")"
ok "sandbox: the join in workspace-write is refused and says to run it outside the sandbox"
echo "hive-smoke: all passed (0 tokens; mock requests: $(ls "$W/requests" | wc -l))"
