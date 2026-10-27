# Commands

[← README](../README.md)

## Overview

| Command | Shows or does |
|---|---|
| `borg status` | Each collective's delivery mode and store (lessons, episodes, outcomes, turn-end and live checks), what is wired and whether the `borg` skill is current |
| `borg doctor` | What is broken: config (an unknown `mode` included), hooks, adapters, the model CLI (claude, omp or opencode, or a pinned `codex:<model>`), `borg` on `PATH`, a missing or stale `borg` skill, the vector model and each store's index, which of borg's hooks codex would skip as untrusted (asked of codex itself, `codex app-server`), and each mesh member on another protocol version, with who has to upgrade |
| `borg collective [DIR]` | Which collective a directory belongs to and whether it may sync |
| `borg setup [--yes] [--no-vectors] [--harness LIST] [--private DIR...] [--team NAME --paths DIR... [--relay URL\|disabled] \| --join TICKET [--paths DIR...]] [--home DIR]` | Guided setup: collectives (personal, team or company, off paths), the team's mesh, the vector model, wiring, an optional import, then `borg doctor`. Shows one plan and writes nothing until you confirm; `--yes` takes every default without asking. Flags answer their question. An existing `collectives.json` is only added to. A second run changes nothing ([Quick start](../README.md#quick-start)) |
| `borg install\|uninstall <claude\|omp\|opencode\|codex\|all> [--apply]` | Wire or unwire the hooks or adapter and the `borg` skill; a dry run without `--apply` ([details](configuration.md#what-borg-install-writes)) |
| `borg assimilate ...` | Import past Claude Code, omp and opencode sessions (opencode 2: known projects only, not the sessions it migrated from 1.x), or team documents ([details](configuration.md#import-what-you-already-have)) |
| `borg reach [--plan FILE] [--apply]` | Sort existing lessons into team, repo-only and personal ([details](configuration.md#who-hears-a-lesson)) |
| `borg learn [--session ID]` | Turn waiting episodes into lessons now (normally at session end); also counts each learned regret against the lessons asked before its decision (`regretted`), and first turns each settled hive group opened on this node into a reflection episode, once (see [Groups](#groups-of-attempts)) |
| `borg learn --group <goal>/<task>` | Reflect on one hive group now: compare its winning and losing attempts and learn what the winners did, then learn like `borg learn`. Prints why nothing was written for a group of one, one not settled yet, or one whose attempts all won or all lost |
| `borg memory <lessons.jsonl>` | Each lesson's activation, usefulness (with its `regretted` share of later fixes), weight, and long-term or dormant state, from this machine's outcomes and the team's synced ones |
| `borg vectors [--fetch\|--rebuild] [--store FILE]` | Vector model and index; on by default once the model is fetched ([details](configuration.md#vectors)) |
| `borg queen ...` | Open decisions for the engineer; see [below](#answering-the-queen) |
| `borg web [--port PORT] [--dir DIR]` | A live page of the collective's knowledge and statistics on 127.0.0.1, with a Learning tab that shows week by week whether learning works ([details](statistics.md)) and a Hive tab that shows each goal's tasks over time, its nodes and its interactive drones ([Watching the hive](#interactive-drones)) |
| `borg top [--once] [--json]` | A live screen, like Unix `top`: the drones of each collective (local and remote) with their state, task and what each is doing, each hive goal as progress bars with its tasks, plan and nodes, and the newest board events. Redraws every second, fits the window, quits on `q`, Esc or Ctrl-C (`NO_COLOR` turns colour off). Piped, or with `--once`, it prints the plain text once (`--watch` reprints it every 2 s) |
| `borg steer <drone\|session\|all> "<text>"` | Send a signed instruction to running drones; `--pause`, `--resume` |
| `borg no [--session ID\|DRONE] [--dir DIR] [--json] <reason>` | Say no to a drone's work: the lessons it was shown lose credit, the reason becomes a lesson; see [below](#saying-no) |
| `borg why <path>[:<line>[-<line>]] \| <commit> [--full] [--json]` | The reasoning behind code: blame, Borg notes, linked lessons, and both ends of a decision → fix link: "→ fixed in Y (task)" on the decision, "← decided in X (task) → fixed here" on the fix (three each, all with `--full`; `--json` adds `fixed_in` and `fixes`) |
| `borg regress [--session ID] [--replay A..B] [--any-origin] [--explain] [--json] [--dir DIR]` | The decision → fix links Borg would make, as a dry run that writes nothing: for the latest session in this repo (or `--session`), or every commit of a range (`--replay`, at most 500). `--explain` lists each candidate left out and why (`format`, `move`, `too-large`, `no-reasoning`, `same-session`, `too-young`, `duplicate`, `cap`, ...); `--any-origin` also takes origins without Borg reasoning, to try the detector on a repo with no notes ([details](configuration.md#decision--fix-links)) |
| `borg review-fix [--home DIR] [--since YYYY-MM-DD] [--json] [--apply [--only ID,…]]` | The bugs a sub-agent review found and a fix sub-agent fixed before any commit (Claude Code workflows and Agent-tool sub-agents, omp tasks, opencode subagents, Codex spawned agents), per collective, with a chance-match control; 0 tokens. `--apply` records the strong ones as episodes and learns them ([details](configuration.md#review--fix-findings)) |
| `borg notes attach [<commit>]` | Write the redacted reasoning note for a session's commits, then link them to the earlier decisions they fixed (normally at session end) |
| `borg notes fetch [<commit>...]` | Get reasoning notes and decision → fix links from team members over the mesh: for the named commits, or the last 200 on HEAD; a link comes with either of its commits |
| `borg trust [--dir DIR]` | Trust per engineer and area, from what happened after their lessons were asked, on this machine and the team's |
| `borg ope [--dir DIR \| --file FILE] [--json]` | Off-policy estimates of the belief's hand-set weights (veto, hive verdicts, regret, green tests) from logged asks with their propensity; reports and never changes them |
| `borg hive init\|ratify\|board\|join\|wait\|leave\|open\|claim\|progress\|release\|block\|done\|verify\|score\|integrate\|approve\|contract\|ask\|tell` | Many drones, one goal: a board of tasks with leased claims, contracts and messages between drones, done gated by the goal's oracle, and verdicts guarded against gaming it; see [below](#hive) |
| `borg hive run <goal> --drones N [--harness H] [--dry-run] [--yes]` | N headless drones, each in a git worktree of the repo with an equal share of what is left of the goal's budget, until the board is finished, once you approved the goal's plan (`--yes`: approved on the command line); see [Running drones](#running-drones) |
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
so far ([Delivery modes](delivery.md#delivery-modes)). Every harness's post-tool event (Claude Code and
omp PostToolUse, opencode `tool.execute.after`) starts it detached after a state-changing tool call, with
the hook's environment. Without a transcript (omp, opencode) the work so far is the turn's tool calls
from `<state>/<session>.turn`. It takes the session's lock
(`<state>/<session>.live.lock`), runs one graded model call on the turn so far, and leaves its verdict
in the session's `.queued` file for the next prompt or tool call. It is not meant to be run by hand and
is not listed in `borg help`.

## Hive

A local spike, before the pilot (docs/design/hive.md): many drones work on one goal. The goal
has an oracle (shell checks) that decides when a task is done. Drones claim tasks from a shared board,
publish contracts for each other, and finish a task only when its checks pass. Everything is in files
under `<store>/hive/<goal>/`. In a collective shared over the mesh ([team sync](team-sync.md)) the goal,
its board, contracts, messages, guard flags and spend sync to every member as signed lines, so drones on
several machines work one board and one budget; the hidden oracle, its scores, joins, runs and logs stay on the node
that has them, and so do the commits (see [Across machines](#across-machines)).

```console
$ borg hive init parser --spec "Ship the parser" --oracle "unit=cargo test -q" --budget 20 \
    --task "t1|work|Add the parser|src/parse/*.rs" --task "t2|work|Use it|src/main.rs|t1"
$ BORG_HIVE=parser claude          # or, in a running session: borg hive join parser --session <drone>
$ borg hive board
```

| Command | Effect |
|---|---|
| `borg hive init <goal> --spec TEXT\|--spec-file F --oracle NAME=CMD... --budget USD [--timeout S] [--repo NAME[@NODE]]... [--task "ID\|role\|title\|files,..\|deps,..\|NAME=CMD"]... [--hidden-dir DIR --hidden CMD... [--hidden-timeout S]]` | A goal, its oracle and its first tasks. Without `--oracle` it refuses: done means the oracle passes. `--hidden-dir`/`--hidden`: a hidden oracle, see [Guardrails](#guardrails-and-the-hidden-oracle) |
| `borg hive ratify <goal> [--budget USD]` | The engineer approves the goal's plan from their terminal: every task not approved yet (each shown as its card) is approved on the board, which every node syncs, and interactive drones may take it from then on; a queued plan question of the goal is answered so. `--budget USD` raises the goal's budget instead, once its metered spend stopped new claims, and settles the queued budget question. Refused to a drone and inside any harness session: there the engineer answers at their own prompt ([The planning session](#the-planning-session)) |
| `borg hive board [<goal>] [--json] [--watch]` | The tasks by status, with holders, age, rework and dependencies |
| `borg hive join [<goal>]` (inside a session) · `borg hive leave` | The session it runs in (Claude Code's `CLAUDE_CODE_SESSION_ID`, omp's `PI_SESSION_ID`, codex's `CODEX_THREAD_ID`; in opencode, whose shell has no session id, the borg plugin binds the session that ran the command with `borg hive join [<goal>] --bind --session <id>`) becomes an interactive drone of the goal, or without one of every open goal of this collective in this repo, and works in a worktree of its own (`.worktrees/hive-i-<drone>`). No other session is touched. When nothing is open to it, its stop asks it to run `borg hive wait` in the background; `/clear` keeps it the same drone. It takes only tasks of a plan the engineer approved ([The planning session](#the-planning-session)), and its model never runs `approve`, `integrate`, `score`, `ratify` or answers a plan question. Refused while a `borg hive run` of the goal runs on this node, and `borg hive run` is refused while an interactive drone serves the goal. `leave` releases its claim, stops its wait and makes it a plain session again; the node leaves the goal with its last drone. In a repo of no collective it says so on stderr with the `borg setup` that adds the repo, and changes nothing ([Interactive drones](#interactive-drones)) |
| `borg hive wait [--stream\|--queue]` | An interactive drone waits for work without spending tokens, claims the next task open to it, prints it and exits; Claude Code starts the next turn with that output. `--stream` is the omp and opencode adapters' waiter: it runs from the join for the session's life and prints each wake as one JSON line (`{"wake": "<text>", "left": false}`), which the adapter sends into the session. `--queue` is codex's: a codex drone's stop starts it, and it wakes the session with `codex queue --thread <session> --message <text>`; a queue that fails releases the claim at once. Never while the engineer's own turn or a stop runs in the session. A claim no hook event of the session takes up within 60 s is released. A stuck goal is said once, then the drone waits on; a drone of one goal leaves once it is finished for good. Once its session is gone (its Claude Code, omp, opencode or codex process ended) it leaves and exits. While no `borg mesh serve` runs on the machine, one waiter does the node's goal work (see [Interactive drones](#interactive-drones)) |
| `borg hive join <goal> [--session ID\|DRONE]` · `borg hive leave` | A running drone joins the goal, or leaves it (its claim is released). A drone started with `BORG_HIVE=<goal>` joins at its first hook |
| `borg hive join <goal> --here [--drone NAME]` · `borg hive leave --here` | Every session that starts in this repo (its git toplevel, or a worktree's own) joins the goal at its first hook, as drone NAME (default `j-` and a hash of the directory). For a harness whose sessions lose the launch env (opencode 2.x's background service). `leave --here` stops new joins; sessions that joined stay |
| `borg hive run <goal> --drones N [--harness H] [--model M] [--base REF] [--iter-timeout S] [--dry-run] [--yes]` | Headless drones in worktrees, once the plan is approved, see [Running drones](#running-drones). Exit 1 when you said no to the plan (nothing was started or claimed) |
| `borg hive open <goal> --id ID --role work\|plan\|verify\|integrate --title T [--repo R] [--files a,b] [--deps x,y] [--accept NAME=CMD]... [--again] [--group K]` | An engineer opens a task; `--again` reopens a blocked one. `--group K` (2 to 19) opens K attempts `ID.g1`..`ID.gK` of one work task, each another drone's; see [Groups](#groups-of-attempts) |
| `borg hive claim [TASK]` | The drone claims a task (the next open one without TASK); exit 3 when another drone won it, exit 1 when another drone holds it (the message names the holder and tells the drone not to work on it). A task with deps is open once they are verified, and its claim first brings their verified work into the drone's tree (see below). When that work does not apply there, `claim TASK` exits 1, and `claim` alone skips the task and takes the next one; a skipped task is not offered to that session again. A verify task of work done on another node whose snapshots this repo lacks and no bundle brings (or a fix task whose tree it lacks) is not offered, and `claim TASK` exits 1 with "its work was done on node X and is not in this repo" ([across machines](#across-machines)). A board it cannot write exits 1 with the error, never as a lost race. Inside a drone of `borg hive run` without `--session`, the session is the drone's (`BORG_HIVE_DRONE`), not whichever drone of the repo was seen last |
| `borg hive progress "<note>"` · `release` · `block "<why>"` | Report on, give back or block the drone's claim |
| `borg hive done [--note N]` | Run the task's checks with their own timeouts (no hook time limit), renewing the claim while they run; on a pass publish done and open its verify task. Exit 3 when the claim passed to another drone meanwhile: the board ignores that done, and the message names the holder |
| `borg hive verify --pass\|--reject "<why>"` | The verdict on a verify task (or end the verifier's turn with the line `VERDICT: pass` or `VERDICT: reject <why>`); a rejection reopens the work with the note. The work's diff is checked first, see [Guardrails](#guardrails-and-the-hidden-oracle) |
| `borg hive score <goal>` | The goal's hidden oracle scores every judged task not scored yet (`borg hive run` does this when its drones end). The engineer's: refused to a drone of a hive goal. Exit 1 when an attempt could not be checked out (run it from the goal's repo) |
| `borg hive integrate <goal>` | Once every task is verified: merge the accepted attempts and run the goal's oracle and every task's checks on the combined tree, as `borg hive run` does itself (see [Running drones](#running-drones)). Exit 0 when it passed (the goal is finished), 1 when it was rejected (a fix task is open) or blocked, or when an accepted attempt's commits are on another node only and no bundle brings them ("integrate on a node that holds these commits", nothing written), 2 when no integration is due. Needs git 2.40+. The engineer's: refused to a drone of a hive goal |
| `borg hive approve <goal>` | Run the checks of goal `<goal>` that another node wrote (a synced goal's oracle, a task another node opened with `--accept`) on this machine: prints each command and approves it here (the local `approved.jsonl`). Until then such a task is not offered or claimable here, its checks never run, and `borg hive board`, `borg top` and the Hive tab list them. The goal's own `goal.json` oracle and the checks of tasks opened here need no approval. The engineer's: refused to a drone of a hive goal |
| `borg hive contract "<text>" --scope a,b [--supersedes ID]` | Publish a contract scoped to files (globs), `id:NAME` or `repo:NAME`. Superseding another drone's contract asks the engineer through `borg queen` |
| `borg hive ask "<text>" [--to TASK\|DRONE\|SESSION]` · `borg hive tell "<text>" [--to …] [--re ID]` | A message to the goal's drones: everyone, whoever holds a task, a drone (that name on the sender's node) or a session. Each recipient's next prompt shows it once, with the command that answers it, while it is fresh (30 min, a lease); never to its sender. One message per sender and recipient per 10 min (the heartbeat). `tell --re ID` without `--to` answers the asker's session |

Drone commands take `--session ID` (needed when the repo has more than one drone; the hook's context
prints the exact command) and `--dir DIR`.

**Files.** `goal.json` (written once; `goal.jsonl` is what syncs of it, without the hidden oracle),
`tasks.jsonl` (the board's events: open, claim, heartbeat, progress, done, verified, rejected, released,
blocked; each line names the node that wrote it), `contracts.jsonl`, `messages.jsonl`, and `oracle/<task>.<ms>.log`
(the last 8 KiB of a failed run, the newest 20 kept). `joins.jsonl` (directories whose sessions join),
`spend.jsonl` (each iteration's cost, naming the node that spent it; synced) and `runs.jsonl` (each
`borg hive run`, local), below, and
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
(ms, blake3 key) wins, on every machine alike; the loser is told at its next hook (a prompt, any tool
call or its stop) and moves on. While its latest claim is lost and it holds no task, its edits and
writes are refused with the reason (a refused edit raises no guard flag). When its turn ends, or at
its next `borg hive claim`, its worktree is set back to where the claim started: the changes since then
are kept in a snapshot commit the message names, pinned at `refs/borg/hive/<goal>/<sha>` (`git diff
<start> <snapshot>` shows them), so nothing of the lost work reaches its next task. This happens only
while the session holds no task, and not in a tree another session holds a task in. Should a line that
syncs later give the task back to the drone after it was told it lost, the drone releases it. A drone
that works 30 minutes without any hook event (one long tool call) loses its claim too.

**In the harness.** At a prompt the drone sees its claim (or the next open task and the command to claim
it), the board's changes since its last prompt and the contracts in its claim's scope, each contract
once. After an edit it sees contracts the edit lands in, and files outside its claim's scope (advice,
never a refusal). At the stop the task's checks run: while they fail the stop blocks, at most 3 times per
claim, then the task is published blocked. When they pass, done is published, a verify task opens (its
author may not claim it), and the drone is handed the next open task; a done the board did not take (the
claim passed to another drone meanwhile) is reported instead, and nothing is handed over. A suite slower than the stop hook
(Claude Code 90 s, omp 30 s) needs `borg hive done`. A harness without prompt context gets this text at
the stop. A paused drone is never blocked by its oracle.

Nothing changes for a session that never joined a goal.

### The planning session

Tell any session what the drones should build. Its borg skill turns your request into a goal:
small, independent tasks, a contract task before the tasks that use its interface, and a check for
every task (`--task "ID|work|title|files|deps|NAME=CMD"`, or `borg hive open ... --accept NAME=CMD`).

```
$ borg hive init parser --spec "..." --oracle "all=cargo test" --budget 20 --task "ast|work|..." ...
[borg hive] The plan of goal parser waits for the engineer's approval (queen question q1f0c9e2a); no drone takes a task of it before:
  ast  define the AST · files src/ast.rs · checks ast=cargo test ast:: · risk normal (ordinary work)
  ...
```

- **You approve the plan at your own prompt** in that session. `yes` approves it. `no, because <why>`
  refuses it: the reason is a veto, borg learns from it, and the session revises the plan with you
  (changed or new tasks with `borg hive open`), which is then asked again. Anything else you type
  leaves the question open, and the session is reminded of it.
- **No drone takes a task of a plan you have not approved**, whatever its harness: not its waiter, its
  stop's handover or `borg hive claim`. The approval is a line of the goal's board (`ratified t1 t2`),
  decided under the board lock, so every node reads it and no approval is ever taken back. Once you
  approve, every waiting drone wakes on that board change, and the session tells you how many are
  waiting: "3 drone(s) on 2 node(s) waiting".
- **The session's model cannot approve it.** `borg queen answer` and `borg hive ratify` refuse inside a
  harness session, and no other session is offered the question. A goal you opened from your own
  terminal has no session to ask: approve it there with `borg hive ratify <goal>`.
- **A task may use subagents or workflows.** They run under the drone's session, so its claim, its
  worktree and its contracts apply to them. They never run `borg hive claim` or `borg hive done`.
- **Budget.** Each claim of a Claude Code drone adds one line to the goal's `spend.jsonl` when it ends
  (done, verdict, release or the session's end): the usage of its transcript since the claim, and of
  its subagents' transcripts, priced by model (`exit: "interactive"`). omp, opencode and codex report no
  usage, so their lines say `unmetered` and charge nothing. When the goal's metered spend on every node
  reaches its `--budget`, its interactive drones claim nothing more, and you are asked once: "Raise the
  budget of G?". Answer with the new budget in dollars at the planning session's prompt, or run
  `borg hive ratify <goal> --budget USD`. There are no per-drone shares.
- `borg hive run` keeps its own plan gate (below) and its inputs.

### Interactive drones

A session becomes a drone when its model runs `borg hive join` in it (ask it to "join the hive"). It
then works in its own worktree and, with nothing open, ends each turn on a background `borg hive wait`
and the line `Drone idle: waiting for hive work, which starts here by itself when a task opens.` Leave
it alone: the next task starts a turn on its own. It stays a drone until you ask it to leave the hive.

**omp and opencode.** Ask the session to "join the hive" as in Claude Code. The difference is who
waits. In omp and opencode the borg adapter keeps the drone's waiter itself, from the join for as long
as the session lives, so no turn ends on a background command and the stop never asks for one:

| | omp (18.12) | opencode 1.x | opencode 2.x |
|---|---|---|---|
| Who binds the session | `borg hive join` itself, from `PI_SESSION_ID` | the plugin, from its `tool.execute.after` of the session that ran `borg hive join` (`borg hive join --bind --session <id>`); the command alone binds nobody | as 1.x, from `execute.after` |
| How a wake goes in | `pi.sendMessage({customType: "borg-hive", ...}, {deliverAs: "nextTurn", triggerTurn: true})`: starts a turn on an idle session, waits behind a running one | `session.promptAsync`, once the session is idle | `session.prompt({delivery: "queue"})`, once the session is idle |
| Idle | omp's own `nextTurn` queue | from a prompt or tool call to `session.idle` (an Esc-halted turn too) | from a prompt or tool call to `session.execution.succeeded`, `.failed` or `.interrupted` |

- The waiter (`borg hive wait --stream`) hands nothing while the engineer's own turn runs, including
  the turn that ran the join. The join's turn ends at its stop.
- An opencode turn you halt (Esc, a provider error) has no stop: the plugin tells borg (`borg hook
  opencode halted`), so work goes in as soon as the session is idle.
- A woken turn's first tool call or stop takes the claim up. Otherwise it is released after 60 s, as in
  Claude Code. A wake opencode refuses is sent again a few seconds later.
- A stop that finishes a task says the result first, then `Drone idle: ...`, as a plain block, which
  both adapters send back into the session. borg marks it `"hive": true`. In opencode such a hive
  handover is never dropped because you typed while the stop ran: it holds a claim, so it queues behind
  your turn. It is never counted against the one round of questions an engineer prompt gets. A drone's
  block of questions alone has no mark and follows the rules for questions.
- While a drone's stop runs, its waiter claims nothing: the stop hands the next task, once.
- The pid the adapter sends with every event (omp's or opencode's own process) is the drone's harness
  process from its first event on. The waiter and the node's reaper end the drone with that process.
  One opencode process may run several sessions: another session's `borg hive join` binds only that
  session.
- The session's end (omp `session_shutdown`; opencode `session.deleted` or the plugin's unload) makes
  the drone leave, which stops its waiter.

**codex.** Ask the session to "join the hive" as in Claude Code: `borg hive join` binds the thread
codex names in the command's `CODEX_THREAD_ID`. No codex hook can start a turn in an idle session, so
borg wakes it the way codex itself offers, with `codex queue` (codex 0.149 or newer):

- The stop that ends a drone's turn starts its waiter (`borg hive wait --queue`) in the background,
  detached from the hook, and passes at once: the stop never waits on it, and the stop's 90 s timeout
  stays as it is (changing a hook's timeout makes codex ask you to trust borg's hooks again). One waiter
  runs per drone, for the session's life.
- When a task is open to the drone and you are not typing in the session, the waiter claims it and runs
  `codex queue --thread <session> --message "[borg hive <goal>] Work arrived. ..."`. The idle session
  takes the message up as a new turn, a few seconds later. That turn's prompt, the message itself,
  takes the claim up; otherwise it is released after 60 s, as in Claude Code (`codex queue` also takes
  a message for a thread no codex runs). After such a release the waiter claims nothing more until the
  session's next hook event, so a thread you left (`/new`, `/resume`) is not handed the same task
  again every minute. A `codex queue` that fails (a nonzero exit, no `codex` on the PATH) releases the
  claim at once, and the waiter claims nothing for 10 s.
- If you type in the session after the message was queued, codex runs your turn first and the task's
  turn after it. Your turn never touches the task: its stop runs no check, reports no done and blocks
  nothing for it.
- A stop that finishes a task says the result first, then the idle line, as a plain block, which codex
  continues the turn with.
- Your own turn in the session holds the waiter off from your prompt to the turn's stop. A turn you
  interrupt (Esc) has no stop: borg's `Interrupt` hook ends it. That hook is new, and codex runs it
  only once you trust it in `/hooks` (an install from before it adds it with `borg install codex
  --apply`). Without it, a quiet turn stops holding the waiter off a minute after its last event; work
  sent then waits in codex's queue behind any turn that still runs, so nothing needs doing.
- The join's own hook checks that the session's hooks carry the thread the join bound. If they carry
  another id, the session is no drone, and the session is told why.
- codex runs its hooks as children of its own process, so the hooks tie the drone to codex's process.
  `/quit` leaves at once (SessionEnd). Once the process is gone without one (a closed terminal, a
  crash), the waiter sees it, leaves for the drone and exits.
- In codex's default sandbox (`workspace-write`) the repo's `.git` and borg's store are read-only to
  the model's commands, so the join cannot register the drone's worktree there. It says so: run it
  outside the sandbox (codex asks you to approve the escalated command). borg's hooks run outside the
  sandbox and need nothing.

**What a stop says.** Claude Code shows a drone's stop as hook feedback, not as a hook error. When a
task's checks pass, the stop says that first, for example `[borg hive] t1 done: its checks passed
(unit); verify task t1.v1 is open for another drone.` When a check fails, it says which one and why.
Only then does it hand over the next task or ask for the wait. The model writes its last message before
the checks run, so the done's note on the board is the stop's own summary: the checks that passed, the
files the attempt changed and its commit subjects. The model's message is not used. A note given with
`borg hive done --note` is kept as written.

**Ending a session.** The hive takes nothing from the board when a drone's session ends:

- `/exit` while the wait runs shows Claude Code's `Background work is running` dialog. **Exit and stop
  tasks** stops the wait, and the session's end releases any claim it held and leaves the hive. The
  node leaves the goal with its last drone. Ctrl-C or Ctrl-D twice and logging out end it the same way.
- **Move to background and exit** backgrounds the session in agent view, and the wait carries over
  with it. Once the Claude Code process the drone joined under has exited, the wait sees that and
  leaves the hive the same way, then exits. No orphan waiter is left. A session you attach to again is
  a plain session; ask it to join again if you want it as a drone. (This path follows Claude Code's
  agent-view docs and has not been watched in a live session yet.)
- **Stay** keeps the drone waiting.
- A terminal closed under a session, or a crash, has the same outcome as moving to the background: the
  waiter leaves for the session. A drone whose session dies in the middle of a task, with no wait
  running, is left for by the node's worker (below): its claim is released `its session is gone`
  within a second or so of the worker's next look.
- `/clear` and compact keep the same drone.

**The node's worker.** A runner's threads did more than run drones: they kept the node in the goal,
said the board finished, integrated it. For interactive drones `borg mesh serve` does that work, on a
thread of its own, for every goal a drone of the machine serves:

- It beats for the node every 3 s, and at once when another node claims, so peers never take the
  node's claims for a dead node's while its drones idle or think.
- It keeps the node's roster of takers: the drones waiting for work or holding a task. A drone in your
  own turn, or with no wait running, drops out after 10 s, so a task only it could take shows as stuck
  instead of waiting on a session nothing wakes.
- It says the board finished, which tasks the node may not take, and what is stuck.
- It reaps a drone whose harness process is gone (Claude Code's, omp's or opencode's; above).
- Once every task is verified it runs `borg hive integrate` as a child process (output in the goal's
  `logs/node-integrate.log`), so the goal is integrated and finished while every drone is idle.

It never makes the node join or leave a goal. Ctrl-C (or SIGTERM) stops the daemon and writes nothing,
and its start writes no join, so the claims of the machine's drones live through a restart. Drones'
joins and their last leave are what move the node's membership. While the daemon is down its peers
hear nothing of the node: after 10 s they may treat its claims as a dead node's (a task done twice at
worst), as during any network stall.

Without a daemon (one machine, no mesh) the waiter of one drone does the same work while it holds the
node lock (`hive/node.lock`); the daemon takes the lock once that waiter exits. With no daemon and no
wait running, nothing integrates until one of them runs (`borg hive integrate` by hand still does).

**The first wake after an idle stretch costs more.** Claude Code's prompt cache lasts 5 minutes. A
task that arrives after a longer wait starts a turn that reads the whole conversation again uncached.
In the live probe that was about 42,000 tokens written to the cache, about $0.16 on Sonnet. Wakes that
come within 5 minutes of each other read from the cache. Waiting itself costs nothing, however long.

**A repo in no collective.** The hive keeps its board in a collective's store, so `borg hive init` and
`join` in a repo that belongs to no collective refuse and print the command that adds the repo:
`borg setup --private <repo>` (this machine only) or `borg setup --team <name> --paths <repo>`. Run it
in a terminal. A fresh `borg setup` that took every default adds no repo, and borg never adds one for
you. A repo on the config's `off` list is told that the hive does not run there.

**Watching the hive.** The Hive tab of `borg web` and the live screen of `borg top` show each goal of
interactive drones from the files the hive keeps anyway, so nothing needs setting up and the page shows
the goal from its first task even when you open it late:

- **Tasks over time**: open, claimed, done, verified, blocked and stuck, rebuilt from the board's own
  lines (each point is the board as of that moment).
- **Spend**: what was spent against the budget, how much of it was metered, and how many claims had no
  usage to meter (omp, opencode and codex sessions that report none).
- **Nodes**: whether each node is in the goal, how long ago this machine last heard its beat (on this
  machine's clock), its takers, and who does this node's goal work: `daemon` (`borg mesh serve`),
  `waiter <drone>`, or `none`. When an integration is due and nothing runs it, the goal says
  "integration due, no node worker".
- **Interactive drones** of this machine, grouped by state: `working <task>`, `busy` (your own turn in
  the session), `waiting` (its wait runs), `not waiting`, or `gone`. Each shows its harness, the tasks it
  finished and judged, its spend, and its wake latency (from the hand to the turn that took it up, on this node's clock).
  Click a drone to see each wake, including wakes no turn took up.
- **Stuck and declined** tasks with their reasons, and every **integration** with its result and the
  newest lines of the node's integration log.

### Guardrails and the hidden oracle

A drone can pass a visible oracle by changing it. The hive lets that happen, records it, and makes it
useless (docs/design/hive.md, L8):

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
    tests run), or the test runner's part of `pyproject.toml` (`[tool.pytest*]`, `[tool.coverage*]`,
    `[tool.tox*]`), `setup.cfg` (`[tool:pytest]`, `[pytest]`, `[coverage:*]`) or `package.json` (`jest`,
    `mocha`, `vitest`, `ava`, `nyc`, `c8`, the `test` scripts); their other parts are not counted;
  - outside them, deletes or renames a test (a file under `test/`, `tests/`, `spec/`, `specs/`,
    `__tests__/`, or named `test_*`, `*_test.*`, `*.test.*`, `*.spec.*`, `*_spec.rb`), or weakens one: a
    removed line that no added line of the same hunk matches in shape (its tokens, with names, strings
    and numbers as holes; assertion words such as `assert*`, `expect*`, `return`, `true` kept) without
    making two different names one (`assert got == want` turned `assert got == got`), fewer test
    declarations than before (`#[test]` turned `#[inline]`, `@Test` turned `@Deprecated`, `def test_x(`
    renamed `def x(`), an added skip, disable or focus marker (`@Disabled`, `@pytest.mark.skip`,
    `it.only(`, `xit(`, `t.Skip()`, `#[ignore]`, a bare `return`, an `if False:` or `if (false) {`), or
    any binary change. Blank and comment lines do not count. Added lines are judged too: a kept line
    moved to another indentation (an assertion put under a new block), a hunk that opens or closes a
    block comment or triple-quoted string around lines it leaves as they are, and, in a Python test,
    a name bound again in the same scope (a second `def test_parse`, `parse = lambda s: s`, `from q
    import parse`: the earlier one no longer runs) each weaken the test. A test whose content diff git
    cannot give counts as weakened;

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
hive run parser: the plan waits for the engineer's approval (queen question q3c1d09aa); no drone starts before it
  t1  add the lexer · files src/lexer.rs · checks unit=cargo test lexer · risk normal (ordinary work)
  t2  document the grammar · files docs/grammar.md · risk low (docs and tests only)
  t3  drop the legacy tokens table · files migrations/ · after t1 · risk high (an irreversible change)
  answer: y here, or `borg queen answer q3c1d09aa a` to approve, `borg queen answer q3c1d09aa "<why not>"` to stop the run
approve the plan? [y/N, or why not] y
hive run parser: plan approved by ann@example.com (queen question q3c1d09aa): 3 task(s), 1 high-risk
d1  integrate.1  integrated 3f2a9c1b07de: the oracle passes
d1  hive-parser-1  $1.84/$6.67  4 iter  tasks done: t1,t3.v1  end: goal finished
...
board: 7 task(s) (7 verified); the goal is finished
integrated 3f2a9c1b07de… (integrate.1, refs/heads/hive/parser/integrated)
```

**Plan approval.** Before any drone starts, `borg hive run` shows the goal's plan as one intent card
per task (its intent, files, deps, checks and a risk class) and asks you to approve it, once, as a
queen question. Answer at the run's terminal (`y`; Enter or `n` is no; or say why not), or from anywhere
with `borg queen answer <id> a` (approve) or `borg queen answer <id> "<why not>"`. The run waits, under
its lock, with no worktree made and nothing claimed, and a no (or a dismissal) stops it with exit 1. A
no in your own words is recorded like `borg no`: `borg learn` can turn it into a lesson. The risk class
is `high` when the queen's classifier finds an irreversible change, an external mutation, a public API
or schema change or a cross-repo decision in the task's title, `low` when every file in scope is docs or
tests, and `normal` otherwise. The decision is kept in `<store>/hive/<goal>/plan.jsonl`: a rerun of the
same plan does not ask again, and a new or changed task does, also one opened while the run waits
(after your approval the run asks again for the changed plan before any drone starts). `--yes` approves the plan on the command
line without a question or a wait, for scripts and evals (borg-eval's runs pass it); without a terminal
and without `--yes` the run waits for an answer through the queue. When a high-risk task is claimed the
first time, the queen asks you about it once (a question of its kind, which the queen's attention reward
counts), and the drone is told to go on with that kind's default meanwhile (an external mutation is held
until you approve). The attempts of a `--group K` task are one task: one question, and each attempt's
drone is told. A drone can neither answer these questions nor be shown them, also in a worktree
`borg hive run` made when its harness drops the environment, and `borg queen dedupe` never dismisses
them. `borg top`, `/api/hive`
and the Hive tab show the plan's state (`approved`, `refused`, `waiting`, `not asked`) and its cards. The
queue is this node's: on another node of the mesh, that node's run asks its own engineer.

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
task built on the tree with its markers, which changes the path or leaves it without markers (a
modify/delete or binary conflict has none: the fix's verified tree is the resolution). The merge
uses `git merge-tree --merge-base`, so it needs git 2.40+; an older git blocks the integration with
that reason. The result is a commit at `refs/heads/hive/<goal>/integrated`, which is the hive's result
for you to review or merge. A conflict rejects it. Otherwise the goal's oracle and every task's own
checks run on it in a scratch checkout, and a failure rejects it. A rejection opens the fix task
`integrate.<n>.fix`, whose claim resets that drone's worktree to the combined tree; a joined session
claims it in a checkout of that tree instead (`git worktree add <dir> <commit>`, as `borg hive
integrate` prints). Once the fix is verified, the next integration includes it, built on the same
commits as before with the fix last. If that integration fails for the same reason (the same
conflicted paths, the same failing or timed-out check) and it merges no attempt the rejected tree
did not already hold, it is blocked instead of opening another fix: the next fix would start from the same tree. The runner
prints why, with the fix's id and `borg hive open <goal> --id integrate.<n> --again` to retry once you
changed something (a new task, a corrected check). The goal is finished only when an integration passes after the last
task was verified. A model session never claims an integrate task; `borg hive integrate <goal>` runs
one for a goal worked without `borg hive run`.

Each drone gets an equal share of what is left of the goal's budget (every earlier run's spend counted,
on every node of the mesh) and runs headless sessions of the harness one after another, launched with
`BORG_HIVE=<goal>` and `BORG_HIVE_DRONE=d<n>`: borg's own hooks join each to the goal, show it the board
and gate its stop on the oracle. Claude Code runs as `claude -p --permission-mode bypassPermissions`
with your settings (so borg's hooks run) and `--max-budget-usd` set to the drone's dollars left, so it
stops itself; codex, omp and opencode 1.x get no budget and are checked between sessions, so they can
overshoot by one session; a session cut off at `--iter-timeout`, or one that failed, without naming its
cost is charged the drone's dearest session so far (or the rest of its share). Drones run with every permission inside their worktree: run it on a repo you
are willing to let them change. Every session is cut off at `--iter-timeout` (default 1800 s).

A drone stops when the board is finished (every task verified and integrated) and every node in the
goal said its board was too, its share is spent, the
goal's budget is spent (by any node's drones: see [Across machines](#across-machines)), a session ended
without naming its cost or at $0 (codex reports tokens only and a subscription model $0, so such a
drone gets one session), two sessions
in a row failed or timed out, two made no board progress, or, with nothing open to it, a task is stuck
and nothing else on the board can move (no task held, none another drone may still take): no running
drone of a node in the goal may take it (a verify task of this node's work with no other node's drone
left, a blocked task, checks approved on no node in the goal; each node says on the board which tasks it
may not take). The run prints each stuck task and why,
and says so on the board (`borg hive board`); a node that joins and may take it takes it up. With
nothing open and nothing stuck a drone waits, however long: a peer's line may open work for it.
Whatever an ended session still held is released (the note names why), so no claim waits for its
lease. A verify task is never claimed by a session of the drone that did the work. Exit 0 when the board
is finished for good (every node in the goal said so), 1 when it is not (a stuck task included), 2 when a precondition fails: an unknown goal, `--drones` outside 1–20, a
goal without a budget, a repo outside the goal's, a harness that cannot run headless, a group with more
attempts than `--drones` (see [Groups](#groups-of-attempts)), or another run of the goal still going (one
at a time).

A re-run shares what `spend.jsonl` says is left, whatever the drone count. The run's last lines say the
goal's total by node (`goal g: $1.25 of $1.00 spent (here $0.75, 1a2b3c4d $0.50)`); `--dry-run` says it too. A run killed mid-session
(Ctrl-C, a closed terminal) takes its sessions with it; the next run of the goal charges each of them
as a cut-off one (`exit: "killed"`) and releases their claims before it starts. Superseding a contract
of your own drone (any of its sessions) needs no answer through `borg queen`. `borg top` and the web page's Hive tab show
each goal's board, its spend (the goal's total over every node, and per drone: another node's drone as
`d1@<its node>`) and its newest contracts.

### Groups of attempts

```console
$ borg hive open parser --id lexer --title "Write the lexer" --files src/lexer.rs --group 3
opened lexer.g1 (work) Write the lexer in goal parser
opened lexer.g2 (work) Write the lexer in goal parser
opened lexer.g3 (work) Write the lexer in goal parser
group lexer: 3 attempts, each another drone's (`borg hive run` needs one drone more, to verify them); the best is integrated
$ borg hive run parser --drones 4
$ borg learn --group parser/lexer
group parser/lexer: reflection group-parser.lexer recorded
1 new lessons, 0 merged into existing ones
```

A group runs one work task as K independent attempts (docs/design/hive.md, L4; best-of-N).
Each member `<id>.gN` is an ordinary task: claimed, done, verified, reworked on a rejection. A session
or drone that held or verified one member is never offered or allowed another, and one that held a
member never verifies another, so the K attempts are K drones', independent, and none is judged by a
rival (a run needs at least K+1 drones; one verifier may judge every member). The drones of one run share a harness and a model, so they differ by
sampling only. Every member must be verified for the goal to finish. The integration merges only
the group's best accepted attempt: the one the hidden oracle passed (a goal with one scores the group's
attempts before it merges), else the one with the fewest rejections, then the earliest verified.

Once every member is verified and every attempt has its reward, the group is settled. Each attempt
won or lost: by the hidden oracle's score where this node has one, else by the verdict (verified and
not hacked wins; a rejected attempt that was reworked lost, its rework is a new attempt). A settled
group with a winner and a loser becomes one episode, `group-<goal>.<id>`: the task, which drones won
and lost and by what, and each attempt's account, the verifier's note and its change, redacted and
cut, winners first. `borg learn` asks the model once with its own prompt what the winners did that the
losers did not, and writes that as a lesson whose `episodes` name the group (or merges it into a
lesson that says it already). Every `borg learn`, the one at each session's end included, records
the settled groups opened on this node first, once each: one node learns each group. `borg learn
--group` reflects on a group from any node. A group of one, or one whose attempts all won or all
lost, writes nothing. `BORG_GROUP_REFLECT=off` stops the automatic reflection (an eval arm without
it); `BORG_LEARN=off` stops it too.

`borg top` and the Hive tab show each group: its attempts, who won and who lost and by what, the
member the integration takes, and its reflection (`waiting` until settled, `to reflect`, `learning`,
`lesson <id>`, `not a lesson`, or `no spread`).

Known limits: a task cannot depend on a group, nor on one of its members (`hive open` refuses a dep on
a member: only the best one is integrated); and a rejected member is reworked like any task, not
dropped once another member won.

### Reworks

A work task whose attempt the verifier rejected and whose later attempt was verified is the hive's own
lesson source: the verifier caught a mistake and the rework fixed it. Every `borg learn`, the one at
each session's end included, first records each such task as one episode, `rework-<goal>.<task>`: the
task, every rejection's note (the verifier's why), each rejected attempt's account and change, what the
accepted rework changed relative to the last rejected attempt (redacted and cut), and the drones. It
then asks the model once, with its own prompt, what mistake the first attempt made that the verifier
caught, and writes it as a lesson that prevents it (source `rework`), or merges it into a lesson that
says it already. Once per task, on one node: for a goal with a hidden oracle the node that holds it
(the goal's own node, the only one with the held-out scores), else the one the verifying verdict was
written on.

What counts: the accepted attempt is the board's, verified and not hacked; with a hidden oracle its
held-out score must pass, and the episode waits until the goal's node has scored it (`borg hive score`);
another node, which never sees the scores, shows the task as `waiting`. A
rejection counts when the verifier rejected the work on its merits: a hacked verdict (a guardrail), an
unchecked one (no snapshot, a dependency's work missing) and one the held-out tests pass (the verifier
was wrong) do not. Several rejections before the pass make one episode with all their notes. A task
verified at its first attempt writes nothing. A fix task after a failed integration counts like any
work task: its own rejection and verified rework. A group's member counts too (its group's reflection
compares drones; a rework compares one task's attempts over time).

`BORG_REWORK_REFLECT=off` stops it (an eval arm without it); `BORG_LEARN=off` stops it too. `borg top`
and the Hive tab show each reworked task: how often it was rejected, the drones, and its reflection
(`waiting` for a hidden score, `to reflect`, `learning`, `lesson <id>`, `not a lesson`).

### Playbooks

```console
$ borg learn
$ borg queen
app (1)
  1  Add to the team's playbook: "Run the parser's tests before calling it done"?
$ borg queen answer 1 b
q3f2a91c0 answered: the edit is promoted into ~/.local/share/borg/collectives/corp/playbooks.jsonl
```

A playbook is the team's procedure for a kind of task: its shape ("fix the parser") and a short list
of steps (docs/design/hive.md, L5, the playbook layer). `playbooks.jsonl` holds edits, never whole
playbooks: each adds a step, changes one, or removes one. Every reader folds the same set of edits
into the same playbooks in any order (the edits replay by time: a removed step can be added again, a
step's text is its latest add or change, and a change promoted on a machine whose clock is behind the
step's add still lands), so the file syncs over the mesh like lessons. A line whose ids its content
does not derive is ignored, so no synced line can rewrite a promoted step under the same version. A playbook's id comes from its repo and its
shape's words; its version (`P…@<edits>.<hash>`) names the exact set of edits.

After a hive attempt wins, or a plain session ends clean, `borg learn` asks the model once, shown the
playbooks of that task's shape with their step ids, for at most one edit. An attempt wins when it was
verified and not hacked, or, with a hidden oracle, passed by it; of a group, only the integrated
member's attempt, and the attempts it beat are in the prompt. A plain session (no hive) ends clean when
it landed commits, its own last test run passed, and you neither corrected nor vetoed it (`borg no`);
its task is its first task prompt (not a later "commit it") and its change runs from where it started
to its newest commit. It is reflected when it ends, never while idle; when the model does not answer,
it waits for a later `borg learn` (`playbooks-plain.txt`, local). Only work done on this node is
reflected, once per attempt or session (`playbooks-reflected.txt`, local). The edit is never applied by learning: it is queued as a queen
question whose default keeps the playbook as it is, and only an engineer's `promote` answer appends it,
stamped with the question and the engineer. An edit asked once is never asked again. A step is one
line of at most 300 characters, and the question shows it whole: what you promote is what drones read.

A drone with a claim sees the playbooks whose shape its task's title and files name (at least 60% of
the shape's significant words, the queen's bar for a text stating something), of the task's repo or
of none, once per version, after its contracts. A session outside the hive sees them when its prompt
names as much of the shape, the same bar: a prompt is how a plain session states its task. Each version shown is logged (`playbook` in `outcomes.jsonl`), so
the verdict on the drone's attempt, a `borg no` and a later regret credit that version as they credit
the lessons shown, and its usefulness belief is the lessons' (memory.rs). The Hive tab lists each
playbook with its steps, the belief of every version shown and the edits waiting for an answer (a
playbook with nothing promoted yet shows with its waiting edits); the feed shows playbooks shown,
edits proposed and edits promoted. `BORG_PLAYBOOKS=off` shows and proposes nothing.

Known limits: promotion is the engineer's answer only, not yet a held-out eval win at the same budget;
a low belief does not yet hide a version; there is no length or coverage check against brevity bias
beyond the one-edit rule; the group reflection itself still writes an L0 lesson, while the playbook
edit comes from the integrated attempt.

### Across machines

In a collective shared over the mesh, `goal.jsonl`, `tasks.jsonl`, `contracts.jsonl`, `messages.jsonl`
and `flags.jsonl` of every goal sync like lessons: signed by their writer, quarantined when the writer
is revoked, caught up after time offline. A drone on any member's machine joins with `BORG_HIVE=<goal>`
or `borg hive join` once the goal arrived; `borg top` and the Hive tab name a holder's node when it is
not this one.

- **Claims and leases.** A claim is provisional until the next sync round: of two claims made on two
  machines, the earlier one wins on both once they sync, and the loser's drone hears so at its next
  hook after that sync, a tool call included, with its edits refused and its worktree set aside from
  then on (see "Claims are leases" under [Hive](#hive); its done is ignored). With drones active on
  another machine, a claim that won its own read-back waits for one sync round with each peer the mesh
  daemon synced with in the last 2 minutes and whose last dial did not fail (at most 1 s), then reads
  the board again: a rival's earlier claim that had reached that peer is there by then, and the claim
  exits 3 before any work starts (one stamped earlier that reached its own machine later still wins,
  heard at the next tool call). Each wait is a line in the
  goal's local `confirms.jsonl` (task, peers, ms waited, `won`, `lost` or `cap`). A claim of a task
  another drone took since it was picked writes nothing and exits 3. Heartbeats are board lines, so a lease held on one machine is renewed on
  every other one. The winner rule compares clocks: keep the machines' clocks well within the 30 min
  lease. A runner releases only its own node's claims.
- **Which task a drone takes.** The next open task is from the drone's own repo first, then by role
  (verify, integrate, work, plan). Within that, each machine takes the tasks it owns first, oldest
  first: the owner is the machine with the highest `blake3(node, task)` among those whose drones wrote
  a board line within the lease (rendezvous hashing; an engineer's lines, such as opening the goal, do
  not count), so drones on two machines start on different tasks instead of racing for one. A machine
  whose own tasks are taken takes the others newest first, the end their owner reaches last. On one
  machine the order is the oldest task first, as before. Drones of one machine start apart: `dN` takes
  the first task it may claim from its place among the machine's idle drones still running on (the
  runner names them on the board at its start and at each drone's end), so two drones reading one board
  take different tasks, and those the machine's share cannot hold take the others' far end one
  after another. `borg hive run` announces its machine on the board before its drones start (a
  heartbeat of no task by `runner`) and, with mesh peers synced in the last 2 minutes, waits for one
  round with each (at most 3 s; `announced to 1 mesh peer(s), heard back in 40 ms`, and `peers` and
  `waited_ms` in `runs.jsonl`), so every machine's drones start knowing each other's share. On one
  machine nothing is waited for. Each machine still judges liveness from what it has seen: a machine
  whose drones were quiet for the lease can race until its next line syncs.
- **Drone names** (`d1`, `d2`) repeat on every machine: a verify task is barred to the author's session
  everywhere, and to the author's drone name only on the author's node.
- **Commits travel as bundles.** A verdict diffs the author's snapshots. At each `done` in a store
  that is a mesh member, the attempt's two snapshots (claim and done) are pinned at
  `refs/borg/hive/<goal>/<sha>` and bundled `--not --remotes` into the goal's synced `bundles.jsonl`
  (at most 1 MiB; a bigger attempt carries nothing and stays where it was done). A node whose repo
  lacks an attempt's commits unbundles them when a verify offer, a dependency's bring-in or an
  integration needs them, and pins them at the same refs. A bundle is refused unless its commits are
  exactly the claim and done snapshots the board names and every object they reach arrives (it is
  unbundled into a quarantine first, so a refused bundle leaves nothing in the repo); the repo must
  hold the remote's history it was cut from (`git fetch` first, the line is tried again, as after a
  local failure such as a git call that did not finish). The goal's node fetches an attempt's commits
  for its hidden oracle too, so work done elsewhere is scored there, accepted or rejected. Without
  a usable bundle the verify task is not offered there, `borg hive claim` says which node holds the
  work, and `borg hive run` and `borg hive integrate` write nothing and say which work is on which
  node. `borg top` and the Hive tab list each bundle: made here, carried, unbundled, waiting or refused
  and why. A fix task's start (a failed integration's tree) does not travel yet: claim it where that
  integration ran.
- **Secrets stay home.** Task titles and notes, contracts, messages and the synced goal's spec are
  redacted before they are written to the synced logs (the redaction presence prompts get: tokens,
  keys, passwords, `.env` values). Ids, commits, scopes and check commands are kept, so the board folds
  and contracts match as before; keep secrets out of check commands (use the environment). The goal's
  own `goal.json` keeps its spec as written.
- **Checks are shell commands from another machine.** A node runs its own goal's oracle and the
  checks of tasks it opened; any other check (a goal made elsewhere, a task opened elsewhere with
  `--accept`) runs only after `borg hive approve <goal>` there, which prints each command. Until then
  the board lists them and the task is not offered on that node.
- **One goal per id.** Two machines that `init` one id before a sync, or a member's line naming a goal
  it did not make, leave two definitions: the goal stops on every machine, and the board says why.
  Make it again under another id.
- **Rewards.** The verdict line carries what it credits; the author's node credits the lessons shown
  during the claim at its next sync round, once, and only for work whose claim and done it wrote itself
  (no other member's signature on them). Verdicts are shared with their attempt's tag, and a peer's
  verdict counts only for an attempt this node's board judged that way, under the author's session tag
  and for lessons the team's records show it was given.
- **Messages and flags.** A message line that `borg hive ask|tell` could not have written (its id is
  not the hash of its sender, time and text, a field holds whitespace or control characters) is
  dropped. A flag on a hidden oracle's file syncs as `<hidden oracle>`, never its path.
- **Hive sessions are visible to the team.** Board lines name session ids, so a team outcome line's
  session tag can be linked to its hive session. A goal with a hidden
  oracle is scored and credited on the node that made it only; a remote author of such a goal's
  attempt is not credited yet.
- **One budget.** A goal's `spend.jsonl` syncs, each line naming its node, so runs on several machines
  split and stop on one budget: a run's share is what every node's drones left, and every drone stops
  once the goal's total reaches the budget, whoever spent it. Runs started at once each split the whole
  budget (neither has seen the other's spend), so together they can pass the budget by the sessions in
  flight when the total reached it, one per drone, each at most its drone's dollars left, while the
  mesh syncs faster than a session lasts (the daemon dials as soon as a hive log changes, looked at every 25 ms); a machine that lags longer
  lets each of its drones start one more session per sync it missed.
- **Not measured** across real machines yet (arm E is a later paid run). Peers on `borg/mesh/11` or
  earlier do not sync with this version (`borg/mesh/12` carries each goal's beats in the live exchange instead of the board, 11 the ratified constraints, 10 the goal's spend, 9 the
  bundles): upgrade every member.
  The mesh log and `borg doctor` name each member on another version and who has to upgrade.

Not yet: goal signatures (any member's goal syncs, signed by its origin), approving a plan once for
every node (each node's run asks its own engineer), multi-repo goals (`run` takes the repo it is started in), moving a failed
integration's tree between machines, stopping an unmetered session mid-way at its budget, crediting verifiers by whether the
hidden oracle agreed with them, and a model that monitors drones for gaming.

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

A drone on another member's machine is never picked without `--session`. Presence keeps only a repo's
name, so two checkouts of repos with the same name count as one repo; use `--session` there.

**Across machines.** Name a drone on another member's machine with `--session` (as `borg top` lists
it) and `borg no` sends the veto as a signed steering directive (`borg steer <drone> --veto
"<reason>"` does the same). It needs what any directive to another engineer's drone needs: a team
collective and the `admin` or `steer` role. It travels with the next mesh round. The drone's machine
records it once, as if it had been said there: the lessons that session was shown lose credit, the
reason becomes a veto episode that machine's `borg learn` turns into a lesson (and counts
`veto_repeat`), and the drone is told at its next hook, labelled with your name and key. Your machine
counts only the directive it sent; the credit reaches its beliefs through the team's outcomes, so each
machine counts the veto once.

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
   `<store>/constraints.jsonl`, and the mesh syncs that file to the team. A ratified constraint is a
   standing lesson: every session, on every harness, may be asked it like any lesson, and on every
   harness that takes the context it comes early, before any match, at the first prompt (unscoped) or once the work names the
   scope of the lesson it came from, marked as a ratified constraint. Nothing blocks an edit or a
   stop on a constraint yet.

4. **A missed escalation.** When the vetoed work had already merged (the session landed a commit of
   its own that is still on HEAD, or its hive task was verified and integrated) and the queen never
   escalated anything from that session, the veto also counts against the queen's attention (see
   [Answering the queen](#answering-the-queen)).

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
| `borg queen answer <n\|id> <text\|a\|b\|1\|2>` | Answer one; the answer becomes a lesson (a reach question moves its lesson instead; a proposed constraint is ratified with `b`, see [Saying no](#saying-no); a drone's change to another drone's hive contract is let through with `b`, see [Hive](#hive); a hive plan is approved with `a`, see [Running drones](#running-drones)). A letter or number picks an option; one that names no option is refused |
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

**The queen's attention.** Every escalation costs you minutes, so Borg scores how well the queen
spends them. Only the questions the queen chooses to raise count (an external mutation, an irreversible
change, a public API or schema change, a cross-repo decision); proposals you must answer anyway
(constraints, contracts, playbooks, reach, open decisions) do not.

| Event | Counts |
|---|---|
| An escalation you answered by changing course (any option but the one that lets the plan go ahead, or your own words) | +1 (`queen_no`) |
| An escalation you answered "yes, fine" (the plan's own option, or a bare yes) | −1 (`queen_stamp`) |
| A veto of merged work, or a later fix of a committed decision, from a session the queen never escalated | −3, a veto's weight (`queen_missed`) |

The page's queen view shows the reward, the share of answered escalations that ended in a "no", and
each count, for the whole team. The weights are design choices, not tuned. Nothing optimises this
score: it is for you to watch, and for a later, evaluated escalation policy to be judged by.
