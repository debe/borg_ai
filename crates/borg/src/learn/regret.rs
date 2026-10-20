//! A regret: an earlier session's decision whose lines a later session fixed (notes/regress.rs).
//! `borg learn` asks about it as about a correction, one model call, with its own opening and
//! sections: the lesson is a question to ask at the decision point ("Before <doing X> in
//! <situation>, has <risk Y> been considered?"). Reach, merging and superseding follow the
//! correction prompt's rules unchanged.
//!
//! Everything here was redacted before it was written (notes.rs `redact`).

use serde::{Deserialize, Serialize};

/// `Episode::kind` of a regret.
pub const KIND: &str = "regret";

/// Each excerpt in the prompt is cut to this many characters.
const CAP: usize = 1500;

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
}

/// How a regret prompt opens (the correction prompt's reach and merge rules follow it).
const HEAD: &str = "A coding agent made a decision in an earlier session (its task and reasoning below). {age} days later, \
in another session, the same lines were changed by a fix ({signals}). Decide whether the fix reveals a reusable lesson about \
the original decision: something the agent should have weighed at the moment it decided. A later change for an unrelated \
reason that merely touched the same lines (a rename, a new feature, a dependency bump, a style change) is NOT a lesson.";

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
        other => other,
    }
}

/// The opening, with the regret's age and signals.
#[must_use]
pub fn head(r: &Regret) -> String {
    let signals = if r.signals.is_empty() { "no stated reason".to_string() } else { r.signals.iter().map(|s| said(s)).collect::<Vec<_>>().join("; ") };
    HEAD.replace("{age}", &r.age_days.to_string()).replace("{signals}", &signals)
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
