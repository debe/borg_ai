#!/usr/bin/env bash
# One headless omp run with the borg extension, fully isolated and free: a temp HOME and agent dir
# (PI_CODING_AGENT_DIR), a temp borg store, and a local mock model. Never touches ~/.omp.
# Proves: the extension loads, the prompt and a tool touch reach borg, borg's questions block the
# first stop (omp continues the turn with them) and the second stop ends the run.
# Usage: cargo build -p borg && adapters/omp/smoke/smoke.sh   (needs omp and python3)
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
  '{"id":"L2","origin_ms":1,"question":"Does the changelog mention the new CLI flag?"}' > "$run/borg/lessons.jsonl"

python3 -I "$here/mock_model.py" "$port" "$run/requests.log" &
mock=$!
trap 'kill $mock 2>/dev/null' EXIT
sleep 1

(cd "$run/repo" && env -i PATH=/usr/bin:/bin HOME="$run/home" TERM=dumb \
  XDG_CONFIG_HOME="$run/home/.config" XDG_DATA_HOME="$run/home/.local/share" XDG_CACHE_HOME="$run/home/.cache" \
  PI_CODING_AGENT_DIR="$run/agent" \
  BORG_BIN="$root/target/debug/borg" BORG_STATE="$run/state" BORG_HOME="$run/borg" BORG_LEARN=off \
  timeout 120 "$omp" -p --no-session --auto-approve --model mock/mock-1 --no-extensions \
  -e "$root/adapters/omp/borg.ts" "make the relay delivery lock-free" > "$run/omp.out" 2> "$run/omp.err")

ctx=$(cat "$run"/state/*.ctx)
expected=$'make the relay delivery lock-free\nsrc/relay.rs\n#borg-asked L1,L2'
requests=$(grep -c chat/completions "$run/requests.log")
if [[ "$ctx" == "$expected" && "$requests" == 3 ]] && grep -q "ABA on the queue head" "$run/requests.log"; then
  echo "ok: prompt, touch and one round of questions went through omp"; rm -rf "$run"
else
  echo "FAILED ($run): ctx=<$ctx> requests=$requests"; tail -5 "$run/omp.err"; exit 1
fi
