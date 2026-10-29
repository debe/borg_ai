//! Delivery modes: when the conscience judges the work (docs/delivery.md).
//!
//! - **hook**: early delivery and queued lessons are free word matching; one graded model
//!   call runs at the turn end (main.rs `stop`), and only a grade 3 blocks the stop.
//! - **live** (the default, chosen by the live-vs-hook eval): the same graded call also runs *during* the turn. After a state-changing tool call
//!   (prefilter.rs `changes`), the PostToolUse hook spawns a detached `borg check` and returns at once;
//!   the check judges the work so far and leaves its verdict in the `.queued` file, which the next
//!   prompt or tool call delivers (main.rs `queued`). The stop still runs as the backstop, and skips its
//!   own model call when a live check this turn already covered every change (`covered`).
//!
//! The mode is chosen per collective (`"mode"` in collectives.json, collective.rs) and `BORG_MODE`
//! overrides it; unset or anything else is `DEFAULT`. Every harness runs it: Claude Code and omp take the
//! next tool call's context as PostToolUse `additionalContext`, the opencode adapter adds it to the tool
//! output (1.x) or steers it into the session (2.x). Without a Claude transcript the work so far is the
//! turn's tool calls (main.rs `turn_file`).
//!
//! Three small files per session beside its state file, no daemon:
//! - `<S>.live.n`: one line per state-changing tool call (the change counter);
//! - `<S>.live.mark`: `<covered> <started_ms> <turn>`, the counter value the last completed check covered,
//!   when the last check started (the debounce runs from there) and the turn it started in (main.rs
//!   `turn_id`: a check of an earlier turn never covers this one);
//! - `<S>.live.lock`: `<pid> <started_ms>` while a check (or a live stop) runs, linked into place whole
//!   so two quick tool calls never run two checks; stale when its process is gone or it is older than
//!   `stale_ms`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Hook,
    Live,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Hook => "hook",
            Mode::Live => "live",
        }
    }
}

/// The mode in force and where it came from (`BORG_MODE`, `config` or `default`), with a warning
/// when a value was not a mode (`borg doctor` shows it; the mode is then `DEFAULT`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub mode: Mode,
    pub source: &'static str,
    pub warning: Option<String>,
}

/// The mode when neither BORG_MODE nor the collective sets one: live, which passed the eval's fixed
/// rule against hook (docs/delivery.md, "Why live is the default"). `"mode": "hook"` or `BORG_MODE=hook` switches back.
pub const DEFAULT: Mode = Mode::Live;

/// `env` (BORG_MODE) wins over `configured` (the collective's `mode`), else `DEFAULT`. An unknown value
/// is `DEFAULT` plus a warning: a typo must never break the hooks.
pub fn resolve(env: Option<&str>, configured: Option<&str>) -> Resolved {
    let pick = |v: &str, source: &'static str| match v.trim().to_ascii_lowercase().as_str() {
        "hook" => Resolved { mode: Mode::Hook, source, warning: None },
        "live" => Resolved { mode: Mode::Live, source, warning: None },
        _ => Resolved { mode: DEFAULT, source, warning: Some(format!("{source}: mode {v:?} is neither \"hook\" nor \"live\"; using {}", DEFAULT.name())) },
    };
    match (env.filter(|v| !v.trim().is_empty()), configured.filter(|v| !v.trim().is_empty())) {
        (Some(v), _) => pick(v, "BORG_MODE"),
        (None, Some(v)) => pick(v, "config"),
        (None, None) => Resolved { mode: DEFAULT, source: "default", warning: None },
    }
}

/// The mode of collective `name` in `cfg` for this process (BORG_MODE first).
pub fn of(cfg: Option<&crate::collective::Config>, name: &str) -> Resolved {
    let configured = cfg.and_then(|c| c.collectives.iter().find(|e| e.name == name)).and_then(|e| e.mode.as_deref());
    resolve(std::env::var("BORG_MODE").ok().as_deref(), configured)
}

/// The mode of this hook process: the hook exports the resolved mode as `BORG_MODE` (main.rs), so the
/// stop and the spawned check read the same one.
pub fn current() -> Mode {
    resolve(std::env::var("BORG_MODE").ok().as_deref(), None).mode
}

/// Why live mode cannot run here, if it cannot: it needs the consequence model, which word matching
/// alone (`BORG_MATCH=bm25`) and the eval modes `BORG_ONLY` and `BORG_CRITIC` replace.
pub fn unsupported() -> Option<&'static str> {
    if std::env::var("BORG_MATCH").as_deref() == Ok("bm25") {
        Some("BORG_MATCH=bm25")
    } else if std::env::var_os("BORG_ONLY").is_some() {
        Some("BORG_ONLY")
    } else if std::env::var_os("BORG_CRITIC").is_some() {
        Some("BORG_CRITIC")
    } else {
        None
    }
}

/// Baseline: the least time between the starts of two checks of a session when `BORG_LIVE_DEBOUNCE_S`
/// is unset, the value live mode passed the live-vs-hook eval with (docs/delivery.md, "Why live is the
/// default"). It also guards against a check per tool call.
pub const DEBOUNCE_S: i64 = 45;

/// The least time between the starts of two checks of a session: `BORG_LIVE_DEBOUNCE_S`, else
/// [`DEBOUNCE_S`].
pub fn debounce_ms() -> i64 {
    std::env::var("BORG_LIVE_DEBOUNCE_S").ok().and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(DEBOUNCE_S).max(0) * 1000
}

/// Guard: what a lock may outlive its check's longest model call by (the check's own reading and
/// writing), before it is taken for a crashed check's.
const STALE_SLACK_MS: i64 = 30_000;

/// A lock older than this belongs to a check that cannot still be waiting for its model (or to a pid
/// that was reused): derived, twice the predict timeout plus [`STALE_SLACK_MS`].
pub fn stale_ms() -> i64 {
    i64::try_from(crate::predict::timeout().as_millis()).unwrap_or(i64::MAX).saturating_mul(2).saturating_add(STALE_SLACK_MS)
}

fn beside(ctx: &Path, ext: &str) -> PathBuf {
    ctx.with_extension(ext)
}

pub fn counter_file(ctx: &Path) -> PathBuf {
    beside(ctx, "live.n")
}

pub fn mark_file(ctx: &Path) -> PathBuf {
    beside(ctx, "live.mark")
}

pub fn lock_file(ctx: &Path) -> PathBuf {
    beside(ctx, "live.lock")
}

/// State-changing tool calls of the session so far.
pub fn count(ctx: &Path) -> u64 {
    std::fs::read_to_string(counter_file(ctx)).map_or(0, |t| t.lines().count() as u64)
}

/// Counts one state-changing tool call.
pub fn bump(ctx: &Path, tool: &str) {
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(counter_file(ctx)).and_then(|mut f| writeln!(f, "{tool}"));
}

/// What the last check covered, when it started and in which turn (None: a mark without one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub covered: u64,
    pub started_ms: i64,
    pub turn: Option<u64>,
}

pub fn read_mark(ctx: &Path) -> Option<Mark> {
    let t = std::fs::read_to_string(mark_file(ctx)).ok()?;
    let mut it = t.split_whitespace();
    Some(Mark { covered: it.next()?.parse().ok()?, started_ms: it.next()?.parse().ok()?, turn: it.next().and_then(|v| v.parse().ok()) })
}

pub fn write_mark(ctx: &Path, m: Mark) {
    let turn = m.turn.map_or(String::new(), |t| format!(" {t}"));
    let _ = std::fs::write(mark_file(ctx), format!("{} {}{turn}\n", m.covered, m.started_ms));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lock {
    Free,
    Held,
    /// The file is there, but its check is gone (or far too old): it may be taken over.
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

impl Drop for Guard {
    fn drop(&mut self) {
        if std::fs::read_to_string(&self.path).is_ok_and(|t| t == self.text) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Takes the session's lock for this process, or None when another check holds it. The lock is written
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

/// Whether a check should start now (Ok) or why not: not live mode, a check still running, no change
/// since the last completed check, or the last check started less than `debounce` ago.
pub fn decide(mode: Mode, lock: Lock, count: u64, mark: Option<Mark>, now: i64, debounce: i64) -> Result<(), &'static str> {
    if mode != Mode::Live {
        return Err("hook mode");
    }
    if lock == Lock::Held {
        return Err("running");
    }
    if count <= mark.map_or(0, |m| m.covered) {
        return Err("nothing new");
    }
    if mark.is_some_and(|m| now - m.started_ms < debounce) {
        return Err("debounce");
    }
    Ok(())
}

/// The stop's skip in live mode: no check runs, a check that started in this turn (`turn`, main.rs
/// `turn_id`) covered every change, and none came since. A check of an earlier turn, or one that
/// finished after its turn's stop, never covers this turn's message.
pub fn covered(lock: Lock, count: u64, mark: Option<Mark>, turn: u64) -> bool {
    lock != Lock::Held && count > 0 && mark.is_some_and(|m| m.covered == count && m.turn == Some(turn))
}

/// Method: how often a stop waiting for a running check looks at its lock.
const WAIT_POLL: Duration = Duration::from_millis(200);

/// Waits up to `max_ms` for a running check to finish; the lock's state after.
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

/// Guard: the most of the stop's budget its wait for a running check may take (a quarter), so a slow
/// check never starves the stop's own model call, which gets the rest ([`call_limit`]).
pub const WAIT_SHARE: u32 = 4;

/// How long a stop in live mode waits for a running check at most. Derived: the share [`WAIT_SHARE`] of
/// the budget a stop gets where the harness names none (`harness::DEFAULT_STOP_BUDGET`, 60 s: 15 s), so
/// a harness with a longer budget does not keep the engineer waiting longer for a check.
pub const STOP_WAIT: Duration = Duration::from_millis(crate::harness::DEFAULT_STOP_BUDGET.as_millis() as u64 / WAIT_SHARE as u64);

/// Guard: what the stop's model call leaves of the budget for the rest of the hook (its records and its
/// answer), so the answer is out before the harness kills the hook.
pub const AFTER_CALL: Duration = Duration::from_secs(3);

/// The stop's wait for a running check, inside the harness's budget (expert_route.rs `stop_budget`; none:
/// Claude Code's): at most [`STOP_WAIT`], and at most its [`WAIT_SHARE`]. omp's 28 s gives 7 s.
pub fn stop_wait(budget: Option<Duration>) -> Duration {
    budget.map_or(STOP_WAIT, |b| STOP_WAIT.min(b / WAIT_SHARE))
}

/// The stop's model call's limit, `spent` into the hook: what the harness's budget leaves but
/// [`AFTER_CALL`] for the rest of the hook, never over the model's own limit. omp's 28 s gives at most
/// 18 s, after a 7 s wait.
pub fn call_limit(budget: Option<Duration>, spent: Duration) -> Duration {
    let Some(b) = budget else { return crate::predict::timeout() };
    crate::predict::timeout().min(b.saturating_sub(spent).saturating_sub(AFTER_CALL))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-live-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("s1.ctx")
    }

    #[test]
    fn env_beats_config_and_unknown_values_fall_back_to_the_default() {
        assert_eq!(resolve(None, None), Resolved { mode: Mode::Live, source: "default", warning: None });
        assert_eq!(resolve(None, Some("live")).mode, Mode::Live);
        assert_eq!(resolve(Some("hook"), Some("live")), Resolved { mode: Mode::Hook, source: "BORG_MODE", warning: None });
        assert_eq!(resolve(Some(" LIVE "), None).mode, Mode::Live);
        assert_eq!(resolve(Some(""), Some("live")).source, "config", "an empty BORG_MODE is unset");
        let r = resolve(None, Some("eager"));
        assert_eq!(r.mode, Mode::Live);
        assert!(r.warning.unwrap().contains("\"eager\""));
    }

    #[test]
    fn a_check_starts_only_in_live_mode_with_new_changes_no_running_check_and_after_the_debounce() {
        let m = |covered, started_ms| Some(Mark { covered, started_ms, turn: None });
        assert_eq!(decide(Mode::Live, Lock::Free, 1, None, 0, 45_000), Ok(()), "the first change checks at once");
        assert_eq!(decide(Mode::Hook, Lock::Free, 1, None, 0, 45_000), Err("hook mode"));
        assert_eq!(decide(Mode::Live, Lock::Held, 3, m(1, 0), 100_000, 45_000), Err("running"));
        assert_eq!(decide(Mode::Live, Lock::Stale, 3, m(1, 0), 100_000, 45_000), Ok(()), "a stale lock does not block");
        assert_eq!(decide(Mode::Live, Lock::Free, 2, m(2, 0), 100_000, 45_000), Err("nothing new"));
        assert_eq!(decide(Mode::Live, Lock::Free, 0, None, 0, 45_000), Err("nothing new"));
        assert_eq!(decide(Mode::Live, Lock::Free, 3, m(2, 60_000), 100_000, 45_000), Err("debounce"));
        assert_eq!(decide(Mode::Live, Lock::Free, 3, m(2, 55_000), 100_000, 45_000), Ok(()));
    }

    #[test]
    fn the_lock_is_held_while_its_process_lives_and_stale_when_gone_or_too_old() {
        let ctx = tmp("lock");
        assert_eq!(lock_state(&ctx, 1000, 120_000, |_| true), Lock::Free);
        let g = acquire(&ctx, 1000).expect("free lock taken");
        assert!(acquire(&ctx, 1001).is_none(), "a second check does not run");
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
    fn counter_and_mark_round_trip() {
        let ctx = tmp("mark");
        assert_eq!((count(&ctx), read_mark(&ctx)), (0, None));
        bump(&ctx, "Edit");
        bump(&ctx, "Bash");
        assert_eq!(count(&ctx), 2);
        write_mark(&ctx, Mark { covered: 2, started_ms: 77, turn: Some(9) });
        assert_eq!(read_mark(&ctx), Some(Mark { covered: 2, started_ms: 77, turn: Some(9) }));
        std::fs::write(mark_file(&ctx), "2 77\n").unwrap();
        assert_eq!(read_mark(&ctx), Some(Mark { covered: 2, started_ms: 77, turn: None }), "a mark of before turns");
    }

    #[test]
    fn the_stop_skips_only_when_a_check_of_this_turn_covered_every_change() {
        let m = Some(Mark { covered: 3, started_ms: 0, turn: Some(40) });
        assert!(covered(Lock::Free, 3, m, 40));
        assert!(!covered(Lock::Held, 3, m, 40), "a running check");
        assert!(!covered(Lock::Free, 4, m, 40), "a change after the check");
        assert!(!covered(Lock::Free, 3, m, 41), "the check was an earlier turn's");
        assert!(!covered(Lock::Free, 3, Some(Mark { covered: 3, started_ms: 0, turn: None }), 40), "a mark without its turn");
        assert!(!covered(Lock::Free, 0, None, 40));
    }

    #[test]
    fn the_stop_fits_its_wait_and_model_call_into_the_harness_budget() {
        let s = Duration::from_secs;
        assert_eq!(STOP_WAIT, s(15), "a quarter of the 60 s a stop gets where the harness names none");
        assert_eq!((stop_wait(None), call_limit(None, s(30))), (s(15), s(45)), "Claude Code: unchanged");
        assert_eq!((stop_wait(Some(s(90))), call_limit(Some(s(90)), s(15))), (s(15), s(45)), "opencode: room for both");
        assert_eq!((stop_wait(Some(s(28))), call_limit(Some(s(28)), s(7))), (s(7), s(18)), "omp");
        assert_eq!(call_limit(Some(s(28)), s(11)), s(14), "time spent before the call comes off it");
        assert_eq!(call_limit(Some(s(28)), s(27)), Duration::ZERO);
        assert_eq!(stop_wait(Some(s(8))), s(2));
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
