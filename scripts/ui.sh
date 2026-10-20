#!/usr/bin/env bash
# Builds the borg web page (crates/borg-web/ui, Leptos) into crates/borg-web/dist, which borg-web embeds, and records
# the hash of its sources so scripts/check.sh can tell a stale dist/ from a fresh one.
#
# usage: scripts/ui.sh [--check]   (--check: only compare the hash, build nothing)
set -euo pipefail
cd "$(dirname "$0")/../crates/borg-web/ui"

hash() { cat Cargo.toml Cargo.lock Trunk.toml index.html $(find src -name '*.rs' | sort) | sha256sum | cut -d' ' -f1; }

if [ "${1:-}" = "--check" ]; then
    if [ "$(hash)" != "$(cat ../dist/.source-hash 2>/dev/null)" ]; then
        echo "crates/borg-web/dist is stale: run scripts/ui.sh and commit it" >&2
        exit 1
    fi
    exit 0
fi
trunk build --release
hash > ../dist/.source-hash
