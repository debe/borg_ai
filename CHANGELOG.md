# Changelog

What changed in each release and what you need to do about it. The details live in the
[docs](https://github.com/debe/borg_ai/tree/main/docs).

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
