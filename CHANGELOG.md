# Changelog

## 0.1.0 (unreleased)

First release.

### Learning

- Captures corrections as episodes: correcting prompts, tool calls rejected with a reason, and the next
  prompt after a wordless denial (omp).
- `borg learn` (run at session end) turns episodes into lessons: question, situations, consequence,
  optional scope. A new correction on the same point updates the existing lesson.
- `borg assimilate` imports corrections from past Claude Code and omp transcripts (dry run by default).
- `borg assimilate --from-dir` imports a team's written knowledge as scoped lessons and open decisions,
  with a reviewable plan file.
- `BORG_ASSIMILATE_SKIP` keeps episodes and documents that mention given words out of the import.

### Delivery

- Early delivery: the first prompt or file touch that names a lesson's topic gets that topic's top lessons
  as context, without a model call (Claude Code).
- Turn-end check: a free word-matching gate, then one graded model call over every eligible lesson.
  Only a grade-3 lesson blocks the stop for another round. Grade-2 and grade-1 lessons go free with the
  next prompt or touch (Claude Code).
- System gate: lessons scoped to other systems are not delivered in a repo that maps to a known system.
  The system map is read from `~/.config/borg/systems.json` (`BORG_SYSTEMS`); a neutral example ships.
- Optional vector recall at the turn end (`--features vectors`, `borg vectors --fetch/--rebuild`).
- Expert repos per collective: cited claims from a local checkout of a dependency, at the pinned version.

### Memory

- Usefulness judged by meaning within the same turn-end call. Activation strengthens with use and fades
  with sessions; unused lessons go dormant and are never deleted.
- Trust per engineer and area (`borg trust`).

### Collectives and team

- Collectives per repo path: team, private (`share: "local"`) or off; an invalid config routes everything off.
- `borg-mesh`: peer-to-peer sync of a team collective's lessons with signed membership, no server.
- Mesh across NAT: peers are dialled by key, with iroh hole punching and relay fallback. The relay is
  set per collective (`relay`: n0's public relays by default, a self-hosted URL, or `disabled`).
- Onboarding by ticket: `borg mesh invite` prints a signed, expiring, single-use ticket and its QR
  code; `borg mesh join <ticket> [--apply]` adds the collective to the config (dry run first),
  joins and syncs.
- `borg top` and `borg steer`: see and steer running drones, locally and across the mesh.
- Reasoning notes: redacted excerpts of a session's commits as git notes (`borg notes`, `borg why`).

### Harnesses and operations

- Claude Code hooks; omp and opencode adapters (turn-end check, learning, steering).
- `borg install|uninstall` with dry run, backups and idempotent merging; `borg status`, `borg doctor`.
- Hooks fail open; the turn-end model call has a 45 s limit.

### Off by default (experimental)

- The queen (`BORG_QUEEN=on`), engineering concepts (`BORG_CONCEPTS=on`), facets (`BORG_FACETS=on`).
