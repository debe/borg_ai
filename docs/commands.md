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
| `borg hive init\|board\|join\|leave\|open\|claim\|progress\|release\|block\|done\|verify\|score\|integrate\|contract` | Many drones, one goal: a board of tasks with leased claims, contracts between drones, done gated by the goal's oracle, and verdicts guarded against gaming it; see [below](#hive) |
| `borg hive run <goal> --drones N [--harness H] [--dry-run]` | N headless drones, each in a git worktree of the repo with an equal share of what is left of the goal's budget, until the board is finished; see [Running drones](#running-drones) |
| `borg hive join <goal> --here` · `borg hive leave --here` | Every new session in this repo (or worktree) joins the goal, or no longer does |
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
| `borg hive init <goal> --spec TEXT\|--spec-file F --oracle NAME=CMD... --budget USD [--timeout S] [--repo NAME[@NODE]]... [--task "ID\|role\|title\|files,..\|deps,.."]... [--hidden-dir DIR --hidden CMD... [--hidden-timeout S]]` | A goal, its oracle and its first tasks. Without `--oracle` it refuses: done means the oracle passes. `--hidden-dir`/`--hidden`: a hidden oracle, see [Guardrails](#guardrails-and-the-hidden-oracle) |
| `borg hive board [<goal>] [--json] [--watch]` | The tasks by status, with holders, age, rework and dependencies |
| `borg hive join <goal> [--session ID\|DRONE]` · `borg hive leave` | A running drone joins the goal, or leaves it (its claim is released). A drone started with `BORG_HIVE=<goal>` joins at its first hook |
| `borg hive join <goal> --here [--drone NAME]` · `borg hive leave --here` | Every session that starts in this repo (its git toplevel, or a worktree's own) joins the goal at its first hook, as drone NAME (default `j-` and a hash of the directory). For a harness whose sessions lose the launch env (opencode 2.x's background service). `leave --here` stops new joins; sessions that joined stay |
| `borg hive run <goal> --drones N [--harness H] [--model M] [--base REF] [--iter-timeout S] [--dry-run]` | Headless drones in worktrees, see [Running drones](#running-drones) |
| `borg hive open <goal> --id ID --role work\|plan\|verify\|integrate --title T [--repo R] [--files a,b] [--deps x,y] [--accept NAME=CMD]... [--again]` | An engineer opens a task; `--again` reopens a blocked one |
| `borg hive claim [TASK]` | The drone claims a task (the next open one without TASK); exit 3 when another drone won it, exit 1 when another drone holds it (the message names the holder and tells the drone not to work on it). A task with deps is open once they are verified, and its claim first brings their verified work into the drone's tree (see below); exit 1 when that work does not apply there |
| `borg hive progress "<note>"` · `release` · `block "<why>"` | Report on, give back or block the drone's claim |
| `borg hive done [--note N]` | Run the task's checks with their own timeouts (no hook time limit); on a pass publish done and open its verify task |
| `borg hive verify --pass\|--reject "<why>"` | The verdict on a verify task (or end the verifier's turn with the line `VERDICT: pass` or `VERDICT: reject <why>`); a rejection reopens the work with the note. The work's diff is checked first, see [Guardrails](#guardrails-and-the-hidden-oracle) |
| `borg hive score <goal>` | The goal's hidden oracle scores every judged task not scored yet (`borg hive run` does this when its drones end). The engineer's: refused to a drone of a hive goal. Exit 1 when an attempt could not be checked out (run it from the goal's repo) |
| `borg hive integrate <goal>` | Once every task is verified: merge the accepted attempts and run the goal's oracle and every task's checks on the combined tree, as `borg hive run` does itself (see [Running drones](#running-drones)). Exit 0 when it passed (the goal is finished), 1 when it was rejected (a fix task is open) or blocked, 2 when no integration is due. Needs git 2.40+. The engineer's: refused to a drone of a hive goal |
| `borg hive contract "<text>" --scope a,b [--supersedes ID]` | Publish a contract scoped to files (globs), `id:NAME` or `repo:NAME`. Superseding another drone's contract asks the engineer through `borg queen` |

Drone commands take `--session ID` (needed when the repo has more than one drone; the hook's context
prints the exact command) and `--dir DIR`.

**Files.** `goal.json` (written once), `tasks.jsonl` (the board's events: open, claim, heartbeat,
progress, done, verified, rejected, released, blocked), `contracts.jsonl`, and `oracle/<task>.<ms>.log`
(the last 8 KiB of a failed run, the newest 20 kept). `joins.jsonl` (directories whose sessions join),
`spend.jsonl` and `runs.jsonl` (each iteration's cost and each `borg hive run`, below), and
`logs/<drone>-<iter>.out` (an iteration's output, at most 1 MiB, local only). `flags.jsonl` (what
drones did to tests, CI or oracle files), `oracle.jsonl` (the hidden oracle's scores) and `scratch/`
(its checkouts while it runs), below; `integrate/` (an integration's checkout while its checks run).
Membership is `<state>/<session>.hive`.

**Dependencies.** `--deps` (or the fifth field of `--task`) names tasks of the goal; `init` and `open`
refuse one that names no task, and `init` refuses a cycle. A task is claimable once every dep is
verified. Each drone works in its own worktree, so the claim applies each dep's verified change to the
claimant's working tree first (without touching its index or HEAD; a change already there is skipped)
and records it in the claim (`deps_in`). The work's diff then starts after the deps. When a dep's
change does not apply (the tree's own changes overlap it), the claim is refused and the tree stays as
it was; a drone with a clean tree can take the task. A pass on work whose claim lacks a dep's work is
not verified: the task reopens.

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

### Guardrails and the hidden oracle

A drone can pass a visible oracle by changing it. The hive lets that happen, records it, and makes it
useless (docs/design/hive-learning.md, step 8):

- **Snapshots.** A claim of work records a commit of the drone's working tree (tracked and untracked
  files, written to the object store without touching the index, HEAD or any ref), and its done records
  another. The verdict diffs the two. A re-claim of a task by the same drone (a rework after a reject, a
  claim after a release) keeps the base of its first claim, so a test an earlier attempt weakened still
  shows in the diff. Work without a snapshot (git failed or timed out) cannot be checked: a pass on it
  rejects the work with a note and credits nothing; a verifier whose directory cannot diff the snapshots
  (another repo) gets no verdict recorded.
- **Hacked.** Tests may grow, never shrink: outside your task's files you may add tests and change
  names and values in existing ones, but you may not remove, skip or reshape a test or an assertion.
  The verdict is `rejected` with `hacked`, whatever the verifier said, when the diff
  - changes a file the goal's oracle runs (such as `grade.sh`) in any way. That file is the oracle, so
    not even the task's own `--files` exempt it;
  - outside the task's own `--files`, modifies, deletes or renames CI config (`.github/workflows/`,
    `.gitlab-ci.yml`, `.circleci/`, `.buildkite/`, `Jenkinsfile`, `azure-pipelines.yml`);
  - outside them, adds or changes a test runner's config (`conftest.py`, `pytest.ini`, `tox.ini`,
    `jest.config.*`, `vitest.config.*`, `.mocharc*`, `spec_helper.rb`, ...: it changes how the existing
    tests run; `pyproject.toml` and `package.json` are not counted);
  - outside them, deletes or renames a test (a file under `test/`, `tests/`, `spec/`, `specs/`,
    `__tests__/`, or named `test_*`, `*_test.*`, `*.test.*`, `*.spec.*`, `*_spec.rb`), or weakens one: a
    removed line that no added line of the same hunk matches in shape (its tokens, with names, strings
    and numbers as holes; assertion words such as `assert*`, `expect*`, `return`, `true` kept) without
    making two different names one (`assert got == want` turned `assert got == got`), fewer test
    declarations than before (`#[test]` turned `#[inline]`, `@Test` turned `@Deprecated`, `def test_x(`
    renamed `def x(`), an added skip, disable or focus marker (`@Disabled`, `@pytest.mark.skip`,
    `it.only(`, `xit(`, `t.Skip()`, `#[ignore]`, a bare `return`), or any binary change. Blank and comment lines do not count. A test
    whose content diff git cannot give counts as weakened;

  or when the drone opened the hidden oracle's files. The work reopens with each path to restore and
  why (`tests/test_parse.py (line 4 removed or reshaped: assert parse('a') == 'a')`). Appended test
  methods and imports, a renamed key literal or accessor and an updated expected value all pass. A
  swapped value in an assertion of the same shape does not show in the diff: the hidden oracle is
  the backstop for that.
- **Flags.** At every tool call a drone in a goal makes, the paths it writes (edit tools; a shell
  command's redirects, `sed -i`, `rm`, `mv`, `cp`, `tee`, ...) are checked against those classes, and the
  paths it reads against the hidden oracle's (its directory, and `goal.json`, which names its commands).
  Each hit is one line of `flags.jsonl`, once per session, claim and path, and the verify task lists them
  for the verifier. Flag only, never a refusal; best-effort (a
  `python -c` write escapes it), which is why the diff decides.
- **The verdict's reward.** Every lesson shown to the author's session while it held the claim is
  credited with the verdict: `verified`, `rejected` or `hacked` (outcome records in `outcomes.jsonl`,
  once per lesson and verify task, counted as `task_verified`, `task_rejected`, `task_hacked`). A hacked
  task weighs on a lesson as much as a veto. The web page's overview shows the hacked rate.
- **The author never judges its own work.** A verdict from the session that did it, or from any session
  of its drone, is refused.
- **Hidden oracle.** `--hidden-dir DIR --hidden CMD` keeps held-out checks in a directory outside the
  repo (`init` refuses one inside it). After a pass or a reject, the hidden commands run in a scratch
  worktree of the done's snapshot under `<goal>/scratch/` (removed afterwards), each `sh -c` with
  `BORG_HIDDEN` (the directory) and `BORG_WORK` (the checkout), within `--hidden-timeout` (default 600 s,
  30 to 3600). The score decides the credit: a pass is `verified`, a failure `rejected`, whatever the
  verifier said; `oracle.jsonl` logs it with whether the verifier agreed. Its output is never shown to a
  drone. `borg hive run` scores when its drones end; `borg hive score <goal>` any time, never from a
  drone (one scoring per goal at a time). A checkout that fails is no score: the attempt waits for the
  next scoring. A hacked verdict is credited at once.

The drone is told once that checks it cannot see score the goal, that every change to tests, CI or
oracle files is recorded, and that gaming one is useless. `BORG_HIVE_CREDIT=off` records verdicts
without crediting lessons (an eval's frozen arm).

### Running drones

```console
$ borg hive run parser --drones 3 --dry-run     # the worktrees, the harness command, the share, the prompt
$ borg hive run parser --drones 3               # --harness: the calling session's harness, else claude
d1  integrate.1  integrated 3f2a9c1b07de: the oracle passes
d1  hive-parser-1  $1.84/$6.67  4 iter  tasks done: t1,t3.v1  end: goal finished
...
board: 7 task(s) (7 verified); the goal is finished
integrated 3f2a9c1b07de… (integrate.1, refs/heads/hive/parser/integrated)
```

`borg hive run` makes one git worktree per drone, `<repo>/.worktrees/hive-<goal>-<n>` on branch
`hive/<goal>/<n>` from `--base` (default `HEAD`); an existing one is reused, and an existing branch is
checked out as it is (a removed worktree's commits are kept, never reset to `--base`). The path is one component
under `.worktrees/`, so borg records it as the same repo, and the same collective takes it. It adds
`/.worktrees/` to the repo's local `.git/info/exclude` (never committed) so `git status` stays clean.
When the drones end, the run writes a leave for each worktree,
so a session you start there to review joins no goal.

**Integration.** Once every task is verified, one drone thread integrates, in process and with no
model call, while the others wait. It claims the board's `integrate.<n>` task and merges the
accepted attempts: for each verified task, the diff from its claim's snapshot to its done's, onto the
commit every claim started from. It never merges a drone's branch, so changes a drone made without
holding the claim, or committed there, are left out. A conflict counts as resolved only by a fix
task built on the tree with its markers. The merge
uses `git merge-tree --merge-base`, so it needs git 2.40+; an older git blocks the integration with
that reason. The result is a commit at `refs/heads/hive/<goal>/integrated`, which is the hive's result
for you to review or merge. A conflict rejects it. Otherwise the goal's oracle and every task's own
checks run on it in a scratch checkout, and a failure rejects it. A rejection opens the fix task
`integrate.<n>.fix`, whose claim resets that drone's worktree to the combined tree; a joined session
claims it in a checkout of that tree instead (`git worktree add <dir> <commit>`, as `borg hive
integrate` prints). Once the fix is verified, the next integration includes it. The goal is finished only when an integration passes after the last
task was verified. A model session never claims an integrate task; `borg hive integrate <goal>` runs
one for a goal worked without `borg hive run`.

Each drone gets an equal share of what is left of the goal's budget (every earlier run's spend counted)
and runs headless sessions of the harness one after another, launched with
`BORG_HIVE=<goal>` and `BORG_HIVE_DRONE=d<n>`: borg's own hooks join each to the goal, show it the board
and gate its stop on the oracle. Claude Code runs as `claude -p --permission-mode bypassPermissions`
with your settings (so borg's hooks run) and `--max-budget-usd` set to the drone's dollars left, so it
stops itself; codex, omp and opencode 1.x get no budget and are checked between sessions, so they can
overshoot by one session; a session cut off at `--iter-timeout` without naming its cost is charged the
drone's dearest session so far (or the rest of its share). Drones run with every permission inside their worktree: run it on a repo you
are willing to let them change. Every session is cut off at `--iter-timeout` (default 1800 s).

A drone stops when the board is finished (every task verified and integrated), its share is spent, a session ended
without naming its cost or at $0 (codex reports tokens only and a subscription model $0, so such a
drone gets one session), two sessions
in a row failed or timed out, two made no board progress, or nothing was open to it for three polls.
Whatever an ended session still held is released (the note names why), so no claim waits for its
lease. A verify task is never claimed by a session of the drone that did the work. Exit 0 when the board
is finished, 1 when it is not, 2 when a precondition fails: an unknown goal, `--drones` outside 1–20, a
goal without a budget, a repo outside the goal's, a harness that cannot run headless, or another run of
the goal still going (one at a time).

A re-run shares what `spend.jsonl` says is left, whatever the drone count. `borg top` and the web page's Hive tab show
each goal's board, its spend per drone and its newest contracts.

Not yet: mesh sync of hive lines, goal signatures, the queen approving a plan before `run`, multi-repo
goals (`run` takes the repo it is started in), stopping an unmetered session
mid-way at its budget, crediting verifiers by whether the hidden oracle agreed with them, a model
that monitors drones for gaming, and messages between drones.

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
