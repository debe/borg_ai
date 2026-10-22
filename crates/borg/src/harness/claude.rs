//! Claude Code: native command hooks in `~/.claude/settings.json` (`$CLAUDE_CONFIG_DIR`), each running
//! `<borg> hook claude <event>`; its events are borg's canonical shape already. The one harness with a
//! transcript (the readers below), early delivery, steering in context and the question dialog.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Caps, Harness, Probe, Run, SubAgent, Transcript, Wiring};

/// Claude Code hook events and the canonical borg events they become.
pub const EVENTS: [(&str, &str); 6] = [
    ("UserPromptSubmit", "prompt"),
    // Only a paused drone's tool calls are refused here (steer.rs); otherwise a stat and out.
    ("PreToolUse", "pre_tool"),
    ("PostToolUse", "post_tool"),
    ("PostToolUseFailure", "post_tool_failure"),
    ("Stop", "stop"),
    ("SessionEnd", "end"),
];

/// Seconds Claude Code gives borg's Stop hook (its own default for a command hook is 60 s). The
/// expert drone keeps inside it (expert_route.rs `drone_cap`), so a slow drone never costs the
/// conscience's answer.
pub const STOP_TIMEOUT_S: u64 = 90;

pub struct Claude;

impl Harness for Claude {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn caps(&self) -> Caps {
        Caps {
            prompt_context: true,
            tool_context: true,
            early_delivery: true,
            steer_in_context: true,
            question_dialog: true,
            transcript: Transcript::Claude,
            deny_capture: true,
            sends_pid: false,
            // Not the 90 s install gives: the expert drone keeps inside Claude Code's 60 s default
            // (`DEFAULT_STOP_BUDGET`), so an install that predates that timeout keeps a margin too.
            stop_budget: None,
        }
    }

    fn wiring(&self) -> Wiring {
        Wiring::CommandHooks { events: &EVENTS, timeouts: &[("stop", STOP_TIMEOUT_S)], through_links: true }
    }

    fn target(&self, home: &Path, var: &dyn Fn(&str) -> Option<PathBuf>) -> PathBuf {
        var("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude")).join("settings.json")
    }

    fn probe(&self) -> Probe {
        Probe { cli: "claude", tested: "" }
    }

    /// Workflow runs: `<projects>/<slug>/<session>/workflows/wf_*.json` (`$CLAUDE_CONFIG_DIR/projects`,
    /// else `home/.claude/projects`).
    fn sub_runs(&self, home: &Path, session: Option<&str>, since: Option<i64>) -> Vec<Run> {
        let config = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()).map_or_else(|| home.join(".claude"), PathBuf::from);
        workflow_runs(&config.join("projects"), session, since)
    }
}

// ---------------------------------------------------------------- workflow runs

/// Every workflow run under a projects directory, oldest first: of `session` only when given, from
/// runs that started at or after `since`. Each run file lists its agents (label, phase, start); each
/// agent's transcript is `<session>/subagents/workflows/<runId>/agent-<id>.jsonl`, its output the
/// last `StructuredOutput` call.
pub fn workflow_runs(projects: &Path, session: Option<&str>, since: Option<i64>) -> Vec<Run> {
    let mut runs = Vec::new();
    for slug in read_dir(projects) {
        let dirs = match session {
            Some(s) => vec![slug.join(s)],
            None => read_dir(&slug),
        };
        for dir in dirs.into_iter().filter(|p| p.is_dir()) {
            let session = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            for f in read_dir(&dir.join("workflows")).into_iter().filter(|p| p.extension().is_some_and(|e| e == "json")) {
                let Some(run) = std::fs::read_to_string(&f).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { continue };
                let started = run.get("startTime").map_or(0, num);
                if since.is_none_or(|s| started >= s) {
                    let s = |k: &str| run.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                    runs.push(Run { id: s("runId"), session: session.clone(), name: s("workflowName"), started, agents: agents(&dir, &run) });
                }
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

fn num(v: &Value) -> i64 {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0)
}

/// The agents of one run, in start order.
fn agents(session_dir: &Path, run: &Value) -> Vec<SubAgent> {
    let id = run.get("runId").and_then(Value::as_str).unwrap_or_default();
    let mut out: Vec<SubAgent> = run
        .get("workflowProgress")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("workflow_agent"))
        .map(|p| {
            let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
            let mut a = SubAgent { label: s("label"), phase: s("phaseTitle"), started: p.get("startedAt").map_or(0, num), ..Default::default() };
            read_agent(&session_dir.join("subagents/workflows").join(id).join(format!("agent-{}.jsonl", s("agentId"))), &mut a);
            a
        })
        .collect();
    out.sort_by_key(|a| a.started);
    out
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// Read one agent transcript into `a`: its cwd, its first user message (the prompt), every tool input
/// and text, and its last `StructuredOutput`.
fn read_agent(path: &Path, a: &mut SubAgent) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if a.cwd.is_empty()
            && let Some(c) = v.get("cwd").and_then(Value::as_str)
        {
            a.cwd = c.to_string();
        }
        let Some(content) = v.get("message").and_then(|m| m.get("content")) else { continue };
        match v.get("type").and_then(Value::as_str) {
            Some("user") if a.prompt.is_empty() => a.prompt = text_of(content),
            Some("assistant") => {
                for b in content.as_array().into_iter().flatten() {
                    match b.get("type").and_then(Value::as_str) {
                        Some("tool_use") if b.get("name").and_then(Value::as_str) == Some("StructuredOutput") => a.output = b.get("input").cloned(),
                        Some("tool_use") => {
                            a.work.push_str(&b.get("input").map(Value::to_string).unwrap_or_default());
                            a.work.push('\n');
                        }
                        Some("text") => {
                            let t = b.get("text").and_then(Value::as_str).unwrap_or_default();
                            a.work.push_str(t);
                            a.work.push('\n');
                            a.final_text = t.to_string();
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- the transcript (JSONL)

/// The drone's last visible text before now, from the transcript (its proposal), its last 2000
/// characters.
pub fn last_assistant_text(transcript: &Path) -> String {
    let t = last_assistant_full(transcript);
    let chars: Vec<char> = t.chars().collect();
    chars[chars.len().saturating_sub(2000)..].iter().collect()
}

/// `last_assistant_text`, whole.
pub fn last_assistant_full(transcript: &Path) -> String {
    let text = std::fs::read_to_string(transcript).unwrap_or_default();
    for line in text.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["type"] != "assistant" {
            continue;
        }
        let t: String = v["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if !t.trim().is_empty() {
            return t;
        }
    }
    String::new()
}

/// This turn's work so far, from a Claude transcript (live.rs): the drone's text and its tool calls (name
/// and a short input: the path, or the command) since the engineer's last prompt, one per line, its last
/// `cap` characters. Mid-turn the last text block may be an earlier turn's (`last_assistant_full`), so
/// this reads the turn itself. Empty without a transcript.
pub fn turn_so_far(transcript: &Path, cap: usize) -> String {
    let text = std::fs::read_to_string(transcript).unwrap_or_default();
    let entries: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect();
    // The engineer's prompt: a user entry with text of its own, not a tool result or an injected note.
    let prompt = |v: &Value| {
        v["type"] == "user"
            && v["isMeta"] != true
            && match &v["message"]["content"] {
                Value::String(t) => !t.trim().is_empty(),
                Value::Array(bs) => bs.iter().any(|b| b["type"] == "text") && !bs.iter().any(|b| b["type"] == "tool_result"),
                _ => false,
            }
    };
    let start = entries.iter().rposition(prompt).map_or(0, |i| i + 1);
    let mut out: Vec<String> = Vec::new();
    for v in entries[start..].iter().filter(|v| v["type"] == "assistant") {
        for b in v["message"]["content"].as_array().into_iter().flatten() {
            match b["type"].as_str() {
                Some("text") => out.extend(b["text"].as_str().filter(|t| !t.trim().is_empty()).map(str::to_string)),
                Some("tool_use") => {
                    let input = &b["input"];
                    let short = ["file_path", "notebook_path", "command", "pattern", "path"]
                        .iter()
                        .find_map(|k| input[k].as_str())
                        .map_or_else(|| input.to_string(), str::to_string);
                    let short: String = short.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect();
                    out.push(format!("[{}] {short}", b["name"].as_str().unwrap_or("tool")));
                }
                _ => {}
            }
        }
    }
    let all = out.join("\n");
    let n = all.chars().count();
    all.chars().skip(n.saturating_sub(cap)).collect()
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_turn_so_far_is_this_turns_text_and_tool_calls_since_the_last_prompt() {
        let t = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/claude/live-turn.jsonl"));
        assert_eq!(turn_so_far(t, 4000), "I will replace the reset arc.\n[Read] /x/net.rs\n[Edit] /x/net.rs\n[Bash] cargo test -p net");
        assert_eq!(turn_so_far(t, 20), "[Bash] cargo test -p net".chars().skip(4).collect::<String>(), "the newest work is kept");
        assert_eq!(turn_so_far(Path::new("/nonexistent/t.jsonl"), 4000), "");
    }

    #[test]
    fn its_config_dir_variable_wins_unless_isolated() {
        let none = |_: &str| None;
        assert_eq!(Claude.target(Path::new("/h"), &none), Path::new("/h/.claude/settings.json"));
        let set = |k: &str| (k == "CLAUDE_CONFIG_DIR").then(|| PathBuf::from("/c"));
        assert_eq!(Claude.target(Path::new("/h"), &set), Path::new("/c/settings.json"));
    }
}
