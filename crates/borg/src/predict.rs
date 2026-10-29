//! The consequence model: the conscience's world-model step.
//!
//! Lessons are `situations → consequence`. At a checkpoint one model call
//! encodes the drone's work into abstract design moves and predicts which
//! shortlisted lessons' consequences would follow from them. It may predict
//! none, and then nothing is asked. Word matching only shortlists; it never
//! decides. Evaluated offline in eval/preempt/situation_eval.py.
//!
//! The same call also judges, by meaning, what the drone did with lessons asked at an earlier Stop
//! of the session (R1, `Pending`): no extra call and no extra round; the section is added only when
//! such lessons are pending.

use serde_json::{Value, json};
use std::time::Duration;

use crate::conscience::Challenge;

const PROMPT: &str = "You are the consequence model of an engineering team. Below is a coding agent's current work (task, \
files touched, current plan) and lessons from earlier work: situations that once led to a bad consequence.

First list the 3-8 most consequential design moves the work makes (or deliberately omits), each as one abstract line: \
<action> on/in <kind of state or component> [under <condition>], without project or class names but keeping technical \
substance (threads, locks, I/O, ordering, ownership, reuse of existing mechanisms, spec contracts).

Then, for each lesson, predict whether its consequence would plausibly follow from THIS work: the work makes the same \
kind of move (or omits the same thing) on the same kind of state. Topic overlap alone is not enough. Return only lessons \
that apply, most consequential first; return none if none apply.";

/// The whole-store layout (whole.rs): the same task, with the lessons first. Lessons are shown as
/// `question -> consequence`; the instructions and the lesson block form a prefix that does not depend on
/// the work, so a provider can cache it across turn ends.
const WHOLE: &str = "You are the consequence model of an engineering team. First come the team's lessons from earlier \
work, one per line: [id] the question that once mattered -> the bad consequence that followed when it was missed. After \
them comes a coding agent's current work (task, files touched, current plan).

First list the 3-8 most consequential design moves the work makes (or deliberately omits), each as one abstract line: \
<action> on/in <kind of state or component> [under <condition>], without project or class names but keeping technical \
substance (threads, locks, I/O, ordering, ownership, reuse of existing mechanisms, spec contracts).

Then, for each lesson, predict whether its consequence would plausibly follow from THIS work: the work makes the same \
kind of move (or omits the same thing) on the same kind of state. Topic overlap alone is not enough. Return only lessons \
that apply, most consequential first; return none if none apply.";

/// Graded applies (the default; `BORG_STOP_GRADED=off` turns them off, internal evaluation "whole-store", "Graded follow-up", not published): appended to the
/// instructions; each applied lesson then carries a grade, and the Stop blocks only for a 3
/// (conscience.rs `graded`).
const GRADE: &str = "Grade each lesson you return: 3 = this work clearly runs into the consequence unless the lesson \
is followed; 2 = probably relevant; 1 = loosely related, worth a thought.";

/// The judge's instructions (judge v2, `JUDGE_VERSION`): route-blind, one template for a lesson asked
/// at a stop or live, handed over free, or never shown (a placebo, hook.rs `#borg-placebo`): the judge
/// sees the same evidence for each and is never told how, or whether, the agent was given it.
const JUDGE: &str = "## Lessons from earlier in this session\n\
Each group below shows the agent's work at one point in this session (before) and its work since (after), then lessons \
that may bear on it. Judge by meaning, not by shared words, comparing the two: did the agent change its work on the \
lesson's point (changed), keep it as it was (unchanged), or does the evidence not tell (unclear)? Set avoids_consequence \
true only if the work after no longer leads to the consequence. A declaration (applies / skip) is the agent's claim, not \
proof: judge the work. Put one verdict per id in `resolved`.";

/// The judge template's version, logged on every judge result (outcome.rs `judge`). Protocol: 0 is the
/// template before versions were logged (it told questions from free deliveries); 2 is `JUDGE`.
pub const JUDGE_VERSION: u8 = 2;

fn schema(judging: bool, graded: bool) -> Value {
    let mut s = json!({"type": "object", "additionalProperties": false, "properties": {
        "moves": {"type": "array", "items": {"type": "string"}},
        "applies": {"type": "array", "items": {"type": "object", "additionalProperties": false,
            "properties": {"id": {"type": "string"}, "why": {"type": "string"}}, "required": ["id", "why"]}}},
        "required": ["moves", "applies"]});
    if graded {
        s["properties"]["applies"]["items"]["properties"]["grade"] = json!({"type": "integer", "enum": [1, 2, 3]});
        s["properties"]["applies"]["items"]["required"] = json!(["id", "why", "grade"]);
    }
    if judging {
        s["properties"]["resolved"] = json!({"type": "array", "items": {"type": "object", "additionalProperties": false,
            "properties": {"id": {"type": "string"}, "verdict": {"type": "string", "enum": ["changed", "unchanged", "unclear"]},
                "avoids_consequence": {"type": "boolean"}},
            "required": ["id", "verdict", "avoids_consequence"]}});
        s["required"] = json!(["moves", "applies", "resolved"]);
    }
    s
}

/// A lesson delivered earlier in this session (asked, handed over free, or a placebo never shown) whose
/// effect is not judged yet (hook.rs `pending`): the drone's work before the delivery and after it (its
/// answer to a question, edits since, this turn's message), and its own `<id>: applies|skip` if it gave
/// one and the judge sees it.
#[derive(Debug, Clone)]
pub struct Pending<'a> {
    pub lesson: &'a Challenge,
    pub before: String,
    pub after: String,
    pub declared: Option<bool>,
    /// The check that routed this delivery (select.rs `check_id`; empty before checks had ids) and the
    /// channel its result is recorded under (`stop`, `live`, `early`, `queued`, outcome.rs `PLACEBO`).
    pub check: String,
    pub channel: &'static str,
    /// The drone's declaration was taken out of the evidence (hook.rs `stripped`): a random half of
    /// the questions, to measure what the declaration adds to the verdict.
    pub stripped: bool,
}

/// The evidence cap per delivery of pending lessons: `before` keeps this many characters, `after` three
/// times as many (hook.rs `pending` builds it from "work since", two parts, and this turn, one).
/// Lessons delivered together share their evidence in the prompt (`judge_section`). Guard: bounds the
/// judge section of a call, whatever the drone wrote.
pub const JUDGE_PART: usize = 1200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Changed,
    Unchanged,
    Unclear,
}

/// The model's verdict on one pending lesson.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    pub id: String,
    pub verdict: Verdict,
    pub avoids_consequence: bool,
}

impl Judged {
    /// The outcome it settles (outcome.rs): a change that avoids the consequence is a use, no change is
    /// an ignore; anything else (unclear, a change that still leads to the consequence) stays pending.
    pub fn label(&self) -> Option<&'static str> {
        match (self.verdict, self.avoids_consequence) {
            (Verdict::Changed, true) => Some(crate::outcome::CHANGED),
            (Verdict::Unchanged, _) => Some(crate::outcome::IGNORED),
            _ => None,
        }
    }
}

/// One checkpoint's answer: the lessons predicted to apply with their grades, the verdicts on pending
/// lessons, the cost.
#[derive(Debug, Clone, PartialEq)]
pub struct Prediction {
    pub applies: Vec<String>,
    /// One per `applies` entry: 3 certain, 2 likely, 1 worth a thought. An ungraded call, or a graded
    /// answer that leaves a grade out, counts as 2.
    pub grades: Vec<u8>,
    pub resolved: Vec<Judged>,
    pub cost: f64,
}

/// The first `n` characters of `s`, marked when cut.
pub fn clip(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().nth(n).is_some() {
        out.push('…');
    }
    out
}

/// The section added to the prompt for pending lessons; empty without any. Its length is the extra
/// prompt cost of judging (logged per call in the session's .cost file). Consecutive lessons with the
/// same evidence (delivered together) share one before/after. Route-blind (`JUDGE`): a lesson shows a
/// declaration only when the drone gave one and the judge may see it, so a question with its
/// declaration stripped reads like a free delivery or a placebo.
pub fn judge_section(pending: &[Pending]) -> String {
    if pending.is_empty() {
        return String::new();
    }
    let mut out = format!("\n{JUDGE}\n");
    for (i, p) in pending.iter().enumerate() {
        if i == 0 || (&pending[i - 1].before, &pending[i - 1].after) != (&p.before, &p.after) {
            out.push_str(&format!("\n### Work\nbefore: {}\nafter: {}\n", clip(&p.before, JUDGE_PART), clip(&p.after, 3 * JUDGE_PART)));
        }
        let declared = match p.declared {
            Some(true) => " (declared: applies)",
            Some(false) => " (declared: skip)",
            None => "",
        };
        out.push_str(&format!("[{}] lesson: {} -> consequence: {}{declared}\n", p.lesson.id, p.lesson.question, p.lesson.consequence));
    }
    out
}

/// The model reads the last this many characters of the work. Guard: bounds the work part of a call's
/// prompt in a long session.
pub const TAIL: usize = 24_000;

/// The instructions, with the grading sentence appended when graded.
fn instructions(base: &str, graded: bool) -> String {
    if graded { format!("{base} {GRADE}") } else { base.to_string() }
}

/// The prompt template a check's call used (`prompt_whole` when `compact`, else `prompt`), as 12 hex
/// digits of blake3 over its instructions and the judge's: logged with every graded check (outcome.rs
/// `prompt_hash`), so a change of wording resets what was learned of the grader and the judge.
pub fn template_hash(compact: bool, graded: bool) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"borg-prompt-v1\0");
    h.update(instructions(if compact { WHOLE } else { PROMPT }, graded).as_bytes());
    h.update(b"\0");
    h.update(JUDGE.as_bytes());
    h.finalize().to_hex()[..crate::select::CHECK_HEX].to_string()
}

/// The prompt for one checkpoint: the work (tail-truncated), the shortlist, and the pending lessons.
pub fn prompt(context: &str, shortlist: &[&Challenge], pending: &[Pending], graded: bool) -> String {
    let tail: String = {
        let chars: Vec<char> = context.chars().collect();
        chars[chars.len().saturating_sub(TAIL)..].iter().collect()
    };
    let lessons: Vec<String> =
        shortlist.iter().map(|c| format!("[{}] situations: {} -> consequence: {}", c.id, c.situations.join("; "), c.consequence)).collect();
    format!("{}\n\n## Current work\n{tail}\n\n## Lessons\n{}\n{}", instructions(PROMPT, graded), lessons.join("\n"), judge_section(pending))
}

/// Ids the model predicts apply, in its order, restricted to the shortlist, with their grades; its
/// verdicts on the pending lessons; and the call's cost (USD for Claude; total tokens for
/// `codex:<model>`). None on any failure or after `timeout`: the caller fails open, and pending lessons
/// stay pending.
pub fn predict(context: &str, shortlist: &[&Challenge], pending: &[Pending], model: &str, graded: bool, timeout: Duration) -> Option<Prediction> {
    ask(&prompt(context, shortlist, pending, graded), shortlist, pending, model, graded, timeout)
}

/// The whole-store prompt (whole.rs): instructions, then every shown lesson as one compact line, then
/// the work (tail-truncated), then the pending lessons.
pub fn prompt_whole(context: &str, pool: &[&Challenge], pending: &[Pending], graded: bool) -> String {
    let lines: Vec<String> = pool.iter().map(|c| crate::whole::line(c)).collect();
    format!(
        "{}\n\n## Lessons\n{}\n\n## Current work\n{}\n{}",
        instructions(WHOLE, graded),
        lines.join("\n"),
        crate::vectors::query_tail(context),
        judge_section(pending)
    )
}

/// `predict` over the whole-store pool (`prompt_whole`): same schema, same tolerant parsing.
pub fn predict_whole(context: &str, pool: &[&Challenge], pending: &[Pending], model: &str, graded: bool, timeout: Duration) -> Option<Prediction> {
    ask(&prompt_whole(context, pool, pending, graded), pool, pending, model, graded, timeout)
}

fn ask(p: &str, shortlist: &[&Challenge], pending: &[Pending], model: &str, graded: bool, timeout: Duration) -> Option<Prediction> {
    let judging = !pending.is_empty();
    let s = schema(judging, graded);
    let (v, cost) = crate::runner::structured(&crate::runner::choose(Some(model)), p, &s, timeout)?;
    let cost = cost.unwrap_or(0.0);
    let (applies, grades) = applies(&v, shortlist)?.into_iter().unzip();
    Some(Prediction { applies, grades, resolved: resolved(&v, pending), cost })
}

/// The model's verdicts, kept only for pending ids with a valid verdict, the first per id. A missing
/// or malformed `resolved` yields none: every lesson stays pending.
fn resolved(v: &Value, pending: &[Pending]) -> Vec<Judged> {
    let mut out: Vec<Judged> = Vec::new();
    for r in v["resolved"].as_array().into_iter().flatten() {
        let Some(id) = r["id"].as_str() else { continue };
        if !pending.iter().any(|p| p.lesson.id == id) || out.iter().any(|j| j.id == id) {
            continue;
        }
        let verdict = match r["verdict"].as_str() {
            Some("changed") => Verdict::Changed,
            Some("unchanged") => Verdict::Unchanged,
            Some("unclear") => Verdict::Unclear,
            _ => continue,
        };
        out.push(Judged { id: id.to_string(), verdict, avoids_consequence: r["avoids_consequence"].as_bool().unwrap_or(false) });
    }
    out
}

/// The model's `applies` ids in its order with their grades, kept only when on the shortlist and first
/// seen: a partial or repeating answer still yields its valid part. A missing grade counts as 2; an
/// entry whose grade is not 1, 2 or 3 is dropped.
fn applies(v: &Value, shortlist: &[&Challenge]) -> Option<Vec<(String, u8)>> {
    let mut out: Vec<(String, u8)> = Vec::new();
    for a in v["applies"].as_array()? {
        let Some(id) = a["id"].as_str() else { continue };
        let grade = match &a["grade"] {
            Value::Null => 2,
            g => match g.as_u64().or_else(|| g.as_str().and_then(|s| s.trim().parse().ok())) {
                Some(n @ 1..=3) => n as u8,
                _ => continue,
            },
        };
        if shortlist.iter().any(|c| c.id == id) && !out.iter().any(|(i, _)| i == id) {
            out.push((id.to_string(), grade));
        }
    }
    Some(out)
}

/// How long the Stop step waits for the model before failing open (the drone just finishes):
/// `BORG_PREDICT_TIMEOUT_S`, default 45 s, under Claude Code's 60 s hook limit. At the stop it is capped
/// by the harness's budget (live.rs `call_limit`). Live mode's lock goes stale after twice this (live.rs).
pub fn timeout() -> Duration {
    let secs = std::env::var("BORG_PREDICT_TIMEOUT_S").ok().and_then(|v| v.parse().ok()).unwrap_or(45u64);
    Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_keeps_known_ids_once_in_model_order() {
        let a = Challenge { id: "a".into(), ..Default::default() };
        let b = Challenge { id: "b".into(), ..Default::default() };
        let v = json!({"applies": [{"id": "b"}, {"id": "zz"}, {"id": "b"}, {"nope": 1}, {"id": "a"}]});
        assert_eq!(applies(&v, &[&a, &b]), Some(vec![("b".to_string(), 2), ("a".to_string(), 2)]));
        assert_eq!(applies(&json!({}), &[&a]), None);
    }

    #[test]
    fn grades_are_parsed_tolerantly_missing_is_two_invalid_is_dropped() {
        let l: Vec<Challenge> = ["a", "b", "c", "d", "e", "f"].iter().map(|i| Challenge { id: (*i).into(), ..Default::default() }).collect();
        let refs: Vec<&Challenge> = l.iter().collect();
        let v = json!({"applies": [
            {"id": "a", "grade": 3}, {"id": "b"}, {"id": "c", "grade": "1"},
            {"id": "d", "grade": 0}, {"id": "e", "grade": "high"}, {"id": "f", "grade": 4}, {"id": "a", "grade": 1}]});
        let got = applies(&v, &refs).unwrap();
        assert_eq!(got, vec![("a".to_string(), 3), ("b".to_string(), 2), ("c".to_string(), 1)]);
        let s = schema(false, true);
        assert_eq!(s["properties"]["applies"]["items"]["required"], json!(["id", "why", "grade"]));
        assert!(schema(true, false)["properties"]["applies"]["items"]["properties"].get("grade").is_none());
        // The grading sentence follows the instructions in both layouts; the rest is unchanged.
        let x = Challenge { id: "x".into(), question: "Q?".into(), consequence: "C".into(), ..Default::default() };
        let (g, u) = (prompt_whole("w", &[&x], &[], true), prompt_whole("w", &[&x], &[], false));
        assert!(g.starts_with(&format!("{WHOLE} {GRADE}\n\n## Lessons\n")) && u.starts_with(&format!("{WHOLE}\n\n## Lessons\n")), "{g}");
        assert_eq!(g.replacen(&format!(" {GRADE}"), "", 1), u);
        assert!(prompt("w", &[&x], &[], true).starts_with(&format!("{PROMPT} {GRADE}\n\n## Current work")));
    }

    fn pending(c: &Challenge) -> Pending<'_> {
        Pending { lesson: c, before: "b".into(), after: "a".into(), declared: None, check: String::new(), channel: "stop", stripped: false }
    }

    #[test]
    fn resolved_keeps_pending_ids_once_with_a_valid_verdict() {
        let a = Challenge { id: "a".into(), ..Default::default() };
        let b = Challenge { id: "b".into(), ..Default::default() };
        let c = Challenge { id: "c".into(), ..Default::default() };
        let p = [pending(&a), pending(&b), pending(&c)];
        let v = json!({"resolved": [
            {"id": "a", "verdict": "changed", "avoids_consequence": true},
            {"id": "a", "verdict": "unchanged", "avoids_consequence": false},
            {"id": "zz", "verdict": "changed", "avoids_consequence": true},
            {"id": "b", "verdict": "maybe", "avoids_consequence": true},
            {"id": "c", "verdict": "changed"}]});
        let r = resolved(&v, &p);
        assert_eq!(
            r,
            vec![
                Judged { id: "a".into(), verdict: Verdict::Changed, avoids_consequence: true },
                Judged { id: "c".into(), verdict: Verdict::Changed, avoids_consequence: false },
            ]
        );
        assert_eq!((r[0].label(), r[1].label()), (Some(crate::outcome::CHANGED), None));
        assert!(resolved(&json!({"applies": []}), &p).is_empty(), "missing: all stay pending");
        let j = |verdict, avoids| Judged { id: "x".into(), verdict, avoids_consequence: avoids }.label();
        assert_eq!((j(Verdict::Unchanged, false), j(Verdict::Unclear, true)), (Some(crate::outcome::IGNORED), None));
    }

    #[test]
    fn the_whole_store_prompt_puts_the_lessons_first_as_a_stable_prefix() {
        let a = Challenge { id: "La".into(), question: "Did I hold the lock?".into(), consequence: "deadlock".into(), ..Default::default() };
        let b = Challenge { id: "Lb".into(), question: "Did I pin the model?".into(), consequence: "drift".into(), ..Default::default() };
        let p1 = prompt_whole("task one: a websocket", &[&a, &b], &[], false);
        let p2 = prompt_whole("task two: something else entirely", &[&a, &b], &[], false);
        let head = format!("{WHOLE}\n\n## Lessons\n[La] Did I hold the lock? -> deadlock\n[Lb] Did I pin the model? -> drift\n\n## Current work\n");
        assert!(p1.starts_with(&head) && p2.starts_with(&head), "{p1}");
        assert!(p1[head.len()..].starts_with("task one"));
        assert!(prompt_whole(&"x".repeat(TAIL + 10), &[&a], &[], false).len() < WHOLE.len() + TAIL + 200);
        assert!(prompt_whole("w", &[&a], &[pending(&a)], false).ends_with(&judge_section(&[pending(&a)])));
    }

    #[test]
    fn the_judge_section_is_added_only_for_pending_lessons_and_capped() {
        let a = Challenge { id: "La".into(), question: "Q?".into(), consequence: "C".into(), ..Default::default() };
        assert_eq!(judge_section(&[]), "");
        assert!(!prompt("work", &[&a], &[], false).contains("asked earlier"));
        assert!(schema(false, false)["properties"].get("resolved").is_none());
        assert_eq!(schema(true, false)["required"], json!(["moves", "applies", "resolved"]));
        let long = Pending { before: "x".repeat(10_000), after: "y".repeat(10_000), declared: Some(false), ..pending(&a) };
        let s = judge_section(&[long]);
        assert!(s.contains("[La] lesson: Q? -> consequence: C (declared: skip)\n"), "{s}");
        assert!(s.len() < JUDGE.len() + 4 * JUDGE_PART + 200, "{}", s.len());
        assert!(prompt("work", &[&a], &[pending(&a)], false).ends_with(&judge_section(&[pending(&a)])));
        // Lessons asked at one block share their evidence; another block gets its own.
        let b = Challenge { id: "Lb".into(), ..Default::default() };
        let other = Pending { before: "other".into(), ..pending(&b) };
        let s = judge_section(&[pending(&a), pending(&b), other]);
        assert_eq!(s.matches("### Work").count(), 2, "{s}");
    }

    /// Judge v2 is route-blind: a question without a declaration (none given, or stripped), a free
    /// delivery and a placebo read the same; nothing in the section names how the lesson came.
    #[test]
    fn the_judge_section_reads_the_same_for_every_route() {
        let a = Challenge { id: "La".into(), question: "Q?".into(), consequence: "C".into(), ..Default::default() };
        let as_route = |channel: &'static str, stripped: bool| judge_section(&[Pending { channel, stripped, check: "c".into(), ..pending(&a) }]);
        let question = as_route("stop", true);
        for (channel, stripped) in [("live", false), ("queued", false), ("early", false), (crate::outcome::PLACEBO, false)] {
            assert_eq!(as_route(channel, stripped), question, "{channel}");
        }
        for word in ["asked", "question", "free", "placebo", "context", "declared"] {
            assert!(!question.to_lowercase().contains(word), "{word}: {question}");
        }
        assert_eq!(JUDGE_VERSION, 2);
        let hashes = [template_hash(true, true), template_hash(true, false), template_hash(false, true), template_hash(false, false)];
        assert!(hashes.iter().all(|h| h.len() == 12) && hashes.iter().collect::<std::collections::HashSet<_>>().len() == 4, "{hashes:?}");
        assert_eq!(template_hash(true, true), hashes[0], "stable");
    }
}
