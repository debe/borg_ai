# Live vs hook mode (pre-registered 2026-10-14, before the run)

Borg's conscience has two delivery modes (README "Delivery modes", crates/borg/src/live.rs). **Hook** mode,
the default, runs one graded model call at the turn end, and only a grade 3 blocks the stop for a redo round.
**Live** mode runs the same call during the turn as well: after a state-changing tool call, a detached
`borg check` judges the turn so far, and its grade-3 lessons arrive with the next tool call. This report
decides whether live becomes the default. The text above the results line is fixed before the run and not
edited after it.

## Question

Does checking during the turn help the agent's work more than checking at its end, without costing it more
redo rounds?

## Method

The workbench eval E3 (reports/e3-workbench.md, eval/workbench/), as in Phase R (reports/phase-r.md):

- **Why this eval.** Its solvers really edit, its grading is outcome-based (the workbench's own criteria,
  paired over cases), and it already counts early deliveries and stop blocks. SWE-chat replay cannot time a
  delivery: it has one checkpoint per case.
- **Arms.** `borg` (hook mode) vs `borg_live` (the same hook env plus `BORG_MODE=live`). Same cases, lessons,
  solver model (claude-opus-5) and seeds. No bare arm: hook vs bare already has a baseline (Phase R
  +1.67 [+0.78, +2.44]). Fixed shipped settings, no sweep.
- **Hook env, both arms.** The shipped Stop path, not word matching: `BORG_MATCH=bm25` is dropped, the
  consequence model is pinned to `BORG_PREDICT_MODEL=claude:claude-sonnet-5-5`, and `~/.local/bin` goes first
  on the hook's PATH (the runner's clean PATH has no `claude`; without it the hook fails open).
  `BORG_LEARN=off BORG_QUEEN=off` as before.
- **Cases.** The knowledge catalog, development split: k1 (service drain rule, 5 criteria), k2 (non-service
  guard, a regression case, 4), k3 (replay experiment, 7).
- **Size.** A smoke run first (k3, 1 seed, both arms, about $8), then 3 cases × 3 seeds × 2 arms = 18 runs,
  about $75. This is **directional**: the report states the CI and claims no significance when it spans 0.
- **Commands.** `ITER=live-vs-hook-claude-opus-5 UTIL_STOP=0.85 eval/workbench/run.sh probe` (the leak probe,
  now with borg_live; it passes when each borg arm's hooks logged a model check or a delivery), then
  `... run.sh live-smoke` (probe-stamp check, k3, arms `borg,borg_live`, 1 seed, ITER `<ITER>-smoke`), then
  `... run.sh live` (probe-stamp check, k1–k3, arms `borg,borg_live`, `--require-injection`, SEEDS 3), which
  grades and prints the report and the rule. Frozen binaries via `BORG=... BE=...`.
- **Collection.** Each step waits up to 60 s for a running live check (`*.live.lock`) before its state is
  copied. Counts read `*.ctx` only (the hooks write the same markers to `.edits`).

## Pre-registered decision rule

Live becomes the default when **both** hold:

1. the paired difference live − hook in workbench criteria passed has a mean > 0 and a 95% CI lower bound
   ≥ −0.5 (Phase R's non-inferiority bar);
2. live causes no more stop blocks (redo rounds) per run than hook: mean `stop_blocks`(borg_live) ≤
   mean `stop_blocks`(borg), over the same paired knowledge runs as rule 1 (other catalogs in the iteration
   and pairs with an invalid run are left out).

A live check's grade 3 that no tool call delivered is asked by the stop (a block, counted in `stop_blocks`),
so live mode cannot meet rule 2 by dropping asks. Only a check that finishes after the stop (it waited 15 s)
can leave one undelivered; `live_undelivered` counts those, and the report states them.

Then the default flips to `live` in the config default, the README and the CHANGELOG; `"mode": "hook"` or
`BORG_MODE=hook` switches back at any time. Otherwise the default stays hook and live remains opt-in.
`e3.py report` prints which rule fired as `rule: live becomes default | stays hook`.

## Reported per run and arm

| Field | Meaning |
|---|---|
| `passed`, `knowledge_passed` | Workbench criteria passed (all, and the knowledge criteria) |
| `borg_live` | Lessons a live check delivered (`#borg-live` lines) |
| `borg_queued` | Grade-2/1 lessons handed over free (`#borg-queued`) |
| `live_undelivered` | Grade-3 asks of a live check still waiting in `*.queued` (`live <ids>`) when the run ended |
| `stop_blocks` | Redo rounds: non-empty `#borg-asked` lines not followed by `#borg-live` |
| `changed`, `ignored`, `pending` | The judge's last verdict per asked lesson (`#borg-judged`) |
| `lead_s` | Seconds from the last edit tool call before each live delivery to the delivery |
| `borg_usd` | The hooks' model cost (`#borg-predict-cost`, turn-end and live) |
| `usd_total` | Solver + grader + hooks |
| `wall_s` | Wall time of the run |

Runs took 433–711 s against a 900 s timeout in Phase R; live mode's added time is watched against it.

---

## Results

Not run yet.
