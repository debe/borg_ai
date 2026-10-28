//! Data a removed feature left behind (the hive, 0.3.0 and before; the playbooks, 0.4.0) that `borg
//! doctor` names with its removal command and never removes itself.

use std::path::Path;

use super::Level;

/// What a removed feature left in a store, by the feature: the hive's goals and drone registry (a
/// directory), and the playbooks' promoted edits, reflected sessions and waiting clean sessions.
const STORE_ORPHANS: [(&str, &str); 4] =
    [("hive", "the hive"), ("playbooks.jsonl", "the playbooks"), ("playbooks-reflected.txt", "the playbooks"), ("playbooks-plain.txt", "the playbooks")];

/// The removed features' data still in the store `home` of collective `name`: nothing reads it now,
/// and borg never deletes a store's data on its own. Named with its removal, never removed here.
pub(super) fn store_checks(name: &str, home: &Path) -> Vec<(Level, String)> {
    STORE_ORPHANS
        .iter()
        .map(|(file, feature)| (home.join(file), feature))
        .filter(|(p, _)| p.exists())
        .map(|(p, feature)| {
            let rm = if p.is_dir() { "rm -r" } else { "rm" };
            (Level::Warn, format!("store {name}: {} is a removed feature's data ({feature}), unused now; remove it with: {rm} '{}'", p.display(), p.display()))
        })
        .collect()
}

/// The git worktrees a 0.3.0 drone made (`<repo>/.worktrees/hive-<drone>`) still registered in the repo
/// at `cwd`: hidden from `git status`, and nothing removes them now. Named with their removal, never
/// removed here.
pub(super) fn hive_worktree_checks(cwd: &Path) -> Vec<(Level, String)> {
    let out = std::process::Command::new("git").arg("-C").arg(cwd).args(["worktree", "list", "--porcelain"]).output();
    let text = out.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    hive_worktrees(&text)
        .into_iter()
        .map(|w| (Level::Warn, format!("worktree {w} was made by a removed feature (the hive), unused now; remove it with: git worktree remove '{w}'")))
        .collect()
}

/// The `worktree` paths of `git worktree list --porcelain` output that a 0.3.0 drone made.
fn hive_worktrees(porcelain: &str) -> Vec<&str> {
    porcelain
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .filter(|p| {
            Path::new(p).parent().is_some_and(|d| d.ends_with(".worktrees"))
                && Path::new(p).file_name().is_some_and(|n| n.to_string_lossy().starts_with("hive-"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_drones_worktree_is_named() {
        let list = "worktree /w/app\nHEAD abc\nbranch refs/heads/main\n\nworktree /w/app/.worktrees/hive-d1\nHEAD abc\ndetached\n\n\
                    worktree /w/app/.worktrees/mine\nHEAD abc\ndetached\n";
        assert_eq!(hive_worktrees(list), ["/w/app/.worktrees/hive-d1"]);
    }

    #[test]
    fn a_stores_leftover_playbooks_are_named_with_their_removal_and_kept() {
        let home = std::env::temp_dir().join(format!("borg-orphans-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        assert!(store_checks("corp", &home).is_empty(), "a clean store names nothing");
        std::fs::write(home.join("playbooks.jsonl"), "{}\n").unwrap();
        std::fs::write(home.join("playbooks-reflected.txt"), "session/s1\n").unwrap();
        let warn: Vec<String> = store_checks("corp", &home)
            .into_iter()
            .map(|(l, m)| {
                assert_eq!(l, Level::Warn);
                m
            })
            .collect();
        assert_eq!(warn.len(), 2, "{warn:?}");
        assert!(warn[0].contains("(the playbooks)") && warn[0].contains(&format!("rm '{}'", home.join("playbooks.jsonl").display())), "{warn:?}");
        assert!(home.join("playbooks.jsonl").exists() && home.join("playbooks-reflected.txt").exists(), "never deleted by doctor");
        let _ = std::fs::remove_dir_all(&home);
    }
}
