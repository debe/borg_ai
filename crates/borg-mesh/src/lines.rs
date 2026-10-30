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
/// And the messages between sessions (borg msg.rs): plain lines, each naming its author's node, and the
/// `seen` lines their recipients write (msg.rs `Seen`, which pace the senders). That name is a claim,
/// so a line travels only under an origin of the key it names (`travels`), and only while it is fresh:
/// a line one node pruned would otherwise come back from every peer. A 0.3.0 peer (`borg/mesh/12`)
/// never reconciles this one.
///
/// And the absorbs and releases (`ABSORBED`, borg `memory absorb` and `release`): plain lines saying a
/// lesson is now held outside Borg, or is no longer. Each must be one borg could have written and
/// folds (`travels`), because a line a peer gives never passes the write path. Last in the list: a
/// 0.4.0 to 0.6.x peer (`borg/mesh/13`, `/14`) never reconciles this one.
///
/// The removed playbooks' log is not listed: a session still steps through it after the outcomes
/// and keeps nothing (sync.rs `PLAYBOOKS`).
pub const SYNCED: [&str; 6] = ["lessons.jsonl", "stats.jsonl", borg::outcome::TEAM_FILE, borg::veto::FILE, borg::msg::FILE, ABSORBED];

/// The absorbs and releases of the collective's lessons (`SYNCED`, borg absorb.rs `FILE`): one line per
/// id and command, `{"id", "kind": "absorb" | "release", "ms", "by", "note"?, "version"?}`. The mesh
/// checks a line as borg reads it (`travels`), never what the note says.
pub const ABSORBED: &str = borg::absorb::FILE;

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
/// line but an absorb's or a message's. An absorb or a release must be a line borg `memory absorb` and
/// `release` could have written at `now` (borg absorb.rs `written`: the record borg folds, its note at
/// most `NOTE_CHARS`, no other field, stamped no further ahead than `SKEW_MS`), whoever stamped it. A
/// message or a seen line must be one
/// borg msg.rs could have written (`Line::parse`), name `key` as its node (the member key its sender or
/// reader wrote, as `borg top` names a remote node), and be fresh (msg.rs `fresh_at`: neither past
/// `TTL_MS` nor stamped further ahead of this clock than `SKEW_MS`): a member's forged node, or a
/// far-future line that would outlive every prune, never reaches another member, and no node
/// acknowledges for another's sessions. A
/// message to the sessions of its own node (`local`, msg.rs `stays_here`) never travels: it is for
/// that machine alone.
pub fn travels(file: &str, line: &str, key: &str, now: i64) -> bool {
    if file == ABSORBED {
        return borg::absorb::written(line, now).is_some();
    }
    if file != borg::msg::FILE {
        return true;
    }
    let Some(l) = borg::msg::Line::parse(line) else { return false };
    l.node() == key && borg::msg::fresh_at(l.ms(), now) && l.message().is_none_or(|m| !m.stays_here())
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
/// Not the absorbs (`ABSORBED`): an absorb names its lesson's `id` and `version`, but it is no version
/// of a record, and borg folds every line of that log whatever their order.
pub fn settle(path: &Path) -> Result<(), String> {
    if path.file_name().is_some_and(|f| f == ABSORBED) {
        return Ok(());
    }
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

    /// `local` is this machine's sessions: its line never travels; `collective` (written `all`) does.
    #[test]
    fn a_local_broadcast_never_travels() {
        let key = "ab".repeat(32);
        let from = borg::msg::Sender { by: "s1".into(), name: "One of Two".into(), harness: "claude".into(), node: key.clone() };
        let line = |to: &str| serde_json::to_string(&borg::msg::Message::new(borg::msg::TELL, &from, to, None, 1_000, "freeze merges")).unwrap();
        assert!(!travels(borg::msg::FILE, &line(&format!("{}@{key}", borg::msg::LOCAL)), &key, 1_000));
        assert!(travels(borg::msg::FILE, &line(borg::msg::ALL), &key, 1_000));
        assert!(travels(borg::msg::FILE, &line(&format!("s2@{}", "cd".repeat(32))), &key, 1_000));
    }

    /// A seen line travels under its own node's key while fresh: no node acknowledges for another's
    /// sessions, and a malformed one never travels.
    #[test]
    fn a_seen_line_travels_only_under_its_node() {
        let (key, other) = ("ab".repeat(32), "cd".repeat(32));
        let seen = |node: &str, ms: i64, ids: &[&str]| serde_json::to_string(&borg::msg::Seen::new("s2", node, ms, ids)).unwrap();
        assert!(travels(borg::msg::FILE, &seen(&key, 1_000, &["m0123456789ab"]), &key, 1_000));
        assert!(!travels(borg::msg::FILE, &seen(&key, 1_000, &["m0123456789ab"]), &other, 1_000), "signed by another node");
        assert!(!travels(borg::msg::FILE, &seen(&key, 1_000, &["m0123456789ab"]), &key, 1_000 + borg::msg::TTL_MS + 1), "expired");
        assert!(!travels(borg::msg::FILE, &seen(&key, 1_000, &["not an id"]), &key, 1_000), "malformed");
    }

    /// An absorb or release travels only as a line borg could have written and folds, whoever signed
    /// it: a peer's line never passes the write path.
    #[test]
    fn an_absorb_travels_only_well_formed() {
        let key = "ab".repeat(32);
        let ok = |v: Value| travels(ABSORBED, &v.to_string(), &key, 10);
        const ABSORB_NOTE: usize = borg::absorb::NOTE_CHARS;
        let absorb = serde_json::json!({"id": "L1", "kind": "absorb", "ms": 5, "by": "lead@x", "note": "skills/review@2026-10-30", "version": 2});
        assert!(ok(absorb.clone()));
        assert!(ok(serde_json::json!({"id": "L1", "kind": "release", "ms": 6, "by": "lead@x"})));
        assert!(ok(serde_json::json!({"id": "L1", "kind": "release", "ms": 6, "by": "lead@x", "note": null, "version": null})), "null is absent");
        assert!(ok(serde_json::json!({"id": "L1", "kind": "absorb", "ms": 5, "by": "lead@x", "note": "é".repeat(ABSORB_NOTE)})), "characters, not bytes");
        let with = |k: &str, v: Value| {
            let mut a = absorb.clone();
            a[k] = v;
            a
        };
        for (what, bad) in [
            ("an oversize note", with("note", Value::from("n".repeat(ABSORB_NOTE + 1)))),
            ("another kind", with("kind", Value::from("delete"))),
            ("no kind", serde_json::json!({"id": "L1", "ms": 5, "by": "lead@x"})),
            ("a numeric id", with("id", Value::from(1))),
            ("an empty id", with("id", Value::from(""))),
            ("a blank id", with("id", Value::from(" "))),
            ("a version borg cannot read", with("version", Value::from(5_000_000_000_u64))),
            ("stamped past the clock skew", with("ms", Value::from(10 + borg::msg::SKEW_MS + 1))),
            ("a text ms", with("ms", Value::from("5"))),
            ("a fractional ms", with("ms", Value::from(5.5))),
            ("no by", serde_json::json!({"id": "L1", "kind": "absorb", "ms": 5})),
            ("a note that is no text", with("note", serde_json::json!({"x": 1}))),
            ("a negative version", with("version", Value::from(-1))),
            ("an unknown field", with("reach", Value::from("team"))),
            ("no object", serde_json::json!(["L1", "absorb"])),
        ] {
            assert!(!ok(bad), "{what} is refused");
        }
        assert!(!travels(ABSORBED, "not json", &key, 1));
        assert!(!travels(ABSORBED, r#"{"id":"A","id":"B","kind":"absorb","ms":1,"by":"a@x"}"#, &key, 10), "a duplicate key");
        assert!(logs(Path::new("s")).contains(&Path::new("s").join(ABSORBED)), "stamped, signed and quarantined like every synced log");
    }

    /// Absorbs name a lesson's `id` and `version` but are no versions of a record: a merge appends what
    /// is new and never re-appends an earlier line to settle them.
    #[test]
    fn absorbs_are_not_settled_as_versions() {
        let d = tmp("absorbed");
        let p = d.join(ABSORBED);
        let a = |version: u32, ms: i64| serde_json::json!({"id": "L1", "kind": "absorb", "ms": ms, "by": "lead@x", "version": version}).to_string();
        merge(&p, &[a(3, 1)]).unwrap();
        merge(&p, &[a(2, 2)]).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), format!("{}\n{}\n", a(3, 1), a(2, 2)));
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
