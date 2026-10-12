# borg for opencode

`borg.ts` is an opencode plugin that connects opencode sessions to the collective. One file serves
both plugin APIs: opencode 1.x (the `server` entrypoint, built against 1.18.32; object entrypoints
need 1.18.29 or newer) and opencode 2.x (the `setup` entrypoint, built against 2.0.23). It turns
opencode's hooks and events into borg's canonical hook event and pipes it to
`borg hook opencode <event>`:

| opencode 1.x | opencode 2.x | borg event | what borg does |
|---|---|---|---|
| `chat.message` (the engineer's text) | session hook `prompt` | `prompt` | records the task; a correction becomes an episode; borg's context for the prompt (a queen question) reaches the drone but stays hidden in the TUI: on 1.x a synthetic text part of the message, on 2.x a synthetic session input without a description, just before the prompt; behind a stop that is still thinking the message does not wait, and the payload's `"context": false` tells borg to hand none over |
| `tool.execute.before` | tool hook `execute.before` | `pre_tool` | throws borg's reason when the drone is paused by `borg steer --pause` (refuses the call); fails open after 2 s |
| `tool.execute.after` | tool hook `execute.after` | `post_tool` | notes what the drone touched (tool names mapped to Claude's: `read` → `Read`, `shell`/`bash` → `Bash`, `filePath`/`path` → `file_path`) |
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
- If the engineer types while borg is still answering a stop (up to 60 s with `BORG_MATCH=predict`),
  the questions about the earlier turn are dropped.

The plugin ignores subagent (child) sessions, except before a tool call: a child's `pre_tool` asks
as its root session, so a drone paused by `borg steer --pause` cannot keep working through a subagent.

The plugin fails open. If borg is missing, slow (60 s limit) or broken, opencode carries on as if
the plugin were not there.

## Install

`borg install opencode` shows what it would write; `borg install opencode --apply` copies the plugin
(built into the binary) to `~/.config/opencode/plugins/borg.ts`, with a backup of anything it
replaces. `borg uninstall opencode --apply` removes it again, `borg doctor` checks the wiring. By hand:

1. Build borg and put it on `PATH`, or point `BORG_BIN` at it:

   ```sh
   cargo build --release -p borg
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
cargo build -p borg
node --test adapters/opencode/borg.test.ts   # Node >= 23.6 runs the TypeScript directly
adapters/opencode/live/run.sh                # real opencode 1.x, isolated homes, mock LLM, 0 tokens
adapters/opencode/live/run-v2.sh             # real opencode 2.x, likewise (never the background service)
```

- `borg.test.ts` is the contract test, for both entrypoints (`server` and `setup`). It sends opencode-shaped hooks and events to the plugin and
  checks the exact canonical JSON the plugin produces. It checks the guards above: no stop after a
  halted turn, no stale questions after the engineer spoke during a slow stop, and no loop when the
  echo comes back changed. It then runs the real borg binary and checks that borg's questions return
  once per idle cycle, that the echoed question is not treated as the engineer's prompt, and that a
  correction and a rejected edit are recorded as episodes.
- `live/run.sh` starts `opencode serve` with every home and config directory in a temp dir, plus a
  local OpenAI-compatible mock LLM. It then runs `opencode run --attach` and checks that the plugin
  loads and that prompt, `post_tool` and `post_tool_failure` reach borg. It also checks that the
  conscience's question reaches the LLM exactly once, that its echo comes back with the borg marker,
  and that deleting the session sends `end`. Set
  `OPENCODE=/path/to/opencode` if opencode is not on `PATH` (e.g. a 1.x binary from the
  `opencode-linux-x64` npm package).
- `live/run-v2.sh` checks the same against opencode 2 with a private `opencode serve` (its own
  password, a disabled service config, every client pointed at it with `--server`), and reports
  whether a normal shutdown ends an undeleted session through the plugin's cleanup.
