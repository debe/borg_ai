//! The write-ahead log of this machine's hand-off operations: `<store>/handoff/<id>.wal`.
//!
//! An offer, a take, an owner's refresh (refresh.rs), an abort and a `done` (close.rs) each take
//! several steps against git remotes that can fail or be cut off between any two. Before each step
//! acts, the operation appends what it is about to do (the intent, with everything a later run needs
//! to finish it) or what it just finished, one JSON line, synced to disk (`fsync`) before it goes on.
//! Every step is idempotent, so a run that stopped half way is rolled forward (or, an offer whose
//! pushes never completed, rolled back; a refresh that cannot be published, put back) by the next
//! `borg handoff` on this machine, from its lines alone.
//!
//! One operation per hand-off at a time: an operation holds `<id>.op.lock` (an exclusive file lock)
//! from its intent to its end, so a second one on this machine waits, and an operation found open with
//! the lock free is one whose process ended: recovery takes it over. The kernel drops the lock with
//! the process, however it ended.
//!
//! Once no operation in `<id>.wal` is open, the file is renamed to `<id>.<n>.done` (one rename: atomic),
//! so the `*.wal` files are the open ones, which recovery and the prompt hook read; `read` reads both.

use std::borrow::Cow;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// `<store>/handoff`: the logs and locks of this machine's hand-off operations.
pub(crate) fn dir(home: &Path) -> PathBuf {
    home.join("handoff")
}

fn path(home: &Path, id: &str) -> PathBuf {
    dir(home).join(format!("{id}.wal"))
}

/// The closed logs of `id`, oldest first: `<id>.<n>.done`.
fn closed(home: &Path, id: &str) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir(home)) else { return Vec::new() };
    let mut v: Vec<(u64, PathBuf)> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name();
            let n = name.to_str()?.strip_prefix(id)?.strip_prefix('.')?.strip_suffix(".done")?.parse().ok()?;
            Some((n, e.path()))
        })
        .collect();
    v.sort();
    v.into_iter().map(|(_, p)| p).collect()
}

/// One repo of an offer, as its intent records it before anything is pushed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Plan<'a> {
    #[serde(borrow)]
    pub root: Cow<'a, str>,
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    #[serde(borrow)]
    pub remote: Cow<'a, str>,
    #[serde(borrow)]
    pub url: Cow<'a, str>,
    #[serde(borrow)]
    pub branch: Cow<'a, str>,
    #[serde(borrow)]
    pub head: Cow<'a, str>,
    /// `url` with its ssh host alias resolved (git.rs `resolved_url`), read once at the offer so every
    /// run builds the same offer line; empty when it is `url` itself, and in a log of 0.8.0.
    #[serde(borrow, default, skip_serializing_if = "str::is_empty")]
    pub resolved: Cow<'a, str>,
    /// The root commits of `head` (git.rs `roots`): which clones a taker asks first. Absent in a log of
    /// 0.8.0.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<Cow<'a, str>>,
}

/// The acting session as an operation records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Who<'a> {
    #[serde(borrow)]
    pub node: Cow<'a, str>,
    #[serde(borrow)]
    pub session: Cow<'a, str>,
    #[serde(borrow)]
    pub engineer: Cow<'a, str>,
    #[serde(borrow)]
    pub harness: Cow<'a, str>,
}

/// The verb of a claim: who ends the offer, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Verb {
    /// Another engineer takes the work.
    Take,
    /// The offering engineer takes it back.
    Back,
    /// The offering engineer, an admin or a steerer closes it: on an open offer a claim of `taken`
    /// (nobody gets the work), after a take the commit of `abort` (both sides fenced until it is done).
    Abort,
}

impl Verb {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Verb::Take => "take",
            Verb::Back => "back",
            Verb::Abort => "abort",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Verb> {
        [Verb::Take, Verb::Back, Verb::Abort].into_iter().find(|v| v.as_str() == s)
    }
}

/// A repo of a take besides the arbiter, as its intent records it: its clone here (empty: none, its
/// clone URL is listed), the URL every call uses, and the state and tree the claim names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TakeRepo<'a> {
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    #[serde(borrow)]
    pub root: Cow<'a, str>,
    #[serde(borrow)]
    pub url: Cow<'a, str>,
    #[serde(borrow)]
    pub state: Cow<'a, str>,
    #[serde(borrow)]
    pub tree: Cow<'a, str>,
    /// The branch it was offered from (where a clean clone on it checks out in place).
    #[serde(borrow)]
    pub branch: Cow<'a, str>,
}

/// One line of the log: a step of the operation `op`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub(crate) enum Step<'a> {
    /// An offer's intent: everything its offer line is built from.
    Offer {
        #[serde(borrow)]
        by: Who<'a>,
        ms: i64,
        /// Makes the id (mod.rs `id_for`).
        #[serde(borrow)]
        nonce: Cow<'a, str>,
        #[serde(borrow)]
        note: Cow<'a, str>,
        /// The brief's first lines, for listings (mod.rs `head_of`).
        #[serde(borrow)]
        brief_head: Cow<'a, str>,
        #[serde(borrow)]
        repos: Vec<Plan<'a>>,
        /// Whom it asks (`--to`), as the check resolved it: an engineer's email or a node key.
        #[serde(borrow, default, deserialize_with = "crate::msg::cow_opt", skip_serializing_if = "Option::is_none")]
        to: Option<Cow<'a, str>>,
    },
    /// Repo `repo` of the offer snapshotted (nothing pushed yet). A refresh's names the branch and
    /// HEAD it was taken on when they moved since the offer.
    Snapshot {
        repo: usize,
        #[serde(borrow)]
        state: Cow<'a, str>,
        #[serde(borrow)]
        tree: Cow<'a, str>,
        #[serde(borrow, deserialize_with = "crate::msg::cow_vec", default)]
        left_out: Vec<Cow<'a, str>>,
        /// Untracked files past git.rs `head_facts`'s bound, listed instead of committed.
        #[serde(borrow, deserialize_with = "crate::msg::cow_vec", default, skip_serializing_if = "Vec::is_empty")]
        large: Vec<Cow<'a, str>>,
        /// Untracked binary files left out (git.rs `new_binaries`).
        #[serde(borrow, deserialize_with = "crate::msg::cow_vec", default, skip_serializing_if = "Vec::is_empty")]
        binary: Vec<Cow<'a, str>>,
        #[serde(borrow, default, deserialize_with = "crate::msg::cow_opt", skip_serializing_if = "Option::is_none")]
        branch: Option<Cow<'a, str>>,
        #[serde(borrow, default, deserialize_with = "crate::msg::cow_opt", skip_serializing_if = "Option::is_none")]
        head: Option<Cow<'a, str>>,
    },
    /// The owner's refresh of an open offer (refresh.rs): its snapshots, brief and pushes follow; a
    /// refresh that was interrupted, or raced a claim, puts back what it moved (`Restored`).
    Refresh { ms: i64 },
    /// Repo `repo`'s refs hold again what they held before the refresh; in a take, what its claim
    /// names (mod.rs `put_claimed`).
    Restored { repo: usize },
    /// The brief committed on the arbiter repo's state (nothing pushed yet).
    Brief {
        #[serde(borrow)]
        sha: Cow<'a, str>,
    },
    /// About to push repo `repo`'s probe, then its state (and the brief, in the same atomic push).
    Pushing { repo: usize },
    /// The push of repo `repo`'s state returned success (not yet read back).
    Pushed { repo: usize },
    /// The remote holds repo `repo`'s state (and the brief), read back by `ls-remote`.
    Verified { repo: usize },
    /// A take's intent: the state it claims and where it checks the work out.
    Take {
        #[serde(borrow)]
        by: Who<'a>,
        verb: Verb,
        /// Taken back into the very tree it was offered from: nothing to check out.
        in_place: bool,
        #[serde(borrow)]
        root: Cow<'a, str>,
        /// The remote's name, for messages.
        #[serde(borrow)]
        remote: Cow<'a, str>,
        /// The arbiter's URL, which every git call of the take uses (git.rs `arbiter`).
        #[serde(borrow)]
        url: Cow<'a, str>,
        #[serde(borrow)]
        state: Cow<'a, str>,
        #[serde(borrow)]
        tree: Cow<'a, str>,
        #[serde(borrow)]
        brief: Cow<'a, str>,
        /// The offer's other repos.
        #[serde(borrow, default, skip_serializing_if = "Vec::is_empty")]
        more: Vec<TakeRepo<'a>>,
    },
    /// An abort's intent (close.rs): who closes it and why, on which arbiter, and the take's claim it
    /// follows (none: the offer is open, the abort claims `taken`).
    Abort {
        #[serde(borrow)]
        by: Who<'a>,
        #[serde(borrow)]
        why: Cow<'a, str>,
        #[serde(borrow)]
        root: Cow<'a, str>,
        #[serde(borrow)]
        remote: Cow<'a, str>,
        #[serde(borrow)]
        url: Cow<'a, str>,
        #[serde(borrow, default, deserialize_with = "crate::msg::cow_opt", skip_serializing_if = "Option::is_none")]
        after: Option<Cow<'a, str>>,
    },
    /// The intent of `borg handoff done` (close.rs): who closes it; every step after is idempotent.
    Close {
        #[serde(borrow)]
        by: Who<'a>,
    },
    /// Repo `repo`'s hand-off refs deleted from its remote (or found gone).
    Cleared { repo: usize },
    /// The claim commit made, about to be pushed: the fencing token everything after cites.
    Claim {
        #[serde(borrow)]
        sha: Cow<'a, str>,
    },
    /// The claim's push returned, whatever it said: the remote's `taken` ref decides next.
    Sent,
    /// The remote's `taken` ref is this operation's claim (`Claim`, or an earlier claim of this session
    /// found on the remote).
    Won {
        #[serde(borrow)]
        sha: Cow<'a, str>,
    },
    /// The offering session told (or its notice found sent already).
    Told,
    /// Repo `repo` checked out: the branch, and the work tree it is in.
    Checkout {
        #[serde(default)]
        repo: usize,
        #[serde(borrow)]
        branch: Cow<'a, str>,
        #[serde(borrow)]
        path: Cow<'a, str>,
    },
    /// The hand-off line appended to the log (`handoffs.jsonl`).
    Logged,
    /// The operation ended as it should.
    Done,
    /// It ended without effect, and why: nothing it pushed is left (an offer rolled back, a claim
    /// another took first, a remote that refused).
    Ended {
        #[serde(borrow)]
        why: Cow<'a, str>,
    },
}

impl Step<'_> {
    /// The step's name, as `BORG_TEST_HANDOFF_STOP` names it.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Step::Offer { .. } => "offer",
            Step::Snapshot { .. } => "snapshot",
            Step::Refresh { .. } => "refresh",
            Step::Restored { .. } => "restored",
            Step::Abort { .. } => "abort",
            Step::Close { .. } => "close",
            Step::Cleared { .. } => "cleared",
            Step::Brief { .. } => "brief",
            Step::Pushing { .. } => "pushing",
            Step::Pushed { .. } => "pushed",
            Step::Verified { .. } => "verified",
            Step::Take { .. } => "take",
            Step::Claim { .. } => "claim",
            Step::Sent => "sent",
            Step::Won { .. } => "won",
            Step::Told => "told",
            Step::Checkout { .. } => "checkout",
            Step::Logged => "logged",
            Step::Done => "done",
            Step::Ended { .. } => "ended",
        }
    }

    /// Whether the operation ends here.
    pub(crate) fn last(&self) -> bool {
        matches!(self, Step::Done | Step::Ended { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry<'a> {
    #[serde(borrow)]
    pub op: Cow<'a, str>,
    #[serde(borrow, flatten)]
    pub step: Step<'a>,
}

/// The test hook: with `BORG_TEST_HANDOFF_STOP=<step>` (and the test harness's variable, harness/mod.rs
/// `TEST_ENV`), an operation returns right after that step is logged, as an interrupted run would:
/// the crash a test injects without killing anything.
pub(crate) const TEST_STOP: &str = "BORG_TEST_HANDOFF_STOP";

/// Why a step did not go on.
#[derive(Debug)]
pub(crate) enum Halt {
    /// The log could not be written: the step did not run.
    Io(std::io::Error),
    /// The operation failed, and why (a remote refused or unreachable, a checkout that failed).
    Failed(String),
    /// `TEST_STOP` asked to stop here.
    Stopped(&'static str),
}

impl std::fmt::Display for Halt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Halt::Io(e) => write!(f, "cannot write the hand-off log: {e}"),
            Halt::Failed(why) => f.write_str(why),
            Halt::Stopped(s) => write!(f, "stopped after step {s} ({TEST_STOP})"),
        }
    }
}

/// An operation in progress on one hand-off: holds its lock until dropped.
pub(crate) struct Op {
    home: PathBuf,
    handoff: String,
    path: PathBuf,
    op: String,
    _lock: std::fs::File,
}

/// The lock of hand-off `id`'s operations; `wait`: block until it is free, else None when held.
fn lock(home: &Path, id: &str, wait: bool) -> std::io::Result<Option<std::fs::File>> {
    lock_at(home, &format!("{id}.op.lock"), wait)
}

/// The lock of the background `borg handoff` of this store (mod.rs `background`): held while one runs,
/// so one runs at a time; None while held.
pub(crate) fn background(home: &Path) -> std::io::Result<Option<std::fs::File>> {
    lock_at(home, "background.lock", false)
}

/// The lock file `name` of this store's hand-off directory (`dir`); `wait`: block until it is free,
/// else None when held.
fn lock_at(home: &Path, name: &str, wait: bool) -> std::io::Result<Option<std::fs::File>> {
    std::fs::create_dir_all(dir(home))?;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir(home).join(name))?;
    if wait {
        f.lock()?;
        return Ok(Some(f));
    }
    match f.try_lock() {
        Ok(()) => Ok(Some(f)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

impl Op {
    /// Starts a new operation on `id`, waiting for one this machine runs on it to end.
    pub(crate) fn begin(home: &Path, id: &str) -> std::io::Result<Op> {
        let lock = lock(home, id, true)?.ok_or_else(|| std::io::Error::other("lock"))?;
        Ok(Op { home: home.to_path_buf(), handoff: id.to_string(), path: path(home, id), op: nonce(), _lock: lock })
    }

    /// Takes over the open operation `op` of `id` whose process ended; None while one runs.
    pub(crate) fn resume(home: &Path, id: &str, op: &str) -> std::io::Result<Option<Op>> {
        Ok(lock(home, id, false)?.map(|l| Op { home: home.to_path_buf(), handoff: id.to_string(), path: path(home, id), op: op.to_string(), _lock: l }))
    }

    pub(crate) fn id(&self) -> &str {
        &self.op
    }

    /// Appends `step` and syncs it to disk before the caller acts on it; then the test hook. A step
    /// that ends the last open operation of the log closes the log (module doc).
    pub(crate) fn log(&self, step: Step) -> Result<(), Halt> {
        let line = serde_json::to_string(&Entry { op: Cow::Borrowed(&self.op), step: step.clone() }).map_err(|e| Halt::Io(e.into()))?;
        let new = !self.path.exists();
        let mut f = std::fs::OpenOptions::new().create(true).read(true).append(true).open(&self.path).map_err(Halt::Io)?;
        // A write a crash cut short left a last line without its newline: end it first, or this step
        // would join it and be lost with it.
        let torn = || -> std::io::Result<bool> {
            use std::io::{Read, Seek, SeekFrom};
            let len = f.metadata()?.len();
            if len == 0 {
                return Ok(false);
            }
            let mut last = [0u8];
            (&f).seek(SeekFrom::Start(len - 1))?;
            (&f).read_exact(&mut last)?;
            Ok(last[0] != b'\n')
        };
        let lead = if torn().map_err(Halt::Io)? { "\n" } else { "" };
        f.write_all(format!("{lead}{line}\n").as_bytes()).and_then(|()| f.sync_data()).map_err(Halt::Io)?;
        // A new file is durable once its directory entry is.
        if new {
            sync_dir(&self.path);
        }
        if step.last() {
            self.close();
        }
        let stop = std::env::var_os(crate::harness::TEST_ENV).is_some() && std::env::var(TEST_STOP).is_ok_and(|s| s == step.name());
        if stop { Err(Halt::Stopped(step.name())) } else { Ok(()) }
    }

    /// Renames the log to the next `<id>.<n>.done` once none of its operations is open. Under the lock,
    /// so no step is appended meanwhile; a failed rename leaves it open, which only costs a read.
    fn close(&self) {
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        if ops(&text).iter().any(|(_, s)| s.last().is_some_and(|s| !s.last())) {
            return;
        }
        let n = closed(&self.home, &self.handoff).len();
        let to = (n..).map(|k| dir(&self.home).join(format!("{}.{k}.done", self.handoff))).find(|p| !p.exists()).unwrap_or_default();
        if std::fs::rename(&self.path, &to).is_ok() {
            sync_dir(&to);
        }
    }
}

/// Syncs the directory of `file`, so a file created or renamed there stays.
fn sync_dir(file: &Path) {
    if let Some(d) = file.parent() {
        let _ = std::fs::File::open(d).and_then(|d| d.sync_all());
    }
}

/// 16 hex of blake3 over this process, the time and a counter: an operation's id, a claim's nonce.
pub(crate) fn nonce() -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut h = blake3::Hasher::new();
    h.update(&std::process::id().to_le_bytes());
    h.update(&std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()).to_le_bytes());
    h.update(&N.fetch_add(1, std::sync::atomic::Ordering::Relaxed).to_le_bytes());
    h.finalize().to_hex()[..16].to_string()
}

/// The logs of hand-off `id` on this machine, open and closed (empty when it has none). The open one is
/// read first: one closed in between is then read twice, never missed, and a line read twice is kept
/// once.
pub(crate) fn read(home: &Path, id: &str) -> String {
    let open = std::fs::read_to_string(path(home, id)).unwrap_or_default();
    let done: Vec<String> = closed(home, id).iter().filter_map(|p| std::fs::read_to_string(p).ok()).collect();
    let mut seen = std::collections::HashSet::new();
    done.iter().chain([&open]).flat_map(|t| t.lines()).filter(|l| seen.insert(*l)).map(|l| format!("{l}\n")).collect()
}

/// The operations of a log `text`, each with its steps in order, in the order they began. A line that
/// does not parse (a write cut short by a crash) is skipped: its step did not happen.
pub(crate) fn ops(text: &str) -> Vec<(Cow<'_, str>, Vec<Step<'_>>)> {
    let mut out: Vec<(Cow<str>, Vec<Step>)> = Vec::new();
    for e in text.lines().filter_map(|l| serde_json::from_str::<Entry>(l).ok()) {
        match out.iter_mut().find(|(op, _)| *op == e.op) {
            Some((_, steps)) => steps.push(e.step),
            None => out.push((e.op, vec![e.step])),
        }
    }
    out
}

/// Removes the closed logs of hand-off `id` while none of its operations is open (a done hand-off's:
/// mod.rs compacts what the remote holds nothing of any more).
pub(crate) fn prune(home: &Path, id: &str) {
    if path(home, id).exists() {
        return;
    }
    for p in closed(home, id) {
        let _ = std::fs::remove_file(p);
    }
}

/// The ids of the hand-offs with an open log on this machine.
pub(crate) fn ids(home: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir(home)) else { return Vec::new() };
    let mut v: Vec<String> =
        rd.filter_map(Result::ok).filter_map(|e| e.file_name().to_str()?.strip_suffix(".wal").map(str::to_string)).filter(|i| super::is_id(i)).collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-handoff-wal-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn steps_read_back_per_operation_and_a_torn_line_did_not_happen() {
        let home = tmp("read");
        let id = "h0123456789ab";
        let a = Op::begin(&home, id).expect("op");
        a.log(Step::Claim { sha: "a".repeat(40).into() }).expect("logged");
        a.log(Step::Ended { why: "taken by x\n\"quoted\"".into() }).expect("logged");
        let op = a.id().to_string();
        drop(a);
        let b = Op::begin(&home, id).expect("op");
        b.log(Step::Logged).expect("logged");
        drop(b);
        let mut text = read(&home, id);
        text.push_str("{\"op\":\"x\",\"step\":\"clai");
        let ops = ops(&text);
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].0, op);
        assert_eq!(ops[0].1[1], Step::Ended { why: "taken by x\n\"quoted\"".into() });
        assert!(ops[0].1[1].last() && !ops[1].1[0].last());
        assert_eq!(ids(&home), vec![id.to_string()]);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A step logged after a torn last line reads back as that step; a log whose operations all ended is
    /// closed, still read, and no longer listed as open.
    #[test]
    fn a_torn_tail_never_swallows_the_next_step_and_an_ended_log_closes() {
        let home = tmp("torn");
        let id = "h0123456789ab";
        let a = Op::begin(&home, id).expect("op");
        a.log(Step::Claim { sha: "a".repeat(40).into() }).expect("logged");
        std::fs::OpenOptions::new().append(true).open(path(&home, id)).unwrap().write_all(b"{\"op\":\"x\",\"st").unwrap();
        a.log(Step::Sent).expect("logged");
        let text = read(&home, id);
        assert_eq!(ops(&text)[0].1, vec![Step::Claim { sha: "a".repeat(40).into() }, Step::Sent], "{text}");
        a.log(Step::Ended { why: "x".into() }).expect("logged");
        assert!(ids(&home).is_empty() && !path(&home, id).exists(), "closed");
        assert_eq!(ops(&read(&home, id))[0].1.len(), 3, "a closed log still reads");
        drop(a);
        let b = Op::begin(&home, id).expect("op");
        b.log(Step::Logged).expect("logged");
        b.log(Step::Done).expect("logged");
        assert_eq!(closed(&home, id).len(), 2);
        assert_eq!(ops(&read(&home, id)).len(), 2);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// While an operation holds the lock, recovery leaves it alone; once its holder is gone it may.
    #[test]
    fn a_running_operation_is_never_taken_over() {
        let home = tmp("lock");
        let id = "h0123456789ab";
        let a = Op::begin(&home, id).expect("op");
        assert!(Op::resume(&home, id, a.id()).expect("lock").is_none(), "held");
        let op = a.id().to_string();
        drop(a);
        // A lock dropped here can look held for a moment: a process another test spawns holds a copy
        // of every descriptor from its fork until its exec closes them. It frees itself.
        let soon = |free: &dyn Fn() -> bool| {
            (0..200).any(|_| {
                let f = free();
                if !f {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                f
            })
        };
        assert!(soon(&|| Op::resume(&home, id, &op).expect("lock").is_some()), "free once its holder ended");
        let run = background(&home).expect("lock");
        assert!(run.is_some() && background(&home).expect("lock").is_none(), "one background run at a time");
        drop(run);
        assert!(soon(&|| background(&home).expect("lock").is_some()));
        let _ = std::fs::remove_dir_all(&home);
    }
}
