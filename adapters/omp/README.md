# borg for omp (oh-my-pi)

`borg.ts` is an omp extension. It turns omp's events into `borg hook omp <event>` calls and hands borg's answer back to omp. Tested against omp 18.8.5.

| omp event | borg event | Notes |
|---|---|---|
| `input` (interactive, RPC) | `prompt` | The engineer's raw text. `/commands` and `!shell` lines are skipped: omp emits `input` before handling them. borg's context for the prompt (a queen question) joins the turn at `before_agent_start` as a hidden custom message. |
| `before_agent_start` | `prompt` | Used until an `input` arrives, because `omp -p` emits no `input`, and for a `/command` that starts an agent run (its expansion). borg's own questions are skipped. |
| `tool_call` | `pre_tool` | A drone paused by `borg steer --pause` gets `{block: true, reason}`; anything else (or no answer within 2 s) lets the call run. |
| `tool_result` | `post_tool` / `post_tool_failure` | A failure passes the result text as `error`. Tool names map to Claude's (`bash` → `Bash`, `find` → `Glob`, …). |
| `tool_approval_resolved`, denied by the user | `post_tool_failure` | `error` is omp's own text, `Tool call denied by user: <tool>`. |
| `session_stop` | `stop` | borg's `{"decision":"block","reason"}` goes straight back to omp. omp then continues the turn with the reason and sets `stop_hook_active` for the next stop, so you get one round of questions per turn. |
| `session_shutdown` | `end` | borg starts `borg learn --session` in the background. |

An omp denial carries no words: the approval dialog only offers Approve and Deny. borg remembers the denied call, and the engineer's next prompt becomes the correction (`learn::denied`). omp, unlike Claude, lets the model carry on after a denial, so the wait ends at the next successful tool call or at the end of the session. A plan refined with "Refine plan" arrives as an ordinary prompt.

The extension fails open. If the borg binary is missing, crashes, times out or prints something it cannot read, the extension does nothing. It looks for borg in `$BORG_BIN`, then on `PATH`.

## Install

`borg install omp` shows what it would write; `borg install omp --apply` copies the adapter (built into the binary) to `~/.omp/agent/extensions/borg.ts`, with a backup of anything it replaces. `borg uninstall omp --apply` removes it again, `borg doctor` checks the wiring. By hand:

```sh
cargo install --path crates/borg            # or put target/release/borg on PATH, or set BORG_BIN
mkdir -p ~/.omp/agent/extensions
ln -s "$PWD/adapters/omp/borg.ts" ~/.omp/agent/extensions/borg.ts
```

Other ways to load it:

- For one project: put it in `<project>/.omp/extensions/borg.ts`.
- For one run: `omp -e adapters/omp/borg.ts`.
- If you set `PI_CODING_AGENT_DIR`, user extensions live in `$PI_CODING_AGENT_DIR/extensions`.

The `BORG_*` variables are documented in `crates/borg/src/main.rs`. Set `BORG_LEARN=off` for eval drones.

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

## Tests

```sh
cargo build -p borg
node --test adapters/omp/borg.test.ts   # contract test: omp-shaped events -> adapter -> real `borg hook omp`
adapters/omp/smoke/smoke.sh             # one real headless omp run (temp HOME and agent dir, local mock model, no tokens)
```
