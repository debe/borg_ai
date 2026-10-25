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
//!   worktree `borg hive run` makes, with the session of each iteration, left when the run ends).
//! - `spend.jsonl`, `runs.jsonl` (run.rs): each headless iteration's cost (synced, naming its node:
//!   every node's runner reads the goal's one budget), each `borg hive run` (local).
//! - `logs/` (run.rs): each iteration's output, local only: it never syncs.
//!
//! `borg hive run` (run.rs) starts N headless drones, each in a git worktree of its own, until the
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
pub mod message;
pub mod oracle;
pub mod plan;
pub mod rework;
pub mod run;
mod sim_drone;
pub mod verdict;
pub mod view;

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
/// How long a claimant waits before reading the board back: a rival's line written at nearly the same
/// moment has landed by then, so the answer is the fold's.
pub(crate) const CLAIM_SETTLE: Duration = Duration::from_millis(150);
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
/// Tasks whose deps' work could not be brought into this session's tree (`Refused::Deps`): never
/// offered or handed to it again; `borg hive claim <task>` still tries.
const REFUSED_MARKER: &str = "#borg-hive-refused ";
const REPO_MARKER: &str = "#borg-hive-repo";
/// Each claim whose read-back saw it won, with a tree (`claim`): `task@ms sha top`, the tree it started
/// from (its base; a rework's base is its first claim's, so a snapshot taken at this claim; `-` for a
/// verify task) and the git top it works in. A lost claim is set aside to that tree (`set_aside`); one
/// its own read-back saw lost has none: it took out what it brought in.
const CLAIM_START_MARKER: &str = "#borg-hive-claim-start ";
/// Lost claims (`task@ms`) set aside, each once.
const SET_ASIDE_MARKER: &str = "#borg-hive-set-aside ";

/// Said once, at a drone's first hive prompt (docs/design/hive.md, inoculation framing).
pub const INOCULATION: &str = "The oracle gates done: your task counts as done only when its checks pass, and a verifier drone checks the work after you. \
Every change to tests, CI config or oracle files is recorded and shown to the verifier; gaming a check is visible, allowed to happen, and useless: \
the task counts as failed. Adding tests, also inside an existing test file, is fine; outside your task's files an existing test may change only in names and values, \
never lose a line, an assertion or gain a skip. If a check looks wrong, say so in your done note instead of changing it.";
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
/// projection), oracle.jsonl (the hidden scores), joins.jsonl, runs.jsonl (this node's worktrees and
/// runs), logs/ and scratch/. flags.jsonl does: a verifier on another node reads the author's
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

/// Whether a line's `node` is this store's: empty (a line from before nodes were stamped) or its id.
pub fn is_local(home: &Path, node: &str) -> bool {
    node.is_empty() || node == self::node(home)
}

/// Appends `events` to `goal`'s board in one write, each without a node stamped with this store's
/// (`node`): every line a drone, runner or engineer of this node writes. `board::append` stays raw.
/// The checks of a task this node opens are approved here (oracle.rs `Approvals`): it wrote them.
pub(crate) fn write(home: &Path, goal: &str, mut events: Vec<Event>) -> std::io::Result<()> {
    let here = node(home);
    for e in events.iter_mut().filter(|e| e.node.is_empty()) {
        e.node = here.clone();
    }
    let opened: Vec<Check> = events.iter().filter(|e| e.ev == Ev::Open).filter_map(|e| e.def.as_ref()).flat_map(|t| t.accept.clone()).collect();
    if !opened.is_empty() {
        oracle::approve(home, goal, &opened, learn::now_ms())?;
    }
    board::append(&tasks_path(home, goal), &events)
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
/// same file's lock (a second lock in one process waits for itself).
pub(crate) fn append_bytes(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::live::locked(path, || std::fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(body))
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
    /// The drone this session is an iteration of (`DRONE_ENV`); empty for a session of its own.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub drone: String,
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
/// `goal`; `by` is `join` (`borg hive join --here`) or `run` (a worktree of `borg hive run`, with the
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

/// Why integrate tasks are refused to a drone's claim.
const INTEGRATE_REFUSED: &str = "integrate tasks are run by `borg hive run` or `borg hive integrate`";

/// Whether `session` in `cwd` may take `t` (`may_claim`, and a fix task only in a runner worktree or
/// a checkout of its `from`).
fn may_take(g: &Goal, t: &Task, repo: &str, session: &str, cwd: &str) -> bool {
    may_claim(g, t, repo) && t.from.as_deref().is_none_or(|from| runner_worktree(g, session, cwd) || checkout_of(cwd, from))
}

/// The task `session` of `drone` in `cwd` takes next (board.rs `next_for`): one it `may_take` whose
/// commits are in this repo (`absent_from`), whose checks this node approved (oracle.rs `Approvals`), and
/// not one refused to it before (`REFUSED_MARKER`), which
/// `next_for`'s fixed order would pick again.
#[allow(clippy::too_many_arguments)]
fn next_open<'a>(home: &Path, g: &Goal, b: &'a Board, session: &str, drone: &str, repo: &str, cwd: &str, now: i64) -> Option<&'a TaskState> {
    let state = std::fs::read_to_string(hook::state_file(session)).unwrap_or_default();
    let refused = hook::listed(&state, REFUSED_MARKER);
    let events = std::cell::OnceCell::new();
    let here = node(home);
    let approvals = oracle::Approvals::load(home, &g.id);
    board::next_for(b, session, drone, &here, repo, now, |s| {
        !refused.contains(s.task.id.as_str())
            && may_take(g, &s.task, repo, session, cwd)
            && approvals.pending(g, &s.task).is_empty()
            && absent_from(home, &g.id, b, || events.get_or_init(|| board::read(&tasks_path(home, &g.id))).as_slice(), s, cwd).is_none()
    })
}

/// The node holding commits `s` starts from that `cwd`'s repo lacks: a verify task of work done on
/// another node whose snapshots are not here, a fix task of another node's integration whose `from`
/// is not. None for anything opened on this node (the git call runs only for another node's), or when
/// the commits are here: both nodes hold one repo, or the work's bundle brought them (commits.rs
/// `fetch`). A fix task's `from` does not travel yet: it is claimed where its integration ran.
fn absent_from<'e>(home: &Path, goal: &str, b: &Board, events: impl FnOnce() -> &'e [Event], s: &TaskState, cwd: &str) -> Option<String> {
    if is_local(home, &s.node) {
        return None;
    }
    let here = match (&s.task.from, s.task.role) {
        (Some(from), _) => verdict::present(Path::new(cwd), &[from], verdict::DIFF_TIMEOUT),
        // The work's commits, from its bundle when this repo lacks them (commits.rs).
        (None, Role::Verify) => match verdict::attempt(events(), b, &s.task.id) {
            Some(a) if !a.base.is_empty() && !a.head.is_empty() => commits::fetch(home, goal, &a, Path::new(cwd)),
            _ => return None,
        },
        _ => return None,
    };
    (!here).then(|| s.node.clone())
}

/// What a drone is told of a task whose commits are on another node.
fn elsewhere(task: &str, node: &str) -> String {
    format!("{task}: its work was done on node {} and is not in this repo; claim it on a node that holds it", short(node))
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

/// Whether `cwd` is the worktree `borg hive run` made for the drone `session` is an iteration of
/// (`d<n>` in `.worktrees/hive-<goal>-<n>`): the only place a fix task's claim may reset to its `from`.
/// An engineer's checkout, or a session of its own, never is.
fn runner_worktree(g: &Goal, session: &str, cwd: &str) -> bool {
    let Some(n) = bound(session).and_then(|hs| hs.drone.strip_prefix('d').and_then(|n| n.parse::<usize>().ok())) else { return false };
    let cwd = Path::new(cwd);
    let repo = PathBuf::from(borg_trace::repo_key(&cwd.display().to_string()));
    let same = |a: &Path, b: &Path| a == b || std::fs::canonicalize(a).is_ok_and(|a| std::fs::canonicalize(b).is_ok_and(|b| a == b));
    n >= 1 && same(cwd, &run::worktree(&repo, &g.id, n))
}

/// A shell word that runs this borg (the hook's own executable), so a printed command works in any
/// harness's shell.
fn exe() -> String {
    std::env::current_exe().map_or_else(|_| "borg".into(), |e| queen::sh_quote(&e.display().to_string()))
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
fn describe(g: &Goal, s: &TaskState, session: &str, cwd: &str) -> String {
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
        let start = if runner_worktree(g, session, cwd) { "Your worktree was reset to" } else { "You work on" };
        out.push_str(&format!(" {start} the integrated tree {from}; the integration failed: {}.", s.notes.first().map_or("", String::as_str)));
    }
    if s.rework > 0 && !s.notes.is_empty() {
        out.push_str(&format!(" Rework {} — the verifier's notes: {}", s.rework, s.notes.join(" | ")));
    }
    out
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
    let renew = board::holding(b, session, now).filter(|_| now - hs.beat_ms > HEARTBEAT_MS);
    let done = renew.is_some_and(|s| write(home, &hs.goal, vec![Event::new(Ev::Heartbeat, &s.task.id, session, now)]).is_ok());
    if done {
        hs.beat_ms = now;
    }
    done
}

/// Claims this session lost and was not told of yet: their lines, and the marker that tells them.
fn lost_news(b: &Board, session: &str, state: &str) -> (Vec<String>, Vec<String>) {
    let told = hook::listed(state, LOST_MARKER);
    let keys: Vec<(String, &str)> = b.lost.iter().filter(|(by, _, _)| by == session).map(|(_, t, ms)| (format!("{t}@{ms}"), t.as_str())).collect();
    let fresh: Vec<&(String, &str)> = keys.iter().filter(|(k, _)| !told.contains(k.as_str())).collect();
    let lines = fresh
        .iter()
        .map(|(_, t)| format!("Your claim on {t} did not take effect (another drone won it, or it was not open to you): pick another task."))
        .collect();
    (lines, fresh.iter().map(|(k, _)| k.clone()).collect())
}

/// The latest claim of `session` (`lines`, the board's), when the fold voided it (a race lost, across
/// nodes perhaps only at a sync after the claim looked won, or a claim the board did not allow) or the
/// session was told so (`LOST_MARKER`: a later line may hand the task back, `release_told_loss`), and
/// the session holds nothing now: its edits are refused (`pre_tool`) and its worktree set aside
/// (`set_aside_lost`). A session that claimed again since, or holds a task, has none.
fn lost_claim<'a>(b: &Board, lines: impl IntoIterator<Item = &'a Event>, session: &str, state: &str, now: i64) -> Option<&'a Event> {
    if board::holding(b, session, now).is_some() {
        return None;
    }
    let last = lines.into_iter().filter(|e| e.ev == Ev::Claim && e.by == session).max_by_key(|e| e.ms)?;
    let told = hook::listed(state, LOST_MARKER).contains(claim_key(last).as_str());
    (told || b.lost.iter().any(|(by, t, ms)| by == session && *t == last.task && *ms == last.ms)).then_some(last)
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
fn set_aside_due<'a>(b: &Board, lines: impl IntoIterator<Item = &'a Event>, session: &str, state: &str, now: i64) -> Option<&'a Event> {
    lost_claim(b, lines, session, state, now).filter(|c| {
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
    lines: impl IntoIterator<Item = &'a Event>,
    session: &str,
    state: &str,
    cwd: &str,
    now: i64,
    within: Duration,
) -> Option<String> {
    let c = set_aside_due(b, lines, session, state, now)?;
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

/// Releases the task the fold gives `session` back after its loss was told (`LOST_MARKER`): a line
/// that syncs later (the winner's release, written before this claim) can make a lost claim win. The
/// drone was told to move on and its tree may be set aside, so it must not hold the task unawares,
/// its heartbeats renewing it. Whether it released one.
fn release_told_loss(home: &Path, g: &Goal, b: &Board, session: &str, repo: &str, state: &str, now: i64) -> bool {
    let Some(s) = board::holding(b, session, now) else { return false };
    let key = format!("{}@{}", s.task.id, s.live(now).map_or(0, |l| l.2));
    if !hook::listed(state, LOST_MARKER).contains(key.as_str()) {
        return false;
    }
    let done = write(home, &g.id, vec![Event::new(Ev::Released, &s.task.id, session, now).with_note("its claim was told lost")]).is_ok();
    if done {
        count(home, stats::HIVE_RELEASED, session, &g.id, &s.task.id, repo);
    }
    done
}

/// The board of goal `g` for `session` at `now` (its lines with their offsets, the end offset, the
/// fold), after `release_told_loss`.
fn board_for(home: &Path, g: &Goal, session: &str, repo: &str, now: i64) -> (Vec<(u64, Event)>, u64, Board) {
    let path = tasks_path(home, &g.id);
    let read = || {
        let (events, end) = board::read_from(&path, 0);
        let b = board::fold(&events.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>());
        (events, end, b)
    };
    let (events, end, b) = read();
    let state = std::fs::read_to_string(hook::state_file(session)).unwrap_or_default();
    if release_told_loss(home, g, &b, session, repo, &state, now) { read() } else { (events, end, b) }
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
    let started = Instant::now();
    let session = input.get("session_id").and_then(Value::as_str).unwrap_or("unknown");
    let Some(mut hs) = bound(session) else { return inner() };
    let Ok(g) = goal::load(home, &hs.goal) else { return inner() };
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
    let repo = queen::repo_name(cwd);
    let (events, end, b) = board_for(home, &g, session, &repo, now);
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
        "stop" => ctx.stop(&mut hs, &b, &events, input, started, inner),
        "end" => {
            let out = inner();
            if let Some(s) = board::holding(&b, session, now)
                && write(home, &g.id, vec![Event::new(Ev::Released, &s.task.id, session, now).with_note("session ended")]).is_ok()
            {
                count(home, stats::HIVE_RELEASED, session, &g.id, &s.task.id, &repo);
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
/// worktree is not set aside here (a 2 s hook on opencode and omp): at the stop or the next claim. Any
/// other tool gets None at once; an edit in no goal after one failed file open.
pub fn pre_tool(h: &dyn Harness, home: &Path, input: &Value, now: i64) -> Option<Value> {
    let tool = input.get("tool_name").and_then(Value::as_str).unwrap_or_default();
    hook::edit_text(tool, input.get("tool_input").unwrap_or(&Value::Null))?;
    let session = input.get("session_id").and_then(Value::as_str).unwrap_or("unknown");
    let g = goal::load(home, &bound(session)?.goal).ok()?;
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
    let repo = queen::repo_name(cwd);
    let (events, _, b) = board_for(home, &g, session, &repo, now);
    let state = std::fs::read_to_string(hook::state_file(session)).unwrap_or_default();
    let task = lost_claim(&b, events.iter().map(|(_, e)| e), session, &state, now)?.task.clone();
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
        let (mut news, keys) = lost_news(b, self.session, state);
        if !keys.is_empty() {
            hook::append(self.session, &format!("{LOST_MARKER}{}", keys.join(",")));
        }
        let lines = || lines.iter().map(|(_, e)| e);
        if settle {
            let total = crate::expert_route::stop_budget(self.h).unwrap_or(crate::expert_route::DEFAULT_STOP_BUDGET);
            let left = total.saturating_sub(self.started.elapsed()).saturating_sub(crate::expert_route::STOP_MARGIN);
            news.extend(set_aside_lost(self.g, b, lines(), self.session, state, self.cwd, self.now, left));
        } else if let Some(c) = set_aside_due(b, lines(), self.session, state, self.now).filter(|c| keys.contains(&claim_key(c))) {
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
        let holding = board::holding(b, self.session, self.now);
        let key = holding.map_or_else(|| "-".to_string(), |s| format!("{}@{}", s.task.id, s.live(self.now).map_or(0, |l| l.2)));
        match holding {
            Some(s) if last != Some(key.as_str()) => parts.push(describe(self.g, s, self.session, self.cwd)),
            Some(s) => parts.push(format!("Your claim: {} ({}) {}.", s.task.id, s.task.role.name(), s.task.title)),
            None => parts.push(offer(self.home, self.g, b, self.session, self.repo, self.cwd, self.now)),
        }
        if last != Some(key.as_str()) {
            marks.push(format!("{TASK_MARKER}{key}"));
        }
        parts.extend(self.lost_told(b, events, &state, false));
        // What other drones and engineers did since the last prompt, newest last.
        let cursor = if hs.cursor > end { 0 } else { hs.cursor };
        let changes: Vec<&Event> = events.iter().filter(|(at, e)| *at > cursor && e.by != self.session && e.ev != Ev::Heartbeat).map(|(_, e)| e).collect();
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
                        let reply =
                            command(&format!("tell \"<answer>\" --re {} --to {}", queen::sh_quote(&m.id), queen::sh_quote(&m.by)), self.session, self.cwd);
                        format!("- {}", message::render(m, &here, &reply))
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
                .filter(|c| c.by != self.session && !told.contains(c.id.as_str()))
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
        let hits: Vec<&Contract> =
            contract::matching(&current, self.repo, &refs, &text).into_iter().filter(|c| c.by != self.session && !told.contains(c.id.as_str())).collect();
        if !hits.is_empty() {
            for c in &hits {
                parts.push(format!("This edit lands in {}", contract::render(c)));
            }
            parts.push(contract::LABEL.into());
            marks.push(format!("{CONTRACT_MARKER}{}", hits.iter().map(|c| c.id.as_str()).collect::<Vec<_>>().join(",")));
        }
        if let Some(held) = board::holding(b, self.session, self.now).filter(|s| !s.task.files.is_empty()) {
            let globs: Vec<regex::Regex> = held.task.files.iter().map(|g| crate::assimilate_dir::glob(g)).collect();
            let warned = hook::listed(state, SCOPE_MARKER);
            let outside: Vec<&str> = refs.iter().copied().filter(|f| !globs.iter().any(|g| g.is_match(f)) && !warned.contains(f) && !f.contains(',')).collect();
            for f in &outside {
                parts.push(format!(
                    "{f} is outside {}'s scope ({}): fine if the task needs it, but another drone may own it.",
                    held.task.id,
                    held.task.files.join(", ")
                ));
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
    /// over the next task); without one, `inner()` with any waiting hive text.
    fn stop(
        &self,
        hs: &mut HiveSession,
        b: &Board,
        lines: &[(u64, Event)],
        input: &Value,
        started: Instant,
        inner: impl FnOnce() -> Option<Value>,
    ) -> Option<Value> {
        let first_round = input.get("stop_hook_active").and_then(Value::as_bool) != Some(true);
        let waiting = std::fs::read_to_string(wait_path(self.session)).unwrap_or_default();
        let _ = std::fs::remove_file(wait_path(self.session));
        let path = tasks_path(self.home, &self.g.id);
        let Some(held) = board::holding(b, self.session, self.now) else {
            let state = std::fs::read_to_string(hook::state_file(self.session)).unwrap_or_default();
            let lost = self.lost_told(b, lines, &state, true);
            let mut text = waiting;
            if !lost.is_empty() {
                text.push_str(&format!("{} {}", self.head(), lost.join("\n")));
            }
            return with_reason(&text, inner(), first_round);
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
            return with_reason(&text, inner(), first_round || handed);
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
            let reason = format!(
                "{waiting}[borg hive] {} is not done: {what}\nFix it, or run `{}` (no hook time limit), or `{} \"why\"` if you are stuck.",
                task.id,
                command("done", self.session, self.cwd),
                command("block", self.session, self.cwd)
            );
            // No conscience round on unfinished work: its model call would judge work still changing.
            return Some(json!({"decision": "block", "reason": reason}));
        }
        let mut text = waiting;
        let mut handed = false;
        if let Some(check) = verdict.check() {
            // At the cap: the drone may stop; the board says why the task stands still.
            let (ev, note) = match task.role {
                Role::Verify => (Ev::Released, format!("no verdict after {MAX_ORACLE_BLOCKS} stops")),
                _ => (Ev::Blocked, format!("oracle failed {MAX_ORACLE_BLOCKS} times at stop: {check}")),
            };
            let _ = write(self.home, &self.g.id, vec![Event::new(ev, &task.id, self.session, self.now).with_note(&note)]);
            text.push_str(&format!(
                "[borg hive] {} is {} on the board ({note}); an engineer reopens it with `borg hive open {} --id {} --again`.\n\n",
                task.id,
                ev.name(),
                self.g.id,
                task.id
            ));
        } else {
            let (said, next) = self.done(held, input, &path, &left);
            text.push_str(&said);
            if next {
                handed = true;
                hs.beat_ms = self.now;
            }
        }
        hs.oracle_blocks = 0;
        hs.blocks_for.clear();
        save(self.session, hs);
        pass_budget(left());
        // A new claim reaches the drone on any round: it must not stop holding a task it never heard of.
        with_reason(&text, inner(), first_round || handed)
    }

    /// The oracle passed at the stop: the done (`finish`), then the next open task claimed for this
    /// drone. The text to block with, and whether it holds a new claim; a done the board did not take
    /// says so and hands over nothing. The done's snapshot keeps within what is `left` of the hook's
    /// time; the next claim is made only when time is left.
    fn done(&self, held: &TaskState, input: &Value, path: &Path, left: &dyn Fn() -> Duration) -> (String, bool) {
        let task = &held.task;
        let message = crate::harness::plan(self.h.caps(), input);
        match finish(self.home, self.g, held, self.session, Path::new(self.cwd), &message, left()) {
            Ok(vid) => match self.hand_next(path, left()) {
                Some(next) => (format!("[borg hive] {} done, verify task {vid} opened. Next: {next}\n\n", task.id), true),
                None => (String::new(), false),
            },
            Err(Unfinished::Lost(why)) => (format!("[borg hive] {why}\n\n"), false),
            Err(Unfinished::Io(e)) => (format!("[borg hive] the done of {} was not recorded: {e}\n\n", task.id), false),
        }
    }

    /// Claims the next open task for this drone (its snapshot within `left`): what it is told of it.
    /// A task whose deps cannot be brought in is passed over for the next. None when no task is open,
    /// the claim was lost or refused, or less than `MIN_ORACLE` of the hook's time is left: the next
    /// prompt offers the task instead.
    fn hand_next(&self, path: &Path, left: Duration) -> Option<String> {
        if left < MIN_ORACLE {
            return None;
        }
        let won = loop {
            let now = self.clock();
            let b = board::fold(&board::read(path));
            let next = next_open(self.home, self.g, &b, self.session, &drone_of(self.session), self.repo, self.cwd, now)?;
            match claim(self.home, self.g, self.session, &next.task, self.cwd, now, left) {
                Ok(won) => break won,
                // Passed over, as `borg hive claim` does: `next_open` picks another.
                Err(Refused::Deps(_)) => refuse(self.session, &next.task.id),
                Err(_) => return None,
            }
        };
        count(self.home, stats::HIVE_CLAIM, self.session, &self.g.id, &won.task.id, self.repo);
        let now = self.clock();
        hook::append(self.session, &format!("{TASK_MARKER}{}@{}", won.task.id, won.live(now).map_or(0, |l| l.2)));
        let risk = plan::escalate(self.home, self.g, &won.task, self.session, self.cwd, now).map(|t| format!("\n{t}")).unwrap_or_default();
        Some(format!("{}{risk}", describe(self.g, &won, self.session, self.cwd)))
    }
}

/// The `done` of the claim `held` (claimed at `since`) by `session` in `dir`, with a snapshot of the
/// work (`head`, verdict.rs, `within` that time), and the verify task it opens, listing the guard's
/// flags of the claim. The done is stamped once its snapshot is taken: the line lands at its ms.
fn done_events(home: &Path, g: &Goal, held: &TaskState, session: &str, dir: &Path, since: i64, within: Duration) -> (Event, Task) {
    let head = verdict::snapshot(dir, within).unwrap_or_default();
    let now = learn::now_ms();
    let mut done = Event::new(Ev::Done, &held.task.id, session, now);
    done.head = head;
    let flags: Vec<String> = guard::flags_of(home, &g.id, session, since, now).iter().map(guard::render).collect();
    let mut check = verify_task(&held.task, held.rework, session);
    check.flags = flags;
    (done, check)
}

/// Why a `done` took no effect.
enum Unfinished {
    /// The board could not be written.
    Io(String),
    /// The board ignored it: the claim had passed to another drone (board.rs `apply`). What to tell
    /// the drone.
    Lost(String),
}

/// Publishes the `done` of the claim `held` by `session` in `dir` (`done_events`, `within` that time)
/// and the verify task it opens in one write, then reads the board back: the verify task's id when
/// the board took the done. A done whose claim passed to another drone meanwhile (a suite that ran past
/// the lease) is ignored by the fold, and the verify task's open with it: then nothing is counted and
/// the drone is told who holds the task. `borg hive done` and the stop share it.
fn finish(home: &Path, g: &Goal, held: &TaskState, session: &str, dir: &Path, note: &str, within: Duration) -> Result<TaskId, Unfinished> {
    let path = tasks_path(home, &g.id);
    let id = &held.task.id;
    let since = match held.status {
        Status::Claimed { since, .. } => since,
        _ => i64::MIN,
    };
    let (done, check) = done_events(home, g, held, session, dir, since, within);
    let (vid, ms) = (check.id.clone(), done.ms);
    write(home, &g.id, vec![done.with_note(&hook::one_line(&crate::notes::redact(note), 200)), Event::open(check, session, ms)])
        .map_err(|e| Unfinished::Io(e.to_string()))?;
    let events = board::read(&path);
    let b = board::fold(&events);
    match b.tasks.get(id) {
        Some(s) if s.status == (Status::Done { by: session.to_string(), ms }) => {}
        s => {
            let now = learn::now_ms();
            let held = s.map_or_else(|| "gone".to_string(), |s| held_by(s, now));
            return Err(Unfinished::Lost(format!("{id}'s done did not take effect: the task is {held} now. {NOT_YOURS}")));
        }
    }
    // The attempt's commits, for a verifier or an integrator on another machine (a store in a mesh only).
    if let Some(a) = verdict::attempt(&events, &b, &vid)
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

/// Why a claim took nothing. Only `Lost` is a race worth another try.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Refused {
    /// Another drone won it, or it was not open to this one.
    Lost,
    /// A dep's verified work cannot be brought into this tree: why.
    Deps(String),
    /// A fix task outside a runner worktree or a checkout of its tree: where it is claimed.
    NotHere(String),
    /// The board could not be written.
    Io(String),
    /// The claim won, but the fix task's worktree reset failed: why, and where the discarded tree is
    /// kept. The claim is released.
    Reset(String),
}

/// Marks `task` refused to `session` (`REFUSED_MARKER`).
fn refuse(session: &str, task: &str) {
    hook::append(session, &format!("{REFUSED_MARKER}{task}"));
}

/// Claims `task` for `session` in `cwd` (a claim and a heartbeat in one write), waits for rivals' lines
/// to land, and reads the board back: the task when the claim won. A claim of work carries the tree it
/// starts from (`base`, verdict.rs), which the verdict diffs against the done's: the base of this
/// drone's (or, without a drone, this session's) first claim of the task when it had one (a tree that
/// an earlier attempt already changed, say a test it weakened, is no base: the rework's diff must
/// show it), else a snapshot taken now, `within` that time. The claim is stamped after the snapshot
/// (`now` moved on by it), so it lands at its ms and never wins a race it reached late. A fix task
/// (`from`, integrate.rs) starts from the combined tree: a runner drone in its own worktree claims it
/// with `from` as its base, and once the claim won the worktree is reset to it (`reset_to`); any
/// other session only in a checkout of `from` (`checkout_of`), never reset, its base a snapshot.
///
/// Work with deps starts from their verified work: before the base is taken, each dep's accepted
/// attempt (board.rs `closure` order) is brought into the tree (verdict.rs `bring_in`), and the claim
/// names them (`deps_in`). The base then holds them, so the work's diff is its own. A dep that cannot
/// be brought in (this tree's own changes overlap it) refuses the claim, and a lost claim takes out
/// what it brought in: either way the tree is as it was.
fn claim(home: &Path, g: &Goal, session: &str, task: &Task, cwd: &str, now: i64, within: Duration) -> Result<TaskState, Refused> {
    let path = tasks_path(home, &g.id);
    let drone = drone_of(session);
    let reset = task.from.is_some() && runner_worktree(g, session, cwd);
    if let Some(from) = task.from.as_deref().filter(|from| !reset && !checkout_of(cwd, from)) {
        return Err(Refused::NotHere(fix_where(&task.id, from)));
    }
    let t0 = Instant::now();
    let (deps_in, brought) = bring_deps(home, &g.id, task, cwd, within)?;
    let take_out = || take_out_all(cwd, &brought, within.saturating_sub(t0.elapsed()));
    let rest = within.saturating_sub(t0.elapsed());
    // A fix task's claim keeps a snapshot of what the reset will discard in its note: recoverable.
    // The tree this claim starts from (`CLAIM_START_MARKER`): its base, but for a rework a snapshot now.
    let (base, note, start) = match (&task.from, task.role != Role::Verify && !cwd.is_empty()) {
        (Some(from), _) if reset => {
            (from.clone(), verdict::snapshot(Path::new(cwd), rest).map(|s| format!("worktree before the reset: {s}")).unwrap_or_default(), from.clone())
        }
        (_, true) => {
            let now = verdict::snapshot(Path::new(cwd), rest).unwrap_or_default();
            (first_base(&board::read(&path), &task.id, session, &drone).unwrap_or_else(|| now.clone()), String::new(), now)
        }
        (_, false) => (String::new(), String::new(), String::new()),
    };
    let left = within.saturating_sub(t0.elapsed());
    let now = now + t0.elapsed().as_millis() as i64;
    let mut c = Event::new(Ev::Claim, &task.id, session, now).with_drone(&drone).with_note(&note);
    c.base = base;
    c.deps_in = deps_in;
    let id = task.id.as_str();
    if let Err(e) = write(home, &g.id, vec![c, Event::new(Ev::Heartbeat, id, session, now)]) {
        take_out();
        return Err(Refused::Io(format!("could not write the board {}: {e}", path.display())));
    }
    std::thread::sleep(CLAIM_SETTLE);
    let b = board::fold(&board::read(&path));
    let Some(s) = b.tasks.get(id).filter(|s| s.live(now).is_some_and(|(by, _, _)| by == session)) else {
        take_out();
        return Err(Refused::Lost);
    };
    if let Some(from) = task.from.as_ref().filter(|_| reset)
        && let Err(why) = reset_to(Path::new(cwd), from, left)
    {
        let _ = write(home, &g.id, vec![Event::new(Ev::Released, id, session, learn::now_ms()).with_note(&why)]);
        let kept = note.strip_prefix("worktree before the reset: ").map(|s| format!("; what the worktree held is kept in commit {s}")).unwrap_or_default();
        return Err(Refused::Reset(format!("{why}: the claim of {id} is released{kept}")));
    }
    if !cwd.is_empty() {
        let start = if start.is_empty() { "-" } else { start.as_str() };
        hook::append(session, &format!("{CLAIM_START_MARKER}{id}@{now} {start} {}", repo_top(Path::new(cwd)).display()));
    }
    if let Some(mut hs) = bound(session) {
        hs.beat_ms = now;
        save(session, &hs);
    }
    Ok(s.clone())
}

/// A dep's change a claim brought into its tree: the (base, head) of its accepted attempt.
type Brought = (String, String);

/// Brings the verified work of `task`'s deps into `cwd` for its claim (see `claim`), `within` that
/// time: the claim's `deps_in`, and the (base, head) of each change it applied, to take out if the
/// claim is lost. Nothing for a verify or fix task, a task without deps, or no directory.
fn bring_deps(home: &Path, goal: &str, task: &Task, cwd: &str, within: Duration) -> Result<(Vec<String>, Vec<Brought>), Refused> {
    if task.role == Role::Verify || task.from.is_some() || task.deps.is_empty() || cwd.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let t0 = Instant::now();
    let events = board::read(&tasks_path(home, goal));
    let b = board::fold(&events);
    let (mut deps_in, mut brought): (Vec<String>, Vec<Brought>) = (Vec::new(), Vec::new());
    let refuse = |brought: &[Brought], why: String| {
        take_out_all(cwd, brought, within.saturating_sub(t0.elapsed()));
        Refused::Deps(why)
    };
    for d in board::closure(&b, task) {
        let Some(a) = verdict::accepted(&events, &b, &d).filter(|a| !a.base.is_empty() && !a.head.is_empty()) else {
            return Err(refuse(&brought, format!("{d} has no verified work to bring into this tree")));
        };
        // A dep done on another machine: its commits from its bundle (commits.rs), else git says why below.
        commits::fetch(home, goal, &a, Path::new(cwd));
        match verdict::bring_in(Path::new(cwd), &a.base, &a.head, within.saturating_sub(t0.elapsed())) {
            Ok(applied) => {
                if applied {
                    brought.push((a.base.clone(), a.head.clone()));
                }
                deps_in.push(format!("{d}@{}", a.head));
            }
            Err(tail) => {
                let why = format!("cannot bring {d}'s verified work into this tree: {tail} (a drone whose tree does not overlap it can claim {})", task.id);
                return Err(refuse(&brought, why));
            }
        }
    }
    Ok((deps_in, brought))
}

/// Takes the changes `bring_deps` applied out of `cwd` again, the last first, all `within` what is
/// left of the claim's time (at least `MIN_ORACLE`, as `reset_to`: the undo comes first).
fn take_out_all(cwd: &str, brought: &[Brought], within: Duration) {
    let deadline = Instant::now() + within.max(MIN_ORACLE);
    for (base, head) in brought.iter().rev() {
        let _ = verdict::take_out(Path::new(cwd), base, head, deadline.saturating_duration_since(Instant::now()));
    }
}

/// Resets the runner worktree `dir` to `from` (a fix task's start): `checkout -f --detach`, then
/// `clean -fd` (ignored files, build output, stay), each within `RESET_TIMEOUT` and both `within`.
/// What it discards was in no accepted attempt: every one is inside `from`.
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
        !left.is_zero() && crate::runner::run_capped_status(cmd, None, left).is_some_and(|(s, _)| s.success())
    };
    if !run(&["checkout", "-q", "-f", "--detach", from]) {
        return Err(format!("could not reset the worktree to the integrated tree {from}"));
    }
    if !run(&["clean", "-fdq"]) {
        return Err(format!("could not clean the worktree after the reset to {from}"));
    }
    Ok(())
}

/// The base of the first claim of `task` by `drone` (by `session` when it has none) that has one.
fn first_base(events: &[Event], task: &str, session: &str, drone: &str) -> Option<String> {
    events
        .iter()
        .filter(|e| e.ev == Ev::Claim && e.task == task && !e.base.is_empty())
        .filter(|e| if drone.is_empty() { e.by == session } else { e.drone == drone })
        .min_by_key(|e| e.ms)
        .map(|e| e.base.clone())
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
    "--watch",
    "--drones",
    "--harness",
    "--model",
    "--base",
    "--iter-timeout",
    "--dry-run",
    "--here",
    "--drone",
    "--hidden-dir",
    "--hidden",
    "--hidden-timeout",
    "--to",
    "--re",
    "--group",
];

/// The flags that take no value.
const SWITCHES: &[&str] = &["--pass", "--again", "--json", "--watch", "--dry-run", "--here", "--yes"];

const USAGE: &str = "usage: borg hive <command> [--dir DIR] [--session ID|DRONE]
  init <goal> --spec TEXT|--spec-file F --oracle NAME=CMD... --budget USD [--timeout S] [--repo NAME[@NODE]]...
       [--task \"ID|role|title|files,..|deps,..\"]...       a goal, its oracle and its first tasks
       [--hidden-dir DIR --hidden CMD... [--hidden-timeout S]]  held-out checks outside the repo, run after verdicts
  score <goal>                                              the hidden oracle scores the judged tasks not scored yet
  integrate <goal>                                          merge the accepted work and run the oracle on it (git 2.40+)
  approve <goal>                                            run the checks other nodes wrote here (lists each one it approves)
  board [<goal>] [--json] [--watch]                         the board: tasks by status, holders, rework
  join <goal> [--session ID|DRONE]                          a running drone joins the goal
  join <goal> --here [--drone NAME]                         every new session in this repo (or worktree) joins it
  leave [--here]                                            the drone leaves (its claim is released); --here: new
                                                            sessions here join no more (joined ones stay)
  run <goal> --drones N [--harness H] [--model M] [--base REF] [--iter-timeout S] [--dry-run] [--yes]
                                                            N headless drones in worktrees until done or the budget is spent,
                                                            once the engineer approved the plan (--yes: approved here)
  open <goal> --id ID --role work|plan|verify|integrate --title T [--repo R] [--files a,b] [--deps x,y]
       [--accept NAME=CMD]... [--again] [--group K]         an engineer opens a task (--again reopens a blocked one;
                                                            --group K: K attempts ID.g1.. by different drones, the
                                                            best integrated, `borg learn --group <goal>/ID` compares them)
  drone commands (the goal is the session's):
  claim [TASK] | progress \"note\" | release | block \"why\" | done [--note N]
  verify --pass | --reject \"why\" (or end the turn with `VERDICT: pass|reject <why>`) | contract \"text\" --scope a,b [--supersedes ID]
  ask \"text\" [--to TASK|DRONE|SESSION] | tell \"text\" [--to TASK|DRONE|SESSION] [--re ID]   a message to the goal's drones";

/// The command line: words, flag values (repeatable), and switches.
struct Args<'a> {
    words: Vec<&'a str>,
    values: Vec<(&'a str, &'a str)>,
    switches: Vec<&'a str>,
}

impl<'a> Args<'a> {
    fn parse(args: &[&'a str]) -> Result<Args<'a>, String> {
        let mut out = Args { words: Vec::new(), values: Vec::new(), switches: Vec::new() };
        let mut i = 0;
        while i < args.len() {
            let a = args[i];
            if SWITCHES.contains(&a) {
                out.switches.push(a);
            } else if FLAGS.contains(&a) {
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

/// `ID|role|title|files,..|deps,..` (`--task`).
fn parse_task(s: &str) -> Result<Task, String> {
    let f: Vec<&str> = s.split('|').map(str::trim).collect();
    let (Some(id), Some(role), Some(title)) = (f.first(), f.get(1), f.get(2)) else {
        return Err(format!("--task {s:?}: give ID|role|title[|files,..[|deps,..]]"));
    };
    let role = Role::parse(role).ok_or_else(|| format!("--task {s:?}: role is plan, work, verify or integrate"))?;
    new_task(id, role, title, "", f.get(3).map(|x| split_list(x)).unwrap_or_default(), f.get(4).map(|x| split_list(x)).unwrap_or_default(), Vec::new())
}

fn new_task(id: &str, role: Role, title: &str, repo: &str, files: Vec<String>, deps: Vec<String>, accept: Vec<Check>) -> Result<Task, String> {
    if !valid_task_id(id) {
        return Err(format!("invalid task id {id:?}: use letters, digits, '.', '_', '-', at most 40"));
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
        let route = match collective::load() {
            Ok(cfg) => collective::route(cfg.as_ref(), &cwd, &learn::home()),
            Err(e) => return Err(fail(format!("invalid collectives config: {e}"))),
        };
        let collective::Route::Collective { home, .. } = route else {
            eprintln!("off: {cwd} belongs to no collective; borg records nothing here");
            return Err(1);
        };
        // The drone's scratch state, as its hooks keep it (hook.rs main).
        if std::env::var_os("BORG_STATE").is_none() {
            // SAFETY: single-threaded: the command has started no thread yet.
            unsafe { std::env::set_var("BORG_STATE", home.join("state")) };
        }
        Ok(Here { home, cwd, now: learn::now_ms() })
    }

    fn engineer(&self) -> String {
        format!("engineer:{}", learn::engineer(&self.cwd))
    }

    /// The session a drone command acts for, and its goal: `--session` names a hive session, or a drone
    /// (`borg top`'s designation or session prefix); without it, the one drone of this repo.
    fn drone(&self, named: Option<&str>) -> Result<(String, HiveSession), String> {
        if let Some(n) = named
            && let Some(hs) = bound(n)
        {
            return Ok((n.to_string(), hs));
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
        board::fold(&board::read(&tasks_path(&self.home, &g.id)))
    }
}

/// `borg hive <command>` (see `USAGE`). Exit 0, 1 on an error, 2 on a usage error, 3 when a claim lost.
pub fn main(args: &[&str]) -> i32 {
    let a = match Args::parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("borg hive: {e}\n{USAGE}");
            return 2;
        }
    };
    let Some((&cmd, rest)) = a.words.split_first() else {
        eprintln!("{USAGE}");
        return 2;
    };
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
        ("join", [id]) => join(&here, id, session),
        ("leave", []) if a.on("--here") => leave_here(&here),
        ("run", [id]) => run_cmd(&here, &a, id),
        ("sim-drone", []) if std::env::var_os(crate::harness::TEST_ENV).is_some() => sim_drone::main(&here),
        ("leave", []) => match here.drone(session) {
            Ok((s, hs)) => {
                let b = goal::load(&here.home, &hs.goal).map(|g| here.board(&g)).unwrap_or_default();
                if let Some(t) = board::holding(&b, &s, here.now) {
                    let _ = write(&here.home, &hs.goal, vec![Event::new(Ev::Released, &t.task.id, &s, here.now).with_note("left the goal")]);
                }
                unbind(&s);
                println!("{} left hive goal {}", short(&s), hs.goal);
                0
            }
            Err(e) => fail(e),
        },
        ("open", [id]) => open(&here, &a, id),
        ("claim", [] | [_]) => claim_cmd(&here, session, rest.first().copied()),
        ("progress" | "block", [_, ..]) => {
            let ev = if cmd == "progress" { Ev::Progress } else { Ev::Blocked };
            held_event(&here, session, ev, &rest.join(" "))
        }
        ("release", []) => held_event(&here, session, Ev::Released, ""),
        ("done", []) => done_cmd(&here, session, a.value("--note").unwrap_or_default()),
        ("score", [id]) => score_cmd(&here, id),
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
        ("contract", [_, ..]) => contract_cmd(&here, session, &rest.join(" "), a.list("--scope"), a.value("--supersedes")),
        ("ask" | "tell", [_, ..]) => message_cmd(&here, session, cmd, &rest.join(" "), a.value("--to"), a.value("--re")),
        _ => {
            eprintln!("{USAGE}");
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
    let tasks = match tasks.and_then(|t| check_deps(&t, |_| false).map(|()| t)) {
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
    };
    if let Err(e) = goal::create(&here.home, &g) {
        return fail(e);
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
    0
}

fn board_cmd(here: &Here, g: &Goal, a: &Args) -> i32 {
    loop {
        let now = learn::now_ms();
        let b = here.board(g);
        if a.on("--json") {
            let rows: Vec<Value> = b
                .tasks
                .values()
                .map(|s| {
                    let (by, renewed) = match &s.status {
                        Status::Claimed { by, renewed, .. } => (by.clone(), *renewed),
                        Status::Done { by, ms } | Status::Verified { by, ms } => (by.clone(), *ms),
                        Status::Blocked { by, .. } => (by.clone(), 0),
                        Status::Open => (String::new(), 0),
                    };
                    json!({"id": s.task.id, "role": s.task.role, "title": s.task.title, "status": s.shown_status(now), "by": by, "renewed": renewed,
                        "rework": s.rework, "deps": s.task.deps})
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
        if !a.on("--watch") {
            print!("{head}{body}");
            return 0;
        }
        print!("\x1b[2J\x1b[H{head}{body}");
        std::thread::sleep(Duration::from_secs(2));
    }
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
        eprintln!("borg hive run: give --drones N (1 to {})", run::MAX_DRONES);
        return 2;
    };
    let harness = match a.value("--harness") {
        Some(n) => match crate::harness::by_name(n) {
            Some(h) => h,
            None => {
                eprintln!("borg hive run: unknown harness {n} (one of {})", crate::harness::names(", "));
                return 2;
            }
        },
        None => crate::harness::current(),
    };
    let iter_timeout = match a.value("--iter-timeout").map(str::parse::<u64>) {
        None => run::DEFAULT_ITER_TIMEOUT,
        Some(Ok(s)) if s > 0 => Duration::from_secs(s),
        Some(_) => {
            eprintln!("borg hive run: --iter-timeout is seconds, above 0");
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
    let task = if a.on("--again") {
        match here.board(&g).tasks.get(id) {
            Some(s) if matches!(s.status, Status::Blocked { .. }) => s.task.clone(),
            Some(s) => return fail(format!("{id} is {}, not blocked: nothing to reopen", s.status.name())),
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
            Ok(t) if b.tasks.contains_key(&t.id) => return fail(format!("task {} exists already (reopen a blocked one with --again)", t.id)),
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
    let opens: Vec<Event> = tasks.iter().map(|t| Event::open(t.clone(), &here.engineer(), here.now)).collect();
    if let Err(e) = write(&here.home, goal_id, opens) {
        return fail(e);
    }
    for task in &tasks {
        println!("opened {} ({}) {} in goal {goal_id}", task.id, task.role.name(), task.title);
    }
    if let [first, ..] = tasks.as_slice()
        && let Some(gr) = &first.group
    {
        println!("group {gr}: {} attempts, each another drone's (`borg hive run` needs one drone more, to verify them); the best is integrated", tasks.len());
    }
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
    let (events, _, b) = board_for(&here.home, &g, &session, &repo, learn::now_ms());
    let state = std::fs::read_to_string(hook::state_file(&session)).unwrap_or_default();
    if let Some(told) = set_aside_lost(&g, &b, events.iter().map(|(_, e)| e), &session, &state, &here.cwd, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT) {
        println!("{told}");
    }
    let mut lost = 0;
    while lost < if task.is_some() { 1 } else { CLAIM_TRIES } {
        let now = learn::now_ms();
        let b = here.board(&g);
        if let Some(s) = board::holding(&b, &session, now) {
            return fail(format!("{} holds {} already: finish, release or block it first", short(&session), s.task.id));
        }
        let pick = match task {
            Some(t) => match b.tasks.get(t) {
                None => return fail(format!("no task {t} in goal {}", g.id)),
                Some(s) if s.task.role == Role::Integrate => return fail(format!("{t}: {INTEGRATE_REFUSED}")),
                Some(s) if s.task.from.as_deref().is_some_and(|f| !runner_worktree(&g, &session, &here.cwd) && !checkout_of(&here.cwd, f)) => {
                    return fail(fix_where(t, s.task.from.as_deref().unwrap_or_default()));
                }
                Some(s) if !may_claim(&g, &s.task, &repo) => {
                    return fail(format!("{t} belongs to repo {}, and {repo} is outside the goal's repos", s.task.repo));
                }
                Some(s) if !oracle::approved(&here.home, &g, &s.task) => {
                    let approvals = oracle::Approvals::load(&here.home, &g.id);
                    return fail(format!("{t}: {}", oracle::refusal(&g.id, &approvals.pending(&g, &s.task))));
                }
                Some(s) if !board::claimable_by(&b, s, &session, &drone_for(&hs), &node(&here.home), now) => {
                    return fail(format!("{t} is not open to {} ({}). {NOT_YOURS}", short(&session), held_by(s, now)));
                }
                Some(s) => {
                    let events = std::cell::OnceCell::new();
                    if let Some(n) =
                        absent_from(&here.home, &g.id, &b, || events.get_or_init(|| board::read(&tasks_path(&here.home, &g.id))).as_slice(), s, &here.cwd)
                    {
                        return fail(elsewhere(t, &n));
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
        match claim(&here.home, &g, &session, &pick.task.clone(), &here.cwd, now, verdict::SNAPSHOT_TIMEOUT) {
            Ok(won) => {
                count(&here.home, stats::HIVE_CLAIM, &session, &g.id, &id, &repo);
                hook::append(&session, &format!("{TASK_MARKER}{id}@{}", won.live(learn::now_ms()).map_or(0, |l| l.2)));
                println!("claimed {id} for {}. {}", short(&session), describe(&g, &won, &session, &here.cwd));
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
            Err(Refused::NotHere(why) | Refused::Io(why) | Refused::Reset(why)) => return fail(why),
        }
    }
    3
}

/// The live claim of the session `named`: (session, goal, task).
fn held(here: &Here, named: Option<&str>) -> Result<(String, Goal, TaskState), String> {
    let (session, hs) = here.drone(named)?;
    let g = goal::load(&here.home, &hs.goal)?;
    let b = here.board(&g);
    let s = board::holding(&b, &session, here.now).cloned().ok_or_else(|| format!("{} holds no claim in goal {}", short(&session), g.id))?;
    Ok((session, g, s))
}

fn held_event(here: &Here, named: Option<&str>, ev: Ev, note: &str) -> i32 {
    match held(here, named) {
        Ok((session, g, s)) => match write(&here.home, &g.id, vec![Event::new(ev, &s.task.id, &session, here.now).with_note(note.trim())]) {
            Ok(()) => {
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
            let vid = match finish(&here.home, &g, &s, &session, dir, note, verdict::SNAPSHOT_TIMEOUT) {
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
            println!("{}", r.summary(&s.task.id));
            0
        }
        Err(e) => fail(e),
    }
}

/// Whether this command runs for a drone ([`drone`]).
fn drone_here(here: &Here) -> bool {
    drone(&here.home, &here.cwd)
}

/// Whether a process in `cwd` runs for a drone: launched into a goal (`BORG_HIVE`, `BORG_HIVE_DRONE`),
/// or in a worktree `borg hive run` made and has not left (a harness that drops the env). What the
/// engineer-only commands and the queen's questions about a hive's plan and tasks are never run,
/// answered or delivered by (queen.rs).
pub fn drone(home: &Path, cwd: &str) -> bool {
    let set = |k: &str| std::env::var(k).is_ok_and(|v| !v.trim().is_empty());
    set(ENV) || set(DRONE_ENV) || joined(home, cwd).is_some_and(|j| j.by == "run")
}

/// `borg hive approve <goal>`: the engineer approves the checks of goal `id` that other nodes wrote (the
/// synced goal's oracle, a remote task's own), printing each one, so this node's drones may take their
/// tasks and run them (oracle.rs `Approvals`). Never a drone's command: approving is the engineer's
/// consent to run another machine's shell commands.
fn approve_cmd(here: &Here, id: &str) -> i32 {
    if drone_here(here) {
        return fail("checks are approved by the engineer, never by a drone of a hive goal");
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

/// `borg hive score <goal>`: the hidden oracle scores every judged attempt not scored yet. An engineer's
/// command: a drone that learned the held-out checks' results could optimise against them.
fn score_cmd(here: &Here, id: &str) -> i32 {
    if drone_here(here) {
        return fail("the hidden oracle is scored by the engineer, never by a drone of a hive goal");
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
        "{t} starts from the integrated tree {from}: claim it in a checkout of it (`git worktree add <dir> {from}`, then `borg hive claim {t}` there), or a drone of `borg hive run` takes it in its own worktree"
    )
}

/// `borg hive integrate <goal>`: what `borg hive run` does once every task is verified (integrate.rs),
/// from the goal's repo. Exit 0 when the combined tree passed, 1 when it failed (a fix task is open), is
/// blocked, or needs another node's commits (nothing written), 2 when no integration is due. The
/// engineer's: a drone's session never integrates.
fn integrate_cmd(here: &Here, id: &str) -> i32 {
    if drone_here(here) {
        return fail(INTEGRATE_REFUSED);
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
        None if let Some(why) = integrate::elsewhere(&here.home, &g, &repo, here.now) => fail(why),
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

/// `borg hive ask|tell "text" [--to TASK|DRONE|SESSION] [--re ID]`: a message to the goal's drones
/// (message.rs). A `tell` that answers (`--re`) without `--to` goes to the asker's session.
fn message_cmd(here: &Here, named: Option<&str>, kind: &str, text: &str, to: Option<&str>, re: Option<&str>) -> i32 {
    let (session, hs) = match here.drone(named) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    let g = match goal::load(&here.home, &hs.goal) {
        Ok(g) => g,
        Err(e) => return fail(e),
    };
    let path = messages_path(&here.home, &g.id);
    let to = match (to, re) {
        (Some(t), _) => t.to_string(),
        (None, Some(r)) => match message::read(&path).into_iter().find(|m| m.id == r) {
            Some(m) => m.by,
            None => return fail(format!("no message {r} in goal {}", g.id)),
        },
        (None, None) => String::new(),
    };
    let b = here.board(&g);
    let task = board::holding(&b, &session, here.now).map(|s| s.task.id.clone()).unwrap_or_default();
    let m = message::Message::new(kind, &session, &drone_for(&hs), &node(&here.home), &task, &to, re.map(String::from), here.now, text);
    match message::send(&here.home, &g.id, m) {
        Ok(m) => {
            stats::count(&here.home, stats::HIVE_MESSAGE, &session, |ev| {
                ev.why = g.id.clone();
                ev.repo = queen::repo_name(&here.cwd);
                ev.text = stats::excerpt(&format!("{kind} {}: {}", if m.to.is_empty() { "all" } else { &m.to }, m.text), 160);
            });
            println!(
                "{} {} sent to {} in goal {}",
                if kind == message::ASK { "question" } else { "message" },
                m.id,
                if m.to.is_empty() { "every drone" } else { &m.to },
                g.id
            );
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
    let task = board::holding(&b, &session, here.now).map(|s| s.task.id.clone()).unwrap_or_default();
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
        assert!(matches!(claim(&home, &g, &runner_s, &fix, &r, 2, verdict::SNAPSHOT_TIMEOUT), Err(Refused::NotHere(_))));
        assert!(matches!(claim(&home, &g, &own_s, &fix, &w, 2, verdict::SNAPSHOT_TIMEOUT), Err(Refused::NotHere(_))));
        assert_eq!(board::read(&tasks_path(&home, "g")).len(), 4);
        let b = board::fold(&board::read(&tasks_path(&home, "g")));
        assert!(offer(&home, &g, &b, &own_s, "app", &w, 2).starts_with("No claim, and no task"), "not offered where it cannot be claimed");
        assert!(offer(&home, &g, &b, &runner_s, "app", &w, 2).contains("integrate.1.fix"));
        // The runner drone in its own worktree: claimed from `from`, the worktree reset to it.
        std::fs::write(wt.join("README"), "stray edit\n").unwrap();
        std::fs::write(wt.join("stray.txt"), "x\n").unwrap();
        let now = learn::now_ms();
        let won = claim(&home, &g, &runner_s, &fix, &w, now, verdict::SNAPSHOT_TIMEOUT).expect("claimed");
        assert!(won.live(now + 1).is_some_and(|(by, _, _)| by == runner_s));
        assert_eq!(git_ok(&wt, &["rev-parse", "HEAD"]), m);
        assert_eq!(std::fs::read_to_string(wt.join("README")).unwrap(), "base\n");
        assert!(wt.join("m.txt").exists() && !wt.join("stray.txt").exists());
        let c = board::read(&tasks_path(&home, "g")).into_iter().find(|e| e.ev == Ev::Claim && e.task == "integrate.1.fix").unwrap();
        assert_eq!(c.base, m, "the diff is judged from the combined tree");
        let kept = c.note.strip_prefix("worktree before the reset: ").expect("the discarded tree is kept");
        assert_eq!(git_ok(&repo, &["show", &format!("{kept}:stray.txt")]), "x");
        assert!(describe(&g, &won, &runner_s, &w).contains(&format!("reset to the integrated tree {m}")));
        // Any session in a checkout of `from` (a goal worked by joined sessions): offered and claimed
        // there, never reset, its base a snapshot of that checkout.
        board::append(&tasks_path(&home, "g"), &[Event::new(Ev::Released, "integrate.1.fix", &runner_s, learn::now_ms())]).unwrap();
        let co = root.join("fix");
        git_ok(&repo, &["worktree", "add", "-q", "--detach", co.to_str().unwrap(), &m]);
        std::fs::write(co.join("notes.txt"), "mine\n").unwrap();
        let c = co.display().to_string();
        let now = learn::now_ms();
        let b = board::fold(&board::read(&tasks_path(&home, "g")));
        assert!(offer(&home, &g, &b, &own_s, "app", &c, now).contains("integrate.1.fix"));
        let won = claim(&home, &g, &own_s, &fix, &c, now, verdict::SNAPSHOT_TIMEOUT).expect("claimed in a checkout of from");
        assert!(co.join("notes.txt").exists(), "no reset");
        let c = board::read(&tasks_path(&home, "g")).into_iter().rfind(|e| e.ev == Ev::Claim).unwrap();
        assert_eq!(git_ok(&repo, &["rev-parse", &format!("{}^", c.base)]), m, "a snapshot on the combined tree");
        assert!(describe(&g, &won, &own_s, &co.display().to_string()).contains(&format!("You work on the integrated tree {m}")));
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
        assert_eq!(b.lost, vec![("s".to_string(), "t1".to_string(), 10)], "its latest claim, t1's, lost");
        assert!(lost_claim(&b, &lines, "s", "", 11).is_none(), "it holds t2: its edits run and its tree stays");
        assert!(lost_claim(&b, &lines, "s", "", 11 + LEASE_MS).is_some(), "once t2's lease is gone, t1's loss counts");
        // A renewal of the held task is no loss either.
        lines.push(Event::new(Ev::Claim, "t2", "s", 12));
        assert!(lost_claim(&board::fold(&lines), &lines, "s", "", 13).is_none());
    }

    #[test]
    fn a_lost_claim_takes_its_deps_out_even_when_its_time_is_up() {
        let root = std::env::temp_dir().join(format!("borg-hive-take-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        git_ok(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        git_ok(&root, &["add", "-A"]);
        git_ok(&root, &["commit", "-qm", "base"]);
        let base = verdict::snapshot(&root, verdict::SNAPSHOT_TIMEOUT).unwrap();
        std::fs::write(root.join("a.txt"), "dep\n").unwrap();
        let head = verdict::snapshot(&root, verdict::SNAPSHOT_TIMEOUT).unwrap();
        git_ok(&root, &["checkout", "-q", "--", "a.txt"]);
        assert!(verdict::bring_in(&root, &base, &head, verdict::SNAPSHOT_TIMEOUT).unwrap());
        let t0 = Instant::now();
        take_out_all(&root.display().to_string(), &[(base, head)], Duration::ZERO);
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "a\n", "the undo gets its floor");
        assert!(t0.elapsed() < MIN_ORACLE + Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(root);
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
