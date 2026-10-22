//! opencode 1.x and 2.x: the TS plugin `adapters/opencode/borg.ts`, compiled in and copied to
//! `~/.config/opencode/plugins/borg.ts` (`$OPENCODE_CONFIG_DIR`, `$XDG_CONFIG_HOME/opencode`). The plugin
//! translates both ways, sends opencode's pid, emulates the stop's second round and sets its budget.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{Caps, Harness, Probe, Run, SubAgent, Transcript, Wiring};

pub const ADAPTER: &str = include_str!("../../assets/adapters/opencode/borg.ts");

pub struct Opencode;

impl Harness for Opencode {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn caps(&self) -> Caps {
        Caps {
            prompt_context: true,
            tool_context: true,
            // The channels exist (borg.ts); turning these on waits for an evaluation.
            early_delivery: false,
            steer_in_context: false,
            question_dialog: false,
            transcript: Transcript::None,
            deny_capture: true,
            sends_pid: true,
            // The adapter names the stop's budget itself (`BORG_STOP_BUDGET_MS`).
            stop_budget: None,
        }
    }

    fn wiring(&self) -> Wiring {
        Wiring::AdapterFile { source: ADAPTER }
    }

    fn target(&self, home: &Path, var: &dyn Fn(&str) -> Option<PathBuf>) -> PathBuf {
        let dir = var("OPENCODE_CONFIG_DIR").or_else(|| var("XDG_CONFIG_HOME").map(|x| x.join("opencode")));
        dir.unwrap_or_else(|| home.join(".config/opencode")).join("plugins/borg.ts")
    }

    fn probe(&self) -> Probe {
        Probe { cli: "opencode", tested: " (adapter tested with 1.18.32, 1.19.0, 2.0.23 and 2.0.29)" }
    }

    /// Each session's subagents, through opencode's CLI (read-only, as `borg assimilate` reads it), and
    /// only where opencode keeps data under `home` (`$XDG_DATA_HOME/opencode`, else
    /// `home/.local/share/opencode`). Every session: 1.x only (2.x has no query that lists them).
    fn sub_runs(&self, home: &Path, session: Option<&str>, since: Option<i64>) -> Vec<Run> {
        let data = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()).map_or_else(|| home.join(".local/share"), PathBuf::from);
        if !data.join("opencode").is_dir() {
            return Vec::new();
        }
        let here = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
        session_runs(session, since, &|id, dir| crate::assimilate::opencode_export(id, dir.unwrap_or(&here)), &crate::assimilate::opencode_db)
    }
}

// ---------------------------------------------------------------- subagents

/// Exports a session by id, in the directory given (else wherever it runs).
pub type Export<'a> = &'a dyn Fn(&str, Option<&Path>) -> Option<Value>;
/// Runs a read-only query of opencode 1.x's database (None: no such query).
pub type Query<'a> = &'a dyn Fn(&str) -> Option<Vec<Value>>;

/// A guard, not a tuning knob: at most this many children of one session are exported, the rest read
/// from the parent's copy of their final text.
pub const CHILDREN: usize = 20;
/// All of one session's exports stop after this long; the remaining children are read from the parent.
const SESSION_BUDGET: Duration = Duration::from_secs(120);

/// An opencode session id (`ses_` and alphanumerics), the only thing passed to its CLI.
fn session_id(s: &str) -> bool {
    s.starts_with("ses_") && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// One run per main session that started subagents, oldest first: `session` (a `ses_…` id) only, else
/// every parent of a subagent session since `since` (1.x's query). A subagent is a child session; the
/// parent's task tool call (1.x `task`, 2.x `subagent`) names it (`metadata.sessionId`/`sessionID`)
/// with its description, agent and prompt, and holds its final text. Its export gives its cwd and
/// work. opencode has no structured output: `output` stays None.
pub fn session_runs(session: Option<&str>, since: Option<i64>, export: Export, query: Query) -> Vec<Run> {
    let roots: Vec<String> = match session {
        Some(s) if session_id(s) => vec![s.to_string()],
        Some(_) => Vec::new(),
        None => {
            let since = since.map(|ms| format!(" AND time_created >= {ms}")).unwrap_or_default();
            let sql = format!("SELECT parent_id AS id, MIN(time_created) AS t FROM session WHERE parent_id IS NOT NULL{since} GROUP BY parent_id ORDER BY t");
            query(&sql).unwrap_or_default().iter().filter_map(|r| r["id"].as_str().filter(|s| session_id(s)).map(String::from)).collect()
        }
    };
    let mut runs: Vec<Run> = roots
        .iter()
        .filter_map(|id| {
            let until = Instant::now() + SESSION_BUDGET;
            run_of(&export(id, None)?, export, until)
        })
        .collect();
    runs.sort_by_key(|r| r.started);
    runs
}

/// A message's parts: 1.x `parts`, 2.x `content`.
fn parts(m: &Value) -> impl Iterator<Item = &Value> {
    m["parts"].as_array().or(m["content"].as_array()).into_iter().flatten()
}

fn assistant(m: &Value) -> bool {
    m["info"]["role"] == "assistant" || m["type"] == "assistant"
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(bs) => bs.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// The run of `root`'s children: each exported while the id is valid, fewer than `CHILDREN` were and
/// `until` has not passed; else read from the parent's copy of its final text.
fn run_of(root: &Value, export: Export, until: Instant) -> Option<Run> {
    let info = &root["info"];
    let dir = info["directory"].as_str().or(info["location"]["directory"].as_str()).unwrap_or_default();
    let mut agents = Vec::new();
    let mut exported = 0;
    for m in root["messages"].as_array().into_iter().flatten() {
        let at = m["info"]["time"]["created"].as_i64().or(m["time"]["created"].as_i64()).unwrap_or(0);
        for p in parts(m).filter(|p| p["type"] == "tool") {
            let state = &p["state"];
            let Some(child) = state["metadata"]["sessionId"].as_str().or(state["metadata"]["sessionID"].as_str()) else { continue };
            let input = &state["input"];
            let s = |k: &str| input[k].as_str().unwrap_or_default().to_string();
            let started = state["time"]["start"].as_i64().or(p["time"]["created"].as_i64()).unwrap_or(at);
            let mut a = SubAgent {
                label: s("description"),
                kind: input["subagent_type"].as_str().map_or_else(|| s("agent"), String::from),
                started,
                prompt: s("prompt"),
                cwd: dir.to_string(),
                ..Default::default()
            };
            let read = session_id(child) && exported < CHILDREN && Instant::now() < until;
            exported += usize::from(read);
            match read.then(|| export(child, (!dir.is_empty()).then_some(Path::new(dir)))).flatten() {
                Some(c) => {
                    let ci = &c["info"];
                    if let Some(d) = ci["directory"].as_str().or(ci["location"]["directory"].as_str()) {
                        a.cwd = d.to_string();
                    }
                    for p in c["messages"].as_array().into_iter().flatten().filter(|m| assistant(m)).flat_map(parts) {
                        match p["type"].as_str() {
                            Some("tool") => a.work.push_str(&format!("{}\n", p["state"]["input"])),
                            Some("text") => {
                                let t = p["text"].as_str().unwrap_or_default();
                                a.work.push_str(&format!("{t}\n"));
                                a.final_text = t.to_string();
                            }
                            _ => {}
                        }
                    }
                }
                // Without its export (or not read), its final text as the parent saw it.
                None => {
                    a.work = state["output"].as_str().map_or_else(|| text(&state["content"]), String::from);
                    a.final_text = a.work.clone();
                }
            }
            agents.push(a);
        }
    }
    if agents.is_empty() {
        return None;
    }
    agents.sort_by_key(|a| a.started);
    let id = info["id"].as_str().unwrap_or_default().to_string();
    let started = info["time"]["created"].as_i64().unwrap_or(agents[0].started);
    Some(Run { session: id.clone(), id, name: info["title"].as_str().unwrap_or_default().to_string(), started, agents })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    /// A parent with one task call per child id, each with its final text.
    fn parent(children: &[String]) -> Value {
        let parts: Vec<Value> = children
            .iter()
            .map(|c| json!({"type": "tool", "state": {"input": {"description": c}, "metadata": {"sessionId": c}, "output": format!("parent's copy of {c}")}}))
            .collect();
        json!({"info": {"id": "ses_root", "directory": "/w/app"}, "messages": [{"info": {"role": "assistant"}, "parts": parts}]})
    }

    fn child(id: &str) -> Value {
        json!({"info": {"directory": "/w/app"}, "messages": [{"info": {"role": "assistant"}, "parts": [{"type": "text", "text": format!("export of {id}")}]}]})
    }

    #[test]
    fn only_valid_ids_are_exported_and_at_most_children_of_them() {
        let asked = RefCell::new(Vec::new());
        let export = |id: &str, _: Option<&Path>| {
            asked.borrow_mut().push(id.to_string());
            Some(child(id))
        };
        let mut ids: Vec<String> = (0..CHILDREN + 5).map(|i| format!("ses_c{i}")).collect();
        ids.insert(0, "--help".into());
        ids.insert(1, "ses_../x".into());
        let run = run_of(&parent(&ids), &export, Instant::now() + SESSION_BUDGET).unwrap();
        let asked = asked.into_inner();
        assert_eq!(asked.len(), CHILDREN, "{asked:?}");
        assert!(asked.iter().all(|id| session_id(id)), "{asked:?}");
        let works: Vec<&str> = run.agents.iter().map(|a| a.work.trim()).collect();
        assert_eq!((works[0], works[1], works[2]), ("parent's copy of --help", "parent's copy of ses_../x", "export of ses_c0"));
        assert_eq!(works.last().copied(), Some(format!("parent's copy of ses_c{}", CHILDREN + 4).as_str()));
    }

    #[test]
    fn past_the_sessions_deadline_children_are_read_from_the_parent() {
        let export = |_: &str, _: Option<&Path>| -> Option<Value> { panic!("no export after the deadline") };
        let run = run_of(&parent(&["ses_c0".into()]), &export, Instant::now()).unwrap();
        assert_eq!(run.agents[0].work, "parent's copy of ses_c0");
    }
}
