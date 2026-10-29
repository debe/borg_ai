//! A long-lived process follows a reinstall in place: `cargo install` renames a new file over the old,
//! and an old image would go on running the code it was started with.
//!
//! - A session's waiter (listen.rs) re-execs its own path (`Image::reexec`), same pid, same stdout (a
//!   plugin's JSON line protocol and Claude Code's background task survive), its lock's open file
//!   description passed on (`ADOPT_FD`), so the lock is never free between the images and no second
//!   waiter starts meanwhile. SIGINT, SIGHUP and SIGTERM are blocked across the exec and seen by the new
//!   image once it trapped them (`inherited`): none is lost. Its only other threads (runner.rs
//!   `run_bounded`'s readers) start with them blocked too.
//!
//! The mesh daemon (borg-mesh main.rs `serve`) follows a reinstall of borg-mesh in place: it lives as
//! long as the machine serves, and `cargo install` renames a new file over the old. Once its image is
//! stale (`Image::due`) it stops as on Ctrl-C, and `Image::restart` execs the new borg-mesh in the same
//! process (`trap_stops`, `stopped`: a stop during it is held for the new image, which ends on it before
//! it serves). Only one that says it takes such a restart (`probed`) is exec'd: one built before it
//! could (a downgrade) would never unblock the stops, so no Ctrl-C or SIGTERM would ever end it. The
//! daemon exits 1 with a message then, to be started again.
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

/// The lock fd a re-exec'd waiter inherits from its previous image (`Image::reexec`, `adopt`). Never set
/// by an engineer: removed from the environment first thing, so no child sees it.
const ADOPT_FD: &str = "BORG_LISTEN_FD";

/// How often an exec of a busy file (`ETXTBSY`) is tried, 25 ms apart, the stop signals held meanwhile.
/// Guard: half a second for an installer still closing the new binary, then the old image keeps
/// running rather than retrying forever with Ctrl-C held off.
const BUSY_TRIES: u32 = 20;

/// Set in the environment of a process `Image::restart` exec'd: its stops are blocked until
/// `trap_stops` trapped them. Removed there, so no child sees it.
const RESTARTED: &str = "BORG_RESTARTED";

/// Set in the environment of `restart`'s probe of the file it would exec (run as `<path> --help`, which
/// every borg-mesh answers at once): one that takes a restart prints `PROBED` instead (`probed`).
const PROBE: &str = "BORG_RESTART_PROBE";
const PROBED: &str = "takes a restart in place";

/// How long `restart` waits for its probe's answer. Guard: a new file that hangs on `--help` is not
/// exec'd, and the running image goes on.
const PROBE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long `restart` waits for the process's other threads to end (Linux), so a stop can reach
/// no thread but the one that holds it blocked. Guard: a thread that never ends delays the restart
/// by this much, never blocks it.
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

/// Why `Image::reexec` did not replace the image.
#[derive(Debug)]
pub enum ReexecError {
    /// The lock's fd could not be duplicated for the new image.
    Dup(std::io::Error),
    /// The exec of this path failed.
    Exec(PathBuf, std::io::Error),
}

impl std::fmt::Display for ReexecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReexecError::Dup(e) => write!(f, "the lock could not be passed on: {e}"),
            ReexecError::Exec(path, e) => write!(f, "{}: {e}", path.display()),
        }
    }
}

impl std::error::Error for ReexecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReexecError::Dup(e) | ReexecError::Exec(_, e) => Some(e),
        }
    }
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
    /// This process's image. In this crate's unit tests an image of no file, never stale or due: their
    /// binary is the shared target's, which another `cargo test` may rebuild while a waiter test runs
    /// (listen.rs `Waiter`), and a re-exec would replace the test runner with it.
    pub fn now() -> Image {
        if cfg!(test) {
            return Image::of(PathBuf::new(), None);
        }
        Image::of(exe_path(), std::fs::File::open("/proc/self/exe").ok())
    }

    fn of(path: PathBuf, running: Option<std::fs::File>) -> Image {
        let base = exec_id(&path);
        Image { path, base, running, failed: Vec::new() }
    }

    /// The path names another file than at the start (or none), or the running binary was deleted: this
    /// image is old code.
    pub fn stale(&self) -> bool {
        exec_id(&self.path) != self.base || self.running.as_ref().is_some_and(|f| f.metadata().is_ok_and(|m| m.nlink() == 0))
    }

    /// Stale, and the path holds an executable no exec failed on: a re-exec is worth trying.
    pub fn due(&self) -> bool {
        let now = exec_id(&self.path);
        now.is_some() && !self.failed.contains(&now) && self.stale()
    }

    /// For a waiter (listen.rs): replaces this process's image by the file at its path, same pid, argv
    /// and environment, stdout and stderr flushed, the stop signals blocked across (`stopped` looked at
    /// once more after they are), `lock`'s open file description passed on as a duplicate without
    /// close-on-exec (`ADOPT_FD`). Returns only when it did not happen: Ok when a stop came (the caller
    /// ends as stopped), Err with why the exec failed (the path is not tried again until it changes).
    ///
    /// # Errors
    /// The dup or the exec failed (`ReexecError`).
    pub fn reexec(&mut self, lock: &std::fs::File, stopped: impl Fn() -> bool) -> Result<(), ReexecError> {
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        let target = exec_id(&self.path);
        let set = stop_set();
        // SAFETY: pthread_sigmask(3) on a set built by sigemptyset/sigaddset.
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, std::ptr::null_mut()) };
        let unblock = || {
            // SAFETY: as above.
            unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const set, std::ptr::null_mut()) };
        };
        if stopped() {
            unblock();
            return Ok(());
        }
        // SAFETY: fcntl(2) F_DUPFD on the lock's own open fd: a new fd, close-on-exec clear.
        let dup = unsafe { libc::fcntl(lock.as_raw_fd(), libc::F_DUPFD, 3) };
        let err = if dup < 0 {
            ReexecError::Dup(std::io::Error::last_os_error())
        } else {
            let e = execve_retrying(&self.path, ADOPT_FD, &dup.to_string());
            // SAFETY: close(2) of the duplicate made above, which nothing else owns.
            unsafe { libc::close(dup) };
            ReexecError::Exec(self.path.clone(), e)
        };
        unblock();
        self.failed.push(target);
        if stopped() { Ok(()) } else { Err(err) }
    }

    /// For the mesh daemon, once it stopped serving: replaces its image by the file at its path, stdout
    /// and stderr flushed, same pid, argv and environment, with SIGINT, SIGHUP and SIGTERM blocked
    /// across; a stop `trap_stops` saw first returns Ok (the caller ends as stopped), one that comes
    /// later stays pending for the new image (`trap_stops` there). Only a
    /// file whose probe says it unblocks them there (`takes_restart`) is exec'd. On Linux it first
    /// waits up to `ALONE_WAIT` for its other threads to end, so none can take a stop meanwhile (the
    /// daemon's are gone once its runtime shut down). Err with what the engineer does now.
    pub fn restart(&mut self) -> Result<(), String> {
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
        let takes = takes_restart(&self.path) && exec_id(&self.path) == target;
        if takes {
            alone();
        }
        if stopped() {
            unblock();
            return Ok(());
        }
        let err = takes.then(|| execve_retrying(&self.path, RESTARTED, "1"));
        unblock();
        // The exec did not happen: the path is not tried again until it changes.
        self.failed.push(target);
        match err {
            // A stop held across the probe (a Ctrl-C at the terminal reached the probe too): the
            // caller ends as stopped.
            _ if stopped() => Ok(()),
            None => Err(format!(
                "{} cannot take over a running `borg mesh serve` (a borg-mesh built before it could, or one that changed meanwhile): start `borg mesh serve` again",
                self.path.display()
            )),
            Some(why) => {
                Err(format!("could not run the reinstalled borg-mesh ({}: {why}): install it again, then start `borg mesh serve`", self.path.display()))
            }
        }
    }
}

/// execve(2) of `path` with this process's argv and environment, `var` set to `value` in it (any value
/// it had dropped); a busy file (`ETXTBSY`) tried again `BUSY_TRIES` times. Returns only on failure,
/// with why.
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
    on_signals(&[libc::SIGINT, libc::SIGTERM], on_stop);
    if std::env::var_os(RESTARTED).is_some() {
        // SAFETY: called while the process runs one thread (the doc above); no other reads the
        // environment.
        unsafe { std::env::remove_var(RESTARTED) };
        unblock_stops();
    }
}

/// `handler` for each of `sigs`, by sigaction(2): it stays installed across deliveries, and a call it
/// interrupts restarts. `handler` must be async-signal-safe (store to an atomic, no more).
pub fn on_signals(sigs: &[libc::c_int], handler: extern "C" fn(libc::c_int)) {
    for &sig in sigs {
        // SAFETY: a zeroed sigaction is valid; only its handler, its empty mask and SA_RESTART are set
        // before sigaction(2) reads it.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handler as libc::sighandler_t;
            sa.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&raw mut sa.sa_mask);
            libc::sigaction(sig, &raw const sa, std::ptr::null_mut());
        }
    }
}

/// The stop signals (`stop_set`) unblocked: one held across an exec is delivered now.
fn unblock_stops() {
    let set = stop_set();
    // SAFETY: pthread_sigmask(3) on a set built by sigemptyset/sigaddset.
    unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const set, std::ptr::null_mut()) };
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

/// The first step of a waiter (listen.rs), once it trapped its stop signals: when a previous image
/// re-exec'd it (`Image::reexec`), the inherited lock fd, its variable gone from the environment, the
/// stop signals unblocked only now, so a stop that came across the exec is seen; None for a fresh
/// start. The fd is the waiter's lock only once `adopt` checked it.
pub fn inherited() -> Option<RawFd> {
    let fd = std::env::var_os(ADOPT_FD)?;
    // SAFETY: the waiter is single-threaded this early; no other thread reads the environment.
    unsafe { std::env::remove_var(ADOPT_FD) };
    unblock_stops();
    fd.to_str().and_then(|s| s.parse::<RawFd>().ok()).filter(|fd| *fd > 2)
}

/// The inherited `fd` as the lock of `path`, when it is that file's open description and holds its lock
/// (a try_lock on the same description succeeds); else None (a forged or stale variable: the waiter
/// takes the lock as a fresh one does). Close-on-exec again from here.
pub fn adopt(fd: RawFd, path: &Path) -> Option<std::fs::File> {
    // SAFETY: fcntl(2) F_GETFD only asks whether `fd` is open, so an fd that is not is never owned.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return None;
    }
    // SAFETY: `fd` is open and was handed to this process for it alone (`Image::reexec`): owned here.
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

    /// Guard: the test binary may be rebuilt under a running waiter test; an image taken in a unit test
    /// names no file, so it is never stale or due.
    #[test]
    fn an_image_in_a_unit_test_never_follows_the_test_binary() {
        let img = Image::now();
        assert!(exec_id(&img.path).is_none() && !img.stale() && !img.due());
        assert!(exec_id(&exe_path()).is_some(), "the test binary itself is a file: the seam, not the path, keeps it out");
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
        use std::os::unix::io::IntoRawFd;
        let d = temp("adopt");
        let (lock, other) = (d.join("w.lock"), d.join("other"));
        std::fs::write(&lock, "").unwrap();
        std::fs::write(&other, "").unwrap();
        let fd = |p: &Path| std::fs::OpenOptions::new().write(true).open(p).unwrap();
        assert!(adopt(fd(&other).into_raw_fd(), &lock).is_none(), "another file");
        let held = fd(&lock);
        held.lock().unwrap();
        assert!(adopt(fd(&lock).into_raw_fd(), &lock).is_none(), "another description, the lock held by it");
        drop(held);
        let mine = fd(&lock);
        mine.lock().unwrap();
        assert!(adopt(mine.into_raw_fd(), &lock).is_some(), "the description that holds the lock");
        assert!(adopt(9999, &lock).is_none(), "no open fd");
        let _ = std::fs::remove_dir_all(d);
    }
}
