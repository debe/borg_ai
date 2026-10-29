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
/// size (select.rs) starts from and keeps until the yield by fill position has data.
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

/// One lesson on one line: `[id] question -> consequence`, each part trimmed.
pub fn line(c: &Challenge) -> String {
    let flat = |s: &str, n: usize| crate::predict::clip(&s.split_whitespace().collect::<Vec<_>>().join(" "), n);
    let q = flat(&c.question, QUESTION_CHARS);
    if c.consequence.trim().is_empty() { format!("[{}] {q}", c.id) } else { format!("[{}] {q} -> {}", c.id, flat(&c.consequence, CONSEQUENCE_CHARS)) }
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

/// Lessons the budget dropped that a check shows the model anyway, at most (`tail`). Guard: bounds the
/// grader's extra lessons per check, and so what the sample adds to its prompt past the stable lesson
/// block; one sample per check is enough to measure the yield of fill positions past the budget.
pub const TAIL_SAMPLE: usize = 2;

/// The tail sample of a pool the budget cut (`Filled::dropped`): `TAIL_SAMPLE` of `dropped` (all of them
/// when fewer) drawn uniformly without replacement from `seed`, each with its place in `dropped`, in
/// that order; and each one's chance of the draw, k / n for k drawn of n (unrounded, as the graded
/// record's `pt` logs it; the summary folds the sample unweighted, select/summary.rs module doc: the
/// tail). Nothing, at chance 0, when nothing was dropped.
#[must_use]
pub fn tail<'a>(dropped: &[&'a Challenge], seed: u64) -> (Vec<(usize, &'a Challenge)>, f64) {
    let (n, k) = (dropped.len(), TAIL_SAMPLE.min(dropped.len()));
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
        let (t, pt) = tail(&dropped, 7);
        assert_eq!((t.len(), pt), (TAIL_SAMPLE, TAIL_SAMPLE as f64 / 10.0));
        assert!(t.windows(2).all(|w| w[0].0 < w[1].0), "in the order the fill met them: {t:?}");
        assert!(t.iter().all(|(i, c)| dropped[*i].id == c.id));
        let at = |t: &[(usize, &Challenge)]| t.iter().map(|(i, _)| *i).collect::<Vec<_>>();
        assert_eq!(at(&tail(&dropped, 7).0), at(&t), "seeded");
        // Uniform: over many seeds every place is drawn about k / n of the time.
        let mut hits = [0usize; 10];
        for seed in 0..10_000 {
            for (i, _) in tail(&dropped, seed).0 {
                hits[i] += 1;
            }
        }
        assert!(hits.iter().all(|h| (1_700..2_300).contains(h)), "{hits:?}");
        let (all, pt) = tail(&dropped[..1], 3);
        assert_eq!((all.len(), pt), (1, 1.0), "fewer than the sample: all of them, surely");
        let (none, pt) = tail(&[], 3);
        assert!(none.is_empty() && pt == 0.0);
    }
}
