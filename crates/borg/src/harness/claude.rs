//! Claude Code: native command hooks in `~/.claude/settings.json` (`$CLAUDE_CONFIG_DIR`), each running
//! `<borg> hook claude <event>`; its events are borg's canonical shape already. The one harness with a
//! transcript (the readers below), early delivery and the question dialog.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Caps, DroneWake, Harness, Probe, Run, STOP_TIMEOUT_S, SubAgent, Transcript, Wiring};

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

/// Hooks an older borg installed: `SessionStart` served the hive's drones (0.3.0 and before).
pub const RETIRED: [(&str, &str); 1] = [("SessionStart", "start")];

// The Stop hook's `timeout` install writes is `STOP_TIMEOUT_S`. Claude Code's own default for a command
// hook is 600 s now (its hooks docs, checked 2026-10-27; earlier versions gave 60 s). The expert drone
// keeps inside it (expert_route.rs `drone_cap`), so a slow drone never costs the conscience's answer.

/// Protocol (Claude Code's): an `additionalContext` (or `systemMessage`, or plain stdout) longer than
/// this is saved to a file, and the model gets a preview and the file's path instead of the text.
/// Claude Code 2.1.312's bundle, read 2026-10-28: the hook-output threshold `1e4`, characters, `<=` kept
/// whole.
pub const CONTEXT_CHARS: usize = 10_000;

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
            question_dialog: true,
            transcript: Transcript::Claude,
            deny_capture: true,
            sends_pid: false,
            // Not the 90 s install gives: the expert drone keeps inside the 60 s of `DEFAULT_STOP_BUDGET`,
            // so an install that predates that timeout, on a Claude Code whose default was 60 s, keeps a
            // margin too.
            stop_budget: None,
            context_chars: CONTEXT_CHARS,
            session_env: Some("CLAUDE_CODE_SESSION_ID"),
            pid_env: None,
            drone_wake: DroneWake::Model,
        }
    }

    fn wiring(&self) -> Wiring {
        Wiring::CommandHooks { events: &EVENTS, timeouts: &[("stop", STOP_TIMEOUT_S)], retired: &RETIRED, through_links: true }
    }

    fn target(&self, home: &Path, var: &dyn Fn(&str) -> Option<PathBuf>) -> PathBuf {
        var("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude")).join("settings.json")
    }

    fn probe(&self) -> Probe {
        Probe { cli: "claude", tested: "" }
    }

    /// Workflow runs (`<projects>/<slug>/<session>/workflows/wf_*.json`) and each session's plain
    /// sub-agents (the Agent tool, [`agent_runs`]), under `$CLAUDE_CONFIG_DIR/projects`, else
    /// `home/.claude/projects`.
    fn sub_runs(&self, home: &Path, session: Option<&str>, since: Option<i64>) -> Vec<Run> {
        let config = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()).map_or_else(|| home.join(".claude"), PathBuf::from);
        project_runs(&config.join("projects"), session, since)
    }
}

// ---------------------------------------------------------------- workflow runs

/// The workflow runs and the plain sub-agents' runs under a projects directory, oldest first (see
/// [`workflow_runs`], [`agent_runs`]).
pub fn project_runs(projects: &Path, session: Option<&str>, since: Option<i64>) -> Vec<Run> {
    let mut runs = workflow_runs(projects, session, since);
    let mut loose = agent_runs(projects, session, since);
    // An Agent-tool sub-agent that a workflow's agent started is part of that workflow's run.
    for l in &mut loose {
        for a in std::mem::take(&mut l.agents) {
            match runs.iter_mut().find(|r| r.session == l.session && r.agents.iter().any(|w| !a.parent.is_empty() && w.id == a.parent)) {
                Some(r) => r.agents.push(a),
                None => l.agents.push(a),
            }
        }
    }
    for r in &mut runs {
        r.agents.sort_by_key(|a| a.started);
    }
    runs.extend(loose.into_iter().filter(|l| !l.agents.is_empty()));
    runs.sort_by_key(|r| r.started);
    runs
}

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
                    runs.push(Run { id: s("runId"), session: session.clone(), name: s("workflowName"), started, agents: agents(&dir, &run), loose: false });
                }
            }
        }
    }
    runs.sort_by_key(|r| r.started);
    runs
}

/// One run per session that ran plain sub-agents (the Agent tool, outside any workflow), oldest first:
/// of `session` only when given, from runs whose first agent started at or after `since`. Each agent's
/// transcript is `<session>/subagents/agent-<id>.jsonl`, its description, type, the agent that started
/// it (`parentAgentId`, none when the session's own agent did) and the Agent call that did
/// (`toolUseId`) in `agent-<id>.meta.json` beside it; it has no structured output, so its findings come
/// from its text. The run's id is `agents-<session>`, and it is [`Run::loose`].
pub fn agent_runs(projects: &Path, session: Option<&str>, since: Option<i64>) -> Vec<Run> {
    let mut runs = Vec::new();
    for slug in read_dir(projects) {
        let dirs = match session {
            Some(s) => vec![slug.join(s)],
            None => read_dir(&slug),
        };
        for dir in dirs.into_iter().filter(|p| p.is_dir()) {
            let session = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let mut agents: Vec<SubAgent> = read_dir(&dir.join("subagents"))
                .into_iter()
                .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
                .map(|f| {
                    let meta = std::fs::read_to_string(f.with_extension("meta.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
                    let s = |k: &str| meta.as_ref().and_then(|m| m.get(k)).and_then(Value::as_str).unwrap_or_default().to_string();
                    let id = f.file_stem().map(|n| n.to_string_lossy().trim_start_matches("agent-").to_string()).unwrap_or_default();
                    let mut a = SubAgent { id, parent: s("parentAgentId"), label: s("description"), kind: s("agentType"), ..Default::default() };
                    read_agent_from(&f, &s("toolUseId"), &mut a);
                    a
                })
                .collect();
            agents.sort_by_key(|a| a.started);
            let Some(started) = agents.first().map(|a| a.started) else { continue };
            if since.is_none_or(|s| started >= s) {
                runs.push(Run { id: format!("agents-{session}"), session, name: String::new(), started, loose: true, agents });
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
            let mut a =
                SubAgent { id: s("agentId"), label: s("label"), phase: s("phaseTitle"), started: p.get("startedAt").map_or(0, num), ..Default::default() };
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

/// Read one agent transcript into `a`: its cwd, its start (where `a` has none), its first user message
/// (the prompt), every tool input and text, and its last `StructuredOutput`.
fn read_agent(path: &Path, a: &mut SubAgent) {
    read_agent_from(path, "", a);
}

/// [`read_agent`] of an agent the Agent call `call` started. A fork's transcript opens with that call,
/// copied from its parent, and a user message of fork boilerplate: the call's prompt is its prompt,
/// and the call is not its work.
fn read_agent_from(path: &Path, call: &str, a: &mut SubAgent) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if a.started == 0 {
            a.started = v.get("timestamp").and_then(Value::as_str).map_or(0, borg_trace::parse_ts);
        }
        if a.cwd.is_empty()
            && let Some(c) = v.get("cwd").and_then(Value::as_str)
        {
            a.cwd = c.to_string();
        }
        let Some(content) = v.get("message").and_then(|m| m.get("content")) else { continue };
        match v.get("type").and_then(Value::as_str) {
            Some("user") if a.prompt.is_empty() => a.prompt = text_of(content),
            Some("assistant") => content.as_array().into_iter().flatten().for_each(|b| read_block(b, call, a)),
            _ => {}
        }
    }
}

/// One content block of an assistant message of [`read_agent_from`] into `a`.
fn read_block(b: &Value, call: &str, a: &mut SubAgent) {
    match b.get("type").and_then(Value::as_str) {
        Some("tool_use") if b.get("name").and_then(Value::as_str) == Some("StructuredOutput") => a.output = b.get("input").cloned(),
        Some("tool_use") if !call.is_empty() && b.get("id").and_then(Value::as_str) == Some(call) => {
            a.prompt = b.get("input").and_then(|i| i.get("prompt")).and_then(Value::as_str).unwrap_or_default().to_string();
        }
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

// ---------------------------------------------------------------- the transcript (JSONL)

/// The drone's last visible text before now, from the transcript (its proposal), its last
/// `learn::PROPOSAL_CHARS` characters (the end an episode keeps).
pub fn last_assistant_text(transcript: &Path) -> String {
    let t = last_assistant_full(transcript);
    let chars: Vec<char> = t.chars().collect();
    chars[chars.len().saturating_sub(crate::learn::PROPOSAL_CHARS)..].iter().collect()
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
/// this reads the turn itself. Empty without a transcript. Read from the end (`TAIL_CHUNK`) until the
/// engineer's last prompt or `cap` characters of the turn, whichever comes first: a hook delivering
/// lessons in a long session never reads or parses the whole transcript.
pub fn turn_so_far(transcript: &Path, cap: usize) -> String {
    turn_tail(transcript, cap, TAIL_CHUNK)
}

/// Method: the first read from a transcript's end (`turn_so_far`), doubled at each further one, so a
/// turn's tail takes one small read and a long turn a few.
const TAIL_CHUNK: u64 = 64 * 1024;

/// Guard: a tool call's input keeps this many characters on its line of the turn so far (a command with
/// a heredoc would otherwise fill it).
const TOOL_CHARS: usize = 200;

/// `turn_so_far`, its first read `chunk` bytes from the end.
fn turn_tail(transcript: &Path, cap: usize, chunk: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(transcript) else { return String::new() };
    let mut start = f.metadata().map_or(0, |m| m.len());
    // The turn's lines, newest first, and how many characters they make joined.
    let (mut newest, mut chars): (Vec<String>, usize) = (Vec::new(), 0);
    // What was read from `start` on and not scanned yet: the end of a line that began before `start`.
    let mut rest: Vec<u8> = Vec::new();
    let mut step = chunk.max(1);
    'read: while chars < cap && start > 0 {
        let from = start.saturating_sub(step);
        let Ok(n) = usize::try_from(start - from) else { break };
        let mut buf = vec![0; n];
        if f.seek(SeekFrom::Start(from)).and_then(|_| f.read_exact(&mut buf)).is_err() {
            break;
        }
        buf.extend_from_slice(&rest);
        (start, step) = (from, step.saturating_mul(2));
        // Up to the first newline, unless the file starts there, is a line not read whole yet.
        let whole = if start == 0 { 0 } else { buf.iter().position(|b| *b == b'\n').map_or(buf.len(), |i| i + 1) };
        for line in buf[whole..].split(|b| *b == b'\n').rev() {
            let Ok(v) = serde_json::from_slice::<Value>(line) else { continue };
            if is_prompt(&v) {
                break 'read;
            }
            if v["type"] == "assistant" {
                for piece in pieces(&v).into_iter().rev() {
                    chars += piece.chars().count() + usize::from(!newest.is_empty());
                    newest.push(piece);
                }
                if chars >= cap {
                    break 'read;
                }
            }
        }
        rest = buf[..whole].to_vec();
    }
    newest.reverse();
    let all = newest.join("\n");
    let n = all.chars().count();
    all.chars().skip(n.saturating_sub(cap)).collect()
}

/// The engineer's prompt: a user entry with text of its own, not a tool result or an injected note.
fn is_prompt(v: &Value) -> bool {
    v["type"] == "user"
        && v["isMeta"] != true
        && match &v["message"]["content"] {
            Value::String(t) => !t.trim().is_empty(),
            Value::Array(bs) => bs.iter().any(|b| b["type"] == "text") && !bs.iter().any(|b| b["type"] == "tool_result"),
            _ => false,
        }
}

/// An assistant entry's lines of the turn so far: each text, and each tool call as `[name] input`.
fn pieces(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for b in v["message"]["content"].as_array().into_iter().flatten() {
        match b["type"].as_str() {
            Some("text") => out.extend(b["text"].as_str().filter(|t| !t.trim().is_empty()).map(str::to_string)),
            Some("tool_use") => {
                let input = &b["input"];
                let short = ["file_path", "notebook_path", "command", "pattern", "path"]
                    .iter()
                    .find_map(|k| input[k].as_str())
                    .map_or_else(|| input.to_string(), str::to_string);
                let short: String = short.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(TOOL_CHARS).collect();
                out.push(format!("[{}] {short}", b["name"].as_str().unwrap_or("tool")));
            }
            _ => {}
        }
    }
    out
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
        // Read from the end in reads of any size, a line cut by one included, the turn is the same.
        for chunk in [1, 7, 64, 333, 1 << 20] {
            assert_eq!(turn_tail(t, 4000, chunk), turn_so_far(t, 4000), "chunk {chunk}");
            assert_eq!(turn_tail(t, 20, chunk), turn_so_far(t, 20), "chunk {chunk}");
        }
        assert_eq!(turn_so_far(t, 0), "");
    }

    /// The turn is read from the end and the scan stops at the engineer's last prompt: what came before
    /// it is never parsed, so bytes no reader could take there leave the turn as it is (reading the whole
    /// file as text gave nothing at all).
    #[test]
    fn the_turn_so_far_reads_back_only_to_the_last_prompt() {
        let dir = std::env::temp_dir().join(format!("borg-claude-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = dir.join("t.jsonl");
        let line = |v: Value| format!("{v}\n").into_bytes();
        let mut bytes = vec![0xff; 300_000];
        bytes.extend(line(serde_json::json!({"type": "user", "message": {"content": "Drain the budget place"}})));
        let text = |s: &str| serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": s}]}});
        bytes.extend(line(text("I will replace the reset arc.")));
        bytes.extend(line(text("Done.")));
        std::fs::write(&t, &bytes).unwrap();
        assert_eq!(turn_tail(&t, 4000, 64), "I will replace the reset arc.\nDone.");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_workflow_agents_sub_agent_joins_its_run_and_a_fork_is_prompted_by_its_call() {
        let projects = std::env::temp_dir().join(format!("borg-claude-agents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&projects);
        let dir = projects.join("-w-app/s1");
        std::fs::create_dir_all(dir.join("subagents/workflows/wf_1")).unwrap();
        std::fs::create_dir_all(dir.join("workflows")).unwrap();
        let line = |v: Value| format!("{v}\n");
        let user = |ts: &str, content: Value| line(serde_json::json!({"type": "user", "timestamp": ts, "cwd": "/w/app", "message": {"content": content}}));
        let progress = serde_json::json!([{"type": "workflow_agent", "label": "fix", "phaseTitle": "Fix", "agentId": "w1", "startedAt": "1000"}]);
        let run = serde_json::json!({"runId": "wf_1", "workflowName": "build", "startTime": 1000, "workflowProgress": progress});
        std::fs::write(dir.join("workflows/wf_1.json"), run.to_string()).unwrap();
        std::fs::write(dir.join("subagents/workflows/wf_1/agent-w1.jsonl"), user("1970-01-01T00:00:01.000Z", "fix it".into())).unwrap();
        let sub = dir.join("subagents");
        let meta = |id: &str, m: Value| std::fs::write(sub.join(format!("agent-{id}.meta.json")), m.to_string()).unwrap();
        meta("n1", serde_json::json!({"agentType": "general-purpose", "description": "Fix the arc", "parentAgentId": "w1"}));
        std::fs::write(sub.join("agent-n1.jsonl"), user("1970-01-01T00:00:02.000Z", "edit src/net.rs".into())).unwrap();
        meta("f1", serde_json::json!({"agentType": "fork", "isFork": true, "description": "Review the net", "toolUseId": "toolu_1"}));
        let call = serde_json::json!({"type": "assistant", "timestamp": "1970-01-01T00:00:03.000Z", "cwd": "/w/app", "message": {"content": [
            {"type": "tool_use", "id": "toolu_1", "name": "Agent", "input": {"subagent_type": "fork", "prompt": "review the net diff"}}
        ]}});
        let boilerplate =
            serde_json::json!([{"type": "tool_result", "tool_use_id": "toolu_1"}, {"type": "text", "text": "<fork-boilerplate>You are a worker fork."}]);
        let fork = line(serde_json::json!({"type": "fork-context-ref", "agentId": "f1"})) + &line(call) + &user("1970-01-01T00:00:03.000Z", boilerplate);
        std::fs::write(sub.join("agent-f1.jsonl"), fork).unwrap();
        let runs = project_runs(&projects, None, None);
        let agents = |r: &Run| r.agents.iter().map(|a| a.id.clone()).collect::<Vec<_>>();
        assert_eq!(
            runs.iter().map(|r| (r.id.as_str(), r.loose, agents(r))).collect::<Vec<_>>(),
            [("wf_1", false, vec!["w1".to_string(), "n1".into()]), ("agents-s1", true, vec!["f1".into()])]
        );
        let f = &runs[1].agents[0];
        assert_eq!((f.prompt.as_str(), f.work.as_str()), ("review the net diff", ""), "the call is the parent's, not the fork's work");
        let _ = std::fs::remove_dir_all(projects);
    }

    #[test]
    fn its_config_dir_variable_wins_unless_isolated() {
        let none = |_: &str| None;
        assert_eq!(Claude.target(Path::new("/h"), &none), Path::new("/h/.claude/settings.json"));
        let set = |k: &str| (k == "CLAUDE_CONFIG_DIR").then(|| PathBuf::from("/c"));
        assert_eq!(Claude.target(Path::new("/h"), &set), Path::new("/c/settings.json"));
    }
}
