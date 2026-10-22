# Hive: many drones, one goal

Status: research and design only. Nothing is built yet. The experiment will run on the work machine against company repos.

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
- **"No" is one cheap line.** `borg no <task|change|plan> "<why>"` and the queen's answer both send a signed `veto` directive. The vetoed task reopens with the reason attached, and the reason becomes a lesson, or a proposed constraint when it names an invariant. A drone may not argue a veto back in. It may ask one clarifying question through the queen.
- **Review intent, not lines.** Every `done` task publishes an intent card:
  - what changed and why, in a few lines
  - the contracts and constraints it touched
  - the risk class
  - the verifier's verdict and the hidden-oracle result

  Semantic approval rules choose the risk class: docs, comments and tests in scope are auto-approved, ordinary logic gets sampled review, and anything touching a constraint's scope, security or data migrations needs an engineer. The queen orders intent cards by risk, not by arrival time.
- **The verifier is scoped against over-engineering.** It checks the work against the task, the contracts and the constraints. It doesn't invent hypothetical threats. Verifier suggestions that add scope go to the queen as proposals, not rejections.
- **Superfan drones.** A drone can own one component for a long time and watch its logs and telemetry (context ownership, thesis 1). Its findings become `open` tasks the engineer can approve or veto.
- **Engineer attention is a budget.** Every escalation costs minutes. The hive tracks engineer minutes per goal, the share of escalations answered "yes, fine" (wasted attention) and the share of "no" answers that arrived only after merge (missed escalation).

## Primitives

Each is a log of lines under `<store>/hive/<goal>/`, synced like `lessons.jsonl` (union, signed `Origin`, revoked keys quarantined).

### Goal

`goal.json`, signed by an admin, holds:
- the spec
- the oracle command(s)
- the dollar budget
- the repos in scope, and the node expected to hold each repo

The goal's plan (its task list) goes through the queen, and the human approves it before workers start.

### Board

`tasks.jsonl` records events: `open`, `claim`, `heartbeat`, `progress`, `done`, `verified`, `rejected`, `released`, `blocked`.

- **Task fields:** id, role tag (`plan`, `work`, `verify`, `integrate`), the repo and files in scope, its dependencies, and acceptance criteria phrased as oracle checks.
- **Claims are leases.** Presence heartbeats renew them, and a claim expires when its drone goes idle.
- **Claim races:** when two claims arrive for the same task, the lowest `(ms, blake3(key))` wins. Every node computes the same winner without coordinating, and the losing drone learns on its next hook call and releases the task.
- **Verification:** a `verify` task is created for every `done`, and the drone that did the work can't claim it. A `rejected` reopens the work task with the verifier's note attached.
- **Reward lines** (hive-learning.md steps 2 and 8, built): a work task's `claim` carries `base` and its `done` `head`, commits of the drone's working tree then (`hive/verdict.rs` `snapshot`, no ref, index untouched). A verify task's `open` carries `flags` (the task def's, rendered from `flags.jsonl`). A `rejected` may carry `hacked: true`: its diff changed protected tests or CI outside the task's `files`, or its drone touched the oracle's files. The verify task's id names the attempt (`<goal>/<verify task>` is the credit's `cause`), so no `attempt` field is needed. Two more files: `flags.jsonl` (`hive/guard.rs`: one line per session, class and path written to tests, CI or oracle files at `pre_tool`) and `oracle.jsonl` (`hive/hidden.rs`: the hidden oracle's score per judged attempt, runner-only, never synced). When goals sync, `goal.json`'s `hidden` is stripped from the synced projection and stays on the runner's node.

### Contracts

`contracts.jsonl` lines hold `{task, scope: [files | identifiers | repo], text}`.

- **When a drone sees them:** at `on_prompt` for contracts in its task's scope, and at `post_tool` for edits that land in a contract's scope. Each is shown once per session, deduplicated with a `.ctx` marker the way `#borg-early` is (`#borg-hive-contract`).
- **Changing one:** a drone may supersede its own contract. Superseding another drone's contract goes through the queen.

### Messages between drones

Extend `steer` with `ask` and `tell`, which any member can send, not only admin and steer roles.

- Messages are rate-limited per drone and per task, against the status-noise failure Anthropic saw.
- Delivery reuses the `steer/seen` exclusive-create claim so each message is shown once.
- `pause` and `resume` stay limited to admin and steer.

### Roles

A role is a tag on a task, not a fixed property of a drone. Any drone can plan, work, verify or integrate. Specialization comes from what a drone holds (its repo, machine and memories), not from a role prompt.

### Hooks

- `on_prompt`: inject the drone's claim, what changed on the board, contracts in scope, and any unread messages.
- `on_stop`: block while the drone holds a claim whose oracle fails. Once the task is done, publish `done` and hand the drone the next open task.
- `pre_tool`: keep honoring `pause`.

### Runner and watching

- **Runner:** `borg hive run --goal G --drones N` creates one git worktree per drone and loops `claude -p` (or another harness) in each. The prompt is short: read the board, claim, work, publish contracts, finish only when the oracle passes.
- **Joining:** interactive sessions join with `borg hive join G`.
- **Watching:** `borg top` and `borg web` get a hive view showing the board, claims, contracts and spend.

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
- **Kill criterion:** if H1 fails at N=3, stop before building mesh sync and write up why.

## Steps

1. **Local spike, file-backed with no mesh** (`crates/borg/src/hive/`): build `board.rs` (the event log, leases and the winner rule), `contract.rs` (publishing contracts and matching them by scope) and `run.rs` (the worktree loop). Add the CLI commands `borg hive init | run | join | board`, and the `on_prompt` and `on_stop` additions.
   *Built (H1, local):* `hive/goal.rs` (goal.json, written once), `hive/board.rs` (tasks.jsonl, a fold that gives one board in any arrival order, leases of `presence::IDLE_MS` renewed by the hooks, the winner rule, verify tasks the author may not claim, rework on rejection, claim spans for credit), `hive/contract.rs` (contracts.jsonl, own supersession, another drone's through a `Kind::Contract` queen question, scope matching by glob, `id:` and `repo:`), `hive/oracle.rs` (bounded `sh -c` checks, process-group kill, failure tails), and the hook glue in `hive/mod.rs`: claim, board changes and contracts at a prompt; contracts and out-of-scope edits after a tool call; at the stop, a failing oracle blocks (at most 3 times per claim, also on the second round), a passing one publishes done, opens the verify task and hands over the next open task. CLI: `borg hive init | board | join | leave | open | claim | progress | release | block | done | verify | contract`; `BORG_HIVE=<goal>` joins a drone at its first hook. Deferred: `run.rs` and the runner (H2), mesh sync of hive lines (after the pilot), goal signatures (`sig` stays empty), queen approval of the plan (H1: the tasks the engineer opens through the CLI are the approved plan; tasks a drone opens are claimable at once, which this doc's "human approves the plan" does not allow yet), budget enforcement (recorded and shown only), and messages. Dedupe follows the `#borg-early` marker pattern (`#borg-hive-contract`), not a `#borg-decision` marker, which does not exist. Known limits: a suite slower than the stop hook needs `borg hive done`; a drone with no hook event for 30 minutes loses its claim; one claim per drone is enforced by the CLI and the handover, not the fold.
   *Built (H2, local):* `hive/run.rs`, `borg hive run <goal> --drones N`: one worktree per drone (`<repo>/.worktrees/hive-<goal>-<n>`, one path component so `repo_key` folds it back, `/.worktrees/` in the local `info/exclude`), each drone a loop of headless sessions through `Harness::headless` (Claude Code, omp and opencode 1.x checked against their `--help`; codex reports no dollars and fails closed after one session; opencode 2.x not yet), an equal share of what is left of the goal's budget (every run's `spend.jsonl` counted, one run of a goal at a time; a session cut off without a cost is charged an estimate), the drone's existing branch checked out on a re-run (never reset), a leave of each worktree when the run ends, and a pure `step` that ends a drone on a finished board, its spent share, an unmetered (or $0) session, two failures, two sessions without board progress or three idle polls. Budget enforcement is both, as the open question below guessed: Claude Code stops itself (`Caps::headless_budget`, `--max-budget-usd`), the runner checks between sessions. An ended session's claims are released at once. Claims carry the drone (`BORG_HIVE_DRONE`), so a verify task is barred to every session of the author drone. `borg hive join --here` (and each worktree) writes a join record, so a harness that drops the env still joins. `hive/view.rs`: the hive in `borg top` (text and `--json`), `/api/hive` and the web page's Hive tab. `hive/sim_drone.rs`: a scripted drone of the hidden `sim` harness against the real hooks, which the runner's suite runs with 3 drones. Deferred: multi-repo goals, merging the worktrees, killing an unmetered session mid-way at its budget, a dollar price for codex tokens (the hidden oracle, the reward step, is built: hive-learning.md step 2, scored in `after_run`), queen approval of the plan before `run`, and the mesh lines.
2. **Eval:** add arms B, D and F to `borg-eval`. Write one parallel workload and one cross-repo workload from company repos on the work machine; their oracles stay private there.
3. **Pilot:** N=3, 3 seeds, a capped budget. Record the summary table in this doc.
4. **Decide.** If H1 holds, add the hive lines to mesh sync, `ask`/`tell` to mesh steer, and arm E across machines. If it doesn't, stop.

### Tests

- Claim races: two claims in any arrival order produce one winner on every node.
- Lease expiry.
- Matching contracts to scope.
- The stop hook blocks while the oracle fails.
- A `sim` harness run with 3 simulated drones on one board: `borg hive run` finishes it, no task has two holders at once, every verdict comes from another drone, and the spend stays within the shares (crates/borg/tests/hive_run_cli.rs).
- The reward (crates/borg/tests/hive_verdict_cli.rs): `pre_tool` flags a test edit only for a drone in a goal and answers nothing; a verdict on a diff that edited a test is `hacked` and credits the lessons shown during the claim once; the author's session cannot give the verdict; a pass at the stop (`VERDICT: pass`) verifies and credits; with a hidden oracle the score decides the credit, once; no hook of a drone outside every goal creates a hive.

## Open questions

- **Budget enforcement:** each drone could stop itself when it reaches its share, or a runner could kill it. Probably both. *H2: both; a harness that cannot stop itself overshoots by at most one session.*
- **Merges:** worktrees on one machine merge locally, but across machines each drone owns its repo, so no two drones edit the same repo. This needs checking against goals where several drones work in one repo.
- **Secrecy:** contracts and task text sync to every member. Do they need the same redaction applied to presence prompts?
