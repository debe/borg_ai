//! Absorbed lessons: long-term knowledge an outside store now holds, so Borg stops delivering it.
//!
//! A team may keep its settled knowledge somewhere else (a reviewed skills repo, a handbook). Once a
//! lesson is held there, Borg delivering it too is a second copy that drifts. `borg memory absorb <id>`
//! says so, `borg memory release <id>` undoes it; `borg memory export` (memory_export.rs) shows the
//! evidence an engineer decides on. Borg never names, reads or resolves the outside store: the note an
//! absorb carries ("skills/review@2026-10-30") is an opaque label it stores, syncs and shows.
//!
//! `<store>/absorbed.jsonl` (`FILE`) is append-only, one plain line per id and command: `{"id", "kind":
//! "absorb" | "release", "ms", "by", "note"?, "version"?}` (`Record`). `by` is the engineer's git email,
//! `note` (absorb only) is redacted (notes.rs `redact`) and at most `NOTE_CHARS` characters, `version`
//! (absorb only) the lesson's version when it was absorbed, which `borg memory export` counts the
//! lesson's recurrence from. Any member may write it. A lesson's state is its record that sorts last by
//! (ms, kind with release after absorb, the line's text) (`state`), so neither line order nor the order
//! lines arrived in over the mesh decides. An id not in the store (yet) is accepted and applies once the
//! lesson arrives. A malformed line is skipped, and a missing or unreadable file is no absorb: a hook
//! never fails on it.
//!
//! **What an absorb does.** It withholds the lesson from every path that hands lessons to a drone, on
//! every harness (`withhold`, applied by hook.rs `lessons`, which early and standing delivery, live and
//! stop checks, the queue and the vector shortlist all read, and by `borg debug recall`, `prefilter`,
//! `vectors --query` and `borg why`). Lessons it hid as their successor stay hidden. An absorbed lesson
//! that a live lesson hides stays in the list, hidden: it is never scored or delivered, and its match
//! still counts for that successor (conscience.rs `rank`). It never widens a lesson's reach, applies
//! only in the store it was written to, and in a private collective (`share: local`) stays on this
//! machine as everything there does. A constraint ratified from the lesson (veto.rs `ratify`) is its own
//! standing item: absorbing the lesson leaves it delivered. Learning still sees an absorbed lesson:
//! `borg learn` offers it for merging (learn.rs `similar`), so a correction of the same concern merges
//! into it and counts as a recurrence, the sign the outside copy did not hold; and the judge still
//! gives a delivery made before the absorb its verdict (hook.rs `Work::judged`).
//!
//! A record stamped further ahead of this clock than msg.rs `SKEW_MS` is ignored (`latest`), and a
//! command is stamped after the records it sees (`stamp`), so a skewed or hand-written clock never
//! makes an absorb that no release undoes. The mesh syncs the file signed and quarantined like the
//! other logs (borg-mesh lines.rs `SYNCED`, protocol `borg/mesh/15`), and lets a peer's line in only
//! when `written` accepts it, the check this module reads by.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::conscience::Challenge;

/// The absorbs and releases of a store, beside its lessons.
pub const FILE: &str = "absorbed.jsonl";

/// `Record::kind` of an absorb, and of a release.
pub const ABSORB: &str = "absorb";
pub const RELEASE: &str = "release";

/// Protocol: the longest note an absorb carries, in characters. Guard: a note is a label (a path and a
/// version, a link), not a copy of the knowledge; the mesh refuses a longer one from a peer.
pub const NOTE_CHARS: usize = 1000;

/// One line of `FILE`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub kind: String,
    pub ms: i64,
    #[serde(default)]
    pub by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
}

impl Record {
    /// What the write path guarantees, checked again on read: an id, kind absorb or release, a note of
    /// at most `NOTE_CHARS` characters. A line that fails it was not written by borg and is skipped.
    pub fn well_formed(&self) -> bool {
        !self.id.trim().is_empty() && (self.kind == ABSORB || self.kind == RELEASE) && self.note.as_ref().is_none_or(|n| n.chars().count() <= NOTE_CHARS)
    }

    pub fn absorbed(&self) -> bool {
        self.kind == ABSORB
    }
}

/// The absorbed log of the store `store`.
pub fn path(store: &Path) -> PathBuf {
    store.join(FILE)
}

/// The absorbed log beside a lessons file (hook.rs `lessons_path`, which BORG_CONSCIENCE may move).
pub fn beside(lessons: &Path) -> PathBuf {
    lessons.with_file_name(FILE)
}

/// A line of `FILE` as borg writes it, at `now`: it parses as `Record`, is `well_formed`, names `by`,
/// holds no other field, and is stamped no further ahead of `now` than msg.rs `SKEW_MS`. The mesh lets
/// a peer's line in only through it (borg-mesh lines.rs `travels`), so a line it stores is one every
/// node folds.
pub fn written(line: &str, now: i64) -> Option<Record> {
    let r = serde_json::from_str::<Record>(line).ok().filter(|r| r.well_formed() && r.ms <= now.saturating_add(crate::msg::SKEW_MS))?;
    let Ok(serde_json::Value::Object(o)) = serde_json::from_str::<serde_json::Value>(line) else { return None };
    let known = ["id", "kind", "ms", "by", "note", "version"];
    (o.contains_key("by") && o.keys().all(|k| known.contains(&k.as_str()))).then_some(r)
}

/// Every lesson id's last record in the log `file` (written before `before_ms`, when given): the one
/// that sorts last by (ms, kind with release after absorb, the line's text), whatever the line order.
/// A record stamped further ahead of this clock than msg.rs `SKEW_MS` is ignored. Absent or
/// unreadable: empty.
pub fn latest(file: &Path, before_ms: Option<i64>) -> HashMap<String, Record> {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let ahead = crate::learn::now_ms().saturating_add(crate::msg::SKEW_MS);
    let mut by: HashMap<String, (i64, u8, &str, Record)> = HashMap::new();
    for line in text.lines() {
        let Ok(r) = serde_json::from_str::<Record>(line) else { continue };
        if !r.well_formed() || r.ms > ahead || before_ms.is_some_and(|b| r.ms >= b) {
            continue;
        }
        let key = (r.ms, u8::from(r.kind == RELEASE), line);
        if by.get(&r.id).is_none_or(|(ms, k, l, _)| (*ms, *k, *l) < key) {
            by.insert(r.id.clone(), (key.0, key.1, key.2, r));
        }
    }
    by.into_iter().map(|(id, (.., r))| (id, r)).collect()
}

/// The lessons absorbed now in the log `file` (before `before_ms`): their absorb record, by id.
pub fn state(file: &Path, before_ms: Option<i64>) -> HashMap<String, Record> {
    let mut all = latest(file, before_ms);
    all.retain(|_, r| r.absorbed());
    all
}

/// The delivery filter: drops from `lessons` every absorbed one, and every lesson it hid as the end of
/// its chain of hiding supersessions (trust.rs `successors`), so the lessons an absorbed one replaced
/// do not come back in its place. An absorbed lesson whose chain ends at a lesson that stays is kept,
/// hidden: no path scores or delivers a hidden lesson, and conscience.rs `rank` gives its match to that
/// live successor, which an absorb must not weaken. A standing lesson (a ratified constraint) is never
/// dropped, whatever its id. The caller passes the lessons as loaded, with their trust (conscience.rs
/// `load`). Returns the lessons dropped.
pub fn withhold(lessons: &mut Vec<Challenge>, absorbed: &HashMap<String, Record>) -> Vec<Challenge> {
    let held = |c: &Challenge| !c.standing() && absorbed.contains_key(&c.id);
    if !lessons.iter().any(held) {
        return Vec::new();
    }
    let hidden = crate::trust::successors(lessons);
    // The chain ends that go: absorbed and hidden by nothing.
    let ends: std::collections::HashSet<String> = lessons.iter().filter(|c| held(c) && !hidden.contains_key(&c.id)).map(|c| c.id.clone()).collect();
    let gone = |c: &Challenge| held(c) && hidden.get(&c.id).is_none_or(|end| ends.contains(end));
    let mut dropped: Vec<Challenge> = Vec::new();
    let mut keep = Vec::with_capacity(lessons.len());
    for c in lessons.drain(..) {
        if gone(&c) { dropped.push(c) } else { keep.push(c) }
    }
    *lessons = keep;
    // What the dropped hid comes back unless something that stays still hides it; until none does, as
    // a lesson that comes back and goes may have hidden others in turn.
    loop {
        let still = crate::trust::successors(lessons);
        let (back, stay): (Vec<Challenge>, Vec<Challenge>) = lessons.drain(..).partition(|c| hidden.contains_key(&c.id) && !still.contains_key(&c.id));
        *lessons = stay;
        if back.is_empty() {
            return dropped;
        }
        dropped.extend(back);
    }
}

/// `withhold` with the log beside the lessons file `lessons_path`, as of `before_ms`.
pub fn withhold_beside(lessons_path: &Path, lessons: &mut Vec<Challenge>, before_ms: Option<i64>) -> Vec<Challenge> {
    withhold(lessons, &state(&beside(lessons_path), before_ms))
}

/// The ms to stamp a command on `ids` with at `now`: after every record of theirs in `latest`, so the
/// command takes effect over the state it sees, however far ahead (within `SKEW_MS`) a skewed clock
/// stamped that.
pub fn stamp(latest: &HashMap<String, Record>, ids: &[&str], now: i64) -> i64 {
    ids.iter().filter_map(|i| latest.get(i.trim())).map(|r| r.ms.saturating_add(1)).fold(now, i64::max)
}

/// The note an absorb stores: trimmed and redacted (notes.rs `redact`); empty is none. Refused when the
/// note or its redaction is longer than `NOTE_CHARS` characters.
///
/// # Errors
/// What to shorten.
pub fn note(text: Option<&str>) -> Result<Option<String>, String> {
    let Some(t) = text.map(str::trim).filter(|t| !t.is_empty()) else { return Ok(None) };
    let redacted = crate::notes::redact(t);
    let n = t.chars().count().max(redacted.chars().count());
    if n > NOTE_CHARS {
        return Err(format!("the note has {n} characters, at most {NOTE_CHARS}: name where the lesson is held, do not copy it"));
    }
    Ok(Some(redacted))
}

/// Appends one record per id (in order, each once) to the store's log, each line in one write
/// (learn.rs `append_line`). `version` gives a lesson's current version for an absorb (None: not in the
/// store yet); a release carries neither note nor version. Returns the records written.
///
/// # Errors
/// When a line cannot be appended; the records before it are written.
pub fn write(
    store: &Path,
    ids: &[&str],
    kind: &str,
    note: Option<&str>,
    by: &str,
    now: i64,
    version: &dyn Fn(&str) -> Option<u32>,
) -> std::io::Result<Vec<Record>> {
    let mut out: Vec<Record> = Vec::new();
    for id in ids.iter().map(|i| i.trim()).filter(|i| !i.is_empty()) {
        if out.iter().any(|r| r.id == id) {
            continue;
        }
        let absorb = kind == ABSORB;
        let r = Record {
            id: id.into(),
            kind: kind.into(),
            ms: now,
            by: by.into(),
            note: note.filter(|_| absorb).map(str::to_string),
            version: if absorb { version(id) } else { None },
        };
        crate::learn::append_jsonl(&path(store), &r)?;
        out.push(r);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conscience;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-absorb-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn line(id: &str, kind: &str, ms: i64, note: Option<&str>) -> String {
        serde_json::to_string(&Record { id: id.into(), kind: kind.into(), ms, by: "a@x".into(), note: note.map(str::to_string), version: None }).unwrap()
    }

    #[test]
    fn the_state_is_the_last_record_by_time_then_kind_then_text_in_any_line_order() {
        let lines = [
            line("A", ABSORB, 10, Some("skills/review@1")),
            line("A", RELEASE, 20, None),
            line("B", ABSORB, 10, None),
            // The same ms: a release sorts after an absorb, so it wins.
            line("C", ABSORB, 30, None),
            line("C", RELEASE, 30, None),
            // Two absorbs at the same ms: the line's text decides, the same on every node.
            line("D", ABSORB, 40, Some("b")),
            line("D", ABSORB, 40, Some("a")),
        ];
        let mut orders = vec![lines.to_vec()];
        let mut rev = lines.to_vec();
        rev.reverse();
        orders.push(rev);
        let dir = temp("order");
        for order in orders {
            std::fs::write(path(&dir), order.join("\n") + "\n").unwrap();
            let s = state(&path(&dir), None);
            let mut ids: Vec<&str> = s.keys().map(String::as_str).collect();
            ids.sort_unstable();
            assert_eq!(ids, ["B", "D"]);
            assert_eq!(s["D"].note.as_deref(), Some("b"));
        }
        // Before the release: A is absorbed (an eval's cut-off, BORG_BEFORE).
        assert!(state(&path(&dir), Some(15)).contains_key("A"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_file_or_malformed_lines_are_no_absorb() {
        let dir = temp("malformed");
        assert!(state(&path(&dir), None).is_empty());
        let long = "x".repeat(NOTE_CHARS + 1);
        let text = [
            "not json".to_string(),
            r#"{"id": "", "kind": "absorb", "ms": 1}"#.into(),
            r#"{"id": "E", "kind": "forget", "ms": 1}"#.into(),
            r#"{"id": "F", "kind": "absorb"}"#.into(),
            line("G", ABSORB, 1, Some(&long)),
            line("H", ABSORB, 1, None),
        ];
        std::fs::write(path(&dir), text.join("\n")).unwrap();
        assert_eq!(state(&path(&dir), None).keys().collect::<Vec<_>>(), ["H"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_note_is_redacted_and_bounded() {
        assert_eq!(note(None), Ok(None));
        assert_eq!(note(Some("  ")), Ok(None));
        assert_eq!(note(Some(" skills/review@2026-10-30 ")), Ok(Some("skills/review@2026-10-30".into())));
        let secret = note(Some("held in skills/review, token ghp_0123456789abcdefghijABCDEFGHIJ012345")).unwrap().unwrap();
        assert!(!secret.contains("ghp_0123456789"), "{secret}");
        assert!(note(Some(&"x".repeat(NOTE_CHARS))).is_ok());
        assert!(note(Some(&"x".repeat(NOTE_CHARS + 1))).unwrap_err().contains("at most 1000"));
    }

    #[test]
    fn write_appends_one_line_per_id_and_a_release_carries_no_note_or_version() {
        let dir = temp("write");
        let version = |id: &str| (id == "A").then_some(3);
        let w = write(&dir, &["A", "Z", "A", " "], ABSORB, Some("skills/review@1"), "a@x", 5, &version).unwrap();
        assert_eq!(w.iter().map(|r| (r.id.as_str(), r.version)).collect::<Vec<_>>(), [("A", Some(3)), ("Z", None)]);
        let r = write(&dir, &["A"], RELEASE, Some("ignored"), "b@x", 6, &version).unwrap();
        assert_eq!((r[0].note.clone(), r[0].version), (None, None));
        let text = std::fs::read_to_string(path(&dir)).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert_eq!(text.lines().next().unwrap(), r#"{"id":"A","kind":"absorb","ms":5,"by":"a@x","note":"skills/review@1","version":3}"#);
        assert_eq!(text.lines().nth(2).unwrap(), r#"{"id":"A","kind":"release","ms":6,"by":"b@x"}"#);
        let s = state(&path(&dir), None);
        assert!(!s.contains_key("A") && s.contains_key("Z"), "released; Z applies when it arrives");
        let _ = std::fs::remove_dir_all(dir);
    }

    fn lesson(id: &str, ms: i64, supersedes: &[&str]) -> Challenge {
        Challenge {
            id: id.into(),
            origin_ms: ms,
            question: format!("q {id}"),
            supersedes: supersedes.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn absorbed(ids: &[&str]) -> HashMap<String, Record> {
        ids.iter().map(|i| (i.to_string(), Record { id: i.to_string(), kind: ABSORB.into(), ms: 1, by: String::new(), note: None, version: None })).collect()
    }

    fn ids(cs: &[Challenge]) -> Vec<&str> {
        cs.iter().map(|c| c.id.as_str()).collect()
    }

    #[test]
    fn withhold_drops_the_absorbed_and_what_they_hid_never_a_constraint() {
        // Old is hidden by New (queen-decided); New is absorbed: Old must not come back in its place.
        let mut new = lesson("New", 2, &["Old"]);
        new.source = "queen".into();
        let constraint = Challenge { id: "cX".into(), source: crate::conscience::CONSTRAINT.into(), ..lesson("cX", 3, &[]) };
        let mut all = vec![lesson("Old", 1, &[]), new, lesson("Free", 4, &[]), constraint];
        withhold(&mut all, &absorbed(&["New", "cX"]));
        assert_eq!(ids(&all), ["Free", "cX"]);
        // Nothing absorbed here: unchanged.
        let mut all = vec![lesson("Old", 1, &[]), lesson("Free", 4, &[])];
        withhold(&mut all, &absorbed(&["Gone"]));
        assert_eq!(ids(&all), ["Old", "Free"]);
        // An absorbed lesson that was itself hidden: what it hid stays hidden by the live successor.
        let mut mid = lesson("Mid", 2, &["Old"]);
        mid.source = "queen".into();
        let mut top = lesson("Top", 3, &["Mid"]);
        top.source = "queen".into();
        let mut all = vec![lesson("Old", 1, &[]), mid.clone(), top.clone()];
        let dropped = withhold(&mut all, &absorbed(&["Mid"]));
        assert_eq!(ids(&all), ["Old", "Mid", "Top"], "Mid stays, hidden by Top, as Old does through it");
        assert!(dropped.is_empty());
        assert_eq!(crate::trust::successors(&all).len(), 2, "both still hidden: neither is scored or delivered");
        // The whole chain absorbed: all of it goes.
        let mut all = vec![lesson("Old", 1, &[]), mid, top];
        let dropped = withhold(&mut all, &absorbed(&["Mid", "Top"]));
        assert!(all.is_empty() && dropped.len() == 3, "{:?}", ids(&all));
    }

    #[test]
    fn what_an_absorbed_end_hid_never_comes_back_through_a_lesson_that_goes() {
        // Older -> Old -> New, New absorbed: Old comes back without New and goes, so Older, which only
        // Old hid then, goes too.
        let mut old = lesson("Old", 2, &["Older"]);
        old.source = "queen".into();
        let mut new = lesson("New", 3, &["Old"]);
        new.source = "queen".into();
        let mut all = vec![lesson("Older", 1, &[]), old, new, lesson("Free", 4, &[])];
        let dropped = withhold(&mut all, &absorbed(&["New"]));
        assert_eq!(ids(&all), ["Free"]);
        assert_eq!(dropped.len(), 3);
    }

    /// Absorbing a lesson a live one hides leaves that successor's rank as it was: the hidden lesson's
    /// match still flows to it.
    #[test]
    fn absorbing_a_hidden_lesson_keeps_its_live_successors_rank() {
        let mut old = Challenge { question: "Is the socket buffer drained before the reconnect?".into(), ..lesson("Old", 1, &[]) };
        old.consequence = "a reconnect replays stale frames".into();
        let mut new = Challenge { question: "Is the retry budget shared across tenants?".into(), ..lesson("New", 2, &["Old"]) };
        new.source = "queen".into();
        let context = "drain the socket buffer before the reconnect, or stale frames replay";
        let score = |all: &[Challenge]| conscience::rank_scoped(all, context, context).iter().find(|(_, c)| c.id == "New").map(|(s, _)| *s);
        let mut all = vec![old, new, lesson("Free", 3, &[])];
        let before = score(&all);
        assert!(before.is_some_and(|s| s > 0.0), "New ranks at Old's match");
        withhold(&mut all, &absorbed(&["Old"]));
        assert_eq!(score(&all), before);
        assert!(conscience::rank_scoped(&all, context, context).iter().all(|(_, c)| c.id != "Old"), "Old itself is never ranked");
    }

    #[test]
    fn written_is_what_the_read_folds_and_bounds_the_clock() {
        let now = 1_000_000;
        let ok = line("L1", ABSORB, now, Some("skills/review@1"));
        assert!(written(&ok, now).is_some());
        assert!(written(&line("L1", RELEASE, now + crate::msg::SKEW_MS, None), now).is_some(), "within the skew");
        for (what, bad) in [
            ("ahead of the clock", line("L1", ABSORB, now + crate::msg::SKEW_MS + 1, None)),
            ("a blank id", line(" ", ABSORB, now, None)),
            ("a version past u32", r#"{"id":"L1","kind":"absorb","ms":1,"by":"a@x","version":5000000000}"#.into()),
            ("a duplicate key", r#"{"id":"A","id":"B","kind":"absorb","ms":1,"by":"a@x"}"#.into()),
            ("no by", r#"{"id":"L1","kind":"release","ms":1}"#.into()),
            ("an unknown field", r#"{"id":"L1","kind":"absorb","ms":1,"by":"a@x","reach":"team"}"#.into()),
            ("an oversize note", line("L1", ABSORB, 1, Some(&"n".repeat(NOTE_CHARS + 1)))),
        ] {
            assert!(written(&bad, now).is_none(), "{what} is refused");
        }
    }

    /// A member whose clock ran far ahead cannot absorb for good: the read ignores the record, and a
    /// command is stamped after what it sees.
    #[test]
    fn a_future_absorb_never_outlives_a_release() {
        let dir = temp("future");
        let far = crate::learn::now_ms() + 365 * 24 * 3_600_000;
        std::fs::write(path(&dir), line("L1", ABSORB, far, None) + "\n").unwrap();
        assert!(state(&path(&dir), None).is_empty(), "stamped a year ahead: ignored");
        let near = crate::learn::now_ms() + crate::msg::SKEW_MS / 2;
        std::fs::write(path(&dir), line("L1", ABSORB, near, None) + "\n").unwrap();
        let seen = latest(&path(&dir), None);
        let now = crate::learn::now_ms();
        let at = stamp(&seen, &["L1"], now);
        assert!(at > near);
        write(&dir, &["L1"], RELEASE, None, "b@x", at, &|_| None).unwrap();
        assert!(state(&path(&dir), None).is_empty(), "the release wins over the absorb it saw");
        assert_eq!(stamp(&seen, &["Other"], now), now);
        let _ = std::fs::remove_dir_all(dir);
    }
}
