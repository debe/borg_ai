# Changelog

What changed in each release and what you need to do about it. The details live in the
[docs](https://github.com/debe/borg_ai/tree/main/docs).

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
