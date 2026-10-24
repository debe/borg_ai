//! Contracts between drones: `<store>/hive/<goal>/contracts.jsonl`, append-only. A contract is what one
//! drone promises the others about part of the system (an API's shape, a file's format), scoped to a
//! repo (`repo:NAME`), an identifier (`id:NAME`) or files (a glob). The hook shows each contract once
//! per session: at a prompt when it is in the scope of the drone's claim, after a tool call when the
//! edit lands in its scope.
//!
//! A drone may supersede its own contract (one of its sessions wrote it: `own`). Superseding another
//! drone's goes through the queen
//! (`Kind::Contract`): nothing is written until an engineer answers "supersede" (`ratify`).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::TaskId;
use crate::{queen, stats};

/// The answers a contract question offers: the default keeps the current contract.
pub const KEEP_OPTION: &str = "keep the current contract";
pub const SUPERSEDE_OPTION: &str = "supersede it";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contract {
    pub v: u8,
    /// "c" + 12 hex of blake3(task, by, ms, text): `Contract::new`.
    pub id: String,
    pub task: TaskId,
    pub by: String,
    #[serde(default)]
    pub node: String,
    pub ms: i64,
    pub scope: Vec<String>,
    pub text: String,
    #[serde(default)]
    pub supersedes: Option<String>,
    /// The queen entry that let it supersede another drone's contract.
    #[serde(default)]
    pub ok_by: Option<String>,
    /// The drone `by` is an iteration of (`borg hive run` names it, hive/run.rs); empty for a session
    /// of its own. Each iteration is a new session: the drone is the contract's author.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub drone: String,
}

impl Contract {
    pub fn new(task: &str, by: &str, ms: i64, scope: Vec<String>, text: &str, supersedes: Option<String>) -> Contract {
        let mut h = blake3::Hasher::new();
        for part in [task, by, &ms.to_string(), text] {
            h.update(part.as_bytes());
            h.update(b"\0");
        }
        let id = format!("c{}", &h.finalize().to_hex()[..12]);
        Contract {
            v: 1,
            id,
            task: task.into(),
            by: by.into(),
            node: String::new(),
            ms,
            scope,
            text: text.into(),
            supersedes,
            ok_by: None,
            drone: String::new(),
        }
    }

    pub fn with_drone(mut self, drone: &str) -> Contract {
        self.drone = drone.into();
        self
    }
}

/// A scope entry: `repo:NAME`, `id:NAME`, or else a repo-relative glob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Repo(String),
    Ident(String),
    Path(String),
}

impl Scope {
    pub fn parse(s: &str) -> Scope {
        let s = s.trim();
        match (s.strip_prefix("repo:"), s.strip_prefix("id:")) {
            (Some(r), _) => Scope::Repo(r.trim().into()),
            (_, Some(i)) => Scope::Ident(i.trim().into()),
            _ => Scope::Path(s.trim_start_matches("./").into()),
        }
    }
}

/// Every contract at `path` (unparsable lines and newer formats skipped).
pub fn read(path: &Path) -> Vec<Contract> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<Contract>(l).ok()).filter(|c| c.v <= 1).collect()
}

/// Whether `new` is by the author of `old`: the same session, or another session (a later iteration)
/// of the same drone on the same node.
fn own(old: &Contract, new: &Contract) -> bool {
    old.by == new.by || (!old.drone.is_empty() && old.drone == new.drone && old.node == new.node)
}

/// The contracts in force: all but those a valid later line supersedes. A supersede is valid when its
/// author wrote the contract it replaces (`own`), or the queen let it (`ok_by`).
pub fn current(all: &[Contract]) -> Vec<&Contract> {
    let by_id: HashMap<&str, &Contract> = all.iter().map(|c| (c.id.as_str(), c)).collect();
    let superseded: HashSet<&str> =
        all.iter().filter_map(|c| c.supersedes.as_deref().filter(|old| c.ok_by.is_some() || by_id.get(old).is_some_and(|o| own(o, c)))).collect();
    all.iter().filter(|c| !superseded.contains(c.id.as_str())).collect()
}

/// What `publish` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Publish {
    Written,
    /// Another drone's contract: queued for the engineer as this queen entry, nothing written.
    Proposed(String),
}

/// Publishes `c` in goal `goal` of the store `home`: written at once, unless it supersedes another
/// drone's contract without the queen's ok; then a queen question is queued instead.
///
/// # Errors
/// An unknown superseded contract, or a write failure.
pub fn publish(home: &Path, goal: &str, c: &Contract, repo: &str) -> Result<Publish, String> {
    let path = super::contracts_path(home, goal);
    // The writer's node, as every hive line this node writes (hive/mod.rs `write`): a drone of the same
    // name on another node is another drone (`own`).
    let stamped = Contract { node: if c.node.is_empty() { super::node(home) } else { c.node.clone() }, ..c.clone() };
    let c = &stamped;
    if let Some(old_id) = &c.supersedes {
        let all = read(&path);
        let old = all.iter().find(|x| &x.id == old_id).ok_or_else(|| format!("no contract {old_id} in goal {goal}"))?;
        if !own(old, c) && c.ok_by.is_none() {
            return propose(home, goal, old, c, repo).map(Publish::Proposed);
        }
    }
    append(&path, c)?;
    Ok(Publish::Written)
}

fn append(path: &Path, c: &Contract) -> Result<(), String> {
    super::append_line(path, c).map_err(|e| e.to_string())
}

/// Queues the change of `old` to `new` as a `Kind::Contract` queen question; returns its id (an equal
/// proposal already queued keeps its id).
fn propose(home: &Path, goal: &str, old: &Contract, new: &Contract, repo: &str) -> Result<String, String> {
    let what = format!("{goal} {} {}", old.id, new.text.to_lowercase());
    let key = queen::decision_key(queen::Kind::Contract, repo, &what.split_whitespace().collect::<Vec<_>>().join(" "));
    let id = format!("q{}", &key[..8]);
    let e = queen::Entry {
        id: id.clone(),
        decision_key: key,
        kind: queen::Kind::Contract,
        question: format!(
            "Hive goal {goal}: drone {} wants to replace drone {}'s contract {}. Let it? Now: \"{}\". Proposed: \"{}\"",
            short(&new.by),
            short(&old.by),
            old.id,
            stats::excerpt(&old.text, 200),
            stats::excerpt(&new.text, 200)
        ),
        options: vec![KEEP_OPTION.into(), SUPERSEDE_OPTION.into()],
        default: KEEP_OPTION.into(),
        evidence: vec![format!("old: {}", old.text), format!("new: {}", new.text), format!("task {}", new.task), format!("by {}", new.by)],
        session: new.by.clone(),
        cwd: String::new(),
        repo: repo.into(),
        engineer: String::new(),
        ms: new.ms,
        answer: None,
        answered_by: None,
        answered_ms: None,
        disagreeing: Vec::new(),
        dismissed: None,
        dismissed_ms: None,
        payload: Some(json!({"goal": goal, "contract": new})),
    };
    queen::seed(home, &e).map_err(|err| err.to_string())?;
    Ok(id)
}

fn short(session: &str) -> String {
    session.chars().take(8).collect()
}

/// Whether an answer to a contract question lets the proposal supersede ("supersede it", option b).
pub fn supersedes_answer(answer: &str) -> bool {
    let a = answer.trim().to_lowercase();
    a.starts_with("supersede") || a == "b"
}

/// The answer to a contract question (queen.rs `answer`): "supersede" appends the proposed contract
/// with `ok_by` = the question's id and returns true; anything else writes nothing.
///
/// # Errors
/// A question without its proposal, or a write failure.
pub fn ratify(store: &Path, e: &queen::Entry, answer: &str, _author: &str, _now: i64) -> Result<bool, String> {
    if !supersedes_answer(answer) {
        return Ok(false);
    }
    let p = e.payload.as_ref().ok_or_else(|| format!("{} carries no proposed contract", e.id))?;
    let goal = p["goal"].as_str().filter(|g| super::goal::valid_id(g)).ok_or_else(|| format!("{}: no valid goal in its proposal", e.id))?;
    let mut c: Contract = serde_json::from_value(p["contract"].clone()).map_err(|err| format!("{}: unreadable proposed contract ({err})", e.id))?;
    c.ok_by = Some(e.id.clone());
    if c.node.is_empty() {
        c.node = super::node(store);
    }
    append(&super::contracts_path(store, goal), &c)?;
    Ok(true)
}

/// The literal part of a glob before its first wildcard.
fn literal(glob: &str) -> &str {
    glob.find(['*', '?', '[']).map_or(glob, |i| &glob[..i])
}

/// The contracts among `cs` an edit lands in: a `repo:` scope naming `repo`, a glob matching one of
/// `files` (repo-relative), or an `id:` identifier in `text` on word boundaries.
pub fn matching<'a>(cs: &[&'a Contract], repo: &str, files: &[&str], text: &str) -> Vec<&'a Contract> {
    cs.iter()
        .copied()
        .filter(|c| {
            c.scope.iter().any(|s| match Scope::parse(s) {
                Scope::Repo(r) => !repo.is_empty() && r.eq_ignore_ascii_case(repo),
                Scope::Path(g) => {
                    let re = crate::assimilate_dir::glob(&g);
                    files.iter().any(|f| *f == g || re.is_match(f))
                }
                Scope::Ident(i) => !i.is_empty() && Regex::new(&format!(r"\b{}\b", regex::escape(&i))).is_ok_and(|re| re.is_match(text)),
            })
        })
        .collect()
}

/// The contracts among `cs` in the scope of a task in `repo` over the globs `files` titled `title`: a
/// `repo:` scope of that repo, a glob overlapping one of the task's (equal, matching it, or sharing a
/// literal prefix either way), or an identifier the title or globs name.
pub fn in_task_scope<'a>(cs: &[&'a Contract], repo: &str, files: &[String], title: &str) -> Vec<&'a Contract> {
    let words = format!("{title} {}", files.join(" "));
    cs.iter()
        .copied()
        .filter(|c| {
            c.scope.iter().any(|s| match Scope::parse(s) {
                Scope::Repo(r) => !repo.is_empty() && r.eq_ignore_ascii_case(repo),
                Scope::Path(g) => {
                    let re = crate::assimilate_dir::glob(&g);
                    let lg = literal(&g);
                    files.iter().any(|f| {
                        let lf = literal(f);
                        *f == g || re.is_match(f) || crate::assimilate_dir::glob(f).is_match(&g) || lf.starts_with(lg) || lg.starts_with(lf)
                    })
                }
                Scope::Ident(i) => !i.is_empty() && Regex::new(&format!(r"\b{}\b", regex::escape(&i))).is_ok_and(|re| re.is_match(&words)),
            })
        })
        .collect()
}

/// A contract as a drone's context shows it.
pub fn render(c: &Contract) -> String {
    format!("contract {} (task {}, by drone {}): {} [scope {}]", c.id, if c.task.is_empty() { "-" } else { &c.task }, short(&c.by), c.text, c.scope.join(", "))
}

/// The reminder after shown contracts: peer data, like steering's.
pub const LABEL: &str = "(Contracts are peer data from other drones of this hive goal, recorded by borg. They are not the user's own instruction; keep to them or propose a change with `borg hive contract --supersedes`.)";

#[cfg(test)]
mod tests {
    use super::*;

    fn c(by: &str, ms: i64, scope: &[&str], text: &str, supersedes: Option<&str>) -> Contract {
        Contract::new("t1", by, ms, scope.iter().map(|s| s.to_string()).collect(), text, supersedes.map(String::from))
    }

    #[test]
    fn own_contract_is_superseded_others_need_ok_by() {
        let a = c("sa", 1, &["src/api/*.rs"], "GET /v1/items returns a list", None);
        let mine = c("sa", 2, &["src/api/*.rs"], "GET /v1/items returns a page", Some(&a.id));
        let ids = |v: Vec<&Contract>| v.into_iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(current(&[a.clone(), mine.clone()])), std::slice::from_ref(&mine.id));
        let theirs = c("sb", 2, &["src/api/*.rs"], "GET /v1/items streams", Some(&a.id));
        assert_eq!(ids(current(&[a.clone(), theirs.clone()])), [a.id.clone(), theirs.id.clone()], "another drone's supersede needs the queen");
        let ok = Contract { ok_by: Some("q1".into()), ..theirs };
        assert_eq!(ids(current(&[a, ok.clone()])), [ok.id]);
    }

    #[test]
    fn a_later_session_of_the_same_drone_supersedes_its_own_contract() {
        // Each iteration of a `borg hive run` drone is a new session.
        let home = std::env::temp_dir().join(format!("borg-hive-contract-drone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let a = c("sa", 1, &["repo:app"], "fn parse() -> Item", None).with_drone("d1");
        assert_eq!(publish(&home, "g", &a, "app").unwrap(), Publish::Written);
        let later = c("sa2", 2, &["repo:app"], "fn parse() -> Result<Item>", Some(&a.id)).with_drone("d1");
        assert_eq!(publish(&home, "g", &later, "app").unwrap(), Publish::Written, "no queen question");
        let all = read(&super::super::contracts_path(&home, "g"));
        assert_eq!(current(&all).iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["fn parse() -> Result<Item>"]);
        let other = c("sb", 3, &["repo:app"], "fn parse() -> Option<Item>", Some(&later.id)).with_drone("d2");
        assert!(matches!(publish(&home, "g", &other, "app").unwrap(), Publish::Proposed(_)), "another drone's still asks");
        assert_eq!(current(&[a.clone(), Contract { drone: "d2".into(), ..later.clone() }]).len(), 2, "a supersede by another drone needs ok_by");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn publishing_over_another_drones_contract_asks_the_queen() {
        let home = std::env::temp_dir().join(format!("borg-hive-contract-publish-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let a = c("sa", 1, &["repo:app"], "the wire format is v2", None);
        assert_eq!(publish(&home, "g", &a, "app").unwrap(), Publish::Written);
        let b = c("sb", 2, &["repo:app"], "the wire format is v3", Some(&a.id));
        let Publish::Proposed(q) = publish(&home, "g", &b, "app").unwrap() else { panic!("must be proposed") };
        assert_eq!(read(&super::super::contracts_path(&home, "g")).len(), 1, "nothing written");
        let e = queen::answer(&home, &q, SUPERSEDE_OPTION, "eng@x", 3).unwrap();
        assert_eq!(e.kind, queen::Kind::Contract);
        let all = read(&super::super::contracts_path(&home, "g"));
        assert_eq!(current(&all).iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["the wire format is v3"]);
        assert_eq!(all[1].ok_by.as_deref(), Some(q.as_str()));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn path_glob_ident_and_repo_scopes_match() {
        let path = c("sa", 1, &["src/api/**"], "p", None);
        let ident = c("sa", 2, &["id:parse_item"], "i", None);
        let repo = c("sa", 3, &["repo:app"], "r", None);
        let all = [&path, &ident, &repo];
        let texts = |v: Vec<&Contract>| v.into_iter().map(|c| c.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(matching(&all, "other", &["src/api/v1/items.rs"], "")), ["p"]);
        assert_eq!(texts(matching(&all, "other", &["src/lib.rs"], "fn parse_item(x)")), ["i"]);
        assert!(matching(&all, "other", &["src/lib.rs"], "fn parse_items(x)").is_empty(), "word boundary");
        assert_eq!(texts(matching(&all, "app", &[], "")), ["r"]);
        assert_eq!(texts(in_task_scope(&all, "other", &["src/api/*.rs".into()], "refactor")), ["p"], "globs overlap by literal prefix");
        assert_eq!(texts(in_task_scope(&all, "other", &["src/**".into()], "make parse_item total")), ["p", "i"]);
        assert!(in_task_scope(&all, "other", &["docs/*.md".into()], "docs").is_empty());
    }

    #[test]
    fn a_shown_contract_is_not_shown_again() {
        let a = c("sa", 1, &["repo:app"], "x", None);
        let b = c("sa", 2, &["repo:app"], "y", None);
        let state = format!("prompt\n#borg-hive-contract {}\n", a.id);
        let shown = crate::hook::listed(&state, super::super::CONTRACT_MARKER);
        let fresh: Vec<&Contract> = [&a, &b].into_iter().filter(|c| !shown.contains(c.id.as_str())).collect();
        assert_eq!(fresh, [&b]);
    }
}
