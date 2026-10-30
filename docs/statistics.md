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
| `POST /api/messages/seen?c=` | The Messages view showed the engineer these messages (a JSON body, `{"ids": [...]}`, at most 2000): those to the engineer are acknowledged as the engineer of the directory `borg web` started in, as `borg inbox` does; any other id is ignored. 415 without a JSON body, so another site's page cannot call it (a JSON body needs a CORS preflight, which the server never answers) |
| `/api/select?c=` | Lesson selection ([Which lessons are asked](delivery.md#which-lessons-are-asked)), from the store's selection summary: the `policy` of this process, the `default` (`learned`) and the `source` (`reverted` once the monitor put the fixed rule back), the `gate` (the quality the learned policy may still risk below the fixed rule, not money; 0 under the fixed rule), the `fit`, `checks` and `sessions` folded, `deviations` from the fixed rule (routes only), `fallbacks` (per `reason` a learned decision took the fixed rule instead, `lock_busy`, `lock_failed` and `unrecorded`, the graded checks it did of those folded), `spend` per source (`stop`, `live`: calls, extra rounds, dollars), the engineer's `budget` (`{tokens, spent, price}`, `null` unless one is set), `effects` with 95% intervals, the forgetting `half_life`, label health (`labels`: the judged share per route, the unclear share, the placebo rate; the free row leaves out applies of a lesson the session already had or had waiting in its queue, which delivery drops as given before or queued twice, while a question of one is delivered and judged as any other), the `declaration`'s effect on the judge, the fitted `delivery` rates (`free_stop`, `free_live` and `ask_live` weigh a route's value: `ask_live` a live question's of a check with a 3, `free_live` a hand-over's and a live question's of a check without one; `later` and `wait` are only shown), the monitor's `evaluation` (its `mean` and upper bound `ucb` over its `sessions`, and the weights' health: labelled `units`, `ess`, `max_weight` against the mean, `truncated`), the `measurement` for the learned policy (`placebos` drawn and `judged`, `tail_shown` and `tail_applied`), `pool` yield by fill position, and `latency` per harness (the stop calls' `p50_ms` and `p95_ms` against the harness's `budget_ms`, `over`: failed calls). A section not measured yet is empty or `null` |
| `/api/learning?c=` | Whether learning works, per week: lessons written per source, the asked→changed and repeat-veto counts; each section with its `scope` (`team` or `this machine`); `weeks` run to the current one (`partial_week`), and series history cannot derive are `null` before `live_from` |
| `/api/memory?c=&absorbed=` | The `borg memory export --json` document (`{"schema": 1, "collective", "lessons"}`, [Memory](learning.md)); `absorbed=true` keeps only absorbed lessons |
| `/api/update?c=` | borg itself: this borg (`borg`), its mesh `protocol`, whether the release check is on (`check`), the last check's `latest`, `checked_ms` and `error` (`null` before the first), `update` (a newer release exists and the check is on), and `members`: each member the mesh last heard from with its `key`, `engineer` (when known), `name` (the engineer, else `peer <short key>`), `borg` (`null`: older than 0.7, it sent no version), the `protocol` its last session agreed on (or the one it speaks when nothing syncs with it), `ms`, `syncs` (`false`: nothing syncs with it) and `note` (what it misses on an older protocol, or why nothing syncs; `null` on this node's footing). No members without the store's `mesh/` ([Versions on the mesh](team-sync.md#how-a-sync-works)) |
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
(checks, what their questions led to, and lesson selection: its policy and the default, spend per
session, effects, label health, what is measured for the learned policy, the delivery rates, pool
yield and stop latency per harness, the learned decisions a guard gave the fixed rule, per reason,
and under the learned policy its gate, the checks it deviated from the fixed rule on (on evidence or by a test draw), the question-count test
(checks drawn, controls, refused by the gate, the draws of s, the risk share in force, the readout once
due) and the monitor's reading, each "not measured yet"
until it is), Learning (above), Steering (corrections, pauses, resumes, vetoes and refused steers),
Messages (the conversation between sessions, sends and refusals; the messages to the engineer it lists
are acknowledged), Queen, Cost and Drones (which sessions run where, and their unread messages).

**The banner.** Over the tabs, the page says when borg itself has news, and only then: a newer
release than this borg, with `borg update` to install it, and each member whose mesh protocol differs
from this node's: one on an older protocol with what it misses, one nothing syncs with and why. It
reads `/api/update` and follows the release check and the mesh's version records live.

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
| `live_ask` | Live mode: the `n` lessons a live check asked (under the fixed rule, a check with a grade 3), delivered with the next prompt or tool call. No extra round |
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
| `msg_sent`, `msg_delivered`, `msg_dropped` | A message (`borg tell`, `borg ask`) sent, shown to a session, or dropped: `why: window` when the sender's window to that recipient was full, its 10-minute rate not yet due, or its unread messages there filled its machine's share of the log ([Pacing](commands.md#messages)), `why: bounds` when the log's bound of 1 MiB per member and class cut lines (`n` lines). `why: rate` is 0.5's rate limit, in older events. Lines that expire after 7 days are not counted |
| `pause_refused` | A tool call refused because its drone was paused |
| `queen_queued`, `queen_delivered`, `queen_answered`, `queen_skipped`, `queen_dismissed` | The queen's questions; an answer carries the time it took |
| `queen_no`, `queen_stamp`, `queen_missed` | The queen's attention reward, shown on the Queen tab and never optimised: an escalation answered by changing course, one answered "yes, fine", and a veto or a later fix of merged work from a session the queen never escalated |
| `assimilated` | One imported chunk of team documents and the lessons it gave |

`<kind>_n` in the counts sums the events' `n`, for example `block_n` is the number of questions asked.

**Mode and channel.** Events from a hook carry `mode`: the delivery mode it ran in (`hook` or `live`,
[Delivery modes](delivery.md#delivery-modes)), on every harness. Asks and their results carry `channel`: how the lesson
reached the drone, `stop` (a turn-end block), `live` (a live check), `early` or `queued` (free). The
`asked` outcome records and the judge's `asked_then_changed` and `asked_ignored` records in
`outcomes.jsonl` carry the same `channel`. A placebo's verdicts carry `placebo`: the lesson was never
shown, so they measure the judge and are left out of every count, belief and team line, as is an
unclear verdict (`asked_unclear`). A free delivery's `asked` record counts as its `early` or
`queued_free` event, never as a `block`, and the next turn end's model call judges it as it judges a
question (questions keep their judge slots first; free deliveries alone pay for no model call). A
correction `borg learn` merges into a lesson delivered free is `asked_then_corrected`; one a graded check
left unchosen stays `applied_then_corrected` too, since a free delivery asks nothing, and `borg debug ope`
leaves free deliveries out: they draw from no pool. A veto adds a `vetoed` record per lesson shown in the session
before it (no channel; it is counted as the `veto` event, not as an outcome kind of its own). `asked`
and `graded` records also carry `p`, each lesson's probability of being asked under the policy in force,
for off-policy evaluation (for an early delivery, the share of the session's draws that deliver it; for
a queued hand-over, the routing check's `pf`). An early delivery's `asked` record also carries `rel`,
each lesson's score as a share of the best on-topic match, for a learned early rule.

**Selection records** (local, never in the team file). A `graded` record is one model call of a check,
also one that found nothing applying: its `check` id, the `policy` (`fixed`, `cap`, `attention` or
`learned`) and `fit`, what set the routes (`source`: under the learned policy `evidence` for the
evidence rule, `experiment` or `experiment_control` for a check of the question-count test, `gate` when
the gate kept the fixed rule, `size` past 16 applies, `fixed` when it could not decide yet, `reverted`
after the monitor's revert; `learned` on records from before the second design, which played one draw
of the model; `fixed`, `cap` or attention's source under the other policies), the test's k and s
(`xk`, the fixed rule's delivered lessons below grade 3 that may go free, and `xs`, the number of them
drawn to be asked; also on a check the gate refused the draw, which then asked as the fixed rule did;
absent on every other check), the collective's tolerance the check was decided under (`alpha`, its
`risk`, under every policy, absent at the default 0.1; a replay decides the check under it), the lessons with their grades and ranks, the asked (`chosen`) and `free` ones, `p`
and `pf` (the probability of being handed over free), the `placebo` draws and their probability `psi`,
the applies the session already had, or had waiting in its queue from another check, which could not
go free (`nofree`), the pool (`eligible`, `shown`, `tokens`, each lesson's `fill` position), the tail
sample (`tail`, the lessons past the budget it showed, each with the chance `pt`; their places follow
the applies' in `fill`, from `shown` on, and they are not in `shown` or `tokens`), the summary the
decision read (`seen`), why a learned check fell back to the fixed rule (`reason`: `lock_busy`,
`lock_failed` or `unrecorded`, [Which lessons are asked](delivery.md#which-lessons-are-asked)), and
the call (`model`, `prompt_hash`, `harness`, `version`, `dur_ms`, `failed`, `usd` as the CLI reported
it). A `delivery` record says what delivery did with each lesson of a check: `route` `asked`, `free`,
`placebo`, `dropped_done` (the session had it already, or it was queued twice), `dropped_chars` (past
the delivery's character bound) or `never_delivered` (the session ended first), with its `reason`. An
`ended` record marks each run of a session's end hook; the selection summary counts the session's
delivery rates there. Every result of a routed lesson carries the `check` too, and the judge's carry
its template version (`judge`) and whether the agent's declaration was left out (`stripped`). Records
from before 0.6.0 lack these fields and read as the fixed rule.

`outcomes-team.jsonl` holds the redacted outcomes the mesh syncs (session and repo hashed); the
statistics never read it, since the rollups already sync their counts. Rollups keep two splits beside the counts:

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
so `borg status` counts it apart. The whole pool's line there reads
`pool mode=all eligible=<n> shown=<n> budget=fits|full`, with ` tail=<n>` when a tail sample was
shown. A graded check writes `#borg-graded n=<applies> <id>:<grade>:<rank>,…`,
`#borg-select policy=… source=… fit=… gate=… applied=… @<check>`, `#borg-p` and `#borg-pf` before it
delivers anything; every delivery marker (`#borg-asked`, `#borg-early`, `#borg-queued`, `#borg-placebo`)
ends in ` @<check>`, the check that routed it, and `#borg-unjudged <ids> why=<reason>` names what a judge
call left out.

## Privacy

Shared rollups name the engineer (their git email) and carry counts, cost and lesson ids per day. They
never carry prompts, corrections, session ids or repo names. Those stay in the local event files.
Private and off collectives never sync. Delete `<store>/stats/` and `<store>/stats.jsonl` to drop the
statistics; Borg starts counting again, and derives history again, at the next event.
