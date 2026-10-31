# Roadmap

What 0.7.3 leaves open, each with what would show it done. Selection changes follow the rule of
0.6.0: checks written and dated before the change is built ([Evidence](evidence.md#lesson-selection)),
quality the objective, no cost cap. The numbers below were measured on 2026-10-31 at 0 tokens, on
copies of the author's stores (`borg status`, `borg debug select --replay`, `borg debug ope` and the
outcome logs).

## Lesson selection: the path to fully adaptive

**Done means:**

- Every number that shapes which lessons are delivered, how, and how they are learned is either learned
  from outcomes under the gate, with its readout in [Evidence](evidence.md#lesson-selection), or fixed
  by a dated decision with its evidence in [Numeric constants](design/constants.md).
- The gate is open in the main stores (a team store and an open-source store), and the monitor has a
  harm bound.

**Today:** the learned policy is the default. Its gate reads 0.023 in the open-source store and is
spent in the team store. No learnable number has a readout yet; the question-count test's is due
first.

The steps are ordered by what unblocks the rest.

### 1. Measurement that can be trusted

Every later readout reads these labels, this monitor and this status line.

**Done in 0.7.3** ([Evidence](evidence.md#lesson-selection)):

- A verdict goes to the delivery that earned it: a lesson handed over free, then asked, is judged on the
  question once answered (12 of the team store's 36 free "changed" verdicts had gone to the hand-over,
  and 15 questions in ended sessions were stuck open).
- The judge's cost line names the ids each call returned and those that matched no pending lesson.
- omp's calls ask a Sonnet-class model unless one is pinned (8 of 13 omp stop checks had failed at
  the deadline).
- The monitor's ESS and largest weight count only labels that can move its estimate, and a route's
  residual enters from 5 such labels (team store: ESS 9 of 127 at 42.3× became 24 of 29 at 2.4×).
- A session's settle closes every delivered lesson it left without a verdict, from sessions settled by
  0.7.3 on (the team store's copy had 307 delivered applies open in ended sessions).
- The graded record logs the pool budget its check filled to (`budget`).
- `borg status` says "gate spent" instead of "cold start" when the allowance is used up.
- The replay counts records copied from another store's log instead of failing P1 (open-source store:
  PASS, 50 imported), and `borg assimilate` no longer duplicates an episode a turn end captured.

**Still open:**

- **Fresh copies confirm the verdict fix.** Done when copies taken after new sessions show no free
  verdict after a live ask and no delivered lesson left open in an ended session. The 15 stuck questions
  settled before 0.7.3 stay open in the team store's summary, so the replay still reproduces the
  decisions made on it; closing them needs a dated re-settle.
- **Missing verdicts are explained.** The judge returns no verdict for 29–44% of the lessons it is sent
  (team store: 2,370 sent, 39% nothing). Decide on a schema or instruction change after 100 calls with
  the new `returned=` and `unmatched=` fields. Done when the share with no verdict is under 10%, or its
  cause is documented.
- **omp's stop answers in time.** The faster model is an expectation, not a measurement. Kill: revert
  it if more than 20% of the next 20 omp stops fail, or P2 on omp's labels worsens by more than 0.01
  log-loss. Status counts a failed call at its deadline, so its p95 ("omp 24.8 s of 28.0 s") is the
  deadline, not the tail: show "p95 ≥ deadline" when failures reach the top 5%. The model calls' log
  does not name the model that answered.
- **The monitor's harm bound.** It needs 10 learned sessions: the team store has 7, the open-source
  store 4.
- **Each store's log describes its own work.** 726 of the team store's 1,104 graded checks are this
  project's history from before the stores were split; a dated note decides whether it stays. A third
  store holds 698 of its 699 checks copied from the open-source store; pooled readouts leave it out by
  the files they are given, since its copies cannot be told apart by their offsets.

### 2. The question-count test's readout

- **Amended 2026-10-31:** due at 300 checks in at least 15 sessions with a draw, or 30 sessions; pooled
  over the two main stores, stratified by collective; intent-to-treat, with the share played per drawn
  count (`borg debug select --replay --file A --file B`, records copied from another store's log left
  out). Today: 243 checks in 7 sessions, due about 2026-11-05.
- **Done** when the readout is in [Evidence](evidence.md#lesson-selection) and the evidence rule deviates
  on it, or it shows no effect. The readout reads only the top tested lesson; pooling lower positions
  would add power.

### 3. Decisions the data already answers

**Dated 2026-10-31** in [Evidence](evidence.md#lesson-selection) and
[Numeric constants](design/constants.md): the gate's allowance keeps scaling with the fixed rule's
modelled value at `risk` 0.1; the pool stays 12,000 tokens, and grows only on a gain above 0 with
latency measured near the larger size (0.7.3); withholding is not learned and `NOT_PRIOR` is fixed;
the stop's prefilter stays at 8.0.

- **The gate reopens.** Played as the fixed rule, the team store reopens after about 60 learned checks,
  then hovers near 0. Done when `borg status` shows a positive gate in both main stores.
- **The pool's size.** The graded record logs the budget a check chose (`budget`, 0.7.3); the
  selection summary does not count a pool deviation yet. Revisit the pool with 30 judged applies past the budget,
  about 2026-11-07. Kill for any pool exploration: revert if the stop's p95 at the larger budget exceeds
  the harness's stop budget, or more than 2% of stop calls fail, over 20 calls.
- **Withholding reopens** when the placebo's unclear rate falls under 50% (step 5) or 100 judged
  placebos exist. Kill: drop the placebo arm if the next 60 placebos give fewer than 15 changed or
  ignored verdicts.
- **One model for calibration and decisions.** P2 passed only with the pruned model (grade, route,
  dilution), while decisions use the full one. Either decide with the pruned model, or rerun P2 once
  the store has enough sessions for the full model to pass on its own.

### 4. Learning weights

`borg debug ope` finds 0 usable decisions in every store: the weights reach a logged propensity only
through the Thompson slot draw over a pool, which the default graded check never takes (ope.rs module
doc). `VETOED_W` and `CORRECTED_W` are fixed by dated decision (0 vetoes, 1–2 corrections after an
ask).

- **`REGRET` and `TESTED_W` learn through the tail sample.** All regret credit comes from one session
  in the team store and two in the open-source store. The weights decide which lessons fill the pool
  when its budget forces a choice (173 of 195 pool checks in the team store), and the tail sample
  randomises exactly that choice, with its chance logged. Build: an off-policy mode over pool
  membership, each candidate weight (half, double) re-ranking the pool, scored on applies and judged
  changes, with a session bootstrap. Kill: date both weights fixed if after 10 more sessions any
  candidate rests on fewer than 30 tail-sampled lessons, or its ESS is under 30.

### 5. Learning quality: judging with the session's own evidence

- **Free hand-overs and placebos are judged after the work** (0.7.3): a live check waits until work
  follows the delivery; a stop's judge call judges the rest. Before, 54% of free verdicts came within 2
  minutes of delivery, 74% of those unclear. Kill: revert if, over the next 150 free deliveries, the
  labelled share (over every free delivery, those settled `no_call` included) does not rise from 38% to at least 50%, or the unclear share does not fall by at least 15
  points.
- **Placebos from the lessons the budget cuts.** A placebo is never a lesson the session already had,
  and 414 of 419 not-now applies in the team store (1,219 of 1,224 in the open-source store) are such
  lessons. Meanwhile the character budget cut 469 free hand-overs in the open-source store (29 in the
  team store), unjudged: a "not now" nobody measures. Build: draw placebos at random among the cut
  lessons, with their chance logged. Kill: revert if placebos per check do not rise from about 1.2% to
  10% over 300 checks.
- **omp's labels catch up.** Labelled share of omp's questions: 27% against Claude's 39%; omp's free
  hand-overs end unclear 71% of the time, Claude's 49%. 18 omp questions saw no judge call after them.
  Re-measure over 10 omp sessions; if more than 10% of omp's questions still see no judge call, check
  whether omp sessions end before it runs.
- **Mistakes, not only recall.** P4 measures whether the right lesson reaches the agent. Whether the
  learned default avoids more mistakes is open until the online readout, or a paid review → fix run of
  the learned default against the fixed rule scored on mistakes (about $55 per arm, run only when
  asked).

### 6. Hand-set delivery cutoffs

- **Early delivery learns its bar.** It keeps today's rule (`RELATIVE` 0.5 of the best match's score,
  `EARLY_CAP` 5 lessons). Only 7 early lessons carry their relative score (`rel`); a bar of 0.8 would
  have dropped 5 of them, among them the only one judged "changed". Lessons below the bar are never
  logged, so a lower bar cannot be judged. Build: a blind-judged sample of 1–2 lessons just below the
  bar (`rel` 0.3 to 0.5) per early delivery, with its chance logged, then a learned bar under the gate.
  Kill: stop if after 30 sessions there are fewer than 30 judged below-bar labels, or the judge calls
  more than half of them unclear; until then a dated note keeps 0.5.
- **The other baselines.** The stop block's 10 lessons (`CAP`), the placebo's and tail's 2 slots, and
  the judge's 2 tries stay baselines until steps 1–5 hold; each then gets a test or a dated decision.

### Smaller items

- A live ask queued during the stop's model call can be asked twice and then dropped as done, which
  counts as a miss.
- The evidence rule's hand-overs on a live check with a 3 are never asked by a stop that comes first
  (the test's are).
- The replay's cold start grows about quadratically with sessions.

## Messages and the mesh

- **The two-node simulation covers more of the sync.** `crates/borg-mesh/tests/two_nodes_cli.rs` runs
  a Claude Code and an opencode session across two nodes through the real merge, without a network. Not
  yet covered: the membership exchange and quarantine, the `borg/mesh/13` (0.5) path, the gate before
  lines leave, the frame budget's "the rest goes next session", the daemon's dial and retry loop, and
  clock skew between nodes.
- **A remote pair's first messages.** Until a remote node acknowledges one of a pair's messages, the pair
  is paced at 0.5.0's rate (one per 10 minutes); afterwards the window. Pair-local and from synced data
  by design; worth revisiting once 0.5 nodes are gone.
- **A forged line stamp.** `now - ms` in the message log's expiry overflows in a debug build for a
  forged stamp near `i64::MIN` (release wraps and keeps the line). Changing it changes which lines every
  node keeps, so it needs a protocol decision.

## Context borg reads

- borg judges a session by its prompt, its tool calls, its edits and the agent's last message. It never
  reads an omp session title or a separate plan file. If intent often lives only there, reading them is
  a small adapter change.

## Tooling

- borg-eval's runners still bound each paid drone run with `--max-budget-usd`. It is a guard on paid
  evaluation runs, not on the product; keep it or drop it by decision.
- borg web's Conscience view has a branch for a fixed default that no longer occurs; remove it at the
  next rebuild of the page.
