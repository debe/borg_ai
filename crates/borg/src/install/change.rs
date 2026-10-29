//! One config file as it is and as it would be ([`Change`]), its diff, and writing it: a timestamped
//! backup first, then an atomic replace that keeps the file's permissions. `borg install` (`install.rs`,
//! `install/skill.rs`) and `borg mesh join` write every file through here.

use std::path::{Path, PathBuf};

/// One file as it is and as it would be (`None` = absent).
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub path: PathBuf,
    pub before: Option<String>,
    pub after: Option<String>,
    /// Settings files are often symlinks into a dotfiles repo: write through them. An adapter file
    /// symlinked into a borg checkout (README's `ln -s`) is replaced instead, never written through.
    pub through_links: bool,
    /// Whether `apply` keeps a backup of `before`. False only where borg can recreate it exactly: a
    /// skill file removed because it was this borg's own (`install/skill.rs`), whose backup would keep
    /// the skill's directory from ever being removed.
    pub backup: bool,
}

/// What a plan does: wire borg in, or take it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Install,
    Uninstall,
}

/// A file's text; `Ok(None)` only when it does not exist. Unreadable (permissions, not UTF-8) is an
/// error, never "absent": treating it as absent would replace the file without a backup.
pub(crate) fn read(p: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(p) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Mode for files borg creates where none existed: settings.json often holds secrets in `env`.
/// Protocol: owner read and write only, as for a credentials file.
#[cfg(unix)]
const NEW_FILE_MODE: u32 = 0o600;

/// Creates `p` (never an existing file) with `text` and permissions `mode` (unix; ignored elsewhere).
/// The mode is set at creation, so the file is never readable by more people than intended, and
/// set again afterwards, because creation is narrowed by the umask.
fn create_with_mode(p: &Path, text: &str, mode: Option<u32>) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    if let Some(m) = mode {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(m);
    }
    let mut f = o.open(p)?;
    #[cfg(unix)]
    if let Some(m) = mode {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(m))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    f.write_all(text.as_bytes())?;
    f.sync_all()
}

/// `<file>.borg-backup-<stamp>`, or `-1`, `-2`, … after it: an existing backup is never overwritten
/// (an install and an uninstall in the same second would otherwise lose the original).
fn write_backup(path: &Path, name: &str, stamp: &str, text: &str, mode: Option<u32>) -> std::io::Result<PathBuf> {
    for n in 0u32.. {
        let suffix = if n == 0 { String::new() } else { format!("-{n}") };
        let p = path.with_file_name(format!("{name}.borg-backup-{stamp}{suffix}"));
        match create_with_mode(&p, text, mode) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    unreachable!("u32 backups")
}

#[cfg(unix)]
fn mode_of(p: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).ok().map(|m| m.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_of(_: &Path) -> Option<u32> {
    None
}

/// Writes one change: a backup of the existing file first, then an atomic replace. Returns the backup.
/// The replacement and the backup keep the original's permissions (a 0600 file stays 0600); a file
/// that did not exist is created 0600.
///
/// # Errors
/// The backup or the write failed; the file is then as it was.
pub fn apply(c: &Change, stamp: &str) -> std::io::Result<Option<PathBuf>> {
    let name = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mode = mode_of(&c.path); // follows a symlink: the mode of the file actually read
    let backup = match &c.before {
        Some(b) if c.backup => Some(write_backup(&c.path, &name, stamp, b, mode)?),
        _ => None,
    };
    match &c.after {
        None => std::fs::remove_file(&c.path)?,
        Some(text) => replace(c, &name, text, mode)?,
    }
    Ok(backup)
}

/// `c.path` now holds `text`, written to a temporary file beside it and renamed over it: through a
/// symlink where `c.through_links`, else replacing the symlink itself.
fn replace(c: &Change, name: &str, text: &str, mode: Option<u32>) -> std::io::Result<()> {
    let is_link = std::fs::symlink_metadata(&c.path).is_ok_and(|m| m.file_type().is_symlink());
    let target = if is_link && c.through_links { std::fs::canonicalize(&c.path)? } else { c.path.clone() };
    if let Some(d) = target.parent() {
        std::fs::create_dir_all(d)?;
    }
    #[cfg(unix)]
    let mode = mode.or(Some(NEW_FILE_MODE));
    let tmp = target.with_file_name(format!(".{name}.borg-tmp"));
    let _ = std::fs::remove_file(&tmp); // a leftover of an interrupted run, with whatever mode it had
    create_with_mode(&tmp, text, mode)?;
    std::fs::rename(&tmp, &target) // replaces a symlink itself when not writing through it
}

/// A unified diff of two texts, 3 lines of context.
#[must_use]
pub fn diff(path: &Path, before: Option<&str>, after: Option<&str>) -> String {
    let a: Vec<&str> = before.map(|t| t.lines().collect()).unwrap_or_default();
    let b: Vec<&str> = after.map(|t| t.lines().collect()).unwrap_or_default();
    let label = |t: Option<&str>| if t.is_some() { path.display().to_string() } else { "/dev/null".into() };
    format!("--- {}\n+++ {}\n{}", label(before), label(after), hunks(&ops(&a, &b)))
}

/// The edit from `a` to `b` as ' ', '-' and '+' lines: the longest common subsequence, walked.
fn ops<'t>(a: &[&'t str], b: &[&'t str]) -> Vec<(char, &'t str)> {
    let lcs = lcs(a, b);
    let (mut i, mut j, mut ops) = (0, 0, Vec::new());
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            ops.push((' ', a[i]));
            (i, j) = (i + 1, j + 1);
        } else if j < b.len() && (i == a.len() || lcs[i][j + 1] >= lcs[i + 1][j]) {
            ops.push(('+', b[j]));
            j += 1;
        } else {
            ops.push(('-', a[i]));
            i += 1;
        }
    }
    ops
}

/// `lcs[i][j]`: the length of the longest common subsequence of `a[i..]` and `b[j..]`.
fn lcs(a: &[&str], b: &[&str]) -> Vec<Vec<u32>> {
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    lcs
}

/// The lines of `ops` within 3 of a change, each run of them after an `@@` line.
fn hunks(ops: &[(char, &str)]) -> String {
    let near = |k: usize| ops[k.saturating_sub(3)..(k + 4).min(ops.len())].iter().any(|(o, _)| *o != ' ');
    let mut out = String::new();
    let mut gap = true;
    for (k, (o, line)) in ops.iter().enumerate() {
        if near(k) {
            if gap {
                out.push_str("@@\n");
            }
            out.push(*o);
            out.push_str(line);
            out.push('\n');
            gap = false;
        } else {
            gap = true;
        }
    }
    out
}
