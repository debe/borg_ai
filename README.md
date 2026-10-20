<p align="center"><img src="https://raw.githubusercontent.com/debe/borg_ai/main/docs/img/cube.svg" width="420" alt="An isometric cube of cells; lit cells are drones linked to one mind at the core"></p>

# Borg

Borg is a shared memory for coding agents. It learns from the corrections engineers give their agents
and brings each lesson back when a later session, yours or a teammate's, meets the same decision. It
runs as hooks in Claude Code and as extensions in omp (oh-my-pi) and opencode. Teams share lessons peer
to peer, with no server.

**Status:** 0.1.0. In daily use at a company, with lessons syncing between engineers' machines over
the mesh, across NATs. Tested on Linux and macOS. Read [Evidence and limits](#evidence-and-limits)
before you rely on it.

- [How it works](#how-it-works)
- [Supported agents](#supported-agents)
- [Delivery modes](#delivery-modes)
- [Quick start](#quick-start)
- [Daily use](#daily-use)
- [Cost](#cost)
- [Privacy](#privacy)
- [Evidence and limits](#evidence-and-limits)
- [Upgrade and uninstall](#upgrade-and-uninstall)
- [Development](#development)

Reference: [Configuration](https://github.com/debe/borg_ai/blob/main/docs/configuration.md) · [Team sync](https://github.com/debe/borg_ai/blob/main/docs/team-sync.md) ·
[Commands](https://github.com/debe/borg_ai/blob/main/docs/commands.md) · [Statistics and the live page](https://github.com/debe/borg_ai/blob/main/docs/statistics.md) ·
[Harnesses](https://github.com/debe/borg_ai/blob/main/docs/harnesses.md) · [Own relay](https://github.com/debe/borg_ai/blob/main/docs/relay.md) · [Reports](https://github.com/debe/borg_ai/tree/main/reports) ·
[Contributing](https://github.com/debe/borg_ai/blob/main/CONTRIBUTING.md) · [Security](https://github.com/debe/borg_ai/blob/main/SECURITY.md)

## How it works

A **drone** is one agent session. A **lesson** is what a correction becomes: a question the agent should
ask itself, the situations where it applies, and the consequence it prevents. A **collective** is one
shared memory for a set of repos and engineers.

![A correction becomes a lesson, changes another drone's work, reaches a colleague over the mesh and becomes long-term memory](https://raw.githubusercontent.com/debe/borg_ai/main/docs/img/one-correction.svg)

**Learning.** Borg records an *episode* when you push back on the agent ("no, use a channel") or reject a
tool call with a reason. At session end, `borg learn` makes one model call per episode and writes a
lesson. A new correction on the same point updates the existing lesson.

**Delivery.** Borg spends tokens only where a lesson is likely to matter:

![One turn: free early delivery, a free word gate at the turn end, one graded model call, an extra round only for a grade-3 lesson](https://raw.githubusercontent.com/debe/borg_ai/main/docs/img/turn.svg)

1. **Early, free.** When a prompt or touched file first names a lesson's topic, Borg adds that topic's
   top lessons to the context. Word matching only. Claude Code only so far.
2. **Turn end, graded.** A free word gate skips most turns. Otherwise one model call grades every
   eligible lesson: 3 = certain, 2 = likely, 1 = worth a thought. A grade 3 blocks the stop and the agent
   works one more round. Grades 2 and 1 arrive free with the next prompt or touch. In
   [live mode](#delivery-modes), the default, the same call also runs during the turn, after the agent
   changes something.
3. **Judged by meaning.** The same call checks whether earlier questions changed the agent's work.
4. **System gate.** When the repo maps to a known system, lessons scoped to other systems stay out, even
   if the conversation names them.

**Memory.** Use strengthens a lesson; disuse lets it fade. A lesson unused for about 54 sessions of its
collective goes dormant. Only a strong match wakes it, and nothing is deleted. The clock counts
sessions, not days, so a holiday erases nothing.

<p align="center"><img src="https://raw.githubusercontent.com/debe/borg_ai/main/docs/img/memory.svg" width="480" alt="Activation of a lesson rises at each use and decays between uses; spaced uses keep it above the dormancy line"></p>

**Reach.** A correction about the code becomes a team lesson. One about how *you* like the agent to
work (commit style, AI attribution, branching) is personal: it goes to your private collective and
never reaches the team. A convention of one repo is delivered only there. See
[Who hears a lesson](https://github.com/debe/borg_ai/blob/main/docs/configuration.md#who-hears-a-lesson).

**Collectives.** Each repo path belongs to one collective, or to none:

- **team** collectives sync to their members over the [peer mesh](https://github.com/debe/borg_ai/blob/main/docs/team-sync.md);
- **private** collectives (`"share": "local"`) never leave the machine;
- **off** repos are invisible: Borg returns before it reads or writes anything.

A session reads only its own collective's lessons.

**Also included:**

- `borg why <file>:<line>` shows the redacted reasoning behind a commit, kept as a git note
  (`refs/notes/borg`, never pushed).
- `borg top` lists the drones running across the team; `borg steer` sends them a signed instruction.
- **Expert repos:** a collective can name local checkouts of its dependencies. A turn end that touches
  one gets at most three cited claims from that checkout, at the pinned version.
- **The queen** holds open decisions for the engineer. A drone asks one at the first prompt of a session.
  See [Answering the queen](https://github.com/debe/borg_ai/blob/main/docs/commands.md#answering-the-queen).

When memory cannot answer, the design escalates. Steps 1, 2 and 4 are built; step 3 is not:

![Escalation: memory, an expert repo, a live drone, the engineer](https://raw.githubusercontent.com/debe/borg_ai/main/docs/img/escalation.svg)

![Layers: harness hooks, the borg CLI, the local store and the turn-end check, the peer mesh, team trust](https://raw.githubusercontent.com/debe/borg_ai/main/docs/img/architecture.svg)

## Supported agents

| Agent | Wired by `borg install` | Gets |
|---|---|---|
| Claude Code | Command hooks in `~/.claude/settings.json` | Every feature, including early delivery, steering at the next tool call, and queen questions as a dialog |
| omp (oh-my-pi) 18.8.5, 18.10.4 | The extension [adapters/omp](https://github.com/debe/borg_ai/blob/main/adapters/omp/README.md) | Live mode (omp 18.4.4 or newer), the turn-end check, learning and steering at the stop |
| opencode 1.19.0, 2.0.29 | The plugin [adapters/opencode](https://github.com/debe/borg_ai/blob/main/adapters/opencode/README.md) | The same as omp |
| Codex CLI 0.153.2 | Command hooks in `~/.codex/hooks.json` ([adapters/codex](https://github.com/debe/borg_ai/blob/main/adapters/codex/README.md)); trust them with `/hooks` in codex, and again when borg's path changes (`borg doctor` says when codex skips them) | The same as omp, without learning from denied tool calls (Codex reports none). Borg's own model calls need `claude`, `omp` or `opencode` on `PATH`, or `codex:<model>` pinned |

Each agent is one module behind one trait. The same conformance suite checks every agent: install,
hooks, live mode and the drones page. [Harnesses](https://github.com/debe/borg_ai/blob/main/docs/harnesses.md) has the capability table, how to
add an agent, and the measured gates.

## Delivery modes

Each collective delivers in one of two modes. Both use the same lessons, the same graded model call and
the same questions. They differ in *when* the call runs.

- **live** (the default): the call also runs *during* the turn, so a lesson can land before a mistake spreads across
  ten edits. It reuses the hook machinery: no daemon, no extra process between turns.
- **hook**: the call runs at the turn end only. A lesson that applies mostly arrives after the work is
  done, and a grade 3 costs the agent a redo round. It costs about a tenth of live on borg's side.

| | hook | live |
|---|---|---|
| Prompt | Early delivery and waiting grade-2/1 lessons, free | The same, plus questions from a live check that finished since |
| Tool call | Early delivery, free | The same. After a state-changing call (an edit, or a shell command that is not read-only), a check of the turn so far starts in the background; a later tool call starts one while changes wait unchecked. A live check has no word gate: a turn half done has too few words for it. Its grade-3 lessons arrive with the next tool call as "borg check of your work so far"; the agent answers them as it works. Grades 2 and 1 arrive free, as at the turn end |
| Turn end | The free gate, then one graded call; a grade 3 blocks for one more round | The same backstop. When a live check that started this turn covered every change and nothing changed since, the stop skips its model call. A live check's grade 3 that no tool call delivered yet blocks the stop, as in hook mode. Lessons asked live are not asked again |
| Model calls | At most one per turn end; the gate skips most | One per burst of changes (at most one running at a time, at most one start per 45 s), plus the turn end when changes came after the last check |
| Added latency | None mid-turn; ~10 s at a checked turn end | None: the hook starts the check and returns at once. A stop waits at most 15 s (omp: 7 s) for a check still running |
| Harnesses | Claude Code, omp, opencode | Claude Code, omp (≥18.4.4), opencode 1.x/2.x |

**Switch back to hook** per collective with `"mode": "hook"` in its entry of `collectives.json`, or for
one shell with `BORG_MODE=hook` (or `live`), which overrides the config. `BORG_LIVE_DEBOUNCE_S` sets the
least time between two checks (default 45). An unknown value means the default, live, and `borg doctor`
warns. Switching back
is the same edit; nothing is migrated.

**See what it does:** `borg status` and `borg doctor` print each collective's mode and where it came
from, and `borg status` counts live checks apart from turn-end checks. In `borg web`, the header and the
drone cards show the mode, the Conscience page splits live checks from turn-end checks and what the
questions led to by channel (live check or turn end), and the Cost page shows check cost by mode per day.

A live check still running when the stop gives up waiting (15 s; on omp 7 s, a quarter of its stop
budget) can finish after the turn ended; its grade-3 lessons then arrive with the next prompt, or never
if the session ends there.

**On omp and opencode** a tool call's context reaches the model before its next step, as on Claude
Code: omp takes it as the `tool_result` handler's `additionalContext` (a hidden message; the tool output
stays as it is), opencode 1.x appends it to the tool output, and opencode 2.x steers it into the session
as a hidden input. Neither harness writes a Claude transcript, so a live check there reads the turn's
tool calls and edits, not the agent's prose. The stop still reads the agent's last message.

Live mode needs the consequence model. Under `BORG_MATCH=bm25`, `BORG_ONLY` or `BORG_CRITIC` it does
nothing: the drones run in hook mode. `borg doctor` warns and `borg status` notes it on the mode line
(the hook's own stderr note is not shown by the agent).

**Why live is the default.** The rule was fixed before the run: live becomes the default if, on an
internal benchmark of real engineering tasks with graded outcomes, it passes more criteria than hook
(mean difference > 0, 95% CI lower bound ≥ −0.5) and causes no more redo rounds per run. 3 tasks × 3
seeds per mode, the same lessons, solver (Claude Opus 5) and check model (Sonnet 5.5):

| Per run | hook | live |
|---|---|---|
| Criteria passed, live − hook (8 pairs; one live run timed out and its pair is dropped) | | +0.38, 95% CI [0, +0.88] |
| Redo rounds from a blocked stop | 0.5 | 0 |
| Lessons delivered during the turn | 0 | 9.6 |
| Borg's own model cost | $0.07 | $0.56 |
| Whole run cost (agent, borg) | $4.97 | $5.05 |
| Wall time | 464 s | 467 s |

The rule fired. Live never passed fewer criteria than hook on the same task and seed; it passed more in
2 of 8 pairs. The gain is small and the sample is small (p = 0.50), so treat it as "not worse, sometimes
better, never blocked" rather than a proven win. Borg's side costs about 8× more ($0.49 more per task),
yet whole runs cost about the same ($5.05 vs $4.97). Switch back with `"mode": "hook"` when borg's own
spend matters more.

## Quick start

**Requirements**

- Linux or macOS (x86_64 or arm64 Linux, arm64 macOS for the release binaries). Tested on both.
- For `cargo install` or a build from source: Rust 1.91 or newer (edition 2024), via
  [rustup](https://rustup.rs). On macOS the vectors build needs the Xcode command line tools; if
  `esaxx-rs` cannot find the C++ headers, set
  `CXXFLAGS="-isystem $(xcrun --show-sdk-path)/usr/include/c++/v1"`.
- A harness: [Claude Code](https://docs.anthropic.com/en/docs/claude-code) gets every feature. omp
  (tested with 18.8.5 and 18.10.4; live mode needs 18.4.4 or newer) and opencode (1.19.0 and 2.0.29) get
  live mode, the turn-end check, learning and steering, but no early delivery yet
  ([omp](https://github.com/debe/borg_ai/blob/main/adapters/omp/README.md), [opencode](https://github.com/debe/borg_ai/blob/main/adapters/opencode/README.md)). The Codex CLI (tested with
  0.153.2) gets the same ([codex](https://github.com/debe/borg_ai/blob/main/adapters/codex/README.md)).
- One logged-in model CLI on `PATH`: `claude`, `omp` or `opencode`. Borg's model steps (learning, the
  turn-end check) run headless through the harness the drone runs in (Sonnet by default for `claude`,
  the configured default model for omp and opencode), and through another installed one if it gives
  no answer. Without any, Borg still records episodes and delivers by word matching, but learns nothing.
  `codex` runs them only when pinned: a Codex drone with no other CLI installed needs
  `BORG_PREDICT_MODEL=codex:<model>` and `BORG_LEARN_MODEL=codex:<model>`, or its live checks never judge
  and Borg learns nothing.

**1. Install.** Pick one of three ways.

*From crates.io* (the package is `borg-ai`; the binary is `borg`):

```sh
cargo install borg-ai --locked      # the CLI and hook binary, vectors included
cargo install borg-mesh --locked    # only for team sync, borg top across machines, borg steer
cargo install borg-web --locked     # only for borg web, the live page
```

*The release binaries* (all three, sha256-checked, into `~/.local/bin`, then `borg setup`):

```sh
curl -fsSL https://raw.githubusercontent.com/debe/borg_ai/main/scripts/install.sh | sh
```

`sh -s -- --version vX.Y.Z --dir DIR --no-setup` picks a version, a directory, or skips setup.
Read the script first if you prefer: it is [scripts/install.sh](https://github.com/debe/borg_ai/blob/main/scripts/install.sh).

*From source:*

```sh
git clone https://github.com/debe/borg_ai borg && cd borg
cargo install --path crates/borg --locked
cargo install --path crates/borg-mesh --locked
cargo install --path crates/borg-web --locked
```

`cargo install` puts them in `~/.cargo/bin`. The hooks call `borg` by absolute path, so keep it where
you installed it. `--no-default-features` builds a lean `borg` without vectors.

**2. Set up.**

```sh
borg setup
```

`borg setup` asks a few questions, shows everything it will change, and writes nothing until you
confirm:

- **Harnesses:** which agents to wire; the default is every one whose CLI is on `PATH`.
- **Collectives:** a personal collective (`"share": "local"`), a team or company collective (found one
  and become its admin, or join one with a `BORG1...` ticket), or both. Then the repo directories of
  each, the repos no collective may ever see, and where every other repo goes (`off` is recommended).
  An existing `collectives.json` is only ever added to, with a backup.
- **Team:** for a team you found, the relay (n0's public relays, `disabled` for LAN or VPN only, or
  your company's `iroh-relay`) and the [delivery mode](#delivery-modes). Then it runs `borg mesh init`,
  or `borg mesh join` with your ticket.
- **Vectors:** fetches the model (~490 MB, once) and indexes every store with lessons. A failed
  download asks for a mirror (`BORG_VECTOR_URL`) and never stops setup.
- **Wiring:** the hooks or adapter of each harness, and the `borg` skill, which teaches the agent itself
  to explain borg, edit `collectives.json` and run these commands for you.
- **Import:** optionally learns the 50 newest corrections from your past sessions (model calls).

It ends with `borg doctor`. A second run changes nothing. Flags answer the questions for scripts:

```sh
borg setup --yes --private ~/src                              # no questions; defaults for the rest
borg setup --team platform --paths ~/work --relay disabled    # found a team collective
borg setup --join BORG1... --paths ~/work                     # join one with a ticket
borg setup --no-vectors --harness claude,codex
```

For a team, continue with [Team sync](https://github.com/debe/borg_ai/blob/main/docs/team-sync.md); for a company,
[Company collective](https://github.com/debe/borg_ai/blob/main/docs/team-sync.md#company-collective).

### Manual setup

The same steps by hand.

**Configure collectives.** Do this before you wire the hooks. Without a config, every repo joins one
team collective named `default`, which a mesh would share. A minimal
`~/.config/borg/collectives.json`:

```json
{
  "default": "off",
  "collectives": [
    { "name": "platform-team", "paths": ["~/work"], "share": "team" },
    { "name": "private", "paths": ["~/personal"], "share": "local" }
  ]
}
```

Check the routing with `borg collective ~/work/some-repo`. Rules, the `off` list and engineer aliases are
in [Configuration](https://github.com/debe/borg_ai/blob/main/docs/configuration.md#collectives).

**Wire the hooks.** Every step is a dry run until `--apply`, and each changed file gets a backup.

```sh
borg install all              # dry run: prints the diff for claude, omp, opencode and codex
borg install all --apply
borg doctor                   # checks wiring, the skill, config, the model CLI, the stores and vectors
```

**Vectors.** On by default, once the model is there:

```sh
borg vectors --fetch          # ~490 MB, once; then each store indexes itself as lessons arrive
borg vectors --rebuild        # only for a store that already has lessons
```

**Import what you have (optional).**

```sh
borg assimilate --apply --limit 50                                  # corrections from past sessions
borg assimilate --from-dir ~/work/handbook --plan /tmp/plan.json    # team documents: writes a plan (model calls)
borg assimilate --from-dir ~/work/handbook --apply --plan /tmp/plan.json
```

Details: [Import](https://github.com/debe/borg_ai/blob/main/docs/configuration.md#import-what-you-already-have),
[Vectors](https://github.com/debe/borg_ai/blob/main/docs/configuration.md#vectors). For a team, continue with [Team sync](https://github.com/debe/borg_ai/blob/main/docs/team-sync.md).

## Daily use

Borg runs on its own. These commands show what it does:

| Command | Shows or does |
|---|---|
| `borg status` | Each collective's mode, lessons, episodes and outcomes, and what is wired |
| `borg doctor` | What is broken: config, hooks, adapters, the skill, the model CLI (claude, omp or opencode), vectors |
| `borg collective [DIR]` | Which collective a directory belongs to |
| `borg queen` | Open decisions waiting for you; `borg queen ask` answers them one by one |
| `borg top` · `borg steer` | Running drones across the team; send them a signed instruction |
| `borg why <path>:<line>` | The reasoning behind a line of code |
| `borg web` | A live page of the knowledge and what the collective did, on 127.0.0.1 |

All commands: [Commands](https://github.com/debe/borg_ai/blob/main/docs/commands.md). Every switch: [Environment variables](https://github.com/debe/borg_ai/blob/main/docs/configuration.md#environment-variables).

## Cost

- **Most turn ends cost 0 tokens** in hook mode. The free gate skipped 78% of real turn ends. Early
  delivery and grade-2 and grade-1 lessons never call a model.
- **A turn-end call** costs about $0.07 (Sonnet, mean over 67 calls) and takes about 10 s.
- **An extra round** happens only for a grade-3 lesson at the turn end, billed by your harness as normal
  agent work.
- **Live mode** (the default) adds one call of the same kind per burst of changes during the turn, in the
  background. The turn end then skips its own call when a live check covered every change. Questions
  asked live cost no extra round. On the benchmark, borg's side cost $0.56 per task in live mode and
  $0.07 in hook mode. There is no cost cap: `borg status` and the Cost page show what each mode spent.
- **Learning** costs about $0.02 per correction (21 lessons from 10 sessions: $0.66).
- `assimilate --from-dir` makes one call per document chunk and prints each call's cost.

## Privacy

**Stays on your machine:** everything from off repos and private collectives; raw episodes (your exact
corrections), transcripts, source code, `collectives.json`, per-session state.

**Goes to team members,** for team collectives only:

- lessons: question, situations, consequence, scope, author emails, repo names;
- reasoning excerpts of landed commits, redacted, each held by two members;
- presence for `borg top`: session, harness, email, repo, first line of your latest prompt (redacted),
  recently touched paths;
- steering instructions and the signed member list;
- daily usage counts per engineer: checks, questions, outcomes, corrections, cost, lesson ids
  ([Statistics](https://github.com/debe/borg_ai/blob/main/docs/statistics.md#privacy)).

**A mesh relay** sees node ids, IP addresses and traffic volume, never content. Run your own relay to
keep even that in-house; see [Privacy on the mesh](https://github.com/debe/borg_ai/blob/main/docs/team-sync.md#privacy-on-the-mesh) and
[Running your own relay](https://github.com/debe/borg_ai/blob/main/docs/relay.md).

**The model provider** receives what each model step needs: the turn and the eligible lessons, or the
episode being learned.

Redaction (`redact` in `crates/borg/src/notes.rs`) strips tokens, keys, passwords and `.env` values from
excerpts and presence. Lessons skip it: the model writes them as general questions, but read
`lessons.jsonl` before you share a collective that handled secrets.

**Hooks fail open.** A missing binary, a crash, an invalid config, an unreachable model or a locked index
lets the agent continue untouched. The turn-end call stops after 45 s (on omp after at most 18 s, inside
its fixed 30 s stop limit); if it fails, the turn ends without questions. A live check that fails asks
nothing, and the turn end checks as in hook mode. Unlearned episodes wait for the next run.

## Evidence and limits

Each change to the defaults was tested against a rule written before the paid run. Most tests used the
author's and a few colleagues' sessions, so their data is not in this repository.

| Experiment | Result |
|---|---|
| Lessons at the turn end, 15 real decisions, sealed sandbox | engineer points raised 34% → 63% (significant) |
| Graded turn end vs. the earlier shortlist | relevant lessons reaching the agent 45% → 64% (+0.17 [+0.09, +0.25]); extra rounds with nothing relevant 8 → 3 of 16 |
| What a memory keeps, 10 real sessions | Borg 21 of 21 lessons useful (95% CI 84.5–100%); Hindsight 169 of 543 items |
| Against Hindsight, same memory model | accuracy tied; with Sonnet-built lessons, +29 points on lesson cases (significant) |
| Team documents condensed into lessons | +1.7 knowledge checks per run over a plain agent; level with the hand-written skill |
| System gate, real cross-system session | wrong-system deliveries 83 → 0; no relevant lesson lost |
| Free gate, 1,117 real turn ends | 78% skipped at 0 tokens |
| Vectors | shortlist recall +0.09, +0.14 on German work, ~120 ms |
| Public data ([SWE-chat](https://github.com/debe/borg_ai/blob/main/reports/swe-chat.md)), 150 corrections in 15 strangers' repos | no memory helped, Borg included; only ~3% of corrections repeat an earlier one |
| End-to-end run in a sandboxed home ([dogfood](https://github.com/debe/borg_ai/blob/main/reports/dogfood-e2e.md)) | all 18 plumbing checks pass; no measurable gain on that task |
| Live vs hook mode, internal task benchmark (9 tasks per mode) | live +0.38 criteria per task [0, +0.88], 0 redo rounds vs 0.5; live is the default |

**Known limits:**

- Most measurements come from the author's and a few colleagues' sessions, not from a broad user base.
- Small samples (16 negative and 51 positive turn-end cases; 10 sessions for memory quality). The
  intervals are wide.
- Turn-end asking of the engineer (`BORG_QUEEN=on`) failed its test and is off.
- omp, opencode and Codex get no early delivery yet.
- omp gives a stop 30 s, which no setting changes. borg budgets 28 s, so the turn-end call there stops
  after at most 18 s (less what the stop spent before it); about one in eight measured turn-end calls took longer, and such a turn ends without
  questions. A stop that a live check covered makes no call.
- On omp, opencode and Codex a live check sees the turn's tool calls and edits, not the agent's prose.
- Codex skips an untrusted hook without a word, and fires no PostToolUse for a failed `apply_patch`, a
  patch run through a shell heredoc or a command still running when it yields: borg never sees those
  edits. In code mode borg's context reaches the model only after the whole `exec` script, and codex cuts
  context over about 2,500 tokens ([codex](https://github.com/debe/borg_ai/blob/main/adapters/codex/README.md#what-borg-misses)).
- opencode 1.x runs no hook after a failed tool call, so a live check's questions wait for the next
  successful call, the next prompt or the stop. Its tool-call context is stored in the tool output: the
  TUI hides it for built-in tools, but session exports show it.
- Borg learns only from corrections. Knowledge nobody corrected enters only through
  `assimilate --from-dir`.

## Upgrade and uninstall

```sh
cargo install borg-ai --locked --force       # or install.sh again, or git pull and --path crates/borg
cargo install borg-mesh --locked --force     # if installed
borg install all              # dry run: shows adapter, hook and skill changes
borg install all --apply      # only if the dry run shows a diff
borg doctor
```

Claude Code hooks call the binary by path, so a reinstall needs no rewiring. Codex hooks do too, but
codex trusts each hook's command and timeout: after an upgrade that moves borg or changes a hook's
timeout, codex skips borg's hooks until you trust them again with `/hooks` in codex, and `borg doctor`
says so. Stores are append-only JSONL and need no migration.

```sh
borg uninstall all --apply    # removes only what install added; keeps a backup
cargo uninstall borg-ai borg-mesh borg-web    # or delete them from ~/.local/bin after install.sh
```

Uninstall leaves an edited adapter or skill file in place and reports it. Your data stays too: delete
`~/.local/share/borg` and `~/.config/borg` to remove it, and `git update-ref -d refs/notes/borg` in each
repo for the reasoning notes.

## Development

```sh
scripts/check.sh                              # everything below; run before a release
cargo test --workspace
cargo test -p borg-ai --no-default-features   # the lean build
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all                               # max_width 160
node --test adapters/omp/borg.test.ts         # adapter tests; need target/debug/borg and Node 23.6+ or bun
node --test adapters/opencode/borg.test.ts
node --test adapters/conformance/conformance.test.ts   # harness conformance (docs/harnesses.md)
scripts/arch.sh                               # the harness abstraction's metrics and gates
python3 scripts/figures.py                    # regenerates the figures in docs/img
```

| Path | What |
|---|---|
| `crates/borg` | The product (crate `borg-ai`, binary and library `borg`): CLI, hooks, learning, turn-end check, memory, collectives, install, vectors. `assets/` holds the skill and the TS adapters it compiles in |
| `crates/borg-mesh` | Peer sync, membership, presence, steering (iroh); separate so the hook binary stays small |
| `crates/borg-trace` | Parses Claude Code and omp transcripts |
| `crates/borg-web` | `borg web`, the live page; `dist/` is its built Leptos client (`ui/`), committed |
| `crates/borg-eval`, `crates/borg-search`, `eval/` | Eval harness and benchmarks; not shipped, use local paths under `~/.local/share/borg/eval/` |
| `adapters/omp`, `adapters/opencode` | TypeScript adapters, compiled into `borg` (`borg.ts` links to `crates/borg/assets/adapters/`) |
| `skills/borg` | The `borg` skill `borg install` copies into each harness (a link to `crates/borg/assets/skills/borg`) |
| `adapters/conformance` | The harness conformance suite for the adapters ([Harnesses](https://github.com/debe/borg_ai/blob/main/docs/harnesses.md)) |
| `reports/` | Measured results |

Contributing: [CONTRIBUTING.md](https://github.com/debe/borg_ai/blob/main/CONTRIBUTING.md). Security reports: [SECURITY.md](https://github.com/debe/borg_ai/blob/main/SECURITY.md).

License: MIT OR Apache-2.0 ([LICENSE-MIT](https://github.com/debe/borg_ai/blob/main/LICENSE-MIT), [LICENSE-APACHE](https://github.com/debe/borg_ai/blob/main/LICENSE-APACHE)).
