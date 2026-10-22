# Commands

[← README](../README.md)

## Overview

| Command | Shows or does |
|---|---|
| `borg status` | Each collective's delivery mode and store (lessons, episodes, outcomes, turn-end and live checks), what is wired and whether the `borg` skill is current |
| `borg doctor` | What is broken: config (an unknown `mode` included), hooks, adapters, the model CLI (claude, omp or opencode, or a pinned `codex:<model>`), `borg` on `PATH`, a missing or stale `borg` skill, the vector model and each store's index, and which of borg's hooks codex would skip as untrusted (asked of codex itself, `codex app-server`) |
| `borg collective [DIR]` | Which collective a directory belongs to and whether it may sync |
| `borg setup [--yes] [--no-vectors] [--harness LIST] [--private DIR...] [--team NAME --paths DIR... [--relay URL\|disabled] \| --join TICKET [--paths DIR...]] [--home DIR]` | Guided setup: collectives (personal, team or company, off paths), the team's mesh, the vector model, wiring, an optional import, then `borg doctor`. Shows one plan and writes nothing until you confirm; `--yes` takes every default without asking. Flags answer their question. An existing `collectives.json` is only added to. A second run changes nothing ([Quick start](../README.md#quick-start)) |
| `borg install\|uninstall <claude\|omp\|opencode\|codex\|all> [--apply]` | Wire or unwire the hooks or adapter and the `borg` skill; a dry run without `--apply` ([details](configuration.md#what-borg-install-writes)) |
| `borg assimilate ...` | Import past Claude Code, omp and opencode sessions (opencode 2: known projects only, not the sessions it migrated from 1.x), or team documents ([details](configuration.md#import-what-you-already-have)) |
| `borg reach [--plan FILE] [--apply]` | Sort existing lessons into team, repo-only and personal ([details](configuration.md#who-hears-a-lesson)) |
| `borg learn [--session ID]` | Turn waiting episodes into lessons now (normally at session end) |
| `borg memory <lessons.jsonl>` | Each lesson's activation, usefulness, weight, and long-term or dormant state |
| `borg vectors [--fetch\|--rebuild] [--store FILE]` | Vector model and index; on by default once the model is fetched ([details](configuration.md#vectors)) |
| `borg queen ...` | Open decisions for the engineer; see [below](#answering-the-queen) |
| `borg web [--port PORT] [--dir DIR]` | A live page of the collective's knowledge and statistics on 127.0.0.1 ([details](statistics.md)) |
| `borg top [--watch] [--json]` | The running drones of each collective, local and remote |
| `borg steer <drone\|session\|all> "<text>"` | Send a signed instruction to running drones; `--pause`, `--resume` |
| `borg why <path>[:<line>[-<line>]] \| <commit> [--full] [--json]` | The reasoning behind code: blame, Borg notes, linked lessons, and both ends of a decision → fix link: "→ fixed in Y (task)" on the decision, "← decided in X (task) → fixed here" on the fix (three each, all with `--full`; `--json` adds `fixed_in` and `fixes`) |
| `borg regress [--session ID] [--replay A..B] [--any-origin] [--explain] [--json] [--dir DIR]` | The decision → fix links Borg would make, as a dry run that writes nothing: for the latest session in this repo (or `--session`), or every commit of a range (`--replay`, at most 500). `--explain` lists each candidate left out and why (`format`, `move`, `too-large`, `no-reasoning`, `same-session`, `too-young`, `duplicate`, `cap`, ...); `--any-origin` also takes origins without Borg reasoning, to try the detector on a repo with no notes ([details](configuration.md#decision--fix-links)) |
| `borg review-fix [--home DIR] [--since YYYY-MM-DD] [--json] [--apply [--only ID,…]]` | The bugs a sub-agent review found and a fix sub-agent fixed before any commit (Claude Code workflows, omp tasks, opencode subagents), per collective, with a chance-match control; 0 tokens. `--apply` records the strong ones as episodes and learns them ([details](configuration.md#review--fix-findings)) |
| `borg notes attach [<commit>]` | Write the redacted reasoning note for a session's commits, then link them to the earlier decisions they fixed (normally at session end) |
| `borg notes fetch [<commit>...]` | Get reasoning notes and decision → fix links from team members over the mesh: for the named commits, or the last 200 on HEAD; a link comes with either of its commits |
| `borg trust [--dir DIR]` | Trust per engineer and area, from what happened after their lessons were asked |
| `borg mesh <command>` | Team sync: `id`, `init`, `invite`, `invites`, `join` (`--paths DIR...` names your repos), `revoke`, `members`, `peer`, `serve`, `sync` ([details](team-sync.md)) |

**Shell completion:** `borg completions <bash|zsh|fish>` prints a completion script.

```sh
borg completions bash > ~/.local/share/bash-completion/completions/borg
borg completions zsh > "${fpath[1]}/_borg"
borg completions fish > ~/.config/fish/completions/borg.fish
```

For debugging and evaluation: `borg recall`, `borg prefilter`, `borg resolve`, `borg expert`,
`borg facets`. Their usage is in the header of [`crates/borg/src/main.rs`](../crates/borg/src/main.rs).

**Internal:** `borg check --session ID [--transcript FILE] --cwd DIR` is live mode's check of the work
so far ([Delivery modes](../README.md#delivery-modes)). Every harness's post-tool event (Claude Code and
omp PostToolUse, opencode `tool.execute.after`) starts it detached after a state-changing tool call, with
the hook's environment. Without a transcript (omp, opencode) the work so far is the turn's tool calls
from `<state>/<session>.turn`. It takes the session's lock
(`<state>/<session>.live.lock`), runs one graded model call on the turn so far, and leaves its verdict
in the session's `.queued` file for the next prompt or tool call. It is not meant to be run by hand and
is not listed in `borg help`.

## Answering the queen

The queen's queue holds decisions that no lesson answers: open decisions found in team documents
(`borg assimilate --from-dir`) and, with `BORG_QUEEN=on`, questions raised at a turn end. A desktop
notification announces new ones, and similar questions are queued once.

**In the terminal:**

```console
$ borg queen
handbook (1)
  1  Should spikes be capped at 3 story points, or estimated like any other story?
     a) cap spikes at 3 points (default) · b) estimate spikes like any other story
answer: borg queen ask · borg queen answer <n> <text|a|b> · borg queen show <n>
$ borg queen answer 1 b
```

| Command | Effect |
|---|---|
| `borg queen` | Open questions, numbered and grouped by repo |
| `borg queen ask` | Answer them one by one |
| `borg queen answer <n\|id> <text\|a\|b\|1\|2>` | Answer one; the answer becomes a lesson. A letter or number picks an option; one that names no option is refused |
| `borg queen show <n\|id>` | One question with its evidence |
| `borg queen skip <n\|id>` | Ask again in 3 days |
| `borg queen dismiss <n\|id> [reason]` | Drop it without a lesson |
| `borg queen --json` · `borg queen next --json [--repo NAME]` | All open questions, or the one to ask now, for drones and scripts |
| `borg queen dedupe [--apply]` | Dismiss later near-duplicates (dry run first) |
| `borg queen notify` | One desktop summary of the open questions |

**In the harness.** At the first prompt of a session in a repo with an open question, Borg hands that
question to the drone. Claude Code asks it in its question dialog; omp and opencode ask it at the start
of the reply. Pick an option, answer in your own words or skip. The drone records the answer with
`borg queen answer`, then does what you asked.

- One question per session, and only one of the session's own repo.
- A question belongs to its git repo, whichever subdirectory or worktree raised it.
- A skipped question waits 3 days; an unanswered one returns after 3 more sessions.
- `BORG_QUEEN_ASK=off` or `BORG_LEARN=off` turns this off.
