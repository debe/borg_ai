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
//!   adapter starts `borg listen --stream --session <id>`: one JSON line per wake (`{"wake": text, "n":
//!   count, "ids": [message ids]}`), for the session's life, which it sends in by the harness's own path.
//!   It answers each wake on the waiter's stdin, one JSON line: `{"sent": ids}` once it went in,
//!   `{"failed": ids}` when the harness refused it; a refused wake backs off (`backoff`) and its
//!   messages wake the session again.
//! - codex (`Queue`): the stop starts `borg listen --queue` detached when none runs. Each wake is
//!   `codex queue --thread <session> --message <text>`, bounded by `QUEUE_TIMEOUT`, and a failed one
//!   backs off (`backoff`) before the same messages are tried again.
//!
//! The engineer's turn holds every wake off: `<session>.busy` from its prompt to its stop, and
//! `<session>.stopping` while the stop runs (`Stopping`). A turn that never reached its stop (an Esc)
//! clears by itself: once it has been quiet longer than `TURN_IDLE_MS` (`QUEUE_IDLE_MS` for codex's
//! waiter), or a tool call that never ended has run longer than `TOOL_MS`; each longer for a session
//! whose own gaps run longer (`Holds`, learned from `<session>.gaps`), never shorter. codex's
//! `Interrupt` and an adapter's `halted` clear it at once. Meanwhile the turn's own hooks show what
//! arrives.
//!
//! One waiter per session (`<session>.lock`, flock, its pid inside). It stats the log every `TICK`;
//! when its length or change time moved it reads the log's talk (msg.rs `talk_mark`), and the log only
//! when a tell, ask or steer was written since (an acknowledgement, here or merged from the mesh, moves
//! the log and not its talk). It looks at the session every `LOOK`, follows a
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
/// Learned, from this floor up (dated 2026-10-28: the fixed value before holds were learned): without
/// an event for this long and no tool call running, the engineer's turn no longer holds a wake off (an
/// Esc with no stop); longer for a session whose own quiet gaps run longer (`Holds`). Claude Code's and
/// an adapter's wake would land in a turn that only went quiet (a long generation), so it waits long;
/// codex's, should it land in a running turn, waits in codex's queue behind it (`QUEUE_IDLE_MS`).
pub const TURN_IDLE_MS: i64 = 5 * 60_000;
/// Learned, from this floor up (dated 2026-10-28): [`TURN_IDLE_MS`] for codex's waiter.
const QUEUE_IDLE_MS: i64 = 60_000;
/// Learned, from this floor up (dated 2026-10-28): a tool call that started this long ago and never
/// ended no longer holds a wake off; longer for a session whose own tool calls run longer (`Holds`).
pub const TOOL_MS: i64 = 10 * 60_000;
/// Method: the percentile of a session's own gaps (nearest rank) that lengthens a hold past its floor: a
/// session whose pauses run long holds through 19 of 20 of them, and once it has 20 gaps, one outlier
/// (a suspended laptop, a call waiting on the engineer's approval) no longer holds every later Esc'd
/// turn as long.
const HOLD_PCT: usize = 95;
/// Method: how many of a session's latest gaps of each kind a hold is read from: it follows the
/// session's recent pace, and `<session>.gaps` stays a few kB.
const GAPS_KEPT: usize = 64;
/// Guard: how long a stop's mark outlives the longest stop of any harness (`stop_ms`), should its hook
/// be killed before it removed the mark: the hook's own work past its budget (its records, its answer)
/// runs inside it, and a killed hook's mark still expires.
const STOP_SLACK_MS: i64 = 30_000;
/// Method: how often the waiter stats the log and looks for a stop signal.
const TICK: Duration = Duration::from_millis(250);
/// Method: how often it looks at the session (its record, its turn, its harness process) with the log
/// unchanged.
const LOOK: Duration = Duration::from_secs(2);
/// Guard: the longest one `codex queue` may take, so a hung codex never stalls the waiter.
const QUEUE_TIMEOUT: Duration = Duration::from_secs(30);
/// Guards: the first and the longest wait after a failed wake. The first stops a harness that refuses
/// every wake from being asked again at once; the longest bounds how late a wake comes once the harness
/// takes them again.
const RETRY_MIN: Duration = Duration::from_secs(10);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// Guard: a detached waiter's log is cut once it is past this size.
const LOG_MAX: u64 = 64 * 1024;
/// Display: how many senders a wake names.
const NAMES: usize = 3;
/// Guards: a lock busy at a waiter's start may be a probe's (`waiter_runs`, an instant): tried this many
/// more times, this far apart, before another waiter holds it.
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

    /// The least a quiet turn of its session holds a wake off, whatever the session's own gaps.
    fn idle_prior(self) -> i64 {
        if self == Mode::Queue { QUEUE_IDLE_MS } else { TURN_IDLE_MS }
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
    for ext in ["json", "busy", "stopping", "woke", "gaps"] {
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

/// The engineer's turn as its marks say (`<session>.busy`: `<tool> <ms>`, or a wake's, `WAKE`).
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

    /// Whether it holds a wake off at `now`, under the session's `holds`.
    fn running(self, now: i64, holds: Holds) -> bool {
        match self {
            Turn::Over => false,
            Turn::Thinking(ms) => now.saturating_sub(ms) <= holds.idle_ms,
            Turn::Tool(ms) => now.saturating_sub(ms) <= holds.tool_ms,
        }
    }
}

fn turn(home: &Path, session: &str) -> Turn {
    std::fs::read_to_string(file(home, session, "busy")).map_or(Turn::Over, |t| Turn::parse(&t))
}

/// How long a running turn of a session holds a wake off: quiet since its last event (`idle_ms`), and
/// in a tool call that never ended (`tool_ms`). Each is its floor (`Mode::idle_prior`, `TOOL_MS`),
/// lengthened by the session's own gaps (`gaps`) where they run longer, never shortened (`hold`): a
/// turn's gaps are heavy-tailed (a build, a test run), so a hold read from them alone would release
/// one ordinary pause or tool call in 20, and a wake released into a running turn costs a round or a
/// turn (Claude Code's wait exits into it, and its stop asks the model to start the wait again; codex
/// runs the queued wake after it, for messages the turn's own hooks showed). Holding an Esc'd turn's
/// wake to the floor costs only its delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Holds {
    idle_ms: i64,
    tool_ms: i64,
}

/// The holds of `session` for a waiter of `mode`.
fn holds(home: &Path, session: &str, mode: Mode) -> Holds {
    let (idle, tool) = gaps(home, session);
    Holds { idle_ms: hold(&idle, mode.idle_prior()), tool_ms: hold(&tool, TOOL_MS) }
}

/// The hold that `seen` gaps of one kind give: their `HOLD_PCT` percentile (nearest rank), never below
/// `floor`; the floor alone while there are none.
fn hold(seen: &[i64], floor: i64) -> i64 {
    let mut all = seen.to_vec();
    all.sort_unstable();
    let rank = (all.len() * HOLD_PCT).div_ceil(100);
    all.get(rank.saturating_sub(1)).map_or(floor, |&p| p.max(floor))
}

/// What `<session>.gaps` records: a turn's quiet gap (from an event to the next: a prompt or a tool
/// call's end to the next tool call or the stop), or a tool call's duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gap {
    Idle,
    Tool,
}

impl Gap {
    fn tag(self) -> &'static str {
        match self {
            Gap::Idle => "idle",
            Gap::Tool => "tool",
        }
    }
}

/// The session's recorded gaps, oldest first: its quiet ones and its tool calls' (`<session>.gaps`, one
/// `idle <ms>` or `tool <ms>` per line; anything else is skipped).
fn gaps(home: &Path, session: &str) -> (Vec<i64>, Vec<i64>) {
    let text = std::fs::read_to_string(file(home, session, "gaps")).unwrap_or_default();
    let (mut idle, mut tool) = (Vec::new(), Vec::new());
    for l in text.lines() {
        let Some((tag, ms)) = l.split_once(' ') else { continue };
        let Ok(ms) = ms.trim().parse::<i64>() else { continue };
        match tag {
            "idle" => idle.push(ms),
            "tool" => tool.push(ms),
            _ => {}
        }
    }
    (idle, tool)
}

/// One gap of `session` recorded, beside the latest `GAPS_KEPT` of each kind; the file is written whole
/// and renamed into place, so a waiter never reads half of it. A negative gap (a clock step) is none.
fn observe(home: &Path, session: &str, gap: Gap, ms: i64) {
    if ms < 0 {
        return;
    }
    let (mut idle, mut tool) = gaps(home, session);
    match gap {
        Gap::Idle => idle.push(ms),
        Gap::Tool => tool.push(ms),
    }
    let latest = |v: &[i64]| v.iter().skip(v.len().saturating_sub(GAPS_KEPT)).copied().collect::<Vec<_>>();
    let mut text = String::new();
    for (g, v) in [(Gap::Idle, latest(&idle)), (Gap::Tool, latest(&tool))] {
        for ms in v {
            text = text + g.tag() + " " + &ms.to_string() + "\n";
        }
    }
    let tmp = dir(home).join(format!(".{}.gaps.{}.tmp", safe(session), std::process::id()));
    if std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, file(home, session, "gaps"))).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn set_turn(home: &Path, session: &str, tool: bool, now: i64) {
    let _ = std::fs::write(file(home, session, "busy"), format!("{} {now}", u8::from(tool)));
}

/// Protocol, of this node alone: the first word of a wake's turn mark, `w <ms>` and the start of a tool
/// call it found running (`set_wake`). Every reader takes it as `Thinking` (an older one too), so it
/// holds the next wake off until the turn's stop; it says it is a wake's, so a waiter undoes it while no
/// hook moved it, also one re-executed since its wake (`unset_wake`); and the tool call's end still
/// records that call's gap (`touch`).
const WAKE: &str = "w";

/// Marks the turn a wake starts (`WAKE`), keeping the start of a tool call it found running.
fn set_wake(home: &Path, session: &str, now: i64) {
    let tool = match turn(home, session) {
        Turn::Tool(ms) => format!(" {ms}"),
        _ => String::new(),
    };
    let _ = std::fs::write(file(home, session, "busy"), format!("{WAKE} {now}{tool}"));
}

/// The turn mark of a wake while no hook moved it: the start of the tool call it found running, if any.
fn wake_mark(home: &Path, session: &str) -> Option<Option<i64>> {
    let text = std::fs::read_to_string(file(home, session, "busy")).ok()?;
    let mut it = text.split_whitespace();
    (it.next() == Some(WAKE) && it.next()?.parse::<i64>().is_ok()).then(|| it.next().and_then(|t| t.parse().ok()))
}

/// Undoes a wake's turn mark while no hook moved it: the tool call it found running runs again, else
/// no turn does.
fn unset_wake(home: &Path, session: &str) {
    match wake_mark(home, session) {
        Some(Some(tool)) => set_turn(home, session, true, tool),
        Some(None) => clear_turn(home, session),
        None => {}
    }
}

fn clear_turn(home: &Path, session: &str) {
    let _ = std::fs::remove_file(file(home, session, "busy"));
}

/// How long a stop's mark lasts should its hook be killed first: derived, the longest stop of any
/// harness (`harness::longest_stop`) and `STOP_SLACK_MS`.
fn stop_ms() -> i64 {
    i64::try_from(harness::longest_stop().as_millis()).unwrap_or(i64::MAX).saturating_add(STOP_SLACK_MS)
}

/// A stop of a listening session runs (hook.rs `main`): marked from its start until it returns (the
/// guard's drop), or until `stop_ms` should the harness kill the hook first. No wake meanwhile. Its
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
/// one stat. A mark that could not be written still says the session listens. The turn's quiet gap
/// before its stop is one of its session's gaps.
pub fn stopping(home: &Path, event: &str, session: &str, now: i64) -> Option<Stopping> {
    if event != "stop" || !listening(home, session) {
        return None;
    }
    let mark = file(home, session, "stopping");
    let _ = std::fs::write(&mark, now.saturating_add(stop_ms()).to_string());
    if let Turn::Thinking(ms) = turn(home, session) {
        observe(home, session, Gap::Idle, now.saturating_sub(ms));
    }
    Some(Stopping { mark, session: session.to_owned() })
}

fn stop_runs(home: &Path, session: &str, now: i64) -> bool {
    std::fs::read_to_string(file(home, session, "stopping")).ok().and_then(|t| t.trim().parse::<i64>().ok()).is_some_and(|until| now <= until)
}

/// Whether a wake of `session` waits at `now`: the engineer's turn (under the session's holds for a
/// waiter of `mode`, read only while a turn runs) or its stop runs.
fn held_off(home: &Path, session: &str, now: i64, mode: Mode) -> bool {
    let t = turn(home, session);
    (t != Turn::Over && t.running(now, holds(home, session, mode))) || stop_runs(home, session, now)
}

/// Whether `session` (started at `started`, asked only when someone steers here) is paused: its stop is
/// its wait for the engineer (steer.rs `around`), and every tool call a wake asks for would be refused.
fn paused(home: &Path, session: &str, started: impl FnOnce() -> Option<i64>, now: i64) -> bool {
    crate::steer::active(home) && crate::steer::paused(home, session, started(), now)
}

/// The hook's bookkeeping of a listening session's turn at `event` (hook.rs `main`, every event but the
/// stop, `after_stop`): a prompt starts it (and lets its stop ask for the wait again), a tool call's
/// start and end move it on, each recording the gap it ends (`observe`: the quiet one before a tool
/// call, a tool call's own at its end), a halted turn ends it, the session's end forgets the opt-in.
/// One stat for a session that does not listen.
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
        (_, Some(tool)) => {
            match (turn(home, session), tool) {
                (Turn::Thinking(ms), true) => observe(home, session, Gap::Idle, now.saturating_sub(ms)),
                (Turn::Tool(ms), false) => observe(home, session, Gap::Tool, now.saturating_sub(ms)),
                // A wake marked the turn while a tool call ran: the call's end still records its gap.
                (Turn::Thinking(_), false) => {
                    if let Some(Some(ms)) = wake_mark(home, session) {
                        observe(home, session, Gap::Tool, now.saturating_sub(ms));
                    }
                }
                _ => {}
            }
            set_turn(home, session, tool, now);
        }
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
/// is: the turn goes on from now (its next gap starts after the stop's own time), and its next stop
/// decides. A paused session's stop starts and asks for nothing: it waits for the engineer. `stop` is
/// `stopping`'s mark: None (a plain session, or not a stop) returns `out` without a stat.
pub fn after_stop(home: &Path, h: &dyn Harness, stop: Option<&Stopping>, input: &Value, out: Option<Value>) -> Option<Value> {
    let Some(stop) = stop else { return out };
    let session = stop.session.as_str();
    let Some(rec) = read(home, session) else { return out };
    if blocks(out.as_ref()) {
        set_turn(home, session, false, learn::now_ms());
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

/// Guard: the most processes `ancestor` walks up; a chain of wrappers deeper than this is no harness's,
/// and the walk ends whatever `/proc` reports.
const ANCESTRY: usize = 16;

/// The nearest ancestor that is no shell, wrapper or borg, when its command name starts with `name`:
/// the harness this runs under, with its start time, so a reused pid is no match.
fn ancestor(name: &str) -> Option<(u32, u64)> {
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..ANCESTRY {
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

/// `ids` woken for no more (the adapter could not send their wake in): they wake the session again.
fn unnote_woken(home: &Path, session: &str, ids: &[String]) {
    let path = file(home, session, "woke");
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let keep: String = text.lines().filter(|l| msg::is_id(l) && !ids.iter().any(|id| id == l)).fold(String::new(), |t, id| t + id + "\n");
    let _ = std::fs::write(path, keep);
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

/// Says a wake for the messages `ids` to `session` the way `mode` does: an adapter's line names them
/// (`n`, `ids`), for its answer (`Answer`) and for the hooks that show them meanwhile.
fn say(mode: Mode, session: &str, text: &str, ids: &[&str]) -> Result<(), WakeError> {
    match mode {
        Mode::Print => {
            println!("{text}");
            Ok(())
        }
        Mode::Stream => emit(&json!({"wake": text, "n": ids.len(), "ids": ids})),
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

/// An adapter's answer to a wake of its stream, one JSON line on the waiter's stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Answer {
    /// `{"sent": ids}`: the wake went in.
    Sent,
    /// `{"failed": ids}`: the harness refused it; these messages wake the session again.
    Failed(Vec<String>),
}

impl Answer {
    fn parse(line: &str) -> Option<Answer> {
        let v: Value = serde_json::from_str(line).ok()?;
        let ids = |k: &str| {
            v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).filter(|id| msg::is_id(id)).map(String::from).collect::<Vec<_>>())
        };
        match (ids("sent"), ids("failed")) {
            (_, Some(ids)) => Some(Answer::Failed(ids)),
            (Some(_), None) => Some(Answer::Sent),
            (None, None) => None,
        }
    }
}

/// The adapter's answers, read from stdin on a thread of their own: the waiter takes them at its next
/// pass. An adapter that answers nothing (or an older one, its stdin closed) leaves every wake as sent.
fn answers() -> std::sync::mpsc::Receiver<Answer> {
    use std::io::BufRead as _;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if let Some(a) = Answer::parse(&line)
                && tx.send(a).is_err()
            {
                break;
            }
        }
    });
    rx
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
    /// The log as it was when the waiter last looked at it.
    seen: Option<(u64, SystemTime)>,
    /// The log's talk when `msgs` was read (msg.rs `talk_mark`).
    talk: Option<String>,
    /// After a failed wake: no wake before this, and the wait it was (a stream's lasts until its adapter
    /// says a wake went in).
    retry: Option<(Instant, Duration)>,
    /// A failed re-exec was said (once).
    told: bool,
    /// The ids this waiter woke for, should `<session>.woke` not take them.
    woke: HashSet<String>,
    /// The adapter's answers to its wakes (`--stream` only).
    answers: Option<std::sync::mpsc::Receiver<Answer>>,
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
        Waiter {
            home,
            session,
            mode,
            lock,
            image,
            here,
            msgs: Vec::new(),
            seen: None,
            talk: None,
            retry: None,
            told: false,
            woke: HashSet::new(),
            answers: None,
        }
    }

    /// The adapter's answers since the last pass: a wake that went in ends the backoff; one the harness
    /// refused is undone (its ids woken for no more, the turn mark it set undone while no hook moved it,
    /// `unset_wake`: also when an earlier image of this waiter sent it) and backs off before its messages
    /// wake the session again.
    fn take_answers(&mut self) {
        let got: Vec<Answer> = self.answers.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
        for a in got {
            match a {
                Answer::Sent => self.retry = None,
                Answer::Failed(ids) => {
                    self.woke.retain(|id| !ids.contains(id));
                    unnote_woken(self.home, self.session, &ids);
                    unset_wake(self.home, self.session);
                    let d = backoff(self.retry.map(|(_, d)| d));
                    note(self.mode, &format!("[borg listen] the adapter could not send the wake in: tried again in {} s", d.as_secs()));
                    self.retry = Some((Instant::now() + d, d));
                }
            }
        }
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

    /// One look: the adapter's answers, a stop, a reinstall, the session, then the wake.
    fn pass(&mut self) -> Pass {
        // First: they live only in this process, and a stop or a re-exec ends it. A refusal dropped
        // there would leave its ids in `<session>.woke`, never to wake the session again.
        self.take_answers();
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
        // Read before any hold-off, so `idle` waits for the log's next change, not this one. The talk is
        // taken before the read, so a write in between moves it past what the waiter holds.
        let mark = mark(&self.path());
        if mark != self.seen {
            let talk = msg::talk_mark(self.home);
            if talk.is_none() || talk != self.talk {
                self.msgs = msg::read(self.home);
            }
            (self.seen, self.talk) = (mark, talk);
        }
        let now = learn::now_ms();
        if self.retry.is_some_and(|(at, _)| Instant::now() < at) || held_off(self.home, self.session, now, self.mode) {
            return Pass::Wait;
        }
        let (new, mine) = news(self.home, &self.msgs, &self.here, self.session, started, now, &self.woke);
        // Paused: a wake's turn could only have its tool calls refused; the resume moves the log.
        if new.is_empty() || paused(self.home, self.session, || Some(started), now) {
            return Pass::Wait;
        }
        let ids: Vec<&str> = new.iter().map(|m| m.id.as_str()).collect();
        self.cut_log();
        match say(self.mode, self.session, &summary(&new), &ids) {
            Ok(()) => {
                self.woke.extend(ids.iter().map(|id| (*id).to_string()));
                note_woken(self.home, self.session, &ids, &mine);
                // The wake's turn may fire no prompt hook (an adapter's own message): held off as a turn
                // until its stop, so the next message does not wake it again for what it shows.
                if listening(self.home, self.session) {
                    set_wake(self.home, self.session, now);
                }
                // A stream's wake went in only once its adapter says so (`take_answers`).
                if self.mode != Mode::Stream {
                    self.retry = None;
                }
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
    let mut w = Waiter::new(home, session, mode, lock);
    if mode == Mode::Stream {
        w.answers = Some(answers());
    }
    w.run()
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
        assert!(!held_off(&home, "s1", 0, Mode::Print));
        touch(&home, "prompt", "s1", 1_000);
        assert!(held_off(&home, "s1", 1_000 + TURN_IDLE_MS, Mode::Print), "a prompt starts the turn");
        assert!(!held_off(&home, "s1", 1_001 + TURN_IDLE_MS, Mode::Print), "a turn quiet that long no longer holds (an Esc)");
        touch(&home, "pre_tool", "s1", 2_000);
        assert!(held_off(&home, "s1", 2_000 + TOOL_MS, Mode::Print), "a tool call runs");
        assert!(!held_off(&home, "s1", 2_001 + TOOL_MS, Mode::Print), "one that never ended stops holding");
        touch(&home, "post_tool_failure", "s1", 3_000);
        assert_eq!(turn(&home, "s1"), Turn::Thinking(3_000), "an interrupted tool call ends it");
        touch(&home, "halted", "s1", 3_500);
        assert_eq!(turn(&home, "s1"), Turn::Over, "a halted turn is over");
        {
            let _s = stopping(&home, "stop", "s1", 4_000).unwrap();
            assert!(held_off(&home, "s1", 4_000, Mode::Print), "its stop runs");
            assert!(!held_off(&home, "s1", 4_001 + stop_ms(), Mode::Print), "a killed stop's mark expires");
        }
        assert!(!held_off(&home, "s1", 4_000, Mode::Print), "the stop's end drops its mark");
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
        touch(&home, "prompt", "s1", learn::now_ms() + TURN_IDLE_MS);
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
        touch(&home, "prompt", "s1", learn::now_ms() + TURN_IDLE_MS);
        let mut w = waiter(&home, Mode::Stream);
        message(&home, "s1", learn::now_ms(), "while the turn runs");
        assert_eq!(w.pass(), Pass::Wait);
        assert_eq!(w.seen, mark(&msg::path(&home)), "the hold-off saw this change");
        let t0 = Instant::now();
        w.idle();
        assert!(t0.elapsed() >= TICK, "idle sleeps at least a tick");
        let _ = std::fs::remove_dir_all(home);
    }

    /// An acknowledgement moves the log and not its talk: the waiter takes the new mark (so `idle` waits
    /// for the next change, not this one) without reading the log again; a tell makes it read.
    #[test]
    fn an_acknowledgement_does_not_make_the_waiter_read_the_log_again() {
        let home = temp("talk");
        opt_in(&home, "s1", "omp");
        touch(&home, "prompt", "s1", learn::now_ms() + TURN_IDLE_MS);
        let mut w = waiter(&home, Mode::Stream);
        let now = learn::now_ms();
        let a = message(&home, "s1", now, "status A");
        let other = message(&home, "s3", now, "for s3");
        assert_eq!(w.pass(), Pass::Wait, "held off: the turn runs");
        assert!(w.msgs.iter().any(|m| m.id == a.id));
        msg::ack(&home, "s3", &[&other.id], now + 1).expect("s3 was shown its message");
        // Same length and change time, another message: a read would see it.
        let log = msg::path(&home);
        let mtime = std::fs::metadata(&log).and_then(|m| m.modified()).expect("mtime");
        let from = Sender { by: a.by.clone(), name: a.name.clone(), harness: a.harness.clone(), node: a.node.clone() };
        let b = Message::new(msg::Kind::Tell, &from, &a.to, None, a.ms, "status B");
        let text = std::fs::read_to_string(&log).expect("the log");
        std::fs::write(&log, text.replacen(&serde_json::to_string(&a).expect("a line"), &serde_json::to_string(&b).expect("a line"), 1)).expect("swapped");
        std::fs::File::options().write(true).open(&log).and_then(|f| f.set_modified(mtime)).expect("mtime kept");
        assert_eq!(w.pass(), Pass::Wait);
        assert_eq!(w.seen, mark(&log), "the new mark taken");
        assert!(w.msgs.iter().any(|m| m.text == "status A"), "the log not read again");
        let c = message(&home, "s4", now + 2, "status C");
        assert_eq!(w.pass(), Pass::Wait);
        assert!(w.msgs.iter().any(|m| m.id == c.id) && w.msgs.iter().any(|m| m.text == "status B"), "a tell moves the talk: read");
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
    fn a_hold_is_the_sessions_own_95th_percentile_never_below_its_floor() {
        let f = TURN_IDLE_MS;
        assert_eq!(hold(&[], f), f, "no gaps: the floor");
        assert_eq!(hold(&[1_000; 40], f), f, "shorter gaps never shorten it");
        assert_eq!(hold(&[2 * f; 3], f), 2 * f, "a session that pauses longer holds longer");
        let mut outlier = vec![f + 1; 19];
        outlier.push(10 * f);
        assert_eq!(hold(&outlier, f), f + 1, "one outlier in 20 holds no longer");
        let mut mixed: Vec<i64> = (1..=100).collect();
        mixed.reverse();
        assert_eq!(hold(&mixed, 0), 95, "nearest rank over 100 values, order of arrival aside");
    }

    /// Each hook records the gap it ends; the holds follow the session's gaps, its latest `GAPS_KEPT`.
    #[test]
    fn the_hooks_record_the_gaps_they_end_and_the_holds_follow_them() {
        let home = temp("gaps");
        opt_in(&home, "s1", "omp");
        touch(&home, "prompt", "s1", 0);
        touch(&home, "pre_tool", "s1", 400);
        touch(&home, "post_tool", "s1", 1_400);
        drop(stopping(&home, "stop", "s1", 1_700));
        assert_eq!(gaps(&home, "s1"), (vec![400, 300], vec![1_000]), "prompt to tool call, the call, its end to the stop");
        touch(&home, "prompt", "s1", 5_000);
        touch(&home, "post_tool", "s1", 6_000);
        assert_eq!(gaps(&home, "s1"), (vec![400, 300], vec![1_000]), "a tool call's end with none started records nothing");
        assert_eq!(holds(&home, "s1", Mode::Stream), Holds { idle_ms: TURN_IDLE_MS, tool_ms: TOOL_MS }, "gaps under the floors: the floors");
        let (idle_ms, tool_ms) = (2 * TURN_IDLE_MS, 2 * TOOL_MS);
        for _ in 0..GAPS_KEPT {
            observe(&home, "s1", Gap::Idle, idle_ms);
            observe(&home, "s1", Gap::Tool, tool_ms);
        }
        let (idle, tool) = gaps(&home, "s1");
        assert_eq!((idle.len(), tool.len()), (GAPS_KEPT, GAPS_KEPT), "the latest of each kind kept");
        assert!(idle.iter().all(|g| *g == idle_ms), "the oldest went first: {idle:?}");
        assert_eq!(holds(&home, "s1", Mode::Stream), Holds { idle_ms, tool_ms }, "gaps past the floors: the session's own");
        assert_eq!(holds(&home, "s1", Mode::Queue).idle_ms, idle_ms, "codex's too");
        forget(&home, "s1");
        assert!(!file(&home, "s1", "gaps").exists(), "the session's end takes its gaps along");
        let _ = std::fs::remove_dir_all(home);
    }

    /// Short gaps never shorten a hold: after 19 quick tool calls, a running `cargo test` longer than
    /// all of them still holds every waiter's wake off (a wake released into it costs Claude Code a round
    /// and codex a turn) until the floor; a quiet turn likewise. A session whose own gaps run past the
    /// floor holds longer.
    #[test]
    fn a_learned_hold_lengthens_past_its_floor_and_never_shortens_below_it() {
        let home = temp("learned");
        opt_in(&home, "s1", "claude");
        for _ in 0..19 {
            observe(&home, "s1", Gap::Idle, 1_000);
            observe(&home, "s1", Gap::Tool, 3_000);
        }
        touch(&home, "prompt", "s1", 10_000);
        for mode in [Mode::Print, Mode::Stream, Mode::Queue] {
            let f = mode.idle_prior();
            assert!(held_off(&home, "s1", 10_000 + f, mode) && !held_off(&home, "s1", 10_001 + f, mode), "{mode:?}: a quiet turn, to the floor");
        }
        touch(&home, "pre_tool", "s1", 20_000);
        for mode in [Mode::Print, Mode::Stream, Mode::Queue] {
            assert!(held_off(&home, "s1", 20_000 + 60_000, mode), "{mode:?}: a tool call longer than every one before it");
            assert!(held_off(&home, "s1", 20_000 + TOOL_MS, mode) && !held_off(&home, "s1", 20_001 + TOOL_MS, mode), "{mode:?}: to the floor");
        }
        for _ in 0..20 {
            observe(&home, "s1", Gap::Tool, 2 * TOOL_MS);
        }
        assert!(held_off(&home, "s1", 20_000 + 2 * TOOL_MS, Mode::Print), "a session whose tool calls run longer holds longer");
        assert!(!held_off(&home, "s1", 20_001 + 2 * TOOL_MS, Mode::Print), "and past them stops holding: most likely an Esc");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn an_adapters_answer_is_one_json_line_of_message_ids() {
        assert_eq!(Answer::parse(r#"{"sent":["m000000000001"]}"#), Some(Answer::Sent));
        assert_eq!(Answer::parse(r#"{"failed":["m000000000001","junk"]}"#), Some(Answer::Failed(vec!["m000000000001".into()])), "only ids");
        assert_eq!(Answer::parse(r#"{"other":1}"#), None);
        assert_eq!(Answer::parse("not json"), None);
    }

    /// A wake the adapter could not send in is undone: its messages wake the session again once the
    /// backoff passed (injected here: the retry's time set to now), doubling while the harness refuses,
    /// and a wake that went in ends the backoff. The backoff alone keeps a refused wake from going out
    /// again at once.
    #[test]
    fn a_refused_wake_backs_off_and_its_messages_wake_the_session_again() {
        let home = temp("refused");
        opt_in(&home, "s1", "omp");
        let here = msg::node(&home);
        let m = message(&home, &format!("s1@{here}"), learn::now_ms(), "x");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut w = waiter(&home, Mode::Stream);
        w.answers = Some(rx);
        assert_eq!(w.pass(), Pass::Again, "woken");
        assert_eq!(woken(&home, "s1"), HashSet::from([m.id.clone()]));
        assert_ne!(turn(&home, "s1"), Turn::Over, "the wake's turn mark");
        tx.send(Answer::Failed(vec![m.id.clone()])).unwrap();
        assert_eq!(w.pass(), Pass::Wait, "backed off");
        assert!(woken(&home, "s1").is_empty() && w.woke.is_empty(), "undone");
        assert_eq!(turn(&home, "s1"), Turn::Over, "no turn started: its mark cleared");
        assert_eq!(w.retry.map(|(_, d)| d), Some(RETRY_MIN));
        w.retry = w.retry.map(|(_, d)| (Instant::now(), d));
        assert_eq!(w.pass(), Pass::Again, "woken again for the same message");
        assert_eq!(woken(&home, "s1"), HashSet::from([m.id.clone()]));
        tx.send(Answer::Failed(vec![m.id.clone()])).unwrap();
        assert_eq!(w.pass(), Pass::Wait);
        assert_eq!(w.retry.map(|(_, d)| d), Some(RETRY_MIN * 2), "refused again: doubled");
        w.retry = w.retry.map(|(_, d)| (Instant::now(), d));
        assert_eq!(w.pass(), Pass::Again);
        tx.send(Answer::Sent).unwrap();
        assert_eq!(w.pass(), Pass::Wait, "its turn holds the next wake off");
        assert_eq!(w.retry, None, "it went in: the backoff ends");
        let _ = std::fs::remove_dir_all(home);
    }

    /// A wake keeps what its turn mark replaced: a tool call that ran past its hold (so the wake went
    /// out) still records its gap when it ends, and a refused wake gives the session that tool call
    /// back, also when the waiter that takes the refusal re-executed since the wake (it remembers
    /// nothing of it: a new waiter here) and once a hook moved the mark, never.
    #[test]
    fn a_wake_keeps_the_tool_call_it_found_and_any_waiter_undoes_it() {
        let home = temp("wake-mark");
        opt_in(&home, "s1", "omp");
        let here = msg::node(&home);
        let now = learn::now_ms();
        let started = now - TOOL_MS - 1_000;
        set_turn(&home, "s1", true, started);
        let m = message(&home, &format!("s1@{here}"), now, "x");
        let mut w = waiter(&home, Mode::Stream);
        assert_eq!(w.pass(), Pass::Again, "the tool call ran past its hold: woken");
        assert!(matches!(turn(&home, "s1"), Turn::Thinking(_)), "the wake's turn holds the next wake off");
        drop(w);
        // The image that sent it is gone; the refusal reaches the next one.
        let (tx, rx) = std::sync::mpsc::channel();
        let mut w = waiter(&home, Mode::Stream);
        w.answers = Some(rx);
        tx.send(Answer::Failed(vec![m.id.clone()])).unwrap();
        assert_eq!(w.pass(), Pass::Wait, "backed off");
        assert_eq!(turn(&home, "s1"), Turn::Tool(started), "the tool call it found runs again");
        // Woken again; the call ends meanwhile: its gap is recorded from its own start.
        w.retry = w.retry.map(|(_, d)| (Instant::now(), d));
        assert_eq!(w.pass(), Pass::Again);
        touch(&home, "post_tool", "s1", now + 5_000);
        assert_eq!(gaps(&home, "s1").1, [now + 5_000 - started], "the call's gap");
        tx.send(Answer::Failed(vec![m.id.clone()])).unwrap();
        assert_eq!(w.pass(), Pass::Wait);
        assert!(matches!(turn(&home, "s1"), Turn::Thinking(ms) if ms == now + 5_000), "a hook moved it: kept");
        let _ = std::fs::remove_dir_all(home);
    }

    /// A pass takes the adapter's answers before anything that ends it: they live only in this
    /// process, and a refusal a stop or a re-exec dropped would leave its ids woken for, never to wake
    /// the session again. A test process can take neither a stop (a signal for every waiter in it) nor
    /// a re-exec; the session's end is the early return it reaches, and it comes after both.
    #[test]
    fn a_pass_takes_the_adapters_answers_before_anything_ends_it() {
        let home = temp("answers-first");
        opt_in(&home, "s1", "omp");
        let here = msg::node(&home);
        let m = message(&home, &format!("s1@{here}"), learn::now_ms(), "x");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut w = waiter(&home, Mode::Stream);
        w.answers = Some(rx);
        assert_eq!(w.pass(), Pass::Again, "woken");
        tx.send(Answer::Failed(vec![m.id.clone()])).unwrap();
        std::fs::remove_file(file(&home, "s1", "json")).unwrap();
        assert_eq!(w.pass(), Pass::Exit(0), "the session listens no more");
        assert!(w.woke.is_empty(), "the refusal was taken first");
        assert!(woken(&home, "s1").is_empty(), "its ids woken for no more");
        assert_eq!(w.retry.map(|(_, d)| d), Some(RETRY_MIN), "and backed off");
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
