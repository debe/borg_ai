//! The verdict on a hive task and its reward (docs/design/hive-learning.md "Reward", steps 2 and 8).
//!
//! A verifier drone ends its verify task with `VERDICT: pass` or `VERDICT: reject <why>` (its last
//! message, read at the stop) or `borg hive verify --pass|--reject`. [`record`] then checks the work
//! itself, whatever the verifier said:
//! - the author never judges its own work (its session, or any session of its drone);
//! - the work's diff, from a snapshot of the claimant's tree at the claim (`base`) to one at the done
//!   (`head`, [`snapshot`], [`diff`]), may not weaken tests or change CI outside the task's own
//!   `files`, nor change the visible check's own files at all ([`protected`]: tests may grow, never
//!   shrink), and the drone may not have touched the oracle's files (guard.rs flags). Either makes the
//!   verdict `rejected` with `hacked`, a done that passed the visible oracle by changing it; the work
//!   reopens with the paths to restore and why.
//! - work with deps must have started from a tree holding each dep's verified work: its claim's
//!   `deps_in` names every transitive dep's accepted head ([`accepted`], brought in by [`bring_in`] at
//!   the claim), or a pass is an unchecked reject ([`NO_DEPS`]).
//!
//! The reward: every lesson shown to the author's session while it held the claim
//! (veto.rs `shown_between`) gets the verdict as an outcome (outcome.rs `verified`, `rejected`,
//! `hacked`), once per attempt (`cause` = `<goal>/<verify task>`). With a hidden oracle (hidden.rs) a
//! pass or a reject waits for its score, which then decides `verified` or `rejected`; `hacked` is
//! final at once. Each final verdict counts one `task_*` stat. BORG_HIVE_CREDIT=off (the frozen arm of
//! an eval) credits nothing; the board and the stats still record.

use std::collections::HashMap;
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
/// A whole [`diff`]: its two git calls (inside the stop hook's 60 s default budget).
pub const DIFF_TIMEOUT: Duration = Duration::from_secs(2 * GIT_TIMEOUT.as_secs());
/// What a reason quotes of a line.
const EXCERPT: usize = 80;
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

/// `git -C dir args` (git.rs) within `GIT_TIMEOUT` and before `deadline`: its trimmed stdout on success.
fn git_by(dir: &Path, args: &[&str], env: &[(&str, &str)], deadline: Instant) -> Option<String> {
    let left = deadline.saturating_duration_since(Instant::now()).min(GIT_TIMEOUT);
    if left.is_zero() {
        return None;
    }
    let mut cmd = super::git::command(dir);
    cmd.args(args).envs(env.iter().copied());
    match runner::run_capped_status(cmd, None, left)? {
        (s, out) if s.success() => Some(String::from_utf8_lossy(&out).trim().to_string()),
        _ => None,
    }
}

/// A commit of `dir`'s working tree as it is (tracked and untracked files, `.gitignore` respected),
/// written without touching the index, HEAD or any ref: a scratch index copied from the real one (so
/// unchanged files are not hashed again), `add -A`, `write-tree`, `commit-tree`, all `within` (each
/// git call also within `GIT_TIMEOUT`: the stop hook passes what is left of its time). None outside a
/// git repo or on any failure: the verdict then cannot check the work's diff, and says so.
///
/// The scratch index lies beside the real one, in the worktree's git dir: a fresh name created anew
/// (never a planted file or link followed), removed with the lock git takes on it, which an `add -A`
/// cut off at the deadline leaves behind.
pub fn snapshot(dir: &Path, within: Duration) -> Option<String> {
    let deadline = Instant::now() + within;
    let git = |dir: &Path, args: &[&str], env: &[(&str, &str)]| git_by(dir, args, env, deadline);
    let index = dir.join(git(dir, &["rev-parse", "--git-path", "index"], &[])?);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let name = format!("borg-hive-index-{}-{nanos}", std::process::id());
    let (tmp, lock) = (index.with_file_name(&name), index.with_file_name(format!("{name}.lock")));
    let clean = || [&tmp, &lock].iter().for_each(|p| drop(std::fs::remove_file(p)));
    // The copy keeps the index's mtime: git re-reads an entry as old as its index (racy git), and a
    // fresh mtime would make it trust a same-size edit made in the second the index was written.
    if index.is_file() && copy_keeping_mtime(&index, &tmp).is_err() {
        clean();
        return None;
    }
    let tmp_s = tmp.display().to_string();
    let env = [("GIT_INDEX_FILE", tmp_s.as_str())];
    let tree = git(dir, &["add", "-A"], &env).and_then(|_| git(dir, &["write-tree"], &env));
    clean();
    let tree = tree?;
    let who = [("GIT_AUTHOR_NAME", "borg"), ("GIT_AUTHOR_EMAIL", "borg@localhost"), ("GIT_COMMITTER_NAME", "borg"), ("GIT_COMMITTER_EMAIL", "borg@localhost")];
    let mut args = vec!["commit-tree", tree.as_str(), "-m", "borg hive snapshot"];
    let parent = git(dir, &["rev-parse", "--verify", "-q", "HEAD"], &[]);
    if let Some(p) = parent.as_deref() {
        args.extend(["-p", p]);
    }
    git(dir, &args, &who).filter(|c| !c.is_empty())
}

/// `from` copied to `to`, a file it creates (none may be there), with `from`'s mtime.
fn copy_keeping_mtime(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut out = std::fs::File::options().write(true).create_new(true).open(to)?;
    std::io::copy(&mut std::fs::File::open(from)?, &mut out)?;
    out.set_modified(std::fs::metadata(from)?.modified()?)
}

/// `git -C dir args` (git.rs; fed `input`) within `GIT_TIMEOUT` and before `deadline`, its stderr merged into
/// its stdout: (success, untrimmed output). None when it did not run or finish.
fn git_out(dir: &Path, args: &[&str], input: Option<&[u8]>, deadline: Instant) -> Option<(bool, String)> {
    let left = deadline.saturating_duration_since(Instant::now()).min(GIT_TIMEOUT);
    if left.is_zero() {
        return None;
    }
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", "exec 2>&1; exec git \"$@\"", "git"]).args(super::git::args(dir)).args(args).env("GIT_TERMINAL_PROMPT", "0");
    runner::run_capped_status(cmd, input, left).map(|(s, out)| (s.success(), String::from_utf8_lossy(&out).into_owned()))
}

/// What a refusal quotes of git's output: its last lines.
fn git_tail(out: &str) -> String {
    let lines: Vec<&str> = out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(3)..].join("; ")
}

/// The toplevel of the worktree `dir` is in.
fn toplevel(dir: &Path, deadline: Instant) -> Option<String> {
    git_by(dir, &["rev-parse", "--show-toplevel"], &[], deadline)
}

/// The change from `base` to `head` (commits of `dir`'s repo) as a binary patch, and `dir`'s toplevel,
/// where it applies: from a subdirectory `git apply` would skip every path outside it.
fn patch_of(dir: &Path, base: &str, head: &str, deadline: Instant) -> Result<(String, Vec<u8>), String> {
    let top = toplevel(dir, deadline).ok_or_else(|| format!("{} is in no git worktree", dir.display()))?;
    let mut cmd = super::git::command(Path::new(&top));
    cmd.args(["diff", "--binary", "--no-color", "--no-ext-diff", "--no-textconv", base, head]);
    let left = deadline.saturating_duration_since(Instant::now()).min(GIT_TIMEOUT);
    match (!left.is_zero()).then(|| runner::run_capped_status(cmd, None, left)).flatten() {
        Some((s, out)) if s.success() => Ok((top, out)),
        _ => Err(format!("git cannot diff {base}..{head} here (not this repo's snapshots, or out of time)")),
    }
}

/// Puts a dependency's own change (`base..head`, the snapshots of its accepted attempt) into `dir`'s
/// working tree, all `within`, without touching the index, HEAD or any ref: `git apply --check`, then
/// `git apply`, at the toplevel. `Ok(true)` when it applied, `Ok(false)` when the change is in already
/// (its reverse applies: the dep's author drone claims the dependent in its own tree, or a rework in a
/// tree it was brought into before) or empty.
///
/// # Errors
/// Neither applies (the tree's own changes overlap it), or git failed or ran out of time: the tree is
/// as it was, and the reason ends with git's last lines.
pub fn bring_in(dir: &Path, base: &str, head: &str, within: Duration) -> Result<bool, String> {
    let deadline = Instant::now() + within;
    let (top, patch) = patch_of(dir, base, head, deadline)?;
    if patch.is_empty() {
        return Ok(false);
    }
    let top = Path::new(&top);
    let apply = |args: &[&str]| git_out(top, &[&["apply", "--whitespace=nowarn"], args].concat(), Some(&patch), deadline);
    match apply(&["--check"]) {
        Some((true, _)) => match apply(&[]) {
            Some((true, _)) => Ok(true),
            Some((false, out)) => Err(git_tail(&out)),
            None => Err("git apply did not finish".into()),
        },
        Some((false, out)) => match apply(&["-R", "--check"]) {
            Some((true, _)) => Ok(false),
            _ => Err(git_tail(&out)),
        },
        None => Err("git apply --check did not finish".into()),
    }
}

/// Undoes a [`bring_in`] that applied (`git apply -R` of the same change), `within` that time.
///
/// # Errors
/// The reverse does not apply, or git failed or ran out of time.
pub fn take_out(dir: &Path, base: &str, head: &str, within: Duration) -> Result<(), String> {
    let deadline = Instant::now() + within;
    let (top, patch) = patch_of(dir, base, head, deadline)?;
    if patch.is_empty() {
        return Ok(());
    }
    match git_out(Path::new(&top), &["apply", "--whitespace=nowarn", "-R"], Some(&patch), deadline) {
        Some((true, _)) => Ok(()),
        Some((false, out)) => Err(git_tail(&out)),
        None => Err("git apply -R did not finish".into()),
    }
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
    changes_by(dir, base, head, Instant::now() + GIT_TIMEOUT)
}

fn changes_by(dir: &Path, base: &str, head: &str, deadline: Instant) -> Option<Vec<Change>> {
    let out = git_by(dir, &["diff", "--name-status", "-z", "-M", "--no-color", base, head], &[], deadline)?;
    Some(parse_changes(&out))
}

/// One run of changed lines of a test file's diff (what a `-U0` diff makes a hunk): the old file's line
/// it starts at, its removed and added lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hunk {
    pub line: usize,
    pub removed: Vec<String>,
    pub added: Vec<String>,
}

/// The content diff of a modified test file (or a [`guard::shared_config`] file): its runs of changed
/// lines, and the whole file before and after (a diff with all of the file as context).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestEdit {
    pub hunks: Vec<Hunk>,
    pub binary: bool,
    pub old: Vec<String>,
    pub new: Vec<String>,
}

/// A line git writes for a conflict (`<<<<<<< x`, `=======`, `>>>>>>> x`, `||||||| x`): no test's line;
/// a fix task that drops one resolves the merge (hive/integrate.rs).
fn conflict_marker(line: &str) -> bool {
    ["<<<<<<< ", "||||||| ", ">>>>>>> "].iter().any(|m| line.starts_with(m)) || line == "======="
}

fn excerpt(line: &str) -> String {
    let t = line.trim();
    match t.chars().count() > EXCERPT {
        true => format!("{}...", t.chars().take(EXCERPT).collect::<String>()),
        false => t.to_string(),
    }
}

/// Why a test edit weakens the file (`path`); None when it preserves it. Its removed lines: in each
/// hunk every one that is not blank, a comment or a conflict marker is kept by one of the hunk's added
/// lines, in order, at the same indentation (guard.rs `keeps`: the same shape, no two names made one;
/// a line moved under a new block is no longer the same test). Its added lines: they open or close no
/// block comment or triple-quoted string that the hunk's removed lines did not (guard.rs
/// `delimiters`: delimiters around unchanged lines make them a comment or a string), declare no fewer
/// tests (guard.rs `declares_test`) and add no more silencers (guard.rs `silencer`, an `if False:`
/// among them) than the removed ones; in a Python file they bind no name the old file binds in the
/// same scope to something else (guard.rs `python_bindings`: a second `def test_parse`, a `parse =
/// lambda s: s`, replace the first). A renamed key, accessor or an updated value keep a line; a
/// deleted, reshaped or tautological assertion, a test no longer declared (`#[test]` turned
/// `#[inline]`), an added skip do not, nor does any binary edit.
pub fn weakens(path: &str, e: &TestEdit) -> Option<String> {
    if e.binary {
        return Some("binary".into());
    }
    let indent = |l: &str| l.len() - l.trim_start().len();
    for h in &e.hunks {
        let mut added = h.added.iter();
        for (i, r) in h.removed.iter().enumerate() {
            if guard::comment_only(r) || conflict_marker(r) {
                continue;
            }
            if !added.any(|a| r[..indent(r)] == a[..indent(a)] && guard::keeps(r, a)) {
                return Some(format!("line {} removed or reshaped: {}", h.line + i, excerpt(r)));
            }
        }
        let nesting = |lines: &[String]| {
            lines.iter().filter(|l| !conflict_marker(l)).map(|l| guard::delimiters(l)).fold((0, 0), |(d, q), (dl, ql)| (d + dl, (q + ql) % 2))
        };
        if nesting(&h.added) != nesting(&h.removed) {
            let first = h.added.iter().chain(&h.removed).find(|l| guard::delimiters(l) != (0, 0));
            let what = first.map_or_else(String::new, |l| excerpt(l));
            return Some(format!("line {} opens or closes a comment or string around unchanged lines: {what}", h.line));
        }
    }
    let count = |lines: &mut dyn Iterator<Item = &String>, f: fn(&str) -> bool| lines.filter(|l| f(l)).count();
    let (removed, added) = (|| e.hunks.iter().flat_map(|h| &h.removed), || e.hunks.iter().flat_map(|h| &h.added));
    if count(&mut added(), guard::declares_test) < count(&mut removed(), guard::declares_test) {
        let first = removed().find(|l| guard::declares_test(l)).map_or_else(String::new, |l| excerpt(l));
        return Some(format!("drops a test: {first}"));
    }
    if count(&mut added(), guard::silencer) > count(&mut removed(), guard::silencer) {
        let first = added().find(|l| guard::silencer(l)).map_or_else(String::new, |l| excerpt(l));
        return Some(format!("adds a skip: {first}"));
    }
    if path.ends_with(".py") {
        let (old, new) = (guard::python_bindings(&e.old), guard::python_bindings(&e.new));
        let mut rebound: Vec<&(String, String)> = new.iter().filter(|(k, n)| old.get(*k).is_some_and(|o| *n > o)).map(|(k, _)| k).collect();
        rebound.sort();
        if let Some((scope, name)) = rebound.first() {
            let what = if scope.is_empty() { name.clone() } else { format!("{scope}.{name}") };
            return Some(format!("binds {what} again: the earlier one no longer runs"));
        }
    }
    None
}

/// A diff from `base` to `head`: its changed paths, and the content diff of each modified test file
/// (keyed by path; one missing could not be read).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    pub changes: Vec<Change>,
    pub edits: HashMap<String, TestEdit>,
}

/// The diff of `dir` from `base` to `head`, within `DIFF_TIMEOUT`: the changed paths, then, when a
/// test file (not a runner's config) was modified or a [`guard::shared_config`] file changed in any
/// way, one diff of those files with the whole file as context, read as literal pathspecs from the
/// toplevel (the paths are named from there, whatever subdirectory `dir` is). None when git cannot
/// list the paths; a content diff that fails leaves `edits` without them, and [`protected`] fails
/// closed.
pub fn diff(dir: &Path, base: &str, head: &str) -> Option<Diff> {
    let deadline = Instant::now() + DIFF_TIMEOUT;
    let changes = changes_by(dir, base, head, deadline)?;
    let test = |c: &Change| c.status == 'M' && !guard::runner_config(&c.path) && guard::class_of(&c.path, Path::new("/"), &[], &[]) == Some(Class::Test);
    let read: Vec<String> = changes.iter().filter(|c| test(c) || guard::shared_config(&c.path)).map(|c| format!(":(top,literal){}", c.path)).collect();
    let mut edits = HashMap::new();
    if !read.is_empty() {
        // All of the file as context: what an added line rebinds or turns into a comment is judged
        // against the lines around it. No rename: a renamed file's old path reads as deleted.
        let all = format!("-U{}", i32::MAX);
        let mut args = vec!["diff", all.as_str(), "--no-renames", "--no-color", "--no-ext-diff", "--no-textconv", "--src-prefix=a/", "--dst-prefix=b/"];
        args.extend([base, head, "--"]);
        args.extend(read.iter().map(String::as_str));
        if let Some(out) = git_by(dir, &args, &[], deadline) {
            edits = parse_test_edits(&out);
        }
    }
    Some(Diff { changes, edits })
}

/// `git diff`'s file sections (each from a `diff --git ` line): the path from `--- a/<path>` and `+++
/// b/<path>` before the first hunk (or, for a binary file, the `diff --git a/<p> b/<p>` line), then
/// each hunk's lines: context (` `, or empty) ends a run of changes, `-` and `+` lines are its removed
/// and added ones (a removed `-- x` is content, never a header), and all three make up the file before
/// and after; `\ No newline` lines are skipped. A path git quoted matches no change and so fails
/// closed.
fn parse_test_edits(out: &str) -> HashMap<String, TestEdit> {
    let mut got = HashMap::new();
    let mut put = |path: Option<String>, e: TestEdit| {
        if let Some(p) = path {
            got.insert(p, e);
        }
    };
    let (mut path, mut e, mut in_hunks, mut line): (Option<String>, TestEdit, bool, usize) = (None, TestEdit::default(), false, 0);
    let mut run: Option<Hunk> = None;
    for l in out.lines() {
        if let Some(rest) = l.strip_prefix("diff --git ") {
            e.hunks.extend(run.take());
            put(path.take(), std::mem::take(&mut e));
            in_hunks = false;
            // `a/<p> b/<p>`, the same path twice: what a binary file is known by.
            let n = rest.len().saturating_sub(5) / 2;
            path = rest.get(2..2 + n).filter(|p| rest == format!("a/{p} b/{p}")).map(String::from);
        } else if let Some(h) = l.strip_prefix("@@ -") {
            e.hunks.extend(run.take());
            in_hunks = true;
            line = h.split([',', ' ']).next().and_then(|n| n.parse().ok()).unwrap_or(0);
        } else if in_hunks {
            let at = line;
            match l.chars().next() {
                Some('-') => {
                    run.get_or_insert_with(|| Hunk { line: at, ..Hunk::default() }).removed.push(l[1..].to_string());
                    e.old.push(l[1..].to_string());
                    line += 1;
                }
                Some('+') => {
                    run.get_or_insert_with(|| Hunk { line: at, ..Hunk::default() }).added.push(l[1..].to_string());
                    e.new.push(l[1..].to_string());
                }
                Some(' ') | None => {
                    e.hunks.extend(run.take());
                    let text = l.get(1..).unwrap_or("").to_string();
                    e.old.push(text.clone());
                    e.new.push(text);
                    line += 1;
                }
                _ => {}
            }
        } else if let Some(p) = l.strip_prefix("--- a/") {
            path = Some(p.strip_suffix('\t').unwrap_or(p).to_string());
        } else if let Some(p) = l.strip_prefix("+++ ") {
            // Git ends a path holding a space with a tab here; a deleted file's is `/dev/null`.
            if let Some(p) = p.strip_prefix("b/") {
                path = Some(p.strip_suffix('\t').unwrap_or(p).to_string());
            } else if p.starts_with('"') {
                path = None;
            }
        } else if l.starts_with("Binary files ") {
            e.binary = true;
        }
    }
    e.hunks.extend(run.take());
    put(path, e);
    got
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

/// A protected change: its path and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protected {
    pub path: String,
    pub why: String,
}

/// The protected changes of a diff ("tests may grow, never shrink"): any change to a file the visible
/// check runs (`oracle_files`; the oracle itself, so even in the task's scope); outside the task's own
/// `files`: CI config (guard.rs `class_of`) modified, deleted, renamed or retyped; a test runner's
/// config (guard.rs `runner_config`) added or changed, as it changes how the existing tests run, and so
/// is a change to the runner's part of a config it shares with the project (guard.rs `shared_config`,
/// `runner_part`: an `addopts` in pyproject.toml, a `jest` key in package.json); a test deleted,
/// renamed or retyped; a test modified so that it weakens ([`weakens`]), or whose content diff could
/// not be read. Adding a test is allowed, and so is adding to one, renaming in it or updating a
/// value: outside your task's files you may add tests and change names and values in existing ones,
/// but you may not remove, skip or reshape a test or an assertion.
pub fn protected(d: &Diff, files: &[String], oracle_files: &[String]) -> Vec<Protected> {
    let globs: Vec<Regex> = files.iter().map(|f| crate::assimilate_dir::glob(f)).collect();
    let verb = |status: char| match status {
        'A' => "added",
        'D' => "deleted",
        'R' => "renamed",
        'T' => "retyped",
        _ => "modified",
    };
    let mut out = Vec::new();
    for c in &d.changes {
        let why = if oracle_files.contains(&c.path) {
            Some(format!("the visible check's file, {}", verb(c.status)))
        } else if globs.iter().any(|g| g.is_match(&c.path)) {
            None
        } else if guard::shared_config(&c.path) {
            match d.edits.get(&c.path) {
                Some(e) if e.binary => Some("binary".into()),
                Some(e) => {
                    (guard::runner_part(&c.path, &e.old) != guard::runner_part(&c.path, &e.new)).then(|| format!("test runner config {}", verb(c.status)))
                }
                None => Some("its content diff could not be read (git failed or timed out)".into()),
            }
        } else {
            match guard::class_of(&c.path, Path::new("/"), &[], &[]) {
                Some(Class::Ci) if matches!(c.status, 'M' | 'D' | 'R' | 'T') => Some(format!("CI config {}", verb(c.status))),
                Some(Class::Test) if guard::runner_config(&c.path) && matches!(c.status, 'A' | 'M' | 'D' | 'R' | 'T') => {
                    Some(format!("test runner config {}", verb(c.status)))
                }
                Some(Class::Test) if matches!(c.status, 'D' | 'R' | 'T') => Some(verb(c.status).to_string()),
                Some(Class::Test) if c.status == 'M' => match d.edits.get(&c.path) {
                    Some(e) => weakens(&c.path, e),
                    None => Some("its content diff could not be read (git failed or timed out)".into()),
                },
                _ => None,
            }
        };
        out.extend(why.map(|why| Protected { path: c.path.clone(), why }));
    }
    out
}

/// What a verdict credits: a final verdict, or (with a hidden oracle) one deferred to its score, with
/// whether the board said verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Verified,
    Rejected,
    Hacked,
    Deferred(bool),
    /// A pass on work whose diff could not be checked (no snapshot at its claim or done), or that
    /// started without a dep's verified work: the board rejects it, nothing is credited and the hidden
    /// oracle never scores it.
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
/// Why a pass cannot verify work that started without its dependencies' verified work (the ids follow).
pub const NO_DEPS: &str = "the work started from a tree without its dependencies' verified work";

/// The truth table: any `oracle` flag or protected change makes a hacked reject, whatever was said;
/// else a pass on work whose diff was not `checked` is an `Unchecked` reject (fail closed: a drone
/// that makes its snapshot fail does not pass unseen), and so is a pass on work whose claim lacks a
/// dep's verified work (`missing_deps`: it was built and checked on a tree without it); else a pass
/// verifies and a reject rejects with its note, both deferred to the hidden oracle when the goal has one.
pub fn decide(said: &Said, protected: &[Protected], oracle_flags: &[String], checked: bool, missing_deps: &[String], has_hidden: bool) -> Decision {
    if !protected.is_empty() || !oracle_flags.is_empty() {
        let mut parts = Vec::new();
        if !protected.is_empty() {
            let paths: Vec<String> = protected.iter().map(|p| format!("{} ({})", p.path, p.why)).collect();
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
        Said::Pass if !missing_deps.is_empty() => {
            Decision { ev: Ev::Rejected, hacked: false, note: format!("{NO_DEPS} ({}); claim it again", missing_deps.join(", ")), kind: Kind::Unchecked }
        }
        Said::Pass => Decision { ev: Ev::Verified, hacked: false, note: String::new(), kind: if has_hidden { Kind::Deferred(true) } else { Kind::Verified } },
        // Unchecked or without its deps, the hidden oracle has no tree to score either: the reject is final.
        Said::Reject(n) => {
            let scored = has_hidden && checked && missing_deps.is_empty();
            Decision { ev: Ev::Rejected, hacked: false, note: n.clone(), kind: if scored { Kind::Deferred(false) } else { Kind::Rejected } }
        }
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
    /// The claim's `deps_in` (board.rs `Event`): `task@head` of each dep's work its tree held.
    pub deps_in: Vec<String>,
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
    let claim = events.iter().find(|e| e.ev == Ev::Claim && e.task == work && e.by == author && e.ms == claim_ms);
    let (base, deps_in) = claim.map(|e| (e.base.clone(), e.deps_in.clone())).unwrap_or_default();
    Some(Attempt { drone: b.drones.get(&author).cloned().unwrap_or_default(), author, work, claim_ms, done_ms, base, head: done.head.clone(), deps_in })
}

/// The accepted attempt of the work task `work`: that of the verify task whose `verified` made it
/// verified (board.rs `TaskState::accepted`; a rejected `.vN` never counts, though the fold closes it
/// as verified too). It has a base and a head: `decide` rejects a pass on work without them. What a
/// dependent's claim brings in, and what an integration merges.
pub fn accepted(events: &[Event], b: &Board, work: &str) -> Option<Attempt> {
    attempt(events, b, b.tasks.get(work)?.accepted.as_deref()?)
}

/// The transitive deps of the attempt's work whose accepted head its claim's tree did not hold (not
/// in its `deps_in` as `task@head`), in `closure` order.
pub fn missing_deps(events: &[Event], b: &Board, a: &Attempt) -> Vec<String> {
    let Some(w) = b.tasks.get(&a.work) else { return Vec::new() };
    board::closure(b, &w.task).into_iter().filter(|d| accepted(events, b, d).is_none_or(|x| !a.deps_in.contains(&format!("{d}@{}", x.head)))).collect()
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
    // From a subdirectory as from the top: the visible check's files are named from there.
    let top = toplevel(Path::new(cwd), Instant::now() + GIT_TIMEOUT);
    let root = top.as_deref().map_or(Path::new(cwd), Path::new);
    // No snapshot: the diff is unchecked (a pass then rejects). Snapshots git cannot diff here (a
    // verifier outside the goal's repo): no verdict, never one that skipped the check.
    let checked = !a.base.is_empty() && !a.head.is_empty();
    let changed = match checked {
        true => diff(root, &a.base, &a.head)
            .ok_or_else(|| format!("cannot diff the work of {} from {cwd}: give the verdict from a worktree of the goal's repo", a.work))?,
        false => Diff::default(),
    };
    let prot = protected(&changed, work.map_or(&[][..], |w| &w.files), &guard::oracle_files(g, root));
    let oracle_flags: Vec<String> =
        guard::flags_of(home, &g.id, &a.author, a.claim_ms, a.done_ms).into_iter().filter(|f| f.class == Class::Oracle).map(|f| f.path).collect();
    let d = decide(said, &prot, &oracle_flags, checked, &missing_deps(&events, &b, &a), g.hidden.is_some());
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

    /// A diff of `changes` with no content diffs.
    fn only(changes: &[Change]) -> Diff {
        Diff { changes: changes.to_vec(), edits: HashMap::new() }
    }

    /// A diff modifying the test `path` by one hunk at line 1 (`removed` and `added`, one line each).
    fn edit(path: &str, removed: &str, added: &str) -> Diff {
        let lines = |t: &str| t.lines().map(String::from).collect::<Vec<_>>();
        let e = TestEdit { hunks: vec![Hunk { line: 1, removed: lines(removed), added: lines(added) }], ..TestEdit::default() };
        Diff { changes: vec![ch(path, 'M')], edits: HashMap::from([(path.to_string(), e)]) }
    }

    fn paths(p: &[Protected]) -> Vec<&str> {
        p.iter().map(|p| p.path.as_str()).collect()
    }

    #[test]
    fn the_truth_table() {
        let flags: Vec<String> = Vec::new();
        let edited = protected(&edit("tests/a.py", "assert parse('a') == 'a'", "assert True"), &[], &[]);
        // 1. A pass whose diff edited a test: hacked.
        let d = decide(&Said::Pass, &edited, &flags, true, &[], false);
        assert_eq!((d.ev, d.hacked, d.kind), (Ev::Rejected, true, Kind::Hacked));
        assert!(d.note.contains("tests/a.py (line 1 removed or reshaped: assert parse('a') == 'a')") && d.note.contains("restore"), "{}", d.note);
        // 2. A test in the task's own scope is not protected, whatever the edit.
        assert!(protected(&only(&[ch("tests/a.py", 'M')]), &["tests/*.py".into()], &[]).is_empty());
        // 3. An added test is allowed; a deleted or renamed one, or CI, is not.
        assert!(protected(&only(&[ch("tests/new.py", 'A'), ch("src/x.rs", 'M')]), &[], &[]).is_empty());
        assert_eq!(protected(&only(&[ch("tests/a.py", 'D'), ch("spec/b_spec.rb", 'R'), ch("grade.sh", 'M')]), &[], &["grade.sh".into()]).len(), 3);
        // 4. An oracle flag makes even a reject hacked.
        let d = decide(&Said::Reject("wrong output".into()), &[], &["/held/t.py".into()], true, &[], true);
        assert_eq!((d.ev, d.hacked, d.kind), (Ev::Rejected, true, Kind::Hacked));
        assert!(d.note.contains("/held/t.py") && d.note.contains("verifier: wrong output"));
        // 5. Clean: a pass verifies, a reject rejects with its note.
        assert_eq!(decide(&Said::Pass, &[], &flags, true, &[], false), Decision { ev: Ev::Verified, hacked: false, note: String::new(), kind: Kind::Verified });
        let d = decide(&Said::Reject("misses tabs".into()), &[], &flags, true, &[], false);
        assert_eq!((d.ev, d.hacked, d.note.as_str(), d.kind), (Ev::Rejected, false, "misses tabs", Kind::Rejected));
        // 6. With a hidden oracle both wait for its score; a hack does not.
        assert_eq!(decide(&Said::Pass, &[], &flags, true, &[], true).kind, Kind::Deferred(true));
        assert_eq!(decide(&Said::Reject("misses tabs".into()), &[], &flags, true, &[], true).kind, Kind::Deferred(false));
        assert_eq!(decide(&Said::Pass, &edited, &flags, true, &[], true).kind, Kind::Hacked);
        assert_eq!((Kind::Hacked.outcome(), Kind::Deferred(true).outcome()), (Some(outcome::HACKED), None));
        // 7. Unchecked (no snapshot): a pass rejects and credits nothing, a reject is final; a hack still shows.
        let d = decide(&Said::Pass, &[], &flags, false, &[], true);
        assert_eq!((d.ev, d.hacked, d.kind, d.kind.outcome()), (Ev::Rejected, false, Kind::Unchecked, None));
        assert_eq!(decide(&Said::Reject("misses tabs".into()), &[], &flags, false, &[], true).kind, Kind::Rejected);
        assert_eq!(decide(&Said::Pass, &[], &["/held/t.py".into()], false, &[], false).kind, Kind::Hacked);
        // 8. A pass on work whose claim lacked a dep's verified work: an unchecked reject naming it; a
        //    reject is final (the hidden oracle has no tree with the dep to score); a hack still shows.
        let missing = ["t1".to_string()];
        let d = decide(&Said::Pass, &[], &flags, true, &missing, true);
        assert_eq!((d.ev, d.hacked, d.kind, d.kind.outcome()), (Ev::Rejected, false, Kind::Unchecked, None));
        assert_eq!(d.note, format!("{NO_DEPS} (t1); claim it again"));
        assert_eq!(decide(&Said::Reject("misses tabs".into()), &[], &flags, true, &missing, true).kind, Kind::Rejected);
        assert_eq!(decide(&Said::Pass, &edited, &flags, true, &missing, true).kind, Kind::Hacked);
        // 9. An added test runner config changes how the existing tests run: protected, added or not.
        assert_eq!(protected(&only(&[ch("tests/conftest.py", 'A'), ch("pytest.ini", 'A'), ch("tests/test_new.py", 'A')]), &[], &[]).len(), 2);
    }

    #[test]
    fn tests_may_grow_never_shrink() {
        let none = |d: &Diff| protected(d, &["src/*.py".into()], &["grade.sh".into()]);
        // 1. A new test method and an import appended to an existing test class.
        let grown = Diff {
            changes: vec![ch("src/test/java/x/FooTest.java", 'M')],
            edits: HashMap::from([(
                "src/test/java/x/FooTest.java".to_string(),
                TestEdit {
                    hunks: vec![
                        Hunk { line: 3, removed: vec![], added: vec!["import java.util.List;".into()] },
                        Hunk {
                            line: 20,
                            removed: vec![],
                            added: ["", "    @Test", "    void parsesTwo() {", "        assertEquals(2, Foo.parse(\"2\"));", "    }"]
                                .map(String::from)
                                .to_vec(),
                        },
                    ],
                    ..TestEdit::default()
                },
            )]),
        };
        assert!(none(&grown).is_empty());
        // 2. A key literal and an accessor renamed in place, a value updated.
        let renamed = edit(
            "src/test/java/x/FooTest.java",
            "        assertEquals(\"x\", json.get(\"colorName\"));\n        assertEquals(3, r.colorName().size());",
            "        assertEquals(\"x\", json.get(\"color_name\"));\n        assertEquals(4, r.color_name().size());",
        );
        assert!(none(&renamed).is_empty());
        // 3. An assertion turned trivial: protected, the line named.
        let p = none(&edit("tests/test_parse.py", "assert parse('a') == 'a'", "assert True"));
        assert_eq!((paths(&p), p[0].why.as_str()), (vec!["tests/test_parse.py"], "line 1 removed or reshaped: assert parse('a') == 'a'"));
        assert_eq!(none(&edit("tests/A.java", "assertTrue(x.ok());", "assertTrue(true);")).len(), 1);
        assert_eq!(none(&edit("tests/A.java", "assertEquals(1, f());", "assertTrue(f() > 0);")).len(), 1);
        // 4. A deleted assertion, a deleted test method.
        assert_eq!(none(&edit("tests/test_parse.py", "    assert parse('a') == 'a'", "")).len(), 1);
        assert_eq!(none(&edit("tests/test_parse.py", "def test_a():\n    assert parse('a') == 'a'\n", "\n")).len(), 1);
        // 5. An added skip, disable, focus or early return.
        for skip in ["    @Disabled", "@pytest.mark.skip(reason='later')", "it.only('x', () => {})", "xit('x', () => {})", "\tt.Skip()", "        return;"] {
            let p = none(&edit("tests/a_test.go", "", skip));
            assert_eq!(p.len(), 1, "{skip:?}");
            assert!(p[0].why.starts_with("adds a skip: "), "{}", p[0].why);
        }
        // 5b. A tautology, a test no longer declared or collected: each keeps its line's shape.
        for (file, removed, added) in [
            ("tests/test_p.py", "    assert result == expected", "    assert result == result"),
            ("tests/FooTest.java", "        assertEquals(expected, actual);", "        assertEquals(actual, actual);"),
            ("tests/p.rs", "    #[test]", "    #[inline]"),
            ("tests/FooTest.java", "    @Test", "    @Deprecated"),
            ("tests/test_p.py", "def test_parse():", "def parse_case():"),
        ] {
            let p = none(&edit(file, removed, added));
            assert_eq!(p.len(), 1, "{added:?}");
        }
        assert_eq!(none(&edit("tests/p.rs", "    #[test]", "    #[inline]"))[0].why, "drops a test: #[test]");
        assert!(none(&edit("tests/test_p.py", "def test_parse():", "def test_parse_v2():")).is_empty(), "a renamed test is kept");
        // 5c. A block that never runs, a comment or string opened around lines the hunk leaves as they
        //     are; a glob or a comment that holds a delimiter is none.
        for (added, why) in [("    if False:", "adds a skip"), ("        if (false) {", "adds a skip"), ("/*", "line 1 opens"), ("'''", "line 1 opens")] {
            assert!(none(&edit("tests/test_p.py", "", added))[0].why.starts_with(why), "{added:?}");
        }
        for added in ["    files = glob('tests/*')", "    /* it's a note */", "    \"\"\"Two.\"\"\""] {
            assert!(none(&edit("tests/test_p.py", "", added)).is_empty(), "{added:?}");
        }
        // 6. An already disabled test's annotation reworded: no more silencers than before.
        assert!(none(&edit("tests/ATest.java", "    @Disabled(\"flaky\")", "    @Disabled(\"flaky on CI, see #12\")")).is_empty());
        // 7. A removed comment or blank line, a removed conflict marker.
        assert!(none(&edit("tests/test_parse.py", "# old note\n\n<<<<<<< ours\n=======", "")).is_empty());
        // 8. An appended line in a runner config.
        let p = none(&edit("tests/conftest.py", "", "collect_ignore = ['test_parse.py']"));
        assert_eq!((paths(&p), p[0].why.as_str()), (vec!["tests/conftest.py"], "test runner config modified"));
        // 9. The visible check's file, even in the task's scope.
        let p = protected(&only(&[ch("grade.sh", 'M')]), &["*.sh".into()], &["grade.sh".into()]);
        assert_eq!(paths(&p), ["grade.sh"]);
        // 10. A CI workflow in scope is allowed; outside it, not.
        let ci = only(&[ch(".github/workflows/ci.yml", 'M')]);
        assert!(protected(&ci, &[".github/workflows/*".into()], &[]).is_empty());
        assert_eq!(protected(&ci, &[], &[])[0].why, "CI config modified");
        // 11. A deleted or renamed test: protected outside the scope, allowed in it.
        let gone = only(&[ch("tests/test_a.py", 'D'), ch("tests/test_b.py", 'R')]);
        assert_eq!(paths(&protected(&gone, &[], &[])), ["tests/test_a.py", "tests/test_b.py"]);
        assert!(protected(&gone, &["tests/*".into()], &[]).is_empty());
        // 12. A binary test.
        let mut bin = edit("tests/fixture_test.bin", "", "");
        bin.edits.values_mut().for_each(|e| e.binary = true);
        assert_eq!(none(&bin)[0].why, "binary");
        // 13. A modified test whose content diff could not be read: fail closed.
        let p = none(&only(&[ch("tests/test_parse.py", 'M')]));
        assert!(p.len() == 1 && p[0].why.contains("could not be read"), "{p:?}");
    }

    #[test]
    fn a_zero_context_diff_reads_hunks_binaries_and_odd_lines() {
        let out = "diff --git a/tests/t.sql b/tests/t.sql\nindex 1..2 100644\n--- a/tests/t.sql\n+++ b/tests/t.sql\n@@ -3 +2,0 @@\n--- x\n\
                   @@ -9,2 +8,2 @@ def f():\n-a\n-b\n+++c\n+d\n\\ No newline at end of file\n\
                   diff --git a/tests/pic.png b/tests/pic.png\nindex 1..2 100644\nBinary files a/tests/pic.png and b/tests/pic.png differ\n\
                   diff --git \"a/tests/q\\tx.py\" \"b/tests/q\\tx.py\"\n--- \"a/tests/q\\tx.py\"\n+++ \"b/tests/q\\tx.py\"\n@@ -1 +1 @@\n-a\n+b\n";
        let got = parse_test_edits(out);
        let t = &got["tests/t.sql"];
        assert_eq!(t.hunks.len(), 2);
        assert_eq!(t.hunks[0], Hunk { line: 3, removed: vec!["-- x".into()], added: vec![] }, "a removed `-- x` is content");
        assert_eq!(t.hunks[1], Hunk { line: 9, removed: vec!["a".into(), "b".into()], added: vec!["++c".into(), "d".into()] });
        assert!(got["tests/pic.png"].binary);
        assert_eq!(got.len(), 2, "a quoted path is kept under no change's name: {:?}", got.keys());
        let quoted = Diff { changes: vec![ch("tests/q\tx.py", 'M')], edits: got };
        assert!(protected(&quoted, &[], &[])[0].why.contains("could not be read"), "fails closed");
    }

    #[test]
    fn a_nul_separated_diff_keeps_unusual_paths_as_they_are() {
        let out = "M\0tests/donn\u{e9}es.py\0R087\0tests/old \"q\".py\0src/new.py\0A\0a\tb.txt\0";
        assert_eq!(parse_changes(out), [ch("tests/donn\u{e9}es.py", 'M'), ch("tests/old \"q\".py", 'R'), ch("a\tb.txt", 'A')]);
        assert!(parse_changes("").is_empty());
        assert_eq!(protected(&only(&parse_changes(out)), &[], &[]).len(), 2, "both tests, unquoted");
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
            from: None,
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
        let want = Attempt {
            work: "t1".into(),
            author: "s1".into(),
            drone: "d1".into(),
            claim_ms: 10,
            done_ms: 20,
            base: "b".into(),
            head: "h".into(),
            deps_in: vec![],
        };
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
            from: None,
        };
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
        let p = protected(&diff(&dir, &base, &head).unwrap(), &[], &[]);
        assert_eq!((paths(&p), p[0].why.as_str()), (vec!["tests/a.py"], "line 1 removed or reshaped: assert 1"));
        let log = std::process::Command::new("git").arg("-C").arg(&dir).args(["rev-list", "--all", "--count"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "1", "no ref points at a snapshot");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_snapshot_sees_an_edit_git_finds_only_because_its_index_is_racy() {
        // git trusts an entry whose stat (to the second) matches unless the index is no newer than it
        // (racy git): a scratch index with a fresh mtime would hide this edit, and a drone's work with it.
        let dir = std::env::temp_dir().join(format!("borg-hive-snap-racy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git_ok(&dir, &["init", "-q"]);
        git_ok(&dir, &["config", "core.trustctime", "false"]);
        std::fs::write(dir.join("f.txt"), "1\n2\n3\n").unwrap();
        git_ok(&dir, &["add", "-A"]);
        git_ok(&dir, &["-c", "user.email=e@x", "-c", "user.name=e", "commit", "-qm", "init"]);
        let base = snapshot(&dir, SNAPSHOT_TIMEOUT).unwrap();
        let old = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        let set = |p: &Path| std::fs::File::options().write(true).open(p).unwrap().set_modified(old).unwrap();
        set(&dir.join("f.txt"));
        git_ok(&dir, &["update-index", "-q", "--refresh"]);
        std::fs::write(dir.join("f.txt"), "1\nb\n3\n").unwrap();
        set(&dir.join("f.txt"));
        set(&dir.join(".git/index"));
        let head = snapshot(&dir, SNAPSHOT_TIMEOUT).unwrap();
        assert_eq!(changes(&dir, &base, &head).unwrap(), [ch("f.txt", 'M')]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A repo at `name` with `a.txt` and `sub/b.txt` committed, and a dep's change (base, head) made in
    /// a second worktree of it: `a.txt` edited, `sub/new.bin` added (binary), `gone.txt` deleted.
    fn dep_fixture(name: &str) -> (std::path::PathBuf, String, String) {
        let dir = std::env::temp_dir().join(format!("borg-hive-bring-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (repo, other) = (dir.join("repo"), dir.join("other"));
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        git_ok(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "1\n2\n3\n").unwrap();
        std::fs::write(repo.join("sub/b.txt"), "b\n").unwrap();
        std::fs::write(repo.join("gone.txt"), "x\n").unwrap();
        git_ok(&repo, &["add", "-A"]);
        git_ok(&repo, &["-c", "user.email=e@x", "-c", "user.name=e", "commit", "-qm", "init"]);
        git_ok(&repo, &["worktree", "add", "-q", "--detach", other.to_str().unwrap()]);
        let base = snapshot(&other, SNAPSHOT_TIMEOUT).unwrap();
        std::fs::write(other.join("a.txt"), "1\ndep\n3\n").unwrap();
        std::fs::write(other.join("sub/new.bin"), [0u8, 159, 146, 150, 0, 1]).unwrap();
        std::fs::remove_file(other.join("gone.txt")).unwrap();
        let head = snapshot(&other, SNAPSHOT_TIMEOUT).unwrap();
        (dir, base, head)
    }

    fn porcelain(dir: &Path) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(["status", "--porcelain"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into()
    }

    fn rev(dir: &Path, what: &str) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(["rev-parse", what]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().into()
    }

    #[test]
    fn bring_in_applies_a_deps_change_to_a_dirty_tree_and_leaves_index_and_head_alone() {
        let (dir, base, head) = dep_fixture("apply");
        let repo = dir.join("repo");
        // The claimant's own unrelated change, and its shell in a subdirectory.
        std::fs::write(repo.join("sub/b.txt"), "b\nmine\n").unwrap();
        let (head_before, index_before) = (rev(&repo, "HEAD"), std::fs::read(repo.join(".git/index")).unwrap());
        assert_eq!(bring_in(&repo.join("sub"), &base, &head, SNAPSHOT_TIMEOUT), Ok(true));
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "1\ndep\n3\n", "a path outside the subdirectory too");
        assert_eq!(std::fs::read(repo.join("sub/new.bin")).unwrap(), [0u8, 159, 146, 150, 0, 1], "binary");
        assert!(!repo.join("gone.txt").exists());
        assert_eq!(std::fs::read_to_string(repo.join("sub/b.txt")).unwrap(), "b\nmine\n", "its own change kept");
        assert_eq!(porcelain(&repo), " M a.txt\n D gone.txt\n M sub/b.txt\n?? sub/new.bin\n", "nothing staged");
        assert_eq!((rev(&repo, "HEAD"), std::fs::read(repo.join(".git/index")).unwrap()), (head_before, index_before));
        assert!(bring_in(&repo, &base, &head, Duration::ZERO).is_err(), "no time left: nothing done");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bring_in_of_a_change_already_in_is_a_no_op() {
        let (dir, base, head) = dep_fixture("again");
        let repo = dir.join("repo");
        assert_eq!(bring_in(&repo, &base, &head, SNAPSHOT_TIMEOUT), Ok(true));
        let before = porcelain(&repo);
        assert_eq!(bring_in(&repo, &base, &head, SNAPSHOT_TIMEOUT), Ok(false), "its reverse applies: in already");
        assert_eq!(porcelain(&repo), before);
        // The dep's author's own tree holds it as well.
        assert_eq!(bring_in(&dir.join("other"), &base, &head, SNAPSHOT_TIMEOUT), Ok(false));
        assert_eq!(bring_in(&repo, &base, &base, SNAPSHOT_TIMEOUT), Ok(false), "an empty change");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_conflicting_tree_refuses_bring_in_and_stays_as_it_was() {
        let (dir, base, head) = dep_fixture("conflict");
        let repo = dir.join("repo");
        std::fs::write(repo.join("a.txt"), "1\nmine\n3\n").unwrap();
        let err = bring_in(&repo, &base, &head, SNAPSHOT_TIMEOUT).unwrap_err();
        assert!(err.contains("a.txt"), "git's reason names the path: {err}");
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "1\nmine\n3\n");
        assert!(repo.join("gone.txt").exists() && !repo.join("sub/new.bin").exists(), "no part of it applied");
        assert_eq!(porcelain(&repo), " M a.txt\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn take_out_restores_the_tree() {
        let (dir, base, head) = dep_fixture("out");
        let repo = dir.join("repo");
        std::fs::write(repo.join("sub/b.txt"), "b\nmine\n").unwrap();
        let before = porcelain(&repo);
        assert_eq!(bring_in(&repo, &base, &head, SNAPSHOT_TIMEOUT), Ok(true));
        assert_eq!(take_out(&repo.join("sub"), &base, &head, SNAPSHOT_TIMEOUT), Ok(()));
        assert_eq!(porcelain(&repo), before);
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "1\n2\n3\n");
        assert!(take_out(&repo, &base, &head, SNAPSHOT_TIMEOUT).is_err(), "not in: nothing to take out");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accepted_is_the_verified_attempt_not_a_rejected_one() {
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
            from: None,
        };
        let v = |n: u32| Task { id: format!("t1.v{n}"), role: Role::Verify, of: Some("t1".into()), not_by: vec!["A".into()], ..t1.clone() };
        let attempt_lines = |ms: i64, n: &str| {
            let mut c = Event::new(Ev::Claim, "t1", "A", ms);
            c.base = format!("b{n}");
            let mut d = Event::new(Ev::Done, "t1", "A", ms + 1);
            d.head = format!("h{n}");
            [c, d]
        };
        let mut lines = vec![Event::open(t1.clone(), "engineer:e", 1)];
        lines.extend(attempt_lines(10, "1"));
        lines.extend([Event::open(v(1), "A", 11), Event::new(Ev::Claim, "t1.v1", "B", 12), Event::new(Ev::Rejected, "t1.v1", "B", 13).with_note("wrong")]);
        let b = board::fold(&lines);
        assert!(accepted(&lines, &b, "t1").is_none(), "a rejected attempt is never accepted, though its verify task is closed");
        lines.extend(attempt_lines(20, "2"));
        lines.extend([Event::open(v(2), "A", 21), Event::new(Ev::Claim, "t1.v2", "B", 22), Event::new(Ev::Verified, "t1.v2", "B", 23)]);
        let b = board::fold(&lines);
        let a = accepted(&lines, &b, "t1").unwrap();
        assert_eq!((a.base.as_str(), a.head.as_str()), ("b2", "h2"));
        assert!(accepted(&lines, &b, "t9").is_none());
        // A dependent's attempt misses t1 unless its claim names t1's accepted head.
        let t2 = Task { id: "t2".into(), deps: vec!["t1".into()], ..t1.clone() };
        lines.push(Event::open(t2, "engineer:e", 30));
        let b = board::fold(&lines);
        let mut dep = Attempt { work: "t2".into(), deps_in: vec!["t1@h1".into()], ..a.clone() };
        assert_eq!(missing_deps(&lines, &b, &dep), ["t1"], "an older head of t1 is no dep");
        dep.deps_in = vec!["t1@h2".into()];
        assert!(missing_deps(&lines, &b, &dep).is_empty());
    }

    #[test]
    fn a_real_diff_lets_an_appended_test_through_and_catches_a_weakened_one() {
        let dir = std::env::temp_dir().join(format!("borg-hive-grow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        git_ok(&dir, &["init", "-q"]);
        let a = "from p import parse\n\ndef test_a():\n    assert parse('a') == 'a'\n";
        std::fs::write(dir.join("tests/test_grow.py"), a).unwrap();
        std::fs::write(dir.join("tests/test_weak [1].py"), a).unwrap();
        let base = snapshot(&dir, SNAPSHOT_TIMEOUT).unwrap();
        std::fs::write(dir.join("tests/test_grow.py"), format!("{a}\ndef test_b():\n    assert parse('b') == 'b'\n")).unwrap();
        std::fs::write(dir.join("tests/test_weak [1].py"), a.replace("assert parse('a') == 'a'", "assert True")).unwrap();
        let head = snapshot(&dir, SNAPSHOT_TIMEOUT).unwrap();
        let got = diff(&dir, &base, &head).unwrap();
        assert_eq!(got.edits.len(), 2, "both read, a glob-like name literally: {:?}", got.edits.keys());
        let p = protected(&got, &[], &[]);
        assert_eq!((paths(&p), p[0].why.as_str()), (vec!["tests/test_weak [1].py"], "line 4 removed or reshaped: assert parse('a') == 'a'"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A repo at `name` with `files` committed and snapshotted: (dir, base).
    fn committed(name: &str, files: &[(&str, &str)]) -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("borg-hive-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git_ok(&dir, &["init", "-q"]);
        for (p, body) in files {
            let p = dir.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        git_ok(&dir, &["add", "-A"]);
        git_ok(&dir, &["-c", "user.email=e@x", "-c", "user.name=e", "commit", "-qm", "init"]);
        let base = snapshot(&dir, SNAPSHOT_TIMEOUT).unwrap();
        (dir, base)
    }

    /// The protected changes (outside `src/*`) of `path` written as `body` on top of `base`; the file is
    /// restored afterwards.
    fn judged(dir: &Path, base: &str, path: &str, body: &str) -> Vec<Protected> {
        let p = dir.join(path);
        let old = std::fs::read_to_string(&p).ok();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        let head = snapshot(dir, SNAPSHOT_TIMEOUT).unwrap();
        let got = protected(&diff(dir, base, &head).unwrap(), &["src/*".into()], &[]);
        match old {
            Some(o) => std::fs::write(&p, o).unwrap(),
            None => std::fs::remove_file(&p).unwrap(),
        }
        got
    }

    #[test]
    fn added_lines_that_switch_off_existing_tests_shrink_them() {
        let py = "from p import parse\n\n\ndef test_parse():\n    assert parse('a') == 'a'\n\n\ndef test_other():\n    assert parse('b') == 'b'\n";
        let java = "class FooTest {\n    @Test\n    void parses() {\n        assertEquals(1, Foo.parse(\"1\"));\n    }\n\n    @Test\n    void parsesTwo() {\n        assertEquals(2, Foo.parse(\"2\"));\n    }\n}\n";
        let (dir, base) = committed("switch-off", &[("tests/test_p.py", py), ("tests/FooTest.java", java)]);
        let one = |path: &str, body: String, why: &str| {
            let p = judged(&dir, &base, path, &body);
            assert_eq!(paths(&p), [path], "{body}");
            assert!(p[0].why.starts_with(why), "{body}: {}", p[0].why);
        };
        // A test of the same name appended shadows the real one; the function under test rebound.
        one("tests/test_p.py", format!("{py}\n\ndef test_parse():\n    pass\n"), "binds test_parse again");
        one("tests/test_p.py", format!("{py}\nparse = lambda s: s\n"), "binds parse again");
        one("tests/test_p.py", format!("{py}\nfrom q import parse\n"), "binds parse again");
        // An assertion put under `if False:`, or re-indented under any new block.
        one("tests/test_p.py", py.replace("    assert parse('a')", "    if False:\n        assert parse('a')"), "line 5 removed or reshaped");
        one("tests/test_p.py", py.replace("    assert parse('a')", "    with open('x'):\n        assert parse('a')"), "line 5 removed or reshaped");
        // A test turned into a string or a comment by delimiters added around unchanged lines.
        let around = |body: &str, open: &str, close: &str, first: &str, after: &str| {
            body.replacen(first, &format!("{open}\n{first}"), 1).replacen(after, &format!("{close}\n{after}"), 1)
        };
        let opens = "line 4 opens or closes a comment or string";
        one("tests/test_p.py", around(py, "'''", "'''", "def test_parse", "\n\ndef test_other"), opens);
        one("tests/test_p.py", around(py, "\"\"\"", "\"\"\"", "def test_parse", "\n\ndef test_other"), opens);
        one(
            "tests/FooTest.java",
            around(java, "    /*", "    */", "    @Test\n    void parses()", "\n    @Test\n    void parsesTwo"),
            "line 2 opens or closes",
        );
        one(
            "tests/FooTest.java",
            java.replace("        assertEquals(1", "        if (false) {\n        assertEquals(1").replace("\"1\"));\n", "\"1\"));\n        }\n"),
            "adds a skip",
        );
        // Growth stays allowed: a new test with docstrings and a block comment, a helper, a test of an
        // existing name in a class of its own, a new import.
        let grown = format!(
            "import re\n{py}\n\ndef helper(s):\n    \"\"\"One line.\"\"\"\n    return s\n\n\ndef test_more():\n    \"\"\"\n    Two lines.\n    \"\"\"\n    assert helper('c') == 'c'\n\n\nclass TestAgain:\n    def test_parse(self):\n        assert parse('c') == 'c'\n"
        );
        assert!(judged(&dir, &base, "tests/test_p.py", &grown).is_empty());
        let java_grown = java.strip_suffix("}\n").unwrap().to_string()
            + "\n    /*\n     * Why: three.\n     */\n    @Test\n    void parsesThree() { /* inline */\n        assertEquals(3, Foo.parse(\"3\"));\n    }\n}\n";
        assert!(judged(&dir, &base, "tests/FooTest.java", &java_grown).is_empty(), "{java_grown}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_test_runners_section_in_a_shared_config_counts_as_its_config() {
        let pyproject = "[project]\nname = \"p\"\nversion = \"1\"\n";
        let cfg = "[metadata]\nname = p\n";
        let pkg = "{\n  \"name\": \"p\",\n  \"scripts\": {\n    \"test\": \"jest\",\n    \"build\": \"tsc\"\n  }\n}\n";
        let (dir, base) = committed("runner-section", &[("pyproject.toml", pyproject), ("setup.cfg", cfg), ("package.json", pkg)]);
        let one = |path: &str, body: String| paths(&judged(&dir, &base, path, &body)).iter().map(|p| p.to_string()).collect::<Vec<_>>();
        assert_eq!(one("pyproject.toml", format!("{pyproject}\n[tool.pytest.ini_options]\naddopts = \"-k 'not test_parse'\"\n")), ["pyproject.toml"]);
        assert_eq!(one("setup.cfg", format!("{cfg}\n[tool:pytest]\naddopts = -k 'not test_parse'\n")), ["setup.cfg"]);
        assert_eq!(
            one("package.json", pkg.replace("\"name\": \"p\",", "\"name\": \"p\",\n  \"jest\": {\"testPathIgnorePatterns\": [\"parse\"]},")),
            ["package.json"]
        );
        assert_eq!(one("package.json", pkg.replace("\"jest\"", "\"jest -t nothing\"")), ["package.json"]);
        assert_eq!(one("sub/pyproject.toml", "[tool.pytest.ini_options]\ntestpaths = [\"none\"]\n".into()), ["sub/pyproject.toml"], "an added one");
        // Their other sections are no test's.
        assert!(one("pyproject.toml", pyproject.replace("version = \"1\"", "version = \"2\"\ndependencies = [\"x\"]")).is_empty());
        assert!(one("setup.cfg", format!("{cfg}version = 2\n")).is_empty());
        assert!(one("package.json", pkg.replace("\"tsc\"", "\"tsc -b\"")).is_empty());
        assert!(one("sub/package.json", "{\"name\": \"q\"}\n".into()).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_users_diff_config_changes_no_patch_path_or_line() {
        let (dir, base, head) = dep_fixture("config");
        let repo = dir.join("repo");
        for kv in ["diff.noprefix=true", "diff.relative=true", "diff.mnemonicPrefix=true", "diff.suppressBlankEmpty=true", "core.quotePath=true"] {
            let (k, v) = kv.split_once('=').unwrap();
            git_ok(&repo, &["config", k, v]);
        }
        assert_eq!(bring_in(&repo.join("sub"), &base, &head, SNAPSHOT_TIMEOUT), Ok(true));
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "1\ndep\n3\n");
        let mut got = changes(&repo.join("sub"), &base, &head).unwrap();
        got.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(got, [ch("a.txt", 'M'), ch("gone.txt", 'D'), ch("sub/new.bin", 'A')], "paths outside the subdirectory too");
        assert_eq!(take_out(&repo.join("sub"), &base, &head, SNAPSHOT_TIMEOUT), Ok(()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_snapshot_cut_off_leaves_no_scratch_index_or_lock() {
        let dir = std::env::temp_dir().join(format!("borg-hive-snap-cut-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git_ok(&dir, &["init", "-q"]);
        std::fs::write(dir.join("f.txt"), "1\n").unwrap();
        git_ok(&dir, &["add", "-A"]);
        git_ok(&dir, &["-c", "user.email=e@x", "-c", "user.name=e", "commit", "-qm", "init"]);
        // A clean filter that outlasts the snapshot's time: `add -A` is killed holding its index lock.
        git_ok(&dir, &["config", "filter.slow.clean", "sleep 5; cat"]);
        std::fs::write(dir.join(".gitattributes"), "f.txt filter=slow\n").unwrap();
        std::fs::write(dir.join("f.txt"), "2\n").unwrap();
        let t0 = Instant::now();
        assert_eq!(snapshot(&dir, Duration::from_secs(1)), None);
        assert!(t0.elapsed() < Duration::from_secs(4));
        let left: Vec<String> = std::fs::read_dir(dir.join(".git"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("borg") || n.ends_with(".lock"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
