---
name: borg
description: borg is a shared memory for coding agents: lessons from corrections, checked against the work, shared with a team. Use when the engineer mentions borg, a lesson, collective, drone, queen, hive, mesh or vectors, asks why borg blocked or asked, or wants to set up, share or fix borg.
---

# borg

borg is a shared memory for coding agents. Hooks in each harness (Claude Code, omp, opencode, Codex)
capture the engineer's corrections, `borg learn` turns them into lessons, and at the end of a turn
(and, in live mode, during it) a model checks the work against the lessons that apply. At session
end borg also links each landed commit to the earlier decisions whose lines it fixed. Lessons stay
on this machine unless the collective they belong to is synced with a team over the mesh.

## Vocabulary

| Word | Meaning |
|---|---|
| drone | One running agent session wired to borg (this one, if the hooks are installed) |
| collective | A separate memory: one store of lessons, episodes and outcomes. Every repo path routes to one collective, or to none (`off`) |
| lesson | A learned rule: a question to ask, the situations it applies in, and the consequence of ignoring it |
| reach | Who hears a lesson: `team` (everyone in the collective), `repo` (only that repo) or `personal` (only the engineer, in their private collective) |
| queen | The queue of open decisions waiting for the engineer (`borg queen`) |
| mesh | Peer-to-peer sync of a `share: "team"` collective between members' machines (`borg mesh`, a separate `borg-mesh` binary) |
| link | A decision → fix pair: an earlier commit with borg reasoning whose lines a later session's commit changed. A *fix* link (revert, fix-shaped subject, test failed then passed, the engineer named a bug) can become a `regret` lesson; a *revisited* link never does |
| vectors | A local embedding index that finds lessons sharing no word with the work (paraphrases, other languages) |
| episode | A captured correction, veto, regret or review finding, waiting for `borg learn` to turn it into a lesson |
| assimilate | Importing lessons from past sessions or a team's documents (`borg assimilate`) |
| live / hook | The delivery modes: `hook` checks the work at the turn's end; `live` (the default) also checks it during the turn, after state-changing tool calls |
| hive | Several drones working one goal: a board of tasks, each gated by its own check and verified by another drone, planned by a session and approved by the engineer |

## What to run when

| The engineer wants | Run |
|---|---|
| Everything set up in one guided pass | `borg setup` (interactive; run it in the engineer's own terminal) |
| To know what is wired and how big each collective is | `borg status` |
| To know what is broken | `borg doctor`, then [troubleshooting](references/troubleshooting.md) |
| To know which collective a repo belongs to | `borg collective <dir>` |
| To wire or unwire a harness | `borg install <harness\|all>`, `borg uninstall <harness\|all>` |
| To answer borg's open questions | `borg queen`, `borg queen show <n>`, `borg queen answer <n> <text>` |
| The reasoning behind a line or commit | `borg why <path>[:<line>]` or `borg why <commit>` |
| Which later commit fixed a decision, or which decision a fix corrected | `borg why <path>[:<line>]` ("→ fixed in", "← decided in") |
| The decision → fix links borg would make, before learning from them | `borg regress --replay <A..B> --explain` (dry run; [details](references/collectives.md#decision--fix-links)) |
| The engineer says no to a drone's work (a veto: shown lessons lose credit, the reason becomes a lesson) | `borg no "<reason>"`, or `borg no --session <drone> "<reason>"` when several drones work in the repo |
| To redirect or pause running drones | `borg steer <drone\|all> "<text>"`, `borg steer <drone> --pause`, `borg steer <drone> --resume` |
| Vector model and index status | `borg vectors`, `borg vectors --fetch`, `borg vectors --rebuild` |
| Lessons from past sessions | `borg assimilate`, then `borg assimilate --apply --limit 50` |
| Team sync | `borg mesh init`, `borg mesh invite`, `borg mesh join <ticket>`, `borg mesh serve` |
| To have drones build something (a hive goal: tasks, checks, plan approval) | this session plans it: `borg hive init <goal> ... --task ...`, then the engineer's yes ([hive](references/hive.md)) |
| This session to become a drone that waits for hive work | `borg hive join` (every open goal of the repo), `borg hive leave` |
| The board of a goal, or the engineer approving a plan from a terminal | `borg hive board <goal>`, `borg hive ratify <goal>` |
| Every command | `borg help`, `borg mesh --help` |

**Hive.** When the engineer says what the drones should build, this session plans the goal and the
engineer approves it at their own prompt: follow [hive](references/hive.md) step by step.

## Safety rules

1. **Dry run first.** `install`, `uninstall`, `assimilate`, `reach`, `mesh join` and `queen dedupe` print
   what they would change without `--apply`. Run the dry run, show the engineer the diff or plan, and
   add `--apply` only after they agree. `--apply` backs up every file it changes. `borg setup` asks
   before it writes, but `borg setup --yes` does not: from a drone, tell the engineer what the flags
   will set up and get a yes first.
2. **collectives.json is the engineer's.** Show the full new file and the diff before writing it, keep
   every key you were not asked to change, and run `borg doctor` afterwards: an invalid file routes
   every repo off. Details: [collectives](references/collectives.md).
3. **`share: "local"` never syncs.** Put private and client work in a `local` collective or under
   `off`. Never move a repo into a `team` collective without the engineer saying so: its lessons then
   reach everyone in that team.
4. **Learning from fixes is the engineer's call.** `BORG_REGRESS` defaults to `links`: nothing is
   learned. Before suggesting `BORG_REGRESS=learn`, run `borg regress --replay <A..B> --explain` on
   their history and `borg regress --session ID --explain` on a few real sessions, and let them check
   the fix links: their precision on real repos is not measured yet.
5. **Ask before calling a model in bulk.** `borg assimilate --apply`, `borg assimilate --from-dir` and
   `borg reach` spend a model call per item; say roughly how many before running them.

## Setting up

- **One engineer:** `borg setup`, or by hand: write collectives.json, `borg install all` (dry run),
  `borg install all --apply`, `borg vectors --fetch`, `borg doctor`.
- **A team or company:** a `share: "team"` collective plus the mesh. One admin runs `borg mesh init`
  and keeps `borg mesh serve` running; everyone else joins with an invite ticket. Relays, a
  self-hosted `iroh-relay`, LAN-only mode and corporate networks: [team](references/team.md).
- **Vectors** are on by default once the model is fetched: [vectors](references/vectors.md).
- `borg install` writes the hook files, the adapters and this skill; rerun it rather than editing them
  (codex asks to trust its hooks again in `/hooks` whenever they change).
- Every command and flag: [commands](https://github.com/debe/borg_ai/blob/main/docs/commands.md).

## References

- [references/collectives.md](references/collectives.md): the collectives.json schema, routing rules,
  worked examples (private, team, company with off paths and experts).
- [references/team.md](references/team.md): admin and joiner steps, relays, proxies, mesh problems.
- [references/vectors.md](references/vectors.md): fetch, rebuild, mirrors, turning them off.
- [references/troubleshooting.md](references/troubleshooting.md): `borg doctor` lines and their fixes.
- [references/hive.md](references/hive.md): planning a hive goal (tasks, contracts, checks), the
  engineer's approval at their prompt, the budget.
