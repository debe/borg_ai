# Troubleshooting

Start with `borg doctor`. Each line is `ok`, `WARN` or `FAIL`; it exits 1 when any line fails. Then
`borg status` for what each collective holds and how each harness is wired. Fix the first FAIL first:
an invalid config or a missing binary explains most of what follows it.

## borg doctor, line by line

| Line | What it means | Fix |
|---|---|---|
| `FAIL BORG_BIN=... does not exist or is not executable` | The binary the adapters run is gone | Point `BORG_BIN` at the installed `borg`, or unset it |
| `WARN borg: not on PATH and BORG_BIN unset` | The omp and opencode adapters cannot find borg | Put `~/.cargo/bin` on `PATH`, or set `BORG_BIN` |
| `FAIL <harness>: the adapter finds borg via $BORG_BIN or PATH, and neither has one` | That adapter silently does nothing | Same as above, then restart the harness |
| `WARN <cli>: ... did not answer --version` | The harness CLI hangs or is broken | Check the CLI itself; borg skips its own checks for it |
| `WARN no harness is wired to borg` | Nothing captures or checks anything | `borg install all` (dry run), then `borg install all --apply` |
| `FAIL <harness>: <file>: cannot read` / a JSON error | The harness config is unreadable or invalid; borg will not touch it | Fix the file's permissions or JSON by hand, then re-run install |
| `WARN <harness>: hooks missing for ...` | Only some events are wired | `borg install <harness>`, then with `--apply` |
| `FAIL <harness>: hook <event> runs <path>, which does not exist` | borg moved or was reinstalled elsewhere | `borg install <harness> --apply` rewires to the current binary |
| `WARN <harness>: <file> differs from this borg's adapter` | An older borg's adapter, or a hand edit | `borg install <harness>` (dry run shows the diff), then `--apply` |
| `WARN <harness>: the borg skill is missing` / `differs from this borg's` | The skill predates this borg, or was edited | `borg install <harness>` (dry run), then `borg install <harness> --apply`, for each harness the line names |
| `WARN codex: skips borg's hooks silently (...)` | Codex has not trusted borg's hooks, or they changed since | Open codex, run `/hooks`, trust borg's hooks. Needed again after every reinstall that changes the hook command |
| `WARN codex: lists none of borg's hooks` | Hooks are off in Codex's config | Remove `features.hooks = false` from Codex's config |
| `FAIL COLLECTIVES CONFIG INVALID (...)` | collectives.json is broken: **every repo routes off** | Fix the error named in the line; see [collectives](collectives.md) |
| `WARN collectives: default "x" names no collective` | Unmatched repos route off | Set `default` to `"off"` or an existing collective's name |
| `WARN collectives: a name appears twice` | Two entries share one store | Rename one |
| `WARN collective x: ... mode "..." is neither "hook" nor "live"` | A typo in `mode` or `BORG_MODE` | Use `"live"` or `"hook"` |
| `WARN collective x: live mode is off while BORG_MATCH=bm25 is set` (or `BORG_ONLY`, `BORG_CRITIC`) | Live checks need the model | Unset that variable, or accept turn-end checks only |
| `FAIL store x: ... not writable` | borg cannot record anything for that collective | Fix permissions on `$BORG_HOME/collectives/<name>` |
| `WARN engineers: git user "..." appears in the stores as ...` | One person under several emails | Add the `engineers` entry the line suggests, if it is one person |
| `WARN vectors: model missing at ...` | Vector recall is on but has no model: the turn-end check uses word matching only | `borg vectors --fetch` (behind a proxy or offline: see [vectors](vectors.md)) |
| `WARN vectors: collective "x" has no vector index` / `... built with another model` | That collective's lessons have no current vectors | The command the line names: `borg vectors --rebuild --store <lessons.jsonl>` |
| `ok vectors: off (BORG_VECTORS=off)` / `not in this build` | Off by choice, or a lean build | Nothing; unset `BORG_VECTORS` or reinstall without `--no-default-features` to turn them on |
| `WARN no model CLI on PATH ...` | No `claude`, `omp` or `opencode` to run borg's model calls: no learning, no turn-end judgement | Install one, or pin a runner (below) |
| `... codex answers only when pinned` | A Codex-only machine | `BORG_PREDICT_MODEL=codex:<model>` and `BORG_LEARN_MODEL=codex:<model>` |

## Model runner

borg's own model calls (learning, the turn-end and live checks) run headless through a harness CLI:
the drone's own first, then `claude`, `omp`, `opencode`. Pin one with a prefix:
`BORG_PREDICT_MODEL=claude:sonnet`, `omp:<model>`, `opencode:<provider/model>`, `codex:<model>`;
`BORG_LEARN_MODEL` likewise for `borg learn`, `assimilate` and `reach`. `off` disables the call. Set
these in the harness's environment (for example the `env` block of `~/.claude/settings.json`) or the
shell profile, after showing the engineer the change.

## Common situations

| Symptom | Check | Fix |
|---|---|---|
| borg never says anything in a repo | `borg collective <repo>` | `off` or the wrong collective: edit collectives.json. No lessons yet: `borg status` |
| Lessons do not reach a teammate | `borg collective <repo>` on both machines: `may sync: true`? | See [team](team.md); `borg mesh serve` must run on both |
| `borg vectors` says `model: missing` | | `borg vectors --fetch`, then `borg vectors --rebuild`; see [vectors](vectors.md) |
| `index: none at ...` or `built with another model` | | `borg vectors --rebuild` in a repo of that collective |
| A drone is paused and refuses tool calls | `borg top` lists the drones | `borg steer <drone> --resume` |
| Too many questions or blocks | `borg status` (cost and stop gate lines) | `"mode": "hook"` for that collective (about a tenth of live's cost) |
| After upgrading borg | `borg doctor` | `borg install all` (dry run), then `borg install all --apply`; for Codex, `/hooks` again |

## Undo

`borg uninstall all` (dry run), then `borg uninstall all --apply`: removes borg's hooks, adapters and
this skill, and only those (a file someone changed is left alone and named). Every file `--apply`
changed has a `<file>.borg-backup-<time>` beside it, except skill files removed because they were
exactly this borg's. The stores under `~/.local/share/borg` stay.
