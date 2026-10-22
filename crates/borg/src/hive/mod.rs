//! The hive: many drones, one goal (docs/design/collective.md). H1, the local spike: file-backed under
//! `<store>/hive/<goal>/`, no mesh sync yet (that waits for the pilot's result), in line formats that
//! can sync later.
//!
//! - `goal.json` (goal.rs): the spec, the oracle checks that gate `done`, the budget, the repos.
//! - `tasks.jsonl` (board.rs): the board's events; `board::fold` reads them the same on every node.
//!   Claims are leases renewed by the drone's hooks; the lowest (ms, blake3 key) claim wins a race.
//! - `contracts.jsonl` (contract.rs): what drones promise each other, shown once per session.
//! - `oracle/` (oracle.rs): the tails of failed oracle runs.
//!
//! A session joins a goal by `borg hive join` or by `BORG_HIVE=<goal>` at its first hook; the
//! membership is `<BORG_STATE>/<session>.hive` (`HiveSession`). The hook's [`around`] then adds the
//! drone's claim, the board's changes and the contracts in scope at a prompt, contracts and scope
//! warnings after an edit, and at the stop runs the oracle: a failing one blocks the stop (at most
//! `MAX_ORACLE_BLOCKS` times per claim), a passing one publishes `done`, opens its verify task and hands
//! the drone the next open task. A session in no goal costs one failed file open and sees no change.

pub mod board;
pub mod contract;
pub mod goal;
pub mod oracle;

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
const CLAIM_SETTLE: Duration = Duration::from_millis(150);
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
const INOCULATION: &str = "The oracle gates done: your task counts as done only when its checks pass, and a verifier drone checks the work after you. \
Gaming the oracle (editing or skipping its tests, special-casing its inputs) is visible on the board and useless to the task.";

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
    let cursor = std::fs::metadata(tasks_path(home, goal)).map_or(0, |m| m.len());
    let hs = HiveSession { goal: goal.into(), cursor, ..HiveSession::default() };
    steer::rewrite(&session_path(session), &serde_json::to_string(&hs).unwrap_or_default())
}

pub fn unbind(session: &str) {
    let _ = std::fs::remove_file(session_path(session));
}

/// The hook's join: a session launched with `BORG_HIVE=<goal>` joins that goal of the routed store at
/// its first hook, if the goal exists (anything else is ignored).
pub fn bind_from_env(home: &Path, session: &str) {
    let Ok(goal) = std::env::var(ENV) else { return };
    let goal = goal.trim();
    if goal::valid_id(goal) && goal_path(home, goal).is_file() && !session_path(session).exists() {
        let _ = bind(home, session, goal);
    }
}

/// Whether a drone in `repo` may claim `t`: a drone outside the goal's repos only claims tasks of its
/// own repo (or of none).
pub fn may_claim(g: &Goal, t: &Task, repo: &str) -> bool {
    goal::in_scope(g, repo) || t.repo.is_empty() || t.repo.eq_ignore_ascii_case(repo)
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
    if !t.deps.is_empty() {
        out.push_str(&format!("; after {}", t.deps.join(", ")));
    }
    let checks: Vec<&str> = oracle::checks(g, t).iter().map(|c| c.name.as_str()).collect();
    match t.role {
        Role::Verify => out.push_str(&format!(
            ". Verify {} independently (its checks: {}), then give your verdict: `{}` or `{} \"why\"`.",
            t.of.as_deref().unwrap_or("its work"),
            checks.join(", "),
            command("verify --pass", session, cwd),
            command("verify --reject", session, cwd)
        )),
        _ => out.push_str(&format!(
            "; done when: {} pass. Ending your turn runs them; a suite slower than the stop hook runs with `{}`. Stuck: `{} \"why\"`.",
            checks.join(", "),
            command("done", session, cwd),
            command("block", session, cwd)
        )),
    }
    if !s.notes.is_empty() {
        out.push_str(&format!(" Rework {} — the verifier's notes: {}", s.rework, s.notes.join(" | ")));
    }
    out
}

/// What a drone without a claim is told: the next open task and the exact claim command.
fn offer(g: &Goal, b: &Board, session: &str, repo: &str, cwd: &str, now: i64) -> String {
    match board::next_for(b, session, repo, now, |t| may_claim(g, t, repo)) {
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
    let ctx = Ctx { h, home, g: &g, session, cwd, repo: &repo, now };
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
}

impl Ctx<'_> {
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
            parts.push(INOCULATION.into());
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
        // A verify task ends with a verdict, not with an oracle run of its own.
        let verdict = if task.role == Role::Verify {
            oracle::Verdict::Fail {
                check: "verdict".into(),
                tail: format!(
                    "no verdict yet: run `{}` or `{} \"why\"`",
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
        } else if let Some(next) = self.done(held, input, &path) {
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
    /// open task claimed for this drone. The text to block with when it got one.
    fn done(&self, held: &TaskState, input: &Value, path: &Path) -> Option<String> {
        let task = &held.task;
        let message = crate::harness::plan(self.h.caps(), input);
        let check = verify_task(task, held.rework, self.session);
        let vid = check.id.clone();
        let wrote = board::append(
            path,
            &[Event::new(Ev::Done, &task.id, self.session, self.now).with_note(&hook::one_line(&message, 200)), Event::open(check, self.session, self.now)],
        );
        wrote.ok()?;
        count(self.home, stats::HIVE_DONE, self.session, &self.g.id, &task.id, self.repo);
        let b = board::fold(&board::read(path));
        let next = board::next_for(&b, self.session, self.repo, self.now, |t| may_claim(self.g, t, self.repo))?;
        let won = claim(self.home, self.g, self.session, &next.task.id, self.now)?;
        count(self.home, stats::HIVE_CLAIM, self.session, &self.g.id, &won.task.id, self.repo);
        hook::append(self.session, &format!("{TASK_MARKER}{}@{}", won.task.id, won.live(self.now).map_or(0, |l| l.2)));
        Some(format!("[borg hive] {} done, verify task {vid} opened. Next: {}\n\n", task.id, describe(self.g, &won, self.session, self.cwd)))
    }
}

/// The verify task a `done` of `t` opens: the same scope and checks, never claimable by its author.
pub fn verify_task(t: &Task, rework: u32, author: &str) -> Task {
    Task {
        id: format!("{}.v{}", t.id, rework + 1),
        role: Role::Verify,
        title: format!("verify {}: {}", t.id, t.title),
        repo: t.repo.clone(),
        files: t.files.clone(),
        deps: Vec::new(),
        accept: t.accept.clone(),
        of: Some(t.id.clone()),
        not_by: vec![author.into()],
    }
}

/// Claims `task` for `session` (a claim and a heartbeat in one write), waits for rivals' lines to land,
/// and reads the board back: the task when the claim won.
fn claim(home: &Path, g: &Goal, session: &str, task: &str, now: i64) -> Option<TaskState> {
    let path = tasks_path(home, &g.id);
    board::append(&path, &[Event::new(Ev::Claim, task, session, now), Event::new(Ev::Heartbeat, task, session, now)]).ok()?;
    std::thread::sleep(CLAIM_SETTLE);
    let b = board::fold(&board::read(&path));
    let s = b.tasks.get(task)?;
    let won = s.live(now).is_some_and(|(by, _, _)| by == session);
    if won && let Some(mut hs) = bound(session) {
        hs.beat_ms = now;
        save(session, &hs);
    }
    won.then(|| s.clone())
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
];

/// The flags that take no value.
const SWITCHES: &[&str] = &["--pass", "--again", "--json", "--watch"];

const USAGE: &str = "usage: borg hive <command> [--dir DIR] [--session ID|DRONE]
  init <goal> --spec TEXT|--spec-file F --oracle NAME=CMD... --budget USD [--timeout S] [--repo NAME[@NODE]]...
       [--task \"ID|role|title|files,..|deps,..\"]...       a goal, its oracle and its first tasks
  board [<goal>] [--json] [--watch]                         the board: tasks by status, holders, rework
  join <goal> [--session ID|DRONE]                          a running drone joins the goal
  leave                                                     it leaves (its claim is released)
  open <goal> --id ID --role work|plan|verify|integrate --title T [--repo R] [--files a,b] [--deps x,y]
       [--accept NAME=CMD]... [--again]                     an engineer opens a task (--again reopens a blocked one)
  drone commands (the goal is the session's):
  claim [TASK] | progress \"note\" | release | block \"why\" | done [--note N]
  verify --pass | --reject \"why\" | contract \"text\" --scope a,b [--supersedes ID]";

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
    Ok(Task { id: id.into(), role, title: title.trim().into(), repo: repo.into(), files, deps, accept, of: None, not_by: Vec::new() })
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
        ("join", [id]) => join(&here, id, session),
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
        ("verify", []) => match (a.on("--pass"), a.value("--reject")) {
            (true, None) => verdict(&here, session, Ev::Verified, ""),
            (false, Some(note)) if !note.trim().is_empty() => verdict(&here, session, Ev::Rejected, note),
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
    let tasks = match tasks {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let author = learn::engineer(&here.cwd);
    let g = Goal { v: 1, id: id.into(), spec, oracle, budget_usd, repos, author: author.clone(), created_ms: here.now, sig: String::new() };
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
        match made {
            Ok(t) if here.board(&g).tasks.contains_key(&t.id) => return fail(format!("task {} exists already (reopen a blocked one with --again)", t.id)),
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
                Some(s) if !may_claim(&g, &s.task, &repo) => {
                    return fail(format!("{t} belongs to repo {}, and {repo} is outside the goal's repos", s.task.repo));
                }
                Some(s) if !board::claimable_by(&b, s, &session, now) => {
                    return fail(format!("{t} is not open to {} ({})", short(&session), s.shown_status(now)));
                }
                Some(s) => s,
            },
            None => match board::next_for(&b, &session, &repo, now, |t| may_claim(&g, t, &repo)) {
                Some(s) => s,
                None => {
                    println!("no task is open to {} now", short(&session));
                    return 0;
                }
            },
        };
        let id = pick.task.id.clone();
        if let Some(won) = claim(&here.home, &g, &session, &id, now) {
            count(&here.home, stats::HIVE_CLAIM, &session, &g.id, &id, &repo);
            hook::append(&session, &format!("{TASK_MARKER}{id}@{}", won.live(now).map_or(0, |l| l.2)));
            println!("claimed {id} for {}. {}", short(&session), describe(&g, &won, &session, &here.cwd));
            return 0;
        }
        println!("lost {id}: another drone claimed it first");
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
            let now = learn::now_ms();
            let check = verify_task(&s.task, s.rework, &session);
            let vid = check.id.clone();
            let events = [Event::new(Ev::Done, &s.task.id, &session, now).with_note(&hook::one_line(note, 200)), Event::open(check, &session, now)];
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

fn verdict(here: &Here, named: Option<&str>, ev: Ev, note: &str) -> i32 {
    let (session, g, s) = match held(here, named) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    if s.task.role != Role::Verify {
        return fail(format!("{} is not a verify task", s.task.id));
    }
    if let Err(e) = board::append(&tasks_path(&here.home, &g.id), &[Event::new(ev, &s.task.id, &session, here.now).with_note(note.trim())]) {
        return fail(e);
    }
    let of = s.task.of.as_deref().unwrap_or_default();
    match ev {
        Ev::Verified => println!("{} verified: {of} is verified", s.task.id),
        _ => println!("{} rejected: {of} is open again with your note", s.task.id),
    }
    0
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
