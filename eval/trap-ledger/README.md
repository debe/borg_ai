# trap-ledger

Eval workload for borg. A tiny stdlib-only Python ledger with **planted knowledge**: facts every drone needs and has to rediscover unless the collective remembers them.

| Trap | Kind | What a drone must find out |
|---|---|---|
| `LEDGER_TZ` | protocol | Tests only pass with `LEDGER_TZ=UTC` set. Without it, `ledger.clock` raises a misleading "naive datetime" error. |
| `ledger._generated` | scar (error→fix) | `ledger.plugins` imports a generated module. `python3 tools/gen.py` must run first (it is gitignored, and the harness runs `git clean -fdx` between tasks). |
| `round_cents` | intentional quirk | Banker's rounding is required. Hidden invariant tests fail if a drone "fixes" it to half-up. |

- `repo/` is the starting state, copied into a fresh git repo for each run.
- `tasks/NN-*.md` are the prompts, run in order.
- `hidden/` holds the grading tests, copied in only after a task finishes.
- Grade command: see `grade.sh`.

## Drift trap (tests whether the collective questions itself)
`drift/before-NN-*.sh` scripts are applied by the harness **before task NN**. It renames the environment variable `LEDGER_TZ` → `LEDGER_TIMEZONE`. A collective that blindly recalls "set LEDGER_TZ=UTC" is now wrong. The metrics are harm (failures caused by stale recall) and recovery (how quickly the stale protocol dies). The grade command uses the variable that is valid at that point.
