//! Arm E on one machine (tasks/todo.md "Arm E on one machine", docs/design/hive.md C4): the
//! hive across two nodes, without a second machine. Two sandbox HOMEs A and B, each a store of its own
//! (its own node id and mesh key), both members of one team collective; each node's workspace is a
//! `--no-local` clone of one bare remote, so their object stores are separate and an attempt's commits
//! reach the other node only as a bundle (borg hive/commits.rs). A sync pump in this process runs
//! borg-mesh's own sync sessions between the two stores ([`Pump`]) as soon as a hive log changes on
//! either (as `borg mesh serve` dials), at least every [`SYNC_EVERY`], for the whole run, and
//! [`FINAL_ROUNDS`] more once both runs ended. Its transport is borg-mesh's `Node`, an iroh QUIC endpoint, bound on
//! loopback with relays off and no address lookup: iroh carries the bytes, but no network beyond this
//! machine is involved (the spec's "no iroh" means no relay or discovery). The
//! pump runs borg's session code in this process, with the nodes' switches ([`PUMP_ENV`], set by
//! `hive::main`; a test calling the cell directly runs it with the test's, the row's `sync.credit`).
//!
//! The goal is opened on A as arm D3 opens it; once it has synced to B, B's engineer approves A's check
//! (`borg hive approve`, as hive_mesh.rs does) and `borg hive run --yes` starts on both nodes at once:
//! [`NODES`] drones on A and on B, each run in its node's sandbox as D3's is. Whichever node's drone
//! integrates (the hive's rules: the first runner to see every task verified, with every accepted
//! attempt's commits present or fetchable), the scored tree is that integration's commit, found in
//! whichever node's repo holds it; both boards must name the same one. With sim drones each node's
//! script names its node, so the drones sign their files `by d1@a` and `by d1@b`: two nodes' drones of
//! one name never write one identical snapshot commit, which would be on the other node without a bundle.
//!
//! The row is D3's plus `e`: per node the attempts done there, the attempts authored there that were
//! verified, the verdicts given there, the bundles made, received, taken, refused and still waiting,
//! the runner's exit and dollars; the attempts verified, and accepted attempts integrated, on a node
//! other than their author's whose bundle that node took (`cross_node`, by node; `on_board`: what the
//! board alone shows); whether the two boards fold identically (a digest of each fold); the most
//! sessions holding one task at once (from the fold's tenures), and the claims that lost a race, with
//! those whose session still wrote its done (double work a lost race hides from the holder count);
//! the pump's rounds, failed rounds and lines moved.
//!
//! Budget: `spend.jsonl` syncs (borg hive/run.rs `ledger`, `borg/mesh/10`), each line naming its node, so
//! both runners read the goal's one total and every drone stops once it reaches the cap. The bound: the
//! cap plus the iterations in flight when the total reached it, at most one per drone (three here),
//! each at most its drone's share left, while the pump syncs faster than an iteration lasts (a spend
//! line is a hive log: at once, and every [`SYNC_EVERY`] at the latest). Both runs start at once and
//! each splits the whole cap (neither has seen the other's spend yet), so in the worst case those in-flight iterations still sum to a cap per node:
//! `--stop-at-usd` keeps reserving twice the cap for the cell (`Arm::reserve`). The row counts both
//! nodes' dollars against the cap (`over_cap`), each node's from its own lines of the synced file.

use super::*;
use borg_mesh::daemon;
use borg_mesh::identity::Identity;
use borg_mesh::membership::Membership;
use borg_mesh::sync::{Node, Place};

/// The nodes and their drones: 2 on A, where the goal is opened, 1 on B (the spec's split).
pub(super) const NODES: [(&str, usize); 2] = [("a", 2), ("b", 1)];
/// Each node's collectives config: the workspace routes to collective `pilot`, shared with the team
/// (a local collective never syncs), relays off (loopback only).
const TEAM: &str = r#"{"default":"off","collectives":[{"name":"pilot","paths":["~/work"],"share":"team","relay":"disabled"}]}"#;
/// How often the pump runs a sync session while the runs go, at the latest: a change to a hive log
/// starts one at once, looked for at `serve`'s pace (daemon.rs `hive_poll`; phase e E3).
const SYNC_EVERY: Duration = Duration::from_secs(2);
/// Sessions after both runs ended: a verdict that arrived is credited at the next session's stamp,
/// and the one after carries that back (hive_mesh.rs).
const FINAL_ROUNDS: u32 = 2;
/// One sync session's bound; one that does not finish counts as failed.
const ROUND_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the goal may take to reach B.
const GOAL_WAIT: Duration = Duration::from_secs(120);
const LOOPBACK: &str = "127.0.0.1:0";

/// What the pump moved over its sessions (each side's report summed: a line moved once counts as
/// sent by one side and received by the other).
#[derive(Debug, Default, Clone)]
struct Synced {
    rounds: u64,
    failed: u64,
    sent: usize,
    received: usize,
    rejected: usize,
    quarantined: usize,
    /// The first failures, at most [`ERRORS_KEPT`].
    errors: Vec<String>,
}

const ERRORS_KEPT: usize = 5;

impl Synced {
    fn fail(&mut self, why: String) {
        self.failed += 1;
        if self.errors.len() < ERRORS_KEPT {
            self.errors.push(why);
        }
    }

    fn json(&self) -> Value {
        json!({"rounds": self.rounds, "failed": self.failed, "sent": self.sent, "received": self.received, "rejected": self.rejected,
               "quarantined": self.quarantined, "errors": self.errors, "every_s": SYNC_EVERY.as_secs(),
               "hive_poll_ms": daemon::Options::default().hive_poll.as_millis() as u64, "final_rounds": FINAL_ROUNDS,
               "credit": pump_credits()})
    }
}

/// The switches the nodes run with that the pump's sessions also read (borg's hive `credit_synced`, run
/// in this process at each session's stamp): what `borg mesh serve` would run with on each node.
pub(super) const PUMP_ENV: [(&str, &str); 2] = [("BORG_LEARN", "off"), ("BORG_HIVE_CREDIT", "off")];

/// Whether the pump's sessions credit verdicts (they should not: the nodes run with learning off).
fn pump_credits() -> bool {
    borg::learn::learning() && std::env::var("BORG_HIVE_CREDIT").as_deref() != Ok("off")
}

/// The sync pump: a thread with a tokio runtime holding two mesh nodes bound on loopback, one per
/// store, running a session (B dials A) once a hive log changed on either and at least every `every`
/// until stopped, then [`FINAL_ROUNDS`] more, then closing both endpoints. Stopped by [`Pump::stop`], or when dropped (an early return of the cell).
struct Pump {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<Synced>>,
}

impl Pump {
    /// Starts it once both nodes are bound.
    fn start(a: Place, b: Place, every: Duration) -> Result<Pump, String> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("arm-e-sync".into())
            .spawn(move || {
                let mut s = Synced::default();
                let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("sync pump runtime: {e}")));
                        return s;
                    }
                };
                rt.block_on(async {
                    let addr = LOOPBACK.parse().expect("a socket address");
                    let na = match Node::bind(&a, addr).await {
                        Ok(n) => n.quiet(),
                        Err(e) => {
                            let _ = ready_tx.send(Err(format!("node a: {e}")));
                            return;
                        }
                    };
                    let nb = match Node::bind(&b, addr).await {
                        Ok(n) => n.quiet(),
                        Err(e) => {
                            na.close().await;
                            let _ = ready_tx.send(Err(format!("node b: {e}")));
                            return;
                        }
                    };
                    let _ = ready_tx.send(Ok(()));
                    let (hive, poll) = (|n: &Node| daemon::hive_state(n.home()), daemon::Options::default().hive_poll);
                    'pump: loop {
                        let seen = (hive(&na), hive(&nb));
                        round(&na, &nb, &mut s).await;
                        // The next session at `every`, or as soon as a hive log changed on either node (a claim,
                        // a done: what `serve` dials for at once, daemon.rs `hive_poll`), including during this one.
                        let next = tokio::time::Instant::now() + every;
                        while tokio::time::Instant::now() < next && seen == (hive(&na), hive(&nb)) {
                            // A dropped sender (the pump dropped) stops it as a stop does.
                            tokio::select! {
                                _ = &mut stop_rx => break 'pump,
                                () = tokio::time::sleep(poll) => {}
                            }
                        }
                    }
                    for _ in 0..FINAL_ROUNDS {
                        round(&na, &nb, &mut s).await;
                    }
                    na.close().await;
                    nb.close().await;
                });
                s
            })
            .map_err(|e| format!("sync pump: {e}"))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Pump { stop: Some(stop_tx), thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err("sync pump ended before its nodes were bound".into())
            }
        }
    }

    /// Stops the pump after its final sessions: what it moved.
    fn stop(mut self) -> Synced {
        self.finish()
    }

    fn finish(&mut self) -> Synced {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        self.thread.take().and_then(|t| t.join().ok()).unwrap_or_default()
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// One session: B dials A, A serves (hive_mesh.rs `round`). A dial that fails ends the session without
/// waiting for A to be dialled.
async fn round(a: &Node, b: &Node, s: &mut Synced) {
    let ticket = a.ticket();
    s.rounds += 1;
    let session = async {
        let served = a.accept();
        let synced = b.sync(&ticket);
        tokio::pin!(served, synced);
        let (mut sv, mut sy) = (None, None);
        while sv.is_none() || sy.is_none() {
            tokio::select! {
                r = &mut served, if sv.is_none() => sv = Some(r),
                r = &mut synced, if sy.is_none() => {
                    if r.is_err() && sv.is_none() {
                        sv = Some(Err("not served: the dial failed".to_string()));
                    }
                    sy = Some(r);
                }
            }
        }
        (sv.unwrap_or_else(|| Err("no session".into())), sy.unwrap_or_else(|| Err("no session".into())))
    };
    match tokio::time::timeout(ROUND_TIMEOUT, session).await {
        Ok((Ok(x), Ok(y))) => {
            for r in [x, y] {
                s.sent += r.sent;
                s.received += r.received;
                s.rejected += r.rejected;
                s.quarantined += r.quarantined;
            }
        }
        Ok((x, y)) => s.fail(format!("served: {:?}; synced: {:?}", x.err(), y.err())),
        Err(_) => s.fail(format!("a session did not finish within {}s", ROUND_TIMEOUT.as_secs())),
    }
}

/// One node of the cell.
struct Side {
    name: &'static str,
    drones: usize,
    ag: Agent,
    /// The collective's store (the hive's `home`).
    store: PathBuf,
    id: String,
}

impl Side {
    fn place(&self) -> Place {
        Place {
            config: self.ag.home.join(".config/borg/collectives.json"),
            base: self.ag.home.join(hive_seq::KEEP[0]),
            dir: self.ag.ws.display().to_string(),
            collective: Some(hive_seq::COLLECTIVE.into()),
        }
    }

    fn gdir(&self, goal: &str) -> PathBuf {
        self.store.join("hive").join(goal)
    }

    fn holds(&self, commit: &str) -> bool {
        run_t(&self.ag.ws, &format!("{GIT} cat-file -e {}", q(format!("{commit}^{{commit}}"))), 30).0
    }
}

/// How a cell's runner ended on one node.
struct Ran {
    code: i32,
    out: String,
    err: String,
    wall: f64,
}

/// The most sessions holding one task at once, per task, from the fold's tenures (a tenure still open
/// or expired counts as held from then on, as hive_mesh.rs `one_holder_at_a_time` reads it).
fn max_holders(b: &board::Board) -> BTreeMap<String, usize> {
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for t in b.tasks.keys() {
        let spans: Vec<(i64, i64)> = b.spans.iter().filter(|s| &s.task == t).map(|s| (s.from_ms, s.to_ms.unwrap_or(i64::MAX))).collect();
        let most = spans.iter().map(|&(from, _)| spans.iter().filter(|&&(f, to)| f <= from && from < to).count()).max().unwrap_or(0);
        out.insert(t.clone(), most);
    }
    out
}

/// The node an attempt was done on: its accepted verify task's (opened with the work's `done`, on the
/// work's node).
fn author_node<'a>(b: &'a board::Board, work: &board::TaskState) -> Option<&'a str> {
    work.accepted.as_ref().and_then(|v| b.tasks.get(v)).map(|v| v.node.as_str()).filter(|n| !n.is_empty())
}

/// The attempts (work task, head) each node took from a bundle: the `ok` lines of its `unbundled.jsonl`
/// (borg hive/commits.rs `fetch`), by node id.
type Taken = BTreeMap<String, BTreeSet<(String, String)>>;

/// Work done on one node and verified, or integrated, on another, counted only where the judging node
/// took that attempt's commits from a bundle: a board alone does not show that commits travelled (two
/// nodes may hold one identical commit of their own).
#[derive(Debug, Default, PartialEq)]
struct Cross {
    /// Verify tenures that ended `verified` on a node other than the attempt's, which took its bundle.
    verified: usize,
    /// Accepted attempts of the passing integration done on another node than the integrator's, which
    /// took their bundles.
    integrated: usize,
    /// Both, by the judging node's id.
    by_node: BTreeMap<String, (usize, usize)>,
    /// What the board alone shows across nodes, bundle or not: (verified, integrated).
    on_board: (usize, usize),
    /// The integrator's node, if an integration passed.
    integrator: Option<String>,
}

fn cross_node(b: &board::Board, events: &[board::Event], taken: &Taken) -> Cross {
    let took = |node: &str, a: &borg::hive::verdict::Attempt| taken.get(node).is_some_and(|t| t.contains(&(a.work.clone(), a.head.clone())));
    let away = |node: &str, a: &borg::hive::verdict::Attempt| !node.is_empty() && !a.node.is_empty() && node != a.node;
    let mut c = Cross::default();
    let verifies = b.spans.iter().filter(|s| s.end == Some(board::Ev::Verified) && b.tasks.get(&s.task).is_some_and(|t| t.task.role == board::Role::Verify));
    for s in verifies {
        let Some(a) = borg::hive::verdict::attempt(events, b, &s.task) else { continue };
        if away(&s.node, &a) {
            c.on_board.0 += 1;
            if took(&s.node, &a) {
                c.verified += 1;
                c.by_node.entry(s.node.clone()).or_default().0 += 1;
            }
        }
    }
    c.integrator = b
        .integrated
        .as_ref()
        .and_then(|i| b.spans.iter().find(|s| s.task == i.task && s.end == Some(board::Ev::Verified)))
        .map(|s| s.node.clone())
        .filter(|n| !n.is_empty());
    if let Some(n) = c.integrator.clone() {
        let works = b.tasks.values().filter(|w| w.task.role == board::Role::Work);
        for a in works.filter_map(|w| borg::hive::verdict::accepted(events, b, &w.task.id)).filter(|a| away(&n, a)) {
            c.on_board.1 += 1;
            if took(&n, &a) {
                c.integrated += 1;
                c.by_node.entry(n.clone()).or_default().1 += 1;
            }
        }
    }
    c
}

/// One node's taken attempts (`Taken`).
fn taken_on(side: &Side, goal: &str) -> BTreeSet<(String, String)> {
    unbundled(side, goal).iter().filter(|o| o["ok"] == true).filter_map(|o| Some((o["task"].as_str()?.to_string(), o["head"].as_str()?.to_string()))).collect()
}

fn unbundled(side: &Side, goal: &str) -> Vec<Value> {
    fs::read_to_string(side.gdir(goal).join("unbundled.jsonl")).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Claims that lost a race (`Board::lost`), and those of them whose session still wrote a `done` on that
/// task after it (work the fold ignores: across nodes a loser learns it lost only at the next sync, and
/// works meanwhile), as (task, session).
fn lost_claims(b: &board::Board, events: &[board::Event]) -> (usize, Vec<(String, String)>) {
    let worked = b
        .lost
        .iter()
        .filter(|(by, node, task, ms)| events.iter().any(|e| e.ev == board::Ev::Done && &e.task == task && &e.by == by && &e.node == node && e.ms >= *ms))
        .map(|(by, _, task, _)| (task.clone(), by.clone()))
        .collect();
    (b.lost.len(), worked)
}

/// Each lost claim's waste, in seconds: from the claim to the first of its session's next claim, a
/// release, done or block of the task (the loser dropped it, or worked it to the end), and the end of
/// its iteration (its `spend` line); None when none is known. Before a loser heard of its loss at its
/// next hook (tasks/todo.md E1) it worked on until its turn ended.
fn loser_waste(b: &board::Board, events: &[board::Event], spend: &[hive_run::Spend]) -> Vec<Option<f64>> {
    use board::Ev;
    b.lost
        .iter()
        .map(|(by, node, task, ms)| {
            let next = events.iter().filter(|e| &e.by == by && &e.node == node && e.ms > *ms).filter(|e| match e.ev {
                Ev::Claim => true,
                Ev::Released | Ev::Done | Ev::Blocked => &e.task == task,
                _ => false,
            });
            // A spend line names its iteration's sessions, comma-separated.
            let end = spend.iter().filter(|s| s.session.split(',').any(|x| x == by) && s.ms >= *ms).map(|s| s.ms);
            next.map(|e| e.ms).chain(end).min().map(|end| (end - ms) as f64 / 1000.0)
        })
        .collect()
}

/// The `q` quantile (nearest rank) of `xs`; None when empty.
pub(super) fn quantile(xs: &[f64], q: f64) -> Option<f64> {
    let mut v = xs.to_vec();
    v.sort_by(f64::total_cmp);
    let i = ((q * v.len() as f64).ceil() as usize).clamp(1, v.len().max(1)) - 1;
    v.get(i).copied()
}

/// One node's stats: attempts done there, attempts authored there that were verified, verdicts given (and how many passed)
/// there, bundles made there, received from the other node, taken (unbundled), refused and still waiting
/// here (a failure that may pass, not taken since), with the reasons, and its claims refused before
/// their write (a rival's had landed: a race no board shows).
fn node_stats(b: &board::Board, events: &[board::Event], side: &Side, goal: &str) -> Value {
    let id = side.id.as_str();
    let role = |t: &str| b.tasks.get(t).map(|s| s.task.role);
    let done = events.iter().filter(|e| e.ev == board::Ev::Done && e.node == id && role(&e.task) == Some(board::Role::Work)).count();
    let verified = b
        .tasks
        .values()
        .filter(|w| w.task.role == board::Role::Work && matches!(w.status, board::Status::Verified { .. }) && author_node(b, w) == Some(id))
        .count();
    let verdicts = b
        .spans
        .iter()
        .filter(|s| s.node == id && matches!(s.end, Some(board::Ev::Verified | board::Ev::Rejected)) && role(&s.task) == Some(board::Role::Verify))
        .count();
    let passes = b.spans.iter().filter(|s| s.node == id && matches!(s.end, Some(board::Ev::Verified)) && role(&s.task) == Some(board::Role::Verify)).count();
    let integrations = b
        .spans
        .iter()
        .filter(|s| s.node == id && matches!(s.end, Some(board::Ev::Verified | board::Ev::Rejected)) && role(&s.task) == Some(board::Role::Integrate))
        .count();
    let lines = borg::hive::commits::read(&side.store, goal);
    let made = lines.iter().filter(|(_, l)| l.node == id).count();
    let outcomes = unbundled(side, goal);
    // Each line's outcomes: taken (an ok one), refused for good (what the bundle holds), or still waiting
    // (only failures that may pass, commits.rs `retry`: tried again when the commits are next needed).
    let line_of = |o: &Value| o["line"].as_str().map(String::from);
    let taken: BTreeSet<String> = outcomes.iter().filter(|o| o["ok"] == true).filter_map(line_of).collect();
    let refused: BTreeSet<String> = outcomes.iter().filter(|o| o["ok"] == false && o["retry"] != true).filter_map(line_of).collect();
    let waiting: BTreeSet<String> =
        outcomes.iter().filter(|o| o["ok"] == false).filter_map(line_of).filter(|l| !taken.contains(l) && !refused.contains(l)).collect();
    let why: BTreeSet<&str> = outcomes.iter().filter(|o| o["ok"] == false).filter_map(|o| o["why"].as_str()).collect();
    let confirms: Vec<borg::hive::Confirm> = read_jsonl(&borg::hive::confirms_path(&side.store, goal));
    let refused_claims = confirms.iter().filter(|c| c.outcome == borg::hive::REFUSED).count();
    json!({"id": id, "drones": side.drones, "refused_claims": refused_claims, "attempts_done": done, "attempts_verified": verified, "verdicts": verdicts, "verdicts_passed": passes, "integrations": integrations,
           "bundles": {"made": made, "received": lines.len() - made, "taken": taken.len(), "refused": refused.len(), "waiting": waiting.len(), "why_not": why}})
}

/// A digest of what the fold says of the lines: each node's own clock (when it held them, `Board::nodes`
/// and each task's `Local`) is left out, as `Board`'s equality leaves it out.
fn digest(b: &board::Board) -> String {
    let mut b = b.clone();
    b.nodes.clear();
    for s in b.tasks.values_mut() {
        s.local = board::Local::default();
    }
    blake3::hash(format!("{b:?}").as_bytes()).to_hex()[..16].to_string()
}

/// Every parsable line of a JSONL file (none when it does not exist).
fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Arm E (module doc).
pub(super) fn run(o: &Opts, wl: &Workload, env: &Env, cell: &Path, row: &mut serde_json::Map<String, Value>) -> Result<(), String> {
    // The shared remote: the workload's workspace, bare.
    let origin = cell.join("origin");
    let base = build_workspace(wl, &origin)?;
    let remote = cell.join("remote.git");
    let (ok, _, out) =
        run_t(cell, &format!("{GIT} clone -q --bare --no-local {} {}", q(origin.display().to_string()), q(remote.display().to_string())), ARCHIVE_TIMEOUT_S);
    if !ok {
        return Err(format!("bare remote: {}", out.trim()));
    }
    row.insert("base".into(), json!(base));
    let mut sides = Vec::new();
    for (name, drones) in NODES {
        let dir = cell.join(name);
        let home = agent_home(&dir, o);
        let sim = home.join("sim.json");
        if o.sim {
            fs::create_dir_all(&home).map_err(|e| e.to_string())?;
            // The node's drones sign their files with its name: A's d1 and B's d1 then never make one
            // identical commit, which would reach the other node without a bundle.
            let mut script = wl.sim.clone().unwrap_or(json!({}));
            if let Some(m) = script.as_object_mut() {
                m.insert("node".into(), json!(name));
            }
            fs::write(&sim, script.to_string()).map_err(|e| e.to_string())?;
        }
        let ag = make_agent_from(&dir, o, wl, env, o.sim.then_some(sim.as_path()), TEAM, Some(&remote))?;
        if ag.base != base {
            return Err(format!("node {name}: its clone's HEAD {} is not the base {base}", ag.base));
        }
        let store = borg::collective::store(&ag.home.join(hive_seq::KEEP[0]), hive_seq::COLLECTIVE);
        sides.push(Side { name, drones, ag, store, id: String::new() });
    }
    let before: u64 = sides.iter().map(|s| s.ag.lessons()).sum();
    row.insert("store_lessons_before".into(), json!(before));
    if before != 0 {
        return Err(format!("the fresh stores hold {before} lesson line(s)"));
    }
    let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    for s in &sides {
        let (code, out, err) = s.ag.exec(&s.ag.home, &argv(&["borg", "install", "claude", "--apply"]), 60);
        let wired = wired_events(&s.ag.home);
        if code != 0 || wired != 5 {
            return Err(format!("node {} borg install: exit {code}, {wired}/5 events wired: {out}{err}", s.name));
        }
    }
    // One collective: A founds it, B is invited as a member (hive_mesh.rs); each store its own key.
    let admin = Identity::load_or_create(&sides[0].store)?;
    let member = Identity::load_or_create(&sides[1].store)?;
    let mut m = Membership::found(hive_seq::COLLECTIVE, &admin);
    m.invite(&admin, &member.public(), "member")?;
    for s in &mut sides {
        m.save(&s.store)?;
        s.id = borg::hive::node(&s.store);
    }
    if sides[0].id == sides[1].id {
        return Err("both stores have one node id".into());
    }
    let (a, b) = (&sides[0], &sides[1]);
    let spec = a.ag.home.join("goal/spec.md");
    fs::create_dir_all(spec.parent().unwrap_or(&a.ag.home)).map_err(|e| e.to_string())?;
    fs::write(&spec, wl.prompt()).map_err(|e| e.to_string())?;
    let (code, out, err) = a.ag.exec(&a.ag.ws, &d3_init_args(wl, &spec, o.cap, &o.goal), 60);
    if code != 0 {
        return Err(format!("borg hive init on a: exit {code}: {out}{err}"));
    }
    let pump = Pump::start(a.place(), b.place(), SYNC_EVERY)?;
    let t0 = Instant::now();
    while borg::hive::goal::load(&b.store, &o.goal).is_err() {
        if t0.elapsed() > GOAL_WAIT {
            let s = pump.stop();
            return Err(format!("the goal did not reach b within {}s (sync: {})", GOAL_WAIT.as_secs(), s.json()));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let goal_synced_s = t0.elapsed().as_secs_f64();
    // B's engineer approves the check A wrote (its drones may not run another node's shell commands
    // before); never a drone.
    let ws_b = b.ag.ws.display().to_string();
    let (code, out, err) = b.ag.exec(&b.ag.ws, &argv(&["borg", "hive", "approve", &o.goal, "--dir", &ws_b]), 60);
    if code != 0 {
        return Err(format!("borg hive approve on b: exit {code}: {out}{err}"));
    }
    eprintln!(
        "  {} e: borg hive run on a (--drones {}) and b (--drones {}) (cap ${:.2}, wall cap {}s, goal on b after {goal_synced_s:.1}s) …",
        wl.id, NODES[0].1, NODES[1].1, o.cap, o.timeout_s
    );
    mark_paid(row);
    let t = Instant::now();
    let ran: Vec<Ran> = std::thread::scope(|sc| {
        let hs: Vec<_> = sides
            .iter()
            .map(|s| {
                sc.spawn(move || {
                    let t = Instant::now();
                    let args = run_args(&o.model, &s.ag.base, o.timeout_s, o.sim, &o.goal, s.drones);
                    let (code, out, err) = s.ag.exec(&s.ag.ws, &args, o.timeout_s);
                    Ran { code, out, err, wall: t.elapsed().as_secs_f64() }
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap_or_else(|_| Ran { code: -1, out: String::new(), err: "runner thread panicked".into(), wall: 0.0 })).collect()
    });
    let run_wall = t.elapsed().as_secs_f64();
    let synced = pump.stop();
    for (s, r) in sides.iter_mut().zip(&ran) {
        let _ = fs::write(cell.join(format!("run-{}.out", s.name)), &r.out);
        let _ = fs::write(cell.join(format!("run-{}.err", s.name)), &r.err);
        s.ag.secrets = None;
    }

    // Dollars, tokens and wall first, summed over the nodes.
    let (mut charged, mut reported, mut all_named, mut iterations, mut cut_usd) = (0.0, 0.0, true, 0usize, 0.0);
    let mut tokens = Tokens::default();
    let mut models = BTreeSet::new();
    let mut per_node = serde_json::Map::new();
    let (mut cut_all, mut ends_all, mut spend_lines) = (serde_json::Map::new(), serde_json::Map::new(), 0usize);
    for (s, r) in sides.iter().zip(&ran) {
        let gdir = s.gdir(&o.goal);
        let (c, rep, named, iters) = d3_spend(&gdir.join("spend.jsonl"), Some(&s.id));
        let (cu, cut) = d3_unreported(&gdir, o.cap / s.drones as f64, Some(&s.id));
        let (tk, ms) = d3_tokens(&gdir.join("logs"));
        charged += c;
        reported += rep;
        all_named &= named;
        iterations += iters + cut.len();
        cut_usd += cu;
        tokens.add(tk);
        models.extend(ms);
        spend_lines += iters;
        let ends: Vec<&str> = r.out.lines().filter(|l| l.starts_with('d') && l.contains(" end: ")).collect();
        ends_all.insert(s.name.into(), json!(ends));
        per_node.insert(s.name.into(), json!({"exit": r.code, "wall_s": r.wall, "charged_usd": c + cu, "iterations": iters, "cut_off_drones": cut}));
        cut_all.insert(s.name.into(), json!(cut));
    }
    let timed_out = ran.iter().any(|r| r.code == 124 || r.code == 137);
    let complete = all_named && cut_all.values().all(|v| v.as_array().is_some_and(Vec::is_empty)) && !timed_out;
    let total = charged + cut_usd;
    let cost = Cost { usd: complete.then_some(total), complete, reported, charged: total, tokens, turns: None, models: models.into_iter().collect() };
    put_cost(row, &cost, o.cap);
    row.insert("wall_s".into(), json!(run_wall));
    row.insert("timed_out".into(), json!(timed_out));
    row.insert("sessions".into(), json!(iterations));

    // The boards: A's facts (the boards must be one), and each node's fold.
    let tasks: Vec<PathBuf> = sides.iter().map(|s| s.gdir(&o.goal).join("tasks.jsonl")).collect();
    let (rework, hacked_verdicts, statuses) = board_facts(&tasks[0]);
    row.insert("rework".into(), json!(rework));
    row.insert("hacked_verdicts".into(), json!(hacked_verdicts));
    let (verifier_rejects, failed_integrations) = board_rejects(&tasks[0]);
    row.insert("verifier_rejects".into(), json!(verifier_rejects));
    row.insert("failed_integrations".into(), json!(failed_integrations));
    let events: Vec<Vec<board::Event>> = tasks.iter().map(|p| board::read(p)).collect();
    let folds: Vec<board::Board> = events.iter().map(|e| board::fold(e)).collect();
    let identical = folds[0] == folds[1];
    let results: Vec<D3Result> = tasks.iter().map(|p| d3_result(p, &base)).collect();
    let heads_agree = results[0].head == results[1].head;
    let holders = max_holders(&folds[0]);
    let taken: Taken = sides.iter().map(|s| (s.id.clone(), taken_on(s, &o.goal))).collect();
    let cross = cross_node(&folds[0], &events[0], &taken);
    let integrator = cross.integrator.clone();
    let (lost, lost_worked) = lost_claims(&folds[0], &events[0]);
    // Both nodes' spend lines (the file syncs; a line on both counts once per session and ms).
    let spend: Vec<hive_run::Spend> = sides.iter().flat_map(|s| spend_of(&s.gdir(&o.goal).join("spend.jsonl"), None)).collect();
    let waste = loser_waste(&folds[0], &events[0], &spend);
    let known: Vec<f64> = waste.iter().flatten().copied().collect();
    // The added claim latency (phase e E4: each confirm window, both nodes') and each run's start wait (E2b).
    let confirms: Vec<borg::hive::Confirm> = sides.iter().flat_map(|s| read_jsonl(&borg::hive::confirms_path(&s.store, &o.goal))).collect();
    let confirm_ms: Vec<f64> = confirms.iter().filter(|c| c.outcome != borg::hive::REFUSED).map(|c| c.waited_ms as f64).collect();
    let mut confirm_outcomes: BTreeMap<String, usize> = BTreeMap::new();
    for c in &confirms {
        *confirm_outcomes.entry(c.outcome.clone()).or_default() += 1;
    }
    let start_wait_ms: BTreeMap<&str, Option<u64>> =
        sides.iter().map(|s| (s.name, read_jsonl::<hive_run::RunLine>(&hive_run::runs_path(&s.store, &o.goal)).last().map(|r| r.waited_ms))).collect();
    let name_of = |id: &str| sides.iter().find(|s| s.id == id).map(|s| s.name);
    for (s, (b, ev)) in sides.iter().zip(folds.iter().zip(&events)) {
        let mut v = node_stats(b, ev, s, &o.goal);
        if let (Some(obj), Some(extra)) = (v.as_object_mut(), per_node.get(s.name).and_then(Value::as_object)) {
            obj.extend(extra.clone());
        }
        per_node.insert(s.name.into(), v);
    }
    let r = &results[0];
    row.insert("integrated".into(), json!(r.integrated));
    row.insert("integrations".into(), json!(r.integrations));
    row.insert("merge_conflicts".into(), json!(r.conflicts));
    let head = r.head.clone();
    row.insert("result_head".into(), json!(head));
    row.insert("store_lessons_after".into(), json!(sides.iter().map(|s| s.ag.lessons()).sum::<u64>()));
    // The scored tree: in the repo that holds it (the integrator's, else either; the base is in both).
    let scored_from = integrator
        .as_deref()
        .and_then(name_of)
        .and_then(|n| sides.iter().find(|s| s.name == n && s.holds(&head)))
        .or_else(|| sides.iter().find(|s| s.holds(&head)));
    row.insert(
        "e".into(),
        json!({"finished": ran.iter().all(|r| r.code == 0), "exits": sides.iter().zip(&ran).map(|(s, r)| (s.name, r.code)).collect::<BTreeMap<_, _>>(),
               "split": NODES.iter().map(|(n, d)| format!("{n}:{d}")).collect::<Vec<_>>(), "iterations": iterations, "drone_ends": ends_all,
               "board": statuses, "spend_lines": spend_lines, "cut_off_drones": cut_all, "cut_off_usd": cut_usd, "goal_synced_s": goal_synced_s,
               "boards_identical": identical, "board_digest": sides.iter().zip(&folds).map(|(s, b)| (s.name, digest(b))).collect::<BTreeMap<_, _>>(),
               "boards_finished": sides.iter().zip(&folds).map(|(s, b)| (s.name, b.finished())).collect::<BTreeMap<_, _>>(),
               "integrated_heads": sides.iter().zip(&results).map(|(s, r)| (s.name, r.head.clone())).collect::<BTreeMap<_, _>>(), "heads_agree": heads_agree,
               "integrated_on": integrator.as_deref().and_then(name_of), "scored_from": scored_from.map(|s| s.name),
               "max_holders": holders.values().copied().max().unwrap_or(0), "max_holders_by_task": holders,
               "lost_claims": lost, "lost_then_done": lost_worked.len(), "lost_then_done_tasks": lost_worked,
               "loser_waste_s": waste, "loser_waste_p95_s": quantile(&known, 0.95), "loser_waste_max_s": quantile(&known, 1.0),
               "confirm_ms": confirm_ms, "confirm_outcomes": confirm_outcomes, "start_wait_ms": start_wait_ms,
               "cross_node": {"verified": cross.verified, "integrated": cross.integrated,
                              "by_node": cross.by_node.iter().filter_map(|(id, (v, i))| Some((name_of(id)?, json!({"verified": v, "integrated": i})))).collect::<BTreeMap<_, _>>(),
                              "on_board": {"verified": cross.on_board.0, "integrated": cross.on_board.1}},
               "nodes": per_node, "sync": synced.json()}),
    );
    let Some(side) = scored_from else {
        return Err(format!("no node's repo holds the scored commit {head}"));
    };
    if !heads_agree {
        return Err(format!("the boards name different results: a {}, b {}", results[0].head, results[1].head));
    }
    let bx = Jail::scorer(cell, wl, &env.real_home);
    let s = score(&bx, wl, &side.ag.ws, &base, &head, &cell.join("score"), Which::Both)?;
    put_score(row, &s, wl.k(), hacked_verdicts);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use borg::hive::board::{Ev, Event, Role, Task};

    fn task(id: &str, role: Role, of: Option<&str>) -> Task {
        Task {
            id: id.into(),
            role,
            title: id.into(),
            repo: String::new(),
            files: Vec::new(),
            deps: Vec::new(),
            accept: Vec::new(),
            of: of.map(String::from),
            not_by: Vec::new(),
            flags: Vec::new(),
            from: None,
            group: None,
        }
    }

    fn on(mut e: Event, node: &str) -> Event {
        e.node = node.into();
        e
    }

    /// Work done on b, verified on a: one cross-node verify once a took b's bundle of it, none before (a
    /// board alone does not show that commits travelled).
    #[test]
    fn cross_node_counts_a_verify_away_from_the_author_only_with_its_bundle_taken() {
        let mut ev = vec![on(Event::open(task("t1", Role::Work, None), "eng", 1), "A")];
        let mut claim = Event::new(Ev::Claim, "t1", "sb", 2).with_drone("d1");
        claim.base = "b0".into();
        ev.push(on(claim, "B"));
        let mut done = Event::new(Ev::Done, "t1", "sb", 3);
        done.head = "h1".into();
        ev.push(on(done, "B"));
        ev.push(on(Event::open(task("t1.v1", Role::Verify, Some("t1")), "sb", 3), "B"));
        ev.push(on(Event::new(Ev::Claim, "t1.v1", "sa", 4).with_drone("d1"), "A"));
        ev.push(on(Event::new(Ev::Verified, "t1.v1", "sa", 5), "A"));
        let b = board::fold(&ev);
        let none = cross_node(&b, &ev, &Taken::new());
        assert_eq!((none.verified, none.on_board, none.integrator), (0, (1, 0), None), "{:?}", b.spans);
        let other = Taken::from([("A".to_string(), BTreeSet::from([("t1".to_string(), "h0".to_string())]))]);
        assert_eq!(cross_node(&b, &ev, &other).verified, 0, "another head of t1");
        let taken = Taken::from([("A".to_string(), BTreeSet::from([("t1".to_string(), "h1".to_string())]))]);
        let c = cross_node(&b, &ev, &taken);
        assert_eq!((c.verified, c.by_node.get("A").copied()), (1, Some((1, 0))));
        assert_eq!(author_node(&b, &b.tasks["t1"]), Some("B"));
        assert_eq!(max_holders(&b).values().copied().max(), Some(1));
    }

    /// A claim that lost the race and whose session still wrote its done: one lost claim, worked on.
    #[test]
    fn a_lost_claim_whose_session_still_did_the_work_is_reported() {
        let mut ev = vec![on(Event::open(task("t1", Role::Work, None), "eng", 1), "A")];
        ev.push(on(Event::new(Ev::Claim, "t1", "sb", 2).with_drone("d1"), "B"));
        ev.push(on(Event::new(Ev::Claim, "t1", "sa", 3).with_drone("d1"), "A"));
        ev.push(on(Event::new(Ev::Done, "t1", "sa", 6), "A"));
        let b = board::fold(&ev);
        assert_eq!(lost_claims(&b, &ev), (1, vec![("t1".to_string(), "sa".to_string())]), "{:?}", b.lost);
        assert_eq!(max_holders(&b)["t1"], 1, "a lost claim holds nothing");
    }

    /// A loser's waste ends at its next claim, a done of the lost task, or its iteration's end,
    /// whichever comes first.
    #[test]
    fn a_losers_waste_runs_to_its_next_claim_done_or_iteration_end() {
        let mut ev = vec![on(Event::open(task("t1", Role::Work, None), "eng", 1), "A"), on(Event::open(task("t2", Role::Work, None), "eng", 1), "A")];
        ev.push(on(Event::new(Ev::Claim, "t1", "sb", 1_000).with_drone("d1"), "B"));
        ev.push(on(Event::new(Ev::Claim, "t1", "sa", 1_010).with_drone("d1"), "A"));
        ev.push(on(Event::new(Ev::Heartbeat, "t1", "sa", 2_000), "A"));
        ev.push(on(Event::new(Ev::Claim, "t2", "sa", 4_010).with_drone("d1"), "A"));
        let spend = |session: &str, ms: i64| hive_run::Spend { v: 1, ms, session: session.into(), ..hive_run::Spend::default() };
        let b = board::fold(&ev);
        assert_eq!(loser_waste(&b, &ev, &[spend("sa", 9_000)]), [Some(3.0)], "dropped for t2 after 3 s");
        let mut done = ev[..4].to_vec();
        done.push(on(Event::new(Ev::Done, "t1", "sa", 61_010), "A"));
        assert_eq!(loser_waste(&board::fold(&done), &done, &[spend("sa", 70_000)]), [Some(60.0)], "worked to its done");
        let quiet = ev[..4].to_vec();
        assert_eq!(loser_waste(&board::fold(&quiet), &quiet, &[spend("sa,sa", 6_010), spend("sb", 9_000)]), [Some(5.0)], "its iteration ended");
        assert_eq!(loser_waste(&board::fold(&quiet), &quiet, &[]), [None]);
        assert_eq!((quantile(&[1.0, 5.0, 2.0], 0.95), quantile(&[], 0.95)), (Some(5.0), None));
    }

    #[test]
    fn two_overlapping_tenures_are_two_holders() {
        let mut b = board::Board::default();
        b.tasks.insert(
            "t1".into(),
            board::TaskState {
                task: task("t1", Role::Work, None),
                status: board::Status::Open,
                opened_ms: 0,
                rework: 0,
                notes: Vec::new(),
                last_progress: String::new(),
                author: None,
                accepted: None,
                attempts: Vec::new(),
                accepted_attempt: None,
                of_attempt: None,
                node: String::new(),
                local: board::Local::default(),
            },
        );
        let span = |from, to| board::Span { task: "t1".into(), by: "x".into(), node: String::new(), from_ms: from, to_ms: to, end: None };
        b.spans = vec![span(1, Some(5)), span(5, None)];
        assert_eq!(max_holders(&b)["t1"], 1, "one after the other");
        b.spans.push(span(3, Some(4)));
        assert_eq!(max_holders(&b)["t1"], 2);
    }
}
