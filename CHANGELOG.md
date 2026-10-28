# Changelog

## 0.4.0 (2026-10-28)

The first release on crates.io (0.3.0 and 0.2.0 were never published, so their notes below are
history). borg keeps the shared memory: lessons from corrections, checked against the work and
shared with a team. The hive and playbooks are gone; sessions talk to each other instead, on any
harness and across machines, and steering is part of that channel. Every crate is 0.4.0 and pins the
others `=0.4.0`. The release steps are in [CONTRIBUTING.md](CONTRIBUTING.md#releasing).

### Added

- Messages between sessions: `borg tell <to> "<text>"`, `borg ask <to> "<question>"` and `borg inbox`.
  A session on any harness, or the engineer's terminal, writes to a drone (as `borg top` names it), a
  session (`<session>@<node>` across machines), `all` or the engineer. The recipient sees each message
  once, at its next prompt or tool call, as peer data with the command that replies. Messages are
  redacted, limited to one per sender and recipient every 10 minutes, kept 7 days and synced with the
  team collective. Stats count `msg_sent`, `msg_delivered` and `msg_dropped`; `borg web` shows them.
- `borg listen` wakes an idle session when a message arrives for it, until `borg listen --off` or the
  session's end. On Claude Code the stop asks the model to run `borg listen --wait` in the background;
  the omp and opencode adapters keep `borg listen --stream` running and send each wake in as a new turn;
  on codex a detached `borg listen --queue` queues one with `codex queue`. A wake names the senders and
  the count, never the text. Nothing wakes a session during the engineer's own turn; on codex, the
  `Interrupt` hook the hive used now ends that hold at once on an Esc. `borg top` shows a WAKE
  column.

### Changed

- Steering is now messages: `borg pause <to> [note]` and `borg resume <to> [note]`, and `borg no` to a
  drone on another machine sends a veto message. Every harness shows them at the next prompt or tool
  call, not only Claude Code. One from another machine counts only when the mesh admits its sender as
  an `admin` or `steer` member. The rate limit never holds them back. `borg steer` is removed: use
  `borg pause`, `borg resume`, or `borg tell` for an instruction. Stats keep `steer_issued`,
  `steer_delivered`, `steer_dropped` and `pause_refused`.
- The mesh protocol is `borg/mesh/13`. A member built from 0.3.0 sources (`borg/mesh/12`) still syncs
  everything except messages; older members do not sync, and `borg doctor` names them.
- The documentation was checked against the code: the README, the command, configuration, harness,
  team and statistics references, the adapter READMEs and the borg skill describe messages, pause and
  `borg listen`, and the mesh figures no longer show the old steering.

### Removed

- The hive: `borg hive` and `borg debug hive`, interactive drones, and goals with their boards, claims,
  verify tasks, oracles, integration, contracts and plans. The Hive tab of `borg web`, the goal sections
  of `borg top`, and the `BORG_HIVE*`, `BORG_GROUP_REFLECT` and `BORG_REWORK_REFLECT` variables go with
  it. Messages between sessions replace it. Plain sessions, lessons, `borg no` and team sync
  work as before. `borg install` removes the Claude Code `SessionStart` hook only the hive used. An old
  store's `hive/` directory is no longer read; `borg doctor` names it and the command that removes it.
- Playbooks are removed: no playbook was ever promoted, and lessons already cover mistakes of procedure.
  A clean session no longer proposes a playbook edit, no prompt shows one, and `borg queen` asks no
  playbook question. The Playbooks tab of `borg web`, `/api/playbooks`, the `playbook_*` statistics and
  `BORG_PLAYBOOKS` go with them. An old store's `playbooks.jsonl`, `playbooks-reflected.txt` and
  `playbooks-plain.txt` are no longer read or synced, and an old queued playbook question is skipped;
  `borg doctor` names the files and the command that removes them. The mesh protocol stays `borg/mesh/13`: a session still steps
  through the playbooks' log and keeps nothing an older member offers there.

## 0.3.0 (never published)

Meant as the first release for crates.io; 0.4.0 took its place: a smaller, consistent command line, a library marked internal,
and documentation whose claims match their reports. The release steps are in
[CONTRIBUTING.md](CONTRIBUTING.md#releasing).

### Command line

- `borg help` groups the commands as setup, learning, decisions, watching, team and hive. One table
  feeds the help, the shell completions and the dispatch, so they cannot disagree; an unknown command
  exits 2 with `borg: unknown command`.
- Evaluation and debugging tools moved under `borg debug` (`borg debug help` lists them): `recall`,
  `resolve`, `prefilter`, `memory`, `expert`, `ope`, `vectors --query`, and `hive run`, `hive score`
  and `hive init --hidden-dir/--hidden`. Their old names exit non-zero.
- Removed: `borg top --watch` (`borg top` is the live screen; piped or with `--once` it prints once),
  `borg hive board --watch`, `borg queen next` (use `borg queen --json`) and `borg queen notify` (the
  notification still comes when questions are queued).
- `borg top` rejects unknown flags, and `borg queen` rejects an unknown subcommand in every repo,
  including one that routes off.
- Completions add `borg hive wait` and `borg debug` with its flags.

### Packaging

- Every crate is 0.3.0, and the workspace's own crates pin each other `=0.3.0`: they are released
  together.
- The `borg` library is internal, with no semver promise: it exists for borg's own crates. Helper
  modules are private or hidden from the docs. Depend on the `borg` binary and its documented files.
- docs.rs builds the lean library (no vectors stack). borg-trace, borg-mesh and borg-web each ship a
  README; borg-mesh's package leaves out its tests.
- borg-trace's three research binaries moved to the unpublished borg-eval, and borg-trace no longer
  depends on rayon.
- Non-Unix targets fail at compile time with a clear message, instead of failing at run time.
- The release workflow runs `cargo publish --dry-run --workspace` before it creates the GitHub release,
  builds borg-web on its own (its tarball binary leaves out the vectors stack) and smoke-runs it. Every
  GitHub action is pinned to a commit SHA. `scripts/check.sh` ends with the same publish dry run.

### Hive

- A drone's waiter follows a reinstall of borg (`cargo install`) in place: it never claims with the
  replaced code, runs the new borg in the same process, so no session restarts, and shows as
  `waiting (old borg, restarting)` in `borg top` and `borg web` until it does.
- Until it runs the new borg, a waiter also gives up its node's work (reaps, integrations) to the
  mesh daemon or another drone's waiter, beating only for its own drone so peers keep its claims.
- `borg mesh serve` follows a reinstall of borg-mesh the same way: it stops serving the old code and
  runs the new borg-mesh in the same process, so a terminal or a service keeps one daemon.
- Drones report plan problems to the session that planned the goal (`borg hive tell "..." --to
  planner`), which sees them at its next prompt, even after the approval, and tells the drones without
  joining; `borg top` and `borg web` show its unread messages. `approve`, `integrate` and `score` now
  refuse inside a harness session like `ratify`, and `borg hive tell` from a terminal sends as the
  engineer, no longer as the repo's one drone.
- A goal opened from a terminal has no planning session, so `borg top` and `borg web` count its drones'
  messages to the planner on the goal's home node, and `borg hive board` in a terminal lists the ones
  not read on that machine and marks them read. `borg hive leave` in a session that only plans ends its
  planning.
- Verified work no longer disappears when a drone reuses its worktree after a blocked attempt: every
  claim starts from the goal's origin plus its deps' verified changes, the drone's worktree is reset
  to it (the tree before kept in a pinned commit `borg top` and `borg web` name), and a verdict rejects
  work whose base is not that start. Deps whose changes conflict block the task once, with the paths.
- Drone trees no longer follow the engineer's HEAD: work starts from the commit `borg hive init` ran
  on (in another repo in scope, its HEAD at the goal's first claim there), and later engineer commits
  reach drones only through a task. A plain checkout claims only where its tree is that start, and a
  node whose clone lacks it says so in `borg top` and the Hive tab.
- A wrong task definition is caught before a drone wastes rounds on it: `init` and `open` refuse a
  file the goal's check reads and a check that cannot pass on any tree, and read a directory as its
  tree; `done` (and the stop) refuses work the verifier would reject for its scope, naming each file;
  the drone asks with `borg hive scope --files +a "why"`, which `borg top` and `borg web` alert until
  the engineer runs the amend it carries or says no with `borg hive tell --re` from a terminal; while
  it waits the drone works in scope and may finish without the change; `open --again` now amends an open or
  claimed task in place, keeping the claim and waking a drone idle on its ask, and refuses a drone
  and any harness session. A `--again` line an older borg wrote still reopens a blocked task. A check
  refused at `open` is one no change could pass (a missing command, a usage error of a tool no repo
  configures, such as git), never a script or a flag the task adds or registers with a test runner.

### Documentation

- [Evidence and limits](docs/evidence.md) states each result as its report has it: the turn-end result
  on all 15 cases first (25% → 34%, not significant) with the 4-case subgroup labelled; review → fix
  with its pre-registered rule, which was not met; the hive pilot probe in which one session beat the
  hive; a column that says which reports are published.
- The README opens with why borg exists and a short glossary.

## 0.2.0 (never published)

Never tagged or published: 0.3.0 is the first release. In daily use from source, with lessons syncing
between engineers' machines over the mesh, across NATs; tested on Linux and macOS.

### Install and setup

- Packaged for crates.io as `borg-ai` (the `borg` binary), `borg-mesh` and `borg-web`, with prebuilt,
  sha256-checked binaries for `scripts/install.sh` (Linux x86_64 and arm64, macOS on Apple silicon);
  both first ship with 0.3.0. Rust 1.91 or newer.
- `borg setup`: one guided command from a fresh install to a working borg. It shows every change and
  writes nothing until you confirm; flags answer its questions for scripts. A second run changes nothing.
- `borg install` also copies the `borg` skill into each harness, so the agent can explain and run borg
  itself. `borg status` and `borg doctor` say what is wired and what is broken.

### Learning and delivery

- Corrections, rejected tool calls and `borg no` vetoes become lessons at session end (`borg learn`);
  `borg assimilate` imports past sessions and, with `--from-dir`, a team's documents.
  [How one session learns](docs/learning.md)
- Lessons arrive at the prompt (free, by word matching), during the turn in live mode (the default), and
  at the turn end through a free gate and one graded model call. `"mode": "hook"` checks only at the turn
  end. [Delivery and modes](docs/delivery.md)
- Model calls run through the drone's own harness (claude, omp, opencode, or codex with a `codex:`
  model), so no particular CLI is required.
- Vector recall finds lessons that share no word with the work (`borg vectors --fetch` once).
- Decision → fix links: a later commit that fixes an earlier decision is linked to it, and `borg why`
  shows both ends. `BORG_REGRESS=learn` turns fix links into lessons; by default they are only recorded.
  [Decision → fix links](docs/configuration.md#decision--fix-links)
- Review → fix: bugs that a reviewer sub-agent found and a fix step fixed, before any commit, become
  lessons.

### The queen

- `borg queen` lists the decisions no lesson answers, with `show`, `ask`, `answer`, `skip`, `dismiss`
  and `dedupe`; `--json` gives drones a stable schema. A session's first prompt in a repo with an open
  question gets that one question to ask you (`BORG_QUEEN_ASK`, on).

### Team

- Collectives per repo path: team, private (`"share": "local"`) or off.
  [Configuration](docs/configuration.md)
- `borg-mesh` syncs a team collective peer to peer with signed membership and no server, across NATs
  through a relay; onboarding by an expiring, single-use ticket. A company can run its own
  [relay](docs/relay.md). [Team sync](docs/team-sync.md)
- `borg top`, a live screen of the team's drones and hive goals, and `borg steer`, a signed instruction
  to running drones. `borg web` is a live page of knowledge, checks, drones and cost.
  [Statistics and the page](docs/statistics.md)

### Harnesses

- Claude Code hooks, omp and opencode 1.x/2.x adapters, and Codex CLI hooks (trust them once with
  `/hooks`). Hooks always fail open. [Harnesses](docs/harnesses.md)

### Hive

- Many drones on one goal: a board of tasks, each finished only when its checks pass, verified by
  another drone and merged into one tree that must pass the goal's oracle, across machines over the
  mesh. Drones are interactive sessions you open on any of the four harnesses; they wait at no token
  cost, and no drone takes a task before you approve the plan. Removed again in the next release.

### Off by default (experimental)

- Turn-end asking of the engineer (`BORG_QUEEN=on`; the queen and its in-harness questions,
  `BORG_QUEEN_ASK`, are on), engineering concepts (`BORG_CONCEPTS=on`), facets (`BORG_FACETS=on`).
