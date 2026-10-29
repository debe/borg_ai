# Delivery and modes

[← README](../README.md)

Borg spends tokens only where a lesson is likely to matter.

- [When a lesson arrives](#when-a-lesson-arrives)
- [Which lessons are asked](#which-lessons-are-asked)
- [Delivery modes](#delivery-modes)
- [Why live is the default](#why-live-is-the-default)

## When a lesson arrives

![One turn: free early delivery, a free word gate at the turn end, one graded model call, an extra round only for a grade-3 lesson](img/turn.svg)

1. **Early, free.** When a prompt or touched file first names a lesson's topic, Borg adds that topic's
   top lessons to the context, by word matching only, on Claude Code only so far: omp, opencode and Codex
   could take it, but it waits for an evaluation there. Standing lessons
   (ratified constraints, see [Saying no](learning.md#saying-no)) are no ranking guess: they arrive early
   on every harness that takes the context, once their scope is named, an unscoped one at the first
   prompt. `BORG_EARLY=off` turns early delivery off.
2. **Turn end, graded.** A free word gate (`BORG_PREFILTER`) skips most turns. Otherwise one model call
   grades every eligible lesson: 3 = certain, 2 = likely, 1 = worth a thought. The selection policy then
   gives each graded lesson a route: asked, handed over free with the next prompt or touch, or left for
   later. A stop that asks anything blocks, and the agent works one more round. The default, the fixed
   rule, decides as 0.5.0 did: a grade 3 blocks, and grades 2 and 1 alone arrive free ([Which lessons
   are asked](#which-lessons-are-asked)). In live mode, the default, the same call also runs during the
   turn, after the agent changes something.
3. **Judged by meaning.** The same call checks whether what was handed over earlier changed the agent's
   work: questions it asked, and free deliveries (early and queued), each recorded with its channel, the
   check that routed it and its probability of that route. Every route gets the same evidence (the work
   before the delivery and after it); a random half of the questions are judged without the agent's own
   `applies`/`skip` line, so its effect on the judge is measured. Questions keep their judge slots first
   (at most 10 wait); free deliveries alone never pay for a call. A live check judges free deliveries at
   once and leaves a question the agent has not answered yet to the stop, which sees the answer. A correction merged into a
   lesson delivered free counts against it. At session end `borg learn` settles the rest: a lesson with
   no verdict is `asked_not_corrected`, with the reason it had none, and counts as unknown, never as
   ignored. See [Mode and channel](statistics.md#what-is-counted).
4. **System gate.** When the repo maps to a known system, lessons scoped to other systems stay out, even
   if the conversation names them ([System map](configuration.md#system-map)).

## Which lessons are asked

A graded check gives each lesson that applies one route: **asked** (in a block at the stop, as a
question with the next tool call during the turn), **free** (context with the next prompt or touch) or
**not now** (it stays eligible for a later check). The selection policy chooses the routes. Its
objective is quality, the mistakes the lessons help avoid; there is no cost cap.

**The fixed rule** is the default, and the reference everything else is measured against. With a
grade 3, ask every 3 and 2, and the 1s while the block has fewer than 10 lessons. Without one, hand
over the 2s and then the 1s, at most 10. A lesson the session already had delivered may still be
asked, but never takes a free slot, since delivery would drop it as given before: the next 2 or 1
takes the slot. For a session that had none of them, this is 0.5.0's rule.

**Measured under the fixed rule.** The fixed rule's history asks and hands over by grade alone, so a
learned policy cannot tell from it what not delivering is worth, or what lessons past the pool would
yield. The checks therefore also collect, at no cost to the fixed rule's quality:

- **Placebo.** Of the applies a check delivers neither way (under the fixed rule, those past its 10),
  up to two are judged blindly at the point a free delivery would be, though the agent never sees
  them; the evaluation switches draw none. A lesson the session already had is never drawn. Each
  candidate's chance of the draw, min(1, 2 / candidates), is logged (`psi`). Their "changed" rate is
  the judge's floor: what it calls a change that would have happened anyway. A route's value is its
  lift over not delivering. Placebo results never reach the beliefs, trust, statistics, the team file
  or the offline estimates.
- **Tail sample.** When the pool's budget leaves lessons out (below), up to two of them, drawn
  uniformly, are graded after the pool under every policy, so the yield of the positions past the
  budget is measured.
- **What the session had.** Each graded record names the applies the session already had, or had
  waiting in its queue from another check (`nofree`). The delivery rates leave them out, since a drop
  as given before or queued twice says nothing about whether a lesson the session lacks arrives, and a
  replay decides each check under the same lessons.
- **Early relevance.** An early delivery records each lesson's score as a share of the best on-topic
  match (`rel`), for a learned early rule. Early delivery itself keeps its rule: lessons at half the
  best match, at most 5.

`borg status`, `borg top` and the Conscience page of `borg web` count the placebos drawn and judged and
the tail lessons shown and applied.

**The learned policy** is opt-in (`BORG_SELECT=learned`) until its pre-registered checks pass (see
*Why the fixed rule is the default*, below). It fits one model of what each route is worth from the
logged outcomes: the chance that a lesson changes the work and avoids its consequence, by grade, rank,
channel, a 3 in the check, route and how many lessons share the route (more questions, or more free
context, make each one less likely to land), with an effect per session, per lesson and per judge
version. A free lesson's value is weighed by its learned chance of
arriving, and so is a live question's (a stop's question blocks, so it always arrives); a lesson left
for later earns its value at the check that delivers it, not here. Each check takes the routes that
avoid the most mistakes under one draw of that model, seeded by the check, so a replay gives the same
routes; a lesson the session already had is never handed over free. A conservative gate keeps the
fixed rule's routes unless the policy has room: a deviation is admitted only while the replayed
quality of the policy's checks stays at most 10% of the fixed rule's own replayed value below the
fixed rule, at the 5% quantile. A new store has no room, so it starts exactly as the fixed rule, and
exploration is rationed by quality, never by a fixed rate. A grade 3 at the stop is always asked, and
a check with more than 16 lessons that apply takes the fixed rule. It draws its placebos as the fixed
rule does, among the applies it leaves for later.

The model is refitted at the end of `borg learn`, which every session end starts, never in a hook. A
hook reads a summary of the outcome log cached beside the lessons (`select-summary.json`, local, never
synced) and folds in only the lines added since. A store's learned checks decide one at a time: a
learned check reloads the summary, decides and writes its record under
`<store>/select-summary.json.lock`, never during its model call, so two checks deciding at once never
both spend the gate's room; the fixed rule and the evaluation switches read no gate and take no lock.
A learned check kept waiting more than 1 s, or whose record cannot reach the log its gate is folded
from (learning off, `BORG_CONSCIENCE` at another store's lessons, a failed append), takes the fixed
rule, and a record it writes says why (`reason`: `lock_busy`, `lock_failed` when the lock cannot be
taken, or `unrecorded`); `borg status`, `borg top` and the Conscience page of `borg web` count those
records per reason, so a guard that binds often shows. A lesson still waiting in the queue to be asked
or handed over is decided already: a later check, the stop included, leaves it out of its pool, and
one that graded it meanwhile never hands it over free.

**Monitor.** Under the learned policy, per session, its value against the fixed rule is estimated
(doubly robust, cross-fitted by session) under a confidence sequence over the sessions. If its bound
shows harm beyond 10% of the fixed rule's value, after at least 10 sessions, the store reverts to the
fixed rule for good: `borg status`, `borg top` and borg web say `REVERTED`, and the mark
(`<store>/select-reverted`) stays until the engineer deletes it. Deleting it lets the policy learn
again, until the monitor trips again.

**The pool.** The model call reads up to 12,000 tokens of lessons (`BORG_STOP_POOL_TOKENS` sets it for
every check). When that budget leaves lessons out, the tail sample adds up to two of them after the
pool, whatever the policy; the session's `.cost` file then reads `tail=<n>` on the pool line. Under
the fixed rule the budget stays 12,000 tokens. Under the learned policy, once its gate is open or an
engineer set a budget, the pool may shrink where the yield of its last fill positions buys nothing,
and it grows past 12,000 only with the tail sample's yield of the positions it would add and a latency
that fits the harness's stop budget (at least 20 calls measured).

**Cost.** Spend is measured and shown, never capped: calls, extra rounds and dollars per session for
the stop and live checks, in `borg status`, `borg top`, `borg debug select` and the Conscience page of
`borg web`. Only an engineer who sets a `budget` for a collective
([Collectives](configuration.md#collectives)) puts a price on it: the pool tokens its checks may read
per session on average. The learned policy then weighs a larger pool against the smallest price that
keeps the replayed spend within the budget, from the first fit that knows the budget, gate open or
not. The fixed rule reads no budget.

**Why the fixed rule is the default.** The learned policy's checks were written before it was built
(P0–P5, `borg debug select --replay`, 0 tokens): that a closed gate decides exactly as the fixed rule,
that a replay reproduces every decision, that the model predicts no worse than the grade alone, that
the gate never risks more than its share, and that recall of the right lesson holds on an evaluation's
runs. Their first run failed two. The model predicted better than the grade alone on average, but the
upper bound of the difference was +0.012 against the bar's 0.005 (P2). On the review → fix evaluation
the right lesson's recall under the store's posterior was −4.02 points [−7.28, −1.24] against the fixed
rule, a lower bound past the bar's −5 (P4). The one rerun the amendment to the rules allows, on the
fixed tree, passed P2 only by the pruned model and failed P4 again: −3.53 points [−6.74, −0.98] under
the store's posterior, −3.77 [−6.89, −1.18] under the n = 0 prior. The fixed rule stays the default
([Evidence](evidence.md#lesson-selection)). Under `BORG_SELECT=learned` the monitor guards it, and
after 300 judged lessons whose route was randomised, or 30 sessions, a readout compares asking,
handing over and not delivering per grade.

**Switches.** `BORG_SELECT=learned` opts in to the learned policy, `BORG_SELECT=fixed` keeps the fixed
rule whatever else is set. `BORG_STOP_ASK_CAP` and `BORG_STOP_ATTENTION=on` are evaluation arms.
`BORG_SELECT=fixed` wins over the cap, the cap over attention, and both over `BORG_SELECT=learned`
([Environment variables](configuration.md#environment-variables)).

## Delivery modes

Each collective delivers in one of two modes. Both use the same lessons, the same graded model call and
the same questions. They differ in *when* the call runs.

- **live** (the default): the call also runs *during* the turn, so a lesson can land before a mistake
  spreads across ten edits. It reuses the hook machinery: no daemon, no extra process between turns.
- **hook**: the call runs at the turn end only. A lesson that applies mostly arrives after the work is
  done, and a grade 3 costs the agent a redo round. Borg's own side costs about an eighth of live.

| | hook | live |
|---|---|---|
| Prompt | Early delivery and waiting free lessons (grades 2 and 1 under the fixed rule) | The same, plus questions from a live check that finished since |
| Tool call | Early delivery, free | The same. After a state-changing call (an edit, or a shell command that is not read-only), a check of the turn so far starts in the background; a later tool call starts one while changes wait unchecked. A live check has no word gate: a turn half done has too few words for it. The lessons it asks (under the fixed rule: with a grade 3, every 3 and 2 and the 1s up to 10) arrive with the next tool call as "borg check of your work so far"; the agent answers them as it works. Its free lessons arrive as at the turn end |
| Turn end | The free gate, then one graded call; a stop that asks blocks for one more round | The same backstop. When a live check that started this turn covered every change and nothing changed since, the stop skips its model call. A live check's question that no tool call delivered yet blocks the stop, as in hook mode. Lessons asked live are not asked again |
| Model calls | At most one per turn end; the gate skips most | One per burst of changes (at most one running at a time, at most one start per 45 s), plus the turn end when changes came after the last check |
| Added latency | None mid-turn; ~10 s at a checked turn end | None: the hook starts the check and returns at once. A stop waits at most 15 s (omp: 7 s) for a check still running |
| Harnesses | Claude Code, omp, opencode 1.x/2.x, Codex | Claude Code, omp (≥18.4.4), opencode 1.x/2.x, Codex |

**Switch back to hook** per collective with `"mode": "hook"` in its entry of `collectives.json`, or for
one shell with `BORG_MODE=hook` (or `live`), which overrides the config. `BORG_LIVE_DEBOUNCE_S` sets the
least time between two checks (default 45). An unknown value means the default, live, and `borg doctor`
warns. Switching back is the same edit; nothing is migrated.

**See what it does:** `borg status` and `borg doctor` print each collective's mode and where it came
from, and `borg status` counts live checks apart from turn-end checks. In `borg web`, the header and the
drone cards show the mode, the Conscience page splits live checks from turn-end checks and what the
questions led to by channel, and the Cost page shows check cost by mode per day.

A live check still running when the stop gives up waiting (15 s; on omp 7 s, a quarter of its stop
budget) can finish after the turn ended; its questions then arrive with the next prompt, or never if
the session ends there.

**On omp, opencode and Codex** a tool call's context reaches the model before its next step, as on
Claude Code: omp takes it as the `tool_result` handler's `additionalContext` (a hidden message; the tool
output stays as it is), opencode 1.x appends it to the tool output, opencode 2.x steers it into the
session as a hidden input, and Codex adds it as a developer message to the next request (in code mode
only after the whole `exec` script). None of them gives borg a Claude transcript, so a live check there
reads the turn's tool calls and edits, not the agent's prose. The stop still reads the agent's last
message. Details per harness: [Capabilities](harnesses.md#capabilities).

Live mode needs the consequence model. Under `BORG_MATCH=bm25`, `BORG_ONLY` or `BORG_CRITIC` it does
nothing: the drones run in hook mode. `borg doctor` warns and `borg status` notes it on the mode line
(the hook's own stderr note is not shown by the agent).

**Time limits.** The turn-end call stops after `BORG_PREDICT_TIMEOUT_S` (45 s). Claude Code's and Codex's
Stop hooks get a 90 s timeout from `borg install`, and opencode's adapter gives a stop the same 90 s.
omp gives a stop 30 s, which no setting changes; borg's stop budget for omp is 28 s, which the adapter
passes on as `BORG_STOP_BUDGET_MS`, so the call there stops after at most 18 s. A call that fails or
runs out ends the turn without questions.

## Why live is the default

The rule was fixed before the run: live becomes the default if, on an internal benchmark of real
engineering tasks with graded outcomes, it passes more criteria than hook (mean difference > 0, 95% CI
lower bound ≥ −0.5) and causes no more redo rounds per run. 3 tasks × 3 seeds per mode, the same
lessons, solver (Claude Opus 5) and check model (Sonnet 5.5):

| Per run | hook | live |
|---|---|---|
| Criteria passed, live − hook (8 pairs; one live run timed out and its pair is dropped) | | +0.38, 95% CI [0, +0.88] |
| Redo rounds from a blocked stop | 0.5 | 0 |
| Lessons delivered during the turn | 0 | 9.6 |
| Borg's own model cost | $0.07 | $0.56 |
| Whole run cost (agent, borg) | $4.97 | $5.05 |
| Wall time | 464 s | 467 s |

The rule fired. Live never passed fewer criteria than hook on the same task and seed; it passed more in
2 of 8 pairs. The gain is small and the sample is small (p = 0.50), so treat it as "not worse, sometimes
better, never blocked" rather than a proven win. Borg's side costs about 8× more ($0.49 more per task),
yet whole runs cost about the same ($5.05 vs $4.97). Switch back with `"mode": "hook"` when borg's own
spend matters more.
