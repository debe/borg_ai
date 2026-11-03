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

/// The judge's instructions (judge v3, `JUDGE_VERSION`): route-blind, one template for a lesson asked
/// at a stop or live, handed over free, or never shown (a placebo, hook.rs `#borg-placebo`): the judge
/// sees the same evidence for each and is never told how, or whether, the agent was given it. Five
/// distinct verdicts (`Verdict`), so "the point never came up" and "changed, but the consequence still
/// follows" are no longer folded into unclear, and a short reason per verdict.
const JUDGE: &str = "## Lessons from earlier in this session\n\
Each group below shows the agent's work at one point in this session (before) and its work since (after), then lessons \
that may bear on it. Judge by meaning, not by shared words, comparing the two, and give each lesson exactly one verdict: \
changed (the work changed on the lesson's point and no longer leads to the consequence); consequence_remains (the work \
changed on that point but still leads to the consequence); ignored (the point came up in the work and was kept as it \
was); not_arisen (nothing in the work since touches the lesson's point, so there was nothing to change); unclear (only \
when the evidence shown cannot tell these apart). A declaration (applies / skip) is the agent's claim, not proof: judge \
the work. Put one verdict per id in `resolved`, each with a reason: one short sentence (at most 25 words) naming what in \
the work decided it.";

/// The judge template's version, logged on every judge result (outcome.rs `judge`). Protocol: 0 is the
/// template before versions were logged (it told questions from free deliveries); 2 the route-blind
/// template with changed / unchanged / unclear and `avoids_consequence` (0.7.4 and before); 3 is
/// `JUDGE`, five verdicts with a reason (0.7.5 on).
pub const JUDGE_VERSION: u8 = 3;

/// Protocol: the first judge template version (`JUDGE_VERSION` 3, 0.7.5) whose `unclear` is pure: before
/// it, `asked_unclear` also held a change that left the consequence. The label health's verdict shares
/// (top.rs) count answers from it on, the 0.7.5 target's measure (docs/evidence.md, Lesson selection).
pub const PURE_UNCLEAR_FROM: u8 = 3;

/// The characters of a judge's reason kept per verdict (`reason_of`), after redaction, on one line.
/// Guard: bounds what one verdict adds to the outcome log and the judge's answer, whatever the model
/// writes; about twice the 25 words the prompt asks for.
pub const JUDGE_REASON_CHARS: usize = 240;

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
        s["properties"]["resolved"] = resolved_schema();
        s["required"] = json!(["moves", "applies", "resolved"]);
    }
    s
}

/// The judge's `resolved` list (judge v3): one verdict per id, each with its reason.
fn resolved_schema() -> Value {
    json!({"type": "array", "items": {"type": "object", "additionalProperties": false,
        "properties": {"id": {"type": "string"}, "verdict": {"type": "string", "enum": Verdict::WORDS},
            "reason": {"type": "string"}},
        "required": ["id", "verdict", "reason"]}})
}

/// The session-end judge's schema (`judge`): its `resolved` list alone, nothing to grade.
fn judge_schema() -> Value {
    json!({"type": "object", "additionalProperties": false, "properties": {"resolved": resolved_schema()}, "required": ["resolved"]})
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

/// The judge's verdict on one pending lesson (judge v3, `JUDGE`). Protocol: its word in the answer
/// (`word`, the schema's enum) and the outcome kind it records (`kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The work changed on the lesson's point and no longer leads to the consequence: a use.
    Changed,
    /// The point came up and the work kept it as it was (v2's `unchanged`).
    Ignored,
    /// Nothing in the work since touches the lesson's point: censored for beliefs, "nothing in the
    /// work" for selection.
    NotArisen,
    /// The work changed on the point but still leads to the consequence (v2: `changed` with
    /// `avoids_consequence` false): as unclear, pending.
    ConsequenceRemains,
    /// The evidence cannot tell: pending.
    Unclear,
}

impl Verdict {
    /// The words the schema allows, in `Verdict` order. Protocol: the judge's answer and the hook's
    /// `#borg-judged` state lines for a verdict that settles.
    pub const WORDS: [&'static str; 5] = ["changed", "ignored", "not_arisen", "consequence_remains", "unclear"];
    /// Every verdict, in `WORDS` order.
    pub const ALL: [Verdict; 5] = [Verdict::Changed, Verdict::Ignored, Verdict::NotArisen, Verdict::ConsequenceRemains, Verdict::Unclear];

    /// Its word in the answer (`WORDS`).
    #[must_use]
    pub fn word(self) -> &'static str {
        Self::WORDS[self as usize]
    }

    /// The verdict a word of the answer names: one of `WORDS`, or judge v2's `unchanged` (an ignore).
    /// None for any other word.
    #[must_use]
    pub fn of(word: &str) -> Option<Verdict> {
        match word {
            "unchanged" => Some(Verdict::Ignored),
            w => Self::WORDS.iter().position(|x| *x == w).map(|i| Self::ALL[i]),
        }
    }

    /// The outcome kind it records (outcome.rs): `asked_then_changed`, `asked_ignored`,
    /// `asked_not_arisen`, `asked_consequence_remains`, `asked_unclear`.
    #[must_use]
    pub fn kind(self) -> &'static str {
        match self {
            Verdict::Changed => crate::outcome::CHANGED,
            Verdict::Ignored => crate::outcome::IGNORED,
            Verdict::NotArisen => crate::outcome::NOT_ARISEN,
            Verdict::ConsequenceRemains => crate::outcome::REMAINS,
            Verdict::Unclear => crate::outcome::UNCLEAR,
        }
    }
}

/// The model's verdict on one pending lesson, with its reason (`reason_of`; empty when it gave none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    pub id: String,
    pub verdict: Verdict,
    pub reason: String,
}

impl Judged {
    /// The outcome it settles (outcome.rs `settles`): a change that avoids the consequence is a use, no
    /// change an ignore, a point that never came up `asked_not_arisen`; an unclear verdict or a change
    /// that still leads to the consequence stays pending (None).
    pub fn label(&self) -> Option<&'static str> {
        let kind = self.verdict.kind();
        crate::outcome::settles(kind).then_some(kind)
    }
}

/// The judge's reason as logged: redacted (notes.rs `redact`) before it is cut, on one line, at most
/// `JUDGE_REASON_CHARS` characters, marked when cut (`clip`).
#[must_use]
pub fn reason_of(text: &str) -> String {
    let one: String = crate::notes::redact(text).split_whitespace().collect::<Vec<_>>().join(" ");
    clip(&one, JUDGE_REASON_CHARS)
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
    /// Every id the answer's `resolved` names, in its order, each once, whatever its verdict and whether
    /// it was pending (`returned`): what the judge gave back, for the cost line (hook.rs
    /// `record_verdicts`), which tells a lesson the model left out from one it judged unclear.
    pub returned: Vec<String>,
    pub cost: f64,
    /// The characters of the prompt the call sent: a judge's prompt size, the effective length's
    /// measure (judge_length.rs; hook.rs logs it on a check's cost line, `prompt=`).
    pub prompt: usize,
    /// The design moves the model read the work as (the answer's `moves`, strings only, in its order),
    /// and per `applies` entry its `why` (empty where it gave none): logged on the check's `graded`
    /// record (outcome.rs `moves`, `whys`), never read back into a decision.
    pub moves: Vec<String>,
    pub whys: Vec<String>,
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

/// A check's model call reads the last this many characters of the work. Guard: bounds the work part of
/// a check's prompt in a long session. Unmeasured: a check's latency, which the stop's budget bounds, is
/// its reason, not a measured loss past it. The session-end judge reads by its own budget instead
/// (hook.rs `end_budget`).
pub const TAIL: usize = 24_000;

/// The instructions, with the grading sentence appended when graded.
fn instructions(base: &str, graded: bool) -> String {
    if graded { format!("{base} {GRADE}") } else { base.to_string() }
}

/// The prompt template a check's call used (`prompt_whole` when `compact`, else `prompt`), as 12 hex
/// digits of blake3 over its instructions and the judge's, and for a compact prompt whose lesson lines
/// are not `A1` their format (whole.rs `line_format`; `A1` hashes as before 0.7.7): logged with every
/// graded check (outcome.rs `prompt_hash`), so a change of wording or of the lesson line resets what was
/// learned of the grader and the judge (select/summary.rs `models` never mixes them).
pub fn template_hash(compact: bool, graded: bool) -> String {
    template_hash_with(compact, graded, crate::whole::line_format())
}

fn template_hash_with(compact: bool, graded: bool, line: crate::whole::Line) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"borg-prompt-v1\0");
    h.update(instructions(if compact { WHOLE } else { PROMPT }, graded).as_bytes());
    h.update(b"\0");
    h.update(JUDGE.as_bytes());
    if compact && line != crate::whole::Line::A1 {
        h.update(b"\0line=");
        h.update(line.name().as_bytes());
    }
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

/// The session-end judge's opening (`judge_prompt`): the session is over, its whole work comes first,
/// then the judge's own section (`JUDGE`) with each delivery's evidence, as at a check.
const END: &str = "You are the consequence model of an engineering team. A coding agent's session has ended. First \
comes the whole session's work (the engineer's prompts and the agent's touches, its edits and answers, its messages), \
then lessons delivered during the session, each with the work before and after its delivery. The session is over: no \
later work will come, so read the whole session's work for each lesson.";

/// The session-end judge's prompt (hook.rs `judge_session`): `END`, the session's work (already within
/// what its batch's budget leaves, hook.rs `within`), then the judge section of `pending`
/// (`judge_section`). Its characters are the work's plus `judge_prompt("", pending)`'s.
#[must_use]
pub fn judge_prompt(work: &str, pending: &[Pending]) -> String {
    format!("{END}\n\n## The session's work\n{work}\n{}", judge_section(pending))
}

/// One judge-only call at a session's end (hook.rs `judge_session`): `judge_prompt` with the resolved
/// list alone as its schema, parsed as a check's verdicts are (`resolved`, `returned`); no applies.
/// None on any failure or after `timeout`: the deliveries stay as they were.
pub fn judge(work: &str, pending: &[Pending], model: &str, timeout: Duration) -> Option<Prediction> {
    let p = judge_prompt(work, pending);
    let (v, cost) = crate::runner::structured(&crate::runner::choose(Some(model)), &p, &judge_schema(), timeout)?;
    v["resolved"].as_array()?;
    let prompt = p.chars().count();
    Some(Prediction {
        applies: Vec::new(),
        grades: Vec::new(),
        resolved: resolved(&v, pending),
        returned: returned(&v),
        cost: cost.unwrap_or(0.0),
        prompt,
        moves: Vec::new(),
        whys: Vec::new(),
    })
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
    let (applies, rest): (Vec<String>, Vec<(u8, String)>) = applies(&v, shortlist)?.into_iter().map(|(id, grade, why)| (id, (grade, why))).unzip();
    let (grades, whys) = rest.into_iter().unzip();
    Some(Prediction { applies, grades, resolved: resolved(&v, pending), returned: returned(&v), cost, prompt: p.chars().count(), moves: moves(&v), whys })
}

/// The answer's `moves`, its strings in order, each kept as a judge's reason is (`reason_of`: redacted,
/// on one line, bounded); none when missing.
fn moves(v: &Value) -> Vec<String> {
    v["moves"].as_array().into_iter().flatten().filter_map(Value::as_str).map(reason_of).filter(|m| !m.is_empty()).collect()
}

/// The ids the answer's `resolved` entries name (`Prediction::returned`), first seen first, each on one
/// line without commas (the cost line lists them comma-separated): every whitespace or comma in one
/// becomes `_`, so an id `resolved` would not match as it was given (" a" for pending "a") never reads as
/// a pending id on the cost line either. Entries without a string id, or an empty one, and a missing or
/// malformed `resolved`, name none.
fn returned(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in v["resolved"].as_array().into_iter().flatten().filter_map(|r| r["id"].as_str()) {
        let id: String = id.chars().map(|c| if c.is_whitespace() || c == ',' { '_' } else { c }).collect();
        if !id.is_empty() && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// The model's verdicts, kept only for pending ids with a valid verdict (`Verdict::of`), the first per
/// id, each with its reason (`reason_of`). A missing or malformed `resolved` yields none: every lesson
/// stays pending. Tolerant of judge v2's answer shape: `unchanged` is an ignore, and `changed` with
/// `avoids_consequence` false a remaining consequence (as v2 read it).
fn resolved(v: &Value, pending: &[Pending]) -> Vec<Judged> {
    let mut out: Vec<Judged> = Vec::new();
    for r in v["resolved"].as_array().into_iter().flatten() {
        let Some(id) = r["id"].as_str() else { continue };
        if !pending.iter().any(|p| p.lesson.id == id) || out.iter().any(|j| j.id == id) {
            continue;
        }
        let Some(mut verdict) = r["verdict"].as_str().and_then(Verdict::of) else { continue };
        if verdict == Verdict::Changed && r["avoids_consequence"].as_bool() == Some(false) {
            verdict = Verdict::ConsequenceRemains;
        }
        out.push(Judged { id: id.to_string(), verdict, reason: reason_of(r["reason"].as_str().unwrap_or_default()) });
    }
    out
}

/// The model's `applies` ids in its order with their grades and whys (`reason_of`; empty where it gave
/// none), kept only when on the shortlist and first seen: a partial or repeating answer still yields its
/// valid part, each why with its own entry. A missing grade counts as 2; an entry whose grade is not 1, 2
/// or 3 is dropped.
fn applies(v: &Value, shortlist: &[&Challenge]) -> Option<Vec<(String, u8, String)>> {
    let mut out: Vec<(String, u8, String)> = Vec::new();
    for a in v["applies"].as_array()? {
        let Some(id) = a["id"].as_str() else { continue };
        let grade = match &a["grade"] {
            Value::Null => 2,
            g => match g.as_u64().or_else(|| g.as_str().and_then(|s| s.trim().parse().ok())) {
                Some(n @ 1..=3) => n as u8,
                _ => continue,
            },
        };
        if shortlist.iter().any(|c| c.id == id) && !out.iter().any(|(i, _, _)| i == id) {
            out.push((id.to_string(), grade, reason_of(a["why"].as_str().unwrap_or_default())));
        }
    }
    Some(out)
}

/// How long the Stop step waits for the model before failing open (the drone just finishes):
/// `BORG_PREDICT_TIMEOUT_S`, default 45 s, under Claude Code's 60 s hook limit. At the stop it is capped
/// by the harness's budget (live.rs `call_limit`). The session lock goes stale after twice this (live.rs).
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
        assert_eq!(applies(&v, &[&a, &b]), Some(vec![("b".to_string(), 2, String::new()), ("a".to_string(), 2, String::new())]));
        assert_eq!(applies(&json!({}), &[&a]), None);
    }

    #[test]
    fn grades_are_parsed_tolerantly_missing_is_two_invalid_is_dropped() {
        let l: Vec<Challenge> = ["a", "b", "c", "d", "e", "f"].iter().map(|i| Challenge { id: (*i).into(), ..Default::default() }).collect();
        let refs: Vec<&Challenge> = l.iter().collect();
        let v = json!({"applies": [
            {"id": "a", "grade": 3}, {"id": "b"}, {"id": "c", "grade": "1"},
            {"id": "d", "grade": 0}, {"id": "e", "grade": "high"}, {"id": "f", "grade": 4}, {"id": "a", "grade": 1}]});
        let got: Vec<(String, u8)> = applies(&v, &refs).unwrap().into_iter().map(|(id, g, _)| (id, g)).collect();
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

    /// Each kept apply keeps its own why, whatever the shortlist, repeat and grade filters dropped
    /// around it; the moves are kept in order, one line each.
    #[test]
    fn whys_stay_aligned_after_filters() {
        let l: Vec<Challenge> = ["a", "b", "c"].iter().map(|i| Challenge { id: (*i).into(), ..Default::default() }).collect();
        let refs: Vec<&Challenge> = l.iter().collect();
        let v = json!({"moves": ["hold a lock\n across an await", 7, "", "retry without a bound"], "applies": [
            {"id": "zz", "why": "not on the shortlist", "grade": 3},
            {"id": "b", "why": "b's why", "grade": 9},
            {"id": "c", "why": "c's why", "grade": 3},
            {"id": "c", "why": "a repeat", "grade": 1},
            {"id": "a", "grade": 2},
            {"id": "b", "why": "b, valid now", "grade": 1}]});
        let got = applies(&v, &refs).unwrap();
        let want = [("c", 3, "c's why"), ("a", 2, ""), ("b", 1, "b, valid now")].map(|(i, g, w)| (i.to_string(), g, w.to_string()));
        assert_eq!(got, want);
        assert_eq!(moves(&v), ["hold a lock across an await", "retry without a bound"]);
        assert!(moves(&json!({"applies": []})).is_empty());
    }

    /// The session-end judge's call (hook.rs `judge_session`): its opening, the session's work, then the
    /// same judge section a check shows, route-blind; a schema of the verdicts alone, each with a reason.
    #[test]
    fn the_session_end_judge_reads_the_whole_work_then_the_judge_section() {
        let a = Challenge { id: "La".into(), question: "Q?".into(), consequence: "C".into(), ..Default::default() };
        let pend = [pending(&a)];
        let p = judge_prompt("### The agent's edits and answers\nedit one\n", &pend);
        assert!(p.starts_with(END), "{p}");
        let (work, judged) = (p.find("edit one").unwrap(), p.find(&judge_section(&pend)).unwrap());
        assert!(p.contains("## The session's work\n### The agent's edits") && work < judged, "{p}");
        for word in ["asked", "question", "free", "placebo", "context", "declared"] {
            assert!(!END.to_lowercase().contains(word), "{word}: {END}");
        }
        let s = judge_schema();
        assert_eq!(s["required"], json!(["resolved"]));
        assert_eq!(s["properties"]["resolved"], schema(true, true)["properties"]["resolved"], "the verdicts a check's judge gives");
        assert!(s["properties"].get("applies").is_none());
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
        let judged = |id: &str, verdict| Judged { id: id.into(), verdict, reason: String::new() };
        // v2's answer shape still reads: `changed` without `avoids_consequence` is a change.
        assert_eq!(r, vec![judged("a", Verdict::Changed), judged("c", Verdict::Changed)]);
        assert_eq!((r[0].label(), r[1].label()), (Some(crate::outcome::CHANGED), Some(crate::outcome::CHANGED)));
        assert!(resolved(&json!({"applies": []}), &p).is_empty(), "missing: all stay pending");
        assert_eq!(returned(&v), ["a", "zz", "b", "c"], "every id named, once, whatever its verdict");
        let odd = json!({"resolved": [{"id": "x y,z"}, {"id": 7}, {"id": ""}, {"verdict": "changed"}, {"id": "x\n"}]});
        assert_eq!(returned(&odd), ["x_y_z", "x_"], "one word per id, so the cost line stays parseable");
        // A padded id is no pending one: not resolved, and not returned as one either.
        let padded = json!({"resolved": [{"id": " a", "verdict": "changed", "avoids_consequence": true}]});
        assert!(resolved(&padded, &p).is_empty());
        assert_eq!(returned(&padded), ["_a"]);
        assert!(returned(&json!({"applies": []})).is_empty() && returned(&json!({"resolved": "a"})).is_empty());
        let j = |verdict| Judged { id: "x".into(), verdict, reason: String::new() }.label();
        assert_eq!((j(Verdict::Ignored), j(Verdict::Unclear)), (Some(crate::outcome::IGNORED), None));
    }

    /// Judge v3: five distinct verdicts, each with its reason; the three that settle are labels, the
    /// other two stay pending under their own kinds; v2's shape reads as v2 meant it.
    #[test]
    fn the_five_verdicts_parse_with_their_reason_and_only_three_settle() {
        use crate::outcome;
        let l: Vec<Challenge> = ["a", "b", "c", "d", "e", "f", "g"].iter().map(|i| Challenge { id: (*i).into(), ..Default::default() }).collect();
        let p: Vec<Pending> = l.iter().map(pending).collect();
        let v = json!({"resolved": [
            {"id": "a", "verdict": "changed", "reason": "moved the send off the lock"},
            {"id": "b", "verdict": "ignored", "reason": "still holds the lock across the await"},
            {"id": "c", "verdict": "not_arisen", "reason": "only CSS was touched"},
            {"id": "d", "verdict": "consequence_remains", "reason": "split the lock, still awaits under it"},
            {"id": "e", "verdict": "unclear"},
            {"id": "f", "verdict": "unchanged", "avoids_consequence": false},
            {"id": "g", "verdict": "changed", "avoids_consequence": false, "reason": "x"}]});
        let r = resolved(&v, &p);
        let got: Vec<(&str, Verdict, Option<&str>, &str)> = r.iter().map(|j| (j.id.as_str(), j.verdict, j.label(), j.reason.as_str())).collect();
        assert_eq!(
            got,
            [
                ("a", Verdict::Changed, Some(outcome::CHANGED), "moved the send off the lock"),
                ("b", Verdict::Ignored, Some(outcome::IGNORED), "still holds the lock across the await"),
                ("c", Verdict::NotArisen, Some(outcome::NOT_ARISEN), "only CSS was touched"),
                ("d", Verdict::ConsequenceRemains, None, "split the lock, still awaits under it"),
                ("e", Verdict::Unclear, None, ""),
                ("f", Verdict::Ignored, Some(outcome::IGNORED), ""),
                ("g", Verdict::ConsequenceRemains, None, "x"),
            ]
        );
        let kinds: Vec<&str> = Verdict::ALL.iter().map(|v| v.kind()).collect();
        assert_eq!(kinds, [outcome::CHANGED, outcome::IGNORED, outcome::NOT_ARISEN, outcome::REMAINS, outcome::UNCLEAR]);
        for v in Verdict::ALL {
            assert_eq!(Verdict::of(v.word()), Some(v), "{v:?}");
        }
        assert_eq!(Verdict::of("maybe"), None);
        // The schema asks for exactly these words, and a reason with each.
        let s = schema(true, false);
        assert_eq!(s["properties"]["resolved"]["items"]["properties"]["verdict"]["enum"], json!(Verdict::WORDS));
        assert_eq!(s["properties"]["resolved"]["items"]["required"], json!(["id", "verdict", "reason"]));
        // A reason is redacted before it is cut, on one line, bounded.
        let secret = format!("kept  the\nkey AKIAIOSFODNN7EXAMPLE in {}", "y".repeat(2 * JUDGE_REASON_CHARS));
        let r = reason_of(&secret);
        assert!(!r.contains("AKIAIOSFODNN7EXAMPLE") && !r.contains('\n') && r.starts_with("kept the key "), "{r}");
        assert_eq!(r.chars().count(), JUDGE_REASON_CHARS + 1, "cut and marked");
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
        assert_eq!(JUDGE_VERSION, 3);
        for word in Verdict::WORDS {
            assert!(question.contains(word), "{word}: {question}");
        }
        let hashes = [template_hash(true, true), template_hash(true, false), template_hash(false, true), template_hash(false, false)];
        assert!(hashes.iter().all(|h| h.len() == 12) && hashes.iter().collect::<std::collections::HashSet<_>>().len() == 4, "{hashes:?}");
        assert_eq!(template_hash(true, true), hashes[0], "stable");
        use crate::whole::Line;
        let (a1, a2) = (template_hash_with(true, true, Line::A1), template_hash_with(true, true, Line::A2));
        assert_ne!(a1, a2, "the lesson line format is part of the compact template");
        assert_eq!(template_hash_with(false, true, Line::A1), template_hash_with(false, true, Line::A2), "the shortlist's layout shows no line");
    }
}
