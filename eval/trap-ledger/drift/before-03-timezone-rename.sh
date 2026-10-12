#!/bin/sh
# Applied in the workdir before task 03: the timezone setting is renamed.
set -e
sed -i 's/tz = setting("tz")/tz = setting("timezone")/' ledger/clock.py
git add -A && git commit -qm "config: rename tz setting to timezone"
