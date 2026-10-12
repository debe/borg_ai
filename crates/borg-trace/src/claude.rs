//! Claude Code transcripts: `~/.claude/projects/<slug>/<session>.jsonl`
//! (sub-agents under `<session>/subagents/`).

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::Value;

use crate::{Event, Harness, Kind, Session, approx_tokens, parse_ts};

struct Pending {
    ts: i64,
    kind: Kind,
}

fn abs(cwd: &str, p: &str) -> String {
    if p.starts_with('/') || cwd.is_empty() { p.to_string() } else { format!("{}/{}", cwd.trim_end_matches('/'), p) }
}

fn classify(name: &str, input: &Value, cwd: &str) -> Kind {
    let s = |k: &str| input.get(k).and_then(Value::as_str);
    match name {
        "Read" => s("file_path").map_or(Kind::Other, |p| Kind::Read { path: abs(cwd, p) }),
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            s("file_path").or_else(|| s("notebook_path")).map_or(Kind::Other, |p| Kind::Edit { path: abs(cwd, p) })
        }
        "Bash" => Kind::Bash { cmd: s("command").unwrap_or_default().to_string(), failed: false, error: None, error_line: None },
        "Grep" | "Glob" | "LS" => Kind::Search,
        n if n.starts_with("mcp__serena__") || n.starts_with("mcp__codebase-memory-mcp__") => Kind::Search,
        _ => Kind::Other,
    }
}

pub fn parse(path: &Path) -> std::io::Result<Session> {
    let file = std::fs::File::open(path)?;
    let id = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut sess = Session::new(id, Harness::Claude);
    sess.sidechain = path.components().any(|c| c.as_os_str() == "subagents");
    let mut pending: HashMap<String, Pending> = HashMap::new();
    let mut usage_seen: HashSet<String> = HashSet::new();
    let mut narrated = false;

    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let ts = v.get("timestamp").and_then(Value::as_str).map_or(0, parse_ts);
        if sess.cwd.is_empty()
            && let Some(c) = v.get("cwd").and_then(Value::as_str)
        {
            sess.cwd = c.to_string();
        }
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            sess.sidechain = true;
        }
        let Some(msg) = v.get("message") else { continue };
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                sess.see(ts);
                let mid = msg.get("id").and_then(Value::as_str).unwrap_or_default();
                if let Some(u) = msg.get("usage")
                    && usage_seen.insert(mid.to_string())
                {
                    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                    sess.input_tokens += n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
                    sess.output_tokens += n("output_tokens");
                }
                for block in msg.get("content").and_then(Value::as_array).into_iter().flatten() {
                    match block.get("type").and_then(Value::as_str) {
                        Some("thinking") => {
                            sess.reasoning.thinking_blocks += 1;
                            if block.get("thinking").and_then(Value::as_str).is_some_and(|t| !t.trim().is_empty()) {
                                sess.reasoning.thinking_nonempty += 1;
                            }
                        }
                        Some("text") => {
                            narrated |= block.get("text").and_then(Value::as_str).is_some_and(|t| t.trim().len() > 10);
                        }
                        Some("tool_use") => {
                            let name = block.get("name").and_then(Value::as_str).unwrap_or_default();
                            let input = block.get("input").unwrap_or(&Value::Null);
                            let kind = classify(name, input, &sess.cwd);
                            if matches!(kind, Kind::Edit { .. }) {
                                sess.reasoning.edits += 1;
                                sess.reasoning.edits_with_text += narrated as u32;
                            }
                            narrated = false;
                            if let Some(id) = block.get("id").and_then(Value::as_str) {
                                pending.insert(id.to_string(), Pending { ts, kind });
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                sess.see(ts);
                let Some(blocks) = msg.get("content").and_then(Value::as_array) else { continue };
                for block in blocks {
                    if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    let Some(p) = block.get("tool_use_id").and_then(Value::as_str).and_then(|id| pending.remove(id)) else {
                        continue;
                    };
                    let content = block.get("content").unwrap_or(&Value::Null);
                    let failed = block.get("is_error").and_then(Value::as_bool) == Some(true);
                    match p.kind {
                        Kind::Bash { cmd, .. } => sess.push_bash(p.ts.max(ts), cmd, failed, content),
                        kind => sess.events.push(Event { ts: p.ts.max(ts), kind, result_tokens: approx_tokens(content) }),
                    }
                }
            }
            _ => {}
        }
    }
    Ok(sess)
}
