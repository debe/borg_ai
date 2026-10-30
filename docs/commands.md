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
| `borg status` | Each collective's delivery mode and store (lessons, absorbed lessons and how many recurred, episodes, outcomes, checks), how lessons are selected (the policy and the default, what the checks measure for the learned policy, spend per session, stop latency per harness, the learned decisions a guard gave the fixed rule, per reason, and under the learned policy its gate and the monitor's reading; [Which lessons are asked](delivery.md#which-lessons-are-asked)), a `version` block (this borg, the latest release and when it was checked, and the members of each team collective by version, an older one with what it misses), what is wired and whether the `borg` skill is current |
| `borg doctor` | What is broken: config, hooks, adapters, the model CLI, `borg` on `PATH`, a stale skill, the vector model and index, codex hooks it would skip as untrusted, mesh members on another protocol version (a member on an older one with what it misses, one on a newer borg as ok), a newer release, and a release check failing for more than 3 days |
| `borg collective [DIR]` | Which collective a directory belongs to and whether it may sync |
| `borg vectors [--fetch\|--rebuild] [--store FILE]` | The vector model and index; on by default once the model is fetched ([details](configuration.md#vectors)) |
| `borg completions <bash\|zsh\|fish>` | A shell completion script (below) |
| `borg version` | borg's version, one line (also `borg --version` and `borg -V`; `borg mesh --version` and `borg web --version` print theirs) |
| `borg update` | Installs the latest release when it is newer than this borg, then refreshes what is wired (`borg install <h> --apply` for each harness wired now, never a new one) and runs `borg doctor`; exits with doctor's code. How it installs follows where `borg` lives: under `$CARGO_HOME/bin` (default `~/.cargo/bin`) `cargo install borg-mesh`, `borg-web` and then `borg-ai` `--locked --force --version X`; elsewhere on Linux x86_64/arm64 (glibc) or macOS on Apple silicon the release tarball and its `.sha256`, as `install.sh` downloads them, checked and renamed over `borg-mesh`, `borg-web` and then `borg` beside it (a `BORG_BIN` symlink counts where it points; a temporary directory an interrupted install left there is removed). `borg` comes last, so a stopped install is started again by the next `borg update`; on the current release it reinstalls a `borg-mesh` or `borg-web` beside `borg` that reports another version. A build under `target/` or another platform prints the commands and changes nothing (exit 1); so does a failed check or download. The mesh daemon and `borg listen` waiters restart on their own; it names a running `borg web` to restart ([Upgrade](../README.md#upgrade-and-uninstall)) |
| `borg update --check [--quiet] [--json]` | Asks the release feed now and prints this borg, the latest release and when it was checked, and the members of each team collective by version, an older one with what it misses ([Versions on the mesh](team-sync.md#how-a-sync-works)). Exits 0 when this borg is current, 10 when a newer release exists, 1 when the check failed. `--quiet` prints nothing (the prompt hook runs it in the background once a day); `--json`: `version`, `latest`, `checked_ms`, `error`, `update` and `members` (per collective, each member key's `borg`, `protocol`, `ms`, `engineer`). `BORG_UPDATE_CHECK=off` turns the check off ([variables](configuration.md#environment-variables)) |

### Learning

| Command | Shows or does |
|---|---|
| `borg learn` | Turn waiting episodes into lessons now (normally at session end). It also counts each learned regret against the lessons asked before its decision |
| `borg no [--session ID\|DRONE] [--dir DIR] [--json] <reason>` | Say no to a drone's work: the lessons it was shown lose credit, the reason becomes a lesson ([Saying no](#saying-no)) |
| `borg assimilate ...` | Import past Claude Code, omp and opencode sessions, or team documents with `--from-dir` ([details](configuration.md#import-what-you-already-have)) |
| `borg reach [--plan FILE] [--apply] [--jobs N]` | Sort existing lessons into team, repo-only and personal ([details](configuration.md#who-hears-a-lesson)) |
| `borg facets [--apply] [--store FILE] [--jobs N]` | Search facets for lessons |
| `borg memory export [--json] [--dir DIR] [--absorbed] [--since YYYY-MM-DD]` | Each lesson's evidence, for a store outside borg to take over long-term knowledge: the lesson, its belief with a 95% credible interval, its breadth, raw placebo and tail counts, its absorb and recurrence. Read-only, 0 tokens; an `off` directory prints nothing. `--json`: schema 1. `--absorbed`: absorbed lessons only; `--since`: lessons changed on or after that day ([details](learning.md#handing-a-lesson-to-an-outside-store)) |
| `borg memory absorb <id>... [--note TEXT] [--dir DIR] [--json]` | The lessons are held outside borg now: borg stops delivering them on every path. The note is an opaque label, at most 1,000 characters. An id not in the store yet applies when the lesson arrives. Exits 2 on a usage error or a longer note, 1 in an `off` directory |
| `borg memory release <id>... [--dir DIR] [--json]` | Undoes an absorb: borg delivers the lessons again |

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
| `borg top [--once] [--json] [--dir DIR]` | A live screen, like Unix `top`: the drones of each collective (local and remote), each with its state (`paused` while paused), whether it listens (WAKE), its unread messages (UNREAD), the last day's messages below, and a `selection:` line once the collective has a graded check (or a switch or the monitor's revert decides). Over the collectives, one line on borg itself when a newer release exists or a member's mesh protocol differs from this node's (not in `--json`). The messages to the engineer it shows are acknowledged, as `borg inbox` does; `--json` acknowledges nothing. Quits on `q`, Esc or Ctrl-C. Piped, or with `--once`, it prints the plain text once |
| `borg web [--port PORT] [--dir DIR]` | A live page on 127.0.0.1: an overview, knowledge, checks and lesson selection (Conscience), learning week by week, steering and refusals, messages (those to the engineer it lists are acknowledged), the queen, cost and drones, with a banner over the tabs while a newer release exists or a member's mesh protocol differs ([details](statistics.md)) |

### Team

| Command | Shows or does |
|---|---|
| `borg mesh <command>` | Team sync: `id`, `init`, `invite`, `invites`, `join`, `revoke`, `members`, `peer`, `serve`, `sync`, `fetch` ([details](team-sync.md)) |
| `borg tell <drone\|session\|local\|collective\|engineer> "<text>" [--re ID] [--from SESSION] [--dir DIR]` | A message to other sessions, on any harness: `local` is this machine's sessions, `collective` (or `all`) every session of the collective. Shown once at the recipient's next prompt or tool call ([Messages](#messages)) |
| `borg ask <drone\|session\|local\|collective\|engineer> "<question>" [--re ID] [--from SESSION] [--dir DIR]` | As `tell`, for a message that asks for a reply |
| `borg inbox [--all] [--json] [--show ID] [--from SESSION] [--dir DIR]` | In a session: its messages, new or shown, then marks them shown and acknowledges them. In a terminal, or with `--all`: every message of the last 7 days and who it was shown to; those to the engineer are acknowledged. `--show ID`: one message whole, with the line that replies |
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
one. What one prompt or tool call shows fits the harness's context budget (10,000 characters on every
harness today, [Capabilities](harnesses.md#capabilities)), which it shares with the lessons that event
delivers: newest first, the rest at the next one. A message longer than what is left shows its head and
`borg inbox --show <id>`, which prints it whole. opencode gives its shell no session id, so its reply
line carries `--from <session>`.

Text is redacted, put on one line and kept whole up to 16,384 characters (a longer one is cut with an
ellipsis). A text of at most 1,000 characters is written in the line format 0.5.0 reads too.

**Pacing.** A sender may have a window of unread tells and asks to one recipient. A message is
acknowledged once its recipient's hook shows it (or `borg inbox`, or `borg inbox --show`; for the
engineer also `borg top` and borg web's Messages page), or when the recipient replies to it: the
recipient writes a `seen` line, which syncs like the messages and is never shown. The window starts at
1, doubles with each acknowledged message, up to 64, and halves for each message that expires unread.
A full window refuses the next message ("`<n>` unread by `<to>`: it may have `<n>` unread at once; send
again once it is shown one or answers"). So a conversation between two sessions that read their
messages never waits, and a flood into a session that is gone stops at one message. An ask leaves the
window when it is shown or answered, as a tell does. Pause, resume and veto are never held back.

Where an acknowledgement may never come, nobody is paced more strictly than 0.5.0 paced them, one
message per sender and recipient every 10 minutes:

- a broadcast (`local`, `collective`) reaches whoever runs when it is sent, maybe nobody, and the
  engineer may read where nothing acknowledges: either may also get a message 10 minutes after the
  sender's previous one, whatever the window holds;
- a session on another machine is paced by that rate alone until its machine acknowledges one of the
  sender's messages to it (a machine on 0.5 never does): "`<to>`'s node has acknowledged no message
  yet, as a node on 0.5.0 never does: one message to it per 10m". The first `seen` line from that
  machine puts the pair under its window, on every machine that holds it, since the window is decided
  from the pair's own lines, which sync.

`borg tell` also refuses a message that would push one of the sender's unread messages to the same
recipient out of the log (its machine's share, below), one whose earlier stamp (a clock that stepped
back) would push a message already admitted out of the window, and a text longer than 1,000 characters
to a session on a machine whose borg is 0.4 or 0.5, which reads no more; a broadcast says how many
member machines will not show it.

`borg tell` decides by the window when it writes. The mesh then carries every message within the bound
any window keeps: one that was acknowledged, or one that, with its sender's later messages to the same
recipient within 10 minutes, leaves at most 64 unread. That bound reads no earlier line, so a message
once written keeps travelling after the lines its window was decided by are pruned, and a writer that
ignores its window gets at most 64 unread through per 10 minutes.

Messages live in `<store>/messages.jsonl` for 7 days. Of each member's lines, the newest of each class
(conversation, steers, acknowledgements, and apart from them the conversation and steers to `local`,
which never leave the machine) stay while they fit 1 MiB, so a flood of tells never pushes out a pause
or the acknowledgements that pace it. Those to `collective`, to a session on another machine
and replies sync with the team collective like lessons ([Team sync](team-sync.md#how-a-sync-works)); one
to `local` stays on this machine. An idle session sees a message when it next runs, unless it listens
([Waking a session](#waking-a-session)).

**Seen in monitoring.** `borg top` shows each drone's unread messages and the last day's messages, a
broadcast as `local` or `collective` (`borg inbox` and borg web's Messages page say it so too);
borg web's Messages page counts messages sent, shown and dropped (`why: window` for a refusal by a full
window, `why: bounds` for lines the 1 MiB bound cut). `borg top`, printed or live, and borg web's
Messages page acknowledge the messages to the engineer they show, as `borg inbox` does, so the
engineer's senders' windows open; `borg top --json` acknowledges nothing, and neither does `borg top`,
printed or live, in a drone's shell (as `borg inbox` there acknowledges nothing as the engineer), nor a
line cut before its message's text.

### Pause and resume

`borg pause` and `borg resume` are messages of their own kind; a veto from `borg no` to a drone on
another machine is one too. They go to a drone, a session, `local` (this machine's sessions) or
`collective` (or `all`: every member's, each machine admitting it by the sender's role), never to `engineer`; a pause or
resume may have no text. A veto names exactly one session on one machine (`<session>@<node>`). No window
holds them back, so a resume can follow a pause at once.

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
| omp, opencode | The stop's answer carries `"listen": true`; the adapter keeps `borg listen --stream` running for the session's life | One JSON line per wake (`{"wake": "<text>", "n": <messages>, "ids": [...]}`), sent in as a new turn: omp `pi.sendMessage`, opencode 1.x `session.promptAsync`, 2.x `session.prompt` with `delivery: "queue"`. A wake that arrives during a turn waits for its end, and is dropped once that turn's hooks handed the model every message it names (borg's answer names them, `"shown_ids"`). The adapter answers on the waiter's stdin, `{"sent": ids}` or `{"failed": ids}`; after a failed one the waiter wakes the session again 10 s later, doubling up to 5 minutes |
| Codex CLI | The stop starts `borg listen --queue` detached, logging to `<store>/listen/<session>.log` | `codex queue --thread <session> --message <text>`, 30 s at most; a failure waits 10 s, doubling up to 5 minutes, before the same messages are tried again |

The wake text names the number of messages, their senders and kinds, never their text. The session's
next prompt hook shows the messages themselves, once. Each message wakes a session once; a wake it never
took leaves the message unread, and the next message wakes it again. While the engineer's own turn runs,
from its prompt to the end of its stop, nothing wakes the session; that turn's hooks show what arrives.
A turn that never reached its stop (an Esc) stops holding wakes off once it has been quiet for 5
minutes (a minute on Codex CLI), or once a tool call that never ended has run for 10 minutes. A session
whose own gaps run longer holds longer: each hold is the 95th percentile of the session's latest 64
gaps of that kind, never below those floors. A wake's own turn holds the next wake off the same way,
until its stop.
A paused session is not woken, nor asked for a wait, until it is resumed.

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
| `borg debug select [--replay \| --bench] [--rebuild] [--dir DIR \| --file FILE] [--json] [--eval RUNS_DIR --qrels FILE]` | The lesson selection policy's state (its gate, fit, deviations, spend, label health) and, with `--replay`, its pre-registered checks P0–P5 replayed from the outcome log, at 0 tokens, always under the learned policy whatever the environment sets; `--eval` with `--qrels` adds P4 from an evaluation's runs. Prints the dates of the rules, of their latest amendment and of rules v2, reports P4 as P4a, P4b and P4c, and exits 1 when a decisive check (P0–P4) fails or tests nothing (NOT TESTED; P4b's NOT TESTED meets P4, as it then is the fixed rule). Reports, never applies ([Evidence](evidence.md#lesson-selection)). `--bench` times the hook's selection after its model call on each logged check that applies a lesson (loading the summary cache, deciding under the learned policy with the gate open, the worst case), in a temp copy, and exits 1 when its 95th percentile reaches 50 ms |
