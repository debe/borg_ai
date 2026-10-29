# Roadmap: 0.7

What 0.6.0 left open, each with what would show it done. Selection changes follow the rule of
0.6.0: checks written and dated before the change is built ([Evidence](evidence.md#lesson-selection)),
quality the objective, no cost cap.

## Lesson selection

- **Early delivery learns its bar.** It keeps today's rule (at most 5 lessons, at least half the best
  match's score). Early records log each lesson's relative score (`rel`), but only for lessons
  delivered, so they can show whether to raise the bar, never what a lower one would admit. Needed: a
  blind-judged sample of lessons just below the bar, as the placebo does for graded applies, then a
  learned bar under the gate. Done when a pre-registered check compares the learned bar with today's
  on those labels.
- **Act on the question-count test.** The randomised test runs by default on live checks. Its readout
  is due after 300 randomised checks or 30 sessions: each question's chance of changing the work by
  the number asked, and asking against handing over a grade 2. Done when the readout is reported in
  [Evidence](evidence.md#lesson-selection) and the evidence rule deviates on it, or the readout shows no
  effect. The readout reads only the top tested lesson today; pooling lower positions would add power.
- **Withholding a lesson, measured wider or ruled out.** Not delivering is judged blindly only for
  lessons past the fixed rule's 10-lesson cap, so the policy can never learn to withhold anything
  else, and it withholds nothing. Either a pre-registered, risk-rationed test of withholding low-grade
  lessons, or a dated decision that withholding stays off.
- **One model for calibration and decisions.** P2 passed only with the pruned model (grade, route,
  dilution), while decisions use the full one. Either decide with the pruned model, or rerun P2 once
  the store has enough sessions for the full model to pass on its own.
- **Mistakes, not only recall.** P4 measures whether the right lesson reaches the agent. Whether the
  learned default avoids more mistakes is open until the online readout, or a paid review → fix run
  of the learned default against the fixed rule scored on mistakes (about $55 per arm, run only when
  asked).
- **The pool grows on evidence.** The tail sample and yields by fill position are logged; the pool stays
  at its 12,000-token baseline until they back a larger one. Done when `borg status` shows a learned
  pool size, or the yields show the tail is not worth it.
- **Smaller items:** a live ask queued during the stop's model call can be asked twice and then dropped
  as done, which counts as a miss; the evidence rule's hand-overs on a live check with a 3 are never
  asked by a stop that comes first (the test's are); the replay's cold start grows about quadratically
  with sessions.

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
