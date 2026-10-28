# Commands

[← README](../README.md)

## Overview

The commands of borg, grouped as `borg help` shows them, and evaluation and debugging tools under
`borg debug` ([appendix](#borg-debug)).

### Setup

| Command | Shows or does |
|---|---|
| `borg setup [--yes] [--no-vectors] [--harness LIST] [--private DIR...] [--team NAME --paths DIR... [--relay URL\|disabled] \| --join TICKET [--paths DIR...]] [--home DIR]` | Guided setup: collectives (personal, team or company, off paths), the team's mesh, the vector model, wiring, an optional import, then `borg doctor`. Shows one plan and writes nothing until you confirm; `--yes` takes every default. An existing `collectives.json` is only added to; a second run changes nothing ([Quick start](../README.md#quick-start)) |
| `borg install\|uninstall <claude\|omp\|opencode\|codex\|all> [--apply]` | Wire or unwire the hooks or adapter and the `borg` skill; a dry run without `--apply` ([details](configuration.md#what-borg-install-writes)) |
| `borg status` | Each collective's delivery mode and store (lessons, episodes, outcomes, checks), what is wired and whether the `borg` skill is current |
| `borg doctor` | What is broken: config, hooks, adapters, the model CLI, `borg` on `PATH`, a stale skill, the vector model and index, codex hooks it would skip as untrusted, and mesh members on another protocol version |
| `borg collective [DIR]` | Which collective a directory belongs to and whether it may sync |
| `borg vectors [--fetch\|--rebuild] [--store FILE]` | The vector model and index; on by default once the model is fetched ([details](configuration.md#vectors)) |
| `borg completions <bash\|zsh\|fish>` | A shell completion script (below) |

### Learning

| Command | Shows or does |
|---|---|
| `borg learn` | Turn waiting episodes into lessons now (normally at session end). It also counts each learned regret against the lessons asked before its decision |
| `borg no [--session ID\|DRONE] [--dir DIR] [--json] <reason>` | Say no to a drone's work: the lessons it was shown lose credit, the reason becomes a lesson ([Saying no](#saying-no)) |
| `borg assimilate ...` | Import past Claude Code, omp and opencode sessions, or team documents with `--from-dir` ([details](configuration.md#import-what-you-already-have)) |
| `borg reach [--plan FILE] [--apply] [--jobs N]` | Sort existing lessons into team, repo-only and personal ([details](configuration.md#who-hears-a-lesson)) |
| `borg facets [--apply] [--store FILE] [--jobs N]` | Search facets for lessons |

### Decisions

| Command | Shows or does |
|---|---|
| `borg queen ...` | Open decisions for the engineer ([Answering the queen](#answering-the-queen)) |
| `borg why <path>[:<line>[-<line>]] \| <commit> [--full] [--json]` | The reasoning behind code: blame, borg notes, linked lessons, and both ends of a decision → fix link ("→ fixed in Y" on the decision, "← decided in X" on the fix) |
| `borg notes fetch [<commit>...]` | Reasoning notes and decision → fix links from team members over the mesh: for the named commits, or the last 200 on HEAD |
| `borg regress [--session ID] [--replay A..B] [--any-origin] [--explain] [--json] [--dir DIR]` | The decision → fix links borg would make, as a dry run that writes nothing; `--explain` lists each candidate left out and why ([details](configuration.md#decision--fix-links)) |
| `borg review-fix [--home DIR] [--since YYYY-MM-DD] [--json] [--apply [--only ID,…]]` | Bugs a sub-agent review found and a fix step fixed before any commit, per collective, with a chance-match control; 0 tokens. `--apply` learns the strong ones ([details](configuration.md#review--fix-findings)) |
| `borg trust [--dir DIR]` | Trust per engineer and area, from what followed their lessons |

### Watching

| Command | Shows or does |
|---|---|
| `borg top [--once] [--json] [--dir DIR]` | A live screen, like Unix `top`: the drones of each collective (local and remote). Quits on `q`, Esc or Ctrl-C. Piped, or with `--once`, it prints the plain text once |
| `borg web [--port PORT] [--dir DIR]` | A live page on 127.0.0.1: knowledge, checks, the queen, learning week by week and cost ([details](statistics.md)) |

### Team

| Command | Shows or does |
|---|---|
| `borg mesh <command>` | Team sync: `id`, `init`, `invite`, `invites`, `join`, `revoke`, `members`, `peer`, `serve`, `sync` ([details](team-sync.md)) |
| `borg steer <drone\|session\|all> "<text>" \| --pause \| --resume` | A signed instruction to running drones |

**Shell completion:**

```sh
borg completions bash > ~/.local/share/bash-completion/completions/borg
borg completions zsh > "${fpath[1]}/_borg"
borg completions fish > ~/.config/fish/completions/borg.fish
```

**Internal, not in `borg help`:** `borg hook <harness> <event>` is what the harnesses run.
`borg check --session ID [--transcript FILE] --cwd DIR` is live mode's check of the work so far, started
detached after a state-changing tool call ([Delivery modes](delivery.md#delivery-modes)).
`borg learn --session ID` and `borg notes attach [<commit>]` run from hooks and adapters at session end.

## Playbooks

A playbook is the team's procedure for a kind of task: its shape ("fix the parser") and a short list of
steps. `playbooks.jsonl` holds edits (add, change or remove one step), never whole playbooks, so it syncs
like lessons and every reader folds the same playbooks.

```console
$ borg queen
app (1)
  1  Add to the team's playbook: "Run the parser's tests before calling it done"?
$ borg queen answer 1 b
```

After a session ends clean (it landed commits, its last test run passed, and you neither corrected nor
vetoed it), `borg learn` asks the model once for at most one edit. The edit is never applied by
learning: it is queued as a queen question, and only your `promote` answer appends it. A session sees
the playbooks its prompt names; each version shown is credited like a lesson. `BORG_PLAYBOOKS=off`
shows and proposes nothing.

## Saying no

A veto is the engineer's "no" to what a drone is doing, with the reason: the strongest negative signal
borg learns from, at 0 tokens.

```console
$ borg no never look merges up in the commit log, use the review API
vetoed One of Two (3f9c2a1b): 2 shown lesson(s) lose credit (L17, L42); the reason becomes a lesson when borg learn runs
to stop it now: borg steer 3f9c2a1b --pause
```

**Which drone.** Without `--session`, the one local drone working in the current repo (or `--dir`), or,
with none running, the most recently seen one. With several running it lists them; name one with
`--session` (as `borg top` shows it, its number, a session id or a prefix of 4+ characters). A drone on
another member's machine is named with `--session`, and the veto travels as a signed steering directive
(it needs the `admin` or `steer` role); that machine records it once.

**Inside the session**, a prompt that starts with `borg no` is the same veto. The drone stops the vetoed
approach and may ask one clarifying question. `BORG_VETO=off` turns this off; the command still works.

What it does:

1. **Credit.** Every lesson shown in the session so far gets a `vetoed` outcome, weighed three times
   (a correction weighs two), once per lesson and session.
2. **A lesson.** The reason becomes a veto episode that `borg learn` turns into a lesson; the same no
   said twice counts as `veto_repeat`.
3. **A proposed constraint.** A reason that names an invariant is queued in `borg queen`; only your
   answer ratifies it into `<store>/constraints.jsonl`, which syncs to the team and is delivered early
   to every session.
4. **A missed escalation.** A veto of merged work from a session the queen never escalated counts
   against the queen's attention ([below](#answering-the-queen)).

With `BORG_LEARN=off` the command records nothing and says so.

## Answering the queen

The queen's queue holds decisions no lesson answers: open decisions found in team documents
(`borg assimilate --from-dir`), proposed constraints, playbook edits and, with
`BORG_QUEEN=on`, questions raised at a turn end. Similar questions are queued once.

```console
$ borg queen
handbook (1)
  1  Should spikes be capped at 3 story points, or estimated like any other story?
     a) cap spikes at 3 points (default) · b) estimate spikes like any other story
$ borg queen answer 1 b
```

| Command | Effect |
|---|---|
| `borg queen [--json] [--repo NAME]` | Open questions, numbered and grouped by repo (`--json`: an array, for drones and scripts) |
| `borg queen show <n\|id>` | One question with its evidence |
| `borg queen ask` | Answer them one by one (a terminal) |
| `borg queen answer <n\|id> <text\|a\|b\|1\|2>` | Answer one; the answer becomes a lesson. A letter or number picks an option |
| `borg queen skip <n\|id>` | Not delivered again for 3 days |
| `borg queen dismiss <n\|id> [reason]` | Drop it without a lesson |
| `borg queen dedupe [--apply]` | Dismiss later near-duplicates (a dry run first) |

Each takes `--dir DIR` or `--collective NAME`.

**In the harness.** At the first prompt of a session in a repo with an open question, borg hands that
one question to the drone, which asks you (Claude Code in its question dialog, omp and opencode at the
start of the reply) and records your answer with `borg queen answer`. One question per session; a
skipped one waits 3 days. `BORG_QUEEN_ASK=off` or `BORG_LEARN=off` turns this off.

**The queen's attention.** Only the questions the queen chooses to raise count (an external mutation,
an irreversible change, a public API or schema change, a cross-repo decision):

| Event | Counts |
|---|---|
| An escalation you answered by changing course | +1 (`queen_no`) |
| An escalation you answered "yes, fine" | −1 (`queen_stamp`) |
| A veto of merged work, or a later fix of a decision, from a session the queen never escalated | −3 (`queen_missed`) |

The page's queen view shows the score for the team. The weights are design choices, not tuned, and
nothing optimises the score yet.

## borg debug

Evaluation and debugging tools, out of the main help (`borg debug help`):

| Command | Shows or does |
|---|---|
| `borg debug recall <lessons.jsonl> [before_ms]` | Rank lessons for text on stdin |
| `borg debug resolve <lessons.jsonl>` | The word rule's changed/ignored for `{asked, before, edits, final}` on stdin |
| `borg debug prefilter <lessons.jsonl> [before_ms]` | The free gate's score for a turn on stdin |
| `borg debug memory <lessons.jsonl> [--session ID]` | Each lesson's activation, usefulness, weight and long-term or dormant state |
| `borg debug expert <repo> --at <commit> --q <text>` | At most 3 cited claims from an expert repo |
| `borg debug vectors --query [--store LESSONS]` | The vector shortlist for a context on stdin |
| `borg debug ope [--dir DIR \| --file FILE] [--json]` | Off-policy estimates of the belief's hand-set weights; reports, never changes them |
