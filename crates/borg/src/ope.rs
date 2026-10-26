//! `borg ope`: off-policy estimates of the usefulness belief's hand-set weights (hive.md
//! L6), from the outcome log alone. Reports, never applies: no constant changes.
//!
//! **What is estimated.** The per-ask change rate under a candidate policy: of the lessons a check
//! asks, the share the drone then changed its work on. The candidates differ from the policy in force
//! in one weight of the belief (memory.rs `Weights`, learn.rs `VERIFIER_REGRET`), the others held.
//!
//! **Which records count.** The weights reach a logged propensity on one path only: the Thompson slot
//! draw over a pool (conscience.rs `budget_pool`/`widen_pool` and `propensity`, the bm25 and the
//! non-graded paths), whose `asked` records carry the pool (outcome.rs `pool`, `base`, `scoped`). On the
//! default graded path p comes from the model's grades (0 or 1) or the learned cut (attention.rs), both
//! free of these weights: a candidate's p equals the logged p there, so those records say nothing about
//! the weights. The weights do order the shortlist the model grades, but nothing there is randomised or
//! logged. So a decision is an `asked` record with p and a pool; the rest is skipped and counted
//! (`Skipped`): no p, p = 0, no pool, no result yet, and `graded` records.
//!
//! **Leak-free replay.** For each decision the beliefs come from memory.rs `derive` over the outcomes
//! before its ms (sorted once, stably), under the candidate's weights, then the same `propensity` the
//! live path ran, on the logged pool with score = base × (0.5 + the candidate's mean) and the
//! decision's session. A lesson's counts depend only on the records that name it, so `evaluate`
//! derives over those records alone (an index per lesson), which keeps a run linear in the log. The
//! logged pool is held fixed: other weights could move a lesson across the LOW/BLOCK bars or change
//! the widen path's forced picks, which the replay does not model. "Before its ms" is what the live
//! draw saw only when every record arrived by its ms: a team record arrives a sync round after its ms,
//! so the draw may not have seen it. A decision whose replay under the current weights misses its
//! logged p (by more than `AGREE`) was drawn from other records than the replay's, and its weights
//! would be biased: it is counted (`Report::drifted`) and left out of every estimate.
//!
//! **The estimate.** Self-normalised inverse propensity scoring over the asked lessons, w = p under the
//! candidate / logged p, reward 1 when the session's result after the ask is a change; the effective
//! sample size (Σw)²/Σw²; a 95% interval from a bootstrap over whole sessions (a session's rewards are
//! not independent), seeded, so a run is deterministic.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::Serialize;

use crate::conscience::{self, Challenge};
use crate::memory::{self, Memory, Weights};
use crate::outcome::{self, Outcome};
use crate::{collective, learn};

/// One hand-set weight: its name, current value, the outcome kinds it weighs (its evidence), and how a
/// candidate sets it.
pub struct Knob {
    pub name: &'static str,
    pub current: f64,
    pub kinds: &'static [&'static str],
    /// Only `regretted` records of a verifier (cause ending `/verify`, learn.rs `credit_regrets`).
    pub verify: bool,
    pub set: fn(&mut Candidate, f64),
}

/// The seven hand-set weights: the veto's (`VETOED_W`), the hive verdicts' (`VERIFIED_W`, `REJECTED_W`,
/// `HACKED_W`), a regret's (`REGRET`), the verifier's share of a regret (learn.rs `VERIFIER_REGRET`) and
/// a green test run's (`TESTED_W`).
pub const KNOBS: [Knob; 7] = [
    Knob { name: "veto", current: memory::VETOED_W as f64, kinds: &[outcome::VETOED], verify: false, set: |c, v| c.weights.vetoed = v },
    Knob { name: "verified", current: memory::VERIFIED_W as f64, kinds: &[outcome::VERIFIED], verify: false, set: |c, v| c.weights.verified = v },
    Knob { name: "rejected", current: memory::REJECTED_W as f64, kinds: &[outcome::REJECTED], verify: false, set: |c, v| c.weights.rejected = v },
    Knob { name: "hacked", current: memory::HACKED_W as f64, kinds: &[outcome::HACKED], verify: false, set: |c, v| c.weights.hacked = v },
    Knob { name: "regret", current: memory::REGRET, kinds: &[outcome::REGRETTED], verify: false, set: |c, v| c.weights.regret = v },
    Knob { name: "verifier_regret", current: learn::VERIFIER_REGRET, kinds: &[outcome::REGRETTED], verify: true, set: |c, v| c.verifier = v },
    Knob { name: "tested", current: memory::TESTED_W, kinds: &[outcome::TESTED], verify: false, set: |c, v| c.weights.tested = v },
];

/// Candidates around a weight: half, itself, double. Derived from the current value, never hand-picked;
/// one knob moves at a time, the others stay current.
pub fn grid(current: f64) -> [f64; 3] {
    [current / 2.0, current, current * 2.0]
}

/// A policy to replay: the belief's weights and the verifier's share of a regret.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub weights: Weights,
    pub verifier: f64,
}

/// The policy in force.
pub const CURRENT: Candidate = Candidate { weights: memory::WEIGHTS, verifier: learn::VERIFIER_REGRET };

/// One logged Thompson check: its `asked` record's index in the log, ms and session, the asked lessons
/// with their logged p and reward, and the pool (id, base, scoped).
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub at: usize,
    pub ms: i64,
    pub session: String,
    pub asked: Vec<(String, f64, f64)>,
    pub pool: Vec<(String, f64, bool)>,
}

/// What `decisions` left out, and why. `no_p`, `zero_p` and `pending` count asked lessons; `no_pool`
/// and `graded` count records.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct Skipped {
    /// An `asked` record without p (or with p not aligned with its lessons): older logs, BORG_ONLY.
    pub no_p: usize,
    /// A lesson asked with p = 0: no inverse weight exists.
    pub zero_p: usize,
    /// An `asked` record with p but no pool: the graded rule, the learned cut, a live ask, a team record,
    /// a log from before pools. The weights do not move them.
    pub no_pool: usize,
    /// An asked lesson with no result in its session at or after the ask yet.
    pub pending: usize,
    /// A `graded` record: its p comes from the model's grades or the learned cut, free of the weights.
    pub graded: usize,
}

/// A result's rank, as memory.rs folds a session: vetoed > corrected > changed > ignored.
fn rank(kind: &str) -> Option<u8> {
    match kind {
        outcome::VETOED => Some(3),
        outcome::CORRECTED => Some(2),
        outcome::CHANGED => Some(1),
        outcome::IGNORED | outcome::NOT_CORRECTED => Some(0),
        _ => None,
    }
}

/// The logged Thompson checks of `outcomes`, in log order, and what was skipped. A lesson's reward is
/// 1 when the strongest result of its session at or after the ask is a change, else 0: a correction or
/// a veto after the ask means asking did not help.
pub fn decisions(outcomes: &[Outcome]) -> (Vec<Decision>, Skipped) {
    let mut results: HashMap<(&str, &str), Vec<(i64, u8)>> = HashMap::new();
    for o in outcomes {
        if let Some(r) = rank(&o.kind) {
            for l in &o.lessons {
                results.entry((o.session.as_str(), l.as_str())).or_default().push((o.ms, r));
            }
        }
    }
    let mut skipped = Skipped::default();
    let mut out = Vec::new();
    for (at, o) in outcomes.iter().enumerate() {
        if o.kind == outcome::GRADED {
            skipped.graded += 1;
            continue;
        }
        // A free delivery (early, queued) asks nothing and draws from no pool the weights move.
        if o.kind != outcome::ASKED || outcome::free(o) {
            continue;
        }
        if o.p.is_empty() || o.p.len() != o.lessons.len() {
            skipped.no_p += o.lessons.len();
            continue;
        }
        let aligned = o.pool.len() == o.base.len() && o.pool.len() == o.scoped.len();
        if o.pool.is_empty() || !aligned || !o.lessons.iter().all(|l| o.pool.contains(l)) {
            skipped.no_pool += 1;
            continue;
        }
        let mut asked = Vec::new();
        for (l, &p) in o.lessons.iter().zip(&o.p) {
            if p <= 0.0 {
                skipped.zero_p += 1;
                continue;
            }
            let after = results.get(&(o.session.as_str(), l.as_str())).and_then(|v| v.iter().filter(|(ms, _)| *ms >= o.ms).map(|(_, r)| *r).max());
            match after {
                Some(r) => asked.push((l.clone(), p, if r == 1 { 1.0 } else { 0.0 })),
                None => skipped.pending += 1,
            }
        }
        if !asked.is_empty() {
            let pool = o.pool.iter().zip(&o.base).zip(&o.scoped).map(|((id, b), s)| (id.clone(), *b, *s)).collect();
            out.push(Decision { at, ms: o.ms, session: o.session.clone(), asked, pool });
        }
    }
    (out, skipped)
}

/// `outcomes` with each verifier regret's credit rescaled to the candidate's share: × `verifier` /
/// `VERIFIER_REGRET`, at most 1 (outcome.rs `regretted` caps a credit at 1).
fn rescaled(outcomes: &[Outcome], verifier: f64) -> Vec<Outcome> {
    let k = verifier / learn::VERIFIER_REGRET;
    outcomes
        .iter()
        .map(|o| match is_verifier_regret(o) {
            true => Outcome { credit: (o.credit * k).min(1.0), ..o.clone() },
            false => o.clone(),
        })
        .collect()
}

fn is_verifier_regret(o: &Outcome) -> bool {
    o.kind == outcome::REGRETTED && o.cause.ends_with("/verify")
}

/// The pool's lessons as `derive` needs them: their ids.
fn pool_lessons(d: &Decision) -> Vec<Challenge> {
    d.pool.iter().map(|(id, _, _)| Challenge { id: id.clone(), ..Default::default() }).collect()
}

/// Each pooled lesson's chance of a slot under weights `w`, given its counts `mem`: the logged pool
/// with each belief and score under `w`, through the live `propensity`.
fn slot_p(mem: &HashMap<String, Memory>, d: &Decision, w: &Weights) -> HashMap<String, f64> {
    let pool: Vec<Challenge> = d
        .pool
        .iter()
        .map(|(id, _, scoped)| {
            let m = &mem[id];
            Challenge {
                id: id.clone(),
                scope: if *scoped { vec![conscience::ANY.into()] } else { Vec::new() },
                belief: (m.alpha_with(w), m.beta_with(w)),
                ..Default::default()
            }
        })
        .collect();
    let scored: Vec<(f64, &Challenge)> = pool
        .iter()
        .zip(&d.pool)
        .map(|(c, (_, base, _))| {
            let (a, b) = c.belief;
            (base * (0.5 + a / (a + b)), c)
        })
        .collect();
    conscience::propensity(&scored, &d.session)
}

/// Each pooled lesson's chance of a slot under `cand`, as of the decision: beliefs from memory::derive
/// over the records of `log` before `d.ms` (the cut is made here: nothing at or after the decision
/// reaches it) with the candidate weights (verifier regrets' credit rescaled by cand.verifier /
/// VERIFIER_REGRET, at most 1), then conscience::propensity on the logged pool with s = base × (0.5 +
/// mean) and the decision's session.
pub fn replay(log: &[Outcome], d: &Decision, cand: &Candidate) -> HashMap<String, f64> {
    let prefix: Vec<Outcome> = log.iter().filter(|o| o.ms < d.ms).cloned().collect();
    let prefix = prefix.as_slice();
    let lessons = pool_lessons(d);
    let mem = match cand.verifier != learn::VERIFIER_REGRET && prefix.iter().any(is_verifier_regret) {
        true => memory::derive(&rescaled(prefix, cand.verifier), &lessons, None),
        false => memory::derive(prefix, &lessons, None),
    };
    slot_p(&mem, d, &cand.weights)
}

/// One estimate of a policy's per-ask change rate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Estimate {
    /// SNIPS: Σwr/Σw; None without weight.
    pub value: Option<f64>,
    /// The 95% bootstrap interval (0, 0 without a value).
    pub lo: f64,
    pub hi: f64,
    /// Effective sample size (Σw)²/Σw².
    pub ess: f64,
    /// Units (asked lessons) behind it.
    pub n: usize,
}

/// How far a replayed p may lie from the logged one and still count as the same draw: the logged p is
/// rounded (outcome.rs `round_p`), and the draw's estimate is Monte Carlo (conscience::P_DRAWS).
pub const AGREE: f64 = 0.01;

/// Resamples behind a CI: precision, not a knob (as conscience::P_DRAWS).
pub const RESAMPLES: usize = conscience::P_DRAWS;

/// The bootstrap's seed: fixed, so a run is deterministic.
const SEED: u64 = 0x006f_7065;

/// Self-normalised IPS over units (w = p_cand / p_logged, reward, session): Σwr/Σw; None when Σw = 0.
/// ESS = (Σw)²/Σw². CI: the 2.5th and 97.5th percentiles of `RESAMPLES` bootstrap resamples of whole
/// sessions (the session's rewards are not independent), drawn with memory::Rng from `seed`:
/// deterministic.
pub fn snips(units: &[(f64, f64, usize)], seed: u64) -> Estimate {
    let ratio = |sw: f64, swr: f64| (sw > 0.0).then(|| swr / sw);
    let sw: f64 = units.iter().map(|u| u.0).sum();
    let sw2: f64 = units.iter().map(|u| u.0 * u.0).sum();
    let value = ratio(sw, units.iter().map(|u| u.0 * u.1).sum());
    let ess = if sw2 > 0.0 { sw * sw / sw2 } else { 0.0 };
    // Per session (in session order, so the resamples do not depend on the units' order): Σw, Σwr.
    let mut by: BTreeMap<usize, (f64, f64)> = BTreeMap::new();
    for (w, r, s) in units {
        let e = by.entry(*s).or_default();
        e.0 += w;
        e.1 += w * r;
    }
    let groups: Vec<(f64, f64)> = by.into_values().collect();
    let mut rng = memory::Rng::new(seed);
    let mut values: Vec<f64> = Vec::new();
    if value.is_some() {
        for _ in 0..RESAMPLES {
            let (mut sw, mut swr) = (0.0, 0.0);
            for _ in 0..groups.len() {
                let g = groups[(rng.next_u64() % groups.len() as u64) as usize];
                sw += g.0;
                swr += g.1;
            }
            values.extend(ratio(sw, swr));
        }
    }
    values.sort_by(f64::total_cmp);
    let pct = |q: f64| values.get((q * (values.len().max(1) - 1) as f64).round() as usize).copied().unwrap_or(0.0);
    Estimate { value, lo: pct(0.025), hi: pct(0.975), ess, n: units.len() }
}

/// One candidate value of one knob.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Row {
    pub knob: &'static str,
    pub value: f64,
    pub est: Estimate,
    /// Records of the knob's kinds before the last decision (in the whole log without a decision):
    /// without any, the candidates cannot differ.
    pub evidence: usize,
}

/// A knob's best candidate (the highest estimate; ties go to the current value) and whether its CI
/// lower bound clears the current value's estimate. Never applied.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pick {
    pub knob: &'static str,
    pub current: f64,
    pub best: f64,
    pub clears: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    /// Records read, `asked` records among them (free deliveries left out: they ask nothing).
    pub records: usize,
    pub asked: usize,
    pub decisions: usize,
    /// Asked lessons behind the estimates (those of drifted decisions left out).
    pub units: usize,
    pub skipped: Skipped,
    /// Decisions whose replay under the current weights misses the logged p by more than `AGREE`: the
    /// live draw saw other records (module doc), so they are left out of every estimate.
    pub drifted: usize,
    /// The current weights' replay against the logged p: units within 0.01, of all, the largest |Δ|.
    pub agree: (usize, usize, f64),
    pub current: Estimate,
    pub rows: Vec<Row>,
    pub picks: Vec<Pick>,
}

/// Evaluates every knob's grid on `outcomes` (`decisions`, `replay`, `snips`).
pub fn evaluate(outcomes: &[Outcome]) -> Report {
    let (ds, skipped) = decisions(outcomes);
    let mut sorted: Vec<&Outcome> = outcomes.iter().collect();
    sorted.sort_by_key(|o| o.ms);
    // Per lesson, the positions (in `sorted`) of the records naming it: a lesson's counts depend on
    // those alone.
    let mut naming: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, o) in sorted.iter().enumerate() {
        for l in &o.lessons {
            naming.entry(l.as_str()).or_default().push(i);
        }
    }
    // Every (knob, value), and the distinct candidates behind them (the current one once).
    let mut cands: Vec<Candidate> = vec![CURRENT];
    let mut grid_rows: Vec<(usize, f64, usize)> = Vec::new();
    for (k, knob) in KNOBS.iter().enumerate() {
        for v in grid(knob.current) {
            let mut c = CURRENT;
            (knob.set)(&mut c, v);
            let u = cands.iter().position(|x| *x == c).unwrap_or_else(|| {
                cands.push(c);
                cands.len() - 1
            });
            grid_rows.push((k, v, u));
        }
    }
    let mut sessions: HashMap<&str, usize> = HashMap::new();
    let mut units: Vec<Vec<(f64, f64, usize)>> = vec![Vec::new(); cands.len()];
    let mut agree = (0, 0, 0.0f64);
    let (mut drifted, mut n_units) = (0, 0);
    for d in &ds {
        let cut = sorted.partition_point(|o| o.ms < d.ms);
        let mut at: Vec<usize> = d.pool.iter().flat_map(|(id, _, _)| naming.get(id.as_str()).into_iter().flatten().copied().take_while(|i| *i < cut)).collect();
        at.sort_unstable();
        at.dedup();
        let prefix: Vec<Outcome> = at.into_iter().map(|i| sorted[i].clone()).collect();
        let lessons = pool_lessons(d);
        let mem = memory::derive(&prefix, &lessons, None);
        let verify = prefix.iter().any(is_verifier_regret);
        let mut by_verifier: Vec<(f64, HashMap<String, Memory>)> = Vec::new();
        let s = sessions.len();
        let s = *sessions.entry(d.session.as_str()).or_insert(s);
        let ps: Vec<HashMap<String, f64>> = cands
            .iter()
            .map(|c| match verify && c.verifier != learn::VERIFIER_REGRET {
                true => {
                    if !by_verifier.iter().any(|(v, _)| *v == c.verifier) {
                        by_verifier.push((c.verifier, memory::derive(&rescaled(&prefix, c.verifier), &lessons, None)));
                    }
                    let m = &by_verifier.iter().find(|(v, _)| *v == c.verifier).expect("derived above").1;
                    slot_p(m, d, &c.weights)
                }
                false => slot_p(&mem, d, &c.weights),
            })
            .collect();
        // The current weights (candidate 0) must reproduce the logged draw.
        let mut same = true;
        for (id, logged, _) in &d.asked {
            let delta = (ps[0].get(id).copied().unwrap_or(0.0) - logged).abs();
            agree.0 += usize::from(delta <= AGREE);
            agree.1 += 1;
            agree.2 = agree.2.max(delta);
            same &= delta <= AGREE;
        }
        if !same {
            drifted += 1;
            continue;
        }
        n_units += d.asked.len();
        for (u, p) in ps.iter().enumerate() {
            for (id, logged, reward) in &d.asked {
                units[u].push((p.get(id).copied().unwrap_or(0.0) / logged, *reward, s));
            }
        }
    }
    let estimates: Vec<Estimate> = units.iter().map(|u| snips(u, SEED)).collect();
    let last = ds.iter().map(|d| d.ms).max().unwrap_or(i64::MAX);
    let evidence = |k: &Knob| outcomes.iter().filter(|o| o.ms < last && k.kinds.contains(&o.kind.as_str()) && (!k.verify || is_verifier_regret(o))).count();
    let rows: Vec<Row> =
        grid_rows.iter().map(|&(k, v, u)| Row { knob: KNOBS[k].name, value: v, est: estimates[u].clone(), evidence: evidence(&KNOBS[k]) }).collect();
    let current = estimates[0].clone();
    let picks = KNOBS
        .iter()
        .map(|k| {
            let of = rows.iter().filter(|r| r.knob == k.name);
            let mut best = of.clone().find(|r| r.value == k.current).expect("the grid holds the current value");
            for r in of {
                if r.est.value.unwrap_or(f64::NEG_INFINITY) > best.est.value.unwrap_or(f64::NEG_INFINITY) {
                    best = r;
                }
            }
            let clears = best.value != k.current && current.value.is_some_and(|c| best.est.value.is_some() && best.est.lo > c);
            Pick { knob: k.name, current: k.current, best: best.value, clears }
        })
        .collect();
    Report {
        records: outcomes.len(),
        asked: outcomes.iter().filter(|o| o.kind == outcome::ASKED && !outcome::free(o)).count(),
        decisions: ds.len(),
        units: n_units,
        skipped,
        drifted,
        agree,
        current,
        rows,
        picks,
    }
}

/// `x` without trailing zeros: 3, 1.5, 0.25.
fn num(x: f64) -> String {
    let s = format!("{x:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The report as text: counts, the agreement of the replay with the log, the reward, the grid, then a
/// table per knob with its best candidate.
pub fn render(r: &Report) -> String {
    let s = &r.skipped;
    let mut out = format!(
        "records: {}, asked: {}\ndecisions: {} with a pool and p > 0 ({} asked lessons)\nskipped: {} lessons without p, {} with p = 0, {} without a result yet; {} asked records without a pool, {} graded records (p free of the weights)\n",
        r.records, r.asked, r.decisions, r.units, s.no_p, s.zero_p, s.pending, s.no_pool, s.graded
    );
    if r.drifted > 0 {
        out.push_str(&format!(
            "left out: {} decision(s) whose replay under the current weights misses the logged p by more than {AGREE} (the live draw saw other records, e.g. a team record that arrived after the ask)\n",
            r.drifted
        ));
    }
    if r.units == 0 {
        out.push_str(match r.drifted {
            0 => "too thin: no logged decision carries a pool and a propensity above 0; constants unchanged\n",
            _ => "too thin: no logged decision left to estimate from; constants unchanged\n",
        });
        out.push_str("(only the Thompson slot draw over a logged pool depends on these weights: graded and learned-cut propensities do not, and the shortlist the model grades is not randomised)\n");
        return out;
    }
    out.push_str(&format!(
        "agreement: {} of {} replayed p within {AGREE} of the logged p under the current weights (max |Δ| {:.4})\n",
        r.agree.0, r.agree.1, r.agree.2
    ));
    out.push_str("reward: 1 when the session's strongest result after the ask is a change (asked_then_changed), 0 for an ignore, a correction or a veto\n");
    out.push_str("estimate: self-normalised IPS of the per-ask change rate; 95% CI from a bootstrap over sessions; ESS = (Σw)²/Σw²\n");
    out.push_str("grid: each weight ×0.5, ×1, ×2 around its current value, the others held\n");
    for (k, pick) in KNOBS.iter().zip(&r.picks) {
        let rows: Vec<&Row> = r.rows.iter().filter(|x| x.knob == k.name).collect();
        let evidence = rows.first().map_or(0, |x| x.evidence);
        out.push_str(&format!("\n{} (current {}, evidence {evidence} records)\n", k.name, num(k.current)));
        out.push_str(&format!("  {:<8} {:>9} {:>17} {:>8}\n", "value", "estimate", "95% CI", "ESS"));
        for x in rows {
            let est = x.est.value.map_or("-".to_string(), |v| format!("{v:.3}"));
            let ci = if x.est.value.is_some() { format!("[{:.3}, {:.3}]", x.est.lo, x.est.hi) } else { "-".into() };
            let mark = if x.value == k.current { " (current)" } else { "" };
            out.push_str(&format!("  {:<8} {est:>9} {ci:>17} {:>8.1}{mark}\n", num(x.value), x.est.ess));
        }
        let best = if pick.best == k.current { format!("{} (current)", num(pick.best)) } else { num(pick.best) };
        out.push_str(&format!("  best: {best}; its CI lower bound clears the current estimate: {}\n", if pick.clears { "yes" } else { "no" }));
    }
    out.push_str("\nconstants unchanged\n");
    out
}

/// The outcomes beliefs were derived from live (outcome.rs `read_team`), with this machine's records
/// as written: the team view tags their causes, and a verifier regret is known by its raw cause.
fn store_view(lessons: &Path) -> Vec<Outcome> {
    // One read of each file: the local records and the team's beside them never overlap.
    let (mut out, team) = outcome::team_parts(lessons, outcome::on());
    out.extend(team);
    out
}

/// `borg ope [--dir DIR | --file OUTCOMES.jsonl] [--json]`: the store of the collective DIR routes to
/// (the working directory by default), or one outcomes file alone.
pub fn main(args: &[&str]) -> i32 {
    let usage = || {
        eprintln!("usage: borg ope [--dir DIR | --file OUTCOMES.jsonl] [--json]");
        2
    };
    let (mut dir, mut file, mut json): (Option<String>, Option<String>, bool) = (None, None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match (*a, dir.is_none() && file.is_none()) {
            ("--json", _) => json = true,
            ("--dir", true) => dir = Some(it.next().map(|d| d.to_string())).flatten(),
            ("--file", true) => file = Some(it.next().map(|f| f.to_string())).flatten(),
            _ => return usage(),
        }
        if matches!(*a, "--dir" | "--file") && dir.is_none() && file.is_none() {
            return usage();
        }
    }
    let (source, outcomes) = match file {
        Some(f) => (format!("file {f}"), outcome::read(Path::new(&f))),
        None => {
            let cwd = dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_default().display().to_string());
            match collective::load().map(|cfg| collective::route(cfg.as_ref(), &cwd, &learn::home())) {
                Ok(collective::Route::Collective { name, home, .. }) => {
                    (format!("collective {name} ({})", home.display()), store_view(&home.join("lessons.jsonl")))
                }
                Ok(collective::Route::Off) => {
                    println!("off: {cwd} belongs to no collective; no outcomes are kept here");
                    return 0;
                }
                Err(e) => {
                    eprintln!("borg ope: invalid collectives config ({e}); everything is off");
                    return 1;
                }
            }
        }
    };
    let report = evaluate(&outcomes);
    match json {
        true => println!("{}", serde_json::json!({ "source": source, "report": report })),
        false => print!("{source}\n{}", render(&report)),
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::{ASKED, CHANGED, GRADED, IGNORED, VETOED};
    use std::collections::HashSet;

    fn o(kind: &str, session: &str, lessons: &[&str], ms: i64) -> Outcome {
        Outcome {
            kind: kind.into(),
            session: session.into(),
            lessons: lessons.iter().map(|l| l.to_string()).collect(),
            ms,
            repo: "r".into(),
            ..Default::default()
        }
    }

    /// An `asked` record of `asked` from `pool` (ids with base 1, unscoped) with logged `p`.
    fn ask(session: &str, asked: &[&str], p: &[f64], pool: &[&str], ms: i64) -> Outcome {
        Outcome {
            p: p.to_vec(),
            pool: pool.iter().map(|l| l.to_string()).collect(),
            base: vec![1.0; pool.len()],
            scoped: vec![false; pool.len()],
            ..o(ASKED, session, asked, ms)
        }
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("borg-ope-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(path: &Path, lines: &[String]) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn json(x: &Outcome) -> String {
        serde_json::to_string(x).unwrap()
    }

    #[test]
    fn replaying_the_current_weights_reproduces_the_logged_p() {
        let home = tmp("agree");
        let lessons = home.join("lessons.jsonl");
        let ids = ["U1", "U2", "U3", "U4", "S1", "S2", "S3", "S4"];
        let ls: Vec<String> = ids
            .iter()
            .map(|id| {
                let scope: Vec<&str> = if id.starts_with('S') { vec!["Beacon"] } else { vec![] };
                serde_json::json!({"id": id, "origin_ms": 0, "question": format!("question of {id}"), "scope": scope}).to_string()
            })
            .collect();
        write(&lessons, &ls);
        let outcomes = home.join("outcomes.jsonl");
        // History: changes, ignores, a veto, a regret and hive verdicts.
        write(
            &outcomes,
            &[
                o(ASKED, "h0", &["U1", "U2", "S1"], 100),
                o(CHANGED, "h0", &["U1"], 110),
                o(IGNORED, "h0", &["U2"], 120),
                o(ASKED, "h1", &["U3", "S2"], 200),
                o(VETOED, "h1", &["U3", "S2"], 210),
                o(ASKED, "h2", &["U4"], 300),
                Outcome { cause: "R1".into(), credit: 0.5, ..o(outcome::REGRETTED, "h2", &["U4"], 310) },
                Outcome { cause: "g/t1.v1".into(), ..o(outcome::VERIFIED, "h3", &["S3"], 400) },
                Outcome { cause: "g/t2.v1".into(), ..o(outcome::HACKED, "h3", &["U2"], 410) },
            ]
            .map(|x| json(&x)),
        );
        // Three checks through the live chain: load as the session, rank, pool, slots, propensity.
        let mut logged = Vec::new();
        for (k, s) in ["d0", "d1", "d2"].iter().enumerate() {
            let ms = 1000 * (k as i64 + 1);
            let cs = conscience::load_for(&lessons, None, Some(s));
            // `rank`'s score, relevance × weight × trust / NEUTRAL, with a relevance that keeps all eight
            // over the low bar.
            let relevance = |i: usize, c: &Challenge| (1.0 + 0.07 * i as f64) / c.weight;
            let mut ranked: Vec<(f64, &Challenge)> =
                cs.iter().enumerate().map(|(i, c)| (relevance(i, c) * c.weight * c.trust / crate::trust::NEUTRAL, c)).collect();
            ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
            let pool = conscience::budget_pool(&ranked, &HashSet::new());
            assert!(pool.iter().filter(|(_, c)| c.scope.is_empty()).count() > conscience::GENERIC_CAP, "the generic slots overflow");
            let p = conscience::propensity(&pool, s);
            let asked = conscience::in_scope_first(conscience::explored(pool.iter().copied()));
            let record = Outcome {
                p: asked.iter().map(|c| outcome::round_p(p[&c.id])).collect(),
                pool: pool.iter().map(|(_, c)| c.id.clone()).collect(),
                base: pool.iter().map(|(sc, c)| conscience::pool_base(*sc, c)).collect(),
                scoped: pool.iter().map(|(_, c)| !c.scope.is_empty()).collect(),
                ..o(ASKED, s, &asked.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ms)
            };
            logged.extend(record.p.iter().copied());
            let mut lines = vec![json(&record)];
            for (i, c) in asked.iter().enumerate() {
                lines.push(json(&o(if i % 2 == 0 { CHANGED } else { IGNORED }, s, &[&c.id], ms + 10)));
            }
            write(&outcomes, &lines);
        }
        assert!(logged.iter().any(|p| *p > 0.0 && *p < 1.0), "{logged:?}");
        let log = outcome::read_team(&lessons);
        let r = evaluate(&log);
        assert_eq!(r.decisions, 3);
        assert_eq!((r.agree.0, r.agree.1), (logged.len(), logged.len()), "{:?}", r.agree);
        assert!(r.agree.2 <= 1e-4, "within round_p: {:?}", r.agree);
        // The public replay over the whole prefix agrees too (evaluate derives over the pool's records).
        let (ds, _) = decisions(&log);
        for d in &ds {
            let prefix: Vec<Outcome> = log.iter().filter(|x| x.ms < d.ms).cloned().collect();
            let p = replay(&prefix, d, &CURRENT);
            assert!(d.asked.iter().all(|(id, logged, _)| (p[id] - logged).abs() <= 1e-4), "{p:?} {:?}", d.asked);
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A world of `sessions` checks over six unscoped lessons of equal base, three slots: V1-V3 carry
    /// one early veto each, G1-G3 none. Logged under the current weights as the live path does, beliefs
    /// derived from the log so far; `helpful_vetoed` says which three the drone changes its work on.
    fn veto_world(sessions: usize, helpful_vetoed: bool) -> Vec<Outcome> {
        let ids = ["V1", "V2", "V3", "G1", "G2", "G3"];
        let ls: Vec<Challenge> = ids.iter().map(|id| Challenge { id: id.to_string(), ..Default::default() }).collect();
        let mut log: Vec<Outcome> = ["V1", "V2", "V3"].iter().map(|v| o(VETOED, "pre", &[v], 0)).collect();
        for i in 0..sessions {
            let s = format!("s{i:03}");
            let ms = 10 * (i as i64 + 1);
            let mem = memory::derive(&log, &ls, Some(&s));
            let cs: Vec<Challenge> = ls
                .iter()
                .map(|c| {
                    let m = &mem[&c.id];
                    Challenge { explore: m.weight(&c.id, Some(&s)) / m.weight(&c.id, None), belief: (m.alpha(), m.beta()), ..c.clone() }
                })
                .collect();
            let pool: Vec<(f64, &Challenge)> = cs.iter().map(|c| (0.5 + c.belief.0 / (c.belief.0 + c.belief.1), c)).collect();
            let p = conscience::propensity(&pool, &s);
            let asked = conscience::in_scope_first(conscience::explored(pool.iter().copied()));
            let names: Vec<&str> = asked.iter().map(|c| c.id.as_str()).collect();
            log.push(ask(&s, &names, &names.iter().map(|id| outcome::round_p(p[*id])).collect::<Vec<_>>(), &ids, ms));
            for id in names {
                let helps = id.starts_with('V') == helpful_vetoed;
                log.push(o(if helps { CHANGED } else { IGNORED }, &s, &[id], ms + 1));
            }
        }
        log
    }

    fn rows<'a>(r: &'a Report, knob: &str) -> Vec<&'a Row> {
        r.rows.iter().filter(|x| x.knob == knob).collect()
    }

    #[test]
    fn a_planted_better_veto_weight_ranks_first() {
        let r = evaluate(&veto_world(40, false));
        assert_eq!(r.agree.0, r.agree.1, "{:?}", r.agree);
        let veto: Vec<f64> = rows(&r, "veto").iter().map(|x| x.est.value.unwrap()).collect();
        assert!(veto[2] > veto[1] && veto[1] > veto[0], "est(6) > est(3) > est(1.5): {veto:?}");
        let pick = r.picks.iter().find(|p| p.knob == "veto").unwrap();
        assert_eq!(pick.best, 6.0);
        assert!(rows(&r, "veto").iter().all(|x| x.est.ess > 0.0 && x.evidence == 3));
        // A knob without evidence cannot tell its candidates apart.
        let hacked: Vec<&Estimate> = rows(&r, "hacked").iter().map(|x| &x.est).collect();
        assert!(hacked.iter().all(|e| *e == hacked[0]) && rows(&r, "hacked")[0].evidence == 0);
        assert_eq!(r.picks.iter().find(|p| p.knob == "hacked").unwrap().best, memory::HACKED_W as f64, "a tie keeps the current value");
        // The mirror world, where the vetoed lessons are the helpful ones: a lighter veto ranks first.
        let r = evaluate(&veto_world(40, true));
        let veto: Vec<f64> = rows(&r, "veto").iter().map(|x| x.est.value.unwrap()).collect();
        assert!(veto[0] > veto[1] && veto[1] > veto[2], "{veto:?}");
        assert_eq!(r.picks.iter().find(|p| p.knob == "veto").unwrap().best, 1.5);
        assert!(render(&r).contains("constants unchanged"));
    }

    #[test]
    fn zero_and_missing_p_are_skipped_and_counted() {
        let log = vec![
            o(ASKED, "a", &["A"], 1),
            ask("b", &["B"], &[0.0], &["B", "X"], 2),
            Outcome { p: vec![0.5], ..o(ASKED, "c", &["C"], 3) },
            o(GRADED, "c", &["C", "Y"], 3),
            ask("d", &["D"], &[0.5], &["D", "Y"], 4),
            Outcome { p: vec![1.0], channel: "queued".into(), ..o(ASKED, "e", &["E"], 4) },
            Outcome { p: vec![0.5], channel: "early".into(), ..o(ASKED, "e", &["F"], 4) },
            o(CHANGED, "a", &["A"], 5),
            o(CHANGED, "b", &["B"], 5),
            o(CHANGED, "c", &["C"], 5),
        ];
        let (ds, skipped) = decisions(&log);
        assert!(ds.is_empty());
        assert_eq!(skipped, Skipped { no_p: 1, zero_p: 1, no_pool: 1, pending: 1, graded: 1 }, "free deliveries count nowhere");
        let r = evaluate(&log);
        assert_eq!((r.units, r.current.value, r.asked), (0, None, 4));
        assert!(render(&r).contains("too thin"), "{}", render(&r));
    }

    #[test]
    fn snips_is_the_mean_under_equal_weights_and_ess_counts_them() {
        let units: Vec<(f64, f64, usize)> = (0..40).map(|i| (1.0, if i % 4 == 0 { 1.0 } else { 0.0 }, i / 2)).collect();
        let e = snips(&units, 7);
        assert_eq!((e.value, e.ess, e.n), (Some(0.25), 40.0, 40));
        assert!(e.lo <= 0.25 && 0.25 <= e.hi && e.lo < e.hi, "{e:?}");
        assert_eq!(snips(&units, 7), e, "one seed, one interval");
        assert_eq!(snips(&[], 7).value, None);
        // Weights tilt the estimate and shrink the effective sample.
        let e = snips(&[(3.0, 1.0, 0), (1.0, 0.0, 1)], 7);
        assert_eq!((e.value, e.ess), (Some(0.75), 1.6));
    }

    /// The bootstrap resamples whole sessions: four sessions of ten identical rewards (two all 1, two
    /// all 0) are four independent results, not forty, and the interval is as wide as four allow.
    #[test]
    fn the_bootstrap_resamples_sessions_not_units() {
        let units: Vec<(f64, f64, usize)> = (0..40).map(|i| (1.0, if i / 10 < 2 { 1.0 } else { 0.0 }, i / 10)).collect();
        let e = snips(&units, 7);
        assert_eq!(e.value, Some(0.5));
        // Four sessions: all four drawn from one side has chance 2 × (1/2)^4 = 1/8 > 5%, so the 95%
        // interval reaches both ends. Forty independent units would give about 0.5 ± 0.16.
        assert_eq!((e.lo, e.hi), (0.0, 1.0), "{e:?}");
        let iid: Vec<(f64, f64, usize)> = units.iter().enumerate().map(|(i, u)| (u.0, u.1, i)).collect();
        let narrow = snips(&iid, 7);
        assert!(narrow.hi - narrow.lo < 0.5 * (e.hi - e.lo), "{narrow:?}");
    }

    #[test]
    fn grid_halves_and_doubles() {
        assert_eq!(grid(3.0), [1.5, 3.0, 6.0]);
        let current: Vec<f64> = KNOBS.iter().map(|k| k.current).collect();
        assert_eq!(
            current,
            [
                memory::VETOED_W as f64,
                memory::VERIFIED_W as f64,
                memory::REJECTED_W as f64,
                memory::HACKED_W as f64,
                memory::REGRET,
                learn::VERIFIER_REGRET,
                memory::TESTED_W
            ]
        );
        // Each knob sets its own weight.
        for k in &KNOBS {
            let mut c = CURRENT;
            (k.set)(&mut c, 99.0);
            assert_ne!(c, CURRENT, "{}", k.name);
        }
    }

    /// Four unscoped lessons over three slots (p depends on the beliefs), history `before` the check at
    /// ms 100, and the check logged with the p the current weights give on that history: the live draw.
    fn leak_world(before: &[Outcome]) -> Vec<Outcome> {
        let pool = ["A", "B", "C", "D"];
        let probe = ask("d", &["A", "B", "C"], &[1.0, 1.0, 1.0], &pool, 100);
        let (ds, _) = decisions(&[[before, &[probe.clone(), o(CHANGED, "d", &["A"], 110)]].concat()].concat());
        let p = replay(before, &ds[0], &CURRENT);
        let logged = Outcome { p: ["A", "B", "C"].iter().map(|l| outcome::round_p(p[*l])).collect(), ..probe };
        [before, &[logged, o(CHANGED, "d", &["A"], 110)]].concat()
    }

    #[test]
    fn replay_is_leak_free() {
        let history = [o(CHANGED, "h", &["A"], 1), o(IGNORED, "h", &["B"], 2)];
        let base = leak_world(&history);
        let veto = |ms: i64| [base.clone(), vec![o(VETOED, "later", &["A", "B"], ms)]].concat();
        let (ds, _) = decisions(&base);
        // The public replay takes the whole log and cuts it at the decision itself.
        let at = |log: &[Outcome]| replay(log, &ds[0], &CURRENT);
        assert_eq!(at(&veto(100)), at(&base), "a veto at the decision's ms is not seen");
        assert_eq!(at(&veto(500)), at(&base), "nor one after it");
        assert_ne!(at(&veto(99)), at(&base), "one before it is");
        // `evaluate` alike: what comes at or after a decision changes no estimate.
        let before = evaluate(&base);
        assert_eq!((before.decisions, before.drifted, before.agree.0, before.agree.1), (1, 0, 1, 1), "{:?}", before.agree);
        assert_eq!(evaluate(&veto(100)).rows, before.rows);
        assert_eq!(evaluate(&veto(500)).rows, before.rows);
        // A veto logged before the check that the live draw saw: replayed as seen.
        let seen = leak_world(&[history.to_vec(), vec![o(VETOED, "early", &["A", "B"], 99)]].concat());
        let r = evaluate(&seen);
        assert_eq!((r.drifted, r.units), (0, 1));
        assert_ne!(r.rows, before.rows, "it moved the draw");
    }

    /// A team record stamped before a check but synced after it: the live draw never saw it, the replay
    /// would. The current weights then miss the logged p, and the decision is left out, not estimated.
    #[test]
    fn a_record_the_live_draw_did_not_see_drops_its_decision() {
        let history = [o(CHANGED, "h", &["A"], 1), o(IGNORED, "h", &["B"], 2)];
        let late = [leak_world(&history), vec![o(VETOED, "peer", &["A", "B"], 50)]].concat();
        let r = evaluate(&late);
        assert_eq!((r.decisions, r.drifted, r.units), (1, 1, 0), "{:?}", r.agree);
        assert!(r.agree.0 < r.agree.1 && r.current.value.is_none());
        assert!(render(&r).contains("left out: 1 decision(s)"), "{}", render(&r));
    }

    #[test]
    fn verifier_regrets_are_rescaled_and_others_kept() {
        let log = vec![
            Outcome { cause: "R1/verify".into(), credit: 0.25, ..o(outcome::REGRETTED, "v", &["A"], 1) },
            Outcome { cause: "R1".into(), credit: 0.5, ..o(outcome::REGRETTED, "a", &["A"], 1) },
            Outcome { cause: "R2/verify".into(), credit: 0.75, ..o(outcome::REGRETTED, "v", &["A"], 1) },
        ];
        let credits: Vec<f64> = rescaled(&log, 2.0 * learn::VERIFIER_REGRET).iter().map(|x| x.credit).collect();
        assert_eq!(credits, [0.5, 0.5, 1.0], "doubled, capped at 1; the author's own regret unchanged");
    }

    #[test]
    fn replay_scales_with_the_store() {
        // 2k decisions over 20k records: pools of two unscoped lessons (p = 1, no draw), so this times the
        // replay's derives, not the propensity.
        let mut log = Vec::new();
        for i in 0..2000usize {
            let (a, b) = (format!("L{}", i % 200), format!("L{}", (i * 7 + 1) % 200));
            let s = format!("s{i}");
            let ms = 10 * i as i64 + 5;
            log.push(ask(&s, &[&a], &[1.0], &[&a, &b], ms));
            log.push(o(if i % 3 == 0 { CHANGED } else { IGNORED }, &s, &[&a], ms + 1));
            for k in 0..8 {
                log.push(o(outcome::NOT_CORRECTED, &format!("x{i}"), &[&format!("L{}", (i + k) % 200)], ms + 2));
            }
        }
        assert_eq!(log.len(), 20_000);
        let started = std::time::Instant::now();
        let r = evaluate(&log);
        assert_eq!(r.decisions, 2000);
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
    }
}
