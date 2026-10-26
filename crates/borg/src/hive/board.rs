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
//! The goal is reached when every task is verified and an integration passed after the last of them
//! ([`Board::finished`]): an `integrate` task (hive/integrate.rs, run by `borg hive run` or `borg hive
//! integrate`, never by a model session) merges the accepted attempts and runs the oracle on the
//! combined tree; its `verified` or `rejected` carries that tree's commit (`head`). A rejected one
//! opens a fix task that starts `from` the combined tree.
//!
//! Lines are plain JSON with no `id` + `version` pair, so the mesh's `settle` (borg-mesh lines.rs) never
//! folds them. The board syncs over the mesh as a signed log (hive.md C4): each line carries the
//! node that wrote it (`node`, stamped by hive/mod.rs `write`; empty in a line from before), lines from
//! another node fold the same as local ones, and leases travel with the heartbeats that renew them.
//! Across nodes a claim is provisional until the next sync round: with drones live on other nodes,
//! `claim` (mod.rs) waits for a round with each reachable peer before work starts, so a loser mostly
//! learns before it began; past that window's cap it learns at its next hook (mod.rs `lost_news`), its
//! done and its verify task's open are ignored by the fold, so a loser wastes at most one sync latency
//! of work. The winner rule compares the writers' clocks, so the skew between
//! nodes must stay well below `LEASE_MS`. Drone names (`d1`..) repeat on every node: a drone name bars
//! a verify task only on the node its work was done on (`Board::barred`); a session id bars everywhere.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::goal::Check;
use super::{LEASE_MS, OWNER_GRACE_MS, TaskId};

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
    /// A fix task's start: the commit a claimant's worktree is reset to at its claim (the combined tree
    /// a failed integration left, hive/integrate.rs), and the base its diff is judged from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// A work task that is one attempt of a group (hive/group.rs): the group's id, which its K members
    /// (`<group>.g1`..) share. A session or drone that held one member is barred from the others
    /// (`Board::barred`), so each attempt is another drone's; only the group's best accepted attempt is
    /// integrated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<TaskId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// On `rejected`: the work weakened tests or changed CI it was not scoped to, changed the visible
    /// check's files, or touched the oracle's (hive/verdict.rs `decide`), whatever the verifier said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hacked: Option<bool>,
    /// On a claim of a work task: a commit of the claimant's working tree then (hive/verdict.rs
    /// `snapshot`); on its `done`, `head`, the same at the done. The verdict diffs the two.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base: String,
    /// On a claim of a work task with deps: `task@head` for each of its transitive deps (`closure`),
    /// whose verified work was in the claimant's tree before `base` was taken (brought in at the claim,
    /// hive/verdict.rs `bring_in`, or there already). A pass on work whose claim lacks one is rejected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps_in: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub head: String,
    /// On a claim: the drone the session is one iteration of (`borg hive run` names it, hive/run.rs);
    /// empty for a session of its own. A verify task barred to an author drone is barred to all its
    /// sessions.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub drone: String,
    /// On a verify task's `verified` or `rejected`: the outcome kind the verdict credits (`verified`,
    /// `rejected`, `hacked`; hive/verdict.rs `Kind::outcome`), empty while it waits for a hidden score.
    /// The node holding the author's shown records credits it (`verdict::credit_synced`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub credit: String,
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
            deps_in: Vec::new(),
            head: String::new(),
            credit: String::new(),
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
    /// The verify task whose `verified` made this work task verified: its attempt is what an
    /// integration merges.
    pub accepted: Option<TaskId>,
    /// The node its `open` was written on (empty: this one, or a line from before nodes were stamped).
    /// A verify task's open is written with its work's `done`: the work's node.
    pub node: String,
}

/// Whether two lines' nodes are one machine: equal, or either unknown (a line from before nodes were
/// stamped), which bars conservatively.
pub fn same_node(a: &str, b: &str) -> bool {
    a == b || a.is_empty() || b.is_empty()
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

/// An integration that passed: its integrate task, the combined tree's commit and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Integration {
    pub task: TaskId,
    pub head: String,
    pub ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Board {
    pub tasks: BTreeMap<TaskId, TaskState>,
    /// Claims that lost a race or were invalid: (by, task, ms).
    pub lost: Vec<(String, TaskId, i64)>,
    pub spans: Vec<Span>,
    /// The drone each session claimed as (`Event::drone`): session -> drone.
    pub drones: BTreeMap<String, String>,
    /// The newest integration that passed.
    pub integrated: Option<Integration>,
    /// Every integration that passed, oldest first.
    pub integrations: Vec<Integration>,
    /// The newest line a session of each node wrote (its ms), any line the fold took or not, an
    /// engineer's not (`by_engineer`): the nodes taking part (`Board::live_nodes`).
    pub nodes: BTreeMap<String, i64>,
    /// The drones each node's runner runs, from its newest line naming them (`roster_note`; mod.rs
    /// `announce` at the run's start, then one at each drone's end): node -> (ms, drone names).
    pub rosters: BTreeMap<String, (i64, BTreeSet<String>)>,
    /// Each runner drone's newest session start (`session_note`, run.rs `run_session`, stamped with the start)
    /// and its newest claim or session end: (node, drone) -> (start ms, settled ms). A start newer than
    /// what settled it is a session that has not claimed yet (`left_to_owner`).
    pub sessions: BTreeMap<(String, String), (i64, i64)>,
}

/// The note of a runner's line saying `drone`'s session starts or ended (`Board::sessions`):
/// `session-start d1`, `session-end d1`.
pub fn session_note(drone: &str, start: bool) -> String {
    format!("{} {drone}", if start { SESSION_START } else { SESSION_END })
}

const SESSION_START: &str = "session-start";
const SESSION_END: &str = "session-end";

/// The note of a runner's line naming the drones it runs (`Board::rosters`): `drones d1 d2 ...`.
pub fn roster_note(drones: &[String]) -> String {
    std::iter::once("drones").chain(drones.iter().map(String::as_str)).collect::<Vec<_>>().join(" ")
}

impl Board {
    /// Whether `s` is barred to `session` of `drone` (empty: none named) on `node`: by its id always;
    /// by `drone`, or by the drone the session claimed as before, only when the work was done on the
    /// same node (`same_node`: every runner names its drones d1..dN). The caller names the drone: a new
    /// iteration's session has no line on the board yet.
    pub fn barred(&self, s: &TaskState, session: &str, drone: &str, node: &str) -> bool {
        let here = same_node(&s.node, node);
        let named = |n: &str| (!drone.is_empty() && n == drone) || self.drones.get(session).is_some_and(|d| d == n);
        s.task.not_by.iter().any(|n| n == session || (here && named(n))) || self.rival(s, session, node, named)
    }

    /// The group member `t` belongs to: itself when it is one, the member it verifies when it is that
    /// member's verify task; with the group's id.
    fn member_of<'a>(&'a self, t: &'a Task) -> Option<(&'a str, &'a str)> {
        let m = match t.role {
            Role::Verify => &self.tasks.get(t.of.as_deref()?)?.task,
            _ => t,
        };
        Some((m.id.as_str(), m.group.as_deref()?))
    }

    /// Whether `session` (or the drone `named` matches, on the same node) held a task of another member
    /// of the group `s` belongs to (`member_of`), so its claim of `s` is not independent: a group's
    /// attempts are different drones' (hive/group.rs), a drone that verified one member has read a rival
    /// solution and may not attempt another, and one that attempted a member may not judge a rival's.
    /// Verifying several members is no rivalry: one verifier may judge them all.
    fn rival(&self, s: &TaskState, session: &str, node: &str, named: impl Fn(&str) -> bool) -> bool {
        let Some((mine, g)) = self.member_of(&s.task) else { return false };
        let verifying = s.task.role == Role::Verify;
        let rivals = |t: &str| {
            self.tasks
                .get(t)
                .and_then(|o| Some((o.task.role, self.member_of(&o.task)?)))
                .is_some_and(|(role, (m, og))| og == g && m != mine && !(verifying && role == Role::Verify))
        };
        self.spans
            .iter()
            .filter(|sp| rivals(&sp.task))
            .any(|sp| sp.by == session || (same_node(&sp.node, node) && self.drones.get(&sp.by).is_some_and(|d| named(d))))
    }

    /// The goal is reached: there are tasks, every one is verified (a work task by its verify task, a
    /// verify or integrate task by its verdict), and an integration passed no earlier than the last
    /// verification: work verified after it (a fix task's) is not in the combined tree yet.
    pub fn finished(&self) -> bool {
        let Some(i) = &self.integrated else { return false };
        !self.tasks.is_empty()
            && self.tasks.values().all(|s| match &s.status {
                Status::Verified { ms, .. } => s.task.role == Role::Integrate || *ms <= i.ms,
                _ => false,
            })
    }

    /// Some task is held under a live lease at `now`: its done will open a verify task, its release
    /// reopens it, and an abandoned one becomes claimable when the lease runs out.
    pub fn in_flight(&self, now: i64) -> bool {
        self.tasks.values().any(|s| s.live(now).is_some())
    }

    /// The nodes taking part at `now`: `node` (this one) and each whose sessions wrote a line within
    /// `LEASE_MS` (a claim, heartbeat, done or a verify task's open; a working drone renews its lease
    /// well within it). An engineer's lines (opens, releases, verdicts from the terminal) do not count:
    /// a node that only opened the goal may run no drone, and a share of its own would wait last on
    /// every other node, which then race for it. A runner's start writes a line of its own (mod.rs
    /// `announce`, a heartbeat of no task by `runner`), so its node is among them before any of its
    /// drones claims, once that line synced.
    pub fn live_nodes<'a>(&'a self, node: &'a str, now: i64) -> BTreeSet<&'a str> {
        self.nodes.iter().filter(|(n, ms)| !n.is_empty() && now - **ms <= LEASE_MS).map(|(n, _)| n.as_str()).chain([node]).collect()
    }
}

/// An integration is due: every task is verified (integrate tasks included: none open, held or blocked)
/// and the goal is not finished, so the combined tree of what was accepted has not been checked yet.
pub fn integration_due(b: &Board) -> bool {
    !b.tasks.is_empty() && b.tasks.values().all(|s| matches!(s.status, Status::Verified { .. })) && !b.finished()
}

/// The id of the next integrate task: `integrate.<n>`, one more than the integrate tasks so far (the
/// same on every node, so two runners racing open the same task).
pub fn integrate_id(b: &Board) -> TaskId {
    let mut n = b.tasks.values().filter(|s| s.task.role == Role::Integrate).count() + 1;
    while b.tasks.contains_key(&format!("integrate.{n}")) {
        n += 1;
    }
    format!("integrate.{n}")
}

/// `e` with the secrets in its text replaced (notes.rs `redact`, what presence prompts get): a task's
/// title and flags, a note. Ids, commits, scopes and checks stay as they are (the fold, the matching and
/// the oracle read them), and a check's command must run the same on every node: a secret belongs in
/// the environment, never in a check.
pub fn redacted(e: &Event) -> Event {
    let mut e = e.clone();
    if !e.note.is_empty() {
        e.note = crate::notes::redact(&e.note);
    }
    if let Some(t) = e.def.as_mut() {
        t.title = crate::notes::redact(&t.title);
        t.flags = t.flags.iter().map(|f| crate::notes::redact(f)).collect();
    }
    e
}

/// Appends `events` in one write(2) with O_APPEND (hive/mod.rs `append_bytes`, under the file's lock):
/// lines written together (a done and its verify task's open, a claim and its heartbeat) never
/// interleave with another writer's. Their text is [`redacted`] first: the board syncs to every member.
///
/// # Errors
/// When the directory or the file cannot be written.
pub fn append(path: &Path, events: &[Event]) -> std::io::Result<()> {
    let mut body = String::new();
    for e in events {
        body.push_str(&serde_json::to_string(&redacted(e)).map_err(std::io::Error::other)?);
        body.push('\n');
    }
    super::append_bytes(path, body.as_bytes())
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
    // Within one ms an open goes first (a task opened and claimed at once is claimed open), except the
    // open of a verify or fix task, which goes after the line written with it: it takes effect only on
    // a `done` or `rejected` the fold took.
    type Key = (i64, u8, [u8; 32], [u8; 32]);
    let order = |e: &Event| match (e.ev, e.def.as_ref().and_then(written_with)) {
        (Ev::Open, None) => 0,
        (Ev::Open, Some(_)) => 2,
        _ => 1,
    };
    let mut keyed: Vec<(Key, &Event)> = events.iter().map(|e| ((e.ms, order(e), rank(e), line_hash(e)), e)).collect();
    keyed.sort_by_key(|a| a.0);
    let mut b = Board { drones: events.iter().filter(|e| !e.drone.is_empty()).map(|e| (e.by.clone(), e.drone.clone())).collect(), ..Board::default() };
    for e in events.iter().filter(|e| !e.node.is_empty() && !by_engineer(&e.by)) {
        let ms = b.nodes.entry(e.node.clone()).or_insert(e.ms);
        *ms = (*ms).max(e.ms);
        if let Some(names) = (e.ev == Ev::Heartbeat && e.task.is_empty() && e.by == "runner").then(|| e.note.strip_prefix("drones")).flatten()
            && b.rosters.get(&e.node).is_none_or(|(at, _)| *at <= e.ms)
        {
            b.rosters.insert(e.node.clone(), (e.ms, names.split_whitespace().map(String::from).collect()));
        }
        let mark = match (e.ev, e.by.as_str(), e.note.split_once(' ')) {
            (Ev::Claim, _, _) if !e.drone.is_empty() => Some((e.drone.as_str(), false)),
            (Ev::Heartbeat, "runner", Some((SESSION_START, d))) if e.task.is_empty() => Some((d, true)),
            (Ev::Heartbeat, "runner", Some((SESSION_END, d))) if e.task.is_empty() => Some((d, false)),
            _ => None,
        };
        if let Some((d, start)) = mark {
            let (started, settled) = b.sessions.entry((e.node.clone(), d.to_string())).or_insert((i64::MIN, i64::MIN));
            let at = if start { started } else { settled };
            *at = (*at).max(e.ms);
        }
    }
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

/// The transitive deps of `t`, each once, ordered by when they were verified (then by id; one not
/// verified last): a dep is verified before its dependent can be claimed, so this order is
/// topological, and each dep's work is brought into a tree after the deps it was built on.
pub fn closure(b: &Board, t: &Task) -> Vec<TaskId> {
    let mut seen: Vec<TaskId> = Vec::new();
    let mut todo: Vec<TaskId> = t.deps.clone();
    while let Some(d) = todo.pop() {
        if d == t.id || seen.contains(&d) {
            continue;
        }
        todo.extend(b.tasks.get(&d).map(|s| s.task.deps.clone()).unwrap_or_default());
        seen.push(d);
    }
    let verified = |d: &TaskId| match b.tasks.get(d).map(|s| &s.status) {
        Some(Status::Verified { ms, .. }) => *ms,
        _ => i64::MAX,
    };
    seen.sort_by_key(|d| (verified(d), d.clone()));
    seen
}

/// The task whose line an open is written with: a verify task's work (its `done`), a fix task's
/// integrate task (its `rejected`, hive/integrate.rs).
fn written_with(t: &Task) -> Option<&str> {
    t.of.as_deref().or_else(|| t.from.as_ref().and(t.id.strip_suffix(".fix")))
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
            // A verify task's open is part of its work's `done`, a fix task's of its integration's
            // `rejected` (same session, same ms): a line the fold ignored (the lease had passed to
            // another drone or runner) opens nothing, so the task and its bar are those of the line
            // that took effect.
            if let Some(w) = written_with(def) {
                let took = match b.tasks.get(w).map(|s| &s.status) {
                    Some(Status::Done { by, ms }) if def.of.is_some() => *by == e.by && *ms == e.ms,
                    Some(Status::Verified { by, ms }) if def.of.is_none() => *by == e.by && *ms == e.ms,
                    _ => false,
                };
                if !took {
                    return;
                }
            }
            match b.tasks.get_mut(&e.task) {
                None => {
                    // An open's note is the task's first note (a fix task's: why the integration failed).
                    let s = TaskState {
                        task: def.clone(),
                        status: Status::Open,
                        opened_ms: e.ms,
                        rework: 0,
                        notes: if e.note.is_empty() { Vec::new() } else { vec![e.note.clone()] },
                        last_progress: String::new(),
                        author: None,
                        accepted: None,
                        node: e.node.clone(),
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
            if !s.claimable(e.ms) || !deps_met(&b.tasks, &s.task) || b.barred(s, &e.by, &e.drone, &e.node) {
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
            if !matches!(s.task.role, Role::Verify | Role::Integrate) || holder(s) != Some(e.by.as_str()) {
                return;
            }
            s.status = Status::Verified { by: e.by.clone(), ms: e.ms };
            if e.ev == Ev::Rejected {
                s.last_progress = format!("rejected: {}", e.note);
            }
            let (role, of) = (s.task.role, s.task.of.clone());
            close(b, open_span, &e.task, e.ms, Some(e.ev));
            // An integration's verdict is on the combined tree, no one task's: a rejected one touches
            // no other task (its fix task's open is written with it).
            if role == Role::Integrate {
                if e.ev == Ev::Verified {
                    let i = Integration { task: e.task.clone(), head: e.head.clone(), ms: e.ms };
                    b.integrations.push(i.clone());
                    b.integrated = Some(i);
                }
                return;
            }
            let Some(w) = of.and_then(|w| b.tasks.get_mut(&w)) else { return };
            if !matches!(w.status, Status::Done { .. }) {
                return;
            }
            if e.ev == Ev::Verified {
                w.status = Status::Verified { by: e.by.clone(), ms: e.ms };
                w.accepted = Some(e.task.clone());
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

/// Whether `session` of `drone` on `node` may claim `s` at `now` (the fold's rules a-c: open or
/// expired, dependencies verified, not barred).
pub fn claimable_by(b: &Board, s: &TaskState, session: &str, drone: &str, node: &str, now: i64) -> bool {
    s.claimable(now) && deps_met(&b.tasks, &s.task) && !b.barred(s, session, drone, node)
}

/// The task `session` of `drone` on `node` in `repo` should take next at `now`, among those `allowed`
/// lets it claim: its own repo's first, then by role (verify, integrate, work, plan); within one such
/// class the tasks this node owns among the live nodes (`owner`), oldest first, then the others newest
/// first (by `affinity` among equals), then by id. Every node owns a different share, so drones on two
/// nodes start apart instead of racing for one task; a node whose share ran out takes another's from
/// the far end, which its owner reaches last (two nodes overlap only where k + k drones exceed the
/// tasks). On one node it owns them all, the order of before (oldest, then id).
///
/// Drones of one node start apart too: the node's idle running drones before `drone` (`idle_before`,
/// in their order d1, d2, ...) each take, in turn, the first task in this order that they may claim
/// and no drone before them took, and `drone` takes the first one left (`free_for`). Two drones
/// reading one board pick different tasks while there are enough, also when one of them may not claim
/// some (its own work's verify task, another node's task left to its owner), and a drone that sees
/// another's claim picks as if it had not: the claimed task left the board's open tasks and its drone
/// the idle ones. Past its node's share a drone walks on into the others' far end. With every task it
/// may claim taken by a drone before it, it gets the first of them anyway: a session that runs takes
/// what is there (a runner starts none for it, `free_for`). A session without a runner's drone name
/// has no drone before it.
///
/// Another node's task is left to its owner while one of the owner's idle drones will take it
/// (`left_to_owner`), so an idle drone here does not race the owner's for it (d3-accept2: node b's
/// tasks all went to node a's drones, and b's sessions ran empty); a busy owner's tasks still spill.
pub fn next_for<'a>(b: &'a Board, session: &str, drone: &str, node: &str, repo: &str, now: i64, allowed: impl Fn(&TaskState) -> bool) -> Option<&'a TaskState> {
    pick(b, session, drone, node, repo, now, allowed).map(|p| p.free.unwrap_or(p.first))
}

/// `next_for` without its last resort: None when every task `drone` may claim is one an idle drone of
/// its node before it takes, so a runner leaves it to that drone instead of starting a session that
/// would race it (two drones woken by one change of the board, run.rs `idle_wait`); `taken_before`
/// says whether that is so.
pub fn free_for<'a>(b: &'a Board, session: &str, drone: &str, node: &str, repo: &str, now: i64, allowed: impl Fn(&TaskState) -> bool) -> Option<&'a TaskState> {
    pick(b, session, drone, node, repo, now, allowed).and_then(|p| p.free)
}

/// Whether a task `drone` may claim is taken by an idle drone of its node before it (`free_for`): one
/// that drone will claim, so there is work in hand on the board.
pub fn taken_before(b: &Board, drone: &str, node: &str, repo: &str, now: i64, allowed: impl Fn(&TaskState) -> bool) -> bool {
    pick(b, "", drone, node, repo, now, allowed).is_some_and(|p| p.free.is_none())
}

/// Whether the work `drone` of `node` could take is in hand at `now`, so an idle drone waits on it
/// instead of ending (run.rs `open_to`): a task is held (`Board::in_flight`), a live peer's session
/// started and has not claimed or ended (`Board::sessions`: it is about to hold one, also a task this
/// drone may not claim, such as its own work's verify task, whose verdict may open a rework it can
/// take), one it may claim is left to another node's idle drones (`left_to_another`: d3-accept3, a
/// peer's session took 12 to 15 s to claim, and the drone here ended on idle polls while the task it
/// was waiting for was hidden from it), or one is an idle drone's of its node before it
/// (`taken_before`, which `free_for` leaves out). The wait on a peer is bounded by a lease, not
/// seconds: a session counts, and a task is left to it, only while it has neither claimed nor ended
/// (or for `OWNER_GRACE_MS` after the task became claimable), and only while its node wrote a line
/// within `LEASE_MS` (`Board::live_nodes`). A peer stopped mid-session without its session end is
/// waited on for up to `LEASE_MS`, as an abandoned claim is under `in_flight`.
pub fn in_hand(b: &Board, drone: &str, node: &str, repo: &str, now: i64, allowed: impl Fn(&TaskState) -> bool) -> bool {
    let live = b.live_nodes(node, now);
    let starting = b.sessions.iter().any(|((n, _), (started, settled))| n != node && live.contains(n.as_str()) && started > settled);
    b.in_flight(now)
        || starting
        || b.tasks.values().any(|s| claimable_by(b, s, "", drone, node, now) && allowed(s) && left_to_owner(b, &live, s, node, now))
        || taken_before(b, drone, node, repo, now, &allowed)
}

/// What `next_for` found: the first task the drone may claim that no idle drone before it takes, and
/// the first it may claim at all.
struct Pick<'a> {
    free: Option<&'a TaskState>,
    first: &'a TaskState,
}

fn pick<'a>(b: &'a Board, session: &str, drone: &str, node: &str, repo: &str, now: i64, allowed: impl Fn(&TaskState) -> bool) -> Option<Pick<'a>> {
    let live = b.live_nodes(node, now);
    let class = |s: &TaskState| (repo.is_empty() || !s.task.repo.eq_ignore_ascii_case(repo), s.task.role.urgency());
    let key = |s: &TaskState| {
        let place = match owner(&live, &s.task.id) == Some(node) {
            true => (false, s.opened_ms, Reverse([0; 32])),
            false => (true, -s.opened_ms, Reverse(affinity(node, &s.task.id))),
        };
        (class(s), place, s.task.id.clone())
    };
    let open = |s: &TaskState, sess: &str, d: &str| claimable_by(b, s, sess, d, node, now) && allowed(s) && !left_to_owner(b, &live, s, node, now);
    let mut order: Vec<&TaskState> = b.tasks.values().collect();
    order.sort_by_cached_key(|s| key(s));
    let first = *order.iter().find(|s| open(s, session, drone))?;
    let mut taken: BTreeSet<&str> = BTreeSet::new();
    for d in idle_before(b, drone, node, now) {
        if let Some(t) = order.iter().find(|s| !taken.contains(s.task.id.as_str()) && open(s, "", &d)) {
            taken.insert(&t.task.id);
        }
    }
    let free = order.iter().copied().find(|s| !taken.contains(s.task.id.as_str()) && open(s, session, drone));
    Some(Pick { free, first })
}

/// Where drone `dN` of a runner stands in `next_for`'s turn: N - 1; 0 for any other name.
fn slot(drone: &str) -> usize {
    drone.strip_prefix('d').and_then(|n| n.parse::<usize>().ok()).map_or(0, |n| n.saturating_sub(1))
}

/// The drones of `node` before `drone` (by `slot`) that run and hold no task at `now`, in that order:
/// not holding a live claim there, and not ended (named in the node's newest `rosters` entry; without
/// one, every drone before it runs).
fn idle_before(b: &Board, drone: &str, node: &str, now: i64) -> Vec<String> {
    let ended = |i: usize| b.rosters.get(node).is_some_and(|(_, r)| !r.iter().any(|d| slot(d) == i));
    let busy: BTreeSet<usize> = b
        .tasks
        .values()
        .filter_map(|s| s.live(now).filter(|(_, at, _)| same_node(at, node)))
        .filter_map(|(by, _, _)| b.drones.get(by).filter(|d| d.as_str() != drone))
        .map(|d| slot(d))
        .collect();
    (0..slot(drone)).filter(|&i| !busy.contains(&i) && !ended(i)).map(|i| format!("d{}", i + 1)).collect()
}

/// Whether `s` is another node's task left to its owner's idle drones at `now` (`left_to_owner`), as a
/// drone of `node` reads `b`.
pub fn left_to_another(b: &Board, s: &TaskState, node: &str, now: i64) -> bool {
    left_to_owner(b, &b.live_nodes(node, now), s, node, now)
}

/// Whether `s`, owned by another node of `live` than `node`, is left to its owner at `now`: the owner's
/// runner names idle drones that may claim it (in its newest roster, holding no live claim there), more
/// of them than the owner's claimable tasks before it in the order its drones walk (role, then oldest)
/// that one of those drones may claim. An idle drone whose session started and has not claimed yet
/// (`Board::sessions`) is taking one: the model claims inside the session, after reading its prompt and
/// hooks, which takes seconds to tens of them. One that started none counts while `s` became claimable
/// less than `OWNER_GRACE_MS` ago, the time for its runner to notice and its start to reach here. The
/// tasks past the owner's idle drones spill at once, and an owner that did not take one (its drones
/// barred, slow to start, or its session went for something else and claimed it or ended) loses no
/// work: the task goes to whoever is idle. A node without a roster (no runner) keeps nothing back.
fn left_to_owner(b: &Board, live: &BTreeSet<&str>, s: &TaskState, node: &str, now: i64) -> bool {
    let Some(o) = owner(live, &s.task.id).filter(|o| *o != node) else { return false };
    let Some((_, roster)) = b.rosters.get(o) else { return false };
    let busy: BTreeSet<&str> =
        b.tasks.values().filter_map(|t| t.live(now).filter(|(_, at, _)| *at == o)).filter_map(|(by, _, _)| b.drones.get(by).map(String::as_str)).collect();
    let starting = |d: &str| b.sessions.get(&(o.to_string(), d.to_string())).is_some_and(|(started, settled)| started > settled);
    let fresh = now - claimable_since(b, s) < OWNER_GRACE_MS;
    let idle: Vec<&str> =
        roster.iter().map(String::as_str).filter(|d| !busy.contains(d) && (fresh || starting(d)) && claimable_by(b, s, "", d, o, now)).collect();
    let key = |t: &TaskState| (t.task.role.urgency(), t.opened_ms, t.task.id.clone());
    let ahead =
        b.tasks.values().filter(|t| owner(live, &t.task.id) == Some(o) && key(t) < key(s) && idle.iter().any(|d| claimable_by(b, t, "", d, o, now))).count();
    ahead < idle.len()
}

/// When `s` last became claimable, as far as the board says: its open, the end of its last tenure (an
/// expired lease at its expiry), its work's done (a verify task), its deps' verification, its last
/// verdict (a rework). A blocked task the engineer reopened reads as of its block: earlier, a shorter
/// grace.
pub fn claimable_since(b: &Board, s: &TaskState) -> i64 {
    let id = s.task.id.as_str();
    let at = |t: &str| match b.tasks.get(t).map(|t| &t.status) {
        Some(Status::Done { ms, .. } | Status::Verified { ms, .. }) => *ms,
        _ => i64::MIN,
    };
    let expired = match &s.status {
        Status::Claimed { renewed, .. } => renewed + LEASE_MS,
        _ => i64::MIN,
    };
    let ended = b.spans.iter().filter(|sp| sp.task == id).filter_map(|sp| sp.to_ms).max().unwrap_or(i64::MIN);
    let verdict = b.tasks.values().filter(|v| v.task.of.as_deref() == Some(id)).map(|v| at(&v.task.id)).max().unwrap_or(i64::MIN);
    let deps = s.task.deps.iter().chain(&s.task.of).map(|d| at(d)).max().unwrap_or(i64::MIN);
    [s.opened_ms, expired, ended, verdict, deps].into_iter().max().unwrap_or(s.opened_ms)
}

/// The node of `live` that owns `task`: the highest `affinity` (rendezvous hashing, as borg-mesh
/// sync.rs `replicas` places excerpts: every node computes the same owner, and a node joining or
/// leaving moves only the tasks it gains or loses).
pub fn owner<'a>(live: &BTreeSet<&'a str>, task: &str) -> Option<&'a str> {
    live.iter().copied().max_by_key(|n| (affinity(n, task), *n))
}

/// How much `task` is `node`'s: blake3 of both, domain-tagged.
fn affinity(node: &str, task: &str) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"borg-hive-owner-v1\0");
    for part in [node, task] {
        h.update(part.as_bytes());
        h.update(b"\0");
    }
    *h.finalize().as_bytes()
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
            from: None,
            group: None,
        }
    }

    fn ev(kind: Ev, t: &str, by: &str, ms: i64) -> Event {
        Event::new(kind, t, by, ms)
    }

    #[test]
    fn a_secret_in_a_title_or_note_never_reaches_the_board() {
        let path = std::env::temp_dir().join(format!("borg-hive-board-redact-{}/tasks.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let head = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b";
        let t = Task { title: format!("rotate the CI token {secret} in deploy.yml"), files: vec!["deploy.yml".into()], ..task("t1") };
        let mut done = ev(Ev::Done, "t1", "s1", 3).with_note(&format!("used password=hunter2 then {head}"));
        done.head = head.into();
        append(&path, &[Event::open(t, "engineer:e", 1), ev(Ev::Claim, "t1", "s1", 2), done]).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains(secret) && !raw.contains("hunter2"), "{raw}");
        let b = fold(&read(&path));
        assert_eq!(b.tasks["t1"].task.title, "rotate the CI token [REDACTED] in deploy.yml");
        assert_eq!(b.tasks["t1"].task.files, ["deploy.yml"], "the scope is kept");
        assert!(matches!(b.tasks["t1"].status, Status::Done { .. }), "the fold still works");
        assert!(raw.contains(&format!("\"head\":\"{head}\"")) && raw.contains(&format!("then {head}")), "commit ids are kept");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
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
        assert!(b.in_flight(2 * LEASE_MS - 1) && !b.in_flight(2 * LEASE_MS), "in flight while the lease lives");
        let b = fold(&[Event::open(task("t2"), "engineer:e", 0), ev(Ev::Claim, "t2", "sa", 0), ev(Ev::Done, "t2", "sa", 1)]);
        assert!(!b.in_flight(2), "a done task is not in flight");
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

    #[test]
    fn closure_is_transitive_and_in_verified_order() {
        // t3 after t2 after t1, and after t0 directly; t0 verified last of them, t1 first.
        let dep = |id: &str, deps: &[&str]| Task { deps: deps.iter().map(|d| d.to_string()).collect(), ..task(id) };
        let mut lines = vec![
            Event::open(dep("t0", &[]), "engineer:e", 0),
            Event::open(dep("t1", &[]), "engineer:e", 0),
            Event::open(dep("t2", &["t1"]), "engineer:e", 0),
            Event::open(dep("t3", &["t0", "t2", "t1"]), "engineer:e", 0),
        ];
        for (n, t) in ["t1", "t2", "t0"].iter().enumerate() {
            let ms = 10 * (n as i64 + 1);
            lines.extend([ev(Ev::Claim, t, "sa", ms), ev(Ev::Done, t, "sa", ms + 1), Event::open(verify(t, "sa", 0), "sa", ms + 1)]);
            lines.extend([ev(Ev::Claim, &format!("{t}.v1"), "sb", ms + 2), ev(Ev::Verified, &format!("{t}.v1"), "sb", ms + 3)]);
        }
        let b = fold(&lines);
        assert_eq!(closure(&b, &b.tasks["t3"].task), ["t1", "t2", "t0"], "each dep once, by when it was verified");
        assert_eq!(closure(&b, &b.tasks["t2"].task), ["t1"]);
        assert!(closure(&b, &b.tasks["t1"].task).is_empty());
        // A cycle (a board from before init refused them) ends: nothing twice, never the task itself.
        let b = fold(&[Event::open(dep("a", &["b"]), "engineer:e", 0), Event::open(dep("b", &["a"]), "engineer:e", 0)]);
        assert_eq!(closure(&b, &b.tasks["a"].task), ["b"]);
    }

    fn verify(of: &str, author: &str, _ms: i64) -> Task {
        Task { id: format!("{of}.v1"), role: Role::Verify, of: Some(of.into()), not_by: vec![author.into()], ..task("x") }
    }

    /// Work `t` done at `ms` by `session` (of `drone`, when named), its verify task barred to both.
    fn worked(t: &str, session: &str, drone: &str, ms: i64) -> Vec<Event> {
        let mut v = verify(t, session, ms);
        v.not_by.extend((!drone.is_empty()).then(|| drone.to_string()));
        vec![Event::open(task(t), "engineer:e", 0), ev(Ev::Claim, t, session, 0).with_drone(drone), ev(Ev::Done, t, session, ms), Event::open(v, session, ms)]
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
        assert!(!claimable_by(&b, &b.tasks["t1.v1"], "sa", "", "", 3) && claimable_by(&b, &b.tasks["t1.v1"], "sb", "", "", 3));
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
        assert!(b.barred(&b.tasks["t1.v1"], "sa2", "", "") && !claimable_by(&b, &b.tasks["t1.v1"], "sa2", "", "", 3));
        assert!(b.barred(&b.tasks["t1.v1"], "sa3", "d1", "") && !b.barred(&b.tasks["t1.v1"], "sa3", "d2", ""));
        // A fresh iteration of d1 that wrote no line yet is not offered it either.
        assert_eq!(next_for(&b, "sa3", "d1", "", "", 3, |_| true).map(|s| s.task.id.as_str()), None, "its own verify task is not offered to d1");
        let b = fold(&[base, vec![ev(Ev::Claim, "t1.v1", "sb", 3).with_drone("d2"), ev(Ev::Verified, "t1.v1", "sb", 4)]].concat());
        assert_eq!(holder_of(&b, "t1.v1"), None);
        assert!(!b.finished() && integration_due(&b), "every task verified: the combined tree is checked next");
        assert!(!Board::default().finished() && !integration_due(&Board::default()), "an empty board is not a reached goal");
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
        assert_eq!(next_for(&b, "me", "", "", "app", 3, |_| true).map(|s| s.task.id.as_str()), Some("t0.v1"), "verify before work in the same repo");
        assert_eq!(next_for(&b, "sx", "", "", "app", 3, |_| true).map(|s| s.task.id.as_str()), Some("z-mine"), "never the own verify task");
        assert_eq!(next_for(&b, "me", "", "", "elsewhere", 3, |_| true).map(|s| s.task.id.as_str()), Some("t0.v1"));
        assert_eq!(next_for(&b, "me", "", "", "other", 3, |_| true).map(|s| s.task.id.as_str()), Some("a-other"), "same repo first");
        assert_eq!(next_for(&b, "me", "", "", "other", 3, |s| s.task.repo != "other").map(|s| s.task.id.as_str()), Some("t0.v1"));
    }

    fn integrate(id: &str) -> Task {
        Task { role: Role::Integrate, title: "integrate".into(), ..task(id) }
    }

    fn verdict(kind: Ev, t: &str, by: &str, ms: i64, head: &str) -> Event {
        Event { head: head.into(), ..ev(kind, t, by, ms) }
    }

    /// t1 done and verified (at 4), integrate.1 opened and claimed by `si` at 5.
    fn verified_t1() -> Vec<Event> {
        [done_t1(), vec![ev(Ev::Claim, "t1.v1", "sb", 3), ev(Ev::Verified, "t1.v1", "sb", 4)]].concat()
    }

    fn claimed_integrate(id: &str, ms: i64) -> Vec<Event> {
        vec![Event::open(integrate(id), "si", ms), ev(Ev::Claim, id, "si", ms)]
    }

    #[test]
    fn finished_needs_an_integration_that_passed_after_every_verification() {
        let b = fold(&verified_t1());
        assert_eq!(b.tasks["t1"].accepted.as_deref(), Some("t1.v1"), "the passing verify task is the accepted attempt's");
        assert_eq!(integrate_id(&b), "integrate.1");
        let pass = [verified_t1(), claimed_integrate("integrate.1", 5), vec![verdict(Ev::Verified, "integrate.1", "si", 6, "m1")]].concat();
        let b = fold(&pass);
        assert!(b.finished() && !integration_due(&b), "{:?}", b.integrated);
        assert_eq!(b.integrated, Some(Integration { task: "integrate.1".into(), head: "m1".into(), ms: 6 }));
        assert_eq!(integrate_id(&b), "integrate.2");
        // While it is held, nothing is due and the goal is not reached.
        let b = fold(&[verified_t1(), claimed_integrate("integrate.1", 5)].concat());
        assert!(!b.finished() && !integration_due(&b) && b.in_flight(6));
        // A verdict from someone not holding it changes nothing.
        let b = fold(&[verified_t1(), claimed_integrate("integrate.1", 5), vec![verdict(Ev::Verified, "integrate.1", "sx", 6, "m1")]].concat());
        assert!(!b.finished() && b.integrated.is_none() && holder_of(&b, "integrate.1").as_deref() == Some("si"));
    }

    #[test]
    fn a_rejected_integration_opens_a_fix_and_the_next_integration_finishes() {
        let mut fix = task("integrate.1.fix");
        fix.from = Some("m1".into());
        let rejected = [
            verified_t1(),
            claimed_integrate("integrate.1", 5),
            vec![verdict(Ev::Rejected, "integrate.1", "si", 6, "m1").with_note("visible fails"), Event::open(fix, "si", 6).with_note("visible fails")],
        ]
        .concat();
        let b = fold(&rejected);
        assert!(!b.finished() && b.integrated.is_none(), "a rejected integration does not finish the board");
        assert!(matches!(b.tasks["integrate.1"].status, Status::Verified { .. }) && b.tasks["integrate.1"].last_progress == "rejected: visible fails");
        assert!(matches!(b.tasks["t1"].status, Status::Verified { .. }), "it touches no work task");
        let f = &b.tasks["integrate.1.fix"];
        assert_eq!((f.status.clone(), f.task.from.as_deref(), f.notes.as_slice()), (Status::Open, Some("m1"), ["visible fails".to_string()].as_slice()));
        assert!(!integration_due(&b), "the fix is open");
        let mut fv = verify("integrate.1.fix", "sa", 0);
        fv.id = "integrate.1.fix.v1".into();
        let fixed = vec![
            ev(Ev::Claim, "integrate.1.fix", "sa", 7),
            ev(Ev::Done, "integrate.1.fix", "sa", 8),
            Event::open(fv, "sa", 8),
            ev(Ev::Claim, "integrate.1.fix.v1", "sb", 9),
            ev(Ev::Verified, "integrate.1.fix.v1", "sb", 10),
        ];
        let b = fold(&[rejected.clone(), fixed.clone()].concat());
        assert!(integration_due(&b) && !b.finished(), "the fix is verified after the last integration");
        assert_eq!(integrate_id(&b), "integrate.2");
        let b = fold(&[rejected, fixed, claimed_integrate("integrate.2", 11), vec![verdict(Ev::Verified, "integrate.2", "si", 12, "m2")]].concat());
        assert!(b.finished(), "{:?}", b.integrated);
        assert_eq!(b.integrated.map(|i| i.head).as_deref(), Some("m2"));
    }

    #[test]
    fn a_stale_integrators_rejection_opens_no_fix_task() {
        // si's lease on integrate.1 ran out and sj's integration passed; si then writes its rejection and
        // the fix task with it. Neither takes effect, in any order.
        let mut fix = task("integrate.1.fix");
        fix.from = Some("m1".into());
        let late = 5 + LEASE_MS + 1;
        let tail = vec![
            ev(Ev::Claim, "integrate.1", "sj", late),
            verdict(Ev::Verified, "integrate.1", "sj", late + 1, "m2"),
            verdict(Ev::Rejected, "integrate.1", "si", late + 2, "m1").with_note("visible fails"),
            Event::open(fix, "si", late + 2).with_note("visible fails"),
        ];
        for p in permutations(&tail) {
            let b = fold(&[verified_t1(), claimed_integrate("integrate.1", 5), p].concat());
            assert!(!b.tasks.contains_key("integrate.1.fix"), "no orphan fix task");
            assert!(b.finished(), "{:?}", b.integrated);
        }
    }

    #[test]
    fn two_runners_racing_open_one_integrate_task_and_one_claim_wins() {
        let race = [
            verified_t1(),
            vec![Event::open(integrate("integrate.1"), "ra", 5), ev(Ev::Claim, "integrate.1", "ra", 5)],
            vec![Event::open(integrate("integrate.1"), "rb", 5), ev(Ev::Claim, "integrate.1", "rb", 5)],
        ]
        .concat();
        let winners: std::collections::BTreeSet<Option<String>> = permutations(&race[race.len() - 4..])
            .into_iter()
            .map(|tail| {
                let b = fold(&[race[..race.len() - 4].to_vec(), tail].concat());
                assert_eq!(b.tasks.values().filter(|s| s.task.role == Role::Integrate).count(), 1);
                assert_eq!(b.lost.len(), 1);
                holder_of(&b, "integrate.1")
            })
            .collect();
        assert_eq!(winners.len(), 1, "the same winner in every order: {winners:?}");
        assert!(winners.iter().all(Option::is_some));
    }

    fn on(mut e: Event, node: &str) -> Event {
        e.node = node.into();
        e
    }

    /// t1 done by session sa of drone d1 on node `node`, its verify task barred to [sa, d1].
    fn done_by_d1_on(node: &str) -> Vec<Event> {
        let mut v = verify("t1", "sa", 2);
        v.not_by.push("d1".into());
        vec![
            Event::open(task("t1"), "engineer:e", 0),
            on(ev(Ev::Claim, "t1", "sa", 1).with_drone("d1"), node),
            on(ev(Ev::Done, "t1", "sa", 2), node),
            on(Event::open(v, "sa", 2), node),
        ]
    }

    #[test]
    fn a_fresh_session_of_the_author_drone_is_not_offered_its_verify_task() {
        let b = fold(&done_by_d1_on(""));
        assert!(b.spans.iter().all(|s| s.by != "fresh") && !b.drones.contains_key("fresh"), "the board has no line of the fresh session");
        assert_eq!(b.tasks["t1.v1"].task.not_by, ["sa", "d1"]);
        assert_eq!(next_for(&b, "fresh", "d1", "", "", 3, |_| true).map(|s| s.task.id.as_str()), None, "the runner's drone name bars it");
        assert_eq!(next_for(&b, "fresh", "d2", "", "", 3, |_| true).map(|s| s.task.id.as_str()), Some("t1.v1"));
    }

    #[test]
    fn a_drone_name_bars_only_on_its_own_node() {
        let base = done_by_d1_on("A");
        assert_eq!(fold(&base).tasks["t1.v1"].node, "A", "a verify task's node is its work's");
        // B's d1 is another drone: its claim takes effect. A's d1 (a later session) is voided.
        let b = fold(&[base.clone(), vec![on(ev(Ev::Claim, "t1.v1", "sb", 3).with_drone("d1"), "B")]].concat());
        assert_eq!(holder_of(&b, "t1.v1").as_deref(), Some("sb"));
        let b = fold(&[base.clone(), vec![on(ev(Ev::Claim, "t1.v1", "sa2", 3).with_drone("d1"), "A")]].concat());
        assert_eq!(holder_of(&b, "t1.v1"), None);
        // A claimant line without a node (from before nodes were stamped) bars conservatively.
        let b = fold(&[base.clone(), vec![ev(Ev::Claim, "t1.v1", "sa3", 3).with_drone("d1")]].concat());
        assert_eq!(holder_of(&b, "t1.v1"), None);
        // The author's session bars on every node.
        let b = fold(&[base, vec![on(ev(Ev::Claim, "t1.v1", "sa", 3), "B")]].concat());
        assert_eq!(holder_of(&b, "t1.v1"), None);
        assert!(same_node("A", "A") && same_node("", "B") && !same_node("A", "B"));
    }

    #[test]
    fn remote_lines_fold_the_same_and_one_claim_wins_across_nodes() {
        let lines = vec![
            Event::open(task("t1"), "engineer:e", 0),
            on(ev(Ev::Claim, "t1", "sa", 100), "A"),
            on(ev(Ev::Claim, "t1", "sb", 90), "B"),
            on(ev(Ev::Done, "t1", "sa", 120), "A"),
            on(ev(Ev::Heartbeat, "t1", "sb", 110), "B"),
        ];
        let all = permutations(&lines);
        let first = fold(&all[0]);
        for p in &all {
            let b = fold(p);
            assert_eq!(b, first, "the same board in every arrival order");
            assert_eq!(holder_of(&b, "t1").as_deref(), Some("sb"), "the earlier claim, on B, holds");
            assert_eq!(lost_since(&b, "sa", 0), ["t1"], "A's claim lost; its later done is ignored");
            // No two tenures of one task overlap.
            let spans: Vec<&Span> = b.spans.iter().filter(|s| s.task == "t1").collect();
            for (i, x) in spans.iter().enumerate() {
                for y in &spans[i + 1..] {
                    assert!(x.to_ms.is_some_and(|t| t <= y.from_ms) || y.to_ms.is_some_and(|t| t <= x.from_ms), "{x:?} overlaps {y:?}");
                }
            }
        }
    }

    /// One wave of the Monte Carlo below: `m` work tasks (random ids) an engineer opened on node
    /// `opener`, a drone line from each of A and B (their runners are warm), then `k` idle drones on each
    /// of A and B pick at once, each node's drones one after another (a local claim is seen at once, a
    /// remote one only after a sync). The picks, per node.
    fn wave(trial: usize, k: usize, m: usize, opener: &str) -> [Vec<TaskId>; 2] {
        let id = |i: usize| blake3::hash(format!("{trial}/{m}/{i}").as_bytes()).to_hex()[..8].to_string();
        let mut base: Vec<Event> = (0..m).map(|i| on(Event::open(task(&id(i)), "engineer:e", i as i64), opener)).collect();
        base.extend(["A", "B"].map(|n| on(ev(Ev::Progress, "warm", &format!("s{n}"), 0), n)));
        ["A", "B"].map(|node| {
            let mut lines = base.clone();
            let mut picks = Vec::new();
            for d in 1..=k {
                let (session, drone) = (format!("{node}{d}"), format!("d{d}"));
                let Some(t) = next_for(&fold(&lines), &session, &drone, node, "", 100, |_| true).map(|s| s.task.id.clone()) else { break };
                lines.push(on(ev(Ev::Claim, &t, &session, 100).with_drone(&drone), node));
                picks.push(t);
            }
            picks
        })
    }

    #[test]
    fn drones_on_two_nodes_rarely_pick_the_same_task() {
        // Pre-registered (todo.md Phase e, E2): 2 nodes x {1,2,3} drones, m in {3,6,12}: no task picked
        // on both nodes while each node's share lasts (before node-affine choice: always one), and ≤ 1/m
        // such tasks per wave overall beyond the pigeonhole floor max(0, 2k - m). A task both nodes pick
        // at any index is a lost claim, so the count is over the two sets of picks, not pick by pick. The
        // engineer opens on A (a node with drones) or on E (a node without: an engineer's line gives no
        // share).
        const TRIALS: usize = 300;
        let live = BTreeSet::from(["A", "B"]);
        for opener in ["A", "E"] {
            for k in 1..=3_usize {
                for m in [3, 6, 12] {
                    let floor = (2 * k).saturating_sub(m);
                    let (mut both, mut excess, mut same, mut shares, mut first) = (0, 0, 0, 0, 0);
                    for trial in 0..TRIALS {
                        let [a, b] = wave(trial, k, m, opener);
                        let common = a.iter().filter(|t| b.contains(t)).count();
                        both += common;
                        excess += common - floor;
                        first += usize::from(a.first() == b.first());
                        same += a.iter().zip(&b).filter(|(x, y)| x == y).count();
                        let ids: Vec<String> = (0..m).map(|i| blake3::hash(format!("{trial}/{m}/{i}").as_bytes()).to_hex()[..8].to_string()).collect();
                        let owned = |n: &str| ids.iter().filter(|t| owner(&live, t) == Some(n)).count();
                        if owned("A") >= k && owned("B") >= k {
                            shares += 1;
                            assert_eq!(common, 0, "opener {opener} k={k} m={m} trial {trial}: both shares last, yet {a:?} and {b:?} overlap");
                        }
                    }
                    let per = excess as f64 / TRIALS as f64;
                    eprintln!(
                        "opener {opener} k={k} m={m}: picked on both {:.3}/wave (floor {floor}), same first pick {:.3}, same dN pick {:.3}, both shares last in {shares}/{TRIALS}",
                        both as f64 / TRIALS as f64,
                        first as f64 / TRIALS as f64,
                        same as f64 / (TRIALS * k) as f64
                    );
                    assert!(per <= 1.0 / m as f64, "opener {opener} k={k} m={m}: {per:.3} tasks per wave on both nodes beyond the floor > 1/{m}");
                }
            }
        }
    }

    #[test]
    fn drones_of_one_node_reading_one_board_pick_apart() {
        // Phase e E2b: k drones of one node read the same board at once (none sees another's claim
        // yet): each picks another task while there are enough, and the same one it would pick
        // seeing the others' claims first. On two live nodes they stay within their node's share.
        for nodes in [vec!["A"], vec!["A", "B"]] {
            for m in [2, 3, 6, 12] {
                for trial in 0..50 {
                    let id = |i: usize| blake3::hash(format!("{trial}/{m}/{i}").as_bytes()).to_hex()[..8].to_string();
                    let mut lines: Vec<Event> = (0..m).map(|i| on(Event::open(task(&id(i)), "engineer:e", i as i64), "A")).collect();
                    lines.extend(nodes.iter().map(|n| on(ev(Ev::Progress, "warm", &format!("s{n}"), 0), n)));
                    let live: BTreeSet<&str> = nodes.iter().copied().collect();
                    let share = (0..m).filter(|&i| owner(&live, &id(i)) == Some("A")).count();
                    let b = fold(&lines);
                    let at_once: Vec<TaskId> =
                        (1..=3).map_while(|d| next_for(&b, &format!("A{d}"), &format!("d{d}"), "A", "", 100, |_| true).map(|s| s.task.id.clone())).collect();
                    let distinct: BTreeSet<&TaskId> = at_once.iter().collect();
                    // Past the share they spill one after another into the far end: apart while there are tasks.
                    assert!(distinct.len() >= 3.min(m), "{nodes:?} m={m} share={share} trial {trial}: {at_once:?}");
                    let mut seen = Vec::new();
                    for d in 1..=3 {
                        let Some(t) = next_for(&fold(&lines), &format!("A{d}"), &format!("d{d}"), "A", "", 100, |_| true).map(|s| s.task.id.clone()) else {
                            break;
                        };
                        lines.push(on(ev(Ev::Claim, &t, &format!("A{d}"), 100).with_drone(&format!("d{d}")), "A"));
                        seen.push(t);
                    }
                    if share >= 3 {
                        assert!(at_once.iter().all(|t| owner(&live, t) == Some("A")), "{nodes:?} m={m} trial {trial}: {at_once:?} leaves A's share");
                        assert_eq!(at_once, seen, "{nodes:?} m={m} trial {trial}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_drone_before_that_may_not_claim_a_task_leaves_it_to_the_next() {
        // One node, both drones idle at once (their works done in one instant): t1.v1 judges d1's work,
        // barred to d1; t2.v1 is newer. d1 takes t2.v1, the first it may claim, so d2 takes t1.v1, not
        // t2.v1 too (a walk from d2's place landed on it: a lost claim). Seeing d1's claim first, d2
        // picks the same.
        let lines = [worked("t1", "sa", "d1", 1), worked("t2", "sb", "", 2)].concat();
        let pick = |lines: &[Event], d: usize| next_for(&fold(lines), &format!("A{d}"), &format!("d{d}"), "", "", 5, |_| true).map(|s| s.task.id.clone());
        assert_eq!((pick(&lines, 1), pick(&lines, 2)), (Some("t2.v1".into()), Some("t1.v1".into())));
        let seen = [lines.clone(), vec![ev(Ev::Claim, "t2.v1", "A1", 3).with_drone("d1")]].concat();
        assert_eq!(pick(&seen, 2), Some("t1.v1".into()));
    }

    #[test]
    fn a_runner_leaves_the_task_an_idle_drone_before_it_takes() {
        // One task, d1 and d2 idle, woken by one change of the board: d2's runner starts no session to
        // race d1's for it, the work is in hand; a session of d2 that runs still gets it; once d1 holds
        // something else, d2 may take it.
        let lines = vec![Event::open(task("t1"), "engineer:e", 1)];
        let b = fold(&lines);
        assert_eq!(free_for(&b, "", "d1", "", "", 5, |_| true).map(|s| s.task.id.as_str()), Some("t1"));
        assert!(!taken_before(&b, "d1", "", "", 5, |_| true));
        assert_eq!(free_for(&b, "", "d2", "", "", 5, |_| true).map(|s| s.task.id.as_str()), None);
        assert!(taken_before(&b, "d2", "", "", 5, |_| true));
        assert_eq!(next_for(&b, "A2", "d2", "", "", 5, |_| true).map(|s| s.task.id.as_str()), Some("t1"));
        let busy = [lines.clone(), vec![Event::open(task("t0"), "engineer:e", 0), ev(Ev::Claim, "t0", "A1", 2).with_drone("d1")]].concat();
        assert_eq!(free_for(&fold(&busy), "", "d2", "", "", 5, |_| true).map(|s| s.task.id.as_str()), Some("t1"));
        // A task d1 may not claim is d2's at once.
        let b = fold(&worked("w", "sx", "d1", 1));
        assert_eq!(free_for(&b, "", "d2", "", "", 5, |_| true).map(|s| s.task.id.as_str()), Some("w.v1"));
    }

    #[test]
    fn a_busy_drone_takes_no_place_in_its_nodes_share() {
        // A owns one work task; d1 holds a plan task (another class). d2 is the only idle drone: it
        // takes A's task, not B's far end, which B's own drones take.
        let live = BTreeSet::from(["A", "B"]);
        let a = (0..).map(|i| format!("a{i}")).find(|t| owner(&live, t) == Some("A")).unwrap();
        let bs: Vec<String> = (0..).map(|i| format!("b{i}")).filter(|t| owner(&live, t) == Some("B")).take(2).collect();
        let plan = Task { role: Role::Plan, ..task("p") };
        let mut lines = vec![on(Event::open(plan, "engineer:e", 0), "A"), on(Event::open(task(&a), "engineer:e", 0), "A")];
        lines.extend(bs.iter().map(|t| on(Event::open(task(t), "engineer:e", 0), "A")));
        lines.extend([on(ev(Ev::Claim, "p", "A1", 1).with_drone("d1"), "A"), on(ev(Ev::Progress, "warm", "sB", 1), "B")]);
        assert_eq!(next_for(&fold(&lines), "A2", "d2", "A", "", 2, |_| true).map(|s| s.task.id.clone()), Some(a));
    }

    #[test]
    fn a_busy_drone_of_the_same_class_is_counted_once() {
        // One node, t1..t5 open, d1 holds t1. d2 and d3 pick from the same board, or d2 before d1's claim
        // reached it: each time two different tasks, the same ones either way.
        let opens: Vec<Event> = (1..=5).map(|i| on(Event::open(task(&format!("t{i}")), "engineer:e", i), "A")).collect();
        let claim = on(ev(Ev::Claim, "t1", "A1", 10).with_drone("d1"), "A");
        let pick = |lines: &[Event], d: usize| next_for(&fold(lines), &format!("A{d}"), &format!("d{d}"), "A", "", 20, |_| true).map(|s| s.task.id.clone());
        let seen: Vec<Event> = opens.iter().cloned().chain([claim]).collect();
        assert_eq!((pick(&seen, 2), pick(&seen, 3)), (Some("t2".into()), Some("t3".into())), "both see d1's claim");
        assert_eq!((pick(&opens, 2), pick(&seen, 3)), (Some("t2".into()), Some("t3".into())), "d2 does not see it yet, d3 does");
    }

    #[test]
    fn an_ended_drone_takes_no_place() {
        // The runner's newest line names d2 alone (d1 and d3 ended): d2 takes the oldest task, not the
        // second; with no such line every drone before it counts.
        let mut lines: Vec<Event> = (1..=3).map(|i| on(Event::open(task(&format!("t{i}")), "engineer:e", i), "A")).collect();
        let pick = |lines: &[Event]| next_for(&fold(lines), "A2", "d2", "A", "", 20, |_| true).map(|s| s.task.id.clone());
        assert_eq!(pick(&lines), Some("t2".into()));
        let all = ["d1", "d2", "d3"].map(String::from);
        lines.push(on(Event::new(Ev::Heartbeat, "", "runner", 5).with_note(&roster_note(&all)), "A"));
        assert_eq!(pick(&lines), Some("t2".into()), "all three run");
        lines.push(on(Event::new(Ev::Heartbeat, "", "runner", 8).with_note(&roster_note(&all[1..2])), "A"));
        assert_eq!(pick(&lines), Some("t1".into()), "d1 and d3 ended");
        assert_eq!(fold(&lines).rosters["A"], (8, BTreeSet::from(["d2".to_string()])));
    }

    #[test]
    fn another_nodes_task_waits_for_its_idle_owner_then_spills() {
        // d3-accept2 w2-s2: B's runner runs d1, idle; B owns b1 and b2. A's idle d1 is not offered the
        // oldest, which B's d1 takes, but the one past B's idle drones; with B's d1 busy, both; and
        // after the grace, whatever B's drones did.
        let live = BTreeSet::from(["A", "B"]);
        let bs: Vec<String> = (0..).map(|i| format!("b{i}")).filter(|t| owner(&live, t) == Some("B")).take(2).collect();
        let mut lines = vec![on(Event::open(task(&bs[0]), "engineer:e", 0), "A"), on(Event::open(task(&bs[1]), "engineer:e", 1), "A")];
        lines.push(on(Event::new(Ev::Heartbeat, "", "runner", 1).with_note(&roster_note(&["d1".to_string()])), "B"));
        let pick = |lines: &[Event], now: i64| next_for(&fold(lines), "A1", "d1", "A", "", now, |_| true).map(|s| s.task.id.clone());
        assert_eq!(pick(&lines[..1].iter().chain(&lines[2..]).cloned().collect::<Vec<_>>(), 5), None, "B's idle d1 takes its one task");
        assert_eq!(pick(&lines, 5), Some(bs[1].clone()), "B has one idle drone for two tasks: the second spills");
        // A's d2 took the second: the oldest is left to B's d1 within the grace, after it whoever is idle.
        let taken = [lines.clone(), vec![on(ev(Ev::Claim, &bs[1], "A2", 2).with_drone("d2"), "A")]].concat();
        assert_eq!(pick(&taken, OWNER_GRACE_MS - 1), None, "within the grace");
        assert_eq!(pick(&taken, OWNER_GRACE_MS), Some(bs[0].clone()), "B did not take it");
        // B's d1 started a session that has not claimed yet: the model claims inside it, seconds to tens
        // of them later, so the task is still B's past the grace; its claim of another task or its end
        // without one hands it on.
        let solo: Vec<Event> = lines[..1].iter().chain(&lines[2..]).cloned().collect();
        let started = [solo.clone(), vec![on(Event::new(Ev::Heartbeat, "", "runner", 2).with_note(&session_note("d1", true)), "B")]].concat();
        assert_eq!(pick(&started, 10 * OWNER_GRACE_MS), None, "B's session is on its way to it");
        let ended = [started.clone(), vec![on(Event::new(Ev::Heartbeat, "", "runner", 9).with_note(&session_note("d1", false)), "B")]].concat();
        assert_eq!(pick(&ended, 10 * OWNER_GRACE_MS), Some(bs[0].clone()), "it ended without a claim");
        let lost = [started, vec![on(ev(Ev::Claim, "elsewhere", "B9", 9).with_drone("d1"), "B")]].concat();
        assert_eq!(pick(&lost, 10 * OWNER_GRACE_MS), Some(bs[0].clone()), "it claimed something else");
        assert_eq!(fold(&lost).sessions[&("B".to_string(), "d1".to_string())], (2, 9));
        // B's d1 busy on a plan task: B has no idle drone, both spill.
        let plan = Task { role: Role::Plan, ..task("p") };
        lines.extend([on(Event::open(plan, "engineer:e", 0), "B"), on(ev(Ev::Claim, "p", "B1", 2).with_drone("d1"), "B")]);
        assert_eq!(pick(&lines, 5), Some(bs[1].clone()));
        lines.push(on(ev(Ev::Claim, &bs[1], "A2", 3).with_drone("d2"), "A"));
        assert_eq!(pick(&lines, 5), Some(bs[0].clone()), "the owner's oldest too while its drones are busy");
        // A node without a runner's roster keeps nothing back.
        let bare = [on(Event::open(task(&bs[0]), "engineer:e", 0), "A"), on(ev(Ev::Progress, "warm", "sB", 1), "B")];
        assert_eq!(pick(&bare, 5), Some(bs[0].clone()));
    }

    #[test]
    fn a_task_left_to_a_peers_starting_session_is_in_hand() {
        // d3-accept3 W2: B's d1 started a session that had not claimed 12 s on; A's idle d1 saw nothing
        // open and nothing in hand, and ended before B's claim. The task is not A's to take, and in hand.
        let live = BTreeSet::from(["A", "B"]);
        let t: String = (0..).map(|i| format!("b{i}")).find(|t| owner(&live, t) == Some("B")).unwrap();
        let roster = on(Event::new(Ev::Heartbeat, "", "runner", 1).with_note(&roster_note(&["d1".to_string()])), "B");
        let start = on(Event::new(Ev::Heartbeat, "", "runner", 2).with_note(&session_note("d1", true)), "B");
        let opened = vec![on(Event::open(task(&t), "engineer:e", 0), "A"), roster];
        let b = fold(&opened);
        assert!(in_hand(&b, "d1", "A", "", 3, |_| true), "B's runner has OWNER_GRACE_MS to start a session for it");
        assert!(!in_hand(&b, "d1", "A", "", 3, |s| s.task.id != t), "not when A may not claim it");
        let started = [opened, vec![start]].concat();
        let at = 10 * OWNER_GRACE_MS;
        let b = fold(&started);
        assert_eq!(free_for(&b, "", "d1", "A", "", at, |_| true).map(|s| s.task.id.clone()), None, "left to B's session");
        assert!(!b.in_flight(at) && in_hand(&b, "d1", "A", "", at, |_| true), "in hand while B's session is on its way to it");
        let ended = [started, vec![on(Event::new(Ev::Heartbeat, "", "runner", 9).with_note(&session_note("d1", false)), "B")]].concat();
        let b = fold(&ended);
        assert_eq!(free_for(&b, "", "d1", "A", "", at, |_| true).map(|s| s.task.id.clone()), Some(t), "it ended without a claim");
        assert!(!in_hand(&b, "d1", "A", "", at, |_| true));
    }

    #[test]
    fn a_peers_starting_session_is_in_hand_for_a_drone_barred_from_its_task() {
        // The first hive_mesh run: B's d1 authored w and may not verify it; A's session started on w.v1 and
        // had not claimed. Nothing is B's to claim, held or left to A, yet its verdict may open a rework.
        let mut lines = worked("w", "sB", "d1", 2).into_iter().map(|e| on(e, "B")).collect::<Vec<_>>();
        lines.push(on(Event::new(Ev::Heartbeat, "", "runner", 3).with_note(&roster_note(&["d1".to_string()])), "A"));
        lines.push(on(Event::new(Ev::Heartbeat, "", "runner", 4).with_note(&session_note("d1", true)), "A"));
        let at = 10 * OWNER_GRACE_MS;
        let b = fold(&lines);
        assert_eq!(free_for(&b, "", "d1", "B", "", at, |_| true).map(|s| s.task.id.clone()), None, "B's d1 may not verify its own work");
        assert!(!b.in_flight(at) && in_hand(&b, "d1", "B", "", at, |_| true), "A's session is about to hold w.v1");
        lines.push(on(Event::new(Ev::Heartbeat, "", "runner", 9).with_note(&session_note("d1", false)), "A"));
        assert!(!in_hand(&fold(&lines), "d1", "B", "", at, |_| true), "A's session ended without a claim");
    }

    #[test]
    fn a_task_the_owners_idle_drones_may_not_claim_does_not_count_ahead() {
        // B's only drone d1 authored w: B owns w.v1, barred to d1, and the work task b. A's d1 takes
        // w.v1; b is left to B's d1, which it may claim, not raced for by another of A's drones.
        let live = BTreeSet::from(["A", "B"]);
        let w = (0..).map(|i| format!("w{i}")).find(|t| owner(&live, &format!("{t}.v1")) == Some("B")).unwrap();
        let b = (0..).map(|i| format!("b{i}")).find(|t| owner(&live, t) == Some("B")).unwrap();
        let mut v = verify(&w, "sB", 2);
        v.not_by.push("d1".into());
        let lines = vec![
            on(Event::open(task(&w), "engineer:e", 0), "A"),
            on(Event::open(task(&b), "engineer:e", 0), "A"),
            on(Event::new(Ev::Heartbeat, "", "runner", 0).with_note(&roster_note(&["d1".to_string()])), "B"),
            on(ev(Ev::Claim, &w, "sB", 1).with_drone("d1"), "B"),
            on(ev(Ev::Done, &w, "sB", 2), "B"),
            on(Event::open(v, "sB", 2), "B"),
        ];
        let pick = |allowed: fn(&TaskState) -> bool| next_for(&fold(&lines), "A1", "d1", "A", "", 5, allowed).map(|s| s.task.id.clone());
        assert_eq!(pick(|_| true), Some(format!("{w}.v1")), "B's d1 may not verify its own work");
        assert_eq!(pick(|s| s.task.role != Role::Verify), None, "b is B's d1's");
    }

    #[test]
    fn one_node_takes_the_oldest_first_and_two_keep_the_role_order() {
        // One node owns every task: the order of before, oldest then id, drone after drone.
        let lines: Vec<Event> = ["c", "a", "b"].iter().enumerate().map(|(i, t)| on(Event::open(task(t), "engineer:e", 5 - i as i64), "A")).collect();
        let mut lines = [lines, vec![on(Event::open(task("d"), "engineer:e", 3), "A")]].concat();
        let mut picks = Vec::new();
        while let Some(t) = next_for(&fold(&lines), "s", "", "A", "", 100, |_| true).map(|s| s.task.id.clone()) {
            lines.push(on(ev(Ev::Claim, &t, &format!("s{t}"), 100), "A"));
            picks.push(t);
        }
        assert_eq!(picks, ["b", "d", "a", "c"]);
        // A node that wrote nothing within a lease takes no share: B's line is older than LEASE_MS. Nor
        // does one whose only lines are an engineer's (E): it may run no drone.
        let b = fold(&[on(Event::open(task("x"), "engineer:e", 0), "E"), on(ev(Ev::Progress, "x", "sb", 0), "B")]);
        assert_eq!(b.live_nodes("A", LEASE_MS + 1), BTreeSet::from(["A"]));
        assert_eq!(b.live_nodes("A", LEASE_MS), BTreeSet::from(["A", "B"]));
        // Across two nodes a verify task still goes before work, whichever node owns which.
        let live = BTreeSet::from(["A", "B"]);
        let id = (0..).map(|i| format!("w{i}")).find(|t| owner(&live, t) == Some("A")).unwrap();
        let v = (0..).map(|i| format!("v{i}")).find(|t| owner(&live, &format!("{t}.v1")) == Some("B")).unwrap();
        let lines = vec![
            on(Event::open(task(&id), "engineer:e", 0), "A"),
            on(Event::open(task(&v), "engineer:e", 0), "B"),
            on(ev(Ev::Claim, &v, "sb", 1), "B"),
            on(ev(Ev::Done, &v, "sb", 2), "B"),
            on(Event::open(verify(&v, "sb", 2), "sb", 2), "B"),
        ];
        assert_eq!(next_for(&fold(&lines), "sa", "", "A", "", 3, |_| true).map(|s| s.task.id.clone()), Some(format!("{v}.v1")));
    }
}
