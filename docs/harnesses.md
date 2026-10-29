# Harnesses

[← README](../README.md)

A **harness** is the coding agent a drone runs in: Claude Code, omp (oh-my-pi), opencode or the Codex CLI. Borg
supports each one through a single Rust trait and a capability table. A conformance suite checks every
harness against the same scenario, and a set of measured gates keeps the abstraction honest.

- [How a harness plugs in](#how-a-harness-plugs-in)
- [Capabilities](#capabilities)
- [Codex notes](#codex-notes)
- [Adding a harness](#adding-a-harness)
- [Conformance](#conformance)
- [Metrics and gates](#metrics-and-gates)

## How a harness plugs in

Every harness sends Borg one canonical, Claude-shaped hook event and gets a Claude-shaped answer back.
Claude Code's command hooks speak that shape natively, and so do Codex's, but for its patch tool, which
`normalise` turns into an edit. For omp and opencode, a TypeScript adapter
(`adapters/<harness>/borg.ts`) translates in both directions. So in the core, harnesses differ only in
data, and that data lives in `crates/borg/src/harness/`:

```rust
pub trait Harness: Sync {
    fn name(&self) -> &'static str;          // `borg hook <name>`, `borg install <name>`, BORG_HARNESS
    fn caps(&self) -> Caps;                  // what reaches its model, and when
    fn wiring(&self) -> Wiring;              // CommandHooks { events, timeouts, retired, through_links } | AdapterFile { source }
    fn target(&self, home: &Path, var: &dyn Fn(&str) -> Option<PathBuf>) -> PathBuf; // the config file install writes
    fn probe(&self) -> Probe;                // what `borg doctor` asks its CLI
    fn hidden(&self) -> bool { false }       // the simulated harness
    fn normalise(&self, raw: Value) -> Value { raw }                       // native payload -> canonical event
    fn checks(&self, target: &Path) -> Vec<(Level, String)> { vec![] }     // doctor checks of its own (e.g. hook trust)
    fn install_notes(&self) -> Vec<String> { vec![] }                      // steps install asks of the engineer
    fn sub_runs(&self, home: &Path, session: Option<&str>, since: Option<i64>) -> Vec<Run> { vec![] } // its sub-agent runs, for review → fix
}

pub static ALL: &[&dyn Harness] = &[&claude::Claude, &omp::Omp, &opencode::Opencode, &codex::Codex, &sim::Sim];
```

| File | What |
|---|---|
| `harness/mod.rs` | The trait, `Caps`, `DroneWake`, `Transcript`, `Wiring`, `Probe`, `Run`, `SubAgent`, `ALL`, `by_name`, and the transcript-or-event readers keyed on `Caps::transcript`: `work_so_far` (live mode's evidence), `last_message` (the drone's last message, whole) and `plan` (what the queen and the experts read) |
| `harness/wiring.rs` | The generic command-hook merge: install, uninstall and status for any `CommandHooks` harness. Each hook runs `<borg> hook <harness> <event>` |
| `harness/claude.rs`, `omp.rs`, `opencode.rs`, `codex.rs` | One harness each: event table or compiled-in adapter, config path, capabilities, doctor probe, and the reader of its sub-agent runs ([Review → fix findings](configuration.md#review--fix-findings)). Claude's transcript readers are in `claude.rs`; Codex's patch-to-edit `normalise` and its hook-trust check for `borg doctor` are in `codex.rs` |
| `harness/sim.rs` | The simulated harness, used by the conformance suite. It is hidden from help, completions, `all`, status and doctor, and `borg install sim` and `borg hook sim …` know it only with `BORG_TEST_HARNESS` set, as the suites run them |
| `hook.rs` | The shared hook handler, `hook::dispatch(h, event, input)`, with one function per event |
| `install.rs` | Plan, apply, diff, backup, status and doctor, generic over `&dyn Harness` |

The rest of the core asks a harness for its `Caps`, its wiring or one of its methods, and never compares
its name. `crates/borg/tests/harness_arch.rs` enforces this rule.

`borg hook <name> <event>` with an unknown name fails open: it prints nothing and exits 0.

## Capabilities

Each flag gates one behaviour of the core. A unit test (`caps_are_todays` in `harness/mod.rs`) pins
the flags. A flag changes only as a product decision with an evaluation behind it, never as a side
effect of a refactor.

| `Caps` field | Gates | Claude Code | omp | opencode 1.x / 2.x | Codex CLI |
|---|---|---|---|---|---|
| `prompt_context` | Context returned at a prompt reaches the model | yes | yes (hidden custom message) | yes (synthetic part / `session.synthetic`) | yes (a developer message in the next request) |
| `tool_context` | Context after a tool call reaches the model before its next step (live mode's asks) | yes | yes (omp ≥ 18.4.4) | yes (appended to the output / steered in) | yes (a developer message in the next request; in code mode after the whole `exec` script) |
| `early_delivery` | A prompt or touch naming a lesson's scope gets that scope's lessons at once | yes | no | no | no |
| `question_dialog` | A queen question uses the AskUserQuestion dialog (otherwise it is asked inline) | yes | no | no | no |
| `transcript` | Where the turn's work and the last message come from | Claude JSONL | none: the tool calls the hook logs (`.turn`) | none | none: `transcript_path` is a Codex rollout and is dropped |
| `deny_capture` | `post_tool_failure` carries the engineer's denials, which become episodes (otherwise the event is ignored) | yes | yes (synthesised) | yes (canonicalised) | no: Codex has no failure event |
| `sends_pid` | A drone whose process is gone reads as ended (otherwise an event's `pid` is dropped) | no | yes | yes | no |
| `session_env` | The variable the harness sets to its session's id in every command its model runs, so `borg listen`, `tell` and `inbox` know the session | `CLAUDE_CODE_SESSION_ID` | `PI_SESSION_ID` | none: the shell gets no session id, so the plugin binds the session itself | `CODEX_THREAD_ID` |
| `pid_env` | For a harness without `session_env`: the variable that carries its pid, which its events carry too, so presence names the session | none | none | `OPENCODE_PID` | none |
| `drone_wake` | How `borg listen` wakes an idle session ([Waking a session](commands.md#waking-a-session)) | `Model`: the model runs `borg listen --wait` in the background | `Plugin`: the adapter runs `borg listen --stream` | `Plugin`, as omp | `Queue`: a detached `borg listen --queue` runs `codex queue` |
| `stop_budget` | The stop's budget when `BORG_STOP_BUDGET_MS` is unset: the live wait, the stop's model call and the expert drone keep inside it | none | 28 s: omp's fixed 30 s handler limit less 2 s for the adapter | 90 s, borg's stop timeout (opencode sets no limit) | 90 s, the Stop hook's timeout |
| `context_chars` | The characters of context one hook answer may inject and have arrive whole, shared by everything that event delivers (messages, lessons) | 10,000: Claude Code saves a longer `additionalContext` to a file and gives the model a preview (2.1.312) | 10,000, the guard: omp injects it whole, with no limit of its own (18.13.1) | 10,000, the guard: no limit of its own (1.19.0; 2.x not measured) | 10,000: Codex's default `additionalContextLimit`; a longer one is cut to its head and tail (0.153.2) |

omp and opencode already have a channel for early delivery. It stays off until
an evaluation shows they help. Standing lessons (ratified constraints) are exempt: they are no ranking
guess, so they arrive early on every harness whose `prompt_context` or `tool_context` takes them
(hook.rs `early`).

Where a harness sets no context limit, the guard is the strictest limit another harness sets
(`CONTEXT_GUARD`), so one event, a flood of messages or a long pool of lessons, cannot fill the model's
context. A message longer than what an event has left shows its head and `borg inbox --show <id>`.

**Budgets for adapters.** How long an adapter waits for borg at each event before it fails open, and
how much of a shell call's output it sends (`tail_chars`, 4,000), are borg's: `Budgets` in
`harness/omp.rs` and `harness/opencode.rs`. An adapter sends `"budgets": true` with its events until an
answer carried `"budgets"` (`prompt_ms`, `pre_ms`, `post_ms`, `stop_ms`, `end_ms`, `tail_chars`); its own
copy is only the fallback before that, and a test keeps the two equal. The adapter passes `stop_ms` on
as `BORG_STOP_BUDGET_MS`.

| Budget | omp | opencode |
|---|---|---|
| prompt | 5 s | 5 s |
| before a tool call | 2 s | 2 s |
| after a tool call | 5 s | 2 s |
| stop | 28 s | 90 s |
| session end | 1.5 s (omp's 2 s limit less 0.5 s) | 90 s |

An answer that showed messages names them (`"shown_ids"`; it gives no count). An adapter drops a held
wake only once its turn's hooks handed the model every message the wake names, by those ids: one event
may show a newer message and leave the wake's. The ids add up over the turn's hooks, so a wake whose
messages borg showed over several is dropped too, and one that arrives after they were handed over is
dropped as it arrives. The wake goes when that context reaches the model, not when borg answers, so an
answer kept for a later call (opencode's that came after its call's budget, omp's to a steer, kept for
the next agent start or tool result) drops nothing until that call carries it. An answer that names none
(an older borg's) drops nothing, and a wake that names none (an older waiter's) is never dropped. Only
the adapters get that key: a `CommandHooks` harness (Claude Code, Codex) parses the answer itself and
never gets it.

With no budget at all, the expert drone keeps inside 60 s and the model call inside the model's own
limit. The 60 s is Claude Code's old command-hook default, so a Stop hook installed without a `timeout`
on an older Claude Code is never cut off. `borg install` now gives Claude Code's Stop hook 90 s. A
`CommandHooks` harness gives any event's hook its own `timeout` (`timeouts`, canonical event → seconds);
Claude Code's table names the stop only, Codex's every event. Hooks an older borg installed and this one
does not (`retired`: Claude Code's `SessionStart`, which served the hive up to 0.3) are removed at
install and uninstall alike.

| Wiring | Harness | Installs |
|---|---|---|
| `CommandHooks` | Claude Code | Hook groups merged into `~/.claude/settings.json` (`$CLAUDE_CONFIG_DIR`), written through a symlink |
| `AdapterFile` | omp | `~/.omp/agent/extensions/borg.ts` (`$PI_CODING_AGENT_DIR/extensions`) |
| `AdapterFile` | opencode | `~/.config/opencode/plugins/borg.ts` (`$OPENCODE_CONFIG_DIR`, `$XDG_CONFIG_HOME/opencode`) |
| `CommandHooks` | Codex CLI | Hook groups appended to `~/.codex/hooks.json` (`$CODEX_HOME`), written through a symlink, each with its own timeout |
| `CommandHooks` | sim (hidden) | `~/.sim/hooks.json`; capabilities from `BORG_SIM_CAPS` |

## Codex notes

What the Codex CLI does differently, measured on 0.153.2 against a local mock of the model API
(`adapters/codex/smoke.sh`, 0 tokens):

- **Trust.** Codex runs a new or changed hook only after the engineer trusts it (`/hooks` in codex, or
  "Trust all" at startup); `codex exec` skips an untrusted hook without a word. The trust covers each
  hook's command and timeout and its position in `hooks.json`, so a new borg path, a changed timeout or
  a hook inserted before borg's needs trusting again. `borg install codex` says so, and `borg doctor`
  asks codex itself (`codex app-server`, `hooks/list`, 3 s at most) which of borg's hooks it would skip.
  Borg never writes that trust: it protects the engineer. Install appends borg's groups after yours
  and never reorders them; a re-install from a new path replaces borg's hook where it stands. Uninstall
  removes them, which moves a hook of yours that came after them up a position, and codex then asks you
  to trust it again.
- **Edits.** `apply_patch` reaches the hook as a patch. `normalise` makes it an `Edit`: the first file
  the patch names is `file_path`, its added lines `new_string`, and every further file (a rename's
  `*** Move to:` destination too) goes into `edits[]` with its own `file_path` and `new_string`. The hook
  reads the files in `edits[]` too, so each one lands in `.ctx` and `.edits` (every path before the text,
  which is cut at 600 characters). Presence (`borg top`) shows one file per drone, the first. Relative
  paths are taken from the event's `cwd`.
- **Edits borg does not see.** Codex fires no PostToolUse for a failed `apply_patch`, for `apply_patch`
  run through a shell heredoc (it shows up as a `Bash` PreToolUse only), or for a shell command still
  running when it yields. Such an edit is in neither `.edits` nor a live check; the stop's check still
  reads the drone's last message.
- **Code mode.** Models in code mode run tool calls inside one `exec` script. Each inner call fires its
  own Pre/PostToolUse, but context returned there reaches the model only after the whole script ran,
  so a live check cannot stop a later edit of the same script.
- **The stop.** A block starts another round in the same turn, and Codex never caps the rounds;
  `stop_hook_active` stays true for every later stop of the turn. Borg answers nothing to a stop with
  `stop_hook_active` set (one round per turn), so it never loops. The stop gets 90 s; Codex's default
  is 600 s.
- **The end.** Codex gives SessionEnd at most 3 s (1 s by default). Borg's `end` only starts `borg learn`
  in the background.
- **Esc.** Codex's `Interrupt` hook (3 s) becomes `halted`: the turn of a listening session ends at
  once, so wakes go in again. Without it, a turn with no stop and a tool call that never ended would
  hold wakes off as long as the session's own tool calls run (10 minutes until it has some).
- **Borg's own calls.** `--ignore-user-config` still loads `$CODEX_HOME/hooks.json`. The `codex:` runner
  passes `-c features.hooks=false`, so its call never runs borg's hooks, and never passes
  `--dangerously-bypass-hook-trust`.

## Adding a harness

1. **Module.** Add `crates/borg/src/harness/<name>.rs` with `impl Harness`. A harness with command hooks
   in Claude's JSON shape needs only an event table (native name → canonical event) and its config path.
   A harness with a plugin API needs an adapter in `adapters/<name>/` that emits the canonical event, and
   `Wiring::AdapterFile { source: include_str!(…) }`. Read a config-dir variable only through `target`'s
   `var`: the conformance suite asks `target` which variables to point outside `--home`.
2. **Register it.** Add one line to `ALL`. Help, `install` usage, completions, `install all`, status and
   doctor all read `ALL`. A step the engineer takes in the harness itself (trusting the hooks, say) is
   `install_notes`; a doctor check of its own (a version too old for live mode, untrusted hooks) is
   `checks` (doctor runs it only once the CLI answered `--version`). Neither needs a change in `install.rs`.
3. **Capabilities.** Set `Caps` to what the harness verifiably delivers. Leave a flag off until an
   evaluation shows it helps. Extend the snapshot in `caps_are_todays`. Name the variable that gives a
   command the session's id (`session_env`, or `pid_env` where the shell gets only a pid) and pick how
   an idle session is woken (`drone_wake`). Where the harness runs sub-agents, `sub_runs` reads them,
   so their review findings are learned from.
4. **Normalise.** If the native payload differs from the canonical event (tool names, input keys, a
   transcript in another format), override `normalise` or translate in the adapter. Borg reads
   `file_path`, `new_string`, `content` and `command`, and for an edit of several files `edits[]`
   entries with their own `file_path` and `new_string`.
5. **Conformance.**
   - Command hooks: add `crates/borg/tests/fixtures/<name>/` with the native payloads. The Rust suite
     picks up every `CommandHooks` harness in `ALL`. A step whose event the harness wires no hook for
     (Codex has no failure event) needs no fixture, and is skipped.
   - Adapter: add a profile to `adapters/conformance/conformance.test.ts`.
6. **Docs.** Add a column to the capability table above, write the adapter's README, add the harness
   to the README's opening paragraph, and add a row to [Waking a session](commands.md#waking-a-session)
   and [Review → fix findings](configuration.md#review--fix-findings).

## Conformance

One scenario runs for every harness:

1. install
2. prompt
3. read
4. edit
5. the live check
6. read (the ask is delivered)
7. stop
8. stop again
9. correction
10. end

The model is a fake `claude` on `PATH`, so the suite spends 0 tokens. An assertion that depends on a
capability is chosen by the harness's `Caps` flag, never by its name, and asserts the side the flag is
on. The TypeScript suite reads the flags from the harness's module, fails when there is no borg binary
(`BORG_TEST_BIN`; `scripts/check.sh` passes cargo's), and takes the stop's bound from the adapter's own
timeout.

```sh
cargo test -p borg-ai --test conformance_cli                    # Claude Code, Codex and sim: the installed command string, native fixtures
adapters/codex/smoke.sh                                      # a real codex exec against a mock model API: install, edit, live ask, stop, runner, doctor
cargo build -p borg-ai && node --test adapters/conformance/conformance.test.ts   # omp, opencode 1.x and 2.x: the installed adapter
cargo test -p borg-web --test drones_api                     # the page's /api/drones and /api/collectives over a seeded store
```

| Area | What it checks |
|---|---|
| Install | A dry run writes nothing and prints a diff. `--apply` writes. A re-run changes nothing. Foreign keys and hooks are kept, with a backup. Status shows `wired:` and doctor shows `ok`. Uninstall removes only borg's entries. `--home` isolates: with `HOME` and every harness's config-dir variable pointing elsewhere, nothing lands outside it. |
| Hooks | An off-repo prompt writes nothing. The prompt is recorded. Touches go to `.ctx`, and an edit's path and text go to `.edits`. A paused drone's tool call is refused. The stop blocks once, within its budget, and the second round passes; a model slower than the budget (`BORG_STOP_BUDGET_MS`) never holds the stop past it. `end` ends the drone. Garbage input fails open. |
| Live | An edit starts a detached check and the hook returns at once. The ask is queued and delivered with the next tool call; without tool context, with the next prompt; with neither, at the stop. A covered stop makes no model call. |
| Web | `borg top --json` shows the drone with its `harness` and its state: working, then waiting, then ended. `/api/drones` and `/api/collectives` show the same. |
| Capabilities | Context at a prompt and after a tool call, early delivery, the transcript source, denials to episodes, and a gone pid, each on both sides. |

`sim` runs four profiles: the default flags (`prompt,tool,deny`), no tool context (`prompt,deny`), no flag
at all, and every flag on. Together they put `prompt_context`, `tool_context`, `early_delivery`,
`transcript`, `deny_capture` and `sends_pid` on both sides from one binary. The real
harnesses add the sides they have. `question_dialog` is the exception: no conformance test asks a queen
question yet, so only the queen's unit tests cover it.

The suite caught one bug: the opencode adapter did not map an edit's `newString`. It also guards the fix
of a bug the research found before the suite existed: the omp adapter passed `edit`/`write` input raw, so
`.edits` had no file path and no patch text. Both are fixed.

## Metrics and gates

`scripts/arch.sh` measures `crates/borg/src` in the working tree and, for the Before column, at the
commit before the trait (`bcebe3c`, or `ARCH_BASE`). A history without that commit (a fresh clone after
the release) has no Before column: the script says so and still runs the gates. The Before values below
were measured once, before the release. It fails when a gate fails. The tools are lizard, cargo-coupling and
rust-code-analysis-cli. They run offline and spend 0 tokens, and the script skips the rows of any tool
that is missing. M1 and M2 need only grep and awk, and `crates/borg/tests/harness_arch.rs` also runs them
under `cargo test`.

| Metric | Before | After | Gate |
|---|---|---|---|
| M1 harness-name branches outside `harness/` (grep lines; `runner.rs`, `assimilate*.rs` aside) | 15 | 0 | 0 |
| Branches in the deferred `runner.rs` and `assimilate*.rs` | 12 | 12 | report |
| M2 files listing harness names outside `harness/` (`runner.rs` aside) | 3 | 0 | 0 |
| M5 dependency cycles through `harness::*` | – | 0 | 0 |
| M7 max CCN of a function in `harness/` | – | 9 | ≤ 10 |
| M9 max CCN of the hook's per-event functions | 45 (`claude_hook`) | 11 | ≤ 15 |
| M10 functions in `install.rs`, tests aside | 44 | 39 | < 40 |
| Cognitive complexity of the hook's per-event functions (rca) | 83 | 9 | report |
| Functions with CCN > 15 in `crates/borg/src` (lizard) | 17 | 15 | report |
| Functions in `main.rs`, tests aside | 51 | < 30 | report |
| cargo-coupling grade / cycle paths | C / 31 | C / 31 | report |

M1 counts a comparison or match arm on a harness's name, and on any enum variant named after a harness
(`Source::Claude =>`, `== Runner::Claude`). It leaves out two files, named in `M1_DEFERRED`
(`harness_arch.rs`) and reported on their own row: `runner.rs` (`Runner`) is a separate table of the CLIs
Borg's own model calls run through, and `assimilate*.rs` imports other agents' history per source.
Folding them into the trait is a follow-up, along with the borg-trace parsers.
