//! What a hook needs to decide, folded from the outcome log once and cached, so no check parses the
//! whole log: every graded apply with its route and what became of it, the delivery tallies, the
//! pool's yield and latency, the gate's ledger, the monitor, the checks whose learned decision fell
//! back to the fixed rule, the randomised question-count test's checks, and the fit
//! (select/policy.rs).
//!
//! **The cache** `<store>/select-summary.json` sits beside the lessons file, local and never synced.
//! `load` reads it and folds only the log's new tail (from `at`); a missing, corrupt or older cache, or
//! a log shorter than `at` (rewritten), is rebuilt from the whole log in memory; `peek` (an early or
//! `PostToolUse` hook) never rebuilds: the prior instead. Neither writes: `refresh` (the live check and
//! the stop) loads and saves what it folded, and `refit` (`borg learn`) fits and saves. Any error is
//! the prior, never a panic in a hook.
//!
//! **The fold** is pure over an append-only log: `fold` of a prefix, then of the rest, equals `fold` of
//! the whole at every line boundary, and only whole lines fold (a line being written waits). A result
//! joins the check that decided it by check id (outcome.rs `check`); a record from before check ids by
//! the session's last graded check before it that graded the lesson on the route its channel names, as
//! attention.rs `labels` joins. A later verdict for the same check and lesson supersedes an earlier one
//! and a censored mark, so nothing counts twice. Kinds it does not use skip serde: the kind is read off
//! the line's head first (12,577 `regretted` lines from one session cost a substring each).
//!
//! **Session ticks**: a session's tick is its place in the order sessions first appear in the folded
//! log (memory.rs `Clock`'s idea, in line order so the fold stays incremental); ages are in ticks,
//! never in wall time.
//!
//! **Session ends**: a session's delivery tallies (`Session::tally`) count at its end, whatever it
//! logged: its end hook's `ended` record (outcome.rs `ENDED`) or the `never_delivered` deliveries
//! before it, or a settle (`asked_not_corrected`, which `borg learn` also writes for a session idle
//! without its end hook). A delivery recorded after that (a resumed session) moves its apply's count,
//! so the tallies follow the latest record.
//!
//! **What a session had**: a lesson shown to a session (asked, handed over free or delivered early; a
//! placebo is judged, never shown) is in its `Session::had` from that delivery on in line order, or in
//! `Summary::fresh` until a first graded check or settle gives the session a tick. An apply of a lesson
//! its session had before its check (`Apply::had`: the record's `nofree`, the hook's view, or that set,
//! the log's) counts in no delivery tally, whose rates are those of a lesson the session lacks
//! (select/policy.rs `Rates`): delivery drops a free one as given before, and a question once its lesson
//! is done (hook.rs `done_in`: asked, or handed over and acted on, as the drone may before the question
//! reaches it), drops that say nothing of whether a lesson the session lacks arrives. A lesson waiting
//! in the session's queue from another check is in the record's `nofree` too, which delivery would drop
//! as queued twice. A question of one handed over and not acted on is delivered and judged as any
//! other: its verdict counts (top.rs label health), its delivery stays out of the tallies with the
//! drops. Before 0.6.0 such an apply of a free lesson counted as undelivered and a question as
//! delivered, so the learned free rate read low against asking.
//!
//! **The tail**: a graded record's `tail` holds lessons the pool's budget dropped that a uniform sample
//! showed the model anyway, each with the chance `pt`, and its `fill` holds, after the applies'
//! positions, each tail lesson's position: `shown` plus its place among the dropped lessons in the fill
//! order (whole.rs `select_filled`), where a budget that held them would have put it (`tail_of`). Each
//! counts in the pool's yield bins at its position as a shown lesson, applied when it is one of the
//! applies, so the yield is learned past the budget. Unweighted: a uniform sample's lesson at a position
//! is one draw of that position's yield, and a 1/pt weight would count it as 1/pt lessons, overstating
//! the yield's certainty. A bin the budget splits holds all its positions below the budget and a pt
//! share of those above, so its yield leans to its lower positions.
//!
//! **Compaction** bounds the cache: past `MAX_APPLIES` the oldest checks go, past `MAX_SESSIONS` the
//! oldest sessions with their checks, and with either the sessions before the first kept check's, but
//! never one of the newest `policy::FIT_SESSIONS` (the sessions a fit reads). What the dropped
//! sessions' readers need stays in `Past`, and the kept ones' ticks shift down by as many, so every
//! age stays as it was. A session that logs again after its drop starts a new one.
//!
//! **The question-count test** (select/policy.rs `choose`, rules v2): a graded record of the learned
//! policy that logs k (outcome.rs `xk`) was considered by the test, and one that also logs s (`xs`)
//! drew its count: played (source `EXPERIMENT`, or `EXPERIMENT_CONTROL` when s is the fixed rule's own
//! count), or refused by the gate (`GATE`), which then plays the fixed rule. Every drawn check is one
//! of the test's randomised checks: its routes, as played, and its propensities, over the k + 1 equally
//! likely counts with the refused ones' routes the fixed rule's, are what the draw made them, so the
//! readout reads refused draws too. The fold counts them all-time by k and s (`Summary::experiment`,
//! which compaction keeps) and marks a drawn one's check (`Check::xk`, `xs`), whose applies hold the
//! verdicts of what it asked and handed over: what the readout reads (select/replay.rs `test_readout`),
//! never a recount of the log. A record whose k or s cannot be its own (no s, s past k, k past its
//! applies) counts nowhere. Like every learned check, a drawn one is on the gate's ledger.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::Route;
use super::policy::{self, Fit, Monitor};
use crate::outcome::{self, Outcome};

/// The cache's format. Protocol: a summary of another version is rebuilt, never read (2: `spend`;
/// 3: `Pool::stops`, the monitor's weights; 4: `Past`, `LATER` a live check's, tallies at `ENDED`;
/// 5: `ASK_LIVE`, what a session had (`Session::had`, `Apply::had`, `Apply::nofree`, `Summary::fresh`),
/// tail yields and counts; 6: `Summary::fell_back`; 7: the question-count test, `Summary::experiment`,
/// `Check::xk`, `xs`, `Session::tested`; 8: a stop's results on a live check's hand-overs it asked
/// (`stop_routes`), a live question of a check without a 3 out of `ASK_LIVE` and `WAIT`, `counted`).
pub const SV: u32 = 8;
/// The cache beside the lessons file. Protocol.
pub const CACHE: &str = "select-summary.json";
/// The monitor's auto-revert mark beside the lessons file: while it exists the store keeps the fixed
/// rule, whatever the cache holds (sticky: removing it is the engineer's call, and resumes learning
/// until the monitor trips again). Protocol.
pub const REVERTED: &str = "select-reverted";
/// Graded applies a summary keeps at most. Guard: bounds the cache a hook reads; past it the oldest
/// checks holding a quarter of them go (`Summary::dropped` counts their applies), whose labels the
/// forgetting already weighs least.
pub const MAX_APPLIES: usize = 50_000;
/// Sessions a summary keeps at most. Guard: bounds the session table, which `MAX_APPLIES` does not
/// when sessions grade few applies (a session is about 100 bytes of cache, so 4,096 are 0.4 MB against
/// the applies' 5 MB); past it the oldest quarter go, with their checks, into `Past`.
pub const MAX_SESSIONS: usize = 4_096;
// A compaction keeps three quarters of `MAX_SESSIONS`: never fewer sessions than a fit reads.
const _: () = assert!(MAX_SESSIONS - MAX_SESSIONS / 4 >= policy::FIT_SESSIONS);
/// Sessions not in the table yet whose deliveries a summary keeps (`Summary::fresh`). Guard: bounds the
/// cache against sessions shown lessons (an early delivery at a prompt) that never reach a graded check,
/// which no other bound counts; far more than run at once, about 0.1 MB. Past it the oldest quarter go,
/// and one of them graded after all counts its applies of lessons it had as a session without them.
pub const MAX_FRESH: usize = 1_024;
/// Model calls whose latency the summary keeps, the newest: of all, and of each harness's stops
/// (`Pool::stops`). Guard: bounds the cache, which a long-lived store's calls would grow without end.
pub const LATENCY_KEEP: usize = 2_000;

/// What delivery did with a decided apply (outcome.rs `delivery`, or an older `asked` record).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Delivered {
    /// Nothing recorded yet (and, at its session's end, not delivered).
    #[default]
    Unknown,
    Yes,
    /// Dropped at delivery (the session had it, the budget was spent) or never delivered.
    Dropped,
}

/// What the judge made of an apply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Label {
    /// No verdict yet.
    #[default]
    Open,
    /// Changed the work and avoids the consequence (`asked_then_changed`).
    Changed,
    /// Changed nothing on its point (`asked_ignored`).
    Ignored,
    /// The judge could not tell (`asked_unclear`).
    Unclear,
    /// Its session settled without a verdict (`asked_not_corrected` with an unjudged reason).
    Censored,
}

impl Label {
    /// Does a verdict of kind `next` replace this one? A verdict replaces anything, an unclear only an
    /// open or censored mark, a censored mark only an open one.
    fn yields_to(self, next: Label) -> bool {
        match next {
            Label::Changed | Label::Ignored => true,
            Label::Unclear => matches!(self, Label::Open | Label::Censored),
            Label::Censored => self == Label::Open,
            Label::Open => false,
        }
    }
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// One graded apply: its lesson, grade and rank, the route its check decided with its propensities
/// (logged `p`, `pf`; `psi` a placebo's chance of the draw), and what came of it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Apply {
    /// Its check (`Summary::checks`) and lesson (`Summary::lessons`).
    pub check: u32,
    pub lesson: u32,
    pub grade: u8,
    #[serde(default, skip_serializing_if = "is_default")]
    pub rank: u16,
    pub route: Route,
    #[serde(default, skip_serializing_if = "is_default")]
    pub p: f64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub pf: f64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub psi: f64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub delivered: Delivered,
    #[serde(default, skip_serializing_if = "is_default")]
    pub label: Label,
    /// The judge of its verdict (`Summary::judges`: template version, model and prompt).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<u32>,
    /// The judge saw it without the drone's declaration.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stripped: bool,
    /// Left for later, then delivered later in the session.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub later: bool,
    /// A live question still waiting at the stop, asked there.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub waited: bool,
    /// Its lesson was shown to its session before its check, or was waiting in its queue from another
    /// check (the record's `nofree`; module doc: what a session had): in no delivery tally (`counted`),
    /// though a question of it may still be delivered and judged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub had: bool,
    /// Its check could not hand it over free: in the record's `nofree`, the `had` its decision read
    /// (select.rs `Ctx::had`), so the gate's ledger and the monitor value the check against the fixed
    /// rule it was decided against (select/policy.rs `ledger`, `monitor`). Never the log's view, which
    /// `had` adds and the decision may not have seen.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub nofree: bool,
    /// The session's delivered applies still waiting for a verdict when its check ran.
    #[serde(default, skip_serializing_if = "is_default")]
    pub pending: u16,
}

/// One graded check with applies.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Check {
    /// select.rs `check_id`; empty for a record from before check ids.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// Its session's tick (`Summary::sessions`).
    pub tick: u32,
    pub ms: i64,
    /// The stop's (else live).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stop: bool,
    /// Decided under the learned policy (outcome.rs `policy` "learned"): on the gate's ledger.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub learned: bool,
    /// Its pool's tokens (0 unknown) and its model and prompt (`Summary::models`).
    #[serde(default, skip_serializing_if = "is_default")]
    pub tokens: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub model: u32,
    /// Its applies: `Summary::applies[first..first + n]`; how many it asked and handed over.
    pub first: u32,
    pub n: u16,
    #[serde(default, skip_serializing_if = "is_default")]
    pub m_q: u16,
    #[serde(default, skip_serializing_if = "is_default")]
    pub m_f: u16,
    /// Counted into its session's delivery tallies (at the session's end, module doc).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tallied: bool,
    /// A check whose question-count test drew its count (module doc), played or refused: k, the fixed
    /// rule's delivered non-3 lessons that may go free, and s, the drawn number of them to ask
    /// (outcome.rs `xk`, `xs`). None on every other check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xk: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xs: Option<u32>,
}

/// The delivery tallies of a session, (applies, delivered), counted when it ends (`counted`): a stop's
/// free lessons, a live check's, a live check's lessons left for later, and of a live check with a 3
/// its questions waiting for the stop and its questions delivered. Protocol: the indices of `Session::tally`, part of the cache's layout
/// (`SV`).
pub const FREE_STOP: usize = 0;
pub const FREE_LIVE: usize = 1;
pub const LATER: usize = 2;
pub const WAIT: usize = 3;
pub const ASK_LIVE: usize = 4;
/// How many delivery tallies a session keeps, `FREE_STOP` to `ASK_LIVE`. Protocol: the width of
/// `Session::tally` and `Past::tally`, part of the cache's layout (`SV`).
pub const TALLIES: usize = 5;
const _: () = assert!(ASK_LIVE + 1 == TALLIES);

/// One session, in tick order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    /// Ended (its first end record, module doc).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ended: bool,
    /// Its graded checks, applies or not: what its pools cost (select/policy.rs `price`).
    #[serde(default, skip_serializing_if = "is_default")]
    pub checks: u32,
    /// `FREE_STOP` to `ASK_LIVE`: (applies, delivered).
    #[serde(default, skip_serializing_if = "is_default")]
    pub tally: [(u32, u32); TALLIES],
    /// The lessons shown to it so far (`Summary::lessons`), sorted (module doc: what a session had).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub had: Vec<u32>,
    /// A check of it drew the question-count test's count: counted once in `Experiment::sessions`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tested: bool,
}

/// What a session not in the table yet was shown (`Summary::fresh`), until its first graded check or
/// settle makes it its `Session::had`; `seq`, the log offset after its first delivery, orders the
/// guard's drops (`MAX_FRESH`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Fresh {
    pub seq: u64,
    pub had: Vec<u32>,
}

/// The pool's evidence: per log2 bin of fill position (positions 2^b − 1 to 2^(b+1) − 2), lessons
/// shown and applied; tokens and lessons shown in all; the model calls' (tokens, ms); and per
/// harness, its stops' model calls (ms, failed), what borg top shows against the harness's stop
/// budget (top.rs `Latency`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Pool {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bins: Vec<(u64, u64)>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub tokens: u64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub shown: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub latency: Vec<(u32, u32)>,
    /// Keyed by a harness borg knows (harness/mod.rs `ALL`), the newest `LATENCY_KEEP` of each.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stops: BTreeMap<String, Vec<(u32, bool)>>,
    /// The tail samples' lessons, shown past the pool, and those applied (`tail_of`), as borg top shows
    /// them; also in `bins`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub tail: (u64, u64),
}

/// Early deliveries the judge gave a verdict, and those that changed the work: shown; a learned early
/// rule waits for each delivery's relative score in the log (conscience.rs `RELATIVE`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Early {
    #[serde(default, skip_serializing_if = "is_default")]
    pub judged: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub changed: u32,
}

/// What the checks' model calls spent, per channel (`STOP`, `LIVE`): calls, the extra rounds a stop's
/// block cost its session, and dollars as the CLIs reported them (outcome.rs `usd`; 0 before 0.6.0).
/// Measured and shown, never a limit.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Spent {
    #[serde(default, skip_serializing_if = "is_default")]
    pub calls: u64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub rounds: u64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub usd: f64,
}

/// `Summary::spend`'s channels. Protocol: its indices, part of the cache's layout (`SV`).
pub const STOP: usize = 0;
pub const LIVE: usize = 1;

/// Why a graded check's learned decision fell back to the fixed rule, as its record's `reason` names it
/// (outcome.rs `DECIDED_BUSY`, `DECIDED_NO_LOCK`, `DECIDED_NO_RECORD`). Protocol: the indices of
/// `Summary::fell_back`, part of the cache's layout (`SV`).
pub const FELL_BACK: [&str; 3] = [outcome::DECIDED_BUSY, outcome::DECIDED_NO_LOCK, outcome::DECIDED_NO_RECORD];

/// The randomised question-count test's checks (module doc), all-time: per (k, s) the checks the test
/// ran, each having drawn count s of k, played or refused, sorted (`ran`); of them the control arm's (`EXPERIMENT_CONTROL`: s the
/// fixed rule's own count) and, per k, those the gate refused (`GATE`), sorted, each also in `ran`;
/// the sessions with a drawn check. What `borg top` and the readout count
/// (select/replay.rs `test_readout`), never a recount of the log.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Experiment {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ran: Vec<(u32, u32, u64)>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub control: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused: Vec<(u32, u64)>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub sessions: u32,
}

impl Experiment {
    /// Checks that drew a count, and those the gate refused.
    #[must_use]
    pub fn totals(&self) -> (u64, u64) {
        (self.ran.iter().map(|c| c.2).sum(), self.refused.iter().map(|c| c.1).sum())
    }
}

/// Signals beside the judge, counted before (`Summary::signals[0]`) and after (`[1]`) the first check
/// of the learned policy: sessions, corrections, vetoes, regrets, green test runs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Signals {
    #[serde(default, skip_serializing_if = "is_default")]
    pub sessions: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub corrections: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub vetoes: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub regrets: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub tested: u32,
}

/// The conservative gate's ledger over the learned policy's checks under the current fit
/// (select/policy.rs `ledger_add_without`): per joint draw, the replayed value of what the policy did
/// minus the fixed rule's for the lessons its session had (A); α × the fixed rule's replayed value
/// (B). A deviation passes while A + its own gain stays ≥ −B at the `DELTA` quantile. B is quality
/// (mistakes avoided), not money.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Gate {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gain: Vec<f64>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub allowance: f64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub checks: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub deviated: u32,
}

/// What compaction took off the session table (module doc), as the totals its readers need: those of
/// the dropped sessions with graded checks and their checks (`Summary::checked`), and their delivery
/// tallies as (delivered, not) per half-life of `policy::HALF_LIVES` (same order), each session's
/// weighted by its age at the first kept session (`Summary::tallies` ages them on from there).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Past {
    #[serde(default, skip_serializing_if = "is_default")]
    pub sessions: u32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub checks: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tally: Vec<[(f64, f64); TALLIES]>,
}

impl Past {
    /// `gone`, the oldest sessions, added; what was here already ages by their count.
    fn add(&mut self, gone: &[Session]) {
        if gone.is_empty() {
            return;
        }
        let n = gone.len() as u32;
        self.tally.resize(policy::HALF_LIVES.len(), [(0.0, 0.0); TALLIES]);
        for (sums, &h) in self.tally.iter_mut().zip(&policy::HALF_LIVES) {
            let aged = policy::decay(n, h);
            for e in sums.iter_mut() {
                *e = (e.0 * aged, e.1 * aged);
            }
            for (t, x) in gone.iter().enumerate() {
                weigh(sums, &x.tally, policy::decay(n - t as u32, h));
            }
        }
        for x in gone.iter().filter(|x| x.checks > 0) {
            self.sessions += 1;
            self.checks += u64::from(x.checks);
        }
    }
}

/// A session's `tally`, (applies, delivered), into `sums`, (delivered, not), at weight `w`.
fn weigh(sums: &mut [(f64, f64); TALLIES], tally: &[(u32, u32); TALLIES], w: f64) {
    for (s, &(n, k)) in sums.iter_mut().zip(tally) {
        s.0 += w * f64::from(k);
        s.1 += w * f64::from(n.saturating_sub(k));
    }
}

/// The selection state of one store (module doc).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// `SV` when written.
    pub sv: u32,
    /// The bytes of `outcomes.jsonl` folded so far, and a hash chained over its lines.
    pub at: u64,
    /// The value model decisions draw from; the prior without data.
    #[serde(default)]
    pub fit: Fit,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hash: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<Session>,
    /// Sessions shown lessons before they have a tick, by session id (module doc: what a session had).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fresh: BTreeMap<String, Fresh>,
    /// Lesson ids, model-and-prompt keys and judge keys, by index.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lessons: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub judges: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applies: Vec<Apply>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub pool: Pool,
    #[serde(default, skip_serializing_if = "is_default")]
    pub early: Early,
    #[serde(default, skip_serializing_if = "is_default")]
    pub signals: [Signals; 2],
    #[serde(default, skip_serializing_if = "is_default")]
    pub spend: [Spent; 2],
    /// Graded checks whose learned decision fell back to the fixed rule, per reason (`FELL_BACK`), with
    /// applies or none: a guard that binds often shows (top.rs `Selection::fallbacks`).
    #[serde(default, skip_serializing_if = "is_default")]
    pub fell_back: [u64; FELL_BACK.len()],
    /// The question-count test's checks (module doc).
    #[serde(default, skip_serializing_if = "is_default")]
    pub experiment: Experiment,
    /// A check of the learned policy was folded: the signals count as after.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub era: bool,
    #[serde(default, skip_serializing_if = "is_default")]
    pub gate: Gate,
    #[serde(default, skip_serializing_if = "is_default")]
    pub monitor: Monitor,
    /// Applies compaction dropped, and what it kept of the sessions it dropped.
    #[serde(default, skip_serializing_if = "is_default")]
    pub dropped: u64,
    #[serde(default, skip_serializing_if = "is_default")]
    pub past: Past,
}

impl Default for Summary {
    /// The prior: nothing folded, every decision the fixed rule's.
    fn default() -> Self {
        Summary {
            sv: SV,
            at: 0,
            fit: Fit::default(),
            hash: String::new(),
            sessions: Vec::new(),
            fresh: BTreeMap::new(),
            lessons: Vec::new(),
            models: Vec::new(),
            judges: Vec::new(),
            checks: Vec::new(),
            applies: Vec::new(),
            pool: Pool::default(),
            early: Early::default(),
            signals: Default::default(),
            spend: Default::default(),
            fell_back: Default::default(),
            experiment: Experiment::default(),
            era: false,
            gate: Gate::default(),
            monitor: Monitor::default(),
            dropped: 0,
            past: Past::default(),
        }
    }
}

impl Summary {
    /// What a fit on this summary is keyed by: the bytes folded and the line hash's first 8 hex digits
    /// (protocol: logged with each check, `Log::fit`).
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}-{}", self.at, self.hash.get(..8).unwrap_or(&self.hash))
    }

    /// Does every index point inside its table, and do the checks tile the applies in order? A cache
    /// that parses but fails this is corrupt (`read`): the fold indexes by them.
    fn consistent(&self) -> bool {
        let mut next = 0u32;
        let checks = self.checks.iter().all(|c| {
            let ok = c.first == next && (c.tick as usize) < self.sessions.len() && (c.model as usize) < self.models.len().max(1);
            next = c.first + u32::from(c.n);
            ok
        });
        let applies = self.applies.iter().enumerate().all(|(i, a)| {
            let inside = self.checks.get(a.check as usize).is_some_and(|c| (c.first as usize..c.first as usize + usize::from(c.n)).contains(&i));
            inside && (a.lesson as usize) < self.lessons.len() && a.judge.is_none_or(|j| (j as usize) < self.judges.len())
        });
        let gate = self.gate.gain.is_empty() || self.gate.gain.len() == policy::DRAWS;
        let past = self.past.tally.is_empty() || self.past.tally.len() == policy::HALF_LIVES.len();
        let had = self.sessions.iter().flat_map(|x| &x.had).chain(self.fresh.values().flat_map(|f| &f.had)).all(|l| (*l as usize) < self.lessons.len());
        checks && next as usize == self.applies.len() && applies && gate && past && had
    }

    /// Every folded session's delivery tallies as (delivered, not), each weighted by its age in
    /// sessions under half-life `h` (select/policy.rs `decay`), the compacted ones included (`Past`,
    /// kept for the half-lives a fit chooses from, `policy::HALF_LIVES`).
    #[must_use]
    pub fn tallies(&self, h: Option<u32>) -> [(f64, f64); TALLIES] {
        let now = self.sessions.len() as u32;
        let mut sums = [(0.0, 0.0); TALLIES];
        if let Some(past) = policy::HALF_LIVES.iter().position(|x| *x == h).and_then(|i| self.past.tally.get(i)) {
            let w = policy::decay(now, h);
            for (s, p) in sums.iter_mut().zip(past) {
                *s = (w * p.0, w * p.1);
            }
        }
        for (t, x) in self.sessions.iter().enumerate() {
            weigh(&mut sums, &x.tally, policy::decay(now.saturating_sub(t as u32), h));
        }
        sums
    }

    /// Sessions with graded checks, and their graded checks, the compacted ones included (`Past`):
    /// what spend per session divides by (top.rs).
    #[must_use]
    pub fn checked(&self) -> (u64, u64) {
        let kept = self.sessions.iter().filter(|x| x.checks > 0);
        (u64::from(self.past.sessions) + kept.clone().count() as u64, self.past.checks + kept.map(|x| u64::from(x.checks)).sum::<u64>())
    }
}

/// The lookups a fold needs, rebuilt from the summary at its start (never stored).
#[derive(Default)]
struct Index {
    sessions: HashMap<String, u32>,
    lessons: HashMap<String, u32>,
    models: HashMap<String, u32>,
    judges: HashMap<String, u32>,
    checks: HashMap<String, u32>,
    /// (tick, lesson) → its applies, oldest first.
    by_lesson: HashMap<(u32, u32), Vec<u32>>,
    /// tick → its checks not yet tallied.
    untallied: HashMap<u32, Vec<u32>>,
    /// tick → its delivered applies without a verdict.
    open: HashMap<u32, u32>,
}

impl Index {
    fn of(s: &Summary) -> Index {
        let table = |v: &[String]| v.iter().enumerate().map(|(i, x)| (x.clone(), i as u32)).collect::<HashMap<_, _>>();
        let mut ix = Index {
            sessions: s.sessions.iter().enumerate().map(|(i, x)| (x.id.clone(), i as u32)).collect(),
            lessons: table(&s.lessons),
            models: table(&s.models),
            judges: table(&s.judges),
            ..Index::default()
        };
        for (ci, c) in s.checks.iter().enumerate() {
            if !c.id.is_empty() {
                ix.checks.insert(c.id.clone(), ci as u32);
            }
            if !c.tallied {
                ix.untallied.entry(c.tick).or_default().push(ci as u32);
            }
            for i in c.first..c.first + u32::from(c.n) {
                let Some(a) = s.applies.get(i as usize) else { continue };
                ix.by_lesson.entry((c.tick, a.lesson)).or_default().push(i);
                if waiting(a) {
                    *ix.open.entry(c.tick).or_default() += 1;
                }
            }
        }
        ix
    }
}

/// Is `a` delivered (or drawn as a placebo) and still without a verdict?
fn waiting(a: &Apply) -> bool {
    a.label == Label::Open && a.route != Route::Not && a.delivered != Delivered::Dropped
}

/// The index of `name` in `table`, added when new.
fn intern(table: &mut Vec<String>, ix: &mut HashMap<String, u32>, name: &str) -> u32 {
    if let Some(&i) = ix.get(name) {
        return i;
    }
    table.push(name.to_string());
    ix.insert(name.to_string(), table.len() as u32 - 1);
    table.len() as u32 - 1
}

/// The kinds the fold reads with serde; any other is skipped (module doc).
const USED: [&str; 11] = [
    outcome::GRADED,
    outcome::ASKED,
    outcome::DELIVERY,
    outcome::ENDED,
    outcome::CHANGED,
    outcome::IGNORED,
    outcome::UNCLEAR,
    outcome::NOT_CORRECTED,
    outcome::CORRECTED,
    outcome::MISSED,
    outcome::VETOED,
];

/// The kind at the head of an outcome line (`{"kind":"<kind>"`, as outcome.rs writes every record),
/// or None for a line in another shape (read with serde instead).
fn kind_of(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("{\"kind\":\"")?;
    rest.split_once('"').map(|(k, _)| k)
}

/// `from` with the outcome lines of `text` (appended after `from.at`) folded in; a last line without
/// its newline is left for the next fold.
#[must_use]
pub fn fold(text: &str, from: Summary) -> Summary {
    let mut s = from;
    let mut ix = Index::of(&s);
    for raw in text.split_inclusive('\n') {
        if !raw.ends_with('\n') {
            break;
        }
        s.at += raw.len() as u64;
        let mut h = blake3::Hasher::new();
        h.update(s.hash.as_bytes());
        h.update(raw.as_bytes());
        // 16 hex digits (64 bits). Protocol: the chain `key` reads.
        s.hash = h.finalize().to_hex()[..16].to_string();
        s.line(raw.trim_end(), &mut ix);
    }
    s
}

impl Summary {
    fn line(&mut self, line: &str, ix: &mut Index) {
        let era = usize::from(self.era);
        match kind_of(line) {
            Some(outcome::REGRETTED) => {
                self.signals[era].regrets += 1;
                return;
            }
            Some(outcome::TESTED) => {
                self.signals[era].tested += 1;
                return;
            }
            Some(k) if !USED.contains(&k) => return,
            _ => {}
        }
        let Ok(o) = serde_json::from_str::<Outcome>(line) else { return };
        match o.kind.as_str() {
            outcome::GRADED => self.graded(&o, ix),
            outcome::CHANGED => self.verdict(&o, Label::Changed, ix),
            outcome::IGNORED => self.verdict(&o, Label::Ignored, ix),
            outcome::UNCLEAR => self.verdict(&o, Label::Unclear, ix),
            outcome::NOT_CORRECTED => self.settled(&o, ix),
            outcome::ASKED | outcome::DELIVERY => self.delivered(&o, ix),
            // A session without a graded check or settle has nothing to tally: no entry for it.
            outcome::ENDED => {
                if let Some(&tick) = ix.sessions.get(&o.session) {
                    self.end(tick, ix);
                }
            }
            outcome::CORRECTED => self.signals[era].corrections += 1,
            // The `applied_then_corrected` a regret writes (its `cause` the regret) is the regret again,
            // counted under regrets (memory.rs `is_correction`).
            outcome::MISSED if o.cause.is_empty() => self.signals[era].corrections += 1,
            outcome::VETOED => self.signals[era].vetoes += 1,
            outcome::REGRETTED => self.signals[era].regrets += 1,
            outcome::TESTED => self.signals[era].tested += 1,
            _ => {}
        }
    }

    /// The tick of `session`, a new one when it first appears (past `MAX_SESSIONS`, after a compaction),
    /// with what it was shown before (`fresh`).
    fn tick(&mut self, session: &str, ix: &mut Index) -> u32 {
        if let Some(&t) = ix.sessions.get(session) {
            return t;
        }
        if self.sessions.len() >= MAX_SESSIONS {
            self.compact(ix, 0, MAX_SESSIONS / 4);
        }
        let had = self.fresh.remove(session).map(|f| f.had).unwrap_or_default();
        self.sessions.push(Session { id: session.to_string(), had, ..Session::default() });
        let t = self.sessions.len() as u32 - 1;
        ix.sessions.insert(session.to_string(), t);
        self.signals[usize::from(self.era)].sessions += 1;
        t
    }

    fn graded(&mut self, o: &Outcome, ix: &mut Index) {
        let tick = self.tick(&o.session, ix);
        self.sessions[tick as usize].checks += 1;
        let stop = o.channel != "live";
        let spent = &mut self.spend[if stop { STOP } else { LIVE }];
        spent.calls += 1;
        spent.rounds += u64::from(stop && !o.chosen.is_empty());
        spent.usd += o.usd;
        // A learned decision the fixed rule took over (hook.rs `decide`), by its reason (one this binary
        // does not know is not counted).
        if let Some(i) = FELL_BACK.iter().position(|r| *r == o.reason) {
            self.fell_back[i] += 1;
        }
        // A failed call applied nothing because it answered nothing: its pool says nothing of the yield.
        if o.shown > 0 && !o.failed {
            self.pool.shown += u64::from(o.shown);
            self.pool.tokens += u64::from(o.tokens);
            let bin = |k: u64| (u64::BITS - (k + 1).leading_zeros() - 1) as usize;
            let last = bin(u64::from(o.shown) - 1);
            if self.pool.bins.len() <= last {
                self.pool.bins.resize(last + 1, (0, 0));
            }
            for (b, e) in self.pool.bins.iter_mut().enumerate().take(last + 1) {
                let (lo, hi) = ((1u64 << b) - 1, (1u64 << (b + 1)) - 1);
                e.0 += u64::from(o.shown).min(hi) - lo;
            }
            for &f in o.fill.iter().take(o.lessons.len()).filter(|f| u32::from(**f) < o.shown) {
                if let Some(e) = self.pool.bins.get_mut(bin(u64::from(f))) {
                    e.1 += 1;
                }
            }
            // The tail sample, past the pool (module doc): each a shown lesson at its position.
            for (l, f) in tail_of(o) {
                let b = bin(u64::from(f));
                if self.pool.bins.len() <= b {
                    self.pool.bins.resize(b + 1, (0, 0));
                }
                let applied = u64::from(o.lessons.iter().any(|x| x == l));
                let e = &mut self.pool.bins[b];
                e.0 += 1;
                e.1 += applied;
                self.pool.tail.0 += 1;
                self.pool.tail.1 += applied;
            }
        }
        if o.dur_ms > 0 && o.tokens > 0 {
            if self.pool.latency.len() >= LATENCY_KEEP {
                self.pool.latency.remove(0);
            }
            self.pool.latency.push((o.tokens, u32::try_from(o.dur_ms).unwrap_or(u32::MAX)));
        }
        // A stop's call against its harness's stop budget; a name borg does not know has no budget, and
        // would let a log grow the cache's keys.
        if stop && o.dur_ms > 0 && crate::harness::ALL.iter().any(|h| h.name() == o.harness) {
            let calls = self.pool.stops.entry(o.harness.clone()).or_default();
            if calls.len() >= LATENCY_KEEP {
                calls.remove(0);
            }
            calls.push((u32::try_from(o.dur_ms).unwrap_or(u32::MAX), o.failed));
        }
        let learned = o.policy == "learned";
        self.era |= learned;
        // Malformed, or past what a check's count holds (`Check::n`): no applies.
        let Ok(n) = u16::try_from(o.lessons.len()) else { return };
        if o.failed || n == 0 || o.grades.len() != o.lessons.len() {
            return;
        }
        let free = outcome::free_of(o);
        let model = format!("{}|{}", o.model, o.prompt_hash);
        let model = intern(&mut self.models, &mut ix.models, &model);
        let ci = self.checks.len() as u32;
        let first = self.applies.len() as u32;
        let pending = u16::try_from(ix.open.get(&tick).copied().unwrap_or(0)).unwrap_or(u16::MAX);
        let mut routes = Vec::with_capacity(o.lessons.len());
        for (i, l) in o.lessons.iter().enumerate() {
            let route = if o.chosen.contains(l) {
                Route::Ask
            } else if free.contains(l) {
                Route::Free
            } else if o.placebo.contains(l) {
                Route::Placebo
            } else {
                Route::Not
            };
            routes.push(route);
            // Older records logged only `p`; a free lesson's was 1 under the fixed rule.
            let pf = o.pf.get(i).copied().unwrap_or(f64::from(u8::from(route == Route::Free)));
            let lesson = intern(&mut self.lessons, &mut ix.lessons, l);
            let rank = o.ranks.get(i).copied().unwrap_or(i as u16);
            let psi = if matches!(route, Route::Placebo | Route::Not) { o.psi } else { 0.0 };
            // Either view suffices: the hook's state may hold a delivery the log has not yet, and the log
            // one the state the hook read did not (a delivery during the model call).
            let nofree = o.nofree.contains(l);
            let had = nofree || self.sessions[tick as usize].had.binary_search(&lesson).is_ok();
            let p = o.p.get(i).copied().unwrap_or(0.0);
            let a = Apply { check: ci, lesson, grade: o.grades[i], rank, route, p, pf, psi, had, nofree, pending, ..Apply::default() };
            if waiting(&a) {
                *ix.open.entry(tick).or_default() += 1;
            }
            self.applies.push(a);
            ix.by_lesson.entry((tick, lesson)).or_default().push(first + i as u32);
        }
        let count = |r: Route| u16::try_from(routes.iter().filter(|x| **x == r).count()).unwrap_or(u16::MAX);
        self.checks.push(Check {
            id: o.check.clone(),
            tick,
            ms: o.ms,
            stop: o.channel == "stop",
            learned,
            tokens: o.tokens,
            model,
            first,
            n,
            m_q: count(Route::Ask),
            m_f: count(Route::Free),
            tallied: false,
            xk: None,
            xs: None,
        });
        if learned {
            self.count_test(o, ci as usize, tick);
        }
        if !o.check.is_empty() {
            ix.checks.insert(o.check.clone(), ci);
        }
        ix.untallied.entry(tick).or_default().push(ci);
        if learned && self.fit.model.is_some() {
            let units: Vec<super::Unit> =
                o.lessons.iter().zip(&o.grades).enumerate().map(|(r, (l, g))| super::Unit { lesson: l, grade: *g, rank: r }).collect();
            let check = policy::Check { session: &o.session, stop: o.channel == "stop", tokens: o.tokens };
            // Against the fixed rule the decision was gated against: under the lessons it had.
            policy::ledger_add_without(&self.fit, &mut self.gate, &check, &units, &routes, &o.nofree);
        }
        if self.applies.len() > MAX_APPLIES {
            self.compact(ix, MAX_APPLIES / 4, 0);
        }
    }

    /// The question-count test at the learned check `ci` of record `o` (module doc): a drawn count
    /// counted by its k and s and marked on the check, its session counted once; a refusal also by its k.
    fn count_test(&mut self, o: &Outcome, ci: usize, tick: u32) {
        let Some(k) = o.xk.filter(|k| *k as usize <= o.lessons.len()) else { return };
        let source = o.source.as_str();
        if ![policy::EXPERIMENT, policy::EXPERIMENT_CONTROL, policy::GATE].contains(&source) {
            return;
        }
        // Every source of the test logs its draw's s (select/policy.rs `choose`): a record without one
        // is not the test's.
        let Some(s) = o.xs.filter(|s| *s <= k) else { return };
        let e = &mut self.experiment;
        if source == policy::GATE {
            match e.refused.binary_search_by_key(&k, |c| c.0) {
                Ok(i) => e.refused[i].1 += 1,
                Err(i) => e.refused.insert(i, (k, 1)),
            }
        }
        match e.ran.binary_search_by_key(&(k, s), |c| (c.0, c.1)) {
            Ok(i) => e.ran[i].2 += 1,
            Err(i) => e.ran.insert(i, (k, s, 1)),
        }
        e.control += u64::from(source == policy::EXPERIMENT_CONTROL);
        (self.checks[ci].xk, self.checks[ci].xs) = (Some(k), Some(s));
        let x = &mut self.sessions[tick as usize];
        e.sessions += u32::from(!x.tested);
        x.tested = true;
    }

    /// The apply of `lesson` that a result in `o` belongs to: in its check by id, else (an older
    /// record) the session's last graded check before it that graded the lesson on one of `routes`.
    fn apply_of(&self, o: &Outcome, lesson: &str, routes: &[Route], ix: &Index) -> Option<usize> {
        let l = *ix.lessons.get(lesson)?;
        if !o.check.is_empty() {
            let c = &self.checks[*ix.checks.get(&o.check)? as usize];
            return (c.first..c.first + u32::from(c.n)).map(|i| i as usize).find(|&i| self.applies[i].lesson == l && routes.contains(&self.applies[i].route));
        }
        let tick = *ix.sessions.get(&o.session)?;
        ix.by_lesson.get(&(tick, l))?.iter().rev().map(|&i| i as usize).find(|&i| {
            let a = &self.applies[i];
            routes.contains(&a.route) && self.checks[a.check as usize].ms < o.ms
        })
    }

    /// The routes a result on channel stop names in its check: a stop check's questions; a live check's
    /// questions waiting for the stop, and its hand-overs in place of questions the stop asked in their
    /// stead (hook.rs `stop_ask`: `#borg-routed`, a delivery with the reason "in place of a hand-over"),
    /// whose apply stays the free one its check decided, as the question-count test drew it (its
    /// verdict and delivery land there: intent to treat). A lesson is on one route in its check.
    fn stop_routes(&self, o: &Outcome, ix: &Index) -> &'static [Route] {
        let live = ix.checks.get(&o.check).is_some_and(|&ci| self.checks.get(ci as usize).is_some_and(|c| !c.stop));
        if live { &[Route::Ask, Route::Free] } else { &[Route::Ask] }
    }

    /// Whether check `ci` graded a 3: a stop may ask its live questions while they wait (hook.rs
    /// `Verdict::Ask`), and without one no stop does.
    fn three(&self, ci: usize) -> bool {
        let Some(c) = self.checks.get(ci) else { return false };
        self.applies.get(c.first as usize..c.first as usize + usize::from(c.n)).is_some_and(|a| a.iter().any(|a| a.grade == 3))
    }

    /// Sets apply `i`'s label to `next` when it yields to it, keeping the session's open count.
    fn label(&mut self, i: usize, next: Label, ix: &mut Index) {
        let a = &self.applies[i];
        if !a.label.yields_to(next) {
            return;
        }
        let tick = self.checks[a.check as usize].tick;
        let before = waiting(a);
        self.applies[i].label = next;
        if before
            && !waiting(&self.applies[i])
            && let Some(n) = ix.open.get_mut(&tick)
        {
            *n = n.saturating_sub(1);
        }
    }

    fn verdict(&mut self, o: &Outcome, label: Label, ix: &mut Index) {
        if o.channel == "early" {
            if label != Label::Unclear {
                self.early.judged += 1;
                self.early.changed += u32::from(label == Label::Changed);
            }
            return;
        }
        let routes: &[Route] = match o.channel.as_str() {
            outcome::PLACEBO => &[Route::Placebo],
            "stop" => self.stop_routes(o, ix),
            "live" => &[Route::Ask],
            "queued" => &[Route::Free],
            _ => &[Route::Ask, Route::Free],
        };
        for l in &o.lessons {
            let Some(i) = self.apply_of(o, l, routes, ix) else { continue };
            self.label(i, label, ix);
            if label != Label::Unclear {
                let model = self.models.get(self.checks[self.applies[i].check as usize].model as usize).cloned().unwrap_or_default();
                let key = format!("v{}|{model}", o.judge);
                self.applies[i].judge = Some(intern(&mut self.judges, &mut ix.judges, &key));
                self.applies[i].stripped = o.stripped;
            }
        }
    }

    /// A settle record: the session ended (`end`); a lesson it settles without a verdict (an unjudged
    /// reason) is censored.
    fn settled(&mut self, o: &Outcome, ix: &mut Index) {
        let tick = self.tick(&o.session, ix);
        self.end(tick, ix);
        if o.reason == outcome::UNJUDGED_NONE {
            return;
        }
        for l in &o.lessons {
            if let Some(i) = self.apply_of(o, l, &[Route::Ask, Route::Free, Route::Placebo], ix) {
                self.label(i, Label::Censored, ix);
            }
        }
    }

    /// The end of session `tick` (module doc): its checks not yet tallied count now.
    fn end(&mut self, tick: u32, ix: &mut Index) {
        self.sessions[tick as usize].ended = true;
        for ci in ix.untallied.remove(&tick).unwrap_or_default() {
            self.tally(ci as usize);
        }
    }

    /// Counts check `ci`'s applies into its session's delivery tallies (`counted`).
    fn tally(&mut self, ci: usize) {
        let three = self.three(ci);
        let c = &mut self.checks[ci];
        c.tallied = true;
        let (tick, stop, range) = (c.tick as usize, c.stop, c.first as usize..c.first as usize + usize::from(c.n));
        for i in range {
            let Some(a) = self.applies.get(i) else { continue };
            for (which, hit) in counted(a, stop, three).into_iter().flatten() {
                let t = &mut self.sessions[tick].tally[which];
                t.0 += 1;
                t.1 += u32::from(hit);
            }
        }
    }

    /// Changes apply `i` by `set` (its delivery, never its route or `had`); when its check is tallied
    /// already (its session ended, then resumed), its counts in the tallies move with it.
    fn update(&mut self, i: usize, set: impl FnOnce(&mut Apply)) {
        let Some(ci) = self.applies.get(i).map(|a| a.check as usize) else { return };
        let Some(c) = self.checks.get(ci) else { return };
        let (tallied, stop, tick, three) = (c.tallied, c.stop, c.tick as usize, self.three(ci));
        let before = counted(&self.applies[i], stop, three);
        set(&mut self.applies[i]);
        let after = counted(&self.applies[i], stop, three);
        let Some(x) = self.sessions.get_mut(tick).filter(|_| tallied) else { return };
        for (b, a) in before.into_iter().zip(after) {
            if let (Some((which, was)), Some((_, is))) = (b, a)
                && was != is
            {
                let hits = &mut x.tally[which].1;
                *hits = if is { *hits + 1 } else { hits.saturating_sub(1) };
            }
        }
    }

    /// `lesson` shown to `session` (module doc: what a session had): into the `Session::had` of its tick,
    /// or, without one yet, its `fresh` entry (past `MAX_FRESH`, the oldest quarter go first). Its index.
    fn show(&mut self, tick: Option<u32>, session: &str, lesson: &str, ix: &mut Index) -> u32 {
        let l = intern(&mut self.lessons, &mut ix.lessons, lesson);
        let had = match tick {
            Some(t) => &mut self.sessions[t as usize].had,
            None => {
                if self.fresh.len() >= MAX_FRESH && !self.fresh.contains_key(session) {
                    let mut seqs: Vec<u64> = self.fresh.values().map(|f| f.seq).collect();
                    seqs.sort_unstable();
                    let keep = seqs[MAX_FRESH / 4];
                    self.fresh.retain(|_, f| f.seq >= keep);
                }
                let seq = self.at;
                &mut self.fresh.entry(session.to_string()).or_insert_with(|| Fresh { seq, had: Vec::new() }).had
            }
        };
        if let Err(at) = had.binary_search(&l) {
            had.insert(at, l);
        }
        l
    }

    /// A delivery (outcome.rs `delivery` with its route, or an older `asked` record): the decided
    /// apply's fate; and, shown to the drone (not a placebo), the lesson the session had (`show`) and
    /// every apply of it an earlier check of the session left for later now delivered later. A
    /// `never_delivered` one ends the session.
    fn delivered(&mut self, o: &Outcome, ix: &mut Index) {
        let (fate, routes): (Delivered, &[Route]) = match (o.kind.as_str(), o.route.as_str()) {
            (outcome::DELIVERY, outcome::ROUTE_ASKED) if o.channel == "stop" => (Delivered::Yes, self.stop_routes(o, ix)),
            (outcome::DELIVERY, outcome::ROUTE_ASKED) => (Delivered::Yes, &[Route::Ask]),
            (outcome::DELIVERY, outcome::ROUTE_FREE) => (Delivered::Yes, &[Route::Free]),
            (outcome::DELIVERY, outcome::ROUTE_PLACEBO) => (Delivered::Yes, &[Route::Placebo]),
            (outcome::DELIVERY, _) => (Delivered::Dropped, &[Route::Ask, Route::Free, Route::Placebo]),
            (_, _) if o.channel == "queued" => (Delivered::Yes, &[Route::Free]),
            (_, _) if o.channel == "stop" => (Delivered::Yes, self.stop_routes(o, ix)),
            (_, _) if o.channel == "live" => (Delivered::Yes, &[Route::Ask]),
            _ => (Delivered::Yes, &[]),
        };
        // A placebo is judged, never shown: not had, and no later delivery of anything.
        let shown = fate == Delivered::Yes && o.route != outcome::ROUTE_PLACEBO;
        let Some(&tick) = ix.sessions.get(&o.session) else {
            // An early delivery before the session's first check: what that check's applies had.
            for l in o.lessons.iter().filter(|_| shown) {
                self.show(None, &o.session, l, ix);
            }
            return;
        };
        for l in &o.lessons {
            let own = if routes.is_empty() { None } else { self.apply_of(o, l, routes, ix) };
            if let Some(i) = own {
                let before = waiting(&self.applies[i]);
                let c = &self.checks[self.applies[i].check as usize];
                let waited = fate == Delivered::Yes && o.channel == "stop" && !c.stop && self.applies[i].route == Route::Ask;
                self.update(i, |a| {
                    a.delivered = fate;
                    a.waited |= waited;
                });
                let after = waiting(&self.applies[i]);
                let open = ix.open.entry(tick).or_default();
                match (before, after) {
                    (true, false) => *open = open.saturating_sub(1),
                    (false, true) => *open += 1,
                    _ => {}
                }
            }
            if !shown {
                continue;
            }
            let lesson = self.show(Some(tick), &o.session, l, ix);
            let own_check = own.map(|i| self.applies[i].check);
            for &i in ix.by_lesson.get(&(tick, lesson)).map_or(&[][..], Vec::as_slice) {
                let a = &self.applies[i as usize];
                if a.route == Route::Not && Some(a.check) != own_check && self.checks[a.check as usize].ms < o.ms {
                    self.update(i as usize, |a| a.later = true);
                }
            }
        }
        // Never delivered because the session ended first: the end hook's, before its `ended`.
        if o.kind == outcome::DELIVERY && o.route == outcome::ROUTE_NEVER {
            self.end(tick, ix);
        }
    }

    /// Compaction (module doc): drops the oldest checks holding at least `applies` applies; then the
    /// oldest sessions, at least `sessions` of them and every one before the first kept check's, with
    /// their checks, but none of the newest `policy::FIT_SESSIONS` and an even count (the monitor's
    /// halves are the ticks' parities, select/policy.rs `monitor`), into `Past`; and the lessons, models
    /// and judges nothing kept names (an apply, or what a kept or fresh session had).
    fn compact(&mut self, ix: &mut Index, applies: usize, sessions: usize) {
        let cut = self.checks.iter().position(|c| c.first as usize >= applies).unwrap_or(self.checks.len());
        let len = self.sessions.len();
        let oldest = self.checks[cut..].iter().map(|c| c.tick as usize).min().unwrap_or(len);
        let gone = oldest.max(sessions).min(len.saturating_sub(policy::FIT_SESSIONS));
        let gone = gone - gone % 2;
        self.past.add(&self.sessions[..gone]);
        self.sessions.drain(..gone);
        let (checks, all) = (std::mem::take(&mut self.checks), std::mem::take(&mut self.applies));
        for (ci, mut c) in checks.into_iter().enumerate() {
            if ci < cut || (c.tick as usize) < gone {
                continue;
            }
            let of = all.get(c.first as usize..c.first as usize + usize::from(c.n)).unwrap_or_default();
            let check = self.checks.len() as u32;
            (c.first, c.tick) = (self.applies.len() as u32, c.tick - gone as u32);
            self.applies.extend(of.iter().map(|a| Apply { check, ..a.clone() }));
            self.checks.push(c);
        }
        self.dropped += (all.len() - self.applies.len()) as u64;
        let had = self.sessions.iter().flat_map(|x| &x.had).chain(self.fresh.values().flat_map(|f| &f.had)).copied();
        let lessons = prune(&mut self.lessons, self.applies.iter().map(|a| a.lesson).chain(had));
        let judges = prune(&mut self.judges, self.applies.iter().filter_map(|a| a.judge));
        let models = prune(&mut self.models, self.checks.iter().map(|c| c.model));
        let new = |map: &[u32], i: u32| map.get(i as usize).copied().unwrap_or(i);
        for a in &mut self.applies {
            (a.lesson, a.judge) = (new(&lessons, a.lesson), a.judge.map(|j| new(&judges, j)));
        }
        // `prune` keeps the order, so each `had` stays sorted.
        for had in self.sessions.iter_mut().map(|x| &mut x.had).chain(self.fresh.values_mut().map(|f| &mut f.had)) {
            for l in had.iter_mut() {
                *l = new(&lessons, *l);
            }
        }
        for c in &mut self.checks {
            c.model = new(&models, c.model);
        }
        *ix = Index::of(self);
    }
}

/// The delivery tallies an apply of a stop's (else a live) check counts into, each with whether it
/// counts as delivered: a free lesson, by its check's channel; a lesson left for later, at a live check
/// only (ρ, select/policy.rs `Rates::later`, is shown only and is a lesson left for later during the
/// turn: a stop's arrives later only if a next prompt brings it again); a live question of a check
/// with a 3 (`three`), whether it reached the drone (during the turn, or waiting for the stop and asked
/// in its block) and whether it waited for the stop. None for an apply of a lesson its session had
/// (`Apply::had`; module doc: the rates are those of a lesson the session lacks, and a question of it
/// may still be delivered and judged), an ask at the stop (it blocks: certain), a live question of a
/// check without a 3 (no stop asks it: it arrives at a tool call or the next prompt, as a hand-over
/// does, and is valued at the free rate, select/policy.rs `Valuer::ask`) and a placebo.
fn counted(a: &Apply, stop: bool, three: bool) -> [Option<(usize, bool)>; 2] {
    if a.had {
        return [None; 2];
    }
    match a.route {
        Route::Free => [Some((if stop { FREE_STOP } else { FREE_LIVE }, a.delivered == Delivered::Yes)), None],
        Route::Not if !stop => [Some((LATER, a.later)), None],
        Route::Ask if !stop && three => [Some((ASK_LIVE, a.delivered == Delivered::Yes)), Some((WAIT, a.waited))],
        _ => [None; 2],
    }
}

/// A graded record's tail sample (module doc) with each lesson's fill position: `fill` past the
/// applies' (aligned with `tail`), each one past the pool (`shown`). Empty without a tail, and for a
/// record not in that shape or without its chance `pt` (0 < pt ≤ 1): no uniform sample then.
fn tail_of(o: &Outcome) -> Vec<(&str, u16)> {
    let at = o.fill.get(o.lessons.len()..).unwrap_or_default();
    let past = at.iter().all(|f| u32::from(*f) >= o.shown && *f != u16::MAX);
    if o.tail.is_empty() || at.len() != o.tail.len() || !past || !(o.pt > 0.0 && o.pt <= 1.0) {
        return Vec::new();
    }
    o.tail.iter().map(String::as_str).zip(at.iter().copied()).collect()
}

/// `table` cut to the names `used` points at, in their order; and for each old index, its new one.
fn prune(table: &mut Vec<String>, used: impl Iterator<Item = u32>) -> Vec<u32> {
    let mut keep = vec![false; table.len()];
    for i in used {
        if let Some(k) = keep.get_mut(i as usize) {
            *k = true;
        }
    }
    let mut map = Vec::with_capacity(keep.len());
    for (name, k) in std::mem::take(table).into_iter().zip(keep) {
        map.push(table.len() as u32);
        if k {
            table.push(name);
        }
    }
    map
}

/// The cache of the store whose lessons file is `lessons`.
#[must_use]
pub fn cache_path(lessons: &Path) -> PathBuf {
    lessons.with_file_name(CACHE)
}

/// The auto-revert mark of that store.
#[must_use]
pub fn reverted_path(lessons: &Path) -> PathBuf {
    lessons.with_file_name(REVERTED)
}

/// The summary of the store whose lessons file is `lessons` (module doc): the cache with the log's
/// new tail folded in, rebuilt in memory when it cannot be resumed. Never writes.
#[must_use]
pub fn load(lessons: &Path) -> Summary {
    read(lessons, true)
}

/// `load` without a rebuild, for a hook that must stay cheap (an early delivery, `PostToolUse`): the
/// prior when the cache cannot be resumed.
#[must_use]
pub fn peek(lessons: &Path) -> Summary {
    read(lessons, false)
}

/// The live check's and the stop's `load`: the summary, saved when it holds more than the cache did
/// (a fold of a new tail, a rebuild). A failed save costs the next hook the same fold, nothing else.
pub fn refresh(lessons: &Path) -> Summary {
    let (s, cached) = read_at(lessons, true);
    if cached != Some(s.at) {
        let _ = save(lessons, &s);
    }
    s
}

fn read(lessons: &Path, rebuild_ok: bool) -> Summary {
    read_at(lessons, rebuild_ok).0
}

/// `read`, and the `at` of the cache it resumed (None: none, or one it could not use).
fn read_at(lessons: &Path, rebuild_ok: bool) -> (Summary, Option<u64>) {
    let log = outcome::path_beside(lessons);
    let len = std::fs::metadata(&log).map_or(0, |m| m.len());
    let cached = std::fs::read(cache_path(lessons))
        .ok()
        .and_then(|b| serde_json::from_slice::<Summary>(&b).ok())
        .filter(|s| s.sv == SV && s.at <= len && s.consistent());
    let reverted = reverted_path(lessons).exists();
    let mut resumed = cached.as_ref().map(|s| s.at);
    let Some(mut s) = cached.or_else(|| rebuild_ok.then(|| rebuild(lessons))) else {
        return (Summary { monitor: Monitor { reverted, ..Monitor::default() }, ..Summary::default() }, None);
    };
    if s.at < len {
        match tail(&log, s.at) {
            Some(text) => s = fold(&text, s),
            None if rebuild_ok => (s, resumed) = (rebuild(lessons), None),
            None => (s, resumed) = (Summary::default(), None),
        }
    }
    // The mark is the revert, not the cache's copy of it: removing the mark resumes learning (`REVERTED`).
    s.monitor.reverted = reverted;
    (s, resumed)
}

/// The log's bytes from `at`, or None when they cannot be read as text.
fn tail(log: &Path, at: u64) -> Option<String> {
    let mut f = std::fs::File::open(log).ok()?;
    f.seek(std::io::SeekFrom::Start(at)).ok()?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).ok()?;
    String::from_utf8(bytes).ok()
}

/// Writes `s` as the cache beside `lessons`: a temp file, then a rename, so a reader never sees half.
pub fn save(lessons: &Path, s: &Summary) -> std::io::Result<()> {
    let target = cache_path(lessons);
    let tmp = target.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(s).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, &target).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// The summary from the whole outcome log beside `lessons` (`borg debug select --rebuild`, a cache that
/// cannot be resumed), folded from the prior: no fit.
#[must_use]
pub fn rebuild(lessons: &Path) -> Summary {
    let text = std::fs::read_to_string(outcome::path_beside(lessons)).unwrap_or_default();
    fold(&text, Summary::default())
}

/// `s` refitted (select/policy.rs `fit`) under an engineer's `budget` (pool tokens per session), with
/// the gate's ledger replayed under the new fit and the monitor read; a revert stays.
#[must_use]
pub fn with_fit(mut s: Summary, budget: Option<f64>) -> Summary {
    s.fit = policy::fit(&s, budget);
    s.gate = policy::ledger(&s);
    let sticky = s.monitor.reverted;
    s.monitor = policy::monitor(&s);
    s.monitor.reverted |= sticky;
    s
}

/// `borg learn`: the summary beside `lessons` refitted (`with_fit`, the collective's budget from its
/// config, select.rs `budget_of`), its revert mark written when the monitor tripped, and the cache
/// saved.
pub fn refit(lessons: &Path) -> Summary {
    let s = with_fit(load(lessons), super::budget_of(lessons));
    if s.monitor.reverted {
        let _ = std::fs::write(reverted_path(lessons), format!("{}\n", serde_json::to_string(&s.monitor).unwrap_or_default()));
    }
    let _ = save(lessons, &s);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(kind: &str, session: &str, lessons: &[&str], ms: i64) -> Outcome {
        Outcome { kind: kind.into(), session: session.into(), lessons: lessons.iter().map(|l| l.to_string()).collect(), ms, ..Default::default() }
    }

    /// A graded check: `ask`ed and `free` lessons by id, the rest left for later.
    fn graded(session: &str, check: &str, lessons: &[&str], grades: &[u8], ask: &[&str], free: &[&str], ms: i64) -> Outcome {
        let owned = |v: &[&str]| v.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        Outcome {
            grades: grades.to_vec(),
            ranks: (0..lessons.len() as u16).collect(),
            applied: lessons.len() as u16,
            chosen: owned(ask),
            free: owned(free),
            channel: "live".into(),
            check: check.into(),
            policy: "learned".into(),
            p: lessons.iter().map(|l| f64::from(u8::from(ask.contains(l)))).collect(),
            pf: lessons.iter().map(|l| f64::from(u8::from(free.contains(l)))).collect(),
            shown: 20,
            tokens: 6000,
            fill: (0..lessons.len() as u16).map(|i| 3 * i).collect(),
            dur_ms: 9000,
            ..rec(outcome::GRADED, session, lessons, ms)
        }
    }

    fn verdict(kind: &str, session: &str, lesson: &str, check: &str, channel: &str, ms: i64) -> Outcome {
        Outcome { check: check.into(), channel: channel.into(), judge: 2, ..rec(kind, session, &[lesson], ms) }
    }

    fn delivery(session: &str, lesson: &str, check: &str, route: &str, ms: i64) -> Outcome {
        Outcome { check: check.into(), route: route.into(), ..rec(outcome::DELIVERY, session, &[lesson], ms) }
    }

    /// `graded` at the stop.
    fn stop(session: &str, check: &str, lessons: &[&str], grades: &[u8], ask: &[&str], free: &[&str], ms: i64) -> Outcome {
        Outcome { channel: "stop".into(), ..graded(session, check, lessons, grades, ask, free, ms) }
    }

    /// What the end hook writes of a queue line still waiting: `never_delivered`.
    fn never(session: &str, check: &str, lessons: &[&str], ms: i64) -> Outcome {
        Outcome { check: check.into(), route: outcome::ROUTE_NEVER.into(), channel: "queued".into(), ..rec(outcome::DELIVERY, session, lessons, ms) }
    }

    fn lines(log: &[Outcome]) -> String {
        log.iter().map(|o| format!("{}\n", serde_json::to_string(o).unwrap())).collect()
    }

    /// A log with every kind the fold reads, older records without check ids among them.
    fn log() -> Vec<Outcome> {
        let legacy = Outcome {
            check: String::new(),
            policy: String::new(),
            pf: Vec::new(),
            free: Vec::new(),
            ..graded("old", "", &["A", "B", "C"], &[2, 1, 1], &[], &[], 1)
        };
        let mut placebo = graded("s", "c3", &["A", "B", "C"], &[1, 1, 2], &[], &["C"], 40);
        placebo.placebo = vec!["A".into()];
        placebo.psi = 0.5;
        vec![
            legacy,
            Outcome { channel: "queued".into(), ..rec(outcome::ASKED, "old", &["A"], 2) },
            Outcome { channel: "queued".into(), ..rec(outcome::CHANGED, "old", &["A"], 3) },
            Outcome { channel: "queued".into(), ..rec(outcome::IGNORED, "old", &["B"], 4) },
            rec(outcome::NOT_CORRECTED, "old", &["C"], 5),
            graded("s", "c1", &["A", "B"], &[3, 2], &["A", "B"], &[], 10),
            delivery("s", "A", "c1", outcome::ROUTE_ASKED, 11),
            delivery("s", "B", "c1", outcome::ROUTE_DROPPED_DONE, 11),
            rec(outcome::REGRETTED, "s", &["A"], 12),
            Outcome { cause: "R1".into(), credit: 0.5, ..rec(outcome::REGRETTED, "s", &["B"], 12) },
            verdict(outcome::UNCLEAR, "s", "A", "c1", "live", 13),
            verdict(outcome::CHANGED, "s", "A", "c1", "live", 14),
            graded("s", "c2", &["D", "E"], &[1, 2], &[], &["E"], 20),
            Outcome { channel: "early".into(), ..rec(outcome::CHANGED, "s", &["Q"], 21) },
            delivery("s", "E", "c2", outcome::ROUTE_FREE, 22),
            Outcome { check: "c9".into(), channel: "live".into(), ..rec(outcome::ASKED, "s", &["D"], 30) },
            placebo,
            delivery("s", "A", "c3", outcome::ROUTE_PLACEBO, 41),
            verdict(outcome::IGNORED, "s", "A", "c3", outcome::PLACEBO, 42),
            verdict(outcome::CHANGED, "s", "C", "c3", "queued", 43),
            rec(outcome::VETOED, "s", &["C"], 44),
            Outcome { reason: outcome::UNJUDGED_TRIES.into(), check: "c2".into(), ..rec(outcome::NOT_CORRECTED, "s", &["E"], 50) },
            rec(outcome::TESTED, "s", &["A"], 51),
            Outcome { kind: "graded".into(), failed: true, tokens: 6000, dur_ms: 30_000, ..graded("s", "c4", &[], &[], &[], &[], 52) },
        ]
    }

    fn label_of(s: &Summary, check: &str, lesson: &str) -> (Route, Label, Delivered) {
        let c = s.checks.iter().find(|c| c.id == check).unwrap();
        let a = s.applies[c.first as usize..(c.first + u32::from(c.n)) as usize].iter().find(|a| s.lessons[a.lesson as usize] == lesson).unwrap();
        (a.route, a.label, a.delivered)
    }

    #[test]
    fn results_join_their_check_by_id_and_older_ones_by_the_last_check_before() {
        let s = fold(&lines(&log()), Summary::default());
        assert_eq!(s.checks.len(), 4, "the failed check has no applies");
        // The older check: no id, its results by the session's last check before them, on the route
        // their channel names.
        let old = &s.applies[..3];
        // C settled by an older binary (no reason) without a verdict: censored.
        assert_eq!(
            old.iter().map(|a| (a.route, a.label)).collect::<Vec<_>>(),
            [(Route::Free, Label::Changed), (Route::Free, Label::Ignored), (Route::Free, Label::Censored)]
        );
        assert_eq!(old[0].delivered, Delivered::Yes);
        assert_eq!(old[0].pf, 1.0, "an older record's free lesson had pf 1");
        // A verdict supersedes an unclear one; a dropped ask stays without a verdict.
        assert_eq!(label_of(&s, "c1", "A"), (Route::Ask, Label::Changed, Delivered::Yes));
        assert_eq!(label_of(&s, "c1", "B"), (Route::Ask, Label::Open, Delivered::Dropped));
        // A placebo's verdict labels the placebo, never the free lesson beside it.
        assert_eq!(label_of(&s, "c3", "A"), (Route::Placebo, Label::Ignored, Delivered::Yes));
        assert_eq!(label_of(&s, "c3", "C"), (Route::Free, Label::Changed, Delivered::Unknown));
        // Settled without a verdict: censored; D was left for later by c2 and asked after it.
        assert_eq!(label_of(&s, "c2", "E").1, Label::Censored);
        let d = s.applies.iter().find(|a| s.lessons[a.lesson as usize] == "D").unwrap();
        assert!(d.later && d.route == Route::Not);
        // c2b leaves A for later; c3 then draws A as a placebo, judged but never shown: no later delivery.
        let mut more = log();
        more.insert(16, graded("s", "c2b", &["A"], &[1], &[], &[], 35));
        let s2 = fold(&lines(&more), Summary::default());
        assert!(!s2.applies[s2.checks.iter().find(|c| c.id == "c2b").unwrap().first as usize].later, "a placebo is no delivery");
        assert_eq!(s.applies.iter().find(|a| a.route == Route::Placebo).map(|a| a.psi), Some(0.5));
        assert_eq!((s.early.judged, s.early.changed), (1, 1));
        assert!(s.applies.iter().filter(|a| a.label == Label::Changed).all(|a| a.judge.is_some()));
        assert_eq!(s.judges, ["v0||", "v2||"], "the older verdicts' judge (version 0) and the new one's, each with its check's model");
        // Signals and tallies.
        assert_eq!((s.signals[1].regrets, s.signals[1].vetoes, s.signals[1].tested), (2, 1, 1));
        assert_eq!(s.sessions.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["old", "s"]);
        assert!(s.sessions.iter().all(|x| x.ended));
        assert_eq!(s.sessions[1].tally[FREE_LIVE], (2, 1), "E delivered, C not");
        assert_eq!(s.sessions[1].tally[LATER], (2, 1), "c2's D delivered later, c3's B not");
        assert_eq!(s.pool.latency.len(), 5, "every timed call, the failed one too");
        assert_eq!(s.pool.bins[0], (4, 4), "position 0 shown by the four answered calls, applied in each");
        assert_eq!(s.pool.bins[2], (16, 6), "positions 3 to 6: fills 3 and 6 of the three-lesson checks, 3 of the two-lesson ones");
    }

    /// An early delivery, as hook.rs `record_delivery` writes it.
    fn early(session: &str, lessons: &[&str], ms: i64) -> Outcome {
        Outcome { channel: "early".into(), ..rec(outcome::ASKED, session, lessons, ms) }
    }

    /// Lines for the state round 0.6.0 added: a session shown a lesson before its first check, a
    /// check's `nofree` and tail sample, live questions' fates.
    fn more() -> Vec<Outcome> {
        let tailed = Outcome {
            nofree: vec!["N1".into()],
            tail: vec!["T1".into(), "T2".into()],
            pt: 0.5,
            fill: vec![0, 3, 25, 25, 40],
            ..graded("n", "n1", &["A", "N1", "T1"], &[3, 1, 2], &["A"], &["T1"], 61)
        };
        vec![
            early("n", &["N1"], 60),
            tailed,
            delivery("n", "A", "n1", outcome::ROUTE_ASKED, 62),
            delivery("n", "T1", "n1", outcome::ROUTE_FREE, 62),
            rec(outcome::ENDED, "n", &[], 63),
        ]
    }

    #[test]
    fn a_fold_of_the_whole_equals_its_folds_at_every_line_boundary() {
        let text = lines(&[log(), more()].concat());
        // With a fit, so the gate's ledger folds too.
        let mut prior = Summary::default();
        let fitted = with_fit(fold(&text, Summary::default()), None);
        prior.fit = fitted.fit.clone();
        assert!(prior.fit.model.is_some(), "the log has labels");
        let whole = fold(&text, prior.clone());
        assert!(whole.gate.checks > 0, "learned checks reached the ledger");
        let mut at = 0;
        let mut one = prior.clone();
        for line in text.split_inclusive('\n') {
            let split = fold(&text[at + line.len()..], fold(&text[..at + line.len()], prior.clone()));
            assert_eq!(split, whole, "split after byte {}", at + line.len());
            one = fold(line, one);
            at += line.len();
        }
        assert_eq!(one, whole, "line by line");
        assert_eq!(whole.at, text.len() as u64);
        // A refit replays the same ledger the folds built.
        let refit = with_fit(whole.clone(), None);
        assert_eq!(refit.fit.key, whole.key());
    }

    #[test]
    fn a_line_being_written_waits_for_its_newline() {
        let text = lines(&log());
        let cut = text.len() - 10;
        let s = fold(&text[..cut], Summary::default());
        let last_newline = text[..cut].rfind('\n').unwrap() + 1;
        assert_eq!(s.at, last_newline as u64);
        assert_eq!(fold(&text[s.at as usize..], s), fold(&text, Summary::default()));
    }

    #[test]
    fn kinds_it_does_not_use_cost_a_substring() {
        // A regret counts without serde (this one would not parse); an unknown kind is never read.
        let text =
            [r#"{"kind":"regretted",broken"#, r#"{"kind":"something_new","session":"z"}"#, r#"{"kind":"graded","session":"s","lessons":[],"ms":1,"repo":""}"#]
                .map(|l| format!("{l}\n"))
                .concat();
        let text = text.as_str();
        let s = fold(text, Summary::default());
        assert_eq!(s.signals[0].regrets, 1);
        assert_eq!(s.sessions.len(), 1, "only the graded line registered a session");
        assert_eq!(kind_of("{\"kind\":\"asked\",\"x\":1}"), Some("asked"));
        assert_eq!(kind_of(" {\"kind\":\"asked\"}"), None);
    }

    /// A temp store: its lessons file (never written) and the outcomes beside it.
    fn store(name: &str, text: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-summary-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("outcomes.jsonl"), text).unwrap();
        d.join("lessons.jsonl")
    }

    #[test]
    fn load_resumes_the_cache_and_rebuilds_what_it_cannot() {
        let text = lines(&log());
        let half = text[..text.len() / 2].rfind('\n').unwrap() + 1;
        let lessons = store("load", &text[..half]);
        let whole = fold(&text, Summary::default());
        // Saved at half the log, then the rest appended: loading folds only the tail.
        save(&lessons, &load(&lessons)).unwrap();
        std::fs::write(outcome::path_beside(&lessons), &text).unwrap();
        assert_eq!(load(&lessons), whole);
        assert_eq!(peek(&lessons), whole);
        // A corrupt cache: load rebuilds, peek takes the prior.
        std::fs::write(cache_path(&lessons), "{\"sv\":1,\"at\":").unwrap();
        assert_eq!(load(&lessons), whole);
        assert_eq!(peek(&lessons), Summary::default());
        // Another format, and a log shorter than the cache read (rewritten): rebuilt.
        let mut other = whole.clone();
        other.sv = SV + 1;
        save(&lessons, &other).unwrap();
        assert_eq!(load(&lessons), whole);
        let mut ahead = whole.clone();
        ahead.at += 1_000;
        save(&lessons, &ahead).unwrap();
        assert_eq!(load(&lessons), whole);
        // A cache that parses but points outside its tables is corrupt too: never indexed in a hook.
        let mut broken = whole.clone();
        broken.checks[1].first += 1;
        save(&lessons, &broken).unwrap();
        assert_eq!((load(&lessons), peek(&lessons)), (whole.clone(), Summary::default()));
        let mut broken = whole.clone();
        broken.applies[0].lesson = 99;
        save(&lessons, &broken).unwrap();
        assert_eq!(load(&lessons), whole);
        let mut broken = whole.clone();
        broken.sessions[1].had.push(99);
        save(&lessons, &broken).unwrap();
        assert_eq!(load(&lessons), whole, "what a session had points outside the lessons");
        assert!(whole.consistent() && Summary::default().consistent());
        // A refit summary, its monitor's bound unset before enough sessions, loads as saved (bit for
        // bit only as serde_json parses floats: without its `float_roundtrip` a few land 1 ULP off).
        let fitted = with_fit(whole.clone(), None);
        assert!(fitted.fit.model.is_some() && fitted.monitor.ucb.is_none());
        save(&lessons, &fitted).unwrap();
        let parsed: Summary = serde_json::from_slice(&serde_json::to_vec(&fitted).unwrap()).unwrap();
        assert_eq!(load(&lessons), parsed);
        // The revert mark is sticky: read on every load.
        std::fs::write(reverted_path(&lessons), "{}\n").unwrap();
        assert!(load(&lessons).monitor.reverted && peek(&lessons).monitor.reverted);
        // The mark is the revert, not the cache's copy of it (`refit` saves both): removing the mark
        // resumes learning.
        save(&lessons, &Summary { monitor: Monitor { reverted: true, ..fitted.monitor.clone() }, ..fitted.clone() }).unwrap();
        assert!(load(&lessons).monitor.reverted);
        std::fs::remove_file(reverted_path(&lessons)).unwrap();
        assert!(!load(&lessons).monitor.reverted && !peek(&lessons).monitor.reverted);
        // `refresh` saves only what it folded past the cache.
        std::fs::write(outcome::path_beside(&lessons), &text[..half]).unwrap();
        std::fs::remove_file(cache_path(&lessons)).unwrap();
        assert_eq!(refresh(&lessons).at, half as u64, "no cache: rebuilt and saved");
        // The same summary written pretty: a rewrite would make it compact again.
        let pretty = serde_json::to_vec_pretty(&peek(&lessons)).unwrap();
        std::fs::write(cache_path(&lessons), &pretty).unwrap();
        assert_eq!(refresh(&lessons).at, half as u64);
        assert_eq!(std::fs::read(cache_path(&lessons)).unwrap(), pretty, "nothing new: not rewritten");
        std::fs::write(outcome::path_beside(&lessons), &text).unwrap();
        assert_eq!(refresh(&lessons), whole);
        assert_eq!(peek(&lessons), whole, "the tail it folded is saved");
        // No store at all: the prior, no panic.
        assert_eq!(load(Path::new("/nonexistent/borg-summary/lessons.jsonl")), Summary::default());
        let _ = std::fs::remove_dir_all(lessons.parent().unwrap());
    }

    #[test]
    fn compaction_keeps_the_newest_checks_and_their_joins() {
        let text = lines(&log());
        let mut s = fold(&text, Summary::default());
        let kept = s.checks.len() - 1;
        let mut ix = Index::of(&s);
        let first = s.checks[1].first as u64;
        s.compact(&mut ix, 1, 0);
        assert_eq!((s.checks.len(), s.dropped), (kept, first));
        assert_eq!(s.checks[0].first, 0);
        assert!(s.applies.iter().all(|a| (a.check as usize) < s.checks.len()));
        // A later verdict still finds its check.
        let more = lines(&[graded("s", "c5", &["Z"], &[2], &[], &["Z"], 60), verdict(outcome::CHANGED, "s", "Z", "c5", "queued", 61)]);
        let s = fold(&more, s);
        assert_eq!(label_of(&s, "c5", "Z").1, Label::Changed);
    }

    #[test]
    fn every_ended_session_counts_its_deliveries_and_a_resumed_one_moves_them() {
        let ended = |session: &str, ms| rec(outcome::ENDED, session, &[], ms);
        let log = [
            // A stop asks nothing and hands A and B over free; the session ends before a next prompt.
            stop("a", "a1", &["A", "B"], &[2, 1], &[], &["A", "B"], 1),
            never("a", "a1", &["A", "B"], 2),
            ended("a", 3),
            // Its one question corrected, a session leaves no settle line: its end alone counts it.
            graded("b", "b1", &["C", "D"], &[3, 1], &["C"], &["D"], 10),
            delivery("b", "C", "b1", outcome::ROUTE_ASKED, 11),
            delivery("b", "D", "b1", outcome::ROUTE_FREE, 11),
            rec(outcome::CORRECTED, "b", &["C"], 12),
            ended("b", 13),
            // An end hook from before `ended`: its `never_delivered` deliveries end the session.
            stop("c", "c1", &["E"], &[2], &[], &["E"], 20),
            never("c", "c1", &["E"], 21),
            // Still open: nothing counts yet.
            stop("d", "d1", &["F"], &[2], &[], &["F"], 30),
            // A session without a graded check has nothing to count: no entry.
            ended("e", 31),
        ];
        let s = fold(&lines(&log), Summary::default());
        let of = |s: &Summary, id: &str| s.sessions.iter().find(|x| x.id == id).cloned().unwrap();
        assert_eq!(of(&s, "a").tally[FREE_STOP], (2, 0), "no next prompt came");
        assert_eq!((of(&s, "b").tally[FREE_LIVE], of(&s, "b").tally[WAIT]), ((1, 1), (1, 0)), "D delivered; C asked during the turn");
        assert_eq!(of(&s, "c").tally[FREE_STOP], (1, 0));
        assert_eq!(of(&s, "d").tally, [(0, 0); TALLIES]);
        assert_eq!(s.sessions.iter().map(|x| (x.id.as_str(), x.ended)).collect::<Vec<_>>(), [("a", true), ("b", true), ("c", true), ("d", false)]);
        // Resumed, `a` hands A over at its next prompt after all: the count follows the latest record.
        let resumed = fold(&lines(&[delivery("a", "A", "a1", outcome::ROUTE_FREE, 40)]), s);
        assert_eq!(of(&resumed, "a").tally[FREE_STOP], (2, 1));
    }

    #[test]
    fn left_for_later_is_a_live_checks_rate() {
        let log = [
            // The stop leaves L and M for later and the next turn's live check asks L: not what ρ is,
            // a lesson left for later during the turn delivered later.
            stop("s", "k1", &["L", "M"], &[1, 1], &[], &[], 1),
            graded("s", "k2", &["L"], &[3], &["L"], &[], 5),
            delivery("s", "L", "k2", outcome::ROUTE_ASKED, 6),
            // A live check leaves N for later, and its turn's stop asks it.
            graded("s", "k3", &["N"], &[1], &[], &[], 7),
            stop("s", "k4", &["N"], &[3], &["N"], &[], 8),
            Outcome { channel: "stop".into(), ..delivery("s", "N", "k4", outcome::ROUTE_ASKED, 9) },
            rec(outcome::ENDED, "s", &[], 10),
        ];
        let s = fold(&lines(&log), Summary::default());
        assert_eq!(s.sessions[0].tally[LATER], (1, 1), "k3's N alone");
        // The stop's applies still say what came of them.
        let k1 = s.checks[0].first as usize;
        assert!(s.applies[k1].later && !s.applies[k1 + 1].later, "L arrived later, M never");
    }

    #[test]
    fn a_lesson_its_session_had_counts_in_no_delivery_tally() {
        let ended = |session: &str, ms| rec(outcome::ENDED, session, &[], ms);
        let log = [
            // Delivered early before the session's first check; an older binary then dropped E's free
            // hand-over without a record: not a lesson that failed to arrive.
            early("a", &["E"], 1),
            stop("a", "a1", &["E", "F"], &[2, 1], &[], &["E", "F"], 2),
            Outcome { channel: "queued".into(), ..rec(outcome::ASKED, "a", &["F"], 3) },
            ended("a", 4),
            // Handed over at the session's first check, free again at its next: dropped as given before.
            graded("b", "b1", &["G"], &[2], &[], &["G"], 10),
            delivery("b", "G", "b1", outcome::ROUTE_FREE, 11),
            graded("b", "b2", &["G", "H"], &[2, 1], &[], &["G", "H"], 12),
            Outcome { reason: "given before".into(), ..delivery("b", "G", "b2", outcome::ROUTE_DROPPED_DONE, 13) },
            delivery("b", "H", "b2", outcome::ROUTE_FREE, 13),
            ended("b", 14),
            // The hook's state had K, the log no delivery of it (`nofree`): left for later, only M counts.
            Outcome { nofree: vec!["K".into()], ..graded("c", "c1", &["K", "M"], &[2, 1], &[], &[], 20) },
            ended("c", 21),
        ];
        let s = fold(&lines(&log), Summary::default());
        let of = |id: &str| s.sessions.iter().find(|x| x.id == id).cloned().unwrap();
        assert_eq!(of("a").tally[FREE_STOP], (1, 1), "F alone: the session had E");
        assert_eq!(of("b").tally[FREE_LIVE], (2, 2), "b1's G and b2's H: at b2 the session had G");
        assert_eq!(of("c").tally[LATER], (1, 0), "M alone");
        let a1 = s.checks.iter().find(|c| c.id == "a1").unwrap().first as usize;
        assert!(s.applies[a1].had && !s.applies[a1 + 1].had);
        // What a session was shown before its first check is its own from that check on.
        assert!(s.fresh.is_empty());
        let names = |x: &Session| x.had.iter().map(|&l| s.lessons[l as usize].clone()).collect::<Vec<_>>();
        assert_eq!((names(&of("a")), names(&of("b")), names(&of("c"))), (vec!["E".into(), "F".into()], vec!["G".into(), "H".into()], vec![]));
    }

    #[test]
    fn a_live_question_is_delivered_when_it_reaches_the_drone() {
        let at_stop = |o: Outcome| Outcome { channel: "stop".into(), ..o };
        let log = [
            // Q4 and Q6 were handed over early. Q4, asked again and found done at the stop, counts
            // nowhere; Q6, not acted on, reaches the drone and is judged as any question: its verdict
            // stays on its apply (top.rs label health), its delivery out of the tallies (`Apply::had`).
            early("q", &["Q4", "Q6"], 1),
            graded("q", "q1", &["Q1", "Q2", "Q3", "Q4", "Q6"], &[3, 3, 3, 3, 3], &["Q1", "Q2", "Q3", "Q4", "Q6"], &[], 2),
            delivery("q", "Q1", "q1", outcome::ROUTE_ASKED, 3),
            delivery("q", "Q6", "q1", outcome::ROUTE_ASKED, 3),
            verdict(outcome::CHANGED, "q", "Q6", "q1", "live", 4),
            // Q2 waited for the stop and was asked in its block; Q3 was not deliverable there.
            at_stop(delivery("q", "Q2", "q1", outcome::ROUTE_ASKED, 5)),
            Outcome { reason: "not deliverable".into(), ..delivery("q", "Q3", "q1", outcome::ROUTE_DROPPED_DONE, 5) },
            Outcome { reason: "done at the stop".into(), ..delivery("q", "Q4", "q1", outcome::ROUTE_DROPPED_DONE, 5) },
            // A stop's question blocks: certain, never tallied.
            stop("q", "q2", &["Q5"], &[3], &["Q5"], &[], 6),
            at_stop(delivery("q", "Q5", "q2", outcome::ROUTE_ASKED, 7)),
            rec(outcome::ENDED, "q", &[], 8),
        ];
        let s = fold(&lines(&log), Summary::default());
        assert_eq!((s.sessions[0].tally[ASK_LIVE], s.sessions[0].tally[WAIT]), ((3, 2), (3, 1)));
        let q6 = s.checks[0].first as usize + 4;
        assert_eq!((label_of(&s, "q1", "Q6"), s.applies[q6].had), ((Route::Ask, Label::Changed, Delivered::Yes), true));
        // Learned as the free rates are, Beta(1, 1): (1 + 2) / (2 + 3).
        assert!((policy::fit(&s, None).rates.ask_live - 0.6).abs() < 1e-12);
        // Before a live question is tallied, the fixed rule's certainty.
        assert_eq!((policy::fit(&Summary::default(), None).rates.ask_live, policy::prior().rates.ask_live), (1.0, 1.0));
        // Resumed, the session asks Q3 after all: both its counts move.
        let s = fold(&lines(&[at_stop(delivery("q", "Q3", "q1", outcome::ROUTE_ASKED, 9))]), s);
        assert_eq!((s.sessions[0].tally[ASK_LIVE], s.sessions[0].tally[WAIT]), ((3, 3), (3, 2)));
    }

    #[test]
    fn a_hand_over_the_stop_asked_in_place_is_its_free_applys_delivery_and_verdict() {
        // What hook.rs `stop_ask` writes for a live check's question-count test draw: H handed over in
        // place of a question, and the stop came before any delivery took it, so it asked H in its block
        // with A, the check's waiting question.
        let at_stop = |o: Outcome| Outcome { channel: "stop".into(), ..o };
        let log = [
            graded("s", "k1", &["A", "H"], &[3, 2], &["A"], &["H"], 1),
            stop("s", "k2", &[], &[], &[], &[], 2),
            at_stop(Outcome { check: "k1".into(), ..rec(outcome::ASKED, "s", &["A"], 3) }),
            Outcome { reason: "at the stop".into(), ..at_stop(delivery("s", "A", "k1", outcome::ROUTE_ASKED, 3)) },
            at_stop(Outcome { check: "k1".into(), ..rec(outcome::ASKED, "s", &["H"], 3) }),
            Outcome { reason: "in place of a hand-over".into(), ..at_stop(delivery("s", "H", "k1", outcome::ROUTE_ASKED, 3)) },
            // `#borg-routed H=k1`: the verdict names H's check, on channel stop.
            verdict(outcome::CHANGED, "s", "H", "k1", "stop", 4),
            verdict(outcome::IGNORED, "s", "A", "k1", "stop", 4),
            Outcome { reason: outcome::UNJUDGED_NONE.into(), check: "k1".into(), ..rec(outcome::NOT_CORRECTED, "s", &["A", "H"], 5) },
            rec(outcome::ENDED, "s", &[], 6),
        ];
        let s = fold(&lines(&log), Summary::default());
        assert_eq!(label_of(&s, "k1", "H"), (Route::Free, Label::Changed, Delivered::Yes), "the draw's free apply, delivered and judged");
        assert_eq!(label_of(&s, "k1", "A"), (Route::Ask, Label::Ignored, Delivered::Yes));
        assert_eq!(s.sessions[0].tally[FREE_LIVE], (1, 1), "H arrived");
        assert_eq!((s.sessions[0].tally[ASK_LIVE], s.sessions[0].tally[WAIT]), ((1, 1), (1, 1)));
        // A stop check's own result stays on its questions.
        let log = [stop("t", "t1", &["F", "G"], &[3, 2], &["F"], &["G"], 1), verdict(outcome::CHANGED, "t", "G", "t1", "stop", 2)];
        assert_eq!(label_of(&fold(&lines(&log), Summary::default()), "t1", "G").1, Label::Open, "a stop's free lesson is judged on channel queued");
    }

    #[test]
    fn a_live_question_of_a_check_without_a_3_is_no_question_that_waits_for_the_stop() {
        let log = [
            // A check with a 3: its question waits for the stop and is asked there.
            graded("t", "t1", &["A"], &[3], &["A"], &[], 1),
            Outcome { channel: "stop".into(), ..delivery("t", "A", "t1", outcome::ROUTE_ASKED, 2) },
            // Without a 3 (`live-next`): no stop asks B, and the session ends before a tool call.
            graded("t", "t2", &["B", "C"], &[2, 1], &["B"], &[], 3),
            Outcome { channel: "queued".into(), ..never("t", "t2", &["B"], 4) },
            rec(outcome::ENDED, "t", &[], 5),
        ];
        let s = fold(&lines(&log), Summary::default());
        assert_eq!((s.sessions[0].tally[ASK_LIVE], s.sessions[0].tally[WAIT]), ((1, 1), (1, 1)), "t1's A alone");
        assert_eq!(label_of(&s, "t2", "B"), (Route::Ask, Label::Open, Delivered::Dropped), "its apply still says what came of it");
        // Resumed, a tool call asks B after all: no tally moves.
        let s = fold(&lines(&[delivery("t", "B", "t2", outcome::ROUTE_ASKED, 6)]), s);
        assert_eq!((s.sessions[0].tally[ASK_LIVE], s.sessions[0].tally[WAIT]), ((1, 1), (1, 1)));
    }

    #[test]
    fn a_tail_sample_teaches_the_yield_past_the_pool() {
        // A pool of four (positions 0 to 3) and a tail of two the budget dropped, at positions 9 and 20 of
        // the whole fill order; T1 applied.
        let tailed = |pt: f64, at: &[u16]| Outcome {
            shown: 4,
            tail: vec!["T1".into(), "T2".into()],
            pt,
            fill: [&[0, 9][..], at].concat(),
            ..graded("s", "t1", &["A", "T1"], &[2, 1], &[], &["A", "T1"], 1)
        };
        let s = fold(&lines(&[tailed(0.25, &[9, 20])]), Summary::default());
        // Positions 0, 1–2, 3–6 as the pool showed them; 7–14 and 15–30 each a tail lesson.
        assert_eq!(s.pool.bins, [(1, 1), (2, 0), (1, 0), (1, 1), (1, 0)]);
        assert_eq!((s.pool.shown, s.pool.tokens), (4, 6000), "the pool's own: the tail is extra");
        assert_eq!(s.pool.tail, (2, 1), "the tail's lessons shown and applied, as borg top counts them");
        // No chance or one past 1 (no uniform sample), positions missing or inside the pool: the pool alone.
        for o in [tailed(0.0, &[9, 20]), tailed(1.5, &[9, 20]), tailed(0.25, &[9]), tailed(0.25, &[2, 20])] {
            let s = fold(&lines(&[o]), Summary::default());
            assert_eq!((s.pool.bins, s.pool.tail), (vec![(1, 1), (2, 0), (1, 0)], (0, 0)));
        }
    }

    #[test]
    fn sessions_shown_lessons_before_a_check_stay_bounded() {
        let n = MAX_FRESH + 10;
        let text = lines(&(0..n).map(|t| early(&format!("e{t}"), &["L"], t as i64)).collect::<Vec<_>>());
        let s = fold(&text, Summary::default());
        // The guard: past `MAX_FRESH` the oldest went, the newest stayed.
        assert!(s.fresh.len() <= MAX_FRESH, "{} kept", s.fresh.len());
        let newest = format!("e{}", n - 1);
        assert!(!s.fresh.contains_key("e0") && s.fresh.contains_key(&newest));
        assert!(s.sessions.is_empty() && s.consistent());
        for at in [text.len() / 2, text.len() - 100] {
            let at = text[..at].rfind('\n').unwrap() + 1;
            assert_eq!(fold(&text[at..], fold(&text[..at], Summary::default())), s, "split after byte {at}");
        }
        // The newest graded after all: its free hand-over of what it had counts nowhere.
        let s = fold(&lines(&[stop(&newest, "x1", &["L"], &[2], &[], &["L"], 9_999), rec(outcome::ENDED, &newest, &[], 10_000)]), s);
        assert_eq!(s.sessions[0].tally[FREE_STOP], (0, 0));
    }

    #[test]
    fn a_regrets_missed_lesson_is_no_correction() {
        let log = [
            rec(outcome::MISSED, "s", &["A"], 1),
            Outcome { cause: "R1".into(), ..rec(outcome::MISSED, "s", &["B"], 2) },
            rec(outcome::CORRECTED, "s", &["C"], 3),
        ];
        assert_eq!(fold(&lines(&log), Summary::default()).signals[0].corrections, 2, "A's and C's: B's is the regret R1 again");
    }

    #[test]
    fn the_monitors_weights_persist_through_save_and_fold() {
        let text = lines(&log());
        let half = text[..text.len() / 2].rfind('\n').unwrap() + 1;
        let lessons = store("monitor", &text[..half]);
        let s = Summary { monitor: Monitor { ess: 12.5, max_weight: 3.25, labelled: 7, ..Monitor::default() }, ..load(&lessons) };
        save(&lessons, &s).unwrap();
        std::fs::write(outcome::path_beside(&lessons), &text).unwrap();
        let folded = refresh(&lessons);
        assert_eq!(folded.at, text.len() as u64, "the tail folded");
        for x in [&folded, &peek(&lessons)] {
            assert_eq!((x.monitor.ess, x.monitor.max_weight, x.monitor.labelled), (12.5, 3.25, 7));
        }
        let _ = std::fs::remove_dir_all(lessons.parent().unwrap());
    }

    /// `n` sessions, each shown a lesson early that no check grades, then one live check handing its
    /// own lesson over free (delivered in the even sessions), then its end.
    fn many(n: usize) -> String {
        let mut log = Vec::new();
        for t in 0..n {
            let (id, check, lesson, ms) = (format!("s{t}"), format!("c{t}"), format!("L{t}"), 10 * t as i64);
            log.push(early(&id, &[&format!("X{t}")], ms));
            log.push(graded(&id, &check, &[&lesson], &[2], &[], &[&lesson], ms));
            if t % 2 == 0 {
                log.push(delivery(&id, &lesson, &check, outcome::ROUTE_FREE, ms + 1));
            }
            log.push(rec(outcome::ENDED, &id, &[], ms + 2));
        }
        lines(&log)
    }

    #[test]
    fn the_session_table_stays_bounded_and_keeps_what_its_readers_need() {
        let n = MAX_SESSIONS + 2;
        let text = many(n);
        let s = fold(&text, Summary::default());
        // The guard: past `MAX_SESSIONS` the oldest went, an even count, never the newest.
        assert!(s.sessions.len() <= MAX_SESSIONS, "{} sessions kept", s.sessions.len());
        assert_eq!((n - s.sessions.len()) % 2, 0, "the monitor's halves keep their sessions");
        assert_eq!(s.sessions.last().map(|x| x.id.clone()), Some(format!("s{}", n - 1)));
        assert!(s.consistent());
        // Their checks went with them, and every lesson only they named; a kept session keeps what it
        // was shown, each lesson by its name.
        assert_eq!((s.checks.len(), s.dropped), (s.sessions.len(), (n - s.sessions.len()) as u64));
        assert_eq!(s.lessons.len(), 2 * s.applies.len());
        let newest = s.sessions.last().unwrap();
        let shown: Vec<&str> = newest.had.iter().map(|&l| s.lessons[l as usize].as_str()).collect();
        assert_eq!(shown, [format!("X{}", n - 1)], "an odd session: its free lesson never came");
        let even = &s.sessions[s.sessions.len() - 2];
        let shown: Vec<&str> = even.had.iter().map(|&l| s.lessons[l as usize].as_str()).collect();
        assert_eq!(shown, [format!("X{}", n - 2), format!("L{}", n - 2)]);
        // Every age as before: under every half-life the tallies are all n sessions', and so are the
        // sessions and checks spend divides by.
        for h in policy::HALF_LIVES {
            let want = (0..n).fold((0.0, 0.0), |(k, miss), t| {
                let w = policy::decay((n - t) as u32, h);
                if t % 2 == 0 { (k + w, miss) } else { (k, miss + w) }
            });
            let got = s.tallies(h)[FREE_LIVE];
            let close = |a: f64, b: f64| (a - b).abs() <= 1e-9 * b.max(1.0);
            assert!(close(got.0, want.0) && close(got.1, want.1), "{h:?}: {got:?} against {want:?}");
        }
        assert_eq!(s.checked(), (n as u64, n as u64));
        // A fold split before the compaction, or after it, is the whole fold.
        for at in [text.len() / 3, text.len() - 200] {
            let at = text[..at].rfind('\n').unwrap() + 1;
            assert_eq!(fold(&text[at..], fold(&text[..at], Summary::default())), s, "split after byte {at}");
        }
    }

    /// The question-count test's checks (module doc): a drawn count, played or refused, counts by its k
    /// and s and marks its check, its control arm apart, its session once; a refusal also counts by its
    /// k; a record whose k or s cannot be its own (a refusal without its s among them), one outside the
    /// test and one not of the learned policy count nowhere. The
    /// counts are all-time: compaction drops the checks, never them.
    #[test]
    fn the_question_count_tests_checks_count_by_k_and_s_and_mark_their_check() {
        let x = |session: &str, check: &str, source: &str, k: Option<u32>, s: Option<u32>, ms| Outcome {
            source: source.into(),
            xk: k,
            xs: s,
            ..graded(session, check, &["A", "B", "C"], &[2, 2, 1], &["A"], &["B", "C"], ms)
        };
        let text = lines(&[
            x("x", "e1", policy::EXPERIMENT, Some(3), Some(1), 10),
            x("x", "e2", policy::EXPERIMENT, Some(3), Some(1), 20),
            x("x", "e3", policy::EXPERIMENT_CONTROL, Some(3), Some(0), 30),
            // No s: no source of the test writes it.
            x("x", "g1", policy::GATE, Some(3), None, 40),
            // Refused after its draw: randomised, the fixed rule played.
            x("x", "g2", policy::GATE, Some(3), Some(2), 45),
            // s past k, k past the check's applies.
            x("x", "bad", policy::EXPERIMENT, Some(3), Some(4), 50),
            x("x", "big", policy::EXPERIMENT, Some(9), Some(1), 60),
            x("x", "ev", policy::EVIDENCE, None, None, 70),
            Outcome { policy: "fixed".into(), ..x("x", "f1", policy::EXPERIMENT, Some(3), Some(2), 80) },
            x("y", "y1", policy::EXPERIMENT, Some(2), Some(2), 90),
        ]);
        let s = fold(&text, Summary::default());
        let e = &s.experiment;
        assert_eq!(
            (e.ran.as_slice(), e.control, e.refused.as_slice(), e.sessions),
            (&[(2, 2, 1), (3, 0, 1), (3, 1, 2), (3, 2, 1)][..], 1, &[(3, 1)][..], 2),
            "{e:?}"
        );
        assert_eq!(e.totals(), (5, 1));
        let marked: Vec<(&str, Option<u32>, Option<u32>)> = s.checks.iter().map(|c| (c.id.as_str(), c.xk, c.xs)).filter(|c| c.1.is_some()).collect();
        assert_eq!(marked, [("e1", Some(3), Some(1)), ("e2", Some(3), Some(1)), ("e3", Some(3), Some(0)), ("g2", Some(3), Some(2)), ("y1", Some(2), Some(2))]);
        assert!(s.sessions.iter().all(|x| x.tested));
        // The fold every count shares: a prefix, then the rest, is the whole.
        let at = text[..text.len() / 2].rfind('\n').unwrap() + 1;
        assert_eq!(fold(&text[at..], fold(&text[..at], Summary::default())), s);
        // Compaction takes the checks, not the counts.
        let mut compacted = s.clone();
        let mut ix = Index::of(&compacted);
        compacted.compact(&mut ix, s.applies.len(), 0);
        assert_eq!((compacted.checks.len(), &compacted.experiment), (0, &s.experiment));
    }

    #[test]
    fn a_learned_decision_the_fixed_rule_took_over_counts_by_its_reason() {
        let fell = |check: &str, reason: &str, applied: &[&str], ms| Outcome {
            policy: "fixed".into(),
            reason: reason.into(),
            ..graded("f", check, applied, &vec![1; applied.len()], &[], applied, ms)
        };
        let text = lines(&[
            fell("f1", outcome::DECIDED_BUSY, &["A"], 1),
            fell("f2", outcome::DECIDED_BUSY, &["B"], 2),
            // No applies: the check still waited for the lock.
            fell("f3", outcome::DECIDED_NO_LOCK, &[], 3),
            fell("f4", outcome::DECIDED_NO_RECORD, &["A"], 4),
            // Decided by the policy in force, and a reason this binary does not know: no fallback.
            graded("f", "f5", &["A"], &[1], &[], &["A"], 5),
            fell("f6", "something_new", &["A"], 6),
        ]);
        let s = fold(&text, Summary::default());
        assert_eq!(s.fell_back, [2, 1, 1], "lock_busy, lock_failed, unrecorded");
        assert_eq!(s.checked().1, 6, "the graded checks a fallback is one of");
        // The fold every count shares: a prefix, then the rest, is the whole.
        let at = text[..text.len() / 2].rfind('\n').unwrap() + 1;
        assert_eq!(fold(&text[at..], fold(&text[..at], Summary::default())), s);
        // Kept by the cache, and a cache of the layout before them (protocol: version 5) is rebuilt,
        // never resumed without them.
        let lessons = store("fell-back", &text);
        save(&lessons, &s).unwrap();
        assert_eq!(peek(&lessons).fell_back, s.fell_back);
        save(&lessons, &Summary { sv: 5, fell_back: Default::default(), ..s.clone() }).unwrap();
        assert_eq!(load(&lessons), s);
        let _ = std::fs::remove_dir_all(lessons.parent().unwrap());
    }

    #[test]
    fn the_prior_writes_the_short_line() {
        let prior = Summary::default();
        assert_eq!((prior.sv, prior.at, &prior.fit), (SV, 0, &Fit::default()));
        let line = serde_json::to_string(&prior).unwrap();
        assert_eq!(line, format!(r#"{{"sv":{SV},"at":0,"fit":{{}}}}"#));
        assert_eq!(serde_json::from_str::<Summary>(&line).unwrap(), prior);
        let none = Path::new("/nonexistent/borg-summary/lessons.jsonl");
        assert_eq!(load(none), prior);
        assert_eq!(rebuild(none), prior);
    }
}
