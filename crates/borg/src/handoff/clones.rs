//! Which repos this machine holds clones of, for the check before an ask (`--to`, mod.rs): a session
//! offered to an engineer is pushed only when one of their machines can take every repo of it.
//!
//! `<store>/handoff/clones.json` maps each clone's git directory (its common one: a linked work tree
//! is its clone) to its name and the hashes of its remotes' URLs (`url_hash`: blake3 of git.rs
//! `normal_url`, so two clones of one repo match whatever URL form each uses and no URL leaves the
//! machine), each URL as git resolves it (`insteadOf` and `pushInsteadOf` rules, the global config):
//! the form the offering side's `git remote get-url` gives too. A session's first hook in a clone
//! records it (`note`): one `git remote -v` when the clone's config or the global one changed since,
//! else the `stat`s of those files alone (an included file's change is read at their next change). What
//! leaves the machine is `machine` (presence.rs `Machine`): the hashes and names of the clones that
//! still exist, never a path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One clone as the file records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Clone {
    name: String,
    /// `url_hash` of every remote's fetch and push URL.
    urls: Vec<String>,
    /// The marks of the configs when they were read (`marks`): read again once one changes.
    #[serde(default)]
    mark: String,
}

fn path(home: &Path) -> PathBuf {
    super::wal::dir(home).join("clones.json")
}

fn read(home: &Path) -> BTreeMap<String, Clone> {
    std::fs::read_to_string(path(home)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// A remote URL as a clone list names it: 32 hex of blake3 over git.rs `normal_url`.
pub(crate) fn url_hash(url: &str) -> String {
    blake3::hash(super::git::normal_url(url).as_bytes()).to_hex()[..32].to_string()
}

/// The work tree `cwd` is in, its git directory and its common one (where the config is), found by
/// walking up to `.git` (a directory, or a linked work tree's `gitdir:` file).
fn git_dirs(cwd: &Path) -> Option<(PathBuf, PathBuf)> {
    let root = cwd.ancestors().find(|d| d.join(".git").exists())?;
    let dot = root.join(".git");
    let dir = if dot.is_dir() {
        dot
    } else {
        let text = std::fs::read_to_string(&dot).ok()?;
        let g = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        if g.is_absolute() { g } else { root.join(g) }
    };
    let common = match std::fs::read_to_string(dir.join("commondir")) {
        Ok(c) => {
            let c = PathBuf::from(c.trim());
            if c.is_absolute() { c } else { dir.join(c) }
        }
        Err(_) => dir.clone(),
    };
    Some((root.to_path_buf(), std::fs::canonicalize(&common).unwrap_or(common)))
}

/// The fetch and push URLs a git config `text` gives its remotes, as written: `url` and `pushurl` of
/// every `[remote "<name>"]` section.
fn config_urls(text: &str) -> Vec<String> {
    let mut remote = false;
    let mut out = Vec::new();
    for l in text.lines().map(str::trim) {
        if l.starts_with('[') {
            remote = l.trim_start_matches('[').trim_start().starts_with("remote ");
            continue;
        }
        if !remote {
            continue;
        }
        if let Some((k, v)) = l.split_once('=')
            && matches!(k.trim().to_ascii_lowercase().as_str(), "url" | "pushurl")
        {
            let v = v.trim().trim_matches('"');
            if !v.is_empty() {
                out.push(v.to_string());
            }
        }
    }
    out
}

/// `url_hash` of each of `urls`, sorted, once each.
fn hashed(urls: &[String]) -> Vec<String> {
    let mut v: Vec<String> = urls.iter().map(|u| url_hash(u)).collect();
    v.sort();
    v.dedup();
    v
}

/// The URL hashes of the clone `cwd` is in: as `note` recorded them (git's resolved URLs), else, for a
/// clone not recorded yet, its config's URLs as written.
pub(crate) fn urls_at(home: &Path, cwd: &Path) -> Vec<String> {
    let Some((_, common)) = git_dirs(cwd) else { return Vec::new() };
    if let Some(c) = read(home).remove(common.to_string_lossy().as_ref()) {
        return c.urls;
    }
    hashed(&config_urls(&std::fs::read_to_string(common.join("config")).unwrap_or_default()))
}

/// The marks (msg.rs `mark_of`) of the configs that give a clone's remote URLs: its own (`common`'s),
/// and the global ones git reads (`GIT_CONFIG_GLOBAL`, else `~/.gitconfig` and the XDG one). None when
/// the clone has no config.
fn marks(common: &Path) -> Option<String> {
    let mut out = crate::msg::mark_of(&common.join("config"))?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let globals: Vec<PathBuf> = match std::env::var_os("GIT_CONFIG_GLOBAL") {
        Some(g) => vec![PathBuf::from(g)],
        None => {
            let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".config")));
            home.iter().map(|h| h.join(".gitconfig")).chain(xdg.map(|x| x.join("git").join("config"))).collect()
        }
    };
    for g in globals {
        out.push(' ');
        out.push_str(&crate::msg::mark_of(&g).unwrap_or_default());
    }
    Some(out)
}

/// Records the clone `cwd` is in, at a session's first hook: the `stat`s of its configs (`marks`) when
/// it is recorded as they are; otherwise one `git remote -v` and the file rewritten under its lock.
/// Nothing for a directory in no clone, or a clone without a remote.
pub(crate) fn note(home: &Path, cwd: &str) {
    let Some((root, common)) = git_dirs(Path::new(cwd)) else { return };
    let Some(mark) = marks(&common) else { return };
    let key = common.to_string_lossy().into_owned();
    if read(home).get(&key).is_some_and(|c| c.mark == mark) {
        return;
    }
    let urls = hashed(&super::git::remote_urls(&root));
    if urls.is_empty() {
        return;
    }
    // A linked work tree's name is its clone's: the directory holding the common git directory.
    let main = if common.file_name().is_some_and(|n| n == ".git") { common.parent().unwrap_or(&root) } else { root.as_path() };
    let name = main.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let p = path(home);
    let _ = std::fs::create_dir_all(super::wal::dir(home));
    crate::live::locked(&p, || {
        let mut all = read(home);
        all.insert(key, Clone { name, urls, mark });
        crate::facts::write_atomic(&p, serde_json::to_string(&all).unwrap_or_default().as_bytes());
    });
}

/// The work trees of the clones recorded here that still exist (a bare one has none).
pub(crate) fn roots(home: &Path) -> Vec<PathBuf> {
    read(home)
        .into_keys()
        .map(PathBuf::from)
        .filter(|d| d.is_dir())
        .filter_map(|d| (d.file_name()? == ".git").then(|| d.parent().map(Path::to_path_buf)).flatten())
        .collect()
}

/// A clone recorded here of the repo `url` names (its URL hash among its remotes'), by its work tree.
pub(crate) fn clone_of(home: &Path, url: &str) -> Option<PathBuf> {
    let want = url_hash(url);
    let all = read(home);
    let dirs = all.iter().filter(|(_, c)| c.urls.contains(&want)).map(|(d, _)| PathBuf::from(d));
    dirs.filter(|d| d.is_dir()).find_map(|d| (d.file_name()? == ".git").then(|| d.parent().map(Path::to_path_buf)).flatten())
}

/// This machine as its presence tells the team (presence.rs `Machine`): its borg, the hand-off protocol
/// it speaks, and the clones recorded that still exist, by URL hash and name.
pub(crate) fn machine(home: &Path) -> crate::presence::Machine<'static> {
    let mut clones: Vec<crate::presence::Cloned> = read(home)
        .into_iter()
        .filter(|(dir, _)| Path::new(dir).is_dir())
        .flat_map(|(_, c)| c.urls.into_iter().map(move |url| crate::presence::Cloned { url: url.into(), name: c.name.clone().into() }))
        .collect();
    clones.sort_by(|a, b| (&a.url, &a.name).cmp(&(&b.url, &b.name)));
    clones.dedup_by(|a, b| a.url == b.url);
    crate::presence::Machine { borg: env!("CARGO_PKG_VERSION").into(), handoff: super::PROTOCOL, clones }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_gives_every_remote_url_and_nothing_else() {
        let cfg = "[core]\n\turl = not-a-remote\n[remote \"origin\"]\n\turl = git@github.com:acme/app.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n[remote \"fork\"]\n\turl = \"https://u:t@github.com/me/app\"\n\tpushurl = /srv/app.git\n[branch \"main\"]\n\tremote = origin\n";
        assert_eq!(config_urls(cfg), vec!["git@github.com:acme/app.git", "https://u:t@github.com/me/app", "/srv/app.git"]);
        assert_eq!(url_hash("git@github.com:acme/app.git"), url_hash("https://GitHub.com/acme/app"), "one repo, two URL forms");
        assert_ne!(url_hash("git@github.com:acme/app.git"), url_hash("git@github.com:acme/web.git"));
    }

    #[test]
    fn a_clone_and_its_work_tree_are_one_entry_and_a_gone_clone_is_not_told() {
        let d = std::env::temp_dir().join(format!("borg-handoff-clones-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let git = |dir: &Path, a: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(dir).args(a).output().unwrap().status.success());
        std::fs::create_dir_all(d.join("app")).unwrap();
        git(&d.join("app"), &["init", "-q", "-b", "main"]);
        git(&d.join("app"), &["remote", "add", "origin", "git@example.com:acme/app.git"]);
        // A shorthand git rewrites: the clone is recorded by the URL it resolves to.
        git(&d.join("app"), &["config", "url.git@example.com:.insteadOf", "ex:"]);
        git(&d.join("app"), &["remote", "add", "lib", "ex:acme/lib"]);
        git(&d.join("app"), &["-c", "user.email=a@x", "-c", "user.name=a", "commit", "-q", "--allow-empty", "-m", "x"]);
        git(&d.join("app"), &["worktree", "add", "-q", "-b", "wt", &d.join("app-wt").to_string_lossy()]);
        let home = d.join("store");
        note(&home, &d.join("app/src").to_string_lossy());
        note(&home, &d.join("app-wt").to_string_lossy());
        let m = machine(&home);
        let mut want = [url_hash("https://example.com/acme/app"), url_hash("git@example.com:acme/lib.git")];
        want.sort();
        let got: Vec<&str> = m.clones.iter().map(|c| c.url.as_ref()).collect();
        assert_eq!(got, want.iter().map(String::as_str).collect::<Vec<_>>(), "{m:?}");
        assert!(m.clones.iter().all(|c| c.name == "app"));
        assert_eq!(read(&home).len(), 1, "the work tree is its clone");
        assert_eq!(urls_at(&home, &d.join("app-wt")), want, "a recorded clone, as git resolves it");
        std::fs::remove_dir_all(d.join("app")).unwrap();
        assert!(machine(&home).clones.is_empty(), "a clone that is gone");
        let _ = std::fs::remove_dir_all(&d);
    }
}
