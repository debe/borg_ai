//! The whole-store Stop pool (the default; `BORG_STOP_POOL=shortlist` turns it off, reports/whole-store.md): instead of a shortlist, the
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

/// The default budget of the lesson block, in tokens (`BORG_STOP_POOL_TOKENS`).
pub const TOKENS: usize = 12_000;
/// A compact line keeps this many characters of the question...
pub const QUESTION_CHARS: usize = 240;
/// ...and this many of the consequence.
pub const CONSEQUENCE_CHARS: usize = 160;

/// The whole-store pool is the default (reports/whole-store.md, "Graded follow-up": with graded applies it
/// passed); `BORG_STOP_POOL=shortlist` keeps the shortlist instead.
pub fn on() -> bool {
    !std::env::var("BORG_STOP_POOL").is_ok_and(|v| v.trim().eq_ignore_ascii_case("shortlist"))
}

/// `BORG_STOP_FORMAT=classic` shows the whole-store pool in the shortlist's layout (`predict::prompt`:
/// work first, `situations -> consequence`) instead of the compact lessons-first layout
/// (`predict::prompt_whole`). Only the lesson set differs from the shortlist then (reports/whole-store.md,
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

/// A line's cost in tokens, estimated at 4 characters per token (and the newline).
pub fn cost(c: &Challenge) -> usize {
    (line(c).chars().count() + 1).div_ceil(4)
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

/// What the model is shown, in store order, and whether the budget forced a choice. All of `eligible`
/// when it fits in `tokens`; otherwise lessons in the order `shortlist` (called only then), `ranked`,
/// then the rest newest first, each taken while it still fits.
pub fn select<'a>(
    eligible: Vec<&'a Challenge>,
    shortlist: impl FnOnce() -> Vec<&'a Challenge>,
    ranked: &[(f64, &'a Challenge)],
    tokens: usize,
) -> (Vec<&'a Challenge>, bool) {
    if eligible.iter().map(|c| cost(c)).sum::<usize>() <= tokens {
        return (eligible, false);
    }
    let allowed: HashSet<&str> = eligible.iter().map(|c| c.id.as_str()).collect();
    let mut rest: Vec<&Challenge> = eligible.clone();
    rest.sort_by_key(|l| std::cmp::Reverse(l.origin_ms));
    let order = shortlist().into_iter().chain(ranked.iter().map(|(_, c)| *c)).chain(rest);
    let mut picked: HashSet<&str> = HashSet::new();
    let mut used = 0;
    for c in order {
        if !allowed.contains(c.id.as_str()) || picked.contains(c.id.as_str()) {
            continue;
        }
        let k = cost(c);
        if used + k <= tokens {
            used += k;
            picked.insert(c.id.as_str());
        }
    }
    (eligible.into_iter().filter(|c| picked.contains(c.id.as_str())).collect(), true)
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
        let (shown, fell) = select(
            all.clone(),
            || {
                called = true;
                vec![]
            },
            &[],
            10 * each,
        );
        assert_eq!((shown.len(), fell, called), (10, false, false), "fits: no shortlist needed");

        // Room for 4: shortlist L7, L2; then BM25 L5; then newest first (L9); shown in store order.
        let ranked = vec![(3.0, &pool[7]), (2.0, &pool[5]), (1.0, &pool[2])];
        let (shown, fell) = select(all, || vec![&pool[7], &pool[2]], &ranked, 4 * each);
        assert!(fell);
        assert_eq!(ids(&shown), ["L2", "L5", "L7", "L9"]);
        assert!(shown.iter().map(|c| cost(c)).sum::<usize>() <= 4 * each);
    }

    #[test]
    fn the_budget_never_shows_a_lesson_outside_the_eligible_set() {
        let pool: Vec<Challenge> = (0..4).map(|i| lesson(&format!("L{i}"), "q", i)).collect();
        let elig = vec![&pool[0], &pool[1]];
        let (shown, _) = select(elig, || vec![&pool[3], &pool[2], &pool[1]], &[(1.0, &pool[3])], 1);
        assert!(shown.is_empty(), "budget too small for any line");
        let (shown, fell) = select(vec![&pool[0], &pool[1]], || vec![&pool[3], &pool[1]], &[], cost(&pool[1]));
        assert_eq!((ids(&shown), fell), (vec!["L1".to_string()], true));
    }
}
