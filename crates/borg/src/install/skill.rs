//! The `borg` skill (`skills/borg/`): how borg works and how to set it up, for the drone itself. Compiled
//! in and copied by `borg install` into every harness that loads skills ([`dir`]), one
//! [`Change`] per file, with an adapter file's rules: each file is copied whole, a symlinked file is
//! replaced (never written through), and uninstall removes only a file whose content is exactly this
//! borg's, without a backup (borg recreates it exactly), then the skill's directories once empty. A
//! skill directory that is itself a symlink is the engineer's (`ln -s` into a checkout): left alone.
//! `borg status` and `borg doctor` report a missing or stale skill (an older borg's, or edited). borg
//! cannot tell an older borg's text from an edit, so install backs up either one beside it: after an
//! upgrade that changed the skill, uninstall leaves those backups and the directory holding them.

use std::path::{Path, PathBuf};

use super::change::{Action, Change, read};
use crate::harness::{Harness, Level};

/// The skill's files, relative to its directory, and their content.
pub const FILES: [(&str, &str); 6] = [
    ("SKILL.md", include_str!("../../assets/skills/borg/SKILL.md")),
    ("references/collectives.md", include_str!("../../assets/skills/borg/references/collectives.md")),
    ("references/team.md", include_str!("../../assets/skills/borg/references/team.md")),
    ("references/vectors.md", include_str!("../../assets/skills/borg/references/vectors.md")),
    ("references/troubleshooting.md", include_str!("../../assets/skills/borg/references/troubleshooting.md")),
    ("references/hive.md", include_str!("../../assets/skills/borg/references/hive.md")),
];

/// Where `h` loads the `borg` skill from: `skills/borg` in its config directory, the one holding its hook
/// config or its adapter's directory (Claude Code, codex-cli 0.154.0, omp 17.3.5, opencode.ai/docs/skills);
/// `home` and `var` as for `Harness::target`. None for the hidden harness, which loads none. Derived from
/// the wiring, never the name: a harness that loads skills elsewhere needs a `Harness` method of its own.
pub fn dir(h: &dyn Harness, home: &Path, var: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    let up = 1 + usize::from(!h.wiring().adapter().is_empty());
    let dir = h.target(home, var).ancestors().nth(up)?.join("skills/borg");
    (!h.hidden()).then_some(dir)
}

/// How the skill stands in one harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// None of its files is there.
    Absent,
    /// Every file is this borg's.
    Current,
    /// Some file is missing or differs (an older borg's skill, or edited).
    Stale,
    /// The directory is a symlink: the engineer's, left alone.
    Linked,
    /// A file could not be read.
    Unreadable(String),
}

fn linked(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink())
}

/// Each file's current text, or the first read error.
fn current(dir: &Path) -> Result<Vec<Option<String>>, String> {
    FILES.iter().map(|(rel, _)| read(&dir.join(rel)).map_err(|e| format!("{}: cannot read ({e}); nothing was changed", dir.join(rel).display()))).collect()
}

/// What `action` would change in the skill directory `dir` (None: the harness loads no skills), plus
/// notes on what is left alone.
///
/// # Errors
/// A skill file that exists but cannot be read: nothing is planned, as for any config.
pub fn plan(action: Action, dir: Option<&Path>) -> Result<(Vec<Change>, Vec<String>), String> {
    let Some(dir) = dir else { return Ok((Vec::new(), Vec::new())) };
    if linked(dir) {
        return Ok((Vec::new(), vec![format!("{}: left alone, a symlink (yours); the borg skill is not managed there", dir.display())]));
    }
    let befores = current(dir)?;
    let mut changes = Vec::new();
    let mut foreign = Vec::new();
    for ((rel, text), before) in FILES.iter().zip(befores) {
        let path = dir.join(rel);
        let ours = before.as_deref() == Some(*text);
        match action {
            Action::Install if !ours => changes.push(Change { path, before, after: Some((*text).to_string()), through_links: false, backup: true }),
            Action::Uninstall if ours => changes.push(Change { path, before, after: None, through_links: false, backup: false }),
            Action::Uninstall if before.is_some() => foreign.push(path.display().to_string()),
            _ => {}
        }
    }
    let notes = foreign.into_iter().map(|p| format!("{p}: left alone, its content is not this borg's skill (yours, or an older borg's)")).collect();
    Ok((changes, notes))
}

/// After an uninstall: removes the skill's directories that are now empty, innermost first. Anything
/// still in them (a backup, the engineer's own file) keeps them.
pub fn tidy(dir: &Path) {
    if linked(dir) {
        return;
    }
    let _ = std::fs::remove_dir(dir.join("references"));
    let _ = std::fs::remove_dir(dir);
}

/// The skill's state in `dir`.
#[must_use]
pub fn state(dir: &Path) -> State {
    if linked(dir) {
        return State::Linked;
    }
    match current(dir) {
        Err(e) => State::Unreadable(e),
        Ok(found) if found.iter().all(Option::is_none) => State::Absent,
        Ok(found) => {
            let same = FILES.iter().zip(&found).all(|((_, text), f)| f.as_deref() == Some(*text));
            if same { State::Current } else { State::Stale }
        }
    }
}

/// `borg status`: the skill's state for harness `name`, in a few words; None where it loads no skills.
#[must_use]
pub fn status(name: &str, dir: Option<&Path>) -> Option<String> {
    let dir = dir?;
    Some(match state(dir) {
        State::Absent => "skill not installed".to_string(),
        State::Current => format!("skill installed ({})", dir.display()),
        State::Stale => format!("skill STALE: {} differs from this borg's (borg install {name})", dir.display()),
        State::Linked => format!("skill: {} is a symlink (yours)", dir.display()),
        State::Unreadable(e) => format!("skill UNREADABLE: {e}"),
    })
}

/// `borg doctor`: the skill's state for harness `name`. A missing skill is a finding only where borg is
/// wired (`wired`): an upgrade from a borg without the skill.
#[must_use]
pub fn checks(name: &str, dir: Option<&Path>, wired: bool) -> Vec<(Level, String)> {
    let Some(dir) = dir else { return Vec::new() };
    let fix = format!("borg install {name} --apply");
    match state(dir) {
        State::Absent if wired => vec![(Level::Warn, format!("{name}: the borg skill is missing at {} ({fix})", dir.display()))],
        State::Absent => Vec::new(),
        State::Current => vec![(Level::Ok, format!("{name}: borg skill installed at {}", dir.display()))],
        State::Stale => vec![(Level::Warn, format!("{name}: the borg skill at {} differs from this borg's, older or edited ({fix})", dir.display()))],
        State::Linked => vec![(Level::Ok, format!("{name}: the borg skill at {} is a symlink (yours); borg leaves it alone", dir.display()))],
        State::Unreadable(e) => vec![(Level::Warn, format!("{name}: borg skill {e}"))],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::change::apply;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("borg-skill-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn run(action: Action, dir: &Path, stamp: &str) -> (usize, Vec<String>) {
        let (changes, notes) = plan(action, Some(dir)).unwrap();
        for c in &changes {
            apply(c, stamp).unwrap();
        }
        tidy(dir);
        (changes.len(), notes)
    }

    #[test]
    fn the_skill_lands_in_each_harness_own_config_directory() {
        let home = Path::new("/h");
        for h in crate::harness::visible() {
            let d = dir(h, home, &|_| None).unwrap();
            assert!(d.starts_with(home) && d.ends_with("skills/borg"), "{}", d.display());
            // Its config-dir variable moves the skill along with the config.
            assert_eq!(dir(h, home, &|_| Some(PathBuf::from("/v"))).unwrap(), Path::new("/v/skills/borg"), "{}", h.name());
        }
        assert!(crate::harness::ALL.iter().filter(|h| h.hidden()).all(|h| dir(*h, home, &|_| None).is_none()));
    }

    #[test]
    fn the_skill_names_itself_and_says_when_to_load() {
        let (name, skill) = FILES[0];
        assert_eq!(name, "SKILL.md");
        let front = skill.strip_prefix("---\n").and_then(|s| s.split_once("\n---\n")).map(|(f, _)| f).expect("frontmatter first");
        assert!(front.lines().any(|l| l == "name: borg"), "{front}");
        let desc = front.lines().find_map(|l| l.strip_prefix("description: ")).expect("a description");
        assert!(desc.len() <= 1024, "opencode's limit");
        for word in ["borg", "collective", "lesson", "drone", "queen", "mesh", "vectors"] {
            assert!(desc.contains(word), "the description triggers on {word}");
        }
        for (rel, _) in &FILES[1..] {
            assert!(skill.contains(rel), "SKILL.md links {rel}");
        }
    }

    #[test]
    fn install_copies_every_file_once_and_uninstall_removes_exactly_them() {
        let home = tmp("cycle");
        let dir = home.join("skills/borg");
        assert_eq!(state(&dir), State::Absent);
        assert_eq!(run(Action::Install, &dir, "a").0, FILES.len());
        assert_eq!(state(&dir), State::Current);
        assert_eq!(run(Action::Install, &dir, "b").0, 0, "idempotent");
        assert_eq!(run(Action::Uninstall, &dir, "c").0, FILES.len());
        assert!(!dir.exists(), "the empty skill directory goes too");
        assert!(home.join("skills").is_dir(), "the harness's skills directory stays");
        assert_eq!(run(Action::Uninstall, &dir, "d").0, 0);
    }

    #[test]
    fn an_edited_file_is_stale_replaced_with_a_backup_and_never_uninstalled() {
        let dir = tmp("edited").join("borg");
        run(Action::Install, &dir, "a");
        std::fs::write(dir.join("references/team.md"), "my notes").unwrap();
        assert_eq!(state(&dir), State::Stale);
        assert!(checks("x", Some(&dir), true)[0].1.contains("borg install x --apply"), "the harness's own fix, not all");
        let (n, notes) = run(Action::Uninstall, &dir, "u");
        assert_eq!((n, notes.len()), (FILES.len() - 1, 1), "{notes:?}");
        assert_eq!(std::fs::read_to_string(dir.join("references/team.md")).unwrap(), "my notes", "left alone");
        assert_eq!(run(Action::Install, &dir, "i").0, FILES.len());
        assert_eq!(std::fs::read_to_string(dir.join("references/team.md.borg-backup-i")).unwrap(), "my notes", "backed up");
        assert_eq!(state(&dir), State::Current);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_a_linked_file_is_replaced_and_a_linked_directory_left_alone() {
        let home = tmp("links");
        let outside = home.join("checkout.md");
        std::fs::write(&outside, "checkout").unwrap();
        let dir = home.join("borg");
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("SKILL.md")).unwrap();
        run(Action::Install, &dir, "a");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "checkout", "not written through");
        assert_eq!(state(&dir), State::Current);

        let linked_dir = home.join("linked");
        std::os::unix::fs::symlink(home.join("elsewhere"), &linked_dir).unwrap();
        let (n, notes) = run(Action::Install, &linked_dir, "b");
        assert_eq!((n, notes.len()), (0, 1));
        assert!(!home.join("elsewhere").exists());
        assert_eq!(state(&linked_dir), State::Linked);
    }

    #[test]
    fn doctor_warns_on_a_missing_skill_only_where_borg_is_wired() {
        let dir = tmp("doctor").join("borg");
        assert!(checks("x", Some(&dir), false).is_empty());
        assert_eq!(checks("x", Some(&dir), true)[0].0, Level::Warn);
        assert!(checks("x", None, true).is_empty(), "a harness without skills");
        assert_eq!(status("x", None), None);
        run(Action::Install, &dir, "a");
        assert_eq!(checks("x", Some(&dir), true)[0].0, Level::Ok);
        assert!(status("x", Some(&dir)).unwrap().starts_with("skill installed"));
    }
}
