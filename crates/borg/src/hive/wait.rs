//! `borg hive wait`: an interactive drone's wait for work (phase g G2, docs/design/hive.md "Phase g").
//! The drone's model starts it in the background when its stop asks (interactive.rs `after_stop`); it
//! spends no token while it waits, and it exits once it holds a task for the drone, printing it: Claude
//! Code starts the session's next turn with that output (a background command's end), so work reaches
//! an idle session at once and the session never just quits.
//!
//! While no mesh daemon runs on the node, the waiter that holds the node lock does the node's goal work
//! (node.rs, phase g G4: beats, the roster of takers, finished, decline and stuck lines, the reaper, the
//! integration child), so a node with no mesh still integrates while its drones idle. It hands
//! nothing while the engineer's own turn runs in the session (interactive.rs `busy`), says a stuck goal
//! once and keeps waiting (phase g correction 17), and leaves a single goal only once it is finished for
//! good. A claim it hands counts once a hook event of the session takes it up; otherwise a watcher it
//! leaves behind (`takeup`) releases it after `TAKEUP` (correction 10).

use std::path::Path;
use std::time::{Duration, Instant};

use super::interactive;
use super::run::Ending;
use super::{Board, Ev, Event, Goal, Here, HiveSession, TaskId, TaskState, board, bound, fail, node, save, short, tasks_path};
use crate::{hook, learn, queen, stats};

/// What a waiting drone does next on one goal (`step`): what the waiter (`main`) and the scenario
/// harness's interactive drones (scenario.rs) both read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Next {
    /// A task is open to it: claim it (with no cap on its confirm, phase f F5).
    Claim,
    /// Nothing to take now, the engineer's turn runs, or the board settles: wait for a change.
    Wait,
    /// Tasks no drone may take (run.rs `Ending::Stuck`): said once, then the drone waits on.
    Stuck(Vec<(TaskId, String)>),
    /// The goal is finished for good: a drone of this goal alone is a drone no more.
    Finished,
}

/// What a drone does on a goal whose board says `end`, a task `open` to it, while the engineer's turn
/// is `busy` (phase g G2): never an end but a finish for good. A runner's drone ends on a stuck, a budget
/// or a fault (run.rs `step`); an interactive drone is the engineer's session, which a stuck clears from
/// (a join, an `open --again`) and which spends what its harness spends, so it waits on.
pub(super) fn step(end: &Ending, open: bool, busy: bool) -> Next {
    match end {
        Ending::Finished => Next::Finished,
        _ if busy => Next::Wait,
        Ending::Settling => Next::Wait,
        _ if open => Next::Claim,
        Ending::Stuck(s) => Next::Stuck(s.clone()),
        Ending::Open => Next::Wait,
    }
}

/// The `.ctx` marker of the stuck tasks a drone was told of (`goal t1,t2`): each set once.
const STUCK_MARKER: &str = "#borg-hive-stuck ";

/// The signal that stops the waiter (SIGTERM from `borg hive leave` or Claude Code's task stop, SIGINT,
/// SIGHUP); 0 while none came.
static STOP: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

extern "C" fn on_stop(sig: libc::c_int) {
    STOP.store(sig, std::sync::atomic::Ordering::SeqCst);
}

fn trap() {
    let handler = on_stop as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGHUP, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

/// How often the waiter looks at the boards' files and a stop signal.
const TICK: Duration = Duration::from_millis(25);
/// How long it waits on unchanged boards before it reads the busy flag, the registry and the beat again.
const LOOK: Duration = Duration::from_millis(500);

/// `borg hive wait [--session S]`: waits for a task for the interactive drone of the session (the
/// harness's, `interactive::env_session`, or `S`), then prints it and exits 0. Exit 0 also once the
/// drone is a drone no more (it left, its single goal finished) or a stuck is said; 1 when the session is
/// no interactive drone; 128 + the signal when it was stopped. One waiter a drone: a second exits at once.
pub(super) fn main(here: &Here, named: Option<&str>) -> i32 {
    let Some(session) = named.map(String::from).or_else(interactive::env_session) else {
        return fail("run `borg hive wait` inside the drone's session (or give --session)");
    };
    let Some(hs) = bound(&session).filter(|hs| hs.interactive) else {
        return fail(format!("{} is no interactive drone (`borg hive join` in it first)", short(&session)));
    };
    let drone = hs.drone.clone();
    let path = interactive::waiter_path(&drone);
    let lock = match std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path) {
        Ok(f) => f,
        Err(e) => return fail(format!("{}: {e}", path.display())),
    };
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            println!("[borg hive] drone {drone}'s wait for work runs already: end your turn");
            return 0;
        }
        Err(std::fs::TryLockError::Error(e)) => return fail(format!("{}: {e}", path.display())),
    }
    let _ = lock.set_len(0);
    let _ = std::io::Write::write_all(&mut &lock, std::process::id().to_string().as_bytes());
    // The stop's ask is answered: once this waiter ends (a stuck said, a task handed), a stop may ask again.
    let mut hs = hs;
    hs.asked_wait = false;
    save(&session, &hs);
    trap();
    let code = waiting(here, &drone);
    drop(lock);
    code
}

/// The waiter's loop (`main`), for `drone`, holding its lock.
fn waiting(here: &Here, drone: &str) -> i32 {
    let here_node = node(&here.home);
    // The node's goal work (phase g G4, node.rs): its mesh daemon's, or this waiter's while it holds the
    // node lock (no daemon runs, no other waiter took it), until it exits.
    let mut worker: Option<(std::fs::File, super::node::Worker)> = None;
    loop {
        let sig = STOP.load(std::sync::atomic::Ordering::SeqCst);
        if sig != 0 {
            if sig != libc::SIGINT {
                stopped(here, drone);
            }
            return 128 + sig;
        }
        // The drone's session now: a `/clear` moved it (interactive.rs `rebind`).
        let Some(entry) = interactive::entry_of(&here.home, drone) else {
            println!("[borg hive] drone {drone} left the hive: nothing more to wait for");
            return 0;
        };
        let session = entry.session.clone();
        let Some(mut hs) = bound(&session).filter(|hs| hs.interactive) else {
            println!("[borg hive] drone {drone} left the hive: nothing more to wait for");
            return 0;
        };
        // Its session is over without a SessionEnd that said so (a terminal closed, Claude Code's "Move
        // to background and exit", a crash): no orphan waiter, no claim or membership left behind.
        if interactive::harness_gone(&entry) {
            let code = interactive_leave(here, &session, &hs);
            println!("[borg hive] drone {drone}'s session is gone: it left the hive");
            return code;
        }
        let cwd = if hs.worktree.is_empty() { here.cwd.clone() } else { hs.worktree.clone() };
        let repo = queen::repo_name(&cwd);
        let goals = interactive::served(&here.home, &hs, &repo);
        if worker.is_none() {
            worker = super::node::take(&here.home).map(|l| (l, super::node::Worker::for_waiter(drone)));
        }
        if let Some((_, w)) = worker.as_mut() {
            w.tick(&here.home);
        }
        let now = learn::now_ms();
        let busy = interactive::busy(&session, now);
        // A claim the stop's capped confirm left not final: waited for with no cap, then handed once the
        // engineer's turn is over.
        if let Some(code) = pending(here, &mut hs, &session, &cwd, busy) {
            return code;
        }
        for g in &goals {
            let b = fold(&here.home, &g.id);
            let lacks = super::lacking_here(&here.home, &g.id, &b, Path::new(&cwd), now);
            let end = Ending::of(&b, &here_node, now, allowed(&here.home, g, &repo), &lacks);
            let open = super::next_open(&here.home, g, &b, &session, drone, &repo, &cwd, now).is_some();
            match step(&end, open, busy) {
                Next::Finished if !hs.any => {
                    let code = interactive_leave(here, &session, &hs);
                    println!("[borg hive {}] the goal is finished: drone {drone} is a plain session again", g.id);
                    return code;
                }
                Next::Finished | Next::Wait => {}
                Next::Stuck(stuck) => {
                    let key = format!("{STUCK_MARKER}{} {}", g.id, stuck.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join(","));
                    let state = std::fs::read_to_string(hook::state_file(&session)).unwrap_or_default();
                    if !state.lines().any(|l| l == key) {
                        hook::append(&session, &key);
                        let why: Vec<String> = stuck.iter().map(|(t, w)| format!("{t}: {w}")).collect();
                        println!(
                            "[borg hive {}] stuck: no drone may take {}. You stay a drone: run the wait again (in the background) and end your turn; \
a new drone, an approval or `borg hive open {} --id <task> --again` clears it.",
                            g.id,
                            why.join("; "),
                            g.id
                        );
                        return 0;
                    }
                }
                Next::Claim => {
                    if let Some(text) = take(here, g, &mut hs, &session, &cwd, &repo) {
                        println!("{text}");
                        return 0;
                    }
                }
            }
        }
        // Until a board of a served goal changes (its file), half a second passes (the busy flag, the next
        // beat) or a signal comes.
        let marks = || goals.iter().map(|g| super::run::mark(&tasks_path(&here.home, &g.id))).collect::<Vec<_>>();
        let (seen, t0) = (marks(), Instant::now());
        while t0.elapsed() < LOOK && STOP.load(std::sync::atomic::Ordering::SeqCst) == 0 && marks() == seen {
            std::thread::sleep(TICK);
            if let Some((_, w)) = worker.as_mut() {
                w.tick(&here.home);
            }
        }
    }
}

fn fold(home: &Path, goal: &str) -> Board {
    board::fold_on(&board::read(&tasks_path(home, goal)), &node(home))
}

/// The tasks this node may claim in `repo` (run.rs `allowed`).
fn allowed<'a>(home: &Path, g: &'a Goal, repo: &'a str) -> impl Fn(&TaskState) -> bool + 'a {
    let approvals = super::oracle::Approvals::load(home, &g.id);
    move |s: &TaskState| super::may_claim(g, &s.task, repo) && approvals.pending(g, &s.task).is_empty()
}

/// The next open task of `g` claimed for the drone of `session` in `cwd` (the node joined first, mod.rs
/// `claim` calls interactive.rs `ensure_member`), the claim final with no cap (mod.rs `claim`, phase f F5): what the
/// drone is told (`deliver`). A task whose deps cannot come in is passed over, as the stop's handover
/// does; None when none was won.
fn take(here: &Here, g: &Goal, hs: &mut HiveSession, session: &str, cwd: &str, repo: &str) -> Option<String> {
    for _ in 0..super::CLAIM_TRIES {
        let now = learn::now_ms();
        let b = fold(&here.home, &g.id);
        let next = super::next_open(&here.home, g, &b, session, &hs.drone, repo, cwd, now)?;
        match super::claim(&here.home, g, session, &next.task.clone(), true, cwd, now, super::verdict::SNAPSHOT_TIMEOUT, None) {
            Ok(won) => return Some(deliver(here, g, hs, session, cwd, &won)),
            Err(super::Refused::Deps(_)) => super::refuse(session, &next.task.id),
            Err(super::Refused::Lost) => {}
            Err(_) => return None,
        }
    }
    None
}

/// A claim the stop made and never showed (its capped confirm ran out, mod.rs `hand_next`,
/// interactive.rs `unshown`): waited for until final (mod.rs `across`, no cap), then handed, never while
/// the engineer's own turn runs (`busy` before the wait or after it, interactive.rs `busy`): the waiter
/// waits on and hands it once the turn is over. Some(exit code) when the waiter is done (also when the
/// drone holds a claim it was shown: it works, it does not wait); None when it holds no claim, lost the
/// one it waited for, or the engineer's turn runs.
fn pending(here: &Here, hs: &mut HiveSession, session: &str, cwd: &str, busy: bool) -> Option<i32> {
    let (held, shown) = interactive::unshown(&here.home, hs, session, learn::now_ms())?;
    if shown {
        println!("[borg hive {}] you hold {}: finish it first (ending your turn runs its checks)", hs.goal, held.task.id);
        return Some(0);
    }
    if busy {
        return None;
    }
    let g = super::goal::load(&here.home, &hs.goal).ok()?;
    let at = held.live(learn::now_ms()).map_or(0, |l| l.2);
    let b = fold(&here.home, &g.id);
    let won = match super::across(&here.home, &g.id, &held.task.id, session, at, learn::now_ms(), &b, None) {
        Some((again, _)) => again?,
        None => held,
    };
    // The engineer typed while it waited: the claim is final now, handed once the turn is over.
    if interactive::busy(session, learn::now_ms()) {
        return None;
    }
    println!("{}", deliver(here, &g, hs, session, cwd, &won));
    Some(0)
}

/// The drone holds `won` of `g`: its goal is that one now (a drone of every open goal moves to it), the
/// claim is marked shown (`TASK_MARKER`), handed until a hook event takes it up (`interactive::handed`,
/// with the watcher that releases it otherwise, `takeup`), and what the drone is told: the claim in full,
/// where to work, and the plan's risk.
fn deliver(here: &Here, g: &Goal, hs: &mut HiveSession, session: &str, cwd: &str, won: &TaskState) -> String {
    let now = learn::now_ms();
    let state = std::fs::read_to_string(hook::state_file(session)).unwrap_or_default();
    let first = super::last_marked(&state, super::TASK_MARKER).is_none();
    let key = format!("{}@{}", won.task.id, won.live(now).map_or(0, |l| l.2));
    hook::append(session, &format!("{}{key}", super::TASK_MARKER));
    if hs.goal != g.id {
        hs.goal = g.id.clone();
        hs.cursor = std::fs::metadata(tasks_path(&here.home, &g.id)).map_or(0, |m| m.len());
    }
    hs.asked_wait = false;
    save(session, hs);
    interactive::handed(session, &g.id, &key, now);
    spawn_takeup(here, &g.id, &key, session);
    super::count(&here.home, stats::HIVE_CLAIM, session, &g.id, &won.task.id, &queen::repo_name(cwd));
    let risk = super::plan::escalate(&here.home, g, &won.task, session, cwd, now).map(|t| format!("\n{t}")).unwrap_or_default();
    let intro = if first { format!("{}\n", super::inoculation(g)) } else { String::new() };
    let tree = if hs.worktree.is_empty() { String::new() } else { format!("\nWork in your worktree only: cd {}", queen::sh_quote(&hs.worktree)) };
    format!("[borg hive {}] Work arrived. {intro}{}{risk}{tree}", g.id, super::describe(&here.home, g, won, session, cwd))
}

/// Starts the watcher of a handed claim (`takeup`), detached from the waiter, which exits now.
fn spawn_takeup(here: &Here, goal: &str, key: &str, session: &str) {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else { return };
    let mut c = std::process::Command::new(exe);
    c.args(["hive", "takeup", goal, key, "--session", session, "--dir", &here.cwd])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: setsid(2) between fork and exec only makes the child its own session, so the end of the
    // waiter's process group (the harness's background task) leaves it running.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let _ = c.spawn();
}

/// `borg hive takeup <goal> <task@claim> --session S` (the waiter's watcher, `spawn_takeup`): once the
/// claim waited `TAKEUP` from when it was handed (`interactive::untaken`; never from its stamp, which a
/// claim the stop made and the waiter handed later is long past) and no hook event of the session took
/// it up (its take-up file still names it), it is released, under the lock while the session still
/// holds it (`write_if`), so a claim no turn ever reached holds its task no longer than that (phase g
/// correction 10). It ends at once when a hook takes the claim up.
pub(super) fn takeup(here: &Here, goal: &str, key: &str, session: &str) -> i32 {
    let Some((task, _)) = key.rsplit_once('@') else { return 2 };
    let named = || interactive::untaken(session).filter(|(g, k, _)| g == goal && k == key);
    let Some((_, _, handed)) = named() else { return 0 };
    let from = if handed > 0 { handed } else { learn::now_ms() };
    let until = from.saturating_add(interactive::takeup_wait().as_millis() as i64);
    while learn::now_ms() < until {
        if named().is_none() {
            return 0;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if named().is_none() {
        return 0;
    }
    let here_node = node(&here.home);
    let note = "not taken up: no hook event of the session followed its wake";
    let released = super::write_if(&here.home, goal, |events| {
        let b = board::fold_on(events, &here_node);
        super::holding(&b, &here_node, task, session, learn::now_ms()).map(|_| vec![Event::new(Ev::Released, task, session, learn::now_ms()).with_note(note)])
    });
    let _ = std::fs::remove_file(hook::state_file(session).with_extension("hive-takeup"));
    i32::from(!matches!(released, Ok(Some(_))))
}

/// How long a stopped waiter looks for its session's end (`stopped`).
const STOP_GRACE: Duration = Duration::from_secs(3);

/// The waiter got SIGTERM or SIGHUP: Claude Code stops its background tasks when the session exits
/// ("Exit and stop tasks", Ctrl-C or Ctrl-D twice, `/exit`), a closed terminal sends SIGHUP, the engineer
/// stops it from `/tasks`, or `borg hive leave` stops it (SIGTERM too: the leave unbinds the session and
/// writes its registry leave within the grace, so the waiter finds the drone gone and only stops). SIGINT,
/// a Ctrl-C at a shell the engineer ran it in, only stops it. When its session is ending (its harness process gone
/// within `STOP_GRACE`) and nothing else made the drone leave (its SessionEnd, the leave), it leaves
/// (interactive.rs `leave_quietly`): its claim released, its registry leave, its node's leave with its
/// last drone. A session that lives on (the task stopped by hand) stays a drone, and its next stop asks
/// for a wait again.
fn stopped(here: &Here, drone: &str) {
    let t0 = Instant::now();
    while t0.elapsed() < STOP_GRACE {
        let Some(e) = interactive::entry_of(&here.home, drone) else { return };
        let Some(hs) = bound(&e.session).filter(|hs| hs.interactive) else { return };
        if e.pid == 0 {
            return;
        }
        if interactive::harness_gone(&e) {
            let _ = interactive::leave_quietly(here, &e.session, &hs);
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The drone's leave once its single goal is finished for good (interactive.rs `leave_quietly`).
fn interactive_leave(here: &Here, session: &str, hs: &HiveSession) -> i32 {
    i32::from(interactive::leave_quietly(here, session, hs).is_err())
}
