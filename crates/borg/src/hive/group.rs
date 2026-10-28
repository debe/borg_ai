//! Groups: one task attempted K times by different drones, the hive's group of rollouts
//! (docs/design/hive.md "Group-relative advantage", L4; arm F's best-of-N).
//!
//! `borg hive open <goal> --id T --group K` opens K work tasks `T.g1`..`T.gK`, each a copy of T with
//! `group: T` (board.rs `Task::group`). Each is an ordinary task (claim, done, verify, rework), with
//! one bar more: a session or drone that held one member, or verified one, may not claim another, and
//! one that held a member may not verify another (board.rs `Board::rival`), so the K attempts are K
//! drones' and independent, and no attempt is judged by a rival (`borg debug hive run` needs K+1 drones). Every member is verified like any work; an
//! integration merges only the group's best accepted attempt ([`best`]): the one the hidden oracle
//! passed, else the one with the fewest rejections, then the earliest verified (integrate.rs scores
//! the group's attempts first when the goal has a hidden oracle, hidden.rs).
//!
//! Reflection, Training-Free GRPO style: once every member is verified and every judged attempt has
//! its reward ([`Group::settled`]), the attempts are split into winners and losers: by the hidden
//! oracle's score where this node has one, else by the visible verdict (verified and not hacked
//! wins). A group with a winner and a loser ([`Group::spread`]) becomes one episode of kind [`KIND`]
//! (`group-<goal>.<task>`: the provenance a lesson made from it keeps in `episodes`): the task, the
//! result, and each attempt's drone, reward, done note, verifier's note and change (redacted, cut to
//! `regret::CAP`), winners first. `borg learn` generalises it with its own opening ([`HEAD`],
//! [`FIELDS`]) into a semantic-advantage lesson (what the winners did that the losers did not), or
//! merges it into the lesson that says it already. A group of one, or one whose attempts all won or
//! all lost, writes nothing.
//!
//! `borg learn --group <goal>/<task>` reflects on one group ([`reflect`]); every `borg learn` (the
//! session end's included) first records each settled group opened on this node that has no episode
//! yet ([`record_pending`]): one learner per group, the node its open was written on. Once per group:
//! the episode's id is fixed, and a group with an episode is never recorded again. Off with
//! `BORG_GROUP_REFLECT=off` (an eval arm without it), and with `BORG_LEARN=off`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::board::{Board, Ev, Event, Role, Status, TaskState};
use super::hidden::Score;
use super::{Goal, TaskId, board, goal, verdict};
use crate::learn::{self, Episode, regret};
use crate::runner;

/// `Episode::kind` of a group's reflection.
pub const KIND: &str = "group";

const GIT_TIMEOUT: Duration = Duration::from_secs(60);

/// How the reflection prompt opens (the regret prompt's neutral reach and merge rules follow it).
pub const HEAD: &str = "Several coding agents attempted the same task independently, each in its own worktree. A reward \
checked every attempt: held-out tests where it says so, else an independent verifier's verdict. Winners passed, losers did \
not. Compare them and decide whether the difference reveals a reusable lesson: something the winners did or weighed that \
the losers did not, which explains the reward. A difference of luck, style or wording that does not explain the reward is \
NOT a lesson.";

/// The fields a group lesson is written in (the area line follows, shared with corrections).
pub const FIELDS: &str = "If it is a lesson, write:
- question: ONE question an agent should ask itself while doing such work, naming what the winners did that the losers \
did not; general enough to transfer, keeping the technical substance;
- situations: 1-3 lines, each `<action> on/in <kind of state or component> [under <condition>]`, without project/class names;
- consequence: one line, what goes wrong without it, as the losers show;";

/// Whether reflection runs where `borg learn` runs (`BORG_GROUP_REFLECT=off` turns it off; an
/// explicit `borg learn --group` still reflects).
pub fn on() -> bool {
    std::env::var("BORG_GROUP_REFLECT").as_deref() != Ok("off")
}

/// Member `n` (1-based) of group `group`.
pub fn member_id(group: &str, n: usize) -> TaskId {
    format!("{group}.g{n}")
}

/// The id of a group's reflection episode, the provenance of what is learned from it: `group-<goal>.<task>`.
/// A goal id has no `.` (goal.rs `valid_id`), so two groups of different goals never share it.
pub fn episode_id(goal: &str, group: &str) -> String {
    format!("group-{goal}.{group}")
}

/// The members of each group on `b`, by group id, members in id order.
pub fn groups(b: &Board) -> BTreeMap<&str, Vec<&TaskState>> {
    let mut out: BTreeMap<&str, Vec<&TaskState>> = BTreeMap::new();
    for s in b.tasks.values().filter(|s| s.task.role == Role::Work) {
        if let Some(g) = &s.task.group {
            out.entry(g.as_str()).or_default().push(s);
        }
    }
    out
}

/// Each judged attempt's hidden score on this node, by verify task (oracle.jsonl, one per attempt).
pub fn scores(home: &Path, goal: &str) -> HashMap<String, bool> {
    super::read_lines::<Score>(&super::hidden::scores_path(home, goal)).into_iter().map(|s| (s.task, s.pass)).collect()
}

/// The member of a group an integration merges, among its accepted ones: the one whose accepted attempt
/// the hidden oracle passed (`passed` by verify task; one not scored next, a failed one last), then the
/// fewest rejections, then the earliest verified, then by id. The same on every node with the same scores.
pub fn best<'a>(members: &[&'a TaskState], passed: impl Fn(&str) -> Option<bool>) -> Option<&'a TaskState> {
    let verified = |s: &TaskState| if let Status::Verified { ms, .. } = s.status { ms } else { i64::MAX };
    members.iter().copied().filter(|s| s.accepted.is_some()).min_by_key(|s| {
        let score = match s.accepted.as_deref().and_then(&passed) {
            Some(true) => 0,
            None => 1,
            Some(false) => 2,
        };
        (score, s.rework, verified(s), s.task.id.clone())
    })
}

/// The accepted work an integration merges: every accepted work task outside a group, and of each group
/// its [`best`].
pub fn merged(b: &Board, passed: impl Fn(&str) -> Option<bool>) -> Vec<&TaskState> {
    let chosen: HashSet<&str> = groups(b).values().filter_map(|m| best(m, &passed)).map(|s| s.task.id.as_str()).collect();
    b.tasks
        .values()
        .filter(|s| s.task.role != Role::Integrate && s.accepted.is_some())
        .filter(|s| s.task.group.is_none() || chosen.contains(s.task.id.as_str()))
        .collect()
}

/// Whether a group on `b` has more than one accepted member: picking its best wants their scores.
pub fn contested(b: &Board) -> bool {
    groups(b).values().any(|m| m.iter().filter(|s| s.accepted.is_some()).count() > 1)
}

/// One judged attempt of a group's member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Try {
    /// The verify task that judged it, and the member it was an attempt of.
    pub verify: TaskId,
    pub work: TaskId,
    /// The drone it was an iteration of (else its session's first 8 characters).
    pub drone: String,
    /// When it was judged.
    pub ms: i64,
    /// Its reward: won, lost, or None while the hidden oracle has not scored it here.
    pub win: Option<bool>,
    /// The reward is the hidden oracle's score (else the visible verdict's).
    pub hidden: bool,
    /// The verdict (`verified`, `rejected` or `hacked`) and the verifier's note.
    pub verdict: String,
    pub note: String,
    /// The author's done note.
    pub done: String,
    pub author: String,
    pub base: String,
    pub head: String,
}

/// A group of a goal's board and its attempts so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub goal: String,
    pub id: TaskId,
    pub title: String,
    pub repo: String,
    /// Its members' ids, and the opened-on node of the first.
    pub members: Vec<TaskId>,
    pub node: String,
    /// Every member is verified.
    pub verified: bool,
    pub tries: Vec<Try>,
}

impl Group {
    /// Every member is verified and every attempt has its reward: nothing will change it.
    pub fn settled(&self) -> bool {
        self.members.len() > 1 && self.verified && self.tries.iter().all(|t| t.win.is_some())
    }

    /// It has a winner and a loser: there is something to compare.
    pub fn spread(&self) -> bool {
        self.tries.iter().any(|t| t.win == Some(true)) && self.tries.iter().any(|t| t.win == Some(false))
    }

    /// The drones that won, and those that lost, each once, in attempt order.
    pub fn sides(&self) -> (Vec<String>, Vec<String>) {
        let side = |w: bool| {
            let mut v: Vec<String> = Vec::new();
            for t in self.tries.iter().filter(|t| t.win == Some(w)) {
                if !v.contains(&t.drone) {
                    v.push(t.drone.clone());
                }
            }
            v
        };
        (side(true), side(false))
    }

    /// What decided the rewards: the hidden oracle, the verdicts, or both (some attempts unscored).
    pub fn decided_by(&self) -> &'static str {
        match (self.tries.iter().any(|t| t.hidden), self.tries.iter().all(|t| t.hidden)) {
            (true, true) => "the hidden oracle",
            (true, false) => "the hidden oracle, else the verdict",
            _ => "the verdicts",
        }
    }
}

/// The groups of goal `g` on the board folded from `events`, with each attempt's reward: its hidden
/// score in `scores` (by verify task); else, when this node scores `g` (`hidden` here) and the attempt
/// is one it scores (not hacked, both snapshots, its deps' work in its tree: hidden.rs `score_pending`),
/// None until it is scored; else the visible verdict.
pub fn of_board(g: &Goal, events: &[Event], b: &Board, scores: &HashMap<String, bool>) -> Vec<Group> {
    let judged = verdict::judged_events(events, b);
    let done_note = |a: &verdict::Attempt| {
        events.iter().find(|e| e.ev == Ev::Done && e.task == a.work && e.by == a.author && e.ms == a.done_ms).map(|e| e.note.clone()).unwrap_or_default()
    };
    groups(b)
        .into_iter()
        .map(|(id, members)| {
            let names: Vec<TaskId> = members.iter().map(|s| s.task.id.clone()).collect();
            let mut tries: Vec<Try> = judged
                .iter()
                .filter(|(_, a)| names.contains(&a.work))
                .map(|(e, a)| {
                    let hacked = e.hacked == Some(true);
                    let scoreable = !hacked && !a.base.is_empty() && !a.head.is_empty() && verdict::missing_deps(events, b, a).is_empty();
                    let (win, hidden) = match scores.get(&e.task) {
                        Some(pass) => (Some(*pass), true),
                        None if g.hidden.is_some() && scoreable => (None, true),
                        None => (Some(e.ev == Ev::Verified && !hacked), false),
                    };
                    let verdict = if hacked { "hacked" } else { e.ev.name() };
                    Try {
                        verify: e.task.clone(),
                        work: a.work.clone(),
                        drone: if a.drone.is_empty() { a.author.chars().take(8).collect() } else { a.drone.clone() },
                        ms: e.ms,
                        win,
                        hidden,
                        verdict: verdict.into(),
                        note: e.note.clone(),
                        done: done_note(a),
                        author: a.author.clone(),
                        base: a.base.clone(),
                        head: a.head.clone(),
                    }
                })
                .collect();
            tries.sort_by_key(|t| (t.ms, t.verify.clone()));
            tries.dedup_by(|a, b| a.verify == b.verify);
            let first = members[0];
            Group {
                goal: g.id.clone(),
                id: id.to_string(),
                title: first.task.title.clone(),
                repo: first.task.repo.clone(),
                members: names,
                node: first.node.clone(),
                verified: members.iter().all(|s| matches!(s.status, Status::Verified { .. })),
                tries,
            }
        })
        .collect()
}

/// The groups of goal `id` in the store `home`, with this node's hidden scores.
pub fn read(home: &Path, id: &str) -> Result<(Goal, Vec<Group>), String> {
    let g = goal::load(home, id)?;
    let events = board::read(&super::tasks_path(home, id));
    let b = board::fold(&events);
    let groups = of_board(&g, &events, &b, &scores(home, id));
    Ok((g, groups))
}

/// A directory of the goal's repo to diff snapshots in: the author's worktree (joins.jsonl), else any
/// join of the goal whose directory still exists. Snapshots are commits of the shared object store, so
/// any worktree of the repo holds them.
pub(crate) fn repo_dir(joins: &[super::Join], author: &str) -> Option<PathBuf> {
    let exists = |j: &&super::Join| !j.dir.is_empty() && Path::new(&j.dir).is_dir();
    joins.iter().rev().filter(exists).find(|j| j.session == author).or_else(|| joins.iter().rev().find(exists)).map(|j| PathBuf::from(&j.dir))
}

/// The change of an attempt, `base` to `head`: its stat and patch, empty when git cannot say (the
/// snapshots are on another node, the worktrees gone).
pub(crate) fn change(dir: &Path, base: &str, head: &str) -> String {
    if base.is_empty() || head.is_empty() {
        return String::new();
    }
    let mut cmd = super::git::command(dir);
    cmd.args(["diff", "--stat", "--patch", "-U2", "--no-color", "--no-ext-diff", "--no-textconv", base, head]);
    match runner::run_capped_status(cmd, None, GIT_TIMEOUT) {
        Some((s, out)) if s.success() => String::from_utf8_lossy(&out).into_owned(),
        _ => String::new(),
    }
}

pub(crate) fn cut(s: &str) -> String {
    s.chars().take(regret::CAP).collect()
}

/// The reflection episode of a group with a spread: what the prompt compares, redacted (notes.rs
/// `redact`), winners first.
pub fn episode(gr: &Group, g: &Goal, joins: &[super::Join]) -> Episode {
    let mut tries: Vec<&Try> = gr.tries.iter().collect();
    tries.sort_by_key(|t| (t.win != Some(true), t.ms));
    let mut agent = String::new();
    for t in tries {
        let reward = match (t.win, t.hidden) {
            (Some(true), true) => "WON: the held-out tests pass",
            (Some(true), false) => "WON: the verifier passed it",
            (_, true) => "LOST: the held-out tests fail",
            (_, false) => "LOST: the verifier did not pass it",
        };
        agent.push_str(&format!("### {reward} (drone {}, attempt {}, verdict {})\n", t.drone, t.work, t.verdict));
        if !t.done.trim().is_empty() {
            agent.push_str(&format!("The agent's account: {}\n", cut(t.done.trim())));
        }
        if !t.note.trim().is_empty() {
            agent.push_str(&format!("The verifier's note: {}\n", cut(t.note.trim())));
        }
        let diff = repo_dir(joins, &t.author).map(|d| change(&d, &t.base, &t.head)).unwrap_or_default();
        if !diff.trim().is_empty() {
            agent.push_str(&format!("Its change (excerpt):\n{}\n", cut(&diff)));
        }
        agent.push('\n');
    }
    let (won, lost) = gr.sides();
    let id = episode_id(&gr.goal, &gr.id);
    Episode {
        id: id.clone(),
        ms: gr.tries.iter().map(|t| t.ms).max().unwrap_or_default(),
        repo: if gr.repo.is_empty() { g.repos.first().map(|r| r.name.clone()).unwrap_or_default() } else { gr.repo.clone() },
        session: id,
        task: crate::notes::redact(&format!("{}\n(goal: {})", gr.title, g.spec.lines().next().unwrap_or_default())),
        agent: crate::notes::redact(agent.trim_end()),
        correction: format!(
            "group {}/{}: {} attempt(s) by {} drone(s); won: {}; lost: {}; decided by {}",
            gr.goal,
            gr.id,
            gr.tries.len(),
            gr.members.len(),
            won.join(", "),
            lost.join(", "),
            gr.decided_by()
        ),
        kind: KIND.into(),
        ..Episode::default()
    }
}

/// What reflecting on a group did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reflected {
    /// The episode was written.
    Recorded(String),
    /// Its episode exists already: reflected before.
    Already(String),
    /// Why nothing was written: not a group, not settled, no spread.
    Nothing(String),
}

/// The ids of the store's episodes.
fn episode_ids(home: &Path) -> HashSet<String> {
    super::read_lines::<Episode>(&home.join("episodes.jsonl")).into_iter().map(|e| e.id).collect()
}

/// Writes `gr`'s episode when it is settled, has a spread and no episode yet (`known`).
fn record(home: &Path, gr: &Group, g: &Goal, known: &HashSet<String>) -> Reflected {
    let id = episode_id(&gr.goal, &gr.id);
    if known.contains(&id) {
        return Reflected::Already(id);
    }
    if gr.members.len() < 2 {
        return Reflected::Nothing(format!("{} is a group of one: nothing to compare", gr.id));
    }
    if !gr.settled() {
        let waiting = gr.tries.iter().filter(|t| t.win.is_none()).count();
        let why = match (gr.verified, waiting) {
            (false, _) => "not every attempt is verified yet".to_string(),
            (true, n) => format!("{n} attempt(s) wait for the hidden oracle (`borg debug hive score {}`)", gr.goal),
        };
        return Reflected::Nothing(format!("{}: {why}", gr.id));
    }
    if !gr.spread() {
        let all = if gr.tries.iter().all(|t| t.win == Some(true)) { "won" } else { "lost" };
        return Reflected::Nothing(format!("{}: every attempt {all}, no difference to learn from", gr.id));
    }
    let joins = super::read_lines::<super::Join>(&super::joins_path(home, &gr.goal));
    let e = episode(gr, g, &joins);
    match learn::append_jsonl(&home.join("episodes.jsonl"), &e) {
        Ok(()) => Reflected::Recorded(id),
        Err(err) => Reflected::Nothing(format!("{}: {err}", gr.id)),
    }
}

/// `borg learn --group <goal>/<task>`: reflects on that group (any node may: the engineer asked), under
/// the learn lock.
///
/// # Errors
/// The spec is not `<goal>/<task>`, the goal is unknown, or the task is no group of it.
pub fn reflect(home: &Path, spec: &str) -> Result<Reflected, String> {
    let (goal_id, task) =
        spec.split_once('/').filter(|(g, t)| !g.is_empty() && !t.is_empty()).ok_or_else(|| format!("--group {spec:?}: give <goal>/<task>"))?;
    let (g, groups) = read(home, goal_id)?;
    // A member's id names its group too.
    let wanted = |gr: &&Group| gr.id == task || gr.members.iter().any(|m| m == task);
    match groups.iter().find(wanted) {
        // Under `borg learn`'s lock (learn.rs `learn_in_reporting`): a session end's run records pending
        // groups under it, and the episode must be written once.
        Some(gr) => Ok(crate::live::locked(&home.join("lessons.jsonl"), || record(home, gr, &g, &episode_ids(home)))),
        None => Ok(Reflected::Nothing(format!("{task} is no group of goal {goal_id} (`borg hive open {goal_id} --id {task} --group K`)"))),
    }
}

/// Records the episode of every settled group with a spread that was opened on this node and has none
/// yet; what `borg learn` runs first (learn.rs), under its lock (the caller holds it). Returns the
/// episode ids written.
pub fn record_pending(home: &Path) -> Vec<String> {
    if std::fs::metadata(home.join("hive")).is_err() {
        return Vec::new();
    }
    let known = std::cell::OnceCell::new();
    let mut out = Vec::new();
    for id in goal::list(home) {
        let Ok((g, groups)) = read(home, &id) else { continue };
        for gr in groups.iter().filter(|gr| super::is_local(home, &gr.node) && gr.settled() && gr.spread()) {
            if let Reflected::Recorded(e) = record(home, gr, &g, known.get_or_init(|| episode_ids(home))) {
                out.push(e);
            }
        }
    }
    out
}

/// A group as borg-web and `borg top` show it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Row {
    pub group: String,
    pub members: usize,
    pub attempts: usize,
    pub won: Vec<String>,
    pub lost: Vec<String>,
    pub decided_by: String,
    /// The member an integration merges now (`best`); empty before one is accepted.
    pub integrates: String,
    /// The reflection: `lesson <id>`, `not a lesson`, `learning` (its episode waits for `borg learn`),
    /// `to reflect` (settled with a spread: the next `borg learn` records it), `no spread`, or `waiting`
    /// (not settled).
    pub reflection: String,
}

/// The rows of goal `g`'s groups on `b` (`events` folded), with the store's scores, episodes and lessons.
pub fn rows(home: &Path, g: &Goal, events: &[Event], b: &Board) -> Vec<Row> {
    if !b.tasks.values().any(|s| s.task.group.is_some()) {
        return Vec::new();
    }
    let scores = scores(home, &g.id);
    let chosen: Vec<&TaskState> = merged(b, |v| scores.get(v).copied());
    let episodes = episode_ids(home);
    let lessons = learn::lessons(&home.join("lessons.jsonl"));
    let skipped = std::fs::read_to_string(home.join("episodes-not-lessons.txt")).unwrap_or_default();
    of_board(g, events, b, &scores)
        .into_iter()
        .map(|gr| {
            let id = episode_id(&gr.goal, &gr.id);
            let reflection = match lessons.iter().find(|l| l.episodes.contains(&id)) {
                Some(l) => format!("lesson {}", l.id),
                None if skipped.lines().any(|l| l == id) => "not a lesson".into(),
                None if episodes.contains(&id) => "learning".into(),
                None if gr.settled() && gr.spread() => "to reflect".into(),
                None if gr.settled() => "no spread".into(),
                None => "waiting".into(),
            };
            let (won, lost) = gr.sides();
            Row {
                integrates: chosen.iter().find(|s| gr.members.contains(&s.task.id)).map(|s| s.task.id.clone()).unwrap_or_default(),
                group: gr.id.clone(),
                members: gr.members.len(),
                attempts: gr.tries.len(),
                decided_by: gr.decided_by().into(),
                won,
                lost,
                reflection,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::board::{Event, Task};
    use crate::hive::goal::Check;

    fn member(group: &str, n: usize) -> Task {
        Task {
            id: member_id(group, n),
            role: Role::Work,
            title: format!("do {group}"),
            repo: String::new(),
            files: vec!["x.txt".into()],
            deps: vec![],
            accept: vec![],
            of: None,
            not_by: vec![],
            flags: vec![],
            from: None,
            group: Some(group.into()),
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

    /// The lines of drone `d` (session `s-<d>`) claiming member `n` of `group` at `ms`, done at ms+1 and
    /// judged by `s-ver` at ms+3 (`pass` or a reject).
    fn attempt(group: &str, n: usize, d: &str, ms: i64, pass: bool) -> Vec<Event> {
        let t = member_id(group, n);
        let s = format!("s-{d}");
        let v = crate::hive::verify_task(&member(group, n), 0, &s);
        let verdict =
            if pass { Event::new(Ev::Verified, &v.id, "s-ver", ms + 3) } else { Event::new(Ev::Rejected, &v.id, "s-ver", ms + 3).with_note("x.txt is wrong") };
        vec![
            Event { base: format!("b{n}"), ..Event::new(Ev::Claim, &t, &s, ms).with_drone(d) },
            Event { head: format!("h{n}"), ..Event::new(Ev::Done, &t, &s, ms + 1).with_note(&format!("{d} wrote x.txt")) },
            Event::open(v.clone(), &s, ms + 1),
            Event::new(Ev::Claim, &v.id, "s-ver", ms + 2).with_drone("dv"),
            verdict,
        ]
    }

    fn opens(group: &str, k: usize) -> Vec<Event> {
        (1..=k).map(|n| Event::open(member(group, n), "engineer:e", 1)).collect()
    }

    /// d1's rework of t.g1 after its rejection, passed at 43.
    fn rework() -> Vec<Event> {
        let v2 = crate::hive::verify_task(&member("t", 1), 1, "s-d1");
        vec![
            Event { base: "b1".into(), ..Event::new(Ev::Claim, "t.g1", "s-d1", 40).with_drone("d1") },
            Event { head: "h1b".into(), ..Event::new(Ev::Done, "t.g1", "s-d1", 41).with_note("d1 fixed x.txt") },
            Event::open(v2.clone(), "s-d1", 41),
            Event::new(Ev::Claim, &v2.id, "s-ver", 42),
            Event::new(Ev::Verified, &v2.id, "s-ver", 43),
        ]
    }

    #[test]
    fn a_drone_that_held_one_attempt_of_a_group_may_not_claim_another() {
        let mut ev = opens("t", 2);
        ev.push(Event::new(Ev::Claim, "t.g1", "s-d1", 10).with_drone("d1"));
        // The same drone's next session, and the same session, are refused the second member.
        ev.push(Event::new(Ev::Claim, "t.g2", "s-d1b", 11).with_drone("d1"));
        ev.push(Event::new(Ev::Claim, "t.g2", "s-d1", 12));
        let b = board::fold(&ev);
        assert_eq!(b.tasks["t.g2"].status, Status::Open, "{:?}", b.lost);
        assert_eq!(b.lost.len(), 2);
        // Another drone takes it.
        ev.push(Event::new(Ev::Claim, "t.g2", "s-d2", 13).with_drone("d2"));
        assert!(matches!(board::fold(&ev).tasks["t.g2"].status, Status::Claimed { .. }));
        // A task outside the group is open to d1.
        let mut other = member("u", 1);
        other.group = None;
        ev.push(Event::open(other, "engineer:e", 1));
        ev.push(Event::new(Ev::Claim, "u.g1", "s-d1b", 14).with_drone("d1"));
        assert!(matches!(board::fold(&ev).tasks["u.g1"].status, Status::Claimed { .. }));
    }

    #[test]
    fn a_drone_that_verified_one_attempt_may_not_make_another_nor_a_rival_judge_one() {
        let mut ev = opens("t", 3);
        // d1 attempts t.g1; dv verifies it.
        ev.extend(attempt("t", 1, "d1", 10, true));
        // dv, having read d1's solution, is refused t.g3; d1 is refused t.g2's verification.
        ev.push(Event::new(Ev::Claim, "t.g3", "s-ver2", 20).with_drone("dv"));
        let b = board::fold(&ev);
        assert_eq!(b.tasks["t.g3"].status, Status::Open, "a verifier of a rival attempt");
        ev.push(Event { base: "b2".into(), ..Event::new(Ev::Claim, "t.g2", "s-d2", 21).with_drone("d2") });
        ev.push(Event { head: "h2".into(), ..Event::new(Ev::Done, "t.g2", "s-d2", 22) });
        let v2 = crate::hive::verify_task(&member("t", 2), 0, "s-d2");
        ev.push(Event::open(v2.clone(), "s-d2", 22));
        ev.push(Event::new(Ev::Claim, &v2.id, "s-d1b", 23).with_drone("d1"));
        let b = board::fold(&ev);
        assert_eq!(b.tasks[&v2.id].status, Status::Open, "an attempter may not judge a rival's attempt");
        // The verifier of one member may verify another; a fresh drone takes t.g3.
        ev.push(Event::new(Ev::Claim, &v2.id, "s-ver", 24).with_drone("dv"));
        ev.push(Event::new(Ev::Claim, "t.g3", "s-d3", 25).with_drone("d3"));
        let b = board::fold(&ev);
        assert!(matches!(b.tasks[&v2.id].status, Status::Claimed { .. }) && matches!(b.tasks["t.g3"].status, Status::Claimed { .. }), "{:?}", b.lost);
    }

    #[test]
    fn the_hidden_score_decides_the_reward_and_the_best_attempt() {
        let mut ev = opens("t", 3);
        ev.extend(attempt("t", 1, "d1", 10, true));
        ev.extend(attempt("t", 2, "d2", 20, true));
        ev.extend(attempt("t", 3, "d3", 30, true));
        let b = board::fold(&ev);
        let none: HashMap<String, bool> = HashMap::new();
        // Unscored with a hidden oracle here: waiting, and the earliest verified is the best.
        let gr = &of_board(&goal(true), &ev, &b, &none)[0];
        assert!(gr.verified && !gr.settled() && gr.tries.iter().all(|t| t.win.is_none() && t.hidden));
        assert_eq!(merged(&b, |_| None).iter().map(|s| s.task.id.as_str()).collect::<Vec<_>>(), ["t.g1"]);
        // Scored: d2 passes the held-out tests, d1 and d3 do not.
        let scores: HashMap<String, bool> = [("t.g1.v1", false), ("t.g2.v1", true), ("t.g3.v1", false)].iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let gr = &of_board(&goal(true), &ev, &b, &scores)[0];
        assert!(gr.settled() && gr.spread());
        assert_eq!(gr.sides(), (vec!["d2".to_string()], vec!["d1".to_string(), "d3".to_string()]));
        assert_eq!(gr.decided_by(), "the hidden oracle");
        let chosen = merged(&b, |v| scores.get(v).copied());
        assert_eq!(chosen.iter().map(|s| s.task.id.as_str()).collect::<Vec<_>>(), ["t.g2"], "only the winner is integrated");
        // Without a hidden oracle the verdicts decide: all passed, no spread.
        let gr = &of_board(&goal(false), &ev, &b, &none)[0];
        assert!(gr.settled() && !gr.spread() && gr.decided_by() == "the verdicts");
    }

    #[test]
    fn a_rejected_attempt_is_a_loser_and_its_rework_a_winner() {
        let mut ev = opens("t", 2);
        ev.extend(attempt("t", 1, "d1", 10, false));
        ev.extend(attempt("t", 2, "d2", 20, true));
        let b = board::fold(&ev);
        let gr = &of_board(&goal(false), &ev, &b, &HashMap::new())[0];
        assert!(!gr.verified && !gr.settled(), "t.g1 is reopened for rework");
        // d1 reworks it and passes: settled, d1's first attempt lost and its rework won.
        ev.extend(rework());
        let b = board::fold(&ev);
        let gr = &of_board(&goal(false), &ev, &b, &HashMap::new())[0];
        assert!(gr.settled() && gr.spread());
        assert_eq!(
            gr.tries.iter().map(|t| (t.verify.as_str(), t.win)).collect::<Vec<_>>(),
            [("t.g1.v1", Some(false)), ("t.g2.v1", Some(true)), ("t.g1.v2", Some(true))]
        );
        // The integration takes the member that needed no rework.
        assert_eq!(merged(&b, |_| None).iter().map(|s| s.task.id.as_str()).collect::<Vec<_>>(), ["t.g2"]);
    }

    fn store(name: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!("borg-group-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    #[test]
    fn two_goals_groups_never_share_an_episode() {
        assert_ne!(episode_id("a", "b-c"), episode_id("a-b", "c"));
        assert_eq!(episode_id("a-b", "c.d"), "group-a-b.c.d");
    }

    #[test]
    fn only_the_node_a_group_was_opened_on_records_it_unasked() {
        let home = store("remote");
        goal::create(&home, &goal(false)).unwrap();
        let mut ev: Vec<Event> = opens("t", 2).into_iter().map(|e| Event { node: "another-node".into(), ..e }).collect();
        ev.extend(attempt("t", 1, "d1", 10, false));
        ev.extend(attempt("t", 2, "d2", 20, true));
        ev.extend(rework());
        board::append(&super::super::tasks_path(&home, "g"), &ev).unwrap();
        assert!(record_pending(&home).is_empty(), "the group's own node learns it");
        assert_eq!(reflect(&home, "g/t").unwrap(), Reflected::Recorded("group-g.t".into()), "an engineer may ask any node");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_group_is_reflected_once_and_one_without_spread_or_of_one_never() {
        let home = store("once");
        goal::create(&home, &goal(false)).unwrap();
        let mut ev = opens("t", 2);
        ev.extend(attempt("t", 1, "d1", 10, false));
        ev.extend(attempt("t", 2, "d2", 20, true));
        board::append(&super::super::tasks_path(&home, "g"), &ev).unwrap();
        // Not settled: t.g1 waits for its rework.
        assert!(record_pending(&home).is_empty());
        assert!(matches!(reflect(&home, "g/t").unwrap(), Reflected::Nothing(why) if why.contains("not every attempt is verified")));
        board::append(&super::super::tasks_path(&home, "g"), &rework()).unwrap();
        assert_eq!(record_pending(&home), ["group-g.t"]);
        assert!(record_pending(&home).is_empty(), "once per group");
        assert_eq!(reflect(&home, "g/t.g2").unwrap(), Reflected::Already("group-g.t".into()));
        let eps = super::super::read_lines::<Episode>(&home.join("episodes.jsonl"));
        assert_eq!(eps.len(), 1);
        let e = &eps[0];
        assert_eq!((e.kind.as_str(), e.ms), (KIND, 43));
        assert!(e.correction.contains("won: d2, d1; lost: d1") && e.correction.contains("decided by the verdicts"), "{}", e.correction);
        let won = e.agent.find("### WON").unwrap();
        let lost = e.agent.find("### LOST").unwrap();
        assert!(won < lost && e.agent.contains("x.txt is wrong") && e.agent.contains("d2 wrote x.txt"), "{}", e.agent);
        assert!(e.task.contains("do t") && e.task.contains("the spec"));
        // A group whose attempts all passed, and a plain task, write nothing.
        let mut ev = opens("u", 2);
        ev.extend(attempt("u", 1, "d1", 100, true));
        ev.extend(attempt("u", 2, "d2", 200, true));
        board::append(&super::super::tasks_path(&home, "g"), &ev).unwrap();
        assert!(record_pending(&home).is_empty());
        assert!(matches!(reflect(&home, "g/u").unwrap(), Reflected::Nothing(why) if why.contains("every attempt won")));
        assert!(matches!(reflect(&home, "g/nope").unwrap(), Reflected::Nothing(why) if why.contains("is no group")));
        assert!(reflect(&home, "g").is_err() && reflect(&home, "missing/t").is_err());
        let _ = std::fs::remove_dir_all(home);
    }
}
