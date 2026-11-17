//! Which repos this machine holds clones of: for the check before an ask (`--to`, mod.rs), the notice
//! in a clone, and a take finding its clone (mod.rs `reach`).
//!
//! `<store>/handoff/clones.json` maps each clone's git directory (its common one: a linked work tree
//! is its clone) to its name and the keys of its remotes: `url_hash` (blake3 of git.rs `normal_url`) of
//! each URL as git resolves it (`insteadOf` and `pushInsteadOf` rules, the global config), the form the
//! offering side's `git remote get-url` gives too; and, beside them, of each URL with its ssh host alias
//! resolved (git.rs `resolved_url`), which an offer names as well. Both sides derive both keys, so a
//! match needs one shared key: an alias on either side, or a `Hostname` rewrite of a real host on one,
//! still finds the repo, and every key one machine derives another can derive from the same URL. No URL
//! leaves the machine.
//!
//! Two ways in. A session's first hook in a clone records it (`note`): one `git remote -v` when the
//! clone's config or the global one changed since, else the `stat`s of those files alone (an included
//! file's change is read at their next change); never ssh, which a hook does not wait for. And
//! `discover` records every clone in the paths the collective's config gives it, to
//! `collective::REPO_DEPTH` deep, whether a harness ever ran there or not, with the resolved keys (read
//! again once the ssh configs change): the mesh daemon runs it in the background, and a take that finds
//! no clone runs it once. What leaves the machine is `machine` (presence.rs `Machine`): the keys and
//! names of the clones that still exist, never a path.

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
    /// `url_hash` of every remote URL with its ssh host alias resolved, where that differs (`discover`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    resolved: Vec<String>,
    /// The ssh configs' marks (git.rs `ssh_mark`) when `resolved` was made; empty: never made.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    ssh: String,
}

impl Clone {
    /// Every key of the clone: its URLs' as git resolves them, and with ssh aliases resolved.
    fn keys(&self) -> impl Iterator<Item = &String> {
        self.urls.iter().chain(&self.resolved)
    }
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

/// The keys of a repo an offer names by `urls` (its URL, and the one its ssh alias stands for).
pub(crate) fn keys_of<'a>(urls: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut v: Vec<String> = urls.into_iter().map(url_hash).collect();
    v.sort();
    v.dedup();
    v
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
    keys_of(urls.iter().map(String::as_str))
}

/// The keys of the clone `cwd` is in: as recorded (git's resolved URLs, with ssh aliases resolved once
/// `discover` saw it), else, for a clone not recorded yet, its config's URLs as written. No process runs.
pub(crate) fn urls_at(home: &Path, cwd: &Path) -> Vec<String> {
    let Some((_, common)) = git_dirs(cwd) else { return Vec::new() };
    if let Some(c) = read(home).remove(common.to_string_lossy().as_ref()) {
        return c.keys().cloned().collect();
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
    let entry = entry(&root, &common, mark, false);
    if entry.urls.is_empty() {
        return;
    }
    write(home, |all| {
        all.insert(key, entry);
        true
    });
}

/// The record of the clone with work tree `root` and common git directory `common`, its configs read
/// at `mark`: one `git remote -v`, and with `resolve` each remote's ssh alias resolved (`ssh -G`, once
/// per host while the ssh configs stay). A clone without a remote has no keys.
fn entry(root: &Path, common: &Path, mark: String, resolve: bool) -> Clone {
    let raw = super::git::remote_urls(root);
    let urls = hashed(&raw);
    let (resolved, ssh) = if resolve {
        let ssh = super::git::ssh_mark();
        let other: Vec<String> = raw.iter().map(|u| super::git::resolved_url(u)).filter(|r| !raw.contains(r)).collect();
        (hashed(&other).into_iter().filter(|k| !urls.contains(k)).collect(), ssh)
    } else {
        (Vec::new(), String::new())
    };
    // A linked work tree's name is its clone's: the directory holding the common git directory.
    let main = if common.file_name().is_some_and(|n| n == ".git") { common.parent().unwrap_or(root) } else { root };
    let name = main.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Clone { name, urls, mark, resolved, ssh }
}

/// Changes the file under its lock with `change`, which says whether it changed anything; written
/// only then. Returns what `change` said.
fn write(home: &Path, change: impl FnOnce(&mut BTreeMap<String, Clone>) -> bool) -> bool {
    let p = path(home);
    let _ = std::fs::create_dir_all(super::wal::dir(home));
    crate::live::locked(&p, || {
        let mut all = read(home);
        let changed = change(&mut all);
        if changed {
            crate::facts::write_atomic(&p, serde_json::to_string(&all).unwrap_or_default().as_bytes());
        }
        changed
    })
}

/// Records every clone in the paths `cfg` gives the collectives whose store is `home`, whether a
/// session ever ran in it or not, and forgets the recorded ones that are gone. Returns whether it
/// recorded a clone it did not know before. A path's clones: the path itself when it is one, else the
/// directories in it that are one, to `collective::REPO_DEPTH` levels (hidden ones skipped, as
/// `collective::places_of` does); each kept only when its route is `home` (a nested path of another
/// collective, or an `off` one, is not this collective's). A clone recorded as its configs are costs
/// their `stat`s alone; nothing is written while nothing changed.
pub(crate) fn discover(home: &Path, cfg: &crate::collective::Config) -> bool {
    use crate::collective::{self, Route};
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if dir.join(".git").exists() {
            out.push(dir.to_path_buf());
            return;
        }
        if depth == 0 {
            return;
        }
        for d in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if !d.file_name().to_string_lossy().starts_with('.') && d.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&d.path(), depth - 1, out);
            }
        }
    }
    let base = collective::root_of(home);
    let mut found = Vec::new();
    for c in cfg.collectives.iter().filter(|c| collective::store(&base, &c.name) == home) {
        for p in &c.paths {
            walk(&collective::expand(p), collective::REPO_DEPTH, &mut found);
        }
    }
    let ours = |d: &Path| matches!(collective::route(Some(cfg), &d.to_string_lossy(), &base), Route::Collective { home: h, .. } if h == home);
    let mut seen: Vec<(PathBuf, PathBuf, String)> = found
        .iter()
        .filter(|d| ours(d))
        .filter_map(|d| {
            let (root, common) = git_dirs(d)?;
            let mark = marks(&common)?;
            Some((root, common, mark))
        })
        .collect();
    seen.sort_by(|a, b| a.1.cmp(&b.1));
    seen.dedup_by(|a, b| a.1 == b.1);
    let held = read(home);
    let ssh = super::git::ssh_mark();
    let stale: Vec<&(PathBuf, PathBuf, String)> =
        seen.iter().filter(|(_, common, mark)| held.get(common.to_string_lossy().as_ref()).is_none_or(|c| c.mark != *mark || c.ssh != ssh)).collect();
    let gone = held.keys().any(|d| !Path::new(d).is_dir());
    if stale.is_empty() && !gone {
        return false;
    }
    // `git remote -v` and `ssh -G` run outside the lock; what they read is written only while the clone's
    // configs are still as they were then (a hook's `note` meanwhile read them as they are now). A clone
    // without a remote is recorded too, without keys: asked again only once its config changes.
    let fresh: Vec<(PathBuf, Clone)> = stale.into_iter().map(|(root, common, mark)| (common.clone(), entry(root, common, mark.clone(), true))).collect();
    let mut new = false;
    write(home, |all| {
        let before = all.len();
        all.retain(|d, _| Path::new(d).is_dir());
        let mut changed = all.len() != before;
        for (common, c) in fresh {
            if marks(&common).as_deref() != Some(c.mark.as_str()) {
                continue;
            }
            let key = common.to_string_lossy().into_owned();
            new |= !c.urls.is_empty() && !all.contains_key(&key);
            all.insert(key, c);
            changed = true;
        }
        changed
    });
    new
}

/// The work trees of the clones recorded here with a remote that still exist (a bare one has none).
pub(crate) fn recorded(home: &Path) -> Vec<PathBuf> {
    read(home)
        .into_iter()
        .filter(|(_, c)| !c.urls.is_empty())
        .map(|(d, _)| PathBuf::from(d))
        .filter(|d| d.is_dir())
        .filter_map(|d| (d.file_name()? == ".git").then(|| d.parent().map(Path::to_path_buf)).flatten())
        .collect()
}

/// The clones recorded here of the repo with the keys `keys` (`keys_of`): one shared key, by work tree.
pub(crate) fn clones_of(home: &Path, keys: &[String]) -> Vec<PathBuf> {
    read(home)
        .into_iter()
        .filter(|(_, c)| c.keys().any(|k| keys.contains(k)))
        .map(|(d, _)| PathBuf::from(d))
        .filter(|d| d.is_dir())
        .filter_map(|d| (d.file_name()? == ".git").then(|| d.parent().map(Path::to_path_buf)).flatten())
        .collect()
}

/// This machine as its presence tells the team (presence.rs `Machine`): its borg, the hand-off protocol
/// it speaks, and the clones recorded that still exist, by key (both kinds: an older borg reads them as
/// URL hashes) and name.
pub(crate) fn machine(home: &Path) -> crate::presence::Machine<'static> {
    let mut clones: Vec<crate::presence::Cloned> = read(home)
        .into_iter()
        .filter(|(dir, _)| Path::new(dir).is_dir())
        .flat_map(|(_, c)| {
            let name = c.name;
            c.urls.into_iter().chain(c.resolved).map(move |url| crate::presence::Cloned { url: url.into(), name: name.clone().into() })
        })
        .collect();
    clones.sort_by(|a, b| (&a.url, &a.name).cmp(&(&b.url, &b.name)));
    clones.dedup_by(|a, b| a.url == b.url);
    crate::presence::Machine { borg: env!("CARGO_PKG_VERSION").into(), handoff: super::PROTOCOL, clones }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keys match across machines whatever ssh config each has: the offer names its URL and the one its
    /// alias stands for, a clone keeps its URLs and their resolved forms, one shared key is a match. A
    /// `Hostname` rewrite of a real host on one side (port 443) leaves the plain key on both.
    #[test]
    fn keys_match_across_machines_whatever_ssh_config_each_has() {
        let shared = |a: &[String], b: &[String]| a.iter().any(|k| b.contains(k));
        let offer_alias = keys_of(["gh-work:team/app.git", "git@github.com:team/app.git"]);
        let offer_443 = keys_of(["git@github.com:team/app.git", "ssh://git@ssh.github.com:443/team/app.git"]);
        let taker_plain = keys_of(["https://github.com/team/app"]);
        let taker_alias = keys_of(["github.com-me:team/app", "git@github.com:team/app"]);
        let retired = keys_of(["git@github.com:team/old-app.git"]);
        for offer in [&offer_alias, &offer_443] {
            assert!(shared(offer, &taker_plain) && shared(offer, &taker_alias), "{offer:?}");
            assert!(!shared(offer, &retired), "another repo of the same history is no match");
        }
    }

    /// Discovery records the clones in this collective's paths that no session ever opened: one in the
    /// path, one a group deeper, a work tree as its clone; not one deeper still, hidden, without a remote,
    /// in another collective's nested path or an `off` one. A clone that is gone is forgotten.
    #[test]
    fn discovery_records_unopened_clones_of_this_collective_only() {
        let d = std::env::temp_dir().join(format!("borg-handoff-discover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let git = |dir: &Path, a: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(dir).args(a).output().unwrap().status.success());
        let w = d.join("work");
        let clone = |rel: &str, remote: Option<&str>| {
            let p = w.join(rel);
            std::fs::create_dir_all(&p).unwrap();
            git(&p, &["init", "-q", "-b", "main"]);
            if let Some(r) = remote {
                git(&p, &["remote", "add", "origin", r]);
            }
            p
        };
        let app = clone("app", Some("git@example.com:acme/app.git"));
        clone("group/lib", Some("git@example.com:acme/lib.git"));
        clone("a/b/deep", Some("git@example.com:acme/deep.git"));
        clone(".hidden/x", Some("git@example.com:acme/hidden.git"));
        clone("loose", None);
        clone("oss/tool", Some("git@example.com:oss/tool.git"));
        clone("secret/key", Some("git@example.com:acme/key.git"));
        git(&app, &["-c", "user.email=a@x", "-c", "user.name=a", "commit", "-q", "--allow-empty", "-m", "x"]);
        git(&app, &["worktree", "add", "-q", "-b", "wt", &w.join("app-wt").to_string_lossy()]);
        let cfg: crate::collective::Config = serde_json::from_value(serde_json::json!({
            "default": "off",
            "off": [w.join("secret")],
            "collectives": [{"name": "corp", "paths": [w], "share": "team"}, {"name": "oss", "paths": [w.join("oss")], "share": "team"}]
        }))
        .unwrap();
        let home = crate::collective::store(&d.join("base"), "corp");
        assert!(discover(&home, &cfg), "new clones");
        let keyed: Vec<String> = read(&home).into_values().filter(|c| !c.urls.is_empty()).map(|c| c.name).collect();
        assert_eq!(keyed, ["app", "lib"], "{:?}", read(&home));
        assert!(read(&home).values().any(|c| c.name == "loose" && c.urls.is_empty()), "a clone without a remote is kept, keyless, so it is not asked again");
        assert!(machine(&home).clones.iter().all(|c| c.name != "loose"), "and is never told");
        // A clone is recorded by its real path (macOS's temp dir is behind the /var symlink).
        assert_eq!(clones_of(&home, &keys_of(["ssh://git@example.com/acme/lib"])), [std::fs::canonicalize(w.join("group/lib")).unwrap()]);
        assert!(!discover(&home, &cfg), "nothing new: nothing written");
        std::fs::remove_dir_all(w.join("group")).unwrap();
        assert!(!discover(&home, &cfg), "a gone clone is no new one");
        assert_eq!(read(&home).into_values().filter(|c| !c.urls.is_empty()).map(|c| c.name).collect::<Vec<_>>(), ["app"], "and is forgotten");
        let _ = std::fs::remove_dir_all(&d);
    }

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
