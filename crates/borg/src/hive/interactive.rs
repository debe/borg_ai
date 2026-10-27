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
//! - `<state>/<drone>.hive-waiter`: locked by the drone's running waiter (`waiting`).
//!
//! Membership on this step: `join` (and any claim path, `ensure_member`) writes the node's JOIN under the
//! board lock before any claim of the drone, the waiter beats while it waits and the hooks while the
//! drone works (`touch`), and the node's LEAVE comes only with its last drone of the goal (`leave`).
//! Phase g G4 hands the beats to the node's mesh daemon, so they no longer depend on a waiter or a hook.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Board, Ev, Event, Here, HiveSession, board, bound, command, fail, goal, member, node, run, save, session_path, short, tasks_path, write_if};
use crate::{harness::Harness, hook, learn, queen};

/// The variables a harness sets to its session's id in every command its model runs and every hook it
/// runs: Claude Code's equals the hook input's `session_id` and follows `/clear`, but under `--continue`
/// or `--resume` without an id it may be the startup id instead (its env-vars docs), so the join's own
/// PostToolUse moves the drone to the id its hooks carry (`adopt`). What binds a session from inside
/// it: only the session that runs `borg hive join` becomes a drone.
pub const SESSION_ENV: &[&str] = &["CLAUDE_CODE_SESSION_ID"];

/// The registry of this node's interactive drones, beside the goals (`hive/drones.local`): never synced.
pub const REGISTRY: &str = "drones.local";

/// How long a claim a waiter handed waits for a hook event of its session before it is released
/// (`takeup`, phase g correction 10): a harness process that exits without starting the turn, or a turn
/// that never reaches a hook, takes nothing up. `BORG_HIVE_TAKEUP_MS` sets it for tests.
pub const TAKEUP: Duration = Duration::from_secs(60);
/// How long an engineer's turn that never reached its stop (an Esc, a crash) still counts as busy after
/// its last tool call ended (`busy`): a tool call still running never expires it (phase g correction 8).
pub const BUSY_IDLE_MS: i64 = 5 * 60_000;
/// The longest the stop's own claim of the next task waits for it to be final across nodes (phase f
/// F5) before the stop hands it to the waiter, which waits on with no cap (phase g correction 9): no
/// Stop spinner waits on peers.
pub const HAND_CAP: Duration = Duration::from_secs(5);

/// The session id the harness gave the process this runs in (`SESSION_ENV`), if any.
pub fn env_session() -> Option<String> {
    SESSION_ENV.iter().find_map(|k| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()))
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
/// after `/clear` is rebound by; 0 when unknown.
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

/// A process's command name, parent and start time (`/proc/<pid>/stat`, Linux).
fn proc_stat(pid: u32) -> Option<(String, u32, u64)> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (l, r) = (s.find('(')?, s.rfind(')')?);
    let rest: Vec<&str> = s.get(r + 2..)?.split_whitespace().collect();
    // After the command: state, ppid, ..., starttime (field 22 of stat(5), the 20th after it).
    Some((s[l + 1..r].to_string(), rest.get(1)?.parse().ok()?, rest.get(19)?.parse().ok()?))
}

/// Processes between a harness and borg: its shell, a wrapper, a sandbox (Claude Code's bubblewrap), borg
/// itself.
const WRAPPERS: &[&str] = &["sh", "bash", "zsh", "dash", "fish", "ksh", "borg", "env", "timeout", "nice", "nohup", "setsid", "script", "bwrap"];

/// The harness process this command or hook runs under: the nearest ancestor that is no shell, wrapper
/// or borg (`WRAPPERS`), with its start time, so a reused pid is no match. None off Linux or when the
/// walk finds none: a session is then rebound by nothing but its own id.
pub fn harness_pid() -> Option<(u32, u64)> {
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..16 {
        let (comm, ppid, start) = proc_stat(pid)?;
        if !WRAPPERS.contains(&comm.as_str()) {
            return Some((pid, start));
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

/// Whether a `borg hive run` of `goal` runs on this node (it holds the goal's run lock, run.rs).
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
    run::git(&repo, &["worktree", "add", "--detach", &p, "HEAD"], Duration::from_secs(60))
        .ok_or_else(|| format!("git worktree add --detach {p} HEAD failed"))?;
    Ok(p)
}

/// `borg hive join [<goal>]` inside a session: the harness's session (`env_session`) becomes an
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
        Err(e) => return fail(e),
    };
    let first = goals.first().map(|g| g.id.clone()).unwrap_or_default();
    let cursor = std::fs::metadata(tasks_path(&here.home, &first)).map_or(0, |m| m.len());
    let hs = HiveSession { goal: first, cursor, drone: drone.clone(), worktree: tree.clone(), ..probe };
    if let Err(e) = steer_write(session, &hs) {
        return fail(e);
    }
    let (pid, pid_start) = harness_pid().unwrap_or_default();
    let harness = crate::harness::named_current().map(|h| h.name().to_string()).unwrap_or_else(|| "claude".into());
    let e = Entry {
        v: 1,
        ms: here.now,
        drone: drone.clone(),
        session: session.into(),
        harness,
        pid,
        pid_start,
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
            "a `borg hive run {}` runs on this node: a node runs a runner or interactive drones for a goal, never both (its join and leave would end their claims)",
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
    let clear = if pid == 0 {
        " Its harness process was not found, so a `/clear` leaves the drone behind: `borg hive leave` before one, and join again after."
    } else {
        ""
    };
    println!(
        "{} joined {what} as drone {drone}.{tree} When nothing is open its turn ends with a wait for work that costs no tokens; `borg hive leave` makes it a plain session again.{clear}",
        short(session)
    );
    0
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
/// Its worktree stays.
pub(super) fn leave_quietly(here: &Here, session: &str, hs: &HiveSession) -> std::io::Result<()> {
    let here_node = node(&here.home);
    let repo = queen::repo_name(if hs.worktree.is_empty() { &here.cwd } else { &hs.worktree });
    let goals = served(&here.home, hs, &repo);
    for g in &goals {
        let b = board::fold_on(&board::read(&tasks_path(&here.home, &g.id)), &here_node);
        if let Some(t) = board::holding(&b, &here_node, session, here.now) {
            super::write(&here.home, &g.id, vec![Event::new(Ev::Released, &t.task.id, session, here.now).with_note("left the goal")])?;
        }
    }
    stop_waiter(&hs.drone);
    clear_busy(session);
    let _ = std::fs::remove_file(takeup_path(session));
    super::unbind(session);
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

/// The engineer's turn in `session` started or moved on at `now`: `tool` whether a tool call runs.
fn set_busy(session: &str, tool: bool, now: i64) {
    let _ = std::fs::write(busy_path(session), format!("{} {now}", u8::from(tool)));
}

pub(crate) fn clear_busy(session: &str) {
    let _ = std::fs::remove_file(busy_path(session));
}

/// Whether `session` is in a turn of the engineer's at `now` (phase g correction 8): from its prompt to
/// its stop, and once its stop never came, until `BUSY_IDLE_MS` after its last tool call ended; a tool
/// call still running (a long build, a test suite) keeps it busy however long. No waiter hands it work
/// meanwhile: the handoff never lands inside the engineer's turn.
pub fn busy(session: &str, now: i64) -> bool {
    let Ok(text) = std::fs::read_to_string(busy_path(session)) else { return false };
    let mut it = text.split_whitespace();
    let (tool, ms) = (it.next() == Some("1"), it.next().and_then(|m| m.parse::<i64>().ok()).unwrap_or(i64::MIN));
    tool || now.saturating_sub(ms) <= BUSY_IDLE_MS
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

/// How long a handed claim waits for its take-up (`TAKEUP`, or `BORG_HIVE_TAKEUP_MS`).
pub(crate) fn takeup_wait() -> Duration {
    std::env::var("BORG_HIVE_TAKEUP_MS").ok().and_then(|v| v.trim().parse().ok()).map_or(TAKEUP, Duration::from_millis)
}

/// Whether `drone`'s waiter runs: its lock is held.
pub fn waiting(drone: &str) -> bool {
    let Ok(f) = std::fs::OpenOptions::new().write(true).open(waiter_path(drone)) else { return false };
    matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock))
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

/// The session `drone` is now (its newest registry line), None once it left.
pub(crate) fn session_of(home: &Path, drone: &str) -> Option<String> {
    live(home).into_iter().find(|e| e.drone == drone).map(|e| e.session)
}

// ---------------------------------------------------------------- the hooks

/// An interactive drone's step at each hook event but its start (mod.rs `around`, `pre_tool`): any event
/// takes a handed claim up (`untaken`, phase g correction 10); the engineer's prompt starts a busy turn,
/// a tool call keeps it, its end moves it on (`busy`); and while the drone works, a goal heartbeat
/// (`board::BEAT`) when one is due (`BEAT_MS`), so its node stays a participant while no waiter runs.
/// Whether `hs` changed (to save).
pub(crate) fn touch(home: &Path, event: &str, hs: &mut HiveSession, session: &str, now: i64) -> bool {
    let _ = std::fs::remove_file(takeup_path(session));
    let mut changed = false;
    match event {
        "prompt" => {
            set_busy(session, false, now);
            changed = hs.asked_wait;
            hs.asked_wait = false;
        }
        "pre_tool" if busy_path(session).exists() => set_busy(session, true, now),
        "post_tool" | "post_tool_failure" if busy_path(session).exists() => set_busy(session, false, now),
        _ => {}
    }
    if event == "pre_tool" || hs.goal.is_empty() || now - hs.member_ms < super::BEAT_MS {
        return changed;
    }
    let here = node(home);
    if member_open(&board::fold_on(&board::read(&tasks_path(home, &hs.goal)), &here), &here) && member(home, &hs.goal, board::BEAT).is_ok() {
        hs.member_ms = now;
        changed = true;
    }
    changed
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

/// Why `tool`'s edit (`input`, a PreToolUse) is refused to the drone of `hs`: a file it writes is in
/// the drone's repo but outside its own worktree (the engineer's checkout, another drone's tree), where
/// its work would never reach its done and its claim's checks (phase g spec 3.1). None for a drone with
/// no worktree, and for files outside the repo (a scratch file, the drone's own notes).
pub(crate) fn outside_tree(hs: &HiveSession, tool: &str, input: &Value) -> Option<String> {
    let tree = Path::new(&hs.worktree);
    let repo = tree.parent().filter(|p| p.file_name().is_some_and(|n| n == ".worktrees")).and_then(Path::parent)?;
    let ti = input.get("tool_input").unwrap_or(&Value::Null);
    let cwd = input.get("cwd").and_then(Value::as_str).map_or_else(|| tree.to_path_buf(), PathBuf::from);
    let path = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).filter(|f| !f.is_empty()).map(String::from);
    let files = [path(ti, "file_path"), path(ti, "notebook_path")].into_iter().flatten();
    let files = files.chain(ti.get("edits").and_then(Value::as_array).into_iter().flatten().filter_map(|e| path(e, "file_path")));
    let off = files.map(|f| lexical(&cwd.join(f))).find(|f| f.starts_with(repo) && !f.starts_with(tree))?;
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

/// An interactive drone's session ended (SessionEnd), not by a `/clear`, whose new session is rebound
/// (`session_start`): the drone leaves (`leave_quietly`).
pub(crate) fn ended(home: &Path, hs: &HiveSession, session: &str, cwd: &str, input: &Value, now: i64) {
    if input.get("reason").and_then(Value::as_str) != Some("clear") {
        let here = Here { home: home.to_path_buf(), cwd: cwd.to_string(), now };
        let _ = leave_quietly(&here, session, hs);
    }
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
/// or one it was never shown: the stop's capped claim, not final yet, mod.rs `hand_next`) and no waiter (none in the stop's
/// `background_tasks`, none holding its lock) is asked once a turn to start one (`borg hive wait` with
/// `run_in_background`): a block that makes it call a tool, so Claude Code's continuation cap never
/// counts it. A stop that ends the turn ends the engineer's busy turn too (`busy`).
pub(crate) fn after_stop(home: &Path, hs: &mut HiveSession, session: &str, cwd: &str, input: &Value, out: Option<Value>, now: i64) -> Option<Value> {
    let working = unshown(home, hs, session, now).is_some_and(|(_, shown)| shown);
    let blocks = |o: &Option<Value>| o.as_ref().is_some_and(|o| o.get("decision").and_then(Value::as_str) == Some("block"));
    if working || hs.asked_wait || wait_armed(input) || waiting(&hs.drone) {
        if !blocks(&out) {
            clear_busy(session);
        }
        return out;
    }
    hs.asked_wait = true;
    save(session, hs);
    super::with_reason(&arm_text(hs, session, cwd), out, true)
}

/// What a drone with nothing in hand is told to run.
fn arm_text(hs: &HiveSession, session: &str, cwd: &str) -> String {
    let head = if hs.goal.is_empty() || hs.any { "[borg hive]".to_string() } else { format!("[borg hive {}]", hs.goal) };
    format!(
        "{head} Nothing is open to you now. Wait for hive work without spending tokens: run `{}` with the Bash tool's `run_in_background` set to true, \
then end your turn with: Drone idle: waiting for hive work (type anything to use this session). When the wait exits, its output is your next task.",
        command("wait", session, cwd)
    )
}

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
        (e.pid, e.pid_start, e.ms, e.by) = (pid, pid_start, now, "rebind".into());
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
    let text = format!("[borg hive] This session is drone {} of {what}.{tree} {wait} `borg hive leave` makes it a plain session again.", hs.drone);
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
pub(crate) fn adopt(home: &Path, input: &Value) -> Option<HiveSession> {
    let command = input.get("tool_input").and_then(|t| t.get("command")).and_then(Value::as_str)?;
    if !command.contains("hive join") || !registry_path(home).exists() {
        return None;
    }
    let session = input.get("session_id").and_then(Value::as_str)?;
    let (pid, pid_start) = harness_pid()?;
    let mut made = live(home).into_iter().filter(|e| e.by == "join" && e.pid == pid && e.pid_start == pid_start && e.session != session);
    let (e, None) = (made.next()?, made.next()) else { return None };
    move_to(home, e, session, learn::now_ms())
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
    record(home, &Entry { ms: now, session: session.into(), by: "rebind".into(), ..e }).ok()?;
    Some(hs)
}

#[cfg(test)]
mod tests {
    use super::*;

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
