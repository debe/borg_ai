//! A task's scope (docs/design/hive.md "Scope asks and amends"): what a definition must be to be
//! written, and how a drone asks for a wider one.
//!
//! - **Plan-time checks** ([`validate`]), on `init`, `open` and `open --again`: a `files` entry that
//!   names a directory is its tree (`dir/**`, a bare directory glob matches only itself); an entry
//!   that is a file the goal's check reads is refused (the verifier protects it before any scope,
//!   verdict.rs `protected`, so no task could ever pass with a change to it); each check is run once
//!   on the tree as it stands, bounded at `PROBE`, and one that cannot pass on any tree is refused
//!   (oracle.rs `probe`). A check still running at the bound is said, never refused.
//! - **The ask** ([`ask_cmd`], `borg hive scope`): the holder of a work task proposes files or checks
//!   and why. It is a message to the planner (message.rs `PLANNER`) carrying the exact `borg hive open
//!   --again` line the engineer runs; nothing on the board changes and nothing is approved (S-def:
//!   only an engineer's open amends a task, board.rs `apply`). The asking session is marked
//!   (`ASKED_MARKER`, with its definition's print, the ask's id and the files asked for). While the ask
//!   waits (the definition is the one it asked from, and the engineer did not reply to the ask), a stop
//!   whose one failure is the done's precheck, on paths the asked files would all admit, ends the turn
//!   without counting it; any other stop runs as without an ask, so a drone that drops the asked change
//!   finishes. The waiter wakes the drone idle on it once the task is amended or the ask answered
//!   (`borg hive tell --re` from a terminal, the engineer's no), with no timer; from the
//!   answer on, the precheck blocks its stop as a failing check does, so it finishes in scope or blocks.

use std::path::Path;
use std::time::Duration;

use super::{Args, Check, Goal, Here, Role, Task, TaskState, fail, held, message, node, queen};
use crate::hook;

/// How long a check's probe runs before it is left unsaid (`BORG_HIVE_PROBE_MS` in tests only).
const PROBE: Duration = Duration::from_secs(5);
/// The most files a scope ask adds: a wider ask is a new plan, the engineer's to make.
pub const MAX_NEW: usize = 8;
/// The `.ctx` marker of a session's scope ask: `<task>@<since> <print> <id> <files>`, the definition it
/// held when it asked, the ask's message id and the files it asked for (a JSON list); `<task>@<since>
/// -` once the waiter told it the amend.
const ASKED_MARKER: &str = "#borg-hive-asked ";

fn bound() -> Duration {
    match std::env::var(crate::harness::TEST_ENV).is_ok() {
        true => std::env::var("BORG_HIVE_PROBE_MS").ok().and_then(|v| v.parse().ok()).map_or(PROBE, Duration::from_millis),
        false => PROBE,
    }
}

/// `files` as a task holds them in the repo at `root`: a directory, or an entry ending in `/`, is its
/// tree (`dir/**`). Refused: an entry that is a file the goal's check reads, told to the engineer who
/// writes the plan, or to the drone that asks (`ask`) as the hook tells it.
fn files(g: &Goal, files: &[String], root: &Path, ask: bool) -> Result<Vec<String>, String> {
    let oracle = super::guard::oracle_files(g, root);
    let mut out = Vec::new();
    for f in files {
        let bare = f.trim_start_matches("./").trim_end_matches('/');
        if oracle.iter().any(|o| o == bare) {
            let then = match ask {
                true => "Leave it; if the check is wrong, tell the planner: `borg hive tell \"...\" --to planner`.",
                false => "Change the goal's oracle, or make this change yourself.",
            };
            return Err(format!("{bare} is a file the goal's check reads; no task scope can admit a change to it (the verifier protects it). {then}"));
        }
        let tree = !bare.contains('*') && (f.ends_with('/') || root.join(bare).is_dir());
        out.push(if tree { format!("{bare}/**") } else { f.clone() });
    }
    Ok(out)
}

/// Probes `checks` in the repo at `root`, at once (oracle.rs `probe`): the refusal of the first that
/// cannot pass on any tree; else a note per check left unsaid at the bound.
fn probe(checks: &[&Check], root: &Path) -> Result<Vec<String>, String> {
    let within = bound();
    let got: Vec<(&Check, super::oracle::Probe)> = std::thread::scope(|s| {
        let runs: Vec<_> = checks.iter().map(|c| (*c, s.spawn(move || super::oracle::probe(c, root, within)))).collect();
        runs.into_iter().map(|(c, h)| (c, h.join().unwrap_or(super::oracle::Probe::Unknown))).collect()
    });
    let mut notes = Vec::new();
    for (c, p) in got {
        match p {
            super::oracle::Probe::Unusable(why) => {
                return Err(format!("check {} cannot pass on any tree ({why}). Fix its command: `{}`", c.name, c.cmd));
            }
            super::oracle::Probe::Unknown => notes.push(format!("check {} not probed ({} s): it ran past the bound", c.name, within.as_secs())),
            super::oracle::Probe::Ran => {}
        }
    }
    Ok(notes)
}

/// Checks and normalizes `t` before it is written from `cwd` (`files`, then `probe` of the checks
/// `fresh` says are new): the notes to print, or why it is refused (to a drone's scope ask: `ask`). A
/// task of another repo is checked only for the goal's check files by name: its tree is not here.
pub(super) fn validate(g: &Goal, t: &mut Task, cwd: &str, ask: bool, fresh: impl Fn(&Check) -> bool) -> Result<Vec<String>, String> {
    let root = super::repo_top(Path::new(cwd)).to_path_buf();
    if !t.repo.is_empty() && t.repo != queen::repo_name(cwd) {
        return Ok(Vec::new());
    }
    t.files = files(g, &t.files, &root, ask).map_err(|e| format!("task {}: {e}", t.id))?;
    let new: Vec<&Check> = t.accept.iter().filter(|c| fresh(c)).collect();
    probe(&new, &root).map_err(|e| format!("task {}: {e}", t.id))
}

/// What an amend changed, as the board and the holder's next hook say it (board.rs `AMENDED`).
pub(super) fn amended(old: &Task, new: &Task) -> String {
    let mut parts = Vec::new();
    let added: Vec<&str> = new.files.iter().filter(|f| !old.files.contains(f)).map(String::as_str).collect();
    let gone: Vec<&str> = old.files.iter().filter(|f| !new.files.contains(f)).map(String::as_str).collect();
    if !added.is_empty() || !gone.is_empty() {
        let mut f: Vec<String> = added.iter().map(|a| format!("+{a}")).collect();
        f.extend(gone.iter().map(|g| format!("-{g}")));
        parts.push(format!("files {}", f.join(" ")));
    }
    let checks: Vec<String> = new.accept.iter().filter(|c| !old.accept.contains(c)).map(|c| format!("{}={}", c.name, c.cmd)).collect();
    if !checks.is_empty() || old.accept.len() != new.accept.len() {
        parts.push(format!("checks {}", if new.accept.is_empty() { "the goal's oracle".into() } else { checks.join(", ") }));
    }
    if old.deps != new.deps {
        parts.push(format!("deps {}", if new.deps.is_empty() { "none".into() } else { new.deps.join(",") }));
    }
    if old.title != new.title {
        parts.push(format!("title {}", new.title));
    }
    if parts.is_empty() { "unchanged".into() } else { parts.join("; ") }
}

/// The print of a definition: what the asked marker compares (its title, files and checks).
fn print(t: &Task) -> String {
    let what = serde_json::json!([t.title, t.files, t.accept]).to_string();
    blake3::hash(what.as_bytes()).to_hex()[..12].to_string()
}

fn key(s: &TaskState) -> String {
    let since = match &s.status {
        super::Status::Claimed { since, .. } => *since,
        _ => 0,
    };
    format!("{}@{since}", s.task.id)
}

/// The print, the ask's id and the files asked for that the session's last scope ask on claim `held`
/// recorded (print `-`: told of its amend).
fn asked(session: &str, held: &TaskState) -> Option<(String, String, Vec<String>)> {
    let state = std::fs::read_to_string(hook::state_file(session)).unwrap_or_default();
    let key = key(held);
    let line = state.lines().rev().filter_map(|l| l.strip_prefix(ASKED_MARKER)).find_map(|l| l.strip_prefix(&key)?.strip_prefix(' '))?;
    let mut words = line.splitn(3, ' ');
    let (print, id) = (words.next().unwrap_or_default().to_string(), words.next().unwrap_or_default().to_string());
    Some((print, id, words.next().and_then(|f| serde_json::from_str(f).ok()).unwrap_or_default()))
}

/// The engineer's newest reply to ask `id` in goal `goal` (a `tell --re` from a terminal): their no. A
/// planner session's reply (an acknowledgement, a "hold on") answers nothing: only the engineer does.
fn answer(home: &Path, goal: &str, id: &str) -> Option<message::Message> {
    if id.is_empty() {
        return None;
    }
    message::read(&super::messages_path(home, goal)).into_iter().filter(|m| m.re.as_deref() == Some(id) && no(m)).max_by_key(|m| m.ms)
}

/// Whether message `m` can be the engineer's no to a scope ask: a terminal's (board.rs `by_engineer`).
fn no(m: &message::Message) -> bool {
    super::board::by_engineer(&m.by)
}

/// The files `session`'s scope ask on `held` in goal `goal` asks for while it waits for the engineer's
/// answer: it asked under this claim, the definition is still the one it asked from, and the engineer
/// did not answer. None when no ask waits.
pub(super) fn waiting_for(home: &Path, goal: &str, session: &str, held: &TaskState) -> Option<Vec<String>> {
    asked(session, held).filter(|(p, id, _)| *p == print(&held.task) && answer(home, goal, id).is_none()).map(|(_, _, files)| files)
}

/// Where a drone's scope ask stands, as its waiter reads it (wait.rs).
pub(super) enum Ask {
    /// It waits for the engineer: the definition held is the one it asked from, and no reply came.
    Waits,
    /// The task was amended or the ask answered since: the line that wakes the drone, and the `.ctx`
    /// marker that says it was told.
    Wake { text: String, told: String },
}

/// The scope ask of interactive drone `hs` of `session` on the claim it holds and was shown, if any;
/// none once its wake was told.
pub(super) fn ask_state(home: &Path, hs: &super::HiveSession, session: &str, now: i64) -> Option<Ask> {
    let (held, shown) = super::interactive::unshown(home, hs, session, now)?;
    let (asked, id, _) = asked(session, &held).filter(|(p, _, _)| shown && p != "-")?;
    if asked == print(&held.task) {
        let Some(no) = answer(home, &hs.goal, &id) else { return Some(Ask::Waits) };
        let state = std::fs::read_to_string(hook::state_file(session)).unwrap_or_default();
        if hook::listed(&state, message::MARKER).contains(no.id.as_str()) {
            return None;
        }
        let text = format!(
            "[borg hive {}] the engineer answered your scope ask on {}: {} Finish {} as it is defined: undo what is outside its scope and end your turn (its checks run), or `borg hive block \"why\"`.",
            hs.goal,
            held.task.id,
            no.text.trim_end(),
            held.task.id
        );
        return Some(Ask::Wake { text, told: format!("{}{}", message::MARKER, no.id) });
    }
    let t = &held.task;
    let checks: Vec<&str> = t.accept.iter().map(|c| c.name.as_str()).collect();
    let text = format!(
        "[borg hive {}] the engineer amended {} (your scope ask): files {}; checks {}. Carry on with it: ending your turn runs its checks.",
        hs.goal,
        t.id,
        if t.files.is_empty() { "any".into() } else { t.files.join(", ") },
        if checks.is_empty() { "the goal's oracle".into() } else { checks.join(", ") }
    );
    Some(Ask::Wake { text, told: format!("{ASKED_MARKER}{} -", key(&held)) })
}

/// Marks a wake said to `session` (`Ask::Wake`), so it wakes the drone once.
pub(super) fn told(session: &str, marker: &str) {
    hook::append(session, marker);
}

/// `borg hive scope [--files +a,-b] [--accept NAME=CMD]... "why"`: the holder of a work task asks the
/// engineer for another definition (module doc). Refused: from no holder, for a verify or integrate
/// task, with `--deps` (they change only on a task no one holds), with a file entry for the whole repo,
/// more than `MAX_NEW` new files, or a definition `validate` refuses.
pub(super) fn ask_cmd(here: &Here, named: Option<&str>, a: &Args, why: &str) -> i32 {
    if a.value("--deps").is_some() {
        return fail("a scope ask changes files and checks; deps change only on a task no one holds: tell the planner (`borg hive tell \"...\" --to planner`)");
    }
    let why = why.trim();
    if why.is_empty() {
        return fail("say why the task needs it: `borg hive scope --files +a \"why\"`");
    }
    let (session, g, s) = match held(here, named) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    if !matches!(s.task.role, Role::Work | Role::Plan) {
        return fail(format!("{} is a {} task: its definition is its work's", s.task.id, s.task.role.name()));
    }
    let old = s.task.clone();
    let mut want = old.clone();
    let mut fresh = Vec::new();
    for f in a.list("--files") {
        let (add, path) = match f.strip_prefix('-') {
            Some(p) => (false, p.to_string()),
            None => (true, f.trim_start_matches('+').to_string()),
        };
        let bare = path.trim_start_matches("./").trim_end_matches('/');
        if bare.is_empty() || bare == "." || bare.starts_with('/') || bare.contains("..") || bare.trim_matches(['*', '/']).is_empty() {
            return fail(format!("{path:?} is the whole repo or outside it: a scope ask names the files the task needs"));
        }
        match add {
            true if !want.files.contains(&path) => {
                fresh.push(path.clone());
                want.files.push(path);
            }
            true => {}
            false => want.files.retain(|x| x != &path),
        }
    }
    if fresh.len() > MAX_NEW {
        return fail(format!("{} new files: a scope ask adds at most {MAX_NEW}; a wider change is a new plan, the planner's to make", fresh.len()));
    }
    for c in a.all("--accept") {
        match Check::parse(c, 600) {
            Ok(c) => match want.accept.iter_mut().find(|x| x.name == c.name) {
                Some(x) => *x = c,
                None => want.accept.push(c),
            },
            Err(e) => return fail(e),
        }
    }
    let cwd = here.cwd.clone();
    let notes = match validate(&g, &mut want, &cwd, true, |c| !old.accept.contains(c)) {
        Ok(n) => n,
        Err(e) => return fail(e),
    };
    if want.files == old.files && want.accept == old.accept {
        return fail(format!("{} has that definition already: nothing to ask", old.id));
    }
    // The engineer's amend, exactly: `--again` replaces the files and the checks it is given.
    let mut cmd = format!("borg hive open {} --id {} --again", g.id, old.id);
    if want.files != old.files {
        cmd.push_str(&format!(" --files {}", queen::sh_quote(&want.files.join(","))));
    }
    if want.accept != old.accept {
        for c in &want.accept {
            cmd.push_str(&format!(" --accept {}", queen::sh_quote(&format!("{}={}", c.name, c.cmd))));
        }
    }
    let path = super::messages_path(&here.home, &g.id);
    let here_node = node(&here.home);
    // A newer ask from the same session about the task answers the older one: the planner reads the last.
    let re = message::read(&path).into_iter().filter(|m| !m.cmd.is_empty() && m.by == session && m.task == old.id).max_by_key(|m| m.ms).map(|m| m.id);
    let text = format!("asks to amend {}: {}. Why: {why}", old.id, amended(&old, &want));
    let drone = super::bound(&session).map(|hs| super::drone_for(&hs)).unwrap_or_default();
    let mut m = message::Message::new(message::TELL, &session, &drone, &here_node, &old.id, message::PLANNER, re, here.now, &text);
    m = m.with_cmd(&cmd);
    match message::send(&here.home, &g.id, m) {
        Ok(m) => {
            let asked = serde_json::to_string(&want.files).unwrap_or_default();
            hook::append(&session, &format!("{ASKED_MARKER}{} {} {} {asked}", key(&s), print(&old), m.id));
            for n in &notes {
                println!("{n}");
            }
            println!(
                "scope ask {} sent to the planner of goal {}: the engineer amends {} with\n  {cmd}\nKeep working on what is in scope. Until the answer, a stop held up only by this change ends your turn waiting; the amend or the engineer's no wakes you.",
                m.id, g.id, old.id
            );
            0
        }
        Err(e) => fail(e),
    }
}

/// The scope asks of goal `g` (board `b`) that wait for the engineer, as `borg top`, `borg hive board`
/// and borg-web's Hive tab alert them: the newest ask about each task that is open, held or blocked,
/// with no engineer's open of it written since and no reply of the engineer's to it. From the synced logs, so every node
/// shows them.
pub(super) fn waiting(home: &Path, g: &Goal, b: &super::Board) -> Vec<String> {
    let all = message::read(&super::messages_path(home, &g.id));
    let replied: std::collections::HashSet<&str> = all.iter().filter(|m| no(m)).filter_map(|m| m.re.as_deref()).collect();
    let asks: Vec<&message::Message> = all.iter().filter(|m| !m.cmd.is_empty()).collect();
    if asks.is_empty() {
        return Vec::new();
    }
    let events = super::board::read(&super::tasks_path(home, &g.id));
    let mut newest: std::collections::BTreeMap<&str, &message::Message> = std::collections::BTreeMap::new();
    for m in asks {
        if newest.get(m.task.as_str()).is_none_or(|n| (n.ms, &n.id) < (m.ms, &m.id)) {
            newest.insert(&m.task, m);
        }
    }
    newest
        .into_values()
        .filter(|m| {
            b.tasks.get(&m.task).is_some_and(|s| matches!(s.status, super::Status::Open | super::Status::Claimed { .. } | super::Status::Blocked { .. }))
        })
        .filter(|m| !replied.contains(m.id.as_str()))
        .filter(|m| !events.iter().any(|e| e.ev == super::Ev::Open && e.task == m.task && super::board::by_engineer(&e.by) && e.ms > m.ms))
        .map(|m| {
            let who = if m.drone.is_empty() { m.by.chars().take(8).collect() } else { m.drone.clone() };
            format!(
                "a scope ask waits for the engineer: {who} {} Run in a terminal: {} (or say no: `borg hive tell \"why not\" --goal {} --re {}`)",
                m.text, m.cmd, g.id, m.id
            )
        })
        .collect()
}
