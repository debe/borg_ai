//! A collective's append-only logs as sets of content-addressed lines.
//!
//! Every line of `lessons.jsonl` is addressed by its blake3 hash, so two
//! nodes compare what they hold without reading it, and a merge is a plain union: appending lines
//! the store lacks never rewrites or loses a line. Only complete JSON lines count, so a line a
//! hook is appending right now is picked up by the next sync rather than copied half-written.
//!
//! Lessons are versioned in place (a consolidation appends the same id with `version + 1`) and
//! `conscience::load` keeps the *last* line per id. After a union the last line must therefore be
//! the winning version on every node, whatever order the lines arrived in: highest version, ties
//! (two nodes consolidated the same lesson concurrently) broken by the higher hash. `settle`
//! re-appends a winner that is not last; that repeats an existing line, so the hash set (and what
//! peers see) is unchanged.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::Path;

use serde_json::Value;

/// The logs a mesh syncs, relative to the collective's store.
///
/// Lessons, and the daily usage rollups (borg stats.rs): versioned records like lessons, one bucket per
/// (node, engineer, day), counts only. Not episodes: a lesson already carries its `episodes` ids and `engineers`; the raw episodes stay
/// on the node that captured them, so exactly one node learns each episode. Synced episodes were
/// pending on every node (learn.rs skips only episodes cited by a lesson or rejected locally), so
/// each member paid a model call per peer episode, and two members could learn the same correction
/// into two lessons under different ids. Episodes also hold the engineer's raw words.
///
/// And the team's outcomes (borg outcome.rs `publish`): redacted lines, one per result, with the kind,
/// lesson ids, time, asking propensity, hashed session and repo, and the node; never raw sessions,
/// repos or text. Plain lines (no `id` + `version`), so `settle` leaves them alone; signed and
/// quarantined like every synced log.
///
/// And the team's L1 playbooks (borg playbook.rs): promoted edits, each a delta to one step, folded
/// order-independently by every reader, so a union is all a merge needs. Plain lines too (`pb` +
/// `edit`, no `id` + `version`).
///
/// And the ratified constraints (borg veto.rs `ratify`): standing lessons every member's sessions get
/// (conscience.rs `standing`), lines holding what a lesson does (the redacted invariant, its lesson,
/// who ratified it). Plain lines (no `version`); the conscience keeps the earliest ratification per id,
/// so arrival order does not matter.
pub const SYNCED: [&str; 5] = ["lessons.jsonl", "stats.jsonl", borg::outcome::TEAM_FILE, borg::playbook::FILE, borg::veto::FILE];

/// Redacted reasoning excerpts behind git notes (`borg notes`): replicated by their author to
/// `sync::REPLICAS` members, not reconciled with everyone (sync.rs, step 4).
pub const REASONING: &str = "reasoning.jsonl";

pub type Hash = [u8; 32];

/// The logs whose lines carry origins, in `home`: the synced ones, every hive goal's (`hive_logs`) and
/// reasoning. Stamping (origin.rs `stamp`), quarantine (`origin::apply`) and the daemon's change
/// fingerprint all cover what this lists. A quarantine rewrites a file: a hive session's board cursor
/// past the new end reads from 0 (borg hive/board.rs `read_from`), so it may be shown changes again once.
pub fn logs(home: &Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = SYNCED.iter().map(|f| home.join(f)).collect();
    out.extend(hive_logs(home));
    out.push(home.join(REASONING));
    out
}

/// The hive's synced logs in `home` (borg hive/mod.rs `SYNCED`: goal projection, board, contracts,
/// messages, guard flags, bundles of commits, spend): each existing one of every goal directory whose
/// name is a valid goal id (`goal::valid_id`), never a goal's local files (goal.json, oracle.jsonl,
/// joins, runs, unbundled.jsonl).
pub fn hive_logs(home: &Path) -> Vec<std::path::PathBuf> {
    let mut ids: Vec<String> = std::fs::read_dir(home.join("hive"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|id| borg::hive::goal::valid_id(id))
        .collect();
    ids.sort();
    ids.iter().flat_map(|id| hive_files(home, id)).filter(|p| p.is_file()).collect()
}

/// Where goal `id`'s synced logs live in `home`, in `SYNCED` order (existing or not).
pub fn hive_files(home: &Path, id: &str) -> Vec<std::path::PathBuf> {
    borg::hive::SYNCED.iter().map(|f| borg::hive::dir(home, id).join(f)).collect()
}

/// Whether `line` of `path` stays on this node: a hive goal's spend line that names no node (borg
/// hive/run.rs `Spend::owner`: one from before spend lines named their node, which only this node
/// can have written, since spend did not sync then). Never stamped (origin.rs `stamp`), so never
/// offered: on a peer it would read as the peer's own spend.
pub fn local(path: &Path, line: &str) -> bool {
    path.file_name().and_then(|n| n.to_str()) == Some(borg::hive::run::SPEND_FILE)
        && serde_json::from_str::<Value>(line).ok().is_none_or(|v| v.get("node").and_then(Value::as_str).is_none_or(str::is_empty))
}

pub fn hash(line: &str) -> Hash {
    *blake3::hash(line.as_bytes()).as_bytes()
}

fn complete(line: &str) -> bool {
    !line.trim().is_empty() && serde_json::from_str::<Value>(line).is_ok()
}

/// The longest line of `path` that counts: a hive goal's bundles (borg hive/commits.rs `MAX_LINE`), else
/// any. A longer line is treated as a torn one: never stamped, offered, sent or stored.
pub fn cap(path: &Path) -> usize {
    match path.file_name().and_then(|n| n.to_str()) {
        Some(borg::hive::commits::FILE) => borg::hive::commits::MAX_LINE,
        _ => usize::MAX,
    }
}

/// The distinct complete lines of `path` by hash (empty if the file does not exist), none over its `cap`.
pub fn read(path: &Path) -> BTreeMap<Hash, String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let max = cap(path);
    text.lines().filter(|l| l.len() <= max && complete(l)).map(|l| (hash(l), l.to_string())).collect()
}

/// One hash for the whole set: equal digests mean there is nothing to exchange.
pub fn digest(set: &BTreeMap<Hash, String>) -> Hash {
    let mut h = blake3::Hasher::new();
    for k in set.keys() {
        h.update(k);
    }
    *h.finalize().as_bytes()
}

/// Appends the `incoming` lines `path` lacks (none over its `cap`), then settles lesson versions.
/// A hive board's new lines are noted as on this node from now (borg hive/board.rs `note_held`, under
/// the board's lock, as a quarantine's fresh index is): the receive time its leases and liveness are
/// timed from, on this node's clock. Returns how many lines were new.
pub fn merge(path: &Path, incoming: &[String]) -> Result<usize, String> {
    let mut known = read(path);
    let max = cap(path);
    let new: Vec<&String> = incoming.iter().filter(|l| l.len() <= max && complete(l) && known.insert(hash(l), (*l).clone()).is_none()).collect();
    append(path, &new)?;
    if !new.is_empty() && path.file_name().is_some_and(|f| f == "tasks.jsonl") {
        borg::live::locked(path, || borg::hive::board::note_held(path));
    }
    settle(path)?;
    Ok(new.len())
}

fn append(path: &Path, lines: &[&String]) -> Result<(), String> {
    if lines.is_empty() {
        return Ok(());
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let ends_open = std::fs::read(path).map(|b| b.last().is_some_and(|c| *c != b'\n')).unwrap_or(false);
    let mut out = String::from(if ends_open { "\n" } else { "" });
    for l in lines {
        out.push_str(l);
        out.push('\n');
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(out.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

/// Makes the winning version of every versioned record the last line for its id (module doc).
/// Also after lines were removed (quarantine): the line that is now last may be an older version.
pub fn settle(path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut winner: HashMap<String, ((u64, Hash), &str)> = HashMap::new();
    let mut last: HashMap<String, Hash> = HashMap::new();
    for l in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(l) else { continue };
        let (Some(id), Some(version)) = (v["id"].as_str(), v["version"].as_u64()) else { continue };
        let rank = (version, hash(l));
        last.insert(id.to_string(), rank.1);
        if winner.get(id).is_none_or(|(r, _)| rank > *r) {
            winner.insert(id.to_string(), (rank, l));
        }
    }
    let mut stale: Vec<(&String, String)> = winner.iter().filter(|(id, (r, _))| last.get(*id) != Some(&r.1)).map(|(id, (_, l))| (id, l.to_string())).collect();
    stale.sort();
    append(path, &stale.iter().map(|(_, l)| l).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lesson(id: &str, version: u32, q: &str) -> String {
        serde_json::json!({"id": id, "origin_ms": 1, "question": q, "version": version}).to_string()
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("borg-mesh-lines-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn hive_logs_list_each_goals_synced_files_and_skip_invalid_ids() {
        let d = tmp("hive-logs");
        for (dir, files) in [
            (
                "g1",
                &[
                    "goal.json",
                    "goal.jsonl",
                    "tasks.jsonl",
                    "oracle.jsonl",
                    "joins.jsonl",
                    "flags.jsonl",
                    "bundles.jsonl",
                    "unbundled.jsonl",
                    "spend.jsonl",
                    "runs.jsonl",
                ][..],
            ),
            ("A", &["tasks.jsonl"][..]),
            ("g2", &["messages.jsonl"][..]),
        ] {
            std::fs::create_dir_all(d.join("hive").join(dir)).unwrap();
            for f in files {
                std::fs::write(d.join("hive").join(dir).join(f), "{}\n").unwrap();
            }
        }
        // A directory named like a path escape never lists (it cannot be created under hive/ anyway).
        std::fs::create_dir_all(d.join("hive/..x")).unwrap();
        std::fs::write(d.join("hive/..x/tasks.jsonl"), "{}\n").unwrap();
        let got: Vec<String> = hive_logs(&d).iter().map(|p| p.strip_prefix(&d).unwrap().display().to_string()).collect();
        assert_eq!(
            got,
            ["hive/g1/goal.jsonl", "hive/g1/tasks.jsonl", "hive/g1/flags.jsonl", "hive/g1/bundles.jsonl", "hive/g1/spend.jsonl", "hive/g2/messages.jsonl"]
        );
        let all = logs(&d);
        assert!(all.iter().any(|p| p.ends_with("hive/g1/tasks.jsonl")) && all.last().unwrap().ends_with(REASONING));
        assert!(!all.iter().any(|p| p.ends_with("oracle.jsonl")
            || p.ends_with("joins.jsonl")
            || p.ends_with("goal.json")
            || p.ends_with("unbundled.jsonl")
            || p.ends_with("runs.jsonl")));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn union_skips_known_and_torn_lines() {
        let d = tmp("union");
        let p = d.join("lessons.jsonl");
        std::fs::write(&p, format!("{}\n{{\"id\":\"L2\",\"ver", lesson("L1", 1, "a"))).unwrap();
        assert_eq!(read(&p).len(), 1, "a half-written line does not count");
        let n = merge(&p, &[lesson("L1", 1, "a"), lesson("L3", 1, "c"), "garbage".into()]).unwrap();
        assert_eq!(n, 1, "only the unknown complete line is new");
        assert_eq!(merge(&p, &[lesson("L3", 1, "c")]).unwrap(), 0, "idempotent");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_bundle_line_over_its_cap_counts_nowhere() {
        let d = tmp("cap");
        let p = d.join("hive/g/bundles.jsonl");
        let big = serde_json::json!({"v": 1, "bundle": "A".repeat(borg::hive::commits::MAX_LINE)}).to_string();
        let small = serde_json::json!({"v": 1, "bundle": "AAAA"}).to_string();
        assert_eq!(merge(&p, &[big.clone(), small.clone()]).unwrap(), 1, "only the line within the cap is stored");
        std::fs::write(&p, format!("{big}\n{small}\n")).unwrap();
        assert_eq!(read(&p).into_values().collect::<Vec<_>>(), [small], "a line written over the cap is never offered");
        assert_eq!(cap(&d.join("lessons.jsonl")), usize::MAX);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn ratified_constraints_sync_and_load_as_the_same_standing_lesson_everywhere() {
        let d = tmp("constraints");
        assert!(logs(&d).contains(&d.join(borg::veto::FILE)), "stamped, signed and quarantined like the lessons");
        let c = |by: &str, ms: i64| {
            serde_json::json!({"id": "cabc", "text": "merge lookups go through the review API", "lesson": "L1", "by": by, "ms": ms, "source": "veto"})
                .to_string()
        };
        let (a, b) = (d.join("a"), d.join("b"));
        for (home, own) in [(&a, c("bob@x", 9)), (&b, c("ann@x", 5))] {
            std::fs::create_dir_all(home).unwrap();
            std::fs::write(home.join("lessons.jsonl"), format!("{}\n", lesson("L1", 1, "lookup?"))).unwrap();
            merge(&home.join(borg::veto::FILE), &[own]).unwrap();
        }
        merge(&a.join(borg::veto::FILE), &read(&b.join(borg::veto::FILE)).into_values().collect::<Vec<_>>()).unwrap();
        merge(&b.join(borg::veto::FILE), &read(&a.join(borg::veto::FILE)).into_values().collect::<Vec<_>>()).unwrap();
        let standing = |h: &Path| {
            let v: Vec<_> = borg::conscience::load(&h.join("lessons.jsonl"), None)
                .into_iter()
                .filter(|c| c.standing())
                .map(|c| (c.id, c.engineers, c.consolidated))
                .collect();
            v
        };
        assert_eq!(standing(&a), [("cabc".to_string(), vec!["ann@x".to_string()], true)], "the earliest ratification, whatever the order");
        assert_eq!(standing(&a), standing(&b));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn concurrent_versions_settle_on_the_same_winner_everywhere() {
        let d = tmp("settle");
        let (a, b) = (d.join("a/lessons.jsonl"), d.join("b/lessons.jsonl"));
        let (v1, v2a, v2b) = (lesson("L1", 1, "orig"), lesson("L1", 2, "alice's merge"), lesson("L1", 2, "bob's merge"));
        merge(&a, &[v1.clone(), v2a.clone()]).unwrap();
        merge(&b, &[v1.clone(), v2b.clone()]).unwrap();
        merge(&a, &read(&b).into_values().collect::<Vec<_>>()).unwrap();
        merge(&b, &read(&a).into_values().collect::<Vec<_>>()).unwrap();
        assert_eq!(digest(&read(&a)), digest(&read(&b)), "same set");
        let q = |p: &Path| borg::conscience::load(p, None).into_iter().map(|c| c.question).collect::<Vec<_>>();
        assert_eq!(q(&a), q(&b), "the conscience asks the same version on both nodes");
        assert_ne!(q(&a), vec!["orig".to_string()], "an older version never wins");
        // Arriving out of order (newest first) still leaves the newest last.
        let c = d.join("c/lessons.jsonl");
        merge(&c, &[v2a]).unwrap();
        merge(&c, &[v1]).unwrap();
        assert_eq!(q(&c), vec!["alice's merge".to_string()]);
        let _ = std::fs::remove_dir_all(d);
    }
}
