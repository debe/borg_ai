//! The conscience's live learning loop.
//!
//! **Capture** (hooks, deterministic, 0 tokens): an engineer correction is a prompt that pushes
//! back on what the drone just proposed ("no…", "double check…", "why do you…"), or a rejected
//! tool call / plan whose rejection carries the engineer's words. Each becomes a raw *episode*:
//! the task, the drone's proposal, the correction.
//!
//! **Generalise** (`borg learn`, at session end, one model call per episode): the episode becomes a
//! lesson (question + situations → consequence) or is merged into an existing lesson that
//! expresses the same concern — the old wording is kept in `history` and `support` grows, so the
//! library consolidates instead of piling up near-duplicates. Each lesson lists the engineers
//! (git user.email) whose corrections it was learned from, its area (the kind of concern, for trust per
//! engineer × area, trust.rs) and, when the correction contradicts a lesson it was shown, which
//! lessons it `supersedes`; all from the same model call.
//!
//! **Outcomes** (`borg learn`, 0 tokens): a merge into a lesson the conscience asked earlier in the
//! same session, or a new lesson superseding one it asked, means asking did not prevent the
//! correction; a correction siding with one side of a rendered disagreement corrects the other
//! sides instead; see outcome.rs.
//!
//! **Vetoes** (`borg no`, veto.rs): an engineer's "no" is an episode of kind `veto`, asked with its own
//! prompt (`veto::OPENING`), whose answer may also name an invariant. It is learned at the end of any
//! session, not only its own (a terminal veto may name an ended session). A veto merged into a lesson
//! an earlier veto made counts as `veto_repeat`; an invariant of a lesson placed in this store becomes
//! a proposed constraint in the queen's queue (`veto::propose`), never a constraint.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::conscience::{self, Challenge};
use crate::outcome;
use crate::trust;

pub mod regret;

static CORRECTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(no\b|nope|don'?t|do not|stop|wrong|that'?s not|not what|why did you|why do you|i said|instead|you should|never\b|always\b|please don'?t|double check|carefully check|check against|are you sure|hmm|but\b|just a second|still\b|can'?t we|don'?t we|isn'?t)").unwrap()
});
static REJECTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)want to proceed with this tool use.*?the user said:\s*(.*)").unwrap());
/// A tool call the engineer refused without saying why: omp's approval prompt ("Tool call denied by
/// user: bash"), omp over ACP ("Tool call rejected by user (bash)"), Claude's plain rejection.
static DENIAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(tool call denied by user: |tool call rejected by user \(|the user doesn'?t want to proceed with this tool use)").unwrap()
});

/// A prompt that pushes back on the drone's last proposal (same heuristic as the offline miner).
pub fn is_correction(prompt: &str) -> bool {
    let t = prompt.trim();
    t.len() > 3 && !t.starts_with('<') && CORRECTION.is_match(t)
}

/// The engineer's words from a rejected tool call or plan, if any.
pub fn rejection(error: &str) -> Option<String> {
    let said = REJECTION.captures(error)?.get(1)?.as_str();
    let said = said.split("Note: The user").next().unwrap_or(said).trim();
    (said.len() > 3).then(|| said.chars().take(1500).collect())
}

/// A refusal that carries no words (see `DENIAL`): the engineer's reason is their next prompt.
pub fn denied(error: &str) -> bool {
    rejection(error).is_none() && DENIAL.is_match(error)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Episode {
    pub id: String,
    pub ms: i64,
    pub repo: String,
    pub session: String,
    pub task: String,
    pub agent: String,
    pub rejected_tool: Option<String>,
    pub correction: String,
    /// Who corrected: `git config user.email` in the episode's cwd, empty if unknown.
    #[serde(default)]
    pub engineer: String,
    /// Empty for a correction; `regret::KIND` for a decision a later session fixed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regret: Option<regret::Regret>,
}

/// A lesson as stored: the conscience's `Challenge` plus provenance and consolidation state.
/// Append-only: a merge writes a new version with the same id; readers keep the latest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Lesson {
    pub id: String,
    pub origin_ms: i64,
    pub repo: String,
    pub question: String,
    pub situations: Vec<String>,
    pub consequence: String,
    pub support: u32,
    pub episodes: Vec<String>,
    pub history: Vec<Value>,
    pub version: u32,
    /// Engineers whose corrections this lesson was learned from, first contributor first.
    #[serde(default)]
    pub engineers: Vec<String>,
    /// When each later engineer joined (ms of their first episode merged in): trust.rs counts a
    /// lesson's outcomes for an engineer only from then on. The first contributor is absent.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub joined: std::collections::BTreeMap<String, i64>,
    /// Kind of concern (trust.rs `AREAS`), set by the generalising model call; "other" if absent.
    #[serde(default = "other_area")]
    pub area: String,
    /// Existing lessons this one replaces or contradicts; the conscience asks this one instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
    /// "queen" for a queen answer (queen.rs), "import" for team knowledge read from a directory
    /// (assimilate_dir.rs), kept across merges; empty for learned lessons.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// Where the lesson applies, as names (systems, repos, services: "Orion", "harbor_orion"). Empty:
    /// wherever it matches. A scoped lesson is asked only when the drone's context names one of them
    /// (conscience.rs `in_scope`), so a team rule for one system never reaches another.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    /// The outside rule or idiom this lesson replaces, and why ("upstream petri-net-design seeds a budget
    /// with a reset arc; Orion drains instead because ..."). Lessons replace lessons via `supersedes`;
    /// this names what is not a lesson (a vendored skill, upstream docs). Rendered with the question:
    /// an override that does not name what it replaces loses to it (seen in an earlier eval).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub overrides: String,
    /// Source location of an imported lesson (`path:line` in the source repo); empty for learned ones.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin: String,
    /// Alternative phrasings of the lesson for search (facets.rs): the concept without names, the symptom,
    /// the action, the consequence, German, the identifiers as aliases. Each is its own BM25 document
    /// (conscience.rs `rank`); never shown to a drone. Empty for lessons learned before facets existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facets: Vec<String>,
    /// Who should hear the lesson (reach.rs): empty for the team, `repo` for its repo only, `personal`
    /// for a tombstone withdrawn to an engineer's private collective.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reach: String,
}

fn other_area() -> String {
    trust::OTHER.into()
}

pub fn home() -> PathBuf {
    std::env::var("BORG_HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share/borg"))
}

pub(crate) fn append_jsonl<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{}", serde_json::to_string(v).unwrap())
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Vec<T> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// False when BORG_LEARN=off (eval drones): nothing is captured or recorded.
pub fn learning() -> bool {
    std::env::var("BORG_LEARN").as_deref() != Ok("off")
}

/// The engineer working in `cwd`: their git user.email, empty when unknown.
pub fn engineer(cwd: &str) -> String {
    if cwd.is_empty() {
        return String::new();
    }
    Command::new("git")
        .args(["-C", cwd, "config", "user.email"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

pub fn record_episode(e: &Episode) -> std::io::Result<()> {
    record_episode_in(&home(), e)
}

/// `record_episode` into `store` (the end-of-session job writes to the route's store).
///
/// # Errors
/// When the episode cannot be appended.
pub fn record_episode_in(store: &Path, e: &Episode) -> std::io::Result<()> {
    append_jsonl(&store.join("episodes.jsonl"), e)?;
    count_correction(store, e);
    Ok(())
}

/// A correction in the usage statistics: a rejected tool call, a pushback or a regret, with a short excerpt.
pub fn count_correction(store: &Path, e: &Episode) {
    // A veto counts itself (`stats::VETO`, veto.rs).
    if e.kind == crate::veto::KIND {
        return;
    }
    let snippet = crate::stats::excerpt(&e.correction, 160);
    let (why, text) = match &e.rejected_tool {
        _ if e.kind == regret::KIND => ("regret", snippet),
        Some(tool) => ("rejected_tool", format!("{tool}: {snippet}")),
        None => ("pushback", snippet),
    };
    crate::stats::count(store, crate::stats::CORRECTION, &e.session, |ev| {
        ev.ms = e.ms;
        ev.engineer.clone_from(&e.engineer);
        ev.repo.clone_from(&e.repo);
        ev.why = why.into();
        ev.text = text;
    });
}

/// The latest version of every lesson.
pub fn lessons(path: &Path) -> Vec<Lesson> {
    let mut by: HashMap<String, Lesson> = HashMap::new();
    for l in read_jsonl::<Lesson>(path) {
        if by.get(&l.id).is_none_or(|o| o.version <= l.version) {
            by.insert(l.id.clone(), l);
        }
    }
    let mut v: Vec<Lesson> = by.into_values().collect();
    v.sort_by_key(|l| l.origin_ms);
    v
}

/// The correction prompt's opening.
const OPENING: &str = "An engineer corrected a coding agent. Decide whether the correction expresses a reusable engineering \
lesson: a critical concern about quality, design, correctness, reuse, spec conformance or root cause that this engineer would \
plausibly raise again about similar future work. Operational remarks (\"still working?\", \"stop\", \"push\", \"restart\") are NOT lessons.";

/// Who should hear a lesson: shared by the correction and the regret prompt.
const REACH_RULES: &str = "Then decide its reach: who should hear it. Ask: would a teammate working on the same code get this correction too?
- personal: how this one engineer likes the agent to work with them, not a property of the code: commit message style \
or length, AI attribution or co-author trailers, whether and when to commit, push, branch or open a PR, what to stage, \
how to report or ask, language and tone, their own tooling habits.
- repo: a convention of the repo it happened in that other repos do not share (its branch or deploy workflow, a tool \
or command it mandates).
- team: everything else: an engineering concern about the code, design, correctness or verification.";

const FIELDS: &str = "If it is a lesson, write:
- question: ONE question the agent should ask itself next time, general enough to transfer, keeping the technical substance;
- situations: 1-3 lines, each `<action> on/in <kind of state or component> [under <condition>]`, without project/class names;
- consequence: one line, what goes wrong or what the engineer requires instead;";

/// The last field of either prompt.
const AREA: &str = "- area: the one kind of concern: concurrency, spec-conformance, reuse-existing, architecture-boundary, verification, minimality, \
protocol-correctness, data-source, security, dependency-version, scope, or other if none fits.";

/// Merging and superseding: shared by the correction and the regret prompt.
const MERGE_RULES: &str = "Existing lessons that may express the same concern are listed. If one of them IS the same concern (the same kind of mistake) \
and agrees with this correction, set same_as to its id and write the merged, improved wording in question/situations/consequence. \
Otherwise same_as is empty.

If this correction contradicts or replaces a listed lesson (in the same situation it requires something different or the \
opposite, e.g. the engineer reversed an earlier correction), list that lesson's id in supersedes; never the same_as lesson. \
Otherwise supersedes is empty.";

/// The prompt for one episode: a correction's, a veto's (veto.rs) or a regret's (regret.rs).
fn episode_prompt(e: &Episode, listed: &[String]) -> String {
    let existing = if listed.is_empty() { "(none)".into() } else { listed.join("\n") };
    if e.kind == crate::veto::KIND {
        let agent = if e.agent.trim().is_empty() { "(not available)" } else { e.agent.as_str() };
        return format!(
            "{}\n\n{REACH_RULES}\n\n{FIELDS}\n{AREA}\n{}\n\n{MERGE_RULES}\n\n## Task the agent had\n{}\n\n## What the agent proposed (excerpt)\n{agent}\n\n## Engineer's veto\n{}\n\n## Existing lessons\n{existing}\n",
            crate::veto::OPENING,
            crate::veto::INVARIANT,
            e.task.chars().take(1500).collect::<String>(),
            e.correction,
        );
    }
    if let Some(r) = e.regret.as_ref().filter(|_| e.kind == regret::KIND) {
        let (head, sections) = (regret::head(r), regret::sections(r, &e.task));
        let (reach, merge) = (regret::neutral(REACH_RULES), regret::neutral(MERGE_RULES));
        return format!("{head}\n\n{reach}\n\n{}\n{AREA}\n\n{merge}\n\n{sections}## Existing lessons\n{existing}\n", regret::FIELDS);
    }
    format!(
        "{OPENING}\n\n{REACH_RULES}\n\n{FIELDS}\n{AREA}\n\n{MERGE_RULES}\n\n## Task the agent had\n{}\n\n## What the agent proposed (excerpt)\n{}\n{}\n\n## Engineer's correction\n{}\n\n## Existing lessons\n{existing}\n",
        e.task.chars().take(1500).collect::<String>(),
        e.agent,
        e.rejected_tool.as_deref().unwrap_or(""),
        e.correction,
    )
}

fn schema() -> Value {
    json!({"type": "object", "additionalProperties": false, "properties": {
        "is_lesson": {"type": "boolean"}, "question": {"type": "string"},
        "situations": {"type": "array", "items": {"type": "string"}}, "consequence": {"type": "string"},
        "same_as": {"type": "string"}, "area": {"type": "string", "enum": trust::AREAS},
        "supersedes": {"type": "array", "items": {"type": "string"}},
        "reach": {"type": "string", "enum": ["team", "repo", "personal"]}},
        "required": ["is_lesson", "question", "situations", "consequence", "same_as", "area", "supersedes", "reach"]})
}

/// `schema` plus the invariant a veto's reason may name (veto.rs).
fn veto_schema() -> Value {
    let mut s = schema();
    s["properties"]["invariant"] = json!({"type": "string"});
    if let Some(r) = s["required"].as_array_mut() {
        r.push(json!("invariant"));
    }
    s
}

/// The answer schema for `prompt`: a veto's prompt (it starts with `veto::OPENING`) gets the invariant
/// field; correction and regret calls stay as they were.
pub fn schema_for(prompt: &str) -> Value {
    if prompt.starts_with(crate::veto::OPENING) { veto_schema() } else { schema() }
}

/// The model call failed (quota, rate limit, not logged in, network, malformed answer): unlike a
/// verdict of "not a lesson", the episode stays waiting and a later `borg learn` retries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AskFailed;

/// After this many failed model calls in a row `borg learn` stops: the model is unavailable, and
/// asking about every remaining episode would only burn time (and quota once it comes back).
const MAX_FAILURES: usize = 3;

/// The model's verdict on an episode that is a lesson.
struct Answer {
    question: String,
    situations: Vec<String>,
    consequence: String,
    area: String,
    reach: String,
    same_as: String,
    supersedes: Vec<String>,
    /// A veto's invariant (veto.rs); empty for every other episode, and when the model named none.
    invariant: String,
}

impl Answer {
    /// `Ok(None)` not a lesson; `Err` no usable verdict.
    fn parse(v: &Value) -> Result<Option<Answer>, AskFailed> {
        match v["is_lesson"].as_bool() {
            Some(true) => {}
            Some(false) => return Ok(None),
            None => return Err(AskFailed),
        }
        let strs = |k: &str| v[k].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect::<Vec<_>>();
        let text = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        let question = v["question"].as_str().filter(|q| !q.trim().is_empty()).ok_or(AskFailed)?.to_string();
        let reach = v["reach"].as_str().filter(|r| [crate::reach::REPO, crate::reach::PERSONAL].contains(r)).unwrap_or_default();
        Ok(Some(Answer {
            question,
            situations: strs("situations"),
            consequence: text("consequence"),
            area: trust::area(v["area"].as_str().unwrap_or_default()).to_string(),
            reach: reach.to_string(),
            same_as: text("same_as"),
            supersedes: strs("supersedes"),
            invariant: text("invariant"),
        }))
    }
}

/// The library's lessons most like the episode (at most 5), offered for merging.
fn similar<'a>(e: &Episode, library: &'a [Lesson]) -> Vec<&'a Lesson> {
    let as_challenges: Vec<Challenge> = library
        .iter()
        .map(|l| Challenge {
            id: l.id.clone(),
            origin_ms: l.origin_ms,
            question: format!("{} {}", l.question, l.situations.join(" ")),
            situations: l.situations.clone(),
            consequence: l.consequence.clone(),
            // A superseded lesson is not offered for merging; its live successor is, in its place. A
            // re-reversal then contradicts the successor and becomes a new lesson superseding it.
            supersedes: l.supersedes.clone(),
            engineers: l.engineers.clone(),
            source: l.source.clone(),
            ..Default::default() // similarity for merging is about meaning, not usefulness or trust
        })
        .collect();
    let query = format!("{} {} {}", e.task, e.agent, e.correction);
    conscience::select(&as_challenges, &query, 5).iter().filter_map(|c| library.iter().find(|l| l.id == c.id)).collect()
}

/// `old` with the episode merged in: the new wording, the old one kept in `history`.
fn merged(old: &Lesson, e: &Episode, a: Answer, mut supersedes: Vec<String>) -> Lesson {
    let mut history = old.history.clone();
    history.push(json!({"question": old.question, "consequence": old.consequence, "version": old.version}));
    let mut episodes = old.episodes.clone();
    episodes.push(e.id.clone());
    let (mut engineers, mut joined) = (old.engineers.clone(), old.joined.clone());
    // Emails compare case-insensitively; an alias (collective.rs "Engineers") is stored as
    // its raw email and mapped when read, so the store never depends on this machine's config.
    let same = |x: &String| x.trim().to_lowercase() == e.engineer.trim().to_lowercase();
    if !e.engineer.trim().is_empty() && !engineers.iter().any(same) {
        engineers.push(e.engineer.clone());
        joined.insert(e.engineer.clone(), e.ms);
    }
    for id in old.supersedes.iter().rev() {
        if !supersedes.contains(id) {
            supersedes.insert(0, id.clone());
        }
    }
    Lesson {
        question: a.question,
        situations: a.situations,
        consequence: a.consequence,
        support: old.support + 1,
        episodes,
        history,
        version: old.version + 1,
        engineers,
        joined,
        area: a.area,
        supersedes,
        // New wording: the old facets describe the old wording; `borg learn` generates new ones.
        facets: Vec::new(),
        reach: crate::reach::narrower(&old.reach, &a.reach).to_string(),
        ..old.clone()
    }
}

/// A new lesson from the episode alone.
fn fresh(e: &Episode, a: Answer, supersedes: Vec<String>) -> Lesson {
    Lesson {
        id: format!("L{}", e.id),
        origin_ms: e.ms,
        repo: e.repo.clone(),
        question: a.question,
        situations: a.situations,
        consequence: a.consequence,
        support: 1,
        episodes: vec![e.id.clone()],
        version: 1,
        engineers: Some(e.engineer.clone()).filter(|x| !x.is_empty()).into_iter().collect(),
        area: a.area,
        supersedes,
        reach: a.reach,
        ..Lesson::default()
    }
}

/// Generalise one episode against the existing library: `Ok(Some)` a new lesson or a merged version
/// of an existing one, `Ok(None)` not a lesson, `Err` no verdict. `ask` performs the model call
/// (injectable for tests). A regret (regret.rs) gets its own prompt, the same verdict.
///
/// # Errors
/// `AskFailed` when the model gave no usable verdict.
pub fn generalise(e: &Episode, library: &[Lesson], ask: &dyn Fn(&str) -> Option<Value>) -> Result<Option<Lesson>, AskFailed> {
    generalise_with(e, library, ask).map(|o| o.map(|(l, _)| l))
}

/// `generalise`, with the invariant a veto's answer named (empty otherwise).
///
/// # Errors
/// `AskFailed` when the model gave no usable verdict.
pub fn generalise_with(e: &Episode, library: &[Lesson], ask: &dyn Fn(&str) -> Option<Value>) -> Result<Option<(Lesson, String)>, AskFailed> {
    // A withdrawn personal lesson (reach.rs) is no merge target: it lives in a private collective now.
    let library: Vec<Lesson> = library.iter().filter(|l| l.reach != crate::reach::PERSONAL).cloned().collect();
    let similar = similar(e, &library);
    let listed: Vec<String> = similar.iter().map(|l| format!("[{}] {} -> {}", l.id, l.question, l.consequence)).collect();
    let Some(a) = Answer::parse(&ask(&episode_prompt(e, &listed)).ok_or(AskFailed)?)? else { return Ok(None) };
    let same_as = similar.iter().find(|l| a.same_as == l.id);
    // Only lessons the model was shown can be superseded, and never the one it merges into.
    let supersedes: Vec<String> =
        a.supersedes.iter().filter(|id| similar.iter().any(|l| &l.id == *id) && same_as.is_none_or(|m| &m.id != *id)).cloned().collect();
    let invariant = a.invariant.trim().to_string();
    Ok(Some((
        match same_as {
            Some(old) => merged(old, e, a, supersedes),
            None => fresh(e, a, supersedes),
        },
        invariant,
    )))
}

/// One structured call to the drone's model (runner.rs picks claude, omp or opencode).
pub fn ask_claude(prompt: &str) -> Option<Value> {
    ask_model(prompt, &schema_for(prompt)).map(|(v, _)| v)
}

/// The model call behind `ask_claude`, for any JSON schema: the structured answer and its cost in USD
/// (None when the CLI reported none). `BORG_LEARN_MODEL` picks the model, `runner::choose` the CLI.
pub fn ask_model(prompt: &str, schema: &Value) -> Option<(Value, Option<f64>)> {
    let started = std::time::Instant::now();
    let answer = call_model(prompt, schema);
    crate::stats::count(&home(), crate::stats::MODEL_CALL, "", |ev| {
        ev.why = crate::stats::purpose().into();
        ev.usd = answer.as_ref().and_then(|(_, c)| *c).unwrap_or(0.0);
        ev.dur_ms = started.elapsed().as_millis() as u64;
        if answer.is_none() {
            ev.text = "no answer".into();
        }
    });
    answer
}

fn call_model(prompt: &str, schema: &Value) -> Option<(Value, Option<f64>)> {
    let candidates = crate::runner::choose(std::env::var("BORG_LEARN_MODEL").ok().as_deref());
    crate::runner::structured(&candidates, prompt, schema, learn_timeout())
}

/// How long one learn-side model call may take (`BORG_LEARN_TIMEOUT_S`, default 300 s): it runs in
/// the background, but a hung runner must not keep `borg learn` alive forever.
fn learn_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(std::env::var("BORG_LEARN_TIMEOUT_S").ok().and_then(|v| v.parse().ok()).unwrap_or(300))
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// `borg learn [--session ID]`: generalise every episode not yet folded into a lesson.
/// `--session` is given when that session ended: its asked lessons are then settled. Any run also
/// settles sessions idle for a day, so a missing `end` hook does not bias the outcomes.
/// Returns (new lessons, merges, episodes still waiting because the model gave no verdict).
pub fn learn_pending(session: Option<&str>, ask: &dyn Fn(&str) -> Option<Value>) -> (usize, usize, usize) {
    learn_in(&home(), session, ask, learning(), now_ms())
}

/// `learn_pending` on the store in `home`. With `track`, outcomes of asked lessons are recorded
/// (outcome.rs): a merge into a lesson asked earlier in the episode's session, and a lesson asked
/// earlier that the new one supersedes, are `asked_then_corrected` (`outcome::contradicted`); at the end of `session`, and in every session idle for
/// `outcome::IDLE_MS` at `now`, the other asked lessons are `asked_not_corrected`. So any run
/// settles, not only the `end` hook's; a session it skipped episodes of is not settled as idle.
/// Only a "not a lesson" verdict is recorded as skipped: an episode whose model call failed stays
/// waiting (and its session unsettled) for the next run, and after `MAX_FAILURES` failures in a row
/// the run stops asking. Returns (new lessons, merges, episodes still waiting for a verdict).
pub fn learn_in(home: &Path, session: Option<&str>, ask: &dyn Fn(&str) -> Option<Value>, track: bool, now: i64) -> (usize, usize, usize) {
    learn_in_reporting(home, session, ask, track, now, &mut |_| {})
}

/// A progress line on stderr: `[12/104 · 18 s · ~28 min left] new lesson L…: <question>`.
pub fn print_step(st: Step) {
    let left = (st.total - st.done) as f64 * st.secs / 60.0;
    let mut v: String = st.verdict.chars().take(110).collect();
    if v.len() < st.verdict.len() {
        v.push('…');
    }
    eprintln!("[{}/{} · {:.0} s · ~{:.0} min left] {v}", st.done, st.total, st.secs, left);
}

/// One model verdict during a learn run, for a progress line: the episode's place among those this
/// run asks about (`done` of `total`), and what came of it.
pub struct Step<'a> {
    pub done: usize,
    pub total: usize,
    pub verdict: &'a str,
    pub secs: f64,
}

/// Whether a veto's lesson, placed as `placed`, proposes its `invariant` as a constraint: only a lesson
/// placed in this store; a personal or dropped one never does, a preference being no invariant.
fn proposes(placed: &crate::reach::Placed, invariant: &str) -> bool {
    matches!(placed, crate::reach::Placed::Here(_)) && !invariant.trim().is_empty()
}

/// False when BORG_REGRET_CREDIT=off: a regret no longer counts against the lessons asked before the
/// decision (`credit_regrets`).
pub fn regret_credit() -> bool {
    std::env::var("BORG_REGRET_CREDIT").as_deref() != Ok("off")
}

/// Each regret episode generalised into a lesson of this store (`library`, as stored: a lesson placed in
/// a private store is not in it) blames the lessons asked in its deciding session before the decision
/// (`outcome::regretted`). Runs over every such episode on every run, so regrets learned before this
/// existed are credited once too; the (cause, lesson) key keeps it idempotent. A regret the model judged
/// "not a lesson", one still waiting, and one without `decision_ms` credit nothing. For a review regret
/// the deciding session is the episode's own: a lesson `contradicted` already marked corrected there is
/// not blamed twice.
///
/// Indexed once per run, so a run costs one pass over the log and the library plus each deciding
/// session's own records: `regretted` reads only the deciding session's records, and a regret's credits
/// carry that session too.
fn credit_regrets(regrets: &[Episode], library: &[Lesson], outcomes: &mut Vec<outcome::Outcome>, path: &Path) {
    let mut holder: HashMap<&str, &Lesson> = HashMap::new();
    for l in library {
        for id in &l.episodes {
            holder.entry(id.as_str()).or_insert(l);
        }
    }
    let deciding: HashSet<&str> = regrets.iter().filter_map(|e| e.regret.as_ref()).map(|r| r.decision_session.as_str()).collect();
    let mut by_session: HashMap<String, Vec<outcome::Outcome>> = HashMap::new();
    for o in outcomes.iter().filter(|o| deciding.contains(o.session.as_str())) {
        by_session.entry(o.session.clone()).or_default().push(o.clone());
    }
    for e in regrets {
        let Some(r) = &e.regret else { continue };
        let Some(holder) = holder.get(e.id.as_str()) else { continue };
        let target: Vec<String> = std::iter::once(holder.id.clone()).chain(holder.supersedes.iter().cloned()).collect();
        let records = by_session.entry(r.decision_session.clone()).or_default();
        for o in outcome::regretted(records, &r.decision_session, r.decision_ms, &e.id, &target, regret::strength(r), e.ms, &e.repo) {
            let _ = outcome::append(path, &o);
            records.push(o.clone());
            outcomes.push(o);
        }
    }
}

/// `learn_in`, calling `report` after every model call (the CLI prints it; the hooks pass a no-op).
///
/// One run per store at a time (an exclusive lock on `lessons.jsonl.lock`): session-end runs of several
/// drones share the veto episodes and the regret credits, and two runs reading the store before either
/// writes would learn a veto twice and count its credits twice. A waiting run then reads what the first
/// one wrote and has nothing left to do for them.
pub fn learn_in_reporting(
    home: &Path,
    session: Option<&str>,
    ask: &dyn Fn(&str) -> Option<Value>,
    track: bool,
    now: i64,
    report: &mut dyn FnMut(Step),
) -> (usize, usize, usize) {
    crate::live::locked(&home.join("lessons.jsonl"), || learn_unlocked(home, session, ask, track, now, report))
}

fn learn_unlocked(
    home: &Path,
    session: Option<&str>,
    ask: &dyn Fn(&str) -> Option<Value>,
    track: bool,
    now: i64,
    report: &mut dyn FnMut(Step),
) -> (usize, usize, usize) {
    let (ep_path, lessons_path) = (home.join("episodes.jsonl"), home.join("lessons.jsonl"));
    let outcomes_path = outcome::path_beside(&lessons_path);
    let mut outcomes = if track { outcome::read(&outcomes_path) } else { Vec::new() };
    let mut library = lessons(&lessons_path);
    let done: std::collections::HashSet<String> = library.iter().flat_map(|l| l.episodes.clone()).collect();
    let skipped_path = home.join("episodes-not-lessons.txt");
    let skipped: std::collections::HashSet<String> = std::fs::read_to_string(&skipped_path).unwrap_or_default().lines().map(str::to_string).collect();
    let (mut new, mut merged, mut failed, mut failures_in_row) = (0, 0, 0, 0);
    // Newest episode per session, and sessions with episodes this run left pending (--session).
    let mut last_episode: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut pending: std::collections::HashSet<String> = std::collections::HashSet::new();
    let episodes = read_jsonl::<Episode>(&ep_path);
    let regrets: Vec<Episode> = episodes.iter().filter(|e| e.kind == regret::KIND && e.regret.is_some()).cloned().collect();
    // A veto is learned at the end of any session (veto.rs): a terminal veto may name one already ended.
    let mine = |e: &Episode| session.is_none_or(|s| s == e.session) || e.kind == crate::veto::KIND;
    let vetoes: HashSet<String> = episodes.iter().filter(|e| e.kind == crate::veto::KIND).map(|e| e.id.clone()).collect();
    let total = episodes.iter().filter(|e| !done.contains(&e.id) && !skipped.contains(&e.id) && mine(e)).count();
    let mut asked = 0;
    for e in episodes {
        let last = last_episode.entry(e.session.clone()).or_insert(e.ms);
        *last = (*last).max(e.ms);
        if done.contains(&e.id) || skipped.contains(&e.id) {
            continue;
        }
        if !mine(&e) {
            pending.insert(e.session.clone());
            continue;
        }
        if failures_in_row >= MAX_FAILURES {
            failed += 1;
            pending.insert(e.session.clone());
            continue;
        }
        let started = std::time::Instant::now();
        let verdict = generalise_with(&e, &library, ask);
        asked += 1;
        let what = match &verdict {
            Err(AskFailed) => "no answer from the model, left pending".to_string(),
            Ok(Some((l, _))) if l.version > 1 => format!("merged into {}: {}", l.id, l.question),
            Ok(Some((l, _))) => format!("new lesson {}: {}", l.id, l.question),
            Ok(None) => "not a lesson".to_string(),
        };
        report(Step { done: asked, total, verdict: &what, secs: started.elapsed().as_secs_f64() });
        failures_in_row = if verdict.is_err() { failures_in_row + 1 } else { 0 };
        match verdict {
            Err(AskFailed) => {
                failed += 1;
                pending.insert(e.session.clone());
            }
            Ok(Some((l, invariant))) => {
                if l.version > 1 {
                    merged += 1
                } else {
                    new += 1
                }
                // A personal lesson goes to the engineer's private collective, never into a team store (reach.rs).
                let placed = crate::reach::place(home, &l);
                // Where it came from, for the feed: a review finding, a later fix, or a correction.
                let source = match &e.regret {
                    Some(r) if regret::is_review(r) => "review",
                    Some(_) => "regret",
                    None if e.kind == crate::veto::KIND => "veto",
                    None => "session",
                };
                let (kind, why, counted_in) = match &placed {
                    crate::reach::Placed::Here(l) => {
                        let _ = append_jsonl(&lessons_path, &**l);
                        (if l.version > 1 { crate::stats::LESSON_MERGED } else { crate::stats::LESSON_NEW }, source, home.to_path_buf())
                    }
                    crate::reach::Placed::Private(store) => (crate::stats::LESSON_NEW, "personal", store.clone()),
                    crate::reach::Placed::Dropped => (crate::stats::NOT_A_LESSON, "personal", home.to_path_buf()),
                };
                crate::stats::count(&counted_in, kind, &e.session, |ev| {
                    ev.engineer = e.engineer.clone();
                    ev.repo = e.repo.clone();
                    ev.lessons = vec![l.id.clone()];
                    ev.why = why.into();
                });
                if e.kind == crate::veto::KIND {
                    // The same "no" said twice: merged into a lesson an earlier veto made (H5b's numerator).
                    let earlier = library.iter().find(|x| x.id == l.id);
                    if l.version > 1 && earlier.is_some_and(|o| o.episodes.iter().any(|id| id != &e.id && vetoes.contains(id))) {
                        crate::stats::count(&counted_in, crate::stats::VETO_REPEAT, &e.session, |ev| {
                            ev.engineer = e.engineer.clone();
                            ev.repo = e.repo.clone();
                            ev.lessons = vec![l.id.clone()];
                            ev.text = crate::stats::excerpt(&e.correction, 160);
                        });
                    }
                    if proposes(&placed, &invariant) {
                        let _ = crate::veto::propose(home, &l, &invariant, &e, now);
                    }
                }
                let target = (l.version > 1).then_some(l.id.as_str());
                // A veto credited the lessons shown before it already (veto.rs): it never doubles as a
                // correction, nor as a missed question (`applied_then_corrected`).
                let track_results = track && e.kind != crate::veto::KIND;
                let contradicted = match track_results {
                    true => outcome::contradicted(&outcomes, &e.session, e.ms, target, &l.supersedes),
                    false => Vec::new(),
                };
                for id in contradicted {
                    let o = outcome::Outcome {
                        kind: outcome::CORRECTED.into(),
                        session: e.session.clone(),
                        lessons: vec![id],
                        ms: e.ms,
                        repo: e.repo.clone(),
                        ..Default::default()
                    };
                    let _ = outcome::append(&outcomes_path, &o);
                    outcomes.push(o);
                }
                let missed = match track_results {
                    true => outcome::missed(&outcomes, &e.session, e.ms, target, &l.supersedes),
                    false => Vec::new(),
                };
                for id in missed {
                    let o = outcome::Outcome {
                        kind: outcome::MISSED.into(),
                        session: e.session.clone(),
                        lessons: vec![id],
                        ms: e.ms,
                        repo: e.repo.clone(),
                        ..Default::default()
                    };
                    let _ = outcome::append(&outcomes_path, &o);
                    outcomes.push(o);
                }
                library.retain(|x| x.id != l.id);
                library.push(l);
            }
            Ok(None) => {
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&skipped_path) {
                    let _ = writeln!(f, "{}", e.id);
                }
                crate::stats::count(home, crate::stats::NOT_A_LESSON, &e.session, |ev| ev.engineer = e.engineer.clone());
            }
        }
    }
    if track {
        if regret_credit() {
            credit_regrets(&regrets, &lessons(&lessons_path), &mut outcomes, &outcomes_path);
        }
        let mut ended: Vec<String> =
            outcome::idle_sessions(&outcomes, &last_episode, now).into_iter().filter(|s| !pending.contains(*s)).map(str::to_string).collect();
        if let Some(s) = session.filter(|s| !ended.iter().any(|x| x == s) && !pending.contains(*s)) {
            ended.push(s.to_string());
        }
        for s in &ended {
            for o in outcome::settle(&outcomes, s, now) {
                let _ = outcome::append(&outcomes_path, &o);
                outcomes.push(o);
            }
        }
    }
    (new, merged, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_regret_is_asked_with_its_own_prompt_and_the_shared_rules() {
        let r = regret::Regret {
            decision_task: "read the flag in b".into(),
            fix_subject: "fix: take the lock".into(),
            signals: vec!["subject".into()],
            age_days: 3,
            ..regret::Regret::default()
        };
        let e = Episode {
            kind: regret::KIND.into(),
            regret: Some(r),
            task: "make the flag safe".into(),
            ..ep("Rdddd-ffff", "Fixed later (3d, subject): fix: take the lock")
        };
        let shown = std::cell::RefCell::new(String::new());
        let l = generalise(&e, &[], &|p| {
            *shown.borrow_mut() = p.to_string();
            Some(json!({"is_lesson": true, "question": "Before reading a shared flag without a lock, has a concurrent writer been considered?",
                "situations": ["reading a shared flag across threads"], "consequence": "a race", "same_as": "", "area": "concurrency", "supersedes": []}))
        })
        .unwrap()
        .unwrap();
        let p = shown.into_inner();
        assert!(p.starts_with("A coding agent made a decision in an earlier session") && p.contains("3 days later"), "{p}");
        for part in [
            regret::neutral(REACH_RULES).as_str(),
            regret::FIELDS,
            AREA,
            regret::neutral(MERGE_RULES).as_str(),
            "## The decision (task)\nread the flag in b",
            "## The fix (subject, task)\nfix: take the lock\nmake the flag safe",
        ] {
            assert!(p.contains(part), "{part}");
        }
        assert!(!p.contains("## Engineer's correction") && !p.contains(OPENING) && !p.contains("correction"), "{p}");
        assert_eq!((l.id.as_str(), l.area.as_str(), l.episodes.as_slice()), ("LRdddd-ffff", "concurrency", &["Rdddd-ffff".to_string()][..]));
        // A correction keeps its prompt; a kind without its regret is asked as a correction.
        assert!(episode_prompt(&ep("e1", "no, the lock"), &[]).starts_with(OPENING));
        assert!(episode_prompt(&Episode { kind: regret::KIND.into(), ..ep("e2", "no") }, &[]).starts_with(OPENING));
    }

    /// The injected model for vetoes: like `fake_ask` on the veto's section, plus `invariant:<rest>`.
    fn fake_veto_ask(p: &str) -> Option<Value> {
        let reason = p.split("## Engineer's veto\n").nth(1)?.lines().next()?;
        let after = |k: &str| reason.split(k).nth(1).and_then(|r| r.split_whitespace().next()).unwrap_or("").to_string();
        let invariant = reason.split("invariant:").nth(1).unwrap_or("").trim().to_string();
        Some(json!({"is_lesson": true, "question": format!("Q {reason}"), "situations": [], "consequence": "c",
            "same_as": after("merge:"), "supersedes": [], "invariant": invariant}))
    }

    fn veto(id: &str, session: &str, ms: i64, reason: &str) -> Episode {
        Episode { kind: crate::veto::KIND.into(), ..at(id, session, ms, "ann@x", reason) }
    }

    #[test]
    fn a_veto_is_asked_with_its_own_prompt_and_schema() {
        let p = episode_prompt(&veto("no-s-1", "s", 1, "never read the commit log for merges"), &[]);
        assert!(p.starts_with(crate::veto::OPENING), "{p}");
        for part in [
            "## Engineer's veto\nnever read the commit log for merges",
            crate::veto::INVARIANT,
            REACH_RULES,
            FIELDS,
            MERGE_RULES,
            "## Task the agent had\nplan the relay",
        ] {
            assert!(p.contains(part), "{part}");
        }
        assert!(!p.contains(OPENING) && !p.contains("## Engineer's correction"));
        let without = episode_prompt(&Episode { agent: String::new(), ..veto("no-s-2", "s", 1, "no") }, &[]);
        assert!(without.contains("## What the agent proposed (excerpt)\n(not available)"), "{without}");
        assert!(schema_for(&p)["required"].as_array().unwrap().contains(&json!("invariant")));
        assert!(schema_for(&p)["properties"]["invariant"].is_object());
        let correction = episode_prompt(&ep("e1", "no, the lock"), &[]);
        assert_eq!(schema_for(&correction), schema(), "a correction's call is unchanged");
        // The invariant comes back with the lesson; a correction's answer has none.
        let (l, inv) =
            generalise_with(&veto("no-s-3", "s", 1, "merge lookups invariant: lookups go through the review API"), &[], &fake_veto_ask).unwrap().unwrap();
        assert_eq!((l.id.as_str(), inv.as_str()), ("Lno-s-3", "lookups go through the review API"));
        let (_, none) = generalise_with(&ep("e2", "no, the lock merge:x"), &[], &fake_ask).unwrap().unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn a_veto_is_learned_at_any_sessions_end_and_never_counted_as_a_correction() {
        let home = temp_home("veto-any");
        crate::learn::record_episode_in(&home, &veto("no-s1-1", "s1", 1, "never read the commit log for merges")).unwrap();
        append_jsonl(&home.join("episodes.jsonl"), &at("e1", "s1", 2, "ann@x", "a correction of s1")).unwrap();
        assert!(!crate::stats::events(&home).iter().any(|e| e.kind == crate::stats::CORRECTION), "count_correction skips a veto");
        // s2 ends: the veto of s1 is learned, s1's correction waits for its own session.
        assert_eq!(learn_in(&home, Some("s2"), &fake_veto_ask, true, 3), (1, 0, 0));
        let ls = lessons(&home.join("lessons.jsonl"));
        assert_eq!(ls.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), ["Lno-s1-1"]);
        let news: Vec<crate::stats::Event> = crate::stats::events(&home).into_iter().filter(|e| e.kind == crate::stats::LESSON_NEW).collect();
        assert_eq!(news[0].why, "veto");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_veto_merged_into_an_earlier_vetos_lesson_is_a_repeat() {
        let home = temp_home("veto-repeat");
        let repeats = |home: &Path| crate::stats::events(home).iter().filter(|e| e.kind == crate::stats::VETO_REPEAT).count();
        // A lesson from a correction, then a veto merged into it: no repeat (the first "no").
        append_jsonl(&home.join("episodes.jsonl"), &at("e0", "s0", 1, "ann@x", "use the lock on the flag")).unwrap();
        append_jsonl(&home.join("episodes.jsonl"), &veto("no-s1-2", "s1", 2, "the lock again merge:Le0")).unwrap();
        // Le0 was asked in s1 before the veto: the veto credits it (veto.rs), learning never adds a correction.
        let asked = outcome::Outcome { kind: outcome::ASKED.into(), session: "s1".into(), lessons: vec!["Le0".into()], ms: 1, ..Default::default() };
        outcome::append(&home.join("outcomes.jsonl"), &asked).unwrap();
        learn_in(&home, None, &|p| if p.starts_with(crate::veto::OPENING) { fake_veto_ask(p) } else { fake_ask(p) }, true, 3);
        assert_eq!(repeats(&home), 0);
        let results = outcome::read(&home.join("outcomes.jsonl"));
        assert!(!results.iter().any(|o| o.kind == outcome::CORRECTED || o.kind == outcome::MISSED), "{results:?}");
        assert!(!crate::stats::events(&home).iter().any(|e| e.kind == crate::stats::CORRECTED));
        // A second veto with the same reason: merged into the lesson a veto is part of.
        append_jsonl(&home.join("episodes.jsonl"), &veto("no-s2-4", "s2", 4, "the lock, third time merge:Le0")).unwrap();
        assert_eq!(learn_in(&home, Some("s2"), &fake_veto_ask, true, 5), (0, 1, 0));
        assert_eq!(repeats(&home), 1);
        let r = crate::stats::events(&home).into_iter().find(|e| e.kind == crate::stats::VETO_REPEAT).unwrap();
        assert_eq!((r.session.as_str(), r.lessons.clone()), ("s2", vec!["Le0".to_string()]));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn an_invariant_of_a_veto_becomes_a_proposed_constraint_never_a_constraint() {
        let home = temp_home("veto-invariant");
        append_jsonl(&home.join("episodes.jsonl"), &veto("no-s1-1", "s1", 1, "no commit log invariant: merge lookups use the review API")).unwrap();
        append_jsonl(&home.join("episodes.jsonl"), &veto("no-s1-2", "s1", 2, "not now, later")).unwrap();
        learn_in(&home, Some("s1"), &fake_veto_ask, true, 3);
        let queue = crate::queen::entries(&home);
        assert_eq!(queue.len(), 1, "only the veto that named an invariant");
        assert_eq!((queue[0].kind, queue[0].session.as_str()), (crate::queen::Kind::Constraint, "s1"));
        assert!(
            queue[0].evidence.contains(&"invariant: merge lookups use the review API".to_string())
                && queue[0].evidence.contains(&"lesson Lno-s1-1".to_string())
        );
        assert!(!crate::veto::constraints_path(&home).exists());
        // A personal or dropped lesson never proposes; nor does one without an invariant.
        let l = Lesson { id: "L".into(), ..Lesson::default() };
        assert!(proposes(&crate::reach::Placed::Here(Box::new(l)), "x") && !proposes(&crate::reach::Placed::Dropped, "x"));
        assert!(!proposes(&crate::reach::Placed::Private(home.clone()), "x") && !proposes(&crate::reach::Placed::Here(Box::default()), " "));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn detects_corrections_and_rejections() {
        assert!(is_correction("double check against spec document"));
        assert!(is_correction("No, let's not move this into the net"));
        assert!(!is_correction("let's pick up the gpt live plan"));
        let err = "The user doesn't want to proceed with this tool use. The tool use was rejected. the user said:\ncarefully check TOCTOU\nNote: The user's next message may contain a correction";
        assert_eq!(rejection(err).as_deref(), Some("carefully check TOCTOU"));
        assert_eq!(rejection("Exit code 1\nTraceback"), None);
        // omp's refusals carry no words; the reason follows as the next prompt.
        assert!(denied("Tool call denied by user: bash"));
        assert!(denied("Tool call rejected by user (edit)"));
        assert!(denied(
            "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed."
        ));
        assert!(!denied(err), "a rejection with words is captured directly");
        assert!(!denied("Exit code 1\nTool call denied by user: bash"), "only the harness's own message, not tool output");
    }

    fn ep(id: &str, correction: &str) -> Episode {
        Episode {
            id: id.into(),
            ms: 1,
            repo: "r".into(),
            session: "s".into(),
            task: "plan the relay".into(),
            agent: "one ReentrantLock guards delivery".into(),
            rejected_tool: None,
            correction: correction.into(),
            ..Episode::default()
        }
    }

    /// The injected model: `same_as` is the lesson id after "merge:" in the correction, if any, and
    /// `supersedes` the one after "supersede:".
    fn fake_ask(p: &str) -> Option<Value> {
        let correction = p.split("## Engineer's correction\n").nth(1)?.lines().next()?;
        let after = |k: &str| correction.split(k).nth(1).and_then(|r| r.split_whitespace().next()).unwrap_or("").to_string();
        let supersedes: Vec<String> = Some(after("supersede:")).filter(|x| !x.is_empty()).into_iter().collect();
        Some(json!({"is_lesson": true, "question": format!("Q {correction}"), "situations": [], "consequence": "c",
            "same_as": after("merge:"), "supersedes": supersedes}))
    }

    fn results(home: &Path) -> Vec<(String, String, String)> {
        outcome::read(&home.join("outcomes.jsonl")).into_iter().filter(|o| o.kind != outcome::ASKED).map(|o| (o.kind, o.session, o.lessons.join(","))).collect()
    }

    #[test]
    fn every_model_call_is_reported_with_its_place_and_verdict() {
        let home = temp_home("progress");
        for e in [at("e0", "s0", 1, "ann@x", "use the lock on the flag"), at("e1", "s0", 2, "bob@x", "the lock again merge:Le0")] {
            append_jsonl(&home.join("episodes.jsonl"), &e).unwrap();
        }
        let mut steps = Vec::new();
        learn_in_reporting(&home, None, &fake_ask, false, 3, &mut |st| steps.push((st.done, st.total, st.verdict.split(':').next().unwrap().to_string())));
        assert_eq!(steps, [(1, 2, "new lesson Le0".to_string()), (2, 2, "merged into Le0".to_string())]);
        // A second run has nothing left to ask, so it reports nothing.
        steps.clear();
        learn_in_reporting(&home, None, &fake_ask, false, 4, &mut |st| steps.push((st.done, st.total, String::new())));
        assert!(steps.is_empty());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_lesson_contradicted_after_asking_is_corrected_not_followed() {
        let home = temp_home("contradicted");
        append_jsonl(&home.join("episodes.jsonl"), &at("e0", "s0", 1, "ann@x", "use the lock on the flag")).unwrap();
        learn_in(&home, Some("s0"), &fake_ask, true, 2);
        // s1 asks Le0, then the engineer contradicts it: a new lesson superseding Le0.
        asked(&home, "s1", 10, &["Le0"]);
        append_jsonl(&home.join("episodes.jsonl"), &at("e1", "s1", 20, "bob@x", "no, CAS on the flag supersede:Le0")).unwrap();
        assert_eq!(learn_in(&home, Some("s1"), &fake_ask, true, 30), (1, 0, 0));
        assert_eq!(results(&home), [(outcome::CORRECTED.into(), "s1".into(), "Le0".into())]);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_correction_of_a_lesson_that_applied_but_was_not_asked_is_a_miss() {
        let home = temp_home("missed");
        for e in [at("e0", "s0", 1, "ann@x", "use the lock on the flag"), at("e1", "s0", 2, "bob@x", "retry with jitter")] {
            append_jsonl(&home.join("episodes.jsonl"), &e).unwrap();
        }
        learn_in(&home, Some("s0"), &fake_ask, true, 3);
        // s1's check graded both as applying and chose to ask only Le1; the engineer then corrects the
        // drone on Le0's concern: Le0 was needed and not asked.
        let graded = outcome::Outcome {
            kind: outcome::GRADED.into(),
            session: "s1".into(),
            lessons: vec!["Le0".into(), "Le1".into()],
            ms: 10,
            repo: "r".into(),
            grades: vec![2, 3],
            ranks: vec![0, 1],
            applied: 2,
            chosen: vec!["Le1".into()],
            ..Default::default()
        };
        outcome::append(&home.join("outcomes.jsonl"), &graded).unwrap();
        asked(&home, "s1", 11, &["Le1"]);
        append_jsonl(&home.join("episodes.jsonl"), &at("e2", "s1", 20, "cy@x", "no, the lock on the flag merge:Le0")).unwrap();
        assert_eq!(learn_in(&home, Some("s1"), &fake_ask, true, 30), (0, 1, 0));
        let r = results(&home);
        assert!(r.contains(&(outcome::MISSED.into(), "s1".into(), "Le0".into())), "{r:?}");
        // The asked one is not a miss; a second run records nothing new.
        assert!(!r.iter().any(|(k, _, l)| k == outcome::MISSED && l == "Le1"), "{r:?}");
        learn_in(&home, Some("s1"), &fake_ask, true, 40);
        assert_eq!(results(&home).iter().filter(|(k, ..)| k == outcome::MISSED).count(), 1);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn siding_with_one_side_of_a_disagreement_corrects_the_other() {
        let home = temp_home("sides");
        for e in [at("e0", "s0", 1, "ann@x", "use the lock on the flag"), at("e1", "s0", 2, "bob@x", "use CAS on the flag")] {
            append_jsonl(&home.join("episodes.jsonl"), &e).unwrap();
        }
        learn_in(&home, Some("s0"), &fake_ask, true, 3);
        // The conscience rendered Le0 and Le1 as one disagreement; the engineer sides with Le0.
        let o = outcome::Outcome {
            kind: outcome::ASKED.into(),
            session: "s1".into(),
            lessons: vec!["Le0".into(), "Le1".into()],
            ms: 10,
            repo: "r".into(),
            groups: vec![vec!["Le0".into(), "Le1".into()]],
            ..Default::default()
        };
        outcome::append(&home.join("outcomes.jsonl"), &o).unwrap();
        append_jsonl(&home.join("episodes.jsonl"), &at("e2", "s1", 20, "cy@x", "no, the lock on the flag merge:Le0")).unwrap();
        assert_eq!(learn_in(&home, Some("s1"), &fake_ask, true, 30), (0, 1, 0));
        assert_eq!(results(&home), [(outcome::CORRECTED.into(), "s1".into(), "Le1".into()), (outcome::NOT_CORRECTED.into(), "s1".into(), "Le0".into())]);
        // cy joined Le0 at the correction; ann's earlier evidence is not his.
        let l0 = lessons(&home.join("lessons.jsonl")).into_iter().find(|l| l.id == "Le0").unwrap();
        assert_eq!(l0.joined.get("cy@x"), Some(&20));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_superseded_lesson_is_not_a_merge_target_its_successor_is_shown() {
        let lesson = |id: &str, ms: i64, q: &str, sup: &[&str]| Lesson {
            id: id.into(),
            origin_ms: ms,
            repo: "r".into(),
            question: q.into(),
            situations: vec![],
            consequence: "c".into(),
            support: 1,
            episodes: vec![],
            history: vec![],
            version: 1,
            engineers: vec!["ann@x".into()],
            joined: Default::default(),
            area: "concurrency".into(),
            supersedes: sup.iter().map(|s| s.to_string()).collect(),
            source: String::new(),
            scope: vec![],
            overrides: String::new(),
            origin: String::new(),
            facets: vec![],
            reach: String::new(),
        };
        let lib = [lesson("L0", 1, "Is the closed flag guarded by the ReentrantLock?", &[]), lesson("L1", 2, "Would a CAS suffice?", &["L0"])];
        // ann reverts to the lock: the model only sees L1 (in L0's place), contradicts it, and the
        // restored view becomes a new lesson that supersedes L1 and is asked again.
        let shown = std::cell::RefCell::new(String::new());
        let back = generalise(&ep("e9", "no, guard the closed flag with the ReentrantLock"), &lib, &|p| {
            *shown.borrow_mut() = p.to_string();
            Some(json!({"is_lesson": true, "question": "Is the closed flag guarded by the ReentrantLock?", "situations": [],
                "consequence": "c", "same_as": "L0", "area": "concurrency", "supersedes": ["L1"]}))
        })
        .unwrap()
        .unwrap();
        let shown = shown.into_inner();
        assert!(shown.contains("[L1]") && !shown.contains("[L0]"), "{shown}");
        assert_eq!((back.version, back.supersedes.clone()), (1, vec!["L1".to_string()]), "same_as on a hidden lesson is no merge");
        let all: Vec<Challenge> = lib
            .iter()
            .chain([&back])
            .map(|l| Challenge {
                id: l.id.clone(),
                origin_ms: l.origin_ms,
                question: l.question.clone(),
                engineers: l.engineers.clone(),
                supersedes: l.supersedes.clone(),
                ..Default::default()
            })
            .collect();
        assert_eq!(trust::successors(&all).get("L0").map(String::as_str), Some(back.id.as_str()));
    }

    fn temp_home(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-learn-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn at(id: &str, session: &str, ms: i64, engineer: &str, correction: &str) -> Episode {
        Episode { ms, session: session.into(), engineer: engineer.into(), ..ep(id, correction) }
    }

    fn asked(home: &Path, session: &str, ms: i64, ids: &[&str]) {
        let o = outcome::Outcome {
            kind: outcome::ASKED.into(),
            session: session.into(),
            lessons: ids.iter().map(|i| i.to_string()).collect(),
            ms,
            repo: "r".into(),
            ..Default::default()
        };
        outcome::append(&home.join("outcomes.jsonl"), &o).unwrap();
    }

    fn regret_at(id: &str, decision_ms: i64) -> Episode {
        let r = regret::Regret { decision_session: "s-dec".into(), decision_ms, signals: vec!["subject".into()], ..regret::Regret::default() };
        Episode { kind: regret::KIND.into(), regret: Some(r), ..at(id, "s-fix", 100, "bob@x", "Fixed later (3d, subject): fix: take the lock") }
    }

    /// The model's verdict on a regret: a lesson, or (`lesson` false) not one.
    fn regret_ask(lesson: bool) -> impl Fn(&str) -> Option<Value> {
        move |_| {
            Some(json!({"is_lesson": lesson, "question": "Before reading a shared flag, has a writer been considered?", "situations": [],
            "consequence": "a race", "same_as": "", "supersedes": []}))
        }
    }

    #[test]
    fn a_regret_blames_lessons_asked_before_the_decision_once() {
        let regretted = |home: &Path| -> Vec<(String, String, f64, String)> {
            outcome::read(&home.join("outcomes.jsonl"))
                .into_iter()
                .filter(|o| o.kind == outcome::REGRETTED)
                .map(|o| (o.lessons.join(","), o.session, o.credit, o.cause))
                .collect()
        };
        let setup = |name: &str, e: Episode| {
            let home = temp_home(name);
            asked(&home, "s-dec", 10, &["Le0", "Le1"]);
            asked(&home, "s-dec", 50, &["Le2"]);
            append_jsonl(&home.join("episodes.jsonl"), &e).unwrap();
            home
        };
        let home = setup("regret", regret_at("R1", 20));
        assert_eq!(learn_in(&home, None, &regret_ask(true), true, 200), (1, 0, 0));
        let got = regretted(&home);
        assert_eq!(
            got,
            [("Le0".into(), "s-dec".into(), 0.25, "R1".into()), ("Le1".into(), "s-dec".into(), 0.25, "R1".into())],
            "Le2 was asked after the decision"
        );
        let before = std::fs::read_to_string(home.join("outcomes.jsonl")).unwrap();
        learn_in(&home, None, &regret_ask(true), true, 300);
        assert_eq!(std::fs::read_to_string(home.join("outcomes.jsonl")).unwrap(), before, "credited once");
        let _ = std::fs::remove_dir_all(home);
        // Not a lesson, no decision time, not tracking: nothing is blamed.
        let home = setup("regret-no", regret_at("R1", 20));
        learn_in(&home, None, &regret_ask(false), true, 200);
        assert!(regretted(&home).is_empty(), "not a lesson");
        let _ = std::fs::remove_dir_all(home);
        let home = setup("regret-old", regret_at("R1", 0));
        learn_in(&home, None, &regret_ask(true), true, 200);
        assert!(regretted(&home).is_empty(), "a regret from before decision times were kept");
        let _ = std::fs::remove_dir_all(home);
        let home = setup("regret-untracked", regret_at("R1", 20));
        learn_in(&home, None, &regret_ask(true), false, 200);
        assert!(regretted(&home).is_empty(), "BORG_LEARN=off writes nothing");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn records_outcomes_of_asked_lessons() {
        let home = temp_home("outcomes");
        // Session s0 teaches lessons Le0 (TOCTOU) and Le1 (spec).
        for e in [at("e0", "s0", 1, "ann@x", "check TOCTOU on the flag"), at("e1", "s0", 2, "", "check against the spec")] {
            append_jsonl(&home.join("episodes.jsonl"), &e).unwrap();
        }
        assert_eq!(learn_in(&home, Some("s0"), &fake_ask, true, 3), (2, 0, 0));
        assert!(outcome::read(&home.join("outcomes.jsonl")).is_empty(), "nothing asked, nothing settled");
        // s1: both asked at 10; the TOCTOU concern is corrected again at 20 (merged into Le0).
        // Le1 asked again after that correction, which must not count as corrected.
        asked(&home, "s1", 10, &["Le0", "Le1"]);
        append_jsonl(&home.join("episodes.jsonl"), &at("e2", "s1", 20, "bob@x", "flag still racy merge:Le0")).unwrap();
        asked(&home, "s1", 30, &["Le1"]);
        assert_eq!(learn_in(&home, Some("s1"), &fake_ask, true, 40), (0, 1, 0));
        let result: Vec<(String, String, i64)> =
            outcome::read(&home.join("outcomes.jsonl")).into_iter().filter(|o| o.kind != outcome::ASKED).map(|o| (o.kind, o.lessons.join(","), o.ms)).collect();
        assert_eq!(result, [(outcome::CORRECTED.into(), "Le0".into(), 20), (outcome::NOT_CORRECTED.into(), "Le1".into(), 40)]);
        // Re-running learn for the same session appends nothing: deterministic and idempotent.
        let before = std::fs::read_to_string(home.join("outcomes.jsonl")).unwrap();
        learn_in(&home, Some("s1"), &fake_ask, true, 50);
        assert_eq!(std::fs::read_to_string(home.join("outcomes.jsonl")).unwrap(), before);
        // Engineers accumulate on the merged lesson.
        let l0 = lessons(&home.join("lessons.jsonl")).into_iter().find(|l| l.id == "Le0").unwrap();
        assert_eq!(l0.engineers, ["ann@x", "bob@x"]);
        // The same email in another case is the same engineer: not added, no join recorded.
        append_jsonl(&home.join("episodes.jsonl"), &at("e3", "s2", 60, "Ann@X", "flag racy again merge:Le0")).unwrap();
        assert_eq!(learn_in(&home, Some("s2"), &fake_ask, true, 70), (0, 1, 0));
        let l0 = lessons(&home.join("lessons.jsonl")).into_iter().find(|l| l.id == "Le0").unwrap();
        assert_eq!((l0.engineers.as_slice(), l0.joined.get("Ann@X")), (&["ann@x".to_string(), "bob@x".to_string()][..], None));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn idle_sessions_settle_without_their_end_hook() {
        let home = temp_home("idle");
        for e in [at("e0", "s0", 1, "", "check TOCTOU on the flag"), at("e1", "s0", 2, "", "check against the spec")] {
            append_jsonl(&home.join("episodes.jsonl"), &e).unwrap();
        }
        learn_in(&home, Some("s0"), &fake_ask, true, 3);
        // s1 and s3 never send `end`. s1 asks both and is corrected on Le0; s3 asks Le0, no correction.
        asked(&home, "s1", 10, &["Le0", "Le1"]);
        append_jsonl(&home.join("episodes.jsonl"), &at("e2", "s1", 20, "", "flag still racy merge:Le0")).unwrap();
        asked(&home, "s3", 5, &["Le0"]);
        let idle = 20 + outcome::IDLE_MS;
        let results = || -> Vec<(String, String, String)> {
            outcome::read(&home.join("outcomes.jsonl"))
                .into_iter()
                .filter(|o| o.kind != outcome::ASKED)
                .map(|o| (o.kind, o.session, o.lessons.join(",")))
                .collect()
        };
        let not = |s: &str, l: &str| (outcome::NOT_CORRECTED.to_string(), s.to_string(), l.to_string());
        // s2 ends a day later: its own ask settles, and idle s3; s1 not, its episode is still pending.
        asked(&home, "s2", idle - 1, &["Le1"]);
        learn_in(&home, Some("s2"), &fake_ask, true, idle);
        assert_eq!(results(), [not("s3", "Le0"), not("s2", "Le1")]);
        // A plain `borg learn` folds e2 in and settles s1 too, so s1 gives both kinds of outcome.
        assert_eq!(learn_in(&home, None, &fake_ask, true, idle), (0, 1, 0));
        let corrected = (outcome::CORRECTED.to_string(), "s1".to_string(), "Le0".to_string());
        assert_eq!(results(), [not("s3", "Le0"), not("s2", "Le1"), corrected.clone(), not("s1", "Le1")]);
        // A fresh session is not settled, and re-running appends nothing.
        asked(&home, "s4", idle, &["Le1"]);
        learn_in(&home, None, &fake_ask, true, idle + 1);
        assert_eq!(results().len(), 4);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn no_outcomes_written_without_tracking() {
        let home = temp_home("off");
        append_jsonl(&home.join("episodes.jsonl"), &at("e0", "s0", 1, "", "check TOCTOU")).unwrap();
        learn_in(&home, Some("s0"), &fake_ask, true, 2);
        asked(&home, "s1", 10, &["Le0"]);
        let before = std::fs::read_to_string(home.join("outcomes.jsonl")).unwrap();
        append_jsonl(&home.join("episodes.jsonl"), &at("e1", "s1", 20, "", "TOCTOU again merge:Le0")).unwrap();
        assert_eq!(learn_in(&home, Some("s1"), &fake_ask, false, 30), (0, 1, 0));
        assert_eq!(std::fs::read_to_string(home.join("outcomes.jsonl")).unwrap(), before);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn failed_model_calls_leave_episodes_waiting_and_stop_early() {
        let home = temp_home("failed");
        for i in 0..5 {
            append_jsonl(&home.join("episodes.jsonl"), &at(&format!("e{i}"), "s0", i, "", &format!("check case {i}"))).unwrap();
        }
        asked(&home, "s0", 0, &["Lx"]);
        let calls = std::cell::Cell::new(0);
        let down = |_: &str| -> Option<Value> {
            calls.set(calls.get() + 1);
            None
        };
        // Quota hit: nothing is marked "not a lesson", the run gives up after MAX_FAILURES calls,
        // and the session is not settled while its episodes wait.
        assert_eq!(learn_in(&home, Some("s0"), &down, true, 1 + outcome::IDLE_MS), (0, 0, 5));
        assert_eq!(calls.get(), MAX_FAILURES);
        assert!(!home.join("episodes-not-lessons.txt").exists());
        assert!(outcome::read(&home.join("outcomes.jsonl")).iter().all(|o| o.kind == outcome::ASKED));
        // The model is back: the next run retries every episode.
        assert_eq!(learn_in(&home, Some("s0"), &fake_ask, true, 2 + outcome::IDLE_MS), (5, 0, 0));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn area_and_supersedes_come_from_the_same_call() {
        let lib = vec![
            generalise(&ep("e1", "use a lock here"), &[], &|_| {
                Some(json!({"is_lesson": true, "question": "Is the flag guarded by a lock?", "situations": ["flag across threads"],
                    "consequence": "race", "same_as": "", "area": "Concurrency", "supersedes": ["Lnope"]}))
            })
            .unwrap()
            .unwrap(),
        ];
        assert_eq!(lib[0].area, "concurrency", "case-insensitive; normalised");
        assert!(lib[0].supersedes.is_empty(), "an id the model was not shown is dropped");
        // A lesson without an area (older store, or a model that ignores it) is "other".
        let old: Lesson = serde_json::from_str(
            r#"{"id":"L","origin_ms":1,"repo":"r","question":"q","situations":[],"consequence":"c",
            "support":1,"episodes":[],"history":[],"version":1}"#,
        )
        .unwrap();
        assert_eq!((old.area.as_str(), old.supersedes.len()), ("other", 0));
        let id = lib[0].id.clone();
        let reversed = generalise(&ep("e2", "no, the lock is wrong here, use the atomic flag"), &lib, &move |_| {
            Some(json!({"is_lesson": true, "question": "Is a lock-free atomic enough for the flag?", "situations": ["flag across threads"],
                "consequence": "lock contention", "same_as": "", "area": "made-up", "supersedes": [id.clone()]}))
        })
        .unwrap()
        .unwrap();
        assert_eq!((reversed.area.as_str(), reversed.supersedes.clone()), ("other", vec![lib[0].id.clone()]));
        assert!(!serde_json::to_string(&lib[0]).unwrap().contains("supersedes"), "empty supersedes stays off the line");
        // The merge target is never superseded; a merge keeps what the old version superseded.
        let (rid, lid) = (reversed.id.clone(), lib[0].id.clone());
        let merged = generalise(&ep("e3", "again: atomic, not a lock"), &[lib[0].clone(), reversed.clone()], &move |_| {
            Some(json!({"is_lesson": true, "question": "Is a lock-free atomic enough?", "situations": ["flag across threads"],
                "consequence": "lock contention", "same_as": rid.clone(), "area": "concurrency", "supersedes": [rid.clone(), lid.clone()]}))
        })
        .unwrap()
        .unwrap();
        assert_eq!((merged.id.as_str(), merged.version, merged.supersedes.clone()), (reversed.id.as_str(), 2, vec![lib[0].id.clone()]));
    }

    #[test]
    fn new_lesson_then_merge_keeps_history() {
        let first = generalise(&ep("e1", "carefully check TOCTOU on the atomic flags"), &[], &|_| {
            Some(json!({"is_lesson": true, "question": "Is every check-then-act on a shared flag TOCTOU-safe?",
                "situations": ["check-then-act on a shared mutable flag across threads"], "consequence": "race", "same_as": ""}))
        })
        .unwrap()
        .unwrap();
        assert_eq!((first.version, first.support), (1, 1));
        let id = first.id.clone();
        let merged = generalise(&ep("e2", "again: the closed flag check is racy"), std::slice::from_ref(&first), &move |p| {
            assert!(p.contains(&id), "the similar lesson must be offered for merging");
            Some(json!({"is_lesson": true, "question": "Is each check-then-act on shared flags atomic or harmless on race?",
                "situations": ["check-then-act on a shared mutable flag across threads"], "consequence": "race", "same_as": id}))
        })
        .unwrap()
        .unwrap();
        assert_eq!((merged.id.as_str(), merged.version, merged.support, merged.history.len()), (first.id.as_str(), 2, 2, 1));
        let skipped =
            generalise(&ep("e3", "stop"), &[], &|_| Some(json!({"is_lesson": false, "question": "", "situations": [], "consequence": "", "same_as": ""})));
        assert!(matches!(skipped, Ok(None)), "a verdict: not a lesson");
        assert_eq!(generalise(&ep("e4", "no, the lock"), &[], &|_| None).map(|_| ()), Err(AskFailed), "no answer is no verdict");
        let malformed = generalise(&ep("e5", "no, the lock"), &[], &|_| Some(json!({"error": "rate limited"})));
        assert_eq!(malformed.map(|_| ()), Err(AskFailed));
    }
}
