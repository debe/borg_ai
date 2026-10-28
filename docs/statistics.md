# Statistics and the live page

[← README](../README.md)

Borg counts what it does: turn ends it checked, questions it asked, what the drones did with them,
corrections, pauses and vetoes, messages between sessions, the queen's decisions and model costs. Each team member sees the whole team's
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

The page listens on 127.0.0.1 only and answers only requests addressed to `127.0.0.1`, `localhost` or
`[::1]`: any other Host gets a 403, so a web page whose name re-resolves to 127.0.0.1 (DNS rebinding)
cannot read it. It updates live: within about a second of a store file changing,
it fetches the views that changed. It reads the same files as `borg top`, so it needs no
`borg mesh serve` and also shows private collectives. Install it with
`cargo install borg-web --locked` (or `--path crates/borg-web` from a checkout).

| API | Returns |
|---|---|
| `/api/collectives` | The collectives on this machine, each with its delivery `mode` (`hook` or `live`), and which one to show first |
| `/api/stats?c=` | Totals, a zero-filled series per day, the same per engineer, the number of machines counted (`nodes`) and the support of off-policy evaluation on this machine (`ope`: decisions, and those left out for lack of `p` or a pool). Totals carry `counts`, `usd`, `usd_by` (cost per kind), `usd_mode` (check cost per delivery mode), `channels` (asks and results per channel) and `dur_ms` |
| `/api/lessons?c=` | Every lesson with its memory state and how often it was asked, used, ignored or corrected |
| `/api/feed?c=&limit=` | This machine's notable events, newest first (200 by default, at most 2000) |
| `/api/queen?c=` | The queen's questions and answers, and the team's attention reward (`queen_no`, `queen_stamp`, `queen_missed`) |
| `/api/drones?c=` | The sessions, as in `borg top`, each with its unread messages |
| `/api/messages?c=&limit=` | Messages between sessions from the last 7 days, newest first: sender, recipient (`to_kind`: `all`, `engineer` or `session`), text, how many sessions here were shown it, and the unread total |
| `/api/learning?c=` | Whether learning works, per week: lessons written per source, the asked→changed and repeat-veto counts; each section with its `scope` (`team` or `this machine`); `weeks` run to the current one (`partial_week`), and series history cannot derive are `null` before `live_from` |
| `/api/events?c=` | A server-sent event stream naming the views whose files changed |

**Is learning working?** The Learning tab answers it per collective, week by week, from the logs
Borg keeps anyway (no state of its own, so the history is there from the first view): lessons written
per source (session, review, regret, veto, queen, import) and the team's lessons written
and merged; the share of asked lessons that changed the work (should rise) and of vetoes that repeated
an earlier "no" (should fall). Each panel
is labelled `team` when it folds synced logs (rollups, lessons) of a team collective,
or `this machine` when it folds the local event log, the only place a lesson's source is kept.
The weeks run up to the current one, marked "so far", so an idle month shows as idle weeks. The team's
new lessons are counted from the lessons themselves, each once in the week it was learned. The history derived before live counting began holds no lesson sources, merges or
repeated vetoes: those series start at the week live counting began on this machine (the API's
`live_from`; earlier weeks are `null`), and a repeated veto is dated when it was said, in its veto's
week. A "verdict on an asked lesson" is a Stop's changed or ignored, or a later correction anyway, which
one lesson can get on top of either, as on the Conscience tab.

**The tabs.** Overview (the day's activity and a live feed), Knowledge (the lessons), Conscience
(checks and what their questions led to), Learning (above), Steering (corrections, pauses, resumes,
vetoes and refused steers), Messages (the conversation between sessions, sends and refusals), Queen,
Cost and Drones (which sessions run where, and their unread messages).

## How counting works

1. **Events, local.** Hooks and commands append one line per event to
   `<store>/stats/events.jsonl`. The line can hold the session, the repo and a short excerpt of a
   correction, so this file never leaves the machine.
2. **History, once.** The first time a store is counted, Borg derives earlier events from the logs it
   kept anyway: corrections, outcomes, lessons, the queen's queue, imports, expert calls, pauses and vetoes.
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
| `live_check` | Live mode: a model call during the turn (`borg check`); as `check`, but never a turn end |
| `live_ask` | Live mode: `n` grade-3 lessons of a live check, delivered with the next prompt or tool call. No extra round |
| `early`, `queued_free` | Lessons handed over free at a prompt or touch |
| `changed`, `ignored`, `corrected`, `not_corrected` | What happened after a lesson was asked: used, ignored, corrected anyway, or no correction followed |
| `regretted` | `borg learn` counted a later fix, or a review finding, against a lesson asked before the decision (`lessons`: that lesson). Its share of the blame is the `credit` of the `regretted` record in `outcomes.jsonl`, keyed by the regret episode (`cause`) |
| `tested` | `borg learn` credited a lesson asked before the session's own tests went green: a failed run after the question, then a pass, or a pass after the drone changed its work on it; never for a question it ignored (`lessons`: that lesson). Weighs 0.5 in its usefulness, once per session and lesson |
| `correction` | An engineer corrected a drone: a rejected tool call or a pushback |
| `veto` | An engineer said no to a drone's work (`borg no`, or a prompt starting "borg no"); `lessons` the lessons shown in the session so far, `n` their number, `why` `cli`, `prompt` or `mesh` (a `borg no` from another machine, counted where the session runs) |
| `veto_repeat` | `borg learn` merged a veto into a lesson an earlier veto made: the same reason said twice. Repeated vetoes per veto is the rate the collective should bring to zero. It is counted when the veto is learned and dated when the veto was said, so the latest days can still grow |
| `review_finding` | A workflow review step found a bug and its fix step fixed it before any commit (recorded as an episode; `why`: severity) |
| `lesson_new`, `lesson_merged`, `not_a_lesson` | What `borg learn`, an import or a queen answer made of it |
| `model_call` | A model call outside the turn end: learn, assimilate, facets, the expert drone |
| `steer_issued`, `steer_delivered`, `steer_dropped` | A pause, resume or veto message (`borg pause`, `borg resume`, or `borg no` naming a session on another machine) sent, shown to a session, or refused because the sending machine may not steer here (`why`: its role, or that it is revoked or unknown). On the first two, `why` is the kind (`pause`, `resume`, `veto`). A store from before 0.4 adds its old directives to the history |
| `msg_sent`, `msg_delivered`, `msg_dropped` | A message (`borg tell`, `borg ask`) sent, shown to a session, or dropped: `why: rate` when the rate limit refused it, `why: bounds` when the log's 500-line cap cut it (`n` lines). Lines that expire after 7 days are not counted |
| `pause_refused` | A tool call refused because its drone was paused |
| `queen_queued`, `queen_delivered`, `queen_answered`, `queen_skipped`, `queen_dismissed` | The queen's questions; an answer carries the time it took |
| `queen_no`, `queen_stamp`, `queen_missed` | The queen's attention reward, shown on the Queen tab and never optimised: an escalation answered by changing course, one answered "yes, fine", and a veto or a later fix of merged work from a session the queen never escalated |
| `assimilated` | One imported chunk of team documents and the lessons it gave |

`<kind>_n` in the counts sums the events' `n`, for example `block_n` is the number of questions asked.

**Mode and channel.** Events from a hook carry `mode`: the delivery mode it ran in (`hook` or `live`,
[Delivery modes](delivery.md#delivery-modes)), on every harness. Asks and their results carry `channel`: how the lesson
reached the drone, `stop` (a turn-end block), `live` (a live check), `early` or `queued` (free). The
`asked` outcome records and the judge's `asked_then_changed` and `asked_ignored` records in
`outcomes.jsonl` carry the same `channel`. A free delivery's `asked` record counts as its `early` or
`queued_free` event, never as a `block`, and the next turn end's model call judges it as it judges a
question (questions keep their judge slots first; free deliveries alone pay for no model call). A
correction `borg learn` merges into a lesson delivered free is `asked_then_corrected`; one a graded check
left unchosen stays `applied_then_corrected` too, since a free delivery asks nothing, and `borg debug ope`
leaves free deliveries out: they draw from no pool. A veto adds a `vetoed` record per lesson shown in the session
before it (no channel; it is counted as the `veto` event, not as an outcome kind of its own). `asked`
and `graded` records also carry `p`, each lesson's probability of being asked under the policy in force,
for off-policy evaluation (1 for a queued hand-over; for an early delivery, the share of the session's
draws that deliver it). `outcomes-team.jsonl` holds the redacted outcomes the mesh syncs (session and
repo hashed); the statistics never read it, since the rollups already sync their counts. Rollups keep two splits beside the counts:

- `usd_mode`: the cost of `check` and `live_check` per mode. Checks from before modes existed count as
  `hook`. It is part of `usd`, never added to `usd_by`.
- `channels`: events per `<kind>@<channel>`, for example `changed@live` (a question a live check asked
  that changed the work) or `block@stop`.

The Conscience page splits the turn-end chart into turn-end and live checks and states what the
questions led to per channel; the Cost page shows check cost by mode per day.

The session state file (`<store>/state/<session>.ctx`) marks live mode too: `#borg-asked <ids>`
followed by `#borg-live <ids> ms=<epoch ms>` when a live check's lessons were delivered, and
`#borg-live-skip <score>` when a live check's free gate skipped the model, or `#borg-live-skip empty`
when it had no lesson to ask and none to judge (a check with nothing in its pool, live or at the turn
end, makes no model call; the turn end logs `#borg-skip empty`). A live check's model call is
logged as `#borg-predict-cost`, like a turn end's, and as `live usd=… lessons=…` in the `.cost` file,
so `borg status` counts it apart.

## Privacy

Shared rollups name the engineer (their git email) and carry counts, cost and lesson ids per day. They
never carry prompts, corrections, session ids or repo names. Those stay in the local event files.
Private and off collectives never sync. Delete `<store>/stats/` and `<store>/stats.jsonl` to drop the
statistics; Borg starts counting again, and derives history again, at the next event.
