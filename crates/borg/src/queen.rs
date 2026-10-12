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
    let line = serde_json::to_string(v).map_err(std::io::Error::other)?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")
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

/// Queue `e` (an imported open decision) unless its decision was ever queued in this collective.
/// Returns whether it was added.
pub fn seed(store: &Path, e: &Entry) -> std::io::Result<bool> {
    if entries(store).iter().any(|x| x.decision_key == e.decision_key) {
        return Ok(false);
    }
    append_line(&queue_path(store), e)?;
    Ok(true)
}

/// Questions still waiting for an answer.
pub fn open(store: &Path) -> Vec<Entry> {
    entries(store).into_iter().filter(|e| e.answer.is_none()).collect()
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
/// lessons file is read only when a candidate decision survives them and the budget.
pub fn consider(cp: &Checkpoint) -> std::io::Result<Vec<Entry>> {
    note_session(cp.store, cp.engineer, cp.session, cp.now)?;
    let asked = entries(cp.store);
    let ask = decide(cp, &asked, || within_budget(cp.store, cp.engineer, cp.session), || lesson_lines(&cp.store.join("lessons.jsonl")));
    for e in &ask {
        append_line(&queue_path(cp.store), e)?;
    }
    Ok(ask)
}

/// The queen's decision at one checkpoint, without I/O (`consider` is this plus the store; the
/// replay eval calls it directly): `asked` is every question asked so far, `budget` criterion 4 and
/// `lessons` the collective's lessons as raw JSON, both evaluated only once a candidate survives
/// criteria 1-3. `cp.store` is not read.
pub fn decide(cp: &Checkpoint, asked: &[Entry], budget: impl FnOnce() -> bool, lessons: impl FnOnce() -> Vec<Value>) -> Vec<Entry> {
    let repo = Path::new(cp.cwd).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
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
            engineer: cp.engineer.into(),
            ms: cp.now,
            answer: None,
            answered_by: None,
            answered_ms: None,
            disagreeing,
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
    // The lesson first: a question marked answered without its lesson would be lost.
    let lessons = lesson_lines(&store.join("lessons.jsonl"));
    append_line(&store.join("lessons.jsonl"), &lesson(&e, text, author, now, &lessons)).map_err(|err| err.to_string())?;
    e.answer = Some(text.into());
    e.answered_by = Some(author.into());
    e.answered_ms = Some(now);
    append_line(&queue_path(store), &e).map_err(|err| err.to_string())?;
    Ok(e)
}

/// `borg queen [--dir DIR]` lists this collective's open questions; `borg queen answer <id> "<text>"
/// [--dir DIR]` answers one. DIR (default: the current directory) picks the collective.
pub fn main(args: &[&str]) -> i32 {
    let mut rest: Vec<&str> = Vec::new();
    let mut cwd = std::env::current_dir().unwrap_or_default().display().to_string();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--dir" => match it.next() {
                Some(d) => cwd = d.to_string(),
                None => return usage(),
            },
            a => rest.push(a),
        }
    }
    let store = match collective::load().map(|cfg| collective::route(cfg.as_ref(), &cwd, &learn::home())) {
        Ok(collective::Route::Collective { home, .. }) => home,
        Ok(collective::Route::Off) => {
            println!("off: {cwd} belongs to no collective; the queen is never asked here");
            return 0;
        }
        Err(e) => {
            eprintln!("borg queen: invalid collectives config ({e}); everything is off");
            return 1;
        }
    };
    match rest.as_slice() {
        [] => {
            let open = open(&store);
            if open.is_empty() {
                println!("no open queen questions ({})", store.display());
            }
            for e in &open {
                println!("{}  {}  {}  {}", e.id, crate::install::utc(e.ms), if e.engineer.is_empty() { "?" } else { &e.engineer }, e.cwd);
                println!("  {}", e.question);
                println!("  options: {}", e.options.join(" | "));
                println!("  default: {}", e.default);
                for ev in &e.evidence {
                    println!("  - {ev}");
                }
                println!();
            }
            0
        }
        ["answer", id, text @ ..] if !text.is_empty() => {
            let author = learn::engineer(&cwd);
            match answer(&store, id, &text.join(" "), &author, learn::now_ms()) {
                Ok(e) => {
                    println!("{} answered; lesson queen-{} added to {}", e.id, e.decision_key, store.join("lessons.jsonl").display());
                    0
                }
                Err(e) => {
                    eprintln!("borg queen: {e}");
                    1
                }
            }
        }
        _ => usage(),
    }
}

fn usage() -> i32 {
    eprintln!("usage: borg queen [--dir DIR] | borg queen answer <id> \"<text>\" [--dir DIR]");
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
}
