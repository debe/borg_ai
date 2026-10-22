# Commands

[← README](../README.md)

## Overview

| Command | Shows or does |
|---|---|
| `borg status` | Each collective's delivery mode and store (lessons, episodes, outcomes, turn-end and live checks), what is wired and whether the `borg` skill is current |
| `borg doctor` | What is broken: config (an unknown `mode` included), hooks, adapters, the model CLI (claude, omp or opencode, or a pinned `codex:<model>`), `borg` on `PATH`, a missing or stale `borg` skill, the vector model and each store's index, and which of borg's hooks codex would skip as untrusted (asked of codex itself, `codex app-server`) |
| `borg collective [DIR]` | Which collective a directory belongs to and whether it may sync |
| `borg setup [--yes] [--no-vectors] [--harness LIST] [--private DIR...] [--team NAME --paths DIR... [--relay URL\|disabled] \| --join TICKET [--paths DIR...]] [--home DIR]` | Guided setup: collectives (personal, team or company, off paths), the team's mesh, the vector model, wiring, an optional import, then `borg doctor`. Shows one plan and writes nothing until you confirm; `--yes` takes every default without asking. Flags answer their question. An existing `collectives.json` is only added to. A second run changes nothing ([Quick start](../README.md#quick-start)) |
| `borg install\|uninstall <claude\|omp\|opencode\|codex\|all> [--apply]` | Wire or unwire the hooks or adapter and the `borg` skill; a dry run without `--apply` ([details](configuration.md#what-borg-install-writes)) |
| `borg assimilate ...` | Import past Claude Code, omp and opencode sessions (opencode 2: known projects only, not the sessions it migrated from 1.x), or team documents ([details](configuration.md#import-what-you-already-have)) |
| `borg reach [--plan FILE] [--apply]` | Sort existing lessons into team, repo-only and personal ([details](configuration.md#who-hears-a-lesson)) |
| `borg learn [--session ID]` | Turn waiting episodes into lessons now (normally at session end); also counts each learned regret against the lessons asked before its decision (`regretted`) |
| `borg memory <lessons.jsonl>` | Each lesson's activation, usefulness (with its `regretted` share of later fixes), weight, and long-term or dormant state, from this machine's outcomes and the team's synced ones |
| `borg vectors [--fetch\|--rebuild] [--store FILE]` | Vector model and index; on by default once the model is fetched ([details](configuration.md#vectors)) |
| `borg queen ...` | Open decisions for the engineer; see [below](#answering-the-queen) |
| `borg web [--port PORT] [--dir DIR]` | A live page of the collective's knowledge and statistics on 127.0.0.1 ([details](statistics.md)) |
| `borg top [--watch] [--json]` | The running drones of each collective, local and remote |
| `borg steer <drone\|session\|all> "<text>"` | Send a signed instruction to running drones; `--pause`, `--resume` |
| `borg no [--session ID\|DRONE] [--dir DIR] [--json] <reason>` | Say no to a drone's work: the lessons it was shown lose credit, the reason becomes a lesson; see [below](#saying-no) |
| `borg why <path>[:<line>[-<line>]] \| <commit> [--full] [--json]` | The reasoning behind code: blame, Borg notes, linked lessons, and both ends of a decision → fix link: "→ fixed in Y (task)" on the decision, "← decided in X (task) → fixed here" on the fix (three each, all with `--full`; `--json` adds `fixed_in` and `fixes`) |
| `borg regress [--session ID] [--replay A..B] [--any-origin] [--explain] [--json] [--dir DIR]` | The decision → fix links Borg would make, as a dry run that writes nothing: for the latest session in this repo (or `--session`), or every commit of a range (`--replay`, at most 500). `--explain` lists each candidate left out and why (`format`, `move`, `too-large`, `no-reasoning`, `same-session`, `too-young`, `duplicate`, `cap`, ...); `--any-origin` also takes origins without Borg reasoning, to try the detector on a repo with no notes ([details](configuration.md#decision--fix-links)) |
| `borg review-fix [--home DIR] [--since YYYY-MM-DD] [--json] [--apply [--only ID,…]]` | The bugs a sub-agent review found and a fix sub-agent fixed before any commit (Claude Code workflows, omp tasks, opencode subagents), per collective, with a chance-match control; 0 tokens. `--apply` records the strong ones as episodes and learns them ([details](configuration.md#review--fix-findings)) |
| `borg notes attach [<commit>]` | Write the redacted reasoning note for a session's commits, then link them to the earlier decisions they fixed (normally at session end) |
| `borg notes fetch [<commit>...]` | Get reasoning notes and decision → fix links from team members over the mesh: for the named commits, or the last 200 on HEAD; a link comes with either of its commits |
| `borg trust [--dir DIR]` | Trust per engineer and area, from what happened after their lessons were asked, on this machine and the team's |
| `borg hive init\|board\|join\|leave\|open\|claim\|progress\|release\|block\|done\|verify\|contract` | Many drones, one goal: a board of tasks with leased claims, contracts between drones, and done gated by the goal's oracle; see [below](#hive) |
| `borg mesh <command>` | Team sync: `id`, `init`, `invite`, `invites`, `join` (`--paths DIR...` names your repos), `revoke`, `members`, `peer`, `serve`, `sync` ([details](team-sync.md)) |

**Shell completion:** `borg completions <bash|zsh|fish>` prints a completion script.

```sh
borg completions bash > ~/.local/share/bash-completion/completions/borg
borg completions zsh > "${fpath[1]}/_borg"
borg completions fish > ~/.config/fish/completions/borg.fish
```

For debugging and evaluation: `borg recall`, `borg prefilter`, `borg resolve`, `borg expert`,
`borg facets`. Their usage is in the header of [`crates/borg/src/main.rs`](../crates/borg/src/main.rs).

**Internal:** `borg check --session ID [--transcript FILE] --cwd DIR` is live mode's check of the work
so far ([Delivery modes](../README.md#delivery-modes)). Every harness's post-tool event (Claude Code and
omp PostToolUse, opencode `tool.execute.after`) starts it detached after a state-changing tool call, with
the hook's environment. Without a transcript (omp, opencode) the work so far is the turn's tool calls
from `<state>/<session>.turn`. It takes the session's lock
(`<state>/<session>.live.lock`), runs one graded model call on the turn so far, and leaves its verdict
in the session's `.queued` file for the next prompt or tool call. It is not meant to be run by hand and
is not listed in `borg help`.

## Hive

A local spike, before the pilot (docs/design/collective.md): many drones work on one goal. The goal
has an oracle (shell checks) that decides when a task is done. Drones claim tasks from a shared board,
publish contracts for each other, and finish a task only when its checks pass. Everything is in files
under `<store>/hive/<goal>/`; nothing syncs over the mesh yet.

```console
$ borg hive init parser --spec "Ship the parser" --oracle "unit=cargo test -q" --budget 20 \
    --task "t1|work|Add the parser|src/parse/*.rs" --task "t2|work|Use it|src/main.rs|t1"
$ BORG_HIVE=parser claude          # or, in a running session: borg hive join parser --session <drone>
$ borg hive board
```

| Command | Effect |
|---|---|
| `borg hive init <goal> --spec TEXT\|--spec-file F --oracle NAME=CMD... --budget USD [--timeout S] [--repo NAME[@NODE]]... [--task "ID\|role\|title\|files,..\|deps,.."]...` | A goal, its oracle and its first tasks. Without `--oracle` it refuses: done means the oracle passes |
| `borg hive board [<goal>] [--json] [--watch]` | The tasks by status, with holders, age, rework and dependencies |
| `borg hive join <goal> [--session ID\|DRONE]` · `borg hive leave` | A running drone joins the goal, or leaves it (its claim is released). A drone started with `BORG_HIVE=<goal>` joins at its first hook |
| `borg hive open <goal> --id ID --role work\|plan\|verify\|integrate --title T [--repo R] [--files a,b] [--deps x,y] [--accept NAME=CMD]... [--again]` | An engineer opens a task; `--again` reopens a blocked one |
| `borg hive claim [TASK]` | The drone claims a task (the next open one without TASK); exit 3 when another drone won it |
| `borg hive progress "<note>"` · `release` · `block "<why>"` | Report on, give back or block the drone's claim |
| `borg hive done [--note N]` | Run the task's checks with their own timeouts (no hook time limit); on a pass publish done and open its verify task |
| `borg hive verify --pass\|--reject "<why>"` | The verdict on a verify task; a rejection reopens the work with the note |
| `borg hive contract "<text>" --scope a,b [--supersedes ID]` | Publish a contract scoped to files (globs), `id:NAME` or `repo:NAME`. Superseding another drone's contract asks the engineer through `borg queen` |

Drone commands take `--session ID` (needed when the repo has more than one drone; the hook's context
prints the exact command) and `--dir DIR`.

**Files.** `goal.json` (written once), `tasks.jsonl` (the board's events: open, claim, heartbeat,
progress, done, verified, rejected, released, blocked), `contracts.jsonl`, and `oracle/<task>.<ms>.log`
(the last 8 KiB of a failed run, the newest 20 kept). Membership is `<state>/<session>.hive`.

**Claims are leases.** A claim lasts while its drone's hooks renew it (every 10 minutes) and expires
after 30 minutes without one. When two drones claim the same open task, the claim with the lowest
(ms, blake3 key) wins, on every machine alike; the loser is told at its next prompt and moves on. A drone
that works 30 minutes without any hook event (one long tool call) loses its claim too.

**In the harness.** At a prompt the drone sees its claim (or the next open task and the command to claim
it), the board's changes since its last prompt and the contracts in its claim's scope, each contract
once. After an edit it sees contracts the edit lands in, and files outside its claim's scope (advice,
never a refusal). At the stop the task's checks run: while they fail the stop blocks, at most 3 times per
claim, then the task is published blocked. When they pass, done is published, a verify task opens (its
author may not claim it), and the drone is handed the next open task. A suite slower than the stop hook
(Claude Code 90 s, omp 30 s) needs `borg hive done`. A harness without prompt context gets this text at
the stop. A paused drone is never blocked by its oracle.

Nothing changes for a session that never joined a goal.

Not yet: mesh sync of hive lines, goal signatures, the queen approving a drone-written plan (tasks a
drone opens are claimable at once), budget enforcement (the budget is recorded and shown), and messages
between drones.

## Saying no

A veto is the engineer's "no" to what a drone is doing, with the reason. It is the strongest negative
signal Borg learns from, and it costs 0 tokens to say.

```console
$ borg no never look merges up in the commit log, use the review API
vetoed One of Two (3f9c2a1b): 2 shown lesson(s) lose credit (L17, L42); the reason becomes a lesson when borg learn runs
to stop it now: borg steer 3f9c2a1b --pause
```

**Which drone.** Without `--session`, the local drone working in the current repo (or `--dir`):

- exactly one that has not ended: that one;
- none still running: the most recently seen one (a veto right after its session ended);
- several running: an error that lists them; name one with `--session` (a designation as `borg top`
  shows it, its number, a session id or a prefix of at least 4 characters).

A drone on another member's machine is never vetoed from here. Presence keeps only a repo's name, so two
checkouts of repos with the same name count as one repo; use `--session` there.

**Inside the session.** A prompt that starts with `borg no` is the same veto for that session:

```text
borg no: don't touch the public schema, add a v2 endpoint instead
```

The drone is told to stop the vetoed approach and not argue it back; it may ask one clarifying
question. The prompt never becomes the session's task and is never also counted as a correction.
`BORG_VETO=off` turns this off; the command keeps working.

**What it does:**

1. **Credit.** Every lesson shown in the session so far (asked at a stop or live, handed over early or
   queued) gets a `vetoed` outcome. The usefulness belief weighs a veto three times, above a
   correction (two), so lessons shown before vetoed work sink fast. Trust counts it as a correction.
   Each lesson is credited once per session; a second veto in the same session counts again but
   credits nothing new.
2. **A lesson.** The reason is recorded as a veto episode. `borg learn` turns it into a lesson, or
   merges it into an existing one, at the end of any session, so a veto of an ended session does not
   wait for that session. A veto merged into a lesson that an earlier veto made counts as
   `veto_repeat`: the same "no" said twice.
3. **A proposed constraint.** When the reason names an invariant of the system ("merge lookups go
   through the review API, never the commit log"), `borg learn` queues it in `borg queen` as a
   question: keep it a lesson only (the default) or ratify it as a constraint. Only an engineer's
   answer ratifies it, and a personal lesson never proposes one. A ratified constraint is appended to
   `<store>/constraints.jsonl`. Nothing enforces constraints yet, and the mesh does not sync that file.

With `BORG_LEARN=off` the command records nothing and says so. The page (`borg web`) shows vetoes and
the share of them that repeated an earlier one; the goal is zero.

## Answering the queen

The queen's queue holds decisions that no lesson answers: open decisions found in team documents
(`borg assimilate --from-dir`) and, with `BORG_QUEEN=on`, questions raised at a turn end. A desktop
notification announces new ones, and similar questions are queued once.

**In the terminal:**

```console
$ borg queen
handbook (1)
  1  Should spikes be capped at 3 story points, or estimated like any other story?
     a) cap spikes at 3 points (default) · b) estimate spikes like any other story
answer: borg queen ask · borg queen answer <n> <text|a|b> · borg queen show <n>
$ borg queen answer 1 b
```

| Command | Effect |
|---|---|
| `borg queen` | Open questions, numbered and grouped by repo |
| `borg queen ask` | Answer them one by one |
| `borg queen answer <n\|id> <text\|a\|b\|1\|2>` | Answer one; the answer becomes a lesson (a reach question moves its lesson instead; a proposed constraint is ratified with `b`, see [Saying no](#saying-no); a drone's change to another drone's hive contract is let through with `b`, see [Hive](#hive)). A letter or number picks an option; one that names no option is refused |
| `borg queen show <n\|id>` | One question with its evidence |
| `borg queen skip <n\|id>` | Ask again in 3 days |
| `borg queen dismiss <n\|id> [reason]` | Drop it without a lesson |
| `borg queen --json` · `borg queen next --json [--repo NAME]` | All open questions, or the one to ask now, for drones and scripts |
| `borg queen dedupe [--apply]` | Dismiss later near-duplicates (dry run first) |
| `borg queen notify` | One desktop summary of the open questions |

**In the harness.** At the first prompt of a session in a repo with an open question, Borg hands that
question to the drone. Claude Code asks it in its question dialog; omp and opencode ask it at the start
of the reply. Pick an option, answer in your own words or skip. The drone records the answer with
`borg queen answer`, then does what you asked.

- One question per session, and only one of the session's own repo.
- A question belongs to its git repo, whichever subdirectory or worktree raised it.
- A skipped question waits 3 days; an unanswered one returns after 3 more sessions.
- `BORG_QUEEN_ASK=off` or `BORG_LEARN=off` turns this off.
