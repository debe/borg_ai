//! The verdict on a hive task and its reward (docs/design/hive-learning.md "Reward", steps 2 and 8).
//!
//! A verifier drone ends its verify task with `VERDICT: pass` or `VERDICT: reject <why>` (its last
//! message, read at the stop) or `borg hive verify --pass|--reject`. [`record`] then checks the work
//! itself, whatever the verifier said:
//! - the author never judges its own work (its session, or any session of its drone);
//! - the work's diff, from a snapshot of the claimant's tree at the claim (`base`) to one at the done
//!   (`head`, [`snapshot`]), may not modify, delete or rename tests or CI outside the task's own
//!   `files` ([`protected`]; adding tests is fine), and the drone may not have touched the oracle's
//!   files (guard.rs flags). Either makes the verdict `rejected` with `hacked`, a done that passed the
//!   visible oracle by changing it; the work reopens with the paths to restore.
//!
//! The reward: every lesson shown to the author's session while it held the claim
//! (veto.rs `shown_between`) gets the verdict as an outcome (outcome.rs `verified`, `rejected`,
//! `hacked`), once per attempt (`cause` = `<goal>/<verify task>`). With a hidden oracle (hidden.rs) a
//! pass or a reject waits for its score, which then decides `verified` or `rejected`; `hacked` is
//! final at once. Each final verdict counts one `task_*` stat. BORG_HIVE_CREDIT=off (the frozen arm of
//! an eval) credits nothing; the board and the stats still record.

use std::path::Path;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;

use super::TaskId;
use super::board::{Board, Ev, Event, Role, Task};
use super::guard::{self, Class};
use super::{Goal, board};
use crate::outcome::{self, Outcome};
use crate::{learn, queen, runner, stats, veto};

/// Each git call of a snapshot or a diff.
const GIT_TIMEOUT: Duration = Duration::from_secs(10);
/// A whole snapshot outside the stop hook (the CLI): its four git calls.
pub const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(40);
/// A reject's reason, at least.
const MIN_NOTE: usize = 4;

static LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?im)^[ \t]*VERDICT:[ \t]*(pass|reject)\b[ \t:,-]*(.*)$").unwrap());

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    Pass,
    Reject(String),
}

/// The verdict a verifier's last message ends with: its last `VERDICT: pass|reject <why>` line; None
/// without one, or for a reject without a reason (at least 4 characters).
pub fn parse(message: &str) -> Option<Said> {
    let c = LINE.captures_iter(message).last()?;
    match c[1].to_ascii_lowercase().as_str() {
        "pass" => Some(Said::Pass),
        _ => {
            let note = c[2].trim();
            (note.chars().count() >= MIN_NOTE).then(|| Said::Reject(note.chars().take(500).collect()))
        }
    }
}

/// `git -C dir args` within `GIT_TIMEOUT` and before `deadline`: its trimmed stdout on success.
fn git_by(dir: &Path, args: &[&str], env: &[(&str, &str)], deadline: Instant) -> Option<String> {
    let left = deadline.saturating_duration_since(Instant::now()).min(GIT_TIMEOUT);
    if left.is_zero() {
        return None;
    }
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(args).env("GIT_TERMINAL_PROMPT", "0").envs(env.iter().copied());
    match runner::run_capped_status(cmd, None, left)? {
        (s, out) if s.success() => Some(String::from_utf8_lossy(&out).trim().to_string()),
        _ => None,
    }
}

fn git(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Option<String> {
    git_by(dir, args, env, Instant::now() + GIT_TIMEOUT)
}

/// A commit of `dir`'s working tree as it is (tracked and untracked files, `.gitignore` respected),
/// written without touching the index, HEAD or any ref: a scratch index copied from the real one (so
/// unchanged files are not hashed again), `add -A`, `write-tree`, `commit-tree`, all `within` (each
/// git call also within `GIT_TIMEOUT`: the stop hook passes what is left of its time). None outside a
/// git repo or on any failure: the verdict then cannot check the work's diff, and says so.
pub fn snapshot(dir: &Path, within: Duration) -> Option<String> {
    let deadline = Instant::now() + within;
    let git = |dir: &Path, args: &[&str], env: &[(&str, &str)]| git_by(dir, args, env, deadline);
    let index = dir.join(git(dir, &["rev-parse", "--git-path", "index"], &[])?);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let tmp = std::env::temp_dir().join(format!("borg-hive-index-{}-{nanos}", std::process::id()));
    if index.is_file() && std::fs::copy(&index, &tmp).is_err() {
        return None;
    }
    let tmp_s = tmp.display().to_string();
    let env = [("GIT_INDEX_FILE", tmp_s.as_str())];
    let tree = git(dir, &["add", "-A"], &env).and_then(|_| git(dir, &["write-tree"], &env));
    let _ = std::fs::remove_file(&tmp);
    let tree = tree?;
    let who = [("GIT_AUTHOR_NAME", "borg"), ("GIT_AUTHOR_EMAIL", "borg@localhost"), ("GIT_COMMITTER_NAME", "borg"), ("GIT_COMMITTER_EMAIL", "borg@localhost")];
    let mut args = vec!["commit-tree", tree.as_str(), "-m", "borg hive snapshot"];
    let parent = git(dir, &["rev-parse", "--verify", "-q", "HEAD"], &[]);
    if let Some(p) = parent.as_deref() {
        args.extend(["-p", p]);
    }
    git(dir, &args, &who).filter(|c| !c.is_empty())
}

/// One changed path of a diff: `status` is git's letter (M, A, D, R, T, C); a rename names its old path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub status: char,
}

/// The paths changed from `base` to `head` (commits of the repo at `dir`); None when git cannot say.
/// Read NUL-separated (`-z`): paths come unquoted whatever their bytes (`core.quotePath`).
pub fn changes(dir: &Path, base: &str, head: &str) -> Option<Vec<Change>> {
    let out = git(dir, &["diff", "--name-status", "-z", "-M", "--no-color", base, head], &[])?;
    Some(parse_changes(&out))
}

/// `git diff --name-status -z`: a status field, then one path (two for a rename or copy: the old one
/// is kept), each NUL-terminated.
fn parse_changes(out: &str) -> Vec<Change> {
    let mut f = out.split('\0').filter(|x| !x.is_empty());
    let mut got = Vec::new();
    while let Some(status) = f.next().and_then(|s| s.chars().next()) {
        let Some(path) = f.next() else { break };
        if matches!(status, 'R' | 'C') {
            f.next();
        }
        got.push(Change { path: path.to_string(), status });
    }
    got
}

/// The protected changes: tests or CI (guard.rs `class_of`) modified, deleted, renamed or retyped,
/// outside the task's own `files` scope. Adding a test is allowed; adding a test runner's config
/// (guard.rs `runner_config`: a `conftest.py`, a `pytest.ini`) is not, as it changes how the existing
/// tests run.
pub fn protected(changes: &[Change], files: &[String], oracle_files: &[String]) -> Vec<Change> {
    let globs: Vec<Regex> = files.iter().map(|f| crate::assimilate_dir::glob(f)).collect();
    changes
        .iter()
        .filter(|c| matches!(c.status, 'M' | 'D' | 'R' | 'T') || (c.status == 'A' && guard::runner_config(&c.path)))
        .filter(|c| matches!(guard::class_of(&c.path, Path::new("/"), &[], oracle_files), Some(Class::Test | Class::Ci)))
        .filter(|c| !globs.iter().any(|g| g.is_match(&c.path)))
        .cloned()
        .collect()
}

/// What a verdict credits: a final verdict, or (with a hidden oracle) one deferred to its score, with
/// whether the board said verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Verified,
    Rejected,
    Hacked,
    Deferred(bool),
    /// A pass on work whose diff could not be checked (no snapshot at its claim or done): the board
    /// rejects it, nothing is credited and the hidden oracle never scores it.
    Unchecked,
}

impl Kind {
    /// The outcome kind it credits now; None while deferred.
    pub fn outcome(self) -> Option<&'static str> {
        match self {
            Kind::Verified => Some(outcome::VERIFIED),
            Kind::Rejected => Some(outcome::REJECTED),
            Kind::Hacked => Some(outcome::HACKED),
            Kind::Deferred(_) | Kind::Unchecked => None,
        }
    }
}

/// The board's verdict and what it credits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub ev: Ev,
    pub hacked: bool,
    pub note: String,
    pub kind: Kind,
}

/// Why a pass cannot verify work whose diff was not checked.
pub const UNCHECKED: &str = "the work has no snapshot at its claim or its done (git failed or timed out), so its diff cannot be checked; publish done again";

/// The truth table: any `oracle` flag or protected change makes a hacked reject, whatever was said;
/// else a pass on work whose diff was not `checked` is an `Unchecked` reject (fail closed: a drone
/// that makes its snapshot fail does not pass unseen); else a pass verifies and a reject rejects with
/// its note, both deferred to the hidden oracle when the goal has one.
pub fn decide(said: &Said, protected: &[Change], oracle_flags: &[String], checked: bool, has_hidden: bool) -> Decision {
    if !protected.is_empty() || !oracle_flags.is_empty() {
        let mut parts = Vec::new();
        if !protected.is_empty() {
            let paths: Vec<&str> = protected.iter().map(|c| c.path.as_str()).collect();
            parts.push(format!("protected files changed: {}; restore them", paths.join(", ")));
        }
        if !oracle_flags.is_empty() {
            parts.push(format!("the oracle's files were touched: {}", oracle_flags.join(", ")));
        }
        if let Said::Reject(n) = said {
            parts.push(format!("verifier: {n}"));
        }
        return Decision { ev: Ev::Rejected, hacked: true, note: parts.join("; "), kind: Kind::Hacked };
    }
    match said {
        Said::Pass if !checked => Decision { ev: Ev::Rejected, hacked: false, note: UNCHECKED.into(), kind: Kind::Unchecked },
        Said::Pass => Decision { ev: Ev::Verified, hacked: false, note: String::new(), kind: if has_hidden { Kind::Deferred(true) } else { Kind::Verified } },
        // Unchecked, the hidden oracle has no snapshot to score either: the reject is final.
        Said::Reject(n) => Decision { ev: Ev::Rejected, hacked: false, note: n.clone(), kind: if has_hidden && checked { Kind::Deferred(false) } else { Kind::Rejected } },
    }
}

/// The attempt a verify task judges: the `done` that opened it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub work: TaskId,
    /// The session that did the work, and the drone it was an iteration of (empty: none).
    pub author: String,
    pub drone: String,
    pub claim_ms: i64,
    pub done_ms: i64,
    pub base: String,
    pub head: String,
}

/// The attempt of the verify task `verify` on the board `b` folded from `events` (one pass): its open
/// was written with the work's `done` (same session, same ms), and that done is one the fold took (it
/// ended a tenure, board.rs `Span`): a done the fold ignored (its lease had passed to another drone)
/// opens nothing (board.rs `apply`) and is never judged. The claim is the tenure that done ended.
pub fn attempt(events: &[Event], b: &Board, verify: &str) -> Option<Attempt> {
    let (open, span) = events.iter().filter(|e| e.ev == Ev::Open && e.task == verify).find_map(|e| {
        let work = e.def.as_ref()?.of.as_ref()?;
        let span = b.spans.iter().rev().find(|s| &s.task == work && s.by == e.by && s.to_ms == Some(e.ms) && s.end == Some(Ev::Done))?;
        Some((e, span))
    })?;
    let (work, author, done_ms, claim_ms) = (span.task.clone(), open.by.clone(), open.ms, span.from_ms);
    let done = events.iter().find(|e| e.ev == Ev::Done && e.task == work && e.by == author && e.ms == done_ms)?;
    let base = events.iter().find(|e| e.ev == Ev::Claim && e.task == work && e.by == author && e.ms == claim_ms).map(|e| e.base.clone()).unwrap_or_default();
    Some(Attempt { drone: b.drones.get(&author).cloned().unwrap_or_default(), author, work, claim_ms, done_ms, base, head: done.head.clone() })
}

/// The idempotence key of a verdict's credit.
pub fn cause(goal: &str, verify: &str) -> String {
    format!("{goal}/{verify}")
}

/// Credits `kind` (an outcome verdict kind) to every lesson shown in the attempt's claim, once per
/// lesson and attempt, and counts the verdict's `task_*` stat; returns the lessons credited now.
#[allow(clippy::too_many_arguments)]
pub fn settle(home: &Path, goal: &str, verify: &str, a: &Attempt, kind: &str, note: &str, repo: &str, now: i64) -> Vec<String> {
    let credited = if credit_on() {
        // Half-open [claim, done): a lesson asked in the done's own millisecond is taken as after the work.
        let lessons = veto::shown_between(home, &a.author, a.claim_ms, a.done_ms - 1);
        let path = home.join("outcomes.jsonl");
        let records: Vec<Outcome> = outcome::verdicts(&outcome::read(&path), kind, &a.author, &lessons, &cause(goal, verify), now, repo);
        records.iter().filter(|o| outcome::append(&path, o).is_ok()).flat_map(|o| o.lessons.clone()).collect()
    } else {
        Vec::new()
    };
    let stat = match kind {
        outcome::VERIFIED => stats::TASK_VERIFIED,
        outcome::REJECTED => stats::TASK_REJECTED,
        _ => stats::TASK_HACKED,
    };
    stats::count(home, stat, &a.author, |ev| {
        ev.ms = now;
        ev.why = goal.into();
        ev.repo = repo.into();
        ev.lessons = credited.clone();
        ev.n = credited.len() as u64;
        ev.text = stats::excerpt(&if note.is_empty() { a.work.clone() } else { format!("{}: {note}", a.work) }, 160);
    });
    credited
}

/// Crediting on: learning is, and BORG_HIVE_CREDIT is not `off` (an eval's frozen arm).
fn credit_on() -> bool {
    learn::learning() && std::env::var("BORG_HIVE_CREDIT").as_deref() != Ok("off")
}

/// What `record` wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct Recorded {
    pub decision: Decision,
    pub work: TaskId,
    pub credited: Vec<String>,
}

impl Recorded {
    /// The line the verifier is told.
    pub fn summary(&self, verify: &str) -> String {
        let d = &self.decision;
        let what = match (d.ev, d.hacked) {
            (_, true) => format!("{verify}: {} rejected as hacked ({}); it is open again", self.work, d.note),
            _ if d.kind == Kind::Unchecked => format!("{verify}: {} not verified ({}); it is open again", self.work, d.note),
            (Ev::Verified, _) => format!("{verify} verified: {} is verified", self.work),
            _ => format!("{verify} rejected: {} is open again with your note", self.work),
        };
        match d.kind {
            Kind::Deferred(_) => format!("{what}. The hidden oracle scores it next."),
            _ => what,
        }
    }
}

/// Records `said` by the verifier `session` (in `cwd`) on the verify task `verify` of goal `g`: the
/// board's verdict (`decide`), then the credit of a final one. Refused when `session` does not hold
/// the task or is (a session of the drone of) the work's author.
///
/// # Errors
/// Why nothing was recorded.
pub fn record(home: &Path, g: &Goal, verify: &str, said: &Said, session: &str, cwd: &str, now: i64) -> Result<Recorded, String> {
    let path = super::tasks_path(home, &g.id);
    let events = board::read(&path);
    let b = board::fold(&events);
    let s = b.tasks.get(verify).ok_or_else(|| format!("no task {verify} in goal {}", g.id))?;
    if s.task.role != Role::Verify {
        return Err(format!("{verify} is not a verify task"));
    }
    if s.live(now).is_none_or(|(by, _, _)| by != session) {
        return Err(format!("{verify} is not claimed by this session"));
    }
    let a = attempt(&events, &b, verify).ok_or_else(|| format!("{verify}: no done of its work to judge"))?;
    let drone = b.drones.get(session).cloned().or_else(|| super::bound(session).map(|hs| hs.drone)).unwrap_or_default();
    if a.author == session || (!a.drone.is_empty() && a.drone == drone) {
        return Err("the author cannot verify its own work".into());
    }
    let work: Option<&Task> = b.tasks.get(&a.work).map(|w| &w.task);
    let root = Path::new(cwd);
    // No snapshot: the diff is unchecked (a pass then rejects). Snapshots git cannot diff here (a
    // verifier outside the goal's repo): no verdict, never one that skipped the check.
    let checked = !a.base.is_empty() && !a.head.is_empty();
    let changed = match checked {
        true => changes(root, &a.base, &a.head)
            .ok_or_else(|| format!("cannot diff the work of {} from {cwd}: give the verdict from a worktree of the goal's repo", a.work))?,
        false => Vec::new(),
    };
    let prot = protected(&changed, work.map_or(&[][..], |w| &w.files), &guard::oracle_files(g, root));
    let oracle_flags: Vec<String> =
        guard::flags_of(home, &g.id, &a.author, a.claim_ms, a.done_ms).into_iter().filter(|f| f.class == Class::Oracle).map(|f| f.path).collect();
    let d = decide(said, &prot, &oracle_flags, checked, g.hidden.is_some());
    let mut e = Event::new(d.ev, verify, session, now).with_note(&d.note);
    e.hacked = d.hacked.then_some(true);
    board::append(&path, &[e]).map_err(|e| e.to_string())?;
    let credited = match d.kind.outcome() {
        Some(kind) => settle(home, &g.id, verify, &a, kind, &d.note, &queen::repo_name(cwd), now),
        None => Vec::new(),
    };
    Ok(Recorded { work: a.work, decision: d, credited })
}

/// A verified verify task: whose work (`author`, in its claim `work_from..=work_to`), by whom
/// (`verifier`, in its claim of the verify task `from..=ms`), and its `cause`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification {
    pub author: String,
    pub work_from: i64,
    pub work_to: i64,
    pub verifier: String,
    pub from: i64,
    pub ms: i64,
    pub cause: String,
}

/// Every `verified` verdict of the store's hive goals (one pass over each board); none without a hive.
/// learn.rs lets a regret of the work reach its verifier.
pub fn verifications(home: &Path) -> Vec<Verification> {
    if std::fs::metadata(home.join("hive")).is_err() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for g in super::goal::list(home) {
        let events = board::read(&super::tasks_path(home, &g));
        let b = board::fold(&events);
        // The work tenure each verify task's open ended: (verify task) -> (author, from, to).
        let done: std::collections::HashMap<(&str, &str, i64), i64> =
            b.spans.iter().filter(|s| s.end == Some(Ev::Done)).filter_map(|s| Some(((s.task.as_str(), s.by.as_str(), s.to_ms?), s.from_ms))).collect();
        let mut works: std::collections::HashMap<&str, (&str, i64, i64)> = std::collections::HashMap::new();
        for e in events.iter().filter(|e| e.ev == Ev::Open) {
            let Some(work) = e.def.as_ref().filter(|d| d.role == Role::Verify).and_then(|d| d.of.as_deref()) else { continue };
            if let Some(from) = done.get(&(work, e.by.as_str(), e.ms)) {
                works.entry(e.task.as_str()).or_insert((e.by.as_str(), *from, e.ms));
            }
        }
        for s in b.spans.iter().filter(|s| s.end == Some(Ev::Verified)) {
            let (Some((author, work_from, work_to)), Some(ms)) = (works.get(s.task.as_str()), s.to_ms) else { continue };
            out.push(Verification {
                author: author.to_string(),
                work_from: *work_from,
                work_to: *work_to,
                verifier: s.by.clone(),
                from: s.from_ms,
                ms,
                cause: cause(&g, &s.task),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_verdict_line_counts_and_a_reject_needs_a_reason() {
        assert_eq!(parse("Checked it.\nVERDICT: pass"), Some(Said::Pass));
        assert_eq!(parse("verdict: Reject - misses the empty input case"), Some(Said::Reject("misses the empty input case".into())));
        assert_eq!(parse("VERDICT: reject: no\n"), None, "too short a reason");
        assert_eq!(parse("VERDICT: pass\n  VERDICT: reject the parser drops tabs"), Some(Said::Reject("the parser drops tabs".into())));
        assert_eq!(parse("the verdict: pass is pending"), None, "only at the start of a line");
        assert_eq!(parse("VERDICT: passable"), None);
        assert_eq!(parse(""), None);
    }

    fn ch(path: &str, status: char) -> Change {
        Change { path: path.into(), status }
    }

    #[test]
    fn the_truth_table() {
        let flags: Vec<String> = Vec::new();
        let edited = protected(&[ch("tests/a.py", 'M')], &[], &[]);
        // 1. A pass whose diff edited a test: hacked.
        let d = decide(&Said::Pass, &edited, &flags, true, false);
        assert_eq!((d.ev, d.hacked, d.kind), (Ev::Rejected, true, Kind::Hacked));
        assert!(d.note.contains("tests/a.py") && d.note.contains("restore"), "{}", d.note);
        // 2. A test in the task's own scope is not protected.
        assert!(protected(&[ch("tests/a.py", 'M')], &["tests/*.py".into()], &[]).is_empty());
        // 3. An added test is allowed; a deleted or renamed one, or CI, is not.
        assert!(protected(&[ch("tests/new.py", 'A'), ch("src/x.rs", 'M')], &[], &[]).is_empty());
        assert_eq!(protected(&[ch("tests/a.py", 'D'), ch("spec/b_spec.rb", 'R'), ch("grade.sh", 'M')], &[], &["grade.sh".into()]).len(), 3);
        // 4. An oracle flag makes even a reject hacked.
        let d = decide(&Said::Reject("wrong output".into()), &[], &["/held/t.py".into()], true, true);
        assert_eq!((d.ev, d.hacked, d.kind), (Ev::Rejected, true, Kind::Hacked));
        assert!(d.note.contains("/held/t.py") && d.note.contains("verifier: wrong output"));
        // 5. Clean: a pass verifies, a reject rejects with its note.
        assert_eq!(decide(&Said::Pass, &[], &flags, true, false), Decision { ev: Ev::Verified, hacked: false, note: String::new(), kind: Kind::Verified });
        let d = decide(&Said::Reject("misses tabs".into()), &[], &flags, true, false);
        assert_eq!((d.ev, d.hacked, d.note.as_str(), d.kind), (Ev::Rejected, false, "misses tabs", Kind::Rejected));
        // 6. With a hidden oracle both wait for its score; a hack does not.
        assert_eq!(decide(&Said::Pass, &[], &flags, true, true).kind, Kind::Deferred(true));
        assert_eq!(decide(&Said::Reject("misses tabs".into()), &[], &flags, true, true).kind, Kind::Deferred(false));
        assert_eq!(decide(&Said::Pass, &edited, &flags, true, true).kind, Kind::Hacked);
        assert_eq!((Kind::Hacked.outcome(), Kind::Deferred(true).outcome()), (Some(outcome::HACKED), None));
        // 7. Unchecked (no snapshot): a pass rejects and credits nothing, a reject is final; a hack still shows.
        let d = decide(&Said::Pass, &[], &flags, false, true);
        assert_eq!((d.ev, d.hacked, d.kind, d.kind.outcome()), (Ev::Rejected, false, Kind::Unchecked, None));
        assert_eq!(decide(&Said::Reject("misses tabs".into()), &[], &flags, false, true).kind, Kind::Rejected);
        assert_eq!(decide(&Said::Pass, &[], &["/held/t.py".into()], false, false).kind, Kind::Hacked);
        // 8. An added test runner config changes how the existing tests run: protected, added or not.
        assert_eq!(protected(&[ch("tests/conftest.py", 'A'), ch("pytest.ini", 'A'), ch("tests/test_new.py", 'A')], &[], &[]).len(), 2);
    }

    #[test]
    fn a_nul_separated_diff_keeps_unusual_paths_as_they_are() {
        let out = "M\0tests/donn\u{e9}es.py\0R087\0tests/old \"q\".py\0src/new.py\0A\0a\tb.txt\0";
        assert_eq!(parse_changes(out), [ch("tests/donn\u{e9}es.py", 'M'), ch("tests/old \"q\".py", 'R'), ch("a\tb.txt", 'A')]);
        assert!(parse_changes("").is_empty());
        assert_eq!(protected(&parse_changes(out), &[], &[]).len(), 2, "both tests, unquoted");
    }

    #[test]
    fn another_session_of_the_authors_drone_cannot_give_the_verdict() {
        let home = std::env::temp_dir().join(format!("borg-hive-verdict-self-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
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
        };
        super::super::goal::create(&home, &g).unwrap();
        let t1 = Task {
            id: "t1".into(),
            role: Role::Work,
            title: "t".into(),
            repo: String::new(),
            files: vec![],
            deps: vec![],
            accept: vec![],
            of: None,
            not_by: vec![],
            flags: vec![],
        };
        // A verify task barred to the author's session only (a line from before drones were barred).
        let v1 = Task { id: "t1.v1".into(), role: Role::Verify, of: Some("t1".into()), not_by: vec!["s1".into()], ..t1.clone() };
        let mut claim = Event::new(Ev::Claim, "t1", "s1", 10).with_drone("d1");
        claim.base = "b".into();
        let mut done = Event::new(Ev::Done, "t1", "s1", 20);
        done.head = "h".into();
        let lines = [Event::open(t1, "engineer:e", 1), claim, done, Event::open(v1, "s1", 20), Event::new(Ev::Claim, "t1.v1", "s2", 30).with_drone("d1")];
        let path = super::super::tasks_path(&home, "g");
        board::append(&path, &lines).unwrap();
        let events = board::read(&path);
        let b = board::fold(&events);
        let a = attempt(&events, &b, "t1.v1").unwrap();
        let want = Attempt { work: "t1".into(), author: "s1".into(), drone: "d1".into(), claim_ms: 10, done_ms: 20, base: "b".into(), head: "h".into() };
        assert_eq!(a, want);
        assert!(matches!(&b.tasks["t1.v1"].status, board::Status::Claimed { by, .. } if by == "s2"), "the board let s2 claim it");
        let got = record(&home, &g, "t1.v1", &Said::Pass, "s2", "/nonexistent", 31);
        assert_eq!(got.unwrap_err(), "the author cannot verify its own work");
        assert_eq!(board::read(&path).len(), lines.len(), "nothing appended");
        assert!(record(&home, &g, "t1.v1", &Said::Pass, "s9", "/nonexistent", 31).unwrap_err().contains("not claimed by this session"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_done_the_fold_ignored_opens_nothing_and_is_never_judged() {
        use super::super::LEASE_MS;
        let t1 = Task { id: "t1".into(), role: Role::Work, title: "t".into(), repo: String::new(), files: vec![], deps: vec![], accept: vec![], of: None, not_by: vec![], flags: vec![] };
        let v1 = |author: &str| Task { id: "t1.v1".into(), role: Role::Verify, of: Some("t1".into()), not_by: vec![author.into()], ..t1.clone() };
        let (a_done, b_claim, b_done) = (LEASE_MS + 50, LEASE_MS + 20, LEASE_MS + 90);
        let mut lines = vec![Event::open(t1.clone(), "engineer:e", 1), Event::new(Ev::Claim, "t1", "A", 10)];
        // A's lease ran out during its checks; B claimed t1, then A's done and open landed anyway.
        let mut b = Event::new(Ev::Claim, "t1", "B", b_claim);
        b.base = "bb".into();
        lines.push(b);
        lines.extend([Event::new(Ev::Done, "t1", "A", a_done), Event::open(v1("A"), "A", a_done)]);
        let mut done = Event::new(Ev::Done, "t1", "B", b_done);
        done.head = "bh".into();
        lines.extend([done, Event::open(v1("B"), "B", b_done)]);
        let b = board::fold(&lines);
        assert_eq!(b.tasks["t1.v1"].task.not_by, ["B"], "the verify task is that of the done the fold took");
        let a = attempt(&lines, &b, "t1.v1").unwrap();
        assert_eq!((a.author.as_str(), a.claim_ms, a.done_ms, a.base.as_str(), a.head.as_str()), ("B", b_claim, b_done, "bb", "bh"));
    }

    fn git_ok(dir: &Path, args: &[&str]) {
        assert!(std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success(), "git {args:?}");
    }

    #[test]
    fn a_snapshot_commits_the_tree_and_leaves_index_and_head_alone() {
        let dir = std::env::temp_dir().join(format!("borg-hive-snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        assert_eq!(snapshot(&dir, SNAPSHOT_TIMEOUT), None, "not a repo");
        git_ok(&dir, &["init", "-q"]);
        std::fs::write(dir.join("tests/a.py"), "assert 1\n").unwrap();
        std::fs::write(dir.join("keep.txt"), "x\n").unwrap();
        git_ok(&dir, &["add", "-A"]);
        git_ok(&dir, &["-c", "user.email=e@x", "-c", "user.name=e", "commit", "-qm", "init"]);
        assert_eq!(snapshot(&dir, Duration::ZERO), None, "no time left: no snapshot");
        let base = snapshot(&dir, SNAPSHOT_TIMEOUT).unwrap();
        std::fs::write(dir.join("tests/a.py"), "assert True\n").unwrap();
        std::fs::write(dir.join("tests/new.py"), "assert 2\n").unwrap();
        std::fs::remove_file(dir.join("keep.txt")).unwrap();
        let head = snapshot(&dir, SNAPSHOT_TIMEOUT).unwrap();
        let mut got = changes(&dir, &base, &head).unwrap();
        got.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(got, [ch("keep.txt", 'D'), ch("tests/a.py", 'M'), ch("tests/new.py", 'A')]);
        let status = std::process::Command::new("git").arg("-C").arg(&dir).args(["status", "--porcelain"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&status.stdout), " D keep.txt\n M tests/a.py\n?? tests/new.py\n", "the index is untouched");
        let log = std::process::Command::new("git").arg("-C").arg(&dir).args(["rev-list", "--all", "--count"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "1", "no ref points at a snapshot");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
