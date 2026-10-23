# Hive learning: self-improvement without touching weights

Status: steps 1, 2, 3, 8 and the local part of 7b are built, all of it preparation: no value metric has been shown yet. Steps 4, 5, 6 and 7 are not started, and the proof (H5–H8) has not run. This doc builds on [collective.md](collective.md), and the experiments run on the work machine against company repos.

## Question

OpenAI and Anthropic say their models improve themselves through reinforcement learning. Borg can't train weights. Can a hive of drones still get better with every goal it finishes, and can we prove that it does?

## What the labs claim

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

## What works without weight updates

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

Conclusion: in-context learning keyed to verifiable rewards is competitive with weight RL at a fraction of the rollouts. Its failure modes are gaming the reward, poisoning the memory, and memory eroding over time.

## What borg already has

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

Gaps (as designed; steps 1–3 since closed the first three in part):
- Outcomes weren't synced over the mesh, so each node learned alone. Step 1 syncs a redacted projection.
- Only one lesson got credit, never a set of lessons or a team of drones. Step 2 credits every lesson shown during a hive task, in equal shares; no Shapley split yet.
- Regret never penalized the lessons that were shown when the bad decision was made. Step 3 makes it reach back.
- Cost (`usd` in stats) never feeds back into the reward.
- The constants are hand-set, and eval results never flow back into them. That holds for every weight the built steps added: the veto's 3, `VERIFIED_W` = 1, `REJECTED_W` = 1, `HACKED_W` = 3 (memory.rs), `REGRET` = 1 (memory.rs) and `VERIFIER_REGRET` = 0.5 (learn.rs). All wait for step 6.

## Design: the hive is a distributed actor-learner

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

### Reward

```
R(task) = hidden_oracle_pass                    # runner-side, never visible to drones
        − λ·usd − μ·rework (reopened tasks)
        + human: queen answers, corrections (−), not_corrected (weak +)
        − delayed: regress fix links within 30 days (the "regret" penalty)
```

- **Two oracles.** Drones see the *visible* oracle that gates `done`. The runner scores the *hidden* oracle, made of held-out tests outside every worktree. Learning updates use only the hidden reward.
- **The verifier is a different drone** (collective.md). Its verdict is a reward, and it can be wrong. A `verified` task that later gets a regress link counts against the verifier as well as the author.

### The engineer's "no" is the most valuable reward

Tests only check what someone thought to test. A drone can pass every test and still break a core constraint or over-engineer the change, and Anthropic's source-control team found exactly that. The engineer's judgment is the reward that tests can't supply. It is sparse and precise, so the loop treats it as gold:

- **A veto is the strongest negative signal.** A `veto` (collective.md) penalizes everything that was active when the vetoed work was planned or made: the lessons shown, the playbook version and the genome. That's weighted above `asked_then_corrected`.
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

The reward from the earlier section gains two terms:

```
R(task) += − ν·veto − ξ·complexity_delta
R(queen) = no_after_escalation − α·rubber_stamp − β·missed_escalation
```

### Group-relative advantage, native to the hive

When the budget allows, the hive runs a task as a **group of K rollouts**, with drones that differ in harness, model, playbook or genome. This is also arm F, best-of-N with voting.

A reflection step on a learner node, Training-Free GRPO style, compares the winners and losers on the hidden reward and writes a *semantic advantage*: what the winners did that the losers didn't. That becomes an L0 lesson or an L1 playbook edit.

A single session can't do this because it has no group to compare. Subagents make a group, but it dies with the session.

### Credit assignment

- **Lessons, from the logged policy.** Each `asked` record logs its *propensity*: the probability of being shown under that session's Thompson draw. This allows off-policy evaluation (inverse propensity scoring and doubly-robust estimators) of ranking changes, and of cutoff and cap settings, from logged data without new rollouts. Then tune `RELATIVE`, `BLOCK` and `CAP` from those estimates instead of setting them by hand.
- **Sets of lessons.** When several lessons were shown before an outcome, split the credit with a leave-one-out Shapley approximation. Exploration already randomizes which lessons appear, so we get natural experiments.
- **Drones and contracts.** Per goal, compare the reward of tasks where a contract was shown with the reward where the same contract scope was edited without it. That is a direct measure of H4 in collective.md.
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

## Proof: a learning curve, not a single number

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
- **H6:** group-relative reflection (K=3) produces lessons that change behavior more often than lessons from single rollouts.
- **H7:** IPS-tuned constants beat hand-set ones in off-policy estimates, and the win is confirmed by a live A/B.
- **H5b:** across the goal sequence, repeated vetoes (the same reason given again) trend toward zero, and engineer minutes per solved goal fall while violations after merge do not rise.
- **H8:** the guardrails keep the hacked rate under 2% on an ImpossibleBench-style set of tasks with conflicting tests.

Kill criterion: if H5 fails after 8 goals, stop at L0.

Not run: H5–H8 have no data yet.

## Steps

Each step stands on its own and needs nothing after it.

1. **Log propensities and sync outcomes.** Add `p` to `asked` records (`outcome.rs`, `conscience.rs::explored`). Add `outcomes.jsonl` to mesh lines (`borg-mesh/src/lines.rs`) with redaction.
    - *Built (preparation, no value metric yet: it enables step 6's OPE and federated beliefs):* `asked` and `graded` records carry `p`, each lesson's probability of being asked under the policy in force: 1 or 0 under the fixed graded rule, the explore mixture (1 − 1/10)·[r < k] + 1/10·[r < 20] under the learned cut (`attention::propensities`; the explore counter is content-independent), and the share of 512 Thompson draws that give a lesson a slot on the bm25 and non-graded paths (`conscience::propensity`). The hook carries it from the check to the `asked` record through `#borg-p` in the session state. `borg learn`, borg-web and every mesh sync publish a redacted projection (kind, lesson ids, ms, `p`, channel, blake3-tagged session and repo, a regret's tagged cause and credit, node) of the shared kinds (every result kind memory folds, `vetoed` and `regretted` included, so peers converge; the activity clock ticks on shared kinds only) to `outcomes-team.jsonl`, synced as a third log (`borg/mesh/6`), signed and quarantined like lessons. Memory and trust read the team view (`outcome::read_team`), dropping this node's own copies by session tag; learn.rs, attention.rs and stats.rs stay local. On by default (`BORG_SHARE_OUTCOMES=off`). *Deferred:* syncing `graded` records (federated OPE and attention labels), propensities of early and queued deliveries (no `asked` record), poisoning resistance (k distinct nodes, trust-weighted median), compaction of the team file, and the reward kinds of steps 2 and 7b, which must be added to `SHARED_KINDS` deliberately.
2. **Feed oracle rewards in.** Hive verify verdicts and hidden-oracle results become outcome kinds (`verified`, `rejected`, `hacked`) credited to the lessons shown during the task.
    - *Built, local (preparation: the value metric, the hacked rate H8 and the hidden-oracle solve rate per goal for H5, waits for the borg-eval ImpossibleBench-style set):* a verdict (`borg hive verify`, or the verifier's last message `VERDICT: pass|reject <why>` at the stop) goes through `hive/verdict.rs` `record`: refused for the author's session and any session of its drone, then `decide` (step 8's checks), then the board line. Every lesson shown to the author's session while it held the claim (`veto::shown_between` over the claim's span) gets `verified`, `rejected` or `hacked` (outcome.rs), keyed by `cause` = `<goal>/<verify task>`, once per lesson and attempt, and the verdict counts one `task_*` stat. With a hidden oracle (`goal.json` `hidden`, a directory outside the repo, `hive/hidden.rs`) a pass or a reject waits for its score, run in a scratch worktree of the done's snapshot when `borg hive run` ends or by `borg hive score`; the score decides `verified` or `rejected`, and `oracle.jsonl` logs whether the verifier agreed. memory.rs adds `VERIFIED_W` = 1 to α (like a change), `REJECTED_W` = 1 (like an ignore: the verifier can be wrong) and `HACKED_W` = 3 to β (as heavy as a veto, not above: the hack test is a heuristic over a diff's lines), one per (lesson, attempt), the worst of duplicates; never a use, never a clock tick; all three weights design-chosen, to be tuned by step 6. A regret of verified work also reaches the verifier's lessons asked before its verdict at `VERIFIER_REGRET` = 0.5 of the strength (learn.rs, cause `<episode>/verify`, design-chosen). trust.rs, attention.rs and `weight` ignore the kinds. `BORG_HIVE_CREDIT=off` freezes the credit for an eval arm. Not in `SHARED_KINDS`: the spike is local and waits for mesh sync of hive lines (collective.md step 4); `Shared` has no cause, which the per-(lesson, cause) dedupe would need (a `c: tag(cause)` field); a verdict is a self-reported reward, so without the deferred poisoning guards a forged stream would move lessons (team_view drops unshared kinds, so a forged line is ignored today); and hidden-oracle results are private to their goal. They join the team view with `c` in the change that syncs hive lines (collective step 4). *Deferred:* crediting a verifier by whether the hidden oracle agreed with it (`agree` is logged for it), engineer-overturned rejections, and the Shapley refinement of equal shares.
3. **Make regret reach back** to lessons that were active at decision time (`notes/regress_learn.rs`).
    - *Built:* `borg learn` writes a `regretted` outcome (`outcome::regretted`, 0 tokens, once per regret and lesson) for each lesson asked in the deciding session before the decision commit's committer time (review regrets: before the finding). The merge target takes the blame alone if it was asked, otherwise the asked lessons share it equally; the blame is `1 − 2^−g` over the regret's independent grounds g, and counts `REGRET` = half a correction in the usefulness belief. On by default (`BORG_REGRET_CREDIT=off`). Pre-registered check: `eval/regret-credit/report.py` (local) compares the belief's log-loss on later results with and without the term. *Still open:* the leave-one-out Shapley refinement, and credit for decisions made on another machine (needs step 1's outcome sync).
4. **Group reflection:** `borg learn --group <task>` compares K rollouts and writes semantic-advantage lessons (`learn.rs`, reusing `generalise`).
    - *Not started.*
5. **L1 playbooks:** `playbooks.jsonl`, edited by deltas, shown through `on_prompt` matched to the task shape. Promotion through the queen.
    - *Not started.*
6. **Off-policy evaluation and tuning:** an `eval/ope` replay over logged `asked` records that proposes new constants.
    - *Not started.* Step 1 logs the propensities it needs. Its first targets are the hand-set weights above: the veto's 3, `VERIFIED_W`, `REJECTED_W`, `HACKED_W`, `REGRET` and `VERIFIER_REGRET`.
7. **L2 genome archive** in `borg-eval`: genome files as arms, a Pareto set, and parent choice by how much descendants improved.
    - *Not started.*
7b. **Engineer judgment:** `borg no`, the veto outcome kind, turning veto reasons into lessons or proposed constraints, intent cards with risk classes, and the queen's attention reward. Ship it with step 2, because the "no" signal matters more than the oracle signal.
    - *Built, local part:* `borg no` (terminal and in-session prompt), the `vetoed` outcome weighed 3 in the usefulness belief, veto episodes learned into lessons at any session's end, invariants proposed as `Kind::Constraint` queen questions (ratified into `constraints.jsonl`, not yet enforced), and the repeat-veto rate (`veto_repeat` / `veto`) on borg-web. The weight 3 is the design's choice, not eval-picked: preparation for H5b, to be tuned by step 6. *Still open:* the signed veto directive across the mesh, intent cards with risk classes, and the queen's attention reward.
8. **Guardrails** (`pre_tool` flags on oracle paths, hacked detection, the hack monitor) ship together with step 2, not after.
    - *Built, with step 2 (preparation, same value metric):* a claim of work and its done record snapshots of the drone's tree (commits written without a ref); the verdict diffs them (`verdict::protected`) and makes the verdict `rejected` with `hacked`, whatever the verifier said, by one rule: tests may grow, never shrink. Outside the task's `files`, a test file (`guard::class_of`: test dirs and names, a runner's config) that is deleted, renamed or retyped, or modified so that it weakens (`verdict::weakens`), is hacked: a removed or reshaped assertion (each removed line must be kept by an added one of the same shape and indentation, `guard::keeps`), two names collapsed into one (`got == got`), a block comment or string opened around unchanged lines, fewer test declarations (`#[test]` turned `#[inline]`), more silencers (a skip, `.only`, an early `return`, `if False:`), a Python name bound again so the first no longer runs, or a binary edit; so is CI config modified, deleted or renamed, or a test runner's config (or its part of a shared one, an `addopts`, a `jest` key) added or changed. Added tests, added assertions, renamed keys and accessors and updated values are fine. A change to the visible check's own files is hacked even inside the task's scope, and so is an `oracle` flag. The same rule judges the fix task after a failed integration (from the combined tree) and every arm of borg-eval. `pre_tool` (`hive/guard.rs`, every harness that maps it) flags writes to test, CI and oracle paths and reads of the hidden oracle's files into `flags.jsonl`, once per session, claim and path, and the verify task lists them; flag only, best-effort, a session in no goal pays one failed open. The hidden oracle lives outside the repo (`init` refuses one inside it) and runs in scratch worktrees under the goal's directory, never in a drone's. Every hive git call runs with hooks, fsmonitor and diff-shaping configs off (`hive/git.rs`). The inoculation sentence is the drone's first hive context (and run.rs's prompt), with "held-out checks you cannot see" for a goal with a hidden oracle. Known limits: what counts as a test file is still decided by path, so inline tests (Rust `#[cfg(test)]` in `src`) are not guarded and a non-test file under `spec/` is judged as a test; the line-shape rule is a heuristic, which is why hacked weighs as a veto and not above; a `python -c` write escapes the flags (the diff does not); a drone can still read `goal.json`. On the acceptance run (collective.md step 3) it gave no false hacked verdict, after the probe before it had flagged required test additions. *Deferred:* the hack monitor (a paid model reading transcripts), and the promotion and erosion gates (they need L1/L2).

## Open questions

- **Where the learner runs:** on one learner node per collective (simple), or on every node deterministically (no single point of failure, but repeated model spend)?
- **Budget for learning:** reflection and eval cost real money. Spend a fixed share of each goal's budget (say 10%), or adjust it by H5's slope?
- **Privacy:** outcome lines reveal which lessons a drone was shown, and when. Is that acceptable for every role?
