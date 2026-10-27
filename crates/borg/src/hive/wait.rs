//! `borg hive wait`: an interactive drone's wait for work (phase g G2, docs/design/hive.md "Phase g").
//! The drone's model starts it in the background when its stop asks (interactive.rs `after_stop`); it
//! spends no token while it waits, and it exits once it holds a task for the drone, printing it: Claude
//! Code starts the session's next turn with that output (a background command's end), so work reaches
//! an idle session at once and the session never just quits.
//!
//! While no mesh daemon runs on the node, the waiter that holds the node lock does the node's goal work
//! (node.rs, phase g G4: beats, the roster of takers, finished, decline and stuck lines, the reaper, the
//! integration child), so a node with no mesh still integrates while its drones idle. It hands
//! nothing while the engineer's own turn runs in the session (interactive.rs `busy`) or a stop of it
//! does (interactive.rs `Stopping`: the stop's own handover decides), says a stuck goal
//! once and keeps waiting (phase g correction 17), and leaves a single goal only once it is finished for
//! good. A claim it hands counts once a hook event of the session takes it up; otherwise a watcher it
//! leaves behind (`takeup`) releases it after `TAKEUP` (correction 10).
//!
//! `--stream` (omp's and opencode's adapters, interactive.rs `Wake::Plugin`, phase g correction 12):
//! the plugin starts the waiter when the session binds and keeps it for the session's life. It never
//! exits on a hand: each wake is one JSON line on stdout (`{"wake": text, "left": false}`), which the
//! plugin sends into the session by the harness's own path, and the waiter waits on, handing nothing
//! while the drone holds a task it was shown. It exits once the drone is a drone no more (`left: true`
//! on its last line), on a signal, or when its session is gone.
//!
//! `--queue` (codex, interactive.rs `Wake::Queue`): no codex hook can wake an idle session, so its
//! stop starts the waiter detached (interactive.rs `spawn_waiter`) when none runs, and the waiter lives
//! as a plugin's does, but each wake is `codex queue --thread <session> --message <text>` (`queue`),
//! codex's own path to an idle session. A queue that fails (nonzero exit, no `codex`, no answer within
//! `QUEUE_TIMEOUT`) releases the claim it carried at once, and the waiter claims nothing for
//! `QUEUE_RETRY`; one that codex took counts only once the turn it starts takes it up (`takeup`,
//! interactive.rs `touch`), as `codex queue` also takes a message for a thread no codex runs, and
//! queues it behind a turn the engineer started meanwhile.
//!
//! In every mode a claim is handed only while the engineer's turn is not running, looked at again once
//! the claim is won (its snapshot takes a while): a turn begun meanwhile keeps it final but unshown,
//! handed once the turn is over (`take`, `pending`). A wake no turn took up (`takeup` released it)
//! makes the waiter claim nothing more until a hook event of the session comes (interactive.rs
//! `missed`), so a thread no harness runs is not handed the same task again and again.

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

/// Who started the waiter, and so how it says a wake (`say`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The drone's model, in the background (Claude Code): it prints the wake and exits.
    Print,
    /// A harness plugin (`--stream`): one JSON line a wake, for the session's life.
    Stream,
    /// codex's stop (`--queue`): each wake through `codex queue`, for the session's life.
    Queue,
}

impl Mode {
    pub fn of(stream: bool, queue: bool) -> Self {
        match (stream, queue) {
            (true, _) => Mode::Stream,
            (false, true) => Mode::Queue,
            (false, false) => Mode::Print,
        }
    }

    /// It waits on after a wake (a plugin's, codex's), where a model's exits with it.
    fn lives(self) -> bool {
        self != Mode::Print
    }
}

/// The longest one `codex queue` may take before the wake counts as failed (it is killed).
const QUEUE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a waiter whose `codex queue` failed claims nothing more: a codex that cannot take the
/// message now is not handed a task again at once, and the task is open to other drones meanwhile.
const QUEUE_RETRY: Duration = Duration::from_secs(10);

/// How often the waiter looks at the boards' files and a stop signal.
const TICK: Duration = Duration::from_millis(25);
/// How long it waits on unchanged boards before it reads the busy flag, the registry and the beat again.
const LOOK: Duration = Duration::from_millis(500);

/// `borg hive wait [--session S] [--stream]`: waits for a task for the interactive drone of the session
/// (the harness's, `interactive::env_session`, or `S`), then prints it and exits 0. Exit 0 also once the
/// drone is a drone no more (it left, its single goal finished) or a stuck is said; 1 when the session is
/// no interactive drone; 128 + the signal when it was stopped. One waiter a drone: a second exits at once.
/// `mode`: who started it, and so how it says a wake and whether it waits on (module doc).
pub(super) fn main(here: &Here, named: Option<&str>, mode: Mode) -> i32 {
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
            match mode {
                Mode::Stream => println!("{}", serde_json::json!({"note": format!("[borg hive] drone {drone}'s wait for work runs already")})),
                Mode::Queue => println!("[borg hive] drone {drone}'s wait for work runs already"),
                Mode::Print => println!("[borg hive] drone {drone}'s wait for work runs already: end your turn"),
            }
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
    let code = waiting(here, &drone, mode);
    drop(lock);
    code
}

/// What the waiter tells `session`: the text, as it is (a background command's output, Claude Code),
/// as one JSON line a plugin wakes the session with (`Stream`), `left` once the drone is one no more, or
/// through `codex queue` (`Queue`, also written to the waiter's log). Whether the session was reached
/// (Err: why `codex queue` failed).
fn say(mode: Mode, session: &str, text: &str, left: bool) -> Result<(), String> {
    match mode {
        Mode::Stream => println!("{}", serde_json::json!({"wake": text, "left": left})),
        Mode::Print => println!("{text}"),
        Mode::Queue => {
            let sent = queue(session, text);
            let how = sent.as_ref().map_or_else(|e| format!("not queued ({e})"), |()| "queued".to_string());
            println!("{} {how}: {}", learn::now_ms(), text.lines().next().unwrap_or_default());
            return sent;
        }
    }
    Ok(())
}

/// `codex queue --thread <session> --message <text>` (codex 0.149+): the message waits in codex's queue
/// for the thread and starts a turn of an idle session. Ok once codex took it (exit 0); Err with the
/// exit, a run past `QUEUE_TIMEOUT` (killed), or no `codex` to run.
fn queue(session: &str, text: &str) -> Result<(), String> {
    let mut c = std::process::Command::new("codex");
    c.args(["queue", "--thread", session, "--message", text]);
    match crate::runner::run_bounded(c, None, QUEUE_TIMEOUT) {
        Some(b) => match b.status {
            Some(s) if s.success() => Ok(()),
            Some(s) => Err(format!("`codex queue` exited {}", s.code().map_or_else(|| "on a signal".to_string(), |c| c.to_string()))),
            None => Err(format!("`codex queue` did not finish within {} s", QUEUE_TIMEOUT.as_secs())),
        },
        None => Err("`codex` could not be started".into()),
    }
}

/// A wake through `codex queue` failed (`why`): the claim it carried (the session's handed one,
/// interactive.rs `untaken`) is released under the lock while the session still holds it, and its
/// take-up ends, so the task is open again at once, not `TAKEUP` later.
fn unqueued(here: &Here, session: &str, why: &str) {
    let Some((goal, key, _)) = interactive::untaken(session) else { return };
    let Some((task, _)) = key.rsplit_once('@') else { return };
    let here_node = node(&here.home);
    let note = format!("not delivered: {why}");
    let _ = super::write_if(&here.home, &goal, |events| {
        let b = board::fold_on(events, &here_node);
        super::holding(&b, &here_node, task, session, learn::now_ms()).map(|_| vec![Event::new(Ev::Released, task, session, learn::now_ms()).with_note(&note)])
    });
    let _ = std::fs::remove_file(hook::state_file(session).with_extension("hive-takeup"));
}

/// The waiter's loop (`main`), for `drone`, holding its lock; `mode`: who started it (module doc).
fn waiting(here: &Here, drone: &str, mode: Mode) -> i32 {
    let here_node = node(&here.home);
    // The node's goal work (phase g G4, node.rs): its mesh daemon's, or this waiter's while it holds the
    // node lock (no daemon runs, no other waiter took it), until it exits.
    let mut worker: Option<(std::fs::File, super::node::Worker)> = None;
    // A wake `codex queue` did not take: nothing claimed or handed before this (`QUEUE_RETRY`).
    let mut retry_at: Option<Instant> = None;
    // A missed wake (interactive.rs `missed`) was said in the log: once a miss.
    let mut told_missed = false;
    loop {
        let sig = STOP.load(std::sync::atomic::Ordering::SeqCst);
        if sig != 0 {
            if sig != libc::SIGINT {
                stopped(here, drone);
            }
            return 128 + sig;
        }
        // The drone's session now: a `/clear` moved it (interactive.rs `rebind`).
        // Left: said to a waiter whose model started it; a plugin's says nothing (its session's end, a
        // leave the model ran, made it so) but that it ends; codex's only logs it (the session left).
        let left = || {
            match mode {
                Mode::Stream => println!("{}", serde_json::json!({"left": true})),
                Mode::Print | Mode::Queue => println!("[borg hive] drone {drone} left the hive: nothing more to wait for"),
            }
            0
        };
        let Some(entry) = interactive::entry_of(&here.home, drone) else { return left() };
        let session = entry.session.clone();
        let Some(mut hs) = bound(&session).filter(|hs| hs.interactive) else { return left() };
        // Its session is over without a SessionEnd that said so (a terminal closed, Claude Code's "Move
        // to background and exit", a crash): no orphan waiter, no claim or membership left behind.
        if interactive::harness_gone(&entry) {
            let code = interactive_leave(here, &session, &hs);
            let text = format!("[borg hive] drone {drone}'s session is gone: it left the hive");
            match mode {
                // No codex runs the thread any more: nothing to queue it for.
                Mode::Queue => println!("{text}"),
                _ => {
                    let _ = say(mode, &session, &text, true);
                }
            }
            return code;
        }
        let cwd = if hs.worktree.is_empty() { here.cwd.clone() } else { hs.worktree.clone() };
        let repo = queen::repo_name(&cwd);
        let goals = interactive::served(&here.home, &hs, &repo);
        if worker.is_none() {
            worker = super::node::take_as(&here.home, &format!("waiter {drone}")).map(|l| (l, super::node::Worker::for_waiter(drone)));
        }
        if let Some((_, w)) = worker.as_mut() {
            w.tick(&here.home);
        }
        let now = learn::now_ms();
        // The engineer's turn, or a stop of the session (its own handover decides), runs: nothing handed;
        // nor while a failed `codex queue` waits out its retry, nor after a wake no turn took up until a
        // hook event of the session comes.
        let missed = interactive::missed(&session);
        if missed && !told_missed {
            println!("[borg hive] drone {drone}: its last wake was not taken up: it claims nothing until its session's next hook event");
        }
        told_missed = missed;
        let busy = interactive::held_off(&session, now) || retry_at.is_some_and(|t| Instant::now() < t) || missed;
        // A wake that did not reach the session releases its claim and waits before the next.
        let mut failed = |why: String| {
            println!("[borg hive] drone {drone}: {why}: its claim is released");
            unqueued(here, &session, &why);
            retry_at = Some(Instant::now() + QUEUE_RETRY);
        };
        // A claim the stop's capped confirm left not final: waited for with no cap, then handed once the
        // engineer's turn is over.
        let held = match pending(here, &mut hs, &session, &cwd, busy) {
            Held::Holds(task) if !mode.lives() => {
                println!("[borg hive {}] you hold {task}: finish it first (ending your turn runs its checks)", hs.goal);
                return 0;
            }
            Held::Handed(text) => {
                if let Err(why) = say(mode, &session, &text, false) {
                    failed(why);
                }
                if !mode.lives() {
                    return 0;
                }
                continue;
            }
            Held::Holds(_) => true,
            Held::Free => false,
        };
        for g in goals.iter().filter(|_| !held) {
            let b = fold(&here.home, &g.id);
            let lacks = super::lacking_here(&here.home, &g.id, &b, Path::new(&cwd), now);
            let end = Ending::of(&b, &here_node, now, allowed(&here.home, g, &repo), &lacks);
            let open = super::next_open(&here.home, g, &b, &session, drone, &repo, &cwd, now).is_some();
            match step(&end, open, busy) {
                Next::Finished if !hs.any => {
                    let code = interactive_leave(here, &session, &hs);
                    let _ = say(mode, &session, &format!("[borg hive {}] the goal is finished: drone {drone} is a plain session again", g.id), true);
                    return code;
                }
                Next::Finished | Next::Wait => {}
                Next::Stuck(stuck) => {
                    let key = format!("{STUCK_MARKER}{} {}", g.id, stuck.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join(","));
                    let state = std::fs::read_to_string(hook::state_file(&session)).unwrap_or_default();
                    if !state.lines().any(|l| l == key) {
                        hook::append(&session, &key);
                        let why: Vec<String> = stuck.iter().map(|(t, w)| format!("{t}: {w}")).collect();
                        let next = match mode.lives() {
                            true => "end your turn: the work comes by itself",
                            false => "run the wait again (in the background) and end your turn",
                        };
                        let _ = say(
                            mode,
                            &session,
                            &format!(
                                "[borg hive {}] stuck: no drone may take {}. You stay a drone: {next}; a new drone, an approval or `borg hive open {} --id <task> --again` clears it.",
                                g.id,
                                why.join("; "),
                                g.id
                            ),
                            false,
                        );
                        if !mode.lives() {
                            return 0;
                        }
                    }
                }
                Next::Claim => match take(here, g, &mut hs, &session, &cwd, &repo) {
                    Took::Handed(text) => {
                        if let Err(why) = say(mode, &session, &text, false) {
                            failed(why);
                        }
                        if !mode.lives() {
                            return 0;
                        }
                        // A waiter that lives on hands one task and waits on: the drone holds it now.
                        break;
                    }
                    // Won while the engineer's turn began: `pending` hands it once the turn is over.
                    Took::Kept => break,
                    Took::Nothing => {}
                },
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

/// What `take` won.
enum Took {
    /// A claim, handed: this text tells the drone.
    Handed(String),
    /// A claim won while the engineer's turn began (or a stop of the session): final, never shown, so
    /// the turn's prompt and stop leave it alone (mod.rs `Ctx::stop`); `pending` hands it once the turn
    /// is over.
    Kept,
    /// No claim.
    Nothing,
}

/// The next open task of `g` claimed for the drone of `session` in `cwd` (the node joined first, mod.rs
/// `claim` calls interactive.rs `ensure_member`), the claim final with no cap (mod.rs `claim`, phase f F5): what the
/// drone is told (`deliver`). The engineer's turn is looked at again once the claim is won (its
/// snapshot takes a while): one begun meanwhile gets nothing handed (`Took::Kept`), so no wake lands in
/// it or queues behind it. A task whose deps cannot come in is passed over, as the stop's handover
/// does.
fn take(here: &Here, g: &Goal, hs: &mut HiveSession, session: &str, cwd: &str, repo: &str) -> Took {
    for _ in 0..super::CLAIM_TRIES {
        let now = learn::now_ms();
        let b = fold(&here.home, &g.id);
        let Some(next) = super::next_open(&here.home, g, &b, session, &hs.drone, repo, cwd, now) else { return Took::Nothing };
        match super::claim(&here.home, g, session, &next.task.clone(), true, cwd, now, super::verdict::SNAPSHOT_TIMEOUT, None) {
            Ok(_) if interactive::held_off(session, learn::now_ms()) => return Took::Kept,
            Ok(won) => return Took::Handed(deliver(here, g, hs, session, cwd, &won)),
            Err(super::Refused::Deps(_)) => super::refuse(session, &next.task.id),
            Err(super::Refused::Lost) => {}
            Err(_) => return Took::Nothing,
        }
    }
    Took::Nothing
}

/// What the drone's claim leaves its waiter to do (`pending`).
enum Held {
    /// It holds no claim, lost the one it waited for, or the engineer's turn runs: look at the goals.
    Free,
    /// The claim the stop made is final and handed: this text tells the drone.
    Handed(String),
    /// It holds this claim (its task id), shown to it: it works and does not wait. A waiter its model
    /// started says so and exits; a plugin's waits on, offering nothing more until the claim ends.
    Holds(String),
}

/// A claim the stop made and never showed (its capped confirm ran out, mod.rs `hand_next`,
/// interactive.rs `unshown`): waited for until final (mod.rs `across`, no cap), then handed, never while
/// the engineer's own turn runs (`busy` before the wait or after it, interactive.rs `busy`): the waiter
/// waits on and hands it once the turn is over. `Held::Holds` when the drone holds a claim it was shown
/// (it works, it does not wait).
fn pending(here: &Here, hs: &mut HiveSession, session: &str, cwd: &str, busy: bool) -> Held {
    let Some((held, shown)) = interactive::unshown(&here.home, hs, session, learn::now_ms()) else { return Held::Free };
    if shown {
        return Held::Holds(held.task.id.clone());
    }
    if busy {
        return Held::Free;
    }
    let Ok(g) = super::goal::load(&here.home, &hs.goal) else { return Held::Free };
    let at = held.live(learn::now_ms()).map_or(0, |l| l.2);
    let b = fold(&here.home, &g.id);
    let won = match super::across(&here.home, &g.id, &held.task.id, session, at, learn::now_ms(), &b, None) {
        Some((Some(again), _)) => again,
        Some((None, _)) => return Held::Free,
        None => held,
    };
    // The engineer typed (or a stop began) while it waited: the claim is final now, handed once the
    // turn is over.
    if interactive::held_off(session, learn::now_ms()) {
        return Held::Free;
    }
    Held::Handed(deliver(here, &g, hs, session, cwd, &won))
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
    format!("{} {intro}{}{risk}{tree}", interactive::arrived(&g.id), super::describe(&here.home, g, won, session, cwd))
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
/// correction 10). Never while a turn of the session may still run (interactive.rs `turn_may_run`): a
/// codex wake waits in codex's queue behind it. A release marks the wake missed (interactive.rs
/// `mark_missed`): the waiter claims nothing more until a hook event of the session comes. It ends at
/// once when a hook takes the claim up.
pub(super) fn takeup(here: &Here, goal: &str, key: &str, session: &str) -> i32 {
    let Some((task, _)) = key.rsplit_once('@') else { return 2 };
    let named = || interactive::untaken(session).filter(|(g, k, _)| g == goal && k == key);
    let Some((_, _, handed)) = named() else { return 0 };
    let from = if handed > 0 { handed } else { learn::now_ms() };
    let until = from.saturating_add(interactive::takeup_wait().as_millis() as i64);
    while learn::now_ms() < until || interactive::turn_may_run(session, learn::now_ms()) {
        if named().is_none() {
            return 0;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if named().is_none() {
        return 0;
    }
    let here_node = node(&here.home);
    let note = interactive::NOT_TAKEN_UP;
    let released = super::write_if(&here.home, goal, |events| {
        let b = board::fold_on(events, &here_node);
        super::holding(&b, &here_node, task, session, learn::now_ms()).map(|_| vec![Event::new(Ev::Released, task, session, learn::now_ms()).with_note(note)])
    });
    let _ = std::fs::remove_file(hook::state_file(session).with_extension("hive-takeup"));
    if !matches!(released, Ok(Some(_))) {
        return 1;
    }
    interactive::mark_missed(session, learn::now_ms());
    0
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
    // Stamped now, not at the waiter's start: a streaming waiter (omp, opencode, codex) lives for hours.
    let here = Here { home: here.home.clone(), cwd: here.cwd.clone(), now: learn::now_ms() };
    i32::from(interactive::leave_quietly(&here, session, hs).is_err())
}
