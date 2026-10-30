#!/usr/bin/env bash
# One headless omp run with the borg extension, fully isolated and free: a temp HOME and agent dir
# (PI_CODING_AGENT_DIR), a temp borg store, a local mock model and word matching at the stop (no
# consequence-model call). Never touches ~/.omp.
# Proves: the extension loads, the prompt and a tool touch reach borg, a live check's question waiting
# for that tool call reaches the model's next request after the tool output (tool_result
# additionalContext), borg's questions block the first stop (omp continues the turn with them) and the
# second stop ends the run.
# Usage: cargo build -p borg-ai && adapters/omp/smoke/smoke.sh < /dev/null   (needs omp and python3;
# without the redirect `omp -p` waits for piped input)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
omp=${OMP:-$(command -v omp)}
port=${PORT:-18931}
run=$(mktemp -d)
mkdir -p "$run/home" "$run/agent" "$run/repo/src" "$run/state" "$run/borg"
echo 'fn deliver() {}' > "$run/repo/src/relay.rs"
cat > "$run/agent/models.yml" <<EOF
providers:
  mock:
    baseUrl: http://127.0.0.1:$port/v1
    apiKey: dummy
    api: openai-completions
    models:
      - id: mock-1
        name: Mock
        contextWindow: 128000
        maxTokens: 4096
EOF
printf '%s\n' '{"id":"L1","origin_ms":1,"question":"Is the lock-free relay delivery free of ABA on the queue head?"}' \
  '{"id":"L2","origin_ms":1,"question":"Does the changelog mention the new CLI flag?"}' \
  '{"id":"L3","origin_ms":1,"question":"Does every reader see the published head before it is reused?"}' > "$run/borg/lessons.jsonl"
# borg behind a wrapper that, at the first tool call, leaves L3 in the session's queue as a live check
# would (a real check needs the consequence model; crates/borg/tests/live_cli.rs runs one on a fake).
cat > "$run/borg-live" <<EOF
#!/bin/sh
in=\$(cat)
if [ "\$3" = post_tool ] && [ ! -e "$run/seeded" ]; then
  sid=\$(printf '%s' "\$in" | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p' | tr -cd 'A-Za-z0-9-')
  echo "live L3" > "$run/state/\$sid.queued"; touch "$run/seeded"
fi
printf '%s' "\$in" | exec "$root/target/debug/borg" "\$@"
EOF
chmod +x "$run/borg-live"

python3 -I "$here/mock_model.py" "$port" "$run/requests.log" &
mock=$!
trap 'kill $mock 2>/dev/null' EXIT
sleep 1

(cd "$run/repo" && env -i PATH=/usr/bin:/bin HOME="$run/home" TERM=dumb \
  XDG_CONFIG_HOME="$run/home/.config" XDG_DATA_HOME="$run/home/.local/share" XDG_CACHE_HOME="$run/home/.cache" \
  PI_CODING_AGENT_DIR="$run/agent" \
  BORG_BIN="$run/borg-live" BORG_STATE="$run/state" BORG_HOME="$run/borg" BORG_LEARN=off BORG_UPDATE_CHECK=off BORG_MATCH=bm25 \
  timeout 120 "$omp" -p --no-session --auto-approve --model mock/mock-1 --no-extensions \
  -e "$root/adapters/omp/borg.ts" "make the relay delivery lock-free" > "$run/omp.out" 2> "$run/omp.err")

ctx=$(sed 's/ ms=[0-9]*$//' "$run"/state/*.ctx)
expected=$'make the relay delivery lock-free\nsrc/relay.rs\n#borg-asked L3\n#borg-live L3\n#borg-asked L1'
requests=$(grep -c chat/completions "$run/requests.log")
# The second request: the tool output, then borg's question in a message of its own after it.
live=$(grep chat/completions "$run/requests.log" | sed -n 2p | python3 -I -c '
import json, sys
msgs = json.loads(sys.stdin.read().split(" ", 1)[1])["messages"]
tool = max(i for i, m in enumerate(msgs) if m["role"] == "tool")
print(any("published head before it is reused" in json.dumps(m) for m in msgs[tool + 1:]))')
if [[ "$ctx" == "$expected" && "$requests" == 3 && "$live" == True ]] && grep -q "ABA on the queue head" "$run/requests.log"; then
  echo "ok: prompt, touch, a live question after the tool output and one round of questions went through omp"; rm -rf "$run"
else
  echo "FAILED ($run): ctx=<$ctx> requests=$requests live-after-tool=$live"; tail -5 "$run/omp.err"; exit 1
fi
