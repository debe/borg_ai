//! Commits travel with the board (docs/design/hive.md C4, "Commits travel"): the node that did a
//! task's work publishes the attempt's two snapshots (its claim's `base`, its done's `head`) as one git
//! bundle in `bundles.jsonl`, a synced log of the goal (hive/mod.rs `SYNCED`), so a node on another
//! machine verifies, builds on and integrates the work as the author's node does.
//!
//! - **Made at the done** ([`publish`]), only in a store that is a mesh member (`mesh/membership.json`,
//!   the mark setup reads too): both snapshots pinned at `refs/borg/hive/<goal>/<sha>` (which also keeps
//!   them from `gc` there), bundled `--not --remotes` (what a remote-tracking ref holds, every member's
//!   clone is taken to hold; the receiver checks), at most [`MAX_BUNDLE`] bytes. A bigger attempt carries
//!   nothing: its work stays where it was done, as before.
//! - **Taken when needed** ([`fetch`]): a verify offer, a dep's bring-in or an integration that finds an
//!   attempt's commits absent unbundles a line naming exactly that attempt (task, base, head). The line
//!   is refused unless the bundle's heads are exactly the attempt's commits, the repo holds its
//!   prerequisites, and every object the two commits reach is there afterwards; then both are pinned at
//!   the same refs. Commits are named by their content: what arrives is the commit the board names,
//!   whoever sent the line and in whatever order lines came, and a second fetch finds it present.
//! - Each outcome lands once per line and reason in `unbundled.jsonl` (this node's, never synced), which
//!   `borg top` and the Hive tab show. The bundle is unbundled into a quarantine object directory and its
//!   objects reach the repo's store only once that walk passed, so a refused line leaves nothing behind.
//!   A forged line is refused once and never tried again; a missing prerequisite or a local failure (an
//!   I/O error, a git call that did not finish) is tried again (a `git fetch` of the remote may bring it).
//!
//! The mesh refuses a line of `bundles.jsonl` longer than [`MAX_LINE`] (borg-mesh lines.rs `cap`), so an
//! oversized bundle is neither stored nor passed on.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use super::TaskId;
use super::verdict::{self, Attempt};
use crate::{learn, runner};

/// The goal's synced log of bundles (hive/mod.rs `SYNCED`).
pub const FILE: &str = "bundles.jsonl";
/// This node's record of what it unbundled or refused.
const UNBUNDLED: &str = "unbundled.jsonl";
/// The largest bundle an attempt carries (raw bytes): a code change is far smaller; a snapshot that
/// took a build's output or a data file along stays on its node.
pub const MAX_BUNDLE: usize = 1 << 20;
/// The longest line of `bundles.jsonl` a node stores or sends: a [`MAX_BUNDLE`] bundle in base64 and
/// its fields.
pub const MAX_LINE: usize = MAX_BUNDLE.div_ceil(3) * 4 + 1024;
/// Each git call of a bundle.
const GIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Bundles a view lists (the newest).
const ROWS: usize = 20;

/// One line of bundles.jsonl: the commits of one attempt, from the node that did it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bundle {
    pub v: u8,
    pub task: TaskId,
    /// The node the work was done on (hive/mod.rs `node`).
    pub node: String,
    pub ms: i64,
    pub base: String,
    pub head: String,
    /// The bundle file, base64.
    pub bundle: String,
}

/// One outcome of unbundling a line on this node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Unbundled {
    ms: i64,
    /// blake3 of the bundles.jsonl line, hex.
    line: String,
    task: TaskId,
    head: String,
    ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    why: String,
    /// The refusal may pass (a prerequisite the repo lacks, a local failure): the line is tried again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    retry: bool,
}

pub fn path(home: &Path, goal: &str) -> PathBuf {
    super::dir(home, goal).join(FILE)
}

fn unbundled_path(home: &Path, goal: &str) -> PathBuf {
    super::dir(home, goal).join(UNBUNDLED)
}

/// Whether the store is a member of a mesh: only then does anything leave this node to need a bundle.
pub fn meshed(home: &Path) -> bool {
    home.join("mesh").join("membership.json").is_file()
}

/// Where a snapshot of goal `goal` is pinned, on the author's node and on every node that took it.
pub fn reference(goal: &str, sha: &str) -> String {
    format!("refs/borg/hive/{goal}/{sha}")
}

/// Pins commit `sha` of the repo at `repo` at `reference(goal, sha)`, which keeps it from `gc`: whether
/// it did. A lost claim's snapshot (hive/mod.rs `set_aside`) is pinned so, never bundled.
pub fn pin(repo: &Path, goal: &str, sha: &str) -> bool {
    git_ok(repo, &["update-ref", &reference(goal, sha), sha])
}

/// A full commit id: 40 (sha1) or 64 (sha256) lowercase hex.
fn sha(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The attempt's commits, each once: its base and its head.
fn commits(a: &Attempt) -> BTreeSet<String> {
    [a.base.clone(), a.head.clone()].into_iter().collect()
}

/// Every well-formed bundle line of goal `goal` with the hash of its line: longer lines than
/// [`MAX_LINE`], other formats and lines whose commits are not full ids are skipped.
pub fn read(home: &Path, goal: &str) -> Vec<(String, Bundle)> {
    let text = std::fs::read_to_string(path(home, goal)).unwrap_or_default();
    text.lines()
        .filter(|l| l.len() <= MAX_LINE)
        .filter_map(|l| serde_json::from_str::<Bundle>(l).ok().map(|b| (blake3::hash(l.as_bytes()).to_hex().to_string(), b)))
        .filter(|(_, b)| b.v <= 1 && sha(&b.base) && sha(&b.head))
        .collect()
}

/// `git -C repo args` (git.rs) within `GIT_TIMEOUT`: (success, stdout). None when it did not finish.
fn git(repo: &Path, args: &[&str]) -> Option<(bool, String)> {
    git_in(repo, None, args)
}

/// [`git`], writing new objects to `quarantine` (an object directory of its own, the repo's store its
/// alternate) when one is given.
fn git_in(repo: &Path, quarantine: Option<&Quarantine>, args: &[&str]) -> Option<(bool, String)> {
    let mut cmd = super::git::command(repo);
    cmd.args(args);
    if let Some(q) = quarantine {
        cmd.env("GIT_OBJECT_DIRECTORY", &q.dir).env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &q.objects);
    }
    runner::run_capped_status(cmd, None, GIT_TIMEOUT).map(|(s, out)| (s.success(), String::from_utf8_lossy(&out).into_owned()))
}

fn git_ok(repo: &Path, args: &[&str]) -> bool {
    git(repo, args).is_some_and(|(ok, _)| ok)
}

/// A fresh file name in the goal's scratch directory (never synced).
fn scratch(home: &Path, goal: &str, tag: &str) -> Result<PathBuf, String> {
    let d = super::dir(home, goal).join("scratch");
    std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    Ok(d.join(format!("{tag}-{}-{nanos}.bundle", std::process::id())))
}

/// Publishes the commits of attempt `a` (made in `repo`) as a bundle line of goal `goal`, once: the
/// bundle's size, or None when nothing was due (no mesh, no snapshots, or a line for it already).
///
/// # Errors
/// A git call failed, or the bundle is bigger than [`MAX_BUNDLE`]: no line is written.
pub fn publish(home: &Path, goal: &str, a: &Attempt, repo: &Path) -> Result<Option<usize>, String> {
    if !meshed(home) || !sha(&a.base) || !sha(&a.head) {
        return Ok(None);
    }
    if read(home, goal).iter().any(|(_, b)| b.task == a.work && b.base == a.base && b.head == a.head) {
        return Ok(None);
    }
    let refs: Vec<String> = commits(a).iter().map(|c| reference(goal, c)).collect();
    for (r, c) in refs.iter().zip(commits(a)) {
        if !git_ok(repo, &["update-ref", r, &c]) {
            return Err(format!("cannot pin {c} at {r}"));
        }
    }
    let file = scratch(home, goal, &a.work)?;
    let f = file.display().to_string();
    let args: Vec<&str> = ["bundle", "create", "-q", f.as_str()].into_iter().chain(refs.iter().map(String::as_str)).chain(["--not", "--remotes"]).collect();
    let made = git_ok(repo, &args);
    let bytes = std::fs::read(&file);
    let _ = std::fs::remove_file(&file);
    let bytes = match (made, bytes) {
        (true, Ok(b)) => b,
        _ => return Err(format!("git bundle create failed for {}", a.work)),
    };
    if bytes.len() > MAX_BUNDLE {
        return Err(format!("{}'s commits are a {} KiB bundle, over {} KiB: they stay on this node", a.work, bytes.len() / 1024, MAX_BUNDLE / 1024));
    }
    let line = Bundle {
        v: 1,
        task: a.work.clone(),
        node: super::node(home),
        ms: learn::now_ms(),
        base: a.base.clone(),
        head: a.head.clone(),
        bundle: base64::engine::general_purpose::STANDARD.encode(&bytes),
    };
    super::append_line(&path(home, goal), &line).map_err(|e| e.to_string())?;
    Ok(Some(bytes.len()))
}

/// Brings the commits of attempt `a` into `repo` from a bundle line naming exactly it, unless they are
/// there already: whether they are there now.
pub fn fetch(home: &Path, goal: &str, a: &Attempt, repo: &Path) -> bool {
    let need: Vec<String> = commits(a).into_iter().collect();
    let refs: Vec<&str> = need.iter().map(String::as_str).collect();
    if verdict::present(repo, &refs, verdict::DIFF_TIMEOUT) {
        return true;
    }
    if !sha(&a.base) || !sha(&a.head) {
        return false;
    }
    let seen: Vec<Unbundled> = super::read_lines(&unbundled_path(home, goal));
    for (h, b) in read(home, goal).into_iter().filter(|(_, b)| b.task == a.work && b.base == a.base && b.head == a.head) {
        if seen.iter().any(|u| u.line == h && !u.ok && !u.retry) {
            continue;
        }
        let (ok, why, retry) = match unbundle(home, goal, repo, &b, &need) {
            Ok(()) => (true, String::new(), false),
            Err((why, retry)) => (false, why, retry),
        };
        if !seen.iter().any(|u| u.line == h && u.ok == ok && u.why == why) {
            let u = Unbundled { ms: learn::now_ms(), line: h, task: b.task.clone(), head: b.head.clone(), ok, why, retry };
            let _ = super::append_line(&unbundled_path(home, goal), &u);
        }
        if ok {
            return true;
        }
    }
    false
}

/// Unbundles `b` into `repo` when it carries exactly the commits `need` and they arrive whole; then
/// pins them. Why not otherwise, and whether that may pass (a local failure: an I/O error, a git call
/// that did not finish; only what the bundle holds is refused for good).
fn unbundle(home: &Path, goal: &str, repo: &Path, b: &Bundle, need: &[String]) -> Result<(), (String, bool)> {
    let forged = |why: String| (why, false);
    let bytes = base64::engine::general_purpose::STANDARD.decode(&b.bundle).map_err(|_| forged("the bundle is not base64".into()))?;
    if bytes.len() > MAX_BUNDLE {
        return Err(forged(format!("a {} KiB bundle, over {} KiB", bytes.len() / 1024, MAX_BUNDLE / 1024)));
    }
    let file = scratch(home, goal, &b.task).map_err(|e| (e, true))?;
    std::fs::write(&file, &bytes).map_err(|e| (e.to_string(), true))?;
    let out = take(repo, &file, goal, need);
    let _ = std::fs::remove_file(&file);
    out
}

/// An object directory a bundle is unbundled into before anything of it reaches the repo's store (as
/// `git receive-pack` quarantines a push): a refused bundle leaves no object behind, so a commit object
/// without its tree never makes the commit look present ([`verdict::present`]) to a later fetch.
struct Quarantine {
    /// The repo's object store.
    objects: PathBuf,
    /// `objects/incoming-borg-…`, on the store's file system so its files move by rename.
    dir: PathBuf,
}

impl Quarantine {
    fn new(repo: &Path) -> Result<Self, String> {
        let objects = match git(repo, &["rev-parse", "--path-format=absolute", "--git-path", "objects"]) {
            Some((true, out)) if !out.trim().is_empty() => PathBuf::from(out.trim()),
            _ => return Err("cannot find the repo's object store".into()),
        };
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let dir = objects.join(format!("incoming-borg-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(dir.join("pack")).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Self { objects, dir })
    }

    /// Moves every object into the repo's store: each pack's index last, so git never sees an index
    /// without its pack.
    fn migrate(&self) -> Result<(), String> {
        fn files(d: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
            for e in std::fs::read_dir(d)? {
                let p = e?.path();
                if p.is_dir() { files(&p, out)? } else { out.push(p) }
            }
            Ok(())
        }
        let mut all = Vec::new();
        files(&self.dir, &mut all).map_err(|e| e.to_string())?;
        all.sort_by_key(|p| p.extension().is_some_and(|x| x == "idx"));
        for p in all {
            let to = self.objects.join(p.strip_prefix(&self.dir).map_err(|e| e.to_string())?);
            if let Some(d) = to.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            if !to.exists() {
                std::fs::rename(&p, &to).map_err(|e| format!("{}: {e}", to.display()))?;
            }
        }
        Ok(())
    }
}

impl Drop for Quarantine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn take(repo: &Path, file: &Path, goal: &str, need: &[String]) -> Result<(), (String, bool)> {
    let forged = |why: String| (why, false);
    let local = |why: String| (why, true);
    let f = file.display().to_string();
    let heads = match git(repo, &["bundle", "list-heads", &f]) {
        Some((true, out)) => out.lines().filter_map(|l| l.split_whitespace().next().map(String::from)).collect::<BTreeSet<String>>(),
        Some((false, _)) => return Err(forged("not a git bundle".into())),
        None => return Err(local("git bundle list-heads did not finish".into())),
    };
    let want: BTreeSet<String> = need.iter().cloned().collect();
    if heads != want {
        let short = |s: &BTreeSet<String>| s.iter().map(|c| c.chars().take(12).collect::<String>()).collect::<Vec<_>>().join(", ");
        return Err(forged(format!("its commits are {}, the board names {}", short(&heads), short(&want))));
    }
    if !git_ok(repo, &["bundle", "verify", "-q", &f]) {
        return Err(local("the repo lacks the bundle's prerequisite commits: `git fetch` its remote".into()));
    }
    let q = Quarantine::new(repo).map_err(local)?;
    match git_in(repo, Some(&q), &["bundle", "unbundle", &f]) {
        Some((true, _)) => {}
        Some((false, _)) => return Err(local("git bundle unbundle failed".into())),
        None => return Err(local(format!("git bundle unbundle did not finish within {}s", GIT_TIMEOUT.as_secs()))),
    }
    // Every object the two commits reach is here (the bundle carried what the repo lacked), before any
    // of it reaches the repo's store or a ref names it.
    let walk: Vec<&str> = ["rev-list", "--objects", "--quiet"].into_iter().chain(need.iter().map(String::as_str)).chain(["--not", "--all"]).collect();
    match git_in(repo, Some(&q), &walk) {
        Some((true, _)) => {}
        Some((false, _)) => return Err(forged("the bundle does not hold every object of its commits".into())),
        None => return Err(local("git rev-list did not finish".into())),
    }
    q.migrate().map_err(local)?;
    for c in need {
        if !git_ok(repo, &["update-ref", &reference(goal, c), c]) {
            return Err(local(format!("cannot pin {c}")));
        }
    }
    Ok(())
}

/// A bundle as `borg top` and the Hive tab show it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Row {
    pub task: TaskId,
    /// The head's first 12 characters.
    pub head: String,
    /// The node it came from: empty for this one, else its first 8 characters.
    pub node: String,
    pub bytes: usize,
    /// `made here`, `carried` (not needed here yet), `unbundled`, `refused: …` or `waiting: …`.
    pub state: String,
}

/// The newest bundles of goal `goal` and what this node did with each.
pub fn rows(home: &Path, goal: &str) -> Vec<Row> {
    let mut last: HashMap<String, Unbundled> = HashMap::new();
    for u in super::read_lines::<Unbundled>(&unbundled_path(home, goal)) {
        last.insert(u.line.clone(), u);
    }
    let all = read(home, goal);
    all[all.len().saturating_sub(ROWS)..]
        .iter()
        .map(|(h, b)| {
            let local = super::is_local(home, &b.node);
            let state = match last.get(h) {
                _ if local => "made here".to_string(),
                Some(u) if u.ok => "unbundled".into(),
                Some(u) if u.retry => format!("waiting: {}", u.why),
                Some(u) => format!("refused: {}", u.why),
                None => "carried".into(),
            };
            Row {
                task: b.task.clone(),
                head: b.head.chars().take(12).collect(),
                node: if local { String::new() } else { b.node.chars().take(8).collect() },
                bytes: b.bundle.len() / 4 * 3,
                state,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.email=e@x", "-c", "user.name=e"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Two machines: an origin, a clone for each node with its own object store, and a store for each
    /// (B a mesh member).
    fn machines(tag: &str) -> (PathBuf, [(PathBuf, PathBuf); 2]) {
        let root = std::env::temp_dir().join(format!("borg-hive-commits-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let origin = root.join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        sh(&origin, &["init", "-q", "-b", "main"]);
        std::fs::write(origin.join("README"), "app\n").unwrap();
        sh(&origin, &["add", "-A"]);
        sh(&origin, &["commit", "-qm", "base"]);
        let mk = |n: &str| {
            let (repo, home) = (root.join(n).join("app"), root.join(n).join("store"));
            sh(&root, &["clone", "-q", "--no-local", origin.to_str().unwrap(), repo.to_str().unwrap()]);
            std::fs::create_dir_all(home.join("mesh")).unwrap();
            std::fs::write(home.join("mesh/membership.json"), "{}").unwrap();
            std::fs::create_dir_all(home.join("stats")).unwrap();
            std::fs::write(home.join("stats/node"), n).unwrap();
            (repo, home)
        };
        let nodes = [mk("a"), mk("b")];
        (root, nodes)
    }

    /// B's attempt at t1: snapshots at its claim and its done.
    fn work(repo: &Path, file: &str) -> Attempt {
        let base = verdict::snapshot(repo, verdict::SNAPSHOT_TIMEOUT).unwrap();
        std::fs::write(repo.join(file), "one\n").unwrap();
        let head = verdict::snapshot(repo, verdict::SNAPSHOT_TIMEOUT).unwrap();
        Attempt { work: "t1".into(), author: "sb".into(), drone: String::new(), claim_ms: 1, done_ms: 2, base, head, deps_in: Vec::new(), node: "b".into() }
    }

    /// The line of bundles.jsonl a peer would hand over: B's lines, copied to A's store.
    fn carry(from: &Path, to: &Path) {
        let p = path(to, "g");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::copy(path(from, "g"), p).unwrap();
    }

    #[test]
    fn an_attempt_travels_to_a_separate_object_store_once() {
        let (root, [(ra, ha), (rb, hb)]) = machines("travel");
        let a = work(&rb, "f1.txt");
        assert!(!verdict::present(&ra, &[&a.head], verdict::DIFF_TIMEOUT), "separate object stores");
        let size = publish(&hb, "g", &a, &rb).unwrap().unwrap();
        assert!(size > 0 && size < 4096, "only the snapshots, not the remote's history: {size}");
        assert_eq!(publish(&hb, "g", &a, &rb).unwrap(), None, "once per attempt");
        assert_eq!(sh(&rb, &["rev-parse", &reference("g", &a.head)]), a.head, "pinned on the author's node too");
        assert!(!fetch(&ha, "g", &a, &ra), "nothing carried yet");
        carry(&hb, &ha);
        assert!(fetch(&ha, "g", &a, &ra));
        assert!(verdict::present(&ra, &[&a.base, &a.head], verdict::DIFF_TIMEOUT));
        assert_eq!(sh(&ra, &["rev-parse", &reference("g", &a.head)]), a.head);
        assert_eq!(verdict::changes(&ra, &a.base, &a.head).unwrap().iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["f1.txt"]);
        assert!(fetch(&ha, "g", &a, &ra), "idempotent");
        assert_eq!(rows(&ha, "g").iter().map(|r| r.state.as_str()).collect::<Vec<_>>(), ["unbundled"]);
        assert_eq!(rows(&hb, "g")[0].state, "made here");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_store_outside_every_mesh_bundles_nothing() {
        let (root, [_, (rb, hb)]) = machines("local");
        std::fs::remove_file(hb.join("mesh/membership.json")).unwrap();
        let a = work(&rb, "f1.txt");
        assert_eq!(publish(&hb, "g", &a, &rb).unwrap(), None);
        assert!(!path(&hb, "g").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_forged_bundle_or_one_without_its_prerequisites_is_refused() {
        let (root, [(ra, ha), (rb, hb)]) = machines("forged");
        let a = work(&rb, "f1.txt");
        // Another commit of B's, bundled under the name of the attempt's head.
        let other = work(&rb, "f2.txt");
        sh(&rb, &["update-ref", &reference("g", &a.head), &other.head]);
        sh(&rb, &["update-ref", &reference("g", &a.base), &other.base]);
        let f = root.join("x.bundle");
        sh(&rb, &["bundle", "create", "-q", f.to_str().unwrap(), &reference("g", &a.base), &reference("g", &a.head), "--not", "--remotes"]);
        let forged = Bundle {
            v: 1,
            task: "t1".into(),
            node: "b".into(),
            ms: 3,
            base: a.base.clone(),
            head: a.head.clone(),
            bundle: base64::engine::general_purpose::STANDARD.encode(std::fs::read(&f).unwrap()),
        };
        super::super::append_line(&path(&ha, "g"), &forged).unwrap();
        assert!(!fetch(&ha, "g", &a, &ra));
        for c in [&a.head, &other.head] {
            assert!(!verdict::present(&ra, &[c], verdict::DIFF_TIMEOUT), "nothing of a forged bundle is named in A's repo");
        }
        let r = rows(&ha, "g");
        assert!(r[0].state.starts_with("refused: its commits are"), "{r:?}");
        assert!(!fetch(&ha, "g", &a, &ra));
        assert_eq!(super::super::read_lines::<Unbundled>(&unbundled_path(&ha, "g")).len(), 1, "refused once, never tried again");
        // Garbage, a bundle over the cap and a line over MAX_LINE, under the right names.
        let junk = |bytes: &[u8]| Bundle { bundle: base64::engine::general_purpose::STANDARD.encode(bytes), ..forged.clone() };
        for b in [Bundle { bundle: "!!".into(), ..forged.clone() }, junk(&vec![0u8; MAX_BUNDLE + 1]), junk(&vec![0u8; 2 * MAX_BUNDLE])] {
            super::super::append_line(&path(&ha, "g"), &b).unwrap();
        }
        assert_eq!(read(&ha, "g").len(), 3, "a line over MAX_LINE does not count");
        assert!(!fetch(&ha, "g", &a, &ra));
        let states: Vec<String> = rows(&ha, "g").into_iter().map(|r| r.state).collect();
        assert_eq!(states[1], "refused: the bundle is not base64");
        assert!(states[2].starts_with("refused: a 1024 KiB bundle, over 1024 KiB"), "{states:?}");
        // A genuine bundle needs the commits it was cut from: a repo without the remote's history waits.
        let (lone, hl) = (root.join("lone"), root.join("lone-store"));
        std::fs::create_dir_all(&lone).unwrap();
        sh(&lone, &["init", "-q"]);
        publish(&hb, "g2", &Attempt { work: "t9".into(), ..a.clone() }, &rb).unwrap();
        std::fs::create_dir_all(super::super::dir(&hl, "g2")).unwrap();
        std::fs::copy(path(&hb, "g2"), path(&hl, "g2")).unwrap();
        let t9 = Attempt { work: "t9".into(), ..a.clone() };
        assert!(!fetch(&hl, "g2", &t9, &lone));
        assert!(rows(&hl, "g2")[0].state.starts_with("waiting: the repo lacks"), "{:?}", rows(&hl, "g2"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A bundle whose heads and prerequisites are right but whose pack holds only the two commit objects
    /// (no tree, no blob): refused, and nothing of it stays in A's store, so B's genuine line is taken
    /// after it.
    #[test]
    fn a_refused_incomplete_bundle_leaves_nothing_behind() {
        let (root, [(ra, ha), (rb, hb)]) = machines("incomplete");
        let a = work(&rb, "f1.txt");
        let parent = sh(&rb, &["rev-parse", &format!("{}^", a.base)]);
        let pack = {
            let mut c = std::process::Command::new("git");
            c.arg("-C").arg(&rb).args(["pack-objects", "--stdout"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped());
            let mut child = c.spawn().unwrap();
            std::io::Write::write_all(&mut child.stdin.take().unwrap(), format!("{}\n{}\n", a.base, a.head).as_bytes()).unwrap();
            child.wait_with_output().unwrap().stdout
        };
        let mut bytes =
            format!("# v2 git bundle\n-{parent} base\n{} {}\n{} {}\n\n", a.base, reference("g", &a.base), a.head, reference("g", &a.head)).into_bytes();
        bytes.extend(pack);
        let thin = Bundle {
            v: 1,
            task: "t1".into(),
            node: "c".into(),
            ms: 1,
            base: a.base.clone(),
            head: a.head.clone(),
            bundle: base64::engine::general_purpose::STANDARD.encode(&bytes),
        };
        super::super::append_line(&path(&ha, "g"), &thin).unwrap();
        assert!(!fetch(&ha, "g", &a, &ra));
        assert!(rows(&ha, "g")[0].state.starts_with("refused: the bundle does not hold every object"), "{:?}", rows(&ha, "g"));
        assert!(!verdict::present(&ra, &[&a.head], verdict::DIFF_TIMEOUT), "no commit object of a refused bundle stays");
        publish(&hb, "g", &a, &rb).unwrap().unwrap();
        let genuine = std::fs::read_to_string(path(&hb, "g")).unwrap();
        std::fs::OpenOptions::new().append(true).open(path(&ha, "g")).and_then(|mut f| std::io::Write::write_all(&mut f, genuine.as_bytes())).unwrap();
        assert!(fetch(&ha, "g", &a, &ra));
        assert_eq!(verdict::changes(&ra, &a.base, &a.head).unwrap().iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["f1.txt"]);
        let incoming =
            std::fs::read_dir(ra.join(".git/objects")).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("incoming")).count();
        assert_eq!(incoming, 0, "no quarantine is left behind");
        let _ = std::fs::remove_dir_all(root);
    }
}
