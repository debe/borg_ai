# Decision → fix links, take 2: live surfacing + confirm

Status: design, not built yet. It replaces the blame-only approach from Milestone B (commit 5747193) as the main way links are made; that detector stays as the fallback.

## Why

Milestone B links a fix to an earlier decision by blaming the lines the fix **removed or changed**. Teams fix forward, not by reverting, so most fixes look like this:

| Fix shape | Example | Blame today |
|---|---|---|
| Guard or missing branch | `if x.is_empty() { return … }` added above the decision | no link: pure additions blame nobody |
| The 80% case | the decision updated 5 call sites; the fix updates the 6th it missed | no link: no shared lines, maybe no shared file |
| Correction | the decision's own lines change | link |

Engineers write down what broke in two places: the **commit message** and the **agent session**. Borg sees both.

## Direction

1. **Surface:** when an edit lands in a recent decision's footprint, borg tells the drone right then: "this area changed 3 days ago in session X because Y". That's `borg why`, pushed instead of pulled. It helps the fix itself.
2. **Confirm:** the drone or the engineer says "this fixes X". A confirmed link is strong.
3. **Fallback:** at session end, offline scoring catches the links nobody confirmed.

## Steps

### 1. Footprint index

For each borg-noted commit in the window (default 30 days), record:
- the files and line ranges it touched, and the functions they sit in;
- the identifiers it introduced or changed (new fn, renamed field, changed signature).

Build it from git plus `refs/notes/borg`, cache it in the collective's state dir and refresh it incrementally. The live hook only reads the cache; it never runs blame on the critical path.

Reuse: `why::blamed` (why.rs), `reasoning::reasoning_for` and `notes_of` (notes/reasoning.rs), `regress_diff` for hunk parsing. New module: `notes/footprint.rs`.

### 2. Live surfacing

**Hook point:** `on_post_tool` (hook.rs). Today it reads the paths and new text (`touched`, `edit_text`) and, for a test run, the shell output (`tool_response`, `capture_test`), never `old_string` or line numbers. It needs:
- Claude: read `tool_response.structuredPatch` if present, else find `old_string` in the file.
- Codex and omp: parse the `@@` hunks from the patch text.
- opencode: the adapter needs to map `oldString` too (assets/adapters/opencode/borg.ts).

**Budget:** opencode allows about 2 s for post_tool and omp about 5 s, and the hook is synchronous. Match against the cached index only. If the index is cold, queue the lookup in the background and deliver through `.queued` on the next call, the way live mode does. The hook fails open.

**Matching:** an edited range or identifier counts when it falls inside the same function or hunk as a decision, not only on overlapping lines.

**Delivery:** through `free_context` (`additionalContext`), at most one notice per decision per session. Record it as a `#borg-decision <sha>` marker in `.ctx`, which `listed()` already parses and which dedups the way `#borg-early` does.

**Code:** `notes/surface.rs`, which does the matching and writes the notice text. Harness differences stay behind `Caps` and `src/harness/` (arch.sh M1/M2).

### 3. Confirm

There are three ways, all cheap:
- **Drone:** runs `borg fixes <sha|task>`, which appends to `<state>/<session>.confirms`.
- **Commit message:** a `fixes: <sha|task>` line, or a reference to the decision's sha.
- **Engineer's words in the session:** an explicit reference ("the migration from yesterday broke X") read from `.prompts`, matched against the decisions surfaced this session.

At session end, `regress_learn::after_notes` reads the confirmations and builds a `Link` for each confirmed (decision, landed commit) pair, with the new signal `confirmed`. Then:
- `signals::strong()` accepts `confirmed` alone.
- `regress::verdict` waives the `too-young` rule for a confirmed link; `same-session` and `not-ancestor` still apply.
- `regress::record` stores the link in both notes, plus in `reasoning.jsonl` when the collective may sync. `borg why` already shows both directions (why/linked.rs).

### 4. Offline fallback (extends notes/regress.rs)

The candidates are the decisions surfaced this session plus the indexed ones. Score each on:

| Evidence | Catches |
|---|---|
| line overlap (today's blame) | correction |
| adjacency: blame the context lines around a pure addition | guard |
| identifiers shared with the decision | forgotten caller |
| vector similarity between the session's words and the decision's reasoning | all, fuzzy |
| an explicit reference | all, exact |

A link is strong only when one candidate clearly leads **and** the existing fix signals (`regress_signals.rs`) fire.

### 5. Two lesson kinds (notes/regret.rs, learn/regret.rs)

- **`wrong`:** the lines overlap. The lesson asks: "before choosing X here, consider Y".
- **`incomplete`:** only adjacency or shared identifiers connect them. The lesson asks: "when changing `foo`, find every caller, including tests and scripts".

### 6. Docs

Update `docs/configuration.md`, the borg skill (crates/borg/assets/skills/borg) and the CHANGELOG.

## Verification

**Scripted temp repos, one test per fix shape:**

| Fix shape | Expected |
|---|---|
| Correction | strong `wrong` link |
| Guard added | link through adjacency |
| Forgotten caller | link through identifiers, kind `incomplete` |
| Formatter-only change | no link |
| Off repo | no link |
| Local collective | link stays in the notes |

**Other checks:**
- A sim-harness test shows one notice per decision, stays within the opencode budget and fails open on timeout.
- `borg fixes` and a `fixes:` commit line both produce a strong link; `borg why` shows both directions.
- `scripts/check.sh` and `arch.sh` pass. Watch M10: `install.rs` must stay under 40 functions, tests aside. lizard, rust-code-analysis and clippy pedantic show no regressions against the baseline.

**On real repos:** run `borg regress --replay <range>`, extended to report the fix shapes it found. Spot-check precision before setting `BORG_REGRESS=learn`.
