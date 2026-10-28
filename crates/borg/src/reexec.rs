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
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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
fn exe_path() -> PathBuf {
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

    fn of(path: PathBuf, running: Option<std::fs::File>) -> Image {
        let base = exec_id(&path);
        Image { path, base, running, failed: Vec::new() }
    }

    /// The path names another file than at the start (or none), or the running binary was deleted: this
    /// image is old code.
    fn stale(&self) -> bool {
        exec_id(&self.path) != self.base || self.running.as_ref().is_some_and(|f| f.metadata().is_ok_and(|m| m.nlink() == 0))
    }

    /// Stale, and the path holds an executable no exec failed on: a re-exec is worth trying.
    pub fn due(&self) -> bool {
        let now = exec_id(&self.path);
        now.is_some() && !self.failed.contains(&now) && self.stale()
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
        let err = takes.then(|| execve_retrying(&self.path));
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

/// execve(2) of `path` with this process's argv and environment, `RESTARTED` set to 1 in it (any
/// value it had dropped); a busy file (`ETXTBSY`) tried again `BUSY_TRIES` times. Returns only on
/// failure, with why.
fn execve_retrying(path: &Path) -> std::io::Error {
    let cstr = |b: &[u8]| CString::new(b).ok();
    let file = cstr(path.as_os_str().as_bytes());
    let argv: Option<Vec<CString>> = std::env::args_os().map(|a| cstr(a.as_bytes())).collect();
    let envp: Option<Vec<CString>> = std::env::vars_os()
        .filter(|(k, _)| k != RESTARTED)
        .map(|(k, v)| cstr(&[k.as_bytes(), b"=", v.as_bytes()].concat()))
        .chain([cstr(format!("{RESTARTED}=1").as_bytes())])
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
}
