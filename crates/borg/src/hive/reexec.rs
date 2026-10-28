//! A waiter follows a reinstall of borg in place (wait.rs, docs/design/hive.md "Phase g"). A `borg hive
//! wait` lives for a session's life and runs the code it was started with: `cargo install` renames a new
//! file over the old, so a waiter started before it went on claiming with the old fold (the 0.3.0
//! release goal's stale `deps_in`). Two rules here, both from what the waiter saw at its start:
//!
//! - Gate: an image whose executable was replaced (`Image::stale`) claims nothing (wait.rs `waiting`
//!   looks at the loop top and again right before `take`). That alone makes a claim by old code
//!   unreachable, whatever the timers, an exec that fails included.
//! - Re-exec (`Image::reexec`): at the loop top the waiter execs its own path again, same pid, same
//!   stdout (a plugin's JSON line protocol and Claude Code's background task survive), and its lock's
//!   open file description passed on (`ADOPT_FD`), so its flock is never free between the images: no
//!   second waiter starts meanwhile and `stop_waiter`'s pid stays right. SIGINT, SIGHUP and SIGTERM are
//!   blocked across the exec and seen by the new image once it trapped them (`inherited`): none is lost.
//!   That needs every other thread of the waiter to hold them blocked too, or a process-directed stop
//!   could land in one, set STOP in the old image and die with it: its only ones, run_bounded's readers
//!   (a `codex queue`'s, which may outlive it), start with them blocked (runner.rs `helper`).
//! - A downgrade to a borg before 0.3.0, which knows no `ADOPT_FD`, ends the waiter: the new image finds
//!   the lock held by its own inherited fd, says the wait "runs already" and exits 0. The drone's next
//!   stop starts a waiter again.
//!
//! - The mesh daemon (borg-mesh main.rs `serve`) lives as long and follows a reinstall of borg-mesh
//!   the same way, with no lock to pass: its node worker stops ticking once the image is stale
//!   (node.rs `serve`), the daemon stops as on Ctrl-C, and `Image::restart` execs the new borg-mesh in
//!   the same process (`trap_stops`, `stopped`: a stop during it is held for the new image, which ends
//!   on it before it serves). Only one that says it takes such a restart (`probed`) is exec'd: one
//!   built before it could (a downgrade) would never unblock the stops, so no Ctrl-C or SIGTERM would
//!   ever end it. The daemon exits 1 with a message then, to be started again.
//!
//! The identity is that of the path the process was started by (argv[0] resolved as execvp does), not
//! of `/proc/self/exe`: a wrapper or shim is the file that is exec'd, so comparing it with the binary it
//! starts would differ forever and re-exec in a loop. A running binary deleted from under a path that
//! stays (a symlinked install whose old target went) counts too: its link count is 0.

use std::ffi::{CString, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The lock fd a re-exec'd waiter inherits from its previous image (`reexec`, `adopt`). Never set by
/// an engineer: removed from the environment first thing, so no child sees it.
const ADOPT_FD: &str = "BORG_HIVE_WAITER_FD";

/// Test only: how long a re-exec'd waiter waits before it traps its stop signals, which are blocked
/// meanwhile (a stop sent then must still end it as a stop, never by the signal's default action).
const ADOPT_DELAY: &str = "BORG_HIVE_ADOPT_MS";

/// How often an exec of a busy file (`ETXTBSY`) is tried, 25 ms apart, the stop signals held meanwhile.
const BUSY_TRIES: u32 = 20;

/// Set in the environment of a process `Image::restart` exec'd: its stops are blocked until
/// `trap_stops` trapped them. Removed there, so no child sees it.
const RESTARTED: &str = "BORG_RESTARTED";

/// Set in the environment of `restart`'s probe of the file it would exec (run as `<path> --help`, which
/// every borg-mesh answers at once): one that takes a restart prints `PROBED` instead (`probed`).
const PROBE: &str = "BORG_RESTART_PROBE";
const PROBED: &str = "takes a restart in place";

/// How long `restart` waits for its probe's answer.
const PROBE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long `restart` waits for the process's other threads to end (Linux), so a stop can reach
/// no thread but the one that holds it blocked.
const ALONE_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// What makes one executable file this one: a rename, a copy or a rebuild changes one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Id {
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i128,
}

fn id_of(m: &std::fs::Metadata) -> Id {
    Id { dev: m.dev(), ino: m.ino(), size: m.size(), mtime_ns: i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()) }
}

/// The identity of the regular executable at `p` (symlinks followed); None for anything else.
fn exec_id(p: &Path) -> Option<Id> {
    let m = std::fs::metadata(p).ok()?;
    (m.is_file() && m.mode() & 0o111 != 0).then(|| id_of(&m))
}

/// The path this process was started by: argv[0] as execvp resolves it (a word with a `/` against the
/// working directory, a bare word on PATH), never canonicalized, so a wrapper or a symlinked install is
/// the file named; else the running binary with Linux's ` (deleted)` cut off; else `borg`. What a
/// long-lived process starts borg as and prints commands with: `current_exe()` of a replaced binary
/// reads `<path> (deleted)`.
pub(crate) fn exe_path() -> PathBuf {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| from_argv0(std::env::args_os().next(), std::env::var_os("PATH")).unwrap_or_else(running)).clone()
}

fn from_argv0(arg0: Option<OsString>, path: Option<OsString>) -> Option<PathBuf> {
    let a = PathBuf::from(arg0.filter(|a| !a.is_empty())?);
    if a.as_os_str().as_bytes().contains(&b'/') {
        return Some(if a.is_absolute() { a } else { std::env::current_dir().ok()?.join(a) });
    }
    std::env::split_paths(&path?).filter(|d| d.is_absolute()).map(|d| d.join(&a)).find(|p| exec_id(p).is_some())
}

fn running() -> PathBuf {
    std::env::current_exe().map_or_else(|_| PathBuf::from("borg"), |e| undeleted(&e))
}

/// `p` without the ` (deleted)` Linux appends to the link of a running binary that was replaced.
fn undeleted(p: &Path) -> PathBuf {
    let b = p.as_os_str().as_bytes();
    PathBuf::from(std::ffi::OsStr::from_bytes(b.strip_suffix(b" (deleted)").unwrap_or(b)))
}

/// What a long-lived process's image was started from (`now`, at its start): whether it is the binary
/// installed now.
pub struct Image {
    path: PathBuf,
    base: Option<Id>,
    /// The running binary, opened at the start (Linux): its link count 0 says it was deleted.
    running: Option<std::fs::File>,
    /// The identities of the path an exec failed on: not tried again until the path changes.
    failed: Vec<Option<Id>>,
}

impl Image {
    pub fn now() -> Image {
        Image::of(exe_path(), std::fs::File::open("/proc/self/exe").ok())
    }

    /// The same start, to look at from another thread (the daemon's node worker).
    pub fn watch(&self) -> Image {
        Image { path: self.path.clone(), base: self.base, running: self.running.as_ref().and_then(|f| f.try_clone().ok()), failed: Vec::new() }
    }

    fn of(path: PathBuf, running: Option<std::fs::File>) -> Image {
        let base = exec_id(&path);
        Image { path, base, running, failed: Vec::new() }
    }

    /// The path names another file than at the start (or none), or the running binary was deleted: this
    /// image is old code and claims nothing (the gate).
    pub fn stale(&self) -> bool {
        exec_id(&self.path) != self.base || self.running.as_ref().is_some_and(|f| f.metadata().is_ok_and(|m| m.nlink() == 0))
    }

    /// Stale, and the path holds an executable no exec failed on: a re-exec is worth trying.
    pub fn due(&self) -> bool {
        let now = exec_id(&self.path);
        now.is_some() && !self.failed.contains(&now) && self.stale()
    }

    /// Replaces this process's image by the file at its path, same pid: stdout and stderr flushed, the
    /// stop signals blocked first (`stopped` looked at once more after), `lock`'s open file description
    /// passed on as a duplicate without close-on-exec (the original keeps it, so no child spawned
    /// before ever holds the lock), the original argv and environment. Returns only when it did not
    /// happen: Ok when a stop came (the caller's next pass stops), Err with why the exec failed (the
    /// image stays gated until the path changes again). The waiter's other threads (run_bounded's
    /// readers) never fork, so the duplicate reaches no process but the new image, and hold the stop
    /// signals blocked (module doc), so a stop during the exec is pending for the new image.
    pub(super) fn reexec(&mut self, lock: &std::fs::File, stopped: impl Fn() -> bool) -> Result<(), String> {
        self.exec_held(stopped, || {
            // SAFETY: fcntl(2) F_DUPFD on the lock's own open fd: a new fd, close-on-exec clear.
            let dup = unsafe { libc::fcntl(lock.as_raw_fd(), libc::F_DUPFD, 3) };
            if dup < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok((ADOPT_FD, dup.to_string(), Some(dup)))
        })
    }

    /// For a process that passes nothing on (the mesh daemon, once it stopped serving): replaces its
    /// image by the file at its path as `reexec` does, same pid, argv and environment, with SIGINT,
    /// SIGHUP and SIGTERM blocked across; a stop `trap_stops` saw first returns Ok (the caller ends as
    /// stopped), one that comes later stays pending for the new image (`trap_stops` there). Only a
    /// file whose probe says it unblocks them there (`takes_restart`) is exec'd. On Linux it first
    /// waits up to `ALONE_WAIT` for its other threads to end, so none can take a stop meanwhile (the
    /// daemon's are gone once its runtime shut down). Err with what the engineer does now.
    pub fn restart(&mut self) -> Result<(), String> {
        let (path, target) = (self.path.clone(), exec_id(&self.path));
        let mut refused = false;
        let done = self.exec_held(stopped, || {
            if !takes_restart(&path) || exec_id(&path) != target {
                refused = true;
                return Err(std::io::Error::other("refused"));
            }
            alone();
            Ok((RESTARTED, "1".into(), None))
        });
        match done {
            // A stop held across the probe (a Ctrl-C at the terminal reached the probe too): the
            // caller ends as stopped.
            Err(_) if stopped() => Ok(()),
            Err(_) if refused => Err(format!(
                "{} cannot take over a running `borg mesh serve` (a borg-mesh built before it could, or one that changed meanwhile): start `borg mesh serve` again",
                path.display()
            )),
            Err(why) => Err(format!("could not run the reinstalled borg-mesh ({why}): install it again, then start `borg mesh serve`")),
            Ok(()) => Ok(()),
        }
    }

    /// The exec with the stop signals blocked (`stopped` looked at once more after, Ok when it says
    /// so): `pass` names the variable that tells the new image what it inherits, and an fd to close if
    /// the exec does not happen.
    fn exec_held(&mut self, stopped: impl Fn() -> bool, pass: impl FnOnce() -> std::io::Result<(&'static str, String, Option<RawFd>)>) -> Result<(), String> {
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        let target = exec_id(&self.path);
        let set = stop_set();
        // SAFETY: pthread_sigmask(3) on a set built by sigemptyset/sigaddset.
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) };
        let unblock = || {
            // SAFETY: as above.
            unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut()) };
        };
        let passed = pass();
        if stopped() {
            if let Ok((_, _, Some(fd))) = passed {
                // SAFETY: close(2) of the fd `pass` made for the exec, which nothing else owns.
                unsafe { libc::close(fd) };
            }
            unblock();
            return Ok(());
        }
        let err = match passed {
            Err(e) => e,
            Ok((var, value, fd)) => {
                let e = execve_retrying(&self.path, var, &value);
                if let Some(fd) = fd {
                    // SAFETY: as above.
                    unsafe { libc::close(fd) };
                }
                e
            }
        };
        unblock();
        self.failed.push(target);
        Err(format!("{}: {err}", self.path.display()))
    }
}

/// execve(2) of `path` with this process's argv and environment, `var` set to `value` in it (any
/// value it had dropped); a busy file (`ETXTBSY`) tried again `BUSY_TRIES` times. Returns only on
/// failure, with why.
fn execve_retrying(path: &Path, var: &str, value: &str) -> std::io::Error {
    let cstr = |b: &[u8]| CString::new(b).ok();
    let file = cstr(path.as_os_str().as_bytes());
    let argv: Option<Vec<CString>> = std::env::args_os().map(|a| cstr(a.as_bytes())).collect();
    let envp: Option<Vec<CString>> = std::env::vars_os()
        .filter(|(k, _)| k != var)
        .map(|(k, v)| cstr(&[k.as_bytes(), b"=", v.as_bytes()].concat()))
        .chain([cstr(format!("{var}={value}").as_bytes())])
        .collect();
    let (Some(file), Some(argv), Some(envp)) = (file, argv, envp) else { return std::io::Error::from(std::io::ErrorKind::InvalidInput) };
    let ptrs = |v: &[CString]| v.iter().map(|c| c.as_ptr()).chain([std::ptr::null()]).collect::<Vec<_>>();
    let (argv, envp) = (ptrs(&argv), ptrs(&envp));
    let mut tries = 0;
    loop {
        // SAFETY: execve(2) with NUL-terminated strings and NULL-terminated arrays that outlive the
        // call; it returns only on failure.
        unsafe { libc::execve(file.as_ptr(), argv.as_ptr(), envp.as_ptr()) };
        let e = std::io::Error::last_os_error();
        // A file still open for writing somewhere (a fork that has not exec'd yet, a copy not closed
        // yet) is busy for a moment only.
        tries += 1;
        if e.raw_os_error() != Some(libc::ETXTBSY) || tries == BUSY_TRIES {
            return e;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// Whether the file at `path`, run as `<path> --help` with `PROBE` set, says `PROBED` within
/// `PROBE_WAIT`: it is a borg-mesh that unblocks the stops `restart` holds across its exec. A borg-mesh
/// built before it could prints its help; a shim passes the probe on to what it runs.
fn takes_restart(path: &Path) -> bool {
    use std::process::{Command, Stdio};
    let spawn = || Command::new(path).arg("--help").env(PROBE, "1").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
    // A busy file (`ETXTBSY`, as for the exec after) is tried again; the child's signal mask is
    // reset by std, so the stops `restart` holds blocked reach it.
    let mut tries = 1;
    let mut child = loop {
        match spawn() {
            Ok(c) => break c,
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && tries < BUSY_TRIES => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(_) => return false,
        }
    };
    let t0 = std::time::Instant::now();
    let done = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if t0.elapsed() < PROBE_WAIT => std::thread::sleep(std::time::Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let mut out = String::new();
    done && child.stdout.take().is_some_and(|mut o| std::io::Read::read_to_string(&mut o, &mut out).is_ok()) && out.trim() == PROBED
}

/// The first step of a process that may be `restart`ed into (borg-mesh `main`): true when this run is
/// `restart`'s probe, answered here (the caller then ends at once).
pub fn probed() -> bool {
    if std::env::var_os(PROBE).is_none() {
        return false;
    }
    println!("{PROBED}");
    true
}

/// Waits up to `ALONE_WAIT` until this process runs one thread (Linux; elsewhere at once).
fn alone() {
    let t0 = std::time::Instant::now();
    let threads = || std::fs::read_dir("/proc/self/task").map_or(1, Iterator::count);
    while threads() > 1 && t0.elapsed() < ALONE_WAIT {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// A stop (SIGINT, SIGTERM) a process that `restart`s saw: `trap_stops`'s handler sets it.
static STOPPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_stop(_: libc::c_int) {
    STOPPED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// The first step of a process that may `restart` (the mesh daemon), while it runs one thread: SIGINT
/// and SIGTERM trapped (`stopped` says whether one came), and, when a previous image restarted it,
/// the stop signals unblocked only then, so a stop that came across the exec is seen now (a SIGHUP
/// then ends it, as it would have the old one).
pub fn trap_stops() {
    let handler = on_stop as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
    if std::env::var_os(RESTARTED).is_some() {
        // SAFETY: called while the process runs one thread (the doc above); no other reads the
        // environment.
        unsafe { std::env::remove_var(RESTARTED) };
        let set = stop_set();
        // SAFETY: pthread_sigmask(3) on a set built by sigemptyset/sigaddset.
        unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut()) };
    }
}

/// Whether a stop came since `trap_stops`.
pub fn stopped() -> bool {
    STOPPED.load(std::sync::atomic::Ordering::SeqCst)
}

fn stop_set() -> libc::sigset_t {
    // SAFETY: sigemptyset(3)/sigaddset(3) initialise the zeroed set before it is read.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in [libc::SIGINT, libc::SIGHUP, libc::SIGTERM] {
            libc::sigaddset(&mut set, s);
        }
        set
    }
}

/// The first step of a waiter (wait.rs `main`): when a previous image re-exec'd it (`reexec`), the
/// inherited lock fd, its variable gone from the environment, the stop signals trapped (`trap`) and
/// only then unblocked, so a stop that came across the exec is in STOP now; None for a fresh start.
/// The fd is a waiter's lock only once `adopt` checked it.
pub(super) fn inherited(trap: impl Fn()) -> Option<RawFd> {
    let fd = std::env::var_os(ADOPT_FD)?;
    // SAFETY: the waiter is single-threaded this early; no other thread reads the environment.
    unsafe { std::env::remove_var(ADOPT_FD) };
    // Test only: the moment a stop would end the process (no handler yet), were it not blocked.
    if let Some(ms) = std::env::var(ADOPT_DELAY).ok().and_then(|v| v.trim().parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    trap();
    let set = stop_set();
    // SAFETY: pthread_sigmask(3) on a set built by sigemptyset/sigaddset.
    unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut()) };
    fd.to_str().and_then(|s| s.parse::<RawFd>().ok()).filter(|fd| *fd > 2)
}

/// The inherited `fd` as the lock of `path`, when it is that file's open description and holds its
/// lock (a try_lock on the same description succeeds); else it is closed and None (a forged or stale
/// variable: the waiter takes the lock as a fresh one does). Close-on-exec again from here.
pub(super) fn adopt(fd: RawFd, path: &Path) -> Option<std::fs::File> {
    // SAFETY: fcntl(2) F_GETFD only asks whether `fd` is open, so an fd that is not is never owned.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return None;
    }
    // SAFETY: `fd` is open and was handed to this process for it alone (`reexec`): owned from here.
    let f = unsafe { std::fs::File::from_raw_fd(fd) };
    let same = match (f.metadata(), std::fs::metadata(path)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    };
    if !same || f.try_lock().is_err() {
        return None;
    }
    // SAFETY: fcntl(2) F_SETFD on the fd owned above.
    unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-reexec-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn install(p: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let part = p.with_extension("part");
        std::fs::write(&part, body).unwrap();
        std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&part, p).unwrap();
    }

    /// A wrapper or shim is the path, the binary it starts another file: never stale for that, so no
    /// re-exec loop. A rename over the path makes it stale and due; an exec that failed on that file is
    /// not due again (still stale: gated) until the path changes once more.
    #[test]
    fn a_shim_is_not_replaced_and_a_rename_over_it_is() {
        let d = temp("shim");
        let p = d.join("borg");
        install(&p, "#!/bin/sh\nexec /bin/true\n");
        let mut img = Image::of(p.clone(), std::fs::File::open("/bin/sh").ok());
        assert!(!img.stale() && !img.due(), "the shim and the running binary differ: that is no reinstall");
        install(&p, "#!/bin/sh\nexec /bin/true\n");
        assert!(img.stale() && img.due(), "a rename over the path");
        img.failed.push(exec_id(&p));
        assert!(img.stale() && !img.due(), "gated, not tried again");
        install(&p, "#!/bin/sh\nexec /bin/false\n");
        assert!(img.due(), "a new file is tried");
        let _ = std::fs::remove_dir_all(d);
    }

    /// A running binary deleted from under a path that stays (a symlinked install's old target) is
    /// stale: its link count is 0.
    #[test]
    fn a_deleted_running_binary_is_stale() {
        let d = temp("unlinked");
        let p = d.join("borg");
        install(&p, "#!/bin/sh\n");
        let old = d.join("old");
        install(&old, "#!/bin/sh\n");
        let img = Image::of(p, std::fs::File::open(&old).ok());
        assert!(!img.stale());
        std::fs::remove_file(&old).unwrap();
        assert!(img.stale() && img.due(), "the path holds an executable to re-exec");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_path_is_argv0_resolved_without_the_deleted_suffix() {
        assert_eq!(undeleted(Path::new("/x/bin/borg (deleted)")), PathBuf::from("/x/bin/borg"));
        assert_eq!(undeleted(Path::new("/x/bin/borg")), PathBuf::from("/x/bin/borg"));
        let d = temp("argv0");
        install(&d.join("borg"), "#!/bin/sh\n");
        let path = std::env::join_paths([Path::new("/nonexistent"), &d]).unwrap();
        assert_eq!(from_argv0(Some("borg".into()), Some(path.clone())), Some(d.join("borg")), "a bare word on PATH");
        assert_eq!(from_argv0(Some("/a/b/borg".into()), Some(path)), Some(PathBuf::from("/a/b/borg")), "as given, never canonicalized");
        assert_eq!(from_argv0(Some("nope".into()), Some(d.as_os_str().into())), None, "the running binary then");
        let _ = std::fs::remove_dir_all(d);
    }

    /// An inherited fd is a waiter's lock only when it is that file's description holding its lock.
    #[test]
    fn an_inherited_fd_of_another_file_is_not_adopted() {
        let d = temp("adopt");
        let (lock, other) = (d.join("w.hive-waiter"), d.join("other"));
        std::fs::write(&lock, "").unwrap();
        std::fs::write(&other, "").unwrap();
        let fd = |p: &Path| std::fs::OpenOptions::new().write(true).open(p).unwrap();
        assert!(adopt(std::os::unix::io::IntoRawFd::into_raw_fd(fd(&other)), &lock).is_none(), "another file");
        let held = fd(&lock);
        held.lock().unwrap();
        assert!(adopt(std::os::unix::io::IntoRawFd::into_raw_fd(fd(&lock)), &lock).is_none(), "another description, the lock held by it");
        drop(held);
        let mine = fd(&lock);
        mine.lock().unwrap();
        let raw = std::os::unix::io::IntoRawFd::into_raw_fd(mine);
        assert!(adopt(raw, &lock).is_some(), "the description that holds the lock");
        assert!(adopt(9999, &lock).is_none(), "no open fd");
        let _ = std::fs::remove_dir_all(d);
    }
}
