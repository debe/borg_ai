# Configuration

[← README](../README.md)

- [Install](#install)
- [Collectives](#collectives)
- [Who hears a lesson](#who-hears-a-lesson)
- [Storage](#storage)
- [System map](#system-map)
- [What `borg install` writes](#what-borg-install-writes)
- [Import what you already have](#import-what-you-already-have)
- [Vectors](#vectors)
- [Decision → fix links](#decision--fix-links)
- [Review → fix findings](#review--fix-findings)
- [Environment variables](#environment-variables)

## Install

**Requirements**

- Linux (x86_64 or arm64) or macOS on Apple silicon; release binaries for all three. Intel Macs are not
  covered by the release binaries (`cargo install borg-ai` may still build there).
- For `cargo install` or a build from source: Rust 1.91 or newer (edition 2024), via
  [rustup](https://rustup.rs). On macOS the vectors build needs the Xcode command line tools; if
  `esaxx-rs` cannot find the C++ headers, set
  `CXXFLAGS="-isystem $(xcrun --show-sdk-path)/usr/include/c++/v1"`.
- A harness ([Harnesses](harnesses.md)) and one logged-in model CLI on `PATH` (see *Model runner* below).

**Three ways to install.** The one-liner puts prebuilt `borg`, `borg-mesh` and `borg-web`, sha256-checked,
into `~/.local/bin` and runs `borg setup`; options go after `sh -s --`
([scripts/install.sh](../scripts/install.sh)):

```sh
curl -fsSL https://raw.githubusercontent.com/debe/borg_ai/main/scripts/install.sh | sh
curl -fsSL https://raw.githubusercontent.com/debe/borg_ai/main/scripts/install.sh | sh -s -- --version v0.5.0 --dir ~/bin --no-setup
```

From crates.io (the package is `borg-ai`; the binary is `borg`), or from a checkout:

```sh
cargo install borg-ai --locked      # the CLI and hook binary, vectors included
cargo install borg-mesh --locked    # only for a team: sync, messages and borg top across machines
cargo install borg-web --locked     # only for borg web, the live page

git clone https://github.com/debe/borg_ai borg && cd borg
cargo install --path crates/borg --locked    # and crates/borg-mesh, crates/borg-web
```

`cargo install` puts them in `~/.cargo/bin`. The hooks call `borg` by absolute path, so keep it where
you installed it. `--no-default-features` builds a lean `borg` without vectors.

**`borg setup`** asks a few questions, shows everything it will change, and writes nothing until you
confirm:

- **Harnesses:** which agents to wire; the default is every one whose CLI is on `PATH`.
- **Collectives:** a personal collective (`"share": "local"`), a team or company collective (found one
  and become its admin, or join one with a `BORG1...` ticket), or both. Then the repo directories of
  each, the repos no collective may ever see, and where every other repo goes (`off` is recommended).
  An existing `collectives.json` is only ever added to, with a backup.
- **Team:** for a team you found, the relay (n0's public relays, `disabled` for LAN or VPN only, or
  your company's `iroh-relay`) and the [delivery mode](delivery.md#delivery-modes). Then it runs
  `borg mesh init`, or `borg mesh join` with your ticket.
- **Vectors:** fetches the model (~490 MB, once) and indexes every store with lessons. A failed
  download asks for a mirror (`BORG_VECTOR_URL`) and never stops setup.
- **Wiring:** the hooks or adapter of each harness, and the `borg` skill, which teaches the agent itself
  to explain borg, edit `collectives.json`, message other sessions and run these commands for you.
- **Import:** optionally learns the 50 newest corrections from your past sessions (model calls).

It ends with `borg doctor`. A second run changes nothing. Its flags are in [Commands](commands.md#overview).

**By hand.** Configure collectives first ([below](#collectives)): without a config, every repo joins one
team collective named `default`, which a mesh would share. Check the routing with
`borg collective ~/work/some-repo`. Then:

```sh
borg install all              # dry run: prints the diff for claude, omp, opencode and codex
borg install all --apply      # each changed file gets a backup
borg doctor                   # checks wiring, the skill, config, the model CLI, the stores and vectors
borg vectors --fetch          # ~490 MB, once; then each store indexes itself as lessons arrive
borg vectors --rebuild        # only for a store that already has lessons
borg assimilate --apply --limit 50                                  # corrections from past sessions
borg assimilate --from-dir ~/work/handbook --plan /tmp/plan.json    # team documents: writes a plan (model calls)
borg assimilate --from-dir ~/work/handbook --apply --plan /tmp/plan.json
```

For a team, continue with [Team sync](team-sync.md).

## Collectives

`~/.config/borg/collectives.json` (or `BORG_CONFIG`) routes every repo path:

```json
{
  "default": "off",
  "off": ["~/work/payroll-secrets", "~/personal/journal"],
  "collectives": [
    { "name": "platform-team", "paths": ["~/work"], "share": "team", "mode": "live" },
    { "name": "private", "paths": ["~/personal"], "share": "local" }
  ],
  "engineers": [
    { "name": "alex", "emails": ["alex.doe@example.com", "alex@home.example"] }
  ]
}
```

Borg applies the first rule that matches:

1. A path under `off` wins over everything.
2. The collective with the longest matching path.
3. `default`: `"off"` or a collective's name.

Paths match whole components: `~/work` does not match `~/work-old`.

| Key | Meaning |
|---|---|
| `share` | `"team"` (default) syncs over the mesh; `"local"` never leaves the machine |
| `engineers` | Optional. Merges one person's git emails into one author for trust, `borg status` and `borg top`. Never synced |
| `relay` | Optional, per collective. How the mesh crosses NAT; see [Relays](team-sync.md#how-peers-find-each-other) |
| `mode` | Optional, per collective. `"live"` (default): the conscience judges the work at the turn end and also during the turn, after state-changing tool calls (Claude Code, omp ≥18.4.4, opencode 1.x and 2.x, Codex). `"hook"`: at the turn end only, about an eighth of live's own cost. `BORG_MODE` overrides it. An unknown value is live, with a warning in `borg doctor`. See [Delivery modes](delivery.md#delivery-modes) |
| `experts` | Optional, per collective. Local checkouts of dependencies to cite at the turn end (`crates/borg/src/expert_route.rs`). `"drone": true` also asks a read-only agent in the checkout when its index has nothing; it is bounded by time (120 s at most, and what the stop's budget leaves), and its cost is measured, never capped |
| `budget` | Optional, per collective; none by default. The pool tokens this collective's checks may read per session, on average. Without it, lesson selection's spend is measured and shown, never priced. With it, the learned policy (the default) prices a larger pool so that the replayed spend stays within the budget, from its first fit that knows the budget, its gate open or not; the fixed rule (`BORG_SELECT=fixed`) reads no budget. `borg status`, `borg top` and borg web show the budget, what was read and the price. A value that is not a positive number counts as none and never invalidates the config. See [Which lessons are asked](delivery.md#which-lessons-are-asked) |
| `risk` | Optional, per collective; 0.1 by default. The engineer's tolerance for learning: the share of the fixed rule's replayed quality (mistakes avoided) the learned policy's gate (the default) lets it risk below the fixed rule while it learns, for its deviations on evidence and its randomised question-count test on live checks alike. The test never withholds a lesson; this share rations how often it may move one between asked and handed over. `0` turns the test off, and a deviation on evidence then plays only where what the gate's earlier deviations gained covers its risk; `1` lets the gate risk the whole value. The monitor's revert keeps its own 10%. The fixed rule (`BORG_SELECT=fixed`) reads no `risk`. Each graded check logs the share it was decided under (`alpha` on its record, under every policy; absent at 0.1), and `borg debug select --replay` decides each check under its logged value, not whatever the config says then. `borg status`, `borg top` and borg web show the share in force. A value that is not a number from 0 to 1 counts as none and never invalidates the config. See [The question-count test](delivery.md#which-lessons-are-asked) |
| `personal` | Optional, top level. The `local` collective that receives personal lessons; needed only when you have several |

An invalid config (bad JSON, two collectives sharing one store, an email under two engineers, `personal`
naming a collective that is not `local`) routes
**everything off**, and `borg doctor` reports it.

```sh
borg collective ~/work/some-repo   # -> platform-team (Team, may sync: true) store ~/.local/share/borg/collectives/platform-team
borg collective ~/personal/notes   # -> private (Local, may sync: false) ...
```

## Who hears a lesson

`borg learn` gives every lesson a **reach**:

| Reach | What it is | Where it goes |
|---|---|---|
| team | An engineering concern about the code, design or verification | The collective it was learned in, delivered to everyone |
| repo | A convention of one repo, such as its branch or deploy workflow | The same collective, delivered only in that repo |
| personal | How you like the agent to work: commit style, AI attribution, when to branch or push, tone | Your private collective, which never syncs; without one it is dropped |

The model decides by asking: would a teammate working on the same code get this correction too?
Imported team documents are always team; their repo conventions are scoped by system already.

Reach also sets which sessions make an unused lesson fade ([Memory](learning.md#memory)). A team lesson ages with
every session of its collective, a teammate's included: everyone hears it, so ~54 sessions in which no
one's agent acts on it put it to sleep, even when its author is away. A repo lesson ages only with
sessions in its repo, so work in other repos, yours or a teammate's, never makes it dormant. A
session's repo is the one it started in: a session that edits another repo does not age that repo's
conventions (a use there still counts). The system gate is not applied to aging, and in the team view a
repo is a case-sensitive name, so a clone whose directory differs in case does not age its lessons.

Personal lessons go to the `local` collective named by `personal`, or to your only `local` collective.
A personal lesson already shared with the team is withdrawn there: a newer version replaces its text
with a tombstone that is never delivered, and peers receive that version too.

**Clean up lessons learned before reach existed:**

```sh
borg reach                              # dry run: one model call per lesson learned from sessions (~$0.02 each), writes a plan
borg reach --apply --plan <store>/reach-plan.json
```

The plan lists what moves to your private collective, what becomes repo-only, and what the model was
unsure about. `--apply` makes the sure moves and turns the unsure ones into queen questions, which you
answer with `borg queen`. Answering one moves the lesson; it writes no new lesson.

## Storage

Each collective keeps plain files in `~/.local/share/borg/collectives/<name>/` (`BORG_HOME` moves the
root). The mesh syncs the files marked *synced*, and only for a team collective.

| File | Holds |
|---|---|
| `lessons.jsonl` | The lessons. Synced |
| `episodes.jsonl` | The corrections, vetoes and regrets `borg learn` turns into lessons |
| `outcomes.jsonl` | What followed each lesson asked: ignored, changed, corrected, vetoed, regretted, tested. Also every graded check with its routes and propensities, what delivery did with each lesson (`delivery`), each run of a session's end hook (`ended`), and the judge's verdicts on placebos |
| `select-summary.json` | The lesson selection summary the hooks read: the outcome log folded and the policy's fit, rebuilt from the log whenever it is missing or of another format. Local, never synced. `select-summary.json.lock` (local) lets one learned check of the store decide and record at a time; the fixed rule takes no lock. `select-reverted` (local) is the monitor's mark that put the fixed rule back: delete it to let the learned policy learn again |
| `outcomes-team.jsonl` | The outcomes redacted for the team: lesson ids, what followed, when, the asking probability, session and repo hashed. Synced. `outcomes-team.published` (local) remembers how much was published |
| `stats.jsonl` | One usage rollup per node, engineer and day: counts, cost, lesson ids. Synced. The raw events stay in `stats/events.jsonl` |
| `constraints.jsonl` | Constraints ratified from a [`borg no`](commands.md#saying-no). Synced |
| `messages.jsonl` | [Messages](commands.md#messages) between sessions, pauses, resumes and vetoes, and the `seen` lines that acknowledge them: a week at most, and of each member's lines the newest of each class within 1 MiB. Synced. `messages/` (local) records which session saw each one, which pauses and vetoes count here, and `messages/generation` the log's talk generation, which moves only when a tell, ask or steer is written, so an acknowledgement makes no hook or waiter read the log again |
| `reasoning.jsonl` | Redacted reasoning excerpts and [decision → fix links](#decision--fix-links), replicated by the mesh |
| `lessons.jsonl.lock` | Lets one `borg learn` run per collective at a time |
| `queen/` | Open questions for the engineer and their answers |
| `presence/` | One record per running session, for `borg top` and message routing |
| `listen/` | The sessions that asked to be woken by a message, and their waiters |
| `vectors/`, `mesh/` | The vector index; the mesh's own state |

Per-session scratch state lives in `/tmp/borg` (`BORG_STATE`).

## System map

The system gate keeps lessons scoped to other systems out of a repo that maps to a known system. Put
your map in `~/.config/borg/systems.json` (or `BORG_SYSTEMS`), in the format of
[`crates/borg/data/systems.json`](../crates/borg/data/systems.json). That example applies when you have
none.

## What `borg install` writes

`borg install` edits your **user** harness config. It prints a diff until you pass `--apply`, and backs
up every file it changes. A second run changes nothing. `--home DIR` installs into another home, for
testing.

- **Claude Code:** one hook per event in `~/.claude/settings.json` (or `$CLAUDE_CONFIG_DIR`):
  `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `Stop` (90 s timeout) and
  `SessionEnd`. Each runs `<path to borg> hook claude <event>`. Your other keys and hooks stay.
  `PreToolUse` costs one `stat` until someone first [pauses](commands.md#pause-and-resume) a session; then
  it refuses a paused session's tool calls.
- **omp:** copies the adapter to `~/.omp/agent/extensions/borg.ts` (or `$PI_CODING_AGENT_DIR/extensions`).
- **opencode:** copies the adapter to `~/.config/opencode/plugins/borg.ts` (or
  `$OPENCODE_CONFIG_DIR/plugins`, `$XDG_CONFIG_HOME/opencode/plugins`).
- **Codex CLI:** one hook per event appended to `~/.codex/hooks.json` (or `$CODEX_HOME/hooks.json`):
  `UserPromptSubmit` (30 s), `PreToolUse` (10 s), `PostToolUse` (30 s), `Stop` (90 s), `SessionEnd`
  (3 s, Codex's maximum) and `Interrupt` (3 s: an Esc ends the turn of a session that
  [listens](commands.md#waking-a-session)). Each runs `<path to borg> hook codex <event>`. Codex runs them only after you
  trust them: open codex and run `/hooks`. `borg doctor` reports any of borg's hooks codex does not trust.
- **Upgrades:** install and uninstall also remove the hooks and skill files an older borg installed and
  this one no longer uses, such as Claude Code's `SessionStart` hook from 0.3 and before.

- **The `borg` skill,** for every harness above: `SKILL.md` and `references/*.md` in
  `~/.claude/skills/borg` (or `$CLAUDE_CONFIG_DIR/skills/borg`), `~/.omp/agent/skills/borg`,
  `~/.config/opencode/skills/borg` and `~/.codex/skills/borg` (or `$CODEX_HOME/skills/borg`). It teaches
  the agent what borg is, how `collectives.json` works, when to run `setup`, `doctor`, `mesh` and
  `vectors`, and how to message other sessions (`tell`, `ask`, `inbox`, `listen`); it tells the agent to show you every dry run before `--apply`. Each file is copied whole.
  Uninstall removes only files exactly as this borg ships them, then the empty directory; a file you
  edited stays and is named. A skill directory that is a symlink is yours and left alone. `borg status`
  and `borg doctor` report a missing or stale skill; `borg install <harness> --apply` updates it there.
  A replaced skill file is backed up beside it, so after an upgrade that changed the skill, uninstall
  keeps those backups and their directory.

The adapters and the skill are compiled into the binary, so an installed `borg` needs no checkout.
After an upgrade, `borg install all --apply` brings both up to date.

## Import what you already have

**Past sessions.** Mine your Claude Code, omp, opencode and Codex sessions for corrections and for your
answers to the agent's own questions:

```sh
borg assimilate                         # dry run: what it found, per collective and harness; no model calls
borg assimilate --apply --limit 50      # learn the newest 50 new episodes (model calls)
borg assimilate --apply --since 2026-09-01
borg assimilate --session <id>          # one session only, to check what it gives
```

It reads `$CLAUDE_CONFIG_DIR/projects` (default `~/.claude/projects`), `$PI_CODING_AGENT_DIR/sessions`
(default `~/.omp/agent/sessions`) and `$CODEX_HOME/sessions` (default `~/.codex/sessions`), or
`--claude DIR`, `--omp DIR`, `--codex DIR`, and opencode
1.x and 2.x sessions through the `opencode` CLI when it is installed (read-only exports; `--opencode DIR`
reads a directory of `opencode export` / `opencode session export` files instead). opencode 1.x lists
every session with one query. opencode 2.x lists sessions per project only, so Borg asks it for the
projects it knows: the directories of your Claude Code and omp sessions, the collectives' paths and
the git repos up to two levels inside them. opencode 2.x does not list the sessions it migrated from
1.x, under any project; export those with an opencode 1.x (2.x keeps the 1.x tables) into a directory
and pass `--opencode DIR`. Sub-agent sessions and Borg's own model calls are skipped. The first output lines count the sessions
per source, then per harness (with the version the newest episode was written by) the new rejected
tool calls, answers and prompts and the episodes already present. A dry run ends with the learn cost:
one model call per episode, at the mean cost of this machine's recorded learn calls (unknown until
one reported a cost). Answers (Claude Code `AskUserQuestion`, omp `ask`, opencode `question`, Codex
`request_user_input`) become episodes marked `via: answer`; the learn call decides whether an answer
holds a lesson beyond its task. Codex rollouts give answers only: Codex records no rejected tool call.
Each episode routes by the directory it happened in, so off repos contribute nothing. A second run
adds nothing twice: an episode is the same when it has the same id (the harness's id for the tool
call), the same words in the same session as one the live hook recorded, or is a prompt of the
same session within 5 seconds. Two tool calls or answers with the same words are two episodes. `BORG_ASSIMILATE_SKIP=word1,word2` drops every episode that mentions one of those words, such as
an abandoned project's name.

**Team documents.** Turn a git checkout of written team knowledge into scoped lessons:

```sh
borg assimilate --from-dir ~/work/handbook --plan /tmp/plan.json           # model calls; writes only the plan
borg assimilate --from-dir ~/work/handbook --apply --plan /tmp/plan.json   # applies that plan; no model calls
```

**The dry run calls the model** (one call per chunk, 4 in parallel), so review the plan before you apply
it. Borg reads tracked Markdown files. The default globs fit a handbook layout:
`skills/*-knowledge/...`, `docs/*retrospectives/*.md`, `docs/**/*coverage-audit*.md`, and decision
markers in `docs/**/*.md`; change them with `--knowledge`, `--retros`, `--families` and `--decisions`.
Lessons go to the collective the directory routes to, unless you pass `--collective NAME` or
`--for PATH`. Open decisions go to the [queen's queue](commands.md#answering-the-queen).

## Vectors

Vectors find lessons that share no rare word with the work: paraphrases, German work against English
lessons, renamed identifiers. Only the turn-end check uses them.

They are **on by default**: built into the default binary and used unless `BORG_VECTORS=off`. They need
the model once; `borg setup` fetches it and indexes every store with lessons (`--no-vectors` skips
that). By hand:

```sh
borg vectors --fetch      # downloads the model once (~490 MB, pinned revision, sha256 checked)
borg vectors --rebuild    # indexes the store the current directory routes to (--store FILE: any store)
borg vectors              # model and index status
```

The model is Model2Vec potion-multilingual-128M. The index updates itself after `borg learn`,
`borg assimilate --apply` and a mesh sync that brought new lessons; the first of these builds a missing
index. `--rebuild` is needed only for a store that already had lessons when the model arrived, or whose
index another model built. `borg doctor` warns while the model or an index is missing and names the
command. No hook downloads the model; without model or index, Borg matches words only.

**Behind a proxy or offline.** `--fetch` downloads with `curl`, which honours `HTTPS_PROXY` and
`CURL_CA_BUNDLE`. `BORG_VECTOR_URL` points it at a mirror that serves the same three files
(`config.json`, `tokenizer.json`, `model.safetensors`), `file://` included; each must match its pinned
sha256. A lean build (`--no-default-features`) has no vectors.

## Decision → fix links

Some choices look fine and break weeks later. At each session end, after `borg notes attach` has
written the session's reasoning notes, the same background job looks for the earlier decisions the
session's commits fixed. `borg learn` starts only after it, so it sees what the job wrote. The hook
itself does no extra work.

**What links.** For each commit the session landed, the lines it removed or changed are blamed at its
parent (`git blame -w -M -C`: a reformat, a move or a copy in between keeps the original author). An
origin commit becomes a link when it

- has a Borg note or a reasoning record of this collective (one `borg notes fetch` brought counts);
- comes from another session;
- is at least `BORG_REGRESS_MIN_AGE` older (default `2d`, by commit times);
- is an ancestor of the fix and not linked to it yet.

Left out: merges and root commits, pure additions (they blame nobody), formatter hunks (identical once
whitespace and trailing commas are removed), moved lines (added again in another file, further away, or
as a block; a line swapped with its neighbour still counts), binary, lock, snapshot, minified and
generated files (`*.pb.*`, `*_generated.*`, `openapi*.json`, or `linguist-generated` / `-diff` in
`.gitattributes`), and any commit over 40 files or 2,000 changed lines (a refactor). At most 5 links per
session, fixes first. `borg regress --explain` names the reason for each.

**Fix or revisited.** A link is a *fix* when the commit reverts the decision (a `git revert` the session
made counts), or its subject reads like a fix (`fix: ...`, `Fix ...`, `resolves #12`, `ABC-12`,
"regression"), or both of these hold:

- a test run failed and a later one passed in the session (the hook notes `cargo test`, `pytest`,
  `npm test` and the like in `<state>/<session>.tests`, 0 tokens; a build that did not compile is no
  test run);
- the engineer's prompts or task name a bug, crash, regression, a failing or flaky test. "Error",
  "issue", a bare "fail" and "fail open" do not count.

Either of the last two alone is no fix: writing a feature test first also goes red, then green.
Otherwise the link is a weak *revisited* one, stored and shown but never learned from. A commit that
changed several decisions' lines is also weak for each decision it only renamed something in.

**Where it is written.** The decision's note gets a `borg fixed in:` section, the fix's note a
`borg fixes:` section; an older Borg shows them as part of the note's text (or of its episode list). A
forced `borg notes attach <commit>` rewrites the reasoning and keeps both sections. Everything is redacted before it
is written and again when `borg why` shows it.

| Repo | Both notes | Link record in `reasoning.jsonl`, replicated by the mesh | Regret episode (`BORG_REGRESS=learn`) |
|---|---|---|---|
| team collective | yes | yes | yes |
| private collective (`"share": "local"`) | yes | no | yes (stays on the machine) |
| off | no | no | no |

**Learning from it.** `BORG_REGRESS` sets how far it goes:

- `links` (default): record the links, learn nothing from them.
- `learn`: also make each fix link an episode of kind `regret` (id
  `R<decision>-<fix>`, written once). `borg learn` makes one model call for it, with the decision's task
  and proposal, the fix's task, subject, the fixing session's prompts and proposal, and a redacted diff
  of the decision's files, and words the lesson as a question
  for the decision point: "Before <doing X> in <situation>, has <risk Y> been considered?". Reach
  follows [the usual rules](#who-hears-a-lesson).
- `off`: no links.

**Regret reaches back.** With `learn`, each regret learned as a lesson also blames the lessons asked in
the deciding session before the decision commit; how much, and how it moves their usefulness, is under
[Outcomes](learning.md#outcomes-and-the-usefulness-belief). `BORG_REGRET_CREDIT=off` turns it off.

Before you switch to `learn`, check what the detector finds in your own history:

```sh
borg regress --replay HEAD~200..HEAD --explain
```

A replay takes each commit's session and task from its note. It has no test runs and no prompts, so it
cannot see the tests and words signals: a live run makes fix links a replay never showed, and those are
the ones most likely wrong. Before you switch, also check a few real sessions with
`borg regress --session ID --explain` (the session's own test runs and prompts). Method and results on a
scripted repo, which measures only the subject and revert signals:
[reports/regress.md](../reports/regress.md).

**Rebases.** A rebase or an amend gives commits new ids, and their notes stay behind unless git copies
them. Run this once per repo, or a rebased decision can no longer be linked:

```sh
git config notes.rewriteRef refs/notes/borg
```

**Fails open.** Each git call stops after `BORG_REGRESS_TIMEOUT_S` (10 s), and the whole run, regret
episodes included, after 60 s of git.
A call past either, a shallow clone's boundary or an unreadable diff means fewer links, never a failed
hook.

## Review → fix findings

Most bugs are not caught after a commit. They are caught inside a session: a workflow builds, a
reviewer finds a problem, and a fix step fixes it, all before anything is committed. Decision → fix
links never see these, because they need a commit from another session. `borg review-fix` reads them
from the sub-agents your harness ran, with 0 tokens:

| Harness | A run is | Read from |
|---|---|---|
| Claude Code | a workflow run, or a session's Agent-tool sub-agents | `~/.claude/projects/` (or `$CLAUDE_CONFIG_DIR/projects`), `<repo>/<session>/workflows/wf_*.json` and each agent's transcript under `<session>/subagents/workflows/<run>/`; the Agent tool's under `<session>/subagents/agent-<id>.jsonl`, with its description and type in `agent-<id>.meta.json`; one that a workflow's agent started joins that workflow's run |
| omp | a session's task agents | `~/.omp/agent/sessions/` (or `$PI_CODING_AGENT_DIR/sessions`), `<repo>/<ts>_<session>/<Task>.jsonl` beside the session's own file |
| opencode | a session's subagents | the child sessions its task calls name, exported through `opencode` itself, read-only; only when `~/.local/share/opencode` (or `$XDG_DATA_HOME/opencode`) exists. Every session at once: 1.x only (2.x has no query that lists them); at session end, both |
| Codex | a session's spawned agents | `~/.codex/sessions/` (or `$CODEX_HOME/sessions`): every rollout whose `session_meta` names the thread that spawned it, grouped under the session that spawned the first; history a forked agent inherited is not its own work. Built from codex's serde names, not yet checked against a real spawned rollout |

`--home DIR` reads under another home; the harnesses' own directory variables still win.

**What counts.** Workflow scripts and prompts are your own, so their output shapes vary. Borg reads
each agent's output tolerantly: Claude Code's last `StructuredOutput`; omp's accepted `yield` calls,
every finding gathered whether it yielded them one per call, as one array or inside one object
(`{overall_correctness, explanation, findings}`); otherwise the last JSON object in the agent's text
with a `findings` or `issues` array, starting a line (a fenced `json` block counts). opencode has no
structured output, nor do Claude Code's Agent-tool sub-agents or Codex's spawned agents, so only
reviewers told to answer in JSON yield findings there.

- A **finding** is an item of a `findings` or `issues` array that has a severity (or a `priority`: 0
  to 3 or `P0` to `P3` reads as `p0` to `p3`) and a text (`title`, `claim`, `problem`, `description`,
  `issue` or `body`). Its file is `file`, `path`, `file_path`, `fileLines` or `where`.
- A **verdict** is a verifier's `real` flag or a `verdict` word (confirmed, refuted, …). It is linked
  to its finding by id, title, label or prompt.
- A finding is **fixed** when a later fix step of the same run (its label, phase or agent type says
  fix or repair, and it found nothing itself) names the finding's file in any tool call or text.
- A finding is **strong** when it was fixed and no verifier rejected it. Only strong findings are
  learned from.

A negative control asks the same question of a fix step from another session in the same repo. That
rate is how often the file rule matches by chance. Run `borg review-fix` to see your own numbers.

**Learning from it.** On by default, with nothing to set: at session end, `borg learn` first makes the
session's strong findings episodes (from the runs of the drone's harness, `BORG_HARNESS`), of
kind `regret` (id `V<run>#<n>`, written once; `V<run>#<agent>.<n>` for a session's Agent-tool or
spawned agents, which finish in any order). Each episode goes to the store its repo routes to; an
off repo is skipped. Then it makes one model call per episode, with the builder's task and the end of
its account of the build (only when the builder worked in the same repo as the reviewer; a session's
Agent-tool or spawned agents have no builder, since the session's own agent built), and the
redacted finding. It words the lesson as a question for the moment of building: "Before <doing X>, has
<risk Y> been considered?". `BORG_REVIEW_FIX=off` turns it off; `borg review-fix --apply` records past
findings by hand.

A finding learned as a lesson also reaches back, as [a later fix does](#decision--fix-links): the
lessons asked in the session before the finding get a `regretted` outcome, half a correction at most
(the finding alone 0.5, confirmed by a verifier 0.75). A lesson already marked corrected there is not
blamed twice. `BORG_REGRET_CREDIT=off` turns this part off.

Why on: in the value eval lessons from earlier findings raised the share of later bugs a plan avoided
by 6 points [−5.1, +17.9], and 89% of the lessons state their finding faithfully. The rule set before
the run (borg beats a plain agent with an interval excluding 0) was not met; the engineer turned it on
anyway, since a user who never configures would otherwise never get it. `borg web` counts the findings learned, so you can
watch it. Numbers in [Evidence and limits](evidence.md).

## Environment variables

All are optional.

**Paths and binaries**

| Variable | Default | Effect |
|---|---|---|
| `BORG_CONFIG` | `~/.config/borg/collectives.json` | The collectives config |
| `BORG_HOME` | `~/.local/share/borg` | Root of the stores |
| `BORG_STATE` | `/tmp/borg` | Per-session scratch state |
| `BORG_BIN` | `borg` on `PATH` | The binary the omp and opencode adapters run, for hooks and for `borg listen` (whose `--stream` waiter also retries a wake the harness refused) |
| `BORG_OPENCODE_LOG` | none | A file the opencode adapter logs what it did to, with every error it swallowed |
| `BORG_RELAY` | the collective's `relay` | Mesh relay for every collective: `default` (n0), `disabled`, or URL(s) |

**Learning and delivery**

| Variable | Default | Effect |
|---|---|---|
| `BORG_LEARN` | on | `off`: record no episodes or outcomes (for eval drones) |
| `BORG_LEARN_MODEL` | the harness's (Codex: none, see *Model runner*) | Model for `borg learn`, `assimilate`, `reach` and `facets` (see *Model runner*); `off`: no model call, episodes wait |
| `BORG_LEARN_TIMEOUT_S` | `300` | Time limit of one learning-side model call |
| `BORG_REGRESS` | `links` | [Decision → fix links](#decision--fix-links): `learn` also makes fix links regret episodes (team or private); `off`: no links |
| `BORG_REGRESS_MIN_AGE` | `2d` | Least age of a decision a fix may link to, by commit times: `2d`, `12h`, `90m`, or seconds |
| `BORG_REGRESS_TIMEOUT_S` | `10` | Time limit of each git call while linking; `0` makes none, so nothing is linked |
| `BORG_TRANSCRIPT_CAPTURE` | on | `off`: Borg no longer reads the harnesses' own transcripts for corrections and answers (at a turn end, at session end, for idle sessions); the hooks alone capture ([The harness's own transcript](harnesses.md#the-harnesss-own-transcript)). `borg assimilate` still reads them |
| `BORG_REVIEW_FIX` | on | [Review → fix findings](#review--fix-findings): `off`: the ended session's sub-agent review findings are not learned from |
| `BORG_REGRET_CREDIT` | on | `off`: a regret (a later fix or a review finding) no longer counts against the lessons asked before the decision ([Regret reaches back](#decision--fix-links)) |
| `BORG_TEST_CREDIT` | on | `off`: a session's own tests going green after a lesson was asked no longer credit it (`tested` outcomes; [Green tests credit the question](learning.md#outcomes-and-the-usefulness-belief)). For an eval's frozen arm. Also off with `BORG_LEARN=off` |
| `BORG_SHARE_OUTCOMES` | on | `off`: publish no outcomes for the team and read none (lesson usefulness and trust from this machine only); lines already synced stay |
| `BORG_VETO` | on | `off`: a prompt starting "borg no" is an ordinary prompt (the `borg no` command still works; [Saying no](commands.md#saying-no)) |
| `BORG_ASSIMILATE_JOBS` | `4` | Parallel model calls in `assimilate --from-dir` |
| `BORG_ASSIMILATE_SKIP` | none | Comma-separated words; matching episodes and documents are skipped |
| `BORG_EARLY` | on | `off`: no early delivery |
| `BORG_SYSTEM_GATE` | on | `off`: no system gate |
| `BORG_SYSTEMS` | `systems.json` next to `BORG_CONFIG` | Your system map |
| `BORG_CONSCIENCE` | the routed store's `lessons.jsonl` | Read lessons from another file |

**Model runner**

Borg's own model calls run headless through a CLI: the harness the drone runs in first (the hook
passes it on as `BORG_HARNESS`), then every other installed one of `claude`, `omp`, `opencode`, until
one answers. A model name with a runner prefix pins the runner: `claude:opus`, `omp:` (omp's default),
`omp:<model>`, `opencode:github-copilot/gpt-6.1-sol`, `codex:gpt-5`. A name without a prefix is a
Claude model (default `sonnet`); omp and opencode then use their own configured default model. codex
answers only when pinned with `codex:`, even for a Codex drone; its call ignores your codex config and
runs with hooks off (`-c features.hooks=false`). A Codex drone on a machine with no `claude`, `omp` or
`opencode` therefore needs `BORG_PREDICT_MODEL=codex:<model>` and `BORG_LEARN_MODEL=codex:<model>`:
without them its live and turn-end checks never judge and Borg learns nothing. `borg doctor` says so.
Each call runs without tools, plugins or extensions (opencode with every permission denied), and is
killed with everything it started when its time limit passes or borg itself is killed. opencode 1.x
runs with `--pure`; opencode 2.x runs with `--standalone` (a private server, never your background
service) and an empty config directory, with your providers' config passed on: the file your own
`OPENCODE_CONFIG` names, else your global `opencode.json`.
opencode stores every call as a session titled `borg (model call)` in the temp directory, not in your
project.

**Turn-end check**

| Variable | Default | Effect |
|---|---|---|
| `BORG_MATCH` | model | `bm25`: word matching only, no model call |
| `BORG_PREFILTER` | `8.0` | Threshold of the free gate; `off` always calls the model |
| `BORG_STOP_GATE` | word gate | `edits`: call the model only for turns that edited files or changed state |
| `BORG_STOP_POOL` | whole pool | `shortlist`: show the model only the word-matching shortlist |
| `BORG_STOP_POOL_TOKENS` | the learned policy's own choice (`12000` under the fixed rule) | Token budget of the lesson pool, for every check. Unset, the fixed rule keeps 12,000 and the learned policy chooses it. A pool the budget cut also shows the model up to two of the lessons it left out (the tail sample, [The pool](delivery.md#which-lessons-are-asked)) |
| `BORG_STOP_FORMAT` | compact | `classic`: the older prompt layout |
| `BORG_STOP_GRADED` | on | `off`: no grades; any applying lesson blocks |
| `BORG_SELECT` | the learned policy | The learned policy decides, the default since its pre-registered checks passed ([Evidence](evidence.md#lesson-selection)): it deviates from the fixed rule only where the outcomes show the deviation avoids more mistakes, and on live checks runs the randomised question-count test, rationed by the collective's `risk` ([Collectives](#collectives)), which withholds no lesson. `learned` names it. `fixed`: the fixed rule whatever else is set, the way back: with a grade 3 every 3 and 2 is asked, and the 1s up to 10 lessons; without one the 2s and 1s are handed over free, up to 10, never a lesson the session already had. `fixed` wins over `BORG_STOP_ASK_CAP` and `BORG_STOP_ATTENTION`, which win over the learned policy. Any other value is the default ([Which lessons are asked](delivery.md#which-lessons-are-asked)) |
| `BORG_STOP_ASK_CAP` | none | Evaluation only: a block asks only the first N grade-3 lessons. Wins over `BORG_STOP_ATTENTION` and the learned policy; `BORG_SELECT=fixed` wins over it |
| `BORG_STOP_ATTENTION` | off | Evaluation only. `on`: a blocking check asks the model's top `k` applying lessons and hands the rest over free. `k` is the smallest cut that, by conformal risk control over the last 200 labelled checks of the store whose lessons it reads, misses a needed lesson no more often than the fixed rule; ranks 0–10 until there are enough labels (at least ⌈1/α⌉ − 1, 999 while the fixed rule misses none). Labels come only from explored checks: every tenth blocking check asks every applying lesson, up to 20, so the cut never calibrates on what it chose not to ask. Its evaluation missed its bar ([Evidence](evidence.md#experiments)) |
| `BORG_MODE` | the collective's `mode`, else `live` | `live`: also check the work during the turn, in a background `borg check` after state-changing tool calls; the lessons it asks arrive with the next tool call (every harness; needs the model, so not with `BORG_MATCH=bm25`, `BORG_ONLY` or `BORG_CRITIC`, which `borg doctor` warns about). A question no tool call delivered blocks the stop only when its check graded a 3; else it arrives with the next prompt, as one from a check that finishes after the stop does. `hook`: at the turn end only. Overrides the config |
| `BORG_LIVE_DEBOUNCE_S` | `45` | Live mode: the least time between the starts of two checks of a session. One check runs at a time; a stop waits at most 15 s for it, or a quarter of `BORG_STOP_BUDGET_MS` when that is less |
| `BORG_PREDICT_MODEL` | the harness's (Codex: none, see *Model runner*) | Model for the turn-end check (see *Model runner*); `off`: no model call |
| `BORG_PREDICT_TIMEOUT_S` | `45` | Time limit of the turn-end call (and of a live check's call; a live check's lock goes stale after twice this plus 30 s). At the stop it is capped by the harness's budget (`BORG_STOP_BUDGET_MS`) |
| `BORG_STOP_BUDGET_MS` | the harness's stop budget: omp 28 s, opencode and Codex 90 s, Claude Code none ([Capabilities](harnesses.md#capabilities)) | The stop hook's budget; the omp and opencode adapters set it to the budget borg's first answer gave them. It bounds the expert drone (60 s without any budget) and, when there is one, sizes the stop's wait for a live check (at most a quarter of it, never over 15 s) and its model call (what the budget leaves when the call starts, less 3 s, never over `BORG_PREDICT_TIMEOUT_S`): on omp 7 s and at most 18 s |
| `BORG_VECTORS` | on | `off`: no vector recall |
| `BORG_VECTOR_MODEL` | `~/.local/share/borg/models/potion-multilingual-128M` | Model directory |
| `BORG_VECTOR_K` | `10` | Vector hits added to the shortlist |
| `BORG_VECTOR_URL` | Hugging Face | Mirror for `borg vectors --fetch` |
| `BORG_EXPERT_MODEL` | `sonnet` | Model of the optional expert drone |

**Queen and notifications**

| Variable | Default | Effect |
|---|---|---|
| `BORG_QUEEN_ASK` | on | `off`: the drone asks no queued question at a session's first prompt (also off with `BORG_LEARN=off`) |
| `BORG_NOTIFY` | on | `off`: no desktop notification (`notify-send`) for new questions |

**Updates and versions** ([Upgrade](../README.md#upgrade-and-uninstall), [Versions on the mesh](team-sync.md#how-a-sync-works))

| Variable | Default | Effect |
|---|---|---|
| `BORG_UPDATE_CHECK` | on | `off`: no release check. The prompt hook starts no background `borg update --check`, the drone hears of no newer release, `borg update` refuses (exit 1), and `borg doctor`, `borg status`, `borg top` and `borg web` say nothing about the release. The notice about a member on a newer mesh protocol stays |
| `BORG_RELEASES_URL` | `https://api.github.com/repos/debe/borg_ai/releases/latest` | The release feed the check reads (`tag_name`); any URL `curl` reads, `file://` included |
| `BORG_DOWNLOAD_URL` | `https://github.com/debe/borg_ai/releases/download` | Where `borg update` (as `install.sh`) downloads `v<X>/borg-v<X>-<target>.tar.gz` and its `.sha256` |
| `BORG_ANNOUNCE` | on | Read by `borg mesh serve` and `borg mesh sync` when they start. `off`: this node never sends the announcement of a newer mesh protocol to its collective |

**Experimental**, off by default. Most failed a pre-registered test and stay for further experiments.

| Variable | Effect when `on` |
|---|---|
| `BORG_QUEEN` | The turn end may queue questions for the engineer (failed its test) |
| `BORG_CONCEPTS` | Boost lessons that share engineering concepts with the work; `BORG_CONCEPTS_BETA` (0.5) sets the strength |
| `BORG_FACETS` | `learn` and `assimilate` write facets (failed its test) |

Evaluation only: `BORG_BEFORE`, `BORG_CRITIC`, `BORG_ONLY`, `BORG_EXPERT_CEILING`, `BORG_VECTORS_TRACE`.
