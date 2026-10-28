# Hive: many drones, one goal

A hive is several borg drones, on one machine or several, working one goal together: a board of small
tasks, each gated by its own check, verified by a different drone and merged once at the end. There is
no leader and no central server. Every piece of shared state is a signed, append-only log that the mesh
syncs like lessons, and every node folds the same logs to the same board.

## Status

- **Built and accepted on real repos:** goals, the board, contracts, messages, verification,
  integration, plan approval and the budget, on one machine and across nodes (commits travel as
  bundles beside the board).
- **Interactive drones (phase g):** sessions the engineer opens on Claude Code, omp, opencode 1.x and
  2.x, or codex join from inside, wait at no token cost and are woken for work. Accepted with real
  sessions of all four harnesses on two nodes.
- **Learning:** lessons, oracle rewards, regret, group reflection, rework lessons, playbooks and
  guardrails are built; the genome archive is parked. Learning stays on and is judged in production
  through monitoring.
- **Not measured:** whether a hive beats one session that uses subagents. That needs a workload hard
  enough to tell them apart.

`borg debug hive run <goal> --drones N` runs headless drones in worktrees; it exists for the evals.
Engineers use interactive drones.

## Why a hive

1. **Context ownership.** Each drone keeps a long-lived context: one repo, one machine with its
   toolchain and credentials, and its engineer's lessons. A subagent borrows its parent's machine and
   returns a short summary.
2. **Contracts, not transcripts.** A decision other drones must respect (an API shape, a schema, a
   name) is published as a contract line and shown to the drones whose edits touch its scope.
3. **Coordination through a signed log.** Claims are leases on an append-only board: no election, no
   consensus round, and it survives partitions and NAT because the mesh already syncs lines this way.
4. **Done means the check passed.** A drone holding a task cannot finish while its check fails, and a
   different drone verifies every finished task in a fresh context.
5. **Decorrelated errors.** Drones may run different harnesses and models and carry different
   engineers' memories, which is what makes verification and voting pay off.
6. **The collective keeps what it learns.** Lessons, contracts and decision links outlive the run.

**The engineer's role.** Drones write code faster than people read it, so the engineer chooses the
goal, owns the invariants, reviews intent rather than lines, and says no. A "no" is the strongest
reward the hive gets: it becomes a lesson, and the lessons shown before it lose credit.

## Primitives

Each is a log under `<store>/hive/<goal>/`, synced over the mesh (signed origin, union merge, a revoked
key's lines quarantined). Synced text is redacted when written; ids, commits, scopes and check commands
are kept.

- **Goal** (`goal.json`, synced as a projection in `goal.jsonl`): the spec, the oracle checks, the
  budget, the repos in scope and the origin commit of the repo `init` ran in. The first claim in any
  other repo (or in a goal made outside git) records that repo's main checkout HEAD as its origin on
  the board (an empty root commit, built alike on every node, for a repo with no commit); the first
  such line in the board's order holds on every node. A node whose clone lacks the origin, after the
  deps' bundles came (they carry an origin no remote holds), claims no work there and shows why as an
  alert in `borg top` and the Hive tab. A hidden oracle stays on its node. Two definitions of one id stop the
  goal everywhere until one is left. A check written by another node runs only after the engineer's
  `borg hive approve`.
- **Board** (`tasks.jsonl`): `open`, `claim`, `heartbeat`, `progress`, `done`, `verified`, `rejected`,
  `released`, `blocked`. A task has a role (`work`, `verify`, `integrate`), files in scope, deps and
  acceptance checks. A claim needs every dep verified. It starts from the goal's origin for its repo
  (see Goal) plus each dep's accepted change: one commit, computed from the board and git objects
  alone, never from the drone's tree, and named on the claim with the attempt of every dep it applied. A
  drone's own worktree is reset to it once the claim won, the tree before kept in a pinned commit
  named on the board; a rework gets its previous attempt back on top. Another checkout claims only
  where its tree is the start already. Deps whose changes conflict block the task once, for the
  engineer. A task whose start a session's tree cannot take (a checkout off it) is passed over for
  that session; one whose start git did not compute in time stays offered and is tried again. So no work a blocked, rejected or lost attempt left behind reaches a base, and every line
  of an accepted head comes from the origin or a verified diff.
- **Scope asks and amends.** A definition no change could pass is refused when it is written (`init`,
  `open`, `open --again`): a directory in `files` is its tree (`dir/**`), a file the goal's check
  reads is refused, and each check runs once on the tree, bounded at 5 s; one no change could make
  pass is refused (its command word not installed, or a usage error from a system tool no file of
  the tree configures, such as git, with no script of the repo named), and one still running is said
  unprobed. A missing script, a flag the task adds to the repo's own tool or registers with a test
  runner, or a file it commits fails today and opens. A drone whose task needs more files or another check asks with `borg hive scope`: a
  message to the planner carrying the exact `borg hive open --again` line, alerted in `borg top` and
  the Hive tab until answered. Only the engineer amends, from a terminal: title, files and checks of
  an open, held or blocked task, the claim kept; deps only while no one holds it. The engineer says no
  by replying to the ask from a terminal (`borg hive tell --re`); a planning session's reply answers
  nothing, and its prompt shows an ask with no reply hint to it. The holder hears either at its next hook, and its
  waiter wakes it if it was idle.
- **Verification:** every `done` opens a verify task its author may not take; a rejection reopens the
  work with the verifier's note. The verdict recomputes the work's start from the attempts its claim
  names, fetching their commits from their bundles on another node; a base that is not it is an
  unchecked reject. Changes to tests outside the task, CI config or oracle files are
  flagged and judged as hacked, against the files its verify task carries (the done's definition).
  A `done` (or a stop) runs the same judge on its own diff first and is refused with each path and the
  `borg hive scope` line that would admit it.
- **Integration:** once every task is verified, an integrate task merges the accepted attempts (each
  claim's base to its done's head, in dependency order, with `git merge-tree`, onto the goal's origin) into
  `refs/heads/hive/<goal>/integrated` and runs the oracle and every task's checks on the result. A
  failure opens a fix task from the combined tree. No model session ever integrates.
- **Contracts** (`contracts.jsonl`) and **messages** (`messages.jsonl`, `borg hive ask` and `tell`),
  shown once per session at a prompt or an edit in scope. A message goes to everyone, a task, a drone,
  a session or the planner (`@planner`): every session with a planner record of the goal, the
  session that opened its plan or sent into it without joining. '@' is in no task, session or drone
  name, so no drone of any version is shown a message to the planner. A planner is shown each message
  for it once, however old, and only when the text went into its context, those to it before the
  broadcasts; it never claims or waits. Monitoring counts the messages to the planner from the synced
  log alone on the goal's home node (its first open's) and on a node with a planner session, so a goal
  opened from a terminal, which no session plans, still shows its drones' reports; the engineer's
  terminal reads them with `borg hive board`, which marks them read on that node only (`planner-read`,
  never synced); a scope ask there offers its amend, a reply without `--re` and the explicit no, the
  only one that replies to it, since any terminal reply to the ask is the engineer's no. A planner that is a drone hears only its own goal, as a drone; `borg hive leave` in a
  session that only plans removes its planner record.
- **Commits travel** (`bundles.jsonl`): each attempt's two snapshots as a git bundle line, so a
  verifier or integrator on another node gets the work while its author is offline. A node takes a
  bundle only if it holds exactly that attempt's commits.
- **Spend** (`spend.jsonl`): one line per iteration or interactive claim; every node stops claiming on
  the goal's total, whoever spent it.

## Protocol

The board must come out the same on every node, whatever order lines arrive in and whatever the
clocks say. Each property below holds with every timer at zero or infinity; timers only decide how
fast, never whether.

- **S1, one board.** The fold is a pure function of the set of lines: any order, any duplicates, the
  same board. Claim races are won by the lowest `(stamp, hash)`, so every node picks the same winner
  without talking.
- **Same-node exclusion.** A claim is a check-and-append under the board file's lock: while a holder's
  lease is live, no other session of its node writes a claim of that task.
- **Causal clocks.** Stamps are hybrid: strictly rising per node and above every line the node holds,
  so a line written after another was read sorts after it everywhere. Leases, liveness and grace
  periods are timed on the observer's clock from when it first held a line, so a peer's clock offset
  moves none of them.
- **Membership and failure detection.** Nodes join a goal and beat while a drone of theirs takes part.
  A node silent for `SUSPECT_MS` on the observer's clock loses its claims to a peer; a slow but beating
  node keeps them. A false suspicion costs at most a duplicate attempt, never a lost task. Beats travel
  in the mesh's live exchange and are no board lines, so an idle node adds nothing to the synced logs.
- **Final claims.** Each node's beat carries watermarks: the newest line of every other node it holds.
  A claim starts its work only once every other participant's watermark is past its stamp, so no
  earlier rival can still arrive. Without a fault, no work is done twice.
- **S2', monotone results.** A done, a verdict, a verify task or an integration that took effect in
  one fold is in every later fold, under every arrival order.
- **Integration once per epoch.** One integration per integrate task in every fold, and an
  integrator's verdict is written only while the board under the lock still names it.
- **Termination by detection.** A drone never ends because things look quiet. A goal is finished
  when every task is verified and an integration passed after the last verification, and a node ends
  on it only once every speaking participant said finished on the same board. A task no node may take
  is said stuck, not waited on forever.
- **Data plane.** A node offers no task whose commits it lacks, waits while they may still come, and
  says stuck once no line can bring them.
- **Identities are (node, name).** A session or drone name says who only together with its node.
- **S-def, definitions are the engineer's.** A task's title, files and checks change only by an
  engineer's open line: a session's open of an existing id changes nothing, a held task keeps its
  claim, and a done or verified task keeps the definition its done was made under. Only a line with
  the `amended:` note (what `open --again` writes) amends an open or held task; an engineer's open
  without it, as a borg before 0.3.0 wrote `--again`, reopens a blocked task with its definition as
  the fold did then, so old logs and older peers fold as before. `open --again`
  refuses a drone and any harness session, the same convention gate as `ratify` (every open line is
  written as the engineer's, so the fold cannot tell who ran it).
- **S-judge, one verdict.** A done is judged and integrated against the definition its verify task
  carries, written in the done's batch, so an amend that reached two nodes in different orders gives
  them the same verdict.
- **S-done.** No done is written whose readable diff the verifier's rule flags under the definition
  the drone holds; an unreadable diff is left to the verifier. An unanswered scope ask holds no lock:
  only a stop held up by the precheck alone, on paths the asked files would all admit, waits on it
  (no attempt counted); every other stop runs
  its checks, so a holder that drops the change finishes, and one that fails a check blocks after 3
  stops. An amend applies whoever holds the task, so a lapsed lease loses nothing. After the answer
  (an amend, or the engineer's reply to the ask) the precheck counts as a failing check, so the claim ends within
  3 stops; liveness with the change kept needs the engineer's answer.

## Interactive drones

- **Join.** The engineer keeps `borg mesh serve` running and opens sessions; a session asked to join
  runs `borg hive join` from inside itself (its harness gives the command the session id) and becomes
  a drone in its own worktree. A session that never joins answers exactly as before.
- **Plan.** One session opens the goal through the borg skill and shows the plan. The engineer
  answers at that session's prompt: `yes`, or `no, because ...` and it revises. The approval is an
  engineer's `ratified` board line; every claim path checks it, and the model can never approve.
  `borg hive ratify <goal>` approves from a terminal. The planning session stays the goal's planner:
  drones report plan problems to it (`tell --to planner`), and since its model reads their text,
  every command that is the engineer's consent (`ratify`, `approve`, `integrate`, `score`) refuses
  inside any harness session.
- **Wait and wake.** A drone with nothing in hand waits at no token cost and is woken by its
  harness's own path: Claude Code by a background `borg hive wait` whose output starts the next turn;
  omp by `sendMessage` from its adapter; opencode by `promptAsync` (1.x) or a queued `prompt` (2.x);
  codex by `codex queue`. No task is handed into the engineer's own turn. A wake no turn takes up
  within a minute is released and the drone claims nothing until its session speaks again.
- **A reinstall.** A waiter lives for its session, so a reinstall of borg would leave it on the old
  code. A waiter whose borg was replaced claims nothing more, gives up the node's work to the daemon
  or another waiter, and re-executes the new borg in place:
  same process, output and lock, so no second waiter starts, no session restarts and no stop signal is
  lost. A borg that cannot run leaves it waiting, claiming nothing and beating for no one, until
  borg is installed again: reaps and integrations wait for a daemon or a fresh install, and with
  neither on the node its peers come to suspect it and take its drones' claims over. The mesh
  daemon does the same for a reinstall of borg-mesh: its worker stops at once, it stops serving and
  re-executes the new borg-mesh in the same process; a stop sent meanwhile ends the new one. A
  borg-mesh that would not take that over (one built before it could, as a downgrade installs it)
  is never run so: the daemon exits 1 with a message.
- **The node's worker.** One process per node, the mesh daemon (or else one drone's waiter), beats,
  writes finished and stuck lines, reaps drones whose session ended and runs integrations, while every
  drone idles. It never writes a join or leave of its own, so a restart ends no claim.
- **Spend.** A Claude Code drone's claims are metered from its transcripts; other harnesses' claims
  are recorded as unmetered. At the budget no drone claims and the engineer is asked once to raise it.
- **Leave.** A session that ends, or is told to leave, stops its waiter, releases its claim and leaves.
- **Watching.** `borg top` (a live screen) and the Hive tab of `borg web` show per goal its tasks over
  time, the plan, spend against the budget, each node and drone with its state and wake latency, stuck
  tasks, declines and integrations.

Interactive drones write the same board lines as headless ones, so every Protocol property holds for
them unchanged.

## Learning

The hive is a distributed actor-learner: drones are the actors, the mesh is the replay buffer, and
what is learned (lessons, playbooks) is the shared state. The model's weights never change.

- **Rewards.** The verifier's verdict and the oracle credit every lesson shown during the task; a later
  regress link reaches back to the lessons active when the decision was made; the engineer's "no" and
  queen answers count most.
- **Group reflection.** When a task runs as several attempts, a reflection compares winners and losers
  and writes what the winners did differently as a lesson or a playbook edit.
- **Rework lessons.** A rejected attempt that is later verified teaches what the fix changed.
- **Playbooks.** Procedures per task shape, edited in small steps after verified work, never
  rewritten.
- **Guardrails.** Hidden oracles outside every worktree; flagged edits to tests, CI and oracle files;
  tests may grow but never shrink; a hacked attempt counts as failed and costs its lessons credit.

## Proof

- **Zero-token tests** for every property: simulated drones against the real hooks and binary, two
  mesh nodes over loopback with separate object stores, scenario tests that replay arrival orders and
  clock skews, and CLI tests of each harness's join, wait, wake and leave.
- **Acceptance runs** with real models: a pilot on two real workloads (every task verified, no
  conflicts, nothing hacked), the cross-node arm with two nodes after the protocol rework, and
  interactive drones of all four harnesses on two nodes (one goal, every task verified and integrated
  once, no model call while drones waited, wake-ups in 0.1 to 4.6 s).
- **Pilot probe,** one run per arm, before the acceptance run's bug fixes: on the cross-repo workload
  one session, one session with subagents and best-of-N each solved 5 of 5, the hive 2 of 5 at about
  4× one session's cost ($0.80 against $0.19). The bugs it found were fixed, and the hive then passed
  that workload 5 of 5 in all 3 acceptance seeds. No fair comparison has run since.
- **Run, not decided:** the learning comparison (H5: 50 of 90 runs, no decision; see
  [evidence](../evidence.md#experiments)). **Not run:** the hive against one session with subagents.

## Known limits

- A task the engineer vetoed cannot be withdrawn yet, so a goal holding one never finishes.
- Only Claude Code's spend is metered; the model price table is kept by hand.
- Claims are provisional until a sync round; clocks must stay well within a lease of each other.
- Every member stores every goal's logs and bundles; compaction is not built.
- A secret inside a check command still syncs: it is code, and changing it would change what runs.
- A daemon restarts only on a new borg-mesh: a reinstall of borg alone leaves it on the borg code it
  was built with, so reinstall both together.
- A node on an older borg folds the board with that borg's rules (an `open --again`, for one) until it
  is upgraded: re-execution fixes a node's own stale code, not a version skew between nodes.
