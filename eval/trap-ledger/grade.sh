#!/bin/sh
# Usage: grade.sh <workdir> <hidden-test-file>...  → exit 0 iff all hidden tests pass.
set -e
cd "$1"; shift
mkdir -p hidden_tests
for t in "$@"; do cp "$t" hidden_tests/; done
touch hidden_tests/__init__.py
export LEDGER_TZ=UTC LEDGER_TIMEZONE=UTC
python3 tools/gen.py >/dev/null
python3 -m unittest discover -s hidden_tests -t . 2>&1
