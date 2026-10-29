//! Usefulness as the memory dynamics of a collective brain (internal evaluation "phase-r", "R1 design", not published).
//!
//! Everything here is derived from the append-only outcome log (outcome.rs), so merging two logs merges
//! memory, whatever the line order. The belief readers derive it from the team view (outcome.rs
//! `read_team`): this machine's log plus the redacted results the mesh syncs from teammates, each
//! remote session under its hashed id and counted once per lesson, so teammates converge on one
//! memory. The activity clock then counts the collective's sessions on every node, not this machine's
//! alone. BORG_SHARE_OUTCOMES=off keeps it this machine's view. Per lesson:
//!
//! - **Uses.** A use is an `asked_then_changed` record: after the Stop block asked the lesson, the
//!   drone changed its work on that point and the change avoids the consequence, as judged by the
//!   consequence model at a later Stop call of the session (predict.rs `Judged`, main.rs `pending`;
//!   no extra call). The lesson's creation counts as one use too, so
//!   a new lesson is not invisible.
//! - **Activation** (ACT-R base-level learning; equally a pheromone trail of evaporating deposits):
//!   `A = ln(Σ_j Δt_j^(−d))` over the uses j, `d = 0.5` (`D_CONSOLIDATED` = 0.3 once consolidated).
//!   Δt counts sessions of the collective since the use (the activity clock, `Clock`), plus one, never
//!   wall time: a holiday erases nothing. Uses spread over time keep a lesson active longer than the
//!   same number massed early; with a fixed d that comes from the later last use (a decay that depends
//!   on the activation at each use, Pavlik and Anderson's spacing model, is a possible refinement).
//! - **Usefulness belief** `Beta(1 + changed + ½·tested, 1 + ignored + 2·corrected + 3·vetoed +
//!   regretted)` (the veto's 3 is `VETOED_W`), one result
//!   per (session, lesson), vetoed > corrected > changed > ignored. `asked_not_corrected` (settled by `borg
//!   learn`) is censored, no result: the judge never said whether the drone acted on the lesson. A
//!   placebo's result and an unclear verdict are none either (outcome.rs `is_result`). A correction counts
//!   twice (outcome.rs `CORRECTED_W`); a veto (`borg no`,
//!   veto.rs) three times, above a correction as the design asks (no eval behind the 3 yet). Regretted is
//!   the weighted share of later fixes of decisions the lesson was asked before (outcome.rs `regretted`),
//!   each regret once, at most `REGRET` × 0.875: a separate, later event, added on top of the session's
//!   result unless that result is a correction or a veto (the stronger result already counts it). One
//!   deciding session's regrets give a lesson at most `regret_cap` of that session's corrections by the
//!   engineer (`is_correction`: a miss a regret wrote is none). A fractional β is sampled by randomised
//!   rounding (`round_seeded`).
//!   `tested` (outcome.rs) is the session's own tests going green after the ask, one per (lesson,
//!   session), weighed `TESTED_W`: additive to the session's result (it is about the work, a session
//!   result about the engineer's reaction), never a use, no tick, and nothing for a session whose
//!   result is a correction or a veto.
//!   A fractional α (a candidate weight) is rounded the same way
//!   (`draw`). `Weights` holds the hand-set weights so that `borg debug ope` (ope.rs)
//!   can replay the belief under other values offline; the counts `derive` returns are constant-free.
//! - **Weight** = `squash(A) × (0.5 + x)`, where `x` is a Thompson sample from the belief, seeded by
//!   (session, lesson) so a session sees one stable ranking, or the belief's mean without a session.
//!   `squash(A) = 2 / (1 + e^(−A/2))` lies in (0, 2) and is 1 at A = 0, a lesson created this session;
//!   `0.5 + x` lies in (0.5, 1.5) with mean 1 for a lesson without outcomes. So a new lesson ranks as
//!   BM25 ranks it, in expectation; uncertain lessons get tried (their samples spread), proven ones win,
//!   harmful ones fade. The sample is tempered (±50 %) so that a lesson with no evidence cannot drop a
//!   strong BM25 match to nothing. In ranking (conscience.rs), the relative bar uses the mean weight and
//!   the session's draw orders the lessons over the bar for the limited slots: exploration never pushes
//!   a strong match under the bar, it decides who gets asked when more qualify than fit.
//! - **Consolidated** (long-term): changed in at least `CONSOLIDATE_SESSIONS` distinct sessions, across
//!   at least two repos or authored by at least two engineers. Consolidated lessons decay slower and
//!   may be delivered early even without a scope (main.rs `early`).
//! - **Dormant**: not consolidated and `A < DORMANT_BELOW` (about 54 sessions of the collective
//!   without a use). Never deleted: a dormant lesson is recalled only on a strong match (conscience.rs
//!   `rank`), and one use reactivates it.
//! - **Whose sessions count.** Disuse is measured where the lesson could have been used, by its reach,
//!   on whichever node the sessions ran. ACT-R's decay stands for
//!   the opportunities a memory had and missed; a session that could never have recalled the lesson is
//!   no such opportunity. So a team lesson ages with every session of the collective, a teammate's
//!   included: they all see it, and a lesson none of their drones acts on for ~54 sessions has stopped
//!   mattering to the people it reaches, even when its own author's node is quiet. A repo convention ages
//!   only with the sessions in its repo (`Clock::eligible`, by the repo of each session's lines), so
//!   neither a teammate's busy week elsewhere nor an engineer's own work in another repo lets it fade.
//!   This is coarser than reach.rs `deliverable`: a session's repo is its working directory's
//!   (hook.rs `repo_of`), not every repo it touched, so a session started in one repo that edits
//!   another is no tick for the other's conventions (a use there still counts: it ranks at the next
//!   eligible tick). The system gate (systems.rs `gate`) is not applied, so a lesson scoped to other
//!   systems ages with every session its reach allows. In the team view each repo is a case-sensitive
//!   tag, so a clone whose directory name differs in case from the lesson's repo does not age it.

use std::collections::{HashMap, HashSet};

use crate::conscience::Challenge;
use crate::outcome::{self, Outcome};

/// Baseline: ACT-R's decay of a use's activation (`d` in the module doc), the value of the ACT-R
/// literature; not fitted to borg's outcomes.
pub const D: f64 = 0.5;
/// Baseline: what one regret at full strength adds to a lesson's β: half a correction (a correction is
/// same-session, same-concern evidence; a regret is delayed and shared among the lessons asked). A veto
/// weighs more than a correction, so regret < corrected < vetoed.
pub const REGRET: f64 = 1.0;
/// Baseline: what one `tested` result (outcome.rs) adds to α: half a change (design-chosen, `borg debug ope`
/// estimates others): the session's own runner said the work passes.
pub const TESTED_W: f64 = 0.5;
/// Baseline: what one veto (`borg no`, veto.rs) adds to a lesson's β: above a correction's 2, as the design asks
/// (design-chosen, no eval behind it yet).
pub const VETOED_W: u32 = 3;

/// The belief's hand-set weights (design-chosen; `borg debug ope` estimates others offline, never applies them).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Weights {
    pub vetoed: f64,
    pub regret: f64,
    pub tested: f64,
}

/// The weights in force.
pub const WEIGHTS: Weights = Weights { vetoed: VETOED_W as f64, regret: REGRET, tested: TESTED_W };
/// Baseline: the slower decay of a consolidated lesson (module doc), design-chosen.
pub const D_CONSOLIDATED: f64 = 0.3;
/// Baseline: the distinct sessions that changed their work on a lesson before it is consolidated
/// (with two repos or two engineers), design-chosen.
pub const CONSOLIDATE_SESSIONS: usize = 3;
/// Baseline: the activation below which an unconsolidated lesson is dormant (about 54 sessions without
/// a use at `D`), design-chosen; a dormant lesson is recalled only on a strong match, never deleted.
pub const DORMANT_BELOW: f64 = -2.0;

/// The collective's activity clock: its sessions in the order they first appear in the outcome log
/// (by first ms, then id: independent of line order). Only the kinds the team shares
/// (outcome.rs `SHARED_KINDS`) tick it, so every node counts the same sessions from the same first
/// ms: a session that only graded never moves it, and neither does a `tested` result (about the
/// session's work) nor a placebo's (never shared, outcome.rs `is_result`).
#[derive(Debug, Default)]
pub struct Clock {
    first: Vec<(i64, String)>,
    at: HashMap<String, u64>,
    /// Per tick, the repos its session's ticking lines name (raw here, `tag`ged in the team view).
    repos: Vec<HashSet<String>>,
}

impl Clock {
    pub fn of(outcomes: &[Outcome]) -> Self {
        let mut first: HashMap<&str, i64> = HashMap::new();
        let mut repos: HashMap<&str, HashSet<String>> = HashMap::new();
        for o in outcomes.iter().filter(|o| outcome::SHARED_KINDS.contains(&o.kind.as_str()) && o.kind != outcome::TESTED && outcome::is_result(o)) {
            let m = first.entry(o.session.as_str()).or_insert(o.ms);
            *m = (*m).min(o.ms);
            if !o.repo.is_empty() {
                repos.entry(o.session.as_str()).or_default().insert(o.repo.clone());
            }
        }
        let mut first: Vec<(i64, String)> = first.into_iter().map(|(s, ms)| (ms, s.to_string())).collect();
        first.sort();
        let at = first.iter().enumerate().map(|(i, (_, s))| (s.clone(), i as u64)).collect();
        let repos = first.iter().map(|(_, s)| repos.remove(s.as_str()).unwrap_or_default()).collect();
        Clock { first, at, repos }
    }

    /// The ticks of the sessions `c` counts its disuse in, ascending, or None for every tick: a repo
    /// convention's are the sessions with a line in its repo, matched ignoring case as reach.rs
    /// `deliverable` does or, in the team view, by its exact `tag`; any other reach's are all of them.
    pub fn eligible(&self, c: &Challenge) -> Option<Vec<u64>> {
        if c.reach != crate::reach::REPO || c.repo.is_empty() {
            return None;
        }
        let tagged = outcome::tag(&c.repo);
        Some((0..self.repos.len() as u64).filter(|t| self.repos[*t as usize].iter().any(|r| r.eq_ignore_ascii_case(&c.repo) || *r == tagged)).collect())
    }

    /// The tick of `session`: its place on the clock, or the next tick for a session not in the log
    /// (or none given).
    pub fn now(&self, session: Option<&str>) -> u64 {
        session.and_then(|s| self.at.get(s).copied()).unwrap_or(self.first.len() as u64)
    }

    /// The tick of a session.
    pub fn of_session(&self, session: &str) -> u64 {
        self.at.get(session).copied().unwrap_or(self.first.len() as u64)
    }

    /// The tick at wall time `ms`: how many sessions had started before it.
    pub fn at_ms(&self, ms: i64) -> u64 {
        self.first.partition_point(|(m, _)| *m < ms) as u64
    }
}

/// `ln(Σ age^(−d))` over ages in ticks (each at least 1); −∞ without any.
pub fn activation(ages: impl IntoIterator<Item = u64>, d: f64) -> f64 {
    ages.into_iter().map(|a| (a.max(1) as f64).powf(-d)).sum::<f64>().ln()
}

/// `2 / (1 + e^(−A/2))`: (0, 2), 1 at A = 0.
pub fn squash(a: f64) -> f64 {
    2.0 / (1.0 + (-a / 2.0).exp())
}

/// One lesson's memory.
#[derive(Debug, Clone, PartialEq)]
pub struct Memory {
    pub activation: f64,
    /// Sessions whose result for this lesson is a change, an ignore, a correction, a veto.
    pub changed: u32,
    pub ignored: u32,
    pub corrected: u32,
    pub vetoed: u32,
    /// Σ credit of the regrets that blame it (outcome.rs `regretted`), each regret once.
    pub regretted: f64,
    /// Sessions whose own tests went green after it was asked (outcome.rs `tested`).
    pub tested: u32,
    pub consolidated: bool,
    pub dormant: bool,
}

impl Memory {
    /// α under weights `w`: the counts are constant-free, so one `derive` serves every candidate.
    pub fn alpha_with(&self, w: &Weights) -> f64 {
        1.0 + self.changed as f64 + w.tested * self.tested as f64
    }

    /// β under weights `w`.
    pub fn beta_with(&self, w: &Weights) -> f64 {
        (1 + self.ignored + outcome::CORRECTED_W * self.corrected) as f64 + w.vetoed * self.vetoed as f64 + w.regret * self.regretted
    }

    pub fn alpha(&self) -> f64 {
        self.alpha_with(&WEIGHTS)
    }

    pub fn beta(&self) -> f64 {
        self.beta_with(&WEIGHTS)
    }

    pub fn mean(&self) -> f64 {
        self.alpha() / (self.alpha() + self.beta())
    }

    /// `squash(A) × (0.5 + x)`: x the Thompson sample for (`session`, `id`), or the mean without a session.
    pub fn weight(&self, id: &str, session: Option<&str>) -> f64 {
        let x = match session {
            Some(s) => draw(self.alpha(), self.beta(), seed(s, id)),
            None => self.mean(),
        };
        squash(self.activation) * (0.5 + x)
    }
}

/// Each lesson's memory as of `session` (its tick; the next tick when absent or None).
pub fn derive(outcomes: &[Outcome], lessons: &[Challenge], session: Option<&str>) -> HashMap<String, Memory> {
    let clock = Clock::of(outcomes);
    let now = clock.now(session);
    // Per (lesson, session): 3 vetoed > 2 corrected > 1 changed > 0 ignored; and the repos of the changes.
    let mut result: HashMap<&str, HashMap<&str, u8>> = HashMap::new();
    let mut uses: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut repos: HashMap<&str, HashSet<&str>> = HashMap::new();
    // Per (lesson, regret): its credit, the largest of duplicate lines (merging two logs merges memory),
    // and the deciding session it blames.
    let mut regret: HashMap<(&str, &str), (f64, &str)> = HashMap::new();
    // Per lesson: the sessions whose own tests went green after it was asked.
    let mut tested: HashMap<&str, HashSet<&str>> = HashMap::new();
    // Per session: the engineer's corrections the log shows (`regret_cap`).
    let mut corrections: HashMap<&str, HashSet<i64>> = HashMap::new();
    // A placebo's result and an unclear verdict are no evidence about the lesson (outcome.rs `is_result`).
    for o in outcomes.iter().filter(|o| outcome::is_result(o)) {
        if is_correction(o) {
            corrections.entry(o.session.as_str()).or_default().insert(o.ms);
        }
        if o.kind == outcome::TESTED {
            for l in &o.lessons {
                tested.entry(l.as_str()).or_default().insert(o.session.as_str());
            }
            continue;
        }
        if o.kind == outcome::REGRETTED {
            for l in &o.lessons {
                let e = regret.entry((l.as_str(), o.cause.as_str())).or_insert((0.0, o.session.as_str()));
                e.0 = e.0.max(o.credit.clamp(0.0, 1.0));
            }
            continue;
        }
        let r = match o.kind.as_str() {
            outcome::VETOED => 3,
            outcome::CORRECTED => 2,
            outcome::CHANGED => 1,
            outcome::IGNORED => 0,
            // `asked_not_corrected` (settled by `borg learn`) is censored: the judge never said whether
            // the drone acted on the lesson, so it is no ignore. A verdict for the same lesson recorded
            // after it counts alone.
            _ => continue,
        };
        for l in &o.lessons {
            let e = result.entry(l.as_str()).or_default().entry(o.session.as_str()).or_insert(r);
            *e = (*e).max(r);
            if r == 1 {
                uses.entry(l.as_str()).or_default().insert(o.session.as_str());
                repos.entry(l.as_str()).or_default().insert(o.repo.as_str());
            }
        }
    }
    // A regret of a session whose result for the lesson is a correction or a veto adds nothing: the
    // stronger result already counts that session's failure, whichever was recorded first.
    // Per (lesson, deciding session) the regrets' credit, at most `regret_cap` of that session.
    let mut blamed: HashMap<(&str, &str), f64> = HashMap::new();
    for ((l, _), (credit, s)) in regret {
        if result.get(l).and_then(|m| m.get(s)).is_none_or(|r| *r < 2) {
            *blamed.entry((l, s)).or_default() += credit;
        }
    }
    let mut regretted: HashMap<&str, f64> = HashMap::new();
    for ((l, s), credit) in blamed {
        *regretted.entry(l).or_default() += credit.min(regret_cap(corrections.get(s).map_or(0, HashSet::len)));
    }
    // Per (reach, repo): the ticks its lessons were eligible in (`Clock::eligible`).
    let mut eligible: HashMap<(&str, &str), Option<Vec<u64>>> = HashMap::new();
    lessons
        .iter()
        .map(|c| {
            let per = result.get(c.id.as_str());
            let count = |k: u8| per.map_or(0, |m| m.values().filter(|r| **r == k).count() as u32);
            let (changed, ignored, corrected, vetoed) = (count(1), count(0), count(2), count(3));
            // Uses: every session whose result is a change (a later correction in that session still
            // deposited the change: the drone did act on it), plus the lesson's creation.
            let used: Vec<u64> = uses.get(c.id.as_str()).map(|u| u.iter().map(|s| clock.of_session(s)).collect()).unwrap_or_default();
            let changed_sessions = used.len();
            let consolidated = changed_sessions >= CONSOLIDATE_SESSIONS && (repos.get(c.id.as_str()).is_some_and(|r| r.len() >= 2) || c.engineers.len() >= 2);
            let d = if consolidated { D_CONSOLIDATED } else { D };
            let born = clock.at_ms(c.origin_ms);
            // Δt in the sessions the lesson was eligible in: a tick's place among them (itself excluded).
            let eligible = eligible.entry((c.reach.as_str(), c.repo.as_str())).or_insert_with(|| clock.eligible(c));
            let rank = |t: u64| eligible.as_ref().map_or(t, |e| e.partition_point(|x| *x < t) as u64);
            let now = rank(now);
            let ages = used.into_iter().chain([born]).map(|t| now.saturating_sub(rank(t)) + 1);
            let a = activation(ages, d);
            let regretted = regretted.get(c.id.as_str()).copied().unwrap_or(0.0);
            // A green run of a session the engineer corrected or vetoed on the lesson adds nothing.
            let tested = tested.get(c.id.as_str()).map_or(0, |t| t.iter().filter(|s| per.and_then(|m| m.get(**s)).is_none_or(|r| *r < 2)).count() as u32);
            let m = Memory { activation: a, changed, ignored, corrected, vetoed, regretted, tested, consolidated, dormant: !consolidated && a < DORMANT_BELOW };
            (c.id.clone(), m)
        })
        .collect()
}

/// The most regret credit one deciding session gives a lesson: that session's corrections in the log
/// (distinct times of its `is_correction` results), at least one, as one session counts once for a
/// lesson's other results. Guard: one session whose work later took many fixes (a long build's review
/// findings: 193 regrets of one session blamed every lesson it asked, 12,577 lines) would otherwise
/// outweigh every other session of the lesson.
pub fn regret_cap(corrections: usize) -> f64 {
    corrections.max(1) as f64
}

/// Whether `o` is a correction the engineer made of its session's work: an `asked_then_corrected`, a
/// `vetoed`, or an `applied_then_corrected` a correction found. The `applied_then_corrected` a regret
/// writes (learn.rs `credit_regrets`, its `cause` the regret; a tag of it in the team view) is none: it
/// is the regret again, and counting it would raise `regret_cap` with every regret the cap bounds.
fn is_correction(o: &Outcome) -> bool {
    match o.kind.as_str() {
        outcome::CORRECTED | outcome::VETOED => true,
        outcome::MISSED => o.cause.is_empty(),
        _ => false,
    }
}

/// FNV-1a over `session \0 lesson`: a stable seed (std's hasher may change between Rust versions).
pub fn seed(session: &str, lesson: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in session.bytes().chain([0]).chain(lesson.bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// SplitMix64.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    /// Uniform in (0, 1).
    pub fn unit(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
}

/// `x` rounded to an integer at random, unbiased: ⌊x⌋ + 1 with probability frac(x), drawn from `seed`.
/// An integer `x` is returned as is without a draw, so a lesson without regrets samples as before.
pub fn round_seeded(x: f64, seed: u64) -> u32 {
    let (whole, frac) = (x.floor(), x - x.floor());
    match frac == 0.0 || Rng::new(seed).unit() >= frac {
        true => whole as u32,
        false => whole as u32 + 1,
    }
}

/// A Thompson draw from Beta(alpha, beta), both randomised-rounded (`round_seeded`); integer
/// parameters draw exactly as before.
pub fn draw(alpha: f64, beta: f64, seed: u64) -> f64 {
    sample_beta(round_seeded(alpha, seed ^ 0x416c_7068), round_seeded(beta, seed ^ 0x5265_6772), seed)
}

/// A Beta(a, b) draw for integer a, b ≥ 1: X / (X + Y) with X ~ Gamma(a), Y ~ Gamma(b), each a sum of
/// exponentials. Counts above 1000 are scaled down to keep the cost bounded (the shape barely moves).
pub fn sample_beta(a: u32, b: u32, seed: u64) -> f64 {
    let scale = (a.max(b) as f64 / 1000.0).max(1.0);
    let (a, b) = (((a as f64 / scale).round() as u32).max(1), ((b as f64 / scale).round() as u32).max(1));
    let mut rng = Rng::new(seed);
    let mut gamma = |k: u32| (0..k).map(|_| -rng.unit().ln()).sum::<f64>();
    let x = gamma(a);
    let y = gamma(b);
    x / (x + y)
}

/// A deterministic shuffle of `items` for `session` (Fisher-Yates on `Rng`): the order questions are
/// shown in, so that no lesson always sits first (position bias).
pub fn shuffle<T>(items: &mut [T], session: &str) {
    let mut rng = Rng::new(seed(session, "#order"));
    for i in (1..items.len()).rev() {
        let j = (rng.next_u64() % (i as u64 + 1)) as usize;
        items.swap(i, j);
    }
}

/// Words of `text` that discriminate in this store: tokens (conscience.rs `tokens`) that occur in at
/// most a third of the lessons' questions.
fn distinctive(all: &[Challenge]) -> impl Fn(&str) -> HashSet<String> + '_ {
    let mut df: HashMap<String, usize> = HashMap::new();
    for c in all {
        for t in crate::conscience::tokens(&c.question).into_iter().collect::<HashSet<_>>() {
            *df.entry(t).or_default() += 1;
        }
    }
    let n = all.len().max(1);
    move |text: &str| crate::conscience::tokens(text).into_iter().filter(|t| df.get(t).is_some_and(|d| 3 * d <= n)).collect()
}

/// Baseline: undeclared, a change needs this many of the lesson's distinctive words new in the drone's
/// work (design-chosen: one or two shared words happen by chance).
pub const NOVEL_WORDS: usize = 3;

/// The word rule: what the drone did with each asked lesson after it was delivered: (changed, ignored).
/// Never an outcome: its labels measured shared vocabulary (55% "changed" against 38% on a mismatched
/// control, internal evaluation "phase-r" R1, not published). It only decides whether an early-delivered lesson counts as done
/// (main.rs `early_acted`) and serves the offline replay (`borg debug resolve`); asked lessons are labelled by
/// the consequence model (main.rs `pending`).
/// - Declared, in its final message: `<id>: applies` is a change, `<id>: skip` an ignore (the block
///   asks for one such line per question).
/// - Undeclared: a change when its work after the block (`after`: the edits since, and the final
///   message) brings in at least `min_new` of the lesson's distinctive words (`distinctive`, over `all`)
///   that its work before the block (`before`: prompt, touches, earlier edits, the blocked message) did
///   not contain. Shared words alone are not enough: a drone working on the lesson's topic already used
///   them (the R1 replay's control matched other cases' edits as often as a run's own).
pub fn resolve(asked: &[&Challenge], all: &[Challenge], before: &str, after: &str, final_msg: &str, min_new: usize) -> (Vec<String>, Vec<String>) {
    let words = distinctive(all);
    let had: HashSet<String> = crate::conscience::tokens(before).into_iter().collect();
    let new: HashSet<String> = words(&format!("{after}\n{final_msg}")).into_iter().filter(|w| !had.contains(w)).collect();
    let lower = final_msg.to_lowercase();
    let (mut changed, mut ignored) = (Vec::new(), Vec::new());
    for c in asked {
        let verdict = declared(&lower, &c.id.to_lowercase());
        let applies = match verdict {
            Some(v) => v,
            None => words(&c.question).intersection(&new).count() >= min_new,
        };
        match applies {
            true => changed.push(c.id.clone()),
            false => ignored.push(c.id.clone()),
        }
    }
    (changed, ignored)
}

/// `<id>: applies` (Some(true)) or `<id>: skip` (Some(false)) in a lowercased message; brackets,
/// backticks, a dash or a colon between, the last such line winning.
pub fn declared(lower: &str, id: &str) -> Option<bool> {
    let mut out = None;
    let mut from = 0;
    while let Some(i) = lower[from..].find(id) {
        let at = from + i + id.len();
        let rest: String = lower[at..].chars().take(16).collect();
        let rest = rest.trim_start_matches(|c: char| c == ']' || c == '`' || c == '*' || c == ':' || c == '-' || c == '–' || c.is_whitespace());
        if rest.starts_with("applies") || rest.starts_with("apply") {
            out = Some(true);
        } else if rest.starts_with("skip") {
            out = Some(false);
        }
        from = at;
    }
    out
}

/// `text` without the drone's declarations of lesson `id` (every `<id>: applies|skip` that `declared`
/// reads, the id and the word both): the judge's evidence for a question whose declaration it must not
/// see (hook.rs `stripped`). Other lessons' declarations and the rest of the text stay.
pub fn strip_declaration(text: &str, id: &str) -> String {
    // ASCII case folding keeps byte offsets, so a match in `lower` is the same span of `text`.
    let (lower, id) = (text.to_ascii_lowercase(), id.to_ascii_lowercase());
    if id.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let (mut from, mut kept) = (0, 0);
    while let Some(i) = lower[from..].find(&id) {
        let start = from + i;
        let at = start + id.len();
        let gap = lower[at..].len()
            - lower[at..].trim_start_matches(|c: char| c == ']' || c == '`' || c == '*' || c == ':' || c == '-' || c == '–' || c.is_whitespace()).len();
        let rest = &lower[at + gap..];
        let word = ["applies", "apply", "skip"].iter().find(|w| rest.starts_with(**w)).map_or(0, |w| w.len());
        // A declaration starts its line or follows a separator: the id at the start of a word.
        let opens = start == 0 || !lower[..start].ends_with(|c: char| c.is_ascii_alphanumeric());
        if word > 0 && opens && gap > 0 {
            out.push_str(&text[kept..start]);
            kept = at + gap + word;
        }
        from = at;
    }
    out.push_str(&text[kept..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::{CHANGED, CORRECTED, IGNORED, NOT_CORRECTED};

    #[test]
    fn a_stripped_declaration_is_gone_for_the_judge_and_the_rest_stays() {
        for said in ["Mdrain: applies. Replaced the reset arc.", "- [Mdrain] - skip", "`Mdrain`: APPLY", "ok mdrain:skip then"] {
            let s = strip_declaration(said, "Mdrain");
            assert_eq!(declared(&s.to_lowercase(), "mdrain"), None, "{said} -> {s}");
            assert!(declared(&said.to_lowercase(), "mdrain").is_some(), "{said}");
        }
        let both = "Mdrain: applies. New: skip. Mdrain is the budget lesson.";
        let s = strip_declaration(both, "Mdrain");
        assert_eq!(declared(&s.to_lowercase(), "new"), Some(false), "{s}");
        assert!(s.contains("Replaced") || s.contains("Mdrain is the budget lesson"), "{s}");
        assert_eq!(strip_declaration("no declaration here", "Mdrain"), "no declaration here");
        assert_eq!(strip_declaration("XMdrain: applies", "Mdrain"), "XMdrain: applies", "another id ending in it");
    }

    fn o(kind: &str, session: &str, lesson: &str, ms: i64, repo: &str) -> Outcome {
        Outcome { kind: kind.into(), session: session.into(), lessons: vec![lesson.into()], ms, repo: repo.into(), ..Default::default() }
    }

    fn lesson(id: &str, ms: i64) -> Challenge {
        Challenge { id: id.into(), origin_ms: ms, question: format!("question of {id}"), ..Default::default() }
    }

    /// `n` sessions s0..s{n-1} that ask some other lesson, at ms 100, 200, ...
    fn sessions(n: usize) -> Vec<Outcome> {
        (0..n).map(|i| o(outcome::ASKED, &format!("s{i:03}"), "other", 100 * (i as i64 + 1), "r")).collect()
    }

    #[test]
    fn a_new_lesson_is_neutral_and_activation_decays_with_the_session_clock_not_wall_time() {
        let l = lesson("L", 0);
        let m = &derive(&[], std::slice::from_ref(&l), None)["L"];
        assert_eq!((m.activation, m.mean(), m.weight("L", None)), (0.0, 0.5, 1.0));
        // Ten sessions later without a use: A = ln(11^-0.5) < 0; weight below 1 but well above 0.
        let m = &derive(&sessions(10), std::slice::from_ref(&l), None)["L"];
        assert!((m.activation - (11f64).powf(-0.5).ln()).abs() < 1e-12);
        assert!(m.weight("L", None) < 1.0 && m.weight("L", None) > 0.5);
        // The same ten sessions spread over a year (a holiday in between): the same activation.
        let spread: Vec<Outcome> = sessions(10)
            .into_iter()
            .enumerate()
            .map(|(i, mut x)| {
                x.ms = if i < 5 { x.ms } else { x.ms + 365 * 86_400_000 };
                x
            })
            .collect();
        assert_eq!(derive(&spread, &[l], None)["L"].activation, m.activation);
    }

    #[test]
    fn a_repo_convention_ages_only_with_the_sessions_of_its_repo_and_a_team_lesson_with_all() {
        let conv = Challenge { reach: crate::reach::REPO.into(), repo: "a".into(), ..lesson("C", 0) };
        let both = [lesson("T", 0), conv];
        let in_repo = |log: Vec<Outcome>, repo: &dyn Fn(usize) -> String| -> Vec<Outcome> {
            log.into_iter()
                .enumerate()
                .map(|(i, mut x)| {
                    x.repo = repo(i);
                    x
                })
                .collect()
        };
        // Sixty sessions elsewhere: the team lesson goes dormant, the convention had no chance to be used.
        let m = derive(&in_repo(sessions(60), &|_| "b".into()), &both, None);
        assert!(m["T"].dormant);
        assert_eq!((m["C"].activation, m["C"].dormant), (0.0, false));
        // Sixty in its repo, raw, in another case or as the team view's tag: it fades like the team lesson.
        for repo in ["a".to_string(), "A".to_string(), outcome::tag("a")] {
            let m = derive(&in_repo(sessions(60), &|_| repo.clone()), &both, None);
            assert_eq!(m["C"].activation, m["T"].activation, "{repo}");
            assert!(m["C"].dormant, "{repo}");
        }
        // Interleaved, every fourth session in its repo (ten of forty): born before the first (age 10 + 1),
        // used in the second (nine sessions from it on, + 1).
        let mut log = in_repo(sessions(40), &|i| if i % 4 == 0 { "a".into() } else { "b".into() });
        log.push(o(CHANGED, "s004", "C", 501, "a"));
        let m = derive(&log, &both, None);
        let want = activation([11, 10], D);
        assert!((m["C"].activation - want).abs() < 1e-12, "{} vs {want}", m["C"].activation);
    }

    #[test]
    fn activation_is_monotone_in_uses_and_spaced_uses_outlast_massed_ones() {
        let l = lesson("L", 0);
        let mut log = sessions(30);
        let a0 = derive(&log, std::slice::from_ref(&l), None)["L"].activation;
        log.push(o(CHANGED, "s010", "L", 1101, "r"));
        let a1 = derive(&log, std::slice::from_ref(&l), None)["L"].activation;
        log.push(o(CHANGED, "s020", "L", 2101, "r"));
        let a2 = derive(&log, std::slice::from_ref(&l), None)["L"].activation;
        assert!(a0 < a1 && a1 < a2, "{a0} {a1} {a2}");
        // Two uses massed early (sessions 1, 2) vs spaced (1, 20): measured long after, spaced is higher.
        // (Plain base-level learning: the advantage comes from the later last use.)
        let at = |uses: &[usize]| {
            let mut log = sessions(60);
            log.extend(uses.iter().map(|i| o(CHANGED, &format!("s{i:03}"), "L", 100 * (*i as i64 + 1) + 1, "r")));
            derive(&log, std::slice::from_ref(&l), None)["L"].activation
        };
        assert!(at(&[1, 20]) > at(&[1, 2]));
    }

    #[test]
    fn belief_counts_each_session_once_corrected_over_changed_over_ignored() {
        let l = lesson("L", 0);
        let log = vec![
            o(CHANGED, "a", "L", 1, "r"),
            o(NOT_CORRECTED, "a", "L", 2, "r"), // settled later: the change stands
            o(IGNORED, "b", "L", 3, "r"),
            o(CHANGED, "c", "L", 4, "r"),
            o(CORRECTED, "c", "L", 5, "r"),     // corrected anyway: corrected wins
            o(NOT_CORRECTED, "d", "L", 6, "r"), // never judged: censored, no result
        ];
        let m = &derive(&log, &[l], None)["L"];
        assert_eq!((m.changed, m.ignored, m.corrected), (1, 1, 1));
        assert_eq!((m.alpha(), m.beta()), (2.0, 4.0));
    }

    #[test]
    fn an_unjudged_settle_is_censored_and_a_later_verdict_counts_once() {
        let l = lesson("L", 0);
        let mem = |log: &[Outcome]| derive(log, std::slice::from_ref(&l), None)["L"].clone();
        let asked = Outcome { check: "c1".into(), ..o(outcome::ASKED, "a", "L", 1, "r") };
        let censored = Outcome { check: "c1".into(), reason: outcome::UNJUDGED_NO_CALL.into(), ..o(NOT_CORRECTED, "a", "L", 5, "r") };
        let alone = mem(std::slice::from_ref(&asked));
        assert_eq!(mem(&[asked.clone(), censored.clone()]), alone, "no belief effect: not an ignore");
        // The session resumed and the judge spoke for the same check: its verdict alone counts.
        for (kind, want) in [(CHANGED, (1, 0)), (IGNORED, (0, 1))] {
            let later = Outcome { check: "c1".into(), ..o(kind, "a", "L", 9, "r") };
            let m = mem(&[asked.clone(), censored.clone(), later]);
            assert_eq!((m.changed, m.ignored), want, "{kind}");
        }
    }

    #[test]
    fn a_placebo_or_unclear_result_moves_no_belief() {
        let l = lesson("L", 0);
        let mem = |log: &[Outcome]| derive(log, std::slice::from_ref(&l), None)["L"].clone();
        let asked = o(outcome::ASKED, "a", "L", 1, "r");
        let alone = mem(std::slice::from_ref(&asked));
        for kind in [CHANGED, IGNORED, CORRECTED, outcome::UNCLEAR] {
            let placebo = Outcome { channel: outcome::PLACEBO.into(), ..o(kind, "b", "L", 5, "r") };
            assert_eq!(mem(&[asked.clone(), placebo]), alone, "placebo {kind}");
        }
        assert_eq!(mem(&[asked.clone(), Outcome { channel: "stop".into(), ..o(outcome::UNCLEAR, "a", "L", 5, "r") }]), alone);
    }

    #[test]
    fn one_sessions_regrets_blame_a_lesson_at_most_its_corrections() {
        let l = lesson("L", 0);
        let mem = |log: &[Outcome]| derive(log, std::slice::from_ref(&l), None)["L"].clone();
        let asked = o(outcome::ASKED, "a", "L", 1, "r");
        // Five later fixes of session a's work, half a correction each: one session counts once.
        let mut log = vec![asked.clone()];
        log.extend((0..5).map(|i| regret("a", "L", 10 + i, &format!("R{i}"), 0.5)));
        assert_eq!(mem(&log).regretted, regret_cap(0));
        assert_eq!(regret_cap(0), 1.0);
        // The engineer corrected session a twice (other lessons): its regrets may blame up to two.
        let mut twice = log.clone();
        twice.extend([o(CORRECTED, "a", "X", 3, "r"), o(outcome::VETOED, "a", "Y", 4, "r"), o(CORRECTED, "a", "Z", 4, "r")]);
        assert_eq!(mem(&twice).regretted, 2.0);
        // Another session's regret is its own: it adds on top.
        let mut other = log.clone();
        other.extend([o(outcome::ASKED, "b", "L", 2, "r"), regret("b", "L", 20, "Rb", 0.5)]);
        assert_eq!(mem(&other).regretted, 1.5);
    }

    #[test]
    fn the_misses_a_regret_writes_leave_its_sessions_cap_unchanged() {
        let l = lesson("L", 0);
        let mem = |log: &[Outcome]| derive(log, std::slice::from_ref(&l), None)["L"].clone();
        // Session a asked L; five later fixes of its work, each folded into a lesson Mi that a graded check
        // of a found applying and a never asked: each regret blames L and writes a miss of Mi with itself as
        // cause, at its decision's time (learn.rs `credit_regrets`).
        let mut log = vec![o(outcome::ASKED, "a", "L", 1, "r")];
        for i in 0..5 {
            log.push(regret("a", "L", 10 + i, &format!("R{i}"), 0.5));
            log.push(Outcome { cause: format!("R{i}"), ..o(outcome::MISSED, "a", &format!("M{i}"), 10 + i, "r") });
        }
        assert_eq!(mem(&log).regretted, regret_cap(0), "the regrets' own misses are no corrections");
        // A team view carries the cause as a tag: the same.
        let tagged: Vec<Outcome> =
            log.iter().map(|x| Outcome { cause: if x.cause.is_empty() { String::new() } else { outcome::tag(&x.cause) }, ..x.clone() }).collect();
        assert_eq!(mem(&tagged).regretted, regret_cap(0));
        // The misses two corrections of the engineer's found (no cause) still count: the cap is two.
        let mut corrected = log.clone();
        corrected.extend([o(outcome::MISSED, "a", "X", 3, "r"), o(outcome::MISSED, "a", "Y", 4, "r")]);
        assert_eq!(mem(&corrected).regretted, 2.0);
    }

    #[test]
    fn a_veto_outweighs_a_correction_and_a_change_in_its_session() {
        let l = lesson("L", 0);
        let log = vec![o(CHANGED, "a", "L", 1, "r"), o(CORRECTED, "a", "L", 2, "r"), o(outcome::VETOED, "a", "L", 3, "r"), o(CORRECTED, "b", "L", 4, "r")];
        let m = &derive(&log, std::slice::from_ref(&l), None)["L"];
        assert_eq!((m.changed, m.ignored, m.corrected, m.vetoed), (0, 0, 1, 1));
        assert_eq!((m.alpha(), m.beta()), (1.0, (1 + 2 + 3) as f64));
        // The veto's change still deposited a use: activation counts it.
        let without = derive(&log[1..], std::slice::from_ref(&l), None)["L"].activation;
        assert!(m.activation > without, "{} {without}", m.activation);
        // With equal counts, a vetoed lesson's belief is below a corrected one's.
        let only = |kind: &str| derive(&[o(kind, "a", "L", 1, "r")], std::slice::from_ref(&l), None)["L"].mean();
        assert!(only(outcome::VETOED) < only(CORRECTED), "{} {}", only(outcome::VETOED), only(CORRECTED));
        assert_eq!(only(outcome::VETOED), 1.0 / 5.0);
    }

    fn regret(session: &str, lesson: &str, ms: i64, cause: &str, credit: f64) -> Outcome {
        Outcome { cause: cause.into(), credit, ..o(outcome::REGRETTED, session, lesson, ms, "r") }
    }

    #[test]
    fn a_regret_lowers_belief_less_than_a_correction() {
        let l = lesson("L", 0);
        let mem = |log: &[Outcome]| derive(log, std::slice::from_ref(&l), None)["L"].clone();
        let asked = o(outcome::ASKED, "a", "L", 1, "r");
        let full = mem(&[asked.clone(), regret("a", "L", 9, "R1", 1.0)]);
        assert_eq!((full.regretted, full.beta(), full.mean()), (1.0, 2.0, 1.0 / 3.0));
        assert_eq!(mem(&[asked.clone(), o(CORRECTED, "a", "L", 9, "r")]).mean(), 1.0 / 4.0);
        // Duplicate lines of one regret count once; two regrets add up; line order does not matter.
        let log = vec![asked.clone(), regret("a", "L", 9, "R1", 0.25), regret("a", "L", 9, "R1", 0.25), regret("a", "L", 12, "R2", 0.5)];
        assert_eq!(mem(&log).regretted, 0.75);
        let mut reversed = log.clone();
        reversed.reverse();
        assert_eq!(mem(&reversed), mem(&log));
        // On top of the session's result, never a use: activation is unchanged.
        let changed = mem(&[asked.clone(), o(CHANGED, "a", "L", 5, "r"), regret("a", "L", 9, "R1", 0.5)]);
        assert_eq!((changed.changed, changed.regretted), (1, 0.5));
        assert_eq!(mem(&[asked.clone(), regret("a", "L", 9, "R1", 1.0)]).activation, mem(std::slice::from_ref(&asked)).activation);
        // A correction or a veto of the deciding session, recorded before or after the regret, takes it.
        for kind in [CORRECTED, outcome::VETOED] {
            let late = mem(&[asked.clone(), regret("a", "L", 9, "R1", 0.5), o(kind, "a", "L", 12, "r")]);
            assert_eq!(late.regretted, 0.0, "{kind}");
            assert_eq!(late.beta(), mem(&[asked.clone(), o(kind, "a", "L", 12, "r")]).beta(), "{kind}");
        }
        let other = mem(&[asked.clone(), o(CORRECTED, "b", "L", 3, "r"), regret("a", "L", 9, "R1", 0.5)]);
        assert_eq!(other.regretted, 0.5, "another session's correction leaves the regret");
    }

    #[test]
    fn an_older_stores_hive_verdicts_change_no_belief_and_no_tick() {
        let l = lesson("L", 0);
        let mem = |log: &[Outcome]| derive(log, std::slice::from_ref(&l), None)["L"].clone();
        let asked = o(outcome::ASKED, "a", "L", 1, "r");
        for kind in ["verified", "rejected", "hacked"] {
            let old = Outcome { cause: "g/t1.v1".into(), ..o(kind, "a", "L", 9, "r") };
            assert_eq!(mem(&[asked.clone(), old.clone()]), mem(std::slice::from_ref(&asked)), "{kind}");
            assert_eq!(Clock::of(&[old]).now(None), 0, "{kind}");
        }
    }

    #[test]
    fn a_green_test_run_weighs_half_a_change_and_never_ticks() {
        let l = lesson("L", 0);
        let mem = |log: &[Outcome]| derive(log, std::slice::from_ref(&l), None)["L"].clone();
        let asked = o(outcome::ASKED, "a", "L", 1, "r");
        let t = mem(&[asked.clone(), o(outcome::TESTED, "a", "L", 9, "r"), o(outcome::TESTED, "a", "L", 10, "r")]);
        assert_eq!((t.tested, t.alpha(), t.changed), (1, 1.0 + TESTED_W, 0), "once per session, never a use");
        assert_eq!(t.activation, mem(std::slice::from_ref(&asked)).activation);
        assert_eq!(Clock::of(&[o(outcome::TESTED, "z", "L", 9, "r")]).now(None), 0, "no tick");
        // The session's correction or veto of the lesson outranks it, in any line order.
        for k in [outcome::CORRECTED, outcome::VETOED] {
            let log = [o(outcome::TESTED, "a", "L", 9, "r"), asked.clone(), o(k, "a", "L", 5, "r")];
            assert_eq!(mem(&log).tested, 0, "{k}");
        }
        assert_eq!(Weights { tested: 2.0, ..WEIGHTS }.tested, 2.0);
        assert_eq!(t.alpha_with(&Weights { tested: 2.0, ..WEIGHTS }), 3.0);
    }

    #[test]
    fn fractional_beta_sampling_is_unbiased_and_integer_beta_unchanged() {
        let n = 20_000;
        let mean = (0..n).map(|i| round_seeded(2.5, seed(&format!("s{i}"), "L")) as f64).sum::<f64>() / n as f64;
        assert!((mean - 2.5).abs() < 0.02, "{mean}");
        assert!((0..100).all(|i| round_seeded(3.0, i) == 3));
        // A lesson without regrets draws exactly what it drew before fractional β (pinned).
        let m = Memory { activation: 0.0, changed: 2, ignored: 1, corrected: 1, vetoed: 0, regretted: 0.0, tested: 0, consolidated: false, dormant: false };
        assert_eq!(m.weight("L", Some("s")), squash(0.0) * (0.5 + sample_beta(3, 4, seed("s", "L"))));
        assert_eq!(m.weight("L", Some("s")), 1.0618369485259265);
    }

    #[test]
    fn fractional_alpha_draws_unbiased_and_integer_unchanged() {
        // Integer parameters: `draw` is the draw `weight` made before fractional α existed.
        assert!((0..200).all(|i| draw(3.0, 4.0, i) == sample_beta(3, 4, i)));
        assert!((0..200).all(|i| draw(2.0, 2.5, i) == sample_beta(2, round_seeded(2.5, i ^ 0x5265_6772), i)));
        // A fractional α rounds unbiased: Beta(1.5, 1) draws average between Beta(1, 1)'s and Beta(2, 1)'s.
        let n = 20_000;
        let mean = |a: f64| (0..n).map(|i| draw(a, 1.0, seed(&format!("s{i}"), "L"))).sum::<f64>() / n as f64;
        let (lo, mid, hi) = (mean(1.0), mean(1.5), mean(2.0));
        assert!((mid - (lo + hi) / 2.0).abs() < 0.01, "{lo} {mid} {hi}");
        let m = Memory { tested: 2, ..derive(&[], &[lesson("L", 0)], None)["L"].clone() };
        let quarter = Weights { tested: 0.25, ..WEIGHTS };
        assert_eq!((m.alpha_with(&quarter), m.alpha(), m.beta_with(&quarter)), (1.5, 2.0, 1.0));
    }

    #[test]
    fn derived_memory_is_independent_of_log_order_so_merging_logs_merges_memory() {
        let ls = [lesson("L", 150), lesson("M", 0), Challenge { engineers: vec!["a@x".into(), "b@x".into()], ..lesson("N", 0) }];
        let mut log = sessions(12);
        for (k, s) in ["s002", "s005", "s009"].iter().enumerate() {
            log.push(o(CHANGED, s, "N", 300 * k as i64, "r"));
            log.push(o(IGNORED, s, "M", 300 * k as i64, "r"));
        }
        log.push(o(CORRECTED, "s007", "L", 800, "r"));
        let base = derive(&log, &ls, Some("s011"));
        let mut rng = Rng::new(7);
        for _ in 0..20 {
            let mut shuffled = log.clone();
            for i in (1..shuffled.len()).rev() {
                shuffled.swap(i, (rng.next_u64() % (i as u64 + 1)) as usize);
            }
            assert_eq!(derive(&shuffled, &ls, Some("s011")), base);
        }
        // Two halves merged in either order: the same memory.
        let (a, b) = log.split_at(log.len() / 2);
        assert_eq!(derive(&[b, a].concat(), &ls, Some("s011")), base);
        assert!(base["N"].consolidated && !base["M"].consolidated);
    }

    #[test]
    fn consolidation_needs_spread_uses_and_dormancy_waits_for_a_long_silence() {
        let l = lesson("L", 0);
        let mut log = sessions(80);
        for i in [1, 2, 3] {
            log.push(o(CHANGED, &format!("s{i:03}"), "L", 100 * (i + 1) + 1, "one-repo"));
        }
        let m = &derive(&log, std::slice::from_ref(&l), None)["L"];
        assert!(!m.consolidated, "one repo, one engineer");
        log.push(o(CHANGED, "s004", "L", 501, "other-repo"));
        let m = &derive(&log, std::slice::from_ref(&l), None)["L"];
        assert!(m.consolidated && !m.dormant);
        // An unused lesson: active at first, dormant after a long silence, active again after one use.
        let fresh = lesson("F", 0);
        assert!(!derive(&sessions(40), std::slice::from_ref(&fresh), None)["F"].dormant);
        let mut quiet = sessions(80);
        assert!(derive(&quiet, std::slice::from_ref(&fresh), None)["F"].dormant);
        quiet.push(o(CHANGED, "s079", "F", 8001, "r"));
        assert!(!derive(&quiet, &[fresh], None)["F"].dormant, "one use reactivates");
    }

    #[test]
    fn thompson_samples_are_stable_per_session_spread_when_uncertain_and_tight_when_proven() {
        assert_eq!(sample_beta(3, 4, seed("s", "L")), sample_beta(3, 4, seed("s", "L")));
        assert_ne!(sample_beta(3, 4, seed("s", "L")), sample_beta(3, 4, seed("t", "L")));
        let draws = |a, b| (0..400).map(|i| sample_beta(a, b, seed(&format!("s{i}"), "L"))).collect::<Vec<f64>>();
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        let sd = |v: &[f64]| (v.iter().map(|x| (x - mean(v)).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
        let (new, proven, harmful) = (draws(1, 1), draws(21, 3), draws(1, 21));
        assert!((mean(&new) - 0.5).abs() < 0.05 && sd(&new) > 0.25, "uniform: {} {}", mean(&new), sd(&new));
        assert!(mean(&proven) > 0.8 && sd(&proven) < 0.1);
        assert!(mean(&harmful) < 0.1);
        assert!(new.iter().chain(&proven).all(|x| *x > 0.0 && *x < 1.0));
        let m = Memory { activation: 0.0, changed: 20, ignored: 2, corrected: 0, vetoed: 0, regretted: 0.0, tested: 0, consolidated: false, dormant: false };
        assert!(m.weight("L", Some("s")) > 1.2 && m.weight("L", Some("s")) < 1.5);
    }

    #[test]
    fn resolve_reads_declarations_first_then_edits_on_the_lessons_point() {
        let mk = |id: &str, q: &str| Challenge { id: id.into(), question: q.into(), ..Default::default() };
        let all = vec![
            mk("Idrain", "Is the per-turn hint place drained by an explicit consumer?"),
            mk("Ijudge", "Is the replay judge exactly the named Vertex model?"),
            mk("Ihost", "Does the replay run on live hosts?"),
            mk("x1", "Is the plan complete?"),
            mk("x2", "Is the plan tested?"),
            mk("x3", "Is the plan small?"),
        ];
        let asked: Vec<&Challenge> = all[..3].iter().collect();
        let fin = "Revised.\n`Ijudge`: applies — switched to Vertex.\n- Ihost: skip (offline task)\n";
        let before = "Add a per-turn hint budget to TextNet; plan: reset arc on the starting transition";
        let edits = "src/TextNet.java drain the hint place with an explicit consumer transition";
        let (changed, ignored) = resolve(&asked, &all, before, edits, fin, NOVEL_WORDS);
        assert_eq!((changed, ignored), (vec!["Idrain".to_string(), "Ijudge".to_string()], vec!["Ihost".to_string()]));
        // No change and nothing declared: ignored.
        assert_eq!(resolve(&asked, &all, before, "", "Done.", NOVEL_WORDS).0, Vec::<String>::new());
        // One new word is not enough; words the work already had do not count.
        assert!(resolve(&asked[..1], &all, before, "renamed the hint label, drained", "", NOVEL_WORDS).0.is_empty());
        assert!(resolve(&asked[..1], &all, "the hint place is drained by an explicit consumer", edits, "", NOVEL_WORDS).0.is_empty());
    }

    #[test]
    fn shuffle_is_a_stable_permutation_per_session() {
        let mut a: Vec<u32> = (0..8).collect();
        let mut b = a.clone();
        shuffle(&mut a, "s1");
        shuffle(&mut b, "s1");
        assert_eq!(a, b);
        let mut sorted = a.clone();
        sorted.sort();
        assert_eq!(sorted, (0..8).collect::<Vec<_>>());
        let firsts: HashSet<u32> = (0..40)
            .map(|i| {
                let mut v: Vec<u32> = (0..8).collect();
                shuffle(&mut v, &format!("s{i}"));
                v[0]
            })
            .collect();
        assert!(firsts.len() >= 5, "{firsts:?}");
    }
}
