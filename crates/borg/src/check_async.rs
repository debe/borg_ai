//! The turn-end check, detached (0.7.7): the stop never waits for the model.
//!
//! With `BORG_ASYNC` on (the default), a stop runs the free gate, writes the turn to a job file and
//! starts `borg check --job <file>` in its own session, stdio null, and returns at once (hook.rs
//! `stop_detached`): no block, no session lock, no model call in the hook. The job holds the turn as the
//! stop saw it (`Job`): the state file's length then, the drone's message, where it worked. The check's
//! decision is made on channel live, and its asks reach the drone with the next prompt or tool call
//! (hook.rs `queued`, `ASK_NEXT`), as questions about the turn they judged. `BORG_ASYNC=off` keeps the
//! stop's own check and its block.
//!
//! One check runs per session (`<s>.checking.lock`, an flock): a job that finds it held waits as
//! `<s>.job.next`, replacing (superseding) an older one there, which never runs; the running check
//! takes the waiting job when it is done. The model is called without any lock; only the write of the
//! answer holds the session lock (live.rs), and a session that ended meanwhile gets nothing queued
//! (hook.rs `run_job`, `settle`). What a check did is in `<s>.checked` (`Checked`), overwritten whole
//! by each; `<s>.checks` counts the session's checks that called the model.
//!
//! Leftovers: the job a process claimed (`<s>.job.<check>.<pid>`) goes when it exits; a later stop
//! removes the job files of a process that is gone or older than a call may take (`sweep`).

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{hook, learn, live};

/// The stats mode of a detached check's events (stats.rs `Event::mode`).
pub const MODE: &str = "async";

/// Protocol: why a job never called the model (`not_run`, the `why` of stats.rs `CHECK_DROPPED`): a
/// newer stop's job took its place, the session ended before the call, its check could not be started,
/// or its job could not be placed or read where the running check takes it. borg-web's feed matches them.
pub const WHY_SUPERSEDED: &str = "superseded";
pub const WHY_GONE: &str = "gone";
pub const WHY_UNSTARTED: &str = "unstarted";
pub const WHY_UNPLACED: &str = "unplaced";
pub const WHYS: [&str; 4] = [WHY_SUPERSEDED, WHY_GONE, WHY_UNSTARTED, WHY_UNPLACED];

/// Protocol: the version of a job file's format (`Job::v`).
pub const JOB_VERSION: u32 = 1;

/// Whether stops check detached: `BORG_ASYNC` anything but `off` (the default is on). Evaluation switch:
/// `off` keeps the stop's own check, its lock and its block, as before 0.7.7.
#[must_use]
pub fn on() -> bool {
    !std::env::var("BORG_ASYNC").is_ok_and(|v| v.trim().eq_ignore_ascii_case("off"))
}

/// One turn to check, as its stop saw it (hook.rs `stop_detached`): `<state>/<s>.job.<check>`, written
/// whole (a rename), read by the check it started. A file, never arguments: it holds the drone's words.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub v: u32,
    pub session: String,
    /// The check's id, from the stop's start and pid (select.rs `check_id`).
    pub check: String,
    pub stop_ms: i64,
    pub harness: String,
    pub cwd: String,
    /// The state file's length at the stop: the check reads the turn up to here, never the next
    /// prompt's lines.
    pub offset: usize,
    /// The hook's `last_assistant_message` (the work the gate and the pool read with the state file).
    pub message: String,
    /// The drone's whole last message (hook.rs `full_proposal`): the judge's message, and the turn the
    /// asks name when they are delivered.
    pub proposal: String,
    /// BORG_STOP_GATE=edits: whether the turn changed anything.
    pub changed: bool,
    pub repo: String,
    /// Where the session worked at the stop, and the message (hook.rs `where_now`): the pool's scopes.
    #[serde(default)]
    pub scope: String,
    /// The .edits file's length at the stop: the deliveries the check judges and the work since them
    /// are read up to here (hook.rs `pending`), never the next turn's. None from a job without it: all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edits: Option<usize>,
}

/// What a detached check did, `<state>/<s>.checked`, overwritten whole by each (`finish`). No prompt
/// data: ids, times, whether the call failed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Checked {
    pub check: String,
    pub stop_ms: i64,
    pub done_ms: i64,
    /// The lessons it queued as asks, and as free hand-overs.
    #[serde(default)]
    pub asks: Vec<String>,
    #[serde(default)]
    pub free: Vec<String>,
    /// The model gave no answer (fail open: nothing asked, the pending lessons stay pending).
    #[serde(default)]
    pub failed: bool,
    /// The model call's duration.
    #[serde(default)]
    pub dur_ms: u64,
    /// Waiting asks of earlier checks it took off the queue (hook.rs `supersede`).
    #[serde(default)]
    pub superseded: Vec<String>,
}

impl Checked {
    /// A check of `job` that did nothing yet.
    pub(crate) fn of(job: &Job) -> Checked {
        Checked { check: job.check.clone(), stop_ms: job.stop_ms, ..Default::default() }
    }
}

/// `<state>/<s>.<ext>`, beside the session's state file.
fn file(session: &str, ext: &str) -> PathBuf {
    hook::state_file(session).with_extension(ext)
}

/// The waiting job (`<s>.job.next`).
fn next_file(session: &str) -> PathBuf {
    file(session, "job.next")
}

/// The job file of `job`, written whole (a temporary file, then a rename); its path.
pub(crate) fn write_job(job: &Job) -> std::io::Result<PathBuf> {
    let path = file(&job.session, &format!("job.{}", job.check));
    let tmp = file(&job.session, &format!("job.{}.tmp", job.check));
    let text = serde_json::to_string(job).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &path)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(path)
}

/// `borg check --job <job>`, detached: its own session (setsid), stdio null (an adapter that waits for
/// the hook's stdout to close is never held), not waited for. The environment goes with it: the routed
/// store and state (`BORG_HOME`, `BORG_STATE`) and the harness (`BORG_HARNESS`, hook.rs `main`). A job
/// that cannot start is removed; false then (the stop counts it, `not_run`).
pub(crate) fn spawn(job: &Path) -> bool {
    use std::os::unix::process::CommandExt;
    let mut c = std::process::Command::new(crate::reexec::exe_path());
    c.arg("check").arg("--job").arg(job).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    // SAFETY: setsid is async-signal-safe and touches no memory of the parent.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    if c.spawn().is_err() {
        let _ = std::fs::remove_file(job);
        return false;
    }
    true
}

/// `borg check --job <file>`: started by a stop, never by hand.
pub fn main(job: &str) -> i32 {
    run(Path::new(job));
    0
}

/// A job this process claimed: its file, removed when it is done, whatever way it ends.
struct Claimed {
    path: PathBuf,
    job: Job,
}

impl Drop for Claimed {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn read_job(path: &Path) -> Option<Job> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// `path` taken by this process (a rename to `<s>.job.<check>.<pid>`); None when another took it or it
/// is gone.
fn claim(path: &Path) -> Option<Claimed> {
    let job = read_job(path)?;
    let to = file(&job.session, &format!("job.{}.{}", job.check, std::process::id()));
    std::fs::rename(path, &to).ok()?;
    Some(Claimed { path: to, job })
}

/// What `claim_next` found waiting.
enum Next {
    /// The waiting job, claimed.
    Job(Box<Claimed>),
    /// None waits (none placed, or one that could not be claimed, removed and counted).
    Empty,
    /// One waits that can be neither claimed nor removed (a state directory gone read-only): the check
    /// stops looking rather than spin on it; a later stop's `sweep` or job takes its place.
    Stuck,
}

/// The waiting job, claimed. One that cannot be claimed (unreadable, a job format this binary does not
/// read, a rename that fails) is removed and counted (`not_run`, `unplaced`): left there, every check of
/// the session would find it waiting and never take it.
fn claim_next(session: &str) -> Next {
    let next = next_file(session);
    live::locked(&file(session, "checking.next"), || {
        if !next.exists() {
            return Next::Empty;
        }
        if let Some(c) = claim(&next) {
            return Next::Job(Box::new(c));
        }
        let job = read_job(&next).unwrap_or_else(|| Job { session: session.to_string(), ..Job::default() });
        match std::fs::remove_file(&next) {
            Ok(()) => {
                not_run(&job, WHY_UNPLACED);
                Next::Empty
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Next::Empty,
            Err(_) => Next::Stuck,
        }
    })
}

/// `c` waits for the running check (`<s>.job.next`): of it and a job waiting already, the newer stop's
/// waits and the older is superseded (counted, never run).
fn wait_next(c: Claimed) {
    let session = c.job.session.clone();
    let next = next_file(&session);
    live::locked(&file(&session, "checking.next"), || match read_job(&next) {
        Some(waiting) if waiting.stop_ms > c.job.stop_ms => not_run(&c.job, WHY_SUPERSEDED),
        waiting => match std::fs::rename(&c.path, &next) {
            Ok(()) => {
                if let Some(w) = waiting {
                    not_run(&w, WHY_SUPERSEDED);
                }
            }
            // Its file goes with `c`: counted, never lost unseen.
            Err(_) => not_run(&c.job, WHY_UNPLACED),
        },
    });
}

/// The session's check lock (`<s>.checking.lock`) as `hold` finds it.
enum Hold {
    /// Taken, held until dropped.
    Mine(std::fs::File),
    /// Another check holds it.
    Busy,
    /// It cannot be opened or locked at all (a file system without flock, a full or read-only state
    /// directory): the check runs without it, as the session lock's write does (hook.rs `run_job`),
    /// rather than leave every job waiting for a check that never comes.
    Broken,
}

fn hold(session: &str) -> Hold {
    let Ok(f) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(file(session, "checking.lock")) else { return Hold::Broken };
    match f.try_lock() {
        Ok(()) => Hold::Mine(f),
        Err(std::fs::TryLockError::WouldBlock) => Hold::Busy,
        Err(std::fs::TryLockError::Error(_)) => Hold::Broken,
    }
}

/// The job at `path`, and every job that waits after it, one at a time per session. A job that finds
/// another check running waits for it (`wait_next`); the running check takes it when done. A check
/// looks for a waiting job once more after it let the lock go, and one that placed its job looks for
/// the lock once more, so a job never waits with no check left to take it. A job whose stop is older
/// than one this process checked already (one left waiting by a check that died) is superseded, never
/// run: a check never judges older work after newer.
fn run(path: &Path) {
    let Some(mine) = claim(path) else { return };
    let session = mine.job.session.clone();
    let mut mine = Some(mine);
    let mut last = i64::MIN;
    loop {
        let lock = match hold(&session) {
            Hold::Mine(f) => Some(f),
            Hold::Broken => None,
            Hold::Busy => match mine.take() {
                Some(c) => {
                    wait_next(c);
                    continue;
                }
                None => return,
            },
        };
        let mut job = match mine.take() {
            Some(c) => Next::Job(Box::new(c)),
            None => claim_next(&session),
        };
        while let Next::Job(c) = job {
            if c.job.stop_ms < last {
                not_run(&c.job, WHY_SUPERSEDED);
            } else {
                last = c.job.stop_ms;
                hook::run_job(&c.job);
            }
            // The next one claimed before this one's file goes: a claimed job of this live process
            // exists throughout, so a stop's `sweep` never takes the waiting job for one no check runs to
            // take.
            job = claim_next(&session);
            drop(c);
        }
        drop(lock);
        if matches!(job, Next::Stuck) || !next_file(&session).exists() {
            return;
        }
    }
}

/// A job that never called the model, counted apart from the model calls (stats.rs `CHECK_DROPPED`) with
/// why (`WHYS`).
pub(crate) fn not_run(job: &Job, why: &str) {
    crate::stats::count(&learn::home(), crate::stats::CHECK_DROPPED, &job.session, |e| {
        e.why = why.into();
        e.mode = MODE.into();
        e.repo.clone_from(&job.repo);
    });
}

/// The session's checks that called the model so far (`<s>.checks`): a check's index among them, read
/// before its own is counted (`finish`).
#[must_use]
pub fn index(session: &str) -> u64 {
    std::fs::read_to_string(file(session, "checks")).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0)
}

/// A detached check's end, under the session lock (hook.rs `run_job`): one more in `<s>.checks` when it
/// called the model (`called`), then `<s>.checked` whole (a temporary file, then a rename).
pub(crate) fn finish(session: &str, out: &mut Checked, called: bool) {
    if called {
        let _ = std::fs::write(file(session, "checks"), format!("{}\n", index(session) + 1));
    }
    out.done_ms = learn::now_ms();
    let (path, tmp) = (file(session, "checked"), file(session, &format!("checked.{}", std::process::id())));
    let Ok(text) = serde_json::to_string(out) else { return };
    if std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &path)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// How long a detached check's model call may take: `BORG_PREDICT_TIMEOUT_S` when set, else the stop's
/// limit (predict.rs `timeout`) or twice the 95th percentile of the call's durations, the larger: the
/// latest `listen::GAPS_KEPT` answered checks of `model` in the store's outcomes (`dur_ms` of each
/// `graded` record that did not fail). Derived as the session lock's staleness is (live.rs `stale_ms`):
/// no hook waits for it, so it bounds a hung call, never a slow model's answer.
#[must_use]
pub fn call_limit(model: &str) -> Duration {
    call_limit_in(&learn::home(), model)
}

/// `call_limit` from the outcomes of the store `home` (`borg status` shows each collective's).
#[must_use]
pub fn call_limit_in(home: &Path, model: &str) -> Duration {
    let floor = crate::predict::timeout();
    if std::env::var_os("BORG_PREDICT_TIMEOUT_S").is_some() {
        return floor;
    }
    let p95 = p95(durations(&home.join("outcomes.jsonl"), model));
    floor.max(Duration::from_millis(p95.saturating_mul(2)))
}

/// The `dur_ms` of the latest `listen::GAPS_KEPT` answered `graded` records of `model` in `log`, oldest
/// first; read line by line.
fn durations(log: &Path, model: &str) -> Vec<u64> {
    let Ok(f) = std::fs::File::open(log) else { return Vec::new() };
    let mut kept = std::collections::VecDeque::with_capacity(crate::listen::GAPS_KEPT);
    for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
        // Most lines are no check's: parsed only when they may be.
        if !line.contains(crate::outcome::GRADED) {
            continue;
        }
        let Ok(o) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if o["kind"] != crate::outcome::GRADED || o["failed"] == true || o["model"].as_str().unwrap_or_default() != model {
            continue;
        }
        let Some(ms) = o["dur_ms"].as_u64().filter(|ms| *ms > 0) else { continue };
        if kept.len() == crate::listen::GAPS_KEPT {
            kept.pop_front();
        }
        kept.push_back(ms);
    }
    kept.into()
}

/// The 95th percentile of `v` (nearest rank); 0 for none.
fn p95(mut v: Vec<u64>) -> u64 {
    v.sort_unstable();
    let rank = (v.len() * 95).div_ceil(100);
    v.get(rank.saturating_sub(1)).copied().unwrap_or(0)
}

/// This process's peak resident memory in KiB (`getrusage`), 0 when unknown: a detached check's, on
/// its `check` event.
#[must_use]
pub fn rss_kb() -> u64 {
    // SAFETY: getrusage only writes the struct it is given, zeroed here.
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) } != 0 {
        return 0;
    }
    kib(u64::try_from(u.ru_maxrss).unwrap_or(0))
}

/// `ru_maxrss` in KiB: Linux counts it in KiB, macOS in bytes.
const fn kib(maxrss: u64) -> u64 {
    if cfg!(target_os = "macos") { maxrss / 1024 } else { maxrss }
}

/// Whether a check of `session` runs: a job claimed (`<s>.job.<check>.<pid>`) by a live process. A
/// directory listing, no lock probe (one could turn a starting check away). A wake reserves no ask
/// meanwhile (hook.rs `reserve_asks`): the running check may grade it again against newer work.
#[must_use]
pub fn running(session: &str) -> bool {
    let ctx = hook::state_file(session);
    let (Some(dir), Some(stem)) = (ctx.parent(), ctx.file_stem().and_then(|s| s.to_str())) else { return false };
    let jobs = format!("{stem}.job.");
    let Ok(rd) = std::fs::read_dir(dir) else { return false };
    rd.flatten().any(|e| e.file_name().to_str().and_then(|n| claimed_pid(n, &jobs)).is_some_and(live::alive))
}

/// The pid of a claimed job's name (`<jobs><check>.<pid>`, `jobs` the session's `<s>.job.`); None for
/// any other.
fn claimed_pid(name: &str, jobs: &str) -> Option<i32> {
    name.strip_prefix(jobs)?.split_once('.')?.1.parse().ok()
}

/// Removes `session`'s files a check left behind: a claimed job (`<s>.job.<check>.<pid>`) or a result
/// being written (`<s>.checked.<pid>`) whose process is gone; a job no check claimed, and the waiting one
/// (`<s>.job.next`) while no check runs to take it (no claimed job of a live process), once older than a
/// check may take (`call_limit` plus the session lock's slack, live.rs `STALE_SLACK_MS`). A live
/// process's files stay whatever their age. At a stop (hook.rs `stop_detached`) and for an idle session
/// (learn.rs). Takes no lock: a probe of `<s>.checking.lock` could turn a starting check away.
pub fn sweep(session: &str) {
    let ctx = hook::state_file(session);
    let (Some(dir), Some(stem)) = (ctx.parent(), ctx.file_stem().and_then(|s| s.to_str())) else { return };
    let (jobs, results) = (format!("{stem}.job."), format!("{stem}.checked."));
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mine: Vec<std::fs::DirEntry> =
        rd.flatten().filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with(&jobs) || n.starts_with(&results))).collect();
    // The pid of a claimed job's or a result's name, None for any other.
    let pid_of = |name: &str| -> Option<i32> { claimed_pid(name, &jobs).or_else(|| name.strip_prefix(&results)?.parse().ok()) };
    let names: Vec<String> = mine.iter().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    let running = names.iter().filter(|n| n.starts_with(&jobs)).filter_map(|n| pid_of(n)).any(live::alive);
    // Only a file older than the least a check may take needs the measured limit.
    let least = ms(crate::predict::timeout()).saturating_add(live::STALE_SLACK_MS);
    let mut limit: Option<i64> = None;
    let now = learn::now_ms();
    for (e, name) in mine.iter().zip(&names) {
        let gone = match pid_of(name) {
            Some(pid) => !live::alive(pid),
            None if name.starts_with(&results) => false,
            None => {
                let age = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| now - ms(d));
                let old = age > least
                    && age
                        > *limit.get_or_insert_with(|| {
                            ms(call_limit(&std::env::var("BORG_PREDICT_MODEL").unwrap_or_default())).saturating_add(live::STALE_SLACK_MS)
                        });
                old && !(running && name.ends_with(".job.next"))
            }
        };
        if gone {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_95th_percentile_is_the_nearest_rank() {
        assert_eq!(p95(vec![]), 0);
        assert_eq!(p95(vec![7]), 7);
        assert_eq!(p95((1..=100).rev().collect()), 95);
        assert_eq!(p95(vec![1, 2, 3, 40]), 40, "4 durations: the 4th");
    }

    #[test]
    fn the_limit_reads_the_latest_answered_checks_of_the_same_model() {
        let d = std::env::temp_dir().join(format!("borg-check-async-limit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let log = d.join("outcomes.jsonl");
        let mut text = String::new();
        for i in 0..100u64 {
            text.push_str(&format!("{{\"kind\":\"graded\",\"session\":\"s\",\"ms\":{i},\"model\":\"m\",\"dur_ms\":{}}}\n", 1000 + i));
        }
        text.push_str("{\"kind\":\"graded\",\"session\":\"s\",\"ms\":200,\"model\":\"m\",\"dur_ms\":900000,\"failed\":true}\n");
        text.push_str("{\"kind\":\"graded\",\"session\":\"s\",\"ms\":201,\"model\":\"other\",\"dur_ms\":800000}\n");
        text.push_str("{\"kind\":\"asked\",\"session\":\"s\",\"ms\":202,\"model\":\"m\",\"dur_ms\":700000}\n");
        std::fs::write(&log, text).unwrap();
        let got = durations(&log, "m");
        assert_eq!(got.len(), crate::listen::GAPS_KEPT, "the latest only");
        assert_eq!((got[0], *got.last().unwrap()), (1036, 1099), "failed calls, other models and other kinds left out");
        let _ = std::fs::remove_dir_all(d);
    }
}
