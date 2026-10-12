#!/usr/bin/env bash
# Live test: a real `opencode serve`, driven headless by `opencode run --attach`, loads the borg
# plugin, with every home and config dir isolated in a temp dir and a local mock LLM (0 tokens).
# Never touches ~/.config/opencode. The first start installs @opencode-ai/plugin from npm into the
# temp config dir (opencode does this for any plugin); when npm hangs, the run times out.
#
#   cargo build -p borg && adapters/opencode/live/run.sh
#
# Passes when the prompt, a successful and a failed tool call reached borg, the conscience's question
# went back into the session (the mock LLM saw it) once, and deleting the session ended it.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
tmp=$(mktemp -d)
port=${PORT:-18231} sport=${SERVER_PORT:-18232}
trap 'kill $mock ${server:-} 2>/dev/null || true' EXIT

export HOME=$tmp/home XDG_CONFIG_HOME=$tmp/config XDG_DATA_HOME=$tmp/data XDG_CACHE_HOME=$tmp/cache XDG_STATE_HOME=$tmp/state
borg=${BORG_BIN:-$repo/target/debug/borg}
export BORG_BIN=$tmp/borg-logged BORG_HOME=$tmp/borg BORG_STATE=$tmp/borg/state BORG_LEARN=off BORG_OPENCODE_LOG=$tmp/adapter.log
unset OPENCODE_CONFIG OPENCODE_CONFIG_DIR OPENCODE_CONFIG_CONTENT
# Less network: no models.dev fetch, updates, LSP downloads or default (auth) plugins.
export OPENCODE_DISABLE_MODELS_FETCH=1 OPENCODE_DISABLE_AUTOUPDATE=1 OPENCODE_DISABLE_LSP_DOWNLOAD=1 OPENCODE_DISABLE_DEFAULT_PLUGINS=1
mkdir -p "$HOME" "$XDG_CONFIG_HOME/opencode/plugins" "$BORG_HOME" "$tmp/work"
# borg behind a wrapper that logs each hook event and its canonical JSON.
printf '#!/bin/sh\nin=$(cat)\nprintf "%%s %%s\\n" "$3" "$in" >> "%s"\nprintf "%%s" "$in" | exec "%s" "$@"\n' "$tmp/events.log" "$borg" > "$BORG_BIN"
chmod +x "$BORG_BIN"
cp "$here/../borg.ts" "$XDG_CONFIG_HOME/opencode/plugins/borg.ts"
cat > "$XDG_CONFIG_HOME/opencode/opencode.json" <<EOF
{
  "\$schema": "https://opencode.ai/config.json",
  "autoupdate": false,
  "share": "disabled",
  "model": "mock/m",
  "small_model": "mock/m",
  "provider": {
    "mock": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "mock",
      "options": { "baseURL": "http://127.0.0.1:$port/v1", "apiKey": "none" },
      "models": { "m": { "name": "m", "tool_call": true } }
    }
  }
}
EOF
echo '{"id":"toctou","origin_ms":1,"question":"Is any check-then-act on an AtomicBoolean flag TOCTOU-safe across threads?"}' > "$BORG_HOME/lessons.jsonl"

python3 "$here/mock_llm.py" "$port" "$tmp/llm.jsonl" "$tmp/work" &
mock=$!
sleep 0.5

oc=${OPENCODE:-opencode}
cd "$tmp/work" && git init -q . && echo "fn deliver() {}" > relay.rs
# A long-lived server, as under the TUI: plain `opencode run` exits at the first idle, before the
# turn that answers borg's questions. `run --attach` drives it headless.
"$oc" serve --port "$sport" > "$tmp/serve.out" 2>&1 &
server=$!
for _ in $(seq 100); do grep -q listening "$tmp/serve.out" && break; sleep 0.2; done
timeout 120 "$oc" run --attach "http://127.0.0.1:$sport" --dir "$tmp/work" "Fix the AtomicBoolean race in relay.rs" > "$tmp/run.out" 2>&1 || echo "opencode run exited $?"
# The questions go back in after the idle; wait for the follow-up turn to reach the LLM.
for _ in $(seq 50); do grep -q "TOCTOU-safe" "$tmp/llm.jsonl" 2>/dev/null && break; sleep 0.2; done
sleep 1
# Deleting the session ends it: session.deleted -> end.
sid=$(grep -o "ses_[A-Za-z0-9]*" "$tmp/adapter.log" | head -1)
curl -sf --max-time 10 -X DELETE "http://127.0.0.1:$sport/session/$sid?directory=$tmp/work" > /dev/null || echo "delete failed"
sleep 1
kill "$server"; wait "$server" || true

echo "--- hook events:"; cut -c1-300 "$tmp/events.log"
echo "--- adapter log:"; cat "$tmp/adapter.log" || true
echo "--- borg state ($BORG_STATE)"
cat "$BORG_STATE"/*.ctx || true
echo "--- requests the mock LLM saw that carry the conscience's question: $(grep -c 'TOCTOU-safe' "$tmp/llm.jsonl" || true)"
echo "--- temp dir: $tmp"
grep -q "Fix the AtomicBoolean race in relay.rs" "$BORG_STATE"/*.ctx
grep -q "#borg-asked toctou" "$BORG_STATE"/*.ctx
grep -q "TOCTOU-safe" "$tmp/llm.jsonl"
grep -q "^end " "$tmp/events.log"
grep -q "^post_tool " "$tmp/events.log"
grep -q "^post_tool_failure " "$tmp/events.log"
grep -q "relay.rs" "$BORG_STATE"/*.ctx
# The echo of our questions came back carrying the borg metadata marker.
! grep -q "taken as their echo" "$tmp/adapter.log"
echo PASS
