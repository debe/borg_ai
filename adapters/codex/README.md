# borg for the Codex CLI

Codex speaks borg's hook shape natively, so there is no adapter: `borg install codex` adds command
hooks to `~/.codex/hooks.json` (`$CODEX_HOME/hooks.json`), each running `<path to borg> hook codex
<event>`. Tested with codex-cli 0.153.2.

| Codex event | borg event | Timeout | Notes |
|---|---|---|---|
| `UserPromptSubmit` | `prompt` | 30 s | borg's context for the prompt reaches the model as a developer message. |
| `PreToolUse` | `pre_tool` | 10 s | Only a drone paused by `borg steer --pause` gets an answer: a deny. A slow hook never blocks a tool: Codex runs the tool once the timeout passes. |
| `PostToolUse` | `post_tool` | 30 s | `apply_patch` becomes an edit of every file the patch names (`*** Add/Update/Delete File:`, and a rename's `*** Move to:`), with the added lines as its text. In live mode a check's questions come back as `additionalContext`, which reaches the model's next request. |
| `Stop` | `stop` | 90 s | borg's `{"decision":"block","reason"}` starts one more round. Codex never caps the rounds, but borg answers nothing to a stop with `stop_hook_active` set, so there is one round of questions per turn. The 90 s are borg's whole budget at the stop. |
| `SessionEnd` | `end` | 3 s | Codex's maximum. borg starts `borg learn --session` in the background and returns. |

Codex has no failure event, so a denied tool call does not become an episode; corrections you type
still do. Codex's `transcript_path` is its own rollout format, which borg does not read: a live check
reads the turn's tool calls and edits, not the drone's prose.

## Install

```sh
borg install codex            # shows the diff
borg install codex --apply    # writes it, with a backup; your own hooks stay first
```

Then open codex and run `/hooks` to trust borg's hooks (or choose "Trust all" when codex starts).
**Codex skips an untrusted hook silently**, and it asks again whenever a hook's command or timeout
changes (a borg binary at a new path, say) or its position in `hooks.json` does (a hook added before
borg's). A re-install keeps borg's hooks where they are, but `borg uninstall codex` moves a hook of
yours that came after borg's up a position, and codex asks you to trust that one again. `borg doctor` asks codex itself (`codex app-server`,
`hooks/list`) and warns about every hook of borg's it would skip. borg never trusts its hooks on your
behalf. `borg uninstall codex --apply` removes them again.

## What borg misses

- An `apply_patch` that fails, one run through a shell heredoc, and a shell command that is still
  running when it yields fire no PostToolUse, so borg never sees those edits during the turn.
- Models in code mode run their tool calls inside one `exec` script: borg's context reaches the model
  only after the whole script, so a live check cannot stop a later edit of the same script.
- Context over about 2,500 tokens is cut by codex (the rest goes to a file under `/tmp/hook_outputs`).

## borg's own model calls

`codex:<model>` as `BORG_LEARN_MODEL` or `BORG_PREDICT_MODEL` runs borg's calls through `codex exec`,
read-only, ephemeral, with your config ignored and `-c features.hooks=false`: `--ignore-user-config`
alone still loads `hooks.json`, and borg's call would run borg's hooks.

Nothing pins codex for you: with neither setting, borg's calls go to `claude`, `omp` or `opencode` on
`PATH`. With none of them installed, live and turn-end checks never judge and borg learns nothing;
`borg doctor` then names the two settings to pin.

## Smoke test

`adapters/codex/smoke.sh` runs a real `codex exec` in a fresh temp dir (its own `CODEX_HOME`, `HOME`
and borg store) against `mock.py`, a local stand-in for the model API: 0 tokens, no network. It checks
that install appends the five hooks with their timeouts beside your own, that an `apply_patch` lands in
borg's `.edits`, that a live check's question reaches the model's next request, that your own hooks run
beside borg's, that in hook mode borg blocks the first stop once and lets the second
(`stop_hook_active`) pass, that borg's own codex call fires no hook, and that `borg doctor` reports the
untrusted hooks.

The turns pass `--dangerously-bypass-hook-trust`: trusting hooks is your step, and the test must not
write trust. A shim also forces the flag onto borg's own codex call, to show that
`-c features.hooks=false` alone keeps the hooks out. borg itself never passes the flag.

```sh
cargo build -p borg-ai && adapters/codex/smoke.sh     # KEEP=1 keeps the temp dir
```
