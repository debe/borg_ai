# Hive: many drones, one goal, and a hive that learns

## Status

Everything below is built as preparation: no value metric has been shown yet. The hive is a local, file-backed spike that syncs over the mesh. It was accepted on two company workloads on the work machine (C3): it works without serious bugs. Whether it beats one session with subagents is not measured. Across machines it is proven on loopback nodes, and arm E runs on one machine; the paid acceptance run across nodes has not run. The learning loop is built except the genome (L7, parked), the refusal predictor and the sampled audit (L7b). H5 ran without a decision, and learning stays on by the engineer's call (Proof). H1–H4b and H6–H8 have not run. The experiments run on the work machine against company repos.

| Step | What | Status | Proof | Deferred |
|---|---|---|---|---|
| C1 | Local spike: goal, board, contracts, oracle, hooks, runner, integration, plan approval and intent cards | Built | Sim drones against the real hooks (crates/borg/tests/hive_run_cli.rs, hive_cli.rs); accepted through C3 | Multi-repo goals; killing an unmetered session mid-way; a dollar price for codex tokens; plan approval across nodes, for joined sessions and for tasks a drone opens; the intent card at `done`, sampled review, ordering by risk |
| C2 | Eval harness: arms A, B, D3, E, F in `borg-eval hive` | Built | Unit tests with fixtures; end to end with bwrap, the sim harness and a fake `claude` | A workload hard enough to tell the arms apart |
| C3 | Pilot, run as an acceptance test | Accepted (hive-accept-1, 6/6, $8.48) | Hidden 6/6 on W1 and 5/5 on W2, conflicts 0, hacked 0 | H1–H4b not measured; the fix-task loop not exercised on a real repo |
| C4 | Across machines: logs sync, ask/tell, commits travel, secrecy, the integration conflict loop (D2), arm E on one machine, budget across nodes (D1), node-affine task choice (E2), a lost claim told at the next hook and set aside (E1, E5), sync on write (E3), a run announced before its drones start and drones of one node starting apart (E2b), a confirm window before work (E4) | Built; arm E's paid acceptance run not run | Two loopback nodes (crates/borg-mesh/tests/hive_mesh.rs), `hive_e2e_e`, hive_run_cli.rs, board.rs Monte Carlo (tasks picked on both nodes at the pigeonhole floor in every cell), hive_cli.rs lost-claim tests, sim arm E loser waste, a claim at the peer within 50 ms p95 on loopback (sync.rs; under 1 s in the default suite), a peer that does not answer delays no claim and a failed dial is retried a `poll` later (sync.rs), an idle node with 2000 old goals at 1% of a core, board.rs and mod.rs E2b/E4 unit tests (a busy or ended drone counted once), sim arm E 0 lost claims in 6 cells (the pre-registered 20 not run yet; the sim's confirm windows never waited, so E4's loss path is proven by unit tests only) | Plan approval across nodes; a claim written while a round with its peer moves bundles waits for its end; hive logs first in a session (`borg/mesh/11`); sessions per hive line (about 2·(n−1)² for n nodes, not coalesced); bounding a claim's `ms`; salted session aliases; a fix task's `from` travelling; a hard budget bound under lag; compaction; liveness per node's view (a node quiet for a lease); runs started further apart than one round; a confirm window for integrate claims; pruning set-aside snapshots |
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
- **Claim races:** when two claims arrive for the same task, the lowest `(ms, blake3(key))` wins. Every node computes the same winner without coordinating, and the losing drone learns on its next hook call (a prompt, any tool call, its stop), its edits are refused while it holds no task, and at its stop or next claim its worktree goes back to where the claim started, the lost work kept in a pinned snapshot commit (C4, "A lost claim told at the next hook"). Across machines a claim is provisional until the next sync round, which the daemon starts as soon as a hive log changes (C4, "Sync on write"): the loser's done and verify-task open are ignored by the fold, so a loser wastes about one sync latency and one tool call, and the clocks of the nodes must stay well within a lease. With drones live on another node, a claim waits for a round with each reachable peer before work starts, so a cross-node loser mostly learns before it began (C4, "Confirm window"). Races are rare to begin with: within one class (repo, role) each node takes the tasks it owns first (C4, "Node-affine task choice"), a run announces its node before its drones start, and drones of one node start on different tasks (C4, "The first wave").
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
- **The combined tree is checked** (built, `hive/integrate.rs`). Each verifier judges one attempt, on its commits. Two attempts can each pass alone and still break together: in the W2 probe, two drones added the same annotation at different spots and git merged them cleanly. Before the goal counts as finished, the accepted attempts are merged and the goal's oracle, plus every task's own checks, runs on the result. Only the board's accepted attempts are merged, so work a drone did without a claim never reaches the result.
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
- **When a drone ends:** a pure `step` ends a drone on a finished board, its spent share, an unmetered (or $0) session, two failures, two sessions without board progress, or three idle polls with no work in flight (a drone waits while another holds work whose verify task it could take; a poll ends early when the board changes, and then does not count).
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
- **The row's `e` object holds:** each node's attempts done and verified, verdicts, integrations and bundles (made, received, taken, refused, waiting); the judgements made away from the author's node, counted only where that node took the attempt's bundle (`cross_node`; `on_board` is what the board alone shows); whether both boards fold identically; the most holders of one task at once; the claims that lost a race, those whose session still did the work, and each loser's waste (`loser_waste_s`: from the claim to its session's next claim, release, done or block, or its iteration's end; p95 and max); the pump's rounds.
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

**Built, node-affine task choice (E2 of tasks/todo.md "Phase e"):** board.rs `next_for`, `owner`.
- **Before:** every drone on every node ordered the open tasks the same way (own repo, role, oldest, id), so two nodes' idle drones always picked the same first task and raced for it (arm E probe: two claims of one task 62 ms apart; the loser worked 83 s).
- **The order now:** the classes are unchanged (own repo first, then verify, integrate, work, plan: a verify or fix task keeps its urgency). Within a class a node takes the tasks it owns first, oldest first, then the others newest first (below). The owner is the live node with the highest `blake3(node, task)`: rendezvous hashing, as borg-mesh places excerpt replicas (sync.rs `replicas`), so every node computes the same owner without a message, and a node joining or leaving moves only the tasks it gains or loses.
- **Another node's tasks newest first:** once a node's own share is taken, it takes the others newest first, the far end of the share their owner takes oldest first. Two nodes then overlap only where their drones outnumber the tasks (k + k > m), and there exactly the pigeonhole floor max(0, 2k − m). A review measured the first version, which took them in hash order: 0.64 tasks per wave picked on both nodes at k=3, m=6.
- **Live nodes** come from the board alone (`Board::live_nodes`): this node, and each node whose sessions wrote a line (claim, heartbeat, done, a verify task's open) within a lease (30 min). An engineer's lines (`engineer:` opens, releases, verdicts) do not count: a node that only opened the goal may run no drone, and its share would wait last on every other node, which then race for it (the first version counted them: 1.18 tasks per wave on both nodes at k=3, m=6 with the goal opened on a third node). On one node it owns every task, so the order is the one of before. Drones on one node took the same first task and settled it locally (`CLAIM_SETTLE`) until E2b.
- *Proof (0 tokens):* board.rs `drones_on_two_nodes_rarely_pick_the_same_task`, a Monte Carlo of 300 trials per cell: m work tasks with random ids opened by an engineer on A (a node with drones) or on E (one without), a drone line from each of A and B, then 2 nodes × k drones picking at once (each node's drones one after another, a local claim seen at once, the other node's not). It counts the tasks picked on both nodes at any index (each is a lost claim), per wave. Before node-affine choice every cell was k (all picks the same). The first version (hash order for another node's tasks, an engineer's line counted) → now, goal opened on A: k=1 m=3 0.067 → 0, m=6 0.007 → 0, m=12 0 → 0; k=2: 1.09 → 1.00 (floor 1), 0.100 → 0, 0.003 → 0; k=3: 3 → 3 (floor 3), 0.643 → 0, 0.017 → 0. Opened on E: k=1 0.157, 0.030, 0.003 → 0; k=2 1.27 → 1.00, 0.337 → 0, 0.020 → 0; k=3 3 → 3, 1.18 → 0, 0.137 → 0. The test asserts no overlap in any trial where both nodes own at least k tasks (the pre-registered "0% while shares last"; the first version met it with the goal opened on A, and failed it opened on E, where E took a share) and at most 1/m tasks per wave beyond the floor (every cell now sits on the floor). `one_node_takes_the_oldest_first_and_two_keep_the_role_order`: one node keeps the old order, a node whose last line is older than a lease takes no share, nor one with only an engineer's lines, and a verify task another node owns still goes before work this node owns.
- *Measured, failed (sim arm E, 20 cells, E1 + E2 together, 2026-10-25):* 79 lost claims, against the pre-registered ≤ 1 in total (E1 alone: 79; before both: 93). The losers' waste splits into about 0.3 s (drones on one node, settled at the claim's read-back, which E2 leaves alone) and about 2.6–2.8 s (across nodes, told at the next tool call). The cross-node races fall in the first wave, before either node's drones wrote a line: each node then counts only itself as live, owns every task and takes the same first one, the case under *Deferred*. E2 stays in (it orders within a class only and removes the overlap once the nodes see each other); the first wave is re-planned with E3 and E4.
- *Deferred:* liveness is each node's own view. A node whose sessions wrote no line within a lease (a runner that just started, or drones idle for 30 min, say waiting for a plan approval) is not live for the others until its next line syncs, while it counts itself: the two then order one class differently and race about half the time with one drone each (a review's scratch run: same pick 0.49–0.53 for k=1, 1.56 tasks per wave on both at k=3, m=12). E1 tells the loser, and E4's confirm window stops its work (a run's start no longer counts here: E2b announces it). A node that left the hive keeps its share for up to 30 min: its tasks wait behind the newer ones of their class on the others (delayed, not starved: `next_for` returns them once a node's own share is taken). With more drones than tasks (k + k > m) the overlap is the pigeonhole floor. More than two nodes whose shares ran out all take the far end of the others' shares, and may overlap there; not measured.

**Built, a lost claim told at the next hook and set aside (E1 and E5 of tasks/todo.md "Phase e"):** hive/mod.rs `lost_claim`, `Ctx::lost_told`, `set_aside_lost`, `set_aside`, `release_told_loss`, `pre_tool`; hook.rs's pre_tool arm.
- **Before:** `post_tool` returned at once for any tool but an edit, so a drone heard of a lost claim only at its next prompt or stop: across nodes, where the loss shows only after a sync, the loser worked on to its turn's end (arm E probe: 83 s, about 6% of the cell's dollars), and its tree kept the lost work into its next task.
- **Told at the next hook:** every hook (prompt, post_tool after any tool, stop, and pre_tool for an edit) tells a lost claim once, marked in the session's state (`lost_news`, `#borg-hive-lost`).
- **Edits refused:** while the session's latest claim is one the fold voided, or one it was told it lost, and it holds no task (`lost_claim`), pre_tool denies the edit and write tools (hook.rs `edit_text`'s: Edit, Write, MultiEdit, NotebookEdit) with the news and "It is not yours" (the steering pause's deny). A new claim lifts it. Other tools run, so the drone can still claim, read and end its turn. The deny comes before the guard (hive/guard.rs), so a refused edit of a test or CI file raises no flag: it never ran.
- **Set aside (E5), at the stop or the next claim:** the worktree goes back to where the lost claim started: a snapshot commit of the tree now (verdict.rs `snapshot`), pinned at `refs/borg/hive/<goal>/<sha>` (commits.rs `pin`, so gc keeps it), then `take_out` of start..snapshot, a `git apply -R` that leaves the index, HEAD, refs and ignored files alone. The news names the snapshot commit and the `git diff` that shows it. It runs in the stop hook (no claim held) or in `borg hive claim` before the next claim takes its base, never in a tool call's or prompt's hook: opencode and omp give those 2 to 5 s and then kill borg, failing open. A tool call's hook says when it will be set aside. Marked once (`#borg-hive-set-aside`) before it starts.
- **Where the claim started:** each claim whose read-back saw it won records the tree it started from and its git top in the session's state (`#borg-hive-claim-start task@ms sha top`). That is its base, but for a rework a snapshot taken at this claim: a rework's base is its first claim's, older than other work done in the same tree since (another task's, say), which a take-out to the base would revert. A claim whose own read-back saw it lost records none: it already took out the deps it brought in, and nothing is set aside. A set-aside tree keeps the deps' work the claim brought in, as it is in the start.
- **What a reset never touches:** held work (it runs only while the session holds no task and its latest claim is the lost one); a tree another session of this node holds a task in (its claim-start names the same git top); a tree whose snapshot fails or cannot be pinned (left as it is, and the drone is told).
- **A loss the fold takes back:** a line that syncs later (the winner's release, written before the loser's claim) can make a lost claim win again. Once the drone was told it lost, every hook releases such a task (`release_told_loss`, counted as a release in `stats`): the drone moved on and its tree may be set aside, so it does not hold the task unawares while its heartbeats renew it.
- *Proof (0 tokens):* hive_cli.rs `a_claim_lost_to_another_node_is_told_at_the_next_tool_call_and_its_edits_are_refused` (a remote claim 5 ms earlier wins: `post_tool(Bash)` tells it once, `pre_tool(Edit)` is denied with "It is not yours", `pre_tool(Bash)` runs, a claim of t2 lifts it); `a_lost_claims_worktree_is_set_aside_at_its_stop_in_a_pinned_snapshot_and_reset_to_its_base` (post_tool tells and leaves the tree; the stop resets it: the tree equals the base, an uncommitted change from before the claim kept, the snapshot holds the changed and the new file, is pinned, and `git diff base snapshot` names them; once); `a_lost_claim_is_set_aside_at_the_next_claim_before_it_takes_its_base`; `a_second_loss_of_a_task_claimed_again_sets_aside_only_this_claims_work` (t2's done work, made between two lost claims of t1, stays); `a_lost_claim_the_fold_gives_back_after_it_was_told_is_released`; `a_refused_edit_of_a_test_file_raises_no_flag`; `a_lost_claim_in_a_tree_another_session_holds_a_task_in_is_left_as_it_is`; `a_lost_claim_learnt_while_holding_the_next_task_resets_nothing`. mod.rs `a_lost_claim_is_set_aside_to_its_own_start_and_only_when_it_has_one`, `a_session_holding_a_task_has_no_lost_claim_whatever_its_latest_claim`. Each review fix's test fails without it: the post_tool reset, the base instead of the start (t2.txt gone), the flip (t1 stays claimed), the flag (one line in flags.jsonl), the next claim (the lost file in t2's tree); the shared-tree test failed on the first version too, whose check read the joins file, where a session joined with `--session` has no line; the holding test fails with the holding check deleted.
- *Measured (sim arm E, 0 tokens):* borg-eval `hive_e2e_e_sim_loser_waste_before_and_after_hook_news` (`BORG_E_CELLS=20 cargo test -p borg-eval hive_e2e_e_sim_loser -- --ignored`), 20 cells per side, 4 tasks, sim drones holding a claim 10 s, the pump syncing every 2 s, on HEAD plus E1/E5 only (E2's task choice not in), measured before the review moved the set-aside to the stop and next claim. Before (the sim drone fires no hook while it works, as a drone that hears only at its turn's end): 98 lost claims, loser waste p50 0.56 s, p95 12.3 s, max 30.3 s; every loser that believed its claim (a cross-node race) worked about 10.7 s, its whole work time. After (post_tool every 2 s): 79 lost claims, p50 0.66 s, p95 2.85 s, max 2.91 s, within the pre-registered sync latency + 2 s (4 s). `lost_then_done` 0 on both sides (the stop already refused a lost done). The sub-second values are claims lost at their own read-back, retried at once.
- *Deferred:* a lost verify claim refuses edits but has no tree to reset to; a snapshot that does not finish in what is left of the stop's time (40 s in `borg hive claim`; a very large tree) leaves the tree as it is and says so; a session that ends without a stop and never claims again leaves its lost work in the tree (the runner's next session in that worktree takes it into its base); the refusal and the set-aside are not counted in `stats` (the board's lost claims and arm E's `loser_waste_s` show them); pinned snapshots are never pruned.

**Built, sync on write (E3 of tasks/todo.md "Phase e"):** borg-mesh daemon.rs `hive_poll`, `live_hive_logs`, `backoff_wait`; sync.rs `Node::synced`, `Node::unreachable`; borg presence.rs `synced_path`, `note_synced`, `forget_synced`; borg-eval arm_e.rs's pump.
- **The daemon dials at once** when a hive log changes: one tick every `hive_poll` (25 ms) stats the synced logs of the goals being worked on (`live_hive_logs`: a goal with a file changed within a lease, 30 min; a run's start writes a line), one stat per file, and the whole store's fingerprint, the gate and the membership are looked at every `poll` (2 s) as before, which also makes a goal live at its first change. Goal directories are never pruned and peers' goals arrive by sync, so a tick that statted every goal grew with the team's history: an idle node with 2000 goals took about half a core (debug build), 1% now. A hive change dials without stamping in the daemon: each session stamps first (sync.rs `exchange`), so the stamp's reading and hashing of every log runs per session, not per tick. A stat per file, no new dependency: a filesystem watcher (the `notify` crate: inotify, FSEvents, kqueue) would save the stats but adds a recursive watch of goal directories that appear while it runs, and its events are coalesced or dropped under load, so the poll would stay as the fallback. What the stamp and a quarantine write before a round goes in that round.
- **One dial per peer, side by side:** a round spawns a dial for each member it owes a change, and a peer whose dial is still running is owed it again for when that dial ends. A member that does not answer (a laptop asleep, a closed endpoint) used to hold every round for its `timeout` (30 s), and so every claim to a live peer behind it; it holds up only its own dials now. A failed dial keeps what it did not carry owed to its peer, which is dialled again once its backoff is over, even with no new change; the first retry comes a `poll` (2 s) after the failure, then `interval · 2^(failures − 1)` (2, 4, 8 … min, at most 30). The failure also drops the peer from `synced.json`, so no claim waits on it until a session completes again.
- **The pump of arm E** does the same: a session as soon as a hive log changed on either store (looked at every `hive_poll`), at least every 2 s.
- **Rounds are recorded** per member: when the last session with it that completed started, dialled or served, in `<store>/mesh/synced.json` (`{"<key>": ms}`, local, an older one never moves it back unless the held one is later than the clock now: a clock stepped back would keep it ahead of every claim; `heard_back` also takes no record later than now as proof). Every line this node held before that ms was offered to the member in it, so E4 can ask "has a round with each live peer completed since my claim".
- **No protocol change.** In a session the four lesson logs come before the hive's, and the order is the protocol (`borg/mesh/10`). Measured on loopback, the four lesson logs with nothing to move took about 6 ms of a 120 ms session (one fingerprint each; debug build with unoptimised signatures, where checking the membership history and origins took about 100 ms). Hive logs first would save those few ms for a bump every member must take: left for `borg/mesh/11`, with a reason to bump.
- **Debug-build crypto left as it is:** unoptimised signature checks make a loopback session about 150 ms in a debug build (about 1 ms optimised). The default suite's timing bound (p95 under 1 s) holds in a debug build (p95 172–192 ms); the pre-registered 300 ms bar is an ignored test for an idle machine (p95 45 ms with the dalek crates optimised, 320–347 ms without them while other suites ran). Optimising those crates in the dev profile was tried and not kept: it is a build-profile change nobody asked for. Release builds are optimised.
- *Proof (0 tokens, pre-registered: p95 < 300 ms over ≥ 20 trials):* sync.rs `a_claim_reaches_the_peer_within_300_ms_at_p95` (ignored: `cargo test -p borg-mesh -- --ignored a_claim_reaches` on an idle machine; p95 45 ms on the last run) and, in the default suite with a bound any machine running it meets (p95 < 1 s; before E3 the median alone was 1.9 s), `serve_carries_a_claim_to_the_peer_at_once`: two `serve` loops with the shipped options on loopback, a claim appended to A's board 24 times off the loops' beat, timed until it is in B's `tasks.jsonl` (checked every 2 ms); after each, both stores' `synced.json` name the other with a start at or after the claim. Before (the 2 s poll, `hive_poll` set to 2 s): p50 1860 ms, p95 2386 ms, max 3847 ms, FAIL. After: p50 32–34 ms, p95 44 ms, max 46 ms, PASS (without the signature optimisation: p50 150 ms, p95 192 ms, against 1866 / 1930 ms before). With `note_synced` not called the test times out at the first trial's record; presence.rs `a_members_last_session_never_moves_back` fails with the older-start check removed, `a_record_ahead_of_the_clock_gives_way` with the clock check removed. sync.rs `a_peer_that_does_not_answer_holds_no_claim_back` (three members, carol's endpoint closed, a 3 s timeout, 15 claims over 6 s: each at bob within 1 s, and carol's record gone after her first failed dial; with a round held while any dial runs, two claims took 2.6 and 2.9 s; without `unreachable` the record stays), `a_failed_dial_is_retried_soon_with_what_it_missed` (alice's dial to bob fails before his `serve` starts; the goal reaches him with no new change and no dial from him; times out with the old backoff of `interval · 2^failures`), `an_idle_node_with_many_old_goals_stays_idle` (2000 empty goal directories, this thread's CPU over 3 s: 1% of a core; 47% when the 25 ms tick lists and stats every goal).
- *Measured (sim arm E, 5 cells per side, 0 tokens, not the pre-registered 20):* `BORG_E_CELLS=5 cargo test -p borg-eval hive_e2e_e_sim_loser -- --ignored` with E1, E2, E5 and E3: before side (no hook news) 19 lost claims, loser waste max 0.41 s; after side 17 lost claims, max 0.41 s. Every remaining loss is a race on one node settled at the claim's read-back (about 0.3–0.4 s); no cross-node loser kept working (wave 1, pump every 2 s: about 2.8 s each with E1). Lost claims stay far above E2's ≤ 1: the same-node races are E2b's. A run beside other test suites showed two losers at 6.95 s (the pump's sessions slowed by the load).
- *Deferred:* a claim written while a dial to its peer moves a goal's bundles (up to 64 MiB a frame) waits for that dial; after a second failure in a row a peer backs off for minutes, and a claim meanwhile reaches it only when it dials in (E4 no longer waits on it: its record is gone); one run of the full borg-mesh suite in about twenty timed out on a claim (not reproduced; the first-failure backoff of 2 min was the likely cause, now a `poll`); sessions per hive line: a session carries every lesson log, every goal's logs and the membership, and each receiver passes the line on to every peer it knows, so one claim costs about 2·(n − 1)² sessions for n nodes (6 for 3), bounded and convergent (no session while idle) but near-continuous under a steady write rate on a large store (a burst of 60 claims took 1.6 s to reach the peer with 500 goals): a session limited to the changed goal, or hive logs first, is `borg/mesh/11`'s; a goal quiet for longer than a lease is looked at every 2 s only; the latency across real machines (the handshake grows with the round trip, and through a relay more).

**Built, the first wave and the confirm window (E2b and E4 of tasks/todo.md "Phase e"):** hive/mod.rs `announce`, `reachable`, `heard_back`, `claim`, `Confirm`; run.rs `main`, `RunLine`; board.rs `next_for`, `idle_slot`; integrate.rs `run`; borg-eval arm_e.rs (`confirm_ms`, `start_wait_ms`).
- **Before:** wave 1's sim arm E had 79 lost claims in 20 cells. Its cells show three kinds: the first wave across nodes (each node live only to itself until its drones' first line synced), drones of one node reading one board and taking the same task, and a pick whose claim was written 100 ms or more later (a snapshot or git call between) after another drone's claim of it had landed, the integrate claim of two drone threads included.
- **A run announces its node** (E2b): once its worktrees are made, `borg hive run` writes a heartbeat of no task by `runner`, which makes its node live in `Board::live_nodes` and does nothing else in the fold (a node from before folds it the same: no format change). With peers reachable it waits until a sync round with each completed after that line (E3's `synced.json`, a start later than the line's write), at most 3 s, prints it and keeps `peers` and `waited_ms` in `runs.jsonl`. A peer is reachable when its last completed session started within 2 minutes (the daemon syncs each peer at least every minute); none on one node or with the daemon stopped: no wait.
- **Drones of one node start apart** (E2b): within a class, drone `dN` walks `next_for`'s order (the share oldest first, then the others' far end) from its place among the node's idle running drones (N − 1, less the drones before it that are busy or ended), over the class's tasks still open, so a drone that sees another's claim picks as one reading the board before it: the claimed task left the order and its drone left the places before it. (The first version also kept the node's held tasks in the order, which counted a busy drone of the same class twice: with t1..t5 open and d1 on t1, d2 and d3 both took t2.) The runner names the drones it still runs on the board (a heartbeat of no task by `runner`, note `drones d1 d2 …`, `Board::rosters`): at its start, then at each drone's end, so a drone that ended (its share spent, nothing to do) takes no place and the oldest task of the share is not left waiting behind it. A place past the share walks on into the others' far end instead of wrapping onto a task of the share another drone takes. Priority classes are unchanged; a session without a runner's drone name starts at the first place (the order of before). The plan said blake3(session or drone, task); a hash order gives two drones the same first pick with chance 1/share (one in two with the sim's two tasks a node), places give none while the share lasts.
- **A claim is not written when a rival's already landed** since the pick: `claim` folds the board again just before the write (after the snapshot and the deps) and stamps the claim's ms after that fold, so a rival's line that lands between stamp and write is limited to the append; `integrate::run` checks the integration is still due after its git calls (`claim_due`). No line, so no lost claim; the drone takes the next task.
- **The confirm window** (E4): after `CLAIM_SETTLE` and the read-back, with drones live on another node and peers reachable, `claim` waits until a round with each completed after its write (at most 1 s, two rounds of 500 ms), folds again, and on a loss takes out what it brought in and refuses as `Lost`, before the drone starts. Each window is a line in the goal's local `confirms.jsonl` (`won`, `lost`, or `cap` when a peer's round did not complete in time and the claim stands as before). What `won` proves: every line on the peer's disk when its round read the logs has arrived. A rival claim stamped earlier that landed on its node after that read still wins later (the clocks' skew plus the rival's stamp-to-write time longer than this claim's write-to-round time; skew is allowed up to well below a lease), and its loser hears it at its next hook (E1): `won` is not "no late loss".
- *Proof (0 tokens):* board.rs `drones_of_one_node_reading_one_board_pick_apart` (1, 2 and 3 drones on one or two live nodes, m ∈ {2, 3, 6, 12}, 50 trials: picks from one board are distinct while there are tasks, within the node's share when it holds them, and the same as picks made seeing each other's claims; fails with the place left out, and with the walk kept within the share) and `a_busy_drone_takes_no_place_in_its_nodes_share` (fails with `slot` in place of `idle_slot`); the E2 Monte Carlo is unchanged in every cell. mod.rs `a_claim_beside_another_nodes_drones_waits_for_a_round_with_each_peer` (B's earlier claim arrives with the round after the read-back: `Lost`; with no rival: `won`, within the cap; fails with the window off), `a_claim_waits_no_longer_than_the_cap_and_not_at_all_on_one_node`, `a_runs_start_makes_its_node_live_and_waits_for_the_peers_round` (fails without the line, and with no wait), `a_claim_of_a_task_a_rival_took_since_the_pick_is_not_written` (fails without the check). After review: board.rs `a_busy_drone_of_the_same_class_is_counted_once` (both picks t2 with held tasks kept in the order, both when d2 sees d1's claim and when it does not) and `an_ended_drone_takes_no_place` (t2, not t1, without the roster); mod.rs `a_claim_is_stamped_after_its_pre_write_read_of_the_board` (a 20 000-line board: the claim 145 ms older than its write when stamped before the fold), `a_peer_record_from_a_clock_now_behind_it_proves_no_round`; integrate.rs `an_integration_that_landed_since_it_was_found_due_is_not_claimed_again` (a second open and a lost claim written without the recheck). The confirm-window test drives `confirm` directly after the claim, so the rival's line always lands after the read-back however loaded the machine (the first version timed it against the read-back).
- *Measured (sim arm E, 3 cells per side, 0 tokens, not the pre-registered 20):* `BORG_E_CELLS=3 cargo test -p borg-eval hive_e2e_e_sim_loser -- --ignored`: 0 lost claims on either side (pre-registered ≤ 1 over 20), so no loser waste. Every confirm window waited 0 ms (48 windows, all `won`: the round the claim started had completed within `CLAIM_SETTLE`), so the added claim latency is the read-back again, median 0 and max 0 ms beyond it; run start waits median 35–41 ms, max 67 ms. On the way there: with the announcement, the window and places that wrapped within the share, a first cell had 4 lost claims (a cross-node pick of the far end, two same-node picks written 100 ms after the rival's claim, two integrate claims); with the pre-write check added, 1 + 3 and 2 + 0 over 3 + 3 cells, each of the 4 whose boards were kept two drones of one node at the same ms whose share held one task (a place that wrapped), which the spill past the share and `idle_slot` removed. Read with care: the two sides differ only in the hook's polling (both run E2b and E4), so neither is a baseline against the commit before; no window waited, so the sim would score the same without E4 (its loss path is proven by the unit test only); and the sim runs two drones on one node and one on the other, so it could not show the busy drone counted twice (3 or more drones on a node).
- *Deferred:* runs started further apart than one round still race in the first wave (the later node's announcement reaches the earlier one's drones only with its round; E4 then stops the loser's work, but the claim counts as lost); a peer that just went away costs each claim the 1 s cap until its first dial fails (at most the dial `timeout`, 30 s); the confirm window waits for every reachable member, not only those live on the goal (the board's node ids are not member keys; the dials run side by side, so the wait is the slowest member's session, not their sum); integrate claims across nodes get the pre-write check, not the window; more drones than a class's tasks (a pigeonhole) still race.

**Fixed, a rework's base is its own node's** (run d3-accept2 w1-s2 lost a hidden check): hive/mod.rs `first_base` matched a rework's first claim by drone name alone, so node B's d1 took node A's d1's base, a tree holding another task's fix; the attempt's diff against it reverted that fix and integration dropped it. It now matches the drone on its node (`same_node`), as `barred`, the verdict's author check and `sessions_of` do. *Proof (0 tokens):* `a_rework_takes_the_base_of_its_own_nodes_drone_never_another_nodes` (fails with the node check left out).

**Fixed, a verifier is told where the work is** (9 of 10 cross-node rejects in d3-accept2 said "no change found" though the attempt's diff had it): the drone prompt sent verifiers to `joins.jsonl`, which maps only this node's sessions to worktrees, and the claim never named the attempt. hive/mod.rs `describe` now names it, `git diff <base> <head>`, says its commits are in this repo (worktrees share it; another node's arrive with its bundle, `absent_from`), and gives a command that exports the head with `git archive` into a directory of the claim's own (task, head and session; cleared first) to run the checks on, then removes it. `git archive` only reads the repo: a codex drone's `workspace-write` sandbox may not write the repo's `.git`, which `git worktree add` does, and a worktree left registered at a fixed path made a re-claim's `worktree add` fail. The verifier's own worktree is not reset to the head, as a fix task's is to its `from`: its next work would start from an unaccepted change. The sim drone verifies on a checkout of the head (`hidden::with_checkout`) and rejects when it fails, never falling back to the author's worktree. *Proof (0 tokens):* `a_verify_claim_of_another_nodes_attempt_names_its_commits` and the hive CLI test `a_verify_claim_of_another_nodes_work_names_its_commits` (both fail with the naming left out), `a_verifiers_copy_of_the_work_can_be_made_again_and_registers_nothing` (fails with a fixed `worktree add`), `a_sim_verifier_judges_the_commits_and_never_falls_back_once_they_fail_to_check_out` (fails with the fallback).

**Fixed, an idle owner keeps its tasks** (d3-accept2 w2-s2: node b did nothing, node a's idle drones took all of b's tasks while b's runner slept 3 s between looks; 9 sessions ran empty, refused at `claim`'s pre-write check, so `lost_claims` stayed 0): board.rs `next_for` leaves another node's task to its owner while the owner's roster names more idle drones that may claim it than it has tasks before it that those drones may claim (`left_to_owner`). The claim is made inside the headless session, seconds to tens of them after it starts, so a session that has not claimed by the end of its task's notice time (claimable since + the 3 s idle wait) says on the board that it started, stamped with its start, and later that it ended unsettled (`session-start dN`, `session-end dN`, `Board::sessions`; run.rs `run_session`): a drone whose session started and has not claimed keeps the task; one that started none keeps it for `OWNER_GRACE_MS` (4.5 s: the owner's idle wait, a confirm window and one round, the time for its start to arrive) after it became claimable. A session that claims within the notice time writes no line: each line starts sync rounds, and a claim that lands during one waits for the next. A busy owner's tasks, those past its idle drones, and those its session passed over (it claimed another or ended) spill at once. run.rs `idle_wait` wakes an idle drone on a change of the board's file (a 25 ms look, capped at the 3 s wait), and a wait a change ended is no idle poll. Each claim refused before its write is a `refused` line in `confirms.jsonl`, counted at the run's end and in arm E's `nodes.*.refused_claims`. *Proof (0 tokens):* board.rs `another_nodes_task_waits_for_its_idle_owner_then_spills` and `a_task_the_owners_idle_drones_may_not_claim_does_not_count_ahead`, hive_run_cli `three_sim_drones_finish_a_board` (every session settled), run.rs `a_session_says_it_started_only_once_unclaimed_past_its_tasks_notice_time`, run.rs `an_idle_drone_wakes_when_the_board_changes`, mod.rs `a_claim_of_a_task_a_rival_took_since_the_pick_is_not_written` (each fails with its fix left out).

**Fixed, the regression the idle owner's fix brought** (sim arm E, after side: 5 lost claims over 20 cells and a confirm-window median of 116 ms, against 0 and 0 ms at the commit before on an idle machine). Three causes, read from the kept boards and the sync pump's round times. (a) Two drones of one node woken by one change of the board (`idle_wait`) started sessions for the one task open to them and claimed it in the same millisecond: the places of `idle_slot` wrap onto one task when a node has fewer tasks than idle drones, and the blind 3 s sleep had kept their wakes apart. (b) A place-based walk lands two drones on one task when one of them may not claim a task before it: two drones done at once, each barred from its own work's verify task, with another node's task left to its owner, both took the third. (c) Each session's start and end line started two sync rounds (a round that brings lines changes the peer's log, which starts one more) about 300 ms before the sim drone's claim, so the claim landed during a round that had started before it and its confirm window waited for that round and a whole next one. Fixes: board.rs `next_for` gives each idle drone of a node, in turn d1, d2, ..., the first task in its order that it may claim and no drone before it took (`free_for`), so drones reading one board pick apart whatever bars them, and a runner starts no session for a task an idle drone before it takes (`taken_before`: the work is in hand, so the wait is no idle poll and no stall); a session that runs still takes the first task there is. The session lines are written only for a session still unclaimed past its task's notice time (above). A claim of a picked task (not one named) is refused before its write when the board then leaves the task to another node's idle drones (`left_to_another`): a run that started a moment before its peer picked from a board without the peer's runner line. *Proof (0 tokens):* board.rs `a_drone_before_that_may_not_claim_a_task_leaves_it_to_the_next` (both take t2.v1 with the place-based walk), `a_runner_leaves_the_task_an_idle_drone_before_it_takes`, mod.rs `a_picked_task_another_nodes_idle_drone_now_takes_is_not_written`, run.rs `a_session_says_it_started_only_once_unclaimed_past_its_tasks_notice_time`; the E2 Monte Carlo and `drones_of_one_node_reading_one_board_pick_apart` unchanged. *Measured (sim arm E, 20 cells per side, 0 tokens, a machine not idle):* the commit before, 4 after-side cells side by side: 3 lost claims, confirm median 61 ms. With the fixes: before side 1 lost claim, confirm median 61 ms (max 189); after side 3 lost claims (2 without an end, boards not kept) and median 56 ms (max 214) in one run, 0 lost claims and median 56 ms (max 219) in a second; run start waits median 157–159 ms; 0 to 3 claims refused before their write per cell. The 0 ms medians of the commit before were measured on an idle machine; on this one it waits as long.

**Fixed, a task left to a peer's slow session was not waited for** (d3-accept3 W2 s1 and s2: the owner's session claimed 15.1 s and 12.3 s after it started, the other node's idle drone saw the task hidden by `left_to_owner` but nothing in hand, ended "nothing to do" after its three idle polls, and the verify task only it could take was stranded, both runs ending unfinished): board.rs `in_hand` now counts as work in hand a task the drone may claim that is left to another node's idle drone, and a live peer's session that started and has not claimed yet, whatever task it is on (the mirror case, seen on the first mesh-test run: a node whose drone may not verify its own work, with the peer's verifier slow to claim, ended and left a rework without a verifier), beside a held task and one an idle drone of its own node before it takes; run.rs `open_to` gives that one answer to the wait and the stall count. The wait is bounded by a lease, not seconds: a peer's session counts only while it has neither claimed nor ended (a task is also left to it for `OWNER_GRACE_MS` after it became claimable), and only while its node wrote a line within `LEASE_MS` (30 min). A peer stopped mid-session without its session end (the runner writes it only once the session returns) is waited on for up to that lease, as an abandoned claim is. *Proof (0 tokens):* board.rs `a_task_left_to_a_peers_starting_session_is_in_hand`, `a_peers_starting_session_is_in_hand_for_a_drone_barred_from_its_task` (fails with the session term left out), borg-mesh hive_mesh `a_peer_slow_to_claim_its_task_is_waited_for` (two runs over loopback, the owner's sim drone claiming 15 s into its session, `claim_s`; both fail with the fix left out).

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
