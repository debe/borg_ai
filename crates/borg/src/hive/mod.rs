//! The hive: many drones, one goal (docs/design/hive.md). File-backed under `<store>/hive/<goal>/`;
//! the logs in [`SYNCED`] travel over the mesh as signed lines like lessons (borg-mesh lines.rs
//! `hive_logs`), so drones on several machines work one board (hive.md C4).
//!
//! - `goal.json` (goal.rs): the spec, the oracle checks that gate `done`, the budget, the repos; it
//!   syncs as its projection in `goal.jsonl` (`publish`), without the hidden oracle.
//! - `tasks.jsonl` (board.rs): the board's events; `board::fold` reads them the same on every node.
//!   Claims are leases renewed by the drone's hooks; the lowest (ms, blake3 key) claim wins a race.
//!   Every line this node writes carries its node id (`write`): a claim from another node is never
//!   this runner's to release.
//! - `contracts.jsonl` (contract.rs): what drones promise each other, shown once per session.
//! - `messages.jsonl` (message.rs): `borg hive ask` and `tell` between drones, shown once per session.
//! - `bundles.jsonl` (commits.rs): each attempt's commits as a git bundle, from a store in a mesh, so a
//!   node on another machine verifies, builds on and integrates work it did not do.
//! - `oracle/` (oracle.rs): the tails of failed oracle runs.
//! - `flags.jsonl` (guard.rs): what drones did to tests, CI or the oracle's files, flagged at each tool
//!   call; `oracle.jsonl` (hidden.rs): the hidden oracle's score of each judged attempt (runner only).
//!   verdict.rs turns a verifier's verdict into the reward: a diff of the claim's snapshots that
//!   weakened tests or changed CI or check files makes it `hacked`, and the lessons shown during the
//!   claim are credited.
//! - `joins.jsonl`: directories whose sessions join the goal (`borg hive join --here`, and each
//!   worktree `borg debug hive run` makes, with the session of each iteration, left when the run ends).
//! - `spend.jsonl`, `runs.jsonl` (run.rs): each headless iteration's cost (synced, naming its node:
//!   every node's runner reads the goal's one budget), each `borg debug hive run` (local).
//! - `confirms.jsonl` (`claim`): each claim's confirm window across nodes, its wait and outcome, and each
//!   claim refused before its write (local).
//! - `logs/` (run.rs): each iteration's output, local only: it never syncs.
//!
//! `borg debug hive run` (run.rs) starts N headless drones, each in a git worktree of its own, until the
//! board is finished or their budget share spent; its drone threads integrate the accepted work and run
//! the oracle on the combined tree (integrate.rs, also `borg hive integrate`); view.rs is the hive in
//! `borg top` and borg-web.
//!
//! A session joins a goal by `borg hive join`, by `BORG_HIVE=<goal>` at its first hook, or by a join
//! record of the directory it starts in; the membership is `<BORG_STATE>/<session>.hive` (`HiveSession`). The hook's [`around`] then adds the
//! drone's claim, the board's changes and the contracts in scope at a prompt, contracts and scope
//! warnings after an edit, and at the stop runs the oracle: a failing one blocks the stop (at most
//! `MAX_ORACLE_BLOCKS` times per claim), a passing one publishes `done`, opens its verify task and hands
//! the drone the next open task. A session in no goal costs one failed file open and sees no change.

pub mod board;
pub mod commits;
pub mod contract;
mod git;
pub mod goal;
pub mod group;
pub mod guard;
pub mod hidden;
pub mod integrate;
pub mod interactive;
pub mod message;
pub mod meter;
pub mod monitor;
pub mod node;
pub mod oracle;
pub mod plan;
pub mod reexec;
pub mod rework;
pub mod run;
#[cfg(test)]
mod scenario;
mod scope;
mod sim_drone;
mod start;
pub mod verdict;
pub mod view;
mod wait;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use board::{Board, Ev, Event, Role, Span, Status, Task, TaskState};
pub use contract::{Contract, Scope};
pub use goal::{Check, Goal, RepoScope};

use crate::harness::Harness;
use crate::{collective, hook, learn, presence, queen, stats, steer, top, veto};

/// "t1", or "t1.v1" for the first verify task of t1.
pub type TaskId = String;

/// A drone: its node (empty: this machine, or a line from before nodes were stamped; else the store's
/// `stats::node`) and session.
/// Lines carry it flat, as `node` and `by`, so they stay syncable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DroneId {
    pub node: String,
    pub session: String,
}

/// The goal a drone is launched into (the hive runner sets it on each drone): its first hook joins it.
pub const ENV: &str = "BORG_HIVE";
/// The drone a launched session is one iteration of (hive/run.rs): its claims carry it, so a verify
/// task barred to the author drone is barred to all its sessions.
pub const DRONE_ENV: &str = "BORG_HIVE_DRONE";
/// A claim expires when its drone went idle (presence.rs).
pub const LEASE_MS: i64 = presence::IDLE_MS;
/// How often the hooks renew a held claim.
pub const HEARTBEAT_MS: i64 = LEASE_MS / 3;
/// `HEARTBEAT_MS` as a duration (`renewing`).
pub(crate) const HEARTBEAT: Duration = Duration::from_millis(HEARTBEAT_MS as u64);
/// Stop blocks per claim while the oracle fails; then the hive lets the drone stop and publishes the
/// task blocked.
pub const MAX_ORACLE_BLOCKS: u32 = 3;
/// Board changes a prompt shows at most (the newest).
pub const SHOW_CHANGES: usize = 12;
/// The stop's time kept for the conscience and the experts after the oracle.
pub const CONSCIENCE_RESERVE: Duration = Duration::from_secs(20);
/// Less stop budget than this left: the oracle does not start (it reports a timeout).
const MIN_ORACLE: Duration = Duration::from_secs(2);
/// What the stop hook says of a check its time ran out on (the drone is told to run `borg hive done`).
pub(crate) const HOOK_TIMED_OUT: &str = "did not finish within the stop hook's time";
/// How long a claim beside a participant that writes no watermark (a node from before phase f F5), with
/// no peer reachable, waits before reading the board back (`across`): a line of a peer that dials in at
/// nearly the same moment has landed by then. A timer, so no exclusion: at 0 two nodes' claims can both
/// read back won, as they can past it (a participant that writes watermarks makes claims final with
/// none; the monotone results make either safe).
const CROSS_SETTLE: Duration = Duration::from_millis(150);
/// One sync round with a peer on a busy machine: the mesh daemon's look at the hive's logs (borg-mesh
/// daemon.rs `hive_poll`, 25 ms) and one session (phase e E3: p95 44 ms quiet, a few hundred under
/// load). The waits for a round below are capped by it.
const ROUND: Duration = Duration::from_millis(500);
/// A claim's confirm window on a participant that writes no watermark (`across`, a node from before
/// phase f F5): two rounds, as one may be running when the claim lands and the next carries it. A
/// liveness bound only, where no failure detector reads that peer: no claim beside a participant that
/// writes watermarks reads it (it waits until final or the peer is suspected). Also a round's way in the
/// grace and suspicion windows below.
pub(crate) const CONFIRM_CAP: Duration = ROUND.saturating_mul(2);
/// How long another node's task is left to its owner's idle drones that started no session (board.rs
/// `left_to_owner`) after it became claimable: the owner's idle runner looks at the board within
/// `IDLE_WAIT` (run.rs; at once on a change of the board) and starts a session, which by the end of
/// that time has claimed or says on the board that it started (`session`, run.rs `run_session`), a line
/// that reaches here within `CONFIRM_CAP` and one more round. From then the task is left while that
/// session has not claimed; without a start by then, the owner is not taking it: an idle drone of this
/// node may.
pub(crate) const OWNER_GRACE_MS: i64 = (run::IDLE_WAIT.as_millis() + CONFIRM_CAP.as_millis() + ROUND.as_millis()) as i64;
/// How often a running runner says on the board that its node still takes part in the goal (a goal
/// heartbeat, `beat`; phase f F4): as often as an idle drone of it looks at the board (`run::IDLE_WAIT`).
/// A node's liveness is then known in seconds, apart from the work lease (`LEASE_MS`), which only a
/// session's own hooks renew (Kafka KIP-62: the member's liveness apart from its work's timeout).
pub(crate) const BEAT_MS: i64 = run::IDLE_WAIT.as_millis() as i64;
/// A goal participant from which this node held no new line for longer, on its own clock, is suspected
/// (board.rs `Board::participants`, `Local::gone`): three beats missed, and the last one's way here
/// (`CONFIRM_CAP`). Safety reads no timer: at 0 every peer is suspected at once and its runner drones'
/// leases may be taken over (a duplicate attempt, both dones kept since phase f F5), and every claim
/// proceeds unconfirmed, unbounded no node is ever suspected and only the lease frees a task, as before F4.
/// The window bounds a beat's way on a healthy mesh only, not the transport's stalls: a dial may take
/// up to 30 s and a peer that failed two dials in a row is dialled again only 2 min later (borg-mesh
/// daemon.rs `timeout`, `backoff_wait`), and on the observer's side a mesh daemon down or backing off,
/// or a machine just resumed, holds nothing new of any peer. Each such stall past the window is a false
/// suspicion: the peer's live claims taken over (a duplicate attempt, its done kept since F5) and, with
/// every peer suspected, a claim that proceeds before it is final (`across`). Safe, as above; the cost is work done twice (scenario.rs
/// `a_cut_on_the_observers_side_costs_at_most_a_duplicate_attempt`). Telling "the peer is silent" from
/// "this node receives nothing" (suspecting only while this node's rounds with the mesh complete, as
/// SWIM's indirect probes do) needs the transport's round records in the fold: left for later.
pub(crate) const SUSPECT_MS: i64 = 3 * BEAT_MS + CONFIRM_CAP.as_millis() as i64;
/// A run's wait for its announcement to reach the peers (`announce`): its first round may carry more
/// than a claim (the goal's bundles), and it is spent once per run.
const START_CAP: Duration = ROUND.saturating_mul(6);
/// A member whose last completed sync session started longer ago than this is away: the mesh daemon
/// syncs every peer at least once a minute (borg-mesh daemon.rs `interval`) and at once on a hive
/// change, so no wait is spent on one not heard from in two.
const PEER_HEARD_MS: i64 = 2 * 60_000;
/// Each git call of a fix task's worktree reset.
const RESET_TIMEOUT: Duration = Duration::from_secs(60);
/// Claims `borg hive claim` without a task tries before it gives up on a busy board.
const CLAIM_TRIES: usize = 5;

/// The `.ctx` markers of what a session was shown (hook.rs `listed`): contracts, lost claims, files
/// outside the claim's scope; the claim last described (`task@since`, `-` for none); tasks refused to
/// it; the out-of-scope repo warning.
pub const CONTRACT_MARKER: &str = "#borg-hive-contract ";
const LOST_MARKER: &str = "#borg-hive-lost ";
const SCOPE_MARKER: &str = "#borg-hive-scope ";
const TASK_MARKER: &str = "#borg-hive-task ";
/// Tasks whose start this session's tree could not take (`Refused::Deps`: a dep without verified
/// work, a checkout off the start): never offered or handed to it again; `borg hive claim <task>`
/// still tries.
const REFUSED_MARKER: &str = "#borg-hive-refused ";
const REPO_MARKER: &str = "#borg-hive-repo";
/// Each claim whose read-back saw it won, with a tree (`claim`): `task@ms sha top`, the tree it started
/// from (its base, the start; `-` for a verify task) and the git top it works in. A lost claim is set
/// aside to that tree (`set_aside`); one its own read-back saw lost has none: it touched no tree.
const CLAIM_START_MARKER: &str = "#borg-hive-claim-start ";
/// Lost claims (`task@ms`) set aside, each once.
const SET_ASIDE_MARKER: &str = "#borg-hive-set-aside ";

/// Said once, at a drone's first hive prompt (docs/design/hive.md, inoculation framing).
pub const INOCULATION: &str = "The oracle gates done: your task counts as done only when its checks pass, and a verifier drone checks the work after you. \
Every change to tests, CI config or oracle files is recorded and shown to the verifier; gaming a check is visible, allowed to happen, and useless: \
the task counts as failed. Adding tests, also inside an existing test file, is fine; outside your task's files an existing test may change only in names and values, \
never lose a line, an assertion or gain a skip. If a check or the plan looks wrong, say so in your done note instead of changing it, \
and tell the planner: `borg hive tell \"<what is wrong>\" --to planner`.";
/// Added for a goal with a hidden oracle.
pub const INOCULATION_HIDDEN: &str = "This goal is also scored by held-out checks you cannot see.";

/// What a drone of `g` is told once (`INOCULATION`).
pub fn inoculation(g: &Goal) -> String {
    match goal::has_hidden(g) {
        true => format!("{INOCULATION_HIDDEN} {INOCULATION}"),
        false => INOCULATION.into(),
    }
}

/// A goal's logs that sync over the mesh (borg-mesh lines.rs `hive_logs`), in the order a session
/// reconciles them. Never synced: goal.json (it may hold the hidden oracle; `goal.jsonl` is its
/// projection), oracle.jsonl (the hidden scores), joins.jsonl, runs.jsonl, confirms.jsonl (this node's
/// worktrees, runs and claim confirms), logs/ and scratch/. flags.jsonl does: a verifier on another node reads the author's
/// oracle-file flags from it (verdict.rs `record`), or a hack that touched the oracle's files would
/// pass there. bundles.jsonl (commits.rs) carries each attempt's commits to the nodes that verify,
/// build on or integrate it elsewhere. spend.jsonl (run.rs `ledger`, `borg/mesh/10`), each line naming
/// its node: every node's runner splits and stops on the goal's one budget.
pub const SYNCED: [&str; 7] = ["goal.jsonl", "tasks.jsonl", "contracts.jsonl", "messages.jsonl", "flags.jsonl", commits::FILE, run::SPEND_FILE];

/// This store's node id: the self-declared id outcomes and the stats rollups carry (stats.rs `node`),
/// stamped on every hive line this node writes. Not the member key: lines are bound to a key by their
/// mesh origin; this id only says "mine" and is shown.
pub fn node(home: &Path) -> String {
    stats::node(home)
}

/// Where a test puts the git repos whose calls run under a claim's or verdict's time: tmpfs
/// (`/dev/shm`) when there is one and `TMPDIR` names no other place, else the temp dir. On disk, a
/// machine's heavy writes beside the tests (builds) throttle git's index and object writes for tens of
/// seconds, past the time each call is given, and the test fails on the machine's load, not the code.
#[cfg(test)]
pub(crate) fn test_tmp() -> PathBuf {
    let shm = Path::new("/dev/shm").join("borg-tests");
    if std::env::var_os("TMPDIR").is_none() && std::fs::create_dir_all(&shm).is_ok() { shm } else { std::env::temp_dir() }
}

/// Whether a line's `node` is this store's: empty (a line from before nodes were stamped) or its id.
pub fn is_local(home: &Path, node: &str) -> bool {
    node.is_empty() || node == self::node(home)
}

/// Appends `events` to `goal`'s board in one write, each without a node stamped with this store's
/// (`node`): every line a drone, runner or engineer of this node writes. `board::append` stays raw.
/// The checks of a task this node opens are approved here (oracle.rs `Approvals`): it wrote them. Each
/// line is stamped under the board file's lock (`land`): the lines as they landed.
pub(crate) fn write(home: &Path, goal: &str, events: Vec<Event>) -> std::io::Result<Vec<Event>> {
    let path = tasks_path(home, goal);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::live::locked(&path, || land(home, goal, &path, &board::read(&path), events))
}

/// Appends what `decide` makes of `goal`'s board as it stands, as `write` does, with the board read,
/// `decide` run and its lines appended under the board file's lock (`append_bytes`): a check-and-append.
/// Every line this node writes lands through that lock, so none lands between what `decide` read and
/// its lines, however long either takes: a decision it makes of the board (a task is claimable for
/// this session) still holds when its lines land, a compare-and-swap with no timing assumption. The
/// lines of other nodes the mesh merges (borg-mesh lines.rs) take no such lock: across nodes this is
/// no exclusion (`across`). `decide` only reads: it runs under the lock (one fold, no git, network or
/// write of this board). The lines as they landed, stamped (`land`; `decide` None: nothing written).
/// When the lock cannot be taken nothing is decided or written and the error is returned (live.rs
/// `try_locked`): a check without the lock would be the race this removes.
pub(crate) fn write_if(home: &Path, goal: &str, decide: impl FnOnce(&[Event]) -> Option<Vec<Event>>) -> std::io::Result<Option<Vec<Event>>> {
    let path = tasks_path(home, goal);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::live::try_locked(&path, || {
        let file = board::read(&path);
        let Some(events) = decide(&file) else { return Ok(None) };
        land(home, goal, &path, &file, events).map(Some)
    })?
}

/// Under the board file's lock, holding `file` (its lines): `events` stamped (`stamped`, and one ms for
/// the write as a hybrid logical clock, `board::landing`, each ms the wall-clock reading it was made at,
/// with its `seq`, the watermark's count) and appended, then the receive index told (`board::note_held`). Stamped here, under the lock, no line of
/// this node lands below one already in its file, and every one is above each line it was decided on:
/// a heartbeat that lands after a claim written meanwhile sorts after it, and renews nothing.
fn land(home: &Path, goal: &str, path: &Path, file: &[Event], events: Vec<Event>) -> std::io::Result<Vec<Event>> {
    let events = board::landing(file, stamped(home, goal, events)?);
    board::append_held(path, &events)?;
    board::note_held(path);
    Ok(events)
}

/// `events` with this store's node on each without one (`node`), the checks of the tasks they open
/// approved here (`write`).
fn stamped(home: &Path, goal: &str, mut events: Vec<Event>) -> std::io::Result<Vec<Event>> {
    let here = node(home);
    for e in events.iter_mut().filter(|e| e.node.is_empty()) {
        e.node = here.clone();
    }
    let opened: Vec<Check> = events.iter().filter(|e| e.ev == Ev::Open).filter_map(|e| e.def.as_ref()).flat_map(|t| t.accept.clone()).collect();
    if !opened.is_empty() {
        oracle::approve(home, goal, &opened, learn::now_ms())?;
    }
    Ok(events)
}

/// Appends to goal.jsonl the projection of every goal made here (with a goal.json) that it lacks:
/// goals made before they synced need nothing done. Called before each mesh round (borg-mesh
/// `Node::stamp`). Returns how many lines it wrote.
pub fn publish(home: &Path) -> usize {
    let mut n = 0;
    for id in goal::list(home) {
        let Some(g) = goal::local(home, &id) else { continue };
        let line = goal::projection(&g);
        let path = goal_jsonl_path(home, &id);
        if !line.is_empty() && !std::fs::read_to_string(&path).unwrap_or_default().lines().any(|l| l == line) && append_raw(&path, &line).is_ok() {
            n += 1;
        }
    }
    n
}

fn append_raw(path: &Path, line: &str) -> std::io::Result<()> {
    append_bytes(path, format!("{line}\n").as_bytes())
}

/// Appends `body` to `path` in one O_APPEND write, under the file's lock (live.rs `locked`): a
/// quarantine (borg-mesh origin.rs) rewrites a synced log by a rename under the same lock, so a line
/// written meanwhile is never lost. Every hive log is written through here, so no caller may hold the
/// same file's lock (a second lock in one process waits for itself): one that does appends with
/// `append_held` (`write_if`).
pub(crate) fn append_bytes(path: &Path, body: &[u8]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::live::locked(path, || append_held(path, body))
}

/// `append_bytes` by a caller that holds `path`'s lock.
pub(crate) fn append_held(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(body)
}

pub fn dir(home: &Path, goal: &str) -> PathBuf {
    home.join("hive").join(goal)
}

pub fn goal_path(home: &Path, goal: &str) -> PathBuf {
    dir(home, goal).join("goal.json")
}

/// The goal as it syncs (goal.rs `projection`).
pub fn goal_jsonl_path(home: &Path, goal: &str) -> PathBuf {
    dir(home, goal).join("goal.jsonl")
}

pub fn messages_path(home: &Path, goal: &str) -> PathBuf {
    dir(home, goal).join("messages.jsonl")
}

pub fn tasks_path(home: &Path, goal: &str) -> PathBuf {
    dir(home, goal).join("tasks.jsonl")
}

pub fn contracts_path(home: &Path, goal: &str) -> PathBuf {
    dir(home, goal).join("contracts.jsonl")
}

pub fn joins_path(home: &Path, goal: &str) -> PathBuf {
    dir(home, goal).join("joins.jsonl")
}

/// A session's membership: `<BORG_STATE>/<session>.hive`.
pub fn session_path(session: &str) -> PathBuf {
    hook::state_file(session).with_extension("hive")
}

/// Hive text a harness could not take at a prompt or tool call: delivered at the stop.
fn wait_path(session: &str) -> PathBuf {
    hook::state_file(session).with_extension("hive-wait")
}

/// A session's place in its goal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiveSession {
    pub goal: String,
    /// The tasks.jsonl offset up to which board changes were shown.
    #[serde(default)]
    pub cursor: u64,
    /// When the session last renewed its claim.
    #[serde(default)]
    pub beat_ms: i64,
    /// Stops blocked in a row by a failing oracle, for the claim `blocks_for`.
    #[serde(default)]
    pub oracle_blocks: u32,
    /// The claim the counter counts for (`task@since`): a new claim starts it again.
    #[serde(default)]
    pub blocks_for: String,
    /// The drone this session is an iteration of (`DRONE_ENV`); empty for a session of its own. An
    /// interactive drone's own name (interactive.rs `drone_name`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub drone: String,
    /// A session the engineer made a drone from inside it (`borg hive join`, phase g, interactive.rs):
    /// its stop asks for a wait when nothing is in hand, and it never runs the engineer's commands.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interactive: bool,
    /// It serves every open goal of the collective in its repo; `goal` is the one it works on now.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub any: bool,
    /// Its own worktree: where its hooks claim, snapshot and run the oracle, whatever directory a hook
    /// event names (phase g correction 14). Empty: the event's directory.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub worktree: String,
    /// When its hooks last beat for its node on `goal` (interactive.rs `touch`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub member_ms: i64,
    /// Its stop asked it to start a wait this turn (interactive.rs `after_stop`): once a turn.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub asked_wait: bool,
    /// The harness process its registry line names (interactive.rs `harness_pid`, or the pid its
    /// harness's plugin sends, `own_pid`); 0 when none was found.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub pid: u32,
    /// The transcript its harness's hooks name (Claude Code's `transcript_path`), for what a claim cost
    /// when it ends outside a hook (`borg hive release`, meter.rs); empty when none named one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub transcript: String,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// The session's goal; None when it joined none, or on any error (fail open).
pub fn bound(session: &str) -> Option<HiveSession> {
    serde_json::from_slice(&std::fs::read(session_path(session)).ok()?).ok()
}

/// The drone a session of membership `hs` is an iteration of: its own, else the runner's
/// (`DRONE_ENV`, inherited by every hook and CLI call in the drone's process tree), else none. One value
/// for the offer, the pre-claim check and the claim line, so a check never passes what the fold voids.
pub fn drone_for(hs: &HiveSession) -> String {
    if !hs.drone.is_empty() {
        return hs.drone.clone();
    }
    std::env::var(DRONE_ENV).map(|d| d.trim().to_string()).unwrap_or_default()
}

/// The drone `session` is an iteration of (`drone_for`), what its claims carry and its bars name.
fn drone_of(session: &str) -> String {
    drone_for(&bound(session).unwrap_or_default())
}

fn save(session: &str, hs: &HiveSession) {
    let _ = steer::rewrite(&session_path(session), &serde_json::to_string(hs).unwrap_or_default());
}

/// Joins `session` to `goal` of the store `home`. Changes from before the join are not news to it: the
/// board shows them all (`borg hive board`).
///
/// # Errors
/// When the membership cannot be written.
pub fn bind(home: &Path, session: &str, goal: &str) -> std::io::Result<()> {
    bind_as(home, session, goal, "")
}

fn bind_as(home: &Path, session: &str, goal: &str, drone: &str) -> std::io::Result<()> {
    let cursor = std::fs::metadata(tasks_path(home, goal)).map_or(0, |m| m.len());
    let hs = HiveSession { goal: goal.into(), cursor, drone: drone.into(), ..HiveSession::default() };
    steer::rewrite(&session_path(session), &serde_json::to_string(&hs).unwrap_or_default())
}

pub fn unbind(session: &str) {
    let _ = std::fs::remove_file(session_path(session));
}

/// The hook's join: a session launched with `BORG_HIVE=<goal>` joins that goal of the routed store at
/// its first hook, if the goal exists (anything else is ignored); without the variable, the latest
/// join record of a directory `cwd` is in decides (`joined`): a harness whose sessions drop the env
/// (opencode 2.x's background service) still joins its worktree's goal. `BORG_HIVE=` (empty) joins
/// nothing, records or not (the oracle's children). A store with no hive costs one failed stat.
pub fn bind_from_env(home: &Path, session: &str, cwd: &str) {
    // A session bound already reads no join record.
    if session_path(session).exists() {
        return;
    }
    let (goal, drone) = match std::env::var(ENV) {
        Ok(g) => (g.trim().to_string(), std::env::var(DRONE_ENV).unwrap_or_default()),
        Err(_) => match joined(home, cwd) {
            Some(j) => (j.goal, j.drone),
            None => return,
        },
    };
    if goal::exists(home, &goal) {
        let _ = bind_as(home, session, &goal, drone.trim());
    }
}

/// One line of joins.jsonl: sessions starting in `dir` (a git toplevel or worktree, absolute) join
/// `goal`; `by` is `join` (`borg hive join --here`) or `run` (a worktree of `borg debug hive run`, with the
/// iteration's `session` when the runner chose it). An empty `goal` is a leave: it shadows the joins
/// before it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Join {
    pub v: u8,
    pub ms: i64,
    pub goal: String,
    pub dir: String,
    #[serde(default)]
    pub drone: String,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub harness: String,
    pub by: String,
}

/// Appends `j` to `goal`'s joins.jsonl (one O_APPEND write).
///
/// # Errors
/// When the file cannot be written.
pub fn append_join(home: &Path, goal: &str, j: &Join) -> std::io::Result<()> {
    append_line(&joins_path(home, goal), j)
}

/// One JSON line appended in one write(2) with O_APPEND (`append_bytes`).
pub(crate) fn append_line<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    let line = serde_json::to_string(v).map_err(std::io::Error::other)? + "\n";
    append_bytes(path, line.as_bytes())
}

/// Every parsable line of a JSONL file, in file order (an incomplete or unknown line is skipped).
pub(crate) fn read_lines<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Every join record of the store's goals (one pass over each tiny joins.jsonl); none when the store
/// has no hive (one failed stat).
pub fn joins(home: &Path) -> Vec<Join> {
    if std::fs::metadata(home.join("hive")).is_err() {
        return Vec::new();
    }
    goal::list(home).iter().flat_map(|g| read_lines::<Join>(&joins_path(home, g))).collect()
}

/// The newest join record whose `dir` holds `cwd` (whole path components: `/w/repo2` is not in
/// `/w/repo`), the deepest dir first among equals; None when it is a leave or there is none.
pub fn joined(home: &Path, cwd: &str) -> Option<Join> {
    if cwd.is_empty() {
        return None;
    }
    let all = joins(home);
    if all.is_empty() {
        return None;
    }
    let path = collective::normal(Path::new(cwd));
    // A symlinked path and its target are one directory (macOS's /var is /private/var; `--here`
    // records the resolved cwd, a harness may report the link): resolved only when the text misses.
    let real = std::cell::OnceCell::new();
    let resolved = |dir: &str| -> Option<usize> {
        let real = real.get_or_init(|| std::fs::canonicalize(cwd).ok().map(|p| collective::normal(&p)));
        let dir = std::fs::canonicalize(collective::expand(dir)).ok()?;
        collective::inside(real.as_ref()?, &dir.to_string_lossy())
    };
    let best = all
        .into_iter()
        .filter_map(|j| collective::inside(&path, &j.dir).or_else(|| resolved(&j.dir)).map(|depth| (j.ms, depth, j)))
        .max_by_key(|(ms, depth, _)| (*ms, *depth))?;
    Some(best.2).filter(|j| !j.goal.is_empty())
}

/// Whether a drone in `repo` may claim `t`: never an integrate task (the runner integrates,
/// integrate.rs), and a drone outside the goal's repos only claims tasks of its own repo (or of none).
pub fn may_claim(g: &Goal, t: &Task, repo: &str) -> bool {
    t.role != Role::Integrate && (goal::in_scope(g, repo) || t.repo.is_empty() || t.repo.eq_ignore_ascii_case(repo))
}

/// Why `borg hive open --again` refuses a drone (hive_cli.rs holds it to this text).
pub const AGAIN_REFUSED: &str =
    "a task's definition is amended by the engineer, never by a drone of a hive goal: ask with `borg hive scope --files +a \"why\"` (or `--accept NAME=CMD`)";

/// Why integrate tasks are refused to a drone's claim.
const INTEGRATE_REFUSED: &str = "integrate tasks are run by `borg hive integrate` or `borg debug hive run`";

/// Whether `session` in `cwd` may take `t` (`may_claim`, and a fix task only in a runner worktree or
/// a checkout of its `from`).
fn may_take(g: &Goal, t: &Task, repo: &str, session: &str, cwd: &str) -> bool {
    may_claim(g, t, repo) && t.from.as_deref().is_none_or(|from| own_tree(g, session, cwd) || checkout_of(cwd, from))
}

/// The task `session` of `drone` in `cwd` takes next (board.rs `next_for`): one it `may_take` whose
/// commits are in this repo (`lacking`, the filter run.rs `open_to` applies too), whose checks this node
/// approved (oracle.rs `Approvals`), and not one refused to it before (`REFUSED_MARKER`), which
/// `next_for`'s fixed order would pick again.
///
/// An interactive drone (phase g G5, `gated`) takes only tasks whose plan the engineer approved
/// (board.rs `Board::plan_approved`), and nothing of a goal whose metered spend reached its budget
/// (`spent_out`, which asks the engineer once whether to raise it).
#[allow(clippy::too_many_arguments)]
fn next_open<'a>(home: &Path, g: &Goal, b: &'a Board, session: &str, drone: &str, repo: &str, cwd: &str, now: i64) -> Option<&'a TaskState> {
    let gate = gated(session);
    if gate && let Some((spent, budget)) = spent_out(home, g, b) {
        plan::ask_budget(home, g, spent, budget, cwd, now);
        return None;
    }
    let state = std::fs::read_to_string(hook::state_file(session)).unwrap_or_default();
    let refused = hook::listed(&state, REFUSED_MARKER);
    let here = node(home);
    let approvals = oracle::Approvals::load(home, &g.id);
    let lacks = lacking_here(home, &g.id, b, Path::new(cwd), now);
    board::next_for(b, board::Actor::new(&here, drone, session), repo, now, |s| {
        !refused.contains(s.task.id.as_str())
            && (!gate || b.plan_approved(&s.task))
            && may_take(g, &s.task, repo, session, cwd)
            && approvals.pending(g, &s.task).is_empty()
            && !lacks.contains_key(&s.task.id)
    })
}

/// Whether `session`'s claims wait for the engineer's approval of the plan and stop at the goal's budget
/// (phase g G5): an interactive drone's, whose work nobody approved by starting it. A runner's drones
/// start only once `borg debug hive run`'s own gate approved the plan (plan.rs `gate`), and a plain session's
/// claim is the engineer's own step, both as before.
fn gated(session: &str) -> bool {
    bound(session).is_some_and(|hs| hs.interactive)
}

/// The budget of goal `g` on board `b`: the goal's own, or what the engineer raised it to (board.rs
/// `Board::budget_usd`), whichever is larger.
pub fn budget_of(g: &Goal, b: &Board) -> f64 {
    b.budget_usd.map_or(g.budget_usd, |u| u.max(g.budget_usd))
}

/// (spent, budget) when goal `g`'s metered spend on every node (spend.jsonl, run.rs `ledger`) reached its
/// budget (`budget_of`): its interactive drones claim nothing more of it. An unmetered session's spend
/// counts nothing (meter.rs), as a runner's that named no cost.
fn spent_out(home: &Path, g: &Goal, b: &Board) -> Option<(f64, f64)> {
    let (spent, budget) = (run::goal_spent(home, &g.id).total, budget_of(g, b));
    (spent >= budget).then_some((spent, budget))
}

/// Where the commits a task starts from stand for a node that lacks them (phase f F7, `lacking`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Commits {
    /// On their way: a drone of this node waits for them (the task is offered to none of its sessions,
    /// and it is not declined). What is awaited.
    Coming(String),
    /// No line can bring them here: this node declines the task (board.rs `DECLINES`), and with no other
    /// taker it is stuck, `commits unavailable` (board.rs `stuck`). Why.
    Unavailable(String),
}

impl Commits {
    pub(crate) fn why(&self) -> &str {
        match self {
            Commits::Coming(w) | Commits::Unavailable(w) => w,
        }
    }
}

/// The claimable tasks of `b` whose commits `here` lacks at `now` (phase f F7), each with where they
/// stand: a verify task of another node's work and a fix task of another node's integration, by
/// `reach` (commits.rs `reach` on a node; the scenario harness's model of the bundles). Every other task
/// needs none (its node's own, or a work task, which starts from this repo). Once per decision, one
/// answer for all who read it: the runner's offer (run.rs `open_to`), its declines and its stuck (run.rs
/// `takes`), a session's pick (`next_open`) and its claim by name (`claim_cmd`). Before, the runner
/// offered what the session then refused (the G6 drift F0 found): each node's drone ran empty sessions on
/// the other's verify task until it ended "stalled".
///
/// *Property:* a node never offers a task whose commits it lacks, waits while they may still come, and
/// declines it once they cannot: `Coming` while the attempt's node may still take part (`Board::takers`:
/// in the goal and heard within `LEASE_MS`, so its bundle line may still reach here from it or a
/// participant that holds it); `Unavailable` once its node said they stay there, its line was refused
/// here, or its node left (or was silent a lease) before its line arrived. Not waited for: a line
/// another participant holds and the mesh may still relay once its author left (no node says which
/// lines it holds); the task is declined, and a line that arrives later lets this node take it while a
/// peer may have ended stuck on it (liveness only, as a decline is). Liveness only: a task is
/// never claimed without its commits, at any timer; at 0 a node declines what a moment more would have
/// brought, at infinity it waits on a dead author (the lease bounds `takers`).
pub(crate) fn lacking(b: &Board, here: &str, now: i64, mut reach: impl FnMut(&TaskState) -> commits::Reach) -> BTreeMap<TaskId, Commits> {
    let takers = b.takers(here, now);
    let mut out = BTreeMap::new();
    for s in b.tasks.values().filter(|s| s.claimable(now) && !board::of_node(&s.node, here) && (s.task.from.is_some() || s.task.role == Role::Verify)) {
        let from = s.node.chars().take(8).collect::<String>();
        let c = match reach(s) {
            commits::Reach::Here => continue,
            commits::Reach::Refused(why) => Commits::Unavailable(format!("commits unavailable: {why}")),
            commits::Reach::Coming if takers.contains(s.node.as_str()) => Commits::Coming(format!("its commits are on their way from node {from}")),
            commits::Reach::Coming => Commits::Unavailable(format!("commits unavailable: node {from} left the goal before they reached this node")),
        };
        out.insert(s.task.id.clone(), c);
    }
    out
}

/// `lacking` on this node, its repo at `cwd`: a verify task's attempt fetched from its bundle when the repo
/// lacks it (commits.rs `reach`), a fix task's `from` looked up (it does not travel yet: it is claimed
/// where its integration ran). The board's events are read only when a task needs them.
pub(crate) fn lacking_here(home: &Path, goal: &str, b: &Board, cwd: &Path, now: i64) -> BTreeMap<TaskId, Commits> {
    let events = std::cell::OnceCell::new();
    lacking(b, &node(home), now, |s| match (&s.task.from, s.task.role) {
        (Some(from), _) if verdict::present(cwd, &[from], verdict::DIFF_TIMEOUT) => commits::Reach::Here,
        (Some(_), _) => commits::Reach::Refused(format!("the integration it starts from is on node {} only", short(&s.node))),
        (None, Role::Verify) => match verdict::attempt(events.get_or_init(|| board::read(&tasks_path(home, goal))), b, &s.task.id) {
            Some(a) if !a.base.is_empty() && !a.head.is_empty() => commits::reach(home, goal, &a, cwd),
            _ => commits::Reach::Here,
        },
        _ => commits::Reach::Here,
    })
}

/// What a drone is told of a task whose commits are on another node (`node`), and where they stand.
fn elsewhere(task: &str, node: &str, c: &Commits) -> String {
    let at = format!("{task}: its work was done on node {} and is not in this repo", short(node));
    match c {
        Commits::Coming(why) => format!("{at} yet ({why}); claim it once they are here"),
        Commits::Unavailable(why) => format!("{at} ({why}); claim it on a node that holds them"),
    }
}

/// The repo `cwd` is in: its nearest directory with a `.git` (a repo's directory, a worktree's file),
/// else `cwd`. A task's files are repo-relative, whatever directory a session runs in.
pub(crate) fn repo_top(cwd: &Path) -> &Path {
    if !cwd.is_absolute() {
        return cwd;
    }
    cwd.ancestors().find(|d| d.join(".git").exists()).unwrap_or(cwd)
}

/// Whether `cwd` is a checkout of the commit `from` (its HEAD): where any session claims a fix task,
/// its tree already the combined one (`git worktree add <dir> <from>`), with no reset.
fn checkout_of(cwd: &str, from: &str) -> bool {
    if cwd.is_empty() {
        return false;
    }
    let mut cmd = std::process::Command::new("git");
    cmd.args(["-C", cwd, "rev-parse", "--verify", "-q", "HEAD"]).env("GIT_TERMINAL_PROMPT", "0");
    crate::runner::run_capped_status(cmd, None, Duration::from_secs(5)).is_some_and(|(st, out)| st.success() && String::from_utf8_lossy(&out).trim() == from)
}

/// Whether `cwd` is the worktree `borg debug hive run` made for the drone `session` is an iteration of
/// (`d<n>` in `.worktrees/hive-<goal>-<n>`): the only place a fix task's claim may reset to its `from`.
/// An engineer's checkout, or a session of its own, never is.
fn runner_worktree(g: &Goal, session: &str, cwd: &str) -> bool {
    let Some(n) = bound(session).and_then(|hs| hs.drone.strip_prefix('d').and_then(|n| n.parse::<usize>().ok())) else { return false };
    let cwd = Path::new(cwd);
    let repo = PathBuf::from(borg_trace::repo_key(&cwd.display().to_string()));
    let same = |a: &Path, b: &Path| a == b || std::fs::canonicalize(a).is_ok_and(|a| std::fs::canonicalize(b).is_ok_and(|b| a == b));
    n >= 1 && same(cwd, &run::worktree(&repo, &g.id, n))
}

/// A shell word that runs this borg (the path the process was started by, reexec.rs `exe_path`), so a
/// printed command works in any harness's shell, from a waiter whose binary was replaced too.
fn exe() -> String {
    queen::sh_quote(&reexec::exe_path().display().to_string())
}

/// The command a drone runs for `sub`, with its session and directory.
fn command(sub: &str, session: &str, cwd: &str) -> String {
    let dir = if cwd.is_empty() { String::new() } else { format!(" --dir {}", queen::sh_quote(cwd)) };
    format!("{} hive {sub} --session {}{dir}", exe(), queen::sh_quote(session))
}

/// What a refused or lost claim tells the drone: work done on a task it does not hold is never
/// integrated (a drone in the W2 probe took "claimed" for its own claim and worked on).
const NOT_YOURS: &str = "It is not yours: do not work on it. Claim another task (`borg hive claim`) or end your turn.";

fn short(session: &str) -> String {
    session.chars().take(8).collect()
}

/// Who holds `s` at `now`, for a drone told the task is not its own: `claimed by <session>`, else its status.
fn held_by(s: &TaskState, now: i64) -> String {
    match s.live(now) {
        Some((by, _, _)) => format!("claimed by {}", short(by)),
        None => s.shown_status(now).to_string(),
    }
}

fn count(home: &Path, kind: &str, session: &str, goal: &str, task: &str, repo: &str) {
    stats::count(home, kind, session, |ev| {
        ev.why = goal.into();
        ev.text = task.into();
        ev.repo = repo.into();
    });
}

/// What a drone is told about its claim `s`.
fn describe(home: &Path, g: &Goal, s: &TaskState, session: &str, cwd: &str) -> String {
    let t = &s.task;
    let mut out = format!("Your claim: {} ({}) {}", t.id, t.role.name(), t.title);
    if !t.files.is_empty() {
        out.push_str(&format!("; files {}", t.files.join(", ")));
    }
    if !t.deps.is_empty() && t.role != Role::Verify {
        out.push_str(&format!("; after {} (its verified work is in your tree)", t.deps.join(", ")));
    }
    if let Some(gr) = &t.group {
        out.push_str(&format!("; one of several independent attempts at {gr} by other drones: solve it your own way in your worktree"));
    }
    let checks: Vec<&str> = oracle::checks(g, t).iter().map(|c| c.name.as_str()).collect();
    match t.role {
        Role::Verify => {
            out.push_str(&format!(
                ". Verify {} independently (its checks: {}), then end your turn with the line `VERDICT: pass` or `VERDICT: reject <why>` \
(or run `{}` or `{} \"why\"`).",
                t.of.as_deref().unwrap_or("its work"),
                checks.join(", "),
                command("verify --pass", session, cwd),
                command("verify --reject", session, cwd)
            ));
            if let Some(a) = under_review(home, g, &t.id) {
                out.push_str(&review_of(&t.id, &a.base, &a.head, session));
            }
            if !t.deps.is_empty() {
                out.push_str(&format!(
                    " Its tree includes the verified work of {}: check {} together with it.",
                    t.deps.join(", "),
                    t.of.as_deref().unwrap_or("the work")
                ));
            }
            if !t.flags.is_empty() {
                out.push_str(&format!(" Flagged while the work was done: {}.", t.flags.join("; ")));
            }
        }
        _ => out.push_str(&format!(
            "; done when: {} pass. Ending your turn runs them; a suite slower than the stop hook runs with `{}`. Stuck: `{} \"why\"`.",
            checks.join(", "),
            command("done", session, cwd),
            command("block", session, cwd)
        )),
    }
    // A fix task's first note is why the integration failed: shown before any rework.
    if let Some(from) = &t.from {
        let start = if own_tree(g, session, cwd) { "Your worktree was reset to" } else { "You work on" };
        out.push_str(&format!(" {start} the integrated tree {from}; the integration failed: {}.", s.notes.first().map_or("", String::as_str)));
    }
    // A work claim's reset (`onto_start`): where the tree before is kept, and whether a rework's previous
    // attempt is back.
    if t.from.is_none() && s.last_progress.starts_with(START_NOTE) {
        out.push_str(&format!(" Your worktree was reset to the claim's start: {}.", s.last_progress));
    }
    if s.rework > 0 && !s.notes.is_empty() {
        out.push_str(&format!(" Rework {} — the verifier's notes: {}", s.rework, s.notes.join(" | ")));
    }
    out
}

/// The attempt the verify task `verify` judges, when it has both snapshots.
fn under_review(home: &Path, g: &Goal, verify: &str) -> Option<verdict::Attempt> {
    let events = board::read(&tasks_path(home, &g.id));
    verdict::attempt(&events, &board::fold(&events), verify).filter(|a| !a.base.is_empty() && !a.head.is_empty())
}

/// Where the work a verifier judges is: its snapshots, by commit. The author's worktree may be on
/// another node (d3-accept2: 9 of 10 cross-node rejects found "no change" in the verifier's own tree);
/// its commits are in this repo either way (worktrees share it, a bundle brought another node's:
/// `absent_from`). Exported apart, so the verifier's own tree, where its next work starts, keeps no
/// unaccepted change: `git archive` only reads the repo (a sandboxed drone, codex's `workspace-write`,
/// may not write its `.git`, which `worktree add` does) and registers nothing, and the directory is the
/// claim's own (two nodes on one machine share the temp dir) and cleared first (a claim released
/// without a verdict, then taken again by its session).
fn review_of(verify: &str, base: &str, head: &str, session: &str) -> String {
    let own: String = session.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    let dir = queen::sh_quote(&std::env::temp_dir().join(format!("borg-verify-{verify}-{}-{own}", short(head))).display().to_string());
    format!(
        " Its change is `git diff {base} {head}` (its commits are in this repo, whichever node did the work; your worktree does not hold it): \
run its checks on a copy of it apart from your tree, `rm -rf {dir} && mkdir -p {dir} && git archive {head} | tar -x -C {dir}`, and remove it after: `rm -rf {dir}`."
    )
}

/// What a drone without a claim is told: the next open task and the exact claim command.
fn offer(home: &Path, g: &Goal, b: &Board, session: &str, repo: &str, cwd: &str, now: i64) -> String {
    match next_open(home, g, b, session, &drone_of(session), repo, cwd, now) {
        Some(s) => format!(
            "No claim. Next open: {} ({}) {}. Claim it: `{}`",
            s.task.id,
            s.task.role.name(),
            s.task.title,
            command(&format!("claim {}", queen::sh_quote(&s.task.id)), session, cwd)
        ),
        None => "No claim, and no task is open to you now (`borg hive board`).".into(),
    }
}

/// The value of the last `marker` line of a state file.
fn last_marked<'a>(state: &'a str, marker: &str) -> Option<&'a str> {
    state.lines().rev().find_map(|l| l.strip_prefix(marker)).map(str::trim)
}

/// Renews `session`'s live claim when the last renewal is `HEARTBEAT_MS` old; whether it did.
fn heartbeat(home: &Path, hs: &mut HiveSession, b: &Board, session: &str, now: i64) -> bool {
    let renew = board::holding(b, &node(home), session, now).filter(|_| now - hs.beat_ms > HEARTBEAT_MS);
    let done = renew.is_some_and(|s| write(home, &hs.goal, vec![Event::new(Ev::Heartbeat, &s.task.id, session, now)]).is_ok());
    if done {
        hs.beat_ms = now;
    }
    done
}

/// Claims `session` of node `here` lost and was not told of yet: their lines, and the marker that tells
/// them.
fn lost_news(b: &Board, here: &str, session: &str, state: &str) -> (Vec<String>, Vec<String>) {
    let told = hook::listed(state, LOST_MARKER);
    let keys: Vec<(String, &str)> = board::lost_by(b, here, session)
        .filter(|(t, ms)| !board::attempted(b, here, session, t, *ms))
        .map(|(t, ms)| (format!("{t}@{ms}"), t.as_str()))
        .collect();
    let fresh: Vec<&(String, &str)> = keys.iter().filter(|(k, _)| !told.contains(k.as_str())).collect();
    let lines = fresh
        .iter()
        .map(|(_, t)| format!("Your claim on {t} did not take effect (another drone won it, or it was not open to you): pick another task."))
        .collect();
    (lines, fresh.iter().map(|(k, _)| k.clone()).collect())
}

/// The latest claim of `session` of node `here` (`lines`, the board's), when the fold voided it (a race lost, across
/// nodes perhaps only at a sync after the claim looked won, or a claim the board did not allow) or the
/// session was told so (`LOST_MARKER`: a later line may hand the task back, `release_lost`), and
/// the session holds nothing now: its edits are refused (`pre_tool`) and its worktree set aside
/// (`set_aside_lost`): the in-progress work of a claim that never became final. A session that claimed
/// again since, or holds a task, has none, nor one whose done under that claim the fold keeps (phase f
/// F5: an attempt for good, `board::attempted`).
fn lost_claim<'a>(b: &Board, here: &str, lines: impl IntoIterator<Item = &'a Event>, session: &str, state: &str, now: i64) -> Option<&'a Event> {
    if board::holding(b, here, session, now).is_some() {
        return None;
    }
    let last = lines.into_iter().filter(|e| e.ev == Ev::Claim && e.by == session && board::of_node(&e.node, here)).max_by_key(|e| e.ms)?;
    if board::attempted(b, here, session, &last.task, last.ms) {
        return None;
    }
    let told = hook::listed(state, LOST_MARKER).contains(claim_key(last).as_str());
    (told || board::lost_by(b, here, session).any(|(t, ms)| *t == last.task && ms == last.ms)).then_some(last)
}

fn claim_key(c: &Event) -> String {
    format!("{}@{}", c.task, c.ms)
}

/// The tree claim `key` (`task@ms`) started from and the git top it works in, from `state`
/// (`CLAIM_START_MARKER`); the tree is `-` for a claim without one.
fn claim_start<'a>(state: &'a str, key: &str) -> Option<(&'a str, &'a str)> {
    state.lines().rev().filter_map(|l| l.strip_prefix(CLAIM_START_MARKER)).find_map(|v| {
        let mut it = v.splitn(3, ' ');
        (it.next()? == key).then_some(())?;
        Some((it.next()?, it.next()?))
    })
}

/// The lost claim (`lost_claim`) whose tree is still to be set aside: it has a start
/// (`CLAIM_START_MARKER`) and was not set aside yet.
fn set_aside_due<'a>(b: &Board, here: &str, lines: impl IntoIterator<Item = &'a Event>, session: &str, state: &str, now: i64) -> Option<&'a Event> {
    lost_claim(b, here, lines, session, state, now).filter(|c| {
        let key = claim_key(c);
        claim_start(state, &key).is_some_and(|(sha, _)| sha != "-") && !hook::listed(state, SET_ASIDE_MARKER).contains(key.as_str())
    })
}

/// Sets aside the worktree `cwd` of `session`'s lost claim when it is due (`set_aside_due`), once
/// (marked before it starts), `within` that time: what the drone is told, None when nothing was due or
/// nothing changed. Only at the stop (what is left of its time) or in `borg hive claim`, never in a tool
/// call's or prompt's hook: opencode and omp give those 2 to 5 s and then kill borg.
#[allow(clippy::too_many_arguments)]
fn set_aside_lost<'a>(
    g: &Goal,
    b: &Board,
    here: &str,
    lines: impl IntoIterator<Item = &'a Event>,
    session: &str,
    state: &str,
    cwd: &str,
    now: i64,
    within: Duration,
) -> Option<String> {
    let c = set_aside_due(b, here, lines, session, state, now)?;
    let key = claim_key(c);
    let (start, _) = claim_start(state, &key)?;
    hook::append(session, &format!("{SET_ASIDE_MARKER}{key}"));
    set_aside(g, b, &c.task, start, session, cwd, now, within)
}

/// The worktree `cwd` after a lost claim of `task` that started from the tree `start`, `within` that
/// time: its changes
/// since then kept in a snapshot commit, pinned (commits.rs `pin`, so gc keeps it), and taken out
/// (verdict.rs `take_out` of start..snapshot: the index, HEAD and refs untouched, ignored files stay),
/// so the tree is the claim's start and nothing of the lost work leaks into the next task; what the
/// drone is told, None when nothing changed. Nothing is taken out without a pinned snapshot, or while
/// another session of this node holds a task in the same git top (`CLAIM_START_MARKER` in its state):
/// its work is in the same files.
#[allow(clippy::too_many_arguments)]
fn set_aside(g: &Goal, b: &Board, task: &str, start: &str, session: &str, cwd: &str, now: i64, within: Duration) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    let (dir, top) = (Path::new(cwd), repo_top(Path::new(cwd)));
    let t0 = Instant::now();
    let Some(snap) = verdict::snapshot(dir, within) else {
        return Some(format!("Your worktree still holds what you did for {task} (no snapshot could be taken): set it aside before your next task."));
    };
    if verdict::changes(dir, start, &snap).is_some_and(|ch| ch.is_empty()) {
        return None;
    }
    if !commits::pin(dir, &g.id, &snap) {
        return Some(format!(
            "Your worktree still holds what you did for {task} (its snapshot {snap} could not be pinned): set it aside before your next task."
        ));
    }
    let shared = b.tasks.values().any(|s| {
        s.live(now).is_some_and(|(by, _, since)| {
            by != session
                && claim_start(&std::fs::read_to_string(hook::state_file(by)).unwrap_or_default(), &format!("{}@{since}", s.task.id))
                    .is_some_and(|(_, t)| Path::new(t) == top)
        })
    });
    if shared {
        return Some(format!("What you did for {task} is kept in commit {snap}; your worktree is left as it is: another drone holds a task in it."));
    }
    Some(match verdict::take_out(dir, start, &snap, within.saturating_sub(t0.elapsed())) {
        Ok(()) => format!(
            "What you did for {task} is set aside in commit {snap} (`git diff {start} {snap}`) and your worktree is back at the claim's start: start your next task from there."
        ),
        Err(why) => format!("Your worktree could not be set back to the claim's start ({why}); what you did for {task} is kept in commit {snap}."),
    })
}

/// Releases each of `lost` (`task@ms` claim keys) of `session` as it learns the loss (phase f F5, in
/// place of a release once the fold flipped such a claim back to a win): a `released` stamped after the
/// claim, which the fold ignores while the claim holds nothing and applies the moment a line that syncs
/// later (the winner's release, written before this claim) makes it win. The drone was told to move on
/// and its tree may be set aside, so it never holds such a task unawares, its heartbeats renewing it.
fn release_lost(home: &Path, goal: &str, session: &str, lost: &[String]) {
    let lines: Vec<Event> = lost
        .iter()
        .filter_map(|k| k.rsplit_once('@'))
        .map(|(task, _)| Event::new(Ev::Released, task, session, learn::now_ms()).with_note("its claim was told lost"))
        .collect();
    if !lines.is_empty() {
        let _ = write(home, goal, lines);
    }
}

/// The board of goal `g` for `session` at `now`: its lines with their offsets, the end offset, the fold
/// (of the lines and the goal's beats, `board::read`).
fn board_for(home: &Path, g: &Goal) -> (Vec<(u64, Event)>, u64, Board) {
    let path = tasks_path(home, &g.id);
    let (events, end) = board::read_from(&path, 0);
    let all: Vec<Event> = events.iter().map(|(_, e)| e.clone()).chain(board::beats_of(&path)).collect();
    let b = board::fold_on(&all, &node(home));
    (events, end, b)
}

/// `out` (the inner answer) with `text` as the event's context, first; when the harness does not take
/// context here (`allowed` false) the text waits for the stop.
fn with_context(session: &str, hook_event: &str, text: String, allowed: bool, out: Option<Value>) -> Option<Value> {
    if text.is_empty() {
        return out;
    }
    if !allowed {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(wait_path(session)) {
            use std::io::Write;
            let _ = f.write_all(format!("{text}\n\n").as_bytes());
        }
        return out;
    }
    let mut out = out.unwrap_or_else(|| json!({}));
    let Some(o) = out.as_object_mut() else { return Some(out) };
    let spec = o.entry("hookSpecificOutput").or_insert_with(|| json!({"hookEventName": hook_event}));
    let merged = match spec.get("additionalContext").and_then(Value::as_str) {
        Some(theirs) if !theirs.is_empty() => format!("{text}\n\n{theirs}"),
        _ => text,
    };
    spec["additionalContext"] = Value::String(merged);
    if spec.get("hookEventName").is_none() {
        spec["hookEventName"] = Value::String(hook_event.into());
    }
    Some(out)
}

/// The block reason of a hook answer, if it blocks with one.
fn reason_of(out: &Option<Value>) -> Option<&str> {
    out.as_ref().and_then(|o| o.get("reason")).and_then(Value::as_str)
}

/// `out` with `text` before its block reason; a stop that would not block blocks with `text` only when
/// it `may_block` (its first round, so never a loop; or a handover, which follows a finished task).
fn with_reason(text: &str, out: Option<Value>, may_block: bool) -> Option<Value> {
    if text.trim().is_empty() {
        return out;
    }
    match out.as_ref().and_then(|o| o.get("reason")).and_then(Value::as_str) {
        Some(r) => Some(json!({"decision": "block", "reason": format!("{}\n\n{r}", text.trim_end())})),
        None if may_block => Some(json!({"decision": "block", "reason": text.trim_end()})),
        None => out,
    }
}

/// The hook's hive step, inside steering and around everything else it answers (`inner`). A session in
/// no goal (or a goal gone) gets `inner()` untouched.
pub fn around(h: &dyn Harness, home: &Path, event: &str, input: &Value, now: i64, inner: impl FnOnce() -> Option<Value>) -> Option<Value> {
    // A planner session (plan.rs `planned`): the engineer's answer to a plan it opened (phase g G5,
    // `at_prompt`), read from their own prompt, and its drones' messages to it (`planner_prompt`), first
    // in its context. One failed open for a session that planned nothing.
    if event == "prompt" {
        let session = input.get("session_id").and_then(Value::as_str).unwrap_or("unknown");
        let prompt = input.get("prompt").and_then(Value::as_str).unwrap_or_default();
        let answer = plan::at_prompt(home, session, prompt, now);
        let allowed = h.caps().prompt_context && input.get("context") != Some(&Value::Bool(false));
        // Only text put into the context is marked shown: an unbound session has no stop that reads
        // `wait_path`, so a harness without prompt context leaves the messages unread (monitoring shows
        // them) and the answer's outcome to the board.
        let msgs = plan::planner_prompt(home, session).filter(|_| allowed);
        if let Some((_, shown)) = &msgs {
            for (goal, ids) in shown {
                hook::append(session, &format!("{}{}", message::MARKER, ids.join(",")));
                stats::count(home, stats::HIVE_SHOWN, session, |ev| ev.why = goal.clone());
            }
        }
        let text: Vec<String> = answer.into_iter().chain(msgs.map(|m| m.0)).collect();
        if allowed && !text.is_empty() {
            return with_context(session, "UserPromptSubmit", text.join("\n\n"), true, around_session(h, home, event, input, now, inner));
        }
    }
    around_session(h, home, event, input, now, inner)
}

/// `around` for any session: a bound one's hive step (`around_bound`), else `inner()` untouched.
fn around_session(h: &dyn Harness, home: &Path, event: &str, input: &Value, now: i64, inner: impl FnOnce() -> Option<Value>) -> Option<Value> {
    let started = Instant::now();
    let session = input.get("session_id").and_then(Value::as_str).unwrap_or("unknown");
    // A join that bound an id no hook carries (interactive.rs `adopt`): its own PostToolUse moves it here.
    let (hs, adopted) = match bound(session) {
        Some(hs) => (hs, false),
        None => match (event == "post_tool").then(|| interactive::adopt(home, input)).flatten() {
            Some(hs) => (hs, true),
            // A codex join whose thread is not this session's: it left again, and the session hears why.
            None => {
                match (event == "post_tool" && h.caps().drone_wake == crate::harness::DroneWake::Queue).then(|| interactive::mismatch(home, input)).flatten() {
                    Some(text) => return with_context(session, "PostToolUse", text, h.caps().tool_context, inner()),
                    None => return inner(),
                }
            }
        },
    };
    if adopted {
        let text = format!(
            "[borg hive] This session is drone {}: the join named another session id (a `--continue` or `--resume`), so the drone is this session now.",
            hs.drone
        );
        return with_context(session, "PostToolUse", text, h.caps().tool_context, around_bound(h, home, event, input, now, started, session, hs, inner));
    }
    around_bound(h, home, event, input, now, started, session, hs, inner)
}

/// `around` for a session bound to a goal (`hs`).
#[allow(clippy::too_many_arguments)]
fn around_bound(
    h: &dyn Harness,
    home: &Path,
    event: &str,
    input: &Value,
    now: i64,
    started: Instant,
    session: &str,
    mut hs: HiveSession,
    inner: impl FnOnce() -> Option<Value>,
) -> Option<Value> {
    // An interactive drone (phase g): its handed claim taken up, the engineer's turn, its node's beat.
    if hs.interactive && interactive::touch(home, event, &mut hs, session, h.name(), input, now) {
        save(session, &hs);
    }
    let cwd = match hs.worktree.is_empty() {
        true => input.get("cwd").and_then(Value::as_str).unwrap_or_default().to_string(),
        false => hs.worktree.clone(),
    };
    let Ok(g) = goal::load(home, &hs.goal) else {
        // An interactive drone of every open goal with none to work on yet still waits for one.
        return match hs.interactive && event == "stop" {
            true => interactive::after_stop(home, &mut hs, session, &cwd, input, inner(), interactive::Wake::of(h.name(), false), false, now),
            false if hs.interactive && event == "end" => {
                let out = inner();
                interactive::ended(home, &hs, session, &cwd, input, now);
                out
            }
            false => inner(),
        };
    };
    let cwd = cwd.as_str();
    let repo = queen::repo_name(cwd);
    let (events, end, b) = board_for(home, &g);
    let ctx = Ctx { h, home, g: &g, session, cwd, repo: &repo, now, started };
    match event {
        "prompt" => {
            heartbeat(home, &mut hs, &b, session, now);
            let text = ctx.prompt(&mut hs, &b, &events, end);
            save(session, &hs);
            let allowed = h.caps().prompt_context && input.get("context") != Some(&Value::Bool(false));
            with_context(session, "UserPromptSubmit", text, allowed, inner())
        }
        "post_tool" => {
            if heartbeat(home, &mut hs, &b, session, now) {
                save(session, &hs);
            }
            let text = ctx.post_tool(&b, &events, input);
            with_context(session, "PostToolUse", text, h.caps().tool_context, inner())
        }
        "stop" => {
            // While an interactive drone's stop runs, its waiter claims and hands nothing (the stop's own
            // done and handover, `Ctx::done`, decide its next claim): interactive.rs `Stopping`.
            let budget = crate::expert_route::stop_budget(h).unwrap_or(crate::expert_route::DEFAULT_STOP_BUDGET);
            let _stopping = hs.interactive.then(|| interactive::Stopping::mark(session, now + budget.as_millis() as i64));
            // What the conscience (and the rest of `inner`) said, so the hive's own words are told apart.
            let theirs = std::cell::RefCell::new(None);
            let held = board::holding(&b, &node(home), session, now).cloned();
            let (out, asks) = ctx.stop(&mut hs, &b, &events, input, started, || {
                let o = inner();
                *theirs.borrow_mut() = Some(o.clone());
                o
            });
            // Its done, verdict, block or release at this stop ended the claim: what it cost (meter.rs).
            if let Some(held) = held {
                meter_end(home, &g.id, session, &hs, &held, h.name(), crate::harness::transcript(h.caps(), input));
            }
            let hive = reason_of(&out) != theirs.borrow().as_ref().and_then(reason_of);
            match hs.interactive {
                true => interactive::after_stop(home, &mut hs, session, cwd, input, out, interactive::Wake::of(h.name(), hive), asks, now),
                false => out,
            }
        }
        "end" => {
            let out = inner();
            let held = board::holding(&b, &node(home), session, now);
            // An interactive drone's end is its leave, which unbinds it before it releases its claim
            // (interactive.rs `ended`), so its waiter claims nothing after; a `/clear` or any other
            // session's end releases here.
            let left = hs.interactive && interactive::ended(home, &hs, session, cwd, input, now);
            if let Some(s) = held {
                let released = left || write(home, &g.id, vec![Event::new(Ev::Released, &s.task.id, session, now).with_note("session ended")]).is_ok();
                if released {
                    count(home, stats::HIVE_RELEASED, session, &g.id, &s.task.id, &repo);
                    meter_end(home, &g.id, session, &hs, s, h.name(), crate::harness::transcript(h.caps(), input));
                }
            }
            out
        }
        _ => inner(),
    }
}

/// The hook's pre_tool step for an edit or write (hook.rs `edit_text`'s tools): denied while the
/// session's latest claim was lost and it holds no task (`lost_claim`), with the lost claim's news
/// when it is new (told once, as at any other hook) and `NOT_YOURS`. A claim lost across nodes is
/// learnt at a sync, so the loser's next edit is its last attempt at the task, not its next turn. The
/// worktree is not set aside here (a 2 s hook on opencode and omp): at the stop or the next claim. An
/// interactive drone's edit of its repo outside its own worktree is denied (interactive.rs
/// `outside_tree`): its work never lands in the engineer's checkout. Any
/// other tool gets None after one failed file open (an interactive drone's tool call keeps the
/// engineer's turn busy, interactive.rs `touch`); an edit in no goal after the same.
pub fn pre_tool(h: &dyn Harness, home: &Path, input: &Value, now: i64) -> Option<Value> {
    let session = input.get("session_id").and_then(Value::as_str).unwrap_or("unknown");
    let mut hs = bound(session);
    if let Some(hs) = hs.as_mut().filter(|hs| hs.interactive)
        && interactive::touch(home, "pre_tool", hs, session, h.name(), input, now)
    {
        save(session, hs);
    }
    let tool = input.get("tool_name").and_then(Value::as_str).unwrap_or_default();
    hook::edit_text(tool, input.get("tool_input").unwrap_or(&Value::Null))?;
    let hs = hs?;
    if hs.interactive
        && let Some(reason) = interactive::outside_tree(&hs, tool, input)
    {
        return Some(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}}));
    }
    let g = goal::load(home, &hs.goal).ok()?;
    let cwd = if hs.worktree.is_empty() { input.get("cwd").and_then(Value::as_str).unwrap_or_default() } else { hs.worktree.as_str() };
    let repo = queen::repo_name(cwd);
    let (events, _, b) = board_for(home, &g);
    let state = std::fs::read_to_string(hook::state_file(session)).unwrap_or_default();
    let task = lost_claim(&b, &node(home), events.iter().map(|(_, e)| e), session, &state, now)?.task.clone();
    let ctx = Ctx { h, home, g: &g, session, cwd, repo: &repo, now, started: Instant::now() };
    let mut news = ctx.lost_told(&b, &events, &state, false);
    if news.is_empty() {
        news.push(format!("Your claim on {task} did not take effect and you hold no task: this {tool} is refused."));
    }
    let reason = format!("{} {}\n{NOT_YOURS}", ctx.head(), news.join("\n"));
    Some(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}}))
}

/// One hook event of a hive drone.
struct Ctx<'a> {
    h: &'a dyn Harness,
    home: &'a Path,
    g: &'a Goal,
    session: &'a str,
    cwd: &'a str,
    repo: &'a str,
    now: i64,
    started: Instant,
}

impl Ctx<'_> {
    /// The time now: the hook's `now` moved on by what the hook has taken (the oracle, a snapshot), so
    /// a line written late in the stop is not stamped with its start.
    fn clock(&self) -> i64 {
        self.now + self.started.elapsed().as_millis() as i64
    }

    fn head(&self) -> String {
        format!("[borg hive {}]", self.g.id)
    }

    /// The lost claims not told yet (`lost_news`), marked told: told once, at whichever hook comes
    /// first. At the stop (`settle`) the latest lost claim's worktree is set aside then
    /// (`set_aside_lost`), by its own session, whose next claim may come from a fresh session that never
    /// hears of this loss; never while it holds a task. At a shorter hook the news says when it will be.
    fn lost_told(&self, b: &Board, lines: &[(u64, Event)], state: &str, settle: bool) -> Vec<String> {
        let (mut news, keys) = lost_news(b, &node(self.home), self.session, state);
        if !keys.is_empty() {
            hook::append(self.session, &format!("{LOST_MARKER}{}", keys.join(",")));
            // A task it holds again under a newer claim is not released.
            let held = board::holding(b, &node(self.home), self.session, self.now).map(|s| s.task.id.clone());
            let stale: Vec<String> = keys.iter().filter(|k| k.rsplit_once('@').is_none_or(|(t, _)| Some(t) != held.as_deref())).cloned().collect();
            release_lost(self.home, &self.g.id, self.session, &stale);
        }
        let lines = || lines.iter().map(|(_, e)| e);
        if settle {
            let total = crate::expert_route::stop_budget(self.h).unwrap_or(crate::expert_route::DEFAULT_STOP_BUDGET);
            let left = total.saturating_sub(self.started.elapsed()).saturating_sub(crate::expert_route::STOP_MARGIN);
            news.extend(set_aside_lost(self.g, b, &node(self.home), lines(), self.session, state, self.cwd, self.now, left));
        } else if let Some(c) = set_aside_due(b, &node(self.home), lines(), self.session, state, self.now).filter(|c| keys.contains(&claim_key(c))) {
            news.push(format!(
                "What you did for {} is set aside when this turn ends or at your next claim (kept in a snapshot commit): build nothing more on it.",
                c.task
            ));
        }
        news
    }

    /// The prompt's hive context: the claim (in full when it is new to the drone), lost claims, the
    /// board's changes since the last prompt, and contracts and playbooks in the claim's scope not shown
    /// yet.
    fn prompt(&self, hs: &mut HiveSession, b: &Board, events: &[(u64, Event)], end: u64) -> String {
        let state = std::fs::read_to_string(hook::state_file(self.session)).unwrap_or_default();
        let mut parts: Vec<String> = Vec::new();
        let mut marks: Vec<String> = Vec::new();
        let last = last_marked(&state, TASK_MARKER);
        if last.is_none() {
            parts.push(inoculation(self.g));
        }
        if !goal::in_scope(self.g, self.repo) && !state.lines().any(|l| l == REPO_MARKER) {
            let names: Vec<&str> = self.g.repos.iter().map(|r| r.name.as_str()).collect();
            parts.push(format!("This repo ({}) is outside the goal's repos ({}): claim only tasks of this repo.", self.repo, names.join(", ")));
            marks.push(REPO_MARKER.into());
        }
        // An interactive drone's claim it was never shown is not final yet (`stop`): neither shown here,
        // in what is the engineer's own turn, nor offered past; the waiter hands it once final. Nor is a
        // codex wake's claim this turn did not start from (interactive.rs `not_in_hand`).
        let unshown = hs.interactive && interactive::not_in_hand(self.home, hs, self.session, self.now);
        let holding = board::holding(b, &node(self.home), self.session, self.now).filter(|_| !unshown);
        let key = holding.map_or_else(|| "-".to_string(), |s| format!("{}@{}", s.task.id, s.live(self.now).map_or(0, |l| l.2)));
        match holding {
            _ if unshown => {}
            Some(s) if last != Some(key.as_str()) => parts.push(describe(self.home, self.g, s, self.session, self.cwd)),
            Some(s) => parts.push(format!("Your claim: {} ({}) {}.", s.task.id, s.task.role.name(), s.task.title)),
            None => parts.push(offer(self.home, self.g, b, self.session, self.repo, self.cwd, self.now)),
        }
        if last != Some(key.as_str()) && !unshown {
            marks.push(format!("{TASK_MARKER}{key}"));
        }
        parts.extend(self.lost_told(b, events, &state, false));
        // What other drones and engineers did since the last prompt, newest last (another node's session
        // of this name is another drone, phase f F8).
        let cursor = if hs.cursor > end { 0 } else { hs.cursor };
        let here = node(self.home);
        let mine = |e: &Event| board::of_node(&e.node, &here) && e.by == self.session;
        let changes: Vec<&Event> = events.iter().filter(|(at, e)| *at > cursor && !mine(e) && e.ev != Ev::Heartbeat).map(|(_, e)| e).collect();
        if !changes.is_empty() {
            let skip = changes.len().saturating_sub(SHOW_CHANGES);
            let mut s = String::from("Board changes:");
            for e in &changes[skip..] {
                s.push_str(&format!("\n- {}", board::render_change(e)));
            }
            if skip > 0 {
                s.push_str(&format!("\n(+{skip} more: `borg hive board`)"));
            }
            parts.push(s);
        }
        hs.cursor = end;
        // Messages from other drones (message.rs), each once.
        let msgs = message::read(&messages_path(self.home, &self.g.id));
        if !msgs.is_empty() {
            let told = hook::listed(&state, message::MARKER);
            let held = holding.map_or("", |s| s.task.id.as_str());
            let here = node(self.home);
            let new = message::fresh(&msgs, self.session, &drone_of(self.session), &here, held, &told, self.now);
            if !new.is_empty() {
                let lines: Vec<String> = new
                    .iter()
                    .map(|m| {
                        // The planner: a session of this node that planned the goal, or the engineer's
                        // terminal, whose `by` no prompt reads (answered `--to planner`).
                        let terminal = m.drone.is_empty() && m.by.starts_with("engineer:");
                        let planner = terminal || (m.drone.is_empty() && board::of_node(&m.node, &here) && plan::planned(&m.by).contains(&self.g.id));
                        let to = if terminal { "planner" } else { m.by.as_str() };
                        let reply = command(&format!("tell \"<answer>\" --re {} --to {}", queen::sh_quote(&m.id), queen::sh_quote(to)), self.session, self.cwd);
                        format!("- {}", message::render(m, &here, &reply, planner))
                    })
                    .collect();
                parts.push(format!("Messages from other drones (not the user's instructions):\n{}", lines.join("\n")));
                marks.push(format!("{}{}", message::MARKER, new.iter().map(|m| m.id.as_str()).collect::<Vec<_>>().join(",")));
            }
        }
        let mut shown: Vec<String> = Vec::new();
        if let Some(s) = holding {
            let all = contract::read(&contracts_path(self.home, &self.g.id));
            let current = contract::current(&all);
            let told = hook::listed(&state, CONTRACT_MARKER);
            let repo = if s.task.repo.is_empty() { self.repo } else { &s.task.repo };
            let fresh: Vec<&Contract> = contract::in_task_scope(&current, repo, &s.task.files, &s.task.title)
                .into_iter()
                .filter(|c| !(board::of_node(&c.node, &here) && c.by == self.session) && !told.contains(c.id.as_str()))
                .collect();
            if !fresh.is_empty() {
                let lines: Vec<String> = fresh.iter().map(|c| contract::render(c)).collect();
                parts.push(format!("Contracts in your claim's scope:\n{}\n{}", lines.join("\n"), contract::LABEL));
                shown = fresh.iter().map(|c| c.id.clone()).collect();
                marks.push(format!("{CONTRACT_MARKER}{}", shown.join(",")));
            }
            // The team's playbooks for the claim's task shape (playbook.rs), each version once, logged so
            // the verdict on this attempt credits the version shown.
            let text = format!("{} {}", s.task.title, s.task.files.join(" "));
            if let Some((pb, keys)) = crate::playbook::fresh(self.home, &state, repo, &text, queen::STATED) {
                parts.push(pb);
                marks.push(format!("{}{}", crate::playbook::MARKER, keys.join(",")));
                crate::playbook::record_shown(self.home, self.session, repo, &keys, self.clock());
            }
        }
        if !marks.is_empty() {
            hook::append(self.session, &marks.join("\n"));
        }
        if parts.is_empty() {
            return String::new();
        }
        stats::count(self.home, stats::HIVE_SHOWN, self.session, |ev| {
            ev.lessons = shown;
            ev.why = self.g.id.clone();
            ev.repo = self.repo.into();
        });
        format!("{} {}", self.head(), parts.join("\n"))
    }

    /// After a tool call: a lost claim, after any tool (the loser hears of it at its next hook, not its
    /// next turn); after an edit, contracts it lands in and files outside the claim's scope (advice,
    /// never a refusal). Each once.
    fn post_tool(&self, b: &Board, lines: &[(u64, Event)], input: &Value) -> String {
        let state = std::fs::read_to_string(hook::state_file(self.session)).unwrap_or_default();
        let mut parts = self.lost_told(b, lines, &state, false);
        let hits = self.edit_news(b, input, &state, &mut parts);
        if parts.is_empty() {
            return String::new();
        }
        stats::count(self.home, stats::HIVE_SHOWN, self.session, |ev| {
            ev.lessons = hits;
            ev.why = self.g.id.clone();
            ev.repo = self.repo.into();
        });
        format!("{} {}", self.head(), parts.join("\n"))
    }

    /// An edit's contracts and out-of-scope files (`post_tool`) added to `parts`, marked shown: the
    /// contracts' ids. Nothing for another tool.
    fn edit_news(&self, b: &Board, input: &Value, state: &str, parts: &mut Vec<String>) -> Vec<String> {
        let tool = input.get("tool_name").and_then(Value::as_str).unwrap_or_default();
        let tool_input = input.get("tool_input").unwrap_or(&Value::Null);
        let Some(text) = hook::edit_text(tool, tool_input) else { return Vec::new() };
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let mut files: Vec<String> = [s(tool_input, "file_path"), s(tool_input, "notebook_path")].into_iter().collect();
        files.extend(tool_input.get("edits").and_then(Value::as_array).into_iter().flatten().map(|e| s(e, "file_path")));
        // Repo-relative, as a task's files and a contract's scope are: a session may run in a subdirectory.
        let (cwd, top) = (Path::new(self.cwd), repo_top(Path::new(self.cwd)));
        let files: Vec<String> =
            files.into_iter().filter(|f| !f.is_empty()).map(|f| cwd.join(&f).strip_prefix(top).map(|p| p.display().to_string()).unwrap_or(f)).collect();
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        let mut marks = Vec::new();
        let all = contract::read(&contracts_path(self.home, &self.g.id));
        let current = contract::current(&all);
        let told = hook::listed(state, CONTRACT_MARKER);
        let here = node(self.home);
        let hits: Vec<&Contract> = contract::matching(&current, self.repo, &refs, &text)
            .into_iter()
            .filter(|c| !(board::of_node(&c.node, &here) && c.by == self.session) && !told.contains(c.id.as_str()))
            .collect();
        if !hits.is_empty() {
            for c in &hits {
                parts.push(format!("This edit lands in {}", contract::render(c)));
            }
            parts.push(contract::LABEL.into());
            marks.push(format!("{CONTRACT_MARKER}{}", hits.iter().map(|c| c.id.as_str()).collect::<Vec<_>>().join(",")));
        }
        if let Some(held) = board::holding(b, &node(self.home), self.session, self.now).filter(|s| !s.task.files.is_empty()) {
            let globs: Vec<regex::Regex> = held.task.files.iter().map(|g| crate::assimilate_dir::glob(g)).collect();
            let warned = hook::listed(state, SCOPE_MARKER);
            let outside: Vec<&str> = refs.iter().copied().filter(|f| !globs.iter().any(|g| g.is_match(f)) && !warned.contains(f) && !f.contains(',')).collect();
            // What the verifier protects comes first (verdict.rs `protected`): a file the goal's check
            // reads (guard.rs files it under CI too) no scope admits; a test or CI file only the
            // engineer's amend does; any other file outside is the drone's call.
            let oracle = guard::oracle_files(self.g, top);
            for f in &outside {
                let id = &held.task.id;
                parts.push(match guard::class_of(f, &top.join(f), &[], &oracle) {
                    _ if oracle.iter().any(|o| o == f) => format!(
                        "{f} is a file the goal's check reads: no scope admits a change to it, and the verifier rejects one. Leave it; if the check is wrong, tell the planner: `borg hive tell \"...\" --to planner`."
                    ),
                    Some(c) => format!(
                        "{f} is a {} file outside {id}'s scope ({}): the verifier rejects a change that removes or reshapes what is in it. If {id} needs it, ask the engineer: `borg hive scope --files +{f} \"why\"`, and keep working on what is in scope meanwhile. Never reshape a test to get past the guard.",
                        c.name(),
                        held.task.files.join(", ")
                    ),
                    None => format!("{f} is outside {id}'s scope ({}): fine if the task needs it, but another drone may own it.", held.task.files.join(", ")),
                });
            }
            if !outside.is_empty() {
                marks.push(format!("{SCOPE_MARKER}{}", outside.join(",")));
            }
        }
        if !marks.is_empty() {
            hook::append(self.session, &marks.join("\n"));
        }
        hits.iter().map(|c| c.id.clone()).collect()
    }

    /// The stop: with a claim, its oracle decides (block, publish blocked, or publish done and hand
    /// over the next task); without one, `inner()` with any waiting hive text. An interactive drone's
    /// claim it was never shown (the stop's capped claim, not final yet, interactive.rs `unshown`) is
    /// nothing in hand: no oracle, no done, no block, so no task reaches it before its claim is final
    /// (phase f F5) and no end of the engineer's own turn is held up by hive work; the waiter hands it.
    /// Nor is a claim a codex wake handed that the turn ending here was not started by (the engineer's
    /// turn, ahead of the wake in codex's queue, interactive.rs `not_in_hand`): its own turn comes next.
    ///
    /// With what it says, whether the turn ends waiting on a scope ask: the claim kept and nothing in
    /// hand until the amend or the engineer's no wakes the drone (interactive.rs `after_stop`).
    fn stop(
        &self,
        hs: &mut HiveSession,
        b: &Board,
        lines: &[(u64, Event)],
        input: &Value,
        started: Instant,
        inner: impl FnOnce() -> Option<Value>,
    ) -> (Option<Value>, bool) {
        let first_round = input.get("stop_hook_active").and_then(Value::as_bool) != Some(true);
        let waiting = std::fs::read_to_string(wait_path(self.session)).unwrap_or_default();
        let _ = std::fs::remove_file(wait_path(self.session));
        let path = tasks_path(self.home, &self.g.id);
        let unshown = hs.interactive && interactive::not_in_hand(self.home, hs, self.session, self.now);
        let Some(held) = board::holding(b, &node(self.home), self.session, self.now).filter(|_| !unshown) else {
            let state = std::fs::read_to_string(hook::state_file(self.session)).unwrap_or_default();
            let lost = self.lost_told(b, lines, &state, true);
            let mut text = waiting;
            if !lost.is_empty() {
                text.push_str(&format!("{} {}", self.head(), lost.join("\n")));
            }
            return (with_reason(&text, inner(), first_round), false);
        };
        let task = &held.task;
        let claim = format!("{}@{}", task.id, held.live(self.now).map_or(0, |l| l.2));
        if hs.blocks_for != claim {
            hs.blocks_for = claim;
            hs.oracle_blocks = 0;
        }
        let total = crate::expert_route::stop_budget(self.h).unwrap_or(crate::expert_route::DEFAULT_STOP_BUDGET);
        let left = || total.saturating_sub(started.elapsed()).saturating_sub(crate::expert_route::STOP_MARGIN);
        // A verify task ends with a verdict, its last message's `VERDICT:` line, not with an oracle run.
        let said = (task.role == Role::Verify).then(|| verdict::parse(&crate::harness::last_message(self.h.caps(), input))).flatten();
        if let Some(said) = said {
            let mut text = waiting;
            let mut handed = false;
            match verdict::record(self.home, self.g, &task.id, &said, self.session, self.cwd, self.now) {
                Ok(r) => {
                    text.push_str(&format!("[borg hive] {}\n\n", r.summary(&task.id)));
                    if let Some(next) = self.hand_next(&path, left()) {
                        text.push_str(&format!("[borg hive] Next: {next}\n\n"));
                        handed = true;
                        hs.beat_ms = self.now;
                    }
                }
                Err(e) => text.push_str(&format!("[borg hive] verdict on {} not recorded: {e}\n\n", task.id)),
            }
            hs.oracle_blocks = 0;
            hs.blocks_for.clear();
            save(self.session, hs);
            pass_budget(left());
            return (with_reason(&text, inner(), first_round || handed), false);
        }
        let verdict = if task.role == Role::Verify {
            oracle::Verdict::Fail {
                check: "verdict".into(),
                tail: format!(
                    "no verdict yet: end your turn with the line `VERDICT: pass` or `VERDICT: reject <why>`, or run `{}` or `{} \"why\"`",
                    command("verify --pass", self.session, self.cwd),
                    command("verify --reject", self.session, self.cwd)
                ),
            }
        } else {
            let budget = left().saturating_sub(CONSCIENCE_RESERVE);
            match budget < MIN_ORACLE {
                true => {
                    oracle::Verdict::Timeout { check: oracle::checks(self.g, task).first().map_or_else(String::new, |c| c.name.clone()), after: Duration::ZERO }
                }
                false => oracle::run(self.home, self.g, task, Path::new(self.cwd), Some(budget)),
            }
        };
        // The done's precheck (S-done) on the snapshot the done would carry blocks as a failing check
        // does: the drone undoes it or asks.
        let (mut head, mut asked) = (String::new(), false);
        let verdict = match verdict {
            oracle::Verdict::Pass if task.role != Role::Verify => {
                head = verdict::snapshot(Path::new(self.cwd), left()).unwrap_or_default();
                let admit = scope::waiting_for(self.home, &self.g.id, self.session, held);
                match scope_refusal(self.home, self.g, held, self.session, Path::new(self.cwd), &head, admit.as_deref()) {
                    Some((why, admitted)) => {
                        asked = admitted;
                        oracle::Verdict::Fail { check: SCOPE_CHECK.into(), tail: why }
                    }
                    None => oracle::Verdict::Pass,
                }
            }
            v => v,
        };
        // An unanswered scope ask (scope.rs) holds up only what it asks for: a stop whose one failure is
        // the precheck, on paths the asked files would all admit, ends the turn waiting, no attempt
        // counted; the amend or the engineer's no wakes the drone. Every other stop runs as without the
        // ask, so dropping the change finishes the task.
        if let oracle::Verdict::Fail { tail, .. } = &verdict
            && asked
        {
            let text = format!(
                "{waiting}[borg hive] {} waits for the engineer's answer to your scope ask: {}\nThe amend or the engineer's no wakes you; nothing is counted meanwhile. To finish {} as it is, undo those changes and end your turn (or run `{}`); if you are stuck, `{} \"why\"`.\n\n",
                task.id,
                tail.trim_end(),
                task.id,
                command("done", self.session, self.cwd),
                command("block", self.session, self.cwd)
            );
            return (with_reason(&text, inner(), first_round), true);
        }
        if verdict != oracle::Verdict::Pass && hs.oracle_blocks < MAX_ORACLE_BLOCKS {
            hs.oracle_blocks += 1;
            save(self.session, hs);
            let _ = oracle::log(self.home, &self.g.id, &task.id, &verdict, self.now);
            count(self.home, stats::HIVE_ORACLE_FAIL, self.session, &self.g.id, &task.id, self.repo);
            let what = match &verdict {
                oracle::Verdict::Fail { check, tail } => format!("{check} failed (attempt {} of {MAX_ORACLE_BLOCKS}).\n{}", hs.oracle_blocks, tail.trim_end()),
                oracle::Verdict::Timeout { check, .. } => format!(
                    "{check} {HOOK_TIMED_OUT} (attempt {} of {MAX_ORACLE_BLOCKS}): run the checks with `{}`, which has no hook time limit.",
                    hs.oracle_blocks,
                    command("done", self.session, self.cwd)
                ),
                oracle::Verdict::Pass => String::new(),
            };
            let mut reason = format!(
                "{waiting}[borg hive] {} is not done: {what}\nFix it, or run `{}` (no hook time limit), or `{} \"why\"` if you are stuck.",
                task.id,
                command("done", self.session, self.cwd),
                command("block", self.session, self.cwd)
            );
            if matches!(&verdict, oracle::Verdict::Fail { check, .. } if check != SCOPE_CHECK) && task.role != Role::Verify {
                reason.push_str(&format!(" {WRONG_CHECK}"));
            }
            // No conscience round on unfinished work: its model call would judge work still changing.
            return (Some(json!({"decision": "block", "reason": reason})), false);
        }
        // An interactive drone hears what became of its task before anything else (`done`).
        let (mut text, after) = match hs.interactive {
            true => (String::new(), waiting),
            false => (waiting, String::new()),
        };
        let mut handed = false;
        if let Some(check) = verdict.check() {
            // At the cap: the drone may stop; the board says why the task stands still.
            let (ev, note) = match task.role {
                Role::Verify => (Ev::Released, format!("no verdict after {MAX_ORACLE_BLOCKS} stops")),
                _ if check == SCOPE_CHECK => (
                    Ev::Blocked,
                    format!("its diff changed protected files outside its scope at {MAX_ORACLE_BLOCKS} stops: the engineer amends its files or it is undone"),
                ),
                _ => (Ev::Blocked, format!("oracle failed {MAX_ORACLE_BLOCKS} times at stop: {check}")),
            };
            let _ = write(self.home, &self.g.id, vec![Event::new(ev, &task.id, self.session, self.now).with_note(&note)]);
            let wrong = match task.role != Role::Verify && check != SCOPE_CHECK {
                true => format!(" {WRONG_CHECK}"),
                false => String::new(),
            };
            text.push_str(&format!(
                "[borg hive] {} is {} on the board ({note}); an engineer reopens it with `borg hive open {} --id {} --again`.{wrong}\n\n",
                task.id,
                ev.name(),
                self.g.id,
                task.id
            ));
        } else {
            let (said, next) = self.done(hs.interactive, held, head, input, &path, &left);
            text.push_str(&said);
            if next {
                handed = true;
                hs.beat_ms = self.now;
            }
        }
        text.push_str(&after);
        hs.oracle_blocks = 0;
        hs.blocks_for.clear();
        save(self.session, hs);
        pass_budget(left());
        // A new claim reaches the drone on any round: it must not stop holding a task it never heard of.
        (with_reason(&text, inner(), first_round || handed), false)
    }

    /// The oracle passed at the stop: the done (`finish`), then the next open task claimed for this
    /// drone. The text to block with, and whether it holds a new claim; a done the board did not take
    /// says so and hands over nothing. The done's snapshot keeps within what is `left` of the hook's
    /// time; the next claim is made only when time is left.
    ///
    /// An interactive drone (`interactive`) hears the done's result first, whatever follows: its model
    /// wrote its last message before the checks ran, so without it the model would report a state that
    /// was never true (the live probe: "I didn't tell borg the task is done"). Its done's note is the
    /// stop's own summary (`Note::Stop`), never that message. A runner's drone is told as before.
    fn done(&self, interactive: bool, held: &TaskState, head: String, input: &Value, path: &Path, left: &dyn Fn() -> Duration) -> (String, bool) {
        let task = &held.task;
        let message = crate::harness::plan(self.h.caps(), input);
        let note = if interactive { Note::Stop } else { Note::Said(&message) };
        match finish(self.home, self.g, held, self.session, Path::new(self.cwd), head, &note, left()) {
            Ok(vid) => {
                let said = format!("[borg hive] {} done: {}; verify task {vid} is open for another drone.", task.id, passed(self.g, task));
                match self.hand_next(path, left()) {
                    Some(next) if interactive => (format!("{said} Next: {next}\n\n"), true),
                    Some(next) => (format!("[borg hive] {} done, verify task {vid} opened. Next: {next}\n\n", task.id), true),
                    None if interactive => (format!("{said}\n\n"), false),
                    None => (String::new(), false),
                }
            }
            Err(Unfinished::Lost(why)) => (format!("[borg hive] {why}\n\n"), false),
            Err(Unfinished::Io(e)) => (format!("[borg hive] the done of {} was not recorded: {e}\n\n", task.id), false),
        }
    }

    /// Claims the next open task for this drone (its snapshot within `left`): what it is told of it.
    /// A task whose deps cannot be brought in is passed over for the next. None when no task is open,
    /// the claim was lost or refused (a start not computed in time among them: the task is not passed
    /// over), or less than `MIN_ORACLE` of the hook's time is left: the next prompt offers the task instead.
    fn hand_next(&self, path: &Path, left: Duration) -> Option<String> {
        if left < MIN_ORACLE {
            return None;
        }
        // An interactive drone's confirm waits at most `HAND_CAP` here; the waiter waits on (phase g).
        let cap = bound(self.session).is_some_and(|hs| hs.interactive).then_some(interactive::HAND_CAP);
        let won = loop {
            let now = self.clock();
            let b = board::fold_on(&board::read(path), &node(self.home));
            let next = next_open(self.home, self.g, &b, self.session, &drone_of(self.session), self.repo, self.cwd, now)?;
            match claim(self.home, self.g, self.session, &next.task, true, self.cwd, now, left, cap) {
                Ok(won) => break won,
                // Passed over, as `borg hive claim` does: `next_open` picks another.
                Err(Refused::Deps(_)) => refuse(self.session, &next.task.id),
                Err(Refused::Blocked(_)) => {}
                Err(_) => return None,
            }
        };
        count(self.home, stats::HIVE_CLAIM, self.session, &self.g.id, &won.task.id, self.repo);
        let now = self.clock();
        let key = format!("{}@{}", won.task.id, won.live(now).map_or(0, |l| l.2));
        // Not final within the cap: not handed now (no work before a claim is final, phase f F5), so not
        // marked shown; the stop asks for the waiter, which hands it once final (wait.rs `pending`).
        if cap.is_some() && !is_final(self.home, &self.g.id, &won, now) {
            return None;
        }
        hook::append(self.session, &format!("{TASK_MARKER}{key}"));
        let risk = plan::escalate(self.home, self.g, &won.task, self.session, self.cwd, now).map(|t| format!("\n{t}")).unwrap_or_default();
        Some(format!("{}{risk}", describe(self.home, self.g, &won, self.session, self.cwd)))
    }
}

/// The `done` of the claim `held` (claimed at `since`) by `session`, with the snapshot of the work its
/// caller took (`head`, verdict.rs `snapshot`; empty: none), and the verify task it opens, listing the
/// guard's flags of the claim. The done is stamped once its snapshot is taken: the line lands at its ms.
fn done_events(home: &Path, g: &Goal, held: &TaskState, session: &str, head: String, since: i64) -> (Event, Task) {
    let now = learn::now_ms();
    let mut done = Event::new(Ev::Done, &held.task.id, session, now);
    done.head = head;
    done.claim = Some(since);
    let flags: Vec<String> = guard::flags_of(home, &g.id, session, since, now).iter().map(guard::render).collect();
    let mut check = verify_task(&held.task, held.rework, session);
    check.flags = flags;
    (done, check)
}

/// What a done's note on the board says (`finish`): what a verifier, a rework and the playbooks read of
/// the attempt (board.rs `render_change`, playbook.rs, rework.rs).
enum Note<'a> {
    /// Words written with the result known or as the done's own claim: `borg hive done --note`, or a
    /// runner drone's last message (which it writes knowing its stop runs the checks).
    Said(&'a str),
    /// The stop's own summary (`stop_note`): the checks that passed, the files the attempt changed and
    /// its commits. An interactive drone's last message is written before its checks ran and may say
    /// anything of the task's state; the summary is only what the stop knows (live probe, phase g).
    Stop,
}

/// What a passed task's checks are said as: `its checks passed (a, b)`.
fn passed(g: &Goal, t: &Task) -> String {
    let checks: Vec<String> = oracle::checks(g, t).iter().map(|c| c.name.clone()).collect();
    match checks.is_empty() {
        true => "it has no checks".into(),
        false => format!("its checks passed ({})", checks.join(", ")),
    }
}

/// The stop's summary of an attempt (`Note::Stop`) from its claim's snapshot `base` to its done's
/// `head` in `dir`: its checks passed, the files it changed and its commits' subjects (borg's own
/// snapshot commits left out), each git call within `within` (at most 5 s). Only the checks when either
/// snapshot is missing.
fn stop_note(g: &Goal, t: &Task, base: &str, head: &str, dir: &Path, within: Duration) -> String {
    let mut note = passed(g, t);
    if base.is_empty() || head.is_empty() {
        return note;
    }
    let cap = within.min(Duration::from_secs(5));
    let files = run::git(dir, &["diff", "--name-only", base, head], cap).unwrap_or_default();
    let files: Vec<&str> = files.lines().map(str::trim).filter(|f| !f.is_empty()).collect();
    if !files.is_empty() {
        let more = files.len().saturating_sub(8);
        note.push_str(&format!("; it changed {}{}", files[..files.len() - more].join(", "), if more > 0 { format!(" (+{more} more)") } else { String::new() }));
    }
    let log = run::git(dir, &["log", "--format=%s", &format!("{base}..{head}")], cap).unwrap_or_default();
    let subjects: Vec<&str> = log.lines().map(str::trim).filter(|s| !s.is_empty() && *s != verdict::SNAPSHOT_SUBJECT).collect();
    if !subjects.is_empty() {
        note.push_str(&format!("; its commits: {}", subjects.join(" | ")));
    }
    note
}

/// The done's precheck (S-done): what the verifier would flag in the attempt `held` has in `dir`, its
/// diff from its claim's start to `head` (the snapshot the done would carry) judged by the verifier's
/// own rule under the definition it holds (verdict.rs `flagged`), as the text that refuses the done:
/// each path, why, and how to go on. None when nothing is flagged; also when the diff cannot be read
/// (no snapshot, git failed: said on stderr), as the verifier still judges it. With the refusal,
/// whether the files of a waiting scope ask (`admit`) would admit every flagged path.
fn scope_refusal(home: &Path, g: &Goal, held: &TaskState, session: &str, dir: &Path, head: &str, admit: Option<&[String]>) -> Option<(String, bool)> {
    // A done as a borg before this check wrote it (a test of the verifier's own guard, tests only).
    if std::env::var_os(crate::harness::TEST_ENV).is_some() && std::env::var_os("BORG_HIVE_UNCHECKED_DONE").is_some() {
        return None;
    }
    let id = &held.task.id;
    let Status::Claimed { since, .. } = held.status else { return None };
    let here = node(home);
    let base = board::read(&tasks_path(home, &g.id))
        .into_iter()
        .rev()
        .find(|e| e.ev == Ev::Claim && e.task == *id && e.by == session && e.ms == since && board::of_node(&e.node, &here))
        .map(|e| e.base)
        .filter(|b| !b.is_empty());
    let Some(base) = base else {
        eprintln!("borg hive: {id}'s claim names no start commit: its done is not prechecked (its verifier still judges it)");
        return None;
    };
    let root = repo_top(dir);
    let read = Some(head).filter(|h| !h.is_empty()).and_then(|head| verdict::diff(root, &base, head));
    let Some(changed) = read else {
        eprintln!("borg hive: the diff of {id} could not be read before its done: not prechecked (its verifier still judges it)");
        return None;
    };
    let flagged = verdict::flagged(g, root, &changed, &held.task.files);
    if flagged.is_empty() {
        return None;
    }
    let admitted = admit.is_some_and(|files| verdict::flagged(g, root, &changed, files).is_empty());
    let oracle = guard::oracle_files(g, root);
    let lines: Vec<String> = flagged
        .iter()
        .map(|p| match oracle.contains(&p.path) {
            true => format!("- {} ({}): the goal's check runs it, so no scope admits it: undo it, and tell the planner if the check is wrong: `borg hive tell \"...\" --to planner`", p.path, p.why),
            false => format!("- {} ({}): undo it, or ask the engineer to widen {id}: `borg hive scope --files +{} \"why\"`", p.path, p.why, p.path),
        })
        .collect();
    let why = format!(
        "the verifier would reject it: it changes files outside {id}'s scope that the verifier protects:\n{}\nNever reshape a test to get past this. Keep working on what is in scope meanwhile.",
        lines.join("\n")
    );
    Some((why, admitted))
}

/// The name a stop's block gives the done's precheck (`scope_refusal`), as a failed check's.
const SCOPE_CHECK: &str = "scope";

/// What a failing check's block and its cap add: a check no tree passes is the engineer's to change
/// (scope.rs), never the drone's to work around.
const WRONG_CHECK: &str = "If the check itself is wrong on any tree, ask the engineer: `borg hive scope --accept NAME=CMD \"why\"`.";

/// Why a `done` took no effect.
enum Unfinished {
    /// The board could not be written.
    Io(String),
    /// Not written: the claim had passed to another drone. What to tell the drone.
    Lost(String),
}

/// Publishes the `done` of the claim `held` by `session` in `dir`, with the snapshot `head` its caller
/// took and prechecked (`scope_refusal`; `done_events`, its note's git calls `within` that time), and
/// the verify task it opens in one write, written only while the board as it stands under the file's
/// lock still gives the session that claim (`write_if`): the done names it (`Event::claim`) and is then
/// an attempt for good (phase f F5, board.rs `Fold::permanent`), which no claim that arrives later voids.
/// A claim that passed to another drone meanwhile (a suite that ran past the lease, a rival's earlier
/// claim from a suspected node) writes nothing: nothing is counted and the drone is told who holds the
/// task. The verify task's id when it took. `borg hive done` and the stop share it; the done's note is
/// `note` (`Note`).
#[allow(clippy::too_many_arguments)]
fn finish(home: &Path, g: &Goal, held: &TaskState, session: &str, dir: &Path, head: String, note: &Note, within: Duration) -> Result<TaskId, Unfinished> {
    let path = tasks_path(home, &g.id);
    let id = &held.task.id;
    let since = match held.status {
        Status::Claimed { since, .. } => since,
        _ => i64::MIN,
    };
    let started = Instant::now();
    let (done, check) = done_events(home, g, held, session, head, since);
    let (vid, ms) = (check.id.clone(), done.ms);
    let here = node(home);
    let note = match note {
        Note::Said(n) => n.to_string(),
        Note::Stop => {
            let base = board::read(&path)
                .into_iter()
                .rev()
                .find(|e| e.ev == Ev::Claim && e.task == *id && e.by == session && e.ms == since && board::of_node(&e.node, &here))
                .map(|e| e.base)
                .unwrap_or_default();
            stop_note(g, &held.task, &base, &done.head, dir, within.saturating_sub(started.elapsed()))
        }
    };
    let lines = vec![done.with_note(&hook::one_line(&crate::notes::redact(&note), 200)), Event::open(check, session, ms)];
    let landed = write_if(home, &g.id, |events| holding(&board::fold_on(events, &here), &here, id, session, learn::now_ms()).map(|_| lines))
        .map_err(|e| Unfinished::Io(e.to_string()))?;
    let events = board::read(&path);
    let b = board::fold_on(&events, &here);
    let Some(ms) = landed.map(|l| l[0].ms) else {
        let held = b.tasks.get(id).map_or_else(|| "gone".to_string(), |s| held_by(s, learn::now_ms()));
        return Err(Unfinished::Lost(format!("{id}'s done did not take effect: the task is {held} now. {NOT_YOURS}")));
    };
    // The attempt's commits, for a verifier or an integrator on another machine (a store in a mesh only).
    if let Some(a) = verdict::attempt_of(&events, &b, id, session, ms)
        && let Err(e) = commits::publish(home, &g.id, &a, dir)
    {
        eprintln!("borg hive: {e}");
    }
    count(home, stats::HIVE_DONE, session, &g.id, id, &queen::repo_name(&dir.display().to_string()));
    Ok(vid)
}

/// `f()` with the claim of `task` (of `goal`) by `session` renewed `every` (`HEARTBEAT_MS`) until it
/// returns: a suite run outside the hooks (`borg hive done`, an integration's checks) renews nothing
/// else. A beat of a claim that lapsed meanwhile takes no effect (board.rs `apply`).
pub(crate) fn renewing<T>(home: &Path, goal: &str, task: &str, session: &str, every: Duration, f: impl FnOnce() -> T) -> T {
    let beat = every.max(Duration::from_millis(1));
    std::thread::scope(|s| {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        s.spawn(move || {
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(beat) {
                let _ = write(home, goal, vec![Event::new(Ev::Heartbeat, task, session, learn::now_ms())]);
            }
        });
        let out = f();
        drop(tx);
        out
    })
}

/// What is left of the stop's time, for the conscience and the experts after the hive: through the
/// environment, which expert_route.rs `stop_budget` reads back one call down this stack.
fn pass_budget(left: Duration) {
    // SAFETY: no thread of this process reads or writes the environment now. The only threads that may
    // still run are a finished command's pipe reader and stdin writer (runner.rs `run_bounded`, not
    // joined once `DRAIN_GRACE` passed), and they touch nothing but their pipe.
    unsafe { std::env::set_var(crate::expert_route::STOP_BUDGET_ENV, left.as_millis().to_string()) };
}

/// The verify task a `done` of `t` opens: the same scope, deps and checks, never claimable by its
/// author (the session, and the drone it is an iteration of).
pub fn verify_task(t: &Task, rework: u32, author: &str) -> Task {
    let drone = Some(drone_of(author)).filter(|d| !d.is_empty());
    Task {
        id: format!("{}.v{}", t.id, rework + 1),
        role: Role::Verify,
        title: format!("verify {}: {}", t.id, t.title),
        repo: t.repo.clone(),
        files: t.files.clone(),
        deps: t.deps.clone(),
        accept: t.accept.clone(),
        of: Some(t.id.clone()),
        not_by: std::iter::once(author.to_string()).chain(drone).collect(),
        flags: Vec::new(),
        from: None,
        group: None,
    }
}

/// A `Confirm` line's outcome for a claim refused before its write.
pub const REFUSED: &str = "refused";
/// A `Confirm` line's outcome for a claim a waiter handed that a turn of its session took up
/// (interactive.rs `taken_up`): `waited_ms` is the wake's latency, from its hand to its take-up on this
/// node's clock. No confirm window: a reader of the windows (phase e E4) leaves it out.
pub const TAKEN: &str = "taken";

/// `<goal>/confirms.jsonl`: each claim's confirm window across nodes, and each claim refused before its
/// write (`claim`), local, never synced.
pub fn confirms_path(home: &Path, goal: &str) -> PathBuf {
    dir(home, goal).join("confirms.jsonl")
}

/// One confirm window: a claim that won its read-back on a board with live drones on other nodes
/// waited `waited_ms` for a sync round with each of its `peers` (`heard_back`), then folded the board
/// again: `won`, `lost`, or `cap` (it kept the claim though a peer's round did not complete in time).
/// A claim not written because a rival's had landed since the pick (`claim`'s pre-write check) is a
/// line too, `refused`, with no peers and no wait: a race no board shows, which cost the session it ran in.
/// A wake's take-up is one too (`TAKEN`, phase g G6), with no peers and its latency as the wait.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Confirm {
    pub v: u8,
    pub ms: i64,
    pub task: TaskId,
    pub session: String,
    pub peers: usize,
    pub waited_ms: u64,
    pub outcome: String,
}

/// The members this node completed a sync session with lately (presence.rs `synced`): the peers a round
/// reaches now. Empty in a store that never synced, on one node, and with the mesh daemon stopped. A
/// member whose last dial failed is not among them (the daemon drops its entry, presence.rs
/// `forget_synced`) until a session with it completes again.
fn reachable(home: &Path, now: i64) -> Vec<String> {
    presence::synced(home).into_iter().filter(|(_, ms)| now - ms <= PEER_HEARD_MS).map(|(k, _)| k).collect()
}

/// Waits until a sync session with each of `peers` that started after `after` (this node's clock) has
/// completed (presence.rs `synced`), at most `cap`: every line written here before `after` reached
/// them, and theirs from before it came back. Whether every one did, and how long it waited. A record
/// later than the clock now (the clock stepped back since it was written) proves nothing.
fn heard_back(home: &Path, peers: &[String], after: i64, cap: Duration) -> (bool, Duration) {
    let t0 = Instant::now();
    loop {
        let (synced, now) = (presence::synced(home), learn::now_ms());
        if peers.iter().all(|k| synced.get(k).is_some_and(|&ms| ms > after && ms <= now)) {
            return (true, t0.elapsed());
        }
        if t0.elapsed() >= cap {
            return (false, t0.elapsed());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A run's start on `goal` (run.rs, phase e E2b): this node joins the goal (a `board::JOIN` line, phase
/// f F4: a participant, `Board::participants`, before any of its drones claims) with the `drones` it
/// runs (a roster line, `roster`), in one write, then, with peers reachable, the wait until a round with
/// each completed after it (`heard_back`, at most `START_CAP`). The peers' runners announced at their
/// start too, so the drones start knowing every node's share. The wait is on every reachable member,
/// not the participants alone: a peer starting at this moment has not joined here yet, and it is spent
/// once per run. What it waited on: (peers, every one heard back, the wait).
pub(crate) fn announce(home: &Path, goal: &str, drones: &[String]) -> std::io::Result<(usize, bool, Duration)> {
    let now = learn::now_ms();
    let line = |note: &str| Event::new(Ev::Heartbeat, "", "runner", now).with_note(note);
    write(home, goal, vec![line(board::JOIN), line(&board::roster_note(drones))])?;
    let written = learn::now_ms();
    let peers = reachable(home, written);
    if peers.is_empty() {
        return Ok((0, true, Duration::ZERO));
    }
    let (all, waited) = heard_back(home, &peers, written, START_CAP);
    Ok((peers.len(), all, waited))
}

/// The drones this node's runner still runs on `goal` (board.rs `Board::rosters`): written at the run's
/// start and at each drone's end, so a drone that ended takes no place in its node's share
/// (`next_for`). A heartbeat of no task: the fold takes it for nothing else, a node from before reads
/// it the same.
pub(crate) fn roster(home: &Path, goal: &str, drones: &[String]) -> std::io::Result<()> {
    write(home, goal, vec![Event::new(Ev::Heartbeat, "", "runner", learn::now_ms()).with_note(&board::roster_note(drones))]).map(drop)
}

/// This node's runner says on `goal`'s board that it is still in the goal (`board::BEAT`, every
/// `BEAT_MS`, run.rs) or that it leaves it (`board::LEAVE`, at the run's end, also on SIGINT or SIGHUP):
/// a heartbeat of no task, as `roster`'s (phase f F4). A beat goes to the goal's beats store (`beat`),
/// never to the board's file.
pub(crate) fn member(home: &Path, goal: &str, note: &str) -> std::io::Result<()> {
    let line = Event::new(Ev::Heartbeat, "", "runner", learn::now_ms()).with_note(note);
    match note == board::BEAT {
        true => beat(home, goal, line),
        false => write(home, goal, vec![line]).map(drop),
    }
}

/// This node's beat `line` on `goal` (phase g, board.rs `beats_path`): stamped and marked under the
/// board file's lock as any line of the board is (`land`: above every line the node holds, beats
/// included, with its `seq` and marks), then kept in the goal's beats store in place of this node's
/// earlier beats (`board::merge_beats`). The board's synced file does not grow: an idle node costs the
/// goal one record, whatever its time idle. The mesh signs and sends it in its next session.
fn beat(home: &Path, goal: &str, line: Event) -> std::io::Result<()> {
    let path = tasks_path(home, goal);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::live::locked(&path, || {
        let file = board::read(&path);
        let events = board::landing(&file, stamped(home, goal, vec![line])?);
        let records: Vec<board::BeatRecord> = events
            .iter()
            .map(|e| serde_json::to_string(e).map(|line| board::BeatRecord { line, seen: learn::now_ms(), key: String::new(), sig: String::new() }))
            .collect::<Result<_, _>>()
            .map_err(std::io::Error::other)?;
        board::merge_beats(&path, records, |_| true).map(drop)
    })
}

/// This node says on `goal`'s board that its board is finished at its newest integration (phase f F6,
/// `board::finished_note`; a heartbeat of no task, as `member`'s): written under the board lock only while
/// the file there folds finished and this run has said so for no other integration since its join, once
/// per integration. From that line on no session of this node can write a line that changes the board
/// while it stays finished (`board::FINISHED`), and a peer holding it with every line of this node
/// before it ends on it (`Board::finished_for_good`). Whether it wrote.
pub(crate) fn finish_line(home: &Path, goal: &str) -> std::io::Result<bool> {
    let here = node(home);
    let line = |b: &Board| {
        let i = board::finish_due(b, &here)?;
        Some(vec![Event::new(Ev::Heartbeat, "", "runner", learn::now_ms()).with_note(&board::finished_note(i))])
    };
    write_if(home, goal, |events| line(&board::fold_on(events, &here))).map(|w| w.is_some())
}

/// This node says on `goal`'s board that each of `stuck` is stuck, with why (phase f F6, `board::stuck`;
/// a heartbeat of no task): each only while the board under the lock has no standing stuck line of it
/// saying the same (`Board::stuck`).
pub(crate) fn stuck_lines(home: &Path, goal: &str, stuck: &[(TaskId, String)]) -> std::io::Result<()> {
    let here = node(home);
    let lines = |b: &Board| {
        let new = stuck.iter().filter(|(t, why)| b.stuck.get(t).is_none_or(|(_, _, w)| w != why));
        let lines: Vec<Event> = new.map(|(t, why)| Event::new(Ev::Heartbeat, "", "runner", learn::now_ms()).with_note(&board::stuck_note(t, why))).collect();
        (!lines.is_empty()).then_some(lines)
    };
    write_if(home, goal, |events| lines(&board::fold_on(events, &here))).map(drop)
}

/// This node says on `goal`'s board which tasks it may not take (phase f F6, `board::declines_due`;
/// a heartbeat of no task each): under the lock, each claimable task `allowed` refuses here that this
/// run has not declined since its join, so another node's `board::stuck` stops leaving it to this one.
pub(crate) fn decline_lines(home: &Path, goal: &str, allowed: impl Fn(&board::TaskState) -> bool) -> std::io::Result<()> {
    let here = node(home);
    let lines = |b: &Board| {
        let due = board::declines_due(b, &here, learn::now_ms(), &allowed);
        let lines: Vec<Event> = due.iter().map(|t| Event::new(Ev::Heartbeat, "", "runner", learn::now_ms()).with_note(&board::decline_note(t))).collect();
        (!lines.is_empty()).then_some(lines)
    };
    write_if(home, goal, |events| lines(&board::fold_on(events, &here))).map(drop)
}

/// This node says on `goal`'s board that it may not run the integration `task` (phase f F7, integrate.rs
/// `gate`: an accepted attempt's commits cannot come here), under the lock, once a run (`Board::declines`):
/// a peer that lacks some commits waits for this node to integrate only while it has not said so, so two
/// nodes that each refused the other's line end stuck instead of each waiting for the other.
pub(crate) fn decline_integration(home: &Path, goal: &str, task: &str) -> std::io::Result<()> {
    let here = node(home);
    let line = || vec![Event::new(Ev::Heartbeat, "", "runner", learn::now_ms()).with_note(&board::decline_note(task))];
    write_if(home, goal, |events| (!board::fold_on(events, &here).declines(&here, task)).then(line)).map(drop)
}

/// `drone`'s session on `goal` started (`start`) or ended at `ms` (board.rs `Board::sessions`; run.rs
/// `run_session` writes them only for a session that has not claimed within its task's grace): another
/// node leaves this node's task to a drone whose session started and has not claimed yet, and an ended
/// session that claimed nothing holds nothing back. A heartbeat of no task, as `roster`'s. A start,
/// `ms` the session's `clock`, is written only while the session is still unsettled on the board as it
/// stands under the lock (`settled_since`): its stamp is above every line there, so one written after
/// the session's claim would read on every node as a session that never claims. Whether it wrote.
pub(crate) fn session(home: &Path, goal: &str, drone: &str, start: bool, ms: i64) -> std::io::Result<bool> {
    let line = Event::new(Ev::Heartbeat, "", "runner", ms).with_note(&board::session_note(drone, start));
    if !start {
        return write(home, goal, vec![line]).map(|_| true);
    }
    let here = node(home);
    write_if(home, goal, |events| (!settled_since(&board::fold_on(events, &here), &here, drone, ms)).then(|| vec![line])).map(|w| w.is_some())
}

/// Whether `drone` of `node` claimed or ended a session on `b` since `at` (a `clock` reading at its
/// session's start): every line this node writes after the reading is stamped at or above it.
pub(crate) fn settled_since(b: &Board, node: &str, drone: &str, at: i64) -> bool {
    b.sessions.get(&(node.to_string(), drone.to_string())).is_some_and(|(_, settled)| *settled >= at)
}

/// This node's hybrid clock on `goal`'s board now (`board::stamp`): every line it writes from here on
/// is stamped at or above it, every line it holds below it (barring its wall clock stepping back).
pub(crate) fn clock(home: &Path, goal: &str) -> i64 {
    board::stamp(&board::read(&tasks_path(home, goal)), &node(home), learn::now_ms())
}

/// The state of `id` on `b` while `session` of node `here` holds it at `now`.
fn holding(b: &Board, here: &str, id: &str, session: &str, now: i64) -> Option<TaskState> {
    b.tasks.get(id).filter(|s| s.held_by(here, session, now)).cloned()
}

/// Whether `who` may claim `id` on board `b` at `now`, as `claim` decides under the board file's lock:
/// the task is there, and `who` holds it already (the claim renews it) or is `eligible` for it (board.rs,
/// phase f F8: the fold's own rule, so a claim written is one the fold takes, stamped above every line
/// held and past the lease it takes over, `board::claim_at`; before, the check read the lease alone and
/// a claim the fold then refused for an unmet dep or a bar was written and lost), and a task `picked`
/// for the session (`next_open`) is not one another node's idle drones now take (`left_to_another`:
/// that node's line, which made it the owner's, arrived since the pick).
pub(super) fn claimable_for(b: &Board, id: &str, who: board::Actor, picked: bool, now: i64) -> bool {
    let open = |s: &TaskState| s.held_by(who.node, who.session, now) || board::claimable_by(b, s, who, now);
    b.tasks.get(id).is_some_and(|s| open(s) && !(picked && board::left_to_another(b, s, who.node, now)))
}

/// The confirm window (phase e E4) of `session`'s claim of `id` at `now`, written at `written`: waits
/// for a sync round with each of `peers` that started after it (`heard_back`, at most `CONFIRM_CAP`),
/// folds the board again and writes the outcome to `confirms.jsonl`. The task while the claim still
/// holds it, and the wait.
fn confirm(home: &Path, goal: &str, id: &str, session: &str, now: i64, written: i64, peers: &[String]) -> (Option<TaskState>, Duration) {
    let (all, waited) = heard_back(home, peers, written, CONFIRM_CAP);
    let here = node(home);
    let again = holding(&board::fold_on(&board::read(&tasks_path(home, goal)), &here), &here, id, session, now);
    let outcome = match (&again, all) {
        (None, _) => "lost",
        (Some(_), true) => "won",
        (Some(_), false) => "cap",
    };
    let line =
        Confirm { v: 1, ms: now, task: id.into(), session: session.into(), peers: peers.len(), waited_ms: waited.as_millis() as u64, outcome: outcome.into() };
    let _ = append_line(&confirms_path(home, goal), &line);
    (again, waited)
}

/// What `session`'s claim of `id`, written at `written` and read back won on `b`, waits for from other
/// nodes before any work on it: None with no other goal participant on `b` (nothing to wait for), else
/// the task while the claim still holds once it is final, and the wait. Final (phase f F5): every other
/// participant's watermark is past the claim's stamp (board.rs `Board::unconfirmed`), so every line of
/// theirs not here yet is stamped above it and no rival's claim that sorts before it can still arrive
/// from them; the fold's winner then is the winner on every node for good. The wait has no cap: it ends
/// when the claim is final, lost, or every participant it waits for is suspected (`SUSPECT_MS` without a
/// new line of it here, the failure detector of F4; a healthy peer acks a claim a line's way after it
/// holds it, run.rs `membership`, else at its next beat). Only then may a claim proceed unconfirmed, and
/// a rival's claim from that node can still win later: its work is then at most a duplicate attempt, its
/// done kept (board.rs `Fold::permanent`). A participant that writes no watermark (a runner from before
/// F5, a node with no runner) is waited for as before F5: the confirm window on its mesh round (`confirm`,
/// at most `CONFIRM_CAP`, a liveness bound where no failure detector reads it), or `CROSS_SETTLE` with none
/// reachable. Both `claim` and the integrate claim (integrate.rs `claim_due`) take it; each wait is a line
/// of `confirms.jsonl` (`final`, `suspected`, `lost`; `won`/`cap` for the window). A `cap` (an interactive
/// drone's stop, phase g correction 9) ends the wait for watermarks there, the claim still held and not
/// final (`cap`): its caller hands nothing then (`is_final`), and the waiter waits on with none.
#[allow(clippy::too_many_arguments)]
pub(super) fn across(
    home: &Path,
    goal: &str,
    id: &str,
    session: &str,
    now: i64,
    written: i64,
    b: &Board,
    cap: Option<Duration>,
) -> Option<(Option<TaskState>, Duration)> {
    let here = node(home);
    if b.participants(&here, learn::now_ms()).len() <= 1 {
        return None;
    }
    let since = holding(b, &here, id, session, now).and_then(|s| s.live(now).map(|(_, _, since)| since)).unwrap_or(now);
    let path = tasks_path(home, goal);
    let t0 = Instant::now();
    let waited_on: Vec<String> = b.unconfirmed(&here, since, learn::now_ms()).0.into_iter().map(String::from).collect();
    let (mut seen, mut b) = (None, b.clone());
    let mut capped = false;
    let confirmed = loop {
        // The board's lines or a beat (its beats store, where a peer's ack lands) changed: fold again.
        let at = board::mark(&path);
        if seen != Some(at) {
            (seen, b) = (Some(at), board::fold_on(&board::read(&path), &here));
        }
        let t = learn::now_ms();
        let Some(s) = holding(&b, &here, id, session, t) else { break None };
        let (waiting, mute) = b.unconfirmed(&here, since, t);
        if waiting.is_empty() {
            let mute: std::collections::BTreeSet<&str> = mute.into_iter().chain([here.as_str()]).collect();
            if mute.len() > 1 {
                // A participant with no watermark: its mesh round, as before F5.
                let peers = participant_peers(home, goal, &mute, &here, reachable(home, t));
                if !peers.is_empty() {
                    let (again, w) = confirm(home, goal, id, session, now, written, &peers);
                    return Some((again, t0.elapsed().max(w)));
                }
                std::thread::sleep(CROSS_SETTLE);
                return Some((holding(&board::fold_on(&board::read(&path), &here), &here, id, session, learn::now_ms()), t0.elapsed()));
            }
            break Some(s);
        }
        if cap.is_some_and(|c| t0.elapsed() >= c) {
            capped = true;
            break Some(s);
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let waited = t0.elapsed();
    let live = b.participants(&here, learn::now_ms());
    let outcome = match &confirmed {
        None => "lost",
        Some(_) if capped => "cap",
        Some(_) if waited_on.iter().any(|n| !live.contains(n.as_str())) => "suspected",
        Some(_) => "final",
    };
    let line = Confirm {
        v: 1,
        ms: now,
        task: id.into(),
        session: session.into(),
        peers: waited_on.len(),
        waited_ms: waited.as_millis() as u64,
        outcome: outcome.into(),
    };
    let _ = append_line(&confirms_path(home, goal), &line);
    Some((confirmed, waited))
}

/// Whether the claim `won` of goal `goal` is final at `now` (phase f F5, `Board::unconfirmed`): no other
/// participant's watermark is short of its stamp.
fn is_final(home: &Path, goal: &str, won: &TaskState, now: i64) -> bool {
    let here = node(home);
    let since = won.live(now).map_or(now, |(_, _, since)| since);
    board::fold_on(&board::read(&tasks_path(home, goal)), &here).unconfirmed(&here, since, now).0.is_empty()
}

/// The mesh members of `reachable` a claim's confirm window waits for (phase f F4, G2): those whose key
/// stamped a join line of a goal participant of `live` other than `here` (borg-mesh origin.rs: each
/// synced line's record names the key that first stamped it). A mesh member that runs nothing on this
/// goal is none of them. A participant whose key is not known here (a node from before F4, which wrote
/// no join line, or whose line's record is not here) makes it wait for every reachable member, as
/// before: a longer wait, never a peer left out.
fn participant_peers(home: &Path, goal: &str, live: &std::collections::BTreeSet<&str>, here: &str, reachable: Vec<String>) -> Vec<String> {
    let others: Vec<&str> = live.iter().copied().filter(|n| *n != here).collect();
    let text = std::fs::read_to_string(tasks_path(home, goal)).unwrap_or_default();
    // The hash of each participant's join lines, as the mesh hashes a line (borg-mesh lines.rs `hash`).
    let mut joins: std::collections::BTreeMap<String, &str> = std::collections::BTreeMap::new();
    for l in text.lines() {
        let Ok(e) = serde_json::from_str::<Event>(l) else { continue };
        if let Some(n) = others.iter().find(|n| **n == e.node).filter(|_| e.by == "runner" && e.task.is_empty() && e.note == board::JOIN) {
            joins.insert(blake3::hash(l.as_bytes()).to_hex().to_string(), n);
        }
    }
    let mut keys: std::collections::BTreeMap<&str, std::collections::BTreeSet<String>> = std::collections::BTreeMap::new();
    let origins = std::fs::read_to_string(home.join("mesh").join("origins.jsonl")).unwrap_or_default();
    for l in origins.lines().filter(|l| !joins.is_empty() && l.contains("\"h\"")) {
        let Ok(o) = serde_json::from_str::<Value>(l) else { continue };
        if let (Some(n), Some(k)) = (o["h"].as_str().and_then(|h| joins.get(h)), o["key"].as_str()) {
            keys.entry(n).or_default().insert(k.to_string());
        }
    }
    if others.iter().any(|n| !keys.contains_key(n)) {
        return reachable;
    }
    reachable.into_iter().filter(|k| keys.values().any(|ks| ks.contains(k))).collect()
}

/// Why a claim took nothing. Only `Lost` is a race worth another try.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Refused {
    /// Another drone won it, or it was not open to this one.
    Lost,
    /// The task's start cannot be had in this tree (start.rs): a dep without verified work, a checkout
    /// that is not at the start. Why. The task is passed over for this session (`refuse`).
    Deps(String),
    /// The task's start was not computed now (start.rs `NoStart::Later`): a git call that failed or ran
    /// out of time, an origin the board could not take. Why. Nothing is marked: a later try may start it.
    Later(String),
    /// Not on this node or in this checkout: no origin here, or a fix task outside a drone's own
    /// worktree or a checkout of its tree. Where it is claimed.
    NotHere(String),
    /// The deps' verified changes conflict (start.rs `NoStart::Conflict`): the claim and a `blocked`
    /// with the reason were written in one go, for the engineer. Why.
    Blocked(String),
    /// The board could not be written.
    Io(String),
    /// The claim won, but its worktree could not be set to the start: why, and where the tree before
    /// is kept. The claim is released.
    Reset(String),
}

/// Marks `task` refused to `session` (`REFUSED_MARKER`).
fn refuse(session: &str, task: &str) {
    hook::append(session, &format!("{REFUSED_MARKER}{task}"));
}

/// Claims `task` for `session` in `cwd` (a claim and a heartbeat in one write, appended only while the
/// task is claimable for it on the board as it stands under the file's lock, `write_if`) and reads the
/// board back: the task when the claim won. Same-node exclusion, with no timing assumption: no session
/// writes a claim of a task another session of its node holds while that holder's lease is live, so two
/// never both hold it, whatever the time between a fold, the lock and the append (phase f F2; before, a
/// sleep and a read-back: Fischer's timed mutex, sound only while a write lands within the sleep). Every
/// line is stamped under that lock (`land`, phase f F3), so a holder's heartbeat that lands after a
/// claim written meanwhile sorts after it and renews nothing retroactively; the holder's lease is timed
/// on this node's clock from when it held the renewal (board.rs `Local`), and a claim that takes over an
/// expired one is stamped past it (`board::claim_at`). With other goal participants it then waits until
/// it is final (`across`, phase f F5: each participant's watermark past its stamp, no cap): a rival's
/// claim from another node that sorts before it is here by then, and the claim is lost before any work
/// on it (released at once, `release_lost`). Only a participant that is suspected meanwhile is not
/// waited for: a rival of its node can still win later, this drone hears it at its next hook
/// (`lost_news`), and a done it wrote first stays an attempt for good. `cap` bounds the wait until final.
///
/// A claim of work in a git repo carries the commit it starts from (`base`, verdict.rs), which the
/// verdict diffs against the done's: for a work task its start (start.rs: the goal's origin plus its
/// deps' accepted changes, `deps_in` naming exactly the ones applied, computed from the board and git
/// objects, pinned), for a fix task its `from` (the combined tree, integrate.rs). Never a snapshot of
/// the tree: what a blocked, rejected or lost attempt left in a reused worktree is in no base. All of it
/// is computed `within` that time before any line is written, and a start that cannot be had refuses the
/// claim with the tree untouched; deps whose changes conflict write the claim and a `blocked` with the
/// reason in one write instead, so the engineer sees it once and no drone tries again. The claim is
/// stamped after (`now` moved on), so it lands at its ms and never wins a race it reached late.
///
/// Where the work runs: in a drone's own worktree (a runner's, `runner_worktree`, or an interactive
/// drone's, `own_worktree`) the tree is reset to the start once the claim won (`reset_to`), the tree
/// before kept in a pinned commit when it held anything else (`start_tree`); a drone's rework gets its
/// previous attempt's change back on top, uncommitted, so its diff still shows what that attempt did.
/// Any other session (an engineer's checkout) claims only where its tree is the start already (or the
/// start with its previous attempt), and nothing is reset. A `picked` claim is refused, unwritten, when
/// the session holds a task under the lock: of two claimers of one session (an interactive drone's stop
/// handing its next task and its waiter), only the first writes, so a drone never holds two claims nor
/// gets one task twice.
#[allow(clippy::too_many_arguments)]
fn claim(
    home: &Path,
    g: &Goal,
    session: &str,
    task: &Task,
    picked: bool,
    cwd: &str,
    now: i64,
    within: Duration,
    cap: Option<Duration>,
) -> Result<TaskState, Refused> {
    let path = tasks_path(home, &g.id);
    let drone = drone_of(session);
    let gate = gated(session);
    // An interactive drone takes no task of a plan the engineer has not approved, nor of a goal whose
    // budget is spent (phase g G5): said before anything is written.
    if gate {
        let b = board::fold_on(&board::read(&path), &node(home));
        if !b.plan_approved(task) {
            return Err(Refused::NotHere(unapproved(&g.id, &task.id)));
        }
        if let Some((spent, budget)) = spent_out(home, g, &b) {
            return Err(Refused::NotHere(format!(
                "goal {} spent ${spent:.2} of its ${budget:.2} budget: its drones claim nothing more until the engineer raises it",
                g.id
            )));
        }
    }
    // An interactive drone's node joins the goal before its first claim there (phase g correction 1).
    if gate {
        interactive::ensure_member(home, &g.id).map_err(|e| Refused::Io(format!("could not join goal {}: {e}", g.id)))?;
    }
    let own = own_tree(g, session, cwd);
    if let Some(from) = task.from.as_deref().filter(|from| !own && !checkout_of(cwd, from)) {
        return Err(Refused::NotHere(fix_where(&task.id, from)));
    }
    let t0 = Instant::now();
    let (dir, here) = (Path::new(cwd), node(home));
    let events = board::read(&path);
    let b = board::fold_on(&events, &here);
    let start = start_tree(home, g, task, session, &drone, cwd, own, &events, &b, within)?;
    let (base, deps_in, blocked) = match &start {
        Started::Tree { base, deps_in, .. } => (base.clone(), deps_in.clone(), None),
        Started::Conflict(why) => (String::new(), Vec::new(), Some(why.clone())),
        Started::None => (String::new(), Vec::new(), None),
    };
    let left = within.saturating_sub(t0.elapsed());
    // Decided under the board file's lock (`write_if`), on the board as it stands then: a rival's claim
    // that landed since the task was picked (the start takes a while) makes this one refused, not
    // written, and the caller tries the next task. Of two sessions of this node only the first to take
    // the lock writes its claim, whatever stalls either (`claimable_for`). The claim is decided on this
    // node's clock after the fold (`now` moved on, never back) and stamped under the lock (`land`: a
    // hybrid clock above every line in the file, past the lease it takes over, `board::claim_at`), so
    // this node's claims are in stamp order in its file and none sorts below a line it was decided on,
    // however long the pick took.
    let id = task.id.as_str();
    let mut c = Event::new(Ev::Claim, id, session, now).with_drone(&drone);
    c.base = base.clone();
    c.deps_in = deps_in.clone();
    let mut at = now;
    let wrote = write_if(home, &g.id, |events| {
        let b = board::fold_on(events, &here);
        at = (now + t0.elapsed().as_millis() as i64).max(learn::now_ms());
        c.ms = b.tasks.get(id).map_or(at, |s| board::claim_at(s, at));
        c.takes = b.tasks.get(id).and_then(|s| board::takes(s, at));
        let beat = Event::new(Ev::Heartbeat, id, session, c.ms);
        // A picked claim of a session that holds a task by now (another of its claimers won first: an
        // interactive drone's stop and its waiter, phase g) is no claim to write: one claim a drone.
        let fresh = !picked || board::holding(&b, &here, session, at).is_none();
        // The plan's approval is decided on the board under the lock too (phase g G5): monotone, so the
        // check above stands, and no claim of an unapproved task is ever written.
        let approved = !gate || b.tasks.get(id).is_some_and(|s| b.plan_approved(&s.task));
        // An interactive drone that left meanwhile (its leave unbinds it before it releases, interactive.rs
        // `leave_noted`) claims nothing: no claim of a waiter follows its drone's leave.
        let drone_still = !gate || bound(session).is_some();
        let ok = fresh && approved && drone_still && claimable_for(&b, id, board::Actor::new(&here, &drone, session), picked, at);
        // Deps that conflict are a fact of the board, the same for every claimant: the task is blocked
        // with the reason in the claim's own write, so it is said once and never claimed in a loop.
        // Stamped past the claim (`board::stamp`), so it ends the tenure the claim begins.
        let block = blocked.as_ref().map(|why| Event::new(Ev::Blocked, id, session, board::stamp(events, &here, c.ms) + 1).with_note(why));
        ok.then(|| [vec![c, beat], block.into_iter().collect()].concat())
    });
    let now = at;
    let stamp = match wrote {
        Err(e) => return Err(Refused::Io(format!("could not write the board {}: {e}", path.display()))),
        Ok(None) => {
            let line = Confirm { v: 1, ms: now, task: task.id.clone(), session: session.into(), peers: 0, waited_ms: 0, outcome: REFUSED.into() };
            let _ = append_line(&confirms_path(home, &g.id), &line);
            return Err(Refused::Lost);
        }
        Ok(Some(landed)) => landed[0].ms,
    };
    if let Some(why) = blocked {
        return Err(Refused::Blocked(format!("{id} is blocked: {why}")));
    }
    let written = learn::now_ms();
    // Read back: a line of another node merged since can have won (the winner rule); none of this one.
    let b = board::fold_on(&board::read(&path), &here);
    // Lost here or in the wait below: released at once (`release_lost`), so a line that arrives later
    // and makes it win never hands the task to a session that moved on. The tree is as it was: nothing
    // touches it before the claim won.
    let lost = || release_lost(home, &g.id, session, &[format!("{id}@{stamp}")]);
    let Some(mut s) = holding(&b, &here, id, session, now) else {
        lost();
        return Err(Refused::Lost);
    };
    // The confirm window (phase e E4): with drones live on other nodes, a rival's earlier claim may not
    // have synced yet. A round with each reachable peer after the claim brings it, so a lost claim is
    // known before any work starts. On one node no wait (`across`).
    let mut waited = Duration::ZERO;
    if let Some((again, w)) = across(home, &g.id, id, session, now, written, &b, cap) {
        waited = w;
        match again {
            Some(again) => s = again,
            None => {
                lost();
                return Err(Refused::Lost);
            }
        }
    }
    // Pinned once the claim won, before the worktree moves (a verdict and the integration diff from it
    // on any node, and nothing but this ref holds a start once the tree moved on): a lost or refused
    // claim pins nothing.
    if matches!(start, Started::Tree { .. }) && !base.is_empty() && !commits::pin(dir, &g.id, &base) && own {
        let why = format!("could not pin the start {base} of {id}");
        let _ = write(home, &g.id, vec![Event::new(Ev::Released, id, session, learn::now_ms()).with_note(&why)]);
        return Err(Refused::Reset(format!("{why}: the claim of {id} is released")));
    }
    if let Started::Tree { prev, .. } = &start
        && own
        && !base.is_empty()
    {
        // Won: the worktree becomes the start. Its tree is snapshotted now, after any wait (an edit made
        // meanwhile is kept too), with the floor the undo gets (`reset_to`): a claim that won always gets
        // the time to set its tree.
        let left = left.saturating_sub(waited).max(MIN_ORACLE);
        match onto_start(g, &base, &deps_in, prev.as_deref(), dir, left) {
            Ok(note) => {
                let _ = write(home, &g.id, vec![Event::new(Ev::Progress, id, session, learn::now_ms()).with_note(&note)]);
                s.last_progress = note;
            }
            Err(why) => {
                let _ = write(home, &g.id, vec![Event::new(Ev::Released, id, session, learn::now_ms()).with_note(&why)]);
                return Err(Refused::Reset(format!("{why}: the claim of {id} is released")));
            }
        }
    }
    if !cwd.is_empty() {
        let start = if base.is_empty() { "-" } else { base.as_str() };
        hook::append(session, &format!("{CLAIM_START_MARKER}{id}@{stamp} {start} {}", repo_top(dir).display()));
    }
    if let Some(mut hs) = bound(session) {
        hs.beat_ms = now;
        save(session, &hs);
    }
    Ok(s)
}

/// What a claim starts from (`start_tree`).
enum Started {
    /// A git tree: the base, the deps it brought in, and the drone's previous attempt of the task to
    /// reapply (a rework in its own worktree).
    Tree { base: String, deps_in: Vec<String>, prev: Option<Box<verdict::Attempt>> },
    /// The deps' changes conflict: why.
    Conflict(String),
    /// No tree: a verify task, or a directory in no git repo (a pass is then unchecked).
    None,
}

/// What `task`'s claim by `session` (of `drone`) in `cwd` starts from, `own` when `cwd` is the drone's
/// own worktree, on the board `b` (folded from `events`), `within` that time (see `claim`).
///
/// # Errors
/// Why it cannot start here: nothing is written and the tree is untouched.
#[allow(clippy::too_many_arguments)]
fn start_tree(
    home: &Path,
    g: &Goal,
    task: &Task,
    session: &str,
    drone: &str,
    cwd: &str,
    own: bool,
    events: &[Event],
    b: &Board,
    within: Duration,
) -> Result<Started, Refused> {
    let dir = Path::new(cwd);
    if task.role == Role::Verify {
        return Ok(Started::None);
    }
    // Outside git there is no tree to start from, and no dep's work can come in.
    if cwd.is_empty() || run::repo_of(dir).is_none() {
        return match board::closure(b, task).first() {
            Some(d) if verdict::accepted(events, b, d).is_none_or(|a| a.base.is_empty() || a.head.is_empty()) => {
                Err(Refused::Deps(format!("{d} has no verified work to bring into this tree")))
            }
            Some(d) => Err(Refused::Deps(format!("cannot bring {d}'s verified work into this tree: {cwd} is in no git repo"))),
            None => Ok(Started::None),
        };
    }
    let t0 = Instant::now();
    let (base, deps_in) = match &task.from {
        Some(from) => (from.clone(), Vec::new()),
        None => match start::of(home, g, session, task, events, b, dir, within) {
            Ok(s) => (s.commit, s.deps_in),
            Err(start::NoStart::Conflict(why)) => return Ok(Started::Conflict(why)),
            Err(start::NoStart::Here(why)) => return Err(Refused::NotHere(why)),
            Err(start::NoStart::Not(why)) => return Err(Refused::Deps(why)),
            Err(start::NoStart::Later(why)) => return Err(Refused::Later(why)),
        },
    };
    let prev = previous(events, b, &task.id, session, drone, &node(home)).map(Box::new);
    if own {
        return Ok(Started::Tree { base, deps_in, prev });
    }
    // Another session's checkout is never reset: it claims where its tree is the start already (a
    // rework's: or the start with its previous attempt), so its diff is its own work.
    let left = within.saturating_sub(t0.elapsed());
    let Some(snap) = verdict::snapshot(dir, left) else {
        return Err(Refused::Later(format!("no snapshot of this tree could be taken to compare with {}'s start", task.id)));
    };
    let deadline = Instant::now() + within.saturating_sub(t0.elapsed());
    let tree = |rev: &str| integrate::git_ok(dir, &["rev-parse", "--verify", "-q", &format!("{rev}^{{tree}}")], &[], deadline);
    let mut ok = matches!((tree(&snap), tree(&base)), (Ok(a), Ok(b)) if a == b);
    if let Some(p) = prev.as_ref().filter(|_| !ok) {
        ok = matches!((integrate::step(dir, &p.base, &base, &p.head, deadline), tree(&snap)), (Ok((t, c)), Ok(here)) if c.is_empty() && t == here);
    }
    if !ok {
        return Err(Refused::Deps(format!(
            "this checkout is not at {}'s start {} (the goal's origin with its deps' verified work): claim it in a drone's own worktree \
(`borg hive join` in a session makes one), or check out {} here",
            task.id,
            &base[..base.len().min(12)],
            &base[..base.len().min(12)]
        )));
    }
    Ok(Started::Tree { base, deps_in, prev: None })
}

/// The newest attempt of `task` done by `drone` on node `here` (by `session` when it has no drone): a
/// rework's previous attempt, whose change goes back onto its start. Another node's d1 is another
/// drone.
fn previous(events: &[Event], b: &Board, task: &str, session: &str, drone: &str, here: &str) -> Option<verdict::Attempt> {
    let s = b.tasks.get(task)?;
    let mine = |x: &&board::Attempt| board::of_node(&x.node, here) && if drone.is_empty() { x.by == session } else { b.drone(here, &x.by) == Some(drone) };
    let x = s.attempts.iter().filter(mine).max_by_key(|x| x.done)?;
    verdict::attempt_of(events, b, task, &x.by, x.done).filter(|a| !a.base.is_empty() && !a.head.is_empty())
}

/// The note a claim's reset leaves on the board (`onto_start`), which `describe`, `borg top` and the
/// Hive tab show.
pub const START_NOTE: &str = "started from ";

/// The drone's own worktree `dir` set to the start `base` (a claim that won), `within` that time: its
/// tree snapshotted and pinned when it held anything else, reset (`reset_to`), and a rework's previous
/// attempt `prev` put back on top, uncommitted (verdict.rs `bring_in`; a conflict puts nothing back).
/// The note that says so (`START_NOTE`).
///
/// # Errors
/// A snapshot, pin or reset that failed: why, naming the pinned commit once there is one.
fn onto_start(g: &Goal, base: &str, deps_in: &[String], prev: Option<&verdict::Attempt>, dir: &Path, within: Duration) -> Result<String, String> {
    let t0 = Instant::now();
    let left = || within.saturating_sub(t0.elapsed());
    let short = |s: &str| s.chars().take(12).collect::<String>();
    let snap = verdict::snapshot(dir, left()).ok_or("no snapshot of the worktree could be taken before its reset")?;
    let kept = !verdict::changes(dir, base, &snap).is_some_and(|c| c.is_empty());
    if kept && !commits::pin(dir, &g.id, &snap) {
        return Err(format!("the worktree's snapshot {snap} could not be pinned before its reset"));
    }
    let kept_note = if kept { format!("; worktree before the reset: {snap}") } else { String::new() };
    reset_to(dir, base, left())
        .map_err(|why| format!("{why}{}", if kept { format!("; what the worktree held is kept in commit {snap}") } else { String::new() }))?;
    let deps = if deps_in.is_empty() { "no dep's work".to_string() } else { deps_in.join(", ") };
    let mut note = format!("{START_NOTE}{} (the goal's origin with {deps}){kept_note}", short(base));
    if let Some(p) = prev {
        note.push_str(&match verdict::bring_in(dir, &p.base, &p.head, left()) {
            Ok(true) => format!("; your previous attempt {} is back on top, uncommitted", short(&p.head)),
            Ok(false) => String::new(),
            Err(why) => format!("; your previous attempt {} could not be put back ({why})", short(&p.head)),
        });
    }
    Ok(note)
}

/// Whether `cwd` is the drone's own worktree: its runner worktree (`runner_worktree`), or the one an
/// interactive drone of `session` works in (interactive.rs `worktree`). The only trees a claim resets.
fn own_tree(g: &Goal, session: &str, cwd: &str) -> bool {
    runner_worktree(g, session, cwd) || own_worktree(session, cwd)
}

fn own_worktree(session: &str, cwd: &str) -> bool {
    let same = |a: &Path, b: &Path| a == b || std::fs::canonicalize(a).is_ok_and(|a| std::fs::canonicalize(b).is_ok_and(|b| a == b));
    !cwd.is_empty() && bound(session).is_some_and(|hs| hs.interactive && !hs.worktree.is_empty() && same(Path::new(&hs.worktree), Path::new(cwd)))
}

/// An interactive drone's (`hs`) claim `held` of goal `goal`, as it stood before a hook or command acted
/// on it: once `session` no longer holds that claim, what it cost goes into the goal's spend (meter.rs),
/// from `transcript` of harness `harness` (else the one its hooks named last). Nothing for a runner's or
/// a plain session's claim.
fn meter_end(home: &Path, goal: &str, session: &str, hs: &HiveSession, held: &TaskState, harness: &str, transcript: Option<&str>) {
    if !hs.interactive {
        return;
    }
    let Status::Claimed { since, .. } = held.status else { return };
    let b = board::fold_on(&board::read(&tasks_path(home, goal)), &node(home));
    let still = b.tasks.get(&held.task.id).is_some_and(|s| matches!(s.status, Status::Claimed { since: at, ref by, .. } if at == since && by == session));
    if still {
        return;
    }
    let transcript = transcript.or(Some(hs.transcript.as_str()).filter(|t| !t.is_empty()));
    meter::record(home, goal, session, &hs.drone, harness, &format!("{}@{since}", held.task.id), since, transcript, learn::now_ms());
}

/// Why an interactive drone may not claim `task` of `goal` yet (phase g G5).
fn unapproved(goal: &str, task: &str) -> String {
    format!(
        "{task} waits for the engineer's approval of goal {goal}'s plan: no drone takes it before (`borg hive ratify {goal}`, or their yes at the prompt of the session that opened it)"
    )
}

/// Resets the drone's own worktree `dir` to `from` (a claim's start, `onto_start`): `checkout -f` of
/// it (a worktree on a branch, a runner's `hive/<goal>/<n>`, keeps it, moved to `from`; any other is
/// detached there), then `clean -fd` (ignored files, build output, stay), each within `RESET_TIMEOUT`
/// and both `within` (at least `MIN_ORACLE`: a claim that won gets its tree set). What it discards was
/// snapshotted and pinned before.
///
/// # Errors
/// A git call that fails or runs out of time: the claim is released with the reason.
fn reset_to(dir: &Path, from: &str, within: Duration) -> Result<(), String> {
    let deadline = Instant::now() + within.max(MIN_ORACLE);
    // Under git.rs's configs: a repo's post-checkout hook (git-lfs without git-lfs, a slow husky) would
    // fail the checkout or eat the stop's time.
    let run = |args: &[&str]| {
        let mut cmd = git::command(dir);
        cmd.args(args);
        let left = deadline.saturating_duration_since(Instant::now()).min(RESET_TIMEOUT);
        (!left.is_zero()).then(|| crate::runner::run_capped_status(cmd, None, left)).flatten().filter(|(s, _)| s.success()).map(|(_, out)| out)
    };
    let branch = run(&["symbolic-ref", "-q", "--short", "HEAD"]).map(|b| String::from_utf8_lossy(&b).trim().to_string()).filter(|b| !b.is_empty());
    let checkout = match &branch {
        Some(b) => run(&["checkout", "-q", "-f", "-B", b, from]),
        None => run(&["checkout", "-q", "-f", "--detach", from]),
    };
    if checkout.is_none() {
        return Err(format!("could not reset the worktree to {from}"));
    }
    if run(&["clean", "-fdq"]).is_none() {
        return Err(format!("could not clean the worktree after the reset to {from}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------
// The CLI.

/// `borg hive`'s flags (completions.rs).
pub const FLAGS: &[&str] = &[
    "--session",
    "--dir",
    "--spec",
    "--spec-file",
    "--oracle",
    "--timeout",
    "--budget",
    "--repo",
    "--task",
    "--id",
    "--role",
    "--title",
    "--files",
    "--deps",
    "--accept",
    "--note",
    "--scope",
    "--supersedes",
    "--reject",
    "--pass",
    "--again",
    "--json",
    "--here",
    "--drone",
    "--to",
    "--goal",
    "--re",
    "--group",
    "--bind",
    "--stream",
    "--queue",
];

/// The flags only `borg debug hive` reads (`run`, `init`'s hidden oracle; completions.rs).
pub const DEBUG_FLAGS: &[&str] =
    &["--drones", "--harness", "--model", "--base", "--iter-timeout", "--dry-run", "--yes", "--hidden-dir", "--hidden", "--hidden-timeout"];

/// The flags that take no value.
const SWITCHES: &[&str] = &["--pass", "--again", "--json", "--here", "--bind", "--stream", "--queue", "--dry-run", "--yes"];

const USAGE: &str = "usage: borg hive <command> [--dir DIR] [--session ID|DRONE]
  init <goal> --spec TEXT|--spec-file F --oracle NAME=CMD... --budget USD [--timeout S] [--repo NAME[@NODE]]...
       [--task \"ID|role|title|files,..|deps,..|NAME=CMD\"]...  a goal, its oracle and its first tasks (each its check)
  integrate <goal>                                          merge the accepted work and run the oracle on it (git 2.40+)
  approve <goal>                                            run the checks other nodes wrote here (lists each one it approves)
  ratify <goal> [--budget USD]                              the engineer approves the goal's plan from a terminal (its
                                                            interactive drones take no task before); --budget: raise it
  board [<goal>] [--json]                                   the board: tasks by status, holders, rework; in a terminal,
                                                            then the messages to the planner not read here (marked read)
  join [<goal>]                                             inside a session: it becomes a drone of the goal (none: of
                                                            every open goal of this repo), in a worktree of its own
  join [<goal>] --bind --session ID                         a harness plugin binds its session ID (opencode: no session id
                                                            in its shell), as `join` inside that session would
  wait [--stream|--queue]                                   a drone waits for work at no token cost, then prints its task;
                                                            --stream: a plugin's waiter for the session's life, one JSON
                                                            line a wake ({\"wake\": TEXT, \"left\": BOOL}); --queue:
                                                            codex's, started by its stop, waking it with `codex queue`
  join <goal> [--session ID|DRONE]                          a running drone joins the goal
  join <goal> --here [--drone NAME]                         every new session in this repo (or worktree) joins it
  leave [--here]                                            the drone leaves (its claim is released), a session that only
                                                            plans stops planning; --here: new sessions here join no more
  open <goal> --id ID --role work|plan|verify|integrate --title T [--repo R] [--files a,b] [--deps x,y]
       [--accept NAME=CMD]... [--again] [--group K]         an engineer opens a task (a directory in --files is its tree; a file
                                                            the goal's check reads, or a check that cannot pass on any tree, is
                                                            refused); --again amends an open, claimed or blocked one from a
                                                            terminal, its given title, files, deps (not on a claimed one) or
                                                            checks replacing the old, a claim kept; --group K: K attempts
                                                            ID.g1.. by different drones, the best integrated, `borg learn
                                                            --group <goal>/ID` compares them)
  drone commands (the goal is the session's):
  claim [TASK] | progress \"note\" | release | block \"why\" | done [--note N]
  verify --pass | --reject \"why\" (or end the turn with `VERDICT: pass|reject <why>`) | contract \"text\" --scope a,b [--supersedes ID]
  scope [--files +a,-b] [--accept NAME=CMD]... \"why\"     the holder asks the engineer to amend its task's files or checks: a
                                                            message to the planner with the `open --again` line to run
  ask \"text\" [--to TASK|DRONE|SESSION|planner] | tell \"text\" [--to ...] [--re ID]   a message to the goal's drones or its planner
  ask|tell \"text\" --goal G [--to ...] [--re ID]          the planning session (or the engineer's terminal) sends into goal G
                                                            without joining it, and hears its drones' messages to the planner";

const DEBUG_USAGE: &str = "usage: borg debug hive <command> [--dir DIR]
  init <goal> ... [--hidden-dir DIR --hidden CMD... [--hidden-timeout S]]  a goal (borg hive init's flags) with held-out
                                                            checks outside the repo, run after verdicts
  score <goal>                                              the hidden oracle scores the judged tasks not scored yet
  run <goal> --drones N [--harness H] [--model M] [--base REF] [--iter-timeout S] [--dry-run] [--yes]
                                                            N headless drones in worktrees until done or the budget is spent,
                                                            once the engineer approved the plan (--yes: approved here)";

/// The command line: words, flag values (repeatable), and switches.
struct Args<'a> {
    words: Vec<&'a str>,
    values: Vec<(&'a str, &'a str)>,
    switches: Vec<&'a str>,
}

impl<'a> Args<'a> {
    /// `debug`: `borg debug hive`, which also reads `DEBUG_FLAGS`.
    fn parse(args: &[&'a str], debug: bool) -> Result<Args<'a>, String> {
        let mut out = Args { words: Vec::new(), values: Vec::new(), switches: Vec::new() };
        let mut i = 0;
        while i < args.len() {
            let a = args[i];
            if DEBUG_FLAGS.contains(&a) && !FLAGS.contains(&a) && !debug {
                return Err(format!("unknown flag {a}"));
            } else if SWITCHES.contains(&a) {
                out.switches.push(a);
            } else if FLAGS.contains(&a) || DEBUG_FLAGS.contains(&a) {
                let v = args.get(i + 1).ok_or_else(|| format!("{a} needs a value"))?;
                out.values.push((a, v));
                i += 1;
            } else if a.starts_with("--") {
                return Err(format!("unknown flag {a}"));
            } else {
                out.words.push(a);
            }
            i += 1;
        }
        Ok(out)
    }

    fn value(&self, f: &str) -> Option<&'a str> {
        self.values.iter().rev().find(|(k, _)| *k == f).map(|(_, v)| *v)
    }

    fn all(&self, f: &str) -> Vec<&'a str> {
        self.values.iter().filter(|(k, _)| *k == f).map(|(_, v)| *v).collect()
    }

    fn on(&self, f: &str) -> bool {
        self.switches.contains(&f)
    }

    fn list(&self, f: &str) -> Vec<String> {
        self.value(f).map(split_list).unwrap_or_default()
    }
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
}

fn fail(msg: impl std::fmt::Display) -> i32 {
    eprintln!("borg hive: {msg}");
    1
}

fn valid_task_id(id: &str) -> bool {
    (1..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// `ID|role|title|files,..|deps,..|NAME=CMD` (`--task`): the last field, the task's own check, is the
/// rest of the line, so its command may hold a `|` of its own (phase g G5: a check per task).
fn parse_task(s: &str) -> Result<Task, String> {
    let f: Vec<&str> = s.splitn(6, '|').map(str::trim).collect();
    let (Some(id), Some(role), Some(title)) = (f.first(), f.get(1), f.get(2)) else {
        return Err(format!("--task {s:?}: give ID|role|title[|files,..[|deps,..[|NAME=CMD]]]"));
    };
    let role = Role::parse(role).ok_or_else(|| format!("--task {s:?}: role is plan, work, verify or integrate"))?;
    let accept = match f.get(5).filter(|c| !c.is_empty()) {
        Some(c) => vec![Check::parse(c, 600).map_err(|e| format!("--task {s:?}: {e}"))?],
        None => Vec::new(),
    };
    new_task(id, role, title, "", f.get(3).map(|x| split_list(x)).unwrap_or_default(), f.get(4).map(|x| split_list(x)).unwrap_or_default(), accept)
}

fn new_task(id: &str, role: Role, title: &str, repo: &str, files: Vec<String>, deps: Vec<String>, accept: Vec<Check>) -> Result<Task, String> {
    if !valid_task_id(id) {
        return Err(format!("invalid task id {id:?}: use letters, digits, '.', '_', '-', at most 40"));
    }
    // `--to planner` names the planner (message.rs `PLANNER`), so no task can be called so.
    if id == "planner" {
        return Err("invalid task id \"planner\": `--to planner` is the planning session's address".into());
    }
    if title.trim().is_empty() {
        return Err(format!("task {id} needs a title"));
    }
    Ok(Task {
        id: id.into(),
        role,
        title: title.trim().into(),
        repo: repo.into(),
        files,
        deps,
        accept,
        of: None,
        not_by: Vec::new(),
        flags: Vec::new(),
        from: None,
        group: None,
    })
}

/// Why `tasks` cannot be opened together: a dep that names no task (of the batch, or one `known`), or a
/// cycle among them (a after b after a). The board would never finish either.
fn check_deps(tasks: &[Task], known: impl Fn(&str) -> bool) -> Result<(), String> {
    let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    for t in tasks {
        if let Some(d) = t.deps.iter().find(|d| !ids.contains(&d.as_str()) && !known(d)) {
            return Err(format!("task {}: its dep {d} names no task", t.id));
        }
    }
    // A task whose deps in the batch are all placed is placed next; what is never placed is on a cycle.
    let mut placed: Vec<&str> = Vec::new();
    while let Some(t) =
        tasks.iter().find(|t| !placed.contains(&t.id.as_str()) && t.deps.iter().all(|d| placed.contains(&d.as_str()) || !ids.contains(&d.as_str())))
    {
        placed.push(&t.id);
    }
    let cycle: Vec<&str> = ids.into_iter().filter(|i| !placed.contains(i)).collect();
    match cycle.is_empty() {
        true => Ok(()),
        false => Err(format!("tasks {} depend on each other in a cycle", cycle.join(", "))),
    }
}

/// Where the command runs: the store the directory routes to, and the directory.
struct Here {
    home: PathBuf,
    cwd: String,
    now: i64,
}

impl Here {
    fn locate(a: &Args) -> Result<Here, i32> {
        let dir = a.value("--dir").map_or_else(|| std::env::current_dir().unwrap_or_default(), PathBuf::from);
        let cwd = std::path::absolute(&dir).unwrap_or(dir).display().to_string();
        let cfg = match collective::load() {
            Ok(cfg) => cfg,
            Err(e) => return Err(fail(format!("invalid collectives config: {e}"))),
        };
        let collective::Route::Collective { home, .. } = collective::route(cfg.as_ref(), &cwd, &learn::home()) else {
            eprintln!("{}", off_here(cfg.as_ref(), &cwd));
            return Err(1);
        };
        // The drone's scratch state, as its hooks keep it (hook.rs main).
        if std::env::var_os("BORG_STATE").is_none() {
            // SAFETY: single-threaded: the command has started no thread yet.
            unsafe { std::env::set_var("BORG_STATE", home.join("state")) };
        }
        // An interactive drone's command acts in its worktree, wherever its shell is (phase g).
        let cwd = match interactive::env_drone() {
            Some((_, hs)) if a.value("--dir").is_none() && !hs.worktree.is_empty() => hs.worktree,
            _ => cwd,
        };
        Ok(Here { home, cwd, now: learn::now_ms() })
    }

    fn engineer(&self) -> String {
        format!("engineer:{}", learn::engineer(&self.cwd))
    }

    /// The session a drone command acts for, and its goal: `--session` names a hive session, or a drone
    /// (`borg top`'s designation or session prefix); without it, the interactive drone whose session this
    /// runs in, else the one drone of this repo.
    fn drone(&self, named: Option<&str>) -> Result<(String, HiveSession), String> {
        if let Some(n) = named
            && let Some(hs) = bound(n)
        {
            return Ok((n.to_string(), hs));
        }
        // Inside an interactive drone's session: that session (its harness's id, interactive.rs).
        if named.is_none()
            && let Some(d) = interactive::env_drone()
        {
            return Ok(d);
        }
        if named.is_none()
            && let Some(s) = self.runner_session()
            && let Some(hs) = bound(&s)
        {
            return Ok((s, hs));
        }
        let drones = top::drones(&self.home, false, self.now);
        let d = veto::target(&drones, &queen::repo_name(&self.cwd), named)?;
        let session = d.p.session.clone();
        let hs = bound(&session).ok_or_else(|| {
            format!("drone {} ({}) is in no hive goal: `borg hive join <goal> --session {}`", d.designation, short(&session), short(&session))
        })?;
        Ok((session, hs))
    }

    /// The session of the runner drone this command runs inside (`DRONE_ENV`, with `ENV`'s goal when
    /// set): the newest run join naming that drone whose session joined a goal. Every runner worktree
    /// folds to one repo name, so the drones `veto::target` finds by presence and repo could be another
    /// drone's, or this drone's stale session.
    fn runner_session(&self) -> Option<String> {
        let drone = std::env::var(DRONE_ENV).ok().map(|d| d.trim().to_string()).filter(|d| !d.is_empty())?;
        let goal = std::env::var(ENV).ok().map(|g| g.trim().to_string()).filter(|g| !g.is_empty());
        joins(&self.home)
            .into_iter()
            .filter(|j| j.by == "run" && j.drone == drone && !j.session.is_empty() && goal.as_ref().is_none_or(|g| *g == j.goal))
            .filter(|j| session_path(&j.session).exists())
            .max_by_key(|j| j.ms)
            .map(|j| j.session)
    }

    /// The goal a board command shows: named, the session's, or the only one; a conflicting one too
    /// (`goal::read`: the board says why it is stopped).
    fn goal(&self, named: Option<&str>, session: Option<&str>) -> Result<Goal, String> {
        if let Some(n) = named {
            return goal::read(&self.home, n);
        }
        if let Some(hs) = session.and_then(bound) {
            return goal::read(&self.home, &hs.goal);
        }
        match goal::list(&self.home).as_slice() {
            [one] => goal::read(&self.home, one),
            [] => Err("no hive goal in this collective (`borg hive init`)".into()),
            many => Err(format!("{} goals ({}): name one", many.len(), many.join(", "))),
        }
    }

    fn board(&self, g: &Goal) -> Board {
        board::fold_on(&board::read(&tasks_path(&self.home, &g.id)), &node(&self.home))
    }
}

/// Why a hive command in `cwd` does nothing, and the one step that fixes it (phase g live probe: a
/// fresh `borg setup` that took every default leaves each repo in no collective, and `borg hive init`
/// or `join` then refused with a line that named no fix). Nothing is changed for the engineer: a repo
/// joins a collective only by their own setup. A repo kept out of every collective (an `off` path) is
/// said so: the hive does not run there.
fn off_here(cfg: Option<&collective::Config>, cwd: &str) -> String {
    let repo = run::repo_of(Path::new(cwd)).map_or_else(|| cwd.to_string(), |r| r.display().to_string());
    if cfg.is_some_and(|c| collective::kept_off(c, cwd)) {
        return format!(
            "borg hive: {repo} is an `off` path in your collectives config (`borg collective {}`): no collective sees it, so the hive does not run here. Run the hive in a repo of a collective.",
            queen::sh_quote(&repo)
        );
    }
    format!(
        "borg hive: {repo} belongs to no collective, so there is no hive board for it. Add it once, in a terminal: \
`borg setup --private {r}` (this machine only), or `borg setup --team <name> --paths {r}` to share the hive with your team over the mesh; \
then run this again. `borg collective {r}` shows where a repo belongs.",
        r = queen::sh_quote(&repo)
    )
}

/// `borg hive <command>` (see `USAGE`). Exit 0, 1 on an error, 2 on a usage error, 3 when a claim lost.
pub fn main(args: &[&str]) -> i32 {
    cli(args, false)
}

/// `borg debug hive`: `run`, `score` and `init`'s hidden oracle, evaluation's commands.
pub fn debug(args: &[&str]) -> i32 {
    cli(args, true)
}

fn cli(args: &[&str], debug: bool) -> i32 {
    let usage = if debug { DEBUG_USAGE } else { USAGE };
    let a = match Args::parse(args, debug) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("borg hive: {e}\n{usage}");
            return 2;
        }
    };
    let Some((&cmd, rest)) = a.words.split_first() else {
        eprintln!("{usage}");
        return 2;
    };
    if debug && !matches!(cmd, "run" | "score" | "init") {
        eprintln!("{usage}");
        return 2;
    }
    // Refused before the board is looked up: the old names fail as gone, never on the directory's
    // collective (an off repo would answer exit 1 as if the command still ran).
    if !debug && matches!(cmd, "run" | "score") {
        eprintln!("borg hive: unknown command {cmd} (borg debug hive {cmd})\n{usage}");
        return 2;
    }
    let here = match Here::locate(&a) {
        Ok(h) => h,
        Err(code) => return code,
    };
    let session = a.value("--session");
    match (cmd, rest) {
        ("init", [id]) => init(&here, &a, id),
        ("board", [] | [_]) => match here.goal(rest.first().copied(), session) {
            Ok(g) => board_cmd(&here, &g, &a),
            Err(e) => fail(e),
        },
        ("join", [id]) if a.on("--here") => join_here(&here, id, a.value("--drone")),
        // A harness plugin binds the session that ran the join (opencode: its shell has no session id).
        ("join", [] | [_]) if a.on("--bind") => match session {
            Some(s) => interactive::join(&here, rest.first().copied(), s),
            None => fail("`borg hive join --bind` names the session it binds: --session ID"),
        },
        // Inside a session (its harness's id): it becomes an interactive drone (phase g).
        ("join", [] | [_])
            if session.is_none()
                && let Some(s) = interactive::env_session() =>
        {
            interactive::join(&here, rest.first().copied(), &s)
        }
        // Inside a session whose harness's plugin binds it (opencode): the plugin binds it once this
        // command ends, never another session of the directory.
        ("join", [] | [_]) if session.is_none() && interactive::plugin_binds() => {
            println!("[borg hive] this session's harness plugin binds it to the hive once this command ends: nothing more to run.");
            0
        }
        ("join", []) => fail("run `borg hive join` inside the session that is to become a drone"),
        ("join", [id]) => join(&here, id, session),
        ("leave", []) if a.on("--here") => leave_here(&here),
        ("wait", []) => wait::main(&here, session, wait::Mode::of(a.on("--stream"), a.on("--queue"))),
        ("takeup", [goal, key]) => wait::takeup(&here, goal, key, session.unwrap_or_default()),
        ("run", [id]) if debug => run_cmd(&here, &a, id),
        ("sim-drone", []) if std::env::var_os(crate::harness::TEST_ENV).is_some() => sim_drone::main(&here),
        // A session that only plans (plan.rs `planned`) leaves its planning.
        ("leave", [])
            if session.is_none()
                && let Some(s) = interactive::env_session().filter(|s| bound(s).is_none() && !plan::planned(s).is_empty()) =>
        {
            leave_planning(&here, &s)
        }
        ("leave", []) => match here.drone(session) {
            Ok((s, hs)) if hs.interactive => interactive::leave(&here, &s, &hs),
            Ok((s, hs)) => {
                let b = goal::load(&here.home, &hs.goal).map(|g| here.board(&g)).unwrap_or_default();
                if let Some(t) = board::holding(&b, &node(&here.home), &s, here.now) {
                    let _ = write(&here.home, &hs.goal, vec![Event::new(Ev::Released, &t.task.id, &s, here.now).with_note("left the goal")]);
                }
                unbind(&s);
                println!("{} left hive goal {}", short(&s), hs.goal);
                0
            }
            Err(e) => fail(e),
        },
        ("open", [id]) => open(&here, &a, id),
        ("ratify", [id]) => ratify_cmd(&here, &a, id),
        ("claim", [] | [_]) => claim_cmd(&here, session, rest.first().copied()),
        ("progress" | "block", [_, ..]) => {
            let ev = if cmd == "progress" { Ev::Progress } else { Ev::Blocked };
            held_event(&here, session, ev, &rest.join(" "))
        }
        ("release", []) => held_event(&here, session, Ev::Released, ""),
        ("done", []) => done_cmd(&here, session, a.value("--note").unwrap_or_default()),
        ("score", [id]) if debug => score_cmd(&here, id),
        ("integrate", [id]) => integrate_cmd(&here, id),
        ("approve", [id]) => approve_cmd(&here, id),
        ("verify", []) => match (a.on("--pass"), a.value("--reject")) {
            (true, None) => verdict_cmd(&here, session, verdict::Said::Pass),
            (false, Some(note)) if !note.trim().is_empty() => verdict_cmd(&here, session, verdict::Said::Reject(note.trim().into())),
            _ => {
                eprintln!("borg hive verify: give --pass or --reject \"why\"");
                2
            }
        },
        ("scope", [_, ..]) => scope::ask_cmd(&here, session, &a, &rest.join(" ")),
        ("contract", [_, ..]) => contract_cmd(&here, session, &rest.join(" "), a.list("--scope"), a.value("--supersedes")),
        ("ask" | "tell", [_, ..]) => message_cmd(&here, session, a.value("--goal"), cmd, &rest.join(" "), a.value("--to"), a.value("--re")),
        _ => {
            eprintln!("{usage}");
            2
        }
    }
}

fn init(here: &Here, a: &Args, id: &str) -> i32 {
    let spec = match (a.value("--spec"), a.value("--spec-file")) {
        (Some(s), _) => s.to_string(),
        (None, Some(f)) => match std::fs::read_to_string(f) {
            Ok(s) => s,
            Err(e) => return fail(format!("cannot read {f}: {e}")),
        },
        (None, None) => {
            eprintln!("borg hive init: give the goal's --spec TEXT or --spec-file F");
            return 2;
        }
    };
    let timeout = a.value("--timeout").and_then(|t| t.parse().ok()).unwrap_or(600);
    let oracle: Result<Vec<Check>, String> = a.all("--oracle").into_iter().map(|o| Check::parse(o, timeout)).collect();
    let oracle = match oracle {
        Ok(o) if !o.is_empty() => o,
        Ok(_) => {
            eprintln!("borg hive init: give at least one --oracle NAME=CMD: done means the oracle passes");
            return 2;
        }
        Err(e) => return fail(e),
    };
    let Some(budget_usd) = a.value("--budget").and_then(|b| b.parse::<f64>().ok()).filter(|b| *b >= 0.0) else {
        eprintln!("borg hive init: give the goal's --budget USD");
        return 2;
    };
    let repos = a
        .all("--repo")
        .into_iter()
        .map(|r| match r.split_once('@') {
            Some((name, node)) => RepoScope { name: name.into(), node: node.into() },
            None => RepoScope { name: r.into(), node: String::new() },
        })
        .collect();
    let tasks: Result<Vec<Task>, String> = a.all("--task").into_iter().map(parse_task).collect();
    let mut tasks = match tasks.and_then(|t| check_deps(&t, |_| false).map(|()| t)) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let hidden = match (a.value("--hidden-dir"), a.all("--hidden")) {
        (None, cmds) if cmds.is_empty() => None,
        (Some(dir), cmds) if !cmds.is_empty() => {
            let timeout_s = a.value("--hidden-timeout").and_then(|t| t.parse().ok()).unwrap_or(600);
            let h = goal::Hidden { dir: dir.into(), commands: cmds.into_iter().map(String::from).collect(), timeout_s };
            if let Err(e) = goal::validate_hidden(&h, run::repo_of(Path::new(&here.cwd)).as_deref()) {
                return fail(e);
            }
            Some(h)
        }
        _ => {
            eprintln!("borg hive init: a hidden oracle needs --hidden-dir DIR and at least one --hidden CMD");
            return 2;
        }
    };
    let author = learn::engineer(&here.cwd);
    // The commit every work claim in this repo starts from (start.rs): the HEAD of the repo `init` runs
    // in. Any other repo's (a goal made outside git: every repo's) is recorded on the board by the
    // first claim there (start.rs `record`).
    let origin = run::git(Path::new(&here.cwd), &["rev-parse", "--verify", "-q", "HEAD"], Duration::from_secs(5))
        .filter(|sha| !sha.is_empty())
        .map(|sha| vec![goal::Origin { repo: queen::repo_name(&here.cwd), sha }])
        .unwrap_or_default();
    let g = Goal {
        v: 1,
        id: id.into(),
        spec,
        oracle,
        budget_usd,
        repos,
        author: author.clone(),
        created_ms: here.now,
        sig: String::new(),
        hidden,
        held_out: false,
        origin,
    };
    // Plan-time checks before anything is written (scope.rs): a task no change could ever pass is no plan.
    let mut notes = Vec::new();
    for t in &mut tasks {
        match scope::validate(&g, t, &here.cwd, false, |_| true) {
            Ok(n) => notes.extend(n),
            Err(e) => return fail(e),
        }
    }
    if let Err(e) = goal::create(&here.home, &g) {
        return fail(e);
    }
    for n in &notes {
        println!("{n}");
    }
    let by = format!("engineer:{author}");
    let opens: Vec<Event> = tasks.into_iter().map(|t| Event::open(t, &by, here.now)).collect();
    if let Err(e) = write(&here.home, id, opens.clone()) {
        return fail(e);
    }
    println!(
        "hive goal {id}: {} task(s), oracle {}; {}",
        opens.len(),
        g.oracle.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", "),
        dir(&here.home, id).display()
    );
    println!("join a running drone: borg hive join {id} --session <ID|DRONE>; or start drones with {ENV}={id}");
    plan_said(here, &g);
    0
}

/// What `init` or `open` says of the plan its tasks wait on (phase g G5), when a task of it is not
/// approved yet: run by a session's model (the harness's session id in its environment, no drone), the
/// plan question goes to that session (plan.rs `ask_opener`), whose model shows it to the engineer, who
/// answers at their own prompt; from a terminal, the command that approves it. Interactive drones take
/// no task of it before (`next_open`).
fn plan_said(here: &Here, g: &Goal) {
    let b = here.board(g);
    let n = b.unratified().len();
    if n == 0 {
        return;
    }
    let asked = interactive::env_session().filter(|_| !drone_here(here)).and_then(|s| plan::ask_opener(&here.home, g, &b, &s, &here.cwd, here.now));
    let Some((e, cards)) = asked else {
        println!("the plan of {n} task(s) waits for the engineer: `borg hive ratify {}` approves it; interactive drones take none of its tasks before", g.id);
        return;
    };
    println!("[borg hive] The plan of goal {} waits for the engineer's approval (queen question {}); no drone takes a task of it before:", g.id, e.id);
    for c in &cards {
        println!("  {}", plan::render_card(c));
    }
    println!(
        "Show the engineer these tasks and ask them to answer at their own prompt: `yes` approves the plan, `no, because <why>` refuses it. Never answer it yourself: no command run in this session approves it."
    );
}

/// `borg hive ratify <goal> [--budget USD]`: the engineer approves goal `id`'s plan from a terminal (phase g
/// G5; a goal whose tasks were opened from a terminal has no session to ask): every task not approved
/// yet, each a card shown here, approved on the board (plan.rs `ratify_board`), a queued plan question of
/// it answered so, and the decision in plan.jsonl. `--budget USD` raises the goal's budget instead and
/// settles (dismisses) its queued budget question. Never
/// a drone's command, nor one run inside a harness session: a model never approves the plan it works on.
fn ratify_cmd(here: &Here, a: &Args, id: &str) -> i32 {
    if drone_here(here) {
        return fail("the plan is approved by the engineer, never by a drone of a hive goal");
    }
    if interactive::env_session().is_some() || interactive::plugin_binds() {
        return fail(
            "`borg hive ratify` runs in the engineer's own terminal, never inside a session: there the engineer answers the plan question at their prompt (`yes`, or `no, because ...`)",
        );
    }
    let g = match goal::load(&here.home, id) {
        Ok(g) => g,
        Err(e) => return fail(e),
    };
    let engineer = learn::engineer(&here.cwd);
    if let Some(usd) = a.value("--budget") {
        let b = here.board(&g);
        let Some(usd) = usd.parse::<f64>().ok().filter(|u| u.is_finite() && *u > budget_of(&g, &b)) else {
            return fail(format!("--budget {usd}: give dollars above the goal's budget now (${:.2})", budget_of(&g, &b)));
        };
        return match plan::raise_budget(&here.home, id, usd, &engineer, here.now) {
            Ok(()) => {
                // A queued budget question of the goal is settled: the raise answered it.
                for e in queen::open(&here.home)
                    .into_iter()
                    .filter(|e| plan::at_own_prompt(e) && e.payload.as_ref().is_some_and(|p| p["goal"] == id && p["budget"].is_number()))
                {
                    let _ = queen::dismiss(&here.home, &e.id, &format!("the engineer raised the budget to ${usd:.2} (borg hive ratify --budget)"), here.now);
                }
                println!("goal {id}: budget raised to ${usd:.2} (spent ${:.2})", run::goal_spent(&here.home, id).total);
                0
            }
            Err(e) => fail(e),
        };
    }
    let b = here.board(&g);
    let cards = plan::unratified(&b);
    if cards.is_empty() {
        println!("nothing to approve in goal {id}: every task of its plan is approved");
        return 0;
    }
    for c in &cards {
        println!("approve {}", plan::render_card(c));
    }
    // A queued question of the plan is answered, as the engineer would at the prompt.
    for e in
        queen::open(&here.home).into_iter().filter(|e| plan::at_own_prompt(e) && e.payload.as_ref().is_some_and(|p| p["goal"] == id && p["cards"].is_array()))
    {
        let _ = queen::answer(&here.home, &e.id, plan::APPROVE_OPTION, &engineer, here.now);
    }
    let ids: Vec<TaskId> = cards.iter().map(|c| c.task.clone()).collect();
    if let Err(e) = plan::ratify_board(&here.home, id, &ids, &engineer, here.now) {
        return fail(e);
    }
    plan::note_ratified(&here.home, &g.id, &cards, &engineer, here.now);
    let (drones, nodes) = plan::waiting(&here.home, id, &here.board(&g));
    println!("goal {id}: the plan of {} task(s) is approved; {drones} drone(s) on {nodes} node(s) waiting", cards.len());
    0
}

fn board_cmd(here: &Here, g: &Goal, a: &Args) -> i32 {
    let now = learn::now_ms();
    let b = here.board(g);
    if a.on("--json") {
        let rows: Vec<Value> = b
            .tasks
            .values()
            .map(|s| {
                let (by, renewed) = match &s.status {
                    Status::Claimed { by, renewed, .. } => (by.clone(), *renewed),
                    Status::Done { by, ms, .. } | Status::Verified { by, ms, .. } => (by.clone(), *ms),
                    Status::Blocked { by, .. } => (by.clone(), 0),
                    Status::Open => (String::new(), 0),
                };
                json!({"id": s.task.id, "role": s.task.role, "title": s.task.title, "status": s.shown_status(now), "by": by, "renewed": renewed,
                    "rework": s.rework, "deps": s.task.deps, "files": s.task.files})
            })
            .collect();
        println!("{}", Value::Array(rows));
        return 0;
    }
    let mut head = format!(
        "hive goal {} (budget ${:.2}, by {})\n{}\n\n",
        g.id,
        g.budget_usd,
        if g.author.is_empty() { "?" } else { &g.author },
        g.spec.lines().next().unwrap_or_default()
    );
    let alerts = view::alerts(&here.home, g, &b);
    for alert in &alerts {
        head.push_str(&format!("! {alert}\n"));
    }
    if !alerts.is_empty() {
        head.push('\n');
    }
    let body = board::render(&b, now, queen::width());
    let mail = if a.value("--session").is_none() && in_session("").is_none() && !drone_here(here) { planner_mail(here, g) } else { String::new() };
    print!("{head}{body}{mail}");
    0
}

/// The engineer's terminal reads goal `g`'s messages to the planner not read on this node before, each
/// with the command that answers it (a scope ask: its amend, a reply that is no answer to it, and the
/// `--re` reply that is the engineer's no), and marks them read (message.rs `read_here`): a goal opened from
/// a terminal, or planned by a session gone, has no planning session whose prompt shows them. Empty
/// with none.
fn planner_mail(here: &Here, g: &Goal) -> String {
    let read = message::read_here(&here.home, &g.id);
    let msgs = message::read(&messages_path(&here.home, &g.id));
    let new: Vec<&message::Message> = msgs.iter().filter(|m| m.to == message::PLANNER && !read.contains(&m.id)).collect();
    if new.is_empty() {
        return String::new();
    }
    let here_node = node(&here.home);
    let lines: Vec<String> = new
        .iter()
        .map(|m| {
            let (re, to) = (queen::sh_quote(&m.id), queen::sh_quote(&m.by));
            // Any terminal reply to a scope ask is the engineer's no (scope.rs `answer`): its plain
            // reply names no `--re`, and the no is said as one.
            if m.cmd.is_empty() {
                let reply = format!("borg hive tell \"<answer>\" --goal {} --re {re} --to {to}", g.id);
                return format!("- {}\n", message::render(m, &here_node, &reply, false));
            }
            let reply = format!("borg hive tell \"<answer>\" --goal {} --to {to}", g.id);
            let no = format!("borg hive tell \"no: <why>\" --goal {} --re {re} --to {to}", g.id);
            format!("- {} To say no: `{no}`\n", message::render(m, &here_node, &reply, false))
        })
        .collect();
    message::mark_read(&here.home, &g.id, &new.iter().map(|m| m.id.as_str()).collect::<Vec<_>>());
    format!("\nTo the planner, unread here:\n{}", lines.concat())
}

/// `borg hive leave` by session `s` that plans goals but is no drone: its planner record removed, so its
/// prompts hear their drones no more. Their reports to the planner wait in the goal's log, counted in
/// monitoring, for the terminal's `borg hive board`; a question it asked the engineer at its prompt
/// (plan.rs `at_own_prompt`) is answered with `borg hive ratify` in a terminal from then on.
fn leave_planning(here: &Here, s: &str) -> i32 {
    let goals = plan::planned(s);
    plan::forget_planned(s);
    println!(
        "{} plans no hive goal now ({}): its prompts hear their drones no more; `borg hive board <goal>` in a terminal reads their reports to the planner",
        short(s),
        goals.join(", ")
    );
    let asked: Vec<String> = queen::open(&here.home)
        .into_iter()
        .filter(|e| e.session == s && plan::at_own_prompt(e))
        .filter_map(|e| e.payload.and_then(|p| p["goal"].as_str().map(String::from)))
        .collect();
    for g in asked {
        println!("its question about goal {g} waits for `borg hive ratify {g}` in a terminal");
    }
    0
}

fn join(here: &Here, id: &str, named: Option<&str>) -> i32 {
    if let Err(e) = goal::load(&here.home, id) {
        return fail(e);
    }
    let drones = top::drones(&here.home, false, here.now);
    let session = match (veto::target(&drones, &queen::repo_name(&here.cwd), named), named) {
        (Ok(d), _) => d.p.session.clone(),
        // A session with no hook yet (a drone being launched): taken as named.
        (Err(_), Some(n)) if presence::safe(n) == n && n.len() >= 8 => n.to_string(),
        (Err(e), _) => return fail(e),
    };
    if let Err(e) = bind(&here.home, &session, id) {
        return fail(e);
    }
    println!("{} joined hive goal {id}; its next prompt shows its claim or the next open task", short(&session));
    0
}

/// The directory a `--here` join names: the git toplevel of the command's directory (a worktree's own),
/// else the directory.
fn here_dir(here: &Here) -> String {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["-C", &here.cwd, "rev-parse", "--show-toplevel"]);
    match crate::runner::run_capped_status(cmd, None, Duration::from_secs(5)) {
        Some((s, out)) if s.success() && !out.trim_ascii().is_empty() => String::from_utf8_lossy(out.trim_ascii()).into_owned(),
        _ => here.cwd.clone(),
    }
}

fn join_here(here: &Here, id: &str, drone: Option<&str>) -> i32 {
    if let Err(e) = goal::load(&here.home, id) {
        return fail(e);
    }
    let dir = here_dir(here);
    let drone = drone.map_or_else(|| format!("j-{}", &blake3::hash(dir.as_bytes()).to_hex()[..6]), String::from);
    let harness = crate::harness::named_current().map(|h| h.name().to_string()).unwrap_or_default();
    let j = Join { v: 1, ms: here.now, goal: id.into(), dir: dir.clone(), drone: drone.clone(), session: String::new(), harness, by: "join".into() };
    if let Err(e) = append_join(&here.home, id, &j) {
        return fail(e);
    }
    println!("joined {id} as {drone}: the next session in {dir} gets the board (`borg hive leave --here` undoes it)");
    0
}

fn leave_here(here: &Here) -> i32 {
    let dir = here_dir(here);
    let Some(j) = joined(&here.home, &dir) else { return fail(format!("{dir} joined no hive goal")) };
    let leave = Join { v: 1, ms: here.now.max(j.ms + 1), goal: String::new(), dir: dir.clone(), by: "join".into(), ..Join::default() };
    if let Err(e) = append_join(&here.home, &j.goal, &leave) {
        return fail(e);
    }
    println!("new sessions in {dir} join no hive goal ({} keeps the sessions that joined)", j.goal);
    0
}

fn run_cmd(here: &Here, a: &Args, id: &str) -> i32 {
    let Some(drones) = a.value("--drones").and_then(|n| n.parse::<usize>().ok()) else {
        eprintln!("borg debug hive run: give --drones N (1 to {})", run::MAX_DRONES);
        return 2;
    };
    let harness = match a.value("--harness") {
        Some(n) => match crate::harness::by_name(n) {
            Some(h) => h,
            None => {
                eprintln!("borg debug hive run: unknown harness {n} (one of {})", crate::harness::names(", "));
                return 2;
            }
        },
        None => crate::harness::current(),
    };
    let iter_timeout = match a.value("--iter-timeout").map(str::parse::<u64>) {
        None => run::DEFAULT_ITER_TIMEOUT,
        Some(Ok(s)) if s > 0 => Duration::from_secs(s),
        Some(_) => {
            eprintln!("borg debug hive run: --iter-timeout is seconds, above 0");
            return 2;
        }
    };
    let o = run::Opts {
        goal: id,
        drones,
        harness,
        model: a.value("--model"),
        base: a.value("--base").unwrap_or("HEAD"),
        iter_timeout,
        dry_run: a.on("--dry-run"),
        yes: a.on("--yes"),
    };
    run::main(&here.home, &here.cwd, &o)
}

fn open(here: &Here, a: &Args, goal_id: &str) -> i32 {
    let g = match goal::load(&here.home, goal_id) {
        Ok(g) => g,
        Err(e) => return fail(e),
    };
    let Some(id) = a.value("--id") else {
        eprintln!("borg hive open: give --id ID");
        return 2;
    };
    if a.on("--again") && a.value("--group").is_some() {
        eprintln!("borg hive open: --again reopens one blocked task (a group's member by its id), never a group");
        return 2;
    }
    // The engineer's amend (S-def): a convention gate as `ratify` and `approve` are, since every open
    // line is written as the engineer's (the fold cannot tell who ran it). A drone asks instead
    // (`borg hive scope`); a session's model reads its drones' asks, each with this very command.
    if a.on("--again") && drone_here(here) {
        return fail(AGAIN_REFUSED);
    }
    if a.on("--again")
        && let Some(why) = in_session("borg hive open --again")
    {
        return fail(why);
    }
    let mut old: Option<Task> = None;
    let task = if a.on("--again") {
        match here.board(&g).tasks.get(id) {
            // An amend may fix what is wrong with the task: a new title, files, deps or checks replace the
            // old ones; a blocked task opens again, a held one keeps its claim (board.rs `apply`).
            Some(s) if s.task.of.is_none() && matches!(s.status, Status::Blocked { .. } | Status::Open | Status::Claimed { .. }) => {
                if a.value("--deps").is_some() && matches!(s.status, Status::Claimed { .. }) {
                    return fail(format!(
                        "{id} is claimed: deps change only on a task no one holds (its claim started from the deps it had); block or release it first"
                    ));
                }
                old = Some(s.task.clone());
                let mut t = s.task.clone();
                if let Some(title) = a.value("--title").map(str::trim).filter(|t| !t.is_empty()) {
                    t.title = title.into();
                }
                if a.value("--files").is_some() {
                    t.files = a.list("--files");
                }
                if a.value("--deps").is_some() {
                    t.deps = a.list("--deps");
                    let b = here.board(&g);
                    if let Err(e) = check_deps(std::slice::from_ref(&t), |d| d != id && b.tasks.contains_key(d)) {
                        return fail(e);
                    }
                }
                if !a.all("--accept").is_empty() {
                    match a.all("--accept").into_iter().map(|c| Check::parse(c, 600)).collect() {
                        Ok(acc) => t.accept = acc,
                        Err(e) => return fail(e),
                    }
                }
                t
            }
            Some(s) if s.task.of.is_some() => return fail(format!("{id} is a verify task: its definition is its work's")),
            Some(s) => {
                return fail(format!("{id} is {}: it keeps the definition its done was judged under; open a new task for more work", s.status.name()));
            }
            None => return fail(format!("no task {id} in goal {goal_id}")),
        }
    } else {
        let Some(role) = a.value("--role").map_or(Some(Role::Work), Role::parse) else {
            eprintln!("borg hive open: --role is plan, work, verify or integrate");
            return 2;
        };
        let accept: Result<Vec<Check>, String> = a.all("--accept").into_iter().map(|c| Check::parse(c, 600)).collect();
        let made = accept.and_then(|acc| {
            new_task(id, role, a.value("--title").unwrap_or_default(), a.value("--repo").unwrap_or_default(), a.list("--files"), a.list("--deps"), acc)
        });
        let b = here.board(&g);
        match made.and_then(|t| check_deps(std::slice::from_ref(&t), |d| b.tasks.contains_key(d)).map(|()| t)) {
            Ok(t) if b.tasks.contains_key(&t.id) => return fail(format!("task {} exists already (the engineer amends it from a terminal with --again)", t.id)),
            Ok(t) if b.tasks.values().any(|s| s.task.group.as_ref() == Some(&t.id)) => return fail(format!("group {} exists already", t.id)),
            // Only a group's best member is integrated (group.rs `merged`): work built on another member
            // would be merged onto a tree without it.
            Ok(t) if t.deps.iter().any(|d| b.tasks.get(d).is_some_and(|s| s.task.group.is_some())) => {
                let d = t.deps.iter().find(|d| b.tasks.get(*d).is_some_and(|s| s.task.group.is_some())).cloned().unwrap_or_default();
                return fail(format!("task {}: its dep {d} is an attempt of a group, and only a group's best attempt is integrated: depend on none", t.id));
            }
            Ok(t) => t,
            Err(e) => return fail(e),
        }
    };
    // Plan-time checks (scope.rs): a directory is its tree, a file the goal's check reads and a check
    // that cannot pass on any tree are refused; an amend probes only the checks it changes.
    let mut task = task;
    let notes = match scope::validate(&g, &mut task, &here.cwd, false, |c| old.as_ref().is_none_or(|o| !o.accept.contains(c))) {
        Ok(n) => n,
        Err(e) => return fail(e),
    };
    let tasks = match a.value("--group").map(str::parse::<usize>) {
        None | Some(Ok(0 | 1)) => vec![task],
        Some(Ok(k)) => match group_of(&here.board(&g), &task, k) {
            Ok(t) => t,
            Err(e) => return fail(e),
        },
        Some(Err(_)) => {
            eprintln!("borg hive open: --group is the number of attempts, 2 to {}", run::MAX_DRONES - 1);
            return 2;
        }
    };
    let note = old.as_ref().map(|o| format!("{}{}", board::AMENDED, scope::amended(o, &tasks[0])));
    let opens: Vec<Event> = tasks.iter().map(|t| Event::open(t.clone(), &here.engineer(), here.now).with_note(note.as_deref().unwrap_or_default())).collect();
    if let Err(e) = write(&here.home, goal_id, opens) {
        return fail(e);
    }
    for n in &notes {
        println!("{n}");
    }
    for task in &tasks {
        match &old {
            // A claim from another node that sorts before this line keeps the deps it started from.
            Some(o) if o.deps != task.deps => println!(
                "amended {} in goal {goal_id}: {} (the deps apply unless a claim of it from another node lands first)",
                task.id,
                scope::amended(o, task)
            ),
            Some(o) => println!("amended {} in goal {goal_id}: {}", task.id, scope::amended(o, task)),
            None => println!("opened {} ({}) {} in goal {goal_id}", task.id, task.role.name(), task.title),
        }
    }
    if let [first, ..] = tasks.as_slice()
        && let Some(gr) = &first.group
    {
        println!(
            "group {gr}: {} attempts, each another drone's (`borg debug hive run` needs one drone more, to verify them); the best is integrated",
            tasks.len()
        );
    }
    plan_said(here, &g);
    0
}

/// The `k` members of a group opened as `t` (hive/group.rs): `<id>.g1`.. copies of the work task `t`.
/// A group needs a drone per attempt and one more to verify them, so `k` is below `run::MAX_DRONES`.
fn group_of(b: &Board, t: &Task, k: usize) -> Result<Vec<Task>, String> {
    if t.role != Role::Work {
        return Err(format!("--group: only a work task is attempted by a group, not a {} task", t.role.name()));
    }
    if k >= run::MAX_DRONES {
        return Err(format!("--group {k}: a group needs a drone per attempt and one more to verify them, at most {}", run::MAX_DRONES - 1));
    }
    let members: Vec<Task> = (1..=k).map(|n| Task { id: group::member_id(&t.id, n), group: Some(t.id.clone()), ..t.clone() }).collect();
    if let Some(m) = members.iter().find(|m| !valid_task_id(&m.id)) {
        return Err(format!("invalid task id {:?}: the group's id must leave room for its members' `.gN`", m.id));
    }
    if let Some(m) = members.iter().find(|m| b.tasks.contains_key(&m.id)) {
        return Err(format!("task {} exists already", m.id));
    }
    Ok(members)
}

fn claim_cmd(here: &Here, named: Option<&str>, task: Option<&str>) -> i32 {
    let (session, hs) = match here.drone(named) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    let g = match goal::load(&here.home, &hs.goal) {
        Ok(g) => g,
        Err(e) => return fail(e),
    };
    let repo = queen::repo_name(&here.cwd);
    // A lost claim's tree is set aside before this claim takes its base from it.
    let (events, _, b) = board_for(&here.home, &g);
    let state = std::fs::read_to_string(hook::state_file(&session)).unwrap_or_default();
    if let Some(told) =
        set_aside_lost(&g, &b, &node(&here.home), events.iter().map(|(_, e)| e), &session, &state, &here.cwd, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT)
    {
        println!("{told}");
    }
    let mut lost = 0;
    while lost < if task.is_some() { 1 } else { CLAIM_TRIES } {
        let now = learn::now_ms();
        let b = here.board(&g);
        if let Some(s) = board::holding(&b, &node(&here.home), &session, now) {
            return fail(format!("{} holds {} already: finish, release or block it first", short(&session), s.task.id));
        }
        let pick = match task {
            Some(t) => match b.tasks.get(t) {
                None => return fail(format!("no task {t} in goal {}", g.id)),
                Some(s) if s.task.role == Role::Integrate => return fail(format!("{t}: {INTEGRATE_REFUSED}")),
                Some(s) if s.task.from.as_deref().is_some_and(|f| !own_tree(&g, &session, &here.cwd) && !checkout_of(&here.cwd, f)) => {
                    return fail(fix_where(t, s.task.from.as_deref().unwrap_or_default()));
                }
                Some(s) if !may_claim(&g, &s.task, &repo) => {
                    return fail(format!("{t} belongs to repo {}, and {repo} is outside the goal's repos", s.task.repo));
                }
                Some(s) if !oracle::approved(&here.home, &g, &s.task) => {
                    let approvals = oracle::Approvals::load(&here.home, &g.id);
                    return fail(format!("{t}: {}", oracle::refusal(&g.id, &approvals.pending(&g, &s.task))));
                }
                Some(s) if !board::claimable_by(&b, s, board::Actor::new(&node(&here.home), &drone_for(&hs), &session), now) => {
                    return fail(format!("{t} is not open to {} ({}). {NOT_YOURS}", short(&session), held_by(s, now)));
                }
                Some(s) => {
                    if let Some(c) = lacking_here(&here.home, &g.id, &b, Path::new(&here.cwd), now).get(t) {
                        return fail(elsewhere(t, &s.node, c));
                    }
                    s
                }
            },
            None => match next_open(&here.home, &g, &b, &session, &drone_for(&hs), &repo, &here.cwd, now) {
                Some(s) => s,
                None => {
                    println!("no task is open to {} now", short(&session));
                    return 0;
                }
            },
        };
        let id = pick.task.id.clone();
        match claim(&here.home, &g, &session, &pick.task.clone(), task.is_none(), &here.cwd, now, verdict::SNAPSHOT_TIMEOUT, None) {
            Ok(won) => {
                count(&here.home, stats::HIVE_CLAIM, &session, &g.id, &id, &repo);
                hook::append(&session, &format!("{TASK_MARKER}{id}@{}", won.live(learn::now_ms()).map_or(0, |l| l.2)));
                println!("claimed {id} for {}. {}", short(&session), describe(&here.home, &g, &won, &session, &here.cwd));
                if let Some(told) = plan::escalate(&here.home, &g, &won.task, &session, &here.cwd, learn::now_ms()) {
                    println!("{told}");
                }
                return 0;
            }
            // Never offered to this session again; without a task named, the next one is tried.
            Err(Refused::Deps(why)) => {
                refuse(&session, &id);
                if task.is_some() {
                    return fail(why);
                }
                println!("skipped {id}: {why}");
            }
            Err(Refused::Lost) => {
                lost += 1;
                println!("lost {id}: another drone claimed it first. {NOT_YOURS}");
            }
            // Blocked for the engineer: no drone takes it now, so the next one is tried.
            Err(Refused::Blocked(why)) => {
                if task.is_some() {
                    return fail(why);
                }
                println!("{why}");
            }
            Err(Refused::NotHere(why) | Refused::Later(why) | Refused::Io(why) | Refused::Reset(why)) => return fail(why),
        }
    }
    3
}

/// The live claim of the session `named`: (session, goal, task).
fn held(here: &Here, named: Option<&str>) -> Result<(String, Goal, TaskState), String> {
    let (session, hs) = here.drone(named)?;
    let g = goal::load(&here.home, &hs.goal)?;
    let b = here.board(&g);
    let s = board::holding(&b, &node(&here.home), &session, here.now).cloned().ok_or_else(|| format!("{} holds no claim in goal {}", short(&session), g.id))?;
    Ok((session, g, s))
}

fn held_event(here: &Here, named: Option<&str>, ev: Ev, note: &str) -> i32 {
    match held(here, named) {
        Ok((session, g, s)) => match write(&here.home, &g.id, vec![Event::new(ev, &s.task.id, &session, here.now).with_note(note.trim())]) {
            Ok(_) => {
                // A release or block of an interactive drone's own claim: what it cost (meter.rs).
                if let Some(hs) = bound(&session) {
                    meter_end(&here.home, &g.id, &session, &hs, &s, &harness_of(&here.home, &session), None);
                }
                if ev == Ev::Released {
                    count(&here.home, stats::HIVE_RELEASED, &session, &g.id, &s.task.id, &queen::repo_name(&here.cwd));
                }
                println!("{} {}", ev.name(), s.task.id);
                0
            }
            Err(e) => fail(e),
        },
        Err(e) => fail(e),
    }
}

fn done_cmd(here: &Here, named: Option<&str>, note: &str) -> i32 {
    let (session, g, s) = match held(here, named) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    if s.task.role == Role::Verify {
        return fail(format!("{} is a verify task: give a verdict with `borg hive verify --pass` or `--reject \"why\"`", s.task.id));
    }
    // No hook limit here: each check runs within its own timeout, and the claim is renewed meanwhile.
    let dir = Path::new(&here.cwd);
    match renewing(&here.home, &g.id, &s.task.id, &session, HEARTBEAT, || oracle::run(&here.home, &g, &s.task, dir, None)) {
        oracle::Verdict::Pass => {
            let head = verdict::snapshot(dir, verdict::SNAPSHOT_TIMEOUT).unwrap_or_default();
            if let Some((why, _)) = scope_refusal(&here.home, &g, &s, &session, dir, &head, None) {
                println!("{} is not done: {why}", s.task.id);
                return 1;
            }
            let vid = match finish(&here.home, &g, &s, &session, dir, head, &Note::Said(note), verdict::SNAPSHOT_TIMEOUT) {
                Ok(vid) => vid,
                Err(Unfinished::Io(e)) => return fail(e),
                Err(Unfinished::Lost(why)) => {
                    println!("{why}");
                    return 3;
                }
            };
            if let Some(mut hs) = bound(&session) {
                hs.oracle_blocks = 0;
                hs.blocks_for.clear();
                save(&session, &hs);
                meter_end(&here.home, &g.id, &session, &hs, &s, &harness_of(&here.home, &session), None);
            }
            println!("{} done: the oracle passed; verify task {vid} opened", s.task.id);
            0
        }
        v => {
            let log = oracle::log(&here.home, &g.id, &s.task.id, &v, learn::now_ms());
            match v {
                oracle::Verdict::Fail { check, tail } => println!("{} is not done: {check} failed\n{}", s.task.id, tail.trim_end()),
                oracle::Verdict::Timeout { check, after } => println!("{} is not done: {check} timed out after {}s", s.task.id, after.as_secs()),
                oracle::Verdict::Pass => {}
            }
            if let Some(l) = log {
                println!("(log: {})", l.display());
            }
            1
        }
    }
}

/// `borg hive verify --pass|--reject "why"`: the verdict of the session's verify claim (verdict.rs
/// `record`: the guardrails, then the reward).
fn verdict_cmd(here: &Here, named: Option<&str>, said: verdict::Said) -> i32 {
    let (session, g, s) = match held(here, named) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    if s.task.role != Role::Verify {
        return fail(format!("{} is not a verify task", s.task.id));
    }
    match verdict::record(&here.home, &g, &s.task.id, &said, &session, &here.cwd, here.now) {
        Ok(r) => {
            if let Some(hs) = bound(&session) {
                meter_end(&here.home, &g.id, &session, &hs, &s, &harness_of(&here.home, &session), None);
            }
            println!("{}", r.summary(&s.task.id));
            0
        }
        Err(e) => fail(e),
    }
}

/// The harness of interactive drone `session` (its registry line), empty for any other session.
fn harness_of(home: &Path, session: &str) -> String {
    bound(session).filter(|hs| hs.interactive).and_then(|hs| interactive::entry_of(home, &hs.drone)).map(|e| e.harness).unwrap_or_default()
}

/// Why an engineer-only command refuses inside a harness session: a session's model reads its drones'
/// messages (plan.rs `planner_prompt`), so a drone's text must never get it to give the engineer's
/// consent. None in the engineer's own terminal.
fn in_session(cmd: &str) -> Option<String> {
    (interactive::env_session().is_some() || interactive::plugin_binds()).then(|| {
        format!(
            "`{cmd}` runs in the engineer's own terminal, never inside a session: a session's model reads its drones' messages, and this is the engineer's consent"
        )
    })
}

/// Whether this command runs for a drone ([`drone`]).
fn drone_here(here: &Here) -> bool {
    drone(&here.home, &here.cwd)
}

/// Whether a process in `cwd` runs for a drone: launched into a goal (`BORG_HIVE`, `BORG_HIVE_DRONE`),
/// in a worktree `borg debug hive run` made and has not left (a harness that drops the env), or for an
/// interactive drone (its harness session bound, or in its worktree, phase g correction 6). What the
/// engineer-only commands and the queen's questions about a hive's plan and tasks are never run,
/// answered or delivered by (queen.rs).
pub fn drone(home: &Path, cwd: &str) -> bool {
    let set = |k: &str| std::env::var(k).is_ok_and(|v| !v.trim().is_empty());
    set(ENV) || set(DRONE_ENV) || joined(home, cwd).is_some_and(|j| j.by == "run") || interactive::drone_here(cwd)
}

/// `borg hive approve <goal>`: the engineer approves the checks of goal `id` that other nodes wrote (the
/// synced goal's oracle, a remote task's own), printing each one, so this node's drones may take their
/// tasks and run them (oracle.rs `Approvals`). Never a drone's command, nor one run inside a session
/// (`in_session`): approving is the engineer's consent to run another machine's shell commands.
fn approve_cmd(here: &Here, id: &str) -> i32 {
    if drone_here(here) {
        return fail("checks are approved by the engineer, never by a drone of a hive goal");
    }
    if let Some(why) = in_session("borg hive approve") {
        return fail(why);
    }
    let g = match goal::load(&here.home, id) {
        Ok(g) => g,
        Err(e) => return fail(e),
    };
    let b = here.board(&g);
    let pending = oracle::unapproved(&here.home, &g, &b);
    if pending.is_empty() {
        println!("nothing to approve in goal {id}: every check is this node's or approved");
        return 0;
    }
    for (at, c) in &pending {
        let repo = if c.repo.is_empty() { String::new() } else { format!(" (in {})", c.repo) };
        println!("approve {at} {}: {}{repo}", c.name, c.cmd);
    }
    let checks: Vec<Check> = pending.iter().map(|(_, c)| (*c).clone()).collect();
    match oracle::approve(&here.home, id, &checks, here.now) {
        Ok(n) => {
            println!("{n} check(s) approved on this node");
            0
        }
        Err(e) => fail(e),
    }
}

/// `borg debug hive score <goal>`: the hidden oracle scores every judged attempt not scored yet. An engineer's
/// command: a drone that learned the held-out checks' results could optimise against them.
fn score_cmd(here: &Here, id: &str) -> i32 {
    if drone_here(here) {
        return fail("the hidden oracle is scored by the engineer, never by a drone of a hive goal");
    }
    if let Some(why) = in_session("borg debug hive score") {
        return fail(why);
    }
    let g = match goal::load(&here.home, id) {
        Ok(g) => g,
        Err(e) => return fail(e),
    };
    if g.held_out && g.hidden.is_none() {
        return fail(format!("goal {id}'s hidden oracle is on the node that made it: score it there"));
    }
    if g.hidden.is_none() {
        return fail(format!("goal {id} has no hidden oracle: its verdicts are credited at once"));
    }
    let Some(repo) = run::repo_of(Path::new(&here.cwd)) else { return fail(format!("{} is not in the goal's git repo", here.cwd)) };
    let scored = hidden::score_pending(&here.home, &g, &repo, usize::MAX, here.now);
    let scores = &scored.scores;
    for s in scores {
        let r = if s.pass {
            "pass"
        } else if s.timeout {
            "timeout"
        } else {
            "fail"
        };
        println!("{}  {r}  {} ms{}", s.task, s.dur_ms, if s.agree { "" } else { "  (the verifier said otherwise)" });
    }
    for why in &scored.skipped {
        println!("{why} (not scored: run it again from the goal's repo)");
    }
    println!("{} attempt(s) scored", scores.len());
    i32::from(!scored.skipped.is_empty())
}

/// Where the fix task `t` (from the combined tree `from`) is claimed.
fn fix_where(t: &str, from: &str) -> String {
    format!(
        "{t} starts from the integrated tree {from}: claim it in a checkout of it (`git worktree add <dir> {from}`, then `borg hive claim {t}` there), or a drone of `borg debug hive run` takes it in its own worktree"
    )
}

/// `borg hive integrate <goal>`: what `borg debug hive run` does once every task is verified (integrate.rs),
/// from the goal's repo. Exit 0 when the combined tree passed, 1 when it failed (a fix task is open), is
/// blocked, or needs another node's commits (nothing written), 2 when no integration is due. The
/// engineer's: a drone's session never integrates.
fn integrate_cmd(here: &Here, id: &str) -> i32 {
    if drone_here(here) {
        return fail(INTEGRATE_REFUSED);
    }
    if let Some(why) = in_session("borg hive integrate") {
        return fail(why);
    }
    let g = match goal::load(&here.home, id) {
        Ok(g) => g,
        Err(e) => return fail(e),
    };
    let Some(repo) = run::repo_of(Path::new(&here.cwd)) else { return fail(format!("{} is not in the goal's git repo", here.cwd)) };
    match integrate::run(&here.home, &g, &repo, "cli", here.now) {
        Some(integrate::Outcome { task, head, verdict: Ok(()) }) => {
            println!("{task}: integrated {}; the goal is finished", head.unwrap_or_default());
            0
        }
        Some(integrate::Outcome { task, head, verdict: Err(why) }) => {
            println!("{task}: {}{why}", head.as_ref().map_or_else(String::new, |h| format!("{h}: ")));
            if let Some(h) = head {
                println!("{}", fix_where(&format!("{task}.fix"), &h));
            }
            1
        }
        // Accepted work whose commits are on another node only: nothing is written here.
        None if let Some((_, c, _)) = integrate::elsewhere(&here.home, &g, &repo, here.now) => fail(c.why().to_string()),
        None => {
            let b = here.board(&g);
            match b.tasks.values().find(|s| s.task.from.is_some() && !matches!(s.status, Status::Verified { .. })) {
                Some(s) => println!("no integration of {id} is due: {}", fix_where(&s.task.id, s.task.from.as_deref().unwrap_or_default())),
                None => println!("no integration of {id} is due: a task is not verified yet, or the goal is finished"),
            }
            2
        }
    }
}

/// `borg hive ask|tell "text" [--goal G] [--to TASK|DRONE|SESSION|planner] [--re ID]`: a message in a
/// goal (message.rs). A drone (`--session`, or a process that runs for one) sends as itself, in its own
/// goal only. Anyone else is the planner: a harness session sends as itself into `--goal` (or the one
/// goal it planned) without joining, and is that goal's planner from then on (plan.rs `planned`); the
/// engineer's terminal sends as the engineer, never as the repo's one drone. A `tell` that answers
/// (`--re`) without `--to` goes to the asker's session, or to the planner when the engineer's terminal
/// asked.
fn message_cmd(here: &Here, named: Option<&str>, goal: Option<&str>, kind: &str, text: &str, to: Option<&str>, re: Option<&str>) -> i32 {
    let as_drone = named.is_some() || drone_here(here);
    let (session, drone, id) = if as_drone {
        match here.drone(named) {
            Ok((_, hs)) if goal.is_some_and(|g| g != hs.goal) => {
                return fail(format!("this is a drone of goal {}: it sends only there (no --goal {})", hs.goal, goal.unwrap_or_default()));
            }
            Ok((s, hs)) => (s, drone_for(&hs), hs.goal),
            Err(e) => return fail(e),
        }
    } else {
        let session = interactive::env_session();
        let planned = session.as_deref().map(plan::planned).unwrap_or_default();
        let id = match (goal, planned.as_slice()) {
            (Some(g), _) => g.to_string(),
            (None, [one]) => one.clone(),
            (None, []) => {
                let goals = goal::list(&here.home);
                return fail(format!("name the goal to send into: --goal G (goals: {})", if goals.is_empty() { "none".into() } else { goals.join(", ") }));
            }
            (None, many) => return fail(format!("name the goal to send into: --goal G (this session planned {})", many.join(", "))),
        };
        (session.unwrap_or_else(|| here.engineer()), String::new(), id)
    };
    let g = match goal::load(&here.home, &id) {
        Ok(g) => g,
        Err(e) => return fail(e),
    };
    let path = messages_path(&here.home, &g.id);
    let to = match (to, re) {
        (Some("planner" | message::PLANNER), _) => message::PLANNER.to_string(),
        (Some(t), _) => t.to_string(),
        (None, Some(r)) => match message::read(&path).into_iter().find(|m| m.id == r) {
            // The engineer's terminal reads no prompt: its question is answered to the planner.
            Some(m) if m.drone.is_empty() && m.by.starts_with("engineer:") => message::PLANNER.to_string(),
            Some(m) => m.by,
            None => return fail(format!("no message {r} in goal {}", g.id)),
        },
        (None, None) => String::new(),
    };
    let task = match as_drone {
        true => board::holding(&here.board(&g), &node(&here.home), &session, here.now).map(|s| s.task.id.clone()).unwrap_or_default(),
        false => String::new(),
    };
    let m = message::Message::new(kind, &session, &drone, &node(&here.home), &task, &to, re.map(String::from), here.now, text);
    match message::send(&here.home, &g.id, m) {
        Ok(m) => {
            // A harness session that sent as the planner hears its drones from now on.
            if !as_drone && !session.starts_with("engineer:") {
                plan::add_planned(&session, &g.id);
            }
            stats::count(&here.home, stats::HIVE_MESSAGE, &session, |ev| {
                ev.why = g.id.clone();
                ev.repo = queen::repo_name(&here.cwd);
                ev.text = stats::excerpt(&format!("{kind} {}: {}", if m.to.is_empty() { "all" } else { &m.to }, m.text), 160);
            });
            let whom = match m.to.as_str() {
                "" => "every drone",
                message::PLANNER => "the planner",
                t => t,
            };
            println!("{} {} sent to {whom} in goal {}", if kind == message::ASK { "question" } else { "message" }, m.id, g.id);
            0
        }
        Err(e) => fail(e),
    }
}

fn contract_cmd(here: &Here, named: Option<&str>, text: &str, scope: Vec<String>, supersedes: Option<&str>) -> i32 {
    let (session, hs) = match here.drone(named) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    if scope.is_empty() {
        eprintln!("borg hive contract: give its --scope (repo:NAME, id:NAME or a file glob), comma-separated");
        return 2;
    }
    let g = match goal::load(&here.home, &hs.goal) {
        Ok(g) => g,
        Err(e) => return fail(e),
    };
    let b = here.board(&g);
    let task = board::holding(&b, &node(&here.home), &session, here.now).map(|s| s.task.id.clone()).unwrap_or_default();
    let c = Contract::new(&task, &session, here.now, scope, text.trim(), supersedes.map(String::from)).with_drone(&drone_for(&hs));
    match contract::publish(&here.home, &g.id, &c, &queen::repo_name(&here.cwd)) {
        Ok(contract::Publish::Written) => {
            println!("contract {} published in goal {}", c.id, g.id);
            0
        }
        Ok(contract::Publish::Proposed(q)) => {
            println!(
                "{} is another drone's contract: proposed to the engineer as queen question {q}; keep to it until they answer",
                supersedes.unwrap_or_default()
            );
            0
        }
        Err(e) => fail(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_line_parses_and_ids_stay_path_safe() {
        let t = parse_task("t1|work|Add the parser|src/parse/*.rs,tests/*.rs|t0").unwrap();
        assert_eq!((t.id.as_str(), t.role, t.files.len(), t.deps.as_slice()), ("t1", Role::Work, 2, ["t0".to_string()].as_slice()));
        assert!(parse_task("t1|chef|x").is_err());
        assert!(parse_task("../t|work|x").is_err());
        assert!(parse_task("t1|work").is_err());
    }

    #[test]
    fn init_and_open_refuse_unknown_deps_and_cycles() {
        let t = |line: &str| parse_task(line).unwrap();
        let none = |_: &str| false;
        // init: deps within the batch, in any order.
        assert_eq!(check_deps(&[t("b|work|B||a"), t("a|work|A")], none), Ok(()));
        assert_eq!(check_deps(&[t("a|work|A"), t("b|work|B||a,x")], none).unwrap_err(), "task b: its dep x names no task");
        assert_eq!(
            check_deps(&[t("a|work|A||c"), t("b|work|B||a"), t("c|work|C||b"), t("d|work|D")], none).unwrap_err(),
            "tasks a, b, c depend on each other in a cycle"
        );
        assert!(check_deps(&[t("a|work|A||a")], none).is_err(), "a task after itself");
        // open: a dep must be on the board already (so no new cycle can form).
        assert_eq!(check_deps(&[t("c|work|C||a")], |d| d == "a"), Ok(()));
        assert!(check_deps(&[t("c|work|C||z")], |d| d == "a").unwrap_err().contains("its dep z names no task"));
    }

    #[test]
    fn a_join_record_holds_the_directories_inside_it_and_a_leave_shadows_it() {
        let home = std::env::temp_dir().join(format!("borg-hive-joins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        assert!(joined(&home, "/w/repo/src").is_none(), "no hive: nothing (one failed stat)");
        let g = Goal {
            v: 1,
            id: "g".into(),
            spec: "s".into(),
            oracle: vec![],
            budget_usd: 1.0,
            repos: vec![],
            author: String::new(),
            created_ms: 0,
            sig: String::new(),
            hidden: None,
            held_out: false,
            origin: Vec::new(),
        };
        goal::create(&home, &g).unwrap();
        let j =
            |ms: i64, goal: &str, dir: &str| Join { v: 1, ms, goal: goal.into(), dir: dir.into(), drone: "j-1".into(), by: "join".into(), ..Join::default() };
        append_join(&home, "g", &j(1, "g", "/w/repo")).unwrap();
        assert_eq!(joined(&home, "/w/repo/src/x").map(|j| (j.goal, j.drone)), Some(("g".into(), "j-1".into())));
        assert_eq!(joined(&home, "/w/repo").map(|j| j.goal).as_deref(), Some("g"));
        assert!(joined(&home, "/w/repo2").is_none(), "whole path components only");
        assert!(joined(&home, "/w").is_none() && joined(&home, "").is_none());
        append_join(&home, "g", &j(2, "", "/w/repo")).unwrap();
        assert!(joined(&home, "/w/repo/src").is_none(), "the later leave shadows the join");
        append_join(&home, "g", &j(3, "g", "/w/repo/.worktrees/hive-g-1")).unwrap();
        assert!(joined(&home, "/w/repo/.worktrees/hive-g-1/src").is_some() && joined(&home, "/w/repo/src").is_none());
        let _ = std::fs::remove_dir_all(home);
    }

    fn git_ok(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.email=e@x", "-c", "user.name=e"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn goal_g() -> Goal {
        Goal {
            v: 1,
            id: "g".into(),
            spec: "s".into(),
            oracle: vec![],
            budget_usd: 1.0,
            repos: vec![],
            author: String::new(),
            created_ms: 0,
            sig: String::new(),
            hidden: None,
            held_out: false,
            origin: Vec::new(),
        }
    }

    #[test]
    fn a_fix_tasks_reset_runs_no_hook_of_the_repo() {
        let root = std::env::temp_dir().join(format!("borg-hive-reset-hook-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        git_ok(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README"), "base\n").unwrap();
        git_ok(&repo, &["add", "-A"]);
        git_ok(&repo, &["commit", "-qm", "base"]);
        let from = git_ok(&repo, &["rev-parse", "HEAD"]);
        // A post-checkout hook that fails (git-lfs's, on a machine without git-lfs) fails the checkout.
        let hook = repo.join(".git/hooks/post-checkout");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch {}\nexit 1\n", root.join("ran").display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(repo.join("stray.txt"), "x\n").unwrap();
        assert_eq!(reset_to(&repo, &from, Duration::from_secs(30)), Ok(()));
        assert!(!root.join("ran").exists(), "the hook never ran");
        assert!(!repo.join("stray.txt").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A store with goal g, tasks t1 and t2 open, and peer `kb` heard from a second ago.
    fn two_node_store(tag: &str) -> (PathBuf, Goal, i64) {
        let home = std::env::temp_dir().join(format!("borg-hive-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let g = goal_g();
        goal::create(&home, &g).unwrap();
        let now = learn::now_ms();
        let open = |id: &str| Event::open(new_task(id, Role::Work, "work", "", vec![], vec![], vec![]).unwrap(), "engineer:e", now - 10);
        board::append(&tasks_path(&home, "g"), &[open("t1"), open("t2")]).unwrap();
        presence::note_synced(&home, "kb", now - 1000).unwrap();
        (home, g, now)
    }

    /// The peer's next session, `after` from now: what it brings to the board, then its record.
    fn peer_round(home: &Path, after: Duration, brings: Vec<Event>) -> std::thread::JoinHandle<()> {
        let home = home.to_path_buf();
        std::thread::spawn(move || {
            std::thread::sleep(after);
            if !brings.is_empty() {
                board::append(&tasks_path(&home, "g"), &brings).unwrap();
            }
            presence::note_synced(&home, "kb", learn::now_ms()).unwrap();
        })
    }

    fn on_b(e: Event) -> Event {
        Event { node: "B".into(), ..e }
    }

    #[test]
    fn a_rework_puts_back_the_attempt_of_its_own_nodes_drone_never_another_nodes() {
        // Node A's d1 did t1 first, then node B's d1. A rework by B's d1 gets B's attempt back on its
        // start: A's would be another drone's work in its diff.
        let at = |ev: Ev, by: &str, node: &str, ms: i64| Event { node: node.into(), drone: "d1".into(), ..Event::new(ev, "t1", by, ms) };
        let t1 = new_task("t1", Role::Work, "work", "", vec![], vec![], vec![]).unwrap();
        let v = verify_task(&t1, 0, "sa");
        let lines = vec![
            Event::open(t1, "engineer:e", 1),
            Event { base: "base-a".into(), ..at(Ev::Claim, "sa", "A", 10) },
            Event { head: "head-a".into(), ..at(Ev::Done, "sa", "A", 11) },
            Event { node: "A".into(), ..Event::open(v, "sa", 11) },
            Event::new(Ev::Claim, "t1.v1", "verifier", 12),
            Event::new(Ev::Rejected, "t1.v1", "verifier", 13).with_note("not yet"),
            Event { base: "base-b".into(), ..at(Ev::Claim, "sb", "B", 20) },
            Event { head: "head-b".into(), ..at(Ev::Done, "sb", "B", 21) },
        ];
        let b = board::fold(&lines);
        let head = |session: &str, drone: &str, node: &str| previous(&lines, &b, "t1", session, drone, node).map(|a| a.head);
        assert_eq!(head("sb2", "d1", "B").as_deref(), Some("head-b"));
        assert_eq!(head("sa2", "d1", "A").as_deref(), Some("head-a"));
        assert_eq!(head("sc", "d1", "C"), None, "no attempt of C's d1");
        assert_eq!(head("sb", "", "B").as_deref(), Some("head-b"), "a session of its own: by its id");
    }

    #[test]
    fn a_claim_beside_another_nodes_drones_waits_for_a_round_with_each_peer() {
        // Phase e E4. Node B is live (its runner announced). This node's claim of t1 won its read-back;
        // B's earlier claim reaches it only with the round after: the window loses the claim before any
        // work starts. The window is driven directly, so B's line lands after the read-back however
        // slow the machine.
        let (home, g, now) = two_node_store("confirm");
        board::append(&tasks_path(&home, "g"), &[on_b(Event::new(Ev::Heartbeat, "", "runner", now - 5))]).unwrap();
        write(&home, "g", vec![Event::new(Ev::Claim, "t1", "sa", now)]).unwrap();
        let written = learn::now_ms();
        let round = peer_round(&home, Duration::from_millis(20), vec![on_b(Event::new(Ev::Claim, "t1", "sb", now - 1))]);
        let (again, waited) = confirm(&home, "g", "t1", "sa", now, written, &["kb".to_string()]);
        round.join().unwrap();
        assert_eq!(again, None, "B's earlier claim came with the round");
        assert!(waited < CONFIRM_CAP, "{waited:?}");
        // No rival, through `claim`: won once the round completed, within the cap.
        let t2 = board::fold(&board::read(&tasks_path(&home, "g"))).tasks["t2"].task.clone();
        let round = peer_round(&home, Duration::from_millis(300), Vec::new());
        assert!(claim(&home, &g, "sa", &t2, false, "", learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).is_ok());
        round.join().unwrap();
        let lines: Vec<Confirm> = read_lines(&confirms_path(&home, "g"));
        assert_eq!(lines.iter().map(|c| (c.task.as_str(), c.outcome.as_str(), c.peers)).collect::<Vec<_>>(), [("t1", "lost", 1), ("t2", "won", 1)]);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_peer_record_from_a_clock_now_behind_it_proves_no_round() {
        // The clock stepped back an hour after the last session: its record lies ahead of every claim
        // written since, yet no round carried them.
        let (home, _, now) = two_node_store("confirm-skew");
        presence::note_synced(&home, "kb", now + 3_600_000).unwrap();
        let cap = Duration::from_millis(50);
        assert!(!heard_back(&home, &["kb".to_string()], learn::now_ms(), cap).0, "a record ahead of the clock waits out the cap");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_claim_is_stamped_after_its_pre_write_read_of_the_board() {
        // A long board: folding it before the write takes a while. The claim's ms is taken after that
        // fold, so it is no older than the write itself (the file's mtime) by more than the append takes.
        let (home, g, _) = two_node_store("stamp-late");
        let filler: Vec<Event> = (0..20_000).map(|i| Event::new(Ev::Heartbeat, &format!("x{i}"), "sx", 1)).collect();
        board::append(&tasks_path(&home, "g"), &filler).unwrap();
        let t1 = board::fold(&board::read(&tasks_path(&home, "g"))).tasks["t1"].task.clone();
        assert!(claim(&home, &g, "sa", &t1, false, "", learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).is_ok());
        let ms = board::read(&tasks_path(&home, "g")).iter().find(|e| e.ev == Ev::Claim && e.by == "sa").unwrap().ms;
        let landed = std::fs::metadata(tasks_path(&home, "g")).unwrap().modified().unwrap();
        let landed = landed.duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
        assert!(landed - ms <= 50, "the claim's ms {ms} is {} ms older than its write", landed - ms);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_claim_waits_no_longer_than_the_cap_and_not_at_all_on_one_node() {
        // A reachable peer whose round never completes: the claim keeps its win after the cap.
        let (home, g, now) = two_node_store("confirm-cap");
        let b = board::fold(&board::read(&tasks_path(&home, "g")));
        let (t1, t2) = (b.tasks["t1"].task.clone(), b.tasks["t2"].task.clone());
        // No other node live: no wait, no confirm line, the peer reachable or not.
        let t0 = Instant::now();
        assert!(claim(&home, &g, "sa", &t1, false, "", now, verdict::SNAPSHOT_TIMEOUT, None).is_ok());
        assert!(t0.elapsed() < CONFIRM_CAP / 2, "{:?}", t0.elapsed());
        assert!(read_lines::<Confirm>(&confirms_path(&home, "g")).is_empty());
        board::append(&tasks_path(&home, "g"), &[on_b(Event::new(Ev::Heartbeat, "", "runner", learn::now_ms()))]).unwrap();
        let t0 = Instant::now();
        assert!(claim(&home, &g, "sa2", &t2, false, "", learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).is_ok());
        assert!(t0.elapsed() >= CONFIRM_CAP && t0.elapsed() < CONFIRM_CAP * 2, "{:?}", t0.elapsed());
        let lines: Vec<Confirm> = read_lines(&confirms_path(&home, "g"));
        assert_eq!(lines.iter().map(|c| (c.task.as_str(), c.outcome.as_str())).collect::<Vec<_>>(), [("t2", "cap")]);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_picked_task_another_nodes_idle_drone_now_takes_is_not_written() {
        // A run that started a moment before its peer's picked from a board without the peer's runner
        // line; that line arrived before the claim's write and makes the task the peer's, whose idle d1
        // takes it: the pick is refused (the next one is tried), never written to lose.
        let (home, g, now) = two_node_store("prewrite-owner");
        let here = node(&home);
        let live = std::collections::BTreeSet::from([here.as_str(), "B"]);
        let id = (0..).map(|i| format!("b{i}")).find(|t| board::owner(&live, t) == Some("B")).unwrap();
        let t = new_task(&id, Role::Work, "work", "", vec![], vec![], vec![]).unwrap();
        board::append(&tasks_path(&home, "g"), &[Event::open(t.clone(), "engineer:e", now - 10)]).unwrap();
        board::append(&tasks_path(&home, "g"), &[on_b(Event::new(Ev::Heartbeat, "", "runner", now).with_note(&board::roster_note(&["d1".to_string()])))])
            .unwrap();
        assert_eq!(claim(&home, &g, "sa", &t, true, "", now + 1, verdict::SNAPSHOT_TIMEOUT, None), Err(Refused::Lost));
        let b = board::fold(&board::read(&tasks_path(&home, "g")));
        assert!(b.tasks[&id].claimable(now + 1) && b.lost.is_empty());
        let lines: Vec<Confirm> = read_lines(&confirms_path(&home, "g"));
        assert_eq!(lines.iter().map(|c| (c.task.as_str(), c.outcome.as_str())).collect::<Vec<_>>(), [(id.as_str(), REFUSED)]);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn sessions_of_one_node_racing_a_claim_never_both_hold_it() {
        // Phase f F2. Eight sessions of this node, released at once by a barrier, claim one open task
        // through `claim`, per trial a new task: one wins, every other is refused unwritten, with no
        // sleep in the claim. Fails with the check made outside the lock (`write_if`).
        let (home, g, now) = two_node_store("same-node-race");
        let tasks: Vec<Task> = (0..30).map(|i| new_task(&format!("r{i}"), Role::Work, "work", "", vec![], vec![], vec![]).unwrap()).collect();
        board::append(&tasks_path(&home, "g"), &tasks.iter().map(|t| Event::open(t.clone(), "engineer:e", now - 5)).collect::<Vec<_>>()).unwrap();
        for t in &tasks {
            let barrier = std::sync::Barrier::new(8);
            let won: Vec<bool> = std::thread::scope(|sc| {
                let racers: Vec<_> = (0..8)
                    .map(|i| {
                        let (home, g, t, barrier) = (&home, &g, t, &barrier);
                        sc.spawn(move || {
                            barrier.wait();
                            claim(home, g, &format!("{}-s{i}", t.id), t, false, "", learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).is_ok()
                        })
                    })
                    .collect();
                racers.into_iter().map(|r| r.join().unwrap()).collect()
            });
            let claims = board::read(&tasks_path(&home, "g")).into_iter().filter(|e| e.ev == Ev::Claim && e.task == t.id).count();
            assert_eq!((won.iter().filter(|w| **w).count(), claims), (1, 1), "{}: {won:?}", t.id);
        }
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_claim_of_a_task_a_rival_took_since_the_pick_is_not_written() {
        // The pick read t1 open; another drone's claim landed before this one was written: it would only
        // lose, so nothing is written and no lost claim shows on the board; the refusal is counted on
        // this node (`confirms.jsonl`), so the empty race is visible.
        let (home, g, now) = two_node_store("prewrite");
        let t1 = board::fold(&board::read(&tasks_path(&home, "g"))).tasks["t1"].task.clone();
        write(&home, "g", vec![Event::new(Ev::Claim, "t1", "s-rival", now)]).unwrap();
        assert_eq!(claim(&home, &g, "sa", &t1, false, "", now + 1, verdict::SNAPSHOT_TIMEOUT, None), Err(Refused::Lost));
        let b = board::fold(&board::read(&tasks_path(&home, "g")));
        assert!(b.lost.is_empty(), "{:?}", b.lost);
        let lines: Vec<Confirm> = read_lines(&confirms_path(&home, "g"));
        assert_eq!(
            lines.iter().map(|c| (c.task.as_str(), c.session.as_str(), c.outcome.as_str(), c.waited_ms)).collect::<Vec<_>>(),
            [("t1", "sa", REFUSED, 0)]
        );
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_line_is_stamped_under_the_lock_above_every_line_its_node_holds() {
        // Phase f F3. A heartbeat stamped from a clock read before a rival's claim landed (a stall
        // between the reading and the append) lands above that claim: it sorts after it on every node,
        // and the holder's lease it would have renewed retroactively stays lapsed. A peer's line stamped
        // ahead of this clock lifts this node's stamps above it, and the receive index times both lines
        // on this node's clock.
        let (home, _, now) = two_node_store("hlc");
        let path = tasks_path(&home, "g");
        board::append(&path, &[Event { node: "B".into(), ..Event::new(Ev::Heartbeat, "", "runner", now + 60_000) }]).unwrap();
        board::note_held(&path);
        let rival = write(&home, "g", vec![Event::new(Ev::Claim, "t1", "s-rival", now)]).unwrap();
        assert_eq!(rival[0].ms, now + 60_001, "above the peer's line it holds");
        let beat = write(&home, "g", vec![Event::new(Ev::Heartbeat, "t1", "s-old", now - 5)]).unwrap();
        assert_eq!(beat[0].ms, now + 60_002, "above this node's own last line");
        let got = board::read(&path);
        assert!(got.iter().all(|e| e.seen.0.is_some_and(|ms| ms >= now && ms < now + 60_000)), "held on this node's clock: {got:?}");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_runs_start_makes_its_node_live_and_waits_for_the_peers_round() {
        // Phase e E2b. With a peer reachable: the announcement, then the wait for its round.
        let (home, _, _) = two_node_store("announce");
        let after = Duration::from_millis(150);
        let round = peer_round(&home, after, Vec::new());
        let (peers, heard, waited) = announce(&home, "g", &["d1".to_string()]).unwrap();
        round.join().unwrap();
        assert!(peers == 1 && heard && waited >= after && waited < START_CAP, "{peers} {heard} {waited:?}");
        // Another node reads this one live before any of its drones wrote a line; the fold takes the
        // line for nothing else.
        let b = board::fold(&board::read(&tasks_path(&home, "g")));
        let here = node(&home);
        assert_eq!(b.participants("B", learn::now_ms()), std::collections::BTreeSet::from(["B", here.as_str()]));
        assert!(b.tasks.values().all(|s| s.status == Status::Open) && b.lost.is_empty() && b.spans.is_empty());
        // One node, no peer ever synced: no wait at all.
        let solo = std::env::temp_dir().join(format!("borg-hive-announce-solo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&solo);
        assert_eq!(announce(&solo, "g", &[]).unwrap(), (0, true, Duration::ZERO));
        let _ = std::fs::remove_dir_all(solo);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_claim_beside_a_peer_that_marks_waits_until_it_is_final_with_no_cap() {
        // Phase f F5. B joined and writes watermarks. This node's claim of t1 waits, past the old 1 s cap,
        // until B's beat marks it (1.5 s later): final. Its claim of t2 meets B's rival claim stamped
        // before it, which B's file held before its beat: lost before any work.
        let (home, _, now) = two_node_store("final");
        let path = tasks_path(&home, "g");
        let land_b = |events: Vec<Event>| {
            let landed = board::landing(&board::read(&path), events.into_iter().map(on_b).collect());
            board::append(&path, &landed).unwrap();
            board::note_held(&path);
        };
        land_b(vec![Event::new(Ev::Heartbeat, "", "runner", now - 5).with_note(board::JOIN)]);
        let beat_after = |wait: Duration, rival: Option<Event>| {
            let home = home.clone();
            std::thread::spawn(move || {
                std::thread::sleep(wait);
                let path = tasks_path(&home, "g");
                // B wrote its rival before this node's claim reached it: stamped on what it held then.
                if let Some(r) = rival {
                    let held: Vec<Event> = board::read(&path).into_iter().filter(|e| !(e.ev == Ev::Claim && e.task == r.task)).collect();
                    board::append(&path, &board::landing(&held, vec![on_b(r)])).unwrap();
                }
                let beat = Event::new(Ev::Heartbeat, "", "runner", learn::now_ms()).with_note(board::BEAT);
                board::append(&path, &board::landing(&board::read(&path), vec![on_b(beat)])).unwrap();
                board::note_held(&path);
            })
        };
        let claim = |task: &str, by: &str| {
            let landed = write(&home, "g", vec![Event::new(Ev::Claim, task, by, learn::now_ms())]).unwrap();
            (landed[0].ms, learn::now_ms())
        };
        let (at, written) = claim("t1", "sa");
        let beat = beat_after(Duration::from_millis(1500), None);
        let (again, waited) = across(&home, "g", "t1", "sa", at, written, &board::fold(&board::read(&path)), None).expect("B takes part");
        beat.join().unwrap();
        assert!(again.is_some() && waited >= Duration::from_millis(1500), "final on B's mark, past the old cap: {waited:?}");
        // B's rival is stamped above B's last line, its beat: with t2's claim in that beat's millisecond the
        // two would tie on one stamp. Past it by more than the rival's 50 ms, the rival sorts first.
        let beat_ms = board::read(&path).iter().filter(|e| e.node == "B").map(|e| e.ms).max().unwrap();
        while learn::now_ms() <= beat_ms + 60 {
            std::thread::sleep(Duration::from_millis(5));
        }
        let (at, written) = claim("t2", "sa2");
        let beat = beat_after(Duration::from_millis(100), Some(Event::new(Ev::Claim, "t2", "sb", at - 50)));
        let (again, _) = across(&home, "g", "t2", "sa2", at, written, &board::fold(&board::read(&path)), None).expect("B takes part");
        beat.join().unwrap();
        assert!(again.is_none(), "B's earlier claim, before its mark: lost");
        let lines: Vec<Confirm> = read_lines(&confirms_path(&home, "g"));
        assert_eq!(lines.iter().map(|c| (c.task.as_str(), c.outcome.as_str())).collect::<Vec<_>>(), [("t1", "final"), ("t2", "lost")]);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn an_idle_hour_of_beats_adds_nothing_to_the_synced_board() {
        // Phase g (live probe gap 4): a node beats every `BEAT_MS` while it is in a goal, an hour idle
        // is 1,200 beats. They go to the goal's beats store, each node's newest only: the synced logs
        // do not grow at all, the store stays one record. Before, each beat was a board line, synced and
        // given an origin record on every node: 1,200 lines an hour. Fails with `member` writing beats
        // to the board (`write`).
        let (home, _, _) = two_node_store("idle-hour");
        let path = tasks_path(&home, "g");
        let synced = || SYNCED.iter().map(|f| std::fs::metadata(dir(&home, "g").join(f)).map_or(0, |m| m.len())).sum::<u64>();
        member(&home, "g", board::JOIN).unwrap();
        member(&home, "g", board::BEAT).unwrap();
        let (board_after_one, store_after_one) = (synced(), std::fs::metadata(board::beats_path(&path)).unwrap().len());
        for _ in 0..1200 {
            member(&home, "g", board::BEAT).unwrap();
        }
        assert_eq!(synced(), board_after_one, "the synced board did not grow");
        let store = std::fs::metadata(board::beats_path(&path)).unwrap().len();
        assert!(store <= store_after_one + 16, "one record, its numbers a little longer: {store_after_one} -> {store}");
        assert_eq!(board::beat_records(&board::beats_path(&path)).len(), 1);
        // What a beat says still reaches the fold: the node's newest beat, above its join.
        let b = board::fold(&board::read(&path));
        let beat = board::read(&path).into_iter().filter(board::is_beat).max_by_key(|e| e.ms).unwrap();
        assert!(b.members.get(&node(&home)).is_some_and(|(join, _)| *join < beat.ms));
        assert_eq!(b.nodes.get(&node(&home)), Some(&beat.seen.0.unwrap()), "its liveness: when this node held its newest beat");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_peers_beat_from_the_beats_store_makes_a_claim_final() {
        // Phase g with phase f F5: B joined and writes watermarks; its beat comes through the goal's beats
        // store (the mesh's live exchange, board.rs `merge_beats`), not the board. This node's claim of t1
        // waits until B's beat marks it, then is final. Fails with `board::read` reading no beats store
        // (the claim waits until B is suspected).
        let (home, _, now) = two_node_store("store-final");
        let path = tasks_path(&home, "g");
        let landed = board::landing(&board::read(&path), vec![on_b(Event::new(Ev::Heartbeat, "", "runner", now - 5).with_note(board::JOIN))]);
        board::append(&path, &landed).unwrap();
        board::note_held(&path);
        let (at, written) = {
            let landed = write(&home, "g", vec![Event::new(Ev::Claim, "t1", "sa", learn::now_ms())]).unwrap();
            (landed[0].ms, learn::now_ms())
        };
        let beat = {
            let home = home.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(600));
                let path = tasks_path(&home, "g");
                let beat = board::landing(&board::read(&path), vec![on_b(Event::new(Ev::Heartbeat, "", "runner", learn::now_ms()).with_note(board::BEAT))]);
                let line = serde_json::to_string(&beat[0]).unwrap();
                let r = board::BeatRecord { line, seen: learn::now_ms(), key: "kb".into(), sig: "s".into() };
                assert_eq!(board::merge_beats(&path, vec![r], |_| true).unwrap(), 1);
            })
        };
        let (again, waited) = across(&home, "g", "t1", "sa", at, written, &board::fold(&board::read(&path)), None).expect("B takes part");
        beat.join().unwrap();
        assert!(again.is_some() && waited >= Duration::from_millis(600) && waited < Duration::from_millis(SUSPECT_MS as u64), "final on B's mark: {waited:?}");
        assert!(!std::fs::read_to_string(&path).unwrap().contains("\"beat\""), "no beat on the board");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn an_interactive_stops_capped_confirm_hands_nothing_that_is_not_final() {
        // Phase g correction 9. B joined and writes watermarks but has not marked this node's claim: the
        // stop's wait ends at its cap with the claim still held and not final (`cap`), so the stop hands
        // nothing (`is_final`); once B's beat marks it, it is final, and the waiter hands it.
        let (home, _, now) = two_node_store("capped");
        let path = tasks_path(&home, "g");
        let on = board::landing(&board::read(&path), vec![on_b(Event::new(Ev::Heartbeat, "", "runner", now - 5).with_note(board::JOIN))]);
        board::append(&path, &on).unwrap();
        board::note_held(&path);
        let landed = write(&home, "g", vec![Event::new(Ev::Claim, "t1", "sa", learn::now_ms())]).unwrap();
        let (at, written) = (landed[0].ms, learn::now_ms());
        let b = board::fold(&board::read(&path));
        let t0 = Instant::now();
        let (held, _) = across(&home, "g", "t1", "sa", at, written, &b, Some(Duration::from_millis(200))).expect("B takes part");
        assert!(t0.elapsed() < Duration::from_secs(2), "the cap ends the wait");
        let held = held.expect("still held at the cap");
        assert!(!is_final(&home, "g", &held, learn::now_ms()), "not final: nothing is handed");
        let beat = Event::new(Ev::Heartbeat, "", "runner", learn::now_ms()).with_note(board::BEAT);
        board::append(&path, &board::landing(&board::read(&path), vec![on_b(beat)])).unwrap();
        board::note_held(&path);
        assert!(is_final(&home, "g", &held, learn::now_ms()), "B's mark makes it final");
        let lines: Vec<Confirm> = read_lines(&confirms_path(&home, "g"));
        assert_eq!(lines.iter().map(|c| c.outcome.as_str()).collect::<Vec<_>>(), ["cap"]);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn an_interactive_stop_on_a_claim_it_was_never_shown_runs_no_oracle_and_writes_no_done() {
        // Phase g G2 (e), (b). The stop's capped claim of t1 is held and not final (B has not marked it)
        // and was never shown (no `TASK_MARKER`): a later stop, the drone's after it armed the wait or the
        // end of the engineer's own turn, has nothing in hand. Fails with the claim read as in hand: the
        // goal's empty oracle passes on the base and the stop publishes a done of no work.
        let (home, g, now) = two_node_store("unshown-stop");
        let path = tasks_path(&home, "g");
        let on = board::landing(&board::read(&path), vec![on_b(Event::new(Ev::Heartbeat, "", "runner", now - 5).with_note(board::JOIN))]);
        board::append(&path, &on).unwrap();
        board::note_held(&path);
        let session = format!("unshown-stop-{}", std::process::id());
        write(&home, "g", vec![Event::new(Ev::Claim, "t1", &session, learn::now_ms())]).unwrap();
        let mut hs = HiveSession { goal: "g".into(), interactive: true, drone: "i-test".into(), ..HiveSession::default() };
        let here = node(&home);
        let lines: Vec<(u64, Event)> = board::read(&path).into_iter().enumerate().map(|(i, e)| (i as u64, e)).collect();
        let b = board::fold_on(&lines.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>(), &here);
        let held = board::holding(&b, &here, &session, learn::now_ms()).expect("held").clone();
        assert!(!is_final(&home, "g", &held, learn::now_ms()), "not final");
        let dir = home.display().to_string();
        let ctx =
            Ctx { h: &crate::harness::sim::Sim, home: &home, g: &g, session: &session, cwd: &dir, repo: "app", now: learn::now_ms(), started: Instant::now() };
        let (out, _) = ctx.stop(&mut hs, &b, &lines, &json!({"stop_hook_active": false}), Instant::now(), || None);
        assert_eq!(out, None, "no block, nothing handed");
        assert!(!board::read(&path).iter().any(|e| e.ev == Ev::Done), "no done");
        assert!(board::holding(&board::fold_on(&board::read(&path), &here), &here, &session, learn::now_ms()).is_some(), "still held for the waiter");
        let _ = std::fs::remove_file(hook::state_file(&session));
        unbind(&session);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_mesh_member_that_is_not_a_goal_participant_delays_no_confirm() {
        // Phase f F4 (G2). Mesh members kb and kc are both reachable; only kb's node, B, joined the goal
        // (its join line's origin record names kb). A claim's confirm window waits for a round with kb
        // alone: kc never syncs again, and a wait on it would run to the cap.
        let (home, _, now) = two_node_store("g2");
        let path = tasks_path(&home, "g");
        let join = |note: &str, ms: i64| on_b(Event::new(Ev::Heartbeat, "", "runner", ms).with_note(note));
        board::append(&path, &[join(board::JOIN, now - 5)]).unwrap();
        board::note_held(&path);
        presence::note_synced(&home, "kc", now - 1000).unwrap();
        let line = std::fs::read_to_string(&path).unwrap().lines().find(|l| l.contains("\"join\"")).unwrap().to_string();
        let origin = json!({"h": blake3::hash(line.as_bytes()).to_hex().to_string(), "key": "kb", "ms": now, "sig": ""});
        std::fs::create_dir_all(home.join("mesh")).unwrap();
        std::fs::write(home.join("mesh/origins.jsonl"), format!("{origin}\n")).unwrap();
        let both = vec!["kb".to_string(), "kc".to_string()];
        let b = board::fold(&board::read(&path));
        let (here, at) = (node(&home), learn::now_ms());
        assert_eq!(participant_peers(&home, "g", &b.participants(&here, at), &here, both.clone()), ["kb"]);
        write(&home, "g", vec![Event::new(Ev::Claim, "t1", "sa", now)]).unwrap();
        let written = learn::now_ms();
        let round = peer_round(&home, Duration::from_millis(50), Vec::new());
        let b = board::fold(&board::read(&path));
        let (again, waited) = across(&home, "g", "t1", "sa", now, written, &b, None).expect("B takes part: a confirm window");
        round.join().unwrap();
        assert!(again.is_some() && waited < CONFIRM_CAP, "won on kb's round alone: {waited:?}");
        // A participant whose key is not known here (no origin record): every reachable member, as before.
        std::fs::remove_file(home.join("mesh/origins.jsonl")).unwrap();
        assert_eq!(participant_peers(&home, "g", &b.participants(&here, at), &here, both), ["kb", "kc"]);
        // B left the goal: no participant but this node, no wait at all.
        board::append(&path, &[join(board::LEAVE, now - 4)]).unwrap();
        board::note_held(&path);
        let b = board::fold(&board::read(&path));
        assert_eq!(across(&home, "g", "t1", "sa", now, written, &b, None), None);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_long_run_renews_its_claim_and_a_lapsed_one_stays_lapsed() {
        let home = std::env::temp_dir().join(format!("borg-hive-renewing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let path = tasks_path(&home, "g");
        let t = new_task("t1", Role::Work, "work", "", vec![], vec![], vec![]).unwrap();
        let now = learn::now_ms();
        board::append(&path, &[Event::open(t, "engineer:e", now), Event::new(Ev::Claim, "t1", "s1", now)]).unwrap();
        // The work lasts until three beats landed (a slow runner beats late, never not at all).
        let beats = |p: &Path| board::read(p).iter().filter(|e| e.ev == Ev::Heartbeat).count();
        let out = renewing(&home, "g", "t1", "s1", Duration::from_millis(20), || {
            let until = Instant::now() + Duration::from_secs(10);
            while beats(&path) < 3 && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            7
        });
        assert_eq!(out, 7);
        let n = beats(&path);
        assert!(n >= 3, "{n} beats");
        let renewed = match board::fold(&board::read(&path)).tasks["t1"].status {
            Status::Claimed { renewed, .. } => renewed,
            ref s => panic!("{s:?}"),
        };
        assert!(renewed > now, "the lease moved on");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_model_session_is_never_offered_an_integrate_task() {
        let g = goal_g();
        let it = Task { role: Role::Integrate, ..new_task("integrate.1", Role::Work, "integrate", "", vec![], vec![], vec![]).unwrap() };
        assert!(!may_claim(&g, &it, "app"), "whatever the repo");
        let b = board::fold(&[Event::open(it, "integrate-d1-1", 1)]);
        assert_eq!(offer(Path::new("/nonexistent-borg-home"), &g, &b, "s", "app", "/w", 2), "No claim, and no task is open to you now (`borg hive board`).");
        let w = new_task("t1", Role::Work, "work", "", vec![], vec![], vec![]).unwrap();
        assert!(may_claim(&g, &w, "app"));
    }

    #[test]
    fn a_verify_claim_of_another_nodes_attempt_names_its_commits() {
        // d3-accept2: verifiers told only of joins.jsonl (this node's) found "no change" in their own
        // tree. The claim names the attempt's base and head, which are in this repo.
        let home = std::env::temp_dir().join(format!("borg-hive-verify-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let g = goal_g();
        goal::create(&home, &g).unwrap();
        let (base, head) = ("1111111111111111111111111111111111111111", "2222222222222222222222222222222222222222");
        let w = new_task("t1", Role::Work, "work", "", vec![], vec![], vec![]).unwrap();
        let on_b = |e: Event| Event { node: "B".into(), ..e };
        let lines = [
            on_b(Event::open(w.clone(), "sr", 0)),
            on_b(Event { base: base.into(), ..Event::new(Ev::Claim, "t1", "sr", 1) }),
            on_b(Event { head: head.into(), ..Event::new(Ev::Done, "t1", "sr", 2) }),
            on_b(Event::open(verify_task(&w, 0, "sr"), "sr", 2)),
            Event::new(Ev::Claim, "t1.v1", "sv", 3),
        ];
        board::append(&tasks_path(&home, "g"), &lines).unwrap();
        let b = board::fold(&board::read(&tasks_path(&home, "g")));
        let said = describe(&home, &g, &b.tasks["t1.v1"], "sv", "/w");
        assert!(said.contains(&format!("`git diff {base} {head}`")) && said.contains(&format!("git archive {head} |")), "{said}");
        assert!(!said.contains("joins.jsonl"), "{said}");
        // Work without snapshots names none: a pass on it rejects as unchecked.
        std::fs::remove_file(tasks_path(&home, "g")).unwrap();
        let bare = lines.map(|e| Event { base: String::new(), head: String::new(), ..e });
        board::append(&tasks_path(&home, "g"), &bare).unwrap();
        let b = board::fold(&board::read(&tasks_path(&home, "g")));
        assert!(!describe(&home, &g, &b.tasks["t1.v1"], "sv", "/w").contains("git diff"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_verifiers_copy_of_the_work_can_be_made_again_and_registers_nothing() {
        // A verify claim released without a verdict (MAX_ORACLE_BLOCKS stops, a lost lease) and taken
        // again names the same copy: making it twice works, holds the head's tree, writes nothing in
        // the repo's .git (a sandboxed drone may not), and one claim's copy is not another session's.
        let root = std::env::temp_dir().join(format!("borg-hive-review-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        git_ok(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README"), "base\n").unwrap();
        git_ok(&repo, &["add", "-A"]);
        git_ok(&repo, &["commit", "-qm", "base"]);
        let base = git_ok(&repo, &["rev-parse", "HEAD"]);
        std::fs::write(repo.join("parser.rs"), "fn parse() {}\n").unwrap();
        git_ok(&repo, &["add", "-A"]);
        git_ok(&repo, &["commit", "-qm", "work"]);
        let head = git_ok(&repo, &["rev-parse", "HEAD"]);
        git_ok(&repo, &["reset", "-q", "--hard", &base]);
        let told = review_of("t1.v1", &base, &head, "sv");
        let make = told.split("apart from your tree, `").nth(1).and_then(|s| s.split('`').next()).expect(&told);
        let run = || std::process::Command::new("sh").arg("-c").arg(make).current_dir(&repo).status().unwrap().success();
        assert!(run(), "{make}");
        assert!(run(), "the same claim's copy made again: {make}");
        let dir = make.rsplit("tar -x -C ").next().unwrap().trim_matches('\'');
        assert!(Path::new(dir).join("parser.rs").exists() && !repo.join("parser.rs").exists());
        assert!(!repo.join(".git/worktrees").exists(), "nothing registered in the repo");
        assert!(told.ends_with(&format!("remove it after: `rm -rf {}`.", queen::sh_quote(dir))), "{told}");
        assert_ne!(make, review_of("t1.v1", &base, &head, "sw").split("apart from your tree, `").nth(1).unwrap().split('`').next().unwrap());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_fix_task_claim_resets_a_runner_worktree_or_takes_a_checkout_of_its_tree() {
        let root = std::env::temp_dir().join(format!("borg-hive-fix-claim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (repo, home) = (root.join("app"), root.join("store"));
        std::fs::create_dir_all(&repo).unwrap();
        git_ok(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README"), "base\n").unwrap();
        git_ok(&repo, &["add", "-A"]);
        git_ok(&repo, &["commit", "-qm", "base"]);
        // The combined tree a failed integration left: another commit.
        git_ok(&repo, &["checkout", "-q", "-b", "m"]);
        std::fs::write(repo.join("m.txt"), "merged\n").unwrap();
        git_ok(&repo, &["add", "-A"]);
        git_ok(&repo, &["commit", "-qm", "m"]);
        let m = git_ok(&repo, &["rev-parse", "HEAD"]);
        git_ok(&repo, &["checkout", "-q", "main"]);
        let wt = run::worktree(&repo, "g", 1);
        git_ok(&repo, &["worktree", "add", "-q", "-b", "hive/g/1", wt.to_str().unwrap()]);
        let g = goal_g();
        goal::create(&home, &g).unwrap();
        let fix = Task { from: Some(m.clone()), ..new_task("integrate.1.fix", Role::Work, "fix the integrated tree", "", vec![], vec![], vec![]).unwrap() };
        // The rejection the fix task is opened with: without it the open takes no effect.
        let (i, int) =
            ("integrate-d2-1", Task { role: Role::Integrate, ..new_task("integrate.1", Role::Work, "integrate", "", vec![], vec![], vec![]).unwrap() });
        let rejected = Event { head: m.clone(), ..Event::new(Ev::Rejected, "integrate.1", i, 1) };
        board::append(&tasks_path(&home, "g"), &[Event::open(int, i, 0), Event::new(Ev::Claim, "integrate.1", i, 0), rejected, Event::open(fix.clone(), i, 1)])
            .unwrap();
        let pid = std::process::id();
        let (runner_s, own_s) = (format!("fix-runner-{pid}"), format!("fix-own-{pid}"));
        bind_as(&home, &runner_s, "g", "d1").unwrap();
        bind_as(&home, &own_s, "g", "").unwrap();
        let (w, r) = (wt.display().to_string(), repo.display().to_string());
        // An engineer's checkout, or a session of its own: refused, nothing written.
        assert!(matches!(claim(&home, &g, &runner_s, &fix, false, &r, 2, verdict::SNAPSHOT_TIMEOUT, None), Err(Refused::NotHere(_))));
        assert!(matches!(claim(&home, &g, &own_s, &fix, false, &w, 2, verdict::SNAPSHOT_TIMEOUT, None), Err(Refused::NotHere(_))));
        assert_eq!(board::read(&tasks_path(&home, "g")).len(), 4);
        let b = board::fold(&board::read(&tasks_path(&home, "g")));
        assert!(offer(&home, &g, &b, &own_s, "app", &w, 2).starts_with("No claim, and no task"), "not offered where it cannot be claimed");
        assert!(offer(&home, &g, &b, &runner_s, "app", &w, 2).contains("integrate.1.fix"));
        // The runner drone in its own worktree: claimed from `from`, the worktree reset to it.
        std::fs::write(wt.join("README"), "stray edit\n").unwrap();
        std::fs::write(wt.join("stray.txt"), "x\n").unwrap();
        let now = learn::now_ms();
        let won = claim(&home, &g, &runner_s, &fix, false, &w, now, verdict::SNAPSHOT_TIMEOUT, None).expect("claimed");
        assert!(won.live(now + 1).is_some_and(|(by, _, _)| by == runner_s));
        assert_eq!(git_ok(&wt, &["rev-parse", "HEAD"]), m);
        assert_eq!(std::fs::read_to_string(wt.join("README")).unwrap(), "base\n");
        assert!(wt.join("m.txt").exists() && !wt.join("stray.txt").exists());
        let c = board::read(&tasks_path(&home, "g")).into_iter().find(|e| e.ev == Ev::Claim && e.task == "integrate.1.fix").unwrap();
        assert_eq!(c.base, m, "the diff is judged from the combined tree");
        let p = board::read(&tasks_path(&home, "g")).into_iter().find(|e| e.ev == Ev::Progress && e.task == "integrate.1.fix").unwrap();
        let kept = p.note.split("worktree before the reset: ").nth(1).expect("the discarded tree is kept");
        assert_eq!(git_ok(&repo, &["show", &format!("{kept}:stray.txt")]), "x");
        assert!(describe(&home, &g, &won, &runner_s, &w).contains(&format!("reset to the integrated tree {m}")));
        // Any session in a checkout of `from` (a goal worked by joined sessions): offered and claimed
        // there, never reset, its base `from`; with anything else in its tree it is refused.
        board::append(&tasks_path(&home, "g"), &[Event::new(Ev::Released, "integrate.1.fix", &runner_s, learn::now_ms())]).unwrap();
        let co = root.join("fix");
        git_ok(&repo, &["worktree", "add", "-q", "--detach", co.to_str().unwrap(), &m]);
        std::fs::write(co.join("notes.txt"), "mine\n").unwrap();
        let c = co.display().to_string();
        let now = learn::now_ms();
        let b = board::fold(&board::read(&tasks_path(&home, "g")));
        assert!(offer(&home, &g, &b, &own_s, "app", &c, now).contains("integrate.1.fix"));
        let refused = claim(&home, &g, &own_s, &fix, false, &c, now, verdict::SNAPSHOT_TIMEOUT, None);
        assert!(matches!(&refused, Err(Refused::Deps(why)) if why.contains("not at integrate.1.fix's start")), "{refused:?}");
        assert!(co.join("notes.txt").exists(), "never reset");
        std::fs::remove_file(co.join("notes.txt")).unwrap();
        let won = claim(&home, &g, &own_s, &fix, false, &c, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).expect("claimed in a checkout of from");
        let c = board::read(&tasks_path(&home, "g")).into_iter().rfind(|e| e.ev == Ev::Claim).unwrap();
        assert_eq!(c.base, m, "its base is the combined tree");
        assert!(describe(&home, &g, &won, &own_s, &co.display().to_string()).contains(&format!("You work on the integrated tree {m}")));
        unbind(&runner_s);
        unbind(&own_s);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A loss the claim's own read-back saw took out the deps it brought in (`take_out_all`) and left no
    /// start (`CLAIM_START_MARKER`): nothing is set aside, which would bring them back. A loss learnt
    /// later sets the tree aside to this claim's start, never to its base: a rework's base is its first
    /// claim's, older than another task's work done in the same tree since.
    #[test]
    fn a_lost_claim_is_set_aside_to_its_own_start_and_only_when_it_has_one() {
        let root = std::env::temp_dir().join(format!("borg-hive-set-aside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (w, home) = (root.join("app"), root.join("store"));
        std::fs::create_dir_all(&w).unwrap();
        git_ok(&w, &["init", "-q", "-b", "main"]);
        std::fs::write(w.join("a.txt"), "a\n").unwrap();
        git_ok(&w, &["add", "-A"]);
        git_ok(&w, &["commit", "-qm", "base"]);
        let g = goal_g();
        goal::create(&home, &g).unwrap();
        // The base: the first claim's tree. The start: this claim's, with a dep's and t2's work since.
        let base = verdict::snapshot(&w, verdict::SNAPSHOT_TIMEOUT).unwrap();
        std::fs::write(w.join("dep.txt"), "the dep's verified work\n").unwrap();
        std::fs::write(w.join("t2.txt"), "t2's done work\n").unwrap();
        let start = verdict::snapshot(&w, verdict::SNAPSHOT_TIMEOUT).unwrap();
        std::fs::write(w.join("mine.txt"), "lost work\n").unwrap();
        let session = format!("set-aside-{}", std::process::id());
        let t1 = new_task("t1", Role::Work, "work", "", vec![], vec![], vec![]).unwrap();
        let mine = Event { base: base.clone(), ..Event::new(Ev::Claim, "t1", &session, 20) };
        let lines: Vec<(u64, Event)> =
            [Event::open(t1, "eng", 1), Event::new(Ev::Claim, "t1", "rival", 10), mine].into_iter().enumerate().map(|(i, e)| (i as u64, e)).collect();
        let b = board::fold(&lines.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>());
        let ctx = Ctx {
            h: &crate::harness::sim::Sim,
            home: &home,
            g: &g,
            session: &session,
            cwd: w.to_str().unwrap(),
            repo: "app",
            now: 30,
            started: Instant::now(),
        };
        let news = ctx.lost_told(&b, &lines, "", true);
        assert_eq!(news.len(), 1, "told, nothing set aside: {news:?}");
        assert!(w.join("mine.txt").exists());
        let read = || std::fs::read_to_string(hook::state_file(&session)).unwrap_or_default();
        let state = format!("{}{CLAIM_START_MARKER}t1@20 {start} {}\n", read(), w.display());
        let news = ctx.lost_told(&b, &lines, &state, true);
        assert!(news.len() == 1 && news[0].contains("set aside in commit") && news[0].contains(&format!("git diff {start} ")), "{news:?}");
        assert!(w.join("dep.txt").exists() && w.join("t2.txt").exists() && !w.join("mine.txt").exists(), "only this claim's work is taken out");
        let marked = format!("{}{CLAIM_START_MARKER}t1@20 {start} {}\n", read(), w.display());
        assert!(ctx.lost_told(&b, &lines, &marked, true).is_empty(), "once");
        let _ = std::fs::remove_file(hook::state_file(&session));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_session_holding_a_task_has_no_lost_claim_whatever_its_latest_claim() {
        let t = |id: &str| Event::open(new_task(id, Role::Work, "work", "", vec![], vec![], vec![]).unwrap(), "eng", 1);
        let mut lines =
            vec![t("t1"), t("t2"), Event::new(Ev::Claim, "t2", "s", 5), Event::new(Ev::Claim, "t1", "rival", 8), Event::new(Ev::Claim, "t1", "s", 10)];
        let b = board::fold(&lines);
        assert_eq!(b.lost, vec![("s".to_string(), String::new(), "t1".to_string(), 10)], "its latest claim, t1's, lost");
        assert!(lost_claim(&b, "", &lines, "s", "", 11).is_none(), "it holds t2: its edits run and its tree stays");
        assert!(lost_claim(&b, "", &lines, "s", "", 11 + LEASE_MS).is_some(), "once t2's lease is gone, t1's loss counts");
        // A renewal of the held task is no loss either.
        lines.push(Event::new(Ev::Claim, "t2", "s", 12));
        assert!(lost_claim(&board::fold(&lines), "", &lines, "s", "", 13).is_none());
    }

    #[test]
    fn context_goes_first_and_waits_when_the_harness_takes_none() {
        let theirs = Some(json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": "lessons"}}));
        let out = with_context("s", "UserPromptSubmit", "hive".into(), true, theirs).unwrap();
        assert_eq!(out["hookSpecificOutput"]["additionalContext"], "hive\n\nlessons");
        let out = with_context("s", "PostToolUse", "hive".into(), true, None).unwrap();
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        assert_eq!(with_context("s", "PostToolUse", String::new(), true, None), None);
        let blocked = with_reason("hive", Some(json!({"decision": "block", "reason": "q"})), false).unwrap();
        assert_eq!(blocked["reason"], "hive\n\nq");
        assert_eq!(with_reason("hive", None, false), None, "never a block of its own on a second round");
        assert_eq!(with_reason("hive", None, true).unwrap()["reason"], "hive");
    }
}
