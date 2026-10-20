#!/bin/sh
# End-to-end dogfood of Borg in a sandbox HOME (crates/borg-eval/src/dogfood.rs, reports/dogfood-e2e.md).
#
#   eval/dogfood/run.sh [--run NAME] [--model sonnet] [--setup-only]
#
# Needs bwrap and a Claude login valid for at least an hour (the sandbox gets a copy of the token and
# must never refresh it). The copy lives in a private temp dir, is bound into the sandbox HOME only
# while each sandbox runs and is deleted when the run ends. Writes only under target/dogfood/<run>/;
# the real ~/.claude, ~/.config/borg and ~/.local/share/borg are hidden from the sandbox. Default
# model sonnet; about $0.5 per run.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
cargo build --release --manifest-path "$root/Cargo.toml" -p borg-ai -p borg-eval
exec "$root/target/release/borg-eval" dogfood "$@"
