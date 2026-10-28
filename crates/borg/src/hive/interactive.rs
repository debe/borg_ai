//! Interactive drones (phase g, docs/design/hive.md "Phase g"): sessions the engineer opens and makes
//! drones from inside them, one step each (`borg hive join [<goal>]`), never by a record of a directory,
//! so a session the engineer works in is never taken over. A joined session keeps its membership file
//! (`HiveSession`, with `interactive`), works in a worktree of its own, and when it has nothing to do its
//! stop asks it to start a waiter (`borg hive wait`, wait.rs), which costs no token until work arrives.
//!
//! - `hive/drones.local`: one line per join, rebind and leave of a drone of this node (`Entry`), never
//!   synced: which session each drone is now, what it serves and where its tree is.
//! - `<state>/<session>.hive-busy`: the engineer's own turn in the session (`busy`); no waiter hands
//!   work while it lasts.
//! - `<state>/<session>.hive-takeup`: a claim a waiter handed and no hook event of the session has
//!   taken up yet (`handed`, `untaken`; `touch` takes it up, wait.rs `takeup` releases it otherwise).
//! - `<state>/<session>.hive-missed`: a wake no turn took up (wait.rs `takeup` released its claim): the
//!   waiter claims nothing more until a hook event of the session comes (`missed`, `touch`).
//! - `<state>/<drone>.hive-waiter`: locked by the drone's running waiter (`waiting`); beside it
//!   `.hive-waiter-stale` while that waiter runs a borg that was replaced (`mark_stale`).
//!
//! Membership: `join` (and any claim path, `ensure_member`) writes the node's JOIN under the board lock
//! before any claim of the drone, and the node's LEAVE comes only with its last drone of the goal
//! (`leave`, the node reaper's `leave_noted` and `forget`). The beats and the rest of the node's goal work
//! are the node worker's (node.rs, phase g G4: the mesh daemon, or a waiter while none runs); the hooks
//! beat while the drone works only when no worker runs (`touch`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Board, Ev, Event, Here, HiveSession, board, bound, command, fail, goal, member, node, run, save, session_path, short, tasks_path, write_if};
use crate::harness::{DroneWake, Harness};
use crate::{hook, learn, queen};

/// The variables harnesses set to their session's id in every command their model runs and every hook
/// they run (`Caps::session_env`), each with its harness, in `harness::ALL`'s order: Claude Code's
/// (`CLAUDE_CODE_SESSION_ID`) equals the hook input's `session_id` and follows `/clear`, but under
/// `--continue` or `--resume` without an id it may be the startup id instead (its env-vars docs), so
/// the join's own PostToolUse moves the drone to the id its hooks carry (`adopt`); omp's (`PI_SESSION_ID`) is its session manager's id, the one its extension sends; codex's
/// (`CODEX_THREAD_ID`, 0.153.2, also inside its sandbox) is the thread id its hooks carry as `session_id`
/// and `codex queue --thread` takes, which the join's own PostToolUse checks (`mismatch`). What binds a
/// session from inside it: only the session that runs `borg hive join` becomes a drone. opencode gives
/// its shell no session id: its plugin binds the session that ran the join (`BINDER_ENV`).
pub fn session_vars() -> Vec<(&'static str, &'static str)> {
    crate::harness::ALL.iter().filter_map(|h| h.caps().session_env.map(|v| (v, h.name()))).collect()
}

/// Set by a harness plugin that binds the session itself (opencode's, in its own process, so every
/// command its model runs has it): a `borg hive join` with no session id in its environment is then
/// the plugin's to bind (`borg hive join --bind --session S`, from its `tool.execute.after` for the
/// session that ran it), not refused.
pub const BINDER_ENV: &str = "BORG_HIVE_BINDER";

/// How an interactive drone of a harness gets its next task once idle, which its stop follows
/// (`after_stop`; the harness's `Caps::drone_wake`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wake {
    /// Claude Code: the stop asks the model to start `borg hive wait` in the background, whose end
    /// starts the next turn.
    Model,
    /// omp, opencode: the adapter keeps the drone's waiter itself, from the join for the session's life
    /// (phase g correction 12: `borg hive wait --stream`), and wakes the session by its own path when the
    /// waiter hands it work (omp `pi.sendMessage` with `nextTurn` and `triggerTurn`, opencode 1.x
    /// `session.promptAsync`, 2.x `session.prompt` with `delivery: "queue"`). Its stops never ask the
    /// model to start a wait. `hive`: the hive's stop said something itself (not only a conscience's
    /// questions).
    Plugin { hive: bool },
    /// codex: no hook can wake its idle session (0.153: a Stop hook only continues a turn that runs, and
    /// a finished async hook starts none), so the stop starts a waiter detached from the hook (`borg hive
    /// wait --queue`, `spawn_waiter`), which wakes the session with `codex queue --thread <session>
    /// --message <task>` (its own path since 0.149.0; an idle TUI takes the message up as a new turn,
    /// checked with 0.153.2 against a local mock model). Its hooks run as children of the harness
    /// process, so a hook's nearest harness ancestor is the drone's harness (`hook_pid`).
    Queue { hive: bool },
}

impl Wake {
    /// How harness `name`'s drones are woken (its `Caps::drone_wake`; an unknown name's: the model's
    /// wait); `hive` as in `Plugin`.
    pub(crate) fn of(name: &str, hive: bool) -> Self {
        match crate::harness::by_name(name).map(|h| h.caps().drone_wake) {
            Some(DroneWake::Plugin) => Wake::Plugin { hive },
            Some(DroneWake::Queue) => Wake::Queue { hive },
            Some(DroneWake::Model) | None => Wake::Model,
        }
    }

    /// Whether harness `name`'s drones are woken through its CLI (`Queue`).
    pub(crate) fn queues(name: &str) -> bool {
        matches!(Wake::of(name, false), Wake::Queue { .. })
    }
}

/// The harness process a hook event of harness `name` runs under: the pid its plugin sent
/// (`event_pid`), or, for a harness that runs its hooks as its own children (`Wake::Queue`: codex runs
/// each as `sh -c` under its process), the hook's nearest harness ancestor (`harness_proc`) when
/// it is that harness by name (a hook run by hand from another harness's shell names none). So a codex
/// drone whose join ran in codex's sandbox (a pid namespace of its own, where the walk finds no harness)
/// is tied to codex's process from its first hook on (`touch`, `own_pid`). None for every other harness
/// that sends none.
pub(crate) fn hook_pid(name: &str, input: &Value) -> Option<u32> {
    let own = || Wake::queues(name).then(harness_proc).flatten().filter(|(_, _, comm)| comm.starts_with(name)).map(|(pid, _, _)| pid);
    event_pid(input).or_else(own)
}

/// The registry of this node's interactive drones, beside the goals (`hive/drones.local`): never synced.
pub const REGISTRY: &str = "drones.local";

/// How long a claim a waiter handed waits for a hook event of its session before it is released
/// (`takeup`, phase g correction 10): a harness process that exits without starting the turn, or a turn
/// that never reaches a hook, takes nothing up. `BORG_HIVE_TAKEUP_MS` sets it for tests.
pub const TAKEUP: Duration = Duration::from_secs(60);
/// How long an engineer's turn that never reached its stop (an Esc, a crash) still counts as busy after
/// its last tool call ended (`busy`): a tool call still running never expires it (phase g correction 8).
pub const BUSY_IDLE_MS: i64 = 5 * 60_000;
/// `BUSY_IDLE_MS` for a codex drone (`Wake::Queue`): its wake waits in codex's queue behind any turn
/// that still runs and only the turn it starts takes it up (`touch`), so a turn that is quiet this long
/// no longer holds its waiter off. An Esc codex's `Interrupt` hook did not report (an install from
/// before that hook, or the hook not trusted yet in `/hooks`) delays the drone's work by this, not by
/// `BUSY_IDLE_MS`, with nothing for the engineer to do.
pub const QUEUE_BUSY_IDLE_MS: i64 = 60_000;
/// The longest the stop's own claim of the next task waits for it to be final across nodes (phase f
/// F5) before the stop hands it to the waiter, which waits on with no cap (phase g correction 9): no
/// Stop spinner waits on peers.
pub const HAND_CAP: Duration = Duration::from_secs(5);

/// The session id the harness gave the process this runs in (`session_vars`), if any.
pub fn env_session() -> Option<String> {
    env_session_of().map(|(s, _)| s)
}

/// The session id the harness gave the process this runs in, and that harness (`session_vars`). A
/// harness started inside another's shell (omp in a Claude Code session, opencode in either) inherits the
/// outer one's variable too: when more than one is set, or a binding plugin's (`BINDER_ENV`) is, the
/// nearest harness process (`harness_proc`) decides which session this is, so a join never binds the
/// outer session; opencode's: none (its plugin binds it). Else the first set, in `session_vars`' order.
pub fn env_session_of() -> Option<(String, &'static str)> {
    let var = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let mut set: Vec<(String, &'static str)> = session_vars().into_iter().filter_map(|(k, h)| var(k).map(|v| (v, h))).collect();
    let binder = var(BINDER_ENV).is_some();
    if set.len() > 1 || (binder && !set.is_empty()) {
        let comm = harness_proc().map(|p| p.2).unwrap_or_default();
        if let Some(i) = set.iter().position(|(_, h)| comm.starts_with(h)) {
            return Some(set.swap_remove(i));
        }
        if binder && comm.contains("opencode") {
            return None;
        }
    }
    set.into_iter().next()
}

/// Whether a binding plugin runs this process's harness (`BINDER_ENV`): a join with no session id of
/// its own is that plugin's to bind.
pub fn plugin_binds() -> bool {
    std::env::var_os(BINDER_ENV).is_some_and(|v| !v.is_empty())
}

/// The interactive drone this process runs for: the harness's session (`env_session`) when it is bound
/// as one (`borg hive join`).
pub fn env_drone() -> Option<(String, HiveSession)> {
    let s = env_session()?;
    bound(&s).filter(|hs| hs.interactive).map(|hs| (s, hs))
}

/// Whether a process in `cwd` runs for an interactive drone (mod.rs `drone`): its harness session is
/// bound as one, or it runs inside a drone's worktree (`.worktrees/hive-i-*`). Such a process never runs
/// the engineer's commands (`approve`, `integrate`, `score`, a plan's answer). One failed open for a
/// session that never joined.
pub fn drone_here(cwd: &str) -> bool {
    env_drone().is_some() || cwd.contains("/.worktrees/hive-i-")
}

pub fn registry_path(home: &Path) -> PathBuf {
    home.join("hive").join(REGISTRY)
}

/// One line of `drones.local`: drone `drone` is now session `session` (`by` join or rebind), or left
/// (`leave`). It serves `goal`, or every open goal of the collective in its repo (`any`), and works in
/// `worktree`. `pid`, `pid_start`: the harness process that ran the join (`harness_pid`), which a session
/// after `/clear` is rebound by; 0 when unknown. `pid_ns`: the pid namespace that pid is of (`pid_ns`),
/// empty on a line from before it was recorded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub v: u8,
    pub ms: i64,
    pub drone: String,
    pub session: String,
    #[serde(default)]
    pub harness: String,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub pid_start: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pid_ns: String,
    #[serde(default)]
    pub any: bool,
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub worktree: String,
    pub by: String,
}

impl Entry {
    /// Whether the drone serves goal `goal`.
    pub fn serves(&self, goal: &str) -> bool {
        self.any || self.goal == goal
    }
}

/// The drones of this node now: each drone's newest line, unless it is a leave. None when the store has
/// no registry (one failed open).
pub fn live(home: &Path) -> Vec<Entry> {
    let mut newest: std::collections::BTreeMap<String, Entry> = std::collections::BTreeMap::new();
    for e in super::read_lines::<Entry>(&registry_path(home)) {
        if newest.get(&e.drone).is_none_or(|n| n.ms <= e.ms) {
            newest.insert(e.drone.clone(), e);
        }
    }
    newest.into_values().filter(|e| e.by != "leave").collect()
}

/// The live drone of this node that serves `goal` and is still bound (its session's membership file):
/// what refuses a runner of the goal on this node (phase g correction 3). A drone of every open goal
/// serves only those open to its repo (`served`'s scope, `goal::in_scope`): a goal of other repos runs.
pub fn serving(home: &Path, goal: &str) -> Option<Entry> {
    let in_scope = |e: &Entry| !e.any || e.worktree.is_empty() || goal::load(home, goal).is_ok_and(|g| goal::in_scope(&g, &queen::repo_name(&e.worktree)));
    live(home).into_iter().find(|e| e.serves(goal) && in_scope(e) && session_path(&e.session).exists())
}

fn record(home: &Path, e: &Entry) -> std::io::Result<()> {
    super::append_line(&registry_path(home), e)
}

/// `session`'s drone name: `i-` and 6 hex of its node and session, stable for the session's life and
/// kept across a rebind, so every claim and bar names one drone.
pub fn drone_name(home: &Path, session: &str) -> String {
    format!("i-{}", &blake3::hash(format!("{}\n{session}", node(home)).as_bytes()).to_hex()[..6])
}

/// A process's command name, parent and start time (`/proc/<pid>/stat` on Linux, `proc_pidinfo` on
/// macOS, None elsewhere).
#[cfg(target_os = "macos")]
fn proc_stat(pid: u32) -> Option<(String, u32, u64)> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`, a plain C struct of that size.
    let n = unsafe { libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
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
    Some((s[l + 1..r].to_string(), rest.get(1)?.parse().ok()?, rest.get(19)?.parse().ok()?))
}

/// Processes between a harness and borg: its shell, a wrapper, a sandbox (Claude Code's bubblewrap), borg
/// itself.
const WRAPPERS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "fish", "ksh", "mksh", "tcsh", "csh", "nu", "xonsh", "elvish", "borg", "env", "timeout", "nice", "nohup", "setsid", "script",
    "bwrap",
];

/// The harness process this command or hook runs under: the nearest ancestor that is no shell, wrapper
/// or borg (`WRAPPERS`), with its start time, so a reused pid is no match. None off Linux and macOS or when the
/// walk finds none: a session is then rebound by nothing but its own id.
pub fn harness_pid() -> Option<(u32, u64)> {
    harness_proc().map(|(pid, start, _)| (pid, start))
}

/// `harness_pid` with the harness process's command name (`claude`, `omp`, `opencode`, ...).
fn harness_proc() -> Option<(u32, u64, String)> {
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..16 {
        let (comm, ppid, start) = proc_stat(pid)?;
        if !WRAPPERS.contains(&comm.as_str()) {
            return Some((pid, start, comm));
        }
        if ppid <= 1 {
            return None;
        }
        pid = ppid;
    }
    None
}

/// Whether this node's membership of `b`'s goal is open: a join newer than its leave (board.rs
/// `Board::members`).
pub fn member_open(b: &Board, here: &str) -> bool {
    b.members.get(here).is_some_and(|(join, leave)| join > leave)
}

/// This node joins `goal` (`board::JOIN`) unless its membership is open already, decided and written
/// under the board lock (`write_if`), so the join lands before any claim of a drone it is written for
/// (phase g correction 1: a claim before its node's join reads as gone on every peer, board.rs
/// `Local::left`). Never a second join while it is open: a later join reads as a new run, the end of
/// every claim the node held. Whether it wrote.
pub(crate) fn ensure_member(home: &Path, goal: &str) -> std::io::Result<bool> {
    let here = node(home);
    let line = || vec![Event::new(Ev::Heartbeat, "", "runner", learn::now_ms()).with_note(board::JOIN)];
    write_if(home, goal, |events| (!member_open(&board::fold_on(events, &here), &here)).then(line)).map(|w| w.is_some())
}

/// Whether a `borg debug hive run` of `goal` runs on this node (it holds the goal's run lock, run.rs).
pub fn runner_runs(home: &Path, goal: &str) -> bool {
    let path = super::dir(home, goal).join("run.lock");
    let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) else { return false };
    matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

/// The goals an interactive drone of `hs` in `repo` serves now: its goal, or (`any`) every goal of the
/// collective open to this repo and not finished, oldest first.
pub fn served(home: &Path, hs: &HiveSession, repo: &str) -> Vec<goal::Goal> {
    if !hs.any {
        return goal::load(home, &hs.goal).into_iter().collect();
    }
    let here = node(home);
    let mut goals: Vec<goal::Goal> = goal::list(home)
        .into_iter()
        .filter_map(|id| goal::load(home, &id).ok())
        .filter(|g| goal::in_scope(g, repo))
        .filter(|g| !board::fold_on(&board::read(&tasks_path(home, &g.id)), &here).finished_for_good(&here, learn::now_ms()))
        .collect();
    goals.sort_by_key(|g| (g.created_ms, g.id.clone()));
    goals
}

/// The drone's own worktree, `<repo>/.worktrees/hive-i-<drone>`, detached at HEAD of the repo `cwd` is in
/// (reused when it is one already), kept out of the engineer's `git status` (run.rs `exclude_worktrees`).
/// Empty when `cwd` is in no git repo: the drone works where its session runs.
fn worktree(cwd: &str, drone: &str) -> Result<String, String> {
    let Some(repo) = run::repo_of(Path::new(cwd)) else { return Ok(String::new()) };
    let path = repo.join(".worktrees").join(format!("hive-{drone}"));
    if run::is_worktree(&path) {
        return Ok(path.display().to_string());
    }
    run::exclude_worktrees(&repo)?;
    let p = path.display().to_string();
    run::git(&repo, &["worktree", "add", "--detach", &p, "HEAD"], Duration::from_secs(60)).ok_or_else(|| {
        // A sandbox that keeps `.git` read-only (codex's workspace-write) is said as such (`sandboxed`).
        let git = repo.join(".git");
        match read_only(&git) {
            true => format!("{}: Read-only file system", git.display()),
            false => format!("git worktree add --detach {p} HEAD failed"),
        }
    })?;
    Ok(p)
}

/// Whether `p` is on a file system this process may not write (access(2) says EROFS: a read-only mount,
/// as a sandbox binds one).
fn read_only(p: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(p.as_os_str())) else { return false };
    // SAFETY: access(2) reads the NUL-terminated path it is given and nothing else.
    let r = unsafe { libc::access(c.as_ptr(), libc::W_OK) };
    r != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EROFS)
}

/// `borg hive join [<goal>]` inside a session: the harness's session (`env_session`, or the session a
/// binding plugin names, `borg hive join --bind --session S`) becomes an
/// interactive drone of `goal`, or of every open goal of this collective in this repo without one. Writes
/// its membership (`interactive`, its worktree) and its registry line, then, for each goal it serves now,
/// the node's join under the lock. Refused for a session that is a drone already, and while a `borg hive
/// run` of a goal it would serve runs on this node (phase g correction 3): looked for after the drone is
/// written, as the run looks for drones under its lock after taking it, so of a join and a run that race
/// one always sees the other.
pub(super) fn join(here: &Here, goal_id: Option<&str>, session: &str) -> i32 {
    if let Some(id) = goal_id
        && let Err(e) = goal::load(&here.home, id)
    {
        return fail(e);
    }
    if let Some(hs) = bound(session) {
        let what = if hs.any { "every open goal".to_string() } else { format!("goal {}", hs.goal) };
        return fail(format!("{} is a drone of {what} already (`borg hive leave` first)", short(session)));
    }
    let repo = queen::repo_name(&here.cwd);
    let probe = HiveSession { goal: goal_id.unwrap_or_default().into(), any: goal_id.is_none(), interactive: true, ..HiveSession::default() };
    let goals = served(&here.home, &probe, &repo);
    let drone = drone_name(&here.home, session);
    let tree = match worktree(&here.cwd, &drone) {
        Ok(t) => t,
        Err(e) => return fail(sandboxed(e)),
    };
    let first = goals.first().map(|g| g.id.clone()).unwrap_or_default();
    let cursor = std::fs::metadata(tasks_path(&here.home, &first)).map_or(0, |m| m.len());
    let (pid, pid_start) = harness_pid().unwrap_or_default();
    let hs = HiveSession { goal: first, cursor, drone: drone.clone(), worktree: tree.clone(), pid, ..probe };
    if let Err(e) = steer_write(session, &hs) {
        return fail(sandboxed(e.to_string()));
    }
    // The plugin that binds names its harness (`BORG_HARNESS`); else the harness whose variable named the
    // session.
    let harness = crate::harness::named_current()
        .map(|h| h.name())
        .or_else(|| env_session_of().filter(|(s, _)| s == session).map(|(_, h)| h))
        .unwrap_or("claude")
        .to_string();
    let e = Entry {
        v: 1,
        ms: here.now,
        drone: drone.clone(),
        session: session.into(),
        harness: harness.clone(),
        pid,
        pid_start,
        pid_ns: pid_ns(),
        any: hs.any,
        goal: goal_id.unwrap_or_default().into(),
        worktree: tree.clone(),
        by: "join".into(),
    };
    if let Err(e) = record(&here.home, &e) {
        super::unbind(session);
        return fail(e);
    }
    if let Some(g) = goals.iter().find(|g| runner_runs(&here.home, &g.id)) {
        super::unbind(session);
        let _ = record(&here.home, &Entry { v: 1, ms: here.now, drone, session: session.into(), by: "leave".into(), ..Entry::default() });
        return fail(format!(
            "a `borg debug hive run {}` runs on this node: a node runs a runner or interactive drones for a goal, never both (its join and leave would end their claims)",
            g.id
        ));
    }
    for g in &goals {
        if let Err(e) = ensure_member(&here.home, &g.id) {
            // No half-made drone: it leaves as it would (its registry leave, the node's leave of goals
            // no other drone of it serves).
            let _ = leave_quietly(here, session, &hs);
            return fail(format!("could not join goal {}: {e}", g.id));
        }
    }
    let what = match goal_id {
        Some(id) => format!("hive goal {id}"),
        None if goals.is_empty() => "every open hive goal of this repo (none is open now)".into(),
        None => format!("every open hive goal of this repo ({})", goals.iter().map(|g| g.id.as_str()).collect::<Vec<_>>().join(", ")),
    };
    let tree = if tree.is_empty() { String::new() } else { format!(" It works in its own worktree {tree}.") };
    // No harness process found (a sandbox's pid namespace, not Linux): nothing ties a later id to it.
    // A codex drone's first hook names codex's process (`hook_pid`).
    let clear = if pid == 0 && !Wake::queues(&harness) {
        " Its harness process was not found, so a `/clear` leaves the drone behind: `borg hive leave` before one, and join again after."
    } else {
        ""
    };
    // A plugin's waiter starts at the join, inside the turn that ran it, whose prompt came before the
    // session was a drone: that turn is the engineer's until its stop (`busy`), so no wake lands in it.
    if matches!(Wake::of(&harness, false), Wake::Plugin { .. }) {
        set_busy(session, false, here.now, BUSY_IDLE_MS);
    }
    let wait = match Wake::of(&harness, false) {
        Wake::Plugin { .. } => "When nothing is open it ends its turn and its harness's plugin waits for work for it at no token cost",
        Wake::Queue { .. } => {
            "When nothing is open it ends its turn and borg waits for work for it at no token cost, then starts its next turn with `codex queue`"
        }
        Wake::Model => "When nothing is open its turn ends with a wait for work that costs no tokens",
    };
    println!(
        "{} joined {what} as drone {drone}.{tree} {wait}, and it stays a drone: leaving the hive is only on the engineer's request.{clear}",
        short(session)
    );
    0
}

/// A join's error `e`, with the step that gets past it when a sandbox made the write fail: codex's
/// `workspace-write` sandbox keeps the repo's `.git` (where the drone's worktree is registered) and
/// anything outside the workspace (borg's store) read-only to the model's commands (checked with codex
/// 0.153.2: `.git/info/exclude: Read-only file system`), while its hooks run outside it.
fn sandboxed(e: String) -> String {
    match e.contains("Read-only file system") {
        true => format!(
            "{e}. This command runs in a sandbox that keeps the repo's .git or borg's store read-only (codex's workspace-write does): run `borg hive join` again outside the sandbox (in codex: with escalated permissions, which the engineer approves)"
        ),
        false => e,
    }
}

fn steer_write(session: &str, hs: &HiveSession) -> std::io::Result<()> {
    crate::steer::rewrite(&session_path(session), &serde_json::to_string(hs).unwrap_or_default())
}

/// `borg hive leave` of an interactive drone (`leave_quietly`), said.
pub(super) fn leave(here: &Here, session: &str, hs: &HiveSession) -> i32 {
    if let Err(e) = leave_quietly(here, session, hs) {
        return fail(e);
    }
    let what = if hs.any { "the hive".to_string() } else { format!("hive goal {}", hs.goal) };
    println!("{} left {what}; it is a plain session again", short(session));
    0
}

/// An interactive drone leaves: its claim released, its waiter stopped (SIGTERM, it says nothing more;
/// not when the waiter is the one leaving), its registry leave, and for each goal no other live drone of
/// this node serves, the node's leave (`board::LEAVE`): only with its last drone (phase g correction 2).
/// Its worktree stays. One leave at a time per drone (its leave lock): a session's end runs its SessionEnd
/// (`ended`) and its waiter's `stopped` (wait.rs) at once, and the one that comes second finds the session
/// unbound and writes nothing, so a drone leaves with one registry leave and one node leave at most.
pub(super) fn leave_quietly(here: &Here, session: &str, hs: &HiveSession) -> std::io::Result<()> {
    leave_noted(here, session, hs, "left the goal")
}

/// `leave_quietly`, its claim released with `note` (the node's reaper: `its session is gone`, node.rs).
pub(super) fn leave_noted(here: &Here, session: &str, hs: &HiveSession, note: &str) -> std::io::Result<()> {
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(hook::state_file(&hs.drone).with_extension("hive-leave"))?;
    lock.lock()?;
    if bound(session).is_none() {
        return Ok(()); // left already
    }
    let here_node = node(&here.home);
    let repo = queen::repo_name(if hs.worktree.is_empty() { &here.cwd } else { &hs.worktree });
    let goals = served(&here.home, hs, &repo);
    // The waiter is stopped first (it ends on the signal, saying nothing), then the session unbound, then
    // its claim released, decided under the board lock: a waiter's claim decided under that lock after
    // the unbind finds the session no drone (mod.rs `claim`), and one decided before it has landed by the
    // time the release takes the lock and is released, so no claim outlives a leave.
    stop_waiter(&hs.drone);
    super::unbind(session);
    for g in &goals {
        super::write_if(&here.home, &g.id, |events| {
            let now = learn::now_ms();
            let b = board::fold_on(events, &here_node);
            board::holding(&b, &here_node, session, now).map(|t| vec![Event::new(Ev::Released, &t.task.id, session, now).with_note(note)])
        })?;
    }
    clear_busy(session);
    let _ = std::fs::remove_file(takeup_path(session));
    let _ = std::fs::remove_file(missed_path(session));
    record(&here.home, &Entry { v: 1, ms: here.now, drone: hs.drone.clone(), session: session.into(), by: "leave".into(), ..Entry::default() })?;
    let others = live(&here.home);
    for g in &goals {
        let b = board::fold_on(&board::read(&tasks_path(&here.home, &g.id)), &here_node);
        if member_open(&b, &here_node) && !others.iter().any(|o| o.serves(&g.id)) {
            member(&here.home, &g.id, board::LEAVE)?;
        }
    }
    Ok(())
}

/// Registry line `e`, whose session is no drone any more (its membership file gone without a leave of
/// the registry), is closed as a leave closes it: its registry leave, and the node's leave of each goal
/// no other live drone of this node serves (the node reaper, node.rs `reap`).
pub(super) fn forget(home: &Path, e: &Entry, now: i64) {
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(hook::state_file(&e.drone).with_extension("hive-leave"));
    let Ok(lock) = lock else { return };
    if lock.lock().is_err() || entry_of(home, &e.drone).is_none_or(|now_e| now_e != *e) {
        return; // left, or moved, meanwhile
    }
    let here_node = node(home);
    let probe = HiveSession { goal: e.goal.clone(), any: e.any, interactive: true, ..HiveSession::default() };
    let goals = served(home, &probe, &queen::repo_name(&e.worktree));
    if record(home, &Entry { v: 1, ms: now, drone: e.drone.clone(), session: e.session.clone(), by: "leave".into(), ..Entry::default() }).is_err() {
        return;
    }
    let others = live(home);
    for g in &goals {
        let b = board::fold_on(&board::read(&tasks_path(home, &g.id)), &here_node);
        if member_open(&b, &here_node) && !others.iter().any(|o| o.serves(&g.id)) {
            let _ = member(home, &g.id, board::LEAVE);
        }
    }
}

// ---------------------------------------------------------------- busy, take-up, waiter

fn busy_path(session: &str) -> PathBuf {
    hook::state_file(session).with_extension("hive-busy")
}

fn takeup_path(session: &str) -> PathBuf {
    hook::state_file(session).with_extension("hive-takeup")
}

/// The lock file of `drone`'s waiter (wait.rs): held while it runs.
pub(crate) fn waiter_path(drone: &str) -> PathBuf {
    hook::state_file(drone).with_extension("hive-waiter")
}

/// The engineer's turn in `session` started or moved on at `now`: `tool` whether a tool call runs,
/// `idle` how long after `now` with no tool running it still holds the waiter off (`busy`).
fn set_busy(session: &str, tool: bool, now: i64, idle: i64) {
    let _ = std::fs::write(busy_path(session), format!("{} {now} {idle}", u8::from(tool)));
}

pub(crate) fn clear_busy(session: &str) {
    let _ = std::fs::remove_file(busy_path(session));
}

/// Whether `session` is in a turn of the engineer's at `now` (phase g correction 8): from its prompt to
/// its stop, and once its stop never came, until `BUSY_IDLE_MS` after its last tool call ended; a tool
/// call still running (a long build, a test suite) keeps it busy however long. No waiter hands it work
/// meanwhile: the handoff never lands inside the engineer's turn. A codex drone's quiet turn holds it
/// off `QUEUE_BUSY_IDLE_MS` only (its wake queues behind the turn, `touch`).
pub fn busy(session: &str, now: i64) -> bool {
    busy_for(session, now, None)
}

/// Whether `session`'s turn may still run at `now` by the longest measure (`BUSY_IDLE_MS`, whatever the
/// harness): a handed wake waits its take-up out meanwhile (wait.rs `takeup`), since a codex wake waits
/// in codex's queue behind such a turn.
pub(crate) fn turn_may_run(session: &str, now: i64) -> bool {
    busy_for(session, now, Some(BUSY_IDLE_MS))
}

fn busy_for(session: &str, now: i64, idle: Option<i64>) -> bool {
    let Ok(text) = std::fs::read_to_string(busy_path(session)) else { return false };
    let mut it = text.split_whitespace();
    let (tool, ms) = (it.next() == Some("1"), it.next().and_then(|m| m.parse::<i64>().ok()).unwrap_or(i64::MIN));
    let own = it.next().and_then(|m| m.parse::<i64>().ok()).unwrap_or(BUSY_IDLE_MS);
    tool || now.saturating_sub(ms) <= idle.unwrap_or(own)
}

fn stopping_path(session: &str) -> PathBuf {
    hook::state_file(session).with_extension("hive-stopping")
}

/// A stop of an interactive drone's session runs (mod.rs `around_bound`): marked from its start until
/// it returns (the guard's drop), or until `until` should the harness kill the hook first. Meanwhile
/// the drone's waiter claims and hands nothing (`held_off`): the stop's own done and handover decide
/// what the drone gets next, so a done the waiter sees on the board never makes it claim beside the
/// stop's handover, and a claim the stop made is never handed by the waiter before the stop marks it
/// shown (phase g, no double delivery).
pub(crate) struct Stopping(PathBuf);

impl Stopping {
    pub(crate) fn mark(session: &str, until: i64) -> Self {
        let path = stopping_path(session);
        let _ = std::fs::write(&path, until.to_string());
        Self(path)
    }
}

impl Drop for Stopping {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Whether a stop of `session` runs at `now` (`Stopping`).
pub fn stopping(session: &str, now: i64) -> bool {
    std::fs::read_to_string(stopping_path(session)).ok().and_then(|t| t.trim().parse::<i64>().ok()).is_some_and(|until| now <= until)
}

/// Whether `session`'s waiter holds off at `now`: the engineer's turn runs (`busy`) or its stop does
/// (`stopping`).
pub fn held_off(session: &str, now: i64) -> bool {
    busy(session, now) || stopping(session, now)
}

/// The engineer's turn in `session` was halted with no stop (an Esc, a provider error, a permission
/// rejected without words: opencode's plugin sends `halted`, hook.rs): it is over, so the drone's
/// waiter hands work again at once, not `BUSY_IDLE_MS` later (phase g correction 12). Nothing for a
/// session that is no interactive drone, after one failed open.
pub fn halted(session: &str) {
    if bound(session).is_some_and(|hs| hs.interactive) {
        clear_busy(session);
    }
}

/// A waiter handed `key` (`task@claim`) of `goal` to `session` at `now`: open until a hook event of the
/// session takes it up (`touch`).
pub(crate) fn handed(session: &str, goal: &str, key: &str, now: i64) {
    let _ = std::fs::write(takeup_path(session), format!("{goal} {key} {now}"));
}

/// The claim handed to `session` not taken up yet: (goal, `task@claim`, when it was handed). The hand
/// time, not the claim's stamp, starts its take-up wait (wait.rs `takeup`): a claim the stop made and the
/// waiter handed later gets the whole wait.
pub(crate) fn untaken(session: &str) -> Option<(String, String, i64)> {
    let text = std::fs::read_to_string(takeup_path(session)).ok()?;
    let mut it = text.split_whitespace();
    let (g, k) = (it.next()?, it.next()?);
    Some((g.to_string(), k.to_string(), it.next().and_then(|m| m.parse().ok()).unwrap_or(0)))
}

/// The note of a handed claim's release when no turn of its session took it up (wait.rs `takeup`):
/// monitoring reads a wake missed from it.
pub const NOT_TAKEN_UP: &str = "not taken up: no hook event of the session followed its wake";

/// A turn of `session` took up `key` (`task@claim`) of `goal`, handed at `handed`, at `now` (`touch`):
/// one `taken` line in the goal's local confirms.jsonl, the wake's latency from its hand to now
/// (`waited_ms`), both on this node's clock, so monitoring shows each drone's wake latency over time
/// (phase g G6). Never from the claim's stamp: a hybrid stamp runs up to `board::AHEAD_MS` past this
/// clock once the board holds a peer's line stamped ahead, and would show a wake shorter than it was. Only
/// while the session still holds that claim on the board: a wake wait.rs `takeup` released meanwhile
/// is a missed one, never also a taken one. A hand of no time (a take-up file from before) writes no line.
/// One line per wake: the take-up file is removed with it.
fn taken_up(home: &Path, goal: &str, key: &str, handed: i64, session: &str, now: i64) {
    let Some((task, claim)) = key.rsplit_once('@') else { return };
    let Ok(claim) = claim.parse::<i64>() else { return };
    if handed <= 0 || claim == 0 {
        return;
    }
    let b = board::fold_on(&board::read(&tasks_path(home, goal)), &node(home));
    if !board::holding(&b, &node(home), session, now).is_some_and(|s| s.task.id == task && s.live(now).is_some_and(|l| l.2 == claim)) {
        return;
    }
    let waited = (now - handed).max(0);
    let line = super::Confirm { v: 1, ms: now, task: task.into(), session: session.into(), peers: 0, waited_ms: waited as u64, outcome: super::TAKEN.into() };
    let _ = super::append_line(&super::confirms_path(home, goal), &line);
}

/// What a wake that hands a claim of `goal` starts with (wait.rs `deliver`): the prompt a codex wake's
/// own turn starts with (`touch`).
pub(crate) fn arrived(goal: &str) -> String {
    format!("[borg hive {goal}] Work arrived.")
}

fn missed_path(session: &str) -> PathBuf {
    hook::state_file(session).with_extension("hive-missed")
}

/// A wake to `session` was not taken up by any turn and its claim was released (wait.rs `takeup`): its
/// waiter claims nothing more until a hook event of the session comes (`touch` ends it), so a thread no
/// harness runs any more (a codex TUI moved to another thread, which `codex queue` still takes messages
/// for) is not handed the same task again and again, each wake holding it `TAKEUP` from every other drone.
pub(crate) fn mark_missed(session: &str, now: i64) {
    let _ = std::fs::write(missed_path(session), now.to_string());
}

/// Takes back `mark_missed` when the release it went with was never written (wait.rs `takeup`).
pub(crate) fn unmark_missed(session: &str) {
    let _ = std::fs::remove_file(missed_path(session));
}

/// Whether `session`'s last wake was missed (`mark_missed`) and no hook event of it came since.
pub fn missed(session: &str) -> bool {
    missed_path(session).exists()
}

/// How long a handed claim waits for its take-up (`TAKEUP`, or `BORG_HIVE_TAKEUP_MS`).
pub(crate) fn takeup_wait() -> Duration {
    std::env::var("BORG_HIVE_TAKEUP_MS").ok().and_then(|v| v.trim().parse().ok()).map_or(TAKEUP, Duration::from_millis)
}

/// Whether `drone`'s waiter runs: its lock is held.
pub fn waiting(drone: &str) -> bool {
    let Ok(f) = std::fs::OpenOptions::new().write(true).open(waiter_path(drone)) else { return false };
    matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

/// The mark a waiter whose borg was replaced keeps while it runs the old code (wait.rs, reexec.rs): it
/// claims nothing until its re-exec. Monitoring shows the drone `waiting (old borg, restarting)`.
fn stale_path(drone: &str) -> PathBuf {
    hook::state_file(drone).with_extension("hive-waiter-stale")
}

/// Marks `drone`'s waiter as running a replaced borg (`stale`), or not.
pub(crate) fn mark_stale(drone: &str, stale: bool) {
    let _ = if stale { std::fs::write(stale_path(drone), "") } else { std::fs::remove_file(stale_path(drone)) };
}

/// Whether `drone`'s waiter runs and still runs the borg that was replaced (`mark_stale`).
pub fn waiting_stale(drone: &str) -> bool {
    stale_path(drone).exists() && waiting(drone)
}

/// Stops `drone`'s waiter, if one runs: SIGTERM to the pid its lock file names, which it ends on quietly.
fn stop_waiter(drone: &str) {
    if !waiting(drone) {
        return;
    }
    let pid = std::fs::read_to_string(waiter_path(drone)).ok().and_then(|t| t.trim().parse::<i32>().ok()).unwrap_or(0);
    if pid > 0 && pid as u32 != std::process::id() {
        // SAFETY: kill(2) with SIGTERM on the pid the running waiter wrote into the file it holds locked.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
}

/// Where a waiter started by a stop (`spawn_waiter`) writes what it says and why it ended: it has no
/// terminal, and the session hears it by `codex queue` alone.
pub(crate) fn waiter_log(drone: &str) -> PathBuf {
    hook::state_file(drone).with_extension("hive-wait.log")
}

/// Starts the waiter of codex drone `hs` of `session` (`borg hive wait --queue`, wait.rs) from its stop:
/// in a session of its own (setsid), stdin from nothing and its output appended to `waiter_log`, so it
/// outlives the hook, codex never waits on the hook's pipes, and its end at the hook's timeout is no
/// end of it; with the root store as its `BORG_HOME` (the hook set it to the collective's own, `home`),
/// so it routes its directory as a command does. Not awaited. A second one started meanwhile finds the
/// lock held and exits.
fn spawn_waiter(home: &Path, hs: &HiveSession, session: &str, cwd: &str) {
    use std::os::unix::process::CommandExt;
    let exe = super::reexec::exe_path();
    let Ok(log) = std::fs::OpenOptions::new().create(true).append(true).open(waiter_log(&hs.drone)) else { return };
    let Ok(err) = log.try_clone() else { return };
    let mut c = std::process::Command::new(exe);
    c.args(["hive", "wait", "--queue", "--session", session]);
    if !cwd.is_empty() {
        c.args(["--dir", cwd]);
    }
    c.env("BORG_HOME", crate::collective::root_of(home)).stdin(std::process::Stdio::null()).stdout(log).stderr(err);
    // SAFETY: setsid(2) between fork and exec only makes the child its own session, so the end of the
    // hook's process group leaves it running.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let _ = c.spawn();
}

/// `drone`'s newest registry line (which session it is now: a `/clear` moves it), None once it left.
pub(crate) fn entry_of(home: &Path, drone: &str) -> Option<Entry> {
    live(home).into_iter().find(|e| e.drone == drone)
}

/// Whether the harness process of registry line `e` is gone (`harness_pid`: its pid no longer runs, or
/// runs another process, a later start time): the session the drone was is over, whether or not its
/// SessionEnd ran (a terminal closed, Claude Code's "Move to background and exit", a crash). False when
/// the join found no harness process (pid 0): then only the session's own end or a leave ends it.
pub(crate) fn harness_gone(e: &Entry) -> bool {
    e.pid != 0 && proc_stat(e.pid).is_none_or(|(_, _, start)| start != e.pid_start)
}

/// This process's pid namespace (`/proc/self/ns/pid`, e.g. `pid:[4026531836]`); empty off Linux.
pub(crate) fn pid_ns() -> String {
    std::fs::read_link("/proc/self/ns/pid").map(|p| p.display().to_string()).unwrap_or_default()
}

/// Whether this process may reap registry line `e` (node.rs `reap`): it sees `e`'s harness pid as the
/// join saw it, the two in one pid namespace. A daemon in a container, or a join in a sandbox with a pid
/// namespace of its own, would read every drone's harness as gone: such a line is left to its own waiter
/// and its session's end. A line with no namespace recorded (from before) is too.
pub(crate) fn reapable(e: &Entry) -> bool {
    e.pid_ns == pid_ns()
}

// ---------------------------------------------------------------- the hooks

/// An interactive drone's step at each hook event of harness `name` (`input`) but its start (mod.rs
/// `around`, `pre_tool`): any event takes a handed claim up (`untaken`, phase g correction 10) and ends
/// a missed wake's hold (`missed`), but a codex drone's (`Wake::Queue`) is taken up by the prompt of the
/// turn its wake starts alone (the handed text, `arrived`): codex queues the wake behind a turn the
/// engineer started meanwhile, whose events leave the claim not in hand (`not_in_hand`), so that turn's
/// stop runs no check, says no done and blocks nothing for it (never hijacks an engineer's turn). The
/// engineer's prompt starts a busy turn, a tool call keeps it, its end moves it on (`busy`); and while
/// the drone works, a goal heartbeat
/// (`board::BEAT`) when one is due (`BEAT_MS`) and no node worker runs (node.rs `worker_runs`), so its node
/// stays a participant with no daemon and no waiter. A harness whose plugin sends its own pid (`pid`,
/// omp's and opencode's `process.pid`) is the drone's harness process from its first event on
/// (`own_pid`), whatever ancestors the join's shell had.
/// Whether `hs` changed (to save).
pub(crate) fn touch(home: &Path, event: &str, hs: &mut HiveSession, session: &str, name: &str, input: &Value, now: i64) -> bool {
    let queued = Wake::queues(name);
    let takes_up = !queued
        || event == "prompt"
            && untaken(session).is_some_and(|(g, _, _)| input.get("prompt").and_then(Value::as_str).unwrap_or_default().trim_start().starts_with(&arrived(&g)));
    if takes_up {
        if let Some((goal, key, handed)) = untaken(session) {
            taken_up(home, &goal, &key, handed, session, now);
        }
        let _ = std::fs::remove_file(takeup_path(session));
    }
    let _ = std::fs::remove_file(missed_path(session));
    let idle = if queued { QUEUE_BUSY_IDLE_MS } else { BUSY_IDLE_MS };
    let mut changed = false;
    if let Some(p) = hook_pid(name, input).filter(|p| *p != 0 && *p != hs.pid) {
        own_pid(home, hs, session, p, now);
        changed = true;
    }
    match event {
        "prompt" => {
            set_busy(session, false, now, idle);
            changed = hs.asked_wait;
            hs.asked_wait = false;
        }
        "pre_tool" if busy_path(session).exists() => set_busy(session, true, now, idle),
        "post_tool" | "post_tool_failure" if busy_path(session).exists() => set_busy(session, false, now, idle),
        _ => {}
    }
    // Where its usage is (Claude Code's transcript), for a claim that ends outside a hook (meter.rs).
    if let Some(t) = input.get("transcript_path").and_then(Value::as_str).filter(|t| !t.is_empty() && *t != hs.transcript) {
        hs.transcript = t.into();
        changed = true;
    }
    if event == "pre_tool" || !own_beat(home, &hs.goal, hs.member_ms, now) {
        return changed;
    }
    hs.member_ms = now;
    true
}

/// A drone's own beat of `goal` at `now` (`touch`, and a waiter gated on a replaced borg, wait.rs):
/// written when the last was `BEAT_MS` ago (`last`), the node's membership of it is open and no node
/// worker runs; whether the drone's beat counts as done now (written, or the worker beats).
pub(crate) fn own_beat(home: &Path, goal: &str, last: i64, now: i64) -> bool {
    if goal.is_empty() || now - last < super::BEAT_MS {
        return false;
    }
    // The node's worker beats (its mesh daemon, or a waiter while none runs, node.rs): no beat of its own.
    if super::node::worker_runs(home) {
        return true;
    }
    let here = node(home);
    member_open(&board::fold_on(&board::read(&tasks_path(home, goal)), &here), &here) && member(home, goal, board::BEAT).is_ok()
}

/// The pid a hook event's harness sent as its own (`pid`: omp and opencode run their adapter in their
/// own process); None from a harness that sends none (hook.rs drops it then).
pub(crate) fn event_pid(input: &Value) -> Option<u32> {
    input.get("pid").and_then(Value::as_u64).and_then(|p| u32::try_from(p).ok())
}

/// Drone `hs` of `session` runs under harness process `pid` (its plugin said so): its registry line
/// names that process, with its start and pid namespace, when it named another (the join's ancestor
/// walk stopped short of the harness, or found none), so its waiter and the node's reaper end the drone
/// with that process and no other (phase g spec 3.1: "plugins pass `process.pid`"). A pid that runs
/// nothing is not taken.
fn own_pid(home: &Path, hs: &mut HiveSession, session: &str, pid: u32, now: i64) {
    hs.pid = pid;
    let Some(mut e) = entry_of(home, &hs.drone).filter(|e| e.session == session && e.pid != pid) else { return };
    let Some((_, _, start)) = proc_stat(pid) else { return };
    (e.pid, e.pid_start, e.pid_ns, e.ms, e.by) = (pid, start, pid_ns(), now.max(e.ms + 1), "rebind".into());
    let _ = record(home, &e);
}

/// `p` with its `.` and `..` parts resolved, no file system read: where an edit lands.
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// `p` resolved (`lexical`) with its nearest existing ancestor's symlinks resolved too, so a path through
/// a linked directory (macOS's `/var` is `/private/var`) and the same path written plainly compare equal.
fn real(p: &Path) -> PathBuf {
    let p = lexical(p);
    let mut base = p.as_path();
    loop {
        if let Ok(r) = base.canonicalize() {
            return r.join(p.strip_prefix(base).unwrap_or(Path::new("")));
        }
        match base.parent() {
            Some(up) => base = up,
            None => return p,
        }
    }
}

/// Why `tool`'s edit (`input`, a PreToolUse) is refused to the drone of `hs`: a file it writes is in
/// the drone's repo but outside its own worktree (the engineer's checkout, another drone's tree), where
/// its work would never reach its done and its claim's checks (phase g spec 3.1). None for a drone with
/// no worktree, and for files outside the repo (a scratch file, the drone's own notes).
pub(crate) fn outside_tree(hs: &HiveSession, tool: &str, input: &Value) -> Option<String> {
    let tree = real(Path::new(&hs.worktree));
    let tree = tree.as_path();
    let repo = tree.parent().filter(|p| p.file_name().is_some_and(|n| n == ".worktrees")).and_then(Path::parent)?;
    let ti = input.get("tool_input").unwrap_or(&Value::Null);
    let cwd = input.get("cwd").and_then(Value::as_str).map_or_else(|| tree.to_path_buf(), PathBuf::from);
    let path = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).filter(|f| !f.is_empty()).map(String::from);
    let files = [path(ti, "file_path"), path(ti, "notebook_path")].into_iter().flatten();
    let files = files.chain(ti.get("edits").and_then(Value::as_array).into_iter().flatten().filter_map(|e| path(e, "file_path")));
    let off = files.map(|f| real(&cwd.join(f))).find(|f| f.starts_with(repo) && !f.starts_with(tree))?;
    let rel = off.strip_prefix(repo).unwrap_or(&off);
    let there = if rel.starts_with(".worktrees") { tree.to_path_buf() } else { tree.join(rel) };
    Some(format!(
        "[borg hive] {} is outside your worktree: this {tool} is refused, so your work never lands in the engineer's checkout. Work in your worktree only: {}",
        off.display(),
        there.display()
    ))
}

/// The claim the drone of `hs` holds on its goal now, and whether it was shown to it (its key is the
/// session's last `TASK_MARKER`): one not shown is the stop's capped claim, which the waiter hands once
/// final (wait.rs `pending`). None when it holds nothing.
pub(crate) fn unshown(home: &Path, hs: &HiveSession, session: &str, now: i64) -> Option<(super::TaskState, bool)> {
    if hs.goal.is_empty() {
        return None;
    }
    let here = node(home);
    let b = board::fold_on(&board::read(&tasks_path(home, &hs.goal)), &here);
    let held = board::holding(&b, &here, session, now)?.clone();
    let key = format!("{}@{}", held.task.id, held.live(now).map_or(0, |l| l.2));
    let state = std::fs::read_to_string(hook::state_file(session)).unwrap_or_default();
    let shown = super::last_marked(&state, super::TASK_MARKER) == Some(key.as_str());
    Some((held, shown))
}

/// Whether drone `hs` of `session` holds a claim not in its hand at `now`: one it was never shown
/// (`unshown`), or one a wake handed that no turn of its own took up yet (`untaken`: a codex wake that
/// waits in codex's queue behind the engineer's turn, `touch`). Such a claim is nothing in hand at the
/// prompt and the stop (mod.rs `Ctx::prompt`, `Ctx::stop`): no check, no done, no block of the turn.
pub(crate) fn not_in_hand(home: &Path, hs: &HiveSession, session: &str, now: i64) -> bool {
    unshown(home, hs, session, now).is_some_and(|(held, shown)| {
        let key = format!("{}@{}", held.task.id, held.live(now).map_or(0, |l| l.2));
        !shown || untaken(session).is_some_and(|(g, k, _)| g == hs.goal && k == key)
    })
}

/// An interactive drone's session ended (SessionEnd), not by a `/clear`, whose new session is rebound
/// (`session_start`): the drone leaves, its claim released as `session ended` (`leave_noted`). Whether it
/// left.
pub(crate) fn ended(home: &Path, hs: &HiveSession, session: &str, cwd: &str, input: &Value, now: i64) -> bool {
    if input.get("reason").and_then(Value::as_str) == Some("clear") {
        return false;
    }
    let here = Here { home: home.to_path_buf(), cwd: cwd.to_string(), now };
    leave_noted(&here, session, hs, "session ended").is_ok()
}

/// Whether the stop's input shows a `borg hive wait` running in the background (Claude Code's
/// `background_tasks`, its in-flight tasks).
fn wait_armed(input: &Value) -> bool {
    input
        .get("background_tasks")
        .and_then(Value::as_array)
        .is_some_and(|ts| ts.iter().any(|t| t.get("command").and_then(Value::as_str).is_some_and(|c| c.contains(" hive wait"))))
}

/// An interactive drone's stop, after the hive's own (`out`): a drone with nothing in hand (no claim,
/// or one it was never shown: the stop's capped claim, not final yet, mod.rs `hand_next`) and no waiter
/// (none in the stop's `background_tasks`, none holding its lock) is asked once a turn to start one
/// (`borg hive wait` with `run_in_background`), after whatever the hive's stop said first (a done's
/// result, mod.rs `Ctx::done`), so the model never reports a state the stop has changed. It makes the
/// model call a tool, so Claude Code's continuation cap never counts it. Everything a drone's stop says
/// goes as Stop feedback (`feedback`), not as a hook error. A stop that ends the turn ends the
/// engineer's busy turn too (`busy`).
///
/// A drone of a harness whose plugin keeps its waiter (`Wake::Plugin`: omp, opencode)
/// is never asked for a wait: its plugin's waiter runs from the join on and wakes the session. Its stop
/// passes as the hive's own (`decision: block` and `reason`, which those adapters read). A block with
/// the hive's words (`hive`: a done, the next task, a check that failed) carries `hive: true`, which the
/// adapters tell from a conscience's questions by (the opencode adapter's rules for questions, a
/// superseded turn's dropped and a round cap, are kept for questions alone); one of them that leaves
/// nothing in hand (a done said, a task published blocked) ends with the idle line, so the model ends
/// its turn on it. A block of questions alone passes as it is.
///
/// A codex drone (`Wake::Queue`) is never asked either: a stop that ends its turn (it does not block)
/// starts the drone's waiter, detached, when none runs (`spawn_waiter`), and passes at once, so no stop
/// waits on it and codex's stop timeout stays as it is; a block goes on as codex reads it (its
/// continuation prompt), the hive's own one that leaves nothing in hand with the idle line, as above.
#[allow(clippy::too_many_arguments)]
pub(crate) fn after_stop(
    home: &Path,
    hs: &mut HiveSession,
    session: &str,
    cwd: &str,
    input: &Value,
    out: Option<Value>,
    wake: Wake,
    now: i64,
) -> Option<Value> {
    let working = unshown(home, hs, session, now).is_some_and(|(_, shown)| shown);
    let idle = |hs: &HiveSession, out: Option<Value>, mark: bool| {
        let mut o = out?;
        if mark {
            o["hive"] = Value::Bool(true);
        }
        if !working {
            let reason = o.get("reason").and_then(Value::as_str).unwrap_or_default().trim_end().to_string();
            o["reason"] = Value::String(format!(
                "{reason}\n\n{} Nothing is open to you now: end your turn with: {IDLE_LINE} You stay a drone; leaving the hive is only on the engineer's request.",
                head_of(hs)
            ));
        }
        Some(o)
    };
    match wake {
        Wake::Plugin { .. } | Wake::Queue { .. } if !blocks(&out) => {
            clear_busy(session);
            if matches!(wake, Wake::Queue { .. }) && !waiting(&hs.drone) {
                spawn_waiter(home, hs, session, cwd);
            }
            return out;
        }
        Wake::Plugin { hive: false } | Wake::Queue { hive: false } => return out,
        Wake::Plugin { hive: true } => return idle(hs, out, true),
        Wake::Queue { hive: true } => return idle(hs, out, false),
        Wake::Model => {}
    }
    if working || hs.asked_wait || wait_armed(input) || waiting(&hs.drone) {
        if !blocks(&out) {
            clear_busy(session);
        }
        return feedback(out);
    }
    hs.asked_wait = true;
    save(session, hs);
    let arm = arm_text(hs, session, cwd);
    let reason = match out.as_ref().and_then(|o| o.get("reason")).and_then(Value::as_str) {
        Some(r) => format!("{}\n\n{arm}", r.trim_end()),
        None => arm,
    };
    let mut o = out.filter(|o| o.is_object()).unwrap_or_else(|| json!({}));
    o["decision"] = json!("block");
    o["reason"] = Value::String(reason);
    feedback(Some(o))
}

fn blocks(o: &Option<Value>) -> bool {
    o.as_ref().is_some_and(|o| o.get("decision").and_then(Value::as_str) == Some("block"))
}

/// A drone's blocking stop (`decision: block`, its `reason`) as Claude Code's Stop feedback
/// (`hookSpecificOutput.additionalContext`): the turn goes on with the same text through the same loop
/// protections (`stop_hook_active`, the 8-continuation cap), but the transcript shows it as hook
/// feedback, not "Stop hook error" (hooks docs, Stop decision control). A drone's stop says a handover
/// (a task done, the next one, the wait to start), never an error. Anything else passes as it is.
fn feedback(out: Option<Value>) -> Option<Value> {
    if !blocks(&out) {
        return out;
    }
    let mut o = out?;
    let map = o.as_object_mut()?;
    let reason = map.remove("reason").and_then(|r| r.as_str().map(String::from)).unwrap_or_default();
    map.remove("decision");
    let theirs = map.get("hookSpecificOutput").and_then(|h| h.get("additionalContext")).and_then(Value::as_str).filter(|t| !t.is_empty());
    let text = match theirs {
        Some(t) => format!("{reason}\n\n{t}"),
        None => reason,
    };
    map.insert("hookSpecificOutput".into(), json!({"hookEventName": "Stop", "additionalContext": text}));
    Some(o)
}

/// What a drone with nothing in hand is told to run, and the line it idles on: one that says the work
/// comes by itself, so the obvious next step is to keep waiting, and leaving the engineer's call.
fn arm_text(hs: &HiveSession, session: &str, cwd: &str) -> String {
    let head = head_of(hs);
    format!(
        "{head} Nothing is open to you now, so wait for hive work without spending tokens: run `{}` with the Bash tool's `run_in_background` set to true, \
then end your turn with: {IDLE_LINE} When the wait exits, its output is your next task. You stay a drone; leaving the hive is only on the engineer's request.",
        command("wait", session, cwd)
    )
}

/// What a drone's hive text starts with: `[borg hive <goal>]`, or `[borg hive]` for a drone of every
/// open goal or of none yet.
fn head_of(hs: &HiveSession) -> String {
    if hs.goal.is_empty() || hs.any { "[borg hive]".to_string() } else { format!("[borg hive {}]", hs.goal) }
}

/// The line an idle drone ends its turn on (`arm_text`).
pub const IDLE_LINE: &str = "Drone idle: waiting for hive work, which starts here by itself when a task opens.";

/// Claude Code's SessionStart (`start`) of a session of this node's drones (phase g correction 7):
/// `/clear` gives the session a new id and compact may, so a session no drone is on yet but whose harness
/// process (`harness_pid`) is a live drone's is that drone's session from now on (its registry line, its
/// membership moved, the old id's claim released: the context that held it is gone); a resumed one keeps
/// its id and is the drone's harness from now on. A bound drone is told it is one, and to start its wait
/// when none runs (a resume restarts no background command). Any other session gets nothing, after one
/// failed open of the registry.
pub fn session_start(_h: &dyn Harness, home: &Path, input: &Value, now: i64) -> Option<Value> {
    if !registry_path(home).exists() {
        return None;
    }
    let session = input.get("session_id").and_then(Value::as_str)?;
    let source = input.get("source").and_then(Value::as_str).unwrap_or("startup");
    let mut hs = match bound(session) {
        Some(hs) if hs.interactive => hs,
        Some(_) => return None,
        None if matches!(source, "clear" | "compact") => rebind(home, session, now)?,
        None => return None,
    };
    if source == "resume"
        && let Some((pid, pid_start)) = harness_pid()
        && let Some(mut e) = live(home).into_iter().find(|e| e.drone == hs.drone).filter(|e| (e.pid, e.pid_start) != (pid, pid_start))
    {
        (e.pid, e.pid_start, e.pid_ns, e.ms, e.by) = (pid, pid_start, pid_ns(), now, "rebind".into());
        let _ = record(home, &e);
    }
    clear_busy(session);
    hs.asked_wait = false;
    save(session, &hs);
    let cwd = if hs.worktree.is_empty() { input.get("cwd").and_then(Value::as_str).unwrap_or_default().to_string() } else { hs.worktree.clone() };
    let what = if hs.any { "every open hive goal of this repo".to_string() } else { format!("hive goal {}", hs.goal) };
    let tree = if hs.worktree.is_empty() { String::new() } else { format!(" Work only in your worktree {}.", hs.worktree) };
    let wait = match waiting(&hs.drone) {
        true => "Its wait for work runs in the background.".to_string(),
        false => {
            format!("When nothing is in hand, run `{}` with the Bash tool's `run_in_background` set to true and end your turn.", command("wait", session, &cwd))
        }
    };
    let text =
        format!("[borg hive] This session is drone {} of {what}.{tree} {wait} It stays a drone: leaving the hive is only on the engineer's request.", hs.drone);
    Some(json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": text}}))
}

/// `session` becomes the session of the live drone whose harness process this hook runs under, when
/// exactly one is and its own session is not this one: its membership moves to `session`, the old id's
/// claim is released, and a registry line says so. The membership as moved.
fn rebind(home: &Path, session: &str, now: i64) -> Option<HiveSession> {
    let (pid, pid_start) = harness_pid()?;
    let mut mine = live(home).into_iter().filter(|e| e.pid == pid && e.pid_start == pid_start && e.session != session);
    let (e, None) = (mine.next()?, mine.next()) else { return None };
    move_to(home, e, session, now)
}

/// The hook event that follows a join, its own command's PostToolUse, of a session no drone is on
/// (phase g spec 3.1): `borg hive join` bound the id the harness gave the command
/// (`CLAUDE_CODE_SESSION_ID`), which under `claude --continue` or `--resume` may be the startup id, an
/// id no hook of the session carries. The hook's own id is the session's: the drone the join made in this
/// harness process (`harness_pid`) moves to it (`move_to`), so the join never leaves a drone nothing
/// wakes or a binding that refuses runners for nobody. Anything else, and every event but a join
/// command's, gets None after no read at all; one failed open when no drone ever ran on this node.
/// Only a drone of a harness whose process runs one session at a time moves (`Caps::
/// one_session_per_process`: Claude Code): one opencode or omp process may run several sessions, so another
/// of its sessions' join command is never this drone's (opencode's plugin binds the session that ran it).
pub(crate) fn adopt(home: &Path, input: &Value) -> Option<HiveSession> {
    let command = input.get("tool_input").and_then(|t| t.get("command")).and_then(Value::as_str)?;
    if !command.contains("hive join") || !registry_path(home).exists() {
        return None;
    }
    let session = input.get("session_id").and_then(Value::as_str)?;
    let (pid, pid_start) = harness_pid()?;
    let mut made = live(home)
        .into_iter()
        .filter(|e| e.by == "join" && one_session_per_process(&e.harness) && e.pid == pid && e.pid_start == pid_start && e.session != session);
    let (e, None) = (made.next()?, made.next()) else { return None };
    move_to(home, e, session, learn::now_ms())
}

/// codex's first hook event after a join, the join command's own PostToolUse, of a session no drone is
/// on (phase g spec 3.1): `borg hive join` bound the `CODEX_THREAD_ID` codex gave the command, which
/// `codex queue --thread` wakes. A hook whose `session_id` is another id shows the two are not one
/// thread here: the drone that join made (the one its output names, `as drone <name>`) would be woken
/// on a thread nobody types in, so it leaves (`leave_quietly`: a plain session again) and the session
/// is told so. Nothing for a matching id (the session is bound, and `around` never asks), for any other
/// command, and after no read for an event that is no join's; one failed open when no drone ever ran
/// on this node. A drone woken through its harness's CLI only (`Wake::Queue`: codex); a Claude Code
/// drone moves to the hook's id (`adopt`).
pub(crate) fn mismatch(home: &Path, input: &Value) -> Option<String> {
    let command = input.get("tool_input").and_then(|t| t.get("command")).and_then(Value::as_str)?;
    if !command.contains("hive join") || !registry_path(home).exists() {
        return None;
    }
    let session = input.get("session_id").and_then(Value::as_str)?;
    let said = match input.get("tool_response") {
        Some(Value::String(t)) => t.clone(),
        Some(v) => v.to_string(),
        None => return None,
    };
    let e = live(home)
        .into_iter()
        .find(|e| Wake::queues(&e.harness) && e.by == "join" && e.session != session && said.contains(&format!("as drone {}.", e.drone)))?;
    let hs = bound(&e.session).filter(|hs| hs.interactive)?;
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default().to_string();
    leave_quietly(&Here { home: home.to_path_buf(), cwd, now: learn::now_ms() }, &e.session, &hs).ok()?;
    Some(format!(
        "[borg hive] The join bound thread {} (the CODEX_THREAD_ID codex gave the command), but this session's hooks name {session}: the hive could not wake this session, so it is no drone (drone {} left again). It is a plain session; nothing more to do.",
        e.session, e.drone
    ))
}

/// Whether harness `name`'s process runs one session at a time (`Caps::one_session_per_process`).
fn one_session_per_process(name: &str) -> bool {
    crate::harness::by_name(name).is_some_and(|h| h.caps().one_session_per_process)
}

/// Drone `e` is session `session` from now on: its membership moved, what its old id held released, a
/// registry line that says so (`rebind`). The membership as moved.
fn move_to(home: &Path, e: Entry, session: &str, now: i64) -> Option<HiveSession> {
    let hs = bound(&e.session).filter(|hs| hs.interactive)?;
    let here = node(home);
    for g in served(home, &hs, &queen::repo_name(&e.worktree)) {
        let b = board::fold_on(&board::read(&tasks_path(home, &g.id)), &here);
        if let Some(t) = board::holding(&b, &here, &e.session, now) {
            let _ = super::write(home, &g.id, vec![Event::new(Ev::Released, &t.task.id, &e.session, now).with_note("its session's context was cleared")]);
        }
    }
    steer_write(session, &hs).ok()?;
    super::unbind(&e.session);
    clear_busy(&e.session);
    let _ = std::fs::remove_file(takeup_path(&e.session));
    let _ = std::fs::remove_file(missed_path(&e.session));
    record(home, &Entry { ms: now, session: session.into(), by: "rebind".into(), ..e }).ok()?;
    Some(hs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wake's latency is its hand to its take-up on this node's clock, never from the claim's stamp: a
    /// peer's line stamped an hour ahead lifts the claim's stamp `board::AHEAD_MS` past this clock, and a
    /// latency from it would read 0. A wake released meanwhile (wait.rs `takeup`) is no taken one, and a
    /// hand of no time writes no line. Fails with `taken_up` on the claim's stamp: 0, not 700.
    #[test]
    fn a_wake_is_timed_from_its_hand_on_this_clock_and_only_while_held() {
        let home = std::env::temp_dir().join(format!("borg-hive-takenup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let session = format!("takenup-{}", std::process::id());
        let task = super::super::Task {
            id: "t1".into(),
            role: super::super::Role::Work,
            title: "do t1".into(),
            repo: String::new(),
            files: vec!["t1.txt".into()],
            deps: vec![],
            accept: vec![],
            of: None,
            not_by: vec![],
            flags: vec![],
            from: None,
            group: None,
        };
        let now = learn::now_ms();
        let path = tasks_path(&home, "g");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        board::append(&path, &[Event::open(task, "engineer:e", now - 1_000)]).unwrap();
        let ahead = Event { node: "remote-abcdef12".into(), ..Event::new(Ev::Heartbeat, "", "runner", now + 3_600_000).with_note(board::JOIN) };
        board::append(&path, &[ahead]).unwrap();
        let landed = super::super::write(&home, "g", vec![Event::new(Ev::Claim, "t1", &session, now)]).unwrap();
        let claim = landed[0].ms;
        assert!(claim >= now + board::AHEAD_MS, "the peer's line lifts the claim's stamp: {claim} vs {now}");
        let key = format!("t1@{claim}");
        let taken = || -> Vec<super::super::Confirm> { super::super::read_lines(&super::super::confirms_path(&home, "g")) };
        taken_up(&home, "g", &key, 0, &session, now + 700);
        assert!(taken().is_empty(), "a hand of no time: no line");
        taken_up(&home, "g", &key, now, &session, now + 700);
        let got: Vec<(String, u64, String)> = taken().into_iter().map(|c| (c.task, c.waited_ms, c.outcome)).collect();
        assert_eq!(got, [("t1".to_string(), 700, super::super::TAKEN.to_string())], "hand to take-up on this clock");
        super::super::write(&home, "g", vec![Event::new(Ev::Released, "t1", &session, now + 800).with_note(NOT_TAKEN_UP)]).unwrap();
        taken_up(&home, "g", &key, now, &session, now + 900);
        assert_eq!(taken().len(), 1, "a wake released meanwhile is missed, never also taken");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_join_refused_by_a_read_only_sandbox_names_the_step_past_it() {
        // codex 0.153.2's workspace-write sandbox, as the join's own error read in it.
        let e = "/w/harbor_orion/.git/info/exclude: Read-only file system (os error 30)".to_string();
        let said = sandboxed(e.clone());
        assert!(said.starts_with(&e) && said.contains("outside the sandbox") && said.contains("escalated permissions"), "{said}");
        assert_eq!(sandboxed("git worktree add --detach /w HEAD failed".into()), "git worktree add --detach /w HEAD failed", "any other error as it is");
    }

    #[test]
    fn a_drone_of_every_goal_refuses_runners_only_of_its_repos_goals() {
        // Phase g correction 3, scoped as `served` is. A drone of every open goal working in harbor_orion
        // serves g1 (no repos: every repo) and not g2 (harbor_vega only): a run of g2 on this node is
        // not refused. Fails with `serving` on `Entry::serves` alone: g2 refused too.
        let home = std::env::temp_dir().join(format!("borg-hive-serving-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let goal = |id: &str, repos: &[&str]| goal::Goal {
            v: 1,
            id: id.into(),
            spec: "s".into(),
            oracle: vec![],
            budget_usd: 1.0,
            repos: repos.iter().map(|r| goal::RepoScope { name: (*r).into(), node: String::new() }).collect(),
            author: String::new(),
            created_ms: 0,
            sig: String::new(),
            hidden: None,
            held_out: false,
        };
        goal::create(&home, &goal("g1", &[])).unwrap();
        goal::create(&home, &goal("g2", &["harbor_vega"])).unwrap();
        let session = format!("serving-{}", std::process::id());
        steer_write(&session, &HiveSession { any: true, interactive: true, ..HiveSession::default() }).unwrap();
        let tree = "/work/harbor_orion/.worktrees/hive-i-abcdef";
        let e =
            Entry { v: 1, ms: 1, drone: "i-abcdef".into(), session: session.clone(), any: true, worktree: tree.into(), by: "join".into(), ..Entry::default() };
        record(&home, &e).unwrap();
        assert!(serving(&home, "g1").is_some(), "its repo's goal");
        assert!(serving(&home, "g2").is_none(), "another repo's goal");
        super::super::unbind(&session);
        let _ = std::fs::remove_dir_all(home);
    }
}
