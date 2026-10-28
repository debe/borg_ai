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
| `borg version` | borg's version, one line (also `borg --version` and `borg -V`; `borg mesh --version` and `borg web --version` print theirs) |

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
| `borg top [--once] [--json] [--dir DIR]` | A live screen, like Unix `top`: the drones of each collective (local and remote), each with its state (`paused` while paused), whether it listens (WAKE), its unread messages (UNREAD), and the last day's messages below. Quits on `q`, Esc or Ctrl-C. Piped, or with `--once`, it prints the plain text once |
| `borg web [--port PORT] [--dir DIR]` | A live page on 127.0.0.1: an overview, knowledge, checks (Conscience), learning week by week, steering and refusals, messages, the queen, cost and drones ([details](statistics.md)) |

### Team

| Command | Shows or does |
|---|---|
| `borg mesh <command>` | Team sync: `id`, `init`, `invite`, `invites`, `join`, `revoke`, `members`, `peer`, `serve`, `sync`, `fetch` ([details](team-sync.md)) |
| `borg tell <drone\|session\|local\|collective\|engineer> "<text>" [--re ID] [--from SESSION] [--dir DIR]` | A message to other sessions, on any harness: `local` is this machine's sessions, `collective` (or `all`) every session of the collective. Shown once at the recipient's next prompt or tool call ([Messages](#messages)) |
| `borg ask <drone\|session\|local\|collective\|engineer> "<question>" [--re ID] [--from SESSION] [--dir DIR]` | As `tell`, for a message that asks for a reply |
| `borg inbox [--all] [--json] [--from SESSION] [--dir DIR]` | In a session: its messages, new or shown, then marks them shown. In a terminal, or with `--all`: every message of the last 7 days and who it was shown to |
| `borg pause <drone\|session\|local\|collective> ["<note>"] [--from SESSION] [--dir DIR] [--json]` | Refuses a session's tool calls, with the note, until it is resumed ([Pause and resume](#pause-and-resume)) |
| `borg resume <drone\|session\|local\|collective> ["<note>"] [--from SESSION] [--dir DIR] [--json]` | Lets a paused session's tool calls run again |
| `borg listen [--off] [--session S] [--dir DIR]` | Wakes this session when a message arrives while it is idle, until `--off` or the session's end ([Waking a session](#waking-a-session)) |

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

## Messages

`borg tell` and `borg ask` send from the calling session, or from the engineer in a plain terminal
(`engineer:<git user.email>`). The recipient is one of:

- a drone as `borg top` names it, or a session id prefix, resolved to the session id and its machine when
  the message is sent. This machine comes first: a prefix that matches a session here and one on a
  member's machine names the one here. A member's session is reached when nothing here matches, or by
  `<session>@<node>`, which names both;
- `local`: the sessions running on this machine then. Its line never leaves this machine;
- `collective`, or its synonym `all`: every session of the collective running then, on every member's
  machine. It reaches, and may wake, sessions on colleagues' machines, so it is for what concerns their
  work;
- `engineer`, read with `borg inbox`.

A reply goes to the sender's session on the machine its message came from, even when the session has
ended here. A sender's name and session are its own claims; `borg top` and borg web show a message from
another machine with that machine's short key, the part the mesh checks.

`collective` and `all` are written `all` in the log, as 0.4.0 wrote `all`, so a line from 0.4.0 or an
older peer means the same. `local` is written `local@<node>` and counts only on a line from that node:
no machine broadcasts into another's sessions. 0.4.0 reads `local@<node>` as a session named `local`,
which no session is, so it reaches nobody there.

The recipient's hook shows each message once, at its next prompt or tool call, ahead of any lessons. It
is marked as peer data, never the user's words, with the line that replies:
`borg tell <session> --re <id> "…"`. If something else blocks that call, the message waits for the next
one. One prompt or tool call shows at most 10 messages and 4,000 characters of them, newest first; the
rest come at the next one. opencode gives its shell no session id, so its reply line carries
`--from <session>`.

Text is redacted, put on one line and cut to 1,000 characters. One message per sender and recipient per
10 minutes; a repeat is refused. Messages live in `<store>/messages.jsonl` for 7 days, at most the 500
newest. Those to `collective`, to a session on another machine and replies sync with the team collective
like lessons ([Team sync](team-sync.md#how-a-sync-works)); one to `local` stays on this machine.
An idle session sees a message when it next runs, unless it listens ([Waking a session](#waking-a-session)).

**Seen in monitoring.** `borg top` shows each drone's unread messages and the last day's messages, a
broadcast as `local` or `collective` (`borg inbox` and borg web's Messages page say it so too);
borg web's Messages page counts messages sent, shown and dropped (a rate-limit refusal or the 500 cap).

### Pause and resume

`borg pause` and `borg resume` are messages of their own kind; a veto from `borg no` to a drone on
another machine is one too. They go to a drone, a session, `local` (this machine's sessions) or
`collective` (or `all`: every member's, each machine admitting it by the sender's role), never to `engineer`; a pause or
resume may have no text. A veto names exactly one session on one machine (`<session>@<node>`). They are not
rate limited, so a resume can follow a pause at once.

They count only when admitted. This machine's own are admitted when sent: the engineer here steers
their own sessions. Another machine's are admitted by the mesh when a sync brings them in and the member list
gives the sender the `admin` or `steer` role; a `share: local` collective admits none. A message without an
admission is neither shown nor in force.

Every harness shows them at the next prompt or tool call: each veto once, and of a session's pauses and
resumes only the latest. While the latest is a pause, the session's tool calls are refused with its note.
Its stop is not blocked and the conscience does not run. A pause lapses after 24 hours. Anything borg
cannot read counts as not paused. `borg inbox` lists tells and asks only; `borg top` shows a paused
drone as `paused`, and borg web's Steering page counts pauses and vetoes issued, shown and refused,
and the tool calls a pause refused.

### Waking a session

`borg listen`, run inside a session, opts it in. The opt-in lives in `<store>/listen/<session>.json`,
is never synced, and ends with `borg listen --off`, the session's end or the prune of its presence
record. A plain session is never woken. Without `--session`, the session comes from the harness's
variables, or on opencode from `OPENCODE_PID`.

When a listening session's turn ends without being blocked, its stop starts a waiter in the way the
harness allows:

| Harness | How the waiter runs | How the wake arrives |
|---|---|---|
| Claude Code | The stop asks the model, once a turn, to run `borg listen --wait` in the background | The waiter exits with a summary, and Claude Code starts the next turn with it |
| omp, opencode | The stop's answer carries `"listen": true`; the adapter keeps `borg listen --stream` running for the session's life | One JSON line per wake (`{"wake": "<text>"}`), sent in as a new turn: omp `pi.sendMessage`, opencode 1.x `session.promptAsync`, 2.x `session.prompt` with `delivery: "queue"`. A wake that arrives during a turn waits for its end, and is dropped if that turn already showed its messages |
| Codex CLI | The stop starts `borg listen --queue` detached, logging to `<store>/listen/<session>.log` | `codex queue --thread <session> --message <text>`, 30 s at most; a failure waits 10 s, doubling up to 5 minutes, before the same messages are tried again |

The wake text names the number of messages, their senders and kinds, never their text. The session's
next prompt hook shows the messages themselves, once. Each message wakes a session once; a wake it never
took leaves the message unread, and the next message wakes it again. While the engineer's own turn runs,
from its prompt to the end of its stop, nothing wakes the session; that turn's hooks show what arrives.
A turn that never reached its stop (an Esc) stops holding wakes off 5 minutes after its last event (a
minute on Codex CLI), or 10 minutes after a tool call that never ended. A wake's own turn holds the next
wake off the same way, until its stop. A paused session is not woken, nor asked for a wait, until it is
resumed.

There is one waiter per session. It follows a reinstall of borg in place and ends on SIGINT, SIGHUP or
SIGTERM, on `borg listen --off`, or once the session is gone. Exit codes: 0 after a wake (`--wait`),
after `--off` or when the session is gone; 1 when the session does not listen; 2 on bad arguments;
128 + the signal when stopped. `borg top` shows `on` (listens, no waiter yet) or `waiter` in its WAKE
column, and the same in `borg top --json` as each session's `listen`.

## Saying no

A veto is the engineer's "no" to what a drone is doing, with the reason: the strongest negative signal
borg learns from, at 0 tokens.

```console
$ borg no never look merges up in the commit log, use the review API
vetoed One of Two (3f9c2a1b): 2 shown lesson(s) lose credit (L17, L42); the reason becomes a lesson when borg learn runs
to stop it now: borg pause 3f9c2a1b
```

**Which drone.** Without `--session`, the one local drone working in the current repo (or `--dir`), or,
with none running, the most recently seen one. With several running it lists them; name one with
`--session` (as `borg top` shows it, its number, a session id or a prefix of 4+ characters). A drone on
another member's machine is named with `--session`, and the veto travels as a message to
`<session>@<node>` ([Pause and resume](#pause-and-resume)); it counts there only if you have the
`admin` or `steer` role, and that machine records it once.

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
(`borg assimilate --from-dir`), who should hear an existing lesson (`borg reach`), proposed
constraints and, with `BORG_QUEEN=on`, questions raised at a turn end. Similar questions are queued once.

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
| `borg queen answer <n\|id> <text\|a\|b\|1\|2>` | Answer one; the answer becomes a lesson (a reach answer moves the lesson instead). A letter or number picks an option |
| `borg queen skip <n\|id>` | Not delivered again for 3 days |
| `borg queen dismiss <n\|id> [reason]` | Drop it without a lesson |
| `borg queen dedupe [--apply]` | Dismiss later near-duplicates (a dry run first) |

Each takes `--dir DIR` or `--collective NAME`.

**In the harness.** At the first prompt of a session in a repo with an open question, borg hands that
one question to the drone, which asks you (Claude Code in its question dialog, omp, opencode and Codex at
the start of the reply) and records your answer with `borg queen answer`. One question per session; a
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
