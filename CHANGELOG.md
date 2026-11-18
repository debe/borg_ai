# Changelog

What changed in each release and what you need to do about it. The details live in the
[docs](https://github.com/debe/borg_ai/tree/main/docs).

## 0.9.0 (2026-11-18)

Engineers, drones and machines get readable names, and borg runs the mesh daemons and borg web as
services of your user that every update restarts.

### Changed

- An engineer is named by the `engineers` alias, else their git `user.name`, else the whole email,
  never the part before the "@". Members tell each other the names with their signed machine record.
- A drone is "Name · repo · harness" (`#2` for a second with the same label) instead of "Two of
  Five", and runs "on <hostname>" instead of "@<key>". `borg top` and `borg msg` take the label
  as well as the number ([Drones](https://github.com/debe/borg_ai/blob/main/docs/commands.md)).
- Each team collective's mesh daemon serves on a port of its own (its last one, else 7878, else a free
  one), so several run on one machine.

### Added

- `borg service [status|install|restart|remove]`: a mesh daemon per team collective and `borg web`
  as systemd user units (Linux) or launchd agents (macOS). `borg setup`, `borg install <h> --apply`
  and `borg update` install them and restart them on a new version; `borg doctor` reports them.

### Upgrading

- The update from 0.8.x installs the services and restarts them. Same-named units borg did not write
  (a hand-written `borg-mesh-<c>.service` or `borg-web.service`) are taken over; the old file is kept
  as `<file>.bak`. Stop a `borg web` or `borg mesh serve` you started by hand.
- `borg service remove` removes the services and keeps them off through later updates.

## 0.8.3 (2026-11-17)

A finished hand-off no longer leaves branches and refs behind that `borg doctor` warns about forever.

### Fixed

- The `borg handoff` listing and `borg handoff done` delete what a done hand-off left in this
  machine's clones if deleting it loses nothing: local hand-off refs, and `borg/handoff/<id>` branches
  whose commits another branch, remote-tracking branch or tag holds. This also works when `done` ran
  on another machine. `borg doctor` names only the branches that hold unmerged work or are checked out
  ([Done](https://github.com/debe/borg_ai/blob/main/docs/handoff.md#done)).

## 0.8.2 (2026-11-16)

borg web's Messages tab becomes a chat, and a detached turn-end check gets a time limit that fits its
own harness, so omp's slow checks stop failing at the limit.

### Changed

- borg web's Messages tab is a chat: conversations on the left (each pair of sessions, or a session and
  the engineer, plus one per broadcast address), the thread on the right with day separators, reply
  quotes and a button that copies the `borg tell <session> --re <id> "…"` reply. Filters for text,
  tells or asks, recipient, this machine or teammates, repo and unread. The Timeline panel is gone;
  refused sends are under the tiles ([Statistics](docs/statistics.md#the-page)). `/api/messages` rows
  carry the conversation, a `status` and each end's repo, engineer and state.
  Only the messages to the engineer in the open conversation are acknowledged now, not the newest 60.

### Fixed

- `borg status` ("stop p95") and borg web's "Blocking turn-end check speed" date each harness's newest
  blocking check: since 0.7.7 a check blocks only under `BORG_ASYNC=off`, so their failures can be
  weeks old (an omp store showed 21 of 26 failed, all from 0.7.6 and before).
- A detached turn-end check's call limit is derived per harness (`BORG_HARNESS`). It counts calls that
  failed at the limit, and is capped at four times `BORG_PREDICT_TIMEOUT_S` (180 s). Claude's ~9 s
  calls set omp's limit, and timeouts never raised it, so omp failed 17 of 19 checks in a day; on that
  store omp's limit is now 142 s and Claude's 45 s.

## 0.8.1 (2026-11-16)

A hand-off finds the taker's clone even when no session opened it since 0.8 or its remote is spelled
another way.

### Fixed

- The mesh records every clone in the collective's paths, not only those a session started in, so
  `--to` and borg web see them ([Take](docs/handoff.md#take)).
- An offer names its push URL and the one its ssh host alias stands for; a take with no URL match asks
  candidate clones' remotes whether they hold the hand-off. A repo sharing root commits is never taken.
- `--to` warns instead of refusing when no clone of the repo is known on the receiver's machine.

## 0.8.0 (2026-11-14)

A session can hand its work over to another drone, like a transferred call: the repo's git remote
decides who takes it, and the original's edits there are refused from then on.

### Added

- `borg handoff "<note>" --brief FILE|-` offers the session's work: the repo it runs in and each repo
  it started or wrote in that holds work is snapshotted (untracked files in; ignored files, secrets, new
  binaries of an untracked kind and files past git's big-file threshold out) and pushed with the brief
  under `refs/borg/handoff/<id>/` ([Hand-off](docs/handoff.md)).
- `borg handoff take <id>`: exactly one take wins; the work is checked out as `borg/handoff/<id>` and
  the brief printed. The offering engineer's take is the take-back; a repo with no clone is listed
  with its clone URL.
- An open offer is refreshed at each stop of its session; a take that crosses a refresh keeps the
  state it claimed, the remote's refs are put back to it, and the original is told which of its
  changes came after it.
- `--to ENGINEER|NODE` asks someone, after checking one of their machines can take it;
  `borg handoff decline <id>` says no.
- `borg handoff abort <id> [--why TEXT]` (the owner, an admin or a steerer) and `borg handoff done <id>`
  (deletes the hand-off's refs and the clean work tree a take made).
- `borg handoff off|on`: an admin switches the collective's hand-offs; open offers are aborted.
- `borg handoff`, `borg top`, borg web (Hand-offs tab, Drones badges) and a session's prompt in a clone
  show the team's hand-offs; `borg doctor` names branches left by done ones.

### Upgrading

- Older borgs neither see nor carry hand-offs; the remote still decides, so nothing breaks. A
  membership version that switches hand-offs off is adopted by 0.8.0 nodes only: an older node stops
  following membership changes from then on, revocations included. `borg handoff off` is refused while
  such a machine was seen in the last day; upgrade every machine first
  ([Switching hand-offs off](docs/handoff.md#switching-hand-offs-off)).

## 0.7.13 (2026-11-13)

A mesh session does about a tenth of 0.7.12's work, and borg web keeps its rollups between requests.

### Changed

- `borg mesh serve` loads the vector model once per process and touches the index only when a session
  brought lessons (or quarantined some): a session that stores a lesson costs ×0.076 the instructions
  of 0.7.12, one without ×0.45.
- A mesh session checks each membership signature once and keeps the reconciliation fingerprints of
  ranges that did not change: ×0.57 the work with vectors off, and a 10× store costs 1.3× per session
  instead of 3.9× ([What a peer keeps](docs/team-sync.md#how-a-sync-works)).
- borg web reads the statistics log whole once, then only what was appended: the page rests at about
  170 MB after load instead of 320 MB.
- The serve process holds the model it loaded (about 40 MB of heap plus the mapped model file).

## 0.7.12 (2026-11-13)

The mesh and the page cost a fraction of the CPU and memory they did, and messages wait at most a
minute.

### Changed

- Message pacing is looser and learns across sessions: the window is earned between machines, so a
  new session starts with the trust its machine's earlier sessions earned, and nobody waits longer
  than a minute (was 10 minutes, or 7 days for a session that ended unread)
  ([Pacing](docs/commands.md#messages)).
- `borg mesh serve` does about 1/300 of the sync work per session (19 G → 64 M instructions, 3.3 GB →
  2 MB allocated on a team store; 1/15 with the vector model, which each storing session still
  loads): logs are parsed without a JSON value per line, and each log and the origin records are read
  whole once, then only what was appended ([What a peer keeps](docs/team-sync.md#how-a-sync-works)).
- borg-web parses only what a page shows and folds logs as it reads them: 8 requests at once peak at
  158 MB of heap instead of 447 MB, and the page rests at about 200 MB instead of 270-320 MB (glibc
  capped at 4 malloc arenas; `MALLOC_ARENA_MAX` overrides).

### Fixed

- Two borg-web requests saving the outcomes fold at once could collide on one temp file.
- 0.7.10 and 0.7.11 counted no judge's ignore in the statistics, so the learning page's asked→changed
  share read 100%. Ignores count again, and the missed ones are recounted once from the outcomes log.

## 0.7.11 (2026-11-10)

### Changed

- Every session listens by itself: a message wakes it while it is idle, with no `borg listen`. Before,
  no session was woken unless it ran `borg listen`, so messages waited for the engineer's next prompt
  (68 and 216 minutes on the author's machine). `borg listen --off` or `BORG_LISTEN=off` opts out
  ([Waking a session](docs/commands.md#waking-a-session)). Run `borg install` again on Claude Code.
- Claude Code: a background Stop hook (`asyncRewake`) waits for messages, so the model is no longer asked to
  start `borg listen --wait`.

### Fixed

- A stop signal sent to `borg listen --stream` while it restarted itself after an update could be lost.
- `borg listen --off` fails when it cannot record the opt-out, instead of saying it held.

## 0.7.10 (2026-11-08)

### Fixed

- Ignores recorded before 0.7.5 no longer count against a lesson: that judge could not say a point never
  came up and filed most such questions as ignored. On the author's stores, 210 of 448 lessons sat below
  0.34 usefulness, 14 after ([Learning](docs/learning.md)). Upgrade every machine of a team: a peer
  before 0.7.10 publishes its ignores without the judge's version, and other machines then do not count
  them. After the upgrade, each machine publishes its ignores since 0.7.5 once more, with the version.

## 0.7.9 (2026-11-05)

### Changed

- Conscience tab reorganised around the developer's questions, with hover help on every card; skip and
  drop reasons are counted (upgrade borg and borg-web together; older nodes' days show "reason not recorded").

## 0.7.8 (2026-11-04)

### Fixed

- Hooks no longer re-read the whole outcome history: a prompt or tool-call hook took 55-60 ms of CPU on a
  7.8 MB history and grew with it; it now folds only new lines into `beliefs.bin` (8-9 ms). A long
  session's turn end took about 2 s of CPU and grew with the square of the session; it now takes about 70 ms.
- omp transcripts are swept again: since omp writes a `title` line first, borg found no omp session's
  transcript, so its corrections and answers waited for `borg assimilate`. Tested with omp 18.15.0.

## 0.7.7 (2026-11-03)

The stop never waits for the model, and every check logs the pool it showed, so a better order can be
measured before it is switched on.

### Changed

- The turn-end check runs detached: the stop returns at once, and its questions arrive with the next
  prompt or tool call; `BORG_ASYNC=off` blocks at the stop as before. [Delivery](https://github.com/debe/borg_ai/blob/main/docs/delivery.md#when-a-lesson-arrives)
- A newer check settles a question still waiting from an older one; one check runs per session.
- A question a newer check asks again is recorded `superseded`, not as an undelivered question.
- A lesson line shows the engineer's correction it was learned from (`BORG_LESSON_LINE=a1`: without).
- A pool its budget cuts fills its last 15% by a draw, and its tail sample adds as many of the lessons
  left out as fit in 15% of the budget, at least two (`BORG_BOUNDARY=off`: as before). [The pool](https://github.com/debe/borg_ai/blob/main/docs/delivery.md#which-lessons-are-asked)

### Added

- Waiting questions wake a listening idle session, with their text in the wake.
- `pools.jsonl`: each detached check's eligible lessons, their exact chance to be shown and the
  candidate orders' scores, logged only. [The pool log](https://github.com/debe/borg_ai/blob/main/docs/learning.md#the-pool-log)
- Retrieval cues per lesson at `borg learn` (`BORG_CUES=off`), and `borg cues` to backfill.
- Miss causes in `borg status` and borg web's Conscience tab.
- `borg status` shows the turn-end check: call limit, failures, superseded jobs, peak memory.
- A graded record keeps the model's design moves and each apply's why; stats count `check_dropped`.

### Upgrading

- Nothing to do. To keep the stop's own check and its block, set `BORG_ASYNC=off`.
- Lessons learned before 0.7.7 have no retrieval cues: `borg cues` estimates the cost, `borg cues --apply` adds them.

## 0.7.6 (2026-11-01)

The session-end judge judges every open delivery, sized by how much its model reads well, not by
two unmeasured limits. Live mode is gone.

### Changed

- At the session's end, `borg learn` judges every open delivery, not at most 40, and reads the
  session's work by a budget instead of its newest 24,000 characters. The lessons go in batches by
  delivery order, one model call per batch, four at a time; a session whose work fits is read whole.
  [Delivery](https://github.com/debe/borg_ai/blob/main/docs/delivery.md)
- A session too long for the budget is read per batch from both ends: right after the batch's
  earliest delivery and the session's end (`BORG_END_WORK=ends`, picked by a backtest); `after` and
  `newest` (0.7.5's) stay switchable. [Configuration](https://github.com/debe/borg_ai/blob/main/docs/configuration.md)
- A failed batch leaves only its own lessons unjudged.

### Added

- The budget is the judge's measured effective length per model, 72,000 characters until its cost
  lines show more or less is read well; a logged share of passes tries the size above.
  `borg status`, `borg top --json` and borg web's label health show it.
  [Delivery](https://github.com/debe/borg_ai/blob/main/docs/delivery.md)
- Every judge cost line names its model; a check's line also gives its prompt's size.
  [Statistics](https://github.com/debe/borg_ai/blob/main/docs/statistics.md)

### Removed

- Live mode, the check during the turn (`borg check`): 8× the cost, and no measured gain. Every check
  runs at the turn end. [Live mode](https://github.com/debe/borg_ai/blob/main/docs/delivery.md#live-mode-removed-in-076)

### Upgrading

- Nothing to do: `"mode"` and `BORG_MODE` are ignored, and `borg doctor` notes one still set. History
  from live mode stays readable.

## 0.7.5 (2026-11-01)

The judge says what happened to a lesson instead of "unclear", and judges a hand-over after the work,
not while it is still being done.

### Changed

- The judge gives one of five verdicts, each with a short reason in the outcome log: changed, ignored,
  not arisen (the lesson's point never came up), consequence remains (changed, but the consequence still
  follows) or unclear. A point that never came up moves no lesson's belief. Older outcome records read
  as before.
  [Outcomes and the usefulness belief](https://github.com/debe/borg_ai/blob/main/docs/learning.md#outcomes-and-the-usefulness-belief)
- Lessons handed over free and placebos are judged at the turn end, never by a live check mid-turn.
  Questions are judged as before.
  [Delivery](https://github.com/debe/borg_ai/blob/main/docs/delivery.md)
- borg web's Conscience page (Label health) and `borg top --json` show the shares of unclear, not
  arisen and consequence remains verdicts.

### Added

- At the session's end, `borg learn` judges every delivery still without a settling verdict once more,
  in one model call that sees the whole session's work; a session whose end hook never ran gets it
  when it is settled as idle. It costs one call per session with something open, and none with
  `BORG_LEARN=off`.
  [Delivery](https://github.com/debe/borg_ai/blob/main/docs/delivery.md)

### Fixed

- Every model call (claude, omp, opencode, the expert drone) hands its prompt over on stdin, never as
  a command-line argument. A long prompt failed on all of them (the kernel caps one argument at
  128 KiB), and an argument is visible to every process on the machine.

## 0.7.4 (2026-10-31)

Collectives stay separate: nothing crosses between them on its own, and crossing on your word is one
command.

### Fixed

- A personal lesson stays in the collective it was learned in, delivered only to you and never synced.
  It no longer moves to your private collective, and `borg reach` never hides a teammate's lesson, one
  no engineer is recorded on, or an imported one.
  [Who hears a lesson](https://github.com/debe/borg_ai/blob/main/docs/configuration.md#who-hears-a-lesson)
- A personal lesson is learned once, not again on every `borg learn`.
- Corrections of a repo that now routes to another collective are held, not learned in the old one.

### Added

- `borg move <lesson-id|repo-path> --to <collective>` moves a misplaced lesson, or a repo's waiting
  corrections; a dry run until `--apply`. A teammate's lesson is copied, never hidden.
  [Commands](https://github.com/debe/borg_ai/blob/main/docs/commands.md#learning)
- `borg correct --collective <name> "<words>"` records a correction in another collective, one command
  per collective, learned there when the session ends. Only your words cross. The `borg` skill runs
  one for each collective a correction concerns.
  [Correcting another collective](https://github.com/debe/borg_ai/blob/main/docs/learning.md#reach-and-collectives)
- `borg doctor` lists held corrections, possibly misplaced lessons and the repair below, each with its
  `borg move`. It never moves anything.

### Upgrading

- Run `borg doctor`. It lists anything to move, including lessons an earlier borg moved to your
  private collective; run each `borg move` it names, dry run first, then `--apply`. The `personal` key
  in `collectives.json` is no longer used and stays valid.

## 0.7.3 (2026-10-31)

Lesson selection's labels and status lines now mean what they say, so the learned policy learns from
the right verdicts.

### Fixed

- A lesson handed over free and then asked is judged on the question once it is answered, and a
  session's settle leaves no delivered lesson open.
  [Which lessons are asked](https://github.com/debe/borg_ai/blob/main/docs/delivery.md#which-lessons-are-asked)
- `borg status` and borg web say "gate spent" instead of "cold start" when the allowance is used up.
- The monitor's ESS and largest weight count only labels that can move its estimate.
- The learned pool grows past 12,000 tokens only on a positive gain and latency measured near that size.
- `borg debug select --replay` counts records copied from another store's log instead of failing P1.
- `borg assimilate --apply` no longer appends an episode a turn end captured while it ran.

### Changed

- omp's model calls ask `claude-sonnet-5` unless a model is pinned, falling back to omp's default
  where no provider offers it. `BORG_OMP_MODEL` names another; `default` keeps omp's own.
  [Model runner](https://github.com/debe/borg_ai/blob/main/docs/configuration.md#environment-variables)
- A live check judges a free hand-over or placebo only once work follows it.
- The question-count test's readout is due at 300 checks in 15 sessions, or 30 sessions;
  `--replay --file A --file B` pools stores.
  [Evidence](https://github.com/debe/borg_ai/blob/main/docs/evidence.md#lesson-selection)
- The judge's `.cost` line names the ids the model returned and those matching no pending lesson; the
  graded record logs its pool `budget`.

## 0.7.2 (2026-10-31)

Corrections and answers reach Borg from every harness's own transcript, so a harness update can no
longer silence them unnoticed.

### Fixed

- Rejected tool calls are captured again on Claude Code 2.1.312 and later, which stopped telling the
  hook about them. Borg now reads them from the transcript at each turn end.
  [The harness's own transcript](https://github.com/debe/borg_ai/blob/main/docs/harnesses.md#the-harnesss-own-transcript)
- A session left open no longer keeps its corrections waiting: `borg learn` picks them up once it has
  been idle for a day.

### Added

- Your answers to the agent's own questions (Claude Code, omp, opencode, Codex) become episodes, and
  lessons where they state a rule. [Sources of lessons](https://github.com/debe/borg_ai/blob/main/docs/learning.md#sources-of-lessons)
- `borg doctor` warns, per harness version, when a harness's transcripts name more rejections or
  answers than Borg holds, and says when a harness stopped telling the hook. `borg status` and borg
  web show which harness's transcript a correction came from.
- `borg assimilate` reads Codex sessions and answers, and gains `--session` and `--codex`.

### Upgrading

- Run `borg doctor`: it names the day capture broke and the `borg assimilate --since <day>` that
  recovers it. The dry run states the cost (one model call per episode); add `--apply` to learn it. A
  longer `--since` also recovers older answers, which no earlier release captured.
  `BORG_TRANSCRIPT_CAPTURE=off` turns transcript reading off.

## 0.7.1 (2026-10-30)

Fixes the `borg` skill for harnesses that read its header strictly.

### Fixed

- The `borg` skill's description is valid YAML now. Its unquoted colon made strict parsers, such as
  Codex's and opencode's, reject the skill. Run `borg update`, or `borg install <harness> --apply`, to
  refresh the installed copies.

## 0.7.0 (2026-10-30)

Hand long-term knowledge over to another store, and Borg now tells you when it or the mesh moves on.

### Added

- **Memory hand-over.**
  - `borg memory export` lists each lesson with its evidence.
  - `borg memory absorb <id>` marks a lesson as held elsewhere, such as a skills repo or a handbook,
    so Borg stops delivering it. If the mistake comes back, Borg counts a recurrence.
  - `borg memory release` undoes an absorb.
- **`borg update`** installs the latest release the way Borg was installed: cargo, or the release
  tarball with its sha256 checked. It then refreshes the harness hooks and runs `borg doctor`.
  `borg update --check` only checks.
- **Update notices.** Borg checks for a new release once a day in the background. Your agent tells
  you once per session and offers to run `borg update`. `BORG_UPDATE_CHECK=off` turns the check off.
- **Mesh announcements.** Nodes exchange their Borg versions. When a node starts using a newer mesh
  protocol, it tells the team once:
  - who runs what;
  - what no longer syncs;
  - how to update.

  `BORG_ANNOUNCE=off` turns this off. `borg status`, `borg doctor`, `borg top` and borg web show
  each member's version.

### Changed

- The mesh protocol is `borg/mesh/15`. Older members keep syncing what they synced before; only
  absorbs need 0.7.

### Fixed

- `--help` on any command prints its usage instead of running it. Before, `borg learn --help` ran a
  learn pass.

### Upgrading

- From 0.6, run `cargo install borg-ai borg-mesh borg-web --locked --force` or
  [`install.sh`](https://github.com/debe/borg_ai/blob/main/scripts/install.sh). From now on,
  `borg update` does it.

## 0.6.0 (2026-10-29)

Messages pace themselves and keep their full text, and lesson selection is measured and learned.

### Added

- **Learned lesson selection** is the default. It learns from outcomes, departs from the fixed rule
  only when the evidence supports it, and falls back for good if it sees harm. `BORG_SELECT=fixed`
  keeps the fixed rule.
  [Evidence](https://github.com/debe/borg_ai/blob/main/docs/evidence.md#lesson-selection)
- **Selection measurements** in `borg status`, `borg top` and borg web. Each check records:
  - what was delivered;
  - how it was judged;
  - its cost and latency.

  `borg debug select --replay` replays the logged checks offline.
- **Message acknowledgements.** A message counts as seen once it is shown. `borg inbox --show <id>`
  prints a long message whole.
- Optional per-collective `risk` and `budget` settings in `collectives.json`.
- [Constants](https://github.com/debe/borg_ai/blob/main/docs/design/constants.md): every numeric
  constant, with its kind and reason.

### Changed

- Messages are paced by acknowledgements instead of one per 10 minutes.
- Messages keep up to 16,384 characters (was 1,000).
- The message log is bounded by size instead of 500 lines.
- The mesh protocol is `borg/mesh/14`. Members on 0.5 still sync.
- The judge no longer sees how a lesson was delivered, so its verdicts compare fairly.
- `borg listen` learns each session's idle times and retries a refused wake with back-off.

### Fixed

- A critical lesson that had already been handed over was dropped. It is now asked.
- Lessons cut by the early-delivery size limit disappeared without a record.
- A queued lesson could be delivered twice.
- omp and opencode could wake a session for messages it had already read.
- `borg reach` kept calling a model that was failing.
- The committed web page contained the build machine's home path.

### Removed

- The fixed message rate and the 500-line message cap.
- The expert drone's $0.50 spend limit. Its time limit stays.

## 0.5.0 (2026-10-28)

The first release on crates.io. Sessions now talk to each other on any harness and across machines.
The hive and playbooks are gone. 0.4.0 was never published; its changes are part of this release.

### Added

- **Messages between sessions:** `borg tell`, `borg ask` and `borg inbox`. A message can go to:
  - a drone;
  - a session on another machine;
  - everyone;
  - the engineer.

  Messages sync with the team.
- **`borg listen`** wakes an idle session when a message arrives.
- `--version` on `borg`, `borg-mesh` and `borg-web`.

### Changed

- Steering is done with messages: `borg pause`, `borg resume`, and `borg no` across machines.
  `borg steer` is removed.
- The mesh protocol is `borg/mesh/13`. 0.3 members sync everything except messages.
- Releases are made with `scripts/release.sh`, as described in
  [CONTRIBUTING.md](https://github.com/debe/borg_ai/blob/main/CONTRIBUTING.md#releasing).

### Removed

- The hive (`borg hive`, goals, boards). Messages between sessions replace it.
- Playbooks, which were never promoted.
- `borg doctor` names any leftover hive or playbook data and the command that removes it.

## 0.3.0 and 0.2.0 (never published)

Development versions, used from source.

0.2.0 built the core:
- lessons learned from corrections and delivered at the prompt, during the turn and at the turn end;
- the queen's questions;
- collectives with peer-to-peer team sync (`borg-mesh`);
- `borg top` and `borg web`;
- hooks for Claude Code, Codex, omp and opencode;
- the hive, which was later removed.

0.3.0 also:
- tidied the command line (evaluation tools moved under `borg debug`);
- marked the library internal;
- prepared the crates.io packaging.
