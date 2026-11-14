//! The git side of a hand-off: the repo checks, the snapshot, the brief, the ref pushes and the claim
//! commit.
//!
//! Every call runs `git -C <repo>` with stdin closed (or a payload piped in), no terminal prompt
//! (`GIT_TERMINAL_PROMPT=0`: a push that needs a password fails instead of waiting) and its stderr
//! kept for the reason. The engineer's branch, index and stash are never touched: the snapshot builds
//! its tree in an index file of its own, and the taker's checkout makes a new branch.
//!
//! Every call to a remote names it by one URL, the arbiter's (mod.rs), never by a remote's name: a
//! remote with a `pushurl` would otherwise write the compare-and-swap register in one repo and read it
//! in another.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The refs of one hand-off on a remote: `refs/borg/handoff/<id>/<leaf>`.
pub(crate) fn remote_ref(id: &str, leaf: &str) -> String {
    format!("refs/borg/handoff/{id}/{leaf}")
}

/// The branch a taker gets: `borg/handoff/<id>`.
pub(crate) fn branch_of(id: &str) -> String {
    format!("borg/handoff/{id}")
}

type Env<'a> = [(&'a str, &'a std::ffi::OsStr)];

fn command(dir: &Path, args: &[&str], env: &Env) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(args).env("GIT_TERMINAL_PROMPT", "0").envs(env.iter().copied()).stdout(Stdio::piped()).stderr(Stdio::piped());
    c
}

/// One git call in `dir`: its stdout as it came, or its error's last stderr line.
fn run_raw(dir: &Path, args: &[&str], env: &Env, input: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let what = || format!("git {}", args.first().unwrap_or(&""));
    let mut c = command(dir, args, env);
    c.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = c.spawn().map_err(|e| format!("cannot run git: {e}"))?;
    if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(bytes).map_err(|e| format!("{}: {e}", what()))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("{}: {e}", what()))?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let last = err.lines().rev().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("hint:")).unwrap_or("failed");
    Err(format!("{}: {last}", what()))
}

/// `run_raw`, its stdout as trimmed text.
fn run_in(dir: &Path, args: &[&str], env: &Env, input: Option<&[u8]>) -> Result<String, String> {
    run_raw(dir, args, env, input).map(|o| String::from_utf8_lossy(&o).trim().to_string())
}

fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    run_in(dir, args, &[], None)
}

/// The top level of the work tree `cwd` is in.
pub(crate) fn toplevel(cwd: &Path) -> Option<PathBuf> {
    run(cwd, &["rev-parse", "--show-toplevel"]).ok().filter(|t| !t.is_empty()).map(PathBuf::from)
}

/// The git directory of the work tree at `dir` (a linked work tree's own), absolute.
pub(crate) fn git_dir(dir: &Path) -> Option<PathBuf> {
    run(dir, &["rev-parse", "--absolute-git-dir"]).ok().filter(|t| !t.is_empty()).map(PathBuf::from)
}

/// `rev` as a full object id, if it names one.
pub(crate) fn rev_parse(dir: &Path, rev: &str) -> Option<String> {
    run(dir, &["rev-parse", "--verify", "--quiet", "--end-of-options", rev]).ok().filter(|s| is_oid(s))
}

/// Whether `s` is a full object id (SHA-1 or SHA-256, lowercase hex).
pub(crate) fn is_oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The tree of commit `sha`.
pub(crate) fn tree_of(dir: &Path, sha: &str) -> Option<String> {
    rev_parse(dir, &format!("{sha}^{{tree}}"))
}

/// Where a repo stands when it may be handed off: on a branch, at a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Base {
    pub branch: String,
    pub head: String,
}

/// `git rev-parse --git-path <file>`, absolute.
fn git_path(root: &Path, file: &str) -> Result<PathBuf, String> {
    let p = run(root, &["rev-parse", "--git-path", file]).map_err(|e| format!("not a git repo ({e})"))?;
    Ok(if Path::new(&p).is_absolute() { PathBuf::from(p) } else { root.join(p) })
}

/// Whether `root` can be handed off as it stands: HEAD on a branch with a commit, and no rebase,
/// merge, cherry-pick, revert or bisect half done (a half-done one cannot be handed over safely).
pub(crate) fn base(root: &Path) -> Result<Base, String> {
    let busy = [
        ("rebase-merge", "a rebase"),
        ("rebase-apply", "a rebase or `git am`"),
        ("MERGE_HEAD", "a merge"),
        ("CHERRY_PICK_HEAD", "a cherry-pick"),
        ("REVERT_HEAD", "a revert"),
        ("BISECT_LOG", "a bisect"),
    ];
    for (file, what) in busy {
        if git_path(root, file)?.exists() {
            return Err(format!("{what} is in progress: finish or abort it first"));
        }
    }
    let branch = run(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).map_err(|_| "HEAD is detached: check out a branch first".to_string())?;
    let head = rev_parse(root, "HEAD").ok_or_else(|| format!("{branch} has no commit yet"))?;
    Ok(Base { branch, head })
}

/// The remote `branch` pushes to: its `pushRemote`, `remote.pushDefault`, its upstream's remote, the
/// only remote, else `origin`; with its push URL.
pub(crate) fn push_remote(root: &Path, branch: &str) -> Result<(String, String), String> {
    let cfg = |k: &str| run(root, &["config", "--get", k]).ok().filter(|v| !v.is_empty());
    let remotes: Vec<String> = run(root, &["remote"]).unwrap_or_default().lines().map(str::to_string).collect();
    let name = cfg(&format!("branch.{branch}.pushRemote"))
        .or_else(|| cfg("remote.pushDefault"))
        .or_else(|| cfg(&format!("branch.{branch}.remote")).filter(|r| r != "."))
        .or_else(|| (remotes.len() == 1).then(|| remotes[0].clone()))
        .or_else(|| remotes.iter().find(|r| *r == "origin").cloned())
        .ok_or_else(|| "no push remote: add one (`git remote add origin <url>`)".to_string())?;
    let url = run(root, &["remote", "get-url", "--push", &name]).map_err(|e| format!("remote {name}: {e}"))?;
    Ok((name, url))
}

/// The fetch and push URLs of every remote of the clone at `dir` as git resolves them (`git remote -v`:
/// `insteadOf` and `pushInsteadOf` rules, included files and the global config applied).
pub(crate) fn remote_urls(dir: &Path) -> Vec<String> {
    let out = run(dir, &["remote", "-v"]).unwrap_or_default();
    out.lines().filter_map(|l| Some(l.split_once('\t')?.1.rsplit_once(' ')?.0.to_string())).collect()
}

/// The remote of `root` that is the arbiter `url` (an offer's push URL, credentials redacted), with
/// the URL every hand-off call then uses: a remote whose push URL names that repo, else one whose fetch
/// URL does (then its fetch URL, which names the arbiter).
pub(crate) fn arbiter(root: &Path, url: &str) -> Option<(String, String)> {
    let want = normal_url(url);
    let names: Vec<String> = run(root, &["remote"]).unwrap_or_default().lines().map(str::to_string).collect();
    let get = |n: &str, push: bool| run(root, &["remote", "get-url", if push { "--push" } else { "--all" }, n]).ok();
    let by = |push: bool| {
        names.iter().find_map(|n| {
            let u = get(n, push)?;
            let u = u.lines().next()?.to_string();
            (normal_url(&u) == want).then(|| (n.clone(), u))
        })
    };
    by(true).or_else(|| by(false))
}

/// A remote URL as two clones of one repo name it alike: no credentials, user, scheme or `.git`
/// suffix, the host lower-cased, and scp-like `host:path` as `host/path`. A local path stays a path.
pub(crate) fn normal_url(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    let u = u.strip_suffix(".git").unwrap_or(u).trim_end_matches('/');
    let rest = match u.split_once("://") {
        Some(("file", p)) => return p.to_string(),
        Some((_, r)) => r.to_string(),
        None if u.starts_with('/') || u.starts_with('.') => return u.to_string(),
        // scp-like `[user@]host:path`
        None => match u.split_once(':') {
            Some((host, path)) if !host.contains('/') => format!("{host}/{}", path.trim_start_matches('/')),
            _ => return u.to_string(),
        },
    };
    let rest = rest.rsplit_once('@').filter(|(user, _)| !user.contains('/')).map_or(rest.as_str(), |(_, r)| r);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    format!("{host}/{path}")
}

/// `git push` to `url` of `specs` (`<src>:<dst>`, every `dst` a hand-off ref), all or none
/// (`--atomic`). `--no-verify` skips the pre-push hook, for the hand-off's own refs only.
fn push_specs(root: &Path, url: &str, extra: &[&str], specs: &[String]) -> Result<(), String> {
    debug_assert!(specs.iter().all(|s| s.split_once(':').is_some_and(|(_, d)| d.starts_with("refs/borg/handoff/"))));
    let mut args = vec!["push", "--quiet", "--no-verify", "--no-follow-tags", "--atomic"];
    args.extend_from_slice(extra);
    args.push(url);
    args.extend(specs.iter().map(String::as_str));
    run(root, &args).map(drop)
}

/// Pushes each `(src, dst)` to `url` in one atomic push, never forced: git refuses to create a ref that
/// exists, or to move one to a commit that does not descend from it.
pub(crate) fn push(root: &Path, url: &str, refs: &[(&str, &str)]) -> Result<(), String> {
    push_specs(root, url, &[], &refs.iter().map(|(s, d)| format!("{s}:{d}")).collect::<Vec<_>>())
}

/// Deletes `dst` on `url` while it still holds `expect` (`--force-with-lease`): never another's value.
pub(crate) fn delete(root: &Path, url: &str, dst: &str, expect: &str) -> Result<(), String> {
    leased(root, url, &[("", dst, expect)])
}

/// Moves (or, `src` empty, deletes) each `(src, dst, expect)` on `url` in one atomic push, each only
/// while `dst` still holds `expect` (`--force-with-lease`): a compare-and-swap of each, all or none.
pub(crate) fn leased(root: &Path, url: &str, moves: &[(&str, &str, &str)]) -> Result<(), String> {
    let leases: Vec<String> = moves.iter().map(|(_, d, e)| format!("--force-with-lease={d}:{e}")).collect();
    let extra: Vec<&str> = leases.iter().map(String::as_str).collect();
    push_specs(root, url, &extra, &moves.iter().map(|(s, d, _)| format!("{s}:{d}")).collect::<Vec<_>>())
}

/// Every ref of hand-off `id` that `url` holds, as `(leaf, sha)`: `state`, `brief`, `taken`, `abort`,
/// `probe`; empty when it holds none, `Err` when it cannot be read.
pub(crate) fn ls_remote_all(root: &Path, url: &str, id: &str) -> Result<Vec<(String, String)>, String> {
    let prefix = remote_ref(id, "");
    let out = run(root, &["ls-remote", "--refs", url, &format!("{prefix}*")])?;
    Ok(out
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .filter_map(|(sha, r)| Some((r.strip_prefix(&prefix)?.to_string(), sha.to_string())))
        .filter(|(leaf, sha)| is_oid(sha) && LEAVES.contains(&leaf.as_str()))
        .collect())
}

/// The leaves of a hand-off's refs (`remote_ref`): no other name under `refs/borg/handoff/<id>/` is one.
const LEAVES: &[&str] = &["state", "brief", "taken", "abort", "probe"];

/// Every hand-off ref `url` holds, per hand-off id (in order), with the ref names: only the ones
/// `remote_ref` makes (a valid id and a leaf of `LEAVES`), so a name a member planted never reaches a
/// printed command.
pub(crate) fn remote_refs(root: &Path, url: &str) -> Result<Vec<(String, Vec<String>)>, String> {
    let out = run(root, &["ls-remote", "--refs", url, "refs/borg/handoff/*"])?;
    let mut by: Vec<(String, Vec<String>)> = Vec::new();
    for name in out.lines().filter_map(|l| l.split_once('\t').map(|(_, r)| r)) {
        let Some((id, leaf)) = name.strip_prefix("refs/borg/handoff/").and_then(|r| r.split_once('/')) else { continue };
        if !super::is_id(id) || !LEAVES.contains(&leaf) {
            continue;
        }
        match by.iter_mut().find(|(i, _)| i == id) {
            Some((_, names)) => names.push(name.to_string()),
            None => by.push((id.to_string(), vec![name.to_string()])),
        }
    }
    Ok(by)
}

/// Fetches the commit `sha` itself from `url` (a state the claim names that the ref no longer points
/// at); fails on a remote that serves only what its refs reach.
pub(crate) fn fetch_sha(root: &Path, url: &str, sha: &str) -> Result<(), String> {
    run(root, &["fetch", "--quiet", "--no-tags", "--no-write-fetch-head", url, sha]).map(drop)
}

/// Whether the commit `sha` is in the repo at `root`.
pub(crate) fn has_commit(root: &Path, sha: &str) -> bool {
    rev_parse(root, &format!("{sha}^{{commit}}")).is_some()
}

/// Whether `root` has commits on HEAD that no remote-tracking ref has: work only this clone holds.
pub(crate) fn unpushed(root: &Path) -> bool {
    run(root, &["rev-list", "--max-count=1", "HEAD", "--not", "--remotes"]).is_ok_and(|s| !s.is_empty())
}

/// The local refs of hand-off `id` in the clone `root` (`refs/borg/handoff/<id>/*`), with their shas.
pub(crate) fn local_refs(root: &Path, id: &str) -> Vec<(String, String)> {
    let out = run(root, &["for-each-ref", "--format=%(refname) %(objectname)", &remote_ref(id, "")]).unwrap_or_default();
    out.lines().filter_map(|l| l.split_once(' ')).map(|(r, s)| (r.to_string(), s.to_string())).collect()
}

/// The hand-off ids the branches `borg/handoff/<id>[-n]` and the refs `refs/borg/handoff/<id>/*` of the
/// clone `root` name: what a take left behind there.
pub(crate) fn local_ids(root: &Path) -> Vec<String> {
    let out = run(root, &["for-each-ref", "--format=%(refname)", "refs/heads/borg/handoff/", "refs/borg/handoff/"]).unwrap_or_default();
    let mut ids: Vec<String> = out
        .lines()
        .filter_map(|r| r.strip_prefix("refs/heads/borg/handoff/").or_else(|| r.strip_prefix("refs/borg/handoff/")))
        .filter_map(|r| r.get(..13))
        .filter(|i| super::is_id(i))
        .map(str::to_string)
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Deletes the local ref `name` while it holds `sha`.
pub(crate) fn delete_local_ref(root: &Path, name: &str, sha: &str) -> Result<(), String> {
    run(root, &["update-ref", "-d", name, sha]).map(drop)
}

/// The ignored files and directories of the work tree at `path` (`git status --ignored`), which `git
/// worktree remove` would delete without asking.
pub(crate) fn ignored(path: &Path) -> Vec<String> {
    let out = run_raw(path, &["status", "--porcelain", "-z", "--ignored=matching", "--untracked-files=normal"], &[], None).unwrap_or_default();
    out.split(|b| *b == 0).filter_map(|e| std::str::from_utf8(e).ok()?.strip_prefix("!! ")).map(str::to_string).collect()
}

/// Removes the work tree at `path` of the clone `root`; git refuses one with changes.
pub(crate) fn worktree_remove(root: &Path, path: &Path) -> Result<(), String> {
    run(root, &["worktree", "remove", &path.to_string_lossy()]).map(drop)
}

/// What `url` holds at `name`: `Ok(None)` when it has no such ref, `Err` when it cannot be read
/// (unreachable, refused).
pub(crate) fn ls_remote(root: &Path, url: &str, name: &str) -> Result<Option<String>, String> {
    let out = run(root, &["ls-remote", "--refs", url, name])?;
    Ok(out.lines().filter_map(|l| l.split_once('\t')).find(|(_, r)| *r == name).map(|(sha, _)| sha.to_string()).filter(|s| is_oid(s)))
}

/// Fetches `names` from `url` into the local refs of the same names (forced: the local copy follows the
/// remote), without touching FETCH_HEAD, tags or any branch.
pub(crate) fn fetch(root: &Path, url: &str, names: &[&str]) -> Result<(), String> {
    let specs: Vec<String> = names.iter().map(|n| format!("+{n}:{n}")).collect();
    let mut args = vec!["fetch", "--quiet", "--no-tags", "--no-write-fetch-head", url];
    args.extend(specs.iter().map(String::as_str));
    run(root, &args).map(drop)
}

/// The identity a commit-tree needs when the repo's config gives none (a fresh machine): borg's.
fn identity(root: &Path) -> Vec<(&'static str, &'static std::ffi::OsStr)> {
    if run(root, &["var", "GIT_COMMITTER_IDENT"]).is_ok() && run(root, &["var", "GIT_AUTHOR_IDENT"]).is_ok() {
        return Vec::new();
    }
    let (name, email) = (std::ffi::OsStr::new("borg"), std::ffi::OsStr::new("borg@localhost"));
    vec![("GIT_AUTHOR_NAME", name), ("GIT_AUTHOR_EMAIL", email), ("GIT_COMMITTER_NAME", name), ("GIT_COMMITTER_EMAIL", email)]
}

/// A commit of `tree` on `parent` with `message` (piped in, never on the command line).
pub(crate) fn commit_tree(root: &Path, tree: &str, parent: &str, message: &str) -> Result<String, String> {
    run_in(root, &["commit-tree", tree, "-p", parent], &identity(root), Some(message.as_bytes()))
        .and_then(|s| if is_oid(&s) { Ok(s) } else { Err(format!("git commit-tree: {s}")) })
}

/// The message of commit `sha`.
pub(crate) fn message(root: &Path, sha: &str) -> Option<String> {
    let raw = run(root, &["cat-file", "commit", sha]).ok()?;
    raw.split_once("\n\n").map(|(_, m)| m.to_string())
}

/// The brief of a hand-off as a commit on its `state`: a tree of `files` (name, bytes; each written as
/// a blob from stdin), so the brief names the code it belongs to.
pub(crate) fn brief_commit(root: &Path, state: &str, files: &[(&str, &[u8])], message: &str) -> Result<String, String> {
    let mut listing = String::new();
    for (name, bytes) in files {
        let blob = run_in(root, &["hash-object", "-w", "--stdin"], &[], Some(bytes))?;
        if !is_oid(&blob) {
            return Err(format!("git hash-object: {blob}"));
        }
        listing.push_str(&format!("100644 blob {blob}\t{name}\n"));
    }
    let tree = run_in(root, &["mktree"], &[], Some(listing.as_bytes()))?;
    commit_tree(root, &tree, state, message)
}

/// The bytes of `file` in commit `sha`, as they were written.
pub(crate) fn file_at(root: &Path, sha: &str, file: &str) -> Option<Vec<u8>> {
    run_raw(root, &["cat-file", "blob", &format!("{sha}:{file}")], &[], None).ok()
}

/// The working tree of `root` as a commit on `head`, without touching its branch, index or stash.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub state: String,
    pub tree: String,
    /// Files whose change is left out because what it adds holds a secret (`secret_changes`): an
    /// untracked file missing, a tracked one as HEAD has it.
    pub left_out: Vec<String>,
    /// Untracked files left out because they are larger than the bound `head_facts` gives.
    pub large: Vec<String>,
    /// Untracked binary files left out (`new_binaries`): the secret screen reads lines, and a binary
    /// has none (a keystore, a key in DER, a vault, a local database).
    pub binary: Vec<String>,
}

/// git's default `core.bigFileThreshold` (git-config(1)): past it git treats a file as big.
const BIG_FILE: u64 = 512 * 1024 * 1024;

/// What HEAD's tree tells the snapshot: the bound past which an untracked file is listed instead of
/// committed, the larger of git's own measure of a big file (`core.bigFileThreshold` as the repo sets
/// it, else git's default) and the largest file HEAD tracks (a repo that keeps bigger files keeps bigger
/// ones in a hand-off too); and the extensions of the files it tracks (a new binary of a kind the repo
/// keeps goes in, `new_binaries`). One `ls-tree`, trees only.
fn head_facts(root: &Path, head: &str) -> (u64, BTreeSet<String>) {
    let big = run(root, &["config", "--type=int", "--get", "core.bigFileThreshold"]).ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(BIG_FILE);
    let out = run_raw(root, &["ls-tree", "-r", "-l", "-z", head], &[], None).unwrap_or_default();
    let (mut largest, mut exts) = (0, BTreeSet::new());
    for e in out.split(|b| *b == 0).filter_map(|e| std::str::from_utf8(e).ok()) {
        let Some((meta, name)) = e.split_once('\t') else { continue };
        largest = largest.max(meta.split_whitespace().nth(3).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0));
        exts.extend(extension(name));
    }
    (big.max(largest), exts)
}

/// A file name's extension, lower-cased (`a/b.P12` → `p12`); none for a name without one.
fn extension(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').filter(|(stem, ext)| !stem.is_empty() && !ext.is_empty()).map(|(_, ext)| ext.to_ascii_lowercase())
}

/// The files the staged change (the index in `env` against `head`) adds that git reads as binary
/// (`--numstat` prints `-` for their lines), unless HEAD tracks files of their extension (`kept`).
fn new_binaries(root: &Path, head: &str, env: &Env, kept: &BTreeSet<String>) -> Result<Vec<String>, String> {
    let out = run_raw(root, &["diff", "--cached", "--numstat", "-z", "--no-renames", "--diff-filter=A", "--no-ext-diff", "--no-textconv", head], env, None)?;
    Ok(out
        .split(|b| *b == 0)
        .filter_map(|e| std::str::from_utf8(e).ok()?.strip_prefix("-\t-\t"))
        .filter(|p| extension(p).is_none_or(|x| !kept.contains(&x)))
        .map(str::to_string)
        .collect())
}

/// Snapshots `root`: an index of its own (`index`, removed after), seeded from the real index (its
/// skip-worktree bits keep a sparse checkout's files, its stat cache spares hashing every file again),
/// the tracked changes added (`git add -u`) and every untracked file not ignored up to the bound of `head_facts`
/// (a submodule as its commit, an LFS file as its pointer; larger ones are listed, never read), the
/// changes that add a secret and the new binaries borg cannot screen taken out again (`secret_changes`,
/// `new_binaries`, on what the index holds: what is screened is what is committed), then `write-tree`
/// and `commit-tree` on `head`.
pub(crate) fn snapshot(root: &Path, head: &str, index: &Path, message: &str) -> Result<Snapshot, String> {
    let env = [("GIT_INDEX_FILE", index.as_os_str()), ("GIT_LITERAL_PATHSPECS", std::ffi::OsStr::new("1"))];
    let git = |args: &[&str], input: Option<&[u8]>| run_in(root, args, &env, input);
    let _ = std::fs::remove_file(index);
    let out = (|| {
        if std::fs::copy(git_path(root, "index")?, index).is_err() {
            git(&["read-tree", head], None)?;
        }
        git(&["add", "-u"], None)?;
        let untracked = run_raw(root, &["ls-files", "-z", "--others", "--exclude-standard"], &env, None)?;
        let (bound, kept) = head_facts(root, head);
        let (mut add, mut large) = (Vec::new(), Vec::new());
        for p in untracked.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let name = String::from_utf8_lossy(p);
            let size = std::fs::symlink_metadata(root.join(name.as_ref())).map_or(0, |m| m.len());
            if size > bound {
                large.push(name.into_owned());
            } else {
                add.extend(p.iter().copied().chain([0]));
            }
        }
        if !add.is_empty() {
            git(&["add", "--pathspec-from-file=-", "--pathspec-file-nul"], Some(&add))?;
        }
        let left_out = secret_changes(root, head, &env)?;
        let binary: Vec<String> = new_binaries(root, head, &env, &kept)?.into_iter().filter(|p| !left_out.contains(p)).collect();
        if !left_out.is_empty() || !binary.is_empty() {
            let list: Vec<u8> = left_out.iter().chain(&binary).flat_map(|p| p.bytes().chain([0])).collect();
            git(&["reset", "-q", head, "--pathspec-from-file=-", "--pathspec-file-nul"], Some(&list))?;
        }
        let tree = git(&["write-tree"], None)?;
        let state = commit_tree(root, &tree, head, message)?;
        Ok(Snapshot { state, tree, left_out, large, binary })
    })();
    let _ = std::fs::remove_file(index);
    out
}

/// Whether a secret is known by its key's name in the file `path` too (notes.rs `has_secret`): in
/// `.env` files and dotfiles of credentials (`.npmrc`, `.netrc`), not in code, where such names are
/// mostly variables.
fn names_secrets(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name.starts_with(".env")
        || name.ends_with(".env")
        || (name.starts_with('.') && !name[1..].contains('.'))
        || name.contains("credential")
        || name.contains("secret")
}

/// The files whose staged change (the index in `env` against `head`) adds a line holding a secret,
/// read from git's diff as it streams: git reads the files (a symlink as its target, a binary file or
/// one past `core.bigFileThreshold` as no lines), borg holds one line at a time.
fn secret_changes(root: &Path, head: &str, env: &Env) -> Result<Vec<String>, String> {
    let args = [
        "-c",
        "core.quotePath=false",
        "diff",
        "--cached",
        "-U0",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        head,
    ];
    let mut c = command(root, &args, env);
    c.stdin(Stdio::null()).stderr(Stdio::null());
    let mut child = c.spawn().map_err(|e| format!("cannot run git: {e}"))?;
    let stdout = child.stdout.take().ok_or("git diff: no output")?;
    let (mut found, mut path, mut header) = (BTreeSet::new(), None::<String>, false);
    let mut r = std::io::BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        match r.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => return Err(format!("git diff: {e}")),
        }
        let l = String::from_utf8_lossy(&line);
        let l = l.strip_suffix('\n').unwrap_or(&l);
        if l.starts_with("diff --git ") {
            (path, header) = (None, true);
        } else if header && let Some(p) = l.strip_prefix("+++ ") {
            path = unquote(p).strip_prefix("b/").map(str::to_string);
        } else if l.starts_with("@@") {
            header = false;
        } else if !header
            && let (Some(added), Some(p)) = (l.strip_prefix('+'), &path)
            && !found.contains(p)
            && crate::notes::has_secret(added, names_secrets(p))
        {
            found.insert(p.clone());
        }
    }
    let status = child.wait().map_err(|e| format!("git diff: {e}"))?;
    if !status.success() {
        return Err("git diff: failed".into());
    }
    Ok(found.into_iter().collect())
}

/// A path as git prints it in a diff header: as is, or C-quoted when it holds a quote, a backslash or
/// a control character.
fn unquote(p: &str) -> String {
    let Some(inner) = p.strip_prefix('"').and_then(|q| q.strip_suffix('"')) else { return p.to_string() };
    let (mut out, mut it) = (Vec::new(), inner.bytes().peekable());
    while let Some(b) = it.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match it.next() {
            Some(d @ b'0'..=b'7') => {
                let mut v = u32::from(d - b'0');
                for _ in 0..2 {
                    if let Some(&o @ b'0'..=b'7') = it.peek() {
                        v = v * 8 + u32::from(o - b'0');
                        it.next();
                    }
                }
                out.push(u8::try_from(v).unwrap_or(b'?'));
            }
            Some(b'a') => out.push(7),
            Some(b'b') => out.push(8),
            Some(b't') => out.push(b'\t'),
            Some(b'n') => out.push(b'\n'),
            Some(b'v') => out.push(11),
            Some(b'f') => out.push(12),
            Some(b'r') => out.push(b'\r'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What changed from `from` to `to`, one `git diff --name-status` line per file.
pub(crate) fn changes(root: &Path, from: &str, to: &str) -> Vec<String> {
    run(root, &["diff", "--name-status", "--no-renames", from, to]).unwrap_or_default().lines().map(|l| l.replace('\t', " ")).collect()
}

/// Whether `root` has no change to commit: nothing staged, modified or untracked (ignored files aside).
pub(crate) fn clean(root: &Path) -> bool {
    run(root, &["status", "--porcelain", "--untracked-files=normal"]).is_ok_and(|s| s.is_empty())
}

/// The branch `root`'s HEAD is on, if any.
pub(crate) fn current_branch(root: &Path) -> Option<String> {
    run(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok()
}

/// The branch to check `state` out on: `want` when it is free or already points at `state`, else the
/// first free `want-2`, `want-3`, …; with whether it exists already (and points at `state`).
pub(crate) fn branch_for(root: &Path, want: &str, state: &str) -> (String, bool) {
    for n in 1.. {
        let name = if n == 1 { want.to_string() } else { format!("{want}-{n}") };
        match rev_parse(root, &format!("refs/heads/{name}")) {
            None => return (name, false),
            Some(at) if at == state => return (name, true),
            Some(_) => {}
        }
    }
    unreachable!("an unbounded range always yields a free name")
}

/// The work tree `branch` is checked out in, if any (`git worktree list`).
pub(crate) fn worktree_of(root: &Path, branch: &str) -> Option<PathBuf> {
    let list = run(root, &["worktree", "list", "--porcelain"]).ok()?;
    let want = format!("branch refs/heads/{branch}");
    list.split("\n\n").find(|w| w.lines().any(|l| l == want)).and_then(|w| w.lines().find_map(|l| l.strip_prefix("worktree ")).map(PathBuf::from))
}

/// Checks `state` out on `branch`: switched to in `root` itself (`in_place`), else in a new work tree at
/// `path`. A branch that exists already (pointing at `state`) is reused.
pub(crate) fn check_out(root: &Path, branch: &str, exists: bool, state: &str, in_place: bool, path: &Path) -> Result<PathBuf, String> {
    if let Some(at) = worktree_of(root, branch) {
        return Ok(at);
    }
    let path_s = path.to_string_lossy();
    match (in_place, exists) {
        (true, true) => run(root, &["switch", "--quiet", branch]).map(|_| root.to_path_buf()),
        (true, false) => run(root, &["switch", "--quiet", "-c", branch, state]).map(|_| root.to_path_buf()),
        (false, true) => run(root, &["worktree", "add", "--quiet", &path_s, branch]).map(|_| path.to_path_buf()),
        (false, false) => run(root, &["worktree", "add", "--quiet", "-b", branch, &path_s, state]).map(|_| path.to_path_buf()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_reads_alike_from_every_clone() {
        let same = ["git@github.com:team/app.git", "ssh://git@GitHub.com/team/app", "https://user:tok@github.com/team/app.git/", "https://github.com/team/app"];
        assert!(same.iter().all(|u| normal_url(u) == "github.com/team/app"), "{:?}", same.map(normal_url));
        assert_eq!(normal_url("/srv/git/app.git"), "/srv/git/app");
        assert_eq!(normal_url("file:///srv/git/app.git"), "/srv/git/app");
        assert_ne!(normal_url("git@github.com:team/app"), normal_url("git@github.com:fork/app"));
    }

    #[test]
    fn an_oid_is_full_lowercase_hex() {
        assert!(is_oid(&"a".repeat(40)) && is_oid(&"0".repeat(64)));
        assert!(!is_oid(&"A".repeat(40)) && !is_oid("abc") && !is_oid(&"g".repeat(40)));
    }

    #[test]
    fn a_quoted_diff_path_reads_back() {
        assert_eq!(unquote("b/src/a.rs"), "b/src/a.rs");
        assert_eq!(unquote(r#""b/a\tb\"c\\d\303\244""#), "b/a\tb\"c\\dä");
    }

    #[test]
    fn names_count_in_env_files_and_dotfiles_only() {
        assert!(names_secrets(".env") && names_secrets("conf/.env.local") && names_secrets("prod.env") && names_secrets(".npmrc"));
        assert!(names_secrets("aws/credentials") && !names_secrets("src/auth.rs") && !names_secrets("package-lock.json") && !names_secrets(".gitignore.bak"));
    }
}
