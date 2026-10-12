# SWE-chat engineering corrections: cross-repo transfer (pre-registration, 2026-10-12)

Written and committed before any paid model call of this test. Nothing below changes after the first paid call;
later changes are dated amendments under it.

## Question

Do lessons learned **earlier** in **other** repos help Borg raise the point an engineer is about to correct, before
they correct it? This is the case for a collective: knowledge from one team's repo reaching another's.

## Data

`~/.local/share/borg/eval/swe-chat/engineering/` (mining: `reports/swe-chat-engineering.md`). Run data in its
`transfer/` subdirectory. Code: `eval/swe-chat/engineering/transfer.py` (and `transfer_fetch.py` for the data, 0
tokens). It reuses the earlier harness: `common.claude` / `common.context`, `arms.SYSTEM`/`SCHEMA`, `borg.run_hook`
and its cost shim, `judge.pooled`/`prompt`/schemas, `report.wilson`/`boot`/`caught`, `followup.tokens`/`bm25`.

- **Test cases: the 149** of the 1,291 quality tier whose canonical concept was corrected earlier in another repo's
  quality-tier correction and that have a linked commit (`q_prior_other_repo & has_commit`). All 149 are kept.
  55 repos; `entireio/cli` 43; 34 with an authorship (`s1 >= 2`) or Notre Dame engineering signal ("signal").
- **Timestamps.** 157 quality-tier prompts lack a timestamp (in `prompt_labels` and `conversations.parquet`
  alike). Each correction gets an interval: its timestamp, else [the session's nearest timestamped prompt before it,
  the nearest after it]. "Strictly earlier" means the memory item's latest possible time is before the case's
  earliest possible time. Under this rule 130 of the 149 keep a strictly earlier other-repo source of their concept
  (19 lost theirs to uncertain times or to the fork rule below); they stay in the test set, and the 130 are a
  reported subset.
- **Same repo** = same repo name, ignoring the owner: forks and mirrors (`osabiohq/osabio` and `marcus-sa/osabio`,
  the `*/cli` forks, `entireio/git-sync` and `stale2000/git-sync` …) count as one repo, so a fork's corrections never
  count as "another repo".
- **Context** (identical for every arm, `common.context`): the session from `conversations.parquet` up to just before
  the correcting prompt, rendered as before (`USER:` / `AGENT:` / `[tool X] arg -> result`, results cut to 300
  chars), from the previous human prompt, tail-cut to 12,000 chars. Median 2.8k chars. 11 corrections open their
  session (no earlier activity is logged); their context says so, and they stay in.

## Memory pool (stated: the 1,291 quality tier, not the 5,680 set)

Learning the whole tier (≈ $0.016 per episode with Borg's Sonnet generalisation ≈ $21) does not fit the $15 cap with
the arms. The pool is therefore **199 quality-tier corrections**: every one of the 151 earlier other-repo
**concept sources** of the 149 cases, plus **48 distractors** drawn at random (seed 20261012) from the rest of the
tier. Consequence, stated now: this pool is enriched for the relevant concept (about one source per case among 199
items instead of among ≈ 1,290), so retrieval is easier than with the full tier. The test is therefore
**optimistic for memory**: a null result here is stronger evidence against transfer than a positive one is for it.

- **Episodes** (as the earlier harness built them): task = the latest earlier prompt of the session not itself
  labelled pushback, agent = the agent's text before the correction (last 1,200 chars, what the mining kept),
  correction = the prompt (1,500 chars), engineer = user, repo = repo name, ms = the latest possible time.
- **Lessons:** the real `borg learn` (`target/release/borg`, `--features vectors`, defaults: Sonnet generalisation,
  merge) over the 199 episodes **once**, in time order, into one store (`transfer/pool_store/`); each episode is
  generalised exactly once (the cache). BORG_HOME = that store, BORG_CONFIG = a missing temp file, every borg
  `claude` call through the logging shim.
- **Per-case store, no leakage** (`transfer/stores/<case>/`, 0 tokens): the episodes the case may see are those of
  another repo and strictly earlier. For each lesson, the store keeps its **latest version all of whose episodes the
  case may see** (a merge that brought in a later or same-repo episode is cut back to the version before it; a
  lesson first learned from such an episode is dropped). The case itself and anything from its repo never enter.
  The vector index is rebuilt per store (`borg vectors --rebuild`), and `BORG_BEFORE` = the case's earliest time.
  Nothing is built in the real collectives; every store and temp dir is under `~/.local/share/borg/eval/`.

## Arms (one per case, same context)

Shared instruction for N, L, F: `arms.SYSTEM` (the reviewer's "concerns the user is most likely to raise", at most 5)
and `arms.SCHEMA`, Sonnet via `claude -p`, as in every earlier SWE-chat run.

- **N:** no memory.
- **L, plain log:** the raw earlier other-repo corrections of the pool the case may see, ranked by Borg's BM25
  (`followup.bm25`, Borg's tokeniser) over the correction text with the turn context as query, **top 10**, one line
  each (date, repo, text cut to 400 chars), most relevant first.
- **B, Borg as built:** `borg.run_hook` (the real `borg hook claude`) on a temp copy of the case's store: one `prompt`
  event per earlier human prompt of the session, one `post_tool` per earlier tool call (last 200), then `stop` with the
  agent's last message. Defaults: whole-store graded pool, graded Stop, vectors on, BM25 prefilter;
  `BORG_LEARN=off`, `BORG_QUEEN=off`, cwd `/tmp/swe-chat/<repo name>`. **B-block** = the lessons asked in a block;
  **B-all** (primary) = asked + graded 2/1 handed over free. Each lesson is an item (question + consequence).
- **System gate.** Learned lessons carry no `scope` (`learn.rs`), and the gate (`systems.rs gate`) only marks scoped
  lessons, so on this store it should never block another repo's lesson. It stays **on (as built)**; after learning,
  zero-token replays (`BORG_MATCH=bm25`, gate on vs `BORG_SYSTEM_GATE=off`) check this on a handful of cases. If any
  surfaced set differs, the gate is set off for B and that is stated as an amendment.
- **F, fair arm:** the follow-up/team F: Sonnet with the lessons this case's Stop surfaced (asked, then free), one line
  each `- question → consequence`, under "Lessons a memory of earlier corrections in other repositories selected for
  this turn"; "(none)" when the Stop surfaced nothing. Keeping grade-3 lessons verbatim inside F is **not feasible**
  without changing the binary: the Stop records which lessons it asked, not each lesson's grade. The verbatim view is
  B-block (a block happens only for a grade 3 and asks the 3s and 2s).

## Metric and judge

The blind Sonnet judge of the earlier runs (`judge.SYSTEM`, `judge.prompt`, `POS_SCHEMA`), one call per case over
the pooled, shuffled, de-duplicated items of N, L, F, B; arm names never reach it. It sees the context's tail (6,000
chars) and the engineer's real correction. **Hit** (strict, primary) = at least one of the arm's items judged `yes`
("had the agent been told this item before the user replied, it would have anticipated the specific point the user
then made"). Lenient = `yes` or `partial`. The judge's `genuine` flag is a secondary subset. No negatives: the set
has none, so false alarms are not measured.

## Analysis plan

- Hit rate per arm (N, L, F, B-block, B-all), Wilson 95% CIs.
- Paired differences **B-all − N** and **F − N** (primary), and L − N, F − L, B-all − L, F − B-all, B-block − N:
  cluster bootstrap over repos (10,000 resamples, seed 20261012), strict and lenient.
- The same on: excluding `entireio/cli`; signal (authorship or ND) vs no signal; the 130 with a strict source; the
  cases whose store holds a lesson learned from a concept source; the judge's genuine subset.
- **Retrieval diagnostics (0 tokens):** cases whose store holds a source lesson; whose Stop surfaced it (asked/free);
  whose L top 10 holds a source correction.
- **Decisions.** H1 (primary) holds for an arm if its difference to N is > 0 with the 95% CI excluding 0: B-all − N for
  Borg as built, F − N for Borg's selection phrased by Sonnet. CI including 0 ⇒ no transfer shown (a tie is not a
  win). < 0 with the CI excluding 0 ⇒ the arm hurts. The subsets are reported, not used to rescue H1.
- **Power, stated now:** at the reachable n (below) and hit rates near 20%, the CI of a paired difference is about
  ±8–12 points. Only a large transfer effect can show; a small one cannot be told from 0.

## Predictions (before any run)

N 15–30%. B-all below N (Borg's raw lessons lost to N by 11 points on the 15-repo run). F within ±5 of N; L within
±5 of N. H1 not supported for either arm. Source lessons surfaced by the Stop in about a third of the cases with one in
the store.

## Budget and order

Hard cap **$15** for everything (learning, Stop, arms, judge, pilots); every call logged with a running total in
`transfer/calls.jsonl` (`common.claude` and borg's shim); both refuse calls from $14.30 on, so calls in flight
cannot cross $15. Haiku is not used: every step here is one the earlier runs ran on Sonnet.

1. Offline first (0 tokens): data, contexts, pool, episodes; L retrieval.
2. Learning (≈ $3.2 expected; stopped if it passes $4.5, then the store is used as it is and stated).
3. Zero-token checks on 8 cases: per-case stores, BM25-only Stop with gate on/off, source lessons in store.
4. Cases run in a **pre-registered seeded order** (shuffle of the 149, seed 20261012, `cases.jsonl` `order`) in
   batches of 10: Stop → F → N → L → judge. Before each batch: stop if spend + 10 × the mean cost per case so far would
   pass $14.30. The n reached depends only on cost, never on results; it is reported. Expected ≈ 60–75 cases.
   Every arm runs on the same cases.

## Caveats known now

- Concept-enriched pool (above); 199 of 1,291 corrections learned.
- Concepts were matched by Haiku and canonicalised within category; "concept source" is that label, not a verified
  restatement.
- Third-party repos and users; the transcript is the only evidence; 11 cases have no earlier activity in the session.
- One sample per arm and case; Sonnet judges Sonnet.

## Amendment 1 (2026-10-12, after learning and the zero-token checks; before any Stop, arm or judge call)

- **Learning done:** 199 episodes → **156 lessons** (185 versions; 29 merges; 14 "not a lesson"), **$3.43**
  (199 Sonnet calls, $0.017 each). 131 of the 199 episodes have agent text (the mining kept none for the rest).
- **Per-case stores:** 149 built, median 83 lessons (max 153); **123 of 149 hold a lesson learned from a concept
  source.** A check over all 15,238 (store, lesson, episode) references finds 0 from the case's repo (by name), 0 not
  strictly earlier, 0 the case itself.
- **12 cases have no timestamped row before the correction** (11 OpenCode/Cursor sessions log no timestamps at all;
  1 opens its session). Their earliest time is set to the session's `created_at` minus 20 days (over the 1,964 dated
  sessions of the 5,680 set, `created_at` − first prompt is ≤ 18.5 days for 99%). Flagged `time_inferred`; this
  bound is likely but not proven, so results are also reported without them. Case ids and order are unchanged.
- **System gate:** BM25-only Stop replays (`BORG_MATCH=bm25`, 0 tokens) on the first 8 cases of the order surface
  identical lesson sets with the gate on and with `BORG_SYSTEM_GATE=off` (8/8), as the code says (learned lessons
  carry no scope). **The gate stays on (as built).** In those 8 replays word matching alone never surfaced the source
  lesson; L's top 10 holds a source correction for 31 of the 130 cases with a strict source.
- **Batches:** `transfer.py run` implements the pre-registered order and cost-only stopping rule (stop before a batch
  if spend + 10 × mean cost per finished case > $14.30).

## Amendment 2 (2026-10-12): requested cap raise not applied

After the run stopped, a relayed request asked to raise the cap to $45 and run all 149 cases. The permission system
refused the change, because a relayed message is not the user's own approval. So the cap stays at $15, and the results
below cover the **70 cases** that the pre-registered cost rule allowed. The harness, order and stores are unchanged, so
cases 71–149 can be run later by raising `CAP_USD` and `STOP_AT` in `transfer.py`. Disclosure: the author saw the catch
rates of the first batch (10 cases) while checking the pipeline. That did not change anything in the run or the
analysis.

## Results (2026-10-12, 70 of 149 cases, seeded order)

Run data: `transfer/` (`summary.json`, `borg_hook.jsonl`, `arm_{N,L,F}.jsonl`, `judge.jsonl`, `calls.jsonl`). There
were 0 errors in any Stop, arm or judge call. The 70 cases come from 30 repos (19 of them `entireio/cli`). 4 of the 70
have inferred times. The judge rates 27 of the 70 as genuine pushback. In 6 cases no arm had an item, so the judge was
not called and each counts as a miss for every arm.

### Hit rate (strict: at least 1 item judged `yes`; Wilson 95% CI)

| Subset | n | N | L | F | B-block | B-all |
|---|---|---|---|---|---|---|
| **All** | 70 | 12.9% [6.9, 22.7] | 11.4% [5.9, 21.0] | **17.1%** [10.1, 27.6] | 2.9% [0.8, 9.8] | 4.3% [1.5, 11.9] |
| Excluding `entireio/cli` | 51 | 13.7% | 13.7% | 15.7% | 3.9% | 5.9% |
| Authorship or ND signal | 18 | 5.6% (1) | 11.1% (2) | 5.6% (1) | 5.6% (1) | 11.1% (2) |
| No signal | 52 | 15.4% | 11.5% | 21.2% | 1.9% | 1.9% |
| Genuine (judge) | 27 | 22.2% | 18.5% | 25.9% | 3.7% | 7.4% |
| Strict source exists | 63 | 14.3% | 12.7% | 17.5% | 1.6% | 3.2% |
| Source lesson in store | 59 | 13.6% | 11.9% | 16.9% | 1.7% | 3.4% |

Lenient hit rate (`yes` or `partial`), all 70 cases: N 37.1%, L 40.0%, F 45.7%, B-block 7.1%, B-all 17.1%. Items per
case: N 3.8, L 3.9, F 3.9, B-block 2.0, B-all 4.0.

### Paired differences (strict, points; cluster bootstrap over repos, 95% CI)

| Pair | All (70, 30 repos) | Excl. `entireio/cli` (51, 29) | Signal (18, 11) | No signal (52, 23) | Lenient, all |
|---|---|---|---|---|---|
| **B-all − N** | **−8.6 [−18.2, +1.4]** | −7.8 [−20.4, +3.4] | +5.6 [0.0, +13.8] | −13.5 [−27.3, −2.4] | −20.0 [−34.0, −9.5] |
| **F − N** | **+4.3 [0.0, +8.3]** | +2.0 [0.0, +7.0] | 0.0 | +5.8 [0.0, +10.8] | +8.6 [0.0, +14.3] |
| L − N | −1.4 [−5.2, +4.1] | 0.0 [−5.2, +5.5] | +5.6 [0.0, +15.8] | −3.8 [−7.7, 0.0] | +2.9 [−2.4, +9.5] |
| F − L | +5.7 [−2.3, +11.6] | +2.0 [−4.3, +8.9] | −5.6 | +9.6 [0.0, +15.5] | +5.7 [−2.3, +11.6] |
| B-all − L | −7.1 [−16.4, 0.0] | −7.8 [−18.8, +2.0] | 0.0 | −9.6 [−22.5, 0.0] | −22.9 [−37.7, −12.0] |
| F − B-all | +12.9 [0.0, +22.4] | +9.8 [−1.8, +23.3] | −5.6 | +19.2 [+5.0, +31.9] | +28.6 [+14.9, +42.5] |
| B-block − N | −10.0 [−20.4, 0.0] | −9.8 [−22.9, +1.9] | 0.0 | −13.5 [−27.3, −2.4] | −30.0 [−44.2, −17.3] |

Without the 4 cases with inferred times (66 cases): F − N = +3.0 [0.0, +6.4] and B-all − N = −9.1 [−19.4, +1.3].

### Retrieval (0 tokens)

In 59 of 70 cases the store holds a lesson learned from a concept source. Borg's Stop **surfaced it in 11** of those
(asked in 7). L's top 10 holds a source correction in 18. Stop activity: the prefilter skipped 24 turn ends, 46 called
the consequence model, 19 blocked (138 lessons asked), 143 lessons were handed over free, and 25 turn ends surfaced
nothing. Of the cases where a source lesson was surfaced, 7 were misses for both B and F.

### Examples (verbatim, trimmed)

Hits (another arm caught the point and N did not):
- **t147** (dayhaysoos/nimbus, *pass subprocess arguments as argv elements*). Engineer: "so why would this fail? mind
  you I'm just copy/pasting the commands you're giving me. I'm not adding quotes …". F: "The agent dismissed the
  smart-quote issue as the user's paste problem rather than making the CLI robust or confirming it isn't contributing
  to the failure."
- **t027** (marcus-sa/osabio, *match external api schema*). Engineer: "what are u talking about ? it is literally
  defined right here in the spec: …/server/tools#output-schema". B-all (a lesson learned in another repo): "Before
  asserting that an external tool or API (hook, protocol, CLI) does not provide a field, have I checked its official
  documentation rather than inferring from our own parsing structs …?"
- **t125** (entireio/cli, *place code in the module owning its responsibility*). Engineer: "`strategy` is the wrong
  home for `DeclaredPushRemoteForBranch`; `gitremote` has already claimed that job …". F: "… instead of the deeper fix
  of moving the general 'which remote' resolver out of `strategy` so the package keeps exactly one exported answer."

Misses (the source lesson was surfaced, and neither B nor F caught the point):
- **t107** (polecatspeaks/StarCar, *include all discriminating fields in identity keys*). Engineer: "five rounds
  under one runtime id collapse to **one** row carrying round 5's outcome …". The surfaced lesson was about per-identity
  store keys for recovery shares: the same concept, but too far from this situation to name the point.
- **t048** (pc035860/agent-tail, *extract duplicated logic into shared helper*). Engineer: "fallback 路徑重複建構
  listArgs …" (the same list built twice). The surfaced lesson: "Before re-implementing logic like resolving the git
  directory via a raw git command, have I checked for an existing helper …?" Neither B nor F turned it into "these two
  blocks repeat each other".

### Cost

**$13.27 of the $15 cap**: learning $3.43 (199 calls), Stop $4.81 (46 consequence-model calls, $0.105 each), N $0.94,
L $1.43, F $1.15, and judge $1.51 (64 calls). That is $0.14 per case after learning. Zero-token checks cost $0.

### Verdicts against the pre-registration

**H1 is not supported for either arm.**
- **B-all − N = −8.6 [−18.2, +1.4].** The CI includes 0, so transfer is not shown. The point estimate is negative,
  and the lenient difference is −20.0 [−34.0, −9.5]. Borg's lessons, as surfaced, do not anticipate the cross-repo
  correction better than no memory, as in the 15-repo run.
- **F − N = +4.3 [0.0, +8.3].** The CI's lower bound touches 0 and does not exclude it, so this is not a win under the
  pre-registered rule. It is the only memory arm with a positive point estimate on every main subset. Most of it comes
  from cases without an authorship or ND signal, and without `entireio/cli` it shrinks to +2.0.
- **L − N = −1.4.** Other repos' raw corrections add nothing.

Predictions: N 15–30% was a slight miss (12.9%), B-all below N held, F and L within ±5 of N held, and H1
unsupported held.

**Power.** 70 cases with hit rates near 13% leave paired CIs about ±8–10 points wide. A transfer effect smaller than
about 8 points cannot be told from 0, and the signal subset (18 cases) is far too small to say anything. Running all
149 cases would narrow the CIs by about 1/√2. That is enough to decide whether F's +4 is real, but not to detect a
smaller effect.

**Reading.** The memory side mostly works: 84% of cases have the concept-source lesson in their store. Borg's Stop
surfaces it in only 19% of those, and when it does, the generic lesson rarely names the specific point. When Sonnet
rephrases Borg's selection for the turn (F), it does slightly better than no memory, but not by a margin this test can
confirm. The pool was enriched to favour memory, so a null result here is a strong negative for cross-repo transfer of
these lessons as Borg currently surfaces them.

### Caveats (beyond those pre-registered)

- Only 70 of the 149 cases were run (the budget, and Amendment 2).
- Despite the mining's paste filter, many corrections are pasted reviews from other agents (`<teammate-message>`,
  "Entire Trail review comment", numbered findings). Nobody could anticipate these from the turn.
- 68 of the 199 pool episodes have no agent text.
