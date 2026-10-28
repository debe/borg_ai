# Commands

[← README](../README.md)

## Overview

The commands of borg 0.3.0, grouped as `borg help` shows them; the hive's are under [Hive](#hive), and
evaluation and debugging tools under `borg debug` ([appendix](#borg-debug)).

### Setup

| Command | Shows or does |
|---|---|
| `borg setup [--yes] [--no-vectors] [--harness LIST] [--private DIR...] [--team NAME --paths DIR... [--relay URL\|disabled] \| --join TICKET [--paths DIR...]] [--home DIR]` | Guided setup: collectives (personal, team or company, off paths), the team's mesh, the vector model, wiring, an optional import, then `borg doctor`. Shows one plan and writes nothing until you confirm; `--yes` takes every default. An existing `collectives.json` is only added to; a second run changes nothing ([Quick start](../README.md#quick-start)) |
| `borg install\|uninstall <claude\|omp\|opencode\|codex\|all> [--apply]` | Wire or unwire the hooks or adapter and the `borg` skill; a dry run without `--apply` ([details](configuration.md#what-borg-install-writes)) |
| `borg status` | Each collective's delivery mode and store (lessons, episodes, outcomes, checks), what is wired and whether the `borg` skill is current |
| `borg doctor` | What is broken: config, hooks, adapters, the model CLI, `borg` on `PATH`, a stale skill, the vector model and index, codex hooks it would skip as untrusted, and mesh members on another protocol version |
| `borg collective [DIR]` | Which collective a directory belongs to and whether it may sync |
| `borg vectors [--fetch\|--rebuild] [--store FILE]` | The vector model and index; on by default once the model is fetched ([details](configuration.md#vectors)) |
| `borg completions <bash\|zsh\|fish>` | A shell completion script (below) |

### Learning

| Command | Shows or does |
|---|---|
| `borg learn` | Turn waiting episodes into lessons now (normally at session end). It also counts each learned regret against the lessons asked before its decision, and first records settled hive groups ([Groups](#groups-of-attempts)) and reworks ([Reworks](#reworks)) of this node |
| `borg learn --group <goal>/<task>` | Reflect on one hive group now: what its winning attempts did that the losing ones did not |
| `borg no [--session ID\|DRONE] [--dir DIR] [--json] <reason>` | Say no to a drone's work: the lessons it was shown lose credit, the reason becomes a lesson ([Saying no](#saying-no)) |
| `borg assimilate ...` | Import past Claude Code, omp and opencode sessions, or team documents with `--from-dir` ([details](configuration.md#import-what-you-already-have)) |
| `borg reach [--plan FILE] [--apply] [--jobs N]` | Sort existing lessons into team, repo-only and personal ([details](configuration.md#who-hears-a-lesson)) |
| `borg facets [--apply] [--store FILE] [--jobs N]` | Search facets for lessons |

### Decisions

| Command | Shows or does |
|---|---|
| `borg queen ...` | Open decisions for the engineer ([Answering the queen](#answering-the-queen)) |
| `borg why <path>[:<line>[-<line>]] \| <commit> [--full] [--json]` | The reasoning behind code: blame, borg notes, linked lessons, and both ends of a decision → fix link ("→ fixed in Y" on the decision, "← decided in X" on the fix) |
| `borg notes fetch [<commit>...]` | Reasoning notes and decision → fix links from team members over the mesh: for the named commits, or the last 200 on HEAD |
| `borg regress [--session ID] [--replay A..B] [--any-origin] [--explain] [--json] [--dir DIR]` | The decision → fix links borg would make, as a dry run that writes nothing; `--explain` lists each candidate left out and why ([details](configuration.md#decision--fix-links)) |
| `borg review-fix [--home DIR] [--since YYYY-MM-DD] [--json] [--apply [--only ID,…]]` | Bugs a sub-agent review found and a fix step fixed before any commit, per collective, with a chance-match control; 0 tokens. `--apply` learns the strong ones ([details](configuration.md#review--fix-findings)) |
| `borg trust [--dir DIR]` | Trust per engineer and area, from what followed their lessons |

### Watching

| Command | Shows or does |
|---|---|
| `borg top [--once] [--json] [--dir DIR]` | A live screen, like Unix `top`: the drones of each collective (local and remote), each hive goal with its tasks, plan and nodes, and the newest board events. Quits on `q`, Esc or Ctrl-C. Piped, or with `--once`, it prints the plain text once |
| `borg web [--port PORT] [--dir DIR]` | A live page on 127.0.0.1: knowledge, checks, the queen, learning week by week and the hive ([details](statistics.md)) |

### Team

| Command | Shows or does |
|---|---|
| `borg mesh <command>` | Team sync: `id`, `init`, `invite`, `invites`, `join`, `revoke`, `members`, `peer`, `serve`, `sync` ([details](team-sync.md)) |
| `borg steer <drone\|session\|all> "<text>" \| --pause \| --resume` | A signed instruction to running drones |

**Shell completion:**

```sh
borg completions bash > ~/.local/share/bash-completion/completions/borg
borg completions zsh > "${fpath[1]}/_borg"
borg completions fish > ~/.config/fish/completions/borg.fish
```

**Internal, not in `borg help`:** `borg hook <harness> <event>` is what the harnesses run.
`borg check --session ID [--transcript FILE] --cwd DIR` is live mode's check of the work so far, started
detached after a state-changing tool call ([Delivery modes](delivery.md#delivery-modes)).
`borg learn --session ID`, `borg notes attach [<commit>]`, `borg hive wait` and
`borg hive join --bind --session ID` run from hooks and adapters at session end or for drones.

## Hive

Many drones work on one goal. The goal has an oracle (shell checks) that decides when a task is done.
Drones claim tasks from a shared board, publish contracts for each other, and finish a task only when
its checks pass; a different drone verifies every finished task. Everything is in files under
`<store>/hive/<goal>/`, and in a collective shared over the mesh the goal, board, contracts, messages,
flags and spend sync to every member as signed lines ([Across machines](#across-machines)). How the
board stays the same on every node is the [protocol](design/hive.md#protocol).

```console
$ borg hive init parser --spec "Ship the parser" --oracle "unit=cargo test -q" --budget 20 \
    --task "t1|work|Add the parser|src/parse/*.rs" --task "t2|work|Use it|src/main.rs|t1"
$ borg hive board
```

| Command | Effect |
|---|---|
| `borg hive init <goal> --spec TEXT\|--spec-file F --oracle NAME=CMD... --budget USD [--timeout S] [--repo NAME[@NODE]]... [--task "ID\|role\|title\|files,..\|deps,..\|NAME=CMD"]...` | A goal, its oracle and its first tasks. Without `--oracle` it refuses: done means the oracle passes |
| `borg hive ratify <goal> [--budget USD]` | The engineer approves the goal's plan from a terminal; `--budget` raises the budget once its spend stopped new claims. Refused inside a harness session ([The planning session](#the-planning-session)) |
| `borg hive board [<goal>] [--json]` | The tasks by status, with holders, age, rework and dependencies; in your terminal, then the drones' messages to the planner not read on this machine, marked read |
| `borg hive join [<goal>]` · `borg hive leave` | Inside a session: it becomes an interactive drone of the goal (or of every open goal of this repo) in a worktree of its own, or stops being one ([Interactive drones](#interactive-drones)) |
| `borg hive join <goal> --here [--drone NAME]` · `borg hive leave --here` | Every new session in this repo (or worktree) joins the goal, or no longer does |
| `borg hive open <goal> --id ID --role work\|plan\|verify\|integrate --title T [--repo R] [--files a,b] [--deps x,y] [--accept NAME=CMD]... [--again] [--group K]` | An engineer opens a task; `--again` reopens a blocked one, and `--title`, `--files`, `--deps` or `--accept` given with it replace those parts (a wrong check, say); `--group K` opens K attempts ([Groups](#groups-of-attempts)) |
| `borg hive integrate <goal>` | Once every task is verified: merge the accepted attempts into `refs/heads/hive/<goal>/integrated` and run the oracle and every task's checks on it. A failure opens a fix task. Needs git 2.40+. Refused to a drone and inside a harness session |
| `borg hive approve <goal>` | Run the checks another node wrote (a synced goal's oracle, a task opened elsewhere with `--accept`) on this machine; prints each command. Refused to a drone and inside a harness session |
| `borg hive claim [TASK]` · `progress "<note>"` · `release` · `block "<why>"` | A drone claims a task (exit 3 when another drone won it), reports on it, gives it back or blocks it |
| `borg hive done [--note N]` | Run the task's checks with their own timeouts; on a pass publish done and open its verify task |
| `borg hive verify --pass\|--reject "<why>"` | The verdict on a verify task (or end the turn with `VERDICT: pass` or `VERDICT: reject <why>`); a rejection reopens the work with the note |
| `borg hive contract "<text>" --scope a,b [--supersedes ID]` | Publish a contract scoped to files (globs), `id:NAME` or `repo:NAME`, shown to drones whose edits touch it |
| `borg hive ask "<text>" [--to TASK\|DRONE\|SESSION\|planner]` · `tell "<text>" [--to …] [--re ID]` | A message to the goal's drones, or with `--to planner` to the sessions that planned it, shown once at each recipient's next prompt |
| `borg hive ask\|tell "<text>" --goal G [--to …] [--re ID]` | The planning session (or your terminal) sends into goal G without joining it, and is G's planner from then on: its prompts show the drones' messages to the planner. `borg hive leave` there ends its planning |

Drone commands take `--session ID` (the hook's context prints the exact command) and `--dir DIR`.

- **Dependencies.** A task is claimable once every dep is verified. A claim starts from the goal's
  origin (the commit `borg hive init` ran on; in another repo, its HEAD when the goal's first claim
  there was made) with each dep's verified change applied, so the work's diff starts after its deps.
  Commits the engineer makes after that reach drone trees only through a task. A drone's own worktree is reset to that start when the claim wins; what
  it held before is kept in a pinned commit that `borg top`, the Hive tab and the drone are told. A
  rework gets the drone's previous attempt back on top, uncommitted. Any other checkout claims only
  where its tree is the start already (the refusal names the commit to check out). Deps whose changes
  conflict block the task with the paths, once.
- **Claims are leases.** Hooks renew a claim every 10 minutes; it expires after 30 without one. Of two
  claims on one task, the lowest (stamp, hash) wins on every node; the loser hears it at its next hook,
  its edits are refused and its worktree is set back, the lost work kept in a snapshot commit.
- **In the harness.** At a prompt the drone sees its claim, the board's changes and the contracts in
  its scope. At the stop the task's checks run: while they fail the stop blocks (at most 3 times, then
  the task is blocked); when they pass, done is published and the next task handed over. A suite slower
  than the stop hook needs `borg hive done`.

Nothing changes for a session that never joined a goal.

### The planning session

Tell any session what the drones should build. Its borg skill turns the request into a goal: small,
independent tasks, a contract task before the tasks that use its interface, and a check for every task.
`borg hive init` prints the plan, one card per task (intent, files, checks, risk).

- **You approve at your own prompt** in that session: `yes`, or `no, because <why>`. A no is a veto
  that borg learns from, and the session revises the plan with you.
- **No drone takes a task of a plan you have not approved.** The approval is a `ratified` line of the
  board, so every node reads it. Once approved, the session tells you how many drones are waiting.
- **The model cannot approve.** `borg queen answer`, `borg hive ratify`, `approve`, `integrate` and
  `borg debug hive score` refuse inside a harness session. A goal opened from your own terminal is
  approved there with `borg hive ratify <goal>`.
- **The drones report back.** A drone that finds the plan wrong (a check that tests nothing, a missing
  task) tells the planner: `borg hive tell "<what>" --to planner`. The message, and each drone's
  message to everyone, shows once at the planning session's next prompt, however long it was idle, with
  the command that answers it; messages to the planner come before a backlog of broadcasts. The
  session stays the goal's planner after the approval, and sends to the drones with `borg hive tell
  "<text>"` without joining. `borg top` and the Hive tab show each planning session of this machine
  with its unread messages: a harness that takes no prompt context leaves them unread there.
- **Messages to the planner reach planning sessions only.** `borg hive tell` from your terminal sends
  as you, the engineer, never as the repo's one drone, and a drone's answer to it goes to the planner.
  Your terminal reads no prompt, so a goal opened and approved from a terminal has no session reading
  those messages: `borg top` and the Hive tab count them on the goal's home node (where it was first
  opened) and on any machine with a planning session (`to the planner: N message(s), M unread here`).
  `borg hive board <goal>` in your terminal lists the ones not read on this machine, each with the
  command that answers it, and marks them read there.
- **Subagents and workflows** may help inside a task under the drone's claim; they never claim or finish.
- **Budget.** Each claim of a Claude Code drone adds its transcript's metered spend to `spend.jsonl`;
  omp, opencode and codex report no usage (`unmetered`). At the budget no drone claims, and you are
  asked once to raise it, at the planning session's prompt or with `borg hive ratify <goal> --budget USD`.

### Interactive drones

A session becomes a drone when its model runs `borg hive join` (ask it to "join the hive"). It works in
its own worktree (`.worktrees/hive-i-<drone>`) and, with nothing open, ends each turn on
`Drone idle: waiting for hive work, which starts here by itself when a task opens.` The next task starts
a turn on its own; waiting costs no tokens. It stays a drone until you ask it to leave.

| Harness | How the drone waits and is woken |
|---|---|
| Claude Code | Its stop asks it to run `borg hive wait` in the background; the wait's output starts the next turn |
| omp | The adapter keeps a waiter (`borg hive wait --stream`) for the session's life and wakes it with `pi.sendMessage` |
| opencode 1.x / 2.x | The plugin binds the session (`borg hive join --bind`) and wakes it with `session.promptAsync` (1.x) or a queued `session.prompt` (2.x) once idle |
| codex | Its stop starts `borg hive wait --queue`, which wakes the thread with `codex queue` (codex 0.149+). In the default sandbox the join must run outside it |

- No task is handed into your own turn. A wake no turn takes up within 60 s is released, and the
  drone claims nothing until its session speaks again.
- A stop that finishes a task says the result first (`t1 done: its checks passed ...`), then hands over
  the next task or the idle line. The done's note is the stop's own summary of checks, files and commits.
- Ending the session (`/exit`, a closed terminal, a crash) releases its claim and leaves the hive;
  `/clear` and compact keep the same drone.
- Reinstalling borg (`cargo install`) needs no session restart: a waiter whose borg was replaced
  claims nothing more and runs the new borg in the same process within half a second, or after a
  failed `codex queue`'s retry; a borg that cannot run leaves it claiming nothing and beating for no
  one until borg is installed again (peers then take the node's claims over unless a daemon serves
  it). `borg mesh serve` restarts in place on a reinstalled borg-mesh (same process); one that
  cannot run, or one built before it could restart so (a downgrade), stops it with a message and
  exit code 1.
- **The node's worker.** `borg mesh serve` (or, without a daemon, one drone's waiter) beats for the
  node, keeps its roster of drones, reaps drones whose session is gone, says what is stuck and runs
  `borg hive integrate` once every task is verified. A restart ends no claim.
- **A repo in no collective** refuses `init` and `join` and prints the `borg setup` that adds it.
- **The first wake after 5 idle minutes** reads the conversation again without Claude Code's prompt
  cache (about 42,000 tokens in the live probe); waiting itself is free.
- **Watching.** `borg top` and the Hive tab of `borg web` show per goal its tasks over time, spend
  against the budget, each node and who does its work, each drone's state (`working`, `busy`,
  `waiting`, `waiting (old borg, restarting)`, `not waiting`, `left`, `gone`) with its wake latency, stuck tasks and integrations.

### Guardrails and the hidden oracle

A drone can pass a visible check by changing it. The hive lets that happen, records it and makes it
useless ([design](design/hive.md#learning)):

- **Snapshots.** A claim records its start, and its done a commit of the drone's tree; the verdict
  diffs the two, after it recomputed the start from the deps the claim names.
- **Hacked.** The verdict is `rejected` with `hacked`, whatever the verifier said, when the diff
  changes a file the goal's oracle runs; or, outside the task's own `--files`, changes CI config or a
  test runner's config, deletes or renames a test, or weakens one: a removed line no added line matches
  in shape (names, strings and numbers as holes), fewer test declarations, an added skip or focus
  marker. Tests may grow but never shrink: added tests and changed names and values pass. The work
  reopens with each path to restore, e.g. `tests/test_parse.py (line 4 removed or reshaped: ...)`.
- **Flags.** The paths a drone writes to tests, CI or oracle files, and reads of the hidden oracle, are
  logged in `flags.jsonl` and listed for the verifier: a flag, never a refusal.
- **Rewards.** Every lesson shown during the claim is credited with the verdict (`verified`, `rejected`
  or `hacked`); a hacked task weighs on a lesson as much as a veto.
- **The author never judges its own work.**
- **Hidden oracle.** `borg debug hive init ... --hidden-dir DIR --hidden CMD` keeps held-out checks
  outside the repo. After a verdict they run on the done's snapshot, and their score decides the
  credit; their output is never shown to a drone. `borg debug hive score <goal>` scores any time.

`BORG_HIVE_CREDIT=off` records verdicts without crediting lessons.

### Groups of attempts

`borg hive open <goal> --id ID --group K` opens K attempts `ID.g1`..`ID.gK` of one work task. Each is an
ordinary task, and each is another drone's: a drone that held or verified one member gets no other, and
none is judged by a rival. The integration takes the group's best attempt: the one the hidden oracle
passed, else the one with the fewest rejections, then the earliest verified.

Once every member is verified, the group is settled. A settled group with a winner and a loser becomes
one episode, and `borg learn` asks the model once what the winners did that the losers did not and
writes it as a lesson; one node learns each group. `borg learn --group <goal>/<task>` reflects on one
now. `BORG_GROUP_REFLECT=off` or `BORG_LEARN=off` stops it. A task cannot depend on a group.

### Reworks

A work task the verifier rejected on its merits and a later attempt passed is the hive's own lesson
source. `borg learn` records it once as an episode (every rejection's note, each attempt's change, what
the accepted rework changed) and asks the model what mistake the first attempt made, writing a lesson
that prevents it. A hacked or unchecked rejection does not count; with a hidden oracle, the episode waits
for its score. `BORG_REWORK_REFLECT=off` or `BORG_LEARN=off` stops it.

### Playbooks

A playbook is the team's procedure for a kind of task: its shape ("fix the parser") and a short list of
steps. `playbooks.jsonl` holds edits (add, change or remove one step), never whole playbooks, so it syncs
like lessons and every reader folds the same playbooks.

```console
$ borg queen
app (1)
  1  Add to the team's playbook: "Run the parser's tests before calling it done"?
$ borg queen answer 1 b
```

After a hive attempt wins, or a plain session ends clean (it landed commits, its last test run passed,
and you neither corrected nor vetoed it), `borg learn` asks the model once for at most one edit. The edit
is never applied by learning: it is queued as a queen question, and only your `promote` answer appends
it. A drone sees the playbooks its task's title and files name, a session those its prompt names; each
version shown is credited like a lesson. `BORG_PLAYBOOKS=off` shows and proposes nothing.

### Across machines

In a collective shared over the mesh, every goal's `goal.jsonl`, `tasks.jsonl`, `contracts.jsonl`,
`messages.jsonl`, `flags.jsonl`, `bundles.jsonl` and `spend.jsonl` sync like lessons: signed by their
writer, quarantined when the writer is revoked, caught up after time offline. The rules that make every
node fold the same board (claim races, causal clocks, failure detection, final claims, termination) are
the [protocol](design/hive.md#protocol). What a member sees of it:

- **Claims** are provisional until a sync round; the earlier one wins on both machines, and the loser
  hears it at its next hook. Keep the machines' clocks well within the 30-minute lease.
- **Which task a drone takes:** its own repo first, then verify before integrate before work; machines
  start on different tasks (rendezvous hashing), and `borg top` names a holder's node.
- **Commits travel as bundles** of each attempt's two snapshots (at most 1 MiB). A node without an
  attempt's commits is not offered its verify task and says which node holds the work.
- **Secrets stay home.** Titles, notes, contracts, messages and the synced spec are redacted; check
  commands are kept, so keep secrets in the environment.
- **Checks from another machine** run only after `borg hive approve <goal>` there.
- **One goal per id.** Two definitions of one id stop the goal everywhere.
- **One budget.** Every drone stops once the goal's total, over every node, reaches its budget.
- Every member must run the same mesh protocol version; `borg doctor` names who has to upgrade.

Not yet: goal signatures, multi-repo goals, moving a failed integration's tree between machines, and
crediting verifiers by whether the hidden oracle agreed with them.

## Saying no

A veto is the engineer's "no" to what a drone is doing, with the reason: the strongest negative signal
borg learns from, at 0 tokens.

```console
$ borg no never look merges up in the commit log, use the review API
vetoed One of Two (3f9c2a1b): 2 shown lesson(s) lose credit (L17, L42); the reason becomes a lesson when borg learn runs
to stop it now: borg steer 3f9c2a1b --pause
```

**Which drone.** Without `--session`, the one local drone working in the current repo (or `--dir`), or,
with none running, the most recently seen one. With several running it lists them; name one with
`--session` (as `borg top` shows it, its number, a session id or a prefix of 4+ characters). A drone on
another member's machine is named with `--session`, and the veto travels as a signed steering directive
(it needs the `admin` or `steer` role); that machine records it once.

**Inside the session**, a prompt that starts with `borg no` is the same veto. The drone stops the vetoed
approach and may ask one clarifying question. `BORG_VETO=off` turns this off; the command still works.

What it does:

1. **Credit.** Every lesson shown in the session so far gets a `vetoed` outcome, weighed three times
   (a correction weighs two), once per lesson and session.
2. **A lesson.** The reason becomes a veto episode that `borg learn` turns into a lesson; the same no
   said twice counts as `veto_repeat`.
3. **A proposed constraint.** A reason that names an invariant is queued in `borg queen`; only your
   answer ratifies it into `<store>/constraints.jsonl`, which syncs to the team and is delivered early
   to every session.
4. **A missed escalation.** A veto of merged work from a session the queen never escalated counts
   against the queen's attention ([below](#answering-the-queen)).

With `BORG_LEARN=off` the command records nothing and says so.

## Answering the queen

The queen's queue holds decisions no lesson answers: open decisions found in team documents
(`borg assimilate --from-dir`), proposed constraints, playbook edits, contract changes and, with
`BORG_QUEEN=on`, questions raised at a turn end. Similar questions are queued once.

```console
$ borg queen
handbook (1)
  1  Should spikes be capped at 3 story points, or estimated like any other story?
     a) cap spikes at 3 points (default) · b) estimate spikes like any other story
$ borg queen answer 1 b
```

| Command | Effect |
|---|---|
| `borg queen [--json] [--repo NAME]` | Open questions, numbered and grouped by repo (`--json`: an array, for drones and scripts) |
| `borg queen show <n\|id>` | One question with its evidence |
| `borg queen ask` | Answer them one by one (a terminal) |
| `borg queen answer <n\|id> <text\|a\|b\|1\|2>` | Answer one; the answer becomes a lesson. A letter or number picks an option |
| `borg queen skip <n\|id>` | Not delivered again for 3 days |
| `borg queen dismiss <n\|id> [reason]` | Drop it without a lesson |
| `borg queen dedupe [--apply]` | Dismiss later near-duplicates (a dry run first) |

Each takes `--dir DIR` or `--collective NAME`.

**In the harness.** At the first prompt of a session in a repo with an open question, borg hands that
one question to the drone, which asks you (Claude Code in its question dialog, omp and opencode at the
start of the reply) and records your answer with `borg queen answer`. One question per session; a
skipped one waits 3 days. `BORG_QUEEN_ASK=off` or `BORG_LEARN=off` turns this off.

**The queen's attention.** Only the questions the queen chooses to raise count (an external mutation,
an irreversible change, a public API or schema change, a cross-repo decision):

| Event | Counts |
|---|---|
| An escalation you answered by changing course | +1 (`queen_no`) |
| An escalation you answered "yes, fine" | −1 (`queen_stamp`) |
| A veto of merged work, or a later fix of a decision, from a session the queen never escalated | −3 (`queen_missed`) |

The page's queen view shows the score for the team. The weights are design choices, not tuned, and
nothing optimises the score yet.

## borg debug

Evaluation and debugging tools, out of the main help (`borg debug help`):

| Command | Shows or does |
|---|---|
| `borg debug recall <lessons.jsonl> [before_ms]` | Rank lessons for text on stdin |
| `borg debug resolve <lessons.jsonl>` | The word rule's changed/ignored for `{asked, before, edits, final}` on stdin |
| `borg debug prefilter <lessons.jsonl> [before_ms]` | The free gate's score for a turn on stdin |
| `borg debug memory <lessons.jsonl> [--session ID]` | Each lesson's activation, usefulness, weight and long-term or dormant state |
| `borg debug expert <repo> --at <commit> --q <text>` | At most 3 cited claims from an expert repo |
| `borg debug vectors --query [--store LESSONS]` | The vector shortlist for a context on stdin |
| `borg debug ope [--dir DIR \| --file FILE] [--json]` | Off-policy estimates of the belief's hand-set weights; reports, never changes them |
| `borg debug hive run <goal> --drones N [--harness H] [--model M] [--base REF] [--iter-timeout S] [--dry-run] [--yes]` | Headless drones for evals (below) |
| `borg debug hive score <goal>` | The hidden oracle scores the judged tasks not scored yet |
| `borg debug hive init <goal> ... --hidden-dir DIR --hidden CMD [--hidden-timeout S]` | A goal with held-out checks outside the repo |

**Headless drones.** `borg debug hive run` makes one worktree per drone (`.worktrees/hive-<goal>-<n>`)
and runs headless sessions of the harness one after another with `BORG_HIVE=<goal>`, each drone with an
equal share of what is left of the goal's budget, until the board is finished and integrated. It asks
for the plan's approval first, at its terminal or through the queen; `--yes` approves it on the command
line, for evals. Drones run with every permission inside their worktree. It is refused while an
interactive drone serves the goal; engineers use [interactive drones](#interactive-drones).
