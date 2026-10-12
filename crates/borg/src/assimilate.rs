//! `borg assimilate`: bootstrap the collectives from the engineer's existing history, so Borg is
//! useful on day one instead of after weeks of live capture.
//!
//! Sources are Claude Code transcripts (`~/.claude/projects/<slug>/<session>.jsonl`) and omp sessions
//! (`~/.omp/agent/sessions/<slug>/<session>.jsonl`); main sessions only (sub-agents never talk to the
//! engineer). Each is replayed the way the live hooks would have seen it:
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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::collective::{self, Config, Route};
use crate::learn::{self, Episode, Lesson};

/// Two episodes of one session this close together are the same correction.
const SAME_MS: i64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Claude,
    Omp,
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
    pub collectives: BTreeMap<String, Tally>,
    pub off_sessions: usize,
    pub off_episodes: usize,
}

const NOTICE: &str = "want to proceed with this tool use";

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
}

impl Replay {
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
            engineer: String::new(),
        };
        self.out.push(Found { cwd: self.cwd.clone(), task_cwd: self.task.1.clone(), agent_cwd: self.said.1.clone(), tool_cwd, episode });
    }

    fn assistant_text(&mut self, t: &str) {
        if !t.trim().is_empty() {
            self.said = (tail(t.trim(), 2000), self.cwd.clone());
        }
    }

    fn prompt(&mut self, ms: i64, t: &str) {
        let t = t.trim();
        if !human(t) {
            return;
        }
        if let Some(tool) = self.denied.take() {
            self.episode(ms, Some(tool), take(t, 1500));
            return;
        }
        if learn::is_correction(t) && !self.task.0.is_empty() {
            self.episode(ms, None, take(t, 1500));
        } else {
            self.task = (take(t, 3000), self.cwd.clone());
        }
    }

    fn tool_result(&mut self, ms: i64, is_error: bool, text: &str, tool: String) {
        if !is_error {
            self.denied = None; // the drone moved on: the next prompt is no longer the reason
            return;
        }
        // Only the harness's own notice, on the first line, is a rejection; the same words further down
        // are tool output (e.g. "Exit code 1" and then an old transcript).
        let first: String = take(text.trim_start().lines().next().unwrap_or_default(), 200);
        if let Some(said) = learn::rejection(text).filter(|_| first.contains(NOTICE)) {
            self.episode(ms, Some((tool, self.cwd.clone())), said);
        } else if learn::denied(text) {
            self.denied = Some((tool, self.cwd.clone()));
        }
    }
}

fn tool_text(name: &str, input: &Value) -> String {
    format!("{name} {}", take(&input.to_string(), 1500))
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
    let mut r = Replay { session: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), ..Default::default() };
    let mut tools: HashMap<String, String> = HashMap::new();
    for v in lines(path) {
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return Vec::new();
        }
        if let Some(c) = v.get("cwd").and_then(Value::as_str) {
            r.cwd = c.to_string();
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
    r.out
}

/// Episodes of one omp session (none for a sub-agent session).
pub fn omp_session(path: &Path) -> Vec<Found> {
    let mut r = Replay { session: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), ..Default::default() };
    let mut tools: HashMap<String, String> = HashMap::new();
    for v in lines(path) {
        match v.get("type").and_then(Value::as_str) {
            Some("session") => {
                if v.get("parentSession").is_some_and(|p| !p.is_null()) {
                    return Vec::new();
                }
                if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                    r.cwd = c.to_string();
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
    r.out
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
    for (root, source) in [(&opts.claude, Source::Claude), (&opts.omp, Source::Omp)] {
        let Some(root) = root else { continue };
        for f in session_files(root) {
            report.scanned += 1;
            found.extend(match source {
                Source::Claude => claude_session(&f),
                Source::Omp => omp_session(&f),
            });
        }
    }
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
    }
    Ok(())
}

pub fn render(r: &Report, apply: bool) -> String {
    let mut s = format!("{} sessions scanned{}\n", r.scanned, if apply { "" } else { " (dry run: nothing written; --apply to assimilate)" });
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

const USAGE: &str = "usage: borg assimilate [--apply] [--claude DIR] [--omp DIR] [--since YYYY-MM-DD] [--limit N]";

/// `borg assimilate [--apply] [--claude DIR] [--omp DIR] [--since DATE] [--limit N]`; the exit code.
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
        Options { claude: Some(claude.into()), omp: Some(omp.into()), since: None, limit: None, apply, skip: vec!["oldproject".into()] }
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
            engineer: String::new(),
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
        let r = run(&Options { claude: Some(claude), omp: None, since: None, limit: None, apply: true, skip: Vec::new() }, Some(&cfg), &base, &ask);
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
}
