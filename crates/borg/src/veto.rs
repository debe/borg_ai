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
//! In the session the drone is told to stop the vetoed approach and not argue it back. A veto prompt is
//! never the session's task and never doubles as a correction. BORG_VETO=off makes such a prompt an
//! ordinary one (the command still works).

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::json;

use crate::outcome::{self, Outcome};
use crate::presence::State;
use crate::{collective, learn, presence, queen, stats, top};

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
    /// `cli` or `prompt`.
    pub via: &'static str,
}

/// What `record` wrote: the episode's id and the lessons credited `vetoed` now.
#[derive(Debug, Clone, PartialEq)]
pub struct Veto {
    pub episode: String,
    pub credited: Vec<String>,
}

/// Records one veto in `home`: the episode, a `vetoed` outcome per shown lesson not yet credited in
/// the session (one per lesson keeps `outcome::has` idempotent; nothing when none, so a session with
/// nothing shown adds no clock tick), and one `veto` stat for every call. The caller checks
/// `learn::learning()` first.
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
    Ok(Veto { episode: e.id, credited })
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
/// `borg steer` takes it, of a local drone. Otherwise the local drone working in `repo`: the one not
/// ended, else the most recently seen ended one (a veto right after the session ended); several active
/// ones are an error, never a guess. A remote drone is never picked.
///
/// # Errors
/// What to name instead.
pub fn target<'a>(drones: &'a [top::Drone], repo: &str, named: Option<&str>) -> Result<&'a top::Drone, String> {
    if let Some(n) = named {
        return match top::resolve(drones, n)? {
            None => Err("a veto names one drone, not all".into()),
            Some(d) if !d.node.is_empty() => Err(format!("{} runs on another machine: a veto reaches local drones only", d.designation)),
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
    let state = std::env::var_os("BORG_STATE").map_or_else(|| home.join("state"), PathBuf::from);
    let task = std::fs::read_to_string(state.join(format!("{}.task", presence::safe(session))))
        .ok()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| d.p.prompt.clone());
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
        // A remote drone is never picked, not even by name.
        let remote = vec![drone("dddd4444", "app", State::Working, 10, "beef", 1)];
        assert!(target(&remote, "app", None).is_err());
        assert!(target(&remote, "app", Some("dddd")).unwrap_err().contains("another machine"));
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
        assert_eq!(v, Veto { episode: "no-s1-100".into(), credited: vec!["A".into(), "B".into()] });
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
