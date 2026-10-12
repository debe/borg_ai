# Statistics and the live page

[← README](../README.md)

Borg counts what it does: turn ends it checked, questions it asked, what the drones did with them,
corrections, steering, the queen's decisions and model costs. Each team member sees the whole team's
numbers, and `borg web` shows them with the collective's knowledge on a local page.

- [The page](#the-page)
- [How counting works](#how-counting-works)
- [What is counted](#what-is-counted)
- [Privacy](#privacy)

## The page

```sh
borg web                  # http://127.0.0.1:7979/, the collective of the current directory first
borg web --port 8080 --dir ~/work/some-repo
```

The page listens on 127.0.0.1 only and updates live: within about a second of a store file changing,
it fetches the views that changed. It reads the same files as `borg top`, so it needs no
`borg mesh serve` and also shows private collectives. Install it with
`cargo install --path crates/borg-web --locked`.

| API | Returns |
|---|---|
| `/api/collectives` | The collectives on this machine and which one to show first |
| `/api/stats?c=` | Totals, a zero-filled series per day, and the same per engineer |
| `/api/lessons?c=` | Every lesson with its memory state and how often it was asked, used, ignored or corrected |
| `/api/feed?c=&limit=` | This machine's notable events, newest first |
| `/api/queen?c=` | The queen's questions and answers |
| `/api/drones?c=` | The drones, as in `borg top` |
| `/api/events?c=` | A server-sent event stream naming the views whose files changed |

## How counting works

1. **Events, local.** Hooks and commands append one line per event to
   `<store>/stats/events.jsonl`. The line can hold the session, the repo and a short excerpt of a
   correction, so this file never leaves the machine.
2. **History, once.** The first time a store is counted, Borg derives earlier events from the logs it
   kept anyway: corrections, outcomes, lessons, the queen's queue, imports, expert calls and steering.
   It writes them to `<store>/stats/backfill.jsonl`. Turn ends from before then are dated by the time
   their session's state file last changed.
3. **Rollups, shared.** At each session end, and once a minute while `borg web` runs, the events
   become one line per machine, engineer and UTC day in `<store>/stats.jsonl`. A rollup holds counts,
   cost and lesson ids. The mesh syncs this file like lessons, and the newest version of each line
   wins everywhere. Team members' activity appears after their next rollup reaches you.

## What is counted

| Kind | When |
|---|---|
| `turn_end` | A drone finished a turn |
| `gate_skip` | The free gate decided no lesson applies: no model call, 0 tokens |
| `check` | The turn-end model call; `n` lessons shown, its cost and duration. `why: failed` when it gave no answer |
| `block` | An extra round: the drone was stopped and asked `n` questions |
| `early`, `queued_free` | Lessons handed over free at a prompt or touch |
| `changed`, `ignored`, `corrected`, `not_corrected` | What happened after a lesson was asked: used, ignored, corrected anyway, or no correction followed |
| `correction` | An engineer corrected a drone: a rejected tool call or a pushback |
| `lesson_new`, `lesson_merged`, `not_a_lesson` | What `borg learn`, an import or a queen answer made of it |
| `model_call` | A model call outside the turn end: learn, assimilate, facets, the expert drone |
| `steer_issued`, `steer_delivered`, `steer_dropped` | A signed directive sent, shown to a drone, or refused on receipt |
| `pause_refused` | A tool call refused because its drone was paused |
| `queen_queued`, `queen_delivered`, `queen_answered`, `queen_skipped`, `queen_dismissed` | The queen's questions; an answer carries the time it took |
| `assimilated` | One imported chunk of team documents and the lessons it gave |

`<kind>_n` in the counts sums the events' `n`, for example `block_n` is the number of questions asked.

## Privacy

Shared rollups name the engineer (their git email) and carry counts, cost and lesson ids per day. They
never carry prompts, corrections, session ids or repo names. Those stay in the local event files.
Private and off collectives never sync. Delete `<store>/stats/` and `<store>/stats.jsonl` to drop the
statistics; Borg starts counting again, and derives history again, at the next event.
