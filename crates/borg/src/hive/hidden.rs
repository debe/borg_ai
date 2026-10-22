//! The hidden oracle: held-out checks no drone sees (goal.json `hidden`, kept outside every repo and
//! worktree), run after a verdict on the work as it was at its `done` (docs/design/hive-learning.md
//! "Reward"). Its score decides what a pass or a reject credits (verdict.rs `settle`): a pass of the
//! hidden checks is `verified`, a failure `rejected`, whatever the verifier said; `agree` logs whether
//! the verifier said the same (for crediting verifiers later, deferred).
//!
//! `<goal>/oracle.jsonl` holds one `Score` per judged attempt, read by the runner only: no hook shows
//! it, it never syncs, and a drone that opens it is flagged (guard.rs). A score runs in a scratch
//! worktree `<goal>/scratch/<task>-<ms>` checked out at the done's snapshot and removed afterwards;
//! each command is bounded by the goal's `timeout_s`. `borg hive run` scores what its drones left when
//! they end, `borg hive score <goal>` any time (an engineer's command: a drone is refused); one attempt
//! is never scored twice, and one the oracle could not run on (a failed checkout) is not scored yet.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::board::{Ev, Role};
use super::goal::Hidden;
use super::{Goal, board, verdict};
use crate::{outcome, runner};

/// The scratch checkouts' directory, under the goal's.
pub const SCRATCH: &str = "scratch";
/// What a score keeps of the commands' output: its last bytes.
const TAIL_BYTES: usize = 2000;
const GIT_TIMEOUT: Duration = Duration::from_secs(60);

pub fn scores_path(home: &Path, goal: &str) -> PathBuf {
    super::dir(home, goal).join("oracle.jsonl")
}

/// One line of oracle.jsonl: the hidden checks' result on the attempt the verify task `task` judged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Score {
    pub v: u8,
    pub task: String,
    /// The done's snapshot it ran on.
    pub head: String,
    pub pass: bool,
    #[serde(default)]
    pub timeout: bool,
    pub dur_ms: u64,
    pub ms: i64,
    /// The board's verdict said the same.
    pub agree: bool,
    #[serde(default)]
    pub tail: String,
}

/// The last `n` bytes of `out`, cut at a character boundary.
fn tail(out: &[u8], n: usize) -> String {
    let text = String::from_utf8_lossy(out);
    let start = text.len().saturating_sub(n);
    let start = (start..text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(text.len());
    text[start..].to_string()
}

fn git(repo: &Path, args: &[&str]) -> bool {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args).env("GIT_TERMINAL_PROMPT", "0");
    runner::run_capped_status(cmd, None, GIT_TIMEOUT).is_some_and(|(s, _)| s.success())
}

/// (pass, timeout, dur_ms, tail): the hidden commands run one after the other in a scratch worktree of
/// `repo` at `head` (`<goal_dir>/scratch/<key>`), each `sh -c` with `BORG_HIDDEN` and `BORG_WORK`
/// set, the first failure deciding; the worktree is removed whatever happened.
///
/// # Errors
/// A checkout that fails (`repo` is not the goal's, another filesystem, a snapshot gone): the oracle
/// never ran, so there is no score, only the note why.
pub fn run_hidden(goal_dir: &Path, h: &Hidden, repo: &Path, head: &str, key: &str) -> Result<(bool, bool, u64, String), String> {
    run_within(goal_dir, h, repo, head, key, h.timeout())
}

/// `run_hidden`, each command within `cap`.
fn run_within(goal_dir: &Path, h: &Hidden, repo: &Path, head: &str, key: &str, cap: Duration) -> Result<(bool, bool, u64, String), String> {
    let t0 = Instant::now();
    let scratch = goal_dir.join(SCRATCH).join(key.replace(['/', '\\'], "_"));
    let _ = std::fs::create_dir_all(goal_dir.join(SCRATCH));
    let s = scratch.display().to_string();
    if !git(repo, &["worktree", "add", "--detach", "--force", &s, head]) {
        let _ = std::fs::remove_dir_all(&scratch);
        return Err(format!("could not check out {head} from {} for the hidden oracle", repo.display()));
    }
    let (mut pass, mut timeout, mut out) = (true, false, Vec::new());
    for c in h.commands.iter().filter(|c| !c.trim().is_empty()) {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(format!("exec 2>&1\n{c}"))
            .current_dir(&scratch)
            .env("BORG_HIDDEN", &h.dir)
            .env("BORG_WORK", &scratch)
            .env("BORG_LEARN", "off")
            .env(super::ENV, "")
            .env_remove(super::DRONE_ENV);
        match runner::run_bounded(cmd, None, cap) {
            Some(b) => {
                out.extend_from_slice(&b.out);
                timeout = b.timed_out();
                pass = b.status.is_some_and(|s| s.success());
            }
            None => {
                out.extend_from_slice(format!("(could not run `{c}`)").as_bytes());
                pass = false;
            }
        }
        if !pass {
            break;
        }
    }
    if !git(repo, &["worktree", "remove", "--force", &s]) {
        let _ = std::fs::remove_dir_all(&scratch);
        let _ = git(repo, &["worktree", "prune"]);
    }
    Ok((pass, timeout, t0.elapsed().as_millis() as u64, tail(&out, TAIL_BYTES)))
}

/// What one scoring did: the attempts scored, and those the oracle could not run on (`task: why`),
/// which stay unscored for a later run.
#[derive(Debug, Default)]
pub struct Scored {
    pub scores: Vec<Score>,
    pub skipped: Vec<String>,
}

/// Scores up to `limit` judged attempts of `g` that have no score yet, oldest first, in `repo` (where
/// the done's snapshots are): each a `Score` line, then its credit (`verified` on a pass, else
/// `rejected`). A judged attempt is a verify task's valid `verified`, or its `rejected` that was not
/// hacked (a hacked verdict credited at once) and whose work has both snapshots (one without them was
/// final at once, verdict.rs `decide`). An attempt whose checkout fails gets no score and no credit:
/// an infrastructure failure is not the work's. One scoring at a time per goal (a lock on
/// oracle.jsonl), so two never score one attempt. Nothing without a hidden oracle.
pub fn score_pending(home: &Path, g: &Goal, repo: &Path, limit: usize, now: i64) -> Scored {
    if g.hidden.is_none() {
        return Scored::default();
    }
    crate::live::locked(&scores_path(home, &g.id), || score_locked(home, g, repo, limit, now))
}

fn score_locked(home: &Path, g: &Goal, repo: &Path, limit: usize, now: i64) -> Scored {
    let Some(h) = g.hidden.as_ref() else { return Scored::default() };
    let path = super::tasks_path(home, &g.id);
    let events = board::read(&path);
    let b = board::fold(&events);
    let scored: std::collections::HashSet<String> = super::read_lines::<Score>(&scores_path(home, &g.id)).into_iter().map(|s| s.task).collect();
    // The verdicts the fold took: the verify tenures they closed.
    let mut judged: Vec<&board::Event> = events
        .iter()
        .filter(|e| matches!(e.ev, Ev::Verified | Ev::Rejected) && e.hacked != Some(true) && !scored.contains(&e.task))
        .filter(|e| b.tasks.get(&e.task).is_some_and(|s| s.task.role == Role::Verify))
        .filter(|e| b.spans.iter().any(|s| s.task == e.task && s.by == e.by && s.to_ms == Some(e.ms) && s.end == Some(e.ev)))
        .collect();
    judged.sort_by_key(|e| (e.ms, e.task.clone()));
    judged.dedup_by(|a, b| a.task == b.task);
    let dir = super::dir(home, &g.id);
    let mut out = Scored::default();
    let attempts = judged.into_iter().filter_map(|e| Some((e, verdict::attempt(&events, &b, &e.task)?))).filter(|(_, a)| !a.base.is_empty() && !a.head.is_empty());
    for (e, a) in attempts.take(limit) {
        let (pass, timeout, dur_ms, tail) = match run_hidden(&dir, h, repo, &a.head, &format!("{}-{}", e.task, now)) {
            Ok(r) => r,
            Err(why) => {
                out.skipped.push(format!("{}: {why}", e.task));
                continue;
            }
        };
        let s = Score { v: 1, task: e.task.clone(), head: a.head.clone(), pass, timeout, dur_ms, ms: now, agree: (e.ev == Ev::Verified) == pass, tail };
        if super::append_line(&scores_path(home, &g.id), &s).is_err() {
            continue;
        }
        let kind = if pass { outcome::VERIFIED } else { outcome::REJECTED };
        let note = if pass { "the hidden oracle passed" } else { "the hidden oracle failed" };
        verdict::settle(home, &g.id, &e.task, &a, kind, note, &queen_repo(repo), now);
        out.scores.push(s);
    }
    out
}

fn queen_repo(repo: &Path) -> String {
    crate::queen::repo_name(&repo.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_ok(dir: &Path, args: &[&str]) {
        assert!(Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success(), "git {args:?}");
    }

    #[test]
    fn pass_fail_and_timeout_in_a_scratch_worktree_that_is_removed() {
        let root = std::env::temp_dir().join(format!("borg-hive-hidden-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (repo, held, goal_dir) = (root.join("app"), root.join("held"), root.join("store/hive/g"));
        for d in [&repo, &held, &goal_dir] {
            std::fs::create_dir_all(d).unwrap();
        }
        git_ok(&repo, &["init", "-q"]);
        std::fs::write(repo.join("answer.txt"), "42\n").unwrap();
        git_ok(&repo, &["add", "-A"]);
        git_ok(&repo, &["-c", "user.email=e@x", "-c", "user.name=e", "commit", "-qm", "init"]);
        std::fs::write(held.join("check.sh"), "grep -q 42 \"$BORG_WORK/answer.txt\"\n").unwrap();
        let head = verdict::snapshot(&repo, verdict::SNAPSHOT_TIMEOUT).unwrap();
        let h = |cmds: &[&str], timeout_s: u64| Hidden { dir: held.display().to_string(), commands: cmds.iter().map(|c| c.to_string()).collect(), timeout_s };
        let (pass, timeout, _, _) = run_hidden(&goal_dir, &h(&["sh \"$BORG_HIDDEN/check.sh\"", "test -f answer.txt"], 30), &repo, &head, "t1.v1-1").unwrap();
        assert!(pass && !timeout);
        let (pass, timeout, _, tail) = run_hidden(&goal_dir, &h(&["echo nope; exit 1"], 30), &repo, &head, "t1.v1-2").unwrap();
        assert!(!pass && !timeout && tail == "nope\n", "{tail}");
        // A timeout is a fail (the goal's floor is 30 s; the test cuts at 200 ms).
        let started = Instant::now();
        let (pass, timeout, _, _) = run_within(&goal_dir, &h(&["sleep 5"], 30), &repo, &head, "t1.v1-3", Duration::from_millis(200)).unwrap();
        assert!(!pass && timeout && started.elapsed() < Duration::from_secs(4));
        // A checkout that fails is no score at all: the oracle never ran.
        let why = run_hidden(&goal_dir, &h(&["true"], 30), &repo, "0000000000000000000000000000000000000000", "t1.v1-4").unwrap_err();
        assert!(why.contains("could not check out"), "{why}");
        let left: Vec<_> = std::fs::read_dir(goal_dir.join(SCRATCH)).unwrap().flatten().collect();
        assert!(left.is_empty(), "every scratch worktree is removed: {left:?}");
        let list = Command::new("git").arg("-C").arg(&repo).args(["worktree", "list"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&list.stdout).lines().count(), 1, "and unregistered");
        let _ = std::fs::remove_dir_all(&root);
    }
}
