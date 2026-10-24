//! `borg hive run <goal> --drones N`: N headless drones on one board, each an iteration loop in a git
//! worktree of its own (`<repo>/.worktrees/hive-<goal>-<n>` on branch `hive/<goal>/<n>`, kept with its
//! commits across runs), each with an equal share of what is left of the goal's budget (all runs'
//! spend counted). One run of a goal at a time (a lock on `run.lock`). A drone is a sequence of headless sessions of the harness
//! (`Harness::headless`), each launched with `BORG_HIVE=<goal>` and `BORG_HIVE_DRONE=d<n>`: the hooks
//! join it to the goal, show it its claim and the board, and gate its stop on the oracle (mod.rs).
//!
//! Once every task is verified, the drone thread that sees it first integrates (integrate.rs): it
//! claims the board's integrate task, merges the accepted attempts and runs the oracle on the combined
//! tree, in process and at no model cost, while the other drones wait on its lease. The goal is
//! finished only when that passes; a failure opens a fix task the drones take like any work.
//!
//! Between iterations the runner reads the board once (`board::fold`) and decides ([`step`]): the
//! board is finished, the share is spent, a session ended well without naming its cost or at $0 (fail
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
//! Lines, under `<store>/hive/<goal>/`: `spend.jsonl` (one per iteration, also what a re-run counts
//! as spent), `runs.jsonl` (one per run), `joins.jsonl` (each iteration's session and worktree, and a
//! leave of each worktree when the run ends: a later session there, an engineer's review, joins
//! nothing). `logs/<drone>-<iter>.out` keeps each iteration's output (at most `LOG_CAP`): local
//! only, never synced. When the drones end, the goal's hidden oracle scores every attempt they had
//! judged ([`after_run`], hidden.rs) in scratch checkouts under the goal's directory: no oracle file
//! is ever put into a drone's worktree.

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
/// inoculation framing (docs/design/hive-learning.md), and names held-out checks only for a goal that
/// has them ({decides}, as mod.rs `inoculation`).
const PROMPT: &str = "You are drone {drone} of hive goal {goal}. Work in this worktree only. \
borg's hooks show your claim or the next open task with the exact commands: claim one task \
(the board: `borg hive board {goal}`), publish every interface you create or change as a contract, \
and finish when its oracle passes (ending your turn runs it). A verify task's work is in its \
author's worktree: {joins} maps sessions to worktrees. Gaming the oracle (editing tests, CI or \
oracle files) is visible, allowed to happen and useless: {decides}.";
const DECIDES_HIDDEN: &str = "held-out checks you cannot see decide";
const DECIDES: &str = "the verifier sees every such change and the task counts as failed";

pub fn spend_path(home: &Path, goal: &str) -> PathBuf {
    super::dir(home, goal).join("spend.jsonl")
}

pub fn runs_path(home: &Path, goal: &str) -> PathBuf {
    super::dir(home, goal).join("runs.jsonl")
}

fn logs_dir(home: &Path, goal: &str) -> PathBuf {
    super::dir(home, goal).join("logs")
}

/// One iteration of one drone: what it cost (None: the harness reported nothing) and how it ended
/// (`ok`, `timeout`, `error`, or `killed`: its run died, written by the next run, [`recover`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spend {
    pub v: u8,
    pub ms: i64,
    pub drone: String,
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

/// What a drone does next: the stop reasons in order, then whether a task is open to it.
pub fn step(t: &Tally, share: f64, finished: bool, open: bool) -> Next {
    if finished {
        Next::End("goal finished")
    } else if t.spent >= share {
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

/// Of the iterations before this run (`lines`), `drone`'s last number and its dearest one's charge (a
/// timed-out iteration's estimate starts from it).
fn spent_before(lines: &[Spend], drone: &str) -> (u32, f64) {
    lines.iter().filter(|s| s.drone == drone).fold((0, 0.0), |(n, dear), s| (n.max(s.iter), dear.max(s.charged())))
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
    let mut lines: Vec<Spend> = super::read_lines(&spend_path(home, goal));
    let run = super::read_lines::<RunLine>(&runs_path(home, goal)).pop();
    let (since, share) = run.map_or((i64::MAX, 0.0), |r| (r.ms, r.share_usd));
    let named: std::collections::HashSet<String> = lines.iter().flat_map(|s| s.session.split(',').map(String::from)).collect();
    let joins = super::read_lines::<Join>(&super::joins_path(home, goal));
    for j in joins.iter().filter(|j| j.by == "run" && j.ms >= since && !j.session.is_empty() && !named.contains(&j.session)) {
        let (iter, dearest) = spent_before(&lines, &j.drone);
        let spent: f64 = lines.iter().filter(|s| s.drone == j.drone && s.ms >= since).map(Spend::charged).sum();
        let line = Spend {
            v: 1,
            ms: now,
            drone: j.drone.clone(),
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
    let (mut iter, dearest) = spent_before(ctx.before, &name);
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
        match step(&t, ctx.share, b.finished(), open_to(ctx.home, ctx.g, &b, &name, ctx.repo, now)) {
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
        let left = (ctx.share - t.spent).max(0.0);
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
        t.stalls = if mine.is_empty() { t.stalls + 1 } else { 0 };
        let b = board::fold(&events);
        let note = match step(&t, ctx.share, b.finished(), true) {
            Next::End(r) => r,
            _ => "iteration ended",
        };
        release_held(ctx.home, goal, &b, &sessions, note, learn::now_ms());
    }
}

/// The reward (docs/design/hive-learning.md "Reward"): the hidden oracle scores every judged attempt
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
    }
    let before: Vec<Spend> = super::read_lines(&spend_path(home, &g.id));
    let spent: f64 = before.iter().map(Spend::charged).sum();
    // What is left of the goal's budget, after every earlier run's spend, shared by this run's drones.
    let share = (g.budget_usd - spent).max(0.0) / o.drones as f64;
    let paths: Vec<PathBuf> = (1..=o.drones).map(|n| worktree(&repo, &g.id, n)).collect();
    if o.dry_run {
        println!("hive run {} (dry run: nothing started, nothing written)", g.id);
        for (n, p) in paths.iter().enumerate() {
            println!("  {}  {}  branch {} from {}", drone_name(n + 1), p.display(), branch(&g.id, n + 1), o.base);
        }
        println!(
            "  each drone: ${share:.2} of ${:.2} left of ${:.2}; every iteration within {}s",
            (g.budget_usd - spent).max(0.0),
            g.budget_usd,
            o.iter_timeout.as_secs()
        );
        println!("  harness {}: {sample:?}", o.harness.name());
        println!("  writes /.worktrees/ to the repo's local .git/info/exclude (never committed)");
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
    let ctx = Ctx { home, g: &g, h: o.harness, root: &repo, repo: &repo_name, model: o.model, share, iter_timeout: o.iter_timeout, before: &before };
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
    fn every_stop_reason_comes_from_the_tally() {
        let t = |spent: f64, idle: u32, errors: u32, stalls: u32, unmetered: bool| Tally { spent, idle, errors, stalls, unmetered, dearest: 0.0 };
        assert_eq!(step(&t(0.0, 0, 0, 0, false), 1.0, false, true), Next::Go);
        assert_eq!(step(&t(0.5, 0, 0, 0, false), 1.0, true, true), Next::End("goal finished"), "finished first");
        assert_eq!(step(&t(1.0, 0, 0, 0, false), 1.0, false, true), Next::End("budget"));
        assert_eq!(step(&t(1.2, 0, 2, 2, true), 1.0, false, true), Next::End("budget"), "budget before the guards");
        assert_eq!(step(&t(0.0, 0, 0, 0, true), 1.0, false, true), Next::End("cost not reported"));
        assert_eq!(step(&t(0.0, 0, 1, 0, false), 1.0, false, true), Next::Go, "one failure is retried");
        assert_eq!(step(&t(0.0, 0, 2, 0, false), 1.0, false, true), Next::End("errors"));
        assert_eq!(step(&t(0.0, 0, 0, 2, false), 1.0, false, true), Next::End("stalled"));
        assert_eq!(step(&t(0.0, 0, 0, 0, false), 1.0, false, false), Next::Wait);
        assert_eq!(step(&t(0.0, 2, 0, 0, false), 1.0, false, false), Next::Wait);
        assert_eq!(step(&t(0.0, 3, 0, 0, false), 1.0, false, false), Next::End("nothing to do"));
        assert_eq!(step(&t(0.0, 3, 0, 0, false), 1.0, false, true), Next::Go, "an open task resets the wait");
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
            iter: 1,
            harness: "x".into(),
            usd,
            est,
            session: String::new(),
            exit: "ok".into(),
        };
        assert_eq!((line(Some(0.4), Some(9.0)).charged(), line(None, Some(0.4)).charged(), line(None, None).charged()), (0.4, 0.4, 0.0));
        assert_eq!(spent_before(&[line(Some(0.2), None), line(None, Some(0.7)), line(Some(0.1), None)], "d1"), (1, 0.7));
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
        assert!(run_lock(&home, "g").unwrap().is_some(), "free again once the first ended");
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
