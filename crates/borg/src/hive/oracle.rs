//! The oracle: what gates `done`. A task's `accept` checks (else the goal's oracle) run one after the
//! other, each `sh -c` in its repo's directory, bounded by its own timeout and by what is left of the
//! caller's budget (the stop hook's, or none for `borg hive done`); the first one that does not pass
//! decides. A failed run's output tail is kept in `<goal>/oracle/<task>.<ms>.log` (the newest
//! `KEEP_LOGS`), for the drone and its verifier.
//!
//! A check runs only where it is approved ([`Approvals`]): the board and goal.jsonl sync from every
//! member, so a check another node wrote is a shell command from another machine. This node's own
//! goal.json oracle is approved, and so is every check of a task this node opens (hive/mod.rs `write`
//! records it); any other check waits for the engineer's `borg hive approve <goal>`, which lists it
//! first. Until then its task is not offered or claimable here and `run` refuses it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::board::Task;
use super::goal::{Check, Goal};

/// What a failed check's verdict carries of its output.
pub const TAIL_CHARS: usize = 2000;
/// What a log file keeps of it.
const LOG_BYTES: usize = 8 * 1024;
/// Failed-run logs kept per goal.
const KEEP_LOGS: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail { check: String, tail: String },
    Timeout { check: String, after: Duration },
}

impl Verdict {
    /// The check that did not pass, if any.
    pub fn check(&self) -> Option<&str> {
        match self {
            Verdict::Pass => None,
            Verdict::Fail { check, .. } | Verdict::Timeout { check, .. } => Some(check),
        }
    }
}

/// The checks that decide `task`: its own, else the goal's.
pub fn checks<'a>(goal: &'a Goal, task: &'a Task) -> &'a [Check] {
    if task.accept.is_empty() { &goal.oracle } else { &task.accept }
}

/// Where a check runs: `cwd` for an empty `repo`; else `repo` as a directory under `cwd`, or beside
/// the repo `cwd` is in (a sibling checkout), or as an absolute path.
fn dir_of(check: &Check, cwd: &Path) -> PathBuf {
    if check.repo.is_empty() {
        return cwd.to_path_buf();
    }
    let p = Path::new(&check.repo);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    let root = PathBuf::from(borg_trace::repo_key(&cwd.display().to_string()));
    [cwd.join(p), root.parent().map(|r| r.join(p)).unwrap_or_default()].into_iter().find(|d| d.is_dir()).unwrap_or_else(|| cwd.join(p))
}

/// The last `n` characters of `text`.
fn tail(text: &str, n: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(n)).collect()
}

/// `<goal>/approved.jsonl`: the checks this node approved (`approve`), by fingerprint. Local: never synced.
pub fn approved_path(home: &Path, goal: &str) -> PathBuf {
    super::dir(home, goal).join("approved.jsonl")
}

/// A check's identity: blake3 of its JSON line (name, command, repo, timeout), 32 hex digits.
pub fn fingerprint(c: &Check) -> String {
    blake3::hash(serde_json::to_string(c).unwrap_or_default().as_bytes()).to_hex()[..32].to_string()
}

/// One line of approved.jsonl; the command is kept for the engineer reading the file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Approval {
    v: u8,
    h: String,
    ms: i64,
    #[serde(default)]
    cmd: String,
}

/// The checks this node runs for a goal: its goal.json oracle (made here) and those in approved.jsonl.
#[derive(Debug, Clone, Default)]
pub struct Approvals {
    local: Vec<Check>,
    set: HashSet<String>,
}

impl Approvals {
    pub fn load(home: &Path, goal: &str) -> Approvals {
        Approvals {
            local: super::goal::local(home, goal).map(|g| g.oracle).unwrap_or_default(),
            set: super::read_lines::<Approval>(&approved_path(home, goal)).into_iter().map(|a| a.h).collect(),
        }
    }

    pub fn allows(&self, c: &Check) -> bool {
        self.local.contains(c) || self.set.contains(&fingerprint(c))
    }

    /// The checks of `task` (`checks`) not approved here.
    pub fn pending<'a>(&self, goal: &'a Goal, task: &'a Task) -> Vec<&'a Check> {
        checks(goal, task).iter().filter(|c| !self.allows(c)).collect()
    }
}

/// Whether every check of `task` is approved on this node.
pub fn approved(home: &Path, goal: &Goal, task: &Task) -> bool {
    Approvals::load(home, &goal.id).pending(goal, task).is_empty()
}

/// Approves `checks` for `goal` on this node (those not approved yet); returns how many it added.
///
/// # Errors
/// When approved.jsonl cannot be written.
pub fn approve(home: &Path, goal: &str, checks: &[Check], now: i64) -> std::io::Result<usize> {
    let have = Approvals::load(home, goal);
    let mut seen = HashSet::new();
    let mut n = 0;
    for c in checks.iter().filter(|c| !have.allows(c) && seen.insert(fingerprint(c))) {
        super::append_line(&approved_path(home, goal), &Approval { v: 1, h: fingerprint(c), ms: now, cmd: c.cmd.clone() })?;
        n += 1;
    }
    Ok(n)
}

/// Every check of goal `g` waiting for approval on this node: the goal's oracle (when it came from a
/// peer) and each task's own, as (where: "goal" or the task id, the check), once per check.
pub fn unapproved<'a>(home: &Path, g: &'a Goal, b: &'a super::Board) -> Vec<(String, &'a Check)> {
    let have = Approvals::load(home, &g.id);
    let mut seen = HashSet::new();
    let goal = g.oracle.iter().map(|c| ("goal".to_string(), c));
    let tasks = b.tasks.values().flat_map(|s| s.task.accept.iter().map(|c| (s.task.id.clone(), c)));
    goal.chain(tasks).filter(|(_, c)| !have.allows(c) && seen.insert(fingerprint(c))).collect()
}

/// Why `run` refuses checks not approved here.
pub fn refusal(goal: &str, pending: &[&Check]) -> String {
    let cmds: Vec<String> = pending.iter().map(|c| format!("{}: {}", c.name, c.cmd)).collect();
    format!("not approved on this node: another node wrote these checks ({}); the engineer reviews them and runs `borg hive approve {goal}`", cmds.join("; "))
}

/// Runs the checks for `task` of `goal` in `cwd` (`exec`) when this node approved every one of them
/// (`Approvals`); else a failure that names them, and nothing runs.
pub fn run(home: &Path, goal: &Goal, task: &Task, cwd: &Path, budget: Option<Duration>) -> Verdict {
    let pending = Approvals::load(home, &goal.id).pending(goal, task);
    if let Some(first) = pending.first() {
        return Verdict::Fail { check: first.name.clone(), tail: refusal(&goal.id, &pending) };
    }
    exec(goal, task, cwd, budget)
}

/// Runs the checks for `task` of `goal` in `cwd`, within `budget` in all (None: each check's own
/// timeout only). The children never re-enter the hive or learning: a test suite that runs borg's own
/// hooks does not recurse.
fn exec(goal: &Goal, task: &Task, cwd: &Path, budget: Option<Duration>) -> Verdict {
    let started = Instant::now();
    for c in checks(goal, task) {
        let own = Duration::from_secs(c.timeout_s.max(1));
        let cap = match budget {
            Some(b) => own.min(b.saturating_sub(started.elapsed())),
            None => own,
        };
        if cap.is_zero() {
            return Verdict::Timeout { check: c.name.clone(), after: Duration::ZERO };
        }
        let mut cmd = Command::new("sh");
        // Stderr joins stdout, so the tail shows the failure.
        cmd.arg("-c")
            .arg(format!("exec 2>&1\n{}", c.cmd))
            .current_dir(dir_of(c, cwd))
            .env("BORG_LEARN", "off")
            .env(super::ENV, "")
            .env_remove(super::DRONE_ENV);
        let t0 = Instant::now();
        match crate::runner::run_capped_status(cmd, None, cap) {
            Some((status, _)) if status.success() => {}
            Some((status, out)) => {
                let text = String::from_utf8_lossy(&out);
                let mut t = tail(&text, TAIL_CHARS);
                if t.trim().is_empty() {
                    t = format!("(no output; exit {})", status.code().map_or("by signal".into(), |c| c.to_string()));
                }
                return Verdict::Fail { check: c.name.clone(), tail: t };
            }
            // Killed at the cap, or it could not start (a missing directory) well before it.
            None if t0.elapsed() + Duration::from_millis(50) >= cap => return Verdict::Timeout { check: c.name.clone(), after: t0.elapsed() },
            None => return Verdict::Fail { check: c.name.clone(), tail: format!("could not run `{}` in {}", c.cmd, dir_of(c, cwd).display()) },
        }
    }
    Verdict::Pass
}

/// Keeps a failed verdict's tail in the goal's oracle logs (the newest `KEEP_LOGS`); returns the file.
pub fn log(home: &Path, goal: &str, task: &str, v: &Verdict, ms: i64) -> Option<PathBuf> {
    let text = match v {
        Verdict::Pass => return None,
        Verdict::Fail { check, tail } => format!("check {check} failed\n{tail}"),
        Verdict::Timeout { check, after } => format!("check {check} timed out after {} ms\n", after.as_millis()),
    };
    let dir = super::dir(home, goal).join("oracle");
    std::fs::create_dir_all(&dir).ok()?;
    let start = text.len().saturating_sub(LOG_BYTES);
    let start = (start..text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(text.len());
    let path = dir.join(format!("{}.{ms}.log", task.replace(['/', '\\'], "_")));
    std::fs::write(&path, &text[start..]).ok()?;
    prune(&dir);
    Some(path)
}

/// Removes all but the newest `KEEP_LOGS` logs (by their ms).
fn prune(dir: &Path) {
    let mut logs: Vec<(i64, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let ms = p.file_stem()?.to_str()?.rsplit('.').next()?.parse().ok()?;
            Some((ms, p))
        })
        .collect();
    if logs.len() <= KEEP_LOGS {
        return;
    }
    logs.sort();
    for (_, p) in &logs[..logs.len() - KEEP_LOGS] {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::board::Role;

    fn goal(cmds: &[&str]) -> Goal {
        Goal {
            v: 1,
            id: "g".into(),
            spec: String::new(),
            oracle: cmds.iter().enumerate().map(|(i, c)| Check { name: format!("c{i}"), cmd: c.to_string(), repo: String::new(), timeout_s: 60 }).collect(),
            budget_usd: 0.0,
            repos: vec![],
            author: String::new(),
            created_ms: 0,
            sig: String::new(),
            hidden: None,
            held_out: false,
        }
    }

    fn task() -> Task {
        Task {
            id: "t1".into(),
            role: Role::Work,
            title: String::new(),
            repo: String::new(),
            files: vec![],
            deps: vec![],
            accept: vec![],
            of: None,
            not_by: vec![],
            flags: vec![],
            from: None,
        }
    }

    fn gone(pid: i32) -> bool {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        // Gone, or a zombie waiting for its new parent to reap it.
        stat.is_empty() || stat.rsplit(')').next().is_some_and(|r| r.trim_start().starts_with('Z'))
    }

    #[test]
    fn pass_fail_and_timeout() {
        let dir = std::env::temp_dir().join(format!("borg-hive-oracle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cap = Some(Duration::from_millis(200));
        assert_eq!(exec(&goal(&["true", "true"]), &task(), &dir, cap), Verdict::Pass);
        let v = exec(&goal(&["true", "echo boom >&2; false", "true"]), &task(), &dir, cap);
        assert_eq!(v, Verdict::Fail { check: "c1".into(), tail: "boom\n".into() }, "stderr is in the tail; the first failure decides");
        let pid_file = dir.join("pid");
        let started = Instant::now();
        let v = exec(&goal(&[&format!("sleep 5 & echo $! > {}; wait", pid_file.display())]), &task(), &dir, cap);
        assert!(matches!(v, Verdict::Timeout { ref check, .. } if check == "c0"), "{v:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !gone(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(gone(pid), "the check's whole process group is killed");
        assert!(matches!(exec(&goal(&["true"]), &task(), &dir, Some(Duration::ZERO)), Verdict::Timeout { .. }), "no budget left: nothing runs");
        // The children run without the hive and without learning.
        let env = exec(&goal(&["test \"$BORG_LEARN\" = off && test -z \"$BORG_HIVE\""]), &task(), &dir, None);
        assert_eq!(env, Verdict::Pass);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A check another node wrote (the synced goal's oracle, a remote task's accept) never runs before
    /// the engineer approves it here; this node's goal.json oracle and the checks of a task it opened do.
    #[test]
    fn checks_from_another_node_run_only_once_approved() {
        let home = std::env::temp_dir().join(format!("borg-hive-oracle-approve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let work = home.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let ran = work.join("ran");
        let mut g = goal(&[&format!("touch {}", ran.display())]);
        // A peer's goal: only its synced projection is here.
        std::fs::create_dir_all(crate::hive::dir(&home, "g")).unwrap();
        std::fs::write(crate::hive::goal_jsonl_path(&home, "g"), format!("{}\n", crate::hive::goal::projection(&g))).unwrap();
        let v = run(&home, &g, &task(), &work, None);
        assert!(matches!(&v, Verdict::Fail { tail, .. } if tail.contains("not approved") && tail.contains("borg hive approve g")), "{v:?}");
        assert!(!ran.exists(), "nothing ran");
        let remote = Task { accept: vec![Check::parse(&format!("evil=touch {}", work.join("evil").display()), 60).unwrap()], ..task() };
        let b = crate::hive::board::fold(&[crate::hive::Event::open(remote.clone(), "engineer:peer", 1)]);
        let listed: Vec<String> = unapproved(&home, &g, &b).iter().map(|(at, c)| format!("{at} {}", c.name)).collect();
        assert_eq!(listed, ["goal c0", "t1 evil"]);
        assert!(!approved(&home, &g, &remote) && matches!(run(&home, &g, &remote, &work, None), Verdict::Fail { .. }));
        assert!(!work.join("evil").exists());
        assert_eq!(approve(&home, "g", &g.oracle, 5).unwrap(), 1);
        assert_eq!(approve(&home, "g", &g.oracle, 6).unwrap(), 0, "once");
        assert_eq!(run(&home, &g, &task(), &work, None), Verdict::Pass);
        assert!(ran.exists() && !approved(&home, &g, &remote), "approving the goal's oracle approves no task's own check");
        // A task this node opens approves its checks (hive::write); a changed command is another check.
        crate::hive::write(&home, "g", vec![crate::hive::Event::open(remote.clone(), "engineer:me", 2)]).unwrap();
        assert!(approved(&home, &g, &remote));
        let changed = Task { accept: vec![Check { cmd: "true".into(), ..remote.accept[0].clone() }], ..task() };
        assert!(!approved(&home, &g, &changed));
        // This node's own goal.json oracle runs unapproved.
        g.id = "mine".into();
        g.oracle = vec![Check::parse("c=true", 60).unwrap()];
        crate::hive::goal::create(&home, &g).unwrap();
        assert_eq!(run(&home, &g, &task(), &work, None), Verdict::Pass);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn failed_runs_are_logged_and_pruned() {
        let home = std::env::temp_dir().join(format!("borg-hive-oracle-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let v = Verdict::Fail { check: "c".into(), tail: "x".repeat(20_000) };
        for ms in 0..25 {
            log(&home, "g", "t1", &v, ms).unwrap();
        }
        let dir = crate::hive::dir(&home, "g").join("oracle");
        let mut names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names.len(), KEEP_LOGS);
        assert!(!names.contains(&"t1.0.log".to_string()) && names.contains(&"t1.24.log".to_string()));
        assert!(std::fs::metadata(dir.join("t1.24.log")).unwrap().len() <= LOG_BYTES as u64);
        assert_eq!(log(&home, "g", "t1", &Verdict::Pass, 99), None);
        let _ = std::fs::remove_dir_all(&home);
    }
}
