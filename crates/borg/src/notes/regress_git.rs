//! git for regress.rs: every call with a timeout inside one deadline for the whole run, so the
//! end-of-session job never hangs on a huge repo. Paths come back unquoted (`core.quotePath=false`),
//! as blame takes them.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::notes::regress_diff::{self as diff, Hunk};

/// git with a per-call timeout inside one deadline; a call past either, or one that fails, is
/// `None`, and so is every call once the deadline has passed.
pub struct Git<'a> {
    repo: &'a Path,
    call: Duration,
    deadline: Instant,
}

impl<'a> Git<'a> {
    /// Calls of at most `call` each, all of them within `budget` from now.
    #[must_use]
    pub fn new(repo: &'a Path, call: Duration, budget: Duration) -> Git<'a> {
        Git { repo, call, deadline: Instant::now() + budget }
    }

    #[must_use]
    pub fn repo(&self) -> &'a Path {
        self.repo
    }

    pub(crate) fn run(&self, args: &[&str]) -> Option<String> {
        let limit = self.call.min(self.deadline.checked_duration_since(Instant::now())?);
        if limit.is_zero() {
            return None;
        }
        let mut child = Command::new("git")
            .arg("-C")
            .arg(self.repo)
            .args(["-c", "core.quotePath=false"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut out = child.stdout.take()?;
        // Read on a thread: a large output must not block the wait below.
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        });
        let status = wait_for(&mut child, limit)?;
        let text = reader.join().ok()?;
        status.success().then_some(text)
    }

    /// The `-U0` hunks of `sha` against `parent`, whitespace changes left out (`regress_diff.rs`).
    pub(crate) fn hunks(&self, parent: &str, sha: &str) -> Option<Vec<Hunk>> {
        let args = ["diff", "-U0", "-w", "--ignore-blank-lines", "-M", "--no-color", "--no-ext-diff", "--src-prefix=a/", "--dst-prefix=b/", parent, sha];
        self.run(&args).map(|t| diff::hunks(&t))
    }
}

/// The child's exit status, or `None` (killed) when it runs past `limit`.
fn wait_for(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let (started, mut nap) = (Instant::now(), Duration::from_millis(1));
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Some(s),
            Ok(None) if started.elapsed() < limit => {
                std::thread::sleep(nap);
                nap = (nap * 2).min(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_past_its_limit_or_the_deadline_is_none() {
        let dir = std::env::temp_dir();
        assert!(Git::new(&dir, Duration::from_secs(5), Duration::from_secs(5)).run(&["--version"]).is_some_and(|v| v.starts_with("git version")));
        assert!(Git::new(&dir, Duration::from_secs(5), Duration::ZERO).run(&["--version"]).is_none(), "the run's budget is spent");
    }
}
