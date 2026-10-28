# Evidence and limits

[← README](../README.md)

Each change to the defaults was tested against a rule written before the paid run; where a default is
on although it missed its rule, the row says so. Most tests used the author's and a few colleagues'
sessions, so their data is not in this repository. The Report column names the published reports in
[reports/](../reports); "internal" means the report stays with the data.

- [Experiments](#experiments)
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
| That conformal cut on the model's rank (cold start: the dated prior asks ranks 0–10), same 14 cases × 6 seeds | questions per run −5.7 [−9, −3] against the fixed rule, later bugs avoided +1 point [−7, +9], +8 [+1, +15] against no borg; the right lesson asked in 59% of runs instead of 67%. Missed its bar (lower bound −5), so it stays opt-in | internal |
| Hive acceptance (history: the hive was removed in 0.4), two private production codebases, 3 drones, 3 seeds each | every held-out check passed in all 6 runs ($8.48 in total, 7–11 minutes per run), no false hacked verdict | internal |
| Hive pilot probe (history: removed in 0.4), the same two workloads, one run per arm, before the bug fixes listed for the acceptance run | on the cross-repo workload (W2) one session, one session with subagents and best-of-N each solved 5 of 5, the hive 2 of 5 at about 4× one session's cost ($0.80 against $0.19). After the fixes the hive passed W2 5 of 5 in 3 seeds (the acceptance row); no fair comparison ran before its removal | internal |
| Hive learning (history: removed in 0.4), 8 goals × 3 arms (frozen, L0, L0+L1), stopped at 50 of 90 runs | mistakes per goal on positions 5–8 of the goal sequence (order rotated per seed): 0.80, 0.75, 0.60, but the gate read 5 of 12 pairs with a range of [0, 1]: no decision. Its pre-set kill criterion (stop at L0 if this test fails) was not applied: the engineer kept learning on until the hive's removal | internal |

Also in reports/: a test set of engineering corrections drawn from SWE-chat
([swe-chat-engineering](../reports/swe-chat-engineering.md)), the pre-registration of a cross-repo transfer test on it
([swe-chat-engineering-transfer](../reports/swe-chat-engineering-transfer.md)), and the baseline of a plain Sonnet agent
([b0-vanilla-sonnet](../reports/b0-vanilla-sonnet.md)).

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
- omp gives a stop 30 s, which no setting changes. borg budgets 28 s, so the turn-end call there stops
  after at most 18 s (less what the stop spent before it); about one in eight measured turn-end calls
  took longer, and such a turn ends without questions. A stop that a live check covered makes no call.
- On omp, opencode and Codex a live check sees the turn's tool calls and edits, not the agent's prose.
- Codex skips an untrusted hook without a word, and fires no PostToolUse for a failed `apply_patch`, a
  patch run through a shell heredoc or a command still running when it yields: borg never sees those
  edits. In code mode borg's context reaches the model only after the whole `exec` script, and codex cuts
  context over about 2,500 tokens ([codex](../adapters/codex/README.md#what-borg-misses)).
- opencode 1.x runs no hook after a failed tool call, so a live check's questions wait for the next
  successful call, the next prompt or the stop. Its tool-call context is stored in the tool output: the
  TUI hides it for built-in tools, but session exports show it.
- Review → fix findings come from Claude Code workflows and omp; opencode, Codex and Claude Code's Agent
  tool yield them only when their reviewers answer in JSON, as they have no structured output. The Codex
  reader is not yet checked against a real spawned rollout.
- Green-test credit sees only test runs whose result the harness shows; a runner whose output the
  adapter does not send counts as no run.
- A plain session's playbook reflection takes its first task prompt as the task and runs only at
  session end, never while the session is idle.
- Decision → fix links have been measured only on a scripted repo, not on real notes, and only for the
  subject and revert signals: check `borg regress --replay` on your own history and
  `borg regress --session ID --explain` on a few real sessions before `BORG_REGRESS=learn`.
