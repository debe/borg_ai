//! Who is working where, right now: one tiny record per drone session, for `borg top` (top.rs) and
//! for steering (steer.rs) to name a drone.
//!
//! `<store>/presence/<session>.json`, rewritten whole by every hook call (one small write to a temp
//! file, then a rename: a reader never sees half a record). No extra process, no network: the
//! engineer (git user.email) and the repo are looked up once, on the session's first hook, and then
//! carried over from the previous record.
//!
//! A session is `working` until its turn ends (`stop`: `waiting`), `idle` after 30 minutes without a
//! hook, `ended` at `end` (or when the harness gave a pid that is gone). Records untouched for a day
//! are pruned (at a prompt and by `borg top`). Remote drones (other members' sessions, carried by the
//! mesh daemon when the collective may sync) are in `<store>/mesh/presence.jsonl`, never in lessons.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::learn;

/// Without a hook for this long a session shows as idle.
pub const IDLE_MS: i64 = 30 * 60 * 1000;
/// Untouched for this long a record is pruned (and the mesh stops carrying it).
pub const PRUNE_MS: i64 = 24 * 60 * 60 * 1000;
/// At most this many recently touched files per record.
pub const FILES: usize = 3;
/// The prompt's first line, at most this many characters.
const PROMPT_CHARS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub session: String,
    pub harness: String,
    /// git user.email in the session's repo, empty when unknown.
    #[serde(default)]
    pub engineer: String,
    /// The repo's name (its key's last component).
    #[serde(default)]
    pub repo: String,
    pub collective: String,
    /// The first line of the engineer's latest prompt.
    #[serde(default)]
    pub prompt: String,
    /// The files the drone touched last, newest first, relative to the session's directory.
    #[serde(default)]
    pub files: Vec<String>,
    /// The canonical hook event last seen, and when (epoch ms).
    pub event: String,
    pub ms: i64,
    /// When this record was first written: the drone's start, for its designation.
    pub started: i64,
    /// The harness process, if the harness gave it (`pid` in the hook event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Working,
    Waiting,
    Idle,
    Ended,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Waiting => "waiting",
            State::Idle => "idle",
            State::Ended => "ended",
        }
    }
}

pub fn dir(home: &Path) -> PathBuf {
    home.join("presence")
}

/// Session ids become file names: only `[A-Za-z0-9-]` survives (as everywhere in the hook's state).
pub fn safe(session: &str) -> String {
    session.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect()
}

pub fn path(home: &Path, session: &str) -> PathBuf {
    dir(home).join(format!("{}.json", safe(session)))
}

/// Remote drones as the mesh daemon last heard of them (one signed record per line).
pub fn remote_path(home: &Path) -> PathBuf {
    home.join("mesh").join("presence.jsonl")
}

pub fn read(home: &Path, session: &str) -> Option<Presence> {
    serde_json::from_slice(&std::fs::read(path(home, session)).ok()?).ok()
}

/// What a tool call names as a file, relative to `cwd` when inside it.
fn file_of(input: &Value, cwd: &str) -> Option<String> {
    let ti = input.get("tool_input")?;
    let f = ["file_path", "notebook_path"].iter().find_map(|k| ti.get(*k).and_then(Value::as_str)).filter(|f| !f.trim().is_empty())?;
    let rel = Path::new(f).strip_prefix(cwd).ok().map(|p| p.display().to_string()).filter(|p| !p.is_empty());
    Some(rel.unwrap_or_else(|| f.to_string()))
}

/// The prompt's first non-empty line, cut.
pub fn first_line(prompt: &str) -> String {
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    let mut out: String = line.chars().take(PROMPT_CHARS).collect();
    if line.chars().count() > PROMPT_CHARS {
        out.push('…');
    }
    out
}

/// The record after `event` (pure: `prev` is the session's last record; `who` looks up the engineer
/// and repo, called only when there is no previous record).
pub fn update(
    prev: Option<Presence>,
    collective: &str,
    harness: &str,
    event: &str,
    input: &Value,
    now: i64,
    who: impl FnOnce(&str) -> (String, String),
) -> Presence {
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
    let session = input.get("session_id").and_then(Value::as_str).unwrap_or("unknown");
    let mut p = prev.unwrap_or_else(|| {
        let (engineer, repo) = who(cwd);
        Presence {
            session: session.into(),
            harness: harness.into(),
            engineer,
            repo,
            collective: collective.into(),
            prompt: String::new(),
            files: Vec::new(),
            event: String::new(),
            ms: now,
            started: now,
            pid: None,
        }
    });
    p.event = event.into();
    p.ms = now;
    if let Some(pid) = input.get("pid").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()) {
        p.pid = Some(pid);
    }
    match event {
        "prompt" => {
            let line = first_line(input.get("prompt").and_then(Value::as_str).unwrap_or_default());
            if !line.is_empty() {
                p.prompt = line;
            }
        }
        "post_tool" | "post_tool_failure" => {
            if let Some(f) = file_of(input, cwd) {
                p.files.retain(|x| *x != f);
                p.files.insert(0, f);
                p.files.truncate(FILES);
            }
        }
        _ => {}
    }
    p
}

/// The hook's one call: updates the session's record with one atomic write. Returns the record
/// (steering needs the session's start); `None` if it could not be written (fail open).
pub fn touch(home: &Path, collective: &str, harness: &str, event: &str, input: &Value, now: i64) -> Option<Presence> {
    let session = input.get("session_id").and_then(Value::as_str).unwrap_or("unknown");
    let p = update(read(home, session), collective, harness, event, input, now, |cwd| {
        (learn::engineer(cwd), borg_trace::repo_key(cwd).rsplit('/').next().unwrap_or_default().to_string())
    });
    write(home, &p).ok()?;
    if event == "prompt" {
        prune(home, now);
    }
    Some(p)
}

pub fn write(home: &Path, p: &Presence) -> std::io::Result<()> {
    let d = dir(home);
    std::fs::create_dir_all(&d)?;
    let target = path(home, &p.session);
    let tmp = d.join(format!(".{}.{}.tmp", safe(&p.session), std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(p).unwrap_or_default())?;
    std::fs::rename(&tmp, &target).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Removes records not touched for a day (by file time: no parsing in the hook path).
pub fn prune(home: &Path, now: i64) {
    let Ok(rd) = std::fs::read_dir(dir(home)) else { return };
    for e in rd.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .is_some_and(|d| now - d.as_millis() as i64 > PRUNE_MS);
        if old {
            let _ = std::fs::remove_file(e.path());
            // Its steering delivery claims go with it (the file stem is the safe session id).
            if let Some(session) = e.path().file_stem().and_then(|s| s.to_str()).filter(|s| !s.starts_with('.')) {
                crate::steer::forget_session(home, session);
            }
        }
    }
}

/// Every local record not yet due for pruning.
pub fn local(home: &Path, now: i64) -> Vec<Presence> {
    let Ok(rd) = std::fs::read_dir(dir(home)) else { return Vec::new() };
    let mut v: Vec<Presence> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice::<Presence>(&std::fs::read(e.path()).ok()?).ok())
        .filter(|p| now - p.ms <= PRUNE_MS)
        .collect();
    v.sort_by(|a, b| (a.started, &a.session).cmp(&(b.started, &b.session)));
    v
}

/// A remote record as the mesh daemon stored it: the drone's presence and the member key that
/// signed it (the daemon verified it on receipt).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remote {
    pub p: Presence,
    pub key: String,
    pub sig: String,
}

/// The remote records not yet due for pruning (latest per member and session).
pub fn remote(home: &Path, now: i64) -> Vec<Remote> {
    let text = std::fs::read_to_string(remote_path(home)).unwrap_or_default();
    let mut v: Vec<Remote> = text.lines().filter_map(|l| serde_json::from_str::<Remote>(l).ok()).filter(|r| now - r.p.ms <= PRUNE_MS).collect();
    v.sort_by(|a, b| (&a.key, &a.p.session, b.p.ms).cmp(&(&b.key, &b.p.session, a.p.ms)));
    v.dedup_by(|a, b| a.key == b.key && a.p.session == b.p.session);
    v
}

/// The session's state at `now`; `alive` says whether a pid the harness gave still runs.
pub fn state(p: &Presence, now: i64, alive: impl Fn(u32) -> bool) -> State {
    if p.event == "end" || p.pid.is_some_and(|pid| !alive(pid)) {
        State::Ended
    } else if now - p.ms > IDLE_MS {
        State::Idle
    } else if p.event == "stop" {
        State::Waiting
    } else {
        State::Working
    }
}

/// Whether a local process runs (Linux /proc; elsewhere every pid counts as alive).
pub fn alive(pid: u32) -> bool {
    !Path::new("/proc/self").exists() || Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-presence-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn who(_: &str) -> (String, String) {
        ("alice@x".into(), "app".into())
    }

    #[test]
    fn a_session_record_follows_its_hooks() {
        let ev = |e: &str, extra: Value| {
            let mut v = json!({"session_id": "s1", "cwd": "/w/app"});
            v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            (e.to_string(), v)
        };
        let mut p = None;
        let steps = [
            ev("prompt", json!({"prompt": "\n  fix the flaky sync test\nand more", "pid": 4242})),
            ev("post_tool", json!({"tool_name": "Edit", "tool_input": {"file_path": "/w/app/src/a.rs"}})),
            ev("post_tool", json!({"tool_name": "Read", "tool_input": {"file_path": "/w/app/src/b.rs"}})),
            ev("post_tool", json!({"tool_name": "Bash", "tool_input": {"command": "ls"}})),
            ev("post_tool", json!({"tool_name": "Edit", "tool_input": {"file_path": "/w/app/src/a.rs"}})),
            ev("post_tool", json!({"tool_name": "Write", "tool_input": {"file_path": "/elsewhere/c.rs"}})),
            ev("post_tool", json!({"tool_name": "Write", "tool_input": {"file_path": "/w/app/d.rs"}})),
        ];
        for (i, (e, input)) in steps.iter().enumerate() {
            p = Some(update(p, "corp", "claude", e, input, 1000 + i as i64, who));
        }
        let p = p.unwrap();
        assert_eq!(p.prompt, "fix the flaky sync test", "first non-empty line");
        assert_eq!(p.files, ["d.rs", "/elsewhere/c.rs", "src/a.rs"], "newest first, deduped, at most 3, relative inside cwd");
        assert_eq!((p.started, p.ms, p.pid, p.engineer.as_str(), p.repo.as_str()), (1000, 1006, Some(4242), "alice@x", "app"));
        let long = "x".repeat(300);
        assert_eq!(first_line(&long).chars().count(), PROMPT_CHARS + 1);
    }

    #[test]
    fn write_read_prune_and_state() {
        let home = tmp("wrp");
        let input = json!({"session_id": "s/../1", "cwd": "/w/app", "prompt": "hello"});
        let p = update(None, "corp", "omp", "prompt", &input, 5_000, who);
        write(&home, &p).unwrap();
        assert_eq!(read(&home, "s/../1"), Some(p.clone()));
        assert_eq!(path(&home, "s/../1"), dir(&home).join("s1.json"), "a session id never escapes the directory");
        assert_eq!(local(&home, 5_000), vec![p.clone()]);
        assert!(std::fs::read_dir(dir(&home)).unwrap().count() == 1, "no temp file left behind");

        assert_eq!(state(&p, 5_000, |_| true), State::Working);
        assert_eq!(state(&Presence { event: "stop".into(), ..p.clone() }, 5_000, |_| true), State::Waiting);
        assert_eq!(state(&p, 5_000 + IDLE_MS + 1, |_| true), State::Idle);
        assert_eq!(state(&Presence { event: "end".into(), ..p.clone() }, 5_000, |_| true), State::Ended);
        assert_eq!(state(&Presence { pid: Some(7), ..p.clone() }, 5_000, |_| false), State::Ended, "the harness process is gone");

        // A day later the record is no longer listed, and pruning (by file time) removes it.
        assert!(local(&home, 5_000 + PRUNE_MS + 1).is_empty());
        let far = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64 + PRUNE_MS + 1000;
        let claims = crate::steer::dir(&home).join("seen").join("s1");
        std::fs::create_dir_all(&claims).unwrap();
        prune(&home, far);
        assert!(read(&home, "s/../1").is_none());
        assert!(!claims.exists(), "its steering delivery claims go with it");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn remote_keeps_the_latest_per_member_and_session() {
        let home = tmp("remote");
        std::fs::create_dir_all(home.join("mesh")).unwrap();
        let p = |s: &str, ms: i64| Presence {
            session: s.into(),
            harness: "claude".into(),
            engineer: "bob@x".into(),
            repo: "r".into(),
            collective: "corp".into(),
            prompt: format!("at {ms}"),
            files: vec![],
            event: "prompt".into(),
            ms,
            started: 1,
            pid: None,
        };
        let line = |s: &str, ms: i64, key: &str| serde_json::to_string(&Remote { p: p(s, ms), key: key.into(), sig: "00".into() }).unwrap();
        let text = [line("a", 10, "k1"), line("a", 30, "k1"), line("a", 20, "k1"), line("a", 5, "k2"), "garbage".into()].join("\n");
        std::fs::write(remote_path(&home), text).unwrap();
        let r = remote(&home, 40);
        assert_eq!(r.iter().map(|r| (r.key.as_str(), r.p.ms)).collect::<Vec<_>>(), [("k1", 30), ("k2", 5)]);
        assert!(remote(&home, 40 + PRUNE_MS).is_empty(), "a day old: gone");
        let _ = std::fs::remove_dir_all(&home);
    }
}
