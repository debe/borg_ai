//! What a node's sessions read of its store, kept between them: each log as a snapshot of its lines
//! (lines.rs `Snapshot`) and the origin records as a compact index (origin.rs `Index`), shared by the
//! concurrent sessions of `serve`.
//!
//! A file is read whole once (borg follow.rs). After that one stat per use says whether it changed:
//! unchanged, nothing is read; grown in place (the logs and the records are appended to), only what was
//! appended is read and parsed; anything else (a quarantine or a message bound renames a new file in, a
//! same-size write moves the ctime) reads it whole again. So a session's cost grows with what changed,
//! not with the store. The writers this node runs keep it so under the lock they write under: a merge or
//! a settle extends the log's snapshot after its append, a rewrite forgets it. A rewrite itself is built
//! from the file read whole under that lock, never from what is held: a write the stat cannot tell would
//! be reverted.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use borg::follow::{Follow, Read, follow, whole};

use crate::identity::Identity;
use crate::lines::{self, Hash, Snapshot};
use crate::membership::Membership;
use crate::origin::{self, Index, Origin, Stamped};
use crate::recon::{self, Fp, Key};

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
    /// A reconciliation's fingerprints of an admitted set's ranges (`Fingerprints`): from the memo, or
    /// hashed.
    pub fingerprints: Ways,
}

/// One kind of decision, counted (`Decisions`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Ways {
    pub part: usize,
    pub whole: usize,
}

/// A counter of decisions in part and whole (`Ways`).
#[derive(Debug, Default)]
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

/// Lines of a log by hash, as ranges of its text.
type Ranges = BTreeMap<Hash, Range<usize>>;

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
    /// The fingerprints of `set`'s ranges, as its reconciliations hashed them.
    fps: Arc<Fingerprints>,
}

impl Admitted {
    /// Decides every line of `log`; its fingerprints are counted in `count`.
    fn whole(log: &Snapshot, origins: &Index, m: &Membership, count: &Arc<Count>) -> Admitted {
        Admitted {
            log: log.generation(),
            at: log.parsed(),
            index: origins.generation(),
            members: m.clone(),
            set: Arc::new(log.ranges().filter(|(h, _)| origins.admits(m, h)).map(|(h, r)| (*h, r.clone())).collect()),
            pending: log.hashes().filter(|h| !origins.contains(h)).copied().collect(),
            open: None,
            fps: Arc::new(Fingerprints { held: Mutex::default(), count: count.clone() }),
        }
    }

    /// Decides line `h` of `log`: admitted, pending a record, or neither. True if it was added to `set`.
    fn decide(&mut self, h: Hash, log: &Snapshot, origins: &Index, m: &Membership) -> bool {
        let Some(r) = log.range(&h) else { return false };
        match origins.signer(&h) {
            Some((key, ms)) if m.admits(key, ms) => Arc::make_mut(&mut self.set).insert(h, r.clone()).is_none(),
            Some(_) => false,
            None => {
                self.pending.insert(h);
                false
            }
        }
    }
}

/// The fingerprints of the ranges of one admitted set (`Admitted`), as reconciliations of it hashed them
/// (recon.rs `Recon::memoized`). A fingerprint depends only on the hashes in its range, so a line added to
/// or taken from the set forgets the ranges that hold it, one per depth, and keeps the rest: a range an
/// earlier reconciliation hashed is hashed again only after such a change in it. A session
/// holds the memo of the set it reconciles; a change while it does so changes a copy, as for the set.
#[derive(Debug)]
pub(crate) struct Fingerprints {
    held: Mutex<HashMap<Key, Fp>>,
    /// The cache's counter (`Decisions::fingerprints`), shared by every copy.
    count: Arc<Count>,
}

impl Clone for Fingerprints {
    fn clone(&self) -> Self {
        Fingerprints { held: Mutex::new(lock(&self.held).clone()), count: self.count.clone() }
    }
}

impl Fingerprints {
    /// The fingerprints held of the ranges `keys` names, in order, `None` for each not held; counted.
    pub(crate) fn get(&self, keys: impl Iterator<Item = Key>) -> Vec<Option<Fp>> {
        let held = lock(&self.held);
        keys.map(|k| {
            let fp = held.get(&k).copied();
            self.count.add(fp.is_some());
            fp
        })
        .collect()
    }

    /// Keeps the fingerprints a reconciliation hashed.
    pub(crate) fn put(&self, hashed: Vec<(Key, Fp)>) {
        if !hashed.is_empty() {
            lock(&self.held).extend(hashed);
        }
    }

    /// Forgets the fingerprints of every range that holds `h`: it was added to the set or taken from it.
    fn forget(&mut self, h: &Hash) {
        let held = self.held.get_mut().unwrap_or_else(|e| e.into_inner());
        if !held.is_empty() {
            for k in recon::ranges_of(h) {
                held.remove(&k);
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
    /// Shared with every `Fingerprints` this cache keeps.
    fingerprinted: Arc<Count>,
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
        Decisions {
            admitted: self.decided_admitted.get(),
            stamped: self.decided_stamped.get(),
            settled: self.decided_settled.get(),
            fingerprints: self.fingerprinted.get(),
        }
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
        self.admits(path, log, origins, m).0
    }

    /// `admitted`, with the memo of its ranges' fingerprints (`Fingerprints`); none for an older snapshot.
    pub(crate) fn admits(&self, path: &Path, log: &Snapshot, origins: &Index, m: &Membership) -> (Arc<Ranges>, Option<Arc<Fingerprints>>) {
        let mut memo = lock(&self.admitted);
        let same = |a: &Admitted| a.log == log.generation() && a.index == origins.generation() && a.members == *m;
        if memo.get(path).is_some_and(|a| same(a) && a.at > log.parsed()) {
            self.decided_admitted.add(false);
            return (Admitted::whole(log, origins, m, &self.fingerprinted).set, None);
        }
        let held = memo.remove(path).filter(same);
        self.decided_admitted.add(held.is_some());
        let mut a = match held {
            Some(mut a) => {
                // The unterminated last line is decided again with the tail. It was pending or in the set, never
                // both; reopened only if in the set: a pending one admitted now is a line added.
                let reopened = a.open.take().filter(|h| !a.pending.remove(h));
                if let Some(h) = reopened {
                    Arc::make_mut(&mut a.set).remove(&h);
                }
                let mut added = Vec::new();
                for h in std::mem::take(&mut a.pending) {
                    if a.decide(h, log, origins, m) {
                        added.push(h);
                    }
                }
                for h in log.lines_from(a.at).map(lines::hash) {
                    if !a.set.contains_key(&h) && !a.pending.contains(&h) && a.decide(h, log, origins, m) {
                        added.push(h);
                    }
                }
                // The set's hashes changed by those added, but the reopened line decided back into it, and by
                // the reopened line if it left.
                let left = reopened.filter(|h| !a.set.contains_key(h));
                let changed: Vec<Hash> = added.into_iter().filter(|h| Some(*h) != reopened).chain(left).collect();
                if !changed.is_empty() {
                    let fps = Arc::make_mut(&mut a.fps);
                    changed.iter().for_each(|h| fps.forget(h));
                }
                a.at = log.parsed();
                a
            }
            None => Admitted::whole(log, origins, m, &self.fingerprinted),
        };
        a.open = log.open().filter(|h| a.set.contains_key(h) || a.pending.contains(h));
        let held = (a.set.clone(), Some(a.fps.clone()));
        memo.insert(path.to_path_buf(), a);
        held
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

    use borg::follow::GUARD;

    use super::*;
    use crate::recon::Recon;

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

    /// What one side of a reconciliation sent and found, `mine` run to the end against a plain one of
    /// `peer`: its rounds as the wire carries them (serde_json bytes), what it wants and what it gives.
    fn reconciled<V>(mut mine: Recon<'_, V>, peer: &BTreeMap<Hash, ()>) -> (Vec<Vec<u8>>, BTreeSet<Hash>, BTreeSet<Hash>) {
        let mut theirs = Recon::new(peer);
        let mut sent = Vec::new();
        loop {
            let (a, b) = (mine.message(), theirs.message());
            sent.push(serde_json::to_vec(&a).unwrap());
            let done = mine.absorb(&a, b.clone()).unwrap();
            assert_eq!(theirs.absorb(&b, a).unwrap(), done, "both sides stay in step");
            if done {
                return (sent, mine.want, mine.give);
            }
        }
    }

    /// A peer of `set`: it lacks every 41st hash and holds three of its own.
    fn peer_of<V>(set: &BTreeMap<Hash, V>) -> BTreeMap<Hash, ()> {
        let own = (0..3u32).map(|i| *blake3::hash(&i.to_le_bytes()).as_bytes());
        set.keys().enumerate().filter(|(i, _)| i % 41 != 7).map(|(_, h)| *h).chain(own).map(|h| (h, ())).collect()
    }

    /// The memo across sessions: after every kind of change an admitted set sees, a reconciliation with the
    /// cache's fingerprints sends the same rounds, byte for byte, as one that hashes every range, and finds
    /// the same difference; and it takes from the memo every range the change did not touch (`Decisions::
    /// fingerprints`): all of them for an unchanged set, a line that leaves it unchanged (an unterminated one
    /// decided again, the same one ended, one still without a record, an outsider's) included; and a
    /// session that holds the old memo across a change leaves the new one as it was.
    #[test]
    fn memoized_rounds_are_the_hashed_ones() {
        let d = std::env::temp_dir().join(format!("borg-mesh-cache-fps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let me = Identity::load_or_create(&d).unwrap();
        let outsider = Identity::load_or_create(&d.join("outsider")).unwrap();
        let m = Membership::found("work", &me);
        let p = d.join("lessons.jsonl");
        let line = |i: usize| format!(r#"{{"id":"L{i}","q":"{}"}}"#, "q".repeat(i % 13));
        let record = |who: &Identity, l: &str| origin::append(&d, &[Origin::new(who, &lines::hash(l), 1)]).unwrap();
        let write = |ls: std::ops::Range<usize>| {
            for i in ls {
                add(&p, &format!("{}\n", line(i)));
                record(&me, &line(i));
            }
        };
        write(0..600);
        let c = Cache::default();
        type Step<'a> = (&'a str, Box<dyn Fn() + 'a>, Option<usize>);
        let unterminated = line(9001);
        let steps: Vec<Step> = vec![
            ("first", Box::new(|| ()), None),
            ("unchanged", Box::new(|| ()), Some(0)),
            ("a line", Box::new(|| write(600..601)), None),
            ("lines", Box::new(|| write(601..621)), None),
            ("unterminated", Box::new(|| (add(&p, &unterminated), record(&me, &unterminated)).0), None),
            ("unterminated, decided again", Box::new(|| ()), Some(0)),
            ("ended", Box::new(|| add(&p, "\n")), Some(0)),
            ("unterminated, no record", Box::new(|| add(&p, &line(9004))), Some(0)),
            ("still no record", Box::new(|| ()), Some(0)),
            ("its record", Box::new(|| record(&me, &line(9004))), None),
            ("that one ended", Box::new(|| add(&p, "\n")), Some(0)),
            ("no record yet", Box::new(|| add(&p, &format!("{}\n", line(9002)))), Some(0)),
            ("recorded", Box::new(|| record(&me, &line(9002))), None),
            ("an outsider's", Box::new(|| (add(&p, &format!("{}\n", line(9003))), record(&outsider, &line(9003))).0), Some(0)),
        ];
        for (what, change, hashed) in steps {
            change();
            let (log, origins) = (c.log(&p), c.origins(&d));
            let (set, fps) = c.admits(&p, &log, &origins, &m);
            let fps = fps.expect("a memo for the current snapshot");
            let peer = peer_of(&set);
            let before = c.decisions().fingerprints;
            let memoized = reconciled(Recon::memoized(&set, &fps), &peer);
            let after = c.decisions().fingerprints;
            let plain = reconciled(Recon::new(&set), &peer);
            assert_eq!(memoized, plain, "{what}: the same rounds, byte for byte");
            let (part, whole) = (after.part - before.part, after.whole - before.whole);
            if let Some(n) = hashed {
                assert_eq!(whole, n, "{what}: every fingerprint from the memo");
            } else if what != "first" {
                assert!(whole > 0 && part > whole, "{what}: only the ranges on a new line's path hashed: {part} from the memo, {whole} hashed");
            }
        }
        // A session still reconciling the old set when a line is added: the change went to a copy of the
        // memo, so what that session hashes of the old set never reaches the new set's memo.
        let held = c.admits(&p, &c.log(&p), &c.origins(&d), &m);
        let (old_set, old_fps) = (held.0, held.1.expect("a memo"));
        write(700..701);
        let (set, fps) = c.admits(&p, &c.log(&p), &c.origins(&d), &m);
        let fps = fps.expect("a memo for the current snapshot");
        assert!(!Arc::ptr_eq(&old_fps, &fps), "a held memo is copied on change");
        let old_peer = peer_of(&old_set);
        assert_eq!(reconciled(Recon::memoized(&old_set, &old_fps), &old_peer), reconciled(Recon::new(&old_set), &old_peer), "the old session");
        let peer = peer_of(&set);
        assert_eq!(reconciled(Recon::memoized(&set, &fps), &peer), reconciled(Recon::new(&set), &peer), "the new set's memo untouched by it");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// On the real store copy (`BORG_BENCH_STORE`): each log's hashes grown in file order by chunks, as
    /// appends grow them, each added hash forgotten as `Cache::admits` forgets it; after each, a
    /// reconciliation with the memo sends what one without it sends, byte for byte, against a peer that holds
    /// the next line and one that lacks the last chunk.
    #[test]
    fn memoized_rounds_on_the_real_store() {
        let Some(store) = std::env::var_os("BORG_BENCH_STORE") else {
            eprintln!("BORG_BENCH_STORE unset: the real-store oracle is skipped");
            return;
        };
        let mut hits = 0;
        for f in lines::SYNCED {
            let text = std::fs::read_to_string(Path::new(&store).join(f)).unwrap_or_default();
            let mut seen = HashSet::new();
            let order: Vec<Hash> = text.lines().filter(|l| !l.trim().is_empty()).map(lines::hash).filter(|h| seen.insert(*h)).collect();
            if order.len() < 20 {
                continue;
            }
            let count = Arc::new(Count::default());
            let mut fps = Fingerprints { held: Mutex::default(), count: count.clone() };
            let start = order.len() * 9 / 10;
            let mut set: BTreeMap<Hash, ()> = order[..start].iter().map(|h| (*h, ())).collect();
            let mut bounds = vec![start, start + 1];
            bounds.extend((1..=5).map(|k| start + 1 + (order.len() - start - 1) * k / 5));
            for w in bounds.windows(2) {
                let chunk = &order[w[0]..w[1]];
                for h in chunk {
                    set.insert(*h, ());
                    fps.forget(h);
                }
                let next: BTreeMap<Hash, ()> = set.keys().chain(order.get(w[1])).map(|h| (*h, ())).collect();
                let lacks: BTreeMap<Hash, ()> = set.keys().filter(|h| !chunk.contains(h)).map(|h| (*h, ())).collect();
                for peer in [&next, &lacks, &set] {
                    assert_eq!(reconciled(Recon::memoized(&set, &fps), peer), reconciled(Recon::new(&set), peer), "{f} at {}", w[1]);
                }
            }
            hits += count.get().part;
        }
        assert!(hits > 0, "the memo served fingerprints");
    }
}
