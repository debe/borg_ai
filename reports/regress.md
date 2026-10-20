# Decision → fix links: replay of the detector (2026-10-20)

`borg regress --replay <A..B>` runs the detector that links a fix to the earlier decision whose lines it
changed ([Decision → fix links](../docs/configuration.md#decision--fix-links)) over every commit of a
range, as a dry run. This report replays two histories: this repository's own, and a scripted repo with
known answers. To reproduce the second, run `eval/regress/run.sh [borg binary] [work dir]`. It builds the
repo with `eval/regress/make-repo.sh`, replays it and scores the links against `eval/regress/truth.tsv`.

**Result.**

- **This repository gives no evidence on precision.** It has no Borg notes, so the replay makes no links.
  With `--any-origin` it makes 12 links, all weak. None of its 26 squashed commits is a fix, so there was
  nothing to find.
- **On the scripted repo, 4 of 5 fix links are right, and 4 of the 5 regrets in reach are found.** The
  wrong one is a typo fix in the README, linked to the commit that wrote the line. The missed one is a
  fix said only in the task ("calls fail under load"): it is now a revisited link, by design. The
  engineer's words alone, or a test that went red then green alone, no longer make a fix, because a
  feature built test first and a prompt about the failures a feature handles look the same. An earlier
  version counted a bare "fail" and scored 5 of 6 and 5 of 5 here, at the cost of those false fixes in
  live sessions, which this scripted repo cannot show (it has no test runs or prompts).
- **The replay wrote nothing.** The scripted repo's store was empty afterwards, and neither repo's notes
  changed.
- **Real precision is still unmeasured.** That needs repos whose commits carry Borg notes from real
  sessions, which this public repository does not have. `BORG_REGRESS` therefore stays at `links`
  (record, learn nothing) until someone spot-checks a replay on such a repo.

## Method

The replay takes every non-merge commit of the range as a fix. It blames the lines each one removed or
changed at its parent (`git blame -w -M -C`) and keeps an origin if all of these hold:

- it has Borg reasoning (`--any-origin` drops this rule);
- it comes from another session;
- it is at least 2 days older (`BORG_REGRESS_MIN_AGE`);
- it is an ancestor of the fix and not already linked.

A link is a **fix** when the commit is a revert or its subject reads like a fix, or when a test went red
then green in the session and the engineer's words name a bug. Any other link is **revisited**, a weak
link that is never learned from. A replay has neither the session's test runs nor its prompts, so the
tests signal never fires and the words signal sees only the task: a replay measures the subject and revert
signals only, and a live run can make fix links a replay never shows.

**Scoring.** A link is right when the fix corrected a mistake in that decision: the decision was wrong,
not just later changed. Revisited links are listed but not scored, because they never become lessons.
Recall counts only regrets the detector can reach by design. A decision without a note, or one younger
than 2 days, is out of reach.

## This repository

The range is the root commit to `96ffcb6`, 24 commits scanned. `refs/notes/borg` is empty here. The notes
that sessions wrote stayed on the machines that wrote them (notes are never pushed), and the published
history was squashed.

| Run | Links | Skipped | Time |
|---|---|---|---|
| `--replay` | 0 | 46 origins without Borg reasoning, 8 moved hunks, 5 commits too large | 6.3 s |
| `--replay --any-origin` | 12, all revisited | 34 origins too young, 8 moved hunks, 5 commits too large | 6.3 s |

- **The links.** All 12 are feature commits that rewrote lines of an earlier feature commit, for example
  live mode (`25c527e`) rewriting `main.rs` lines from `d70c651`. That is what a weak link is meant to
  record.
- **No fix links.** None of the subjects reads like a fix, and none is a revert. No fix link is correct
  here: these commits added features, they did not fix decisions.
- **Too large.** Five commits, each over 40 files or 2,000 lines, are skipped whole. Together they are 5
  of the 24 commits. A squashed history hides most of what the detector could see.
- **Too young.** Most origins were less than 2 days older than the commit that changed them, because the
  history was written within a week.

## Scripted repo

The repo is a small price service. Three engineers (Ann, Bo, Cy, all `example.com`) make 20 commits over
31 days. 19 of them are drone commits from 14 sessions, each with a Borg note (session, collective, task,
last proposal). One is a hand-made commit without a note. The repo routes to a team collective. The
history includes the traps a detector meets in daily use:

| Trap | Expected | Got |
|---|---|---|
| rustfmt reflow of `cache.rs` | nothing | nothing (format, trailing comma included) |
| `git mv` of `http.rs` | nothing | nothing |
| A later fix across that move (`fix: back off between retries`) | fix link to the retry decision | fix link |
| A revert of a 3-day-old commit | fix link | fix link (revert) |
| A fix said only in the task ("calls fail under load") | fix link | revisited (words alone are no fix) |
| `Fix typo in the README` | not a regret | fix link (wrong) |
| A follow-up in the same session | nothing | skipped: same-session |
| A fix of a 1-day-old decision | nothing (too young) | skipped: too-young |
| A fix of the hand-made commit | nothing (no reasoning) | skipped: no-reasoning |
| A rename over 45 files | nothing (too large) | skipped: too-large |
| A dependency bump (`Cargo.toml` and `Cargo.lock`) | revisited at most, lock file ignored | revisited (`Cargo.toml` only) |
| A refactor over a fix's line | revisited | revisited |

**Fix links** (the score):

| Fix | Decision | Signals | Right |
|---|---|---|---|
| fix: cache keys ignore the currency | Price cache in front of the upstream lookup | subject | yes |
| Revert "Log every cache miss" | Log every cache miss | revert | yes |
| Fix typo in the README | Price cache in front of the upstream lookup | subject | **no** |
| fix: back off between retries | Retry failed upstream calls | subject | yes |
| fix: cache entries never expire | Price cache in front of the upstream lookup | subject | yes |

Precision 4 of 5. Recall 4 of 5 regrets in reach. The two out of reach (no note, 1 day old) were skipped,
as designed.

**Revisited links:** 6. Four are expected: a config default replaced, a README line extended, a
dependency bump and a refactor. One comes from the hand-made commit, which a live run never takes as a
fix because only a session's own commits are fixes; a replay takes every commit. The last is the fix said
only in the task (above). A revisited link is shown, never learned from.

## Precision notes

- **A fix-shaped subject is the weakest signal.** "Fix typo", "fix lint" or "fix formatting" read like
  fixes but correct no decision. If real replays show many of these, the subject signal should ignore
  them. That change is cheap, but it needs real data first.
- **Collateral lines.** A fix that rewrites a whole function also blames the lines around the bug, and so
  links decisions that were not at fault. A cap of 5 links per session, fix links first, limits the
  damage. The scripted repo has one changed line per fix, so it does not measure this.
- **Redaction cuts the task.** A note's task is redacted before it is written, and `NAME=value` is
  redacted to the end of the line. A task such as "TIMEOUT=30 gives 3 seconds and calls fail" then loses
  the words that make it a fix in a replay. A live run reads the session's prompts and still sees them.
- **Rebases.** A rebased decision keeps its note only with `git config notes.rewriteRef refs/notes/borg`.
  Without it, the decision is no longer linked.
- **Before `BORG_REGRESS=learn`.** Replay the last few hundred commits of a repo with real notes, read
  every fix link with `--explain`, and count the wrong ones. The bar for learning should be the same as for
  corrections: a wrong regret lesson asks a misleading question at every similar decision.
