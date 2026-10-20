# borg for opencode

`borg.ts` is an opencode plugin that connects opencode sessions to the collective. One file serves
both plugin APIs: opencode 1.x (the `server` entrypoint, built against 1.18.32 and verified on
1.19.0; object entrypoints need 1.18.29 or newer) and opencode 2.x (the `setup` entrypoint, built
against 2.0.23 and verified on 2.0.29). It turns
opencode's hooks and events into borg's canonical hook event and pipes it to
`borg hook opencode <event>`:

| opencode 1.x | opencode 2.x | borg event | what borg does |
|---|---|---|---|
| `chat.message` (the engineer's text) | session hook `prompt` | `prompt` | records the task; a correction becomes an episode; borg's context for the prompt (a queen question) reaches the drone but stays hidden in the TUI: on 1.x a synthetic text part of the message, on 2.x a synthetic session input without a description, just before the prompt; behind a stop that is still thinking the message does not wait, and the payload's `"context": false` tells borg to hand none over |
| `tool.execute.before` | tool hook `execute.before` | `pre_tool` | throws borg's reason when the drone is paused by `borg steer --pause` (refuses the call); fails open after 2 s |
| `tool.execute.after` | tool hook `execute.after` | `post_tool` | notes what the drone touched (tool names mapped to Claude's: `read` → `Read`, `shell`/`bash` → `Bash`, `filePath`/`path` → `file_path`, an edit's `newString` → `new_string`); borg's context for the call (in live mode a check's questions, or waiting graded lessons) reaches the model before its next step, see below |
| a tool part that ends in `error`, including a permission rejected with feedback | `execute.after` with an error (a rejection with feedback carries it), or `session.tool.failed` for a call declined without feedback | `post_tool_failure` | the engineer's feedback becomes an episode |
| `session.idle` | `session.execution.succeeded` | `stop` | the conscience may answer `{"decision":"block","reason"}`; the plugin sends the reason back into the session as a user message |
| `session.deleted`, opencode shutting down | `session.deleted`, the plugin's cleanup (reload, shutdown) | `end` | runs `borg learn --session ID` in the background |

The plugin answers a stop with borg's questions at most once per engineer prompt. The next idle is
sent with `stop_hook_active: true`, as Claude Code does. Three more guards keep the plugin from
prompting the drone on its own:

- The questions go in with `metadata: { borg: true }` (opencode 1: on the text part; opencode 2: on
  the prompt, delivered as `queue`), so their echo is never taken for the engineer's prompt. An unmarked message that arrives while that echo is pending is
  also taken for the echo.
- A turn that the engineer or the provider halted gets no stop. This covers Esc or an abort
  (`MessageAbortedError`), a provider error, and a permission rejected without feedback. opencode 2
  ends such a turn with `session.execution.interrupted` or `session.execution.failed` instead of
  `succeeded`. Claude Code's
  Stop hook does not fire on an interrupt either.
- If the engineer types while borg is still answering a stop (up to 90 s with the consequence model),
  the questions about the earlier turn are dropped.

**Live mode.** Both after-hooks are awaited, so borg's answer to a tool call reaches the model at its
next step. opencode 1.x appends it to the tool's output, which opencode stores with the call: the TUI
does not show it for built-in tools, but session exports do. opencode 2.x sends it into the session as a
synthetic input with `delivery: "steer"` and leaves the tool result alone (the TUI shows the result).
The plugin waits 2 s for borg's answer (while a stop is still answering, not at all); one that comes
later is kept and goes with the next tool call or the next prompt, since borg already counts it as
delivered. The same holds for the prompt's context after 5 s, and for a steer that opencode 2.x refuses. opencode 1.x runs no after-hook for a
failed tool call, so a question waiting then goes with the next successful call, the next prompt or the
stop. opencode writes no Claude transcript, so borg's live check reads the turn's tool calls and edits,
not the drone's prose.

The plugin ignores subagent (child) sessions, except before a tool call: a child's `pre_tool` asks
as its root session, so a drone paused by `borg steer --pause` cannot keep working through a subagent.

The plugin fails open. If borg is missing, slow or broken, opencode carries on as if the plugin were
not there. A stop gets 90 s (opencode sets no limit of its own), which the plugin passes to borg as
`BORG_STOP_BUDGET_MS`: room for a 15 s wait for a running live check, the 45 s model call and an
expert drone.

Rejections reach borg in Claude Code's wording. A rejection with feedback carries the engineer's
words. A rejection without feedback (opencode 1: `The user rejected permission to use this specific
tool call.`, opencode 2: `The user declined this tool call`) becomes Claude's plain rejection, so the
engineer's next prompt is recorded as the reason. `opencode run` rejects permission asks by itself
on opencode 2 ("This non-interactive run cannot ask the user for permission..."); that is not feedback
and records nothing.

## borg's model calls and `borg assimilate`

borg can answer its own model calls (learning, assimilate, the turn-end check) with opencode, so an
opencode-only engineer needs no `claude` CLI. borg tells opencode 1.x from 2.x by whether
`opencode run --help` offers `--standalone`. opencode 1.x runs `opencode run --pure` with every
permission denied (`OPENCODE_PERMISSION`). opencode 2.x runs `opencode run --standalone`, a private
server rather than the background service, which would load this plugin and ignore the call's
environment. It also gets an empty `OPENCODE_CONFIG_DIR`, which keeps every plugin out, your providers'
config (`OPENCODE_CONFIG`: the file you set it to, else your global `opencode.json`), and a deny-all config
(`OPENCODE_CONFIG_CONTENT`). Each call is stored as a session titled `borg (model call)` under the
temp dir.

`borg assimilate` imports corrections from past opencode sessions of both versions through the CLI.
It only reads: `opencode db` and `opencode export` on 1.x, and `opencode session list` and
`opencode session export` with `--standalone` on 2.x, in the same env as a model call (no plugins,
no project config). opencode 2.x lists sessions per project, so borg reads the projects it knows: the
directories of your Claude Code and omp sessions, the collectives' paths and the git repos up to two
levels inside them. opencode 2.x does not list the sessions it migrated from 1.x; export those with an
opencode 1.x (2.x keeps the 1.x tables) into a directory and pass `--opencode DIR`, which reads a
directory of exports of either version. Sub-agent sessions, borg's own calls, its echoed questions and
synthetic context are skipped, and so are the plain rejections opencode 1.x adds to the other pending
calls when you reject one with words.

## Install

`borg install opencode` shows what it would write; `borg install opencode --apply` copies the plugin
(built into the binary) to `~/.config/opencode/plugins/borg.ts`, with a backup of anything it
replaces. `borg uninstall opencode --apply` removes it again, `borg doctor` checks the wiring. By hand:

1. Build borg and put it on `PATH`, or point `BORG_BIN` at it:

   ```sh
   cargo build --release -p borg-ai
   export BORG_BIN=$PWD/target/release/borg   # or copy it into a directory on PATH
   ```

2. Copy the plugin into a plugins directory that opencode reads:

   ```sh
   # for every project
   mkdir -p ~/.config/opencode/plugins && cp adapters/opencode/borg.ts ~/.config/opencode/plugins/
   # or for one project only
   mkdir -p .opencode/plugins && cp adapters/opencode/borg.ts .opencode/plugins/
   ```

   opencode loads `{plugin,plugins}/*.{ts,js}` from its config directory (`$XDG_CONFIG_HOME/opencode`,
   or `$OPENCODE_CONFIG_DIR`) and from `.opencode/` in the project. Copy only `borg.ts`, because
   opencode loads every `.ts` file in that directory as a plugin. opencode 1 installs
   `@opencode-ai/plugin` next to its config on the first start, which needs npm to be reachable. The
   plugin uses that package only for types and needs no package at all under opencode 2. opencode 2
   reloads a plugin when its file changes, which ends (and sends `end` for) the sessions it knew.

3. Start opencode as usual (opencode 2's background service picks up the new file by itself). Set `BORG_OPENCODE_LOG=/tmp/borg-opencode.log` to see what the plugin
   sends and every error it swallowed.

borg's own environment applies unchanged: `BORG_HOME`, `BORG_LEARN=off`, `BORG_CONSCIENCE`,
`BORG_MATCH` and the others are listed in `crates/borg/src/main.rs`.

## Reasoning on demand: `borg why`

When a session lands commits, borg keeps the drone's redacted reasoning as a git note
(`refs/notes/borg`, never pushed). Ask for it when you need it; nothing injects it, so it costs no
tokens until someone asks:

```sh
borg why src/login.rs:40-60   # blame the span: each commit, its task and proposal, linked lessons
borg why 3f2a9c1              # one commit
borg why src/login.rs --full --json
```

If the repo lacks the note, `borg why` uses the copy the collective's store holds (replicated from
peers). It prints nothing in a repo that belongs to no collective, and shows a private collective's
note only inside that collective.

## Limits

- opencode 2 loads a plugin on the first request for a directory, which is also the one that creates
  the first session there; the plugin does not see that session's `session.created`. Only subagent
  detection reads that event, and a subagent is always created later.
- opencode 1: plain `opencode run "…"` exits at the first idle, so it ends before the turn that answers borg's
  questions. The questions work in the TUI, under `opencode serve`, and with
  `opencode run --attach <server>`.
- opencode 1 does not report the end of a session that it never deletes when its server is killed;
  opencode 2 ends them through the plugin's cleanup on a normal shutdown (SIGTERM), not on SIGKILL. In
  that case the `end` event (and with it `borg learn`) does not run. Run `borg learn` by hand to
  generalise those episodes.

## Tests

```sh
cargo build -p borg-ai
node --test adapters/opencode/borg.test.ts   # Node >= 23.6 runs the TypeScript directly
node --test adapters/conformance/conformance.test.ts   # the installed plugin (1.x and 2.x) through the harness conformance scenario
adapters/opencode/live/run.sh                # real opencode 1.x, isolated homes, mock LLM, 0 tokens
adapters/opencode/live/run-v2.sh             # real opencode 2.x, likewise (never the background service)
```

- The fake hosts (opencode 1's SDK client, opencode 2's plugin context) are in `fakes.ts`, shared with the
  harness conformance suite ([docs/harnesses.md](../../docs/harnesses.md)).
- `borg.test.ts` is the contract test, for both entrypoints (`server` and `setup`). It sends opencode-shaped hooks and events to the plugin and
  checks the exact canonical JSON the plugin produces. It checks the guards above: no stop after a
  halted turn, no stale questions after the engineer spoke during a slow stop, and no loop when the
  echo comes back changed. It checks that borg's tool-call context is appended to the tool output on
  1.x and steered in on 2.x (the tool result untouched), and that an answer later than 2 s comes with
  the next tool call or prompt. It then runs the real borg binary and checks that borg's questions return
  once per idle cycle, that the echoed question is not treated as the engineer's prompt, and that a
  correction and a rejected edit are recorded as episodes.
- `live/run.sh` starts `opencode serve` with every home and config directory in a temp dir, plus a
  local OpenAI-compatible mock LLM. It then runs `opencode run --attach` and checks that the plugin
  loads and that prompt, `post_tool` and `post_tool_failure` reach borg. A wrapper leaves a lesson in
  the session's queue at the first tool call, as a live check would, and the test checks that the LLM
  request after that call carries it in the tool output. It also checks that the
  conscience's question reaches the LLM exactly once, that its echo comes back with the borg marker,
  and that deleting the session sends `end`. Set
  `OPENCODE=/path/to/opencode` if opencode is not on `PATH` (e.g. a 1.x binary from the
  `opencode-linux-x64` npm package).
- `live/run-v2.sh` checks the same against opencode 2 (the queued lesson right after the tool
  results, as a steer) with a private `opencode serve` (its own
  password, a disabled service config, every client pointed at it with `--server`), and reports
  whether a normal shutdown ends an undeleted session through the plugin's cleanup.
