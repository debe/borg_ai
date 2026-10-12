//! The conversation between engineer and drone, as the live hooks see it: the engineer's prompts,
//! the drone's visible text, what its tool calls touched, and tool calls the engineer refused with
//! words. The touch streams (`claude::parse`, `omp::parse`) drop all of this; replay evals of hook
//! logic (the queen) need it.
//!
//! Claude Code: `~/.claude/projects/<slug>/<session>.jsonl`; omp: `~/.omp/agent/sessions/<slug>/<ts>_<id>.jsonl`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::Value;

use crate::{Harness, fingerprint, parse_ts};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A prompt the engineer typed (harness meta, slash commands and interrupts dropped).
    Prompt { ts: i64, text: String },
    /// A submitted prompt that is not typed words: a slash command as typed (`/name args`) or a reply of
    /// 3 characters or fewer ("ok"). The live prompt hook sees these too (its `.prompts` file), so a
    /// replay of the hook's inputs needs them; corrections and turn boundaries do not.
    Input { ts: i64, text: String },
    /// One assistant message's visible text.
    Said { ts: i64, text: String },
    /// What a tool call touched, as the hook records it (paths, command head, search pattern).
    Touch { ts: i64, text: String },
    /// A tool call the engineer refused, with the first line of the harness's error text (the
    /// engineer's words are in it for a worded rejection).
    Refused { ts: i64, tool: String, error: String },
}

impl Step {
    pub fn ts(&self) -> i64 {
        match self {
            Step::Prompt { ts, .. } | Step::Input { ts, .. } | Step::Said { ts, .. } | Step::Touch { ts, .. } | Step::Refused { ts, .. } => *ts,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Dialogue {
    pub id: String,
    pub harness: Harness,
    pub cwd: String,
    /// Claude Code's `entrypoint` ("cli" interactive, "sdk-cli" headless `claude -p`); empty for omp.
    pub entrypoint: String,
    /// A sub-agent: never talks to the engineer.
    pub sidechain: bool,
    pub steps: Vec<Step>,
}

/// A prompt the engineer typed, not harness plumbing.
fn human(t: &str) -> bool {
    t.len() > 3
        && !t.starts_with('<')
        && !t.starts_with('/')
        && !t.starts_with('!')
        && !t.contains("[Request interrupted")
        && !t.starts_with("Stop hook feedback")
        && !t.starts_with("Caveat:")
}

/// What the prompt hook receives for a user message that `human` drops, if anything: a slash command
/// rebuilt as typed from Claude Code's `<command-name>`/`<command-args>` tags, a short reply, or a
/// `/command` as omp stores it. Interrupts, stop-hook feedback, caveats and other tags are none.
fn submitted(t: &str) -> Option<String> {
    let tag = |name: &str| {
        let open = format!("<{name}>");
        let start = t.find(&open)? + open.len();
        let end = t[start..].find(&format!("</{name}>"))? + start;
        Some(t[start..end].trim().to_string())
    };
    if let Some(cmd) = tag("command-name") {
        let args = tag("command-args").unwrap_or_default();
        return Some(if args.is_empty() { cmd } else { format!("{cmd} {args}") });
    }
    let plain = !t.is_empty() && !t.starts_with('<') && !t.starts_with('!') && !t.contains("[Request interrupted");
    (plain && !t.starts_with("Stop hook feedback") && !t.starts_with("Caveat:") && (t.len() <= 3 || t.starts_with('/'))).then(|| t.to_string())
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

/// What the hook records for a finished tool call (crates/borg/src/main.rs `touched`).
fn touched(tool: &str, input: &Value) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    match tool.to_lowercase().as_str() {
        "bash" => {
            let cmd = s("command");
            let paths = fingerprint::bash_read_paths(&cmd);
            if paths.is_empty() { cmd.chars().take(200).collect() } else { paths.join(" ") }
        }
        "grep" => format!("{} {}", s("pattern"), s("path")),
        "glob" => s("pattern"),
        _ => [s("file_path"), s("notebook_path"), s("path")].join(" ").trim().to_string(),
    }
}

fn lines(path: &Path) -> impl Iterator<Item = Value> {
    std::fs::File::open(path)
        .ok()
        .into_iter()
        .flat_map(|f| BufReader::new(f).lines())
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
}

fn ts(v: &Value) -> i64 {
    v.get("timestamp").and_then(Value::as_str).map_or(0, parse_ts)
}

/// Push a touch or refusal for a tool result.
fn result(steps: &mut Vec<Step>, ts: i64, tool: Option<&(String, Value)>, is_error: bool, content: &Value) {
    let Some((name, input)) = tool else { return };
    if is_error {
        let first: String = text_of(content).trim_start().lines().next().unwrap_or_default().chars().take(2000).collect();
        let full = text_of(content);
        let lower = full.to_lowercase();
        if lower.contains("want to proceed with this tool use") || lower.contains("denied by user") || lower.contains("rejected by user") {
            let error = if first.to_lowercase().contains("want to proceed") { full.chars().take(2000).collect() } else { first };
            steps.push(Step::Refused { ts, tool: format!("{name} {}", input.to_string().chars().take(300).collect::<String>()), error });
        }
        return;
    }
    let t = touched(name, input);
    if !t.trim().is_empty() {
        steps.push(Step::Touch { ts, text: t });
    }
}

/// One Claude Code transcript.
pub fn claude(path: &Path) -> Dialogue {
    let id = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut d = Dialogue { id, harness: Harness::Claude, cwd: String::new(), entrypoint: String::new(), sidechain: false, steps: Vec::new() };
    d.sidechain = path.components().any(|c| c.as_os_str() == "subagents");
    let mut tools: HashMap<String, (String, Value)> = HashMap::new();
    for v in lines(path) {
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            d.sidechain = true;
        }
        if d.cwd.is_empty()
            && let Some(c) = v.get("cwd").and_then(Value::as_str)
        {
            d.cwd = c.to_string();
        }
        if d.entrypoint.is_empty()
            && let Some(e) = v.get("entrypoint").and_then(Value::as_str)
        {
            d.entrypoint = e.to_string();
        }
        if v.get("isMeta") == Some(&Value::Bool(true)) || v.get("isCompactSummary") == Some(&Value::Bool(true)) {
            continue;
        }
        let Some(msg) = v.get("message") else { continue };
        let content = msg.get("content").unwrap_or(&Value::Null);
        let t = ts(&v);
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                let mut said = Vec::new();
                for b in content.as_array().into_iter().flatten() {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => said.push(b.get("text").and_then(Value::as_str).unwrap_or_default()),
                        Some("tool_use") => {
                            let name = b.get("name").and_then(Value::as_str).unwrap_or_default();
                            let id = b.get("id").and_then(Value::as_str).unwrap_or_default();
                            tools.insert(id.into(), (name.into(), b.get("input").cloned().unwrap_or(Value::Null)));
                        }
                        _ => {}
                    }
                }
                let said = said.join("\n");
                if !said.trim().is_empty() {
                    d.steps.push(Step::Said { ts: t, text: said });
                }
            }
            Some("user") => {
                let results: Vec<&Value> =
                    content.as_array().into_iter().flatten().filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")).collect();
                if results.is_empty() {
                    let text = text_of(content);
                    if human(text.trim()) {
                        d.steps.push(Step::Prompt { ts: t, text: text.trim().to_string() });
                    } else if let Some(text) = submitted(text.trim()) {
                        d.steps.push(Step::Input { ts: t, text });
                    }
                }
                for b in results {
                    let tool = b.get("tool_use_id").and_then(Value::as_str).and_then(|id| tools.get(id));
                    let is_error = b.get("is_error").and_then(Value::as_bool) == Some(true);
                    result(&mut d.steps, t, tool, is_error, b.get("content").unwrap_or(&Value::Null));
                }
            }
            _ => {}
        }
    }
    d
}

/// One omp session.
pub fn omp(path: &Path) -> Dialogue {
    let id = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut d = Dialogue { id, harness: Harness::Omp, cwd: String::new(), entrypoint: String::new(), sidechain: false, steps: Vec::new() };
    let mut tools: HashMap<String, (String, Value)> = HashMap::new();
    for v in lines(path) {
        let t = ts(&v);
        match v.get("type").and_then(Value::as_str) {
            Some("session") => {
                if v.get("parentSession").is_some_and(|p| !p.is_null()) {
                    d.sidechain = true;
                }
                if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                    d.cwd = c.to_string();
                }
                if let Some(id) = v.get("id").and_then(Value::as_str) {
                    d.id = id.to_string();
                }
            }
            Some("message") => {
                let Some(msg) = v.get("message") else { continue };
                let content = msg.get("content").unwrap_or(&Value::Null);
                match msg.get("role").and_then(Value::as_str) {
                    Some("user") => {
                        let text = text_of(content);
                        if human(text.trim()) {
                            d.steps.push(Step::Prompt { ts: t, text: text.trim().to_string() });
                        } else if let Some(text) = submitted(text.trim()) {
                            d.steps.push(Step::Input { ts: t, text });
                        }
                    }
                    Some("assistant") => {
                        let mut said = Vec::new();
                        for b in content.as_array().into_iter().flatten() {
                            match b.get("type").and_then(Value::as_str) {
                                Some("text") => said.push(b.get("text").and_then(Value::as_str).unwrap_or_default()),
                                Some("toolCall") => {
                                    let name = b.get("name").and_then(Value::as_str).unwrap_or_default();
                                    let id = b.get("id").and_then(Value::as_str).unwrap_or_default();
                                    tools.insert(id.into(), (name.into(), b.get("arguments").cloned().unwrap_or(Value::Null)));
                                }
                                _ => {}
                            }
                        }
                        let said = said.join("\n");
                        if !said.trim().is_empty() {
                            d.steps.push(Step::Said { ts: t, text: said });
                        }
                    }
                    Some("toolResult") => {
                        let tool = msg.get("toolCallId").and_then(Value::as_str).and_then(|id| tools.get(id));
                        let is_error = msg.get("isError").and_then(Value::as_bool) == Some(true);
                        result(&mut d.steps, t, tool, is_error, content);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_dialogue_keeps_prompts_text_touches_and_refusals() {
        let dir = std::env::temp_dir().join(format!("borg-dialogue-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s1.jsonl");
        let lines = [
            r#"{"type":"user","cwd":"/w/app","entrypoint":"cli","timestamp":"2026-09-01T10:00:00.000Z","message":{"content":"fix the parser"}}"#,
            r#"{"type":"user","isMeta":true,"timestamp":"2026-09-01T10:00:00.000Z","message":{"content":"Caveat: meta"}}"#,
            r#"{"type":"assistant","timestamp":"2026-09-01T10:00:01.000Z","message":{"content":[{"type":"text","text":"Reading it."},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/w/app/p.rs"}}]}}"#,
            r#"{"type":"user","timestamp":"2026-09-01T10:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"code"}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-09-01T10:00:03.000Z","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"git push origin main"}}]}}"#,
            r#"{"type":"user","timestamp":"2026-09-01T10:00:04.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"t2","is_error":true,"content":"The user doesn't want to proceed with this tool use. The tool use was rejected. the user said: not to main"}]}}"#,
            r#"{"type":"user","timestamp":"2026-09-01T10:00:05.000Z","message":{"content":"<command-name>/clear</command-name>"}}"#,
            r#"{"type":"user","timestamp":"2026-09-01T10:00:06.000Z","message":{"content":"no, use a branch"}}"#,
            r#"{"type":"user","timestamp":"2026-09-01T10:00:07.000Z","message":{"content":"<command-message>ship</command-message>\n<command-name>/ship</command-name>\n<command-args>to prod, keep the tag</command-args>"}}"#,
            r#"{"type":"user","timestamp":"2026-09-01T10:00:08.000Z","message":{"content":"[Request interrupted by user]"}}"#,
        ];
        std::fs::write(&p, lines.join("\n")).unwrap();
        let d = claude(&p);
        assert_eq!((d.cwd.as_str(), d.entrypoint.as_str(), d.sidechain), ("/w/app", "cli", false));
        let kinds: Vec<&str> = d
            .steps
            .iter()
            .map(|s| match s {
                Step::Prompt { .. } => "prompt",
                Step::Input { .. } => "input",
                Step::Said { .. } => "said",
                Step::Touch { .. } => "touch",
                Step::Refused { .. } => "refused",
            })
            .collect();
        assert_eq!(kinds, ["prompt", "said", "touch", "refused", "input", "prompt", "input"]);
        assert!(matches!(&d.steps[4], Step::Input { text, .. } if text == "/clear"));
        assert!(matches!(&d.steps[6], Step::Input { text, .. } if text == "/ship to prod, keep the tag"));
        assert!(matches!(&d.steps[3], Step::Refused { error, .. } if error.contains("not to main")));
        let _ = std::fs::remove_dir_all(dir);
    }
}
