//! A goal of interactive drones as monitoring shows it (phase g G6, docs/design/hive.md "Phase g"):
//! what `view.rs` adds to each goal's view for borg-web and `borg top`, read from the files the hive
//! keeps anyway and never written by a reader:
//! - the board's tasks by state over time (`history`), backfilled from the board itself: the fold of the
//!   lines stamped up to each point, so a page opened late shows the goal from its first open;
//! - each node of the goal (`members`): its membership (its join and leave lines), its last beat's age
//!   on this node's clock (when this node first held it, the beats store's `seen`, never the peer's stamp),
//!   its roster of takers, and for this node who does its goal work (node.rs `worker`: the mesh daemon or a
//!   drone's waiter, or none);
//! - each interactive drone of this node that served the goal (`drones`, from `hive/drones.local`): its
//!   harness, its state (`working` a task, `busy` in the engineer's turn, `waiting` with its waiter
//!   running, `not waiting`, or `gone`), its wakes (hand to take-up, from `confirms.jsonl`'s `taken`
//!   lines, and the wakes no turn took up, released on the board), the tasks it did and judged;
//! - the stuck and declines lines that stand (`stuck`, `declines`), each with its reason;
//! - every integration of the goal (`integrations`) and the tail of the node's integration log;
//! - spend against the budget split into metered and unmetered claims (`Spend::usd` none).
//!
//! A store with no registry and no interactive line pays one failed open per file; a goal of `borg hive
//! run` drones shows its history and integrations, the rest stays empty.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use super::interactive::{self, Entry};
use super::run::Spend;
use super::{Board, Ev, Event, Role, Status, board};

/// The most points a goal's history carries: a long goal's lines are sampled evenly.
pub const POINTS: usize = 60;
/// The wakes a drone's row carries (the newest).
pub const WAKES: usize = 30;
/// The lines of the node's integration log a view carries (the newest).
pub const LOG_LINES: usize = 20;

/// The board at one moment: its tasks by state (a stuck task counts as stuck, whatever its status).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Point {
    pub ms: i64,
    pub open: usize,
    pub claimed: usize,
    pub done: usize,
    pub verified: usize,
    pub blocked: usize,
    pub stuck: usize,
}

/// One node of the goal as this node sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NodeRow {
    /// Its id's first 8 characters.
    pub node: String,
    /// This node.
    pub here: bool,
    /// `in` (a join newer than its leave), `left`, or `not joined`.
    pub state: String,
    /// The stamps of its newest join and leave; 0 for none.
    pub joined_ms: i64,
    pub left_ms: i64,
    /// How long ago this node first held its newest beat, on this node's clock; None without a beat.
    pub beat_age_ms: Option<i64>,
    /// How long ago this node first held its newest line of any kind; None without one.
    pub line_age_ms: Option<i64>,
    /// Its roster of takers (its newest roster line).
    pub takers: Vec<String>,
    /// This node's: who does its goal work now (`daemon`, `waiter <drone>`, or `none`); empty for a peer.
    pub worker: String,
}

/// One wake of a drone: the claim a waiter handed it, and when a turn of its session took it up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WakeRow {
    pub task: String,
    /// When it was taken up, or released for want of a turn.
    pub ms: i64,
    /// From its hand to its take-up, on this node's clock; 0 for a wake no turn took up.
    pub latency_ms: i64,
    pub taken: bool,
}

/// One interactive drone of this node that served the goal.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DroneRow {
    pub drone: String,
    pub harness: String,
    /// Its session now (the first 8 characters), or the last one before it left.
    pub session: String,
    /// `working`, `busy`, `waiting`, `not waiting` or `gone`.
    pub state: String,
    /// The task it holds while `working`.
    pub task: String,
    /// Its newest registry line's stamp: its join, rebind or leave.
    pub since_ms: i64,
    /// Work tasks it finished (its claims that ended in a done), and tasks it judged (a verdict).
    pub done: usize,
    pub judged: usize,
    /// Its newest wakes, oldest first.
    pub wakes: Vec<WakeRow>,
    /// What its claims of the goal were charged, and whether one of them was unmetered.
    pub usd: f64,
    pub unmetered: bool,
}

/// A stuck line that stands, or a node's decline of a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineRow {
    pub task: String,
    /// Its node's first 8 characters.
    pub node: String,
    pub ms: i64,
    pub why: String,
}

/// One integration of the goal: its integrate task and how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegrationRow {
    pub task: String,
    /// open, claimed, verified, blocked, ...
    pub status: String,
    /// `integrated <commit>`, the verifier's `rejected: …`, `blocked: …`, or empty while it runs.
    pub result: String,
    pub opened_ms: i64,
}

/// What monitoring adds to a goal's view.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Monitor {
    pub history: Vec<Point>,
    pub members: Vec<NodeRow>,
    pub interactive: Vec<DroneRow>,
    pub stuck: Vec<LineRow>,
    pub declines: Vec<LineRow>,
    pub integrations: Vec<IntegrationRow>,
    pub integrate_log: Vec<String>,
    /// What the goal's spend lines say they cost (`Spend::usd`), and how many named no cost: an
    /// interactive claim whose harness reports no usage, or a runner's iteration charged its estimate.
    pub metered_usd: f64,
    pub unmetered: usize,
}

fn short(s: &str) -> String {
    s.chars().take(8).collect()
}

/// The board's state counts at the end of `events` (each already stamped up to the point).
fn point(events: &[Event], here: &str, ms: i64) -> Point {
    let b = board::fold_on(events, here);
    let mut p = Point { ms, ..Point::default() };
    for (id, s) in &b.tasks {
        let n = match &s.status {
            _ if b.stuck.contains_key(id) => &mut p.stuck,
            Status::Open => &mut p.open,
            Status::Claimed { .. } => &mut p.claimed,
            Status::Done { .. } => &mut p.done,
            Status::Verified { .. } => &mut p.verified,
            Status::Blocked { .. } => &mut p.blocked,
        };
        *n += 1;
    }
    p
}

/// The goal's tasks by state over time: at each distinct stamp of its lines (beats aside), at most
/// `POINTS` of them evenly spaced from the first line to the last, the fold of every line stamped up to
/// it. The last point is the board's state now (every line held).
pub fn history(events: &[Event], here: &str) -> Vec<Point> {
    let mut lines: Vec<Event> = events.iter().filter(|e| !board::is_beat(e)).cloned().collect();
    lines.sort_by_key(|e| e.ms);
    let stamps: Vec<i64> = lines.iter().map(|e| e.ms).collect::<BTreeSet<_>>().into_iter().collect();
    let (Some(&first), Some(&last)) = (stamps.first(), stamps.last()) else { return Vec::new() };
    let at: Vec<i64> = if stamps.len() <= POINTS {
        stamps
    } else {
        let span = (last - first) as f64;
        (0..POINTS).map(|i| first + (span * i as f64 / (POINTS - 1) as f64).round() as i64).collect::<BTreeSet<_>>().into_iter().collect()
    };
    at.into_iter().map(|t| point(&lines[..lines.partition_point(|e| e.ms <= t)], here, t)).collect()
}

/// The nodes of goal `b` on this node `here` at `now`: every node with a membership, roster or beat line,
/// and this one; this node first.
pub fn members(home: &Path, b: &Board, events: &[Event], here: &str, now: i64) -> Vec<NodeRow> {
    let mut beats: BTreeMap<String, i64> = BTreeMap::new();
    for e in events.iter().filter(|e| board::is_beat(e)) {
        let seen = e.seen.0.unwrap_or(e.ms);
        let n = if e.node.is_empty() { here.to_string() } else { e.node.clone() };
        let at = beats.entry(n).or_insert(seen);
        *at = (*at).max(seen);
    }
    let mut nodes: BTreeSet<String> = b.members.keys().chain(b.rosters.keys()).chain(beats.keys()).filter(|n| !n.is_empty()).cloned().collect();
    nodes.insert(here.to_string());
    let worker = super::node::worker(home);
    let mut rows: Vec<NodeRow> = nodes
        .into_iter()
        .map(|n| {
            let (join, leave) = b.members.get(&n).copied().unwrap_or((i64::MIN, i64::MIN));
            let state = if join > leave {
                "in"
            } else if leave > i64::MIN {
                "left"
            } else {
                "not joined"
            };
            let stamp = |m: i64| if m == i64::MIN { 0 } else { m };
            NodeRow {
                node: short(&n),
                here: n == here,
                state: state.into(),
                joined_ms: stamp(join),
                left_ms: stamp(leave),
                beat_age_ms: beats.get(&n).map(|s| (now - s).max(0)),
                line_age_ms: b.nodes.get(&n).map(|s| (now - s).max(0)),
                takers: b.rosters.get(&n).map(|(_, r)| r.iter().cloned().collect()).unwrap_or_default(),
                worker: if n == here { worker.clone() } else { String::new() },
            }
        })
        .collect();
    rows.sort_by_key(|r| (!r.here, r.node.clone()));
    rows
}

/// The interactive drones of this node that served goal `goal` (any registry line of theirs serves it,
/// or a claim of theirs is on its board), with their state at `now`, gone ones last.
pub fn drones(home: &Path, goal: &str, b: &Board, events: &[Event], spend: &[Spend], here: &str, now: i64) -> Vec<DroneRow> {
    let lines: Vec<Entry> = super::read_lines(&interactive::registry_path(home));
    if lines.is_empty() {
        return Vec::new();
    }
    let live: BTreeMap<String, Entry> = interactive::live(home).into_iter().map(|e| (e.drone.clone(), e)).collect();
    let mine = |n: &str| super::is_local(home, n);
    // Each drone's sessions over its life (a `/clear` rebinds it), and its newest line.
    let mut sessions: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut newest: BTreeMap<String, Entry> = BTreeMap::new();
    let mut serves: BTreeSet<String> = BTreeSet::new();
    for e in &lines {
        sessions.entry(e.drone.clone()).or_default().insert(e.session.clone());
        if e.by != "leave" && e.serves(goal) {
            serves.insert(e.drone.clone());
        }
        if newest.get(&e.drone).is_none_or(|n| n.ms <= e.ms) {
            newest.insert(e.drone.clone(), e.clone());
        }
    }
    serves.extend(b.drones.iter().filter(|((n, _), d)| mine(n) && newest.contains_key(*d)).map(|(_, d)| d.clone()));
    let confirms: Vec<super::Confirm> = super::read_lines(&super::confirms_path(home, goal));
    // A line the sync delivered twice counts once, as in the ledger (run.rs `distinct`).
    let spend = super::run::distinct(spend);
    let mut rows: Vec<DroneRow> = serves
        .into_iter()
        .filter_map(|drone| {
            let last = newest.get(&drone)?;
            let ss = sessions.get(&drone).cloned().unwrap_or_default();
            let of = |by: &str, node: &str| ss.contains(by) && mine(node);
            // The harness its join named: the leave line names none.
            let harness = lines.iter().rev().find(|e| e.drone == drone && !e.harness.is_empty()).map(|e| e.harness.clone()).unwrap_or_default();
            let session = lines.iter().rev().find(|e| e.drone == drone && e.by != "leave").map_or_else(|| last.session.clone(), |e| e.session.clone());
            let (state, task) = match live.get(&drone) {
                None => ("gone", String::new()),
                Some(e) if interactive::harness_gone(e) || super::bound(&e.session).is_none_or(|hs| !hs.interactive) => ("gone", String::new()),
                Some(e) => match board::holding(b, here, &e.session, now) {
                    Some(t) => ("working", t.task.id.clone()),
                    None if interactive::busy(&e.session, now) => ("busy", String::new()),
                    None if interactive::waiting(&drone) => ("waiting", String::new()),
                    None => ("not waiting", String::new()),
                },
            };
            let ended = |ev: Ev| b.spans.iter().filter(|s| s.end == Some(ev) && of(&s.by, &s.node)).count();
            let done = ended(Ev::Done);
            let judged = ended(Ev::Verified) + ended(Ev::Rejected);
            let mut wakes: Vec<WakeRow> = confirms
                .iter()
                .filter(|c| c.outcome == super::TAKEN && ss.contains(&c.session))
                .map(|c| WakeRow { task: c.task.clone(), ms: c.ms, latency_ms: c.waited_ms as i64, taken: true })
                .chain(events.iter().filter(|e| e.ev == Ev::Released && e.note == interactive::NOT_TAKEN_UP && of(&e.by, &e.node)).map(|e| WakeRow {
                    task: e.task.clone(),
                    ms: e.ms,
                    latency_ms: 0,
                    taken: false,
                }))
                .collect();
            wakes.sort_by_key(|w| w.ms);
            let wakes = wakes[wakes.len().saturating_sub(WAKES)..].to_vec();
            let own: Vec<&Spend> = spend.iter().copied().filter(|s| s.drone == drone && !s.claim.is_empty() && mine(&s.node)).collect();
            Some(DroneRow {
                harness,
                session: short(&session),
                state: state.into(),
                task,
                since_ms: last.ms,
                done,
                judged,
                wakes,
                usd: own.iter().map(|s| s.charged()).sum(),
                unmetered: own.iter().any(|s| s.usd.is_none()),
                drone,
            })
        })
        .collect();
    rows.sort_by_key(|r| (r.state == "gone", r.drone.clone()));
    rows
}

/// The stuck lines of `b` that stand, and the declines that stand, each with its reason: a stuck line
/// says its own; a decline stands while it is newer than its node's newest join (`Board::declines`, what
/// the scheduler reads) and its task is not verified, so a node that rejoined since, or a task done for
/// good, shows none. A decline says no reason (its node's repo scope and approvals are known there
/// alone), so this node's own declines are given the reason it reads here, and a peer's says where it is
/// known.
pub fn lines(home: &Path, g: &super::Goal, b: &Board, here: &str) -> (Vec<LineRow>, Vec<LineRow>) {
    let node = |n: &str| if super::is_local(home, n) { short(here) } else { short(n) };
    let stuck = b.stuck.iter().map(|(t, (ms, n, why))| LineRow { task: t.clone(), node: node(n), ms: *ms, why: why.clone() }).collect();
    let repos: Vec<String> = interactive::live(home).iter().filter(|e| e.serves(&g.id)).map(|e| crate::queen::repo_name(&e.worktree)).collect();
    let approvals = super::oracle::Approvals::load(home, &g.id);
    let mut declines: Vec<LineRow> = b
        .declined
        .iter()
        .filter(|((n, t), _)| b.declines(n, t) && !b.tasks.get(t).is_some_and(|s| matches!(s.status, Status::Verified { .. })))
        .map(|((n, t), ms)| {
            let why = match b.tasks.get(t) {
                _ if !super::is_local(home, n) => format!("known on node {} alone", short(n)),
                Some(s) if s.task.role == Role::Integrate => "the accepted work's commits are not here".to_string(),
                Some(s) if !approvals.pending(g, &s.task).is_empty() => format!("its checks wait for `borg hive approve {}`", g.id),
                Some(s) if !repos.iter().any(|r| super::may_claim(g, &s.task, r)) => "no drone here works in its repo".to_string(),
                Some(_) => "its commits are not here".to_string(),
                None => String::new(),
            };
            LineRow { task: t.clone(), node: node(n), ms: *ms, why }
        })
        .collect();
    declines.sort_by(|a, b| (&a.task, &a.node).cmp(&(&b.task, &b.node)));
    (stuck, declines)
}

/// Every integrate task of `b`, oldest first, with how it ended.
pub fn integrations(b: &Board) -> Vec<IntegrationRow> {
    let mut rows: Vec<IntegrationRow> = b
        .tasks
        .values()
        .filter(|s| s.task.role == Role::Integrate)
        .map(|s| {
            let passed = b.integrations.iter().find(|i| i.task == s.task.id);
            let result = match (&s.status, passed) {
                (_, Some(i)) => format!("integrated {}", i.head),
                (Status::Blocked { note, .. }, _) => format!("blocked: {note}"),
                (Status::Verified { .. }, _) if !s.last_progress.is_empty() => s.last_progress.clone(),
                _ => String::new(),
            };
            IntegrationRow { task: s.task.id.clone(), status: s.status.name().into(), result, opened_ms: s.opened_ms }
        })
        .collect();
    rows.sort_by_key(|r| (r.opened_ms, r.task.clone()));
    rows
}

/// The newest `LOG_LINES` lines of the goal's node integration log (node.rs `integrate_log`).
pub fn integrate_log(home: &Path, goal: &str) -> Vec<String> {
    let text = std::fs::read_to_string(super::dir(home, goal).join("logs").join(super::node::INTEGRATE_LOG)).unwrap_or_default();
    let lines: Vec<String> = text.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect();
    lines[lines.len().saturating_sub(LOG_LINES)..].to_vec()
}

/// What monitoring adds to goal `g`'s view at `now` (`events` its board and beats, `b` their fold here).
pub fn of(home: &Path, g: &super::Goal, events: &[Event], b: &Board, spend: &[Spend], now: i64) -> Monitor {
    let here = super::node(home);
    let (stuck, declines) = lines(home, g, b, &here);
    Monitor {
        history: history(events, &here),
        members: members(home, b, events, &here, now),
        interactive: drones(home, &g.id, b, events, spend, &here, now),
        stuck,
        declines,
        integrations: integrations(b),
        integrate_log: integrate_log(home, &g.id),
        metered_usd: super::run::distinct(spend).iter().filter_map(|s| s.usd).map(|u| u.max(0.0)).sum(),
        unmetered: super::run::distinct(spend).iter().filter(|s| s.usd.is_none()).count(),
    }
}

/// What stops an interactive goal's integration on this node: it is due, a drone of this node serves
/// the goal, and neither a node worker (the mesh daemon, a drone's waiter) nor a runner runs here, so
/// nothing integrates until one does (phase g spec section 4).
pub fn no_worker(home: &Path, goal: &str, b: &Board, now: i64) -> Option<String> {
    let serving = interactive::live(home).iter().any(|e| e.serves(goal));
    (serving && super::integrate::due(b, now).is_some() && !super::node::worker_runs(home) && !interactive::runner_runs(home, goal))
        .then(|| "integration due, no node worker: start `borg mesh serve`, or a drone's wait runs it".to_string())
}

#[cfg(test)]
mod tests {
    use super::super::{Goal, HiveSession, Task, goal, tasks_path};
    use super::*;
    use crate::learn;

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("borg-hive-monitor-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn task(id: &str, role: Role) -> Task {
        Task {
            id: id.into(),
            role,
            title: format!("do {id}"),
            repo: String::new(),
            files: vec![format!("{id}.txt")],
            deps: vec![],
            accept: vec![],
            of: None,
            not_by: vec![],
            flags: vec![],
            from: None,
            group: None,
        }
    }

    fn seed(home: &Path) -> Goal {
        let g = Goal {
            v: 1,
            id: "g".into(),
            spec: "Ship it".into(),
            oracle: vec![goal::Check::parse("ok=true", 60).unwrap()],
            budget_usd: 3.0,
            repos: vec![],
            author: "e@x".into(),
            created_ms: 1,
            sig: String::new(),
            hidden: None,
            held_out: false,
        };
        goal::create(home, &g).unwrap();
        g
    }

    fn fold(home: &Path) -> (Vec<Event>, Board) {
        let events = board::read(&tasks_path(home, "g"));
        let b = board::fold_on(&events, &super::super::node(home));
        (events, b)
    }

    /// The history is the board's own past: each point folds the lines stamped up to it, so a page
    /// opened at the end still shows the goal from its first open; beats are no points, and a long
    /// goal is sampled to `POINTS` with its last point the board now.
    #[test]
    fn the_history_is_backfilled_from_the_board() {
        let home = temp("history");
        seed(&home);
        let path = tasks_path(&home, "g");
        board::append(&path, &[Event::open(task("t1", Role::Work), "engineer:e", 10), Event::open(task("t2", Role::Work), "engineer:e", 10)]).unwrap();
        board::append(&path, &[Event::new(Ev::Claim, "t1", "s1", 20)]).unwrap();
        board::append(&path, &[Event::new(Ev::Done, "t1", "s1", 30)]).unwrap();
        super::super::member(&home, "g", board::BEAT).unwrap();
        let (events, _) = fold(&home);
        let h = history(&events, &super::super::node(&home));
        let at = |p: &Point| (p.ms, p.open, p.claimed, p.done);
        assert_eq!(h.iter().map(at).collect::<Vec<_>>(), [(10, 2, 0, 0), (20, 1, 1, 0), (30, 1, 0, 1)], "{h:?}");
        let many: Vec<Event> = (0..200).map(|i| Event::open(task(&format!("x{i}"), Role::Work), "engineer:e", 100 + i)).collect();
        board::append(&path, &many).unwrap();
        let (events, b) = fold(&home);
        let h = history(&events, &super::super::node(&home));
        assert!(h.len() <= POINTS && h.len() > POINTS / 2, "{}", h.len());
        assert_eq!((h[0].ms, h.last().unwrap().ms, h.last().unwrap().open), (10, 299, b.tasks.len() - 1));
        let _ = std::fs::remove_dir_all(home);
    }

    /// Each node shows its membership, its last beat's age on this node's clock (when it was held here,
    /// whatever the peer stamped) and its roster; this node shows who does its goal work.
    #[test]
    fn nodes_show_membership_beat_age_and_the_worker() {
        let home = temp("nodes");
        seed(&home);
        let path = tasks_path(&home, "g");
        board::append(&path, &[Event::open(task("t1", Role::Work), "engineer:e", 10)]).unwrap();
        super::super::member(&home, "g", board::JOIN).unwrap();
        super::super::member(&home, "g", board::BEAT).unwrap();
        let runner = |note: &str, ms: i64| Event { node: "remote-abcdef12".into(), ..Event::new(Ev::Heartbeat, "", "runner", ms).with_note(note) };
        board::append(&path, &[runner(board::JOIN, 20), runner(&board::roster_note(&["d1".into()]), 21), runner(board::LEAVE, 30)]).unwrap();
        // A peer's beat stamped an hour ahead of this clock, held here 4 s ago: its age is 4 s.
        let now = learn::now_ms();
        let beat = Event { node: "remote-abcdef12".into(), ..Event::new(Ev::Heartbeat, "", "runner", now + 3_600_000).with_note(board::BEAT) };
        let rec = board::BeatRecord { line: serde_json::to_string(&beat).unwrap(), seen: now - 4_000, key: String::new(), sig: String::new() };
        board::merge_beats(&path, vec![rec], |_| true).unwrap();
        let (events, b) = fold(&home);
        let here = super::super::node(&home);
        let rows = members(&home, &b, &events, &here, now);
        assert_eq!(rows.len(), 2, "{rows:?}");
        let (mine, peer) = (&rows[0], &rows[1]);
        assert!(mine.here && mine.state == "in" && mine.beat_age_ms.is_some_and(|a| a < 60_000), "{mine:?}");
        assert_eq!(mine.worker, "none");
        assert_eq!((peer.node.as_str(), peer.state.as_str(), peer.beat_age_ms, peer.takers.clone()), ("remote-a", "left", Some(4_000), vec!["d1".to_string()]));
        assert_eq!((peer.joined_ms, peer.left_ms, peer.worker.as_str()), (20, 30, ""));
        let lock = super::super::node::take_as(&home, "waiter i-abc123").unwrap();
        assert_eq!(members(&home, &b, &events, &here, now)[0].worker, "waiter i-abc123");
        drop(lock);
        let lock = super::super::node::take(&home).unwrap();
        assert_eq!(super::super::node::worker(&home), "daemon");
        drop(lock);
        assert_eq!(super::super::node::worker(&home), "none", "a lock no process holds names no worker");
        let _ = std::fs::remove_dir_all(home);
    }

    /// A drone of this node shows its harness and state: working its task, busy in the engineer's turn,
    /// waiting with its waiter running, not waiting, gone once it left; its wakes (hand to take-up, and
    /// the wake no turn took up), the tasks it did and what its claims were charged.
    #[test]
    fn drones_show_their_state_wakes_and_work() {
        let home = temp("drones");
        let g = seed(&home);
        let tag = format!("mon{}", std::process::id());
        let s = |n: &str| format!("{tag}-{n}");
        let now = learn::now_ms();
        let reg = interactive::registry_path(&home);
        for (d, h) in [("i-work", "claude"), ("i-busy", "codex"), ("i-wait", "omp"), ("i-idle", "opencode"), ("i-gone", "claude")] {
            let e =
                Entry { v: 1, ms: now - 50_000, drone: d.into(), session: s(d), harness: h.into(), goal: "g".into(), by: "join".into(), ..Entry::default() };
            super::super::append_line(&reg, &e).unwrap();
            let hs = HiveSession { goal: "g".into(), drone: d.into(), interactive: true, ..HiveSession::default() };
            std::fs::write(super::super::session_path(&s(d)), serde_json::to_string(&hs).unwrap()).unwrap();
        }
        super::super::append_line(&reg, &Entry { v: 1, ms: now - 1_000, drone: "i-gone".into(), session: s("i-gone"), by: "leave".into(), ..Entry::default() })
            .unwrap();
        let busy = crate::hook::state_file(&s("i-busy")).with_extension("hive-busy");
        std::fs::write(&busy, format!("0 {now} 300000")).unwrap();
        let waiter = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(interactive::waiter_path("i-wait")).unwrap();
        waiter.lock().unwrap();
        let _ = std::fs::remove_file(interactive::waiter_path("i-idle"));
        let ev = |e: Ev, t: &str, d: &str, ms: i64| Event::new(e, t, &s(d), ms).with_drone(d);
        super::super::write(
            &home,
            "g",
            vec![
                Event::open(task("t1", Role::Work), "engineer:e", now - 40_000),
                Event::open(task("t2", Role::Work), "engineer:e", now - 40_000),
                Event::open(task("t3", Role::Work), "engineer:e", now - 40_000),
            ],
        )
        .unwrap();
        super::super::write(&home, "g", vec![ev(Ev::Claim, "t2", "i-work", now - 30_000)]).unwrap();
        super::super::write(&home, "g", vec![ev(Ev::Done, "t2", "i-work", now - 20_000)]).unwrap();
        super::super::write(&home, "g", vec![ev(Ev::Claim, "t3", "i-wait", now - 15_000)]).unwrap();
        super::super::write(&home, "g", vec![Event::new(Ev::Released, "t3", &s("i-wait"), now - 14_000).with_note(interactive::NOT_TAKEN_UP)]).unwrap();
        super::super::write(&home, "g", vec![ev(Ev::Claim, "t1", "i-work", now - 3_000)]).unwrap();
        // The take-up of the claim of t1, through the hooks' own step (`touch` of a prompt).
        interactive::handed(&s("i-work"), "g", &format!("t1@{}", now - 3_000), now - 2_900);
        let mut hs = super::super::bound(&s("i-work")).unwrap();
        interactive::touch(&home, "prompt", &mut hs, &s("i-work"), "claude", &serde_json::json!({}), now - 1_500);
        let spend = vec![
            Spend { v: 1, ms: now, drone: "i-work".into(), usd: Some(0.25), exit: "interactive".into(), claim: "t2@1".into(), ..Spend::default() },
            Spend { v: 1, ms: now, drone: "i-wait".into(), usd: None, exit: "unmetered".into(), claim: "t3@1".into(), ..Spend::default() },
        ];
        // The sync delivered both lines twice: each counts once, as in the ledger.
        let spend: Vec<Spend> = spend.iter().chain(&spend).cloned().collect();
        let (events, b) = fold(&home);
        let here = super::super::node(&home);
        let rows = drones(&home, "g", &b, &events, &spend, &here, now);
        let states: Vec<(&str, &str, &str, &str)> = rows.iter().map(|r| (r.drone.as_str(), r.harness.as_str(), r.state.as_str(), r.task.as_str())).collect();
        assert_eq!(
            states,
            [
                ("i-busy", "codex", "busy", ""),
                ("i-idle", "opencode", "not waiting", ""),
                ("i-wait", "omp", "waiting", ""),
                ("i-work", "claude", "working", "t1"),
                ("i-gone", "claude", "gone", "")
            ]
        );
        let work = rows.iter().find(|r| r.drone == "i-work").unwrap();
        assert_eq!((work.done, work.usd, work.unmetered), (1, 0.25, false));
        assert_eq!(work.wakes, [WakeRow { task: "t1".into(), ms: now - 1_500, latency_ms: 1_400, taken: true }], "hand to take-up");
        let wait = rows.iter().find(|r| r.drone == "i-wait").unwrap();
        assert_eq!((wait.wakes.len(), wait.wakes[0].taken, wait.unmetered), (1, false, true), "a wake no turn took up");
        let m = of(&home, &g, &events, &b, &spend, now);
        assert_eq!((m.metered_usd, m.unmetered), (0.25, 1));
        assert_eq!(m.metered_usd, super::super::run::ledger(&spend, &here).total, "the page's metered spend is the ledger's");
        let text = super::super::view::render(&super::super::view::of(&home, "g", now).unwrap());
        assert!(
            text.contains("\n  DRONE      HARNESS   STATE        TASK       DONE  WAKE\n")
                && text.contains("\n  i-work     claude    working      t1            1  1."),
            "{text}"
        );
        drop(waiter);
        for d in ["i-work", "i-busy", "i-wait", "i-idle", "i-gone"] {
            let _ = std::fs::remove_file(super::super::session_path(&s(d)));
            let _ = std::fs::remove_file(crate::hook::state_file(&s(d)).with_extension("hive-busy"));
        }
        let _ = std::fs::remove_file(busy);
        let _ = std::fs::remove_dir_all(home);
    }

    /// Stuck lines say their reason; this node's decline is given the reason it reads here, a peer's
    /// says where it is known; each integration shows how it ended; with an integration due and no
    /// worker, the goal says so.
    #[test]
    fn stuck_declines_integrations_and_a_missing_worker_show() {
        let home = temp("lines");
        let g = seed(&home);
        let path = tasks_path(&home, "g");
        let now = learn::now_ms();
        board::append(&path, &[Event::open(task("t1", Role::Work), "engineer:e", 10), Event::open(task("t2", Role::Work), "engineer:e", 10)]).unwrap();
        super::super::member(&home, "g", board::JOIN).unwrap();
        super::super::stuck_lines(&home, "g", &[("t1".into(), "no participant may take it".into())]).unwrap();
        let line = |note: String| Event::new(Ev::Heartbeat, "", "runner", now).with_note(&note);
        super::super::write(&home, "g", vec![line(board::decline_note("t2"))]).unwrap();
        board::append(&path, &[Event { node: "remote-abcdef12".into(), ..line(board::decline_note("t1")) }]).unwrap();
        let (_, b) = fold(&home);
        let (stuck, declines) = lines(&home, &g, &b, &super::super::node(&home));
        assert_eq!((stuck.len(), stuck[0].task.as_str(), stuck[0].why.as_str()), (1, "t1", "no participant may take it"), "{stuck:?}");
        let why: Vec<(&str, &str)> = declines.iter().map(|d| (d.task.as_str(), d.why.as_str())).collect();
        assert_eq!(why, [("t1", "known on node remote-a alone"), ("t2", "no drone here works in its repo")], "{declines:?}");
        // The peer rejoined since its decline: it no longer stands, for the scheduler and the page alike.
        let rejoin = Event { node: "remote-abcdef12".into(), ..Event::new(Ev::Heartbeat, "", "runner", now + 1).with_note(board::JOIN) };
        board::append(&path, &[rejoin]).unwrap();
        let (_, b) = fold(&home);
        assert!(!b.declines("remote-abcdef12", "t1"));
        let (_, declines) = lines(&home, &g, &b, &super::super::node(&home));
        assert_eq!(declines.iter().map(|d| d.task.as_str()).collect::<Vec<_>>(), ["t2"], "{declines:?}");

        let it = |id: &str| Task { role: Role::Integrate, files: vec![], ..task(id, Role::Integrate) };
        board::append(&path, &[Event::open(it("integrate.1"), "integrate-d1-1", 20), Event::new(Ev::Claim, "integrate.1", "integrate-d1-1", 21)]).unwrap();
        board::append(&path, &[Event::new(Ev::Rejected, "integrate.1", "integrate-d1-1", 22).with_note("unit fails")]).unwrap();
        board::append(&path, &[Event::open(it("integrate.2"), "integrate-d1-2", 30), Event::new(Ev::Claim, "integrate.2", "integrate-d1-2", 31)]).unwrap();
        board::append(&path, &[Event::new(Ev::Blocked, "integrate.2", "integrate-d1-2", 32).with_note("t2 has no snapshot")]).unwrap();
        let (_, b) = fold(&home);
        let rows: Vec<(String, String)> = integrations(&b).into_iter().map(|r| (r.task, r.result)).collect();
        assert_eq!(rows, [("integrate.1".into(), "rejected: unit fails".into()), ("integrate.2".into(), "blocked: t2 has no snapshot".into())]);
        std::fs::create_dir_all(super::super::dir(&home, "g").join("logs")).unwrap();
        let log: String = (0..30).map(|i| format!("line {i}\n")).collect();
        std::fs::write(super::super::dir(&home, "g").join("logs").join(super::super::node::INTEGRATE_LOG), log).unwrap();
        assert_eq!(integrate_log(&home, "g").len(), LOG_LINES);
        assert_eq!(integrate_log(&home, "g").last().map(String::as_str), Some("line 29"));

        // Every work task verified, a drone of this node serving the goal: the integration is due.
        let fresh = temp("due");
        seed(&fresh);
        let p = tasks_path(&fresh, "g");
        board::append(
            &p,
            &[Event::open(task("t1", Role::Work), "engineer:e", 10), Event::new(Ev::Claim, "t1", "s1", 11), Event::new(Ev::Done, "t1", "s1", 12)],
        )
        .unwrap();
        let v = Task { of: Some("t1".into()), ..task("t1.v1", Role::Verify) };
        board::append(&p, &[Event::open(v, "s1", 12), Event::new(Ev::Claim, "t1.v1", "v1", 13), Event::new(Ev::Verified, "t1.v1", "v1", 14)]).unwrap();
        let (_, b) = fold(&fresh);
        assert_eq!(no_worker(&fresh, "g", &b, now), None, "no drone here: a runner's goal, not this alert");
        let e = Entry { v: 1, ms: now, drone: "i-x".into(), session: "s-x".into(), goal: "g".into(), by: "join".into(), ..Entry::default() };
        super::super::append_line(&interactive::registry_path(&fresh), &e).unwrap();
        assert!(super::super::integrate::due(&b, now).is_some(), "{:?}", b.tasks);
        assert!(no_worker(&fresh, "g", &b, now).is_some_and(|a| a.starts_with("integration due, no node worker")));
        let lock = super::super::node::take(&fresh).unwrap();
        assert_eq!(no_worker(&fresh, "g", &b, now), None, "the daemon runs it");
        drop(lock);
        let _ = std::fs::remove_dir_all(fresh);
        let _ = std::fs::remove_dir_all(home);
    }
}
