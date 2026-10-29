//! `borg debug select [--replay] [--rebuild] [--dir DIR | --file FILE] [--json] [--eval RUNS_DIR --qrels
//! FILE]`: the selection policy's state and its pre-registered checks, replayed from the outcome log (0
//! tokens). Reports, never applies.
//!
//! **The rules.** docs/evidence.md "Lesson selection" (`RULES_AT`): the pre-registered checks P0–P5,
//! dated `RULES`; a later change is a dated amendment, and `AMENDMENT` names the latest one this file
//! implements. Every report prints both.
//!
//! **Verdicts.** PASS or FAIL against the bar; NOT RUN when there is nothing to run it on (no
//! evaluation given, or one still incomplete); NOT TESTED when it ran on data that could not have
//! failed it (no check that applies a lesson, since a check none applied to and a failed call decide
//! nothing under any policy; no deviation from the fixed rule; an arm that is the fixed rule
//! everywhere; no label to learn from; every prediction 1/2), never a pass. The command exits 1 when a
//! decisive check (P0–P4) fails or is NOT TESTED (`unmet`); P5 only reports.
//!
//! **Two passes over the log, in file order** (the order the hook appended it), each folding it into the
//! selection summary with select/summary.rs `fold`, so every decision sees only the lines before it,
//! and each deciding a check under the lessons its session already had, as its record logs them
//! (outcome.rs `nofree`, `had_of`):
//! - *as logged*: the log folded as it is. At each graded check the replay re-runs the production
//!   decision (select.rs `graded_under`) on the summary the hook had: the one its record names
//!   (outcome.rs `seen`, read before the model call), else every line before the record. P0: with the
//!   gate closed the learned policy decides as the fixed rule (conscience.rs `graded_without`, no cap),
//!   which is also the logged decision of every check logged under the fixed rule. P1: the logged
//!   policy, fit key, summary, revert (outcome.rs `source`), session, channel and check id (its seed)
//!   give the logged decision, its source and propensities within `ope::AGREE`, and folding piece by
//!   piece equals folding the whole log.
//! - *cold start*: the same log, the learned policy deciding from the prior on, each graded record
//!   replaced by the replayed decision before it is folded (`decided`), so the gate's state is the
//!   policy's own, and the summary refitted where production refits it (`borg learn` when a session
//!   ends), so the policy has a model to deviate by. P3: no decision risks more than the gate's budget,
//!   the first one is the fixed rule's, and with an engineer's budget for the collective the policy's
//!   pools stay within it; spend is reported.
//!
//! **Labels** (P2, P5, the readout): a judge's verdict (`asked_then_changed`, `asked_ignored`) joined to
//! the graded check that routed its delivery: by check id, or for older records the session's last
//! graded check before it that routed the lesson that way (asked for a stop or live verdict, free for a
//! queued one, placebo for a placebo one). A verdict on an early delivery has no check. A unit is 1 when
//! its strongest result is the change (`ope::rank`: a later veto or correction in the session outranks
//! it), 0 otherwise; `asked_not_corrected` is censored, never a 0. What a check could know of a unit is
//! its value then (`Label::known`): a veto or a stronger verdict after the check is a result from after
//! it.
//! - P2 predicts each unit from the labels known before its check (leave-future-out), by the value model
//!   (select/policy.rs `fit` on exactly those labels at their values then, `predict`; the n = 0 prior's
//!   before any, `prior`, or the pruned model's `prior_pruned` in the rerun) and by grade alone
//!   (Beta(1, 1) per grade), and compares their log-loss. After a FAIL, the pre-registered rerun once by
//!   the model pruned to grade, route and dilution (`fit_pruned`), whose verdict is P2's.
//! - P5 estimates each policy's change against the fixed rule by doubly robust and inverse propensity
//!   scoring with the logged propensities (a placebo's times its draw's chance ψ: routed nowhere, a
//!   unit is judged only when drawn), a Beta(1, 1) per grade and route (leave-future-out) as the
//!   outcome model; non-decisive.
//!
//! **P4** reads an evaluation's graded lines (`#borg-graded` in each run's state files) and the case's
//! relevant lessons (qrels), and compares the right-lesson recall of the fixed rule with the learned
//! policy's, ungated, expected over `conscience::P_DRAWS` seeded draws, under the n = 0 prior's model
//! (select/policy.rs `prior`) and under the store's posterior (the whole log fitted as `borg learn`
//! fits it). Neither arm is the fixed rule unless the model says so, and an arm that is (no draw off it
//! at any check) is NOT TESTED.
//!
//! **Where it meets the policy core.** Every decision is select.rs `graded_under` with the full `Ctx`
//! (session, check id, channel, lessons file, summary, logged pool tokens, had), the learned policy
//! named explicitly whatever this process's `policy`; every fold summary.rs `fold`; every pool select.rs
//! `pool_tokens_under` the learned policy, per check as the hook calls it; the cold start's refits
//! summary.rs `with_fit`; P0's reference conscience.rs `graded_without`; P2 policy.rs `fit`,
//! `fit_pruned`, `predict`, `prior`, `prior_pruned`; P3 policy.rs `Decision::risk`; P4 policy.rs `ungated`, `prior`,
//! `fit` and its baseline policy.rs `fixed_routes`. Where the replay still differs from what production ran:
//! - The refit is `with_fit`, the pure part of the summary.rs `refit` that main.rs `learn_store` calls:
//!   `refit` loads the cache plus the log's tail (equal to the replay's fold, which P1's fold check
//!   shows), adds the revert mark beside the store (the replay keeps the revert in memory, sticky as
//!   `with_fit` keeps it), and writes the cache and the mark, which a replay never does. It runs before
//!   the first graded record after a session's end, where production runs it in the background after
//!   the session settles: a hook that decided before `borg learn` finished read the older fit. P1
//!   replays each record from its logged fit, so only the cold start assumes the refit was in time.
//! - The had lessons are the record's `nofree`, which is all of `had` a decision reads (the had lessons
//!   among its applies); a record from before it replays with none, as its hook decided. In the cold
//!   start they stay the logged history's, not what the replayed policy's own earlier decisions would
//!   have delivered: only the log's deliveries say that.
//! - The cold start's labels join by route: where the replayed route differs from the logged one, the
//!   verdict joins nothing, so its model learns only from the routes the replay took.
//! - The pools are chosen under the loosest latency limit (`LOOSEST`), as `price` replays them, where
//!   the hook passes the harness's stop budget: a limit only takes the largest pools out of the
//!   choice, so P3's pool spend is the most any hook's limit allows.
//! - P2's fit reads a log rebuilt from the labels (`known`): the graded checks up to the newest result
//!   then and each label's verdict at its value then, without the delivery records, settles and other
//!   lines of the real log, so the judge's missingness model sees fewer fates than production's fit at
//!   that check; no budget (it prices only pools).
//! - P4 draws `ungated` (the play of `choose` with a gate that admits every deviation), at pool tokens
//!   0 (the state files log none), with no had (the state lines log no `nofree`, and the hook that ran
//!   the evaluation decided without), from a stand-in check id per check, each check independent of the
//!   run's earlier draws.
//! - P1 compares the asked and free lists, propensities and source, not the placebo draw or its chance
//!   (`psi`): the pre-registered measure.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::policy::{self, Fit};
use super::summary::{self, Summary};
use super::{Ctx, Policy, Route, Routed, Unit};
use crate::conscience::{self, Challenge};
use crate::memory;
use crate::ope::{self, Estimate};
use crate::outcome::{self, Outcome};
use crate::{collective, learn};

/// The pre-registration these checks implement (`RULES_AT`), by the date its rules were written.
/// Protocol: printed with every report, never changed: a later change is a dated amendment
/// (`AMENDMENT`).
pub const RULES: &str = "2026-10-28";
/// The latest dated amendment of `RULES` this file implements (2026-10-29: P2's prune-and-rerun once;
/// the bars unchanged). Protocol: printed with every report beside `RULES`.
pub const AMENDMENT: &str = "2026-10-29";
/// Where the rules and their dated amendments are written: a tracked file of this repository, so a
/// reader of the report can open them. Protocol: printed with every report.
pub const RULES_AT: &str = "docs/evidence.md#lesson-selection";

/// P2's bar: the session-bootstrap 95% upper bound of the mean log-loss difference (model − grade
/// only) may not exceed this. Method: pre-registered.
pub const CALIBRATION: f64 = 0.005;

/// P4's margin: the policy's right-lesson recall may lie at most this far below the fixed rule's (5
/// points), point estimate and case-paired bootstrap lower bound. Method: pre-registered.
pub const RECALL_MARGIN: f64 = 0.05;

/// The online readout is due after this many randomised labelled units (a logged route probability
/// strictly between 0 and 1) or this many sessions under the learned policy. Method: pre-registered.
pub const READOUT_UNITS: usize = 300;
pub const READOUT_SESSIONS: usize = 30;

/// A probability a log-loss reads is kept this far from 0 and 1. Guard: a model's 0 or 1 would make
/// one unit's log-loss infinite and the mean meaningless.
const CLAMP: f64 = 1e-9;

/// Differences listed per check. Display.
const SHOW: usize = 5;

/// The routes a label can have been delivered by, as indices: asked, free, not delivered (placebo).
/// Protocol: select.rs `ARMS` and `Route::arm`, the order of every per-route array below.
const ASK: usize = Route::Ask.arm();
const FREE: usize = Route::Free.arm();
const NOT: usize = Route::Not.arm();
const ROUTES: usize = super::ARMS;
const ROUTE_NAMES: [&str; ROUTES] = ["Q", "F", "D"];

/// Grades index their counts directly, 1–3; 0 is unused. Protocol: the model's grades.
const GRADES: usize = 4;

/// A check's result against its pre-registered bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
    /// Nothing to check it on yet (`why` says what is missing): no evaluation given, or one still
    /// incomplete. Not decisive: the command passes it.
    NotRun,
    /// Run, but on data that could not have failed it: no check that applies a lesson, no deviation
    /// from the fixed rule, an arm that decides as the fixed rule everywhere, no label learned from,
    /// every prediction 1/2. Never a pass: a decisive one fails the command (`unmet`).
    NotTested,
    /// Reported, not decisive (P5).
    Reported,
}

impl Verdict {
    /// PASS when `ok`, else FAIL; but NOT TESTED instead of a pass when the data could not have failed
    /// it (`tested` false).
    fn tested(ok: bool, tested: bool) -> Self {
        match (ok, tested) {
            (false, _) => Verdict::Fail,
            (true, false) => Verdict::NotTested,
            (true, true) => Verdict::Pass,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::NotRun => "NOT RUN",
            Verdict::NotTested => "NOT TESTED",
            Verdict::Reported => "REPORTED (non-decisive)",
        }
    }
}

// --- The log -------------------------------------------------------------------------------------

/// One line of the outcome log: its text (with its newline) and its record, None when it does not parse.
struct Line<'t> {
    text: &'t str,
    o: Option<Outcome>,
}

fn lines(text: &str) -> Vec<Line<'_>> {
    text.split_inclusive('\n').map(|t| Line { text: t, o: serde_json::from_str(t.trim()).ok() }).collect()
}

/// A graded record's applies, one lesson per apply; None when its grades are not aligned with them.
fn challenges(o: &Outcome) -> Option<Vec<Challenge>> {
    (o.grades.len() == o.lessons.len()).then(|| o.lessons.iter().map(|id| Challenge { id: id.clone(), ..Default::default() }).collect())
}

fn applied<'a>(cs: &'a [Challenge], o: &Outcome) -> Vec<(&'a Challenge, u8)> {
    cs.iter().zip(o.grades.iter().copied()).collect()
}

fn ids(v: &[&Challenge]) -> Vec<String> {
    v.iter().map(|c| c.id.clone()).collect()
}

fn channel(o: &Outcome) -> &'static str {
    if o.channel == "live" { "live" } else { "stop" }
}

/// The lessons a graded record's session already had, as its decision read them (select.rs `Ctx::had`):
/// its logged `nofree`, sorted, empty for a record from before it (whose hook let every apply go free).
/// Only the had lessons among a check's applies can change its decision, and `nofree` is exactly those.
fn had_of(o: &Outcome) -> Vec<String> {
    let mut had = o.nofree.clone();
    had.sort_unstable();
    had.dedup();
    had
}

/// The production decision's context for the graded record `o` with its session's `had` (`had_of`).
fn ctx<'a>(o: &'a Outcome, had: &'a [String], lessons: &'a Path, s: &'a Summary) -> Ctx<'a> {
    Ctx { session: &o.session, check: &o.check, channel: channel(o), lessons, summary: s, tokens: o.tokens, had }
}

/// The policy a graded record can be replayed under: the fixed rule or the learned policy. The
/// evaluation switches are not: the cap's N is not logged, and the learned cut reads the whole store.
fn replayable(o: &Outcome) -> Option<Policy> {
    match outcome::policy_of(o) {
        "fixed" => Some(Policy::Fixed),
        "learned" => Some(Policy::Learned),
        _ => None,
    }
}

/// Per apply, 1 when `of` holds its lesson, else 0 (attention.rs `fixed_p`).
fn holds(lessons: &[String], of: &[String]) -> Vec<f64> {
    lessons.iter().map(|l| f64::from(u8::from(of.contains(l)))).collect()
}

/// A graded check of the log: its record, its session's had lessons (`had_of`), its logged decision
/// and propensities (an older record's free list and pf replayed by `outcome::free_of`), and the fixed
/// rule's decision on its applies for that session (conscience.rs `graded_without`, no cap).
#[derive(Debug, Clone)]
struct Check {
    o: Outcome,
    had: Vec<String>,
    asked: Vec<String>,
    free: Vec<String>,
    p: Vec<f64>,
    pf: Vec<f64>,
    fixed: (Vec<String>, Vec<String>),
}

impl Check {
    fn new(o: Outcome, applied: &[(&Challenge, u8)]) -> Self {
        let free = outcome::free_of(&o);
        let p = if o.p.len() == o.lessons.len() { o.p.clone() } else { holds(&o.lessons, &o.chosen) };
        let pf = if o.pf.len() == o.lessons.len() { o.pf.clone() } else { holds(&o.lessons, &free) };
        let had = had_of(&o);
        let (ask, fr) = conscience::graded_without(applied, None, &had);
        Check { had, asked: o.chosen.clone(), free, p, pf, fixed: (ids(&ask), ids(&fr)), o }
    }

    fn line(&self) -> String {
        let check = if self.o.check.is_empty() { "-" } else { self.o.check.as_str() };
        format!("session {} check {check} ms {}", self.o.session, self.o.ms)
    }
}

// --- Where the checks meet the policy core ---------------------------------------------------------

/// `f` on `s` with the gate's budget at 0 (P0), `s` as it was afterwards: the gate's state is swapped
/// out and back, never cloned with the summary, so a replay stays linear in the log.
fn gate_closed<T>(s: &mut Summary, f: impl FnOnce(&Summary) -> T) -> T {
    let gate = std::mem::take(&mut s.gate);
    let out = f(s);
    s.gate = gate;
    out
}

/// One draw of the learned policy without its gate (P4): select/policy.rs `ungated` (the play of
/// `choose` with a gate that admits every deviation) on `fit` at the evaluation's check `c`, whose
/// state file logs no pool tokens (the baseline's).
fn ungated(units: &[Unit], c: &EvalCheck, fit: &Fit, seed: u64) -> Vec<Route> {
    policy::ungated(units, &policy::Check { session: &c.session, stop: c.channel == "stop", tokens: 0 }, fit, seed)
}

/// The quality `r` risks below the fixed rule at the gate's 5% quantile (P3): select/policy.rs
/// `Decision::risk`, which the gate admitted only within its budget; a decision that keeps the fixed
/// rule risks nothing.
fn risk(r: &Routed, deviates: bool) -> f64 {
    if deviates { r.log.risk } else { 0.0 }
}

/// A unit's prediction, fitted on the labels before it.
type Predictor<'a> = Box<dyn Fn(&Label) -> f64 + 'a>;

/// The value model P2 predicts by: the one the policy decides with (select/policy.rs `fit`), or, in
/// the pre-registered rerun after its failure, the model pruned to grade, route and dilution
/// (`fit_pruned`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Value {
    Full,
    Pruned,
}

impl Value {
    fn fit(self, s: &Summary) -> Fit {
        match self {
            Value::Full => policy::fit(s, None),
            Value::Pruned => policy::fit_pruned(s, None),
        }
    }

    /// Its n = 0 prior, for a unit with no label before its check.
    fn prior(self) -> Fit {
        match self {
            Value::Full => policy::prior(),
            Value::Pruned => policy::prior_pruned(),
        }
    }
}

/// The value model `value` fitted on `past` as known before `at` (P2): its fit of the log `past` makes
/// (`known`), its n = 0 prior's (`Value::prior`) while it has no label, and per unit
/// `policy::predict`: P(changed ∧ avoids) on its logged route among the check's applies of that route,
/// its features read from its check's record (`checks[l.check].o`, apply `l.j`) as select/summary.rs
/// `fold` reads them.
fn model<'a>(checks: &'a [Check], past: &[&Label], at: i64, value: Value) -> Option<Predictor<'a>> {
    let fit = Some(value.fit(&known(checks, past, at))).filter(|f| f.model.is_some()).unwrap_or_else(|| value.prior());
    Some(Box::new(move |l: &Label| {
        let o = &checks[l.check].o;
        let routes = routes_of(&checks[l.check]);
        let units: Vec<Unit> = o
            .lessons
            .iter()
            .zip(&o.grades)
            .enumerate()
            .map(|(j, (lesson, grade))| Unit { lesson, grade: *grade, rank: o.ranks.get(j).map_or(j, |r| usize::from(*r)) })
            .collect();
        let route = [Route::Ask, Route::Free, Route::Not][l.route];
        let m = routes.iter().filter(|r| **r == route).count();
        let feats = policy::Feat::of(&units, o.channel == "stop", o.tokens);
        policy::predict(&fit, &feats[l.j], route, m, &o.session, &o.lessons[l.j]).unwrap_or(0.5)
    }))
}

/// A check's route per apply, as select/summary.rs `fold` reads its record: asked, else free, else
/// placebo, else not now.
fn routes_of(c: &Check) -> Vec<Route> {
    let has = |v: &[String], l: &String| v.contains(l);
    c.o.lessons
        .iter()
        .map(|l| match () {
            _ if has(&c.asked, l) => Route::Ask,
            _ if has(&c.free, l) => Route::Free,
            _ if has(&c.o.placebo, l) => Route::Placebo,
            _ => Route::Not,
        })
        .collect()
}

/// The summary a fit on exactly `past` as known before `at` reads: every check up to the newest of
/// their results then and every check of theirs (as logged, in log order, an older one given a check id
/// of its own), then each label's verdict as known before `at` (`Label::at`) joined to its check by id,
/// on its route, by its judge. Leave-future-out by construction: no other verdict is in it, and no
/// result from `at` on.
fn known(checks: &[Check], past: &[&Label], at: i64) -> Summary {
    let then: Vec<(&Label, (i64, f64))> = past.iter().filter_map(|l| l.at(at).map(|k| (*l, k))).collect();
    let Some(until) = then.iter().map(|(_, (ms, _))| *ms).max() else { return Summary::default() };
    // An id no logged check has (they are `select::CHECK_HEX` hex digits).
    let id = |i: usize| if checks[i].o.check.is_empty() { format!("replay-{i}") } else { checks[i].o.check.clone() };
    let theirs: BTreeSet<usize> = then.iter().map(|(l, _)| l.check).collect();
    let mut log: Vec<Outcome> =
        checks.iter().enumerate().filter(|(i, c)| c.o.ms <= until || theirs.contains(i)).map(|(i, c)| Outcome { check: id(i), ..c.o.clone() }).collect();
    for (l, (ms, y)) in then {
        let o = &checks[l.check].o;
        log.push(Outcome {
            kind: if y > 0.5 { outcome::CHANGED } else { outcome::IGNORED }.into(),
            session: o.session.clone(),
            lessons: vec![o.lessons[l.j].clone()],
            ms,
            channel: match l.route {
                ASK => channel(o).into(),
                FREE => "queued".into(),
                _ => outcome::PLACEBO.into(),
            },
            check: id(l.check),
            judge: l.judge,
            ..Outcome::default()
        });
    }
    let text: String = log.iter().filter_map(|o| serde_json::to_string(o).ok()).map(|j| j + "\n").collect();
    summary::fold(&text, Summary::default())
}

// --- Pass 1: the log as logged (P0, P1) -----------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct P0 {
    pub verdict: Verdict,
    /// Graded checks decided at gate budget 0.
    pub checks: usize,
    /// Of them, the checks that apply no lesson (none applied, or the call failed): every policy asks
    /// and hands over nothing there, so they are compared but test nothing, and P0 is NOT TESTED when
    /// they are all it has.
    pub empty: usize,
    /// Checks whose decision is not the fixed rule's, or (logged under the fixed rule) not the logged one.
    pub differ: usize,
    pub first: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct P1 {
    pub verdict: Verdict,
    /// Checks replayed (fixed rule, learned policy), and those that reproduced their log.
    pub replayed: usize,
    pub reproduced: usize,
    /// Of the replayed checks, those that apply no lesson (P0 `empty`): their decision and propensities
    /// are empty under any policy, so only the summary and the source their record names are tested
    /// there, and P1 is NOT TESTED when they are all it replayed.
    pub empty: usize,
    /// Replayed checks whose record names the summary its decision read (`Outcome::seen`); the others
    /// (older records) are replayed on every line before them.
    pub seen: usize,
    /// Checks under an evaluation switch (cap, learned cut): counted, not replayed.
    pub switches: usize,
    /// The largest |Δ| between a replayed and a logged propensity (p or pf).
    pub max_delta: f64,
    /// Folding the log check by check gives the summary that folding it whole gives.
    pub fold: bool,
    pub first: Vec<String>,
}

/// Pass 1's results: the graded checks (in log order), P0, P1 and the store's posterior summary.
struct AsLogged {
    checks: Vec<Check>,
    malformed: usize,
    p0: P0,
    p1: P1,
    posterior: Summary,
}

/// One fit's summary as the replay folds it on (`fitted`): the fit's offset `at` and what the refit set
/// there (summary.rs `with_fit`: the fit, the gate's ledger, the monitor), and the summary folded on
/// to byte `pos`.
struct Fitted {
    at: usize,
    refit: (Fit, summary::Gate, policy::Monitor),
    s: Summary,
    pos: usize,
}

/// The summary a hook decided with under fit `key` when it had read the log up to byte `upto` of
/// `text`: the refit (summary.rs `with_fit`, the collective's `budget` included) of the log up to the
/// fit's offset (the key's `at`), then the log from there folded on it as summary.rs `load` does. Kept
/// per key and folded on as the records come, so each fit's tail is folded about once; a decision that
/// read less of the log than the last one under the same fit (two checks at once, `Outcome::seen`) is
/// folded again from the fit's offset. None when no refit of the log there gives that key, or the
/// decision read less than the fit.
fn fitted<'a>(cache: &'a mut HashMap<String, Option<Fitted>>, text: &str, upto: usize, key: &str, budget: Option<f64>) -> Option<&'a mut Summary> {
    let entry = cache.entry(key.to_string()).or_insert_with(|| {
        let at: usize = key.split('-').next()?.parse().ok()?;
        if !text.is_char_boundary(at) {
            return None;
        }
        let s = summary::with_fit(summary::fold(&text[..at], Summary::default()), budget);
        (s.fit.key == key).then(|| Fitted { at, refit: (s.fit.clone(), s.gate.clone(), s.monitor.clone()), s, pos: at })
    });
    let f = entry.as_mut().filter(|f| f.at <= upto)?;
    if upto < f.pos {
        // A fresh fold with what the refit set is the refit itself (`with_fit` changes nothing else).
        let mut s = summary::fold(&text[..f.at], Summary::default());
        (s.fit, s.gate, s.monitor) = f.refit.clone();
        (f.s, f.pos) = (s, f.at);
    }
    if f.pos < upto {
        f.s = summary::fold(&text[f.pos..upto], std::mem::take(&mut f.s));
        f.pos = upto;
    }
    Some(&mut f.s)
}

/// The byte of `text` up to which the decision of the graded record at byte `here` read the log: the
/// summary its record names (`Outcome::seen`, read before the model call, so it can end before
/// records other checks appended meanwhile), else every line before the record. Err when the named
/// summary is not a line boundary before the record.
fn seen_at(o: &Outcome, text: &str, here: usize) -> Result<usize, String> {
    if o.seen.is_empty() {
        return Ok(here);
    }
    let at = o.seen.split('-').next().and_then(|a| a.parse::<usize>().ok());
    at.filter(|&a| a <= here && text.is_char_boundary(a) && (a == 0 || text[..a].ends_with('\n')))
        .ok_or_else(|| format!("its summary {:?} is no line boundary before it", o.seen))
}

fn as_logged(text: &str, lessons: &Path) -> AsLogged {
    let mut s = Summary::default();
    // Per fit key the hook logged, the summary it decided with (`fitted`).
    let mut fits: HashMap<String, Option<Fitted>> = HashMap::new();
    let budget = super::budget_of(lessons);
    let mut at = 0;
    let mut checks = Vec::new();
    let mut malformed = 0;
    let mut p0 = P0 { verdict: Verdict::Pass, checks: 0, empty: 0, differ: 0, first: Vec::new() };
    let mut p1 = P1 { verdict: Verdict::Pass, replayed: 0, reproduced: 0, empty: 0, seen: 0, switches: 0, max_delta: 0.0, fold: true, first: Vec::new() };
    let mut start = 0;
    for line in lines(text) {
        let here = start;
        start += line.text.len();
        let Some(o) = line.o.filter(|o| o.kind == outcome::GRADED) else { continue };
        let Some(cs) = challenges(&o) else {
            malformed += 1;
            continue;
        };
        // The summary the hook had: every line before this record.
        s = summary::fold(&text[at..here], s);
        at = here;
        let applied = applied(&cs, &o);
        let check = Check::new(o, &applied);
        // P0: at gate budget 0 the learned policy is the fixed rule, and the fixed rule is what was logged.
        let r = gate_closed(&mut s, |s| super::graded_under(Policy::Learned, &applied, &ctx(&check.o, &check.had, lessons, s)));
        let mut same = (ids(&r.ask), ids(&r.free)) == check.fixed;
        if replayable(&check.o) == Some(Policy::Fixed) {
            same &= (&check.asked, &check.free) == (&check.fixed.0, &check.fixed.1);
        }
        p0.checks += 1;
        p0.empty += usize::from(applied.is_empty());
        if !same {
            p0.differ += 1;
            if p0.first.len() < SHOW {
                p0.first.push(format!(
                    "{}: gate 0 asks {:?} free {:?}; logged {:?} free {:?}; fixed rule {:?} free {:?}",
                    check.line(),
                    ids(&r.ask),
                    ids(&r.free),
                    check.asked,
                    check.free,
                    check.fixed.0,
                    check.fixed.1
                ));
            }
        }
        // P1: the logged policy from its logged fit, summary and seed.
        match replayable(&check.o) {
            None => p1.switches += 1,
            Some(policy) => {
                p1.replayed += 1;
                p1.empty += usize::from(applied.is_empty());
                p1.seen += usize::from(!check.o.seen.is_empty());
                let found = match seen_at(&check.o, text, here) {
                    Err(e) => Err(e),
                    // The prior has no model: its decision is the fixed rule whatever else the summary holds.
                    Ok(_) if check.o.fit.is_empty() => Ok(&mut s),
                    Ok(upto) => fitted(&mut fits, text, upto, &check.o.fit, budget)
                        .filter(|f| check.o.seen.is_empty() || f.key() == check.o.seen)
                        .ok_or_else(|| format!("no summary with fit {:?} and key {:?} before it", check.o.fit, check.o.seen)),
                };
                let miss = match found {
                    Err(e) => Some(format!("{}: {e}", check.line())),
                    Ok(_) if policy == Policy::Learned && check.o.check.is_empty() => Some(format!("{}: learned without a check id", check.line())),
                    Ok(at) => {
                        // The store's revert as the hook read it: its summary ORs in the sticky mark beside
                        // the store (summary.rs `REVERTED`), which no log line holds; select.rs `learned`
                        // logs it as its source.
                        let logged = !check.o.source.is_empty();
                        let kept = at.monitor.reverted;
                        if logged {
                            at.monitor.reverted = check.o.source == super::REVERTED_SOURCE;
                        }
                        let r = super::graded_under(policy, &applied, &ctx(&check.o, &check.had, lessons, at));
                        at.monitor.reverted = kept;
                        let delta = r.p.iter().zip(&check.p).chain(r.pf.iter().zip(&check.pf)).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
                        p1.max_delta = p1.max_delta.max(delta);
                        let decided = ids(&r.ask) == check.asked && ids(&r.free) == check.free && (!logged || r.log.source == check.o.source);
                        let aligned = r.p.len() == check.p.len() && r.pf.len() == check.pf.len();
                        (!decided || !aligned || delta > ope::AGREE).then(|| {
                            format!(
                                "{}: replay asks {:?} free {:?} by {} (|Δp| {delta:.4}); logged {:?} free {:?} by {}",
                                check.line(),
                                ids(&r.ask),
                                ids(&r.free),
                                r.log.source,
                                check.asked,
                                check.free,
                                if logged { check.o.source.as_str() } else { "-" }
                            )
                        })
                    }
                };
                match miss {
                    None => p1.reproduced += 1,
                    Some(m) if p1.first.len() < SHOW => p1.first.push(m),
                    Some(_) => {}
                }
            }
        }
        checks.push(check);
    }
    let posterior = summary::fold(&text[at..], s);
    p1.fold = posterior == summary::fold(text, Summary::default());
    // Tested only by a check that applies a lesson: on the others every policy decides nothing (`empty`).
    p0.verdict = Verdict::tested(p0.differ == 0, p0.checks > p0.empty);
    p1.verdict = Verdict::tested(p1.reproduced == p1.replayed && p1.fold, p1.replayed > p1.empty);
    AsLogged { checks, malformed, p0, p1, posterior }
}

// --- Pass 2: cold start (P3, and the policy P5 evaluates) ------------------------------------------

/// What a decision spends, summed over checks: model calls, questions asked, blocks at the stop (an
/// extra round each), lessons handed over free, the pool's tokens where logged, and the logged calls'
/// cost in dollars (`usd`, the same for any policy replayed on them: a replay makes no call). Measured,
/// never priced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Spend {
    pub checks: usize,
    pub questions: usize,
    pub blocks: usize,
    pub free: usize,
    pub tokens: u64,
    pub usd: f64,
}

impl Spend {
    fn add(&mut self, o: &Outcome, asked: usize, free: usize) {
        self.checks += 1;
        self.questions += asked;
        self.blocks += usize::from(channel(o) == "stop" && asked > 0);
        self.free += free;
        self.tokens += u64::from(o.tokens);
        self.usd += o.usd;
    }
}

/// One check of the cold-start replay: whether the policy left the fixed rule, the gate's budget at
/// that check and the quality the decision risked below the fixed rule.
#[derive(Debug, Clone, PartialEq)]
struct Step {
    deviates: bool,
    gate: f64,
    risk: f64,
    at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct P3 {
    pub verdict: Verdict,
    /// Why it is not tested: no check, or no decision left the fixed rule, so the gate was never put to
    /// the test. Empty otherwise.
    pub why: String,
    pub checks: usize,
    pub deviations: usize,
    /// The first check decided as the fixed rule with a budget of 0.
    pub cold: bool,
    /// Checks whose decision risked more than the gate's budget there, or whose budget was negative.
    pub over: usize,
    /// The engineer's budget for the collective (select.rs `budget_of`: pool tokens per session), by
    /// which the policy prices pool tokens; none by default. With one, the replayed pools must stay
    /// within it (`pool`); spend is reported either way.
    pub budget: Option<f64>,
    /// The pool tokens the replayed policy's pools spend per session, as select/policy.rs `price`
    /// measures what a budget bounds: each check at the pool budget `pool_tokens` chose for it, averaged
    /// over the sessions with a check.
    pub pool: f64,
    /// The refits the replay made, one wherever a session had ended since the last (`cold_start`).
    pub refits: usize,
    pub policy: Spend,
    pub fixed: Spend,
    pub first: Vec<String>,
}

/// P3 over the cold-start steps, and the replayed pools' spend per session (`pool`) against the
/// engineer's `budget` (the spends are carried through).
fn conduct(steps: &[Step], policy: Spend, fixed: Spend, budget: Option<f64>, pool: f64) -> P3 {
    let cold = steps.first().is_none_or(|s| !s.deviates && s.gate == 0.0);
    let bad: Vec<&Step> = steps.iter().filter(|s| s.gate.is_nan() || s.gate < 0.0 || s.risk > s.gate).collect();
    let mut first: Vec<String> = bad.iter().take(SHOW).map(|s| format!("{}: risked {} with a budget of {:.4}", s.at, s.risk, s.gate)).collect();
    let over_budget = budget.filter(|b| pool > *b);
    if let Some(b) = over_budget {
        first.push(format!("the policy's pools spend {pool:.0} tokens per session, over the collective's budget of {b}"));
    }
    let deviations = steps.iter().filter(|s| s.deviates).count();
    let verdict = Verdict::tested(cold && bad.is_empty() && over_budget.is_none(), deviations > 0);
    let why = match (verdict, steps.len()) {
        (Verdict::NotTested, 0) => "no graded check to replay".to_string(),
        (Verdict::NotTested, _) => "no decision left the fixed rule, so the gate never had to hold one back".to_string(),
        _ => String::new(),
    };
    P3 { verdict, why, checks: steps.len(), deviations, cold, over: bad.len(), budget, pool, refits: 0, policy, fixed, first }
}

/// A check's propensities of asking (`p`) and of handing over free (`pf`), aligned with its applies.
type Propensities = (Vec<f64>, Vec<f64>);

/// The latency limit the replay's pools are chosen under: the loosest, which no pool's latency
/// exceeds. Method: as select/policy.rs `price` replays pools, so the spend P3 holds against an
/// engineer's budget is the most a hook's limit (the harness's stop budget) allows; no limit at all
/// (None) would instead forbid every pool above the baseline.
const LOOSEST: Option<u64> = Some(u64::MAX);

/// The pool budget the learned policy chooses for a check of `session` on `s` (select.rs
/// `pool_tokens_under`, whatever this process's `policy`), under `LOOSEST`.
fn pool_of(s: &Summary, session: &str) -> usize {
    super::pool_tokens_under(Policy::Learned, s, LOOSEST, session)
}

/// The graded record `o` as the decision `r` would have logged it (hook.rs `record_graded`): its
/// routes, its own propensities (the placebo's `psi` weighs the monitor's estimate, select/policy.rs
/// `monitor`), policy, fit and source; its applies, had lessons (`nofree`), pool and tail stay the
/// logged check's.
fn decided(o: Outcome, r: &Routed) -> Outcome {
    let round = |v: &[f64]| v.iter().map(|p| outcome::round_p(*p)).collect();
    Outcome {
        chosen: ids(&r.ask),
        free: ids(&r.free),
        placebo: ids(&r.placebo),
        p: round(&r.p),
        pf: round(&r.pf),
        psi: r.log.psi,
        policy: r.log.policy.into(),
        fit: r.log.fit.clone(),
        source: r.log.source.into(),
        explore: false,
        ..o
    }
}

/// The cold-start replay: P3, and per graded check (aligned with pass 1's) the learned policy's
/// propensities. The policy decides as a hook would: with the summary of every line before the check,
/// fitted where production fits it, at `borg learn` when a session ends (main.rs `learn_store`, after
/// the session's settle): refitted (summary.rs `with_fit`, the pure part of the `refit` that
/// `learn_store` calls, under the collective's `budget`) before the first check after a session ended,
/// the gate's ledger replayed over the policy's own checks; and under the had lessons its record logs
/// (`had_of`). Each check's pool is the budget the policy chooses there (select.rs `pool_tokens_under`
/// the learned policy, called per check as the hook calls `pool_tokens`) under the loosest latency
/// limit (`pool_of`).
fn cold_start(text: &str, lessons: &Path, budget: Option<f64>) -> (P3, Vec<Propensities>) {
    let mut s = Summary::default();
    let mut chunk = String::new();
    let (mut steps, mut targets) = (Vec::new(), Vec::new());
    let (mut spent, mut fixed) = (Spend::default(), Spend::default());
    let (mut ended, mut refits) = (0, 0);
    // Pool tokens per session.
    let mut pools: BTreeMap<String, u64> = BTreeMap::new();
    for line in lines(text) {
        let Some((o, cs)) = line.o.filter(|o| o.kind == outcome::GRADED).and_then(|o| challenges(&o).map(|cs| (o, cs))) else {
            chunk.push_str(line.text);
            continue;
        };
        s = summary::fold(&chunk, s);
        chunk.clear();
        // Ended sessions the summary holds; a compaction (summary.rs `MAX_SESSIONS`) drops some, after
        // which the next end counts from what is left.
        let now = s.sessions.iter().filter(|x| x.ended).count();
        ended = ended.min(now);
        if now > ended {
            s = summary::with_fit(s, budget);
            (ended, refits) = (now, refits + 1);
        }
        let applied = applied(&cs, &o);
        let had = had_of(&o);
        let r = super::graded_under(Policy::Learned, &applied, &ctx(&o, &had, lessons, &s));
        let (fa, ff) = conscience::graded_without(&applied, None, &had);
        let deviates = (ids(&r.ask), ids(&r.free)) != (ids(&fa), ids(&ff));
        spent.add(&o, r.ask.len(), r.free.len());
        fixed.add(&o, fa.len(), ff.len());
        let pool = pool_of(&s, &o.session);
        *pools.entry(o.session.clone()).or_default() += pool as u64;
        steps.push(Step { deviates, gate: r.log.gate, risk: risk(&r, deviates), at: format!("session {} ms {}", o.session, o.ms) });
        targets.push((r.p.clone(), r.pf.clone()));
        // The policy's own decision is what the summary folds next.
        match serde_json::to_string(&decided(o, &r)) {
            Ok(j) => {
                chunk.push_str(&j);
                chunk.push('\n');
            }
            Err(_) => chunk.push_str(line.text),
        }
    }
    let pool = if pools.is_empty() { 0.0 } else { pools.values().sum::<u64>() as f64 / pools.len() as f64 };
    let p3 = P3 { refits, ..conduct(&steps, spent, fixed, budget, pool) };
    (p3, targets)
}

// --- Labels (P2, P5, the readout) -----------------------------------------------------------------

/// One judged unit: a graded check's apply with the strongest result of its delivery.
#[derive(Debug, Clone, PartialEq)]
struct Label {
    /// Its check (index into pass 1's checks) and its place among the check's applies.
    check: usize,
    j: usize,
    grade: u8,
    /// The route that delivered it (`ASK`, `FREE`, `NOT` for a placebo) and each route's logged
    /// probability of a label (`NOT`'s: routed nowhere and drawn as a placebo).
    route: usize,
    mu: [f64; ROUTES],
    /// 1 changed and avoided, 0 not: from every result of it (the last of `known`).
    y: f64,
    session: String,
    check_ms: i64,
    /// Its value as the log knew it: from each result's ms on that changed it, the value then, in time
    /// order, the first at its first verdict. A prediction for a check at t reads the value before t
    /// (`at`): a veto, correction or stronger verdict after t is a result from after t.
    known: Vec<(i64, f64)>,
    /// The judge template version of its first verdict (outcome.rs `judge`).
    judge: u8,
}

impl Label {
    /// Its first result's ms: before it no check could know it.
    fn first(&self) -> i64 {
        self.known.first().map_or(i64::MAX, |k| k.0)
    }

    /// Its value as known before `t`, with the ms of the result that set it; None before its first.
    fn at(&self, t: i64) -> Option<(i64, f64)> {
        self.known.iter().take_while(|(ms, _)| *ms < t).last().copied()
    }
}

/// A unit's value over its results (each its ms and `ope::rank`), in time order: 1 while its strongest
/// result so far is the change, 0 otherwise, from its first verdict on; an entry per change.
fn values(verdicts: &[(i64, u8)], against: &[(i64, u8)]) -> Vec<(i64, f64)> {
    let Some(start) = verdicts.iter().map(|v| v.0).min() else { return Vec::new() };
    let mut events: Vec<(i64, u8)> = verdicts.iter().chain(against).copied().collect();
    events.sort_unstable();
    let mut out: Vec<(i64, f64)> = Vec::new();
    let mut strongest = 0;
    for (i, &(ms, rank)) in events.iter().enumerate() {
        strongest = strongest.max(rank);
        // Results at the same ms land together.
        if ms < start || events.get(i + 1).is_some_and(|e| e.0 == ms) {
            continue;
        }
        let y = f64::from(u8::from(strongest == 1));
        if out.last().is_none_or(|l| l.1 != y) {
            out.push((ms, y));
        }
    }
    out
}

/// The judged units of `checks` from the results in `log`, in check order, and the verdicts that
/// joined no check.
fn labels(checks: &[Check], log: &[Outcome]) -> (Vec<Label>, usize) {
    let by_id: HashMap<&str, usize> = checks.iter().enumerate().filter(|(_, c)| !c.o.check.is_empty()).map(|(i, c)| (c.o.check.as_str(), i)).collect();
    let mut by_lesson: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (i, c) in checks.iter().enumerate() {
        for l in &c.o.lessons {
            by_lesson.entry((c.o.session.as_str(), l.as_str())).or_default().push(i);
        }
    }
    let routed = |c: &Check, route: usize, l: &str| match route {
        ASK => c.asked.iter().any(|x| x == l),
        FREE => c.free.iter().any(|x| x == l),
        _ => c.o.placebo.iter().any(|x| x == l),
    };
    // A unit's verdicts (ms, rank), the route that delivered it, and its first verdict's ms and judge.
    struct Joined {
        verdicts: Vec<(i64, u8)>,
        route: usize,
        first: (i64, u8),
    }
    let mut joined: BTreeMap<(usize, usize), Joined> = BTreeMap::new();
    let mut unjoined = 0;
    for r in log.iter().filter(|r| r.kind == outcome::CHANGED || r.kind == outcome::IGNORED) {
        let route = match r.channel.as_str() {
            "early" => continue,
            "queued" => FREE,
            outcome::PLACEBO => NOT,
            _ => ASK,
        };
        let rank = ope::rank(&r.kind).unwrap_or(0);
        for l in &r.lessons {
            let at = match r.check.as_str() {
                "" => by_lesson
                    .get(&(r.session.as_str(), l.as_str()))
                    .and_then(|v| v.iter().rev().find(|&&i| checks[i].o.ms <= r.ms && routed(&checks[i], route, l)))
                    .copied(),
                id => by_id.get(id).copied(),
            };
            let Some((i, j)) = at.and_then(|i| checks[i].o.lessons.iter().position(|x| x == l).filter(|_| routed(&checks[i], route, l)).map(|j| (i, j))) else {
                unjoined += 1;
                continue;
            };
            let e = joined.entry((i, j)).or_insert(Joined { verdicts: Vec::new(), route, first: (r.ms, r.judge) });
            e.verdicts.push((r.ms, rank));
            if r.ms < e.first.0 {
                e.first = (r.ms, r.judge);
            }
        }
    }
    // A veto or correction in the session after the check outranks the change.
    let mut against: HashMap<(&str, &str), Vec<(i64, u8)>> = HashMap::new();
    for o in log.iter().filter(|o| o.kind == outcome::VETOED || o.kind == outcome::CORRECTED) {
        for l in &o.lessons {
            against.entry((o.session.as_str(), l.as_str())).or_default().push((o.ms, ope::rank(&o.kind).unwrap_or(0)));
        }
    }
    let out = joined
        .into_iter()
        .map(|((i, j), Joined { verdicts, route, first: (_, judge) })| {
            let c = &checks[i];
            let l = c.o.lessons[j].as_str();
            let later: Vec<(i64, u8)> = against.get(&(c.o.session.as_str(), l)).into_iter().flatten().filter(|(t, _)| *t >= c.o.ms).copied().collect();
            let known = values(&verdicts, &later);
            let (p, pf) = (c.p.get(j).copied().unwrap_or(0.0), c.pf.get(j).copied().unwrap_or(0.0));
            // Not delivered, a unit is judged only when the placebo draw took it (its chance ψ, `psi`):
            // that route's probability is both, as select/policy.rs `monitor` weighs it, a placebo that
            // happened at least at `PSI_FLOOR`.
            let psi = if route == NOT { c.o.psi.max(policy::PSI_FLOOR) } else { c.o.psi };
            Label {
                check: i,
                j,
                grade: c.o.grades[j],
                route,
                mu: [p, pf, (1.0 - p - pf).max(0.0) * psi],
                y: known.last().map_or(0.0, |k| k.1),
                session: c.o.session.clone(),
                check_ms: c.o.ms,
                known,
                judge,
            }
        })
        .collect();
    (out, unjoined)
}

/// Judged and changed counts per grade and route.
type Counts = [[(u32, u32); ROUTES]; GRADES];

/// Every change of a label's known value (`Label::known`), as (ms, label index, value), in time order.
fn changes(labels: &[Label]) -> Vec<(i64, usize, f64)> {
    let mut v: Vec<(i64, usize, f64)> = labels.iter().enumerate().flat_map(|(i, l)| l.known.iter().map(move |&(ms, y)| (ms, i, y))).collect();
    v.sort_by_key(|c| (c.0, c.1));
    v
}

/// Per label, the counts of the labels whose result came before its check, each at its value then:
/// what a decision at that check could have known (leave-future-out).
fn past_counts(labels: &[Label]) -> Vec<Counts> {
    let changes = changes(labels);
    let mut order: Vec<usize> = (0..labels.len()).collect();
    order.sort_by_key(|&i| labels[i].check_ms);
    let mut counts: Counts = [[(0, 0); ROUTES]; GRADES];
    let mut now: Vec<Option<f64>> = vec![None; labels.len()];
    let mut out = vec![counts; labels.len()];
    let mut next = 0;
    for i in order {
        while next < changes.len() && changes[next].0 < labels[i].check_ms {
            let (_, k, y) = changes[next];
            let c = &mut counts[usize::from(labels[k].grade).min(GRADES - 1)][labels[k].route];
            if let Some(was) = now[k] {
                c.0 -= 1;
                c.1 -= u32::from(was > 0.5);
            }
            c.0 += 1;
            c.1 += u32::from(y > 0.5);
            now[k] = Some(y);
            next += 1;
        }
        out[i] = counts;
    }
    out
}

/// Beta(1, 1)'s posterior mean after `k` of `n`.
fn beta(n: u32, k: u32) -> f64 {
    (1.0 + f64::from(k)) / (2.0 + f64::from(n))
}

/// The grade-only Beta-Binomial's prediction of a unit of `grade` (every route pooled).
fn by_grade(c: &Counts, grade: u8) -> f64 {
    let row = &c[usize::from(grade).min(GRADES - 1)];
    beta(row.iter().map(|x| x.0).sum(), row.iter().map(|x| x.1).sum())
}

fn log_loss(p: f64, y: f64) -> f64 {
    let p = p.clamp(CLAMP, 1.0 - CLAMP);
    -(y * p.ln() + (1.0 - y) * (1.0 - p).ln())
}

/// Session index per label, in first-seen order, for the session bootstrap.
fn sessions(labels: &[Label]) -> Vec<usize> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    labels
        .iter()
        .map(|l| {
            let n = seen.len();
            *seen.entry(l.session.as_str()).or_insert(n)
        })
        .collect()
}

// --- P2 -------------------------------------------------------------------------------------------

/// A prediction this close to 1/2 is a coin's: P2 is NOT TESTED when every prediction of both models
/// is one. Method: float slack around an exact 1/2 (Beta(1, 1) at k = n/2, a model's η = 0), far
/// below anything a label moves.
const COIN: f64 = 1e-9;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct P2 {
    pub verdict: Verdict,
    pub why: String,
    pub labels: usize,
    pub sessions: usize,
    /// Judged verdicts that joined no graded check (an early delivery's never does).
    pub unjoined: usize,
    /// Judged units with a label known before their check: those both models predict from data.
    pub informed: usize,
    /// Mean leave-future-out log-loss, grade only and of the value model.
    pub grade_only: f64,
    pub model: Option<f64>,
    /// The mean difference (model − grade only) with its session-bootstrap 95% interval.
    pub diff: Option<Estimate>,
    /// The pre-registered rerun after the value model's FAIL, on the same units: the model pruned to
    /// grade, route and dilution (select/policy.rs `fit_pruned`); its verdict is P2's. None unless it
    /// ran.
    pub pruned: Option<Rerun>,
}

/// One rerun of P2's comparison by another value model.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rerun {
    pub verdict: Verdict,
    pub model: f64,
    pub diff: Estimate,
}

/// A fit of P2's value model `Value` on the labels known before a check at the given ms.
type Fitter<'f, 'a> = dyn Fn(Value, &[&Label], i64) -> Option<Predictor<'a>> + 'f;

/// Each label's leave-future-out prediction by `value` (`fit` refitted wherever what is known before
/// a check changed), or why there is none.
fn predictions<'a>(labels: &[Label], value: Value, fit: &Fitter<'_, 'a>) -> Result<Vec<f64>, String> {
    let changes = changes(labels);
    let mut order: Vec<usize> = (0..labels.len()).collect();
    order.sort_by_key(|&i| labels[i].check_ms);
    let mut out = vec![0.5; labels.len()];
    let (mut known, mut fitted): (usize, Option<(usize, Predictor<'a>)>) = (0, None);
    for i in order {
        let at = labels[i].check_ms;
        while known < changes.len() && changes[known].0 < at {
            known += 1;
        }
        if fitted.as_ref().is_none_or(|(n, _)| *n != known) {
            let past: Vec<&Label> = labels.iter().filter(|l| l.first() < at).collect();
            let Some(m) = fit(value, &past, at) else { return Err("no value model yet (select/policy.rs)".into()) };
            fitted = Some((known, m));
        }
        if let Some((_, m)) = &fitted {
            out[i] = m(&labels[i]);
        }
    }
    Ok(out)
}

/// The model's predictions `model` against grade only's `grade` on `labels`: the verdict, its mean
/// log-loss and the mean difference (session bootstrap over `ss`). NOT TESTED when every prediction of
/// both is 1/2 (`COIN`).
fn compare(labels: &[Label], grade: &[f64], model: &[f64], ss: &[usize]) -> Rerun {
    let ll = |p: &[f64]| -> Vec<f64> { p.iter().zip(labels).map(|(p, l)| log_loss(*p, l.y)).collect() };
    let (g, m) = (ll(grade), ll(model));
    let units: Vec<(f64, f64, usize)> = m.iter().zip(&g).zip(ss).map(|((m, g), s)| (1.0, m - g, *s)).collect();
    let diff = ope::snips(&units, ope::SEED);
    let coin = |p: &[f64]| p.iter().all(|p| (p - 0.5).abs() < COIN);
    let verdict = Verdict::tested(diff.hi <= CALIBRATION, !(coin(grade) && coin(model)));
    Rerun { verdict, model: m.iter().sum::<f64>() / m.len().max(1) as f64, diff }
}

/// P2 over `labels` with the value model fitted by `fit` on the labels before each unit's check; after
/// its FAIL, the pre-registered rerun by the pruned model (`Value::Pruned`), once.
fn calibration<'a>(labels: &[Label], unjoined: usize, fit: &Fitter<'_, 'a>) -> P2 {
    let past = past_counts(labels);
    let ss = sessions(labels);
    let grade: Vec<f64> = labels.iter().zip(&past).map(|(l, c)| by_grade(c, l.grade)).collect();
    let mean = |v: &[f64]| if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 };
    let informed = past.iter().filter(|c| c.iter().flatten().any(|x| x.0 > 0)).count();
    let mut p2 = P2 {
        verdict: Verdict::NotTested,
        why: String::new(),
        labels: labels.len(),
        sessions: ss.iter().collect::<BTreeSet<_>>().len(),
        unjoined,
        informed,
        grade_only: mean(&grade.iter().zip(labels).map(|(p, l)| log_loss(*p, l.y)).collect::<Vec<_>>()),
        model: None,
        diff: None,
        pruned: None,
    };
    if labels.is_empty() {
        p2.why = "no judged unit joins a graded check".into();
        return p2;
    }
    if informed == 0 {
        p2.why = "no judged unit has a label before its check, so neither model learned anything".into();
        return p2;
    }
    let full = match predictions(labels, Value::Full, fit) {
        Ok(p) => compare(labels, &grade, &p, &ss),
        Err(why) => {
            (p2.verdict, p2.why) = (Verdict::NotRun, why);
            return p2;
        }
    };
    (p2.verdict, p2.model, p2.diff) = (full.verdict, Some(full.model), Some(full.diff));
    if p2.verdict == Verdict::NotTested {
        p2.why = "every prediction of both models is 1/2".into();
    }
    if p2.verdict == Verdict::Fail {
        // Pre-registered: prune the model to grade, route and dilution and rerun once; its verdict is P2's.
        let pruned = match predictions(labels, Value::Pruned, fit) {
            Ok(p) => compare(labels, &grade, &p, &ss),
            Err(why) => {
                p2.why = format!("the pruned rerun: {why}");
                return p2;
            }
        };
        p2.verdict = pruned.verdict;
        p2.pruned = Some(pruned);
    }
    p2
}

// --- P5 and the readout ---------------------------------------------------------------------------

/// One policy's doubly robust change against the fixed rule (per judged unit), its own inverse
/// propensity estimate, and how much of its route probability the log covers.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Row {
    pub policy: String,
    pub dr: Estimate,
    pub ips: Estimate,
    pub coverage: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct P5 {
    pub verdict: Verdict,
    pub units: usize,
    /// Units delivered by a route their check gave probability 0: left out.
    pub inconsistent: usize,
    /// The learned policy first, then the fixed rule with one grade's route moved.
    pub rows: Vec<Row>,
}

/// Per unit, a policy's probability of each route (`ASK`, `FREE`, `NOT`).
type Target<'a> = dyn Fn(&Label) -> [f64; ROUTES] + 'a;

/// The fixed rule's route of a unit, as probabilities.
fn fixed_route(checks: &[Check], l: &Label) -> [f64; ROUTES] {
    let (ask, free) = &checks[l.check].fixed;
    let lesson = &checks[l.check].o.lessons[l.j];
    match (ask.contains(lesson), free.contains(lesson)) {
        (true, _) => [1.0, 0.0, 0.0],
        (_, true) => [0.0, 1.0, 0.0],
        _ => [0.0, 0.0, 1.0],
    }
}

/// The DR change of `target` against `base` per unit, its IPS value and coverage: with q̂ the
/// leave-future-out Beta(1, 1) per grade and route, a unit adds Σ_a (π_a − b_a)·(q̂_a + [a = its
/// route]·(y − q̂_a)/μ_a). Session bootstrap, as `ope::snips`.
fn doubly_robust(labels: &[Label], past: &[Counts], ss: &[usize], target: &Target, base: &Target, name: &str) -> Row {
    let (mut dr, mut ips, mut covered) = (Vec::new(), Vec::new(), 0.0);
    for ((l, c), s) in labels.iter().zip(past).zip(ss) {
        let mu = l.mu[l.route];
        if mu <= 0.0 {
            continue;
        }
        let (t, b) = (target(l), base(l));
        let row = &c[usize::from(l.grade).min(GRADES - 1)];
        let q: [f64; ROUTES] = std::array::from_fn(|a| beta(row[a].0, row[a].1));
        let inc: f64 = (0..ROUTES).map(|a| (t[a] - b[a]) * (q[a] + if a == l.route { (l.y - q[a]) / mu } else { 0.0 })).sum();
        dr.push((1.0, inc, *s));
        ips.push((t[l.route] / mu, l.y, *s));
        covered += (0..ROUTES).filter(|&a| l.mu[a] > 0.0).map(|a| t[a]).sum::<f64>();
    }
    let n = dr.len().max(1) as f64;
    Row { policy: name.into(), dr: ope::snips(&dr, ope::SEED), ips: ope::snips(&ips, ope::SEED), coverage: covered / n }
}

/// P5: the learned policy (`targets`, the cold-start replay's propensities per check) and the fixed
/// rule with one grade's asks handed over free, or its free lessons asked, against the fixed rule.
fn off_policy(checks: &[Check], labels: &[Label], targets: &[Propensities]) -> P5 {
    let past = past_counts(labels);
    let ss = sessions(labels);
    let base = |l: &Label| fixed_route(checks, l);
    let learned = |l: &Label| {
        let (p, pf) = targets.get(l.check).map_or((0.0, 0.0), |(p, pf)| (p.get(l.j).copied().unwrap_or(0.0), pf.get(l.j).copied().unwrap_or(0.0)));
        [p, pf, (1.0 - p - pf).max(0.0)]
    };
    let mut rows = vec![doubly_robust(labels, &past, &ss, &learned, &base, "learned")];
    for grade in 1..=3u8 {
        for (from, to, name) in [(ASK, FREE, "free"), (FREE, ASK, "asked")] {
            let moved = |l: &Label| {
                let b = base(l);
                match l.grade == grade && b[from] == 1.0 {
                    true => {
                        let mut m = [0.0; ROUTES];
                        m[to] = 1.0;
                        m
                    }
                    false => b,
                }
            };
            if labels.iter().any(|l| moved(l) != base(l)) {
                rows.push(doubly_robust(labels, &past, &ss, &moved, &base, &format!("fixed, grade {grade} {name}")));
            }
        }
    }
    let units = rows[0].dr.n;
    P5 {
        verdict: if units == 0 { Verdict::NotTested } else { Verdict::Reported },
        units,
        inconsistent: labels.iter().filter(|l| l.mu[l.route] <= 0.0).count(),
        rows,
    }
}

/// One within-check contrast of two routes at one grade: per check with judged units of that grade on
/// both, the difference of their change rates; session bootstrap.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Contrast {
    pub grade: u8,
    pub routes: String,
    pub est: Estimate,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Readout {
    pub due: bool,
    pub randomised: usize,
    pub sessions: usize,
    pub contrasts: Vec<Contrast>,
}

fn readout(checks: &[Check], labels: &[Label]) -> Readout {
    let randomised = labels.iter().filter(|l| l.mu[l.route] > 0.0 && l.mu[l.route] < 1.0).count();
    let learned: BTreeSet<&str> = checks.iter().filter(|c| outcome::policy_of(&c.o) == "learned").map(|c| c.o.session.as_str()).collect();
    let ss = sessions(labels);
    // (grade, check) -> per route (units, changed), and the check's session.
    type Cell = ([(f64, f64); ROUTES], usize);
    let mut cells: BTreeMap<(u8, usize), Cell> = BTreeMap::new();
    for (l, s) in labels.iter().zip(&ss) {
        let e = cells.entry((l.grade, l.check)).or_insert(([(0.0, 0.0); ROUTES], *s));
        e.0[l.route].0 += 1.0;
        e.0[l.route].1 += l.y;
    }
    let mut contrasts = Vec::new();
    for grade in 1..=3u8 {
        for (a, b) in [(ASK, FREE), (FREE, NOT)] {
            let units: Vec<(f64, f64, usize)> = cells
                .range((grade, 0)..=(grade, usize::MAX))
                .filter(|(_, (r, _))| r[a].0 > 0.0 && r[b].0 > 0.0)
                .map(|(_, (r, s))| (1.0, r[a].1 / r[a].0 - r[b].1 / r[b].0, *s))
                .collect();
            contrasts.push(Contrast { grade, routes: format!("{}−{}", ROUTE_NAMES[a], ROUTE_NAMES[b]), est: ope::snips(&units, ope::SEED) });
        }
    }
    Readout { due: randomised >= READOUT_UNITS || learned.len() >= READOUT_SESSIONS, randomised, sessions: learned.len(), contrasts }
}

// --- P4: full-information recall from an evaluation -----------------------------------------------

/// One graded check of an evaluation run: its session, channel, applies (lesson, grade) in the
/// model's order, and what its model call cost.
#[derive(Debug, Clone, PartialEq)]
struct EvalCheck {
    session: String,
    channel: &'static str,
    applies: Vec<(String, u8)>,
    usd: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct EvalRun {
    case: String,
    checks: Vec<EvalCheck>,
}

/// A session's graded checks from its state file `ctx` (`#borg-graded n=<m> <id>:<grade>:<rank>,…`
/// after the call's `#borg-predict-cost`) and its cost log `cost`, whose `live usd=` line before a
/// check's `graded` line marks a live check. Without an aligned cost log, a check followed by a
/// `#borg-live` delivery is live.
fn eval_checks(session: &str, ctx: &str, cost: &str) -> Vec<EvalCheck> {
    let mut live = Vec::new();
    let mut seen = false;
    for l in cost.lines() {
        seen |= l.starts_with("live usd=");
        if l.starts_with("graded applied=") {
            live.push(seen);
            seen = false;
        }
    }
    let lines: Vec<&str> = ctx.lines().collect();
    let graded = lines.iter().filter(|l| l.starts_with("#borg-graded n=")).count();
    let mut out = Vec::new();
    let mut usd = 0.0;
    for (i, l) in lines.iter().enumerate() {
        if let Some(c) = l.strip_prefix("#borg-predict-cost ") {
            usd = c.trim().parse().unwrap_or(0.0);
            continue;
        }
        let Some(rest) = l.strip_prefix("#borg-graded n=") else { continue };
        let applies = rest
            .split_once(' ')
            .map_or("", |(_, items)| items.trim())
            .split(',')
            .filter_map(|item| {
                let mut parts = item.rsplitn(3, ':');
                let (rank, grade, id) = (parts.next()?, parts.next()?, parts.next()?);
                rank.parse::<usize>().ok()?;
                Some((id.to_string(), grade.parse().ok()?))
            })
            .collect();
        let is_live = match live.len() == graded {
            true => live[out.len()],
            false => lines[i + 1..]
                .iter()
                .take_while(|x| !x.starts_with("#borg-graded ") && !x.starts_with("#borg-predict-cost "))
                .any(|x| x.starts_with("#borg-live ")),
        };
        out.push(EvalCheck { session: session.into(), channel: if is_live { "live" } else { "stop" }, applies, usd });
        usd = 0.0;
    }
    out
}

fn sorted_dir(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    v.sort();
    v
}

/// Every run under `dir`: a directory `<case>-s<seed>` holding `borg-state`, one or two levels down
/// (a run set's directory, or a directory of run sets).
fn eval_runs(dir: &Path) -> Vec<EvalRun> {
    let mut found = Vec::new();
    for d in sorted_dir(dir) {
        if d.join("borg-state").is_dir() {
            found.push(d);
        } else {
            found.extend(sorted_dir(&d).into_iter().filter(|r| r.join("borg-state").is_dir()));
        }
    }
    found
        .into_iter()
        .filter_map(|run| {
            let name = run.file_name()?.to_string_lossy().into_owned();
            let case = name.rsplit_once("-s").map_or(name.as_str(), |(c, _)| c).to_string();
            let mut checks = Vec::new();
            for f in sorted_dir(&run.join("borg-state")).into_iter().filter(|f| f.extension().is_some_and(|e| e == "ctx")) {
                let session = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let ctx = std::fs::read_to_string(&f).unwrap_or_default();
                let cost = std::fs::read_to_string(f.with_extension("cost")).unwrap_or_default();
                checks.extend(eval_checks(&session, &ctx, &cost));
            }
            Some(EvalRun { case, checks })
        })
        .collect()
}

/// The lessons the qrels grade above 0, per case.
fn qrels(path: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for v in std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()) {
        if let (Some(case), Some(id)) = (v["case"].as_str(), v["challenge"].as_str())
            && v["grade"].as_f64().unwrap_or(0.0) > 0.0
        {
            out.entry(case.into()).or_default().insert(id.into());
        }
    }
    out
}

/// One policy's right-lesson recall (the share of runs in which a check delivers a right lesson,
/// expected over its draws), its change against the fixed rule (per case, case-paired bootstrap), per
/// check its expected questions, free deliveries and blocks at the stop, and the checks at which a
/// draw of it routes otherwise than the fixed rule: with none it is the fixed rule, and its comparison
/// tests nothing.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Arm {
    pub recall: f64,
    pub delta: Option<Estimate>,
    pub questions: f64,
    pub free: f64,
    pub blocks: f64,
    pub deviations: usize,
    pub pass: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct P4 {
    pub verdict: Verdict,
    pub why: String,
    /// Cases with runs, of the cases the qrels name; runs and graded checks read.
    pub cases: usize,
    pub of: usize,
    pub runs: usize,
    pub checks: usize,
    /// Fewest and most runs (seeds) per case.
    pub seeds: (usize, usize),
    /// What a check's model call cost, on average: the same call under every policy.
    pub usd: f64,
    /// The store's posterior the posterior arm draws from (select/policy.rs `Fit::key`, empty for the
    /// prior) and the labels it read.
    pub fit: String,
    pub labels: u32,
    pub baseline: Option<Arm>,
    pub prior: Option<Arm>,
    pub posterior: Option<Arm>,
}

impl P4 {
    fn not_run(why: &str) -> Self {
        P4 {
            verdict: Verdict::NotRun,
            why: why.into(),
            cases: 0,
            of: 0,
            runs: 0,
            checks: 0,
            seeds: (0, 0),
            usd: 0.0,
            fit: String::new(),
            labels: 0,
            baseline: None,
            prior: None,
            posterior: None,
        }
    }
}

/// One draw of a policy's routes for a check's units, from a seed.
type Draw<'a> = dyn Fn(&[Unit], &EvalCheck, u64) -> Vec<Route> + 'a;

/// What `recalls` measures of one policy on an evaluation's runs.
struct Recalls {
    /// Per run, the chance it delivers a right lesson at some check.
    per_run: Vec<f64>,
    /// Per check, expected questions, free deliveries and blocks at the stop.
    spend: [f64; 3],
    /// Checks at which some draw routes otherwise than the fixed rule.
    deviations: usize,
}

/// The policy `draw` on `runs` (`Recalls`). `draw` None is the fixed rule (select/policy.rs
/// `fixed_routes`: by position, P0's conscience.rs `graded_without` with no cap); otherwise
/// `conscience::P_DRAWS` draws of the policy seeded per check (a stand-in check id from the session and
/// the check's place in it: the run logged none), independent across checks. The evaluation's state
/// lines log no `nofree`: every check is decided with an empty had, as the hook that ran it decided
/// (module doc).
fn recalls(runs: &[EvalRun], right: &BTreeMap<String, BTreeSet<String>>, draw: Option<&Draw>, lessons: &Path) -> Recalls {
    let none = BTreeSet::new();
    let prior = Summary::default();
    let (mut per_run, mut spend, mut n, mut deviations) = (Vec::new(), [0.0; 3], 0.0, 0);
    for run in runs {
        let right = right.get(&run.case).unwrap_or(&none);
        let mut missed = 1.0;
        for (k, c) in run.checks.iter().enumerate() {
            let cs: Vec<Challenge> = c.applies.iter().map(|(id, _)| Challenge { id: id.clone(), ..Default::default() }).collect();
            let applied: Vec<(&Challenge, u8)> = cs.iter().zip(c.applies.iter().map(|(_, g)| *g)).collect();
            let units = super::units(&applied);
            let fixed = policy::fixed_routes(&units, &[]);
            let draws: Vec<Vec<Route>> = match draw {
                None => vec![fixed.clone()],
                Some(draw) => {
                    let check = super::check_id(&c.session, c.channel, k as i64, 0);
                    // Only the seed: the evaluation's state file logs no pool tokens.
                    let seed = super::seed(&Ctx { session: &c.session, check: &check, channel: c.channel, lessons, summary: &prior, tokens: 0, had: &[] });
                    let mut rng = memory::Rng::new(seed);
                    (0..conscience::P_DRAWS).map(|_| draw(&units, c, rng.next_u64())).collect()
                }
            };
            deviations += usize::from(draws.iter().any(|d| *d != fixed));
            let share = 1.0 / draws.len() as f64;
            let mut hit = 0.0;
            for routes in &draws {
                let delivered = |j: usize| matches!(routes.get(j), Some(Route::Ask | Route::Free));
                hit += share * f64::from(u8::from((0..cs.len()).any(|j| delivered(j) && right.contains(&cs[j].id))));
                let asked = routes.iter().filter(|r| **r == Route::Ask).count();
                spend[0] += share * asked as f64;
                spend[1] += share * routes.iter().filter(|r| **r == Route::Free).count() as f64;
                spend[2] += share * f64::from(u8::from(c.channel == "stop" && asked > 0));
            }
            missed *= 1.0 - hit;
            n += 1.0;
        }
        per_run.push(1.0 - missed);
    }
    Recalls { per_run, spend: spend.map(|x| if n > 0.0 { x / n } else { 0.0 }), deviations }
}

/// P4 on the runs under `dir` with the qrels at `qrels_path`: the learned policy, ungated, under the
/// n = 0 prior (select/policy.rs `prior`: its draws are the prior's) and under `posterior`, the store's
/// fit (`evaluate`).
fn recall(dir: &Path, qrels_path: &Path, posterior: &Fit, lessons: &Path) -> P4 {
    let right = qrels(qrels_path);
    let runs = eval_runs(dir);
    if right.is_empty() {
        return P4::not_run(&format!("no qrels in {}", qrels_path.display()));
    }
    if runs.is_empty() {
        return P4::not_run(&format!("no runs with graded lines under {}", dir.display()));
    }
    let prior = policy::prior();
    let p4 = against_baseline(&runs, &right, &|u, c, s| ungated(u, c, &prior, s), &|u, c, s| ungated(u, c, posterior, s), lessons);
    P4 { fit: posterior.key.clone(), labels: posterior.labels, ..p4 }
}

/// P4's comparison of two policies (the prior's and the posterior's) with the fixed rule on `runs`.
fn against_baseline(runs: &[EvalRun], right: &BTreeMap<String, BTreeSet<String>>, prior: &Draw, posterior: &Draw, lessons: &Path) -> P4 {
    let mut per_case: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, r) in runs.iter().enumerate() {
        per_case.entry(r.case.as_str()).or_default().push(i);
    }
    let checks: Vec<&EvalCheck> = runs.iter().flat_map(|r| &r.checks).collect();
    let seeds = per_case.values().map(Vec::len);
    let mut p4 = P4 {
        verdict: Verdict::NotRun,
        why: String::new(),
        cases: right.keys().filter(|c| per_case.contains_key(c.as_str())).count(),
        of: right.len(),
        runs: runs.len(),
        checks: checks.len(),
        seeds: (seeds.clone().min().unwrap_or(0), seeds.max().unwrap_or(0)),
        usd: if checks.is_empty() { 0.0 } else { checks.iter().map(|c| c.usd).sum::<f64>() / checks.len() as f64 },
        fit: String::new(),
        labels: 0,
        baseline: None,
        prior: None,
        posterior: None,
    };
    let base = recalls(runs, right, None, lessons);
    let case_mean = |v: &[f64], idx: &[usize]| idx.iter().map(|&i| v[i]).sum::<f64>() / idx.len() as f64;
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
    let arm = |draw: &Draw| {
        let rec = recalls(runs, right, Some(draw), lessons);
        let units: Vec<(f64, f64, usize)> =
            per_case.values().enumerate().map(|(k, idx)| (1.0, case_mean(&rec.per_run, idx) - case_mean(&base.per_run, idx), k)).collect();
        let delta = ope::snips(&units, ope::SEED);
        let pass = delta.value.is_some_and(|v| v >= -RECALL_MARGIN) && delta.lo > -RECALL_MARGIN;
        let [questions, free, blocks] = rec.spend;
        Arm { recall: mean(&rec.per_run), delta: Some(delta), questions, free, blocks, deviations: rec.deviations, pass }
    };
    let (prior, post) = (arm(prior), arm(posterior));
    // The pre-registered design is every case at the same seeds: a run still going, or a seed that
    // failed, leaves it incomplete.
    p4.verdict = match (p4.cases == p4.of, p4.seeds.0 == p4.seeds.1) {
        (true, true) => {
            let same: Vec<&str> = [("n = 0 prior", &prior), ("store posterior", &post)].iter().filter(|(_, a)| a.deviations == 0).map(|(n, _)| *n).collect();
            let verdict = Verdict::tested(prior.pass && post.pass, same.is_empty());
            if verdict == Verdict::NotTested {
                p4.why = format!("the {} arm decides as the fixed rule at every check, so it tests nothing", same.join(" and the "));
            }
            verdict
        }
        (false, _) => {
            p4.why = format!("runs cover {} of the {} cases the qrels name", p4.cases, p4.of);
            Verdict::NotRun
        }
        (true, false) => {
            p4.why = format!("cases have {} to {} runs: the evaluation is incomplete", p4.seeds.0, p4.seeds.1);
            Verdict::NotRun
        }
    };
    let [questions, free, blocks] = base.spend;
    p4.baseline = Some(Arm { recall: mean(&base.per_run), delta: None, questions, free, blocks, deviations: 0, pass: true });
    p4.prior = Some(prior);
    p4.posterior = Some(post);
    p4
}

// --- The report -----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Checks {
    pub p0: P0,
    pub p1: P1,
    pub p2: P2,
    pub p3: P3,
    pub p4: P4,
    pub p5: P5,
    pub readout: Readout,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub rules: &'static str,
    pub amendment: &'static str,
    /// The policy this process would decide under (select.rs `policy`). The checks evaluate the
    /// learned policy (and P1 each record's logged policy) whatever it is.
    pub policy: &'static str,
    /// The store's selection summary as a hook would load it (select/summary.rs `load`), or rebuilt.
    pub summary: Summary,
    pub records: usize,
    pub graded: usize,
    /// Graded records whose grades are not aligned with their lessons: folded, never replayed.
    pub malformed: usize,
    /// Graded checks per policy (outcome.rs `policy_of`).
    pub policies: BTreeMap<String, usize>,
    pub checks: Option<Checks>,
}

/// What to evaluate: the outcome log's text, the lessons file beside it, the summary to show, whether
/// to run the checks, and an evaluation for P4.
pub struct Input<'a> {
    pub text: &'a str,
    pub lessons: &'a Path,
    pub summary: Summary,
    pub replay: bool,
    pub eval: Option<(&'a Path, &'a Path)>,
}

/// The state and, with `replay`, the checks P0–P5 and the readout.
pub fn evaluate(input: Input) -> Report {
    let log: Vec<Outcome> = lines(input.text).into_iter().filter_map(|l| l.o).collect();
    let mut policies: BTreeMap<String, usize> = BTreeMap::new();
    for o in log.iter().filter(|o| o.kind == outcome::GRADED) {
        *policies.entry(outcome::policy_of(o).to_string()).or_default() += 1;
    }
    let mut report = Report {
        rules: RULES,
        amendment: AMENDMENT,
        policy: super::policy().name(),
        summary: input.summary,
        records: log.len(),
        graded: policies.values().sum(),
        malformed: 0,
        policies,
        checks: None,
    };
    if !input.replay {
        report.malformed = log.iter().filter(|o| o.kind == outcome::GRADED && challenges(o).is_none()).count();
        return report;
    }
    let pass = as_logged(input.text, input.lessons);
    let budget = super::budget_of(input.lessons);
    let (p3, targets) = cold_start(input.text, input.lessons, budget);
    let (labels, unjoined) = labels(&pass.checks, &log);
    // The store's posterior: the whole log fitted as `borg learn` fits it (summary.rs `refit`), never
    // the folded summary's own fit, which a fold leaves at whatever it started from.
    let p4 = match input.eval {
        Some((dir, q)) => recall(dir, q, &policy::fit(&pass.posterior, budget), input.lessons),
        None => P4::not_run("pass --eval RUNS_DIR --qrels FILE"),
    };
    report.malformed = pass.malformed;
    report.checks = Some(Checks {
        p2: calibration(&labels, unjoined, &|value, past, at| model(&pass.checks, past, at, value)),
        p5: off_policy(&pass.checks, &labels, &targets),
        readout: readout(&pass.checks, &labels),
        p0: pass.p0,
        p1: pass.p1,
        p3,
        p4,
    });
    report
}

/// A decisive check (P0–P4) that failed or tested nothing (`Verdict::NotTested`): the command then
/// exits 1. One not run (P4 without an evaluation) is no verdict either way.
pub fn unmet(r: &Report) -> bool {
    r.checks
        .as_ref()
        .is_some_and(|c| [c.p0.verdict, c.p1.verdict, c.p2.verdict, c.p3.verdict, c.p4.verdict].iter().any(|v| matches!(v, Verdict::Fail | Verdict::NotTested)))
}

fn ci(e: &Estimate, scale: f64) -> String {
    match e.value {
        Some(v) => format!("{:+.3} [{:+.3}, {:+.3}]", v * scale, e.lo * scale, e.hi * scale),
        None => "-".into(),
    }
}

fn spend(s: &Spend) -> String {
    let tokens = if s.tokens > 0 { format!("{} pool tokens", s.tokens) } else { "pool tokens not logged".into() };
    let usd = if s.usd > 0.0 { format!(", ${:.2} logged", s.usd) } else { String::new() };
    format!("{} checks, {} questions, {} blocks at the stop, {} free, {tokens}{usd}", s.checks, s.questions, s.blocks, s.free)
}

fn arm(a: &Arm) -> String {
    let delta = a.delta.as_ref().map_or(String::new(), |d| format!(", Δ {} points (case-paired 95% CI)", ci(d, 100.0)));
    format!("recall {:.1}%{delta}; per check {:.2} questions, {:.2} free, {:.2} blocks at the stop", 100.0 * a.recall, a.questions, a.free, a.blocks)
}

/// The report as text: the state, then each check with its verdict against its bar.
pub fn render(r: &Report) -> String {
    let fit = if r.summary.fit.key.is_empty() { "the prior" } else { r.summary.fit.key.as_str() };
    let by: Vec<String> = r.policies.iter().map(|(p, n)| format!("{n} {p}")).collect();
    let mut out = format!(
        "rules {}, amendment {} ({RULES_AT}: pre-registered checks)\npolicy: {}{}; summary v{} at byte {}, fit {fit}\nrecords: {}, graded checks: {} ({}){}\n",
        r.rules,
        r.amendment,
        r.policy,
        if r.checks.is_some() { " here, the checks replay the learned policy" } else { "" },
        r.summary.sv,
        r.summary.at,
        r.records,
        r.graded,
        if by.is_empty() { "none".to_string() } else { by.join(", ") },
        if r.malformed > 0 { format!(", {} malformed", r.malformed) } else { String::new() }
    );
    let Some(c) = &r.checks else {
        out.push_str("run with --replay for the pre-registered checks P0–P5\n");
        return out;
    };
    let list = |out: &mut String, first: &[String]| {
        for f in first {
            out.push_str(&format!("  {f}\n"));
        }
    };
    let empty = |n: usize, tested: &str| if n > 0 { format!("; {n} of them apply no lesson and test {tested}") } else { String::new() };
    out.push_str(&format!(
        "P0 baseline identity: {}: {} of {} checks at gate budget 0 decide as the fixed rule (conscience::graded_without) (bar: every check){}\n",
        c.p0.verdict.label(),
        c.p0.checks - c.p0.differ,
        c.p0.checks,
        empty(c.p0.empty, "nothing")
    ));
    list(&mut out, &c.p0.first);
    out.push_str(&format!(
        "P1 determinism: {}: {} of {} replayed checks reproduce their decision and propensities within {} (max |Δ| {:.4}), {} from the summary their record names{}; {} under an evaluation switch not replayed; incremental fold {} the whole fold\n",
        c.p1.verdict.label(),
        c.p1.reproduced,
        c.p1.replayed,
        ope::AGREE,
        c.p1.max_delta,
        c.p1.seen,
        empty(c.p1.empty, "only the summary and source they name"),
        c.p1.switches,
        if c.p1.fold { "=" } else { "≠" }
    ));
    list(&mut out, &c.p1.first);
    let p2 = &c.p2;
    let by = if p2.verdict == Verdict::Pass && p2.pruned.is_some() { " by the pruned model" } else { "" };
    out.push_str(&format!("P2 calibration: {}{by}", p2.verdict.label()));
    if !p2.why.is_empty() {
        out.push_str(&format!(": {}", p2.why));
    }
    match (&p2.model, &p2.diff) {
        (Some(m), Some(d)) => out.push_str(&format!(
            ": leave-future-out log-loss model {m:.4} vs grade only {:.4}, mean difference {} (bar: upper bound ≤ {CALIBRATION})",
            p2.grade_only,
            ci(d, 1.0)
        )),
        _ => out.push_str(&format!("; grade-only leave-future-out log-loss {:.4}", p2.grade_only)),
    }
    out.push_str(&format!(
        " over {} judged units in {} sessions, {} with a label before their check ({} verdicts join no check)\n",
        p2.labels, p2.sessions, p2.informed, p2.unjoined
    ));
    if let Some(r) = &p2.pruned {
        out.push_str(&format!(
            "  pre-registered rerun, the model pruned to grade, route and dilution: log-loss {:.4} vs grade only {:.4}, mean difference {}: {}{}\n",
            r.model,
            p2.grade_only,
            ci(&r.diff, 1.0),
            r.verdict.label(),
            match r.verdict {
                Verdict::Fail => ": a second miss fails P2",
                Verdict::Pass => ": P2 passes on the pruned model",
                _ => "",
            }
        ));
    }
    let p3 = &c.p3;
    out.push_str(&format!(
        "P3 gate conduct: {}{}: cold-start replay of {} checks ({} refits, one after each session end), {} deviations from the fixed rule, first check {}, {} over the gate's budget; {}\n",
        p3.verdict.label(),
        if p3.why.is_empty() { String::new() } else { format!(" ({})", p3.why) },
        p3.checks,
        p3.refits,
        p3.deviations,
        if p3.cold { "the fixed rule" } else { "not the fixed rule" },
        p3.over,
        match p3.budget {
            Some(b) => format!(
                "the collective's budget of {b} pool tokens per session: the policy's pools spend {:.0} per session, {} it",
                p3.pool,
                if p3.pool <= b { "within" } else { "over" }
            ),
            None => format!("no budget set for the collective: spend reported, not gated; the policy's pools spend {:.0} tokens per session", p3.pool),
        }
    ));
    out.push_str(&format!("  spend, policy: {}\n  spend, fixed rule: {}\n", spend(&p3.policy), spend(&p3.fixed)));
    list(&mut out, &p3.first);
    let p4 = &c.p4;
    out.push_str(&format!("P4 full-information recall: {}", p4.verdict.label()));
    if !p4.why.is_empty() {
        out.push_str(&format!(": {}", p4.why));
    }
    if let (Some(b), Some(pr), Some(po)) = (&p4.baseline, &p4.prior, &p4.posterior) {
        out.push_str(&format!(
            "\n  {} of {} cases, {} runs ({}–{} per case), {} checks, ${:.4} per check (one model call under every policy; bar: Δ ≥ −{} points and lower bound > −{})\n  fixed rule: {}\n  learned, n = 0 prior: {}\n  learned, store posterior (fit {}, {} labels): {}",
            p4.cases,
            p4.of,
            p4.runs,
            p4.seeds.0,
            p4.seeds.1,
            p4.checks,
            p4.usd,
            100.0 * RECALL_MARGIN,
            100.0 * RECALL_MARGIN,
            arm(b),
            arm(pr),
            if p4.fit.is_empty() { "the prior" } else { p4.fit.as_str() },
            p4.labels,
            arm(po)
        ));
    }
    out.push('\n');
    let p5 = &c.p5;
    out.push_str(&format!(
        "P5 off-policy: {}: {} judged units{}; change against the fixed rule per unit (DR, session 95% CI), the policy's own value (SNIPS), coverage\n",
        p5.verdict.label(),
        p5.units,
        if p5.inconsistent > 0 { format!(" ({} delivered by a route logged at p = 0, left out)", p5.inconsistent) } else { String::new() }
    ));
    for row in &p5.rows {
        out.push_str(&format!(
            "  {:<22} DR {:>26}  SNIPS {:>26}  coverage {:>5.1}%  ESS {:.1}\n",
            row.policy,
            ci(&row.dr, 1.0),
            ci(&row.ips, 1.0),
            100.0 * row.coverage,
            row.ips.ess
        ));
    }
    let ro = &c.readout;
    out.push_str(&format!(
        "readout: {} ({} of {READOUT_UNITS} randomised labelled units, {} of {READOUT_SESSIONS} sessions under the learned policy); the policy stays on unless the monitor fired\n",
        if ro.due { "due" } else { "not due" },
        ro.randomised,
        ro.sessions
    ));
    let shown: Vec<&Contrast> = ro.contrasts.iter().filter(|x| x.est.n > 0).collect();
    if shown.is_empty() {
        out.push_str("  no check has judged units of one grade on two routes yet\n");
    }
    for x in shown {
        out.push_str(&format!("  grade {} {}: {} over {} checks (within-check, session 95% CI)\n", x.grade, x.routes, ci(&x.est, 1.0), x.est.n));
    }
    out
}

/// `borg debug select …` (module doc): the store of the collective DIR routes to (the working directory
/// by default), or one outcomes file alone. Exits 1 when a decisive check fails or tests nothing
/// (`unmet`), 2 on bad usage.
pub fn main(args: &[&str]) -> i32 {
    let usage = || {
        eprintln!("usage: borg debug select [--replay] [--rebuild] [--dir DIR | --file OUTCOMES.jsonl] [--json] [--eval RUNS_DIR --qrels QRELS.jsonl]");
        2
    };
    let (mut replay, mut rebuild, mut json) = (false, false, false);
    let (mut dir, mut file, mut eval, mut qrels_path): (Option<&str>, Option<&str>, Option<&str>, Option<&str>) = (None, None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let slot = match *a {
            "--replay" => {
                replay = true;
                continue;
            }
            "--rebuild" => {
                rebuild = true;
                continue;
            }
            "--json" => {
                json = true;
                continue;
            }
            "--dir" if file.is_none() => &mut dir,
            "--file" if dir.is_none() => &mut file,
            "--eval" => &mut eval,
            "--qrels" => &mut qrels_path,
            _ => return usage(),
        };
        match (slot.is_none(), it.next()) {
            (true, Some(v)) => *slot = Some(v),
            _ => return usage(),
        }
    }
    if eval.is_some() != qrels_path.is_some() {
        return usage();
    }
    let (source, lessons): (String, PathBuf) = match file {
        Some(f) => (format!("file {f}"), Path::new(f).with_file_name("lessons.jsonl")),
        None => {
            let cwd = dir.map_or_else(|| std::env::current_dir().unwrap_or_default().display().to_string(), str::to_string);
            match collective::load().map(|cfg| collective::route(cfg.as_ref(), &cwd, &learn::home())) {
                Ok(collective::Route::Collective { name, home, .. }) => (format!("collective {name} ({})", home.display()), home.join("lessons.jsonl")),
                Ok(collective::Route::Off) => {
                    println!("off: {cwd} belongs to no collective; no outcomes are kept here");
                    return 0;
                }
                Err(e) => {
                    eprintln!("borg debug select: invalid collectives config ({e}); everything is off");
                    return 1;
                }
            }
        }
    };
    let outcomes = file.map_or_else(|| outcome::path_beside(&lessons), PathBuf::from);
    let text = std::fs::read_to_string(&outcomes).unwrap_or_default();
    // A file alone has no store whose cache a hook would load: its summary is the file folded.
    let summary = match (file, rebuild) {
        (Some(_), _) => summary::fold(&text, Summary::default()),
        (None, true) => summary::rebuild(&lessons),
        (None, false) => summary::load(&lessons),
    };
    let report = evaluate(Input {
        text: &text,
        lessons: &lessons,
        summary,
        replay: replay || eval.is_some(),
        eval: eval.zip(qrels_path).map(|(e, q)| (Path::new(e), Path::new(q))),
    });
    match json {
        true => println!("{}", serde_json::json!({ "source": source, "report": report })),
        false => print!("{source}\n{}", render(&report)),
    }
    i32::from(unmet(&report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graded(session: &str, ms: i64, lessons: &[&str], grades: &[u8]) -> Outcome {
        let cs: Vec<Challenge> = lessons.iter().map(|id| Challenge { id: id.to_string(), ..Default::default() }).collect();
        let applied: Vec<(&Challenge, u8)> = cs.iter().zip(grades.iter().copied()).collect();
        let (ask, _) = conscience::graded_capped(&applied, None);
        Outcome {
            kind: outcome::GRADED.into(),
            session: session.into(),
            lessons: lessons.iter().map(|l| l.to_string()).collect(),
            ms,
            repo: "r".into(),
            channel: "live".into(),
            grades: grades.to_vec(),
            ranks: (0..lessons.len() as u16).collect(),
            applied: lessons.len() as u16,
            chosen: ids(&ask),
            p: holds(&lessons.iter().map(|l| l.to_string()).collect::<Vec<_>>(), &ids(&ask)),
            ..Default::default()
        }
    }

    fn result(kind: &str, session: &str, lesson: &str, ms: i64, channel: &str) -> Outcome {
        Outcome {
            kind: kind.into(),
            session: session.into(),
            lessons: vec![lesson.into()],
            ms,
            repo: "r".into(),
            channel: channel.into(),
            ..Default::default()
        }
    }

    fn text(log: &[Outcome]) -> String {
        log.iter().map(|o| format!("{}\n", serde_json::to_string(o).unwrap())).collect()
    }

    fn checks_of(log: &[Outcome]) -> Vec<Check> {
        as_logged(&text(log), Path::new("/nonexistent/lessons.jsonl")).checks
    }

    #[test]
    fn a_fixed_log_reproduces_and_a_changed_decision_fails_p0_and_p1() {
        let log = [graded("s", 1, &["A", "B"], &[3, 1]), graded("s", 2, &["C", "D"], &[2, 1]), result(outcome::CHANGED, "s", "A", 3, "live")];
        let pass = as_logged(&text(&log), Path::new("/nonexistent/lessons.jsonl"));
        assert_eq!((pass.p0.verdict, pass.p0.checks, pass.p0.differ), (Verdict::Pass, 2, 0));
        assert_eq!((pass.p1.verdict, pass.p1.replayed, pass.p1.reproduced, pass.p1.max_delta), (Verdict::Pass, 2, 2, 0.0));
        assert!(pass.p1.fold);
        // The second check logged only one of its free lessons: the fixed rule hands over both.
        let mut other = log.clone();
        other[1].policy = "fixed".into();
        other[1].free = vec!["C".into()];
        other[1].pf = vec![1.0, 0.0];
        let pass = as_logged(&text(&other), Path::new("/nonexistent/lessons.jsonl"));
        assert_eq!((pass.p0.verdict, pass.p0.differ), (Verdict::Fail, 1), "{:?}", pass.p0.first);
        assert_eq!((pass.p1.verdict, pass.p1.reproduced), (Verdict::Fail, 1));
        assert!(pass.p1.max_delta >= 1.0 && pass.p1.first[0].contains("check - ms 2"), "{:?}", pass.p1);
        // Under an evaluation switch: counted, not replayed.
        let mut cut = log.clone();
        cut[0].policy = "cap".into();
        let pass = as_logged(&text(&cut), Path::new("/nonexistent/lessons.jsonl"));
        assert_eq!((pass.p1.replayed, pass.p1.switches), (1, 1));
    }

    /// P0 and P1 are NOT TESTED, never a pass, when no check they replay applies a lesson: every policy
    /// decides nothing on a check none applied to or a failed call, so the comparison cannot fail on
    /// the policy. One check with an apply tests both; P1 alone is NOT TESTED when every check ran under
    /// an evaluation switch.
    #[test]
    fn p0_and_p1_on_checks_that_decide_nothing_are_not_tested() {
        let none = Path::new("/nonexistent/lessons.jsonl");
        let pass = as_logged(&text(&[result(outcome::CHANGED, "s", "A", 1, "live")]), none);
        assert_eq!((pass.p0.verdict, pass.p0.checks, pass.p1.verdict, pass.p1.replayed), (Verdict::NotTested, 0, Verdict::NotTested, 0));
        // A check none applied to, and a failed learned call at the stop.
        let failed = Outcome {
            failed: true,
            policy: "learned".into(),
            check: crate::select::check_id("s", "stop", 3, 1),
            channel: "stop".into(),
            ..graded("s", 3, &[], &[])
        };
        let mut log = vec![graded("s", 2, &[], &[]), failed, result(outcome::CHANGED, "s", "A", 4, "live")];
        let pass = as_logged(&text(&log), none);
        assert_eq!((pass.p0.verdict, pass.p0.checks, pass.p0.empty, pass.p0.differ), (Verdict::NotTested, 2, 2, 0), "{:?}", pass.p0);
        assert_eq!((pass.p1.verdict, pass.p1.replayed, pass.p1.reproduced, pass.p1.empty), (Verdict::NotTested, 2, 2, 2), "{:?}", pass.p1);
        // The command fails on P0 or P1 NOT TESTED alone, the other checks passing.
        let mut report = evaluate(Input { text: &text(&log), lessons: none, summary: Summary::default(), replay: true, eval: None });
        let c = report.checks.as_mut().unwrap();
        assert_eq!((c.p0.verdict, c.p1.verdict), (Verdict::NotTested, Verdict::NotTested));
        (c.p1.verdict, c.p2.verdict, c.p3.verdict, c.p4.verdict) = (Verdict::Pass, Verdict::Pass, Verdict::Pass, Verdict::NotRun);
        assert!(unmet(&report));
        // One check with an apply: both tested, the others still counted and named.
        log.push(graded("s", 5, &["A", "B"], &[3, 1]));
        let pass = as_logged(&text(&log), none);
        assert_eq!((pass.p0.verdict, pass.p0.checks, pass.p0.empty), (Verdict::Pass, 3, 2), "{:?}", pass.p0);
        assert_eq!((pass.p1.verdict, pass.p1.replayed, pass.p1.empty), (Verdict::Pass, 3, 2), "{:?}", pass.p1);
        let shown = render(&evaluate(Input { text: &text(&log), lessons: none, summary: Summary::default(), replay: true, eval: None }));
        assert!(shown.contains("(bar: every check); 2 of them apply no lesson and test nothing\n"), "{shown}");
        assert!(shown.contains("their record names; 2 of them apply no lesson and test only the summary and source they name;"), "{shown}");
        // Every check under an evaluation switch: P0 decides them, P1 replays none.
        let cut = [Outcome { policy: "cap".into(), ..graded("s", 6, &["A"], &[2]) }];
        let pass = as_logged(&text(&cut), none);
        assert_eq!((pass.p0.verdict, pass.p0.checks), (Verdict::Pass, 1), "{:?}", pass.p0);
        assert_eq!((pass.p1.verdict, pass.p1.replayed, pass.p1.switches), (Verdict::NotTested, 0, 1), "{:?}", pass.p1);
    }

    /// P1 replays a learned decision from the summary its record names (`seen`): the hook read it
    /// before its model call, so a record another check appended meanwhile lies before this one but not
    /// in it, and that record's entry on the gate's ledger must not count. The revert it logged
    /// (`source`) counts too: the sticky mark beside the store is in no log line.
    #[test]
    fn p1_replays_each_decision_from_the_summary_its_record_names() {
        let none = Path::new("/nonexistent/lessons.jsonl");
        let mut log = labelled(6, 12, 11);
        let fitted = summary::with_fit(summary::fold(&text(&log), Summary::default()), None);
        assert!(fitted.fit.model.is_some() && fitted.gate.allowance == 0.0, "a model, and no learned check on its ledger: {:?}", fitted.gate);
        // A hook's decision on the summary `s` it read, logged as hook.rs `record_graded` logs it.
        let decide = |o: Outcome, s: &Summary| {
            let cs = challenges(&o).unwrap();
            let r = crate::select::graded_under(Policy::Learned, &applied(&cs, &o), &ctx(&o, &had_of(&o), none, s));
            let round = |v: &[f64]| v.iter().map(|p| outcome::round_p(*p)).collect();
            Outcome {
                chosen: ids(&r.ask),
                free: ids(&r.free),
                placebo: ids(&r.placebo),
                p: round(&r.p),
                pf: round(&r.pf),
                psi: r.log.psi,
                policy: r.log.policy.into(),
                fit: r.log.fit.clone(),
                seen: s.key(),
                source: r.log.source.into(),
                ..o
            }
        };
        let check =
            |ms: i64, lessons: &[&str], grades: &[u8]| Outcome { check: crate::select::check_id("t", "live", ms, 1), ..graded("t", ms, lessons, grades) };
        // x and b read the refit; a reads it after x; then x's, a's and b's records land in that order.
        let x = decide(check(100_001, &["L1", "L2", "L3"], &[2, 1, 2]), &fitted);
        let after_x = summary::fold(&text(std::slice::from_ref(&x)), fitted.clone());
        assert!(after_x.gate.allowance > 0.0, "x's check gives the gate a budget b never read: {:?}", after_x.gate);
        let a = decide(check(100_002, &["L4", "L5"], &[1, 2]), &after_x);
        let b = decide(check(100_003, &["L6", "L7", "L8"], &[1, 2, 1]), &fitted);
        assert_eq!((a.seen != b.seen, b.source.as_str()), (true, "fixed"));
        // c reads all three, the store reverted by the mark beside it.
        let mut reverted = summary::fold(&text(&[x.clone(), a.clone(), b.clone()]), fitted.clone());
        reverted.monitor.reverted = true;
        let c = decide(check(100_004, &["L9", "L10"], &[2, 2]), &reverted);
        assert_eq!(c.source, crate::select::REVERTED_SOURCE);
        log.extend([x, a, b, c]);
        let pass = as_logged(&text(&log), none);
        assert_eq!((pass.p1.verdict, pass.p1.reproduced, pass.p1.seen), (Verdict::Pass, pass.p1.replayed, 4), "{:?}", pass.p1.first);
        // Replayed on every line before it instead, b's decision is not the one its hook made (c goes:
        // b's shorter line would move the byte c's summary names).
        log.pop();
        let b_at = log.len() - 1;
        log[b_at].seen.clear();
        let pass = as_logged(&text(&log), none);
        assert_eq!((pass.p1.verdict, pass.p1.reproduced + 1), (Verdict::Fail, pass.p1.replayed), "{:?}", pass.p1.first);
        assert!(pass.p1.first[0].contains("ms 100003") && pass.p1.first[0].contains("by fixed"), "{:?}", pass.p1.first);
        // A summary the log does not hold at the byte it names is not replayed on another.
        log[b_at].seen = format!("{}-00000000", log[b_at].fit.split('-').next().unwrap());
        let pass = as_logged(&text(&log), none);
        assert!(pass.p1.first.iter().any(|m| m.contains("ms 100003") && m.contains("no summary")), "{:?}", pass.p1.first);
    }

    #[test]
    fn verdicts_join_the_check_that_routed_their_delivery() {
        let mut log = vec![
            graded("s", 10, &["A", "B"], &[3, 1]), // asks A and B
            graded("s", 20, &["A", "C"], &[2, 1]), // hands A and C over free
            result(outcome::CHANGED, "s", "A", 25, "live"),
            result(outcome::CHANGED, "s", "A", 26, "queued"),
            result(outcome::IGNORED, "s", "C", 27, "queued"),
            result(outcome::CHANGED, "s", "B", 28, "early"),
            result(outcome::NOT_CORRECTED, "s", "B", 29, ""),
            result(outcome::CHANGED, "t", "Z", 30, "live"),
        ];
        let checks = checks_of(&log);
        let (ls, unjoined) = labels(&checks, &log);
        let got: Vec<(usize, usize, usize, f64)> = ls.iter().map(|l| (l.check, l.j, l.route, l.y)).collect();
        assert_eq!(got, [(0, 0, ASK, 1.0), (1, 0, FREE, 1.0), (1, 1, FREE, 0.0)], "early and censored results label nothing");
        assert_eq!(unjoined, 1, "Z was graded by no check");
        assert_eq!(ls[1].mu, [0.0, 1.0, 0.0]);
        // A veto after the check outranks the change; one before it does not.
        log.push(result(outcome::VETOED, "s", "A", 21, ""));
        let (ls, _) = labels(&checks, &log);
        assert_eq!(ls.iter().map(|l| l.y).collect::<Vec<_>>(), [0.0, 0.0, 0.0]);
        log.pop();
        log.push(result(outcome::VETOED, "s", "A", 5, ""));
        assert_eq!(labels(&checks, &log).0[0].y, 1.0);
        // A check id joins whatever the times.
        let mut a = graded("s", 100, &["Q"], &[3]);
        a.check = "abcdef012345".into();
        let late = Outcome { check: "abcdef012345".into(), ..result(outcome::CHANGED, "s", "Q", 50, "stop") };
        let log = [a, late];
        assert_eq!(labels(&checks_of(&log), &log).0.len(), 1);
        // The fixed rule's placebo: a leftover judged blindly joins its check as not delivered, at the
        // fixed rule's route probabilities; a verdict on the route it did not take joins nothing.
        let lessons: Vec<String> = (0..11).map(|i| format!("L{i}")).collect();
        let mut f = graded("s", 200, &lessons.iter().map(String::as_str).collect::<Vec<_>>(), &[1; 11]);
        (f.check, f.policy, f.placebo, f.psi) = ("0123456789ab".into(), "fixed".into(), vec!["L10".into()], 0.5);
        f.free = lessons[..10].to_vec();
        f.pf = holds(&f.lessons, &f.free);
        let blind = Outcome { check: "0123456789ab".into(), ..result(outcome::CHANGED, "s", "L10", 210, outcome::PLACEBO) };
        let wrong = Outcome { check: "0123456789ab".into(), ..result(outcome::CHANGED, "s", "L10", 211, "queued") };
        let log = [f, blind, wrong];
        let (ls, unjoined) = labels(&checks_of(&log), &log);
        assert_eq!((ls.len(), unjoined), (1, 1));
        // Its chance of a label is the draw's: routed nowhere (1) and drawn as a placebo (ψ).
        assert_eq!((ls[0].j, ls[0].route, ls[0].mu, ls[0].y), (10, NOT, [0.0, 0.0, 0.5], 1.0));
    }

    /// Each unit is predicted from the results before its check: a result arriving later moves no
    /// earlier prediction, one before it does.
    #[test]
    fn predictions_are_leak_free() {
        let unit = |check_ms: i64, result_ms: i64, y: f64| Label {
            check: 0,
            j: 0,
            grade: 1,
            route: ASK,
            mu: [1.0, 0.0, 0.0],
            y,
            session: "s".into(),
            check_ms,
            known: vec![(result_ms, y)],
            judge: 2,
        };
        let early = vec![unit(10, 50, 1.0), unit(20, 60, 1.0)];
        let p = |ls: &[Label]| past_counts(ls).iter().zip(ls).map(|(c, l)| by_grade(c, l.grade)).collect::<Vec<_>>();
        assert_eq!(p(&early), [0.5, 0.5], "no result before either check");
        let mut later = early.clone();
        later.push(unit(100, 110, 1.0));
        assert_eq!(&p(&later)[..2], &[0.5, 0.5], "the future moves nothing");
        assert_eq!(p(&later)[2], beta(2, 2), "both results came before it");
        let mut before = early.clone();
        before.push(unit(1, 5, 1.0));
        assert_eq!(&p(&before)[..2], &[beta(1, 1), beta(1, 1)], "a result before both checks moves both");
    }

    fn world(n: usize, free_better: bool) -> Vec<Label> {
        // Grade-1 units, randomised: asked or free with probability 1/2, in 2-unit checks per session.
        (0..n)
            .map(|i| {
                let route = if i % 2 == 0 { ASK } else { FREE };
                let changed = if route == FREE { i % 10 != 1 } else { i % 10 == 0 };
                let y = f64::from(u8::from(changed == free_better));
                Label {
                    check: i / 2,
                    j: i % 2,
                    grade: 1,
                    route,
                    mu: [0.5, 0.5, 0.0],
                    y,
                    session: format!("s{}", i / 4),
                    check_ms: 10 * (i as i64 / 2),
                    known: vec![(10 * (i as i64 / 2) + 5, y)],
                    judge: 2,
                }
            })
            .collect()
    }

    /// A predictor that ignores what it was fitted on.
    fn fixed_p<'a>(f: impl Fn(&Label) -> f64 + 'a) -> Option<Predictor<'a>> {
        Some(Box::new(f))
    }

    /// Knowing the route, in the world where free changes and asked does not.
    fn by_route(l: &Label) -> f64 {
        if l.route == FREE { 0.85 } else { 0.15 }
    }

    /// The route backwards.
    fn backwards(l: &Label) -> f64 {
        1.0 - by_route(l)
    }

    #[test]
    fn calibration_passes_a_model_no_worse_than_grade_only_and_fails_a_worse_one() {
        let ls = world(80, true);
        let none = calibration(&ls, 0, &|_, _, _| None);
        assert_eq!((none.verdict, none.labels, none.sessions), (Verdict::NotRun, 80, 20));
        assert!(none.why.contains("no value model") && none.grade_only > 0.0);
        // The grade-only model itself: difference 0. Every check of this world has one change and one
        // not, so grade only says 1/2 throughout, and so does its copy: not tested.
        let grade_only = |_, past: &[&Label], at| {
            let mut c: Counts = [[(0, 0); ROUTES]; GRADES];
            for (l, (_, y)) in past.iter().filter_map(|l| l.at(at).map(|k| (l, k))) {
                c[usize::from(l.grade)][l.route].0 += 1;
                c[usize::from(l.grade)][l.route].1 += u32::from(y > 0.5);
            }
            fixed_p(move |l: &Label| by_grade(&c, l.grade))
        };
        assert_eq!(calibration(&ls, 0, &grade_only).verdict, Verdict::NotTested);
        // Two changes in three: grade only learns, and its copy is no worse.
        let tilted: Vec<Label> = ls
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let y = f64::from(u8::from(i % 3 != 0));
                Label { y, known: vec![(l.known[0].0, y)], ..l.clone() }
            })
            .collect();
        let same = calibration(&tilted, 0, &grade_only);
        assert_eq!(same.verdict, Verdict::Pass, "{same:?}");
        assert!(same.diff.as_ref().is_some_and(|d| d.value == Some(0.0)) && same.pruned.is_none(), "a pass is not rerun");
        let route = calibration(&ls, 0, &|_, _, _| fixed_p(by_route));
        assert_eq!(route.verdict, Verdict::Pass);
        assert!(route.diff.as_ref().is_some_and(|d| d.hi < 0.0), "{route:?}");
        // The route backwards is worse, and P2 says so; the pruned rerun misses again.
        let wrong = calibration(&ls, 0, &|_, _, _| fixed_p(backwards));
        assert_eq!(wrong.verdict, Verdict::Fail, "{wrong:?}");
        assert!(wrong.pruned.as_ref().is_some_and(|r| r.verdict == Verdict::Fail), "a second miss fails P2: {wrong:?}");
    }

    /// P2's pre-registered rerun: after the value model's FAIL, the pruned model is fitted and compared
    /// once, and its verdict is P2's; the value model's numbers stay reported beside it.
    #[test]
    fn a_failed_calibration_reruns_once_with_the_pruned_model() {
        let ls = world(80, true);
        let asked = std::cell::RefCell::new(Vec::new());
        let p2 = calibration(&ls, 0, &|value, _, _| {
            asked.borrow_mut().push(value);
            if value == Value::Full { fixed_p(backwards) } else { fixed_p(by_route) }
        });
        assert_eq!((p2.verdict, p2.pruned.as_ref().map(|r| r.verdict)), (Verdict::Pass, Some(Verdict::Pass)), "{p2:?}");
        assert!(p2.diff.as_ref().is_some_and(|d| d.hi > CALIBRATION), "the full model's miss is kept: {p2:?}");
        assert!(p2.pruned.as_ref().is_some_and(|r| r.diff.hi < 0.0 && r.model < p2.grade_only), "{p2:?}");
        let asked = asked.into_inner();
        let first_pruned = asked.iter().position(|v| *v == Value::Pruned);
        assert!(
            first_pruned.is_some_and(|i| asked[..i].iter().all(|v| *v == Value::Full) && asked[i..].iter().all(|v| *v == Value::Pruned)),
            "the full model, then the rerun once: {asked:?}"
        );
        let text = render(&Report {
            rules: RULES,
            amendment: AMENDMENT,
            policy: "fixed",
            summary: Summary::default(),
            records: 0,
            graded: 0,
            malformed: 0,
            policies: BTreeMap::new(),
            checks: Some(Checks {
                p0: P0 { verdict: Verdict::Pass, checks: 1, empty: 0, differ: 0, first: Vec::new() },
                p1: P1 { verdict: Verdict::Pass, replayed: 1, reproduced: 1, empty: 0, seen: 0, switches: 0, max_delta: 0.0, fold: true, first: Vec::new() },
                p2,
                p3: conduct(&[], Spend::default(), Spend::default(), None, 0.0),
                p4: P4::not_run("x"),
                p5: off_policy(&[], &[], &[]),
                readout: readout(&[], &[]),
            }),
        });
        assert!(text.contains("P2 calibration: PASS by the pruned model: leave-future-out log-loss model "), "{text}");
        assert!(
            text.contains("  pre-registered rerun, the model pruned to grade, route and dilution: log-loss ")
                && text.contains(": PASS: P2 passes on the pruned model"),
            "{text}"
        );
    }

    /// P5 estimates nothing without a judged unit: NOT TESTED, not an estimate of 0 units. One judged
    /// unit makes it an estimate (REPORTED).
    #[test]
    fn off_policy_without_a_judged_unit_is_not_tested() {
        let p5 = off_policy(&[], &[], &[]);
        assert_eq!((p5.verdict, p5.units), (Verdict::NotTested, 0), "{p5:?}");
        let log = [graded("s", 10, &["A", "B"], &[3, 1]), result(outcome::CHANGED, "s", "A", 25, "live")];
        let checks = checks_of(&log);
        let (ls, _) = labels(&checks, &log);
        let p5 = off_policy(&checks, &ls, &[]);
        assert_eq!((p5.verdict, p5.units), (Verdict::Reported, 1), "{p5:?}");
    }

    /// P2 is NOT TESTED, never a pass, when it tested nothing: no label, no unit with a label before
    /// its check, or every prediction of both models 1/2.
    #[test]
    fn calibration_on_nothing_to_learn_from_is_not_tested() {
        let empty = calibration(&[], 0, &|_, _, _| fixed_p(by_route));
        assert_eq!((empty.verdict, empty.why.as_str()), (Verdict::NotTested, "no judged unit joins a graded check"));
        // Every result after every check.
        let late: Vec<Label> = world(20, true).into_iter().map(|l| Label { known: vec![(10_000, l.y)], ..l }).collect();
        let blind = calibration(&late, 0, &|_, _, _| fixed_p(by_route));
        assert_eq!((blind.verdict, blind.informed), (Verdict::NotTested, 0), "{blind:?}");
        // One change and one not per check: grade only says 1/2 wherever it knows anything, and so does
        // the model.
        let balanced: Vec<Label> = world(80, true)
            .into_iter()
            .enumerate()
            .map(|(i, l)| {
                let y = f64::from(u8::from(i % 2 == 0));
                Label { y, known: vec![(l.known[0].0, y)], ..l }
            })
            .collect();
        let coin = calibration(&balanced, 0, &|_, _, _| fixed_p(|_| 0.5));
        assert!(coin.informed > 0 && coin.model.is_some(), "{coin:?}");
        assert_eq!((coin.verdict, coin.why.as_str()), (Verdict::NotTested, "every prediction of both models is 1/2"), "{coin:?}");
        // The same units under an informative model are tested.
        assert_ne!(calibration(&balanced, 0, &|_, _, _| fixed_p(by_route)).verdict, Verdict::NotTested);
    }

    /// The guard: a model certain of the wrong answer costs a large but finite log-loss, so the mean
    /// and its interval stay numbers. Without `CLAMP` this unit's loss is infinite.
    #[test]
    fn a_certain_wrong_prediction_keeps_the_log_loss_finite() {
        assert!(log_loss(1.0, 0.0).is_finite() && log_loss(0.0, 1.0).is_finite());
        let certain = calibration(&world(40, true), 0, &|_, _, _| fixed_p(|l: &Label| if l.route == FREE { 1.0 } else { 0.0 }));
        assert!(certain.model.is_some_and(f64::is_finite), "{certain:?}");
        assert!(certain.diff.as_ref().is_some_and(|d| d.hi.is_finite()), "{certain:?}");
        assert_eq!(certain.verdict, Verdict::Fail, "one certain miss outweighs every right call");
    }

    /// Where free beats asked for grade 1 (randomised logging), DR ranks a policy that hands grade 1
    /// over free above the fixed rule, which asks it, and the readout's within-check Q−F says the same.
    #[test]
    fn a_planted_world_where_free_beats_asked_ranks_free_above_asked() {
        let ls = world(120, true);
        let past = past_counts(&ls);
        let ss = sessions(&ls);
        let asks = |_: &Label| [1.0, 0.0, 0.0];
        let frees = |_: &Label| [0.0, 1.0, 0.0];
        let row = doubly_robust(&ls, &past, &ss, &frees, &asks, "free");
        let d = row.dr.value.unwrap();
        assert!(d > 0.5 && row.dr.lo > 0.0, "{row:?}");
        assert_eq!(row.coverage, 1.0, "randomised: the log covers every route the policy takes");
        let back = doubly_robust(&ls, &past, &ss, &asks, &frees, "asked");
        assert!(back.dr.value.unwrap() < -0.5 && back.dr.hi < 0.0, "{back:?}");
        // The mirror world reverses the ranking.
        let mirror = world(120, false);
        let row = doubly_robust(&mirror, &past_counts(&mirror), &sessions(&mirror), &frees, &asks, "free");
        assert!(row.dr.hi < 0.0, "{row:?}");
        // Deterministic logging (every unit asked): the free policy's route is not covered.
        let fixed: Vec<Label> = ls.iter().map(|l| Label { route: ASK, mu: [1.0, 0.0, 0.0], ..l.clone() }).collect();
        assert_eq!(doubly_robust(&fixed, &past_counts(&fixed), &sessions(&fixed), &frees, &asks, "free").coverage, 0.0);
    }

    /// The guard: no decision may risk more than the gate's budget. Removing the `risk > gate` test
    /// from `conduct` turns this red.
    #[test]
    fn a_decision_risking_more_than_its_budget_fails_p3() {
        let step = |deviates: bool, gate: f64, risk: f64| Step { deviates, gate, risk, at: "x".into() };
        let ok = conduct(&[step(false, 0.0, 0.0), step(true, 0.5, 0.25)], Spend::default(), Spend::default(), None, 0.0);
        assert_eq!((ok.verdict, ok.deviations, ok.over, ok.cold), (Verdict::Pass, 1, 0, true));
        let over = conduct(&[step(false, 0.0, 0.0), step(true, 0.5, 0.75)], Spend::default(), Spend::default(), None, 0.0);
        assert_eq!((over.verdict, over.over), (Verdict::Fail, 1));
        let stated = |r: f64| Routed { log: crate::select::Log { risk: r, ..crate::select::Log::default() }, ..empty() };
        let logged = conduct(&[step(false, 0.0, 0.0), step(true, 0.5, risk(&stated(0.75), true))], Spend::default(), Spend::default(), None, 0.0);
        assert_eq!(logged.verdict, Verdict::Fail, "the decision's own risk is what P3 compares");
        assert_eq!(risk(&stated(0.75), false), 0.0, "keeping the fixed rule risks nothing");
        let warm = conduct(&[step(true, 0.0, 0.0)], Spend::default(), Spend::default(), None, 0.0);
        assert_eq!((warm.verdict, warm.cold), (Verdict::Fail, false), "cold start is the fixed rule");
        // No deviation puts the gate to no test: NOT TESTED; a failure still fails.
        let none = conduct(&[], Spend::default(), Spend::default(), None, 0.0);
        assert_eq!((none.verdict, none.why.as_str()), (Verdict::NotTested, "no graded check to replay"));
        let kept = conduct(&[step(false, 0.0, 0.0), step(false, 0.5, 0.0)], Spend::default(), Spend::default(), None, 0.0);
        assert_eq!(kept.verdict, Verdict::NotTested, "{kept:?}");
        assert!(kept.why.contains("no decision left the fixed rule"), "{}", kept.why);
        let bad = conduct(&[step(false, 0.0, 0.0), step(false, -1.0, 0.0)], Spend::default(), Spend::default(), None, 0.0);
        assert_eq!(bad.verdict, Verdict::Fail, "a negative budget fails without a deviation");
    }

    /// The guard: with an engineer's budget set, the replayed pools must spend at most it per session.
    /// Removing the budget clause from `conduct` turns this red; without a budget, spend only shows.
    #[test]
    fn pools_spending_past_the_engineers_budget_fail_p3() {
        let fine = [Step { deviates: false, gate: 0.0, risk: 0.0, at: "x".into() }, Step { deviates: true, gate: 0.5, risk: 0.25, at: "y".into() }];
        let p3 = |budget: Option<f64>| conduct(&fine, Spend::default(), Spend::default(), budget, 24_000.0);
        assert_eq!(p3(None).verdict, Verdict::Pass, "no budget: reported, not gated");
        assert_eq!(p3(Some(24_000.0)).verdict, Verdict::Pass, "at the budget is within it");
        let over = p3(Some(20_000.0));
        assert_eq!((over.verdict, over.budget, over.pool), (Verdict::Fail, Some(20_000.0), 24_000.0));
        assert!(over.first[0].contains("over the collective's budget of 20000"), "{:?}", over.first);
        // Replayed: a budget below what the baseline's pools spend in a session fails while the gate keeps
        // the baseline's pool; one above it is met.
        let log = text(&labelled(3, 4, 13));
        let none = Path::new("/nonexistent/lessons.jsonl");
        let (tight, _) = cold_start(&log, none, Some(1.0));
        assert_eq!((tight.verdict, tight.budget), (Verdict::Fail, Some(1.0)), "{tight:?}");
        assert!(tight.pool > 1.0 && tight.pool <= 4.0 * crate::whole::TOKENS as f64, "four checks a session, none past the baseline's pool: {tight:?}");
        let (loose, _) = cold_start(&log, none, Some(1e12));
        // Met; with no deviation on this short log the gate itself is not tested.
        assert_eq!((loose.verdict, loose.first.len()), (Verdict::NotTested, 0), "{loose:?}");
    }

    fn empty() -> Routed<'static> {
        Routed { ask: Vec::new(), free: Vec::new(), placebo: Vec::new(), p: Vec::new(), pf: Vec::new(), log: crate::select::Log::default() }
    }

    #[test]
    fn the_cold_start_replay_of_a_fixed_log_is_the_fixed_rule_and_counts_spend() {
        let mut stop = graded("s", 1, &["A", "B"], &[3, 1]);
        stop.channel = "stop".into();
        stop.tokens = 1200;
        let log = [stop, graded("s", 2, &["C", "D"], &[2, 1]), graded("t", 3, &["E"], &[3])];
        let (p3, targets) = cold_start(&text(&log), Path::new("/nonexistent/lessons.jsonl"), None);
        assert_eq!((p3.verdict, p3.checks, p3.deviations, p3.cold, p3.budget), (Verdict::NotTested, 3, 0, true, None), "no fit, no deviation: {p3:?}");
        assert_eq!(p3.policy, Spend { checks: 3, questions: 3, blocks: 1, free: 2, tokens: 1200, usd: 0.0 });
        assert_eq!(p3.policy, p3.fixed);
        assert_eq!(targets[1], (vec![0.0, 0.0], vec![1.0, 1.0]));
    }

    /// A record logs the applies its session already had (`nofree`), and every pass decides its check
    /// under them as the hook did: such a lesson is never handed over free, by the fixed rule (P0's
    /// reference and the logged decision), by the replayed policy (P1), or in the cold start (P3's spend).
    #[test]
    fn every_pass_replays_a_check_under_the_lessons_its_session_had() {
        let none = Path::new("/nonexistent/lessons.jsonl");
        // A live check without a 3, its session already had A: the fixed rule hands over only B.
        let mut o = graded("s", 1, &["A", "B"], &[2, 1]);
        o.check = crate::select::check_id("s", "live", 1, 1);
        o.policy = "fixed".into();
        o.free = vec!["B".into()];
        o.pf = vec![0.0, 1.0];
        o.nofree = vec!["A".into()];
        let pass = as_logged(&text(std::slice::from_ref(&o)), none);
        assert_eq!(pass.checks[0].fixed, (Vec::<String>::new(), vec!["B".to_string()]), "the fixed rule for a session that had A");
        assert_eq!((pass.p0.verdict, pass.p1.verdict), (Verdict::Pass, Verdict::Pass), "{:?} {:?}", pass.p0.first, pass.p1.first);
        let (p3, targets) = cold_start(&text(std::slice::from_ref(&o)), none, None);
        assert_eq!((p3.policy.free, p3.fixed.free, p3.deviations), (1, 1, 0), "{p3:?}");
        assert_eq!(targets[0], (vec![0.0, 0.0], vec![0.0, 1.0]));
        // The same decision without its `nofree` is no fixed-rule decision: the rule would hand A over too.
        o.nofree.clear();
        let pass = as_logged(&text(std::slice::from_ref(&o)), none);
        assert_eq!((pass.p0.verdict, pass.p1.verdict), (Verdict::Fail, Verdict::Fail));
    }

    /// The guard: the replay's pools are chosen under the loosest latency limit, as `price` replays
    /// them, so a pool a hook's limit would allow above the baseline counts against the engineer's
    /// budget. Under no limit (None) every pool above the baseline is forbidden and P3 would understate
    /// the spend.
    #[test]
    fn the_replays_pools_may_grow_where_a_hooks_limit_would_let_them() {
        let pool = policy::PoolFit {
            yields: vec![(50.0, 1.0); 8],
            per_lesson: 400.0,
            latency: (1000.0, 0.1, 500.0),
            latency_n: 30,
            eta: 0.0,
            slope: (0.0, 0.0),
            not: (-3.0, 0.0),
        };
        let gate = summary::Gate { gain: vec![0.0; policy::DRAWS], allowance: 1.0, ..summary::Gate::default() };
        let s = Summary { fit: Fit { pool, ..Fit::default() }, gate, ..Summary::default() };
        let baseline = crate::whole::TOKENS;
        assert!(pool_of(&s, "x") > baseline, "a larger pool is worth more and its latency fits: {}", pool_of(&s, "x"));
        assert_eq!(crate::select::pool_tokens_under(Policy::Learned, &s, None, "x"), baseline, "without a limit, never above the baseline");
        // Closed gate: the baseline, as in production.
        let closed = Summary { gate: summary::Gate::default(), ..s.clone() };
        assert_eq!(pool_of(&closed, "x"), baseline);
    }

    /// The cold start folds the replayed decision as the hook would have logged it: its own routes,
    /// propensities, placebo chance and source, on the logged check's applies and had lessons.
    #[test]
    fn the_cold_start_folds_the_replayed_decision_with_its_own_propensities() {
        let mut o = graded("s", 1, &["A", "B", "C"], &[2, 1, 1]);
        (o.policy, o.source, o.placebo, o.psi, o.nofree) = ("fixed".into(), "fixed".into(), vec!["C".into()], 0.5, vec!["A".into()]);
        let ls: Vec<Challenge> = ["A", "B", "C"].iter().map(|id| Challenge { id: id.to_string(), ..Default::default() }).collect();
        let r = Routed {
            ask: vec![&ls[1]],
            free: Vec::new(),
            placebo: vec![&ls[2]],
            p: vec![0.0, 1.0, 0.0],
            pf: vec![0.0; 3],
            log: crate::select::Log { policy: "learned", source: "learned", fit: "9-abcdef01".into(), psi: 1.0, ..crate::select::Log::default() },
        };
        let d = decided(o.clone(), &r);
        assert_eq!((d.chosen, d.free, d.placebo, d.p, d.psi), (vec!["B".to_string()], Vec::new(), vec!["C".to_string()], vec![0.0, 1.0, 0.0], 1.0));
        assert_eq!((d.policy.as_str(), d.source.as_str(), d.fit.as_str()), ("learned", "learned", "9-abcdef01"));
        assert_eq!((d.lessons, d.grades, d.nofree, d.check), (o.lessons, o.grades, o.nofree, o.check), "the logged check's");
    }

    /// Leave-future-out over time: a label is known before a check at its value then. A veto after
    /// the check, or a stronger verdict, is a result from after it and reaches only later checks.
    #[test]
    fn a_later_veto_reaches_no_prediction_before_it() {
        let mut a = graded("s", 10, &["A"], &[1]);
        a.check = "0000000000aa".into();
        let later = |ms: i64, lesson: &str| {
            let mut c = graded("s", ms, &[lesson], &[1]);
            c.check = format!("{ms:012x}");
            c
        };
        let log = [
            a,
            Outcome { check: "0000000000aa".into(), ..result(outcome::CHANGED, "s", "A", 15, "queued") },
            later(50, "B"),
            Outcome { check: format!("{:012x}", 50), ..result(outcome::IGNORED, "s", "B", 55, "queued") },
            result(outcome::VETOED, "s", "A", 100, ""),
            later(200, "C"),
            Outcome { check: format!("{:012x}", 200), ..result(outcome::IGNORED, "s", "C", 205, "queued") },
        ];
        let checks = checks_of(&log);
        let (ls, _) = labels(&checks, &log);
        assert_eq!(ls[0].known, [(15, 1.0), (100, 0.0)], "changed, then vetoed");
        assert_eq!((ls[0].y, ls[0].at(15), ls[0].at(50), ls[0].at(200)), (0.0, None, Some((15, 1.0)), Some((100, 0.0))), "{:?}", ls[0]);
        // Grade only before B knows A as changed, before C as not.
        let past = past_counts(&ls);
        assert_eq!((past[1][1][FREE], past[2][1][FREE]), ((1, 1), (2, 0)), "{past:?}");
        // So does the value model's training log.
        let label_of = |s: &Summary, lesson: &str| {
            let at = s.lessons.iter().position(|l| l == lesson).unwrap() as u32;
            s.applies.iter().find(|x| x.lesson == at).map(|x| x.label)
        };
        let all: Vec<&Label> = ls.iter().collect();
        assert_eq!(label_of(&known(&checks, &all[..1], 50), "A"), Some(summary::Label::Changed));
        assert_eq!(label_of(&known(&checks, &all[..2], 200), "A"), Some(summary::Label::Ignored));
        // An ignored verdict then a change: unknown before the first, then 0, then 1.
        assert_eq!(values(&[(5, 0), (9, 1)], &[]), [(5, 0.0), (9, 1.0)]);
        assert_eq!(values(&[(5, 1)], &[(5, 3)]), [(5, 0.0)], "results at the same ms land together");
        assert_eq!(values(&[(5, 1)], &[(2, 2)]), [(5, 0.0)], "a correction after the check and before the verdict outranks it");
    }

    #[test]
    fn eval_checks_read_graded_lines_channels_and_cost() {
        let ctx = "#borg-predict-cost 0.0500\n#borg-graded n=2 L1:3:0,L2:1:1\n#borg-p L1=1,L2=1\n#borg-asked L1,L2\n#borg-predict-cost 0.0700\n#borg-graded n=1 La:b:2:0\n#borg-queued La:b\n#borg-graded n=0 \n";
        let cost =
            "live usd=0.05 lessons=3\ngraded applied=2 ask=2 free=0 dropped\ngraded applied=1 ask=0 free=1 queued\ngraded applied=0 ask=0 free=0 dropped\n";
        let cs = eval_checks("s", ctx, cost);
        assert_eq!(cs.iter().map(|c| c.channel).collect::<Vec<_>>(), ["live", "stop", "stop"]);
        assert_eq!(cs.iter().map(|c| c.usd).collect::<Vec<_>>(), [0.05, 0.07, 0.0], "each check's own call");
        let applies: Vec<Vec<(String, u8)>> = cs.into_iter().map(|c| c.applies).collect();
        assert_eq!(applies, [vec![("L1".into(), 3), ("L2".into(), 1)], vec![("La:b".into(), 2)], vec![]], "an id may hold a colon");
        // Without an aligned cost log, a #borg-live delivery after the check marks it live.
        let cs = eval_checks("s", ctx, "");
        assert_eq!(cs.iter().map(|c| c.channel).collect::<Vec<_>>(), ["stop", "stop", "stop"]);
        let live = "#borg-graded n=1 L1:3:0\n#borg-asked L1\n#borg-live L1 ms=5\n#borg-graded n=1 L2:3:0\n#borg-asked L2\n";
        assert_eq!(eval_checks("s", live, "").iter().map(|c| c.channel).collect::<Vec<_>>(), ["live", "stop"]);
    }

    #[test]
    fn recall_counts_a_run_whose_check_delivers_a_right_lesson() {
        let check = |applies: &[(&str, u8)]| EvalCheck {
            session: "s".into(),
            channel: "stop",
            applies: applies.iter().map(|(i, g)| (i.to_string(), *g)).collect(),
            usd: 0.1,
        };
        let runs = vec![
            EvalRun { case: "c1".into(), checks: vec![check(&[("X", 1)]), check(&[("R", 2)])] }, // R handed over free
            EvalRun { case: "c1".into(), checks: vec![check(&[("X", 3)])] },
            EvalRun { case: "c2".into(), checks: vec![check(&[("R2", 3), ("X", 1)])] }, // a block asks both
            EvalRun { case: "c2".into(), checks: vec![check(&[("R2", 1)])] },
        ];
        let right: BTreeMap<String, BTreeSet<String>> =
            [("c1".to_string(), BTreeSet::from(["R".to_string()])), ("c2".to_string(), BTreeSet::from(["R2".to_string()]))].into();
        let none = Path::new("/nonexistent/lessons.jsonl");
        let base = recalls(&runs, &right, None, none);
        assert_eq!((base.per_run.as_slice(), base.deviations), (&[1.0, 0.0, 1.0, 1.0][..], 0));
        assert_eq!(base.spend, [3.0 / 5.0, 3.0 / 5.0, 2.0 / 5.0], "questions, free and blocks per check");
        // Expected over draws: a policy that delivers on half its seeds recalls a one-check run half the time.
        let coin = |u: &[Unit], _: &EvalCheck, seed: u64| vec![if seed.is_multiple_of(2) { Route::Free } else { Route::Not }; u.len()];
        let half = recalls(&runs, &right, Some(&coin), none);
        assert!(half.per_run.iter().zip(&base.per_run).all(|(h, b)| (h - 0.5 * b).abs() < 0.1), "half of each run the fixed rule recalls: {:?}", half.per_run);
        assert_eq!(half.deviations, 5, "every check has a draw off the fixed rule");
        // The bar: a policy that delivers nothing loses every recall and fails; one that delivers all passes.
        let never = |u: &[Unit], _: &EvalCheck, _: u64| vec![Route::Not; u.len()];
        let all = |u: &[Unit], _: &EvalCheck, _: u64| vec![Route::Ask; u.len()];
        let lost = against_baseline(&runs, &right, &never, &all, none);
        assert_eq!((lost.verdict, lost.cases, lost.of, lost.seeds), (Verdict::Fail, 2, 2, (2, 2)));
        assert!(!lost.prior.as_ref().unwrap().pass && lost.posterior.as_ref().unwrap().pass, "{lost:?}");
        assert_eq!(lost.prior.as_ref().unwrap().delta.as_ref().unwrap().value, Some(-0.75));
        let pass = against_baseline(&runs, &right, &all, &all, none);
        assert_eq!(
            (pass.verdict, pass.prior.as_ref().map(|a| a.deviations)),
            (Verdict::Pass, Some(3)),
            "the fixed rule asks both checks with a 3 too: {pass:?}"
        );
        // An arm that decides as the fixed rule everywhere tests nothing, and says so; a failing arm
        // still fails.
        let fixed = |u: &[Unit], _: &EvalCheck, _: u64| policy::fixed_routes(u, &[]);
        let same = against_baseline(&runs, &right, &all, &fixed, none);
        assert_eq!((same.verdict, same.posterior.as_ref().map(|a| (a.deviations, a.pass))), (Verdict::NotTested, Some((0, true))), "{same:?}");
        assert!(same.why.contains("store posterior arm decides as the fixed rule at every check"), "{}", same.why);
        assert_eq!(against_baseline(&runs, &right, &never, &fixed, none).verdict, Verdict::Fail);
        // A case short of a seed: not decided.
        assert_eq!(against_baseline(&runs[..3], &right, &all, &all, none).verdict, Verdict::NotRun);
    }

    /// P4's baseline (`recalls` without a policy: select/policy.rs `fixed_routes`) is P0's reference
    /// (conscience.rs `graded_without`, no cap) by position, on a grade table with and without lessons
    /// the session had: the two checks judge against one fixed rule.
    #[test]
    fn the_recall_baseline_is_the_fixed_rule_p0_checks() {
        // P0's reference as routes: each copy of a lesson by its own place in the applies.
        fn reference(applied: &[(&Challenge, u8)], had: &[String]) -> Vec<Route> {
            let (ask, free) = conscience::graded_without(applied, None, had);
            let held = |v: &[&Challenge], x: &Challenge| v.iter().any(|y| std::ptr::eq(*y, x));
            applied
                .iter()
                .map(|(x, _)| match (held(&ask, x), held(&free, x)) {
                    (true, _) => Route::Ask,
                    (_, true) => Route::Free,
                    _ => Route::Not,
                })
                .collect()
        }
        // A check's applies as `recalls` reads them.
        fn challenges(applies: &[(String, u8)]) -> Vec<Challenge> {
            applies.iter().map(|(id, _)| Challenge { id: id.clone(), ..Default::default() }).collect()
        }
        fn applied<'a>(cs: &'a [Challenge], applies: &[(String, u8)]) -> Vec<(&'a Challenge, u8)> {
            cs.iter().zip(applies.iter().map(|(_, g)| *g)).collect()
        }
        let lessons: Vec<String> = (0..12).map(|i| format!("L{i}")).collect();
        let named = |gs: &[u8]| -> Vec<(String, u8)> { gs.iter().zip(&lessons).map(|(g, id)| (id.clone(), *g)).collect() };
        let twice = |gs: &[u8], again: u8| [named(gs), vec![(lessons[0].clone(), again)]].concat();
        let table = [
            Vec::new(),
            named(&[3]),
            named(&[1, 3, 2]),                            // a 3 blocks: every 3, 2 and 1
            named(&[1, 2, 1]),                            // no 3: handed over free
            named(&[1, 2, 3, 1, 2, 1, 1, 2, 1, 1, 2, 3]), // a block past the cap: the 3s and 2s, then the first 1s
            named(&[2, 1, 1, 2, 1, 1, 2, 1, 1, 2, 1, 1]), // free past the cap: the 2s, then the first 1s
            twice(&[1; 10], 1),                           // L0 named again past the cap: that copy waits
            twice(&[1, 2], 3),                            // L0 named again as a 3: both copies asked
        ];
        let hads: [Vec<String>; 4] = [Vec::new(), vec![lessons[0].clone()], vec![lessons[1].clone(), "Z".into()], lessons.clone()];
        let mut moved = 0;
        for row in &table {
            let cs = challenges(row);
            let applied = applied(&cs, row);
            for had in &hads {
                let want = reference(&applied, had);
                assert_eq!(policy::fixed_routes(&crate::select::units(&applied), had), want, "{row:?} had {had:?}");
                moved += usize::from(want != reference(&applied, &[]));
            }
        }
        assert!(moved > 0, "a lesson the session had changes some row's routes");
        // `recalls` decides its baseline by that rule: a policy that is P0's reference never deviates
        // from it and recalls as it does.
        let checks = table.iter().map(|row| EvalCheck { session: "s".into(), channel: "stop", applies: row.clone(), usd: 0.0 }).collect();
        let runs = [EvalRun { case: "c".into(), checks }];
        let right: BTreeMap<String, BTreeSet<String>> = [("c".to_string(), BTreeSet::from([lessons[11].clone()]))].into();
        let p0 = |_: &[Unit], c: &EvalCheck, _: u64| reference(&applied(&challenges(&c.applies), &c.applies), &[]);
        let none = Path::new("/nonexistent/lessons.jsonl");
        let (base, same) = (recalls(&runs, &right, None, none), recalls(&runs, &right, Some(&p0), none));
        assert_eq!((same.deviations, &same.per_run, same.spend), (0, &base.per_run, base.spend));
    }

    /// A labelled log under the fixed rule: `sessions` sessions of `checks` checks (with ids, every
    /// fourth at the stop), each asked or free apply delivered and judged (changed more often the higher
    /// its grade), each session settled at its end as `borg learn` settles it.
    fn labelled(sessions: usize, checks: usize, seed: u64) -> Vec<Outcome> {
        let mut rng = memory::Rng::new(seed);
        let (mut log, mut ms) = (Vec::new(), 0);
        for s in 0..sessions {
            let session = format!("s{s}");
            for c in 0..checks {
                ms += 10;
                let n = 1 + (rng.next_u64() % 4) as usize;
                let mut lessons: Vec<String> = Vec::new();
                while lessons.len() < n {
                    let l = format!("L{}", rng.next_u64() % 12);
                    if !lessons.contains(&l) {
                        lessons.push(l);
                    }
                }
                let grades: Vec<u8> = (0..n).map(|_| 1 + (rng.next_u64() % 3) as u8).collect();
                let mut o = graded(&session, ms, &lessons.iter().map(String::as_str).collect::<Vec<_>>(), &grades);
                o.channel = if c % 4 == 3 { "stop" } else { "live" }.into();
                o.check = crate::select::check_id(&session, &o.channel, ms, 1);
                o.policy = "fixed".into();
                let cs: Vec<Challenge> = lessons.iter().map(|id| Challenge { id: id.clone(), ..Default::default() }).collect();
                o.free = ids(&conscience::graded_capped(&cs.iter().zip(grades.iter().copied()).collect::<Vec<_>>(), None).1);
                o.pf = holds(&o.lessons, &o.free);
                // Per delivered apply: its lesson, delivery route, verdict channel and grade.
                let routed: Vec<(String, &str, String, u8)> = o
                    .lessons
                    .iter()
                    .zip(&grades)
                    .filter_map(|(l, g)| match (o.chosen.contains(l), o.free.contains(l)) {
                        (true, _) => Some((l.clone(), outcome::ROUTE_ASKED, o.channel.clone(), *g)),
                        (_, true) => Some((l.clone(), outcome::ROUTE_FREE, "queued".to_string(), *g)),
                        _ => None,
                    })
                    .collect();
                let check = o.check.clone();
                log.push(o);
                for (l, route, ch, g) in routed {
                    ms += 1;
                    log.push(Outcome { check: check.clone(), route: route.into(), ..result(outcome::DELIVERY, &session, &l, ms, "") });
                    let changed = rng.unit() < [0.0, 0.15, 0.35, 0.5][usize::from(g)];
                    let kind = if changed { outcome::CHANGED } else { outcome::IGNORED };
                    log.push(Outcome { check: check.clone(), judge: 2, ..result(kind, &session, &l, ms, &ch) });
                }
            }
            ms += 10;
            log.push(Outcome { lessons: Vec::new(), reason: outcome::UNJUDGED_NONE.into(), ..result(outcome::NOT_CORRECTED, &session, "", ms, "") });
        }
        log
    }

    /// The cold-start replay decides as the hooks would: with the fit `borg learn` makes when a session
    /// ends, the gate's ledger over the policy's own checks. Until the first end it is the fixed rule;
    /// after, the learned policy leaves it within the gate's budget. Without the refits it has no model,
    /// never leaves the fixed rule, and P3 passes on nothing.
    #[test]
    fn the_cold_start_replay_refits_at_each_session_end_and_deviates_within_the_gate() {
        let (sessions, checks) = (8, 16);
        let log = labelled(sessions, checks, 3);
        let (p3, targets) = cold_start(&text(&log), Path::new("/nonexistent/lessons.jsonl"), None);
        assert_eq!(p3.refits, sessions - 1, "before the first check after each end but the last: {p3:?}");
        assert_eq!((p3.verdict, p3.over, p3.cold, p3.checks), (Verdict::Pass, 0, true, sessions * checks), "{p3:?}");
        assert!(p3.deviations > 0, "the learned policy decides: {p3:?}");
        assert_ne!(p3.policy, p3.fixed, "its own spend: {p3:?}");
        // The first session is decided before any fit: the fixed rule, its choice as the propensities.
        let pass = as_logged(&text(&log), Path::new("/nonexistent/lessons.jsonl"));
        for (c, t) in pass.checks.iter().zip(&targets).take(checks) {
            assert_eq!(t, &(c.p.clone(), c.pf.clone()), "{}", c.line());
        }
        assert!(pass.checks.iter().zip(&targets).skip(checks).any(|(c, t)| t != &(c.p.clone(), c.pf.clone())), "then its own propensities");
    }

    /// P2 runs on the value model (select/policy.rs `fit`, `predict`), leave-future-out: the summary it
    /// fits on holds exactly the labels whose result came before, no check after them, and while there
    /// is none the prior predicts.
    #[test]
    fn calibration_fits_the_value_model_on_the_labels_before_each_check() {
        let log = labelled(6, 12, 5);
        let none = Path::new("/nonexistent/lessons.jsonl");
        let pass = as_logged(&text(&log), none);
        let (ls, unjoined) = labels(&pass.checks, &log);
        assert!(ls.len() > 20 && unjoined == 0, "{} labels, {unjoined} unjoined", ls.len());
        let p2 = calibration(&ls, unjoined, &|value, past, at| model(&pass.checks, past, at, value));
        assert!(matches!(p2.verdict, Verdict::Pass | Verdict::Fail), "{p2:?}");
        assert!(p2.model.is_some_and(f64::is_finite) && p2.diff.as_ref().is_some_and(|d| d.n > 0) && p2.informed > 0, "{p2:?}");
        let cut = ls[ls.len() / 2].check_ms;
        let past: Vec<&Label> = ls.iter().filter(|l| l.first() < cut).collect();
        let s = known(&pass.checks, &past, cut);
        let judged = s.applies.iter().filter(|a| matches!(a.label, summary::Label::Changed | summary::Label::Ignored)).count();
        assert_eq!(judged, past.len(), "exactly the labels before");
        let newest = past.iter().filter_map(|l| l.at(cut)).map(|k| k.0).max().unwrap();
        assert!(s.checks.iter().all(|c| c.ms <= newest), "no check after them");
        assert!(policy::fit(&s, None).model.is_some());
        let prior = model(&pass.checks, &[], cut, Value::Full).unwrap();
        assert!(ls.iter().all(|l| (0.0..1.0).contains(&prior(l))), "the prior predicts before any label");
        assert_eq!(known(&pass.checks, &[], cut), Summary::default());
        // The pruned rerun predicts before any label by its own prior (select/policy.rs `prior_pruned`).
        let pruned = model(&pass.checks, &[], cut, Value::Pruned).unwrap();
        let by = |fit: &Fit, l: &Label| {
            let o = &pass.checks[l.check].o;
            let units: Vec<Unit> = o
                .lessons
                .iter()
                .zip(&o.grades)
                .enumerate()
                .map(|(j, (lesson, grade))| Unit { lesson, grade: *grade, rank: o.ranks.get(j).map_or(j, |r| usize::from(*r)) })
                .collect();
            let route = [Route::Ask, Route::Free, Route::Not][l.route];
            let m = routes_of(&pass.checks[l.check]).iter().filter(|r| **r == route).count();
            policy::predict(fit, &policy::Feat::of(&units, o.channel == "stop", o.tokens)[l.j], route, m, &o.session, &o.lessons[l.j]).unwrap()
        };
        assert!(ls.iter().all(|l| pruned(l) == by(&policy::prior_pruned(), l)), "the pruned prior");
        assert!(ls.iter().any(|l| pruned(l) != by(&policy::prior(), l)), "not the full model's prior");
    }

    /// An evaluation's run directory: per run `<case>-s<seed>/borg-state/<session>.ctx` with its graded
    /// lines, and the qrels beside them.
    fn eval_dir(name: &str, runs: &[(&str, &str)], qrels: &[(&str, &str)]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-replay-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (i, (case, ctx)) in runs.iter().enumerate() {
            let state = d.join("runs").join(format!("{case}-s{i}")).join("borg-state");
            std::fs::create_dir_all(&state).unwrap();
            std::fs::write(state.join(format!("sess{i}.ctx")), ctx).unwrap();
        }
        let q: String = qrels.iter().map(|(case, id)| format!("{}\n", serde_json::json!({"case": case, "challenge": id, "grade": 2}))).collect();
        std::fs::write(d.join("qrels.jsonl"), q).unwrap();
        d
    }

    /// P4's arms are the learned policy's: the posterior arm draws from the whole log's fit (what `borg
    /// learn` would fit, not the folded summary's prior), the prior arm from the n = 0 prior's model,
    /// whose draws leave the fixed rule. Either one being the fixed rule would pass P4 on nothing.
    #[test]
    fn p4_draws_from_the_stores_posterior_and_from_the_prior() {
        let log = labelled(6, 12, 7);
        let checks = "#borg-graded n=4 R:2:0,X:1:1,Y:1:2,Z:2:3\n#borg-graded n=3 W:3:0,R:1:1,V:2:2\n";
        let d = eval_dir("p4", &[("c1", checks), ("c1", checks)], &[("c1", "R")]);
        let report = evaluate(Input {
            text: &text(&log),
            lessons: Path::new("/nonexistent/lessons.jsonl"),
            summary: Summary::default(),
            replay: true,
            eval: Some((&d.join("runs"), &d.join("qrels.jsonl"))),
        });
        let p4 = report.checks.map(|c| c.p4).unwrap();
        let whole = policy::fit(&summary::fold(&text(&log), Summary::default()), None);
        assert!(whole.model.is_some() && whole.labels > 0);
        assert_eq!((&p4.fit, p4.labels), (&whole.key, whole.labels), "the store's posterior: {p4:?}");
        let (base, prior) = (p4.baseline.unwrap(), p4.prior.unwrap());
        assert_eq!((p4.runs, p4.checks), (2, 4));
        assert!((prior.questions, prior.free) != (base.questions, base.free), "the prior's draws leave the fixed rule: {prior:?} {base:?}");
        let _ = std::fs::remove_dir_all(d);
    }
}
