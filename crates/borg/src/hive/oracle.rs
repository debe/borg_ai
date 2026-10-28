//! The oracle: what gates `done`. A task's `accept` checks (else the goal's oracle) run one after the
//! other, each `sh -c` in its repo's directory (the worktree root for an empty `repo`, whichever
//! subdirectory the caller's shell is in), bounded by its own timeout and by what is left of the
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

/// The root of the worktree `cwd` is in, else `cwd` itself (outside git). A check is written against
/// the root (`cd sub && make test`), and a drone's shell may have moved into a subdirectory.
fn root_of(cwd: &Path) -> PathBuf {
    crate::notes::toplevel(&cwd.display().to_string()).unwrap_or_else(|| cwd.to_path_buf())
}

/// Where a check runs, for `root` a worktree root (`root_of`): `root` for an empty `repo`; else `repo`
/// as a directory under `root`, or beside the repo `root` is in (a sibling checkout), or as an
/// absolute path.
fn dir_of(check: &Check, root: &Path) -> PathBuf {
    if check.repo.is_empty() {
        return root.to_path_buf();
    }
    let p = Path::new(&check.repo);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    // Under the worktree itself first (a drone's `.worktrees/hive-…` checkout, never the main one
    // `repo_key` folds it onto); the folded key only finds a sibling checkout.
    let key = PathBuf::from(borg_trace::repo_key(&root.display().to_string()));
    [root.join(p), key.parent().map(|r| r.join(p)).unwrap_or_default()].into_iter().find(|d| d.is_dir()).unwrap_or_else(|| root.join(p))
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

/// Runs the checks for `task` of `goal` from the root of the worktree `cwd` is in, within `budget` in all (None: each check's own
/// timeout only). The children never re-enter the hive or learning: a test suite that runs borg's own
/// hooks does not recurse.
fn exec(goal: &Goal, task: &Task, cwd: &Path, budget: Option<Duration>) -> Verdict {
    let started = Instant::now();
    let root = root_of(cwd);
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
            .current_dir(dir_of(c, &root))
            .env("BORG_LEARN", "off")
            .env(super::ENV, "")
            .env_remove(super::DRONE_ENV);
        let t0 = Instant::now();
        match crate::runner::run_capped_status(cmd, None, cap) {
            Some((status, _)) if status.success() => {}
            Some((status, out)) => {
                let text = String::from_utf8_lossy(&out);
                // Redacted whole, before the cut: a tail that starts past a secret's key would keep its
                // value (a failure's tail reaches the synced board, integrate.rs and verdict.rs).
                let mut t = tail(&crate::notes::redact(&text), TAIL_CHARS);
                if t.trim().is_empty() {
                    t = format!("(no output; exit {})", status.code().map_or("by signal".into(), |c| c.to_string()));
                }
                return Verdict::Fail { check: c.name.clone(), tail: t };
            }
            // Killed at the cap, or it could not start (a missing directory) well before it.
            None if t0.elapsed() + Duration::from_millis(50) >= cap => return Verdict::Timeout { check: c.name.clone(), after: t0.elapsed() },
            None => return Verdict::Fail { check: c.name.clone(), tail: format!("could not run `{}` in {}", c.cmd, dir_of(c, &root).display()) },
        }
    }
    Verdict::Pass
}

/// What one run of a check on the tree as it stands says of the check itself (`probe`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// It ran: passed or failed as a check does.
    Ran,
    /// It cannot pass on any tree: not valid shell, or it failed by its own usage (`unusable`); why.
    Unusable(String),
    /// Still running at the bound: nothing said of it.
    Unknown,
}

/// Runs check `c` once from the root of the worktree `cwd` is in, as `exec` does, within `within`:
/// a syntax check (`sh -n`), then the check itself. `Unusable` only when no change to the tree makes
/// it pass (a task never opens with such a check, and never fails to open with any other):
/// - exit 127 whose output says the check's command word (`word`) is not found, the word no path and
///   no command here: a task cannot install a system command (`bash scripts/x.sh` naming a script the
///   task creates exits 127 too, and opens);
/// - exit 2, 4, 64, 126, 128 or 129 with a usage error at the head of its output (`usage_error`), from
///   a system tool that reads no option from the tree (`foreign`, `FIXED_USAGE`), with no script of the
///   repo named: its own usage, which no tree changes (`git check-ignore -q a b` exits 128 on any tree;
///   `git show HEAD:new`, a repo script's flag the task adds, and a test runner's flag a conftest.py
///   the task writes registers, open).
///
/// A check still running at the bound is `Unknown`, never a refusal: a slow suite is a good check.
pub fn probe(c: &Check, cwd: &Path, within: Duration) -> Probe {
    let root = root_of(cwd);
    // Stderr joins stdout (run_bounded reads stdout alone), so the refusal shows the shell's words.
    let mut syntax = Command::new("sh");
    syntax.args(["-c", "exec 2>&1; sh -n -c \"$1\"", "probe", &c.cmd]).current_dir(&root);
    match crate::runner::run_capped_status(syntax, None, within) {
        Some((s, out)) if !s.success() => return Probe::Unusable(format!("not valid shell: {}", tail(&String::from_utf8_lossy(&out), 300).trim())),
        Some(_) => {}
        None => return Probe::Unknown,
    }
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(format!("exec 2>&1\n{}", c.cmd)).current_dir(dir_of(c, &root)).env("BORG_LEARN", "off").env(super::ENV, "").env_remove(super::DRONE_ENV);
    let Some((status, out)) = crate::runner::run_capped_status(cmd, None, within) else { return Probe::Unknown };
    let text = crate::notes::redact(&String::from_utf8_lossy(&out));
    let code = status.code();
    let dir = dir_of(c, &root);
    let words = words(&c.cmd);
    let first = words.first().map_or("", String::as_str);
    let unusable = match code {
        Some(127) => !first.contains('/') && not_found(&text, first) && resolve(first, &dir).is_none(),
        Some(2 | 4 | 64 | 126 | 128 | 129) => usage_error(&text) && foreign(&words, &dir, &root),
        _ => false,
    };
    match unusable {
        true => Probe::Unusable(format!("exit {}: {}", code.unwrap_or_default(), tail(&text, 300).trim())),
        false => Probe::Ran,
    }
}

/// The words of a check's first simple command, its leading `NAME=value` assignments skipped (quotes
/// are not read: a quoted word with a space is two, which only makes `foreign` see a script less).
fn words(cmd: &str) -> Vec<String> {
    let head = cmd.split(['\n', ';', '|', '&']).next().unwrap_or_default();
    let assign = |w: &str| w.split_once('=').is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    head.split_whitespace().skip_while(|w| assign(w)).map(|w| w.trim_matches(['\'', '"']).to_string()).collect()
}

/// Whether the shell's own words in `out` say `word` was not found (`sh: 1: x: not found`, `bash: x:
/// command not found`), not some later command or a script's missing file.
fn not_found(out: &str, word: &str) -> bool {
    !word.is_empty() && out.lines().any(|l| l.contains(&format!("{word}: not found")) || l.contains(&format!("{word}: command not found")))
}

/// Where `word` runs from in `dir` (`command -v`): an absolute path, or the word itself for a shell
/// builtin; None when no such command is here.
fn resolve(word: &str, dir: &Path) -> Option<String> {
    let mut c = Command::new("sh");
    c.args(["-c", "command -v \"$1\"", "probe", word]).current_dir(dir);
    let (s, out) = crate::runner::run_capped_status(c, None, Duration::from_secs(2))?;
    let got = String::from_utf8_lossy(&out).trim().to_string();
    (s.success() && !got.is_empty()).then_some(got)
}

/// System tools whose options no file of a repo's tree adds to: a test runner or a build tool reads
/// plugins and config from the tree (pytest's `pytest_addoption`, npm's scripts), so its usage error is
/// never taken for one no change fixes.
const FIXED_USAGE: &[&str] = &["git", "grep", "egrep", "fgrep", "diff", "cmp", "find", "ls", "cat", "head", "tail", "wc", "sort", "sed", "awk", "jq"];

/// Whether a check's usage error is its command's own, from outside the repo at `root`: its command
/// word resolves to a program outside the repo (never a builtin, never one the tree holds) that reads
/// no option from the tree (`FIXED_USAGE`), and no later word names a script or `-m` module of the tree
/// (an interpreter's script owns the usage).
fn foreign(words: &[String], dir: &Path, root: &Path) -> bool {
    let Some(first) = words.first() else { return false };
    let Some(at) = resolve(first, dir).filter(|p| p.starts_with('/')) else { return false };
    if !Path::new(&at).file_name().and_then(|n| n.to_str()).is_some_and(|n| FIXED_USAGE.contains(&n)) {
        return false;
    }
    let inside = |p: &Path| {
        let (p, r) = (std::fs::canonicalize(p).unwrap_or(p.to_path_buf()), std::fs::canonicalize(root).unwrap_or(root.to_path_buf()));
        p.starts_with(r)
    };
    if inside(Path::new(&at)) {
        return false;
    }
    let script = |w: &str| {
        let p = dir.join(w);
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default();
        p.is_file() && (["py", "sh", "bash", "js", "mjs", "cjs", "ts", "rb", "pl", "php", "lua"].contains(&ext) || executable(&p))
    };
    let module = |m: &str| {
        let p = dir.join(m.replace('.', "/"));
        p.is_dir() || p.with_extension("py").is_file()
    };
    !words.iter().skip(1).enumerate().any(|(i, w)| script(w) || (w == "-m" && words.get(i + 2).is_some_and(|m| module(m))))
}

#[cfg(unix)]
fn executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(_: &Path) -> bool {
    false
}

/// Whether the head of a check's output is a usage error: what a tool prints for a flag it does not
/// take, not a failing test's report (that comes later in the output, and with another exit).
fn usage_error(out: &str) -> bool {
    out.lines().take(3).any(|l| {
        let l = l.to_lowercase();
        ["usage:", "only valid with", "unknown option", "unrecognized option", "invalid option"].iter().any(|p| l.contains(p))
    })
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
            origin: Vec::new(),
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
            group: None,
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
        // Pass and fail get room to spawn on a loaded runner; only the timeout case is tight.
        let roomy = Some(Duration::from_secs(30));
        assert_eq!(exec(&goal(&["true", "true"]), &task(), &dir, roomy), Verdict::Pass);
        let v = exec(&goal(&["true", "echo boom >&2; false", "true"]), &task(), &dir, roomy);
        assert_eq!(v, Verdict::Fail { check: "c1".into(), tail: "boom\n".into() }, "stderr is in the tail; the first failure decides");
        // A tail cut just past a secret's key would keep its value: the output is redacted before the cut.
        let v = exec(&goal(&[&format!("printf 'password=hunter2 %0{}d' 0; false", TAIL_CHARS - 8)]), &task(), &dir, roomy);
        let Verdict::Fail { tail: t, .. } = v else { panic!("{v:?}") };
        assert!(!t.contains("hunter2") && t.contains("DACTED]"), "{}", &t[..40]);
        let pid_file = dir.join("pid");
        let cap = Some(Duration::from_millis(200));
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

    /// A check written against the worktree root (`cd sub && ...`, or `repo` = `sub`) passes from a
    /// subdirectory too: a drone whose shell is already in `sub` runs `borg hive done` there.
    #[test]
    fn checks_run_from_the_worktree_root_whatever_the_callers_directory() {
        let dir = std::env::temp_dir().join(format!("borg-hive-oracle-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sub = dir.join("sub");
        std::fs::create_dir_all(sub.join("deep")).unwrap();
        std::fs::write(sub.join("marker"), "").unwrap();
        let st = std::process::Command::new("git").args(["init", "-q"]).current_dir(&dir).status().unwrap();
        assert!(st.success());
        let roomy = Some(Duration::from_secs(30));
        let g = goal(&["cd sub && test -f marker", "test -d sub"]);
        for cwd in [dir.clone(), sub.clone(), sub.join("deep")] {
            assert_eq!(exec(&g, &task(), &cwd, roomy), Verdict::Pass, "from {}", cwd.display());
        }
        let mut in_repo = goal(&["test -f marker"]);
        in_repo.oracle[0].repo = "sub".into();
        assert_eq!(exec(&in_repo, &task(), &sub.join("deep"), roomy), Verdict::Pass, "a check's repo is under the root, not the caller");
        // In a linked worktree (a drone's `.worktrees/hive-…`), `repo` resolves in that worktree, not
        // in the main checkout `repo_key` folds it onto.
        let git = |args: &[&str]| assert!(std::process::Command::new("git").args(args).current_dir(&dir).status().unwrap().success());
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "base"]);
        git(&["worktree", "add", "-q", ".worktrees/hive-g-1"]);
        let wt = dir.join(".worktrees/hive-g-1");
        std::fs::create_dir_all(wt.join("sub")).unwrap();
        std::fs::write(wt.join("sub/drone-only"), "").unwrap();
        let mut drone = goal(&["test -f drone-only"]);
        drone.oracle[0].repo = "sub".into();
        assert_eq!(exec(&drone, &task(), &wt.join("sub"), roomy), Verdict::Pass, "a check's repo is in the drone's worktree, not the main checkout");
        // Outside git the caller's directory is the root.
        let plain = std::env::temp_dir().join(format!("borg-hive-oracle-plain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&plain);
        std::fs::create_dir_all(plain.join("sub")).unwrap();
        assert_eq!(exec(&goal(&["test -d sub"]), &task(), &plain, roomy), Verdict::Pass);
        let _ = std::fs::remove_dir_all(&plain);
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
