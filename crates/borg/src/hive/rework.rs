//! Rework episodes: the hive's own lesson source (docs/design/hive.md L4, "rework episodes").
//!
//! A work task whose attempt the verifier rejected and whose later attempt was verified holds a
//! mistake a verifier caught and the fix that passed. That pair becomes one episode of kind [`KIND`]
//! (`rework-<goal>.<task>`, once per task): the task, every rejection's note (the verifier's why), each
//! rejected attempt's account and change, and what the accepted rework changed relative to the last
//! rejected attempt (redacted, cut to `regret::CAP`), plus the drones. `borg learn` generalises it with
//! its own opening ([`HEAD`], [`FIELDS`]) into an L0 lesson that would have prevented the mistake, or
//! merges it into the lesson that says it already.
//!
//! What counts, like group.rs's reward: an attempt's reward is its hidden score on this node, else
//! (the goal has a hidden oracle, here or held out on the node that made it, and the attempt is
//! scoreable) none yet, else the visible verdict. A node that does not hold a held-out goal's oracle
//! never has its scores (oracle.jsonl does not sync), so there its scoreable attempts always wait. The
//! accepted attempt must be the board's (`TaskState::accepted`), verified, not hacked, and not failed
//! by the hidden oracle. A rejection counts when the verifier rejected the work on its merits: not a
//! hacked verdict (a guardrail, not the verifier's judgment), not an unchecked one (no snapshot, a dep
//! missing: nothing about the work), and not one the hidden oracle passed (the verifier was wrong).
//! A task verified at its first attempt writes nothing; one whose attempts wait for a hidden score
//! waits. A fix task after a failed integration is a work task too: its own reject then pass counts.
//!
//! One learner per task, at every `borg learn` ([`record_pending`], under its lock): for a goal with a
//! hidden oracle the node that holds it (the only one with the scores), else the node the board's
//! verifying verdict was written on. Off with `BORG_REWORK_REFLECT=off` (an eval
//! arm without it), and with `BORG_LEARN=off`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::board::{Board, Ev, Event, Role, Status};
use super::group::{change, cut, repo_dir, scores};
use super::{Goal, TaskId, board, goal, verdict};
use crate::learn::{self, Episode};

/// `Episode::kind` of a rework.
pub const KIND: &str = "rework";

/// How the rework prompt opens (the regret prompt's neutral reach and merge rules follow it).
pub const HEAD: &str = "A coding agent's attempt at a task was rejected by an independent verifier, and a later attempt at \
the same task (a rework) was accepted. Decide whether the rejection reveals a reusable lesson: a mistake the rejected \
attempt made, which the verifier caught and the rework fixed, and which one question asked while doing such work would \
have prevented. A rejection for luck, flakiness, style or wording, or for something the rework did not change, is NOT a \
lesson.";

/// The fields a rework lesson is written in (the area line follows, shared with corrections).
pub const FIELDS: &str = "If it is a lesson, write:
- question: ONE question an agent should ask itself before it calls such work done, naming the mistake the verifier \
caught; general enough to transfer, keeping the technical substance;
- situations: 1-3 lines, each `<action> on/in <kind of state or component> [under <condition>]`, without project/class names;
- consequence: one line, what goes wrong without it, as the rejection shows;";

/// Whether rework episodes are recorded where `borg learn` runs (`BORG_REWORK_REFLECT=off` turns it off).
pub fn on() -> bool {
    std::env::var("BORG_REWORK_REFLECT").as_deref() != Ok("off")
}

/// The id of a task's rework episode: `rework-<goal>.<task>` (a goal id has no `.`).
pub fn episode_id(goal: &str, task: &str) -> String {
    format!("rework-{goal}.{task}")
}

/// One judged attempt of the task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Try {
    pub verify: TaskId,
    /// The drone it was an iteration of (else its session's first 8 characters).
    pub drone: String,
    pub author: String,
    pub ms: i64,
    /// The verifier's note, and the author's done note.
    pub note: String,
    pub done: String,
    pub base: String,
    pub head: String,
    /// Its reward came from the hidden oracle.
    pub hidden: bool,
}

/// A verified work task with rejections before its accepted attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rework {
    pub goal: String,
    pub task: TaskId,
    pub title: String,
    pub repo: String,
    /// The node the verifying verdict was written on: the learner unless the goal has a hidden oracle.
    pub node: String,
    /// The rejections counted, in order (with `waiting`, those still unscored too).
    pub rejected: Vec<Try>,
    pub passed: Try,
    /// Attempts (rejected or the accepted one) still waiting for their hidden score.
    pub waiting: usize,
}

impl Rework {
    /// Every counted attempt has its reward: nothing will change the episode.
    pub fn settled(&self) -> bool {
        self.waiting == 0
    }

    /// The drones that were rejected, each once, then the accepted one's.
    pub fn drones(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for t in self.rejected.iter().chain([&self.passed]) {
            if !v.contains(&t.drone) {
                v.push(t.drone.clone());
            }
        }
        v
    }
}

/// A rejection that says nothing about the work: no snapshot to check, or a dep's work missing.
fn unchecked(note: &str) -> bool {
    note == verdict::UNCHECKED || note.starts_with(verdict::NO_DEPS)
}

/// The reworked tasks of goal `g` on the board folded from `events`, with this node's hidden `scores`.
pub fn of_board(g: &Goal, events: &[Event], b: &Board, scores: &HashMap<String, bool>) -> Vec<Rework> {
    let judged = verdict::judged_events(events, b);
    let done_note = |a: &verdict::Attempt| {
        events.iter().find(|e| e.ev == Ev::Done && e.task == a.work && e.by == a.author && e.ms == a.done_ms).map(|e| e.note.clone()).unwrap_or_default()
    };
    let mut out = Vec::new();
    // Only a task with a rework can have a rejection before its accepted attempt.
    for s in b.tasks.values().filter(|s| s.task.role == Role::Work && s.rework > 0 && matches!(s.status, Status::Verified { .. })) {
        let Some(accepted) = &s.accepted else { continue };
        let mut tries: Vec<&(&Event, verdict::Attempt)> = judged.iter().filter(|(_, a)| a.work == s.task.id).collect();
        tries.sort_by_key(|(e, _)| (e.ms, e.task.clone()));
        tries.dedup_by(|x, y| x.0.task == y.0.task);
        // Its reward: the hidden score here, none yet when the hidden oracle (here or on the goal's
        // node) scores it, else the verdict.
        let reward = |e: &Event, a: &verdict::Attempt| -> (Option<bool>, bool) {
            let hacked = e.hacked == Some(true);
            if let Some(pass) = scores.get(&e.task) {
                return (Some(*pass), true);
            }
            let scoreable = || !hacked && !a.base.is_empty() && !a.head.is_empty() && verdict::missing_deps(events, b, a).is_empty();
            if goal::has_hidden(g) && scoreable() { (None, true) } else { (Some(e.ev == Ev::Verified && !hacked), false) }
        };
        let to_try = |e: &Event, a: &verdict::Attempt, hidden: bool| Try {
            verify: e.task.clone(),
            drone: if a.drone.is_empty() { a.author.chars().take(8).collect() } else { a.drone.clone() },
            author: a.author.clone(),
            ms: e.ms,
            note: e.note.clone(),
            done: done_note(a),
            base: a.base.clone(),
            head: a.head.clone(),
            hidden,
        };
        let Some(at) = tries.iter().position(|(e, _)| &e.task == accepted && e.ev == Ev::Verified && e.hacked != Some(true)) else { continue };
        let (pe, pa) = tries[at];
        let (pass_win, pass_hidden) = reward(pe, pa);
        if pass_win == Some(false) {
            // The held-out tests fail the accepted attempt: no rework that worked.
            continue;
        }
        let mut waiting = usize::from(pass_win.is_none());
        let mut rejected = Vec::new();
        for (e, a) in &tries[..at] {
            if e.ev != Ev::Rejected || e.hacked == Some(true) || unchecked(&e.note) {
                continue;
            }
            let (win, hidden) = reward(e, a);
            match win {
                // The hidden oracle passed it: the verifier was wrong, no mistake to learn.
                Some(true) => continue,
                None => waiting += 1,
                Some(false) => {}
            }
            rejected.push(to_try(e, a, hidden));
        }
        if rejected.is_empty() {
            continue;
        }
        out.push(Rework {
            goal: g.id.clone(),
            task: s.task.id.clone(),
            title: s.task.title.clone(),
            repo: s.task.repo.clone(),
            node: pe.node.clone(),
            rejected,
            passed: to_try(pe, pa, pass_hidden),
            waiting,
        });
    }
    out
}

/// The rework episode: what the prompt shows, redacted (notes.rs `redact`).
pub fn episode(r: &Rework, g: &Goal, joins: &[super::Join]) -> Episode {
    let diff = |author: &str, base: &str, head: &str| repo_dir(joins, author).map(|d| change(&d, base, head)).unwrap_or_default();
    let mut agent = String::new();
    for (n, t) in r.rejected.iter().enumerate() {
        agent.push_str(&format!("### REJECTED attempt {} (drone {}, {})\n", n + 1, t.drone, t.verify));
        if !t.done.trim().is_empty() {
            agent.push_str(&format!("The agent's account: {}\n", cut(t.done.trim())));
        }
        agent.push_str(&format!("The verifier's note: {}\n", cut(t.note.trim())));
        let d = diff(&t.author, &t.base, &t.head);
        if !d.trim().is_empty() {
            agent.push_str(&format!("Its change (excerpt):\n{}\n", cut(&d)));
        }
        agent.push('\n');
    }
    let p = &r.passed;
    let how = if p.hidden { "the verifier passed it and the held-out tests pass" } else { "the verifier passed it" };
    agent.push_str(&format!("### ACCEPTED rework (drone {}, {}): {how}\n", p.drone, p.verify));
    if !p.done.trim().is_empty() {
        agent.push_str(&format!("The agent's account: {}\n", cut(p.done.trim())));
    }
    // What the rework changed relative to the last rejected attempt: the fix of what was caught.
    let last = r.rejected.last().map(|t| t.head.as_str()).unwrap_or_default();
    let d = diff(&p.author, last, &p.head);
    if !d.trim().is_empty() {
        agent.push_str(&format!("What it changed relative to the rejected attempt (excerpt):\n{}\n", cut(&d)));
    }
    let notes: Vec<String> = r.rejected.iter().enumerate().map(|(n, t)| format!("{}. {}", n + 1, cut(t.note.trim()))).collect();
    let id = episode_id(&r.goal, &r.task);
    Episode {
        id: id.clone(),
        ms: p.ms,
        repo: if r.repo.is_empty() { g.repos.first().map(|x| x.name.clone()).unwrap_or_default() } else { r.repo.clone() },
        session: id,
        task: crate::notes::redact(&format!("{}\n(goal: {})", r.title, g.spec.lines().next().unwrap_or_default())),
        agent: crate::notes::redact(agent.trim_end()),
        correction: crate::notes::redact(&format!(
            "rework {}/{}: rejected {} time(s), then verified; drones: {}\nThe verifier's why:\n{}",
            r.goal,
            r.task,
            r.rejected.len(),
            r.drones().join(", "),
            notes.join("\n")
        )),
        kind: KIND.into(),
        ..Episode::default()
    }
}

fn episode_ids(home: &Path) -> HashSet<String> {
    super::read_lines::<Episode>(&home.join("episodes.jsonl")).into_iter().map(|e| e.id).collect()
}

/// Whether this node learns rework `r` of goal `g`: the node holding a hidden oracle (its scores decide),
/// else the node the verifying verdict was written on.
fn learns(home: &Path, g: &Goal, r: &Rework) -> bool {
    if goal::has_hidden(g) { g.hidden.is_some() } else { super::is_local(home, &r.node) }
}

/// Records the episode of every settled rework this node learns ([`learns`]) that has none yet; what
/// `borg learn` runs first (learn.rs), under its lock. Returns the ids written.
pub fn record_pending(home: &Path) -> Vec<String> {
    if std::fs::metadata(home.join("hive")).is_err() {
        return Vec::new();
    }
    let mut known: Option<HashSet<String>> = None;
    let mut out = Vec::new();
    for id in goal::list(home) {
        let Ok(g) = goal::load(home, &id) else { continue };
        if goal::has_hidden(&g) && g.hidden.is_none() {
            // A held-out goal: the node holding its oracle learns it.
            continue;
        }
        let events = board::read(&super::tasks_path(home, &id));
        let b = board::fold(&events);
        if !b.tasks.values().any(|s| s.rework > 0) {
            continue;
        }
        let joins = std::cell::OnceCell::new();
        for r in of_board(&g, &events, &b, &scores(home, &id)).iter().filter(|r| r.settled() && learns(home, &g, r)) {
            let eid = episode_id(&r.goal, &r.task);
            let known = known.get_or_insert_with(|| episode_ids(home));
            if known.contains(&eid) {
                continue;
            }
            let e = episode(r, &g, joins.get_or_init(|| super::read_lines::<super::Join>(&super::joins_path(home, &id))));
            if learn::append_jsonl(&home.join("episodes.jsonl"), &e).is_ok() {
                known.insert(eid.clone());
                out.push(eid);
            }
        }
    }
    out
}

/// A rework as borg-web and `borg top` show it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Row {
    pub task: String,
    pub rejections: usize,
    /// The rejected drones, then the accepted one's.
    pub drones: Vec<String>,
    /// `lesson <id>`, `not a lesson`, `learning` (its episode waits for `borg learn`), `to reflect`
    /// (the next `borg learn` on its node records it), or `waiting` (for a hidden score).
    pub reflection: String,
}

/// The rows of goal `g`'s reworked tasks on `b` (`events` folded).
pub fn rows(home: &Path, g: &Goal, events: &[Event], b: &Board) -> Vec<Row> {
    if !b.tasks.values().any(|s| s.rework > 0) {
        return Vec::new();
    }
    let all = of_board(g, events, b, &scores(home, &g.id));
    if all.is_empty() {
        return Vec::new();
    }
    let episodes = episode_ids(home);
    let lessons = learn::lessons(&home.join("lessons.jsonl"));
    let skipped = std::fs::read_to_string(home.join("episodes-not-lessons.txt")).unwrap_or_default();
    all.into_iter()
        .map(|r| {
            let id = episode_id(&r.goal, &r.task);
            let reflection = match lessons.iter().find(|l| l.episodes.contains(&id)) {
                Some(l) => format!("lesson {}", l.id),
                None if skipped.lines().any(|l| l == id) => "not a lesson".into(),
                None if episodes.contains(&id) => "learning".into(),
                None if r.settled() => "to reflect".into(),
                None => "waiting".into(),
            };
            Row { drones: r.drones(), task: r.task, rejections: r.rejected.len(), reflection }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::board::Task;
    use crate::hive::goal::Check;
    use std::path::PathBuf;

    fn task(id: &str) -> Task {
        Task {
            id: id.into(),
            role: Role::Work,
            title: format!("do {id}"),
            repo: String::new(),
            files: vec!["x.txt".into()],
            deps: vec![],
            accept: vec![],
            of: None,
            not_by: vec![],
            flags: vec![],
            from: None,
            group: None,
        }
    }

    fn goal(hidden: bool) -> Goal {
        Goal {
            v: 1,
            id: "g".into(),
            spec: "the spec".into(),
            oracle: vec![Check { name: "ok".into(), cmd: "true".into(), repo: String::new(), timeout_s: 60 }],
            budget_usd: 1.0,
            repos: vec![],
            author: String::new(),
            created_ms: 0,
            sig: String::new(),
            hidden: hidden.then(|| goal::Hidden { dir: "/x".into(), commands: vec!["true".into()], timeout_s: 60 }),
            held_out: false,
        }
    }

    enum V {
        Pass,
        Reject(&'static str),
        Hacked,
    }

    /// Attempt number `n` (0-based rework count) of `id` by drone `d` at `ms`, judged by `s-ver` at ms+3.
    fn attempt(id: &str, n: u32, d: &str, ms: i64, v: V) -> Vec<Event> {
        let s = format!("s-{d}");
        let vt = crate::hive::verify_task(&task(id), n, &s);
        let verdict = match v {
            V::Pass => Event { credit: "verified".into(), ..Event::new(Ev::Verified, &vt.id, "s-ver", ms + 3) },
            V::Reject(why) => Event { credit: "rejected".into(), ..Event::new(Ev::Rejected, &vt.id, "s-ver", ms + 3).with_note(why) },
            V::Hacked => {
                Event { hacked: Some(true), credit: "hacked".into(), ..Event::new(Ev::Rejected, &vt.id, "s-ver", ms + 3).with_note("protected files changed") }
            }
        };
        vec![
            Event { base: format!("b{n}"), ..Event::new(Ev::Claim, id, &s, ms).with_drone(d) },
            Event { head: format!("h{n}"), ..Event::new(Ev::Done, id, &s, ms + 1).with_note(&format!("{d} wrote x.txt ({n})")) },
            Event::open(vt.clone(), &s, ms + 1),
            Event::new(Ev::Claim, &vt.id, "s-ver", ms + 2).with_drone("dv"),
            verdict,
        ]
    }

    fn board_of(events: &[Event], hidden: bool, scores: &HashMap<String, bool>) -> Vec<Rework> {
        of_board(&goal(hidden), events, &board::fold(events), scores)
    }

    fn store(name: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!("borg-rework-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        goal::create(&home, &goal(false)).unwrap();
        home
    }

    #[test]
    fn only_a_rejection_followed_by_a_verified_attempt_is_a_rework() {
        let none = HashMap::new();
        // Verified at the first attempt: nothing.
        let mut ev = vec![Event::open(task("t"), "engineer:e", 1)];
        ev.extend(attempt("t", 0, "d1", 10, V::Pass));
        assert!(board_of(&ev, false, &none).is_empty());
        // Rejected, not reworked yet: nothing.
        let mut ev = vec![Event::open(task("t"), "engineer:e", 1)];
        ev.extend(attempt("t", 0, "d1", 10, V::Reject("misses the empty input")));
        assert!(board_of(&ev, false, &none).is_empty());
        // Its rework verified: one rework, the verifier's why kept.
        ev.extend(attempt("t", 1, "d2", 20, V::Pass));
        let r = board_of(&ev, false, &none);
        assert_eq!(r.len(), 1);
        assert!(r[0].settled() && r[0].rejected.len() == 1 && r[0].rejected[0].note == "misses the empty input");
        assert_eq!((r[0].passed.verify.as_str(), r[0].drones()), ("t.v2", vec!["d1".to_string(), "d2".into()]));
    }

    #[test]
    fn a_hacked_or_unchecked_rejection_is_no_rework_and_several_rejections_make_one() {
        let none = HashMap::new();
        // Hacked, then verified: the guardrail caught it, not the verifier's judgment.
        let mut ev = vec![Event::open(task("t"), "engineer:e", 1)];
        ev.extend(attempt("t", 0, "d1", 10, V::Hacked));
        ev.extend(attempt("t", 1, "d1", 20, V::Pass));
        assert!(board_of(&ev, false, &none).is_empty());
        // Unchecked, then verified: nothing about the work.
        let mut ev = vec![Event::open(task("u"), "engineer:e", 1)];
        ev.extend(attempt("u", 0, "d1", 10, V::Reject(verdict::UNCHECKED)));
        ev.extend(attempt("u", 1, "d1", 20, V::Pass));
        assert!(board_of(&ev, false, &none).is_empty());
        // Two rejections and a hack before the pass: one rework with both notes, the hack left out.
        let mut ev = vec![Event::open(task("w"), "engineer:e", 1)];
        ev.extend(attempt("w", 0, "d1", 10, V::Reject("no test for the edge")));
        ev.extend(attempt("w", 1, "d1", 20, V::Hacked));
        ev.extend(attempt("w", 2, "d2", 30, V::Reject("still panics on empty")));
        ev.extend(attempt("w", 3, "d2", 40, V::Pass));
        let r = board_of(&ev, false, &none);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].rejected.iter().map(|t| t.note.as_str()).collect::<Vec<_>>(), ["no test for the edge", "still panics on empty"]);
        let e = episode(&r[0], &goal(false), &[]);
        assert_eq!((e.id.as_str(), e.kind.as_str(), e.ms), ("rework-g.w", KIND, 43));
        assert!(
            e.correction.contains("rejected 2 time(s)") && e.correction.contains("1. no test for the edge") && e.correction.contains("2. still panics"),
            "{}",
            e.correction
        );
        let (rej, acc) = (e.agent.find("### REJECTED attempt 1").unwrap(), e.agent.find("### ACCEPTED rework (drone d2, w.v4)").unwrap());
        assert!(rej < acc && e.agent.contains("d1 wrote x.txt (0)") && e.task.contains("do w"), "{}", e.agent);
    }

    #[test]
    fn the_hidden_score_decides_like_a_groups_reward() {
        let mut ev = vec![Event::open(task("t"), "engineer:e", 1)];
        ev.extend(attempt("t", 0, "d1", 10, V::Reject("wrong rounding")));
        ev.extend(attempt("t", 1, "d1", 20, V::Pass));
        // Unscored here: waiting, never recorded yet.
        let r = board_of(&ev, true, &HashMap::new());
        assert!(r.len() == 1 && !r[0].settled() && r[0].waiting == 2);
        let s = |a: bool, b: bool| -> HashMap<String, bool> { [("t.v1".to_string(), a), ("t.v2".to_string(), b)].into_iter().collect() };
        // The held-out tests fail the rejected attempt and pass the rework: a rework.
        assert!(board_of(&ev, true, &s(false, true)).first().is_some_and(|r| r.settled() && r.passed.hidden));
        // They pass the rejected one: the verifier was wrong. They fail the rework: nothing worked.
        assert!(board_of(&ev, true, &s(true, true)).is_empty());
        assert!(board_of(&ev, true, &s(false, false)).is_empty());
    }

    #[test]
    fn a_rework_is_recorded_once_by_the_node_that_verified_it() {
        let home = store("once");
        let mut ev = vec![Event::open(task("t"), "engineer:e", 1)];
        ev.extend(attempt("t", 0, "d1", 10, V::Reject("misses the empty input")));
        ev.extend(attempt("t", 1, "d1", 20, V::Pass));
        board::append(&super::super::tasks_path(&home, "g"), &ev).unwrap();
        assert_eq!(record_pending(&home), ["rework-g.t"]);
        assert!(record_pending(&home).is_empty(), "once per task");
        assert_eq!(super::super::read_lines::<Episode>(&home.join("episodes.jsonl")).len(), 1);
        let g = goal(false);
        let rows = rows(&home, &g, &ev, &board::fold(&ev));
        assert_eq!((rows[0].task.as_str(), rows[0].rejections, rows[0].reflection.as_str()), ("t", 1, "learning"));
        // A task verified on another node is that node's to learn.
        let mut ev = vec![Event::open(task("u"), "engineer:e", 1)];
        ev.extend(attempt("u", 0, "d1", 100, V::Reject("misses the empty input")));
        let mut pass = attempt("u", 1, "d1", 200, V::Pass);
        pass.last_mut().unwrap().node = "another-node".into();
        ev.extend(pass);
        board::append(&super::super::tasks_path(&home, "g"), &ev).unwrap();
        assert!(record_pending(&home).is_empty(), "one learner: the verifying verdict's node");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_held_out_goal_is_learned_by_the_node_holding_its_oracle() {
        let mut ev = vec![Event::open(task("t"), "engineer:e", 1)];
        ev.extend(attempt("t", 0, "d1", 10, V::Reject("wrong rounding")));
        let mut pass = attempt("t", 1, "d1", 20, V::Pass);
        pass.last_mut().unwrap().node = "another-node".into();
        ev.extend(pass);
        // A node the goal synced to (held out, no oracle, no scores): its attempts wait, never recorded.
        let synced = Goal { held_out: true, ..goal(false) };
        let r = of_board(&synced, &ev, &board::fold(&ev), &HashMap::new());
        assert!(r.len() == 1 && !r[0].settled() && r[0].waiting == 2);
        let home = store("held-out");
        // As a peer holds it: only the synced projection, no goal.json.
        std::fs::remove_file(super::super::goal_path(&home, "g")).unwrap();
        std::fs::write(super::super::goal_jsonl_path(&home, "g"), goal::projection(&goal(true)) + "\n").unwrap();
        assert!(goal::load(&home, "g").is_ok_and(|g| g.held_out && g.hidden.is_none()));
        board::append(&super::super::tasks_path(&home, "g"), &ev).unwrap();
        assert!(record_pending(&home).is_empty(), "the verdict's node does not hold the scores");
        let _ = std::fs::remove_dir_all(&home);
        // The goal's node holds the oracle and the scores: it learns the rework verified elsewhere.
        let home = store("held-out-own");
        std::fs::write(super::super::goal_path(&home, "g"), serde_json::to_string(&goal(true)).unwrap()).unwrap();
        board::append(&super::super::tasks_path(&home, "g"), &ev).unwrap();
        assert!(record_pending(&home).is_empty(), "unscored: waiting");
        let scores = crate::hive::hidden::scores_path(&home, "g");
        for (task, pass) in [("t.v1", false), ("t.v2", true)] {
            let s = crate::hive::hidden::Score {
                v: 1,
                task: task.into(),
                head: String::new(),
                pass,
                timeout: false,
                dur_ms: 0,
                ms: 0,
                agree: true,
                tail: String::new(),
            };
            super::super::append_line(&scores, &s).unwrap();
        }
        assert_eq!(record_pending(&home), ["rework-g.t"]);
        let _ = std::fs::remove_dir_all(home);
    }
}
