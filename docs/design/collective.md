# Hive: many drones, one goal

Status: a local, file-backed spike (steps 1–2), accepted on two company workloads on the work machine (step 3): it works without serious bugs. Whether it beats one session with subagents is not measured. No mesh sync yet.

## Question

Can 3–9 drones connected by borg, possibly on several machines, solve a problem better than one session that uses subagents? And how do we prove the answer either way?

What we've seen so far: work that spans several repos goes better with one Claude Code session per repo, each using its own subagents, than with one session spanning all the repos.

## What is known

| Source | Finding | Takeaway for borg |
|---|---|---|
| Anthropic, [multi-agent research system](https://www.anthropic.com/engineering/multi-agent-research-system) (Jun 2025) | An Opus 4 lead with Sonnet 4 subagents scored +90.2% over a single Opus 4. It used about 15× the tokens of chat, and token use alone explains about 80% of the variance on BrowseComp. Coding and tightly coupled tasks fit poorly. Observed failures: spawning too many agents, duplicated work, status-message noise. | Most of the gain comes from spending more tokens. Every comparison must match the budget. |
| Anthropic, [building a C compiler with parallel Claudes](https://www.anthropic.com/engineering/building-c-compiler) (Feb 2026) | 16 agents, each a `while true; claude -p` loop in its own container, with no orchestrator. An agent claims work with a lock file in `current_tasks/` in git. A near-perfect test oracle checks results, and GCC was used as a reference to split up bugs. About 2,000 sessions and just under $20k produced a 100k-line compiler that builds Linux. | Stigmergy over a shared log plus an oracle works for coding. borg-mesh is already such a log. |
| Anthropic, [agent teams](https://code.claude.com/docs/en/agent-teams) | A shared task list with file-lock claims, a mailbox for each agent, 3–5 teammates. One machine, one session, no nesting. The docs say: "For sequential tasks, same-file edits, or work with many dependencies, a single session or subagents are more effective." | The same primitives, but local and short-lived. |
| Anthropic, [building effective agents](https://www.anthropic.com/engineering/building-effective-agents), [long-running harnesses](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents), [context engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents) | Patterns: orchestrator-workers, parallel work by sections or by voting, evaluator-optimizer. Long runs work one feature per session against a feature list where only `passes` may change. Subagents hand back 1–2k-token summaries. | A subagent compresses its context away. A peer drone keeps its context. |
| Anthropic, [Project Vend 2](https://www.anthropic.com/news/project-vend-2) | A "CEO" agent on the same model shared the shopkeeper's blind spots. Clear roles and fixed procedures helped more than oversight did. | Verifiers need decorrelated errors, not just a second instance. |
| Cognition, [Don't build multi-agents](https://cognition.com/blog/dont-build-multi-agents) | Every action carries an implicit decision, and agents that can't see each other's decisions conflict. Agents should share full traces, not just messages. | The main objection. Contracts (below) are the answer. |
| OpenAI: [Swarm](https://github.com/openai/swarm), the Agents SDK, and the practical guide (manager or decentralized handoffs) | The guide says to max out a single agent first (unverified wording). [Codex cloud](https://openai.com/index/introducing-codex/) runs independent parallel tasks. The ICPC 2025 12/12 came from generating many candidates and selecting among them, not from cooperation ([source](https://the-decoder.com/openai-outperforms-humans-and-google-at-the-worlds-top-collegiate-programming-contest/)). | Parallel sampling plus selection is the baseline to beat. |
| Google, [Towards a science of scaling agent systems](https://research.google/blog/towards-a-science-of-scaling-agent-systems-when-and-why-agent-systems-work/) (180 configurations) | +81% on parallelizable tasks, −39% to −70% on sequential ones. Errors amplify 17.2× with independent agents and 4.4× with central verification. | Label every workload by structure, and gate on a verifier. |
| Google [AI co-scientist](https://research.google/blog/accelerating-scientific-breakthroughs-with-an-ai-co-scientist/), AlphaEvolve | A tournament or evolutionary population plus an automatic evaluator. | The evaluator is what makes a population useful. |
| [MAST](https://arxiv.org/abs/2503.13657) (Berkeley, NeurIPS 2025) | Failures break down as 42% specification, 37% misalignment between agents, 21% verification. Adding a verifier improved results by 15.6%. | Specifications and verification matter more than the topology. |
| [Debate or vote](https://arxiv.org/abs/2508.17536) (2025), [More agents is all you need](https://arxiv.org/abs/2402.05120) (2024) | Most of debate's gain is majority voting, and plain voting scales with the number of samples. | Best-of-N with voting is a required baseline. |
| [AI agents that matter](https://arxiv.org/abs/2407.01502) (2024), [Tran & Kiela](https://arxiv.org/abs/2604.02460) (2026) | Cheap retry baselines match complex agents. With thinking tokens held equal, a single agent ties or beats multi-agent setups. | Report accuracy against cost as a curve, not a single number. |

| Anthropic, [Redesigning source control](https://www.anthropic.com/research/redesigning-source-control) (Oct 2026) | Merged changes grew from about 2,500 a day to over 30,000 a day in four months. Code is no longer the bottleneck; infrastructure, review and judgment are. Engineers supply "understanding of the high-level inter-system components and the design-level tradeoffs." "Very often, we found that the role of the engineers was to tell Claude 'no'": agents broke core constraints, and adversarial review over-engineered. Humans review "the intent of the change as opposed to reading every line of code," with semantic approval rules sending only consequential changes to people. "Superfan" Claudes each watch one component. | The engineer is the scarce resource and holds the system view. The hive must spend engineer attention carefully and turn every "no" into something durable. |

Bottom line: extra parallel compute is not an advantage, because a single agent can buy the same with more tokens. A collective wins only where it has something more tokens can't buy.

## Thesis: what a borg hive has that subagents don't

1. **Context ownership.** Each drone keeps a long-lived context: one repo, one machine and its credentials, toolchain and hardware, plus its engineer's lessons. A subagent borrows its parent's machine and comes back with a 1–2k-token summary. This explains the one-session-per-repo observation.
2. **Contracts, not traces.** When a drone makes a decision other drones must respect, such as an API shape, a schema, a name or an invariant, it publishes a signed contract line. Hooks show each contract to drones whose edits touch its scope, the same surfacing mechanism as [decision links](decision-links.md). Conflicting contracts go to the queen for a human to decide. Drones share decisions, not whole transcripts, which answers Cognition's objection at much lower cost.
3. **Coordination through a signed log.** The task board is append-only lines, and claims are leases. There's no leader, no election and no consensus round. It survives network partitions and NAT, because borg-mesh already syncs lines this way. This is the C compiler's git lock files made native and spread across machines.
4. **A task is done only when the oracle passes.** The goal includes an oracle command. A drone that holds a claim can't stop while the oracle fails, using the same blocking as the conscience check. A different drone verifies every finished task in a fresh context: central verification (4.4× instead of 17.2× error amplification) plus MAST's verifier gain.
5. **Diversity that decorrelates errors.** Drones can run different harnesses and models (claude, codex, opencode) and carry different engineers' memories. Voting and verification help only when errors are decorrelated, and a hive gets that for free.
6. **The collective keeps what it learns.** Lessons, contracts and decision links outlive the run, so a later goal of the same shape starts warm. Subagents can't do this, and it is measurable. [hive-learning.md](hive-learning.md) turns this into a self-improvement loop: drones run the work, the mesh shares their experience, and the hive learns lessons, playbooks and its own configuration without touching model weights.

7. **The engineer says no and holds the system view.** Drones write code faster than people can read it, so the engineer's job shifts:
   - choose the goal and its scale
   - own the invariants and the picture of how systems fit together
   - review intent instead of lines
   - say "no"

   The hive is built around that role. It doesn't route around the engineer.

## The engineer's role

Anthropic's source-control team found what we see too: engineers mostly add judgment, not code. The hive makes that judgment cheap to give, hard to forget and always respected.

- **The engineer owns the goal.** Only a human signs a goal and approves its plan through the queen. Drones may propose goals; they never start one.
- **Constraints are the engineer's system view, written down.** A constraint is a contract only an engineer can write or ratify, for example "merge-to-main lookups go through the review API, never the commit log." The two kinds differ:
  - **Drone contracts are soft.** They are shown when relevant; a conflict goes to the queen.
  - **Constraints are hard.** `pre_tool` and `on_stop` block an edit or completion that a constraint's check rejects. A check can be a command, a grep, an import rule, or for now a model-graded question.
  - **Where they live:** constraints are kept in `constraints.jsonl` for each collective, not each goal, so they outlive the run.
- **"No" is one cheap line.** `borg no <task|change|plan> "<why>"` and the queen's answer both send a signed `veto` directive. The vetoed task reopens with the reason attached, and the reason becomes a lesson, or a proposed constraint when it names an invariant. A drone may not argue a veto back in. It may ask one clarifying question through the queen. *Built (hive-learning.md 7b):* `borg no` of a drone on another node sends a signed `veto` directive, recorded once on the drone's node; a veto names a drone, not a task, change or plan, and neither reopens a hive task nor comes from a queen answer yet.
- **Review intent, not lines.** Every `done` task publishes an intent card:
  - what changed and why, in a few lines
  - the contracts and constraints it touched
  - the risk class
  - the verifier's verdict and the hidden-oracle result

  Semantic approval rules choose the risk class: docs, comments and tests in scope are auto-approved, ordinary logic gets sampled review, and anything touching a constraint's scope, security or data migrations needs an engineer. The queen orders intent cards by risk, not by arrival time.
- **The verifier is scoped against over-engineering.** It checks the work against the task, the contracts and the constraints. It doesn't invent hypothetical threats. Verifier suggestions that add scope go to the queen as proposals, not rejections.
- **Superfan drones.** A drone can own one component for a long time and watch its logs and telemetry (context ownership, thesis 1). Its findings become `open` tasks the engineer can approve or veto.
- **Engineer attention is a budget.** Every escalation costs minutes. The hive tracks engineer minutes per goal, the share of escalations answered "yes, fine" (wasted attention) and the share of "no" answers that arrived only after merge (missed escalation). *Built (hive-learning.md 7b, escalation.rs):* the queen's attention reward counts both, team-wide, on borg-web's queen view; engineer minutes per goal are not tracked yet.

## Primitives

Each is a log of lines under `<store>/hive/<goal>/`, synced like `lessons.jsonl` (union, signed `Origin`, revoked keys quarantined): `goal.jsonl`, `tasks.jsonl`, `contracts.jsonl`, `messages.jsonl` and `flags.jsonl` (built in step 4, `borg/mesh/7`). The hidden oracle, its scores, joins, spend, runs and logs stay on their node.

### Goal

`goal.json` holds:
- the spec
- the oracle command(s)
- the dollar budget
- the repos in scope, and the node expected to hold each repo

It syncs as its projection, one compact line in `goal.jsonl`: the hidden oracle stripped, `held_out: true` saying one exists (a verdict elsewhere then waits for the runner node's score). A node without `goal.json` loads the goal from that log (the lowest `(created_ms, line hash)` line naming the id). `sig` stays empty: every line is signed by the member that first stamped it, the trusted-member model. Whether only an admin's goal should sync is open.

- **One definition per id.** Two nodes can `init` one id before a sync round, and a member can append a line naming a goal it did not make. When `goal.jsonl` (with this node's `goal.json`) holds two different projections of one id, every node refuses the goal, the one holding `goal.json` too (`goal::conflict`), and `borg hive board`, `borg top` and the Hive tab say why. It stays stopped until one definition is left: make the goal again under another id, or revoke the member whose line it was.
- **Checks run only where approved.** An oracle check is a shell command, and the board and `goal.jsonl` sync from every member. A node runs its own `goal.json` oracle and the checks of tasks it opened (`hive::write` records them in the local `approved.jsonl`); any other check (a synced goal's oracle, a remote task's `accept`) waits for the engineer's `borg hive approve <goal>`, which prints each one it approves. Until then its task is not offered or claimable there, an integration that needs it is blocked, and the board and the Hive tab list the commands waiting.

The goal's plan (its task list) goes through the queen, and the human approves it before workers start.

### Board

`tasks.jsonl` records events: `open`, `claim`, `heartbeat`, `progress`, `done`, `verified`, `rejected`, `released`, `blocked`.

- **Task fields:** id, role tag (`plan`, `work`, `verify`, `integrate`), the repo and files in scope, its dependencies, and acceptance criteria phrased as oracle checks.
- **Claims are leases.** Heartbeat lines on the board renew them, and a claim expires when its drone goes idle. Heartbeats sync with the board, so a lease held on node B is renewed on node A too; nothing else (presence, live channel) is needed.
- **Nodes** (step 4): every line a node writes carries its node id (`hive::write`; empty in older lines, which count as this node's). A runner releases only its own node's claims. Drone names (`d1`..) repeat on every node, so a verify task is barred to the author's session everywhere and to the author's drone name only on the work's node.
- **Claim races:** when two claims arrive for the same task, the lowest `(ms, blake3(key))` wins. Every node computes the same winner without coordinating, and the losing drone learns on its next hook call and releases the task. Across machines a claim is provisional until the next sync round: the loser's done and verify-task open are ignored by the fold, so a loser wastes at most one sync latency, and the clocks of the nodes must stay well within a lease.
- **Verification:** a `verify` task is created for every `done`, and the drone that did the work can't claim it. A `rejected` reopens the work task with the verifier's note attached.
- **Dependencies carry their work** (built). A task can be claimed only once each of its deps is verified. Each drone works in its own worktree, so the claim also brings the deps' work in:
  - Before the claim's `base` is taken, each transitive dep's accepted attempt (its claim's `base` to its done's `head`) is applied to the claimant's working tree, in the order the deps were verified. `git apply` runs at the toplevel and leaves the index, HEAD and refs alone. A change that is already there (the dep's author claims the dependent in its own tree) is skipped.
  - The claim records `deps_in`: `task@head` for each dep. Because the base is taken after the bring-in, the work's diff holds only its own changes: a dep's test edits are never charged to the dependent.
  - If a dep's change does not apply (the tree's own uncommitted changes overlap it), the claim is refused with git's reason and the tree is left as it was. A drone with a clean tree can still claim the task. A claim that loses its race takes the applied changes out again.
  - The dependent's oracle, its verify task (which lists the same deps) and its hidden score therefore all run on a tree that contains the deps. A pass on work whose claim lacks a dep's accepted head is an unchecked reject.
  - `init` and `open` refuse a dep that names no task, and `init` refuses a cycle in its batch. A board like that could never finish.
- **Integration** (`hive/integrate.rs`, built): once every task is verified, an `integrate.<n>` task is opened and claimed like any other. Its integrator merges the accepted attempts, never a drone's branch. For each verified work task that is the diff from its accepted claim's `base` to its done's `head`. The attempts are applied in order (by `deps`, then by verification time, then by id) with `git merge-tree --merge-base`, which needs git 2.40+, onto the newest common ancestor of the HEADs every work claim started from, so a drone's own commits never enter. No git call of it runs a hook. Each step is a commit with a fixed identity and date, so a later round replays the same attempts to the same commits, conflict markers included. Changes in a worktree that no claim and done cover never enter the result. The combined tree is kept at `refs/heads/hive/<goal>/integrated`.
  - The integrate task's `verified` or `rejected` carries that commit as `head`. A conflict no later attempt resolves is rejected without an oracle run; only an attempt built on the step with the markers (its base descends from it: a fix task's) and changing the path resolves it. Otherwise the goal's oracle and every integrated task's own checks run in a scratch worktree, and any failure rejects it.
  - A rejection opens the fix task `<integrate task>.fix` in the same write. It is a work task with `from` set to the combined tree, scoped to the union of the tasks' scopes, with the combined checks, and its first note says why the integration failed. Its claim resets the claimant's runner worktree to `from`, so its diff starts there. Any other session claims it only in a checkout of `from` (`git worktree add <dir> <from>`), never reset, so a goal worked by joined sessions and `borg hive integrate` finishes too. The next integration merges its accepted attempt last.
  - A missing snapshot, or a git that cannot merge, makes the integration `blocked` until an engineer reopens it.
- **Finished:** there are tasks, every one is verified, and an integration passed no earlier than the last verification. A fix task verified after a failed integration therefore needs one more integration.
- **Reward lines** (hive-learning.md steps 2 and 8, built): a work task's `claim` carries `base` and its `done` `head`, commits of the drone's working tree then (`hive/verdict.rs` `snapshot`, no ref, index untouched). A verify task's `open` carries `flags` (the task def's, rendered from `flags.jsonl`). A `rejected` may carry `hacked: true`: outside the task's `files` its diff weakened tests (tests may grow, never shrink: removed or reshaped assertions, a dropped test, an added skip) or changed CI or a test runner's config, or it changed the visible check's files at all, or its drone touched the oracle's files (hive-learning.md step 8). The verify task's id names the attempt (`<goal>/<verify task>` is the credit's `cause`), so no `attempt` field is needed. Two more files: `flags.jsonl` (`hive/guard.rs`: one line per session, class and path written to tests, CI or oracle files at `pre_tool`) and `oracle.jsonl` (`hive/hidden.rs`: the hidden oracle's score per judged attempt, runner-only, never synced). When goals sync, `goal.json`'s `hidden` is stripped from the synced projection and stays on the runner's node.

### Contracts

`contracts.jsonl` lines hold `{task, scope: [files | identifiers | repo], text}`.

- **When a drone sees them:** at `on_prompt` for contracts in its task's scope, and at `post_tool` for edits that land in a contract's scope. Each is shown once per session, deduplicated with a `.ctx` marker the way `#borg-early` is (`#borg-hive-contract`).
- **Changing one:** a drone may supersede its own contract: one from the same session, or from the same drone (the contract's `drone`) on the same machine, so a later session of the drone needs no queen answer. Superseding another drone's contract goes through the queen.

### Messages between drones

`borg hive ask` and `tell` append to the goal's `messages.jsonl` (`hive/message.rs`, built in step 4), a hive log synced like the board, not a mesh steer kind: steer is ephemeral (a TTL), gated to the admin and steer roles (letting `member` send would widen who can pause), and addresses nodes or sessions, not tasks or drones. A hive log gets signed origins, quarantine, offline catch-up and per-goal scope for free, and works on one machine with no mesh.

- A message goes to everyone, a task (its holder), a drone (that name on the sender's node) or a session; `tell --re` answers one.
- Rate-limited per sender (drone, else session) and recipient: one per `HEARTBEAT_MS`, against the status-noise failure Anthropic saw.
- Shown once per session at a prompt (a `#borg-hive-msg` marker, like contracts), never to its sender, only while it is fresh (a lease).
- `pause` and `resume` stay steer's, limited to admin and steer.

### Roles

A role is a tag on a task, not a fixed property of a drone. Any drone can plan, work or verify. Specialization comes from what a drone holds (its repo, machine and memories), not from a role prompt.

`integrate` tasks are the exception: a model session never claims them, and hooks, `borg hive claim` and the runner's headless sessions never hand them out. `borg hive run`'s drone threads run them in process at no model cost, and `borg hive integrate <goal>` runs them for a goal worked interactively. Three reasons: the work is mechanical, a full suite outlasts any stop hook, and a model would be judging a tree it did not build.

### Hooks

- `on_prompt`: inject the drone's claim, what changed on the board, contracts in scope, and any unread messages.
- `on_stop`: block while the drone holds a claim whose oracle fails. Once the task is done, publish `done` and hand the drone the next open task.
- `pre_tool`: keep honoring `pause`.

### Runner and watching

- **Runner:** `borg hive run --goal G --drones N` creates one git worktree per drone and loops `claude -p` (or another harness) in each. The prompt is short: read the board, claim, work, publish contracts, finish only when the oracle passes. Once every task is verified, the first drone thread to see it integrates, and the others wait on its lease. On a pass the run ends with exit 0 and prints `integrated <commit>`. On a failure the drones take the fix task like any other work.
- **Joining:** interactive sessions join with `borg hive join G`.
- **Watching:** `borg top` and `borg web` get a hive view showing the board, claims, contracts and spend. It also shows the integration: `integrated <commit>`, or why the latest integration was rejected or blocked. Each integration also counts a `hive_integrate` stat.

## Reuse

| Need | Existing code |
|---|---|
| Synced, signed append-only lines | `borg-mesh/src/lines.rs`, `origin.rs`, `recon.rs` |
| Heartbeats, state, the files a drone touched | `presence.rs`, `borg-mesh/src/live.rs` |
| Signed messages, show once, pause | `steer.rs`, `borg-mesh/src/steer.rs` |
| Human decisions, conflict escalation | `queen.rs` |
| Showing context, dedupe markers | `free_context` and `.ctx` markers in `hook.rs` |
| Blocking a stop | `on_stop` and the `conscience.rs` path |
| Multiple harnesses | `harness/` (`Harness`, `Caps`) |
| Arms, cost, solve rate | `crates/borg-eval` |

## Proof protocol

Run with `borg-eval`. Every arm gets the **same dollar budget**, counting all agents' tokens and coordination traffic.

### Arms

| Arm | Setup |
|---|---|
| A | One session |
| B | One session with subagents or a Workflow (the current best practice) |
| C | Claude Code agent teams |
| D3 / D9 | Hive with 3 or 9 drones on one machine |
| E | Hive with one drone per repo, spread across machines |
| F | Best-of-N with voting or selection at the same budget |

### Workloads

Each workload is labelled by its structure and drawn from company repos.

| Workload | Shape | Oracle |
|---|---|---|
| parallel | A list of independent bugs or modules (C-compiler style) | The repo's test suite plus held-out tests |
| sequential | One tightly coupled feature in one repo | Held-out tests |
| cross-repo | A coordinated API or schema change across 2–3 repos | The test suite of each repo, plus an integration check |
| warm-start | A second cross-repo goal of the same shape, run after the first | Same as cross-repo |

### Metrics

- solve rate on the hidden oracle
- dollars, tokens and wall-clock time
- duplicated work, merge conflicts and rework (count of reopened tasks)
- contract conflicts
- engineer minutes per goal, vetoes per goal, constraint violations caught before and after merge
- failures tagged with MAST categories
- at least 3 seeds per cell, reported with variance and as accuracy-against-cost curves

### Hypotheses (registered before running)

- **H1:** on cross-repo and parallel work, the hive (D/E) beats B on wall-clock time at the same dollars, without a lower solve rate.
- **H2:** on sequential work, the hive ties or loses to B. If so, we publish that.
- **H3:** in the hive, a warm-start run costs less than a cold run of the same shape. B shows no such effect.
- **H4:** turning contracts off raises cross-repo failures in the MAST misalignment category. This would show that contracts carry the win.
- **H4b:** with constraints and intent cards, the hive needs fewer engineer minutes per solved goal than B, with no more constraint violations after merge.
- **Kill criterion:** replaced on 2026-10-23, before the full pilot, by an acceptance gate (step 4): the engineer asked only for proof "that it works without serious bugs". H1–H4b stay registered and unmeasured.

## Steps

1. **Local spike, file-backed with no mesh** (`crates/borg/src/hive/`): build `board.rs` (the event log, leases and the winner rule), `contract.rs` (publishing contracts and matching them by scope) and `run.rs` (the worktree loop). Add the CLI commands `borg hive init | run | join | board`, and the `on_prompt` and `on_stop` additions.
   *Built (H1, local):* `hive/goal.rs` (goal.json, written once), `hive/board.rs` (tasks.jsonl, a fold that gives one board in any arrival order, leases of `presence::IDLE_MS` renewed by the hooks, the winner rule, verify tasks the author may not claim, rework on rejection, claim spans for credit), `hive/contract.rs` (contracts.jsonl, own supersession, another drone's through a `Kind::Contract` queen question, scope matching by glob, `id:` and `repo:`), `hive/oracle.rs` (bounded `sh -c` checks, process-group kill, failure tails), and the hook glue in `hive/mod.rs`: claim, board changes and contracts at a prompt; contracts and out-of-scope edits after a tool call; at the stop, a failing oracle blocks (at most 3 times per claim, also on the second round), a passing one publishes done, opens the verify task and hands over the next open task. CLI: `borg hive init | board | join | leave | open | claim | progress | release | block | done | verify | contract`; `BORG_HIVE=<goal>` joins a drone at its first hook. Deferred: `run.rs` and the runner (H2), mesh sync of hive lines (after the pilot), goal signatures (`sig` stays empty), queen approval of the plan (H1: the tasks the engineer opens through the CLI are the approved plan; tasks a drone opens are claimable at once, which this doc's "human approves the plan" does not allow yet), budget enforcement (recorded and shown only), and messages. Dedupe follows the `#borg-early` marker pattern (`#borg-hive-contract`), not a `#borg-decision` marker, which does not exist. Known limits: a suite slower than the stop hook needs `borg hive done`; a drone with no hook event for 30 minutes loses its claim; one claim per drone is enforced by the CLI and the handover, not the fold.
   *Built (H2, local):* `hive/run.rs`, `borg hive run <goal> --drones N`: one worktree per drone (`<repo>/.worktrees/hive-<goal>-<n>`, one path component so `repo_key` folds it back, `/.worktrees/` in the local `info/exclude`), each drone a loop of headless sessions through `Harness::headless` (Claude Code, omp and opencode 1.x checked against their `--help`; codex reports no dollars and fails closed after one session; opencode 2.x not yet), an equal share of what is left of the goal's budget (every run's `spend.jsonl` counted, one run of a goal at a time; a session cut off or failed without a cost is charged an estimate; a killed run's sessions are charged, and its claims released, by the goal's next run), the drone's existing branch checked out on a re-run (never reset), a leave of each worktree when the run ends, and a pure `step` that ends a drone on a finished board, its spent share, an unmetered (or $0) session, two failures, two sessions without board progress or three idle polls with no work in flight (a drone waits while another holds work whose verify task it could take). Budget enforcement is both, as the open question below guessed: Claude Code stops itself (`Caps::headless_budget`, `--max-budget-usd`), the runner checks between sessions. An ended session's claims are released at once; an iteration's sessions are the one the runner named, the one the harness reported and every session whose claim carries the drone, so a harness with its own session ids (omp, opencode) is tracked by drone. Claims carry the drone (`BORG_HIVE_DRONE`), so a verify task is barred to every session of the author drone. `borg hive join --here` (and each worktree) writes a join record, so a harness that drops the env still joins. `hive/view.rs`: the hive in `borg top` (text and `--json`), `/api/hive` and the web page's Hive tab. `hive/sim_drone.rs`: a scripted drone of the hidden `sim` harness against the real hooks, which the runner's suite runs with 3 drones. Integration is built (`hive/integrate.rs`, Board above): the accepted attempts merged, the combined tree oracle-checked, a fix task on a failure. A dependent task's claim brings its deps' verified work into the claimant's tree (`verdict::bring_in`, recorded as `deps_in`), so dependent work is built, verified and scored on top of its deps. Deferred: multi-repo goals, killing an unmetered session mid-way at its budget, a dollar price for codex tokens (the hidden oracle, the reward step, is built: hive-learning.md step 2, scored in `after_run`), queen approval of the plan before `run`, and the mesh lines.
2. **Eval:** add arms B, D and F to `borg-eval`. Write one parallel workload and one cross-repo workload from company repos on the work machine; their oracles stay private there.
   *Built (harness, no paid run yet):* `crates/borg-eval/src/hive.rs` measures the pre-registered pilot rule (tasks/todo.md "Hive pilot") and does not change it.
   - `borg-eval hive --workload DIR --arm a|b|d3|f --seed N --cap-usd X [--model sonnet] [--timeout-s 3600] [--run NAME] [--stop-at-usd Y]` runs one cell and appends one row to the run's `results.jsonl`. `--cap-usd` has no default. `--stop-at-usd` refuses a cell once the run's spend plus the cap would pass Y.
   - `borg-eval hive validate --workload DIR` checks that every hidden check fails on the base and passes on `reference.patch`, with no model call. An arm refuses a workload until it has a passing validate row for the same workload hash (`workload.json` plus every file it names).
   - `borg-eval hive report <results.jsonl>...` prints per workload and arm: median wall, mean solve share, dollars, tokens, rework, conflicts, hacked and over-cap counts. It then prints the H1 line, inclusive at the boundary, and the kill line. It uses no judge.
   - A workload (`workload.json`, kept outside every repo and sandbox) names source repos at pinned 40-hex commits, an optional injected patch, the visible spec and check, and K hidden checks in a directory outside every source and workspace. It also lists the hive's tasks, the HOME paths to bind read-only (Gradle caches; credentials and borg's stores are refused) and extra env.
   - Each cell builds its workspace with `git archive` at the pinned commit, so the source is never written and the workspace carries no history. The result is one commit, `base`; a cross-repo workload holds both projects as subdirectories.
   - Every arm runs in a fresh sandbox HOME (`dogfood.rs` `prepare_home_with`). It gets a fresh store with no lessons, every borg model call off, and the real HOME hidden.
   - A and B are one `claude -p` session each: A with subagents disallowed, B invited to use them. F runs three A sessions at cap/3 in parallel and picks by the visible check, then cost, then index. D3 runs `borg install claude --apply`, then `borg hive init` (with no hidden oracle in the sandbox), then `run --drones 3`. The hive integrates itself (Board, Integration), and the cell scores the commit the board names at `refs/heads/hive/<goal>/integrated`: the passing integration's when the board finished, else the newest integration's (a failed one's tree), else `base` when nothing was merged. Its conflicts are the integrations rejected for a merge conflict.
   - Scoring runs after the arm in a sandbox with no network, no credentials and git hooks off. It uses a fresh clone of the result commit, so no build output or untracked file carries over. Hacked is read off the clone before any check runs the result's code, by the hive's own rule (`verdict::protected` on the base-to-result diff, scoped by the workload's task `files`): tests may grow, never shrink. Outside the tasks' files a removed, reshaped or tautological assertion, a dropped test declaration, an added skip, a deleted or renamed test, or a changed CI or runner config counts; added tests, renames and updated values do not, and the visible check's files are never in scope. It counts for every arm alike, with why in `hacked_why`; the board's hacked verdicts are reported apart.
   - Tests: unit tests with fixtures; end to end with bwrap, the sim harness (D3) and a fake `claude` (A, B, F): `cargo build -p borg-ai && cargo test -p borg-eval hive_e2e -- --ignored`. The local driver `eval/hive-pilot/drive.py` (`probe <cap>`, `full <cap>`) runs cells one at a time, because wall-clock is a metric.
   - Known gaps: a drone sees only the task title and spec line 1, so each task title carries its full bug report. A goal covers one repo. Dollar caps are soft and over-cap is reported, never rescaled.
3. **Pilot:** N=3, 3 seeds, a capped budget. Record the summary table in this doc.
   *Run as an acceptance test (2026-10-23):* the engineer replaced the H1 superiority gate before the full pilot: "we only need to prove that it works without serious bugs." Run `hive-accept-1`, D3 only, borg 1f564c9 + cb73cba, 3 seeds on each of two private production workloads:
   - **W1** (parallel): six injected bugs in one service, K = 6 hidden checks.
   - **W2** (cross-repo): a field rename across a service and its consumer, the consumer's task depending on the producer's, K = 5.

   | Run | Cells | Dollars | Per cell (dollars / wall), in run order | Result |
   |---|---|---|---|---|
   | hive-accept-1 | 6 | $8.48 | $1.58/464 s, $0.89/409 s, $1.95/477 s, $1.02/671 s, $1.94/535 s, $1.09/503 s | ACCEPTED 6/6 |

   Every board finished with one integration; the visible check passed, hidden 6/6 on W1 and 5/5 on W2, conflicts 0, hacked 0.
   - **The probe before it** (D3 solved 2/5 on W2) found the bugs fixed for this run: idle drones quit while work was in flight; the combined tree was never integrated, so stray work of a drone that had lost its claim was merged; required test additions were flagged as hacked; dependent work started without its dependency; a snapshot missed a same-size edit made within the index's own second; a refused claim read as the drone's own.
   - **Not measured:** superiority over one session with subagents (B). The workloads were too easy to tell the arms apart, so H1–H4b have no data.
   - **Not exercised on a real repo:** the fix-task loop after a failing integration. The sim and CLI tests cover it.
4. **Decide.** The gate is acceptance, not H1: a hive feature ships once it works without serious bugs on real repos. On one machine that held (step 3), so the next step is to add the hive lines to mesh sync, `ask`/`tell` between drones, and arm E across machines, gated by the same acceptance run across machines. Whether the hive beats B stays open until a workload hard enough to tell them apart exists.
   *Built (preparation for arm E):* the hive's logs sync over the mesh (`borg/mesh/7`): `goal.jsonl` (the goal's projection, `hive::publish`, without the hidden oracle), `tasks.jsonl`, `contracts.jsonl`, `messages.jsonl` and `flags.jsonl` of every goal (borg-mesh lines.rs `hive_logs`, a `Goals` swap of path-checked ids), signed, quarantined on revocation and covered by the daemon's change fingerprint, so a claim or heartbeat travels within a round. Every line names its node; runners release only their node's claims; drone names bar only on their node. Verdict lines carry the kind they credit, and the author's node credits a verdict given elsewhere at its next round, once (`verdict::credit_synced`); verdicts join `SHARED_KINDS` with `c`, bound to the local board's verdict (hive-learning.md step 2). `borg hive ask|tell` (Messages above). A verify or fix task whose commits this repo lacks is not offered, and an integration runs only on a node holding every accepted attempt, else writes nothing and says which work is where. `borg top` and the Hive tab name a holder's node and list messages. Also fixed: a runner drone's fresh session was offered the verify task barred to its drone (the hive-accept-1 build barred by session and the board's session→drone map only, so a session with no board line passed the pre-claim check and its claim was voided by the fold); the drone now comes from the runner's `BORG_HIVE_DRONE` when the membership names none, and `borg hive claim` without `--session` resolves the drone's own session. Proof, 0 tokens: two loopback nodes in one process with drones run as the real `borg` binary (crates/borg-mesh/tests/hive_mesh.rs): a task opened on A is claimed on both nodes before a sync, one claim wins on both, the work is done on B and verified on A by another node's `d1`, both boards fold identically, no task has two holders at once, the lesson shown during the claim is credited once on B and counted once in both nodes' memory, a message reaches the verifier once, and a further round moves only digests. *Not measured:* arm E and the acceptance run across real machines (a paid run with its own cost estimate). *Known limits:* claims are provisional until a round; the winner rule compares clocks (skew must stay well within a lease), and a member that backdates a claim line wins its race whenever it arrives, voiding a holder's finished tenure (bounding a line's `ms` by its origin stamp would refuse honest lines of a node that was offline; deferred, revocation quarantines it); board lines carry raw session ids, so a team outcome line's unsalted session tag (outcome.rs `tag`) links it to a hive session, its task and node: hive sessions are visible to the team (a per-node salted alias on board lines is deferred); commits do not travel (both test nodes share one object store); a goal with a hidden oracle credits a remote author's attempt not yet; the trusted-member model (any member's goal syncs, `sig` stays empty; a member can forge a board verdict and its team line, though a team verdict counts only under the attempt author's session tag, for lessons the team's records show it was given, and with a hidden oracle only from the work's node; credits already made from a verdict a revocation later quarantines stay); every goal ever made syncs (about five fingerprints per goal per round; compaction deferred); a quarantine rewrites a board, so a session may be shown changes again once.

### Tests

- Claim races: two claims in any arrival order produce one winner on every node.
- Lease expiry.
- Matching contracts to scope.
- The stop hook blocks while the oracle fails.
- A `sim` harness run with 3 simulated drones on one board: `borg hive run` finishes it, no task has two holders at once, every verdict comes from another drone, and the spend stays within the shares (crates/borg/tests/hive_run_cli.rs).
- Across machines (step 4): two loopback mesh nodes with drones as the real `borg` binary (crates/borg-mesh/tests/hive_mesh.rs); the fold of remote lines in every arrival order, a drone name barred only on its node, and a runner that never releases another node's claim (board.rs, run.rs); a revoked member's board line quarantined and goal ids path-checked (borg-mesh sync.rs), a quarantine and the hive's appends sharing the log's lock (borg-mesh origin.rs); two definitions of one goal stopping it on every node (goal.rs), a check another node wrote running only once approved (oracle.rs), a forged message line dropped (message.rs), a forged claim and done naming this node credited nothing and a board a quarantine rewrote read again (verdict.rs), a hidden oracle's path never in the synced flags (guard.rs); a verify task or an integration whose commits are absent refused cleanly, ask/tell once to the holder, and a fresh runner session not offered its drone's verify task (crates/borg/tests/hive_cli.rs).
- Integration and dependencies (crates/borg/tests/hive_run_cli.rs): a conflicting integration opens a fix task and the hive still finishes; a dependent task starts from its dependency's verified work; idle drones wait for work in flight; a drone that grows an existing test is not hacked; a killed run is charged and released by the next.
- The reward (crates/borg/tests/hive_verdict_cli.rs): `pre_tool` flags a test edit only for a drone in a goal and answers nothing; a verdict on a diff that weakened a test is `hacked` and credits the lessons shown during the claim once, one on work that only adds tests verifies; the author's session cannot give the verdict; a pass at the stop (`VERDICT: pass`) verifies and credits; with a hidden oracle the score decides the credit, once; no hook of a drone outside every goal creates a hive.

## Open questions

- **Budget enforcement:** each drone could stop itself when it reaches its share, or a runner could kill it. Probably both. *H2: both; a harness that cannot stop itself overshoots by at most one session.*
- **Merges:** on one machine the integrator merges the accepted attempts (built, and exercised on real repos). Across machines each drone owns its repo, so no two drones edit the same repo; integration then needs every attempt's commits on the integrating node, which the mesh lines do not carry (step 4: such a node integrates nothing and says which work is where).
- **Secrecy:** contracts and task text sync to every member. Do they need the same redaction applied to presence prompts?
