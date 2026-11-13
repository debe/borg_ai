//! What happened after the conscience asked a lesson: the signal for down-weighting lessons
//! that are generic or do not help, and for trust per engineer × area (trust.rs).
//!
//! `$BORG_HOME/outcomes.jsonl` is append-only and deterministic, one record per line:
//! - `asked`: the conscience asked these lessons at a stop or live, or delivered them as context (early,
//!   or a graded check's 2s and 1s handed over free; hook, 0 tokens);
//! - `asked_then_corrected`: after lesson X was asked, the engineer still made a correction in the
//!   same session that `borg learn` merged into X (same concern): asking did not prevent it;
//! - `asked_not_corrected`: the session ended, or has been idle for `IDLE_MS`, and no later
//!   correction was merged into X. The idle rule keeps both kinds coming from every `borg learn`
//!   run: without it, a session whose `end` hook never ran could only ever add corrected counts.
//!
//! A correction that contradicts a lesson asked earlier in the session (`supersedes`) also marks that
//! lesson `asked_then_corrected`; when the conscience rendered asked lessons as a disagreement (the
//! `asked` record's `groups`), a correction siding with one of them marks the other sides, not the
//! one it agreed with (`contradicted`).
//!
//! - `asked_then_changed` (hook, no extra call): at a later Stop of the session, the consequence model's
//!   call (predict.rs) also judged, by meaning, that the drone changed its work on the lesson's point
//!   and that the change avoids the consequence: a use, the deposit that keeps a lesson active
//!   (memory.rs). The drone's `<id>: applies|skip` is evidence in that prompt, never a label alone;
//! - `asked_ignored` (hook): the same judge found the lesson's point came up and the drone changed
//!   nothing on it. Before 0.7.5 (`judge` < `NEVER_SPLIT`) the judge had no verdict for a point that
//!   never came up and wrote those as `asked_ignored` too, about nine in ten of them by the later
//!   judge's split: such a record is censored as `asked_not_arisen` is (`ambiguous`, `is_result`).
//! - `asked_not_arisen` (hook, 0.7.5 on): the same judge found the lesson's point never came up in the
//!   work: nothing to change, so nothing about the lesson. It settles the delivery (no further try),
//!   is censored for beliefs, trust, the statistics and the team (`is_result`), and the selection model
//!   reads it as "nothing in the work" (select/summary.rs `Label::NotArisen`).
//! - `asked_consequence_remains` (hook, 0.7.5 on): the drone changed its work on the point, but the work
//!   still leads to the consequence. As an unclear verdict: measured, never a result, pending for another
//!   try. Before 0.7.5 such a verdict was written as `asked_unclear`, which therefore mixes both there.
//!
//! Every judge result (0.7.5 on) carries the judge's short reason per lesson (`reasons`, redacted,
//! predict.rs `JUDGE_REASON_CHARS`). Local: never shared.
//! - `vetoed` (`borg no`, veto.rs, 0 tokens): the engineer said no to the session's work; every lesson
//!   shown in the session before it is credited, the strongest negative result.
//!
//! - `regretted` (`borg learn`, 0 tokens): a regret episode (a later fix of a decision, or a review finding;
//!   learn/regret.rs) generalised into a lesson blames the lessons asked in the deciding session before the
//!   decision (`regretted`): asking did not prevent it. `credit` carries each lesson's share of the blame;
//!   memory.rs weighs it below a correction. Deliberately not read by trust.rs (`sessions`), by `weight`
//!   (the legacy single-number view) or by attention.rs: a regret is about a lesson, not about an
//!   engineer following it.
//!
//! - `tested` (`borg learn`, 0 tokens): the session's own test runs (hook.rs `capture_test`) ended green
//!   after a lesson was asked as a question (a free delivery is none): a failed run after the ask, then a
//!   pass, or a pass after the drone changed its work on the lesson's point (`asked_then_changed`); never
//!   for a question the drone ignored (`asked_then_ignored`). A
//!   weak credit (memory.rs `TESTED_W`), once per (session, lesson) (`tested`). It is about the work,
//!   not about an engineer following a lesson: never a use, and not read by trust.rs, `weight` or
//!   attention.rs.
//!
//! An unclear verdict (`asked_unclear`, measured, never a result), a remaining consequence
//! (`asked_consequence_remains`, the same), or a session whose later turn ends
//! never reach the model (the prefilter skipped them, or BORG_MATCH=bm25), leaves the lesson pending:
//! `settle` turns it into `asked_not_corrected` with the reason it had no verdict (`reason`). That is
//! censored for beliefs (memory.rs): no correction followed, but whether the drone acted on the lesson
//! is unknown.
//!
//! - `delivery` (hook, local): per graded check and lesson, the route delivery actually took (`route`:
//!   asked, free, placebo, dropped because done or over the character budget, never delivered) and why,
//!   so what a check decided (`graded`) and what reached the drone never disagree unseen.
//! - A placebo (hook.rs `#borg-placebo`): an apply the check routed nowhere, judged blindly where a free
//!   delivery would be. Its results carry channel `placebo` (`PLACEBO`): the judge's floor, no result for
//!   any reader that learns from results (`is_result`), never shared. One the judge never gave a verdict
//!   settles as `asked_not_corrected` on that channel with its reason (`settle_with`), as an unjudged ask
//!   does: the selection summary censors it (select/summary.rs `settled`).
//!
//! `asked` and `graded` records carry `p`, each lesson's probability of being asked under the policy in
//! force at that check (its propensity), for off-policy evaluation of another policy from this log. An
//! `asked` record from a Thompson slot draw (the bm25 and non-graded paths) also carries that draw's
//! pool (`pool`, `base`, `scoped`): every competitor with its score before the usefulness factor, so
//! `borg debug ope` (ope.rs) can recompute the propensity under other belief weights. Local only: the team
//! projection drops them.
//!
//! **Team outcomes.** The raw log stays on this machine (it holds session ids and repos). `publish`
//! appends a redacted, deterministic projection of the results the team may see (`SHARED_KINDS`) to
//! `outcomes-team.jsonl` (`TEAM_FILE`), which the mesh syncs (borg-mesh lines.rs `SYNCED`): kind,
//! lesson ids, time, propensity, channel, the hashed session and repo (`tag`), a regret's hashed cause
//! and credit, and the node, never raw sessions, repos, causes, groups, grades or text. Only the belief readers opt in to the team view
//! (`read_team`): memory and trust in the conscience (conscience.rs `load_as`, folded incrementally by
//! facts.rs into the same view), `borg trust`, `borg memory` and borg-web's lessons. learn.rs (it would
//! settle remote sessions), attention.rs (labels and
//! the explore counter are this node's policy), stats.rs (rollups sync their own counts), install.rs and
//! the hook's pending/judge read this machine's log only. BORG_SHARE_OUTCOMES=off publishes and reads
//! nothing of the team's.
//!
//! A session counts once per lesson. If a resumed session gets both results, corrected wins.
//! memory.rs derives activation and the usefulness belief from these results; `weight` below is the
//! earlier, single-number view (kept for reporting and for comparison in the R1 replay).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::jsonl::{Each, Str, Text};

pub const ASKED: &str = "asked";
pub const CORRECTED: &str = "asked_then_corrected";
pub const NOT_CORRECTED: &str = "asked_not_corrected";
pub const CHANGED: &str = "asked_then_changed";
pub const IGNORED: &str = "asked_ignored";
/// The judge template version (predict.rs `JUDGE_VERSION`) from which a point that never came up has
/// its own verdict (`NOT_ARISEN`, 0.7.5); an older judge wrote it as `IGNORED`.
pub const NEVER_SPLIT: u8 = 3;
/// One graded check: every lesson the model graded as applying (`lessons`, in its order, with
/// `grades` and `ranks`), and which of them the check chose to ask (`chosen`). The denominator for
/// learning how many to ask: a lesson that applied but was not chosen can still turn out needed.
pub const GRADED: &str = "graded";
/// A lesson a graded check of the session found applying but did not ask, which a later correction
/// in that session then merged into or superseded: a question that was needed and not asked (`missed`).
pub const MISSED: &str = "applied_then_corrected";
/// The engineer vetoed the session's work (`borg no`, veto.rs): every lesson shown in the session
/// before it is credited. The strongest negative result.
pub const VETOED: &str = "vetoed";
/// A lesson asked in a session before a decision that a later fix (or a review finding) showed was
/// wrong (learn.rs `credit_regrets`): asking did not prevent it. `cause` names the regret episode and
/// `credit` carries the share of the blame, (0, 1]. Weaker than a correction (memory.rs `REGRET`).
pub const REGRETTED: &str = "regretted";

/// The session's own tests went green after the lesson was asked (`tested`): red then green, or green
/// after a change on the lesson's point. Once per (session, lesson), dated at that pass.
pub const TESTED: &str = "tested";

/// The judge saw the lesson's delivery but could not tell (an `unclear` verdict): the lesson stays
/// pending for another try. A label-health measurement only: no belief, trust, statistic, team line or
/// off-policy decision reads it (`counts`). Before 0.7.5 a change that still leads to the consequence
/// was written as this kind too (now `REMAINS`), so an older record means either.
pub const UNCLEAR: &str = "asked_unclear";

/// The judge found the lesson's point never came up in the work since its delivery (`not_arisen`,
/// predict.rs `Verdict::NotArisen`): a verdict that settles the delivery (`settles`), but no evidence
/// about the lesson's usefulness, so no result (`is_result`) for beliefs, trust, the statistics, the
/// team file or the off-policy log. The selection summary reads it as "nothing in the work": no change
/// on the lesson's point (select/summary.rs `Label::NotArisen`).
pub const NOT_ARISEN: &str = "asked_not_arisen";

/// The judge found the drone changed its work on the lesson's point but the work still leads to the
/// consequence (`consequence_remains`): `UNCLEAR`'s semantics (pending for another try, a measurement
/// only, no result), kept apart so the pure unclear share is measured.
pub const REMAINS: &str = "asked_consequence_remains";

/// A judge result's `at` when the session-end judge gave it (hook.rs `judge_session`, at `borg learn
/// --session`): with the whole session's work in view, after every check of the session. Protocol: empty
/// for a check's verdict (a stop or a live check), so its records stay as before.
pub const AT_END: &str = "end";

/// Whether a judge result of `kind` settles its delivery: a use (`CHANGED`), an ignore (`IGNORED`) or
/// nothing to judge (`NOT_ARISEN`). An unclear verdict and a remaining consequence leave it pending.
#[must_use]
pub fn settles(kind: &str) -> bool {
    matches!(kind, CHANGED | IGNORED | NOT_ARISEN)
}

/// What delivery did with a graded check's decision, per check and lesson (hook.rs): `route` (one of
/// `ROUTES`), `reason`, `check` the check that decided it. The latest record per (check, lesson) is
/// the route it took (a session that ends with a lesson queued gets `never_delivered`; resumed, its
/// later delivery supersedes that). Local and never a result: the denominator of what each route led
/// to, read by the selection replay (select.rs), not by beliefs.
pub const DELIVERY: &str = "delivery";

/// The session's end hook ran (hook.rs `on_end`), after its `never_delivered` records: one per end, a
/// resumed session's again at its next. Local and never a result: where the selection summary counts a
/// session's delivery tallies (select/summary.rs), whatever else the session logged, so a session with
/// nothing queued and no ask left open counts too.
pub const ENDED: &str = "ended";

/// The channel of a placebo's judge results (`asked_then_changed`, `asked_ignored`, `asked_unclear`, and
/// from 0.7.5 `asked_not_arisen`, `asked_consequence_remains`): a
/// graded apply never delivered, judged blindly where a free delivery would be (hook.rs `#borg-placebo`).
/// Its "changed" rate is the judge's own false positives plus what the drone would have done anyway, so
/// it is never a result about the lesson (`is_result`).
pub const PLACEBO: &str = "placebo";

/// Whether `o` is evidence about its lessons for the readers that learn from results (memory.rs, trust.rs,
/// the statistics, the team file): not a placebo's result (`PLACEBO`), not an unclear verdict
/// (`UNCLEAR`), not a change that leaves the consequence (`REMAINS`) and not a point that never came up
/// (`NOT_ARISEN`, censored), and not an ignore by a judge that could not tell it from one (`ambiguous`).
#[must_use]
pub fn is_result(o: &Outcome) -> bool {
    o.channel != PLACEBO && !matches!(o.kind.as_str(), UNCLEAR | REMAINS | NOT_ARISEN) && !ambiguous(&o.kind, o.judge)
}

/// Whether a record of `kind` by judge version `judge` is an ignore written before the judge could say
/// the point never came up (`NEVER_SPLIT`): no evidence about the lesson, censored as `NOT_ARISEN` is.
/// A team record without a version (a peer before 0.7.10) counts as such an ignore.
#[must_use]
pub fn ambiguous(kind: &str, judge: u8) -> bool {
    kind == IGNORED && judge < NEVER_SPLIT
}

/// Why a lesson settled as `asked_not_corrected` had no verdict (`reason`, `settle_why`). Protocol: the
/// judge gave one (`UNJUDGED_NONE`, the settle only says no correction followed); its tries ran out on
/// unclear or missing verdicts (hook.rs `MAX_JUDGE_TRIES`); a fuller judge call left it out
/// (hook.rs `MAX_PENDING`); its last judge call failed; no model call came after its delivery; or (a
/// placebo) a later delivery of the lesson in the session took its place in the judge's queue
/// (`UNJUDGED_LATER`, hook.rs `pending`).
pub const UNJUDGED_NONE: &str = "judged";
pub const UNJUDGED_TRIES: &str = "tries";
pub const UNJUDGED_SLOTS: &str = "slots";
pub const UNJUDGED_FAILED: &str = "failed";
pub const UNJUDGED_NO_CALL: &str = "no_call";
pub const UNJUDGED_LATER: &str = "later";

/// Why a `graded` record's check took the fixed rule instead of the learned policy in force (`reason`;
/// empty when it decided by the policy in force, hook.rs `decide`). Protocol: another check held the
/// store's decision lock past hook.rs `DECIDE_WAIT`, the lock could not be taken at all, or the learned
/// decision's record would not or did not land in the log its gate is folded from (learning off, a
/// `BORG_CONSCIENCE` outside the store the hook records to, a failed append), so its gate would never be
/// debited for it. Only the learned policy reads the gate and takes the lock; such a record carries
/// `policy` fixed and is replayed as the fixed rule.
pub const DECIDED_BUSY: &str = "lock_busy";
pub const DECIDED_NO_LOCK: &str = "lock_failed";
pub const DECIDED_NO_RECORD: &str = "unrecorded";

/// What one correction adds against a lesson, in units of an ignore: a correction of the same concern
/// right after asking is strong evidence the question did not help, a session without one weak evidence
/// (the engineer may not have looked). Baseline: design-chosen (no evaluation behind the 2), used by
/// `weight` and memory.rs `Memory::beta_with` alike; `borg debug ope` (ope.rs) estimates other belief
/// weights offline, never applies them. Fixed by dated decision (2026-10-31): 1–2 corrections after an
/// ask per store, and `borg debug ope` finds 0 usable decisions.
pub const CORRECTED_W: u32 = 2;

/// A session without asks or episodes for this long is treated as ended (24h). Guard: a session whose
/// `end` hook never ran would otherwise never settle; its state file must be as quiet (learn.rs).
pub const IDLE_MS: i64 = crate::clock::DAY_MS;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub kind: String,
    pub session: String,
    pub lessons: Vec<String>,
    pub ms: i64,
    pub repo: String,
    /// `asked` only: the asked lessons rendered as one disagreement (conscience.rs `disagreements`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<Vec<String>>,
    /// How the lessons were asked (`asked` and the judge's results): `stop` (a turn-end block), `live`
    /// (a live check during the turn, before 0.7.6), `early` (early delivery, conscience.rs `early`),
    /// `queued` (a graded check's 2s and 1s, handed over free) or `placebo` (`PLACEBO`: never shown,
    /// judged blindly where a free delivery would be). Empty in records from before delivery modes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel: String,
    /// `graded` and `asked`: each lesson's grade (3 certain, 2 likely, 1 worth a thought) and its rank in
    /// the model's applies (0 first), aligned with `lessons`, and how many lessons the check graded as
    /// applying; on `asked`, 0 where no check of the session graded the lesson. For learning how many to
    /// ask (attention.rs reads them from `graded`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grades: Vec<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranks: Vec<u16>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub applied: u16,
    /// `graded` only: the lessons the check chose to ask.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen: Vec<String>,
    /// `graded` only: the check explored, asking every apply up to `attention::RAIL` whatever the cut,
    /// so its outcomes label the cut (attention.rs).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub explore: bool,
    /// `regretted`: the regret episode (`R…`/`V…` id). The idempotence key with the lesson.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cause: String,
    /// `regretted` only: the lesson's share of the regret, (0, 1].
    #[serde(default, skip_serializing_if = "is_zero_f")]
    pub credit: f64,
    /// `asked` and `graded`: per lesson (aligned with `lessons`), the probability that the policy in force
    /// at that check asked it: 1 or 0 under the fixed graded rule, the explore mixture under the learned
    /// cut (attention.rs `propensities`), the share of Thompson draws that give it a slot in
    /// `budget`/`widen` (conscience.rs `propensity`), the share that delivers it early (conscience.rs
    /// `early_propensity`), 1 for a free hand-over of a graded check's 2s and 1s. Rounded to 4 decimals
    /// (`round_p`). Empty in records from before propensities and when unknown (BORG_ONLY, a state file
    /// from an older binary). For off-policy evaluation (ope.rs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub p: Vec<f64>,
    /// `asked` from a Thompson slot draw only (conscience.rs `budget_pool`/`widen_pool`, hook.rs
    /// `#borg-pool`): the pool the check drew from, in ranking order, the asked lessons included; each
    /// one's score without its usefulness factor (conscience.rs `pool_base`, unrounded so a replay is
    /// exact); and whether it is scoped. Aligned with each other, not with `lessons`. Empty for graded
    /// and live asks and free deliveries. Local: never shared (`share`). For `borg debug ope` (ope.rs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pool: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub base: Vec<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scoped: Vec<bool>,
    // The selection fields (select.rs). Every one is left out when empty, so a record without them
    // writes the line an older binary wrote, and an older line reads with them empty. Local: `share`
    // copies none of them.
    /// The graded check (select.rs `check_id`): on `graded`, and on every record of a lesson that check
    /// routed (its delivery, the judge's result, a placebo's), so a result joins the decision that made
    /// it. Empty in records from before checks had ids.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub check: String,
    /// `graded`: the selection policy in force (select.rs `Policy::name`: fixed, cap, attention,
    /// learned). Empty in records from before select.rs, which read as the fixed rule (`policy_of`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub policy: String,
    /// `graded`: the fit the policy decided with (select/policy.rs `Fit::key`); empty for the prior.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fit: String,
    /// `graded`: the selection summary the decision read (select/summary.rs `Summary::key`: the bytes
    /// folded and their hash), and what set its routes (select.rs `Log::source`). A learned check reads
    /// the summary again under the store's decision lock after its model call (hook.rs `decide`), so
    /// every learned decision recorded before it is in it; a check by a policy that reads no gate, and
    /// one that fell back to the fixed rule (`reason`), names the summary it read at its start, which
    /// records of checks that decided meanwhile may follow. Records other than decisions (a delivery, a
    /// verdict) may still land between that read and this record; `borg debug select` P1 replays from
    /// the logged state.
    /// Empty in older records and for a failed call.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub seen: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// `graded`: the lessons the check handed over free, in delivery order (`free_of` for older records).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub free: Vec<String>,
    /// `graded`: per lesson (aligned with `lessons`), its probability of being handed over free under
    /// the policy in force, as `p` is of being asked; rounded like `p`. Empty in older records, where
    /// a free lesson's was 1 under the fixed rule.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pf: Vec<f64>,
    /// `graded`: the applies the check did not deliver and drew for the placebo arm (judged blindly
    /// where a free delivery would be, never shown), and each undelivered apply's chance of that draw.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub placebo: Vec<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub psi: f64,
    /// `graded`: the graded applies that could not go free at this check because the session already
    /// had them delivered (select.rs `Ctx::had`, conscience.rs `graded_without`), in the applies' order:
    /// the applies among the session's deliveries when the check decided, and among the lessons waiting
    /// in its queue to be asked or handed over by another check (hook.rs `decide`, `waiting_in`), so the
    /// record alone holds every `had` that bears on its decision.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nofree: Vec<String>,
    /// `graded`, a check of the randomised question-count test (select/policy.rs `Decision::xk`,
    /// sources `experiment`, `experiment_control` and `gate`): `xk` the fixed rule's delivered non-3
    /// lessons that may go free (k), `xs` how many of them it asked (s, drawn uniformly in 0..=k; 0 is
    /// logged). None on every other record, so older lines and checks outside the test write back
    /// unchanged. Local like the rest of a graded record (`share`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xk: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xs: Option<u32>,
    /// `graded`: the collective's tolerance the check was decided under (select.rs `Log::alpha`, its
    /// `risk`), so a replay decides under it whatever the config says then; None at the default
    /// (select/policy.rs `ALPHA`), so older lines and default collectives write back unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha: Option<f64>,
    /// `graded`: the lessons the pool budget dropped from the model call that a tail sample showed
    /// anyway (whole.rs `tail`, under every policy), and each one's chance of being sampled (one chance
    /// for all of the check's tail: k drawn uniformly of the `eligible` − `shown` dropped, unrounded).
    /// Not in `shown` or `tokens`; each has its place past the budget's in `fill`, after the applies'
    /// (and, if it applies, among them too), so every place from `shown` to `eligible` − 1 is one
    /// dropped lesson, shown with chance `pt`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tail: Vec<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub pt: f64,
    /// An early delivery's record: per lesson (aligned with `lessons`), its score as a share of the best
    /// on-topic match, done or not (conscience.rs `early_pool`, the share `RELATIVE` bars; hook.rs
    /// `early_rel`), rounded as `p` is, for a learned early rule. A standing lesson has one too, though
    /// no score gates it; empty when no plain lesson was on topic to scale by.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rel: Vec<f64>,
    /// A delivery's actual route for every lesson of the record (one of `ROUTES`), and why: a drop's
    /// cause, or why a delivered lesson stayed unjudged (no later call, the judge's slots, its tries);
    /// on `graded`, why its check fell back to the fixed rule (`DECIDED_BUSY`, `DECIDED_NO_LOCK`,
    /// `DECIDED_NO_RECORD`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub route: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// `graded`, the pool the model graded: lessons eligible for it, lessons its budget showed, their
    /// size in tokens (the tail sample beside them in `tail`), and per lesson (aligned with `lessons`)
    /// its fill position among the shown (0 first), so the yield of each position can be learned from
    /// every check, a check without applies included; then, past the applies', one position per tail
    /// lesson (aligned with `tail`), so the yield past the budget is learned from the lessons a tail
    /// showed that did not apply too (select/summary.rs `tail_of`). A tail lesson's position goes on
    /// past the budget's: `shown` + its place among the lessons the budget dropped, in the order the
    /// fill met them (whole.rs `Filled::dropped`), so it is never below `shown`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub eligible: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub shown: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub tokens: u32,
    /// `graded`, the pool's token budget its check filled to (hook.rs `check`: `BORG_STOP_POOL_TOKENS`,
    /// else select.rs `pool_tokens`), so a check at a budget other than the baseline shows. 0: not
    /// logged (a record from before 0.7.3, or a check without the whole pool).
    #[serde(default, skip_serializing_if = "is_default")]
    pub budget: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fill: Vec<u16>,
    /// `graded`, the model call: the model (empty: the CLI's default), a hash of the prompt template,
    /// the harness, borg's version, the call's duration, and whether it failed (the check then asked
    /// nothing: fail open); then its cost (`usd`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt_hash: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub harness: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub dur_ms: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
    /// `graded`: the call's cost in dollars as the harness's CLI reported it (0: it reported none).
    /// Measured and shown (`borg debug select` P3), never a limit.
    #[serde(default, skip_serializing_if = "is_default")]
    pub usd: f64,
    /// The judge's results (`asked_then_changed`, `asked_ignored`): the judge template's version, 0 for
    /// the template before versions were logged; and whether the judge saw the question without the
    /// drone's `<id>: applies|skip` declaration (a random half are, to measure what it adds).
    #[serde(default, skip_serializing_if = "is_default")]
    pub judge: u8,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stripped: bool,
    /// The judge's results (0.7.5 on): per lesson (aligned with `lessons`), the judge's short reason for
    /// its verdict, redacted and at most predict.rs `JUDGE_REASON_CHARS`; empty where it gave none.
    /// Empty in older records. Local: never shared (`share`), never read as a result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    /// The judge's results (0.7.5 on): where the verdict was given, `AT_END` at the session-end judge
    /// (hook.rs `judge_session`); empty at a check (a stop or a live check) and in older records. Local.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub at: String,
    /// `graded` (0.7.7 on): the model's design moves of the work it checked (predict.rs
    /// `Prediction::moves`), and per apply (aligned with `lessons`) its one-line why; empty in older
    /// records and on a failed call. Local, as the whole record is (`share`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub moves: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub whys: Vec<String>,
    /// `graded` (0.7.7 on): the tokens the call showed past `tokens`, the boundary draw's and the tail
    /// sample's (`tail`), so a prompt's whole lesson size is `tokens` + `xtokens`; 0 without any, and in
    /// older records. Logged only: the latency fit reads `tokens`, the size a budget is predicted at.
    #[serde(default, skip_serializing_if = "is_default")]
    pub xtokens: u32,
    /// `graded` (0.7.7 on): the lesson line format of the compact prompt (whole.rs `Line`, `a1` or
    /// `a2`); empty without it (`BORG_STOP_FORMAT=classic`, the shortlist) and in older records.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub line: String,
    /// `graded` (0.7.7 on): how its check ran, `async` for a detached check (check_async.rs); empty for
    /// a stop's own check and in older records.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub run: String,
}

/// `route` values. Protocol: what a check decided for a lesson, then what delivery did with it.
/// Asked, handed over free, or judged as a placebo, as decided; or dropped at delivery because the
/// session had the lesson already (`ROUTE_DROPPED_DONE`) or the delivery budget was spent
/// (`ROUTE_DROPPED_CHARS`); or never delivered because the session ended first (`ROUTE_NEVER`); or (0.7.7
/// on) a detached check's waiting ask taken off the queue because a newer check asks the same lesson
/// (`ROUTE_SUPERSEDED`): the newer ask carries its delivery.
pub const ROUTE_ASKED: &str = "asked";
pub const ROUTE_FREE: &str = "free";
pub const ROUTE_PLACEBO: &str = "placebo";
pub const ROUTE_DROPPED_DONE: &str = "dropped_done";
pub const ROUTE_DROPPED_CHARS: &str = "dropped_chars";
pub const ROUTE_NEVER: &str = "never_delivered";
pub const ROUTE_SUPERSEDED: &str = "superseded";
pub const ROUTES: [&str; 7] = [ROUTE_ASKED, ROUTE_FREE, ROUTE_PLACEBO, ROUTE_DROPPED_DONE, ROUTE_DROPPED_CHARS, ROUTE_NEVER, ROUTE_SUPERSEDED];

/// The policy a `graded` record was decided under: its `policy`; for a record from before select.rs,
/// the fixed rule (`fixed`), unless it shows the one other policy an older binary logged,
/// `BORG_STOP_ATTENTION=on`'s learned cut (`attention`): it explored, or a propensity lies strictly
/// between 0 and 1 (attention.rs `propensities`). A learned cut that asked every apply up to its rail
/// logged only 0s and 1s, and an evaluation's `BORG_STOP_ASK_CAP` logged nothing of its cap: both read
/// as the fixed rule.
#[must_use]
pub fn policy_of(o: &Outcome) -> &str {
    match o.policy.as_str() {
        "" if o.explore || o.p.iter().any(|p| *p > 0.0 && *p < 1.0) => "attention",
        "" => "fixed",
        p => p,
    }
}

/// The lessons a `graded` record handed over free: its `free`; for a record from before select.rs
/// (`policy` empty), its policy's rule (`policy_of`) replayed on its grades: the fixed rule's 2s, then
/// 1s, at most `conscience::CAP`, when no lesson is a 3 (conscience.rs `graded_capped`), else none; the
/// learned cut's lessons after the ones it asked, at most `conscience::CAP` (attention.rs `split`).
#[must_use]
pub fn free_of(o: &Outcome) -> Vec<String> {
    use crate::conscience::{self, Challenge};
    if !o.policy.is_empty() || o.grades.len() != o.lessons.len() {
        return o.free.clone();
    }
    if o.grades.contains(&3) {
        return match policy_of(o) {
            "attention" => o.lessons.iter().filter(|l| !o.chosen.contains(l)).take(conscience::CAP).cloned().collect(),
            _ => Vec::new(),
        };
    }
    let ls: Vec<Challenge> = o.lessons.iter().map(|id| Challenge { id: id.clone(), ..Default::default() }).collect();
    let applied: Vec<(&Challenge, u8)> = ls.iter().zip(o.grades.iter().copied()).collect();
    conscience::graded_capped(&applied, None).1.into_iter().map(|c| c.id.clone()).collect()
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// A propensity rounded to 4 decimals, so lines stay deterministic and short.
pub fn round_p(p: f64) -> f64 {
    (p * 1e4).round() / 1e4
}

fn is_zero(v: &u16) -> bool {
    *v == 0
}

fn is_zero_f(v: &f64) -> bool {
    *v == 0.0
}

/// The outcomes store next to a lessons file.
pub fn path_beside(lessons: &Path) -> std::path::PathBuf {
    lessons.with_file_name("outcomes.jsonl")
}

/// The redacted outcomes the team sees, at the store root beside `outcomes.jsonl` (module doc).
pub const TEAM_FILE: &str = "outcomes-team.jsonl";
/// Beside `TEAM_FILE`, never synced: the length of `outcomes.jsonl` at the last `publish`.
pub const PUBLISHED: &str = "outcomes-team.published";

/// The kinds `publish` shares. A whitelist: `graded` and any later kind stay local until deliberately
/// added here.
///
/// Closed under memory.rs `derive`: every kind that changes a lesson's result for a session, or its
/// belief, travels, so a peer folds the same memory. `vetoed` outranks the session's `changed` (shared
/// alone, a lesson vetoed here would gain a use and spread there); `regretted` carries its `credit` and
/// its hashed `cause` (`Shared::c`), the key derive dedupes regrets by. `graded` is the asking policy's
/// denominator, not a result, and never moves the activity clock (`memory::Clock` counts shared kinds).
///
/// Unguarded: any member can write a line under any session tag (tags are public in this file), so a
/// forged `asked_then_corrected` or `vetoed` overrides a teammate's session on every node but that
/// teammate's, and a few forged `asked_then_changed` with fresh tags and two repo tags consolidate a
/// lesson. `node` is self-declared. The mesh admits only members' signed lines (borg-mesh origin.rs),
/// so this is the trusted-member model; the deferred guards (a session tag bound to its first signer,
/// consolidation from at least two signers) would close it.
///
/// `tested` is shared like `asked_then_changed`, the result it is weaker than: memory folds it, so a peer
/// without it would hold another belief. Both are this node's own reading of its session (the judge's,
/// the test runner's output), so the trusted-member model above is the same;
/// `team_view` counts a peer's `tested` line only for lessons the team's records show its session was
/// given. It never moves the activity clock either.
pub const SHARED_KINDS: [&str; 9] = [ASKED, CHANGED, IGNORED, CORRECTED, NOT_CORRECTED, MISSED, VETOED, REGRETTED, TESTED];

/// A session or repo as the team sees it: the first 32 hex digits of blake3("borg-outcome-v1\0" + s).
/// Unsalted on purpose: the same session hashes to the same tag on every node, so a node can drop its
/// own published copies (`read_team`).
pub fn tag(s: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"borg-outcome-v1\0");
    h.update(s.as_bytes());
    h.finalize().to_hex()[..32].to_string()
}

/// `tag`, keeping an empty repo empty.
fn tag_repo(repo: &str) -> String {
    if repo.is_empty() { String::new() } else { tag(repo) }
}

/// One line of `TEAM_FILE`. Never carries `id` and `version` together (the mesh would settle it as a
/// versioned record).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Shared {
    pub kind: String,
    /// `tag` of the session.
    pub s: String,
    pub lessons: Vec<String>,
    pub ms: i64,
    /// The publishing node (stats.rs `node`), self-declared: dedup does not depend on it.
    pub node: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub p: Vec<f64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel: String,
    /// `tag` of the repo: memory.rs counts distinct repos to consolidate a lesson.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub r: String,
    /// `tag` of a `regretted` record's cause (the regret episode): memory.rs counts each regret once.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub c: String,
    /// A `regretted` record's share of the blame.
    #[serde(default, skip_serializing_if = "is_zero_f")]
    pub credit: f64,
    /// An ignore's judge version (`Outcome::judge`), 0 on every other line: a peer reads an ignore without
    /// one as `ambiguous`. A node before 0.7.10 wrote no version, so after an upgrade `publish` appends a
    /// versioned copy of each of its ignores from judge 3; `team_parts` keeps both, and the result per
    /// (lesson, session) is a max, so the copy counts once.
    #[serde(default, skip_serializing_if = "is_default")]
    pub judge: u8,
}

/// `o` as the team sees it, or None for a kind not in `SHARED_KINDS` and for a placebo's result
/// (`is_result`): the judge's floor on this node, never a result a peer folds.
pub fn share(o: &Outcome, node: &str) -> Option<Shared> {
    (SHARED_KINDS.contains(&o.kind.as_str()) && is_result(o)).then(|| Shared {
        kind: o.kind.clone(),
        s: tag(&o.session),
        lessons: o.lessons.clone(),
        ms: o.ms,
        node: node.into(),
        p: o.p.clone(),
        channel: o.channel.clone(),
        r: tag_repo(&o.repo),
        c: tag_repo(&o.cause),
        credit: o.credit,
        // Only an ignore's: what `ambiguous` reads. Every other line stays as an older node writes it, so
        // `publish` (text dedup) appends no copy of it after an upgrade.
        judge: if o.kind == IGNORED { o.judge } else { 0 },
    })
}

/// Team outcomes on: `BORG_SHARE_OUTCOMES` is not `off`.
pub fn on() -> bool {
    std::env::var("BORG_SHARE_OUTCOMES").as_deref() != Ok("off")
}

/// Appends to `TEAM_FILE` in `home` the shared lines of `home`'s `outcomes.jsonl` it lacks; returns
/// how many. The lines are deterministic, so a re-run appends nothing (but once after an upgrade to
/// 0.7.10, a versioned copy of each judge-3 ignore, `Shared::judge`); two runs at once may append the
/// same line twice, which the mesh's union and the per-session count make harmless. Nothing when off.
/// A local log no longer than at the last publish (`PUBLISHED`, its length then) is not read again: the
/// log only grows, so nothing new can be in it.
pub fn publish(home: &Path) -> usize {
    let local = home.join("outcomes.jsonl");
    let team = home.join(TEAM_FILE);
    let mark = home.join(PUBLISHED);
    let len = std::fs::metadata(&local).map_or(0, |m| m.len());
    if !on() || len == 0 {
        return 0;
    }
    let published = std::fs::read_to_string(&mark).ok().and_then(|t| t.trim().parse::<u64>().ok());
    if team.exists() && published == Some(len) {
        return 0;
    }
    let mine = std::fs::read_to_string(&local).unwrap_or_default();
    let node = crate::stats::node(home);
    let text = std::fs::read_to_string(&team).unwrap_or_default();
    // This engineer's personal lessons (reach.rs) are no team's business, not even by id.
    let personal = crate::reach::personal_ids(home);
    let records = mine.lines().filter_map(|l| serde_json::from_str::<Outcome>(l).ok()).filter_map(|mut o| {
        let named = !o.lessons.is_empty();
        o.lessons.retain(|id| !personal.contains(id));
        (!named || !o.lessons.is_empty()).then_some(o)
    });
    let shared: Vec<String> = records.filter_map(|o| share(&o, &node)).filter_map(|x| serde_json::to_string(&x).ok()).collect();
    // The lines the team file holds and the ones added here, borrowed: none is copied to be compared.
    let mut have: std::collections::HashSet<&str> = text.lines().collect();
    let mut out = String::new();
    for line in &shared {
        if have.insert(line) {
            out.push_str(line);
            out.push('\n');
        }
    }
    if out.is_empty() {
        let _ = std::fs::write(&mark, mine.len().to_string());
        return 0;
    }
    use std::io::Write;
    let open = text.as_bytes().last().is_some_and(|c| *c != b'\n');
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&team)
        .and_then(|mut f| f.write_all(format!("{}{out}", if open { "\n" } else { "" }).as_bytes()));
    if written.is_err() {
        return 0;
    }
    let _ = std::fs::write(&mark, mine.len().to_string());
    out.lines().count()
}

/// The belief view of the outcomes beside `lessons`: this machine's log plus, unless
/// BORG_SHARE_OUTCOMES=off, the team's (`team_view`).
pub fn read_team(lessons: &Path) -> Vec<Outcome> {
    team_view(lessons, on())
}

/// Off: this machine's log as is. On: its records with each repo and cause as its `tag` (memory.rs only
/// compares them for distinctness, so beliefs are the same and comparable with the team's), then the
/// team's lines that count (`team_parts`). A remote session then counts once per lesson like a local
/// one (memory.rs `derive`, `sessions`). learn.rs keys on raw causes: it reads this
/// machine's log (`read`).
pub fn team_view(lessons: &Path, on: bool) -> Vec<Outcome> {
    let (mut out, team) = team_parts(lessons, on);
    if on {
        for o in &mut out {
            o.repo = tag_repo(&o.repo);
            o.cause = tag_repo(&o.cause);
        }
    }
    out.extend(team);
    out
}

/// This machine's records as written, and (on) every `TEAM_FILE` line of a shared kind whose session
/// is not one of this machine's (that drops this node's own published copies, whatever its node id),
/// once per (kind, session, lessons, ms, cause): one read of each file, so the two never disagree on
/// where the local records end (ope.rs reads both). A kind this binary does not share (a newer node's,
/// or an older one's hive verdict) is ignored, and so is a `tested` line naming a lesson the team's
/// records do not show its session was given.
pub fn team_parts(lessons: &Path, on: bool) -> (Vec<Outcome>, Vec<Outcome>) {
    let local = read(&path_beside(lessons));
    if !on {
        return (local, Vec::new());
    }
    // Each of this machine's sessions tagged once, however many records it has.
    let sessions: std::collections::HashSet<&str> = local.iter().map(|o| o.session.as_str()).collect();
    let mine: std::collections::HashSet<String> = sessions.into_iter().map(tag).collect();
    let text = std::fs::read_to_string(lessons.with_file_name(TEAM_FILE)).unwrap_or_default();
    let mut team = Vec::new();
    team_lines(&text, &mine, |x| {
        team.push(Outcome {
            kind: x.kind.to_string(),
            session: x.s.to_string(),
            lessons: x.lessons.iter().map(|l| l.to_string()).collect(),
            ms: x.ms,
            repo: x.r.to_string(),
            p: x.p.to_vec(),
            channel: x.channel.to_string(),
            cause: x.c.to_string(),
            credit: x.credit,
            judge: x.judge,
            ..Default::default()
        });
    });
    (local, team)
}

/// A record `fold_parts` hands over: one of this machine's, or a team line `team_parts` keeps.
pub(crate) enum Part<'r, 'a> {
    Local(&'r OutcomeRef<'a>),
    Team(&'r Team<'r, 'a>),
}

/// `team_parts` as a fold, for a reader that only folds them: `f` sees each of this machine's records,
/// then (on) each team line `team_parts` keeps, in its order, borrowed from the line. Neither log's
/// records are kept: this machine's log is read once, its sessions tagged once, and its text is gone before
/// the team's is read; of the team's lines only what `team_lines` must remember stays.
pub(crate) fn fold_parts(lessons: &Path, on: bool, mut f: impl FnMut(Part)) {
    let mine: HashSet<String> = {
        let text = std::fs::read_to_string(path_beside(lessons)).unwrap_or_default();
        let mut sessions: HashSet<Cow<str>> = HashSet::new();
        for o in text.lines().filter_map(|l| serde_json::from_str::<OutcomeRef>(l).ok()) {
            f(Part::Local(&o));
            if on && !sessions.contains(&o.session) {
                sessions.insert(o.session);
            }
        }
        sessions.iter().map(|s| tag(s)).collect()
    };
    if !on {
        return;
    }
    let team = std::fs::read_to_string(lessons.with_file_name(TEAM_FILE)).unwrap_or_default();
    team_lines(&team, &mine, |x| f(Part::Team(x)));
}

/// A `TEAM_FILE` line `team_lines` keeps, borrowed: its `SharedRef` fields but the unread `node`.
pub(crate) struct Team<'r, 'a> {
    pub(crate) kind: &'r str,
    pub(crate) s: &'r str,
    pub(crate) lessons: &'r [Text<'a>],
    pub(crate) ms: i64,
    pub(crate) r: &'r str,
    pub(crate) p: &'r [f64],
    pub(crate) channel: &'r str,
    pub(crate) c: &'r str,
    pub(crate) credit: f64,
    pub(crate) judge: u8,
}

/// The `TEAM_FILE` lines of `text` that count (`team_parts`), in file order: of a shared kind, of no
/// session tagged in `mine`, the first of their (kind, session, lessons, ms, cause, judge), and a `tested`
/// line only when the team's records show its session was given each of its lessons.
///
/// One read, no line kept whole: each is handed over as it is parsed. What stays is the lessons each
/// session was given and the dedup keys, both moved or borrowed out of their lines (a string copied only
/// when it held an escape). A `tested` line whose session was not given its lessons yet waits, and the
/// lines after it with it, until a later line gives them or the log ends: what a session was given only
/// grows, so each is decided as a read of the whole log decides it.
fn team_lines<'a>(text: &'a str, mine: &HashSet<String>, mut f: impl FnMut(&Team<'_, 'a>)) {
    let mut given: HashMap<Cow<str>, HashSet<Cow<str>>> = HashMap::new();
    // A map for its entry API, which lends the key out before it is kept.
    let mut seen: HashMap<TeamKey, ()> = HashMap::new();
    // From the first `tested` line not yet due on, the lines waiting their turn, in file order.
    let mut held: std::collections::VecDeque<Held> = std::collections::VecDeque::new();
    for x in text.lines().filter_map(|l| serde_json::from_str::<SharedRef>(l).ok()) {
        if !SHARED_KINDS.contains(&x.kind.as_ref()) {
            continue;
        }
        // The lessons each session's shared records name: a green run credits only lessons it was given.
        if x.kind != TESTED {
            given.entry(x.s.clone()).or_default().extend(x.lessons.iter().map(|l| l.0.clone()));
        }
        if !mine.contains(x.s.as_ref()) {
            let SharedRef { kind, s, lessons, ms, p, channel, r, c, credit, judge, .. } = x;
            // The judge in the key: an ignore's versioned copy is kept beside an older node's line (`Shared::judge`).
            if let std::collections::hash_map::Entry::Vacant(new) = seen.entry((kind, s, lessons, ms, c, judge)) {
                if held.is_empty() && due(new.key(), &given) {
                    f(&Held::view(new.key(), &r, &p, &channel, credit));
                } else {
                    held.push_back(Held { key: new.key().clone(), r, p, channel, credit });
                }
                new.insert(());
            }
        }
        while let Some(h) = held.front().filter(|h| due(&h.key, &given)) {
            f(&Held::view(&h.key, &h.r, &h.p, &h.channel, h.credit));
            held.pop_front();
        }
    }
    // The whole log read: a `tested` line still waiting was not given its lessons.
    for h in held.iter().filter(|h| due(&h.key, &given)) {
        f(&Held::view(&h.key, &h.r, &h.p, &h.channel, h.credit));
    }
}

/// A team line's dedup key in `team_lines`: (kind, session, lessons, ms, cause, judge).
type TeamKey<'a> = (Cow<'a, str>, Cow<'a, str>, Vec<Text<'a>>, i64, Cow<'a, str>, u8);

/// Whether the line keyed `key` may be handed over by what is known of `given`: any but a `tested` line,
/// and a `tested` line once its session was given each of its lessons.
fn due(key: &TeamKey, given: &HashMap<Cow<str>, HashSet<Cow<str>>>) -> bool {
    let (kind, s, lessons, ..) = key;
    kind != TESTED || lessons.iter().all(|l| given.get(s).is_some_and(|g| g.contains(&l.0)))
}

/// A team line `team_lines` holds until it may be handed over: its key and the rest of what `Team` reads.
struct Held<'a> {
    key: TeamKey<'a>,
    r: Cow<'a, str>,
    p: Vec<f64>,
    channel: Cow<'a, str>,
    credit: f64,
}

impl Held<'_> {
    /// The `Team` a line's key and the rest of it make, held or not.
    fn view<'r, 'a>(key: &'r TeamKey<'a>, r: &'r str, p: &'r [f64], channel: &'r str, credit: f64) -> Team<'r, 'a> {
        let (kind, s, lessons, ms, c, judge) = key;
        Team { kind, s, lessons, ms: *ms, r, p, channel, c, credit, judge: *judge }
    }
}

/// A `TEAM_FILE` line as `team_parts` reads it, borrowed from the line (a string with an escape is
/// copied): every line `Shared` reads, field for field, so the two take the same lines.
#[derive(Deserialize)]
struct SharedRef<'a> {
    #[serde(borrow)]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    s: Cow<'a, str>,
    #[serde(borrow)]
    lessons: Vec<Text<'a>>,
    ms: i64,
    /// Never read: required as in `Shared`.
    #[serde(borrow, rename = "node")]
    _node: Cow<'a, str>,
    #[serde(default)]
    p: Vec<f64>,
    #[serde(default, borrow)]
    channel: Cow<'a, str>,
    #[serde(default, borrow)]
    r: Cow<'a, str>,
    #[serde(default, borrow)]
    c: Cow<'a, str>,
    #[serde(default)]
    credit: f64,
    #[serde(default)]
    judge: u8,
}

/// A record of this machine's log as a fold reads it (`fold_parts`), borrowed from its line (a string with
/// an escape is copied): every field `Outcome` reads, in its order and with its types, so the two take the
/// same lines; the fields no fold reads are checked, not kept.
#[derive(Deserialize)]
pub(crate) struct OutcomeRef<'a> {
    #[serde(borrow)]
    pub(crate) kind: Cow<'a, str>,
    #[serde(borrow)]
    pub(crate) session: Cow<'a, str>,
    #[serde(borrow)]
    pub(crate) lessons: Vec<Text<'a>>,
    pub(crate) ms: i64,
    #[serde(rename = "repo")]
    _repo: Str,
    #[serde(default, rename = "groups")]
    _groups: Each<Each<Str>>,
    #[serde(default, borrow)]
    pub(crate) channel: Cow<'a, str>,
    #[serde(default, rename = "grades")]
    _grades: Each<u8>,
    #[serde(default, rename = "ranks")]
    _ranks: Each<u16>,
    #[serde(default, rename = "applied")]
    _applied: u16,
    #[serde(default, rename = "chosen")]
    _chosen: Each<Str>,
    #[serde(default, rename = "explore")]
    _explore: bool,
    #[serde(default, rename = "cause")]
    _cause: Str,
    #[serde(default, rename = "credit")]
    _credit: f64,
    #[serde(default)]
    pub(crate) p: Vec<f64>,
    #[serde(default, borrow)]
    pub(crate) pool: Vec<Text<'a>>,
    #[serde(default)]
    pub(crate) base: Vec<f64>,
    #[serde(default)]
    pub(crate) scoped: Vec<bool>,
    #[serde(default, rename = "check")]
    _check: Str,
    #[serde(default, rename = "policy")]
    _policy: Str,
    #[serde(default, rename = "fit")]
    _fit: Str,
    #[serde(default, rename = "seen")]
    _seen: Str,
    #[serde(default, rename = "source")]
    _source: Str,
    #[serde(default, rename = "free")]
    _free: Each<Str>,
    #[serde(default, rename = "pf")]
    _pf: Each<f64>,
    #[serde(default, rename = "placebo")]
    _placebo: Each<Str>,
    #[serde(default, rename = "psi")]
    _psi: f64,
    #[serde(default, rename = "nofree")]
    _nofree: Each<Str>,
    #[serde(default, rename = "xk")]
    _xk: Option<u32>,
    #[serde(default, rename = "xs")]
    _xs: Option<u32>,
    #[serde(default, rename = "alpha")]
    _alpha: Option<f64>,
    #[serde(default, borrow)]
    pub(crate) tail: Vec<Text<'a>>,
    #[serde(default, rename = "pt")]
    _pt: f64,
    #[serde(default, rename = "rel")]
    _rel: Each<f64>,
    #[serde(default, rename = "route")]
    _route: Str,
    #[serde(default, rename = "reason")]
    _reason: Str,
    #[serde(default, rename = "eligible")]
    _eligible: u32,
    #[serde(default, rename = "shown")]
    _shown: u32,
    #[serde(default, rename = "tokens")]
    _tokens: u32,
    #[serde(default, rename = "budget")]
    _budget: u32,
    #[serde(default, rename = "fill")]
    _fill: Each<u16>,
    #[serde(default, rename = "model")]
    _model: Str,
    #[serde(default, rename = "prompt_hash")]
    _prompt_hash: Str,
    #[serde(default, rename = "harness")]
    _harness: Str,
    #[serde(default, rename = "version")]
    _version: Str,
    #[serde(default, rename = "dur_ms")]
    _dur_ms: u64,
    #[serde(default, rename = "failed")]
    _failed: bool,
    #[serde(default, rename = "usd")]
    _usd: f64,
    #[serde(default)]
    pub(crate) judge: u8,
    #[serde(default, rename = "stripped")]
    _stripped: bool,
    #[serde(default, rename = "reasons")]
    _reasons: Each<Str>,
    #[serde(default, rename = "at")]
    _at: Str,
    #[serde(default, rename = "moves")]
    _moves: Each<Str>,
    #[serde(default, rename = "whys")]
    _whys: Each<Str>,
    #[serde(default, rename = "xtokens")]
    _xtokens: u32,
    #[serde(default, rename = "line")]
    _line: Str,
    #[serde(default, rename = "run")]
    _run: Str,
}

pub fn read(path: &Path) -> Vec<Outcome> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Appends `o` in one write (learn.rs `append_line`: concurrent writers never tear a record), and
/// counts it in the usage statistics (`count_result`).
pub fn append(path: &Path, o: &Outcome) -> std::io::Result<()> {
    crate::learn::append_jsonl(path, o)?;
    if let Some(home) = path.parent() {
        count_result(home, o);
    }
    Ok(())
}

/// `o` in the usage statistics of the store `home`: one event of its result kind with its lessons and
/// channel. `asked` is already counted as the Stop's block, a `vetoed` result as the `veto` that
/// credited it (veto.rs); a placebo's result is no result (`is_result`): its "changed" never joins the
/// deliveries' counts. The judge (hook.rs) writes one record per routing check but counts the lessons
/// of a call's channel as one event, as before checks had ids.
pub fn count_result(home: &Path, o: &Outcome) {
    if !is_result(o) {
        return;
    }
    let kind = match o.kind.as_str() {
        CHANGED => crate::stats::CHANGED,
        IGNORED => crate::stats::IGNORED,
        CORRECTED => crate::stats::CORRECTED,
        NOT_CORRECTED => crate::stats::NOT_CORRECTED,
        REGRETTED => crate::stats::REGRETTED,
        TESTED => crate::stats::TESTED,
        // Measurements, never results (`is_result` already turned them away).
        UNCLEAR | REMAINS | NOT_ARISEN => return,
        _ => return,
    };
    crate::stats::count(home, kind, &o.session, |e| {
        e.repo = o.repo.clone();
        e.lessons = o.lessons.clone();
        e.channel = o.channel.clone();
    });
}

/// Did the conscience ask `lesson` in `session` before `ms`?
pub fn asked_before(outcomes: &[Outcome], session: &str, lesson: &str, ms: i64) -> bool {
    outcomes.iter().any(|o| o.kind == ASKED && o.session == session && o.ms < ms && o.lessons.iter().any(|l| l == lesson))
}

/// Is a result (`kind`) already recorded for `lesson` in `session`? Keeps `borg learn` idempotent.
pub fn has(outcomes: &[Outcome], kind: &str, session: &str, lesson: &str) -> bool {
    outcomes.iter().any(|o| o.kind == kind && o.session == session && o.lessons.iter().any(|l| l == lesson))
}

/// The lessons a correction at `ms` in `session` shows were asked in vain, each asked in `session`
/// before `ms` and not yet recorded as corrected or vetoed there (a veto is the stronger result): the lessons the correction `supersedes`
/// (it contradicts them), and `target`, the lesson it was merged into (the same concern came up
/// again). When `target` was asked as one side of a rendered disagreement, the engineer sided with
/// it: the other sides of that disagreement were corrected, not `target`. Sorted, unique.
pub fn contradicted(outcomes: &[Outcome], session: &str, ms: i64, target: Option<&str>, supersedes: &[String]) -> Vec<String> {
    let mut out: Vec<String> = supersedes.to_vec();
    if let Some(t) = target {
        let sides: Vec<String> = outcomes
            .iter()
            .filter(|o| o.kind == ASKED && o.session == session && o.ms < ms)
            .flat_map(|o| &o.groups)
            .filter(|g| g.iter().any(|x| x == t))
            .flatten()
            .filter(|x| *x != t)
            .cloned()
            .collect();
        match sides.is_empty() {
            true => out.push(t.to_string()),
            false => out.extend(sides),
        }
    }
    out.retain(|l| asked_before(outcomes, session, l, ms) && !has(outcomes, CORRECTED, session, l) && !has(outcomes, VETOED, session, l));
    out.sort();
    out.dedup();
    out
}

/// Was `o` a free delivery (channel `early` or `queued`): lessons handed over as context, no question?
pub fn free(o: &Outcome) -> bool {
    free_channel(&o.channel)
}

/// Whether a record of `channel` was a free delivery (`free`).
pub(crate) fn free_channel(channel: &str) -> bool {
    channel == "early" || channel == "queued"
}

/// The lessons a correction at `ms` in `session` shows were needed and not asked: of the lessons it
/// `supersedes` and `target` (the lesson it was merged into), those a graded check of the session found
/// applying before `ms` without choosing them, never asked as a question in the session before `ms` (a
/// free delivery asks nothing: a lesson only handed over as context stays a miss for the cut,
/// attention.rs), and not yet recorded as missed there. Sorted, unique.
pub fn missed(outcomes: &[Outcome], session: &str, ms: i64, target: Option<&str>, supersedes: &[String]) -> Vec<String> {
    let mut out: Vec<String> = supersedes.iter().cloned().chain(target.map(str::to_string)).collect();
    let applied_unchosen = |l: &str| {
        outcomes.iter().any(|o| o.kind == GRADED && o.session == session && o.ms < ms && o.lessons.iter().any(|x| x == l) && !o.chosen.iter().any(|x| x == l))
    };
    let questioned = |l: &str| outcomes.iter().any(|o| o.kind == ASKED && !free(o) && o.session == session && o.ms < ms && o.lessons.iter().any(|x| x == l));
    out.retain(|l| applied_unchosen(l) && !questioned(l) && !has(outcomes, MISSED, session, l));
    out.sort();
    out.dedup();
    out
}

/// Has `lesson` been credited for the regret `cause` already (in any session)?
fn credited(outcomes: &[Outcome], cause: &str, lesson: &str) -> bool {
    outcomes.iter().any(|o| o.kind == REGRETTED && o.cause == cause && o.lessons.iter().any(|l| l == lesson))
}

/// The lessons a regret blames: those asked in `session` (the deciding one) before `before_ms`, without
/// a correction or a veto there already (both outrank regretted, as in memory.rs) and not yet credited
/// for `cause`. When some of `target` (the lesson the regret was folded into, and the lessons that lesson
/// supersedes) were asked, they share the blame alone: the same concern came up again, and when all of
/// them already took a correction or a veto for it, nobody else is blamed. Otherwise every asked lesson
/// shares it equally (the Shapley value of a symmetric game). A lesson a graded check found applying but
/// did not ask was never shown, so it is not blamed. One record per lesson, sorted, at `ms`,
/// with `credit = strength / n`; n counts the blamed lessons before the already-credited ones are left
/// out, so a re-run writes nothing and the shares never change. Every record's session has an earlier
/// `asked` record, so it never moves memory.rs's `Clock`.
#[allow(clippy::too_many_arguments)]
pub fn regretted(outcomes: &[Outcome], session: &str, before_ms: i64, cause: &str, target: &[String], strength: f64, ms: i64, repo: &str) -> Vec<Outcome> {
    if session.is_empty() || before_ms <= 0 || strength <= 0.0 {
        return Vec::new();
    }
    let mut asked: Vec<&str> =
        outcomes.iter().filter(|o| o.kind == ASKED && o.session == session && o.ms < before_ms).flat_map(|o| o.lessons.iter().map(String::as_str)).collect();
    asked.sort_unstable();
    asked.dedup();
    if asked.iter().any(|l| target.iter().any(|t| t == l)) {
        asked.retain(|l| target.iter().any(|t| t == l));
    }
    asked.retain(|l| !has(outcomes, CORRECTED, session, l) && !has(outcomes, VETOED, session, l));
    let credit = (strength / asked.len().max(1) as f64).min(1.0);
    asked
        .into_iter()
        .filter(|l| !credited(outcomes, cause, l))
        .map(|l| Outcome {
            kind: REGRETTED.into(),
            session: session.into(),
            lessons: vec![l.into()],
            ms,
            repo: repo.into(),
            cause: cause.into(),
            credit,
            ..Default::default()
        })
        .collect()
}

/// The `tested` records a session's own test runs earn (`tests`: its `<state>/<session>.tests`, `fail <ms>`
/// and `pass <ms>` lines, hook.rs `capture_test`): each lesson asked as a question in `session` (a free
/// delivery asks nothing, `free`: handed over at every prompt of its scope, it would gain from every
/// test-first session) whose last test result after its first ask passed, after a failed run since that ask (red, then green) or a change the drone
/// made on the lesson's point (`asked_then_changed`), and that has no `tested` record there yet, nor a
/// correction or a veto (the engineer's word outranks a green run), nor `asked_then_ignored` (red, then
/// green is also how a feature is built: it does not make an ignored question useful), nor
/// `asked_not_arisen` on a question of it since its first ask without a change (the lesson's point never came up: a green run says nothing of
/// it, and `tested` would move a belief `asked_not_arisen` is censored for). A pass before the ask says nothing
/// of the lesson. One record per lesson, sorted, at that last pass. Every record's session has an
/// earlier `asked` record, so it never moves memory.rs's `Clock`.
pub fn tested(outcomes: &[Outcome], session: &str, tests: &str) -> Vec<Outcome> {
    let mut runs: Vec<(i64, bool)> = tests
        .lines()
        .filter_map(|l| l.split_once(' '))
        .filter_map(|(w, m)| Some((m.trim().parse::<i64>().ok()?, matches!(w, "pass" | "fail").then_some(w == "pass")?)))
        .collect();
    runs.sort_unstable();
    // Each lesson's first ask in the session, and its repo.
    let mut first: HashMap<&str, (i64, &str)> = HashMap::new();
    for o in outcomes.iter().filter(|o| o.kind == ASKED && !free(o) && o.session == session) {
        for l in &o.lessons {
            let e = first.entry(l.as_str()).or_insert((o.ms, o.repo.as_str()));
            if o.ms < e.0 {
                *e = (o.ms, o.repo.as_str());
            }
        }
    }
    // A verdict that the question's point never came up, on a question of the lesson (not a placebo's
    // or a free delivery's, none before its first ask): only that says the green run is not about it.
    let not_arisen_on_ask = |l: &str, at: i64| {
        outcomes
            .iter()
            .any(|o| o.kind == NOT_ARISEN && o.session == session && o.channel != PLACEBO && !free(o) && o.ms >= at && o.lessons.iter().any(|x| x == l))
    };
    let mut out: Vec<Outcome> = first
        .into_iter()
        .filter(|(l, _)| ![TESTED, CORRECTED, VETOED, IGNORED].iter().any(|k| has(outcomes, k, session, l)))
        .filter(|&(l, (at, _))| !not_arisen_on_ask(l, at) || has(outcomes, CHANGED, session, l))
        .filter_map(|(l, (at, repo))| {
            let after = &runs[runs.partition_point(|r| r.0 <= at)..];
            let &(ms, pass) = after.last()?;
            (pass && (after.iter().any(|r| !r.1) || has(outcomes, CHANGED, session, l))).then(|| Outcome {
                kind: TESTED.into(),
                session: session.into(),
                lessons: vec![l.into()],
                ms,
                repo: repo.into(),
                ..Default::default()
            })
        })
        .collect();
    out.sort_by(|a, b| a.lessons.cmp(&b.lessons));
    out
}

/// At session end: every lesson asked in `session` that has no result yet was not corrected, and every
/// placebo of it without a verdict is closed. A vetoed lesson already has its result. `settle_with`,
/// every unjudged lesson's reason `no_call`.
pub fn settle(outcomes: &[Outcome], session: &str, ms: i64) -> Vec<Outcome> {
    settle_with(outcomes, session, ms, &|_| UNJUDGED_NO_CALL)
}

/// `settle`, each record with its `reason`: `judged` when the judge gave the lesson's latest ask in the
/// session a verdict that settles it (`settles`: `asked_then_changed`, `asked_ignored`,
/// `asked_not_arisen`), else `unjudged(lesson)` (one of the
/// `UNJUDGED_*`, from the session's state file, hook.rs `unjudged`); and its `check`, the check behind
/// that latest `asked` record. A verdict on an earlier delivery says nothing of a later question: a
/// stop that judges a lesson handed over free and then asks it writes the verdict first, and no judge
/// call need follow the answer. Nor does a verdict on a free delivery (`free`) say anything of a
/// question (or one on a question of a free delivery), whenever it came: an older binary's live check
/// judged the earlier free delivery of a lesson asked since and not yet answered (hook.rs `pending`),
/// and its question, never judged, then settles censored instead of staying open in the selection
/// summary (select/summary.rs `settled`).
/// Then every placebo of the session (a `delivery` routed `placebo`, which writes no `asked`) that no
/// verdict and no earlier settle closed (`open_placebos`): `asked_not_corrected` on channel `placebo`
/// (no result, `is_result`; it ends no ask's wait, `resolves`) with the placebo's check and repo, and
/// the reason `unjudged(lesson)` when it is the lesson's latest delivery in the session, else
/// `UNJUDGED_LATER`. So the selection summary censors an unjudged placebo as it does an unjudged ask
/// (select/summary.rs `settled`), and the judge-missingness fit sees both.
/// Not a result: memory.rs reads `asked_not_corrected` as censored, so a verdict recorded after it (a
/// resumed session) counts once, alone.
/// The asks first, sorted by lesson id, then the placebos by lesson id and check, so the appended lines
/// are deterministic. A few passes over the session's records, whatever the number of asks.
pub fn settle_with(outcomes: &[Outcome], session: &str, ms: i64, unjudged: &dyn Fn(&str) -> &'static str) -> Vec<Outcome> {
    let mine: Vec<&Outcome> = outcomes.iter().filter(|o| o.session == session).collect();
    let resolved: std::collections::HashSet<&str> = mine.iter().filter(|o| resolves(o)).flat_map(|o| o.lessons.iter().map(String::as_str)).collect();
    // Per lesson: its verdicts' (ms, check, on a free delivery).
    let mut verdicts: HashMap<&str, Vec<(i64, &str, bool)>> = HashMap::new();
    for o in mine.iter().filter(|o| settles(&o.kind) && o.channel != PLACEBO) {
        for l in &o.lessons {
            verdicts.entry(l.as_str()).or_default().push((o.ms, o.check.as_str(), free(o)));
        }
    }
    // Per lesson: the first of its asks' repos in sort order (as before checks were kept), and the
    // ms, check and whether free of its latest ask.
    let mut asked: std::collections::BTreeMap<&str, (&str, i64, &str, bool)> = std::collections::BTreeMap::new();
    // Per lesson: the ms of its latest delivery, an `asked` for every one the drone sees and a
    // placebo's `delivery`.
    let mut latest: HashMap<&str, i64> = HashMap::new();
    for o in mine.iter().filter(|o| o.kind == ASKED || is_placebo(o)) {
        for l in o.lessons.iter().map(String::as_str) {
            let t = latest.entry(l).or_insert(o.ms);
            *t = (*t).max(o.ms);
            if o.kind != ASKED || resolved.contains(l) {
                continue;
            }
            let e = asked.entry(l).or_insert((o.repo.as_str(), o.ms, o.check.as_str(), free(o)));
            e.0 = e.0.min(o.repo.as_str());
            if o.ms >= e.1 {
                (e.1, e.2, e.3) = (o.ms, o.check.as_str(), free(o));
            }
        }
    }
    // A verdict on the latest ask is on the same kind of delivery (a question, or free) and came after it
    // (the judge sees a delivery in a later call), or names its check. Time first: an older binary's stop
    // wrote one `asked` naming the stop's check also for a lesson another check routed (hook.rs
    // `#borg-routed`), whose verdict names that check; a stop now writes one `asked` per routing check
    // (hook.rs `stop_ask`).
    let judged = |l: &str, at: i64, check: &str, as_free: bool| {
        verdicts.get(l).is_some_and(|v| v.iter().any(|&(t, c, f)| f == as_free && (t > at || (!check.is_empty() && c == check))))
    };
    let mut out: Vec<Outcome> = asked
        .into_iter()
        .map(|(l, (repo, at, check, as_free))| Outcome {
            kind: NOT_CORRECTED.into(),
            session: session.into(),
            lessons: vec![l.into()],
            ms,
            repo: repo.into(),
            check: check.into(),
            reason: if judged(l, at, check, as_free) { UNJUDGED_NONE } else { unjudged(l) }.into(),
            // The binary that settled: from 0.7.3 on, the selection summary closes every delivery of the
            // lesson the session left without a verdict (select/summary.rs `settled`).
            version: env!("CARGO_PKG_VERSION").into(),
            ..Default::default()
        })
        .collect();
    let mut placebos: Vec<Outcome> = open_placebos(&mine)
        .into_iter()
        .map(|(l, d)| Outcome {
            kind: NOT_CORRECTED.into(),
            session: session.into(),
            lessons: vec![l.into()],
            ms,
            repo: d.repo.clone(),
            channel: PLACEBO.into(),
            check: d.check.clone(),
            // Delivered again later, the lesson's later delivery is the one the judge saw.
            reason: if latest.get(l).is_some_and(|&t| t > d.ms) { UNJUDGED_LATER } else { unjudged(l) }.into(),
            ..Default::default()
        })
        .collect();
    placebos.sort_by(|a, b| (&a.lessons, &a.check).cmp(&(&b.lessons, &b.check)));
    out.extend(placebos);
    out
}

/// Does `o` end an ask's wait for its result (`settle`)? A placebo's settle record ends none: a
/// placebo is never asked, and the lesson may be asked later in the session.
fn resolves(o: &Outcome) -> bool {
    matches!(o.kind.as_str(), CORRECTED | NOT_CORRECTED | VETOED) && o.channel != PLACEBO
}

/// A placebo's delivery (`delivery` routed `placebo`, hook.rs `queued`): judged blindly, never shown,
/// so it writes no `asked`.
fn is_placebo(o: &Outcome) -> bool {
    o.kind == DELIVERY && o.route == ROUTE_PLACEBO
}

/// Does `r`, a placebo channel's verdict that settles (`settles`: `asked_then_changed`, `asked_ignored`,
/// `asked_not_arisen`) or settle record (`asked_not_corrected`), close placebo delivery `d` of its
/// lesson? A settle record names the placebo's own check, so it closes exactly that one, and settling
/// again writes nothing. A verdict names the check it judged (a check routes a lesson once), or, a
/// record from before check ids, came after the placebo. An unclear verdict or a remaining consequence
/// closes nothing, as it leaves an ask pending.
fn closes(r: &Outcome, d: &Outcome) -> bool {
    match r.kind.as_str() {
        NOT_CORRECTED => r.check == d.check,
        k if settles(k) && (r.check.is_empty() || d.check.is_empty()) => r.ms > d.ms,
        k if settles(k) => r.check == d.check,
        _ => false,
    }
}

/// The open placebos among `mine` (one session's records), in log order: per placebo delivery
/// (`is_placebo`) and lesson, none of the session's placebo records `closes` it.
fn open_placebos<'a>(mine: &[&'a Outcome]) -> Vec<(&'a str, &'a Outcome)> {
    let mut closers: HashMap<&str, Vec<&Outcome>> = HashMap::new();
    for &o in mine.iter().filter(|o| o.channel == PLACEBO && !is_placebo(o)) {
        for l in &o.lessons {
            closers.entry(l.as_str()).or_default().push(o);
        }
    }
    mine.iter()
        .copied()
        .filter(|o| is_placebo(o))
        .flat_map(|d| d.lessons.iter().map(move |l| (l.as_str(), d)))
        .filter(|&(l, d)| !closers.get(l).is_some_and(|rs| rs.iter().any(|r| closes(r, d))))
        .collect()
}

/// Sessions with asks or placebos that are idle at `now`: their newest ask or placebo and newest
/// episode (`last_episode`, newest episode ms per session) are at least `IDLE_MS` old. Sorted. Only
/// sessions with an ask still without its result or an open placebo (`open_placebos`), so `borg learn`
/// (under the store lock) settles what is new, not every session it ever settled: one pass over the
/// log.
pub fn idle_sessions<'a>(outcomes: &'a [Outcome], last_episode: &HashMap<String, i64>, now: i64) -> Vec<&'a str> {
    let mut newest: HashMap<&str, i64> = HashMap::new();
    let (mut asked, mut resolved) = (std::collections::HashSet::new(), std::collections::HashSet::new());
    // Per session, its placebo channel's records: deliveries, verdicts and settles.
    let mut placebos: HashMap<&str, Vec<&Outcome>> = HashMap::new();
    for o in outcomes {
        if o.channel == PLACEBO || is_placebo(o) {
            placebos.entry(o.session.as_str()).or_default().push(o);
        }
        if o.kind == ASKED || is_placebo(o) {
            let m = newest.entry(o.session.as_str()).or_insert(o.ms);
            *m = (*m).max(o.ms);
        }
        let into = match o.kind.as_str() {
            ASKED => &mut asked,
            _ if resolves(o) => &mut resolved,
            _ => continue,
        };
        into.extend(o.lessons.iter().map(|l| (o.session.as_str(), l.as_str())));
    }
    let mut open: std::collections::HashSet<&str> = asked.difference(&resolved).map(|(s, _)| *s).collect();
    open.extend(placebos.iter().filter(|(_, rs)| !open_placebos(rs).is_empty()).map(|(s, _)| *s));
    let mut idle: Vec<&str> = newest
        .into_iter()
        .filter(|&(s, ms)| open.contains(s) && ms.max(last_episode.get(s).copied().unwrap_or(i64::MIN)) <= now - IDLE_MS)
        .map(|(s, _)| s)
        .collect();
    idle.sort();
    idle
}

/// Usefulness of a lesson from its asked outcomes:
///
/// w = (1 + not_corrected) / (1 + not_corrected + 2 * corrected)
///
/// The +1 is a neutral prior, so a lesson without outcomes keeps w = 1 and a single bad session
/// cannot zero it. A correction of the same concern right after asking is strong evidence the
/// question did not help (too generic, or it does not change what the drone does), while a
/// session without correction is weak evidence (the engineer may simply not have looked), so a
/// correction counts twice. w stays in (0, 1]: outcomes only demote, BM25 relevance still decides
/// what is on topic. One corrected session gives 1/3; one corrected and one not gives 1/2.
pub fn weight(not_corrected: u32, corrected: u32) -> f64 {
    let (n, c) = (not_corrected as f64, corrected as f64);
    (1.0 + n) / (1.0 + n + f64::from(CORRECTED_W) * c)
}

/// The weight of every lesson that has outcomes. Each (session, lesson) counts once; corrected wins.
pub fn weights(outcomes: &[Outcome]) -> HashMap<String, f64> {
    counts(outcomes).into_iter().map(|(l, (n, c))| (l, weight(n, c))).collect()
}

/// Per lesson with results: (sessions not corrected, sessions corrected). Each (session, lesson)
/// counts once; corrected wins. Used by `weights`.
pub fn counts(outcomes: &[Outcome]) -> HashMap<String, (u32, u32)> {
    sessions(outcomes).into_iter().map(|(l, ss)| (l.to_string(), tally(ss.values(), i64::MIN))).collect()
}

/// Per lesson, per session with a result: (corrected, ms of the session's earliest result for the
/// lesson). Corrected wins over a later (resumed) settle. trust.rs filters these by when an engineer
/// joined the lesson. A veto (`vetoed`) counts as a correction here, so trust and `weight` see it too
/// (as heavy as a correction; only memory.rs weighs it above one).
pub fn sessions(outcomes: &[Outcome]) -> HashMap<&str, HashMap<&str, (bool, i64)>> {
    let mut out: HashMap<&str, HashMap<&str, (bool, i64)>> = HashMap::new();
    // A placebo's record is no result (`is_result`).
    for o in outcomes.iter().filter(|o| is_result(o)) {
        let corrected = match o.kind.as_str() {
            CORRECTED | VETOED => true,
            NOT_CORRECTED => false,
            _ => continue,
        };
        for l in &o.lessons {
            let r = out.entry(l.as_str()).or_default().entry(o.session.as_str()).or_insert((corrected, o.ms));
            r.0 |= corrected;
            r.1 = r.1.min(o.ms);
        }
    }
    out
}

/// (sessions not corrected, sessions corrected) among `sessions` results whose earliest result is
/// after `since`.
pub fn tally<'a>(sessions: impl IntoIterator<Item = &'a (bool, i64)>, since: i64) -> (u32, u32) {
    sessions.into_iter().filter(|(_, ms)| *ms > since).fold((0, 0), |(n, c), (corrected, _)| match corrected {
        true => (n, c + 1),
        false => (n + 1, c),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record by today's judge: its ignore is a result (outcome.rs `ambiguous`).
    fn o(kind: &str, session: &str, lessons: &[&str], ms: i64) -> Outcome {
        Outcome {
            kind: kind.into(),
            session: session.into(),
            lessons: lessons.iter().map(|l| l.to_string()).collect(),
            ms,
            repo: "r".into(),
            judge: if kind == IGNORED { crate::outcome::NEVER_SPLIT } else { 0 },
            ..Default::default()
        }
    }

    #[test]
    fn missed_is_an_applied_unasked_lesson_a_later_correction_lands_on() {
        let graded = |ms: i64, chosen: &[&str]| Outcome {
            kind: GRADED.into(),
            session: "s".into(),
            lessons: vec!["A".into(), "B".into()],
            ms,
            chosen: chosen.iter().map(|x| x.to_string()).collect(),
            ..Default::default()
        };
        let none: Vec<String> = Vec::new();
        let log = vec![graded(10, &["B"])];
        assert_eq!(missed(&log, "s", 20, Some("A"), &none), ["A"]);
        assert_eq!(missed(&log, "s", 20, None, &["A".into(), "B".into()]), ["A"], "B was chosen");
        assert!(missed(&log, "s", 5, Some("A"), &none).is_empty(), "a correction before the check");
        assert!(missed(&log, "t", 20, Some("A"), &none).is_empty(), "another session");
        assert!(missed(&log, "s", 20, Some("C"), &none).is_empty(), "never applied");
        let mut asked = log.clone();
        asked.push(o(ASKED, "s", &["A"], 15));
        assert!(missed(&asked, "s", 20, Some("A"), &none).is_empty(), "asked later in the session: not a miss");
        let mut free = log.clone();
        free.push(Outcome { channel: "queued".into(), ..o(ASKED, "s", &["A"], 15) });
        free.push(Outcome { channel: "early".into(), ..o(ASKED, "s", &["A"], 16) });
        assert_eq!(missed(&free, "s", 20, Some("A"), &none), ["A"], "handed over free, never asked: still a miss");
        let mut done = log.clone();
        done.push(o(MISSED, "s", &["A"], 18));
        assert!(missed(&done, "s", 20, Some("A"), &none).is_empty(), "recorded once");
    }

    #[test]
    fn weight_formula() {
        assert_eq!(weight(0, 0), 1.0);
        assert_eq!(weight(5, 0), 1.0);
        assert!((weight(0, 1) - 1.0 / 3.0).abs() < 1e-12);
        assert_eq!(weight(1, 1), 0.5);
        assert!(weight(0, 3) < weight(0, 1));
    }

    #[test]
    fn weights_count_each_session_once_and_corrected_wins() {
        let all = vec![
            o(ASKED, "s1", &["A", "B"], 10),
            o(CORRECTED, "s1", &["A"], 20),
            o(CORRECTED, "s1", &["A"], 30),     // second correction in the same session: still one
            o(NOT_CORRECTED, "s1", &["A"], 40), // resumed session settled later: corrected wins
            o(NOT_CORRECTED, "s1", &["B"], 40),
            o(NOT_CORRECTED, "s2", &["A"], 50),
        ];
        let w = weights(&all);
        assert_eq!(w["A"], weight(1, 1));
        assert_eq!(w["B"], 1.0);
        assert!(!w.contains_key("C"));
    }

    #[test]
    fn a_session_whose_tests_go_green_after_an_ask_credits_it_once() {
        let mut log = vec![o(ASKED, "s", &["A", "B"], 10), o(ASKED, "t", &["A"], 10)];
        // Red after the ask, then green: one credit per lesson asked, dated at the pass.
        let got = tested(&log, "s", "fail 20\npass 30\n");
        assert_eq!(got.iter().map(|x| (x.kind.as_str(), x.lessons[0].as_str(), x.ms)).collect::<Vec<_>>(), [(TESTED, "A", 30), (TESTED, "B", 30)]);
        // Idempotent: a re-run with the records written writes nothing.
        log.extend(got);
        assert!(tested(&log, "s", "fail 20\npass 30\n").is_empty());
        // Only the session's own asks: t ran no tests.
        assert!(tested(&log, "t", "").is_empty(), "no tests file: nothing");
    }

    #[test]
    fn a_green_run_credits_only_after_the_ask_and_with_a_reason() {
        let log = vec![o(ASKED, "s", &["A"], 50)];
        assert!(tested(&log, "s", "fail 20\npass 30\n").is_empty(), "red and green before the ask");
        assert!(tested(&log, "s", "fail 20\npass 60\n").is_empty(), "the failure before the ask: green proves nothing new");
        assert!(tested(&log, "s", "fail 60\npass 70\nfail 80\n").is_empty(), "last run red");
        assert!(tested(&log, "s", "pass 60\n").is_empty(), "green alone, nothing changed");
        assert!(tested(&log, "s", "pass 60\npass 70 \ngarbage\n").is_empty());
        let early = [Outcome { channel: "early".into(), ..o(ASKED, "e", &["A"], 10) }];
        assert!(tested(&early, "e", "fail 20\npass 30\n").is_empty(), "a free delivery asked nothing");
        // Green after the drone changed its work on the lesson: a credit.
        let changed = [log.clone(), vec![o(CHANGED, "s", &["A"], 55)]].concat();
        assert_eq!(tested(&changed, "s", "pass 60\n").len(), 1);
        // A correction or a veto of the lesson in the session outranks a green run; an ignored question
        // gains nothing from the drone fixing its own failing test.
        for k in [CORRECTED, VETOED, IGNORED] {
            let worse = [log.clone(), vec![o(k, "s", &["A"], 55)]].concat();
            assert!(tested(&worse, "s", "fail 60\npass 70\n").is_empty(), "{k}");
        }
        // Lines out of order still find the last run; the first ask decides what is after it.
        let twice = [log.clone(), vec![o(ASKED, "s", &["A"], 65)]].concat();
        assert_eq!(tested(&twice, "s", "pass 70\nfail 60\n")[0].ms, 70);
    }

    #[test]
    fn settle_marks_only_unresolved_asks_of_this_session() {
        let all = vec![o(ASKED, "s1", &["B", "A"], 10), o(ASKED, "s1", &["A", "C"], 15), o(CORRECTED, "s1", &["A"], 20), o(ASKED, "s2", &["D"], 10)];
        let settled = settle(&all, "s1", 99);
        assert_eq!(settled, vec![unjudged("s1", "B", UNJUDGED_NO_CALL), unjudged("s1", "C", UNJUDGED_NO_CALL)]);
        assert!(asked_before(&all, "s1", "A", 11) && !asked_before(&all, "s1", "C", 15) && !asked_before(&all, "s2", "A", 99));
    }

    /// An ask's settle record, as this binary writes it (`settle_with`: with its version).
    fn unjudged(session: &str, lesson: &str, reason: &str) -> Outcome {
        Outcome { reason: reason.into(), version: env!("CARGO_PKG_VERSION").into(), ..o(NOT_CORRECTED, session, &[lesson], 99) }
    }

    #[test]
    fn a_settled_lesson_says_why_it_had_no_verdict_and_names_its_latest_check() {
        let ask = |lessons: &[&str], ms: i64, check: &str| Outcome { check: check.into(), ..o(ASKED, "s", lessons, ms) };
        let placebo = Outcome { channel: PLACEBO.into(), ..o(CHANGED, "s", &["C"], 30) };
        let all = vec![ask(&["A", "B", "C"], 10, "c1"), ask(&["B"], 20, "c2"), o(IGNORED, "s", &["A"], 25), placebo];
        let why = |l: &str| if l == "B" { UNJUDGED_TRIES } else { UNJUDGED_SLOTS };
        let got = settle_with(&all, "s", 99, &why);
        let want = |l: &str, reason: &str, check: &str| Outcome { check: check.into(), ..unjudged("s", l, reason) };
        assert_eq!(got, vec![want("A", UNJUDGED_NONE, "c1"), want("B", UNJUDGED_TRIES, "c2"), want("C", UNJUDGED_SLOTS, "c1")], "a placebo's verdict is none");
    }

    /// A verdict on an earlier delivery of the lesson is none for its later question: handed over free,
    /// judged ignored by a stop that then asks it, and the session ends after the answer, so no judge
    /// saw the question. A verdict after the question, or on its check, is its own.
    #[test]
    fn a_verdict_on_an_earlier_delivery_leaves_the_later_question_unjudged() {
        let ask = |channel: &str, ms: i64, check: &str| Outcome { channel: channel.into(), check: check.into(), ..o(ASKED, "s", &["L"], ms) };
        let verdict = |channel: &str, ms: i64, check: &str| Outcome { channel: channel.into(), check: check.into(), ..o(IGNORED, "s", &["L"], ms) };
        let why = |log: &[Outcome]| settle_with(log, "s", 99, &|_| UNJUDGED_NO_CALL).into_iter().map(|x| (x.reason, x.check)).collect::<Vec<_>>();
        let said = |reason: &str, check: &str| vec![(reason.to_string(), check.to_string())];
        // The stop writes its verdict on the free delivery, then the question, in the same millisecond.
        let free_then_asked = vec![ask("queued", 10, "cF"), verdict("queued", 20, "cF"), ask("stop", 20, "cS")];
        assert_eq!(why(&free_then_asked), said(UNJUDGED_NO_CALL, "cS"));
        let answered = [free_then_asked, vec![verdict("stop", 30, "cS")]].concat();
        assert_eq!(why(&answered), said(UNJUDGED_NONE, "cS"));
        // A lesson another check routed, asked at the stop: its verdict names the routing check.
        assert_eq!(why(&[ask("stop", 20, "cS"), verdict("stop", 30, "cL")]), said(UNJUDGED_NONE, "cS"));
        // On the question's check, though its clock stepped back.
        assert_eq!(why(&[ask("stop", 20, "cS"), verdict("stop", 15, "cS")]), said(UNJUDGED_NONE, "cS"));
    }

    /// An older binary's live check judged a lesson's earlier free delivery after the lesson was asked
    /// live and before the answer (hook.rs `pending`); no judge saw the question. The session ends: the
    /// free verdict is not the question's, so the question settles censored (an unjudged reason), which
    /// the selection summary reads as its label, instead of staying open for good. A free delivery's own
    /// verdict still settles it as judged.
    #[test]
    fn a_free_deliverys_verdict_after_a_later_question_leaves_the_question_unjudged() {
        let ask = |channel: &str, ms: i64, check: &str| Outcome { channel: channel.into(), check: check.into(), ..o(ASKED, "s", &["L"], ms) };
        let verdict = |channel: &str, ms: i64, check: &str| Outcome { channel: channel.into(), check: check.into(), ..o(CHANGED, "s", &["L"], ms) };
        let why = |log: &[Outcome]| settle_with(log, "s", 99, &|_| UNJUDGED_TRIES).into_iter().map(|x| (x.reason, x.check)).collect::<Vec<_>>();
        let said = |reason: &str, check: &str| vec![(reason.to_string(), check.to_string())];
        let stuck = vec![ask("queued", 10, "cA"), ask("live", 20, "cB"), verdict("queued", 30, "cA")];
        assert_eq!(why(&stuck), said(UNJUDGED_TRIES, "cB"));
        let early = vec![ask("early", 10, "cA"), ask("stop", 20, "cB"), verdict("early", 30, "cA")];
        assert_eq!(why(&early), said(UNJUDGED_TRIES, "cB"));
        assert_eq!(why(&[stuck.clone(), vec![verdict("live", 40, "cB")]].concat()), said(UNJUDGED_NONE, "cB"), "the question's own verdict");
        assert_eq!(why(&[ask("queued", 10, "cA"), verdict("queued", 30, "cA")]), said(UNJUDGED_NONE, "cA"), "a free delivery's own");

        // The selection summary labels the stuck question censored once the session settles.
        use crate::select::{
            Route,
            summary::{Label, Summary, fold},
        };
        let graded = |check: &str, ms: i64, channel: &str| Outcome {
            grades: vec![3],
            ranks: vec![0],
            applied: 1,
            channel: channel.into(),
            check: check.into(),
            ..o(GRADED, "s", &["L"], ms)
        };
        let mut log = vec![Outcome { free: vec!["L".into()], ..graded("cA", 5, "live") }, Outcome { chosen: vec!["L".into()], ..graded("cB", 15, "live") }];
        log.extend(stuck);
        log.extend(settle_with(&log, "s", 99, &|_| UNJUDGED_TRIES));
        let text: String = log.iter().map(|x| format!("{}\n", serde_json::to_string(x).unwrap())).collect();
        let s = fold(&text, Summary::default());
        let label = |route: Route| s.applies.iter().find(|a| a.route == route).map(|a| a.label);
        assert_eq!(label(Route::Ask), Some(Label::Censored), "{:?}", s.applies);
    }

    /// The later delivery wins the judge (hook.rs `pending`): handed over free at cA then asked at cB, or
    /// asked at cA then handed over free at cB, the lesson is judged on cB alone. Once the session settles
    /// (a settle naming its binary), the delivery the judge never got to is censored, never left open,
    /// whose reason says "judged".
    #[test]
    fn a_settle_leaves_no_delivery_of_its_lesson_open() {
        use crate::select::{
            Route,
            summary::{Label, Summary, fold},
        };
        let graded = |check: &str, ms: i64, ask: bool| Outcome {
            grades: vec![2],
            ranks: vec![0],
            applied: 1,
            channel: "live".into(),
            check: check.into(),
            chosen: if ask { vec!["L".into()] } else { Vec::new() },
            free: if ask { Vec::new() } else { vec!["L".into()] },
            ..o(GRADED, "s", &["L"], ms)
        };
        let asked = |channel: &str, ms: i64, check: &str| Outcome { channel: channel.into(), check: check.into(), ..o(ASKED, "s", &["L"], ms) };
        let verdict = |channel: &str, check: &str| Outcome { channel: channel.into(), check: check.into(), ..o(CHANGED, "s", &["L"], 40) };
        for (first_ask, channels) in [(false, ["queued", "live"]), (true, ["live", "queued"])] {
            let mut log = vec![graded("cA", 5, first_ask), asked(channels[0], 10, "cA"), graded("cB", 15, !first_ask), asked(channels[1], 20, "cB")];
            log.push(verdict(channels[1], "cB"));
            let settled = settle_with(&log, "s", 99, &|_| UNJUDGED_TRIES);
            assert_eq!(settled.iter().map(|x| (x.reason.as_str(), x.check.as_str())).collect::<Vec<_>>(), [(UNJUDGED_NONE, "cB")]);
            log.extend(settled);
            let text: String = log.iter().map(|x| format!("{}\n", serde_json::to_string(x).unwrap())).collect();
            let s = fold(&text, Summary::default());
            let label = |route: Route| s.applies.iter().find(|a| a.route == route).map(|a| a.label);
            let (a, b) = if first_ask { (Route::Ask, Route::Free) } else { (Route::Free, Route::Ask) };
            assert_eq!((label(a), label(b)), (Some(Label::Censored), Some(Label::Changed)), "first asked {first_ask}: {:?}", s.applies);
        }
        // An older binary judged the free delivery while the question waited, and its settle (no
        // version) said "judged" for the question: it folds as when its hooks decided on it, the
        // question open, so the replay still reproduces those decisions.
        let mut log = vec![graded("cA", 5, false), asked("queued", 10, "cA"), graded("cB", 15, true), asked("live", 20, "cB"), verdict("queued", "cA")];
        log.push(Outcome { reason: UNJUDGED_NONE.into(), check: "cB".into(), ..o(NOT_CORRECTED, "s", &["L"], 99) });
        let text: String = log.iter().map(|x| format!("{}\n", serde_json::to_string(x).unwrap())).collect();
        let s = fold(&text, Summary::default());
        let labels: Vec<(Route, Label)> = s.applies.iter().map(|a| (a.route, a.label)).collect();
        assert_eq!(labels, [(Route::Free, Label::Changed), (Route::Ask, Label::Open)]);
    }

    fn placebo(lessons: &[&str], check: &str, ms: i64) -> Outcome {
        Outcome { channel: PLACEBO.into(), check: check.into(), route: ROUTE_PLACEBO.into(), ..o(DELIVERY, "s", lessons, ms) }
    }

    /// A placebo writes no `asked`, only its `delivery`: one the judge gave no verdict settles too, on the
    /// placebo channel (no result, and no ask's end), with its own check and reason. Delivered again
    /// later, the later delivery took its place in the judge's queue.
    #[test]
    fn an_unjudged_placebo_settles_on_its_own_channel() {
        let on = |kind: &str, lesson: &str, check: &str, ms: i64| Outcome { channel: PLACEBO.into(), check: check.into(), ..o(kind, "s", &[lesson], ms) };
        let log = vec![
            placebo(&["A", "B"], "p1", 10),
            on(IGNORED, "A", "p1", 15),
            on(UNCLEAR, "B", "p1", 15),
            placebo(&["C"], "p2", 20),
            Outcome { channel: "queued".into(), check: "c3".into(), ..o(ASKED, "s", &["C"], 30) },
            Outcome { route: ROUTE_DROPPED_DONE.into(), ..placebo(&["D"], "p4", 40) },
        ];
        let why = |l: &str| if l == "B" { UNJUDGED_TRIES } else { UNJUDGED_NO_CALL };
        let got = settle_with(&log, "s", 99, &why);
        let closed =
            |l: &str, check: &str, reason: &str| Outcome { channel: PLACEBO.into(), check: check.into(), version: String::new(), ..unjudged("s", l, reason) };
        let asked = Outcome { check: "c3".into(), ..unjudged("s", "C", UNJUDGED_NO_CALL) };
        let want = vec![asked, closed("B", "p1", UNJUDGED_TRIES), closed("C", "p2", UNJUDGED_LATER)];
        assert_eq!(got, want, "A has its verdict; D never reached the judge's queue");
        assert!(got[1..].iter().all(|x| !is_result(x) && share(x, "n1").is_none()), "no result, never shared");
        // Settled once: the same records close their placebos, so settling again writes nothing.
        let mut resumed = [log, got].concat();
        assert!(settle_with(&resumed, "s", 120, &why).is_empty());
        // Resumed: B asked, E a new placebo. B's placebo settle ended no ask's wait.
        resumed.extend([Outcome { channel: "stop".into(), check: "c5".into(), ..o(ASKED, "s", &["B"], 110) }, placebo(&["E"], "p6", 111)]);
        let again: Vec<(String, String, String)> =
            settle_with(&resumed, "s", 120, &why).into_iter().map(|x| (x.lessons.join(","), x.channel, x.check)).collect();
        assert_eq!(again, [("B".to_string(), String::new(), "c5".to_string()), ("E".to_string(), PLACEBO.to_string(), "p6".to_string())]);
    }

    /// Judge v3 (0.7.5): `asked_not_arisen` settles its delivery (a question's settle says "judged", a
    /// placebo is closed) while staying no result and never shared; `asked_consequence_remains` leaves
    /// it pending as `asked_unclear` does. The selection summary labels each apart, an older
    /// `asked_unclear` still `Unclear`, and a pending verdict never overwrites a settling one.
    #[test]
    fn not_arisen_settles_its_delivery_and_a_remaining_consequence_leaves_it_pending() {
        use crate::select::{
            Route,
            summary::{Label, Summary, fold},
        };
        let ask = |l: &str, check: &str| Outcome { channel: "stop".into(), check: check.into(), ..o(ASKED, "s", &[l], 10) };
        let on = |kind: &str, l: &str, check: &str, channel: &str| Outcome {
            channel: channel.into(),
            check: check.into(),
            judge: 3,
            reasons: vec!["only the docs changed".into()],
            ..o(kind, "s", &[l], 20)
        };
        let log = vec![
            ask("A", "cA"),
            on(NOT_ARISEN, "A", "cA", "stop"),
            ask("B", "cB"),
            on(REMAINS, "B", "cB", "stop"),
            placebo(&["P"], "pP", 10),
            on(NOT_ARISEN, "P", "pP", PLACEBO),
            placebo(&["Q"], "pQ", 10),
            on(REMAINS, "Q", "pQ", PLACEBO),
        ];
        assert!(log.iter().filter(|x| x.kind == NOT_ARISEN || x.kind == REMAINS).all(|x| !is_result(x) && share(x, "n1").is_none()));
        assert!(settles(NOT_ARISEN) && settles(CHANGED) && settles(IGNORED) && !settles(REMAINS) && !settles(UNCLEAR));
        let got: Vec<(String, String, String)> =
            settle_with(&log, "s", 99, &|_| UNJUDGED_TRIES).into_iter().map(|x| (x.lessons.join(","), x.channel, x.reason)).collect();
        let row = |l: &str, channel: &str, reason: &str| (l.to_string(), channel.to_string(), reason.to_string());
        assert_eq!(got, [row("A", "", UNJUDGED_NONE), row("B", "", UNJUDGED_TRIES), row("Q", PLACEBO, UNJUDGED_TRIES)], "P closed by its verdict");
        // The reason travels on the record and reads back; an older line without it reads empty.
        let line = serde_json::to_string(&log[1]).unwrap();
        assert!(line.contains(r#""reasons":["only the docs changed"]"#), "{line}");
        assert_eq!(serde_json::from_str::<Outcome>(&line).unwrap(), log[1]);
        let old: Outcome = serde_json::from_str(r#"{"kind":"asked_unclear","session":"s","lessons":["A"],"ms":1,"repo":"r","channel":"stop"}"#).unwrap();
        assert!(old.reasons.is_empty() && !is_result(&old) && !settles(&old.kind));

        // The summary: each graded apply its own label.
        let graded = |l: &str, check: &str, ask: bool, placebo: bool| Outcome {
            grades: vec![3],
            ranks: vec![0],
            applied: 1,
            channel: "stop".into(),
            check: check.into(),
            chosen: if ask { vec![l.into()] } else { Vec::new() },
            placebo: if placebo { vec![l.into()] } else { Vec::new() },
            psi: if placebo { 0.5 } else { 0.0 },
            ..o(GRADED, "s", &[l], 5)
        };
        let mut all = vec![graded("A", "cA", true, false), graded("B", "cB", true, false), graded("C", "cC", true, false)];
        all.push(graded("P", "pP", false, true));
        all.extend(log);
        all.extend([ask("C", "cC"), on(UNCLEAR, "C", "cC", "stop")]);
        // A later pending verdict does not undo a settling one; a later settling one replaces a pending one.
        all.extend([on(UNCLEAR, "A", "cA", "stop"), on(CHANGED, "B", "cB", "stop")]);
        let text: String = all.iter().map(|x| format!("{}\n", serde_json::to_string(x).unwrap())).collect();
        let s = fold(&text, Summary::default());
        let label = |l: &str| {
            let i = s.lessons.iter().position(|x| x == l).unwrap() as u32;
            s.applies.iter().find(|a| a.lesson == i).map(|a| (a.route, a.label, a.judge.is_some()))
        };
        assert_eq!(label("A"), Some((Route::Ask, Label::NotArisen, true)), "{:?}", s.applies);
        assert_eq!(label("B"), Some((Route::Ask, Label::Changed, true)));
        assert_eq!(label("C"), Some((Route::Ask, Label::Unclear, false)), "an older kind keeps its label");
        assert_eq!(label("P"), Some((Route::Placebo, Label::NotArisen, true)));
        // Before the later change, B's remaining consequence is its own pending label.
        let first: String = all[..all.len() - 1].iter().map(|x| format!("{}\n", serde_json::to_string(x).unwrap())).collect();
        let s = fold(&first, Summary::default());
        let b = s.lessons.iter().position(|x| x == "B").unwrap() as u32;
        let a = s.applies.iter().find(|a| a.lesson == b).unwrap();
        assert_eq!((a.label, a.judge, a.jv), (Label::Remains, None, 3), "a pending label keeps its judge's version");
        assert!(a.label.pending() && !a.label.verdict() && Label::NotArisen.verdict() && !Label::NotArisen.pending());
        // The session-end judge is a judge level of its own; a check's verdict keeps the level it had.
        let end = [&all[..], &[graded("E", "cE", true, false), ask("E", "cE"), Outcome { at: AT_END.into(), ..on(CHANGED, "E", "cE", "stop") }]].concat();
        let text: String = end.iter().map(|x| format!("{}\n", serde_json::to_string(x).unwrap())).collect();
        let s = fold(&text, Summary::default());
        assert!(s.judges.iter().any(|j| j.starts_with("v3|")) && s.judges.iter().any(|j| j.starts_with("v3@end|")), "{:?}", s.judges);
    }

    /// An open placebo keeps its session waiting to settle when its `end` hook never ran, as an open ask
    /// does; its settle closes it.
    #[test]
    fn idle_sessions_wait_for_open_placebos() {
        let only = vec![placebo(&["A"], "p1", 0)];
        assert_eq!(idle_sessions(&only, &HashMap::new(), IDLE_MS + 10), ["s"]);
        assert!(idle_sessions(&only, &HashMap::new(), IDLE_MS - 10).is_empty(), "the placebo is the session's newest activity");
        let settled = [only.clone(), settle(&only, "s", 5)].concat();
        assert!(idle_sessions(&settled, &HashMap::new(), IDLE_MS + 10).is_empty());
    }

    /// The selection summary reads a placebo's settle as its censoring (select/summary.rs `settled`), so
    /// the judge-missingness fit (select/policy.rs `judged`) counts the placebo as not judged instead of
    /// leaving it out.
    #[test]
    fn the_selection_summary_censors_a_settled_placebo() {
        use crate::select::summary::{Label, Summary, fold};
        let graded = Outcome {
            grades: vec![1, 1],
            ranks: vec![0, 1],
            applied: 2,
            channel: "stop".into(),
            check: "p1".into(),
            policy: "learned".into(),
            placebo: vec!["A".into(), "B".into()],
            ..o(GRADED, "s", &["A", "B"], 5)
        };
        let mut log = vec![graded, placebo(&["A", "B"], "p1", 10), Outcome { channel: PLACEBO.into(), check: "p1".into(), ..o(IGNORED, "s", &["A"], 15) }];
        log.extend(settle(&log, "s", 99));
        let text: String = log.iter().map(|x| format!("{}\n", serde_json::to_string(x).unwrap())).collect();
        let s = fold(&text, Summary::default());
        let label = |l: &str| s.applies.iter().find(|a| s.lessons[a.lesson as usize] == l).map(|a| a.label);
        assert_eq!((label("A"), label("B")), (Some(Label::Ignored), Some(Label::Censored)));
    }

    #[test]
    fn contradicted_marks_superseded_asks_and_the_other_sides_of_a_disagreement() {
        let mut ask = o(ASKED, "s1", &["A", "B", "C", "D"], 10);
        ask.groups = vec![vec!["A".into(), "B".into()]];
        let all = vec![ask, o(ASKED, "s1", &["E"], 30), o(CORRECTED, "s1", &["D"], 15)];
        let ids = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // A plain merge into C: C was asked in vain.
        assert_eq!(contradicted(&all, "s1", 20, Some("C"), &[]), ["C"]);
        // A correction that sides with A in the rendered A/B disagreement: B was wrong, A was right.
        assert_eq!(contradicted(&all, "s1", 20, Some("A"), &[]), ["B"]);
        // A new lesson superseding C and E: only C was asked before it; D is already corrected.
        assert_eq!(contradicted(&all, "s1", 20, None, &ids(&["C", "E", "D", "Z"])), ["C"]);
        assert!(contradicted(&all, "s2", 20, Some("C"), &ids(&["A"])).is_empty(), "another session asked nothing");
        let mut vetoed = all.clone();
        vetoed.push(o(VETOED, "s1", &["C"], 12));
        assert!(contradicted(&vetoed, "s1", 20, Some("C"), &[]).is_empty(), "a vetoed lesson already has the stronger result");
    }

    #[test]
    fn a_placebo_or_unclear_result_is_no_result_for_any_reader() {
        let placebo = |kind: &str| Outcome { channel: PLACEBO.into(), ..o(kind, "s", &["A"], 5) };
        let unclear = Outcome { channel: "stop".into(), ..o(UNCLEAR, "s", &["A"], 5) };
        for x in [placebo(CHANGED), placebo(IGNORED), placebo(UNCLEAR), unclear.clone()] {
            assert!(!is_result(&x), "{x:?}");
            assert!(share(&x, "n1").is_none(), "never shared: {x:?}");
        }
        assert!(is_result(&Outcome { channel: "queued".into(), ..o(CHANGED, "s", &["A"], 5) }));
        assert!(!SHARED_KINDS.contains(&UNCLEAR) && !SHARED_KINDS.contains(&DELIVERY));
        // trust's view (`sessions`): a placebo result moves nothing, whatever its kind.
        let all = [placebo(CORRECTED), placebo(NOT_CORRECTED), o(NOT_CORRECTED, "t", &["A"], 6)];
        assert_eq!(tally(sessions(&all)["A"].values(), i64::MIN), (1, 0));
        // The statistics count no placebo result (`append`).
        let d = tmp("placebo-stats");
        append(&d.join("outcomes.jsonl"), &placebo(CHANGED)).unwrap();
        append(&d.join("outcomes.jsonl"), &unclear).unwrap();
        append(&d.join("outcomes.jsonl"), &Outcome { channel: "stop".into(), ..o(CHANGED, "s", &["B"], 7) }).unwrap();
        let kinds: Vec<String> = crate::stats::events(&d).into_iter().map(|e| e.kind).collect();
        assert_eq!(kinds, [crate::stats::CHANGED], "{kinds:?}");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_vetoed_lesson_is_not_settled_and_counts_as_corrected() {
        let all = vec![o(ASKED, "s1", &["A", "B"], 10), o(VETOED, "s1", &["A"], 20)];
        assert_eq!(settle(&all, "s1", 99), vec![unjudged("s1", "B", UNJUDGED_NO_CALL)]);
        assert_eq!(sessions(&all)["A"]["s1"], (true, 20));
        assert_eq!(weights(&all)["A"], weight(0, 1));
    }

    #[test]
    fn sessions_keep_the_earliest_result_and_tally_filters_by_it() {
        let all =
            vec![o(NOT_CORRECTED, "s1", &["A"], 10), o(CORRECTED, "s2", &["A"], 20), o(NOT_CORRECTED, "s2", &["A"], 25), o(NOT_CORRECTED, "s3", &["A"], 30)];
        let s = sessions(&all);
        assert_eq!(s["A"]["s2"], (true, 20));
        assert_eq!(tally(s["A"].values(), i64::MIN), (2, 1));
        assert_eq!(tally(s["A"].values(), 20), (1, 0));
    }

    #[test]
    fn idle_sessions_need_old_asks_and_old_episodes() {
        let all = vec![o(ASKED, "old", &["A"], 0), o(ASKED, "busy", &["A"], 0), o(ASKED, "fresh", &["A"], 50), o(CORRECTED, "x", &["A"], 0)];
        let episodes = HashMap::from([("busy".to_string(), 60)]);
        assert_eq!(idle_sessions(&all, &episodes, IDLE_MS + 10), ["old"]);
        assert_eq!(idle_sessions(&all, &episodes, IDLE_MS + 60), ["busy", "fresh", "old"]);
    }

    #[test]
    fn idle_sessions_skip_settled_ones() {
        let all = vec![
            o(ASKED, "done", &["A", "B"], 0),
            o(NOT_CORRECTED, "done", &["A"], 1),
            o(CORRECTED, "done", &["B"], 1),
            o(ASKED, "vetoed", &["A"], 0),
            o(VETOED, "vetoed", &["A"], 1),
            o(ASKED, "half", &["A", "B"], 0),
            o(NOT_CORRECTED, "half", &["A"], 1),
        ];
        assert_eq!(idle_sessions(&all, &HashMap::new(), IDLE_MS + 10), ["half"], "every ask of the others has its result");
    }

    #[test]
    fn regretted_blames_asks_before_the_decision_shared_or_by_target() {
        let mut graded = o(GRADED, "s", &["G"], 5);
        graded.chosen = vec![];
        let log = vec![graded, o(ASKED, "s", &["B", "A"], 10), o(ASKED, "s", &["C"], 30), o(ASKED, "t", &["D"], 10)];
        let ids = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let blame = |log: &[Outcome], target: &[String]| regretted(log, "s", 20, "R1", target, 0.5, 99, "r");
        let got = blame(&log, &[]);
        let want = |l: &str, credit: f64| Outcome { cause: "R1".into(), credit, ..o(REGRETTED, "s", &[l], 99) };
        assert_eq!(got, vec![want("A", 0.25), want("B", 0.25)], "shared; C was asked after the decision, G never shown");
        assert_eq!(blame(&log, &ids(&["B", "Z"])), vec![want("B", 0.5)], "the merge target takes it all");
        assert_eq!(blame(&log, &ids(&["Z"])), got, "a target not asked: shared");
        let mut corrected = log.clone();
        corrected.push(o(CORRECTED, "s", &["B"], 15));
        assert_eq!(blame(&corrected, &[]), vec![want("A", 0.5)], "corrected outranks regretted");
        let mut vetoed = log.clone();
        vetoed.push(o(VETOED, "s", &["B"], 15));
        assert_eq!(blame(&vetoed, &[]), vec![want("A", 0.5)], "vetoed outranks regretted");
        assert!(blame(&corrected, &ids(&["B"])).is_empty(), "the target took it as a correction: nobody else is blamed");
        assert!(blame(&vetoed, &ids(&["B", "Z"])).is_empty(), "the target took it as a veto: nobody else is blamed");
        assert_eq!(blame(&corrected, &ids(&["A", "B"])), vec![want("A", 0.5)], "the uncorrected target keeps its share");
        let mut again = log.clone();
        again.extend(got.clone());
        assert!(blame(&again, &[]).is_empty(), "credited once per cause");
        let mut half = log.clone();
        half.push(got[0].clone());
        assert_eq!(blame(&half, &[]), vec![want("B", 0.25)], "a partial run completes with the same share");
        assert_eq!(regretted(&again, "s", 20, "R2", &[], 0.5, 99, "r").len(), 2, "another regret blames again");
        assert!(regretted(&log, "u", 20, "R1", &[], 0.5, 99, "r").is_empty(), "another session asked nothing");
        assert!(regretted(&log, "", 20, "R1", &[], 0.5, 99, "r").is_empty());
        assert!(regretted(&log, "s", 0, "R1", &[], 0.5, 99, "r").is_empty(), "no decision time");
        assert!(regretted(&log, "s", 20, "R1", &[], 0.0, 99, "r").is_empty(), "not strong");
        // An old line without cause and credit still reads; a regretted line round-trips.
        let old: Outcome = serde_json::from_str(r#"{"kind":"asked","session":"s","lessons":["A"],"ms":1,"repo":"r"}"#).unwrap();
        assert_eq!(old, o(ASKED, "s", &["A"], 1));
        assert!(!serde_json::to_string(&old).unwrap().contains("credit"));
        let line = serde_json::to_string(&got[0]).unwrap();
        assert!(line.contains(r#""cause":"R1","credit":0.25"#), "{line}");
        assert_eq!(serde_json::from_str::<Outcome>(&line).unwrap(), got[0]);
    }

    #[test]
    fn a_line_from_before_propensities_reads_and_writes_back_unchanged() {
        let line = r#"{"kind":"asked","session":"s","lessons":["A","B"],"ms":1,"repo":"r","channel":"stop","grades":[3,1],"ranks":[0,1],"applied":2}"#;
        let o: Outcome = serde_json::from_str(line).unwrap();
        assert!(o.p.is_empty());
        assert_eq!(serde_json::to_string(&o).unwrap(), line);
        let with = Outcome { p: vec![round_p(1.0), round_p(1.0 / 3.0)], ..o };
        assert!(serde_json::to_string(&with).unwrap().ends_with(r#""applied":2,"p":[1.0,0.3333]}"#));
    }

    #[test]
    fn a_graded_line_from_before_select_reads_and_writes_back_unchanged() {
        let line = r#"{"kind":"graded","session":"s","lessons":["A","B"],"ms":1,"repo":"r","channel":"live","grades":[3,1],"ranks":[0,1],"applied":2,"chosen":["A","B"],"explore":true,"p":[1.0,0.1]}"#;
        let o: Outcome = serde_json::from_str(line).unwrap();
        assert!(o.check.is_empty() && o.policy.is_empty() && o.free.is_empty() && o.pf.is_empty() && o.judge == 0 && !o.failed);
        assert!(o.nofree.is_empty() && o.tail.is_empty() && o.pt == 0.0 && o.rel.is_empty());
        assert_eq!(serde_json::to_string(&o).unwrap(), line);
        // A 0.6.0 graded line and an early delivery's from before `nofree`, `tail`, `pt` and `rel`.
        for line in [
            r#"{"kind":"graded","session":"s","lessons":["A","B","C"],"ms":1,"repo":"r","channel":"stop","grades":[2,1,1],"ranks":[0,1,2],"applied":3,"p":[0.0,0.0,0.0],"check":"0123456789ab","policy":"learned","fit":"120-ab12","seen":"9-abcdef01","source":"fixed","free":["A","B"],"pf":[1.0,1.0,0.0],"placebo":["C"],"psi":1.0,"eligible":40,"shown":30,"tokens":12000,"fill":[0,4,7],"model":"m","version":"0.6.0","dur_ms":9000,"usd":0.01}"#,
            r#"{"kind":"asked","session":"s","lessons":["A","B"],"ms":2,"repo":"r","channel":"early","p":[1.0,0.5],"route":"asked"}"#,
        ] {
            let o: Outcome = serde_json::from_str(line).unwrap();
            assert!(o.nofree.is_empty() && o.tail.is_empty() && o.pt == 0.0 && o.rel.is_empty(), "{line}");
            assert_eq!((o.xk, o.xs), (None, None), "{line}");
            assert_eq!(serde_json::to_string(&o).unwrap(), line);
        }
    }

    #[test]
    fn the_selection_fields_round_trip_and_stay_local() {
        let g = Outcome {
            check: "0123456789ab".into(),
            policy: "learned".into(),
            fit: "120-ab12".into(),
            free: vec!["B".into()],
            pf: vec![0.0, 1.0, 0.0],
            placebo: vec!["C".into()],
            psi: 0.5,
            nofree: vec!["N9".into()],
            xk: Some(2),
            xs: Some(0),
            tail: vec!["T9".into()],
            pt: 0.125,
            eligible: 40,
            shown: 30,
            tokens: 12_000,
            fill: vec![0, 4, 7],
            model: "m".into(),
            prompt_hash: "ph".into(),
            harness: "claude".into(),
            version: "0.6.0".into(),
            dur_ms: 9_000,
            failed: true,
            ..o(GRADED, "s", &["A", "B", "C"], 5)
        };
        let delivered = Outcome { check: "0123456789ab".into(), route: ROUTE_DROPPED_CHARS.into(), reason: "budget".into(), ..o(ASKED, "s", &["B"], 6) };
        let judged = Outcome { check: "0123456789ab".into(), judge: 2, stripped: true, ..o(CHANGED, "s", &["A"], 7) };
        let early = Outcome { channel: "early".into(), rel: vec![0.75], ..o(ASKED, "s", &["D"], 8) };
        // Shareable kinds carrying every per-check field, so `share` is put to the test on each.
        let carried = Outcome { nofree: g.nofree.clone(), xk: g.xk, xs: g.xs, tail: g.tail.clone(), pt: g.pt, rel: vec![0.75], ..judged.clone() };
        for x in [&g, &delivered, &judged, &early, &carried] {
            let line = serde_json::to_string(x).unwrap();
            assert_eq!(&serde_json::from_str::<Outcome>(&line).unwrap(), x, "round trip");
        }
        // The test's s = 0 is logged, not skipped: it is a draw, not an absent field.
        assert!(serde_json::to_string(&g).unwrap().contains(r#""xk":2,"xs":0"#));
        assert!(share(&g, "n1").is_none(), "graded stays local");
        for x in [&delivered, &judged, &early, &carried] {
            let shared = serde_json::to_string(&share(x, "n1").unwrap()).unwrap();
            for field in
                ["check", "0123456789ab", "route", "reason", "judge", "stripped", "nofree", "N9", "xk", "xs", "tail", "T9", "pt", "0.125", "rel", "0.75"]
            {
                assert!(!shared.contains(field), "{field}: {shared}");
            }
        }
        assert!(ROUTES.iter().all(|r| !r.is_empty() && !r.contains(' ')), "one word each");
    }

    #[test]
    fn an_older_graded_record_is_the_fixed_rule_unless_it_shows_the_cut() {
        let graded = |grades: &[u8], chosen: &[&str], p: &[f64]| Outcome {
            lessons: (0..grades.len()).map(|i| format!("L{i:02}")).collect(),
            grades: grades.to_vec(),
            chosen: chosen.iter().map(|c| c.to_string()).collect(),
            p: p.to_vec(),
            ..o(GRADED, "s", &[], 1)
        };
        // No 3: the fixed rule hands over the 2s, then the 1s, at most CAP.
        let soft: Vec<u8> = [vec![1; 6], vec![2; 6]].concat();
        let old = graded(&soft, &[], &[0.0; 12]);
        assert_eq!(policy_of(&old), "fixed");
        let want: Vec<String> = (6..12).chain(0..4).map(|i| format!("L{i:02}")).collect();
        assert_eq!(free_of(&old), want);
        // A 3: the fixed rule asks, hands over nothing.
        let block = graded(&[3, 1], &["L00", "L01"], &[1.0, 1.0]);
        assert_eq!((policy_of(&block), free_of(&block)), ("fixed", Vec::<String>::new()));
        // The learned cut: a propensity between 0 and 1, or an explored check; the rest go free.
        let cut = graded(&[3, 1, 1, 1], &["L00", "L01"], &[1.0, 1.0, 0.1, 0.1]);
        assert_eq!((policy_of(&cut), free_of(&cut)), ("attention", vec!["L02".to_string(), "L03".into()]));
        let explored = Outcome { explore: true, ..graded(&[3, 1, 1], &["L00", "L01", "L02"], &[1.0, 1.0, 1.0]) };
        assert_eq!((policy_of(&explored), free_of(&explored)), ("attention", Vec::<String>::new()));
        // A record that names its policy is taken at its word, its free list as logged.
        let named = Outcome { policy: "learned".into(), free: vec!["L01".into()], ..graded(&[3, 1], &["L00"], &[1.0, 0.0]) };
        assert_eq!((policy_of(&named), free_of(&named)), ("learned", vec!["L01".to_string()]));
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("borg-outcome-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn lines(path: &Path, v: &[String]) {
        std::fs::write(path, v.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    }

    fn changed(team: &[Outcome], lesson: &str) -> u32 {
        let ls = [crate::conscience::Challenge { id: lesson.into(), ..Default::default() }];
        crate::memory::derive(team, &ls, None)[lesson].changed
    }

    #[test]
    fn share_redacts() {
        let mut a = o(ASKED, "session-raw", &["A"], 5);
        a.repo = "secret-repo".into();
        a.groups = vec![vec!["A".into(), "B".into()]];
        a.grades = vec![3];
        a.ranks = vec![0];
        a.p = vec![0.5];
        let x = share(&a, "n1").unwrap();
        let line = serde_json::to_string(&x).unwrap();
        for raw in ["session-raw", "secret-repo", "groups", "grades", "ranks", "\"id\"", "version"] {
            assert!(!line.contains(raw), "{raw} in {line}");
        }
        assert_eq!((x.s, x.r, x.p, x.node), (tag("session-raw"), tag("secret-repo"), vec![0.5], "n1".to_string()));
        assert_eq!(tag("s").len(), 32);
        for kind in [GRADED, "veto"] {
            assert!(share(&o(kind, "s", &["A"], 1), "n1").is_none(), "{kind} is not shared");
        }
        let regret = share(&Outcome { cause: "episode-raw".into(), credit: 0.25, ..o(REGRETTED, "s", &["A"], 1) }, "n1").unwrap();
        let line = serde_json::to_string(&regret).unwrap();
        assert!(!line.contains("episode-raw") && line.contains(&tag("episode-raw")) && line.contains(r#""credit":0.25"#), "{line}");
    }

    #[test]
    fn pool_fields_stay_local() {
        let a = Outcome { pool: vec!["A".into(), "B".into()], base: vec![1.25, 0.5], scoped: vec![true, false], p: vec![0.5], ..o(ASKED, "s", &["A"], 5) };
        let line = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<Outcome>(&line).unwrap(), a, "round trip");
        let shared = serde_json::to_string(&share(&a, "n1").unwrap()).unwrap();
        for field in ["pool", "base", "scoped"] {
            assert!(line.contains(field) && !shared.contains(field), "{field}: {shared}");
        }
        assert!(!serde_json::to_string(&o(ASKED, "s", &["A"], 5)).unwrap().contains("pool"), "empty: not written");
    }

    #[test]
    fn new_graded_fields_stay_local() {
        let g = Outcome {
            moves: vec!["hold a lock across an await".into()],
            whys: vec!["the send awaits under the lock".into()],
            xtokens: 120,
            line: "a2".into(),
            run: "async".into(),
            ..o(GRADED, "s", &["A"], 5)
        };
        let line = serde_json::to_string(&g).unwrap();
        assert_eq!(serde_json::from_str::<Outcome>(&line).unwrap(), g, "round trip");
        for field in ["moves", "whys", "xtokens", "\"line\"", "\"run\""] {
            assert!(line.contains(field), "{field}: {line}");
        }
        assert!(share(&g, "n1").is_none(), "graded stays local");
        let plain = serde_json::to_string(&o(GRADED, "s", &["A"], 5)).unwrap();
        for field in ["moves", "whys", "xtokens", "\"line\"", "\"run\""] {
            assert!(!plain.contains(field), "empty: not written, older lines write back unchanged: {plain}");
        }
    }

    /// `team_parts` before `SharedRef`: every team line parsed into an owned `Shared`, owned dedup keys.
    fn team_parts_owned(lessons: &Path, on: bool) -> (Vec<Outcome>, Vec<Outcome>) {
        let local = read(&path_beside(lessons));
        if !on {
            return (local, Vec::new());
        }
        let mine: std::collections::HashSet<String> = local.iter().map(|o| tag(&o.session)).collect();
        let text = std::fs::read_to_string(lessons.with_file_name(TEAM_FILE)).unwrap_or_default();
        let lines: Vec<Shared> = text.lines().filter_map(|l| serde_json::from_str::<Shared>(l).ok()).collect();
        let mut given: HashMap<&str, std::collections::HashSet<&str>> = HashMap::new();
        for x in lines.iter().filter(|x| SHARED_KINDS.contains(&x.kind.as_str()) && x.kind != TESTED) {
            given.entry(x.s.as_str()).or_default().extend(x.lessons.iter().map(String::as_str));
        }
        let mut seen: std::collections::HashSet<(String, String, Vec<String>, i64, String, u8)> = std::collections::HashSet::new();
        let mut team = Vec::new();
        for x in lines.iter() {
            if !SHARED_KINDS.contains(&x.kind.as_str())
                || mine.contains(&x.s)
                || !seen.insert((x.kind.clone(), x.s.clone(), x.lessons.clone(), x.ms, x.c.clone(), x.judge))
            {
                continue;
            }
            if x.kind == TESTED && !x.lessons.iter().all(|l| given.get(x.s.as_str()).is_some_and(|g| g.contains(l.as_str()))) {
                continue;
            }
            team.push(Outcome {
                kind: x.kind.clone(),
                session: x.s.clone(),
                lessons: x.lessons.clone(),
                ms: x.ms,
                repo: x.r.clone(),
                p: x.p.to_vec(),
                channel: x.channel.clone(),
                cause: x.c.clone(),
                credit: x.credit,
                judge: x.judge,
                ..Default::default()
            });
        }
        (local, team)
    }

    /// `TEAM_FILE` lines at every edge `Shared` and `SharedRef` could decide apart; `mine` is a session
    /// of this machine's log.
    fn team_edges(mine: &str) -> Vec<String> {
        let x = |kind: &str, s: &str, lessons: &[&str], ms: i64| {
            serde_json::to_string(&Shared {
                kind: kind.into(),
                s: s.into(),
                lessons: lessons.iter().map(|l| l.to_string()).collect(),
                ms,
                node: "n".into(),
                ..Default::default()
            })
            .unwrap()
        };
        let a = x(CHANGED, "t1", &["L1"], 1);
        vec![
            // A `tested` line before what its session was given; one with its kind and session escaped;
            // one whose session was given only in the positional form, which `jsonl::lead` cannot read.
            x(TESTED, "t6", &["L1"], 0),
            x(TESTED, "t6", &["L2"], 0)
                .replace(r#""kind":"tested""#, &format!(r#""kind":"{}""#, crate::jsonl::escaped(TESTED)))
                .replace(r#""s":"t6""#, &format!(r#""s":"{}""#, crate::jsonl::escaped("t6"))),
            x(TESTED, "t7", &["L1"], 0),
            x(ASKED, "t6", &["L1", "L2"], 0),
            r#"["asked","t7",["L1"],0,"n"]"#.into(),
            a.clone(),
            a.clone(),
            x(ASKED, "t1", &["L1", "L2"], 1),
            x(TESTED, "t1", &["L1"], 2),
            x(TESTED, "t1", &["L3"], 2),
            x(TESTED, "t2", &["L1"], 2),
            x(CHANGED, &tag(mine), &["L1"], 3),
            x(GRADED, "t3", &["L1"], 3),
            // Escapes: the same session and lesson as `a`, one ms later.
            x(CHANGED, "t1", &["L1"], 4)
                .replace(r#""s":"t1""#, &format!(r#""s":"{}""#, crate::jsonl::escaped("t1")))
                .replace(r#""lessons":["L1"]"#, &format!(r#""lessons":["{}"]"#, crate::jsonl::escaped("L1"))),
            x(IGNORED, "t4", &["L1"], 5).replacen('{', r#"{"judge":3,"#, 1),
            x(IGNORED, "t4", &["L1"], 5).replacen('{', r#"{"judge":300,"#, 1),
            x(REGRETTED, "t5", &["L1"], 6).replacen('{', r#"{"c":"cause","credit":0.5,"r":"repo","channel":"stop","p":[0.25],"#, 1),
            // Duplicate keys, wrong types, missing or wrong `node`, a lone surrogate in it.
            a.replacen('{', r#"{"kind":"asked","#, 1),
            a.replacen('{', r#"{"node":"m","#, 1),
            a.replacen('{', r#"{"x":1,"x":2,"#, 1),
            a.replace(r#""kind":"asked_then_changed""#, r#""kind":5"#),
            a.replace(r#","node":"n""#, ""),
            a.replace(r#""node":"n""#, r#""node":5"#),
            a.replace(r#""node":"n""#, r#""node":"\ud800""#),
            a.replace(r#""ms":1"#, r#""ms":1.5"#),
            a.replace(r#""lessons":["L1"]"#, r#""lessons":"L1""#),
            a.replacen('{', r#"{"p":["x"],"#, 1),
            // Truncated, trailing characters, whitespace, empty, not an object.
            a.trim_end_matches('}').to_string(),
            a.clone() + " x",
            a.clone() + " ",
            " ".into(),
            String::new(),
            "[]".into(),
            "null".into(),
        ]
    }

    /// `team_parts` keeps exactly the team lines and gives exactly the outcomes the owned parse did, on edge
    /// lines and on the outcome logs of a real store (`BORG_BENCH_STORE`), on and off.
    #[test]
    fn team_parts_read_what_the_owned_parse_read() {
        let dir = tmp("team-parts-oracle");
        let lessons = dir.join("lessons.jsonl");
        let mine = o(ASKED, "mine", &["L1"], 1);
        let real = |f: &str| crate::jsonl::store_lines(&[f]);
        let local = [vec![serde_json::to_string(&mine).unwrap()], real("outcomes.jsonl")].concat();
        for team in [team_edges("mine"), real("outcomes-team.jsonl"), [real("outcomes-team.jsonl"), team_edges("mine")].concat()] {
            lines(&path_beside(&lessons), &local);
            lines(&lessons.with_file_name(TEAM_FILE), &team);
            for on in [true, false] {
                assert_eq!(team_parts(&lessons, on), team_parts_owned(&lessons, on), "on={on}");
            }
        }
        lines(&lessons.with_file_name(TEAM_FILE), &team_edges("mine"));
        lines(&path_beside(&lessons), &[serde_json::to_string(&mine).unwrap()]);
        let (_, team) = team_parts(&lessons, true);
        assert_eq!(team.iter().filter(|o| o.session == "t1" && o.kind == CHANGED).count(), 2, "a line and its copies once, the escaped line apart");
        assert!(!team.iter().any(|o| o.session == tag("mine")), "this machine's own copy");
        let tested = |s: &str| team.iter().filter(|o| o.kind == TESTED && o.session == s).count();
        assert_eq!((tested("t6"), tested("t7")), (2, 1), "given after the test, escaped, in the positional form");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Local outcome lines at every edge `Outcome` and `OutcomeRef` could decide apart: each kind of field
    /// with a wrong type or a number past its type, duplicate keys read and unread, escapes, the positional
    /// form, and lines that are no record.
    fn local_edges() -> Vec<String> {
        let g = Outcome {
            channel: PLACEBO.into(),
            tail: vec!["T1".into(), "L1".into()],
            groups: vec![vec!["L1".into()]],
            grades: vec![3],
            p: vec![0.5],
            xk: Some(2),
            alpha: Some(0.1),
            ..o(GRADED, "s1", &["L1"], 7)
        };
        let a = serde_json::to_string(&g).unwrap();
        let esc = crate::jsonl::escaped;
        let with = |field: &str| a.replacen('{', &format!("{{{field},"), 1);
        let mut out = vec![
            a.clone(),
            serde_json::to_string(&o(CHANGED, "s2", &["L1", "L2"], 8)).unwrap(),
            // Escapes in every string a fold reads.
            a.replace(r#""kind":"graded""#, &format!(r#""kind":"{}""#, esc("graded")))
                .replace(r#""session":"s1""#, &format!(r#""session":"{}""#, esc("s1")))
                .replace(r#""channel":"placebo""#, &format!(r#""channel":"{}""#, esc("placebo")))
                .replace(r#""tail":["T1","L1"]"#, &format!(r#""tail":["{}","L1"]"#, esc("T1")))
                .replace(r#""lessons":["L1"]"#, &format!(r#""lessons":["{}"]"#, esc("L1"))),
            // Duplicate keys, read, unread and unknown.
            with(r#""kind":"asked""#),
            with(r#""model":"m","model":"n""#),
            with(r#""x":1,"x":2"#),
            // Wrong types and numbers past their type in the fields no fold reads.
            with(r#""groups":[[1]]"#),
            with(r#""grades":[256]"#),
            with(r#""ranks":[-1]"#),
            with(r#""applied":70000"#),
            with(r#""explore":1"#),
            with(r#""credit":"1""#),
            with(r#""scoped":[1]"#),
            with(r#""pool":"L1""#),
            with(r#""fill":[65536]"#),
            with(r#""xs":5000000000"#),
            with(r#""xs":null"#),
            with(r#""eligible":-1"#),
            with(r#""tokens":4294967296"#),
            with(r#""dur_ms":18446744073709551616"#),
            with(r#""judge":256"#),
            with(r#""stripped":null"#),
            with(r#""reasons":[null]"#),
            with(r#""at":5"#),
            with(r#""line":"\ud800""#),
            with(r#""psi":true"#),
            // Wrong types and missing fields among those a fold reads, and the required `repo`.
            a.replace(r#""ms":7"#, r#""ms":7.5"#),
            a.replace(r#""ms":7"#, r#""ms":9223372036854775808"#),
            a.replace(r#""lessons":["L1"]"#, r#""lessons":["L1",1]"#),
            a.replace(r#""tail":["T1","L1"]"#, r#""tail":"T1""#),
            a.replace(r#""session":"s1","#, ""),
            a.replace(r#""repo":"r","#, ""),
            a.replace(r#""repo":"r""#, r#""repo":null"#),
            // The positional form serde reads a struct from too.
            r#"["asked","s3",["L1"],9,"r"]"#.into(),
            r#"["asked","s3",["L1"],9]"#.into(),
            r#"["asked","s3",["L1"],9,"r",[],"placebo"]"#.into(),
            // Truncated, trailing characters, whitespace, empty, not an object.
            a.trim_end_matches('}').to_string(),
            format!("{a} x"),
            format!("{a} "),
            " ".into(),
            String::new(),
            "{}".into(),
            "[]".into(),
            "null".into(),
        ];
        out.push(serde_json::to_string(&Outcome { channel: PLACEBO.into(), ..o(IGNORED, "s4", &["L2"], 9) }).unwrap());
        out
    }

    /// What a fold reads of a record, owned, from either kind of line.
    type Read = (String, String, Vec<String>, i64, String, Vec<String>);

    fn owned_read(o: &Outcome) -> Read {
        (o.kind.clone(), o.session.clone(), o.lessons.clone(), o.ms, o.channel.clone(), o.tail.clone())
    }

    fn borrowed_read(o: &OutcomeRef) -> Read {
        let strings = |v: &[Text]| v.iter().map(|t| t.to_string()).collect();
        (o.kind.to_string(), o.session.to_string(), strings(&o.lessons), o.ms, o.channel.to_string(), strings(&o.tail))
    }

    /// `OutcomeRef` takes exactly the lines `Outcome` takes and reads the same values from them, on the
    /// edges and every line of a real store's local log (`BORG_BENCH_STORE`).
    #[test]
    fn the_borrowed_record_reads_what_the_owned_one_read() {
        let mut taken = 0;
        for line in local_edges().iter().chain(&crate::jsonl::store_lines(&["outcomes.jsonl"])) {
            let owned = serde_json::from_str::<Outcome>(line).ok();
            let borrowed = serde_json::from_str::<OutcomeRef>(line).ok();
            assert_eq!(owned.is_some(), borrowed.is_some(), "{line:?}");
            if let (Some(o), Some(r)) = (owned, borrowed) {
                assert_eq!(borrowed_read(&r), owned_read(&o), "{line:?}");
                taken += 1;
            }
        }
        assert!(taken > 3, "the edges hold records");
    }

    /// `fold_parts` hands over exactly the records `team_parts` returns, in its order, on and off: on the
    /// edges and on the outcome logs of a real store (`BORG_BENCH_STORE`).
    #[test]
    fn fold_parts_folds_what_team_parts_returns() {
        let dir = tmp("fold-parts-oracle");
        let lessons = dir.join("lessons.jsonl");
        let real = |f: &str| crate::jsonl::store_lines(&[f]);
        let mine = serde_json::to_string(&o(ASKED, "mine", &["L1"], 1)).unwrap();
        for (local, team) in [
            ([vec![mine.clone()], local_edges()].concat(), team_edges("mine")),
            ([vec![mine.clone()], real("outcomes.jsonl")].concat(), [real("outcomes-team.jsonl"), team_edges("mine")].concat()),
        ] {
            lines(&path_beside(&lessons), &local);
            lines(&lessons.with_file_name(TEAM_FILE), &team);
            for on in [true, false] {
                let (mut l, mut t) = (Vec::new(), Vec::new());
                fold_parts(&lessons, on, |part| match part {
                    Part::Local(o) => l.push(borrowed_read(o)),
                    Part::Team(x) => t.push((x.kind.to_string(), x.s.to_string(), x.lessons.iter().map(|l| l.to_string()).collect(), x.ms)),
                });
                let (want_l, want_t) = team_parts(&lessons, on);
                assert_eq!(l, want_l.iter().map(owned_read).collect::<Vec<_>>(), "on={on}");
                let want_t: Vec<(String, String, Vec<String>, i64)> = want_t.into_iter().map(|o| (o.kind, o.session, o.lessons, o.ms)).collect();
                assert_eq!(t, want_t, "on={on}");
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `publish` before its borrowed set: every team line copied into the set.
    fn publish_owned(home: &Path) -> usize {
        let local = home.join("outcomes.jsonl");
        let team = home.join(TEAM_FILE);
        let mark = home.join(PUBLISHED);
        let len = std::fs::metadata(&local).map_or(0, |m| m.len());
        if !on() || len == 0 {
            return 0;
        }
        let published = std::fs::read_to_string(&mark).ok().and_then(|t| t.trim().parse::<u64>().ok());
        if team.exists() && published == Some(len) {
            return 0;
        }
        let mine = std::fs::read_to_string(&local).unwrap_or_default();
        let node = crate::stats::node(home);
        let text = std::fs::read_to_string(&team).unwrap_or_default();
        let mut have: std::collections::HashSet<String> = text.lines().map(str::to_string).collect();
        let mut out = String::new();
        let personal = crate::reach::personal_ids(home);
        let records = mine.lines().filter_map(|l| serde_json::from_str::<Outcome>(l).ok()).filter_map(|mut o| {
            let named = !o.lessons.is_empty();
            o.lessons.retain(|id| !personal.contains(id));
            (!named || !o.lessons.is_empty()).then_some(o)
        });
        for line in records.filter_map(|o| share(&o, &node)).filter_map(|x| serde_json::to_string(&x).ok()) {
            if have.insert(line.clone()) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        if out.is_empty() {
            let _ = std::fs::write(&mark, mine.len().to_string());
            return 0;
        }
        use std::io::Write;
        let open = text.as_bytes().last().is_some_and(|c| *c != b'\n');
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&team)
            .and_then(|mut f| f.write_all(format!("{}{out}", if open { "\n" } else { "" }).as_bytes()));
        if written.is_err() {
            return 0;
        }
        let _ = std::fs::write(&mark, mine.len().to_string());
        out.lines().count()
    }

    /// `publish` appends exactly the lines the owned set appended: to no team file, one without its last
    /// newline, one holding some of them already, with the same record twice in the log, on edge records
    /// and a real store's logs (`BORG_BENCH_STORE`).
    #[test]
    fn publish_appends_what_the_owned_set_appended() {
        let real = |f: &str| crate::jsonl::store_lines(&[f]);
        let shared = |x: &Outcome| serde_json::to_string(&share(x, "n").unwrap()).unwrap();
        let (a, b) = (o(CHANGED, "s1", &["A"], 1), o(IGNORED, "s2", &["B"], 2));
        let edge_log: Vec<String> = [&a, &a, &b, &o(GRADED, "s1", &["A"], 3)].iter().map(|x| serde_json::to_string(x).unwrap()).collect();
        let cases: Vec<(Vec<String>, Option<String>)> = vec![
            (edge_log.clone(), None),
            (edge_log.clone(), Some(String::new())),
            (edge_log.clone(), Some(shared(&a))),
            (edge_log.clone(), Some(format!("{}\n{}\n", shared(&b), shared(&a)))),
            (real("outcomes.jsonl"), None),
            (real("outcomes.jsonl"), real("outcomes-team.jsonl").first().map(|l| format!("{l}\n"))),
            (real("outcomes.jsonl"), Some(real("outcomes-team.jsonl").iter().map(|l| format!("{l}\n")).collect())),
        ];
        for (i, (log, team)) in cases.into_iter().enumerate() {
            let homes = [tmp(&format!("publish-new-{i}")), tmp(&format!("publish-old-{i}"))];
            for h in &homes {
                std::fs::create_dir_all(h.join("stats")).unwrap();
                std::fs::write(h.join("stats").join("node"), "n").unwrap();
                lines(&h.join("outcomes.jsonl"), &log);
                if let Some(t) = &team {
                    std::fs::write(h.join(TEAM_FILE), t).unwrap();
                }
            }
            assert_eq!(publish(&homes[0]), publish_owned(&homes[1]), "case {i}");
            let team_of = |h: &Path| std::fs::read_to_string(h.join(TEAM_FILE)).ok();
            assert_eq!(team_of(&homes[0]), team_of(&homes[1]), "case {i}");
            for h in homes {
                let _ = std::fs::remove_dir_all(h);
            }
        }
    }

    #[test]
    fn publish_is_idempotent() {
        let home = tmp("publish");
        assert_eq!(publish(&home), 0, "no outcomes, nothing to publish");
        let mut asked = o(ASKED, "s1", &["A"], 1);
        asked.p = vec![1.0];
        lines(&home.join("outcomes.jsonl"), &[asked, o(GRADED, "s1", &["A"], 1), o(CHANGED, "s1", &["A"], 2)].map(|x| serde_json::to_string(&x).unwrap()));
        assert_eq!(publish(&home), 2, "graded stays local");
        let first = std::fs::read_to_string(home.join(TEAM_FILE)).unwrap();
        assert_eq!(publish(&home), 0);
        assert_eq!(std::fs::read_to_string(home.join(TEAM_FILE)).unwrap(), first);
        assert!(first.contains(r#""p":[1.0]"#) && !first.contains("\"s1\""), "{first}");
        // Unchanged since the last publish: the log is not read again; grown: only the new line goes.
        assert_eq!(std::fs::read_to_string(home.join(PUBLISHED)).unwrap(), std::fs::metadata(home.join("outcomes.jsonl")).unwrap().len().to_string());
        std::fs::OpenOptions::new()
            .append(true)
            .open(home.join("outcomes.jsonl"))
            .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{}\n", serde_json::to_string(&o(IGNORED, "s2", &["A"], 3)).unwrap()).as_bytes()))
            .unwrap();
        assert_eq!(publish(&home), 1);
        assert_eq!(publish(&home), 0);
        // A team file gone (a fresh mesh) is published again in full.
        std::fs::remove_file(home.join(TEAM_FILE)).unwrap();
        assert_eq!(publish(&home), 3);
        let _ = std::fs::remove_dir_all(home);
    }

    /// 0.7.10: an ignore's line carries its judge. A node upgraded from before appends a versioned copy
    /// of its judge-3 ignores and nothing else (an older judge's ignore is no longer shared), and a peer
    /// keeps both copies of the ignore and counts it once.
    #[test]
    fn an_upgrade_republishes_only_versioned_ignores_and_a_peer_counts_them_once() {
        let home = tmp("upgrade");
        let log = [Outcome { judge: 2, ..o(CHANGED, "s1", &["A"], 1) }, o(IGNORED, "s1", &["B"], 2), Outcome { judge: 2, ..o(IGNORED, "s1", &["C"], 3) }];
        lines(&home.join("outcomes.jsonl"), &log.clone().map(|x| serde_json::to_string(&x).unwrap()));
        // As a node before 0.7.10 published them: every result, no judge.
        let node = crate::stats::node(&home);
        let before = log.clone().map(|x| serde_json::to_string(&Shared { judge: 0, ..share(&Outcome { judge: 3, ..x }, &node).unwrap() }).unwrap());
        lines(&home.join(TEAM_FILE), &before);
        assert_eq!(publish(&home), 1, "only the judge-3 ignore again");
        let team = std::fs::read_to_string(home.join(TEAM_FILE)).unwrap();
        assert!(team.lines().last().unwrap().contains(r#""lessons":["B"]"#) && team.contains(r#""judge":3"#), "{team}");
        // A peer: both copies of B's ignore kept, C's unversioned one censored, B counted once.
        let peer = tmp("upgrade-peer");
        std::fs::write(peer.join("lessons.jsonl"), "").unwrap();
        std::fs::write(peer.join(TEAM_FILE), &team).unwrap();
        let seen = team_view(&peer.join("lessons.jsonl"), true);
        assert_eq!(seen.iter().filter(|x| x.kind == IGNORED && x.lessons == ["B"]).count(), 2);
        let counted: Vec<&Outcome> = seen.iter().filter(|x| x.kind == IGNORED && is_result(x)).collect();
        assert_eq!(counted.iter().map(|x| x.lessons.clone()).collect::<Vec<_>>(), [vec!["B".to_string()]]);
        let lessons = ["A", "B", "C"].map(|id| crate::conscience::Challenge { id: id.into(), ..Default::default() });
        let m = crate::memory::derive(&seen, &lessons, None);
        assert_eq!((m["B"].ignored, m["C"].ignored), (1, 0));
        let _ = std::fs::remove_dir_all(home);
        let _ = std::fs::remove_dir_all(peer);
    }

    /// A store whose local log has s1 changed L1, and whose team file holds that result as published
    /// by this node, a remote session's change of L1, the same remote line from another node's copy,
    /// and a kind this binary does not share.
    fn team_store(name: &str, shuffle: bool) -> std::path::PathBuf {
        let home = tmp(name);
        let mut local = o(CHANGED, "s1", &["L1"], 10);
        local.repo = "app".into();
        lines(&home.join("outcomes.jsonl"), &[serde_json::to_string(&local).unwrap()]);
        let remote = |kind: &str, s: &str, node: &str| {
            serde_json::to_string(&Shared {
                kind: kind.into(),
                s: s.into(),
                lessons: vec!["L1".into()],
                ms: 20,
                node: node.into(),
                r: tag("web"),
                ..Default::default()
            })
            .unwrap()
        };
        let mut team = vec![
            serde_json::to_string(&share(&local, "me").unwrap()).unwrap(),
            remote(CHANGED, "x", "n2"),
            remote(CHANGED, "x", "n3"),
            remote("hacked", "y", "n2"),
            remote("veto", "z", "n2"),
        ];
        if shuffle {
            team.reverse();
            team.swap(0, 2);
        }
        lines(&home.join(TEAM_FILE), &team);
        home
    }

    #[test]
    fn read_team_drops_own_copies_and_keeps_remote() {
        let home = team_store("team", false);
        let team = team_view(&home.join("lessons.jsonl"), true);
        assert_eq!(changed(&team, "L1"), 2, "{team:?}");
        assert_eq!(team.iter().filter(|o| o.session == "x").count(), 1, "one remote line per (kind, session, lessons, ms)");
        assert!(team.iter().all(|o| o.kind != "hacked" && o.kind != "veto"), "unshared kinds, an older peer's hive verdict among them, are ignored");
        assert!(team.iter().all(|o| o.repo != "app"), "local repos compare as tags");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn read_team_is_order_independent() {
        let (a, b) = (team_store("order-a", false), team_store("order-b", true));
        let ls = [crate::conscience::Challenge { id: "L1".into(), ..Default::default() }];
        let m = |h: &Path| crate::memory::derive(&team_view(&h.join("lessons.jsonl"), true), &ls, None);
        assert_eq!(m(&a), m(&b));
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    /// A node's whole fold reaches its peers: a veto that outranks the session's change, a regret's
    /// credit, and a session that only graded (no tick on either node). A peer holding only the team file
    /// derives the same memory as the node that recorded it.
    #[test]
    fn a_peer_derives_the_same_memory_from_the_team_file() {
        let (a, b) = (tmp("fold-a"), tmp("fold-b"));
        let regret = Outcome { cause: "episode-1".into(), credit: 0.5, ..o(REGRETTED, "s2", &["L1"], 40) };
        let log = [
            o(GRADED, "s0", &["L1"], 5),
            o(ASKED, "s1", &["L1"], 10),
            o(CHANGED, "s1", &["L1"], 20),
            o(VETOED, "s1", &["L1"], 30),
            o(ASKED, "s2", &["L1"], 35),
            regret,
        ];
        lines(&a.join("outcomes.jsonl"), &log.map(|x| serde_json::to_string(&x).unwrap()));
        publish(&a);
        std::fs::copy(a.join(TEAM_FILE), b.join(TEAM_FILE)).unwrap();
        let ls = [crate::conscience::Challenge { id: "L1".into(), origin_ms: 1, ..Default::default() }];
        let m = |h: &Path| crate::memory::derive(&team_view(&h.join("lessons.jsonl"), true), &ls, None);
        let (here, there) = (m(&a), m(&b));
        assert_eq!((here["L1"].vetoed, here["L1"].changed, here["L1"].regretted), (1, 0, 0.5), "{here:?}");
        assert_eq!(here, there);
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    /// The unguarded surface `SHARED_KINDS` documents, pinned until the deferred guards land: a remote
    /// line under one of this node's session tags is dropped here, whatever its kind, while a peer
    /// without that session takes it, and the stronger result wins there.
    #[test]
    fn a_remote_line_under_a_local_tag_counts_on_peers_only() {
        let (victim, peer) = (tmp("forged-victim"), tmp("forged-peer"));
        let own = o(CHANGED, "s1", &["L1"], 10);
        let forged = Shared { kind: CORRECTED.into(), s: tag("s1"), lessons: vec!["L1".into()], ms: 11, node: "n2".into(), ..Default::default() };
        let team = [serde_json::to_string(&share(&own, "me").unwrap()).unwrap(), serde_json::to_string(&forged).unwrap()];
        lines(&victim.join("outcomes.jsonl"), &[serde_json::to_string(&own).unwrap()]);
        lines(&victim.join(TEAM_FILE), &team);
        lines(&peer.join(TEAM_FILE), &team);
        let ls = [crate::conscience::Challenge { id: "L1".into(), ..Default::default() }];
        let m = |h: &Path| crate::memory::derive(&team_view(&h.join("lessons.jsonl"), true), &ls, None).remove("L1").unwrap();
        assert_eq!((m(&victim).changed, m(&victim).corrected), (1, 0), "the victim never sees the override");
        assert_eq!((m(&peer).changed, m(&peer).corrected), (0, 1), "a peer counts the forged correction");
        let _ = std::fs::remove_dir_all(victim);
        let _ = std::fs::remove_dir_all(peer);
    }

    /// A peer's `tested` line counts for a lesson its session's shared records name, and for no other.
    #[test]
    fn a_peer_green_run_counts_only_for_lessons_its_session_was_given() {
        let home = tmp("tested-team");
        let asked = share(&o(ASKED, "p1", &["L1"], 10), "peer").unwrap();
        let ok = share(&o(TESTED, "p1", &["L1"], 20), "peer").unwrap();
        let stray = share(&o(TESTED, "p1", &["L2"], 20), "peer").unwrap();
        let alone = share(&o(TESTED, "p2", &["L1"], 20), "peer").unwrap();
        // A 0.3.0 peer's hive verdict is not shared any more, so it gives its session nothing.
        let verdict = Shared { kind: "verified".into(), ..share(&o(ASKED, "p3", &["L1"], 10), "peer").unwrap() };
        let unbacked = share(&o(TESTED, "p3", &["L1"], 20), "peer").unwrap();
        let all = [&asked, &ok, &stray, &alone, &verdict, &unbacked];
        lines(&home.join(TEAM_FILE), &all.map(|x| serde_json::to_string(x).unwrap()));
        let ls = ["L1", "L2"].map(|id| crate::conscience::Challenge { id: id.into(), ..Default::default() });
        let m = crate::memory::derive(&team_view(&home.join("lessons.jsonl"), true), &ls, None);
        assert_eq!((m["L1"].tested, m["L2"].tested), (1, 0), "{m:?}");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn off_keeps_beliefs_local() {
        let home = team_store("off", false);
        let local = team_view(&home.join("lessons.jsonl"), false);
        assert_eq!(local, read(&home.join("outcomes.jsonl")));
        assert_eq!(changed(&local, "L1"), 1);
        let _ = std::fs::remove_dir_all(home);
    }

    /// Several writers appending to one log at once (several drones on one store): every record
    /// reads back, none torn into `{A}{B}\n\n`.
    #[test]
    fn concurrent_appends_keep_every_record() {
        let home = tmp("concurrent");
        let p = home.join("outcomes.jsonl");
        let (writers, each) = (8, 2000);
        std::thread::scope(|scope| {
            for w in 0..writers {
                let p = &p;
                scope.spawn(move || (0..each).for_each(|i| append(p, &o(ASKED, &format!("s{w}"), &["A"], i)).unwrap()));
            }
        });
        assert_eq!(read(&p).len(), writers * each as usize);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn append_and_read_round_trip() {
        let dir = std::env::temp_dir().join(format!("borg-outcome-{}", std::process::id()));
        let p = dir.join("outcomes.jsonl");
        let _ = std::fs::remove_file(&p);
        append(&p, &o(ASKED, "s", &["A"], 1)).unwrap();
        append(&p, &o(CORRECTED, "s", &["A"], 2)).unwrap();
        assert_eq!(read(&p), vec![o(ASKED, "s", &["A"], 1), o(CORRECTED, "s", &["A"], 2)]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
