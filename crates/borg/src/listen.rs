//! `borg listen`: a session that listens is woken when a message for it (msg.rs) arrives while it is
//! idle. Run inside a session, it opts that session in (`<store>/listen/<session>.json`, never synced)
//! until `borg listen --off`, the session's end, or its presence record's prune (presence.rs `prune`).
//! A plain session is never woken.
//!
//! A wake only says that messages wait: the hook shows them (msg.rs `deliver`, its shown-once claim). A
//! message is not exclusive, so a wake the session never took leaves it unread, and the next message
//! wakes it again. Each message wakes a session once (`<session>.woke`).
//!
//! How an idle session is woken follows its harness (`Caps::drone_wake`, `after_stop`):
//! - Claude Code (`Model`): a stop with no waiter asks the model once to start `borg listen --wait` in
//!   the background. It exits printing a summary once a message waits and the engineer's turn is not
//!   running, and Claude Code starts the next turn with that output.
//! - omp, opencode (`Plugin`): the stop's answer carries `"listen": true` while no waiter runs, and the
//!   adapter starts `borg listen --stream --session <id>`: one JSON line per wake (`{"wake": text}`),
//!   for the session's life, which it sends in by the harness's own path.
//! - codex (`Queue`): the stop starts `borg listen --queue` detached when none runs. Each wake is
//!   `codex queue --thread <session> --message <text>`, bounded by `QUEUE_TIMEOUT`, and a failed one
//!   backs off (`backoff`) before the same messages are tried again.
//!
//! The engineer's turn holds every wake off: `<session>.busy` from its prompt to its stop, and
//! `<session>.stopping` while the stop runs (`Stopping`). A turn that never reached its stop (an Esc)
//! clears by itself: `IDLE_MS` after its last event, or `TOOL_MS` after a tool call that never ended.
//! codex's `Interrupt` and an adapter's `halted` clear it at once. Meanwhile the turn's own hooks show
//! what arrives.
//!
//! One waiter per session (`<session>.lock`, flock, its pid inside). It stats the log every `TICK`,
//! reads it only when its length or change time moved, looks at the session every `LOOK`, follows a
//! reinstall of borg in place (reexec.rs, same pid, its lock passed on), and exits on SIGINT, SIGHUP or
//! SIGTERM, on `--off`, or once the session is gone.

use std::collections::HashSet;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::harness::{self, DroneWake, Harness};
use crate::msg::{self, Message};
use crate::presence::{self, safe};
use crate::{collective, learn, queen, reexec, top};

/// The opt-in records and the waiters' files, in the collective's store.
pub const DIR: &str = "listen";
/// Without an event for this long and no tool call running, the engineer's turn no longer holds a wake
/// off (an Esc with no stop). Claude Code's and an adapter's wake would land in a turn that only went
/// quiet (a long generation), so they wait long; codex's queues behind a running turn anyway.
pub const IDLE_MS: i64 = 5 * 60_000;
const QUEUE_IDLE_MS: i64 = 60_000;
/// A tool call that started this long ago and never ended no longer holds a wake off.
pub const TOOL_MS: i64 = 10 * 60_000;
/// How long a stop's mark lasts should its hook be killed first (longer than any stop timeout).
const STOP_MS: i64 = 120_000;
/// How often the waiter stats the log and looks for a stop signal.
const TICK: Duration = Duration::from_millis(250);
/// How often it looks at the session (its record, its turn, its harness process) with the log unchanged.
const LOOK: Duration = Duration::from_secs(2);
/// The longest one `codex queue` may take.
const QUEUE_TIMEOUT: Duration = Duration::from_secs(30);
/// The first and the longest wait after a failed wake.
const RETRY_MIN: Duration = Duration::from_secs(10);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// A detached waiter's log is cut once it is past this size.
const LOG_MAX: u64 = 64 * 1024;
/// How many senders a wake names.
const NAMES: usize = 3;
/// A lock busy at a waiter's start may be a probe's (`waiter_runs`, an instant): tried this many more
/// times, this far apart, before another waiter holds it.
const LOCK_TRIES: u32 = 10;
const LOCK_RETRY: Duration = Duration::from_millis(5);

pub const USAGE: &str = "usage: borg listen [--off] [--session S] [--dir D]
       borg listen --wait|--stream|--queue [--session S] [--dir D]";
/// `borg listen`'s flags (completions.rs).
pub const FLAGS: &[&str] = &["--off", "--wait", "--stream", "--queue", "--session", "--dir"];

/// A session's opt-in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub session: String,
    pub harness: String,
    /// When it opted in.
    pub ms: i64,
    /// Its harness process and that process's start, when known (0: only its end event or `--off` ends
    /// its waiter).
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub pid_start: u64,
    /// A stop asked the model to start its wait, and no waiter or engineer's prompt came since (`Model`:
    /// asked once a turn).
    #[serde(default)]
    pub asked: bool,
}

/// What `borg top` shows of a session (`status`), its WAKE column and its JSON spelled alike (`name`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Status {
    /// Opted in, no waiter runs: the next stop starts or asks for one.
    #[serde(rename = "on")]
    Listens,
    /// Opted in, its waiter runs.
    #[serde(rename = "waiter")]
    Waits,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Listens => "on",
            Status::Waits => "waiter",
        }
    }
}

/// Who runs the waiter, and so how it says a wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The model, in the background (`--wait`): it prints the wake and exits.
    Print,
    /// A harness adapter (`--stream`): one JSON line per wake, for the session's life.
    Stream,
    /// codex's stop (`--queue`): each wake through `codex queue`, for the session's life.
    Queue,
}

impl Mode {
    /// It waits on after a wake, where the model's exits with it.
    fn lives(self) -> bool {
        self != Mode::Print
    }

    /// How long a quiet turn of its session holds a wake off (`IDLE_MS`).
    fn idle_ms(self) -> i64 {
        if self == Mode::Queue { QUEUE_IDLE_MS } else { IDLE_MS }
    }
}

/// Why a wake did not reach the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeError {
    /// `codex` could not be started.
    Spawn,
    /// `codex queue` exited with this code (None: on a signal).
    Exit(Option<i32>),
    /// `codex queue` ran past `QUEUE_TIMEOUT`.
    Timeout,
    /// The adapter's end of the stream is gone.
    Closed,
}

impl fmt::Display for WakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WakeError::Spawn => f.write_str("`codex` could not be started"),
            WakeError::Exit(Some(c)) => write!(f, "`codex queue` exited {c}"),
            WakeError::Exit(None) => f.write_str("`codex queue` ended on a signal"),
            WakeError::Timeout => write!(f, "`codex queue` did not finish within {} s", QUEUE_TIMEOUT.as_secs()),
            WakeError::Closed => f.write_str("the adapter closed the stream"),
        }
    }
}

impl std::error::Error for WakeError {}

// ---------------------------------------------------------------- the store

fn dir(home: &Path) -> PathBuf {
    home.join(DIR)
}

/// `<store>/listen/<safe session>.<ext>`.
fn file(home: &Path, session: &str, ext: &str) -> PathBuf {
    dir(home).join(format!("{}.{ext}", safe(session)))
}

/// Whether `session` listens: one stat, the whole cost of a hook of a plain session.
pub fn listening(home: &Path, session: &str) -> bool {
    !safe(session).is_empty() && file(home, session, "json").exists()
}

pub fn read(home: &Path, session: &str) -> Option<Record> {
    if safe(session).is_empty() {
        return None;
    }
    serde_json::from_slice(&std::fs::read(file(home, session, "json")).ok()?).ok()
}

/// `f` on `session`'s record as it is now, saved when `f` says it changed it and the session still
/// listens, so a `--off` meanwhile is not undone (but for the instant of the rename). Whether it saved.
fn amend(home: &Path, session: &str, f: impl FnOnce(&mut Record) -> bool) -> bool {
    let Some(mut r) = read(home, session) else { return false };
    f(&mut r) && listening(home, session) && save(home, &r).is_ok()
}

fn save(home: &Path, r: &Record) -> std::io::Result<()> {
    std::fs::create_dir_all(dir(home))?;
    let target = file(home, &r.session, "json");
    let tmp = dir(home).join(format!(".{}.{}.tmp", safe(&r.session), std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(r).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, &target).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// The session listens no more (`--off`, its end, its presence pruned): its record and turn marks go;
/// its lock and log too once no waiter holds the lock. A waiter that runs sees the record gone at its
/// next look and exits.
pub fn forget(home: &Path, session: &str) {
    if safe(session).is_empty() {
        return;
    }
    for ext in ["json", "busy", "stopping", "woke"] {
        let _ = std::fs::remove_file(file(home, session, ext));
    }
    if !waiter_runs(home, session) {
        let _ = std::fs::remove_file(file(home, session, "lock"));
        let _ = std::fs::remove_file(file(home, session, "log"));
    }
}

/// `session`'s status for `borg top`: None when it does not listen.
pub fn status(home: &Path, session: &str) -> Option<Status> {
    listening(home, session).then(|| if waiter_runs(home, session) { Status::Waits } else { Status::Listens })
}

/// Whether a waiter of `session` runs: its lock is held. The probe takes the lock for an instant when
/// none runs (`take_lock` tries again past it).
pub fn waiter_runs(home: &Path, session: &str) -> bool {
    let Ok(f) = std::fs::OpenOptions::new().write(true).open(file(home, session, "lock")) else { return false };
    matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

/// Stops `session`'s waiter, if one runs: SIGTERM to the pid its lock file names.
fn stop_waiter(home: &Path, session: &str) {
    if !waiter_runs(home, session) {
        return;
    }
    let pid = std::fs::read_to_string(file(home, session, "lock")).ok().and_then(|t| t.trim().parse::<i32>().ok()).unwrap_or(0);
    if pid > 0 && u32::try_from(pid).is_ok_and(|p| p != std::process::id()) {
        // SAFETY: kill(2) with SIGTERM on the pid the running waiter wrote into the file it holds locked.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
}

// ---------------------------------------------------------------- the engineer's turn

/// The engineer's turn as its marks say (`<session>.busy`: `<tool> <ms>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Turn {
    /// No turn, or its stop came.
    Over,
    /// The turn's last event (a prompt, a tool call's end) was at this time.
    Thinking(i64),
    /// A tool call started at this time and has not ended.
    Tool(i64),
}

impl Turn {
    fn parse(text: &str) -> Turn {
        let mut it = text.split_whitespace();
        match (it.next(), it.next().and_then(|m| m.parse::<i64>().ok())) {
            (Some("1"), Some(ms)) => Turn::Tool(ms),
            (Some(_), Some(ms)) => Turn::Thinking(ms),
            _ => Turn::Over,
        }
    }

    /// Whether it holds a wake off at `now`, quiet for at most `idle_ms`.
    fn running(self, now: i64, idle_ms: i64) -> bool {
        match self {
            Turn::Over => false,
            Turn::Thinking(ms) => now.saturating_sub(ms) <= idle_ms,
            Turn::Tool(ms) => now.saturating_sub(ms) <= TOOL_MS,
        }
    }
}

fn turn(home: &Path, session: &str) -> Turn {
    std::fs::read_to_string(file(home, session, "busy")).map_or(Turn::Over, |t| Turn::parse(&t))
}

fn set_turn(home: &Path, session: &str, tool: bool, now: i64) {
    let _ = std::fs::write(file(home, session, "busy"), format!("{} {now}", u8::from(tool)));
}

fn clear_turn(home: &Path, session: &str) {
    let _ = std::fs::remove_file(file(home, session, "busy"));
}

/// A stop of a listening session runs (hook.rs `main`): marked from its start until it returns (the
/// guard's drop), or until `STOP_MS` should the harness kill the hook first. No wake meanwhile. Its
/// presence is the stop's one stat: `after_stop` takes it and stats nothing more for a plain session.
#[derive(Debug)]
pub struct Stopping {
    mark: PathBuf,
    session: String,
}

impl Drop for Stopping {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.mark);
    }
}

/// The mark of `event` when it is the stop of a listening session; None for anything else, after at most
/// one stat. A mark that could not be written still says the session listens.
pub fn stopping(home: &Path, event: &str, session: &str, now: i64) -> Option<Stopping> {
    if event != "stop" || !listening(home, session) {
        return None;
    }
    let mark = file(home, session, "stopping");
    let _ = std::fs::write(&mark, now.saturating_add(STOP_MS).to_string());
    Some(Stopping { mark, session: session.to_owned() })
}

fn stop_runs(home: &Path, session: &str, now: i64) -> bool {
    std::fs::read_to_string(file(home, session, "stopping")).ok().and_then(|t| t.trim().parse::<i64>().ok()).is_some_and(|until| now <= until)
}

/// Whether a wake of `session` waits at `now`: the engineer's turn (quiet for at most `idle_ms`) or its
/// stop runs.
fn held_off(home: &Path, session: &str, now: i64, idle_ms: i64) -> bool {
    turn(home, session).running(now, idle_ms) || stop_runs(home, session, now)
}

/// Whether `session` (started at `started`, asked only when someone steers here) is paused: its stop is
/// its wait for the engineer (steer.rs `around`), and every tool call a wake asks for would be refused.
fn paused(home: &Path, session: &str, started: impl FnOnce() -> Option<i64>, now: i64) -> bool {
    crate::steer::active(home) && crate::steer::paused(home, session, started(), now)
}

/// The hook's bookkeeping of a listening session's turn at `event` (hook.rs `main`, every event but the
/// stop, `after_stop`): a prompt starts it (and lets its stop ask for the wait again), a tool call's
/// start and end move it on, a halted turn ends it, the session's end forgets the opt-in. One stat for
/// a session that does not listen.
pub fn touch(home: &Path, event: &str, session: &str, now: i64) {
    let tool = match event {
        "prompt" | "post_tool" | "post_tool_failure" => Some(false),
        "pre_tool" => Some(true),
        "halted" | "end" => None,
        _ => return,
    };
    if !listening(home, session) {
        return;
    }
    match (event, tool) {
        ("end", _) => forget(home, session),
        ("prompt", _) => {
            set_turn(home, session, false, now);
            amend(home, session, |r| std::mem::take(&mut r.asked));
        }
        (_, Some(tool)) => set_turn(home, session, tool, now),
        (_, None) => clear_turn(home, session),
    }
}

// ---------------------------------------------------------------- the stop

fn blocks(o: Option<&Value>) -> bool {
    o.is_some_and(|o| o.get("decision").and_then(Value::as_str) == Some("block"))
}

/// Whether the stop's input shows a `borg listen --wait` running in the background (Claude Code's
/// `background_tasks`).
fn wait_armed(input: &Value) -> bool {
    input
        .get("background_tasks")
        .and_then(Value::as_array)
        .is_some_and(|ts| ts.iter().any(|t| t.get("command").and_then(Value::as_str).is_some_and(|c| c.contains("listen --wait"))))
}

/// A listening session's stop, after everything else answered (`out`). A stop that ends the turn (no
/// block) ends the engineer's turn mark, and when no waiter runs: Claude Code's model is asked once to
/// start `borg listen --wait` in the background (as Stop feedback, never a hook error); an adapter is
/// told to start its stream (`"listen": true`); codex's waiter is started detached. A block passes as it
/// is: the turn goes on, and its next stop decides. A paused session's stop starts and asks for nothing:
/// it waits for the engineer. `stop` is `stopping`'s mark: None (a plain session,
/// or not a stop) returns `out` without a stat.
pub fn after_stop(home: &Path, h: &dyn Harness, stop: Option<&Stopping>, input: &Value, out: Option<Value>) -> Option<Value> {
    let Some(stop) = stop else { return out };
    let session = stop.session.as_str();
    let Some(rec) = read(home, session) else { return out };
    if blocks(out.as_ref()) {
        return out;
    }
    clear_turn(home, session);
    // A harness process the opt-in's shell could not name (codex's sandbox): its hook runs under it.
    if rec.pid == 0
        && let Some((pid, start)) = own_harness(h, input)
    {
        amend(home, session, |r| {
            (r.pid, r.pid_start) = (pid, start);
            true
        });
    }
    if paused(home, session, || presence::read(home, session).map(|p| p.started), learn::now_ms()) || waiter_runs(home, session) {
        return out;
    }
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
    match h.caps().drone_wake {
        DroneWake::Plugin => {
            let mut o = out.filter(Value::is_object).unwrap_or_else(|| json!({}));
            o["listen"] = Value::Bool(true);
            Some(o)
        }
        DroneWake::Queue => {
            let _ = spawn_queue(home, session, cwd);
            out
        }
        DroneWake::Model if wait_armed(input) => out,
        DroneWake::Model if amend(home, session, |r| !std::mem::replace(&mut r.asked, true)) => Some(ask(out, &arm_text(session, cwd))),
        DroneWake::Model => out,
    }
}

/// What a stop with no waiter asks Claude Code's model to run.
fn arm_text(session: &str, cwd: &str) -> String {
    format!(
        "[borg listen] This session listens for messages from other sessions. To be woken when one arrives, without spending tokens meanwhile, \
run `{}` with the Bash tool's run_in_background set to true, then end your turn. When it exits, its output says what arrived.",
        command("--wait", session, cwd)
    )
}

/// `borg listen <flag> --session S --dir D`, quoted for a shell.
fn command(flag: &str, session: &str, cwd: &str) -> String {
    let dir = if cwd.is_empty() { String::new() } else { format!(" --dir {}", queen::sh_quote(cwd)) };
    format!("borg listen {flag} --session {}{dir}", queen::sh_quote(session))
}

/// `text` as Claude Code's Stop feedback (`hookSpecificOutput.additionalContext`), after any context
/// `out` carried: the turn goes on with it, shown as hook feedback, not as "Stop hook error".
fn ask(out: Option<Value>, text: &str) -> Value {
    let mut o = out.filter(Value::is_object).unwrap_or_else(|| json!({}));
    let theirs = o.get("hookSpecificOutput").and_then(|h| h.get("additionalContext")).and_then(Value::as_str).filter(|t| !t.is_empty());
    let text = match theirs {
        Some(t) => format!("{t}\n\n{text}"),
        None => text.to_string(),
    };
    o["hookSpecificOutput"] = json!({"hookEventName": "Stop", "additionalContext": text});
    o
}

/// Starts codex's waiter of `session` (`--queue`), detached: a session of its own (setsid), stdin from
/// nothing, its output appended to `<session>.log`, the root store as its `BORG_HOME` (the hook set the
/// collective's own), so it routes `cwd` as a command does. Not awaited; one started meanwhile finds the
/// lock held and exits.
fn spawn_queue(home: &Path, session: &str, cwd: &str) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let path = file(home, session, "log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX) {
        let _ = std::fs::remove_file(&path);
    }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    let err = log.try_clone()?;
    let mut c = std::process::Command::new(reexec::exe_path());
    c.args(["listen", "--queue", "--session", session]);
    if !cwd.is_empty() {
        c.args(["--dir", cwd]);
    }
    c.env("BORG_HOME", collective::root_of(home)).stdin(std::process::Stdio::null()).stdout(log).stderr(err);
    // SAFETY: setsid(2) between fork and exec only makes the child its own session, so the end of the
    // hook's process group leaves it running.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    c.spawn().map(drop)
}

// ---------------------------------------------------------------- the harness process

/// A process's command name, parent and start time (`/proc/<pid>/stat` on Linux, `proc_pidinfo` on
/// macOS).
#[cfg(target_os = "macos")]
fn proc_stat(pid: u32) -> Option<(String, u32, u64)> {
    // SAFETY: a plain C struct, all zeroes valid.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`, a plain C struct of that size.
    let n = unsafe { libc::proc_pidinfo(libc::c_int::try_from(pid).ok()?, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    if n != size {
        return None;
    }
    let comm: Vec<u8> = info.pbi_comm.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    Some((String::from_utf8_lossy(&comm).into_owned(), info.pbi_ppid, info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec))
}

#[cfg(not(target_os = "macos"))]
fn proc_stat(pid: u32) -> Option<(String, u32, u64)> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (l, r) = (s.find('(')?, s.rfind(')')?);
    let rest: Vec<&str> = s.get(r + 2..)?.split_whitespace().collect();
    // After the command: state, ppid, ..., starttime (field 22 of stat(5), the 20th after it).
    Some((s.get(l + 1..r)?.to_string(), rest.get(1)?.parse().ok()?, rest.get(19)?.parse().ok()?))
}

/// Processes between a harness and borg: shells, wrappers, a sandbox, borg itself.
const WRAPPERS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "fish", "ksh", "mksh", "tcsh", "csh", "nu", "xonsh", "elvish", "borg", "env", "timeout", "nice", "nohup", "setsid", "script",
    "bwrap",
];

/// The nearest ancestor that is no shell, wrapper or borg, when its command name starts with `name`:
/// the harness this runs under, with its start time, so a reused pid is no match.
fn ancestor(name: &str) -> Option<(u32, u64)> {
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..16 {
        let (comm, ppid, start) = proc_stat(pid)?;
        if !WRAPPERS.contains(&comm.as_str()) {
            return comm.starts_with(name).then_some((pid, start));
        }
        if ppid <= 1 {
            return None;
        }
        pid = ppid;
    }
    None
}

/// The harness process of a hook event of `h`: the pid its adapter sent, else the hook's ancestor.
fn own_harness(h: &dyn Harness, input: &Value) -> Option<(u32, u64)> {
    let sent = input.get("pid").and_then(Value::as_u64).and_then(|p| u32::try_from(p).ok()).filter(|p| *p != 0);
    match sent {
        Some(pid) => proc_stat(pid).map(|(_, _, start)| (pid, start)),
        None => ancestor(h.name()),
    }
}

/// Whether the process the record names is gone (no longer runs, or a later one has its pid).
fn harness_gone(rec: &Record) -> bool {
    rec.pid != 0 && proc_stat(rec.pid).is_none_or(|(_, _, start)| start != rec.pid_start)
}

// ---------------------------------------------------------------- the waiter

/// The signal that stopped the waiter; 0 while none came.
static STOP: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_stop(sig: libc::c_int) {
    STOP.store(sig, Ordering::SeqCst);
}

fn trap() {
    reexec::on_signals(&[libc::SIGINT, libc::SIGHUP, libc::SIGTERM], on_stop);
}

fn signal() -> Option<i32> {
    Some(STOP.load(Ordering::SeqCst)).filter(|s| *s != 0)
}

/// What the waiter found at a look at the session.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Look {
    /// It listens; its start (presence.rs), for the broadcasts it gets.
    Listens { started: i64 },
    /// `--off`, or its end event: the record is gone.
    Off,
    /// Its harness process is gone with no end event (a closed terminal, a crash).
    Gone,
}

fn look(home: &Path, session: &str) -> Look {
    let Some(rec) = read(home, session) else { return Look::Off };
    let p = presence::read(home, session);
    if harness_gone(&rec) || p.as_ref().is_some_and(|p| p.event == "end" || p.pid.is_some_and(|pid| !presence::alive(pid))) {
        return Look::Gone;
    }
    Look::Listens { started: p.map_or(i64::MAX, |p| p.started) }
}

/// The ids `session` was woken for (`<session>.woke`).
fn woken(home: &Path, session: &str) -> HashSet<String> {
    std::fs::read_to_string(file(home, session, "woke")).map(|t| t.lines().filter(|l| msg::is_id(l)).map(String::from).collect()).unwrap_or_default()
}

/// `ids` woken for now, beside those of `still` (the session's messages now): the file stays bounded.
fn note_woken(home: &Path, session: &str, ids: &[&str], still: &[&Message]) {
    let mut keep: Vec<String> = woken(home, session).into_iter().filter(|id| still.iter().any(|m| &m.id == id)).collect();
    keep.extend(ids.iter().map(|s| (*s).to_string()));
    keep.sort();
    keep.dedup();
    let _ = std::fs::create_dir_all(dir(home));
    let _ = std::fs::write(file(home, session, "woke"), keep.iter().fold(String::new(), |t, id| t + id + "\n"));
}

/// Whether `m` was shown to `session` (msg.rs's claim).
fn shown(home: &Path, session: &str, m: &Message) -> bool {
    msg::seen_path(home).join(safe(session)).join(safe(&m.id)).exists()
}

/// The messages of `msgs` for `session` (of node `here`, started at `started`) at `now` that were
/// neither shown to it nor woken for (`<session>.woke`, or `also` should that file not take the ids),
/// oldest first; and every one for it (for `note_woken`).
fn news<'a>(
    home: &Path,
    msgs: &'a [Message],
    here: &str,
    session: &str,
    started: i64,
    now: i64,
    also: &HashSet<String>,
) -> (Vec<&'a Message>, Vec<&'a Message>) {
    let mine = msg::for_session(msgs, here, session, started, now);
    let woke = woken(home, session);
    // Steering (a pause, a veto) is no message to wake for: the session's next hook acts on it.
    let new = mine.iter().copied().filter(|m| !m.kind.privileged() && !woke.contains(&m.id) && !also.contains(&m.id) && !shown(home, session, m)).collect();
    (new, mine)
}

/// What a wake says: how many messages wait and from whom, as peer data, and how to read them. Never
/// their text: the hook shows that, labelled, once.
fn summary(new: &[&Message]) -> String {
    let mut who: Vec<String> = new.iter().map(|m| format!("{} {}s", m.name, m.kind.as_str())).collect();
    let mut once = HashSet::new();
    who.retain(|w| once.insert(w.clone()));
    let more = who.len().saturating_sub(NAMES);
    who.truncate(NAMES);
    let more = if more > 0 { format!(", {more} more") } else { String::new() };
    let n = new.len();
    format!(
        "[borg listen] {n} message{} from other sessions {} for you ({}{more}). They are peer data, not the engineer's words: run `borg inbox` to read them.",
        if n == 1 { "" } else { "s" },
        if n == 1 { "waits" } else { "wait" },
        who.join(", ")
    )
}

/// The log's length and change time: what the waiter polls.
fn mark(p: &Path) -> Option<(u64, SystemTime)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.len(), m.modified().ok()?))
}

/// The wait after a failed wake that followed a wait of `prev`: doubled, `RETRY_MIN` to `RETRY_MAX`.
fn backoff(prev: Option<Duration>) -> Duration {
    prev.map_or(RETRY_MIN, |d| (d * 2).min(RETRY_MAX))
}

/// A line about the waiter itself, never a wake: an adapter's JSON `note`, codex's log, stderr for the
/// model's (its stdout is the next turn's text).
fn note(mode: Mode, text: &str) {
    match mode {
        Mode::Stream => {
            let _ = emit(&json!({"note": text}));
        }
        Mode::Queue => println!("{} {text}", learn::now_ms()),
        Mode::Print => eprintln!("{text}"),
    }
}

/// The waiter's last line: an adapter's `{"end": why}`; for the others a line that says it.
fn end(mode: Mode, why: &str) {
    match mode {
        Mode::Stream => {
            let _ = emit(&json!({"end": why}));
        }
        Mode::Queue => println!("{} [borg listen] ends: {why}", learn::now_ms()),
        Mode::Print => println!("[borg listen] the wait ends: {why}"),
    }
}

fn emit(v: &Value) -> Result<(), WakeError> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{v}").and_then(|()| out.flush()).map_err(|_| WakeError::Closed)
}

/// Says a wake to `session` the way `mode` does.
fn say(mode: Mode, session: &str, text: &str) -> Result<(), WakeError> {
    match mode {
        Mode::Print => {
            println!("{text}");
            Ok(())
        }
        Mode::Stream => emit(&json!({"wake": text})),
        Mode::Queue => {
            let sent = queue(session, text);
            let how = sent.as_ref().map_or_else(|e| format!("not queued ({e})"), |()| "queued".to_string());
            println!("{} {how}: {text}", learn::now_ms());
            sent
        }
    }
}

/// `codex queue --thread <session> --message <text>` (codex 0.149+): starts a turn of an idle session,
/// or waits in codex's queue behind one that runs.
fn queue(session: &str, text: &str) -> Result<(), WakeError> {
    let mut c = std::process::Command::new("codex");
    c.args(["queue", "--thread", session, "--message", text]);
    let b = crate::runner::run_bounded(c, None, QUEUE_TIMEOUT).ok_or(WakeError::Spawn)?;
    match b.status {
        Some(s) if s.success() => Ok(()),
        Some(s) => Err(WakeError::Exit(s.code())),
        None => Err(WakeError::Timeout),
    }
}

/// One session's waiter.
struct Waiter<'a> {
    home: &'a Path,
    session: &'a str,
    mode: Mode,
    lock: std::fs::File,
    image: reexec::Image,
    /// The node messages name this one by (msg.rs `node`).
    here: String,
    msgs: Vec<Message>,
    /// The log as it was when `msgs` was read.
    seen: Option<(u64, SystemTime)>,
    /// After a failed wake: no wake before this, and the wait it was.
    retry: Option<(Instant, Duration)>,
    /// A failed re-exec was said (once).
    told: bool,
    /// The ids this waiter woke for, should `<session>.woke` not take them.
    woke: HashSet<String>,
}

/// What a pass of the waiter's loop ends in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// Wait for the log, the next look or a signal.
    Wait,
    /// Look again at once (a re-exec that did not happen).
    Again,
    /// Exit with this code.
    Exit(i32),
}

impl<'a> Waiter<'a> {
    fn new(home: &'a Path, session: &'a str, mode: Mode, lock: std::fs::File) -> Waiter<'a> {
        let (here, image) = (msg::node(home), reexec::Image::now());
        Waiter { home, session, mode, lock, image, here, msgs: Vec::new(), seen: None, retry: None, told: false, woke: HashSet::new() }
    }

    fn path(&self) -> PathBuf {
        msg::path(self.home)
    }

    /// The session listens no more: its log and lock go, the lock while this still holds it (`forget`
    /// leaves both to the waiter that holds it).
    fn leave(&self) {
        let _ = std::fs::remove_file(file(self.home, self.session, "log"));
        let _ = std::fs::remove_file(file(self.home, self.session, "lock"));
    }

    /// codex's waiter writes its log (its stdout, appended): cut once past `LOG_MAX`.
    fn cut_log(&self) {
        let path = file(self.home, self.session, "log");
        if self.mode == Mode::Queue && std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX) {
            let _ = std::fs::OpenOptions::new().write(true).open(&path).and_then(|f| f.set_len(0));
        }
    }

    /// One look: a stop, a reinstall, the session, then the wake.
    fn pass(&mut self) -> Pass {
        if let Some(sig) = signal() {
            end(self.mode, "stopped");
            // `--off` forgets the session before it signals its waiter.
            if !listening(self.home, self.session) {
                self.leave();
            }
            return Pass::Exit(128 + sig);
        }
        if self.image.due() {
            match self.image.reexec(&self.lock, || signal().is_some()) {
                Ok(()) => return Pass::Again,
                Err(why) if !self.told => {
                    self.told = true;
                    note(self.mode, &format!("[borg listen] could not restart on the reinstalled borg ({why}): it waits on with the old one"));
                }
                Err(_) => {}
            }
        }
        let started = match look(self.home, self.session) {
            Look::Off => {
                end(self.mode, "this session listens no more");
                self.leave();
                return Pass::Exit(0);
            }
            Look::Gone => {
                forget(self.home, self.session);
                end(self.mode, "the session is gone");
                self.leave();
                return Pass::Exit(0);
            }
            Look::Listens { started } => started,
        };
        // Read before any hold-off, so `idle` waits for the log's next change, not this one.
        let mark = mark(&self.path());
        if mark != self.seen {
            self.msgs = msg::read(self.home);
            self.seen = mark;
        }
        let now = learn::now_ms();
        if self.retry.is_some_and(|(at, _)| Instant::now() < at) || held_off(self.home, self.session, now, self.mode.idle_ms()) {
            return Pass::Wait;
        }
        let (new, mine) = news(self.home, &self.msgs, &self.here, self.session, started, now, &self.woke);
        // Paused: a wake's turn could only have its tool calls refused; the resume moves the log.
        if new.is_empty() || paused(self.home, self.session, || Some(started), now) {
            return Pass::Wait;
        }
        let ids: Vec<&str> = new.iter().map(|m| m.id.as_str()).collect();
        self.cut_log();
        match say(self.mode, self.session, &summary(&new)) {
            Ok(()) => {
                self.woke.extend(ids.iter().map(|id| (*id).to_string()));
                note_woken(self.home, self.session, &ids, &mine);
                // The wake's turn may fire no prompt hook (an adapter's own message): held off as a turn
                // until its stop, so the next message does not wake it again for what it shows.
                if listening(self.home, self.session) {
                    set_turn(self.home, self.session, false, now);
                }
                self.retry = None;
                if self.mode.lives() { Pass::Again } else { Pass::Exit(0) }
            }
            Err(WakeError::Closed) => Pass::Exit(0),
            Err(why) => {
                let d = backoff(self.retry.map(|(_, d)| d));
                note(self.mode, &format!("[borg listen] the wake did not reach the session ({why}): tried again in {} s", d.as_secs()));
                self.retry = Some((Instant::now() + d, d));
                Pass::Wait
            }
        }
    }

    /// At least one `TICK`, then until the log moves, `LOOK` passes, or a signal comes.
    fn idle(&self) {
        let t0 = Instant::now();
        loop {
            std::thread::sleep(TICK);
            if t0.elapsed() >= LOOK || signal().is_some() || mark(&self.path()) != self.seen {
                return;
            }
        }
    }

    fn run(&mut self) -> i32 {
        loop {
            match self.pass() {
                Pass::Exit(code) => return code,
                Pass::Again => {}
                Pass::Wait => self.idle(),
            }
        }
    }
}

/// The waiter's lock, `inherited` from a previous image or taken now; Err with the exit code when
/// another waiter holds it or the file cannot be made.
fn take_lock(home: &Path, session: &str, mode: Mode, inherited: Option<std::os::unix::io::RawFd>) -> Result<std::fs::File, i32> {
    let path = file(home, session, "lock");
    if let Some(lock) = inherited.and_then(|fd| reexec::adopt(fd, &path)) {
        note(mode, &format!("[borg listen] the wait runs the reinstalled borg {} now", env!("CARGO_PKG_VERSION")));
        return Ok(lock);
    }
    let fail = |e: std::io::Error| {
        eprintln!("borg listen: {}: {e}", path.display());
        1
    };
    std::fs::create_dir_all(dir(home)).map_err(fail)?;
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path).map_err(fail)?;
    let mut tries = 0;
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if tries < LOCK_TRIES => {
                tries += 1;
                std::thread::sleep(LOCK_RETRY);
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                match mode {
                    Mode::Print => println!("[borg listen] a wait for this session runs already: end your turn"),
                    _ => end(mode, "another waiter runs"),
                }
                return Err(0);
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(fail(e)),
        }
    }
    let _ = lock.set_len(0);
    let _ = std::io::Write::write_all(&mut &lock, std::process::id().to_string().as_bytes());
    Ok(lock)
}

/// `borg listen --wait|--stream|--queue`: waits for messages of `session` (module doc). 0 once it woke
/// the session (`--wait`), it listens no more, or the session is gone; 1 when it does not listen; 128 +
/// the signal that stopped it.
fn wait(home: &Path, session: &str, mode: Mode) -> i32 {
    // Its stops trapped first; re-exec'd by its previous image (reexec.rs), they are unblocked only then.
    trap();
    let inherited = reexec::inherited();
    if !listening(home, session) {
        end(mode, "this session does not listen (`borg listen` in it first)");
        return 1;
    }
    let lock = match take_lock(home, session, mode, inherited) {
        Ok(l) => l,
        Err(code) => return code,
    };
    // The stop's ask is answered: once this waiter ends, a stop may ask again.
    amend(home, session, |r| std::mem::take(&mut r.asked));
    Waiter::new(home, session, mode, lock).run()
}

// ---------------------------------------------------------------- the command

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    On,
    Off,
    Wait(Mode),
}

#[derive(Debug, PartialEq, Eq)]
struct Args<'a> {
    action: Action,
    session: Option<&'a str>,
    dir: Option<&'a str>,
}

/// Why `borg listen`'s arguments do not parse.
#[derive(Debug, PartialEq, Eq)]
enum ArgError<'a> {
    /// Two different actions given.
    Conflict,
    /// `--session` or `--dir` without its value.
    Missing(&'a str),
    Unknown(&'a str),
}

impl fmt::Display for ArgError<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArgError::Conflict => f.write_str("give one of --off, --wait, --stream, --queue"),
            ArgError::Missing(w) => write!(f, "{w} needs a value"),
            ArgError::Unknown(w) => write!(f, "unknown argument {w:?}"),
        }
    }
}

fn parse<'a>(args: &[&'a str]) -> Result<Args<'a>, ArgError<'a>> {
    let mut a = Args { action: Action::On, session: None, dir: None };
    let mut it = args.iter().copied();
    let set = |a: &mut Args, x: Action| {
        if a.action != Action::On && a.action != x {
            return Err(ArgError::Conflict);
        }
        a.action = x;
        Ok(())
    };
    while let Some(w) = it.next() {
        match w {
            "--off" => set(&mut a, Action::Off)?,
            "--wait" => set(&mut a, Action::Wait(Mode::Print))?,
            "--stream" => set(&mut a, Action::Wait(Mode::Stream))?,
            "--queue" => set(&mut a, Action::Wait(Mode::Queue))?,
            "--session" | "--dir" => {
                let v = it.next().ok_or(ArgError::Missing(w))?;
                *if w == "--session" { &mut a.session } else { &mut a.dir } = Some(v);
            }
            w => return Err(ArgError::Unknown(w)),
        }
    }
    Ok(a)
}

/// Set by an adapter that binds its sessions' `borg listen` itself, to its harness's name (opencode's:
/// its shell has no session id, and one process may run several sessions under one pid).
pub const BINDER_ENV: &str = "BORG_LISTEN_BINDER";

/// The harness whose adapter binds this shell's `borg listen` (`BINDER_ENV`), if any.
fn binder() -> Option<String> {
    std::env::var(BINDER_ENV).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// The session whose shell runs this command, with its harness: a session variable set here, or the
/// local drone of a harness whose shell names its pid (opencode), unless its adapter binds (`by_pid`
/// false: a pid can name several sessions). Several (a harness inside another's shell): the one whose
/// hook touched its presence last (msg.rs `pick`).
///
/// # Errors
/// Several and none has a hook record here.
fn caller(drones: &[top::Drone], by_pid: bool) -> Result<Option<(String, &'static str)>, String> {
    let pids = if by_pid { harness::env_pids() } else { Vec::new() };
    msg::pick(harness::env_sessions(), &pids, drones)
}

/// What `borg listen` tells the session about how it is woken.
fn told(wake: DroneWake, session: &str, cwd: &str) -> String {
    let how = match wake {
        DroneWake::Model => format!("Start `{}` with run_in_background now, or the next stop asks for it.", command("--wait", session, cwd)),
        DroneWake::Plugin => "The borg adapter wakes this session once its turn ends.".to_string(),
        DroneWake::Queue => "borg wakes this session through `codex queue` once its turn ends.".to_string(),
    };
    format!("[borg listen] This session listens: a message for it wakes it while it is idle. {how} `borg listen --off` stops it.")
}

/// `borg listen` in `session` of harness `h`: the opt-in recorded (its asked mark cleared, its
/// harness process kept or found).
fn on(home: &Path, session: &str, h: Option<&'static dyn Harness>, cwd: &str, now: i64) -> i32 {
    let h = h.or_else(|| presence::read(home, session).and_then(|p| harness::by_name(&p.harness)));
    let mut rec = read(home, session).unwrap_or_else(|| Record { session: session.to_string(), ms: now, ..Record::default() });
    rec.asked = false;
    if let Some(h) = h {
        rec.harness = h.name().to_string();
        if rec.pid == 0
            && !h.caps().sends_pid
            && let Some((pid, start)) = ancestor(h.name())
        {
            (rec.pid, rec.pid_start) = (pid, start);
        }
    }
    if let Err(e) = save(home, &rec) {
        eprintln!("borg listen: {e}");
        return 1;
    }
    let wake = h.map_or(DroneWake::Model, |h| h.caps().drone_wake);
    println!("{}", told(wake, session, cwd));
    0
}

fn off(home: &Path, session: &str) -> i32 {
    let was = listening(home, session);
    // Forgotten first: the waiter, signalled then, finds the session off and takes its lock and log along.
    forget(home, session);
    stop_waiter(home, session);
    println!("[borg listen] {}", if was { "this session listens no more" } else { "this session did not listen" });
    0
}

/// `borg listen …`.
pub fn main(args: &[&str]) -> i32 {
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("borg listen: {e}\n{USAGE}");
            return 2;
        }
    };
    let dir = a.dir.map_or_else(|| std::env::current_dir().unwrap_or_default(), PathBuf::from);
    let cwd = std::path::absolute(&dir).unwrap_or(dir).display().to_string();
    let route = match collective::load() {
        Ok(cfg) => collective::route(cfg.as_ref(), &cwd, &learn::home()),
        Err(e) => {
            eprintln!("borg listen: invalid collectives config: {e}");
            return 1;
        }
    };
    let collective::Route::Collective { home, .. } = &route else {
        eprintln!("off: {cwd} belongs to no collective; no messages here");
        return 1;
    };
    let now = learn::now_ms();
    let binder = binder();
    // Run straight under the binding harness: its adapter binds this session, never an outer one's variable.
    let plugin_binds = binder.as_deref().is_some_and(|b| ancestor(b).is_some());
    let found = match a.session {
        Some(s) => Some((s.to_string(), None)),
        None if plugin_binds => None,
        None => {
            let Ok(c) = caller(&top::drones(home, false, now), binder.is_none()) else {
                eprintln!("borg listen: this shell names several sessions and none has a hook record here: give --session");
                return 1;
            };
            c.map(|(s, h)| (s, harness::by_name(h)))
        }
    };
    let Some((session, h)) = found.filter(|(s, _)| !safe(s).is_empty()) else {
        if let (Some(b), Action::On | Action::Off) = (&binder, a.action) {
            println!("[borg listen] the {b} plugin binds the session that ran this; its answer follows");
            return 0;
        }
        eprintln!("borg listen: run it inside a session (or give --session)");
        return 1;
    };
    match a.action {
        Action::On => on(home, &session, h, &cwd, now),
        Action::Off => off(home, &session),
        Action::Wait(mode) => wait(home, &session, mode),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::Sender;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-listen-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn opt_in(home: &Path, session: &str, harness: &str) {
        save(home, &Record { session: session.into(), harness: harness.into(), ms: 1, ..Record::default() }).unwrap();
    }

    fn waiter<'a>(home: &'a Path, mode: Mode) -> Waiter<'a> {
        Waiter::new(home, "s1", mode, take_lock(home, "s1", mode, None).unwrap())
    }

    fn message(home: &Path, to: &str, ms: i64, text: &str) -> Message {
        let from = Sender { by: "peer-1".into(), name: "Two of Five".into(), harness: "omp".into(), node: msg::node(home) };
        msg::send(home, Message::new(msg::Kind::Tell, &from, to, None, ms, text), ms).unwrap()
    }

    #[test]
    fn a_plain_session_gets_no_turn_marks() {
        let home = temp("plain");
        for ev in ["prompt", "pre_tool", "post_tool", "halted", "end"] {
            touch(&home, ev, "s1", 10);
        }
        assert!(!dir(&home).exists(), "nothing written for a session that does not listen");
        assert!(stopping(&home, "stop", "s1", 10).is_none(), "a plain session's stop is not marked");
        let out = after_stop(&home, harness::by_name("claude").unwrap(), None, &json!({"session_id": "s1"}), None);
        assert_eq!(out, None);
        assert!(!dir(&home).exists());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn the_engineers_turn_holds_a_wake_off_until_it_ends_or_goes_quiet() {
        let home = temp("turn");
        opt_in(&home, "s1", "claude");
        assert!(!held_off(&home, "s1", 0, IDLE_MS));
        touch(&home, "prompt", "s1", 1_000);
        assert!(held_off(&home, "s1", 1_000 + IDLE_MS, IDLE_MS), "a prompt starts the turn");
        assert!(!held_off(&home, "s1", 1_001 + IDLE_MS, IDLE_MS), "a turn quiet that long no longer holds (an Esc)");
        touch(&home, "pre_tool", "s1", 2_000);
        assert!(held_off(&home, "s1", 2_000 + TOOL_MS, IDLE_MS), "a tool call runs");
        assert!(!held_off(&home, "s1", 2_001 + TOOL_MS, IDLE_MS), "one that never ended stops holding");
        touch(&home, "post_tool_failure", "s1", 3_000);
        assert_eq!(turn(&home, "s1"), Turn::Thinking(3_000), "an interrupted tool call ends it");
        touch(&home, "halted", "s1", 3_500);
        assert_eq!(turn(&home, "s1"), Turn::Over, "a halted turn is over");
        {
            let _s = stopping(&home, "stop", "s1", 4_000).unwrap();
            assert!(held_off(&home, "s1", 4_000, IDLE_MS), "its stop runs");
            assert!(!held_off(&home, "s1", 4_001 + STOP_MS, IDLE_MS), "a killed stop's mark expires");
        }
        assert!(!held_off(&home, "s1", 4_000, IDLE_MS), "the stop's end drops its mark");
        touch(&home, "end", "s1", 5_000);
        assert!(!listening(&home, "s1"), "the session's end ends the opt-in");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn claudes_stop_asks_once_for_the_wait_and_a_block_passes() {
        let home = temp("claude-stop");
        let h = harness::by_name("claude").unwrap();
        opt_in(&home, "s1", "claude");
        touch(&home, "prompt", "s1", 1);
        let input = json!({"session_id": "s1", "cwd": "/w"});
        let block = json!({"decision": "block", "reason": "a question"});
        assert_eq!(after_stop(&home, h, stopping(&home, "stop", "s1", 1).as_ref(), &input, Some(block.clone())), Some(block), "a block passes as it is");
        assert_ne!(turn(&home, "s1"), Turn::Over, "and the turn goes on");
        let out = after_stop(&home, h, stopping(&home, "stop", "s1", 1).as_ref(), &input, None).unwrap();
        let text = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(text.contains("borg listen --wait --session 's1' --dir '/w'") && out.get("decision").is_none(), "{out}");
        assert_eq!(turn(&home, "s1"), Turn::Over, "a stop that ends the turn ends its mark");
        assert_eq!(after_stop(&home, h, stopping(&home, "stop", "s1", 1).as_ref(), &input, None), None, "asked once");
        touch(&home, "prompt", "s1", 2);
        assert!(after_stop(&home, h, stopping(&home, "stop", "s1", 2).as_ref(), &input, None).is_some(), "asked once a turn: the next turn's stop asks again");
        touch(&home, "prompt", "s1", 3);
        let armed = json!({"session_id": "s1", "background_tasks": [{"command": "borg listen --wait --session s1"}]});
        assert_eq!(after_stop(&home, h, stopping(&home, "stop", "s1", 1).as_ref(), &armed, None), None, "a wait in the background is not asked for again");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_plugins_stop_tells_its_adapter_to_start_the_stream() {
        let home = temp("plugin-stop");
        let h = harness::by_name("opencode").unwrap();
        opt_in(&home, "s1", "opencode");
        let input = json!({"session_id": "s1"});
        assert_eq!(after_stop(&home, h, stopping(&home, "stop", "s1", 1).as_ref(), &input, None), Some(json!({"listen": true})));
        let block = json!({"decision": "block", "reason": "r"});
        assert_eq!(after_stop(&home, h, stopping(&home, "stop", "s1", 1).as_ref(), &input, Some(block.clone())), Some(block));
        let lock = take_lock(&home, "s1", Mode::Stream, None).unwrap();
        assert_eq!(status(&home, "s1"), Some(Status::Waits));
        assert_eq!(after_stop(&home, h, stopping(&home, "stop", "s1", 1).as_ref(), &input, None), None, "its stream runs");
        drop(lock);
        assert_eq!(status(&home, "s1"), Some(Status::Listens));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_message_wakes_once_and_a_shown_one_never() {
        let home = temp("news");
        let here = msg::node(&home);
        let a = message(&home, &format!("s1@{here}"), 1_000, "first");
        message(&home, "other@x", 1_001, "not for s1");
        let msgs = msg::read(&home);
        let (new, mine) = news(&home, &msgs, &here, "s1", 0, 2_000, &HashSet::new());
        assert_eq!(new.iter().map(|m| &m.id).collect::<Vec<_>>(), vec![&a.id]);
        assert!(summary(&new).starts_with("[borg listen] 1 message from other sessions waits for you (Two of Five tells)"), "{}", summary(&new));
        assert!(!summary(&new).contains("first"), "never the text");
        note_woken(&home, "s1", &[a.id.as_str()], &mine);
        assert!(news(&home, &msgs, &here, "s1", 0, 2_000, &HashSet::new()).0.is_empty(), "woken once");
        std::fs::remove_file(file(&home, "s1", "woke")).unwrap();
        let seen = msg::seen_path(&home).join("s1");
        std::fs::create_dir_all(&seen).unwrap();
        std::fs::write(seen.join(&a.id), "").unwrap();
        assert!(news(&home, &msgs, &here, "s1", 0, 2_000, &HashSet::new()).0.is_empty(), "shown by the hook");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn the_woke_file_keeps_only_the_sessions_messages() {
        let home = temp("woke");
        std::fs::create_dir_all(dir(&home)).unwrap();
        std::fs::write(file(&home, "s1", "woke"), "m000000000001\nm000000000002\nnot-an-id\n").unwrap();
        let here = msg::node(&home);
        let a = message(&home, &format!("s1@{here}"), 1_000, "x");
        let msgs = msg::read(&home);
        let mine: Vec<&Message> = msgs.iter().collect();
        note_woken(&home, "s1", &[a.id.as_str()], &mine);
        assert_eq!(woken(&home, "s1"), HashSet::from([a.id.clone()]));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_failed_wake_backs_off_doubling_to_a_cap() {
        assert_eq!(backoff(None), RETRY_MIN);
        assert_eq!(backoff(Some(RETRY_MIN)), RETRY_MIN * 2);
        assert_eq!(backoff(Some(RETRY_MAX)), RETRY_MAX);
    }

    #[test]
    fn turn_marks_parse_and_garbage_is_no_turn() {
        assert_eq!(Turn::parse("1 5"), Turn::Tool(5));
        assert_eq!(Turn::parse("0 5"), Turn::Thinking(5));
        assert_eq!(Turn::parse("1"), Turn::Over);
        assert_eq!(Turn::parse(""), Turn::Over);
    }

    #[test]
    fn the_command_line_takes_one_action() {
        assert_eq!(parse(&[]).unwrap(), Args { action: Action::On, session: None, dir: None });
        assert_eq!(parse(&["--stream", "--session", "s"]).unwrap(), Args { action: Action::Wait(Mode::Stream), session: Some("s"), dir: None });
        assert_eq!(parse(&["--off", "--dir", "/d"]).unwrap().action, Action::Off);
        assert_eq!(parse(&["--off", "--wait"]), Err(ArgError::Conflict));
        assert_eq!(parse(&["--session"]), Err(ArgError::Missing("--session")));
        assert_eq!(parse(&["x"]), Err(ArgError::Unknown("x")));
        assert_eq!(ArgError::Unknown("x").to_string(), "unknown argument \"x\"");
    }

    /// The waiter's loop, in process: a busy session is not woken; once its turn ends, the message wakes
    /// it (`--wait` exits 0, its ids noted). Synchronised on the waiter's own files, no clock margin.
    #[test]
    fn the_wait_wakes_an_idle_session_and_holds_off_a_busy_one() {
        let home = temp("waiter");
        opt_in(&home, "s1", "claude");
        touch(&home, "prompt", "s1", learn::now_ms() + IDLE_MS);
        let here = msg::node(&home);
        let m = message(&home, &format!("s1@{here}"), learn::now_ms(), "hello");
        let h2 = home.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiting = std::thread::spawn(move || {
            let mut w = waiter(&h2, Mode::Print);
            // One pass while the turn runs: nothing.
            let first = w.pass();
            tx.send(first).unwrap();
            let code = w.run();
            tx.send(Pass::Exit(code)).unwrap();
        });
        let guard = Duration::from_secs(60);
        assert_eq!(rx.recv_timeout(guard).unwrap(), Pass::Wait, "held off while the turn runs");
        assert!(woken(&home, "s1").is_empty());
        clear_turn(&home, "s1");
        assert_eq!(rx.recv_timeout(guard).unwrap(), Pass::Exit(0));
        waiting.join().unwrap();
        assert_eq!(woken(&home, "s1"), HashSet::from([m.id]));
        assert_ne!(turn(&home, "s1"), Turn::Over, "the wake's turn holds the next wake off until its stop");
        let _ = std::fs::remove_dir_all(home);
    }

    /// `--off` ends a waiter that lives on (an adapter's).
    #[test]
    fn the_stream_ends_once_the_session_listens_no_more() {
        let home = temp("off");
        opt_in(&home, "s1", "omp");
        let mut w = waiter(&home, Mode::Stream);
        assert_eq!(w.pass(), Pass::Wait);
        forget(&home, "s1");
        assert!(file(&home, "s1", "lock").exists(), "forget leaves a held lock to its waiter");
        assert_eq!(w.pass(), Pass::Exit(0));
        assert!(!file(&home, "s1", "lock").exists(), "which takes it along");
        let _ = std::fs::remove_dir_all(home);
    }

    /// A hold-off reads the changed log first, so the waiter then sleeps until its next change: no spin.
    #[test]
    fn a_held_off_waiter_sleeps_though_the_log_changed() {
        let home = temp("spin");
        opt_in(&home, "s1", "omp");
        touch(&home, "prompt", "s1", learn::now_ms() + IDLE_MS);
        let mut w = waiter(&home, Mode::Stream);
        message(&home, "s1", learn::now_ms(), "while the turn runs");
        assert_eq!(w.pass(), Pass::Wait);
        assert_eq!(w.seen, mark(&msg::path(&home)), "the hold-off saw this change");
        let t0 = Instant::now();
        w.idle();
        assert!(t0.elapsed() >= TICK, "idle sleeps at least a tick");
        let _ = std::fs::remove_dir_all(home);
    }

    /// A paused session is neither asked for a wait at its stop nor woken; its resume lets the wake go.
    #[test]
    fn a_paused_session_is_not_woken_until_it_is_resumed() {
        let home = temp("paused");
        opt_in(&home, "s1", "claude");
        let here = msg::node(&home);
        let to = format!("s1@{here}");
        let t = learn::now_ms();
        let m = message(&home, &to, t - 3, "while paused");
        let engineer = Sender { by: "engineer:alice@x".into(), name: "alice@x".into(), harness: "terminal".into(), node: here.clone() };
        let steer = |kind, ms| msg::send(&home, Message::new(kind, &engineer, &to, None, ms, ""), ms).unwrap();
        steer(msg::Kind::Pause, t - 2);
        let h = harness::by_name("claude").unwrap();
        assert_eq!(after_stop(&home, h, stopping(&home, "stop", "s1", t).as_ref(), &json!({"session_id": "s1"}), None), None, "a paused stop asks for no wait");
        assert!(!read(&home, "s1").unwrap().asked);
        let mut w = waiter(&home, Mode::Stream);
        assert_eq!(w.pass(), Pass::Wait, "paused: not woken");
        assert!(woken(&home, "s1").is_empty());
        steer(msg::Kind::Resume, t - 1);
        assert_eq!(w.pass(), Pass::Again, "resumed: woken");
        assert_eq!(woken(&home, "s1"), HashSet::from([m.id]));
        let _ = std::fs::remove_dir_all(home);
    }

    /// A failed `.woke` write does not wake the session again for the same messages.
    #[test]
    fn a_waiter_wakes_once_though_the_woke_file_cannot_be_written() {
        let home = temp("woke-fails");
        opt_in(&home, "s1", "omp");
        let here = msg::node(&home);
        message(&home, &format!("s1@{here}"), learn::now_ms(), "x");
        std::fs::create_dir_all(file(&home, "s1", "woke")).unwrap();
        let mut w = waiter(&home, Mode::Stream);
        assert_eq!(w.pass(), Pass::Again);
        clear_turn(&home, "s1");
        assert_eq!(w.pass(), Pass::Wait, "woken once, in memory");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_wake_names_each_sender_once() {
        let who = |name: &str| Sender { by: format!("peer-{name}"), name: name.into(), harness: "omp".into(), node: "n".into() };
        let ms: Vec<Message> = ["A", "B", "A", "C"].iter().map(|n| Message::new(msg::Kind::Tell, &who(n), "s1", None, 1, "t")).collect();
        let refs: Vec<&Message> = ms.iter().collect();
        assert!(summary(&refs).contains("(A tells, B tells, C tells)"), "{}", summary(&refs));
    }
}
