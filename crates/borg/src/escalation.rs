//! The queen's attention reward: did
//! an escalation spend the engineer's minutes well?
//!
//! ```text
//! R(queen) = no_after_escalation − α·rubber_stamp − β·missed_escalation
//! ```
//!
//! Three stat lines (stats.rs), each logged once, where its event happens:
//! - `queen_no` (+1): an escalation the engineer answered by changing course: holding the move,
//!   making it reversible, keeping the old contract, leaving the other side to its owners, or own words
//!   (queen.rs `answer`).
//! - `queen_stamp` (−α): an escalation answered "yes, fine": the option that lets the plan go ahead
//!   (`Kind::proceeds`), or a bare yes.
//! - `queen_missed` (−β): a "no" that came after the work merged, from a session with no escalation:
//!   a veto (veto.rs `record`, any of its three ways in) of a session whose work landed (a commit of its
//!   own still on HEAD, notes.rs `landed_by`), or a strong regress link (notes/regress_learn.rs: a later
//!   fix of a committed decision) of a deciding session.
//!
//! Escalations are the questions the queen chooses to raise: the four checkpoint kinds
//! (`Kind::escalates`). The other queue kinds are proposals or gates an engineer must answer whatever
//! the queen thinks (constraints, playbooks, reach, open decisions), so they earn nothing.
//! "No escalation" is judged against this node's queue: the queue is local, and so are the sessions it
//! names.
//!
//! The lines roll up into `stats.jsonl` like every count and sync with it, so `of` folds the team's
//! yield from the buckets; borg-web's queen view shows it. α = 1 (a wasted answer costs what a useful
//! one earns) and β = memory.rs `VETOED_W` (a missed escalation weighs what the veto it missed weighs
//! in a lesson's belief); both design-chosen, neither tuned. Nothing reads the reward but monitoring:
//! the design keeps the queen's predictor a router, and optimising a proxy of the engineer is the
//! Goodhart trap it warns about. Preparation: no value metric yet; it is what the value metric of a
//! learned escalation policy will be read from.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::queen::{self, Entry};
use crate::{notes, stats};

/// α: what a rubber stamp costs.
pub const STAMP_W: f64 = 1.0;
/// β: what a missed escalation costs.
pub const MISSED_W: f64 = crate::memory::VETOED_W as f64;

/// Words an answer that only says "yes, fine" is made of.
const YES: [&str; 11] = ["yes", "y", "ok", "okay", "fine", "sure", "go", "ahead", "proceed", "approved", "lgtm"];

fn norm(text: &str) -> String {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Whether `answer` to escalation `e` is "yes, fine": it is (or starts with) the option that lets the
/// plan go ahead, or every word of it is a plain yes.
pub fn stamp(e: &Entry, answer: &str) -> bool {
    let a = norm(answer);
    let plan = norm(e.kind.proceeds());
    (!plan.is_empty() && a.starts_with(&plan)) || yes(answer)
}

/// Whether `answer` says only yes: every word of it is a plain yes.
pub fn yes(answer: &str) -> bool {
    let a = norm(answer);
    !a.is_empty() && a.split(' ').all(|w| YES.contains(&w))
}

/// Logs the reward of an answer to `e` (queen.rs `answer`, after it was recorded): `queen_no` or
/// `queen_stamp` for an escalation, nothing for any other kind. Returns the kind logged.
pub fn answered(store: &Path, e: &Entry, answer: &str, author: &str, now: i64) -> Option<&'static str> {
    if !e.kind.escalates() {
        return None;
    }
    let kind = if stamp(e, answer) { stats::QUEEN_STAMP } else { stats::QUEEN_NO };
    stats::count(store, kind, &e.session, |ev| {
        ev.ms = now;
        ev.engineer = author.into();
        ev.repo = e.repo.clone();
        ev.why = format!("{:?}", e.kind);
        ev.text = format!("{} {}", e.id, stats::excerpt(answer, 120));
    });
    Some(kind)
}

/// Whether the queen escalated anything from `session` (answered, open or dismissed alike: it was
/// raised).
pub fn escalated(store: &Path, session: &str) -> bool {
    !session.is_empty() && queen::entries(store).iter().any(|e| e.session == session && e.kind.escalates())
}

/// Whether `session`'s work merged: it landed a commit of its own that is still on HEAD (notes.rs
/// `landed_by`, from the hook's state in `state`).
pub fn merged(state: &Path, session: &str) -> bool {
    !notes::landed_by(state, session).is_empty()
}

/// Logs a missed escalation once per `id` (a veto episode, a regress link): `cause` `veto` or
/// `regret`, of `session` in `repo`. Nothing when the queen escalated from that session, or for an
/// unknown session. Returns whether it logged.
pub fn missed(store: &Path, session: &str, cause: &str, id: &str, repo: &str, now: i64) -> bool {
    if session.is_empty() || escalated(store, session) {
        return false;
    }
    if stats::events(store).iter().any(|e| e.kind == stats::QUEEN_MISSED && e.text.split(' ').next() == Some(id)) {
        return false;
    }
    stats::count(store, stats::QUEEN_MISSED, session, |ev| {
        ev.ms = now;
        ev.repo = repo.into();
        ev.why = cause.into();
        ev.text = id.into();
    });
    true
}

/// The queen's attention reward over some counts (a bucket's, or the team's summed).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Yield {
    /// Escalations answered: `no` + `stamps`.
    pub answered: u64,
    pub no: u64,
    pub stamps: u64,
    pub missed: u64,
    /// The share of answered escalations that ended in a "no"; None before any answer.
    pub share: Option<f64>,
    /// R(queen) = no − α·stamps − β·missed.
    pub reward: f64,
    pub alpha: f64,
    pub beta: f64,
}

/// The yield of `counts` (stats.rs `Bucket::counts`).
pub fn of(counts: &BTreeMap<String, u64>) -> Yield {
    let n = |k: &str| counts.get(k).copied().unwrap_or(0);
    let (no, stamps, missed) = (n(stats::QUEEN_NO), n(stats::QUEEN_STAMP), n(stats::QUEEN_MISSED));
    let answered = no + stamps;
    Yield {
        answered,
        no,
        stamps,
        missed,
        share: (answered > 0).then(|| no as f64 / answered as f64),
        reward: no as f64 - STAMP_W * stamps as f64 - MISSED_W * missed as f64,
        alpha: STAMP_W,
        beta: MISSED_W,
    }
}

/// The team's yield: every bucket of the store (this node's events and the synced rollups), summed.
pub fn team(home: &Path) -> Yield {
    let mut all = stats::Bucket::default();
    for b in stats::current(home) {
        all.add(&b);
    }
    of(&all.counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queen::Kind;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("borg-escalation-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn entry(id: &str, kind: Kind, session: &str) -> Entry {
        let (a, b) = kind.options();
        Entry {
            id: id.into(),
            decision_key: format!("k{id}"),
            kind,
            question: format!("q {id}"),
            options: vec![a.into(), b.into()],
            default: a.into(),
            evidence: vec![],
            session: session.into(),
            cwd: String::new(),
            repo: "app".into(),
            engineer: "ann@x".into(),
            ms: 1,
            answer: None,
            answered_by: None,
            answered_ms: None,
            disagreeing: vec![],
            dismissed: None,
            dismissed_ms: None,
            payload: None,
        }
    }

    #[test]
    fn letting_the_plan_go_ahead_is_a_stamp_and_changing_course_is_a_no() {
        let irreversible = entry("q1", Kind::Irreversible, "s");
        assert!(stamp(&irreversible, "proceed as planned"));
        assert!(stamp(&irreversible, "Yes, fine."));
        assert!(stamp(&irreversible, "ok go ahead"));
        assert!(!stamp(&irreversible, "make it reversible first (backup, soft delete, a revert path)"));
        assert!(!stamp(&irreversible, "yes but keep a backup"), "a yes with a condition changes the course");
        assert!(!stamp(&irreversible, "no"));
        // An external mutation's default holds it: the engineer letting it run is the yes.
        let push = entry("q2", Kind::ExternalMutation, "s");
        assert!(stamp(&push, "run it as planned"));
        assert!(!stamp(&push, &push.default), "holding it is the no");
    }

    #[test]
    fn the_yield_folds_counts_with_the_design_weights() {
        let counts: BTreeMap<String, u64> =
            [(stats::QUEEN_NO, 3), (stats::QUEEN_STAMP, 1), (stats::QUEEN_MISSED, 1)].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let y = of(&counts);
        assert_eq!((y.answered, y.no, y.stamps, y.missed), (4, 3, 1, 1));
        assert_eq!(y.share, Some(0.75));
        assert_eq!(y.reward, 3.0 - 1.0 - 3.0, "a missed escalation weighs a veto");
        assert_eq!(of(&BTreeMap::new()), Yield { alpha: 1.0, beta: 3.0, ..Yield::default() }, "no answer, no share");
    }

    #[test]
    fn only_the_queens_own_escalations_earn_and_each_answer_logs_once() {
        let store = tmp("answered");
        let mut e = entry("q1", Kind::PublicApi, "s1");
        assert_eq!(answered(&store, &e, "keep the old contract working", "ann@x", 10), Some(stats::QUEEN_NO));
        assert_eq!(answered(&store, &e, "proceed as planned", "ann@x", 11), Some(stats::QUEEN_STAMP));
        for k in [Kind::Constraint, Kind::Playbook, Kind::Reach, Kind::OpenDecision] {
            e.kind = k;
            assert_eq!(answered(&store, &e, "anything", "ann@x", 12), None, "{k:?} is a gate, not the queen's choice");
        }
        let y = team(&store);
        assert_eq!((y.no, y.stamps), (1, 1));
        let _ = std::fs::remove_dir_all(&store);
    }

    #[test]
    fn a_no_without_an_escalation_is_missed_once_and_never_after_one() {
        let store = tmp("missed");
        assert!(missed(&store, "s1", "veto", "no-s1-5", "app", 5));
        assert!(!missed(&store, "s1", "veto", "no-s1-5", "app", 6), "once per id");
        assert!(!missed(&store, "", "regret", "r1", "app", 6), "an unknown session is nobody's miss");
        // The queen escalated from s2: a no there is the escalation working, not a miss.
        let line = serde_json::to_string(&entry("q9", Kind::CrossRepo, "s2")).unwrap();
        std::fs::create_dir_all(queen::dir(&store)).unwrap();
        std::fs::write(queen::dir(&store).join("queue.jsonl"), format!("{line}\n")).unwrap();
        assert!(escalated(&store, "s2"));
        assert!(!missed(&store, "s2", "regret", "r2", "app", 7));
        // A gate question from s3 is no escalation.
        let line = serde_json::to_string(&entry("q10", Kind::Playbook, "s3")).unwrap();
        std::fs::write(queen::dir(&store).join("queue.jsonl"), format!("{line}\n")).unwrap();
        assert!(missed(&store, "s3", "regret", "r3", "app", 8));
        let y = team(&store);
        assert_eq!((y.missed, y.reward), (2, -6.0));
        let _ = std::fs::remove_dir_all(&store);
    }
}
