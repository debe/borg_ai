//! The oracle: what gates `done`. A task's `accept` checks (else the goal's oracle) run one after the
//! other, each `sh -c` in its repo's directory, bounded by its own timeout and by what is left of the
//! caller's budget (the stop hook's, or none for `borg hive done`); the first one that does not pass
//! decides. A failed run's output tail is kept in `<goal>/oracle/<task>.<ms>.log` (the newest
//! `KEEP_LOGS`), for the drone and its verifier.

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

/// Runs the checks for `task` of `goal` in `cwd`, within `budget` in all (None: each check's own
/// timeout only). The children never re-enter the hive or learning: a test suite that runs borg's own
/// hooks does not recurse.
pub fn run(goal: &Goal, task: &Task, cwd: &Path, budget: Option<Duration>) -> Verdict {
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
        assert_eq!(run(&goal(&["true", "true"]), &task(), &dir, cap), Verdict::Pass);
        let v = run(&goal(&["true", "echo boom >&2; false", "true"]), &task(), &dir, cap);
        assert_eq!(v, Verdict::Fail { check: "c1".into(), tail: "boom\n".into() }, "stderr is in the tail; the first failure decides");
        let pid_file = dir.join("pid");
        let started = Instant::now();
        let v = run(&goal(&[&format!("sleep 5 & echo $! > {}; wait", pid_file.display())]), &task(), &dir, cap);
        assert!(matches!(v, Verdict::Timeout { ref check, .. } if check == "c0"), "{v:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !gone(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(gone(pid), "the check's whole process group is killed");
        assert!(matches!(run(&goal(&["true"]), &task(), &dir, Some(Duration::ZERO)), Verdict::Timeout { .. }), "no budget left: nothing runs");
        // The children run without the hive and without learning.
        let env = run(&goal(&["test \"$BORG_LEARN\" = off && test -z \"$BORG_HIVE\""]), &task(), &dir, None);
        assert_eq!(env, Verdict::Pass);
        let _ = std::fs::remove_dir_all(&dir);
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
