//! What a node's sessions read of its store, kept between them: each log as a snapshot of its lines
//! (lines.rs `Snapshot`) and the origin records as a compact index (origin.rs `Index`), shared by the
//! concurrent sessions of `serve`.
//!
//! A file is read whole once. After that one stat per use says whether it changed: unchanged, nothing is
//! read; grown in place (the logs and the records are appended to), only what was appended is read and
//! parsed; anything else (a quarantine or a message bound renames a new file in, a same-size write moves
//! the ctime) reads it whole again. So a session's cost grows with what changed, not with the store.
//! The writers this node runs keep it so under the lock they write under: a merge or a settle extends the
//! log's snapshot after its append, a rewrite forgets it. A rewrite itself is built from the file read
//! whole under that lock, never from what is held: a write the stat cannot tell would be reverted.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{File, Metadata};
use std::io::{Read as _, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use crate::identity::Identity;
use crate::lines::{self, Hash, Snapshot};
use crate::membership::Membership;
use crate::origin::{self, Index, Origin, Stamped};

/// How many bytes before the point a file was parsed to must read the same for an extension to be
/// trusted (`follow`). Method: one page, read with the tail anyway; a rewrite that grows the file in
/// place and keeps the page before the old end byte for byte is not one this store's writers make
/// (they append, or rename a new file in, which the inode tells).
pub(crate) const GUARD: usize = 4096;

/// A file as a stat saw it: which file (device and inode), its size, its mtime and its ctime. A missing
/// file is the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Mark {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: Option<SystemTime>,
    /// The inode's change time (seconds, nanoseconds): the kernel moves it at every write, truncation and
    /// mtime change, and no caller can set it, so a same-size write that puts the mtime back still shows.
    ctime: (i64, i64),
}

impl Mark {
    fn of(m: &Metadata) -> Mark {
        #[cfg(unix)]
        let (dev, ino, ctime) = {
            use std::os::unix::fs::MetadataExt;
            (m.dev(), m.ino(), (m.ctime(), m.ctime_nsec()))
        };
        #[cfg(not(unix))]
        let (dev, ino, ctime) = (0, 0, (0, 0));
        Mark { dev, ino, len: m.len(), mtime: m.modified().ok(), ctime }
    }

    fn same_file(&self, other: &Mark) -> bool {
        self.ino != 0 && (self.dev, self.ino) == (other.dev, other.ino)
    }
}

/// What a reader of a followed file holds of it (`Follow::held`).
pub(crate) struct Held<'a> {
    /// The stat it was read under.
    pub mark: Mark,
    /// How many bytes it holds.
    pub end: usize,
    /// Where its unterminated last line starts (`end` when it ends with a newline): an extension reads
    /// from here, as that line may have been half written.
    pub parsed: usize,
    /// The bytes before `parsed`, at most `GUARD` of them.
    pub guard: &'a [u8],
}

/// What a followed file holds now (`follow`).
pub(crate) enum Read {
    /// What was read: nothing changed.
    Same,
    /// The text from the held `parsed` on, under the new stat.
    Tail(Mark, String),
    /// The whole text (empty when the file is missing or not UTF-8, as `read_to_string` gives nothing).
    Whole(Mark, String),
}

/// A file read whole once and then followed (`follow`): a log's snapshot, the origin index.
pub(crate) trait Follow: Clone {
    /// What a whole read of the file gives.
    fn whole(mark: Mark, text: String) -> Self;
    /// What it holds, for `follow` to read on from.
    fn held(&self) -> Held<'_>;
    /// `tail`, the text from `held().parsed` on, read under `mark`.
    fn extend(&mut self, mark: Mark, tail: String);
}

/// Reads of `path` past `held`: nothing when one stat says it is unchanged, the appended tail when it is
/// the same file grown past what was read and the `GUARD` bytes before `held.parsed` still read the same,
/// else all of it (a same-size write with a new ctime among them).
/// The stat recorded is the open file's, taken before it is read, and the read stops at the size it saw:
/// what is appended during the read is the next call's tail, so what is held always ends where its stat
/// says.
pub(crate) fn follow(path: &Path, held: Held<'_>) -> Read {
    let now = std::fs::metadata(path).map(|m| Mark::of(&m)).unwrap_or_default();
    if held.mark == now {
        return Read::Same;
    }
    let Ok(mut f) = File::open(path) else { return Read::Whole(Mark::default(), String::new()) };
    let (mark, len) = stat(&f);
    if held.mark.same_file(&mark)
        && mark.len > held.end as u64
        && let Some(tail) = tail(&mut f, &held, len)
    {
        return Read::Tail(mark, tail);
    }
    let text = if f.seek(SeekFrom::Start(0)).is_ok() { read_text(&mut f, len) } else { String::new() };
    Read::Whole(mark, text)
}

/// The whole text of `path` and the stat it was read under (empty when it is missing or not UTF-8), up to
/// the size that stat saw.
pub(crate) fn whole(path: &Path) -> (Mark, String) {
    let Ok(mut f) = File::open(path) else { return (Mark::default(), String::new()) };
    let (mark, len) = stat(&f);
    (mark, read_text(&mut f, len))
}

/// The open file's stat, and how far to read it: the size the stat saw (to the end, when it failed).
fn stat(f: &File) -> (Mark, u64) {
    f.metadata().map_or((Mark::default(), u64::MAX), |m| (Mark::of(&m), m.len()))
}

/// The first `len` bytes of `f`, read from its start, as text, or nothing, as `read_to_string(..)
/// .unwrap_or_default()` reads it.
fn read_text(f: &mut File, len: u64) -> String {
    let mut text = String::new();
    if f.take(len).read_to_string(&mut text).is_err() {
        text.clear();
    }
    text
}

/// The text of `f` from `h.parsed` up to offset `end`, if the guard bytes before it are unchanged and it
/// is UTF-8.
fn tail(f: &mut File, h: &Held<'_>, end: u64) -> Option<String> {
    let from = h.parsed.checked_sub(h.guard.len())?;
    f.seek(SeekFrom::Start(from as u64)).ok()?;
    let mut bytes = Vec::new();
    f.take(end.saturating_sub(from as u64)).read_to_end(&mut bytes).ok()?;
    if !bytes.starts_with(h.guard) {
        return None;
    }
    String::from_utf8(bytes.split_off(h.guard.len())).ok()
}

/// The last `GUARD` bytes of `before` (the text up to a parse point), or of `kept` followed by it when
/// `before` is shorter: the guard of a reader that keeps no text (origin.rs `Index`).
pub(crate) fn guard_after(kept: &[u8], before: &[u8]) -> Vec<u8> {
    let take = |b: &[u8]| b[b.len().saturating_sub(GUARD)..].to_vec();
    if before.len() >= GUARD {
        return take(before);
    }
    take(&[kept, before].concat())
}

/// A new generation for a whole read: a reader whose generation is unchanged holds the same prefix.
pub(crate) fn generation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// How often a cache read its files whole and in part (`Cache::reads`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Reads {
    pub whole: usize,
    pub tail: usize,
}

/// How often a cache decided on from where it left off, and over a whole log (`Cache::decisions`), so a
/// test sees the fast path taken and not only its result.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Decisions {
    /// `Cache::admitted`: from its memo, or over the whole log.
    pub admitted: Ways,
    /// `Cache::stamp`, per log: past where it last stamped, or every line.
    pub stamped: Ways,
    /// `Cache::settle`: the versions read on from the last settle, or the whole text read for them.
    pub settled: Ways,
}

/// One kind of decision, counted (`Decisions`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Ways {
    pub part: usize,
    pub whole: usize,
}

/// A counter of decisions in part and whole (`Ways`).
#[derive(Default)]
struct Count {
    part: AtomicUsize,
    whole: AtomicUsize,
}

impl Count {
    fn add(&self, part: bool) {
        if part { &self.part } else { &self.whole }.fetch_add(1, Ordering::Relaxed);
    }

    fn get(&self) -> Ways {
        Ways { part: self.part.load(Ordering::Relaxed), whole: self.whole.load(Ordering::Relaxed) }
    }
}

/// Where a log was last stamped up to (`Cache::stamp`): every complete line before `at` of the snapshot
/// of generation `log` has a record in the index of generation `index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    log: u64,
    at: usize,
    index: u64,
}

/// The lines of one log whose origin a membership admits (`Cache::admitted`), with what they were decided
/// from: the log's generation and where it was read up to, the index's generation, the membership.
#[derive(Debug, Clone)]
struct Admitted {
    log: u64,
    at: usize,
    index: u64,
    members: Membership,
    set: Arc<BTreeMap<Hash, Range<usize>>>,
    /// Lines without a record yet: looked at again each time, as a stamp or a peer may record them.
    pending: BTreeSet<Hash>,
    /// The log's unterminated last line, if it is in `set` or `pending`: decided again with the tail.
    open: Option<Hash>,
}

impl Admitted {
    /// Decides every line of `log`.
    fn whole(log: &Snapshot, origins: &Index, m: &Membership) -> Admitted {
        Admitted {
            log: log.generation(),
            at: log.parsed(),
            index: origins.generation(),
            members: m.clone(),
            set: Arc::new(log.ranges().filter(|(h, _)| origins.admits(m, h)).map(|(h, r)| (*h, r.clone())).collect()),
            pending: log.hashes().filter(|h| !origins.contains(h)).copied().collect(),
            open: None,
        }
    }

    /// Decides line `h` of `log`: admitted, pending a record, or neither.
    fn decide(&mut self, h: Hash, log: &Snapshot, origins: &Index, m: &Membership) {
        let Some(r) = log.range(&h) else { return };
        match origins.signer(&h) {
            Some((key, ms)) if m.admits(key, ms) => {
                Arc::make_mut(&mut self.set).insert(h, r.clone());
            }
            Some(_) => {}
            None => {
                self.pending.insert(h);
            }
        }
    }
}

/// The snapshots of one store's logs and its origin index, kept between sessions (module doc). `Default`
/// is empty: its first use of each file reads it whole.
#[derive(Default)]
pub struct Cache {
    logs: Mutex<HashMap<PathBuf, Arc<Snapshot>>>,
    origins: Mutex<Option<(PathBuf, Arc<Index>)>>,
    stamped: Mutex<HashMap<PathBuf, Stamp>>,
    admitted: Mutex<HashMap<PathBuf, Admitted>>,
    reads: Count,
    decided_admitted: Count,
    decided_stamped: Count,
    decided_settled: Count,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Cache {
    /// The log at `path` as it is now: a stat, and a read of what changed (module doc).
    pub fn log(&self, path: &Path) -> Arc<Snapshot> {
        let mut logs = lock(&self.logs);
        let s = self.fresh(logs.remove(path), path);
        logs.insert(path.to_path_buf(), s.clone());
        s
    }

    /// The origin records of the store `home` as they are now (`log` for origins.jsonl).
    pub fn origins(&self, home: &Path) -> Arc<Index> {
        let path = origin::path(home);
        let mut held = lock(&self.origins);
        let i = self.fresh(held.take().filter(|(p, _)| *p == path).map(|(_, i)| i), &path);
        *held = Some((path, i.clone()));
        i
    }

    /// How many times this cache read a file whole, and in part.
    pub fn reads(&self) -> Reads {
        let Ways { part, whole } = self.reads.get();
        Reads { whole, tail: part }
    }

    /// How many times this cache decided in part, and whole (`Decisions`).
    pub fn decisions(&self) -> Decisions {
        Decisions { admitted: self.decided_admitted.get(), stamped: self.decided_stamped.get(), settled: self.decided_settled.get() }
    }

    /// Drops what is held of the log at `path`: its writer rewrote it, so the next use reads it whole.
    pub fn forget(&self, path: &Path) {
        lock(&self.logs).remove(path);
    }

    /// `held` brought up to the file at `path` (`follow`), counted; read whole when nothing is held.
    /// Extended in place when no session holds it any more; else the extension is a copy, and the session
    /// keeps what it read.
    fn fresh<T: Follow>(&self, held: Option<Arc<T>>, path: &Path) -> Arc<T> {
        let Some(mut s) = held else {
            self.reads.add(false);
            let (mark, text) = whole(path);
            return Arc::new(T::whole(mark, text));
        };
        match follow(path, s.held()) {
            Read::Same => s,
            Read::Tail(mark, tail) => {
                self.reads.add(true);
                Arc::make_mut(&mut s).extend(mark, tail);
                s
            }
            Read::Whole(mark, text) => {
                self.reads.add(false);
                Arc::new(T::whole(mark, text))
            }
        }
    }

    /// Appends the `incoming` lines the log at `path` lacks, then settles it (lines.rs `merge`, which
    /// reads it whole): its snapshot says which lines are new, and is extended by them.
    pub fn merge(&self, path: &Path, incoming: &[String]) -> Result<usize, String> {
        if incoming.is_empty() {
            return Ok(0);
        }
        let new: Vec<&String> = {
            let log = self.log(path);
            let mut seen = HashSet::new();
            incoming
                .iter()
                .filter(|l| {
                    lines::complete(l) && {
                        let h = lines::hash(l);
                        !log.contains(&h) && seen.insert(h)
                    }
                })
                .collect()
        };
        if new.is_empty() {
            return Ok(0);
        }
        lines::append(path, &new)?;
        self.settle(path)?;
        Ok(new.len())
    }

    /// Makes the winning version of every versioned record of the log at `path` the last line for its id
    /// (lines.rs `settle`), from its snapshot: only the lines appended since the last settle are read for
    /// their versions. The snapshot is then extended by what was appended.
    pub fn settle(&self, path: &Path) -> Result<(), String> {
        if lines::log_name(path) == lines::ABSORBED {
            return Ok(());
        }
        let mut logs = lock(&self.logs);
        let mut s = self.fresh(logs.remove(path), path);
        self.decided_settled.add(s.settles_on());
        let wrote = {
            let stale = Arc::make_mut(&mut s).stale();
            lines::append(path, &stale).map(|()| !stale.is_empty())
        };
        let s = match wrote {
            Ok(true) => self.fresh(Some(s), path),
            _ => s,
        };
        logs.insert(path.to_path_buf(), s);
        wrote.map(|_| ())
    }

    /// Stamps every line of `logs` in the store `home` that has no origin record yet as `me`'s, at `now`:
    /// the records written per log in hash order, each line once (origin.rs `stamp`). A log is looked at
    /// only past where it was last stamped, while neither it nor the index was read whole since.
    pub fn stamp<'l>(&self, home: &Path, me: &Identity, logs: &'l [PathBuf], now: i64) -> Result<Stamped<'l>, String> {
        let index = self.origins(home);
        let (mut new, mut grown, mut fresh, mut done) = (Vec::new(), Vec::new(), BTreeSet::new(), Vec::new());
        for log in logs {
            let snap = self.log(log);
            let before = new.len();
            let since = lock(&self.stamped).get(log).filter(|s| s.log == snap.generation() && s.index == index.generation()).map(|s| s.at);
            self.decided_stamped.add(since.is_some());
            let candidates: BTreeSet<Hash> = match since {
                Some(at) => snap.lines_from(at).map(lines::hash).collect(),
                None => snap.hashes().copied().collect(),
            };
            for h in candidates {
                if !index.contains(&h) && fresh.insert(h) {
                    new.push(Origin::new(me, &h, now));
                }
            }
            if new.len() > before {
                grown.push(log.as_path());
            }
            done.push((log.clone(), snap.generation(), snap.parsed()));
        }
        drop(index);
        // Stamped against the index the records leave: the one read whole when they created the file.
        let index = self.record(home, &new)?.generation();
        lock(&self.stamped).extend(done.into_iter().map(|(path, log, at)| (path, Stamp { log, at, index })));
        Ok(Stamped { new: new.len(), grown })
    }

    /// The lines of `log`, the log at `path`, whose origin in `origins` the membership `m` admits (origin.rs
    /// `Index::admits`), as ranges of it. Decided whole once; then, while neither the log nor the index
    /// was read whole since and the membership is the same, only for the lines appended since and those
    /// that had no record: a record, once read, never changes (the first per line counts).
    /// A snapshot older than the one decided last (a session read it before another's read extended a
    /// copy, which keeps the generation) is decided whole and leaves the memo as it is: the memo's ranges
    /// may lie past its end.
    pub fn admitted(&self, path: &Path, log: &Snapshot, origins: &Index, m: &Membership) -> Arc<BTreeMap<Hash, Range<usize>>> {
        let mut memo = lock(&self.admitted);
        let same = |a: &Admitted| a.log == log.generation() && a.index == origins.generation() && a.members == *m;
        if memo.get(path).is_some_and(|a| same(a) && a.at > log.parsed()) {
            self.decided_admitted.add(false);
            return Admitted::whole(log, origins, m).set;
        }
        let held = memo.remove(path).filter(same);
        self.decided_admitted.add(held.is_some());
        let mut a = match held {
            Some(mut a) => {
                if let Some(h) = a.open.take() {
                    Arc::make_mut(&mut a.set).remove(&h);
                    a.pending.remove(&h);
                }
                for h in std::mem::take(&mut a.pending) {
                    a.decide(h, log, origins, m);
                }
                for h in log.lines_from(a.at).map(lines::hash) {
                    if !a.set.contains_key(&h) && !a.pending.contains(&h) {
                        a.decide(h, log, origins, m);
                    }
                }
                a.at = log.parsed();
                a
            }
            None => Admitted::whole(log, origins, m),
        };
        a.open = log.open().filter(|h| a.set.contains_key(h) || a.pending.contains(h));
        let set = a.set.clone();
        memo.insert(path.to_path_buf(), a);
        set
    }

    /// Appends `records` to the store's origins (origin.rs `append`) and extends the index by them, which
    /// it returns.
    pub fn record<'o>(&self, home: &Path, records: impl IntoIterator<Item = &'o Origin>) -> Result<Arc<Index>, String> {
        origin::append(home, records)?;
        Ok(self.origins(home))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::Duration;

    use super::*;

    /// `lines::read` as it was before the cache: the file read whole, a `Value` parse per line.
    fn read_whole(path: &Path) -> BTreeMap<Hash, String> {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        text.lines().filter(|l| !l.trim().is_empty() && serde_json::from_str::<serde_json::Value>(l).is_ok()).map(|l| (lines::hash(l), l.to_string())).collect()
    }

    fn held(c: &Cache, p: &Path) -> BTreeMap<Hash, String> {
        c.log(p).iter().map(|(h, l)| (*h, l.to_string())).collect()
    }

    fn add(p: &Path, text: &str) {
        std::fs::OpenOptions::new().create(true).append(true).open(p).unwrap().write_all(text.as_bytes()).unwrap();
    }

    /// The same bytes written over the file in place, its size kept and its mtime moved on.
    fn overwrite(p: &Path, text: &str) {
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        (&f).write_all(text.as_bytes()).unwrap();
        let later = std::fs::metadata(p).unwrap().modified().unwrap() + Duration::from_secs(1);
        f.set_modified(later).unwrap();
    }

    /// `text` written over the start of the file in place, its size kept and its mtime put back (as
    /// `cp -p` or `rsync -t` leave it): only the ctime tells. A pause first, so a filesystem with coarse
    /// timestamps moves the ctime past the last change's.
    fn overwrite_keeping_mtime(p: &Path, text: &str) {
        std::thread::sleep(Duration::from_millis(20));
        let was = std::fs::metadata(p).unwrap().modified().unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        (&f).write_all(text.as_bytes()).unwrap();
        f.set_modified(was).unwrap();
    }

    /// Oracle: a followed snapshot holds what a whole read of the file holds after every kind of change a
    /// log sees, and reads it whole exactly when the change is no append: a missing file, appends of whole,
    /// torn, unterminated, repeated and CRLF lines, a rename (shorter, then longer), a same-size write in
    /// place with a new mtime and one with the mtime put back, an in-place rewrite that grows, bytes that
    /// are no UTF-8, a removal.
    #[test]
    fn a_snapshot_follows_appends_and_reads_a_rewrite_whole() {
        let d = std::env::temp_dir().join(format!("borg-mesh-cache-follow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("lessons.jsonl");
        let c = Cache::default();
        let big = format!(r#"{{"id":"pad","q":"{}"}}"#, "p".repeat(2 * GUARD));
        type Step<'a> = (&'a str, Box<dyn Fn() + 'a>, Reads);
        let rename = |text: &str| {
            let tmp = d.join("tmp");
            std::fs::write(&tmp, text).unwrap();
            std::fs::rename(&tmp, &p).unwrap();
        };
        let steps: Vec<Step> = vec![
            ("missing", Box::new(|| ()), Reads { whole: 1, tail: 0 }),
            ("created", Box::new(|| add(&p, "{\"a\":1}\n{\"b\":2}\n")), Reads { whole: 1, tail: 0 }),
            ("unchanged", Box::new(|| ()), Reads { whole: 0, tail: 0 }),
            ("a line", Box::new(|| add(&p, "{\"c\":3}\n")), Reads { whole: 0, tail: 1 }),
            ("a long line", Box::new(|| add(&p, &format!("{big}\n"))), Reads { whole: 0, tail: 1 }),
            ("torn", Box::new(|| add(&p, "{\"d\":")), Reads { whole: 0, tail: 1 }),
            ("completed", Box::new(|| add(&p, "4}\n")), Reads { whole: 0, tail: 1 }),
            ("unterminated", Box::new(|| add(&p, "{\"e\":5}")), Reads { whole: 0, tail: 1 }),
            ("spoilt", Box::new(|| add(&p, "x")), Reads { whole: 0, tail: 1 }),
            ("ended", Box::new(|| add(&p, "\n{\"e\":5}\n")), Reads { whole: 0, tail: 1 }),
            ("a repeat and CRLF", Box::new(|| add(&p, "{\"a\":1}\n{\"f\":6}\r\n{\"g\":7}\r")), Reads { whole: 0, tail: 1 }),
            ("CR ended", Box::new(|| add(&p, "\n")), Reads { whole: 0, tail: 1 }),
            ("renamed shorter", Box::new(|| rename("{\"a\":1}\n")), Reads { whole: 1, tail: 0 }),
            ("renamed longer", Box::new(|| rename(&format!("{{\"a\":1}}\n{big}\n{{\"h\":8}}\n"))), Reads { whole: 1, tail: 0 }),
            ("same size in place", Box::new(|| overwrite(&p, "{\"z\":1}")), Reads { whole: 1, tail: 0 }),
            ("same size in place, mtime put back", Box::new(|| overwrite_keeping_mtime(&p, "{\"y\":2}")), Reads { whole: 1, tail: 0 }),
            (
                "rewritten in place, grown",
                Box::new(|| {
                    let t = std::fs::read_to_string(&p).unwrap().replace("\"h\":8", "\"i\":9");
                    overwrite(&p, &format!("{t}{{\"j\":10}}\n"));
                }),
                Reads { whole: 1, tail: 0 },
            ),
            (
                "no UTF-8",
                Box::new(|| std::fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(&[0xff, b'\n']).unwrap()),
                Reads { whole: 1, tail: 0 },
            ),
            ("removed", Box::new(|| std::fs::remove_file(&p).unwrap()), Reads { whole: 1, tail: 0 }),
        ];
        for (what, change, reads) in steps {
            change();
            let before = c.reads();
            assert_eq!(held(&c, &p), read_whole(&p), "{what}");
            let after = c.reads();
            assert_eq!(Reads { whole: after.whole - before.whole, tail: after.tail - before.tail }, reads, "{what}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A session that still holds a snapshot keeps what it read while the log grows: the cache extends a
    /// copy, and a session that let go has its snapshot extended in place.
    #[test]
    fn a_held_snapshot_stays_as_read() {
        let d = std::env::temp_dir().join(format!("borg-mesh-cache-held-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("lessons.jsonl");
        add(&p, "{\"a\":1}\n");
        let c = Cache::default();
        let first = c.log(&p);
        add(&p, "{\"b\":2}\n");
        let second = c.log(&p);
        assert_eq!((first.len(), second.len()), (1, 2), "the held one as it was read");
        drop(first);
        let at = Arc::as_ptr(&second);
        drop(second);
        add(&p, "{\"c\":3}\n");
        assert_eq!(Arc::as_ptr(&c.log(&p)), at, "extended in place");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// What is appended while a file is read is left to the next read, which takes it as a tail: a whole
    /// read and a tail read both stop at the size their stat saw.
    #[test]
    fn a_write_during_a_read_is_read_next_as_a_tail() {
        let d = std::env::temp_dir().join(format!("borg-mesh-cache-during-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("lessons.jsonl");
        add(&p, "{\"a\":1}\n");
        let mut f = File::open(&p).unwrap();
        let (mark, len) = stat(&f);
        add(&p, "{\"b\":2}\n");
        let s = Snapshot::whole(mark, read_text(&mut f, len));
        assert_eq!(s.text(), "{\"a\":1}\n", "the whole read stops where its stat ended");
        let mut f = File::open(&p).unwrap();
        let (mark, len) = stat(&f);
        add(&p, "{\"c\":3}\n");
        assert_eq!(tail(&mut f, &s.held(), len).as_deref(), Some("{\"b\":2}\n"), "and so does a tail read");
        let mut s = s;
        s.extend(mark, "{\"b\":2}\n".into());
        assert!(matches!(follow(&p, s.held()), Read::Tail(_, t) if t == "{\"c\":3}\n"), "the rest is the next tail");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A session that read a snapshot before another session's read extended a copy of it (the same
    /// generation, a longer text) is admitted what its own snapshot holds, ranges inside it, though the
    /// memo was moved on past it; and the memo still serves the newer snapshot from where it left off.
    #[test]
    fn an_older_snapshot_is_admitted_what_it_holds() {
        let d = std::env::temp_dir().join(format!("borg-mesh-cache-older-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let me = Identity::load_or_create(&d).unwrap();
        let m = Membership::found("work", &me);
        let p = d.join("lessons.jsonl");
        let write = |ls: &[String]| {
            add(&p, &ls.iter().map(|l| format!("{l}\n")).collect::<String>());
            origin::append(&d, &ls.iter().map(|l| Origin::new(&me, &lines::hash(l), 1)).collect::<Vec<_>>()).unwrap();
        };
        let line = |i: usize| format!(r#"{{"id":"L{i}","q":"{}"}}"#, "q".repeat(i * 7));
        write(&[line(0)]);
        let c = Cache::default();
        let older = c.log(&p);
        write(&[line(1), line(2)]);
        let newer = c.log(&p);
        assert_eq!((older.generation(), older.len(), newer.len()), (newer.generation(), 1, 3), "a copy extended");
        let origins = c.origins(&d);
        let whole = |s: &Snapshot| s.ranges().filter(|(h, _)| origins.admits(&m, h)).map(|(h, r)| (*h, r.clone())).collect::<BTreeMap<_, _>>();
        assert_eq!(*c.admitted(&p, &newer, &origins, &m), whole(&newer));
        let before = c.decisions().admitted;
        let set = c.admitted(&p, &older, &origins, &m);
        assert_eq!(*set, whole(&older), "what the older snapshot holds");
        assert!(set.values().all(|r| r.end <= older.text().len()), "every range inside it");
        assert_eq!(c.decisions().admitted, Ways { part: before.part, whole: before.whole + 1 }, "decided whole");
        assert_eq!(*c.admitted(&p, &newer, &origins, &m), whole(&newer));
        assert_eq!(c.decisions().admitted, Ways { part: before.part + 1, whole: before.whole + 1 }, "the memo kept for the newer one");
        let _ = std::fs::remove_dir_all(&d);
    }
}
