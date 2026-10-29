# Evidence and limits

[← README](../README.md)

Each change to the defaults was tested against a rule written before the paid run; where a default is
on although it missed its rule, the row says so. Most tests used the author's and a few colleagues'
sessions, so their data is not in this repository. The Report column names the published reports in
[reports/](../reports); "internal" means the report stays with the data.

- [Experiments](#experiments)
- [Lesson selection](#lesson-selection)
- [Regret credit](#regret-credit)
- [Known limits](#known-limits)

## Experiments

Intervals are 95% paired-bootstrap confidence intervals unless the row names another.

| Experiment | Result | Report |
|---|---|---|
| Lessons at the turn end, 15 real decisions, sealed sandbox | engineer points 25% → 34% on all 15 cases, +8.8 [−1.4, +22.2], not significant (7 won, 5 tied, 3 lost). Subgroup of the 4 cases that had a directly relevant lesson: 34% → 63%, +29.2 [+4.2, +68.8] | internal |
| Graded turn end vs. the earlier shortlist | relevant lessons reaching the agent 45% → 64% (+0.17 [+0.09, +0.25]); extra rounds with nothing relevant 8 → 3 of 16 | internal |
| What a memory keeps, 10 real sessions | Borg 21 of 21 lessons useful (95% Wilson interval 84.5–100%); Hindsight 169 of 543 items | internal |
| Against Hindsight, same memory model, 15 cases | accuracy tied (no comparison significant). With Sonnet-built lessons, Borg over a plain agent: +8.8 [−1.4, +22.2] on all 15 (not significant), +29.2 [+4.2, +68.8] on the 4 lesson cases | internal |
| Team documents condensed into lessons, 3 cases × 3 runs (n = 9) | +1.7 [+0.8, +2.4] knowledge checks per run over a plain agent; against the hand-written skill +0.11 [−0.56, +0.78]. The pre-registered rule (non-inferior to the skill, lower bound ≥ −0.5) was **not met**. The comparison arms are recorded runs from an earlier experiment, not concurrent | internal |
| System gate, real cross-system session | wrong-system deliveries 83 → 0; no relevant lesson lost | internal |
| Free gate, 1,117 real turn ends | 78% skipped at 0 tokens | internal |
| Vectors | shortlist recall +0.09, +0.14 on German work, ~120 ms | internal |
| Public data, 150 corrections in 15 strangers' repos | no memory helped, Borg included; only ~3% of corrections repeat an earlier one | [swe-chat](../reports/swe-chat.md) |
| End-to-end run in a sandboxed home | all 18 plumbing checks pass; no measurable gain on that task | [dogfood](../reports/dogfood-e2e.md) |
| Live vs hook mode, internal task benchmark, 3 tasks × 3 seeds per mode (8 pairs) | live +0.38 criteria per task [0, +0.88], p = 0.50, 0 redo rounds vs 0.5; directional only (2 pairs differ, both in live's favour); live is the default ([details](delivery.md#why-live-is-the-default)) | internal |
| Decision → fix links, replay of a scripted repo with known answers | fix links 4 of 5 right, 4 of 5 reachable fixes found (subject and revert signals only; tests and prompts are not measured); no real-repo precision yet | [regress](../reports/regress.md) |
| Review → fix findings as lessons, 14 sealed cases × 6 seeds from recurring real findings | later bugs avoided 42% → 48%, +6.3 [−5.1, +17.9] (9 cases won, 1 tied, 4 lost); a lesson handed over directly +7.3 [−4.0, +19.3]; 89% of lessons faithful to their finding, 9% add a claim it does not make. The pre-registered rule (borg beats a plain agent, interval excluding 0) was **not met**; the engineer turned it on anyway, since a user who never configures would otherwise never get it | internal |
| How many lessons a check asks, learned from outcomes by an earlier policy, since retired (cold start: no outcomes yet), same 14 cases × 6 seeds | questions per run 19 → 5, but later bugs avoided −3 points [−9, +2] against the fixed rule and the right lesson asked in 18% of runs instead of 67%: missed its bar. `BORG_STOP_ATTENTION=on` now runs a conformal cut on the model's rank instead | internal |
| That conformal cut on the model's rank (cold start: the dated prior asks ranks 0–10), same 14 cases × 6 seeds | questions per run −5.7 [−9, −3] against the fixed rule, later bugs avoided +1 point [−7, +9], +8 [+1, +15] against no borg; the right lesson asked in 59% of runs instead of 67%. Missed its bar (lower bound −5), so it stays an evaluation switch; 0.6.0's default stays the fixed rule ([below](#lesson-selection)) | internal |
| Hive acceptance (history: the hive was removed in 0.4), two private production codebases, 3 drones, 3 seeds each | every held-out check passed in all 6 runs ($8.48 in total, 7–11 minutes per run), no false hacked verdict | internal |
| Hive pilot probe (history: removed in 0.4), the same two workloads, one run per arm, before the bug fixes listed for the acceptance run | on the cross-repo workload (W2) one session, one session with subagents and best-of-N each solved 5 of 5, the hive 2 of 5 at about 4× one session's cost ($0.80 against $0.19). After the fixes the hive passed W2 5 of 5 in 3 seeds (the acceptance row); no fair comparison ran before its removal | internal |
| Hive learning (history: removed in 0.4), 8 goals × 3 arms (frozen, L0, L0+L1), stopped at 50 of 90 runs | mistakes per goal on positions 5–8 of the goal sequence (order rotated per seed): 0.80, 0.75, 0.60, but the gate read 5 of 12 pairs with a range of [0, 1]: no decision. Its pre-set kill criterion (stop at L0 if this test fails) was not applied: the engineer kept learning on until the hive's removal | internal |

Also in reports/: a test set of engineering corrections drawn from SWE-chat
([swe-chat-engineering](../reports/swe-chat-engineering.md)), the pre-registration of a cross-repo transfer test on it
([swe-chat-engineering-transfer](../reports/swe-chat-engineering-transfer.md)), and the baseline of a plain Sonnet agent
([b0-vanilla-sonnet](../reports/b0-vanilla-sonnet.md)).

## Lesson selection

The fixed rule is the default. The learned selection policy ([Which lessons are
asked](delivery.md#which-lessons-are-asked)) is opt-in (`BORG_SELECT=learned`) until its
pre-registered checks P0–P4 pass: their first run failed P2 and P4, and the one rerun failed P4.
Quality (mistakes avoided) is the objective; there is no cost bar. What holds it:

- **It starts as the fixed rule.** The conservative gate admits a deviation only while the policy's
  replayed quality stays at most 10% of the fixed rule's own below it (at the 5% quantile), so a new
  store decides exactly as the fixed rule does, and a stop's grade 3 is always asked.
- **The measurement was fixed first.** In the author's team store, 73 of 90 grade-3 questions had been
  logged as asked although delivery dropped them (they had been handed over earlier); free lessons past
  the character bound vanished unlogged; the judge saw less evidence for free deliveries than for
  questions; and `asked_not_corrected`, the end state of every lesson nobody judged, counted as
  ignored. 0.6.0 logs the route each lesson actually took, judges every route on the same evidence,
  censors what nobody judged, and adds a placebo arm so not delivering has a measured value.
- **The fixed default measures what the policy needs.** The fixed rule asks and hands over by grade
  alone, so its history cannot say what not delivering is worth or what lessons past the pool would
  yield. Under the default, the placebo (applies the fixed rule does not deliver, judged blindly)
  collects the not-now labels, the tail sample the yield past the pool's budget, each graded record the
  lessons its session already had (`nofree`), and each early delivery its lessons' relative scores
  (`rel`), all at no cost to the fixed rule's quality.
- **Checks written before the policy was built** (rules dated 2026-10-28, amended 2026-10-29; any
  change is a dated amendment below). `borg debug select --replay` runs them on a store's outcome log at
  0 tokens and prints the dates of the rules and of the latest amendment it implements, then per check
  PASS, FAIL, NOT RUN (nothing to check it on yet), NOT TESTED (it ran on data that could not have
  failed it, never a pass: no check that applies a lesson, since a check none applied to and a failed
  call decide nothing under any policy; no deviation from the fixed rule; an arm that is the fixed rule
  everywhere; no label to learn from; every prediction 1/2) or, for P5, the estimate (REPORTED). It
  exits 1 when P0–P4 has a FAIL or a NOT TESTED; P4 NOT RUN, without `--eval`, does not.

| Check | Bar |
|---|---|
| P0 baseline identity | With the gate closed, the policy asks and hands over exactly what the fixed rule does, on every logged graded check: 0 differences |
| P1 determinism | Each check replayed from its logged fit, session, channel and check id gives its logged routes and propensities within 0.01, and folding the log piece by piece equals folding it whole |
| P2 calibration | Each judged lesson predicted from the labels known before its check: the model's log-loss is no worse than the grade alone's (session-bootstrap 95% upper bound of the difference ≤ 0.005); else the model is pruned to grade, route and dilution and run once more, and that verdict is P2's |
| P3 gate conduct | Replayed from a cold start in time order, no decision risks more than the gate's 10% share and the first ones are the fixed rule's; with a budget set, the replayed spend stays within it. Spend is reported either way |
| P4 recall | On the review → fix eval's 14 cases × 6 seeds, the ungated policy, under the prior and under the store's posterior, keeps the right lesson's recall within 5 points of the fixed rule's (case-paired bootstrap lower bound above −5). Questions and spend reported, not gated |
| P5 off-policy estimate | Inverse-propensity and doubly robust estimates on the production log, with their coverage: reported, never decisive |

Every pass decides each check under the lessons its record says its session already had (`nofree`;
none for older records) and through the production functions; where the replay still differs from
what a hook ran (P4 decides at pool tokens 0 with no such lessons, because an evaluation's state files
log neither), `crates/borg/src/select/replay.rs` lists it in its module doc.

**First run** (2026-10-28): the policy as first built, a copy of the author's team store (887 graded
checks) and the P4 baseline run of 2026-10-28 (84 runs, $52.29).

| Check | Result |
|---|---|
| P0 | PASS: 887 of 887 checks |
| P1 | PASS |
| P2 | FAIL: log-loss 0.5252 against the grade alone's 0.5457, mean difference −0.020, upper bound +0.012 > 0.005; the prune-and-rerun was not built yet |
| P3 | PASS: 10 refits, 203 of 887 checks deviated from the fixed rule, 0 over the gate's budget |
| P4 | FAIL: recall against the fixed rule −4.02 points [−7.28, −1.24] under the store's posterior, −7.20 [−13.85, −2.21] under the n = 0 prior |
| P5 | the learned policy, doubly robust: +0.049 [−0.001, +0.097]; non-decisive |

**Amendment 2026-10-29**, written after the first replay of P0–P5, before any rerun:

- The bars above are unchanged. No prior (`ASK_PRIOR`, `NOT_PRIOR`, `DILUTION_PRIOR`) is changed in
  response to P4.
- Defects found while diagnosing the failure are fixed before the one rerun, each on its own argument,
  not on its effect on P4: (a) deferral during the turn credited a later delivery that the later check
  credits again, with an undrawn rate; (c) delivery rates from legacy logs biased by route (a free apply
  of an already delivered lesson counted undelivered, an ask always delivered); (d) type-II ML
  shrinking the sd of terms with a dated prior (dilution) without data.
- P2's pre-registered prune-and-rerun is implemented and run once.
- P0–P5 are rerun once on the fixed tree with the same store copy and the same 84 runs. The P4 cases
  have now been seen twice, so a rerun PASS is weaker evidence than a first-run PASS, and is reported
  as such.
- The default policy is the fixed rule until P0–P4 pass (P2 after its one prune); the learned policy
  is opt-in (`BORG_SELECT=learned`). Under the fixed default the zero-quality-cost placebo (applies the
  fixed rule does not deliver, judged blindly) collects the not-now labels the learned policy cannot
  identify from the fixed rule's history.

**Fixed before the rerun**, each on its own argument:

- (a) Not now is worth 0 during the turn, as at the stop: a lesson left for later earns its value at
  the check that delivers it.
- (c) An apply of a lesson its session already had counts in no delivery rate. A live question's
  chance of reaching the agent is learned and weighs its value; a stop's question blocks, so it stays
  certain.
- (d) The dilution slopes keep their dated prior; automatic relevance learns only the rank, pool and
  interaction terms.
- A label holds the value known at each check, so a veto or a stronger verdict after a check no longer
  reaches that check's prediction (P2, and P5's outcome model).
- The gate's ledger and the monitor value each learned check against the fixed rule it was gated
  against, under the lessons its session had.
- In the pruned rerun, a lesson with no label before its check is predicted by the pruned model's
  prior.
- P5 weighs a placebo's label by its draw's chance (`psi`).
- The cold start folds each replayed decision with its own propensities, and chooses pools under the
  loosest latency limit, as the budget's price replays them.
- A check that tests nothing says NOT TESTED instead of PASS.

**Decisive rerun** (amendment 2026-10-29):

Run once (2026-10-29) on the fixed 0.6.0 tree, after the release checks passed, with the same store
copy (887 graded checks, all under the fixed rule) and the same 84 runs, which it left unchanged. It
missed P4, so **the fixed rule stays the default** and the learned policy stays opt-in. P4's cases
had been seen twice by then (the first run and this one), so even a P4 pass would have been weaker
evidence than a first-run pass.

| Check | Result |
|---|---|
| P0 | PASS: 887 of 887 checks |
| P1 | PASS: 887 of 887 checks replayed, max \|Δ\| 0.0000; the piece-by-piece fold equals the whole one |
| P2 | PASS by the pruned model. The full model missed: log-loss 0.5251 against the grade alone's 0.5457, mean difference −0.021 [−0.036, +0.011], upper bound > 0.005. The pre-registered rerun pruned to grade, route and dilution passed: 0.5377 against 0.5457, mean difference −0.008 [−0.013, +0.001]. 406 judged lessons in 13 sessions |
| P3 | PASS: 10 refits, 101 of 887 checks deviated from the fixed rule, the first check was the fixed rule's, 0 over the gate's budget; no budget set. Spend: 544 questions, 9 blocks at the stop, 3,388 free against the fixed rule's 540, 6 and 3,504 (pool tokens not logged) |
| P4 | FAIL: the fixed rule's recall 76.2%; under the store's posterior 72.7%, −3.53 points [−6.74, −0.98]; under the n = 0 prior 72.4%, −3.77 [−6.89, −1.18]. Both lower bounds are below −5. Questions per check 8.49 under the fixed rule, 7.81 and 7.92 under the policy; $0.102 per check |
| P5 | the learned policy, doubly robust: +0.021 [−0.017, +0.051], coverage 89.0%, ESS 388; non-decisive |

P2 passed only by the pruned model, so the report says so ("PASS by the pruned model"); the learned
policy itself decides with the full model.

**In use**, under `BORG_SELECT=learned`, a monitor estimates the policy's value against the fixed rule
per session and reverts the store to the fixed rule for good if its bound shows harm beyond 10% of the
fixed rule's value. After 300 judged lessons whose route was randomised, or 30 sessions under the
learned policy, a readout compares asking against handing over, and handing over against not
delivering, per grade, with session-bootstrap intervals. The policy stays on unless the monitor fired.

Not built yet: a learned rule for early delivery (it keeps today's; the early records now carry `rel`
for one, though only of delivered lessons, every plain one at or above today's bar, so they can show
whether to raise it, not what a lower bar would admit), and counting deviations of the pool's size and
of early delivery from the fixed rule (the graded record does not log the pool budget a check chose,
so `deviations` counts routes only).

## Regret credit

`BORG_REGRET_CREDIT` (a regret counts against the lessons asked before its decision) has a pre-registered
rule: keep it on if the belief with the regret term predicts later outcomes no worse (upper bound of the
95% bootstrap CI of the log-loss difference ≤ 0.005) and credited lessons are later changed no more often
than a matched control; with fewer than 30 later results among credited lessons the result is
inconclusive and the default stays on.

The latest run (0 tokens, read-only, on the author's team store: 135 episodes, 629 outcomes) was
**inconclusive**: the store holds no regret episode and no decision → fix link, so there were 0 later
results to measure. The default stays on for that reason only. The store cannot produce regrets while
`BORG_REGRESS` is `links`, since only `learn` makes regret episodes, and this rule does not decide
`BORG_REGRESS` at all. Its own bar is a replay of a few hundred real commits with `--explain`, counting the
wrong fix links; that replay has not run, so `BORG_REGRESS` stays `links`.

## Known limits

- Most measurements come from the author's and a few colleagues' sessions, not from a broad user base.
- Small samples (16 negative and 51 positive turn-end cases; 10 sessions for memory quality). The
  intervals are wide.
- The weights of a veto (3), a regret (at most half a correction) and green tests (0.5) are design
  choices. `borg debug ope` can replay them offline, but no logged decision has the support
  for it yet.
- Turn-end asking of the engineer (`BORG_QUEEN=on`) failed its test and is off.
- omp, opencode and Codex get no early delivery of ordinary lessons yet; standing lessons (ratified
  constraints) do arrive early there.
- omp gives a stop 30 s, which no setting changes. borg's stop budget there is 28 s, so the turn-end call stops
  after at most 18 s (less what the stop spent before it); about one in eight measured turn-end calls
  took longer, and such a turn ends without questions. A stop that a live check covered makes no call.
- On omp, opencode and Codex a live check sees the turn's tool calls and edits, not the agent's prose.
- Codex skips an untrusted hook without a word, and fires no PostToolUse for a failed `apply_patch`, a
  patch run through a shell heredoc or a command still running when it yields: borg never sees those
  edits. In code mode borg's context reaches the model only after the whole `exec` script, and codex cuts
  a hook's context past 10,000 characters, the budget borg sizes messages by
  ([codex](../adapters/codex/README.md#what-borg-misses)).
- opencode 1.x runs no hook after a failed tool call, so a live check's questions wait for the next
  successful call, the next prompt or the stop. Its tool-call context is stored in the tool output: the
  TUI hides it for built-in tools, but session exports show it.
- Review → fix findings come from Claude Code workflows and omp; opencode, Codex and Claude Code's Agent
  tool yield them only when their reviewers answer in JSON, as they have no structured output. The Codex
  reader is not yet checked against a real spawned rollout.
- Green-test credit sees only test runs whose result the harness shows; a runner whose output the
  adapter does not send counts as no run.
- Decision → fix links have been measured only on a scripted repo, not on real notes, and only for the
  subject and revert signals: check `borg regress --replay` on your own history and
  `borg regress --session ID --explain` on a few real sessions before `BORG_REGRESS=learn`.
