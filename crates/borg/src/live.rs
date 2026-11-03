//! The session lock, and the file locks every store append takes (`locked`).
//!
//! Live mode, which checked the work during the turn from here, was removed in 0.7.6
//! (docs/delivery.md, "Live mode (removed in 0.7.6)"); the module kept its name and its lock file's.
//!
//! The session lock: `<S>.live.lock` beside the session's state file, `<pid> <started_ms>`, held by a
//! stop for its whole check and by the session-end judge (hook.rs `judge_session`), so two judge calls
//! never judge the same deliveries at once. Linked into place whole; stale when its process is gone or
//! it is older than `stale_ms`.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Guard: what a lock may outlive its holder's longest model call by (its own reading and writing),
/// before it is taken for a crashed holder's.
pub const STALE_SLACK_MS: i64 = 30_000;

/// A lock older than this belongs to a holder that cannot still be waiting for its model (or to a pid
/// that was reused): derived, twice the predict timeout plus [`STALE_SLACK_MS`].
pub fn stale_ms() -> i64 {
    i64::try_from(crate::predict::timeout().as_millis()).unwrap_or(i64::MAX).saturating_mul(2).saturating_add(STALE_SLACK_MS)
}

/// The session lock's file beside the state file `ctx`. Its name is live mode's: a lock an older
/// binary holds across an upgrade still counts.
pub fn lock_file(ctx: &Path) -> PathBuf {
    ctx.with_extension("live.lock")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lock {
    Free,
    Held,
    /// The file is there, but its holder is gone (or far too old): it may be taken over.
    Stale,
}

/// Whether process `pid` still exists (signal 0 checks without sending).
pub fn alive(pid: i32) -> bool {
    // SAFETY: kill with signal 0 only checks that the process exists and may be signalled.
    pid > 0 && (unsafe { libc::kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

/// The state of a lock whose file reads `text` at `now`. A lock is written whole (`acquire`), so one
/// that does not parse is someone's half-written file: held, until its file is older than `stale_after`.
fn state_of(text: &str, file_ms: i64, now: i64, stale_after: i64, alive: impl Fn(i32) -> bool) -> Lock {
    let mut it = text.split_whitespace();
    match (it.next().and_then(|p| p.parse::<i32>().ok()), it.next().and_then(|m| m.parse::<i64>().ok())) {
        (Some(pid), Some(ms)) if alive(pid) && now - ms <= stale_after => Lock::Held,
        (Some(_), Some(_)) => Lock::Stale,
        _ if now - file_ms > stale_after => Lock::Stale,
        _ => Lock::Held,
    }
}

fn modified_ms(path: &Path) -> i64 {
    std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64)
}

/// The lock's state at `now`; `alive` tells whether a pid still runs.
pub fn lock_state(ctx: &Path, now: i64, stale_after: i64, alive: impl Fn(i32) -> bool) -> Lock {
    let path = lock_file(ctx);
    let Ok(t) = std::fs::read_to_string(&path) else { return Lock::Free };
    state_of(&t, modified_ms(&path), now, stale_after, alive)
}

/// A held lock; dropping it removes the file, if it is still this one's.
#[derive(Debug)]
pub struct Guard {
    path: PathBuf,
    text: String,
}

impl Guard {
    /// Writes the lock anew at `now`, if it is still this one's: a holder whose work outlasts one model
    /// call (the session-end judge's batches, hook.rs `judge_session`) renews it before each call, so it
    /// never reads stale while its holder still works. Whole, by a rename: no one reads it half-written.
    /// `Lost` when another holds it now (or it is gone), and nothing is written; `Unwritten` when it is
    /// still this one's but could not be written (it stays as it was, and may read stale later).
    pub fn renew(&mut self, now: i64) -> Renewal {
        match std::fs::read_to_string(&self.path) {
            Ok(t) if t == self.text => {}
            Ok(_) => return Renewal::Lost,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Renewal::Lost,
            Err(_) => return Renewal::Unwritten,
        }
        let pid = std::process::id();
        let text = format!("{pid} {now}\n");
        let tmp = self.path.with_extension(format!("lock.{pid}.renew"));
        if std::fs::write(&tmp, &text).is_err() || std::fs::rename(&tmp, &self.path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return Renewal::Unwritten;
        }
        self.text = text;
        Renewal::Renewed
    }
}

/// What renewing a held lock did (`Guard::renew`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renewal {
    /// Written anew: held for another `stale_ms`.
    Renewed,
    /// Another process holds it now (it read stale and was taken over), or it is gone.
    Lost,
    /// Still this one's, but not written (an I/O failure): held as before.
    Unwritten,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if std::fs::read_to_string(&self.path).is_ok_and(|t| t == self.text) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Takes the session's lock for this process, or None when another holds it. The lock is written
/// to a file of its own first and hard-linked into place, which fails when a lock is there: no one ever
/// reads a half-written lock. A stale lock is moved aside (a rename: only one process gets it) and taken
/// once more; when what was moved aside is no longer the stale lock seen, someone took it over first,
/// and it goes back.
pub fn acquire(ctx: &Path, now: i64) -> Option<Guard> {
    let path = lock_file(ctx);
    let pid = std::process::id();
    let text = format!("{pid} {now}\n");
    let tmp = path.with_extension(format!("lock.{pid}"));
    std::fs::write(&tmp, &text).ok()?;
    let mut got = None;
    for _ in 0..2 {
        if std::fs::hard_link(&tmp, &path).is_ok() {
            got = Some(Guard { path: path.clone(), text: text.clone() });
            break;
        }
        let Ok(seen) = std::fs::read_to_string(&path) else { continue };
        if state_of(&seen, modified_ms(&path), now, stale_ms(), alive) != Lock::Stale {
            break;
        }
        let aside = path.with_extension(format!("lock.stale.{pid}"));
        if std::fs::rename(&path, &aside).is_err() {
            continue;
        }
        let moved = std::fs::read_to_string(&aside).unwrap_or_default();
        if moved != seen {
            let _ = std::fs::hard_link(&aside, &path);
            let _ = std::fs::remove_file(&aside);
            break;
        }
        let _ = std::fs::remove_file(&aside);
    }
    let _ = std::fs::remove_file(&tmp);
    got
}

/// Runs `f` while holding an exclusive lock on `<file>.lock` (kept, never removed), so appends to `file`
/// and taking it whole (a rename) never interleave: an append never lands in a file already taken.
/// When the lock cannot be taken `f` runs unlocked: for a plain append no worse than without it.
pub fn locked<T>(file: &Path, f: impl FnOnce() -> T) -> T {
    let held = lock_of(file).ok();
    let out = f();
    if let Some(l) = held {
        let _ = l.unlock();
    }
    out
}

/// The lock of `file` (`<file>.lock`), held until the returned file is dropped; None when it cannot be
/// taken (`locked` runs unlocked then too). For holding several at once (moves.rs `with_locks`).
pub fn hold(file: &Path) -> Option<std::fs::File> {
    lock_of(file).ok()
}

/// `<file>.lock`, opened and locked exclusively.
fn lock_of(file: &Path) -> std::io::Result<std::fs::File> {
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(file.with_extension(format!("{}.lock", file.extension().and_then(|e| e.to_str()).unwrap_or_default())))?;
    lock.lock()?;
    Ok(lock)
}

/// Method: how often a holder waiting for the lock (hook.rs `judge_session`) looks at it.
const WAIT_POLL: Duration = Duration::from_millis(200);

/// Waits up to `max_ms` for the lock's holder to finish; the lock's state after.
pub fn wait_free(ctx: &Path, max_ms: u64) -> Lock {
    let until = std::time::Instant::now() + std::time::Duration::from_millis(max_ms);
    loop {
        let l = lock_state(ctx, crate::learn::now_ms(), stale_ms(), alive);
        if l != Lock::Held || std::time::Instant::now() >= until {
            return l;
        }
        std::thread::sleep(WAIT_POLL);
    }
}

/// Guard: what the stop's model call leaves of the budget for the rest of the hook (its records and its
/// answer), so the answer is out before the harness kills the hook.
pub const AFTER_CALL: Duration = Duration::from_secs(3);

/// The stop's model call's limit, `spent` into the hook: what the harness's budget leaves but
/// [`AFTER_CALL`] for the rest of the hook, never over the model's own limit. omp's 28 s gives at most
/// 25 s.
pub fn call_limit(budget: Option<Duration>, spent: Duration) -> Duration {
    let Some(b) = budget else { return crate::predict::timeout() };
    crate::predict::timeout().min(b.saturating_sub(spent).saturating_sub(AFTER_CALL))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-live-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("s1.ctx")
    }

    #[test]
    fn the_lock_is_held_while_its_process_lives_and_stale_when_gone_or_too_old() {
        let ctx = tmp("lock");
        assert_eq!(lock_state(&ctx, 1000, 120_000, |_| true), Lock::Free);
        let g = acquire(&ctx, 1000).expect("free lock taken");
        assert!(acquire(&ctx, 1001).is_none(), "a second holder does not get it");
        assert_eq!(lock_state(&ctx, 2000, 120_000, |_| true), Lock::Held);
        assert_eq!(lock_state(&ctx, 2000, 120_000, |_| false), Lock::Stale, "its process is gone");
        assert_eq!(lock_state(&ctx, 200_000, 120_000, |_| true), Lock::Stale, "older than twice the timeout");
        drop(g);
        assert_eq!(lock_state(&ctx, 2000, 120_000, |_| true), Lock::Free, "the guard removes it");
        // A lock left by a process that is gone is taken over.
        std::fs::write(lock_file(&ctx), format!("{} 1000\n", i32::MAX)).unwrap();
        assert!(!alive(i32::MAX));
        let g = acquire(&ctx, 2000).expect("stale lock taken over");
        // A guard whose lock was taken over leaves the new holder's file alone.
        std::fs::write(lock_file(&ctx), "1 2000\n").unwrap();
        drop(g);
        assert!(lock_file(&ctx).exists(), "not this guard's lock any more");
    }

    /// A holder that works past one call renews its lock, so it stays held; a guard whose lock was taken
    /// over renews nothing and leaves the new holder's file alone.
    #[test]
    fn a_renewed_lock_stays_held_and_a_lost_one_is_left_alone() {
        let ctx = tmp("renew");
        let mut g = acquire(&ctx, 1000).expect("free lock taken");
        assert_eq!(lock_state(&ctx, 200_000, 120_000, |_| true), Lock::Stale, "unrenewed: stale after the limit");
        assert_eq!(g.renew(150_000), Renewal::Renewed);
        assert_eq!(lock_state(&ctx, 200_000, 120_000, |_| true), Lock::Held, "renewed: held");
        assert!(acquire(&ctx, 200_000).is_none());
        // Not written (here: its temp file's place is taken), but still this guard's: not lost.
        let tmp = lock_file(&ctx).with_extension(format!("lock.{}.renew", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(g.renew(160_000), Renewal::Unwritten, "an I/O failure is no takeover");
        std::fs::remove_dir_all(&tmp).unwrap();
        assert_eq!(g.renew(170_000), Renewal::Renewed, "still this guard's");
        std::fs::write(lock_file(&ctx), "1 2000\n").unwrap();
        assert_eq!(g.renew(210_000), Renewal::Lost, "taken over: not renewed");
        assert_eq!(std::fs::read_to_string(lock_file(&ctx)).unwrap(), "1 2000\n");
        drop(g);
        assert!(lock_file(&ctx).exists(), "not this guard's lock any more");
    }

    #[test]
    fn a_half_written_lock_is_held_until_its_file_is_old() {
        assert_eq!(state_of("", 1000, 2000, 120_000, |_| false), Lock::Held, "being written: never taken over");
        assert_eq!(state_of("12", 1000, 2000, 120_000, |_| false), Lock::Held);
        assert_eq!(state_of("", 1000, 200_000, 120_000, |_| false), Lock::Stale, "left empty long ago");
        assert_eq!(state_of("7 1000", 0, 2000, 120_000, |_| true), Lock::Held);
        let ctx = tmp("half");
        std::fs::write(lock_file(&ctx), "").unwrap();
        assert!(acquire(&ctx, crate::learn::now_ms()).is_none(), "an empty, fresh lock is someone's");
    }

    #[test]
    fn the_stop_fits_its_model_call_into_the_harness_budget() {
        let s = Duration::from_secs;
        assert_eq!(call_limit(None, s(30)), crate::predict::timeout(), "Claude Code: the model's own limit");
        assert_eq!(call_limit(Some(s(90)), s(15)), crate::predict::timeout().min(s(72)), "opencode");
        assert_eq!(call_limit(Some(s(28)), s(0)), crate::predict::timeout().min(s(25)), "omp");
        assert_eq!(call_limit(Some(s(28)), s(11)), crate::predict::timeout().min(s(14)), "time spent before the call comes off it");
        assert_eq!(call_limit(Some(s(28)), s(27)), Duration::ZERO);
    }

    #[test]
    fn a_lock_that_cannot_be_taken_still_runs_f_unlocked() {
        // The lock file cannot be opened (a directory in its place): `locked` runs `f` unlocked.
        let d = tmp("try-locked");
        let file = d.join("tasks.jsonl");
        std::fs::create_dir_all(d.join("tasks.jsonl.lock")).unwrap();
        assert!(locked(&file, || true));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn appends_under_the_queue_lock_never_land_in_a_taken_file() {
        let ctx = tmp("queue");
        let q = ctx.with_extension("queued");
        let writers: Vec<_> = (0..8)
            .map(|i| {
                let q = q.clone();
                std::thread::spawn(move || {
                    for j in 0..50 {
                        locked(&q, || std::fs::OpenOptions::new().create(true).append(true).open(&q).and_then(|mut f| writeln!(f, "{i}-{j}")).unwrap());
                    }
                })
            })
            .collect();
        let mut got = 0;
        let taken = q.with_extension("queued.t");
        while writers.iter().any(|w| !w.is_finished()) {
            if locked(&q, || std::fs::rename(&q, &taken).is_ok()) {
                got += std::fs::read_to_string(&taken).unwrap().lines().count();
            }
        }
        for w in writers {
            w.join().unwrap();
        }
        if std::fs::rename(&q, &taken).is_ok() {
            got += std::fs::read_to_string(&taken).unwrap().lines().count();
        }
        assert_eq!(got, 400, "every line taken once");
    }
}
