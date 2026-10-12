//! Execution memory (layer 1): scars and protocols, learned deterministically from what a
//! session did, at zero tokens.
//!
//! - **Scar:** an error fingerprint and what fixed it: a later passing run of the same command
//!   shape, with the different command that worked and/or the files edited in between.
//! - **Protocol:** a command shape that failed and then passed: the command that works here.
//!
//! Recall is a single line at the moment a drone hits a known error (or reruns a shape whose
//! protocol differs from what it just tried); otherwise nothing is injected.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fingerprint::{command_shape, error_fingerprint, invocation, is_test_failure, is_tool_shape};
use crate::{Kind, Session};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Memory {
    Scar {
        repo: String,
        /// Error fingerprint as hex (u64 does not survive JSON number precision).
        fp: String,
        error_line: String,
        shape: String,
        failed_cmd: String,
        /// The different command that passed, if the fix was a command change.
        fix_cmd: Option<String>,
        /// Files edited between the failure and the passing run.
        fix_edits: Vec<String>,
        ms: i64,
        session: String,
    },
    Protocol {
        repo: String,
        shape: String,
        cmd: String,
        failed_cmd: String,
        ms: i64,
        session: String,
    },
}

impl Memory {
    fn key(&self) -> String {
        match self {
            Memory::Scar { fp, .. } => format!("scar:{fp}"),
            Memory::Protocol { shape, .. } => format!("protocol:{shape}"),
        }
    }
    fn ms(&self) -> i64 {
        match self {
            Memory::Scar { ms, .. } | Memory::Protocol { ms, .. } => *ms,
        }
    }
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// What one session teaches: every failure whose command shape later passed in the same session.
pub fn learn(session: &Session, repo: &str) -> Vec<Memory> {
    let ev = &session.events;
    let mut out: HashMap<String, Memory> = HashMap::new();
    for (i, e) in ev.iter().enumerate() {
        let Kind::Bash { cmd, failed: true, error, error_line } = &e.kind else { continue };
        let shape = command_shape(cmd);
        if shape.is_empty() {
            continue;
        }
        let pass = ev[i + 1..].iter().position(|x| matches!(&x.kind, Kind::Bash { cmd: c, failed: false, .. } if command_shape(c) == shape));
        let Some(off) = pass else { continue };
        let j = i + 1 + off;
        let Kind::Bash { cmd: ok_cmd, .. } = &ev[j].kind else { continue };
        let mut edits: Vec<String> = Vec::new();
        for x in &ev[i + 1..j] {
            if let Kind::Edit { path } = &x.kind
                && !edits.contains(path)
            {
                edits.push(path.clone());
            }
        }
        let fix_cmd = (ok_cmd.trim() != cmd.trim()).then(|| clip(ok_cmd, 300));
        if let (Some(fp), Some(line)) = (error, error_line)
            && !is_test_failure(line)
            && (fix_cmd.is_some() || !edits.is_empty())
        {
            let m = Memory::Scar {
                repo: repo.into(),
                fp: format!("{fp:016x}"),
                error_line: line.clone(),
                shape: shape.clone(),
                failed_cmd: clip(cmd, 300),
                fix_cmd: fix_cmd.clone(),
                fix_edits: edits.into_iter().take(5).collect(),
                ms: ev[j].ts,
                session: session.id.clone(),
            };
            out.entry(m.key()).or_insert(m);
        }
        if let Some(fix) = fix_cmd.filter(|f| is_tool_shape(&shape) && invocation(f) != invocation(cmd)) {
            let m = Memory::Protocol { repo: repo.into(), shape, cmd: fix, failed_cmd: clip(cmd, 300), ms: ev[j].ts, session: session.id.clone() };
            out.insert(m.key(), m); // the latest working command wins
        }
    }
    let mut v: Vec<Memory> = out.into_values().collect();
    v.sort_by_key(Memory::ms);
    v
}

/// Latest memory per key: a later scar or protocol supersedes an earlier one.
pub fn latest(mems: impl IntoIterator<Item = Memory>) -> Vec<Memory> {
    let mut by: HashMap<String, Memory> = HashMap::new();
    for m in mems {
        let k = m.key();
        if by.get(&k).is_none_or(|old| old.ms() <= m.ms()) {
            by.insert(k, m);
        }
    }
    by.into_values().collect()
}

/// One line for a failure the collective has seen before, or None (0 tokens).
pub fn recall(mems: &[Memory], cmd: &str, error_text: &str) -> Option<String> {
    let shape = command_shape(cmd);
    if let Some(fp) = error_fingerprint(error_text).map(|f| format!("{f:016x}")) {
        for m in mems {
            if let Memory::Scar { fp: f, error_line, fix_cmd, fix_edits, .. } = m
                && *f == fp
            {
                let fix = match (fix_cmd, fix_edits.is_empty()) {
                    (Some(c), true) => format!("`{c}` passed"),
                    (Some(c), false) => format!("`{c}` passed after editing {}", fix_edits.join(", ")),
                    (None, _) => format!("it passed after editing {}", fix_edits.join(", ")),
                };
                return Some(format!("↳ borg scar: \"{}\" was hit before; {fix}.", clip(error_line, 160)));
            }
        }
    }
    mems.iter().find_map(|m| match m {
        Memory::Protocol { shape: s, cmd: ok, .. } if *s == shape && is_tool_shape(&shape) && invocation(ok) != invocation(cmd) => {
            Some(format!("↳ borg protocol: `{ok}` worked here before."))
        }
        _ => None,
    })
}

/// Append-only local store, one JSONL file per repo (`$BORG_HOME/memory/<repo>.jsonl`).
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(home: &Path) -> Self {
        Store { dir: home.join("memory") }
    }
    fn file(&self, repo: &str) -> PathBuf {
        let safe: String = repo.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        self.dir.join(format!("{safe}.jsonl"))
    }
    pub fn load(&self, repo: &str) -> Vec<Memory> {
        let text = std::fs::read_to_string(self.file(repo)).unwrap_or_default();
        latest(text.lines().filter_map(|l| serde_json::from_str(l).ok()))
    }
    /// Append what is new (unknown key, or a different command/fix than known).
    pub fn add(&self, repo: &str, mems: &[Memory]) -> std::io::Result<usize> {
        let known = self.load(repo);
        let fresh: Vec<&Memory> = mems.iter().filter(|m| !known.iter().any(|k| k.key() == m.key() && same_content(k, m))).collect();
        if fresh.is_empty() {
            return Ok(0);
        }
        std::fs::create_dir_all(&self.dir)?;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.file(repo))?;
        for m in &fresh {
            writeln!(f, "{}", serde_json::to_string(m).unwrap())?;
        }
        Ok(fresh.len())
    }
}

fn same_content(a: &Memory, b: &Memory) -> bool {
    match (a, b) {
        (Memory::Scar { fix_cmd: x, fix_edits: e, .. }, Memory::Scar { fix_cmd: y, fix_edits: f, .. }) => x == y && e == f,
        (Memory::Protocol { cmd: x, .. }, Memory::Protocol { cmd: y, .. }) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Event, Harness, Reasoning};

    fn bash(ts: i64, cmd: &str, out: Option<&str>) -> Event {
        let (failed, error, error_line) = match out {
            Some(o) => (true, error_fingerprint(o), crate::fingerprint::error_line(o)),
            None => (false, None, None),
        };
        Event { ts, kind: Kind::Bash { cmd: cmd.into(), failed, error, error_line }, result_tokens: 0 }
    }

    fn session(events: Vec<Event>) -> Session {
        Session {
            id: "s1".into(),
            harness: Harness::Claude,
            cwd: "/r".into(),
            sidechain: false,
            start: 0,
            end: 9,
            events,
            input_tokens: 0,
            output_tokens: 0,
            reasoning: Reasoning::default(),
        }
    }

    #[test]
    fn learns_scar_and_protocol_and_recalls_them() {
        let err = "ERROR: test_balance failed\nValueError: naive datetime";
        let s = session(vec![
            bash(1, "python3 -m pytest tests", Some(err)),
            Event { ts: 2, kind: Kind::Edit { path: "/r/ledger/clock.py".into() }, result_tokens: 0 },
            bash(3, "LEDGER_TZ=UTC python3 -m pytest tests", None),
        ]);
        let mems = learn(&s, "r");
        assert_eq!(mems.len(), 2, "{mems:?}");
        let line = recall(&mems, "python3 -m pytest tests -k balance", err).unwrap();
        assert!(line.contains("LEDGER_TZ=UTC") && line.contains("clock.py"), "{line}");
        // An unseen error and an unrelated command stay silent.
        assert!(recall(&mems, "cargo build", "error[E0425]: cannot find value").is_none());
    }

    #[test]
    fn flaky_rerun_is_not_a_scar() {
        let s = session(vec![bash(1, "npm test", Some("FAIL: timeout\nError: timed out")), bash(2, "npm test", None)]);
        assert!(learn(&s, "r").is_empty());
    }
}
