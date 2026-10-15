#!/usr/bin/env bash
# Release checks: formatting, lints, the workspace tests, the vectors build's tests and, when bun is
# installed, the omp and opencode adapter tests and the adapter harnesses' conformance suite. Stops at
# the first failure.
#
# usage: scripts/check.sh
set -euo pipefail
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$*"; }

step "cargo fmt --check"
cargo fmt --all --check
# Optional borg-eval modules (crates/borg-eval/build.rs) are skipped by cargo fmt; check them directly.
for f in crates/borg-eval/src/preempt.rs crates/borg-eval/src/queen_replay.rs crates/borg-eval/src/queen_test.rs; do
    if [ -f "$f" ]; then rustfmt --check --edition 2024 "$f"; fi
done

step "borg web page: ui/dist built from the current sources"
scripts/ui.sh --check

step "cargo clippy (workspace, all targets, -D warnings)"
cargo clippy --workspace --all-targets -- -D warnings

step "cargo clippy -p borg --no-default-features (all targets, -D warnings)"
cargo clippy -p borg --all-targets --no-default-features -- -D warnings

step "cargo test --workspace"
cargo test --workspace

step "harness architecture gates (scripts/arch.sh; rows of a missing tool are skipped)"
scripts/arch.sh

step "cargo test -p borg --no-default-features"
cargo test -p borg --no-default-features

# The adapter contract tests drive the borg just built, wherever cargo puts it (the conformance suite
# fails without one). They are TypeScript run directly by `node --test` (Node 23.6+ strips types by
# default) or by bun.
cargo build -p borg
BORG_TEST_BIN="$(cargo metadata --format-version 1 --no-deps | sed -E 's/.*"target_directory":"([^"]*)".*/\1/')/debug/borg"
export BORG_TEST_BIN
if command -v node >/dev/null 2>&1 && node -e 'const [a, b] = process.versions.node.split(".").map(Number); process.exit(a > 23 || (a === 23 && b >= 6) ? 0 : 1)'; then
    step "adapter tests (node --test)"
    node --test adapters/omp/borg.test.ts
    node --test adapters/opencode/borg.test.ts
    step "harness conformance, adapter harnesses (node --test)"
    node --test adapters/conformance/conformance.test.ts
elif command -v bun >/dev/null 2>&1; then
    step "adapter tests (bun test)"
    bun test adapters/omp/borg.test.ts adapters/opencode/borg.test.ts adapters/conformance/conformance.test.ts
else
    step "adapter tests SKIPPED: neither node 23.6+ nor bun found"
fi

step "all checks passed"
