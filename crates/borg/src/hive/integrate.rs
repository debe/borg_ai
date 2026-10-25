//! Integration: the combined tree of a goal's accepted work, checked by the oracle before the goal counts
//! as reached (docs/design/collective.md "Board", "Roles"). An `integrate` task is an ordinary board task
//! (open, claim, lease, verdict), run in process by `borg hive run`'s drone threads or by `borg hive
//! integrate`, never by a model session (mod.rs `may_claim`): the work is mechanical, a suite outlasts
//! any stop hook, and a model would judge a tree it did not build.
//!
//! What is merged is the accepted attempts, never a drone's branch (of a group, hive/group.rs, only its
//! best: the K attempts are one task's, scored by the hidden oracle first when the goal has one): for
//! each verified work task the diff
//! from its accepted claim's `base` to its done's `head` (verdict.rs `accepted`), in order (by `deps`,
//! then by when it was verified, then by id), each applied with `git merge-tree --merge-base` (git 2.40+)
//! onto the tree before it, from the goal's origin: the newest common ancestor of the HEADs every work
//! claim started from, so no drone's own commits (a rejected attempt it committed, say) are in it. Each
//! step is a commit with a fixed identity and date, so the same attempts give the same commits,
//! conflict markers included: the next round replays them to the same tree, and a fix made on it
//! applies cleanly. Changes in a worktree that no claim and done cover never enter. The result is kept
//! at `refs/heads/hive/<goal>/integrated` (it survives `gc`, and a clone carries it). No git call runs
//! a hook: a drone can plant one in the shared `.git/hooks`.
//!
//! The verdict: a conflict no later attempt resolves (one built on the tree with its markers, a fix
//! task's, that changes the path) is `rejected` at once; else the goal's oracle and
//! every integrated task's own checks run in a scratch worktree (`<goal>/integrate/<task>`, hidden.rs
//! `with_checkout`) while a thread renews the lease: all pass, `verified`; one fails, `rejected`. A
//! rejection opens the fix task `<integrate task>.fix` in the same write, which starts `from` the
//! combined tree (mod.rs `claim` resets a runner worktree to it; any other session claims it in a
//! checkout of it, `git worktree add <dir> <commit>`); its accepted attempt is merged last
//! next round. A missing snapshot or a git that cannot merge is `blocked`, for the engineer. Nothing is
//! credited to lessons: an integration is no verdict on any one attempt.
//!
//! Across machines the commits travel as bundles beside the board (commits.rs): a goal integrates on a
//! node whose repo holds every accepted attempt's snapshots, or takes them from their bundles
//! ([`absent`]); elsewhere nothing is opened, claimed or written, and the caller says which work is on
//! which node. A snapshot
//! missing on the node that made it is still `blocked`.

use std::path::Path;
use std::time::{Duration, Instant};

use super::board::{Board, Ev, Event, Role, Status, Task, TaskState};
use super::goal::Check;
use super::verdict::{self, Attempt};
use super::{Goal, TaskId, board, group, hidden, oracle};
use crate::{learn, queen, runner, stats};

/// Each git call of a merge.
const GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// A whole merge: every attempt's merge-tree and commit.
const MERGE_TIMEOUT: Duration = Duration::from_secs(600);
/// The scratch checkouts' directory, under the goal's.
pub const DIR: &str = "integrate";
/// What a rejection's note keeps of a failed check's output.
const NOTE_TAIL: usize = 400;

/// What one integration did: its task, the combined tree's commit (None when it got no merge), and the
/// verdict (`Err`: why it was rejected or blocked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub task: TaskId,
    pub head: Option<String>,
    pub verdict: Result<(), String>,
}

/// A merge of attempts: the last step's commit, and the conflicts no later attempt resolved (the
/// attempt's work task and its conflicted paths).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub head: String,
    pub conflicts: Vec<(TaskId, Vec<String>)>,
}

/// `git -C repo args` with `env`, within `GIT_TIMEOUT` and before `deadline`, no hook run (`update-ref`
/// runs `reference-transaction`; git.rs): (exit code, stdout).
fn git(repo: &Path, args: &[&str], env: &[(&str, &str)], deadline: Instant) -> Result<(i32, String), String> {
    let left = deadline.saturating_duration_since(Instant::now()).min(GIT_TIMEOUT);
    if left.is_zero() {
        return Err("the merge ran out of time".into());
    }
    let mut cmd = super::git::command(repo);
    cmd.args(args).envs(env.iter().copied());
    match runner::run_capped_status(cmd, None, left) {
        Some((s, out)) => Ok((s.code().unwrap_or(-1), String::from_utf8_lossy(&out).into_owned())),
        None => Err(format!("git {} did not finish", args.first().unwrap_or(&""))),
    }
}

/// `git` that must succeed: its trimmed stdout.
fn git_ok(repo: &Path, args: &[&str], env: &[(&str, &str)], deadline: Instant) -> Result<String, String> {
    match git(repo, args, env, deadline)? {
        (0, out) => Ok(out.trim().to_string()),
        (code, _) => Err(format!("git {} failed (exit {code})", args.first().unwrap_or(&""))),
    }
}

/// The HEAD a claim's `base` snapshot was taken on (its parent; a snapshot of a repo without commits
/// has none, and a fix task's base is a commit of its own).
fn head_at(repo: &Path, base: &str, deadline: Instant) -> Result<String, String> {
    git_ok(repo, &["rev-parse", "--verify", "-q", &format!("{base}^")], &[], deadline)
        .or_else(|_| git_ok(repo, &["rev-parse", "--verify", "-q", &format!("{base}^{{commit}}")], &[], deadline))
}

/// The origin of a merge: the newest common ancestor of the HEADs `bases` (every work claim's, the
/// attempts' among them) were taken on. A drone's commits after its first claim are past it.
fn origin(repo: &Path, bases: &[&str], deadline: Instant) -> Result<String, String> {
    let mut heads: Vec<String> = Vec::new();
    for b in bases {
        let h = head_at(repo, b, deadline)?;
        if !heads.contains(&h) {
            heads.push(h);
        }
    }
    if heads.len() < 2 {
        return heads.pop().ok_or_else(|| "no accepted attempt to merge".into());
    }
    let args: Vec<&str> = ["merge-base", "--octopus"].into_iter().chain(heads.iter().map(String::as_str)).collect();
    git_ok(repo, &args, &[], deadline).map_err(|_| "the claims' trees share no commit to merge onto".into())
}

/// Applies `attempts` in order onto the origin of `claims` (the bases of every work claim) and the
/// attempts' own, each step a commit (parents: the tree so far and the attempt's head) with borg's
/// identity and the attempt's done time as its date, all `within`. A step that conflicts commits the
/// tree with conflict markers; a path of it counts as resolved when a later attempt built on that
/// step (its base descends from the step's commit: a fix task's) changes it. An attempt from a tree
/// without the markers merges around them and leaves them in.
///
/// # Errors
/// No attempts, a git call that fails or runs out of time, or a git without `merge-tree --merge-base`.
pub(crate) fn merge(repo: &Path, claims: &[&str], attempts: &[Attempt], within: Duration) -> Result<Merged, String> {
    let deadline = Instant::now() + within;
    if attempts.is_empty() {
        return Err("no accepted attempt to merge".into());
    }
    let bases: Vec<&str> = claims.iter().copied().chain(attempts.iter().map(|a| a.base.as_str())).collect();
    let mut t = origin(repo, &bases, deadline)?;
    let mut steps: Vec<String> = Vec::new();
    let mut conflicted: Vec<(usize, Vec<String>)> = Vec::new();
    for (i, a) in attempts.iter().enumerate() {
        let base = format!("--merge-base={}", a.base);
        let (code, out) = git(repo, &["merge-tree", "--write-tree", "-z", "--name-only", "--no-messages", &base, &t, &a.head], &[], deadline)?;
        if code != 0 && code != 1 {
            return Err(format!("git merge-tree --merge-base failed on {} (exit {code}; it needs git 2.40+)", a.work));
        }
        // `<tree>\0<path>\0...\0\0`: the conflicted paths (exit 1) up to the empty field.
        let mut fields = out.split('\0');
        let tree = fields.next().unwrap_or_default().trim().to_string();
        if code == 1 {
            let mut paths: Vec<String> = fields.take_while(|p| !p.is_empty()).map(String::from).collect();
            paths.dedup();
            conflicted.push((i, paths));
        }
        let date = format!("@{} +0000", a.done_ms.max(0) / 1000);
        let who = [
            ("GIT_AUTHOR_NAME", "borg"),
            ("GIT_AUTHOR_EMAIL", "borg@localhost"),
            ("GIT_COMMITTER_NAME", "borg"),
            ("GIT_COMMITTER_EMAIL", "borg@localhost"),
            ("GIT_AUTHOR_DATE", date.as_str()),
            ("GIT_COMMITTER_DATE", date.as_str()),
        ];
        let msg = format!("integrate {}", a.work);
        t = git_ok(repo, &["commit-tree", &tree, "-p", &t, "-p", &a.head, "-m", &msg], &who, deadline)?;
        steps.push(t.clone());
    }
    let mut conflicts = Vec::new();
    for (i, paths) in conflicted {
        let mut later: Vec<String> = Vec::new();
        for a in &attempts[i + 1..] {
            let (on, _) = git(repo, &["merge-base", "--is-ancestor", &steps[i], &a.base], &[], deadline)?;
            if on != 0 {
                continue;
            }
            let changed = verdict::changes(repo, &a.base, &a.head).ok_or_else(|| format!("cannot diff the attempt of {}", a.work))?;
            later.extend(changed.into_iter().map(|c| c.path));
        }
        let open: Vec<String> = paths.into_iter().filter(|p| !later.contains(p)).collect();
        if !open.is_empty() {
            conflicts.push((attempts[i].work.clone(), open));
        }
    }
    Ok(Merged { head: t, conflicts })
}

/// The accepted work tasks in merge order: a task after its dependencies, else by when it was verified,
/// then by id (a fix task, verified last, comes last).
fn ordered<'a>(works: &[&'a TaskState]) -> Vec<&'a TaskState> {
    let key = |s: &TaskState| (if let Status::Verified { ms, .. } = s.status { ms } else { i64::MAX }, s.task.id.clone());
    let mut left: Vec<&TaskState> = works.to_vec();
    let mut out: Vec<&TaskState> = Vec::new();
    while !left.is_empty() {
        let ready = |s: &&TaskState| s.task.deps.iter().all(|d| !left.iter().any(|o| &o.task.id == d));
        // A dependency cycle (the fold never verifies one) cannot stall it: then the oldest goes.
        let i = (0..left.len())
            .filter(|i| ready(&left[*i]))
            .min_by_key(|i| key(left[*i]))
            .unwrap_or_else(|| (0..left.len()).min_by_key(|i| key(left[*i])).unwrap_or(0));
        out.push(left.remove(i));
    }
    out
}

/// The checks of a combined tree: the goal's oracle, then each task's own, each (name, command) once.
fn checks_of(g: &Goal, tasks: &[&Task]) -> Vec<Check> {
    let mut all: Vec<Check> = Vec::new();
    for c in g.oracle.iter().chain(tasks.iter().flat_map(|t| t.accept.iter())) {
        if !all.iter().any(|o| o.name == c.name && o.cmd == c.cmd) {
            all.push(c.clone());
        }
    }
    all
}

/// The last `n` characters of `text`.
fn tail(text: &str, n: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(n)).collect()
}

/// The accepted attempts of `g` whose snapshots `repo` lacks, work done on another node: (work task,
/// its node, its head). Empty when every one is here (the git call runs only for another node's work),
/// one its bundle brought here included (commits.rs `fetch`).
pub fn absent(home: &Path, g: &Goal, repo: &Path) -> Vec<(TaskId, String, String)> {
    let events = board::read(&super::tasks_path(home, &g.id));
    let b = board::fold(&events);
    let scores = group::scores(home, &g.id);
    group::merged(&b, |v| scores.get(v).copied())
        .into_iter()
        .filter_map(|s| verdict::accepted(&events, &b, &s.task.id))
        .filter(|a| !super::is_local(home, &a.node) && !a.base.is_empty() && !a.head.is_empty())
        .filter(|a| !super::commits::fetch(home, &g.id, a, repo))
        .map(|a| (a.work, a.node, a.head))
        .collect()
}

/// Why `absent` work stops an integration here, for the engineer or the runner.
pub fn absent_reason(absent: &[(TaskId, String, String)]) -> String {
    let parts: Vec<String> = absent.iter().map(|(t, node, _)| format!("{t} from node {}", node.chars().take(8).collect::<String>())).collect();
    format!("integration needs {}: integrate on a node that holds these commits (or fetch them)", parts.join(", "))
}

/// The integrate task an integration of `b` would take at `now`: one open to a claim while every work
/// task is verified and the goal is not finished (its integrator died, an engineer opened or reopened
/// one), else, when an integration is due (board.rs `integration_due`), a new one (`true`: to open).
fn due(b: &Board, now: i64) -> Option<(TaskId, bool)> {
    let works_verified = b.tasks.values().filter(|s| s.task.role != Role::Integrate).all(|s| matches!(s.status, Status::Verified { .. }));
    let pending = b.tasks.values().find(|s| s.task.role == Role::Integrate && s.claimable(now)).map(|s| s.task.id.clone());
    match pending {
        Some(id) if works_verified && !b.finished() => Some((id, false)),
        _ if board::integration_due(b) => Some((board::integrate_id(b), true)),
        _ => None,
    }
}

/// Why `g` cannot integrate in `repo` at `now` although an integration is due: an accepted attempt's
/// commits are on another node only (`absent`). None when none is due or every commit is here.
pub fn elsewhere(home: &Path, g: &Goal, repo: &Path, now: i64) -> Option<String> {
    due(&board::fold(&board::read(&super::tasks_path(home, &g.id))), now)?;
    let a = absent(home, g, repo);
    (!a.is_empty()).then(|| absent_reason(&a))
}

/// Integrates goal `g` when it is due (`due`), as `drone` (session `integrate-<drone>-<ms>`), in `repo`
/// (where the snapshots are): the claim, the merge, the oracle on the combined tree, the verdict. None
/// when nothing is due, another runner won the claim, or an accepted attempt's commits are on another
/// node only (`elsewhere` says so; nothing is written).
pub fn run(home: &Path, g: &Goal, repo: &Path, drone: &str, now: i64) -> Option<Outcome> {
    let path = super::tasks_path(home, &g.id);
    let b = board::fold(&board::read(&path));
    let scores = group::scores(home, &g.id);
    let accepted = group::merged(&b, |v| scores.get(v).copied()).len();
    let session = format!("integrate-{drone}-{now}");
    let (id, new) = due(&b, now)?;
    let mut lines = Vec::new();
    if new {
        lines.push(Event::open(Task { title: format!("integrate {accepted} verified task(s)"), ..board_task(&id) }, &session, now));
    }
    if !absent(home, g, repo).is_empty() {
        return None;
    }
    // Two runners racing write the same open (the fold keeps the first) and a claim each: one wins.
    lines.extend([Event::new(Ev::Claim, &id, &session, now).with_drone(drone), Event::new(Ev::Heartbeat, &id, &session, now)]);
    super::write(home, &g.id, lines).ok()?;
    std::thread::sleep(super::CLAIM_SETTLE);
    let events = board::read(&path);
    let b = board::fold(&events);
    if b.tasks.get(&id).and_then(|s| s.live(now)).is_none_or(|(by, _, _)| by != session) {
        return None;
    }
    let i = Integrator { home, g, id: &id, session: &session };
    Some(i.integrate(repo, &events, &b))
}

/// One claimed integration.
struct Integrator<'a> {
    home: &'a Path,
    g: &'a Goal,
    id: &'a str,
    session: &'a str,
}

impl Integrator<'_> {
    fn integrate(&self, repo: &Path, events: &[Event], b: &Board) -> Outcome {
        // A group's best attempt is the one the hidden oracle passed: its accepted attempts are scored
        // first (hidden.rs; each attempt once), while the lease is renewed.
        if self.g.hidden.is_some() && group::contested(b) {
            super::renewing(self.home, &self.g.id, self.id, self.session, super::HEARTBEAT, || {
                hidden::score_pending(self.home, self.g, repo, usize::MAX, learn::now_ms())
            });
        }
        let scores = group::scores(self.home, &self.g.id);
        let works = ordered(&group::merged(b, |v| scores.get(v).copied()));
        let mut attempts: Vec<Attempt> = Vec::new();
        for w in &works {
            match verdict::accepted(events, b, &w.task.id) {
                Some(a) if !a.base.is_empty() && !a.head.is_empty() => attempts.push(a),
                _ => return self.blocked(repo, &format!("{} has no snapshot of its accepted attempt", w.task.id)),
            }
        }
        // Every work claim's base, a rejected attempt's too: the origin is before all of them. Another
        // node's claim whose tree never came here (no accepted attempt of it, no bundle) is left out.
        let claims: Vec<&str> = events
            .iter()
            .filter(|e| e.ev == Ev::Claim && !e.base.is_empty())
            .filter(|e| b.tasks.get(&e.task).is_some_and(|s| !matches!(s.task.role, Role::Verify | Role::Integrate)))
            .filter(|e| super::is_local(self.home, &e.node) || verdict::present(repo, &[&e.base], verdict::DIFF_TIMEOUT))
            .map(|e| e.base.as_str())
            .collect();
        let m = match merge(repo, &claims, &attempts, MERGE_TIMEOUT) {
            Ok(m) => m,
            Err(e) => return self.blocked(repo, &e),
        };
        let reference = format!("refs/heads/hive/{}/integrated", self.g.id);
        let _ = git(repo, &["update-ref", &reference, &m.head], &[], Instant::now() + GIT_TIMEOUT);
        let ids: Vec<&str> = works.iter().map(|w| w.task.id.as_str()).collect();
        let tasks: Vec<&Task> = works.iter().map(|w| &w.task).collect();
        let checks = checks_of(self.g, &tasks);
        // Checks another node wrote run only once approved here; a fix task carrying them would approve
        // them by being opened here (hive/mod.rs `write`).
        let approvals = oracle::Approvals::load(self.home, &self.g.id);
        let pending: Vec<&Check> = checks.iter().filter(|c| !approvals.allows(c)).collect();
        if !pending.is_empty() {
            return self.blocked(repo, &oracle::refusal(&self.g.id, &pending));
        }
        if !m.conflicts.is_empty() {
            let parts: Vec<String> = m
                .conflicts
                .iter()
                .map(|(t, paths)| {
                    let earlier: Vec<&str> = ids.iter().copied().take_while(|i| i != t).collect();
                    format!("{t} onto {} in {}", if earlier.is_empty() { "the base".into() } else { earlier.join(", ") }, paths.join(", "))
                })
                .collect();
            let paths: Vec<&str> = m.conflicts.iter().flat_map(|(_, p)| p.iter().map(String::as_str)).collect();
            let note = format!("conflict: {}", parts.join("; "));
            return self.rejected(repo, &m.head, &note, &format!("conflict in {}", paths.join(", ")), &tasks, checks);
        }
        let probe = Task { accept: checks.clone(), ..board_task(self.id) };
        // The checks run for minutes: the lease is renewed until they return.
        let ran = super::renewing(self.home, &self.g.id, self.id, self.session, super::HEARTBEAT, || {
            hidden::with_checkout(&super::dir(self.home, &self.g.id).join(DIR), repo, &m.head, self.id, |wt| oracle::run(self.home, self.g, &probe, wt, None))
        });
        let v = match ran {
            Ok(v) => v,
            Err(e) => return self.blocked(repo, &e),
        };
        let ms = learn::now_ms();
        match &v {
            oracle::Verdict::Pass => {
                let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
                let note = format!("merged {}; {} pass", ids.join(", "), if names.is_empty() { "no checks".into() } else { names.join(", ") });
                let e = Event { head: m.head.clone(), ..Event::new(Ev::Verified, self.id, self.session, ms).with_note(&note) };
                if let Err(e) = super::write(self.home, &self.g.id, vec![e]) {
                    return Outcome { task: self.id.into(), head: Some(m.head), verdict: Err(e.to_string()) };
                }
                self.count(repo, &note, true);
                Outcome { task: self.id.into(), head: Some(m.head), verdict: Ok(()) }
            }
            oracle::Verdict::Fail { check, tail: t } => {
                let _ = oracle::log(self.home, &self.g.id, self.id, &v, ms);
                let note = format!("{check} fails: {}", tail(t.trim_end(), NOTE_TAIL));
                self.rejected(repo, &m.head, &note, &format!("{check} fails"), &tasks, checks)
            }
            oracle::Verdict::Timeout { check, after } => {
                let _ = oracle::log(self.home, &self.g.id, self.id, &v, ms);
                let note = format!("{check} timed out after {} s", after.as_secs());
                self.rejected(repo, &m.head, &note, &format!("{check} times out"), &tasks, checks)
            }
        }
    }

    /// `rejected` on the combined tree `head`, with the fix task it opens in the same write: from
    /// `head`, scoped to the union of the tasks' scopes (none when one has none), the same `checks`.
    fn rejected(&self, repo: &Path, head: &str, note: &str, what: &str, tasks: &[&Task], checks: Vec<Check>) -> Outcome {
        let ms = learn::now_ms();
        let scoped = tasks.iter().all(|t| !t.files.is_empty());
        let mut files: Vec<String> = if scoped { tasks.iter().flat_map(|t| t.files.iter().cloned()).collect() } else { Vec::new() };
        files.sort();
        files.dedup();
        let fix = Task {
            title: format!("fix the integrated tree: {what}"),
            role: Role::Work,
            files,
            accept: checks,
            from: Some(head.to_string()),
            ..board_task(&format!("{}.fix", self.id))
        };
        let e = Event { head: head.to_string(), ..Event::new(Ev::Rejected, self.id, self.session, ms).with_note(note) };
        let why = match super::write(self.home, &self.g.id, vec![e, Event::open(fix, self.session, ms).with_note(note)]) {
            Ok(()) => note.to_string(),
            Err(e) => e.to_string(),
        };
        self.count(repo, note, false);
        Outcome { task: self.id.into(), head: Some(head.into()), verdict: Err(why) }
    }

    /// `blocked` with `why`: what an engineer has to fix (a snapshot, git) before it is reopened.
    fn blocked(&self, repo: &Path, why: &str) -> Outcome {
        let _ = super::write(self.home, &self.g.id, vec![Event::new(Ev::Blocked, self.id, self.session, learn::now_ms()).with_note(why)]);
        self.count(repo, why, false);
        Outcome { task: self.id.into(), head: None, verdict: Err(why.into()) }
    }

    fn count(&self, repo: &Path, note: &str, pass: bool) {
        stats::count(self.home, stats::HIVE_INTEGRATE, self.session, |ev| {
            ev.why = self.g.id.clone();
            ev.repo = queen::repo_name(&repo.display().to_string());
            ev.n = u64::from(pass);
            ev.text = stats::excerpt(&format!("{}: {note}", self.id), 160);
        });
    }
}

/// A task named `id` with nothing else set.
fn board_task(id: &str) -> Task {
    Task {
        id: id.into(),
        role: Role::Integrate,
        title: String::new(),
        repo: String::new(),
        files: Vec::new(),
        deps: Vec::new(),
        accept: Vec::new(),
        of: None,
        not_by: Vec::new(),
        flags: Vec::new(),
        from: None,
        group: None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.email=e@x", "-c", "user.name=e"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A repo with `f.txt` (lines 1..6) committed, three drone worktrees, and a store: (root, repo, home).
    fn setup(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("borg-hive-integrate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (repo, home) = (root.join("app"), root.join("store"));
        std::fs::create_dir_all(&repo).unwrap();
        sh(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("f.txt"), "1\n2\n3\n4\n5\n6\n").unwrap();
        sh(&repo, &["add", "-A"]);
        sh(&repo, &["commit", "-qm", "base"]);
        for n in 1..=3 {
            let wt = super::super::run::worktree(&repo, "g", n);
            sh(&repo, &["worktree", "add", "-q", "-b", &format!("hive/g/{n}"), wt.to_str().unwrap()]);
        }
        (root, repo, home)
    }

    /// `super::run` with every check of the goal and its board approved here, as the engineer's `borg
    /// hive approve` does: these tests write the board raw, as another node's lines would arrive.
    fn run(home: &Path, g: &Goal, repo: &Path, drone: &str, now: i64) -> Option<Outcome> {
        let b = board::fold(&board::read(&super::super::tasks_path(home, &g.id)));
        let all: Vec<Check> = oracle::unapproved(home, g, &b).into_iter().map(|(_, c)| c.clone()).collect();
        oracle::approve(home, &g.id, &all, 0).unwrap();
        super::run(home, g, repo, drone, now)
    }

    /// A task's check another node wrote and nobody approved here: the integration is blocked before
    /// any merge result is checked, nothing runs, and no fix task carries the check (it would be
    /// approved by being opened here).
    #[test]
    fn an_unapproved_check_blocks_the_integration_and_never_runs() {
        let (root, repo, home) = setup("unapproved");
        let ran = root.join("ran");
        let g = goal(&["unit=true"]);
        oracle::approve(&home, "g", &g.oracle, 0).unwrap();
        accepted(&home, work("t1", &["a.txt"], &[&format!("remote=touch {}", ran.display())]), &wt(&repo, 1), "s1", write("a.txt", "a\n"));
        let o = super::run(&home, &g, &repo, "d1", learn::now_ms()).unwrap();
        let b = fold(&home);
        assert!(
            matches!(&b.tasks["integrate.1"].status, Status::Blocked { note, .. } if note.contains("not approved") && note.contains("borg hive approve g")),
            "{o:?}"
        );
        assert!(!ran.exists(), "nothing ran");
        assert!(!b.tasks.values().any(|s| s.task.from.is_some()), "no fix task");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn wt(repo: &Path, n: usize) -> PathBuf {
        super::super::run::worktree(repo, "g", n)
    }

    fn goal(oracle: &[&str]) -> Goal {
        Goal {
            v: 1,
            id: "g".into(),
            spec: "s".into(),
            oracle: oracle.iter().map(|c| Check::parse(c, 30).unwrap()).collect(),
            budget_usd: 1.0,
            repos: vec![],
            author: String::new(),
            created_ms: 0,
            sig: String::new(),
            hidden: None,
            held_out: false,
        }
    }

    fn work(id: &str, files: &[&str], accept: &[&str]) -> Task {
        Task {
            role: Role::Work,
            title: format!("do {id}"),
            files: files.iter().map(|f| f.to_string()).collect(),
            accept: accept.iter().map(|c| Check::parse(c, 30).unwrap()).collect(),
            ..board_task(id)
        }
    }

    /// `t` opened, claimed in `dir` by `author` (its base: a snapshot then, or `from`), changed by `edit`,
    /// done (its head: a snapshot then), and verified by `verifier`: the board lines, at real times.
    fn accepted(home: &Path, t: Task, dir: &Path, author: &str, edit: impl FnOnce(&Path)) {
        let path = super::super::tasks_path(home, "g");
        let ms = learn::now_ms();
        let mut claim = Event::new(Ev::Claim, &t.id, author, ms + 1);
        claim.base = t.from.clone().unwrap_or_else(|| verdict::snapshot(dir, verdict::SNAPSHOT_TIMEOUT).unwrap());
        edit(dir);
        let mut done = Event::new(Ev::Done, &t.id, author, ms + 2);
        done.head = verdict::snapshot(dir, verdict::SNAPSHOT_TIMEOUT).unwrap();
        let v = Task { role: Role::Verify, of: Some(t.id.clone()), not_by: vec![author.into()], ..board_task(&format!("{}.v1", t.id)) };
        let vid = v.id.clone();
        let opened = if t.from.is_some() { Vec::new() } else { vec![Event::open(t.clone(), "engineer:e", ms)] };
        let lines = [
            opened,
            vec![
                claim,
                done,
                Event::open(v, author, ms + 2),
                Event::new(Ev::Claim, &vid, "verifier", ms + 3),
                Event::new(Ev::Verified, &vid, "verifier", ms + 4),
            ],
        ]
        .concat();
        board::append(&path, &lines).unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }

    fn write(file: &str, body: &str) -> impl FnOnce(&Path) {
        let (file, body) = (file.to_string(), body.to_string());
        move |dir: &Path| std::fs::write(dir.join(file), body).unwrap()
    }

    fn fold(home: &Path) -> Board {
        board::fold(&board::read(&super::super::tasks_path(home, "g")))
    }

    fn show(repo: &Path, head: &str, file: &str) -> String {
        sh(repo, &["show", &format!("{head}:{file}")])
    }

    #[test]
    fn disjoint_attempts_merge_and_the_oracle_runs_in_a_scratch_checkout() {
        let (root, repo, home) = setup("disjoint");
        // `update-ref` runs reference-transaction, a checkout post-checkout: neither may run.
        for name in ["post-checkout", "reference-transaction"] {
            use std::os::unix::fs::PermissionsExt;
            let hook = repo.join(".git/hooks").join(name);
            std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", root.join("hook-ran").display())).unwrap();
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let where_ = root.join("where");
        let g = goal(&[&format!("unit=test -f a.txt && test -f b.txt && pwd > {}", where_.display())]);
        let own = root.join("own");
        accepted(&home, work("t1", &["a.txt"], &[&format!("own=touch {}", own.display())]), &wt(&repo, 1), "s1", write("a.txt", "a\n"));
        accepted(&home, work("t2", &["b.txt"], &[]), &wt(&repo, 2), "s2", write("b.txt", "b\n"));
        let o = run(&home, &g, &repo, "d1", learn::now_ms()).unwrap();
        assert_eq!(o.verdict, Ok(()));
        let b = fold(&home);
        assert!(b.finished(), "{:?}", b.tasks["integrate.1"]);
        let head = b.integrated.clone().unwrap().head;
        assert_eq!((show(&repo, &head, "a.txt"), show(&repo, &head, "b.txt")), ("a".into(), "b".into()));
        assert_eq!(sh(&repo, &["rev-parse", "refs/heads/hive/g/integrated"]), head, "the ref keeps the combined tree");
        let ran_in = std::fs::read_to_string(&where_).unwrap();
        // `pwd` names the real path: macOS's temp dir is a symlink into /private.
        let scratch = std::fs::canonicalize(super::super::dir(&home, "g").join(DIR)).unwrap();
        assert!(ran_in.trim().starts_with(&scratch.display().to_string()), "{ran_in}");
        assert!(own.exists(), "a task's own checks run on the combined tree too");
        assert_eq!(std::fs::read_dir(super::super::dir(&home, "g").join(DIR)).unwrap().count(), 0, "the scratch checkout is removed");
        assert!(!root.join("hook-ran").exists(), "a hook planted in the repo never runs");
        for n in 1..=3 {
            assert!(!wt(&repo, n).join("b.txt").exists() || n == 2, "no worktree is touched");
        }
        assert!(run(&home, &g, &repo, "d1", learn::now_ms()).is_none(), "finished: nothing is due");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stray_changes_no_claim_covers_never_reach_the_combined_tree() {
        // The W2 D3 probe: drone 3 lost its claim and edited t1's file anyway; only the board's
        // accepted attempts are merged, so its edit is not in the result.
        let (root, repo, home) = setup("stray");
        let g = goal(&["unit=test \"$(grep -c color_name f.txt)\" -eq 1"]);
        accepted(&home, work("t1", &["f.txt"], &[]), &wt(&repo, 1), "s1", write("f.txt", "1\ncolor_name\n2\n3\n4\n5\n6\n"));
        accepted(&home, work("t2", &["b.txt"], &[]), &wt(&repo, 2), "s2", write("b.txt", "b\n"));
        std::fs::write(wt(&repo, 3).join("f.txt"), "1\n2\n3\n4\n5\ncolor_name\n6\n").unwrap();
        std::fs::write(wt(&repo, 3).join("stray.txt"), "x\n").unwrap();
        sh(&wt(&repo, 3), &["add", "-A"]);
        sh(&wt(&repo, 3), &["commit", "-qm", "drone 3"]);
        let o = run(&home, &g, &repo, "d1", learn::now_ms()).unwrap();
        assert_eq!(o.verdict, Ok(()));
        let head = o.head.unwrap();
        assert_eq!(show(&repo, &head, "f.txt"), "1\ncolor_name\n2\n3\n4\n5\n6");
        assert!(!sh(&repo, &["ls-tree", "-r", "--name-only", &head]).contains("stray.txt"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_clean_text_merge_that_breaks_the_oracle_is_rejected_with_a_fix_task() {
        let (root, repo, home) = setup("semantic");
        let g = goal(&["unit=n=$(grep -c X f.txt); echo \"found $n X\"; test $n -eq 1"]);
        accepted(&home, work("t1", &["f.txt"], &[]), &wt(&repo, 1), "s1", write("f.txt", "1\nX\n2\n3\n4\n5\n6\n"));
        accepted(&home, work("t2", &["f.txt"], &[]), &wt(&repo, 2), "s2", write("f.txt", "1\n2\n3\n4\n5\n6\nX\n"));
        let o = run(&home, &g, &repo, "d2", learn::now_ms()).unwrap();
        let why = o.verdict.unwrap_err();
        assert!(why.starts_with("unit fails:") && why.contains("found 2 X"), "{why}");
        let head = o.head.unwrap();
        let b = fold(&home);
        assert!(!b.finished() && b.integrated.is_none());
        let fix = &b.tasks["integrate.1.fix"];
        assert_eq!((fix.status.clone(), fix.task.role, fix.task.from.as_deref()), (Status::Open, Role::Work, Some(head.as_str())));
        assert_eq!((fix.task.files.as_slice(), fix.notes.as_slice()), (["f.txt".to_string()].as_slice(), [why.clone()].as_slice()));
        assert_eq!(fix.task.accept, g.oracle, "the combined checks");
        assert!(fix.task.title.contains("unit fails"), "{}", fix.task.title);
        assert_eq!(sh(&repo, &["rev-parse", "refs/heads/hive/g/integrated"]), head, "the failing tree is kept for the fix");
        let logs = std::fs::read_dir(super::super::dir(&home, "g").join("oracle")).unwrap().count();
        assert_eq!(logs, 1, "the full tail is logged");
        assert!(run(&home, &g, &repo, "d2", learn::now_ms()).is_none(), "the fix is open: nothing is due");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_conflict_is_rejected_without_running_the_oracle_and_its_fix_replays_to_a_pass() {
        let (root, repo, home) = setup("conflict");
        let ran = root.join("ran");
        let g = goal(&[&format!("unit=touch {}; grep -q one s.txt && grep -q two s.txt && ! grep -q '<<<<<<<' s.txt", ran.display())]);
        accepted(&home, work("t1", &["s.txt"], &[]), &wt(&repo, 1), "s1", write("s.txt", "one\n"));
        accepted(&home, work("t2", &["s.txt"], &[]), &wt(&repo, 2), "s2", write("s.txt", "two\n"));
        let o = run(&home, &g, &repo, "d1", learn::now_ms()).unwrap();
        let why = o.verdict.unwrap_err();
        assert_eq!(why, "conflict: t2 onto t1 in s.txt");
        assert!(!ran.exists(), "no oracle run on a conflicted tree");
        let m = o.head.unwrap();
        assert!(show(&repo, &m, "s.txt").contains("<<<<<<<"), "the markers are in the tree the fix starts from");
        let fix = fold(&home).tasks["integrate.1.fix"].task.clone();
        assert!(fix.title.contains("conflict in s.txt"), "{}", fix.title);
        // The fix: a worktree reset to the combined tree, the markers resolved.
        let w3 = wt(&repo, 3);
        sh(&w3, &["checkout", "-q", "-f", "--detach", &m]);
        accepted(&home, fix, &w3, "s3", write("s.txt", "one\ntwo\n"));
        let o = run(&home, &g, &repo, "d1", learn::now_ms()).unwrap();
        assert_eq!((o.task.as_str(), o.verdict.clone()), ("integrate.2", Ok(())));
        let head = o.head.unwrap();
        assert_eq!(sh(&repo, &["rev-parse", &format!("{head}^1")]), m, "the replay reproduces the same combined tree, commit for commit");
        assert_eq!(show(&repo, &head, "s.txt"), "one\ntwo");
        assert!(ran.exists() && fold(&home).finished());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_later_attempt_that_changes_a_conflicted_path_resolves_it() {
        let (root, repo, home) = setup("resolve");
        accepted(&home, work("t1", &["s.txt"], &[]), &wt(&repo, 1), "s1", write("s.txt", "one\n"));
        accepted(&home, work("t2", &["s.txt"], &[]), &wt(&repo, 2), "s2", write("s.txt", "two\n"));
        let events = board::read(&super::super::tasks_path(&home, "g"));
        let b = board::fold(&events);
        let a: Vec<Attempt> = ["t1.v1", "t2.v1"].iter().map(|v| verdict::attempt(&events, &b, v).unwrap()).collect();
        let m = merge(&repo, &[], &a, MERGE_TIMEOUT).unwrap();
        assert_eq!(m.conflicts, [("t2".to_string(), vec!["s.txt".to_string()])]);
        assert_eq!(merge(&repo, &[], &a, MERGE_TIMEOUT).unwrap(), m, "the same attempts, the same commits");
        let w3 = wt(&repo, 3);
        sh(&w3, &["checkout", "-q", "-f", "--detach", &m.head]);
        std::fs::write(w3.join("s.txt"), "both\n").unwrap();
        let head = verdict::snapshot(&w3, verdict::SNAPSHOT_TIMEOUT).unwrap();
        let fixed = Attempt { work: "t3".into(), base: m.head.clone(), head, ..a[0].clone() };
        let m3 = merge(&repo, &[], &[a[0].clone(), a[1].clone(), fixed], MERGE_TIMEOUT).unwrap();
        assert!(m3.conflicts.is_empty(), "{:?}", m3.conflicts);
        assert_eq!(show(&repo, &m3.head, "s.txt"), "both");
        assert!(merge(&repo, &[], &[], MERGE_TIMEOUT).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_attempt_built_without_the_markers_never_resolves_a_conflict() {
        // t3 edits the conflicted file elsewhere, from the clean base: its change merges around the
        // markers and leaves them in, so the conflict stays open and the oracle never runs.
        let (root, repo, home) = setup("around");
        let ran = root.join("ran");
        let g = goal(&[&format!("unit=touch {}", ran.display())]);
        accepted(&home, work("t1", &["f.txt"], &[]), &wt(&repo, 1), "s1", write("f.txt", "1\na\n3\n4\n5\n6\n"));
        accepted(&home, work("t2", &["f.txt"], &[]), &wt(&repo, 2), "s2", write("f.txt", "1\nb\n3\n4\n5\n6\n"));
        accepted(&home, work("t3", &["f.txt"], &[]), &wt(&repo, 3), "s3", write("f.txt", "1\n2\n3\n4\n5\nz\n"));
        let o = run(&home, &g, &repo, "d1", learn::now_ms()).unwrap();
        assert_eq!(o.verdict, Err("conflict: t2 onto t1 in f.txt".to_string()));
        let head = o.head.unwrap();
        let f = show(&repo, &head, "f.txt");
        assert!(f.contains("<<<<<<<") && f.ends_with('z'), "{f}");
        assert!(!ran.exists() && !fold(&home).finished());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_drone_s_own_commits_are_not_the_origin_of_the_merge() {
        // d1 committed its rejected t1 attempt, then claimed t3 on top of it; t3 is verified first.
        // The merge starts before every claim's HEAD, so the rejected work is not in the result.
        let (root, repo, home) = setup("origin");
        let g = goal(&["unit=true"]);
        let path = super::super::tasks_path(&home, "g");
        let (w1, ms) = (wt(&repo, 1), learn::now_ms());
        let mut c = Event::new(Ev::Claim, "t1", "s1", ms + 1);
        c.base = verdict::snapshot(&w1, verdict::SNAPSHOT_TIMEOUT).unwrap();
        board::append(&path, &[Event::open(work("t1", &["a.txt"], &[]), "engineer:e", ms), c, Event::new(Ev::Released, "t1", "s1", ms + 2)]).unwrap();
        std::fs::write(w1.join("hack.txt"), "rejected\n").unwrap();
        sh(&w1, &["add", "-A"]);
        sh(&w1, &["commit", "-qm", "rejected t1"]);
        std::thread::sleep(Duration::from_millis(10));
        accepted(&home, work("t3", &["c.txt"], &[]), &w1, "s3", write("c.txt", "c\n"));
        accepted(&home, work("t1", &["a.txt"], &[]), &wt(&repo, 2), "s2", write("a.txt", "a\n"));
        let o = run(&home, &g, &repo, "d1", learn::now_ms()).unwrap();
        assert_eq!(o.verdict, Ok(()));
        let files = sh(&repo, &["ls-tree", "-r", "--name-only", &o.head.unwrap()]);
        assert!(files.contains("a.txt") && files.contains("c.txt") && !files.contains("hack.txt"), "{files}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_attempt_without_a_snapshot_blocks_the_integration() {
        let (root, repo, home) = setup("blocked");
        let g = goal(&["unit=true"]);
        accepted(&home, work("t1", &["a.txt"], &[]), &wt(&repo, 1), "s1", write("a.txt", "a\n"));
        // t2's claim had no snapshot (git failed at the claim).
        let path = super::super::tasks_path(&home, "g");
        let ms = learn::now_ms();
        let v = Task { role: Role::Verify, of: Some("t2".into()), not_by: vec!["s2".into()], ..board_task("t2.v1") };
        let lines = vec![
            Event::open(work("t2", &[], &[]), "engineer:e", ms),
            Event::new(Ev::Claim, "t2", "s2", ms + 1),
            Event::new(Ev::Done, "t2", "s2", ms + 2),
            Event::open(v, "s2", ms + 2),
            Event::new(Ev::Claim, "t2.v1", "verifier", ms + 3),
            Event::new(Ev::Verified, "t2.v1", "verifier", ms + 4),
        ];
        board::append(&path, &lines).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        let o = run(&home, &g, &repo, "d1", learn::now_ms()).unwrap();
        assert_eq!((o.head, o.verdict), (None, Err("t2 has no snapshot of its accepted attempt".to_string())));
        let b = fold(&home);
        assert!(matches!(&b.tasks["integrate.1"].status, Status::Blocked { .. }) && !b.finished());
        assert!(run(&home, &g, &repo, "d1", learn::now_ms()).is_none(), "blocked waits for the engineer, it does not loop");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn nothing_is_due_before_every_task_is_verified_and_a_lost_claim_integrates_nothing() {
        let (root, repo, home) = setup("due");
        let g = goal(&["unit=true"]);
        board::append(&super::super::tasks_path(&home, "g"), &[Event::open(work("t1", &[], &[]), "engineer:e", 1)]).unwrap();
        assert!(run(&home, &g, &repo, "d1", learn::now_ms()).is_none());
        let (root2, repo2, home2) = setup("lost");
        accepted(&home2, work("t1", &["a.txt"], &[]), &wt(&repo2, 1), "s1", write("a.txt", "a\n"));
        // Another runner opened and claimed integrate.1 first.
        let now = learn::now_ms();
        let b = fold(&home2);
        let rival = [
            Event::open(Task { title: "integrate".into(), ..board_task(&board::integrate_id(&b)) }, "rival", now - 5),
            Event::new(Ev::Claim, "integrate.1", "rival", now - 5),
        ];
        board::append(&super::super::tasks_path(&home2, "g"), &rival).unwrap();
        assert!(run(&home2, &g, &repo2, "d1", now).is_none());
        let b = fold(&home2);
        assert_eq!(b.tasks.values().filter(|s| s.task.role == Role::Integrate).count(), 1);
        assert!(matches!(&b.tasks["integrate.1"].status, Status::Claimed { by, .. } if by == "rival"));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(root2);
    }
}
