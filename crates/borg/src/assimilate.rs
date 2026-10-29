//! `borg assimilate`: bootstrap the collectives from the engineer's existing history, so Borg is
//! useful on day one instead of after weeks of live capture.
//!
//! Sources are Claude Code transcripts (`~/.claude/projects/<slug>/<session>.jsonl`), omp sessions
//! (`~/.omp/agent/sessions/<slug>/<session>.jsonl`) and opencode 1.x and 2.x sessions (exported through
//! the `opencode` CLI, see `opencode_session` and `cli_sessions`); main sessions only (sub-agents never
//! talk to the engineer, and borg's own model calls are not the engineer either). Each is replayed the
//! way the live hooks would have seen it:
//! - a human prompt that `learn::is_correction` flags becomes an episode; any other prompt becomes the task;
//! - an `is_error` tool result that *starts* as the harness's rejection notice and carries the engineer's
//!   words (`learn::rejection`) becomes an episode; the same words inside ordinary tool output are echoed
//!   history (a script printing an old transcript), not a correction;
//! - a wordless denial (`learn::denied`) waits for the next prompt, which is the engineer's reason; a later
//!   successful tool call ends the wait, as in the hook.
//!
//! Every episode is routed by its own cwd (`collective::route`): an off repo is only counted, never
//! stored; each collective gets its own episodes. The task, the drone's text and a denied tool call
//! last across prompts, and a session can move between repos, so each remembers the cwd it was seen
//! in and is dropped from an episode routed to another collective: the live hook keeps that state per
//! collective and writes nothing in an off repo, so text from an off repo never reaches a store. Anything mentioning a word of `BORG_ASSIMILATE_SKIP`
//! (comma-separated, case-insensitive, default none; for knowledge the team abandoned) is dropped. The default is a dry run that writes nothing; `--apply` appends the new episodes and runs
//! `borg learn` on each collective. Re-running adds nothing: an episode already in the store for the same
//! session within `SAME_MS` (the live hook stamps capture time, the transcript message time) is a duplicate.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::collective::{self, Config, Route};
use crate::harness::opencode;
use crate::learn::{self, Episode, Lesson};
use crate::runner;

/// Two episodes of one session this close together are the same correction. Method: a dedupe
/// window; the live hook's capture time and the transcript's message time of one correction differ
/// by a second or two, two corrections of a session by far more.
const SAME_MS: i64 = 5_000;

/// Its flags and `--from-dir`'s (completions.rs).
pub const FLAGS: &[&str] = &[
    "--apply",
    "--since",
    "--limit",
    "--claude",
    "--omp",
    "--opencode",
    "--from-dir",
    "--plan",
    "--for",
    "--collective",
    "--knowledge",
    "--retros",
    "--families",
    "--decisions",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Claude,
    Omp,
    Opencode,
}

/// Where opencode sessions come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpencodeFrom {
    /// The installed `opencode` CLI (the default when it is on PATH).
    Cli,
    /// A directory of session exports of either version (`--opencode DIR`: tests, offline copies).
    Dir(PathBuf),
}

/// An episode found in a transcript, with the cwd it happened in (for routing) and the cwds its
/// task, drone text and rejected tool call were seen in (they may come from an earlier repo).
#[derive(Debug, Clone)]
pub struct Found {
    pub cwd: String,
    pub task_cwd: String,
    pub agent_cwd: String,
    pub tool_cwd: String,
    pub episode: Episode,
}

pub struct Options {
    pub claude: Option<PathBuf>,
    pub omp: Option<PathBuf>,
    /// None: opencode is not installed.
    pub opencode: Option<OpencodeFrom>,
    /// Only episodes at or after this epoch ms.
    pub since: Option<i64>,
    /// At most this many new episodes (the newest not yet stored, across collectives), so repeated
    /// runs work through the history in batches.
    pub limit: Option<usize>,
    pub apply: bool,
    /// Episodes mentioning any of these (lowercase) are dropped (`skip_words`).
    pub skip: Vec<String>,
}

/// What assimilation found (dry run) or did (`--apply`) for one collective.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tally {
    pub home: PathBuf,
    pub may_sync: bool,
    pub sessions: usize,
    /// Episodes not yet in the store.
    pub episodes: usize,
    /// Episodes already in the store (from an earlier run or the live hook).
    pub known: usize,
    /// Model calls `borg learn` needs: the new episodes plus those already waiting in the store.
    pub calls: usize,
    pub new_lessons: usize,
    pub merged: usize,
    /// Episodes `borg learn` could not get a verdict on (model unavailable); a re-run retries them.
    pub pending: usize,
}

#[derive(Debug, Default)]
pub struct Report {
    pub scanned: usize,
    /// Main sessions read per source; `opencode` is None when it is not installed.
    pub claude: usize,
    pub omp: usize,
    pub opencode: Option<usize>,
    pub collectives: BTreeMap<String, Tally>,
    pub off_sessions: usize,
    pub off_episodes: usize,
}

const NOTICE: &str = "want to proceed with this tool use";
/// The characters of a failed tool call's first line searched for `NOTICE`. Method, not a display:
/// the harness writes its notice at the very start of that line, so past this many characters the same
/// words are the tool's own output (a long first line of a command's output), not a rejection.
const NOTICE_CHARS: usize = 200;

fn human(t: &str) -> bool {
    t.len() > 3 && !t.starts_with('<') && !t.contains("[Request interrupted")
}

fn take(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn tail(s: &str, n: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    c[c.len().saturating_sub(n)..].iter().collect()
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter(|b| b.get("type").is_none_or(|t| t == "text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str).or(b.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Replays one session's events with the live hooks' capture rules.
#[derive(Default)]
struct Replay {
    session: String,
    cwd: String,
    /// Each piece of state with the cwd it was seen in.
    task: (String, String),
    said: (String, String),
    /// The tool call a wordless denial refused, waiting for the engineer's next prompt.
    denied: Option<(String, String)>,
    out: Vec<Found>,
    /// Every cwd the session worked in: where opencode 2's sessions are listed (`cli_sessions`).
    cwds: BTreeSet<String>,
}

impl Replay {
    fn at(&mut self, cwd: &str) {
        if self.cwd != cwd {
            self.cwd = cwd.to_string();
            self.cwds.insert(self.cwd.clone());
        }
    }

    /// `rejected_tool` is the refused call and the cwd it was refused in.
    fn episode(&mut self, ms: i64, rejected_tool: Option<(String, String)>, correction: String) {
        let (rejected_tool, tool_cwd) = match rejected_tool {
            Some((tool, cwd)) => (Some(tool), cwd),
            None => (None, self.cwd.clone()),
        };
        let episode = Episode {
            id: format!("{}-{ms}", take(&self.session, 8)),
            ms,
            repo: String::new(),
            session: self.session.clone(),
            task: self.task.0.clone(),
            agent: self.said.0.clone(),
            rejected_tool,
            correction,
            ..Episode::default()
        };
        self.out.push(Found { cwd: self.cwd.clone(), task_cwd: self.task.1.clone(), agent_cwd: self.said.1.clone(), tool_cwd, episode });
    }

    fn assistant_text(&mut self, t: &str) {
        if !t.trim().is_empty() {
            self.said = (tail(t.trim(), learn::PROPOSAL_CHARS), self.cwd.clone());
        }
    }

    fn prompt(&mut self, ms: i64, t: &str) {
        let t = t.trim();
        if !human(t) {
            return;
        }
        // "borg no <reason>", as the prompt hook reads it (veto.rs): a veto episode that answers a pending
        // wordless denial, never the session's task nor a correction.
        if let Some(reason) = crate::veto::enabled().then(|| crate::veto::reason_in(t)).flatten() {
            self.denied = None;
            self.episode(ms, None, reason);
            if let Some(f) = self.out.last_mut() {
                f.episode.id = format!("no-{}", f.episode.id);
                f.episode.kind = crate::veto::KIND.into();
            }
            return;
        }
        if let Some(tool) = self.denied.take() {
            self.episode(ms, Some(tool), take(t, learn::EXCERPT_CHARS));
            return;
        }
        if learn::is_correction(t) && !self.task.0.is_empty() {
            self.episode(ms, None, take(t, learn::EXCERPT_CHARS));
        } else {
            self.task = (take(t, learn::TASK_CHARS), self.cwd.clone());
        }
    }

    fn tool_result(&mut self, ms: i64, is_error: bool, text: &str, tool: String) {
        if !is_error {
            self.denied = None; // the drone moved on: the next prompt is no longer the reason
            return;
        }
        // Only the harness's own notice, on the first line, is a rejection; the same words further down
        // are tool output (e.g. "Exit code 1" and then an old transcript).
        let first: String = take(text.trim_start().lines().next().unwrap_or_default(), NOTICE_CHARS);
        if let Some(said) = learn::rejection(text).filter(|_| first.contains(NOTICE)) {
            self.episode(ms, Some((tool, self.cwd.clone())), said);
        } else if learn::denied(text) {
            self.denied = Some((tool, self.cwd.clone()));
        }
    }
}

fn tool_text(name: &str, input: &Value) -> String {
    format!("{name} {}", take(&input.to_string(), learn::EXCERPT_CHARS))
}

fn lines(path: &Path) -> impl Iterator<Item = Value> {
    let file = std::fs::File::open(path).ok();
    file.into_iter().flat_map(|f| BufReader::new(f).lines()).map_while(Result::ok).filter_map(|l| serde_json::from_str::<Value>(&l).ok())
}

fn ts(v: &Value) -> i64 {
    v.get("timestamp").and_then(Value::as_str).map_or(0, borg_trace::parse_ts)
}

/// Episodes of one Claude Code transcript (none for a sub-agent).
pub fn claude_session(path: &Path) -> Vec<Found> {
    claude_replay(path).out
}

fn claude_replay(path: &Path) -> Replay {
    let mut r = Replay { session: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), ..Default::default() };
    let mut tools: HashMap<String, String> = HashMap::new();
    for v in lines(path) {
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return Replay::default();
        }
        if let Some(c) = v.get("cwd").and_then(Value::as_str) {
            r.at(c);
        }
        if v.get("isMeta") == Some(&Value::Bool(true)) || v.get("isCompactSummary") == Some(&Value::Bool(true)) {
            continue;
        }
        let Some(msg) = v.get("message") else { continue };
        let content = msg.get("content").unwrap_or(&Value::Null);
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                for b in content.as_array().into_iter().flatten() {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => r.assistant_text(b.get("text").and_then(Value::as_str).unwrap_or_default()),
                        Some("tool_use") => {
                            let name = b.get("name").and_then(Value::as_str).unwrap_or_default();
                            let id = b.get("id").and_then(Value::as_str).unwrap_or_default();
                            tools.insert(id.to_string(), tool_text(name, b.get("input").unwrap_or(&Value::Null)));
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                let results: Vec<&Value> =
                    content.as_array().into_iter().flatten().filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")).collect();
                if results.is_empty() {
                    r.prompt(ts(&v), &text_of(content));
                }
                for b in results {
                    let tool = b.get("tool_use_id").and_then(Value::as_str).and_then(|id| tools.get(id)).cloned().unwrap_or_default();
                    let is_error = b.get("is_error").and_then(Value::as_bool) == Some(true);
                    r.tool_result(ts(&v), is_error, &text_of(b.get("content").unwrap_or(&Value::Null)), tool);
                }
            }
            _ => {}
        }
    }
    r
}

/// Episodes of one omp session (none for a sub-agent session).
pub fn omp_session(path: &Path) -> Vec<Found> {
    omp_replay(path).out
}

fn omp_replay(path: &Path) -> Replay {
    let mut r = Replay { session: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), ..Default::default() };
    let mut tools: HashMap<String, String> = HashMap::new();
    for v in lines(path) {
        match v.get("type").and_then(Value::as_str) {
            Some("session") => {
                if v.get("parentSession").is_some_and(|p| !p.is_null()) {
                    return Replay::default();
                }
                if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                    r.at(c);
                }
                if let Some(id) = v.get("id").and_then(Value::as_str) {
                    r.session = id.to_string();
                }
            }
            Some("message") => {
                let Some(msg) = v.get("message") else { continue };
                let content = msg.get("content").unwrap_or(&Value::Null);
                match msg.get("role").and_then(Value::as_str) {
                    Some("user") => r.prompt(ts(&v), &text_of(content)),
                    Some("assistant") => {
                        let blocks = content.as_array().into_iter().flatten();
                        for b in blocks {
                            match b.get("type").and_then(Value::as_str) {
                                Some("text") => r.assistant_text(b.get("text").and_then(Value::as_str).unwrap_or_default()),
                                Some("toolCall") => {
                                    let name = b.get("name").and_then(Value::as_str).unwrap_or_default();
                                    let id = b.get("id").and_then(Value::as_str).unwrap_or_default();
                                    tools.insert(id.to_string(), tool_text(name, b.get("arguments").unwrap_or(&Value::Null)));
                                }
                                _ => {}
                            }
                        }
                    }
                    Some("toolResult") => {
                        let tool = msg.get("toolCallId").and_then(Value::as_str).and_then(|id| tools.get(id)).cloned().unwrap_or_default();
                        let is_error = msg.get("isError").and_then(Value::as_bool) == Some(true);
                        r.tool_result(ts(&v), is_error, &text_of(content), tool);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    r
}

// ---- opencode: one parser for both export shapes, normalised into `Step`s at one point.
//
// 1.x `opencode export`: {info: {id, directory, parentID?, title}, messages: [{info: {role, time}, parts}]}.
// 2.x `opencode session export`: {info: {id, location: {directory}, parentID?, title}, messages:
// [{type: user|synthetic|assistant|idle, time, text | content}]}.

/// opencode 1.x: a permission the engineer rejected without words (it halts the turn).
const OC_REJECTED: &str = "The user rejected permission to use this specific tool call.";
/// opencode 1.x: a rejection with the engineer's words after it.
const OC_FEEDBACK: &str = "The user rejected permission to use this specific tool call with the following feedback:";
/// opencode 2.x (`aborted`): a call the engineer declined without words.
const OC_DECLINED: &str = "The user declined this tool call";
/// opencode 2.x (`permission.rejected`): `opencode run` cannot ask, so it rejects every ask by itself.
/// It shares its type with the engineer's feedback but is nobody's correction.
const OC_AUTO_REJECTED: &str = "This non-interactive run cannot ask the user for permission, so the request was rejected. Continue without this action.";
/// Claude's wordless rejection, which `learn::denied` matches.
const DENIED: &str = "The user doesn't want to proceed with this tool use.";

/// An opencode tool error in Claude's wording, so the replay (`learn::rejection`, `learn::denied`)
/// reads it as the hook reads Claude's; the adapter's `canonicalError` does the same live. `kind` is
/// 2.x's `error.type` (1.x errors are bare strings). Every other error comes back as it was and is no
/// correction: a rule that denied the call, a dismissed question, an abort, a failed tool.
fn canonical(kind: Option<&str>, message: &str) -> String {
    let m = message.trim();
    let said = |words: &str| {
        format!("The user doesn't want to proceed with this tool use. The tool use was rejected. To tell you how to proceed, the user said:\n{words}")
    };
    match kind {
        None if m == OC_REJECTED => DENIED.into(),
        None => m.strip_prefix(OC_FEEDBACK).map_or_else(|| m.to_string(), |w| said(w.trim())),
        Some("aborted") if m == OC_DECLINED => DENIED.into(),
        Some("permission.rejected") if m == OC_AUTO_REJECTED => m.into(),
        Some("permission.rejected") if m.is_empty() => DENIED.into(),
        Some("permission.rejected") => said(m),
        _ => m.into(),
    }
}

/// `opencode run "fix it"` stores one argument that holds whitespace wrapped in literal double
/// quotes, its inner quotes backslash-escaped (1.x and 2.x); the engineer's prompt is inside. A text
/// with an unescaped quote inside is not such an argument and stays as it is.
fn unquote(t: &str) -> Cow<'_, str> {
    let Some(inner) = t.trim().strip_prefix('"').and_then(|s| s.strip_suffix('"')) else { return t.into() };
    if inner.match_indices('"').any(|(i, _)| !inner[..i].ends_with('\\')) {
        return t.into();
    }
    inner.replace("\\\"", "\"").into()
}

/// What the replay needs of one opencode message, either version.
#[derive(Debug, PartialEq)]
enum Step {
    Prompt(i64, String),
    Said(String),
    /// A finished tool call: its time, its error in Claude's wording (None: it completed), the call.
    Tool(i64, Option<String>, String),
}

/// A finished tool call: 1.x names it `tool`, 2.x `name`; 1.x errors are strings, 2.x `{type, message}`.
/// None while it is still pending or running. `ms` is when it ended (1.x `state.time.end`, 2.x the
/// item's `time.completed`), else its message's time: a rejection happens when the engineer answers,
/// often long after the message began, and two episodes of a session within `SAME_MS` are one.
fn tool_step(ms: i64, name: &Value, state: &Value) -> Option<Step> {
    let tool = tool_text(name.as_str().unwrap_or_default(), &state["input"]);
    let (kind, message) = match &state["error"] {
        Value::String(s) => (None, s.as_str()),
        e => (e["type"].as_str(), e["message"].as_str().unwrap_or_default()),
    };
    match state["status"].as_str() {
        Some("completed") => Some(Step::Tool(ms, None, tool)),
        Some("error") => Some(Step::Tool(ms, Some(canonical(kind, message)), tool)),
        _ => None,
    }
}

/// The steps of one message: the one place the two export shapes differ.
/// - 1.x: the engineer's words are the text parts not marked `synthetic` (borg's prompt context, a
///   compaction's "Continue", an attached file's contents, editor context) or `ignored`; a message
///   with a part carrying `metadata.borg` is borg's own question coming back. When the engineer
///   rejects a call with words, 1.x rejects every other pending call of the message with the plain
///   `OC_REJECTED`: that cascade is nobody's denial and is dropped (the adapter does the same live).
/// - 2.x: a `user` message's `text`, unless it carries `metadata.borg`; `synthetic` (borg's context,
///   hidden inputs) and `idle` messages are nobody speaking.
fn opencode_steps(m: &Value) -> Vec<Step> {
    let spoken = |p: &&Value| p["type"] == "text" && p["synthetic"] != true && p["ignored"] != true;
    let text = |p: &Value| p["text"].as_str().unwrap_or_default().to_string();
    if let Some(info) = m.get("info") {
        let ms = info["time"]["created"].as_i64().unwrap_or(0);
        let parts = m["parts"].as_array().map(Vec::as_slice).unwrap_or_default();
        let error = |p: &Value| p["state"]["error"].as_str().unwrap_or_default().trim().to_string();
        let worded = parts.iter().any(|p| error(p).starts_with(OC_FEEDBACK));
        return match info["role"].as_str() {
            Some("user") if !parts.iter().any(|p| p["metadata"]["borg"] == true) => {
                vec![Step::Prompt(ms, parts.iter().filter(spoken).map(text).collect::<Vec<_>>().join("\n"))]
            }
            Some("assistant") => by_completion(
                parts
                    .iter()
                    .filter_map(|p| match p["type"].as_str() {
                        Some("tool") if worded && error(p) == OC_REJECTED => None,
                        Some("tool") => tool_step(p["state"]["time"]["end"].as_i64().unwrap_or(ms), &p["tool"], &p["state"]),
                        _ => spoken(&p).then(|| Step::Said(text(p))),
                    })
                    .collect(),
            ),
            _ => Vec::new(),
        };
    }
    let ms = m["time"]["created"].as_i64().unwrap_or(0);
    match m["type"].as_str() {
        Some("user") if m["metadata"]["borg"] != true => vec![Step::Prompt(ms, text(m))],
        Some("assistant") => by_completion(
            m["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| match c["type"].as_str() {
                    Some("text") => Some(Step::Said(text(c))),
                    Some("tool") => tool_step(c["time"]["completed"].as_i64().unwrap_or(ms), &c["name"], &c["state"]),
                    _ => None,
                })
                .collect(),
        ),
        _ => Vec::new(),
    }
}

/// Each run of tool calls in the order they finished, text keeping its place. A message lists
/// parallel calls in the order they were made, and one that waits for the engineer's permission
/// often finishes last; the live hook hears them finish, so a sibling that completed first does
/// not end the wait for a reason that a later wordless rejection starts.
fn by_completion(mut steps: Vec<Step>) -> Vec<Step> {
    for run in steps.split_mut(|s| matches!(s, Step::Said(_))) {
        run.sort_by_key(|s| match s {
            Step::Tool(ms, ..) => *ms,
            _ => 0,
        });
    }
    steps
}

/// A session the engineer talked in: not a sub-agent's (`parentID`; its first prompt is the parent
/// drone's task) and not one of borg's own model calls (`runner::OPENCODE_TITLE`).
fn engineers_session(export: &Value) -> bool {
    let info = &export["info"];
    info.is_object() && info.get("parentID").is_none_or(Value::is_null) && info["title"] != runner::OPENCODE_TITLE
}

/// Episodes of one opencode session export, 1.x or 2.x (none for a sub-agent or a model call of borg's).
/// Like the live hook, a wordless 1.x rejection cannot be told from 1.x `opencode run`'s own
/// auto-reject (the same `OC_REJECTED`): a session continued with `run -c` takes its next prompt
/// for the reason of a refusal nobody made.
pub fn opencode_session(export: &Value) -> Vec<Found> {
    if !engineers_session(export) {
        return Vec::new();
    }
    let info = &export["info"];
    let mut r = Replay { session: info["id"].as_str().unwrap_or_default().to_string(), ..Default::default() };
    r.at(info["directory"].as_str().or(info["location"]["directory"].as_str()).unwrap_or_default());
    for step in export["messages"].as_array().into_iter().flatten().flat_map(opencode_steps) {
        match step {
            Step::Prompt(ms, t) => r.prompt(ms, &unquote(&t)),
            Step::Said(t) => r.assistant_text(&t),
            Step::Tool(ms, error, tool) => r.tool_result(ms, error.is_some(), error.as_deref().unwrap_or_default(), tool),
        }
    }
    r.out
}

/// One opencode CLI call: (arguments, the directory it runs in) -> stdout. `opencode::cli` in production.
pub type Exec<'a> = &'a dyn Fn(&[String], &Path) -> Option<Vec<u8>>;

fn strings(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|x| x.to_string()).collect()
}

/// opencode 1.x's read-only query for every main session that is not one of borg's model calls.
fn db_args(since: Option<i64>) -> Vec<String> {
    let since = since.map(|ms| format!(" AND time_updated >= {ms}")).unwrap_or_default();
    let title = runner::OPENCODE_TITLE.replace('\'', "''");
    let sql = format!("SELECT id FROM session WHERE parent_id IS NULL AND title != '{title}'{since} ORDER BY time_created");
    let mut a = strings(&["db", "--pure", "--format", "json"]);
    a.push(sql);
    a
}

/// The main sessions of the project the call runs in. 2.x always `--standalone`: a private server,
/// so the engineer's background service is never started or asked.
fn list_args(v2: bool) -> Vec<String> {
    strings(&["session", "list", if v2 { "--standalone" } else { "--pure" }, "--format", "json", "-n", "100000"])
}

/// The session ids in a JSON array of rows (`opencode db`, `opencode session list`), without borg's
/// model calls and sessions last updated before `since`. None: no such array (the call failed).
fn ids_of(out: &[u8], since: Option<i64>) -> Option<Vec<String>> {
    let rows: Vec<Value> = serde_json::from_slice(out).ok()?;
    let recent = |r: &Value| since.is_none_or(|s| r["updated"].as_i64().is_none_or(|u| u >= s));
    Some(rows.iter().filter(|r| r["title"] != runner::OPENCODE_TITLE && recent(r)).filter_map(|r| Some(r["id"].as_str()?.to_string())).collect())
}

/// Where the collectives config says the engineer works: each collective path and the git repos up
/// to two levels inside it. A path is a prefix, often a folder of repos (`~/work-repos`), and
/// opencode 2 lists sessions per project (a folder outside git is a project of its own), so the
/// folder alone would list none of its repos' sessions.
fn collective_dirs(cfg: Option<&Config>) -> BTreeSet<PathBuf> {
    cfg.into_iter().flat_map(|c| &c.collectives).flat_map(|c| &c.paths).flat_map(|p| with_repos(collective::expand(p))).collect()
}

fn with_repos(dir: PathBuf) -> Vec<PathBuf> {
    let repo = |d: &Path| d.join(".git").exists();
    let subdirs = |d: &Path| -> Vec<PathBuf> { std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect() };
    let mut out = Vec::new();
    if !dir.ancestors().any(repo) {
        for child in subdirs(&dir) {
            match repo(&child) {
                true => out.push(child),
                false => out.extend(subdirs(&child).into_iter().filter(|g| repo(g))),
            }
        }
    }
    out.push(dir);
    out
}

/// One directory per opencode project among `dirs` (a project is a git repo; a dir outside any repo
/// stands for itself), existing ones only.
fn projects(dirs: &BTreeSet<PathBuf>) -> Vec<PathBuf> {
    let root = |d: &Path| d.ancestors().find(|a| a.join(".git").exists()).unwrap_or(d).to_path_buf();
    dirs.iter().filter(|d| d.is_dir()).map(|d| root(d)).collect::<BTreeSet<_>>().into_iter().collect()
}

/// The exports of the engineer's opencode sessions, through opencode's own CLI: read-only, never its
/// database file, never its HTTP API. 1.x lists every main session with one `opencode db` query.
/// 2.x has no such query and lists per project, so it sees the projects in `known` only (the cwds of
/// this run's Claude Code and omp sessions and `collective_dirs`); so does 1.x when its query fails
/// (an older 1.x without `db`, a changed schema). Each session is exported in the directory it was
/// listed in and handed to `each`; a call that fails or answers no JSON is skipped.
///
/// 2.x does not list the sessions it migrated from 1.x: it files them under the project `global`,
/// and a listing in their own directory (now a project of its own) does not show them; no
/// directory is known whose listing does. 2.x keeps the 1.x tables, so a 1.x `opencode` still
/// lists and exports them; their exports go in through `--opencode DIR`.
fn cli_sessions(v2: bool, since: Option<i64>, known: &BTreeSet<PathBuf>, exec: Exec, each: &mut dyn FnMut(&Value)) {
    let tmp = std::env::temp_dir();
    if v2 {
        eprintln!(
            "borg assimilate: opencode 2 lists sessions per known project only, and never the ones it migrated from 1.x (export those with a 1.x opencode into a directory and pass --opencode DIR)"
        );
    }
    let queried = if v2 { None } else { exec(&db_args(since), &tmp).and_then(|o| ids_of(&o, since)) };
    let listed: Vec<(String, PathBuf)> = match queried {
        Some(ids) => ids.into_iter().map(|id| (id, tmp.clone())).collect(),
        None => {
            if !v2 {
                eprintln!("borg assimilate: `opencode db` failed; reading the opencode sessions of known projects only");
            }
            let mut seen = HashSet::new();
            let mut out = Vec::new();
            for dir in projects(known) {
                for id in exec(&list_args(v2), &dir).and_then(|o| ids_of(&o, since)).unwrap_or_default() {
                    if seen.insert(id.clone()) {
                        out.push((id, dir.clone()));
                    }
                }
            }
            out
        }
    };
    if !listed.is_empty() {
        eprintln!("borg assimilate: exporting {} opencode sessions", listed.len());
    }
    for (id, dir) in listed {
        if let Some(v) = exec(&opencode::export_args(v2, &id), &dir).and_then(|o| serde_json::from_slice::<Value>(&o).ok()) {
            each(&v);
        }
    }
}

/// The `*.json` exports in `dir`, in name order; a file that is no JSON is skipped.
fn dir_sessions(dir: &Path, each: &mut dyn FnMut(&Value)) {
    let mut files: Vec<PathBuf> =
        std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    files.sort();
    for f in files {
        if let Some(v) = std::fs::read(&f).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            each(&v);
        }
    }
}

/// `<root>/<slug>/<session>.jsonl`, one level deep: deeper files are sub-agents.
fn session_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for slug in std::fs::read_dir(root).into_iter().flatten().flatten() {
        if !slug.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        for f in std::fs::read_dir(slug.path()).into_iter().flatten().flatten() {
            let p = f.path();
            if f.file_type().is_ok_and(|t| t.is_file()) && p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Words whose mention keeps a session's text or a document out of assimilation, from
/// `BORG_ASSIMILATE_SKIP` (comma-separated, case-insensitive; default none): knowledge the team abandoned.
pub fn skip_words() -> Vec<String> {
    std::env::var("BORG_ASSIMILATE_SKIP").unwrap_or_default().split(',').map(|w| w.trim().to_lowercase()).filter(|w| !w.is_empty()).collect()
}

/// Whether `text` mentions any of `words` (lowercase).
pub fn mentions(text: &str, words: &[String]) -> bool {
    !words.is_empty() && {
        let t = text.to_lowercase();
        words.iter().any(|w| t.contains(w.as_str()))
    }
}

fn obsolete(f: &Found, words: &[String]) -> bool {
    let e = &f.episode;
    [&f.cwd, &f.task_cwd, &f.agent_cwd, &f.tool_cwd, &e.task, &e.agent, &e.correction, e.rejected_tool.as_deref().unwrap_or_default()]
        .iter()
        .any(|s| mentions(s, words))
}

fn read_jsonl<T: for<'de> serde::Deserialize<'de>>(path: &Path) -> Vec<T> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Episodes in a store that `borg learn` has not folded into a lesson or skipped yet.
fn waiting(home: &Path) -> usize {
    let done: HashSet<String> = learn::lessons(&home.join("lessons.jsonl")).into_iter().flat_map(|l: Lesson| l.episodes).collect();
    let skipped: HashSet<String> = std::fs::read_to_string(home.join("episodes-not-lessons.txt")).unwrap_or_default().lines().map(str::to_string).collect();
    read_jsonl::<Episode>(&home.join("episodes.jsonl")).iter().filter(|e| !done.contains(&e.id) && !skipped.contains(&e.id)).count()
}

fn repo_of(cwd: &str) -> String {
    borg_trace::repo_key(cwd).rsplit('/').next().unwrap_or_default().to_string()
}

/// Mine the sources, route every episode and, with `apply`, record and learn them per collective.
/// `cfg` and `base` are the collectives config and the root store (`BORG_HOME`); `ask` is the model.
pub fn run(opts: &Options, cfg: Option<&Config>, base: &Path, ask: &dyn Fn(&str) -> Option<Value>) -> Report {
    let mut report = Report::default();
    let mut found: Vec<Found> = Vec::new();
    // Where the engineer works: opencode 2 can only list sessions per project.
    let mut known = collective_dirs(cfg);
    for (root, source) in [(&opts.claude, Source::Claude), (&opts.omp, Source::Omp)] {
        let Some(root) = root else { continue };
        for f in session_files(root) {
            let r = match source {
                Source::Claude => claude_replay(&f),
                _ => omp_replay(&f),
            };
            *(if source == Source::Claude { &mut report.claude } else { &mut report.omp }) += 1;
            known.extend(r.cwds.iter().map(PathBuf::from));
            found.extend(r.out);
        }
    }
    if let Some(from) = &opts.opencode {
        let mut n = 0;
        let mut each = |v: &Value| {
            if engineers_session(v) {
                n += 1;
                found.extend(opencode_session(v));
            }
        };
        match from {
            OpencodeFrom::Dir(d) => dir_sessions(d, &mut each),
            OpencodeFrom::Cli => {
                let v2 = runner::opencode() == runner::Opencode::V2;
                cli_sessions(v2, opts.since, &known, &|a, d| opencode::cli(v2, None, a, d), &mut each)
            }
        }
        report.opencode = Some(n);
    }
    report.scanned = report.claude + report.omp + report.opencode.unwrap_or(0);
    found.retain(|f| !obsolete(f, &opts.skip) && opts.since.is_none_or(|s| f.episode.ms >= s));
    found.sort_by_key(|f| f.episode.ms);

    // Route each episode by its own cwd; off episodes are counted and dropped here.
    let collective_of = |cwd: &str| match collective::route(cfg, cwd, base) {
        Route::Off => None,
        Route::Collective { name, .. } => Some(name),
    };
    let mut by: BTreeMap<String, (PathBuf, bool, Vec<Found>)> = BTreeMap::new();
    let mut off_sessions = HashSet::new();
    for mut f in found {
        match collective::route(cfg, &f.cwd, base) {
            Route::Off => {
                report.off_episodes += 1;
                off_sessions.insert(f.episode.session.clone());
            }
            r @ Route::Collective { .. } => {
                let may = collective::may_sync(&r);
                let Route::Collective { name, home, .. } = r else { unreachable!() };
                // State carried over from another collective's (or an off) repo stays there.
                let here = Some(name.as_str());
                if collective_of(&f.task_cwd).as_deref() != here {
                    f.episode.task.clear();
                }
                if collective_of(&f.agent_cwd).as_deref() != here {
                    f.episode.agent.clear();
                }
                if collective_of(&f.tool_cwd).as_deref() != here {
                    f.episode.rejected_tool = None;
                }
                by.entry(name).or_insert_with(|| (home, may, Vec::new())).2.push(f);
            }
        }
    }
    report.off_sessions = off_sessions.len();

    // Episodes not in their collective's store yet; the rest are counted as known.
    let mut tallies: BTreeMap<String, (Tally, Vec<Found>)> = BTreeMap::new();
    for (name, (home, may_sync, eps)) in by {
        let mut known: HashMap<String, Vec<i64>> = HashMap::new();
        for e in read_jsonl::<Episode>(&home.join("episodes.jsonl")) {
            known.entry(e.session).or_default().push(e.ms);
        }
        let mut t = Tally { home, may_sync, ..Default::default() };
        let mut fresh = Vec::new();
        for f in eps {
            let seen = known.entry(f.episode.session.clone()).or_default();
            if seen.iter().any(|ms| (ms - f.episode.ms).abs() < SAME_MS) {
                t.known += 1;
                continue;
            }
            seen.push(f.episode.ms);
            fresh.push(f);
        }
        tallies.insert(name, (t, fresh));
    }

    // --limit keeps the newest new episodes across collectives, so the next run takes the next batch.
    if let Some(n) = opts.limit {
        let mut newest: Vec<(i64, String, usize)> =
            tallies.iter().flat_map(|(name, (_, fresh))| fresh.iter().enumerate().map(move |(i, f)| (f.episode.ms, name.clone(), i))).collect();
        newest.sort_by_key(|(ms, ..)| std::cmp::Reverse(*ms));
        let keep: HashSet<(String, usize)> = newest.into_iter().take(n).map(|(_, name, i)| (name, i)).collect();
        for (name, (_, fresh)) in tallies.iter_mut() {
            let mut i = 0;
            fresh.retain(|_| {
                i += 1;
                keep.contains(&(name.clone(), i - 1))
            });
        }
    }

    let mut engineers: HashMap<String, String> = HashMap::new();
    for (name, (mut t, mut fresh)) in tallies {
        let ep_path = t.home.join("episodes.jsonl");
        t.sessions = fresh.iter().map(|f| &f.episode.session).collect::<HashSet<_>>().len();
        t.episodes = fresh.len();
        t.calls = fresh.len() + waiting(&t.home);
        if opts.apply {
            if !fresh.is_empty() && append(&ep_path, &mut fresh, &mut engineers).is_err() {
                eprintln!("borg assimilate: cannot write {}", ep_path.display());
            }
            eprintln!("learning {name}: one model call per episode, in order (each sees the lessons before it)");
            (t.new_lessons, t.merged, t.pending) = learn::learn_in_reporting(&t.home, None, ask, learn::learning(), learn::now_ms(), &mut learn::print_step);
            crate::vectors::refresh(&t.home.join("lessons.jsonl"));
        }
        report.collectives.insert(name, t);
    }
    report
}

/// Complete the episodes (repo, engineer) and append them to the store.
fn append(path: &Path, eps: &mut [Found], engineers: &mut HashMap<String, String>) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    for x in eps {
        x.episode.repo = repo_of(&x.cwd);
        x.episode.engineer = engineers.entry(x.cwd.clone()).or_insert_with(|| learn::engineer(&x.cwd)).clone();
        writeln!(f, "{}", serde_json::to_string(&x.episode)?)?;
        // An imported correction counts at the time it was made, an imported veto as a `veto` (veto.rs
        // `record`; the lessons shown then are not known, so nothing is credited).
        if let Some(store) = path.parent() {
            learn::count_correction(store, &x.episode);
            if x.episode.kind == crate::veto::KIND {
                crate::stats::count(store, crate::stats::VETO, &x.episode.session, |ev| {
                    ev.ms = x.episode.ms;
                    ev.engineer.clone_from(&x.episode.engineer);
                    ev.repo.clone_from(&x.episode.repo);
                    ev.why = "prompt".into();
                    ev.text = crate::stats::excerpt(&x.episode.correction, crate::stats::FEED_CHARS);
                });
            }
        }
    }
    Ok(())
}

pub fn render(r: &Report, apply: bool) -> String {
    let mut s = format!("{} sessions scanned{}\n", r.scanned, if apply { "" } else { " (dry run: nothing written; --apply to assimilate)" });
    // Per source, so a source that found nothing (or is not installed) shows instead of going silent.
    let opencode = r.opencode.map_or("opencode not installed".to_string(), |n| format!("opencode {n}"));
    s.push_str(&format!("sources: claude {} sessions, omp {}, {opencode}\n", r.claude, r.omp));
    for (name, t) in &r.collectives {
        let sync = if t.may_sync { "team" } else { "local, never syncs" };
        s.push_str(&format!("{name} ({sync}) {}: {} new episodes from {} sessions, {} already known", t.home.display(), t.episodes, t.sessions, t.known));
        if apply {
            s.push_str(&format!("; {} new lessons, {} merged", t.new_lessons, t.merged));
            if t.pending > 0 {
                s.push_str(&format!("; {} still pending (model unavailable), re-run to retry", t.pending));
            }
            s.push('\n');
        } else {
            s.push_str(&format!("; ~{} model calls to learn\n", t.calls));
        }
    }
    s.push_str(&format!("off: {} episodes in {} sessions skipped (not stored)\n", r.off_episodes, r.off_sessions));
    if !apply {
        // Last line, where it is read: the first line's note is easy to miss.
        s.push_str("Dry run: nothing was written. Run `borg assimilate --apply` to learn these episodes.\n");
    }
    s
}

/// `YYYY-MM-DD` (or a full RFC 3339 UTC time) to epoch ms.
fn parse_date(d: &str) -> Option<i64> {
    let full = if d.len() == 10 { format!("{d}T00:00:00Z") } else { d.to_string() };
    let ms = borg_trace::parse_ts(&full);
    (ms > 0).then_some(ms)
}

const USAGE: &str = "usage: borg assimilate [--apply] [--claude DIR] [--omp DIR] [--opencode DIR] [--since YYYY-MM-DD] [--limit N]";

/// `borg assimilate [--apply] [--claude DIR] [--omp DIR] [--opencode DIR] [--since DATE] [--limit N]`;
/// the exit code. opencode sessions come from the `opencode` CLI when it is installed, or from
/// `--opencode DIR`, a directory of session exports.
pub fn main(args: &[&str], ask: &dyn Fn(&str) -> Option<Value>) -> i32 {
    let flag = |f: &str| args.iter().position(|a| *a == f).and_then(|i| args.get(i + 1)).copied();
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let dir = |f: &str, default: &str| Some(flag(f).map(PathBuf::from).unwrap_or_else(|| home.join(default)));
    let since = match flag("--since").map(parse_date) {
        Some(None) => {
            eprintln!("borg assimilate: --since wants YYYY-MM-DD\n{USAGE}");
            return 2;
        }
        s => s.flatten(),
    };
    let limit = match flag("--limit").map(str::parse::<usize>) {
        Some(Err(_)) => {
            eprintln!("borg assimilate: --limit wants a number\n{USAGE}");
            return 2;
        }
        l => l.and_then(Result::ok),
    };
    let opts = Options {
        claude: dir("--claude", ".claude/projects"),
        omp: dir("--omp", ".omp/agent/sessions"),
        opencode: flag("--opencode").map(|d| OpencodeFrom::Dir(d.into())).or_else(|| runner::on_path("opencode").then_some(OpencodeFrom::Cli)),
        since,
        limit,
        apply: args.contains(&"--apply"),
        skip: skip_words(),
    };
    // A broken config routes everything off, exactly as in the hook.
    let cfg = match collective::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("borg assimilate: invalid collectives config ({e}): every repo is off, nothing to assimilate");
            return 1;
        }
    };
    let report = run(&opts, cfg.as_ref(), &learn::home(), ask);
    print!("{}", render(&report, opts.apply));
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-assimilate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(path: &Path, rows: &[Value]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, rows.iter().map(|r| r.to_string() + "\n").collect::<String>()).unwrap();
    }

    fn user(cwd: &str, t: &str, text: &str) -> Value {
        json!({"type": "user", "cwd": cwd, "timestamp": t, "message": {"role": "user", "content": text}})
    }

    fn says(cwd: &str, t: &str, text: &str) -> Value {
        json!({"type": "assistant", "cwd": cwd, "timestamp": t, "message": {"content": [{"type": "text", "text": text}]}})
    }

    fn tool_use(cwd: &str, t: &str, id: &str) -> Value {
        json!({"type": "assistant", "cwd": cwd, "timestamp": t,
            "message": {"content": [{"type": "tool_use", "id": id, "name": "Bash", "input": {"command": "rm -rf build"}}]}})
    }

    fn result(cwd: &str, t: &str, id: &str, is_error: bool, text: &str) -> Value {
        json!({"type": "user", "cwd": cwd, "timestamp": t,
            "message": {"content": [{"type": "tool_result", "tool_use_id": id, "is_error": is_error, "content": text}]}})
    }

    /// The model: every episode is a new lesson.
    fn ask(p: &str) -> Option<Value> {
        let c = p.split("## Engineer's correction\n").nth(1)?.lines().next()?.to_string();
        Some(json!({"is_lesson": true, "question": format!("Q {c}"), "situations": [], "consequence": "c", "same_as": ""}))
    }

    const CFG: &str = r#"{ "default": "off", "off": ["/w/secret"],
        "collectives": [ { "name": "corp", "paths": ["/w/corp"] }, { "name": "mine", "paths": ["/w/mine"], "share": "local" } ] }"#;

    /// A history with: a correction in a corp repo, a rejection with words in a private repo, a
    /// correction in an off repo, and tool output that merely echoes an old rejection.
    fn fixture(root: &Path) -> (PathBuf, PathBuf) {
        let (claude, omp) = (root.join("claude"), root.join("omp"));
        let c = "/w/corp/app";
        write(
            &claude.join("-w-corp-app/s-corp-1.jsonl"),
            &[
                user(c, "2026-09-01T10:00:00.000Z", "add a retry to the relay"),
                says(c, "2026-09-01T10:00:05.000Z", "I'll wrap delivery in a ReentrantLock."),
                user(c, "2026-09-01T10:01:00.000Z", "no, check the TOCTOU on the closed flag first"),
                tool_use(c, "2026-09-01T10:02:00.000Z", "t1"),
                // A script printing an old transcript: the rejection words are output, not a rejection.
                result(
                    c,
                    "2026-09-01T10:02:01.000Z",
                    "t1",
                    true,
                    "Exit code 1\nold log: The user doesn't want to proceed with this tool use. the user said: never touch prod",
                ),
                tool_use(c, "2026-09-01T10:03:00.000Z", "t2"),
                result(c, "2026-09-01T10:03:01.000Z", "t2", false, "The user doesn't want to proceed with this tool use. the user said: use the staging db"),
            ],
        );
        let m = "/w/mine/notes";
        write(
            &claude.join("-w-mine-notes/s-mine-1.jsonl"),
            &[
                user(m, "2026-09-02T09:00:00.000Z", "clean the build"),
                says(m, "2026-09-02T09:00:01.000Z", "Removing build/."),
                tool_use(m, "2026-09-02T09:00:02.000Z", "t1"),
                result(
                    m,
                    "2026-09-02T09:00:03.000Z",
                    "t1",
                    true,
                    "The user doesn't want to proceed with this tool use. The tool use was rejected. the user said:\nkeep the cache dir\nNote: The user's next message may contain a correction",
                ),
            ],
        );
        let s = "/w/secret/diary";
        write(
            &claude.join("-w-secret-diary/s-off-1.jsonl"),
            &[
                user(s, "2026-09-03T09:00:00.000Z", "summarise the week"),
                says(s, "2026-09-03T09:00:01.000Z", "Here is a summary."),
                user(s, "2026-09-03T09:01:00.000Z", "no, don't mention the doctor"),
            ],
        );
        // Sub-agents are never the engineer talking.
        write(
            &claude.join("-w-corp-app/s-corp-1/subagents/a.jsonl"),
            &[user(c, "2026-09-01T10:00:00.000Z", "task"), user(c, "2026-09-01T10:00:09.000Z", "no, wrong file")],
        );
        // omp: a wordless denial, then the engineer's reason as the next prompt.
        write(
            &omp.join("-w-corp-svc/2026-09-04_x.jsonl"),
            &[
                json!({"type": "session", "id": "omp-sess-1", "cwd": "/w/corp/svc", "timestamp": "2026-09-04T08:00:00.000Z"}),
                json!({"type": "message", "timestamp": "2026-09-04T08:00:01.000Z", "message": {"role": "user", "content": [{"type": "text", "text": "bump the dependency"}]}}),
                json!({"type": "message", "timestamp": "2026-09-04T08:00:02.000Z", "message": {"role": "assistant", "content": [
                {"type": "text", "text": "Running cargo update."}, {"type": "toolCall", "id": "c1", "name": "bash", "arguments": {"command": "cargo update"}}]}}),
                json!({"type": "message", "timestamp": "2026-09-04T08:00:03.000Z", "message": {"role": "toolResult", "toolCallId": "c1", "isError": true,
                "content": [{"type": "text", "text": "Tool call denied by user: bash"}]}}),
                json!({"type": "message", "timestamp": "2026-09-04T08:00:09.000Z", "message": {"role": "user", "content": [{"type": "text", "text": "only the one crate, pin the rest"}]}}),
                json!({"type": "message", "timestamp": "2026-09-04T08:01:00.000Z", "message": {"role": "user", "content": [{"type": "text", "text": "no, the OldProject client too"}]}}),
            ],
        );
        (claude, omp)
    }

    fn opts(claude: &Path, omp: &Path, apply: bool) -> Options {
        Options { claude: Some(claude.into()), omp: Some(omp.into()), opencode: None, since: None, limit: None, apply, skip: vec!["oldproject".into()] }
    }

    fn files(dir: &Path) -> Vec<PathBuf> {
        borg_trace::jsonl_files(dir)
    }

    #[test]
    fn echoed_rejection_in_tool_output_is_not_a_correction() {
        let root = temp("echo");
        let (claude, _) = fixture(&root);
        let eps = claude_session(&claude.join("-w-corp-app/s-corp-1.jsonl"));
        let said: Vec<&str> = eps.iter().map(|f| f.episode.correction.as_str()).collect();
        assert_eq!(said, ["no, check the TOCTOU on the closed flag first"], "only the real prompt correction");
        let e = &eps[0].episode;
        assert_eq!((e.task.as_str(), e.agent.as_str(), e.session.as_str()), ("add a retry to the relay", "I'll wrap delivery in a ReentrantLock.", "s-corp-1"));
        assert_eq!(e.ms, borg_trace::parse_ts("2026-09-01T10:01:00.000Z"));
        assert!(claude_session(&claude.join("-w-corp-app/s-corp-1/subagents/a.jsonl")).len() == 1, "parser itself is neutral");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_veto_prompt_is_replayed_as_the_hook_reads_it() {
        let mut r = Replay { session: "sess-veto-1".into(), ..Default::default() };
        r.prompt(1, "fix the merge lookup");
        r.prompt(2, "borg no: never read the commit log");
        r.prompt(3, "no, use the review API");
        // After a wordless denial, the veto answers it: no rejected-tool correction.
        r.denied = Some(("Bash git log".into(), String::new()));
        r.prompt(5, "borg no, stop reading history");
        let eps: Vec<&Episode> = r.out.iter().map(|f| &f.episode).collect();
        let got: Vec<(&str, &str, &str, &str)> = eps.iter().map(|e| (e.id.as_str(), e.kind.as_str(), e.task.as_str(), e.correction.as_str())).collect();
        assert_eq!(
            got,
            [
                ("no-sess-vet-2", crate::veto::KIND, "fix the merge lookup", "never read the commit log"),
                ("sess-vet-3", "", "fix the merge lookup", "no, use the review API"),
                ("no-sess-vet-5", crate::veto::KIND, "fix the merge lookup", "stop reading history"),
            ]
        );
        assert!(eps.iter().all(|e| e.rejected_tool.is_none()) && r.denied.is_none());
    }

    #[test]
    fn dry_run_writes_nothing_and_counts_per_collective() {
        let root = temp("dry");
        let (claude, omp) = fixture(&root);
        let base = root.join("home");
        let cfg: Config = serde_json::from_str(CFG).unwrap();
        let r = run(&opts(&claude, &omp, false), Some(&cfg), &base, &|_| panic!("a dry run calls no model"));
        assert!(!base.exists(), "dry run wrote something");
        assert_eq!(r.scanned, 4, "main sessions only");
        let corp = &r.collectives["corp"];
        assert_eq!((corp.sessions, corp.episodes, corp.calls, corp.may_sync), (2, 2, 2, true));
        let mine = &r.collectives["mine"];
        assert_eq!((mine.sessions, mine.episodes, mine.may_sync), (1, 1, false));
        assert_eq!((r.off_sessions, r.off_episodes), (1, 1));
        assert!(render(&r, false).contains("dry run"));
        assert!(render(&r, false).trim_end().ends_with("to learn these episodes."));
        assert!(!render(&r, true).contains("Dry run"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn apply_routes_episodes_learns_and_is_idempotent() {
        let root = temp("apply");
        let (claude, omp) = fixture(&root);
        let base = root.join("home");
        let cfg: Config = serde_json::from_str(CFG).unwrap();
        let r = run(&opts(&claude, &omp, true), Some(&cfg), &base, &ask);
        assert_eq!((r.collectives["corp"].new_lessons, r.collectives["mine"].new_lessons), (2, 1));

        let corp: Vec<Episode> = read_jsonl(&base.join("collectives/corp/episodes.jsonl"));
        let said: Vec<&str> = corp.iter().map(|e| e.correction.as_str()).collect();
        assert_eq!(said, ["no, check the TOCTOU on the closed flag first", "only the one crate, pin the rest"]);
        assert_eq!(corp[1].rejected_tool.as_deref(), Some(r#"bash {"command":"cargo update"}"#), "a denial's reason is the next prompt");
        assert_eq!((corp[1].session.as_str(), corp[1].repo.as_str()), ("omp-sess-1", "svc"));
        let mine: Vec<Episode> = read_jsonl(&base.join("collectives/mine/episodes.jsonl"));
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].correction, "keep the cache dir");
        assert!(mine[0].rejected_tool.as_deref().unwrap().starts_with("Bash "));
        // Nothing of the off repo, nor of anything mentioning a skipped word, anywhere.
        for f in files(&base) {
            let t = std::fs::read_to_string(&f).unwrap();
            assert!(!t.contains("doctor") && !t.contains("diary") && !t.to_lowercase().contains("oldproject"), "{}", f.display());
        }
        assert_eq!(learn::lessons(&base.join("collectives/corp/lessons.jsonl")).len(), 2);

        // Re-running adds nothing and calls no model.
        let snapshot = |d: &Path| files(d).into_iter().map(|f| std::fs::read_to_string(f).unwrap()).collect::<Vec<_>>();
        let before = snapshot(&base);
        let again = run(&opts(&claude, &omp, true), Some(&cfg), &base, &|_| panic!("nothing new to learn"));
        assert_eq!((again.collectives["corp"].episodes, again.collectives["corp"].known), (0, 2));
        assert_eq!(snapshot(&base), before);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn off_repo_leaves_nothing_and_live_episodes_are_not_duplicated() {
        let root = temp("off");
        let (claude, omp) = fixture(&root);
        let base = root.join("home");
        let only_off: Config = serde_json::from_str(r#"{"default": "off", "off": ["/w"]}"#).unwrap();
        let r = run(&opts(&claude, &omp, true), Some(&only_off), &base, &|_| panic!("off calls no model"));
        assert!(r.collectives.is_empty() && !base.exists());
        assert_eq!(r.off_episodes, 4);

        // The live hook already captured the corp correction (stamped a moment after the message).
        let corp = base.join("collectives/corp");
        std::fs::create_dir_all(&corp).unwrap();
        let live = Episode {
            id: "s-corp-1-live".into(),
            ms: borg_trace::parse_ts("2026-09-01T10:01:00.800Z"),
            repo: "app".into(),
            session: "s-corp-1".into(),
            task: String::new(),
            agent: String::new(),
            rejected_tool: None,
            correction: "no".into(),
            ..Episode::default()
        };
        std::fs::write(corp.join("episodes.jsonl"), serde_json::to_string(&live).unwrap() + "\n").unwrap();
        let cfg: Config = serde_json::from_str(CFG).unwrap();
        let since = Options { since: parse_date("2026-09-01"), limit: Some(10), ..opts(&claude, &omp, false) };
        let r = run(&since, Some(&cfg), &base, &|_| None);
        assert_eq!((r.collectives["corp"].episodes, r.collectives["corp"].known, r.collectives["corp"].calls), (1, 1, 2));
        // --since and --limit bound what is considered.
        let late = Options { since: parse_date("2026-09-04"), ..opts(&claude, &omp, false) };
        assert!(!run(&late, Some(&cfg), &base, &|_| None).collectives.contains_key("mine"));
        let one = Options { limit: Some(1), ..opts(&claude, &omp, false) };
        assert_eq!(run(&one, Some(&cfg), &base, &|_| None).collectives.values().map(|t| t.episodes).sum::<usize>(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn limit_works_through_the_history_in_batches() {
        let root = temp("limit");
        let (claude, omp) = fixture(&root);
        let base = root.join("home");
        let cfg: Config = serde_json::from_str(CFG).unwrap();
        let one = Options { limit: Some(1), ..opts(&claude, &omp, true) };
        let stored = || -> Vec<String> {
            ["corp", "mine"].iter().flat_map(|c| read_jsonl::<Episode>(&base.join(format!("collectives/{c}/episodes.jsonl")))).map(|e| e.correction).collect()
        };
        // Newest first: the omp reason (09-04), then the private rejection (09-02), then the corp prompt (09-01).
        // The off episode (09-03) and the skipped one never use up the limit.
        run(&one, Some(&cfg), &base, &ask);
        assert_eq!(stored(), ["only the one crate, pin the rest"]);
        run(&one, Some(&cfg), &base, &ask);
        assert_eq!(stored(), ["only the one crate, pin the rest", "keep the cache dir"], "the second batch is the next episode");
        run(&one, Some(&cfg), &base, &ask);
        let last = run(&one, Some(&cfg), &base, &|_| panic!("nothing left to learn"));
        assert_eq!(stored().len(), 3);
        assert_eq!(last.collectives.values().map(|t| (t.episodes, t.known)).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1)), (0, 3));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn state_from_an_off_repo_never_reaches_a_collective() {
        let root = temp("moved");
        let claude = root.join("claude");
        let (s, c) = ("/w/secret/notes", "/w/corp/app");
        // One session: the task and a denied tool call in an off repo, then the cwd moves to corp.
        write(
            &claude.join("-w-secret-notes/s-moved-1.jsonl"),
            &[
                user(s, "2026-09-05T09:00:00.000Z", "summarise my therapy notes"),
                says(s, "2026-09-05T09:00:01.000Z", "Reading the therapy notes now."),
                tool_use(s, "2026-09-05T09:00:02.000Z", "t1"),
                result(
                    s,
                    "2026-09-05T09:00:03.000Z",
                    "t1",
                    true,
                    "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.",
                ),
                user(c, "2026-09-05T09:05:00.000Z", "never read files outside the repo"),
                user(c, "2026-09-05T09:06:00.000Z", "no, use the relay config instead"),
                says(c, "2026-09-05T09:06:30.000Z", "Switching to the relay config."),
                user(c, "2026-09-05T09:07:00.000Z", "no, the staging one"),
            ],
        );
        let base = root.join("home");
        let cfg: Config = serde_json::from_str(CFG).unwrap();
        let r =
            run(&Options { claude: Some(claude), omp: None, opencode: None, since: None, limit: None, apply: true, skip: Vec::new() }, Some(&cfg), &base, &ask);
        assert_eq!(r.collectives["corp"].episodes, 3);
        let corp: Vec<Episode> = read_jsonl(&base.join("collectives/corp/episodes.jsonl"));
        let t = std::fs::read_to_string(base.join("collectives/corp/episodes.jsonl")).unwrap();
        assert!(!t.contains("therapy") && !t.contains("rm -rf"), "off text in the team store: {t}");
        // The denial's reason is kept (said in corp), but not the off task, drone text or tool call.
        assert_eq!((corp[0].task.as_str(), corp[0].agent.as_str(), corp[0].rejected_tool.as_ref()), ("", "", None));
        assert_eq!((corp[0].correction.as_str(), corp[1].task.as_str()), ("never read files outside the repo", ""));
        // State seen in corp itself is kept.
        assert_eq!((corp[2].task.as_str(), corp[2].agent.as_str()), ("", "Switching to the relay config."));
        for f in files(&base) {
            assert!(!std::fs::read_to_string(&f).unwrap().contains("therapy"), "{}", f.display());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    // ---- opencode

    /// A redacted real export (crates/borg/tests/fixtures/opencode).
    macro_rules! export {
        ($f:literal) => {
            serde_json::from_str::<Value>(include_str!(concat!("../tests/fixtures/opencode/", $f))).unwrap()
        };
    }

    /// A 1.x / 2.x user message typed by the engineer, appended to an export.
    fn say_v1(mut export: Value, ms: i64, text: &str) -> Value {
        let m = json!({"info": {"role": "user", "time": {"created": ms}}, "parts": [{"type": "text", "text": text}]});
        export["messages"].as_array_mut().unwrap().push(m);
        export
    }

    fn say_v2(mut export: Value, ms: i64, text: &str) -> Value {
        export["messages"].as_array_mut().unwrap().push(json!({"type": "user", "time": {"created": ms}, "text": text}));
        export
    }

    fn corrections(found: &[Found]) -> Vec<&str> {
        found.iter().map(|f| f.episode.correction.as_str()).collect()
    }

    #[test]
    fn opencode_errors_take_claudes_wording_and_only_the_engineers_refusals_count() {
        let feedback = canonical(None, &format!("{OC_FEEDBACK} keep the public API"));
        assert_eq!(learn::rejection(&feedback).as_deref(), Some("keep the public API"));
        assert!(feedback.lines().next().unwrap().contains(NOTICE), "the notice is on the first line, as the replay requires");
        for wordless in [canonical(None, OC_REJECTED), canonical(Some("aborted"), OC_DECLINED), canonical(Some("permission.rejected"), "")] {
            assert!(learn::denied(&wordless), "{wordless}");
        }
        let words = canonical(Some("permission.rejected"), "no, do not delete files, use git rm instead");
        assert_eq!(learn::rejection(&words).as_deref(), Some("no, do not delete files, use git rm instead"));
        // Nobody's correction: opencode run's own auto-reject, a config rule, a dismissed question, an
        // abort, a forced call under deny-all, a failed tool.
        for (kind, m) in [
            (Some("permission.rejected"), OC_AUTO_REJECTED),
            (None, "The user has specified a rule which prevents you from using this specific tool call. Here are some of the relevant rules []"),
            (None, "The user dismissed this question"),
            (None, "Tool execution aborted"),
            (Some("aborted"), "Step interrupted"),
            (None, "Model tried to call unavailable tool 'invalid'. Available tools: ."),
            (Some("tool.execution"), "File not found: /home/dev/work/missing.rs"),
        ] {
            let c = canonical(kind, m);
            assert!(learn::rejection(&c).is_none() && !learn::denied(&c), "{m}");
        }
    }

    #[test]
    fn opencode_run_quoting_is_stripped_from_prompts() {
        let quirk = export!("v1/run-deny-all-forced-tool-call-export.json")["messages"][0]["parts"][0]["text"].as_str().unwrap().to_string();
        assert_eq!(unquote(&quirk), r#"Read relay.rs and answer with {"ok": true}"#);
        assert_eq!(unquote("\"no, that is wrong, use a Mutex instead\""), "no, that is wrong, use a Mutex instead");
        assert_eq!(unquote("no quotes"), "no quotes");
        assert_eq!(unquote(r#""a" and "b""#), r#""a" and "b""#, "an unescaped quote inside: typed, not argv");
        assert_eq!(unquote("\""), "\"");
    }

    #[test]
    fn opencode_1_rejection_with_feedback_is_an_episode() {
        let found = opencode_session(&export!("v1/reject-with-feedback.json"));
        assert_eq!(corrections(&found), ["no, that is wrong, use the AtomicBoolean compareAndSet instead"]);
        let (f, e) = (&found[0], &found[0].episode);
        assert_eq!((f.cwd.as_str(), e.session.as_str(), e.task.as_str()), ("/home/dev/work", "ses_ec5c7766cffe7lNfHnHDzyNoUc", "Fix the race in relay.rs"));
        assert!(e.rejected_tool.as_deref().unwrap().starts_with(r#"read {"filePath":"/home/dev/work/"#));
        assert_eq!(e.ms, 1791978152946, "when the engineer answered, not when the message began");
        // The sibling 1.x rejected along with it (plain words, 4 ms later) is the cascade, not the
        // engineer: the next prompt is a new task, no second episode.
        let next = say_v1(export!("v1/reject-with-feedback.json"), 1791978160000, "now add a test for the relay");
        assert_eq!(corrections(&opencode_session(&next)), ["no, that is wrong, use the AtomicBoolean compareAndSet instead"]);
    }

    #[test]
    fn opencode_tool_calls_replay_in_the_order_they_finished() {
        // A wordless rejection made first but answered last, and a sibling read that completed
        // before it: live, the hook hears the read finish and then the rejection, so the denial waits.
        let mut export = export!("v1/reject-plain-then-correction.json");
        let sibling = &mut export["messages"][1]["parts"][2]["state"];
        sibling["status"] = json!("completed");
        sibling["output"] = json!("fn deliver() {}");
        sibling["time"]["end"] = json!(1791978164300i64);
        sibling.as_object_mut().unwrap().remove("error");
        let found = opencode_session(&export);
        assert_eq!(corrections(&found), ["no, that is wrong, use compareAndSet instead of get then set"]);
        assert!(found[0].episode.rejected_tool.as_deref().unwrap().starts_with("read "));
        // Text keeps its place; only the calls between two texts are reordered.
        let steps =
            by_completion(vec![Step::Tool(9, None, "b".into()), Step::Tool(3, None, "a".into()), Step::Said("then".into()), Step::Tool(2, None, "c".into())]);
        let order: Vec<String> = steps
            .iter()
            .map(|s| match s {
                Step::Tool(_, _, t) | Step::Said(t) => t.clone(),
                Step::Prompt(..) => unreachable!(),
            })
            .collect();
        assert_eq!(order, ["a", "b", "then", "c"]);
    }

    #[test]
    fn opencode_1_wordless_rejection_makes_the_next_prompt_the_reason() {
        let found = opencode_session(&export!("v1/reject-plain-then-correction.json"));
        assert_eq!(corrections(&found), ["no, that is wrong, use compareAndSet instead of get then set"]);
        assert!(found[0].episode.rejected_tool.as_deref().unwrap().starts_with("read "));
        assert_eq!(found[0].episode.ms, 1791978166088);
        // A real session: the reason need not read like a correction.
        let found = opencode_session(&export!("v1/bash-rejected-then-correction-window.json"));
        assert_eq!(corrections(&found), ["wait, revert that one. no, that is wrong, use the existing close constant instead"]);
        assert!(found[0].episode.rejected_tool.as_deref().unwrap().starts_with("bash "));
        assert_eq!(found[0].cwd, "/home/dev/work/service-a");
    }

    #[test]
    fn opencode_1_correcting_prompts_are_episodes_and_a_dismissed_question_is_not() {
        let found = opencode_session(&export!("v1/correction-rejection-dismissed.json"));
        assert_eq!(
            corrections(&found),
            ["no, that is wrong, use the test script of this project instead", "no, you need to wait until the server is ready, read the project notes first"]
        );
        assert!(found.iter().all(|f| f.episode.task == "Run the test suite and then the app." && f.episode.rejected_tool.is_none()));
        assert_eq!(found[0].episode.agent, "I will look at the relevant file and then make the change.");
    }

    #[test]
    fn opencode_1_a_rule_that_denies_a_call_is_not_the_engineer() {
        let by_rule = say_v1(export!("v1/permission-denied-by-rule.json"), 1776791500000, "use the plan file");
        assert!(opencode_session(&by_rule).is_empty());
        // The same call refused by the engineer: the next prompt is the reason.
        let mut refused = by_rule.clone();
        refused["messages"][0]["parts"][2]["state"]["error"] = json!(OC_REJECTED);
        assert_eq!(corrections(&opencode_session(&refused)), ["use the plan file"]);
    }

    #[test]
    fn opencode_1_echoes_and_synthetic_parts_are_not_the_engineer() {
        // borg's prompt context (a synthetic part) is not in the task; borg's echoed question
        // (metadata.borg) does not replace it.
        let echo = say_v1(export!("v1/echo-marker-and-synthetic-part.json"), 1791978190000, "no, use a channel");
        let found = opencode_session(&echo);
        assert_eq!(corrections(&found), ["no, use a channel"]);
        assert_eq!(found[0].episode.task, "Fix the race in relay.rs");
        assert_eq!(found[0].episode.agent, "Plan: I check closed.get() on the AtomicBoolean before deliver() in relay.rs.");
        // An attached file and editor context are synthetic; the engineer's words are the task.
        let file = say_v1(export!("v1/synthetic-file-attachment.json"), 1776791100000, "no, the other plan");
        assert_eq!(opencode_session(&file)[0].episode.task, "Read @docs/plan.md and tell me what is missing.");
        let editor = say_v1(export!("v1/synthetic-editor-context.json"), 1779367490000, "no, the other workflow");
        assert_eq!(opencode_session(&editor)[0].episode.task, "Please make the requested change in this project.");
        // A compaction's "Continue" is no task: the next prompt becomes the task, not a correction.
        let compacted = say_v1(export!("v1/compaction-synthetic-continue.json"), 1776858910000, "no, the other module");
        assert!(opencode_session(&compacted).is_empty());
    }

    #[test]
    fn opencode_sub_agents_and_borgs_model_calls_are_skipped() {
        for export in [
            say_v1(export!("v1/subagent-child.json"), 1776861800000, "no, the other file"),
            export!("v1/model-call-session.json"),
            export!("v1/run-deny-all-forced-tool-call-export.json"),
            say_v2(export!("v2/session-export-child.json"), 1791977829000, "no, the other file"),
        ] {
            assert!(!engineers_session(&export) && opencode_session(&export).is_empty(), "{}", export["info"]["title"]);
        }
        assert!(engineers_session(&export!("v2/session-export.json")) && !engineers_session(&json!({"messages": []})));
    }

    #[test]
    fn opencode_2_session_replays_like_the_live_hook() {
        // The tail: a synthetic input (borg's context) and borg's echoed question, then the engineer.
        let export = say_v2(export!("v2/session-export.json"), 1791977840000, "no, check the queue bound first");
        let found = opencode_session(&export);
        assert_eq!(
            corrections(&found),
            [
                "no, that is wrong, use a Mutex instead of the AtomicBoolean", // run's argv quoting stripped
                "SHELL clean up the file",                                     // the reason after a declined call
                "no, do not delete files, use git rm instead",                 // feedback; the auto-reject before it is not
                "no, check the queue bound first",
            ]
        );
        let e: Vec<&Episode> = found.iter().map(|f| &f.episode).collect();
        assert!(found.iter().all(|f| f.cwd == "/home/dev/work" && f.episode.session == "ses_ec5cd46d0ffeGGDxs4GhqC9Ifu"));
        assert_eq!(
            (e[0].task.as_str(), e[0].agent.as_str(), e[0].rejected_tool.as_deref()),
            ("READS Please fix the race in relay.rs", "Done: the mock finished this turn.", None)
        );
        assert_eq!(e[1].rejected_tool.as_deref(), Some(r#"shell {"command":"rm -f relay.rs"}"#));
        assert_eq!(e[1].task, "SHELL remove the old file again");
        assert_eq!(e[2].rejected_tool.as_deref(), Some(r#"shell {"command":"rm -f relay.rs"}"#));
        assert_eq!(e[3].task, "SUBAGENT delegate a check of relay.rs", "the synthetic input and the echo are not the task");
        assert!(e.windows(2).all(|w| w[0].ms < w[1].ms));
    }

    #[test]
    fn opencode_2_session_migrated_from_1_reads_its_location() {
        let found = opencode_session(&say_v2(export!("v2/session-export-migrated-from-v1.json"), 1791978280000, "no, keep the lock"));
        assert_eq!(corrections(&found), ["no, keep the lock"]);
        assert_eq!((found[0].cwd.as_str(), found[0].episode.task.as_str()), ("/home/dev/legacy-work", "no, that is wrong, use a Mutex instead"));
    }

    #[test]
    fn opencode_discovery_asks_the_cli_read_only() {
        let db = db_args(Some(1_700_000_000_000));
        assert_eq!(&db[..4], ["db", "--pure", "--format", "json"]);
        assert_eq!(
            db[4],
            "SELECT id FROM session WHERE parent_id IS NULL AND title != 'borg (model call)' AND time_updated >= 1700000000000 ORDER BY time_created"
        );
        assert!(!db_args(None)[4].contains("time_updated"));
        assert_eq!(list_args(true), ["session", "list", "--standalone", "--format", "json", "-n", "100000"]);
        assert_eq!(list_args(false), ["session", "list", "--pure", "--format", "json", "-n", "100000"]);
        assert_eq!(opencode::export_args(true, "ses_a"), ["session", "export", "--standalone", "ses_a"]);
        assert_eq!(opencode::export_args(false, "ses_a"), ["export", "--pure", "ses_a"]);
        let listed = include_str!("../tests/fixtures/opencode/v2/session-list.json").as_bytes();
        assert_eq!(ids_of(listed, None).unwrap().len(), 3);
        assert_eq!(ids_of(listed, Some(1791977900000)).unwrap(), ["ses_ec5bbe808ffe4C17dEwmu9aHAe", "ses_ec5caa71cffeRc4s7USxbim6zn"]);
        assert_eq!(ids_of(br#"[{"id": "a", "title": "borg (model call)"}, {"id": "b"}]"#, None).unwrap(), ["b"]);
        assert_eq!(ids_of(b"Error: no such table", None), None);
    }

    #[test]
    fn the_cwds_of_claude_and_omp_sessions_seed_opencode_2s_listing() {
        let root = temp("cwds");
        let (claude, omp) = fixture(&root);
        let cwds = |r: Replay| r.cwds.into_iter().collect::<Vec<_>>();
        assert_eq!(cwds(claude_replay(&claude.join("-w-corp-app/s-corp-1.jsonl"))), ["/w/corp/app"]);
        assert_eq!(cwds(omp_replay(&omp.join("-w-corp-svc/2026-09-04_x.jsonl"))), ["/w/corp/svc"]);
        let moved = claude.join("-w-secret-notes/moved.jsonl");
        write(&moved, &[user("/w/secret/notes", "2026-09-05T09:00:00.000Z", "a task"), user("/w/corp/app", "2026-09-05T09:05:00.000Z", "next")]);
        assert_eq!(cwds(claude_replay(&moved)), ["/w/corp/app", "/w/secret/notes"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_collective_folder_of_repos_lists_each_repo() {
        let root = temp("collective-dirs");
        let work = root.join("work-repos");
        for repo in ["a", "team/b"] {
            std::fs::create_dir_all(work.join(repo).join(".git")).unwrap();
        }
        std::fs::create_dir_all(work.join("a/src")).unwrap();
        std::fs::create_dir_all(work.join("notes")).unwrap();
        let cfg: Config =
            serde_json::from_str(&format!(r#"{{"collectives": [{{"name": "w", "paths": ["{}", "{}"]}}]}}"#, work.display(), work.join("a").display())).unwrap();
        let known = collective_dirs(Some(&cfg));
        assert_eq!(known, [work.clone(), work.join("a"), work.join("team/b")].into_iter().collect(), "repos two levels deep; not inside a repo");
        // With no Claude Code or omp session at all, opencode 2 lists in each repo.
        let cli = FakeCli { calls: Default::default(), db: None, list: "[]" };
        cli.run(true, &known);
        let lists: Vec<PathBuf> = cli.calls.borrow().iter().map(|(_, d)| d.clone()).collect();
        assert_eq!(lists, [work.clone(), work.join("a"), work.join("team/b")]);
        assert!(collective_dirs(None).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_cli_wrapper_passes_every_argument_and_reads_a_long_answer_whole() {
        let fake = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/opencode/fake"));
        let path = std::env::join_paths(std::iter::once(fake).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()))).unwrap();
        let dir = temp("cli-wrapper");
        let args = db_args(Some(1));
        let out = opencode::cli(true, Some(&path), &args, &dir).unwrap();
        assert!(out.len() > 64 * 1024, "the fake answers more than a pipe holds");
        let ids = ids_of(&out, None).unwrap();
        assert_eq!((ids.len(), ids.last().map(String::as_str)), (3001, Some("ses_last")));
        let seen = std::fs::read_to_string(dir.join("args.txt")).unwrap();
        let mut want = vec![dir.display().to_string()];
        want.extend(args);
        assert_eq!(seen.lines().collect::<Vec<_>>(), want, "PWD, then the arguments unsplit (the query holds spaces and quotes)");
        // opencode 2 runs with a model call's env: no plugins, no project config.
        let env = std::fs::read_to_string(dir.join("env.txt")).unwrap();
        assert!(env.contains("OPENCODE_CONFIG_DIR=") && env.contains("OPENCODE_DISABLE_PROJECT_CONFIG=1"), "{env}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn opencode_projects_are_git_roots_of_existing_dirs() {
        let root = temp("projects");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        std::fs::create_dir_all(root.join("loose")).unwrap();
        let dirs: BTreeSet<PathBuf> = [repo.join("src/deep"), repo.clone(), root.join("loose"), root.join("gone")].into_iter().collect();
        assert_eq!(projects(&dirs), [root.join("loose"), repo]);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A fake opencode CLI: the calls it got, and canned answers by first argument / session id.
    struct FakeCli {
        calls: std::cell::RefCell<Vec<(Vec<String>, PathBuf)>>,
        db: Option<&'static str>,
        list: &'static str,
    }

    impl FakeCli {
        fn exec(&self, args: &[String], dir: &Path) -> Option<Vec<u8>> {
            self.calls.borrow_mut().push((args.to_vec(), dir.to_path_buf()));
            let id = args.last().unwrap().as_str();
            let export = |v: Value| Some(v.to_string().into_bytes());
            match args[0].as_str() {
                "db" => self.db.map(|d| d.as_bytes().to_vec()),
                "session" if args[1] == "list" => Some(self.list.as_bytes().to_vec()),
                _ if id == "ses_v1" => export(export!("v1/reject-with-feedback.json")),
                _ if id == "ses_v2" => export(export!("v2/session-export.json")),
                _ => Some(b"Session not found".to_vec()),
            }
        }

        fn run(&self, v2: bool, known: &BTreeSet<PathBuf>) -> Vec<String> {
            let mut got = Vec::new();
            cli_sessions(v2, None, known, &|a, d| self.exec(a, d), &mut |v| got.push(v["info"]["id"].as_str().unwrap().to_string()));
            got
        }
    }

    #[test]
    fn opencode_1_lists_every_session_with_one_query_and_exports_each() {
        let tmp = std::env::temp_dir();
        let cli = FakeCli { calls: Default::default(), db: Some(r#"[{"id": "ses_v1"}, {"id": "ses_gone"}]"#), list: "[]" };
        let known: BTreeSet<PathBuf> = [tmp.clone()].into_iter().collect();
        assert_eq!(cli.run(false, &known).len(), 1, "an export that is no JSON is skipped");
        let calls = cli.calls.borrow();
        let firsts: Vec<&str> = calls.iter().map(|(a, _)| a[0].as_str()).collect();
        assert_eq!(firsts, ["db", "export", "export"], "no listing when the query works");
        assert!(calls.iter().all(|(_, d)| *d == tmp));
    }

    #[test]
    fn opencode_1_without_its_query_and_opencode_2_list_the_known_projects() {
        let root = temp("cli");
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let known: BTreeSet<PathBuf> = [a.clone(), b.clone(), root.join("gone")].into_iter().collect();
        let list = r#"[{"id": "ses_v2", "title": "research main"}, {"id": "ses_x", "title": "borg (model call)"}, {"id": "ses_v1", "title": "t"}]"#;
        for v2 in [false, true] {
            let cli = FakeCli { calls: Default::default(), db: (!v2).then_some("Error: no such column"), list };
            // Both projects list the same sessions: each is exported once, in the first dir it was seen in.
            assert_eq!(cli.run(v2, &known), ["ses_ec5cd46d0ffeGGDxs4GhqC9Ifu", "ses_ec5c7766cffe7lNfHnHDzyNoUc"]);
            let calls = cli.calls.borrow();
            let lists: Vec<&PathBuf> = calls.iter().filter(|(c, _)| c[1] == "list").map(|(_, d)| d).collect();
            assert_eq!(lists, [&a, &b], "existing dirs only");
            let exports: Vec<(&str, &PathBuf)> =
                calls.iter().filter(|(c, _)| c.contains(&"ses_v2".to_string()) || c.contains(&"ses_v1".to_string())).map(|(c, d)| (c[0].as_str(), d)).collect();
            assert_eq!(exports.len(), 2);
            assert!(exports.iter().all(|(_, d)| **d == a));
            if v2 {
                assert!(calls.iter().all(|(c, _)| c[0] == "session" && c.contains(&"--standalone".to_string())), "never the background service: {calls:?}");
            } else {
                assert_eq!(calls[0].0[0], "db");
                assert!(calls[1..].iter().all(|(c, _)| c.contains(&"--pure".to_string())));
            }
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn render_names_every_source() {
        let r = Report { scanned: 61, claude: 40, omp: 12, opencode: Some(9), ..Default::default() };
        assert!(render(&r, false).contains("sources: claude 40 sessions, omp 12, opencode 9\n"));
        let r = Report { opencode: None, ..r };
        assert!(render(&r, true).contains("sources: claude 40 sessions, omp 12, opencode not installed\n"));
    }

    /// `--opencode DIR` with exports of both versions: a 2.x session in a corp repo, a 1.x one in a
    /// private repo, a 1.x one in an off repo, a sub-agent and one of borg's model calls.
    fn opencode_dir(root: &Path) -> PathBuf {
        let dir = root.join("opencode");
        std::fs::create_dir_all(&dir).unwrap();
        let put = |name: &str, mut v: Value, cwd: &str| {
            if v["info"].get("location").is_some() {
                v["info"]["location"]["directory"] = json!(cwd);
            } else {
                v["info"]["directory"] = json!(cwd);
            }
            std::fs::write(dir.join(name), v.to_string()).unwrap();
        };
        // The research run answered the permission 0.2 s after the prompt before it; an engineer takes
        // longer (two episodes of a session within SAME_MS are one).
        let mut corp = export!("v2/session-export.json");
        for m in corp["messages"].as_array_mut().unwrap() {
            for c in m["content"].as_array_mut().into_iter().flatten().filter(|c| c["id"] == "call_1791977813763_0") {
                c["time"]["completed"] = json!(1791977830000i64);
            }
        }
        put("corp.json", corp, "/w/corp/relay");
        put("mine.json", export!("v1/reject-with-feedback.json"), "/w/mine/notes");
        put("off.json", export!("v1/correction-rejection-dismissed.json"), "/w/secret/app");
        put("child.json", export!("v2/session-export-child.json"), "/w/corp/relay");
        put("call.json", export!("v1/model-call-session.json"), "/w/corp/relay");
        std::fs::write(dir.join("notes.txt"), "not an export").unwrap();
        dir
    }

    #[test]
    fn apply_opencode_exports_routes_episodes_learns_and_is_idempotent() {
        let root = temp("oc-apply");
        let dir = opencode_dir(&root);
        let base = root.join("home");
        let cfg: Config = serde_json::from_str(CFG).unwrap();
        let opts =
            |apply| Options { claude: None, omp: None, opencode: Some(OpencodeFrom::Dir(dir.clone())), since: None, limit: None, apply, skip: Vec::new() };

        let dry = run(&opts(false), Some(&cfg), &base, &|_| panic!("a dry run calls no model"));
        assert_eq!((dry.scanned, dry.claude, dry.omp, dry.opencode), (3, 0, 0, Some(3)), "main sessions only");
        assert!(render(&dry, false).contains("sources: claude 0 sessions, omp 0, opencode 3\n"));
        assert_eq!((dry.off_sessions, dry.off_episodes), (1, 2));
        assert!(!base.exists());

        let r = run(&opts(true), Some(&cfg), &base, &ask);
        assert_eq!((r.collectives["corp"].new_lessons, r.collectives["mine"].new_lessons), (3, 1));
        let corp: Vec<Episode> = read_jsonl(&base.join("collectives/corp/episodes.jsonl"));
        assert_eq!(
            corp.iter().map(|e| e.correction.as_str()).collect::<Vec<_>>(),
            ["no, that is wrong, use a Mutex instead of the AtomicBoolean", "SHELL clean up the file", "no, do not delete files, use git rm instead"]
        );
        assert_eq!((corp[0].session.as_str(), corp[0].repo.as_str()), ("ses_ec5cd46d0ffeGGDxs4GhqC9Ifu", "relay"));
        let mine: Vec<Episode> = read_jsonl(&base.join("collectives/mine/episodes.jsonl"));
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].correction, "no, that is wrong, use the AtomicBoolean compareAndSet instead");
        for f in files(&base) {
            let t = std::fs::read_to_string(&f).unwrap();
            assert!(!t.contains("test script") && !t.contains("CHILDTASK") && !t.contains("is_lesson"), "{}", f.display());
        }

        let snapshot = |d: &Path| files(d).into_iter().map(|f| std::fs::read_to_string(f).unwrap()).collect::<Vec<_>>();
        let before = snapshot(&base);
        let again = run(&opts(true), Some(&cfg), &base, &|_| panic!("nothing new to learn"));
        assert_eq!((again.collectives["corp"].episodes, again.collectives["corp"].known), (0, 3));
        assert_eq!(snapshot(&base), before);
        let _ = std::fs::remove_dir_all(root);
    }
}
