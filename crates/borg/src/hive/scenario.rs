//! Scenario tests of the hive's coordination on the real board code (phase f F0, tasks/hive-soundness.md):
//! hivemc.py's scenarios (tasks/soundness-model/run.py A..L) and the review's counterexamples (CE1..CE5,
//! G), run on `board::fold` and the predicates the runner and the claim read instead of the model's
//! re-implementation of them.
//!
//! A small deterministic simulation: each node holds the lines it received and folds them at its own
//! clock (`skew`, plus its index in ms, so two nodes never stamp one ms unless a scenario says so). A
//! line reaches the other nodes `delay` ticks after its write; a `Hold` keeps a chosen line back
//! longer, from the others or from its own node's file too. Each drone runs run.rs's loop
//! (`integrate::due`, `board::free_for` / `in_hand`, `run::step`, `run::stalls`, the session-start and
//! session-end lines, a roster line at each drone's end) and each session claims as mod.rs `claim_cmd`
//! and `claim` do (`board::next_for`, the pre-write rival and `left_to_another` check, the read-back),
//! writing real `Event` lines. As in hivemc.py, one tick is one second; within a tick every drone reads
//! its node's board as it stood at the tick's start and its lines land at the tick's end; a claim's
//! read-back and its confirm window are read at the next tick, the work's hooks at every tick (a
//! loser stops at its first tick after it learns), and a session takes one task.
//!
//! Where the harness is not the run: no git, oracle, approvals or harness process (an iteration costs
//! 1 of a node's `share`; every check passes); `open_to`'s `allowed` is `may_claim` (no integrate task),
//! a session's adds `absent_from` (a verify task of another node's work only with `bundles`); the
//! pre-write rival check is mod.rs `claim`'s closure, copied (it is no function of its own); the
//! confirm window is the next tick's fold, never its cap; an integrate claim's read-back is the next
//! tick's fold too, where integrate.rs `claim_due` reads back after `CLAIM_SETTLE` (150 ms) with no
//! confirm window, so a peer's integrate claim that takes up to a tick to arrive is always seen here
//! (integrate-once holds at a delay of 1 on this abstraction, not on the code).
//!
//! Each test names the hivemc scenario it mirrors and the property it checks: S1 (one board for any
//! order of the lines), S2' (a done, a verdict or an integration that took effect in a node's fold is in
//! the converged board), integrate-once (one integration in full per integrate task), same-node
//! exclusion (one read-back win per node per task attempt), no premature end (every runner ended:
//! finished, or a budget spent, a node killed or a task blocked) and stable-prefix exit (exit 0 only
//! on a finished converged board). A property that does not hold yet is `#[ignore]`d with the step of
//! tasks/todo.md phase f that fixes it, asserting it as the property reads, so `--ignored` shows the
//! violation.

use std::collections::{BTreeMap, BTreeSet};

use super::board::{self, Board, Ev, Event, Role, Status, Task, TaskState};
use super::run::{self, Next, Offer, Tally};
use super::{CLAIM_TRIES, START_CAP, integrate};

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
    /// A verify task's or an integration's commits reach the other nodes (commits.rs bundles).
    bundles: bool,
    /// Sessions claim the task their prompt offered by name (mod.rs `offer`, `picked` false: G1).
    named: bool,
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
        let node = |name| NodeCfg { name, drones: 1, start: 0, skew: 0, share: 9, stop: None };
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
            bundles: true,
            named: false,
            horizon: 90,
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
/// session), an integration (task, ms).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Seen {
    Done(String, String),
    Verdict(String, String),
    Integrated(String, i64),
}

#[derive(Debug, Clone)]
struct Session {
    id: String,
    /// Its start on its node's clock, and what the board offered it then.
    started: i64,
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
    Wait { until: i64, seen: usize, in_hand: bool },
    Session(Box<Session>),
    Integrating { session: String, task: String, until: Option<i64> },
    Post { start: Box<Offer>, wrote: bool },
    Ended(&'static str),
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
    drones: Vec<Drone>,
    held: BTreeSet<usize>,
    stopped: BTreeSet<usize>,
    exits: BTreeMap<usize, (bool, i64)>,
    ends: Vec<(String, &'static str, i64)>,
    seen: BTreeSet<Seen>,
    /// Integrators that merged and wrote their verdict: integrate task -> sessions.
    integrations: BTreeMap<String, Vec<String>>,
    /// Sessions whose read-back won: (node, task, rework) -> sessions.
    won: BTreeMap<(String, String, u32), Vec<String>>,
    log: Vec<String>,
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

/// run.rs `open_to` with every check approved.
fn open_to(b: &Board, drone: &str, node: &str, now: i64) -> Offer {
    Offer::of(b, drone, node, "", now, |s| s.task.role != Role::Integrate)
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
        let mut s = Sim {
            t: 0,
            lines: vec![opens; n],
            pending: Vec::new(),
            out: vec![Vec::new(); n],
            drones,
            held: BTreeSet::new(),
            stopped: BTreeSet::new(),
            exits: BTreeMap::new(),
            ends: Vec::new(),
            seen: BTreeSet::new(),
            integrations: BTreeMap::new(),
            won: BTreeMap::new(),
            log: Vec::new(),
            cfg,
        };
        while s.t < s.cfg.horizon && !(s.pending.is_empty() && s.drones.iter().all(|d| matches!(d.st, St::Ended(_)))) {
            s.tick();
        }
        s
    }

    fn name(&self, node: usize) -> &'static str {
        self.cfg.nodes[node].name
    }

    fn now(&self, node: usize) -> i64 {
        self.t * TICK + self.cfg.nodes[node].skew + node as i64
    }

    fn note(&mut self, node: usize, what: String) {
        self.log.push(format!("t={} {}: {what}", self.t, self.name(node)));
    }

    /// Writes `events` on `node` (hive/mod.rs `write` stamps the node): they land in its file at the
    /// tick's end, and on the other nodes `delay` ticks after that (a `Hold` later). The tick they are
    /// in its own file.
    fn write(&mut self, node: usize, events: Vec<Event>) -> i64 {
        let mut lands = self.t + 1;
        if self.stopped.contains(&node) {
            return lands;
        }
        for e in events {
            let e = Event { node: self.name(node).into(), ..e };
            let hold = self.cfg.holds.iter().position(|h| h.node == e.node && h.ev == e.ev && h.task == e.task).filter(|i| !self.held.contains(i));
            let (local, remote) = hold.map_or((0, 0), |i| (self.cfg.holds[i].local, self.cfg.holds[i].remote));
            if let Some(i) = hold {
                self.held.insert(i);
            }
            lands = lands.max(self.t + local);
            let what = format!("{} {} {}@{}{}", e.by, e.ev.name(), e.task, e.ms, if e.note.is_empty() { String::new() } else { format!(" ({})", e.note) });
            self.note(node, what);
            for to in 0..self.lines.len() {
                match (to == node, local) {
                    (true, 0) => self.out[node].push(e.clone()),
                    (true, _) => self.pending.push((self.t + local, to, e.clone())),
                    (false, _) => self.pending.push((self.t + (local - 1).max(0) + self.cfg.delay + remote, to, e.clone())),
                }
            }
        }
        lands
    }

    fn tick(&mut self) {
        let t = self.t;
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending).into_iter().partition(|(at, _, _)| *at <= t);
        self.pending = later;
        for (_, to, e) in due {
            self.lines[to].push(e);
        }
        for i in 0..self.cfg.nodes.len() {
            if self.cfg.nodes[i].start == t {
                let names: Vec<String> = (1..=self.cfg.nodes[i].drones).map(|k| format!("d{k}")).collect();
                let now = self.now(i);
                self.write(i, vec![Event::new(Ev::Heartbeat, "", "runner", now).with_note(&board::roster_note(&names))]);
            }
            if self.cfg.nodes[i].stop == Some(t) {
                self.note(i, "runner killed".into());
                for d in self.drones.iter_mut().filter(|d| d.node == i) {
                    d.st = St::Ended("killed");
                }
                self.stopped.insert(i);
            }
        }
        let folds: Vec<Board> = self.lines.iter().map(|l| board::fold(l)).collect();
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
                let ms = self.now(node) + self.out[node].len() as i64;
                self.write(node, vec![Event::new(Ev::Heartbeat, "", "runner", ms).with_note(&board::roster_note(&still))]);
            }
        }
        for i in 0..self.lines.len() {
            let out = std::mem::take(&mut self.out[i]);
            self.lines[i].extend(out);
            let all_ended = self.drones.iter().filter(|d| d.node == i).all(|d| matches!(d.st, St::Ended(_)));
            if all_ended && !self.stopped.contains(&i) && !self.exits.contains_key(&i) {
                let finished = board::fold(&self.lines[i]).finished();
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
    }

    fn act(&mut self, d: usize, b: &Board) {
        let node = self.drones[d].node;
        let ready = self.cfg.nodes[node].start + if self.cfg.nodes.len() > 1 { self.cfg.delay.min(START_CAP.as_secs() as i64) } else { 0 };
        loop {
            match self.drones[d].st.clone() {
                St::Ended(_) => return,
                St::Off if self.t >= ready => self.drones[d].st = St::Decide,
                St::Off => return,
                St::Wait { until, seen, in_hand } => {
                    // run.rs `idle_wait`: a change of the board's file ends the wait early, no idle poll.
                    let changed = self.lines[node].len() != seen;
                    if !changed && self.t < until {
                        return;
                    }
                    if !changed && !in_hand {
                        self.drones[d].tally.idle += 1;
                    }
                    self.drones[d].st = St::Decide;
                }
                St::Post { start, wrote } => {
                    let end = open_to(b, &self.drones[d].name, self.name(node), self.now(node));
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
        if let Some((id, new)) = integrate::due(b, now) {
            // integrate.rs `absent`: an accepted attempt done on another node, its bundle not here.
            let absent = !self.cfg.bundles
                && b.tasks.values().any(|s| {
                    matches!(s.status, Status::Verified { .. })
                        && s.task.role == Role::Work
                        && b.spans.iter().any(|sp| sp.task == s.task.id && Some(&sp.by) == s.author.as_ref() && sp.node != here)
                });
            if absent {
                self.drones[d].st = St::Ended("integration needs another node's commits");
                return;
            }
            // integrate.rs `run` and `claim_due`: the open when new, a claim and a heartbeat in one write.
            let session = format!("integrate-{drone}-{now}");
            let mut lines: Vec<Event> = new.then(|| Event::open(work_task(&id, Role::Integrate, &[]), &session, now)).into_iter().collect();
            lines.extend([Event::new(Ev::Claim, &id, &session, now).with_drone(&drone), Event::new(Ev::Heartbeat, &id, &session, now)]);
            self.write(node, lines);
            self.drones[d].st = St::Integrating { session, task: id, until: None };
            return;
        }
        let offer = open_to(b, &drone, here, now);
        let x = &mut self.drones[d];
        match run::step(&x.tally, f64::from(self.cfg.nodes[node].share), 1.0, b.finished(), offer.task.is_some()) {
            Next::End(why) => x.st = St::Ended(why),
            Next::Wait => x.st = St::Wait { until: self.t + IDLE_WAIT, seen: self.lines[node].len(), in_hand: offer.in_hand },
            Next::Go => {
                x.tally.idle = 0;
                x.iters += 1;
                let iters = x.iters as usize;
                let id = format!("{here}.{drone}#{iters}");
                let lat = self.cfg.lat.iter().find(|((n, dn), _)| *n == here && *dn == drone).map_or(1, |(_, l)| l[(iters - 1).min(l.len() - 1)]);
                let since = offer.task.as_ref().map_or(now, |(_, since)| *since);
                let notice = self.t + ((since + IDLE_WAIT * TICK - now).max(0) + TICK - 1) / TICK;
                let offered = if self.cfg.named { self.next(b, &id, &drone, node, now) } else { None };
                let s = Session {
                    id,
                    started: now,
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

    /// The task mod.rs `next_open` offers `session`: `next_for` among those it may take here.
    fn next(&self, b: &Board, session: &str, drone: &str, node: usize, now: i64) -> Option<String> {
        let here = self.name(node);
        let present = |s: &TaskState| self.cfg.bundles || s.task.role != Role::Verify || s.node == here;
        board::next_for(b, session, drone, here, "", now, |s| s.task.role != Role::Integrate && present(s)).map(|s| s.task.id.clone())
    }

    /// mod.rs `claim_cmd` up to `claim`'s write: the task claimed (its claim written), None when no
    /// claim was written.
    fn claim(&mut self, d: usize, b: &Board, s: &mut Session) -> Option<String> {
        let (node, now, drone) = (self.drones[d].node, self.now(self.drones[d].node), self.drones[d].name.clone());
        // mod.rs `claim`'s pre-write check: a rival holds it, or (a picked task) another node's idle
        // drones now take it.
        let session = s.id.clone();
        let rival = |t: &TaskState| !t.claimable(now) && t.live(now).is_none_or(|(by, _, _)| by != session);
        let tries = if self.cfg.named { 1 } else { CLAIM_TRIES };
        while s.lost < tries {
            let id = match self.cfg.named {
                true => s.offered.clone().filter(|id| b.tasks.get(id).is_some_and(|t| board::claimable_by(b, t, &s.id, &drone, self.name(node), now)))?,
                false => self.next(b, &s.id, &drone, node, now)?,
            };
            let t = &b.tasks[&id];
            if rival(t) || (!self.cfg.named && board::left_to_another(b, t, self.name(node), now)) {
                s.lost += 1;
                continue;
            }
            s.lands = self.write(node, vec![Event::new(Ev::Claim, &id, &s.id, now).with_drone(&drone), Event::new(Ev::Heartbeat, &id, &s.id, now)]);
            s.wrote = true;
            return Some(id);
        }
        None
    }

    fn session(&mut self, d: usize, b: &Board, mut s: Session) {
        let (node, now, drone) = (self.drones[d].node, self.now(self.drones[d].node), self.drones[d].name.clone());
        let here = self.name(node);
        let started = s.started;
        let key = (here.to_string(), drone.clone());
        let unsettled = |b: &Board| !b.sessions.get(&key).is_some_and(|(_, settled)| *settled > started);
        if !s.noticed && self.t >= s.notice {
            s.noticed = true;
            if unsettled(b) {
                self.write(node, vec![Event::new(Ev::Heartbeat, "", "runner", s.started).with_note(&board::session_note(&drone, true))]);
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
            (Some(id), false) => match super::holding(b, &id, &s.id, now) {
                Some(t) => {
                    let key = (here.to_string(), id.clone(), t.rework);
                    self.won.entry(key).or_default().push(s.id.clone());
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
            (Some(id), true) => match super::holding(b, &id, &s.id, now) {
                None => {
                    self.note(node, format!("{} is told it lost {id}", s.id));
                    true
                }
                Some(_) if self.t < s.until => false,
                Some(t) => {
                    let lines = match t.task.role {
                        Role::Verify => vec![Event::new(Ev::Verified, &id, &s.id, now)],
                        _ => {
                            // mod.rs `finish`: the done and its verify task's open (`verify_task`) at one ms.
                            let v = Task {
                                id: format!("{id}.v{}", t.rework + 1),
                                of: Some(id.clone()),
                                not_by: vec![s.id.clone(), drone.clone()],
                                ..work_task("", Role::Verify, &t.task.deps)
                            };
                            vec![Event { head: format!("head-{}", s.id), ..Event::new(Ev::Done, &id, &s.id, now) }, Event::open(v, &s.id, now)]
                        }
                    };
                    self.write(node, lines);
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

    /// integrate.rs after `claim_due`'s write: the read-back, then the merge and the oracle, and the
    /// verdict, with no fold between. Whether the drone is done for this tick.
    fn integrating(&mut self, d: usize, b: &Board, session: String, task: String, until: Option<i64>) -> bool {
        let (node, now) = (self.drones[d].node, self.now(self.drones[d].node));
        let x = &mut self.drones[d];
        match until {
            None if b.tasks.get(&task).and_then(|s| s.live(now)).is_none_or(|(by, _, _)| by != session) => {
                x.st = St::Decide;
                false
            }
            None => {
                x.st = St::Integrating { session, task, until: Some(self.t + self.cfg.integrate) };
                true
            }
            Some(at) if self.t < at => true,
            Some(_) => {
                self.integrations.entry(task.clone()).or_default().push(session.clone());
                self.write(node, vec![Event { head: format!("merged-{session}"), ..Event::new(Ev::Verified, &task, &session, now) }]);
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

    /// No premature end: once every runner ended, the converged board is finished, or every drone's
    /// budget ran out (one that ended with budget left could have gone on), a node was killed, or a task
    /// is blocked. F6/F7's explicit stuck(task, constraint) joins these once it exists.
    fn premature_end(&self) -> Option<String> {
        let u = self.converged();
        let excused = self.ends.iter().all(|(_, why, _)| *why == "budget")
            || self.ends.iter().any(|(_, why, _)| *why == "killed")
            || u.tasks.values().any(|s| matches!(s.status, Status::Blocked { .. }));
        let all_ended = self.drones.iter().all(|d| matches!(d.st, St::Ended(_)));
        (all_ended && !u.finished() && !excused).then(|| {
            let open: Vec<String> =
                u.tasks.values().filter(|s| !matches!(s.status, Status::Verified { .. })).map(|s| format!("{} {}", s.task.id, s.status.name())).collect();
            format!("unfinished board at every runner's end ({}): {:?}", open.join(", "), self.ends)
        })
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
            Seen::Integrated(t, ms) => u.integrations.iter().any(|i| i.task == *t && i.ms == *ms),
        };
        let gone: Vec<&Seen> = self.seen.iter().filter(|x| !kept(x)).collect();
        (!gone.is_empty()).then(|| format!("took effect in a node's fold, void in the converged board: {gone:?}"))
    }

    /// Integrate-once: one integrator merged and wrote its verdict per integrate task.
    fn integrated_twice(&self) -> Option<String> {
        self.integrations.iter().find(|(_, by)| by.len() > 1).map(|(t, by)| format!("{t} integrated in full by {by:?}"))
    }

    /// Same-node exclusion: one session of a node won its read-back per task attempt.
    fn same_node_twice(&self) -> Option<String> {
        self.won.iter().find(|(_, by)| by.len() > 1).map(|((n, t, r), by)| format!("{t} (rework {r}) held after the read-back on {n} by {by:?}"))
    }

    /// Every property but the ones `skip` names, with the trace.
    fn check(&self, skip: &[&str]) {
        let props: [(&str, Option<String>); 5] = [
            ("end", self.premature_end()),
            ("exit", self.exit_unfinished()),
            ("S2'", self.retracted()),
            ("integrate", self.integrated_twice()),
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
/// drone waits while a's session started and has not claimed (`in_hand`) instead of ending after three
/// idle polls, and verifies a's work. No premature end.
#[test]
fn a_ce1_a_peer_slow_to_claim_keeps_the_other_node_waiting() {
    let r = Sim::run(Cfg::new(&[("a", None)]).lat("a", "d1", &[12]));
    r.check(&[]);
    assert!(r.converged().finished(), "{}", r.trace());
}

/// A dependency chain (hivemc B; CE2): a's two tasks, the second after the first; b's model takes 12 s
/// to claim the first one's verify task. a's drone, which may claim nothing until b's verdict, waits on
/// b's unsettled session (the peer session in `in_hand`) and then does the second. No premature end.
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

/// A slow network (hivemc D): lines take 3 s. Every property but integrate-once (`d_integrates_once`).
#[test]
fn d_a_slow_network() {
    for cfg in Cfg::new(&[("a", None), ("b", None)]).sweep(&[1, 6]) {
        Sim::run(Cfg { delay: 3, ..cfg }).check(&["integrate"]);
    }
}

/// hivemc D's integration: with lines taking 3 s, both nodes see the last verdict and integrate (CE4).
#[test]
#[ignore = "F5: integrate claims wait for every live participant, the integration is fenced by its epoch"]
fn d_integrates_once() {
    for cfg in Cfg::new(&[("a", None), ("b", None)]).sweep(&[1, 6]) {
        let r = Sim::run(Cfg { delay: 3, ..cfg });
        assert_eq!(r.integrated_twice(), None, "{}", r.trace());
    }
}

/// Clock skew (hivemc E): b's clock 2 s behind, lines take 2 s. Every property but integrate-once.
#[test]
fn e_clock_skew() {
    for cfg in Cfg::new(&[("a", None), ("b", None)]).node("b", |n| n.skew = -2000).sweep(&[1, 6]) {
        Sim::run(Cfg { delay: 2, ..cfg }).check(&["integrate"]);
    }
}

/// b's runner starts 6 s after a's (hivemc F): every property.
#[test]
fn f_a_late_start() {
    for cfg in Cfg::new(&[("a", None), ("b", None)]).node("b", |n| n.start = 6).sweep(&[1, 6]) {
        Sim::run(cfg).check(&[]);
    }
}

/// No bundles cross (hivemc G): a bundle over 1 MiB, or a fetch that fails. In the run a verify task of
/// another node's work is not offered there (mod.rs `absent_from`), where hivemc let it be verified and
/// stopped at the integration (`elsewhere`): each node's one drone may verify only the other's work, so
/// both runners end with the verify tasks open, "stalled" (`open_to` offers them; the session finds
/// nothing it may claim). No premature end: an end there must be an explicit stuck.
#[test]
#[ignore = "F7: commits a node cannot fetch make the task stuck (premature_end's excuse then), not a runner's end"]
fn g_unfetchable_commits_end_no_runner() {
    let r = Sim::run(Cfg { bundles: false, ..Cfg::new(&[("a", None), ("b", None)]) });
    assert_eq!(r.premature_end(), None, "{}", r.trace());
}

/// hivemc G with a's clock 20 s and 60 s ahead: b's sessions refuse the verify task they are offered,
/// stamped by a's done, so its claimable time is ahead of each session's start. The stall is judged
/// by the session's own start view (run.rs `stalls`, no clock against another), so b's drone ends
/// "stalled" after as many iterations as with one clock, not on its budget.
#[test]
fn g_a_peer_clock_ahead_hides_no_stall() {
    let ends = |skew: i64| {
        let r = Sim::run(Cfg { bundles: false, ..Cfg::new(&[("a", None), ("b", None)]) }.node("a", |n| n.skew = skew));
        let b = r.drones.iter().find(|d| r.name(d.node) == "b").unwrap();
        (b.iters, r.ends.iter().find(|(d, _, _)| d == "b.d1").map(|(_, why, _)| *why), r.trace())
    };
    let (iters, why, trace) = ends(0);
    assert_eq!(why, Some("stalled"), "{trace}");
    for skew in [20_000, 60_000] {
        let (n, why, trace) = ends(skew);
        assert_eq!((n, why), (iters, Some("stalled")), "a's clock {skew} ms ahead\n{trace}");
    }
}

/// a's runner is killed at t=4 holding its task (hivemc H): b waits on the lease (`in_flight`) and never
/// takes the task back; the lease is 30 min, and only a could verify b's work. The property: a node
/// gone silent hands its tasks back within seconds.
#[test]
#[ignore = "F4: a goal participant silent for seconds is suspected and its leases count as expired"]
fn h_a_killed_holder_hands_its_task_back() {
    let cfg = Cfg { horizon: 120, ..Cfg::new(&[("a", None), ("b", None)]) }.node("a", |n| n.stop = Some(4));
    let r = Sim::run(cfg);
    let t = &r.cfg.tasks[0].0;
    let taken = r.converged().spans.iter().any(|sp| sp.task == *t && sp.node == "b");
    assert!(taken, "{t} still a's 2 minutes after a's runner died\n{}", r.trace());
}

/// a's lease wait (hivemc H) ends no runner early: b keeps waiting on a killed node's claim.
#[test]
fn h_a_killed_holder_ends_no_runner_early() {
    let r = Sim::run(Cfg::new(&[("a", None), ("b", None)]).node("a", |n| n.stop = Some(4)));
    assert!(matches!(r.drones[1].st, St::Wait { .. }), "{}", r.trace());
    r.check(&[]);
}

/// a has the budget for one iteration and its model is slow to claim (hivemc I): b verifies a's work.
#[test]
fn i_a_budget_of_one_iteration() {
    let r = Sim::run(Cfg::new(&[("a", None)]).lat("a", "d1", &[12]).node("a", |n| n.share = 1));
    r.check(&[]);
    assert!(r.converged().finished(), "{}", r.trace());
}

/// Named claims (hivemc J; G1): the model claims the task its prompt offered, by name (`picked` false,
/// no `left_to_another` refusal, no retry); b starts 2 s late. Every property but integrate-once.
#[test]
fn j_named_claims() {
    for cfg in Cfg::new(&[("a", None), ("b", None)]).node("b", |n| n.start = 2).sweep(&[1, 6]) {
        Sim::run(Cfg { named: true, ..cfg }).check(&["integrate"]);
    }
}

/// The integration race (hivemc K; CE4): lines take 2 s, an integration 5. Both nodes' drones see the
/// last verdicts within a tick of each other and claim the integrate task; neither read-back has the
/// other's claim, and the integrator never folds again, so both merge, run the oracle and write their
/// verdict. Integrate-once.
#[test]
#[ignore = "F5: integrate claims wait for every live participant, the integration is fenced by its epoch"]
fn k_ce4_two_nodes_integrate_once() {
    let r = Sim::run(Cfg { delay: 2, integrate: 5, ..Cfg::new(&[("a", None), ("b", None)]) });
    assert_eq!(r.integrated_twice(), None, "{}", r.trace());
}

/// hivemc K's other properties: the duplicate integration is ignored by the fold and costs no result.
#[test]
fn k_the_integration_race_loses_no_result() {
    Sim::run(Cfg { delay: 2, integrate: 5, ..Cfg::new(&[("a", None), ("b", None)]) }).check(&["integrate"]);
}

/// One task, a's; a starts 3 s late, b's clock is 2 s behind, lines take 2 s, work 5 (hivemc L):
/// every property but integrate-once.
#[test]
fn l_a_late_owner_with_skew() {
    let cfg = Cfg { delay: 2, work: 5, ..Cfg::new(&[("a", None)]) }.node("a", |n| n.start = 3).node("b", |n| n.skew = -2000);
    for cfg in cfg.sweep(&[1, 2, 3]) {
        Sim::run(cfg).check(&["integrate"]);
    }
}

/// CE3 (hivemc L at HEAD): a starts 3 s late, b's clock is 2 s behind, lines take 2 s, b's model 3 s to
/// claim, work 1. b's first session finds t0 left to a (a's roster landed within the owner's grace),
/// its second finds a's claim; as it ends, a's done has opened t0.v1 to b. A task that became claimable during the session
/// is no stall (run.rs `stalls` by the session's start view, the working tree's CE3 fix), so b verifies
/// instead of ending "stalled" as its work appears. No premature end.
#[test]
fn ce3_work_that_opens_during_a_session_is_no_stall() {
    let cfg = Cfg { delay: 2, work: 1, ..Cfg::new(&[("a", None)]) }.node("a", |n| n.start = 3).node("b", |n| n.skew = -2000).lat("b", "d1", &[3]);
    let r = Sim::run(cfg);
    let empty = r.log.iter().filter(|l| l.contains(" b: ") && l.contains("session-end d1")).count();
    assert_eq!(empty, 2, "b's first two sessions claim nothing\n{}", r.trace());
    assert!(r.first("b", "verified t0.v1").is_some(), "b verifies a's work\n{}", r.trace());
    r.check(&["integrate"]);
}

/// CE5: b's clock is 2 s behind and lines take 5 s, work 2. b claims a's task in the tick a does,
/// stamped 2 s earlier, before a's roster reached it; a's read-back and confirm window see nothing, a
/// works and its done takes effect in a's fold, until b's claim lands and voids it (and its verify
/// task). S2'.
#[test]
#[ignore = "F5: dones are never voided; attempts are a G-set, the accepted one chosen at integration"]
fn ce5_a_done_that_took_effect_is_never_voided() {
    let cfg = Cfg { delay: 5, work: 2, ..Cfg::new(&[("a", None)]) }.node("b", |n| n.skew = -2000);
    let r = Sim::run(cfg);
    assert_eq!(r.retracted(), None, "{}", r.trace());
}

/// Not in hivemc (its read-back was always in time): a's d1 stamps its claim of t (a's) at t=2, and the
/// line lands in the board's file 5 s later (a stall between the pre-write fold and the append, under
/// load); its read-back follows. a started a moment before b, so a's d2 started its session on u (b's,
/// in a's share while a knew no peer, #22); b's d1 claims u, and at t=5 d2 claims what is left, t
/// (`next_for`'s last resort): its read-back has no rival. d1's line lands with the earlier stamp and
/// wins its read-back too: two sessions of a held t past their read-back, d2 is told after it started.
/// Same-node exclusion without a timing assumption.
#[test]
#[ignore = "F2: same-node claims are a check-and-append under the board's file lock"]
fn same_node_claims_need_no_timing() {
    let mut cfg = Cfg::new(&[("a", None), ("b", None)]).node("a", |n| n.drones = 2).node("b", |n| n.start = 1).lat("a", "d2", &[4]);
    let t = cfg.tasks[0].0.clone();
    cfg.holds.push(Hold { node: "a", ev: Ev::Claim, task: t, local: 5, remote: 0 });
    let r = Sim::run(cfg);
    assert_eq!(r.same_node_twice(), None, "{}", r.trace());
}

/// Not in hivemc (it named integrators by node): an integrator's session is `integrate-<drone>-<ms>`,
/// so drone d1 of a and of b integrating in one ms share a session; each read-back sees "its" session
/// hold the task, whichever node's line won, and both integrate even with lines in a tick.
/// Integrate-once.
#[test]
#[ignore = "F8: unique node/session ids; an integrator's session names its node"]
fn integrators_on_two_nodes_in_one_ms_are_two_sessions() {
    let r = Sim::run(Cfg::new(&[("a", None), ("b", None)]).node("b", |n| n.skew = -1));
    assert_eq!(r.integrated_twice(), None, "{}", r.trace());
}

/// Not in hivemc (spec G5): b's integrate claim is stamped before a's (b's clock 2 s behind) and reaches
/// a only after a's integration passed; b's runner is killed before its own passes. a's runner exits 0
/// on its fold, then b's claim lands and voids a's integration: the converged board is unfinished, b's
/// lease on the integrate task live. Stable-prefix exit (and S2').
#[test]
#[ignore = "F5/F6: integrations are never voided, a runner exits 0 only on a finished stable prefix"]
fn g5_a_runner_exits_0_only_on_a_stable_prefix() {
    let mut cfg = Cfg::new(&[("a", None), ("b", None)]).node("b", |n| n.skew = -2000);
    cfg.holds.push(Hold { node: "b", ev: Ev::Claim, task: "integrate.1".into(), local: 0, remote: 6 });
    let at = Sim::run(cfg.clone()).first("b", "claim integrate.1").unwrap();
    let r = Sim::run(cfg.node("b", |n| n.stop = Some(at + 1)));
    assert_eq!(r.exit_unfinished(), None, "{}", r.trace());
}

/// hivemc's liveness sweep (tasks/soundness-model/sweep.py), reduced: task sets with dep chains within
/// and across nodes, b starting at once or 2 s late with its clock 2 s either side of a's, lines in 1 or
/// 2 s, each drone's claims after 1 or 12 s (or every drone's after 4), work and integrations 4. Every property but integrate-once
/// (CE4, `k_ce4_two_nodes_integrate_once`).
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
                        Sim::run(cfg).check(&["integrate"]);
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

/// Not in hivemc: two drones of a runner ending in one ms write two roster lines of one ms (run.rs
/// `main`, each end names the drones still running), and the fold keeps the one later in its file
/// (board.rs `fold`'s `rosters`, `at <= e.ms`): two nodes holding both lines in another order fold two
/// boards. S1.
#[test]
#[ignore = "F3: HLC stamps under the append lock, so no two lines of one node share a stamp"]
fn s1_two_roster_lines_of_one_ms_fold_to_one_board() {
    let roster = |names: &[&str]| {
        let names: Vec<String> = names.iter().map(|d| d.to_string()).collect();
        Event { node: "a".into(), ..Event::new(Ev::Heartbeat, "", "runner", 5).with_note(&board::roster_note(&names)) }
    };
    let lines = [Event::open(work_task("t1", Role::Work, &[]), "engineer:e", 0), roster(&["d2"]), roster(&[])];
    assert_eq!(same_in_any_order(&lines), Ok(()));
}
