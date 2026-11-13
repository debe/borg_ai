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

use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, btree_map};
use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::Path;

use borg::jsonl::Scalar;

use crate::cache::{self, Cache, Follow, Held, Mark};

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
    borg::msg::LineRef::parse(line).is_some_and(|l| message_travels(&l, key, now))
}

/// `travels` for a line of the message log already parsed.
pub fn message_travels(l: &borg::msg::LineRef, key: &str, now: i64) -> bool {
    l.node() == key && borg::msg::fresh_at(l.ms(), now) && l.message().is_none_or(|m| !m.stays_here())
}

/// Test support: the text of `file` in the store `BORG_BENCH_STORE` names (a local copy of a real store,
/// read only here, never copied into the repo); `None`, with a note, when it is unset or lacks it.
#[cfg(test)]
pub(crate) fn bench_store(file: &str) -> Option<String> {
    let Some(dir) = std::env::var_os("BORG_BENCH_STORE") else {
        eprintln!("BORG_BENCH_STORE unset: only the edge lines are checked");
        return None;
    };
    std::fs::read_to_string(Path::new(&dir).join(file)).ok()
}

pub fn hash(line: &str) -> Hash {
    *blake3::hash(line.as_bytes()).as_bytes()
}

/// Whether `line` is one whole JSON value (borg jsonl.rs `valid`: as a `serde_json::Value` parse
/// decides, without building one).
pub(crate) fn complete(line: &str) -> bool {
    !line.trim().is_empty() && borg::jsonl::valid(line)
}

/// A log's file name, as `SYNCED` lists it.
pub(crate) fn log_name(path: &Path) -> &str {
    path.file_name().and_then(|f| f.to_str()).unwrap_or_default()
}

/// The lines of `text` from byte `from` (a line start) on, as `str::lines` splits them, each with where it
/// starts in `text`.
pub(crate) fn split(text: &str, from: usize) -> impl Iterator<Item = (usize, &str)> {
    let mut at = from;
    text[from..].split_inclusive('\n').map(move |seg| {
        let start = at;
        at += seg.len();
        (start, seg.strip_suffix('\n').map_or(seg, |l| l.strip_suffix('\r').unwrap_or(l)))
    })
}

/// A log as read (cache.rs `Cache::log`): its text, and its distinct complete lines by hash as ranges of
/// it, so a session borrows a line instead of copying it. Grown by what is appended (`Follow::extend`),
/// which is all that is parsed and hashed then.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    text: String,
    lines: BTreeMap<Hash, Range<usize>>,
    /// Where the text's unterminated last line starts (its length when it ends with a newline).
    parsed: usize,
    /// The hash that unterminated line added to `lines`: taken out again before it is parsed again.
    open: Option<Hash>,
    /// The versions `settle` reads, built at its first call (`Versions`).
    versions: Option<Versions>,
    mark: Mark,
    generation: u64,
}

impl Snapshot {
    /// The log at `path`, read whole now (what `read` and the one-shot `merge` and `settle` read).
    pub fn read(path: &Path) -> Snapshot {
        let (mark, text) = cache::whole(path);
        Snapshot::whole(mark, text)
    }

    /// The line `h` names.
    pub fn get(&self, h: &Hash) -> Option<&str> {
        self.lines.get(h).map(|r| &self.text[r.clone()])
    }

    pub fn contains(&self, h: &Hash) -> bool {
        self.lines.contains_key(h)
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The distinct complete lines by hash, in hash order.
    pub fn iter(&self) -> impl Iterator<Item = (&Hash, &str)> {
        self.lines.iter().map(|(h, r)| (h, &self.text[r.clone()]))
    }

    /// The lines' hashes, in order.
    pub fn hashes(&self) -> impl Iterator<Item = &Hash> {
        self.lines.keys()
    }

    /// The lines with where each is in the text (`line` reads it back).
    pub(crate) fn ranges(&self) -> impl Iterator<Item = (&Hash, &Range<usize>)> {
        self.lines.iter()
    }

    /// Where line `h` is in the text.
    pub(crate) fn range(&self, h: &Hash) -> Option<&Range<usize>> {
        self.lines.get(h)
    }

    /// The line the unterminated end of the text added, which a later read may change.
    pub(crate) fn open(&self) -> Option<Hash> {
        self.open
    }

    /// The text of a range `ranges` gave.
    pub(crate) fn line(&self, r: &Range<usize>) -> &str {
        &self.text[r.clone()]
    }

    /// The whole text as read.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Where the text's unterminated last line starts (`Held::parsed`).
    pub(crate) fn parsed(&self) -> usize {
        self.parsed
    }

    /// The complete lines from byte `at` (a line start) on, in file order, repeats included.
    pub(crate) fn lines_from(&self, at: usize) -> impl Iterator<Item = &str> {
        split(&self.text, at.min(self.text.len())).map(|(_, l)| l).filter(|l| complete(l))
    }

    /// Same generation, same prefix: it was only extended since (cache.rs `generation`).
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Indexes the lines from `from` (a line start) on.
    fn index(&mut self, from: usize) {
        if let Some(h) = self.open.take() {
            self.lines.remove(&h);
        }
        let ends_open = !self.text.ends_with('\n');
        self.parsed = if ends_open { self.text.rfind('\n').map_or(0, |i| i + 1).max(from) } else { self.text.len() };
        for (start, l) in split(&self.text, from) {
            if !complete(l) {
                continue;
            }
            if let btree_map::Entry::Vacant(e) = self.lines.entry(hash(l)) {
                if start >= self.parsed && ends_open {
                    self.open = Some(*e.key());
                }
                e.insert(start..start + l.len());
            }
        }
    }

    /// Whether `stale` reads the versions on from its last call, rather than the whole text.
    pub(crate) fn settles_on(&self) -> bool {
        self.versions.is_some() && self.parsed == self.text.len()
    }

    /// The lines to append so the winning version of every record is its id's last line (`settle`): the
    /// versions read on from where the last call left them. A log whose last line is unterminated is
    /// read whole for it, as that line may still change.
    pub(crate) fn stale(&mut self) -> Vec<&str> {
        if self.parsed < self.text.len() {
            return stale_in(&self.text);
        }
        let v = self.versions.get_or_insert_with(Versions::default);
        v.read(&self.text, self.parsed);
        v.stale.iter().filter_map(|id| v.ids.get(id)).map(|e| &self.text[e.winner.clone()]).collect()
    }
}

impl Follow for Snapshot {
    fn whole(mark: Mark, text: String) -> Snapshot {
        let mut s = Snapshot { text, mark, generation: cache::generation(), ..Snapshot::default() };
        s.index(0);
        s
    }

    fn held(&self) -> Held<'_> {
        let guard = &self.text.as_bytes()[self.parsed.saturating_sub(cache::GUARD)..self.parsed];
        Held { mark: self.mark, end: self.text.len(), parsed: self.parsed, guard }
    }

    fn extend(&mut self, mark: Mark, tail: String) {
        let from = self.parsed;
        self.text.truncate(from);
        self.text.push_str(&tail);
        self.mark = mark;
        self.index(from);
    }
}

/// What `settle` needs of a log's versioned records, read once and then on from where it was left
/// (`Snapshot::stale`): per id the winning line and the last line, and the ids whose winner is not last.
#[derive(Debug, Clone, Default)]
struct Versions {
    /// How far the text is read: a line start, never into an unterminated line.
    upto: usize,
    ids: HashMap<Box<str>, Version>,
    /// The ids whose winning line is not their last one, in the order `settle` appends their winners.
    stale: BTreeSet<Box<str>>,
}

/// One id's versions (`Versions`): the winning version, its hash once a tie needed it, its line, and the
/// id's last line.
#[derive(Debug, Clone)]
struct Version {
    version: u64,
    hash: Option<Hash>,
    winner: Range<usize>,
    last: Range<usize>,
}

impl Versions {
    /// Reads the lines of `text` from `upto` to `end` (a line start) as `stale_in` reads every line.
    fn read(&mut self, text: &str, end: usize) {
        for (start, l) in split(&text[..end], self.upto.min(end)) {
            let Some([Scalar::Str(id), version]) = borg::jsonl::pick(l, ["id", "version"]) else { continue };
            let Some(version) = version.as_u64() else { continue };
            let here = start..start + l.len();
            let Some(e) = self.ids.get_mut(id.as_ref()) else {
                self.ids.insert(id.into(), Version { version, hash: None, winner: here.clone(), last: here });
                continue;
            };
            e.last = here.clone();
            if version > e.version {
                (e.version, e.hash, e.winner) = (version, None, here);
            } else if version == e.version {
                let hl = hash(l);
                if hl > *e.hash.get_or_insert_with(|| hash(&text[e.winner.clone()])) {
                    (e.hash, e.winner) = (Some(hl), here);
                }
            }
            if text[e.winner.clone()] == text[e.last.clone()] {
                self.stale.remove(id.as_ref());
            } else if !self.stale.contains(id.as_ref()) {
                self.stale.insert(id.into());
            }
        }
        self.upto = end;
    }
}

/// The distinct complete lines of `path` by hash (empty if the file does not exist).
pub fn read(path: &Path) -> BTreeMap<Hash, String> {
    Snapshot::read(path).iter().map(|(h, l)| (*h, l.to_string())).collect()
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
/// were new. Nothing new, nothing to do: the log is neither read again nor settled, as appending
/// nothing leaves its order as it was (a quarantine, the other rewrite, settles itself). The log read
/// whole: a node merges through its cache (cache.rs `Cache::merge`).
pub fn merge(path: &Path, incoming: &[String]) -> Result<usize, String> {
    Cache::default().merge(path, incoming)
}

/// Appends `lines` to `path`, each on a line of its own (a newline first when the file ends without one).
pub(crate) fn append(path: &Path, lines: &[impl AsRef<str>]) -> Result<(), String> {
    if lines.is_empty() {
        return Ok(());
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let mut out = String::from(if ends_open(path) { "\n" } else { "" });
    for l in lines {
        out.push_str(l.as_ref());
        out.push('\n');
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(out.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

/// Whether the file at `path` ends without a newline: its last byte alone is read.
fn ends_open(path: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut last = [0u8];
    f.seek(SeekFrom::End(-1)).is_ok() && f.read_exact(&mut last).is_ok() && last[0] != b'\n'
}

/// Makes the winning version of every versioned record the last line for its id (module doc).
/// Also after lines were removed (quarantine): the line that is now last may be an older version.
/// Not the absorbs (`ABSORBED`): an absorb names its lesson's `id` and `version`, but it is no version
/// of a record, and borg folds every line of that log whatever their order. The log read whole: a node
/// settles through its cache (cache.rs `Cache::settle`), which reads only what was appended since.
pub fn settle(path: &Path) -> Result<(), String> {
    Cache::default().settle(path)
}

/// The winners `settle` appends for `text`, read whole. Each line is read for its `id` and `version`
/// alone (borg jsonl.rs `pick`, as a `Value` reads them: a key given twice counts last), borrowed from the
/// text. A line is hashed only to break a tie of versions: the winner is the highest (version, hash), and
/// it is in place when the last line of its id is that very line.
fn stale_in(text: &str) -> Vec<&str> {
    // Per id: the winning line with its version and (once a tie needed it) its hash; and the last line.
    let mut winner: HashMap<Cow<str>, (u64, Option<Hash>, &str)> = HashMap::new();
    let mut last: HashMap<Cow<str>, &str> = HashMap::new();
    for l in text.lines() {
        let Some([Scalar::Str(id), version]) = borg::jsonl::pick(l, ["id", "version"]) else { continue };
        let Some(version) = version.as_u64() else { continue };
        last.insert(id.clone(), l);
        match winner.entry(id) {
            Entry::Vacant(e) => {
                e.insert((version, None, l));
            }
            Entry::Occupied(e) => {
                let (v, h, w) = e.into_mut();
                if version > *v {
                    (*v, *h, *w) = (version, None, l);
                } else if version == *v {
                    let hl = hash(l);
                    if hl > *h.get_or_insert_with(|| hash(w)) {
                        (*h, *w) = (Some(hl), l);
                    }
                }
            }
        }
    }
    let mut stale: Vec<(&Cow<str>, &str)> = winner.iter().filter(|(id, (.., w))| last.get(*id) != Some(w)).map(|(id, (.., w))| (id, *w)).collect();
    stale.sort();
    stale.into_iter().map(|(_, l)| l).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// `complete` as it was: a full `Value`.
    fn old_complete(line: &str) -> bool {
        !line.trim().is_empty() && serde_json::from_str::<Value>(line).is_ok()
    }

    /// `read` as it was: the file read whole, every complete line copied.
    pub(crate) fn old_read(path: &Path) -> BTreeMap<Hash, String> {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        text.lines().filter(|l| old_complete(l)).map(|l| (hash(l), l.to_string())).collect()
    }

    /// `append` as it was: the whole file read to see whether it ends with a newline.
    fn old_append(path: &Path, lines: &[impl AsRef<str>]) -> Result<(), String> {
        if lines.is_empty() {
            return Ok(());
        }
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let ends_open = std::fs::read(path).map(|b| b.last().is_some_and(|c| *c != b'\n')).unwrap_or(false);
        let mut out = String::from(if ends_open { "\n" } else { "" });
        for l in lines {
            out.push_str(l.as_ref());
            out.push('\n');
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        f.write_all(out.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// `merge` as it was: every line copied into the map, settled whatever arrived.
    fn old_merge(path: &Path, incoming: &[String]) -> Result<usize, String> {
        let mut known = old_read(path);
        let new: Vec<&String> = incoming.iter().filter(|l| old_complete(l) && known.insert(hash(l), (*l).clone()).is_none()).collect();
        old_append(path, &new)?;
        old_settle(path)?;
        Ok(new.len())
    }

    /// `settle` as it was: a `Value` per line, every line hashed, owned ids.
    fn old_settle(path: &Path) -> Result<(), String> {
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
        let mut stale: Vec<(&String, String)> =
            winner.iter().filter(|(id, (r, _))| last.get(*id) != Some(&r.1)).map(|(id, (_, l))| (id, l.to_string())).collect();
        stale.sort();
        append(path, &stale.iter().map(|(_, l)| l).collect::<Vec<_>>())
    }

    /// Versioned lines at the edges `settle` decides: ties, duplicate keys, ids and versions of every
    /// type, escapes, torn and blank lines, out of order.
    fn version_edges() -> Vec<String> {
        [
            lesson("L1", 2, "two"),
            lesson("L1", 1, "one"),
            lesson("L1", 2, "two, the other node's"),
            lesson("L2", 1, "a"),
            r#"{"id":"L2","version":1,"id":"L3","q":"duplicate id: the last counts"}"#.into(),
            r#"{"id":"L3","version":1,"q":"L3 at 1"}"#.into(),
            r#"{"id":"L3","version":2,"version":"x","q":"duplicate version, the last no number"}"#.into(),
            r#"{"id":"L3","version":"x","version":3,"q":"duplicate version, the last a number"}"#.into(),
            r#"{"id":"\u004c4","version":3,"q":"an escaped id"}"#.into(),
            r#"{"id":"L4","version":2,"q":"the same id unescaped"}"#.into(),
            r#"{"id":"\u004c4","version":3,"q":"an escaped id tying on version"}"#.into(),
            r#"{"id":"L4","version":3,"q":"the same id unescaped, tying too"}"#.into(),
            r#"{"id":5,"version":9}"#.into(),
            r#"{"id":"L5","version":5000000000,"q":"above u32"}"#.into(),
            r#"{"id":"L5","version":-1}"#.into(),
            r#"{"id":"L5","version":2.0}"#.into(),
            r#"{"id":"L5","version":18446744073709551615,"q":"u64 max"}"#.into(),
            r#"{"id":"L6","version":1,"x":"\ud800"}"#.into(),
            r#"{"id":"L6","version":2,"x":1e400}"#.into(),
            r#"{"id":"L6","version":1,"x":"kept"}"#.into(),
            r#"[{"id":"L7","version":1}]"#.into(),
            r#"{"id":"L7","version":1"#.into(),
            String::new(),
            "   ".into(),
            lesson("L1", 1, "one"),
            lesson("L2", 1, "a"),
        ]
        .into()
    }

    fn written(dir: &Path, name: &str, lines: &[String]) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        p
    }

    /// Oracle: `complete` takes exactly the lines a full `Value` took, and a snapshot holds the lines `read`
    /// copied, on every line of a real store's synced logs and at the edges, with and without a last
    /// newline, and with CRLF line ends.
    #[test]
    fn complete_and_snapshots_read_as_before() {
        let d = tmp("complete");
        let real: Vec<String> =
            SYNCED.iter().chain(&[REASONING]).filter_map(|f| bench_store(f)).flat_map(|t| t.lines().map(str::to_string).collect::<Vec<_>>()).collect();
        let all: Vec<String> = version_edges().into_iter().chain(real).collect();
        for l in &all {
            assert_eq!(complete(l), old_complete(l), "{l}");
        }
        let p = written(&d, "lessons.jsonl", &all);
        assert_eq!(read(&p), old_read(&p));
        let text = std::fs::read_to_string(&p).unwrap();
        for variant in [text.trim_end().to_string(), text.replace('\n', "\r\n"), format!("{text}{{\"torn\":"), format!("{text}{{\"open\":1}}\r")] {
            std::fs::write(&p, &variant).unwrap();
            assert_eq!(read(&p), old_read(&p));
        }
        let _ = std::fs::remove_dir_all(d);
    }

    /// Oracle: `settle` writes the same bytes as before on the edges, in either order, and on a real
    /// store's versioned logs as they are and reversed (every winner then out of place).
    #[test]
    fn settle_writes_what_it_wrote_before() {
        let d = tmp("settle-oracle");
        let edges = version_edges();
        let escaped = edges.iter().filter(|l| l.contains("\\u004c4")).collect::<Vec<_>>();
        assert!(
            escaped.len() == 2 && escaped.iter().all(|l| matches!(borg::jsonl::pick(l, ["id"]), Some([Scalar::Str(Cow::Owned(_))]))),
            "an escaped id is read into a key of its own, grouped with the borrowed one"
        );
        let mut cases: Vec<(&str, Vec<String>)> = vec![("edges", edges.clone()), ("reversed", edges.iter().rev().cloned().collect())];
        for f in ["lessons.jsonl", "stats.jsonl"] {
            if let Some(t) = bench_store(f) {
                let lines: Vec<String> = t.lines().map(str::to_string).collect();
                cases.push((f, lines.clone()));
                cases.push((f, lines.into_iter().rev().collect()));
            }
        }
        for (i, (what, lines)) in cases.iter().enumerate() {
            let (old, new) = (written(&d, &format!("old{i}.jsonl"), lines), written(&d, &format!("new{i}.jsonl"), lines));
            old_settle(&old).unwrap();
            settle(&new).unwrap();
            assert!(std::fs::read_to_string(&new).unwrap() == std::fs::read_to_string(&old).unwrap(), "{what}: the bytes differ");
        }
        let _ = std::fs::remove_dir_all(d);
    }

    /// Oracle: a merge that appends writes the same bytes as before; one with nothing new leaves the log
    /// untouched, which on a settled log is what the old settle wrote too.
    #[test]
    fn merge_writes_what_it_wrote_before() {
        let d = tmp("merge-oracle");
        let edges = version_edges();
        let real = bench_store("lessons.jsonl").map(|t| t.lines().map(str::to_string).collect::<Vec<_>>()).unwrap_or_default();
        let (held, incoming) = edges.split_at(edges.len() / 2);
        let cases: [(&str, Vec<String>, Vec<String>); 5] = [
            ("half the edges arrive", held.to_vec(), incoming.to_vec()),
            ("all again", edges.clone(), edges.clone()),
            ("nothing", edges.clone(), vec![]),
            ("garbage", held.to_vec(), vec!["garbage".into(), String::new(), r#"{"id":"L9""#.into()]),
            (
                "a real store's lessons, the older half arriving",
                real.iter().skip(real.len() / 2).cloned().collect(),
                real.iter().take(real.len() / 2).cloned().collect(),
            ),
        ];
        for (i, (what, held, incoming)) in cases.iter().enumerate() {
            let (old, new) = (written(&d, &format!("old{i}.jsonl"), held), written(&d, &format!("new{i}.jsonl"), held));
            // Settled first, as a log the mesh merged into before is.
            old_settle(&old).unwrap();
            settle(&new).unwrap();
            assert_eq!(merge(&new, incoming).unwrap(), old_merge(&old, incoming).unwrap(), "{what}");
            assert!(std::fs::read_to_string(&new).unwrap() == std::fs::read_to_string(&old).unwrap(), "{what}: the bytes differ");
        }
        let _ = std::fs::remove_dir_all(d);
    }

    /// Oracle: merging and settling through one cache, which reads each log whole once and then only what
    /// was appended, write the bytes the whole-read `merge` and `settle` wrote, step after step: the edges
    /// and a real store's lessons and stats arriving in chunks, in order and reversed, with a hook's lines
    /// (older versions among them) appended between merges and settled as a stamp settles them.
    #[test]
    fn a_followed_merge_and_settle_write_what_they_wrote_before() {
        let d = tmp("followed");
        let mut cases: Vec<(&str, Vec<String>)> = vec![("edges", version_edges()), ("edges reversed", version_edges().into_iter().rev().collect())];
        for f in ["lessons.jsonl", "stats.jsonl"] {
            if let Some(t) = bench_store(f) {
                let lines: Vec<String> = t.lines().map(str::to_string).collect();
                cases.push((f, lines.clone()));
                cases.push((f, lines.into_iter().rev().collect()));
            }
        }
        for (i, (what, all)) in cases.iter().enumerate() {
            let chunk = (all.len() / 9).max(1);
            let mut parts = all.chunks(chunk);
            let first = parts.next().unwrap_or_default();
            let (old, new) = (written(&d, &format!("old{i}.jsonl"), first), written(&d, &format!("new{i}.jsonl"), first));
            let cache = Cache::default();
            old_settle(&old).unwrap();
            cache.settle(&new).unwrap();
            for (k, part) in parts.enumerate() {
                if k % 3 == 2 {
                    let raw: String = part.iter().map(|l| format!("{l}\n")).collect();
                    for p in [&old, &new] {
                        std::fs::OpenOptions::new().append(true).open(p).unwrap().write_all(raw.as_bytes()).unwrap();
                    }
                    old_settle(&old).unwrap();
                    cache.settle(&new).unwrap();
                } else {
                    assert_eq!(cache.merge(&new, part).unwrap(), old_merge(&old, part).unwrap(), "{what}, step {k}");
                }
                assert!(std::fs::read(&new).unwrap() == std::fs::read(&old).unwrap(), "{what}, step {k}: the bytes differ");
            }
            assert_eq!(cache.reads().whole, 1, "{what}: read whole once, then followed");
        }
        let _ = std::fs::remove_dir_all(d);
    }

    /// The deliberate edge of the fast path: a merge that brings nothing new leaves even an unsettled
    /// log (a hook appended an older version last) as it is, where the old merge settled it. Such a log
    /// is settled where the hook's line is first seen instead (sync.rs `Store::stamp`).
    #[test]
    fn a_merge_of_nothing_new_leaves_the_log_as_it_is() {
        let d = tmp("merge-nothing");
        let unsettled = vec![lesson("L1", 2, "two"), lesson("L1", 1, "one")];
        for (what, incoming) in [("nothing", vec![]), ("all held", unsettled.clone())] {
            let (old, new) = (written(&d, "old.jsonl", &unsettled), written(&d, "new.jsonl", &unsettled));
            let before = std::fs::read_to_string(&new).unwrap();
            assert_eq!(merge(&new, &incoming).unwrap(), 0, "{what}");
            assert!(std::fs::read_to_string(&new).unwrap() == before, "{what}: untouched");
            assert_eq!(old_merge(&old, &incoming).unwrap(), 0, "{what}");
            assert!(std::fs::read_to_string(&old).unwrap() != before, "{what}: the old merge settled it");
        }
        let _ = std::fs::remove_dir_all(d);
    }

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

    /// The belief snapshot (borg facts.rs) is this node's cache of its own fold: never synced.
    #[test]
    fn the_belief_snapshot_is_never_synced() {
        let d = tmp("facts");
        for f in [borg::facts::FILE, borg::facts::REGRETS_FILE] {
            assert!(!SYNCED.contains(&f) && !logs(&d).contains(&d.join(f)), "{f}");
        }
        let _ = std::fs::remove_dir_all(d);
    }

    /// An engineer's personal lessons (borg reach.rs `PERSONAL_FILE`) stay on their machine: not a synced
    /// log, not stamped or fingerprinted, and none of their lines is in what a store offers its peers.
    #[test]
    fn personal_lessons_are_never_synced_nor_offered() {
        let d = tmp("personal");
        let personal = borg::reach::PERSONAL_FILE;
        assert!(!SYNCED.contains(&personal), "personal.jsonl is no synced log");
        assert!(!logs(&d).contains(&d.join(personal)), "nor stamped, signed or fingerprinted");
        std::fs::write(d.join("lessons.jsonl"), lesson("L1", 1, "team lesson") + "\n").unwrap();
        std::fs::write(d.join(personal), lesson("L2", 1, "my commit style") + "\n").unwrap();
        let offered: Vec<String> = logs(&d).iter().flat_map(|p| read(p).into_values()).collect();
        assert_eq!(offered, [lesson("L1", 1, "team lesson")], "only the team's line is offered");
        // Nor where a `borg move` placed a lesson from (it names the source collective).
        assert!(!SYNCED.contains(&borg::moves::PLACED_FILE) && !logs(&d).contains(&d.join(borg::moves::PLACED_FILE)));
        let _ = std::fs::remove_dir_all(d);
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
