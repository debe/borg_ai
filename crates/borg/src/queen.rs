//! The queen: the rare, budgeted escalation to a human engineer (PLAN.md "Expertise and
//! the queen"; reports/expertise.md §2, L4).
//!
//! At the Stop checkpoint, after the conscience decided, the drone's plan goes to the queen only if
//! all four hold:
//! 1. **High consequence**: a plan sentence makes an irreversible change, an external mutation, a
//!    public API/schema change or a cross-repo/cross-team decision (`classify`, deterministic, 0 tokens).
//! 2. **The collective cannot resolve it**: either the conscience asked nothing and the collective's
//!    lessons carrying this decision key are absent or disagree, or the lessons the conscience selected
//!    contradict each other on this decision (same situations, different consequences: `conflicts`).
//! 3. **The inputs do not answer it**: the engineer's prompts in this session do not already state it.
//! 4. **Budget**: at most one queen message (≤ `MAX_QUESTIONS` questions) per engineer per
//!    `BUDGET_SESSIONS` sessions, counted in the collective's own store.
//!
//! An asked question goes to `<store>/queen/queue.jsonl`; the drone is told once which default it
//! proceeds with and is never held up. A decision (its key, or a reworded near-duplicate of an asked
//! question or a queen lesson) is asked at most once, ever. `borg queen answer`
//! turns the engineer's answer into a lesson of the collective, so later drones get it from the
//! conscience (L1) instead of the queen.
//!
//! Off by default: the queen test (reports/queen-test.md, rounds 1–3) failed its pre-registered bar,
//! so the Stop hook asks the queen only with `BORG_QUEEN=on`. `borg queen` (list, answer) works either way.
//!
//! Answering the queue: `borg queen` numbers the open questions (asking order) for the terminal, `--json`
//! and `next --json` give drones a stable schema (`Question`), `dismiss` closes a question without a
//! lesson, `skip` holds it back for `SKIP_MS`, and a near-duplicate of a queued question of the same repo
//! is never queued (`duplicate`; `dedupe` cleans old queues). The prompt hook delivers at most one
//! question per session, only of the session's own repos (`deliver`, `render_delivery`;
//! `BORG_QUEEN_ASK=off` disables it), and newly queued questions get one desktop notification (notify.rs).
//!
//! Fails open: any error means the drone proceeds untouched.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::conscience::Challenge;
use crate::{collective, conscience, learn};

/// At most one queen message per engineer per this many sessions.
pub const BUDGET_SESSIONS: usize = 5;
/// At most this many questions in one message (one message per checkpoint).
pub const MAX_QUESTIONS: usize = 2;
/// Of a decision's significant words, this share already in the engineer's prompts means the inputs
/// state the decision (criterion 3).
const STATED: f64 = 0.6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    ExternalMutation,
    Irreversible,
    PublicApi,
    CrossRepo,
    /// A decision the team's own documents leave open ("Your decision", "Open conflict"), seeded into the
    /// queue by `borg assimilate --from-dir` (assimilate_dir.rs). Never raised by a checkpoint and not a
    /// message to anyone, so it does not use up an engineer's budget.
    OpenDecision,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::ExternalMutation => "an external mutation",
            Kind::Irreversible => "an irreversible change",
            Kind::PublicApi => "a public API or schema change",
            Kind::CrossRepo => "a cross-repo or cross-team decision",
            Kind::OpenDecision => "a decision the team's documents leave open",
        }
    }

    /// (the drone's default, the alternative). External mutations stay approval-gated: their default
    /// is to hold, everything else proceeds as planned.
    fn options(self) -> (&'static str, &'static str) {
        match self {
            Kind::ExternalMutation => ("hold: prepare it, but do not run it until the engineer approves", "run it as planned"),
            Kind::Irreversible => ("proceed as planned", "make it reversible first (backup, soft delete, a revert path)"),
            Kind::PublicApi => ("proceed as planned", "keep the old contract working (additive change, versioning)"),
            Kind::CrossRepo => ("proceed as planned", "leave the other repo's or team's side to its owners"),
            Kind::OpenDecision => ("keep the documented interim rule", "decide it now"),
        }
    }
}

/// Trigger phrases per kind. Each is a move, anchored on a present/future verb or on a phrase that
/// characterises the change; bare noun mentions ("the prod database", "the public API") and past
/// tenses ("deployed", "bumped": at Stop usually a summary of work already done) do not trigger.
static KINDS: LazyLock<Vec<(Kind, Regex)>> = LazyLock::new(|| {
    let r = |s: &str| Regex::new(s).unwrap();
    vec![
        (
            Kind::ExternalMutation,
            r(
                r"\b(deploy(ing)? (to|the)|(cargo|npm|pnpm|yarn|twine|gem) publish|publish(ing)? (the |a )?(package|crate|release|artifact|image)|push(ing)? (it |this )?to (origin|main|master|the remote|upstream|production)|merg(e|ing) (the |this )?(pr|pull request)|terraform apply|kubectl (apply|delete)|helm (install|upgrade)|send(ing)? (an |the )?(email|e-mail|notification)|post(ing)? to slack|(creat|clos|transition)(e|ing) (a |the )?(jira|ticket)|(writ(e|ing)( to)?|migrat(e|ing)|updat(e|ing)|modify(ing)?|delet(e|ing) from|insert(ing)? into|alter(ing)?) (the )?prod(uction)? (database|db)|tag(ging)? (a |the )?release|cut(ting)? (a |the )?release)\b",
            ),
        ),
        // A push to a shared branch: the refspec's target is `main`, `master` or `prod(uction)` exactly
        // ("HEAD:main" is, "main-fix" and "feature/main" are not).
        (Kind::ExternalMutation, r(r"\bgit push ([-\w./=@]+ ){0,3}?((origin|upstream) )?([\w./-]+:)?(main|master|prod(uction)?)([^-/\w]|$)")),
        (
            Kind::Irreversible,
            r(
                r"\b(drop(ping)? (the )?(\w+ )?(table|column|database|schema|index)|truncat(e|ing) (the )?(table|\w+ table)|rm -rf|force[- ]push(ing)?|push(ing)? --force|reset --hard|hard reset|(delete|deleting) (the )?(\w+ )?(branch|bucket|database|table|history|backups?)|permanently delet(e|ing)|purg(e|ing) (the |all )?(\w+ )?(data|records|rows|bucket|cache|queue|history|backups?|users?|tables?|database)|irreversibl[ey]|cannot be undone|wip(e|ing) (the )?\w+|rewrit(e|ing) (the )?(git )?history)\b",
            ),
        ),
        (
            Kind::PublicApi,
            r(
                r"\b((is|introduces?|introducing|makes?|making)( this| it)? (a |the )?(major[- ])?breaking change|(is|makes?|making) (it |this )?backwards?[- ]incompatible|alter table|(run|runn?ing|apply|applying|add|adding|write|writing) (a |the )?(database |schema )migration|chang(e|ing) the (\w+ )?schema|(renam|remov)(e|ing) (the |a )?(public |exported )?(endpoint|api|field|column|route)|(chang(e|es|ing)|break(s|ing)?|renam(e|es|ing)|remov(e|es|ing)|bump(s|ing)?) (the |a |our )?(public api|protobuf( schema| messages?)?|openapi( spec)?|graphql schema|wire format|major version|semver major)|bump(s|ing)? to (a |the )?(new )?major( version)?)\b",
            ),
        ),
        (
            Kind::CrossRepo,
            r(
                r"\b(chang(e|es|ing)|patch(es|ing)?|modify(ing)?|edit(s|ing)?|updat(e|es|ing)|fork(s|ing)?)( \S+){0,3}? (the |a )?(other|another|upstream|shared|vendor|cross[- ]repo)[- ](repo(sitory|s)?|library|project|crate|module|infra(structure)?|config(uration)?|team'?s?|api)\b",
            ),
        ),
    ]
});

static SENTENCES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[.!?]\s+|\n|;").unwrap());

/// A sentence that only suggests a move to the engineer, hands it to them ("you: force-push ..."), or
/// says something stays as it is, makes none.
static NOT_A_MOVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(would be|could|might|you (can|may|should|could|might|want)|if you|suggest(ed|ion)?|recommend(ed|ation)?|optionally|unchanged|untouched|no changes?|nothing (is |was )?(changed|touched)|should i|shall i|do you want|want me to|when you'?re ready|whoever|an admin|(stays|is|lies) with you|up to you|your call)\b|\byou:").unwrap()
});

/// The start of a described clause: what a job, script or convention does ("that job runs ...", "which
/// calls ...", "the convention is ..."). A move after it is described, not made; a move before it is
/// still made ("I'm force-pushing main now, which triggers the deploy").
static DESCRIBES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b((which|that|it|job|script|pipeline|workflow|step|hook|command|target) (runs|does|calls|triggers|executes|invokes|ends (in|with))|convention (is|says))\b").unwrap()
});

/// The shared ref or environment a sentence names, for telling two sentences about one move apart.
static REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(main|master|prod|production|staging)\b").unwrap());

/// The target ref or environment of a move ("to origin/main" -> "main").
static TARGET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\S+(?:\s+\S+){0,6}?\s+to\s+(?:the\s+)?(?:(?:origin|upstream)[/\s]+)?([\w./-]+)").unwrap());

const NEGATIONS: [&str; 18] = [
    "no",
    "not",
    "never",
    "without",
    "avoid",
    "instead",
    "skip",
    "nor",
    "dont",
    "wont",
    "didnt",
    "doesnt",
    "isnt",
    "wasnt",
    "shouldnt",
    "cant",
    "cannot",
    "unchanged",
];

fn negation(word: &str) -> bool {
    let w = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'');
    w.ends_with("n't") || NEGATIONS.contains(&w)
}

const STOP: [&str; 24] = [
    "the", "and", "will", "then", "this", "that", "with", "for", "from", "into", "our", "now", "also", "i'll", "we'll", "let", "lets", "can", "should",
    "would", "first", "after", "before", "all",
];

/// A high-consequence move of the plan: its kind and the sentence that makes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub kind: Kind,
    pub sentence: String,
    /// The move itself, normalised ("force push main"): what the key is computed from.
    pub what: String,
    /// Stable hash of the repo and the normalised move (`decision_key`).
    pub key: String,
}

/// Significant words of `text`: tokens of 3+ characters minus filler, sorted and unique.
fn words(text: &str) -> Vec<String> {
    let mut w: Vec<String> = conscience::tokens(text).into_iter().filter(|t| !STOP.contains(&t.as_str())).collect();
    w.sort();
    w.dedup();
    w
}

/// Share of `need`'s words that `have` contains; 0 for fewer than `min` words.
fn overlap(need: &[String], have: &HashSet<String>, min: usize) -> f64 {
    match need.len() >= min {
        true => need.iter().filter(|w| have.contains(*w)).count() as f64 / need.len() as f64,
        false => 0.0,
    }
}

/// FNV-1a 64: stable across builds and platforms, no dependency in the hook binary.
fn fnv(s: &str) -> String {
    format!("{:016x}", s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)))
}

/// A move's words, punctuation and articles ignored ("force-push the" -> "force push").
fn norm(text: &str) -> String {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty() && !["the", "a", "an", "this", "it"].contains(w)).collect::<Vec<_>>().join(" ")
}

/// The decision key: the kind, the repo and the normalised move, so a reworded sentence making the
/// same move in the same repo is the same decision.
pub fn decision_key(kind: Kind, repo: &str, what: &str) -> String {
    fnv(&format!("{kind:?}:{repo}:{what}"))
}

/// The trimmed sentences of `text`, each with whether it ends in a question mark.
fn sentences(text: &str) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    let mut at = 0;
    for m in SENTENCES.find_iter(text) {
        let s = &text[at..m.start()];
        out.push((s.trim(), m.as_str().starts_with('?') || s.trim_end().ends_with('?')));
        at = m.end();
    }
    let s = &text[at..];
    out.push((s.trim(), s.trim_end().ends_with('?')));
    out
}

/// Criterion 1: every high-consequence move the plan makes (not ones it rules out, suggests or says
/// it left unchanged), one per sentence (its earliest move), first occurrence of each decision key.
pub fn classify(plan: &str, repo: &str) -> Vec<Decision> {
    let lower = plan.to_lowercase().replace(['\u{2019}', '\u{2018}'], "'");
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (sentence, asks) in sentences(&lower) {
        // A question: the drone already puts this to the engineer itself.
        if sentence.is_empty() || asks || NOT_A_MOVE.is_match(sentence) {
            continue;
        }
        let described = DESCRIBES.find(sentence).map_or(usize::MAX, |m| m.end());
        let hit = KINDS
            .iter()
            .flat_map(|(kind, re)| re.find_iter(sentence).map(move |m| (*kind, m)))
            .filter(|(_, m)| m.start() < described)
            .filter(|(_, m)| !sentence[..m.start()].split_whitespace().rev().take(4).any(negation))
            .min_by_key(|(_, m)| m.start());
        let Some((kind, m)) = hit else { continue };
        let mut what = norm(m.as_str());
        if let Some(t) = TARGET.captures(&sentence[m.start()..]).and_then(|c| c.get(1)) {
            let t = norm(t.as_str().trim_end_matches(['.', ',']).rsplit('/').next().unwrap_or_default());
            if !t.is_empty() && !what.split(' ').any(|w| w == t) {
                what = format!("{what} {t}");
            }
        }
        let key = decision_key(kind, repo, &what);
        if seen.insert(key.clone()) {
            let sentence: String = sentence.chars().take(300).collect();
            out.push(Decision { kind, sentence, what, key });
        }
    }
    out
}

/// Criterion 3: the engineer's inputs (their prompts, not the drone's own tool calls) already state
/// this decision: they name the same move (a move of its kind with most of its words: "force-push"
/// states "force push main", "wipe the cache" does not), or contain most of the sentence's words.
pub fn stated(d: &Decision, inputs: &str) -> bool {
    let lower = inputs.to_lowercase().replace(['\u{2019}', '\u{2018}'], "'");
    let move_words = words(&d.what);
    let same = |m: &str| {
        let have: HashSet<String> = words(m).into_iter().collect();
        move_words.is_empty() || overlap(&move_words, &have, 1) >= STATED
    };
    if KINDS.iter().any(|(k, re)| *k == d.kind && re.find_iter(&lower).any(|m| same(m.as_str()))) {
        return true;
    }
    let have: HashSet<String> = words(&lower).into_iter().collect();
    overlap(&words(&d.sentence), &have, 3) >= STATED
}

/// A reworded duplicate of an earlier decision question: most of the decision's significant words
/// (at least 4 of them) are in it. Catches rewordings the key does not.
fn near(d: &Decision, earlier: &str) -> bool {
    let have: HashSet<String> = words(earlier).into_iter().collect();
    overlap(&words(&d.sentence), &have, 4) >= STATED
}

/// Two decisions of one plan that are one move: the same key, either sentence a reworded duplicate of
/// the other, or the same kind on the same named ref ("`git push -u origin main`" and "they push to
/// the remote `origin` and the branch `main`"). One message asks each move once.
fn same_move(a: &Decision, b: &Decision) -> bool {
    let target = |d: &Decision| REF.find(&d.sentence).map(|m| m.as_str().replace("production", "prod"));
    a.key == b.key || near(a, &b.sentence) || near(b, &a.sentence) || (a.kind == b.kind && target(a).is_some() && target(a) == target(b))
}

/// What the collective knows about one decision key (criterion 2).
#[derive(Debug, Clone, PartialEq)]
pub enum Known {
    /// No lesson carries this key.
    Nothing,
    /// The lessons carrying it agree: the collective resolves it.
    Resolved,
    /// Lessons carrying it give different consequences, none superseding the others.
    Disagree(Vec<String>),
}

/// The latest version of every lesson line as raw JSON (fields `learn::Lesson` does not know survive).
fn lesson_lines(path: &Path) -> Vec<Value> {
    let mut by: HashMap<String, Value> = HashMap::new();
    let mut order = Vec::new();
    for v in read_lines::<Value>(path) {
        let Some(id) = v["id"].as_str().map(str::to_string) else { continue };
        let ver = |v: &Value| v["version"].as_u64().unwrap_or(0);
        if by.get(&id).is_none_or(|o| ver(o) <= ver(&v)) {
            if !by.contains_key(&id) {
                order.push(id.clone());
            }
            by.insert(id, v);
        }
    }
    order.into_iter().filter_map(|id| by.remove(&id)).collect()
}

pub fn known(lessons: &[Value], key: &str) -> Known {
    let with: Vec<&Value> = lessons.iter().filter(|l| l["decision_key"].as_str() == Some(key)).collect();
    let superseded: HashSet<&str> = with.iter().flat_map(|l| l["supersedes"].as_array().into_iter().flatten().filter_map(Value::as_str)).collect();
    let live: Vec<&&Value> = with.iter().filter(|l| !superseded.contains(l["id"].as_str().unwrap_or_default())).collect();
    let answers: HashSet<String> = live.iter().map(|l| l["consequence"].as_str().unwrap_or_default().trim().to_lowercase()).collect();
    match answers.len() {
        0 => Known::Nothing,
        1 => Known::Resolved,
        _ => Known::Disagree(live.iter().filter_map(|l| l["id"].as_str().map(str::to_string)).collect()),
    }
}

/// One queen question (a line of queue.jsonl). Answering appends the same id with the answer set;
/// the latest line per id wins.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub id: String,
    pub decision_key: String,
    pub kind: Kind,
    pub question: String,
    pub options: Vec<String>,
    pub default: String,
    pub evidence: Vec<String>,
    pub session: String,
    pub cwd: String,
    /// The repo the question is about (`repo_name` of its cwd at queueing); empty on older lines.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo: String,
    pub engineer: String,
    pub ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_ms: Option<i64>,
    /// Lessons whose disagreement led to the question; the answer supersedes them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disagreeing: Vec<String>,
    /// Why the question was dismissed (`borg queen dismiss`, `dedupe`): it is no longer open, never
    /// delivered, and no lesson is written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_ms: Option<i64>,
}

pub fn dir(store: &Path) -> PathBuf {
    store.join("queen")
}

fn queue_path(store: &Path) -> PathBuf {
    dir(store).join("queue.jsonl")
}

fn sessions_path(store: &Path) -> PathBuf {
    dir(store).join("sessions.jsonl")
}

fn read_lines<T: for<'de> Deserialize<'de>>(path: &Path) -> Vec<T> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn append_line<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut line = serde_json::to_string(v).map_err(std::io::Error::other)?;
    line.push('\n');
    // One write(2) per record: concurrent appenders (parallel sessions' hooks) never interleave a line.
    std::fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(line.as_bytes())
}

/// Every question ever asked of this collective, latest version per id, in asking order.
pub fn entries(store: &Path) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for e in read_lines::<Entry>(&queue_path(store)) {
        match out.iter_mut().find(|o| o.id == e.id) {
            Some(o) => *o = e,
            None => out.push(e),
        }
    }
    out
}

/// The key of an open decision found in a team document: its source repo and normalised question.
pub fn open_decision_key(repo: &str, question: &str) -> String {
    decision_key(Kind::OpenDecision, repo, &norm(&question.to_lowercase()))
}

/// Queue `e` (an imported open decision) unless its decision, or a near-duplicate question of its repo
/// (`duplicate`), was ever queued in this collective. Returns whether it was added.
pub fn seed(store: &Path, e: &Entry) -> std::io::Result<bool> {
    if entries(store).iter().any(|x| x.decision_key == e.decision_key || duplicate(x, e)) {
        return Ok(false);
    }
    append_line(&queue_path(store), e)?;
    Ok(true)
}

/// Questions still waiting for an answer (neither answered nor dismissed), in asking order: `borg
/// queen` numbers them 1.. in this order.
pub fn open(store: &Path) -> Vec<Entry> {
    entries(store).into_iter().filter(Entry::is_open).collect()
}

impl Entry {
    pub fn is_open(&self) -> bool {
        self.answer.is_none() && self.dismissed.is_none()
    }
}

/// A session the queen saw a checkpoint of (a line of sessions.jsonl), in first-seen order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Seen {
    pub engineer: String,
    pub session: String,
    pub ms: i64,
}

/// Count this checkpoint's session for its engineer (once per session).
fn note_session(store: &Path, engineer: &str, session: &str, now: i64) -> std::io::Result<()> {
    let seen: Vec<Seen> = read_lines(&sessions_path(store));
    if seen.iter().any(|s| s.engineer == engineer && s.session == session) {
        return Ok(());
    }
    append_line(&sessions_path(store), &Seen { engineer: engineer.into(), session: session.into(), ms: now })
}

/// Criterion 4: the engineer was never asked, or at least `BUDGET_SESSIONS` of their sessions began
/// since the session in which they were last asked (that one counted as the first).
pub fn within_budget(store: &Path, engineer: &str, session: &str) -> bool {
    budget_allows(&entries(store), &read_lines::<Seen>(&sessions_path(store)), engineer, session, collective::aliases())
}

/// Criterion 4, pure: `asked` every question so far, `seen` every session in first-seen order. An
/// engineer's emails under `aliases` share one budget; open decisions imported from documents use none.
pub fn budget_allows(asked: &[Entry], seen: &[Seen], engineer: &str, session: &str, aliases: &collective::Aliases) -> bool {
    let me = aliases.canon(engineer);
    let sent = asked.iter().filter(|e| e.kind != Kind::OpenDecision);
    let Some(last) = sent.filter(|e| aliases.canon(&e.engineer) == me).max_by_key(|e| e.ms) else { return true };
    if last.session == session {
        return false; // one message per session at most
    }
    let mine: Vec<&Seen> = seen.iter().filter(|s| aliases.canon(&s.engineer) == me).collect();
    let now = mine.iter().position(|s| s.session == session);
    let then = mine.iter().position(|s| s.session == last.session);
    let since = match (then, now) {
        (Some(t), Some(n)) => n.saturating_sub(t),
        // The asking session's record is gone: count sessions first seen after the question.
        _ => mine.iter().filter(|s| s.ms > last.ms).count(),
    };
    since >= BUDGET_SESSIONS
}

/// One Stop checkpoint as the queen sees it.
pub struct Checkpoint<'a> {
    /// The routed collective's store.
    pub store: &'a Path,
    pub session: &'a str,
    pub cwd: &'a str,
    pub engineer: &'a str,
    /// The drone's plan / last proposal.
    pub plan: &'a str,
    /// What the engineer gave the drone this session: their prompts (not the drone's own tool calls,
    /// which would otherwise "state" any move the drone already ran).
    pub inputs: &'a str,
    /// The lessons the conscience asked at this checkpoint.
    pub selected: &'a [&'a Challenge],
    pub now: i64,
}

fn question(d: &Decision) -> String {
    format!("The drone's plan says: \"{}\". That is {}. Is this the right call here, or should it be done differently?", d.sentence, d.kind.label())
}

/// A lesson is about a move when its question, situations and consequence name most (`STATED`) of
/// the move's significant words ("force push main").
pub fn covers(c: &Challenge, d: &Decision) -> bool {
    let have: HashSet<String> = words(&format!("{} {} {}", c.question, c.situations.join(" "), c.consequence)).into_iter().collect();
    overlap(&words(&d.what), &have, 1) >= STATED
}

/// Lessons among `selected` that contradict another one (`conflict_pairs`). Ids in selection order.
pub fn conflicts(selected: &[&Challenge]) -> Vec<String> {
    let pairs = conflict_pairs(selected);
    (0..selected.len()).filter(|i| pairs.iter().any(|&(a, b)| a == *i || b == *i)).map(|i| selected[i].id.clone()).collect()
}

/// Pairs (i < j, indices into `selected`) of lessons that contradict each other: their situations
/// mostly agree (≥ `STATED` of the shorter one's words) and their consequences mostly do not.
pub fn conflict_pairs(selected: &[&Challenge]) -> Vec<(usize, usize)> {
    let set = |v: &[String]| words(&v.join(" ")).into_iter().collect::<HashSet<String>>();
    let same = |a: &HashSet<String>, b: &HashSet<String>| {
        let n = a.len().min(b.len());
        n > 0 && a.intersection(b).count() as f64 / n as f64 >= STATED
    };
    let form: Vec<(HashSet<String>, HashSet<String>)> = selected.iter().map(|c| (set(&c.situations), set(std::slice::from_ref(&c.consequence)))).collect();
    let mut out = Vec::new();
    for i in 0..selected.len() {
        for j in i + 1..selected.len() {
            if !form[i].1.is_empty() && !form[j].1.is_empty() && same(&form[i].0, &form[j].0) && !same(&form[i].1, &form[j].1) {
                out.push((i, j));
            }
        }
    }
    out
}

/// Decide whether to ask the queen and, if so, queue the questions. Returns the asked entries
/// (empty: nothing asked). Errors only on I/O; the caller fails open. Cheap checks first: the
/// lessons file is read only when a candidate decision survives them and the budget. A question that
/// near-duplicates one already queued for its repo (`duplicate`) is not queued again. Newly queued
/// questions get one desktop notification (notify.rs).
pub fn consider(cp: &Checkpoint) -> std::io::Result<Vec<Entry>> {
    note_session(cp.store, cp.engineer, cp.session, cp.now)?;
    let asked = entries(cp.store);
    let mut ask: Vec<Entry> = Vec::new();
    for e in decide(cp, &asked, || within_budget(cp.store, cp.engineer, cp.session), || lesson_lines(&cp.store.join("lessons.jsonl"))) {
        if !asked.iter().chain(&ask).any(|x| duplicate(x, &e)) {
            ask.push(e);
        }
    }
    for e in &ask {
        append_line(&queue_path(cp.store), e)?;
    }
    crate::notify::queued(&ask);
    Ok(ask)
}

/// The queen's decision at one checkpoint, without I/O (`consider` is this plus the store; the
/// replay eval calls it directly): `asked` is every question asked so far, `budget` criterion 4 and
/// `lessons` the collective's lessons as raw JSON, both evaluated only once a candidate survives
/// criteria 1-3. `cp.store` is not read.
pub fn decide(cp: &Checkpoint, asked: &[Entry], budget: impl FnOnce() -> bool, lessons: impl FnOnce() -> Vec<Value>) -> Vec<Entry> {
    let repo = repo_name(cp.cwd);
    let ever: HashSet<&str> = asked.iter().map(|e| e.decision_key.as_str()).collect();
    let mut candidates: Vec<(Decision, Vec<String>)> = Vec::new();
    for d in classify(cp.plan, &repo) {
        if ever.contains(d.key.as_str()) || stated(&d, cp.inputs) || asked.iter().any(|e| near(&d, &e.question)) {
            continue;
        }
        // Criterion 2, L1 side: the lessons the conscience asked that are about this move. Agreeing,
        // they answered it; contradicting each other, the collective cannot resolve it. Lessons about
        // other things do not answer it (the conscience selects something for almost any plan).
        let about: Vec<&Challenge> = cp.selected.iter().copied().filter(|c| covers(c, &d)).collect();
        let clashing = conflicts(&about);
        if !about.is_empty() && clashing.is_empty() {
            continue;
        }
        candidates.push((d, clashing));
    }
    if candidates.is_empty() || !budget() {
        return Vec::new();
    }
    let lessons = lessons();
    let mut ask = Vec::new();
    let mut picked: Vec<Decision> = Vec::new();
    for (d, relevant) in candidates {
        if ask.len() == MAX_QUESTIONS {
            break;
        }
        if picked.iter().any(|p| same_move(&d, p)) {
            continue;
        }
        let answered = lessons.iter().any(|l| l["source"] == "queen" && near(&d, l["decision_question"].as_str().unwrap_or_default()));
        let disagreeing = match (known(&lessons, &d.key), answered) {
            (Known::Resolved, _) | (Known::Nothing, true) => continue,
            (Known::Nothing, false) => relevant,
            (Known::Disagree(ids), _) => ids.into_iter().chain(relevant).collect::<Vec<_>>(),
        };
        let (default, other) = d.kind.options();
        let mut evidence = vec![format!("plan: \"{}\"", d.sentence), format!("high consequence: {}", d.kind.label())];
        evidence.push(match disagreeing.is_empty() {
            true => "no lesson of this collective is about this move".into(),
            false => format!("lessons {} disagree", disagreeing.join(", ")),
        });
        evidence.push("the engineer's prompts this session do not state this decision".into());
        picked.push(d.clone());
        ask.push(Entry {
            id: format!("q{}", &d.key[..8]),
            decision_key: d.key.clone(),
            kind: d.kind,
            question: question(&d),
            options: vec![default.into(), other.into()],
            default: default.into(),
            evidence,
            session: cp.session.into(),
            cwd: cp.cwd.into(),
            repo: repo.clone(),
            engineer: cp.engineer.into(),
            ms: cp.now,
            answer: None,
            answered_by: None,
            answered_ms: None,
            disagreeing,
            dismissed: None,
            dismissed_ms: None,
        });
    }
    ask
}

/// What the drone is told, once: whom was asked what, and which default it proceeds with.
pub fn render(asked: &[Entry]) -> String {
    let who = asked.first().map(|e| e.engineer.as_str()).filter(|e| !e.is_empty()).unwrap_or("the engineer");
    let mut s = format!(
        "Borg asked the engineer ({who}) about a high-consequence decision this collective has no settled lesson for. \
Do not wait for the answer and do not ask again: proceed with the default below unless an answer arrives. \
Mention the open question and the default you took in your final message.\n"
    );
    for (i, e) in asked.iter().enumerate() {
        s.push_str(&format!("\n{}. [{}] {}\n   proceed with default: {}", i + 1, e.id, e.question, e.default));
    }
    s
}

/// Whether the Stop hook asks the queen: only with `BORG_QUEEN=on` (off by default since the queen test).
pub fn enabled() -> bool {
    std::env::var("BORG_QUEEN").is_ok_and(|v| v.trim().eq_ignore_ascii_case("on"))
}

/// The Stop hook's single call, after the conscience decided. `prompts` are the engineer's prompts
/// this session, `selected` the lessons the conscience asked. Some(block once) when the queen was
/// asked; its reason then carries the conscience's questions too.
pub fn stop(session: &str, input: &Value, prompts: &str, selected: &[&Challenge]) -> Option<Value> {
    if !learn::learning() || !enabled() {
        return None;
    }
    std::panic::catch_unwind(|| {
        let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default();
        let last = s("last_assistant_message");
        let plan = match last.trim().is_empty() {
            true => learn::last_assistant_text(s("transcript_path").as_ref()),
            false => last.to_string(),
        };
        let store = learn::home();
        let engineer = learn::engineer(s("cwd"));
        let cp = Checkpoint { store: &store, session, cwd: s("cwd"), engineer: &engineer, plan: &plan, inputs: prompts, selected, now: learn::now_ms() };
        let asked = consider(&cp).ok()?;
        let reason = match selected.is_empty() {
            true => render(&asked),
            false => format!("{}\n\n{}", conscience::render(selected), render(&asked)),
        };
        (!asked.is_empty()).then(|| json!({"decision": "block", "reason": reason}))
    })
    .ok()
    .flatten()
}

/// The normalised consequence `known` compares answers by.
fn said(text: &str) -> String {
    text.trim().to_lowercase()
}

/// The lesson an answer becomes: the decision question with the engineer's answer, keyed by the
/// decision so the queen is never asked it again. It supersedes only the disagreeing lessons the
/// answer overrules (their consequence differs from it, as `known` compares), never the side it
/// confirms, and takes the area most of the disagreeing lessons (`lessons`, latest lines) share.
pub fn lesson(e: &Entry, answer: &str, author: &str, now: i64, lessons: &[Value]) -> Value {
    let repo = Path::new(&e.cwd).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let sides: Vec<&Value> = e.disagreeing.iter().filter_map(|id| lessons.iter().find(|l| l["id"].as_str() == Some(id))).collect();
    // A disagreeing lesson that is gone from the store cannot be compared: it stays superseded.
    let supersedes: Vec<&String> = e
        .disagreeing
        .iter()
        .filter(|id| !sides.iter().any(|l| l["id"].as_str() == Some(id) && said(l["consequence"].as_str().unwrap_or_default()) == said(answer)))
        .collect();
    let mut areas: Vec<(&str, usize)> = Vec::new();
    for a in sides.iter().map(|l| crate::trust::area(l["area"].as_str().unwrap_or_default())) {
        match areas.iter_mut().find(|(x, _)| *x == a) {
            Some((_, n)) => *n += 1,
            None => areas.push((a, 1)),
        }
    }
    // Most shared area; ties go to the first disagreeing lesson's.
    let area = areas.iter().fold(None, |best: Option<(&str, usize)>, &(a, n)| match best {
        Some((_, m)) if m >= n => best,
        _ => Some((a, n)),
    });
    json!({
        "id": format!("queen-{}", e.decision_key),
        "origin_ms": now,
        "repo": repo,
        // The conscience shows only the question, so it carries the answer too.
        "question": format!("{} The engineer ({author}) decided: \"{answer}\"", e.question),
        "situations": [format!("{} under a plan that says: {}", e.kind.label(), e.evidence.first().map(String::as_str).unwrap_or_default())],
        "consequence": answer,
        "support": 1,
        "episodes": [],
        "history": [],
        "version": 1,
        "engineers": if author.is_empty() { vec![] } else { vec![author.to_string()] },
        "author": author,
        "decision_key": e.decision_key,
        "decision_question": e.question,
        "area": area.map_or(crate::trust::OTHER, |(a, _)| a),
        "supersedes": supersedes,
        "source": "queen",
    })
}

/// Record the engineer's answer to `id` and add it to the collective's lessons.
pub fn answer(store: &Path, id: &str, text: &str, author: &str, now: i64) -> Result<Entry, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("the answer is empty".into());
    }
    let Some(mut e) = entries(store).into_iter().find(|e| e.id == id) else {
        return Err(format!("no queen question {id} in this collective"));
    };
    if e.answer.is_some() {
        return Err(format!("{id} is already answered"));
    }
    if e.dismissed.is_some() {
        return Err(format!("{id} was dismissed"));
    }
    // The lesson first: a question marked answered without its lesson would be lost.
    let lessons = lesson_lines(&store.join("lessons.jsonl"));
    append_line(&store.join("lessons.jsonl"), &lesson(&e, text, author, now, &lessons)).map_err(|err| err.to_string())?;
    e.answer = Some(text.into());
    e.answered_by = Some(author.into());
    e.answered_ms = Some(now);
    append_line(&queue_path(store), &e).map_err(|err| err.to_string())?;
    Ok(e)
}

// ---------------------------------------------------------------------------------------------------
// Answering the queue: numbering, dismissing, skipping, deduplicating, the machine interface and the
// in-harness delivery (one question per session, in the session's own repo).

/// A skipped question is not delivered again for this long (3 days).
pub const SKIP_MS: i64 = 3 * 24 * 60 * 60 * 1000;
/// A question delivered but neither answered nor skipped is offered again only once more than this many
/// sessions (with open questions of their repos) began after the delivering one.
pub const REOFFER_SESSIONS: usize = 3;
/// Two questions of one repo whose significant words overlap this much (Jaccard) are one question.
pub const DUPLICATE: f64 = 0.6;

fn skips_path(store: &Path) -> PathBuf {
    dir(store).join("skips.jsonl")
}

fn delivered_path(store: &Path) -> PathBuf {
    dir(store).join("delivered.jsonl")
}

fn visits_path(store: &Path) -> PathBuf {
    dir(store).join("visits.jsonl")
}

/// The repo of a directory as the hooks name it: its git root's base name, a worktree folded onto its
/// repo (`borg_trace::repo_key`); the directory's own name outside git.
pub fn repo_name(cwd: &str) -> String {
    Path::new(&borg_trace::repo_key(cwd)).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The repo a question is about: the one recorded when it was queued, else `repo_name` of the
/// directory it was asked (or imported) in.
pub fn repo(e: &Entry) -> String {
    if e.repo.is_empty() { repo_name(&e.cwd) } else { e.repo.clone() }
}

fn same_repo(a: &str, b: &str) -> bool {
    !a.is_empty() && a.eq_ignore_ascii_case(b)
}

/// What a question is compared and summarised by: a checkpoint question's quoted plan sentence (its
/// frame is the same for every move), else the question itself.
pub fn gist(e: &Entry) -> &str {
    e.question.strip_prefix("The drone's plan says: \"").and_then(|r| r.split_once("\". That is ")).map_or(e.question.as_str(), |(s, _)| s)
}

/// `text` trimmed and cut to `max` characters, an ellipsis marking the cut.
pub fn clip(text: &str, max: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    let mut s: String = t.chars().take(max.saturating_sub(1)).collect::<String>().trim_end().to_string();
    s.push('…');
    s
}

/// Jaccard similarity of the significant words of `a` and `b` (0 when both have none).
pub fn jaccard(a: &str, b: &str) -> f64 {
    let (a, b): (HashSet<String>, HashSet<String>) = (words(a).into_iter().collect(), words(b).into_iter().collect());
    match a.union(&b).count() {
        0 => 0.0,
        n => a.intersection(&b).count() as f64 / n as f64,
    }
}

/// Two questions are one: the same repo and `DUPLICATE` of their words (`gist`) shared.
pub fn duplicate(a: &Entry, b: &Entry) -> bool {
    repo(a).eq_ignore_ascii_case(&repo(b)) && jaccard(gist(a), gist(b)) >= DUPLICATE
}

/// A question marked in a session: a skip (a line of skips.jsonl; session "cli" from the terminal) or a
/// delivery by the prompt hook (a line of delivered.jsonl).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Mark {
    pub id: String,
    pub session: String,
    pub ms: i64,
}

/// A session the delivery saw a prompt of while its repos had open questions (a line of visits.jsonl),
/// in first-seen order: what "3 more sessions" counts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Visit {
    pub session: String,
    pub ms: i64,
}

pub fn skips(store: &Path) -> Vec<Mark> {
    read_lines(&skips_path(store))
}

pub fn deliveries(store: &Path) -> Vec<Mark> {
    read_lines(&delivered_path(store))
}

/// `id` was skipped less than `SKIP_MS` before `now`.
pub fn skipped(skips: &[Mark], id: &str, now: i64) -> bool {
    skips.iter().any(|s| s.id == id && now.saturating_sub(s.ms) < SKIP_MS)
}

/// The open question `id`, or why it is not one.
fn find_open(store: &Path, id: &str) -> Result<Entry, String> {
    let Some(e) = entries(store).into_iter().find(|e| e.id == id) else {
        return Err(format!("no queen question {id} in this collective"));
    };
    match (&e.answer, &e.dismissed) {
        (Some(_), _) => Err(format!("{id} is already answered")),
        (_, Some(_)) => Err(format!("{id} was dismissed")),
        _ => Ok(e),
    }
}

/// Dismiss `id`: no longer open, never delivered, no lesson written. The latest queue line wins.
pub fn dismiss(store: &Path, id: &str, reason: &str, now: i64) -> Result<Entry, String> {
    let mut e = find_open(store, id)?;
    e.dismissed = Some(Some(reason.trim()).filter(|r| !r.is_empty()).unwrap_or("dismissed").to_string());
    e.dismissed_ms = Some(now);
    append_line(&queue_path(store), &e).map_err(|err| err.to_string())?;
    Ok(e)
}

/// Skip `id` in `session`: it stays open but is not delivered again for `SKIP_MS`.
pub fn skip(store: &Path, id: &str, session: &str, now: i64) -> Result<Entry, String> {
    let e = find_open(store, id)?;
    append_line(&skips_path(store), &Mark { id: e.id.clone(), session: session.into(), ms: now }).map_err(|err| err.to_string())?;
    Ok(e)
}

/// `key` as `borg queen` takes it: a listing number n (1-based, into `open` in asking order), else the
/// id of any question in `all`.
pub fn resolve<'a>(open: &'a [Entry], all: &'a [Entry], key: &str) -> Option<&'a Entry> {
    let key = key.trim();
    match key.parse::<usize>() {
        Ok(n) => n.checked_sub(1).and_then(|i| open.get(i)),
        Err(_) => all.iter().find(|e| e.id == key),
    }
}

/// An answer as typed: exactly an option's letter ("a", "b") or number ("1", "2") stands for that
/// option's text; a lone letter or number that names no option is refused (a typo, never recorded as
/// a lesson) when the question has options; anything else is the answer itself.
pub fn option_answer(e: &Entry, text: &str) -> Result<String, String> {
    let t = text.trim();
    let i = match t.as_bytes() {
        [c @ b'a'..=b'z'] => Some((c - b'a') as usize),
        [c @ b'A'..=b'Z'] => Some((c - b'A') as usize),
        _ => t.parse::<usize>().ok().map(|n| n.wrapping_sub(1)),
    };
    match (i, e.options.len()) {
        (Some(i), n) if i < n => Ok(e.options[i].clone()),
        (Some(_), n) if n > 0 => Err(format!("no option {t} (a-{} or 1-{n}); to answer in your own words, write more than a letter or number", letter(n - 1))),
        _ => Ok(t.to_string()),
    }
}

/// Open questions that near-duplicate an earlier, not dismissed question of their repo (`duplicate`):
/// (later id, earlier id) in asking order. A duplicate found here is never the earlier side.
pub fn duplicates(all: &[Entry]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (j, e) in all.iter().enumerate().filter(|(_, e)| e.is_open()) {
        let first = all[..j].iter().find(|x| x.dismissed.is_none() && !out.iter().any(|(d, _)| *d == x.id) && duplicate(x, e));
        if let Some(x) = first {
            out.push((e.id.clone(), x.id.clone()));
        }
    }
    out
}

/// `borg queen dedupe`: the duplicates; with `apply`, each dismissed as "duplicate of <id>".
pub fn dedupe(store: &Path, apply: bool, now: i64) -> Result<Vec<(String, String)>, String> {
    let found = duplicates(&entries(store));
    if apply {
        for (id, of) in &found {
            dismiss(store, id, &format!("duplicate of {of}"), now)?;
        }
    }
    Ok(found)
}

/// One open question for a drone or a script: `borg queen --json` (an array), `borg queen next --json`
/// (one, or null) and the prompt hook. The schema is stable: `id`, `n` (its number in `borg queen`),
/// `repo`, `kind` (snake_case `Kind`), `question`, `options`, `default`, `evidence`, `ms` (asked, epoch
/// ms) and three exact shell commands: `answer_cmd` (replace `<answer>` inside its single quotes),
/// `skip_cmd` and `dismiss_cmd` (a reason may follow).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Question {
    pub id: String,
    pub n: usize,
    pub repo: String,
    pub kind: Kind,
    pub question: String,
    pub options: Vec<String>,
    pub default: String,
    pub evidence: Vec<String>,
    pub ms: i64,
    pub answer_cmd: String,
    pub skip_cmd: String,
    pub dismiss_cmd: String,
}

/// `s` single-quoted for a POSIX shell.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

impl Question {
    /// `e` as listing number `n`. `scope` follows each command (" --collective 'team'"); empty, the
    /// command routes by the directory it runs in.
    pub fn of(e: &Entry, n: usize, scope: &str) -> Question {
        let id = sh_quote(&e.id);
        Question {
            id: e.id.clone(),
            n,
            repo: repo(e),
            kind: e.kind,
            question: e.question.clone(),
            options: e.options.clone(),
            default: e.default.clone(),
            evidence: e.evidence.clone(),
            ms: e.ms,
            answer_cmd: format!("borg queen answer {id} '<answer>'{scope}"),
            skip_cmd: format!("borg queen skip {id}{scope}"),
            dismiss_cmd: format!("borg queen dismiss {id}{scope}"),
        }
    }

    /// `self` with its commands run by `bin` (a shell word, e.g. the hook's own sh-quoted exe) rather
    /// than by `borg` on PATH.
    pub fn run_by(mut self, bin: &str) -> Question {
        for c in [&mut self.answer_cmd, &mut self.skip_cmd, &mut self.dismiss_cmd] {
            if let Some(rest) = c.strip_prefix("borg ") {
                *c = format!("{bin} {rest}");
            }
        }
        self
    }
}

/// The open questions as `borg queen --json` lists them, only `only`'s repo when given.
pub fn questions(open: &[Entry], only: Option<&str>, scope: &str) -> Vec<Question> {
    open.iter().enumerate().filter(|(_, e)| only.is_none_or(|r| same_repo(r, &repo(e)))).map(|(i, e)| Question::of(e, i + 1, scope)).collect()
}

/// The question a drone should ask now (`borg queen next`): the first open one (of `only`'s repo when
/// given) not skipped within `SKIP_MS`. Read only.
pub fn next(open: &[Entry], skips: &[Mark], only: Option<&str>, now: i64, scope: &str) -> Option<Question> {
    questions(open, only, scope).into_iter().find(|q| !skipped(skips, &q.id, now))
}

/// Whether the prompt hook delivers queen questions: `BORG_QUEEN_ASK` is not "off" (default on).
pub fn ask_enabled() -> bool {
    ask_on(std::env::var("BORG_QUEEN_ASK").ok().as_deref())
}

/// `ask_enabled` for a given `BORG_QUEEN_ASK` value.
pub fn ask_on(value: Option<&str>) -> bool {
    !value.is_some_and(|v| v.trim().eq_ignore_ascii_case("off"))
}

/// The question to deliver in `session`, pure. None once `delivered` has one for this session (one per
/// session). Else an open question of one of `repos` (never another repo's), not skipped within
/// `SKIP_MS`, and, if delivered before, only once more than `REOFFER_SESSIONS` sessions of `visits`
/// (first-seen order) began since. Never-delivered questions first, then asking order. The index is
/// into `open`.
pub fn eligible<'a>(
    open: &'a [Entry],
    skips: &[Mark],
    delivered: &[Mark],
    visits: &[Visit],
    session: &str,
    repos: &[String],
    now: i64,
) -> Option<(usize, &'a Entry)> {
    if delivered.iter().any(|d| d.session == session) {
        return None;
    }
    let at = |s: &str| visits.iter().position(|v| v.session == s);
    open.iter()
        .enumerate()
        .filter(|(_, e)| repos.iter().any(|r| same_repo(r, &repo(e))) && !skipped(skips, &e.id, now))
        .filter_map(|(i, e)| {
            let Some(last) = delivered.iter().filter(|d| d.id == e.id).max_by_key(|d| d.ms) else { return Some((false, i, e)) };
            let since = match (at(&last.session), at(session)) {
                (Some(t), Some(n)) => n.saturating_sub(t),
                // The delivering session's record is gone: count sessions first seen after the delivery.
                _ => visits.iter().filter(|v| v.ms > last.ms).count(),
            };
            (since > REOFFER_SESSIONS).then_some((true, i, e))
        })
        .min_by_key(|&(again, i, _)| (again, i))
        .map(|(_, i, e)| (i, e))
}

/// The prompt hook's call (0 model calls): the one question to ask in `session`, whose work is in
/// `repos` (computed only once the session has none delivered and the queue has open questions),
/// recorded as delivered (delivered.jsonl). Its commands end with `scope`. None when nothing is
/// eligible (`eligible`) or the store cannot record it: fails open. `BORG_QUEEN_ASK` is the caller's.
pub fn deliver(store: &Path, session: &str, repos: impl FnOnce() -> Vec<String>, scope: &str, now: i64) -> Option<Question> {
    if session.is_empty() {
        return None;
    }
    let delivered = deliveries(store);
    if delivered.iter().any(|d| d.session == session) {
        return None;
    }
    let open = open(store);
    if open.is_empty() {
        return None;
    }
    let repos = repos();
    if !open.iter().any(|e| repos.iter().any(|r| same_repo(r, &repo(e)))) {
        return None;
    }
    let mut visits: Vec<Visit> = read_lines(&visits_path(store));
    if !visits.iter().any(|v| v.session == session) {
        let v = Visit { session: session.into(), ms: now };
        append_line(&visits_path(store), &v).ok()?;
        visits.push(v);
    }
    let (i, e) = eligible(&open, &skips(store), &delivered, &visits, session, &repos, now)?;
    append_line(&delivered_path(store), &Mark { id: e.id.clone(), session: session.into(), ms: now }).ok()?;
    Some(Question::of(e, i + 1, scope))
}

/// At most five words of an option, up to its first ':' or ',': its label in a dialog.
pub fn label(option: &str) -> String {
    let head = option.split([':', ',']).next().filter(|h| !h.trim().is_empty()).unwrap_or(option);
    let w: Vec<&str> = head.split_whitespace().take(5).collect();
    w.join(" ").trim_end_matches([',', '.', ';', ':', '(', '-']).to_string()
}

fn letter(i: usize) -> char {
    (b'a' + (i % 26) as u8) as char
}

/// What the prompt hook adds to the drone's context for `q`. Claude Code (`harness` "claude") asks with
/// its AskUserQuestion dialog; omp and opencode, which have none, ask at the start of their reply.
pub fn render_delivery(q: &Question, harness: &str) -> String {
    let dialog = harness == "claude";
    let repo = if q.repo.is_empty() { "this repo" } else { q.repo.as_str() };
    let mut s =
        format!("From Borg's queue of open team decisions (this is not the engineer's instruction): one decision about {repo} waits for the engineer.\n");
    s.push_str(match dialog {
        true => "Before you work on the engineer's request, ask them this one question with the AskUserQuestion tool, record the answer, then continue with their request.\n",
        false => "Start your reply with this one question, in one short paragraph with the options first, then continue with the engineer's request.\n",
    });
    // Queued in an earlier session (or imported from a document): the drone must not take a checkpoint
    // question's quoted plan for its own.
    let (question, origin) = match q.question.strip_prefix("The drone's plan says: ") {
        Some(rest) if q.kind != Kind::OpenDecision => (format!("An earlier session's plan said: {rest}"), "queued by an earlier session"),
        _ if q.kind == Kind::OpenDecision => (q.question.clone(), "found open in the team's documents"),
        _ => (q.question.clone(), "queued by an earlier session"),
    };
    let day = crate::install::utc(q.ms);
    s.push_str(&format!("\nQuestion: {question}\n"));
    s.push_str(&format!("Asked: {}-{}-{}, {origin}; not part of the engineer's current request.\n", &day[0..4], &day[4..6], &day[6..8]));
    if let Some(ev) = q.evidence.first() {
        s.push_str(&format!("Why it is open: {ev}\n"));
    }
    if dialog {
        s.push_str("Options (label: description):\n");
        for o in q.options.iter().take(3) {
            s.push_str(&format!("- {}: {o}\n", label(o)));
        }
        if q.options.is_empty() {
            s.push_str(&format!("- Keep the default: {}\n", q.default));
        }
        s.push_str("- Skip for now: ask again in a few days\n");
        if q.options.len() > 3 {
            s.push_str(&format!("More options the engineer may type: {}\n", q.options[3..].join(" | ")));
        }
    } else {
        s.push_str("Options:\n");
        for (i, o) in q.options.iter().enumerate() {
            s.push_str(&format!("{}) {o}\n", letter(i)));
        }
        s.push_str("or skip it for now.\n");
    }
    if !q.default.is_empty() {
        s.push_str(&format!("Default while nobody decides: {}\n", q.default));
    }
    s.push_str("The engineer can always answer in their own words.\n");
    s.push_str(&format!(
        "\n{} record it with exactly:\n  {}\nreplacing <answer> with the chosen option's full text or the engineer's own words, inside the single quotes (write a ' as '\\'').\n",
        if dialog { "Then" } else { "When the engineer answers," },
        q.answer_cmd
    ));
    s.push_str(&format!("If the engineer skips it: {}\nIf they say it no longer applies: {} '<reason>'\n", q.skip_cmd, q.dismiss_cmd));
    s.push_str("Ask it only once in this session.");
    s
}

fn paint(color: bool, code: &str, s: &str) -> String {
    match color {
        true => format!("\x1b[{code}m{s}\x1b[0m"),
        false => s.to_string(),
    }
}

/// The options of `e` on one line: "a) … · b) … (default)", the default appended when no option is it.
fn options_line(e: &Entry) -> String {
    let mut parts: Vec<String> =
        e.options.iter().enumerate().map(|(i, o)| format!("{}) {o}{}", letter(i), if *o == e.default { " (default)" } else { "" })).collect();
    if !e.options.contains(&e.default) && !e.default.is_empty() {
        parts.push(format!("default: {}", e.default));
    }
    parts.join(" · ")
}

const HINT: &str = "answer: borg queen ask · borg queen answer <n> <text|a|b> · borg queen show <n>";

/// `borg queen`: the open questions (only `only`'s repo when given) numbered in asking order, grouped by
/// repo (repos in order of their first question), each line cut to `width` characters; ANSI colors
/// when `color`.
pub fn render_list(open: &[Entry], only: Option<&str>, width: usize, color: bool) -> String {
    let mut repos: Vec<String> = Vec::new();
    for r in open.iter().map(repo).filter(|r| only.is_none_or(|o| same_repo(o, r))) {
        if !repos.contains(&r) {
            repos.push(r);
        }
    }
    let body = width.saturating_sub(5).max(10);
    let mut s = String::new();
    for r in &repos {
        let mine: Vec<(usize, &Entry)> = open.iter().enumerate().filter(|(_, e)| repo(e) == *r).collect();
        let name = if r.is_empty() { "(no repo)" } else { r };
        s.push_str(&format!("{}\n", paint(color, "1", &format!("{name} ({})", mine.len()))));
        for (i, e) in mine {
            s.push_str(&format!("{}  {}\n", paint(color, "1;36", &format!("{:>3}", i + 1)), clip(&e.question, body)));
            let opts = options_line(e);
            if !opts.is_empty() {
                s.push_str(&format!("     {}\n", paint(color, "2", &clip(&opts, body))));
            }
        }
        s.push('\n');
    }
    s.push_str(&paint(color, "2", &clip(HINT, width)));
    s.push('\n');
    s
}

/// `borg queen show`: everything about one question; `n` its listing number when open.
pub fn render_show(e: &Entry, n: Option<usize>) -> String {
    let who = |s: &str| if s.is_empty() { "?".to_string() } else { s.to_string() };
    let mut s = match n {
        Some(n) => format!("#{n}  {}\n", e.id),
        None => format!("{}\n", e.id),
    };
    s.push_str(&format!("{}\n\n", e.question));
    s.push_str(&format!("kind:     {}\n", e.kind.label()));
    s.push_str(&format!("repo:     {} ({})\n", repo(e), e.cwd));
    s.push_str(&format!("asked:    {} by {} (session {})\n", crate::install::utc(e.ms), who(&e.engineer), e.session));
    if !e.options.is_empty() {
        s.push_str("options:\n");
        for (i, o) in e.options.iter().enumerate() {
            s.push_str(&format!("  {}) {o}{}\n", letter(i), if *o == e.default { " (default)" } else { "" }));
        }
    }
    s.push_str(&format!("default:  {}\n", e.default));
    if !e.evidence.is_empty() {
        s.push_str("evidence:\n");
        for ev in &e.evidence {
            s.push_str(&format!("  - {ev}\n"));
        }
    }
    let status = match (&e.answer, &e.dismissed) {
        (Some(a), _) => {
            format!("answered by {} at {}: {a}", who(e.answered_by.as_deref().unwrap_or_default()), crate::install::utc(e.answered_ms.unwrap_or(0)))
        }
        (_, Some(r)) => format!("dismissed at {}: {r}", crate::install::utc(e.dismissed_ms.unwrap_or(0))),
        _ => "open".into(),
    };
    s.push_str(&format!("status:   {status}\n"));
    s
}

/// What `borg queen ask` did: (answered, skipped, dismissed).
pub type Asked = (usize, usize, usize);

/// `borg queen ask` over any input: each open question with one evidence line and numbered options. An
/// option's number or letter answers with that option, a lone number or letter naming no option asks
/// again, other text is the answer; "s" skips, "d" dismisses, "q" or the end of input quits, an empty
/// line asks again.
pub fn ask_loop(store: &Path, input: &mut dyn std::io::BufRead, out: &mut dyn Write, author: &str, now: &dyn Fn() -> i64) -> std::io::Result<Asked> {
    let open = open(store);
    let mut done: Asked = (0, 0, 0);
    'questions: for (i, e) in open.iter().enumerate() {
        writeln!(out, "\n[{}/{}] {}  {}", i + 1, open.len(), repo(e), e.question)?;
        if let Some(ev) = e.evidence.first() {
            writeln!(out, "  evidence: {ev}")?;
        }
        for (k, o) in e.options.iter().enumerate() {
            writeln!(out, "  {}) {o}{}", k + 1, if *o == e.default { " (default)" } else { "" })?;
        }
        if !e.options.contains(&e.default) {
            writeln!(out, "  default: {}", e.default)?;
        }
        loop {
            write!(out, "answer (option number, or your own words; s skip, d dismiss, q quit): ")?;
            out.flush()?;
            let mut line = String::new();
            if input.read_line(&mut line)? == 0 {
                break 'questions;
            }
            let said = match line.trim() {
                "" => continue,
                "q" => break 'questions,
                "s" => skip(store, &e.id, "cli", now()).map(|_| {
                    done.1 += 1;
                    "skipped for 3 days".to_string()
                }),
                "d" => dismiss(store, &e.id, "dismissed in borg queen ask", now()).map(|_| {
                    done.2 += 1;
                    "dismissed".to_string()
                }),
                t => match option_answer(e, t) {
                    Err(err) => {
                        writeln!(out, "  {err}")?;
                        continue;
                    }
                    Ok(a) => answer(store, &e.id, &a, author, now()).map(|x| {
                        done.0 += 1;
                        format!("answered: {}", x.answer.unwrap_or_default())
                    }),
                },
            };
            match said {
                Ok(m) => writeln!(out, "  {m}")?,
                Err(err) => writeln!(out, "  borg queen: {err}")?,
            }
            break;
        }
    }
    writeln!(out, "\n{} answered, {} skipped, {} dismissed", done.0, done.1, done.2)?;
    Ok(done)
}

/// The terminal's width in columns: `COLUMNS`, else the terminal of stdout, else none (no cut).
fn width() -> usize {
    if let Some(w) = std::env::var("COLUMNS").ok().and_then(|c| c.trim().parse::<usize>().ok()).filter(|w| *w > 20) {
        return w;
    }
    #[cfg(unix)]
    {
        // SAFETY: TIOCGWINSZ only writes a winsize into the zeroed struct we own.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 20 {
            return ws.ws_col as usize;
        }
    }
    usize::MAX
}

/// The store `borg queen` works on: the collective named `named`, else the one `cwd` routes to. None:
/// `cwd` routes off. Err: (message, exit code).
fn locate(cwd: &str, named: Option<&str>) -> Result<Option<PathBuf>, (String, i32)> {
    let cfg = collective::load().map_err(|e| (format!("invalid collectives config ({e}); everything is off"), 1))?;
    let base = learn::home();
    let Some(n) = named else {
        return Ok(match collective::route(cfg.as_ref(), cwd, &base) {
            collective::Route::Collective { home, .. } => Some(home),
            collective::Route::Off => None,
        });
    };
    match &cfg {
        None if n == "default" => Ok(Some(base)),
        None => Err((format!("no collective named {n:?}: there is no collectives config"), 2)),
        Some(c) => {
            c.collectives.iter().find(|e| e.name == n).map(|e| Some(collective::store(&base, &e.name))).ok_or_else(|| (format!("no collective named {n:?}"), 2))
        }
    }
}

fn print_json<T: Serialize>(v: &T) -> i32 {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_else(|_| "null".into()));
    0
}

fn fail(msg: impl std::fmt::Display) -> i32 {
    eprintln!("borg queen: {msg}");
    1
}

/// `borg queen`: list, show, ask, answer, dismiss, skip, next, dedupe, notify (see `usage`). `--dir DIR`
/// (default: the current directory) or `--collective NAME` picks the collective.
pub fn main(args: &[&str]) -> i32 {
    use std::io::IsTerminal;
    let mut rest: Vec<&str> = Vec::new();
    let (mut dir, mut named, mut only, mut json, mut apply) = (None, None, None, false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--dir" | "--collective" | "--repo" => {
                let Some(v) = it.next().map(|v| v.to_string()) else { return usage() };
                match *a {
                    "--dir" => dir = Some(v),
                    "--collective" => named = Some(v),
                    _ => only = Some(v),
                }
            }
            "--json" => json = true,
            "--apply" => apply = true,
            a => rest.push(a),
        }
    }
    let cwd = dir.clone().unwrap_or_else(|| std::env::current_dir().unwrap_or_default().display().to_string());
    let store = match locate(&cwd, named.as_deref()) {
        Ok(Some(s)) => s,
        Ok(None) => {
            match json {
                true => println!("{}", if rest.first() == Some(&"next") { "null" } else { "[]" }),
                false => println!("off: {cwd} belongs to no collective; the queen is never asked here"),
            }
            return 0;
        }
        Err((msg, code)) => {
            eprintln!("borg queen: {msg}");
            return code;
        }
    };
    let scope = match (&named, &dir) {
        (Some(n), _) => format!(" --collective {}", sh_quote(n)),
        (None, Some(d)) => format!(" --dir {}", sh_quote(d)),
        _ => String::new(),
    };
    let now = learn::now_ms();
    let all = entries(&store);
    let open: Vec<Entry> = all.iter().filter(|e| e.is_open()).cloned().collect();
    let find = |key: &str| resolve(&open, &all, key).cloned().ok_or_else(|| format!("no queen question {key} in this collective (see borg queen)"));
    match rest.as_slice() {
        [] if json => print_json(&questions(&open, only.as_deref(), &scope)),
        [] => {
            if !open.iter().any(|e| only.as_deref().is_none_or(|r| same_repo(r, &repo(e)))) {
                println!("no open queen questions ({})", store.display());
                return 0;
            }
            print!(
                "{}",
                render_list(&open, only.as_deref(), width(), std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()))
            );
            0
        }
        ["next"] => {
            let q = next(&open, &skips(&store), only.as_deref(), now, &scope);
            match (json, q) {
                (true, q) => print_json(&q),
                (false, None) => {
                    println!("no question to ask now");
                    0
                }
                (false, Some(q)) => {
                    println!("{:>3}  {}", q.n, q.question);
                    0
                }
            }
        }
        ["show", key] => match find(key) {
            Ok(e) => {
                let n = open.iter().position(|o| o.id == e.id).map(|i| i + 1);
                match json {
                    true => print_json(&n.map_or_else(|| serde_json::to_value(&e).unwrap_or_default(), |n| json!(Question::of(&e, n, &scope)))),
                    false => {
                        print!("{}", render_show(&e, n));
                        0
                    }
                }
            }
            Err(err) => fail(err),
        },
        ["ask"] => {
            if !std::io::stdin().is_terminal() {
                eprintln!("borg queen ask needs a terminal; otherwise: borg queen answer <n> <text|a|b>");
                return 2;
            }
            let author = learn::engineer(&cwd);
            match ask_loop(&store, &mut std::io::stdin().lock(), &mut std::io::stdout(), &author, &learn::now_ms) {
                Ok(_) => 0,
                Err(err) => fail(err),
            }
        }
        ["answer", key, text @ ..] if !text.is_empty() => {
            let author = learn::engineer(&cwd);
            match find(key).and_then(|e| answer(&store, &e.id, &option_answer(&e, &text.join(" "))?, &author, now)) {
                Ok(e) => {
                    println!("{} answered; lesson queen-{} added to {}", e.id, e.decision_key, store.join("lessons.jsonl").display());
                    0
                }
                Err(err) => fail(err),
            }
        }
        ["dismiss", key, reason @ ..] => match find(key).and_then(|e| dismiss(&store, &e.id, &reason.join(" "), now)) {
            Ok(e) => {
                println!("{} dismissed ({}); no lesson written", e.id, e.dismissed.unwrap_or_default());
                0
            }
            Err(err) => fail(err),
        },
        ["skip", key] => match find(key).and_then(|e| skip(&store, &e.id, "cli", now)) {
            Ok(e) => {
                println!("{} skipped; not delivered again for 3 days", e.id);
                0
            }
            Err(err) => fail(err),
        },
        ["dedupe"] => match dedupe(&store, apply, now) {
            Ok(found) => {
                for (id, of) in &found {
                    let q = all.iter().find(|e| e.id == *id).map(|e| clip(gist(e), 100)).unwrap_or_default();
                    println!("{} {id} (duplicate of {of}): {q}", if apply { "dismissed" } else { "would dismiss" });
                }
                match (found.is_empty(), apply) {
                    (true, _) => println!("no duplicate open questions"),
                    (false, false) => println!("{} duplicates; dry run, --apply dismisses them", found.len()),
                    (false, true) => println!("{} duplicates dismissed", found.len()),
                }
                0
            }
            Err(err) => fail(err),
        },
        ["notify"] => {
            match crate::notify::message(&open) {
                None => println!("no open queen questions"),
                Some(_) if !crate::notify::enabled() => println!("{} open; no notification (BORG_NOTIFY=off, no display or no notify-send)", open.len()),
                Some(_) => {
                    crate::notify::queued(&open);
                    println!("notified: {} open", open.len());
                }
            }
            0
        }
        _ => usage(),
    }
}

fn usage() -> i32 {
    eprintln!(
        "usage: borg queen [--json] [--repo NAME]            open questions, numbered
       borg queen show <n|id>                     one question in full
       borg queen ask                             answer them one by one (a terminal)
       borg queen answer <n|id> <text|a|b|1|2>    answer one (an option letter or number picks it)
       borg queen dismiss <n|id> [reason]         drop one: no lesson, never delivered
       borg queen skip <n|id>                     not delivered again for 3 days
       borg queen next --json [--repo NAME]       the one question a drone should ask now (or null)
       borg queen dedupe [--apply]                dismiss later near-duplicates (dry run by default)
       borg queen notify                          one desktop notification of the open questions
  each takes --dir DIR (default: the current directory) or --collective NAME"
    );
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-queen-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const PUSH: &str = "Plan: rewrite the commit messages, then force-push the rewritten history to origin main.";
    const TASK: &str = "Clean up the commit messages on this branch.";

    fn cp<'a>(store: &'a Path, session: &'a str, plan: &'a str, inputs: &'a str) -> Checkpoint<'a> {
        Checkpoint { store, session, cwd: "/w/app", engineer: "eng@x", plan, inputs, selected: &[], now: 1 }
    }

    fn lesson_ch(id: &str, situation: &str, consequence: &str) -> Challenge {
        Challenge {
            id: id.into(),
            origin_ms: 1,
            question: format!("When you {situation}, what happens?"),
            situations: vec![situation.into()],
            consequence: consequence.into(),
            ..Default::default()
        }
    }

    #[test]
    fn criterion_1_only_high_consequence_moves_are_asked() {
        let store = tmp("c1");
        let low = "Plan: rename the local helper and add a unit test for the parser.";
        assert!(consider(&cp(&store, "s1", low, TASK)).unwrap().is_empty());
        let asked = consider(&cp(&store, "s1", PUSH, TASK)).unwrap();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].kind, Kind::Irreversible);
        // A move the plan rules out is not one it makes.
        assert!(classify("We will not drop the users table; we add a nullable column instead.", "").iter().all(|d| d.kind != Kind::Irreversible));
        let kinds = |p: &str| classify(p, "").into_iter().map(|d| d.kind).collect::<Vec<_>>();
        assert_eq!(kinds("Then npm publish the package."), [Kind::ExternalMutation]);
        assert_eq!(kinds("This is a breaking change to the public API."), [Kind::PublicApi]);
        assert_eq!(kinds("Patch the shared library in the other repo."), [Kind::CrossRepo]);
        assert_eq!(kinds("Next, deploy to production."), [Kind::ExternalMutation]);
        assert_eq!(kinds("I'll write to the prod database to backfill the column."), [Kind::ExternalMutation]);
        assert_eq!(kinds("Rename the public API endpoint /v1/users."), [Kind::PublicApi]);
        assert_eq!(kinds("Bump the major version and publish the crate."), [Kind::PublicApi]);
        assert_eq!(kinds("We keep nothing of it: drop the users table."), [Kind::Irreversible]);
    }

    #[test]
    fn criterion_1_mentions_completed_negated_and_suggested_moves_are_not_moves() {
        for p in [
            "Done. The public API is unchanged.",
            "This change doesn't touch the production database.",
            "This isn't a breaking change.",
            "I didn't force-push anything.",
            "We don\u{2019}t drop the users table.",
            "I deployed to staging and verified it.",
            "I read rows from the prod database to check counts.",
            "Updated the protobuf comments",
            "Bumped the major version in the changelog notes",
            "Next step would be to git push.",
            "You can now force-push to main if you want.",
            "No changes to the openapi spec.",
            "I force-pushed the branch earlier.",
            "Talked to the other team about the shared config.",
        ] {
            assert_eq!(classify(p, ""), vec![], "{p}");
        }
    }

    /// Rules tuned on the development half of replayed history (reports/queen-replay.md).
    #[test]
    fn criterion_1_handed_over_asked_described_and_routine_moves_are_not_asked() {
        for p in [
            "You:** force-push the corrected tip to `main` and tell me.",
            "Should I run git push origin main?",
            "The force-push to `main` stays with you and the teams.",
            "Run it when you're ready: git push origin main",
            "That job runs `terraform apply -auto-approve` on 3_github.",
            "To push, the repo's convention is `git push origin main`.",
            "Then git push origin feature/lumen-120.",
            "- the 30-day purge, backups included: crypto-shredding",
            "Truncate the product title with an ellipsis.",
            "- **a breaking change:** `TerminationReason` gets a new value.",
            "Added a changelog entry that flags the breaking change.",
        ] {
            assert_eq!(classify(p, ""), vec![], "{p}");
        }
        let kinds = |p: &str| classify(p, "").into_iter().map(|d| d.kind).collect::<Vec<_>>();
        assert_eq!(kinds("Then I'll git push origin main."), [Kind::ExternalMutation]);
        assert_eq!(kinds("Then git push -u origin main."), [Kind::ExternalMutation]);
        assert_eq!(kinds("Truncate the audit table before the import."), [Kind::Irreversible]);
        assert_eq!(kinds("Purge the old backups."), [Kind::Irreversible]);
        assert_eq!(kinds("Adding variants to the enum is a major-breaking change."), [Kind::PublicApi]);
    }

    /// Review of the tuned rules: a description excludes only the move inside the described clause,
    /// and a push targets `main` only when its refspec ends there.
    #[test]
    fn criterion_1_a_trailing_clause_or_a_convention_mention_does_not_hide_a_move() {
        let kinds = |p: &str| classify(p, "").into_iter().map(|d| d.kind).collect::<Vec<_>>();
        assert_eq!(kinds("I'm force-pushing main now, which triggers the deploy."), [Kind::Irreversible]);
        assert_eq!(kinds("Next I run terraform apply on prod, it runs about 4 minutes."), [Kind::ExternalMutation]);
        assert_eq!(kinds("I will drop the users table, per the team convention."), [Kind::Irreversible]);
        assert_eq!(kinds("The deploy step runs `kubectl apply -f prod.yaml` nightly."), []);
        assert_eq!(kinds("Then git push origin HEAD:main."), [Kind::ExternalMutation]);
        assert_eq!(kinds("Then `git push origin main:main`"), [Kind::ExternalMutation]);
        assert_eq!(kinds("Then git push origin main-fix."), []);
        assert_eq!(kinds("Then git push origin feature/main."), []);
        assert_eq!(kinds("Then git push origin production"), [Kind::ExternalMutation]);
    }

    #[test]
    fn one_message_asks_one_move_once() {
        let store = tmp("onemove");
        // The command and a sentence describing it: two sentences, one push.
        let plan = "Next:\n`git push -u origin main`\nSo we push to the remote `origin` and its branch `main`, no force.";
        assert_eq!(classify(plan, "app").len(), 2, "{:?}", classify(plan, "app"));
        let asked = consider(&cp(&store, "s1", plan, TASK)).unwrap();
        assert_eq!(asked.len(), 1, "{asked:?}");
        // Two different moves in one plan are still both asked.
        let store = tmp("twomoves");
        let asked = consider(&cp(&store, "s1", "Drop the audit table.\nThen git push origin main.", TASK)).unwrap();
        assert_eq!(asked.len(), 2, "{asked:?}");
    }

    #[test]
    fn criterion_3_the_inputs_must_state_the_same_move_not_just_its_kind() {
        let d = classify("Then force-push the rewritten history to origin main.", "app").remove(0);
        // A synthetic prompt (the held-out half's wording must not pin a tuned rule).
        assert!(stated(&d, "go ahead and force-push it once the messages read well"));
        assert!(!stated(&d, "coordinate a history wipe among the teams"), "another irreversible move");
        assert!(!stated(&d, "wipe the build cache first"));
    }

    #[test]
    fn criterion_2_a_resolving_lesson_or_agreeing_conscience_lessons_stop_the_queen() {
        let store = tmp("c2");
        let agree = [lesson_ch("x", "force-push to main", "never force-push main")];
        let sel: Vec<&Challenge> = agree.iter().collect();
        let mut c = cp(&store, "s1", PUSH, TASK);
        c.selected = &sel;
        assert!(consider(&c).unwrap().is_empty(), "the conscience asked one lesson: L1 answered");
        let key = classify(PUSH, "app").remove(0).key;
        let line = |id: &str, cons: &str| format!("{}\n", json!({"id": id, "origin_ms": 1, "question": "q", "consequence": cons, "decision_key": key}));
        std::fs::write(store.join("lessons.jsonl"), line("a", "never force-push main")).unwrap();
        assert!(consider(&cp(&store, "s2", PUSH, TASK)).unwrap().is_empty(), "one lesson resolves it");
        // Two engineers' lessons disagree: the collective cannot resolve it.
        // (Same key: two team peers each answered it and the mesh synced both lessons.)
        std::fs::write(store.join("lessons.jsonl"), line("a", "never force-push main") + &line("b", "force-push is fine on main")).unwrap();
        let asked = consider(&cp(&store, "s3", PUSH, TASK)).unwrap();
        assert_eq!(asked[0].disagreeing, ["a", "b"]);
        // A lesson superseding the other agrees with itself again.
        let lessons =
            vec![json!({"id": "a", "consequence": "x", "decision_key": "k"}), json!({"id": "b", "consequence": "y", "decision_key": "k", "supersedes": ["a"]})];
        assert_eq!(known(&lessons, "k"), Known::Resolved);
    }

    #[test]
    fn criterion_2_contradicting_lessons_the_conscience_selected_go_to_the_queen() {
        let store = tmp("c2conflict");
        let ls = [
            lesson_ch("a", "force-push the rewritten history to origin main", "the team loses commits; never do it"),
            lesson_ch("b", "force-push rewritten history to main", "fine after a squash; CI rebuilds cleanly"),
            lesson_ch("c", "rename a parser helper", "callers break"),
        ];
        let sel: Vec<&Challenge> = ls.iter().collect();
        assert_eq!(conflicts(&sel), ["a", "b"]);
        // Same situation, same consequence in other words: no contradiction.
        let same = [lesson_ch("a", "force-push to main", "never force-push main"), lesson_ch("b", "force-push to main", "never force-push to main")];
        assert!(conflicts(&same.iter().collect::<Vec<_>>()).is_empty());
        let mut c = cp(&store, "s1", PUSH, TASK);
        c.selected = &sel;
        let asked = consider(&c).unwrap();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].disagreeing, ["a", "b"]);
        assert!(asked[0].evidence.iter().any(|e| e.contains("lessons a, b disagree")), "{:?}", asked[0].evidence);
        // The contradiction must be about the plan's move: unrelated clashing lessons are not a
        // disagreement about it, and do not answer it either (no lesson is about this move).
        let store = tmp("c2unrelated");
        let other = [lesson_ch("p", "cache the parser tables", "stale reads"), lesson_ch("q", "cache parser tables", "faster startup, no issue")];
        let sel: Vec<&Challenge> = other.iter().collect();
        assert_eq!(conflicts(&sel), ["p", "q"]);
        let mut c = cp(&store, "s1", PUSH, TASK);
        c.selected = &sel;
        let asked = consider(&c).unwrap();
        assert_eq!(asked.len(), 1);
        assert!(asked[0].disagreeing.is_empty());
        assert!(asked[0].evidence.iter().any(|e| e.contains("no lesson of this collective is about this move")), "{:?}", asked[0].evidence);
        // An unrelated lesson the conscience asked does not settle the move either.
        let store = tmp("c2unrelated1");
        let one = [lesson_ch("p", "cache the parser tables", "stale reads")];
        let sel: Vec<&Challenge> = one.iter().collect();
        let mut c = cp(&store, "s1", PUSH, TASK);
        c.selected = &sel;
        assert_eq!(consider(&c).unwrap().len(), 1);
    }

    #[test]
    fn criterion_3_a_decision_the_inputs_state_is_not_asked() {
        let store = tmp("c3");
        let inputs = "Squash the branch and force-push it to origin main, I approve.";
        assert!(consider(&cp(&store, "s1", PUSH, inputs)).unwrap().is_empty());
        // Same words, other phrasing: the inputs state it too.
        let d =
            Decision { kind: Kind::Irreversible, sentence: "drop the legacy orders table".into(), what: "drop legacy orders table".into(), key: "k".into() };
        assert!(stated(&d, "please remove the legacy orders table"));
        assert!(!stated(&d, "speed up the checkout page"));
    }

    #[test]
    fn criterion_4_budget_is_one_message_per_engineer_per_five_sessions() {
        let store = tmp("c4");
        let plans = ["Then npm publish the package.", "Drop the audit table.", "Run terraform apply on prod.", "This is a breaking change to the public API."];
        assert_eq!(consider(&cp(&store, "s0", plans[0], TASK)).unwrap().len(), 1);
        // Same session again, and the next four sessions: over budget.
        for s in ["s0", "s1", "s2", "s3", "s4"] {
            assert!(consider(&cp(&store, s, plans[1], TASK)).unwrap().is_empty(), "{s}");
        }
        // Another engineer has their own budget.
        let mut other = cp(&store, "t0", plans[2], TASK);
        other.engineer = "other@x";
        assert_eq!(consider(&other).unwrap().len(), 1);
        // The fifth session after the asking one may ask again; a revisited old session counts once.
        assert!(consider(&cp(&store, "s2", plans[1], TASK)).unwrap().is_empty());
        assert_eq!(consider(&cp(&store, "s5", plans[1], TASK)).unwrap().len(), 1);
        assert!(consider(&cp(&store, "s6", plans[3], TASK)).unwrap().is_empty());
    }

    #[test]
    fn batched_at_most_two_questions_in_one_message() {
        let store = tmp("batch");
        let plan = "Drop the audit table.\nThen npm publish the package.\nPatch the shared library in the other repo.";
        let asked = consider(&cp(&store, "s1", plan, TASK)).unwrap();
        assert_eq!(asked.len(), MAX_QUESTIONS);
        assert_eq!(open(&store).len(), 2);
        let reason = render(&asked);
        assert!(reason.contains("eng@x") && reason.contains("proceed with default: proceed as planned"), "{reason}");
        assert!(reason.contains("hold: prepare it"), "external mutations default to holding: {reason}");
    }

    #[test]
    fn the_same_decision_is_never_asked_twice_even_after_its_answer() {
        let store = tmp("twice");
        let asked = consider(&cp(&store, "s0", PUSH, TASK)).unwrap();
        assert_eq!(asked.len(), 1);
        for i in 1..=6 {
            let s = format!("s{i}");
            assert!(consider(&cp(&store, &s, PUSH, TASK)).unwrap().is_empty(), "asked again in {s}");
        }
        answer(&store, &asked[0].id, "Never force-push main; open a new branch.", "lead@x", 9).unwrap();
        // The answer's lesson would be filtered by the conscience first; even without it, the key is spent.
        std::fs::remove_file(store.join("lessons.jsonl")).unwrap();
        assert!(consider(&cp(&store, "s9", PUSH, TASK)).unwrap().is_empty());
        assert!(answer(&store, &asked[0].id, "again", "lead@x", 10).is_err());
        assert!(answer(&store, "qnope", "x", "lead@x", 10).is_err());
    }

    #[test]
    fn an_answer_becomes_a_lesson_the_conscience_recalls() {
        let store = tmp("answer");
        let asked = consider(&cp(&store, "s0", PUSH, TASK)).unwrap();
        let e = answer(&store, &asked[0].id, "Never force-push main; open a new branch.", "lead@x", 9).unwrap();
        assert_eq!(e.answer.as_deref(), Some("Never force-push main; open a new branch."));
        assert!(open(&store).is_empty());
        let v: Value = serde_json::from_str(std::fs::read_to_string(store.join("lessons.jsonl")).unwrap().trim()).unwrap();
        assert_eq!((v["author"].as_str(), v["decision_key"].as_str()), (Some("lead@x"), Some(asked[0].decision_key.as_str())));
        assert_eq!(v["consequence"], "Never force-push main; open a new branch.");
        // `borg learn` still reads it as a lesson.
        assert_eq!(learn::lessons(&store.join("lessons.jsonl")).len(), 1);
        // A later drone with the same plan: the conscience asks it, so the queen stays quiet.
        let cs = conscience::load(&store.join("lessons.jsonl"), None);
        let later = format!("{TASK}\n{PUSH}");
        let sel = conscience::select(&cs, &later, 5);
        assert_eq!(sel.len(), 1);
        assert!(conscience::render(&sel).contains("Never force-push main"));
        assert_eq!(known(&lesson_lines(&store.join("lessons.jsonl")), &asked[0].decision_key), Known::Resolved);
    }

    #[test]
    fn an_answer_supersedes_only_the_side_it_overrules_and_takes_their_area() {
        let store = tmp("sides");
        let key = classify(PUSH, "app").remove(0).key;
        let line = |id: &str, cons: &str, area: &str, eng: &str| {
            format!("{}\n", json!({"id": id, "origin_ms": 1, "question": "q", "consequence": cons, "decision_key": key, "area": area, "engineers": [eng]}))
        };
        std::fs::write(
            store.join("lessons.jsonl"),
            line("a", "Never force-push main", "protocol-correctness", "ann@x") + &line("b", "force-push is fine on main", "protocol-correctness", "bob@x"),
        )
        .unwrap();
        let asked = consider(&cp(&store, "s1", PUSH, TASK)).unwrap();
        assert_eq!(asked[0].disagreeing, ["a", "b"]);
        answer(&store, &asked[0].id, "  never force-push main ", "lead@x", 9).unwrap();
        let q = lesson_lines(&store.join("lessons.jsonl")).into_iter().find(|l| l["source"] == "queen").unwrap();
        assert_eq!(q["supersedes"], json!(["b"]), "the side the answer confirms is not superseded");
        assert_eq!(q["area"], "protocol-correctness");
        // The trust signal follows the answer: bob (overruled) loses, ann (confirmed) does not, and
        // the answer settles it at once (no followed outcome needed).
        let t = crate::trust::of_store(&store);
        assert_eq!((t.of("bob@x", "protocol-correctness"), t.of("ann@x", "protocol-correctness")), (0.25, 0.5));
        // Without the disagreeing lessons in the store (an empty question): nothing confirmed, all superseded, area other.
        let e = Entry { disagreeing: vec!["x".into()], ..asked[0].clone() };
        let v = lesson(&e, "fine", "lead@x", 1, &[]);
        assert_eq!((v["supersedes"].clone(), v["area"].clone()), (json!(["x"]), json!("other")));
    }

    #[test]
    fn decision_key_is_the_move_not_the_wording() {
        let key = |p: &str| classify(p, "app").remove(0).key;
        let a = key("force-push the rewritten history to origin main");
        assert_eq!(a, key("Plan: force-push the rewritten branch history to origin/main, then open a PR."));
        assert_eq!(a, key("Squash 3 commits in src/lib.rs, then force-push to main"), "kind and key do not depend on regex order");
        assert_eq!(key("Drop the AUDIT table."), key("then drop the audit table, it is unused"));
        assert_ne!(key("drop the audit table"), key("drop the users table"));
        assert_ne!(a, classify("force-push to main", "other").remove(0).key, "the repo is part of the key");
    }

    #[test]
    fn a_reworded_answered_decision_is_not_asked_again() {
        let store = tmp("reworded");
        let asked = consider(&cp(&store, "s0", PUSH, TASK)).unwrap();
        answer(&store, &asked[0].id, "Never force-push main; open a new branch.", "lead@x", 9).unwrap();
        // Another target ref: another key, but the same decision in other words.
        let reworded = "Plan: rewrite those commit messages, then force-push the rewritten history to origin release";
        assert_ne!(classify(reworded, "app")[0].key, asked[0].decision_key);
        for i in 1..=6 {
            assert!(consider(&cp(&store, &format!("s{i}"), reworded, TASK)).unwrap().is_empty(), "asked again in s{i}");
        }
        // The queue alone was not what stopped it: the queen lesson catches the rewording too.
        std::fs::remove_file(queue_path(&store)).unwrap();
        assert!(consider(&cp(&store, "s7", reworded, TASK)).unwrap().is_empty());
        // And the budget is open: an unrelated decision is asked.
        assert_eq!(consider(&cp(&store, "s8", "Then npm publish the package.", TASK)).unwrap().len(), 1);
    }

    #[test]
    fn fails_open_on_an_unwritable_store() {
        let root = tmp("failopen");
        let store = root.join("file");
        std::fs::write(&store, "not a dir").unwrap();
        assert!(consider(&cp(&store, "s1", PUSH, TASK)).is_err());
        // Corrupt lines are skipped, not fatal.
        let ok = tmp("corrupt");
        std::fs::create_dir_all(dir(&ok)).unwrap();
        std::fs::write(queue_path(&ok), "{broken\n").unwrap();
        std::fs::write(sessions_path(&ok), "[]\n").unwrap();
        assert_eq!(consider(&cp(&ok, "s1", PUSH, TASK)).unwrap().len(), 1);
    }

    fn open_q(id: &str, cwd: &str, question: &str, options: &[&str]) -> Entry {
        Entry {
            id: id.into(),
            decision_key: format!("k-{id}"),
            kind: Kind::OpenDecision,
            question: question.into(),
            options: options.iter().map(|o| o.to_string()).collect(),
            default: options.first().map_or("none stated".into(), |o| o.to_string()),
            evidence: vec![format!("docs/{id}.md:3: still open")],
            session: "import".into(),
            cwd: cwd.into(),
            repo: String::new(),
            engineer: "eng@x".into(),
            ms: 1,
            answer: None,
            answered_by: None,
            answered_ms: None,
            disagreeing: vec![],
            dismissed: None,
            dismissed_ms: None,
        }
    }

    const RETRY: &str = "Which retry budget applies to the payment webhook consumer?";
    const AUTH: &str = "Do service tokens rotate monthly or quarterly in the auth gateway?";
    const CACHE: &str = "Should the catalog cache be invalidated on every price update?";

    /// app: RETRY, lib: AUTH, app: CACHE, in that asking order.
    fn queue(name: &str) -> PathBuf {
        let store = tmp(name);
        assert!(seed(&store, &open_q("qa", "/w/app", RETRY, &["three retries", "no retries, dead-letter at once"])).unwrap());
        assert!(seed(&store, &open_q("qb", "/w/lib", AUTH, &["monthly", "quarterly"])).unwrap());
        assert!(seed(&store, &open_q("qc", "/w/app", CACHE, &[])).unwrap());
        store
    }

    #[test]
    fn the_listing_numbers_open_questions_in_asking_order_grouped_by_repo() {
        let store = queue("list");
        let open = open(&store);
        let out = render_list(&open, None, usize::MAX, false);
        assert_eq!(
            out,
            format!(
                "app (2)\n  1  {RETRY}\n     a) three retries (default) · b) no retries, dead-letter at once\n  3  {CACHE}\n     default: none stated\n\n\
                 lib (1)\n  2  {AUTH}\n     a) monthly (default) · b) quarterly\n\n{HINT}\n"
            )
        );
        // One repo keeps the whole list's numbers.
        let lib = render_list(&open, Some("LIB"), usize::MAX, false);
        assert!(lib.starts_with(&format!("lib (1)\n  2  {AUTH}\n")) && !lib.contains(RETRY), "{lib}");
        // Cut to the terminal's width; colors only when asked.
        let narrow = render_list(&open, None, 40, false);
        assert!(narrow.lines().filter(|l| l.starts_with("  ")).all(|l| l.chars().count() <= 40), "{narrow}");
        assert!(narrow.contains("  1  Which retry budget applies to the…\n"), "{narrow}");
        assert!(!out.contains('\x1b') && render_list(&open, None, 80, true).contains("\x1b[1;36m  1\x1b[0m"));
        // Answered and dismissed questions leave the listing; the others keep their order.
        dismiss(&store, "qa", "", 2).unwrap();
        assert_eq!(open_ids(&store), ["qb", "qc"]);
    }

    fn open_ids(store: &Path) -> Vec<String> {
        open(store).into_iter().map(|e| e.id).collect()
    }

    #[test]
    fn a_number_or_an_id_resolves_and_an_option_letter_or_number_is_its_text() {
        let store = queue("resolve");
        answer(&store, "qb", "monthly", "lead@x", 2).unwrap();
        let (all, open) = (entries(&store), open(&store));
        let id = |k: &str| resolve(&open, &all, k).map(|e| e.id.clone());
        assert_eq!((id("1"), id(" 2 "), id("0"), id("3")), (Some("qa".into()), Some("qc".into()), None, None));
        assert_eq!((id("qb"), id("qnope")), (Some("qb".into()), None), "an id also finds an answered question");
        let e = &open[0];
        for (typed, meant) in
            [("a", "three retries"), ("B", "no retries, dead-letter at once"), ("2", "no retries, dead-letter at once"), ("1", "three retries")]
        {
            assert_eq!(option_answer(e, typed).as_deref(), Ok(meant), "{typed}");
        }
        for typo in ["c", "3", "0", " Z "] {
            assert!(option_answer(e, typo).unwrap_err().contains("no option"), "{typo}");
        }
        for free in ["c d", "33 retries", "five retries, then dead-letter"] {
            assert_eq!(option_answer(e, free).as_deref(), Ok(free));
        }
        // Without options, a lone number is an answer ("how many retries?": 3).
        let bare = open_q("qn", "/w/app", "How many retries?", &[]);
        assert_eq!(option_answer(&bare, "3").as_deref(), Ok("3"));
    }

    #[test]
    fn dismiss_closes_without_a_lesson_and_skip_holds_delivery_for_three_days() {
        let store = queue("dismiss");
        let e = dismiss(&store, "qa", "  not ours to decide ", 5).unwrap();
        assert_eq!((e.dismissed.as_deref(), e.dismissed_ms), (Some("not ours to decide"), Some(5)));
        assert_eq!(dismiss(&store, "qc", "", 5).unwrap().dismissed.as_deref(), Some("dismissed"));
        assert_eq!(open_ids(&store), ["qb"]);
        assert!(!store.join("lessons.jsonl").exists(), "no lesson");
        assert!(answer(&store, "qa", "x", "lead@x", 6).unwrap_err().contains("dismissed"));
        assert!(dismiss(&store, "qa", "", 6).is_err() && skip(&store, "qa", "s1", 6).is_err());
        // A dismissed question is never delivered.
        assert_eq!(deliver(&store, "s1", || vec!["app".into()], "", 7), None);

        skip(&store, "qb", "cli", 100).unwrap();
        assert_eq!(open_ids(&store), ["qb"], "a skipped question stays open");
        let sk = skips(&store);
        assert_eq!(sk, [Mark { id: "qb".into(), session: "cli".into(), ms: 100 }]);
        assert!(skipped(&sk, "qb", 100 + SKIP_MS - 1) && !skipped(&sk, "qb", 100 + SKIP_MS) && !skipped(&sk, "qa", 100));
        assert_eq!(deliver(&store, "s2", || vec!["lib".into()], "", 100 + SKIP_MS - 1), None);
        assert_eq!(deliver(&store, "s3", || vec!["lib".into()], "", 100 + SKIP_MS).map(|q| q.id), Some("qb".into()));
    }

    #[test]
    fn near_duplicates_of_a_repo_are_queued_once_and_dedupe_dismisses_old_ones() {
        let store = queue("dedupe");
        let reworded = "Which retry budget applies to the payment webhook consumer now?";
        assert!(jaccard(RETRY, reworded) >= DUPLICATE);
        assert!(!seed(&store, &open_q("qd", "/w/app", reworded, &[])).unwrap(), "a reworded duplicate is not queued");
        assert!(seed(&store, &open_q("qe", "/w/lib", reworded, &[])).unwrap(), "another repo's question is its own");
        answer(&store, "qa", "three retries", "lead@x", 2).unwrap();
        assert!(!seed(&store, &open_q("qf", "/w/app", reworded, &[])).unwrap(), "nor of an answered one");
        // Older queues hold duplicates: dedupe finds the later ones, dry run first.
        append_line(&queue_path(&store), &open_q("qg", "/w/lib", AUTH, &[])).unwrap();
        append_line(&queue_path(&store), &open_q("qh", "/w/lib", &format!("{AUTH} Still?"), &[])).unwrap();
        let want = vec![("qg".to_string(), "qb".to_string()), ("qh".to_string(), "qb".to_string())];
        assert_eq!(dedupe(&store, false, 3).unwrap(), want);
        assert_eq!(open_ids(&store), ["qb", "qc", "qe", "qg", "qh"], "dry run changes nothing");
        assert_eq!(dedupe(&store, true, 3).unwrap(), want);
        assert_eq!(open_ids(&store), ["qb", "qc", "qe"]);
        assert_eq!(entries(&store).iter().find(|e| e.id == "qh").unwrap().dismissed.as_deref(), Some("duplicate of qb"));
        assert!(dedupe(&store, true, 4).unwrap().is_empty());
        // Checkpoint questions compare by their plan sentence, not their shared frame.
        let q = |plan: &str| Entry { question: question(&classify(plan, "app").remove(0)), ..open_q("x", "/w/app", "", &[]) };
        assert!(!duplicate(&q("Drop the audit table."), &q("Drop the users table.")));
        assert!(duplicate(&q("Drop the audit table."), &q("Then drop the audit table.")));
    }

    #[test]
    fn consider_does_not_queue_a_near_duplicate_of_a_queued_question() {
        let store = tmp("considerdup");
        let asked = consider(&cp(&store, "s0", "Then npm publish the package.", TASK)).unwrap();
        assert_eq!(asked.len(), 1);
        // A seeded question in the same words as the next checkpoint's move.
        let e = Entry { question: question(&classify("Drop the audit table.", "app").remove(0)), ..open_q("qx", "/w/app", "", &[]) };
        append_line(&queue_path(&store), &e).unwrap();
        for s in ["s1", "s2", "s3", "s4", "s5"] {
            note_session(&store, "eng@x", s, 2).unwrap();
        }
        assert!(consider(&cp(&store, "s6", "Drop the audit table, it is unused.", TASK)).unwrap().is_empty());
    }

    #[test]
    fn the_json_schema_is_stable_and_its_commands_are_exact() {
        let store = queue("json");
        let qs = questions(&open(&store), None, " --collective 'team'");
        let v = serde_json::to_value(&qs[0]).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["id", "n", "repo", "kind", "question", "options", "default", "evidence", "ms", "answer_cmd", "skip_cmd", "dismiss_cmd"],
            "documented order"
        );
        assert_eq!((v["id"].as_str(), v["n"].as_u64(), v["repo"].as_str(), v["kind"].as_str()), (Some("qa"), Some(1), Some("app"), Some("open_decision")));
        assert_eq!(v["answer_cmd"], "borg queen answer 'qa' '<answer>' --collective 'team'");
        assert_eq!(v["skip_cmd"], "borg queen skip 'qa' --collective 'team'");
        assert_eq!(v["dismiss_cmd"], "borg queen dismiss 'qa' --collective 'team'");
        assert_eq!(serde_json::from_value::<Question>(v).unwrap(), qs[0]);
        assert_eq!(questions(&open(&store), Some("lib"), "").iter().map(|q| (q.id.as_str(), q.n)).collect::<Vec<_>>(), [("qb", 2)]);
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        // `next`: the first open one of the repo, skipped ones left out.
        let open = open(&store);
        assert_eq!(next(&open, &[], None, 1, "").map(|q| q.id), Some("qa".into()));
        let sk = [Mark { id: "qa".into(), session: "cli".into(), ms: 1 }];
        assert_eq!(next(&open, &sk, Some("app"), 2, "").map(|q| q.id), Some("qc".into()));
        assert_eq!(next(&open, &sk, Some("nope"), 2, ""), None);
    }

    #[test]
    fn delivery_is_one_question_per_session_of_the_sessions_own_repos() {
        let store = queue("deliver");
        let app = ["app".to_string()];
        assert_eq!(deliver(&store, "s1", || vec!["other".into()], "", 10), None, "never another repo's question");
        assert!(!visits_path(&store).exists(), "a session without questions of its repos is not counted");
        let q = deliver(&store, "s1", || app.to_vec(), "", 10).unwrap();
        assert_eq!((q.id.as_str(), q.n, q.answer_cmd.as_str()), ("qa", 1, "borg queen answer 'qa' '<answer>'"));
        assert_eq!(deliver(&store, "s1", || app.to_vec(), "", 11), None, "once per session");
        assert_eq!(deliver(&store, "s1", || unreachable!("settled before the work's repos"), "", 11), None);
        assert_eq!(deliver(&store, "s1", || vec!["lib".into()], "", 11), None, "once per session, whatever the repo");
        assert_eq!(deliveries(&store), [Mark { id: "qa".into(), session: "s1".into(), ms: 10 }]);
        // The next session gets the never-delivered one first; then qa waits for 3 more sessions.
        assert_eq!(deliver(&store, "s2", || app.to_vec(), "", 20).map(|q| q.id), Some("qc".into()));
        answer(&store, "qc", "never", "lead@x", 21).unwrap();
        for s in ["s3", "s4"] {
            assert_eq!(deliver(&store, s, || app.to_vec(), "", 30), None, "{s}");
        }
        assert_eq!(deliver(&store, "s5", || app.to_vec(), "", 40).map(|q| q.id), Some("qa".into()), "after 3 more sessions");
        // The work's repo matches by name, case aside, and a touched repo counts.
        assert_eq!(deliver(&store, "s6", || vec!["x".into(), "LIB".into()], "", 50).map(|q| q.id), Some("qb".into()));
        assert!(ask_on(None) && ask_on(Some("on")) && !ask_on(Some("off")) && !ask_on(Some(" OFF ")));
    }

    #[test]
    fn eligibility_counts_sessions_and_the_skip_window_purely() {
        let open = vec![open_q("qa", "/w/app", RETRY, &[])];
        let app = ["app".to_string()];
        let visits: Vec<Visit> = (0..6).map(|i| Visit { session: format!("s{i}"), ms: i * 10 }).collect();
        let delivered = [Mark { id: "qa".into(), session: "s1".into(), ms: 10 }];
        let at = |s: &str, skips: &[Mark], now: i64| eligible(&open, skips, &delivered, &visits, s, &app, now).map(|(i, e)| (i, e.id.clone()));
        assert_eq!(at("s4", &[], 40), None, "3 sessions later is not yet after them");
        assert_eq!(at("s5", &[], 50), Some((0, "qa".into())));
        assert_eq!(at("s1", &[], 50), None, "its own session");
        let sk = [Mark { id: "qa".into(), session: "s4".into(), ms: 45 }];
        assert_eq!(at("s5", &sk, 50), None, "skipped within 3 days");
        assert_eq!(at("s5", &sk, 45 + SKIP_MS), Some((0, "qa".into())));
        // An unknown session: sessions first seen after the delivery count.
        assert_eq!(at("s9", &[], 60), Some((0, "qa".into())));
    }

    #[test]
    fn a_questions_repo_is_its_git_roots_as_the_hook_names_it() {
        let root = tmp("reponame");
        for d in ["work/lib/.git", "work/lib/docs", "work/lib/.claude/worktrees/x/src", "other/docs"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("work/lib/.claude/worktrees/x/.git"), "gitdir: ../../../.git/worktrees/x\n").unwrap();
        let at = |d: &str| open_q("q", &root.join(d).display().to_string(), RETRY, &[]);
        for d in ["work/lib", "work/lib/docs", "work/lib/.claude/worktrees/x/src"] {
            assert_eq!(repo(&at(d)), "lib", "{d}");
        }
        assert_eq!(repo(&at("other/docs")), "docs", "outside git: the directory's own name");
        assert!(!duplicate(&at("work/lib/docs"), &at("other/docs")), "two repos' docs/ folders are two repos");
        assert_eq!(repo(&Entry { repo: "app".into(), ..at("other/docs") }), "app", "the recorded repo wins");
        // A checkpoint in a subdirectory queues its question under the repo.
        let d = decide(&Checkpoint { cwd: &root.join("work/lib/docs").display().to_string(), ..cp(&root, "s1", PUSH, TASK) }, &[], || true, Vec::new);
        assert_eq!(d.iter().map(|e| e.repo.as_str()).collect::<Vec<_>>(), ["lib"]);
    }

    #[test]
    fn the_delivery_text_asks_with_the_dialog_in_claude_and_inline_elsewhere() {
        let store = queue("render");
        let q = Question::of(&open(&store)[0], 1, "");
        let c = render_delivery(&q, "claude");
        assert!(c.contains("AskUserQuestion") && c.contains("not the engineer's instruction") && c.contains(RETRY), "{c}");
        assert!(
            c.contains("borg queen answer 'qa' '<answer>'") && c.contains("borg queen skip 'qa'") && c.contains("Before you work on the engineer's request")
        );
        assert!(
            c.contains("- three retries: three retries\n- no retries: no retries, dead-letter at once\n- Skip for now")
                && c.contains("Default while nobody decides: three retries\n")
                && c.contains("Asked: 1970-01-01, found open in the team's documents; not part of"),
            "the default is its own line, never inside an option's text: {c}"
        );
        let o = render_delivery(&q, "omp");
        assert!(!o.contains("AskUserQuestion") && o.contains("one short paragraph with the options first"), "{o}");
        assert!(o.contains("a) three retries\nb) no retries, dead-letter at once\n") && o.contains("borg queen answer 'qa' '<answer>'"), "{o}");
        assert_eq!(label("Keep the documented interim rule until the board decides."), "Keep the documented interim rule");
        assert_eq!(label("hold: prepare it, but do"), "hold");
        assert_eq!(label("one two three four five six"), "one two three four five");
        assert_eq!(label(": odd"), ": odd");
        // A checkpoint question queued in an earlier session is not the current drone's plan.
        let e = Entry { kind: Kind::ExternalMutation, question: question(&classify("Then push to main.", "app").remove(0)), ..open(&store)[0].clone() };
        let cp = render_delivery(&Question::of(&e, 1, " --dir '/w/app'").run_by("'/opt/borg'"), "claude");
        assert!(cp.contains("Question: An earlier session's plan said: \"") && cp.contains("queued by an earlier session"), "{cp}");
        assert!(
            cp.contains("\n  '/opt/borg' queen answer 'qa' '<answer>' --dir '/w/app'\n") && cp.contains("'/opt/borg' queen skip 'qa' --dir '/w/app'"),
            "{cp}"
        );
        // No options: the default is one.
        let none = Question::of(&open(&store)[2], 3, "");
        assert!(render_delivery(&none, "claude").contains("- Keep the default: none stated\n- Skip for now"));
    }

    #[test]
    fn ask_answers_skips_dismisses_and_quits_from_any_input() {
        let store = queue("ask");
        let mut out = Vec::new();
        let done = ask_loop(&store, &mut std::io::Cursor::new("\nb\ns\nd\n"), &mut out, "lead@x", &|| 9).unwrap();
        assert_eq!(done, (1, 1, 1));
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("[1/3] app  Which retry budget") && text.contains("evidence: docs/qa.md:3") && text.contains("  1) three retries (default)"),
            "{text}"
        );
        assert_eq!(entries(&store)[0].answer.as_deref(), Some("no retries, dead-letter at once"));
        assert_eq!((open_ids(&store), skips(&store).len()), (vec!["qb".to_string()], 1));
        let store = queue("askquit");
        assert_eq!(ask_loop(&store, &mut std::io::Cursor::new("q\n"), &mut Vec::new(), "lead@x", &|| 9).unwrap(), (0, 0, 0));
        // A lone letter or number naming no option is a typo: asked again, never recorded.
        let mut out = Vec::new();
        assert_eq!(ask_loop(&store, &mut std::io::Cursor::new("9\nmy own words\n"), &mut out, "lead@x", &|| 9).unwrap(), (1, 0, 0));
        assert!(String::from_utf8(out).unwrap().contains("no option 9 (a-b or 1-2)"));
        assert_eq!(entries(&store)[0].answer.as_deref(), Some("my own words"));
    }
}
