#!/usr/bin/env bash
# Live test for opencode 2.x (written against 2.0.23, verified on 2.0.29; run.sh is the 1.x test): a private
# `opencode serve` loads the borg plugin through its v2 `setup` API, `opencode run --server` drives
# it headless, with every home, config, state and tmp dir isolated in a temp dir and a local mock
# LLM (0 tokens). Never talks to the engineer's background service: the temp service config is
# disabled, every command names the private server, and nothing reads ~/.config/opencode.
#
#   cargo build -p borg && adapters/opencode/live/run-v2.sh
#
# Passes when the plugin loaded, the prompt, a successful and a failed tool call reached borg, a lesson
# waiting in the session's queue reached the LLM right after the tool results (a steer, live mode's
# mid-turn channel), the conscience's question went back into the session (the mock LLM saw it) exactly once and its echo
# carried the borg marker, a queen question reached the LLM through a hidden synthetic input, and
# deleting the session ended it. Then it reports, without failing,
# whether a second session gets `end` when the server shuts down normally (setup's cleanup).
# A spy plugin logs every bus event the plugins see to events-v2.log, for comparing with borg.ts.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
tmp=$(mktemp -d)
port=${PORT:-18241} sport=${SERVER_PORT:-18242}
url=http://127.0.0.1:$sport
trap 'kill $mock ${server:-} 2>/dev/null || true' EXIT

export HOME=$tmp/home XDG_CONFIG_HOME=$tmp/config XDG_DATA_HOME=$tmp/data XDG_CACHE_HOME=$tmp/cache XDG_STATE_HOME=$tmp/state TMPDIR=$tmp/tmp
borg=${BORG_BIN:-$repo/target/debug/borg}
# BORG_CONFIG names no file: no collectives, no model call, nothing of the engineer's config.
export BORG_BIN=$tmp/borg-logged BORG_HOME=$tmp/borg BORG_STATE=$tmp/borg/state BORG_LEARN=off BORG_MATCH=bm25 \
  BORG_CONFIG=$tmp/no-collectives.json BORG_OPENCODE_LOG=$tmp/adapter.log SPY_LOG=$tmp/events-v2.log
unset OPENCODE_CONFIG OPENCODE_CONFIG_DIR OPENCODE_CONFIG_CONTENT OPENCODE_SERVER_PASSWORD
# The private server's password, sent by every client that names it with --server.
export OPENCODE_PASSWORD=borg-live-$$
export OPENCODE_DISABLE_MODELS_FETCH=1 OPENCODE_DISABLE_AUTOUPDATE=1 OPENCODE_DISABLE_LSP_DOWNLOAD=1 OPENCODE_DISABLE_DEFAULT_PLUGINS=1
cfg=$XDG_CONFIG_HOME/opencode
mkdir -p "$HOME" "$cfg/plugins" "$BORG_HOME" "$TMPDIR" "$tmp/work"
# Belt and braces: should a command fall back to "the background service", it starts a private
# standalone server instead of finding or spawning one.
echo '{"disabled": true}' > "$cfg/service.json"
# borg behind a wrapper that logs each hook event and its canonical JSON. The prompt event runs with
# learning on, which queen delivery needs (no model call at prompt); everything else keeps it off,
# so no background `borg learn` starts.
# At the first successful tool call the wrapper also leaves the lesson `drain` in the session's queue,
# as a live check would (a real check needs the consequence model; crates/borg/tests/live_cli.rs runs
# one on a fake): borg hands it over with that call, and the next LLM request must carry it.
cat > "$BORG_BIN" <<EOF
#!/bin/sh
in=\$(cat)
printf "%s %s\n" "\$3" "\$in" >> "$tmp/events.log"
if [ "\$3" = post_tool ] && [ ! -e "$tmp/seeded" ]; then
  sid=\$(printf '%s' "\$in" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p' | tr -cd 'A-Za-z0-9-')
  mkdir -p "$BORG_STATE" && echo "live drain" > "$BORG_STATE/\$sid.queued" && touch "$tmp/seeded"
fi
[ "\$3" = prompt ] && unset BORG_LEARN
printf '%s' "\$in" | exec "$borg" "\$@"
EOF
chmod +x "$BORG_BIN"
cp "$here/../borg.ts" "$cfg/plugins/borg.ts"
# The spy: every bus event and every prompt (with its metadata), as the v2 plugin API hands them over.
cat > "$cfg/plugins/spy.ts" <<'EOF'
import { appendFileSync } from "node:fs"
const log = (kind: string, v: unknown) => appendFileSync(process.env.SPY_LOG!, `${new Date().toISOString()} ${kind} ${JSON.stringify(v)}\n`)
export default {
  id: "spy",
  setup: async (ctx: any) => {
    await ctx.session.hook("prompt", (e: any) => log("prompt", { sessionID: e.sessionID, text: String(e.prompt?.text ?? "").slice(0, 80), metadata: e.metadata, delivery: e.delivery }))
    const stop = new AbortController()
    void (async () => {
      try {
        for await (const e of ctx.event.subscribe({ signal: stop.signal })) log("event", e)
      } catch (e) { log("error", String(e)) }
    })()
    return () => { log("cleanup", {}); stop.abort() }
  },
}
EOF
cat > "$cfg/opencode.json" <<EOF
{
  "share": "disabled",
  "update": "disable",
  "model": "mock/m",
  "providers": {
    "mock": {
      "name": "mock",
      "package": "@opencode/ai/providers/openai-compatible",
      "settings": { "baseURL": "http://127.0.0.1:$port/v1", "apiKey": "none" },
      "models": { "m": { "name": "m", "capabilities": { "tools": true, "input": ["text"], "output": ["text"] } } }
    }
  }
}
EOF
printf '%s\n' '{"id":"toctou","origin_ms":1,"question":"Is any check-then-act on an AtomicBoolean flag TOCTOU-safe across threads?"}' \
  '{"id":"drain","origin_ms":1,"question":"Does every reader see the published head before it is reused?"}' > "$BORG_HOME/lessons.jsonl"
# An open queen question for this repo: borg hands it over at the first prompt as context.
mkdir -p "$BORG_HOME/queen"
printf '{"id":"qrepo","decision_key":"open_decision:repo:x","kind":"open_decision","question":"Is the relay queue bounded?","options":["bound it at 1024","leave it unbounded"],"default":"bound it at 1024","evidence":[],"session":"seed","cwd":"%s","engineer":"e@x","ms":1}\n' "$tmp/work" > "$BORG_HOME/queen/queue.jsonl"

# The queued lesson reached the LLM in the request after a tool call, in or after its tool message, and
# before the stop's questions (whose block may name it too).
live_after_tool() {
  python3 -I -c '
import json, sys
for line in open(sys.argv[1]):
    msgs = json.loads(line)["body"].get("messages", [])
    hit = [i for i, m in enumerate(msgs) if "published head before it is reused" in json.dumps(m)]
    if hit:
        tools = [i for i, m in enumerate(msgs) if m.get("role") == "tool"]
        sys.exit(0 if tools and hit[0] >= tools[0] and "TOCTOU-safe" not in json.dumps(msgs) else 1)
sys.exit(1)' "$tmp/llm.jsonl"
}

python3 "$here/mock_llm.py" "$port" "$tmp/llm.jsonl" "$tmp/work" &
mock=$!
sleep 0.5

oc=${OPENCODE:-opencode}
cd "$tmp/work" && git init -q . && echo "fn deliver() {}" > relay.rs
# A private, long-lived server: the turn that answers borg's questions runs after `run` returned.
"$oc" serve --port "$sport" --print-logs --log-level info > "$tmp/serve.out" 2>&1 &
server=$!
for _ in $(seq 150); do grep -q "listening" "$tmp/serve.out" && break; sleep 0.2; done
grep -q "listening" "$tmp/serve.out" || { cat "$tmp/serve.out"; echo "server did not start"; exit 1; }

sid_of() { grep "^$1 " "$tmp/events.log" | grep -o '"session_id":"[^"]*"' | head -1 | cut -d'"' -f4; }
settle() { # waits until the mock LLM saw no request for 2 s
  local n=-1
  for _ in $(seq 60); do
    local m; m=$(wc -l < "$tmp/llm.jsonl" 2>/dev/null || echo 0)
    [ "$m" = "$n" ] && break
    n=$m; sleep 2
  done
}

timeout 120 "$oc" run --server "$url" "Fix the AtomicBoolean race in relay.rs" > "$tmp/run.out" 2>&1 || echo "opencode run exited $?"
# The questions go back in after the turn; wait for the follow-up turn to reach the LLM.
for _ in $(seq 75); do grep -q "TOCTOU-safe" "$tmp/llm.jsonl" 2>/dev/null && break; sleep 0.2; done
settle
asked=$(grep -c 'TOCTOU-safe' "$tmp/llm.jsonl" || true)
queen=$(grep -c 'Is the relay queue bounded' "$tmp/llm.jsonl" || true)
# Deleting the session ends it: session.deleted -> end.
sid=$(sid_of prompt)
"$oc" session delete --server "$url" "$sid" > "$tmp/delete.out" 2>&1 || { cat "$tmp/delete.out"; echo "delete failed"; }
for _ in $(seq 50); do grep -q "^end .*$sid" "$tmp/events.log" && break; sleep 0.2; done

# Observation: a second session, not deleted, then a normal server shutdown (SIGTERM).
before=$(grep -c "^prompt " "$tmp/events.log" || true)
timeout 120 "$oc" run --server "$url" "Rename deliver in relay.rs" > "$tmp/run2.out" 2>&1 || echo "second opencode run exited $?"
settle
sid2=$(grep "^prompt " "$tmp/events.log" | sed -n "$((before + 1))p" | grep -o '"session_id":"[^"]*"' | cut -d'"' -f4)
kill -TERM "$server"
for _ in $(seq 100); do kill -0 "$server" 2>/dev/null || break; sleep 0.1; done
kill -0 "$server" 2>/dev/null && { echo "server still up 10 s after SIGTERM, killing"; kill -KILL "$server"; }
wait "$server" 2>/dev/null || true
sleep 1 # a borg call started by the cleanup may still be writing

echo "--- hook events:"; cut -c1-300 "$tmp/events.log"
echo "--- adapter log:"; cat "$tmp/adapter.log" || true
echo "--- v2 events and prompts the plugins saw (type, sessionID, data excerpt):"
cut -c1-400 "$SPY_LOG" || true
echo "--- borg state ($BORG_STATE)"
cat "$BORG_STATE"/*.ctx || true
echo "--- requests the mock LLM saw that carry the conscience's question: $asked, the queen's: $queen"
if [ -n "$sid2" ] && grep -q "^end .*$sid2" "$tmp/events.log"; then
  echo "--- OBSERVATION: server shutdown (SIGTERM) sent end for the undeleted session $sid2: setup's cleanup ran"
else
  echo "--- OBSERVATION: server shutdown (SIGTERM) sent no end for the undeleted session ${sid2:-?}: setup's cleanup did not reach borg"
fi
grep -q '"cleanup"\| cleanup ' "$SPY_LOG" && echo "--- (the spy plugin's cleanup ran)" || echo "--- (the spy plugin's cleanup did not run)"
echo "--- temp dir: $tmp"
fail() { echo "FAIL: $*"; exit 1; }
! grep -q "Plugin must export a default definition" "$tmp/serve.out" "$tmp/run.out" || fail "plugin rejected"
grep -qi "plugin.*fail\|failed to load" "$tmp/serve.out" && fail "a plugin failed to load (serve.out)"
grep -q "^prompt " "$tmp/events.log" || fail "the plugin never called borg"
grep -q "Fix the AtomicBoolean race in relay.rs" "$BORG_STATE"/*.ctx || fail "prompt did not reach borg"
grep -q "^post_tool " "$tmp/events.log" || fail "no post_tool"
grep -q "^post_tool_failure " "$tmp/events.log" || fail "no post_tool_failure"
grep -q "relay.rs" "$BORG_STATE"/*.ctx || fail "tool input did not reach borg"
grep -q "#borg-asked toctou" "$BORG_STATE"/*.ctx || fail "the conscience did not ask"
[ "$asked" = 1 ] || fail "the mock LLM saw the question $asked times, not once"
# The echo of our questions came back carrying the borg metadata marker.
! grep -q "taken as their echo" "$tmp/adapter.log" || fail "the echo came back unmarked"
grep -q "borg's prompt context added" "$tmp/adapter.log" || fail "borg's prompt context was not added"
[ "$queen" -ge 1 ] || fail "the queen question never reached the LLM"
grep -q "^end .*\"$sid\"" "$tmp/events.log" || fail "deleting the session sent no end"
live_after_tool || fail "the queued lesson did not reach the LLM after a tool call"
echo PASS
