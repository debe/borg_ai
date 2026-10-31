# How one session learns

[← README](../README.md)

Everything on this page works in one plain interactive session, with no team. A team adds other
people's lessons ([Team sync](team-sync.md)) and changes nothing else a session does, on every harness and
in both [delivery modes](delivery.md) (`crates/borg/tests/plain_cli.rs`).

A **drone** is one agent session. A **lesson** is what a correction becomes: a question the agent should
ask itself, the situations where it applies, and the consequence it prevents. A **collective** is one
shared memory for a set of repos and engineers.

![A correction becomes a lesson, changes another drone's work, reaches a colleague over the mesh and becomes long-term memory](img/one-correction.svg)

- [Sources of lessons](#sources-of-lessons)
- [Outcomes and the usefulness belief](#outcomes-and-the-usefulness-belief)
- [Saying no](#saying-no)
- [Memory](#memory)
- [Reach and collectives](#reach-and-collectives)
- [The queen](#the-queen)
- [Also included](#also-included)

## Sources of lessons

**Corrections.** Borg records an *episode* when you push back on the agent ("no, use a channel") or
reject a tool call with a reason (Codex records no rejected calls). At session end, `borg learn` makes one
model call per episode and writes a lesson. A new correction on the same point updates the existing
lesson.

**Answers to the agent's questions.** When the agent asks you (Claude Code `AskUserQuestion`, omp `ask`,
opencode `question`, Codex `request_user_input`), your answer is an episode too, marked `via: answer`:
the questions and their options are what the agent proposed, your picks, typed words and notes the
correction. The learn call decides whether the answer holds a rule beyond its task; a pick that only
fits the task is no lesson. Answers are counted apart from corrections.

**Where they come from.** The hooks capture corrections live. Each harness's own transcript is read too:
at every turn end (Claude Code, omp, Codex: only what the turn added), at session end (all four, opencode
through its CLI export), and by every `borg learn` for a session left open and idle for a day. One
dedupe keeps the hook's episode and the transcript's as one. So a correction still arrives when a
harness stops telling a hook (Claude Code 2.1.312 sends no `PostToolUseFailure` for a rejected tool
call), and `borg doctor` warns when the transcripts name more than Borg holds
([The harness's own transcript](harnesses.md#the-harnesss-own-transcript)). `borg assimilate` reads the
same transcripts for the past ([Import](configuration.md#import-what-you-already-have)).
`BORG_TRANSCRIPT_CAPTURE=off` leaves the hooks alone.

**Team documents.** `borg assimilate --from-dir` condenses a checkout of written team knowledge into
scoped lessons, and its open decisions into queen questions ([Import](configuration.md#import-what-you-already-have)).

**Decision → fix links (regrets).** Some choices look fine and break weeks later. At session end Borg
links each landed commit to the earlier decisions whose lines it fixed. With `BORG_REGRESS=learn` a fix
becomes a lesson for the decision point; by default links are only recorded. How links are found, where
they are written and how to check them first: [Decision → fix links](configuration.md#decision--fix-links).

**Review → fix.** Most bugs never reach a commit. A workflow builds, a reviewer finds a problem, and a
fix step fixes it, all inside one session. `borg review-fix` reads the sub-agent runs of Claude Code
(workflows, and Agent-tool sub-agents; one a workflow's agent started joins that workflow's run), omp
(task agents), opencode (subagents) and Codex (spawned agents) at 0 tokens. It finds each reviewer
finding that a later fix step of the same run fixed and no verifier rejected. At session end those
findings become episodes, and `borg learn` words each as a question for the moment of building. On by
default; `BORG_REVIEW_FIX=off` turns it off. opencode, the Agent tool and Codex have no structured
output, so only reviewers told to answer in JSON yield findings there, and the Codex reader is built from
codex's serde names, not yet checked against a real spawned rollout. See
[Review → fix findings](configuration.md#review--fix-findings).

**Vetoes.** The reason of a `borg no` becomes a lesson at the next session end ([Saying no](#saying-no)).

## Outcomes and the usefulness belief

Each delivered lesson gets an `asked` outcome, and what follows is recorded against it: `changed` or
`ignored` (judged by the next turn-end model call, see [Delivery](delivery.md)), `corrected` anyway,
`vetoed`, `regretted`, `tested`. These outcomes form each lesson's usefulness belief, which ranks it
against others for the model's pool and lets an unused one fade. Which of the lessons the model grades
are asked, handed over free or left for later is the selection policy's call: the fixed rule by default,
or under `BORG_SELECT=learned` a policy that learns from the same outcomes and is refitted at the end of
every `borg learn` ([Which lessons are asked](delivery.md#which-lessons-are-asked)).
`borg debug memory <lessons.jsonl>` shows each lesson's belief, activation and state. `borg debug ope`
replays the hand-set weights offline against the logged asks, with their asking probability, and
reports; it never changes them. `borg debug select` does the same for the selection policy.

At session end every delivered lesson that no correction followed settles as `asked_not_corrected`,
with the reason when it had no verdict (no model call came after it, the judge's tries or slots ran
out, its call failed). That mark is censored: it counts as unknown, never as ignored, and a verdict
counts on its own. An unclear verdict and a placebo's result (a lesson judged blindly, never shown) are
kept for measurement only: no belief, trust, statistic or team line reads them.

| Outcome | Weight in the belief | Set by |
|---|---|---|
| a correction after the lesson was asked | 2 | design |
| `vetoed` | 3 | design, no eval yet |
| `regretted` | at most half a correction | design; on while its offline check is inconclusive, see [Evidence](evidence.md#regret-credit) |
| `tested` | 0.5 (`TESTED_W`) | design, not evaluated |
| `asked_not_corrected`, an unclear verdict, a placebo's result | none | censored or measurement only |

**Regret reaches back.** A regret learned as a lesson, from a later fix or a review finding, also counts
against the lessons asked in the deciding session before the decision: asking them did not prevent it.
The lesson it merged into takes the blame if it was asked; otherwise the asked lessons share it equally.
The blame grows with the fix's evidence (one signal 0.5, two 0.75, three 0.875; a review finding 0.5,
confirmed by a verifier 0.75). It costs 0 tokens, each regret counts once, and it weighs at most half a
correction, so a lesson that keeps being asked before decisions that break sinks slowly. One deciding
session's regrets give a lesson at most as much blame as that session had corrections (at least one),
so a long build whose work later took many fixes cannot outweigh every other session. A regret judged
"not a lesson", or one whose lesson went to a private store, blames nothing. Regret credit reads only
this machine's outcome log, so a decision made on another machine finds no asks here. `borg debug memory` and the page show it as
`regretted`; `BORG_REGRET_CREDIT=off` turns it off. Whether the default stays on is decided by a pre-registered
offline check: the belief with the regret term must predict later outcomes at least as well as without it.

**Green tests credit the question.** A session's own test runs count too. When a lesson was asked and the session's tests then failed and passed again, or passed after the
drone changed its work on the lesson's point, `borg learn` gives that lesson a `tested` result at session
end, once per session and lesson, at 0 tokens. The last run must pass; a pass before the question, a
question the drone ignored, or a lesson only handed over as context earns nothing, and a correction or a
veto of the lesson in the session outranks it. Borg tells a pass from a failure by the shell call's
output: Claude Code and Codex send it, and the omp and opencode adapters send the last 4,000 characters
of a shell call's output (opencode with an `Exit code: N` line when it failed). A run whose result the
harness does not show counts as no run. That output stays on the machine; the `tested` outcome syncs to
the team like the others. `BORG_TEST_CREDIT=off` turns it off.

## Saying no

`borg no <reason>` (or a prompt that starts with "borg no") is your veto of what a drone is doing. It
costs 0 tokens and has these effects:

1. Every lesson the drone was shown in the session loses credit, weighed above a correction, so lessons
   that did not prevent the vetoed work sink.
2. The reason becomes a lesson at the next session end, like a correction.
3. When the reason names an invariant of the system, Borg proposes it as a constraint in `borg queen`;
   only you can ratify it. A ratified constraint is a standing lesson: it reaches every later session on
   every harness that takes context (and your team's, through the mesh), as soon as its scope is named,
   an unscoped one at the first prompt, marked as a ratified constraint. It reaches only where the
   lesson it came from reaches: a repo convention's constraint stays in that repo. Nothing blocks work
   on it yet.
4. In the session, the drone is told to stop and not argue the veto back.

The measure is the *repeat-veto rate*: vetoes that repeat an earlier one, which should fall toward zero;
`borg web` shows it. Name a drone on a teammate's machine with `--session` and `borg no` sends it a veto
[message](commands.md#messages). It counts there only when the mesh admitted your machine as an `admin`
or `steer` member; that machine records the credit and the lesson, and its drone hears the veto at its
next prompt or tool call. A veto of work that already merged, from a session the
queen never asked you about, counts as a missed escalation in the queen's attention score, shown in the
queen view; nothing optimises it. See [Saying no](commands.md#saying-no).

## Memory

Use strengthens a lesson; disuse lets it fade. A lesson unused for about 54 sessions of its collective
goes dormant. Only a strong match wakes it, and nothing is deleted. The clock counts sessions, not days,
so a holiday erases nothing. A team lesson ages with every session of the collective, a teammate's
included; a repo convention ages only with sessions started in its repo
([details](configuration.md#who-hears-a-lesson)).

<p align="center"><img src="img/memory.svg" width="480" alt="Activation of a lesson rises at each use and decays between uses; spaced uses keep it above the dormancy line"></p>

### Handing a lesson to an outside store

A team may keep its settled knowledge somewhere else, for example a reviewed skills repo or a
handbook. Once a lesson is held there, Borg delivering it too is a second copy that can drift. Borg
does not know that store and never names it. Three commands hand a lesson over; the agent runs them
for the engineer ([commands](commands.md#learning)):

```sh
borg memory export --json                                   # every lesson's evidence
borg memory absorb L42 --note "skills/review@2026-10-30"    # held elsewhere now
borg memory release L42                                     # Borg delivers it again
```

**The export** is read-only and costs 0 tokens. It holds one record per lesson of the collective the
directory routes to; an `off` directory prints nothing. `--json` prints `{"schema": 1, "collective",
"lessons": [...]}`. A personal lesson (withdrawn to a private collective) is left out. Every text field
but the ids (`id`, `supersedes`, `superseded_by`) and `reach` is redacted, the engineers and an absorb's
`by` too. A field the logs do not hold is omitted, never guessed.

| Field | What it holds |
|---|---|
| `id`, `version`, `repo`, `question`, `situations`, `consequence`, `area`, `scope` | The lesson as stored |
| `reach` | `team` or `repo` |
| `overrides`, `supersedes`, `superseded_by` | What it overrides, the lessons it supersedes, and those that supersede it |
| `source`, `origin`, `support`, `engineers`, `history` | Where it came from, how many episodes back it, who corrected it, and its old versions' wording |
| `memory` | The belief the conscience weighs it by: `alpha`, `beta`, `mean`, `ci95` (the Beta belief's 95% equal-tailed credible interval), the counts `changed`, `ignored`, `corrected`, `vetoed`, `regretted` and `tested`, and `activation`, `consolidated`, `dormant` |
| `breadth` | The distinct sessions that changed their work on it, and the number of distinct repos of those changes, as memory.rs counts them to consolidate it (a change without a repo counts as one more). Team lines carry hashed repos, so this is a count, never names |
| `placebo`, `tail` | Raw counts from this machine's outcome log: how often it was judged as a placebo or shown in the tail sample (`count`), and how often it applied (`applied`). No estimate and no threshold. Omitted when the log holds none |
| `absorbed` | `{note, ms, by, version}`, or `null` |
| `recurrence` | Absorbed lessons only: the episodes merged into it since the absorbed version |
| `last_change_ms` | The latest of its first correction, an engineer joining it, its episodes held here, an outcome naming it, and its last absorb or release |

`--absorbed` lists only absorbed lessons. `--since YYYY-MM-DD` lists lessons whose last change is on
or after that UTC day.

**Absorb and release** append one line per id to `<store>/absorbed.jsonl`: the id, `absorb` or
`release`, the time, the engineer's git email, and for an absorb the note and the lesson's current
version. Any member may write it. A lesson's state is its record that sorts last by time, then kind (a
release after an absorb), then the line's text, so neither line order nor arrival order decides. An id
the store does not hold yet is accepted with a warning and applies when the lesson arrives. A missing
file or a malformed line counts as no absorb, and so does a line stamped more than a minute (the
members' clock skew) ahead of this machine's clock. A command is stamped after the records of its ids
that it sees, so a release always takes effect over the absorb it releases.

**What an absorb does.** Borg stops delivering the lesson on every harness and every path that hands
lessons to a drone: early delivery, standing lessons, live checks and their queue, the stop's check,
the vector shortlist, `borg why`, and `borg debug recall`, `prefilter` and `vectors --query`. A lesson
the absorbed one replaced does not come back in its place. An absorbed lesson that a live lesson
supersedes stays hidden behind it, so the live lesson ranks as it did before. In a team collective the line syncs to the members
(`borg/mesh/15`); in a private one it stays on the machine.

**What it does not do.** The note is an opaque label: Borg redacts, stores, syncs and shows it, and
never reads, resolves or matches it. A note longer than 1,000 characters is refused: name where the
lesson is held, do not copy it. The state is what Borg acts on, not the note. An absorb never widens a
lesson's reach and applies only in the store it was written to. A constraint ratified from the lesson
is its own standing item and is still delivered. A lesson delivered before its absorb still gets the
judge's verdict on that delivery: that is evidence about the lesson, not a delivery.

**Recurrence keeps measuring.** Learning still sees an absorbed lesson. `borg learn` offers it for
merging, so a correction of the same concern can merge into it. Each episode merged after the absorb
counts as a recurrence: a sign the outside copy did not hold. The lesson stays absorbed. `recurrence`
is the latest version's episodes beyond those of the version absorbed. It is omitted when the absorb
recorded no version (the lesson was not in the store yet, or its line has no version) or the store no
longer holds that version's line. `borg status` prints `absorbed N, recurred R` per collective: R is
the number of absorbed lessons with at least one recurrence. borg web's Knowledge view marks absorbed
lessons with their note and recurrence, and `/api/memory` serves the export.

## Reach and collectives

A correction about the code becomes a team lesson. One about how *you* like the agent to work (commit
style, AI attribution, branching) is personal: it goes to your private collective and never reaches the
team. A convention of one repo is delivered only there. See
[Who hears a lesson](configuration.md#who-hears-a-lesson).

Each repo path belongs to one collective, or to none:

- **team** collectives sync to their members over the [peer mesh](team-sync.md);
- **private** collectives (`"share": "local"`) never leave the machine;
- **off** repos are invisible: Borg returns before it reads or writes anything.

A session reads only its own collective's lessons.

## The queen

The queen holds the decisions no lesson answers: open decisions from team documents, proposed
constraints and reach questions. At the first prompt of a session the drone asks you one
question of that session's repo: Claude Code in its question dialog, the other harnesses at the start of
the reply. Answer
in the terminal with `borg queen` and `borg queen ask`. See [Answering the queen](commands.md#answering-the-queen).

When memory cannot answer, the design escalates. Steps 1, 2 and 4 are built. Step 3, asking a live
drone, is not automatic; a session can ask one itself with `borg ask`:

![Escalation: memory, an expert repo, a live drone, the engineer](img/escalation.svg)

## Also included

- `borg why <file>:<line>` shows the redacted reasoning behind a commit, kept as a git note
  (`refs/notes/borg`, never pushed), and both ends of a decision → fix link: "→ fixed in Y" on the
  decision, "← decided in X → fixed here" on the fix.
- `borg top` lists the drones running across the team. `borg tell`, `borg ask` and `borg inbox` let
  sessions and engineers talk, on any harness and across machines; `borg pause` and `borg resume` hold
  or release a drone; `borg listen` wakes an idle session when a message arrives. See
  [Messages](commands.md#messages).
- **Expert repos:** a collective can name local checkouts of its dependencies. A turn end that touches
  one gets at most three cited claims from that checkout, at the pinned version.

![Layers: harness hooks, the borg CLI, the local store and the turn-end check, the peer mesh, team trust](img/architecture.svg)
