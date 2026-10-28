//! The node's goal work for its interactive drones (phase g G4, docs/design/hive.md "Phase g"): what a
//! runner's threads do beside its drones (run.rs `membership`, `drone`), done for sessions the engineer
//! opened by a process that is no model's and runs while every drone is idle: the node's mesh daemon
//! (`borg mesh serve`, borg-mesh main.rs, `serve` on a thread of its own), or, while no daemon runs, the
//! waiter of one of its drones (wait.rs). Whoever holds the node lock (`hive/node.lock`, `take`) does it,
//! so it runs once a node.
//!
//! For each goal a live drone of this node serves (interactive.rs `served`), while the board shows this
//! node's membership of it open (interactive.rs `member_open`):
//! - its beat every `BEAT_MS`, and at once on another node's claim (`board::to_ack`), as run.rs
//!   `membership`: the node stays a participant however long its drones idle or work (phase f F4, F5);
//! - the roster of its takers (`takers`, phase g correction 4): a drone that waits for work or holds a
//!   claim, not one in the engineer's turn or with no wait running, so a task only such drones could
//!   take reads stuck (F6) instead of waiting behind a drone nothing wakes;
//! - its finished, decline and stuck lines when due (mod.rs `finish_line`, `decline_lines`,
//!   `stuck_lines`), and the bundle line of a done that never wrote one (commits.rs `republish`);
//! - the integration once it is due here, as a child `borg hive integrate` (`integrate_child`), one at a
//!   time a goal, so no tick waits on a merge and its oracle: a goal is integrated while every drone is
//!   idle.
//!
//! And for the node: the reaper (`reap`), a drone whose harness process is gone leaves (interactive.rs
//! `leave_noted`), its claim released, whether or not a waiter of it runs; only a drone whose join ran
//! in the reaper's pid namespace (interactive.rs `reapable`), so a pid this process cannot see as the
//! join saw it is never read as gone.
//!
//! It never writes the node's join or leave (phase g correction 2): a join is written by a drone's join
//! (interactive.rs `ensure_member`) and the leave with the node's last drone of a goal, so a daemon's stop
//! (SIGINT, SIGTERM, SIGHUP) or start ends no claim on any peer (board.rs `Local::left` reads a later
//! join or a leave as the end of every claim the node held). A goal whose membership is not open is not
//! beaten. While the worker is down its node is a transport stall to its peers: they may suspect it (a
//! duplicate attempt at worst, phase f F5), its claims stay held here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::interactive::{self, Entry};
use super::run::{self, Ending};
use super::{Board, Commits, Goal, HiveSession, board, bound, integrate, member, tasks_path};
use crate::{learn, queen};

/// The node lock, beside the goals: held by the process that does the node's goal work.
pub const LOCK: &str = "node.lock";

/// How often the worker looks at the registry and the boards (a stat each); a board that grew is read at
/// once, so an ack beat goes within a tick, as a runner's (run.rs `IDLE_TICK`).
pub const TICK: Duration = Duration::from_millis(25);
/// A goal's lines are looked at in full (roster, finished, declines, stuck, integration) on a change of its
/// board, else at least this often: the grace of a stuck and the takers' grace run on time alone.
const LOOK_MS: i64 = 1000;
/// How long a drone that stopped waiting (its engineer's turn, its wait not started yet after a task)
/// stays in its node's roster of takers: a drone between two tasks keeps its place, one that stays away
/// leaves it within the grace a stuck has (board.rs `stuck`'s `SUSPECT_MS`).
pub const TAKER_GRACE_MS: i64 = super::SUSPECT_MS;
/// How long after an integration child ended without making the integration go away another starts: a
/// child that failed on git or a lock is not started again every look.
const INTEGRATE_AGAIN_MS: i64 = super::SUSPECT_MS;

pub fn lock_path(home: &Path) -> PathBuf {
    home.join("hive").join(LOCK)
}

/// What the mesh daemon's worker writes into the node lock it holds (`take`, `worker`).
pub const DAEMON: &str = "daemon";

/// The node lock for the mesh daemon (`take_as` with `DAEMON`).
pub fn take(home: &Path) -> Option<std::fs::File> {
    take_as(home, DAEMON)
}

/// The node lock, when no other process holds it: held for as long as the returned file is open, and
/// saying who holds it (`who`: `daemon`, or `waiter <drone>`), which monitoring reads (`worker`). None
/// when another holds it (a daemon, a waiter) or it cannot be opened. The lock is taken before the file
/// is rewritten, so a reader never finds a loser's name in it.
pub fn take_as(home: &Path, who: &str) -> Option<std::fs::File> {
    use std::io::Write;
    let path = lock_path(home);
    std::fs::create_dir_all(path.parent()?).ok()?;
    let mut f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path).ok()?;
    f.try_lock().ok()?;
    let _ = f.set_len(0).and_then(|()| f.write_all(format!("{who}\n").as_bytes()));
    Some(f)
}

/// Whether a process does this node's goal work now (it holds the node lock): then a drone's hooks write
/// no beat of their own (interactive.rs `touch`).
pub fn worker_runs(home: &Path) -> bool {
    let Ok(f) = std::fs::OpenOptions::new().write(true).open(lock_path(home)) else { return false };
    matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

/// Who does this node's goal work now (monitoring, phase g G6): `daemon` (the mesh daemon), `waiter
/// <drone>` (a drone's waiter, while no daemon runs), `a process` (a holder from before the lock named
/// it), or `none` when no process holds the node lock.
pub fn worker(home: &Path) -> String {
    if !worker_runs(home) {
        return "none".into();
    }
    let who = std::fs::read_to_string(lock_path(home)).unwrap_or_default();
    match who.trim() {
        "" => "a process".into(),
        w => w.to_string(),
    }
}

/// The mesh daemon's node worker (borg-mesh main.rs `serve`): takes the node lock (a waiter that holds it
/// keeps it until it exits; the daemon takes it then) and ticks until `stopped` says so (a stop, or
/// borg-mesh replaced: no tick of old code), then returns writing nothing: a daemon's stop is no leave
/// of the node (phase g correction 2).
pub fn serve(home: &Path, stopped: impl Fn() -> bool) {
    let lock = loop {
        if stopped() {
            return;
        }
        if let Some(l) = take(home) {
            break l;
        }
        std::thread::sleep(Duration::from_secs(1));
    };
    let mut w = Worker::default();
    while !stopped() {
        w.tick(home);
        std::thread::sleep(TICK);
    }
    drop(lock);
}

/// What the worker keeps between ticks of one goal.
#[derive(Default)]
struct Pace {
    next_beat: i64,
    acked: i64,
    mark: run::Mark,
    looked: i64,
    /// When each drone of the node was last a taker of the goal (`takers`).
    taker_ms: BTreeMap<String, i64>,
    child: Option<std::process::Child>,
    /// No integration child starts before this (`INTEGRATE_AGAIN_MS`).
    integrate_after: i64,
}

/// The node's goal work between ticks (`tick`): per goal its pace, and the registry's mark.
#[derive(Default)]
pub struct Worker {
    goals: BTreeMap<String, Pace>,
    registry: run::Mark,
    looked: i64,
    /// The goals the node's drones serve, each with its drones (registry line, membership) and the repos
    /// they work in (the main checkout of each drone's tree).
    served: BTreeMap<String, Served>,
    /// The drone whose waiter runs this worker (`for_waiter`): its own end is the waiter's to see and say.
    own: Option<String>,
}

#[derive(Clone)]
struct Served {
    goal: Goal,
    drones: Vec<(Entry, HiveSession)>,
    repos: Vec<PathBuf>,
}

impl Worker {
    /// The worker of `drone`'s waiter (wait.rs): it reaps every drone but that one, whose waiter sees its
    /// session gone itself and says so.
    pub fn for_waiter(drone: &str) -> Worker {
        Worker { own: Some(drone.to_string()), ..Worker::default() }
    }

    /// One pass of the node's goal work at the wall clock now. With no registry (no drone ever ran on this
    /// node) one failed stat, nothing else.
    pub fn tick(&mut self, home: &Path) {
        let now = learn::now_ms();
        let mark = run::mark(&interactive::registry_path(home));
        if mark.is_none() && self.goals.values().all(|p| p.child.is_none()) {
            return;
        }
        if mark != self.registry || now - self.looked >= LOOK_MS {
            (self.registry, self.looked) = (mark, now);
            for e in interactive::live(home).iter().filter(|e| self.own.as_ref() != Some(&e.drone) && interactive::reapable(e) && interactive::harness_gone(e))
            {
                reap(home, e, now);
            }
            self.served = served(home);
            self.goals.retain(|id, p| self.served.contains_key(id) || p.child.is_some());
        }
        let here = super::node(home);
        for (id, s) in &self.served {
            let pace = self.goals.entry(id.clone()).or_default();
            one(home, &here, s, pace);
        }
        for (_, pace) in self.goals.iter_mut().filter(|(id, _)| !self.served.contains_key(*id)) {
            reap_child(pace, learn::now_ms());
        }
    }
}

/// The goals this node's live, bound drones serve now, with those drones and their repos.
fn served(home: &Path) -> BTreeMap<String, Served> {
    let mut out: BTreeMap<String, Served> = BTreeMap::new();
    for e in interactive::live(home) {
        let Some(hs) = bound(&e.session).filter(|hs| hs.interactive) else { continue };
        let repo = main_checkout(&e.worktree);
        for g in interactive::served(home, &hs, &queen::repo_name(&e.worktree)) {
            let s = out.entry(g.id.clone()).or_insert_with(|| Served { goal: g, drones: Vec::new(), repos: Vec::new() });
            s.drones.push((e.clone(), hs.clone()));
            if let Some(r) = repo.clone().filter(|r| !s.repos.contains(r)) {
                s.repos.push(r);
            }
        }
    }
    out
}

/// The checkout a drone's tree (`<repo>/.worktrees/hive-i-<drone>`) was made from: where its node
/// integrates (an integration in a drone's tree would be refused as a drone's, mod.rs `drone`). None for a
/// drone with no tree.
fn main_checkout(tree: &str) -> Option<PathBuf> {
    let t = Path::new(tree);
    let wt = t.parent().filter(|p| p.file_name().is_some_and(|n| n == ".worktrees"))?;
    wt.parent().map(Path::to_path_buf)
}

/// The goal work of `s` on this tick.
fn one(home: &Path, here: &str, s: &Served, pace: &mut Pace) {
    let id = s.goal.id.as_str();
    let path = tasks_path(home, id);
    let now = learn::now_ms();
    let mark = run::mark(&path);
    let grew = mark != pace.mark;
    pace.mark = mark;
    let events = (grew || now >= pace.next_beat || now - pace.looked >= LOOK_MS).then(|| board::read(&path));
    let Some(events) = events else { return };
    let b = board::fold_on(&events, here);
    // Never a join (phase g correction 2): a goal whose membership is not open (its drones' join not
    // here yet, or the node left with its last drone) is not beaten, and read again only on a change of
    // its board or after `LOOK_MS`, as an open one: a drone of every goal serves a goal opened after its
    // join with no join of its node there until it claims in it, maybe never.
    if !interactive::member_open(&b, here) {
        reap_child(pace, now);
        pace.looked = now;
        pace.next_beat = now + LOOK_MS;
        return;
    }
    let ack = grew.then(|| board::to_ack(&events, here)).flatten().filter(|m| *m > pace.acked);
    if now >= pace.next_beat || ack.is_some() {
        let _ = member(home, id, board::BEAT);
        pace.next_beat = now + super::BEAT_MS;
        pace.acked = pace.acked.max(ack.unwrap_or(i64::MIN));
    }
    if grew || now - pace.looked >= LOOK_MS {
        pace.looked = now;
        look(home, here, s, &b, pace, now);
    }
}

/// The goal's lines that are due (roster, bundles, finished, declines, stuck) and its integration.
fn look(home: &Path, here: &str, s: &Served, b: &Board, pace: &mut Pace, now: i64) {
    let (g, id) = (&s.goal, s.goal.id.as_str());
    let root = s.repos.first().map(PathBuf::as_path);
    if let Some(r) = root {
        super::commits::republish(home, id, b, here, r);
    }
    let takers = takers(b, here, &s.drones, &mut pace.taker_ms, now);
    if b.rosters.get(here).is_none_or(|(_, r)| *r != takers) {
        let _ = super::roster(home, id, &takers.into_iter().collect::<Vec<_>>());
    }
    if board::finish_due(b, here).is_some() {
        let _ = super::finish_line(home, id);
    }
    let names: Vec<String> = s.drones.iter().map(|(e, _)| queen::repo_name(&e.worktree)).collect();
    let approvals = super::oracle::Approvals::load(home, id);
    let allowed = |t: &board::TaskState| names.iter().any(|r| super::may_claim(g, &t.task, r)) && approvals.pending(g, &t.task).is_empty();
    let lacks = root.map(|r| super::lacking_here(home, id, b, r, now)).unwrap_or_default();
    let take = run::takes(&allowed, &lacks);
    if !board::declines_due(b, here, now, &take).is_empty() {
        let _ = super::decline_lines(home, id, &take);
    }
    if let Ending::Stuck(stuck) = Ending::of(b, here, now, allowed, &lacks) {
        let _ = super::stuck_lines(home, id, &stuck);
    }
    reap_child(pace, now);
    let Some(root) = root.filter(|_| pace.child.is_none() && now >= pace.integrate_after && integrate::due(b, now).is_some()) else { return };
    match integrate::elsewhere(home, g, root, now) {
        // Accepted work whose commits this node cannot get: said once, so no peer waits for it here
        // (mod.rs `decline_integration`, phase f F7); none can hold them together: stuck.
        Some((task, c, refused)) => {
            if refused && !b.declines(here, &task) {
                let _ = super::decline_integration(home, id, &task);
            }
            if let Commits::Unavailable(why) = c {
                let _ = super::stuck_lines(home, id, &[(task, why)]);
            }
        }
        None => match integrate_child(home, id, root) {
            Ok(c) => pace.child = Some(c),
            // Said in the log the child would have written, and not tried again within
            // `INTEGRATE_AGAIN_MS`, as a child that failed: the engineer sees why nothing integrates.
            Err(e) => {
                pace.integrate_after = now + INTEGRATE_AGAIN_MS;
                note_integrate(home, id, &format!("borg-ai: the node could not start `borg hive integrate {id}` ({}): {e}", borg_exe().display()));
            }
        },
    }
}

/// The drones of `drones` that take tasks of the goal at `now` (phase g correction 4): each holding a
/// claim on `b`, or waiting for work (its waiter runs, interactive.rs `waiting`) outside the engineer's
/// turn (`busy`), kept as `roster_of` keeps them.
fn takers(b: &Board, here: &str, drones: &[(Entry, HiveSession)], seen: &mut BTreeMap<String, i64>, now: i64) -> BTreeSet<String> {
    let now_taking = |e: &Entry, hs: &HiveSession| {
        board::holding(b, here, &e.session, now).is_some() || (interactive::waiting(&hs.drone) && !interactive::busy(&e.session, now))
    };
    let drones: Vec<(String, i64, bool)> = drones.iter().map(|(e, hs)| (hs.drone.clone(), e.ms, now_taking(e, hs))).collect();
    roster_of(seen, &drones, now)
}

/// The roster of takers at `now` from `drones`, (drone, the ms of its registry line, whether it takes
/// now): each that takes now, or did, or joined, within `TAKER_GRACE_MS` (`seen`, when each last did), so
/// a drone between two tasks, or one starting its first wait, keeps its place, and one that stays away
/// (the engineer's turn, no wait running) leaves it. A drone not in `drones` (gone from the registry) is
/// dropped at once. The worker's rule and the scenario's (scenario.rs `node_work`).
pub(crate) fn roster_of(seen: &mut BTreeMap<String, i64>, drones: &[(String, i64, bool)], now: i64) -> BTreeSet<String> {
    for (d, joined, taking) in drones {
        let at = seen.entry(d.clone()).or_insert(*joined);
        *at = if *taking { now } else { (*at).max(*joined) };
    }
    seen.retain(|d, at| now - *at <= TAKER_GRACE_MS && drones.iter().any(|(x, _, _)| x == d));
    seen.keys().cloned().collect()
}

/// A drone whose harness process is gone (interactive.rs `harness_gone`: the session crashed, its
/// terminal closed, its waiter was never started or died with it): it leaves as at its session's end, its
/// claim released `its session is gone` (interactive.rs `leave_noted`), its registry leave and, with its
/// node's last drone of a goal, the node's leave. A registry line whose session is no drone any more
/// (its membership file gone) is closed the same way (`forget`).
fn reap(home: &Path, e: &Entry, now: i64) {
    match bound(&e.session).filter(|hs| hs.interactive) {
        Some(hs) => {
            let cwd = main_checkout(&e.worktree).map_or_else(|| e.worktree.clone(), |p| p.display().to_string());
            let here = super::Here { home: home.to_path_buf(), cwd, now };
            let _ = interactive::leave_noted(&here, &e.session, &hs, "its session is gone");
        }
        None => interactive::forget(home, e, now),
    }
}

/// The goal's integration child, once it ended: not started again within `INTEGRATE_AGAIN_MS`.
fn reap_child(pace: &mut Pace, now: i64) {
    if pace.child.as_mut().is_some_and(|c| !matches!(c.try_wait(), Ok(None))) {
        pace.child = None;
        pace.integrate_after = now + INTEGRATE_AGAIN_MS;
    }
}

/// The `borg` binary: this process when it is one (a waiter), else the one beside it (the mesh daemon's
/// `borg-mesh`, installed together), else `borg` on PATH.
fn borg_exe() -> PathBuf {
    let me = super::reexec::exe_path();
    match me {
        p if p.file_stem().is_some_and(|s| s == "borg") => p,
        p if p.with_file_name("borg").is_file() => p.with_file_name("borg"),
        _ => PathBuf::from("borg"),
    }
}

/// `borg hive integrate <goal> --dir <repo>` as the node's child (integrate.rs `run`: the claim of the due
/// integration under the lock, final across nodes, fenced by its epoch), its output in the goal's
/// `logs/node-integrate.log`. A process of its own session (`setsid`), so neither a tick nor a signal to
/// the daemon's terminal stops a merge or its oracle midway; never as a drone (no harness session id, no
/// hive variables in its environment, its directory the checkout, not a drone's tree), so mod.rs `drone`
/// does not refuse it.
fn integrate_child(home: &Path, goal: &str, repo: &Path) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    let log = integrate_log(home, goal)?;
    let mut c = std::process::Command::new(borg_exe());
    c.args(["hive", "integrate", goal, "--dir", &repo.display().to_string()]).stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    for k in interactive::session_vars().into_iter().map(|(k, _)| k).chain([super::ENV, super::DRONE_ENV]) {
        c.env_remove(k);
    }
    // SAFETY: setsid(2) between fork and exec only makes the child its own session.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    c.spawn()
}

/// The node's integration log of a goal, in its `logs/` (`integrate_log`; monitoring shows its tail).
pub const INTEGRATE_LOG: &str = "node-integrate.log";

/// The goal's `logs/node-integrate.log`, opened to append.
fn integrate_log(home: &Path, goal: &str) -> std::io::Result<std::fs::File> {
    let logs = super::dir(home, goal).join("logs");
    std::fs::create_dir_all(&logs)?;
    std::fs::OpenOptions::new().create(true).append(true).open(logs.join(INTEGRATE_LOG))
}

/// `line` appended to the goal's `logs/node-integrate.log`, else on stderr (the daemon's or the waiter's).
fn note_integrate(home: &Path, goal: &str, line: &str) {
    use std::io::Write;
    if integrate_log(home, goal).and_then(|mut f| writeln!(f, "{line}")).is_err() {
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_served_goal_with_no_membership_here_is_read_once_a_look() {
        // A drone of every goal serves a goal opened after its join, with no join of this node there
        // (none until it claims in it): `one` reads its board on a change or once a `LOOK_MS`, not every
        // tick. Fails with the early return leaving the pace as it was (`looked` and `next_beat` 0: a
        // read and a fold every 25 ms tick).
        let home = std::env::temp_dir().join(format!("borg-node-unjoined-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let goal = Goal {
            v: 1,
            id: "g1".into(),
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
        super::super::goal::create(&home, &goal).unwrap();
        let s = Served { goal, drones: Vec::new(), repos: Vec::new() };
        let mut pace = Pace::default();
        let before = learn::now_ms();
        one(&home, "here", &s, &mut pace);
        assert!(pace.looked >= before && pace.next_beat >= before + LOOK_MS, "looked {} next {}", pace.looked, pace.next_beat);
        let looked = pace.looked;
        for _ in 0..5 {
            std::thread::sleep(TICK);
            one(&home, "here", &s, &mut pace);
        }
        assert_eq!(pace.looked, looked, "no read within a look of an unchanged board");
        let _ = std::fs::remove_dir_all(home);
    }
}
