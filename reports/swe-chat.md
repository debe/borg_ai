# SWE-chat: can memory raise the user's correction before they type it? (pre-registration, 2026-10-12)

Written and committed before any paid model call of this eval (one $0.006 flag test of `claude -p` excepted; it is
in the cost log). Nothing below changes after the first arm call; later changes are dated amendments under it.

## Question

Third-party users, real repositories. Does memory of a repo's **earlier** corrections let a system raise, at the
agent's turn end, the point the user is about to correct? Compared: no memory, a plain log of earlier corrections
(the DreamBench lesson: the plain log is the baseline to beat), Hindsight, and Borg's real Stop path.

## Data

`~/.local/share/borg/eval/swe-chat/`: 159 Claude Code sessions from 15 repos (SWE-chat v2, Entire.io CLI users).
`sample.csv` splits each repo by time: `learn` = first ⅔ of its sessions (107), `test` = last ⅓ (52).
Prompt labels (`prompt_pushback`, LLM-annotated by the dataset authors) come from `prompt_labels.parquet`; prompts
are matched to `transcripts_compact/` by exact timestamp (prompts without a timestamp are dropped).

## Cases (eval/swe-chat/prep.py, seed 20261012, no model calls)

- **Positives: 150.** User prompts in test sessions labelled `correction` or `rejection`, at least 5 words, not a
  filler ("continue", "yes", "go ahead" …), not a slash command or command expansion (`/`, `#`), with an agent
  message before them. 10 per repo by seeded shuffle; repos with fewer would be topped up from a shuffled pool of
  the rest (all 15 have ≥ 10, so exactly 10 each). Realised: 145 correction, 5 rejection.
- **Negatives: 50.** Prompts labelled `non_pushback` in the same test sessions (same filters except the word
  minimum), round-robin over repos.
- **Turn context** (identical for every arm): the transcript from the user's previous prompt up to and including
  the agent's last message before the hidden prompt, rendered as `USER:` / `AGENT:` / `[tool X] arg -> result`
  (results cut to 300 chars), tail-cut to 12,000 characters while always keeping the previous prompt (cut to
  2,000). Median 5.3k chars. The hidden prompt is shown only to the judge.

## Memory (built only from learn sessions)

- **Episodes** (shared by L and B): every learn-session prompt labelled correction/rejection that passes the same
  filters, as a borg `Episode` (task = the latest earlier non-pushback prompt of the session, agent = the agent's
  last 2,000 chars of text before it, correction = the prompt, engineer = the session's `user_id`). 851 exist; to
  fit the budget each repo keeps its **newest 40** (466 in all, 4–40 per repo). Both L and B see only episodes
  dated before the case (`BORG_BEFORE` for B, the same filter for L).
- **L, plain log:** that repo's episodes' correction text, verbatim, one line each (cut to 400 chars), with date,
  oldest first; capped at 24,000 chars per repo (oldest dropped). No model call to build.
- **B, Borg:** the episodes appended to `episodes.jsonl` of one store per repo, then the real `borg learn`
  (`target/release/borg`, `--features vectors`, defaults: Sonnet generalisation, merge, vectors index; facets off as
  default). BORG_HOME = the eval store, BORG_CONFIG = a missing temp file, BORG_STATE = temp. Every `claude` call
  borg makes goes through a logging shim (cost into the same log as the eval's own calls).
- **H, Hindsight 0.10.3** (the gate's container `borg-eval-hindsight`, LLM gpt-5.6-luna via the dedicated Codex
  account at `~/.config/borg-eval/codex`): one bank per repo; each learn session retained as one document
  (user prompts + agent text, no tool output, as its coding-agent plugin retains), timestamp = session start,
  through Hindsight's own extraction and consolidation; measured only after every bank has no pending operation.
  Hindsight's log is scanned for Codex quota errors; a bank built while its LLM failed is rebuilt or excluded.
  **Fallback (approved):** if the ingest of all 107 learn sessions looks heavier than ~20M tokens or fails on quota,
  Hindsight is capped to 5 repos and H is compared on those repos only (stated in the results). Repos are ingested
  in a seeded shuffle (seed 20261012); under the cap H covers the first 5 repos of that order that completed cleanly.
  Hindsight cannot hide learn sessions dated after a case (none of L/B's filtering); the number of such episodes
  is reported.

## Arms (one per case, same turn context)

Shared instruction (system prompt for N, L, H): *"You review a coding agent's turn before the user replies. List
the concerns the user is most likely to raise about this turn in their next message: corrections, objections,
missed requirements, things they will push back on. At most 5, most likely first, each one sentence and specific
to this turn. If you expect no pushback, return an empty list."* Sonnet (`claude -p --model sonnet`, no tools, no
settings), structured output `{"concerns": [..]}`.

- **N (no memory):** the instruction + the turn context.
- **L (plain log):** the same + "Earlier corrections by users of this repository (oldest first)" + the log.
- **H (Hindsight):** the same + Hindsight `recall` for this context (bank of the repo; query = the previous user
  prompt (≤1,500 chars) + the agent's last message (≤2,500 chars); budget mid, max_tokens 2048,
  query_timestamp = case time), wrapped in the plugin's `<hindsight_memory>` framing. Recall has no LLM call; the
  plugin's default reflect is not used (Codex quota), so H is Hindsight's retrieval, not its reflect answer.
- **B (Borg):** the real `borg hook claude` on a copy of the repo's store: one `prompt` event per earlier user
  prompt of the session, one `post_tool` per earlier tool call of the session (last 200), then `stop` with the
  agent's last message; graded Stop (default), `BORG_BEFORE` = case time, `BORG_LEARN=off`, `BORG_QUEEN=off`,
  fake session id, cwd `/tmp/swe-chat/<repo>`. Recorded: the lessons asked in a block (grade 3) and the graded
  2/1 lessons queued as free context. Two views: **B-block** = asked lessons only; **B-all** = asked + free.
  Each lesson is an item (its question + consequence).

## Judge (Sonnet, blind)

One call per case. All items of all arms for that case are pooled, exact duplicates merged, shuffled with a seeded
RNG and numbered; arm names never appear. The judge sees the turn context's tail (≤ 6,000 chars) and the user's
real next message.
- **Positives:** per item `yes` (raising it would have anticipated the point the user then made), `partial` (same
  topic/direction but misses what the user actually wanted) or `no`. Case-level: `genuine` — does the user's
  message push back on / correct the agent's turn (as opposed to a new request or continuation)?
- **Negatives:** per item `false_alarm` (true when the item is a concern this user evidently did not have and
  would not want raised now, given their actual reply) or not.

## Metrics

- **Catch rate (primary):** share of the 150 positives where at least one of the arm's items is judged `yes`.
  Lenient catch: `yes` or `partial`. Wilson 95% CIs.
- **Paired differences** B−L, B−N, H−L, B−H (also L−N), on the same cases; 95% CI by cluster bootstrap over repos
  (10,000 resamples of the 15 repos with replacement; seed 20261012). B uses B-all as primary and B-block as the
  second view; both reported.
- **Secondary:** the same on the `genuine` subset; items per case per arm; for H on the capped repo subset if the
  fallback applies.
- **False alarms:** mean false-alarm items per negative case per arm, and share of negatives with ≥ 1.
- **Cost:** Claude $ per arm (arm calls; B = its Stop consequence-model calls; learning separately) and judge;
  Hindsight Codex tokens for ingest (from its retain usage and operation stats).
- **Examples:** 2 positives B caught (`yes`) that L missed, 2 the other way (if they exist).

## Decision statements (fixed now)

1. **Memory helps:** L − N > 0 with the 95% CI excluding 0 ⇒ a repo's earlier corrections help anticipate the next
   one. Otherwise: no evidence the history helps on this data.
2. **Borg vs the baseline to beat:** B-all − L > 0 with the CI excluding 0 ⇒ Borg beats the plain log. CI includes
   0 ⇒ Borg does not beat the plain log (a tie is not a win). B-all − L < 0 with the CI excluding 0 ⇒ Borg loses
   to the plain log.
3. **Hindsight vs the baseline:** the same rule for H − L.
4. **Borg vs Hindsight:** the same rule for B-all − H.
5. **Noise:** an arm whose false-alarm rate on negatives is more than 2× N's while not beating N on catch is noise.

## Predictions (before any run)

Catch rates low for all arms (N 10–25%): many LLM-labelled "corrections" are new requests, which no reviewer can
foresee. L ≥ N by a few points (n.s. likely). B-all below L: Borg's Stop surfaces generic lessons only when its
consequence model grades them, and blocks rarely; ~60% chance B-all − L < 0. H ≈ L.

## Budget

Claude (Sonnet via `claude -p`): hard cap **$45** for everything (borg learning, B's Stop calls, N/L/H, judge);
every call logged with a running total (`run/claude-calls.jsonl`); the runner refuses calls past $44.50. Order:
learning → B → N → L → H → judge. If the projection exceeds the cap before the judge, negatives are dropped from the
remaining arms first (stated). Codex: Hindsight only, dedicated `CODEX_HOME`, never `~/.codex`.

## Caveats (known now)

- Third-party users and repos: no access to the repos, their conventions or later outcomes; the transcripts are
  the only evidence.
- Pushback labels are LLM-made by the dataset authors and broad (≈ half of all prompts in these sessions); many are
  new requests. The `genuine` subset is a secondary check, judged by the same Sonnet judge.
- Compact transcripts lack system turns and reasoning; parallel tool results may be missing.
- One sample per arm and case (no seeds); Sonnet judges Sonnet's lists (same family).
- B's items are generic lessons, the other arms' are turn-specific sentences written by Sonnet; the judge rates
  each item on its own.
- Borg and L learn from the newest 40 episodes per repo; Hindsight from all learn sessions.

## Amendment 2026-10-12 02:35 UTC (before any H arm call or judge call)

The Hindsight ingest hit the Codex quota (retry at 04:18 UTC) during the 12th repo (`suhaanthayyil/entire-sem`,
partial bank, excluded). 11 repos had finished cleanly (0 quota errors, 0 failed operations, consolidation drained):
84 sessions, 7.69M retain tokens. Per the pre-registered fallback, **H's primary comparison uses the first 5 repos of
the seeded order** (entireio/cli, JanDeDobbeleer/oh-my-posh, ebridges/wakil, yash-learner/care_fe,
hxrshx/ehl-bayern-codewerk). Added as secondary (not pre-registered): H on all 11 clean repos. Recall runs for the
11; the judge sees H's items for those cases only. Not done: entire-sem, raman325/lock_code_manager,
0-uddeshya-0/Recognition, entireio/git-sync.

## Results (2026-10-12)

Run data: `~/.local/share/borg/eval/swe-chat/run/` (`summary.json`, per-arm rows, `judge.jsonl`, `claude-calls.jsonl`).
150 positives (judge: 58 of them are genuine pushback on the agent's turn, the rest are new requests or follow-ups),
50 negatives. 0 errors in any arm, hook or judge call. H covers the 5 pre-registered repos (50 positives, primary)
and all 11 clean repos (110 positives, secondary).

### Catch rate (≥ 1 item judged `yes`; Wilson 95% CI)

| Arm | n | Strict | Lenient (yes/partial) | Genuine-pushback subset | Items per case |
|---|---|---|---|---|---|
| N  no memory | 150 | 16.0% [11.0, 22.7] | 48.7% | 29.3% (17/58) | 4.66 |
| L  plain log | 150 | 18.7% [13.2, 25.7] | 54.0% | 31.0% (18/58) | 4.65 |
| H  Hindsight, 5 repos | 50 | 14.0% [7.0, 26.2] | 52.0% | 33.3% (5/15) | 4.58 |
| H  Hindsight, 11 repos | 110 | 14.5% [9.2, 22.3] | 49.1% | 31.7% (13/41) | 4.65 |
| B-block (asked lessons) | 150 | 3.3% [1.4, 7.6] | 8.0% | 3.4% (2/58) | 1.73 |
| B-all (asked + graded free) | 150 | 4.7% [2.3, 9.3] | 18.0% | 6.9% (4/58) | 3.38 |

### Paired differences (strict, points; cluster bootstrap over repos, 95% CI)

| Comparison | n (repos) | All positives | Genuine subset | Lenient |
|---|---|---|---|---|
| L − N | 150 (15) | +2.7 [−2.0, +7.3] | +1.7 [−5.5, +9.3] | +5.3 [−2.0, +14.0] |
| **B-all − L** | 150 (15) | **−14.0 [−20.0, −8.0]** | −24.1 [−36.7, −11.3] | −36.0 [−47.3, −24.7] |
| B-all − N | 150 (15) | −11.3 [−17.3, −5.3] | −22.4 [−35.1, −9.5] | −30.7 |
| B-block − L | 150 (15) | −15.3 [−20.7, −10.0] | −27.6 | −46.0 |
| **H − L** (5 repos) | 50 (5) | **−8.0 [−18.0, +2.0]** | −6.7 [−23.1, +22.2] | +8.0 [−2.0, +16.0] |
| H − L (11 repos) | 110 (11) | −4.5 [−10.9, +2.7] | 0.0 [−12.5, +14.3] | −3.6 |
| H − N (5 repos) | 50 (5) | −2.0 [−6.0, 0.0] | 0.0 | +4.0 |
| **B-all − H** (5 repos) | 50 (5) | **−6.0 [−10.0, −2.0]** | −26.7 [−55.6, −5.9] | −38.0 |
| B-all − H (11 repos) | 110 (11) | −9.1 [−15.5, −3.6] | −24.4 [−38.8, −10.6] | −30.9 |

### False alarms on the 50 negatives

| Arm | Items per case | False alarms per case | Negatives with ≥ 1 |
|---|---|---|---|
| N | 4.60 | 3.16 | 92% |
| L | 4.82 | 3.34 | 100% |
| H (11 repos, n = 28) | 4.39 | 2.89 | 89% |
| B-block | 1.24 | 1.18 | 24% [14, 37] |
| B-all | 3.36 | 3.30 | 76% |

### Borg's Stop on the 200 turn ends

466 episodes → 192 lessons (4–40 episodes, 1–20 lessons per repo). The prefilter skipped 33 turn ends; 167 called the
consequence model; 56 blocked (322 asked lessons), 100 handed graded 2/1 lessons over free (353). Stop wall clock p50
8.6 s, p95 12.2 s. Hindsight recall p50 15.1 s (19 memories).

### Cost

Claude (Sonnet, `claude -p`), all logged: **$33.20 of the $45 cap** — borg learning $7.36 (466 calls), B's Stop
consequence model $9.29 (167 calls, $0.056 each), N $3.15 (200), L $4.83 (200), H $4.19 (138), judge $4.35 (200),
judge pilot $0.04, flag test $0.01. Per case: N $0.016, L $0.024, H $0.030, B $0.046 at Stop (+ $0.016 per learned
episode). Codex (Hindsight, gpt-5.6-luna, dedicated CODEX_HOME): **7.69M retain tokens** for 84 sessions (sync retain
usage) plus **≈13.1M consolidation input tokens** (Hindsight's own log estimates, 1,824 batches) ≈ 21M tokens for 11
repos; the quota ran out on the 12th.

### Examples

B caught, L missed:
- p106 (photon-hq/spectrum-ts), user: "will this broke the behaviour like when a stream failed we reconnect etc". Borg
  asked: "Could this change, or any new error path it introduces, interrupt or break the live message stream …?" —
  learned from an earlier correction in the same repo.
- p029 (oh-my-posh), user: move the SVG renderer out of the default build, analyse dropping bubbletea. Borg asked
  whether non-core code and heavy frameworks should move behind a build tag or be replaced by a small own
  implementation.

L caught, B missed (24 cases):
- p022 (oh-my-posh), user: "Do an analysis of why the [themes] are failing before we jump to conclusions." L: "14
  themes still fail … the user will likely want the real error found, using debug logging, instead of a commit of a
  partial fix."
- p134 (entire-sem), user: "Never ever do force pushes". L: "The agent attempted a force push to someone else's
  branch without explicit authorization …". (Both are visible in the turn; L's catches mostly come from reading the
  turn, not from the log: L − N is only +2.7.)

### Verdicts against the pre-registration

1. **Memory helps: not shown.** L − N = +2.7 [−2.0, +7.3]; the plain log adds nothing measurable over Sonnet reading
   the turn.
2. **Borg vs the plain log: Borg loses.** B-all − L = −14.0 [−20.0, −8.0] (B-block −15.3). It also loses to no memory
   (−11.3 [−17.3, −5.3]). The prediction (B-all < L) held, more strongly than expected.
3. **Hindsight vs the plain log: does not beat it.** H − L = −8.0 [−18.0, +2.0] on the 5 pre-registered repos
   (−4.5 [−10.9, +2.7] on 11): a tie at best.
4. **Borg vs Hindsight: Borg loses.** B-all − H = −6.0 [−10.0, −2.0] (5 repos), −9.1 [−15.5, −3.6] (11 repos).
5. **Noise rule: no arm triggers it** (no arm has > 2× N's false-alarm rate). B-block is the quietest (24% of
   negatives with a false alarm vs 92% for N) but it is quiet because it rarely says anything; per item, 98% of
   B-all's items on negatives are false alarms (N 69%).

Reading: on third-party repos, what a user is about to correct is mostly visible in the turn itself (N catches 29% of
genuine pushbacks); a repo's earlier corrections, whether as a raw log, Hindsight facts or Borg lessons, add nothing
detectable. Borg's Stop surfaces generic lessons that rarely match the specific point, and they are the wrong unit
for this task.

### Caveats (beyond the pre-registered ones)

- The arms differ in form: N/L/H are Sonnet writing turn-specific concerns, B is Borg's lessons as surfaced. B is
  not "Sonnet + Borg lessons"; an arm that gives Sonnet the lessons Borg's Stop selected (the L-analogue) was not run
  and would separate Borg's retrieval from the item format.
- Only 58 of 150 "corrections" are genuine pushback per the judge; the strict catch rate on the rest is near zero for
  every arm.
- 180 of 4,660 (episode, positive case) pairs have the episode dated after the case (created_at splits are not
  strict in time); L and B filter them, Hindsight cannot.
- H covers 5 (primary) / 11 repos; its ingest cost ≈ 21M Codex tokens, heavier than the pre-registered ~20M estimate
  for all 15.
- One sample per arm and case; Sonnet judges Sonnet.

## Follow-up (2026-10-12): does the hidden correction recur, and is Borg's loss retrieval or format?

### Pre-registration (committed before the paid part; nothing here changes after the first F call)

Two questions the results leave open. Code: `eval/swe-chat/followup.py`; run data in `run/followup/`.

**1. Recurrence (no model calls).** For each of the 150 positives: how similar is the hidden correction to the most
similar *earlier* learn-side correction of the same repo (every learn-session correction/rejection prompt passing the
prep filters and dated before the case, not only the 40 newest that L and B saw; the 40-newest view is also reported)?
Similarity: TF-IDF cosine over Borg's tokeniser (`conscience::tokens`, ported), idf over all corrections and cases
(primary); cosine of the local Model2Vec vectors (`potion-multilingual-128M`, Borg's mean-pool embed) as the meaning
check. **Control:** the same case against corrections of *other* repos, size-matched (for a case with k same-repo
candidates, best match over k random other-repo corrections dated before the case; 200 seeded draws per case).
**Threshold** = the 95th percentile of the control's best-match similarity over all draws. Reported: the share of
cases whose best same-repo match exceeds the threshold (Wilson CI; 5% is what chance gives), on all 150 and on the 58
judge-rated genuine pushbacks, plus example pairs above the threshold. Reading: a share near 5% ⇒ these corrections
are new each time; no memory of earlier corrections could have anticipated them. Prediction: 10–20% on all, lower on
the genuine subset in absolute terms of usable recurrences (most matches are boilerplate like "commit and push").

**2. Arm F, "Sonnet + Borg's selection" (paid).** Same system prompt, schema and turn context as N/L/H; memory block =
"Lessons a memory of this repository's earlier corrections selected for this turn (question → consequence)", one
line per lesson `- question → consequence`. Lessons: the ones Borg's Stop surfaced for that case in the B run
(asked, then graded-free, as recorded in `borg_hook.jsonl`). The Stop's pool was not recorded, so for a case where
Borg surfaced nothing (32 positives, 12 negatives) the block is the repo store's lessons dated before the case
(latest version, `origin_ms` < case time) ranked by Borg's BM25 (`conscience::bm25`, ported, over the questions; query
= the case's turn context), top 15. All 150 positives + 50 negatives, one Sonnet call each.
**Judge:** the same blind judge (system prompt, instructions, schemas from `judge.py`), one call per case over F's
items only (shuffled, numbered). F's items are therefore judged in a separate call, not pooled with the other arms;
if the spend allows (≤ $6.5 after the F judge), N's items are re-judged alone on a seeded 40 positives to measure
what the pooling changes (calibration, reported, not used to adjust). `genuine` comes from the original judge.
**Metrics:** F's catch (strict, lenient, genuine subset), items per case, false alarms on negatives; paired F − N,
F − L, F − H (5 repos; 11 repos secondary), F − B-all, F − B-block by the same cluster bootstrap (10,000 resamples of
repos, seed 20261012); also split by whether F's block came from Borg's surfaced lessons or the BM25 fallback.
**Decisions:** F − B-all > 0 with the CI excluding 0 ⇒ the output format (lessons surfaced as is) explains much of
Borg's loss. F − L < 0 with the CI excluding 0 ⇒ Borg's selected lessons are worse memory than the plain log (retrieval
/ generalisation loses information); CI including 0 ⇒ Borg's selection is as good as the raw log once Sonnet writes
the concerns. F − N: the same rule for "Borg's lessons add anything". **Prediction:** F ≈ N ≈ L (15–20%), F − B-all
≈ +10 points (CI excluding 0), F − L within ±4 (CI including 0).
**Budget:** this follow-up's own cost log `run/followup/claude-calls.jsonl`, running total, hard cap **$8** (refuses
calls past $7.50); expected ≈ $4 for F, ≈ $2.5 for its judge.

### Results (2026-10-12)

Run data: `run/followup/` (`recur.json`, `recur_cases.jsonl`, `arm_F.jsonl`, `judge_F.jsonl`, `summary.json`,
`claude-calls.jsonl`). 0 errors in any F or judge call.

**1. Recurrence.** 851 learn-side corrections (all 15 repos; every positive has ≥ 1 earlier same-repo correction).
Best same-repo match above the size-matched cross-repo 95th percentile (chance = 5%):

| Similarity | Threshold | All 150 | Genuine 58 | All 150, threshold = p95 of best over *all* other repos |
|---|---|---|---|---|
| TF-IDF (Borg tokens) | 0.194 | 18.7% [13.2, 25.7] (28) | 15.5% [8.4, 26.9] (9) | 6.0% (0.285) |
| Model2Vec cosine | 0.670 | 13.3% [8.8, 19.7] (20) | 15.5% [8.4, 26.9] (9) | 3.3% (0.735) |
| TF-IDF, only the 40 newest (what L/B saw) | 0.169 | 22.0% (33) | 19.0% (11) | 6.7% |

Median best match: same repo 0.134 vs control 0.091 (TF-IDF); 0.525 vs 0.481 (vectors). The excess over 5% is real but
mostly not recurrence: reading all 41 distinct cases above either threshold (by the author, unblinded), **4 restate an
earlier correction** (2.7% of 150; **1 of the 58 genuine**), 6 are on a related topic, and the rest share only the repo's
language (French in terraform-examples), genre (pasted code reviews in git-sync) or vocabulary. The four:
- p025 (oh-my-posh), 0.62 / 0.86: "…I want to change the order: I want JSON first, YAML second, TOML last" ← earlier,
  verbatim: "…I want to change the order: I want JSON first, YAML second, TOML last."
- p072 (faucet-stream): "now can u pick the 539-544? and solve them in 1 pr itself" ← "can u pick all these new 4 issues
  and lets solve them in single pr?"
- p026 (oh-my-posh): "fold the changes into the right commits, do not run the test suite every time" ← "commit
  everything, folded into the right commits"
- p144 (care_fe, genuine): "update my backend pullrequest to update fixtures, lets not hack it via frontend" ← "we need
  to improve the backend fixtures for tests in frontend … a backend stacked pr"
Related, not restated: p018 (staging GOOGLE_CLIENT_ID value ← "set the staging env variables … GOOGLE_CLIENT_ID"),
p147 (long-term maintainability as the goal), p001/p009 (the "architect as a product" vision restated), p019, p090.
Surface-only, e.g. p031/p034/p039 (any French correction matches "Eh bien, bien vu…"), p062/p066/p069 (any pasted
review matches another pasted review).

Who caught the four: L 3 (p072, p026, p144), B-all 2 (p026, p144), F 2, N 0. So the plain log's +2.7 over N is about
the size of the recurrence there is to find. **Answer: memory of earlier corrections could have anticipated at most a
few percent of these cases (≈ 3% restated, ≈ 7% with related topics); on this data the ceiling for any memory system
is low, whatever its design.** (Prediction 10–20% above threshold held; usable recurrence is lower than predicted.)

**2. Arm F (Sonnet + Borg's selection).** 118 positives / 38 negatives got Borg's surfaced lessons, 32 / 12 the BM25
fallback (top 15); 2,640 chars of lessons per case on average.

| Arm | Strict catch | Lenient | Genuine | Items | False alarms per negative | Negatives with ≥ 1 |
|---|---|---|---|---|---|---|
| **F** Sonnet + Borg's lessons | **14.7% [9.9, 21.2]** | 51.3% | 22.4% (13/58) | 4.71 | 3.24 | 94% |
| N | 16.0% | 48.7% | 29.3% | 4.66 | 3.16 | 92% |
| L | 18.7% | 54.0% | 31.0% | 4.65 | 3.34 | 100% |
| H (5 / 11 repos) | 14.0% / 14.5% | 52.0% / 49.1% | 33.3% / 31.7% | 4.6 | 2.89 (11) | 89% (11) |
| B-all | 4.7% | 18.0% | 6.9% | 3.38 | 3.30 | 76% |

| Paired (strict, points, cluster bootstrap) | n (repos) | All positives | Genuine | Lenient |
|---|---|---|---|---|
| F − N | 150 (15) | −1.3 [−7.3, +4.7] | −6.9 [−21.6, +6.2] | +2.7 [−4.7, +10.7] |
| **F − L** | 150 (15) | **−4.0 [−10.0, +2.0]** | −8.6 [−22.2, +3.0] | −2.7 [−13.3, +6.7] |
| F − H (5 repos) | 50 (5) | +6.0 [+2.0, +10.0] | −6.7 [−36.4, +10.5] | +2.0 |
| F − H (11 repos) | 110 (11) | +2.7 [−2.7, +8.2] | −2.4 | +0.9 |
| **F − B-all** | 150 (15) | **+10.0 [+5.3, +15.3]** | +15.5 [+5.8, +26.3] | +33.3 [+20.7, +45.3] |
| F − B-block | 150 (15) | +11.3 [+6.7, +16.7] | +19.0 | +43.3 |
| F − B-all, Borg-surfaced cases | 118 (15) | +10.2 [+5.2, +15.2] | | |
| F − L, Borg-surfaced cases | 118 (15) | −2.5 [−8.8, +3.7] | | |

F's 22 catches overlap N's 24 in 12 cases (10 F-only, 12 N-only): mostly the same turn-visible points, a different
sample. B-all caught 7; F caught 5 of them.

**Verdicts.** (a) **Format: F − B-all = +10.0 [+5.3, +15.3] ⇒ the output form explains Borg's loss.** Given the same
lessons, Sonnet writing turn-specific concerns recovers N-level catch; surfacing the lessons as is does not.
(b) **Retrieval: F − L = −4.0 [−10.0, +2.0], CI includes 0 ⇒ Borg's selection is not shown to be worse memory than the
raw log** (point estimate below it; the 4 restated corrections give L its edge: 3 vs F's 2). (c) F − N = −1.3: Borg's
lessons add nothing detectable over no memory, the same as the log. F − H (5 repos) +6.0 has a CI excluding 0 on only
5 clusters; on 11 repos it is +2.7 [−2.7, +8.2]: a tie. Predictions held (F ≈ N ≈ L; F − B-all ≈ +10; F − L at the edge
of ±4).

**Reading.** Borg's loss on SWE-chat is mostly the unit it returns (generic questions) and only possibly what it
retrieves; and with ≈ 3% of corrections restating an earlier one, no memory arm (log, Hindsight, Borg's lessons) has
more than a few points to win on this data. A test that can separate memory systems needs a corpus where corrections
recur (same user, standing preferences), not third-party sessions sampled at random.

**Caveats.** F's items were judged in their own call, not pooled with the other arms' items; the pre-registered
calibration (N re-judged alone) was skipped because the spend after F's judge was $6.87 (> $6.50). The recurrence
example classification is the author's unblinded reading. One sample per case; Sonnet judges Sonnet.

**Cost:** $6.87 of the $8 cap (F arm $3.89 for 200 calls, $0.019 each; judge $2.98 for 200); recurrence: 0 model calls.

## One-team test (2026-10-12): one team's history as one collective

### Pre-registration (committed before any paid call of this test; nothing here changes after the first paid call)

Question: the follow-up found ≈ 3% of corrections restating an earlier one across 15 random repos. Borg's model is a
team collective, so test where knowledge should repeat: one team, many users, long history. Code:
`eval/swe-chat/team.py`; run data in `run/team/`.

**Team choice (from the data, `run/team/team.json`).** Among Claude Code sessions that have a compact transcript
(3,760 in SWE-chat), grouped by GitHub owner: **entireio** has 475 sessions, **26 distinct users**, 7 repos; the next
team with ≥ 2 users has 53 sessions (JanDeDobbeleer, 2 users). ezpds (167) and pc-switcher (130) are single-user.
entireio's repos are used as one collective (cli 419 sessions, git-sync 16, entire-graph 14, go-nuts 14, forgemark 6,
entire-sem 5, auth-go 1).

**Sample.** The newest 200 entireio Claude Code sessions with a compact transcript, ordered by `created_at`
(2026-08-12 → 2026-09-04; cli 172, git-sync 12, entire-graph 8, go-nuts 4, entire-sem 2, forgemark 1, auth-go 1;
20 users + 27 sessions without a user id). Learn = first 150 (17 users, until 2026-08-31 14:47), test = last 50
(11 users, from 2026-08-31 15:28; 8 of them also in learn). 176 transcripts downloaded (24 were already local).
Labels from `prompt_labels.parquet`, matched as before.

**Cases (no model calls).** Same filters as the main eval (≥ 5 words, no filler, no `/`/`#`, an agent message before).
273 correction/rejection prompts qualify in the test sessions; **150 drawn** by seeded shuffle (seed 20261012;
142 correction, 8 rejection), plus **50 `non_pushback` negatives** (of 231); 38 of the 50 test sessions contribute.
Same turn context as before.

**Episodes.** All 854 correction/rejection prompts of the 150 learn sessions that pass the filters (15 engineers), as
borg Episodes built as before (engineer = `user_id`, repo = the repo's short name).

**1. Recurrence (done, zero model calls; `run/team/recur.json`, `repeats_themes.json`).** Same method as the
follow-up: best match of each case against every earlier team correction (854), TF-IDF over Borg's tokens and
Model2Vec cosine; control = the same case against size-matched draws (k = the number of earlier team corrections,
200 draws) from 1,299 correction prompts of the 13 non-team repos of the earlier sample; threshold = the control's
95th percentile.

| Similarity | Threshold | Above (of 150) | Wilson 95% | of which the best match is another user's |
|---|---|---|---|---|
| TF-IDF | 0.355 | 18 (12.0%) | [7.7, 18.2] | 13 |
| Model2Vec | 0.727 | 15 (10.0%) | [6.2, 15.8] | 13 |

Reading (author, unblinded): of the 29 distinct cases above either threshold, **5 restate an earlier correction**
(p005, p079, p095: resolve addressed review findings/comments; p076: "remove the artificial line breaks" ← the same
user earlier "kill the artificial line breaks from the pr description"; p118: keep posts short); the rest share genre
(pasted reviews, trail links) or words only. So the lexical method gives **3.3% [1.4, 7.6]**, as on random repos.
But it misses restatements worded differently, so all 150 positives were also read against the learn corrections
(theme search, unblinded): **21 of 150 restate a point the team had already corrected (14.0% [9.3, 20.5])**:
- *run the real end-to-end test, not just CI* (7: p028, p036, p037, p038, p042, p110, p111; e.g. p036 "did you run
  the full user test end to end?" ← another user, earlier: "ddi we e2e test it fully", "then run full e2e test");
- *keep it short* (4: p076, p094, p118, p130 ← "keep it short and more condensed", "can we do shorter, more concise");
- *resolve the review findings/comments that are addressed* (4: p005, p079, p095, p144 ← "close/resolve/fix trail
  findings", "can we resolve the rest of the comments in the PR? have they been addressed?");
- *explain it more simply* (3: p010, p043, p140 ← "in simpler and more concise terms please", "explain again the fix");
- *make the code simpler* (2: p141, p149 ← the same user: "We are making this over complex", "Lets keep it simple");
- *use go-git for git operations* (1: p001 ← "could we not use go-git native logic for this?").
8 more are related but not restated (p025, p056, p062, p067, p089, p096, p143, p146): 29 of 150 (19.3%) with them.
Most themes were raised earlier by a different team member than the one who repeats them. **Above the ~5% stop line,
so the paid arms run.** The 21 are the "repeats" subset (`run/team/repeats.json`), fixed now.

**2. Learn.** One store (`run/team/stores/entireio/`), the real `borg learn` (`target/release/borg`, `--features
vectors`, defaults), BORG_HOME = that store, BORG_CONFIG = a missing temp file, every borg `claude` call through the
logging shim. Budget: 854 episodes × ≈ $0.016 = ≈ $13.7, which does not fit the $40 cap with the arms and judge
(projected ≈ $27). So **650 episodes, taken evenly over time** (evenly spaced ranks of the time order), are learned
(≈ $10.4); if learning passes $12 it is stopped and the store is used as it is (stated).

**3. Arms** (all 200 cases; same instruction, schema and turn context as N/L before):
- **N:** no memory.
- **L:** "Earlier corrections by users of this repository (oldest first)" + the team's learn corrections dated before
  the case, the newest that fit **25,000 chars**, one line each (date, repo, text cut to 400 chars). All 854, not
  only the 650 learned.
- **B:** the real Stop path on a temp copy of the team store, as before (prompts and tool calls of the session replayed
  as hooks, then `stop`; graded Stop, `BORG_BEFORE` = case time, `BORG_LEARN=off`, `BORG_QUEEN=off`, cwd
  `/tmp/swe-chat/<repo>`). **B-block** = asked lessons; **B-all** = asked + graded free.
- **F:** Sonnet + the lessons this case's Stop surfaced (asked then free), in the follow-up's format (`- question →
  consequence`). A case where the Stop surfaced nothing gets an empty block "(none)" (no BM25 fallback this time:
  F is exactly Sonnet + what the real Stop path selects).

**Judge:** the same blind Sonnet judge (`judge.py` prompts and schemas), one call per case over the pooled, shuffled
items of N, L, F, B (exact duplicates merged).

**Metrics.** Strict catch (≥ 1 item `yes`) and lenient, Wilson CIs, per arm on: all 150; the judge's `genuine`
subset; the **21 repeats**; the 29 cases above either recurrence threshold. Paired differences **F − L, F − N, L − N,
B-all − F** (also B-all − L, B-block − F) by **cluster bootstrap over test sessions** (10,000 resamples, seed
20261012). False alarms on the 50 negatives (per case; share with ≥ 1, Wilson). Cost per stage.

**Decisions.** For each pair, difference > 0 with the CI excluding 0 ⇒ the first arm wins; CI including 0 ⇒ no
difference shown. (1) L − N > 0 ⇒ the team's history helps. (2) F − L ⇒ Borg's selection vs the raw log. (3) F − N ⇒
Borg's lessons add anything. (4) B-all − F ⇒ surfacing as is vs Sonnet phrasing. On the 21 repeats (n small) the
differences are reported with their CIs; a catch-count gap is noted, not called a win, unless the CI excludes 0.

**Predictions.** All arms catch 15–25% overall. On the repeats L catches clearly more than N (the e2e and brevity
points are in the log; ≈ +20 points, CI likely including 0 at n = 21); F between N and L; B-all < F overall (as before)
but B-all closer to F on the repeats. L − N overall +2 to +5 (n.s.).

**Budget.** Own cost log `run/team/claude-calls.jsonl` (running total), hard cap **$40** (refuses past $39.50). Order:
learn → B → F → N → L → judge. If the projection exceeds the cap before the judge, negatives are dropped from the
remaining arms first (stated).

**Caveats known now.** The theme reading that defines the repeats is the author's, unblinded and after seeing the test
prompts (the lexical method alone gives 3.3%). The 50 test sessions span four days and 38 of them contribute cases;
some prompts appear twice (p037/p038 are the same text at two turns). Sonnet judges Sonnet; one sample per case.

#### Amendment (2026-10-12, during learning; before any judge call)

- **Repeats: 20, not 21.** A date check of the theme reading found p001's earlier source ("could we not use go-git
  native logic for this?") dated *after* the case (test sessions contain prompts from as early as 2026-08-12, before
  the last learn prompts; memory is filtered by prompt time, so this was a reading error). Repeats = 20 of 150,
  **13.3% [8.8, 19.7]**; each of the 20 has ≥ 8 earlier learn corrections of its theme, from ≥ 3 team members.
- **Order:** N ran while Borg was learning (it depends on nothing; the projection was within the cap).

#### Amendment 2 (2026-10-12, after N and L, during learning; before any Stop, F or judge call)

L cost **$11.11** ($0.056 per call, not the ≈ $0.025 projected: `claude -p` wrote the whole ≈ 13k-token prompt to
the prompt cache on every call, with no reuse of the shared log prefix). Spend at this point $17.58 (learning $3.08
for 193 episodes, N $3.39, L $11.11), so the pre-registered plan no longer fits $40. Changes, per the pre-registered
fallback and to keep the positives whole:
- **Learning cut to 443 episodes:** the 193 already learned (the first 253 of the 854 by time, at the 650-plan's
  spacing) + **250 evenly spaced over the remaining 601** (ranks 253–853). The older part is denser (0.76 vs 0.42 of
  episodes learned). Learning stops if its spend passes $7.30.
- **Negatives dropped from B and F** (no Stop or F call for them); the judge still grades N's and L's items on the 50
  negatives, so false alarms are reported for N and L only. F is reported on positives only.
- If the projection still exceeds the cap before the judge, the judge drops negatives first.

#### Amendment 3 (2026-10-12, after 52 Stop runs; before any F or judge call)

Borg's Stop on the 197-lesson team store costs **$0.118 per consequence-model call** (50 calls, $5.92), 2× the main
eval's $0.056 (the larger store hands it more candidate lessons; it asked 10–13 lessons in a blocked turn). At $27.66
spent, Stop on all 150 positives no longer fits. Changes:
- **B and F run on a subset of 90 positives:** the 52 already run (p000–p051, the oldest by time), **all 20 repeats**
  (12 of them not yet run), and **26 more drawn by seeded shuffle** (seed 20261012) from the remaining non-repeat
  positives (`run/team/bf_subset_extra.json`). Comparisons with F or B are paired on this subset; L − N uses all 150.
  Catch rates on "all positives" for B/F are on these 90 (repeats over-represented: 20 of 90; reported both with and
  without them).
- **The judge drops the negatives** (pre-registered fallback): no false-alarm numbers in this test.
- The Stop run stops if total spend passes $33.20 (reserving F and the judge).

### Results (2026-10-12)

Run data: `run/team/` (`summary.json`, `arm_*.jsonl`, `borg_hook.jsonl`, `judge.jsonl`, `recur*.json`,
`repeats*.json`, `claude-calls.jsonl`). 0 errors in any arm, hook or judge call. Borg learned 443 episodes → **197
lessons** (one team store). The judge rates 67 of the 150 positives as genuine pushback. Bootstrap clusters = test
sessions (33 for all 150; 20 on the B/F subset; 11 for the repeats).

**Catch rate (strict: ≥ 1 item `yes`; Wilson 95%)**

| Subset | n | N | L | F | B-block | B-all |
|---|---|---|---|---|---|---|
| All positives (N, L) | 150 | 21.3% [15.5, 28.6] | 21.3% [15.5, 28.6] | – | – | – |
| B/F subset | 90 | 25.6% [17.7, 35.4] | 21.1% [14.0, 30.6] | **27.8%** [19.6, 37.8] | 6.7% [3.1, 13.8] | 12.2% [7.0, 20.6] |
| B/F subset, non-repeats | 70 | 25.7% | 20.0% | 25.7% | 4.3% | 8.6% |
| Genuine (B/F subset) | 35 | 40.0% | 34.3% | 40.0% | 2.9% | 5.7% |
| **Repeats** | 20 | 25.0% [11.2, 46.9] (5) | 25.0% (5) | **35.0%** [18.1, 56.7] (7) | 15.0% (3) | 25.0% (5) |
| Above a recurrence threshold | 17 | 17.6% (3) | 17.6% (3) | 41.2% (7) | 17.6% (3) | 29.4% (5) |

Lenient (yes/partial), B/F subset: N 54.4%, L 63.3%, F 61.1%, B-block 13.3%, B-all 32.2%; repeats: N 50%, L 70%,
F 55%, B-all 35%. Items per case: N/L/F ≈ 4.8, B-block 4.0, B-all 8.1.

**Paired differences (strict, points; cluster bootstrap over test sessions, 95% CI)**

| Pair | B/F subset (n = 90) | Repeats (n = 20) | Non-repeats (n = 70) | Genuine (n = 35) |
|---|---|---|---|---|
| **F − L** | +6.7 [−1.3, +12.9] | +10.0 [0.0, +28.6] | +5.7 [−4.1, +10.7] | +5.7 [−13.3, +26.1] |
| **F − N** | +2.2 [−5.8, +7.8] | +10.0 [−14.3, +35.0] | 0.0 [−5.4, +3.4] | 0.0 [−17.2, +14.8] |
| **L − N** | −4.4 [−11.6, +1.9]; **all 150: 0.0 [−4.7, +5.0]** | 0.0 [−23.5, +17.6] (lenient +20 [0, +57]) | −5.7 [−12.3, +3.4] | −5.7 [−21.9, +11.5] |
| **B-all − F** | **−15.6 [−23.0, −5.8]** | −10.0 [−35.7, +8.7] | −17.1 [−24.0, −6.4] | −34.3 [−53.8, −6.9] |
| B-all − L | −8.9 [−17.5, +1.5] | 0.0 [−23.1, +20.0] | −11.4 [−19.6, −2.4] | −28.6 [−46.9, −8.3] |
| B-block − F | −21.1 [−27.7, −10.2] | −20.0 [−40.9, +5.9] | −21.4 [−28.1, −10.7] | −37.1 [−54.8, −10.5] |

(Not pre-registered: on the 17 B/F cases above a recurrence threshold, F − L = +23.5 [+7.1, +36.8], F − N +17.6
[−7.1, +37.5].)

**Who caught the 20 repeats.** Caught by a memory arm but not by N: **B-all 5** (p028, p036, p111: Borg's team lesson
"have I reproduced it live … not just unit, integration, lint and CI", learned from another member's e2e
corrections; p042, p079: "resolve review threads yourself"), **F 4** (p005, p042, p079, p130), **L 2** (p005, p043:
"they've asked for concise answers"). N alone: p094, p140. No arm caught 7 (p010, p076, p095, p110, p118, p141,
p149: explain more simply ×1, brevity ×2, resolve, e2e, simpler code ×2). F had Borg's live-reproduction lesson in
its block for p036 and p111 and did not raise it.

**False alarms (50 negatives, N and L only):** N 3.20 false-alarm items per case (98% of negatives with ≥ 1), L 3.54
(100%). B and F were not run on negatives (amendment 2).

**Borg's Stop (90 positives):** 6 skipped by the prefilter, 85 consequence-model calls, 37 blocked (357 asked
lessons), 374 handed over free; 7 turns surfaced nothing. Stop wall clock p50 12.3 s; $0.118 per call (2× the
15-repo stores).

**Cost:** **$38.19 of the $40 cap** — learning $7.23 (444 calls), Stop $10.05 (85), N $3.39 (200), L $11.11 (200),
F $2.02 (90), judge $4.38 (200: 150 positives pooled over N/L/F/B, 50 negatives over N/L). Recurrence: 0 model calls.

### Verdicts

1. **Recurrence on one team: real but small.** The lexical method finds what it found on random repos (3.3% restated);
   a full reading finds **13.3% [8.8, 19.7]** of the team's corrections restating a point the team had made before
   (mostly standing preferences: run the real e2e test, keep it short, resolve addressed findings, explain simply,
   simpler code), most of them first raised by another member. (The 15-repo follow-up's 2.7% came from the threshold
   reading only, so the two rates are not directly comparable; by the same threshold reading the team gives 3.3%.)
2. **Does the team's history help (L − N)? Not shown:** 0.0 [−4.7, +5.0] on 150; 0.0 on the repeats (lenient +20,
   CI [0, +57]). The raw log of 160 earlier corrections does not make Sonnet raise them.
3. **Borg's selection vs the raw log (F − L): not shown to differ;** +6.7 [−1.3, +12.9] on 90, +10.0 [0.0, +28.6] on
   the repeats (CI touches 0). Point estimates now favour Borg's selection (the reverse of the 15-repo follow-up's
   −4.0), at ¼ of L's per-call cost.
4. **Borg's lessons add anything over no memory (F − N)? Not shown:** +2.2 [−5.8, +7.8]; +10.0 [−14.3, +35.0] on the
   repeats.
5. **Format (B-all − F): Borg's raw surfacing loses again,** −15.6 [−23.0, −5.8], as in the follow-up. On the repeats
   alone the gap closes (−10.0 [−35.7, +8.7]; both catch 5–7 of 20) and B-all catches 3 e2e repeats nobody else does,
   but it lists 8 lessons per turn to do so.

Predictions: L ≫ N on the repeats did not happen (0.0); F between N and L did not happen (F highest); B-all < F overall
held; B-all closer to F on the repeats held.

**Reading.** In a real team, corrections do repeat (≈ 13%, mostly standing preferences across members), and Borg
learns the right lessons from them: its live-reproduction lesson is exactly the point three later e2e corrections make.
But no memory arm turns that into a measurable gain at n = 20 repeats, and on the other 87% of corrections memory is
neutral at best. The unit Borg surfaces (8 generic lessons per turn) still loses to Sonnet's turn-specific concerns;
Sonnet given Borg's selection (F) is the best arm on every subset here, but not by a margin the CIs confirm. To show a
memory effect at these rates needs ≈ 100+ repeat cases (several teams or a longer history), not 20.

### Caveats

- The repeats subset is defined by the author's unblinded theme reading after seeing the test prompts.
- B and F ran on 90 of the 150 positives (budget); that subset is the 52 oldest + all 20 repeats + 26 random, so
  B/F rates on it are not rates on a random sample; N and L are reported on it for the paired view.
- Borg learned 443 of 854 episodes, the older third denser (amendment 2).
- L's log is the newest 25k chars (≈ 160 of 854 corrections); older standing preferences may fall out of it.
- One sample per case; Sonnet judges Sonnet; no false alarms for B or F.
