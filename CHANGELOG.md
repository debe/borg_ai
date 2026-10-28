# Changelog

## 0.3.0 (unreleased)

The first release meant for crates.io: a smaller, consistent command line, a library marked internal,
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
  cost, and no drone takes a task before you approve the plan. [Hive commands](docs/commands.md#hive)

### Off by default (experimental)

- Turn-end asking of the engineer (`BORG_QUEEN=on`; the queen and its in-harness questions,
  `BORG_QUEEN_ASK`, are on), engineering concepts (`BORG_CONCEPTS=on`), facets (`BORG_FACETS=on`).
