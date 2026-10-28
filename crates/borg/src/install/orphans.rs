//! Data a removed feature left behind (the hive, 0.3.0 and before) that `borg doctor` names with its
//! removal command and never removes itself.

use std::path::Path;

use super::Level;

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
}
