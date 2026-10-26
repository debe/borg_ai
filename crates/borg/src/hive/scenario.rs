//! Scenario tests of the hive's coordination on the real board code (phase f F0, tasks/hive-soundness.md):
//! hivemc.py's scenarios (tasks/soundness-model/run.py A..L) and the review's counterexamples (CE1..CE5,
//! G), run on `board::fold` and the predicates the runner and the claim read instead of the model's
//! re-implementation of them.
//!
//! A small deterministic simulation: each node holds the lines it received and folds them at its own
//! clock (`skew`, plus its index in ms, so two nodes never stamp one ms unless a scenario says so). A
//! line reaches the other nodes `delay` ticks after its write; a `Hold` keeps a chosen line back
//! longer, from the others or from its own node's file too. Each drone runs run.rs's loop
//! (`integrate::due`, `board::free_for`, `run::Ending`, `run::step`, `run::stalls`, the session-start and
//! session-end lines, a roster line at each drone's end, the finished and stuck lines of phase f F6) and
//! each session claims as mod.rs `claim_cmd`
//! and `claim` do (`board::next_for`, the claim checked under its node's board lock, the read-back),
//! writing real `Event` lines. Each runner joins the goal with its roster, beats every `BEAT_MS` (a
//! line no trace shows) and leaves once its drones ended (phase f F4); a killed one just stops. A
//! claim's lock is modelled as the code's (hive/mod.rs `write_if`): one
//! node's appends are serialized, and a claim is appended only when its check (mod.rs `claimable_for`,
//! integrate.rs `due`) holds on its node's file as it stands when it lands, after the lines its node
//! wrote before it (earlier in its tick, or while a `Hold` kept it back); else it is refused unwritten. As in hivemc.py, one tick is one second; within a tick every drone reads
//! its node's board as it stood at the tick's start and its lines land at the tick's end; a claim's
//! read-back is read at the next tick and it waits from there until it is final (phase f F5, mod.rs
//! `across`: `Board::unconfirmed` empty on its node's fold; each runner acks a claim of another node in
//! the tick it holds it, `board::to_ack`, as run.rs `membership` does), the work's hooks at every tick (a
//! loser stops at its first tick after it learns), and a session takes one task.
//!
//! Where the harness is not the run: no git, oracle, approvals or harness process (an iteration costs
//! 1 of a node's `share`; every check passes, a verifier of a verify task `reject` names rejects);
//! `open_to`'s `allowed` is `may_claim` (no integrate task); the data plane is a model of commits.rs
//! (phase f F7): a done's node holds its commits, and its bundle line (or, without `bundles` or on a node
//! whose commits `stays`, its line saying they stay there) reaches the other nodes `bundle_late` ticks
//! after the done would, or is written by its runner's next pass when the done's session was cut first
//! (`unpublished`), each node reading it through mod.rs `lacking` and integrate.rs `gate` as the run does
//! (an unbundle succeeds but on a node that `refuses` lines); every
//! node writes watermarks (F5), so no claim takes the confirm window's capped fallback; an integration's
//! ref is a per-node token (integrate.rs `fence`), its git work none.
//!
//! Each test names the hivemc scenario it mirrors and the property it checks: S1 (one board for any
//! order of the lines), S2' (a done, a verdict or an integration that took effect in a node's fold is in
//! the converged board), integrate-once (one merge per integrate task), same-node
//! exclusion (one read-back win per node per task attempt), no premature end (every runner ended:
//! finished, or a budget spent, a node killed or a stuck whose constraint held, the commits a task
//! needs on no running node that may take it included), no hang (every runner
//! ended by the horizon, phase f F6) and stable-prefix exit (exit 0 only on a finished converged board). A property that does not hold yet is `#[ignore]`d with the step of
//! tasks/todo.md phase f that fixes it, asserting it as the property reads, so `--ignored` shows the
//! violation.

use std::collections::{BTreeMap, BTreeSet};

use super::board::{self, Board, Ev, Event, Role, Status, Task, TaskState};
use super::commits::Reach;
use super::run::{self, Next, Offer, Tally};
use super::{BEAT_MS, CLAIM_TRIES, Commits, LEASE_MS, START_CAP, SUSPECT_MS, TaskId, integrate};

/// One tick, in ms.
const TICK: i64 = 1000;
/// run.rs `IDLE_WAIT`, in ticks.
const IDLE_WAIT: i64 = run::IDLE_WAIT.as_secs() as i64;

#[derive(Clone)]
struct NodeCfg {
    name: &'static str,
    drones: usize,
    /// The tick its runner starts.
    start: i64,
    /// Its clock against the true one, in ms.
    skew: i64,
    /// Iterations each of its drones may run.
    share: u32,
    /// The tick its runner is killed: no line of it after that (its mesh daemon still syncs).
    stop: Option<i64>,
    /// The tick its runner's beats stop while it and its drones run on (a suspend, a failed beat write,
    /// a stalled beat thread).
    mute: Option<i64>,
    /// Its attempts' commits stay on it (each bundle over `MAX_BUNDLE`), whatever `Cfg::bundles` says.
    stays: bool,
    /// It refuses every other node's bundle line (a prerequisite its repo lacks, an unbundle that never
    /// finishes: commits.rs `reach`'s `Refused`), though the line carries the commits.
    refuses: bool,
}

/// A line held back: the first `ev` line on `task` written on `node` lands in its own file `local` ticks
/// after its write (0: at the tick's end, as any) and on the other nodes `remote` ticks after the delay,
/// which counts from its landing in its own file (sync copies the line from there).
#[derive(Clone)]
struct Hold {
    node: &'static str,
    ev: Ev,
    task: String,
    local: i64,
    remote: i64,
}

/// What a claim's append is checked against under its node's board lock (hive/mod.rs `write_if`).
#[derive(Clone, Debug)]
enum Guard {
    /// mod.rs `claim`: the task is `claimable_for` the session at its stamp.
    Claim { task: String, session: String, drone: String, picked: bool, now: i64 },
    /// integrate.rs `claim_due`: the integration found is still the one `due`.
    Integrate { found: (TaskId, bool), now: i64 },
    /// mod.rs `finish`, verdict.rs `record`, integrate.rs `write_held`: the session still holds the task.
    Holds { task: String, session: String, now: i64 },
    /// mod.rs `finish_line`: the board is finished at `integration`, not said yet in this run.
    Finished { integration: String },
    /// mod.rs `decline_lines`: the tasks this node is to decline are still these.
    Declines { tasks: Vec<String> },
}

impl Guard {
    fn holds(&self, b: &Board, here: &str) -> bool {
        match self {
            Guard::Claim { task, session, drone, picked, now } => super::claimable_for(b, task, board::Actor::new(here, drone, session), *picked, *now),
            Guard::Integrate { found, now } => integrate::due(b, *now).as_ref() == Some(found),
            Guard::Holds { task, session, now } => super::holding(b, here, task, session, *now).is_some(),
            Guard::Finished { integration } => board::finish_due(b, here) == Some(integration.as_str()),
            Guard::Declines { tasks } => {
                let joined = b.members.get(here).map_or(i64::MIN, |(join, _)| *join);
                tasks.iter().all(|t| b.declined.get(&(here.to_string(), t.clone())).is_none_or(|ms| *ms <= joined))
            }
        }
    }
}

/// A claim kept back from its own node's file by a `Hold`: checked when it lands (`Guard`), at the
/// start of tick `at`, its copies then reaching the other nodes at tick `remote`.
struct Landing {
    at: i64,
    node: usize,
    events: Vec<Event>,
    guard: Guard,
    remote: i64,
}

#[derive(Clone)]
struct Cfg {
    nodes: Vec<NodeCfg>,
    /// Work tasks, each with its deps.
    tasks: Vec<(String, Vec<String>)>,
    /// Ticks from a session's start to its claim, per (node, drone), one per session, the last
    /// repeating; 1 for any other.
    lat: BTreeMap<(&'static str, &'static str), Vec<i64>>,
    work: i64,
    verify: i64,
    integrate: i64,
    delay: i64,
    holds: Vec<Hold>,
    /// Ticks [from, to) in which the network is cut: a line written then reaches the other nodes at
    /// `to` at the earliest.
    cuts: Vec<(i64, i64)>,
    /// (node, from, to): ticks [from, to) in which `node` receives nothing (its mesh daemon down or
    /// backing off, its machine just resumed): a line due there then lands at `to`. The others receive
    /// its lines as ever.
    deaf: Vec<(&'static str, i64, i64)>,
    /// A done's commits reach the other nodes as a bundle (commits.rs `publish`); without, its node says
    /// they stay there (a bundle over `MAX_BUNDLE`).
    bundles: bool,
    /// Ticks a bundle line takes to reach the other nodes after the done's line would (a mesh round of
    /// its own, a big line).
    bundle_late: i64,
    /// Work tasks whose done writes no bundle line (mod.rs `finish`: its session killed at its timeout,
    /// or its stop hook cut, during `git bundle create`): only the runner's pass writes it (commits.rs
    /// `republish`).
    unpublished: Vec<String>,
    /// Verify tasks every verifier rejects (`t0.v1`): a rework round, its attempts judged again.
    reject: Vec<String>,
    /// Sessions claim the task their prompt offered by name (mod.rs `offer`, `picked` false: G1).
    named: bool,
    /// Work tasks an engineer opens during the run: (tick, node, task), a dep-free task on that node's board
    /// (`borg hive open` there).
    opens: Vec<(i64, &'static str, String)>,
    /// Tasks no node may claim (mod.rs `may_claim` and the approvals: another repo's, its checks approved
    /// on no node), as run.rs `allowed` says on every node.
    refuse: Vec<String>,
    horizon: i64,
}

/// A work task id `owner` owns among nodes a and b (`board::owner`), not one of `taken`.
fn owned(owner: &str, taken: &[(String, Vec<String>)]) -> String {
    let live = BTreeSet::from(["a", "b"]);
    (0..).map(|i| format!("t{i}")).find(|t| board::owner(&live, t) == Some(owner) && !taken.iter().any(|(id, _)| id == t)).unwrap()
}

impl Cfg {
    /// Nodes a and b, one drone each, both starting at once with one clock; a work task per entry of
    /// `tasks`: (its owner, the index of its dep). Work takes 3 ticks, a verdict and an integration 2,
    /// and a line one tick to reach the other node.
    fn new(tasks: &[(&str, Option<usize>)]) -> Cfg {
        let node = |name| NodeCfg { name, drones: 1, start: 0, skew: 0, share: 9, stop: None, mute: None, stays: false, refuses: false };
        let mut ids: Vec<(String, Vec<String>)> = Vec::new();
        for (owner, dep) in tasks {
            let id = owned(owner, &ids);
            let deps = dep.map(|d| ids[d].0.clone()).into_iter().collect();
            ids.push((id, deps));
        }
        Cfg {
            nodes: vec![node("a"), node("b")],
            tasks: ids,
            lat: BTreeMap::new(),
            work: 3,
            verify: 2,
            integrate: 2,
            delay: 1,
            holds: Vec::new(),
            cuts: Vec::new(),
            deaf: Vec::new(),
            bundles: true,
            bundle_late: 0,
            unpublished: Vec::new(),
            reject: Vec::new(),
            named: false,
            opens: Vec::new(),
            refuse: Vec::new(),
            horizon: 150,
        }
    }

    fn node(mut self, name: &str, f: impl FnOnce(&mut NodeCfg)) -> Cfg {
        f(self.nodes.iter_mut().find(|n| n.name == name).unwrap());
        self
    }

    fn lat(mut self, node: &'static str, drone: &'static str, lat: &[i64]) -> Cfg {
        self.lat.insert((node, drone), lat.to_vec());
        self
    }

    /// The same scenario with every drone's sessions claiming after one of `lats` (each drone its own
    /// choice): a small deterministic sweep in place of hivemc's search over latencies.
    fn sweep(&self, lats: &[i64]) -> Vec<Cfg> {
        let drones: Vec<(&'static str, &'static str)> =
            self.nodes.iter().flat_map(|n| (1..=n.drones).map(move |i| (n.name, ["d1", "d2", "d3"][i - 1]))).collect();
        let mut out = vec![self.clone()];
        for d in drones {
            out = out.into_iter().flat_map(|c| lats.iter().map(move |l| c.clone().lat(d.0, d.1, &[*l]))).collect();
        }
        out
    }
}

/// What a node's fold showed taking effect at some tick: a done (task, session), a verdict (task,
/// session), an integration (task, ms), a rejection's reopen (work task, its rework round).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Seen {
    Done(String, String),
    Verdict(String, String),
    Integrated(String, i64),
    Reopened(String, u32),
}

#[derive(Debug, Clone)]
struct Session {
    id: String,
    /// Its node's board clock at its start (mod.rs `clock`), and what the board offered it then.
    at: i64,
    offer: Offer,
    /// The tick its runner looks whether it settled (run.rs `run_session` notice), and whether it did.
    notice: i64,
    noticed: bool,
    said: bool,
    claim_at: i64,
    /// What its prompt offered (mod.rs `offer`): the task a named claim names.
    offered: Option<String>,
    lost: usize,
    /// The task it claimed, the tick its claim is in its node's file (its read-back is after that),
    /// whether its read-back won, and the tick its work is done.
    task: Option<String>,
    lands: i64,
    won: bool,
    until: i64,
    wrote: bool,
}

#[derive(Debug, Clone)]
enum St {
    Off,
    Decide,
    Wait { until: i64, seen: Box<Board> },
    Session(Box<Session>),
    Integrating { session: String, task: String, until: Option<i64> },
    Post { start: Box<Offer>, wrote: bool },
    Ended(&'static str),
}

/// The drones running at a tick on a node that had started and was not killed: (node, drone).
type Running = Vec<(usize, String)>;

/// An attempt as a bundle line names it: (work task, head).
type AttemptKey = (String, String);

/// A stuck line a runner wrote (phase f F6): the tick, its node, the task and why, and the drones then
/// running on a node that had started and was not killed, (node, drone): who could have taken it.
#[derive(Debug)]
struct Stuck {
    at: i64,
    node: usize,
    task: String,
    why: String,
    running: Vec<(usize, String)>,
}

struct Drone {
    node: usize,
    name: String,
    st: St,
    tally: Tally,
    iters: u32,
}

struct Sim {
    cfg: Cfg,
    t: i64,
    lines: Vec<Vec<Event>>,
    /// Lines on their way: (tick, node, line).
    pending: Vec<(i64, usize, Event)>,
    /// This tick's writes, per node, landing at its end.
    out: Vec<Vec<Event>>,
    /// Claims held back from their own node's file, checked when they land.
    landing: Vec<Landing>,
    drones: Vec<Drone>,
    held: BTreeSet<usize>,
    stopped: BTreeSet<usize>,
    exits: BTreeMap<usize, (bool, i64)>,
    ends: Vec<(String, &'static str, i64)>,
    /// Each stuck line a runner wrote (phase f F6).
    stuck: Vec<Stuck>,
    /// Each drone that ended stuck: (its node, the tick, the drones then running as `Stuck::running`).
    stuck_ends: Vec<(usize, i64, Running)>,
    seen: BTreeSet<Seen>,
    /// Integrators that started the merge (their claim final, their ref moved): integrate task -> sessions.
    integrations: BTreeMap<String, Vec<String>>,
    /// Sessions whose read-back won: (node, task, rework) -> sessions.
    won: BTreeMap<(String, String, u32), Vec<String>>,
    /// Per node, the newest claim stamp of another node its runner acked (`board::to_ack`).
    acked: Vec<i64>,
    /// Ticks each claim waited after its line landed until it was final (phase f F5), and each node's
    /// integrated ref: its fencing token (epoch, claim stamp), and the integrators that moved it, by
    /// (node, integrate task).
    waits: Vec<i64>,
    refs: BTreeMap<usize, (u32, i64)>,
    ref_moves: BTreeMap<(String, String), Vec<String>>,
    log: Vec<String>,
    /// Each node's last fold and how many lines it folded: a node's lines only grow, and between two
    /// beats most ticks bring none.
    folded: Vec<(usize, Board)>,
    /// The bundle lines each node holds (commits.rs `bundles.jsonl`): (work task, head) -> None for a
    /// bundle (its commits taken at once), Some(why) for a line saying they stay on their node.
    bundles: Vec<BTreeMap<AttemptKey, Option<String>>>,
    /// Bundle lines on their way: (tick, node, attempt, line).
    bundles_pending: Vec<(i64, usize, AttemptKey, Option<String>)>,
    /// The attempts a bundle was made of: their commits can reach any node.
    published: BTreeSet<AttemptKey>,
    /// Sessions that wrote no board line (run.rs `stalls`' `wrote_nothing`).
    empty: Vec<String>,
}

fn work_task(id: &str, role: Role, deps: &[String]) -> Task {
    Task {
        id: id.into(),
        role,
        title: id.into(),
        repo: String::new(),
        files: vec![],
        deps: deps.to_vec(),
        accept: vec![],
        of: None,
        not_by: vec![],
        flags: vec![],
        from: None,
        group: None,
    }
}

/// hive/mod.rs `land`: `events` of one write stamped under the lock onto a file holding `file` (a hybrid
/// clock and the watermark's count, `board::landing`), a claim past the lease it takes over (`board::claim_at`, as mod.rs `claim`
/// and integrate.rs `claim_due` propose it; a claim's ms is its node's clock when it was decided).
fn stamped(file: &[Event], events: Vec<Event>) -> Vec<Event> {
    let here = events.first().map_or("", |e| e.node.as_str());
    let b = if events.iter().any(|e| e.ev == Ev::Claim) { board::fold_on(file, here) } else { Board::default() };
    let at = |e: &Event| match (e.ev, b.tasks.get(&e.task)) {
        (Ev::Claim | Ev::Heartbeat, Some(s)) if events.iter().any(|c| c.ev == Ev::Claim && c.task == e.task) => board::claim_at(s, e.ms),
        _ => e.ms,
    };
    // A claim names the lease it takes over (`board::takes`), as mod.rs `claim` and integrate.rs do.
    let takes = |e: &Event| b.tasks.get(&e.task).filter(|_| e.ev == Ev::Claim).and_then(|s| board::takes(s, e.ms));
    let props: Vec<(i64, Option<i64>)> = events.iter().map(|e| (at(e), takes(e))).collect();
    board::landing(file, events.into_iter().zip(props).map(|(e, (ms, takes))| Event { ms, takes, ..e }).collect())
}

/// The stamp of the claim its holder holds `t` under at `now`.
fn since(t: &TaskState, now: i64) -> i64 {
    t.live(now).map_or(i64::MIN, |(_, _, since)| since)
}

/// The head of the attempt the verify task `t` judges (its `of_attempt`), which its verdict names.
fn judged_head(b: &Board, t: &TaskState) -> String {
    let (Some(w), Some((by, done))) = (t.task.of.as_ref().and_then(|w| b.tasks.get(w)), t.of_attempt.as_ref()) else { return String::new() };
    w.attempts.iter().find(|a| a.by == *by && a.done == *done).map_or_else(String::new, |a| a.head.clone())
}

/// run.rs `open_to` with every check approved but the tasks `Cfg::refuse` names, the commits `node`
/// lacks left out (`lacks`, run.rs `offered`).
fn open_to(cfg: &Cfg, b: &Board, drone: &str, node: &str, lacks: &BTreeMap<TaskId, Commits>, now: i64) -> Offer {
    Offer::of(b, drone, node, "", now, run::offered(allowed(cfg), lacks))
}

/// run.rs `allowed` on every node: no integrate task (integrate.rs takes those), none `Cfg::refuse` names.
fn allowed(cfg: &Cfg) -> impl Fn(&TaskState) -> bool + '_ {
    |s| s.task.role != Role::Integrate && !cfg.refuse.contains(&s.task.id)
}

impl Sim {
    fn run(cfg: Cfg) -> Sim {
        let n = cfg.nodes.len();
        let opens = cfg.tasks.iter().map(|(id, deps)| Event { node: "a".into(), ..Event::open(work_task(id, Role::Work, deps), "engineer:e", -TICK) });
        let opens: Vec<Event> = opens.collect();
        let drones = cfg
            .nodes
            .iter()
            .enumerate()
            .flat_map(|(i, nc)| (1..=nc.drones).map(move |k| Drone { node: i, name: format!("d{k}"), st: St::Off, tally: Tally::default(), iters: 0 }))
            .collect();
        // Every node holds the goal's opens from a tick before the start, on its own clock.
        let held = |i: usize| opens.iter().map(|e| Event { seen: board::Seen(Some(-TICK + cfg.nodes[i].skew + i as i64)), ..e.clone() }).collect();
        let lines = (0..n).map(held).collect();
        let mut s = Sim {
            t: 0,
            lines,
            pending: Vec::new(),
            out: vec![Vec::new(); n],
            landing: Vec::new(),
            drones,
            held: BTreeSet::new(),
            stopped: BTreeSet::new(),
            exits: BTreeMap::new(),
            ends: Vec::new(),
            stuck: Vec::new(),
            stuck_ends: Vec::new(),
            seen: BTreeSet::new(),
            integrations: BTreeMap::new(),
            won: BTreeMap::new(),
            acked: vec![i64::MIN; n],
            waits: Vec::new(),
            refs: BTreeMap::new(),
            ref_moves: BTreeMap::new(),
            log: Vec::new(),
            folded: vec![(usize::MAX, Board::default()); n],
            bundles: vec![BTreeMap::new(); n],
            bundles_pending: Vec::new(),
            published: BTreeSet::new(),
            empty: Vec::new(),
            cfg,
        };
        while s.t < s.cfg.horizon
            && !(s.pending.is_empty() && s.landing.is_empty() && s.bundles_pending.is_empty() && s.drones.iter().all(|d| matches!(d.st, St::Ended(_))))
        {
            s.tick();
        }
        s
    }

    fn name(&self, node: usize) -> &'static str {
        self.cfg.nodes[node].name
    }

    fn now(&self, node: usize) -> i64 {
        self.clock(node, self.t)
    }

    /// `node`'s clock at tick `t`.
    fn clock(&self, node: usize, t: i64) -> i64 {
        t * TICK + self.cfg.nodes[node].skew + node as i64
    }

    fn note(&mut self, node: usize, what: String) {
        self.log.push(format!("t={} {}: {what}", self.t, self.name(node)));
    }

    /// Writes `events` on `node` (hive/mod.rs `write` stamps the node): they land in its file at the
    /// tick's end, and on the other nodes `delay` ticks after that (a `Hold` later). The tick they are
    /// in its own file.
    fn write(&mut self, node: usize, events: Vec<Event>) -> i64 {
        self.write_as(node, events, None).unwrap_or(self.t + 1)
    }

    /// `write` under its node's board lock (hive/mod.rs `write_if`): `events` land only when `guard`
    /// holds on the node's file as it stands then, after the lines the node wrote before them. A claim
    /// a `Hold` keeps back is checked when it lands (`Landing`); any other at once, as it lands at the
    /// tick's end after the node's earlier lines of the tick. None: refused at once, nothing written.
    fn write_if(&mut self, node: usize, events: Vec<Event>, guard: Guard) -> Option<i64> {
        self.write_as(node, events, Some(guard))
    }

    fn write_as(&mut self, node: usize, events: Vec<Event>, guard: Option<Guard>) -> Option<i64> {
        let mut lands = self.t + 1;
        if self.stopped.contains(&node) {
            return Some(lands);
        }
        if let Some(guard) = guard {
            let events: Vec<Event> = events.into_iter().map(|e| Event { node: self.name(node).into(), ..e }).collect();
            let hold = events.iter().find_map(|e| self.hold(e));
            let (local, remote) = hold.map_or((0, 0), |i| (self.cfg.holds[i].local, self.cfg.holds[i].remote));
            if let Some(i) = hold {
                self.held.insert(i);
            }
            let remote = self.arrive(self.t + (local - 1).max(0) + self.cfg.delay + remote);
            if local > 0 {
                for e in &events {
                    self.note(node, format!("{} {} {}@{} (held {local} ticks)", e.by, e.ev.name(), e.task, e.ms));
                }
                self.landing.push(Landing { at: self.t + local, node, events, guard, remote });
                return Some(self.t + local);
            }
            let file = self.file(node);
            if !guard.holds(&board::fold_on(&file, self.name(node)), self.name(node)) {
                self.note(node, format!("{guard:?} refused under the lock"));
                return None;
            }
            for e in stamped(&file, events) {
                self.note(node, format!("{} {} {}@{}", e.by, e.ev.name(), e.task, e.ms));
                self.out[node].push(e.clone());
                for to in (0..self.lines.len()).filter(|to| *to != node) {
                    self.pending.push((remote, to, e.clone()));
                }
            }
            return Some(lands);
        }
        let file = self.file(node);
        let events: Vec<Event> = events.into_iter().map(|e| Event { node: self.name(node).into(), ..e }).collect();
        for e in stamped(&file, events) {
            let hold = self.hold(&e);
            let (local, remote) = hold.map_or((0, 0), |i| (self.cfg.holds[i].local, self.cfg.holds[i].remote));
            if let Some(i) = hold {
                self.held.insert(i);
            }
            lands = lands.max(self.t + local);
            if e.note != board::BEAT {
                let what = format!("{} {} {}@{}{}", e.by, e.ev.name(), e.task, e.ms, if e.note.is_empty() { String::new() } else { format!(" ({})", e.note) });
                self.note(node, what);
            }
            let remote = self.arrive(self.t + (local - 1).max(0) + self.cfg.delay + remote);
            for to in 0..self.lines.len() {
                match (to == node, local) {
                    (true, 0) => self.out[node].push(e.clone()),
                    (true, _) => self.pending.push((self.t + local, to, e.clone())),
                    (false, _) => self.pending.push((remote, to, e.clone())),
                }
            }
        }
        Some(lands)
    }

    /// When a line written now that would reach the other nodes at tick `at` does: not before a cut it is
    /// written in heals (`Cfg::cuts`).
    fn arrive(&self, at: i64) -> i64 {
        self.cfg.cuts.iter().filter(|(from, to)| (*from..*to).contains(&self.t)).fold(at, |at, (_, to)| at.max(*to))
    }

    /// A runner line of `node` with `note` (a roster, a membership line) at its clock now.
    fn runner(&self, node: usize, note: &str) -> Event {
        Event::new(Ev::Heartbeat, "", "runner", self.now(node) + self.out[node].len() as i64).with_note(note)
    }

    /// What `node`'s board file holds now: its lines and its writes of this tick (each landed under
    /// the lock after the ones before it).
    fn file(&self, node: usize) -> Vec<Event> {
        [self.lines[node].as_slice(), self.out[node].as_slice()].concat()
    }

    /// The `Hold` not used yet that keeps `e` back.
    fn hold(&self, e: &Event) -> Option<usize> {
        self.cfg.holds.iter().position(|h| h.node == e.node && h.ev == e.ev && h.task == e.task).filter(|i| !self.held.contains(i))
    }

    /// The held claims due by now land in their node's file, each only when its `Guard` holds there.
    fn land(&mut self) {
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.landing).into_iter().partition(|l| l.at <= self.t);
        self.landing = later;
        for l in due {
            if !l.guard.holds(&board::fold_on(&self.lines[l.node], self.name(l.node)), self.name(l.node)) {
                self.note(l.node, format!("{:?} refused under the lock", l.guard));
                continue;
            }
            let seen = board::Seen(Some(self.now(l.node)));
            for e in stamped(&self.lines[l.node], l.events) {
                self.note(l.node, format!("{} {} {}@{} lands", e.by, e.ev.name(), e.task, e.ms));
                for to in (0..self.lines.len()).filter(|to| *to != l.node) {
                    self.pending.push((l.remote.max(self.t), to, e.clone()));
                }
                self.lines[l.node].push(Event { seen, ..e });
            }
        }
    }

    fn tick(&mut self) {
        let t = self.t;
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending).into_iter().partition(|(at, _, _)| *at <= t);
        self.pending = later;
        for (at, to, e) in due {
            if let Some(&(_, _, until)) = self.cfg.deaf.iter().find(|(n, from, until)| *n == self.cfg.nodes[to].name && (*from..*until).contains(&t)) {
                self.pending.push((at.max(until), to, e));
                continue;
            }
            let seen = board::Seen(Some(self.now(to)));
            self.lines[to].push(Event { seen, ..e });
        }
        self.land();
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.bundles_pending).into_iter().partition(|(at, _, _, _)| *at <= t);
        self.bundles_pending = later;
        for (_, to, attempt, line) in due {
            self.note(to, format!("holds the bundle line of {}@{}{}", attempt.0, attempt.1, line.as_ref().map_or(String::new(), |w| format!(" (stays: {w})"))));
            self.bundles[to].entry(attempt).or_insert(line);
        }
        for (_, node, id) in self.cfg.opens.clone().into_iter().filter(|(at, _, _)| *at == t) {
            let i = self.cfg.nodes.iter().position(|n| n.name == node).unwrap();
            let now = self.now(i);
            self.write(i, vec![Event::open(work_task(&id, Role::Work, &[]), "engineer:e", now)]);
        }
        for i in 0..self.cfg.nodes.len() {
            let start = self.cfg.nodes[i].start;
            if start == t {
                // mod.rs `announce`: the join and the roster in one write.
                let names: Vec<String> = (1..=self.cfg.nodes[i].drones).map(|k| format!("d{k}")).collect();
                let now = self.now(i);
                let line = |note: &str| Event::new(Ev::Heartbeat, "", "runner", now).with_note(note);
                self.write(i, vec![line(board::JOIN), line(&board::roster_note(&names))]);
            }
            // run.rs `membership`: a goal heartbeat every `BEAT_MS` while the runner runs.
            let beats = |n: &NodeCfg| n.stop.into_iter().chain(n.mute).all(|stop| t < stop);
            let running = t >= start && !self.exits.contains_key(&i) && beats(&self.cfg.nodes[i]);
            if t > start && (t - start) % (BEAT_MS / TICK) == 0 && running {
                let beat = self.runner(i, board::BEAT);
                self.write(i, vec![beat]);
            } else if let Some(m) = board::to_ack(&self.file(i), self.name(i)).filter(|m| running && *m > self.acked[i]) {
                // run.rs `membership`: a claim of another node arrived, a beat now moves this node's
                // watermark past it (phase f F5).
                self.acked[i] = m;
                let beat = self.runner(i, board::BEAT);
                self.write(i, vec![beat]);
            }
            if self.cfg.nodes[i].stop == Some(t) {
                self.note(i, "runner killed".into());
                for d in self.drones.iter_mut().filter(|d| d.node == i && !matches!(d.st, St::Ended(_))) {
                    d.st = St::Ended("killed");
                    self.ends.push((format!("{}.{}", self.cfg.nodes[i].name, d.name), "killed", t));
                }
                self.stopped.insert(i);
            }
        }
        for (i, l) in self.lines.iter().enumerate() {
            if self.folded[i].0 != l.len() {
                self.folded[i] = (l.len(), board::fold_on(l, self.cfg.nodes[i].name));
            }
        }
        let folds: Vec<Board> = self.folded.iter().map(|(_, b)| b.clone()).collect();
        for b in &folds {
            self.observe(b);
        }
        for d in 0..self.drones.len() {
            let was_running = !matches!(self.drones[d].st, St::Ended(_));
            self.act(d, &folds[self.drones[d].node]);
            if let (true, St::Ended(why)) = (was_running, &self.drones[d].st) {
                let (node, why) = (self.drones[d].node, *why);
                self.ends.push((format!("{}.{}", self.name(node), self.drones[d].name), why, t));
                self.note(node, format!("{} ends: {why}", self.drones[d].name));
                // Its runner names the drones still running (each end its own ms, as the threads end apart).
                let still: Vec<String> = self.drones.iter().filter(|x| x.node == node && !matches!(x.st, St::Ended(_))).map(|x| x.name.clone()).collect();
                let roster = self.runner(node, &board::roster_note(&still));
                self.write(node, vec![roster]);
                // run.rs `main`: its drones ended, the runner leaves the goal (`Joined`).
                if still.is_empty() && !self.stopped.contains(&node) {
                    let leave = self.runner(node, board::LEAVE);
                    self.write(node, vec![leave]);
                }
            }
        }
        for i in 0..self.lines.len() {
            let seen = board::Seen(Some(self.now(i)));
            let out = std::mem::take(&mut self.out[i]);
            self.lines[i].extend(out.into_iter().map(|e| Event { seen, ..e }));
            let all_ended = self.drones.iter().filter(|d| d.node == i).all(|d| matches!(d.st, St::Ended(_)));
            if all_ended && !self.stopped.contains(&i) && !self.exits.contains_key(&i) {
                // run.rs `main`: exit 0 only on a finish for good (phase f F6).
                let b = board::fold_on(&self.lines[i], self.name(i));
                let saw = self.ends.iter().any(|(d, why, _)| *why == "goal finished" && d.starts_with(&format!("{}.", self.name(i))));
                let finished = b.finished() && (saw || b.finished_for_good(self.name(i), self.now(i)));
                self.exits.insert(i, (finished, t));
                self.note(i, format!("runner exits {}", if finished { 0 } else { 1 }));
            }
        }
        self.t += 1;
    }

    fn observe(&mut self, b: &Board) {
        for sp in &b.spans {
            match sp.end {
                Some(Ev::Done) => self.seen.insert(Seen::Done(sp.task.clone(), sp.by.clone())),
                Some(Ev::Verified | Ev::Rejected) => self.seen.insert(Seen::Verdict(sp.task.clone(), sp.by.clone())),
                _ => false,
            };
        }
        for i in &b.integrations {
            self.seen.insert(Seen::Integrated(i.task.clone(), i.ms));
        }
        for s in b.tasks.values().filter(|s| s.task.role == Role::Work) {
            self.seen.extend((1..=s.rework).map(|r| Seen::Reopened(s.task.id.clone(), r)));
        }
    }

    fn act(&mut self, d: usize, b: &Board) {
        let node = self.drones[d].node;
        let ready = self.cfg.nodes[node].start + if self.cfg.nodes.len() > 1 { self.cfg.delay.min(START_CAP.as_secs() as i64) } else { 0 };
        loop {
            match self.drones[d].st.clone() {
                St::Ended(_) => return,
                St::Off if self.t >= ready => self.drones[d].st = St::Decide,
                St::Off => return,
                St::Wait { until, seen } => {
                    // run.rs `idle_wait`: a change of the board ends the wait early; a line that changes no
                    // board (a beat) does not.
                    if *b == *seen && self.t < until {
                        return;
                    }
                    self.drones[d].st = St::Decide;
                }
                St::Post { start, wrote } => {
                    let lacks = self.lacks(b, node, self.now(node));
                    let end = open_to(&self.cfg, b, &self.drones[d].name, self.name(node), &lacks, self.now(node));
                    if !wrote {
                        let n = self.drones[d].iters;
                        self.empty.push(format!("{}.{}#{n}", self.name(node), self.drones[d].name));
                    }
                    if let Some((id, _)) = end.task.as_ref().filter(|_| !wrote && run::stalls(0, true, &start, &end) == 0) {
                        self.note(node, format!("{} wrote nothing; {id} became claimable during its session: no stall", self.drones[d].name));
                    }
                    let x = &mut self.drones[d];
                    x.tally.spent += 1.0;
                    x.tally.stalls = run::stalls(x.tally.stalls, !wrote, &start, &end);
                    x.st = St::Decide;
                }
                St::Decide => return self.decide(d, b),
                St::Session(s) => return self.session(d, b, *s),
                St::Integrating { session, task, until } => {
                    if self.integrating(d, b, session, task, until) {
                        return;
                    }
                }
            }
        }
    }

    /// One pass of run.rs `drone`'s loop.
    fn decide(&mut self, d: usize, b: &Board) {
        let (node, now) = (self.drones[d].node, self.now(self.drones[d].node));
        let (here, drone) = (self.name(node), self.drones[d].name.clone());
        // run.rs `drone`: commits.rs `republish`, each pass.
        self.republish(node, b);
        if let Some((id, new)) = integrate::due(b, now) {
            // run.rs `drone` on integrate.rs `elsewhere`: an accepted attempt whose commits this node lacks
            // waits while they may come or another node that can hold them all integrates, else the
            // integration is stuck; a node that cannot get one says so (mod.rs `decline_integration`).
            let needs = self.needs(b, node);
            if integrate::refuses(here, &needs) && !b.declines(here, &id) {
                let line = self.runner(node, &board::decline_note(&id));
                self.write_if(node, vec![line], Guard::Declines { tasks: vec![id.clone()] });
            }
            match integrate::gate(b, here, now, &id, &needs) {
                Some(Commits::Unavailable(why)) => {
                    let running = self.running();
                    if b.stuck.get(&id).is_none_or(|(_, _, w)| *w != why) {
                        self.stuck.push(Stuck { at: self.t, node, task: id.clone(), why: why.clone(), running: running.clone() });
                        let line = self.runner(node, &board::stuck_note(&id, &why));
                        self.write(node, vec![line]);
                    }
                    self.stuck_ends.push((node, self.t, running));
                    self.drones[d].st = St::Ended("stuck");
                    return;
                }
                Some(Commits::Coming(_)) => {
                    self.drones[d].st = St::Wait { until: self.t + IDLE_WAIT, seen: Box::new(b.clone()) };
                    return;
                }
                None => {}
            }
            // integrate.rs `run` and `claim_due`: the open when new, a claim and a heartbeat in one write,
            // under the lock while the integration is still the one due; refused, the drone goes on.
            let session = integrate::session(here, &drone, now);
            let mut lines: Vec<Event> = new.then(|| Event::open(work_task(&id, Role::Integrate, &[]), &session, now)).into_iter().collect();
            lines.extend([Event::new(Ev::Claim, &id, &session, now).with_drone(&drone), Event::new(Ev::Heartbeat, &id, &session, now)]);
            if self.write_if(node, lines, Guard::Integrate { found: (id.clone(), new), now }).is_some() {
                self.drones[d].st = St::Integrating { session, task: id, until: None };
                return;
            }
        }
        let lacks = self.lacks(b, node, now);
        let offer = open_to(&self.cfg, b, &drone, here, &lacks, now);
        // run.rs `drone`: a finished board is said (mod.rs `finish_line`, under the lock), and its drones end
        // only once every participant said it (`Ending`, phase f F6).
        if let Some(i) = board::finish_due(b, here) {
            let line = self.runner(node, &board::finished_note(i));
            self.write_if(node, vec![line], Guard::Finished { integration: i.to_string() });
        }
        // mod.rs `decline_lines`: the tasks this node may not take, said under its lock.
        let declines = board::declines_due(b, here, now, run::takes(allowed(&self.cfg), &lacks));
        if !declines.is_empty() {
            let lines = declines.iter().map(|t| self.runner(node, &board::decline_note(t))).collect();
            self.write_if(node, lines, Guard::Declines { tasks: declines });
        }
        let ending = run::Ending::of(b, here, now, allowed(&self.cfg), &lacks);
        match run::step(&self.drones[d].tally, f64::from(self.cfg.nodes[node].share), 1.0, &ending, offer.task.is_some()) {
            Next::End(why) => {
                if let (run::Ending::Stuck(stuck), "stuck") = (&ending, why) {
                    // mod.rs `stuck_lines`: each stuck task said once while it stands (`Board::stuck`).
                    let running = self.running();
                    let new: Vec<&(String, String)> = stuck.iter().filter(|(t, why)| b.stuck.get(t).is_none_or(|(_, _, w)| w != why)).collect();
                    let lines = new.iter().map(|(t, why)| self.runner(node, &board::stuck_note(t, why))).collect::<Vec<_>>();
                    for (t, why) in new {
                        self.stuck.push(Stuck { at: self.t, node, task: t.clone(), why: why.clone(), running: running.clone() });
                    }
                    self.stuck_ends.push((node, self.t, running));
                    if !lines.is_empty() {
                        self.write(node, lines);
                    }
                }
                self.drones[d].st = St::Ended(why);
            }
            Next::Wait => self.drones[d].st = St::Wait { until: self.t + IDLE_WAIT, seen: Box::new(b.clone()) },
            Next::Go => {
                let x = &mut self.drones[d];
                x.iters += 1;
                let iters = x.iters as usize;
                let id = format!("{here}.{drone}#{iters}");
                let lat = self.cfg.lat.iter().find(|((n, dn), _)| *n == here && *dn == drone).map_or(1, |(_, l)| l[(iters - 1).min(l.len() - 1)]);
                let since = offer.task.as_ref().map_or(now, |(_, since)| *since);
                let notice = self.t + ((since + IDLE_WAIT * TICK - now).max(0) + TICK - 1) / TICK;
                let offered = if self.cfg.named { self.next(b, &id, &drone, node, now) } else { None };
                let s = Session {
                    id,
                    at: board::stamp(&self.file(node), here, now),
                    offer,
                    notice,
                    noticed: false,
                    said: false,
                    claim_at: self.t + lat,
                    offered,
                    lost: 0,
                    task: None,
                    lands: 0,
                    won: false,
                    until: 0,
                    wrote: false,
                };
                self.note(node, format!("{drone} starts {} (claims at t={})", s.id, s.claim_at));
                self.session(d, b, s);
            }
        }
    }

    /// The task mod.rs `next_open` offers `session`: `next_for` among those it may take here, its commits
    /// here (`lacking`, as the runner's offer reads them).
    fn next(&self, b: &Board, session: &str, drone: &str, node: usize, now: i64) -> Option<String> {
        let lacks = self.lacks(b, node, now);
        board::next_for(b, board::Actor::new(self.name(node), drone, session), "", now, run::offered(allowed(&self.cfg), &lacks)).map(|s| s.task.id.clone())
    }

    /// The drones running now on a node that had started and was not killed: who could take a task.
    fn running(&self) -> Running {
        self.drones
            .iter()
            .filter(|x| !matches!(x.st, St::Ended(_)) && !self.stopped.contains(&x.node) && self.t >= self.cfg.nodes[x.node].start)
            .map(|x| (x.node, x.name.clone()))
            .collect()
    }

    /// commits.rs `reach` on `node` for the attempt (work task, head) done on `author`: here when done
    /// there or its bundle line is held (refused on a node that `refuses` lines), refused when its line
    /// says they stay there, else coming.
    fn reach(&self, node: usize, author: &str, attempt: (String, String)) -> Reach {
        match self.bundles[node].get(&attempt) {
            _ if author == self.name(node) => Reach::Here,
            Some(None) if self.cfg.nodes[node].refuses => Reach::Refused(format!("a prerequisite of node {author}'s bundle is not in this repo")),
            Some(None) => Reach::Here,
            Some(Some(why)) => Reach::Refused(format!("they stay on node {author}: {why}")),
            None => Reach::Coming,
        }
    }

    /// commits.rs `publish` on `node` for the attempt (work task, head): the bundle line, or one saying the
    /// commits stay on this node, held here at once and on the others `bundle_late` ticks after a line.
    fn publish(&mut self, node: usize, attempt: AttemptKey) {
        let line = (!self.cfg.bundles || self.cfg.nodes[node].stays).then(|| "a 2048 KiB bundle, over 1024 KiB".to_string());
        if line.is_none() {
            self.published.insert(attempt.clone());
        }
        let at = self.arrive(self.t + self.cfg.delay + self.cfg.bundle_late);
        self.bundles[node].insert(attempt.clone(), line.clone());
        for to in (0..self.lines.len()).filter(|to| *to != node) {
            self.bundles_pending.push((at, to, attempt.clone(), line.clone()));
        }
    }

    /// commits.rs `republish` on `node`: the line of each attempt done there that has none.
    fn republish(&mut self, node: usize, b: &Board) {
        let here = self.name(node);
        let mine = b.tasks.values().flat_map(|s| s.attempts.iter().filter(|a| board::of_node(&a.node, here)).map(|a| (s.task.id.clone(), a.head.clone())));
        let missing: Vec<AttemptKey> = mine.filter(|k| !self.bundles[node].contains_key(k)).collect();
        for k in missing {
            self.note(node, format!("republishes the bundle line of {}@{}", k.0, k.1));
            self.publish(node, k);
        }
    }

    /// mod.rs `lacking` on `node`: a verify task's attempt by `reach`.
    fn lacks(&self, b: &Board, node: usize, now: i64) -> BTreeMap<TaskId, Commits> {
        super::lacking(b, self.name(node), now, |s| match &s.task.of {
            Some(w) => self.reach(node, &s.node, (w.clone(), judged_head(b, s))),
            None => Reach::Here,
        })
    }

    /// The accepted attempts of `b`'s verified work tasks: (work task, head, its node).
    fn accepted(b: &Board) -> Vec<(String, String, String)> {
        b.tasks
            .values()
            .filter(|s| s.task.role == Role::Work)
            .filter_map(|s| {
                let (by, done) = s.accepted_attempt.as_ref()?;
                let a = s.attempts.iter().find(|a| a.by == *by && a.done == *done)?;
                Some((s.task.id.clone(), a.head.clone(), a.node.clone()))
            })
            .collect()
    }

    /// integrate.rs `elsewhere`'s needs on `node`: each accepted attempt, where its commits stand there,
    /// and whether a bundle of it is held there.
    fn needs(&self, b: &Board, node: usize) -> Vec<integrate::Need> {
        Self::accepted(b)
            .into_iter()
            .map(|(task, head, author)| {
                let travels = self.bundles[node].get(&(task.clone(), head.clone())).is_some_and(|l| l.is_none());
                integrate::Need { reach: self.reach(node, &author, (task.clone(), head)), task, node: author, travels }
            })
            .collect()
    }

    /// Whether node `n` holds, or a bundle can bring it, the commits of the attempt (work task, head) done
    /// on `author`: what no stuck may deny (a node that `refuses` lines takes none). A bundle of a killed author not held on `n` yet is given up
    /// with its author (mod.rs `lacking`, after a lease of silence), as a cut that long is.
    fn reachable(&self, n: usize, author: &str, attempt: &(String, String)) -> bool {
        let killed = self.cfg.nodes.iter().position(|c| c.name == author).is_some_and(|i| self.stopped.contains(&i));
        let taken = (self.published.contains(attempt) && !killed) || self.bundles[n].get(attempt).is_some_and(|l| l.is_none());
        self.name(n) == author || (taken && !self.cfg.nodes[n].refuses)
    }

    /// mod.rs `claim_cmd` up to `claim`'s write: the task claimed (its claim written, or held back to
    /// be checked when it lands), None when no claim was written.
    fn claim(&mut self, d: usize, b: &Board, s: &mut Session) -> Option<String> {
        let (node, now, drone) = (self.drones[d].node, self.now(self.drones[d].node), self.drones[d].name.clone());
        let tries = if self.cfg.named { 1 } else { CLAIM_TRIES };
        while s.lost < tries {
            let id = match self.cfg.named {
                true => s
                    .offered
                    .clone()
                    .filter(|id| b.tasks.get(id).is_some_and(|t| board::claimable_by(b, t, board::Actor::new(self.name(node), &drone, &s.id), now)))?,
                false => self.next(b, &s.id, &drone, node, now)?,
            };
            // mod.rs `claim`'s check under the lock: a rival holds it, or (a picked task) another node's
            // idle drones now take it, and the claim is refused unwritten.
            let lines = vec![Event::new(Ev::Claim, &id, &s.id, now).with_drone(&drone), Event::new(Ev::Heartbeat, &id, &s.id, now)];
            let guard = Guard::Claim { task: id.clone(), session: s.id.clone(), drone: drone.clone(), picked: !self.cfg.named, now };
            let Some(lands) = self.write_if(node, lines, guard) else {
                s.lost += 1;
                continue;
            };
            s.lands = lands;
            s.wrote = true;
            return Some(id);
        }
        None
    }

    fn session(&mut self, d: usize, b: &Board, mut s: Session) {
        let (node, now, drone) = (self.drones[d].node, self.now(self.drones[d].node), self.drones[d].name.clone());
        let here = self.name(node);
        let at = s.at;
        let unsettled = |b: &Board| !super::settled_since(b, here, &drone, at);
        if !s.noticed && self.t >= s.notice {
            s.noticed = true;
            if unsettled(b) {
                self.write(node, vec![Event::new(Ev::Heartbeat, "", "runner", s.at).with_note(&board::session_note(&drone, true))]);
                s.said = true;
            }
        }
        let ended = match (s.task.clone(), s.won) {
            (None, _) if self.t < s.claim_at => false,
            (None, _) => {
                s.task = self.claim(d, b, &mut s);
                s.task.is_none()
            }
            (Some(_), false) if self.t < s.lands => false,
            (Some(id), false) => match super::holding(b, here, &id, &s.id, now) {
                // mod.rs `across`: the claim waits until it is final (phase f F5), every participant's
                // watermark past its stamp, or the ones it waits for suspected.
                Some(t) if !b.unconfirmed(here, since(&t, now), now).0.is_empty() => false,
                Some(t) => {
                    let key = (here.to_string(), id.clone(), t.rework);
                    self.won.entry(key).or_default().push(s.id.clone());
                    self.waits.push(self.t - s.lands);
                    s.won = true;
                    s.until = self.t + if t.task.role == Role::Verify { self.cfg.verify } else { self.cfg.work };
                    false
                }
                None => {
                    s.lost += 1;
                    self.note(node, format!("{} lost {id}", s.id));
                    s.task = if self.cfg.named { None } else { self.claim(d, b, &mut s) };
                    s.task.is_none()
                }
            },
            // Its hooks tell a loser at its next tool call: it sets the work aside and ends.
            (Some(id), true) => match super::holding(b, here, &id, &s.id, now) {
                None => {
                    self.note(node, format!("{} is told it lost {id}", s.id));
                    true
                }
                Some(_) if self.t < s.until => false,
                Some(t) => {
                    // Written under the claim the fold gives it now, and only while its node's file under the
                    // lock still gives it: for good (phase f F5).
                    let claim = Some(since(&t, now));
                    let lines = match t.task.role {
                        Role::Verify if self.cfg.reject.contains(&id) => {
                            vec![Event { claim, head: judged_head(b, &t), ..Event::new(Ev::Rejected, &id, &s.id, now).with_note("rejected") }]
                        }
                        Role::Verify => vec![Event { claim, head: judged_head(b, &t), ..Event::new(Ev::Verified, &id, &s.id, now) }],
                        _ => {
                            // mod.rs `finish`: the done and its verify task's open (`verify_task`) at one ms.
                            let v = Task {
                                id: format!("{id}.v{}", t.rework + 1),
                                of: Some(id.clone()),
                                not_by: vec![s.id.clone(), drone.clone()],
                                ..work_task("", Role::Verify, &t.task.deps)
                            };
                            vec![Event { head: format!("head-{}", s.id), claim, ..Event::new(Ev::Done, &id, &s.id, now) }, Event::open(v, &s.id, now)]
                        }
                    };
                    let done = lines.iter().find(|e| e.ev == Ev::Done).map(|e| (e.task.clone(), e.head.clone()));
                    if self.write_if(node, lines, Guard::Holds { task: id.clone(), session: s.id.clone(), now }).is_none() {
                        self.note(node, format!("{} no longer holds {id}: nothing written", s.id));
                    } else if let Some(attempt) = done.filter(|_| !self.stopped.contains(&node)) {
                        // mod.rs `finish` then commits.rs `publish`, unless the session is cut first.
                        match self.cfg.unpublished.contains(&attempt.0) {
                            true => self.note(node, format!("{} cut before the bundle line of {}@{}", s.id, attempt.0, attempt.1)),
                            false => self.publish(node, attempt),
                        }
                    }
                    true
                }
            },
        };
        if ended {
            if s.said && unsettled(b) {
                self.write(node, vec![Event::new(Ev::Heartbeat, "", "runner", now).with_note(&board::session_note(&drone, false))]);
            }
            self.drones[d].st = St::Post { start: Box::new(s.offer), wrote: s.wrote };
        } else {
            self.drones[d].st = St::Session(Box::new(s));
        }
    }

    /// integrate.rs after `claim_due`'s write: the read-back and the wait until the claim is final (phase
    /// f F5), the integrated ref moved under its fencing token (`integrate::fence`: refused to a token not
    /// above the ref's), the merge and the oracle, then a fold again before the verdict (written only
    /// while the claim still holds, for good, with the attempts merged). Whether the drone is done for
    /// this tick.
    fn integrating(&mut self, d: usize, b: &Board, session: String, task: String, until: Option<i64>) -> bool {
        let (node, now) = (self.drones[d].node, self.now(self.drones[d].node));
        let here = self.name(node);
        let held = b.tasks.get(&task).filter(|s| s.held_by(here, &session, now)).and_then(|s| s.live(now)).map(|(_, _, since)| since);
        match (until, held) {
            (None, None) => {
                self.drones[d].st = St::Decide;
                false
            }
            (None, Some(since)) if !b.unconfirmed(here, since, now).0.is_empty() => true,
            (None, Some(since)) => {
                let token = (integrate::epoch(&task), since);
                if self.refs.get(&node).is_some_and(|held| *held >= token) {
                    self.note(node, format!("{session} fenced from the ref by {:?}", self.refs[&node]));
                    self.drones[d].st = St::Decide;
                    return false;
                }
                self.refs.insert(node, token);
                self.integrations.entry(task.clone()).or_default().push(session.clone());
                self.ref_moves.entry((here.to_string(), task.clone())).or_default().push(session.clone());
                self.drones[d].st = St::Integrating { session, task, until: Some(self.t + self.cfg.integrate) };
                true
            }
            (Some(at), _) if self.t < at => true,
            (Some(_), None) => {
                self.note(node, format!("{session} no longer holds {task}: no verdict"));
                self.drones[d].st = St::Decide;
                true
            }
            (Some(_), Some(since)) => {
                let merged = b.tasks.values().filter(|s| s.task.role == Role::Work).filter_map(|s| {
                    let (by, done) = s.accepted_attempt.as_ref()?;
                    s.attempts.iter().find(|a| a.by == *by && a.done == *done).map(|a| format!("{}@{}", s.task.id, a.head))
                });
                let e = Event {
                    head: format!("merged-{session}"),
                    claim: Some(since),
                    deps_in: merged.collect(),
                    ..Event::new(Ev::Verified, &task, &session, now)
                };
                // integrate.rs `write_held`: under the lock, only while its node's file still gives it the claim.
                if self.write_if(node, vec![e], Guard::Holds { task: task.clone(), session: session.clone(), now }).is_none() {
                    self.note(node, format!("{session} no longer holds {task}: no verdict"));
                }
                self.drones[d].st = St::Decide;
                true
            }
        }
    }

    /// Every line any node holds, each once: what each node folds once everything is delivered.
    fn union(&self) -> Vec<Event> {
        let mut seen = BTreeSet::new();
        self.lines.iter().flatten().chain(self.pending.iter().map(|(_, _, e)| e)).filter(|e| seen.insert(serde_json::to_string(e).unwrap())).cloned().collect()
    }

    fn converged(&self) -> Board {
        board::fold(&self.union())
    }

    /// The tick of the first line of the trace on `node` that says `what`.
    fn first(&self, node: &str, what: &str) -> Option<i64> {
        let on = format!(" {node}: ");
        self.log.iter().find(|l| l.contains(&on) && l.contains(what)).and_then(|l| l.strip_prefix("t=")?.split(' ').next()?.parse().ok())
    }

    fn trace(&self) -> String {
        self.log.join("\n")
    }

    /// No premature end (phase f F6): once every runner ended, the converged board is finished, or every
    /// drone's budget ran out (one that ended with budget left could have gone on), a node was killed, or
    /// each drone ended on its budget or a stuck. Each stuck must have held when it was said: the task
    /// blocked, a dep of it on no board, refused on every node (`Cfg::refuse`), or barred (on the converged
    /// board) to every drone then running on a node that had started and was not killed, or its commits
    /// (an integration's: one of its accepted attempts') on no such node for good (`reachable`, phase f
    /// F7); a stuck said while a running drone could take it is itself an end too early. And a stuck end
    /// gives up only what
    /// the stuck lines name and what waits on it: a drone ends too early when the converged board has
    /// another task left, claimable with its deps met, that a drone running at that end (itself included)
    /// could have taken.
    fn premature_end(&self) -> Option<String> {
        let u = self.converged();
        let refused = |s: &TaskState| !allowed(&self.cfg)(s) && s.task.role != Role::Integrate;
        // The commits a verify task judges are on node n for good, or one of an integration's are not.
        let lacks = |s: &TaskState, n: usize| s.task.of.as_ref().is_some_and(|w| !self.reachable(n, &s.node, &(w.clone(), judged_head(&u, s))));
        let integrable = |n: usize| Self::accepted(&u).iter().all(|(w, head, author)| self.reachable(n, author, &(w.clone(), head.clone())));
        let held = |x: &Stuck| match u.tasks.get(&x.task) {
            None => x.running.iter().all(|(n, _)| !integrable(*n)),
            Some(s) if s.task.role == Role::Integrate => x.running.iter().all(|(n, _)| !integrable(*n)),
            Some(s) => {
                matches!(s.status, Status::Blocked { .. })
                    || s.task.deps.iter().any(|d| !u.tasks.contains_key(d))
                    || refused(s)
                    || x.running.iter().all(|(n, d)| u.barred(s, board::Actor::new(self.name(*n), d, "")) || lacks(s, *n))
            }
        };
        if let Some(Stuck { at: t, node: n, task, why, running }) = self.stuck.iter().find(|x| !held(x)) {
            return Some(format!("{} said {task} stuck at t={t} ({why}) while one of {running:?} could take it", self.name(*n)));
        }
        let named: BTreeSet<&str> = self.stuck.iter().map(|x| x.task.as_str()).collect();
        let far = i64::MAX / 4;
        let left = |running: &[(usize, String)]| {
            u.tasks.values().find(|s| {
                !named.contains(s.task.id.as_str())
                    && !refused(s)
                    && running.iter().any(|(n, d)| board::claimable_by(&u, s, board::Actor::new(self.name(*n), d, ""), far) && !lacks(s, *n))
            })
        };
        for (n, t, running) in &self.stuck_ends {
            if let Some(s) = left(running) {
                return Some(format!("{} ended stuck at t={t} while {} was left to one of {running:?}", self.name(*n), s.task.id));
            }
        }
        let excused = self.ends.iter().all(|(_, why, _)| *why == "budget" || *why == "stuck") || self.ends.iter().any(|(_, why, _)| *why == "killed");
        let all_ended = self.drones.iter().all(|d| matches!(d.st, St::Ended(_)));
        (all_ended && !u.finished() && !excused).then(|| {
            let open: Vec<String> =
                u.tasks.values().filter(|s| !matches!(s.status, Status::Verified { .. })).map(|s| format!("{} {}", s.task.id, s.status.name())).collect();
            format!("unfinished board at every runner's end ({}): {:?}", open.join(", "), self.ends)
        })
    }

    /// No hang (phase f F6): every runner ended by the horizon. A drone that waits on the board ends only
    /// on a finish for good, its budget, a fault of its own or a stuck, so one still running at the horizon
    /// waits on something no node will write.
    fn hung(&self) -> Option<String> {
        let running: Vec<String> =
            self.drones.iter().filter(|d| !matches!(d.st, St::Ended(_))).map(|d| format!("{}.{} {:?}", self.name(d.node), d.name, d.st)).collect();
        (!running.is_empty()).then(|| format!("still running at the horizon t={}: {running:?}", self.cfg.horizon))
    }

    /// Stable-prefix exit: a runner exits 0 only when the converged board is finished.
    fn exit_unfinished(&self) -> Option<String> {
        let u = self.converged();
        self.exits
            .iter()
            .find(|(_, (ok, _))| *ok && !u.finished())
            .map(|(n, (_, t))| format!("{} exited 0 at t={t}; the converged board is unfinished", self.name(*n)))
    }

    /// S2': whatever took effect in a node's fold is in the converged board.
    fn retracted(&self) -> Option<String> {
        let u = self.converged();
        let kept = |x: &Seen| match x {
            Seen::Done(t, by) => u.spans.iter().any(|sp| sp.task == *t && sp.by == *by && sp.end == Some(Ev::Done)),
            Seen::Verdict(t, by) => u.spans.iter().any(|sp| sp.task == *t && sp.by == *by && matches!(sp.end, Some(Ev::Verified | Ev::Rejected))),
            // The epoch stays integrated; of two integrators that both passed under a suspicion, the
            // verdict that closed the task is its one integration on every board, the other a verdict kept
            // as its own (`Seen::Verdict`).
            Seen::Integrated(t, _) => u.integrations.iter().any(|i| i.task == *t),
            // A reopen stands until the task is verified (a pass of the round, sticky, may sort first).
            Seen::Reopened(t, r) => u.tasks.get(t).is_some_and(|s| s.rework >= *r || matches!(s.status, Status::Verified { .. })),
        };
        let gone: Vec<&Seen> = self.seen.iter().filter(|x| !kept(x)).collect();
        (!gone.is_empty()).then(|| format!("took effect in a node's fold, void in the converged board: {gone:?}"))
    }

    /// Integrate-once: one integrator merged per integrate task (and so ran the oracle; its verdict only
    /// while it still holds the claim).
    fn integrated_twice(&self) -> Option<String> {
        self.integrations.iter().find(|(_, by)| by.len() > 1).map(|(t, by)| format!("{t} integrated in full by {by:?}"))
    }

    /// Integrate-once on the board: one integration per integrate task in the converged board, whatever
    /// integrators ran (two under a suspicion, `integrated_twice`).
    fn integrations_twice(&self) -> Option<String> {
        let u = self.converged();
        let mut per: BTreeMap<&str, usize> = BTreeMap::new();
        for i in &u.integrations {
            *per.entry(i.task.as_str()).or_default() += 1;
        }
        per.iter().find(|(_, n)| **n > 1).map(|(t, n)| format!("{t} integrated {n} times on the converged board: {:?}", u.integrations))
    }

    /// Same-node exclusion: one session of a node won its read-back per task attempt.
    fn same_node_twice(&self) -> Option<String> {
        self.won.iter().find(|(_, by)| by.len() > 1).map(|((n, t, r), by)| format!("{t} (rework {r}) held after the read-back on {n} by {by:?}"))
    }

    /// What the run did, read without a clock: its trace up to the first integration, each line's
    /// stamps left out (two nodes' integrators claim one integration in one tick, CE4, and the lower
    /// stamp wins once both claims are final, phase f F5; who wins moves the ends after it), each drone's end and
    /// its iterations.
    fn outcome(&self) -> (Vec<String>, BTreeSet<(String, &'static str)>, Vec<u32>) {
        let unstamped = |l: &String| l.split(' ').map(|w| w.split_once('@').map_or(w, |(w, _)| w)).collect::<Vec<_>>().join(" ");
        let trace = self.log.iter().take_while(|l| !l.contains("integrate")).map(unstamped).collect();
        (trace, self.ends.iter().map(|(d, why, _)| (d.clone(), *why)).collect(), self.drones.iter().map(|d| d.iters).collect())
    }

    /// Every property but the ones `skip` names, with the trace.
    fn check(&self, skip: &[&str]) {
        let props: [(&str, Option<String>); 7] = [
            ("end", self.premature_end()),
            ("hang", self.hung()),
            ("exit", self.exit_unfinished()),
            ("S2'", self.retracted()),
            ("integrate", self.integrated_twice()),
            ("integration", self.integrations_twice()),
            ("same-node", self.same_node_twice()),
        ];
        for (name, v) in props {
            if let Some(v) = v.filter(|_| !skip.contains(&name)) {
                panic!("{name}: {v}\n{}", self.trace());
            }
        }
    }
}

/// A seeded shuffle (xorshift): the S1 check needs orders, not randomness.
fn shuffled<T: Clone>(v: &[T], seed: u64) -> Vec<T> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v = v.to_vec();
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
    v
}

/// S1: the fold of `lines` in any order is one board.
fn same_in_any_order(lines: &[Event]) -> Result<(), String> {
    let b = board::fold(lines);
    let mut orders: Vec<Vec<Event>> = (0..64).map(|seed| shuffled(lines, seed)).collect();
    orders.push(lines.iter().rev().cloned().collect());
    match orders.iter().position(|o| board::fold(o) != b) {
        Some(i) => Err(format!("order {i} of {} lines folds another board", lines.len())),
        None => Ok(()),
    }
}

/// A peer slow to claim (hivemc A; CE1, bug #23): one task, a's; a's model takes 12 s to claim it. b's
/// drone waits while a's session started and has not claimed instead of ending after three idle polls
/// (phase f F1's `in_hand`; since F6 a drone ends on no idle poll at all), and verifies a's work. No
/// premature end.
#[test]
fn a_ce1_a_peer_slow_to_claim_keeps_the_other_node_waiting() {
    let r = Sim::run(Cfg::new(&[("a", None)]).lat("a", "d1", &[12]));
    r.check(&[]);
    assert!(r.converged().finished(), "{}", r.trace());
}

/// A dependency chain (hivemc B; CE2): a's two tasks, the second after the first; b's model takes 12 s
/// to claim the first one's verify task. a's drone, which may claim nothing until b's verdict, waits on
/// b's unsettled session (F1's `in_hand`; since F6 it waits on any unfinished board) and then does the
/// second. No premature end.
#[test]
fn b_ce2_a_peer_session_on_its_way_to_a_dep_keeps_the_waiting_drone() {
    let r = Sim::run(Cfg::new(&[("a", None), ("a", Some(0))]).lat("b", "d1", &[12, 1]));
    r.check(&[]);
    assert!(r.converged().finished(), "{}", r.trace());
}

/// Two drones on a, one on b, a task each (hivemc C): every property, over a sweep of claim latencies.
#[test]
fn c_two_drones_on_one_node() {
    for cfg in Cfg::new(&[("a", None), ("b", None)]).node("a", |n| n.drones = 2).sweep(&[1, 6]) {
        Sim::run(cfg).check(&[]);
    }
}

/// A slow network (hivemc D): lines take 3 s. Every property, integrate-once too since phase f F5
/// (`d_integrates_once`).
#[test]
fn d_a_slow_network() {
    for cfg in Cfg::new(&[("a", None), ("b", None)]).sweep(&[1, 6]) {
        Sim::run(Cfg { delay: 3, ..cfg }).check(&[]);
    }
}

/// hivemc D's integration: with lines taking 3 s, both nodes see the last verdict and claim the
/// integration (CE4); each claim waits until it is final (phase f F5: the other's watermark past its
/// stamp), by then each node holds the other's claim, and only the one that sorts first integrates.
#[test]
fn d_integrates_once() {
    for cfg in Cfg::new(&[("a", None), ("b", None)]).sweep(&[1, 6]) {
        let r = Sim::run(Cfg { delay: 3, ..cfg });
        assert_eq!(r.integrated_twice(), None, "{}", r.trace());
    }
}

/// Clock skew (hivemc E): b's clock 2 s behind, lines take 2 s. Every property.
#[test]
fn e_clock_skew() {
    for cfg in Cfg::new(&[("a", None), ("b", None)]).node("b", |n| n.skew = -2000).sweep(&[1, 6]) {
        Sim::run(Cfg { delay: 2, ..cfg }).check(&[]);
    }
}

/// b's runner starts 6 s after a's (hivemc F): every property.
#[test]
fn f_a_late_start() {
    for cfg in Cfg::new(&[("a", None), ("b", None)]).node("b", |n| n.start = 6).sweep(&[1, 6]) {
        Sim::run(cfg).check(&[]);
    }
}

/// No bundles cross (hivemc G): each attempt's bundle is over 1 MiB, so its node says its commits stay
/// there (commits.rs `publish`), and each node's one drone may verify only the other's work. hivemc let
/// it be verified and stopped at the integration (`elsewhere`); before phase f F7 the runner offered the
/// verify task (`open_to` had no commits filter) and its sessions found nothing they could claim, so both
/// runners ran empty sessions and ended "stalled", the board unfinished. The property: no session for a
/// task whose commits a node lacks, no end but an explicit stuck: each node declines the other's verify
/// task (mod.rs `lacking`: `Unavailable`), and once both declines are in each says both stuck,
/// `commits unavailable`, whose constraint held (no running node holds or can get the commits it was
/// not barred from: premature_end checks it), and exits 1.
#[test]
fn g_unfetchable_commits_end_no_runner() {
    let r = Sim::run(Cfg { bundles: false, ..Cfg::new(&[("a", None), ("b", None)]) });
    r.check(&[]);
    assert_eq!(r.empty, Vec::<String>::new(), "no session for a task whose commits its node lacks\n{}", r.trace());
    assert!(r.ends.iter().all(|(_, why, _)| *why == "stuck"), "{:?}\n{}", r.ends, r.trace());
    let verify: BTreeSet<String> = r.cfg.tasks.iter().map(|(t, _)| format!("{t}.v1")).collect();
    let said: BTreeSet<String> = r.stuck.iter().filter(|x| x.why.starts_with("commits unavailable")).map(|x| x.task.clone()).collect();
    assert_eq!(said, verify, "{:?}\n{}", r.stuck, r.trace());
    assert!(r.exits.values().all(|(ok, _)| !*ok), "{}", r.trace());
}

/// hivemc G with a's clock 20 s and 60 s ahead, and either way: b's end is the same as with one clock.
/// Before phase f F7 b's sessions refused the verify task they were offered, stamped by a's done, and
/// the stall was judged by the session's own start view (run.rs `stalls`), so b ended "stalled" after
/// as many iterations for every skew. Since F7 b is offered no task whose commits it lacks: it ends stuck
/// after its one work iteration for every skew, its stuck's grace on its own clock (`claimable_since`).
#[test]
fn g_a_peer_clock_ahead_changes_no_end() {
    let ends = |skew: i64| {
        let r = Sim::run(Cfg { bundles: false, ..Cfg::new(&[("a", None), ("b", None)]) }.node("a", |n| n.skew = skew));
        let b = r.drones.iter().find(|d| r.name(d.node) == "b").unwrap();
        (b.iters, r.ends.iter().find(|(d, _, _)| d == "b.d1").map(|(_, why, _)| *why), r.empty.clone(), r.trace())
    };
    let (iters, why, empty, trace) = ends(0);
    assert_eq!((iters, why, empty), (1, Some("stuck"), Vec::new()), "{trace}");
    for skew in [20_000, 60_000, 10_000, -10_000, 300_000, -300_000] {
        let (n, why, empty, trace) = ends(skew);
        assert_eq!((n, why, empty), (iters, Some("stuck"), Vec::new()), "a's clock {skew} ms ahead\n{trace}");
    }
}

/// Phase f F7: a bundle that arrives late. Each attempt's bundle line reaches the other node 15 ticks after
/// its done (a mesh round of its own, a big line: past `stuck`'s grace), lines in 1 to 3 s. Each node's drone, its own work
/// done, waits for the other's commits with nothing open (mod.rs `lacking`: `Coming`, its node a taker):
/// no session starts for the verify task before its bundle line is held there, none runs empty, and the
/// goal is finished. Fails with the runner's offer unfiltered (run.rs `open_to` without `offered`): each
/// drone runs sessions that find nothing to claim and ends "stalled"; and with a missing bundle read as
/// unavailable at once, each node declines the other's verify task and ends stuck.
#[test]
fn a_bundle_that_arrives_late_is_waited_for() {
    for delay in [1, 3] {
        let r = Sim::run(Cfg { delay, bundle_late: SUSPECT_MS / TICK + 5, ..Cfg::new(&[("a", None), ("b", None)]) });
        r.check(&[]);
        assert!(r.converged().finished(), "delay {delay}\n{}", r.trace());
        assert_eq!(r.empty, Vec::<String>::new(), "delay {delay}\n{}", r.trace());
        for (node, other) in [("a", "b"), ("b", "a")] {
            let work = r.cfg.tasks.iter().map(|(t, _)| t).find(|t| board::owner(&BTreeSet::from(["a", "b"]), t) == Some(other)).unwrap();
            let held = r.first(node, &format!("holds the bundle line of {work}@")).unwrap_or(i64::MAX);
            let claimed = r.first(node, &format!("claim {work}.v1@")).unwrap_or(i64::MIN);
            assert!(held <= claimed, "delay {delay}: {node} claimed {work}.v1 at t={claimed}, its bundle there at t={held}\n{}", r.trace());
        }
        assert!(r.stuck.is_empty(), "{:?}\n{}", r.stuck, r.trace());
    }
}

/// Phase f F7: an author that leaves before its bundle reached a peer. b's bundle line takes 40 ticks;
/// b's runner is killed at t=9, after its done. a waits for b's commits while b may still take part
/// (`Board::takers`: heard within `LEASE_MS`), then declines b's verify task and says it stuck,
/// `commits unavailable`, once b was silent a lease: a cut that long is given up, and no line from b
/// can bring them. Before F7 a's drone ran empty sessions on it and ended "stalled".
#[test]
fn an_author_gone_before_its_bundle_arrived_leaves_its_task_stuck() {
    let lease = LEASE_MS / TICK;
    let cfg = Cfg { bundle_late: lease + 300, horizon: lease + 120, ..Cfg::new(&[("a", None), ("b", None)]) }.node("b", |n| n.stop = Some(9));
    let r = Sim::run(cfg);
    r.check(&[]);
    assert_eq!(r.empty, Vec::<String>::new(), "{}", r.trace());
    let bwork = r.cfg.tasks.iter().map(|(t, _)| t).find(|t| board::owner(&BTreeSet::from(["a", "b"]), t) == Some("b")).unwrap();
    let x = r.stuck.iter().find(|x| x.task == format!("{bwork}.v1")).unwrap_or_else(|| panic!("{:?}\n{}", r.stuck, r.trace()));
    assert!(x.why.contains("left the goal") && x.at > lease, "{:?}\n{}", r.stuck, r.trace());
    assert_eq!(r.ends.iter().find(|(d, _, _)| d == "a.d1").map(|(_, why, _)| *why), Some("stuck"), "{}", r.trace());
}

/// Phase f F7: an integration no node can hold together. Two drones a node, each node's work verified by
/// its other drone, no bundles: a holds only its attempt's commits, b only its own. Before F7 each
/// runner ended "integration needs another node's commits", an end no stuck explained; now each says the
/// integration stuck, `commits unavailable`, whose constraint held (no running node can hold every
/// accepted attempt: premature_end checks it), and exits 1.
#[test]
fn an_integration_no_node_can_hold_together_ends_stuck() {
    let cfg = Cfg { bundles: false, ..Cfg::new(&[("a", None), ("b", None)]) }.node("a", |n| n.drones = 2).node("b", |n| n.drones = 2);
    let r = Sim::run(cfg);
    r.check(&[]);
    let u = r.converged();
    assert!(u.tasks.values().filter(|s| s.task.role != Role::Integrate).all(|s| matches!(s.status, Status::Verified { .. })), "{}", r.trace());
    assert!(
        !r.stuck.is_empty() && r.stuck.iter().all(|x| x.task.starts_with("integrate") && x.why.starts_with("commits unavailable")),
        "{:?}\n{}",
        r.stuck,
        r.trace()
    );
    assert!(r.ends.iter().all(|(_, why, _)| *why == "stuck") && r.exits.values().all(|(ok, _)| !*ok), "{:?}\n{}", r.ends, r.trace());
}

/// Phase f F7: an integration waits for the one node that holds its commits. a's two drones do and
/// verify both tasks, no bundles; b's drone may take neither verify task (`Unavailable`: declined) and
/// lacks both attempts at the integration, all a's (integrate.rs `gate`: one taker holds them all,
/// `Coming`): it waits while a integrates, and ends on the finish. Before F7 b's runner ended
/// "integration needs another node's commits" while a was about to integrate.
#[test]
fn an_integration_waits_for_the_one_node_that_holds_its_commits() {
    let cfg = Cfg { bundles: false, ..Cfg::new(&[("a", None), ("a", None)]) }.node("a", |n| n.drones = 2);
    let r = Sim::run(cfg);
    r.check(&[]);
    assert!(r.converged().finished(), "{}", r.trace());
    assert_eq!(r.ends.iter().find(|(d, _, _)| d == "b.d1").map(|(_, why, _)| *why), Some("goal finished"), "{:?}\n{}", r.ends, r.trace());
    assert!(r.exits.values().all(|(ok, _)| *ok), "{}", r.trace());
}

/// Phase f F7: two nodes that each refuse the other's bundle line (a prerequisite its repo lacks), the
/// lines carrying the commits. Two drones a node, so each node's work is verified by its other drone (the
/// other node's verify task declined: `Unavailable`). At the integration each lacks the other's attempt,
/// whose line travels: before, each read the other as the one holder that integrates (integrate.rs
/// `gate`: `Coming`), and both waited for ever. Now each declines the integration (mod.rs
/// `decline_integration`), and once both declines are in each says it stuck, `commits unavailable`,
/// whose constraint held (premature_end), and exits 1: no hang at the horizon. Fails with the holder
/// rule reading no declines: both still waiting at the horizon.
#[test]
fn nodes_that_refuse_each_others_lines_end_stuck_at_the_integration() {
    let two = |n: &mut NodeCfg| {
        n.drones = 2;
        n.refuses = true;
    };
    let r = Sim::run(Cfg::new(&[("a", None), ("b", None)]).node("a", two).node("b", two));
    r.check(&[]);
    let u = r.converged();
    assert!(u.tasks.values().filter(|s| s.task.role != Role::Integrate).all(|s| matches!(s.status, Status::Verified { .. })), "{}", r.trace());
    let id = board::integrate_id(&u);
    assert!(u.declines("a", &id) && u.declines("b", &id), "{:?}\n{}", u.declined, r.trace());
    assert!(!r.stuck.is_empty() && r.stuck.iter().all(|x| x.task == id && x.why.starts_with("commits unavailable")), "{:?}\n{}", r.stuck, r.trace());
    assert!(r.ends.iter().all(|(_, why, _)| *why == "stuck") && r.exits.values().all(|(ok, _)| !*ok), "{:?}\n{}", r.ends, r.trace());
}

/// Phase f F7: three nodes, an integration two of them cannot hold and one can. Two drones a node; b's
/// commits stay on b (over the cap), a refuses every other node's line. a lacks b's attempt (it stays)
/// and c's (refused): two holders, and before, integrate.rs `gate` waited only for a single one, so a
/// said the integration stuck at once and ended, exit 1, while b, which holds its own and takes every
/// travelling line, integrated. Now a waits while a taker that can hold every attempt has not declined
/// the integration: no stuck, every runner ends on the finish.
#[test]
fn an_integration_waits_for_any_node_that_can_hold_its_commits() {
    let mut cfg = Cfg::new(&[("a", None), ("b", None), ("a", None), ("b", None)]);
    cfg.nodes.push(NodeCfg { name: "c", ..cfg.nodes[0].clone() });
    // a starts late, so b and c do the work.
    let cfg = cfg
        .node("a", |n| {
            n.refuses = true;
            n.start = 4;
        })
        .node("b", |n| n.stays = true);
    let cfg = ["a", "b", "c"].into_iter().fold(cfg, |c, n| c.node(n, |n| n.drones = 2));
    let r = Sim::run(cfg);
    r.check(&[]);
    let u = r.converged();
    let by: BTreeSet<&str> = u.tasks.values().filter(|s| s.task.role == Role::Work).filter_map(|s| s.attempts.first()).map(|a| a.node.as_str()).collect();
    assert!(by.contains("b") && by.contains("c"), "work done on b and c: {by:?}\n{}", r.trace());
    assert!(u.finished() && r.stuck.is_empty(), "{:?}\n{}", r.stuck, r.trace());
    assert!(r.exits.values().all(|(ok, _)| *ok), "{:?}\n{}", r.ends, r.trace());
}

/// Phase f F7: a done whose bundle line is never written: a's session is cut during `git bundle create`
/// after its done landed (mod.rs `finish`). b waits for the line (mod.rs `lacking`: `Coming` while a
/// takes part) and a leaves its verify task to b (no decline): before, both waited for ever. Now a's
/// runner writes the missing line on its next pass (commits.rs `republish`): b claims the verify task
/// only once the line is held there, no session runs empty, and the goal is finished. Fails without the
/// pass: both still waiting at the horizon.
#[test]
fn a_done_whose_bundle_line_was_never_written_is_published_by_its_runner() {
    let mut cfg = Cfg::new(&[("a", None), ("b", None)]);
    let awork = cfg.tasks.iter().map(|(t, _)| t.clone()).find(|t| board::owner(&BTreeSet::from(["a", "b"]), t) == Some("a")).unwrap();
    cfg.unpublished = vec![awork.clone()];
    let r = Sim::run(cfg);
    r.check(&[]);
    assert!(r.converged().finished(), "{}", r.trace());
    assert_eq!(r.empty, Vec::<String>::new(), "{}", r.trace());
    let cut = r.first("a", &format!("cut before the bundle line of {awork}@")).unwrap_or_else(|| panic!("{}", r.trace()));
    let again = r.first("a", &format!("republishes the bundle line of {awork}@")).unwrap_or(i64::MAX);
    let held = r.first("b", &format!("holds the bundle line of {awork}@")).unwrap_or(i64::MAX);
    let claimed = r.first("b", &format!("claim {awork}.v1@")).unwrap_or(i64::MIN);
    assert!(cut <= again && again < held && held <= claimed, "cut t={cut}, republished t={again}, held on b t={held}, claimed t={claimed}\n{}", r.trace());
}

/// a's runner is killed at t=4 holding its task (hivemc H). Before phase f F4 b waited on the lease
/// (`in_flight`), 30 min, and never took the task back. The property: a node gone silent hands its
/// tasks back within seconds: a's runner beats no more, b suspects it `SUSPECT_MS` after the last line
/// it held of a (`Board::participants`), and a's drone's lease counts as expired there (`Local::gone`):
/// b's claim names a's last renewal (`board::takes`) and the fold takes it within the lease.
#[test]
fn h_a_killed_holder_hands_its_task_back() {
    let cfg = Cfg { horizon: 120, ..Cfg::new(&[("a", None), ("b", None)]) }.node("a", |n| n.stop = Some(4));
    let r = Sim::run(cfg);
    let t = &r.cfg.tasks[0].0;
    let taken = r.converged().spans.iter().any(|sp| sp.task == *t && sp.node == "b");
    assert!(taken, "{t} still a's 2 minutes after a's runner died\n{}", r.trace());
}

/// hivemc H ends no runner early: b takes a's task back once a is suspected (phase f F4; before, b
/// waited on a's lease), does it, and then waits: only a could verify either task, and a killed node is
/// told from a cut one only by time. Phase f F6: no end on nothing to do; b ends stuck once a was silent a
/// whole lease (`Board::takers`), a cut that long given up, with a stuck line naming both verify tasks,
/// whose constraint held (premature_end checks it), exit non-zero.
#[test]
fn h_a_killed_holder_ends_no_runner_early() {
    let lease = LEASE_MS / TICK;
    let r = Sim::run(Cfg { horizon: lease + 120, ..Cfg::new(&[("a", None), ("b", None)]) }.node("a", |n| n.stop = Some(4)));
    r.check(&[]);
    assert_eq!(r.ends.iter().find(|(d, _, _)| d == "b.d1").map(|(_, why, _)| *why), Some("stuck"), "{}", r.trace());
    let stuck: BTreeSet<&str> = r.stuck.iter().map(|x| x.task.as_str()).collect();
    let verify: BTreeSet<String> = r.cfg.tasks.iter().map(|(t, _)| format!("{t}.v1")).collect();
    assert_eq!(stuck, verify.iter().map(String::as_str).collect(), "{}", r.trace());
    assert!(r.stuck.iter().all(|x| x.at > lease), "not before a was silent a lease\n{}", r.trace());
    assert_eq!(r.exits.get(&1).map(|(ok, _)| *ok), Some(false), "{}", r.trace());
}

/// a has the budget for one iteration and its model is slow to claim (hivemc I): b verifies a's work.
#[test]
fn i_a_budget_of_one_iteration() {
    let r = Sim::run(Cfg::new(&[("a", None)]).lat("a", "d1", &[12]).node("a", |n| n.share = 1));
    r.check(&[]);
    assert!(r.converged().finished(), "{}", r.trace());
}

/// Named claims (hivemc J; G1): the model claims the task its prompt offered, by name (`picked` false,
/// no `left_to_another` refusal, no retry); b starts 2 s late. Every property.
#[test]
fn j_named_claims() {
    for cfg in Cfg::new(&[("a", None), ("b", None)]).node("b", |n| n.start = 2).sweep(&[1, 6]) {
        Sim::run(Cfg { named: true, ..cfg }).check(&[]);
    }
}

/// The integration race (hivemc K; CE4): lines take 2 s, an integration 5. Both nodes' drones see the
/// last verdicts within a tick of each other and claim the integrate task; neither read-back has the
/// other's claim, and the integrator never folded again, so both merged, ran the oracle and wrote their
/// verdict. Integrate-once: each claim now waits until it is final (phase f F5), the loser learns before
/// its merge, and the integrator folds again before its verdict.
#[test]
fn k_ce4_two_nodes_integrate_once() {
    let r = Sim::run(Cfg { delay: 2, integrate: 5, ..Cfg::new(&[("a", None), ("b", None)]) });
    assert_eq!(r.integrated_twice(), None, "{}", r.trace());
}

/// hivemc K's other properties: no result is lost (every property, integrate-once included).
#[test]
fn k_the_integration_race_loses_no_result() {
    Sim::run(Cfg { delay: 2, integrate: 5, ..Cfg::new(&[("a", None), ("b", None)]) }).check(&[]);
}

/// One task, a's; a starts 3 s late, b's clock is 2 s behind, lines take 2 s, work 5 (hivemc L):
/// every property.
#[test]
fn l_a_late_owner_with_skew() {
    let cfg = Cfg { delay: 2, work: 5, ..Cfg::new(&[("a", None)]) }.node("a", |n| n.start = 3).node("b", |n| n.skew = -2000);
    for cfg in cfg.sweep(&[1, 2, 3]) {
        Sim::run(cfg).check(&[]);
    }
}

/// CE3 (hivemc L at HEAD): a session that finds its work only as it ends. a starts 1 s late, lines take
/// 2 s, b's model 8 s to claim (5 before phase f F5, whose final wait puts a's done 3 s later), work 1. b's first session starts on t0 (a's roster not here yet) and
/// claims after a's claim landed, so it writes nothing, and as it ends a's done has opened t0.v1 to b.
/// A task that became claimable during the session is no stall (run.rs `stalls` by the session's start
/// view, the CE3 fix), so b verifies instead of counting it. No premature end. (At HEAD this needed b's
/// clock 2 s behind, which lengthened a's grace on b; phase f F3 times the grace on b's clock, so the
/// skew no longer changes it: `skew_changes_nothing_a_node_does`.)
#[test]
fn ce3_work_that_opens_during_a_session_is_no_stall() {
    let cfg = Cfg { delay: 2, work: 1, ..Cfg::new(&[("a", None)]) }.node("a", |n| n.start = 1).lat("b", "d1", &[8]);
    let r = Sim::run(cfg);
    let t = r.first("b", "d1 wrote nothing; t0.v1 became claimable during its session: no stall");
    assert!(t.is_some(), "b's empty session found a's work as it ended\n{}", r.trace());
    assert!(r.first("b", "verified t0.v1") > t, "b verifies a's work\n{}", r.trace());
    r.check(&[]);
}

/// CE5: b's clock is 2 s behind and lines take 5 s, work 2. b claims a's task in the tick a does,
/// stamped 2 s earlier, before a's roster reached it; a's read-back and confirm window see nothing, a
/// works and its done takes effect in a's fold, until b's claim lands and voids it (and its verify
/// task). S2'. Since phase f F5 a's claim waits until it is final, b's watermark past it, which b's claim
/// (earlier in b's file) reaches a before: a loses before any work, and a done would be for good anyway.
#[test]
fn ce5_a_done_that_took_effect_is_never_voided() {
    let cfg = Cfg { delay: 5, work: 2, ..Cfg::new(&[("a", None)]) }.node("b", |n| n.skew = -2000);
    let r = Sim::run(cfg);
    assert_eq!(r.retracted(), None, "{}", r.trace());
}

/// CE5 with b suspected: as `ce5_a_done_that_took_effect_is_never_voided`, two drones a node (a node
/// that did an attempt verifies none of its rival's), and every line written from t=1 until `heal` kept
/// from the other node until then, so the joins arrive and the claims do not. Each node's claim waits
/// on the other's watermark until it suspects the other (`SUSPECT_MS`), then proceeds: both work, and
/// both dones take effect in their own folds. b's claim, stamped 2 s earlier, wins the converged fold.
/// Healed at t=30, each node's verifier has judged its own node's attempt in the cut.
fn ce5_suspected(heal: i64) -> Cfg {
    let cfg = Cfg { delay: 5, work: 2, cuts: vec![(1, heal)], horizon: 120, ..Cfg::new(&[("a", None)]) };
    cfg.node("a", |n| n.drones = 2).node("b", |n| (n.drones, n.skew) = (2, -2000))
}

/// Phase f F5, after review: `ce5_suspected`. The final wait no longer protects a's done, only the
/// monotone results do: a's done is kept beside b's, b's the task's (S2'), and every property holds.
/// Fails with `Fold::permanent` never true: a's done, which took effect in a's fold, is voided once
/// b's earlier claim arrives.
#[test]
fn ce5_a_done_under_a_suspicion_is_never_voided() {
    let r = Sim::run(ce5_suspected(20));
    let t = &r.cfg.tasks[0].0;
    let u = r.converged();
    let done: Vec<&str> = u.spans.iter().filter(|sp| sp.task == *t && sp.end == Some(Ev::Done)).map(|sp| sp.node.as_str()).collect();
    assert_eq!(done.len(), 2, "both did it: {done:?}\n{}", r.trace());
    assert!(matches!(&u.tasks[t].status, Status::Verified { .. }), "{}", r.trace());
    assert!(r.first("a", &format!("done {t}@")).is_some() && r.first("b", &format!("done {t}@")).is_some(), "both did it\n{}", r.trace());
    assert_eq!(r.retracted(), None, "{}", r.trace());
    r.check(&["integrate"]);
}

/// Phase f F5, after review: both nodes' integrators finish under a suspicion. Two drones a node, a task
/// each; the network is cut from t=6 for a minute, so each node suspects the other, takes the other's
/// task over, verifies and integrates on its own. Both merge (a duplicate, `integrated_twice`), and
/// once the cut heals the converged board has one integration of the epoch (only the verdict that
/// closed the integrate task integrates; the other is an attempt), every other property holding. Fails
/// with every verdict for good integrating: two integrations of integrate.1, `integrated` moving to
/// the later-arriving one.
#[test]
fn integrators_that_both_finish_under_a_suspicion_integrate_once_on_the_board() {
    let cfg = Cfg { cuts: vec![(6, 66)], horizon: 160, ..Cfg::new(&[("a", None), ("b", None)]) }.node("a", |n| n.drones = 2).node("b", |n| n.drones = 2);
    let r = Sim::run(cfg);
    let both: Vec<&String> = r.integrations.values().flatten().collect();
    assert!(both.len() >= 2, "both nodes integrated in the cut: {both:?}\n{}", r.trace());
    assert_eq!(r.integrations_twice(), None, "{}", r.trace());
    r.check(&["integrate"]);
}

/// Not in hivemc (its read-back was always in time): a's d1 stamps its claim of t (a's) at t=2, and the
/// line lands in the board's file 5 s later (a stall between the pre-write fold and the append, under
/// load); its read-back follows. a started a moment before b, so a's d2 started its session on u (b's,
/// in a's share while a knew no peer, #22); b's d1 claims u, and at t=5 d2 claims what is left, t
/// (`next_for`'s last resort): its read-back has no rival. d1's line lands with the earlier stamp and
/// wins its read-back too: two sessions of a held t past their read-back, d2 is told after it started.
/// Same-node exclusion without a timing assumption.
#[test]
fn same_node_claims_need_no_timing() {
    let mut cfg = Cfg::new(&[("a", None), ("b", None)]).node("a", |n| n.drones = 2).node("b", |n| n.start = 1).lat("a", "d2", &[4]);
    let t = cfg.tasks[0].0.clone();
    cfg.holds.push(Hold { node: "a", ev: Ev::Claim, task: t, local: 5, remote: 0 });
    let r = Sim::run(cfg);
    assert_eq!(r.same_node_twice(), None, "{}", r.trace());
}

/// Not in hivemc (it named integrators by node): an integrator's session was `integrate-<drone>-<ms>`,
/// so drone d1 of a and of b integrating in one ms shared a session; each read-back saw "its" session
/// hold the task, whichever node's line won, and both integrated even with lines in a tick.
/// Integrate-once, by phase f F8: the session names its node (integrate.rs `session`) and the fold
/// tells holders apart node for node (board.rs `Actor`), so one claim wins and the other loses.
#[test]
fn integrators_on_two_nodes_in_one_ms_are_two_sessions() {
    let r = Sim::run(Cfg::new(&[("a", None), ("b", None)]).node("b", |n| n.skew = -1));
    assert_eq!(r.integrated_twice(), None, "{}", r.trace());
    // The name alone pins half of it: the two integrators' claims name two sessions.
    let claims: BTreeSet<String> = r.union().into_iter().filter(|e| e.ev == Ev::Claim && e.task.starts_with("integrate")).map(|e| e.by).collect();
    assert_eq!(claims.len(), 2, "{claims:?}\n{}", r.trace());
}

/// Not in hivemc (spec G5): b's integrate claim is stamped before a's (b's clock 2 s behind) and reaches
/// a only after a's integration passed; b's runner is killed before its own passes. a's runner exited 0
/// on its fold, then b's claim landed and voided a's integration: the converged board unfinished, b's
/// lease on the integrate task live. Stable-prefix exit (and S2'), by phase f F5 alone: a's claim waits
/// for b's watermark, which the held claim keeps short of it (`Event::seq`) until b is suspected, and a's
/// verdict, written under its claim (`Event::claim`), is for good: the late claim takes no integration
/// back. (F6's stable-prefix exit covers what F5 does not: a late line that opens a task.)
#[test]
fn g5_a_runner_exits_0_only_on_a_stable_prefix() {
    let mut cfg = Cfg::new(&[("a", None), ("b", None)]).node("b", |n| n.skew = -2000);
    cfg.holds.push(Hold { node: "b", ev: Ev::Claim, task: "integrate.1".into(), local: 0, remote: 6 });
    let at = Sim::run(cfg.clone()).first("b", "claim integrate.1").unwrap();
    let r = Sim::run(cfg.node("b", |n| n.stop = Some(at + 1)));
    assert_eq!(r.exit_unfinished(), None, "{}", r.trace());
}

/// hivemc's liveness sweep (tasks/soundness-model/sweep.py), reduced: task sets with dep chains within
/// and across nodes, b starting at once or 2 s late with its clock 2 s either side of a's, lines in 1 or
/// 2 s, each drone's claims after 1 or 12 s (or every drone's after 4), work and integrations 4. Every
/// property, integrate-once too since phase f F5 (CE4, `k_ce4_two_nodes_integrate_once`).
#[test]
fn a_reduced_liveness_sweep() {
    let sets: [&[(&str, Option<usize>)]; 4] =
        [&[("a", None), ("b", None)], &[("a", None), ("a", Some(0))], &[("a", None), ("b", Some(0))], &[("a", None), ("b", None), ("a", Some(1))]];
    for tasks in sets {
        for start in [0, 2] {
            for skew in [0, 2000, -2000] {
                for delay in [1, 2] {
                    let cfg = Cfg { delay, work: 4, integrate: 4, ..Cfg::new(tasks) }.node("b", |n| (n.start, n.skew) = (start, skew));
                    for cfg in [cfg.sweep(&[1, 12]), cfg.sweep(&[4])].concat() {
                        Sim::run(cfg).check(&[]);
                    }
                }
            }
        }
    }
}

/// S1 on every scenario's lines: the fold of any order of them is one board.
#[test]
fn s1_every_scenarios_lines_fold_to_one_board_in_any_order() {
    let two = || Cfg::new(&[("a", None), ("b", None)]);
    let cfgs = [
        Cfg::new(&[("a", None)]).lat("a", "d1", &[12]),
        Cfg::new(&[("a", None), ("a", Some(0))]).lat("b", "d1", &[12, 1]),
        two().node("a", |n| n.drones = 2),
        Cfg { delay: 3, ..two() },
        Cfg { delay: 2, ..two() }.node("b", |n| n.skew = -2000),
        Cfg { bundles: false, ..two() },
        two().node("a", |n| n.stop = Some(4)),
        Cfg { delay: 2, integrate: 5, ..two() },
        Cfg { delay: 5, work: 2, ..Cfg::new(&[("a", None)]) }.node("b", |n| n.skew = -2000),
    ];
    for cfg in cfgs {
        let r = Sim::run(cfg);
        if let Err(e) = same_in_any_order(&r.union()) {
            panic!("{e}\n{}", r.trace());
        }
    }
}

/// Not in hivemc: two drones of a runner ending in one ms wrote two roster lines of one ms before stamps
/// were a hybrid clock (run.rs `main`, each end names the drones still running; a log from then, or a
/// node not upgraded), and the fold kept the one later in its file (board.rs `fold`'s `rosters`, `at <=
/// e.ms`): two nodes holding both lines in another order folded two boards. The pre-pass now reads the
/// lines in the fold's keyed order, the same lines' hash breaking the tie. S1.
#[test]
fn s1_two_roster_lines_of_one_ms_fold_to_one_board() {
    let roster = |names: &[&str]| {
        let names: Vec<String> = names.iter().map(|d| d.to_string()).collect();
        Event { node: "a".into(), ..Event::new(Ev::Heartbeat, "", "runner", 5).with_note(&board::roster_note(&names)) }
    };
    let lines = [Event::open(work_task("t1", Role::Work, &[]), "engineer:e", 0), roster(&["d2"]), roster(&[])];
    assert_eq!(same_in_any_order(&lines), Ok(()));
}

/// The skews of the fault matrix (tasks/hive-soundness.md §5): 10 s, past `OWNER_GRACE_MS`, and 5 min,
/// either way.
const SKEWS: [i64; 4] = [10_000, -10_000, 300_000, -300_000];

/// Phase f F3: every age the runner and the claim read is measured on the observer's clock (board.rs
/// `Local`), so a node's clock 10 s or 5 min off its peer's changes nothing it does. hivemc A (CE1: the
/// owner's grace and its slow session), B (CE2: a peer session on its way to a dep), F (a late start)
/// and CE3 (a session that finds its work only as it ends: the stall count), each node's clock skewed
/// in turn: the same tenures, the same ends at the same ticks, the same iterations as with one clock.
/// Before, `fresh` and `live_nodes` read `now` against the writer's stamp: a's clock 10 s behind made
/// its tasks never fresh on b (G3), and b raced a's slow session for them.
#[test]
fn skew_changes_nothing_a_node_does() {
    let cfgs = [
        Cfg::new(&[("a", None)]).lat("a", "d1", &[12]),
        Cfg::new(&[("a", None), ("a", Some(0))]).lat("b", "d1", &[12, 1]),
        Cfg::new(&[("a", None), ("b", None)]).node("b", |n| n.start = 6),
        Cfg { delay: 2, work: 1, ..Cfg::new(&[("a", None)]) }.node("a", |n| n.start = 1).lat("b", "d1", &[8]),
    ];
    for cfg in cfgs {
        let base = Sim::run(cfg.clone());
        base.check(&[]);
        for (node, skew) in ["a", "b"].into_iter().flat_map(|n| SKEWS.map(|s| (n, s))) {
            let r = Sim::run(cfg.clone().node(node, |n| n.skew = skew));
            r.check(&[]);
            assert_eq!(r.outcome(), base.outcome(), "{node}'s clock {skew} ms off\n{}\n---- one clock:\n{}", r.trace(), base.trace());
        }
    }
}

/// A session hung on a node that still beats keeps its claim for the whole lease (phase f F4: a node's
/// liveness apart from the work lease), its clock 10 s or 5 min off b's (F3): a's only drone claims its
/// task and its work never ends, no hook renews the lease, a's runner beats on. On b, a stays a
/// participant and its task stays held until `LEASE_MS` after b held a's claim, then turns claimable at
/// the same tick for every skew: b times the lease from when it held the renewal, on its own clock (a,
/// the owner, takes it again within its grace). Before F3 the lease was `now` against a's stamps: a's
/// clock 5 min ahead kept its task 5 min longer, 5 min behind handed it over 5 min early.
#[test]
fn a_dead_holders_lease_and_liveness_run_out_on_the_observers_clock() {
    let horizon = LEASE_MS / TICK + 20;
    let run = |skew: i64| {
        let r = Sim::run(Cfg { horizon, work: 3 * horizon, ..Cfg::new(&[("a", None)]) }.node("a", |n| n.skew = skew));
        let t = r.cfg.tasks[0].0.clone();
        // b's board as it stood at tick `at`, on b's clock.
        let on_b = |at: i64| board::fold(&r.lines[1].iter().filter(|e| e.seen.0.is_some_and(|ms| ms <= r.clock(1, at))).cloned().collect::<Vec<_>>());
        let freed = (LEASE_MS / TICK - 5..horizon).find(|&at| on_b(at).tasks[&t].claimable(r.clock(1, at)));
        for at in (3..LEASE_MS / TICK - 5).step_by(97) {
            let b = on_b(at);
            assert!(b.tasks[&t].live(r.clock(1, at)).is_some() && b.participants("b", r.clock(1, at)).contains("a"), "a's at t={at}\n{}", r.trace());
        }
        freed
    };
    // Half an hour of ticks per skew: the runs side by side.
    let freed: Vec<Option<i64>> = std::thread::scope(|s| {
        let runs: Vec<_> = [0].into_iter().chain(SKEWS).map(|skew| s.spawn(move || run(skew))).collect();
        runs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let base = freed[0];
    assert!(base.is_some_and(|t| t >= LEASE_MS / TICK), "{base:?}");
    for (skew, at) in SKEWS.iter().zip(&freed[1..]) {
        assert_eq!(*at, base, "a's clock {skew} ms off");
    }
}

/// Phase f F4: a node killed mid-claim hands its task to the other within the suspicion window, for
/// every skew. a's runner is killed as its claim reaches b (t=3, before its confirm window's read), or
/// holding it (t=4, t=6); b suspects a `SUSPECT_MS` after it last held a line of a, on its own clock
/// (`Board::participants`), and takes the task at once (`Local::gone`, `board::takes`): the same ticks
/// for each skew, the claim within an idle drone's look and a session's claim of the suspicion, or of b's
/// drone's done of its own task: b's claim of it, made as a died, waits until it is final (phase f F5),
/// which a dead a never acks, so until a is suspected. Before, 30 min.
#[test]
fn a_node_killed_mid_claim_hands_its_task_over_within_the_suspicion_window() {
    let window = SUSPECT_MS / TICK;
    for stop in [3, 4, 6] {
        let run = |skew: i64| {
            let r = Sim::run(Cfg { horizon: 60, ..Cfg::new(&[("a", None), ("b", None)]) }.node("a", |n| (n.stop, n.skew) = (Some(stop), skew)));
            let t = r.cfg.tasks[0].0.clone();
            let took = r.first("b", &format!("claim {t}@"));
            let free = r.first("b", "done ");
            let b = board::fold(&r.lines[1]);
            let away = (stop..60).find(|&t| !b.participants("b", t * TICK + 1).contains("a"));
            assert!(r.converged().spans.iter().any(|sp| sp.task == t && sp.node == "b"), "b holds {t}\n{}", r.trace());
            // b then waits on a for a lease before it gives up the verify task only a may take (phase f F6,
            // `h_a_killed_holder_ends_no_runner_early`): no end within this horizon.
            r.check(&["hang"]);
            (took, away, free, r.trace())
        };
        let (took, away, free, trace) = run(0);
        // Suspected a window after the last line b held of a; claimed within an idle drone's look and a
        // session's claim of that, or of b's drone's own done.
        let ready = away.zip(free).map(|(a, f)| a.max(f));
        let (away_ok, took_ok) = (away.is_some_and(|t| t <= stop + window + 1), took.zip(ready).is_some_and(|(t, a)| t <= a + IDLE_WAIT + 1));
        assert!(away_ok && took_ok, "killed at t={stop}: claimed at {took:?}, suspected at {away:?}, b free at {free:?}\n{trace}");
        for skew in SKEWS {
            let (t, a, f, trace) = run(skew);
            assert_eq!((t, a, f), (took, away, free), "killed at t={stop}, a's clock {skew} ms off\n{trace}");
        }
    }
}

/// Phase f F4: a node that is slow but beats keeps its claims. a's work takes 40 s, four suspicion
/// windows, and no hook renews its lease in that time; its runner beats every `BEAT_MS`, so b never
/// suspects it and never claims a's task, for every skew: one tenure, a's done, a finished board.
#[test]
fn a_slow_node_that_beats_keeps_its_claims() {
    for skew in [0].into_iter().chain(SKEWS) {
        let r = Sim::run(Cfg { work: 40, ..Cfg::new(&[("a", None), ("b", None)]) }.node("a", |n| n.skew = skew));
        let t = &r.cfg.tasks[0].0;
        let u = r.converged();
        let held: Vec<&str> = u.spans.iter().filter(|sp| sp.task == *t).map(|sp| sp.node.as_str()).collect();
        assert_eq!(held, ["a"], "a's clock {skew} ms off\n{}", r.trace());
        r.check(&[]);
        assert!(u.finished(), "{}", r.trace());
    }
}

/// Phase f F4: a false suspicion costs at most a duplicate attempt, never a lost task. The network is
/// cut from t=5 to t=36, longer than the suspicion window, while a works on its task (16 s; its claim
/// final at t=4, b's ack in before the cut): b suspects a, takes the task over (`board::takes` names
/// a's claim, the last renewal it held; final at once, its only peer suspected) and does it again; a,
/// cut off, finishes and its done takes effect in its own fold, and so does b's in b's. Once the cut
/// heals both stay (phase f F5): a's done, written under its claim, is the task's done (b's tenure ends
/// there), b's a duplicate attempt kept beside it. Two attempts, every runner's end excused or the
/// board finished, one accepted result, every property (S2': `a_false_suspicion_voids_no_done`).
#[test]
fn a_false_suspicion_costs_at_most_a_duplicate_attempt() {
    let r = Sim::run(Cfg { work: 16, cuts: vec![(5, 36)], ..Cfg::new(&[("a", None)]) });
    let t = &r.cfg.tasks[0].0;
    let u = r.converged();
    let attempts: Vec<&str> = u.spans.iter().filter(|sp| sp.task == *t).map(|sp| sp.node.as_str()).collect();
    assert_eq!(attempts, ["a", "b"], "b took a's task over in the cut\n{}", r.trace());
    assert!(r.first("a", &format!("done {t}@")).is_some() && r.first("b", &format!("done {t}@")).is_some(), "both did it\n{}", r.trace());
    assert!(u.finished(), "{}", r.trace());
    r.check(&[]);
}

/// What phase f F5 adds to `a_false_suspicion_costs_at_most_a_duplicate_attempt`: a's done, which took
/// effect in a's fold during the cut, is not voided by b's earlier takeover, nor b's by a's (S2').
#[test]
fn a_false_suspicion_voids_no_done() {
    let r = Sim::run(Cfg { work: 16, cuts: vec![(5, 36)], ..Cfg::new(&[("a", None)]) });
    assert_eq!(r.retracted(), None, "{}", r.trace());
}

/// Phase f F4, after review: a node never suspects itself. a runs alone with two drones; its runner's
/// beats stop at t=2 (a suspend, a failed beat write, a stalled beat thread) while d1 works 40 s on its
/// task, four suspicion windows, and no hook renews the lease meanwhile. Its idle d2 reads the board
/// on a's own clock and never takes d1's live claim (F2's exclusion): one tenure, d1's, and the board
/// finished. Fails with the sim's folds observed by no node (`board::fold` for `fold_on`): d2 takes the
/// task over a window after a's last line.
#[test]
fn a_node_whose_own_beats_stop_keeps_its_drones_claims() {
    let mut cfg = Cfg { work: 40, ..Cfg::new(&[("a", None)]) }.node("a", |n| (n.drones, n.mute) = (2, Some(2)));
    cfg.nodes.truncate(1);
    let r = Sim::run(cfg);
    let t = &r.cfg.tasks[0].0;
    let u = r.converged();
    let held: Vec<&str> = u.spans.iter().filter(|sp| sp.task == *t).map(|sp| sp.by.as_str()).collect();
    assert_eq!(held.len(), 1, "one tenure of {t}: {held:?}\n{}", r.trace());
    r.check(&[]);
    assert!(u.finished(), "{}", r.trace());
}

/// Phase f F4, after review: a cut on the observer's side (b's mesh daemon down or backing off after two
/// failed dials, about 2 min; b just resumed) longer than the suspicion window. b receives nothing from
/// t=5 to t=40 while its own lines reach a; a works 30 s on its task. b suspects a once `SUSPECT_MS`
/// passed since the last line it held (it cannot tell its own deafness from a's silence; F5's final
/// claims do not tell either: with a suspected, b's claim is final at once) and takes the task over,
/// naming a's claim; a learns of the takeover at once and drops its attempt. What holds: at most a
/// duplicate attempt, never a lost task, the board finished, every property. What it costs: the work
/// done twice (a done of a's that landed after b's takeover would be kept since phase f F5,
/// `a_false_suspicion_voids_no_done`).
#[test]
fn a_cut_on_the_observers_side_costs_at_most_a_duplicate_attempt() {
    let r = Sim::run(Cfg { work: 30, deaf: vec![("b", 5, 40)], horizon: 120, ..Cfg::new(&[("a", None)]) });
    let t = &r.cfg.tasks[0].0;
    let u = r.converged();
    let attempts: Vec<&str> = u.spans.iter().filter(|sp| sp.task == *t).map(|sp| sp.node.as_str()).collect();
    assert_eq!(attempts, ["a", "b"], "b, deaf, took a's task over\n{}", r.trace());
    assert!(u.finished(), "{}", r.trace());
    r.check(&[]);
}

/// Phase f F3, after review: a live holder keeps its lease beside a peer whose clock is 25 min or 2 h
/// ahead. a claims its task and renews it every `HEARTBEAT_MS` on its own clock; b, its clock that far
/// ahead, writes a runner line every `HEARTBEAT_MS`, each reaching the other node a tick later. a's
/// stamps jump to b's clock (board.rs `stamp`, up to `AHEAD_MS`), so the gap between two of a's
/// renewals is up to a heartbeat plus the jump: with the fold renewing only within `LEASE_MS` of the
/// last renewal's stamp, every heartbeat after the jump was ignored, and a's lease read expired on both
/// nodes half an hour in while a heartbeated on. The fold now renews on any heartbeat of the holder
/// that no rival's claim sorts before (`TaskState::claimable_at`): over 4 h, a holds its task on a's
/// and b's boards at every minute, b never sees it claimable, and the converged board has one tenure.
#[test]
fn a_live_holder_keeps_its_lease_beside_a_peer_clock_far_ahead() {
    use super::HEARTBEAT_MS;
    for skew in [25 * 60_000, 2 * 3_600_000] {
        let clock = |node: usize, t: i64| t + if node == 1 { skew } else { 0 };
        let open = Event { node: "a".into(), ..Event::open(work_task("t1", Role::Work, &[]), "engineer:e", -TICK) };
        let mut files: Vec<Vec<Event>> = (0..2).map(|i| vec![Event { seen: board::Seen(Some(clock(i, -TICK))), ..open.clone() }]).collect();
        // `events` written on `node` at true time `t`: in its own file then, on the other a tick later.
        let write = |files: &mut Vec<Vec<Event>>, node: usize, t: i64, events: Vec<Event>| {
            let name = ["a", "b"][node];
            let events = events.into_iter().map(|e| Event { node: name.into(), ..e }).collect();
            for e in stamped(&files[node], events) {
                files[node].push(Event { seen: board::Seen(Some(clock(node, t))), ..e.clone() });
                files[1 - node].push(Event { seen: board::Seen(Some(clock(1 - node, t + TICK))), ..e });
            }
        };
        let horizon = 4 * 3_600_000;
        let mut beats = [(0, TICK), (1, 400 * TICK)];
        let mut t_check = 0;
        while let Some(i) = (0..beats.len()).min_by_key(|&i| beats[i].1).filter(|&i| beats[i].1 <= horizon) {
            let (node, t) = beats[i];
            // Check every minute up to this write, on each node's own clock.
            while t_check < t {
                for n in 0..2 {
                    let b = board::fold(&files[n]);
                    let s = &b.tasks["t1"];
                    let now = clock(n, t_check);
                    if t_check > 2 * TICK {
                        assert!(
                            s.live(now).is_some_and(|(by, _, _)| by == "a.d1#1"),
                            "skew {skew}: a's lease on {} at {} min",
                            ["a", "b"][n],
                            t_check / 60_000
                        );
                        assert!(!s.claimable(now), "skew {skew}");
                    }
                }
                t_check += 60_000;
            }
            let lines = match (node, t) {
                (0, TICK) => vec![Event::new(Ev::Claim, "t1", "a.d1#1", clock(0, t)).with_drone("d1"), Event::new(Ev::Heartbeat, "t1", "a.d1#1", clock(0, t))],
                (0, _) => vec![Event::new(Ev::Heartbeat, "t1", "a.d1#1", clock(0, t))],
                _ => vec![Event::new(Ev::Heartbeat, "", "runner", clock(1, t)).with_note(&board::roster_note(&["d1".into()]))],
            };
            write(&mut files, node, t, lines);
            beats[i].1 += HEARTBEAT_MS;
        }
        let u = board::fold(&files[0]);
        assert_eq!(u, board::fold(&files[1]), "skew {skew}: one board");
        assert!(u.lost.is_empty() && u.spans.len() == 1 && u.spans[0].to_ms.is_none(), "skew {skew}: one tenure, still held: {:?}", u.spans);
    }
}

/// What a fold says took effect for good (phase f F5): every done and verdict (a tenure that ended so,
/// by task and session), every verify task, every work task verified, every integrated epoch (which
/// verdict integrated it is the fold's order's, `Sim::retracted`), every rework
/// round a rejection opened (`reopened`, which stands until the task is verified).
fn results(b: &Board) -> BTreeSet<String> {
    let ends = b.spans.iter().filter_map(|sp| match sp.end {
        Some(Ev::Done) => Some(format!("done {} {}", sp.task, sp.by)),
        Some(Ev::Verified | Ev::Rejected) => Some(format!("verdict {} {}", sp.task, sp.by)),
        _ => None,
    });
    let tasks = b.tasks.values().filter_map(|s| match (s.task.role, &s.status) {
        (Role::Verify, _) => Some(format!("verify task {}", s.task.id)),
        (Role::Work, Status::Verified { .. }) => Some(format!("verified {}", s.task.id)),
        _ => None,
    });
    let rounds = b.tasks.values().filter(|s| s.task.role == Role::Work).flat_map(|s| (1..=s.rework).map(move |r| format!("reopened {} {r}", s.task.id)));
    ends.chain(tasks).chain(b.integrations.iter().map(|i| format!("integrated {}", i.task))).chain(rounds).collect()
}

/// Whether `result` of an earlier fold is in `now` (`results`): a reopen also once its task is verified.
fn kept(result: &str, now: &BTreeSet<String>, b: &Board) -> bool {
    let verified = |t: &str| b.tasks.get(t).is_some_and(|s| matches!(s.status, Status::Verified { .. }));
    now.contains(result) || result.strip_prefix("reopened ").and_then(|r| r.split(' ').next()).is_some_and(verified)
}

/// Phase f F5, S2': a result once in a fold is in every later fold, under every arrival order of the
/// lines. The lines of runs where results race: two attempts of one task after a false suspicion (a cut,
/// a deaf observer), a claim stamped earlier that arrives late (CE5), an integrate claim held back
/// until after the other node's integration passed (G5), two nodes' integrators (CE4), and the false
/// suspicions and CE5 again with every first verdict a rejection (two attempts of one round, a rework
/// round, its attempts judged again; in CE5's, a's verifier rejects a's attempt in the cut, before b's
/// attempt, stamped earlier and the task's done once it arrives). Each one's lines arrive at a node in 24 orders, its fold read at
/// 40 points along the way: no result read at one point is gone at a later one. Fails with the fold's
/// holder rule alone (a done or verdict counted only from the task's holder in the fold's order,
/// `Fold::permanent` never true): a's done in the cut is voided once b's takeover arrives; and with a
/// rejection that reopens only while the attempt it judged is the task's done: the reopen is
/// retracted once the other attempt of the round, stamped earlier, arrives.
#[test]
fn s2_an_accepted_result_stays_in_every_later_fold_in_any_arrival_order() {
    let mut g5 = Cfg::new(&[("a", None), ("b", None)]).node("b", |n| n.skew = -2000);
    g5.holds.push(Hold { node: "b", ev: Ev::Claim, task: "integrate.1".into(), local: 0, remote: 6 });
    let at = Sim::run(g5.clone()).first("b", "claim integrate.1").unwrap();
    let reject = |c: Cfg| Cfg { reject: vec![format!("{}.v1", c.tasks[0].0)], horizon: 160, ..c }.node("a", |n| n.drones = 2).node("b", |n| n.drones = 2);
    let cfgs = [
        Cfg { work: 16, cuts: vec![(5, 36)], ..Cfg::new(&[("a", None)]) },
        Cfg { work: 30, deaf: vec![("b", 5, 40)], horizon: 120, ..Cfg::new(&[("a", None)]) },
        Cfg { delay: 5, work: 2, ..Cfg::new(&[("a", None)]) }.node("b", |n| n.skew = -2000),
        g5.node("b", |n| n.stop = Some(at + 1)),
        Cfg { delay: 2, integrate: 5, ..Cfg::new(&[("a", None), ("b", None)]) },
        reject(Cfg { work: 16, cuts: vec![(5, 36)], ..Cfg::new(&[("a", None)]) }),
        reject(Cfg { work: 30, deaf: vec![("b", 5, 40)], ..Cfg::new(&[("a", None)]) }),
        reject(ce5_suspected(30)),
    ];
    for cfg in cfgs {
        let r = Sim::run(cfg);
        r.check(&["integrate"]);
        let lines = r.union();
        for seed in 0..24 {
            let order = shuffled(&lines, seed);
            let mut before: BTreeSet<String> = BTreeSet::new();
            for k in 1..=40 {
                let b = board::fold(&order[..order.len() * k / 40]);
                let now = results(&b);
                let gone: Vec<&String> = before.iter().filter(|x| !kept(x, &now, &b)).collect();
                assert!(gone.is_empty(), "order {seed}, at {k}/40: {gone:?} retracted\n{}", r.trace());
                before = now;
            }
        }
    }
}

/// Phase f F5: integrate once under delays and skew. Two nodes, a task each, so both see the last verdict
/// within a few ticks and claim the integration; lines take 1 to 5 s, b's clock is up to 10 s either way,
/// an integration takes 2 or 5 s. One integrator merges and writes a verdict per integrate task, each
/// node's integrated ref moves at most once per integrate task under its fencing token, and every other
/// property holds. Before F5 (`k_ce4_two_nodes_integrate_once`): both nodes integrated in every run with
/// lines in 2 s or more.
#[test]
fn integrate_once_under_delays_and_skew() {
    for delay in [1, 2, 3, 5] {
        for skew in [0, 2000, -2000, 10_000, -10_000] {
            for integrate in [2, 5] {
                let r = Sim::run(Cfg { delay, integrate, ..Cfg::new(&[("a", None), ("b", None)]) }.node("b", |n| n.skew = skew));
                r.check(&[]);
                let moved = r.ref_moves.iter().find(|(_, by)| by.len() > 1);
                assert_eq!(moved, None, "delay {delay}, skew {skew}\n{}", r.trace());
                let u = r.converged();
                assert_eq!(u.integrations.len(), 1, "delay {delay}, skew {skew}: {:?}\n{}", u.integrations, r.trace());
            }
        }
    }
}

/// Phase f F5: no claim proceeds to its work before it is final unless a participant it waits for is
/// suspected. Without a fault (no node killed, cut, deaf or mute), over task sets with deps across
/// nodes, lines in 1 to 5 s, b's clock 2 or 10 s either way, b starting late, claims after 1 to 12 s:
/// no two sessions of any nodes worked on one task in one round, so no work is done twice, and every
/// property holds. With a cut longer than the suspicion window (`a_false_suspicion_voids_no_done`) a
/// claim proceeds on its suspicion and the work is done twice. Fails before F5 (the confirm window's
/// next-tick fold): CE5, b's claim stamped 2 s earlier landing at a after a's read-back, has both work.
/// The final waits, in ticks after a claim's line landed, are measured here too (`waits`).
#[test]
fn no_claim_works_before_it_is_final_without_a_suspicion() {
    let sets: [&[(&str, Option<usize>)]; 3] = [&[("a", None)], &[("a", None), ("b", None)], &[("a", None), ("b", Some(0))]];
    let mut waits: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for tasks in sets {
        for delay in [1, 2, 3, 5] {
            for (start, skew) in [(0, 0), (0, -2000), (2, 2000), (0, 10_000), (3, -10_000)] {
                let cfg = Cfg { delay, work: 2, ..Cfg::new(tasks) }.node("b", |n| (n.start, n.skew) = (start, skew));
                for cfg in cfg.sweep(&[1, 4, 12]) {
                    let r = Sim::run(cfg);
                    r.check(&[]);
                    let mut by_round: BTreeMap<(&str, u32), Vec<&str>> = BTreeMap::new();
                    for ((_, t, rework), by) in &r.won {
                        by_round.entry((t, *rework)).or_default().extend(by.iter().map(String::as_str));
                    }
                    let twice = by_round.iter().find(|(_, by)| by.len() > 1);
                    assert_eq!(twice, None, "delay {delay}, b start {start} skew {skew}\n{}", r.trace());
                    waits.entry(delay).or_default().extend(&r.waits);
                }
            }
        }
    }
    // A healthy peer acks a claim in the tick it holds it: final two line delays after the claim landed,
    // at most a beat and a delay more when the ack waits on a line held back (none here).
    for (delay, w) in &waits {
        let (max, mean) = (w.iter().max().copied().unwrap_or(0), w.iter().sum::<i64>() as f64 / w.len().max(1) as f64);
        eprintln!("final waits, lines in {delay} s: {} claims, mean {mean:.2} ticks, max {max}", w.len());
        assert!(max <= 2 * delay + BEAT_MS / TICK, "lines in {delay} s: a final wait of {max} ticks");
    }
}

/// Phase f F6, the case F5 found: one drone a node, each node's verifier the other's, a cut, a rejection.
/// a's and b's work are done and each verified by the other node, a's first attempt rejected; the cut
/// from t=8 for 30 s falls on the rework, so each node suspects the other and has nothing it may do. Before
/// F6 a drone with nothing open and nothing in hand (a suspected peer's claim is in no hand) counted idle
/// polls and ended "nothing to do" within the cut, leaving the rework no verifier once it healed. Now it
/// waits on the board however long the goal is unfinished, a cut being no stuck within the lease
/// (`Board::takers`): the goal finishes, every runner on a finish for good, exit 0.
#[test]
fn one_drone_a_node_a_cut_and_a_rejection_finish() {
    let cfg = Cfg::new(&[("a", None), ("b", None)]);
    let reject = vec![format!("{}.v1", cfg.tasks[0].0)];
    let r = Sim::run(Cfg { reject, cuts: vec![(8, 38)], ..cfg });
    r.check(&[]);
    assert!(r.converged().finished(), "{}", r.trace());
    assert!(r.ends.iter().all(|(_, why, _)| *why == "goal finished"), "{:?}\n{}", r.ends, r.trace());
    assert!(r.exits.values().all(|(ok, _)| *ok), "{}", r.trace());
}

/// Phase f F6: a one-drone node whose work needs the other node's verifier finishes. a's one drone does
/// a's one task; b's has nothing to do but verify it. The network is cut from t=2 to t=30, so a's claim
/// waits on b's watermark until it suspects b, then proceeds and works in the cut, and b, suspecting a,
/// sees nothing open and no claim of a live peer. Before F6 b ended "nothing to do" after three idle
/// polls and a's work was never verified; now b waits, verifies it once the cut heals, and the goal
/// finishes on both nodes.
#[test]
fn a_one_drone_node_whose_work_needs_the_other_nodes_verifier_finishes() {
    let r = Sim::run(Cfg { cuts: vec![(2, 30)], ..Cfg::new(&[("a", None)]) });
    r.check(&[]);
    let t = &r.cfg.tasks[0].0;
    assert!(r.first("b", &format!("verified {t}.v1")).is_some_and(|at| at >= 30), "b verifies once the cut healed\n{}", r.trace());
    assert!(r.converged().finished() && r.exits.values().all(|(ok, _)| *ok), "{}", r.trace());
}

/// Phase f F6: a peer that leaves while it is the only possible verifier yields stuck on the remaining
/// node, not a hang and not exit 0. b's one drone has the budget of one iteration: it does b's task and
/// ends, so b's runner leaves the goal; a's one drone verifies b's work and does a's, whose verify task
/// it may not take. No running drone of the goal may: a says so on the board (a stuck line, its
/// constraint held: premature_end checks it) and ends "stuck", its runner exiting non-zero.
#[test]
fn a_peer_that_leaves_as_the_only_verifier_yields_stuck() {
    let r = Sim::run(Cfg::new(&[("a", None), ("b", None)]).node("b", |n| n.share = 1));
    r.check(&[]);
    let mine = format!("{}.v1", r.cfg.tasks[0].0);
    assert_eq!(r.stuck.iter().map(|x| (r.name(x.node), x.task.as_str())).collect::<Vec<_>>(), vec![("a", mine.as_str())], "{}", r.trace());
    assert_eq!(r.ends.iter().find(|(d, _, _)| d == "a.d1").map(|(_, why, _)| *why), Some("stuck"), "{}", r.trace());
    assert_eq!(r.exits.get(&0).map(|(ok, _)| *ok), Some(false), "{}", r.trace());
    assert!(r.converged().stuck.contains_key(&mine), "the stuck line stands: {:?}", r.converged().stuck);
}

/// Phase f F6: a participant that joins late clears a stuck. a runs alone with one drone: it does its
/// task, whose verify task it may not take, says it stuck once that held `SUSPECT_MS` and ends. b's runner
/// starts at t=40: its join clears a's stuck line on every board (`Board::stuck`), its drone verifies a's
/// work and integrates, and b ends on a finish for good, exit 0.
#[test]
fn a_participant_that_joins_late_clears_a_stuck() {
    let r = Sim::run(Cfg::new(&[("a", None)]).node("b", |n| n.start = 40));
    r.check(&[]);
    let u = r.converged();
    assert_eq!(r.ends.iter().find(|(d, _, _)| d == "a.d1").map(|(_, why, _)| *why), Some("stuck"), "{}", r.trace());
    assert!(r.stuck.iter().all(|x| x.at < 40), "{}", r.trace());
    assert!(u.stuck.is_empty() && u.finished(), "b's join cleared it, b finished the goal: {:?}\n{}", u.stuck, r.trace());
    assert_eq!(r.exits.get(&1).map(|(ok, _)| *ok), Some(true), "{}", r.trace());
}

/// Phase f F6: a line on its way that opens a task ends no runner on a finish that is not for good (F5's
/// "left": the stable-prefix exit). Lines take 2 s; an engineer opens a task on b at t=28, a tick after
/// a's integration passed on a's board and before it reached b: b's board is not finished then, so b
/// writes no finished line for it, and a, its own board finished, waits for one (`Board::settling`)
/// until the open arrives; both nodes then do the new task, integrate again and end on that. Fails with
/// `Board::settling` never waiting: a ends "goal finished" and exits 0 before the open reaches it, and the
/// converged board is unfinished at every runner's end. (An open written after every participant's
/// finished line is outside what any detection sees: the board was finished for good then.)
#[test]
fn a_line_on_its_way_that_opens_a_task_ends_no_runner_on_a_finish_not_for_good() {
    let cfg = Cfg { delay: 2, opens: vec![(28, "b", "late".into())], ..Cfg::new(&[("a", None), ("b", None)]) };
    let r = Sim::run(cfg);
    r.check(&[]);
    let u = r.converged();
    assert!(u.finished() && u.tasks["late"].status.name() == "verified" && u.integrations.len() == 2, "{}", r.trace());
    assert!(r.first("a", "integrate-d1@a-21000 verified integrate.1").is_some_and(|t| t < 28), "a's board finished before the open\n{}", r.trace());
    assert!(r.exits.values().all(|(ok, _)| *ok), "{}", r.trace());
}

/// Phase f F6: a peer starting seconds late is waited for, not given up. a runs alone with one drone and
/// does its task by t=5, whose verify task it may not take; b's runner starts at t=10, its join reaching a
/// within `SUSPECT_MS` of the verify task becoming claimable there, so a says no stuck (`board::stuck`'s
/// grace) and b verifies. Fails with that grace at 0: a ends stuck before b's join arrives.
#[test]
fn a_peer_starting_seconds_late_is_waited_for_not_given_up() {
    let r = Sim::run(Cfg::new(&[("a", None)]).node("b", |n| n.start = 10));
    r.check(&[]);
    assert!(r.stuck.is_empty() && r.converged().finished(), "{:?}\n{}", r.stuck, r.trace());
    assert!(r.ends.iter().all(|(_, why, _)| *why == "goal finished"), "{:?}\n{}", r.ends, r.trace());
}

/// Phase f F6, after review: a stuck task gives up only itself and what waits on it, never the rest of the
/// board. Beside a's one task, an engineer's task needs a task that is on no board (stuck at once). b's
/// drone has nothing open while a holds its task, and waits on it (`board::moving`), then verifies it;
/// both end stuck on the orphan alone. Fails with a drone ending on any stuck while work still moves: b
/// ends at t=1 and a's work is never verified (`premature_end` names the verify task left to b).
#[test]
fn a_stuck_task_ends_no_drone_while_other_work_still_moves() {
    let mut cfg = Cfg::new(&[("a", None)]);
    cfg.tasks.push(("orphan".into(), vec!["gone".into()]));
    let r = Sim::run(cfg);
    r.check(&[]);
    let u = r.converged();
    let t = &r.cfg.tasks[0].0;
    assert!(matches!(u.tasks[&format!("{t}.v1")].status, Status::Verified { .. }), "{}", r.trace());
    assert_eq!(r.stuck.iter().map(|x| x.task.as_str()).collect::<BTreeSet<_>>(), BTreeSet::from(["orphan"]), "{}", r.trace());
    assert!(r.ends.iter().all(|(_, why, _)| *why == "stuck") && r.exits.values().all(|(ok, _)| !*ok), "{:?}\n{}", r.ends, r.trace());
}

/// Phase f F6, after review: a task no node may take ends stuck, not a hang. Its checks are approved on no
/// node (`Cfg::refuse`), so each node's drone may not take it; each says so on the board (`declines`), and
/// once both lines are in, every node reads the task stuck and the run ends, exit 1. Fails with another
/// node counted as able to take whatever it declined: each node leaves the task to the other, and both
/// drones wait at the horizon.
#[test]
fn a_task_no_node_may_take_ends_stuck_not_a_hang() {
    let cfg = Cfg::new(&[("a", None), ("b", None)]);
    let refuse = vec![cfg.tasks[0].0.clone()];
    let r = Sim::run(Cfg { refuse: refuse.clone(), ..cfg });
    r.check(&[]);
    let u = r.converged();
    assert_eq!(u.declined.keys().map(|(n, t)| (n.as_str(), t.as_str())).collect::<Vec<_>>(), vec![("a", refuse[0].as_str()), ("b", refuse[0].as_str())]);
    assert!(r.stuck.iter().all(|x| x.task == refuse[0]) && !r.stuck.is_empty(), "{:?}\n{}", r.stuck, r.trace());
    let other = format!("{}.v1", r.cfg.tasks[1].0);
    assert!(matches!(u.tasks[&other].status, Status::Verified { .. }), "the other task is done and verified\n{}", r.trace());
    assert!(r.ends.iter().all(|(_, why, _)| *why == "stuck") && r.exits.values().all(|(ok, _)| !*ok), "{:?}\n{}", r.ends, r.trace());
}
