//! Canonical touch streams from agent transcripts.
//!
//! Every harness (Claude Code, omp, later opencode) is reduced to the same
//! sequence of [`Event`]s. Phase 0 mines these for headroom; later the same
//! parser drives offline counterfactual replay of borg's recall policy.

pub mod claude;
pub mod dialogue;
pub mod fingerprint;
pub mod omp;
pub mod scars;

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Harness {
    Claude,
    Omp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// File content pulled into context.
    Read {
        path: String,
    },
    /// File changed by the agent.
    Edit {
        path: String,
    },
    /// Grep/glob/symbol search: exploration without a single target file.
    Search,
    /// Shell command. `error` is a normalized fingerprint when it failed, `error_line` the
    /// first error-looking line it was taken from (for showing a scar to a drone).
    Bash {
        cmd: String,
        failed: bool,
        error: Option<u64>,
        error_line: Option<String>,
    },
    Other,
}

#[derive(Debug, Clone)]
pub struct Event {
    /// Milliseconds since the Unix epoch.
    pub ts: i64,
    pub kind: Kind,
    /// Approximate tokens the tool result added to context (chars / 4).
    pub result_tokens: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Reasoning {
    pub thinking_blocks: u32,
    pub thinking_nonempty: u32,
    pub edits: u32,
    /// Edits whose assistant message (or the one before) carries visible text.
    pub edits_with_text: u32,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub harness: Harness,
    pub cwd: String,
    pub sidechain: bool,
    pub start: i64,
    pub end: i64,
    pub events: Vec<Event>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning: Reasoning,
}

impl Session {
    fn new(id: String, harness: Harness) -> Self {
        Session {
            id,
            harness,
            cwd: String::new(),
            sidechain: false,
            start: i64::MAX,
            end: i64::MIN,
            events: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
            reasoning: Reasoning::default(),
        }
    }

    fn see(&mut self, ts: i64) {
        if ts > 0 {
            self.start = self.start.min(ts);
            self.end = self.end.max(ts);
        }
    }

    /// Record a finished shell command; reads through the shell become Read touches.
    pub(crate) fn push_bash(&mut self, ts: i64, cmd: String, failed: bool, content: &serde_json::Value) {
        let tokens = approx_tokens(content);
        let reads = fingerprint::bash_read_paths(&cmd);
        let text = result_text(content);
        // Pure file reads print code, which may legitimately contain "Error:".
        let failed = failed || (!fingerprint::only_reads(&cmd) && fingerprint::looks_failed(&text));
        let error = if failed { fingerprint::error_fingerprint(&text) } else { None };
        let error_line = if failed { fingerprint::error_line(&text) } else { None };
        if reads.is_empty() {
            self.events.push(Event { ts, kind: Kind::Bash { cmd, failed, error, error_line }, result_tokens: tokens });
            return;
        }
        let share = tokens / reads.len() as u64;
        self.events.push(Event { ts, kind: Kind::Bash { cmd, failed, error, error_line }, result_tokens: 0 });
        for p in reads {
            let p = p.strip_prefix("./").map(str::to_string).unwrap_or(p);
            let path = if p.starts_with('/') || self.cwd.is_empty() { p } else { format!("{}/{}", self.cwd.trim_end_matches('/'), p) };
            self.events.push(Event { ts, kind: Kind::Read { path }, result_tokens: share });
        }
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty() || self.start > self.end
    }
}

/// Parse an RFC 3339 UTC timestamp (`2026-09-15T22:18:11.638Z`) to epoch ms.
pub fn parse_ts(s: &str) -> i64 {
    let b = s.as_bytes();
    if b.len() < 19 {
        return 0;
    }
    let num = |r: std::ops::Range<usize>| s.get(r).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
    let (y, mo, d) = (num(0..4), num(5..7), num(8..10));
    let (h, mi, sec) = (num(11..13), num(14..16), num(17..19));
    let ms = if b.get(19) == Some(&b'.') { num(20..23) } else { 0 };
    // Days from civil (Howard Hinnant).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    ((days * 24 + h) * 60 + mi) * 60_000 + sec * 1000 + ms
}

/// Approximate token count of a JSON tool result.
pub fn approx_tokens(v: &serde_json::Value) -> u64 {
    match v {
        serde_json::Value::String(s) => s.len() as u64 / 4,
        serde_json::Value::Array(a) => a.iter().map(approx_tokens).sum(),
        serde_json::Value::Object(o) => o.get("text").or_else(|| o.get("content")).map(approx_tokens).unwrap_or(0),
        _ => 0,
    }
}

/// Flatten a tool result into text (for error fingerprinting).
pub fn result_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => a.iter().map(result_text).collect::<Vec<_>>().join("\n"),
        serde_json::Value::Object(o) => o.get("text").or_else(|| o.get("content")).map(result_text).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Resolve the repository a session worked in: nearest ancestor with `.git`,
/// with borg/claude worktree suffixes folded back onto their parent repo.
/// Only a worktree itself folds: a repo of its own nested deeper inside a worktree (a test
/// fixture, a sandbox HOME's repos) keeps its own name.
pub fn repo_key(cwd: &str) -> String {
    let mut p = PathBuf::from(cwd);
    let mut at_git_root = false;
    let mut cur: Option<&Path> = Some(p.as_path());
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            p = dir.to_path_buf();
            at_git_root = true;
            break;
        }
        cur = dir.parent();
    }
    let s = p.to_string_lossy().into_owned();
    for marker in ["/.claude/worktrees/", "-worktrees/", "/.worktrees/"] {
        if let Some(i) = s.find(marker) {
            if at_git_root && s[i + marker.len()..].trim_end_matches('/').contains('/') {
                continue; // the git root lies below the worktree: a nested repo, not the worktree
            }
            return s[..i].trim_end_matches('/').to_string();
        }
    }
    s
}

/// All `*.jsonl` files below `root`.
pub fn jsonl_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                Ok(t) if t.is_file() && path.extension().is_some_and(|x| x == "jsonl") => out.push(path),
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timestamps() {
        assert_eq!(parse_ts("1970-01-01T00:00:00.000Z"), 0);
        assert_eq!(parse_ts("2026-09-15T22:18:11.638Z"), 1_789_510_691_638);
        assert_eq!(parse_ts("garbage"), 0);
    }

    #[test]
    fn folds_worktrees() {
        assert_eq!(repo_key("/nonexistent/repo/.claude/worktrees/x"), "/nonexistent/repo");
        assert_eq!(repo_key("/nonexistent/ai_lab-worktrees/lean-a"), "/nonexistent/ai_lab");
    }

    /// Found by the dogfood run: a repo nested inside a worktree (there, the sandbox HOME's
    /// `work/relay` under `target/`) was recorded as the worktree's parent repo.
    #[test]
    fn a_repo_nested_in_a_worktree_keeps_its_name() {
        let base = std::env::temp_dir().join(format!("borg-repo-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let wt = base.join("repo/.claude/worktrees/w");
        let nested = wt.join("target/home/work/relay");
        std::fs::create_dir_all(nested.join(".git")).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: elsewhere\n").unwrap();
        std::fs::create_dir_all(wt.join("src")).unwrap();
        let s = |p: &Path| p.to_string_lossy().into_owned();
        assert_eq!(repo_key(&s(&nested.join("src"))), s(&nested));
        assert_eq!(repo_key(&s(&wt.join("src"))), s(&base.join("repo")), "the worktree itself still folds");
        let _ = std::fs::remove_dir_all(&base);
    }
}
