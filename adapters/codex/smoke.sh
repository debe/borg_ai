#!/usr/bin/env bash
# A real Codex CLI run through borg's installed hooks, against a local mock of the Responses API:
# 0 tokens, no network, nothing outside a fresh temp dir (CODEX_HOME, HOME, borg's store all in it).
#
#  1. `borg install codex --apply --home`, beside a hook of the engineer's own (a marker).
#  2. `codex exec` through a scripted turn: the prompt, an apply_patch edit, a slow read, the final
#     message. The edit starts borg's live check (a fake `claude` answers it: Mdrain at grade 3), and the
#     read after it delivers the ask. Asserted: the ask reached the model (the mock's request log), the
#     edit's path and text are in borg's state, the file changed, the engineer's hook ran.
# 2b. A second turn in hook mode: borg blocks the first stop with its question (Mdrain), codex runs one
#     more round, and the second stop (stop_hook_active) passes: exactly 2 model requests, one hook_prompt.
#  3. borg's own model call through codex (`BORG_PREDICT_MODEL=codex:…`) runs no hook, although
#     hooks.json holds borg's and the marker, and a shim forces the trust bypass on top: only
#     `-c features.hooks=false` keeps them out.
#  4. `borg doctor --home` asks codex which of borg's hooks it does not trust: all of them, here.
#
# The turns pass `--dangerously-bypass-hook-trust`: codex skips untrusted hooks silently, and trusting
# them is the engineer's step (/hooks in codex), never borg's. Step 3's shim also forces it onto borg's
# own codex call, to show `features.hooks=false` alone keeps the hooks out. borg itself never passes it
# (runner.rs).
#
# usage: adapters/codex/smoke.sh      needs codex (tested with 0.153.2), python3, target/debug/borg
set -euo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd)
BORG=${BORG_TEST_BIN:-$REPO/target/debug/borg}
REAL_CODEX=$(command -v codex) || { echo "smoke: codex is not installed" >&2; exit 1; }
[ -x "$BORG" ] || { echo "smoke: no borg binary at $BORG (cargo build -p borg)" >&2; exit 1; }

W=$(mktemp -d "${TMPDIR:-/tmp}/borg-codex-smoke.XXXXXX")
PIDS=()
cleanup() {
    for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null || true; done
    [ -n "${KEEP:-}" ] && echo "smoke: kept $W" || rm -rf "$W"
}
trap cleanup EXIT

fail() {
    echo "FAIL: $*" >&2
    KEEP=1
    exit 1
}
ok() { echo "ok   $*"; }

# ---------------------------------------------------------------- the world
REPOD=$W/harbor_orion
mkdir -p "$W/home" "$W/bin" "$W/shim" "$W/prompts" "$W/store/collectives/corp" "$REPOD/src"
git -C "$REPOD" init -q
printf 'fn net() {\n    drain the budget\n}\n' > "$REPOD/src/net.rs"
echo '{"id":"Mdrain","origin_ms":1,"scope":["Orion"],"question":"Does the Orion net drain the budget place with an explicit consumer instead of a reset arc?","consequence":"a reset arc drops the budget token"}' \
    > "$W/store/collectives/corp/lessons.jsonl"
printf '{"default":"off","collectives":[{"name":"corp","paths":["%s"],"share":"local"}]}' "$REPOD" > "$W/collectives.json"
# The fake consequence model (crates/borg/tests/common): Mdrain at grade 3.
cat > "$W/bin/claude" <<EOF
#!/bin/sh
n=\$(ls $W/prompts | wc -l)
printf '%s' "\$2" > $W/prompts/\$n.txt
echo '{"structured_output":{"moves":[],"applies":[{"id":"Mdrain","why":"w","grade":3}]},"total_cost_usd":0.05}'
EOF
chmod +x "$W/bin/claude"

# mock NAME SCENARIO_JSON: starts a mock on a free port, written to $W/NAME.port.
mock() {
    printf '%s' "$2" > "$W/$1.scenario.json"
    python3 -I "$REPO/adapters/codex/mock.py" "$W/$1.port" "$W/$1.scenario.json" "$W/$1.requests" > "$W/$1.log" 2>&1 &
    PIDS+=($!)
    for _ in $(seq 100); do [ -s "$W/$1.port" ] && return; sleep 0.05; done
    fail "mock $1 did not start: $(cat "$W/$1.log")"
}

PATCH='*** Begin Patch\n*** Update File: src/net.rs\n@@\n-    drain the budget\n+    use a reset arc on the budget place\n*** End Patch\n'
mock turn "[
 [{\"type\":\"custom_tool_call\",\"call_id\":\"call_patch\",\"name\":\"apply_patch\",\"input\":\"$PATCH\"}],
 [{\"type\":\"function_call\",\"call_id\":\"call_read\",\"name\":\"exec_command\",\"arguments\":\"{\\\"cmd\\\":\\\"sleep 3; cat src/net.rs\\\"}\"}],
 [{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"I replaced the drain of the Orion budget place with a reset arc.\"}]}]
]"
# Step 2b, in hook mode: the final message, the drone's answer to borg's block, then nothing more is due.
SAY() { printf '[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"%s"}]}]' "$1"; }
mock stopturn "[$(SAY "I replaced the drain of the Orion budget place with a reset arc."), $(SAY "Mdrain: applies, I keep an explicit consumer."), $(SAY "extra"), $(SAY "extra")]"
mock judge '[[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"{\"moves\":[],\"applies\":[{\"id\":\"Mdrain\",\"why\":\"w\",\"grade\":3}]}"}]}]]'
TURN=$(cat "$W/turn.port") JUDGE=$(cat "$W/judge.port") STOPTURN=$(cat "$W/stopturn.port")

export CODEX_HOME=$W/home/.codex
mkdir -p "$CODEX_HOME"
# The mock model's metadata: plain tool mode, apply_patch as a freeform tool (an unknown model would get
# fallback metadata without it).
cat > "$CODEX_HOME/catalog.json" <<'EOF'
{"models": [{"slug": "mock-plain", "display_name": "mock-plain", "description": null, "base_instructions": "You are a scripted mock.", "supported_reasoning_levels": [],
  "shell_type": "unified_exec", "visibility": "list", "supported_in_api": true, "priority": 1, "availability_nux": null,
  "upgrade": null, "support_verbosity": false, "default_verbosity": null, "apply_patch_tool_type": "freeform",
  "truncation_policy": {"mode": "tokens", "limit": 10000}, "experimental_supported_tools": []}]}
EOF
cat > "$CODEX_HOME/config.toml" <<EOF
model = "mock-plain"
model_provider = "mock"
model_catalog_json = "$CODEX_HOME/catalog.json"

[model_providers.mock]
name = "mock"
base_url = "http://127.0.0.1:$TURN/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0

[projects."$REPOD"]
trust_level = "trusted"
EOF
# The engineer's own hooks, which borg must keep (and which mark every run they fire in).
cat > "$CODEX_HOME/hooks.json" <<EOF
{"hooks": {
  "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "sh -c 'cat >/dev/null; echo prompt >> $W/markers'"}]}],
  "Stop": [{"hooks": [{"type": "command", "command": "sh -c 'cat >/dev/null; echo stop >> $W/markers'"}]}]
}}
EOF

# Everything borg's hooks inherit through codex: the store, the fake model, live mode at once.
export PATH=$W/bin:$PATH HOME=$W/home GIT_CONFIG_NOSYSTEM=1 OPENAI_API_KEY=sk-dummy-not-real
export BORG_BIN=$BORG BORG_HOME=$W/store BORG_CONFIG=$W/collectives.json BORG_PREDICT_MODEL=claude:sonnet
export BORG_MODE=live BORG_LIVE_DEBOUNCE_S=0 BORG_QUEEN=off BORG_LEARN=off BORG_VECTORS=off BORG_EARLY=off BORG_NOTIFY=off
unset OPENAI_BASE_URL BORG_STATE BORG_HARNESS BORG_STOP_BUDGET_MS

# ---------------------------------------------------------------- 1. install
"$BORG" install codex --apply --home "$W/home" > "$W/install.out"
grep -q "run /hooks to trust borg's hooks" "$W/install.out" || fail "install names the trust step: $(cat "$W/install.out")"
python3 -I - "$CODEX_HOME/hooks.json" "$BORG" <<'EOF' || fail "hooks.json: $(cat "$CODEX_HOME/hooks.json")"
import json, sys
h = json.load(open(sys.argv[1]))["hooks"]
want = {"UserPromptSubmit": ("prompt", 30), "PreToolUse": ("pre_tool", 10), "PostToolUse": ("post_tool", 30), "Stop": ("stop", 90), "SessionEnd": ("end", 3)}
for native, (ev, t) in want.items():
    ours = [x for g in h[native] for x in g["hooks"] if x["command"] == f"{sys.argv[2]} hook codex {ev}"]
    assert len(ours) == 1 and ours[0]["timeout"] == t, (native, ours)
assert "echo prompt" in h["UserPromptSubmit"][0]["hooks"][0]["command"] and "echo stop" in h["Stop"][0]["hooks"][0]["command"], "theirs first, kept"
EOF
ok "install: borg's 5 hooks appended with their timeouts, the engineer's kept first"

# ---------------------------------------------------------------- 2. a turn through codex
(cd "$REPOD" && timeout 120 codex exec --dangerously-bypass-hook-trust --skip-git-repo-check -s workspace-write --json \
    "Tidy the budget handling of the Orion net" < /dev/null > "$W/turn.jsonl" 2> "$W/turn.err") || fail "codex exec: $(tail -5 "$W/turn.err")"
STATE=$W/store/collectives/corp/state
EDITS=$(cat "$STATE"/*.edits 2>/dev/null || true)
CTX=$(cat "$STATE"/*.ctx 2>/dev/null || true)
grep -q "reset arc" "$REPOD/src/net.rs" || fail "the patch was applied: $(cat "$REPOD/src/net.rs")"
[[ $EDITS == *"$REPOD/src/net.rs"* && $EDITS == *"use a reset arc on the budget place"* ]] || fail ".edits has the edit's path and text: $EDITS"
ok "apply_patch: the edit's path and text are in borg's .edits"
[[ $CTX == *"#borg-asked Mdrain"* && $CTX == *"#borg-live Mdrain"* ]] || fail "the live check asked Mdrain: $CTX"
ok "live check: Mdrain asked during the turn ($(ls "$W/prompts" | wc -l) fake model call(s))"
python3 -I - "$W/turn.requests" <<'EOF' || fail "borg's ask did not reach the model"
import json, os, sys
d = sys.argv[1]
reqs = [json.load(open(os.path.join(d, f)))["body"] for f in sorted(os.listdir(d))]
dev = [c["text"] for r in reqs for it in r.get("input", []) if it.get("role") == "developer" for c in it.get("content", []) if "text" in c]
hit = [t for t in dev if "borg check of your work so far" in t and "[Mdrain]" in t]
assert hit, dev
print(f"     request log: {len(reqs)} model requests, borg's ask in a developer message ({len(hit[0])} chars)")
EOF
ok "delivery: borg's ask reached the model's next request"
grep -q '^prompt$' "$W/markers" && grep -q '^stop$' "$W/markers" || fail "the engineer's hooks ran: $(cat "$W/markers" 2>/dev/null)"
ok "the engineer's own hooks ran beside borg's"

# ---------------------------------------------------------------- 2b. a blocking stop through codex
# Hook mode: borg blocks the first stop with its question, codex starts one more round, and borg
# answers nothing to the second stop (stop_hook_active), which codex itself never caps.
: > "$W/markers"
(cd "$REPOD" && BORG_MODE=hook BORG_PREFILTER=off timeout 120 codex exec --dangerously-bypass-hook-trust --skip-git-repo-check -s workspace-write --json \
    -c "model_providers.mock={name=\"mock\",base_url=\"http://127.0.0.1:$STOPTURN/v1\",wire_api=\"responses\",request_max_retries=0,stream_max_retries=0}" \
    "Tidy the Orion budget place" < /dev/null > "$W/stopturn.jsonl" 2> "$W/stopturn.err") || fail "codex exec (stop): $(tail -5 "$W/stopturn.err")"
python3 -I - "$W/stopturn.requests" <<'EOF' || fail "the stop's block did not round-trip through codex"
import json, os, sys
d = sys.argv[1]
reqs = [json.load(open(os.path.join(d, f)))["body"] for f in sorted(os.listdir(d))]
reqs = [r for r in reqs if "input" in r]
texts = lambda r: [c.get("text", "") for it in r["input"] for c in (it.get("content") or []) if isinstance(c, dict)]
assert len(reqs) == 2, f"{len(reqs)} model requests, want 2 (one round, then the turn ends)"
hit = [t for t in texts(reqs[1]) if "hook_prompt" in t and "[Mdrain]" in t]
assert len(hit) == 1, texts(reqs[1])[-3:]
assert not any("hook_prompt" in t for t in texts(reqs[0])), "no block before the first stop"
EOF
[ "$(grep -c '^stop$' "$W/markers")" -eq 2 ] || fail "the engineer's stop hook ran at both stops: $(cat "$W/markers")"
ok "stop: borg blocked once with [Mdrain], the second stop (stop_hook_active) passed, 2 model requests"

# ---------------------------------------------------------------- 3. borg's own call through codex: no hook
: > "$W/markers"
cat > "$W/shim/codex" <<EOF
#!/bin/sh
# Records borg's arguments, then runs them against the judge mock with the trust bypass forced on.
printf '%s\n' "\$@" > $W/runner-args
shift
exec "$REAL_CODEX" exec -c model_provider=mock -c 'model_providers.mock={name="mock",base_url="http://127.0.0.1:$JUDGE/v1",wire_api="responses"}' --dangerously-bypass-hook-trust "\$@"
EOF
chmod +x "$W/shim/codex"
before=$(ls "$STATE"/*.ctx | wc -l)
hook() { printf '%s' "$2" | PATH=$W/shim:$PATH BORG_MODE=hook BORG_PREFILTER=off BORG_PREDICT_MODEL=codex:mock-plain "$BORG" hook codex "$1"; }
S='"session_id":"smoke-runner","cwd":"'$REPOD'"'
hook prompt "{$S,\"hook_event_name\":\"UserPromptSubmit\",\"prompt\":\"Tidy the Orion net\"}" > /dev/null
hook post_tool "{$S,\"hook_event_name\":\"PostToolUse\",\"tool_name\":\"apply_patch\",\"tool_input\":{\"command\":\"*** Begin Patch\\n*** Update File: src/net.rs\\n@@\\n+reset arc\\n*** End Patch\\n\"},\"tool_response\":\"ok\"}" > /dev/null
OUT=$(hook stop "{$S,\"hook_event_name\":\"Stop\",\"stop_hook_active\":false,\"last_assistant_message\":\"Used a reset arc.\"}")
[[ $OUT == *'"decision":"block"'* && $OUT == *"[Mdrain]"* ]] || fail "the stop's model call went through codex: $OUT"
grep -qx 'features.hooks=false' "$W/runner-args" && grep -qx -- '--ignore-user-config' "$W/runner-args" || fail "runner args: $(cat "$W/runner-args")"
! grep -q bypass "$W/runner-args" || fail "borg passed the trust bypass: $(cat "$W/runner-args")"
[ ! -s "$W/markers" ] || fail "a hook fired inside borg's own codex call: $(cat "$W/markers")"
[ "$(ls "$STATE"/*.ctx | wc -l)" -eq $((before + 1)) ] || fail "borg's hooks ran for the runner's codex session: $(ls "$STATE")"
[ -n "$(ls "$W/judge.requests")" ] || fail "the judge mock was not asked"
ok "runner: borg's own codex call (features.hooks=false, trust bypass forced) fired no hook and blocked with [Mdrain]"

# ---------------------------------------------------------------- 4. doctor: trust, as codex reports it
DOC=$(cd "$REPOD" && "$BORG" doctor --home "$W/home" 2>&1 || true)
[[ $DOC == *"WARN codex: skips borg's hooks silently"*"run /hooks"* ]] || fail "doctor reports the untrusted hooks: $DOC"
ok "doctor: $(grep "skips borg's hooks" <<< "$DOC" | cut -c1-110)…"
echo "smoke: all passed (0 tokens; mock requests: turn $(ls "$W/turn.requests" | wc -l), judge $(ls "$W/judge.requests" | wc -l))"
