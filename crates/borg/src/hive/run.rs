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
//! Once every task is verified, the drone thread that sees it first integrates (integrate.rs): it
//! claims the board's integrate task, merges the accepted attempts and runs the oracle on the combined
//! tree, in process and at no model cost, while the other drones wait on its lease. The goal is
//! finished only when that passes; a failure opens a fix task the drones take like any work.
//!
//! Between iterations the runner reads the board once (`board::fold`) and decides ([`step`]): the
//! board is finished, the share is spent or the goal's budget is (every node's spend, [`ledger`]), a session ended well without naming its cost or at $0 (fail
//! closed: an unmetered harness, or one on a subscription, gets one iteration), two iterations in a row
//! failed or timed out, two made no board progress, or nothing was open for three polls. An iteration
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
//! A run killed mid-iteration (Ctrl-C, a closed terminal) writes nothing more, and its sessions die
//! with it (runner.rs `run_bounded`). The next run of the goal makes that good under the lock, before
//! it counts what is spent ([`recover`]).
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
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{Board, Ev, Goal, Join, board, goal};
use crate::harness::{self, Harness, Headless};
use crate::{learn, queen, runner};

/// At most this many drones (`borg top` names drones One to Twenty).
pub const MAX_DRONES: usize = 20;
/// One iteration's cap when `--iter-timeout` names none.
pub const DEFAULT_ITER_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Guards, not tuned defaults: polls of a board with nothing open to the drone before it ends, failed
/// iterations and iterations without board progress in a row.
const IDLE_POLLS: u32 = 3;
const IDLE_WAIT: Duration = Duration::from_secs(3);
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
and finish when its oracle passes (ending your turn runs it). A verify task's work is in its \
author's worktree: {joins} maps sessions to worktrees. Gaming the oracle (editing tests, CI or \
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
pub fn prompt(g: &Goal, drone: &str, joins: &Path) -> String {
    let decides = if goal::has_hidden(g) { DECIDES_HIDDEN } else { DECIDES };
    PROMPT.replace("{goal}", &g.id).replace("{drone}", drone).replace("{joins}", &joins.display().to_string()).replace("{decides}", decides)
}

/// A drone's tally between iterations.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tally {
    pub spent: f64,
    pub idle: u32,
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
    /// Nothing open to the drone: poll again.
    Wait,
    End(&'static str),
}

/// What a drone does next: the stop reasons in order, then whether a task is open to it. `left` is what
/// is left of the goal's budget after every node's spend ([`ledger`]).
pub fn step(t: &Tally, share: f64, left: f64, finished: bool, open: bool) -> Next {
    if finished {
        Next::End("goal finished")
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
    } else if t.idle >= IDLE_POLLS {
        Next::End("nothing to do")
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
}

fn fold(home: &Path, goal: &str) -> Board {
    board::fold(&board::read(&super::tasks_path(home, goal)))
}

/// A task is open to `drone` of this node (`home`) at `now`: one of its new sessions could claim it (its
/// checks approved here, oracle.rs `Approvals`).
fn open_to(home: &Path, g: &Goal, b: &Board, drone: &str, repo: &str, now: i64) -> bool {
    let approvals = super::oracle::Approvals::load(home, &g.id);
    board::next_for(b, "", drone, &super::node(home), repo, now, |s| super::may_claim(g, &s.task, repo) && approvals.pending(g, &s.task).is_empty()).is_some()
}

/// The stall count after an iteration. One that wrote no board line is a stall only while work is
/// still open to the drone: one beaten to its task (a race lost here or on another node) while that
/// work is in hand left nothing to do, so the runner waits on the holder instead of ending.
fn stalls(n: u32, wrote_nothing: bool, open: bool, in_flight: bool) -> u32 {
    match (wrote_nothing, open || !in_flight) {
        (false, _) => 0,
        (true, false) => n,
        (true, true) => n + 1,
    }
}

/// A fresh UUID-shaped session id (Claude Code's `--session-id` takes only a UUID).
fn session_id(goal: &str, drone: &str, iter: u32) -> String {
    let mut h = blake3::Hasher::new();
    for part in [goal, drone, &iter.to_string(), &learn::now_ms().to_string(), &std::process::id().to_string()] {
        h.update(part.as_bytes());
        h.update(b"\0");
    }
    let x = h.finalize().to_hex();
    format!("{}-{}-4{}-a{}-{}", &x[0..8], &x[8..12], &x[13..16], &x[17..20], &x[20..32])
}

/// git in `dir`, running no hook: a drone can plant one in the shared `.git/hooks` (`worktree add`
/// runs post-checkout), and the runner runs as the engineer, outside any iteration's cap (as
/// integrate.rs `git`).
fn git(dir: &Path, args: &[&str], timeout: Duration) -> Option<String> {
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
fn is_worktree(path: &Path) -> bool {
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
fn exclude_worktrees(repo: &Path) -> Result<(), String> {
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
    let held: Vec<board::Event> =
        sessions.iter().filter_map(|s| board::holding(b, s, now).map(|t| board::Event::new(Ev::Released, &t.task.id, s, now).with_note(note))).collect();
    if !held.is_empty() {
        let _ = super::write(home, goal, held);
    }
}

/// The sessions of `drone`'s iteration that started at `started`: those named (`known`, empty ones
/// skipped) and every one whose claim on this node (`home`) carries the drone since. A harness may
/// ignore the session id the runner chose and report none (omp, opencode); its hooks still claim as the
/// drone (`DRONE_ENV`). Another node's d1 is another drone: its sessions are never this runner's.
fn sessions_of(home: &Path, events: &[board::Event], drone: &str, started: i64, known: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = known.iter().filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
    for e in events.iter().filter(|e| e.ev == Ev::Claim && e.drone == drone && super::is_local(home, &e.node) && e.ms >= started) {
        if !out.contains(&e.by) {
            out.push(e.by.clone());
        }
    }
    out
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
/// task for `LEASE_MS`. A claim made on another node (a peer's d1) is live there and never released.
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
    let held: Vec<String> = b
        .tasks
        .values()
        .filter_map(|s| s.live(now))
        .filter(|(by, node, _)| super::is_local(home, node) && b.drones.get(*by).is_some_and(|d| runner(d)))
        .map(|(by, _, _)| by.to_string())
        .collect();
    release_held(home, goal, &b, &held, "its run ended", now);
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
    let mut end = DroneEnd { drone: name.clone(), worktree: wt.to_path_buf(), spent: 0.0, iters: 0, done: Vec::new(), reason: "" };
    let text = prompt(ctx.g, &name, &super::joins_path(ctx.home, goal));
    let env = [(super::ENV, goal.to_string()), (super::DRONE_ENV, name.clone())];
    loop {
        let now = learn::now_ms();
        let b = fold(ctx.home, goal);
        if let Some(o) = super::integrate::run(ctx.home, ctx.g, ctx.root, &name, now) {
            let head = o.head.as_deref().map_or("-", |h| &h[..h.len().min(12)]);
            match &o.verdict {
                Ok(()) => println!("{name}  {}  integrated {head}: the oracle passes", o.task),
                Err(why) => println!("{name}  {}  {head}: {why}", o.task),
            }
            continue;
        }
        // Accepted work whose commits are on another node only: this node cannot integrate it, and
        // writes no board line for it (integrate.rs `absent`).
        if let Some(why) = super::integrate::elsewhere(ctx.home, ctx.g, ctx.root, now) {
            println!("{name}  {why}");
            end.reason = "integration needs another node's commits";
            end.spent = t.spent;
            return end;
        }
        // The goal's budget left after every node's spend, read before each decision: a peer's lines
        // arrive between iterations.
        let left = ctx.g.budget_usd - goal_spent(ctx.home, goal).total;
        match step(&t, ctx.share, left, b.finished(), open_to(ctx.home, ctx.g, &b, &name, ctx.repo, now)) {
            Next::End(r) => {
                end.reason = r;
                end.spent = t.spent;
                return end;
            }
            Next::Wait => {
                // Work held by another drone may still open a verify task this drone can take.
                if !b.in_flight(now) {
                    t.idle += 1;
                }
                std::thread::sleep(IDLE_WAIT);
                continue;
            }
            Next::Go => t.idle = 0,
        }
        iter += 1;
        end.iters += 1;
        let session = session_id(goal, &name, iter);
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
        let started = learn::now_ms();
        let ran = runner::run_bounded(cmd, None, ctx.iter_timeout);
        let out = ran.as_ref().map(|b| b.out.as_slice()).unwrap_or_default();
        let said = ctx.h.headless_end(out);
        log(ctx.home, goal, &name, iter, out);
        let exit = match &ran {
            Some(b) if b.timed_out() => "timeout",
            Some(b) if !said.error && b.status.is_some_and(|s| s.success()) => "ok",
            _ => "error",
        };
        let events = board::read(&super::tasks_path(ctx.home, goal));
        let sessions = sessions_of(ctx.home, &events, &name, started, &[&session, &said.session]);
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
        // Board progress: any line of this iteration's sessions (a claim, a done, a verdict).
        let mine: Vec<&board::Event> = events.iter().filter(|e| e.ms >= started && sessions.contains(&e.by) && e.ev != Ev::Heartbeat).collect();
        end.done.extend(mine.iter().filter(|e| matches!(e.ev, Ev::Done | Ev::Verified | Ev::Rejected)).map(|e| e.task.clone()));
        let b = board::fold(&events);
        let now = learn::now_ms();
        t.stalls = stalls(t.stalls, mine.is_empty(), open_to(ctx.home, ctx.g, &b, &name, ctx.repo, now), b.in_flight(now));
        let left = ctx.g.budget_usd - goal_spent(ctx.home, goal).total;
        let note = match step(&t, ctx.share, left, b.finished(), true) {
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
        println!("prompt ({}):\n{}", drone_name(1), prompt(&g, &drone_name(1), &super::joins_path(home, &g.id)));
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
    let run = RunLine { v: 1, ms: learn::now_ms(), drones: o.drones, harness: o.harness.name().into(), share_usd: share, base: o.base.into() };
    if let Err(e) = super::append_line(&runs_path(home, &g.id), &run) {
        eprintln!("borg hive run: {e}");
        return 1;
    }
    let ctx =
        Ctx { home, g: &g, h: o.harness, root: &repo, repo: &repo_name, model: o.model, share, iter_timeout: o.iter_timeout, before: &before, here: &here };
    let ends: Vec<DroneEnd> = std::thread::scope(|s| {
        let handles: Vec<_> = wts
            .iter()
            .enumerate()
            .map(|(i, wt)| {
                s.spawn({
                    let ctx = &ctx;
                    move || drone(ctx, i + 1, wt)
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    leave_worktrees(home, &g.id, &wts);
    after_run(home, &g, &repo);
    for e in &ends {
        let name = e.worktree.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let done = if e.done.is_empty() { "-".to_string() } else { e.done.join(",") };
        println!("{}  {name}  ${:.2}/${share:.2}  {} iter  tasks done: {done}  end: {}", e.drone, e.spent, e.iters, e.reason);
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
    if b.finished() { 0 } else { 1 }
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
    fn an_iteration_beaten_to_its_task_while_the_work_is_in_hand_is_no_stall() {
        assert_eq!(stalls(1, false, true, true), 0, "a board line resets the count");
        assert_eq!(stalls(1, true, true, true), 2, "work still open to it and it did nothing");
        assert_eq!(stalls(1, true, false, false), 2, "nothing open and nothing in hand");
        assert_eq!(stalls(1, true, false, true), 1, "beaten to the task: another drone holds the work");
    }

    #[test]
    fn every_stop_reason_comes_from_the_tally() {
        let t = |spent: f64, idle: u32, errors: u32, stalls: u32, unmetered: bool| Tally { spent, idle, errors, stalls, unmetered, dearest: 0.0 };
        assert_eq!(step(&t(0.0, 0, 0, 0, false), 1.0, 1.0, false, true), Next::Go);
        assert_eq!(step(&t(0.5, 0, 0, 0, false), 1.0, 1.0, true, true), Next::End("goal finished"), "finished first");
        assert_eq!(step(&t(1.0, 0, 0, 0, false), 1.0, 1.0, false, true), Next::End("budget"));
        assert_eq!(step(&t(1.2, 0, 2, 2, true), 1.0, 1.0, false, true), Next::End("budget"), "budget before the guards");
        assert_eq!(step(&t(0.0, 0, 0, 0, true), 1.0, 1.0, false, true), Next::End("cost not reported"));
        assert_eq!(step(&t(0.0, 0, 1, 0, false), 1.0, 1.0, false, true), Next::Go, "one failure is retried");
        assert_eq!(step(&t(0.0, 0, 2, 0, false), 1.0, 1.0, false, true), Next::End("errors"));
        assert_eq!(step(&t(0.0, 0, 0, 2, false), 1.0, 1.0, false, true), Next::End("stalled"));
        assert_eq!(step(&t(0.0, 0, 0, 0, false), 1.0, 1.0, false, false), Next::Wait);
        assert_eq!(step(&t(0.0, 2, 0, 0, false), 1.0, 1.0, false, false), Next::Wait);
        assert_eq!(step(&t(0.0, 3, 0, 0, false), 1.0, 1.0, false, false), Next::End("nothing to do"));
        assert_eq!(step(&t(0.0, 3, 0, 0, false), 1.0, 1.0, false, true), Next::Go, "an open task resets the wait");
        assert_eq!(step(&t(0.1, 0, 0, 0, false), 1.0, 0.0, false, true), Next::End("budget"), "another node spent the rest of the goal's budget");
        assert_eq!(step(&t(0.1, 0, 0, 0, false), 1.0, -0.3, false, true), Next::End("budget"));
        assert_eq!(step(&t(0.1, 0, 0, 0, false), 1.0, 0.01, false, true), Next::Go);
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
        let p = prompt(&g, "d3", Path::new("/s/hive/parser/joins.jsonl"));
        assert!(p.contains("drone d3 of hive goal parser") && p.contains("`borg hive board parser`") && p.contains("/s/hive/parser/joins.jsonl"));
        assert!(p.ends_with("useless: the verifier sees every such change and the task counts as failed."), "no hidden oracle is named: {p}");
        assert!(!p.contains("hidden") && !p.contains("held-out"), "{p}");
        assert!(!p.contains('{'), "every placeholder filled: {p}");
        assert!(p.len() < 700, "{} chars", p.len());
        g.hidden = Some(goal::Hidden { dir: "/held".into(), commands: vec!["true".into()], timeout_s: 60 });
        let p = prompt(&g, "d3", Path::new("/s/hive/parser/joins.jsonl"));
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
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while run_lock(&home, "g").unwrap().is_none() {
            assert!(std::time::Instant::now() < deadline, "free again once the first ended");
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn session_ids_are_uuid_shaped_and_fresh() {
        let a = session_id("g", "d1", 1);
        let re = regex::Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-a[0-9a-f]{3}-[0-9a-f]{12}$").unwrap();
        assert!(re.is_match(&a), "{a}");
        assert_ne!(a, session_id("g", "d1", 2));
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
