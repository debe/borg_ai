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

/// Without a hook for this long a session shows as idle. Display: it only picks the word `borg top`
/// and the web page show (`state`); no pacing, pruning or decision may derive from it.
pub const IDLE_MS: i64 = 30 * 60 * 1000;
/// Untouched for this long a record is pruned (and the mesh stops carrying it). Guard: bounds the
/// presence files to the sessions of the last day (`clock::DAY_MS`), one record per session.
pub const PRUNE_MS: i64 = crate::clock::DAY_MS;
/// At most this many recently touched files per record. Display: what a drone's row names.
pub const FILES: usize = 3;
/// The prompt's first line, at most this many characters. Display: one row of `borg top`.
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
            // Its message claims stay: a resumed session keeps its id, and they must outlive the messages
            // they cover (msg.rs drops them past the TTL).
            let _ = std::fs::remove_file(e.path());
            // Its opt-in to wakes (listen.rs) goes with it (the file stem is the safe session id).
            if let Some(session) = e.path().file_stem().and_then(|s| s.to_str()).filter(|s| !s.starts_with('.')) {
                crate::listen::forget(home, session);
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

/// `<store>/mesh/synced.json`, `{"<member key>": ms}`: when the last session with each member that
/// completed started (this node's clock), written by the mesh daemon (borg-mesh sync.rs), dialled or
/// served. Every line this node held before that ms was offered to the member in it, and the member's
/// lines up to its own reading of the logs came back. Local, never synced.
pub fn synced_path(home: &Path) -> PathBuf {
    home.join("mesh").join("synced.json")
}

/// The members' last completed sessions (`synced_path`); empty when none completed yet.
pub fn synced(home: &Path) -> std::collections::BTreeMap<String, i64> {
    std::fs::read_to_string(synced_path(home)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Records a completed session with `key` that started at `ms` (an older one never moves it back, but a
/// record later than the clock now goes: the clock stepped back since, and it would stay ahead of
/// every claim for as long). One small write to a temp file, then a rename: a reader never sees half
/// of it. `serve` and a one-shot `borg mesh sync` take no lock between them: one may drop the other's
/// newer entry, which only makes a claim wait for the next session.
pub fn note_synced(home: &Path, key: &str, ms: i64) -> std::io::Result<()> {
    let mut all = synced(home);
    if all.get(key).is_some_and(|&held| held >= ms && held <= crate::learn::now_ms()) {
        return Ok(());
    }
    all.insert(key.into(), ms);
    write_synced(home, &all)
}

/// Drops `key`'s record (its last dial failed): no claim waits on it until a session completes again.
pub fn forget_synced(home: &Path, key: &str) -> std::io::Result<()> {
    let mut all = synced(home);
    if all.remove(key).is_none() {
        return Ok(());
    }
    write_synced(home, &all)
}

fn write_synced(home: &Path, all: &std::collections::BTreeMap<String, i64>) -> std::io::Result<()> {
    write_mesh(home, &synced_path(home), all)
}

/// One small write to a temp file, then a rename: a reader never sees half of `p`.
fn write_mesh<T: Serialize>(home: &Path, p: &Path, all: &T) -> std::io::Result<()> {
    std::fs::create_dir_all(home.join("mesh"))?;
    let tmp = p.with_extension(format!("{}.tmp", std::process::id())); // a one-shot `borg mesh sync` beside `serve`
    std::fs::write(&tmp, serde_json::to_string(all).unwrap_or_default())?;
    std::fs::rename(&tmp, p)
}

/// `<store>/mesh/protocols.json`, `{"<member key>": Mismatch}`: the members whose last session ended at
/// the handshake because they speak another session protocol (borg-mesh sync.rs `ALPN`), so nothing
/// syncs with them. Written by the mesh daemon; `borg doctor` names each one. A session with the member
/// that completes drops its record, and so does a membership that no longer lists it. Local, never synced.
pub fn protocols_path(home: &Path) -> PathBuf {
    home.join("mesh").join("protocols.json")
}

/// A member's protocol beside this node's, both as their ALPN (`borg/mesh/<n>`), and when it was seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mismatch {
    pub theirs: String,
    pub ours: String,
    pub ms: i64,
}

impl Mismatch {
    /// What the mesh log and `borg doctor` say about member `key`: who has to upgrade.
    pub fn line(&self, key: &str) -> String {
        let n = |p: &str| p.rsplit('/').next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
        let who = if n(&self.theirs) < n(&self.ours) { "upgrade it" } else { "upgrade this node" };
        format!("peer {} speaks {}, this node {}: nothing syncs with it; {who}", crate::msg::short_node(key), self.theirs, self.ours)
    }

    /// The same, as the member on the other end reads it about this node (`me`): the close reason sent to it.
    pub fn told(&self, me: &str) -> String {
        Mismatch { theirs: self.ours.clone(), ours: self.theirs.clone(), ms: self.ms }.line(me)
    }
}

/// The members whose protocol differs (`protocols_path`); empty when none.
pub fn mismatches(home: &Path) -> std::collections::BTreeMap<String, Mismatch> {
    std::fs::read_to_string(protocols_path(home)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Records that `key` speaks another protocol (the newest sighting stays).
pub fn note_mismatch(home: &Path, key: &str, m: Mismatch) -> std::io::Result<()> {
    let mut all = mismatches(home);
    all.insert(key.into(), m);
    write_mesh(home, &protocols_path(home), &all)
}

/// Drops `key`'s record: a session with it completed, so it speaks this node's protocol now.
pub fn forget_mismatch(home: &Path, key: &str) -> std::io::Result<()> {
    let mut all = mismatches(home);
    if all.remove(key).is_none() {
        return Ok(());
    }
    write_mesh(home, &protocols_path(home), &all)
}

/// Keeps only the records of keys `keep` still allows (the mesh calls it when the membership it holds
/// changes: a removed or revoked member never completes another session, so nothing else drops it).
pub fn retain_mismatches(home: &Path, keep: impl Fn(&str) -> bool) -> std::io::Result<()> {
    let mut all = mismatches(home);
    let before = all.len();
    all.retain(|k, _| keep(k));
    if all.len() == before {
        return Ok(());
    }
    write_mesh(home, &protocols_path(home), &all)
}

/// `<store>/mesh/reads.json`, `{"<member key>": v}`: the newest message line format (msg.rs
/// `Message::v`) each member's node reads, as the protocol of its last completed session with this node
/// says (borg-mesh sync.rs `reads`): 1 for 0.4.0 and 0.5.x, which drop a v2 line, 0 for 0.3.0, which
/// syncs no messages. Written by the mesh daemon, dialled or served; `send` refuses what a session
/// there could never be shown (msg.rs `SendError::Reads`). Local, never synced.
pub fn reads_path(home: &Path) -> PathBuf {
    home.join("mesh").join("reads.json")
}

/// What each member's node reads (`reads_path`); empty before any session completed.
pub fn reads(home: &Path) -> std::collections::BTreeMap<String, u8> {
    std::fs::read_to_string(reads_path(home)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Records that `key`'s node reads message lines up to format `v`; no write when that is known (a
/// session completes every minute or so).
pub fn note_reads(home: &Path, key: &str, v: u8) -> std::io::Result<()> {
    let mut all = reads(home);
    if all.get(key) == Some(&v) {
        return Ok(());
    }
    all.insert(key.into(), v);
    write_mesh(home, &reads_path(home), &all)
}

/// Keeps only the reads records of keys `keep` still allows, as `retain_mismatches` keeps theirs: a
/// removed member's record misleads nothing (msg.rs `unread_on` counts the held members alone), but no
/// session with it would ever replace it.
pub fn retain_reads(home: &Path, keep: impl Fn(&str) -> bool) -> std::io::Result<()> {
    let mut all = reads(home);
    let before = all.len();
    all.retain(|k, _| keep(k));
    if all.len() == before {
        return Ok(());
    }
    write_mesh(home, &reads_path(home), &all)
}

/// `borg doctor`'s line for each member the mesh cannot sync with because it speaks another session
/// protocol (`mismatches`, recorded by `borg mesh serve` or `sync`), in the stores of `routes`
/// (install.rs `collectives`): a handshake that fails otherwise says nothing.
pub fn mesh_checks(routes: &[crate::collective::Route]) -> Vec<(crate::harness::Level, String)> {
    use crate::collective::Route;
    routes
        .iter()
        .filter_map(|r| match r {
            Route::Collective { name, home, .. } => Some((name, home)),
            Route::Off => None,
        })
        .flat_map(|(name, home)| mismatches(home).into_iter().map(move |(k, m)| (crate::harness::Level::Warn, format!("mesh {name}: {}", m.line(&k)))))
        .collect()
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

/// Whether a local process runs (signal 0, so Linux and macOS alike).
pub fn alive(pid: u32) -> bool {
    i32::try_from(pid).is_ok_and(crate::live::alive)
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

    /// Two sessions with one member can complete out of order (a served one beside a dialled one): the
    /// later start stays.
    #[test]
    fn a_members_last_session_never_moves_back() {
        let h = tmp("synced");
        assert!(synced(&h).is_empty());
        note_synced(&h, "k1", 200).unwrap();
        note_synced(&h, "k1", 100).unwrap();
        note_synced(&h, "k2", 50).unwrap();
        assert_eq!(synced(&h).into_iter().collect::<Vec<_>>(), [("k1".to_string(), 200), ("k2".to_string(), 50)]);
        note_synced(&h, "k1", 300).unwrap();
        assert_eq!(synced(&h)["k1"], 300);
        forget_synced(&h, "k1").unwrap();
        assert_eq!(synced(&h).into_keys().collect::<Vec<_>>(), ["k2"], "a failed dial drops the member");
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn doctor_names_a_member_on_another_mesh_protocol() {
        use crate::collective::Route;
        use crate::harness::Level;
        let home = tmp("protocols-doctor");
        let route = Route::Collective { name: "team".into(), home: home.clone(), share: crate::collective::Share::Team };
        assert!(mesh_checks(std::slice::from_ref(&route)).is_empty());
        let m = Mismatch { theirs: "borg/mesh/5".into(), ours: "borg/mesh/10".into(), ms: 1 };
        note_mismatch(&home, "abcdef0123456789", m).unwrap();
        let checks = mesh_checks(&[route, Route::Off]);
        assert_eq!(checks.len(), 1, "{checks:?}");
        assert_eq!(checks[0].0, Level::Warn);
        assert!(checks[0].1.starts_with("mesh team: peer abcdef012345 speaks borg/mesh/5") && checks[0].1.ends_with("upgrade it"), "{checks:?}");
    }

    /// A member on another protocol is recorded with who has to upgrade, until a session completes.
    #[test]
    fn a_protocol_mismatch_is_kept_until_a_session_completes() {
        let h = tmp("protocols");
        assert!(mismatches(&h).is_empty());
        let old = Mismatch { theirs: "borg/mesh/5".into(), ours: "borg/mesh/10".into(), ms: 1 };
        note_mismatch(&h, "abcdef0123456789", old.clone()).unwrap();
        note_mismatch(&h, "k2", Mismatch { theirs: "borg/mesh/11".into(), ..old.clone() }).unwrap();
        let all = mismatches(&h);
        assert_eq!(
            all["abcdef0123456789"].line("abcdef0123456789"),
            "peer abcdef012345 speaks borg/mesh/5, this node borg/mesh/10: nothing syncs with it; upgrade it"
        );
        assert!(all["k2"].line("k2").ends_with("upgrade this node"), "a newer peer: this node is the old one");
        forget_mismatch(&h, "abcdef0123456789").unwrap();
        assert_eq!(mismatches(&h).into_keys().collect::<Vec<_>>(), ["k2"]);
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn a_record_ahead_of_the_clock_gives_way() {
        // The clock stepped back an hour after a session: the next session's start replaces it.
        let h = tmp("synced-skew");
        let now = crate::learn::now_ms();
        note_synced(&h, "k1", now + 3_600_000).unwrap();
        note_synced(&h, "k1", now).unwrap();
        assert_eq!(synced(&h)["k1"], now);
        let _ = std::fs::remove_dir_all(&h);
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
        prune(&home, far);
        assert!(read(&home, "s/../1").is_none());
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

    /// A session resumed a day later keeps its id: the prune of its presence does not show its direct
    /// messages again.
    #[test]
    fn a_resumed_session_is_not_shown_its_messages_again() {
        let home = tmp("resume");
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
        let input = json!({"session_id": "s2"});
        write(&home, &update(None, "corp", "claude", "prompt", &input, now, who)).unwrap();
        let from = crate::msg::Sender { by: "s1".into(), name: "Two of Five".into(), harness: "claude".into(), node: "n1".into() };
        crate::msg::send(&home, crate::msg::Message::new(crate::msg::TELL, &from, "s2", None, now, "the API is stable"), now).unwrap();
        let caps = crate::harness::by_name("claude").expect("a harness").caps();
        assert!(crate::msg::deliver(&home, caps, "prompt", &input, Some(now), now, None).is_some(), "shown");

        let later = now + PRUNE_MS + 60_000;
        prune(&home, later);
        assert!(read(&home, "s2").is_none(), "the presence is pruned");
        write(&home, &update(None, "corp", "claude", "prompt", &input, later, who)).unwrap();
        assert_eq!(crate::msg::deliver(&home, caps, "prompt", &input, Some(later), later, None), None, "not shown again");
        let _ = std::fs::remove_dir_all(&home);
    }
}
