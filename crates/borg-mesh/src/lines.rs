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
/// And the ratified constraints (borg veto.rs `ratify`): standing lessons every member's sessions get
/// (conscience.rs `standing`), lines holding what a lesson does (the redacted invariant, its lesson,
/// who ratified it). Plain lines (no `version`); the conscience keeps the earliest ratification per id,
/// so arrival order does not matter.
///
/// And the messages between sessions (borg msg.rs): plain lines, each naming its author's node. That
/// name is a claim, so a message travels only under an origin of the key it names (`travels`), and
/// only while it is fresh: a line one node pruned would otherwise come back from every peer. Last in
/// the list: a 0.3.0 peer (`borg/mesh/12`) never reconciles this one.
///
/// The removed playbooks' log is not listed: a session still steps through it after the outcomes
/// and keeps nothing (sync.rs `PLAYBOOKS`).
pub const SYNCED: [&str; 5] = ["lessons.jsonl", "stats.jsonl", borg::outcome::TEAM_FILE, borg::veto::FILE, borg::msg::FILE];

/// Redacted reasoning excerpts behind git notes (`borg notes`): replicated by their author to
/// `sync::REPLICAS` members, not reconciled with everyone (sync.rs, step 4).
pub const REASONING: &str = "reasoning.jsonl";

pub type Hash = [u8; 32];

/// The logs whose lines carry origins, in `home`: the synced ones and reasoning. Stamping (origin.rs
/// `stamp`), quarantine (`origin::apply`) and the daemon's change fingerprint all cover what this lists.
pub fn logs(home: &Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = SYNCED.iter().map(|f| home.join(f)).collect();
    out.push(home.join(REASONING));
    out
}

/// Whether `line` of the log `file` may be offered or stored under an origin of `key`, at `now`. Any
/// line but a message's. A message must be one borg msg.rs could have written (`well_formed`), name
/// `key` as its node (the member key its sender wrote, as `borg top` names a remote node), and be
/// fresh (msg.rs `fresh`: neither past `TTL_MS` nor stamped further ahead of this clock than
/// `SKEW_MS`): a member's forged node, or a far-future line that would outlive every prune, never
/// reaches another member.
pub fn travels(file: &str, line: &str, key: &str, now: i64) -> bool {
    if file != borg::msg::FILE {
        return true;
    }
    let Ok(m) = serde_json::from_str::<borg::msg::Message>(line) else { return false };
    m.well_formed() && m.node == key && borg::msg::fresh(&m, now)
}

pub fn hash(line: &str) -> Hash {
    *blake3::hash(line.as_bytes()).as_bytes()
}

fn complete(line: &str) -> bool {
    !line.trim().is_empty() && serde_json::from_str::<Value>(line).is_ok()
}

/// The distinct complete lines of `path` by hash (empty if the file does not exist).
pub fn read(path: &Path) -> BTreeMap<Hash, String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines().filter(|l| complete(l)).map(|l| (hash(l), l.to_string())).collect()
}

/// One hash for the whole set: equal digests mean there is nothing to exchange.
pub fn digest(set: &BTreeMap<Hash, String>) -> Hash {
    let mut h = blake3::Hasher::new();
    for k in set.keys() {
        h.update(k);
    }
    *h.finalize().as_bytes()
}

/// Appends the `incoming` lines `path` lacks, then settles lesson versions. Returns how many lines
/// were new.
pub fn merge(path: &Path, incoming: &[String]) -> Result<usize, String> {
    let mut known = read(path);
    let new: Vec<&String> = incoming.iter().filter(|l| complete(l) && known.insert(hash(l), (*l).clone()).is_none()).collect();
    append(path, &new)?;
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
