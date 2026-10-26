# Delivery and modes

[← README](../README.md)

Borg spends tokens only where a lesson is likely to matter.

- [When a lesson arrives](#when-a-lesson-arrives)
- [Delivery modes](#delivery-modes)
- [Why live is the default](#why-live-is-the-default)

## When a lesson arrives

![One turn: free early delivery, a free word gate at the turn end, one graded model call, an extra round only for a grade-3 lesson](img/turn.svg)

1. **Early, free.** When a prompt or touched file first names a lesson's topic, Borg adds that topic's
   top lessons to the context, by word matching only, on Claude Code only so far: omp, opencode and Codex
   could take it, but it waits for an evaluation there (`Caps::early_delivery`). Standing lessons
   (ratified constraints, see [Saying no](learning.md#saying-no)) are no ranking guess: they arrive early
   on every harness that takes the context, once their scope is named, an unscoped one at the first
   prompt. `BORG_EARLY=off` turns early delivery off.
2. **Turn end, graded.** A free word gate (`BORG_PREFILTER`) skips most turns. Otherwise one model call
   grades every eligible lesson: 3 = certain, 2 = likely, 1 = worth a thought. A grade 3 blocks the stop
   and the agent works one more round. Grades 2 and 1 arrive free with the next prompt or touch. In live
   mode, the default, the same call also runs during the turn, after the agent changes something.
3. **Judged by meaning.** The same call checks whether what was handed over earlier changed the agent's
   work: questions it asked, and free deliveries (early and queued), each recorded with its channel and
   its asking probability. Questions keep their judge slots first (at most 10 wait); free deliveries alone
   never pay for a call. A correction merged into a lesson delivered free counts against it; `borg learn`
   settles the rest at session end. See [Mode and channel](statistics.md#what-is-counted).
4. **System gate.** When the repo maps to a known system, lessons scoped to other systems stay out, even
   if the conversation names them ([System map](configuration.md#system-map)).

## Delivery modes

Each collective delivers in one of two modes. Both use the same lessons, the same graded model call and
the same questions. They differ in *when* the call runs.

- **live** (the default): the call also runs *during* the turn, so a lesson can land before a mistake
  spreads across ten edits. It reuses the hook machinery: no daemon, no extra process between turns.
- **hook**: the call runs at the turn end only. A lesson that applies mostly arrives after the work is
  done, and a grade 3 costs the agent a redo round. Borg's own side costs about an eighth of live.

| | hook | live |
|---|---|---|
| Prompt | Early delivery and waiting grade-2/1 lessons, free | The same, plus questions from a live check that finished since |
| Tool call | Early delivery, free | The same. After a state-changing call (an edit, or a shell command that is not read-only), a check of the turn so far starts in the background; a later tool call starts one while changes wait unchecked. A live check has no word gate: a turn half done has too few words for it. Its grade-3 lessons arrive with the next tool call as "borg check of your work so far"; the agent answers them as it works. Grades 2 and 1 arrive free, as at the turn end |
| Turn end | The free gate, then one graded call; a grade 3 blocks for one more round | The same backstop. When a live check that started this turn covered every change and nothing changed since, the stop skips its model call. A live check's grade 3 that no tool call delivered yet blocks the stop, as in hook mode. Lessons asked live are not asked again |
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
budget) can finish after the turn ended; its grade-3 lessons then arrive with the next prompt, or never
if the session ends there.

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
Stop hooks get a 90 s timeout from `borg install`. omp gives a stop 30 s, which no setting changes; the
adapter budgets 28 s (`BORG_STOP_BUDGET_MS`), so the call there stops after at most 18 s. A call that
fails or runs out ends the turn without questions.

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
