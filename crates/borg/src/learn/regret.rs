//! A regret: an earlier session's decision whose lines a later session fixed (notes/regress.rs).
//! `borg learn` asks about it as about a correction, one model call, with its own opening and
//! sections: the lesson is a question to ask at the decision point ("Before <doing X> in
//! <situation>, has <risk Y> been considered?"). Reach, merging and superseding follow the
//! correction prompt's rules unchanged.
//!
//! Everything here was redacted before it was written (notes.rs `redact`).

use serde::{Deserialize, Serialize};

use crate::notes::regress_signals::{REVERT, SUBJECT, TESTS, WORDS};

/// `Episode::kind` of a regret.
pub const KIND: &str = "regret";

/// The first signal of a regret from a review finding (review_fix.rs): found and fixed inside the
/// session, before any commit. `decision_task` is then the built task, `decision_proposal` the
/// builder's account, `fix_subject` the finding and `diff` its detail.
pub const REVIEW: &str = "review";

/// Each excerpt in the prompt is cut to this many characters. Derived: an episode's excerpt bound
/// (learn.rs `EXCERPT_CHARS`), so a regret's prompt keeps as much of each part as a correction's.
pub const CAP: usize = super::EXCERPT_CHARS;

/// The decision and the fix that changed its lines.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Regret {
    /// The decision's commit.
    pub decision: String,
    pub decision_session: String,
    pub decision_task: String,
    /// The end of the decision's last proposal.
    pub decision_proposal: String,
    /// The fix's commit.
    pub fix: String,
    pub fix_subject: String,
    /// What the fix changed in the decision's files (`-`/`+` lines per file).
    pub diff: String,
    /// The fixing session's own words (prompts, the end of its last proposal).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fix_why: String,
    pub signals: Vec<String>,
    pub age_days: u32,
    /// When the decision was made, epoch ms: the decision commit's committer time, or the review
    /// finding's time. 0 when unknown (episodes from before it was kept): such a regret credits no lesson.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub decision_ms: i64,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// How strongly a regret blames the decision, in (0, 1): `1 − 2^−g` over its independent grounds g: a
/// revert, a fix-shaped subject, a red-then-green test run together with the engineer's words of a bug
/// (regress_signals.rs `strong`); for a review regret the finding itself, and a verifier's confirmation.
/// One ground 0.5, two 0.75, three 0.875; 0 for a regret that is not strong (never learned from anyway).
/// A parameter-free count of evidence, not a tuning knob: the offline report breaks the credit down by g.
#[must_use]
pub fn strength(r: &Regret) -> f64 {
    let has = |s: &str| r.signals.iter().any(|x| x == s);
    let g = match is_review(r) {
        true => 1 + u32::from(has("confirmed")),
        false => u32::from(has(REVERT)) + u32::from(has(SUBJECT)) + u32::from(has(TESTS) && has(WORDS)),
    };
    match g {
        0 => 0.0,
        g => 1.0 - 0.5f64.powi(g as i32),
    }
}

/// How a regret prompt opens (the correction prompt's reach and merge rules follow it).
const HEAD: &str = "A coding agent made a decision in an earlier session (its task and reasoning below). {age} days later, \
in another session, the same lines were changed by a fix ({signals}). Decide whether the fix reveals a reusable lesson about \
the original decision: something the agent should have weighed at the moment it decided. A later change for an unrelated \
reason that merely touched the same lines (a rename, a new feature, a dependency bump, a style change) is NOT a lesson.";

/// How a review regret opens.
const REVIEW_HEAD: &str = "A coding agent built a change (its task and its own account below). Before anything was committed, a \
reviewer in the same session found the problem below ({signals}), and a fix step fixed it. Decide whether the finding reveals a \
reusable lesson about the build: something the agent should have weighed while building. A typo, a style nit, or a one-off slip \
with no risk that transfers to other work is NOT a lesson.";

/// The fields a regret lesson is written in (the area line follows, shared with corrections).
pub const FIELDS: &str = "If it is a lesson, write:
- question: ONE question to ask at the decision point, in the form \"Before <choosing/doing X> in <kind of situation>, has \
<the risk Y that the fix revealed> been considered?\"; general enough to transfer, keeping the technical substance;
- situations: 1-3 lines, the decision's situations, each `<action> on/in <kind of state or component> [under <condition>]`, \
without project/class names;
- consequence: one line, what went wrong later, as the fix shows;";

/// What a signal (`notes/regress_signals.rs`) says, for the model.
fn said(signal: &str) -> &str {
    match signal {
        "revert" => "the fix reverts the decision",
        "subject" => "the fix's commit subject says it fixes something",
        "tests" => "a test failed, then passed in the fixing session",
        "words" => "the engineer spoke of a bug in that session",
        REVIEW => "a reviewer found it before any commit",
        "confirmed" => "a verifier confirmed it",
        "unchecked" => "no verifier checked it",
        other => other,
    }
}

/// The opening, with the regret's age and signals.
#[must_use]
pub fn head(r: &Regret) -> String {
    let signals = if r.signals.is_empty() { "no stated reason".to_string() } else { r.signals.iter().map(|s| said(s)).collect::<Vec<_>>().join("; ") };
    let head = if is_review(r) { REVIEW_HEAD } else { HEAD };
    head.replace("{age}", &r.age_days.to_string()).replace("{signals}", &signals)
}

/// A regret from a review finding, not from a later commit.
#[must_use]
pub fn is_review(r: &Regret) -> bool {
    r.signals.first().is_some_and(|s| s == REVIEW)
}

/// The correction prompt's reach and merge rules (learn.rs), worded for a regret.
#[must_use]
pub fn neutral(rules: &str) -> String {
    rules.replace(", e.g. the engineer reversed an earlier correction", "").replace("this correction", "this lesson")
}

fn cut(s: &str) -> String {
    s.chars().take(CAP).collect()
}

fn tail(s: &str) -> String {
    let n = s.chars().count();
    s.chars().skip(n.saturating_sub(CAP)).collect()
}

/// The decision, its reasoning, the fix (`fix_task`: the fixing session's task), why it was made
/// and its change.
#[must_use]
pub fn sections(r: &Regret, fix_task: &str) -> String {
    if is_review(r) {
        return format!(
            "## The task the agent built\n{}\n\n## The agent's account of its build (excerpt)\n{}\n\n## The review finding (fixed in the same session, before any commit)\n{}\n{}\n\n",
            cut(&r.decision_task),
            tail(&r.decision_proposal),
            r.fix_subject,
            cut(&r.diff)
        );
    }
    let why = if r.fix_why.is_empty() { String::new() } else { format!("## Why it was fixed (the fixing session's words, excerpt)\n{}\n\n", tail(&r.fix_why)) };
    format!(
        "## The decision (task)\n{}\n\n## The decision's reasoning (excerpt)\n{}\n\n## The fix (subject, task)\n{}\n{}\n\n{why}## What the fix changed in the decision's files (excerpt)\n{}\n\n",
        cut(&r.decision_task),
        tail(&r.decision_proposal),
        r.fix_subject,
        cut(fix_task),
        cut(&r.diff)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strength_counts_independent_grounds() {
        let of = |signals: &[&str]| strength(&Regret { signals: signals.iter().map(|s| s.to_string()).collect(), ..Regret::default() });
        assert_eq!(of(&["subject"]), 0.5);
        assert_eq!(of(&["revert", "subject"]), 0.75);
        assert_eq!(of(&["revert", "subject", "tests", "words"]), 0.875);
        assert_eq!(of(&["tests"]), 0.0, "red-then-green alone is how a feature is built");
        assert_eq!(of(&["tests", "words"]), 0.5);
        assert_eq!(of(&[REVIEW, "unchecked"]), 0.5);
        assert_eq!(of(&[REVIEW, "confirmed"]), 0.75);
        assert_eq!(of(&[]), 0.0);
    }

    #[test]
    fn a_review_regret_has_its_own_opening_and_sections() {
        let r = Regret {
            decision_task: "port the voice lane".into(),
            decision_proposal: "Built it; all tests pass.".into(),
            fix_subject: "A failed recommendation reaches the model as status ok".into(),
            diff: "src/Rec.java: the error branch returns ok()".into(),
            signals: vec![REVIEW.into(), "confirmed".into()],
            ..Regret::default()
        };
        assert!(head(&r).starts_with("A coding agent built a change") && !head(&r).contains("days later"), "{}", head(&r));
        assert!(head(&r).contains("(a reviewer found it before any commit; a verifier confirmed it)"), "{}", head(&r));
        let s = sections(&r, "ignored");
        for part in [
            "## The task the agent built\nport the voice lane",
            "Built it; all tests pass.",
            "before any commit)\nA failed recommendation",
            "error branch returns ok()",
        ] {
            assert!(s.contains(part), "{part}: {s}");
        }
        assert!(!s.contains("ignored") && !is_review(&Regret::default()));
    }

    #[test]
    fn the_prompt_names_age_signals_and_both_sides() {
        let r = Regret {
            decision_task: "read the flag in b".into(),
            decision_proposal: "No lock needed.".into(),
            fix_subject: "fix: take the lock".into(),
            diff: "src/lib.rs\n- fn b() { read_the_flag(); }".into(),
            signals: vec!["subject".into(), "tests".into()],
            age_days: 3,
            ..Regret::default()
        };
        assert!(head(&r).starts_with("A coding agent made a decision") && head(&r).contains("3 days later"));
        assert!(head(&r).contains("(the fix's commit subject says it fixes something; a test failed, then passed in the fixing session)"), "{}", head(&r));
        assert!(head(&Regret::default()).contains("(no stated reason)"));
        let s = sections(&r, "make the flag safe");
        for part in ["## The decision (task)\nread the flag in b", "No lock needed.", "fix: take the lock\nmake the flag safe", "- fn b()"] {
            assert!(s.contains(part), "{part}: {s}");
        }
        assert!(!s.contains("## Why it was fixed"));
        let why = sections(&Regret { fix_why: "the sync test is flaky".into(), ..r.clone() }, "");
        assert!(why.contains("## Why it was fixed (the fixing session's words, excerpt)\nthe sync test is flaky\n"), "{why}");
        assert_eq!(
            neutral("get this correction too? the opposite, e.g. the engineer reversed an earlier correction), x"),
            "get this lesson too? the opposite), x"
        );
        let long = Regret { decision_proposal: format!("{}END", "x".repeat(2000)), ..Regret::default() };
        assert!(sections(&long, "").contains("xEND\n") && !sections(&long, "").contains(&"x".repeat(1501)), "the proposal's end is kept");
    }
}
