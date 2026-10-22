//! The board: `<store>/hive/<goal>/tasks.jsonl`, append-only events (`open`, `claim`, `heartbeat`,
//! `progress`, `done`, `verified`, `rejected`, `released`, `blocked`), and `fold`, the one reading of
//! them every node shares.
//!
//! The fold sorts the events by (ms, rank, line hash), opens first within one ms, before applying them, so it gives the same board
//! whatever order the lines arrived in. A claim is a lease: valid on an open task, or on one whose
//! holder sent nothing for `LEASE_MS`. Of two claims on one open task the one with the lowest
//! (ms, blake3(node, by, task)) wins on every node: the design's winner rule. The loser's claim never
//! took effect (`Board::lost`), so it needs no `released`.
//!
//! Lines are plain JSON with no `id` + `version` pair, so the mesh's `settle` (borg-mesh lines.rs) never
//! folds them, and they can sync as they are once the hive goes on the mesh.

use std::collections::BTreeMap;
use std::io::{Read, Seek, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::goal::Check;
use super::{LEASE_MS, TaskId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Plan,
    Work,
    Verify,
    Integrate,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Plan => "plan",
            Role::Work => "work",
            Role::Verify => "verify",
            Role::Integrate => "integrate",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        [Role::Plan, Role::Work, Role::Verify, Role::Integrate].into_iter().find(|r| r.name() == s.trim().to_lowercase())
    }

    /// Which open task a drone takes first: what unblocks the flow (a verify frees its work, an
    /// integrate the goal), then work, then planning.
    fn urgency(self) -> u8 {
        match self {
            Role::Verify => 0,
            Role::Integrate => 1,
            Role::Work => 2,
            Role::Plan => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub role: Role,
    pub title: String,
    /// `queen::repo_name` form; empty: any.
    #[serde(default)]
    pub repo: String,
    /// Globs, repo-relative: the task's scope.
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub deps: Vec<TaskId>,
    /// Empty: the goal's oracle.
    #[serde(default)]
    pub accept: Vec<Check>,
    /// A verify task's work task.
    #[serde(default)]
    pub of: Option<TaskId>,
    /// Sessions that may not claim it: the work's author, for a verify task.
    #[serde(default)]
    pub not_by: Vec<String>,
    /// A verify task's guard flags: what the work's author did to tests, CI or oracle files while it
    /// held the claim (hive/guard.rs `render`), shown to the verifier.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ev {
    Open,
    Claim,
    Heartbeat,
    Progress,
    Done,
    Verified,
    Rejected,
    Released,
    Blocked,
}

impl Ev {
    pub fn name(self) -> &'static str {
        match self {
            Ev::Open => "open",
            Ev::Claim => "claim",
            Ev::Heartbeat => "heartbeat",
            Ev::Progress => "progress",
            Ev::Done => "done",
            Ev::Verified => "verified",
            Ev::Rejected => "rejected",
            Ev::Released => "released",
            Ev::Blocked => "blocked",
        }
    }
}

/// One line of tasks.jsonl. `by` is the session (or `engineer:<email>` for an engineer's own event),
/// `node` the machine (empty: this one), flat so the line syncs as it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub v: u8,
    pub ev: Ev,
    pub task: TaskId,
    pub ms: i64,
    pub by: String,
    #[serde(default)]
    pub node: String,
    /// The task, on `open` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub def: Option<Task>,
    /// A progress, rejection, block or done summary.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// On `rejected`: the work changed protected files (tests or CI it was not scoped to) or touched
    /// the oracle's (hive/verdict.rs `decide`), whatever the verifier said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hacked: Option<bool>,
    /// On a claim of a work task: a commit of the claimant's working tree then (hive/verdict.rs
    /// `snapshot`); on its `done`, `head`, the same at the done. The verdict diffs the two.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub head: String,
    /// On a claim: the drone the session is one iteration of (`borg hive run` names it, hive/run.rs);
    /// empty for a session of its own. A verify task barred to an author drone is barred to all its
    /// sessions.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub drone: String,
}

impl Event {
    pub fn new(ev: Ev, task: &str, by: &str, ms: i64) -> Event {
        Event {
            v: 1,
            ev,
            task: task.into(),
            ms,
            by: by.into(),
            node: String::new(),
            def: None,
            note: String::new(),
            hacked: None,
            drone: String::new(),
            base: String::new(),
            head: String::new(),
        }
    }

    pub fn open(t: Task, by: &str, ms: i64) -> Event {
        Event { def: Some(t.clone()), ..Event::new(Ev::Open, &t.id, by, ms) }
    }

    pub fn with_note(mut self, note: &str) -> Event {
        self.note = note.into();
        self
    }

    pub fn with_drone(mut self, drone: &str) -> Event {
        self.drone = drone.into();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Open,
    Claimed { by: String, node: String, since: i64, renewed: i64 },
    Done { by: String, ms: i64 },
    Verified { by: String, ms: i64 },
    Blocked { by: String, note: String },
}

impl Status {
    pub fn name(&self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Claimed { .. } => "claimed",
            Status::Done { .. } => "done",
            Status::Verified { .. } => "verified",
            Status::Blocked { .. } => "blocked",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskState {
    pub task: Task,
    pub status: Status,
    pub opened_ms: i64,
    /// How often its work was rejected.
    pub rework: u32,
    /// Verifiers' notes on rejection, newest last, at most `MAX_NOTES`.
    pub notes: Vec<String>,
    pub last_progress: String,
    /// The session whose `done` was accepted last.
    pub author: Option<String>,
}

/// Verifier notes kept per task.
pub const MAX_NOTES: usize = 5;

impl TaskState {
    /// The live holder at `now`: (session, node, since), None when the task is not claimed or its
    /// lease expired (then it is open to claim; the log is not rewritten).
    pub fn live(&self, now: i64) -> Option<(&str, &str, i64)> {
        match &self.status {
            Status::Claimed { by, node, since, renewed } if now - renewed <= LEASE_MS => Some((by, node, *since)),
            _ => None,
        }
    }

    /// Open to a claim at `now`: open, or claimed with an expired lease.
    pub fn claimable(&self, now: i64) -> bool {
        match &self.status {
            Status::Open => true,
            Status::Claimed { renewed, .. } => now - renewed > LEASE_MS,
            _ => false,
        }
    }

    /// What the status shows as at `now`: an expired claim reads as open.
    pub fn shown_status(&self, now: i64) -> &'static str {
        match &self.status {
            Status::Claimed { .. } if self.live(now).is_none() => "expired",
            s => s.name(),
        }
    }
}

/// One claim tenure: from the claim to its done, release, block or expiry (`to_ms` None while it is
/// held). The reward step's credit window: lessons asked in session `by` within it share the task's
/// outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub task: TaskId,
    pub by: String,
    pub node: String,
    pub from_ms: i64,
    pub to_ms: Option<i64>,
    /// How it ended: done, verified, rejected, released or blocked; None while held or when it expired.
    pub end: Option<Ev>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Board {
    pub tasks: BTreeMap<TaskId, TaskState>,
    /// Claims that lost a race or were invalid: (by, task, ms).
    pub lost: Vec<(String, TaskId, i64)>,
    pub spans: Vec<Span>,
    /// The drone each session claimed as (`Event::drone`): session -> drone.
    pub drones: BTreeMap<String, String>,
}

impl Board {
    /// Whether `t` is barred to `session`: by its id, or by the drone it claims as.
    pub fn barred(&self, t: &Task, session: &str) -> bool {
        t.not_by.iter().any(|n| n == session || self.drones.get(session).is_some_and(|d| d == n))
    }

    /// The goal is reached: there are tasks, and every one is verified (a work task by its verify task,
    /// a verify task by its verdict).
    pub fn finished(&self) -> bool {
        !self.tasks.is_empty() && self.tasks.values().all(|s| matches!(s.status, Status::Verified { .. }))
    }
}

/// Appends `events` in one write(2) with O_APPEND: lines written together (a done and its verify
/// task's open, a claim and its heartbeat) never interleave with another writer's.
///
/// # Errors
/// When the directory or the file cannot be written.
pub fn append(path: &Path, events: &[Event]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut body = String::new();
    for e in events {
        body.push_str(&serde_json::to_string(e).map_err(std::io::Error::other)?);
        body.push('\n');
    }
    std::fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(body.as_bytes())
}

/// An event of a line, None for anything unparsable, incomplete (no newline yet) or of a newer
/// format: the mesh rule, an unknown line is skipped, never an error.
fn parse(line: &str) -> Option<Event> {
    serde_json::from_str::<Event>(line).ok().filter(|e| e.v <= 1)
}

/// Every event of the board at `path`, in file order.
pub fn read(path: &Path) -> Vec<Event> {
    read_from(path, 0).0.into_iter().map(|(_, e)| e).collect()
}

/// The events after byte `offset` (each with the offset its line ends at) and the offset to read from
/// next: only complete lines count. An offset past the end (the file was replaced) reads from 0.
pub fn read_from(path: &Path, offset: u64) -> (Vec<(u64, Event)>, u64) {
    let Ok(mut f) = std::fs::File::open(path) else { return (Vec::new(), 0) };
    let len = f.metadata().map_or(0, |m| m.len());
    let start = if offset > len { 0 } else { offset };
    if f.seek(std::io::SeekFrom::Start(start)).is_err() {
        return (Vec::new(), offset);
    }
    let mut raw = Vec::new();
    let _ = f.read_to_end(&mut raw);
    let mut out = Vec::new();
    let mut pos = start;
    let mut rest = raw.as_slice();
    while let Some(i) = rest.iter().position(|b| *b == b'\n') {
        let (line, tail) = rest.split_at(i + 1);
        pos += line.len() as u64;
        if let Some(e) = std::str::from_utf8(&line[..i]).ok().and_then(parse) {
            out.push((pos, e));
        }
        rest = tail;
    }
    (out, pos)
}

/// The rank of an event in the winner rule: blake3 of its claimant's key and task, domain-tagged.
fn rank(e: &Event) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"borg-hive-v1\0");
    for part in [e.node.as_str(), e.by.as_str(), e.task.as_str()] {
        h.update(part.as_bytes());
        h.update(b"\0");
    }
    *h.finalize().as_bytes()
}

fn line_hash(e: &Event) -> [u8; 32] {
    *blake3::hash(serde_json::to_string(e).unwrap_or_default().as_bytes()).as_bytes()
}

/// An engineer's own event (`borg hive open`, a release from the terminal): may release any claim.
pub fn by_engineer(by: &str) -> bool {
    by.starts_with("engineer:")
}

/// The board the events say, the same in any arrival order (see the module doc).
pub fn fold(events: &[Event]) -> Board {
    // Within one ms an open goes first (a task opened and claimed at once is claimed open), except a
    // verify task's open, which goes after the `done` written with it: it takes effect only on a done
    // the fold took.
    type Key = (i64, u8, [u8; 32], [u8; 32]);
    let order = |e: &Event| match (e.ev, e.def.as_ref().and_then(|d| d.of.as_ref())) {
        (Ev::Open, None) => 0,
        (Ev::Open, Some(_)) => 2,
        _ => 1,
    };
    let mut keyed: Vec<(Key, &Event)> = events.iter().map(|e| ((e.ms, order(e), rank(e), line_hash(e)), e)).collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    let mut b = Board { drones: events.iter().filter(|e| !e.drone.is_empty()).map(|e| (e.by.clone(), e.drone.clone())).collect(), ..Board::default() };
    // The open span of each task's holder, by task: an index into `b.spans`.
    let mut open_span: BTreeMap<TaskId, usize> = BTreeMap::new();
    for (_, e) in keyed {
        apply(&mut b, &mut open_span, e);
    }
    b
}

fn close(b: &mut Board, open_span: &mut BTreeMap<TaskId, usize>, task: &str, to: i64, end: Option<Ev>) {
    if let Some(i) = open_span.remove(task) {
        b.spans[i].to_ms = Some(to);
        b.spans[i].end = end;
    }
}

/// Whether a claim of `t` is allowed by its dependencies: each verified (a verify task: its work done).
fn deps_met(tasks: &BTreeMap<TaskId, TaskState>, t: &Task) -> bool {
    let of_done = t.of.as_ref().is_none_or(|w| matches!(tasks.get(w).map(|s| &s.status), Some(Status::Done { .. })));
    of_done && t.deps.iter().all(|d| matches!(tasks.get(d).map(|s| &s.status), Some(Status::Verified { .. })))
}

/// The holder of a claimed task, whatever its lease.
fn holder(s: &TaskState) -> Option<&str> {
    match &s.status {
        Status::Claimed { by, .. } => Some(by),
        _ => None,
    }
}

fn apply(b: &mut Board, open_span: &mut BTreeMap<TaskId, usize>, e: &Event) {
    match e.ev {
        Ev::Open => {
            let Some(def) = e.def.as_ref().filter(|d| d.id == e.task) else { return };
            // A verify task's open is part of its work's `done` (same session, same ms): a done the
            // fold ignored (the lease had passed to another drone) opens nothing, so the verify task
            // and its bar are those of the done that took effect.
            if let Some(w) = def.of.as_ref()
                && !matches!(b.tasks.get(w).map(|s| &s.status), Some(Status::Done { by, ms }) if *by == e.by && *ms == e.ms)
            {
                return;
            }
            match b.tasks.get_mut(&e.task) {
                None => {
                    let s = TaskState {
                        task: def.clone(),
                        status: Status::Open,
                        opened_ms: e.ms,
                        rework: 0,
                        notes: Vec::new(),
                        last_progress: String::new(),
                        author: None,
                    };
                    b.tasks.insert(e.task.clone(), s);
                }
                // `borg hive open --again`: an engineer reopens a blocked task.
                Some(s) if matches!(s.status, Status::Blocked { .. }) => s.status = Status::Open,
                Some(_) => {}
            }
        }
        Ev::Claim => {
            let Some(s) = b.tasks.get(&e.task) else {
                b.lost.push((e.by.clone(), e.task.clone(), e.ms));
                return;
            };
            if holder(s) == Some(e.by.as_str()) && s.live(e.ms).is_some() {
                // Claimed by itself already: a heartbeat.
                if let Some(s) = b.tasks.get_mut(&e.task)
                    && let Status::Claimed { renewed, .. } = &mut s.status
                {
                    *renewed = e.ms;
                }
                return;
            }
            if !s.claimable(e.ms) || !deps_met(&b.tasks, &s.task) || b.barred(&s.task, &e.by) {
                b.lost.push((e.by.clone(), e.task.clone(), e.ms));
                return;
            }
            // The previous holder's lease expired: its tenure ends at its last sign of life.
            if let Status::Claimed { renewed, .. } = s.status {
                close(b, open_span, &e.task, renewed, None);
            }
            if let Some(s) = b.tasks.get_mut(&e.task) {
                s.status = Status::Claimed { by: e.by.clone(), node: e.node.clone(), since: e.ms, renewed: e.ms };
            }
            open_span.insert(e.task.clone(), b.spans.len());
            b.spans.push(Span { task: e.task.clone(), by: e.by.clone(), node: e.node.clone(), from_ms: e.ms, to_ms: None, end: None });
        }
        Ev::Heartbeat | Ev::Progress => {
            let Some(s) = b.tasks.get_mut(&e.task) else { return };
            let live = s.live(e.ms).is_some_and(|(by, _, _)| by == e.by);
            if let (true, Status::Claimed { renewed, .. }) = (live, &mut s.status) {
                *renewed = e.ms;
                if e.ev == Ev::Progress {
                    s.last_progress = e.note.clone();
                }
            }
        }
        Ev::Done => {
            let Some(s) = b.tasks.get_mut(&e.task) else { return };
            if holder(s) != Some(e.by.as_str()) {
                return;
            }
            s.status = Status::Done { by: e.by.clone(), ms: e.ms };
            s.author = Some(e.by.clone());
            if !e.note.is_empty() {
                s.last_progress = e.note.clone();
            }
            close(b, open_span, &e.task, e.ms, Some(Ev::Done));
        }
        Ev::Verified | Ev::Rejected => {
            let Some(s) = b.tasks.get_mut(&e.task) else { return };
            if s.task.role != Role::Verify || holder(s) != Some(e.by.as_str()) {
                return;
            }
            s.status = Status::Verified { by: e.by.clone(), ms: e.ms };
            if e.ev == Ev::Rejected {
                s.last_progress = format!("rejected: {}", e.note);
            }
            let of = s.task.of.clone();
            close(b, open_span, &e.task, e.ms, Some(e.ev));
            let Some(w) = of.and_then(|w| b.tasks.get_mut(&w)) else { return };
            if !matches!(w.status, Status::Done { .. }) {
                return;
            }
            if e.ev == Ev::Verified {
                w.status = Status::Verified { by: e.by.clone(), ms: e.ms };
            } else {
                w.status = Status::Open;
                w.rework += 1;
                w.notes.push(e.note.clone());
                if w.notes.len() > MAX_NOTES {
                    w.notes.remove(0);
                }
            }
        }
        Ev::Released => {
            let Some(s) = b.tasks.get_mut(&e.task) else { return };
            let mine = holder(s) == Some(e.by.as_str());
            let engineer = by_engineer(&e.by) && matches!(s.status, Status::Claimed { .. } | Status::Blocked { .. });
            if mine || engineer {
                s.status = Status::Open;
                close(b, open_span, &e.task, e.ms, Some(Ev::Released));
            }
        }
        Ev::Blocked => {
            let Some(s) = b.tasks.get_mut(&e.task) else { return };
            if holder(s) == Some(e.by.as_str()) {
                s.status = Status::Blocked { by: e.by.clone(), note: e.note.clone() };
                close(b, open_span, &e.task, e.ms, Some(Ev::Blocked));
            }
        }
    }
}

/// The live claim of `session` at `now`, if any (one per drone: the CLI and the handover refuse a second).
pub fn holding<'a>(b: &'a Board, session: &str, now: i64) -> Option<&'a TaskState> {
    b.tasks.values().filter(|s| s.live(now).is_some_and(|(by, _, _)| by == session)).max_by_key(|s| s.live(now).map(|(_, _, since)| since))
}

/// The tasks `session` lost a claim on after `ms` (a race, or a claim the board did not allow).
pub fn lost_since(b: &Board, session: &str, ms: i64) -> Vec<TaskId> {
    let mut out: Vec<TaskId> = b.lost.iter().filter(|(by, _, at)| by == session && *at > ms).map(|(_, t, _)| t.clone()).collect();
    out.dedup();
    out
}

/// Whether `session` may claim `s` at `now` (the fold's rules a-c: open or expired, dependencies
/// verified, not barred).
pub fn claimable_by(b: &Board, s: &TaskState, session: &str, now: i64) -> bool {
    s.claimable(now) && deps_met(&b.tasks, &s.task) && !b.barred(&s.task, session)
}

/// The task `session` in `repo` should take next at `now`, among those `allowed` lets it claim: its
/// own repo's first, then by role (verify, integrate, work, plan), then the oldest, then by id.
pub fn next_for<'a>(b: &'a Board, session: &str, repo: &str, now: i64, allowed: impl Fn(&Task) -> bool) -> Option<&'a TaskState> {
    b.tasks
        .values()
        .filter(|s| claimable_by(b, s, session, now) && allowed(&s.task))
        .min_by_key(|s| (repo.is_empty() || !s.task.repo.eq_ignore_ascii_case(repo), s.task.role.urgency(), s.opened_ms, s.task.id.clone()))
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>()) }
}

fn ago(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

fn short(session: &str) -> String {
    session.chars().take(8).collect()
}

/// Who holds or held a task, as a board line names it.
fn who(s: &TaskState, now: i64) -> String {
    match &s.status {
        Status::Claimed { by, renewed, .. } => {
            format!("{} {}", short(by), if s.live(now).is_some() { format!("renewed {} ago", ago(now - renewed)) } else { "lease expired".into() })
        }
        Status::Done { by, .. } | Status::Verified { by, .. } | Status::Blocked { by, .. } => short(by),
        Status::Open => String::new(),
    }
}

/// `borg hive board`: the tasks grouped by status, one line each (id, role, title, holder, age,
/// rework, deps), each line at most `width` characters.
pub fn render(b: &Board, now: i64, width: usize) -> String {
    const GROUPS: [&str; 6] = ["claimed", "open", "expired", "blocked", "done", "verified"];
    let mut out = String::new();
    for g in GROUPS {
        let rows: Vec<&TaskState> = b.tasks.values().filter(|s| s.shown_status(now) == g).collect();
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!("{g} ({})\n", rows.len()));
        for s in rows {
            let mut line = format!("  {:<8} {:<9} {}", s.task.id, s.task.role.name(), s.task.title);
            let who = who(s, now);
            if !who.is_empty() {
                line.push_str(&format!("  [{who}]"));
            }
            line.push_str(&format!("  opened {} ago", ago(now - s.opened_ms)));
            if s.rework > 0 {
                line.push_str(&format!("  rework {}", s.rework));
            }
            if !s.task.deps.is_empty() {
                line.push_str(&format!("  after {}", s.task.deps.join(",")));
            }
            if let Status::Blocked { note, .. } = &s.status
                && !note.is_empty()
            {
                line.push_str(&format!("  ({note})"));
            }
            out.push_str(&cut(&line, width));
            out.push('\n');
        }
    }
    if out.is_empty() {
        out.push_str("no tasks yet (`borg hive open`)\n");
    }
    out
}

/// One board change as a drone's prompt context shows it.
pub fn render_change(e: &Event) -> String {
    let who = if by_engineer(&e.by) { e.by.clone() } else { format!("drone {}", short(&e.by)) };
    let mut s = match (&e.ev, &e.def) {
        (Ev::Open, Some(t)) => format!("{} opened {} ({}) {}", who, e.task, t.role.name(), cut(&t.title, 80)),
        _ => format!("{who} {} {}", e.ev.name(), e.task),
    };
    if !e.note.is_empty() {
        s.push_str(&format!(": {}", cut(&e.note, 160)));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str) -> Task {
        Task {
            id: id.into(),
            role: Role::Work,
            title: format!("do {id}"),
            repo: String::new(),
            files: vec![],
            deps: vec![],
            accept: vec![],
            of: None,
            not_by: vec![],
            flags: vec![],
        }
    }

    fn ev(kind: Ev, t: &str, by: &str, ms: i64) -> Event {
        Event::new(kind, t, by, ms)
    }

    fn holder_of(b: &Board, t: &str) -> Option<String> {
        match &b.tasks[t].status {
            Status::Claimed { by, .. } => Some(by.clone()),
            _ => None,
        }
    }

    fn permutations(v: &[Event]) -> Vec<Vec<Event>> {
        if v.len() <= 1 {
            return vec![v.to_vec()];
        }
        let mut out = Vec::new();
        for i in 0..v.len() {
            let mut rest = v.to_vec();
            let x = rest.remove(i);
            for mut p in permutations(&rest) {
                p.insert(0, x.clone());
                out.push(p);
            }
        }
        out
    }

    #[test]
    fn two_claims_in_any_arrival_order_pick_one_winner() {
        for (ma, mb) in [(5, 5), (5, 6), (7, 6)] {
            let events = vec![Event::open(task("t1"), "engineer:e", 1), ev(Ev::Claim, "t1", "sa", ma), ev(Ev::Claim, "t1", "sb", mb)];
            let all = permutations(&events);
            assert_eq!(all.len(), 6);
            let boards: Vec<Board> = all.iter().map(|p| fold(p)).collect();
            let winner = holder_of(&boards[0], "t1").expect("one claim wins");
            for b in &boards {
                assert_eq!(holder_of(b, "t1").as_deref(), Some(winner.as_str()), "same winner in every order ({ma},{mb})");
                let loser = if winner == "sa" { "sb" } else { "sa" };
                assert_eq!(b.lost.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), [loser]);
            }
            if ma != mb {
                assert_eq!(winner, if ma < mb { "sa" } else { "sb" }, "the earlier claim wins");
            }
        }
    }

    #[test]
    fn the_lowest_ms_wins_then_lowest_hash() {
        let a = ev(Ev::Claim, "t1", "sa", 10);
        let b = ev(Ev::Claim, "t1", "sb", 10);
        let lower = if rank(&a) < rank(&b) { "sa" } else { "sb" };
        let board = fold(&[Event::open(task("t1"), "engineer:e", 1), b.clone(), a.clone()]);
        assert_eq!(holder_of(&board, "t1").as_deref(), Some(lower), "same ms: the lower blake3 rank wins");
        let earlier = ev(Ev::Claim, "t1", if lower == "sa" { "sb" } else { "sa" }, 9);
        let board = fold(&[Event::open(task("t1"), "engineer:e", 1), a, b, earlier.clone()]);
        assert_eq!(holder_of(&board, "t1"), Some(earlier.by), "a lower ms beats any rank");
    }

    #[test]
    fn a_lease_expires_after_idle_and_another_drone_may_claim() {
        let open = Event::open(task("t1"), "engineer:e", 0);
        let b = fold(&[open.clone(), ev(Ev::Claim, "t1", "sa", 0), ev(Ev::Claim, "t1", "sb", LEASE_MS + 1)]);
        assert_eq!(holder_of(&b, "t1").as_deref(), Some("sb"), "expired: the rival wins");
        assert_eq!((b.spans[0].to_ms, b.spans[0].end), (Some(0), None), "the old tenure ends expired");
        let b = fold(&[open.clone(), ev(Ev::Claim, "t1", "sa", 0), ev(Ev::Claim, "t1", "sb", LEASE_MS - 1)]);
        assert_eq!(holder_of(&b, "t1").as_deref(), Some("sa"), "still leased");
        let b = fold(&[open, ev(Ev::Claim, "t1", "sa", 0), ev(Ev::Heartbeat, "t1", "sa", LEASE_MS - 1), ev(Ev::Claim, "t1", "sb", LEASE_MS + 1)]);
        assert_eq!(holder_of(&b, "t1").as_deref(), Some("sa"), "a heartbeat renews the lease");
        assert!(b.tasks["t1"].live(LEASE_MS - 1 + LEASE_MS).is_some() && b.tasks["t1"].live(2 * LEASE_MS).is_none());
    }

    #[test]
    fn deps_gate_claims() {
        let mut t2 = task("t2");
        t2.deps = vec!["t1".into()];
        let base = vec![Event::open(task("t1"), "engineer:e", 0), Event::open(t2, "engineer:e", 0)];
        let b = fold(&[base.clone(), vec![ev(Ev::Claim, "t2", "sa", 1)]].concat());
        assert_eq!(holder_of(&b, "t2"), None);
        assert_eq!(lost_since(&b, "sa", 0), ["t2"]);
        let mut v = verify("t1", "sa", 3);
        v.id = "t1.v1".into();
        let done = vec![ev(Ev::Claim, "t1", "sa", 1), ev(Ev::Done, "t1", "sa", 2), Event::open(v, "sa", 2)];
        let b = fold(&[base.clone(), done.clone(), vec![ev(Ev::Claim, "t2", "sb", 3)]].concat());
        assert_eq!(holder_of(&b, "t2"), None, "done is not verified");
        let verified = vec![ev(Ev::Claim, "t1.v1", "sb", 3), ev(Ev::Verified, "t1.v1", "sb", 4), ev(Ev::Claim, "t2", "sc", 5)];
        let b = fold(&[base, done, verified].concat());
        assert_eq!(holder_of(&b, "t2").as_deref(), Some("sc"));
    }

    fn verify(of: &str, author: &str, _ms: i64) -> Task {
        Task { id: format!("{of}.v1"), role: Role::Verify, of: Some(of.into()), not_by: vec![author.into()], ..task("x") }
    }

    fn done_t1() -> Vec<Event> {
        vec![
            Event::open(task("t1"), "engineer:e", 0),
            ev(Ev::Claim, "t1", "sa", 1),
            ev(Ev::Done, "t1", "sa", 2).with_note("did it"),
            Event::open(verify("t1", "sa", 2), "sa", 2),
        ]
    }

    #[test]
    fn the_author_cannot_claim_its_verify_task() {
        let b = fold(&[done_t1(), vec![ev(Ev::Claim, "t1.v1", "sa", 3)]].concat());
        assert_eq!(holder_of(&b, "t1.v1"), None);
        assert!(!claimable_by(&b, &b.tasks["t1.v1"], "sa", 3) && claimable_by(&b, &b.tasks["t1.v1"], "sb", 3));
        assert_eq!(b.tasks["t1"].author.as_deref(), Some("sa"));
    }

    #[test]
    fn a_verify_task_barred_to_a_drone_is_barred_to_every_session_of_it() {
        let mut v = verify("t1", "sa", 2);
        v.not_by.push("d1".into());
        let base =
            vec![Event::open(task("t1"), "engineer:e", 0), ev(Ev::Claim, "t1", "sa", 1).with_drone("d1"), ev(Ev::Done, "t1", "sa", 2), Event::open(v, "sa", 2)];
        // A later iteration of d1 (a new session) claims the verify task: it does not take effect.
        let b = fold(&[base.clone(), vec![ev(Ev::Claim, "t1.v1", "sa2", 3).with_drone("d1")]].concat());
        assert_eq!(holder_of(&b, "t1.v1"), None);
        assert!(b.barred(&b.tasks["t1.v1"].task, "sa2") && !claimable_by(&b, &b.tasks["t1.v1"], "sa2", 3));
        let b = fold(&[base, vec![ev(Ev::Claim, "t1.v1", "sb", 3).with_drone("d2"), ev(Ev::Verified, "t1.v1", "sb", 4)]].concat());
        assert_eq!(holder_of(&b, "t1.v1"), None);
        assert!(b.finished(), "every task verified");
        assert!(!Board::default().finished(), "an empty board is not a reached goal");
    }

    #[test]
    fn rejected_reopens_with_the_note_and_counts_rework() {
        let b = fold(&[done_t1(), vec![ev(Ev::Claim, "t1.v1", "sb", 3), ev(Ev::Rejected, "t1.v1", "sb", 4).with_note("misses the edge case")]].concat());
        let t1 = &b.tasks["t1"];
        assert_eq!((&t1.status, t1.rework, t1.notes.as_slice()), (&Status::Open, 1, ["misses the edge case".to_string()].as_slice()));
        assert!(matches!(b.tasks["t1.v1"].status, Status::Verified { .. }), "the verify task is closed");
        assert_eq!(b.spans.last().unwrap().end, Some(Ev::Rejected));
    }

    #[test]
    fn verified_closes_work_and_verify() {
        let b = fold(&[done_t1(), vec![ev(Ev::Claim, "t1.v1", "sb", 3), ev(Ev::Verified, "t1.v1", "sb", 4)]].concat());
        assert!(matches!(b.tasks["t1"].status, Status::Verified { .. }) && matches!(b.tasks["t1.v1"].status, Status::Verified { .. }));
        let spans: Vec<(&str, Option<Ev>)> = b.spans.iter().map(|s| (s.by.as_str(), s.end)).collect();
        assert_eq!(spans, [("sa", Some(Ev::Done)), ("sb", Some(Ev::Verified))]);
        // A verdict from someone not holding the verify task changes nothing.
        let b = fold(&[done_t1(), vec![ev(Ev::Claim, "t1.v1", "sb", 3), ev(Ev::Verified, "t1.v1", "sc", 4)]].concat());
        assert!(matches!(b.tasks["t1"].status, Status::Done { .. }));
    }

    #[test]
    fn released_and_end_reopen() {
        let open = Event::open(task("t1"), "engineer:e", 0);
        let b = fold(&[open.clone(), ev(Ev::Claim, "t1", "sa", 1), ev(Ev::Released, "t1", "sb", 2)]);
        assert_eq!(holder_of(&b, "t1").as_deref(), Some("sa"), "only the holder releases");
        let b = fold(&[open.clone(), ev(Ev::Claim, "t1", "sa", 1), ev(Ev::Released, "t1", "sa", 2), ev(Ev::Claim, "t1", "sb", 3)]);
        assert_eq!(holder_of(&b, "t1").as_deref(), Some("sb"));
        let b = fold(&[open, ev(Ev::Claim, "t1", "sa", 1), ev(Ev::Released, "t1", "engineer:e", 2)]);
        assert_eq!(b.tasks["t1"].status, Status::Open, "an engineer may release any claim");
    }

    #[test]
    fn blocked_until_reopened() {
        let open = Event::open(task("t1"), "engineer:e", 0);
        let blocked = vec![open.clone(), ev(Ev::Claim, "t1", "sa", 1), ev(Ev::Blocked, "t1", "sa", 2).with_note("needs a key")];
        let b = fold(&[blocked.clone(), vec![ev(Ev::Claim, "t1", "sb", 3)]].concat());
        assert!(matches!(&b.tasks["t1"].status, Status::Blocked { note, .. } if note == "needs a key"));
        let mut again = open;
        again.ms = 4;
        let b = fold(&[blocked, vec![again, ev(Ev::Claim, "t1", "sb", 5)]].concat());
        assert_eq!(holder_of(&b, "t1").as_deref(), Some("sb"), "an open again reopens it");
    }

    #[test]
    fn done_by_non_holder_is_ignored() {
        let b = fold(&[Event::open(task("t1"), "engineer:e", 0), ev(Ev::Claim, "t1", "sa", 1), ev(Ev::Done, "t1", "sb", 2)]);
        assert_eq!(holder_of(&b, "t1").as_deref(), Some("sa"));
        // An expired holder's done counts while nobody claimed the task again.
        let b = fold(&[Event::open(task("t1"), "engineer:e", 0), ev(Ev::Claim, "t1", "sa", 1), ev(Ev::Done, "t1", "sa", LEASE_MS * 2)]);
        assert!(matches!(b.tasks["t1"].status, Status::Done { .. }));
    }

    #[test]
    fn unknown_or_partial_lines_are_skipped() {
        let dir = std::env::temp_dir().join(format!("borg-hive-board-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("tasks.jsonl");
        append(&path, &[Event::open(task("t1"), "engineer:e", 0), ev(Ev::Claim, "t1", "sa", 1)]).unwrap();
        let good = std::fs::read_to_string(&path).unwrap();
        let newer = serde_json::to_string(&Event { v: 2, ..ev(Ev::Released, "t1", "sa", 2) }).unwrap();
        let unknown = r#"{"v":1,"ev":"teleport","task":"t1","ms":3,"by":"sa"}"#;
        std::fs::write(&path, format!("{good}garbage\n{newer}\n{unknown}\n{{\"v\":1,\"ev\":\"released\",\"task\":\"t1\"")).unwrap();
        let events = read(&path);
        assert_eq!(events.len(), 2, "garbage, a newer format, an unknown event and a partial line are skipped");
        assert_eq!(holder_of(&fold(&events), "t1").as_deref(), Some("sa"));
        let (tail, end) = read_from(&path, good.len() as u64);
        assert!(tail.is_empty());
        assert!(end < std::fs::metadata(&path).unwrap().len(), "the partial line is read again next time");
        assert_eq!(read_from(&path, u64::MAX).0.len(), 2, "an offset past the end reads from the start");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_for_prefers_same_repo_then_verify() {
        let mut other = task("a-other");
        other.repo = "other".into();
        let mut mine = task("z-mine");
        mine.repo = "app".into();
        let mut v = verify("t0", "sx", 0);
        v.repo = "app".into();
        let mut t0 = task("t0");
        t0.repo = "app".into();
        let events = vec![
            Event::open(other, "engineer:e", 0),
            Event::open(mine, "engineer:e", 5),
            Event::open(t0, "engineer:e", 0),
            ev(Ev::Claim, "t0", "sx", 1),
            ev(Ev::Done, "t0", "sx", 2),
            Event::open(v, "sx", 2),
        ];
        let b = fold(&events);
        assert_eq!(next_for(&b, "me", "app", 3, |_| true).map(|s| s.task.id.as_str()), Some("t0.v1"), "verify before work in the same repo");
        assert_eq!(next_for(&b, "sx", "app", 3, |_| true).map(|s| s.task.id.as_str()), Some("z-mine"), "never the own verify task");
        assert_eq!(next_for(&b, "me", "elsewhere", 3, |_| true).map(|s| s.task.id.as_str()), Some("t0.v1"));
        assert_eq!(next_for(&b, "me", "other", 3, |_| true).map(|s| s.task.id.as_str()), Some("a-other"), "same repo first");
        assert_eq!(next_for(&b, "me", "other", 3, |t| t.repo != "other").map(|s| s.task.id.as_str()), Some("t0.v1"));
    }
}
