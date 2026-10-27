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
//! Across nodes a claim is provisional until it is final (phase f F5): with other goal participants,
//! `claim` (mod.rs `across`) waits until each one's watermark is past the claim's stamp
//! (`Board::unconfirmed`, `Event::seq`/`marks`), so no rival's claim that sorts before it can still
//! arrive from them, and a loser learns before any work; only a suspected participant is not waited
//! for. Results are monotone: a done or verdict written under the claim its writer's fold gave it
//! (`Event::claim`) is kept for good whatever claim arrives later (`Fold::permanent`; a work task's
//! attempts are a G-set, `TaskState::attempts`, the accepted one chosen at the integration), so a
//! claim that proceeded on a suspicion costs at most a duplicate attempt. Stamps are a hybrid logical clock taken under the board lock (`stamp`): a line written after
//! another was folded on its node sorts after it everywhere, so the winner rule compares clocks only
//! between claims neither writer had seen. Every age the scheduling reads (a lease, a node's liveness,
//! the owner's grace) is timed on the reader's own clock from when it first held the line (`Local`,
//! `held_path`), never `now` against another node's stamp: a skew between nodes moves none of them.
//! Identities are (node, name) (phase f F8, `Actor`): drone names (`d1`..) repeat on every node and a
//! session id says who only with its node, so a holder, an author and a bar are each a name on a node
//! (`Board::barred`). Whether an actor may take a task is one predicate, [`eligible`], which the fold's
//! claim rule, the scheduling (`claimable_by`, `next_for`, `free_for`, `open_for`), the pre-write check
//! (hive/mod.rs `claimable_for`) and the verdict's author check (`judges_own`) all read.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::goal::Check;
use super::{LEASE_MS, OWNER_GRACE_MS, SUSPECT_MS, TaskId};

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
    /// On a claim that takes over a lease this node saw end with its holder's node (`Local::gone`, phase
    /// f F4): the stamp of the holder's last renewal it held. The fold takes the claim within the lease
    /// only while that is still the holder's last renewal before it (`apply`): a renewal the claimant had
    /// not held, a sign the holder was alive, refuses it. None on any other line, and on a line from before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub takes: Option<i64>,
    /// How many lines of its node its node's file held before the write it landed in (phase f F5, mod.rs
    /// `land`): with its stamp, how far a peer may read this node's lines as complete (`Board::watermarks`).
    /// None on a line from before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// On a runner's join or beat (phase f F5, `landing`): for each other node, the newest stamp of its
    /// lines its node's file held then. Every later line of this node is stamped above it, so once a peer
    /// holds this line and every one before it, no line of this node that sorts at or below it is missing
    /// there (`Board::watermarks`). Empty on any other line, and on a line from before.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub marks: BTreeMap<String, i64>,
    /// On a done or a verdict (phase f F5): the stamp of the writer's claim it was written under, which the
    /// writer's fold then gave it. Such a line is a permanent attempt or verdict (`apply`): no line that
    /// arrives later voids it. None on a line from before, which counts only from the fold's holder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<i64>,
    /// When this node first held the line, on its own clock: local, never written or synced (`read`
    /// fills it from the board's receive index, [`held_path`]).
    #[serde(skip)]
    pub seen: Seen,
}

/// When a node first held a line, on that node's clock (`Event::seen`); None for a line built in memory,
/// whose stamp stands in for it (`read` gives one past the receive index the time of the read). Never part of a line's identity: two copies
/// of one line are equal wherever and whenever they were seen.
#[derive(Debug, Clone, Copy, Default)]
pub struct Seen(pub Option<i64>);

impl PartialEq for Seen {
    fn eq(&self, _: &Seen) -> bool {
        true
    }
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
            takes: None,
            seq: None,
            marks: BTreeMap::new(),
            claim: None,
            seen: Seen(None),
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
    Done { by: String, node: String, ms: i64 },
    Verified { by: String, node: String, ms: i64 },
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
    /// Of a work task, every done the fold took, each for good (phase f F5: a G-set); `accepted_attempt`
    /// names the one accepted, (author, done stamp): the first verified in the fold's order, or the one
    /// the newest integration merged (`Integration::merged`). Of a verify task, `of_attempt` names the
    /// attempt its open was written with, the one it judges unless a verdict names another (`Event::head`).
    pub attempts: Vec<Attempt>,
    pub accepted_attempt: Option<(String, i64)>,
    pub of_attempt: Option<(String, i64)>,
    /// The node its `open` was written on (empty: this one, or a line from before nodes were stamped).
    /// A verify task's open is written with its work's `done`: the work's node.
    pub node: String,
    /// When this node held the lines behind its stamps, on its own clock (`Local`).
    pub local: Local,
}

/// When this node first held the lines behind a task's state, on its own clock (`Event::seen`): what
/// every age the scheduling reads is measured on (`live`, `claimable`, `claimable_since`), never `now`
/// against another node's stamp, so a peer's clock offset moves no lease, liveness or grace (Gray &
/// Cheriton 1989: each party times a lease on its own clock; Kubernetes leader election: the local
/// time at which the record was seen to change). Local, as `Board::nodes`: the fold's rules read the
/// stamps alone, so every node holding the same lines folds the same tasks.
#[derive(Debug, Clone, Copy)]
pub struct Local {
    /// Its open.
    pub opened: i64,
    /// The line that last renewed its holder's lease (its claim or a heartbeat).
    pub renewed: i64,
    /// Its done or verdict (`Status::Done` / `Verified`).
    pub status: i64,
    /// The end of its last tenure (an expired one at its last renewal).
    pub ended: i64,
    /// For a claim of a runner's drone on another node that joined the goal (`Board::members`): when this
    /// node last held a line of the holder's node (`Board::nodes`); `i64::MAX` when no membership speaks
    /// for the holder (a session of its own, a node from before F4, this node itself: `fold_on`), whose
    /// lease alone times it.
    pub heard: i64,
    /// When this node held the line that ended the holder's membership after its claim: its node's leave,
    /// or a later join (a new run of that node: the one before, and its sessions, are gone); `i64::MAX`
    /// while none did.
    pub left: i64,
}

/// One attempt of a work task (phase f F5): a done the fold took, from its writer's claim (`claim`, its
/// stamp) to the done (`done`), the tree it left (`head`), and whether a verdict passed it. A done written
/// under a claim its writer's fold gave it is one for good, whatever claim later arrives (S2': at-least-once
/// work, at-most-once acceptance; Helland, "Life beyond Distributed Transactions").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub by: String,
    pub node: String,
    pub claim: i64,
    pub done: i64,
    pub head: String,
    pub verified: bool,
}

/// No part of what the lines say (`Board`'s equality): equal wherever and whenever they were held.
impl PartialEq for Local {
    fn eq(&self, _: &Local) -> bool {
        true
    }
}

/// Nothing held yet: every time before any other.
impl Default for Local {
    fn default() -> Local {
        Local { opened: i64::MIN, renewed: i64::MIN, status: i64::MIN, ended: i64::MIN, heard: i64::MAX, left: i64::MAX }
    }
}

impl Local {
    /// Whether the holder's node is gone at `now`, this node's clock (phase f F4): it left the goal or
    /// ran again since the claim, or this node held no new line of it for `SUSPECT_MS` (a failure
    /// detector in seconds, on the observer's own clock: SWIM, phi-accrual; Kubernetes' lease on the
    /// time the record was seen to change). Never for a holder no membership speaks for, and never by
    /// silence for one of the observer's own node (`fold_on`).
    pub fn gone(&self, now: i64) -> bool {
        now >= self.gone_since()
    }

    /// When the holder's node is gone from (`gone`); `i64::MAX` while it is not.
    fn gone_since(&self) -> i64 {
        self.left.min(self.heard.saturating_add(SUSPECT_MS + 1))
    }
}

/// Whether two lines' nodes may be one machine, for a bar only (`Board::barred`, `rival`): equal, or
/// either unknown (a line from before nodes were stamped), which bars conservatively: a bar too many
/// costs a wait, never a result. Never an identity (phase f F8): who holds, wrote or is asked about a
/// line is a name on a node (`Actor`), and an unknown node is no node in particular there.
pub fn same_node(a: &str, b: &str) -> bool {
    a == b || a.is_empty() || b.is_empty()
}

/// Whether a line written on `line` is `here`'s own, as `here` asks of its sessions and drones: its id,
/// or unknown, a line from before nodes were stamped, which only this node can have written (the logs
/// synced only once every line named its node, hive.md C4) (hive/mod.rs `is_local`). An empty `here`
/// owns only unknown lines: a stamped line is never anyone's by accident.
pub fn of_node(line: &str, here: &str) -> bool {
    line == here || line.is_empty()
}

/// Who takes, holds or judges a task (phase f F8): a session, of a runner's drone (empty: a session of
/// its own), on a node. A name says who only with its node: every runner names its drones d1..dN, and
/// two nodes' sessions may share a name (an integrator's before it named its node, a harness's own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actor<'a> {
    pub node: &'a str,
    pub drone: &'a str,
    pub session: &'a str,
}

impl<'a> Actor<'a> {
    pub fn new(node: &'a str, drone: &'a str, session: &'a str) -> Actor<'a> {
        Actor { node, drone, session }
    }

    /// The writer of `e`: its node, drone and session (`by`).
    pub fn of(e: &'a Event) -> Actor<'a> {
        Actor { node: &e.node, drone: &e.drone, session: &e.by }
    }

    /// Whether this is the session `by` of `node` (a line's writer, a holder): the same name on the same
    /// node. Both are lines' nodes here (the fold's holder checks), and an unknown one, a line from
    /// before nodes were stamped, is any: such a line was written on one node before the logs synced,
    /// so a claim it made is renewed, finished or released by that session's stamped lines after an
    /// upgrade. Two stamped lines of two nodes are never one session.
    pub fn is(&self, node: &str, by: &str) -> bool {
        self.session == by && (self.node == node || self.node.is_empty() || node.is_empty())
    }
}

/// When a claim is judged (`eligible`): as the fold takes a claim line stamped `ms` naming the renewal
/// `takes` it takes over (`Event::takes`), on stamps alone; or `Now`, on this node's clock, as every
/// scheduling decision reads leases (`TaskState::claimable`).
#[derive(Debug, Clone, Copy)]
pub enum At {
    Line { ms: i64, takes: Option<i64> },
    Now(i64),
}

/// Whether `who` may claim `s` on `b` `at` (phase f F8): the task is open there (open, or its holder's
/// lease ended: by stamps for a line, `TaskState::claimable_at` or a takeover naming the holder's last
/// renewal; on this node's clock for a decision), its deps are verified (a verify task: its work done),
/// and `who` is not barred (`Board::barred`). The one definition of "may X take t": the fold takes a
/// claim line exactly when it says so of its writer (`apply`, but for the holder's own renewal), and
/// every decision before a claim reads it (`claimable_by`, `next_for`/`free_for` through `open_for`,
/// hive/mod.rs `claimable_for`, run.rs `Offer`, `stuck`, `left_to_owner`) or the bar it ends with
/// (verdict.rs `record` through `judges_own`), adding only what narrows it (approvals, a repo, commits,
/// an owner's share), so none can offer a task the fold refuses or refuse one it takes (G6, the drift
/// behind #4, #20, #23 and #29). No timer decides it but the lease's, read by `At`.
pub fn eligible(b: &Board, s: &TaskState, who: Actor, at: At) -> bool {
    let open = match at {
        At::Line { ms, takes } => s.claimable_at(ms) || matches!(s.status, Status::Claimed { renewed, .. } if takes == Some(renewed) && ms > renewed),
        At::Now(now) => s.claimable(now),
    };
    open && deps_met(&b.tasks, &s.task) && !b.barred(s, who)
}

/// Verifier notes kept per task.
pub const MAX_NOTES: usize = 5;

impl TaskState {
    /// The live holder at `now`, this node's clock: (session, node, since), None when the task is not
    /// claimed, no renewal of its lease was seen here within `LEASE_MS`, or the holder's node is gone
    /// (`Local::gone`, phase f F4: a runner's drone dies with its runner) (then it is open to claim; the
    /// log is not rewritten). The lease is timed from when this node held the renewal (`Local`), so a
    /// holder's clock ahead or behind this one's neither stretches nor cuts it. A session hung on a node
    /// that still beats keeps its claim for the whole lease.
    pub fn live(&self, now: i64) -> Option<(&str, &str, i64)> {
        match &self.status {
            Status::Claimed { by, node, since, .. } if now - self.local.renewed <= LEASE_MS && !self.local.gone(now) => Some((by, node, *since)),
            _ => None,
        }
    }

    /// Open to a claim at `now`, this node's clock: open, or claimed and no longer `live`.
    pub fn claimable(&self, now: i64) -> bool {
        match &self.status {
            Status::Open => true,
            Status::Claimed { .. } => self.live(now).is_none(),
            _ => false,
        }
    }

    /// The fold's lease rule, on stamps: open, or a rival's claim stamped `ms` is more than `LEASE_MS`
    /// past the holder's last renewal the fold took. Only a takeover reads a stamp gap; a holder's own
    /// heartbeat or claim renews whatever the gap since its last (`apply`): a hybrid stamp jumps when its
    /// node hears a peer's clock ahead (`stamp`, `claim_at`), so a gap between one holder's stamps says
    /// nothing of how long it was silent, and the fold would otherwise end a live holder's lease on
    /// every node for a peer's clock more than `LEASE_MS − HEARTBEAT_MS` ahead. The gap this rule reads,
    /// a rival's stamp past the holder's, only grows when the holder's stamps jump: a jump never breaks
    /// a lease, whatever `AHEAD_MS` is.
    fn claimable_at(&self, ms: i64) -> bool {
        match self.status {
            Status::Open => true,
            Status::Claimed { renewed, .. } => ms - renewed > LEASE_MS,
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

/// An integration that passed: its integrate task, the combined tree's commit and when, and the attempts
/// it merged (`task@head`, phase f F5: the accepted attempt of each work task is chosen there and kept;
/// empty on a line from before).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Integration {
    pub task: TaskId,
    pub head: String,
    pub ms: i64,
    pub merged: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Board {
    pub tasks: BTreeMap<TaskId, TaskState>,
    /// Claims that lost a race or were invalid: (by, node, task, ms).
    pub lost: Vec<(String, String, TaskId, i64)>,
    pub spans: Vec<Span>,
    /// The drone each session claimed as (`Event::drone`): (node, session) -> drone (phase f F8: a
    /// session's name says who only with its node, `Actor`).
    pub drones: BTreeMap<(String, String), String>,
    /// Each node's watermarks (phase f F5): node -> (other node -> stamp). For a node P and another X: the
    /// newest stamp of X's lines P had held when it wrote a beat or join line (`Event::marks`) that is held
    /// here with every line of P before it (`Event::seq`). Every line of P not held here yet was written
    /// after that one, stamped above it, so above X's line stamped there: a claim of X stamped at or
    /// below it has no rival from P still to arrive (`unconfirmed`). Read from what P received alone, never
    /// from P's clock. Only nodes that write `seq` (F5 or later) have an entry.
    pub watermarks: BTreeMap<String, BTreeMap<String, i64>>,
    /// The newest integration that passed.
    pub integrated: Option<Integration>,
    /// Every integration that passed, oldest first.
    pub integrations: Vec<Integration>,
    /// When this node last held a new line a session or the runner of each node wrote, on its own clock
    /// (`Local`), any line the fold took or not, an engineer's not (`by_engineer`): the nodes taking part
    /// (`Board::participants`). Local: a peer's stamp ahead of this clock keeps it live no longer.
    pub nodes: BTreeMap<String, i64>,
    /// Each node's goal membership (phase f F4): the stamps of its runner's newest join and leave lines
    /// (`JOIN`, `LEAVE`; `i64::MIN` for none). A join newer than the leave is a node in the goal.
    pub members: BTreeMap<String, (i64, i64)>,
    /// The drones each node's runner runs, from its newest line naming them (`roster_note`; mod.rs
    /// `announce` at the run's start, then one at each drone's end): node -> (ms, drone names).
    pub rosters: BTreeMap<String, (i64, BTreeSet<String>)>,
    /// Each runner drone's newest session start (`session_note`, run.rs `run_session`, stamped with the start)
    /// and its newest claim or session end: (node, drone) -> (start ms, settled ms). A start newer than
    /// what settled it is a session that has not claimed yet (`left_to_owner`).
    pub sessions: BTreeMap<(String, String), (i64, i64)>,
    /// Each node's newest finished line (phase f F6, `FINISHED`): the integration its board was finished
    /// at, its stamp, and whether every line of its node before it is held here (`Event::seq`, as
    /// `watermarks` reads it). What `settling` waits for.
    pub finishes: BTreeMap<String, Finish>,
    /// The stuck lines that stand (phase f F6, `STUCK`): task -> (stamp, node, why), the newest per task,
    /// while the task is not verified and no node that joined the goal after it is still in it (a new
    /// participant may take it: each node judges it again, `stuck`, and writes it again while it holds;
    /// one that joined and left again took nothing, so the line stands).
    pub stuck: BTreeMap<TaskId, (i64, String, String)>,
    /// The tasks each node said it may not take (phase f F6, `DECLINES`): (node, task) -> the newest such
    /// line's stamp. What another node's `stuck` reads of this node's repo scope and approvals.
    pub declined: BTreeMap<(String, TaskId), i64>,
}

/// A node's newest finished line (`Board::finishes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finish {
    pub ms: i64,
    pub integration: TaskId,
    pub complete: bool,
}

/// Two boards are one when they say the same of the lines: this node's clock (`Board::nodes`, each
/// task's `Local`) is no part of it, so two nodes holding the same lines fold equal boards (S1).
impl PartialEq for Board {
    fn eq(&self, o: &Board) -> bool {
        let Board { tasks, lost, spans, drones, watermarks, integrated, integrations, nodes: _, members, rosters, sessions, finishes, stuck, declined } = self;
        declined == &o.declined
            && (tasks, lost, spans, drones, watermarks, integrated, integrations, members, rosters, sessions, finishes, stuck)
                == (
                    &o.tasks,
                    &o.lost,
                    &o.spans,
                    &o.drones,
                    &o.watermarks,
                    &o.integrated,
                    &o.integrations,
                    &o.members,
                    &o.rosters,
                    &o.sessions,
                    &o.finishes,
                    &o.stuck,
                )
    }
}

/// The note of a runner's line saying `drone`'s session starts or ended (`Board::sessions`):
/// `session-start d1`, `session-end d1`.
pub fn session_note(drone: &str, start: bool) -> String {
    format!("{} {drone}", if start { SESSION_START } else { SESSION_END })
}

const SESSION_START: &str = "session-start";
const SESSION_END: &str = "session-end";

/// The notes of a runner's membership lines (phase f F4; a heartbeat of no task by `runner`, as a
/// roster's): its node joins the goal at the run's start (`JOIN`), says every `BEAT_MS` that it is still
/// in it (`BEAT`, read only as the node's newest line, `Board::nodes`, and its watermarks), and leaves at
/// its end (`LEAVE`). A beat is no board line (phase g, `beats_path`): it lives in the goal's beats store,
/// the newest of each node only, so liveness costs O(1) space per node, never a line per beat.
pub const JOIN: &str = "join";
pub const BEAT: &str = "beat";
pub const LEAVE: &str = "leave";

/// The note of a runner's line saying its node's board was finished at an integrate task (phase f F6,
/// `Board::finishes`): `finished integrate.1`. Written under the board lock while the node's file folds
/// finished at that integration (hive/mod.rs `finish_line`): from then on no session of the node can
/// write a line that changes the board while it stays so (a claim needs a claimable task, a done or a
/// verdict a held one, an integration one due), so a peer holding this line and every line of the node
/// before it holds every change the node can still make from that board.
const FINISHED: &str = "finished";
/// The note of a runner's line saying a task is stuck (phase f F6, `stuck`): `stuck t1.v1 <why>`.
const STUCK: &str = "stuck";
/// The note of a runner's line saying its node may not take a task (phase f F6, `declines_due`):
/// `declines t1`. Its repo scope and its approvals are known on that node alone (mod.rs `may_claim`,
/// oracle.rs `Approvals`), so another node's `stuck` reads them from this line.
const DECLINES: &str = "declines";

/// `FINISHED`'s note for `integration`.
pub fn finished_note(integration: &str) -> String {
    format!("{FINISHED} {integration}")
}

/// `STUCK`'s note for `task`, stuck for `why`.
pub fn stuck_note(task: &str, why: &str) -> String {
    format!("{STUCK} {task} {why}")
}

/// `DECLINES`'s note for `task`.
pub fn decline_note(task: &str) -> String {
    format!("{DECLINES} {task}")
}

/// The note of a runner's line naming the drones it runs (`Board::rosters`): `drones d1 d2 ...`.
pub fn roster_note(drones: &[String]) -> String {
    std::iter::once("drones").chain(drones.iter().map(String::as_str)).collect::<Vec<_>>().join(" ")
}

impl Board {
    /// The drone `session` of `node` claimed as (`drones`), if any: its claims there, else one from
    /// before nodes were stamped (`of_node`).
    pub fn drone(&self, node: &str, session: &str) -> Option<&str> {
        let of = |n: &str| self.drones.get(&(n.to_string(), session.to_string())).map(String::as_str);
        of(node).or_else(|| of(""))
    }

    /// Whether `s` is barred to `who` (its drone empty: none named): by its session, by its drone, or by
    /// the drone its session claimed as before, each a name on the node the task was opened on (phase f
    /// F8: a verify task's open is written with its work's done, so its bars are the author's session and
    /// drone there; another node's session or drone of the same name is another), or a group rival
    /// (`rival`). A line from before nodes were stamped bars on every node (`same_node`). The caller names
    /// the drone: a new iteration's session has no line on the board yet.
    pub fn barred(&self, s: &TaskState, who: Actor) -> bool {
        let named = |n: &str| {
            (!who.session.is_empty() && n == who.session) || (!who.drone.is_empty() && n == who.drone) || self.drone(who.node, who.session) == Some(n)
        };
        (same_node(&s.node, who.node) && s.task.not_by.iter().any(|n| named(n))) || self.rival(s, who)
    }

    /// Whether `who`'s verdict on the verify task `s`, judging the attempt `judged` ((node, session,
    /// drone) of its done), is the author's (verdict.rs `record`, phase f F8): `who` is barred from `s`
    /// (`eligible`'s bar: the author of the attempt its open was written with), or is the judged
    /// attempt's session or drone on its node, when a verdict names another attempt of the round.
    pub fn judges_own(&self, s: &TaskState, who: Actor, judged: (&str, &str, &str)) -> bool {
        let (node, by, drone) = judged;
        self.barred(s, who) || (same_node(node, who.node) && (by == who.session || (!drone.is_empty() && drone == who.drone)))
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
    fn rival(&self, s: &TaskState, who: Actor) -> bool {
        let Some((mine, g)) = self.member_of(&s.task) else { return false };
        let verifying = s.task.role == Role::Verify;
        let rivals = |t: &str| {
            self.tasks
                .get(t)
                .and_then(|o| Some((o.task.role, self.member_of(&o.task)?)))
                .is_some_and(|(role, (m, og))| og == g && m != mine && !(verifying && role == Role::Verify))
        };
        self.spans.iter().filter(|sp| rivals(&sp.task)).any(|sp| {
            same_node(&sp.node, who.node)
                && (sp.by == who.session || self.drone(&sp.node, &sp.by).is_some_and(|d| d == who.drone || self.drone(who.node, who.session) == Some(d)))
        })
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

    /// The goal's participants at `now`, this node's clock (phase f F4): `node` (this one) and each node
    /// whose runner joined and has not left since (`members`) and of which this node held a new line
    /// within `SUSPECT_MS` (`nodes`: its runner beats every `BEAT_MS`, mod.rs `beat`), so a node killed or
    /// cut off drops out in seconds, measured on the observer's clock. Every coordination decision reads
    /// this one set: a task's owner (`owner`, `left_to_owner`), whom a claim's confirm window waits for (mod.rs `across`), whom a finish waits for (`settling`).
    /// A node with no membership line (a runner from before F4) takes part by the lease rule of before: a
    /// line held within `LEASE_MS`. An engineer's lines (opens, releases, verdicts from the terminal) do
    /// not count: a node that only opened the goal may run no drone, and a share of its own would wait
    /// last on every other node.
    pub fn participants<'a>(&'a self, node: &'a str, now: i64) -> BTreeSet<&'a str> {
        self.heard_within(node, now, SUSPECT_MS)
    }

    /// The nodes that may still take a task at `now` (phase f F6, `stuck`): `participants` with `LEASE_MS`
    /// for the suspicion window. A node cut off or suspended is no participant after `SUSPECT_MS` (no
    /// claim waits for it), but it has not left: its drones may take the task once it is heard again, so a
    /// task only it may take is no stuck until it left the goal or was silent for a whole lease. A cut
    /// costs a wait, never a goal given up (with one drone a node, each node's verifier is the other's).
    pub fn takers<'a>(&'a self, node: &'a str, now: i64) -> BTreeSet<&'a str> {
        self.heard_within(node, now, LEASE_MS)
    }

    fn heard_within<'a>(&'a self, node: &'a str, now: i64, window: i64) -> BTreeSet<&'a str> {
        let taking_part = |n: &str, held: i64| match self.members.get(n) {
            Some(&(join, leave)) => join > leave && now.saturating_sub(held) <= window,
            None => now.saturating_sub(held) <= LEASE_MS,
        };
        self.nodes.iter().filter(|(n, held)| !n.is_empty() && taking_part(n, **held)).map(|(n, _)| n.as_str()).chain([node]).collect()
    }

    /// Whether node `n` said in its current run that it may not take `task` (`DECLINES`, since its newest
    /// join): what `stuck` and `left_to_owner` read of another node's repo scope, approvals and commits.
    pub fn declines(&self, n: &str, task: &str) -> bool {
        let joined = self.members.get(n).map_or(i64::MIN, |(join, _)| *join);
        self.declined.get(&(n.to_string(), task.to_string())).is_some_and(|ms| *ms > joined)
    }

    /// Whether `n` is in the goal and writes watermarks (phase f F5): a runner that can say what it held.
    fn speaks(&self, n: &str) -> bool {
        self.members.get(n).is_some_and(|(join, leave)| join > leave) && self.watermarks.contains_key(n)
    }

    /// Whom a finish `here` still waits for at `now` (phase f F6): the other participants that speak
    /// (`speaks`) whose newest finished line (`finishes`) is missing, written before their node's newest
    /// join (another run's), names another integration than the board's newest, or has a line of its node
    /// before it not held here yet. Empty, the finished board is a stable prefix: each participant wrote,
    /// under its lock, that its own board was finished at this integration, and every line it wrote
    /// before that is here, so none of them can still add a line that changes it (`FINISHED`) unless a
    /// line from outside the participants reaches it first (a suspected node's late done, an engineer's
    /// open: termination detection with crashes needs the failure detector, Mittal et al. 2008). A
    /// participant from before F5 is not waited for; one from F5 writes no finished line and is waited for
    /// until it leaves (it ends on its own fold, as before). Empty on a board with no integration.
    pub fn settling<'a>(&'a self, here: &'a str, now: i64) -> Vec<&'a str> {
        let Some(i) = &self.integrated else { return Vec::new() };
        let joined = |n: &str| self.members.get(n).map_or(i64::MIN, |(join, _)| *join);
        let wrote = |n: &str| self.finishes.get(n).is_some_and(|f| f.complete && f.integration == i.task && f.ms > joined(n));
        self.participants(here, now).into_iter().filter(|n| *n != here && self.speaks(n) && !wrote(n)).collect()
    }

    /// The goal is finished on a stable prefix at `now` as `here` reads it (phase f F6): `finished`, and
    /// no participant can still change it (`settling`). The only finish a runner ends on or exits 0 for.
    pub fn finished_for_good(&self, here: &str, now: i64) -> bool {
        self.finished() && self.settling(here, now).is_empty()
    }

    /// Whom a claim `here` stamped `ms` still waits for at `now` before it is final (phase f F5): (the other
    /// participants, `participants`, whose watermark of `here` has not reached `ms` yet; those with none to
    /// read). A claim is final once the first list is empty: every line of those nodes not held here yet
    /// was written once they held a line of `here` stamped at or past it, so it is stamped above it (a
    /// hybrid clock, `watermarks`): no rival's claim that sorts before it can still arrive from them, and
    /// its fold's winner is the winner for good, whatever the nodes' clocks. A participant drops out of both once it is
    /// suspected (`SUSPECT_MS` without a new line held of it): the wait has no cap but the failure
    /// detector. One that writes no watermark (a runner from before F5, or a node with no runner, which no
    /// detector reads within the lease) is in the second list: nothing makes a claim final against it
    /// (hive/mod.rs `across` waits for its mesh round, as before F5; the monotone results make that safe).
    pub fn unconfirmed<'a>(&'a self, here: &'a str, ms: i64, now: i64) -> (Vec<&'a str>, Vec<&'a str>) {
        let others: Vec<&str> = self.participants(here, now).into_iter().filter(|n| *n != here).collect();
        let (speaking, mute): (Vec<&str>, Vec<&str>) = others.into_iter().partition(|n| self.speaks(n));
        let past = |n: &str| self.watermarks.get(n).and_then(|w| w.get(here)).is_some_and(|w| *w >= ms);
        (speaking.into_iter().filter(|n| !past(n)).collect(), mute)
    }

    /// Whether `drone` of `node` has a session that started and has neither claimed nor ended
    /// (`sessions`), started since its node's last join: one of a run before (killed before it wrote
    /// the session's end) is none.
    pub fn starting(&self, node: &str, drone: &str) -> bool {
        let joined = self.members.get(node).map_or(i64::MIN, |(join, _)| *join);
        self.sessions.get(&(node.to_string(), drone.to_string())).is_some_and(|(started, settled)| started > settled && *started > joined)
    }
}

/// The integration `here` is to say its board is finished at (phase f F6, `FINISHED`; hive/mod.rs
/// `finish_line`): the newest, while `b` is finished and `here`'s run has not said so for it since its
/// join. None otherwise.
pub fn finish_due<'a>(b: &'a Board, here: &str) -> Option<&'a str> {
    let i = b.integrated.as_ref().filter(|_| b.finished())?;
    let joined = b.members.get(here).map_or(i64::MIN, |(join, _)| *join);
    let said = b.finishes.get(here).is_some_and(|f| f.integration == i.task && f.ms > joined);
    (!said).then_some(i.task.as_str())
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
    super::append_bytes(path, lines(events)?.as_bytes())
}

/// `append` by a caller that holds the file's lock (hive/mod.rs `write_if`).
pub(crate) fn append_held(path: &Path, events: &[Event]) -> std::io::Result<()> {
    super::append_held(path, lines(events)?.as_bytes())
}

/// `events` as the board's lines, each [`redacted`].
fn lines(events: &[Event]) -> std::io::Result<String> {
    let mut body = String::new();
    for e in events {
        body.push_str(&serde_json::to_string(&redacted(e)).map_err(std::io::Error::other)?);
        body.push('\n');
    }
    Ok(body)
}

/// How far past its own clock a node stamps a line to stay above the lines it holds (`stamp`): a peer's
/// clock ahead by less is absorbed, one further ahead (a broken clock) does not drag this node's stamps
/// along. No rule reads it for safety: at 0 a node's stamps are its clock's, monotone but blind to a
/// peer's ahead (the order across nodes is the writers' clocks', as before hybrid clocks); unbounded it
/// is Kulkarni et al.'s clock, which follows the fastest clock it heard of. Either way a jump it lets a
/// holder's stamps make renews its lease all the same: the fold's renewal reads no gap between one
/// holder's stamps (`TaskState::claimable_at`).
pub const AHEAD_MS: i64 = LEASE_MS;

/// The stamp of a line `node` writes at `wall` (its clock, or an earlier reading of it) onto a board
/// holding `events`: a hybrid logical clock (Kulkarni et al. 2014), taken under the board file's lock
/// (hive/mod.rs `write`), in ms with the logical step folded in as +1. It is above every line of
/// `node`'s, so a node's stamps are strictly monotone in its file whatever its clock does, and above
/// every other line it holds (up to `AHEAD_MS` past `wall`), so a line written after another was folded
/// here sorts after it on every node. One `ms` field, no counter: the fold's order key, (ms, order,
/// rank, line hash), reads it as it read the wall clock, and a log from before folds the same. Lines of
/// one write share their stamp when they share their ms (a done and its verify task's open).
pub fn stamp(events: &[Event], node: &str, wall: i64) -> i64 {
    let (mut own, mut other) = (i64::MIN, i64::MIN);
    for e in events {
        let last = if e.node == node { &mut own } else { &mut other };
        *last = (*last).max(e.ms);
    }
    own.saturating_add(1).max(wall.max(other.saturating_add(1)).min(wall.saturating_add(AHEAD_MS)))
}

/// The lines of one write landing on a board that holds `file` (hive/mod.rs `land`, under the board
/// file's lock), as they land: each stamped (`stamp`) and with `seq` (phase f F5), how many lines of its
/// node are stamped below it then: those in `file` (all of them: a node's stamps rise strictly) and those
/// of this write. Every later line of the node is stamped above every one of these, so that count is fixed
/// for good: a peer that holds that many lines of the node stamped below a line holds every one written
/// before it. A runner's join or beat also carries its marks (`Event::marks`): what it held of each other
/// node (`Board::watermarks`). A beat counts in no `seq` (`is_beat`): beats are kept newest-only
/// (`beats_path`), so no count of them could be held complete, and no beat is a rival of a claim.
pub fn landing(file: &[Event], mut events: Vec<Event>) -> Vec<Event> {
    for e in &mut events {
        e.ms = stamp(file, &e.node, e.ms);
    }
    // Beats count in no `seq` (`is_beat`): only each node's newest is kept anywhere (`beats_path`).
    let own = |node: &str| file.iter().filter(|e| e.node == node && !is_beat(e)).map(line_hash).collect::<BTreeSet<_>>().len();
    let seqs: Vec<Option<u64>> = events
        .iter()
        .map(|e| (!e.node.is_empty()).then(|| (own(&e.node) + events.iter().filter(|o| o.node == e.node && o.ms < e.ms && !is_beat(o)).count()) as u64))
        .collect();
    for (e, seq) in events.iter_mut().zip(seqs) {
        e.seq = seq;
        if e.by == "runner" && e.task.is_empty() && (e.note == JOIN || e.note == BEAT) {
            e.marks = held_marks(file, &e.node);
        }
    }
    events
}

/// For each node but `node` with lines in `file`, the newest stamp of them (`Event::marks`), beats left
/// out: a beat travels in the mesh's live exchange, outside the board's reconciliation (`beats_path`), so
/// holding a node's beat says nothing about holding its lines stamped below it, and a mark past a claim
/// this node never held would make that claim final on its node (`Board::unconfirmed`).
fn held_marks(file: &[Event], node: &str) -> BTreeMap<String, i64> {
    let mut out: BTreeMap<String, i64> = BTreeMap::new();
    for e in file.iter().filter(|e| !e.node.is_empty() && e.node != node && !is_beat(e)) {
        let m = out.entry(e.node.clone()).or_insert(i64::MIN);
        *m = (*m).max(e.ms);
    }
    out
}

/// The newest stamp of a claim another node wrote that `node` holds on `events` beyond what its own beat
/// and join lines marked of that node so far (phase f F5): a beat written now marks it (`Event::marks`),
/// which moves `node`'s watermark past that claim on every peer (`Board::watermarks`), and the claim,
/// waiting for that (`Board::unconfirmed`), is final there a line's way later instead of at `node`'s next
/// beat. None when there is none. What it reads is what `node` received, never a clock.
pub fn to_ack(events: &[Event], node: &str) -> Option<i64> {
    let mut marked: BTreeMap<&str, i64> = BTreeMap::new();
    for e in events.iter().filter(|e| e.node == node) {
        for (x, m) in &e.marks {
            let w = marked.entry(x.as_str()).or_insert(i64::MIN);
            *w = (*w).max(*m);
        }
    }
    events
        .iter()
        .filter(|e| e.ev == Ev::Claim && !e.node.is_empty() && e.node != node && marked.get(e.node.as_str()).is_none_or(|m| e.ms > *m))
        .map(|e| e.ms)
        .max()
}

/// The least stamp a claim of `s` at `now` (this node's clock) is written at (hive/mod.rs `claim`): past
/// the fold's lease rule (`claimable_at`) when this node saw the holder's lease expire (`claimable`),
/// so the fold, which compares the claim's stamp with the holder's, agrees with the expiry this node
/// timed on its own clock (at most `AHEAD_MS` past it; a holder's clock further ahead keeps its lease
/// until this one catches up). Otherwise `now`: a holder whose node is gone within its lease is taken
/// over by naming its last renewal (`takes`), never by a stamp in the future.
pub fn claim_at(s: &TaskState, now: i64) -> i64 {
    match s.status {
        Status::Claimed { renewed, .. } if now - s.local.renewed > LEASE_MS => now.max(renewed.saturating_add(LEASE_MS + 1).min(now.saturating_add(AHEAD_MS))),
        _ => now,
    }
}

/// What a claim of `s` at `now` takes over (`Event::takes`): the holder's last renewal, when `s` is
/// claimed and no longer live here (`TaskState::live`: its lease ran out, or its node is gone). A
/// compare-and-swap on the lease: the fold takes the claim only while no renewal of the holder that
/// sorts before it came in since, so a node wrongly suspected that renewed meanwhile keeps its claim.
pub fn takes(s: &TaskState, now: i64) -> Option<i64> {
    match s.status {
        Status::Claimed { renewed, .. } if s.claimable(now) => Some(renewed),
        _ => None,
    }
}

/// The board's receive index, local and never synced: `<board>.held`, lines of `Held`, each saying the
/// file's first `len` bytes were on this node by `ms` (its clock). `note_held` writes one after every
/// append (this node's under the board lock, hive/mod.rs `write`; a peer's lines once the mesh merged
/// them, borg-mesh lines.rs `merge`); `read` gives each line the earliest `ms` whose `len` covers it
/// (`Event::seen`): when this node first held it, at most the record's lag late, never early. Every
/// record is written under the board file's lock, and a quarantine's rewrite starts the index afresh
/// (`reset_held`), so no record of a file before speaks of the file now.
pub fn held_path(path: &Path) -> std::path::PathBuf {
    path.with_extension("held")
}

/// One record of the receive index (`held_path`): the board file (`ino`, its inode: a quarantine
/// rewrites the board by a rename, and offsets of the file before mean nothing in the new one), its
/// length then and the time.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Held {
    ino: u64,
    len: u64,
    ms: i64,
}

#[cfg(unix)]
fn ino(m: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(m)
}

#[cfg(not(unix))]
fn ino(_: &std::fs::Metadata) -> u64 {
    0
}

/// Records that every line now in the board at `path` is on this node (`held_path`): its length, read
/// before the clock, so the record never says a line was here before it was. Best effort: a record
/// lost leaves the lines to the next one, later, so they read as seen later (younger), never earlier.
pub fn note_held(path: &Path) {
    note_held_at(path, crate::learn::now_ms);
}

/// `note_held` at the time `clock` gives, read after the length; a test's own clock.
pub(crate) fn note_held_at(path: &Path, clock: impl FnOnce() -> i64) {
    let Ok(m) = std::fs::metadata(path) else { return };
    let h = Held { ino: ino(&m), len: m.len(), ms: clock() };
    if let Ok(line) = serde_json::to_string(&h) {
        let _ = super::append_bytes(&held_path(path), format!("{line}\n").as_bytes());
    }
}

/// Starts the receive index of the board at `path` afresh, with one record of the file as it is now
/// (`note_held`), by a rename: a quarantine (borg-mesh origin.rs) rewrote the board under its lock,
/// and a record of the file before would date this one's lines early once a later rewrite's file got
/// that file's inode again (a filesystem reuses a freed one). Its lines read as held from now, later
/// than they arrived, never earlier. Under the board file's lock, as every record is written, so no
/// record of the file before lands after it.
pub fn reset_held(path: &Path) {
    let held = held_path(path);
    let Ok(m) = std::fs::metadata(path) else {
        let _ = std::fs::remove_file(&held);
        return;
    };
    let h = Held { ino: ino(&m), len: m.len(), ms: crate::learn::now_ms() };
    let tmp = held.with_extension("held-tmp");
    let fresh = serde_json::to_string(&h).ok().is_some_and(|line| std::fs::write(&tmp, format!("{line}\n")).is_ok());
    if !fresh || std::fs::rename(&tmp, &held).is_err() {
        // No fresh index: none at all, so every line reads as held at the read (`read_from`).
        let _ = std::fs::remove_file(&held);
    }
}

/// For the board file `ino`: (len, earliest ms of any record covering len) by len, from its receive
/// index.
fn held_index(path: &Path, ino: u64) -> Vec<(u64, i64)> {
    let text = std::fs::read_to_string(held_path(path)).unwrap_or_default();
    let mut out: Vec<(u64, i64)> = text.lines().filter_map(|l| serde_json::from_str::<Held>(l).ok()).filter(|h| h.ino == ino).map(|h| (h.len, h.ms)).collect();
    out.sort_unstable();
    for i in (0..out.len().saturating_sub(1)).rev() {
        out[i].1 = out[i].1.min(out[i + 1].1);
    }
    out
}

/// An event of a line, None for anything unparsable, incomplete (no newline yet) or of a newer
/// format: the mesh rule, an unknown line is skipped, never an error.
fn parse(line: &str) -> Option<Event> {
    serde_json::from_str::<Event>(line).ok().filter(|e| e.v <= 1)
}

/// Every event of the board at `path`, in file order, and after them, for a goal's board
/// (`tasks.jsonl`), the beats its beats store holds (`beats`): what every fold of the goal reads.
pub fn read(path: &Path) -> Vec<Event> {
    let mut events: Vec<Event> = read_from(path, 0).0.into_iter().map(|(_, e)| e).collect();
    events.extend(beats_of(path));
    events
}

/// The beats store's events of the goal whose board is at `path` (`beats_path`), none for any other file.
pub fn beats_of(path: &Path) -> Vec<Event> {
    match path.file_name().is_some_and(|f| f == "tasks.jsonl") {
        true => beats(&beats_path(path)),
        false => Vec::new(),
    }
}

/// Whether `e` is a node's goal heartbeat (`BEAT`): a runner's line of no task saying its node is still in
/// the goal, with its watermarks. A beat of a board from before phase g's beats store still folds as one.
pub fn is_beat(e: &Event) -> bool {
    e.ev == Ev::Heartbeat && e.task.is_empty() && e.by == "runner" && e.note == BEAT
}

// ---------------------------------------------------------------- the beats store

/// A goal's beats store beside its board (`<goal>/beats.jsonl`, phase g): each node's newest beat
/// (`BeatRecord`), and its newest complete one when that is older (`kept`), rewritten whole on each new
/// beat, so a node idle for an hour costs the goal one record, not 1,200 board lines (3 s beats). Never a
/// reconciled log: the board's lines sync as a set union (borg-mesh lines.rs), which would bring every
/// compacted beat back and give each a signed origin record for good. The mesh carries a store's
/// records in its ephemeral exchange instead, signed by their node (borg-mesh beats.rs), newest wins,
/// as presence is (live.rs). What a beat says is unchanged (phase f F4, F5): the fold reads it with the
/// board's lines (`read`), its liveness from when this node first held it, on this node's clock
/// (`BeatRecord::seen`), its marks only once complete.
pub fn beats_path(board: &Path) -> std::path::PathBuf {
    board.with_file_name(BEATS_FILE)
}

pub const BEATS_FILE: &str = "beats.jsonl";

/// One beat in a goal's beats store: the beat's board line as its node wrote it (`line`, a JSON `Event`),
/// when this node first held it (`seen`, its clock), and for a peer's beat the member key that signed it
/// and the signature (borg-mesh beats.rs; empty for this node's own beats, signed when they are sent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeatRecord {
    pub line: String,
    pub seen: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sig: String,
}

impl BeatRecord {
    fn event(&self) -> Option<Event> {
        parse(&self.line).filter(is_beat).map(|e| Event { seen: Seen(Some(self.seen)), ..e })
    }
}

/// What a wait on the goal whose board is at `board` re-folds on (mod.rs `across`): the board file's and
/// its beats store's length and mtime. A beat replaces another in place, so the store's length alone
/// may not change.
pub fn mark(board: &Path) -> [(u64, Option<std::time::SystemTime>); 2] {
    let at = |p: &Path| std::fs::metadata(p).map_or((0, None), |m| (m.len(), m.modified().ok()));
    [at(board), at(&beats_path(board))]
}

/// The records of the beats store at `path` (`beats_path`), none for a missing or torn one.
pub fn beat_records(path: &Path) -> Vec<BeatRecord> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// The beats the store at `path` holds, as events seen when this node first held them.
pub fn beats(path: &Path) -> Vec<Event> {
    beat_records(path).iter().filter_map(BeatRecord::event).collect()
}

/// Of `records`, the ones that still say anything on a board holding `file` (its lines, beats aside):
/// for each node, its newest beat (its liveness: `Board::nodes` reads a node's newest line) and its newest
/// complete one (`complete` on `file`: every line of its node before it held, so its marks count,
/// `Board::watermarks`), one record when they are the same. A node's marks only grow, so the newest
/// complete beat says all its older ones did: no watermark moves back when the older beats go. A
/// function of the records and `file`.
pub fn kept(file: &[Event], records: Vec<BeatRecord>) -> Vec<BeatRecord> {
    kept_by(file, records.into_iter().filter_map(|r| Some((r.event()?, r))).collect())
}

/// `kept` of beats given as events (each with when it was held, `Event::seen`) among a board's other
/// lines: the lines as they are, then the beats that still say anything. What a node holds of a goal's
/// lines and beats at any time (scenario.rs keeps each node's view so).
pub fn compact(events: Vec<Event>) -> Vec<Event> {
    let (beats, mut lines): (Vec<Event>, Vec<Event>) = events.into_iter().partition(is_beat);
    let kept = kept_by(&lines, beats.into_iter().map(|e| (e.clone(), e)).collect());
    lines.extend(kept);
    lines
}

/// `kept` of `items`, each a beat and what stands for it, against `file`'s other lines; a beat seen
/// earlier wins a tie of one stamp (two copies of one beat: when this node first held it).
fn kept_by<T: Clone>(file: &[Event], items: Vec<(Event, T)>) -> Vec<T> {
    let lines: Vec<&Event> = file.iter().filter(|e| !is_beat(e)).collect();
    let mut keyed: Vec<(Key, &Event)> = lines.iter().map(|e| ((e.ms, 1, rank(e), line_hash(e)), *e)).collect();
    keyed.sort_by_key(|a| a.0);
    let stamps = own_stamps(&keyed);
    let mut newest: BTreeMap<String, (i64, i64, T)> = BTreeMap::new();
    let mut whole: BTreeMap<String, (i64, i64, T)> = BTreeMap::new();
    for (e, item) in items {
        if e.node.is_empty() {
            continue;
        }
        let at = (e.ms, seen(&e));
        let later = |m: &BTreeMap<String, (i64, i64, T)>| m.get(&e.node).is_none_or(|(ms, held, _)| at.0 > *ms || (at.0 == *ms && at.1 < *held));
        if complete(&stamps, &e) && later(&whole) {
            whole.insert(e.node.clone(), (at.0, at.1, item.clone()));
        }
        if later(&newest) {
            newest.insert(e.node.clone(), (at.0, at.1, item));
        }
    }
    let mut out: Vec<T> = Vec::new();
    for (node, (ms, _, item)) in newest {
        if let Some((_, _, c)) = whole.remove(&node).filter(|(w, _, _)| *w != ms) {
            out.push(c);
        }
        out.push(item);
    }
    out
}

/// Adds `records` (each a beat line and when it was held) to the beats store of the board at `board`,
/// under the store's lock, keeping what still says anything (`kept`); records `keep` refuses (a key no
/// longer a member) go. Rewritten whole by a rename, so a reader sees the store before or after, never
/// half. How many of `records` were kept: a beat older than one already held is not. When the store
/// holds each of `records` already and `keep` refuses none it holds, nothing changes, and the board is
/// not read (a peer sends the beats it holds each session).
pub fn merge_beats(board: &Path, records: Vec<BeatRecord>, keep: impl Fn(&BeatRecord) -> bool) -> std::io::Result<usize> {
    let path = beats_path(board);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::live::locked(&path, || {
        let held = beat_records(&path);
        if held.iter().all(&keep) && records.iter().all(|r| r.event().is_none() || held.iter().any(|h| h.line == r.line)) {
            return Ok(0);
        }
        let file: Vec<Event> = read_from(board, 0).0.into_iter().map(|(_, e)| e).collect();
        let all: Vec<BeatRecord> = held.iter().cloned().chain(records.iter().cloned()).filter(&keep).collect();
        let kept = kept(&file, all);
        let new = records.iter().filter(|r| kept.contains(r) && !held.contains(r)).count();
        if kept != held {
            let tmp = path.with_extension("jsonl.tmp");
            let mut body = String::new();
            for r in &kept {
                body.push_str(&serde_json::to_string(r).map_err(std::io::Error::other)?);
                body.push('\n');
            }
            std::fs::write(&tmp, body)?;
            std::fs::rename(&tmp, &path)?;
        }
        Ok(new)
    })
}

/// The events after byte `offset` (each with the offset its line ends at) and the offset to read from
/// next: only complete lines count. An offset past the end (the file was replaced) reads from 0. A line
/// no record of the receive index covers (a board from before it, a record lost) reads as held now, the
/// youngest it can be, never at its writer's stamp, another node's clock: a missing record only
/// lengthens the ages read from it (a lease, a node's liveness), never cuts one.
pub fn read_from(path: &Path, offset: u64) -> (Vec<(u64, Event)>, u64) {
    let Ok(mut f) = std::fs::File::open(path) else { return (Vec::new(), 0) };
    let meta = f.metadata().ok();
    let len = meta.as_ref().map_or(0, |m| m.len());
    let held = meta.as_ref().map(|m| held_index(path, ino(m))).unwrap_or_default();
    let seen_at = |end: u64| held.get(held.partition_point(|(len, _)| *len < end)).map(|(_, ms)| *ms);
    let start = if offset > len { 0 } else { offset };
    if f.seek(std::io::SeekFrom::Start(start)).is_err() {
        return (Vec::new(), offset);
    }
    let mut raw = Vec::new();
    let _ = f.read_to_end(&mut raw);
    // Read after the index and the lines: every line past the index was here by then.
    let now = crate::learn::now_ms();
    let mut out = Vec::new();
    let mut pos = start;
    let mut rest = raw.as_slice();
    while let Some(i) = rest.iter().position(|b| *b == b'\n') {
        let (line, tail) = rest.split_at(i + 1);
        pos += line.len() as u64;
        if let Some(e) = std::str::from_utf8(&line[..i]).ok().and_then(parse) {
            out.push((pos, Event { seen: Seen(Some(seen_at(pos).unwrap_or(now))), ..e }));
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
///
/// Every rule reads the lines in one order, the sort below, the pre-pass too: the board is a function of
/// the set of lines alone (S1), never of the order they arrived in. Two lines of one key's prefix (two
/// roster lines of a node in one ms, from before stamps were a hybrid logical clock, `stamp`) are
/// ordered by their hash. `Local` and `Board::nodes` are this node's own clock, from `Event::seen`.
/// Folded by no node in particular: every node's runner drones may be suspected (`Local::gone`); a
/// node reading its own board folds with `fold_on`.
pub fn fold(events: &[Event]) -> Board {
    fold_on(events, "")
}

/// `fold` as node `here` reads its board (phase f F4): a node never suspects itself. A claim of one of
/// its own runner's drones is gone only by its lease or a membership line (its leave, a later run's
/// join), never by this node's silence on its own board: a beat late after a suspend (the beat timer
/// on the monotonic clock, ages on the wall clock), a beat write that failed or a stalled beat thread
/// would otherwise let a sibling session take over a live claim (F2's exclusion), and the session read
/// its own claim lost. The node knows directly whether its runner runs; only a peer must guess. The
/// board is the same (`Board`'s equality): only the local clock of its tasks differs.
pub fn fold_on(events: &[Event], here: &str) -> Board {
    // Within one ms an open goes first (a task opened and claimed at once is claimed open), except the
    // open of a verify or fix task, which goes after the line written with it: it takes effect only on
    // a `done` or `rejected` the fold took.
    let order = |e: &Event| match (e.ev, e.def.as_ref().and_then(written_with)) {
        (Ev::Open, None) => 0,
        (Ev::Open, Some(_)) => 2,
        _ => 1,
    };
    let mut keyed: Vec<(Key, &Event)> = events.iter().map(|e| ((e.ms, order(e), rank(e), line_hash(e)), e)).collect();
    keyed.sort_by_key(|a| a.0);
    let mut b = Board { watermarks: watermarks(&keyed), ..Board::default() };
    // Every claim line, taken or not (phase f F5): a done or verdict written under one is for good.
    let claims: BTreeSet<(&str, &str, &str, i64)> =
        keyed.iter().filter(|(_, e)| e.ev == Ev::Claim).map(|(_, e)| (e.task.as_str(), e.node.as_str(), e.by.as_str(), e.ms)).collect();
    // When this node held each node's newest join and leave (`Local::left`): local, beside `members`.
    let mut member_held: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    // Each node's newest finished line, and each task's newest stuck line (phase f F6), in the fold's order.
    let mut finished: BTreeMap<&str, &Event> = BTreeMap::new();
    let mut stuck_lines: BTreeMap<&str, (&Event, &str)> = BTreeMap::new();
    for e in keyed.iter().map(|(_, e)| *e) {
        if !e.drone.is_empty() {
            b.drones.insert((e.node.clone(), e.by.clone()), e.drone.clone());
        }
        if e.node.is_empty() || by_engineer(&e.by) {
            continue;
        }
        let held = b.nodes.entry(e.node.clone()).or_insert(i64::MIN);
        *held = (*held).max(seen(e));
        let runner = e.ev == Ev::Heartbeat && e.task.is_empty() && e.by == "runner";
        if let Some(names) = runner.then(|| e.note.strip_prefix("drones")).flatten() {
            b.rosters.insert(e.node.clone(), (e.ms, names.split_whitespace().map(String::from).collect()));
        }
        if runner && e.note.strip_prefix(FINISHED).is_some_and(|r| r.starts_with(' ')) {
            finished.insert(&e.node, e);
        }
        if let Some((task, why)) = runner.then(|| e.note.strip_prefix(STUCK).and_then(|r| r.strip_prefix(' '))).flatten().and_then(|r| r.split_once(' ')) {
            stuck_lines.insert(task, (e, why));
        }
        if let Some(task) = runner.then(|| e.note.strip_prefix(DECLINES).and_then(|r| r.strip_prefix(' '))).flatten() {
            b.declined.insert((e.node.clone(), task.to_string()), e.ms);
        }
        if runner && (e.note == JOIN || e.note == LEAVE) {
            let (stamps, at) =
                (b.members.entry(e.node.clone()).or_insert((i64::MIN, i64::MIN)), member_held.entry(e.node.clone()).or_insert((i64::MIN, i64::MIN)));
            let (stamp, held) = if e.note == JOIN { (&mut stamps.0, &mut at.0) } else { (&mut stamps.1, &mut at.1) };
            if e.ms >= *stamp {
                (*stamp, *held) = (e.ms, seen(e));
            }
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
    let mut fold = Fold { open_span: BTreeMap::new(), claims, rounds: BTreeMap::new() };
    for (_, e) in &keyed {
        apply(&mut b, &mut fold, e);
    }
    accept_merged(&mut b);
    let stamps = own_stamps(&keyed);
    for (n, e) in finished {
        let integration = e.note[FINISHED.len() + 1..].to_string();
        b.finishes.insert(n.to_string(), Finish { ms: e.ms, integration, complete: complete(&stamps, e) });
    }
    let rejoined = |ms: i64| b.members.values().any(|(join, leave)| *join > ms && join > leave);
    for (task, (e, why)) in stuck_lines {
        let verified = b.tasks.get(task).is_some_and(|s| matches!(s.status, Status::Verified { .. }));
        if !verified && !rejoined(e.ms) {
            b.stuck.insert(task.to_string(), (e.ms, e.node.clone(), why.to_string()));
        }
    }
    // A runner's drone's claim lives while its node takes part (`Local::gone`): its membership after the
    // claim, and when this node last heard from it (never for this node's own, `fold_on`). Only on a
    // board every writing node of which joined: a node from before F4 folds a takeover within the lease
    // as lost (it reads no `takes`), so while one wrote here no claim is gone but by its lease, and every
    // node folds the same board (S1 across versions).
    let joined = |n: &str| b.members.get(n).is_some_and(|(join, _)| *join != i64::MIN);
    if !b.nodes.keys().all(|n| n.is_empty() || joined(n)) {
        return b;
    }
    for s in b.tasks.values_mut() {
        let Status::Claimed { by, node, since, .. } = &s.status else { continue };
        let (Some(&(join, leave)), true) = (b.members.get(node), b.drones.get(&(node.clone(), by.clone())).is_some_and(|d| !d.is_empty())) else { continue };
        if join == i64::MIN {
            continue;
        }
        let (join_held, leave_held) = member_held.get(node).copied().unwrap_or((i64::MIN, i64::MIN));
        if here.is_empty() || node != here {
            s.local.heard = b.nodes.get(node).copied().unwrap_or(i64::MIN);
        }
        s.local.left = match (join > *since, leave > *since) {
            (true, _) => join_held,
            (false, true) => leave_held,
            _ => i64::MAX,
        };
    }
    b
}

/// A line's place in the fold's order: (ms, opens first, rank, line hash).
type Key = (i64, u8, [u8; 32], [u8; 32]);

/// `Board::watermarks` of the lines in the fold's order (two copies of one line are adjacent there, and
/// count once). A line of node P with `seq` n is complete here when n lines of P stamped below it are held
/// here: a node's stamps rise strictly in its file (`stamp`), so those are every line P wrote before it.
/// Each complete line's marks count, each only where the line is stamped above the mark (a mark further
/// ahead than P's clock may go, `AHEAD_MS`, says nothing of P's later stamps). A function of the set of
/// lines (S1).
fn watermarks(keyed: &[(Key, &Event)]) -> BTreeMap<String, BTreeMap<String, i64>> {
    let stamps = own_stamps(keyed);
    let mut out: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    for (_, e) in keyed.iter().filter(|(_, e)| !e.node.is_empty() && e.seq.is_some()) {
        let marks = out.entry(e.node.clone()).or_default();
        if !complete(&stamps, e) {
            continue;
        }
        for (x, m) in e.marks.iter().filter(|(_, m)| **m < e.ms) {
            let w = marks.entry(x.clone()).or_insert(i64::MIN);
            *w = (*w).max(*m);
        }
    }
    out
}

/// Each node's stamps of the lines in the fold's order, each line once (two copies are adjacent there),
/// beats left out (`landing`: no `seq` counts them).
fn own_stamps<'a>(keyed: &[(Key, &'a Event)]) -> BTreeMap<&'a str, Vec<i64>> {
    let mut stamps: BTreeMap<&str, Vec<i64>> = BTreeMap::new();
    for (i, (k, e)) in keyed.iter().enumerate() {
        if !e.node.is_empty() && !is_beat(e) && (i == 0 || keyed[i - 1].0 != *k) {
            stamps.entry(e.node.as_str()).or_default().push(e.ms);
        }
    }
    stamps
}

/// Whether every line `e`'s node wrote before it is held (its `seq` lines of the node stamped below it,
/// `own_stamps`): a node's stamps rise strictly in its file, so those are all of them. False on a line
/// from before F5 (no `seq`).
fn complete(stamps: &BTreeMap<&str, Vec<i64>>, e: &Event) -> bool {
    let held = stamps.get(e.node.as_str()).map_or(0, |v| v.partition_point(|m| *m < e.ms)) as u64;
    Some(held) == e.seq
}

/// What `apply` keeps beside the board while it folds: the open span of each task's holder (an index
/// into `Board::spans`), every claim line (`fold_on`), and the rework round each claim line was made in.
struct Fold<'a> {
    open_span: BTreeMap<TaskId, usize>,
    claims: BTreeSet<(&'a str, &'a str, &'a str, i64)>,
    rounds: BTreeMap<(String, String, String, i64), u32>,
}

impl Fold<'_> {
    /// The stamp of the claim `e` (a done or verdict) was written under, when it is one for good: it names
    /// one (`Event::claim`) and that claim line is on the board (phase f F5). Taken or lost in this fold,
    /// the writer's fold gave it the task when it wrote `e`.
    fn permanent(&self, e: &Event) -> Option<i64> {
        e.claim.filter(|c| self.claims.contains(&(e.task.as_str(), e.node.as_str(), e.by.as_str(), *c)))
    }
}

/// The accepted attempt of each work task the newest integration merged (`Integration::merged`): chosen
/// there, deterministically, and kept (phase f F5, the review's correction 3: "first verified" by stamp
/// can flip as an earlier verdict arrives, a merge cannot be undone).
fn accept_merged(b: &mut Board) {
    let Some(i) = b.integrated.clone() else { return };
    for m in &i.merged {
        let Some((task, head)) = m.split_once('@') else { continue };
        let Some(s) = b.tasks.get_mut(task) else { continue };
        if let Some(a) = s.attempts.iter().find(|a| a.verified && a.head == head) {
            s.accepted_attempt = Some((a.by.clone(), a.done));
        }
    }
}

/// When this node first held `e`, on its own clock: its receive time, or its stamp for a line built in
/// memory (`Seen`).
fn seen(e: &Event) -> i64 {
    e.seen.0.unwrap_or(e.ms)
}

/// Ends `task`'s open span at stamp `to`, held here at `at` (`Local::ended`).
fn close(b: &mut Board, open_span: &mut BTreeMap<TaskId, usize>, task: &str, (to, at): (i64, i64), end: Option<Ev>) {
    if let Some(i) = open_span.remove(task) {
        b.spans[i].to_ms = Some(to);
        b.spans[i].end = end;
        if let Some(s) = b.tasks.get_mut(task) {
            s.local.ended = s.local.ended.max(at);
        }
    }
}

/// The tenure of a done or verdict for good whose writer the fold does not give the task (phase f F5):
/// the writer's tenure from `claim`, ended by a takeover or a line that displaced it, now ends with `e`;
/// one the fold never took (its claim lost here) is added. One tenure per claim either way.
fn settle(b: &mut Board, e: &Event, claim: i64) {
    let end = Span { task: e.task.clone(), by: e.by.clone(), node: e.node.clone(), from_ms: claim, to_ms: Some(e.ms), end: Some(e.ev) };
    match b.spans.iter_mut().find(|sp| sp.task == e.task && Actor::of(e).is(&sp.node, &sp.by) && sp.from_ms == claim && sp.end.is_none() && sp.to_ms.is_some())
    {
        Some(sp) => *sp = end,
        None => b.spans.push(end),
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

/// Whether `e`'s writer holds `s` in the fold, whatever its lease: the holder's session on the holder's
/// node (phase f F8, `Actor::is`: before, a line of another node's session of the same name, two
/// integrators' `integrate-d1-<ms>` in one ms, renewed, finished or judged the other's claim).
fn holds(s: &TaskState, e: &Event) -> bool {
    matches!(&s.status, Status::Claimed { by, node, .. } if Actor::of(e).is(node, by))
}

fn apply(b: &mut Board, f: &mut Fold, e: &Event) {
    let permanent = f.permanent(e);
    let open_span = &mut f.open_span;
    match e.ev {
        Ev::Open => {
            let Some(def) = e.def.as_ref().filter(|d| d.id == e.task) else { return };
            // A verify task's open is part of its work's `done`, a fix task's of its integration's
            // `rejected` (same session, same ms): a line the fold ignored opens nothing, so the task and
            // its bar are those of a line that took effect. A done or rejection taken once stays taken
            // (phase f F5, `Fold::permanent`), and its open with it; of two attempts of one round
            // (`t.v1` twice), the first in the fold's order names the verify task, and a verdict names
            // the attempt it judged (`Event::head`).
            if let Some(w) = written_with(def) {
                let end = if def.of.is_some() { Ev::Done } else { Ev::Rejected };
                if !b.spans.iter().any(|sp| sp.task == w && Actor::of(e).is(&sp.node, &sp.by) && sp.to_ms == Some(e.ms) && sp.end == Some(end)) {
                    return;
                }
                // A fix task only with the rejection that closed its integration: a later one for good (an
                // integrator whose claim lost in this fold) is an attempt only and opens nothing.
                let closed = matches!(b.tasks.get(w).map(|s| &s.status), Some(Status::Verified { by, node, ms }) if Actor::of(e).is(node, by) && *ms == e.ms);
                if def.of.is_none() && !closed {
                    return;
                }
            }
            let of_attempt = def.of.as_ref().map(|_| (e.by.clone(), e.ms));
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
                        attempts: Vec::new(),
                        accepted_attempt: None,
                        of_attempt,
                        node: e.node.clone(),
                        local: Local { opened: seen(e), ..Local::default() },
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
                b.lost.push((e.by.clone(), e.node.clone(), e.task.clone(), e.ms));
                return;
            };
            f.rounds.insert((e.task.clone(), e.node.clone(), e.by.clone(), e.ms), s.rework);
            if holds(s, e) {
                // Claimed by itself already, no rival's claim taken since: a heartbeat (`claimable_at`).
                if let Some(s) = b.tasks.get_mut(&e.task)
                    && let Status::Claimed { renewed, .. } = &mut s.status
                {
                    *renewed = e.ms;
                    s.local.renewed = s.local.renewed.max(seen(e));
                }
                return;
            }
            // A takeover of a holder whose node the claimant saw gone (`takes`) needs no lapsed lease: only
            // that the renewal it names is still the holder's last one here.
            // Taken exactly when its writer is `eligible` for the task on the line's stamp (phase f F8).
            if !eligible(b, s, Actor::of(e), At::Line { ms: e.ms, takes: e.takes }) {
                b.lost.push((e.by.clone(), e.node.clone(), e.task.clone(), e.ms));
                return;
            }
            // The previous holder's lease expired: its tenure ends at its last sign of life.
            if let Status::Claimed { renewed, .. } = s.status {
                let at = s.local.renewed;
                close(b, open_span, &e.task, (renewed, at), None);
            }
            if let Some(s) = b.tasks.get_mut(&e.task) {
                s.status = Status::Claimed { by: e.by.clone(), node: e.node.clone(), since: e.ms, renewed: e.ms };
                s.local.renewed = seen(e);
            }
            open_span.insert(e.task.clone(), b.spans.len());
            b.spans.push(Span { task: e.task.clone(), by: e.by.clone(), node: e.node.clone(), from_ms: e.ms, to_ms: None, end: None });
        }
        Ev::Heartbeat | Ev::Progress => {
            let Some(s) = b.tasks.get_mut(&e.task) else { return };
            // The holder's, with no rival's claim taken since: it renews, whatever the gap (`claimable_at`).
            let live = holds(s, e);
            if let (true, Status::Claimed { renewed, .. }) = (live, &mut s.status) {
                *renewed = e.ms;
                s.local.renewed = s.local.renewed.max(seen(e));
                if e.ev == Ev::Progress {
                    s.last_progress = e.note.clone();
                }
            }
        }
        Ev::Done => {
            let Some(s) = b.tasks.get_mut(&e.task) else { return };
            let held = holds(s, e);
            if !held && permanent.is_none() {
                return;
            }
            let claim = match (&s.status, permanent) {
                (Status::Claimed { since, .. }, _) if held => *since,
                (_, Some(c)) => c,
                _ => return,
            };
            // A done for good whose claim lost in this fold (a rival's claim stamped earlier arrived later,
            // or a takeover of a node wrongly suspected): an attempt all the same. It is the task's done
            // when it is of the round open now and no attempt of that round is done yet (the holder it
            // displaces is told at its next hook); else one more attempt of a done task.
            let round = f.rounds.get(&(e.task.clone(), e.node.clone(), e.by.clone(), claim)).copied().unwrap_or(s.rework);
            let current = held || (round == s.rework && matches!(s.status, Status::Open | Status::Claimed { .. }));
            s.attempts.push(Attempt { by: e.by.clone(), node: e.node.clone(), claim, done: e.ms, head: e.head.clone(), verified: false });
            if current {
                s.status = Status::Done { by: e.by.clone(), node: e.node.clone(), ms: e.ms };
                s.local.status = seen(e);
                s.author = Some(e.by.clone());
                if !e.note.is_empty() {
                    s.last_progress = e.note.clone();
                }
            }
            if held {
                close(b, open_span, &e.task, (e.ms, seen(e)), Some(Ev::Done));
                return;
            }
            if current {
                // The holder it displaces: its tenure ends here, its work superseded.
                close(b, open_span, &e.task, (e.ms, seen(e)), None);
            }
            settle(b, e, claim);
        }
        Ev::Verified | Ev::Rejected => {
            let Some(s) = b.tasks.get_mut(&e.task) else { return };
            let held = holds(s, e);
            if !matches!(s.task.role, Role::Verify | Role::Integrate) || (!held && permanent.is_none()) {
                return;
            }
            // A verdict for good on a task another verdict closed first (a verifier or integrator whose
            // claim lost in this fold): it stands as its own, the task keeps the first.
            let first = held || !matches!(s.status, Status::Verified { .. });
            let claim = match &s.status {
                Status::Claimed { since, .. } if held => *since,
                _ => permanent.unwrap_or(e.ms),
            };
            if first {
                s.status = Status::Verified { by: e.by.clone(), node: e.node.clone(), ms: e.ms };
                s.local.status = seen(e);
                if e.ev == Ev::Rejected {
                    s.last_progress = format!("rejected: {}", e.note);
                }
            }
            let (role, of, judged) = (s.task.role, s.task.of.clone(), s.of_attempt.clone());
            match held {
                true => close(b, open_span, &e.task, (e.ms, seen(e)), Some(e.ev)),
                false => {
                    if first {
                        close(b, open_span, &e.task, (e.ms, seen(e)), None);
                    }
                    settle(b, e, claim);
                }
            }
            // An integration's verdict is on the combined tree, no one task's: a rejected one touches
            // no other task (its fix task's open is written with it). Only the verdict that closed the
            // integrate task integrates (phase f F5: at most one integration per epoch on every board, and
            // `integrated` never moves to a later verdict for good); a later one is an attempt only.
            if role == Role::Integrate {
                if e.ev == Ev::Verified && first {
                    let merged = e.deps_in.clone();
                    let i = Integration { task: e.task.clone(), head: e.head.clone(), ms: e.ms, merged };
                    b.integrations.push(i.clone());
                    b.integrated = Some(i);
                }
                return;
            }
            let Some(w) = of.and_then(|w| b.tasks.get_mut(&w)) else { return };
            // The attempt judged: the one the verdict names by its head (phase f F5), else the verify
            // task's own.
            let judged = match e.head.as_str() {
                "" => judged,
                head => w.attempts.iter().find(|a| a.head == head).map(|a| (a.by.clone(), a.done)).or(judged),
            };
            let Some(judged) = judged else { return };
            if e.ev == Ev::Verified {
                if let Some(a) = w.attempts.iter_mut().find(|a| (a.by.as_str(), a.done) == (judged.0.as_str(), judged.1)) {
                    a.verified = true;
                }
                // Verified for good: the first verdict that passed an attempt decides the status and its
                // time, which a later verdict, a rework's claim or a rejection never moves.
                if !matches!(w.status, Status::Verified { .. }) {
                    w.status = Status::Verified { by: e.by.clone(), node: e.node.clone(), ms: e.ms };
                    w.local.status = seen(e);
                    w.accepted = Some(e.task.clone());
                    w.accepted_attempt = Some(judged);
                }
            } else if matches!(w.status, Status::Done { .. }) && {
                // A rejection reopens the work while the round of the attempt it judged is the task's done
                // round, whichever attempt of that round the done is: of two attempts of one round (a false
                // suspicion, a node not yet a participant) the done can move to the other one as its
                // earlier-stamped line arrives, and the rejection's effect must not move with it (S2').
                let attempt = w.attempts.iter().find(|a| (a.by.as_str(), a.done) == (judged.0.as_str(), judged.1));
                let round = attempt.and_then(|a| f.rounds.get(&(w.task.id.clone(), a.node.clone(), a.by.clone(), a.claim)).copied());
                round.is_none_or(|r| r == w.rework)
            } {
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
            let mine = holds(s, e);
            let engineer = by_engineer(&e.by) && matches!(s.status, Status::Claimed { .. } | Status::Blocked { .. });
            if mine || engineer {
                s.status = Status::Open;
                close(b, open_span, &e.task, (e.ms, seen(e)), Some(Ev::Released));
            }
        }
        Ev::Blocked => {
            let Some(s) = b.tasks.get_mut(&e.task) else { return };
            if holds(s, e) {
                s.status = Status::Blocked { by: e.by.clone(), note: e.note.clone() };
                close(b, open_span, &e.task, (e.ms, seen(e)), Some(Ev::Blocked));
            }
        }
    }
}

impl TaskState {
    /// Whether `session` of node `here` holds this task live at `now` (`live`): its session on its node
    /// (phase f F8; `of_node`: a claim from before nodes were stamped is this node's).
    pub fn held_by(&self, here: &str, session: &str, now: i64) -> bool {
        self.live(now).is_some_and(|(by, node, _)| by == session && of_node(node, here))
    }
}

/// The live claim of `session` of node `here` at `now`, if any (one per drone: the CLI and the handover
/// refuse a second).
pub fn holding<'a>(b: &'a Board, here: &str, session: &str, now: i64) -> Option<&'a TaskState> {
    b.tasks.values().filter(|s| s.held_by(here, session, now)).max_by_key(|s| s.live(now).map(|(_, _, since)| since))
}

/// Whether `session` of node `here`'s claim of `task` stamped `claim` ended in a done the fold keeps
/// (phase f F5: an attempt for good, though the claim itself lost in this fold to a rival's that
/// arrived later).
pub fn attempted(b: &Board, here: &str, session: &str, task: &str, claim: i64) -> bool {
    b.tasks.get(task).is_some_and(|s| s.attempts.iter().any(|a| a.by == session && of_node(&a.node, here) && a.claim == claim))
}

/// The claims `session` of node `here` lost (a race, or a claim the board did not allow): (task, stamp).
pub fn lost_by<'a>(b: &'a Board, here: &'a str, session: &'a str) -> impl Iterator<Item = (&'a TaskId, i64)> + 'a {
    b.lost.iter().filter(move |(by, node, _, _)| by == session && of_node(node, here)).map(|(_, _, t, ms)| (t, *ms))
}

/// The tasks `session` of node `here` lost a claim on after `ms`.
pub fn lost_since(b: &Board, here: &str, session: &str, ms: i64) -> Vec<TaskId> {
    let mut out: Vec<TaskId> = lost_by(b, here, session).filter(|(_, at)| *at > ms).map(|(t, _)| t.clone()).collect();
    out.dedup();
    out
}

/// Whether `who` may claim `s` at `now`, this node's clock: `eligible` there.
pub fn claimable_by(b: &Board, s: &TaskState, who: Actor, now: i64) -> bool {
    eligible(b, s, who, At::Now(now))
}

/// Whether a session of `who` would go for `s` at `now` (`next_for`'s candidates, run.rs `Offer`): it
/// may claim it (`eligible`), `allowed` lets it here, and it is not another node's task left to that
/// node's idle drones (`left_to_owner`, among `live`, the participants). Never true where `eligible` is
/// false (phase f F8): every offer and pick is a claim the fold would take.
fn open_among(b: &Board, live: &BTreeSet<&str>, s: &TaskState, who: Actor, now: i64, allowed: &impl Fn(&TaskState) -> bool) -> bool {
    claimable_by(b, s, who, now) && allowed(s) && !left_to_owner(b, live, s, who.node, now)
}

/// `open_among` among the participants at `now` (`Board::participants`).
pub fn open_for(b: &Board, s: &TaskState, who: Actor, now: i64, allowed: impl Fn(&TaskState) -> bool) -> bool {
    open_among(b, &b.participants(who.node, now), s, who, now, &allowed)
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
pub fn next_for<'a>(b: &'a Board, who: Actor, repo: &str, now: i64, allowed: impl Fn(&TaskState) -> bool) -> Option<&'a TaskState> {
    pick(b, who, repo, now, allowed).map(|p| p.free.unwrap_or(p.first))
}

/// `next_for` without its last resort: None when every task `drone` may claim is one an idle drone of
/// its node before it takes, so a runner leaves it to that drone instead of starting a session that
/// would race it (two drones woken by one change of the board, run.rs `idle_wait`); `taken_before`
/// says whether that is so.
pub fn free_for<'a>(b: &'a Board, who: Actor, repo: &str, now: i64, allowed: impl Fn(&TaskState) -> bool) -> Option<&'a TaskState> {
    pick(b, who, repo, now, allowed).and_then(|p| p.free)
}

/// The tasks no node that may still take one can take at `now`, as `here` reads `b` (phase f F6): (task,
/// why). A drone with nothing open to it ends on these (run.rs `Ending::Stuck`) instead of waiting for
/// ever; with none it waits however long the board stays unfinished (no local quiet ends it: Dijkstra &
/// Scholten 1980, Safra EWD998, termination is detected, never inferred from a quiet prefix). A task is
/// stuck when it is blocked (an engineer reopens it), when a dep is not on the board, or when it is
/// claimable with its deps met and no running drone of any of `takers` may take it: barred (a verify
/// task of a node's work, with no drone of another node left; a group's rival), or not allowed on the
/// node (its checks not approved there, another repo's): here by `allowed`, on another node by its
/// `DECLINES` line since its newest join (`declines_due`; with none it may, so its line is waited for).
/// A node's running drones are its newest roster's (its drones that ended on a budget or a fault take
/// nothing); a node with no roster may take anything it did not decline. A drone ends on these only
/// once nothing else on the board can move (`moving`). The constraint must have held for `SUSPECT_MS` since the
/// task became claimable here (`claimable_since`): a peer that started a moment later is heard within
/// that (its join and roster are one write), so a run is not given up on a join still on its way.
/// Not a safety rule: at 0 a stuck ends a run early and non-zero, at infinity a run waits on a dead
/// peer (the lease bounds `takers` instead); no result and no exit 0 depends on it. Drinking
/// philosophers' progress condition made explicit (Chandy & Misra 1984): a constrained task's eligible
/// set must stay non-empty while it is open, and an empty one is shown, not waited out. Integrate tasks
/// are none (integrate.rs runs them on any node).
pub fn stuck(b: &Board, here: &str, now: i64, allowed: impl Fn(&TaskState) -> bool) -> Vec<(TaskId, String)> {
    let takers = b.takers(here, now);
    let takes = |s: &TaskState, n: &str| if n == here { allowed(s) } else { !b.declines(n, &s.task.id) };
    let may = |s: &TaskState, n: &str| takes(s, n) && b.rosters.get(n).is_none_or(|(_, r)| r.iter().any(|d| !b.barred(s, Actor::new(n, d, ""))));
    let names = takers.iter().copied().collect::<Vec<_>>().join(", ");
    let mut out = Vec::new();
    for s in b.tasks.values().filter(|s| s.task.role != Role::Integrate) {
        let why = match &s.status {
            Status::Blocked { note, .. } => Some(format!("blocked{}", if note.is_empty() { String::new() } else { format!(": {note}") })),
            _ if !s.claimable(now) => None,
            _ => match s.task.deps.iter().find(|d| !b.tasks.contains_key(*d)) {
                Some(d) => Some(format!("needs {d}, which is not on the board")),
                None if !deps_met(&b.tasks, &s.task) || now.saturating_sub(claimable_since(b, s)) <= SUSPECT_MS => None,
                None if takers.iter().any(|n| may(s, n)) => None,
                None => Some(format!("no running drone of the nodes in the goal ({names}) may take it")),
            },
        };
        out.extend(why.map(|w| (s.task.id.clone(), w)));
    }
    out
}

/// The tasks `here` is to say it may not take at `now` (phase f F6, `DECLINES`; hive/mod.rs
/// `decline_lines`): each claimable one, deps met, not an integrate task, that `allowed` refuses on this
/// node and that its run has not declined since its join. Another node's `stuck` counts this node out
/// of such a task only by this line: without it, two nodes that may each not take a task would each
/// leave it to the other and wait for ever. A decline stands for the run: a check approved here later
/// lets this node's drones take the task (`allowed` is read again), while a peer may have ended stuck
/// on it (liveness only: the task stays on the board).
pub fn declines_due(b: &Board, here: &str, now: i64, allowed: impl Fn(&TaskState) -> bool) -> Vec<TaskId> {
    let joined = b.members.get(here).map_or(i64::MIN, |(join, _)| *join);
    let said = |s: &TaskState| b.declined.get(&(here.to_string(), s.task.id.clone())).is_some_and(|ms| *ms > joined);
    b.tasks
        .values()
        .filter(|s| s.task.role != Role::Integrate && s.claimable(now) && deps_met(&b.tasks, &s.task) && !allowed(s) && !said(s))
        .map(|s| s.task.id.clone())
        .collect()
}

/// Whether a task not `stuck` can still change the board at `now` (phase f F6): one held under a live
/// lease (its done, its release or its expiry is to come), or one claimable with its deps met (a drone
/// of some taker may take it, or `stuck`'s grace is on it). Each change may open work a waiting drone
/// can take (a verify task, a rework, a dependant), so a drone ends on a stuck only once none can: a
/// stuck task anywhere is no reason to give up the rest of the board. Every other unfinished task waits
/// on one of these or on a stuck one (a done task on its verify task, an open one on its deps), so with
/// none the board changes only by a line from outside the goal's work (an engineer's, a joiner's).
pub fn moving(b: &Board, now: i64, stuck: &[(TaskId, String)]) -> bool {
    let is_stuck = |s: &TaskState| stuck.iter().any(|(t, _)| *t == s.task.id);
    b.tasks.values().any(|s| !is_stuck(s) && (s.live(now).is_some() || (s.claimable(now) && deps_met(&b.tasks, &s.task))))
}

/// What `next_for` found: the first task the drone may claim that no idle drone before it takes, and
/// the first it may claim at all.
struct Pick<'a> {
    free: Option<&'a TaskState>,
    first: &'a TaskState,
}

fn pick<'a>(b: &'a Board, who: Actor, repo: &str, now: i64, allowed: impl Fn(&TaskState) -> bool) -> Option<Pick<'a>> {
    let node = who.node;
    let live = b.participants(node, now);
    let class = |s: &TaskState| (repo.is_empty() || !s.task.repo.eq_ignore_ascii_case(repo), s.task.role.urgency());
    let key = |s: &TaskState| {
        let place = match owner(&live, &s.task.id) == Some(node) {
            true => (false, s.opened_ms, Reverse([0; 32])),
            false => (true, -s.opened_ms, Reverse(affinity(node, &s.task.id))),
        };
        (class(s), place, s.task.id.clone())
    };
    let open = |s: &TaskState, sess: &str, d: &str| open_among(b, &live, s, Actor::new(node, d, sess), now, &allowed);
    let mut order: Vec<&TaskState> = b.tasks.values().collect();
    order.sort_by_cached_key(|s| key(s));
    let first = *order.iter().find(|s| open(s, who.session, who.drone))?;
    let mut taken: BTreeSet<&str> = BTreeSet::new();
    for d in idle_before(b, who.drone, node, now) {
        if let Some(t) = order.iter().find(|s| !taken.contains(s.task.id.as_str()) && open(s, "", &d)) {
            taken.insert(&t.task.id);
        }
    }
    let free = order.iter().copied().find(|s| !taken.contains(s.task.id.as_str()) && open(s, who.session, who.drone));
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
        .filter_map(|s| s.live(now).filter(|(_, at, _)| of_node(at, node)))
        .filter_map(|(by, at, _)| b.drone(at, by).filter(|d| *d != drone))
        .map(slot)
        .collect();
    (0..slot(drone)).filter(|&i| !busy.contains(&i) && !ended(i)).map(|i| format!("d{}", i + 1)).collect()
}

/// Whether `s` is another node's task left to its owner's idle drones at `now` (`left_to_owner`), as a
/// drone of `node` reads `b`.
pub fn left_to_another(b: &Board, s: &TaskState, node: &str, now: i64) -> bool {
    left_to_owner(b, &b.participants(node, now), s, node, now)
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
/// work: the task goes to whoever is idle. A node without a roster (no runner) keeps nothing back, nor
/// one that declined `s` (`Board::declines`; before phase f F7 an owner lacking a verify task's commits
/// kept it back from the node that holds them, each node's drone waiting out the grace and then running
/// sessions the owner's started ones kept refused).
fn left_to_owner(b: &Board, live: &BTreeSet<&str>, s: &TaskState, node: &str, now: i64) -> bool {
    let Some(o) = owner(live, &s.task.id).filter(|o| *o != node) else { return false };
    // An owner that declined it (phase f F7: its commits cannot reach it; F6: its scope, its approvals)
    // takes it with none of its drones: it is no one's to wait for.
    if b.declines(o, &s.task.id) {
        return false;
    }
    let Some((_, roster)) = b.rosters.get(o) else { return false };
    let busy: BTreeSet<&str> = b.tasks.values().filter_map(|t| t.live(now).filter(|(_, at, _)| *at == o)).filter_map(|(by, at, _)| b.drone(at, by)).collect();
    let starting = |d: &str| b.starting(o, d);
    let fresh = now - claimable_since(b, s) < OWNER_GRACE_MS;
    let idle: Vec<&str> =
        roster.iter().map(String::as_str).filter(|d| !busy.contains(d) && (fresh || starting(d)) && claimable_by(b, s, Actor::new(o, d, ""), now)).collect();
    let key = |t: &TaskState| (t.task.role.urgency(), t.opened_ms, t.task.id.clone());
    let ahead = b
        .tasks
        .values()
        .filter(|t| owner(live, &t.task.id) == Some(o) && key(t) < key(s) && idle.iter().any(|d| claimable_by(b, t, Actor::new(o, d, ""), now)))
        .count();
    ahead < idle.len()
}

/// When `s` last became claimable, as far as the board says, on this node's clock (`Local`: when it held
/// the line that made it so): its open, the end of its last tenure (an expired lease at its expiry),
/// its work's done (a verify task), its deps' verification, its last verdict (a rework). A blocked task
/// the engineer reopened reads as of its block: earlier, a shorter grace. Every age of it (the owner's
/// grace, run.rs `run_session`'s notice) is then this clock's alone.
pub fn claimable_since(b: &Board, s: &TaskState) -> i64 {
    let id = s.task.id.as_str();
    let at = |t: &str| b.tasks.get(t).filter(|t| matches!(t.status, Status::Done { .. } | Status::Verified { .. })).map_or(i64::MIN, |t| t.local.status);
    let expired = match &s.status {
        Status::Claimed { .. } => (s.local.renewed + LEASE_MS).min(s.local.gone_since()),
        _ => i64::MIN,
    };
    let verdict = b.tasks.values().filter(|v| v.task.of.as_deref() == Some(id)).map(|v| at(&v.task.id)).max().unwrap_or(i64::MIN);
    let deps = s.task.deps.iter().chain(&s.task.of).map(|d| at(d)).max().unwrap_or(i64::MIN);
    [s.local.opened, expired, s.local.ended, verdict, deps].into_iter().max().unwrap_or(s.local.opened)
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
        Status::Claimed { by, .. } => {
            format!("{} {}", short(by), if s.live(now).is_some() { format!("renewed {} ago", ago(now - s.local.renewed)) } else { "lease expired".into() })
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
            line.push_str(&format!("  opened {} ago", ago(now - s.local.opened)));
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
    // Phase f F6: what a runner gave up on, and why, until a node that may take it joins.
    if !b.stuck.is_empty() {
        out.push_str(&format!("stuck ({})\n", b.stuck.len()));
        for (t, (_, _, why)) in &b.stuck {
            out.push_str(&cut(&format!("  {t:<8} {why}"), width));
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
        let b = fold(&[Event::open(task("t2"), "engineer:e", 0), ev(Ev::Claim, "t2", "sa", 0), ev(Ev::Done, "t2", "sa", 1)]);
        assert!(b.tasks["t2"].live(2).is_none(), "a done task has no holder");
    }

    /// `e` written on `node`, first held here at `seen` (this node's clock).
    fn held_on(node: &str, seen: i64, e: Event) -> Event {
        Event { node: node.into(), seen: Seen(Some(seen)), ..e }
    }

    #[test]
    fn leases_liveness_and_grace_are_timed_on_the_observers_clock() {
        // Node B's clock is 10 s or 5 min ahead of or behind this one's (A's): its claim and its renewal
        // reach A at A's 1000 and 2000 whatever their stamps. The lease lives, B is live and the task is
        // claimable since exactly as long, on A's clock, as with one clock.
        for skew in [0, 10_000, -10_000, 300_000, -300_000] {
            let lines = b_lines(skew);
            let b = fold(&lines);
            let s = &b.tasks["t1"];
            assert!(s.live(2000 + LEASE_MS).is_some() && s.live(2001 + LEASE_MS).is_none(), "skew {skew}: the lease, from A's 2000");
            assert!(!s.claimable(2000 + LEASE_MS) && s.claimable(2001 + LEASE_MS), "skew {skew}");
            assert_eq!(claimable_since(&b, s), 2000 + LEASE_MS, "skew {skew}: claimable once the lease A timed ran out");
            assert_eq!(claimable_since(&b, &b.tasks["t2"]), 0, "skew {skew}");
            assert!(b.participants("A", 2000 + LEASE_MS).contains("B") && !b.participants("A", 2001 + LEASE_MS).contains("B"), "skew {skew}: B's liveness");
            // A's claim once it saw the lease run out is stamped where the fold, comparing it with B's
            // stamp, agrees: it takes the task over.
            let now = 2001 + LEASE_MS;
            let at = stamp(&lines, "A", claim_at(s, now));
            assert!(at >= now && at <= now + skew.max(0) + 1, "skew {skew}: {at}");
            let mut lines = lines;
            lines.push(held_on("A", now, ev(Ev::Claim, "t1", "sa", at)));
            assert_eq!(holder_of(&fold(&lines), "t1").as_deref(), Some("sa"), "skew {skew}: the takeover took effect");
        }
    }

    #[test]
    fn a_runner_drones_claim_lives_with_its_nodes_membership() {
        // Phase f F4, on A's clock. B's runner joins at 0 (held at 100) and its drone d1's session sb
        // claims t1 at 1000; B's runner beats until A last holds a line of it at 3000.
        let runner = |note: &str, ms: i64| ev(Ev::Heartbeat, "", "runner", ms).with_note(note);
        let mut lines = vec![held_on("A", 0, Event::open(task("t1"), "engineer:e", 0)), held_on("B", 100, runner(JOIN, 50))];
        lines.push(held_on("B", 1000, ev(Ev::Claim, "t1", "sb", 1000).with_drone("d1")));
        lines.push(held_on("B", 3000, runner(BEAT, 3000)));
        let b = fold(&lines);
        let s = &b.tasks["t1"];
        // Live while B is heard from within `SUSPECT_MS`; then suspected, its lease counts as expired at once.
        assert!(s.live(3000 + SUSPECT_MS).is_some() && b.participants("A", 3000 + SUSPECT_MS).contains("B"));
        let gone = 3001 + SUSPECT_MS;
        assert!(s.claimable(gone) && !b.participants("A", gone).contains("B"), "suspected");
        assert_eq!(claimable_since(&b, s), gone);
        // A's takeover names B's last renewal and is stamped now, not past the lease: the fold takes it.
        assert_eq!((claim_at(s, gone), takes(s, gone)), (gone, Some(1000)));
        let take = held_on("A", gone, Event { takes: Some(1000), ..ev(Ev::Claim, "t1", "sa", gone) });
        assert_eq!(holder_of(&fold(&[lines.clone(), vec![take.clone()]].concat()), "t1").as_deref(), Some("sa"));
        // B was alive after all: a renewal A had not held sorts before the takeover, which is refused; the
        // same claim with no `takes`, within the lease, too.
        let renewal = held_on("B", gone + 5, ev(Ev::Heartbeat, "t1", "sb", 5000));
        assert_eq!(holder_of(&fold(&[lines.clone(), vec![take.clone(), renewal]].concat()), "t1").as_deref(), Some("sb"));
        assert_eq!(holder_of(&fold(&[lines.clone(), vec![Event { takes: None, ..take }]].concat()), "t1").as_deref(), Some("sb"));
        // B heard again before anyone took over: its claim lives again (a false suspicion costs nothing).
        let back = fold(&[lines.clone(), vec![held_on("B", gone + 10, runner(BEAT, 6000))]].concat());
        assert!(back.tasks["t1"].live(gone + 10).is_some());
        // B leaves: its drone's claim is gone from when A held the leave, whatever B's last line.
        let left = fold(&[lines.clone(), vec![held_on("B", 3500, runner(LEAVE, 3500))]].concat());
        assert!(left.tasks["t1"].live(3499).is_some() && left.tasks["t1"].claimable(3500));
        // B runs again (a new join): the claim of the run before, and its session's start, are gone at once.
        let start = held_on("B", 1500, ev(Ev::Heartbeat, "", "runner", 1500).with_note(&session_note("d2", true)));
        let again = fold(&[lines.clone(), vec![start.clone(), held_on("B", 4000, runner(JOIN, 4000))]].concat());
        assert!(again.tasks["t1"].claimable(4000) && !again.starting("B", "d2"), "the run before's");
        assert!(fold(&[lines.clone(), vec![start]].concat()).starting("B", "d2"));
        // A session of its own (no drone) is no runner's: only its lease times it.
        let own = fold(&[lines[..2].to_vec(), vec![held_on("B", 1000, ev(Ev::Claim, "t1", "sb", 1000))]].concat());
        assert!(own.tasks["t1"].live(gone).is_some() && own.tasks["t1"].live(1000 + LEASE_MS + 1).is_none());
        // A node from before F4 (no join line) takes part by the lease rule.
        let old = fold(&[held_on("C", 0, runner("drones d1", 0))]);
        assert!(old.participants("A", LEASE_MS).contains("C") && !old.participants("A", LEASE_MS + 1).contains("C"));
    }

    #[test]
    fn a_done_written_under_its_claim_is_kept_whatever_claim_arrives_later() {
        // Phase f F5: sa claims t1 at 10 and is done at 20 under that claim; sb's claim stamped 5 arrives
        // after. In any arrival order the done stands (an attempt for good), its verify task with it, and
        // sb's tenure ends at the done. Without `claim` (a line from before) the done is voided as ever.
        let mut v = task("t1.v1");
        (v.role, v.of, v.not_by) = (Role::Verify, Some("t1".into()), vec!["sa".into()]);
        let done = |claim| Event { claim, head: "hA".into(), ..ev(Ev::Done, "t1", "sa", 20) };
        let lines = |claim| {
            vec![
                Event::open(task("t1"), "engineer:e", 1),
                ev(Ev::Claim, "t1", "sa", 10),
                done(claim),
                Event::open(v.clone(), "sa", 20),
                ev(Ev::Claim, "t1", "sb", 5),
            ]
        };
        for p in permutations(&lines(Some(10))) {
            let b = fold(&p);
            let s = &b.tasks["t1"];
            assert_eq!(s.status, Status::Done { by: "sa".into(), node: String::new(), ms: 20 });
            assert_eq!(s.attempts.iter().map(|a| (a.by.as_str(), a.claim, a.done)).collect::<Vec<_>>(), [("sa", 10, 20)]);
            assert!(b.tasks.contains_key("t1.v1") && attempted(&b, "", "sa", "t1", 10));
            assert!(b.spans.iter().any(|sp| sp.by == "sa" && sp.end == Some(Ev::Done)) && b.spans.iter().any(|sp| sp.by == "sb" && sp.to_ms == Some(20)));
        }
        let old = fold(&lines(None));
        assert_eq!(holder_of(&old, "t1").as_deref(), Some("sb"), "a done from before counts only from the holder");
        assert!(!old.tasks.contains_key("t1.v1"));
    }

    #[test]
    fn a_verdict_names_the_attempt_it_judged() {
        // Phase f F5: two attempts of t1's first round: sa's done (the task's, t1.v1 written with it) and
        // sb's, under a claim that lost here. A verifier that judged sb's (its fold had sb's t1.v1)
        // names it by its head: sb's attempt is the accepted one, verified for good.
        let mut v = task("t1.v1");
        (v.role, v.of) = (Role::Verify, Some("t1".into()));
        let lines = vec![
            Event::open(task("t1"), "engineer:e", 1),
            ev(Ev::Claim, "t1", "sa", 10),
            Event { claim: Some(10), head: "hA".into(), ..ev(Ev::Done, "t1", "sa", 20) },
            Event::open(v.clone(), "sa", 20),
            ev(Ev::Claim, "t1", "sb", 11),
            Event { claim: Some(11), head: "hB".into(), ..ev(Ev::Done, "t1", "sb", 25) },
            Event::open(v, "sb", 25),
            ev(Ev::Claim, "t1.v1", "sv", 30),
            Event { claim: Some(30), head: "hB".into(), ..ev(Ev::Verified, "t1.v1", "sv", 40) },
        ];
        let b = fold(&lines);
        let s = &b.tasks["t1"];
        assert_eq!((s.attempts.len(), b.tasks["t1.v1"].of_attempt.clone()), (2, Some(("sa".to_string(), 20))), "the first done names t1.v1");
        assert!(matches!(s.status, Status::Verified { ms: 40, .. }));
        assert_eq!(s.accepted_attempt, Some(("sb".to_string(), 25)));
        assert!(s.attempts.iter().any(|a| a.by == "sb" && a.verified) && !s.attempts.iter().any(|a| a.by == "sa" && a.verified));
    }

    #[test]
    fn a_rejection_reopens_its_round_whichever_attempt_is_the_done() {
        // Phase f F5, after review: sa's done (claim 10) opens t1.v1, which sv rejects. sb's attempt of the
        // same round (claim 5, done 15) arrives late and becomes the task's done. The rejection still
        // reopens t1 in every order and at every point after it: before, the reopen was retracted, t1.v1
        // closed and sb's attempt never judged, so nothing was claimable and the goal stalled.
        let mut v = task("t1.v1");
        (v.role, v.of) = (Role::Verify, Some("t1".into()));
        let early = vec![
            Event::open(task("t1"), "engineer:e", 1),
            ev(Ev::Claim, "t1", "sa", 10),
            Event { claim: Some(10), head: "hA".into(), ..ev(Ev::Done, "t1", "sa", 20) },
            Event::open(v.clone(), "sa", 20),
            ev(Ev::Claim, "t1.v1", "sv", 30),
            Event { claim: Some(30), head: "hA".into(), ..ev(Ev::Rejected, "t1.v1", "sv", 40) },
        ];
        let late = vec![ev(Ev::Claim, "t1", "sb", 5), Event { claim: Some(5), head: "hB".into(), ..ev(Ev::Done, "t1", "sb", 15) }, Event::open(v, "sb", 15)];
        let reopened = |b: &Board| matches!(b.tasks["t1"].status, Status::Open) && b.tasks["t1"].rework == 1;
        assert!(reopened(&fold(&early)), "the rejection took effect");
        for order in permutations(&late) {
            let mut lines = early.clone();
            for e in order {
                lines.push(e);
                let b = fold(&lines);
                assert!(reopened(&b), "retracted with {} of sb's lines: {:?} rework {}", lines.len() - early.len(), b.tasks["t1"].status, b.tasks["t1"].rework);
            }
        }
        let b = fold(&[early, late].concat());
        assert_eq!(b.tasks["t1"].attempts.len(), 2);
        assert!(claimable_by(&b, &b.tasks["t1"], Actor::new("", "d2", "sc"), 50), "the rework is claimable");
    }

    #[test]
    fn only_the_verdict_that_closed_an_integration_integrates() {
        // Phase f F5, after review: si integrates (claim 5, tree m1); sj's claim (4, tree m2), which lost
        // nowhere it was seen, arrives with its own pass for good. One integration, si's or sj's by the
        // fold's order, the same at every point once it is in; a late rejection for good opens no fix task.
        let integrate = Task { role: Role::Integrate, ..task("integrate.1") };
        let open = Event::open(integrate, "si", 3);
        let verdict = |ev_: Ev, by: &str, claim: i64, ms: i64, head: &str| Event { claim: Some(claim), head: head.into(), ..ev(ev_, "integrate.1", by, ms) };
        let si = vec![open.clone(), ev(Ev::Claim, "integrate.1", "si", 5), verdict(Ev::Verified, "si", 5, 20, "m1")];
        for (sj_ms, winner) in [(25, "m1"), (15, "m2")] {
            let sj = vec![ev(Ev::Claim, "integrate.1", "sj", 4), verdict(Ev::Verified, "sj", 4, sj_ms, "m2")];
            let b = fold(&[si.clone(), sj].concat());
            assert_eq!(b.integrations.len(), 1, "{:?}", b.integrations);
            assert_eq!(b.integrated.as_ref().map(|i| i.head.as_str()), Some(winner));
        }
        let fix = Task { from: Some("m2".into()), ..task("integrate.1.fix") };
        let sj = vec![ev(Ev::Claim, "integrate.1", "sj", 4), verdict(Ev::Rejected, "sj", 4, 25, "m2"), Event::open(fix, "sj", 25)];
        let b = fold(&[si, sj].concat());
        assert_eq!(b.integrated.as_ref().map(|i| i.head.as_str()), Some("m1"));
        assert!(!b.tasks.contains_key("integrate.1.fix"), "a rejection that closed nothing opens no fix task");
    }

    #[test]
    fn a_claim_is_final_once_each_participants_watermark_is_past_it() {
        // Phase f F5. A claims t1 at 100; B, joined, claimed t2, then holds A's claim and beats (its marks
        // name A's 100). On A: with B's beat and every line of B before it, A's claim is final; with B's
        // claim of t2 held back, it is not, whatever B's stamps say; once B is suspected, it no longer waits.
        let on = |node: &str, e: Event| Event { node: node.into(), ..e };
        let runner = |note: &str, ms: i64| on("B", ev(Ev::Heartbeat, "", "runner", ms).with_note(note));
        let base = vec![on("A", Event::open(task("t1"), "engineer:e", 0)), on("A", Event::open(task("t2"), "engineer:e", 0))];
        let mut b_file = base.clone();
        for e in [runner(JOIN, 50), on("B", ev(Ev::Claim, "t2", "sb", 60))] {
            let landed = landing(&b_file, vec![e]);
            b_file.extend(landed);
        }
        let claim = on("A", ev(Ev::Claim, "t1", "sa", 100).with_drone("d1"));
        b_file.push(claim.clone());
        assert_eq!(to_ack(&b_file, "B"), Some(100), "B owes A's claim a mark");
        let beat = landing(&b_file, vec![runner(BEAT, 70)]).remove(0);
        assert!(beat.ms > 100 && beat.marks.get("A") == Some(&100) && beat.seq == Some(2), "{beat:?}");
        b_file.push(beat.clone());
        assert_eq!(to_ack(&b_file, "B"), None);
        let (join, b_claim) = (b_file[2].clone(), b_file[3].clone());
        let on_a = |lines: Vec<Event>| fold_on(&[base.clone(), vec![claim.clone()], lines].concat(), "A");
        let now = 150;
        let full = on_a(vec![join.clone(), b_claim, beat.clone()]);
        assert_eq!(full.unconfirmed("A", 100, now), (vec![], vec![]), "final");
        assert_eq!(full.unconfirmed("A", 101, now).0, ["B"], "a claim past the mark waits");
        let gap = on_a(vec![join, beat]);
        assert_eq!(gap.unconfirmed("A", 100, now).0, ["B"], "B's claim of t2 held back: not every line of B before the beat is here");
        assert_eq!(gap.unconfirmed("A", 100, now + SUSPECT_MS + 1), (vec![], vec![]), "B suspected: no longer waited for");
        // A node that writes no watermark (from before F5) is one to wait for its mesh round instead.
        let old = on_a(vec![Event { seq: None, marks: BTreeMap::new(), ..b_file[2].clone() }]);
        assert_eq!(old.unconfirmed("A", 100, now), (vec![], vec!["B"]));
    }

    #[test]
    fn a_peers_beat_marks_nothing_of_its_node() {
        // Phase g, after review: C's beat (500) reaches B through the live exchange before C's claim of t1
        // (400) reaches it through reconciliation. B's next beat marks C only to the claim B holds, so on C
        // the claim still waits for B (`Board::unconfirmed`) instead of passing for one B acknowledged.
        let on = |node: &str, e: Event| Event { node: node.into(), ..e };
        let runner = |node: &str, note: &str, ms: i64| on(node, ev(Ev::Heartbeat, "", "runner", ms).with_note(note));
        let b_file = vec![on("C", Event::open(task("t1"), "engineer:e", 0)), runner("C", JOIN, 10), runner("C", BEAT, 500)];
        let beat = landing(&b_file, vec![runner("B", BEAT, 600)]).remove(0);
        assert_eq!(beat.marks.get("C"), Some(&10), "marked to C's join, below its claim at 400, not to its beat: {beat:?}");
    }

    #[test]
    fn a_node_never_suspects_itself() {
        // Phase f F4, after review: A's runner joins at 0 and its drone d1's session sa1 claims t1 at 1000;
        // A holds no line of its own after its beat at 3000 (a suspend, a failed beat write, a stalled
        // beat thread). Well within the lease, a peer suspects A; A itself does not: its d2 may not take
        // t1, and sa1 still holds it. A leave or a new run's join still ends the claim at once.
        let runner = |note: &str, ms: i64| ev(Ev::Heartbeat, "", "runner", ms).with_note(note);
        let lines = vec![
            held_on("A", 0, Event::open(task("t1"), "engineer:e", 0)),
            held_on("A", 0, runner(JOIN, 0)),
            held_on("A", 1000, ev(Ev::Claim, "t1", "sa1", 1000).with_drone("d1")),
            held_on("A", 3000, runner(BEAT, 3000)),
        ];
        let now = 3001 + SUSPECT_MS;
        let (peer, own) = (fold(&lines), fold_on(&lines, "A"));
        assert_eq!(peer, own, "one board");
        assert!(claimable_by(&peer, &peer.tasks["t1"], Actor::new("A", "d2", "sa2"), now) && takes(&peer.tasks["t1"], now).is_some(), "a peer suspects A");
        let s = &own.tasks["t1"];
        assert!(!claimable_by(&own, s, Actor::new("A", "d2", "sa2"), now) && takes(s, now).is_none(), "A's own sibling may not take t1");
        assert!(s.live(now).is_some_and(|(by, _, _)| by == "sa1") && s.live(1000 + LEASE_MS + 1).is_none(), "held for its lease");
        let left = fold_on(&[lines.clone(), vec![held_on("A", 3500, runner(LEAVE, 3500))]].concat(), "A");
        assert!(left.tasks["t1"].claimable(3500), "its leave");
        let again = fold_on(&[lines.clone(), vec![held_on("A", 4000, runner(JOIN, 4000))]].concat(), "A");
        assert!(again.tasks["t1"].claimable(4000), "a new run's join");
    }

    #[test]
    fn a_board_a_node_from_before_membership_wrote_to_suspects_no_one() {
        // Phase f F4, after review: C wrote to the board but never joined (a runner from before F4). It
        // folds an in-lease takeover as lost, so no node takes one: B's drone's claim is gone only by its
        // lease, on every node, and the board stays one across versions (S1).
        let runner = |note: &str, ms: i64| ev(Ev::Heartbeat, "", "runner", ms).with_note(note);
        let lines = vec![
            held_on("A", 0, Event::open(task("t1"), "engineer:e", 0)),
            held_on("B", 100, runner(JOIN, 50)),
            held_on("B", 1000, ev(Ev::Claim, "t1", "sb", 1000).with_drone("d1")),
            held_on("C", 1500, runner("drones d1", 1500)),
        ];
        let b = fold(&lines);
        let s = &b.tasks["t1"];
        assert!(s.live(1001 + SUSPECT_MS).is_some() && takes(s, 1001 + SUSPECT_MS).is_none(), "no suspicion beside C");
        assert!(s.claimable(1001 + LEASE_MS) && takes(s, 1001 + LEASE_MS) == Some(1000), "its lease");
        let leave = fold(&[lines.clone(), vec![held_on("B", 2000, runner(LEAVE, 2000))]].concat());
        assert!(leave.tasks["t1"].live(2000).is_some(), "nor a leave");
        // C joins (upgraded): suspicion holds again.
        let joined = fold(&[lines, vec![held_on("C", 1600, runner(JOIN, 1600))]].concat());
        assert!(joined.tasks["t1"].claimable(1001 + SUSPECT_MS));
    }

    /// Two tasks opened on A; B, its clock `skew` off A's, claims t1 and renews it, each line reaching A
    /// a second after the one before, B's stamps its hybrid clock (`stamp`).
    fn b_lines(skew: i64) -> Vec<Event> {
        let mut lines = vec![held_on("A", 0, Event::open(task("t1"), "engineer:e", 0)), held_on("A", 0, Event::open(task("t2"), "engineer:e", 0))];
        for (at, e) in [(1000, Ev::Claim), (2000, Ev::Heartbeat)] {
            let ms = stamp(&lines, "B", at + skew);
            lines.push(held_on("B", at, ev(e, "t1", "sb", ms)));
        }
        lines
    }

    #[test]
    fn a_hybrid_stamp_is_above_every_line_held_and_monotone_per_node() {
        let lines = [held_on("A", 0, ev(Ev::Heartbeat, "", "runner", 50)), held_on("B", 0, ev(Ev::Heartbeat, "", "runner", 900))];
        assert_eq!(stamp(&lines, "A", 100), 901, "above B's line it holds: a line written after it sorts after it");
        assert_eq!(stamp(&lines, "A", 2000), 2000, "the wall clock when it is ahead");
        assert_eq!(stamp(&lines, "B", 10), 901, "above its own last line whatever its clock");
        assert_eq!(stamp(&lines[..1], "A", 50), 51, "two lines of one node never share a stamp");
        let broken = [held_on("B", 0, ev(Ev::Heartbeat, "", "runner", 10 * AHEAD_MS))];
        assert_eq!(stamp(&broken, "A", 100), 100 + AHEAD_MS, "a peer's clock far ahead drags this one at most AHEAD_MS");
        assert_eq!(stamp(&[], "A", 7), 7);
    }

    #[test]
    fn read_gives_each_line_when_this_node_first_held_it() {
        let dir = std::env::temp_dir().join(format!("borg-hive-board-held-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("tasks.jsonl");
        append(&path, &[ev(Ev::Heartbeat, "", "runner", 5)]).unwrap();
        let t0 = crate::learn::now_ms();
        assert!(read(&path)[0].seen.0.is_some_and(|ms| ms >= t0), "no record: held as of the read, never at its stamp");
        note_held(&path);
        let first = read(&path)[0].seen.0.expect("held");
        append(&path, &[ev(Ev::Heartbeat, "t1", "s", 6)]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        note_held(&path);
        note_held(&path);
        let got = read(&path);
        assert_eq!(got[0].seen.0, Some(first), "a later record never moves a line's time");
        assert!(got[1].seen.0.is_some_and(|ms| ms > first), "{got:?}");
        assert_eq!(got[0], ev(Ev::Heartbeat, "", "runner", 5), "where it was seen is no part of the line");
        // A rewrite (a quarantine's rename) is a new file: the records of the old one say nothing of it.
        let tmp = dir.join("tasks.jsonl.tmp");
        std::fs::copy(&path, &tmp).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        let t1 = crate::learn::now_ms();
        assert!(read(&path).iter().all(|e| e.seen.0.is_some_and(|ms| ms >= t1)), "held as of the read");
        // A record of a file before that the rewrite's file shares an inode with (a reused one, here the
        // same file's) would date its lines early: the rewrite starts the index afresh.
        let ino = ino(&std::fs::metadata(&path).unwrap());
        super::super::append_bytes(&held_path(&path), format!("{{\"ino\":{ino},\"len\":999999,\"ms\":1}}\n").as_bytes()).unwrap();
        assert!(read(&path).iter().all(|e| e.seen.0 == Some(1)), "the stale record dates every line at 1");
        reset_held(&path);
        let got = read(&path);
        assert!(got.iter().all(|e| e.seen.0.is_some_and(|ms| ms >= t1)), "{got:?}");
        assert_eq!(std::fs::read_to_string(held_path(&path)).unwrap().lines().count(), 1, "one record, of the file now");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deps_gate_claims() {
        let mut t2 = task("t2");
        t2.deps = vec!["t1".into()];
        let base = vec![Event::open(task("t1"), "engineer:e", 0), Event::open(t2, "engineer:e", 0)];
        let b = fold(&[base.clone(), vec![ev(Ev::Claim, "t2", "sa", 1)]].concat());
        assert_eq!(holder_of(&b, "t2"), None);
        assert_eq!(lost_since(&b, "", "sa", 0), ["t2"]);
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
        assert!(!claimable_by(&b, &b.tasks["t1.v1"], Actor::new("", "", "sa"), 3) && claimable_by(&b, &b.tasks["t1.v1"], Actor::new("", "", "sb"), 3));
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
        assert!(b.barred(&b.tasks["t1.v1"], Actor::new("", "", "sa2")) && !claimable_by(&b, &b.tasks["t1.v1"], Actor::new("", "", "sa2"), 3));
        assert!(b.barred(&b.tasks["t1.v1"], Actor::new("", "d1", "sa3")) && !b.barred(&b.tasks["t1.v1"], Actor::new("", "d2", "sa3")));
        // A fresh iteration of d1 that wrote no line yet is not offered it either.
        assert_eq!(next_for(&b, Actor::new("", "d1", "sa3"), "", 3, |_| true).map(|s| s.task.id.as_str()), None, "its own verify task is not offered to d1");
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
        assert_eq!(
            next_for(&b, Actor::new("", "", "me"), "app", 3, |_| true).map(|s| s.task.id.as_str()),
            Some("t0.v1"),
            "verify before work in the same repo"
        );
        assert_eq!(next_for(&b, Actor::new("", "", "sx"), "app", 3, |_| true).map(|s| s.task.id.as_str()), Some("z-mine"), "never the own verify task");
        assert_eq!(next_for(&b, Actor::new("", "", "me"), "elsewhere", 3, |_| true).map(|s| s.task.id.as_str()), Some("t0.v1"));
        assert_eq!(next_for(&b, Actor::new("", "", "me"), "other", 3, |_| true).map(|s| s.task.id.as_str()), Some("a-other"), "same repo first");
        assert_eq!(next_for(&b, Actor::new("", "", "me"), "other", 3, |s| s.task.repo != "other").map(|s| s.task.id.as_str()), Some("t0.v1"));
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
        assert_eq!(b.integrated, Some(Integration { task: "integrate.1".into(), head: "m1".into(), ms: 6, merged: vec![] }));
        assert_eq!(integrate_id(&b), "integrate.2");
        // While it is held, nothing is due and the goal is not reached.
        let b = fold(&[verified_t1(), claimed_integrate("integrate.1", 5)].concat());
        assert!(!b.finished() && !integration_due(&b) && b.tasks.values().any(|s| s.live(6).is_some()));
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
        assert!(b.spans.iter().all(|s| s.by != "fresh") && !b.drones.keys().any(|(_, s)| s == "fresh"), "the board has no line of the fresh session");
        assert_eq!(b.tasks["t1.v1"].task.not_by, ["sa", "d1"]);
        assert_eq!(next_for(&b, Actor::new("", "d1", "fresh"), "", 3, |_| true).map(|s| s.task.id.as_str()), None, "the runner's drone name bars it");
        assert_eq!(next_for(&b, Actor::new("", "d2", "fresh"), "", 3, |_| true).map(|s| s.task.id.as_str()), Some("t1.v1"));
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
        // The author's session bars on its node; since phase f F8 a session of that name on B is another
        // (`Actor`: a name says who only with its node).
        let b = fold(&[base.clone(), vec![on(ev(Ev::Claim, "t1.v1", "sa", 3), "A")]].concat());
        assert_eq!(holder_of(&b, "t1.v1"), None);
        let b = fold(&[base, vec![on(ev(Ev::Claim, "t1.v1", "sa", 3), "B")]].concat());
        assert_eq!(holder_of(&b, "t1.v1").as_deref(), Some("sa"));
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
            assert_eq!(lost_since(&b, "A", "sa", 0), ["t1"], "A's claim lost; its later done is ignored");
            assert!(lost_since(&b, "B", "sa", 0).is_empty(), "a session of that name on B lost nothing");
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
                let Some(t) = next_for(&fold(&lines), Actor::new(node, &drone, &session), "", 100, |_| true).map(|s| s.task.id.clone()) else { break };
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
                    let at_once: Vec<TaskId> = (1..=3)
                        .map_while(|d| next_for(&b, Actor::new("A", &format!("d{d}"), &format!("A{d}")), "", 100, |_| true).map(|s| s.task.id.clone()))
                        .collect();
                    let distinct: BTreeSet<&TaskId> = at_once.iter().collect();
                    // Past the share they spill one after another into the far end: apart while there are tasks.
                    assert!(distinct.len() >= 3.min(m), "{nodes:?} m={m} share={share} trial {trial}: {at_once:?}");
                    let mut seen = Vec::new();
                    for d in 1..=3 {
                        let Some(t) =
                            next_for(&fold(&lines), Actor::new("A", &format!("d{d}"), &format!("A{d}")), "", 100, |_| true).map(|s| s.task.id.clone())
                        else {
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
        let pick =
            |lines: &[Event], d: usize| next_for(&fold(lines), Actor::new("", &format!("d{d}"), &format!("A{d}")), "", 5, |_| true).map(|s| s.task.id.clone());
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
        assert_eq!(free_for(&b, Actor::new("", "d1", ""), "", 5, |_| true).map(|s| s.task.id.as_str()), Some("t1"));
        assert_eq!(free_for(&b, Actor::new("", "d2", ""), "", 5, |_| true).map(|s| s.task.id.as_str()), None);
        assert_eq!(next_for(&b, Actor::new("", "d2", "A2"), "", 5, |_| true).map(|s| s.task.id.as_str()), Some("t1"));
        let busy = [lines.clone(), vec![Event::open(task("t0"), "engineer:e", 0), ev(Ev::Claim, "t0", "A1", 2).with_drone("d1")]].concat();
        assert_eq!(free_for(&fold(&busy), Actor::new("", "d2", ""), "", 5, |_| true).map(|s| s.task.id.as_str()), Some("t1"));
        // A task d1 may not claim is d2's at once.
        let b = fold(&worked("w", "sx", "d1", 1));
        assert_eq!(free_for(&b, Actor::new("", "d2", ""), "", 5, |_| true).map(|s| s.task.id.as_str()), Some("w.v1"));
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
        assert_eq!(next_for(&fold(&lines), Actor::new("A", "d2", "A2"), "", 2, |_| true).map(|s| s.task.id.clone()), Some(a));
    }

    #[test]
    fn a_busy_drone_of_the_same_class_is_counted_once() {
        // One node, t1..t5 open, d1 holds t1. d2 and d3 pick from the same board, or d2 before d1's claim
        // reached it: each time two different tasks, the same ones either way.
        let opens: Vec<Event> = (1..=5).map(|i| on(Event::open(task(&format!("t{i}")), "engineer:e", i), "A")).collect();
        let claim = on(ev(Ev::Claim, "t1", "A1", 10).with_drone("d1"), "A");
        let pick = |lines: &[Event], d: usize| {
            next_for(&fold(lines), Actor::new("A", &format!("d{d}"), &format!("A{d}")), "", 20, |_| true).map(|s| s.task.id.clone())
        };
        let seen: Vec<Event> = opens.iter().cloned().chain([claim]).collect();
        assert_eq!((pick(&seen, 2), pick(&seen, 3)), (Some("t2".into()), Some("t3".into())), "both see d1's claim");
        assert_eq!((pick(&opens, 2), pick(&seen, 3)), (Some("t2".into()), Some("t3".into())), "d2 does not see it yet, d3 does");
    }

    #[test]
    fn an_ended_drone_takes_no_place() {
        // The runner's newest line names d2 alone (d1 and d3 ended): d2 takes the oldest task, not the
        // second; with no such line every drone before it counts.
        let mut lines: Vec<Event> = (1..=3).map(|i| on(Event::open(task(&format!("t{i}")), "engineer:e", i), "A")).collect();
        let pick = |lines: &[Event]| next_for(&fold(lines), Actor::new("A", "d2", "A2"), "", 20, |_| true).map(|s| s.task.id.clone());
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
        let pick = |lines: &[Event], now: i64| next_for(&fold(lines), Actor::new("A", "d1", "A1"), "", now, |_| true).map(|s| s.task.id.clone());
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
    fn a_task_left_to_a_peers_starting_session_is_not_open_here() {
        // d3-accept3 W2: B's d1 started a session that had not claimed 12 s on; A's idle d1 saw nothing
        // open, and ended before B's claim (phase f F6: it now waits while the goal is unfinished). The task
        // is not A's to take while B's session is on its way to it.
        let live = BTreeSet::from(["A", "B"]);
        let t: String = (0..).map(|i| format!("b{i}")).find(|t| owner(&live, t) == Some("B")).unwrap();
        let roster = on(Event::new(Ev::Heartbeat, "", "runner", 1).with_note(&roster_note(&["d1".to_string()])), "B");
        let start = on(Event::new(Ev::Heartbeat, "", "runner", 2).with_note(&session_note("d1", true)), "B");
        let opened = vec![on(Event::open(task(&t), "engineer:e", 0), "A"), roster];
        let b = fold(&opened);
        assert_eq!(
            free_for(&b, Actor::new("A", "d1", ""), "", 3, |_| true).map(|s| s.task.id.clone()),
            None,
            "B's runner has OWNER_GRACE_MS to start a session for it"
        );
        let started = [opened, vec![start]].concat();
        let at = 10 * OWNER_GRACE_MS;
        let b = fold(&started);
        assert_eq!(free_for(&b, Actor::new("A", "d1", ""), "", at, |_| true).map(|s| s.task.id.clone()), None, "left to B's session");
        let ended = [started, vec![on(Event::new(Ev::Heartbeat, "", "runner", 9).with_note(&session_note("d1", false)), "B")]].concat();
        let b = fold(&ended);
        assert_eq!(free_for(&b, Actor::new("A", "d1", ""), "", at, |_| true).map(|s| s.task.id.clone()), Some(t), "it ended without a claim");
    }

    #[test]
    fn a_task_no_running_drone_of_the_goal_may_take_is_stuck_until_one_joins() {
        // Phase f F6. B's only drone d1 authored w: w.v1 is barred to it. While A runs a drone, w.v1 is
        // A's to take and not stuck, whatever A's drone is doing; once A's roster is empty (its drone
        // ended on its budget) or A left the goal, no running drone may take it, and once claimable for
        // `SUSPECT_MS` it is stuck. A cut, a suspended A, is no stuck within the lease (`takers`). A join
        // with a drone that may take it ends the stuck, also the stuck line's on the board.
        let runner = |node: &str, ms: i64, note: &str| on(Event::new(Ev::Heartbeat, "", "runner", ms).with_note(note), node);
        let d1 = || roster_note(&["d1".to_string()]);
        let mut lines = vec![runner("B", 0, JOIN), runner("B", 0, &d1()), runner("A", 0, JOIN), runner("A", 0, &d1())];
        lines.extend(worked("w", "sB", "d1", 2).into_iter().map(|e| on(e, "B")));
        let at = 2 + SUSPECT_MS + 1;
        let stuck_at = |lines: &[Event], now: i64| stuck(&fold_on(lines, "B"), "B", now, |_| true).into_iter().map(|(t, _)| t).collect::<Vec<_>>();
        assert_eq!(stuck_at(&lines, at), Vec::<String>::new(), "A's d1 may verify it");
        assert_eq!(stuck_at(&lines, LEASE_MS - 1), Vec::<String>::new(), "A silent but within the lease: a cut, not a stuck");
        assert_eq!(stuck_at(&lines, LEASE_MS + 2), vec!["w.v1".to_string()], "A silent a whole lease");
        let left = [lines.clone(), vec![runner("A", 3, LEAVE)]].concat();
        assert_eq!(stuck_at(&left, 2 + SUSPECT_MS), Vec::<String>::new(), "claimable for no more than SUSPECT_MS");
        assert_eq!(stuck_at(&left, at), vec!["w.v1".to_string()], "A left: only B's d1 runs, and it authored w");
        let spent = [lines.clone(), vec![runner("A", 3, &roster_note(&[]))]].concat();
        assert_eq!(stuck_at(&spent, at), vec!["w.v1".to_string()], "A's drone ended");
        assert_eq!(stuck(&fold_on(&lines, "B"), "B", at, |s| s.task.id != "w.v1"), vec![], "not allowed here, A may take it");
        // A says it may not take w.v1 (its checks not approved there): no running drone may. A decline
        // from A's run before its newest join counts no more.
        let declined = [lines.clone(), vec![runner("A", 3, &decline_note("w.v1"))]].concat();
        assert_eq!(stuck_at(&declined, at), vec!["w.v1".to_string()], "A declined it");
        assert_eq!(declines_due(&fold_on(&declined, "A"), "A", at, |s| s.task.id != "w.v1"), Vec::<String>::new(), "said once a run");
        assert_eq!(declines_due(&fold_on(&lines, "A"), "A", at, |s| s.task.id != "w.v1"), vec!["w.v1".to_string()]);
        let rejoined = [declined, vec![runner("A", 4, JOIN)]].concat();
        assert_eq!(stuck_at(&rejoined, at), Vec::<String>::new(), "a new run of A judges it again");
        // The runner says so on the board; a join of a node still in the goal clears it, and C's d1 may take w.v1.
        let said = [left.clone(), vec![runner("B", at, &stuck_note("w.v1", "no running drone"))]].concat();
        assert_eq!(fold(&said).stuck.get("w.v1").map(|(_, n, _)| n.as_str()), Some("B"));
        assert!(render(&fold(&said), at, 120).contains("stuck (1)\n  w.v1     no running drone"), "{}", render(&fold(&said), at, 120));
        let joined = [said, vec![runner("C", at + 1, JOIN), runner("C", at + 1, &d1())]].concat();
        assert!(fold(&joined).stuck.is_empty(), "a join after it clears the stuck line");
        assert_eq!(stuck_at(&joined, at + 2), Vec::<String>::new(), "C's d1 may take it");
        // C leaves again without taking it: the line stands (`render` shows it), as the constraint does.
        let gone = [joined, vec![runner("C", at + 2, LEAVE)]].concat();
        assert_eq!(fold(&gone).stuck.get("w.v1").map(|(_, n, _)| n.as_str()), Some("B"), "a joiner that left clears nothing");
        // One stuck task ends no drone while another task can still move (`moving`): a held one, or one
        // claimable with its deps met; once neither is left, it does.
        let st = vec![("w.v1".to_string(), String::new())];
        let held = [left.clone(), vec![Event::open(task("z"), "engineer:e", 1), ev(Ev::Claim, "z", "sZ", at)]].concat();
        assert!(moving(&fold_on(&held, "B"), at, &st) && moving(&fold_on(&left, "B"), at, &[]), "z held, w.v1 claimable");
        assert!(!moving(&fold_on(&left, "B"), at, &st), "only w.v1 could move, and it is stuck");
        // A blocked task is stuck at once; a dep not on the board too.
        let blocked =
            [lines.clone(), vec![Event::open(task("x"), "engineer:e", 1), ev(Ev::Claim, "x", "sX", 2), ev(Ev::Blocked, "x", "sX", 3).with_note("needs a key")]]
                .concat();
        assert!(stuck(&fold_on(&blocked, "B"), "B", 4, |_| true).contains(&("x".to_string(), "blocked: needs a key".to_string())));
        let orphan = Task { deps: vec!["gone".into()], ..task("y") };
        let b = fold_on(&[lines, vec![Event::open(orphan, "engineer:e", 1)]].concat(), "B");
        assert_eq!(stuck(&b, "B", 2, |_| true), vec![("y".to_string(), "needs gone, which is not on the board".to_string())]);
    }

    #[test]
    fn a_finish_is_for_good_once_every_participant_wrote_it_at_that_integration() {
        // Phase f F6. A finished board on A: B is a participant that speaks (joined, writes watermarks).
        // A finishes for good only once it holds B's finished line naming the board's integration, written
        // in B's current run, with every line of B before it (a gap in B's lines keeps it settling).
        let b_lines = |file: &[Event], notes: &[&str], at: i64| {
            let ls = notes.iter().map(|n| Event { node: "B".into(), ..Event::new(Ev::Heartbeat, "", "runner", at).with_note(n) }).collect();
            landing(file, ls)
        };
        let mut lines = vec![Event::open(task("t1"), "engineer:e", 0)];
        lines.extend(b_lines(&lines, &[JOIN, &roster_note(&["d1".to_string()])], 1));
        lines.extend([ev(Ev::Claim, "t1", "sa", 2), ev(Ev::Done, "t1", "sa", 3)]);
        lines.extend([Event::open(verify("t1", "sa", 3), "sa", 3), ev(Ev::Claim, "t1.v1", "sv", 4), ev(Ev::Verified, "t1.v1", "sv", 5)]);
        lines.extend([Event::open(Task { role: Role::Integrate, ..task("integrate.1") }, "si", 6), ev(Ev::Claim, "integrate.1", "si", 6)]);
        lines.push(Event { head: "c0".into(), ..ev(Ev::Verified, "integrate.1", "si", 7) });
        let now = 8;
        let b = fold_on(&lines, "A");
        assert!(b.finished() && !b.finished_for_good("A", now), "B wrote no finished line: {:?}", b.settling("A", now));
        // B's line before its finished line (a roster): the gap. A beat is none (phase g: beats are kept
        // newest-only, so no `seq` counts them, `landing`).
        let beat = b_lines(&lines, &[&roster_note(&["d1".to_string()])], 8);
        let fin = b_lines(&[lines.clone(), beat.clone()].concat(), &[&finished_note("integrate.1")], 9);
        assert!(fold_on(&[lines.clone(), beat.clone(), fin.clone()].concat(), "A").finished_for_good("A", 10));
        assert!(!fold_on(&[lines.clone(), fin.clone()].concat(), "A").finished_for_good("A", 10), "B's line before it is not here");
        let pulse = b_lines(&lines, &[BEAT], 8);
        let after_pulse = b_lines(&[lines.clone(), pulse].concat(), &[&finished_note("integrate.1")], 9);
        assert!(fold_on(&[lines.clone(), after_pulse].concat(), "A").finished_for_good("A", 10), "a beat of B not held here is no gap");
        let other = b_lines(&[lines.clone(), beat.clone()].concat(), &[&finished_note("integrate.0")], 9);
        assert!(!fold_on(&[lines.clone(), beat.clone(), other].concat(), "A").finished_for_good("A", 10), "another integration");
        let rejoin = b_lines(&[lines.clone(), beat.clone(), fin.clone()].concat(), &[JOIN], 10);
        assert!(!fold_on(&[lines.clone(), beat.clone(), fin.clone(), rejoin].concat(), "A").finished_for_good("A", 11), "a new run of B");
        let left = b_lines(&[lines.clone(), beat.clone()].concat(), &[LEAVE], 9);
        assert!(fold_on(&[lines.clone(), beat, left].concat(), "A").finished_for_good("A", 10), "B left: no participant");
        assert!(b.finished_for_good("A", 1 + SUSPECT_MS + 1), "B suspected: none waited for");
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
        let pick = |allowed: fn(&TaskState) -> bool| next_for(&fold(&lines), Actor::new("A", "d1", "A1"), "", 5, allowed).map(|s| s.task.id.clone());
        assert_eq!(pick(|_| true), Some(format!("{w}.v1")), "B's d1 may not verify its own work");
        assert_eq!(pick(|s| s.task.role != Role::Verify), None, "b is B's d1's");
    }

    #[test]
    fn one_node_takes_the_oldest_first_and_two_keep_the_role_order() {
        // One node owns every task: the order of before, oldest then id, drone after drone.
        let lines: Vec<Event> = ["c", "a", "b"].iter().enumerate().map(|(i, t)| on(Event::open(task(t), "engineer:e", 5 - i as i64), "A")).collect();
        let mut lines = [lines, vec![on(Event::open(task("d"), "engineer:e", 3), "A")]].concat();
        let mut picks = Vec::new();
        while let Some(t) = next_for(&fold(&lines), Actor::new("A", "", "s"), "", 100, |_| true).map(|s| s.task.id.clone()) {
            lines.push(on(ev(Ev::Claim, &t, &format!("s{t}"), 100), "A"));
            picks.push(t);
        }
        assert_eq!(picks, ["b", "d", "a", "c"]);
        // A node that wrote nothing within a lease takes no share: B's line is older than LEASE_MS. Nor
        // does one whose only lines are an engineer's (E): it may run no drone.
        let b = fold(&[on(Event::open(task("x"), "engineer:e", 0), "E"), on(ev(Ev::Progress, "x", "sb", 0), "B")]);
        assert_eq!(b.participants("A", LEASE_MS + 1), BTreeSet::from(["A"]));
        assert_eq!(b.participants("A", LEASE_MS), BTreeSet::from(["A", "B"]));
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
        assert_eq!(next_for(&fold(&lines), Actor::new("A", "", "sa"), "", 3, |_| true).map(|s| s.task.id.clone()), Some(format!("{v}.v1")));
    }

    /// A seeded random board for `every_scheduling_predicate_is_derived_from_eligible` (no dependency:
    /// splitmix64). Four work tasks (`t2` depending on `t1` in some boards, `g.g1` and `g.g2` one group),
    /// then 4 to 27 lines at random gaps up to half a lease: claims (some naming a renewal they take
    /// over), heartbeats, dones (with their verify task's open, barred to the author's session and drone,
    /// some written under a claim of theirs), verdicts, releases and runners' joins. Writers are sessions
    /// of a small pool on nodes A, B and none (a line from before nodes were stamped), so names collide
    /// across nodes, as two nodes' integrators' did; each session is one drone's (`DRONES`) wherever it runs.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            (z ^ (z >> 31)) % n
        }

        fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
            xs[self.below(xs.len() as u64) as usize]
        }
    }

    const NODES: [&str; 3] = ["A", "B", ""];
    const SESSIONS: [&str; 4] = ["s1", "s2", "s3", "integrate-d1-7"];
    const WORK: [&str; 4] = ["t1", "t2", "g.g1", "g.g2"];

    fn drone_of(session: &str) -> &'static str {
        match session {
            "s1" | "integrate-d1-7" => "d1",
            "s2" => "d2",
            _ => "",
        }
    }

    fn random_board(seed: u64) -> Vec<Event> {
        let mut r = Rng(seed);
        let mut t2 = task("t2");
        if r.below(2) == 0 {
            t2.deps = vec!["t1".into()];
        }
        let member = |id: &str| Task { group: Some("g".into()), ..task(id) };
        let mut lines: Vec<Event> = [task("t1"), t2, member("g.g1"), member("g.g2")].into_iter().map(|t| on(Event::open(t, "engineer:e", 0), "A")).collect();
        let tasks: Vec<String> = WORK.iter().flat_map(|w| [w.to_string(), format!("{w}.v1")]).collect();
        let mut claims: Vec<(String, String, String, i64)> = Vec::new();
        let mut ms = 0;
        for _ in 0..4 + r.below(24) {
            ms += 1 + r.below((LEASE_MS / 2) as u64) as i64;
            let (node, by) = (r.pick(&NODES), r.pick(&SESSIONS));
            let drone = drone_of(by);
            let t = tasks[r.below(tasks.len() as u64) as usize].as_str();
            let under = |claims: &[(String, String, String, i64)], t: &str| {
                claims.iter().rev().find(|c| (c.0.as_str(), c.1.as_str(), c.2.as_str()) == (node, by, t)).map(|c| c.3)
            };
            let written: Vec<Event> = match r.below(8) {
                0..=2 => {
                    claims.push((node.into(), by.into(), t.into(), ms));
                    let takes = (r.below(3) == 0).then(|| ms - 1 - r.below(LEASE_MS as u64) as i64);
                    vec![Event { takes, ..ev(Ev::Claim, t, by, ms).with_drone(drone) }]
                }
                3 => vec![ev(Ev::Heartbeat, t, by, ms)],
                4 => {
                    let w = WORK[r.below(4) as usize];
                    let claim = under(&claims, w).filter(|_| r.below(2) == 0);
                    let v = Task { id: format!("{w}.v1"), role: Role::Verify, of: Some(w.into()), not_by: vec![by.into(), drone.into()], ..task("x") };
                    vec![Event { head: format!("h{ms}"), claim, ..ev(Ev::Done, w, by, ms) }, Event::open(v, by, ms)]
                }
                5 => {
                    let v = format!("{}.v1", WORK[r.below(4) as usize]);
                    let claim = under(&claims, &v).filter(|_| r.below(2) == 0);
                    let kind = if r.below(3) == 0 { Ev::Rejected } else { Ev::Verified };
                    vec![Event { claim, ..ev(kind, &v, by, ms) }]
                }
                6 => vec![ev(Ev::Released, t, by, ms)],
                _ if node.is_empty() => vec![],
                _ => vec![ev(Ev::Heartbeat, "", "runner", ms).with_note(JOIN)],
            };
            lines.extend(written.into_iter().map(|e| on(e, node)));
        }
        lines
    }

    /// Phase f F8 (G6): over 250 random boards (`random_board`), as each of A and B folds them, at four
    /// moments up to three leases past the last line, every scheduling predicate is `eligible` or a
    /// narrowing of it, for every actor of the pool and a fresh session:
    /// - `claimable_by` equals it at `now`;
    /// - the fold takes a claim line exactly when its writer holds the task (a renewal) or is eligible on
    ///   the line's stamp (`At::Line`): `apply` refolded with the line appended, for any `takes`;
    /// - a claim `claimable_for` allows (hive/mod.rs, the check under the lock) and written as `claim`
    ///   writes it (`claim_at`, `takes`, stamped above every line) is one the fold takes;
    /// - `next_for` and `free_for` give only tasks `open_for` the actor, and something whenever one is;
    ///   run.rs `Offer` offers exactly those, `free_for`'s first;
    /// - the verdict's author check (`judges_own`, on the attempt its open was written with) is the bar.
    ///
    /// Fails with any of them read apart from `eligible` again: the fold's claim rule without the group
    /// rival or a dep, `claimable_for` on the lease alone (an unmet dep written and lost), a holder
    /// compared by session without its node (two nodes' `integrate-d1-7` renewing each other's claim).
    #[test]
    fn every_scheduling_predicate_is_derived_from_eligible() {
        let mut fresh = 0u64;
        for seed in 0..250u64 {
            let lines = random_board(seed);
            let last = lines.iter().map(|e| e.ms).max().unwrap_or(0);
            let mut r = Rng(seed ^ 0x5eed);
            for here in ["A", "B"] {
                let b = fold_on(&lines, here);
                let allowed = |s: &TaskState| s.task.id != "t2";
                for now in [last, last + LEASE_MS / 2, last + LEASE_MS + 1, last + 3 * LEASE_MS] {
                    for node in NODES {
                        for session in SESSIONS.iter().copied().chain(["fresh", ""]) {
                            let who = Actor::new(node, if session.is_empty() { "d2" } else { drone_of(session) }, session);
                            let ctx = || format!("seed {seed}, {here} at {now}, {who:?}");
                            let open: Vec<&str> = b.tasks.values().filter(|s| open_for(&b, s, who, now, allowed)).map(|s| s.task.id.as_str()).collect();
                            for s in b.tasks.values() {
                                let e = eligible(&b, s, who, At::Now(now));
                                assert_eq!(claimable_by(&b, s, who, now), e, "{}: {}", ctx(), s.task.id);
                                assert!(!open.contains(&s.task.id.as_str()) || e, "{}: {} open but not eligible", ctx(), s.task.id);
                                if let (Some((by, done)), Some(w)) = (&s.of_attempt, s.task.of.as_ref().and_then(|w| b.tasks.get(w))) {
                                    let a = w.attempts.iter().find(|a| (&a.by, a.done) == (by, *done)).unwrap();
                                    let judged = (a.node.as_str(), a.by.as_str(), b.drone(&a.node, &a.by).unwrap_or_default());
                                    assert_eq!(b.judges_own(s, who, judged), b.barred(s, who), "{}: {}", ctx(), s.task.id);
                                }
                                let allows = crate::hive::claimable_for(&b, &s.task.id, who, false, now);
                                assert_eq!(allows, s.held_by(node, session, now) || e, "{}: {}", ctx(), s.task.id);
                                if session.is_empty() || r.below(if allows { 6 } else { 24 }) != 0 {
                                    continue;
                                }
                                // The line as hive/mod.rs `claim` writes it, then one at a random later stamp and takeover.
                                let as_written = (claim_at(s, now).max(last + 1), takes(s, now));
                                let random = (
                                    last + 1 + r.below(3 * LEASE_MS as u64) as i64,
                                    [None, takes(s, now), Some(r.below(last as u64 + 1) as i64)][r.below(3) as usize],
                                );
                                for (i, (ms, taking)) in [as_written, random].into_iter().enumerate() {
                                    let c = Event { takes: taking, node: node.into(), ..ev(Ev::Claim, &s.task.id, session, ms).with_drone(who.drone) };
                                    let refolded = fold_on(&[lines.clone(), vec![c.clone()]].concat(), here);
                                    let taken = !refolded.lost.contains(&(session.to_string(), node.to_string(), s.task.id.clone(), ms));
                                    let renews = matches!(&s.status, Status::Claimed { by, node: n, .. } if Actor::of(&c).is(n, by));
                                    assert_eq!(
                                        taken,
                                        renews || eligible(&b, s, Actor::of(&c), At::Line { ms, takes: taking }),
                                        "{}: {} claimed at {ms}",
                                        ctx(),
                                        s.task.id
                                    );
                                    assert!(i > 0 || !allows || taken, "{}: {} allowed under the lock, refused by the fold", ctx(), s.task.id);
                                }
                                fresh += 1;
                            }
                            if session.is_empty() {
                                let o = super::super::run::Offer::of(&b, who.drone, node, "", now, allowed);
                                assert_eq!(o.open.keys().map(String::as_str).collect::<Vec<_>>(), open, "{}", ctx());
                                assert_eq!(o.task.map(|(t, _)| t), free_for(&b, who, "", now, allowed).map(|s| s.task.id.clone()), "{}", ctx());
                            }
                            let next = next_for(&b, who, "", now, allowed).map(|s| s.task.id.as_str());
                            assert_eq!(next.is_some(), !open.is_empty(), "{}: next {next:?}, open {open:?}", ctx());
                            assert!(next.is_none_or(|t| open.contains(&t)), "{}: next {next:?} not open", ctx());
                            let free = free_for(&b, who, "", now, allowed).map(|s| s.task.id.as_str());
                            assert!(free.is_none_or(|t| open.contains(&t)), "{}: free {free:?} not open", ctx());
                        }
                    }
                }
            }
        }
        assert!(fresh > 2000, "only {fresh} claims refolded");
    }
}
