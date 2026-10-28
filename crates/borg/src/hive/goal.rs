//! A hive goal: `<store>/hive/<goal>/goal.json`, one document written once (`create`) and never
//! rewritten. It holds the spec, the oracle checks that gate `done`, the dollar budget (recorded and
//! shown, not enforced yet) and the repos in scope.
//!
//! Goals sync over the mesh as `goal.jsonl` (hive.md C4): one compact line per goal, its
//! projection (`projection`: the hidden oracle stripped, `held_out` saying one exists), appended by
//! `publish` (hive/mod.rs) on the node that made it. A node without goal.json loads the goal from that
//! log ([`load`]). `sig` stays empty: every line is signed by the member that first stamped it (borg-mesh
//! origin.rs), the trusted-member model. Two definitions of one id stop the goal on every node
//! ([`conflict`]), and a synced goal's oracle runs only once this node's engineer approved it (oracle.rs
//! `Approvals`). Whether only an admin's goal should sync is an open question (docs/design/hive.md
//! "Goal").

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
    /// Empty: lines are signed by their mesh origin (module doc).
    #[serde(default)]
    pub sig: String,
    /// The held-out checks (hive/hidden.rs): never shown to a drone, run outside every worktree after
    /// a verdict. Stripped from the synced projection: it stays on the runner's node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<Hidden>,
    /// A hidden oracle exists and stays on its runner's node (projection only): a verdict elsewhere
    /// waits for that node's score.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held_out: bool,
}

/// Whether `g` is scored by a hidden oracle: here (`hidden`), or on the node that made it (`held_out`).
pub fn has_hidden(g: &Goal) -> bool {
    g.hidden.is_some() || g.held_out
}

/// The goal as it syncs: no hidden oracle, `held_out` when it has one, its spec redacted (notes.rs
/// `redact`, as presence prompts); one compact line, the same for the same goal (so `publish` appends
/// it once).
pub fn projection(g: &Goal) -> String {
    serde_json::to_string(&shared(g)).unwrap_or_default()
}

/// `g` as other nodes hold it: what `projection` writes.
fn shared(g: &Goal) -> Goal {
    Goal { hidden: None, held_out: has_hidden(g), spec: crate::notes::redact(&g.spec), ..g.clone() }
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
/// `repo` (the repo the goal is made in, with every worktree `borg debug hive run` puts under it), so no
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

/// The goal `id` of the store `home`: its goal.json when this node made it, else the goal a peer synced
/// (`synced`). Refused while its definitions conflict ([`conflict`]): drones of two definitions would
/// work one board against different specs and oracles.
///
/// # Errors
/// An invalid id, no readable goal.json and no synced line of it, or a conflict.
pub fn load(home: &Path, id: &str) -> Result<Goal, String> {
    let g = read(home, id)?;
    match conflict(home, id) {
        Some(why) => Err(why),
        None => Ok(g),
    }
}

/// [`load`] without the conflict check: what the board and borg-web show of a conflicting goal.
///
/// # Errors
/// An invalid id, or no readable goal.json and no synced line of it.
pub fn read(home: &Path, id: &str) -> Result<Goal, String> {
    if !valid_id(id) {
        return Err(format!("invalid goal id {id:?}: use [a-z0-9-], at most 40"));
    }
    match std::fs::read(super::goal_path(home, id)) {
        Ok(raw) => serde_json::from_slice(&raw).map_err(|e| format!("goal {id}: unreadable goal.json ({e})")),
        Err(_) => synced(home, id).ok_or_else(|| format!("no hive goal {id} in this collective (`borg hive init {id}`)")),
    }
}

/// The goal.json of this node, when it made goal `id`.
pub fn local(home: &Path, id: &str) -> Option<Goal> {
    valid_id(id).then(|| std::fs::read(super::goal_path(home, id)).ok().and_then(|raw| serde_json::from_slice(&raw).ok())).flatten()
}

/// Why goal `id` has more than one definition, if it does: goal.jsonl (with this node's goal.json, as
/// its projection) holds two different projections of the id. Two nodes that `init` one id before a
/// sync round, or a member's line naming a goal it did not make, end there; each node would otherwise
/// load its own. The goal stays refused on every node until one definition is left (a revoked member's
/// lines are quarantined), and the board and borg-web say why.
pub fn conflict(home: &Path, id: &str) -> Option<String> {
    let text = std::fs::read_to_string(super::goal_jsonl_path(home, id)).unwrap_or_default();
    let shown = |g: &Goal| Goal { sig: String::new(), ..shared(g) };
    let mut defs: Vec<Goal> = local(home, id).iter().map(shown).collect();
    for g in text.lines().filter_map(|l| serde_json::from_str::<Goal>(l).ok()).filter(|g| g.v <= 1 && g.id == id) {
        let g = shown(&g);
        if !defs.contains(&g) {
            defs.push(g);
        }
    }
    (defs.len() > 1).then(|| {
        let who: Vec<String> = defs.iter().map(|g| format!("by {} at {}", if g.author.is_empty() { "?" } else { &g.author }, g.created_ms)).collect();
        format!(
            "goal {id} has {} conflicting definitions ({}): it stays stopped until one is left; make the goal again under another id",
            defs.len(),
            who.join(", ")
        )
    })
}

/// The goal `id` of goal.jsonl: of its lines naming that id (a line naming another cannot redirect the
/// goal), the one with the lowest (created_ms, blake3 of the line), the same on every node. More than
/// one definition is a [`conflict`], which `load` refuses.
fn synced(home: &Path, id: &str) -> Option<Goal> {
    let text = std::fs::read_to_string(super::goal_jsonl_path(home, id)).ok()?;
    text.lines()
        .filter_map(|l| {
            serde_json::from_str::<Goal>(l).ok().filter(|g| g.v <= 1 && g.id == id).map(|g| ((g.created_ms, *blake3::hash(l.as_bytes()).as_bytes()), g))
        })
        .min_by_key(|(k, _)| *k)
        .map(|(_, g)| Goal { hidden: None, ..g })
}

/// Whether the store holds goal `id`: made here, or synced from a peer.
pub fn exists(home: &Path, id: &str) -> bool {
    valid_id(id) && (super::goal_path(home, id).is_file() || super::goal_jsonl_path(home, id).is_file())
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
    if path.exists() || super::goal_jsonl_path(home, &g.id).exists() {
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

/// The goal ids of the store `home` (made here or synced), sorted.
pub fn list(home: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(home.join("hive"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("goal.json").is_file() || e.path().join("goal.jsonl").is_file())
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
            held_out: false,
        }
    }

    #[test]
    fn a_synced_goal_loads_without_goal_json_and_never_carries_hidden() {
        let (home, peer) = (temp("synced-a"), temp("synced-b"));
        let held = temp("synced-held");
        std::fs::create_dir_all(&held).unwrap();
        let mut g = goal("g-s");
        g.hidden = Some(Hidden { dir: held.display().to_string(), commands: vec!["true".into()], timeout_s: 30 });
        create(&home, &g).unwrap();
        assert_eq!(crate::hive::publish(&home), 1);
        assert_eq!(crate::hive::publish(&home), 0, "idempotent");
        let line = std::fs::read_to_string(crate::hive::goal_jsonl_path(&home, "g-s")).unwrap();
        assert_eq!(line.lines().count(), 1);
        assert!(line.contains("\"held_out\":true") && !line.contains("hidden") && !line.contains(&held.display().to_string()), "{line}");
        // A peer holds only the synced line (and a forged one naming another goal in it).
        let dir = crate::hive::dir(&peer, "g-s");
        std::fs::create_dir_all(&dir).unwrap();
        let forged = serde_json::to_string(&Goal { id: "g-x".into(), created_ms: 0, ..goal("g-x") }).unwrap();
        std::fs::write(dir.join("goal.jsonl"), format!("{forged}\n{line}")).unwrap();
        let got = load(&peer, "g-s").unwrap();
        assert_eq!((got.id.as_str(), got.held_out, got.hidden.is_none(), got.spec.as_str()), ("g-s", true, true, "s"));
        assert!(has_hidden(&got) && list(&peer) == ["g-s"] && exists(&peer, "g-s"));
        assert!(create(&peer, &goal("g-s")).unwrap_err().contains("exists"), "a synced goal is not made again");
        for d in [home, peer, held] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn a_secret_in_the_spec_stays_on_the_node_that_made_the_goal() {
        let home = temp("secret");
        let g = Goal { spec: "Fix the deploy\nDATABASE_URL=postgres://admin:hunter2@db/app".into(), ..goal("g-p") };
        create(&home, &g).unwrap();
        crate::hive::publish(&home);
        let line = std::fs::read_to_string(crate::hive::goal_jsonl_path(&home, "g-p")).unwrap();
        assert!(!line.contains("hunter2"), "{line}");
        assert_eq!(local(&home, "g-p").unwrap().spec, g.spec, "this node's goal.json keeps it");
        assert!(conflict(&home, "g-p").is_none(), "its own projection is one definition");
        let _ = std::fs::remove_dir_all(home);
    }

    /// Two nodes that made one id before a sync round, or a member's line naming a goal it did not make
    /// (an earlier `created_ms` would have won `synced`), leave two definitions: every node refuses the
    /// goal and says why, the one holding goal.json too. One definition, written twice, is no conflict.
    #[test]
    fn two_definitions_of_one_goal_stop_it_on_every_node() {
        let (a, c) = (temp("conflict-a"), temp("conflict-c"));
        let mine = goal("g-c");
        create(&a, &mine).unwrap();
        crate::hive::publish(&a);
        let line = std::fs::read_to_string(crate::hive::goal_jsonl_path(&a, "g-c")).unwrap();
        assert!(load(&a, "g-c").is_ok() && conflict(&a, "g-c").is_none());
        let theirs = projection(&Goal { spec: "another".into(), author: "b@x".into(), created_ms: 0, ..goal("g-c") });
        std::fs::write(crate::hive::goal_jsonl_path(&a, "g-c"), format!("{line}{theirs}\n{line}")).unwrap();
        let err = load(&a, "g-c").unwrap_err();
        assert!(err.contains("2 conflicting definitions") && err.contains("by a@x at 1") && err.contains("by b@x at 0"), "{err}");
        assert_eq!(read(&a, "g-c").unwrap(), mine, "read still shows this node's");
        // A node holding only the synced lines refuses it as well.
        std::fs::create_dir_all(crate::hive::dir(&c, "g-c")).unwrap();
        std::fs::write(crate::hive::goal_jsonl_path(&c, "g-c"), format!("{theirs}\n{line}")).unwrap();
        assert!(load(&c, "g-c").unwrap_err().contains("conflicting"));
        std::fs::write(crate::hive::goal_jsonl_path(&c, "g-c"), format!("{line}{line}")).unwrap();
        assert_eq!(load(&c, "g-c").unwrap().spec, "s", "the same definition twice");
        for d in [a, c] {
            let _ = std::fs::remove_dir_all(d);
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
