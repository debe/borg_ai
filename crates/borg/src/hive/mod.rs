//! The hive: many drones, one goal (docs/design/collective.md). H1, the local spike: file-backed under
//! `<store>/hive/<goal>/`, no mesh sync yet (that waits for the pilot's result), in line formats that
//! can sync later.
//!
//! - `goal.json` (goal.rs): the spec, the oracle checks that gate `done`, the budget, the repos.
//! - `tasks.jsonl` (board.rs): the board's events; `board::fold` reads them the same on every node.
//!   Claims are leases renewed by the drone's hooks; the lowest (ms, blake3 key) claim wins a race.
//! - `contracts.jsonl` (contract.rs): what drones promise each other, shown once per session.
//! - `oracle/` (oracle.rs): the tails of failed oracle runs.
//! - `flags.jsonl` (guard.rs): what drones did to tests, CI or the oracle's files, flagged at each tool
//!   call; `oracle.jsonl` (hidden.rs): the hidden oracle's score of each judged attempt (runner only).
//!   verdict.rs turns a verifier's verdict into the reward: a diff of the claim's snapshots that
//!   weakened tests or changed CI or check files makes it `hacked`, and the lessons shown during the
//!   claim are credited.
//! - `joins.jsonl`: directories whose sessions join the goal (`borg hive join --here`, and each
//!   worktree `borg hive run` makes, with the session of each iteration, left when the run ends).
//! - `spend.jsonl`, `runs.jsonl` (run.rs): each headless iteration's cost, each `borg hive run`.
//! - `logs/` (run.rs): each iteration's output, local only: it never syncs.
//!
//! `borg hive run` (run.rs) starts N headless drones, each in a git worktree of its own, until the
//! board is finished or their budget share spent; its drone threads integrate the accepted work and run
//! the oracle on the combined tree (integrate.rs, also `borg hive integrate`); view.rs is the hive in
//! `borg top` and borg-web.
//!
//! A session joins a goal by `borg hive join`, by `BORG_HIVE=<goal>` at its first hook, or by a join
//! record of the directory it starts in; the membership is `<BORG_STATE>/<session>.hive` (`HiveSession`). The hook's [`around`] then adds the
//! drone's claim, the board's changes and the contracts in scope at a prompt, contracts and scope
//! warnings after an edit, and at the stop runs the oracle: a failing one blocks the stop (at most
//! `MAX_ORACLE_BLOCKS` times per claim), a passing one publishes `done`, opens its verify task and hands
//! the drone the next open task. A session in no goal costs one failed file open and sees no change.

pub mod board;
pub mod contract;
pub mod goal;
pub mod guard;
pub mod hidden;
pub mod integrate;
pub mod oracle;
pub mod run;
mod sim_drone;
pub mod verdict;
pub mod view;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use board::{Board, Ev, Event, Role, Span, Status, Task, TaskState};
pub use contract::{Contract, Scope};
pub use goal::{Check, Goal, RepoScope};

use crate::harness::Harness;
use crate::{collective, hook, learn, presence, queen, stats, steer, top, veto};

/// "t1", or "t1.v1" for the first verify task of t1.
pub type TaskId = String;

/// A drone: its node (empty: this machine; a member key once the mesh carries the hive) and session.
/// Lines carry it flat, as `node` and `by`, so they stay syncable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DroneId {
    pub node: String,
    pub session: String,
}

/// The goal a drone is launched into (the hive runner sets it on each drone): its first hook joins it.
pub const ENV: &str = "BORG_HIVE";
/// The drone a launched session is one iteration of (hive/run.rs): its claims carry it, so a verify
/// task barred to the author drone is barred to all its sessions.
pub const DRONE_ENV: &str = "BORG_HIVE_DRONE";
/// A claim expires when its drone went idle (presence.rs).
pub const LEASE_MS: i64 = presence::IDLE_MS;
/// How often the hooks renew a held claim.
pub const HEARTBEAT_MS: i64 = LEASE_MS / 3;
/// Stop blocks per claim while the oracle fails; then the hive lets the drone stop and publishes the
/// task blocked.
pub const MAX_ORACLE_BLOCKS: u32 = 3;
/// Board changes a prompt shows at most (the newest).
pub const SHOW_CHANGES: usize = 12;
/// The stop's time kept for the conscience and the experts after the oracle.
pub const CONSCIENCE_RESERVE: Duration = Duration::from_secs(20);
/// Less stop budget than this left: the oracle does not start (it reports a timeout).
const MIN_ORACLE: Duration = Duration::from_secs(2);
/// How long a claimant waits before reading the board back: a rival's line written at nearly the same
/// moment has landed by then, so the answer is the fold's.
pub(crate) const CLAIM_SETTLE: Duration = Duration::from_millis(150);
/// Each git call of a fix task's worktree reset.
const RESET_TIMEOUT: Duration = Duration::from_secs(60);
/// Claims `borg hive claim` without a task tries before it gives up on a busy board.
const CLAIM_TRIES: usize = 5;

/// The `.ctx` markers of what a session was shown (hook.rs `listed`): contracts, lost claims, files
/// outside the claim's scope; the claim last described (`task@since`, `-` for none); the out-of-scope
/// repo warning.
pub const CONTRACT_MARKER: &str = "#borg-hive-contract ";
const LOST_MARKER: &str = "#borg-hive-lost ";
const SCOPE_MARKER: &str = "#borg-hive-scope ";
const TASK_MARKER: &str = "#borg-hive-task ";
const REPO_MARKER: &str = "#borg-hive-repo";

/// Said once, at a drone's first hive prompt (docs/design/hive-learning.md, inoculation framing).
pub const INOCULATION: &str = "The oracle gates done: your task counts as done only when its checks pass, and a verifier drone checks the work after you. \
Every change to tests, CI config or oracle files is recorded and shown to the verifier; gaming a check is visible, allowed to happen, and useless: \
the task counts as failed. Adding tests, also inside an existing test file, is fine; outside your task's files an existing test may change only in names and values, \
never lose a line, an assertion or gain a skip. If a check looks wrong, say so in your done note instead of changing it.";
/// Added for a goal with a hidden oracle.
pub const INOCULATION_HIDDEN: &str = "This goal is also scored by held-out checks you cannot see.";

/// What a drone of `g` is told once (`INOCULATION`).
pub fn inoculation(g: &Goal) -> String {
    match g.hidden {
        Some(_) => format!("{INOCULATION_HIDDEN} {INOCULATION}"),
        None => INOCULATION.into(),
    }
}

pub fn dir(home: &Path, goal: &str) -> PathBuf {
    home.join("hive").join(goal)
}

pub fn goal_path(home: &Path, goal: &str) -> PathBuf {
    dir(home, goal).join("goal.json")
}

pub fn tasks_path(home: &Path, goal: &str) -> PathBuf {
    dir(home, goal).join("tasks.jsonl")
}

pub fn contracts_path(home: &Path, goal: &str) -> PathBuf {
    dir(home, goal).join("contracts.jsonl")
}

pub fn joins_path(home: &Path, goal: &str) -> PathBuf {
    dir(home, goal).join("joins.jsonl")
}

/// A session's membership: `<BORG_STATE>/<session>.hive`.
pub fn session_path(session: &str) -> PathBuf {
    hook::state_file(session).with_extension("hive")
}

/// Hive text a harness could not take at a prompt or tool call: delivered at the stop.
fn wait_path(session: &str) -> PathBuf {
    hook::state_file(session).with_extension("hive-wait")
}

/// A session's place in its goal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiveSession {
    pub goal: String,
    /// The tasks.jsonl offset up to which board changes were shown.
    #[serde(default)]
    pub cursor: u64,
    /// When the session last renewed its claim.
    #[serde(default)]
    pub beat_ms: i64,
    /// Stops blocked in a row by a failing oracle, for the claim `blocks_for`.
    #[serde(default)]
    pub oracle_blocks: u32,
    /// The claim the counter counts for (`task@since`): a new claim starts it again.
    #[serde(default)]
    pub blocks_for: String,
    /// The drone this session is an iteration of (`DRONE_ENV`); empty for a session of its own.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub drone: String,
}

/// The session's goal; None when it joined none, or on any error (fail open).
pub fn bound(session: &str) -> Option<HiveSession> {
    serde_json::from_slice(&std::fs::read(session_path(session)).ok()?).ok()
}

fn save(session: &str, hs: &HiveSession) {
    let _ = steer::rewrite(&session_path(session), &serde_json::to_string(hs).unwrap_or_default());
}

/// Joins `session` to `goal` of the store `home`. Changes from before the join are not news to it: the
/// board shows them all (`borg hive board`).
///
/// # Errors
/// When the membership cannot be written.
pub fn bind(home: &Path, session: &str, goal: &str) -> std::io::Result<()> {
    bind_as(home, session, goal, "")
}

fn bind_as(home: &Path, session: &str, goal: &str, drone: &str) -> std::io::Result<()> {
    let cursor = std::fs::metadata(tasks_path(home, goal)).map_or(0, |m| m.len());
    let hs = HiveSession { goal: goal.into(), cursor, drone: drone.into(), ..HiveSession::default() };
    steer::rewrite(&session_path(session), &serde_json::to_string(&hs).unwrap_or_default())
}

pub fn unbind(session: &str) {
    let _ = std::fs::remove_file(session_path(session));
}

/// The hook's join: a session launched with `BORG_HIVE=<goal>` joins that goal of the routed store at
/// its first hook, if the goal exists (anything else is ignored); without the variable, the latest
/// join record of a directory `cwd` is in decides (`joined`): a harness whose sessions drop the env
/// (opencode 2.x's background service) still joins its worktree's goal. `BORG_HIVE=` (empty) joins
/// nothing, records or not (the oracle's children). A store with no hive costs one failed stat.
pub fn bind_from_env(home: &Path, session: &str, cwd: &str) {
    // A session bound already reads no join record.
    if session_path(session).exists() {
        return;
    }
    let (goal, drone) = match std::env::var(ENV) {
        Ok(g) => (g.trim().to_string(), std::env::var(DRONE_ENV).unwrap_or_default()),
        Err(_) => match joined(home, cwd) {
            Some(j) => (j.goal, j.drone),
            None => return,
        },
    };
    if goal::valid_id(&goal) && goal_path(home, &goal).is_file() {
        let _ = bind_as(home, session, &goal, drone.trim());
    }
}

/// One line of joins.jsonl: sessions starting in `dir` (a git toplevel or worktree, absolute) join
/// `goal`; `by` is `join` (`borg hive join --here`) or `run` (a worktree of `borg hive run`, with the
/// iteration's `session` when the runner chose it). An empty `goal` is a leave: it shadows the joins
/// before it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Join {
    pub v: u8,
    pub ms: i64,
    pub goal: String,
    pub dir: String,
    #[serde(default)]
    pub drone: String,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub harness: String,
    pub by: String,
}

/// Appends `j` to `goal`'s joins.jsonl (one O_APPEND write).
///
/// # Errors
/// When the file cannot be written.
pub fn append_join(home: &Path, goal: &str, j: &Join) -> std::io::Result<()> {
    append_line(&joins_path(home, goal), j)
}

/// One JSON line appended in one write(2) with O_APPEND.
pub(crate) fn append_line<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let line = serde_json::to_string(v).map_err(std::io::Error::other)? + "\n";
    std::fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(line.as_bytes())
}

/// Every parsable line of a JSONL file, in file order (an incomplete or unknown line is skipped).
pub(crate) fn read_lines<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Every join record of the store's goals (one pass over each tiny joins.jsonl); none when the store
/// has no hive (one failed stat).
pub fn joins(home: &Path) -> Vec<Join> {
    if std::fs::metadata(home.join("hive")).is_err() {
        return Vec::new();
    }
    goal::list(home).iter().flat_map(|g| read_lines::<Join>(&joins_path(home, g))).collect()
}

/// The newest join record whose `dir` holds `cwd` (whole path components: `/w/repo2` is not in
/// `/w/repo`), the deepest dir first among equals; None when it is a leave or there is none.
pub fn joined(home: &Path, cwd: &str) -> Option<Join> {
    if cwd.is_empty() {
        return None;
    }
    let path = collective::normal(Path::new(cwd));
    let best =
        joins(home).into_iter().filter_map(|j| collective::inside(&path, &j.dir).map(|depth| (j.ms, depth, j))).max_by_key(|(ms, depth, _)| (*ms, *depth))?;
    Some(best.2).filter(|j| !j.goal.is_empty())
}

/// Whether a drone in `repo` may claim `t`: never an integrate task (the runner integrates,
/// integrate.rs), and a drone outside the goal's repos only claims tasks of its own repo (or of none).
pub fn may_claim(g: &Goal, t: &Task, repo: &str) -> bool {
    t.role != Role::Integrate && (goal::in_scope(g, repo) || t.repo.is_empty() || t.repo.eq_ignore_ascii_case(repo))
}

/// Why integrate tasks are refused to a drone's claim.
const INTEGRATE_REFUSED: &str = "integrate tasks are run by `borg hive run` or `borg hive integrate`";

/// Whether `session` in `cwd` may take `t` (`may_claim`, and a fix task only in a runner worktree or
/// a checkout of its `from`).
fn may_take(g: &Goal, t: &Task, repo: &str, session: &str, cwd: &str) -> bool {
    may_claim(g, t, repo) && t.from.as_deref().is_none_or(|from| runner_worktree(g, session, cwd) || checkout_of(cwd, from))
}

/// Whether `cwd` is a checkout of the commit `from` (its HEAD): where any session claims a fix task,
/// its tree already the combined one (`git worktree add <dir> <from>`), with no reset.
fn checkout_of(cwd: &str, from: &str) -> bool {
    if cwd.is_empty() {
        return false;
    }
    let mut cmd = std::process::Command::new("git");
    cmd.args(["-C", cwd, "rev-parse", "--verify", "-q", "HEAD"]).env("GIT_TERMINAL_PROMPT", "0");
    crate::runner::run_capped_status(cmd, None, Duration::from_secs(5)).is_some_and(|(st, out)| st.success() && String::from_utf8_lossy(&out).trim() == from)
}

/// Whether `cwd` is the worktree `borg hive run` made for the drone `session` is an iteration of
/// (`d<n>` in `.worktrees/hive-<goal>-<n>`): the only place a fix task's claim may reset to its `from`.
/// An engineer's checkout, or a session of its own, never is.
fn runner_worktree(g: &Goal, session: &str, cwd: &str) -> bool {
    let Some(n) = bound(session).and_then(|hs| hs.drone.strip_prefix('d').and_then(|n| n.parse::<usize>().ok())) else { return false };
    let cwd = Path::new(cwd);
    let repo = PathBuf::from(borg_trace::repo_key(&cwd.display().to_string()));
    let same = |a: &Path, b: &Path| a == b || std::fs::canonicalize(a).is_ok_and(|a| std::fs::canonicalize(b).is_ok_and(|b| a == b));
    n >= 1 && same(cwd, &run::worktree(&repo, &g.id, n))
}

/// A shell word that runs this borg (the hook's own executable), so a printed command works in any
/// harness's shell.
fn exe() -> String {
    std::env::current_exe().map_or_else(|_| "borg".into(), |e| queen::sh_quote(&e.display().to_string()))
}

/// The command a drone runs for `sub`, with its session and directory.
fn command(sub: &str, session: &str, cwd: &str) -> String {
    let dir = if cwd.is_empty() { String::new() } else { format!(" --dir {}", queen::sh_quote(cwd)) };
    format!("{} hive {sub} --session {}{dir}", exe(), queen::sh_quote(session))
}

fn short(session: &str) -> String {
    session.chars().take(8).collect()
}

fn count(home: &Path, kind: &str, session: &str, goal: &str, task: &str, repo: &str) {
    stats::count(home, kind, session, |ev| {
        ev.why = goal.into();
        ev.text = task.into();
        ev.repo = repo.into();
    });
}

/// What a drone is told about its claim `s`.
fn describe(g: &Goal, s: &TaskState, session: &str, cwd: &str) -> String {
    let t = &s.task;
    let mut out = format!("Your claim: {} ({}) {}", t.id, t.role.name(), t.title);
    if !t.files.is_empty() {
        out.push_str(&format!("; files {}", t.files.join(", ")));
    }
    if !t.deps.is_empty() && t.role != Role::Verify {
        out.push_str(&format!("; after {} (its verified work is in your tree)", t.deps.join(", ")));
    }
    let checks: Vec<&str> = oracle::checks(g, t).iter().map(|c| c.name.as_str()).collect();
    match t.role {
        Role::Verify => {
            out.push_str(&format!(
                ". Verify {} independently (its checks: {}), then end your turn with the line `VERDICT: pass` or `VERDICT: reject <why>` \
(or run `{}` or `{} \"why\"`).",
                t.of.as_deref().unwrap_or("its work"),
                checks.join(", "),
                command("verify --pass", session, cwd),
                command("verify --reject", session, cwd)
            ));
            if !t.deps.is_empty() {
                out.push_str(&format!(
                    " Its tree includes the verified work of {}: check {} together with it.",
                    t.deps.join(", "),
                    t.of.as_deref().unwrap_or("the work")
                ));
            }
            if !t.flags.is_empty() {
                out.push_str(&format!(" Flagged while the work was done: {}.", t.flags.join("; ")));
            }
        }
        _ => out.push_str(&format!(
            "; done when: {} pass. Ending your turn runs them; a suite slower than the stop hook runs with `{}`. Stuck: `{} \"why\"`.",
            checks.join(", "),
            command("done", session, cwd),
            command("block", session, cwd)
        )),
    }
    // A fix task's first note is why the integration failed: shown before any rework.
    if let Some(from) = &t.from {
        let start = if runner_worktree(g, session, cwd) { "Your worktree was reset to" } else { "You work on" };
        out.push_str(&format!(" {start} the integrated tree {from}; the integration failed: {}.", s.notes.first().map_or("", String::as_str)));
    }
    if s.rework > 0 && !s.notes.is_empty() {
        out.push_str(&format!(" Rework {} — the verifier's notes: {}", s.rework, s.notes.join(" | ")));
    }
    out
}

/// What a drone without a claim is told: the next open task and the exact claim command.
fn offer(g: &Goal, b: &Board, session: &str, repo: &str, cwd: &str, now: i64) -> String {
    match board::next_for(b, session, repo, now, |t| may_take(g, t, repo, session, cwd)) {
        Some(s) => format!(
            "No claim. Next open: {} ({}) {}. Claim it: `{}`",
            s.task.id,
            s.task.role.name(),
            s.task.title,
            command(&format!("claim {}", queen::sh_quote(&s.task.id)), session, cwd)
        ),
        None => "No claim, and no task is open to you now (`borg hive board`).".into(),
    }
}

/// The value of the last `marker` line of a state file.
fn last_marked<'a>(state: &'a str, marker: &str) -> Option<&'a str> {
    state.lines().rev().find_map(|l| l.strip_prefix(marker)).map(str::trim)
}

/// Renews `session`'s live claim when the last renewal is `HEARTBEAT_MS` old; whether it did.
fn heartbeat(home: &Path, hs: &mut HiveSession, b: &Board, session: &str, now: i64) -> bool {
    let renew = board::holding(b, session, now).filter(|_| now - hs.beat_ms > HEARTBEAT_MS);
    let done = renew.is_some_and(|s| board::append(&tasks_path(home, &hs.goal), &[Event::new(Ev::Heartbeat, &s.task.id, session, now)]).is_ok());
    if done {
        hs.beat_ms = now;
    }
    done
}

/// Claims this session lost and was not told of yet: their lines, and the marker that tells them.
fn lost_news(b: &Board, session: &str, state: &str) -> (Vec<String>, Vec<String>) {
    let told = hook::listed(state, LOST_MARKER);
    let keys: Vec<(String, &str)> = b.lost.iter().filter(|(by, _, _)| by == session).map(|(_, t, ms)| (format!("{t}@{ms}"), t.as_str())).collect();
    let fresh: Vec<&(String, &str)> = keys.iter().filter(|(k, _)| !told.contains(k.as_str())).collect();
    let lines = fresh
        .iter()
        .map(|(_, t)| format!("Your claim on {t} did not take effect (another drone won it, or it was not open to you): pick another task."))
        .collect();
    (lines, fresh.iter().map(|(k, _)| k.clone()).collect())
}

/// `out` (the inner answer) with `text` as the event's context, first; when the harness does not take
/// context here (`allowed` false) the text waits for the stop.
fn with_context(session: &str, hook_event: &str, text: String, allowed: bool, out: Option<Value>) -> Option<Value> {
    if text.is_empty() {
        return out;
    }
    if !allowed {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(wait_path(session)) {
            use std::io::Write;
            let _ = f.write_all(format!("{text}\n\n").as_bytes());
        }
        return out;
    }
    let mut out = out.unwrap_or_else(|| json!({}));
    let Some(o) = out.as_object_mut() else { return Some(out) };
    let spec = o.entry("hookSpecificOutput").or_insert_with(|| json!({"hookEventName": hook_event}));
    let merged = match spec.get("additionalContext").and_then(Value::as_str) {
        Some(theirs) if !theirs.is_empty() => format!("{text}\n\n{theirs}"),
        _ => text,
    };
    spec["additionalContext"] = Value::String(merged);
    if spec.get("hookEventName").is_none() {
        spec["hookEventName"] = Value::String(hook_event.into());
    }
    Some(out)
}

/// `out` with `text` before its block reason; a stop that would not block blocks with `text` only when
/// it `may_block` (its first round, so never a loop; or a handover, which follows a finished task).
fn with_reason(text: &str, out: Option<Value>, may_block: bool) -> Option<Value> {
    if text.trim().is_empty() {
        return out;
    }
    match out.as_ref().and_then(|o| o.get("reason")).and_then(Value::as_str) {
        Some(r) => Some(json!({"decision": "block", "reason": format!("{}\n\n{r}", text.trim_end())})),
        None if may_block => Some(json!({"decision": "block", "reason": text.trim_end()})),
        None => out,
    }
}

/// The hook's hive step, inside steering and around everything else it answers (`inner`). A session in
/// no goal (or a goal gone) gets `inner()` untouched.
pub fn around(h: &dyn Harness, home: &Path, event: &str, input: &Value, now: i64, inner: impl FnOnce() -> Option<Value>) -> Option<Value> {
    let started = Instant::now();
    let session = input.get("session_id").and_then(Value::as_str).unwrap_or("unknown");
    let Some(mut hs) = bound(session) else { return inner() };
    let Ok(g) = goal::load(home, &hs.goal) else { return inner() };
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
    let repo = queen::repo_name(cwd);
    let path = tasks_path(home, &g.id);
    let (events, end) = board::read_from(&path, 0);
    let b = board::fold(&events.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>());
    let ctx = Ctx { h, home, g: &g, session, cwd, repo: &repo, now, started };
    match event {
        "prompt" => {
            heartbeat(home, &mut hs, &b, session, now);
            let text = ctx.prompt(&mut hs, &b, &events, end);
            save(session, &hs);
            let allowed = h.caps().prompt_context && input.get("context") != Some(&Value::Bool(false));
            with_context(session, "UserPromptSubmit", text, allowed, inner())
        }
        "post_tool" => {
            if heartbeat(home, &mut hs, &b, session, now) {
                save(session, &hs);
            }
            let text = ctx.post_tool(&b, input);
            with_context(session, "PostToolUse", text, h.caps().tool_context, inner())
        }
        "stop" => ctx.stop(&mut hs, &b, input, started, inner),
        "end" => {
            let out = inner();
            if let Some(s) = board::holding(&b, session, now)
                && board::append(&path, &[Event::new(Ev::Released, &s.task.id, session, now).with_note("session ended")]).is_ok()
            {
                count(home, stats::HIVE_RELEASED, session, &g.id, &s.task.id, &repo);
            }
            out
        }
        _ => inner(),
    }
}

/// One hook event of a hive drone.
struct Ctx<'a> {
    h: &'a dyn Harness,
    home: &'a Path,
    g: &'a Goal,
    session: &'a str,
    cwd: &'a str,
    repo: &'a str,
    now: i64,
    started: Instant,
}

impl Ctx<'_> {
    /// The time now: the hook's `now` moved on by what the hook has taken (the oracle, a snapshot), so
    /// a line written late in the stop is not stamped with its start.
    fn clock(&self) -> i64 {
        self.now + self.started.elapsed().as_millis() as i64
    }

    fn head(&self) -> String {
        format!("[borg hive {}]", self.g.id)
    }

    /// The prompt's hive context: the claim (in full when it is new to the drone), lost claims, the
    /// board's changes since the last prompt, and contracts in the claim's scope not shown yet.
    fn prompt(&self, hs: &mut HiveSession, b: &Board, events: &[(u64, Event)], end: u64) -> String {
        let state = std::fs::read_to_string(hook::state_file(self.session)).unwrap_or_default();
        let mut parts: Vec<String> = Vec::new();
        let mut marks: Vec<String> = Vec::new();
        let last = last_marked(&state, TASK_MARKER);
        if last.is_none() {
            parts.push(inoculation(self.g));
        }
        if !goal::in_scope(self.g, self.repo) && !state.lines().any(|l| l == REPO_MARKER) {
            let names: Vec<&str> = self.g.repos.iter().map(|r| r.name.as_str()).collect();
            parts.push(format!("This repo ({}) is outside the goal's repos ({}): claim only tasks of this repo.", self.repo, names.join(", ")));
            marks.push(REPO_MARKER.into());
        }
        let holding = board::holding(b, self.session, self.now);
        let key = holding.map_or_else(|| "-".to_string(), |s| format!("{}@{}", s.task.id, s.live(self.now).map_or(0, |l| l.2)));
        match holding {
            Some(s) if last != Some(key.as_str()) => parts.push(describe(self.g, s, self.session, self.cwd)),
            Some(s) => parts.push(format!("Your claim: {} ({}) {}.", s.task.id, s.task.role.name(), s.task.title)),
            None => parts.push(offer(self.g, b, self.session, self.repo, self.cwd, self.now)),
        }
        if last != Some(key.as_str()) {
            marks.push(format!("{TASK_MARKER}{key}"));
        }
        let (lost, lost_keys) = lost_news(b, self.session, &state);
        parts.extend(lost);
        if !lost_keys.is_empty() {
            marks.push(format!("{LOST_MARKER}{}", lost_keys.join(",")));
        }
        // What other drones and engineers did since the last prompt, newest last.
        let cursor = if hs.cursor > end { 0 } else { hs.cursor };
        let changes: Vec<&Event> = events.iter().filter(|(at, e)| *at > cursor && e.by != self.session && e.ev != Ev::Heartbeat).map(|(_, e)| e).collect();
        if !changes.is_empty() {
            let skip = changes.len().saturating_sub(SHOW_CHANGES);
            let mut s = String::from("Board changes:");
            for e in &changes[skip..] {
                s.push_str(&format!("\n- {}", board::render_change(e)));
            }
            if skip > 0 {
                s.push_str(&format!("\n(+{skip} more: `borg hive board`)"));
            }
            parts.push(s);
        }
        hs.cursor = end;
        let mut shown: Vec<String> = Vec::new();
        if let Some(s) = holding {
            let all = contract::read(&contracts_path(self.home, &self.g.id));
            let current = contract::current(&all);
            let told = hook::listed(&state, CONTRACT_MARKER);
            let repo = if s.task.repo.is_empty() { self.repo } else { &s.task.repo };
            let fresh: Vec<&Contract> = contract::in_task_scope(&current, repo, &s.task.files, &s.task.title)
                .into_iter()
                .filter(|c| c.by != self.session && !told.contains(c.id.as_str()))
                .collect();
            if !fresh.is_empty() {
                let lines: Vec<String> = fresh.iter().map(|c| contract::render(c)).collect();
                parts.push(format!("Contracts in your claim's scope:\n{}\n{}", lines.join("\n"), contract::LABEL));
                shown = fresh.iter().map(|c| c.id.clone()).collect();
                marks.push(format!("{CONTRACT_MARKER}{}", shown.join(",")));
            }
        }
        if !marks.is_empty() {
            hook::append(self.session, &marks.join("\n"));
        }
        if parts.is_empty() {
            return String::new();
        }
        stats::count(self.home, stats::HIVE_SHOWN, self.session, |ev| {
            ev.lessons = shown;
            ev.why = self.g.id.clone();
            ev.repo = self.repo.into();
        });
        format!("{} {}", self.head(), parts.join("\n"))
    }

    /// After a tool call: contracts the edit lands in, and files outside the claim's scope (advice,
    /// never a refusal), each once.
    fn post_tool(&self, b: &Board, input: &Value) -> String {
        let tool = input.get("tool_name").and_then(Value::as_str).unwrap_or_default();
        let tool_input = input.get("tool_input").unwrap_or(&Value::Null);
        let Some(text) = hook::edit_text(tool, tool_input) else { return String::new() };
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let mut files: Vec<String> = [s(tool_input, "file_path"), s(tool_input, "notebook_path")].into_iter().collect();
        files.extend(tool_input.get("edits").and_then(Value::as_array).into_iter().flatten().map(|e| s(e, "file_path")));
        let files: Vec<String> =
            files.into_iter().filter(|f| !f.is_empty()).map(|f| Path::new(&f).strip_prefix(self.cwd).map(|p| p.display().to_string()).unwrap_or(f)).collect();
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        let state = std::fs::read_to_string(hook::state_file(self.session)).unwrap_or_default();
        let mut parts = Vec::new();
        let mut marks = Vec::new();
        let all = contract::read(&contracts_path(self.home, &self.g.id));
        let current = contract::current(&all);
        let told = hook::listed(&state, CONTRACT_MARKER);
        let hits: Vec<&Contract> =
            contract::matching(&current, self.repo, &refs, &text).into_iter().filter(|c| c.by != self.session && !told.contains(c.id.as_str())).collect();
        if !hits.is_empty() {
            for c in &hits {
                parts.push(format!("This edit lands in {}", contract::render(c)));
            }
            parts.push(contract::LABEL.into());
            marks.push(format!("{CONTRACT_MARKER}{}", hits.iter().map(|c| c.id.as_str()).collect::<Vec<_>>().join(",")));
        }
        if let Some(held) = board::holding(b, self.session, self.now).filter(|s| !s.task.files.is_empty()) {
            let globs: Vec<regex::Regex> = held.task.files.iter().map(|g| crate::assimilate_dir::glob(g)).collect();
            let warned = hook::listed(&state, SCOPE_MARKER);
            let outside: Vec<&str> = refs.iter().copied().filter(|f| !globs.iter().any(|g| g.is_match(f)) && !warned.contains(f) && !f.contains(',')).collect();
            for f in &outside {
                parts.push(format!(
                    "{f} is outside {}'s scope ({}): fine if the task needs it, but another drone may own it.",
                    held.task.id,
                    held.task.files.join(", ")
                ));
            }
            if !outside.is_empty() {
                marks.push(format!("{SCOPE_MARKER}{}", outside.join(",")));
            }
        }
        if !marks.is_empty() {
            hook::append(self.session, &marks.join("\n"));
        }
        if parts.is_empty() {
            return String::new();
        }
        stats::count(self.home, stats::HIVE_SHOWN, self.session, |ev| {
            ev.lessons = hits.iter().map(|c| c.id.clone()).collect();
            ev.why = self.g.id.clone();
            ev.repo = self.repo.into();
        });
        format!("{} {}", self.head(), parts.join("\n"))
    }

    /// The stop: with a claim, its oracle decides (block, publish blocked, or publish done and hand
    /// over the next task); without one, `inner()` with any waiting hive text.
    fn stop(&self, hs: &mut HiveSession, b: &Board, input: &Value, started: Instant, inner: impl FnOnce() -> Option<Value>) -> Option<Value> {
        let first_round = input.get("stop_hook_active").and_then(Value::as_bool) != Some(true);
        let waiting = std::fs::read_to_string(wait_path(self.session)).unwrap_or_default();
        let _ = std::fs::remove_file(wait_path(self.session));
        let path = tasks_path(self.home, &self.g.id);
        let Some(held) = board::holding(b, self.session, self.now) else {
            let state = std::fs::read_to_string(hook::state_file(self.session)).unwrap_or_default();
            let (lost, keys) = lost_news(b, self.session, &state);
            if !keys.is_empty() {
                hook::append(self.session, &format!("{LOST_MARKER}{}", keys.join(",")));
            }
            let mut text = waiting;
            if !lost.is_empty() {
                text.push_str(&format!("{} {}", self.head(), lost.join("\n")));
            }
            return with_reason(&text, inner(), first_round);
        };
        let task = &held.task;
        let claim = format!("{}@{}", task.id, held.live(self.now).map_or(0, |l| l.2));
        if hs.blocks_for != claim {
            hs.blocks_for = claim;
            hs.oracle_blocks = 0;
        }
        let total = crate::expert_route::stop_budget(self.h).unwrap_or(crate::expert_route::DEFAULT_STOP_BUDGET);
        let left = || total.saturating_sub(started.elapsed()).saturating_sub(crate::expert_route::STOP_MARGIN);
        // A verify task ends with a verdict, its last message's `VERDICT:` line, not with an oracle run.
        let said = (task.role == Role::Verify).then(|| verdict::parse(&crate::harness::last_message(self.h.caps(), input))).flatten();
        if let Some(said) = said {
            let mut text = waiting;
            let mut handed = false;
            match verdict::record(self.home, self.g, &task.id, &said, self.session, self.cwd, self.now) {
                Ok(r) => {
                    text.push_str(&format!("[borg hive] {}\n\n", r.summary(&task.id)));
                    if let Some(next) = self.hand_next(&path, left()) {
                        text.push_str(&format!("[borg hive] Next: {next}\n\n"));
                        handed = true;
                        hs.beat_ms = self.now;
                    }
                }
                Err(e) => text.push_str(&format!("[borg hive] verdict on {} not recorded: {e}\n\n", task.id)),
            }
            hs.oracle_blocks = 0;
            hs.blocks_for.clear();
            save(self.session, hs);
            // SAFETY: single-threaded here, as hook.rs main.
            unsafe { std::env::set_var(crate::expert_route::STOP_BUDGET_ENV, left().as_millis().to_string()) };
            return with_reason(&text, inner(), first_round || handed);
        }
        let verdict = if task.role == Role::Verify {
            oracle::Verdict::Fail {
                check: "verdict".into(),
                tail: format!(
                    "no verdict yet: end your turn with the line `VERDICT: pass` or `VERDICT: reject <why>`, or run `{}` or `{} \"why\"`",
                    command("verify --pass", self.session, self.cwd),
                    command("verify --reject", self.session, self.cwd)
                ),
            }
        } else {
            let budget = left().saturating_sub(CONSCIENCE_RESERVE);
            match budget < MIN_ORACLE {
                true => {
                    oracle::Verdict::Timeout { check: oracle::checks(self.g, task).first().map_or_else(String::new, |c| c.name.clone()), after: Duration::ZERO }
                }
                false => oracle::run(self.g, task, Path::new(self.cwd), Some(budget)),
            }
        };
        if verdict != oracle::Verdict::Pass && hs.oracle_blocks < MAX_ORACLE_BLOCKS {
            hs.oracle_blocks += 1;
            save(self.session, hs);
            let _ = oracle::log(self.home, &self.g.id, &task.id, &verdict, self.now);
            count(self.home, stats::HIVE_ORACLE_FAIL, self.session, &self.g.id, &task.id, self.repo);
            let what = match &verdict {
                oracle::Verdict::Fail { check, tail } => format!("{check} failed (attempt {} of {MAX_ORACLE_BLOCKS}).\n{}", hs.oracle_blocks, tail.trim_end()),
                oracle::Verdict::Timeout { check, .. } => format!(
                    "{check} did not finish within the stop hook's time (attempt {} of {MAX_ORACLE_BLOCKS}): run the checks with `{}`, which has no hook time limit.",
                    hs.oracle_blocks,
                    command("done", self.session, self.cwd)
                ),
                oracle::Verdict::Pass => String::new(),
            };
            let reason = format!(
                "{waiting}[borg hive] {} is not done: {what}\nFix it, or run `{}` (no hook time limit), or `{} \"why\"` if you are stuck.",
                task.id,
                command("done", self.session, self.cwd),
                command("block", self.session, self.cwd)
            );
            // No conscience round on unfinished work: its model call would judge work still changing.
            return Some(json!({"decision": "block", "reason": reason}));
        }
        let mut text = waiting;
        let mut handed = false;
        if let Some(check) = verdict.check() {
            // At the cap: the drone may stop; the board says why the task stands still.
            let (ev, note) = match task.role {
                Role::Verify => (Ev::Released, format!("no verdict after {MAX_ORACLE_BLOCKS} stops")),
                _ => (Ev::Blocked, format!("oracle failed {MAX_ORACLE_BLOCKS} times at stop: {check}")),
            };
            let _ = board::append(&path, &[Event::new(ev, &task.id, self.session, self.now).with_note(&note)]);
            text.push_str(&format!(
                "[borg hive] {} is {} on the board ({note}); an engineer reopens it with `borg hive open {} --id {} --again`.\n\n",
                task.id,
                ev.name(),
                self.g.id,
                task.id
            ));
        } else if let Some(next) = self.done(held, input, &path, &left) {
            text.push_str(&next);
            handed = true;
            hs.beat_ms = self.now;
        }
        hs.oracle_blocks = 0;
        hs.blocks_for.clear();
        save(self.session, hs);
        // The conscience and the experts keep inside what is left of the hook's time.
        // SAFETY: single-threaded here (the oracle's reader threads have finished), as hook.rs main.
        unsafe { std::env::set_var(crate::expert_route::STOP_BUDGET_ENV, left().as_millis().to_string()) };
        // A new claim reaches the drone on any round: it must not stop holding a task it never heard of.
        with_reason(&text, inner(), first_round || handed)
    }

    /// The oracle passed at the stop: `done` and the verify task's open in one write, then the next
    /// open task claimed for this drone. The text to block with when it got one. The done's snapshot
    /// keeps within what is `left` of the hook's time; the next claim is made only when time is left.
    fn done(&self, held: &TaskState, input: &Value, path: &Path, left: &dyn Fn() -> Duration) -> Option<String> {
        let task = &held.task;
        let message = crate::harness::plan(self.h.caps(), input);
        let since = held.live(self.now).map_or(i64::MIN, |l| l.2);
        let (done, check) = done_events(self.home, self.g, held, self.session, Path::new(self.cwd), since, left());
        let vid = check.id.clone();
        let ms = done.ms;
        board::append(path, &[done.with_note(&hook::one_line(&message, 200)), Event::open(check, self.session, ms)]).ok()?;
        count(self.home, stats::HIVE_DONE, self.session, &self.g.id, &task.id, self.repo);
        let next = self.hand_next(path, left())?;
        Some(format!("[borg hive] {} done, verify task {vid} opened. Next: {next}\n\n", task.id))
    }

    /// Claims the next open task for this drone (its snapshot within `left`): what it is told of it.
    /// None when no task is open, the claim was lost or refused, or less than `MIN_ORACLE` of the
    /// hook's time is left: the next prompt offers the task instead.
    fn hand_next(&self, path: &Path, left: Duration) -> Option<String> {
        if left < MIN_ORACLE {
            return None;
        }
        let now = self.clock();
        let b = board::fold(&board::read(path));
        let next = board::next_for(&b, self.session, self.repo, now, |t| may_take(self.g, t, self.repo, self.session, self.cwd))?;
        let won = claim(self.home, self.g, self.session, &next.task, self.cwd, now, left).ok()?;
        count(self.home, stats::HIVE_CLAIM, self.session, &self.g.id, &won.task.id, self.repo);
        let now = self.clock();
        hook::append(self.session, &format!("{TASK_MARKER}{}@{}", won.task.id, won.live(now).map_or(0, |l| l.2)));
        Some(describe(self.g, &won, self.session, self.cwd))
    }
}

/// The `done` of the claim `held` (claimed at `since`) by `session` in `dir`, with a snapshot of the
/// work (`head`, verdict.rs, `within` that time), and the verify task it opens, listing the guard's
/// flags of the claim. The done is stamped once its snapshot is taken: the line lands at its ms.
fn done_events(home: &Path, g: &Goal, held: &TaskState, session: &str, dir: &Path, since: i64, within: Duration) -> (Event, Task) {
    let head = verdict::snapshot(dir, within).unwrap_or_default();
    let now = learn::now_ms();
    let mut done = Event::new(Ev::Done, &held.task.id, session, now);
    done.head = head;
    let flags: Vec<String> = guard::flags_of(home, &g.id, session, since, now).iter().map(guard::render).collect();
    let mut check = verify_task(&held.task, held.rework, session);
    check.flags = flags;
    (done, check)
}

/// The verify task a `done` of `t` opens: the same scope, deps and checks, never claimable by its
/// author (the session, and the drone it is an iteration of).
pub fn verify_task(t: &Task, rework: u32, author: &str) -> Task {
    let drone = bound(author).map(|hs| hs.drone).filter(|d| !d.is_empty());
    Task {
        id: format!("{}.v{}", t.id, rework + 1),
        role: Role::Verify,
        title: format!("verify {}: {}", t.id, t.title),
        repo: t.repo.clone(),
        files: t.files.clone(),
        deps: t.deps.clone(),
        accept: t.accept.clone(),
        of: Some(t.id.clone()),
        not_by: std::iter::once(author.to_string()).chain(drone).collect(),
        flags: Vec::new(),
        from: None,
    }
}

/// Why a claim took nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Refused {
    /// Another drone won it, it was not open to this one, or the board could not be written.
    Lost,
    /// A dep's verified work cannot be brought into this tree: why.
    Deps(String),
}

/// Claims `task` for `session` in `cwd` (a claim and a heartbeat in one write), waits for rivals' lines
/// to land, and reads the board back: the task when the claim won. A claim of work carries the tree it
/// starts from (`base`, verdict.rs), which the verdict diffs against the done's: the base of this
/// drone's (or, without a drone, this session's) first claim of the task when it had one (a tree that
/// an earlier attempt already changed, say a test it weakened, is no base: the rework's diff must
/// show it), else a snapshot taken now, `within` that time. The claim is stamped after the snapshot
/// (`now` moved on by it), so it lands at its ms and never wins a race it reached late. A fix task
/// (`from`, integrate.rs) starts from the combined tree: a runner drone in its own worktree claims it
/// with `from` as its base, and once the claim won the worktree is reset to it (`reset_to`); any
/// other session only in a checkout of `from` (`checkout_of`), never reset, its base a snapshot.
///
/// Work with deps starts from their verified work: before the base is taken, each dep's accepted
/// attempt (board.rs `closure` order) is brought into the tree (verdict.rs `bring_in`), and the claim
/// names them (`deps_in`). The base then holds them, so the work's diff is its own. A dep that cannot
/// be brought in (this tree's own changes overlap it) refuses the claim, and a lost claim takes out
/// what it brought in: either way the tree is as it was.
fn claim(home: &Path, g: &Goal, session: &str, task: &Task, cwd: &str, now: i64, within: Duration) -> Result<TaskState, Refused> {
    let path = tasks_path(home, &g.id);
    let drone = bound(session).map(|hs| hs.drone).unwrap_or_default();
    let reset = task.from.is_some() && runner_worktree(g, session, cwd);
    if task.from.as_deref().is_some_and(|from| !reset && !checkout_of(cwd, from)) {
        return Err(Refused::Lost);
    }
    let t0 = Instant::now();
    let (deps_in, brought) = bring_deps(&path, task, cwd, within)?;
    let take_out = || take_out_all(cwd, &brought, within.saturating_sub(t0.elapsed()));
    let rest = within.saturating_sub(t0.elapsed());
    // A fix task's claim keeps a snapshot of what the reset will discard in its note: recoverable.
    let (base, note) = match (&task.from, task.role != Role::Verify && !cwd.is_empty()) {
        (Some(from), _) if reset => {
            (from.clone(), verdict::snapshot(Path::new(cwd), rest).map(|s| format!("worktree before the reset: {s}")).unwrap_or_default())
        }
        (_, true) => {
            (first_base(&board::read(&path), &task.id, session, &drone).or_else(|| verdict::snapshot(Path::new(cwd), rest)).unwrap_or_default(), String::new())
        }
        (_, false) => (String::new(), String::new()),
    };
    let left = within.saturating_sub(t0.elapsed());
    let now = now + t0.elapsed().as_millis() as i64;
    let mut c = Event::new(Ev::Claim, &task.id, session, now).with_drone(&drone).with_note(&note);
    c.base = base;
    c.deps_in = deps_in;
    let id = task.id.as_str();
    if board::append(&path, &[c, Event::new(Ev::Heartbeat, id, session, now)]).is_err() {
        take_out();
        return Err(Refused::Lost);
    }
    std::thread::sleep(CLAIM_SETTLE);
    let b = board::fold(&board::read(&path));
    let Some(s) = b.tasks.get(id).filter(|s| s.live(now).is_some_and(|(by, _, _)| by == session)) else {
        take_out();
        return Err(Refused::Lost);
    };
    if let Some(from) = task.from.as_ref().filter(|_| reset)
        && let Err(why) = reset_to(Path::new(cwd), from, left)
    {
        let _ = board::append(&path, &[Event::new(Ev::Released, id, session, learn::now_ms()).with_note(&why)]);
        return Err(Refused::Lost);
    }
    if let Some(mut hs) = bound(session) {
        hs.beat_ms = now;
        save(session, &hs);
    }
    Ok(s.clone())
}

/// A dep's change a claim brought into its tree: the (base, head) of its accepted attempt.
type Brought = (String, String);

/// Brings the verified work of `task`'s deps into `cwd` for its claim (see `claim`), `within` that
/// time: the claim's `deps_in`, and the (base, head) of each change it applied, to take out if the
/// claim is lost. Nothing for a verify or fix task, a task without deps, or no directory.
fn bring_deps(path: &Path, task: &Task, cwd: &str, within: Duration) -> Result<(Vec<String>, Vec<Brought>), Refused> {
    if task.role == Role::Verify || task.from.is_some() || task.deps.is_empty() || cwd.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let t0 = Instant::now();
    let events = board::read(path);
    let b = board::fold(&events);
    let (mut deps_in, mut brought): (Vec<String>, Vec<Brought>) = (Vec::new(), Vec::new());
    let refuse = |brought: &[Brought], why: String| {
        take_out_all(cwd, brought, within.saturating_sub(t0.elapsed()));
        Refused::Deps(why)
    };
    for d in board::closure(&b, task) {
        let Some(a) = verdict::accepted(&events, &b, &d).filter(|a| !a.base.is_empty() && !a.head.is_empty()) else {
            return Err(refuse(&brought, format!("{d} has no verified work to bring into this tree")));
        };
        match verdict::bring_in(Path::new(cwd), &a.base, &a.head, within.saturating_sub(t0.elapsed())) {
            Ok(applied) => {
                if applied {
                    brought.push((a.base.clone(), a.head.clone()));
                }
                deps_in.push(format!("{d}@{}", a.head));
            }
            Err(tail) => {
                let why = format!("cannot bring {d}'s verified work into this tree: {tail} (a drone whose tree does not overlap it can claim {})", task.id);
                return Err(refuse(&brought, why));
            }
        }
    }
    Ok((deps_in, brought))
}

/// Takes the changes `bring_deps` applied out of `cwd` again, the last first, all `within` what is
/// left of the claim's time (at least `MIN_ORACLE`, as `reset_to`: the undo comes first).
fn take_out_all(cwd: &str, brought: &[Brought], within: Duration) {
    let deadline = Instant::now() + within.max(MIN_ORACLE);
    for (base, head) in brought.iter().rev() {
        let _ = verdict::take_out(Path::new(cwd), base, head, deadline.saturating_duration_since(Instant::now()));
    }
}

/// Resets the runner worktree `dir` to `from` (a fix task's start): `checkout -f --detach`, then
/// `clean -fd` (ignored files, build output, stay), each within `RESET_TIMEOUT` and both `within`.
/// What it discards was in no accepted attempt: every one is inside `from`.
///
/// # Errors
/// A git call that fails or runs out of time: the claim is released with the reason.
fn reset_to(dir: &Path, from: &str, within: Duration) -> Result<(), String> {
    let deadline = Instant::now() + within.max(MIN_ORACLE);
    let git = |args: &[&str]| {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(dir).args(args).env("GIT_TERMINAL_PROMPT", "0");
        let left = deadline.saturating_duration_since(Instant::now()).min(RESET_TIMEOUT);
        !left.is_zero() && crate::runner::run_capped_status(cmd, None, left).is_some_and(|(s, _)| s.success())
    };
    if !git(&["checkout", "-q", "-f", "--detach", from]) {
        return Err(format!("could not reset the worktree to the integrated tree {from}"));
    }
    if !git(&["clean", "-fdq"]) {
        return Err(format!("could not clean the worktree after the reset to {from}"));
    }
    Ok(())
}

/// The base of the first claim of `task` by `drone` (by `session` when it has none) that has one.
fn first_base(events: &[Event], task: &str, session: &str, drone: &str) -> Option<String> {
    events
        .iter()
        .filter(|e| e.ev == Ev::Claim && e.task == task && !e.base.is_empty())
        .filter(|e| if drone.is_empty() { e.by == session } else { e.drone == drone })
        .min_by_key(|e| e.ms)
        .map(|e| e.base.clone())
}

// ---------------------------------------------------------------------------------------------------
// The CLI.

/// `borg hive`'s flags (completions.rs).
pub const FLAGS: &[&str] = &[
    "--session",
    "--dir",
    "--spec",
    "--spec-file",
    "--oracle",
    "--timeout",
    "--budget",
    "--repo",
    "--task",
    "--id",
    "--role",
    "--title",
    "--files",
    "--deps",
    "--accept",
    "--note",
    "--scope",
    "--supersedes",
    "--reject",
    "--pass",
    "--again",
    "--json",
    "--watch",
    "--drones",
    "--harness",
    "--model",
    "--base",
    "--iter-timeout",
    "--dry-run",
    "--here",
    "--drone",
    "--hidden-dir",
    "--hidden",
    "--hidden-timeout",
];

/// The flags that take no value.
const SWITCHES: &[&str] = &["--pass", "--again", "--json", "--watch", "--dry-run", "--here"];

const USAGE: &str = "usage: borg hive <command> [--dir DIR] [--session ID|DRONE]
  init <goal> --spec TEXT|--spec-file F --oracle NAME=CMD... --budget USD [--timeout S] [--repo NAME[@NODE]]...
       [--task \"ID|role|title|files,..|deps,..\"]...       a goal, its oracle and its first tasks
       [--hidden-dir DIR --hidden CMD... [--hidden-timeout S]]  held-out checks outside the repo, run after verdicts
  score <goal>                                              the hidden oracle scores the judged tasks not scored yet
  integrate <goal>                                          merge the accepted work and run the oracle on it (git 2.40+)
  board [<goal>] [--json] [--watch]                         the board: tasks by status, holders, rework
  join <goal> [--session ID|DRONE]                          a running drone joins the goal
  join <goal> --here [--drone NAME]                         every new session in this repo (or worktree) joins it
  leave [--here]                                            the drone leaves (its claim is released); --here: new
                                                            sessions here join no more (joined ones stay)
  run <goal> --drones N [--harness H] [--model M] [--base REF] [--iter-timeout S] [--dry-run]
                                                            N headless drones in worktrees until done or the budget is spent
  open <goal> --id ID --role work|plan|verify|integrate --title T [--repo R] [--files a,b] [--deps x,y]
       [--accept NAME=CMD]... [--again]                     an engineer opens a task (--again reopens a blocked one)
  drone commands (the goal is the session's):
  claim [TASK] | progress \"note\" | release | block \"why\" | done [--note N]
  verify --pass | --reject \"why\" (or end the turn with `VERDICT: pass|reject <why>`) | contract \"text\" --scope a,b [--supersedes ID]";

/// The command line: words, flag values (repeatable), and switches.
struct Args<'a> {
    words: Vec<&'a str>,
    values: Vec<(&'a str, &'a str)>,
    switches: Vec<&'a str>,
}

impl<'a> Args<'a> {
    fn parse(args: &[&'a str]) -> Result<Args<'a>, String> {
        let mut out = Args { words: Vec::new(), values: Vec::new(), switches: Vec::new() };
        let mut i = 0;
        while i < args.len() {
            let a = args[i];
            if SWITCHES.contains(&a) {
                out.switches.push(a);
            } else if FLAGS.contains(&a) {
                let v = args.get(i + 1).ok_or_else(|| format!("{a} needs a value"))?;
                out.values.push((a, v));
                i += 1;
            } else if a.starts_with("--") {
                return Err(format!("unknown flag {a}"));
            } else {
                out.words.push(a);
            }
            i += 1;
        }
        Ok(out)
    }

    fn value(&self, f: &str) -> Option<&'a str> {
        self.values.iter().rev().find(|(k, _)| *k == f).map(|(_, v)| *v)
    }

    fn all(&self, f: &str) -> Vec<&'a str> {
        self.values.iter().filter(|(k, _)| *k == f).map(|(_, v)| *v).collect()
    }

    fn on(&self, f: &str) -> bool {
        self.switches.contains(&f)
    }

    fn list(&self, f: &str) -> Vec<String> {
        self.value(f).map(split_list).unwrap_or_default()
    }
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
}

fn fail(msg: impl std::fmt::Display) -> i32 {
    eprintln!("borg hive: {msg}");
    1
}

fn valid_task_id(id: &str) -> bool {
    (1..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// `ID|role|title|files,..|deps,..` (`--task`).
fn parse_task(s: &str) -> Result<Task, String> {
    let f: Vec<&str> = s.split('|').map(str::trim).collect();
    let (Some(id), Some(role), Some(title)) = (f.first(), f.get(1), f.get(2)) else {
        return Err(format!("--task {s:?}: give ID|role|title[|files,..[|deps,..]]"));
    };
    let role = Role::parse(role).ok_or_else(|| format!("--task {s:?}: role is plan, work, verify or integrate"))?;
    new_task(id, role, title, "", f.get(3).map(|x| split_list(x)).unwrap_or_default(), f.get(4).map(|x| split_list(x)).unwrap_or_default(), Vec::new())
}

fn new_task(id: &str, role: Role, title: &str, repo: &str, files: Vec<String>, deps: Vec<String>, accept: Vec<Check>) -> Result<Task, String> {
    if !valid_task_id(id) {
        return Err(format!("invalid task id {id:?}: use letters, digits, '.', '_', '-', at most 40"));
    }
    if title.trim().is_empty() {
        return Err(format!("task {id} needs a title"));
    }
    Ok(Task {
        id: id.into(),
        role,
        title: title.trim().into(),
        repo: repo.into(),
        files,
        deps,
        accept,
        of: None,
        not_by: Vec::new(),
        flags: Vec::new(),
        from: None,
    })
}

/// Why `tasks` cannot be opened together: a dep that names no task (of the batch, or one `known`), or a
/// cycle among them (a after b after a). The board would never finish either.
fn check_deps(tasks: &[Task], known: impl Fn(&str) -> bool) -> Result<(), String> {
    let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    for t in tasks {
        if let Some(d) = t.deps.iter().find(|d| !ids.contains(&d.as_str()) && !known(d)) {
            return Err(format!("task {}: its dep {d} names no task", t.id));
        }
    }
    // A task whose deps in the batch are all placed is placed next; what is never placed is on a cycle.
    let mut placed: Vec<&str> = Vec::new();
    while let Some(t) =
        tasks.iter().find(|t| !placed.contains(&t.id.as_str()) && t.deps.iter().all(|d| placed.contains(&d.as_str()) || !ids.contains(&d.as_str())))
    {
        placed.push(&t.id);
    }
    let cycle: Vec<&str> = ids.into_iter().filter(|i| !placed.contains(i)).collect();
    match cycle.is_empty() {
        true => Ok(()),
        false => Err(format!("tasks {} depend on each other in a cycle", cycle.join(", "))),
    }
}

/// Where the command runs: the store the directory routes to, and the directory.
struct Here {
    home: PathBuf,
    cwd: String,
    now: i64,
}

impl Here {
    fn locate(a: &Args) -> Result<Here, i32> {
        let dir = a.value("--dir").map_or_else(|| std::env::current_dir().unwrap_or_default(), PathBuf::from);
        let cwd = std::path::absolute(&dir).unwrap_or(dir).display().to_string();
        let route = match collective::load() {
            Ok(cfg) => collective::route(cfg.as_ref(), &cwd, &learn::home()),
            Err(e) => return Err(fail(format!("invalid collectives config: {e}"))),
        };
        let collective::Route::Collective { home, .. } = route else {
            eprintln!("off: {cwd} belongs to no collective; borg records nothing here");
            return Err(1);
        };
        // The drone's scratch state, as its hooks keep it (hook.rs main).
        if std::env::var_os("BORG_STATE").is_none() {
            // SAFETY: single-threaded: the command has started no thread yet.
            unsafe { std::env::set_var("BORG_STATE", home.join("state")) };
        }
        Ok(Here { home, cwd, now: learn::now_ms() })
    }

    fn engineer(&self) -> String {
        format!("engineer:{}", learn::engineer(&self.cwd))
    }

    /// The session a drone command acts for, and its goal: `--session` names a hive session, or a drone
    /// (`borg top`'s designation or session prefix); without it, the one drone of this repo.
    fn drone(&self, named: Option<&str>) -> Result<(String, HiveSession), String> {
        if let Some(n) = named
            && let Some(hs) = bound(n)
        {
            return Ok((n.to_string(), hs));
        }
        let drones = top::drones(&self.home, false, self.now);
        let d = veto::target(&drones, &queen::repo_name(&self.cwd), named)?;
        let session = d.p.session.clone();
        let hs = bound(&session).ok_or_else(|| {
            format!("drone {} ({}) is in no hive goal: `borg hive join <goal> --session {}`", d.designation, short(&session), short(&session))
        })?;
        Ok((session, hs))
    }

    /// The goal a board command shows: named, the session's, or the only one.
    fn goal(&self, named: Option<&str>, session: Option<&str>) -> Result<Goal, String> {
        if let Some(n) = named {
            return goal::load(&self.home, n);
        }
        if let Some(hs) = session.and_then(bound) {
            return goal::load(&self.home, &hs.goal);
        }
        match goal::list(&self.home).as_slice() {
            [one] => goal::load(&self.home, one),
            [] => Err("no hive goal in this collective (`borg hive init`)".into()),
            many => Err(format!("{} goals ({}): name one", many.len(), many.join(", "))),
        }
    }

    fn board(&self, g: &Goal) -> Board {
        board::fold(&board::read(&tasks_path(&self.home, &g.id)))
    }
}

/// `borg hive <command>` (see `USAGE`). Exit 0, 1 on an error, 2 on a usage error, 3 when a claim lost.
pub fn main(args: &[&str]) -> i32 {
    let a = match Args::parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("borg hive: {e}\n{USAGE}");
            return 2;
        }
    };
    let Some((&cmd, rest)) = a.words.split_first() else {
        eprintln!("{USAGE}");
        return 2;
    };
    let here = match Here::locate(&a) {
        Ok(h) => h,
        Err(code) => return code,
    };
    let session = a.value("--session");
    match (cmd, rest) {
        ("init", [id]) => init(&here, &a, id),
        ("board", [] | [_]) => match here.goal(rest.first().copied(), session) {
            Ok(g) => board_cmd(&here, &g, &a),
            Err(e) => fail(e),
        },
        ("join", [id]) if a.on("--here") => join_here(&here, id, a.value("--drone")),
        ("join", [id]) => join(&here, id, session),
        ("leave", []) if a.on("--here") => leave_here(&here),
        ("run", [id]) => run_cmd(&here, &a, id),
        ("sim-drone", []) if std::env::var_os(crate::harness::TEST_ENV).is_some() => sim_drone::main(&here),
        ("leave", []) => match here.drone(session) {
            Ok((s, hs)) => {
                let b = goal::load(&here.home, &hs.goal).map(|g| here.board(&g)).unwrap_or_default();
                if let Some(t) = board::holding(&b, &s, here.now) {
                    let _ = board::append(&tasks_path(&here.home, &hs.goal), &[Event::new(Ev::Released, &t.task.id, &s, here.now).with_note("left the goal")]);
                }
                unbind(&s);
                println!("{} left hive goal {}", short(&s), hs.goal);
                0
            }
            Err(e) => fail(e),
        },
        ("open", [id]) => open(&here, &a, id),
        ("claim", [] | [_]) => claim_cmd(&here, session, rest.first().copied()),
        ("progress" | "block", [_, ..]) => {
            let ev = if cmd == "progress" { Ev::Progress } else { Ev::Blocked };
            held_event(&here, session, ev, &rest.join(" "))
        }
        ("release", []) => held_event(&here, session, Ev::Released, ""),
        ("done", []) => done_cmd(&here, session, a.value("--note").unwrap_or_default()),
        ("score", [id]) => score_cmd(&here, id),
        ("integrate", [id]) => integrate_cmd(&here, id),
        ("verify", []) => match (a.on("--pass"), a.value("--reject")) {
            (true, None) => verdict_cmd(&here, session, verdict::Said::Pass),
            (false, Some(note)) if !note.trim().is_empty() => verdict_cmd(&here, session, verdict::Said::Reject(note.trim().into())),
            _ => {
                eprintln!("borg hive verify: give --pass or --reject \"why\"");
                2
            }
        },
        ("contract", [_, ..]) => contract_cmd(&here, session, &rest.join(" "), a.list("--scope"), a.value("--supersedes")),
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

fn init(here: &Here, a: &Args, id: &str) -> i32 {
    let spec = match (a.value("--spec"), a.value("--spec-file")) {
        (Some(s), _) => s.to_string(),
        (None, Some(f)) => match std::fs::read_to_string(f) {
            Ok(s) => s,
            Err(e) => return fail(format!("cannot read {f}: {e}")),
        },
        (None, None) => {
            eprintln!("borg hive init: give the goal's --spec TEXT or --spec-file F");
            return 2;
        }
    };
    let timeout = a.value("--timeout").and_then(|t| t.parse().ok()).unwrap_or(600);
    let oracle: Result<Vec<Check>, String> = a.all("--oracle").into_iter().map(|o| Check::parse(o, timeout)).collect();
    let oracle = match oracle {
        Ok(o) if !o.is_empty() => o,
        Ok(_) => {
            eprintln!("borg hive init: give at least one --oracle NAME=CMD: done means the oracle passes");
            return 2;
        }
        Err(e) => return fail(e),
    };
    let Some(budget_usd) = a.value("--budget").and_then(|b| b.parse::<f64>().ok()).filter(|b| *b >= 0.0) else {
        eprintln!("borg hive init: give the goal's --budget USD");
        return 2;
    };
    let repos = a
        .all("--repo")
        .into_iter()
        .map(|r| match r.split_once('@') {
            Some((name, node)) => RepoScope { name: name.into(), node: node.into() },
            None => RepoScope { name: r.into(), node: String::new() },
        })
        .collect();
    let tasks: Result<Vec<Task>, String> = a.all("--task").into_iter().map(parse_task).collect();
    let tasks = match tasks.and_then(|t| check_deps(&t, |_| false).map(|()| t)) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let hidden = match (a.value("--hidden-dir"), a.all("--hidden")) {
        (None, cmds) if cmds.is_empty() => None,
        (Some(dir), cmds) if !cmds.is_empty() => {
            let timeout_s = a.value("--hidden-timeout").and_then(|t| t.parse().ok()).unwrap_or(600);
            let h = goal::Hidden { dir: dir.into(), commands: cmds.into_iter().map(String::from).collect(), timeout_s };
            if let Err(e) = goal::validate_hidden(&h, run::repo_of(Path::new(&here.cwd)).as_deref()) {
                return fail(e);
            }
            Some(h)
        }
        _ => {
            eprintln!("borg hive init: a hidden oracle needs --hidden-dir DIR and at least one --hidden CMD");
            return 2;
        }
    };
    let author = learn::engineer(&here.cwd);
    let g = Goal { v: 1, id: id.into(), spec, oracle, budget_usd, repos, author: author.clone(), created_ms: here.now, sig: String::new(), hidden };
    if let Err(e) = goal::create(&here.home, &g) {
        return fail(e);
    }
    let by = format!("engineer:{author}");
    let opens: Vec<Event> = tasks.into_iter().map(|t| Event::open(t, &by, here.now)).collect();
    if let Err(e) = board::append(&tasks_path(&here.home, id), &opens) {
        return fail(e);
    }
    println!(
        "hive goal {id}: {} task(s), oracle {}; {}",
        opens.len(),
        g.oracle.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", "),
        dir(&here.home, id).display()
    );
    println!("join a running drone: borg hive join {id} --session <ID|DRONE>; or start drones with {ENV}={id}");
    0
}

fn board_cmd(here: &Here, g: &Goal, a: &Args) -> i32 {
    loop {
        let now = learn::now_ms();
        let b = here.board(g);
        if a.on("--json") {
            let rows: Vec<Value> = b
                .tasks
                .values()
                .map(|s| {
                    let (by, renewed) = match &s.status {
                        Status::Claimed { by, renewed, .. } => (by.clone(), *renewed),
                        Status::Done { by, ms } | Status::Verified { by, ms } => (by.clone(), *ms),
                        Status::Blocked { by, .. } => (by.clone(), 0),
                        Status::Open => (String::new(), 0),
                    };
                    json!({"id": s.task.id, "role": s.task.role, "title": s.task.title, "status": s.shown_status(now), "by": by, "renewed": renewed,
                        "rework": s.rework, "deps": s.task.deps})
                })
                .collect();
            println!("{}", Value::Array(rows));
            return 0;
        }
        let head = format!(
            "hive goal {} (budget ${:.2}, by {})\n{}\n\n",
            g.id,
            g.budget_usd,
            if g.author.is_empty() { "?" } else { &g.author },
            g.spec.lines().next().unwrap_or_default()
        );
        let body = board::render(&b, now, queen::width());
        if !a.on("--watch") {
            print!("{head}{body}");
            return 0;
        }
        print!("\x1b[2J\x1b[H{head}{body}");
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn join(here: &Here, id: &str, named: Option<&str>) -> i32 {
    if let Err(e) = goal::load(&here.home, id) {
        return fail(e);
    }
    let drones = top::drones(&here.home, false, here.now);
    let session = match (veto::target(&drones, &queen::repo_name(&here.cwd), named), named) {
        (Ok(d), _) => d.p.session.clone(),
        // A session with no hook yet (a drone being launched): taken as named.
        (Err(_), Some(n)) if presence::safe(n) == n && n.len() >= 8 => n.to_string(),
        (Err(e), _) => return fail(e),
    };
    if let Err(e) = bind(&here.home, &session, id) {
        return fail(e);
    }
    println!("{} joined hive goal {id}; its next prompt shows its claim or the next open task", short(&session));
    0
}

/// The directory a `--here` join names: the git toplevel of the command's directory (a worktree's own),
/// else the directory.
fn here_dir(here: &Here) -> String {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["-C", &here.cwd, "rev-parse", "--show-toplevel"]);
    match crate::runner::run_capped_status(cmd, None, Duration::from_secs(5)) {
        Some((s, out)) if s.success() && !out.trim_ascii().is_empty() => String::from_utf8_lossy(out.trim_ascii()).into_owned(),
        _ => here.cwd.clone(),
    }
}

fn join_here(here: &Here, id: &str, drone: Option<&str>) -> i32 {
    if let Err(e) = goal::load(&here.home, id) {
        return fail(e);
    }
    let dir = here_dir(here);
    let drone = drone.map_or_else(|| format!("j-{}", &blake3::hash(dir.as_bytes()).to_hex()[..6]), String::from);
    let harness = crate::harness::named_current().map(|h| h.name().to_string()).unwrap_or_default();
    let j = Join { v: 1, ms: here.now, goal: id.into(), dir: dir.clone(), drone: drone.clone(), session: String::new(), harness, by: "join".into() };
    if let Err(e) = append_join(&here.home, id, &j) {
        return fail(e);
    }
    println!("joined {id} as {drone}: the next session in {dir} gets the board (`borg hive leave --here` undoes it)");
    0
}

fn leave_here(here: &Here) -> i32 {
    let dir = here_dir(here);
    let Some(j) = joined(&here.home, &dir) else { return fail(format!("{dir} joined no hive goal")) };
    let leave = Join { v: 1, ms: here.now.max(j.ms + 1), goal: String::new(), dir: dir.clone(), by: "join".into(), ..Join::default() };
    if let Err(e) = append_join(&here.home, &j.goal, &leave) {
        return fail(e);
    }
    println!("new sessions in {dir} join no hive goal ({} keeps the sessions that joined)", j.goal);
    0
}

fn run_cmd(here: &Here, a: &Args, id: &str) -> i32 {
    let Some(drones) = a.value("--drones").and_then(|n| n.parse::<usize>().ok()) else {
        eprintln!("borg hive run: give --drones N (1 to {})", run::MAX_DRONES);
        return 2;
    };
    let harness = match a.value("--harness") {
        Some(n) => match crate::harness::by_name(n) {
            Some(h) => h,
            None => {
                eprintln!("borg hive run: unknown harness {n} (one of {})", crate::harness::names(", "));
                return 2;
            }
        },
        None => crate::harness::current(),
    };
    let iter_timeout = match a.value("--iter-timeout").map(str::parse::<u64>) {
        None => run::DEFAULT_ITER_TIMEOUT,
        Some(Ok(s)) if s > 0 => Duration::from_secs(s),
        Some(_) => {
            eprintln!("borg hive run: --iter-timeout is seconds, above 0");
            return 2;
        }
    };
    let o =
        run::Opts { goal: id, drones, harness, model: a.value("--model"), base: a.value("--base").unwrap_or("HEAD"), iter_timeout, dry_run: a.on("--dry-run") };
    run::main(&here.home, &here.cwd, &o)
}

fn open(here: &Here, a: &Args, goal_id: &str) -> i32 {
    let g = match goal::load(&here.home, goal_id) {
        Ok(g) => g,
        Err(e) => return fail(e),
    };
    let Some(id) = a.value("--id") else {
        eprintln!("borg hive open: give --id ID");
        return 2;
    };
    let task = if a.on("--again") {
        match here.board(&g).tasks.get(id) {
            Some(s) if matches!(s.status, Status::Blocked { .. }) => s.task.clone(),
            Some(s) => return fail(format!("{id} is {}, not blocked: nothing to reopen", s.status.name())),
            None => return fail(format!("no task {id} in goal {goal_id}")),
        }
    } else {
        let Some(role) = a.value("--role").map_or(Some(Role::Work), Role::parse) else {
            eprintln!("borg hive open: --role is plan, work, verify or integrate");
            return 2;
        };
        let accept: Result<Vec<Check>, String> = a.all("--accept").into_iter().map(|c| Check::parse(c, 600)).collect();
        let made = accept.and_then(|acc| {
            new_task(id, role, a.value("--title").unwrap_or_default(), a.value("--repo").unwrap_or_default(), a.list("--files"), a.list("--deps"), acc)
        });
        let b = here.board(&g);
        match made.and_then(|t| check_deps(std::slice::from_ref(&t), |d| b.tasks.contains_key(d)).map(|()| t)) {
            Ok(t) if b.tasks.contains_key(&t.id) => return fail(format!("task {} exists already (reopen a blocked one with --again)", t.id)),
            Ok(t) => t,
            Err(e) => return fail(e),
        }
    };
    if let Err(e) = board::append(&tasks_path(&here.home, goal_id), &[Event::open(task.clone(), &here.engineer(), here.now)]) {
        return fail(e);
    }
    println!("opened {} ({}) {} in goal {goal_id}", task.id, task.role.name(), task.title);
    0
}

fn claim_cmd(here: &Here, named: Option<&str>, task: Option<&str>) -> i32 {
    let (session, hs) = match here.drone(named) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    let g = match goal::load(&here.home, &hs.goal) {
        Ok(g) => g,
        Err(e) => return fail(e),
    };
    let repo = queen::repo_name(&here.cwd);
    for _ in 0..if task.is_some() { 1 } else { CLAIM_TRIES } {
        let now = learn::now_ms();
        let b = here.board(&g);
        if let Some(s) = board::holding(&b, &session, now) {
            return fail(format!("{} holds {} already: finish, release or block it first", short(&session), s.task.id));
        }
        let pick = match task {
            Some(t) => match b.tasks.get(t) {
                None => return fail(format!("no task {t} in goal {}", g.id)),
                Some(s) if s.task.role == Role::Integrate => return fail(format!("{t}: {INTEGRATE_REFUSED}")),
                Some(s) if s.task.from.as_deref().is_some_and(|f| !runner_worktree(&g, &session, &here.cwd) && !checkout_of(&here.cwd, f)) => {
                    return fail(fix_where(t, s.task.from.as_deref().unwrap_or_default()));
                }
                Some(s) if !may_claim(&g, &s.task, &repo) => {
                    return fail(format!("{t} belongs to repo {}, and {repo} is outside the goal's repos", s.task.repo));
                }
                Some(s) if !board::claimable_by(&b, s, &session, now) => {
                    return fail(format!("{t} is not open to {} ({})", short(&session), s.shown_status(now)));
                }
                Some(s) => s,
            },
            None => match board::next_for(&b, &session, &repo, now, |t| may_take(&g, t, &repo, &session, &here.cwd)) {
                Some(s) => s,
                None => {
                    println!("no task is open to {} now", short(&session));
                    return 0;
                }
            },
        };
        let id = pick.task.id.clone();
        match claim(&here.home, &g, &session, &pick.task.clone(), &here.cwd, now, verdict::SNAPSHOT_TIMEOUT) {
            Ok(won) => {
                count(&here.home, stats::HIVE_CLAIM, &session, &g.id, &id, &repo);
                hook::append(&session, &format!("{TASK_MARKER}{id}@{}", won.live(learn::now_ms()).map_or(0, |l| l.2)));
                println!("claimed {id} for {}. {}", short(&session), describe(&g, &won, &session, &here.cwd));
                return 0;
            }
            Err(Refused::Deps(why)) => return fail(why),
            Err(Refused::Lost) => println!("lost {id}: another drone claimed it first"),
        }
    }
    3
}

/// The live claim of the session `named`: (session, goal, task).
fn held(here: &Here, named: Option<&str>) -> Result<(String, Goal, TaskState), String> {
    let (session, hs) = here.drone(named)?;
    let g = goal::load(&here.home, &hs.goal)?;
    let b = here.board(&g);
    let s = board::holding(&b, &session, here.now).cloned().ok_or_else(|| format!("{} holds no claim in goal {}", short(&session), g.id))?;
    Ok((session, g, s))
}

fn held_event(here: &Here, named: Option<&str>, ev: Ev, note: &str) -> i32 {
    match held(here, named) {
        Ok((session, g, s)) => match board::append(&tasks_path(&here.home, &g.id), &[Event::new(ev, &s.task.id, &session, here.now).with_note(note.trim())]) {
            Ok(()) => {
                if ev == Ev::Released {
                    count(&here.home, stats::HIVE_RELEASED, &session, &g.id, &s.task.id, &queen::repo_name(&here.cwd));
                }
                println!("{} {}", ev.name(), s.task.id);
                0
            }
            Err(e) => fail(e),
        },
        Err(e) => fail(e),
    }
}

fn done_cmd(here: &Here, named: Option<&str>, note: &str) -> i32 {
    let (session, g, s) = match held(here, named) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    if s.task.role == Role::Verify {
        return fail(format!("{} is a verify task: give a verdict with `borg hive verify --pass` or `--reject \"why\"`", s.task.id));
    }
    // No hook limit here: each check runs within its own timeout.
    match oracle::run(&g, &s.task, Path::new(&here.cwd), None) {
        oracle::Verdict::Pass => {
            let since = s.live(learn::now_ms()).map_or(i64::MIN, |l| l.2);
            let (done, check) = done_events(&here.home, &g, &s, &session, Path::new(&here.cwd), since, verdict::SNAPSHOT_TIMEOUT);
            let vid = check.id.clone();
            let ms = done.ms;
            let events = [done.with_note(&hook::one_line(note, 200)), Event::open(check, &session, ms)];
            if let Err(e) = board::append(&tasks_path(&here.home, &g.id), &events) {
                return fail(e);
            }
            count(&here.home, stats::HIVE_DONE, &session, &g.id, &s.task.id, &queen::repo_name(&here.cwd));
            if let Some(mut hs) = bound(&session) {
                hs.oracle_blocks = 0;
                hs.blocks_for.clear();
                save(&session, &hs);
            }
            println!("{} done: the oracle passed; verify task {vid} opened", s.task.id);
            0
        }
        v => {
            let log = oracle::log(&here.home, &g.id, &s.task.id, &v, learn::now_ms());
            match v {
                oracle::Verdict::Fail { check, tail } => println!("{} is not done: {check} failed\n{}", s.task.id, tail.trim_end()),
                oracle::Verdict::Timeout { check, after } => println!("{} is not done: {check} timed out after {}s", s.task.id, after.as_secs()),
                oracle::Verdict::Pass => {}
            }
            if let Some(l) = log {
                println!("(log: {})", l.display());
            }
            1
        }
    }
}

/// `borg hive verify --pass|--reject "why"`: the verdict of the session's verify claim (verdict.rs
/// `record`: the guardrails, then the reward).
fn verdict_cmd(here: &Here, named: Option<&str>, said: verdict::Said) -> i32 {
    let (session, g, s) = match held(here, named) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    if s.task.role != Role::Verify {
        return fail(format!("{} is not a verify task", s.task.id));
    }
    match verdict::record(&here.home, &g, &s.task.id, &said, &session, &here.cwd, here.now) {
        Ok(r) => {
            println!("{}", r.summary(&s.task.id));
            0
        }
        Err(e) => fail(e),
    }
}

/// Whether this command runs for a drone: launched into a goal (`BORG_HIVE`, `BORG_HIVE_DRONE`), or in
/// a worktree `borg hive run` made and has not left (a harness that drops the env).
fn drone_here(here: &Here) -> bool {
    let set = |k: &str| std::env::var(k).is_ok_and(|v| !v.trim().is_empty());
    set(ENV) || set(DRONE_ENV) || joined(&here.home, &here.cwd).is_some_and(|j| j.by == "run")
}

/// `borg hive score <goal>`: the hidden oracle scores every judged attempt not scored yet. An engineer's
/// command: a drone that learned the held-out checks' results could optimise against them.
fn score_cmd(here: &Here, id: &str) -> i32 {
    if drone_here(here) {
        return fail("the hidden oracle is scored by the engineer, never by a drone of a hive goal");
    }
    let g = match goal::load(&here.home, id) {
        Ok(g) => g,
        Err(e) => return fail(e),
    };
    if g.hidden.is_none() {
        return fail(format!("goal {id} has no hidden oracle: its verdicts are credited at once"));
    }
    let Some(repo) = run::repo_of(Path::new(&here.cwd)) else { return fail(format!("{} is not in the goal's git repo", here.cwd)) };
    let scored = hidden::score_pending(&here.home, &g, &repo, usize::MAX, here.now);
    let scores = &scored.scores;
    for s in scores {
        let r = if s.pass {
            "pass"
        } else if s.timeout {
            "timeout"
        } else {
            "fail"
        };
        println!("{}  {r}  {} ms{}", s.task, s.dur_ms, if s.agree { "" } else { "  (the verifier said otherwise)" });
    }
    for why in &scored.skipped {
        println!("{why} (not scored: run it again from the goal's repo)");
    }
    println!("{} attempt(s) scored", scores.len());
    i32::from(!scored.skipped.is_empty())
}

/// Where the fix task `t` (from the combined tree `from`) is claimed.
fn fix_where(t: &str, from: &str) -> String {
    format!(
        "{t} starts from the integrated tree {from}: claim it in a checkout of it (`git worktree add <dir> {from}`, then `borg hive claim {t}` there), or a drone of `borg hive run` takes it in its own worktree"
    )
}

/// `borg hive integrate <goal>`: what `borg hive run` does once every task is verified (integrate.rs),
/// from the goal's repo. Exit 0 when the combined tree passed, 1 when it failed (a fix task is open) or
/// is blocked, 2 when no integration is due. The engineer's: a drone's session never integrates.
fn integrate_cmd(here: &Here, id: &str) -> i32 {
    if drone_here(here) {
        return fail(INTEGRATE_REFUSED);
    }
    let g = match goal::load(&here.home, id) {
        Ok(g) => g,
        Err(e) => return fail(e),
    };
    let Some(repo) = run::repo_of(Path::new(&here.cwd)) else { return fail(format!("{} is not in the goal's git repo", here.cwd)) };
    match integrate::run(&here.home, &g, &repo, "cli", here.now) {
        Some(integrate::Outcome { task, head, verdict: Ok(()) }) => {
            println!("{task}: integrated {}; the goal is finished", head.unwrap_or_default());
            0
        }
        Some(integrate::Outcome { task, head, verdict: Err(why) }) => {
            println!("{task}: {}{why}", head.as_ref().map_or_else(String::new, |h| format!("{h}: ")));
            if let Some(h) = head {
                println!("{}", fix_where(&format!("{task}.fix"), &h));
            }
            1
        }
        None => {
            let b = here.board(&g);
            match b.tasks.values().find(|s| s.task.from.is_some() && !matches!(s.status, Status::Verified { .. })) {
                Some(s) => println!("no integration of {id} is due: {}", fix_where(&s.task.id, s.task.from.as_deref().unwrap_or_default())),
                None => println!("no integration of {id} is due: a task is not verified yet, or the goal is finished"),
            }
            2
        }
    }
}

fn contract_cmd(here: &Here, named: Option<&str>, text: &str, scope: Vec<String>, supersedes: Option<&str>) -> i32 {
    let (session, hs) = match here.drone(named) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    if scope.is_empty() {
        eprintln!("borg hive contract: give its --scope (repo:NAME, id:NAME or a file glob), comma-separated");
        return 2;
    }
    let g = match goal::load(&here.home, &hs.goal) {
        Ok(g) => g,
        Err(e) => return fail(e),
    };
    let b = here.board(&g);
    let task = board::holding(&b, &session, here.now).map(|s| s.task.id.clone()).unwrap_or_default();
    let c = Contract::new(&task, &session, here.now, scope, text.trim(), supersedes.map(String::from));
    match contract::publish(&here.home, &g.id, &c, &queen::repo_name(&here.cwd)) {
        Ok(contract::Publish::Written) => {
            println!("contract {} published in goal {}", c.id, g.id);
            0
        }
        Ok(contract::Publish::Proposed(q)) => {
            println!(
                "{} is another drone's contract: proposed to the engineer as queen question {q}; keep to it until they answer",
                supersedes.unwrap_or_default()
            );
            0
        }
        Err(e) => fail(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_line_parses_and_ids_stay_path_safe() {
        let t = parse_task("t1|work|Add the parser|src/parse/*.rs,tests/*.rs|t0").unwrap();
        assert_eq!((t.id.as_str(), t.role, t.files.len(), t.deps.as_slice()), ("t1", Role::Work, 2, ["t0".to_string()].as_slice()));
        assert!(parse_task("t1|chef|x").is_err());
        assert!(parse_task("../t|work|x").is_err());
        assert!(parse_task("t1|work").is_err());
    }

    #[test]
    fn init_and_open_refuse_unknown_deps_and_cycles() {
        let t = |line: &str| parse_task(line).unwrap();
        let none = |_: &str| false;
        // init: deps within the batch, in any order.
        assert_eq!(check_deps(&[t("b|work|B||a"), t("a|work|A")], none), Ok(()));
        assert_eq!(check_deps(&[t("a|work|A"), t("b|work|B||a,x")], none).unwrap_err(), "task b: its dep x names no task");
        assert_eq!(
            check_deps(&[t("a|work|A||c"), t("b|work|B||a"), t("c|work|C||b"), t("d|work|D")], none).unwrap_err(),
            "tasks a, b, c depend on each other in a cycle"
        );
        assert!(check_deps(&[t("a|work|A||a")], none).is_err(), "a task after itself");
        // open: a dep must be on the board already (so no new cycle can form).
        assert_eq!(check_deps(&[t("c|work|C||a")], |d| d == "a"), Ok(()));
        assert!(check_deps(&[t("c|work|C||z")], |d| d == "a").unwrap_err().contains("its dep z names no task"));
    }

    #[test]
    fn a_join_record_holds_the_directories_inside_it_and_a_leave_shadows_it() {
        let home = std::env::temp_dir().join(format!("borg-hive-joins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        assert!(joined(&home, "/w/repo/src").is_none(), "no hive: nothing (one failed stat)");
        let g = Goal {
            v: 1,
            id: "g".into(),
            spec: "s".into(),
            oracle: vec![],
            budget_usd: 1.0,
            repos: vec![],
            author: String::new(),
            created_ms: 0,
            sig: String::new(),
            hidden: None,
        };
        goal::create(&home, &g).unwrap();
        let j =
            |ms: i64, goal: &str, dir: &str| Join { v: 1, ms, goal: goal.into(), dir: dir.into(), drone: "j-1".into(), by: "join".into(), ..Join::default() };
        append_join(&home, "g", &j(1, "g", "/w/repo")).unwrap();
        assert_eq!(joined(&home, "/w/repo/src/x").map(|j| (j.goal, j.drone)), Some(("g".into(), "j-1".into())));
        assert_eq!(joined(&home, "/w/repo").map(|j| j.goal).as_deref(), Some("g"));
        assert!(joined(&home, "/w/repo2").is_none(), "whole path components only");
        assert!(joined(&home, "/w").is_none() && joined(&home, "").is_none());
        append_join(&home, "g", &j(2, "", "/w/repo")).unwrap();
        assert!(joined(&home, "/w/repo/src").is_none(), "the later leave shadows the join");
        append_join(&home, "g", &j(3, "g", "/w/repo/.worktrees/hive-g-1")).unwrap();
        assert!(joined(&home, "/w/repo/.worktrees/hive-g-1/src").is_some() && joined(&home, "/w/repo/src").is_none());
        let _ = std::fs::remove_dir_all(home);
    }

    fn git_ok(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.email=e@x", "-c", "user.name=e"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn goal_g() -> Goal {
        Goal {
            v: 1,
            id: "g".into(),
            spec: "s".into(),
            oracle: vec![],
            budget_usd: 1.0,
            repos: vec![],
            author: String::new(),
            created_ms: 0,
            sig: String::new(),
            hidden: None,
        }
    }

    #[test]
    fn a_model_session_is_never_offered_an_integrate_task() {
        let g = goal_g();
        let it = Task { role: Role::Integrate, ..new_task("integrate.1", Role::Work, "integrate", "", vec![], vec![], vec![]).unwrap() };
        assert!(!may_claim(&g, &it, "app"), "whatever the repo");
        let b = board::fold(&[Event::open(it, "integrate-d1-1", 1)]);
        assert_eq!(offer(&g, &b, "s", "app", "/w", 2), "No claim, and no task is open to you now (`borg hive board`).");
        let w = new_task("t1", Role::Work, "work", "", vec![], vec![], vec![]).unwrap();
        assert!(may_claim(&g, &w, "app"));
    }

    #[test]
    fn a_fix_task_claim_resets_a_runner_worktree_or_takes_a_checkout_of_its_tree() {
        let root = std::env::temp_dir().join(format!("borg-hive-fix-claim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (repo, home) = (root.join("app"), root.join("store"));
        std::fs::create_dir_all(&repo).unwrap();
        git_ok(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README"), "base\n").unwrap();
        git_ok(&repo, &["add", "-A"]);
        git_ok(&repo, &["commit", "-qm", "base"]);
        // The combined tree a failed integration left: another commit.
        git_ok(&repo, &["checkout", "-q", "-b", "m"]);
        std::fs::write(repo.join("m.txt"), "merged\n").unwrap();
        git_ok(&repo, &["add", "-A"]);
        git_ok(&repo, &["commit", "-qm", "m"]);
        let m = git_ok(&repo, &["rev-parse", "HEAD"]);
        git_ok(&repo, &["checkout", "-q", "main"]);
        let wt = run::worktree(&repo, "g", 1);
        git_ok(&repo, &["worktree", "add", "-q", "-b", "hive/g/1", wt.to_str().unwrap()]);
        let g = goal_g();
        goal::create(&home, &g).unwrap();
        let fix = Task { from: Some(m.clone()), ..new_task("integrate.1.fix", Role::Work, "fix the integrated tree", "", vec![], vec![], vec![]).unwrap() };
        board::append(&tasks_path(&home, "g"), &[Event::open(fix.clone(), "integrate-d2-1", 1)]).unwrap();
        let pid = std::process::id();
        let (runner_s, own_s) = (format!("fix-runner-{pid}"), format!("fix-own-{pid}"));
        bind_as(&home, &runner_s, "g", "d1").unwrap();
        bind_as(&home, &own_s, "g", "").unwrap();
        let (w, r) = (wt.display().to_string(), repo.display().to_string());
        // An engineer's checkout, or a session of its own: refused, nothing written.
        assert_eq!(claim(&home, &g, &runner_s, &fix, &r, 2, verdict::SNAPSHOT_TIMEOUT), Err(Refused::Lost));
        assert_eq!(claim(&home, &g, &own_s, &fix, &w, 2, verdict::SNAPSHOT_TIMEOUT), Err(Refused::Lost));
        assert_eq!(board::read(&tasks_path(&home, "g")).len(), 1);
        let b = board::fold(&board::read(&tasks_path(&home, "g")));
        assert!(offer(&g, &b, &own_s, "app", &w, 2).starts_with("No claim, and no task"), "not offered where it cannot be claimed");
        assert!(offer(&g, &b, &runner_s, "app", &w, 2).contains("integrate.1.fix"));
        // The runner drone in its own worktree: claimed from `from`, the worktree reset to it.
        std::fs::write(wt.join("README"), "stray edit\n").unwrap();
        std::fs::write(wt.join("stray.txt"), "x\n").unwrap();
        let now = learn::now_ms();
        let won = claim(&home, &g, &runner_s, &fix, &w, now, verdict::SNAPSHOT_TIMEOUT).expect("claimed");
        assert!(won.live(now + 1).is_some_and(|(by, _, _)| by == runner_s));
        assert_eq!(git_ok(&wt, &["rev-parse", "HEAD"]), m);
        assert_eq!(std::fs::read_to_string(wt.join("README")).unwrap(), "base\n");
        assert!(wt.join("m.txt").exists() && !wt.join("stray.txt").exists());
        let c = board::read(&tasks_path(&home, "g")).into_iter().find(|e| e.ev == Ev::Claim).unwrap();
        assert_eq!(c.base, m, "the diff is judged from the combined tree");
        let kept = c.note.strip_prefix("worktree before the reset: ").expect("the discarded tree is kept");
        assert_eq!(git_ok(&repo, &["show", &format!("{kept}:stray.txt")]), "x");
        assert!(describe(&g, &won, &runner_s, &w).contains(&format!("reset to the integrated tree {m}")));
        // Any session in a checkout of `from` (a goal worked by joined sessions): offered and claimed
        // there, never reset, its base a snapshot of that checkout.
        board::append(&tasks_path(&home, "g"), &[Event::new(Ev::Released, "integrate.1.fix", &runner_s, learn::now_ms())]).unwrap();
        let co = root.join("fix");
        git_ok(&repo, &["worktree", "add", "-q", "--detach", co.to_str().unwrap(), &m]);
        std::fs::write(co.join("notes.txt"), "mine\n").unwrap();
        let c = co.display().to_string();
        let now = learn::now_ms();
        let b = board::fold(&board::read(&tasks_path(&home, "g")));
        assert!(offer(&g, &b, &own_s, "app", &c, now).contains("integrate.1.fix"));
        let won = claim(&home, &g, &own_s, &fix, &c, now, verdict::SNAPSHOT_TIMEOUT).expect("claimed in a checkout of from");
        assert!(co.join("notes.txt").exists(), "no reset");
        let c = board::read(&tasks_path(&home, "g")).into_iter().rfind(|e| e.ev == Ev::Claim).unwrap();
        assert_eq!(git_ok(&repo, &["rev-parse", &format!("{}^", c.base)]), m, "a snapshot on the combined tree");
        assert!(describe(&g, &won, &own_s, &co.display().to_string()).contains(&format!("You work on the integrated tree {m}")));
        unbind(&runner_s);
        unbind(&own_s);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_lost_claim_takes_its_deps_out_even_when_its_time_is_up() {
        let root = std::env::temp_dir().join(format!("borg-hive-take-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        git_ok(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        git_ok(&root, &["add", "-A"]);
        git_ok(&root, &["commit", "-qm", "base"]);
        let base = verdict::snapshot(&root, verdict::SNAPSHOT_TIMEOUT).unwrap();
        std::fs::write(root.join("a.txt"), "dep\n").unwrap();
        let head = verdict::snapshot(&root, verdict::SNAPSHOT_TIMEOUT).unwrap();
        git_ok(&root, &["checkout", "-q", "--", "a.txt"]);
        assert!(verdict::bring_in(&root, &base, &head, verdict::SNAPSHOT_TIMEOUT).unwrap());
        let t0 = Instant::now();
        take_out_all(&root.display().to_string(), &[(base, head)], Duration::ZERO);
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "a\n", "the undo gets its floor");
        assert!(t0.elapsed() < MIN_ORACLE + Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn context_goes_first_and_waits_when_the_harness_takes_none() {
        let theirs = Some(json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": "lessons"}}));
        let out = with_context("s", "UserPromptSubmit", "hive".into(), true, theirs).unwrap();
        assert_eq!(out["hookSpecificOutput"]["additionalContext"], "hive\n\nlessons");
        let out = with_context("s", "PostToolUse", "hive".into(), true, None).unwrap();
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        assert_eq!(with_context("s", "PostToolUse", String::new(), true, None), None);
        let blocked = with_reason("hive", Some(json!({"decision": "block", "reason": "q"})), false).unwrap();
        assert_eq!(blocked["reason"], "hive\n\nq");
        assert_eq!(with_reason("hive", None, false), None, "never a block of its own on a second round");
        assert_eq!(with_reason("hive", None, true).unwrap()["reason"], "hive");
    }
}
