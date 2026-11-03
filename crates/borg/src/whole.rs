//! The whole-store Stop pool (the default; `BORG_STOP_POOL=shortlist` turns it off, internal evaluation "whole-store", not published): instead of a shortlist, the
//! consequence model (predict.rs) reads every lesson that the Stop path would allow, one compact line
//! each, up to a token budget.
//!
//! Allowed means what the shortlist already honours: not done this session and not of another system
//! (the caller's pool), in scope for where the session works (`conscience::in_scope`), not superseded,
//! and not dormant unless a strong word match recalls it (`conscience::rank_scoped`). When the allowed
//! lessons exceed the budget, the shortlist's order picks which fit: the Stop shortlist first (BM25 top
//! plus vector hits), then the rest by BM25, then lessons no word matches, newest first. Whatever is
//! picked is shown in store order, so the lesson block stays the same from turn to turn (a stable
//! prompt prefix for caching).

use std::collections::HashSet;

use crate::conscience::{self, Challenge, Names};

/// The default budget of the lesson block, in tokens (`BORG_STOP_POOL_TOKENS`). Baseline: the size
/// the internal evaluation "whole-store" passed with; it defines today's pool, which the learned pool
/// size (select.rs) starts from and keeps until the yield by fill position has data. Kept by dated
/// decision (2026-10-31): a smaller pool loses 0.89 [0.79, 1.00] applies per check (a team store); a
/// larger one has 7 judged applies past the budget and no latency measured above it.
pub const TOKENS: usize = 12_000;
/// Guard: a compact line keeps this many characters of the question...
pub const QUESTION_CHARS: usize = 240;
/// ...and this many of the consequence. Guard, both: one long lesson cannot take a share of the
/// budget many short ones would fill.
pub const CONSEQUENCE_CHARS: usize = 160;
/// Characters per token in every token estimate of borg (`cost`; the cost lines of `borg status` and
/// the trace's tool results report in it too): borg-trace's, the lowest crate's, so there is one.
pub use borg_trace::CHARS_PER_TOKEN;

/// The whole-store pool is the default (internal evaluation "whole-store", "Graded follow-up", not published: with graded applies it
/// passed); `BORG_STOP_POOL=shortlist` keeps the shortlist instead.
pub fn on() -> bool {
    !std::env::var("BORG_STOP_POOL").is_ok_and(|v| v.trim().eq_ignore_ascii_case("shortlist"))
}

/// `BORG_STOP_FORMAT=classic` shows the whole-store pool in the shortlist's layout (`predict::prompt`:
/// work first, `situations -> consequence`) instead of the compact lessons-first layout
/// (`predict::prompt_whole`). Only the lesson set differs from the shortlist then (internal evaluation "whole-store", not published,
/// arm C).
pub fn classic() -> bool {
    std::env::var("BORG_STOP_FORMAT").is_ok_and(|v| v.trim().eq_ignore_ascii_case("classic"))
}

/// Graded applies, the default: the model grades each lesson it applies, and the Stop blocks only for a
/// lesson graded 3 (`conscience::graded`); without a block, the 2s and 1s go free at the next prompt or
/// touch. `BORG_STOP_GRADED=off` keeps one undifferentiated `applies` list (any apply blocks, widened).
pub fn graded() -> bool {
    !std::env::var("BORG_STOP_GRADED").is_ok_and(|v| v.trim().eq_ignore_ascii_case("off"))
}

/// The lesson block's budget: `BORG_STOP_POOL_TOKENS`, else `TOKENS`.
pub fn tokens() -> usize {
    std::env::var("BORG_STOP_POOL_TOKENS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(TOKENS)
}

/// How the compact prompt shows a lesson (`line`): `A1`, `[id] question -> consequence`; or `A2`, the
/// same with the engineer's correction it was learned from, where the store holds one
/// (`Challenge::correction`; a synced peer's lesson has none and shows as `A1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    A1,
    A2,
}

impl Line {
    /// The format's name, as `BORG_LESSON_LINE` takes it and a `graded` record logs it (outcome.rs `line`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Line::A1 => "a1",
            Line::A2 => "a2",
        }
    }
}

/// The lesson line format: `BORG_LESSON_LINE=a1|a2`, `A2` by default (the pre-registered replay of
/// logged checks picked it over `A1`, internal evaluation "check replay", not published). An `A2` line
/// costs more tokens (`cost`), so fewer lessons fit the same budget. Evaluation switch: `a1` keeps the
/// line before 0.7.7.
#[must_use]
pub fn line_format() -> Line {
    if std::env::var("BORG_LESSON_LINE").is_ok_and(|v| v.trim().eq_ignore_ascii_case("a1")) { Line::A1 } else { Line::A2 }
}

/// One lesson on one line in the format `line_format` names (`line_with`).
pub fn line(c: &Challenge) -> String {
    let correction = (line_format() == Line::A2).then_some(c.correction.as_str());
    line_with(c, correction)
}

/// One lesson on one line: `[id] question -> consequence`, each part trimmed; with a `correction`
/// (not empty), `| the engineer's correction: "<correction>"` after it, flat and whole (the store keeps
/// at most learn.rs `EXCERPT_CHARS` of it).
pub fn line_with(c: &Challenge, correction: Option<&str>) -> String {
    let words = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = |s: &str, n: usize| crate::predict::clip(&words(s), n);
    let q = flat(&c.question, QUESTION_CHARS);
    let a1 = if c.consequence.trim().is_empty() { format!("[{}] {q}", c.id) } else { format!("[{}] {q} -> {}", c.id, flat(&c.consequence, CONSEQUENCE_CHARS)) };
    match correction.map(words).filter(|w| !w.is_empty()) {
        Some(said) => format!("{a1} | the engineer's correction: \"{said}\""),
        None => a1,
    }
}

/// A line's cost in tokens, estimated at `CHARS_PER_TOKEN` (and the newline).
pub fn cost(c: &Challenge) -> usize {
    (line(c).chars().count() + 1).div_ceil(CHARS_PER_TOKEN)
}

/// The lessons of `pool` the Stop path allows, in store order. `ranked` is `rank_scoped` over the same
/// pool and context: a dormant lesson stays only if it ranks there (a strong match).
pub fn eligible<'a>(pool: &'a [Challenge], ranked: &[(f64, &'a Challenge)], scope_text: &str) -> Vec<&'a Challenge> {
    let have = Names::of(scope_text);
    let successors = crate::trust::successors(pool);
    let recalled: HashSet<&str> = ranked.iter().map(|(_, c)| c.id.as_str()).collect();
    pool.iter()
        .filter(|c| !c.off_system && conscience::in_scope(c, &have) && !successors.contains_key(&c.id))
        .filter(|c| !c.dormant || recalled.contains(c.id.as_str()))
        .collect()
}

/// What the model is shown, in store order, and whether the budget forced a choice (`select_filled`
/// without the fill order).
pub fn select<'a>(
    eligible: Vec<&'a Challenge>,
    shortlist: impl FnOnce() -> Vec<&'a Challenge>,
    ranked: &[(f64, &'a Challenge)],
    tokens: usize,
) -> (Vec<&'a Challenge>, bool) {
    let f = select_filled(eligible, shortlist, ranked, tokens);
    (f.shown, f.fell)
}

/// How a pool filled its budget (`select_filled`).
#[derive(Debug, Clone)]
pub struct Filled<'a> {
    /// What the model is shown, in store order.
    pub shown: Vec<&'a Challenge>,
    /// Whether the budget forced a choice.
    pub fell: bool,
    /// The shown lessons in their fill order, the order they took the budget in (a check's record logs
    /// each apply's place in it: the yield of each fill position).
    pub order: Vec<&'a Challenge>,
    /// The lessons the budget dropped, in the order the fill met them: the full fill order goes on past
    /// the budget with them, `order.len()` first (a tail lesson's place, `tail`). Empty when all fit.
    pub dropped: Vec<&'a Challenge>,
}

/// The pool of `eligible` within `tokens` (`Filled`). All of `eligible` when it fits in `tokens`,
/// filled in the order `ranked`, then the rest newest first; otherwise lessons in the order `shortlist`
/// (called only then), `ranked`, then the rest newest first, each taken while it still fits.
pub fn select_filled<'a>(
    eligible: Vec<&'a Challenge>,
    shortlist: impl FnOnce() -> Vec<&'a Challenge>,
    ranked: &[(f64, &'a Challenge)],
    tokens: usize,
) -> Filled<'a> {
    let allowed: HashSet<&str> = eligible.iter().map(|c| c.id.as_str()).collect();
    let mut rest: Vec<&Challenge> = eligible.clone();
    rest.sort_by_key(|l| std::cmp::Reverse(l.origin_ms));
    let fits = eligible.iter().map(|c| cost(c)).sum::<usize>() <= tokens;
    let first = if fits { Vec::new() } else { shortlist() };
    let order = first.into_iter().chain(ranked.iter().map(|(_, c)| *c)).chain(rest);
    let (mut picked, mut dropped): (Vec<&Challenge>, Vec<&Challenge>) = (Vec::new(), Vec::new());
    let mut met: HashSet<&str> = HashSet::new();
    let mut used = 0;
    for c in order {
        if !allowed.contains(c.id.as_str()) || !met.insert(c.id.as_str()) {
            continue;
        }
        let k = cost(c);
        if fits || used + k <= tokens {
            used += k;
            picked.push(c);
        } else {
            dropped.push(c);
        }
    }
    if fits {
        return Filled { shown: eligible, fell: false, order: picked, dropped };
    }
    let shown: HashSet<&str> = picked.iter().map(|c| c.id.as_str()).collect();
    Filled { shown: eligible.into_iter().filter(|c| shown.contains(c.id.as_str())).collect(), fell: true, order: picked, dropped }
}

/// Lessons the budget dropped that a check shows the model anyway, at least (`tail_k`; all of them when
/// fewer). Guard: one sample per check is enough to measure the yield of fill positions past the
/// budget even where the share (`BOUNDARY_SHARE`) of the budget holds fewer.
pub const TAIL_SAMPLE: usize = 2;

/// The exploration share (0.7.7 on; `BORG_BOUNDARY=off` → 0): the last share of a pool's budget a check
/// fills by a draw over the fill order's next lessons (`explore`, the boundary), and the share of the
/// budget's tokens its tail sample may add past the budget (`tail_k`). Dated decision (2026-11-03, the
/// engineer's): 0.15, sized for the pre-registered switch test of the pool's order (about 100 tail
/// applies at grade ≥ 2 per evaluation window); the tail adds about this share of the budget's tokens,
/// however many lessons the budget dropped.
pub const BOUNDARY_SHARE: f64 = 0.15;

/// The exploration share in force: `BOUNDARY_SHARE`, 0 with `BORG_BOUNDARY=off` (evaluation switch:
/// the pool fills as before 0.7.7, its tail `TAIL_SAMPLE` lessons).
#[must_use]
pub fn boundary_share() -> f64 {
    if std::env::var("BORG_BOUNDARY").is_ok_and(|v| v.trim().eq_ignore_ascii_case("off")) { 0.0 } else { BOUNDARY_SHARE }
}

/// The tail sample's size for the dropped lessons' token `costs` at exploration share `s` of the
/// budget `tokens`: as many lessons of their mean cost as fit in ⌊s·tokens⌋, at least `TAIL_SAMPLE`, at
/// most all of them. It depends on the remainder alone, never on which lessons a draw takes, so each
/// one's chance stays k / n exactly.
#[must_use]
pub fn tail_k(costs: &[usize], s: f64, tokens: usize) -> usize {
    let n = costs.len();
    if n == 0 {
        return 0;
    }
    let mean = costs.iter().sum::<usize>().div_ceil(n).max(1);
    let room = (s * tokens as f64).floor() as usize;
    n.min(TAIL_SAMPLE.max(room / mean))
}

/// The tail sample of a pool the budget cut (`Filled::dropped`): `tail_k` of `dropped` at share `s` of
/// the budget `tokens`, drawn uniformly without replacement from `seed`, each with its place in `dropped`, in that order; and
/// each one's chance of the draw, k / n for k drawn of n, exactly (unrounded, as the graded record's
/// `pt` logs it; the summary folds the sample unweighted, select/summary.rs module doc: the tail).
/// Nothing, at chance 0, when nothing was dropped.
#[must_use]
pub fn tail<'a>(dropped: &[&'a Challenge], seed: u64, s: f64, tokens: usize) -> (Vec<(usize, &'a Challenge)>, f64) {
    let costs: Vec<usize> = dropped.iter().map(|c| cost(c)).collect();
    let (n, k) = (dropped.len(), tail_k(&costs, s, tokens));
    if k == 0 {
        return (Vec::new(), 0.0);
    }
    // The first k of a seeded Fisher-Yates shuffle of the places.
    let mut at: Vec<usize> = (0..n).collect();
    let mut rng = crate::memory::Rng::new(seed);
    for i in 0..k {
        let j = i + (rng.next_u64() % (n - i) as u64) as usize;
        at.swap(i, j);
    }
    let mut drawn = at[..k].to_vec();
    drawn.sort_unstable();
    (drawn.into_iter().map(|i| (i, dropped[i])).collect(), k as f64 / n as f64)
}

/// A pool filled with exploration (`explore`): a deterministic prefix, then a draw at its boundary and
/// a uniform tail sample, both over what the prefix left.
#[derive(Debug, Clone)]
pub struct Explored<'a> {
    /// The deterministic prefix: `select_filled` within ⌊(1 − s)·B⌋ (all of B when the eligible lessons
    /// fit in it, no exploration then). Its `dropped` is the remainder R the draws are over; a lesson's
    /// place in the full fill order is `filled.order.len()` + its place in R.
    pub filled: Filled<'a>,
    /// The tail sample (`tail`) of R, drawn first, each with its place in the fill order; and its
    /// chance k / n (0 without a tail).
    pub tail: Vec<(usize, &'a Challenge)>,
    pub pt: f64,
    /// The boundary draw: lessons of R not in the tail that fill the window W = B − cost(prefix),
    /// drawn without replacement with weight exp(−i/τ) at place i in R (Plackett–Luce), skipping any
    /// that no longer fits, until none fits; each with its place in the fill order.
    pub boundary: Vec<(usize, &'a Challenge)>,
    /// τ: how many lessons of R a deterministic fill of W would place (measured cost per lesson).
    pub tau: usize,
    /// The exploration share the pool was filled at, the tail's k and the remainder's n.
    pub s: f64,
    pub k: usize,
    pub n: usize,
    /// The boundary's window W and its draw's seed: what `chances` replays.
    pub window: usize,
    pub seed: u64,
}

impl Explored<'_> {
    /// The lessons shown past the prefix, boundary and tail (their places in the fill order).
    #[must_use]
    pub fn drawn(&self) -> usize {
        self.boundary.len() + self.tail.len()
    }

    /// Per lesson of R (aligned with `filled.dropped`): π_b, its chance of the boundary draw given the
    /// tail missed it (None where no draw could reach it: every lesson in the tail), estimated by
    /// `conscience::P_DRAWS` seeded replays of tail and boundary; and its overall chance to be shown,
    /// pt + (1 − pt)·π_b, exact in pt (the tail is drawn uniformly first). Computed on demand: only a
    /// pool that is logged (pools.rs) pays for the replays.
    #[must_use]
    pub fn chances(&self) -> (Vec<Option<f64>>, Vec<f64>) {
        let costs: Vec<usize> = self.filled.dropped.iter().map(|c| cost(c)).collect();
        inclusion(&costs, self.window, self.tau, self.k, self.seed, self.pt)
    }
}

/// The pool of `eligible` within `tokens` (B) with exploration at share `s` (`Explored`). All of
/// `eligible` that fits in B is shown as `select_filled` shows it, with nothing drawn. Otherwise the
/// deterministic prefix fills ⌊(1 − s)·B⌋; the tail sample draws k of the n lessons it left
/// (`tail`, from `seed`) first; the boundary then fills the rest of B from the remaining ones
/// (from `seed` ^ "boundary"), favouring those the fill met first. At s = 0 the prefix is today's fill and
/// the boundary is empty: a greedy fill leaves no dropped lesson that fits what remains. No tail beside
/// an empty prefix: the boundary alone fills B then, and a pool that shows nothing has no sample (it
/// alone would pay for a call the budget does not); `chances` replays the same rule.
pub fn explore<'a>(
    eligible: Vec<&'a Challenge>,
    shortlist: impl FnOnce() -> Vec<&'a Challenge>,
    ranked: &[(f64, &'a Challenge)],
    tokens: usize,
    s: f64,
    seed: u64,
) -> Explored<'a> {
    let fits = eligible.iter().map(|c| cost(c)).sum::<usize>() <= tokens;
    let prefix = if fits { tokens } else { ((1.0 - s) * tokens as f64).floor() as usize };
    let filled = select_filled(eligible, shortlist, ranked, prefix);
    let at = filled.order.len();
    let used: usize = filled.order.iter().map(|c| cost(c)).sum();
    let window = tokens.saturating_sub(used);
    let costs: Vec<usize> = filled.dropped.iter().map(|c| cost(c)).collect();
    let tau = greedy(&costs, window);
    let b_seed = seed ^ u64::from_be_bytes(*b"boundary");
    let (drawn, pt) = if at == 0 { (Vec::new(), 0.0) } else { tail(&filled.dropped, seed, s, tokens) };
    let mut boundary = draw_boundary(&costs, &drawn.iter().map(|(i, _)| *i).collect::<Vec<_>>(), window, tau, b_seed);
    let pt = if drawn.is_empty() { 0.0 } else { pt };
    let k = drawn.len();
    let tail = drawn.into_iter().map(|(i, c)| (at + i, c)).collect();
    boundary.sort_unstable();
    let boundary = boundary.into_iter().map(|i| (at + i, filled.dropped[i])).collect();
    let n = filled.dropped.len();
    Explored { filled, tail, pt, boundary, tau, s, k, n, window, seed: b_seed }
}

/// How many of `costs`, in order, a greedy fill with skips places in `window`.
fn greedy(costs: &[usize], window: usize) -> usize {
    let mut left = window;
    costs
        .iter()
        .filter(|&&k| {
            let fit = k <= left;
            if fit {
                left -= k;
            }
            fit
        })
        .count()
}

/// One boundary draw over the places of `costs` not in `taken` (`explore`): without replacement, each
/// with weight exp(−i/τ) at place i among those that still fit `window`, until none fits; the places
/// drawn, in draw order. None at τ = 0 (nothing fits).
fn draw_boundary(costs: &[usize], taken: &[usize], window: usize, tau: usize, seed: u64) -> Vec<usize> {
    let mut out = Vec::new();
    if tau == 0 {
        return out;
    }
    let mut rng = crate::memory::Rng::new(seed);
    let mut out_of = vec![false; costs.len()];
    for &i in taken {
        out_of[i] = true;
    }
    let mut open: Vec<usize> = (0..costs.len()).filter(|&i| !out_of[i]).collect();
    let mut left = window;
    loop {
        let fit: Vec<usize> = open.iter().copied().filter(|&i| costs[i] <= left).collect();
        let Some(&first) = fit.first() else { break };
        // Relative to the first that fits: the same draw as exp(−i/τ), without underflow far down.
        let w: Vec<f64> = fit.iter().map(|&i| (-((i - first) as f64) / tau as f64).exp()).collect();
        let mut u = rng.unit() * w.iter().sum::<f64>();
        let mut pick = *fit.last().unwrap_or(&first);
        for (&i, &wi) in fit.iter().zip(&w) {
            if u < wi {
                pick = i;
                break;
            }
            u -= wi;
        }
        left -= costs[pick];
        out.push(pick);
        open.retain(|&i| i != pick);
    }
    out
}

/// Per place of `costs` (`explore`'s remainder): π_b, the share of `conscience::P_DRAWS` replays of the
/// tail draw (`k` of them, as `explore` drew) and the boundary draw after it in which the boundary drew it, among those
/// whose tail missed it (None when every replay's tail took it); and the overall chance
/// pt + (1 − pt)·π_b. Replay r draws from its own seeds of `seed`'s stream.
fn inclusion(costs: &[usize], window: usize, tau: usize, k: usize, seed: u64, pt: f64) -> (Vec<Option<f64>>, Vec<f64>) {
    let n = costs.len();
    let (mut hit, mut open) = (vec![0usize; n], vec![0usize; n]);
    if tau > 0 {
        let mut seeds = crate::memory::Rng::new(seed ^ u64::from_be_bytes(*b"replayed"));
        let k = if pt > 0.0 { k } else { 0 };
        for _ in 0..crate::conscience::P_DRAWS {
            let mut rng = crate::memory::Rng::new(seeds.next_u64());
            // The tail's draw: the first k of a Fisher-Yates shuffle, as `tail`.
            let mut at: Vec<usize> = (0..n).collect();
            for i in 0..k {
                let j = i + (rng.next_u64() % (n - i) as u64) as usize;
                at.swap(i, j);
            }
            let taken = &at[..k];
            for i in at[k..].iter() {
                open[*i] += 1;
            }
            for i in draw_boundary(costs, taken, window, tau, seeds.next_u64()) {
                hit[i] += 1;
            }
        }
    }
    let pb: Vec<Option<f64>> = (0..n).map(|i| if tau == 0 { Some(0.0) } else { (open[i] > 0).then(|| hit[i] as f64 / open[i] as f64) }).collect();
    let pi = pb.iter().map(|b| pt + (1.0 - pt) * b.unwrap_or(0.0)).collect();
    (pb, pi)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lesson(id: &str, q: &str, ms: i64) -> Challenge {
        Challenge { id: id.into(), question: q.into(), consequence: "bad".into(), origin_ms: ms, ..Default::default() }
    }

    fn ids(v: &[&Challenge]) -> Vec<String> {
        v.iter().map(|c| c.id.clone()).collect()
    }

    #[test]
    fn a_line_is_trimmed_and_flat() {
        let c = Challenge { id: "L1".into(), question: format!("Did I\n check {}?", "x".repeat(500)), consequence: "c\nd".into(), ..Default::default() };
        let l = line(&c);
        assert!(l.starts_with("[L1] Did I check x") && l.ends_with("… -> c d") && !l.contains('\n'), "{l}");
        assert!(l.chars().count() < QUESTION_CHARS + 20, "{}", l.len());
        assert_eq!(line(&Challenge { id: "q".into(), question: "Q?".into(), ..Default::default() }), "[q] Q?");
    }

    #[test]
    fn an_a2_line_carries_the_correction_and_falls_back() {
        let said = format!("no,\n  drain the budget {}", "y".repeat(1400));
        let c = Challenge {
            id: "L1".into(),
            question: "Is the budget drained?".into(),
            consequence: "a stuck net".into(),
            correction: said.clone(),
            ..Default::default()
        };
        let a1 = line_with(&c, None);
        assert_eq!(a1, "[L1] Is the budget drained? -> a stuck net");
        let a2 = line_with(&c, Some(&said));
        assert_eq!(a2, format!("{a1} | the engineer's correction: \"no, drain the budget {}\"", "y".repeat(1400)), "flat, never clipped");
        assert_eq!(line_with(&c, Some(" \n ")), a1, "an empty correction is no correction");
        // A lesson without a resolvable correction (a synced peer's) costs and reads as before.
        let peer = Challenge { correction: String::new(), ..c.clone() };
        assert_eq!(line(&peer), a1);
        assert!(cost(&c) > cost(&peer), "the budget counts the correction");
        assert_eq!((Line::A1.name(), Line::A2.name()), ("a1", "a2"));
    }

    #[test]
    fn the_pool_honours_scope_system_supersession_and_dormancy() {
        let mut pool = vec![
            lesson("free", "generic question about tests", 1),
            Challenge { scope: vec!["Orion".into()], ..lesson("orion", "orion drain rule", 2) },
            Challenge { scope: vec!["Beacon".into()], ..lesson("beacon", "beacon relaxed baseline", 3) },
            Challenge { off_system: true, ..lesson("other", "another system", 4) },
            lesson("old", "the old rule about locks", 5),
            Challenge { supersedes: vec!["old".into()], source: "queen".into(), ..lesson("new", "the new rule about locks", 6) },
            Challenge { dormant: true, ..lesson("sleepy", "dormant lesson about caches", 7) },
            Challenge { dormant: true, ..lesson("woken", "dormant lesson about websocket deadlocks", 8) },
        ];
        pool[5].trust = 0.9;
        let scope = "repo orion-text\nwork on the websocket deadlocks";
        let ranked = conscience::rank_scoped(&pool, "websocket deadlocks", scope);
        assert_eq!(ids(&eligible(&pool, &ranked, scope)), ["free", "orion", "new", "woken"]);
    }

    #[test]
    fn everything_fits_in_store_order_else_the_shortlist_order_fills_the_budget() {
        let pool: Vec<Challenge> = (0..10).map(|i| lesson(&format!("L{i}"), &format!("question number {i} about topic{i}"), i)).collect();
        let all: Vec<&Challenge> = pool.iter().collect();
        let each = cost(&pool[0]);
        let mut called = false;
        let ranked = vec![(3.0, &pool[7]), (2.0, &pool[5]), (1.0, &pool[2])];
        let f = select_filled(
            all.clone(),
            || {
                called = true;
                vec![]
            },
            &ranked,
            10 * each,
        );
        assert_eq!((f.shown.len(), f.fell, called, f.dropped.len()), (10, false, false, 0), "fits: no shortlist needed");
        // Filled by BM25, then newest first.
        assert_eq!(ids(&f.order), ["L7", "L5", "L2", "L9", "L8", "L6", "L4", "L3", "L1", "L0"]);

        // Room for 4: shortlist L7, L2; then BM25 L5; then newest first (L9); shown in store order.
        let f = select_filled(all, || vec![&pool[7], &pool[2]], &ranked, 4 * each);
        assert!(f.fell);
        assert_eq!(ids(&f.shown), ["L2", "L5", "L7", "L9"]);
        assert_eq!(ids(&f.order), ["L7", "L2", "L5", "L9"], "the order they took the budget in");
        assert!(f.shown.iter().map(|c| cost(c)).sum::<usize>() <= 4 * each);
        // The fill order goes on past the budget: every other eligible lesson, once, as the fill met it.
        assert_eq!(ids(&f.dropped), ["L8", "L6", "L4", "L3", "L1", "L0"]);
    }

    #[test]
    fn the_budget_never_shows_a_lesson_outside_the_eligible_set() {
        let pool: Vec<Challenge> = (0..4).map(|i| lesson(&format!("L{i}"), "q", i)).collect();
        let elig = vec![&pool[0], &pool[1]];
        let f = select_filled(elig, || vec![&pool[3], &pool[2], &pool[1]], &[(1.0, &pool[3])], 1);
        assert!(f.shown.is_empty() && f.order.is_empty(), "budget too small for any line");
        assert_eq!(ids(&f.dropped), ["L1", "L0"], "dropped, but never one outside the eligible set");
        let f = select_filled(vec![&pool[0], &pool[1]], || vec![&pool[3], &pool[1]], &[], cost(&pool[1]));
        assert_eq!((ids(&f.shown), f.fell, ids(&f.order), ids(&f.dropped)), (vec!["L1".to_string()], true, vec!["L1".to_string()], vec!["L0".to_string()]));
    }

    /// The tail sample: `TAIL_SAMPLE` of the dropped lessons, uniform, the same for the same seed, each at
    /// chance k / n; all of them when fewer; none of an empty drop.
    #[test]
    fn the_tail_is_a_small_uniform_seeded_sample_of_what_the_budget_dropped() {
        let pool: Vec<Challenge> = (0..10).map(|i| lesson(&format!("D{i}"), "q", i)).collect();
        let dropped: Vec<&Challenge> = pool.iter().collect();
        let (t, pt) = tail(&dropped, 7, BOUNDARY_SHARE, 0);
        assert_eq!((t.len(), pt), (TAIL_SAMPLE, TAIL_SAMPLE as f64 / 10.0));
        assert!(t.windows(2).all(|w| w[0].0 < w[1].0), "in the order the fill met them: {t:?}");
        assert!(t.iter().all(|(i, c)| dropped[*i].id == c.id));
        let at = |t: &[(usize, &Challenge)]| t.iter().map(|(i, _)| *i).collect::<Vec<_>>();
        assert_eq!(at(&tail(&dropped, 7, BOUNDARY_SHARE, 0).0), at(&t), "seeded");
        // Uniform: over many seeds every place is drawn about k / n of the time.
        let mut hits = [0usize; 10];
        for seed in 0..10_000 {
            for (i, _) in tail(&dropped, seed, BOUNDARY_SHARE, 0).0 {
                hits[i] += 1;
            }
        }
        assert!(hits.iter().all(|h| (1_700..2_300).contains(h)), "{hits:?}");
        let (all, pt) = tail(&dropped[..1], 3, BOUNDARY_SHARE, 0);
        assert_eq!((all.len(), pt), (1, 1.0), "fewer than the sample: all of them, surely");
        let (none, pt) = tail(&[], 3, BOUNDARY_SHARE, 0);
        assert!(none.is_empty() && pt == 0.0);
    }

    /// The tail is a share of the budget: k = max(`TAIL_SAMPLE`, ⌊s·B / mean cost⌋) of the n dropped,
    /// at most n, whatever n is; uniform, at pt = k / n exactly.
    #[test]
    fn the_tail_is_uniform_over_the_remainder_and_pt_is_k_over_n() {
        let ten = [10usize; 40];
        // 0.15 · 400 = 60 tokens: six lessons of 10; a larger remainder draws no more.
        assert_eq!((tail_k(&ten, 0.15, 400), tail_k(&[10; 400], 0.15, 400), tail_k(&ten[..10], 0.15, 100)), (6, 6, 2));
        assert_eq!((tail_k(&ten[..1], 0.15, 400), tail_k(&ten, 0.0, 400), tail_k(&[], 0.15, 400), tail_k(&[0, 0], 0.15, 400)), (1, 2, 0, 2));
        let pool: Vec<Challenge> = (0..40).map(|i| lesson(&format!("D{i}"), "q", i)).collect();
        let dropped: Vec<&Challenge> = pool.iter().collect();
        // 0.15 · (40c + 1) = 6c + 0.15: six lessons of cost c.
        let tokens = 40 * cost(&pool[0]) + 1;
        let (t, pt) = tail(&dropped, 11, 0.15, tokens);
        assert_eq!((t.len(), pt), (6, 6.0 / 40.0));
        let mut hits = [0usize; 40];
        for seed in 0..20_000 {
            for (i, _) in tail(&dropped, seed, 0.15, tokens).0 {
                hits[i] += 1;
            }
        }
        // 20,000 × 6 / 40 = 3,000 per place.
        assert!(hits.iter().all(|h| (2_700..3_300).contains(h)), "{hits:?}");
    }

    /// Ten lessons of different sizes, at most four of which fit the budget.
    fn overflowing() -> Vec<Challenge> {
        (0..10).map(|i| lesson(&format!("B{i}"), &format!("question {i} {}", "w ".repeat(10 + 7 * (i % 3) as usize)), i)).collect()
    }

    /// `BORG_BOUNDARY=off` (s = 0) fills as before 0.7.7: the prefix is `select_filled` within the whole
    /// budget, nothing is drawn at the boundary, and the tail is `TAIL_SAMPLE` lessons.
    #[test]
    fn boundary_off_is_todays_fill() {
        let pool = overflowing();
        let all: Vec<&Challenge> = pool.iter().collect();
        let budget = cost(&pool[0]) * 4;
        for seed in 0..50 {
            let e = explore(all.clone(), Vec::new, &[], budget, 0.0, seed);
            let today = select_filled(all.clone(), Vec::new, &[], budget);
            assert_eq!((ids(&e.filled.order), ids(&e.filled.dropped)), (ids(&today.order), ids(&today.dropped)));
            assert!(e.boundary.is_empty() && e.tau == 0 && e.chances().0.iter().all(|b| *b == Some(0.0)), "{e:?}");
            assert_eq!((e.tail.len(), e.pt), (TAIL_SAMPLE, TAIL_SAMPLE as f64 / today.dropped.len() as f64));
            let (t, _) = tail(&today.dropped, seed, 0.0, 0);
            assert_eq!(
                e.tail.iter().map(|(i, c)| (*i, c.id.clone())).collect::<Vec<_>>(),
                t.iter().map(|(i, c)| (today.order.len() + i, c.id.clone())).collect::<Vec<_>>()
            );
        }
        // A pool that fits draws nothing at any share.
        let e = explore(all.clone(), Vec::new, &[], usize::MAX / 2, BOUNDARY_SHARE, 1);
        assert_eq!((e.filled.order.len(), e.drawn(), e.n), (10, 0, 0));
    }

    /// A prefix the share leaves empty (one lesson fills B, none fits ⌊(1 − s)·B⌋): no tail, the boundary
    /// alone shows one, and its chances say so (π of the two sums to 1, the first fill place favoured,
    /// e^0 : e^−1 at τ = 1), as `BORG_BOUNDARY=off` would show one too.
    #[test]
    fn an_empty_prefix_draws_the_boundary_without_a_tail() {
        let pool = [lesson("A", "same words here", 1), lesson("B", "same words also", 2)];
        let all: Vec<&Challenge> = pool.iter().collect();
        let budget = cost(&pool[0]);
        assert_eq!(cost(&pool[1]), budget);
        for seed in 0..20 {
            let e = explore(all.clone(), Vec::new, &[], budget, BOUNDARY_SHARE, seed);
            assert_eq!((e.filled.order.len(), e.tail.len(), e.pt, e.boundary.len()), (0, 0, 0.0, 1), "{e:?}");
            let (pb, pi) = e.chances();
            let first = 1.0 / (1.0 + (-1.0f64).exp());
            assert_eq!(pb.iter().map(|b| b.unwrap_or(-1.0)).collect::<Vec<_>>(), pi, "no tail: π is π_b");
            assert!((pi[0] - first).abs() < 0.05 && (pi[0] + pi[1] - 1.0).abs() < 1e-9, "{pi:?}");
        }
    }

    /// The boundary fills the budget's last share within it, never twice, never a tail lesson; π_b
    /// estimated per check matches how often the draw takes each place over many checks.
    #[test]
    fn boundary_pi_matches_frequency() {
        let pool = overflowing();
        let all: Vec<&Challenge> = pool.iter().collect();
        let budget = cost(&pool[0]) * 4;
        let first = explore(all.clone(), Vec::new, &[], budget, BOUNDARY_SHARE, 0);
        let n = first.n;
        assert!(n > 0 && first.tau > 0, "the scenario draws: {first:?}");
        let (mut shown, mut boundary, mut open) = (vec![0usize; n], vec![0usize; n], vec![0usize; n]);
        let (mut pb, mut pi) = (vec![0.0; n], vec![0.0; n]);
        let at = first.filled.order.len();
        const SEEDS: u64 = 10_000;
        const ESTIMATES: u64 = 100;
        for seed in 0..SEEDS {
            let e = explore(all.clone(), Vec::new, &[], budget, BOUNDARY_SHARE, seed);
            assert_eq!(ids(&e.filled.order), ids(&first.filled.order), "the prefix is deterministic");
            let used: usize = e.filled.order.iter().chain(e.boundary.iter().map(|(_, c)| c)).map(|c| cost(c)).sum();
            assert!(used <= budget, "within the budget");
            assert!(e.boundary.iter().all(|(b, _)| e.tail.iter().all(|(t, _)| t != b)), "never a tail lesson");
            let tail: Vec<usize> = e.tail.iter().map(|(i, _)| i - at).collect();
            for (i, o) in open.iter_mut().enumerate() {
                *o += usize::from(!tail.contains(&i));
            }
            for (i, _) in &e.boundary {
                boundary[i - at] += 1;
            }
            for i in tail.iter().copied().chain(e.boundary.iter().map(|(i, _)| i - at)) {
                shown[i] += 1;
            }
            if seed < ESTIMATES {
                let (epb, epi) = e.chances();
                for (i, (b, p)) in pb.iter_mut().zip(pi.iter_mut()).enumerate() {
                    *b += epb[i].unwrap_or(0.0) / ESTIMATES as f64;
                    *p += epi[i] / ESTIMATES as f64;
                }
            }
        }
        for i in 0..n {
            let (b, p) = (boundary[i] as f64 / open[i] as f64, shown[i] as f64 / SEEDS as f64);
            assert!((pb[i] - b).abs() <= 0.03, "π_b at {i}: {} vs {b}", pb[i]);
            assert!((pi[i] - p).abs() <= 0.03, "π at {i}: {} vs {p}", pi[i]);
        }
        assert!(boundary.windows(2).any(|w| w[0] > w[1]), "the draw favours the fill order's first: {boundary:?}");
    }
}
