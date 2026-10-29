# Changelog

## 0.6.0 (unreleased)

Messages are paced by acknowledgements instead of a fixed rate and keep their full text; the mesh
speaks `borg/mesh/14`. Lesson selection is measured end to end: what a check delivered, how it was
judged, and what not delivering would have changed. The fixed rule stays the default: the learned
selection policy is opt-in (`BORG_SELECT=learned`) until its pre-registered checks pass, since its
recall check (P4) failed both the first run and the one rerun
([docs/evidence.md](docs/evidence.md#lesson-selection)). Quality is the objective: there is no cost
cap, and cost is measured and shown. Every numeric constant says what kind of number it is
([docs/design/constants.md](docs/design/constants.md)). Every crate is 0.6.0 and pins the others
`=0.6.0`.

### Added

- Message acknowledgements: the recipient's hook, or `borg inbox` for the engineer, acknowledges the
  messages it showed with a `seen` line (synced, never shown, never paced); a reply acknowledges what it
  answers. `borg top`, printed or live, and borg web's Messages view acknowledge the messages to the
  engineer they show (`POST /api/messages/seen`, JSON only); `borg top --json`, and `borg top` in a
  drone's shell (printed or live, also on a pseudo-terminal), acknowledge nothing; neither screen counts
  a message whose line is cut before its text.
- `borg inbox --show <id>` prints a message whole. A message longer than what a prompt or tool call has
  room for shows its head and that command.
- A learned lesson selection policy, opt-in with `BORG_SELECT=learned`. Per check it chooses, for each
  lesson the model graded as applying, between asking, handing it over free and not now, by the
  mistakes it expects each to avoid, from one Bayesian model of the logged outcomes; a free lesson's
  and a live question's value is weighed by its learned chance of arriving. A conservative gate keeps it
  on the fixed rule until the replayed outcomes support a deviation (at most 10% of the fixed rule's
  quality at risk), so a new store starts exactly as the fixed rule; a stop's grade 3 is always asked. A
  monitor reverts a store to the fixed rule for good when it sees harm beyond that share
  (`<store>/select-reverted`; deleting that file alone lets it learn again). `borg learn` refits the
  model at its end; a hook only reads it. It stays opt-in until its pre-registered checks pass: on
  their first run its right-lesson recall on the review → fix eval was −4.02 points [−7.28, −1.24]
  against the fixed rule, past the bar's −5 (P4), and its calibration missed its bar (P2). The one
  rerun on the fixed tree passed P0, P1, P3 and P2 (by the pruned model) and failed P4 again: −3.53
  points [−6.74, −0.98] under the store's posterior, −3.77 [−6.89, −1.18] under the n = 0 prior.
  `BORG_SELECT=fixed` wins over `BORG_STOP_ASK_CAP`, which wins over `BORG_STOP_ATTENTION=on` (both
  evaluation switches now), and each over `BORG_SELECT=learned`.
- `borg debug select [--replay] [--rebuild] [--dir DIR | --file FILE] [--json] [--eval RUNS_DIR
  --qrels FILE]` shows the policy's state and replays the logged checks against the pre-registered
  checks P0–P5 under the learned policy, at 0 tokens. It prints the dates of the rules and of their
  latest amendment; after a P2 FAIL it reruns P2 once with the model pruned to grade, route and
  dilution, whose verdict is P2's. A check that ran on data that could not have failed it says NOT
  TESTED, never PASS, and the command exits 1 when P0–P4 has a FAIL or a NOT TESTED.
- Selection is measured: a check id on every graded record and delivery marker, the route each lesson
  actually took (`asked`, `free`, `placebo`, `dropped_done`, `dropped_chars`, `never_delivered`) and why,
  checks that apply nothing, pool tokens, model, prompt hash, harness, version, latency and failures,
  the call's cost in dollars as the CLI reported it, the propensity of each free delivery (`pf`), the
  applies the session already had or had waiting in its queue (`nofree`), the summary the decision read
  (`seen`), each run of a session's end hook (`ended`), and on an early delivery each lesson's score
  relative to the best on-topic match (`rel`), for a learned early rule. Older records read as the
  fixed rule.
- A placebo arm, under the fixed rule too: of the applies a check delivers neither way (under the fixed
  rule, those past its 10), up to two are judged blindly at the point a free delivery would be, never
  a lesson the session already had, with their chance of the draw (`psi`). Their rate is the judge's
  floor, and under the fixed default they collect the not-now labels its own history cannot give.
  Placebo results never reach beliefs, trust, stats, the team file or the offline estimates.
- A tail sample: when the pool's budget leaves lessons out, up to two of them, drawn uniformly, are
  graded after the pool under every policy (`tail`, their chance `pt`), so the yield past the budget is
  measured; the pool line of the session's `.cost` file reads `tail=<n>`.
- `borg status`, `borg top` and `borg web` (`/api/select`, on the Conscience page) show the policy and
  the default, spend and extra rounds per session, effects with their intervals, label health, the
  placebos drawn and judged, the tail lessons shown and applied, the fitted delivery rates, pool yield
  and stop latency per harness against its stop budget with its failed calls, and the learned
  decisions a guard gave the fixed rule, per reason; under the learned policy also its gate,
  deviations from the fixed rule and the monitor's reading and bound with its weights' health (ESS,
  largest weight). What is not measured yet says so.
- An optional per-collective `budget` in `collectives.json` (pool tokens per session on average), off
  by default: only an engineer who sets one puts a price on spend, and only the learned policy reads
  it, from its first fit that knows it. A value that is not a positive number counts as none and never
  invalidates the config.
- [docs/design/constants.md](docs/design/constants.md): every numeric constant by kind (protocol,
  guard, derived, learned, method, baseline, display, experiment).

### Changed

- A sender may have a window of unacknowledged messages to a recipient instead of one message per 10
  minutes: it starts at 1, doubles per acknowledgement up to 64 and halves when one expires unread.
  An ask leaves the window once it is shown or answered, as a tell does. A refusal says how many are
  unread (`msg_dropped` with `why: window`). Pause, resume and veto are never held back. Where no
  acknowledgement may come, nobody is paced more strictly than 0.5.0: a broadcast or a message to the
  engineer may also go 10 minutes after the sender's previous one, and a session on another machine is
  paced at that rate alone until its machine acknowledges one of the sender's messages to it. `borg
  tell` also refuses a message that would push one of its sender's unread messages to the same
  recipient out of the log, and a text past 1,000 characters to a session on a 0.4 or 0.5 machine; a
  broadcast says how many member machines will not show it. The mesh carries every message within the
  bound any window keeps (acknowledged, or at most 64 unread from one sender to one recipient within
  10 minutes), which reads no earlier line, so pruning old lines never stops a message once sent.
- Acknowledgements no longer make a hook or `borg listen` read the message log again:
  `<store>/messages/generation` (local) moves only when a tell, ask or steer is written.
- A message keeps its whole text up to 16,384 characters (was 1,000). What one prompt or tool call
  shows fits the harness's context budget, shared with the lessons of that event (was at most 10
  messages and 4,000 characters).
- The message log keeps, of each member's lines, the newest of each class (conversation, steers,
  acknowledgements, and apart from them those to `local`) within 1 MiB, instead of the newest 500
  lines; a cut is counted (`msg_dropped`, `why: bounds`). A line, an answered ask too, stays until it
  is 7 days old or cut.
- The mesh protocol is `borg/mesh/14`. Members on `/13` and `/12` still sync; a `/13` member is offered
  only what 0.5.0 reads: messages of at most 1,000 characters, no `seen` line, paced as 0.5.0 paced them.
- Each harness's capabilities carry its context budget (10,000 characters: Claude Code's and Codex's
  own limits, and for omp and opencode, which set none, the stricter of those) and its stop budget (omp
  28 s, opencode and Codex 90 s); there is one stop timeout, 90 s. The omp and opencode adapters read
  their budgets and the shell-output tail from borg's first answer; borg's answer names the messages
  it showed (`"shown_ids"`, which Claude Code and Codex never get), and a held wake is dropped only once
  the model was handed all its messages (opencode: also a context that came too late for its call; omp:
  also a steer's, kept for the next agent start). A wake the harness
  refused is retried by `borg listen --stream` itself, after 10 s, doubling up to 5 minutes (the
  opencode adapter's own 5 s × 5 retry and `BORG_WAKE_RETRY_MS` are gone). opencode's session end waits
  borg's end budget instead of the stop's (both 90 s). `borg listen` learns each session's idle and
  tool holds from its own gaps (the 95th percentile of its latest 64, never below today's 5 minutes, a
  minute on Codex and 10 minutes).
- The judge is route-blind (judge v2, logged): a free delivery gets the same evidence as a question, a
  live check judges free deliveries and placebos at once, and a random half of questions are judged
  without the agent's own declaration.
- `asked_not_corrected` is censored, not counted as ignored, in the beliefs and in `borg debug ope`;
  regret blame from one session is capped at that session's corrections; settling runs only for
  sessions that ended.
- The fixed rule never hands over free a lesson the session already had, which delivery dropped as
  given before; the next 2 or 1 takes its slot. Such a lesson may still be asked.
- `BORG_STOP_POOL_TOKENS` fixes the pool for every check; unset, the fixed rule keeps 12,000 tokens and
  the learned policy chooses it, 12,000 until it has evidence for another size.
- The live check and the stop fold only the outcome log's new tail into the selection summary and save
  it beside the lessons file (`select-summary.json`, never synced); the first check after the upgrade
  builds it from the whole log. A store's learned checks decide and record one at a time under
  `<store>/select-summary.json.lock`; the fixed rule and the evaluation switches take no lock. A
  learned check kept waiting more than 1 s, or whose record cannot reach the log its gate is folded
  from (learning off, `BORG_CONSCIENCE` at another store's lessons, a failed append), takes the fixed
  rule, and a record it writes says why (`reason: lock_busy`, `lock_failed` or `unrecorded`).
- Relative times read `3h05m` under a day and `2d` from a day on, in `borg inbox`, `borg top` and its
  live screen alike.
- Every numeric constant's doc comment says its kind and why, the adapters' too (the conformance suite
  checks omp's and opencode's). Values that were defined in several places are defined once (clock
  skew, the day, BM25's parameters, characters per token, the failed-call limit, the job count, the
  correction weight, the excerpt lengths, the id labels).
- The documentation describes the 0.6.0 messages, selection, budgets and statistics; every number it
  states was checked against the code.

### Fixed

- A live check's grade 3 on a lesson handed over earlier was logged as asked and then dropped. It is
  now asked, or kept out of the pool.
- Free lessons past the early-delivery character bound were dropped without a record; they are logged
  as `dropped_chars`.
- A lesson still waiting in the queue to be asked or handed over is decided already: the stop leaves
  it out of its pool, as a live check does, and a check that graded it meanwhile never hands it over
  free or draws it as a placebo (`nofree` names it). It used to take a free slot from the next lesson
  with a copy that delivery dropped as queued twice.
- omp and opencode: a held wake whose messages the turn's hooks showed over several hooks, or before
  the wake arrived, still went in as a turn for messages the model had read. It is dropped now, by the
  ids the turn's hooks handed the model (`"shown_ids"`).
- The attention cut (`BORG_STOP_ATTENTION=on`) read the outcomes of the routed collective, so evals
  read the real store; selection reads the outcomes beside the lessons file.
- `borg reach` stops asking a model that failed three times before its first answer, as the facets and
  `borg assimilate --from-dir` batches do, instead of making one failed call per lesson.
- The committed web page (`crates/borg-web/dist`) named the build machine's home directory in its
  panic locations; `scripts/ui.sh` writes those paths relative to `~` now.

### Removed

- The fixed message rate (`msg::RATE_MS`, a third of the presence idle time) and the 500-line message
  cap (`msg::KEEP`).
- The expert drone's $0.50 spend limit (`--max-budget-usd`): its time limit bounds a runaway drone,
  and its cost is measured, never capped.

## 0.5.0 (2026-10-28)

The first release on crates.io (0.4.0, 0.3.0 and 0.2.0 were never published: 0.4.0's notes are
part of this section, and those of 0.3.0 and 0.2.0 below are history). borg keeps the shared memory:
lessons from corrections, checked against the work and shared with a team. The hive and playbooks are gone; sessions talk to each other instead, on any
harness and across machines, and steering is part of that channel. Every crate is 0.5.0 and pins the
others `=0.5.0`. The release steps are in [CONTRIBUTING.md](CONTRIBUTING.md#releasing).

### Added

- `borg --version`, `borg -V` and `borg version` print `borg <version>`; `borg-mesh` and `borg-web` take
  `--version` and `-V` too (`borg-web` no longer starts its server on an unknown `--version`).
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

- `borg tell`, `borg ask`, `borg pause` and `borg resume local` reach this machine's sessions;
  `collective` (or `all`) the whole collective; a name prefers a session on this machine. `local` is
  written `local@<node>`, never syncs and counts only on a line of that node; `collective` and `all`
  are written `all`, as before. A drone or session prefix that matches here and on a member's machine
  names the one here; a member's session is reached when nothing here matches, by `<session>@<node>`,
  or as the sender a reply answers. A `borg resume local` warns when a collective pause sent from here
  still holds on the members' machines. `borg top`, `borg inbox` and borg web name the recipient
  `local` or `collective`, and the borg skill asks this machine's sessions first.
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
- Releases are made locally with `scripts/release.sh X.Y.Z`, as in libpetri: it runs the gate,
  publishes the crates to crates.io, then tags and creates a draft GitHub release. A pushed tag no
  longer publishes anything; the release workflow only builds the tarballs, attaches them and
  publishes the draft.

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

Meant as the first release for crates.io; 0.5.0 took its place: a smaller, consistent command line, a library marked internal,
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
