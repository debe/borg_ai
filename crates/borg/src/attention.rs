//! How many of a graded check's lessons to ask: a cut on the model's rank, set by conformal risk
//! control (CRC) on this collective's outcomes, not a fixed rule.
//!
//! The model's rank is the signal: the chance that a lesson is the one the work needed falls steadily
//! with its rank, while its grade says little (eval/review-fix/replay.py). So a blocking check asks the
//! lessons ranked below `k`, and `k` is the smallest cut whose miss rate, corrected for the sample, is
//! no worse than the fixed rule's own (`conscience::graded`):
//! - **labels**: only explored checks (outcome `graded` with `explore`): every `EXPLORE`th check under
//!   the cut asks every apply up to `RAIL`, so whether a lesson is seen to be needed does not depend on
//!   the cut in force. A lesson of it was needed when it was asked there and changed the work
//!   (`asked_then_changed`), or applied unasked and was corrected anyway (`applied_then_corrected`).
//!   Each such outcome labels one check, the session's last before it that asked (or left unasked) the
//!   lesson, so a long session does not repeat a label. The label is the ranks of those lessons;
//! - **α**: the share of labelled checks where the fixed rule would have asked none of the needed
//!   lessons, at least `MIN_ALPHA`;
//! - **the cut**: the smallest `k` with (n · miss(k) + 1)/(n + 1) ≤ α, where miss(k) is the share of
//!   labelled checks with no needed lesson ranked below `k`.
//!
//! Too few labels for α (n < ⌈1/α⌉ − 1, as in eval/review-fix/crc.py) keep the prior cut. Only the
//! newest `WINDOW` labels count, so the cut follows drift. Deterministic: the log line replays every
//! decision.

use std::collections::HashMap;

use crate::conscience::{self, Challenge};
use crate::outcome::{self, Outcome};

/// The prior cut: ranks 0 to 10. CRC by rank with α = the fixed rule's miss rate on the 169 logged
/// checks of the review-fix runs, calibrated 2026-10-22 (eval/review-fix/crc.py, α 0.059).
pub const PRIOR_K: usize = 11;
/// The newest labelled checks the cut is set on.
pub const WINDOW: usize = 200;
/// A guard against runaway rounds, never the tuning knob: no round asks more.
pub const RAIL: usize = 20;
/// The least α (crc.py's floor): with no fixed-rule miss yet, ⌈1/α⌉ − 1 = 999 labels before the cut moves.
pub const MIN_ALPHA: f64 = 1e-3;
/// One in `EXPLORE` blocking checks under the cut asks every apply up to `RAIL`: the only labels.
pub const EXPLORE: usize = 10;

/// One labelled check: its grades in the model's order and the ranks of the lessons it needed.
#[derive(Debug, Clone, PartialEq)]
pub struct Labelled {
    pub ms: i64,
    pub grades: Vec<u8>,
    pub needed: Vec<usize>,
}

/// The labelled checks in `outcomes`, oldest first, at most the newest `WINDOW` (module doc). Only
/// blocking explored checks (a grade 3) count. One pass over `outcomes`, then a search per outcome.
pub fn labels(outcomes: &[Outcome]) -> Vec<Labelled> {
    // (session, lesson) → the graded checks that graded it, oldest first: (ms, index, asked it).
    type Checks = Vec<(i64, usize, bool)>;
    let mut checks: HashMap<(&str, &str), Checks> = HashMap::new();
    for (i, o) in outcomes.iter().enumerate().filter(|(_, o)| o.kind == outcome::GRADED) {
        for l in &o.lessons {
            checks.entry((o.session.as_str(), l.as_str())).or_default().push((o.ms, i, o.chosen.contains(l)));
        }
    }
    for v in checks.values_mut() {
        v.sort_by_key(|c| c.0);
    }
    let mut needed: HashMap<usize, Vec<&str>> = HashMap::new();
    for o in outcomes.iter().filter(|o| o.kind == outcome::CHANGED || o.kind == outcome::MISSED) {
        for l in &o.lessons {
            let Some(cs) = checks.get(&(o.session.as_str(), l.as_str())) else { continue };
            let before = &cs[..cs.partition_point(|c| c.0 < o.ms)];
            if let Some(&(_, i, _)) = before.iter().rev().find(|c| c.2 == (o.kind == outcome::CHANGED)) {
                needed.entry(i).or_default().push(l);
            }
        }
    }
    let mut out: Vec<Labelled> = needed
        .into_iter()
        .filter_map(|(i, ls)| {
            let o = &outcomes[i];
            if !o.explore || o.grades.len() != o.lessons.len() || !o.grades.contains(&3) {
                return None;
            }
            let rank = |i: usize| if o.ranks.len() == o.lessons.len() { o.ranks[i] as usize } else { i };
            let needed = o.lessons.iter().enumerate().filter(|(_, l)| ls.contains(&l.as_str())).map(|(i, _)| rank(i)).collect();
            Some(Labelled { ms: o.ms, grades: o.grades.clone(), needed })
        })
        .collect();
    out.sort_by_key(|l| l.ms);
    let drop = out.len().saturating_sub(WINDOW);
    out.drain(..drop);
    out
}

/// The ranks the fixed rule asks for these grades (`conscience::graded`): with a 3, every 3 and 2, then
/// the 1s while fewer than `conscience::CAP`; without a 3, none.
pub fn incumbent(grades: &[u8]) -> Vec<usize> {
    if !grades.contains(&3) {
        return Vec::new();
    }
    let mut ask: Vec<usize> = (0..grades.len()).filter(|&i| grades[i] >= 2).collect();
    let room = conscience::CAP.saturating_sub(ask.len());
    ask.extend((0..grades.len()).filter(|&i| grades[i] == 1).take(room));
    ask
}

/// α: the share of `labels` where the fixed rule asks no needed lesson, at least `MIN_ALPHA`.
pub fn alpha(labels: &[Labelled]) -> f64 {
    let missed = labels.iter().filter(|l| !incumbent(&l.grades).iter().any(|r| l.needed.contains(r))).count();
    (missed as f64 / labels.len().max(1) as f64).max(MIN_ALPHA)
}

/// The cut and what set it: ask ranks below `k`; `n` labelled checks, `alpha`, and `source` "prior",
/// "crc" or "explore" (this check asks up to `RAIL` and is logged as a label candidate).
#[derive(Debug, Clone, PartialEq)]
pub struct Cut {
    pub k: usize,
    pub n: usize,
    pub alpha: f64,
    pub source: &'static str,
}

/// The cut for `labels` (module doc).
pub fn cut(labels: &[Labelled]) -> Cut {
    let (n, a) = (labels.len(), alpha(labels));
    // ⌈1/α⌉ − 1 labels at least, or no cut can meet α; the tolerance keeps 1/(1/m) from rounding up.
    if n + 1 < (1.0 / a - 1e-9).ceil() as usize {
        return Cut { k: PRIOR_K, n, alpha: a, source: "prior" };
    }
    let widest = labels.iter().map(|l| l.grades.len()).max().unwrap_or(0);
    let missed = |k: usize| labels.iter().filter(|l| !l.needed.iter().any(|r| *r < k)).count();
    // Integers on both sides would be exact; the tolerance keeps α = 1/(n + 1) passing.
    let k = (1..=widest).find(|&k| (missed(k) + 1) as f64 / (n + 1) as f64 <= a + 1e-12).unwrap_or(widest);
    Cut { k, n, alpha: a, source: "crc" }
}

/// `BORG_STOP_ATTENTION=on`: the learned cut. Off by default, so the fixed rule (`conscience::graded`)
/// stays until the live evaluation shows the cut is no worse and asks fewer (tasks/todo.md).
pub fn on() -> bool {
    std::env::var("BORG_STOP_ATTENTION").as_deref() == Ok("on")
}

/// What to ask, what to hand over free, and the cut (None under the fixed rule).
pub type Split<'a> = (Vec<&'a Challenge>, Vec<&'a Challenge>, Option<Cut>);

/// A graded check's split under `cut`: without a grade 3 nothing blocks, as in `conscience::graded`;
/// with a 3, the applies ranked below `cut.k` are asked (at most `RAIL`) and the rest go free (at most
/// `conscience::CAP`).
pub fn split<'a>(applied: &[(&'a Challenge, u8)], cut: &Cut) -> (Vec<&'a Challenge>, Vec<&'a Challenge>) {
    if !applied.iter().any(|(_, g)| *g == 3) {
        return conscience::graded_capped(applied, None);
    }
    let ask = cut.k.min(RAIL).min(applied.len());
    let mut free: Vec<&Challenge> = applied[ask..].iter().map(|(c, _)| *c).collect();
    free.truncate(conscience::CAP);
    (applied[..ask].iter().map(|(c, _)| *c).collect(), free)
}

/// A graded check's split: learned (`split`, cut from the outcomes in `home`) under
/// `BORG_STOP_ATTENTION=on`, else `conscience::graded`. `BORG_STOP_ASK_CAP` and checks without a 3 keep
/// the fixed rule. Every `EXPLORE`th graded check in the store explores (`explores`). Also returns the
/// cut, for the log.
pub fn graded<'a>(applied: &[(&'a Challenge, u8)], home: &std::path::Path) -> Split<'a> {
    if !on() || conscience::ask_cap().is_some() || !applied.iter().any(|(_, g)| *g == 3) {
        let (ask, free) = conscience::graded(applied);
        return (ask, free, None);
    }
    let outcomes = outcome::read(&home.join("outcomes.jsonl"));
    let c = cut(&labels(&outcomes));
    let c = match explores(outcomes.iter().filter(|o| o.kind == outcome::GRADED).count()) {
        true => Cut { k: RAIL, source: "explore", ..c },
        false => c,
    };
    let (ask, free) = split(applied, &c);
    (ask, free, Some(c))
}

/// Does the check after `graded` graded checks explore? Every `EXPLORE`th, never the first.
pub fn explores(graded: usize) -> bool {
    (graded + 1).is_multiple_of(EXPLORE)
}

/// `#borg-attention k=<k> n=<n> alpha=<α> source=<prior|crc|explore> applied=<m>`: one check's cut; with the
/// check's `#borg-graded` line it replays which lessons were asked.
pub fn line(c: &Cut, applied: usize) -> String {
    format!("#borg-attention k={} n={} alpha={:.4} source={} applied={applied}", c.k, c.n, c.alpha, c.source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(kind: &str, session: &str, lessons: &[&str], ms: i64) -> Outcome {
        Outcome { kind: kind.into(), session: session.into(), lessons: lessons.iter().map(|l| l.to_string()).collect(), ms, ..Default::default() }
    }

    /// An explored check: it asked every lesson but `unasked`.
    fn graded_rec(session: &str, lessons: &[&str], grades: &[u8], ms: i64) -> Outcome {
        explored(session, lessons, grades, &[], ms)
    }

    fn explored(session: &str, lessons: &[&str], grades: &[u8], unasked: &[&str], ms: i64) -> Outcome {
        Outcome {
            grades: grades.to_vec(),
            ranks: (0..lessons.len() as u16).collect(),
            chosen: lessons.iter().filter(|l| !unasked.contains(l)).map(|l| l.to_string()).collect(),
            explore: true,
            ..rec(outcome::GRADED, session, lessons, ms)
        }
    }

    fn lab(grades: &[u8], needed: &[usize]) -> Labelled {
        Labelled { ms: 0, grades: grades.to_vec(), needed: needed.to_vec() }
    }

    #[test]
    fn labels_are_needed_lessons_after_the_check_in_its_session() {
        let log = vec![
            rec(outcome::CHANGED, "s", &["C"], 5), // before the check: not a label for it
            explored("s", &["A", "B", "C", "D"], &[3, 2, 1, 1], &["D"], 10),
            rec(outcome::CHANGED, "s", &["B"], 20),
            rec(outcome::MISSED, "s", &["D"], 30),
            rec(outcome::IGNORED, "s", &["A"], 30),
            rec(outcome::CHANGED, "t", &["A"], 30), // another session
            graded_rec("u", &["A"], &[3], 10),      // nothing needed: unlabelled
            graded_rec("v", &["A"], &[2], 10),      // no 3: the cut decides nothing
            rec(outcome::CHANGED, "v", &["A"], 20),
        ];
        assert_eq!(labels(&log), vec![Labelled { ms: 10, grades: vec![3, 2, 1, 1], needed: vec![1, 3] }]);
    }

    #[test]
    fn only_explored_checks_label_the_cut() {
        // Under a cut of 1 the check asked only A; B, ranked above the cut, could never show it was
        // needed, so the check would read as a hit for k = 1. Not explored: no label.
        let cut = |explore: bool| Outcome { explore, ..explored("s", &["A", "B"], &[3, 3], &["B"], 10) };
        let log = |explore: bool| vec![cut(explore), rec(outcome::CHANGED, "s", &["A"], 20)];
        assert_eq!(labels(&log(false)), vec![]);
        assert_eq!(labels(&log(true)), vec![Labelled { ms: 10, grades: vec![3, 3], needed: vec![0] }]);
    }

    #[test]
    fn an_outcome_labels_only_the_last_check_that_asked_the_lesson() {
        let log = vec![
            graded_rec("s", &["A"], &[3], 10),
            graded_rec("s", &["A"], &[3], 20),
            explored("s", &["A"], &[3], &["A"], 25), // graded A without asking it: not the asking check
            rec(outcome::CHANGED, "s", &["A"], 35),
            graded_rec("s", &["A"], &[3], 40), // after the outcome
        ];
        assert_eq!(labels(&log), vec![Labelled { ms: 20, grades: vec![3], needed: vec![0] }]);
    }

    #[test]
    fn labels_are_linear_in_the_store() {
        // 100k records, a quarter blocking checks of ten lessons, the rest outcomes on them: the scan
        // per lesson and record this replaced took minutes here.
        let ls: Vec<String> = (0..10).map(|i| format!("L{i}")).collect();
        let l: Vec<&str> = ls.iter().map(String::as_str).collect();
        let log: Vec<Outcome> = (0..25_000i64)
            .flat_map(|i| {
                let s = format!("s{}", i % 500);
                let mut v = vec![graded_rec(&s, &l, &[3; 10], 4 * i)];
                v.extend((1..4).map(|j| rec(outcome::CHANGED, &s, &[l[(i as usize + j) % 10]], 4 * i + j as i64)));
                v
            })
            .collect();
        let t = std::time::Instant::now();
        assert_eq!(labels(&log).len(), WINDOW);
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "{:?}", t.elapsed());
    }

    #[test]
    fn labels_keep_the_newest_window() {
        let log: Vec<Outcome> = (0..WINDOW as i64 + 5)
            .flat_map(|i| [graded_rec(&format!("s{i}"), &["A"], &[3], 2 * i), rec(outcome::CHANGED, &format!("s{i}"), &["A"], 2 * i + 1)])
            .collect();
        let l = labels(&log);
        assert_eq!(l.len(), WINDOW);
        assert_eq!(l[0].ms, 10, "the five oldest dropped");
    }

    #[test]
    fn incumbent_replays_the_fixed_rule() {
        assert_eq!(incumbent(&[2, 1, 2]), Vec::<usize>::new());
        assert_eq!(incumbent(&[1, 3, 2, 1]), [1, 2, 0, 3]);
        let many: Vec<u8> = std::iter::once(3).chain(std::iter::repeat_n(1, 12)).collect();
        assert_eq!(incumbent(&many), (0..conscience::CAP).collect::<Vec<_>>());
        // Needed at rank 11 is beyond the 1s the rule has room for; needed at 0 is asked.
        let l = [lab(&many, &[11]), lab(&many, &[0]), lab(&many, &[0]), lab(&many, &[3])];
        assert_eq!(alpha(&l), 0.25);
        assert_eq!(alpha(&l[1..]), MIN_ALPHA, "never below MIN_ALPHA");
    }

    #[test]
    fn too_few_labels_keep_the_prior() {
        assert_eq!(cut(&[]), Cut { k: PRIOR_K, n: 0, alpha: MIN_ALPHA, source: "prior" });
        // The fixed rule missed nothing: α = MIN_ALPHA wants 999 labels, so one label moves nothing.
        assert_eq!(cut(&[lab(&[3, 3, 3], &[0])]), Cut { k: PRIOR_K, n: 1, alpha: MIN_ALPHA, source: "prior" });
        // Once the rule misses, α is its miss rate: one miss in three wants two labels (⌈3⌉ − 1).
        let g: Vec<u8> = std::iter::once(1).chain(std::iter::repeat_n(3, 10)).collect(); // the rule skips rank 0
        let l = [lab(&g, &[0]), lab(&[3, 3], &[1]), lab(&[3, 3], &[0])];
        assert_eq!(cut(&l[1..]).source, "prior", "no miss in two");
        assert_eq!((cut(&l).k, cut(&l).source), (2, "crc"), "{:?}", cut(&l));
    }

    #[test]
    fn the_cut_is_the_smallest_k_that_meets_alpha() {
        // 40 checks: 10 whose fixed rule skips the needed rank 0 (a 1 before ten 3s), and 30 of a 3 then
        // 1s needing rank 0, 1 or 2, ten each. α = 10/40. k = 3: no miss, 1/41 ≤ α; k = 2: 10 misses,
        // 11/41 > α.
        let skip: Vec<u8> = std::iter::once(1).chain(std::iter::repeat_n(3, 10)).collect();
        let g: Vec<u8> = std::iter::once(3).chain(std::iter::repeat_n(1, 11)).collect();
        let mut l: Vec<Labelled> = (0..10).map(|_| lab(&skip, &[0])).collect();
        l.extend((0..30).map(|i| lab(&g, &[i % 3])));
        assert_eq!(cut(&l), Cut { k: 3, n: 40, alpha: 0.25, source: "crc" });
        // 15 of 40 need rank 11 instead, which the rule never asks: α = 15/40. Every k ≤ 11 misses at
        // least those 15 (16/41 > α), so only the widest cut, 12, passes.
        let mut l: Vec<Labelled> = (0..25).map(|i| lab(&g, &[i % 4])).collect();
        l.extend((0..15).map(|_| lab(&g, &[11])));
        assert_eq!(cut(&l), Cut { k: 12, n: 40, alpha: 0.375, source: "crc" });
    }

    #[test]
    fn every_tenth_check_explores() {
        assert_eq!((0..30).filter(|&g| explores(g)).collect::<Vec<_>>(), [9, 19, 29]);
    }

    #[test]
    fn no_three_is_the_fixed_rule_and_the_rail_holds() {
        let ls: Vec<Challenge> = (0..30).map(|i| Challenge { id: format!("L{i}"), ..Default::default() }).collect();
        let ids = |v: &[&Challenge]| v.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        let c = Cut { k: 2, n: 50, alpha: 0.1, source: "crc" };
        let soft: Vec<(&Challenge, u8)> = ls.iter().take(12).map(|c| (c, 2)).collect();
        let (a, f) = split(&soft, &c);
        let (fa, ff) = conscience::graded_capped(&soft, None);
        assert_eq!((ids(&a), ids(&f)), (ids(&fa), ids(&ff)));
        let mixed: Vec<(&Challenge, u8)> = ls.iter().take(5).enumerate().map(|(i, c)| (c, if i == 3 { 3 } else { 1 })).collect();
        let (a, f) = split(&mixed, &c);
        assert_eq!((ids(&a), ids(&f)), (vec!["L0".to_string(), "L1".into()], vec!["L2".to_string(), "L3".into(), "L4".into()]));
        let all: Vec<(&Challenge, u8)> = ls.iter().map(|c| (c, 3)).collect();
        let (a, f) = split(&all, &Cut { k: 30, ..c.clone() });
        assert_eq!((a.len(), f.len()), (RAIL, conscience::CAP.min(30 - RAIL)));
    }

    #[test]
    fn the_line_names_the_cut() {
        let c = Cut { k: 4, n: 40, alpha: 1.0 / 41.0, source: "crc" };
        assert_eq!(line(&c, 12), "#borg-attention k=4 n=40 alpha=0.0244 source=crc applied=12");
    }
}
