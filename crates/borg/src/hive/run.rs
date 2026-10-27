//! `borg hive run <goal> --drones N`: N headless drones on one board, each an iteration loop in a git
//! worktree of its own (`<repo>/.worktrees/hive-<goal>-<n>` on branch `hive/<goal>/<n>`, kept with its
//! commits across runs), each with an equal share of what is left of the goal's budget (all runs'
//! spend counted). One run of a goal at a time (a lock on `run.lock`). A drone is a sequence of headless sessions of the harness
//! (`Harness::headless`), each launched with `BORG_HIVE=<goal>` and `BORG_HIVE_DRONE=d<n>`: the hooks
//! join it to the goal, show it its claim and the board, and gate its stop on the oracle (mod.rs).
//!
//! Before the first drone starts, the engineer approves the goal's plan (plan.rs `gate`): an intent card
//! per task in one queen question, waited for under the run lock, so a "no" stops the run with no
//! worktree made and nothing claimed. `--yes` approves it at the command line (borg-eval's runs).
//!
//! Once the worktrees are made, the runner joins its node to the goal on the board (mod.rs `announce`)
//! and, with mesh peers reachable, waits for one sync round with each, a few seconds at most: the
//! drones of every node then start knowing each node's share (board.rs `next_for`), instead of racing
//! for one task until their first lines sync. On one node there is no wait. While its drones run it
//! beats on the board every `BEAT_MS` (`membership`), and it leaves the goal when they ended: a peer
//! tells a node gone from one alive in seconds, apart from the 30-min work lease (phase f F4).
//!
//! Once every task is verified, the drone thread that sees it first integrates (integrate.rs): it
//! claims the board's integrate task, merges the accepted attempts and runs the oracle on the combined
//! tree, in process and at no model cost, while the other drones wait on its lease. The goal is
//! finished only when that passes; a failure opens a fix task the drones take like any work.
//!
//! Between iterations the runner reads the board once (`board::fold`) and decides ([`step`]): the
//! board is finished on a stable prefix (every participant said its board was finished at that
//! integration, with every line before that here: `Board::finished_for_good`), the share is spent or the goal's budget is (every node's spend, [`ledger`]), a session ended well without naming its cost or at $0 (fail
//! closed: an unmetered harness, or one on a subscription, gets one iteration), two iterations in a row
//! failed or timed out, two made no board progress, or, with nothing open to the drone, a task no
//! running drone of the goal may take while nothing else on the board can move (`board::stuck`,
//! `board::moving`, said on the board and in the end report, exit 1). Nothing else ends a drone (phase f F6): with nothing open it waits on the board however long the
//! goal is unfinished, since a peer's line may open work at any time; quiet is no termination. An iteration
//! cut off at `--iter-timeout` without naming its cost is charged an estimate (`Spend::est`: the
//! drone's dearest iteration so far, else the rest of its share). A harness that stops itself at a
//! budget (`Caps::headless_budget`) gets the drone's dollars left; any other overshoots by at most one
//! iteration, which `--iter-timeout` bounds. One that failed without naming its cost is charged the
//! same estimate: it may have spent up to the cap. Whatever an ended iteration's session still holds is
//! released: the session is gone, and its lease would otherwise keep the task for `LEASE_MS`. An
//! iteration's sessions are the one the runner named, the one the harness reported, and every one
//! whose claim carries the drone since the iteration started: omp and opencode keep ids of their own
//! and report none.
//!
//! A run stopped by Ctrl-C (SIGINT) or a closed terminal (SIGHUP) says on the board that its unsettled
//! sessions ended and that its node left the goal, then exits (`trap`, `stop`); one killed otherwise
//! writes nothing more, and its peers suspect it within `SUSPECT_MS`. Either way its sessions die with
//! it (runner.rs `run_bounded`), and the next run of the goal makes good what it did not write under the
//! lock, before it counts what is spent ([`recover`]).
//!
//! Budget across nodes: `spend.jsonl` syncs over the mesh (mod.rs `SYNCED`), each line naming its node,
//! so every runner of a goal, on any node, reads the goal's one total ([`ledger`]): a run's share is
//! what is left of the budget after every node's spend, split over its drones, and every drone stops
//! once the goal's total reaches the budget, whoever spent it. Runs started at once on two nodes each
//! split the whole budget (neither saw the other's spend yet); the stop rule bounds what they spend
//! together: at most the budget plus the iterations in flight when the total reached it, one per drone
//! (each at most its drone's dollars left, the harness's own cap when it has one, else `--iter-timeout`),
//! provided each node sees the others' lines before its drones start another iteration (the mesh syncs
//! more often than an iteration lasts). Each sync period a node lags behind lets each of its drones
//! start at most one more iteration. A drone name (`d1`) is per node: a drone's own tally and its
//! earlier iterations are its node's lines only.
//!
//! Lines, under `<store>/hive/<goal>/`: `spend.jsonl` (one per iteration, also what a re-run counts
//! as spent; synced), `runs.jsonl` (one per run), `joins.jsonl` (each iteration's session and worktree, and a
//! leave of each worktree when the run ends: a later session there, an engineer's review, joins
//! nothing). `logs/<drone>-<iter>.out` keeps each iteration's output (at most `LOG_CAP`): local
//! only, never synced. When the drones end, the goal's hidden oracle scores every attempt they had
//! judged ([`after_run`], hidden.rs) in scratch checkouts under the goal's directory: no oracle file
//! is ever put into a drone's worktree.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{Board, Commits, Ev, Goal, Join, TaskId, board, goal};
use crate::harness::{self, Harness, Headless};
use crate::{learn, queen, runner};

/// At most this many drones (`borg top` names drones One to Twenty).
pub const MAX_DRONES: usize = 20;
/// One iteration's cap when `--iter-timeout` names none.
pub const DEFAULT_ITER_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Guards, not tuned defaults: failed iterations and iterations without board progress in a row. An idle
/// drone looks at the board again after `IDLE_WAIT` at the latest.
pub(crate) const IDLE_WAIT: Duration = Duration::from_secs(3);
/// How often an idle drone looks at the board's file within `IDLE_WAIT` (`idle_wait`): as often as the
/// mesh daemon looks at the hive's logs (borg-mesh daemon.rs `hive_poll`), so a peer's line that lands
/// wakes it within a tick, not a blind wait.
const IDLE_TICK: Duration = Duration::from_millis(25);
const MAX_ERRORS: u32 = 2;
const MAX_STALLS: u32 = 2;
const GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// What an iteration's log keeps of its output.
const LOG_CAP: usize = 1 << 20;

/// What each drone is told (the hooks add its claim and the exact commands); the last sentence is the
/// inoculation framing (docs/design/hive.md, L8), and names held-out checks only for a goal that
/// has them ({decides}, as mod.rs `inoculation`).
const PROMPT: &str = "You are drone {drone} of hive goal {goal}. Work in this worktree only. \
borg's hooks show your claim or the next open task with the exact commands: claim one task \
(the board: `borg hive board {goal}`), publish every interface you create or change as a contract, \
and finish when its oracle passes (ending your turn runs it). A verify task's work is the \
commits its claim names, in this repo whatever node did it, not your tree. Gaming the oracle (editing tests, CI or \
oracle files) is visible, allowed to happen and useless: {decides}.";
const DECIDES_HIDDEN: &str = "held-out checks you cannot see decide";
const DECIDES: &str = "the verifier sees every such change and the task counts as failed";

/// Each iteration's cost, every node's (mod.rs `SYNCED`).
pub const SPEND_FILE: &str = "spend.jsonl";

pub fn spend_path(home: &Path, goal: &str) -> PathBuf {
    super::dir(home, goal).join(SPEND_FILE)
}

pub fn runs_path(home: &Path, goal: &str) -> PathBuf {
    super::dir(home, goal).join("runs.jsonl")
}

fn logs_dir(home: &Path, goal: &str) -> PathBuf {
    super::dir(home, goal).join("logs")
}

/// One iteration of one drone: what it cost (None: the harness reported nothing) and how it ended
/// (`ok`, `timeout`, `error`, or `killed`: its run died, written by the next run, [`recover`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Spend {
    pub v: u8,
    pub ms: i64,
    pub drone: String,
    /// The node whose runner wrote it (mod.rs `node`): `d1` on two nodes is two drones. Empty on a line
    /// from before lines named their node, which is this node's: spend did not sync then, and such a
    /// line never leaves the node (borg-mesh lines.rs `local`: it is never stamped, so never offered).
    #[serde(default)]
    pub node: String,
    pub iter: u32,
    pub harness: String,
    pub usd: Option<f64>,
    /// What a timed-out, failed or killed iteration that named no cost is charged instead (see the
    /// module doc).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub est: Option<f64>,
    #[serde(default)]
    pub session: String,
    pub exit: String,
}

impl Spend {
    /// What the iteration counts against the budget: its cost, else its estimate.
    pub fn charged(&self) -> f64 {
        self.usd.or(self.est).unwrap_or(0.0).max(0.0)
    }
}

impl Spend {
    /// The node that spent it: its `node`, else `here` (a line from before lines named their node,
    /// which stays on the node that wrote it).
    pub fn owner<'a>(&'a self, here: &'a str) -> &'a str {
        if self.node.is_empty() { here } else { &self.node }
    }
}

/// A goal's spend over every node (spend.jsonl syncs): the fold every runner's share and stop rule read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ledger {
    /// Every node's charges (`Spend::charged`).
    pub total: f64,
    /// By node.
    pub nodes: BTreeMap<String, f64>,
}

/// The ledger of `lines` read on node `here`: each distinct line once, summed in one fixed order, so
/// the fold is the same whatever order the mesh delivered the lines in, and a line delivered twice
/// counts once.
pub fn ledger(lines: &[Spend], here: &str) -> Ledger {
    let distinct: BTreeMap<String, &Spend> = lines.iter().map(|s| (serde_json::to_string(s).unwrap_or_default(), s)).collect();
    let mut l = Ledger::default();
    for s in distinct.values() {
        l.total += s.charged();
        *l.nodes.entry(s.owner(here).to_string()).or_default() += s.charged();
    }
    l
}

/// The ledger of `goal` in `home`.
pub fn goal_spent(home: &Path, goal: &str) -> Ledger {
    ledger(&super::read_lines(&spend_path(home, goal)), &super::node(home))
}

/// One `borg hive run`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunLine {
    pub v: u8,
    pub ms: i64,
    pub drones: usize,
    pub harness: String,
    pub share_usd: f64,
    pub base: String,
    /// The mesh peers its start waited on (mod.rs `announce`; 0: none reachable, no wait) and for how
    /// long.
    #[serde(default)]
    pub peers: usize,
    #[serde(default)]
    pub waited_ms: u64,
}

/// Drone `n`'s worktree: one path component under `.worktrees/`, so borg-trace's `repo_key` folds it
/// back onto the repo (a nested path would be a repo of its own) and the collective routes it the same.
pub fn worktree(repo: &Path, goal: &str, n: usize) -> PathBuf {
    repo.join(".worktrees").join(format!("hive-{goal}-{n}"))
}

pub fn branch(goal: &str, n: usize) -> String {
    format!("hive/{goal}/{n}")
}

pub fn drone_name(n: usize) -> String {
    format!("d{n}")
}

/// The prompt of `drone`'s every iteration of `g`.
pub fn prompt(g: &Goal, drone: &str) -> String {
    let decides = if goal::has_hidden(g) { DECIDES_HIDDEN } else { DECIDES };
    PROMPT.replace("{goal}", &g.id).replace("{drone}", drone).replace("{decides}", decides)
}

/// A drone's tally between iterations.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tally {
    pub spent: f64,
    pub errors: u32,
    pub stalls: u32,
    /// An iteration that ended well reported no cost, or $0.
    pub unmetered: bool,
    /// The dearest iteration's cost so far (a timed-out one's estimate).
    pub dearest: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Start an iteration.
    Go,
    /// Nothing open to the drone: wait for the board to change, and decide again.
    Wait,
    End(&'static str),
}

/// What the board says of the goal's end to a drone (phase f F6, [`step`]): a goal ends by detection on
/// what every participant said, never by a drone's own quiet (Dijkstra & Scholten 1980; Safra EWD998).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    /// Unfinished, nothing stuck: the drone works, or waits however long.
    Open,
    /// Finished, and a participant may still change it (`Board::settling`): the drone waits, spending
    /// nothing (nothing is open on a finished board).
    Settling,
    /// Finished on a stable prefix (`Board::finished_for_good`).
    Finished,
    /// Tasks no running drone of the goal may take, with why (`board::stuck`), and nothing else on the
    /// board can move (`board::moving`): no held task, none another drone may still take.
    Stuck(Vec<(TaskId, String)>),
}

impl Ending {
    /// What `b` says to a drone of `here` at `now`, among the tasks `allowed` lets this node claim, with
    /// the commits it lacks (`lacks`, mod.rs `lacking`; phase f F7): a task whose commits are on their way
    /// counts as one this node may take (it waits for them, `takes`), one whose commits cannot come as
    /// one it may not, stuck with why they cannot (`commits unavailable: …`) when no other node may.
    pub(super) fn of(b: &Board, here: &str, now: i64, allowed: impl Fn(&board::TaskState) -> bool, lacks: &BTreeMap<TaskId, Commits>) -> Ending {
        if b.finished_for_good(here, now) {
            return Ending::Finished;
        }
        if b.finished() {
            return Ending::Settling;
        }
        let mut stuck = board::stuck(b, here, now, takes(allowed, lacks));
        for (t, why) in &mut stuck {
            if let Some(Commits::Unavailable(w)) = lacks.get(t) {
                *why = w.clone();
            }
        }
        if stuck.is_empty() || board::moving(b, now, &stuck) { Ending::Open } else { Ending::Stuck(stuck) }
    }
}

/// The tasks a session of this node may go for (phase f F7): `allowed`, and its commits here (none in
/// `lacks`, mod.rs `lacking`). The runner's offer (`open_to`) and the session's pick (mod.rs `next_open`)
/// filter by the same answer, so no session starts for a task it would then refuse.
pub(super) fn offered<'a>(allowed: impl Fn(&board::TaskState) -> bool + 'a, lacks: &'a BTreeMap<TaskId, Commits>) -> impl Fn(&board::TaskState) -> bool + 'a {
    move |s| allowed(s) && !lacks.contains_key(&s.task.id)
}

/// The tasks this node may take for its declines and its stuck (phase f F7): `allowed`, unless no line
/// can bring its commits here (`Commits::Unavailable`). A task whose commits are on their way is one it
/// takes once they are here: it is neither declined nor stuck, and its drones wait.
pub(super) fn takes<'a>(allowed: impl Fn(&board::TaskState) -> bool + 'a, lacks: &'a BTreeMap<TaskId, Commits>) -> impl Fn(&board::TaskState) -> bool + 'a {
    move |s| allowed(s) && !matches!(lacks.get(&s.task.id), Some(Commits::Unavailable(_)))
}

/// What a drone does next: the goal's end, the stop reasons in order, then whether a task is open to it,
/// and with none whether a task is stuck. `left` is what is left of the goal's budget after every node's
/// spend ([`ledger`]). A drone ends only on a finish for good, its budget, a fault of its own (errors,
/// stalls, no cost reported) or a stuck: never on nothing to do, however long (phase f F6).
pub fn step(t: &Tally, share: f64, left: f64, end: &Ending, open: bool) -> Next {
    if *end == Ending::Finished {
        Next::End("goal finished")
    } else if *end == Ending::Settling {
        Next::Wait
    } else if t.spent >= share || left <= 0.0 {
        Next::End("budget")
    } else if t.unmetered {
        Next::End("cost not reported")
    } else if t.errors >= MAX_ERRORS {
        Next::End("errors")
    } else if t.stalls >= MAX_STALLS {
        Next::End("stalled")
    } else if open {
        Next::Go
    } else if matches!(end, Ending::Stuck(_)) {
        Next::End("stuck")
    } else {
        Next::Wait
    }
}

/// How one drone ended.
#[derive(Debug, Clone)]
pub struct DroneEnd {
    pub drone: String,
    pub worktree: PathBuf,
    pub spent: f64,
    pub iters: u32,
    pub done: Vec<String>,
    pub reason: &'static str,
    /// What it ended stuck on (`Ending::Stuck`).
    pub stuck: Vec<(TaskId, String)>,
}

pub struct Opts<'a> {
    pub goal: &'a str,
    pub drones: usize,
    pub harness: &'static dyn Harness,
    pub model: Option<&'a str>,
    pub base: &'a str,
    pub iter_timeout: Duration,
    pub dry_run: bool,
    /// The engineer approves the plan on the command line (`--yes`): no question, no wait (plan.rs).
    pub yes: bool,
}

/// One run: what every drone shares.
struct Ctx<'a> {
    home: &'a Path,
    g: &'a Goal,
    h: &'static dyn Harness,
    /// The repo (its git toplevel), where the snapshots are; `repo` is its name.
    root: &'a Path,
    repo: &'a str,
    model: Option<&'a str>,
    share: f64,
    iter_timeout: Duration,
    /// spend.jsonl as the run found it.
    before: &'a [Spend],
    /// This node's id (mod.rs `node`).
    here: &'a str,
    /// The sessions running now: each drone's, with the board's clock at its start (mod.rs `clock`).
    sessions: &'a Sessions,
}

type Sessions = std::sync::Mutex<BTreeMap<String, i64>>;

/// The signal that stops the run (SIGINT, SIGHUP), 0 while none came (`trap`).
static STOP: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

extern "C" fn on_stop(sig: libc::c_int) {
    STOP.store(sig, std::sync::atomic::Ordering::SeqCst);
}

/// From here until `untrap` SIGINT (Ctrl-C) and SIGHUP (a closed terminal) stop the run as an end, not
/// a death: the handler only notes the signal, and the run's membership thread (`membership`) says on
/// the board that the node leaves and its unsettled sessions ended (`stop`), then exits. Before, such a
/// run wrote nothing more, and its peers waited out its leases.
fn trap() {
    let handler = on_stop as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGHUP, handler);
    }
}

/// The run's drones ended, the membership thread with them, and the node left the goal: SIGINT and
/// SIGHUP act as they do by default again, so a Ctrl-C ends the hidden oracle's scoring (`after_run`) or
/// the summary at once, as before `trap`, and a closed terminal ends the run instead of leaving it
/// printing to nothing. A signal noted since the membership thread's last look (`STOP`) ends it here:
/// nothing is left to write on the board.
fn untrap(goal: &str) {
    // SAFETY: restoring the default disposition is async-signal-safe and takes no pointer.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::signal(libc::SIGHUP, libc::SIG_DFL);
    }
    let sig = STOP.load(std::sync::atomic::Ordering::SeqCst);
    if sig != 0 {
        let name = if sig == libc::SIGHUP { "SIGHUP" } else { "SIGINT" };
        println!("hive run {goal}: stopped by {name}; this node left the goal");
        std::process::exit(128 + sig);
    }
}

/// While the drones run (until `done`): this node's goal heartbeat every `BEAT_MS` (mod.rs `member`,
/// phase f F4: its peers tell it from a node gone in `SUSPECT_MS`, seconds, apart from the work lease),
/// one at once when a claim of another node arrived that no line of this node is stamped above yet
/// (`board::to_ack`, phase f F5: the beat moves this node's watermark past it, so that claim is final a
/// line's way later instead of at the next beat), and a look every `IDLE_TICK` for a stop signal
/// (`trap`), which ends the run (`stop`). The board is read again only when its file grew.
fn membership(home: &Path, goal: &str, sessions: &Sessions, done: &std::sync::atomic::AtomicBool) {
    let beat = Duration::from_millis(super::BEAT_MS as u64);
    let mut next = Instant::now() + beat;
    let (path, here) = (super::tasks_path(home, goal), super::node(home));
    let (mut len, mut acked) = (0, i64::MIN);
    while !done.load(std::sync::atomic::Ordering::SeqCst) {
        let sig = STOP.load(std::sync::atomic::Ordering::SeqCst);
        if sig != 0 {
            stop(home, goal, sessions, sig);
        }
        let grew = std::fs::metadata(&path).map_or(len, |m| m.len());
        let ack = (grew != len).then(|| board::to_ack(&board::read(&path), &here)).flatten().filter(|m| *m > acked);
        len = grew;
        if Instant::now() >= next || ack.is_some() {
            let _ = super::member(home, goal, board::BEAT);
            next = Instant::now() + beat;
            acked = acked.max(ack.unwrap_or(i64::MIN));
        }
        std::thread::sleep(IDLE_TICK);
    }
}

/// A run stopped by `sig` (`trap`): each running session that has not settled on the board (claimed, or
/// said that it ended) says that it ended, so no peer leaves a task to it (board.rs `starting`), and the
/// node leaves the goal (`board::LEAVE`), so no peer waits on its drones' leases (`Local::gone`); then
/// the process exits, its sessions with it (runner.rs `run_bounded`). What else the run did not write
/// (its iterations' spend, its releases) the next run makes good (`recover`).
fn stop(home: &Path, goal: &str, sessions: &Sessions, sig: i32) -> ! {
    let (b, here) = (fold(home, goal), super::node(home));
    let running = sessions.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for (drone, at) in running {
        if !super::settled_since(&b, &here, &drone, at) {
            let _ = super::session(home, goal, &drone, false, learn::now_ms());
        }
    }
    let _ = super::member(home, goal, board::LEAVE);
    let name = if sig == libc::SIGHUP { "SIGHUP" } else { "SIGINT" };
    println!("hive run {goal}: stopped by {name}; this node left the goal");
    std::process::exit(128 + sig);
}

/// This node in the goal from the run's announcement (mod.rs `announce`) until it drops: then its leave
/// (`board::LEAVE`), on every way out of the run but a signal's, which `stop` writes itself.
struct Joined<'a> {
    home: &'a Path,
    goal: &'a str,
}

impl Drop for Joined<'_> {
    fn drop(&mut self) {
        let _ = super::member(self.home, self.goal, board::LEAVE);
    }
}

fn fold(home: &Path, goal: &str) -> Board {
    board::fold_on(&board::read(&super::tasks_path(home, goal)), &super::node(home))
}

/// What the board offers a drone at one moment (`open_to`).
#[derive(Debug, Clone, Default)]
pub(super) struct Offer {
    /// The task open to it and when it became claimable (board.rs `claimable_since`): one of its new
    /// sessions could claim it and no idle drone of its node before it takes it (board.rs `free_for`).
    pub(super) task: Option<(TaskId, i64)>,
    /// Every task its session could go for (board.rs `open_for`, `next_for`'s candidates: eligible for it,
    /// allowed, not left to another node's idle drones), each with when it became claimable.
    pub(super) open: BTreeMap<TaskId, i64>,
}

impl Offer {
    /// The offer of `b` to `drone` of `node` in `repo` at `now`, among the tasks `allowed` lets it claim.
    pub(super) fn of(b: &Board, drone: &str, node: &str, repo: &str, now: i64, allowed: impl Fn(&board::TaskState) -> bool) -> Offer {
        let since = |s: &board::TaskState| (s.task.id.clone(), board::claimable_since(b, s));
        let who = board::Actor::new(node, drone, "");
        let open = b.tasks.values().filter(|s| board::open_for(b, s, who, now, &allowed));
        Offer { task: board::free_for(b, who, repo, now, &allowed).map(since), open: open.map(since).collect() }
    }
}

/// What the board offers `drone` of this node (`home`) at `now` (`Offer`), its checks approved here
/// (`allowed`) and its commits in this repo (`offered`, `lacks`): the task open to it, if one is. With
/// none it waits (`Next::Wait`); the stall count (`stalls`) reads the same answer.
fn open_to(home: &Path, g: &Goal, b: &Board, drone: &str, repo: &str, lacks: &BTreeMap<TaskId, Commits>, now: i64) -> Offer {
    Offer::of(b, drone, &super::node(home), repo, now, offered(allowed(home, g, repo), lacks))
}

/// The tasks this node may claim in `repo` (mod.rs `may_claim`), their checks approved here (oracle.rs
/// `Approvals`).
fn allowed<'a>(home: &Path, g: &'a Goal, repo: &'a str) -> impl Fn(&board::TaskState) -> bool + 'a {
    let approvals = super::oracle::Approvals::load(home, &g.id);
    move |s: &board::TaskState| super::may_claim(g, &s.task, repo) && approvals.pending(g, &s.task).is_empty()
}

/// Runs `cmd`, a session of `drone` on `goal` for a task claimable since `since` (`open_to`), within
/// `timeout`. Another node leaves this node's task to its idle drones for `OWNER_GRACE_MS` after it
/// became claimable (board.rs `left_to_owner`): `IDLE_WAIT` for the runner to start a session, then a
/// line's way to the peers. A session that claims within that time needs no line of its own; one that
/// has not claimed by then says on the board that it started (`session`, stamped with its start), so
/// the peers keep leaving the task to it until it claims, and that it ended once it ends unsettled.
/// A line only then: each line starts sync rounds, and a claim that lands during one waits for the
/// next (mod.rs `heard_back`). `at` is the board's clock at the iteration's start (mod.rs `clock`).
fn run_session(home: &Path, goal: &str, drone: &str, cmd: Command, timeout: Duration, since: i64, at: i64) -> Option<runner::Bounded> {
    // Two clocks: the notice is timed on this node's (`since` is when this node saw the task become
    // claimable, board.rs `claimable_since`), what settled the session is read in board stamps from its
    // start on (`at`, `settled_since`), so a stamp ahead of the wall clock (a peer's clock ahead,
    // absorbed) never reads an earlier session's claim as this one's.
    let started = learn::now_ms();
    let notice = Duration::from_millis(u64::try_from(since.saturating_add(IDLE_WAIT.as_millis() as i64) - started).unwrap_or(0));
    let unsettled = || !super::settled_since(&fold(home, goal), &super::node(home), drone, at);
    let (ended, ending) = std::sync::mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let said = scope.spawn(move || {
            let timed_out = matches!(ending.recv_timeout(notice), Err(std::sync::mpsc::RecvTimeoutError::Timeout));
            timed_out && super::session(home, goal, drone, true, at).is_ok_and(|w| w)
        });
        let ran = runner::run_bounded(cmd, None, timeout);
        drop(ended);
        if said.join().unwrap_or(false) && unsettled() {
            let _ = super::session(home, goal, drone, false, learn::now_ms());
        }
        ran
    })
}

/// The stall count after an iteration, judged by what was open to its session: `start` is the board's
/// offer as the session started, `end` as it ended (`open_to`). One that wrote no board line is a
/// stall while the task open to the drone now was open to that session and has not become claimable
/// again since (whichever task it went for): a task that became claimable during the session (another
/// drone's done opens its verify, a reject reopens its work) was never that session's to take. One
/// beaten to its task (a race lost here or on another node) with nothing open after it is no stall: the
/// drone waits on the board (phase f F6). Before F6 that was a stall unless the work was in hand (board.rs
/// `in_hand`, held or left to another drone), a guard of the idle-poll end, which F6 removed with it.
/// Both times are one task's own stamps, so no clock is compared with another (a peer's ahead of this
/// one would otherwise count every empty session as no stall).
pub(super) fn stalls(n: u32, wrote_nothing: bool, start: &Offer, end: &Offer) -> u32 {
    let stalled = end.task.as_ref().is_some_and(|(id, since)| start.open.get(id).is_some_and(|at| since <= at));
    match (wrote_nothing, stalled) {
        (false, _) => 0,
        (true, false) => n,
        (true, true) => n + 1,
    }
}

/// A fresh UUID-shaped session id (Claude Code's `--session-id` takes only a UUID) of `drone` on `node`:
/// the node is hashed in, so two nodes' runners (one pid each in two containers) in one ms name two
/// sessions (phase f F8; the board compares sessions node for node all the same, board.rs `Actor`).
fn session_id(node: &str, goal: &str, drone: &str, iter: u32) -> String {
    let mut h = blake3::Hasher::new();
    for part in [node, goal, drone, &iter.to_string(), &learn::now_ms().to_string(), &std::process::id().to_string()] {
        h.update(part.as_bytes());
        h.update(b"\0");
    }
    let x = h.finalize().to_hex();
    format!("{}-{}-4{}-a{}-{}", &x[0..8], &x[8..12], &x[13..16], &x[17..20], &x[20..32])
}

/// git in `dir`, running no hook: a drone can plant one in the shared `.git/hooks` (`worktree add`
/// runs post-checkout), and the runner runs as the engineer, outside any iteration's cap (as
/// integrate.rs `git`).
pub(super) fn git(dir: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null"]).args(args).env("GIT_TERMINAL_PROMPT", "0");
    match runner::run_capped_status(cmd, None, timeout)? {
        (status, out) if status.success() => Some(String::from_utf8_lossy(&out).trim().to_string()),
        _ => None,
    }
}

/// The repo `dir` is in: its git toplevel, a worktree folded back onto its repo.
pub fn repo_of(dir: &Path) -> Option<PathBuf> {
    let top = git(dir, &["rev-parse", "--show-toplevel"], Duration::from_secs(5))?;
    Some(PathBuf::from(borg_trace::repo_key(&top)))
}

/// Whether `path` is a worktree's own top (not merely a directory inside the repo).
pub(super) fn is_worktree(path: &Path) -> bool {
    path.is_dir() && git(path, &["rev-parse", "--show-toplevel"], Duration::from_secs(5)).is_some_and(|t| Path::new(&t) == path)
}

/// Drone `n`'s worktree: reused when it exists already; else its branch checked out when that exists
/// (an earlier run's commits are the drone's work: never reset), else a new branch from `base`.
fn ensure_worktree(repo: &Path, goal: &str, n: usize, base: &str) -> Result<PathBuf, String> {
    let path = worktree(repo, goal, n);
    if is_worktree(&path) {
        return Ok(path);
    }
    let p = path.display().to_string();
    let b = branch(goal, n);
    let exists = git(repo, &["rev-parse", "--verify", "-q", &format!("refs/heads/{b}")], Duration::from_secs(5)).is_some();
    let args: Vec<&str> = if exists { vec!["worktree", "add", &p, &b] } else { vec!["worktree", "add", "-b", &b, &p, base] };
    git(repo, &args, GIT_TIMEOUT).map(|_| path).ok_or_else(|| format!("git {} failed", args.join(" ")))
}

/// Keeps `.worktrees/` out of the engineer's `git status`: one line in the repo's local
/// `info/exclude` (never committed), written once.
pub(super) fn exclude_worktrees(repo: &Path) -> Result<(), String> {
    let common = git(repo, &["rev-parse", "--git-common-dir"], Duration::from_secs(5)).ok_or("not a git repo")?;
    let common = if Path::new(&common).is_absolute() { PathBuf::from(common) } else { repo.join(common) };
    let path = common.join("info/exclude");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    if text.lines().any(|l| l.trim() == "/.worktrees/") {
        return Ok(());
    }
    let _ = std::fs::create_dir_all(common.join("info"));
    let sep = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
    std::fs::write(&path, format!("{text}{sep}/.worktrees/\n")).map_err(|e| format!("{}: {e}", path.display()))
}

/// Of the iterations before this run (`lines`), the last number and the dearest one's charge of `drone`
/// of node `here` (a timed-out iteration's estimate starts from it). Another node's d1 is another drone.
fn spent_before(lines: &[Spend], here: &str, drone: &str) -> (u32, f64) {
    lines.iter().filter(|s| s.drone == drone && s.owner(here) == here).fold((0, 0.0), |(n, dear), s| (n.max(s.iter), dear.max(s.charged())))
}

/// Releases what `sessions` still hold on `b` at `now`, with `note`.
fn release_held(home: &Path, goal: &str, b: &Board, sessions: &[String], note: &str, now: i64) {
    let here = super::node(home);
    let held: Vec<board::Event> =
        sessions.iter().filter_map(|s| board::holding(b, &here, s, now).map(|t| board::Event::new(Ev::Released, &t.task.id, s, now).with_note(note))).collect();
    if !held.is_empty() {
        let _ = super::write(home, goal, held);
    }
}

/// The sessions of `drone`'s iteration that started at board clock `at` (mod.rs `clock`, a stamp at or
/// below every line this node writes after it and above every one before, never the wall clock, which
/// a stamp runs ahead of once this node heard a peer's clock ahead): those named (`known`, empty ones
/// skipped) and every one whose claim on this node (`home`) carries the drone since. A harness may
/// ignore the session id the runner chose and report none (omp, opencode); its hooks still claim as the
/// drone (`DRONE_ENV`). Another node's d1 is another drone: its sessions are never this runner's.
fn sessions_of(home: &Path, events: &[board::Event], drone: &str, at: i64, known: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = known.iter().filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
    for e in events.iter().filter(|e| e.ev == Ev::Claim && e.drone == drone && super::is_local(home, &e.node) && e.ms >= at) {
        if !out.contains(&e.by) {
            out.push(e.by.clone());
        }
    }
    out
}

/// An iteration's board progress: every line of its `sessions` but a heartbeat (a claim, a done, a
/// verdict) stamped from its start at board clock `at` on (`sessions_of`).
fn progress<'a>(events: &'a [board::Event], sessions: &[String], at: i64) -> Vec<&'a board::Event> {
    events.iter().filter(|e| e.ms >= at && sessions.contains(&e.by) && e.ev != Ev::Heartbeat).collect()
}

/// What an iteration that named no cost is charged: the drone's dearest iteration so far, else the
/// rest of its share (it may have spent up to the cap).
fn estimate(dearest: f64, share: f64, spent: f64) -> f64 {
    if dearest > 0.0 { dearest } else { (share - spent).max(0.0) }
}

/// Makes good what the goal's last run, killed mid-iteration, never wrote; under the run lock, so no
/// drone of the goal runs now. Each iteration it started (a `run` join naming its session) that no
/// spend line names is charged its estimate (`exit: "killed"`), and every claim a runner drone's
/// session still holds on this node is released: the session is dead, and its lease would keep the
/// task for `LEASE_MS`. Also one already read as gone (`Local::gone`: the run before left, and this
/// run's join ends it too): released explicitly, so a peer that reads no membership (a node from before
/// phase f F4) sees it free before its lease runs out, and no fold relies on membership for it. A claim
/// made on another node (a peer's d1) is never released.
fn recover(home: &Path, goal: &str, now: i64) {
    let here = super::node(home);
    let mut lines: Vec<Spend> = super::read_lines(&spend_path(home, goal));
    let run = super::read_lines::<RunLine>(&runs_path(home, goal)).pop();
    let (since, share) = run.map_or((i64::MAX, 0.0), |r| (r.ms, r.share_usd));
    let named: std::collections::HashSet<String> = lines.iter().flat_map(|s| s.session.split(',').map(String::from)).collect();
    let joins = super::read_lines::<Join>(&super::joins_path(home, goal));
    for j in joins.iter().filter(|j| j.by == "run" && j.ms >= since && !j.session.is_empty() && !named.contains(&j.session)) {
        let (iter, dearest) = spent_before(&lines, &here, &j.drone);
        let spent: f64 = lines.iter().filter(|s| s.drone == j.drone && s.owner(&here) == here && s.ms >= since).map(Spend::charged).sum();
        let line = Spend {
            v: 1,
            ms: now,
            drone: j.drone.clone(),
            node: here.clone(),
            iter: iter + 1,
            harness: j.harness.clone(),
            usd: None,
            est: Some(estimate(dearest, share, spent)),
            session: j.session.clone(),
            exit: "killed".into(),
        };
        if super::append_line(&spend_path(home, goal), &line).is_ok() {
            lines.push(line);
        }
    }
    let b = fold(home, goal);
    let runner = |d: &str| (1..=MAX_DRONES).any(|n| drone_name(n) == d);
    let held: Vec<board::Event> = b
        .tasks
        .values()
        .filter_map(|s| match &s.status {
            super::Status::Claimed { by, node, .. } if super::is_local(home, node) && b.drone(node, by).is_some_and(runner) => {
                Some(board::Event::new(Ev::Released, &s.task.id, by, now).with_note("its run ended"))
            }
            _ => None,
        })
        .collect();
    if !held.is_empty() {
        let _ = super::write(home, goal, held);
    }
}

/// The board file's length and mtime (`idle_wait`), None while it does not exist.
pub(super) type Mark = Option<(u64, Option<std::time::SystemTime>)>;

pub(super) fn mark(path: &Path) -> Mark {
    std::fs::metadata(path).ok().map(|m| (m.len(), m.modified().ok()))
}

/// Waits until the board at `path` says something else than `b`, its fold when its file was `seen`, at
/// most `cap`, looking at the file every `IDLE_TICK` and folding it only once that changed: whether it
/// did. A line that changes no board ends no wait: a goal heartbeat (`board::BEAT`, phase f F4) moves
/// only this node's clock of its writer (`Board::nodes`), and with every peer's beat ending each idle
/// drone's wait as no idle poll, no idle drone would ever end.
fn idle_wait(path: &Path, seen: Mark, b: &Board, cap: Duration) -> bool {
    let t0 = Instant::now();
    let mut seen = seen;
    loop {
        let now = mark(path);
        if now != seen {
            if board::fold(&board::read(path)) != *b {
                return true;
            }
            seen = now;
        }
        let left = cap.saturating_sub(t0.elapsed());
        if left.is_zero() {
            return false;
        }
        std::thread::sleep(IDLE_TICK.min(left));
    }
}

/// Up to `LOG_CAP` of an iteration's output, in `logs/<drone>-<iter>.out`.
fn log(home: &Path, goal: &str, drone: &str, iter: u32, out: &[u8]) {
    let dir = logs_dir(home, goal);
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(dir.join(format!("{drone}-{iter}.out")), &out[..out.len().min(LOG_CAP)]);
    }
}

/// One drone's iterations, until [`step`] ends them.
fn drone(ctx: &Ctx, n: usize, wt: &Path) -> DroneEnd {
    let name = drone_name(n);
    let goal = ctx.g.id.as_str();
    let (mut iter, dearest) = spent_before(ctx.before, ctx.here, &name);
    let mut t = Tally { dearest, ..Tally::default() };
    let mut end = DroneEnd { drone: name.clone(), worktree: wt.to_path_buf(), spent: 0.0, iters: 0, done: Vec::new(), reason: "", stuck: Vec::new() };
    let text = prompt(ctx.g, &name);
    let env = [(super::ENV, goal.to_string()), (super::DRONE_ENV, name.clone())];
    let tasks = super::tasks_path(ctx.home, goal);
    // What the drone last said it waits for (an integration's commits), said once.
    let mut waiting: Option<String> = None;
    loop {
        let now = learn::now_ms();
        let seen = mark(&tasks);
        let b = fold(ctx.home, goal);
        // A done of this node whose bundle line was never written (its session or stop hook cut during
        // `git bundle create`): written now, so no peer waits for it while this node runs (phase f F7).
        super::commits::republish(ctx.home, goal, &b, ctx.here, ctx.root);
        if let Some(o) = super::integrate::run(ctx.home, ctx.g, ctx.root, &name, now) {
            let head = o.head.as_deref().map_or("-", |h| &h[..h.len().min(12)]);
            match &o.verdict {
                Ok(()) => println!("{name}  {}  integrated {head}: the oracle passes", o.task),
                Err(why) => println!("{name}  {}  {head}: {why}", o.task),
            }
            continue;
        }
        // Accepted work whose commits this repo lacks (integrate.rs `gate`, phase f F7): the drone waits
        // while they may come or another node integrates; once no node can hold them together, the
        // integration is stuck, said on the board. A node that cannot get one of them says so first
        // (mod.rs `decline_integration`), so no peer waits for it to integrate.
        let gate = super::integrate::elsewhere(ctx.home, ctx.g, ctx.root, now);
        if let Some((id, _, true)) = &gate
            && !b.declines(ctx.here, id)
        {
            let _ = super::decline_integration(ctx.home, goal, id);
        }
        match gate.map(|(id, c, _)| (id, c)) {
            Some((id, Commits::Unavailable(why))) => {
                println!("{name}  {id}  {why}");
                let stuck = vec![(id, why)];
                let _ = super::stuck_lines(ctx.home, goal, &stuck);
                end.stuck = stuck;
                end.reason = "stuck";
                end.spent = t.spent;
                return end;
            }
            Some((id, Commits::Coming(why))) => {
                if waiting.as_deref() != Some(why.as_str()) {
                    println!("{name}  {id}  waits: {why}");
                    waiting = Some(why);
                }
                idle_wait(&tasks, seen, &b, IDLE_WAIT);
                continue;
            }
            None => waiting = None,
        }
        // The goal's budget left after every node's spend, read before each decision: a peer's lines
        // arrive between iterations.
        let left = ctx.g.budget_usd - goal_spent(ctx.home, goal).total;
        // The commits this node lacks, once per pass (mod.rs `lacking`): offer, declines and stuck read one answer.
        let lacks = super::lacking_here(ctx.home, goal, &b, ctx.root, now);
        let offer = open_to(ctx.home, ctx.g, &b, &name, ctx.repo, &lacks, now);
        // This node's board is finished: it says so (mod.rs `finish_line`), and its drones wait until
        // every participant did (phase f F6).
        if board::finish_due(&b, ctx.here).is_some() {
            let _ = super::finish_line(ctx.home, goal);
        }
        // A task this node may not take is said on the board, so no peer leaves it to this node (mod.rs
        // `decline_lines`, phase f F6).
        let ok = allowed(ctx.home, ctx.g, ctx.repo);
        let take = takes(&ok, &lacks);
        if !board::declines_due(&b, ctx.here, now, &take).is_empty() {
            let _ = super::decline_lines(ctx.home, goal, &take);
        }
        let ending = Ending::of(&b, ctx.here, now, &ok, &lacks);
        match step(&t, ctx.share, left, &ending, offer.task.is_some()) {
            Next::End(r) => {
                // Said on the board, so every node sees why (a join of a node still in the goal clears it, `Board::stuck`).
                if let (Ending::Stuck(stuck), "stuck") = (&ending, r) {
                    let _ = super::stuck_lines(ctx.home, goal, stuck);
                    end.stuck = stuck.clone();
                }
                end.reason = r;
                end.spent = t.spent;
                return end;
            }
            Next::Wait => {
                // Work held by another drone, a peer's session on its way to a task, a verdict to come:
                // any line may open work this drone can take, so it waits on the board, however long.
                idle_wait(&tasks, seen, &b, IDLE_WAIT);
                continue;
            }
            Next::Go => {}
        }
        iter += 1;
        end.iters += 1;
        let session = session_id(ctx.here, goal, &name, iter);
        // The harness's own cap: the drone's dollars left, never more than the goal has left.
        let left = (ctx.share - t.spent).min(left).max(0.0);
        let run = Headless {
            prompt: &text,
            cwd: wt,
            model: ctx.model,
            session: &session,
            budget_usd: ctx.h.caps().headless_budget.then_some(left),
            env: &env.iter().map(|(k, v)| (*k, v.clone())).collect::<Vec<_>>(),
        };
        let Some(cmd) = ctx.h.headless(&run) else {
            end.reason = "cannot run headless";
            end.spent = t.spent;
            return end;
        };
        // Written before the session starts (its hooks route by it), and only for one that starts: a
        // run join no spend line names is an iteration a killed run started (`recover`).
        let dir = wt.display().to_string();
        let join =
            Join { v: 1, ms: now, goal: goal.into(), dir, drone: name.clone(), session: session.clone(), harness: ctx.h.name().into(), by: "run".into() };
        let _ = super::append_join(ctx.home, goal, &join);
        // The wall clock for the notice, the board's for which lines are this iteration's (`sessions_of`).
        let (started, at) = (learn::now_ms(), super::clock(ctx.home, goal));
        ctx.sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(name.clone(), at);
        let ran = run_session(ctx.home, goal, &name, cmd, ctx.iter_timeout, offer.task.as_ref().map_or(started, |(_, since)| *since), at);
        ctx.sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(&name);
        let out = ran.as_ref().map(|b| b.out.as_slice()).unwrap_or_default();
        let said = ctx.h.headless_end(out);
        log(ctx.home, goal, &name, iter, out);
        let exit = match &ran {
            Some(b) if b.timed_out() => "timeout",
            Some(b) if !said.error && b.status.is_some_and(|s| s.success()) => "ok",
            _ => "error",
        };
        let events = board::read(&super::tasks_path(ctx.home, goal));
        let sessions = sessions_of(ctx.home, &events, &name, at, &[&session, &said.session]);
        // One cut off at the cap, or one that failed, spent up to `iter_timeout` of model calls
        // without saying what.
        let est = (said.usd.is_none() && exit != "ok").then(|| estimate(t.dearest, ctx.share, t.spent));
        let line = Spend {
            v: 1,
            ms: learn::now_ms(),
            drone: name.clone(),
            node: ctx.here.to_string(),
            iter,
            harness: ctx.h.name().into(),
            usd: said.usd,
            est,
            session: sessions.join(","),
            exit: exit.into(),
        };
        let _ = super::append_line(&spend_path(ctx.home, goal), &line);
        t.spent += line.charged();
        t.dearest = t.dearest.max(line.charged());
        // Fail closed: a session that ended well and named no cost, or $0, runs on a harness that
        // meters nothing (or a subscription), which the budget cannot bound. One that failed may not
        // have got to say it: the error count bounds those.
        if exit == "ok" && said.usd.is_none_or(|u| u <= 0.0) {
            t.unmetered = true;
        }
        t.errors = if exit == "ok" { 0 } else { t.errors + 1 };
        let mine = progress(&events, &sessions, at);
        end.done.extend(mine.iter().filter(|e| matches!(e.ev, Ev::Done | Ev::Verified | Ev::Rejected)).map(|e| e.task.clone()));
        let b = board::fold_on(&events, ctx.here);
        let now = learn::now_ms();
        let lacks = super::lacking_here(ctx.home, goal, &b, ctx.root, now);
        t.stalls = stalls(t.stalls, mine.is_empty(), &offer, &open_to(ctx.home, ctx.g, &b, &name, ctx.repo, &lacks, now));
        let left = ctx.g.budget_usd - goal_spent(ctx.home, goal).total;
        let note = match step(&t, ctx.share, left, &if b.finished() { Ending::Finished } else { Ending::Open }, true) {
            Next::End(r) => r,
            _ => "iteration ended",
        };
        release_held(ctx.home, goal, &b, &sessions, note, learn::now_ms());
    }
}

/// The reward (docs/design/hive.md "Reward"): the hidden oracle scores every judged attempt
/// not scored yet, outside every worktree, once the drones ended (scoring between iterations would
/// have the drones' threads race for one attempt). Nothing without a hidden oracle.
fn after_run(home: &Path, g: &Goal, repo: &Path) {
    let scored = super::hidden::score_pending(home, g, repo, usize::MAX, learn::now_ms());
    if !scored.scores.is_empty() {
        let passed = scored.scores.iter().filter(|s| s.pass).count();
        println!("hidden oracle: {passed} of {} judged attempt(s) passed", scored.scores.len());
    }
    for why in &scored.skipped {
        println!("hidden oracle: {why} (not scored; `borg hive score {}` retries)", g.id);
    }
}

/// The run lock of `goal`: held while the run's drones work (released when it drops). None when
/// another run holds it: two runs would drive the same worktrees and each spend the whole budget.
fn run_lock(home: &Path, goal: &str) -> Result<Option<std::fs::File>, String> {
    let path = super::dir(home, goal).join("run.lock");
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    match f.try_lock() {
        Ok(()) => Ok(Some(f)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(format!("{}: {e}", path.display())),
    }
}

/// The ledger by node, this node first as `here`, each other by its id's first 8 characters
/// (`here $0.40, 1a2b3c4d $0.35`); `nothing` when no line was written.
fn spent_by(l: &Ledger, here: &str) -> String {
    let mut parts: Vec<String> = l.nodes.get(here).map(|u| format!("here ${u:.2}")).into_iter().collect();
    parts.extend(l.nodes.iter().filter(|(n, _)| n.as_str() != here).map(|(n, u)| format!("{} ${u:.2}", &n[..n.len().min(8)])));
    if parts.is_empty() { "nothing".into() } else { parts.join(", ") }
}

/// Leaves of the run's worktrees, once its drones ended: a later session there (an engineer reviewing
/// a drone's branch) joins no hive goal. Each shadows the joins before it (mod.rs `joined`).
fn leave_worktrees(home: &Path, goal: &str, wts: &[PathBuf]) {
    let last = super::read_lines::<Join>(&super::joins_path(home, goal)).iter().map(|j| j.ms).max().unwrap_or(0);
    let ms = learn::now_ms().max(last + 1);
    for wt in wts {
        let _ = super::append_join(home, goal, &Join { v: 1, ms, goal: String::new(), dir: wt.display().to_string(), by: "run".into(), ..Join::default() });
    }
}

/// The names of the visible harnesses that can run headless drones.
fn headless_names(probe: &Headless) -> String {
    harness::visible().filter(|h| h.headless(probe).is_some()).map(|h| h.name()).collect::<Vec<_>>().join(", ")
}

fn usage(msg: impl std::fmt::Display) -> i32 {
    eprintln!("borg hive run: {msg}");
    2
}

/// `borg hive run` from `dir` (in the goal's collective): exit 0 when the board is finished, 1 when it
/// is not (or on an error), 2 on a refused precondition.
pub fn main(home: &Path, dir: &str, o: &Opts) -> i32 {
    let g = match goal::load(home, o.goal) {
        Ok(g) => g,
        Err(e) => return usage(e),
    };
    if !(1..=MAX_DRONES).contains(&o.drones) {
        return usage(format!("--drones is 1 to {MAX_DRONES}"));
    }
    if g.budget_usd <= 0.0 || !g.budget_usd.is_finite() {
        return usage(format!("goal {} has no budget: drones spend money", g.id));
    }
    // A group's attempts are different drones', and none of them may judge a rival's (board.rs
    // `Board::rival`): K attempts need K drones and one more to verify them.
    let b = fold(home, &g.id);
    let short =
        super::group::groups(&b).into_iter().find(|(_, m)| m.len() >= o.drones && m.iter().any(|s| !matches!(s.status, super::Status::Verified { .. })));
    if let Some((gr, m)) = short {
        return usage(format!("group {gr} has {} attempts, each another drone's, verified by none of them: run at least {} drones", m.len(), m.len() + 1));
    }
    let Some(repo) = repo_of(Path::new(dir)) else { return usage(format!("{dir} is not in a git repo: drones work in its worktrees")) };
    let repo_name = queen::repo_name(&repo.display().to_string());
    if !goal::in_scope(&g, &repo_name) {
        let names: Vec<&str> = g.repos.iter().map(|r| r.name.as_str()).collect();
        return usage(format!("{repo_name} is outside goal {}'s repos ({})", g.id, names.join(", ")));
    }
    let probe = Headless { prompt: "", cwd: &repo, model: o.model, session: "", budget_usd: None, env: &[] };
    let Some(sample) = o.harness.headless(&probe) else {
        return usage(format!("{} cannot run headless drones (Harness::headless); use one of: {}", o.harness.name(), headless_names(&probe)));
    };
    // The lock before the spend is read: a run ending now has written all of its own.
    let _lock = if o.dry_run {
        None
    } else {
        match run_lock(home, &g.id) {
            Ok(Some(l)) => Some(l),
            Ok(None) => return usage(format!("another `borg hive run {}` is running: one run of a goal at a time", g.id)),
            Err(e) => {
                eprintln!("borg hive run: {e}");
                return 1;
            }
        }
    };
    // Read under the run lock (an interactive join writes its drone, then looks for this lock): a node
    // runs a runner or interactive drones for a goal, never both, since the run's join and leave would
    // end the drones' claims on every peer (phase g correction 3, board.rs `Local::left`).
    if let Some(d) = super::interactive::serving(home, &g.id).filter(|_| !o.dry_run) {
        return usage(format!(
            "interactive drone {} (session {}) serves goal {} on this node: a node runs a runner or interactive drones for a goal, never both (`borg hive leave` in that session)",
            d.drone,
            super::short(&d.session),
            g.id
        ));
    }
    if !o.dry_run {
        recover(home, &g.id, learn::now_ms());
        // The engineer's approval of the plan before any drone starts (plan.rs): no worktree, run line
        // or claim before it. A task opened while it waited (another shell, a peer's line) was on no
        // approved card: the changed plan is asked again.
        loop {
            match super::plan::gate(home, &g, &fold(home, &g.id), &repo, o.yes) {
                Ok(super::plan::Gate::Refused(d)) => {
                    let by = if d.by.is_empty() { String::new() } else { format!(" ({})", d.by) };
                    let why = if d.answer.is_empty() { String::new() } else { format!(": {}", d.answer) };
                    println!("hive run {}: the engineer said no to the plan{by}{why}; nothing was started or claimed", g.id);
                    println!("  change the plan (borg hive open) and run again, or approve this one with --yes");
                    return 1;
                }
                Ok(super::plan::Gate::Approved(d)) => {
                    if super::plan::key(&g.id, &super::plan::cards(&fold(home, &g.id))) != d.key {
                        println!("hive run {}: the plan changed while it waited for approval; the new plan is asked again", g.id);
                        continue;
                    }
                    let by = if d.by.is_empty() { String::new() } else { format!(" by {}", d.by) };
                    let via = if d.via == "yes" { "--yes".to_string() } else { format!("queen question {}", d.question) };
                    println!("hive run {}: plan approved{by} ({via}): {} task(s), {} high-risk", g.id, d.tasks, d.high);
                }
                Ok(super::plan::Gate::Nothing) => {}
                Err(e) => {
                    eprintln!("borg hive run: {e}");
                    return 1;
                }
            }
            break;
        }
    }
    let before: Vec<Spend> = super::read_lines(&spend_path(home, &g.id));
    let here = super::node(home);
    let book = ledger(&before, &here);
    let spent = book.total;
    // What is left of the goal's budget, after every earlier run's spend on every node, shared by this
    // run's drones.
    let share = (g.budget_usd - spent).max(0.0) / o.drones as f64;
    let paths: Vec<PathBuf> = (1..=o.drones).map(|n| worktree(&repo, &g.id, n)).collect();
    if o.dry_run {
        println!("hive run {} (dry run: nothing started, nothing written)", g.id);
        for (n, p) in paths.iter().enumerate() {
            println!("  {}  {}  branch {} from {}", drone_name(n + 1), p.display(), branch(&g.id, n + 1), o.base);
        }
        println!(
            "  each drone: ${share:.2} of ${:.2} left of ${:.2} ({} spent on {} node(s)); every iteration within {}s",
            (g.budget_usd - spent).max(0.0),
            g.budget_usd,
            spent_by(&book, &here),
            book.nodes.len(),
            o.iter_timeout.as_secs()
        );
        println!("  harness {}: {sample:?}", o.harness.name());
        println!("  writes /.worktrees/ to the repo's local .git/info/exclude (never committed)");
        let cards = super::plan::cards(&b);
        if !cards.is_empty() {
            println!("  the plan the engineer approves first ({}):", if o.yes { "--yes approves it" } else { "a queen question, waited for" });
            for c in &cards {
                println!("    {}", super::plan::render_card(c));
            }
        }
        println!("prompt ({}):\n{}", drone_name(1), prompt(&g, &drone_name(1)));
        return 0;
    }
    if let Err(e) = exclude_worktrees(&repo) {
        eprintln!("borg hive run: {e}");
        return 1;
    }
    let mut wts = Vec::new();
    for n in 1..=o.drones {
        match ensure_worktree(&repo, &g.id, n, o.base) {
            Ok(p) => wts.push(p),
            Err(e) => {
                eprintln!("borg hive run: {e}");
                return 1;
            }
        }
    }
    // This node in the goal before its drones claim, and that heard by the peers (phase e E2b, f F4).
    let names: Vec<String> = (1..=wts.len()).map(drone_name).collect();
    trap();
    let (peers, heard, waited) = match super::announce(home, &g.id, &names) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("borg hive run: {e}");
            return 1;
        }
    };
    let joined = Joined { home, goal: &g.id };
    if peers > 0 {
        let how = if heard { "heard back" } else { "not all heard back" };
        println!("hive run {}: announced to {peers} mesh peer(s), {how} in {} ms", g.id, waited.as_millis());
    }
    let run = RunLine {
        v: 1,
        ms: learn::now_ms(),
        drones: o.drones,
        harness: o.harness.name().into(),
        share_usd: share,
        base: o.base.into(),
        peers,
        waited_ms: waited.as_millis() as u64,
    };
    if let Err(e) = super::append_line(&runs_path(home, &g.id), &run) {
        eprintln!("borg hive run: {e}");
        return 1;
    }
    let sessions = Sessions::default();
    let ctx = Ctx {
        home,
        g: &g,
        h: o.harness,
        root: &repo,
        repo: &repo_name,
        model: o.model,
        share,
        iter_timeout: o.iter_timeout,
        before: &before,
        here: &here,
        sessions: &sessions,
    };
    // The drones still running, named on the board at each one's end (mod.rs `roster`).
    let running = std::sync::Mutex::new(names);
    let done = std::sync::atomic::AtomicBool::new(false);
    let ends: Vec<DroneEnd> = std::thread::scope(|s| {
        let beats = s.spawn(|| membership(home, &g.id, &sessions, &done));
        let handles: Vec<_> = wts
            .iter()
            .enumerate()
            .map(|(i, wt)| {
                s.spawn({
                    let (ctx, running) = (&ctx, &running);
                    move || {
                        let end = drone(ctx, i + 1, wt);
                        let mut r = running.lock().unwrap_or_else(|e| e.into_inner());
                        r.retain(|d| *d != end.drone);
                        let _ = super::roster(ctx.home, &ctx.g.id, &r);
                        end
                    }
                })
            })
            .collect();
        let ends = handles.into_iter().filter_map(|h| h.join().ok()).collect();
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = beats.join();
        ends
    });
    drop(joined);
    untrap(&g.id);
    leave_worktrees(home, &g.id, &wts);
    after_run(home, &g, &repo);
    for e in &ends {
        let name = e.worktree.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let done = if e.done.is_empty() { "-".to_string() } else { e.done.join(",") };
        println!("{}  {name}  ${:.2}/${share:.2}  {} iter  tasks done: {done}  end: {}", e.drone, e.spent, e.iters, e.reason);
    }
    // A stuck task: no running drone of the goal may take it (phase f F6). The run gives up on it, and says
    // what would unstick it.
    let stuck: BTreeMap<&str, &str> = ends.iter().flat_map(|e| e.stuck.iter().map(|(t, why)| (t.as_str(), why.as_str()))).collect();
    for (t, why) in &stuck {
        println!("stuck: {t}: {why}");
    }
    if !stuck.is_empty() {
        println!(
            "hive run {}: stuck on {} task(s) no running drone of the goal may take; a run of a node that may take them (another node for a verify task) takes them up",
            g.id,
            stuck.len()
        );
    }
    // Races no board shows: claims a rival's beat to the write, each a session's start spent for nothing.
    let refused =
        super::read_lines::<super::Confirm>(&super::confirms_path(home, &g.id)).iter().filter(|c| c.ms >= run.ms && c.outcome == super::REFUSED).count();
    if refused > 0 {
        println!("claims refused here (a rival's landed first): {refused}");
    }
    let total = goal_spent(home, &g.id);
    println!("goal {}: ${:.2} of ${:.2} spent ({})", g.id, total.total, g.budget_usd, spent_by(&total, &here));
    let b = fold(home, &g.id);
    let now = learn::now_ms();
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for t in b.tasks.values() {
        *counts.entry(t.shown_status(now)).or_default() += 1;
    }
    let counts: Vec<String> = counts.iter().map(|(k, v)| format!("{v} {k}")).collect();
    println!("board: {} task(s) ({}){}", b.tasks.len(), counts.join(", "), if b.finished() { "; the goal is finished" } else { "" });
    if let Some(i) = b.integrated.as_ref().filter(|_| b.finished()) {
        println!("integrated {} ({}, refs/heads/hive/{}/integrated)", i.head, i.task, g.id);
    }
    // Exit 0 only on a finish for good (phase f F6): a drone saw it, or the board says it now.
    let for_good = ends.iter().any(|e| e.reason == "goal finished") || b.finished_for_good(&here, now);
    if b.finished() && for_good { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worktree_is_one_component_under_the_repo_and_folds_back_onto_it() {
        let root = std::env::temp_dir().join(format!("borg-hive-wt-{}", std::process::id()));
        let repo = root.join("app");
        let wt = worktree(&repo, "g", 2);
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        // A linked worktree has a `.git` file.
        std::fs::write(wt.join(".git"), "gitdir: x").unwrap();
        assert_eq!(wt.strip_prefix(repo.join(".worktrees")).unwrap().components().count(), 1);
        assert_eq!(borg_trace::repo_key(&wt.display().to_string()), repo.display().to_string());
        assert_eq!(queen::repo_name(&wt.join("src").display().to_string()), "app");
        assert_eq!(branch("g", 2), "hive/g/2");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_session_says_it_started_only_once_unclaimed_past_its_tasks_notice_time() {
        // Another node leaves this node's task to its idle drones for the grace: a session that claims
        // within the notice time writes nothing (each line starts sync rounds a claim then waits on);
        // one past it says it started, stamped with its start, and that it ended unsettled; one that
        // claims after saying so is settled by its claim.
        let home = std::env::temp_dir().join(format!("borg-hive-run-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let path = super::super::tasks_path(&home, "g");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let notes = || board::read(&path).into_iter().filter(|e| e.by == "runner").map(|e| (e.note, e.ms)).collect::<Vec<_>>();
        let sh = |script: &str| {
            let mut c = Command::new("sh");
            c.args(["-c", script]);
            c
        };
        let cap = Duration::from_secs(10);
        run_session(&home, "g", "d1", sh("sleep 0.2"), cap, learn::now_ms(), super::super::clock(&home, "g"));
        assert_eq!(notes(), [], "within the notice time");
        let t0 = learn::now_ms();
        run_session(&home, "g", "d1", sh("sleep 0.2"), cap, t0 - IDLE_WAIT.as_millis() as i64, super::super::clock(&home, "g"));
        let n = notes();
        assert_eq!(n.iter().map(|(note, _)| note.as_str()).collect::<Vec<_>>(), ["session-start d1", "session-end d1"]);
        assert!(n[0].1 >= t0 && n[0].1 < t0 + 200 && n[1].1 >= n[0].1 + 200, "{n:?}");
        let claim = board::Event { node: super::super::node(&home), ..board::Event::new(Ev::Claim, "t1", "s1", 0).with_drone("d1") };
        let line = serde_json::to_string(&claim).unwrap().replace("\"ms\":0", &format!("\"ms\":{}", learn::now_ms() + 100));
        run_session(&home, "g", "d1", sh(&format!("sleep 0.2 && printf '%s\\n' '{line}' >> '{}'", path.display())), cap, 0, super::super::clock(&home, "g"));
        assert_eq!(notes().len(), 3, "a start, no end: {:?}", notes());
        let b = fold(&home, "g");
        let (started, settled) = b.sessions[&(super::super::node(&home), "d1".to_string())];
        assert!(settled > started, "{:?}", b.sessions);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn an_idle_drone_wakes_when_the_board_changes() {
        // A peer's line lands 100 ms into the wait: the drone looks again then, not after a blind
        // IDLE_WAIT; a quiet board waits the cap out, and so does one only a goal heartbeat came to.
        let dir = std::env::temp_dir().join(format!("borg-hive-idle-wait-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tasks.jsonl");
        std::fs::write(&path, "{}\n").unwrap();
        let seen = mark(&path);
        let lands = std::thread::spawn({
            let path = path.clone();
            move || {
                std::thread::sleep(Duration::from_millis(100));
                board::append(&path, &[board::Event::new(Ev::Claim, "t1", "sb", 1)]).unwrap();
            }
        });
        let t0 = Instant::now();
        assert!(idle_wait(&path, seen, &board::fold(&board::read(&path)), IDLE_WAIT), "the board changed");
        assert!(t0.elapsed() < IDLE_WAIT / 2, "{:?}", t0.elapsed());
        lands.join().unwrap();
        let t0 = Instant::now();
        assert!(!idle_wait(&path, mark(&path), &board::fold(&board::read(&path)), Duration::from_millis(150)), "nothing changed");
        assert!(t0.elapsed() >= Duration::from_millis(150), "{:?}", t0.elapsed());
        // A peer's goal heartbeat lands (phase f F4): the file changed, the board did not; the wait goes on.
        let (seen, b) = (mark(&path), board::fold(&board::read(&path)));
        let beats = std::thread::spawn({
            let path = path.clone();
            move || {
                std::thread::sleep(Duration::from_millis(50));
                let beat = board::Event { node: "B".into(), ..board::Event::new(Ev::Heartbeat, "", "runner", 2).with_note(board::BEAT) };
                board::append(&path, &[beat]).unwrap();
            }
        });
        let t0 = Instant::now();
        assert!(!idle_wait(&path, seen, &b, Duration::from_millis(200)), "a beat changes no board");
        assert!(t0.elapsed() >= Duration::from_millis(200), "{:?}", t0.elapsed());
        beats.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_iteration_beaten_to_its_task_is_no_stall() {
        let offer = |task: Option<(&str, i64)>, open: &[(&str, i64)]| Offer {
            task: task.map(|(id, at)| (id.to_string(), at)),
            open: open.iter().map(|(id, at)| (id.to_string(), *at)).collect(),
        };
        let start = offer(Some(("t1", 0)), &[("t1", 0), ("t2", 5)]);
        assert_eq!(stalls(1, false, &start, &offer(Some(("t1", 0)), &[])), 0, "a board line resets the count");
        assert_eq!(stalls(1, true, &start, &offer(Some(("t1", 0)), &[])), 2, "work open to it as it started, and it did nothing");
        assert_eq!(stalls(1, true, &start, &offer(Some(("t2", 5)), &[])), 2, "another task open to it as it started");
        assert_eq!(stalls(1, true, &start, &offer(None, &[])), 1, "beaten to the task, nothing open after it: it waits (phase f F6)");
        assert_eq!(stalls(1, true, &start, &offer(Some(("t3", -9)), &[])), 1, "beaten to the task: work that became claimable during the session");
        assert_eq!(stalls(1, true, &start, &offer(Some(("t1", 7)), &[])), 1, "its task claimable again during the session (a reject reopened it)");
        let ahead = offer(Some(("t1", 20_000)), &[("t1", 20_000)]);
        assert_eq!(stalls(1, true, &ahead, &ahead.clone()), 2, "stamped by a peer's clock 20 s ahead, still open to it: a stall");
    }

    /// CE3 (tasks/hive-soundness.md) on the board: b's first session writes nothing while its tasks
    /// stay open to it; during its second, node a claims t1 and t2 and its done opens `t2.v1` to b
    /// before b's runner looks again. Work that appeared during the session is no stall: b goes on to
    /// verify instead of ending "stalled" as its work appears.
    #[test]
    fn a_session_that_ends_as_work_opens_to_it_is_no_stall() {
        let task = |id: &str, role: board::Role, of: Option<&str>| board::Task {
            id: id.into(),
            role,
            title: id.into(),
            repo: String::new(),
            files: vec![],
            deps: vec![],
            accept: vec![],
            of: of.map(Into::into),
            not_by: of.map_or(vec![], |_| vec!["sa".into(), "a1".into()]),
            flags: vec![],
            from: None,
            group: None,
        };
        let on_a = |e: board::Event| board::Event { node: "a".into(), ..e };
        let grace = super::super::OWNER_GRACE_MS;
        let look = |events: &[board::Event], now: i64| Offer::of(&board::fold(events), "d1", "b", "", now, |_| true);
        let mut events = vec![
            on_a(board::Event::open(task("t1", board::Role::Work, None), "engineer:e", 0)),
            on_a(board::Event::open(task("t2", board::Role::Work, None), "engineer:e", 0)),
        ];
        let mut t = Tally::default();
        let started = 10 * grace;
        let start = look(&events, started);
        assert!(start.task.is_some(), "b's first session starts on an open task");
        t.stalls = stalls(t.stalls, true, &start, &look(&events, started + 1));
        assert_eq!(t.stalls, 1, "refused its task, which is still open to it");
        let started = 20 * grace;
        let start = look(&events, started);
        events.extend([
            on_a(board::Event::new(Ev::Claim, "t1", "sa1", started + 1).with_drone("a2")),
            on_a(board::Event::new(Ev::Claim, "t2", "sa", started + 1).with_drone("a1")),
            on_a(board::Event::new(Ev::Done, "t2", "sa", started + 2)),
            on_a(board::Event::open(task("t2.v1", board::Role::Verify, Some("t2")), "sa", started + 2)),
        ]);
        let end = look(&events, 30 * grace);
        assert_eq!(end.task.as_ref().map(|(id, _)| id.as_str()), Some("t2.v1"), "a's done opened t2.v1 to b");
        t.stalls = stalls(t.stalls, true, &start, &end);
        assert_eq!(t.stalls, 1, "beaten to t2; t2.v1 became claimable during the session");
        assert_eq!(step(&t, 1.0, 1.0, &Ending::Open, true), Next::Go, "it verifies t2.v1");
    }

    #[test]
    fn every_stop_reason_comes_from_the_tally() {
        let t = |spent: f64, errors: u32, stalls: u32, unmetered: bool| Tally { spent, errors, stalls, unmetered, dearest: 0.0 };
        let (open, stuck) = (Ending::Open, Ending::Stuck(vec![("t1.v1".into(), "no running drone".into())]));
        assert_eq!(step(&t(0.0, 0, 0, false), 1.0, 1.0, &open, true), Next::Go);
        assert_eq!(step(&t(0.5, 0, 0, false), 1.0, 1.0, &Ending::Finished, true), Next::End("goal finished"), "finished first");
        assert_eq!(step(&t(1.2, 2, 2, true), 1.0, 1.0, &Ending::Settling, false), Next::Wait, "finished, not for good yet: it waits, spending nothing");
        assert_eq!(step(&t(1.0, 0, 0, false), 1.0, 1.0, &open, true), Next::End("budget"));
        assert_eq!(step(&t(1.2, 2, 2, true), 1.0, 1.0, &open, true), Next::End("budget"), "budget before the guards");
        assert_eq!(step(&t(0.0, 0, 0, true), 1.0, 1.0, &open, true), Next::End("cost not reported"));
        assert_eq!(step(&t(0.0, 1, 0, false), 1.0, 1.0, &open, true), Next::Go, "one failure is retried");
        assert_eq!(step(&t(0.0, 2, 0, false), 1.0, 1.0, &open, true), Next::End("errors"));
        assert_eq!(step(&t(0.0, 0, 2, false), 1.0, 1.0, &open, true), Next::End("stalled"));
        assert_eq!(step(&t(0.0, 0, 0, false), 1.0, 1.0, &open, false), Next::Wait, "nothing open: it waits, however long (phase f F6)");
        assert_eq!(step(&t(0.0, 0, 0, false), 1.0, 1.0, &stuck, false), Next::End("stuck"));
        assert_eq!(step(&t(0.0, 0, 0, false), 1.0, 1.0, &stuck, true), Next::Go, "work open to it goes first");
        assert_eq!(step(&t(1.0, 0, 0, false), 1.0, 1.0, &stuck, false), Next::End("budget"));
        assert_eq!(step(&t(0.1, 0, 0, false), 1.0, 0.0, &open, true), Next::End("budget"), "another node spent the rest of the goal's budget");
        assert_eq!(step(&t(0.1, 0, 0, false), 1.0, -0.3, &open, true), Next::End("budget"));
        assert_eq!(step(&t(0.1, 0, 0, false), 1.0, 0.01, &open, true), Next::Go);
    }

    fn spend(node: &str, drone: &str, iter: u32, usd: f64) -> Spend {
        Spend {
            v: 1,
            ms: iter as i64,
            drone: drone.into(),
            node: node.into(),
            iter,
            harness: "x".into(),
            usd: Some(usd),
            exit: "ok".into(),
            ..Spend::default()
        }
    }

    #[test]
    fn the_ledger_folds_every_nodes_lines_once_in_any_order() {
        let lines = vec![spend("a", "d1", 1, 0.1), spend("b", "d1", 1, 0.2), spend("", "d2", 1, 0.3), spend("a", "d1", 2, 0.7), spend("b", "d1", 2, 1e-17)];
        let l = ledger(&lines, "a");
        assert_eq!(l.nodes.keys().collect::<Vec<_>>(), ["a", "b"], "a line without a node is this node's");
        assert!((l.total - 1.3).abs() < 1e-12 && (l.nodes["a"] - 1.1).abs() < 1e-12 && (l.nodes["b"] - 0.2).abs() < 1e-12, "{l:?}");
        // Order-independent to the bit, and a line delivered twice counts once.
        let mut rev = lines.clone();
        rev.reverse();
        assert_eq!(ledger(&rev, "a"), l);
        let mut rot = lines.clone();
        rot.rotate_left(2);
        rot.extend(lines.iter().take(3).cloned());
        assert_eq!(ledger(&rot, "a"), l);
        // Read on b, the old line without a node is b's.
        assert!((ledger(&lines, "b").nodes["b"] - 0.5).abs() < 1e-12);
        assert_eq!(ledger(&[], "a"), Ledger::default());
        assert_eq!(spent_by(&l, "a"), "here $1.10, b $0.20");
        assert_eq!(spent_by(&ledger(&[spend("0123456789ab", "d1", 1, 0.25)], "a"), "a"), "01234567 $0.25");
        assert_eq!(spent_by(&Ledger::default(), "a"), "nothing");
    }

    #[test]
    fn a_drones_earlier_iterations_are_its_nodes_only() {
        let lines = vec![spend("a", "d1", 1, 0.1), spend("b", "d1", 5, 0.9), spend("", "d1", 2, 0.2)];
        assert_eq!(spent_before(&lines, "a", "d1"), (2, 0.2), "b's d1 is another drone");
        assert_eq!(spent_before(&lines, "b", "d1"), (5, 0.9));
    }

    #[test]
    fn the_prompt_names_goal_drone_and_the_inoculation() {
        let mut g = Goal {
            v: 1,
            id: "parser".into(),
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
        let p = prompt(&g, "d3");
        assert!(p.contains("drone d3 of hive goal parser") && p.contains("`borg hive board parser`") && p.contains("the commits its claim names"));
        assert!(p.ends_with("useless: the verifier sees every such change and the task counts as failed."), "no hidden oracle is named: {p}");
        assert!(!p.contains("hidden") && !p.contains("held-out"), "{p}");
        assert!(!p.contains('{'), "every placeholder filled: {p}");
        assert!(p.len() < 700, "{} chars", p.len());
        g.hidden = Some(goal::Hidden { dir: "/held".into(), commands: vec!["true".into()], timeout_s: 60 });
        let p = prompt(&g, "d3");
        assert!(p.ends_with(
            "Gaming the oracle (editing tests, CI or oracle files) is visible, allowed to happen and useless: held-out checks you cannot see decide."
        ));
    }

    #[test]
    fn a_timeout_without_a_cost_is_charged_and_a_zero_cost_is_unmetered() {
        let line = |usd: Option<f64>, est: Option<f64>| Spend {
            v: 1,
            ms: 1,
            drone: "d1".into(),
            node: String::new(),
            iter: 1,
            harness: "x".into(),
            usd,
            est,
            session: String::new(),
            exit: "ok".into(),
        };
        assert_eq!((line(Some(0.4), Some(9.0)).charged(), line(None, Some(0.4)).charged(), line(None, None).charged()), (0.4, 0.4, 0.0));
        assert_eq!(spent_before(&[line(Some(0.2), None), line(None, Some(0.7)), line(Some(0.1), None)], "n", "d1"), (1, 0.7));
    }

    #[test]
    fn one_run_of_a_goal_at_a_time() {
        let home = std::env::temp_dir().join(format!("borg-hive-runlock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(super::super::dir(&home, "g")).unwrap();
        let held = run_lock(&home, "g").unwrap();
        assert!(held.is_some());
        assert!(run_lock(&home, "g").unwrap().is_none(), "a second run is refused while the first holds it");
        drop(held);
        // A child another test forks right now holds a copy of every open file, this lock's too, until
        // its exec closes it (close-on-exec): free again within moments, not necessarily at once.
        let deadline = Instant::now() + Duration::from_secs(5);
        while run_lock(&home, "g").unwrap().is_none() {
            assert!(Instant::now() < deadline, "free again once the first ended");
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn session_ids_are_uuid_shaped_and_fresh() {
        let a = session_id("n", "g", "d1", 1);
        let re = regex::Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-a[0-9a-f]{3}-[0-9a-f]{12}$").unwrap();
        assert!(re.is_match(&a), "{a}");
        assert_ne!(a, session_id("n", "g", "d1", 2));
        assert_ne!(a, session_id("m", "g", "d1", 1), "another node's runner names another session");
    }

    /// Phase f F3, after review: this node holds a peer's line stamped 5 min ahead of its clock, so its
    /// own stamps run 5 min ahead too (board.rs `stamp`). d1's first iteration claims and finishes t1;
    /// its second starts a second later. Read against the wall clock at the second's start, the first's
    /// claim and done (5 min ahead) were the second's: its sessions, its progress (the stall count reset
    /// by work done before, `done` given an old task, `release_held` acting for the first). Read from the
    /// board's clock at its start (mod.rs `clock`), they are the first's, and the second's own claim is
    /// its.
    #[test]
    fn an_iteration_reads_its_own_lines_beside_a_peer_clock_ahead() {
        let home = std::env::temp_dir().join(format!("borg-hive-iter-lines-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let path = super::super::tasks_path(&home, "g");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let wall = learn::now_ms();
        let t = |id: &str| board::Task {
            id: id.into(),
            role: board::Role::Work,
            title: id.into(),
            repo: String::new(),
            files: vec![],
            deps: vec![],
            accept: vec![],
            of: None,
            not_by: vec![],
            flags: vec![],
            from: None,
            group: None,
        };
        let peer = board::Event { node: "peer".into(), ..board::Event::new(Ev::Heartbeat, "", "runner", wall + 300_000) };
        board::append(&path, &[peer]).unwrap();
        super::super::write(&home, "g", vec![board::Event::open(t("t1"), "engineer:e", wall), board::Event::open(t("t2"), "engineer:e", wall)]).unwrap();
        let first = super::super::clock(&home, "g");
        let w = |e: board::Event| super::super::write(&home, "g", vec![e]).unwrap();
        w(board::Event::new(Ev::Claim, "t1", "s1", learn::now_ms()).with_drone("d1"));
        w(board::Event::new(Ev::Done, "t1", "s1", learn::now_ms()));
        let (started, at) = (learn::now_ms(), super::super::clock(&home, "g"));
        let events = board::read(&path);
        assert!(events.iter().any(|e| e.by == "s1" && e.ms >= started), "the first's lines are stamped past the second's wall-clock start");
        assert_eq!(sessions_of(&home, &events, "d1", at, &[]), Vec::<String>::new(), "no session of the second's yet");
        assert_eq!(sessions_of(&home, &events, "d1", first, &[]), ["s1"], "the first's");
        assert!(progress(&events, &["s1".to_string()], at).is_empty(), "the first's done is no progress of the second's");
        w(board::Event::new(Ev::Claim, "t2", "s2", learn::now_ms()).with_drone("d1"));
        let events = board::read(&path);
        let sessions = sessions_of(&home, &events, "d1", at, &[]);
        assert_eq!(sessions, ["s2"]);
        assert_eq!(progress(&events, &sessions, at).iter().map(|e| e.task.as_str()).collect::<Vec<_>>(), ["t2"]);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_runner_never_releases_a_remote_claim() {
        let home = std::env::temp_dir().join(format!("borg-hive-remote-claim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let now = learn::now_ms();
        let t = |id: &str| board::Task {
            id: id.into(),
            role: board::Role::Work,
            title: id.into(),
            repo: String::new(),
            files: vec![],
            deps: vec![],
            accept: vec![],
            of: None,
            not_by: vec![],
            flags: vec![],
            from: None,
            group: None,
        };
        let remote = board::Event { node: "remote".into(), ..board::Event::new(Ev::Claim, "t1", "x", now).with_drone("d1") };
        let local = board::Event::new(Ev::Claim, "t2", "y", now).with_drone("d1");
        super::super::write(
            &home,
            "g",
            vec![board::Event::open(t("t1"), "engineer:e", now - 1), board::Event::open(t("t2"), "engineer:e", now - 1), remote.clone(), local],
        )
        .unwrap();
        let events = board::read(&super::super::tasks_path(&home, "g"));
        assert_eq!(sessions_of(&home, &events, "d1", now, &[]), ["y"], "another node's d1 is another drone");
        // The killed-run cleanup: this node's runner drone's claim is released, the remote one stays held.
        recover(&home, "g", now + 1);
        let b = fold(&home, "g");
        assert_eq!(b.tasks["t1"].live(now + 2).map(|(by, node, _)| (by.to_string(), node.to_string())), Some(("x".into(), "remote".into())));
        assert!(b.tasks["t2"].live(now + 2).is_none(), "the local claim is released");
        let released: Vec<String> = board::read(&super::super::tasks_path(&home, "g")).into_iter().filter(|e| e.ev == Ev::Released).map(|e| e.by).collect();
        assert_eq!(released, ["y"]);
        let _ = std::fs::remove_dir_all(home);
    }
}
