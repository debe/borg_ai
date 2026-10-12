# Dogfood end to end: Borg as a product in a sandbox HOME (2026-10-09)

Branch `borg/dogfood-e2e`. To reproduce, run `eval/dogfood/run.sh [--run NAME] [--model sonnet] [--setup-only]`, which
builds borg and then runs `borg-eval dogfood` (code in `crates/borg-eval/src/dogfood.rs`). Each run writes
`target/dogfood/<run>/results.json` (the checks with evidence, every step's cost, wall time and hook latencies) and one
`<step>.stream.jsonl` per Claude turn. Since commit 8c5968e, results.json also records the harness's git commit (and
whether its sources were dirty) and the definition of each model-dependent check. The model-free part (install, doctor,
routing, status, uninstall, credentials) is also an ignored Rust test: `cargo test -p borg-eval dogfood_setup -- --ignored`.

**Result.**
- **The plumbing works.** Runs 4 and 5 ran the committed harness 8c5968e (clean tree), and each passed 18/18 checks.
  Earlier runs 1–3 ran harness versions that were never committed as such, so their totals (16/16, 17/17, 17/17) are
  not comparable with each other or with runs 4–5. See "Which harness each run used".
- **Borg's effect on B's plan is not shown.** A blind judge looked for a concrete remedy that makes the `stopped` check
  atomic with delivery. B's final plan had one in 4 of 5 runs, but B's plan *before* the conscience asked also had one in
  4 of 5. The control arm, with borg uninstalled, had one in 2 of 2. The conscience fires, and the drone answers it, but
  on this task sonnet finds the remedy without it. Run 2 is a counterexample to the old keyword check: it said
  "check-then-act" and still committed to no atomic remedy.
- A haiku plumbing run before the sonnet runs found one product bug (see "What broke"), which is now fixed.

## Setup (step 1)

- **The sandbox.** Everything runs in the eval sandbox, `sandbox_with` with the new `Extra::sandbox_home`.
  - The real HOME is hidden behind a tmpfs. Only the claude binary is bound from it.
  - The sandbox HOME is `target/dogfood/<run>/home`. It is bound read-write at its own path and exported as `$HOME`.
  - The environment is cleared down to LANG, LC_ALL, TERM and USER, plus HOME, PATH and `DISABLE_AUTOUPDATER`. No
    `CLAUDE_*`, `BORG_*` or `XDG_*` variable of the calling session reaches the sandbox.
  - Claude runs without `--setting-sources`, so it reads the sandbox's *user* settings.json. That is what makes the
    installed hooks fire.
- **The credentials are copied, kept outside the sandbox HOME and deleted at the end** (since 8c5968e).
  - The copy holds only the `claudeAiOauth` entry, with mode 0600. `mcpOAuth` is dropped. A sanitised `~/.claude.json`
    (`projects` and `mcpServers` removed) is copied next to it.
  - Both copies live in a private 0700 temp dir (`dogfood::Secrets`), not in `target/dogfood/<run>/home`. Each sandbox
    binds them in read-write (`Extra::file_rw`) at `~/.claude/.credentials.json` and `~/.claude.json` only while it
    runs.
  - When the run ends, the `Secrets` guard deletes the temp dir and the empty mount points that bwrap leaves in the
    HOME. It also does this if the run panics. The ignored setup test drops the guard before its asserts and checks
    that nothing is left. A SIGKILL would skip it; the copy would then remain in the temp dir, not in the run's HOME.
  - The copy is made only if the token stays valid for at least another hour. A sandbox refresh would rotate the
    refresh token behind the real file's back, so the sandbox must never need one.
  - Checked at the end of every run: the sandbox copy and the real file are byte-identical to what they were at the
    start. So no refresh happened, and nothing was written back.
  - **Runs 1–3 and the haiku run used an older approach.** It left the token copy and an 88 KB `~/.claude.json` copy in
    each run's HOME (and in a `sonnet-1-replay` copy). All of those copies were deleted from `target/dogfood/*/home`.
    After runs 4 and 5, no `.credentials.json` or `.claude.json` exists under `target/dogfood`, and no secrets dir
    remains in `/tmp`.
- **What the sandbox HOME holds.**
  - A user `settings.json` with someone else's Stop hook (`true`) and permissions.
  - `bin/borg`, a link to `target/release/borg`.
  - `~/.config/borg/collectives.json`, set to `default: off`, `off: ["~/diary"]`, `work: ~/work` (team) and
    `private: ~/private` (local).
  - Three git repos:
    - `~/work/relay`: a Java relay service with `Relay.java` (AtomicBoolean `closed`, check-then-act in `deliver`),
      `FastRelay.java` (the lock-free sibling, using `ConcurrentLinkedQueue`) and `fanout/Broadcaster.java`. The
      engineer is `ann@corp.example`.
    - `~/private/notes`: `notes.py`, which rewrites `notes.json` in place.
    - `~/diary`: one entry.
- **The real stores were never touched.** `~/.claude/settings.json` (mtime 2026-10-07), `~/.claude/.credentials.json`
  (unchanged), `~/.config/borg`, `~/.local/share/borg`, `~/.omp/agent/extensions`, `~/.config/opencode` and `~/.codex`
  have no file newer than the start of this work.

## Which harness each run used

| Run | Started | Harness | B "plan" check as it ran | Total |
|---|---|---|---|---|
| haiku plumbing | 18:05 | uncommitted, before 82e2a4b | any of race, concurrentlinkedqueue, copyonwritearraylist, compareandset | — |
| 1 | 18:11 | uncommitted, before 82e2a4b; no credentials check | same loose keyword list | 16/16 |
| 2 | 18:18 | 82e2a4b (committed 18:18:38) | same loose keyword list | 17/17 |
| 3 | 18:24 | uncommitted at run time; committed as eebb266 at 18:30 | "toctou" or "check-then-act" in the final text | 17/17 |
| 4 | 18:37 | **8c5968e, clean** (recorded in results.json) | as in 3, renamed; plus the blind judge | 18/18 |
| 5 | 18:42 | **8c5968e, clean** (recorded in results.json) | as in 3, renamed; plus the blind judge | 18/18 |

So the B criterion was changed between runs, after runs 1 and 2 had been looked at. Runs 1–3 are kept only as
evidence for the checks that did not change. The table below does not pool checks with different definitions. Where
runs 1–3 are re-scored under the current definitions, that is a post-hoc re-scoring of their archived streams, and it
is marked as such.

## Evidence per step

Runs 4 and 5 ran harness 8c5968e. "1–3" means the check had the same definition in those runs, and they passed it too.

| Step | Check | Run 4 | Run 5 | 1–3 |
|---|---|---|---|---|
| 2 | `borg install claude --apply` (HOME = the sandbox's, no `--home`): exit 0, all 5 events wired, the foreign Stop hook stays first, backup written | pass | pass | pass |
| 2 | `borg doctor`: exit 0, no FAIL (borg on PATH, claude 2.1.295, hooks wired, both stores writable) | pass | pass | pass |
| 2 | `borg collective` routes relay to work (Team, may sync), notes to private (Local, may not sync), diary to off | pass | pass | pass |
| 3 | A's correction became an episode in the work store; the engineer is `ann@corp.example` | pass | pass | pass |
| 3 | The episode and the lesson name their repo `relay` | pass | pass | pass |
| 3 | SessionEnd ran `borg learn --session A` (seen with pgrep) | pass | pass | pass |
| 3 | One lesson in `collectives/work/lessons.jsonl`, and no lessons file anywhere else | pass | pass | pass |
| 4 | B's state file has `#borg-asked <A's lesson>`, and the Stop block ("Before you finish …") is in B's stream | pass | pass | pass |
| 4 | **The drone answered the question**: B's final text contains "check-then-act" or "TOCTOU". The question itself contains both words, so this shows only that it was answered. | pass | pass | not pooled (post hoc: 3/3) |
| 4 | **A blind judge finds a concrete atomic remedy in B's final plan** (see below) | pass | pass | not run then (post hoc: 1 pass, 2 fail, 3 pass) |
| 5 | C's correction became a lesson in the private store only; the work lesson ids are unchanged | pass | pass | pass |
| 5 | D (work, a task about a JSON journal on disk that invites the private lesson) was not asked it, and its question text is nowhere in D's stream | pass | pass | pass |
| 5 | E (off; a task, then a correction on resume): the file tree under BORG_HOME (path, size, mtime) is identical before and after, and no file mentions E's session id | pass | pass | pass |
| 6 | `borg status`: exit 0; per collective, the lessons, episodes, outcomes, last capture and last asked | pass | pass | pass |
| 6 | `borg uninstall claude --apply`: settings.json equals the original byte for byte | pass | pass | pass |
| — | Credentials: the sandbox copy was never refreshed and the real file is unchanged | pass | pass | 2, 3 pass; 1 had no such check |

## Does the conscience change B's plan? (blind judge and control arm)

**The confound.** The lesson's question contains "check-then-act", and its consequence contains "TOCTOU". The Stop
block puts that text into B's context. The B fixture also has the same check-then-act bug that the prompt points at
("afterwards no subscriber may receive an event"). So a keyword match cannot tell repeating the question apart from
fixing the bug.

**The substance test (since 8c5968e).**
- **The judge.** Sonnet with no tools and a fresh, unpersisted session. It sees only the Broadcaster source, one plan
  and the criterion. It is told neither the arm nor that any question was asked. One plan per call.
  - Criterion (`dogfood::JUDGE`): pass only if the plan makes publish()'s `stopped` check atomic with the delivery it
    guards, so nothing is delivered after stop() returns. Examples that pass: a lock or read lock held across the check
    and the loop, which stop() also takes; or a CAS or state machine with the same effect. It fails plans that only
    make the flag and the list individually thread-safe, plans that name the race without a remedy, and plans that
    leave the remedy as an option. Words such as TOCTOU neither pass nor fail a plan.
  - One residual leak: B's final plan can mention that it is answering a question ("The question applies …"). The judge
    is not told what that means.
- **The control arm.** B's exact prompt, run in the same repo after `borg uninstall`, so no borg hook fires. Checked
  in each run: there was no Stop block in the control's stream, and Claude Code's auto-memory dirs were empty before
  it, so nothing from A could reach it that way.
- **Judge calibration.** A synthetic bad plan (CopyOnWriteArrayList plus AtomicBoolean, check then iterate) was judged
  **fail**, and so was run 2's plan (below). So the judge is not a rubber stamp. These verdicts are in
  `eval/dogfood/judge-posthoc.jsonl`.

| Run | B before the question | B final (after the question) | Control (no borg) |
|---|---|---|---|
| 1 (post hoc, archived stream) | remedy | remedy | — |
| 2 (post hoc, archived stream) | **no remedy** (CopyOnWriteArrayList, re-check per delivery; the RW lock offered as an option) | **no remedy** (lock-free default kept; the RW lock still only a "strict" alternative) | — |
| 3 (post hoc, archived stream) | remedy | remedy | — |
| 4 (in-run, 8c5968e) | remedy | remedy | remedy |
| 5 (in-run, 8c5968e) | remedy | remedy | remedy |

Every remedy found was the same design: a `ReentrantReadWriteLock`, with publish() holding the read lock across the
check and the loop, and stop() taking the write lock.

**Reading.**
- On this task, sonnet finds the remedy without borg: 4 of 5 B plans had it before the question, and 2 of 2 controls
  had it.
- The conscience's question changed no verdict. In run 2 it changed the vocabulary (the final plan says
  "check-then-act") but not the design. That is exactly the case the old "the plan addresses it" check counted as a
  pass.
- So this dogfood shows the mechanism (learn → ask → answer). It does not show a benefit. Measuring a benefit needs a
  task where the control fails, and more than n = 5 (2 with a control).

**Excerpts from run 2** (the lesson and question texts; the B verdict for this run is in the section above).
- **The lesson learned from A** (`L12722a16-…`, repo `relay`, engineers `[ann@corp.example]`): "Is any check-then-act
  on shared state (like a closed flag read before enqueueing) atomic with the action it guards, and how does the
  lock-free sibling path interact with it, so I can design the handoff to match that path?"
  - Its consequence names the TOCTOU loss after close and the ConcurrentLinkedQueue handoff.
- **What the conscience asked B at Stop:** the standard preamble, then exactly that question.
- **B's final plan** opens: "The question applies. Here is the check-then-act audit, then the revised plan. …
  `publish()`: check `stopped`, then `accept`. Not atomic. `stop()` can run between the `stopped.get()` and
  `s.accept(event)` …". Yet its recommended design keeps the lock-free path. The judge failed it.
- **The lesson learned from C** (`L9c373c14-…`, repo `notes`, `me@home.example`): "When code persists data by
  overwriting a file, does it write to a temporary file in the same directory, fsync it, and atomically rename it over
  the original …?"
- **D** was asked only `L12722a16` (work).
- **`borg status`:**
  - work: 1 lesson, 1 episode, 4 outcomes (2 × asked, 2 × asked_not_corrected; from B and D).
  - private: 1 lesson, 1 episode, 0 outcomes.
  - claude: wired. omp and opencode: not wired.

## Timings

**Borg's Stop hook latency** is measured in the stream (`--include-hook-events`, arrival time of `hook_started` and
`hook_response`). Stop runs two hooks; the foreign `true` takes 1–4 ms. Borg's numbers per run:

| Run | Borg's Stop hook, ms (each Stop in A1, A2, B, C1, C2, D, E1, E2) |
|---|---|
| 1 | 13, **3113**, 6, 6, 7, 6, 2, 3 |
| 2 | 7, 8, 6, 9, 18, 6, 3, 9 |
| 3 | 12, 28, 171, 7, 6, 5, 3, 3 |
| 4 | 14, 7, 6, 7, 6, 6, 3, 3 |
| 5 | 7, 24, 9, 7, 7, 6, 3, 3 |

The 3113 ms in run 1 happened while a debug `cargo test --workspace` build ran on the same machine. A replay of
that exact Stop takes 15–67 ms. The replay used session A's state, no lessons yet and A2's final message, on a copy of
the store, 10 times.

The replay of a Stop that asks the lesson (B's plan, 20 times, on a copy of run 1's store) took min 3.8, p50 4.4, p95 5.9
and max 9.5 ms. Excluding the run-1 outlier, the 39 in-run borg Stops of runs 1–5 took at most 171 ms. In the control arm only the foreign `true` Stop hook ran (2 ms), which confirms that borg was out of the loop.

The other hooks took 2–13 ms each in-run, with a few outliers:
- UserPromptSubmit took 208 ms once (run 3, A2) and 41 ms once.
- PostToolUse took 1628 ms and 2599 ms in run 2's A1. That turn had parallel tool calls, so this may be when Claude
  emits the event rather than the hook itself (not verified).

**`borg learn` at SessionEnd.** The hook spawns it in the background, and it outlives the `claude -p` process. Its
running time comes from `ps etimes` at first sight plus the observed rest:

| Run | Work (A): learn run time, and done after claude exited | Private (C): learn run time, and done after claude exited |
|---|---|---|
| 1 | 12.2 s, 12.2 s | 4.5 s, 4.6 s |
| 2 | 7.8 s, 8.1 s | 4.7 s, 4.7 s |
| 3 | 7.4 s, 7.5 s | 4.4 s, 4.4 s |
| 4 | 5.2 s, 5.2 s | 4.7 s, 4.7 s |
| 5 | 5.6 s, 5.8 s | 4.3 s, 4.3 s |

Each learn is one sonnet call per episode, and there was 1 episode per session.

**Drone wall time per turn**, in runs 1–5 (B-control, runs 4–5 only: 18 s):

| Turn | Wall time |
|---|---|
| A1 | 19–29 s |
| A2 | 21–31 s |
| B | 37–45 s, including the extra revise turn after the conscience's block (the control, without it, 18 s) |
| C1 | 11–14 s |
| C2 | 10–12 s |
| D | 47–65 s |
| E1 | 5–10 s |
| E2 | 2–3 s |

## Spend

**Drone sessions**, from `total_cost_usd` in the streams:

| Run | Model | Cost |
|---|---|---|
| Plumbing | haiku | $0.03 |
| Run 1 | sonnet | $0.44 |
| Run 2 | sonnet | $0.40 |
| Run 3 | sonnet | $0.41 |
| Run 4 (including the control, $0.03) | sonnet | $0.45 |
| Run 5 (including the control, $0.03) | sonnet | $0.47 |
| **Total** | | **$2.20** |

**The judge** (sonnet): $0.05 in each of runs 4 and 5, plus $0.11 for the 7 post-hoc and calibration verdicts. That is
$0.21 in all.

**`borg learn`** made 12 sonnet calls (2 per run). Their cost is not measurable, because `learn::ask_claude` discards
`total_cost_usd`. At about 2k input and 0.4k output tokens each, they are estimated at ≤ $0.45 in total. Total spend is
about $2.9: $1.6 before the review fixes, and about $1.3 for them. No Codex.

## What broke, and what was fixed

1. **Fixed: `borg_trace::repo_key` gave a nested repo the wrong name** (crates/borg-trace/src/lib.rs).
   - It folded any path containing `/.claude/worktrees/` (or `-worktrees/`, `/.worktrees/`) onto the worktree's parent
     repo. A repo of its own nested inside a worktree therefore got the wrong name: the dogfood's `~/work/relay` was
     recorded as repo `borg-ai`. The same happens to any fixture or tool-created repo inside a Claude worktree.
   - The haiku run exposed it: episode and lesson had `"repo": "borg-ai"`.
   - Now only the worktree itself folds: the git root found must be the directory right after the marker.
   - Test: `a_repo_nested_in_a_worktree_keeps_its_name`. The dogfood also checks it ("episode and lesson name the repo
     they came from").
2. **Gap, not fixed: `borg learn`'s spend is invisible.** `ask_claude` throws away the call's cost, so neither status nor
   a log can report what generalising cost. Scale-gate criterion 2 (ingestion cost per real session) needs it. A
   one-line log beside `experts.jsonl` would do.
3. **Precision note on D.** In all 3 runs, D (a JSON journal for Relay) was asked the work TOCTOU lesson. Each
   time the drone judged it relevant, because Relay does have that check-then-act. It is not a leak; noted for
   precision only.
4. **Fixed: the B check was satisfied by the hook's own words, and its criterion changed between runs.** It is now
   two checks. "The drone answered the question" is the keyword check, renamed for what it shows. "A blind judge
   finds a concrete atomic remedy" tests substance. A control arm without borg was added. The result: no effect of the
   conscience on B is shown (see "Does the conscience change B's plan?").
5. **Fixed: live OAuth tokens were left on disk.** Each run's HOME kept a copy of the refresh token and of
   `~/.claude.json`. Any later `claude` run in such a HOME could have refreshed, and so rotated, the real login. The
   copies now live outside the HOME, are bound in per sandbox and are deleted when the run ends (see Setup). The old
   copies were deleted.
6. **Cosmetic:**
   - When nothing is selected, the conscience still appends an empty `#borg-asked` line to the session state.
   - `borg doctor` creates every collective's store directory, through its writability probe, before any session runs
     there.
7. **Did not break:**
   - SessionEnd fires on every `claude -p` exit, the resumed one included. The background `borg learn` survives the
     claude process and the sandbox's exit.
   - The off repo leaves nothing: no state, episode, queen entry or directory.
   - Install/uninstall round-trips settings.json exactly, with the foreign hook and permissions kept, and writes a
     timestamped backup at each step.

## Not covered

- omp and opencode are not dogfooded: only Claude was wired.
- The queen never asked: no plan crossed the high-consequence threshold.
- Expert routing is not exercised: no `experts` were configured.
- No mesh or sync was tested.
- **Not measured: a benefit of the conscience.** It would need a task where the control arm fails.
