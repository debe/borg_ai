# Hive: many drones, one goal, and a hive that learns

## Status

Everything below is built as preparation: no value metric has been shown yet. The hive is a local, file-backed spike that syncs over the mesh. It was accepted on two company workloads on the work machine (C3): it works without serious bugs. Whether it beats one session with subagents is not measured. Across machines it is proven on loopback nodes, and arm E runs on one machine; the paid acceptance run across nodes has not run. The learning loop is built except the genome (L7, parked), the refusal predictor and the sampled audit (L7b). H5 ran without a decision, and learning stays on by the engineer's call (Proof). H1–H4b and H6–H8 have not run. The experiments run on the work machine against company repos.

| Step | What | Status | Proof | Deferred |
|---|---|---|---|---|
| C1 | Local spike: goal, board, contracts, oracle, hooks, runner, integration, plan approval and intent cards | Built | Sim drones against the real hooks (crates/borg/tests/hive_run_cli.rs, hive_cli.rs); accepted through C3 | Multi-repo goals; killing an unmetered session mid-way; a dollar price for codex tokens; plan approval across nodes, for joined sessions and for tasks a drone opens; the intent card at `done`, sampled review, ordering by risk |
| C2 | Eval harness: arms A, B, D3, E, F in `borg-eval hive` | Built | Unit tests with fixtures; end to end with bwrap, the sim harness and a fake `claude` | A workload hard enough to tell the arms apart |
| C3 | Pilot, run as an acceptance test | Accepted (hive-accept-1, 6/6, $8.48) | Hidden 6/6 on W1 and 5/5 on W2, conflicts 0, hacked 0 | H1–H4b not measured; the fix-task loop not exercised on a real repo |
| C4 | Across machines: logs sync, ask/tell, commits travel, secrecy, the integration conflict loop (D2), arm E on one machine, budget across nodes (D1) | Built; arm E's paid acceptance run not run | Two loopback nodes (crates/borg-mesh/tests/hive_mesh.rs), `hive_e2e_e`, hive_run_cli.rs | Plan approval across nodes; bounding a claim's `ms`; salted session aliases; a fix task's `from` travelling; a hard budget bound under lag; compaction |
| L1 | Log propensities and sync outcomes | Built | Unit tests; `outcomes-team.jsonl` synced (`borg/mesh/6`) | Syncing `graded` records; propensities of early and queued deliveries; poisoning resistance; compaction |
| L2 | Oracle rewards credited to the lessons shown | Built (local; verdicts given on another node credited since C4) | crates/borg/tests/hive_verdict_cli.rs | Crediting a remote author's attempt at a goal with a hidden oracle; crediting a verifier by the hidden oracle's agreement; engineer-overturned rejections; Shapley; k distinct nodes and a trust-weighted median |
| L3 | Regret reaches back | Built | Pre-registered check `eval/regret-credit/report.py` (local); no result recorded here | Shapley; credit for decisions made on another machine |
| L4 | Group reflection and rework lessons | Built | Sim runs against a stub model (hive_run_cli.rs), group.rs, rework.rs, learn.rs | Drones that differ within one run; a playbook edit as the reflection's output; dropping a group's losers; deps on a group; the integration's rejection as a lesson source; `borg learn --rework`; `μ·rework` in the reward; H6 not run |
| L5 | Playbooks (layer L1) | Built | playbook.rs, hive_verdict_cli.rs, sync.rs, borg-web `hive_api.rs` | Promotion gate on a held-out set; using the belief; the erosion check |
| L6 | Off-policy evaluation | Built; no logged decision has support yet | ope.rs tests; this machine's store: 0 decisions, constants unchanged | A randomised shortlist on the graded path; a doubly-robust estimator |
| L7 | Genome archive (layer L2) | Parked (not started) | — | All of it |
| L7b | Engineer judgment: `borg no`, veto across the mesh, the queen's attention reward, intent cards | Built except the refusal predictor and the sampled audit | sync.rs, membership_steer_cli.rs, veto.rs, escalation.rs, borg-web `queen_api.rs` | The refusal predictor; the sampled audit; routing beyond the claim escalation; enforcing ratified constraints; a veto that names a task, change or plan, reopens a hive task or comes from a queen answer; engineer minutes per goal |
| L8 | Guardrails | Built | hive_verdict_cli.rs; no false hacked verdict on hive-accept-1 | The hack monitor; the promotion and erosion gates |
| — | Learning health in production (D5) | Built | crates/borg-web/tests/learning_api.rs | Lesson sources team-wide; hidden-oracle failures per goal; engineer minutes per solved goal |
| H1–H4b | Hive against one session with subagents | Not run | — | Needs a harder workload |
| H5 | Learning lowers mistakes over a goal sequence | Run, not decided; learning kept on by the engineer | 50 of 90 runs, not informative | Judged in production through monitoring |
| H6–H8 | Group reflection, IPS tuning, hacked rate | Not run | — | — |

## Question

Can 3–9 drones connected by borg, possibly on several machines, solve a problem better than one session that uses subagents? And how do we prove the answer either way?

What we've seen so far: work that spans several repos goes better with one Claude Code session per repo, each using its own subagents, than with one session spanning all the repos.

The second half of the question: OpenAI and Anthropic say their models improve themselves through reinforcement learning. Borg can't train weights. Can a hive of drones still get better with every goal it finishes, and can we prove that it does?

## What is known

### Many agents on one problem

| Source | Finding | Takeaway for borg |
|---|---|---|
| Anthropic, [multi-agent research system](https://www.anthropic.com/engineering/multi-agent-research-system) (Jun 2025) | An Opus 4 lead with Sonnet 4 subagents scored +90.2% over a single Opus 4. It used about 15× the tokens of chat, and token use alone explains about 80% of the variance on BrowseComp. Coding and tightly coupled tasks fit poorly. Observed failures: spawning too many agents, duplicated work, status-message noise. | Most of the gain comes from spending more tokens. Every comparison must match the budget. |
| Anthropic, [building a C compiler with parallel Claudes](https://www.anthropic.com/engineering/building-c-compiler) (Feb 2026) | 16 agents, each a `while true; claude -p` loop in its own container, with no orchestrator. An agent claims work with a lock file in `current_tasks/` in git. A near-perfect test oracle checks results, and GCC was used as a reference to split up bugs. About 2,000 sessions and just under $20k produced a 100k-line compiler that builds Linux. | Stigmergy over a shared log plus an oracle works for coding. borg-mesh is already such a log. |
| Anthropic, [agent teams](https://code.claude.com/docs/en/agent-teams) | A shared task list with file-lock claims, a mailbox for each agent, 3–5 teammates. One machine, one session, no nesting. The docs say: "For sequential tasks, same-file edits, or work with many dependencies, a single session or subagents are more effective." | The same primitives, but local and short-lived. |
| Anthropic, [building effective agents](https://www.anthropic.com/engineering/building-effective-agents), [long-running harnesses](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents), [context engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents) | Patterns: orchestrator-workers, parallel work by sections or by voting, evaluator-optimizer. Long runs work one feature per session against a feature list where only `passes` may change. Subagents hand back 1–2k-token summaries. | A subagent compresses its context away. A peer drone keeps its context. |
| Anthropic, [Project Vend 2](https://www.anthropic.com/news/project-vend-2) | A "CEO" agent on the same model shared the shopkeeper's blind spots. Clear roles and fixed procedures helped more than oversight did. | Verifiers need decorrelated errors, not just a second instance. |
| Anthropic, [Redesigning source control](https://www.anthropic.com/research/redesigning-source-control) (Oct 2026) | Merged changes grew from about 2,500 a day to over 30,000 a day in four months. Code is no longer the bottleneck; infrastructure, review and judgment are. Engineers supply "understanding of the high-level inter-system components and the design-level tradeoffs." "Very often, we found that the role of the engineers was to tell Claude 'no'": agents broke core constraints, and adversarial review over-engineered. Humans review "the intent of the change as opposed to reading every line of code," with semantic approval rules sending only consequential changes to people. "Superfan" Claudes each watch one component. | The engineer is the scarce resource and holds the system view. The hive must spend engineer attention carefully and turn every "no" into something durable. |
| Cognition, [Don't build multi-agents](https://cognition.com/blog/dont-build-multi-agents) | Every action carries an implicit decision, and agents that can't see each other's decisions conflict. Agents should share full traces, not just messages. | The main objection. Contracts (below) are the answer. |
| OpenAI: [Swarm](https://github.com/openai/swarm), the Agents SDK, and the practical guide (manager or decentralized handoffs) | The guide says to max out a single agent first (unverified wording). [Codex cloud](https://openai.com/index/introducing-codex/) runs independent parallel tasks. The ICPC 2025 12/12 came from generating many candidates and selecting among them, not from cooperation ([source](https://the-decoder.com/openai-outperforms-humans-and-google-at-the-worlds-top-collegiate-programming-contest/)). | Parallel sampling plus selection is the baseline to beat. |
| Google, [Towards a science of scaling agent systems](https://research.google/blog/towards-a-science-of-scaling-agent-systems-when-and-why-agent-systems-work/) (180 configurations) | +81% on parallelizable tasks, −39% to −70% on sequential ones. Errors amplify 17.2× with independent agents and 4.4× with central verification. | Label every workload by structure, and gate on a verifier. |
| Google [AI co-scientist](https://research.google/blog/accelerating-scientific-breakthroughs-with-an-ai-co-scientist/), AlphaEvolve | A tournament or evolutionary population plus an automatic evaluator. | The evaluator is what makes a population useful. |
| [MAST](https://arxiv.org/abs/2503.13657) (Berkeley, NeurIPS 2025) | Failures break down as 42% specification, 37% misalignment between agents, 21% verification. Adding a verifier improved results by 15.6%. | Specifications and verification matter more than the topology. |
| [Debate or vote](https://arxiv.org/abs/2508.17536) (2025), [More agents is all you need](https://arxiv.org/abs/2402.05120) (2024) | Most of debate's gain is majority voting, and plain voting scales with the number of samples. | Best-of-N with voting is a required baseline. |
| [AI agents that matter](https://arxiv.org/abs/2407.01502) (2024), [Tran & Kiela](https://arxiv.org/abs/2604.02460) (2026) | Cheap retry baselines match complex agents. With thinking tokens held equal, a single agent ties or beats multi-agent setups. | Report accuracy against cost as a curve, not a single number. |

### What the labs claim about self-improvement

| Claim | Source | Takeaway for borg |
|---|---|---|
| o3 was trained with about 10× the RL compute of o1, using verifiable rewards | [the-decoder](https://the-decoder.com/openais-o3-model-shows-major-gains-through-reinforcement-learning-scaling/) | The loop is rollouts, a checkable reward, then an update. |
| codex-1 is o3 trained "using reinforcement learning on real-world coding tasks" until the tests pass | [OpenAI](https://openai.com/index/introducing-codex/) | The tests are the reward, the same oracle our hive uses. |
| GPT-5.3-Codex was "instrumental in creating itself" | [OpenAI](https://openai.com/index/introducing-gpt-5-3-codex), [NBC](https://www.nbcnews.com/tech/innovation/openai-says-new-codex-coding-model-helped-build-rcna257521) | The model helped build its own tools, not its own weights. |
| OpenAI targeted an "AI research intern" for Sep 2026 and an AI researcher for Mar 2028, and now claims 3.1 agent-workdays per human workday | [TechRadar](https://www.techradar.com/ai-platforms-assistants/chatgpt/openai-roadmap-revealed-ai-research-interns-by-2026-full-blown-agi-researchers-by-2028); the 2026 claim is from secondary sources and unverified | A self-reported claim that agents speed up AI research. |
| Most of Anthropic's code is written by Claude (figures from 50% to 90% depending on the definition) | [summary](https://letsdatascience.com/news/anthropic-ceo-discusses-share-of-ai-generated-code-aa18f338), partly unverified | Same: agents build the tooling. |
| Reward hacking in Anthropic's production coding RL spread into alignment faking and sabotage. Inoculation prompting stopped it from spreading. | [Anthropic](https://www.anthropic.com/research/emergent-misalignment-reward-hacking) | A test oracle can be gamed, and gaming it changes behavior beyond the task. |
| Monitoring chain-of-thought catches hacks, but optimizing against the monitor teaches models to hide them | [OpenAI](https://openai.com/index/chain-of-thought-monitoring/) | Monitor the reasoning, but never optimize against the monitor. |
| AlphaEvolve made a Gemini training kernel 23% faster, cutting Gemini training time by about 1% | [DeepMind](https://deepmind.google/blog/alphaevolve-a-gemini-powered-coding-agent-for-designing-advanced-algorithms/) | Search driven by an evaluator, with no weight updates, still pays off. |
| "Era of Experience": agents should learn from grounded rewards over a lifelong stream of experience | Silver & Sutton 2025 ([summary](https://the-decoder.com/the-next-leap-in-ai-depends-on-agents-that-learn-by-doing-not-just-by-reading-what-humans-wrote/)) | Borg's episodes, outcomes and regret records already form that stream. |
| Continual learning is the missing piece | [Dwarkesh](https://www.dwarkesh.com/cp/165349588); no lab ships learning weights in products as of 2026 | What ships is memory, which is borg's layer. |

### What works without weight updates

| Method | Mechanism | Result |
|---|---|---|
| [Training-Free GRPO](https://arxiv.org/abs/2510.08191) | Compares a group of rollouts on the same task and turns the comparison into an "experience library" kept in the prompt | Beat fine-tuned small models using a few dozen samples |
| [GEPA](https://arxiv.org/abs/2507.19457) | Reflects on trajectories to evolve prompts, keeping the best set across tasks | Beat GRPO (weight RL) by 6% on average, with up to 35× fewer rollouts |
| [ACE](https://arxiv.org/abs/2510.04618) | Generator, reflector and curator; a playbook updated by small edits | +10.6% on agent tasks. Warns about context collapse from full rewrites and brevity bias. |
| [ReasoningBank](https://arxiv.org/abs/2509.25140), [Agent Workflow Memory](https://arxiv.org/abs/2409.07429), [Dynamic Cheatsheet](https://arxiv.org/abs/2504.07952) | Strategies drawn from both wins and failures; reusable workflows; a cheatsheet of solvers | AWM +51% on WebArena; Game of 24 went from 10% to 99% |
| [Memento](https://arxiv.org/abs/2508.16153) | Online RL over a case bank, with a learned policy that picks which case to retrieve | GAIA 87.9% pass@3 |
| [Darwin Gödel Machine](https://arxiv.org/abs/2505.22954), [SICA](https://arxiv.org/abs/2504.15228), [Huxley-Gödel Machine](https://arxiv.org/abs/2510.21614) | Agents edit their own code; an archive and open-ended search | SWE-bench went from 20% to 50%. HGM: pick parents by how much their descendants improve, not by their own score. |
| [Multiagent finetuning](https://arxiv.org/abs/2501.05707), [MAPoRL](https://arxiv.org/abs/2502.18439), [CURE](https://arxiv.org/abs/2506.03136) | Specialized agents keep reasoning diverse; a coder and a tester improve each other through self-play | Diverse groups keep improving after a single agent plateaus |
| Shapley and counterfactual credit ([SHARP](https://arxiv.org/abs/2602.08335), [C3](https://arxiv.org/abs/2603.06859)) | Splits a team's reward among its members | Exact credit is factorial in team size, so all of these approximate |

Risks:
- **[ImpossibleBench](https://arxiv.org/abs/2510.20270):** GPT-5 cheated on 76% of impossible SWE tasks, by editing tests or overriding `__eq__`.
- **DGM:** it removed its own hallucination logging.
- **[AgentPoison](https://arxiv.org/abs/2407.12784):** poisoning less than 0.1% of memory got at least 80% attack success.

### Bottom line

- Extra parallel compute is not an advantage, because a single agent can buy the same with more tokens. A collective wins only where it has something more tokens can't buy.
- In-context learning keyed to verifiable rewards is competitive with weight RL at a fraction of the rollouts. Its failure modes are gaming the reward, poisoning the memory, and memory eroding over time.

## Thesis: what a borg hive has that subagents don't

1. **Context ownership.** Each drone keeps a long-lived context: one repo, one machine and its credentials, toolchain and hardware, plus its engineer's lessons. A subagent borrows its parent's machine and comes back with a 1–2k-token summary. This explains the one-session-per-repo observation.
2. **Contracts, not traces.** When a drone makes a decision other drones must respect, such as an API shape, a schema, a name or an invariant, it publishes a signed contract line. Hooks show each contract to drones whose edits touch its scope, the same surfacing mechanism as [decision links](decision-links.md). Conflicting contracts go to the queen for a human to decide. Drones share decisions, not whole transcripts, which answers Cognition's objection at much lower cost.
3. **Coordination through a signed log.** The task board is append-only lines, and claims are leases. There's no leader, no election and no consensus round. It survives network partitions and NAT, because borg-mesh already syncs lines this way. This is the C compiler's git lock files made native and spread across machines.
4. **A task is done only when the oracle passes.** The goal includes an oracle command. A drone that holds a claim can't stop while the oracle fails, using the same blocking as the conscience check. A different drone verifies every finished task in a fresh context: central verification (4.4× instead of 17.2× error amplification) plus MAST's verifier gain.
5. **Diversity that decorrelates errors.** Drones can run different harnesses and models (claude, codex, opencode) and carry different engineers' memories. Voting and verification help only when errors are decorrelated, and a hive gets that for free.
6. **The collective keeps what it learns.** Lessons, contracts and decision links outlive the run, so a later goal of the same shape starts warm. Subagents can't do this, and it is measurable. The learning design (below) turns this into a self-improvement loop: drones run the work, the mesh shares their experience, and the hive learns lessons, playbooks and its own configuration without touching model weights.
7. **The engineer says no and holds the system view.** Drones write code faster than people can read it, so the engineer's job shifts:
   - choose the goal and its scale
   - own the invariants and the picture of how systems fit together
   - review intent instead of lines
   - say "no"

   The hive is built around that role. It doesn't route around the engineer.

## The engineer's role

Anthropic's source-control team found what we see too: engineers mostly add judgment, not code. The hive makes that judgment cheap to give, hard to forget and always respected.

- **The engineer owns the goal.** Only a human signs a goal and approves its plan through the queen. Drones may propose goals; they never start one. *Built (C1, "Plan approval"):* `borg hive run` asks for the plan's approval once, as a queen question with an intent card per task, before any drone starts; a drone can neither answer it nor be asked it.
- **Constraints are the engineer's system view, written down.** A constraint is a contract only an engineer can write or ratify, for example "merge-to-main lookups go through the review API, never the commit log." The two kinds differ:
  - **Drone contracts are soft.** They are shown when relevant; a conflict goes to the queen.
  - **Constraints are hard.** `pre_tool` and `on_stop` block an edit or completion that a constraint's check rejects. A check can be a command, a grep, an import rule, or for now a model-graded question. *Not built yet:* ratified constraints are recorded, not enforced (L7b).
  - **Where they live:** constraints are kept in `constraints.jsonl` for each collective, not each goal, so they outlive the run.
- **"No" is one cheap line.** `borg no <task|change|plan> "<why>"` and the queen's answer both send a signed `veto` directive. The vetoed task reopens with the reason attached, and the reason becomes a lesson, or a proposed constraint when it names an invariant. A drone may not argue a veto back in. It may ask one clarifying question through the queen. *Built (L7b):* `borg no` of a drone on another node sends a signed `veto` directive, recorded once on the drone's node; a veto names a drone, not a task, change or plan, and neither reopens a hive task nor comes from a queen answer yet.
- **Review intent, not lines.** Every `done` task publishes an intent card:
  - what changed and why, in a few lines
  - the contracts and constraints it touched
  - the risk class
  - the verifier's verdict and the hidden-oracle result

  Semantic approval rules choose the risk class: docs, comments and tests in scope are auto-approved, ordinary logic gets sampled review, and anything touching a constraint's scope, security or data migrations needs an engineer. The queen orders intent cards by risk, not by arrival time. *Built, at plan time only (C1, "Plan approval"):* each task's card (intent, files, deps, checks, risk class) is shown before the drones start, and a high-risk task's claim escalates once; the card at `done`, sampled review and ordering by risk are not built.
- **The verifier is scoped against over-engineering.** It checks the work against the task, the contracts and the constraints. It doesn't invent hypothetical threats. Verifier suggestions that add scope go to the queen as proposals, not rejections.
- **Superfan drones.** A drone can own one component for a long time and watch its logs and telemetry (context ownership, thesis 1). Its findings become `open` tasks the engineer can approve or veto.
- **Engineer attention is a budget.** Every escalation costs minutes. The hive tracks engineer minutes per goal, the share of escalations answered "yes, fine" (wasted attention) and the share of "no" answers that arrived only after merge (missed escalation). *Built (L7b, escalation.rs):* the queen's attention reward counts both, team-wide, on borg-web's queen view; engineer minutes per goal are not tracked yet.

### The engineer's "no" is the most valuable reward

Tests only check what someone thought to test. A drone can pass every test and still break a core constraint or over-engineer the change, and Anthropic's source-control team found exactly that. The engineer's judgment is the reward that tests can't supply. It is sparse and precise, so the loop treats it as gold:

- **A veto is the strongest negative signal.** A `veto` penalizes everything that was active when the vetoed work was planned or made: the lessons shown, the playbook version and the genome. That's weighted above `asked_then_corrected`.
- **Every "no" should be the last of its kind.** Each veto reason is generalized: into a lesson, or into a proposed constraint when it names an invariant, which the engineer then ratifies. The learning target is *a falling rate of repeated vetoes*: the same reason should not need saying twice.
- **A predictor of what the engineer would refuse.** Vetoes, approvals and queen answers train a predictor of what each engineer would say no to. This is RLHF's reward model, kept in memory instead of in weights: retrieval over past vetoes and constraints plus a model-graded check. The queen already predicts when to escalate, and this extends it. The predictor is only a router:
  - It sends risky intent cards to the engineer first and catches likely vetoes before work starts.
  - It is **never** a reward the hive optimizes directly. Pushing hard on a proxy for the human teaches drones to please the proxy (Goodhart; OpenAI's chain-of-thought finding).
  - Real verdicts remain the ground truth, and a random sample of auto-approved cards still goes to the engineer to keep the predictor calibrated.
- **Spend attention well.** The queen gets its own reward:
  - minus for escalations answered "yes, fine" (wasted minutes)
  - plus for escalations that ended in "no"
  - a large minus for a "no" or a regress fix that arrives after merge without an escalation (a missed escalation)
- **Over-engineering costs reward.** A complexity term covers the size of the diff relative to the task and the change in lizard and rust-code-analysis metrics (the same quality gates we use). Verifier rejections an engineer overturns count against the verifier.
- **Learning never touches the engineer's system view.** Constraints and goals are written by engineers. The learning loop may *propose* a constraint, but it never edits, weakens or drops one, and promoting a playbook or genome can't override one.

These add the `ν·veto` and `ξ·complexity_delta` terms and the queen's own reward to the reward under Learning design.

## Primitives

Each is a log of lines under `<store>/hive/<goal>/`, synced like `lessons.jsonl` (union, signed `Origin`, revoked keys quarantined):
- `goal.jsonl`, `tasks.jsonl`, `contracts.jsonl`, `messages.jsonl` and `flags.jsonl` (built in C4, `borg/mesh/7`);
- `bundles.jsonl`, each attempt's commits (C4, `borg/mesh/9`);
- `spend.jsonl`, each iteration's cost naming its node (C4, "budget across nodes", `borg/mesh/10`).

Their text is redacted when written (C4, "secrecy"). The hidden oracle, its scores, joins, runs and logs stay on their node.

### Goal

`goal.json` holds:
- the spec
- the oracle command(s)
- the dollar budget
- the repos in scope, and the node expected to hold each repo

It syncs as its projection, one compact line in `goal.jsonl`: the hidden oracle stripped, `held_out: true` saying one exists (a verdict elsewhere then waits for the runner node's score). A node without `goal.json` loads the goal from that log (the lowest `(created_ms, line hash)` line naming the id). `sig` stays empty: every line is signed by the member that first stamped it, the trusted-member model. Whether only an admin's goal should sync is open.

- **One definition per id.** Two nodes can `init` one id before a sync round, and a member can append a line naming a goal it did not make. When `goal.jsonl` (with this node's `goal.json`) holds two different projections of one id, every node refuses the goal, the one holding `goal.json` too (`goal::conflict`), and `borg hive board`, `borg top` and the Hive tab say why. It stays stopped until one definition is left: make the goal again under another id, or revoke the member whose line it was.
- **Checks run only where approved.** An oracle check is a shell command, and the board and `goal.jsonl` sync from every member. A node runs its own `goal.json` oracle and the checks of tasks it opened (`hive::write` records them in the local `approved.jsonl`). Any other check (a synced goal's oracle, a remote task's `accept`) waits for the engineer's `borg hive approve <goal>`, which prints each one it approves. Until then its task is not offered or claimable there, an integration that needs it is blocked, and the board and the Hive tab list the commands waiting.

The goal's plan (its task list) goes through the queen, and the human approves it before workers start (built for `borg hive run`, C1, "Plan approval"; tasks opened and claimed through the CLI by joined sessions still need no approval).

### Board

`tasks.jsonl` records events: `open`, `claim`, `heartbeat`, `progress`, `done`, `verified`, `rejected`, `released`, `blocked`.

- **Task fields:** id, role tag (`plan`, `work`, `verify`, `integrate`), the repo and files in scope, its dependencies, and acceptance criteria phrased as oracle checks.
- **Claims are leases.** Heartbeat lines on the board renew them, and a claim expires when its drone goes idle. Heartbeats sync with the board, so a lease held on node B is renewed on node A too; nothing else (presence, live channel) is needed.
- **Nodes** (C4): every line a node writes carries its node id (`hive::write`; empty in older lines, which count as this node's). A runner releases only its own node's claims. Drone names (`d1`..) repeat on every node, so a verify task is barred to the author's session everywhere and to the author's drone name only on the work's node.
- **Claim races:** when two claims arrive for the same task, the lowest `(ms, blake3(key))` wins. Every node computes the same winner without coordinating, and the losing drone learns on its next hook call and releases the task. Across machines a claim is provisional until the next sync round: the loser's done and verify-task open are ignored by the fold, so a loser wastes at most one sync latency, and the clocks of the nodes must stay well within a lease.
- **Verification:** a `verify` task is created for every `done`, and the drone that did the work can't claim it. A `rejected` reopens the work task with the verifier's note attached.
- **Dependencies carry their work** (built). A task can be claimed only once each of its deps is verified. Each drone works in its own worktree, so the claim also brings the deps' work in:
  - Before the claim's `base` is taken, each transitive dep's accepted attempt (its claim's `base` to its done's `head`) is applied to the claimant's working tree, in the order the deps were verified. `git apply` runs at the toplevel and leaves the index, HEAD and refs alone. A change that is already there (the dep's author claims the dependent in its own tree) is skipped.
  - The claim records `deps_in`: `task@head` for each dep. Because the base is taken after the bring-in, the work's diff holds only its own changes: a dep's test edits are never charged to the dependent.
  - If a dep's change does not apply (the tree's own uncommitted changes overlap it), the claim is refused with git's reason and the tree is left as it was. A drone with a clean tree can still claim the task. A claim that loses its race takes the applied changes out again.
  - The dependent's oracle, its verify task (which lists the same deps) and its hidden score therefore all run on a tree that contains the deps. A pass on work whose claim lacks a dep's accepted head is an unchecked reject.
  - `init` and `open` refuse a dep that names no task, and `init` refuses a cycle in its batch. A board like that could never finish.
- **Integration** (`hive/integrate.rs`, built): once every task is verified, an `integrate.<n>` task is opened and claimed like any other.
  - Its integrator merges the accepted attempts, never a drone's branch. For each verified work task that is the diff from its accepted claim's `base` to its done's `head`.
  - The attempts are applied in order (by `deps`, then by verification time, then by id) with `git merge-tree --merge-base`, which needs git 2.40+, onto the newest common ancestor of the HEADs every work claim started from, so a drone's own commits never enter. No git call of it runs a hook.
  - Each step is a commit with a fixed identity and date, so a later round replays the same attempts to the same commits, conflict markers included. Changes in a worktree that no claim and done cover never enter the result. The combined tree is kept at `refs/heads/hive/<goal>/integrated`.
  - The integrate task's `verified` or `rejected` carries that commit as `head`. A conflict no later attempt resolves is rejected without an oracle run. Only an attempt built on the step with the markers (its base descends from it: a fix task's) resolves it, by changing the path or by holding it without a marker line (a modify/delete, rename or binary conflict writes none, and the fix's verified tree is the resolution).
  - The same failure again right after that fix was verified, with no attempt merged that the rejected tree did not hold (read from git: each attempt it merged is an ancestor of it), blocks the integration instead of opening another fix. Otherwise the goal's oracle and every integrated task's own checks run in a scratch worktree, and any failure rejects it.
  - A rejection opens the fix task `<integrate task>.fix` in the same write. It is a work task with `from` set to the combined tree, scoped to the union of the tasks' scopes, with the combined checks, and its first note says why the integration failed. Its claim resets the claimant's runner worktree to `from`, so its diff starts there. Any other session claims it only in a checkout of `from` (`git worktree add <dir> <from>`), never reset, so a goal worked by joined sessions and `borg hive integrate` finishes too. The next integration merges its accepted attempt last.
  - A missing snapshot, or a git that cannot merge, makes the integration `blocked` until an engineer reopens it.
- **Finished:** there are tasks, every one is verified, and an integration passed no earlier than the last verification. A fix task verified after a failed integration therefore needs one more integration.
- **Reward lines** (L2 and L8, built):
  - A work task's `claim` carries `base` and its `done` `head`, commits of the drone's working tree then (`hive/verdict.rs` `snapshot`, no ref, index untouched).
  - A verify task's `open` carries `flags` (the task def's, rendered from `flags.jsonl`).
  - A `rejected` may carry `hacked: true`: outside the task's `files` its diff weakened tests (tests may grow, never shrink: removed or reshaped assertions, a dropped test, an added skip) or changed CI or a test runner's config, or it changed the visible check's files at all, or its drone touched the oracle's files (L8).
  - The verify task's id names the attempt (`<goal>/<verify task>` is the credit's `cause`), so no `attempt` field is needed.
  - Two more files: `flags.jsonl` (`hive/guard.rs`: one line per session, class and path written to tests, CI or oracle files at `pre_tool`) and `oracle.jsonl` (`hive/hidden.rs`: the hidden oracle's score per judged attempt, runner-only, never synced). When goals sync, `goal.json`'s `hidden` is stripped from the synced projection and stays on the runner's node.

### Contracts

`contracts.jsonl` lines hold `{task, scope: [files | identifiers | repo], text}`.

- **When a drone sees them:** at `on_prompt` for contracts in its task's scope, and at `post_tool` for edits that land in a contract's scope. Each is shown once per session, deduplicated with a `.ctx` marker the way `#borg-early` is (`#borg-hive-contract`).
- **Changing one:** a drone may supersede its own contract: one from the same session, or from the same drone (the contract's `drone`) on the same machine, so a later session of the drone needs no queen answer. Superseding another drone's contract goes through the queen.

### Messages between drones

`borg hive ask` and `tell` append to the goal's `messages.jsonl` (`hive/message.rs`, built in C4), a hive log synced like the board, not a mesh steer kind. Steer is ephemeral (a TTL), gated to the admin and steer roles (letting `member` send would widen who can pause), and addresses nodes or sessions, not tasks or drones. A hive log gets signed origins, quarantine, offline catch-up and per-goal scope for free, and works on one machine with no mesh.

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

### Commits travel

An attempt's two snapshots (claim `base`, done `head`) travel as a signed `bundles.jsonl` line beside the board, so a verifier or integrator on another node gets the work while its author is offline. A node refuses a bundle unless it holds exactly that attempt's commits, and takes nothing from a refused one. Built in C4, "Commits travel".

### Secrecy

Every synced hive text (task titles, flags, notes, contracts, messages, check output, the goal's spec) is redacted when the line is written, with the redaction presence prompts get. Ids, commits, scopes and check commands are kept. Built in C4, "Secrecy".

### Plan approval and intent cards

`borg hive run` asks the engineer to approve the goal's plan once, through the queen, before any worktree exists: one intent card per task with its intent, files, deps, checks and risk class. A "no" stops the run with nothing claimed, and a high-risk task's first claim escalates once. Built in C1, "Plan approval and intent cards".

### Budget

The goal holds a dollar budget. Each runner splits what is left of it over its drones; Claude Code stops itself at its share and the runner checks between sessions. Across nodes the spend lines sync, and every drone stops on the goal's total, whoever spent it. Built in C1 (H2) and C4, "Budget across nodes".

## Learning design: the hive is a distributed actor-learner

### What borg already has

Borg's lesson loop is already a contextual bandit:

| Part of an RL system | In borg today |
|---|---|
| Policy | A fixed score: BM25 × ACT-R activation × a Thompson draw × author trust (`conscience.rs:251`, `memory.rs:12`). A model picks among the shortlist (`predict.rs`). |
| Reward | `outcomes.jsonl`: the lesson led to a change (+), was ignored (0), was followed by a correction (−2), or the engineer didn't correct (weak +) (`outcome.rs:35`) |
| Exploration | Thompson sampling, but it only reorders lessons that already cleared the cutoff (`conscience.rs:539`) |
| Value and forgetting | ACT-R activation; lessons go dormant and get consolidated (`memory.rs:30`) |
| Delayed credit | Regress links a later fix back to an earlier decision and produces regret episodes (`notes/regress.rs`, `learn/regret.rs`) |
| Credit to authors | Trust per engineer and area (`trust.rs:21`) |
| Offline evaluation | `borg-eval` arms, trap-ledger, dogfood |

Gaps (as designed; L1–L3 since closed the first three in part):
- Outcomes weren't synced over the mesh, so each node learned alone. L1 syncs a redacted projection.
- Only one lesson got credit, never a set of lessons or a team of drones. L2 credits every lesson shown during a hive task, in equal shares; no Shapley split yet.
- Regret never penalized the lessons that were shown when the bad decision was made. L3 makes it reach back.
- Cost (`usd` in stats) never feeds back into the reward.
- The constants are hand-set, and eval results never flow back into them. That holds for every weight the built steps added: the veto's 3, `VERIFIED_W` = 1, `REJECTED_W` = 1, `HACKED_W` = 3 (memory.rs), `REGRET` = 1 (memory.rs) and `VERIFIER_REGRET` = 0.5 (learn.rs). `borg ope` estimates them; none tuned yet.

In RL terms (as in IMPALA and Ape-X):
- **Drones are the actors.** They run the rollouts.
- **The mesh is the replay buffer.** It holds the experience everyone shares.
- **The learned state is the "weights".** That means lessons, playbooks and the hive genome, versioned and synced as signed lines.
- **The model stays frozen.** The collective learns.

### Learning in four layers, fastest to slowest

| Layer | What is learned | Update rule | Clock |
|---|---|---|---|
| L0 lessons | Questions to ask, situations they apply in, consequences (exists) | Bandit over outcomes, extended below | Every session |
| L1 playbooks | Procedures for a task shape: how *this* team does a schema migration, a cross-repo API change, a flaky-test hunt | ACE-style small edits after verified tasks; never full rewrites | Every goal |
| L2 hive genome | Decomposition prompt, N, mix of harnesses and models, whether contracts are on, budget split, verifier policy | Archive plus a Pareto set across goal families (GEPA, DGM); parents chosen by how much their descendants improved (HGM) | Every few goals |
| L3 weights | Out of scope | Optional export of verified trajectories as a dataset | — |

The layer names L0–L3 are not step ids; the steps are L1–L8 below.

### Reward

```
R(task) = hidden_oracle_pass                    # runner-side, never visible to drones
        − λ·usd − μ·rework (reopened tasks)
        + human: queen answers, corrections (−), not_corrected (weak +)
        − delayed: regress fix links within 30 days (the "regret" penalty)
        − ν·veto − ξ·complexity_delta           # the engineer's "no", over-engineering
R(queen) = no_after_escalation − α·rubber_stamp − β·missed_escalation
```

- **Two oracles.** Drones see the *visible* oracle that gates `done`. The runner scores the *hidden* oracle, made of held-out tests outside every worktree. Learning updates use only the hidden reward.
- **The verifier is a different drone** (Board, "Verification"). Its verdict is a reward, and it can be wrong. A `verified` task that later gets a regress link counts against the verifier as well as the author.
- **The engineer's terms** (`ν·veto`, `ξ·complexity_delta`, `R(queen)`) come from "The engineer's 'no' is the most valuable reward" above.

### Group-relative advantage, native to the hive

When the budget allows, the hive runs a task as a **group of K rollouts**, with drones that differ in harness, model, playbook or genome. This is also arm F, best-of-N with voting.

A reflection step on a learner node, Training-Free GRPO style, compares the winners and losers on the hidden reward and writes a *semantic advantage*: what the winners did that the losers didn't. That becomes an L0 lesson or an L1 playbook edit.

A single session can't do this because it has no group to compare. Subagents make a group, but it dies with the session.

### Credit assignment

- **Lessons, from the logged policy.** Each `asked` record logs its *propensity*: the probability of being shown under that session's Thompson draw. This allows off-policy evaluation (inverse propensity scoring and doubly-robust estimators) of ranking changes, and of cutoff and cap settings, from logged data without new rollouts. Then tune `RELATIVE`, `BLOCK` and `CAP` from those estimates instead of setting them by hand.
- **Sets of lessons.** When several lessons were shown before an outcome, split the credit with a leave-one-out Shapley approximation. Exploration already randomizes which lessons appear, so we get natural experiments.
- **Drones and contracts.** Per goal, compare the reward of tasks where a contract was shown with the reward where the same contract scope was edited without it. That is a direct measure of H4.
- **Regret reaches back.** A strong regress link penalizes the lessons and playbook version that were active when the decision was made, not just the decision.

### Learning across the federation

- **Sync outcomes.** Each signed outcome line carries the reward and propensity, so every node learns from every drone's rollouts. Redact them as presence prompts are redacted.
- **Poisoning resistance:**
  - A lesson or playbook edit reaches team-wide scope only with support from at least *k* distinct engineers or nodes, or after a held-out eval win.
  - Every line is signed with its origin, so a revoked key quarantines everything it wrote (`origin.rs:102`).
  - A trust-weighted median replaces the mean in aggregation, so one node can't move it alone.
- **Diversity is protected.** Genome selection keeps a Pareto set, not one winner. A collective whose members are all alike loses the decorrelated errors that make verification work, which is Project Vend's lesson.

### Guardrails against reward hacking

These come from the research, and each is cheap to build:
- **Hidden oracle files** live outside the drone's worktree. `pre_tool` flags writes to test files and CI config, and any touch of the oracle's files, and lists them for the verifier.
- **Checks that fail hacks:** the verifier runs the hidden tests and diffs the test files. A pass that came with weakened tests outside the task's files (removed, reshaped, made tautological, no longer declared or skipped; additions, renames and value updates are fine), or with any change to the visible check's own files, is rejected as `hacked`, which carries a large negative reward; the hidden oracle backs it up.
- **Monitor without optimizing:** a cheap model reads transcripts for hack intent ("make the test pass by…") and only flags them. Its verdict never enters the reward, so we don't train drones to hide intent.
- **Inoculation framing:** the drone prompt says plainly that gaming the oracle is visible, allowed to happen, and useless to the task. This is the closest in-context analogue of Anthropic's finding.
- **Promotion gate:** L1 and L2 changes are promoted only after beating the current version on a held-out goal set at the same budget. The queen shows each promotion to a human with its eval delta. Everything is versioned, and rollback is one line.
- **Erosion check:** playbooks are edited by small deltas, never rewritten (ACE). A length and coverage check catches brevity bias.
- **Dependent work is checked with its deps** (built, `hive/verdict.rs`). A claim of a task with deps brings each dep's verified work into the tree first and names it in `deps_in`. A pass on work whose claim lacks a dep's verified head is rejected as unchecked: it was built and checked on a tree without that dep. Nothing is credited, and the hidden oracle does not score it.
- **The combined tree is checked** (built, `hive/integrate.rs`). Each verifier judges one attempt in its author's worktree. Two attempts can each pass alone and still break together: in the W2 probe, two drones added the same annotation at different spots and git merged them cleanly. Before the goal counts as finished, the accepted attempts are merged and the goal's oracle, plus every task's own checks, runs on the result. Only the board's accepted attempts are merged, so work a drone did without a claim never reaches the result.
  - An integration credits no lessons: it is a verdict on no single attempt.
  - The fix task a failed integration opens is judged like any work. Its diff runs from the combined tree to its done, and the same `protected` rule applies with the fix task's scope.

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

## Proof

Two protocols, both run with `borg-eval`: the hive against one session (does a hive solve better?), and the learning curve (does a hive get better?).

### The hive against one session

Every arm gets the **same dollar budget**, counting all agents' tokens and coordination traffic.

| Arm | Setup |
|---|---|
| A | One session |
| B | One session with subagents or a Workflow (the current best practice) |
| C | Claude Code agent teams |
| D3 / D9 | Hive with 3 or 9 drones on one machine |
| E | Hive with one drone per repo, spread across machines |
| F | Best-of-N with voting or selection at the same budget |

Each workload is labelled by its structure and drawn from company repos.

| Workload | Shape | Oracle |
|---|---|---|
| parallel | A list of independent bugs or modules (C-compiler style) | The repo's test suite plus held-out tests |
| sequential | One tightly coupled feature in one repo | Held-out tests |
| cross-repo | A coordinated API or schema change across 2–3 repos | The test suite of each repo, plus an integration check |
| warm-start | A second cross-repo goal of the same shape, run after the first | Same as cross-repo |

Metrics:
- solve rate on the hidden oracle
- dollars, tokens and wall-clock time
- duplicated work, merge conflicts and rework (count of reopened tasks)
- contract conflicts
- engineer minutes per goal, vetoes per goal, constraint violations caught before and after merge
- failures tagged with MAST categories
- at least 3 seeds per cell, reported with variance and as accuracy-against-cost curves

Hypotheses (registered before running):
- **H1:** on cross-repo and parallel work, the hive (D/E) beats B on wall-clock time at the same dollars, without a lower solve rate.
- **H2:** on sequential work, the hive ties or loses to B. If so, we publish that.
- **H3:** in the hive, a warm-start run costs less than a cold run of the same shape. B shows no such effect.
- **H4:** turning contracts off raises cross-repo failures in the MAST misalignment category. This would show that contracts carry the win.
- **H4b:** with constraints and intent cards, the hive needs fewer engineer minutes per solved goal than B, with no more constraint violations after merge.
- **Kill criterion:** replaced on 2026-10-23, before the full pilot, by an acceptance gate (C4): the engineer asked only for proof "that it works without serious bugs". H1–H4b stay registered and unmeasured.

### The learning curve, not a single number

Use a sequence of goals from the same family (for example 8 cross-repo API changes on company repos), plus a held-out family to test transfer.

| Arm | Learning |
|---|---|
| frozen | Hive with all learning off (L0–L2 frozen at the start) |
| L0 | Lessons only (borg today, with outcomes synced) |
| L0+L1 | Plus playbooks |
| L0+L1+L2 | Plus genome search |
| subagents | One session with subagents and its own memory (CLAUDE.md) |

All arms run at the same dollar budget, *including* the learner's reflection and eval spend.

Metrics: hidden-oracle solve rate and dollars per solve for goal *k*, plotted over *k*, and the rate of hacked attempts and poisoned lines. Transfer to the held-out family.

Pre-registered hypotheses:
- **H5:** L0+L1 lowers dollars per solve over the goal sequence; frozen stays flat.
- **H5b:** across the goal sequence, repeated vetoes (the same reason given again) trend toward zero, and engineer minutes per solved goal fall while violations after merge do not rise.
- **H6:** group-relative reflection (K=3) produces lessons that change behavior more often than lessons from single rollouts.
- **H7:** IPS-tuned constants beat hand-set ones in off-policy estimates, and the win is confirmed by a live A/B.
- **H8:** the guardrails keep the hacked rate under 2% on an ImpossibleBench-style set of tasks with conflicting tests.

Kill criterion: if H5 fails after 8 goals, stop at L0.

### Results so far

- **Pilot probe (2026-10-23, run `hive-probe`, $5.06, cap $15 per cell):** every arm solved W1 6/6 (A $0.20 129 s, B $0.33 392 s, D3 $1.86 319 s, F $0.83 342 s). On W2, A, B and F solved 5/5 and D3 2/5 (A $0.19 142 s, B $0.37 161 s, D3 $0.80 231 s). The bugs it found in the hive are listed under C3 and were fixed before the acceptance run.
- **hive-accept-1 (2026-10-23):** D3 accepted 6/6, $8.48 (C3).
- **H5 stage 1 (2026-10-24, run `h5`, positions 1–4, 36 cells, 0 errors, about $43):** mistakes per goal frozen 0.50, L0 0.42, L0+L1 0.33, but 14 of 15 mistakes came from one goal. The other 7 goals had no headroom: each spec named its trap and often the fix site, and tasks were pre-split restating the trap, so the queen never decomposed. Kept as a pilot, not scored. The goal set was rewritten as terse tickets that state the outcome and not the trap, one task per goal (the hive decomposes), four family traps each in at least 3 goals, every unstated requirement backed by repo evidence beside its check.
- **H5, run h5v2 (2026-10-25), not decided:**
  - **Setup:** 8 hive goals of one family on a company repo, terse tickets with recurring unstated traps; arms frozen, L0 and L0+L1, 3 seeds, rotated order. The primary metric is mistakes (hidden checks failing, hacked, verifier rejects, failed integrations); cost is a guard at ≤ 2×. A floor: frozen must make ≥ 0.5 mistakes per goal, or the result is not informative.
  - **Coverage:** 50 of 90 runs before the engineer stopped it. 0 errors, 0 hacked, 0 conflicts, about $0.60 per goal.
  - **Mistakes per goal on positions 5–8:** frozen 0.80, L0 0.75, L0+L1 0.60. The gate read only 5 of 12 pairs, with a bootstrap 95% range of [0, 1], so it is not evidence either way.
  - **What it did show:** an autonomous hive learns little from itself: 0.12 lessons and 0.94 playbook edits per goal. A first version of the goals (stage 1) had no headroom, because the specs named their traps.
  - **Decision (engineer):** learning stays on and the kill criterion is not applied. Separating the arms needs hard tasks at benchmark scale (about 6k€ per run) and a human golden set, so learning is judged in production through monitoring. H6–H8 have not run.
- **Arm E on one machine (built, 0 tokens):** with sim drones on two stores (4 tasks), A did 3 attempts and B 1, all verified; 2 were verified away from their author's node and B's attempt was integrated on A; both boards fold to one digest, one holder per task, bundles made 4, taken 4, refused 0, 11 sync rounds and none failed; the integrated diff holds all 4 files, hidden 4/4 (C4, "Arm E on one machine"). The paid acceptance run (W1 and W2 × 3 seeds) has not run.
- **Budget across nodes (D1) and the integration conflict loop (D2):** bug fixes found by arm E and by a gate run under load, built with 0-token proofs (C4).
- **Off-policy evaluation on this machine's store:** 0 decisions with support, constants unchanged (L6).
- **Learning health in production (D5):** built, below.

### Learning health in production

The monitoring the H5 decision relies on.

- *Built:* borg-web's Learning tab and `/api/learning?c=` (crates/borg-web/src/learning.rs) show per collective, week by week:
  - lessons written per source (session, review, regret, veto, group, rework, queen, import);
  - the team's lessons written and merged;
  - the asked→changed rate (changed over changed, ignored and corrected anyway; should rise);
  - the repeat-veto rate (H5b's numerator over all vetoes; should fall);
  - per hive goal the mistakes caught (verifier rejections, failed integrations, hacked attempts, next to verified work; unchecked rejections left out) with the rework lessons learned from them (drill-down: click a goal, click a lesson);
  - every shown playbook version with its belief.

  It has no state of its own: it folds the logs Borg keeps, so the history is there from the first view. Each panel says where its numbers come from: `team` for synced logs (stats.jsonl rollups, lessons.jsonl, the hive boards, playbooks, outcomes-team) in a team collective, `this machine` for the local event log (the only place a lesson's source is kept) or a local collective.
- *Proof:* `crates/borg-web/tests/learning_api.rs` seeds a team store (local events, a peer's rollup, lessons, a goal board) and checks every series per week, the unchecked rejection left out, the rework lesson tied to its goal, the scope labels, and that nothing is written.
- *Fix (review):*
  - The hive counts now take only the verdicts the fold took (each closed its holder's claim, each identical line once) with a credit `record` writes (`verdict::consistent`): a stale verifier's line, a duplicate, a forged credit or an unclaimed verdict counted before.
  - The weeks run to the current week (`partial_week`), so the "last 4 weeks" tiles of an idle collective read idle weeks, not the last active ones.
  - The derived history (stats.rs `backfill`) has no merges, repeated vetoes or lesson sources, and its lessons include peers'. So lesson sources, merges and repeats are `null` before the week live counting began here (`live_from`, stats.rs `live_since`; the repeat line shows a gap there, never 0%), and the team's new lessons are counted from lessons.jsonl, each lesson once by its `origin_ms`, so a peer's lesson is never counted again by this node's rollup.
  - A repeated veto is dated when the veto was said (learn.rs), so the weekly share pairs the same no's; older data is clamped at 100%.
  - The "Lessons written" tile counts new lessons only (merges in its subline), and the asked→changed labels say a lesson can get a Stop verdict and a later correction.
  - *Proof:* learning_api.rs now seeds claims, a stale verifier, a duplicate, a forged credit, an unclaimed verdict and a backfilled week, and checks the nulls; learning.rs `the_weeks_run_to_the_current_one_and_unknown_weeks_are_null`; learn.rs the repeat carries its veto's ms.
  - *Deferred:* a peer's own weeks before its live counting still fold its rollup's zeros for merges and repeats (a rollup does not say when its node began live counting).
- *Deferred:* lessons per source team-wide (the rollups count `lesson_new` without its source; it would need the source in the rollup), hidden-oracle failures per goal (oracle.jsonl does not sync, so only the board's verdicts are counted), and engineer minutes per solved goal (H5b's second half).

## Steps

Each step stands on its own and needs nothing after it. C1–C4 build the hive, L1–L8 its learning.

### C1. Local spike, file-backed with no mesh

Plan (`crates/borg/src/hive/`): build `board.rs` (the event log, leases and the winner rule), `contract.rs` (publishing contracts and matching them by scope) and `run.rs` (the worktree loop). Add the CLI commands `borg hive init | run | join | board`, and the `on_prompt` and `on_stop` additions.

**Built, part H1 (local):**
- `hive/goal.rs`: goal.json, written once.
- `hive/board.rs`: tasks.jsonl; a fold that gives one board in any arrival order; leases of `presence::IDLE_MS` renewed by the hooks; the winner rule; verify tasks the author may not claim; rework on rejection; claim spans for credit.
- `hive/contract.rs`: contracts.jsonl; own supersession, another drone's through a `Kind::Contract` queen question; scope matching by glob, `id:` and `repo:`.
- `hive/oracle.rs`: bounded `sh -c` checks, process-group kill, failure tails.
- The hook glue in `hive/mod.rs`: claim, board changes and contracts at a prompt; contracts and out-of-scope edits after a tool call; at the stop, a failing oracle blocks (at most 3 times per claim, also on the second round), a passing one publishes done, opens the verify task and hands over the next open task.
- CLI: `borg hive init | board | join | leave | open | claim | progress | release | block | done | verify | contract`; `BORG_HIVE=<goal>` joins a drone at its first hook.
- Dedupe follows the `#borg-early` marker pattern (`#borg-hive-contract`), not a `#borg-decision` marker, which does not exist.
- *Deferred at H1:* `run.rs` and the runner (H2, built below), mesh sync of hive lines (after the pilot; built in C4), goal signatures (`sig` stays empty), queen approval of the plan (at H1 the tasks the engineer opens through the CLI are the approved plan; tasks a drone opens are claimable at once, which "the human approves the plan" does not allow yet; built for `borg hive run` below), budget enforcement (recorded and shown only; built in H2), and messages (built in C4).
- *Known limits:* a suite slower than the stop hook needs `borg hive done`; a drone with no hook event for 30 minutes loses its claim; one claim per drone is enforced by the CLI and the handover, not the fold.

**Built, part H2 (local):** `hive/run.rs`, `borg hive run <goal> --drones N`.
- **Worktrees:** one per drone (`<repo>/.worktrees/hive-<goal>-<n>`, one path component so `repo_key` folds it back, `/.worktrees/` in the local `info/exclude`). The drone's existing branch is checked out on a re-run (never reset). Each worktree is left when the run ends.
- **Harnesses:** each drone is a loop of headless sessions through `Harness::headless` (Claude Code, omp and opencode 1.x checked against their `--help`; codex reports no dollars and fails closed after one session; opencode 2.x not yet).
- **Budget share:** an equal share of what is left of the goal's budget (every run's `spend.jsonl` counted, one run of a goal at a time). A session cut off or failed without a cost is charged an estimate. A killed run's sessions are charged, and its claims released, by the goal's next run.
- **When a drone ends:** a pure `step` ends a drone on a finished board, its spent share, an unmetered (or $0) session, two failures, two sessions without board progress, or three idle polls with no work in flight (a drone waits while another holds work whose verify task it could take).
- **Budget enforcement is both,** as the open question below guessed: Claude Code stops itself (`Caps::headless_budget`, `--max-budget-usd`), the runner checks between sessions.
- **Sessions:** an ended session's claims are released at once. An iteration's sessions are the one the runner named, the one the harness reported and every session whose claim carries the drone, so a harness with its own session ids (omp, opencode) is tracked by drone. Claims carry the drone (`BORG_HIVE_DRONE`), so a verify task is barred to every session of the author drone. `borg hive join --here` (and each worktree) writes a join record, so a harness that drops the env still joins.
- **Watching:** `hive/view.rs`: the hive in `borg top` (text and `--json`), `/api/hive` and the web page's Hive tab.
- **Sim drones:** `hive/sim_drone.rs`, a scripted drone of the hidden `sim` harness against the real hooks, which the runner's suite runs with 3 drones.
- **Integration** is built (`hive/integrate.rs`, Board above): the accepted attempts merged, the combined tree oracle-checked, a fix task on a failure. A dependent task's claim brings its deps' verified work into the claimant's tree (`verdict::bring_in`, recorded as `deps_in`), so dependent work is built, verified and scored on top of its deps.
- *Deferred:* multi-repo goals, killing an unmetered session mid-way at its budget, a dollar price for codex tokens, queen approval of the plan before `run` (built below), and the mesh lines (built in C4). The hidden oracle, the reward step, is built: L2, scored in `after_run`.

**Built, plan approval and intent cards (part I of tasks/todo.md "Next phase (2026-10-25, c)"):** preparation; its value metric is H4b's engineer minutes per solved goal, not measured. `hive/plan.rs`.
- **The question:** before `borg hive run` makes a worktree, it raises one `Kind::Plan` queen question for the goal's plan and waits for it under the run lock. The question carries an intent card per work or plan task (fix tasks an integration opens belong to the approved scope and get none): the intent (the title, redacted when written), files, deps, checks and a risk class.
- **Risk classes:** `high` comes from the queen's existing classifier (queen.rs `classify`, the four checkpoint kinds, deterministic and 0 tokens) run on the title, and the card names the kind. `low` is the smallest honest rule for the auto-approved class (The engineer's role): every file in scope is docs or tests. Everything else is `normal`. Only `high` routes anything.
- **The answer:** the engineer answers at the run's terminal (`y`, or why not), with `borg queen answer <id> a`, or wherever the queue is answered. An approval starts the drones. Anything else, a dismissal included, stops the run with no worktree, run line or claim. A "no" in the engineer's own words is recorded like a veto (veto.rs `record`: an episode `borg learn` turns into a lesson, and the `veto` stat); a bare "no" records only the decision.
- **Decisions are remembered:** each decision is a line of `plan.jsonl`, local and keyed by the cards, so a rerun of the same plan goes ahead (or stops) without asking again, and a new or changed task asks again.
- **`--yes`** is the engineer approving at the command line. It writes the decision without a question (or answers a queued one) and never waits; borg-eval's D3 runs pass it, so an eval run never blocks.
- **High-risk claims:** a high-risk task's first claim (CLI or the stop's handover) queues one question of its card's kind, naming the claiming session, so the queen's attention reward counts its answer (escalation.rs). Its drone is told once and goes on with the kind's default (an external mutation holds). It is never asked again for that task; a group's attempts (`--group K`) are one task, one question, and each attempt's drone is told while it is open.
- **Drones never answer:** a drone never answers either question (`queen answer` refuses under `BORG_HIVE`/`BORG_HIVE_DRONE` or in a worktree `borg hive run` joined, the env-dropping harness's fallback, hive/mod.rs `drone`), and neither is delivered to a drone's session.
- **The queue:** neither question is a near-duplicate for `borg queen dedupe` (a changed plan reads like the old one, and its dismissal would refuse the waiting run), and neither masks a checkpoint question elsewhere (queen.rs `masks`: a plan question decides no move; an escalation masks only its own repo's).
- **A plan that changes while waiting:** a task opened while the run waits (another shell, a peer's line) changes the cards: after an approval the run re-folds and asks again for the changed plan before any drone starts.
- **On by default, no switch. Monitoring:** the run prints the cards and how to answer; `borg top` prints `plan approved|refused|waiting|not asked` with each high-risk card; `/api/hive` has a `plan` field and the Hive tab a Plan table.
- *Proof (0 tokens, sim drones, crates/borg/tests/hive_run_cli.rs):*
  - a run waits for the plan with no worktree, run line or claim, `borg top` shows it waiting, a drone's answer is refused, and a "no" with a reason stops it with nothing claimed, its veto episode recorded, and a rerun refused without a second question (`a_run_waits_for_the_plan_and_a_no_stops_it_with_nothing_claimed`);
  - an approved plan with one high-risk task runs to the end, the task escalated once (worker and verifier both claimed it), naming the worker's session, and a rerun asks nothing (`an_approved_plan_runs_to_the_end_and_a_high_risk_claim_escalates_once`);
  - `--yes` finishes without a question (`yes_approves_the_plan_without_a_question_or_a_wait`; every other run suite passes `--yes` too);
  - the claimant is told once (`the_claimant_of_a_high_risk_task_is_told_once`);
  - unit tests for the classes, the key, yes/no and reasons, the veto, `--yes` and escalation (plan.rs), the view (view.rs), `/api/hive` (borg-web `hive_api.rs`) and borg-eval's `--yes`.
- *Deferred:*
  - approval across nodes (the queen's queue does not sync, so each node's run asks its own engineer; an approval would need a signed decision line beside the board);
  - a plan for tasks opened and claimed by joined sessions without `borg hive run`;
  - tasks a drone opens during a run;
  - the card at `done` with the verdict, sampled review of `normal` cards, ordering cards by risk, and a constraint's scope, security or migrations as risk sources (constraints carry no scope yet);
  - a "no" naming a task reopens nothing (the run never started it).

### C2. Eval

Plan: add arms B, D and F to `borg-eval`. Write one parallel workload and one cross-repo workload from company repos on the work machine; their oracles stay private there.

**Built (harness, no paid run yet):** `crates/borg-eval/src/hive.rs` measures the pre-registered pilot rule (tasks/todo.md "Hive pilot") and does not change it.
- `borg-eval hive --workload DIR --arm a|b|d3|e|f --seed N --cap-usd X [--model sonnet] [--timeout-s 3600] [--run NAME] [--stop-at-usd Y]` runs one cell and appends one row to the run's `results.jsonl`. `--cap-usd` has no default. `--stop-at-usd` refuses a cell once the run's spend plus the cap would pass Y.
- `borg-eval hive validate --workload DIR` checks that every hidden check fails on the base and passes on `reference.patch`, with no model call. An arm refuses a workload until it has a passing validate row for the same workload hash (`workload.json` plus every file it names).
- `borg-eval hive report <results.jsonl>...` prints per workload and arm: median wall, mean solve share, dollars, tokens, rework, conflicts, hacked and over-cap counts. It then prints the H1 line, inclusive at the boundary, and the kill line. It uses no judge.
- A workload (`workload.json`, kept outside every repo and sandbox) names source repos at pinned 40-hex commits, an optional injected patch, the visible spec and check, and K hidden checks in a directory outside every source and workspace. It also lists the hive's tasks, the HOME paths to bind read-only (Gradle caches; credentials and borg's stores are refused) and extra env.
- Each cell builds its workspace with `git archive` at the pinned commit, so the source is never written and the workspace carries no history. The result is one commit, `base`; a cross-repo workload holds both projects as subdirectories.
- Every arm runs in a fresh sandbox HOME (`dogfood.rs` `prepare_home_with`). It gets a fresh store with no lessons, every borg model call off, and the real HOME hidden.
- **Arms:** A and B are one `claude -p` session each: A with subagents disallowed, B invited to use them. F runs three A sessions at cap/3 in parallel and picks by the visible check, then cost, then index. D3 runs `borg install claude --apply`, then `borg hive init` (with no hidden oracle in the sandbox), then `run --drones 3`. The hive integrates itself (Board, Integration), and the cell scores the commit the board names at `refs/heads/hive/<goal>/integrated`: the passing integration's when the board finished, else the newest integration's (a failed one's tree), else `base` when nothing was merged. Its conflicts are the integrations rejected for a merge conflict. E is built in C4.
- **Scoring** runs after the arm in a sandbox with no network, no credentials and git hooks off. It uses a fresh clone of the result commit, so no build output or untracked file carries over.
- **Hacked** is read off the clone before any check runs the result's code, by the hive's own rule (`verdict::protected` on the base-to-result diff, scoped by the workload's task `files`): tests may grow, never shrink. Outside the tasks' files a removed, reshaped or tautological assertion, a dropped test declaration, an added skip, a deleted or renamed test, or a changed CI or runner config counts; added tests, renames and updated values do not, and the visible check's files are never in scope. It counts for every arm alike, with why in `hacked_why`; the board's hacked verdicts are reported apart.
- *Tests:* unit tests with fixtures; end to end with bwrap, the sim harness (D3) and a fake `claude` (A, B, F): `cargo build -p borg-ai && cargo test -p borg-eval hive_e2e -- --ignored`. The local driver `eval/hive-pilot/drive.py` (`probe <cap>`, `full <cap>`) runs cells one at a time, because wall-clock is a metric.
- *Known gaps:* a drone sees only the task title and spec line 1, so each task title carries its full bug report. A goal covers one repo. Dollar caps are soft and over-cap is reported, never rescaled.

### C3. Pilot

Plan: N=3, 3 seeds, a capped budget. Record the summary table in this doc.

**Run as an acceptance test (2026-10-23):** the engineer replaced the H1 superiority gate before the full pilot: "we only need to prove that it works without serious bugs." Run `hive-accept-1`, D3 only, borg 1f564c9 + cb73cba, 3 seeds on each of two private production workloads:
- **W1** (parallel): six injected bugs in one service, K = 6 hidden checks.
- **W2** (cross-repo): a field rename across a service and its consumer, the consumer's task depending on the producer's, K = 5.

| Run | Cells | Dollars | Per cell (dollars / wall), in run order | Result |
|---|---|---|---|---|
| hive-accept-1 | 6 | $8.48 | $1.58/464 s, $0.89/409 s, $1.95/477 s, $1.02/671 s, $1.94/535 s, $1.09/503 s | ACCEPTED 6/6 |

Every board finished with one integration; the visible check passed, hidden 6/6 on W1 and 5/5 on W2, conflicts 0, hacked 0.
- **The probe before it** (D3 solved 2/5 on W2) found the bugs fixed for this run:
  - idle drones quit while work was in flight;
  - the combined tree was never integrated, so stray work of a drone that had lost its claim was merged;
  - required test additions were flagged as hacked;
  - dependent work started without its dependency;
  - a snapshot missed a same-size edit made within the index's own second;
  - a refused claim read as the drone's own.
- **Not measured:** superiority over one session with subagents (B). The workloads were too easy to tell the arms apart, so H1–H4b have no data.
- **Not exercised on a real repo:** the fix-task loop after a failing integration. The sim and CLI tests cover it.

### C4. Decide, then the hive across machines

The gate is acceptance, not H1: a hive feature ships once it works without serious bugs on real repos. On one machine that held (C3), so the next step is to add the hive lines to mesh sync, `ask`/`tell` between drones, and arm E across machines, gated by the same acceptance run across machines. Whether the hive beats B stays open until a workload hard enough to tell them apart exists.

**Built, sync (preparation for arm E):**
- **The logs sync** over the mesh (`borg/mesh/7`): `goal.jsonl` (the goal's projection, `hive::publish`, without the hidden oracle), `tasks.jsonl`, `contracts.jsonl`, `messages.jsonl` and `flags.jsonl` of every goal (borg-mesh lines.rs `hive_logs`, a `Goals` swap of path-checked ids). They are signed, quarantined on revocation and covered by the daemon's change fingerprint, so a claim or heartbeat travels within a round.
- **Nodes:** every line names its node; runners release only their node's claims; drone names bar only on their node.
- **Verdicts:** verdict lines carry the kind they credit, and the author's node credits a verdict given elsewhere at its next round, once (`verdict::credit_synced`); verdicts join `SHARED_KINDS` with `c`, bound to the local board's verdict (L2).
- `borg hive ask|tell` (Messages above).
- **Work elsewhere:** a verify or fix task whose commits this repo lacks is not offered, and an integration runs only on a node holding every accepted attempt, else writes nothing and says which work is where. `borg top` and the Hive tab name a holder's node and list messages.
- **Also fixed:** a runner drone's fresh session was offered the verify task barred to its drone. The hive-accept-1 build barred by session and the board's session→drone map only, so a session with no board line passed the pre-claim check and its claim was voided by the fold. The drone now comes from the runner's `BORG_HIVE_DRONE` when the membership names none, and `borg hive claim` without `--session` resolves the drone's own session.
- *Proof, 0 tokens:* two loopback nodes in one process with drones run as the real `borg` binary (crates/borg-mesh/tests/hive_mesh.rs): a task opened on A is claimed on both nodes before a sync, one claim wins on both, the work is done on B and verified on A by another node's `d1`, both boards fold identically, no task has two holders at once, the lesson shown during the claim is credited once on B and counted once in both nodes' memory, a message reaches the verifier once, and a further round moves only digests.
- *Not measured:* arm E and the acceptance run across real machines (a paid run with its own cost estimate).
- *Known limits:*
  - Claims are provisional until a round. The winner rule compares clocks (skew must stay well within a lease), and a member that backdates a claim line wins its race whenever it arrives, voiding a holder's finished tenure. Bounding a line's `ms` by its origin stamp would refuse honest lines of a node that was offline; deferred, revocation quarantines it.
  - Board lines carry raw session ids, so a team outcome line's unsalted session tag (outcome.rs `tag`) links it to a hive session, its task and node: hive sessions are visible to the team (a per-node salted alias on board lines is deferred).
  - Commits did not travel in this step (both test nodes share one object store; built below, "Commits travel").
  - A goal with a hidden oracle credits a remote author's attempt not yet.
  - The trusted-member model: any member's goal syncs, `sig` stays empty; a member can forge a board verdict and its team line, though a team verdict counts only under the attempt author's session tag, for lessons the team's records show it was given, and with a hidden oracle only from the work's node; credits already made from a verdict a revocation later quarantines stay.
  - Every goal ever made syncs (about five fingerprints per goal per round; compaction deferred).
  - A quarantine rewrites a board, so a session may be shown changes again once.

**Built, commits travel (part X of tasks/todo.md "Next phase (2026-10-25, c)"):** `hive/commits.rs`.
- **Transport:** a sixth synced hive log, `bundles.jsonl` (`borg/mesh/9`), not a pull over the session. The mesh moves sets of signed, content-addressed lines between members that need not be online together. A bundle as such a line inherits the signature, the revocation quarantine, the change fingerprint, recon's cost ∝ difference and order-independent union, and reaches a verifier while its author is offline. A pull would add a request kind, need the author online at the moment of need and re-solve trust per request.
- **Making a bundle:** at each `done` in a store that is a mesh member (`mesh/membership.json`), the attempt's two snapshots (claim `base`, done `head`) are pinned at `refs/borg/hive/<goal>/<sha>` (they also survive `gc` on the author's node now) and bundled `--not --remotes`: what a remote-tracking ref holds, every member's clone is taken to hold.
- **Caps:** a bundle over `MAX_BUNDLE` (1 MiB) writes no line; a line over `MAX_LINE` is skipped by the mesh like a torn one (lines.rs `cap`: never stamped, offered or stored) and by the reader. One `Lines` frame carries at most 64 MiB (`MAX_GIVE`), the rest go next session, so many bundles never make a frame the peer refuses.
- **Taking a bundle:** a node that lacks an attempt's commits (a verify offer or claim, a dep's bring-in, an integration: `commits::fetch`) takes a line naming exactly that attempt (task, base, head). It refuses it unless `git bundle list-heads` is exactly {base, head}, `bundle verify` finds its prerequisites, and after `unbundle` into a quarantine object directory (`objects/incoming-borg-…`, the repo's store its alternate, as `receive-pack` quarantines a push) a `rev-list --objects` of the two commits `--not --all` finds every object. Only then do the objects move into the repo's store (each pack's index last) and are pinned at the same refs. So a refused bundle leaves no object behind: a pack of just the two commit objects would otherwise make them look present (`cat-file -e`) and the genuine line would never be taken.
- **Order:** the commit ids are content addresses, so the first valid line wins in any order and a second fetch finds them present.
- **Outcomes** land in the local `unbundled.jsonl`, once per line and reason. A forged line (wrong heads, not base64, over the cap, an object missing after the unbundle) is never tried again; a missing prerequisite (a `git fetch` brings it) and a local failure (a git call past its 10 s, an I/O error) are.
- **Scoring and integration:** the goal's node fetches a judged attempt's commits before its hidden oracle runs (hidden.rs `score_locked`), so work done elsewhere is scored, a rejected attempt included (no integration fetches that one). An integration leaves out another node's claim base its repo lacks (a rejected or abandoned attempt never comes here).
- **Monitoring:** `borg top` (`commits t1 <head> from node X (N KiB): unbundled|refused: …|waiting: …|carried|made here`), `/api/hive` (`commits`) and the Hive tab's Commits table.
- *Proof, 0 tokens:* two loopback nodes whose clones of one remote have separate object stores (crates/borg-mesh/tests/hive_mesh.rs `work_done_on_b_is_verified_and_integrated_on_a_with_its_own_object_store`): B does t1; a forged bundle (another commit under the attempt's names) reaches A and is refused, A's claim of t1.v1 says the work is on B's node and A's repo holds neither commit; a line over the cap never leaves B; the genuine line arrives after the forged one, A's verifier claims and passes t1.v1, A integrates, the combined tree holds B's head as an ancestor and its file, both boards fold identically and finish, a further round moves only digests.
- *Unit:* hive/commits.rs: one bundle per attempt with only the snapshots past the remote, idempotent fetch, no bundle outside a mesh, refusal of a wrong-commit bundle (once), garbage and an over-cap bundle, waiting on missing prerequisites, a bundle of only the two commit objects refused with nothing left in the repo and the genuine line taken after it (`a_refused_incomplete_bundle_leaves_nothing_behind`); lines.rs a line over the cap counts nowhere; sync.rs a frame's budget.
- *Deferred:*
  - a fix task's `from` (a failed integration's tree) does not travel, it is claimed where its integration ran;
  - a repo without a shared remote bundles its whole history and then usually exceeds the cap;
  - every member stores every bundle of every goal (bounded by the cap per attempt; compaction with the goals');
  - a node that joins the mesh after a done has no bundle for that attempt;
  - the bundle's contents are code and are not redacted (Secrecy covers text).

**Built, secrecy (part S):**
- The redaction presence prompts get (notes.rs `redact`) applies at write time to every synced hive text:
  - a board line's task title, flags and note (board.rs `append`, `redacted`);
  - a contract's text before its id is hashed (`Contract::new`);
  - a message's before it is cut and hashed (`Message::new`, so `well_formed` still holds everywhere);
  - a done's note and a failing check's output before they are cut to a line or a tail (mod.rs `finish`, oracle.rs: a tail that starts past a secret's key would keep its value);
  - the goal's spec in its projection (goal.rs `projection`; `conflict` compares redacted specs, so this node's goal.json, which keeps the spec, is still one definition).
- At write time, not at send: lines are content-addressed and signed, so a line changed on its way out would never reconcile, and a store that joins a mesh later would sync its older lines unredacted.
- Ids, commits, scopes and check commands are kept: the fold, contract matching and the oracle read them, and a check must run the same on every node.
- *Proof, 0 tokens:* the mesh test above puts a token in a task title, a password in a contract and an API key in a message, and none of the three is in any byte either store holds after the goal finished; the fold reads the redacted title, the message reaches the verifier redacted. Unit tests in board.rs (title and note redacted, scope and commit ids kept, the fold intact), contract.rs (same id for the same text), message.rs (`well_formed`) and goal.rs (projection without the secret, no conflict with this node's goal.json).
- *Deferred:* a secret in a check command still syncs (it is code, and redacting it would change what runs); the redaction is the regex list's, so a secret of an unknown shape passes.

**Built, the integration conflict loop (part D2 of tasks/todo.md "Next phase (2026-10-25, d)"; bug fix):**
- **What happened:** the gate run under load (load ~6) ended `three_sim_drones_finish_a_board` with 11 integrations each rejected "conflict: t1 onto t2, t3 in f1.txt" and a verified fix after each, until the budget was gone.
- **Why, from the run's own files:** the three first claims of t1 got no snapshot (a git call past its 10 s bound; the parallel deps suite's done timed out in the same second), so t1's rework claim took a snapshot of a tree that already held its own first attempt, and its accepted diff only added a line to f1.txt. Merged after t2 and t3 (which never had f1.txt in their diffs), that is a modify/delete conflict: git writes the file without markers, so the scripted fix had nothing to change. The rule "resolved only when a later attempt built on the step changes the path" rejected every replay, although each replay built on the fix's tree commit for commit.
- **Fix** (integrate.rs `merge`, `with_markers`): an attempt built on the conflicted step also resolves a path it holds without a marker line.
- **Bound** (`repeated`): an integration that fails for the same reason (the fix's title: `conflict in <paths>`, `<check> fails`, `<check> times out`) right after the previous one's fix was verified, with no other work verified since, is `blocked` with that reason, the fix's id and how to reopen it, instead of opening another fix.
- **Why one fix per reason:** a verified fix passed the combined checks on its own tree, and the replay merges the same attempts to the same commits with the fix last, so the next tree is the fix's; failing the same way again, every further round meets the same tree and costs a fix and its verify (two iterations). A fix whose done lost its snapshot is rejected by its verify as unchecked and reworked, a snapshot problem, never a new conflict.
- **Monitoring:** the runner prints the blocked reason on its integrate line, the board and `borg top` show the task blocked with its note, and the `hive_integrate` stat counts it.
- *Proof, 0 tokens:* a `git` wrapper on the drones' PATH fails a snapshot's `write-tree` on demand (crates/borg/tests/hive_run_cli.rs `a_verified_fix_ends_the_conflict_its_integration_found_and_a_missing_snapshot_is_retried`): the data's sequence through the CLI, then `borg hive run` with 2 sim drones finishes with `integrate.1` rejected and `integrate.2` verified on top of it, one fix, whose first done lost its snapshot and was reworked. With the resolution rule reverted the same test loops through 8 integrations until the $2 budget is spent (with only the bound, it stops blocked at `integrate.2` after $0.40). Unit (integrate.rs): `a_verified_fix_of_a_conflict_without_markers_resolves_it`, `the_same_failure_after_its_verified_fix_blocks_instead_of_looping`; both fail before the fix.
- *Fix (review):* the bound first measured "no other work" by work verified after the fix was opened. The rejected integration's checks run for minutes, and work verified meanwhile (or by a peer whose clock runs early) was dated before the fix yet never in the rejected tree, so its breakage was blocked instead of fixed. Now `new_work` asks git whether each merged attempt's head (the fix's aside) is an ancestor of the rejected tree (the fix's `from`), with no timestamps. *Proof, 0 tokens:* `work_the_rejected_tree_did_not_hold_gets_a_fix_whatever_its_dates` (t2 dated 50 ms before the fix opened; with the old rule integrate.2 is blocked).
- *Deferred:* a claim whose snapshot fails still proceeds with an empty base (its work is rejected unchecked and its tree's leftovers can become a later claim's base, as here); refusing such a claim and retrying it would save the iteration. The deps suite's failure in the same run (`t3 not verified (the work has no snapshot ...)`) is that same git timeout in a hand-driven `done`, the fail-closed verdict working as designed, not a separate bug.

**Built, arm E on one machine (0-token proof only; tasks/todo.md "Arm E on one machine"):** `borg-eval hive --arm e` (crates/borg-eval/src/hive/arm_e.rs).
- **Setup:** two sandbox HOMEs A and B, each its own store, key and node id, one team collective (A founds it and invites B). Each node's workspace is a `--no-local` clone of one bare remote, so commits reach the other node only as bundles.
- **Transport:** a pump thread in borg-eval runs borg-mesh sync sessions between the two stores every 2 s and twice more at the end. Its transport is borg-mesh's `Node`, an iroh QUIC endpoint bound on loopback with relays and address lookup off, so iroh carries the bytes but no network is involved.
- **The run:** the goal is opened on A; once it is on B, B's engineer approves A's check (`borg hive approve`), and `borg hive run --yes` runs 2 drones on A and 1 on B at once. The cell scores the integration's commit from whichever node's repo holds it, and both boards must name it.
- **The row's `e` object holds:** each node's attempts done and verified, verdicts, integrations and bundles (made, received, taken, refused, waiting); the judgements made away from the author's node, counted only where that node took the attempt's bundle (`cross_node`; `on_board` is what the board alone shows); whether both boards fold identically; the most holders of one task at once; the claims that lost a race and those whose session still did the work; the pump's rounds.
- Sim drones sign their files with their node (`by d1@a`), so two nodes' `d1` never make one identical commit that would reach the other node without a bundle.
- *Proof:* `cargo test -p borg-eval hive_e2e_e -- --ignored` (4 tasks; results under Proof, "Results so far").
- *Known limits:* fixed by "budget across nodes" below: the cell's spend is bounded by the cap plus the iterations in flight when the goal's total reached it (one per drone, three here). Since both runs start at once and each splits the whole cap, those in-flight iterations can still sum to a cap per node, so `--stop-at-usd` keeps reserving twice the cap for an arm E cell. The row counts both nodes against the cap (`over_cap`), each node's dollars from its own lines of the synced file.
- *Not measured:* the paid acceptance runs.

**Built, budget across nodes (part D1 of tasks/todo.md "Next phase (2026-10-25, d)"; bug fix):**
- **Before:** each node's `borg hive run` split the goal's budget from its own `spend.jsonl`, which did not sync, so two nodes could spend up to twice the cap (found in arm E).
- **The log:** `spend.jsonl` is the seventh synced hive log (hive/mod.rs `SYNCED`, `borg/mesh/10`: the hive logs reconcile in a fixed list, so a peer on 9 is refused at the handshake rather than reconcile another file at the same step), signed and quarantined like the others and in the daemon's change fingerprint.
- **Node-qualified drones:** each line names its node (`node`, hive/mod.rs `node`). A line without one is from before and is the reading node's: such a line is never stamped, so never offered (borg-mesh lines.rs `local`), since on a peer it would read as the peer's own. So `d1` on two nodes is two drones: a drone's own tally, iteration numbers, dearest iteration and a killed run's recovery read its node's lines only.
- **The fold** (run.rs `ledger`) sums each distinct line once in one fixed order, so every node folds the same total, by node, whatever order the lines arrived in.
- **The split and the stop:** every runner's share is what is left of the budget after every node's spend, split over its drones. Every drone reads the goal's total before each decision and stops (`budget`) once it reaches the budget, whoever spent it. The harness's own cap per iteration is the drone's dollars left, never more than the goal has left.
- **The bound:** runs started at once on two nodes each split the whole budget (neither has seen the other's spend), so the stop rule, not the split, bounds them. The goal's total is at most the budget plus the iterations in flight when it reached it, one per drone, each at most its drone's dollars left (the harness's cap) or what `--iter-timeout` lets it spend, as long as each node sees the others' lines before its drones start another iteration (the daemon syncs a change within seconds; a real iteration lasts minutes). Each sync a node misses lets each of its drones start one more iteration.
- **Monitoring:** the run's last lines print the goal's total by node (`goal g: $1.25 of $1.00 spent (here $0.75, 1a2b3c4d $0.50)`, also in `--dry-run`); `borg top` and the Hive tab show the goal's total over every node and another node's drone as `d1@<node>`.
- *Proof (0 tokens):* crates/borg-mesh/tests/hive_mesh.rs `runs_on_two_nodes_spend_one_goal_budget`: two loopback nodes with clones of one remote, `borg hive run` at once on A (2 sim drones) and B (1), $0.25 per iteration (one task each, 3 s of work), budget $1, a sync session every 50 ms. Both runs split the whole budget ($0.50 per drone on A, $1 on B, so the old rule allowed $2); both nodes end with the same spend lines naming both nodes and fold one ledger; the total is at least $1 and at most $1 + 3 × $0.25; every drone ends on `budget`; the run prints the total by node; and B's hive view shows the total and A's drones as `d2@…`.
- *Unit:* run.rs the ledger is equal under reversal, rotation and duplication, a line without a node is the reader's; a drone's earlier iterations are its node's only; the stop rule ends on the goal's budget left; view.rs another node's drone is listed under its node with all its lines and no share; lines.rs `spend.jsonl` is listed as synced, `runs.jsonl` not; origin.rs a spend line without a node is never stamped (review fix: before it, an upgraded node signed and offered its old lines, which a peer then counted as its own spend, iteration numbers and dearest iteration); borg-eval each node's dollars from its own lines of the synced file.
- *Deferred:* a hard bound under lag (a reservation line before each iteration, or one allowance per node agreed over the mesh): today two nodes that start at once can each spend up to their split if they do not sync; a revoked member's spend lines are quarantined with its other lines, so they stop counting against the goal; another node's drone shows no share (runs.jsonl stays on its node).

### L1. Log propensities and sync outcomes

Plan: add `p` to `asked` records (`outcome.rs`, `conscience.rs::explored`). Add `outcomes.jsonl` to mesh lines (`borg-mesh/src/lines.rs`) with redaction.

**Built** (preparation, no value metric yet: it enables L6's OPE and federated beliefs):
- `asked` and `graded` records carry `p`, each lesson's probability of being asked under the policy in force:
  - 1 or 0 under the fixed graded rule;
  - the explore mixture (1 − 1/10)·[r < k] + 1/10·[r < 20] under the learned cut (`attention::propensities`; the explore counter is content-independent);
  - the share of 512 Thompson draws that give a lesson a slot on the bm25 and non-graded paths (`conscience::propensity`).
- The hook carries it from the check to the `asked` record through `#borg-p` in the session state.
- `borg learn`, borg-web and every mesh sync publish a redacted projection (kind, lesson ids, ms, `p`, channel, blake3-tagged session and repo, a regret's tagged cause and credit, node) of the shared kinds to `outcomes-team.jsonl`, synced as a third log (`borg/mesh/6`), signed and quarantined like lessons. The shared kinds are every result kind memory folds, `vetoed` and `regretted` included, so peers converge; the activity clock ticks on shared kinds only.
- Memory and trust read the team view (`outcome::read_team`), dropping this node's own copies by session tag; learn.rs, attention.rs and stats.rs stay local.
- On by default (`BORG_SHARE_OUTCOMES=off`).
- *Done since:* L2's reward kinds (`verified`, `rejected`, `hacked`) joined `SHARED_KINDS` with `c` in C4 (`borg/mesh/7`).
- *Deferred:* syncing `graded` records (federated OPE and attention labels), propensities of early and queued deliveries (no `asked` record), poisoning resistance (k distinct nodes, trust-weighted median), compaction of the team file, and the reward kinds of L7b, which must be added to `SHARED_KINDS` deliberately.

### L2. Feed oracle rewards in

Plan: hive verify verdicts and hidden-oracle results become outcome kinds (`verified`, `rejected`, `hacked`) credited to the lessons shown during the task.

**Built, local** (preparation: the value metric, the hacked rate H8 and the hidden-oracle solve rate per goal for H5, waits for the borg-eval ImpossibleBench-style set):
- **The verdict:** a verdict (`borg hive verify`, or the verifier's last message `VERDICT: pass|reject <why>` at the stop) goes through `hive/verdict.rs` `record`: refused for the author's session and any session of its drone, then `decide` (L8's checks), then the board line.
- **The credit:** every lesson shown to the author's session while it held the claim (`veto::shown_between` over the claim's span) gets `verified`, `rejected` or `hacked` (outcome.rs), keyed by `cause` = `<goal>/<verify task>`, once per lesson and attempt, and the verdict counts one `task_*` stat.
- **The hidden oracle:** with one (`goal.json` `hidden`, a directory outside the repo, `hive/hidden.rs`) a pass or a reject waits for its score, run in a scratch worktree of the done's snapshot when `borg hive run` ends or by `borg hive score`. The score decides `verified` or `rejected`, and `oracle.jsonl` logs whether the verifier agreed.
- **The weights:** memory.rs adds `VERIFIED_W` = 1 to α (like a change), `REJECTED_W` = 1 (like an ignore: the verifier can be wrong) and `HACKED_W` = 3 to β (as heavy as a veto, not above: the hack test is a heuristic over a diff's lines), one per (lesson, attempt), the worst of duplicates; never a use, never a clock tick. All three weights are design-chosen, to be tuned by L6.
- **The verifier's regret:** a regret of verified work also reaches the verifier's lessons asked before its verdict at `VERIFIER_REGRET` = 0.5 of the strength (learn.rs, cause `<episode>/verify`, design-chosen).
- trust.rs, attention.rs and `weight` ignore the kinds. `BORG_HIVE_CREDIT=off` freezes the credit for an eval arm.
- **Across nodes:** in `SHARED_KINDS` with `c` since C4 (hive lines sync): `c` = the tag of the cause, and `team_view` compares a local record's cause as its tag too, so memory derives one verdict per (lesson, attempt) from a local record and a peer's copy alike; they never tick the activity clock. The verdict line on the board carries the kind it credits (`credit`), and the author's node, which alone holds the shown records, credits a verdict given on another node at its next mesh round (`verdict::credit_synced`), once; the verifier's node counts the `task_*` stat.
- **Guard:** a peer's verdict line counts only for an attempt whose board verdict took effect on this node, and only of a kind that verdict allows (`hacked` for a hacked verdict, else its `credit`, either of verified and rejected when a hidden score decides; `verdict::judged`). So a forged team line also needs a forged, signed board verdict, which is auditable and quarantined on revocation.
- *Remaining caveat, the trusted-member model:* a member can still forge both, counted once per (lesson, attempt), but attempts are mintable; the k-distinct-nodes rule and a trust-weighted median stay deferred.
- *Known limit:* hidden-oracle results stay private: a goal with one credits on its runner's node only, so a remote author of such a goal's attempt is not credited yet (deferred).
- *Deferred:* crediting a verifier by whether the hidden oracle agreed with it (`agree` is logged for it), engineer-overturned rejections, and the Shapley refinement of equal shares.

### L3. Make regret reach back

Plan: reach back to lessons that were active at decision time (`notes/regress_learn.rs`).

**Built:**
- `borg learn` writes a `regretted` outcome (`outcome::regretted`, 0 tokens, once per regret and lesson) for each lesson asked in the deciding session before the decision commit's committer time (review regrets: before the finding).
- The merge target takes the blame alone if it was asked, otherwise the asked lessons share it equally. A playbook version shown in the session shares the same regret apart, among the versions (L5), so it never dilutes a lesson's share.
- The blame is `1 − 2^−g` over the regret's independent grounds g, and counts `REGRET` = half a correction in the usefulness belief.
- On by default (`BORG_REGRET_CREDIT=off`).
- *Pre-registered check:* `eval/regret-credit/report.py` (local) compares the belief's log-loss on later results with and without the term.
- *Still open:* the leave-one-out Shapley refinement, and credit for decisions made on another machine (needs L1's outcome sync).

### L4. Group reflection and rework lessons

Plan: `borg learn --group <task>` compares K rollouts and writes semantic-advantage lessons (`learn.rs`, reusing `generalise`).

**Built, groups** (preparation for H6, no value metric yet):
- **Opening a group:** `borg hive open <goal> --id T --group K` opens K work tasks `T.g1`..`T.gK` with `group: T` (`hive/group.rs`, `board.rs` `Task::group`). Each is an ordinary task (claim, done, verify, rework).
- **Independence:** the fold bars a session, or a drone on the same node, that held or verified one member from claiming another, and one that held a member from verifying another (`Board::rival`; one verifier may judge every member). So the K attempts are K drones', independent, and none is judged by a rival. `borg hive run` refuses fewer than K+1 drones, so `--group` is 2 to 19.
- **Deps:** a dependent may not name a member as its dep (`hive open` refuses it): only the best member is integrated, so work built on another would merge onto a tree without it.
- **The best member:** every member is verified; the integration merges only the group's best accepted attempt (`group::best`: the hidden oracle's pass, which integrate.rs scores first for a contested group, else the fewest rejections, then the earliest verified).
- **Rewards:** the reward of each judged attempt is its hidden score on this node, else the visible verdict (verified and not hacked wins; a rejected attempt lost, its rework is a new attempt).
- **The episode:** a group is settled when every member is verified and every attempt has its reward. A settled group with a winner and a loser becomes one episode of kind `group` (`group-<goal>.<task>`, unique since a goal id has no `.`): task, result, and each attempt's drone, reward, done note, verifier note and redacted diff excerpt, winners first.
- **The lesson:** `borg learn` generalises it with its own opening (`group::HEAD`, `group::FIELDS`, the regret prompt's neutral reach and merge rules) into a semantic-advantage L0 lesson, or a merge, whose `episodes` name the group. It is learned at any session's end (like a veto), credits no session (no `asked_then_corrected`), and counts as a new lesson with source `group`.
- **One learner:** every `borg learn` first records each settled group opened on this node with a spread and no episode yet (`group::record_pending`: one learner per group, the node its open was written on, which answers the open question "Where the learner runs" for groups). `borg learn --group <goal>/<task>` reflects on one from any node. Once per group (a fixed episode id). Groups of one, unsettled groups and groups without a spread write nothing.
- On by default (`BORG_GROUP_REFLECT=off` for an eval arm; also off with `BORG_LEARN=off`). `borg top`, `/api/hive` and the Hive tab show each group's attempts, winners, losers, what decided them, the integrated member and the reflection.
- *Proof (0 tokens):* a sim run of K=3 drones with a held-out check only d2's attempt passes integrates d2's attempt, and one `borg learn` against a stub model asks the group prompt once (winners first) and writes one lesson `Lgroup-g.t1`; neither a second `borg learn` nor `borg learn --group` asks again (`hive_run_cli.rs`); a group of one is a plain task and reflects nothing; a dep on a member is refused (`hive_run_cli.rs`); fold, reward, best-pick, rival (a verifier of one member may not attempt another, an attempter may not judge a rival), once-only and one-learner (a group opened on another node is not recorded unasked, `borg learn --group` still records it) unit tests (`group.rs`, `learn.rs`).
- *Deferred:* drones that differ in harness, model, playbook or genome within one run (the drones of one `borg hive run` share them, so a group's attempts differ by sampling only; L7's genome arms); an L1 playbook edit as the reflection's output (L5); dropping a group's losers once another member won instead of reworking them; deps on a group as a whole (a dependent of the integrated member). H6 (paid) is not run.

**Built, rework episodes** (preparation for H5, no value metric yet: the hive's own lesson source, alongside the engineer's corrections):
- **The episode:** a work task whose attempt the verifier rejected and whose later attempt was verified (`hive/rework.rs`) becomes one episode of kind `rework` (`rework-<goal>.<task>`, once per task): the task, every counted rejection's note (the verifier's why), each rejected attempt's done note and redacted diff excerpt, what the accepted rework changed relative to the last rejected attempt (diff excerpt), and the drones.
- **The lesson:** `borg learn` generalises it with its own opening (`rework::HEAD`, `rework::FIELDS`, the regret prompt's neutral reach and merge rules): what mistake the rejected attempt made that the verifier caught, as a lesson that prevents it. Learned at any session's end, credits no session (no `asked_then_corrected`: the verdicts credited the lessons shown already, L2), source `rework`.
- **The reward** is group.rs's: the hidden score on this node, else none yet where the goal has a hidden oracle (here, or held out on the goal's node, whose scores never sync), else the visible verdict.
- **What counts:** the accepted attempt is the board's, verified, not hacked and not failed by the hidden oracle. A rejection counts when the verifier rejected the work on its merits, so a hacked verdict (a guardrail's), an unchecked one (no snapshot, a dep's work missing) and one the hidden oracle passed (the verifier was wrong) do not. Several rejections make one episode with all their notes. A fix task after a failed integration is a work task, so its own reject-then-verified counts; a group's member counts too.
- **One learner:** every `borg learn` first records each settled rework whose verifying verdict was written on this node (`rework::record_pending`, mirroring group's rule), except for a goal with a hidden oracle, whose node (the only one with the held-out scores) learns all its reworks; another node leaves them `waiting`.
- On by default (`BORG_REWORK_REFLECT=off` for an eval arm; also off with `BORG_LEARN=off`). `borg top`, `/api/hive` and the Hive tab show each reworked task with its rejections, drones and reflection; the feed names the source of its lesson.
- *Proof (0 tokens):* a sim run whose first verdict rejects (`reject_first`) and whose rework names its check is verified, and one `borg learn` against a stub model asks the rework prompt once (with the verifier's why and the rework's diff relative to the rejected attempt) and writes one lesson `Lrework-g.t1`; a second `borg learn` does not ask again (`hive_run_cli.rs`). Unit tests: an episode only for reject then verified, none for verified-first, a hacked or unchecked rejection, a hidden pass of the rejected attempt or a hidden fail of the rework, waiting while unscored, several rejections in one episode, once, one learner (a task verified on another node is not recorded here; a held-out goal's rework waits on a node it synced to and is recorded by the node holding its oracle, even when verified elsewhere) (`rework.rs`), and the prompt, the source and no correction counted (`learn.rs`).
- *Deferred:* the integration's own rejection as a lesson source (it judges the combined tree, no single attempt: the fix task that follows has no rejected attempt of its own); `borg learn --rework` to reflect on one from any node; penalising rework in the reward (`μ·rework`).

### L5. Playbooks (layer L1)

Plan: `playbooks.jsonl`, edited by deltas, shown through `on_prompt` matched to the task shape. Promotion through the queen.

**Built** (preparation for H5, no value metric yet): `playbook.rs`.
- **Edits, never whole playbooks (ACE):** `playbooks.jsonl` holds promoted edits; each adds, modifies or removes one step of one playbook.
- **The fold is order-independent:** a set of edits, replayed by (time, edit id). An add adds a step, again after a removal; a modify sets its text; a removal drops it. A modify or removal of a step no add reached yet waits for the add, so a promoting node's clock behind the add's loses nothing. A line counts only when its edit id hashes its whole content and its playbook id derives from its repo and shape, so no synced line can take another's id, change a promoted text under the same version key, or move an edit to every repo.
- **Sync:** the file syncs over the mesh as a fourth plain log beside lessons, stats and team outcomes (borg-mesh lines.rs `SYNCED`, `borg/mesh/8`), signed and quarantined.
- **Ids and versions:** a playbook's id hashes its repo and its shape's significant words, so two nodes learning one shape edit one playbook; its version `<id>@<edits>.<hash>` names the exact set of edits.
- **Proposed:** every `borg learn` (with `track`) reflects once on each attempt done on this node that won (verified and not hacked, or the hidden oracle's pass; of a settled group, only the attempt of the member `group::best` integrates, with the attempts it beat in the prompt). It is one model call shown the matching playbooks with step ids, answering at most one edit or none (`playbook::OPENING`, `schema`); once per attempt (`playbooks-reflected.txt`, local). A step's text and a shape are one line of at most `STEP_MAX` = 300 characters (the length the question showed before); a longer answer proposes nothing, and the question shows the new text whole, so what the engineer promotes is what drones read (`render` also puts each step on one line).
- **Promoted through the queen:** the edit is a `Kind::Playbook` question with the edit as payload (as `Kind::Contract`), default "keep the playbook as it is", deduped like every question, outside the engineer's budget. Only an answer "promote" appends it, stamped `ok_by` (the question) and `by` (the engineer); lines without `ok_by` never fold.
- **Shown:** a hive drone's claim gets the playbooks whose shape's significant words its task's title and files name at `queen::STATED` (the queen's existing bar for "the text states it"), of the task's repo or of none, after its contracts. A normal session's prompt (hook.rs `on_prompt`, not a hive session) gets one only when it names every one of them. Each version once per session (`#borg-playbook`).
- **Credit:** the version shown is logged as a local `playbook` outcome (`outcome::PLAYBOOK`, the key in `lessons`), which `outcome::shows` puts beside `asked`. veto.rs `shown_between` (so `borg no` and the hive verdict credit it) and `outcome::regretted` read it (the regret's share of the versions is apart from the lessons'), and memory.rs derives the version's usefulness belief from those credits exactly as for a lesson (`playbook::beliefs`; the credits are shared kinds, so peers derive the same).
- On by default (`BORG_PLAYBOOKS=off` for an eval arm).
- **Monitoring:** `/api/hive` and the Hive tab list each playbook's steps, every shown version's belief and the edits waiting (each one's text; a playbook with nothing promoted yet shows with its waiting edits alone); the feed shows `playbook_shown`, `playbook_proposed`, `playbook_promoted`.
- **Cost:** reflection reads an attempt's diff only when it is reflected on (attempts reflected already cost nothing at a session end).
- *Proof (0 tokens):* the fold is equal under reversal, rotation and duplication, ignores an unratified line and a line whose ids its content does not derive, lands a modify promoted before its add, and adds a removed step again (`playbook.rs`); a step longer than its question or spread over lines is never proposed or promoted; a verified task asks the reflection once against a stub model and queues one edit that applies only after "promote"; the promoted playbook is shown once to the drone of a task of its shape, not to a task of another shape; the verdict on that attempt and a `borg no` credit its version, and a plain session sees it only when its prompt names the whole shape (`hive_verdict_cli.rs`); a regret reaches the version shown before the decision and no later one; two mesh nodes fold the same playbook after a round (`sync.rs`); `/api/hive` shows the belief and the waiting edit, before the first promotion too (borg-web `hive_api.rs`).
- *Deferred:* the promotion gate on a held-out goal set at the same budget (the engineer's answer is the only gate); using the belief (a version with a low belief is still shown; rollback is a `remove` edit); the erosion check (length and coverage) beyond the one-small-edit rule; the group reflection's own output stays an L0 lesson, its playbook edit comes from the integrated attempt; drones of one run that differ by playbook (L4's deferred arms); H5 (paid) is not run.

### L6. Off-policy evaluation and tuning

Plan: an `eval/ope` replay over logged `asked` records that proposes new constants.

**Built** (preparation: no logged decision carries support yet): `borg ope [--dir DIR | --file FILE] [--json]` (ope.rs) replays the outcome log for the six hand-set weights: the veto's 3 (now `VETOED_W`), `VERIFIED_W`, `REJECTED_W`, `HACKED_W`, `REGRET` and `VERIFIER_REGRET`.
- **What can identify them:** only the Thompson slot draw over a pool. On the default graded path p comes from the model's grades (0 or 1) or the learned cut, both free of these weights, and the shortlist the model grades, which the weights do order, is neither randomised nor logged.
- **The logged pool:** `asked` records from a Thompson draw now carry the draw's pool, local and never shared: `pool` (ids in ranking order), `base` (each one's score without its usefulness factor, `conscience::pool_base`) and `scoped`, carried from the check by a `#borg-pool` state line tagged with its check, as the check's `#borg-p` line is. An asked record whose lessons' p come from different draws, as when a stop merges a waiting live ask, carries no pool.
- **The replay:** for each such decision it derives the beliefs from the outcomes before its ms (memory.rs `derive`, counts reused across candidates through `Memory::alpha_with`/`beta_with`; a verifier regret's credit rescaled for its candidate) and runs the live `conscience::propensity` on the logged pool.
- **The grid:** each weight ×0.5, ×1 and ×2 around its current value, one knob at a time, the others held.
- **The value:** self-normalised IPS of the per-ask change rate (reward 1 when the session's strongest result after the ask is a change), with a 95% bootstrap CI over whole sessions and the effective sample size. A knob's best candidate is reported with whether its CI lower bound clears the current value's estimate. It never applies anything.
- **Skipped and checked:** records without p, with p = 0, without a pool or without a result yet, and `graded` records, are skipped and counted. An agreement line checks the current weights' replay against the logged p, and a decision it misses by more than 0.01 is left out of every estimate and counted: the live draw saw other records than the replay's prefix (a team record stamped before the ask but synced after it).
- *Proof (0 tokens):* the replay of the current weights reproduces the logged p of the live chain within rounding; the public replay cuts the log at the decision itself (a veto at or after it changes nothing, one before it does); a team record the live draw did not see drops its decision; the bootstrap interval of four sessions of ten correlated rewards is as wide as four results allow; a planted better veto weight ranks first (and a lighter one in the mirror world); a knob without evidence cannot tell its candidates apart; and 2k decisions over 20k records replay within 5 s.
- *Result on this machine's only store* (449 records, 2026-10-09 to 2026-10-23): 100 `asked` records (441 asked lessons), none with p (they predate propensities), no `graded` record and no veto, regret or verdict record, so 0 decisions and every knob without evidence: too thin, constants unchanged.
- *Deferred:* a randomised shortlist on the graded path, so that the default path gets support (a live behaviour change, its own eval decision); a doubly-robust estimator; pool-membership effects (the logged pool is held fixed: other weights can move a lesson across the LOW/BLOCK bars or change the widen path's forced picks, which the replay does not model); a peer's verifier regret (its cause arrives tagged, so it replays at the current share); the correction's weight 2.

### L7. Genome archive (layer L2)

Plan: in `borg-eval`, genome files as arms, a Pareto set, and parent choice by how much descendants improved.

*Not started; parked.*

### L7b. Engineer judgment

Plan: `borg no`, the veto outcome kind, turning veto reasons into lessons or proposed constraints, intent cards with risk classes, and the queen's attention reward. Ship it with L2, because the "no" signal matters more than the oracle signal.

**Built, local part:** `borg no` (terminal and in-session prompt), the `vetoed` outcome weighed 3 in the usefulness belief, veto episodes learned into lessons at any session's end, invariants proposed as `Kind::Constraint` queen questions (ratified into `constraints.jsonl`, not yet enforced), and the repeat-veto rate (`veto_repeat` / `veto`) on borg-web. The weight 3 is the design's choice, not eval-picked: preparation for H5b, to be tuned by L6.

**Built, across the mesh** (preparation, same value metric, H5b):
- `borg no --session <drone>` for a drone on another node signs a `veto` steering directive (`borg steer <drone> --veto "<reason>"`; steer.rs `VETO`, borg-mesh steer.rs), checked and relayed like every directive (admin or steer role, fresh, this collective, quarantined on revocation; a veto without a node and a session is refused by every node).
- The drone's node records it once when it arrives, and only a node that holds the session (its presence or outcomes) does (veto.rs `apply`, after each round's steering and at once for a drone of the issuing node; `<store>/steer/vetoed/<id>` created exclusively): the `vetoed` credit of the lessons that session was shown, the veto episode its `borg learn` turns into a lesson (so `veto_repeat` counts there), and the `veto` stat (`why: mesh`). Its drone is told once at its next hook, labelled with the signer's key.
- The issuing node records only `steer_issued`; the credit reaches its beliefs as team outcomes (`vetoed` is in `SHARED_KINDS`), so each node counts the veto once.
- *Proof (0 tokens):* two loopback nodes, a veto signed on bob's node for alice's drone is credited once on alice's over two rounds, recorded there as one veto episode (which its `borg learn` turns into a lesson), shown once, and both nodes' memory counts one veto (borg-mesh sync.rs `a_veto_on_one_node_reaches_the_drones_node_once`); the binary records a veto of a local drone at once and refuses one without a reason or for every drone (`membership_steer_cli.rs`); `apply` is idempotent and ignores other kinds (veto.rs).

**Built, the queen's attention reward** (preparation: no value metric yet; it is what a learned escalation policy will be judged by): escalation.rs logs three stat lines once each.
- `queen_no` (+1) when an escalation is answered by changing course.
- `queen_stamp` (−α) when it is answered with the option that lets the plan go ahead (`Kind::proceeds`: "proceed as planned", an external mutation's "run it as planned") or a bare yes.
- `queen_missed` (−β) for a "no" after merge from a session with no escalation: a veto whose session landed a commit of its own still on HEAD (notes.rs `landed_by`) or whose hive work an integration merged before it (`merged`: work verified before some integration that passed by then, every passing one counted, and of a group only its best attempt), and a strong regress link of a deciding session (notes/regress_learn.rs, once per link).
- Only the four checkpoint kinds count (`Kind::escalates`); constraints, contracts, playbooks, reach and open decisions are gates the queen does not choose. α = 1 and β = `VETOED_W` = 3, design-chosen, not tuned.
- The lines roll up into the synced `stats.jsonl`, so `escalation::team` folds the team's yield (reward, share of answered escalations ending in a no, each count), served on `/api/queen` and shown on borg-web's queen view. Nothing optimises it: it is logged and monitored only (the design keeps the predictor a router).
- *Proof (0 tokens):* stamp versus no per kind, the fold's weights, each answer and each missed id logged once, gates ignored, a veto of integrated hive work missed and one of unmerged work not, work merged by an earlier integration while a later one passed after the veto, and a group's losing attempt never merged, `landed_by` (escalation.rs, veto.rs, queen.rs, notes.rs), and `/api/queen` folding local events with a peer's rollup (borg-web `queen_api.rs`).
- *Known limits:* "no escalation" is judged against the node's own queue (the queue does not sync), so a regret of a decision made on another node always reads as missed there; "merged" for a plain session is a landed commit, not a merge to a shared branch; a free-text answer counts as a no unless it is only yes-words.

**Built, intent cards** (preparation, H4b's engineer minutes not measured): `borg hive run` asks the engineer to approve the plan once before drones start (C1, "Plan approval and intent cards"): a card per task with a risk class from the queen's classifier (high: one of the four checkpoint kinds in the title), docs-and-tests-only scope (low) or neither (normal). A "no" stops the run with nothing claimed, and a "no" with a reason is recorded as a veto episode. A high-risk task's claim escalates once as a question of its card's kind, which the attention reward counts.

*Still open:* the predictor of what an engineer would refuse; the sampled audit of auto-approved cards; routing beyond the claim escalation.

### L8. Guardrails

Plan: `pre_tool` flags on oracle paths, hacked detection and the hack monitor ship together with L2, not after.

**Built, with L2** (preparation, same value metric):
- **Snapshots:** a claim of work and its done record snapshots of the drone's tree (commits written without a ref); the verdict diffs them (`verdict::protected`) and makes the verdict `rejected` with `hacked`, whatever the verifier said, by one rule: tests may grow, never shrink.
- **Hacked, outside the task's `files`:** a test file (`guard::class_of`: test dirs and names, a runner's config) that is deleted, renamed or retyped, or modified so that it weakens (`verdict::weakens`):
  - a removed or reshaped assertion (each removed line must be kept by an added one of the same shape and indentation, `guard::keeps`);
  - two names collapsed into one (`got == got`);
  - a block comment or string opened around unchanged lines;
  - fewer test declarations (`#[test]` turned `#[inline]`);
  - more silencers (a skip, `.only`, an early `return`, `if False:`);
  - a Python name bound again so the first no longer runs;
  - a binary edit.

  So is CI config modified, deleted or renamed, or a test runner's config (or its part of a shared one, an `addopts`, a `jest` key) added or changed.
- **Fine:** added tests, added assertions, renamed keys and accessors and updated values.
- **Hacked anywhere:** a change to the visible check's own files, even inside the task's scope, and an `oracle` flag.
- The same rule judges the fix task after a failed integration (from the combined tree) and every arm of borg-eval.
- **Flags:** `pre_tool` (`hive/guard.rs`, every harness that maps it) flags writes to test, CI and oracle paths and reads of the hidden oracle's files into `flags.jsonl`, once per session, claim and path, and the verify task lists them. Flag only, best-effort; a session in no goal pays one failed open.
- **The hidden oracle** lives outside the repo (`init` refuses one inside it) and runs in scratch worktrees under the goal's directory, never in a drone's.
- Every hive git call runs with hooks, fsmonitor and diff-shaping configs off (`hive/git.rs`).
- **Inoculation:** the inoculation sentence is the drone's first hive context (and run.rs's prompt), with "held-out checks you cannot see" for a goal with a hidden oracle.
- *Known limits:* what counts as a test file is still decided by path, so inline tests (Rust `#[cfg(test)]` in `src`) are not guarded and a non-test file under `spec/` is judged as a test; the line-shape rule is a heuristic, which is why hacked weighs as a veto and not above; a `python -c` write escapes the flags (the diff does not); a drone can still read `goal.json`.
- *On the acceptance run* (C3) it gave no false hacked verdict, after the probe before it had flagged required test additions.
- *Deferred:* the hack monitor (a paid model reading transcripts), and the promotion and erosion gates (they need L1/L2 layers).

## Tests

- Claim races: two claims in any arrival order produce one winner on every node.
- Lease expiry.
- Matching contracts to scope.
- The stop hook blocks while the oracle fails.
- A `sim` harness run with 3 simulated drones on one board: `borg hive run` finishes it, no task has two holders at once, every verdict comes from another drone, and the spend stays within the shares (crates/borg/tests/hive_run_cli.rs).
- Across machines (C4):
  - two loopback mesh nodes with drones as the real `borg` binary (crates/borg-mesh/tests/hive_mesh.rs);
  - the fold of remote lines in every arrival order, a drone name barred only on its node, and a runner that never releases another node's claim (board.rs, run.rs);
  - a revoked member's board line quarantined and goal ids path-checked (borg-mesh sync.rs), a quarantine and the hive's appends sharing the log's lock (borg-mesh origin.rs);
  - two definitions of one goal stopping it on every node (goal.rs), a check another node wrote running only once approved (oracle.rs), a forged message line dropped (message.rs), a forged claim and done naming this node credited nothing and a board a quarantine rewrote read again (verdict.rs), a hidden oracle's path never in the synced flags (guard.rs);
  - a verify task or an integration whose commits are absent refused cleanly, ask/tell once to the holder, and a fresh runner session not offered its drone's verify task (crates/borg/tests/hive_cli.rs).
- Commits across machines (C4): two loopback nodes whose clones have separate object stores, work done on B verified and integrated on A, a forged and an oversized bundle refused, no secret of a title, contract or message in either store (crates/borg-mesh/tests/hive_mesh.rs); bundling, fetching and every refusal (hive/commits.rs); a line over its cap counts nowhere (borg-mesh lines.rs).
- Budget across nodes (C4): two runs on two loopback nodes spend one goal budget (crates/borg-mesh/tests/hive_mesh.rs `runs_on_two_nodes_spend_one_goal_budget`).
- Integration and dependencies (crates/borg/tests/hive_run_cli.rs): a conflicting integration opens a fix task and the hive still finishes; a verified fix ends a conflict without markers, and the same failure after its fix blocks instead of looping; a dependent task starts from its dependency's verified work; idle drones wait for work in flight; a drone that grows an existing test is not hacked; a killed run is charged and released by the next.
- The reward (crates/borg/tests/hive_verdict_cli.rs): `pre_tool` flags a test edit only for a drone in a goal and answers nothing; a verdict on a diff that weakened a test is `hacked` and credits the lessons shown during the claim once, one on work that only adds tests verifies; the author's session cannot give the verdict; a pass at the stop (`VERDICT: pass`) verifies and credits; with a hidden oracle the score decides the credit, once; no hook of a drone outside every goal creates a hive.
- Learning health (crates/borg-web/tests/learning_api.rs): every series per week from a seeded team store, nothing written.

## Open questions

- **Budget enforcement:** each drone could stop itself when it reaches its share, or a runner could kill it. Probably both. *C1 (H2): both; a harness that cannot stop itself overshoots by at most one session. Across nodes (D1, C4): the goal's spend syncs and every drone stops on the goal's total, so the hive overshoots by at most one session per drone in flight, while the mesh syncs faster than a session lasts.*
- **Merges:** on one machine the integrator merges the accepted attempts (built, and exercised on real repos). Across machines an attempt's commits travel as a bundle beside the board (C4, "Commits travel", built), so the integrating node merges as on one machine; a failed integration's tree does not travel yet.
- **Secrecy:** contracts and task text sync to every member. Do they need the same redaction applied to presence prompts? *Yes, built (C4, "Secrecy"): applied when the line is written, since a signed line cannot change on its way out; check commands are kept.*
- **Goal signatures:** whether only an admin's goal should sync (Goal; today `sig` stays empty under the trusted-member model).
- **Where the learner runs:** on one learner node per collective (simple), or on every node deterministically (no single point of failure, but repeated model spend)? *For groups and reworks: the node the group was opened on, or the rework verified on, or a held-out goal's node (L4).*
- **Budget for learning:** reflection and eval cost real money. Spend a fixed share of each goal's budget (say 10%), or adjust it by H5's slope?
- **Privacy:** outcome lines reveal which lessons a drone was shown, and when. Is that acceptable for every role?
