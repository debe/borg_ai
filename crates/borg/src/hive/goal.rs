//! A hive goal: `<store>/hive/<goal>/goal.json`, one document written once (`create`) and never
//! rewritten in H1. It holds the spec, the oracle checks that gate `done`, the dollar budget (recorded
//! and shown, not enforced yet) and the repos in scope. `sig` stays empty until the mesh carries goals
//! (an admin's signature then; docs/design/collective.md "Goal").

use std::path::Path;

use serde::{Deserialize, Serialize};

fn default_timeout() -> u64 {
    600
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Goal {
    /// The format's version: 1.
    pub v: u8,
    pub id: String,
    pub spec: String,
    /// What "done" means: every check passes. A task's own `accept` replaces it.
    pub oracle: Vec<Check>,
    pub budget_usd: f64,
    /// Empty: any repo.
    #[serde(default)]
    pub repos: Vec<RepoScope>,
    /// The engineer who made the goal (git user.email).
    pub author: String,
    pub created_ms: i64,
    /// Empty until the mesh syncs goals: an admin's signature.
    #[serde(default)]
    pub sig: String,
    /// The held-out checks (hive/hidden.rs): never shown to a drone, run outside every worktree after
    /// a verdict. When goals sync (collective step 4) this field is stripped from the synced projection
    /// and stays on the runner's node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<Hidden>,
}

/// A goal's hidden oracle: `commands` run by `sh -c` in a scratch checkout of the work, with
/// `BORG_HIDDEN` naming `dir` (the held-out tests, outside every repo) and `BORG_WORK` the checkout.
/// Passed when every command exits 0 within `timeout_s` (clamped to `HIDDEN_TIMEOUT`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hidden {
    pub dir: String,
    pub commands: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
}

/// The bounds of a hidden command's timeout, in seconds.
pub const HIDDEN_TIMEOUT: std::ops::RangeInclusive<u64> = 30..=3600;

impl Hidden {
    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.timeout_s.clamp(*HIDDEN_TIMEOUT.start(), *HIDDEN_TIMEOUT.end()))
    }
}

/// Whether `h` may be a goal's hidden oracle: its dir absolute, an existing directory, and outside
/// `repo` (the repo the goal is made in, with every worktree `borg hive run` puts under it), so no
/// drone's checkout holds the held-out checks.
///
/// # Errors
/// What is wrong with it.
pub fn validate_hidden(h: &Hidden, repo: Option<&Path>) -> Result<(), String> {
    let dir = Path::new(&h.dir);
    if !dir.is_absolute() {
        return Err(format!("hidden oracle dir {} is relative: give an absolute path outside the repo", h.dir));
    }
    if !dir.is_dir() {
        return Err(format!("hidden oracle dir {} is not a directory", h.dir));
    }
    if h.commands.iter().all(|c| c.trim().is_empty()) {
        return Err("a hidden oracle needs at least one command".into());
    }
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if let Some(repo) = repo
        && real(dir).starts_with(real(repo))
    {
        return Err(format!("hidden oracle dir {} lies inside the repo {}: drones would see it", h.dir, repo.display()));
    }
    Ok(())
}

/// A repo in scope, by name as the hooks name it (`queen::repo_name`), and the node expected to hold it
/// (empty: this machine).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoScope {
    pub name: String,
    #[serde(default)]
    pub node: String,
}

/// One oracle check: `cmd` run by `sh -c` in `repo` (empty: the drone's directory), passed by exit 0
/// within `timeout_s`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub cmd: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
}

impl Check {
    /// `NAME=CMD` as `borg hive init --oracle` and `open --accept` take it; a bare command is its own name.
    pub fn parse(s: &str, timeout_s: u64) -> Result<Check, String> {
        let (name, cmd) = match s.split_once('=') {
            Some((n, c)) if !n.trim().is_empty() && !n.contains(char::is_whitespace) => (n.trim(), c.trim()),
            _ => (s.trim(), s.trim()),
        };
        if cmd.is_empty() {
            return Err(format!("empty oracle check {s:?}: give NAME=CMD"));
        }
        Ok(Check { name: name.into(), cmd: cmd.into(), repo: String::new(), timeout_s })
    }
}

/// A goal id is a path component and a line field: `[a-z0-9-]{1,40}`, nothing else.
pub fn valid_id(id: &str) -> bool {
    (1..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The goal `id` of the store `home`.
///
/// # Errors
/// An invalid id, or no readable goal.json.
pub fn load(home: &Path, id: &str) -> Result<Goal, String> {
    if !valid_id(id) {
        return Err(format!("invalid goal id {id:?}: use [a-z0-9-], at most 40"));
    }
    let raw = std::fs::read(super::goal_path(home, id)).map_err(|_| format!("no hive goal {id} in this collective (`borg hive init {id}`)"))?;
    serde_json::from_slice(&raw).map_err(|e| format!("goal {id}: unreadable goal.json ({e})"))
}

/// Writes `g` as a new goal: refused when the id exists, even when two `init`s race (the file is
/// linked into place, never renamed over another).
///
/// # Errors
/// An invalid or taken id, or a write failure.
pub fn create(home: &Path, g: &Goal) -> Result<(), String> {
    if !valid_id(&g.id) {
        return Err(format!("invalid goal id {:?}: use [a-z0-9-], at most 40", g.id));
    }
    let path = super::goal_path(home, &g.id);
    if path.exists() {
        return Err(format!("goal {} exists already", g.id));
    }
    let dir = path.parent().ok_or("no goal directory")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("goal.json.{}.tmp", std::process::id()));
    let body = serde_json::to_string_pretty(g).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body + "\n").map_err(|e| e.to_string())?;
    // A hard link fails when the target exists: create-new semantics for a whole document.
    let linked = std::fs::hard_link(&tmp, &path);
    let _ = std::fs::remove_file(&tmp);
    linked.map_err(|e| if e.kind() == std::io::ErrorKind::AlreadyExists { format!("goal {} exists already", g.id) } else { e.to_string() })
}

/// The goal ids of the store `home`, sorted.
pub fn list(home: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(home.join("hive"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("goal.json").is_file())
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|id| valid_id(id))
        .collect();
    out.sort();
    out
}

/// Whether a drone in `repo` works inside the goal's repos (always, when the goal names none).
pub fn in_scope(g: &Goal, repo: &str) -> bool {
    g.repos.is_empty() || g.repos.iter().any(|r| r.name.eq_ignore_ascii_case(repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("borg-hive-goal-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn goal(id: &str) -> Goal {
        Goal {
            v: 1,
            id: id.into(),
            spec: "s".into(),
            oracle: vec![Check::parse("t=true", 600).unwrap()],
            budget_usd: 1.0,
            repos: vec![],
            author: "a@x".into(),
            created_ms: 1,
            sig: String::new(),
            hidden: None,
        }
    }

    #[test]
    fn a_goal_is_created_once_and_listed() {
        let home = temp("create");
        create(&home, &goal("g-1")).unwrap();
        assert!(create(&home, &goal("g-1")).unwrap_err().contains("exists"));
        assert!(create(&home, &goal("../x")).is_err(), "path-safe ids only");
        assert_eq!(load(&home, "g-1").unwrap(), goal("g-1"));
        assert_eq!(list(&home), ["g-1"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_check_is_name_equals_command() {
        assert_eq!(Check::parse("unit=cargo test -q", 60).unwrap().cmd, "cargo test -q");
        assert_eq!(Check::parse("test -f ok", 60).unwrap().name, "test -f ok", "a bare command names itself");
        assert!(Check::parse("x=", 60).is_err());
    }

    #[test]
    fn a_hidden_oracle_lies_outside_the_repo() {
        let root = temp("hidden");
        let (repo, held) = (root.join("app"), root.join("held"));
        std::fs::create_dir_all(repo.join("tests")).unwrap();
        std::fs::create_dir_all(&held).unwrap();
        let h = |dir: &Path| Hidden { dir: dir.display().to_string(), commands: vec!["true".into()], timeout_s: 5 };
        assert!(validate_hidden(&h(&held), Some(&repo)).is_ok());
        assert!(validate_hidden(&h(&repo.join("tests")), Some(&repo)).unwrap_err().contains("inside the repo"));
        assert!(validate_hidden(&h(Path::new("held")), Some(&repo)).unwrap_err().contains("relative"));
        assert!(validate_hidden(&h(&root.join("gone")), Some(&repo)).unwrap_err().contains("not a directory"));
        assert!(validate_hidden(&Hidden { commands: vec![], ..h(&held) }, None).is_err());
        assert_eq!(h(&held).timeout(), std::time::Duration::from_secs(30), "clamped up to 30 s");
        // An old goal.json without `hidden` reads; a goal without one writes none.
        let mut g = goal("g-2");
        assert!(!serde_json::to_string(&g).unwrap().contains("hidden"));
        g.hidden = Some(h(&held));
        assert_eq!(serde_json::from_str::<Goal>(&serde_json::to_string(&g).unwrap()).unwrap(), g);
        let _ = std::fs::remove_dir_all(&root);
    }
}
