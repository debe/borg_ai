//! `borg no`: the engineer's veto, the strongest negative signal.
//!
//! An engineer says no to a drone's work, with a reason: `borg no <reason>` from a terminal (the drone
//! of the current repo, or the one `--session` names), or a prompt that starts with "borg no" inside the
//! session. Saying it costs 0 tokens and has three effects:
//! - **Credit.** Every lesson shown in the session so far (asked at a stop or live, handed over early or
//!   queued) gets a `vetoed` outcome (outcome.rs), which memory.rs weighs above `asked_then_corrected`.
//! - **A lesson.** The reason is an episode of kind `veto`; `borg learn` generalises it with its own
//!   prompt at the end of any session, so a veto from a terminal never waits for its session. A veto
//!   merged into a lesson an earlier veto made is counted as `veto_repeat`: the same "no" said twice,
//!   the rate learning is to bring to zero.
//! - **A proposed constraint.** When the reason names an invariant of the system, `borg learn` queues it
//!   as a queen question (`Kind::Constraint`). Only an engineer's answer ratifies it into
//!   `<store>/constraints.jsonl`; learning never applies a constraint. A ratified constraint is a
//!   standing lesson (conscience.rs `standing`): every session gets it early once its scope is in play
//!   (an unscoped one at the first prompt), whatever the match, and the mesh syncs the file signed like
//!   the lessons. Nothing blocks an edit or a stop on a constraint yet.
//!
//! - **A missed escalation**, when the vetoed work had already merged and the queen never escalated
//!   from that session (escalation.rs, the queen's attention reward; logged and shown, never optimised).
//!
//! **Across the mesh.** A drone on another node is vetoed by naming it (`--session`, a designation or a
//! session id, as `borg pause` takes it): `borg no` then sends a `veto` message to that session of that
//! node (msg.rs `Kind::Veto`). It counts there only under a steering role the mesh admitted this node
//! with (steer.rs), as any pause. The drone's node records it once when it arrives (`apply_messages`):
//! the credit, the episode and the stat land where the shown lessons and the session are, and its
//! drone is told at its next prompt or tool call, labelled with the sender and role. The issuing node
//! records nothing but `steer_issued`; the `vetoed` outcomes reach it as team outcomes (outcome.rs
//! `SHARED_KINDS`), so both nodes' beliefs count the veto once.
//!
//! In the session the drone is told to stop the vetoed approach and not argue it back. A veto prompt is
//! never the session's task and never doubles as a correction. BORG_VETO=off makes such a prompt an
//! ordinary one (the command still works).

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::json;

use crate::msg::{self, Address, Kind, Message};
use crate::outcome::{self, Outcome};
use crate::presence::State;
use crate::{collective, escalation, learn, presence, queen, stats, steer, top};

/// `Episode::kind` of a veto.
pub const KIND: &str = "veto";

/// The veto prompt's opening; `learn::schema_for` picks the veto schema by it.
pub const OPENING: &str = "An engineer vetoed a coding agent's work: they said no to what it was doing, with the reason below. \
Decide whether the reason expresses a reusable engineering lesson: a critical concern about quality, design, correctness, reuse, \
spec conformance or root cause that this engineer would plausibly raise again about similar future work (a veto is rarely \
operational; \"stop\", \"later\" are NOT lessons).";

/// The extra field line in the veto prompt.
pub const INVARIANT: &str = "- invariant: if the reason names an invariant of the system (a property that must always hold, e.g. \
\"merge-to-main lookups go through the review API, never the commit log\"), state it as one rule; otherwise empty. Never invent one.";

/// The answers a constraint question offers: the default never applies anything.
pub const KEEP_OPTION: &str = "keep it a lesson only";
pub const RATIFY_OPTION: &str = "ratify: make it a constraint (only an engineer can)";

static PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)^\s*borg\s+no\b[\s:,.-]*(.*)$").unwrap());

/// False when BORG_VETO=off: a prompt starting "borg no" is an ordinary prompt.
pub fn enabled() -> bool {
    std::env::var("BORG_VETO").as_deref() != Ok("off")
}

/// The reason of a prompt that is a veto ("borg no: never …"), trimmed, at most 1500 characters; None
/// when it is not a veto or its reason is shorter than 4 characters.
pub fn reason_in(prompt: &str) -> Option<String> {
    let reason = PREFIX.captures(prompt)?.get(1)?.as_str().trim();
    (reason.chars().count() >= 4).then(|| reason.chars().take(1500).collect())
}

/// The lessons the drone was shown in `session` so far, sorted and unique: its `asked` outcomes (stop
/// or live) and its stats events of the delivering kinds (early, queued, live asks, blocks).
pub fn shown(home: &Path, session: &str) -> Vec<String> {
    const DELIVERED: [&str; 4] = [stats::EARLY, stats::QUEUED_FREE, stats::LIVE_ASK, stats::BLOCK];
    let asked = outcome::read(&home.join("outcomes.jsonl")).into_iter().filter(|o| o.kind == outcome::ASKED && o.session == session).flat_map(|o| o.lessons);
    let delivered = stats::events(home).into_iter().filter(|e| e.session == session && DELIVERED.contains(&e.kind.as_str())).flat_map(|e| e.lessons);
    let mut out: Vec<String> = asked.chain(delivered).filter(|l| !l.is_empty()).collect();
    out.sort();
    out.dedup();
    out
}

/// One veto as said.
pub struct Said<'a> {
    pub session: &'a str,
    pub repo: &'a str,
    pub engineer: &'a str,
    /// The session's task, as the hooks kept it.
    pub task: &'a str,
    /// The drone's last proposal; empty from a terminal (no transcript there).
    pub agent: &'a str,
    pub reason: &'a str,
    pub ms: i64,
    /// `cli`, `prompt` or `mesh` (a signed directive from another node, `apply`).
    pub via: &'static str,
}

/// What `record` wrote: the episode's id, the lessons credited `vetoed` now, and whether the veto
/// counted as a missed escalation (escalation.rs).
#[derive(Debug, Clone, PartialEq)]
pub struct Veto {
    pub episode: String,
    pub credited: Vec<String>,
    pub missed: bool,
}

/// Records one veto in `home`: the episode, a `vetoed` outcome per shown lesson not yet credited in
/// the session (one per lesson keeps `outcome::has` idempotent; nothing when none, so a session with
/// nothing shown adds no clock tick), and one `veto` stat for every call. A veto of work that had
/// already merged, from a session the queen never escalated, is also a missed escalation
/// (escalation.rs `missed`). The caller checks `learn::learning()` first.
///
/// # Errors
/// When the episode or an outcome cannot be appended.
pub fn record(home: &Path, s: &Said) -> std::io::Result<Veto> {
    let e = learn::Episode {
        id: format!("no-{}-{}", s.session.chars().take(8).collect::<String>(), s.ms),
        ms: s.ms,
        repo: s.repo.into(),
        session: s.session.into(),
        task: s.task.into(),
        agent: s.agent.into(),
        rejected_tool: None,
        correction: s.reason.into(),
        engineer: s.engineer.into(),
        kind: KIND.into(),
        ..learn::Episode::default()
    };
    learn::record_episode_in(home, &e)?;
    let path = home.join("outcomes.jsonl");
    let outcomes = outcome::read(&path);
    let credited: Vec<String> = shown(home, s.session).into_iter().filter(|l| !outcome::has(&outcomes, outcome::VETOED, s.session, l)).collect();
    for l in &credited {
        let o =
            Outcome { kind: outcome::VETOED.into(), session: s.session.into(), lessons: vec![l.clone()], ms: s.ms, repo: s.repo.into(), ..Default::default() };
        outcome::append(&path, &o)?;
    }
    stats::count(home, stats::VETO, s.session, |ev| {
        ev.ms = s.ms;
        ev.engineer = s.engineer.into();
        ev.repo = s.repo.into();
        ev.lessons = credited.clone();
        ev.n = credited.len() as u64;
        ev.why = s.via.into();
        ev.text = stats::excerpt(s.reason, 160);
    });
    let missed = escalation::merged(&crate::notes::state_dir(), s.session) && escalation::missed(home, s.session, KIND, &e.id, s.repo, s.ms);
    Ok(Veto { episode: e.id, credited, missed })
}

/// The session's task as the hooks kept it (`<state>/<session>.task`), else `fallback` (its last prompt).
fn task_of(home: &Path, session: &str, fallback: &str) -> String {
    let state = std::env::var_os("BORG_STATE").map_or_else(|| home.join("state"), PathBuf::from);
    std::fs::read_to_string(state.join(format!("{}.task", presence::safe(session))))
        .ok()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

/// Records the veto a message carries (msg.rs `Kind::Veto`), once, on the node `here` whose session it
/// names: `borg no` for a drone on another machine sends it from there (`remote`), the mesh carries it
/// here like any message, and this node holds what the veto credits (the lessons the session was
/// shown) and learns its episode, so its beliefs and its repeat-veto count see it. The caller checks
/// that it counts (steer.rs `Roles`). The issuing node records nothing but its `steer_issued` stat;
/// the `vetoed` outcomes reach it as team outcomes. Once: `<store>/messages/vetoed/<id>` is created
/// exclusively first (removed again if recording fails, so the next round retries). None for any other
/// message, one for another node, or one already recorded.
///
/// # Errors
/// When the marker or the veto cannot be written.
pub fn apply(home: &Path, m: &Message, here: &str) -> std::io::Result<Option<Veto>> {
    let Some(Address::Session { id: session, node: Some(node) }) = Address::parse(&m.to) else { return Ok(None) };
    if m.kind != Kind::Veto || node != here || m.text.trim().is_empty() {
        return Ok(None);
    }
    // A session unknown here is another node's, or none.
    let session = session.as_str();
    let p = presence::read(home, session);
    if p.is_none() && !outcome::read(&home.join("outcomes.jsonl")).iter().any(|o| o.session == session) {
        return Ok(None);
    }
    let dir = msg::dir(home).join("vetoed");
    std::fs::create_dir_all(&dir)?;
    let marker = dir.join(presence::safe(&m.id));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&marker) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(None),
        Err(e) => return Err(e),
    }
    let repo = p.as_ref().map(|p| p.repo.clone()).unwrap_or_default();
    let task = task_of(home, session, p.as_ref().map_or("", |p| p.prompt.as_str()));
    let reason: String = m.text.trim().chars().take(1500).collect();
    // The engineer's email when an engineer sent it, else the sending session's name.
    let engineer = match Address::parse(&m.by) {
        Some(Address::Engineer { email }) if !email.is_empty() => email,
        _ => m.name.clone(),
    };
    let said = Said { session, repo: &repo, engineer: &engineer, task: &task, agent: "", reason: &reason, ms: m.ms, via: "mesh" };
    record(home, &said).map(Some).inspect_err(|_| {
        let _ = std::fs::remove_file(&marker);
    })
}

/// Records every admitted veto message for a session of this node not recorded yet (`apply`); the mesh
/// calls it after it admits a round's messages (steer.rs `admit`) and at every `enforce`, which
/// retries what failed. Nothing with learning off or while nobody steers. Returns how many it recorded.
pub fn apply_messages(home: &Path, now: i64) -> usize {
    if !learn::learning() {
        return 0;
    }
    let roles = steer::roles(home);
    if roles.is_empty() {
        return 0;
    }
    let here = msg::node(home);
    let lines: Vec<Message> = crate::jsonl::read_lines(&msg::path(home));
    lines
        .iter()
        .filter(|m| m.kind == Kind::Veto && roles.has(&m.id) && m.well_formed() && roles.of(m, &here, now).is_some())
        .filter_map(|m| apply(home, m, &here).ok().flatten())
        .count()
}

/// What the drone is told when the engineer vetoes in the session.
pub fn render(reason: &str) -> String {
    format!(
        "[borg] The engineer said no to this work: \"{reason}\". Stop the vetoed approach now; do not argue it back. If the reason is unclear, ask one short clarifying question, then redo the work within it."
    )
}

fn short(session: &str) -> String {
    session.chars().take(8).collect()
}

/// The drone a terminal veto names (pure). `named`: a designation, number or session id (prefix), as
/// `borg pause` takes it, of a local drone or one on another node (`main` then sends it as a veto
/// message). Otherwise the local drone working in `repo`: the one not ended, else the most recently
/// seen ended one (a veto right after the session ended); several active ones are an error, never a
/// guess. A remote drone is never picked unnamed.
///
/// # Errors
/// What to name instead.
pub fn target<'a>(drones: &'a [top::Drone], repo: &str, named: Option<&str>) -> Result<&'a top::Drone, String> {
    if let Some(n) = named {
        return match top::resolve(drones, n)? {
            None => Err("a veto names one drone, not all".into()),
            Some(d) => Ok(d),
        };
    }
    let here: Vec<&top::Drone> = drones.iter().filter(|d| d.node.is_empty() && d.p.repo == repo).collect();
    let active: Vec<&top::Drone> = here.iter().copied().filter(|d| d.state != State::Ended).collect();
    match active.as_slice() {
        [d] => Ok(d),
        [] => here.into_iter().max_by_key(|d| d.p.ms).ok_or_else(|| format!("no drone in {repo} (`borg top`): name a session with --session")),
        many => {
            let names: Vec<String> = many.iter().map(|d| format!("{} {}", d.designation, short(&d.p.session))).collect();
            Err(format!("{} drones work in {repo}: name one with --session ({})", many.len(), names.join(", ")))
        }
    }
}

/// Queues the invariant a veto's lesson named as a `Kind::Constraint` question, never a constraint: the
/// default keeps it a lesson only. Dedups by key and near-duplicate (`queen::seed`). Returns whether it
/// was queued.
///
/// # Errors
/// When the queue cannot be appended.
pub fn propose(home: &Path, lesson: &learn::Lesson, invariant: &str, e: &learn::Episode, now: i64) -> std::io::Result<bool> {
    let key = queen::constraint_key(&lesson.repo, invariant);
    let entry = queen::Entry {
        id: format!("q{}", &key[..8]),
        decision_key: key,
        kind: queen::Kind::Constraint,
        question: format!("Make this a constraint of the collective? \"{}\"", stats::excerpt(invariant, 300)),
        options: vec![KEEP_OPTION.into(), RATIFY_OPTION.into()],
        default: KEEP_OPTION.into(),
        evidence: vec![format!("invariant: {invariant}"), format!("lesson {}", lesson.id), format!("veto: {}", stats::excerpt(&e.correction, 200))],
        session: e.session.clone(),
        cwd: String::new(),
        repo: lesson.repo.clone(),
        engineer: e.engineer.clone(),
        ms: now,
        answer: None,
        answered_by: None,
        answered_ms: None,
        disagreeing: Vec::new(),
        dismissed: None,
        dismissed_ms: None,
        payload: None,
    };
    queen::seed(home, &entry)
}

/// The ratified constraints of a store, beside its lessons (`constraints_path`).
pub const FILE: &str = "constraints.jsonl";

/// The constraints file of a store. Written on ratification, read with the lessons as standing ones
/// (conscience.rs `load`) and synced by the mesh (borg-mesh lines.rs `SYNCED`).
pub fn constraints_path(store: &Path) -> PathBuf {
    store.join(FILE)
}

/// The answer to a constraint question (queen.rs `answer`): an answer starting with "ratify" (option
/// b) appends the invariant to `constraints.jsonl` and returns true; anything else writes nothing.
/// The line syncs to the team, so it carries what a lesson does and no more: the invariant (redacted,
/// as a lesson is generalised), the lesson it came from and who ratified it, never the veto's raw words.
/// It also carries who should hear it: the lesson's scope, repo and reach (reach.rs), or, the lesson
/// unknown here, the question's repo with reach `repo`, so a node without the lesson never widens it.
///
/// # Errors
/// When the constraints file cannot be appended.
pub fn ratify(store: &Path, e: &queen::Entry, answer: &str, author: &str, now: i64) -> Result<bool, String> {
    if !answer.trim().to_lowercase().starts_with("ratify") {
        return Ok(false);
    }
    let field = |prefix: &str| e.evidence.iter().find_map(|x| x.strip_prefix(prefix)).unwrap_or_default().to_string();
    let lesson = field("lesson ");
    let from = crate::conscience::load_unweighted(&store.join("lessons.jsonl"), None).into_iter().find(|l| l.id == lesson);
    let (scope, repo, reach) = match from {
        Some(l) => (l.scope, if l.repo.is_empty() { e.repo.clone() } else { l.repo }, l.reach),
        None => (Vec::new(), e.repo.clone(), crate::reach::REPO.to_string()),
    };
    let line = json!({"id": format!("c{}", &e.decision_key[..8.min(e.decision_key.len())]), "text": crate::notes::redact(&field("invariant: ")),
        "lesson": lesson, "scope": scope, "repo": repo, "reach": reach, "by": author, "ms": now, "source": "veto", "check": ""});
    learn::append_jsonl(&constraints_path(store), &line).map_err(|err| err.to_string())?;
    Ok(true)
}

/// `borg no`'s flags (completions.rs).
pub const FLAGS: &[&str] = &["--session", "--dir", "--json"];

const USAGE: &str = "usage: borg no [--session ID|DRONE] [--dir DIR] [--json] <reason…>
  says no to a drone's work: the lessons it was shown lose credit, and the reason becomes a lesson";

/// A veto of the drone `d` on another node: a veto message to its session of its node (the drone's node
/// records it, `apply_messages`).
fn remote(home: &Path, drones: &[top::Drone], d: &top::Drone, cwd: &str, reason: &str, now: i64) -> i32 {
    let to = Address::Session { id: d.p.session.clone(), node: Some(d.node.clone()) };
    let sent = msg::sender(home, drones, None, cwd).and_then(|from| msg::send(home, Message::new(Kind::Veto, &from, &to.to_string(), None, now, reason), now));
    match sent {
        Ok(m) => {
            println!(
                "vetoed {} ({}) on another machine: {}, recorded there after the next mesh round (`borg mesh serve`)",
                d.designation,
                short(&d.p.session),
                m.id
            );
            0
        }
        Err(e) => {
            eprintln!("borg no: cannot send the veto: {e}");
            1
        }
    }
}

/// `borg no [--session ID|DRONE] [--dir DIR] [--json] <reason…>`.
pub fn main(args: &[&str]) -> i32 {
    let value = |f: &str| args.iter().position(|a| *a == f).and_then(|i| args.get(i + 1)).copied();
    let mut words = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--session" | "--dir" => i += 2,
            "--json" => i += 1,
            w => {
                words.push(w);
                i += 1;
            }
        }
    }
    let reason = words.join(" ").trim().to_string();
    if reason.is_empty() {
        eprintln!("{USAGE}");
        return 2;
    }
    // Routing compares path text, so a relative --dir is made absolute first, as hooks' cwd is.
    let dir = value("--dir").map_or_else(|| std::env::current_dir().unwrap_or_default(), PathBuf::from);
    let cwd = std::path::absolute(&dir).unwrap_or(dir).display().to_string();
    let route = match collective::load() {
        Ok(cfg) => collective::route(cfg.as_ref(), &cwd, &learn::home()),
        Err(e) => {
            eprintln!("borg no: invalid collectives config: {e}");
            return 1;
        }
    };
    let collective::Route::Collective { home, .. } = &route else {
        eprintln!("off: {cwd} belongs to no collective; borg records nothing here");
        return 1;
    };
    if !learn::learning() {
        println!("learning is off (BORG_LEARN=off): nothing recorded");
        return 0;
    }
    let now = learn::now_ms();
    // The same drones and designations as `borg top`; remote ones are listed but never targeted.
    let drones = top::drones(home, collective::may_sync(&route), now);
    let d = match target(&drones, &queen::repo_name(&cwd), value("--session")) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("borg no: {e}");
            return 1;
        }
    };
    let session = d.p.session.as_str();
    if !d.node.is_empty() {
        return remote(home, &drones, d, &cwd, &reason, now);
    }
    let task = task_of(home, session, &d.p.prompt);
    let engineer = learn::engineer(&cwd);
    let said = Said { session, repo: &d.p.repo, engineer: &engineer, task: &task, agent: "", reason: &reason, ms: now, via: "cli" };
    let v = match record(home, &said) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("borg no: cannot record the veto: {e}");
            return 1;
        }
    };
    if args.contains(&"--json") {
        println!("{}", json!({"session": session, "episode": v.episode, "credited": v.credited}));
        return 0;
    }
    let lost = match v.credited.len() {
        0 => "no shown lessons lose credit".to_string(),
        n => format!("{n} shown lesson(s) lose credit ({})", v.credited.join(", ")),
    };
    println!("vetoed {} ({}): {lost}; the reason becomes a lesson when borg learn runs", d.designation, short(session));
    println!("to stop it now: borg pause {}", short(session));
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::Presence;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-veto-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_veto_prompt_starts_with_borg_no_and_carries_a_reason() {
        assert_eq!(reason_in("borg no: never read the commit log").as_deref(), Some("never read the commit log"));
        assert_eq!(reason_in("Borg no, don't").as_deref(), Some("don't"));
        assert_eq!(reason_in("  BORG NO\nthe schema is public").as_deref(), Some("the schema is public"));
        assert_eq!(reason_in("borg now run the tests"), None);
        assert_eq!(reason_in("no, use X"), None, "a correction, not a veto");
        assert_eq!(reason_in("borg no"), None);
        assert_eq!(reason_in("borg no: ok"), None, "too short a reason");
        assert_eq!(reason_in(&format!("borg no {}", "x".repeat(2000))).map(|r| r.chars().count()), Some(1500));
    }

    fn drone(session: &str, repo: &str, state: State, ms: i64, node: &str, number: usize) -> top::Drone {
        top::Drone {
            designation: format!("{} of Five", top::word(number)),
            number,
            node: node.into(),
            state,
            paused: false,
            p: Presence {
                session: session.into(),
                harness: "claude".into(),
                engineer: "a@x".into(),
                repo: repo.into(),
                collective: "corp".into(),
                prompt: "p".into(),
                files: vec![],
                event: "prompt".into(),
                ms,
                started: ms,
                pid: None,
            },
        }
    }

    #[test]
    fn a_terminal_veto_targets_the_one_local_drone_of_the_repo_or_asks() {
        let one = vec![drone("aaaa1111", "app", State::Working, 10, "", 1), drone("bbbb2222", "other", State::Working, 20, "", 2)];
        assert_eq!(target(&one, "app", None).unwrap().p.session, "aaaa1111");
        assert!(target(&one, "nope", None).unwrap_err().contains("no drone in nope"));
        let two = vec![drone("aaaa1111", "app", State::Working, 10, "", 1), drone("cccc3333", "app", State::Idle, 30, "", 2)];
        let err = target(&two, "app", None).unwrap_err();
        assert!(err.contains("2 drones work in app") && err.contains("aaaa1111") && err.contains("cccc3333") && err.contains("One of Five"), "{err}");
        assert_eq!(target(&two, "app", Some("Two")).unwrap().p.session, "cccc3333");
        assert_eq!(target(&two, "app", Some("aaaa")).unwrap().p.session, "aaaa1111");
        assert!(target(&two, "app", Some("all")).is_err());
        // Only ended ones: the most recently seen (a veto right after the session ended).
        let ended = vec![drone("aaaa1111", "app", State::Ended, 50, "", 1), drone("cccc3333", "app", State::Ended, 40, "", 2)];
        assert_eq!(target(&ended, "app", None).unwrap().p.session, "aaaa1111");
        // A remote drone is never picked unnamed; named, it is (`main` signs the veto for its node).
        let remote = vec![drone("dddd4444", "app", State::Working, 10, "beef", 1)];
        assert!(target(&remote, "app", None).is_err());
        assert_eq!(target(&remote, "app", Some("dddd")).unwrap().node, "beef");
    }

    fn asked(home: &Path, session: &str, lessons: &[&str]) {
        let o = Outcome {
            kind: outcome::ASKED.into(),
            session: session.into(),
            lessons: lessons.iter().map(|l| l.to_string()).collect(),
            ms: 1,
            ..Default::default()
        };
        outcome::append(&home.join("outcomes.jsonl"), &o).unwrap();
    }

    fn event(home: &Path, kind: &str, session: &str, lessons: &[&str]) {
        let ev =
            stats::Event { kind: kind.into(), ms: 2, session: session.into(), lessons: lessons.iter().map(|l| l.to_string()).collect(), ..Default::default() };
        stats::record(home, &ev);
    }

    #[test]
    fn shown_is_every_lesson_the_session_was_shown() {
        let home = temp("shown");
        asked(&home, "s1", &["C", "A"]);
        asked(&home, "s2", &["X"]);
        event(&home, stats::EARLY, "s1", &["B", "A"]);
        event(&home, stats::QUEUED_FREE, "s1", &["D"]);
        event(&home, stats::EARLY, "s2", &["Y"]);
        event(&home, stats::CHECK, "s1", &["Z"]);
        assert_eq!(shown(&home, "s1"), ["A", "B", "C", "D"]);
        assert!(shown(&home, "s3").is_empty());
        let _ = std::fs::remove_dir_all(home);
    }

    fn said<'a>(session: &'a str, reason: &'a str, ms: i64) -> Said<'a> {
        Said { session, repo: "app", engineer: "a@x", task: "fix the merge lookup", agent: "", reason, ms, via: "cli" }
    }

    #[test]
    fn a_veto_records_its_episode_credits_shown_lessons_once_and_counts_every_call() {
        let home = temp("record");
        asked(&home, "s1", &["A"]);
        event(&home, stats::EARLY, "s1", &["B"]);
        let v = record(&home, &said("s1", "never read the commit log for merges", 100)).unwrap();
        assert_eq!(v, Veto { episode: "no-s1-100".into(), credited: vec!["A".into(), "B".into()], missed: false });
        let eps: Vec<learn::Episode> =
            std::fs::read_to_string(home.join("episodes.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!((eps.len(), eps[0].kind.as_str(), eps[0].correction.as_str()), (1, KIND, "never read the commit log for merges"));
        let vetoed = || outcome::read(&home.join("outcomes.jsonl")).into_iter().filter(|o| o.kind == outcome::VETOED).count();
        assert_eq!(vetoed(), 2);
        // A second veto in the same session: no duplicate credit, but a second veto stat.
        let again = record(&home, &said("s1", "and not the reflog either", 200)).unwrap();
        assert!(again.credited.is_empty());
        assert_eq!(vetoed(), 2);
        let vetoes: Vec<stats::Event> = stats::events(&home).into_iter().filter(|e| e.kind == stats::VETO).collect();
        assert_eq!(vetoes.len(), 2);
        assert_eq!((vetoes[0].lessons.clone(), vetoes[0].n, vetoes[0].why.as_str()), (vec!["A".to_string(), "B".to_string()], 2, "cli"));
        // No correction is counted for a veto: it has its own stat.
        assert!(!stats::events(&home).iter().any(|e| e.kind == stats::CORRECTION));
        // A session that was shown nothing adds no outcome line (no tick on the memory clock).
        let before = std::fs::read_to_string(home.join("outcomes.jsonl")).unwrap();
        record(&home, &said("s9", "never deploy on fridays", 300)).unwrap();
        assert_eq!(std::fs::read_to_string(home.join("outcomes.jsonl")).unwrap(), before);
        let _ = std::fs::remove_dir_all(home);
    }

    const PEER: &str = "abababababababababababababababababababababababababababababababab";

    /// A message from the peer node `PEER`, merged into the log as the mesh would, admitted under
    /// `role` (`None`: not admitted).
    fn arrive(home: &Path, kind: Kind, to: &str, text: &str, ms: i64, role: Option<steer::Role>) -> Message {
        let from = msg::Sender { by: "engineer:lead@x".into(), name: "lead@x".into(), harness: "terminal".into(), node: PEER.into() };
        let m = Message::new(kind, &from, to, None, ms, text);
        learn::append_line(&msg::path(home), &serde_json::to_string(&m).unwrap()).unwrap();
        if let Some(r) = role {
            steer::admit(home, &[steer::Admission::of(&m, r)], ms).unwrap();
        }
        m
    }

    #[test]
    fn a_veto_from_another_node_is_recorded_once_where_its_drone_runs() {
        let home = temp("apply");
        asked(&home, "s1", &["A"]);
        let here = msg::node(&home);
        let s1 = format!("s1@{here}");
        let v = arrive(&home, Kind::Veto, &s1, "never read the commit log for merges", 100, Some(steer::Role::Steer));
        let got = apply(&home, &v, &here).unwrap().unwrap();
        assert_eq!((got.episode.as_str(), got.credited.clone(), got.missed), ("no-s1-100", vec!["A".to_string()], false));
        assert_eq!(apply(&home, &v, &here).unwrap(), None, "the same message is recorded once");
        // Through the log (the mesh's way in): already recorded, and a tell or an unadmitted veto never records.
        arrive(&home, Kind::Tell, &s1, "run the tests", 110, Some(steer::Role::Steer));
        arrive(&home, Kind::Veto, &s1, "not admitted", 120, None);
        assert_eq!(apply_messages(&home, 200), 0);
        let eps = std::fs::read_to_string(home.join("episodes.jsonl")).unwrap();
        assert_eq!(eps.lines().count(), 1);
        let e: learn::Episode = serde_json::from_str(eps.lines().next().unwrap()).unwrap();
        assert_eq!((e.kind.as_str(), e.engineer.as_str(), e.session.as_str()), (KIND, "lead@x", "s1"), "the sender's veto, of its session");
        let vetoes: Vec<stats::Event> = stats::events(&home).into_iter().filter(|e| e.kind == stats::VETO).collect();
        assert_eq!((vetoes.len(), vetoes[0].why.as_str()), (1, "mesh"));
        assert_eq!(outcome::read(&home.join("outcomes.jsonl")).iter().filter(|o| o.kind == outcome::VETOED).count(), 1);
        // A new admitted one through the log is recorded there, once; past the TTL it no longer counts.
        arrive(&home, Kind::Veto, &s1, "and not the reflog either", 130, Some(steer::Role::Admin));
        assert_eq!(apply_messages(&home, 200), 1);
        assert_eq!(apply_messages(&home, 200), 0);
        arrive(&home, Kind::Veto, &s1, "too late", 140, Some(steer::Role::Admin));
        assert_eq!(apply_messages(&home, 140 + steer::TTL_MS + 1), 0);
        // A veto of a session this node never held, or of another node's, is not this node's to learn.
        let stranger = arrive(&home, Kind::Veto, &format!("zz@{here}"), "not here", 150, Some(steer::Role::Steer));
        assert_eq!(apply(&home, &stranger, &here).unwrap(), None);
        let elsewhere = arrive(&home, Kind::Veto, "s1@n2", "elsewhere", 160, Some(steer::Role::Steer));
        assert_eq!(apply(&home, &elsewhere, &here).unwrap(), None);
        assert_eq!(std::fs::read_to_string(home.join("episodes.jsonl")).unwrap().lines().count(), 2);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn an_invariant_is_proposed_and_only_ratifying_writes_a_constraint() {
        let home = temp("propose");
        let l = learn::Lesson { id: "Lno-s1-1".into(), repo: "app".into(), question: "q".into(), ..Default::default() };
        let e = learn::Episode {
            id: "no-s1-1".into(),
            session: "s1".into(),
            engineer: "a@x".into(),
            correction: "never the commit log".into(),
            ..Default::default()
        };
        let rule = "merge lookups go through the review API";
        assert!(propose(&home, &l, rule, &e, 5).unwrap());
        assert!(!propose(&home, &l, "Merge lookups go through the review API.", &e, 6).unwrap(), "the same invariant once");
        let q = queen::entries(&home).into_iter().find(|x| x.kind == queen::Kind::Constraint).unwrap();
        assert_eq!((q.default.as_str(), q.options.len(), q.repo.as_str()), (KEEP_OPTION, 2, "app"));
        assert!(q.question.contains(rule) && q.evidence[0] == format!("invariant: {rule}"));
        assert!(!constraints_path(&home).exists(), "proposing never applies");
        // Keeping it a lesson writes nothing; ratifying writes exactly one constraint.
        assert!(!ratify(&home, &q, KEEP_OPTION, "lead@x", 7).unwrap());
        assert!(!constraints_path(&home).exists());
        assert!(ratify(&home, &q, RATIFY_OPTION, "lead@x", 8).unwrap());
        let text = std::fs::read_to_string(constraints_path(&home)).unwrap();
        assert_eq!(text.lines().count(), 1);
        let c: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(
            (c["text"].as_str(), c["lesson"].as_str(), c["by"].as_str(), c["source"].as_str()),
            (Some(rule), Some("Lno-s1-1"), Some("lead@x"), Some("veto"))
        );
        // The lesson unknown here: bound to the question's repo, never everywhere.
        assert_eq!((c["repo"].as_str(), c["reach"].as_str()), (Some("app"), Some(crate::reach::REPO)));
        // The lesson known: its scope, repo and reach.
        let lesson = serde_json::json!({"id": "Lno-s1-1", "origin_ms": 1, "question": "q", "repo": "app", "scope": ["Orion"]});
        std::fs::write(home.join("lessons.jsonl"), format!("{lesson}\n")).unwrap();
        std::fs::remove_file(constraints_path(&home)).unwrap();
        assert!(ratify(&home, &q, RATIFY_OPTION, "lead@x", 9).unwrap());
        let c: serde_json::Value = serde_json::from_str(std::fs::read_to_string(constraints_path(&home)).unwrap().trim()).unwrap();
        assert_eq!((c["scope"].clone(), c["repo"].as_str(), c["reach"].as_str()), (serde_json::json!(["Orion"]), Some("app"), Some("")));
        let _ = std::fs::remove_dir_all(home);
    }
}
