//! omp (oh-my-pi) sessions: `~/.omp/agent/sessions/<slug>/<ts>_<id>.jsonl`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::{Event, Harness, Kind, Session, approx_tokens, parse_ts};

/// File headers inside omp's patch format: `[src/x.rs#06FE]` or `*** Update File: src/x.rs`.
static PATCH_FILE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^(?:\[([^\]#\n]+)(?:#[^\]\n]*)?\]|\*\*\* (?:Update|Add) File: (.+))$").unwrap());

fn local_path(cwd: &str, p: &str) -> Option<String> {
    let p = p.strip_prefix("local://").unwrap_or(p);
    if p.contains("://") {
        return None;
    }
    // `read` accepts `path:10-40` line selectors.
    let p = match p.rsplit_once(':') {
        Some((head, tail)) if tail.chars().next().is_some_and(|c| c.is_ascii_digit()) => head,
        _ => p,
    };
    Some(if p.starts_with('/') || cwd.is_empty() { p.to_string() } else { format!("{}/{}", cwd.trim_end_matches('/'), p) })
}

fn classify(name: &str, args: &Value, cwd: &str) -> Vec<Kind> {
    let s = |k: &str| args.get(k).and_then(Value::as_str);
    match name {
        "read" => s("path").and_then(|p| local_path(cwd, p)).map(|path| Kind::Read { path }).into_iter().collect(),
        "write" => s("path").and_then(|p| local_path(cwd, p)).map(|path| Kind::Edit { path }).into_iter().collect(),
        "edit" => {
            let patch = s("input").or_else(|| s("_input")).unwrap_or_default();
            let mut files: Vec<Kind> = PATCH_FILE
                .captures_iter(patch)
                .filter_map(|c| c.get(1).or_else(|| c.get(2)))
                .filter_map(|m| local_path(cwd, m.as_str().trim()))
                .map(|path| Kind::Edit { path })
                .collect();
            files.dedup();
            if files.is_empty() { s("path").and_then(|p| local_path(cwd, p)).map(|path| Kind::Edit { path }).into_iter().collect() } else { files }
        }
        "bash" => vec![Kind::Bash { cmd: s("command").unwrap_or_default().to_string(), failed: false, error: None, error_line: None }],
        "grep" | "glob" | "find" | "search" | "lsp" => vec![Kind::Search],
        n if n.starts_with("mcp__serena_") || n.starts_with("mcp__codebase-memory-mcp_") => vec![Kind::Search],
        _ => vec![Kind::Other],
    }
}

pub fn parse(path: &Path) -> std::io::Result<Session> {
    let file = std::fs::File::open(path)?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut sess = Session::new(stem, Harness::Omp);
    let mut pending: HashMap<String, (i64, Vec<Kind>)> = HashMap::new();
    let mut narrated = false;

    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let ts = v.get("timestamp").and_then(Value::as_str).map_or(0, parse_ts);
        match v.get("type").and_then(Value::as_str) {
            Some("session") => {
                if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                    sess.cwd = c.to_string();
                }
                if let Some(id) = v.get("id").and_then(Value::as_str) {
                    sess.id = id.to_string();
                }
                if v.get("parentSession").is_some() {
                    sess.sidechain = true;
                }
            }
            Some("message") => {
                sess.see(ts);
                let Some(msg) = v.get("message") else { continue };
                match msg.get("role").and_then(Value::as_str) {
                    Some("assistant") => {
                        if let Some(u) = msg.get("usage") {
                            let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                            sess.input_tokens += n("input") + n("cacheRead") + n("cacheWrite");
                            sess.output_tokens += n("output");
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
                                Some("toolCall") => {
                                    let name = block.get("name").and_then(Value::as_str).unwrap_or_default();
                                    let args = block.get("arguments").unwrap_or(&Value::Null);
                                    let kinds = classify(name, args, &sess.cwd);
                                    if kinds.iter().any(|k| matches!(k, Kind::Edit { .. })) {
                                        sess.reasoning.edits += 1;
                                        // omp tools carry a one-line intent (`i`) on every call: free provenance.
                                        let intent = args.get("i").and_then(Value::as_str).is_some_and(|t| t.len() > 5);
                                        sess.reasoning.edits_with_text += (narrated || intent) as u32;
                                    }
                                    narrated = false;
                                    if let Some(id) = block.get("id").and_then(Value::as_str) {
                                        pending.insert(id.to_string(), (ts, kinds));
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Some("toolResult") => {
                        let Some((cts, kinds)) = msg.get("toolCallId").and_then(Value::as_str).and_then(|id| pending.remove(id)) else {
                            continue;
                        };
                        let content = msg.get("content").unwrap_or(&Value::Null);
                        let failed = msg.get("isError").and_then(Value::as_bool) == Some(true);
                        let n = kinds.len().max(1) as u64;
                        let tokens = approx_tokens(content) / n;
                        for kind in kinds {
                            match kind {
                                Kind::Bash { cmd, .. } => sess.push_bash(cts.max(ts), cmd, failed, content),
                                kind => sess.events.push(Event { ts: cts.max(ts), kind, result_tokens: tokens }),
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Ok(sess)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_files() {
        let args = serde_json::json!({"input": "*** Begin Patch\n[src/a.rs#06FE]\nPUT 1:\n+x\n[local://plan.md#01]\n*** Update File: b/c.rs\n"});
        let kinds = classify("edit", &args, "/r");
        assert_eq!(kinds, vec![Kind::Edit { path: "/r/src/a.rs".into() }, Kind::Edit { path: "/r/plan.md".into() }, Kind::Edit { path: "/r/b/c.rs".into() },]);
    }

    #[test]
    fn read_line_selector() {
        assert_eq!(local_path("/r", "src/x.rs:10-20").as_deref(), Some("/r/src/x.rs"));
        assert_eq!(local_path("/r", "skill://codebase-memory"), None);
    }
}
