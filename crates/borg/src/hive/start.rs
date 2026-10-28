//! A work claim's start (docs/design/hive.md "Board"): the commit its work begins from, computed from
//! the board and git objects alone, never from the drone's worktree. Start = one commit whose only
//! parent is the goal's origin for the repo ([`origin`]) and whose tree is the origin with the
//! accepted change (base to head, verdict.rs `accepted`) of each dep in the task's closure applied in
//! closure order (integrate.rs `step`, as the integration applies them), borg's identity and the
//! newest applied done as its date: the same board gives the same commit on every node.
//!
//! The origin of a repo is the one the goal recorded (`borg hive init`, the repo it ran in), else the
//! one on the board: the first claim in a repo the goal recorded none for (another repo in scope, a
//! goal made outside git or by an older borg) writes the HEAD of that repo's main checkout (an empty
//! root commit when it has none yet, which every clone builds the same) as an `origin` line, and the first such line in the fold's order
//! is the origin on every node (board.rs `Board::origins`). Two nodes that each wrote one before the
//! other's came: work started from the losing one fails its verdict's recompute, and its rework starts
//! from the winner.
//!
//! Safety (no orphan work): a claim's base is its start, so its tree holds nothing but the origin and
//! verified diffs, and `deps_in` names the attempt of every dep whose accepted change was applied, an
//! empty one too (one rule for the claim and the verdict). Work a blocked, rejected or lost attempt left
//! in a reused worktree is in no base: the claim resets the drone's own worktree to the start once it
//! won (hive/mod.rs `claim`), the tree before kept in a pinned commit; a session outside such a worktree
//! claims only where its tree already is the start. Deps whose changes conflict are refused here and
//! written `blocked` by the claim, once, for the engineer: never a base with conflict markers. No timer
//! decides any of it: a git call out of time (`within` at 0) is no start, and the claim is refused
//! before it writes its claim or touches the tree (a first claim in a repo may have recorded the
//! repo's origin by then: a board fact, kept). Such a refusal is worth another try ([`NoStart::Later`]):
//! the task stays offered to the session, unlike a dep without verified work to bring in.
//!
//! A verifier recomputes the start from the attempts `deps_in` names, each found on the board by its
//! head (never by `accepted`, which can move) and its commits fetched from their bundle when another
//! node did it (commits.rs `fetch`), and a base that is not it is an unchecked reject ([`missing`]).
//!
//! A clone that lacks the origin, after the deps' commits came from their bundles (which carry an
//! origin no remote holds), starts nothing: why is kept for this node (`NOT_HERE`, never synced)
//! and shown as an alert in `borg top` and the Hive tab until a start is had in that repo.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use super::board::{self, Board, Ev, Event, Role, Task};
use super::verdict::{self, Attempt};
use super::{Goal, commits, goal, integrate};
use crate::{learn, queen};

/// The subject of every start commit.
pub const SUBJECT: &str = "borg hive start";
/// The subject of the origin of a repo that had no commit when its first claim recorded one.
const EMPTY_ORIGIN: &str = "borg hive origin";
/// This node's repos of a goal where no work starts, and why (`note_here`): repo -> why.
const NOT_HERE: &str = "not-here.json";

/// A claim's start: the commit, and the deps whose accepted change it applied (`task@head`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Start {
    pub commit: String,
    pub deps_in: Vec<String>,
}

/// Why a claim has no start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoStart {
    /// The deps' accepted changes conflict: a board fact, the same for every claimant (`blocked`).
    Conflict(String),
    /// The origin is not in this clone, nor in the deps' bundles: claimed elsewhere.
    Here(String),
    /// No start this tree can take: a dep without verified work to bring in.
    Not(String),
    /// Not computed now, worth another try: a git call that failed or ran out of time, an origin that
    /// could not be recorded on the board.
    Later(String),
}

/// The origin of `repo` recorded for goal `g`: the goal's own, else the board's `b`.
fn recorded(g: &Goal, b: &Board, repo: &str) -> Option<String> {
    goal::origin_of(g, repo).or_else(|| b.origin(repo)).map(str::to_string)
}

impl NoStart {
    /// Why, whatever the outcome.
    fn why(self) -> String {
        match self {
            NoStart::Conflict(why) | NoStart::Here(why) | NoStart::Not(why) | NoStart::Later(why) => why,
        }
    }
}

/// The origin of goal `g` on the board `b` for the repo `dir` is in: recorded and present in this clone.
///
/// # Errors
/// Why there is none here.
fn origin(g: &Goal, b: &Board, dir: &Path, deadline: Instant) -> Result<String, String> {
    let repo = queen::repo_name(&dir.display().to_string());
    let sha = recorded(g, b, &repo).ok_or_else(|| format!("goal {} has no origin recorded for repo {repo}", g.id))?;
    present(g, &repo, &sha, dir, deadline).map_err(NoStart::why)
}

/// `sha`, the origin of `repo` (which `dir` is in) for goal `g`, when this clone holds it, or it is the
/// empty root origin ([`empty_origin`]), which any clone builds the same.
///
/// # Errors
/// [`NoStart::Here`] when this clone lacks it; [`NoStart::Later`] when git failed or ran out of time.
fn present(g: &Goal, repo: &str, sha: &str, dir: &Path, deadline: Instant) -> Result<String, NoStart> {
    match integrate::git(dir, &["cat-file", "-e", &format!("{sha}^{{commit}}")], &[], deadline).map_err(NoStart::Later)? {
        (0, _) => Ok(sha.to_string()),
        _ if empty_origin(dir, deadline).map_err(NoStart::Later)? == sha => Ok(sha.to_string()),
        _ => Err(NoStart::Here(format!(
            "the origin {} of goal {} is not in this clone of {repo}: fetch it (`git fetch`), or claim on a node that holds it",
            short(sha),
            g.id
        ))),
    }
}

/// The origin of a repo that had no commit when its first claim recorded one: a root commit of the
/// empty tree with borg's identity and date 0, written in `dir`'s repo, the same commit on every node.
fn empty_origin(dir: &Path, deadline: Instant) -> Result<String, String> {
    let empty = integrate::git_ok(dir, &["mktree"], &[], deadline)?;
    integrate::commit(dir, &empty, &[], EMPTY_ORIGIN, 0, deadline)
}

/// Records the origin of `repo` (which `dir` is in) for goal `g` on its board, by `by`, unless a line
/// holds one already, under the board's lock: the HEAD of the repo's main checkout, or the empty root
/// origin when it has none. The origin the board folds to then.
fn record(home: &Path, g: &Goal, by: &str, repo: &str, dir: &Path, deadline: Instant) -> Result<String, String> {
    let common = integrate::git_ok(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"], &[], deadline)?;
    let sha = match integrate::git(dir, &[&format!("--git-dir={common}"), "rev-parse", "--verify", "-q", "HEAD^{commit}"], &[], deadline)? {
        (0, out) if !out.trim().is_empty() => out.trim().to_string(),
        _ => empty_origin(dir, deadline)?,
    };
    let line = Event::new(Ev::Heartbeat, "", by, learn::now_ms()).with_note(&board::origin_note(repo, &sha));
    super::write_if(home, &g.id, |events| board::fold(events).origin(repo).is_none().then(|| vec![line]))
        .map_err(|e| format!("could not record the origin of {repo} on goal {}'s board: {e}", g.id))?;
    let b = board::fold(&board::read(&super::tasks_path(home, &g.id)));
    b.origin(repo).map(str::to_string).ok_or_else(|| format!("no origin of {repo} on goal {}'s board", g.id))
}

/// Keeps for this node why no work starts in `repo` of goal `goal` (None: it does now), written only
/// when that changed, so a waiter refused on every poll writes it once.
fn note_here(home: &Path, goal: &str, repo: &str, why: Option<&str>) {
    let path = super::dir(home, goal).join(NOT_HERE);
    let mut all = not_here(home, goal);
    let was = all.get(repo).cloned();
    match why {
        Some(w) if was.as_deref() != Some(w) => all.insert(repo.to_string(), w.to_string()),
        None if was.is_some() => all.remove(repo),
        _ => return,
    };
    if let Ok(text) = serde_json::to_string(&all) {
        let _ = std::fs::write(path, text);
    }
}

/// This node's repos of goal `goal` where no work starts, and why: what `borg top` and the Hive tab
/// show as alerts (view.rs `alerts`).
pub fn not_here(home: &Path, goal: &str) -> BTreeMap<String, String> {
    std::fs::read_to_string(super::dir(home, goal).join(NOT_HERE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

fn tree(dir: &Path, rev: &str, deadline: Instant) -> Result<String, String> {
    integrate::git_ok(dir, &["rev-parse", "--verify", "-q", &format!("{rev}^{{tree}}")], &[], deadline)
}

/// The start of a claim by `by` of `task` (a work task without `from`) of goal `g` on the board `b`
/// (folded from `events`), in the repo `dir` is in, `within` that time: the repo's origin (recorded by
/// this claim when there is none, [`record`]), and each dep's accepted attempt, its commits fetched from
/// their bundle when another node did it (commits.rs `fetch`), applied in `closure` order onto it. The
/// deps' commits are fetched before the origin is looked for: a bundle carries an origin no remote holds.
///
/// # Errors
/// A conflict between the deps' changes ([`NoStart::Conflict`], with the paths), or why it cannot be
/// computed here.
#[allow(clippy::too_many_arguments)]
pub fn of(home: &Path, g: &Goal, by: &str, task: &Task, events: &[Event], b: &Board, dir: &Path, within: Duration) -> Result<Start, NoStart> {
    let deadline = Instant::now() + within;
    let repo = queen::repo_name(&dir.display().to_string());
    let mut picked = Vec::new();
    for d in board::closure(b, task) {
        let a = verdict::accepted(events, b, &d).filter(|a| !a.base.is_empty() && !a.head.is_empty());
        let a = a.ok_or_else(|| NoStart::Not(format!("{d} has no verified work to bring into this tree")))?;
        commits::fetch(home, &g.id, &a, dir);
        picked.push((d, a));
    }
    // A git call that failed or ran out of time here is no clone lacking the origin: no alert, and
    // worth another try (`Later`), never a start this tree cannot take.
    let sha = match recorded(g, b, &repo) {
        Some(sha) => sha,
        None => record(home, g, by, &repo, dir, deadline).map_err(NoStart::Later)?,
    };
    let origin = match present(g, &repo, &sha, dir, deadline) {
        Ok(o) => o,
        Err(NoStart::Here(why)) => {
            note_here(home, &g.id, &repo, Some(&why));
            return Err(NoStart::Here(why));
        }
        Err(e) => return Err(e),
    };
    note_here(home, &g.id, &repo, None);
    build(g, &origin, &picked, dir, deadline)
}

/// The start from `origin` with the changes of `attempts` (dep, its attempt) applied in order: every one
/// named in `deps_in`, an empty change too, and the start one commit of the final tree on the origin.
fn build(g: &Goal, origin: &str, attempts: &[(String, Attempt)], dir: &Path, deadline: Instant) -> Result<Start, NoStart> {
    let (mut tip, mut deps_in, mut ms) = (origin.to_string(), Vec::new(), g.created_ms);
    let mut held = tree(dir, origin, deadline).map_err(NoStart::Later)?;
    let mut applied: Vec<&str> = Vec::new();
    for (d, a) in attempts {
        let (t, paths) =
            integrate::step(dir, &a.base, &tip, &a.head, deadline).map_err(|e| NoStart::Later(format!("cannot bring {d}'s verified work in: {e}")))?;
        if !paths.is_empty() {
            let with = if applied.is_empty() { "the goal's origin".to_string() } else { applied.join(", ") };
            return Err(NoStart::Conflict(format!("the verified work of {d} conflicts with {with} on {}: reopen one on top of the other", paths.join(", "))));
        }
        ms = ms.max(a.done_ms);
        deps_in.push(format!("{d}@{}", a.head));
        applied.push(d);
        // A change the tip holds already (another dep made it) leaves the tree: no step commit.
        if t != held {
            tip = integrate::commit(dir, &t, &[origin], SUBJECT, ms, deadline).map_err(NoStart::Later)?;
            held = t;
        }
    }
    let commit = integrate::commit(dir, &held, &[origin], SUBJECT, ms, deadline).map_err(NoStart::Later)?;
    Ok(Start { commit, deps_in })
}

/// What is wrong with the start of the attempt `a` of goal `g`, checked in `dir` `within` that time:
/// empty when its base is the start of the attempts its claim's `deps_in` names (each found on the
/// board by its head, its commits fetched from their bundle, commits.rs `fetch`), every dep of its
/// closure is named, and its base's one parent is the origin. A repo with no origin recorded (work
/// claimed before origins were) and a fix task (its base is the combined tree) are checked on the board
/// alone (verdict.rs `missing_deps`). Out of time is said too: never a pass on a start not checked.
pub fn missing(home: &Path, g: &Goal, events: &[Event], b: &Board, a: &Attempt, dir: &Path, within: Duration) -> Vec<String> {
    let repo = queen::repo_name(&dir.display().to_string());
    let fix = b.tasks.get(&a.work).is_some_and(|w| w.task.from.is_some() || w.task.role != Role::Work);
    if recorded(g, b, &repo).is_none() || fix || a.base.is_empty() {
        return verdict::missing_deps(events, b, a);
    }
    let deadline = Instant::now() + within;
    match recheck(home, g, events, b, a, dir, deadline) {
        Ok(()) => Vec::new(),
        Err(why) => vec![why],
    }
}

fn recheck(home: &Path, g: &Goal, events: &[Event], b: &Board, a: &Attempt, dir: &Path, deadline: Instant) -> Result<(), String> {
    let w = b.tasks.get(&a.work).ok_or_else(|| format!("no task {}", a.work))?;
    let mut picked = Vec::new();
    for d in board::closure(b, &w.task) {
        let head = a.deps_in.iter().find_map(|x| x.strip_prefix(&format!("{d}@"))).ok_or_else(|| format!("{d}'s verified work is not in its tree"))?;
        let attempts = b.tasks.get(&d).map(|s| s.attempts.as_slice()).unwrap_or_default();
        let x = attempts.iter().find(|x| x.verified && x.head == head).ok_or_else(|| format!("{d}@{} is no verified attempt", short(head)))?;
        let at = verdict::attempt_of(events, b, &d, &x.by, x.done).ok_or_else(|| format!("{d}@{} is no attempt on the board", short(head)))?;
        // Another node's dep: its commits come from their bundle, as the claim's did.
        commits::fetch(home, &g.id, &at, dir);
        picked.push((d, at));
    }
    let origin = origin(g, b, dir, deadline)?;
    let parents = integrate::git_ok(dir, &["rev-parse", &format!("{}^@", a.base)], &[], deadline)?;
    if parents != origin {
        return Err(format!("its base {} is not a start on the goal's origin {}", short(&a.base), short(&origin)));
    }
    let start = build(g, &origin, &picked, dir, deadline).map_err(NoStart::why)?;
    if tree(dir, &start.commit, deadline)? != tree(dir, &a.base, deadline)? {
        return Err(format!(
            "its base {} is not the start of the origin and {}",
            short(&a.base),
            if a.deps_in.is_empty() { "no dep".into() } else { a.deps_in.join(", ") }
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::super::*;
    use super::{missing, of};

    fn git_ok(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.email=e@x", "-c", "user.name=e"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn tree(dir: &Path, rev: &str) -> String {
        git_ok(dir, &["rev-parse", &format!("{rev}^{{tree}}")])
    }

    /// A repo `app` with `README` committed (the goal's origin), runner worktrees 1 and 2 of goal g,
    /// and a store whose goal g records that origin, on tmpfs (`test_tmp`): (root, repo, home, goal,
    /// origin).
    fn origin_repo(tag: &str) -> (PathBuf, PathBuf, PathBuf, Goal, String) {
        let root = test_tmp().join(format!("borg-hive-start-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (repo, home) = (root.join("app"), root.join("store"));
        std::fs::create_dir_all(&repo).unwrap();
        git_ok(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README"), "base\n").unwrap();
        git_ok(&repo, &["add", "-A"]);
        git_ok(&repo, &["commit", "-qm", "base"]);
        let origin = git_ok(&repo, &["rev-parse", "HEAD"]);
        for n in 1..=2 {
            git_ok(&repo, &["worktree", "add", "-q", "-b", &format!("hive/g/{n}"), run::worktree(&repo, "g", n).to_str().unwrap()]);
        }
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
            held_out: false,
            origin: vec![goal::Origin { repo: "app".into(), sha: origin.clone() }],
        };
        goal::create(&home, &g).unwrap();
        (root, repo, home, g, origin)
    }

    fn open_work(home: &Path, id: &str, deps: &[&str]) -> Task {
        let t = new_task(id, Role::Work, &format!("do {id}"), "", vec![], deps.iter().map(|d| d.to_string()).collect(), vec![]).unwrap();
        board::append(&tasks_path(home, "g"), &[Event::open(t.clone(), "engineer:e", learn::now_ms())]).unwrap();
        t
    }

    fn lines(home: &Path) -> Vec<Event> {
        board::read(&tasks_path(home, "g"))
    }

    fn claim_line(home: &Path, task: &str, by: &str) -> Event {
        lines(home).into_iter().rfind(|e| e.ev == Ev::Claim && e.task == task && e.by == by).expect("a claim line")
    }

    /// The commit the claim of `task` by `by` kept the worktree in before its reset (its progress line).
    fn kept_before_reset(home: &Path, task: &str, by: &str) -> String {
        let e = lines(home).into_iter().rfind(|e| e.ev == Ev::Progress && e.task == task && e.by == by).expect("a reset note");
        e.note.split("worktree before the reset: ").nth(1).and_then(|s| s.split(|c: char| !c.is_ascii_hexdigit()).next()).expect(&e.note).to_string()
    }

    /// `session`'s claim of `t` done in `dir` (its head a snapshot now) and passed by a verifier: the head.
    fn done_verified(home: &Path, t: &Task, session: &str, dir: &Path) -> String {
        std::thread::sleep(Duration::from_millis(5));
        let ms = learn::now_ms();
        let head = verdict::snapshot(dir, verdict::SNAPSHOT_TIMEOUT).unwrap();
        let v = verify_task(t, 0, session);
        let vid = v.id.clone();
        let done = Event { head: head.clone(), ..Event::new(Ev::Done, &t.id, session, ms) };
        let lines = [
            done,
            Event::open(v, session, ms),
            Event::new(Ev::Claim, &vid, "verifier", ms + 1),
            Event { head: head.clone(), ..Event::new(Ev::Verified, &vid, "verifier", ms + 2) },
        ];
        board::append(&tasks_path(home, "g"), &lines).unwrap();
        // Its commits as a bundle line, as the done publishes them in a mesh member (commits.rs).
        let events = board::read(&tasks_path(home, "g"));
        let a = verdict::attempt_of(&events, &board::fold(&events), &t.id, session, ms).unwrap();
        commits::publish(home, "g", &a, dir).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        head
    }

    /// The store a mesh member: dones publish their commits as bundle lines (commits.rs `meshed`).
    fn mesh(home: &Path) {
        std::fs::create_dir_all(home.join("mesh")).unwrap();
        std::fs::write(home.join("mesh/membership.json"), "{}").unwrap();
    }

    /// `session`'s claim of `t` done in `dir` and rejected by a verifier: the head.
    fn done_rejected(home: &Path, t: &Task, session: &str, dir: &Path) -> String {
        std::thread::sleep(Duration::from_millis(5));
        let ms = learn::now_ms();
        let head = verdict::snapshot(dir, verdict::SNAPSHOT_TIMEOUT).unwrap();
        let v = verify_task(t, 0, session);
        let vid = v.id.clone();
        let lines = [
            Event { head: head.clone(), ..Event::new(Ev::Done, &t.id, session, ms) },
            Event::open(v, session, ms),
            Event::new(Ev::Claim, &vid, "verifier", ms + 1),
            Event { head: head.clone(), ..Event::new(Ev::Rejected, &vid, "verifier", ms + 2).with_note("not yet") },
        ];
        board::append(&tasks_path(home, "g"), &lines).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        head
    }

    fn sessions(home: &Path, tag: &str, drones: &[&str]) -> Vec<String> {
        drones
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let s = format!("{tag}-{i}-{}", std::process::id());
                bind_as(home, &s, "g", d).unwrap();
                s
            })
            .collect()
    }

    /// Deps whose verified changes conflict are a fact of the board: the first claimant writes its claim
    /// and `blocked` with the paths in one write, its tree untouched, and no drone claims the task again.
    #[test]
    fn deps_that_conflict_block_the_task_once_and_no_drone_claims_it() {
        let (root, repo, home, g, _) = origin_repo("conflict");
        let (wt1, wt2) = (run::worktree(&repo, "g", 1), run::worktree(&repo, "g", 2));
        let (w1, w2) = (wt1.display().to_string(), wt2.display().to_string());
        let s = sessions(&home, "conflict", &["d1", "d1", "d2", "d1"]);
        let (t1, t2, t3) = (open_work(&home, "t1", &[]), open_work(&home, "t2", &[]), open_work(&home, "t3", &["t1", "t2"]));
        for (t, sess, body) in [(&t1, &s[0], "one\n"), (&t2, &s[1], "two\n")] {
            claim(&home, &g, sess, t, false, &w1, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).expect("claimed");
            std::fs::write(wt1.join("README"), body).unwrap();
            done_verified(&home, t, sess, &wt1);
        }
        let r = claim(&home, &g, &s[2], &t3, false, &w2, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None);
        assert!(matches!(&r, Err(Refused::Blocked(why)) if why.contains("conflicts with t1 on README")), "{r:?}");
        let blocked = || lines(&home).into_iter().filter(|e| e.ev == Ev::Blocked && e.task == "t3").count();
        assert_eq!(blocked(), 1);
        let b = board::fold(&lines(&home));
        assert!(matches!(&b.tasks["t3"].status, Status::Blocked { note, .. } if note.contains("reopen one on top of the other")));
        assert_eq!(std::fs::read_to_string(wt2.join("README")).unwrap(), "base\n", "nothing touched the tree");
        let n = lines(&home).len();
        assert_eq!(claim(&home, &g, &s[3], &t3, false, &w1, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None), Err(Refused::Lost));
        assert_eq!((lines(&home).len(), blocked()), (n, 1), "no second claim, no second block");
        s.iter().for_each(|s| unbind(s));
        let _ = std::fs::remove_dir_all(root);
    }

    /// Two nodes that both claim the task before either holds the other's lines each write their claim
    /// and `blocked` once: merged, the task is blocked on both, and neither claims it again, so the
    /// conflict is said at most once a node and never claimed in a loop.
    #[test]
    fn deps_that_conflict_on_two_nodes_block_once_on_each_and_no_node_claims_again() {
        let (root, repo, home, g, _) = origin_repo("conflict2");
        let (wt1, wt2) = (run::worktree(&repo, "g", 1), run::worktree(&repo, "g", 2));
        let (w1, w2) = (wt1.display().to_string(), wt2.display().to_string());
        std::fs::create_dir_all(home.join("stats")).unwrap();
        std::fs::write(home.join("stats/node"), "na").unwrap();
        let s = sessions(&home, "conflict2", &["d1", "d1", "d2"]);
        let (t1, t2, t3) = (open_work(&home, "t1", &[]), open_work(&home, "t2", &[]), open_work(&home, "t3", &["t1", "t2"]));
        for (t, sess, body) in [(&t1, &s[0], "one\n"), (&t2, &s[1], "two\n")] {
            claim(&home, &g, sess, t, false, &w1, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).expect("claimed");
            std::fs::write(wt1.join("README"), body).unwrap();
            done_verified(&home, t, sess, &wt1);
        }
        // Node B: its own store holding the same lines, a drone of its own.
        let hb = root.join("store-b");
        goal::create(&hb, &g).unwrap();
        std::fs::create_dir_all(hb.join("stats")).unwrap();
        std::fs::write(hb.join("stats/node"), "nb").unwrap();
        std::fs::copy(tasks_path(&home, "g"), tasks_path(&hb, "g")).unwrap();
        let sb = format!("conflict2-b-{}", std::process::id());
        bind_as(&hb, &sb, "g", "d1").unwrap();
        let held = lines(&home).len();
        let ra = claim(&home, &g, &s[2], &t3, false, &w2, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None);
        let rb = claim(&hb, &g, &sb, &t3, false, &w1, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None);
        assert!(matches!((&ra, &rb), (Err(Refused::Blocked(_)), Err(Refused::Blocked(_)))), "{ra:?} {rb:?}");
        // The mesh round: each node gets the other's new lines.
        let (a_new, b_new) = (lines(&home)[held..].to_vec(), board::read(&tasks_path(&hb, "g"))[held..].to_vec());
        board::append(&tasks_path(&home, "g"), &b_new).unwrap();
        board::append(&tasks_path(&hb, "g"), &a_new).unwrap();
        for h in [&home, &hb] {
            let events = board::read(&tasks_path(h, "g"));
            assert_eq!(events.iter().filter(|e| e.ev == Ev::Blocked && e.task == "t3").count(), 2, "one a node");
            assert!(matches!(board::fold(&events).tasks["t3"].status, Status::Blocked { .. }));
        }
        let n = lines(&home).len();
        assert_eq!(claim(&home, &g, &s[2], &t3, false, &w2, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None), Err(Refused::Lost));
        assert_eq!(claim(&hb, &g, &sb, &t3, false, &w1, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None), Err(Refused::Lost));
        assert_eq!(lines(&home).len(), n, "no node claims it again");
        s.iter().for_each(|s| unbind(s));
        unbind(&sb);
        let _ = std::fs::remove_dir_all(root);
    }

    /// d1 does T (rejected), then U, then T again in one worktree: the rework starts from T's start with
    /// T's previous attempt back on top, uncommitted, and nothing of U; its own diff shows that attempt.
    #[test]
    fn a_rework_starts_from_its_start_with_its_previous_attempt_and_nothing_of_another_task() {
        let (root, repo, home, g, origin) = origin_repo("rework");
        let wt1 = run::worktree(&repo, "g", 1);
        let w1 = wt1.display().to_string();
        let s = sessions(&home, "rework", &["d1", "d1", "d1"]);
        let (t, u) = (open_work(&home, "t", &[]), open_work(&home, "u", &[]));
        claim(&home, &g, &s[0], &t, false, &w1, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).expect("t claimed");
        std::fs::write(wt1.join("t.txt"), "first attempt\n").unwrap();
        done_rejected(&home, &t, &s[0], &wt1);
        claim(&home, &g, &s[1], &u, false, &w1, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).expect("u claimed");
        assert!(!wt1.join("t.txt").exists(), "u starts from the origin");
        std::fs::write(wt1.join("u.txt"), "u's work\n").unwrap();
        done_verified(&home, &u, &s[1], &wt1);
        let won = claim(&home, &g, &s[2], &t, false, &w1, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).expect("t claimed again");
        let c = claim_line(&home, "t", &s[2]);
        assert_eq!(tree(&repo, &c.base), tree(&repo, &origin));
        assert_eq!(std::fs::read_to_string(wt1.join("t.txt")).unwrap(), "first attempt\n", "its previous attempt is back");
        assert!(!wt1.join("u.txt").exists(), "nothing of u");
        let snap = verdict::snapshot(&wt1, verdict::SNAPSHOT_TIMEOUT).unwrap();
        let changed: Vec<String> = verdict::changes(&repo, &c.base, &snap).unwrap().into_iter().map(|c| c.path).collect();
        assert_eq!(changed, ["t.txt"], "the rework's diff holds its previous attempt");
        assert!(
            won.last_progress.contains("previous attempt") && describe(&home, &g, &won, &s[2], &w1).contains("worktree before the reset: "),
            "{}",
            won.last_progress
        );
        s.iter().for_each(|s| unbind(s));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The start is a function of the board and git objects. A verifier on another node holds only its
    /// own clone and the judged attempt's bundle: the recompute fetches each named dep's commits from
    /// their bundle, and passes from the attempt `deps_in` names after `accepted` moved to another
    /// verified attempt (an integration merged it). The same start is computed in that clone.
    #[test]
    fn a_verdict_on_another_node_recomputes_the_start_from_the_named_attempts_and_their_bundles() {
        let (root, repo, home, g, _) = origin_repo("clone");
        mesh(&home);
        let (wt1, wt2) = (run::worktree(&repo, "g", 1), run::worktree(&repo, "g", 2));
        let s = sessions(&home, "clone", &["d1", "d2"]);
        let (t1, t2) = (open_work(&home, "t1", &[]), open_work(&home, "t2", &["t1"]));
        claim(&home, &g, &s[0], &t1, false, &wt1.display().to_string(), learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).expect("t1 claimed");
        std::fs::write(wt1.join("a.txt"), "t1's work\n").unwrap();
        let h1 = done_verified(&home, &t1, &s[0], &wt1);
        claim(&home, &g, &s[1], &t2, false, &wt2.display().to_string(), learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).expect("t2 claimed");
        std::fs::write(wt2.join("b.txt"), "t2's work\n").unwrap();
        done_verified(&home, &t2, &s[1], &wt2);
        let c = claim_line(&home, "t2", &s[1]);
        assert_eq!(c.deps_in, [format!("t1@{h1}")]);
        // A second verified attempt of t1 (another node's, its claim lost here) that an integration
        // merged: `accepted` names it now, the dependent's claim still names the first.
        let ms = learn::now_ms();
        std::fs::write(wt1.join("a.txt"), "t1 again\n").unwrap();
        let h1b = verdict::snapshot(&wt1, verdict::SNAPSHOT_TIMEOUT).unwrap();
        let t1c = claim_line(&home, "t1", &s[0]);
        let mut integrate = new_task("integrate.1", Role::Integrate, "integrate", "", vec![], vec![], vec![]).unwrap();
        integrate.deps = vec!["t1".into()];
        let again = vec![
            Event { base: t1c.base.clone(), ..Event::new(Ev::Claim, "t1", "sx", t1c.ms + 1) },
            Event { claim: Some(t1c.ms + 1), head: h1b.clone(), ..Event::new(Ev::Done, "t1", "sx", ms) },
            Event::new(Ev::Claim, "t1.v1", "verifier2", ms + 1),
            Event { claim: Some(ms + 1), head: h1b.clone(), ..Event::new(Ev::Verified, "t1.v1", "verifier2", ms + 2) },
            Event::open(integrate, "engineer:e", ms + 3),
            Event::new(Ev::Claim, "integrate.1", "integrator", ms + 4),
            Event { deps_in: vec![format!("t1@{h1b}")], ..Event::new(Ev::Verified, "integrate.1", "integrator", ms + 5) },
        ];
        board::append(&tasks_path(&home, "g"), &again).unwrap();
        let events = lines(&home);
        let b = board::fold(&events);
        assert_eq!(verdict::accepted(&events, &b, "t1").map(|a| a.head), Some(h1b), "accepted moved");
        // The verifier's node: a clone of its own, the judged attempt's commits from its bundle, no pin.
        let other = root.join("other").join("app");
        git_ok(&root, &["clone", "-q", "--no-local", repo.to_str().unwrap(), other.to_str().unwrap()]);
        let done = events.iter().rfind(|e| e.ev == Ev::Done && e.task == "t2").unwrap();
        let a = verdict::attempt_of(&events, &b, "t2", &s[1], done.ms).unwrap();
        assert!(commits::fetch(&home, "g", &a, &other), "the judged attempt's bundle");
        let has = |sha: &str| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&other)
                .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        assert!(!has(&h1), "the dep's head is in no bundle but its own");
        assert_eq!(missing(&home, &g, &events, &b, &a, &other, verdict::SNAPSHOT_TIMEOUT), Vec::<String>::new());
        let lied = verdict::Attempt { deps_in: Vec::new(), ..a.clone() };
        assert!(missing(&home, &g, &events, &b, &lied, &other, verdict::SNAPSHOT_TIMEOUT)[0].contains("t1's verified work is not in its tree"));
        assert!(!missing(&home, &g, &events, &b, &a, &other, Duration::ZERO).is_empty(), "out of time: never a pass");
        s.iter().for_each(|s| unbind(s));
        let _ = std::fs::remove_dir_all(root);
    }

    /// Two deps make the same change: the second leaves the tip's tree as it is, and is named all the
    /// same, so the claim and the verdict follow one rule and the dependent's correct work passes.
    #[test]
    fn deps_that_make_the_same_change_are_both_named_and_the_verdict_passes() {
        let (root, repo, home, g, _) = origin_repo("same");
        let (wt1, wt2) = (run::worktree(&repo, "g", 1), run::worktree(&repo, "g", 2));
        let (w1, w2) = (wt1.display().to_string(), wt2.display().to_string());
        let s = sessions(&home, "same", &["d1", "d1", "d2"]);
        let (t1, t2, t3) = (open_work(&home, "t1", &[]), open_work(&home, "t2", &[]), open_work(&home, "t3", &["t1", "t2"]));
        let mut heads = Vec::new();
        for (t, sess) in [(&t1, &s[0]), (&t2, &s[1])] {
            claim(&home, &g, sess, t, false, &w1, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).expect("claimed");
            std::fs::write(wt1.join("c.txt"), "same\n").unwrap();
            heads.push(done_verified(&home, t, sess, &wt1));
        }
        claim(&home, &g, &s[2], &t3, false, &w2, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).expect("t3 claimed");
        let c = claim_line(&home, "t3", &s[2]);
        assert_eq!(c.deps_in, [format!("t1@{}", heads[0]), format!("t2@{}", heads[1])]);
        std::fs::write(wt2.join("d.txt"), "t3's work\n").unwrap();
        done_verified(&home, &t3, &s[2], &wt2);
        let events = lines(&home);
        let b = board::fold(&events);
        let done = events.iter().rfind(|e| e.ev == Ev::Done && e.task == "t3").unwrap();
        let a = verdict::attempt_of(&events, &b, "t3", &s[2], done.ms).unwrap();
        assert_eq!(missing(&home, &g, &events, &b, &a, &repo, verdict::SNAPSHOT_TIMEOUT), Vec::<String>::new());
        s.iter().for_each(|s| unbind(s));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A repo in scope the goal recorded no origin for (init ran in another, or outside git): the first
    /// claim there records the HEAD of its main checkout on the board, every claim there starts from it,
    /// and a verdict recomputes the start from it. A repo with no commit yet gets an empty root commit.
    #[test]
    fn the_first_claim_in_a_repo_without_an_origin_records_one_on_the_board() {
        let (root, _repo, home, g, _) = origin_repo("second");
        let lib = root.join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        git_ok(&lib, &["init", "-q", "-b", "main"]);
        std::fs::write(lib.join("lib.rs"), "fn f() {}\n").unwrap();
        git_ok(&lib, &["add", "-A"]);
        git_ok(&lib, &["commit", "-qm", "lib"]);
        let head = git_ok(&lib, &["rev-parse", "HEAD"]);
        let wt = run::worktree(&lib, "g", 1);
        git_ok(&lib, &["worktree", "add", "-q", "-b", "hive/g/1", wt.to_str().unwrap()]);
        // A commit on the drone's branch is no origin: the main checkout's HEAD is.
        std::fs::write(wt.join("left.txt"), "left over\n").unwrap();
        git_ok(&wt, &["add", "-A"]);
        git_ok(&wt, &["commit", "-qm", "left over"]);
        let s = sessions(&home, "second", &["d1", "d1"]);
        let (t, u) = (open_work(&home, "t", &[]), open_work(&home, "u", &["t"]));
        claim(&home, &g, &s[0], &t, false, &wt.display().to_string(), learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).expect("t claimed in lib");
        let b = board::fold(&lines(&home));
        assert_eq!(b.origin("lib"), Some(head.as_str()));
        let c = claim_line(&home, "t", &s[0]);
        assert_eq!(git_ok(&lib, &["rev-parse", &format!("{}^@", c.base)]), head, "its start is on lib's origin");
        assert!(!wt.join("left.txt").exists(), "reset to the start");
        std::fs::write(wt.join("t.txt"), "t\n").unwrap();
        done_verified(&home, &t, &s[0], &wt);
        claim(&home, &g, &s[1], &u, false, &wt.display().to_string(), learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).expect("u claimed in lib");
        let origins = lines(&home).into_iter().filter(|e| e.note.starts_with("origin ")).count();
        assert_eq!(origins, 1, "recorded once");
        std::fs::write(wt.join("u.txt"), "u\n").unwrap();
        done_verified(&home, &u, &s[1], &wt);
        let events = lines(&home);
        let b = board::fold(&events);
        let done = events.iter().rfind(|e| e.ev == Ev::Done && e.task == "u").unwrap();
        let a = verdict::attempt_of(&events, &b, "u", &s[1], done.ms).unwrap();
        assert_eq!(missing(&home, &g, &events, &b, &a, &wt, verdict::SNAPSHOT_TIMEOUT), Vec::<String>::new());
        // A repo with no commit yet: an empty root commit, the same on every node.
        let fresh = root.join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        git_ok(&fresh, &["init", "-q", "-b", "main"]);
        let v = open_work(&home, "v", &[]);
        let got = of(&home, &g, &s[0], &v, &events, &b, &fresh, verdict::SNAPSHOT_TIMEOUT).expect("a start in an empty repo");
        let o = board::fold(&lines(&home)).origin("fresh").expect("recorded").to_string();
        assert_eq!(git_ok(&fresh, &["rev-parse", &format!("{}^@", got.commit)]), o);
        assert_eq!(tree(&fresh, &o), "4b825dc642cb6eb9a060e54bf8d69288fbee4904", "the empty tree");
        s.iter().for_each(|s| unbind(s));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A clone that lacks the origin starts nothing there, and says why in monitoring (`borg top`, the
    /// Hive tab: the goal's alerts) until a start is had in that repo.
    #[test]
    fn a_clone_without_the_origin_says_so_in_monitoring_until_it_has_it() {
        let (root, repo, home, mut g, _) = origin_repo("nothere");
        let other = root.join("other").join("app");
        git_ok(&root, &["clone", "-q", "--no-local", repo.to_str().unwrap(), other.to_str().unwrap()]);
        std::fs::write(repo.join("README"), "later\n").unwrap();
        git_ok(&repo, &["commit", "-qam", "later"]);
        g.origin = vec![goal::Origin { repo: "app".into(), sha: git_ok(&repo, &["rev-parse", "HEAD"]) }];
        let t = open_work(&home, "t", &[]);
        let events = lines(&home);
        let b = board::fold(&events);
        let r = of(&home, &g, "s", &t, &events, &b, &other, verdict::SNAPSHOT_TIMEOUT);
        assert!(matches!(&r, Err(super::NoStart::Here(why)) if why.contains("is not in this clone")), "{r:?}");
        let alerts = view::alerts(&home, &g, &b);
        assert!(alerts.iter().any(|a| a.starts_with("no work starts in app on this node: the origin")), "{alerts:?}");
        let n = std::fs::metadata(super::super::dir(&home, "g").join("not-here.json")).unwrap().modified().unwrap();
        let _ = of(&home, &g, "s", &t, &events, &b, &other, verdict::SNAPSHOT_TIMEOUT);
        assert_eq!(std::fs::metadata(super::super::dir(&home, "g").join("not-here.json")).unwrap().modified().unwrap(), n, "written once");
        git_ok(&other, &["fetch", "-q", "origin"]);
        of(&home, &g, "s", &t, &events, &b, &other, verdict::SNAPSHOT_TIMEOUT).expect("a start once fetched");
        assert!(view::alerts(&home, &g, &b).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A repo with no commit on any node: the empty root origin one node recorded is built the same
    /// on another node from the same inputs, so work starts there too.
    #[test]
    fn an_empty_origin_another_node_recorded_is_built_here() {
        let (root, _repo, home, g, _) = origin_repo("empty");
        let (a, b) = (root.join("a").join("fresh"), root.join("b").join("fresh"));
        for d in [&a, &b] {
            std::fs::create_dir_all(d).unwrap();
            git_ok(d, &["init", "-q", "-b", "main"]);
        }
        let v = open_work(&home, "v", &[]);
        let events = lines(&home);
        let on_a = of(&home, &g, "s", &v, &events, &board::fold(&events), &a, verdict::SNAPSHOT_TIMEOUT).expect("a start on node a");
        let events = lines(&home);
        let on_b = of(&home, &g, "s", &v, &events, &board::fold(&events), &b, verdict::SNAPSHOT_TIMEOUT).expect("a start on node b");
        assert_eq!(on_a.commit, on_b.commit, "the same start on both nodes");
        assert!(super::not_here(&home, "g").is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    /// The origin is a commit no remote holds (made after the other clone): the dep's bundle carries it,
    /// so a claim on that clone fetches the deps' commits before it looks for the origin and starts.
    #[test]
    fn a_clone_without_the_origin_gets_it_from_the_deps_bundles() {
        let (root, repo, home, mut g, _) = origin_repo("bundled");
        mesh(&home);
        let other = root.join("other").join("app");
        git_ok(&root, &["clone", "-q", "--no-local", repo.to_str().unwrap(), other.to_str().unwrap()]);
        std::fs::write(repo.join("README"), "later\n").unwrap();
        git_ok(&repo, &["commit", "-qam", "later"]);
        g.origin = vec![goal::Origin { repo: "app".into(), sha: git_ok(&repo, &["rev-parse", "HEAD"]) }];
        let wt1 = run::worktree(&repo, "g", 1);
        let s = sessions(&home, "bundled", &["d1"]);
        let (t, u) = (open_work(&home, "t", &[]), open_work(&home, "u", &["t"]));
        claim(&home, &g, &s[0], &t, false, &wt1.display().to_string(), learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).expect("t claimed");
        std::fs::write(wt1.join("t.txt"), "t\n").unwrap();
        let head = done_verified(&home, &t, &s[0], &wt1);
        let events = lines(&home);
        let got = of(&home, &g, "s", &u, &events, &board::fold(&events), &other, verdict::SNAPSHOT_TIMEOUT);
        assert_eq!(got.map(|s| s.deps_in), Ok(vec![format!("t@{head}")]));
        assert!(super::not_here(&home, "g").is_empty(), "no alert");
        s.iter().for_each(|s| unbind(s));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The release goal's incident (0.3.0): drone d1 committed its blocked attempt at `release` in its
    /// reused worktree, claimed the replacement `release2` there and found the work in its tree already,
    /// so release2's accepted diff was empty and `b`, after release2, started without the work. A claim
    /// starts from the origin plus its deps' accepted changes: the leftover is pinned and reset away, the
    /// replacement's diff carries the work, and the dependent's base and tree hold it.
    #[test]
    fn a_blocked_attempt_left_in_a_reused_worktree_is_no_base_and_a_dependent_holds_every_deps_change() {
        let (root, repo, home, g, origin) = origin_repo("release2");
        let (wt1, wt2) = (run::worktree(&repo, "g", 1), run::worktree(&repo, "g", 2));
        let (w1, w2) = (wt1.display().to_string(), wt2.display().to_string());
        let pid = std::process::id();
        let (s1, s1b, s2) = (format!("rel-a-{pid}"), format!("rel-b-{pid}"), format!("rel-c-{pid}"));
        bind_as(&home, &s1, "g", "d1").unwrap();
        bind_as(&home, &s1b, "g", "d1").unwrap();
        bind_as(&home, &s2, "g", "d2").unwrap();
        let release = open_work(&home, "release", &[]);
        let dependent = open_work(&home, "b", &["release2"]);
        claim(&home, &g, &s1, &release, false, &w1, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).expect("release claimed");
        std::fs::create_dir_all(wt1.join("scripts")).unwrap();
        std::fs::write(wt1.join("scripts/check.sh"), "cargo test\n").unwrap();
        git_ok(&wt1, &["add", "-A"]);
        git_ok(&wt1, &["commit", "-qm", "release"]);
        let work = git_ok(&wt1, &["rev-parse", "HEAD"]);
        board::append(&tasks_path(&home, "g"), &[Event::new(Ev::Blocked, "release", &s1, learn::now_ms()).with_note("the check cannot pass")]).unwrap();
        let release2 = open_work(&home, "release2", &[]);
        claim(&home, &g, &s1b, &release2, false, &w1, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).expect("release2 claimed");
        let c = claim_line(&home, "release2", &s1b);
        assert_eq!(tree(&repo, &c.base), tree(&repo, &origin), "the blocked attempt's commit is in no later claim's base");
        assert_eq!(git_ok(&repo, &["rev-parse", &format!("{}^@", c.base)]), origin, "one parent: the origin");
        assert!(!wt1.join("scripts/check.sh").exists(), "the worktree is reset to the start");
        let kept = kept_before_reset(&home, "release2", &s1b);
        assert_eq!(git_ok(&repo, &["show", &format!("{kept}:scripts/check.sh")]), "cargo test", "what the reset discarded is kept");
        assert_eq!(git_ok(&repo, &["rev-parse", &format!("refs/borg/hive/g/{kept}")]), kept, "pinned: gc keeps it");
        // The replacement brings the work in: its accepted diff now carries it.
        git_ok(&wt1, &["cherry-pick", &work]);
        let head = done_verified(&home, &release2, &s1b, &wt1);
        assert_eq!(git_ok(&repo, &["diff", "--name-only", &c.base, &head]), "scripts/check.sh");
        claim(&home, &g, &s2, &dependent, false, &w2, learn::now_ms(), verdict::SNAPSHOT_TIMEOUT, None).expect("b claimed");
        let c2 = claim_line(&home, "b", &s2);
        assert_eq!(c2.deps_in, [format!("release2@{head}")]);
        assert_eq!(git_ok(&repo, &["show", &format!("{}:scripts/check.sh", c2.base)]), "cargo test", "the dependent's base holds the dep's change");
        assert_eq!(std::fs::read_to_string(wt2.join("scripts/check.sh")).unwrap(), "cargo test\n", "and so does its tree");
        // Monitoring (borg top, the Hive tab): each task's start, its deps' work, where the tree before is kept.
        let v = view::of(&home, "g", learn::now_ms()).unwrap();
        let row = |id: &str| v.tasks.iter().find(|t| t.id == id).unwrap().start.clone();
        assert!(row("release2").contains(&format!("worktree before the reset: {kept}")), "{}", row("release2"));
        assert!(row("b").starts_with(START_NOTE) && row("b").contains(&format!("release2@{head}")), "{}", row("b"));
        for s in [&s1, &s1b, &s2] {
            unbind(s);
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// The timer-at-0 half of the property: a claim with no time left computes no start, so it writes
    /// nothing, pins nothing and leaves the worktree as it was, leftovers included.
    #[test]
    fn a_claim_out_of_time_writes_nothing_and_leaves_the_tree() {
        let (root, repo, home, g, _) = origin_repo("zero");
        let wt1 = run::worktree(&repo, "g", 1);
        let s = format!("zero-{}", std::process::id());
        bind_as(&home, &s, "g", "d1").unwrap();
        let t1 = open_work(&home, "t1", &[]);
        std::fs::write(wt1.join("README"), "left over\n").unwrap();
        std::fs::write(wt1.join("left.txt"), "x\n").unwrap();
        let n = lines(&home).len();
        // Worth another try, never a refusal that keeps the task from this session for good.
        let r = claim(&home, &g, &s, &t1, false, &wt1.display().to_string(), learn::now_ms(), Duration::ZERO, None);
        assert!(matches!(r, Err(Refused::Later(_))), "{r:?}");
        assert_eq!(lines(&home).len(), n, "no claim line");
        assert_eq!(std::fs::read_to_string(wt1.join("README")).unwrap(), "left over\n");
        assert_eq!(std::fs::read_to_string(wt1.join("left.txt")).unwrap(), "x\n");
        assert_eq!(git_ok(&repo, &["for-each-ref", "refs/borg"]), "", "nothing pinned");
        // Out of time is no clone lacking the origin and no start this tree cannot take: no alert, and
        // the task stays offered to this session.
        let events = lines(&home);
        let r = of(&home, &g, &s, &t1, &events, &board::fold(&events), &wt1, Duration::ZERO);
        assert!(matches!(r, Err(super::NoStart::Later(_))), "{r:?}");
        assert!(super::not_here(&home, "g").is_empty(), "no alert");
        unbind(&s);
        let _ = std::fs::remove_dir_all(root);
    }
}
