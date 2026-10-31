//! omp (oh-my-pi): the TS extension `adapters/omp/borg.ts`, compiled in and copied to
//! `~/.omp/agent/extensions/borg.ts` (`$PI_CODING_AGENT_DIR/extensions`). The adapter translates omp's
//! events into borg's canonical ones and back, sends omp's pid, and gives borg the stop's budget
//! (`BUDGETS`, which borg's first answer tells it).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use super::{Budgets, CONTEXT_GUARD, Caps, DroneWake, Harness, Probe, Run, SubAgent, Transcript, Wiring};

pub const ADAPTER: &str = include_str!("../../assets/adapters/omp/borg.ts");

/// Protocol (omp's): how long omp waits for an extension's handler of any event but `session_shutdown`
/// before it gives up on it (aborting `session_stop`'s signal); no setting changes it. omp 18.13.1's
/// extensions runner (`EXTENSION_HANDLER_TIMEOUT_MS`, 30000), read 2026-10-28.
pub const HANDLER_LIMIT: Duration = Duration::from_secs(30);
/// Protocol (omp's): its limit for a `session_shutdown` handler (`SESSION_SHUTDOWN_HANDLER_TIMEOUT_MS`,
/// 2000, the same runner).
pub const SHUTDOWN_LIMIT: Duration = Duration::from_secs(2);
/// Guard: what the extension keeps of omp's limit for itself (spawning borg, reading its answer,
/// returning it to omp), so borg's answer arrives before omp gives up on the handler.
const STOP_MARGIN: Duration = Duration::from_secs(2);
/// Guard: the same at the session's end, where omp's limit leaves room for less.
const END_MARGIN: Duration = Duration::from_millis(500);

/// What the extension waits for each of borg's answers. Guards: the prompt's and a tool call's end are
/// file appends, cut off at 5 s so a hung borg never holds the engineer's turn; before a tool call borg
/// answers with a stat when nobody steers, cut off at 2 s. Derived: the stop and the end, omp's limits
/// less the margins.
pub const BUDGETS: Budgets = Budgets {
    prompt: Duration::from_secs(5),
    pre_tool: Duration::from_secs(2),
    post_tool: Duration::from_secs(5),
    stop: HANDLER_LIMIT.saturating_sub(STOP_MARGIN),
    end: SHUTDOWN_LIMIT.saturating_sub(END_MARGIN),
};

pub struct Omp;

impl Harness for Omp {
    fn name(&self) -> &'static str {
        "omp"
    }

    fn caps(&self) -> Caps {
        Caps {
            prompt_context: true,
            tool_context: true,
            // The channels exist (borg.ts); turning these on waits for an evaluation.
            early_delivery: false,
            question_dialog: false,
            transcript: Transcript::None,
            deny_capture: true,
            sends_pid: true,
            // The adapter names it too (`BORG_STOP_BUDGET_MS`), as borg's answer told it.
            stop_budget: Some(BUDGETS.stop),
            // omp injects a tool_result's additionalContext whole, as one developer message (18.13.1,
            // packages/agent/src/tool-context.ts), and before_agent_start's message whole: no limit.
            context_chars: CONTEXT_GUARD,
            session_env: Some("PI_SESSION_ID"),
            pid_env: None,
            drone_wake: DroneWake::Plugin,
        }
    }

    fn wiring(&self) -> Wiring {
        Wiring::AdapterFile { source: ADAPTER }
    }

    fn budgets(&self) -> Option<Budgets> {
        Some(BUDGETS)
    }

    fn target(&self, home: &Path, var: &dyn Fn(&str) -> Option<PathBuf>) -> PathBuf {
        var("PI_CODING_AGENT_DIR").unwrap_or_else(|| home.join(".omp/agent")).join("extensions/borg.ts")
    }

    fn probe(&self) -> Probe {
        Probe { cli: "omp", tested: " (adapter tested with 18.8.5 and 18.10.4; live mode needs 18.4.4 or newer)" }
    }

    /// Each session's task agents (`$PI_CODING_AGENT_DIR/sessions`, else `home/.omp/agent/sessions`).
    fn sub_runs(&self, home: &Path, session: Option<&str>, since: Option<i64>) -> Vec<Run> {
        session_runs(&sessions(home), session, since)
    }

    /// `<sessions>/<slug>/<ts>_<session>.jsonl` ([`session_file`]).
    fn session_log(&self, home: &Path, session: &str, _input: &Value) -> Option<PathBuf> {
        session_file(&sessions(home), session)
    }
}

// ---------------------------------------------------------------- transcripts

/// Where omp keeps its sessions, `<sessions>/<slug>/<ts>_<id>.jsonl`: `$PI_CODING_AGENT_DIR/sessions`,
/// else `home/.omp/agent/sessions`.
pub fn sessions(home: &Path) -> PathBuf {
    super::env_dir("PI_CODING_AGENT_DIR").unwrap_or_else(|| home.join(".omp/agent")).join("sessions")
}

/// Main session `id`'s file under `sessions`: a `<slug>/<ts>_<id>.jsonl` whose session record names `id`.
pub fn session_file(sessions: &Path, id: &str) -> Option<PathBuf> {
    let suffix = format!("_{id}.jsonl");
    let named = |f: &PathBuf| f.file_name().is_some_and(|n| n.to_string_lossy().ends_with(&suffix));
    // The session record is the file's first line: one line read per candidate.
    let records = |f: &PathBuf| first_line(f).is_some_and(|v| v["type"] == "session" && v["id"] == id);
    read_dir(sessions).into_iter().filter(|p| p.is_dir()).flat_map(|slug| read_dir(&slug)).filter(|f| f.is_file() && named(f)).find(records)
}

// ---------------------------------------------------------------- sub-agent sessions

/// One run per main session that has sub-agents, oldest first: of `session` only when given, from
/// sessions that started at or after `since`. A main session is `<slug>/<ts>_<id>.jsonl`, its
/// sub-agents' sessions `<slug>/<ts>_<id>/<Task>.jsonl` (theirs nested one directory deeper, named
/// `<Task>.<Sub>.jsonl`).
pub fn session_runs(sessions: &Path, session: Option<&str>, since: Option<i64>) -> Vec<Run> {
    let mut runs = Vec::new();
    for slug in read_dir(sessions).into_iter().filter(|p| p.is_dir()) {
        for dir in read_dir(&slug).into_iter().filter(|p| p.is_dir()) {
            let name = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let suffix = name.split_once('_').map_or(name.as_str(), |x| x.1);
            // The dir name's suffix picks the candidates, so session end reads one main file, not all.
            if session.is_some_and(|s| s != suffix) {
                continue;
            }
            // The main file's session record names the id the adapter passes, and confirms it.
            let main = read_lines(&dir.with_extension("jsonl"));
            let id = main.iter().find(|v| v["type"] == "session").and_then(|v| v["id"].as_str()).map_or_else(|| suffix.to_string(), String::from);
            if session.is_some_and(|s| s != id) {
                continue;
            }
            let mut agents: Vec<SubAgent> = jsonl_under(&dir).iter().map(|f| sub_agent(f)).collect();
            if agents.is_empty() {
                continue;
            }
            agents.sort_by_key(|a| a.started);
            let title = main.iter().find_map(|v| (v["type"] == "title").then(|| v["title"].as_str()).flatten()).unwrap_or_default().to_string();
            let started = main.iter().find(|v| v["type"] == "session").map_or(agents[0].started, |v| ts(&v["timestamp"]));
            if since.is_none_or(|s| started >= s) {
                runs.push(Run { id: name, session: id, name: title, started, agents, loose: false });
            }
        }
    }
    runs.sort_by_key(|r| r.started);
    runs
}

fn read_dir(p: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(p).into_iter().flatten().filter_map(|e| e.ok().map(|e| e.path())).collect();
    v.sort();
    v
}

/// The first line of `p`, parsed: an omp session file's session record.
pub fn first_line(p: &Path) -> Option<Value> {
    use std::io::BufRead;
    let line = std::io::BufReader::new(std::fs::File::open(p).ok()?).lines().next()?.ok()?;
    serde_json::from_str(&line).ok()
}

fn read_lines(p: &Path) -> Vec<Value> {
    std::fs::read_to_string(p).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// The `*.jsonl` files under `dir`, at any depth.
fn jsonl_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in read_dir(dir) {
        if p.is_dir() {
            out.extend(jsonl_under(&p));
        } else if p.extension().is_some_and(|x| x == "jsonl") {
            out.push(p);
        }
    }
    out
}

fn ts(v: &Value) -> i64 {
    v.as_str().map_or(0, borg_trace::parse_ts)
}

/// One sub-agent's session file: its label is the file's stem, its kind and prompt `session_init`'s
/// `agent` and `task`, its work every tool call's arguments and its text, its output what it
/// yielded ([`yielded`]).
fn sub_agent(path: &Path) -> SubAgent {
    let label = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut a = SubAgent { label, ..Default::default() };
    let mut accepted = Vec::new();
    let mut called = Vec::new();
    for v in read_lines(path) {
        match v["type"].as_str() {
            Some("session") if a.cwd.is_empty() => {
                a.cwd = v["cwd"].as_str().unwrap_or_default().to_string();
                a.started = ts(&v["timestamp"]);
            }
            Some("session_init") => {
                a.kind = v["agent"].as_str().unwrap_or_default().to_string();
                a.prompt = v["task"].as_str().unwrap_or_default().to_string();
                a.started = ts(&v["timestamp"]).max(a.started);
            }
            Some("message") => message(&mut a, &mut called, &mut accepted, &v["message"]),
            _ => {}
        }
    }
    // A transcript without results (an older omp) has its calls; a call wraps its data in `result`.
    let unwrap = |c: Value| match c.get("result") {
        Some(r) if r.is_object() => r.clone(),
        _ => c,
    };
    a.output = yielded(if accepted.is_empty() { called.into_iter().map(unwrap).collect() } else { accepted });
    a
}

/// One message of a sub-agent's session: an assistant's tool calls and texts are its work (its
/// `yield` calls kept in `called`), a `yield` result omp accepted goes to `accepted` (a call that
/// failed its schema is retried, and only the retry counts).
fn message(a: &mut SubAgent, called: &mut Vec<Value>, accepted: &mut Vec<Value>, m: &Value) {
    match m["role"].as_str() {
        Some("assistant") => {
            for b in m["content"].as_array().into_iter().flatten() {
                match b["type"].as_str() {
                    Some("toolCall") if b["name"] == "yield" => called.push(b["arguments"].clone()),
                    Some("toolCall") => a.called(&b["arguments"]),
                    Some("text") => a.said(b["text"].as_str().unwrap_or_default()),
                    _ => {}
                }
            }
        }
        Some("toolResult") if m["toolName"] == "yield" && m["isError"] != true && m["details"]["status"] == "success" => {
            accepted.extend(m["details"].as_object().map(|d| Value::Object(d.clone())));
        }
        _ => {}
    }
}

/// One output from an agent's yields (`{type?, data}` each): a reviewer yields its findings one call
/// each (`type: ["findings"]`), as one array, or inside one object (`{overall_correctness,
/// explanation, findings}`); every finding is gathered into the last object's `findings`. Any other
/// agent's output is its last data.
fn yielded(calls: Vec<Value>) -> Option<Value> {
    let mut findings: Vec<Value> = Vec::new();
    let mut last: Option<Value> = None;
    for c in calls {
        let data = c.get("data").cloned().unwrap_or(Value::Null);
        let typed = c["type"].as_array().is_some_and(|t| t.iter().any(|x| x == "findings"));
        match data {
            Value::Array(items) if typed => findings.extend(items),
            Value::Object(_) if typed || data.get("title").is_some() && data.get("findings").is_none() => findings.push(data),
            Value::Object(mut o) => {
                findings.extend(o.remove("findings").and_then(|f| f.as_array().cloned()).unwrap_or_default());
                last = Some(Value::Object(o));
            }
            Value::Null => {}
            d => last = Some(d),
        }
    }
    if findings.is_empty() {
        return last;
    }
    let mut out = match last {
        Some(Value::Object(o)) => o,
        _ => serde_json::Map::new(),
    };
    out.insert("findings".into(), Value::Array(findings));
    Some(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sessions_task_agents_are_one_run_nested_ones_included() {
        let sessions = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/review_fix/omp/sessions"));
        let runs = session_runs(sessions, Some("01a0a00d-0000-7000-8000-000000000002"), None);
        assert_eq!(runs.len(), 1);
        let r = &runs[0];
        assert_eq!(
            (r.id.as_str(), r.started),
            ("2026-09-15T08-00-00-000Z_01a0a00d-0000-7000-8000-000000000002", borg_trace::parse_ts("2026-09-15T08:00:00.000Z"))
        );
        let rows: Vec<(&str, &str, &str)> = r.agents.iter().map(|a| (a.label.as_str(), a.kind.as_str(), a.prompt.as_str())).collect();
        assert_eq!(
            rows,
            [("FinalReview.Scout", "scout", "Find the client code."), ("FinalReview", "reviewer", "Review the network client.")],
            "in start order"
        );
        assert_eq!(r.agents[0].output, Some(serde_json::json!({"summary": "client in src/net"})), "another agent's output is its last data");
        assert!(session_runs(sessions, None, Some(borg_trace::parse_ts("2026-09-15T00:00:00Z"))).len() == 1, "since");
    }

    #[test]
    fn one_session_reads_only_the_main_file_its_dir_name_points_to() {
        let root = std::env::temp_dir().join(format!("borg-omp-suffix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let slug = root.join("-w-app");
        // `T1_aaa`'s main file claims `bbb`: a dir whose suffix is not the session is never read.
        for (dir, id) in [("T1_aaa", "bbb"), ("T2_bbb", "bbb")] {
            std::fs::create_dir_all(slug.join(dir)).unwrap();
            let rec = serde_json::json!({"type": "session", "id": id, "timestamp": "2026-09-15T08:00:00.000Z", "cwd": "/w/app"});
            std::fs::write(slug.join(format!("{dir}.jsonl")), format!("{rec}\n")).unwrap();
            std::fs::write(slug.join(dir).join("Build.jsonl"), format!("{rec}\n")).unwrap();
        }
        let ids = |s: &str| session_runs(&root, Some(s), None).into_iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(ids("bbb"), ["T2_bbb"]);
        assert!(ids("aaa").is_empty(), "the main file confirms the id");
        assert_eq!(session_runs(&root, None, None).len(), 2, "a full scan reads every one");
        let _ = std::fs::remove_dir_all(root);
    }
}
