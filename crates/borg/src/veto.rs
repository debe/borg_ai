//! `borg no`: the engineer's veto, the strongest negative signal (docs/design/hive-learning.md "The
//! engineer's \"no\"", step 7b).
//!
//! An engineer says no to a drone's work, with a reason: `borg no <reason>` from a terminal (the drone
//! of the current repo, or the one `--session` names), or a prompt that starts with "borg no" inside the
//! session. Saying it costs 0 tokens and has three effects:
//! - **Credit.** Every lesson shown in the session so far (asked at a stop or live, handed over early or
//!   queued) gets a `vetoed` outcome (outcome.rs), which memory.rs weighs above `asked_then_corrected`.
//! - **A lesson.** The reason is an episode of kind `veto`; `borg learn` generalises it with its own
//!   prompt at the end of any session, so a veto from a terminal never waits for its session. A veto
//!   merged into a lesson an earlier veto made is counted as `veto_repeat`: the same "no" said twice,
//!   the rate the hive learns to bring to zero (H5b).
//! - **A proposed constraint.** When the reason names an invariant of the system, `borg learn` queues it
//!   as a queen question (`Kind::Constraint`). Only an engineer's answer ratifies it into
//!   `<store>/constraints.jsonl`; learning never applies a constraint, and nothing enforces them yet.
//!
//! - **A missed escalation**, when the vetoed work had already merged and the queen never escalated
//!   from that session (escalation.rs, the queen's attention reward; logged and shown, never optimised).
//!
//! **Across the mesh.** A drone on another node is vetoed by naming it (`--session`, a designation or a
//! session id, as `borg steer` takes it): `borg no` then signs a `veto` steering directive with this
//! node's member key (borg-mesh steer.rs; the signer needs a steering role, as for any directive to
//! another engineer's drone). The drone's node records it once when it arrives (`apply`): the credit,
//! the episode and the stat land where the shown lessons and the session are, and its drone is told at
//! its next hook, labelled with the signer. The issuing node records nothing but `steer_issued`; the
//! `vetoed` outcomes reach it as team outcomes (outcome.rs `SHARED_KINDS`), so both nodes' beliefs
//! count the veto once.
//!
//! In the session the drone is told to stop the vetoed approach and not argue it back. A veto prompt is
//! never the session's task and never doubles as a correction. BORG_VETO=off makes such a prompt an
//! ordinary one (the command still works).

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::json;

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
/// or live), the playbook versions shown to it (`outcome::PLAYBOOK`, playbook.rs), and its stats events
/// of the delivering kinds (early, queued, live asks, blocks).
pub fn shown(home: &Path, session: &str) -> Vec<String> {
    shown_between(home, session, i64::MIN, i64::MAX)
}

/// `shown`, of the records and events at `from_ms..=to_ms` only: what a hive drone was shown while it
/// held a claim (hive/verdict.rs credits them with the task's verdict).
pub fn shown_between(home: &Path, session: &str, from_ms: i64, to_ms: i64) -> Vec<String> {
    const DELIVERED: [&str; 4] = [stats::EARLY, stats::QUEUED_FREE, stats::LIVE_ASK, stats::BLOCK];
    let within = |ms: i64| (from_ms..=to_ms).contains(&ms);
    let asked = outcome::read(&home.join("outcomes.jsonl"))
        .into_iter()
        .filter(|o| outcome::shows(&o.kind) && o.session == session && within(o.ms))
        .flat_map(|o| o.lessons);
    let delivered =
        stats::events(home).into_iter().filter(|e| e.session == session && within(e.ms) && DELIVERED.contains(&e.kind.as_str())).flat_map(|e| e.lessons);
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
    let missed = escalation::merged(home, &crate::notes::state_dir(), s.session, s.ms) && escalation::missed(home, s.session, KIND, &e.id, s.repo, s.ms);
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

/// Records the veto a signed directive carries (steer.rs `VETO`), once, on the node whose drone it
/// names: `borg no` for a drone on another machine signs it there (borg-mesh steer.rs `issue`), the
/// mesh carries it here like any steering directive, and this node holds what the veto credits (the
/// lessons the session was shown) and learns its episode, so its beliefs and its repeat-veto count see
/// it. The issuing node records nothing but its `steer_issued` stat; the `vetoed` outcomes reach it as
/// team outcomes. Once: `<store>/steer/vetoed/<id>` is created exclusively first (removed again if
/// recording fails, so the next round retries). None for any other directive, or one already recorded.
///
/// # Errors
/// When the marker or the veto cannot be written.
pub fn apply(home: &Path, a: &steer::Accepted) -> std::io::Result<Option<Veto>> {
    let session = a.d.target.session.as_str();
    if a.d.kind != steer::VETO || a.d.target.node.is_empty() || session.is_empty() || a.d.text.trim().is_empty() {
        return Ok(None);
    }
    // Only the node whose drone it names holds the session (borg-mesh steer.rs `check` refuses a veto
    // without one): a session unknown here is another node's, or none.
    let p = presence::read(home, session);
    if p.is_none() && !outcome::read(&home.join("outcomes.jsonl")).iter().any(|o| o.session == session) {
        return Ok(None);
    }
    let dir = steer::dir(home).join("vetoed");
    std::fs::create_dir_all(&dir)?;
    let marker = dir.join(presence::safe(&a.d.id));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&marker) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(None),
        Err(e) => return Err(e),
    }
    let repo = p.as_ref().map(|p| p.repo.clone()).unwrap_or_default();
    let task = task_of(home, session, p.as_ref().map_or("", |p| p.prompt.as_str()));
    let reason: String = a.d.text.trim().chars().take(1500).collect();
    let said = Said { session, repo: &repo, engineer: &a.d.engineer, task: &task, agent: "", reason: &reason, ms: a.d.ms, via: "mesh" };
    record(home, &said).map(Some).inspect_err(|_| {
        let _ = std::fs::remove_file(&marker);
    })
}

/// Records every veto directive in this store's inbox not recorded yet (`apply`); the mesh calls it
/// after each round's steering. Nothing with learning off. Returns how many it recorded.
pub fn apply_inbox(home: &Path) -> usize {
    if !learn::learning() {
        return 0;
    }
    steer::read_inbox(home).iter().filter(|a| a.d.kind == steer::VETO).filter_map(|a| apply(home, a).ok().flatten()).count()
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
/// `borg steer` takes it, of a local drone or one on another node (`main` then sends it as a signed
/// directive). Otherwise the local drone working in `repo`: the one not ended, else the most recently
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

/// The constraints file of a store. Written on ratification; nothing reads it yet (enforcement at
/// pre_tool and at the stop is a later hive step) and the mesh does not sync it.
pub fn constraints_path(store: &Path) -> PathBuf {
    store.join("constraints.jsonl")
}

/// The answer to a constraint question (queen.rs `answer`): an answer starting with "ratify" (option
/// b) appends the invariant to `constraints.jsonl` and returns true; anything else writes nothing.
///
/// # Errors
/// When the constraints file cannot be appended.
pub fn ratify(store: &Path, e: &queen::Entry, answer: &str, author: &str, now: i64) -> Result<bool, String> {
    if !answer.trim().to_lowercase().starts_with("ratify") {
        return Ok(false);
    }
    let field = |prefix: &str| e.evidence.iter().find_map(|x| x.strip_prefix(prefix)).unwrap_or_default().to_string();
    let line = json!({"id": format!("c{}", &e.decision_key[..8.min(e.decision_key.len())]), "text": field("invariant: "), "lesson": field("lesson "),
        "veto": field("veto: "), "by": author, "ms": now, "source": "veto", "check": ""});
    learn::append_jsonl(&constraints_path(store), &line).map_err(|err| err.to_string())?;
    Ok(true)
}

/// `borg no`'s flags (completions.rs).
pub const FLAGS: &[&str] = &["--session", "--dir", "--json"];

const USAGE: &str = "usage: borg no [--session ID|DRONE] [--dir DIR] [--json] <reason…>
  says no to a drone's work: the lessons it was shown lose credit, and the reason becomes a lesson";

/// A veto of a drone on another node: `borg steer <session> --veto <reason>` (the mesh binary signs it
/// with this node's member key; the drone's node records it, `apply`). Its output is this command's.
fn remote(session: &str, cwd: &str, reason: &str) -> i32 {
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("borg no: cannot find the borg binary to sign the veto with");
        return 1;
    };
    match std::process::Command::new(exe).args(["steer", session, "--veto", "--dir", cwd, reason]).status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("borg no: cannot sign the veto: {e}");
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
        return remote(session, &cwd, &reason);
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
    println!("to stop it now: borg steer {} --pause", short(session));
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

    #[test]
    fn shown_between_keeps_the_window_inclusive() {
        let home = temp("window");
        for (ms, l) in [(10, "A"), (20, "B"), (30, "C")] {
            let o = Outcome { kind: outcome::ASKED.into(), session: "s".into(), lessons: vec![l.into()], ms, repo: "r".into(), ..Default::default() };
            outcome::append(&home.join("outcomes.jsonl"), &o).unwrap();
        }
        let mut early = stats::Event::new(stats::EARLY, 25);
        early.session = "s".into();
        early.lessons = vec!["E".into()];
        stats::record(&home, &early);
        assert_eq!(shown_between(&home, "s", 20, 30), ["B", "C", "E"], "both bounds are in");
        assert_eq!(shown_between(&home, "s", 11, 19), Vec::<String>::new());
        assert_eq!(shown_between(&home, "s", i64::MIN, 10), ["A"]);
        assert_eq!(shown(&home, "s"), ["A", "B", "C", "E"]);
        assert!(shown_between(&home, "t", i64::MIN, i64::MAX).is_empty());
        let _ = std::fs::remove_dir_all(home);
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

    fn directive(id: &str, kind: &str, session: &str, text: &str, ms: i64) -> steer::Accepted {
        steer::Accepted {
            d: steer::Directive {
                id: id.into(),
                collective: "corp".into(),
                key: "ab".repeat(32),
                engineer: "lead@x".into(),
                target: steer::Target { node: "me".into(), session: session.into() },
                kind: kind.into(),
                text: text.into(),
                ms,
                sig: String::new(),
            },
            role: "steer".into(),
        }
    }

    #[test]
    fn a_signed_veto_from_another_node_is_recorded_once_where_its_drone_runs() {
        let home = temp("apply");
        asked(&home, "s1", &["A"]);
        let v = directive("v1", steer::VETO, "s1", "never read the commit log for merges", 100);
        let got = apply(&home, &v).unwrap().unwrap();
        assert_eq!((got.episode.as_str(), got.credited.clone(), got.missed), ("no-s1-100", vec!["A".to_string()], false));
        assert_eq!(apply(&home, &v).unwrap(), None, "the same directive is recorded once");
        // Through the inbox (the mesh's way in): already recorded, and a say or an untargeted veto never records.
        steer::accept_all(
            &home,
            &[v.clone(), directive("s9", steer::SAY, "s1", "run the tests", 110), directive("v2", steer::VETO, "", "no target", 120)],
            120,
        )
        .unwrap();
        assert_eq!(apply_inbox(&home), 0);
        let eps = std::fs::read_to_string(home.join("episodes.jsonl")).unwrap();
        assert_eq!(eps.lines().count(), 1);
        let e: learn::Episode = serde_json::from_str(eps.lines().next().unwrap()).unwrap();
        assert_eq!((e.kind.as_str(), e.engineer.as_str(), e.session.as_str()), (KIND, "lead@x", "s1"), "the signer's veto, of its session");
        let vetoes: Vec<stats::Event> = stats::events(&home).into_iter().filter(|e| e.kind == stats::VETO).collect();
        assert_eq!((vetoes.len(), vetoes[0].why.as_str()), (1, "mesh"));
        assert_eq!(outcome::read(&home.join("outcomes.jsonl")).iter().filter(|o| o.kind == outcome::VETOED).count(), 1);
        // A new one through the inbox is recorded there.
        steer::accept(&home, &directive("v3", steer::VETO, "s1", "and not the reflog either", 130), 130).unwrap();
        assert_eq!(apply_inbox(&home), 1);
        assert_eq!(apply_inbox(&home), 0);
        // A veto of a session this node never held, or naming no node, is not this node's to learn.
        assert_eq!(apply(&home, &directive("v4", steer::VETO, "zz", "not here", 140)).unwrap(), None);
        let mut nodeless = directive("v5", steer::VETO, "s1", "everywhere", 150);
        nodeless.d.target.node.clear();
        assert_eq!(apply(&home, &nodeless).unwrap(), None);
        assert_eq!(std::fs::read_to_string(home.join("episodes.jsonl")).unwrap().lines().count(), 2);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_veto_of_integrated_hive_work_never_escalated_is_a_missed_escalation() {
        use crate::hive::board::{self, Ev, Event, Role, Task};
        let home = temp("missed");
        let task = |id: &str, role: Role, of: Option<&str>| Task {
            id: id.into(),
            role,
            title: id.into(),
            repo: String::new(),
            files: vec![],
            deps: vec![],
            accept: vec![],
            of: of.map(String::from),
            not_by: vec![],
            flags: vec![],
            from: None,
            group: None,
        };
        let goal = crate::hive::dir(&home, "g");
        std::fs::create_dir_all(&goal).unwrap();
        std::fs::write(goal.join("goal.jsonl"), "").unwrap();
        let events = [
            Event::open(task("w", Role::Work, None), "engineer:a@x", 1),
            Event::new(Ev::Claim, "w", "s1", 2),
            Event::new(Ev::Done, "w", "s1", 3),
            Event::open(task("w.v1", Role::Verify, Some("w")), "s1", 3),
            Event::new(Ev::Claim, "w.v1", "s2", 4),
            Event::new(Ev::Verified, "w.v1", "s2", 5),
            Event::open(task("integrate.1", Role::Integrate, None), "s2", 5),
            Event::new(Ev::Claim, "integrate.1", "i", 6),
            Event::new(Ev::Verified, "integrate.1", "i", 7),
        ];
        board::append(&crate::hive::tasks_path(&home, "g"), &events).unwrap();
        let state = home.join("state");
        assert!(!escalation::merged(&home, &state, "s1", 6), "not integrated yet at 6");
        assert!(escalation::merged(&home, &state, "s1", 7));
        assert!(!escalation::merged(&home, &state, "s2", 7), "the verifier authored nothing");
        let v = record(&home, &said("s1", "this broke the public schema", 50)).unwrap();
        assert!(v.missed, "merged and never escalated");
        let during = record(&home, &said("s2", "never skip the migration", 50)).unwrap();
        assert!(!during.missed, "work that never merged: the veto caught it in time");
        let y = escalation::team(&home);
        assert_eq!((y.missed, y.reward), (1, -escalation::MISSED_W));
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
        let _ = std::fs::remove_dir_all(home);
    }
}
