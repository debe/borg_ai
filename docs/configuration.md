# Configuration

[← README](../README.md)

- [Collectives](#collectives)
- [Who hears a lesson](#who-hears-a-lesson)
- [Storage](#storage)
- [System map](#system-map)
- [What `borg install` writes](#what-borg-install-writes)
- [Import what you already have](#import-what-you-already-have)
- [Vectors](#vectors)
- [Environment variables](#environment-variables)

## Collectives

`~/.config/borg/collectives.json` (or `BORG_CONFIG`) routes every repo path:

```json
{
  "default": "off",
  "off": ["~/work/payroll-secrets", "~/personal/journal"],
  "collectives": [
    { "name": "platform-team", "paths": ["~/work"], "share": "team" },
    { "name": "private", "paths": ["~/personal"], "share": "local" }
  ],
  "engineers": [
    { "name": "alex", "emails": ["alex.doe@example.com", "alex@home.example"] }
  ]
}
```

Borg applies the first rule that matches:

1. A path under `off` wins over everything.
2. The collective with the longest matching path.
3. `default`: `"off"` or a collective's name.

Paths match whole components: `~/work` does not match `~/work-old`.

| Key | Meaning |
|---|---|
| `share` | `"team"` (default) syncs over the mesh; `"local"` never leaves the machine |
| `engineers` | Optional. Merges one person's git emails into one author for trust, `borg status` and `borg top`. Never synced |
| `relay` | Optional, per collective. How the mesh crosses NAT; see [Relays](team-sync.md#how-peers-find-each-other) |
| `experts` | Optional, per collective. Local checkouts of dependencies to cite at the turn end (`crates/borg/src/expert_route.rs`) |
| `personal` | Optional, top level. The `local` collective that receives personal lessons; needed only when you have several |

An invalid config (bad JSON, two collectives sharing one store, an email under two engineers) routes
**everything off**, and `borg doctor` reports it.

```sh
borg collective ~/work/some-repo   # -> platform-team (Team, may sync: true) store ~/.local/share/borg/collectives/platform-team
borg collective ~/personal/notes   # -> private (Local, may sync: false) ...
```

## Who hears a lesson

`borg learn` gives every lesson a **reach**:

| Reach | What it is | Where it goes |
|---|---|---|
| team | An engineering concern about the code, design or verification | The collective it was learned in, delivered to everyone |
| repo | A convention of one repo, such as its branch or deploy workflow | The same collective, delivered only in that repo |
| personal | How you like the agent to work: commit style, AI attribution, when to branch or push, tone | Your private collective, which never syncs; without one it is dropped |

The model decides by asking: would a teammate working on the same code get this correction too?
Imported team documents are always team; their repo conventions are scoped by system already.

Personal lessons go to the `local` collective named by `personal`, or to your only `local` collective.
A personal lesson already shared with the team is withdrawn there: a newer version replaces its text
with a tombstone that is never delivered, and peers receive that version too.

**Clean up lessons learned before reach existed:**

```sh
borg reach                              # dry run: one model call per lesson learned from sessions (~$0.02 each), writes a plan
borg reach --apply --plan <store>/reach-plan.json
```

The plan lists what moves to your private collective, what becomes repo-only, and what the model was
unsure about. `--apply` makes the sure moves and turns the unsure ones into queen questions, which you
answer with `borg queen`. Answering one moves the lesson; it writes no new lesson.

## Storage

Each collective keeps plain files in `~/.local/share/borg/collectives/<name>/` (`BORG_HOME` moves the
root): `episodes.jsonl`, `lessons.jsonl`, `outcomes.jsonl`, and, when used, `vectors/`, `mesh/`, `queen/`
and `presence/`. Per-session scratch state lives in `/tmp/borg` (`BORG_STATE`).

## System map

The system gate keeps lessons scoped to other systems out of a repo that maps to a known system. Put
your map in `~/.config/borg/systems.json` (or `BORG_SYSTEMS`), in the format of
[`crates/borg/data/systems.json`](../crates/borg/data/systems.json). That example applies when you have
none.

## What `borg install` writes

`borg install` edits your **user** harness config. It prints a diff until you pass `--apply`, and backs
up every file it changes. A second run changes nothing. `--home DIR` installs into another home, for
testing.

- **Claude Code:** one hook per event in `~/.claude/settings.json` (or `$CLAUDE_CONFIG_DIR`):
  `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `Stop` (90 s timeout) and
  `SessionEnd`. Each runs `<path to borg> hook claude <event>`. Your other keys and hooks stay.
- **omp:** copies the adapter to `~/.omp/agent/extensions/borg.ts`.
- **opencode:** copies the adapter to `~/.config/opencode/plugins/borg.ts`.

The adapters are compiled into the binary, so an installed `borg` needs no checkout.

## Import what you already have

**Past sessions.** Mine your Claude Code and omp transcripts for corrections:

```sh
borg assimilate                         # dry run: what it found, per collective; no model calls
borg assimilate --apply --limit 50      # learn the newest 50 new episodes (model calls)
borg assimilate --apply --since 2026-09-01
```

It reads `~/.claude/projects` and `~/.omp/agent/sessions` (`--claude DIR`, `--omp DIR`). Each episode
routes by the directory it happened in, so off repos contribute nothing. A second run adds nothing
twice. `BORG_ASSIMILATE_SKIP=word1,word2` drops every episode that mentions one of those words, such as
an abandoned project's name.

**Team documents.** Turn a git checkout of written team knowledge into scoped lessons:

```sh
borg assimilate --from-dir ~/work/handbook --plan /tmp/plan.json           # model calls; writes only the plan
borg assimilate --from-dir ~/work/handbook --apply --plan /tmp/plan.json   # applies that plan; no model calls
```

**The dry run calls the model** (one call per chunk, 4 in parallel), so review the plan before you apply
it. Borg reads tracked Markdown files. The default globs fit a handbook layout:
`skills/*-knowledge/...`, `docs/*retrospectives/*.md`, `docs/**/*coverage-audit*.md`, and decision
markers in `docs/**/*.md`; change them with `--knowledge`, `--retros`, `--families` and `--decisions`.
Lessons go to the collective the directory routes to, unless you pass `--collective NAME` or
`--for PATH`. Open decisions go to the [queen's queue](commands.md#answering-the-queen).

## Vectors

Vectors find lessons that share no rare word with the work: paraphrases, German work against English
lessons, renamed identifiers. Only the turn-end check uses them.

```sh
borg vectors --fetch      # downloads the model once (~490 MB, pinned revision, sha256 checked)
borg vectors --rebuild    # indexes the store the current directory routes to
borg vectors              # model and index status
```

The model is Model2Vec potion-multilingual-128M. The index updates itself after `borg learn` and
`borg assimilate --apply`. No hook downloads the model; without model or index, Borg matches words
only.

## Environment variables

All are optional.

**Paths and binaries**

| Variable | Default | Effect |
|---|---|---|
| `BORG_CONFIG` | `~/.config/borg/collectives.json` | The collectives config |
| `BORG_HOME` | `~/.local/share/borg` | Root of the stores |
| `BORG_STATE` | `/tmp/borg` | Per-session scratch state |
| `BORG_BIN` | `borg` on `PATH` | The binary the omp and opencode adapters run |
| `BORG_RELAY` | the collective's `relay` | Mesh relay for every collective: `default` (n0), `disabled`, or URL(s) |

**Learning and delivery**

| Variable | Default | Effect |
|---|---|---|
| `BORG_LEARN` | on | `off`: record no episodes or outcomes (for eval drones) |
| `BORG_LEARN_MODEL` | the harness's | Model for `borg learn`, `assimilate`, `reach` and `facets` (see *Model runner*) |
| `BORG_LEARN_TIMEOUT_S` | `300` | Time limit of one learning-side model call |
| `BORG_ASSIMILATE_JOBS` | `4` | Parallel model calls in `assimilate --from-dir` |
| `BORG_ASSIMILATE_SKIP` | none | Comma-separated words; matching episodes and documents are skipped |
| `BORG_EARLY` | on | `off`: no early delivery |
| `BORG_SYSTEM_GATE` | on | `off`: no system gate |
| `BORG_SYSTEMS` | `systems.json` next to `BORG_CONFIG` | Your system map |
| `BORG_CONSCIENCE` | the routed store's `lessons.jsonl` | Read lessons from another file |

**Model runner**

Borg's own model calls run headless through a CLI: the harness the drone runs in first (the hook
passes it on as `BORG_HARNESS`), then every other installed one of `claude`, `omp`, `opencode`, until
one answers. A model name with a runner prefix pins the runner: `claude:opus`, `omp:` (omp's default),
`omp:<model>`, `opencode:github-copilot/gpt-6.1-sol`, `codex:gpt-5`. A name without a prefix is a
Claude model (default `sonnet`); omp and opencode then use their own configured default model.
Each call runs without tools, plugins or extensions (opencode with every permission denied), and is
killed with everything it started when its time limit passes or borg itself is killed.

**Turn-end check**

| Variable | Default | Effect |
|---|---|---|
| `BORG_MATCH` | model | `bm25`: word matching only, no model call |
| `BORG_PREFILTER` | `8.0` | Threshold of the free gate; `off` always calls the model |
| `BORG_STOP_GATE` | word gate | `edits`: call the model only for turns that edited files or changed state |
| `BORG_STOP_POOL` | whole pool | `shortlist`: show the model only the word-matching shortlist |
| `BORG_STOP_POOL_TOKENS` | `12000` | Token budget of the lesson pool |
| `BORG_STOP_FORMAT` | compact | `classic`: the older prompt layout |
| `BORG_STOP_GRADED` | on | `off`: no grades; any applying lesson blocks |
| `BORG_PREDICT_MODEL` | the harness's | Model for the turn-end check (see *Model runner*) |
| `BORG_PREDICT_TIMEOUT_S` | `45` | Time limit of the turn-end call |
| `BORG_STOP_BUDGET_MS` | 60 s | Stop-hook budget when the harness gives none (the adapters set it) |
| `BORG_VECTORS` | on | `off`: no vector recall |
| `BORG_VECTOR_MODEL` | `~/.local/share/borg/models/potion-multilingual-128M` | Model directory |
| `BORG_VECTOR_K` | `10` | Vector hits added to the shortlist |
| `BORG_VECTOR_URL` | Hugging Face | Mirror for `borg vectors --fetch` |
| `BORG_EXPERT_MODEL` | `sonnet` | Model of the optional expert drone |

**Queen and notifications**

| Variable | Default | Effect |
|---|---|---|
| `BORG_QUEEN_ASK` | on | `off`: the drone asks no queued question at a session's first prompt (also off with `BORG_LEARN=off`) |
| `BORG_NOTIFY` | on | `off`: no desktop notification (`notify-send`) for new questions |

**Experimental**, off by default. Most failed a pre-registered test and stay for further experiments.

| Variable | Effect when `on` |
|---|---|
| `BORG_QUEEN` | The turn end may queue questions for the engineer (failed its test) |
| `BORG_CONCEPTS` | Boost lessons that share engineering concepts with the work; `BORG_CONCEPTS_BETA` (0.5) sets the strength |
| `BORG_FACETS` | `learn` and `assimilate` write facets (failed its test) |

Evaluation only: `BORG_BEFORE`, `BORG_CRITIC`, `BORG_ONLY`, `BORG_EXPERT_CEILING`, `BORG_VECTORS_TRACE`.
