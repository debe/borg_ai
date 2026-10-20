//! Regret episodes: a strong link (regress.rs) becomes an episode `borg learn` generalises with the
//! regret prompt (learn/regret.rs) into a question to ask at the decision point. Only with
//! `BORG_REGRESS=learn`, and only where the collective may sync: a local collective keeps its links
//! in the repo's notes and nothing more.
//!
//! The episode's id is `R<decision[..10]>-<fix[..10]>`, so a second end-of-session run finds it and
//! writes nothing. Its session is the fix's, so `borg learn --session S` at that session's end picks
//! it up; its engineer is the fix's author, the one whose work revealed the risk. Every text is
//! redacted before it is written; the diff whole, then cut, so a cut never shows half a secret.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

use crate::learn::{self, Episode, regret};
use crate::notes::links::{Link, Touched};
use crate::notes::regress_diff::Hunk;
use crate::notes::{Excerpt, redact};

/// Each excerpt of an episode is cut to this many characters.
const CAP: usize = 1500;

/// What the fix's session and commit add to a link.
#[derive(Debug, Clone, Default)]
pub struct FixContext {
    /// The fixing session's task.
    pub task: String,
    /// The fix's author (`%ae`).
    pub email: String,
    /// `diff_summary` of the fix.
    pub diff: String,
    /// The fixing session's own words: the engineer's prompts and the end of its last proposal.
    pub why: String,
}

/// The episode id of a link: deterministic, so it is also the dedupe key.
#[must_use]
pub fn id_of(l: &Link) -> String {
    let short = |s: &str| s.chars().take(10).collect::<String>();
    format!("R{}-{}", short(&l.commit), short(&l.fix))
}

fn cut(s: &str) -> String {
    redact(s).chars().take(CAP).collect()
}

fn tail(s: &str) -> String {
    let s = redact(s);
    let n = s.chars().count();
    s.chars().skip(n.saturating_sub(CAP)).collect()
}

fn overlaps(h: &Hunk, t: &Touched) -> bool {
    h.path == t.path && h.removed.iter().any(|(n, _)| t.lines.iter().any(|&(a, b)| (a..=b).contains(n)))
}

/// The hunks of the fix in the decision's files (`files`), those that changed its lines first (a
/// guard the fix added elsewhere in the file shows too), as `path`, `- old` and `+ new` lines:
/// redacted whole, then cut.
#[must_use]
pub fn diff_summary(hunks: &[Hunk], files: &[Touched]) -> String {
    let (on, beside): (Vec<&Hunk>, Vec<&Hunk>) =
        hunks.iter().filter(|h| files.iter().any(|t| t.path == h.path)).partition(|h| files.iter().any(|t| overlaps(h, t)));
    let mut s = String::new();
    for h in on.into_iter().chain(beside) {
        let _ = writeln!(s, "{}", h.path);
        for (_, l) in &h.removed {
            let _ = writeln!(s, "- {}", l.trim_end());
        }
        for l in &h.added {
            let _ = writeln!(s, "+ {}", l.trim_end());
        }
    }
    cut(&s)
}

/// The regret episode of strong link `l`, with the decision's reasoning (`decision`, if found).
#[must_use]
pub fn episode(l: &Link, decision: Option<&Excerpt>, fix: &FixContext) -> Episode {
    let r = regret::Regret {
        decision: l.commit.clone(),
        decision_session: l.decision.session.clone(),
        decision_task: cut(decision.map_or(l.decision.task.as_str(), |e| e.task.as_str())),
        decision_proposal: tail(decision.map_or("", |e| e.proposal.as_str())),
        fix: l.fix.clone(),
        fix_subject: cut(&l.fixed.subject),
        diff: cut(&fix.diff),
        fix_why: tail(&fix.why),
        signals: l.signals.clone(),
        age_days: l.age_days,
    };
    let correction = format!("Fixed later ({}d, {}): {}\n{}", r.age_days, r.signals.join(", "), r.fix_subject, r.diff);
    Episode {
        id: id_of(l),
        ms: learn::now_ms(),
        repo: l.repo.clone(),
        session: l.fixed.session.clone(),
        task: cut(&fix.task),
        agent: r.decision_proposal.clone(),
        rejected_tool: None,
        correction: correction.trim_end().to_string(),
        engineer: fix.email.trim().to_string(),
        kind: regret::KIND.into(),
        regret: Some(r),
    }
}

/// Whether `store` holds an episode with this id already.
#[must_use]
pub fn known(store: &Path, id: &str) -> bool {
    let text = std::fs::read_to_string(store.join("episodes.jsonl")).unwrap_or_default();
    text.lines().filter(|l| l.contains(id)).filter_map(|l| serde_json::from_str::<Value>(l).ok()).any(|v| v["id"] == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::links::Side;

    fn link() -> Link {
        Link {
            commit: "d".repeat(40),
            fix: "f".repeat(40),
            repo: "app".into(),
            strength: "fix".into(),
            signals: vec!["subject".into(), "tests".into()],
            decision: Side { session: "s-1".into(), task: "first line".into(), ..Side::default() },
            fixed: Side { session: "s-7".into(), subject: "fix: take the lock".into(), ..Side::default() },
            files: vec![Touched { path: "src/lib.rs".into(), lines: vec![(2, 2)] }],
            age_days: 3,
            ..Link::default()
        }
    }

    #[test]
    fn a_strong_link_becomes_a_regret_of_the_fix_session() {
        let decision = Excerpt {
            task: "read the flag in b\nwithout a lock".into(),
            proposal: "No lock needed. token=sk-abcdefghijklmnopqrstuvwx".into(),
            ..Excerpt::default()
        };
        let fix = FixContext {
            task: "make the flag safe".into(),
            email: "bob@example.com\n".into(),
            diff: "src/lib.rs\n- fn b()".into(),
            why: "the sync test is flaky\nTook the lock; password=hunter22".into(),
        };
        let e = episode(&link(), Some(&decision), &fix);
        assert_eq!((e.id.as_str(), e.session.as_str(), e.kind.as_str(), e.engineer.as_str()), ("Rdddddddddd-ffffffffff", "s-7", "regret", "bob@example.com"));
        assert_eq!(e.correction, "Fixed later (3d, subject, tests): fix: take the lock\nsrc/lib.rs\n- fn b()");
        let r = e.regret.unwrap();
        assert_eq!((r.decision_task.as_str(), r.age_days), ("read the flag in b\nwithout a lock", 3));
        assert!(r.decision_proposal.starts_with("No lock needed.") && !r.decision_proposal.contains("sk-abc"), "redacted: {}", r.decision_proposal);
        assert!(r.fix_why.starts_with("the sync test is flaky") && !r.fix_why.contains("hunter22"), "{}", r.fix_why);
        // Without the decision's reasoning the link's task line stands in.
        assert_eq!(episode(&link(), None, &fix).regret.unwrap().decision_task, "first line");
    }

    #[test]
    fn the_summary_keeps_the_decisions_files_its_lines_first() {
        let h = |path: &str, at: u32| Hunk { path: path.into(), removed: vec![(at, format!("old {at}"))], added: vec![format!("new {at}")], ..Hunk::default() };
        let files = [Touched { path: "src/lib.rs".into(), lines: vec![(2, 4)] }];
        let s = diff_summary(&[h("src/lib.rs", 9), h("src/lib.rs", 3), h("src/other.rs", 3)], &files);
        assert_eq!(s, "src/lib.rs\n- old 3\n+ new 3\nsrc/lib.rs\n- old 9\n+ new 9\n");
        let big = Hunk { path: "src/lib.rs".into(), removed: vec![(2, "x".repeat(3000))], ..Hunk::default() };
        assert_eq!(diff_summary(&[big], &files).chars().count(), CAP);
    }
}
