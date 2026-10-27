# Changelog

## 0.2.0 (unreleased)

First release. In use at a company with lessons syncing between engineers over the mesh, across NATs;
tested on Linux and macOS.

Release checklist (the heading keeps "unreleased" until the tag exists):

1. `scripts/check.sh` passes on main, and CI is green on Linux and macOS.
2. `cargo publish --dry-run --workspace` passes.
3. Replace "(unreleased)" above with the date, commit, and tag `v0.2.0`. The release workflow builds
   the tarballs, attaches them with their sha256 files and publishes the crates in order.
4. `scripts/install.sh` from that release in a clean home, then `borg doctor`.

### Packaging

- On crates.io as `borg-trace`, `borg-ai` (the `borg` binary and library), `borg-mesh` and `borg-web`,
  in that order; `borg-eval` and `borg-search` are not published. The skill and the TS adapters moved
  into `crates/borg/assets/` so the package holds what `include_str!` compiles in; `skills/borg` and
  `adapters/{omp,opencode}/borg.ts` are links to them. borg-web's built page moved from `ui/dist` to
  `crates/borg-web/dist` (cargo leaves a directory with its own `Cargo.toml` out of a package).
  Rust 1.91 or newer (iroh 1.3).
- Three ways to install: `cargo install borg-ai` (with `borg-mesh` and `borg-web`), the release
  binaries through `scripts/install.sh` (Linux x86_64 and arm64, macOS arm64; sha256-checked), or from
  source.
- `docs/relay.md`: running your own `iroh-relay` 1.3.0 for a company collective (ports, TLS through
  Let's Encrypt or your own certificate, the config file, a systemd unit, a container, load balancers,
  an allowlist of member keys, two relays for redundancy). Linked from Team sync, the README and the
  skill.
- `SECURITY.md`, `CONTRIBUTING.md` (check.sh, the arch.sh gates, the metric bar) and issue templates.
  Source comments that cited reports not in the repository now say "internal evaluation (not
  published)".

### Setup

- `borg setup`: one guided command from a fresh install to a working borg. It detects the harnesses,
  the model CLI and git identity; asks for a personal collective, a team or company collective (found
  one, or join with a `BORG1...` ticket), their repo directories, off paths and the default (`off`
  recommended); for a founded team, the relay (n0, `disabled` or a company `iroh-relay`) and the mode.
  It shows one plan (the config diff, the mesh command, the vector fetch, the wiring diffs) and writes
  nothing until you confirm. Then: the config with a backup, `borg mesh init` or `join`, the vector
  model and indexes, the wiring, an optional import, `borg doctor`. An existing config is only added
  to. Mesh and vector failures end as WARNs with their fix, never as an abort. Flags answer the
  questions (`--yes`, `--no-vectors`, `--harness`, `--private`, `--team`, `--paths`, `--relay`,
  `--join`, `--home`); without a terminal setup needs `--yes`. A second run changes nothing.
- The `borg` skill: `borg install` copies `SKILL.md` and its references into each harness's skills
  directory (Claude Code, omp, opencode, Codex), so the agent itself can explain borg, edit
  `collectives.json` and run setup, doctor, the mesh and vectors, dry run first. Uninstall removes only
  files exactly as shipped. `borg status` and `borg doctor` report a missing or stale skill. A test
  checks that every command and flag the skill names exists.
- `borg mesh join <ticket> --paths DIR...` writes your repo directories instead of the
  `~/CHANGE-ME/repos-of-<name>` placeholder; joining again with `--paths` replaces the placeholder.
- `borg install --apply` prints a one-line hint when the vector model is missing.

### Learning

- Captures corrections as episodes: correcting prompts, tool calls rejected with a reason, and the next
  prompt after a wordless denial (omp).
- `borg learn` (run at session end) turns episodes into lessons: question, situations, consequence,
  optional scope. A new correction on the same point updates the existing lesson.
- `borg assimilate` imports corrections from past Claude Code, omp and opencode 1.x/2.x sessions (dry run
  by default). opencode sessions are read through its CLI, or from a directory of exports (`--opencode DIR`).
- `borg assimilate --from-dir` imports a team's written knowledge as scoped lessons and open decisions,
  with a reviewable plan file.
- `BORG_ASSIMILATE_SKIP` keeps episodes and documents that mention given words out of the import.
- Model calls run through the drone's own harness (`claude -p`, `omp -p`, `opencode run`), falling back to
  any other installed one, so omp- or opencode-only engineers need no `claude` CLI. Runner prefixes
  (`omp:`, `opencode:provider/model`, `claude:`, `codex:`) pin one. opencode 2 runs through
  `--standalone` with an empty config dir, opencode 1 through `--pure`; either way borg's calls are
  filed under the temp dir, not the engineer's project.

### Decision → fix links

- At session end, in the background notes job, each landed commit's removed or changed lines are blamed
  at its parent (`-w -M -C`). An origin with Borg reasoning from another session, at least
  `BORG_REGRESS_MIN_AGE` (2 days) older, becomes a link in both notes (`borg fixed in:`, `borg fixes:`)
  and, for a team collective, a `kind: "regress"` record in `reasoning.jsonl` that the mesh replicates
  and `borg notes fetch` brings back. Private collectives keep the links in their notes; off repos get
  nothing. Redacted before writing and again when shown.
- A link is a *fix* on a revert (a `git revert` in the session counts), a fix-shaped subject, or a test
  that failed and then passed in the session (the hook now records test runs in
  `<state>/<session>.tests`) together with the engineer's words about a bug; otherwise a weak
  *revisited* link that is never learned from. Formatter hunks, moves (a swapped line is no move),
  pure additions, lock and generated files and commits over 40 files or 2,000 lines link nothing; at
  most 5 links per session.
- `BORG_REGRESS=learn` turns fix links into `regret` episodes (team and private collectives), which `borg learn`
  words as a question for the decision point ("Before X, has Y been considered?"). The default `links`
  records without learning. `borg learn` now starts after the notes job, not beside it.
- `borg regress [--replay A..B] [--explain] [--any-origin] [--json]`: the links Borg would make, as a
  dry run. `borg why` shows both ends: "→ fixed in" on the decision, "← decided in … → fixed here" on
  the fix (`fixed_in` and `fixes` in `--json`).
- Every git call has a 10 s limit (`BORG_REGRESS_TIMEOUT_S`) inside a 60 s run; it fails open.
- The mesh answers a link record for either of its commits, and a bare `borg notes fetch` asks for the
  last 200 commits on HEAD, noted ones too, so later links to a decision noted here arrive.
- [reports/regress.md](reports/regress.md): a replay of this repository (no notes, so no links) and of a
  scripted repo with known answers.

### Delivery

- Early delivery: the first prompt or file touch that names a lesson's topic gets that topic's top lessons
  as context, without a model call (Claude Code).
- Turn-end check: a free word-matching gate, then one graded model call over every eligible lesson.
  Only a grade-3 lesson blocks the stop for another round. Grade-2 and grade-1 lessons go free with the
  next prompt or touch.
- System gate: lessons scoped to other systems are not delivered in a repo that maps to a known system.
  The system map is read from `~/.config/borg/systems.json` (`BORG_SYSTEMS`); a neutral example ships.
- Delivery modes per collective (`"mode"` in collectives.json, `BORG_MODE` overrides): `live` (default)
  also checks during the turn; `hook` checks at the turn end only. In live mode a state-changing tool call
  starts a detached `borg check` of the turn so far (one at a time, at most one start per
  `BORG_LIVE_DEBOUNCE_S`, default 45 s); its grade-3 lessons arrive with the next tool call as "borg check
  of your work so far", and a stop whose changes a live check of the same turn already covered skips its
  model call (a grade 3 still waiting is asked there). `borg doctor` warns when bm25, BORG_ONLY or
  BORG_CRITIC turn live off. `borg status`, `borg doctor` and `borg web` show the
  mode; stats and outcomes carry `mode` and `channel` (`live_check`, `live_ask`, `usd_mode`, `channels`).
  Live is the default because it met the rule fixed before the live-vs-hook eval: on an internal benchmark
  of real engineering tasks (3 tasks × 3 seeds per mode), +0.38 criteria per task (95% CI [0, +0.88]),
  0 redo rounds against 0.5, borg's own cost $0.56 against $0.07 per task. `"mode": "hook"` or
  `BORG_MODE=hook` switches back; an unknown value means live.
- Live mode for omp and opencode 1.x/2.x: questions from a live check arrive with the next tool call
  (omp ≥18.4.4: the `tool_result` handler's `additionalContext`; opencode 1.x: appended to the tool
  output; opencode 2.x: a steer synthetic input); graded 2s and 1s wait for the next prompt as on Claude
  Code. Without a Claude transcript a live check reads the turn's tool calls and edits. The stop sizes its
  wait and model call to the harness's budget (`BORG_STOP_BUDGET_MS`; omp 28 s: a 7 s wait, then the call
  gets what is left but 3 s, at most 18 s; opencode 90 s).
- Vector recall at the turn end, on by default (`borg vectors --fetch` once; the index then keeps
  itself current after learn, assimilate and mesh sync; `borg doctor` warns while the model or an index
  is missing; `BORG_VECTORS=off` or a `--no-default-features` build for BM25 only).
- Expert repos per collective: cited claims from a local checkout of a dependency, at the pinned version.

### Memory

- Usefulness judged by meaning within the same turn-end call. Activation strengthens with use and fades
  with sessions; unused lessons go dormant and are never deleted.
- Trust per engineer and area (`borg trust`).

### Collectives and team

- Collectives per repo path: team, private (`share: "local"`) or off; an invalid config routes everything off.
- `borg-mesh`: peer-to-peer sync of a team collective's lessons with signed membership, no server.
- Mesh across NAT: peers are dialled by key, with iroh hole punching and relay fallback. The relay is
  set per collective (`relay`: n0's public relays by default, a self-hosted URL, or `disabled`).
- The relay's TLS certificate is checked against the system trust store, so a corporate VPN that
  re-signs TLS with its own installed CA no longer cuts the node off its relay.
- Onboarding by ticket: `borg mesh invite` prints a signed, expiring, single-use ticket and its QR
  code; `borg mesh join <ticket> [--apply]` adds the collective to the config (dry run first),
  joins and syncs.
- `borg top` and `borg steer`: see and steer running drones, locally and across the mesh.
- Reasoning notes: redacted excerpts of a session's commits as git notes (`borg notes`, `borg why`).

### Answering the queen

- `borg queen` lists open questions numbered and grouped by repo, with their options. `show`, `ask`
  (interactive), `answer <n> <text|a|b>`, `skip` (3 days), `dismiss` (no lesson) and `dedupe` work on them.
- `borg queen --json` and `borg queen next --json` give drones a stable schema with exact
  answer, skip and dismiss commands.
- Near-duplicate questions of the same repo are not queued again.
- In the harness: the first prompt of a session in a repo with an open question gets that one
  question as context. Claude Code asks it with AskUserQuestion; omp and opencode ask at the start of
  the reply. The drone records the answer. One question per session, only the session's repos, 0 model
  calls; `BORG_QUEEN_ASK=off` or `BORG_LEARN=off` disables it. The recording commands carry `--dir` and
  the hook's own binary. An adapter that will not wait for the prompt's context sends `"context": false`
  and gets none, so nothing is marked delivered unseen.
- A question's repo is its git root's name (worktrees folded), recorded when it is queued, as the hooks
  name it.
- A desktop notification (`notify-send`) when questions are queued, one per run; `borg queen notify`
  sends a summary. `BORG_NOTIFY=off` disables it.

### Harnesses and operations

- Claude Code hooks; omp and opencode adapters (turn-end check, live mode, learning, steering).
- Codex CLI support (tested with 0.153.2): `borg install codex` appends command hooks to
  `~/.codex/hooks.json` (`$CODEX_HOME`) with an explicit timeout each (prompt 30 s, pre-tool 10 s,
  post-tool 30 s, stop 90 s, end 3 s). An `apply_patch` call becomes an edit of every file the patch
  names. Codex runs a new or changed hook only after you trust it (`/hooks` in codex) and skips it
  silently otherwise: install says so, and `borg doctor` asks codex (`codex app-server`, `hooks/list`)
  which of borg's hooks it does not trust. borg never writes that trust. `adapters/codex/smoke.sh` runs
  a real `codex exec` against a local mock of the model API (0 tokens).
- borg's own model calls through codex (`codex:` runner) pass `-c features.hooks=false`:
  `--ignore-user-config` still loads `$CODEX_HOME/hooks.json`, so without it a codex drone's call
  could run borg's hooks inside borg.
- opencode 2 support: the opencode adapter serves opencode 1.x and 2.x from one file, queen questions
  included (2.x: the `setup` plugin API).
- `borg install|uninstall` with dry run, backups and idempotent merging; `borg status`, `borg doctor`.
- Hooks fail open; the turn-end model call has a 45 s limit (on omp at most 18 s, inside its fixed 30 s
  stop limit).
- One `Harness` trait (`crates/borg/src/harness/`, [docs/harnesses.md](docs/harnesses.md)): each harness
  is one module plus a line in `harness::ALL`, with its wiring, config path, doctor probe and a capability
  table (`Caps`) that the hook, install and steering consult instead of the harness's name. The capability
  flags are unchanged and pinned by a test. Help, `install` usage and completions are generated from
  `ALL`. `borg hook <unknown> <event>` now fails open (exit 0, no output) instead of printing usage.
- The command-hook merge is generic: each hook runs `<borg> hook <harness> <event>` with its own
  harness's name.
- Harness conformance suites: `crates/borg/tests/conformance_cli.rs` runs the installed command hooks
  (Claude Code and a hidden simulated harness, `sim`, whose capabilities `BORG_SIM_CAPS` sets; only a
  process with `BORG_TEST_HARNESS` set knows `sim` by name, so `borg install sim` stays a usage error), and
  `adapters/conformance/conformance.test.ts` runs the installed omp and opencode 1.x/2.x adapters. Both
  cover the same scenario: install, hooks, live mode and `borg top --json`. `crates/borg-web/tests/drones_api.rs`
  checks the page's `/api/drones` and `/api/collectives` (borg-web now has a library target).
- `scripts/arch.sh` measures the abstraction (lizard, cargo-coupling, rust-code-analysis) against the
  commit before it and gates harness-name branches, name tables, cycles and complexity;
  `crates/borg/tests/harness_arch.rs` runs the tool-free gates in `cargo test`. Without that commit in
  the history the script skips the before column and still runs the gates.
- Fixed: the omp adapter passed `edit` and `write` input raw, so an omp edit reached borg without its
  file (`.edits`, live checks) or its patch text. It now sends Claude's keys: `file_path` (an edit's files
  from its patch headers) and `new_string`. The opencode adapter now maps an edit's `newString` too.
- `borg doctor` checks for any model CLI borg can run its calls through (claude, omp or opencode), not
  only `claude`.

### Hive: interactive drones

- A hive is many drones on one goal: a signed, synced board of leased tasks, each finished only when its
  checks pass, verified by another drone and merged into `hive/<goal>/integrated`, which must pass the
  goal's oracle. Across machines the board, contracts, messages, commits and spend sync over the mesh.
- Drones are the sessions you open: Claude Code, omp, opencode 1.x and 2.x, and codex, in any mix.
  `borg hive join`, run by the model inside a session, makes only that session a drone, in its own
  worktree; `borg hive leave`, `/exit` or a closed terminal makes it a plain session again and releases
  its claim.
- A drone with nothing to do waits at no token cost (`borg hive wait`) and is woken by its harness's own
  path: Claude Code's background wait, omp's `sendMessage`, opencode's prompt (1.x) or queued prompt
  (2.x), and `codex queue` for codex. No task is handed into the engineer's own turn, and a claim no turn
  takes up within 60 s is released.
- `borg mesh serve` is the node's worker while every drone idles: it beats, acks peers' claims, reaps
  drones whose session is gone, integrates the verified work and says when the goal is finished or stuck.
  Without it, one drone's waiter does that work.
- The planning session: tell one session what to build and its borg skill opens the goal and shows the
  plan. You answer `yes` (or `no, because ...`) at that session's prompt; no drone takes a task before
  you do, and the model can never approve its own plan. `borg hive ratify <goal> [--budget USD]` approves
  from a terminal.
- Spend: each claim of a Claude Code drone is metered from its transcript (and its subagents') and
  priced by model; omp, opencode and codex claims are `unmetered`. New claims stop at the goal's budget
  until you raise it.
- The coordination protocol holds with every timer at 0 or infinity: claims are final before any work,
  results are monotone across arrival orders, integration runs once per epoch, and a goal ends only on a
  finish every node agrees on (docs/design/hive.md, phase f).
- Accepted with real sessions on two nodes (Claude Code and omp on one, opencode and codex on the
  other): one 4-task goal approved at the prompt, every task verified and integrated once, no model call
  while drones waited, wake-ups in 0.1 to 4.6 s, $0.17.
- `borg top` is a live screen like Unix `top`: the drones by state, each goal's progress and budget bars,
  its tasks, plan and nodes, and the newest board events; `q` quits. Piped, or with `--once`, it prints
  the plain text once. The Hive tab of `borg web` shows the same, with each goal's history.
- `borg hive run --drones N` runs headless drones for evals.

### Off by default (experimental)

- The queen (`BORG_QUEEN=on`), engineering concepts (`BORG_CONCEPTS=on`), facets (`BORG_FACETS=on`).
