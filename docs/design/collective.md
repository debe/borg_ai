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

Bottom line: extra parallel compute is not an advantage, because a single agent can buy the same with more tokens. A collective wins only where it has something more tokens can't buy.

## Thesis: what a borg hive has that subagents don't

1. **Context ownership.** Each drone keeps a long-lived context: one repo, one machine and its credentials, toolchain and hardware, plus its engineer's lessons. A subagent borrows its parent's machine and comes back with a 1–2k-token summary. This explains the one-session-per-repo observation.
2. **Contracts, not traces.** When a drone makes a decision other drones must respect, such as an API shape, a schema, a name or an invariant, it publishes a signed contract line. Hooks show each contract to drones whose edits touch its scope, the same surfacing mechanism as [decision links](decision-links.md). Conflicting contracts go to the queen for a human to decide. Drones share decisions, not whole transcripts, which answers Cognition's objection at much lower cost.
3. **Coordination through a signed log.** The task board is append-only lines, and claims are leases. There's no leader, no election and no consensus round. It survives network partitions and NAT, because borg-mesh already syncs lines this way. This is the C compiler's git lock files made native and spread across machines.
4. **A task is done only when the oracle passes.** The goal includes an oracle command. A drone that holds a claim can't stop while the oracle fails, using the same blocking as the conscience check. A different drone verifies every finished task in a fresh context: central verification (4.4× instead of 17.2× error amplification) plus MAST's verifier gain.
5. **Diversity that decorrelates errors.** Drones can run different harnesses and models (claude, codex, opencode) and carry different engineers' memories. Voting and verification help only when errors are decorrelated, and a hive gets that for free.
6. **The collective keeps what it learns.** Lessons, contracts and decision links outlive the run, so a later goal of the same shape starts warm. Subagents can't do this, and it is measurable.

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

### Contracts

`contracts.jsonl` lines hold `{task, scope: [files | identifiers | repo], text}`.

- **When a drone sees them:** at `on_prompt` for contracts in its task's scope, and at `post_tool` for edits that land in a contract's scope. Each is shown once per session, deduplicated with a `.ctx` marker the way `#borg-decision` is.
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
- failures tagged with MAST categories
- at least 3 seeds per cell, reported with variance and as accuracy-against-cost curves

### Hypotheses (registered before running)

- **H1:** on cross-repo and parallel work, the hive (D/E) beats B on wall-clock time at the same dollars, without a lower solve rate.
- **H2:** on sequential work, the hive ties or loses to B. If so, we publish that.
- **H3:** in the hive, a warm-start run costs less than a cold run of the same shape. B shows no such effect.
- **H4:** turning contracts off raises cross-repo failures in the MAST misalignment category. This would show that contracts carry the win.
- **Kill criterion:** if H1 fails at N=3, stop before building mesh sync and write up why.

## Steps

1. **Local spike, file-backed with no mesh** (`crates/borg/src/hive/`): build `board.rs` (the event log, leases and the winner rule), `contract.rs` (publishing contracts and matching them by scope) and `run.rs` (the worktree loop). Add the CLI commands `borg hive init | run | join | board`, and the `on_prompt` and `on_stop` additions.
2. **Eval:** add arms B, D and F to `borg-eval`. Write one parallel workload and one cross-repo workload from company repos on the work machine; their oracles stay private there.
3. **Pilot:** N=3, 3 seeds, a capped budget. Record the summary table in this doc.
4. **Decide.** If H1 holds, add the hive lines to mesh sync, `ask`/`tell` to mesh steer, and arm E across machines. If it doesn't, stop.

### Tests

- Claim races: two claims in any arrival order produce one winner on every node.
- Lease expiry.
- Matching contracts to scope.
- The stop hook blocks while the oracle fails.
- A `sim` harness run with 3 simulated drones on one board.

## Open questions

- **Budget enforcement:** each drone could stop itself when it reaches its share, or a runner could kill it. Probably both.
- **Merges:** worktrees on one machine merge locally, but across machines each drone owns its repo, so no two drones edit the same repo. This needs checking against goals where several drones work in one repo.
- **Secrecy:** contracts and task text sync to every member. Do they need the same redaction applied to presence prompts?
