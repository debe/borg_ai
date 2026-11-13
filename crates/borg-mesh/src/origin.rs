//! Where each synced line came from: `<store>/mesh/origins.jsonl`, one signed record per line hash.
//!
//! ```json
//! { "h": "<blake3 of the line>", "key": "<hex key of the node that first stamped it>", "ms": 1760000000000,
//!   "sig": "<that key's signature of borg-origin-v1, h and ms>" }
//! ```
//!
//! A node stamps the lines its own hooks wrote (any line without a record) with its key and the time
//! it first saw them; a line received from a peer travels with its record, which the receiver
//! verifies before storing either. The membership then decides whether a line is admitted
//! (`Membership::admits`): a member's always, a revoked key's only if stamped before the revocation.
//! Records are verified on receipt, not on every load: only this node writes this file.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use borg::follow::{self, Follow, Held, Mark};

use crate::cache::Cache;
use crate::identity::{self, Identity};
use crate::lines::{self, Hash, Snapshot};
use crate::membership::Membership;
use crate::{from_hex, from_lower_hex, to_hex};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub h: String,
    pub key: String,
    pub ms: i64,
    pub sig: String,
}

fn signed(h: &str, ms: i64) -> Vec<u8> {
    format!("borg-origin-v1\n{h}\n{ms}").into_bytes()
}

impl Origin {
    pub fn new(me: &Identity, h: &Hash, ms: i64) -> Origin {
        let h = to_hex(h);
        let sig = me.sign(&signed(&h, ms));
        Origin { key: me.public(), h, ms, sig }
    }

    /// The record is `key`'s signature for exactly this line.
    pub fn verify(&self, line: &str) -> Result<(), String> {
        if from_hex::<32>(&self.h)? != lines::hash(line) {
            return Err("origin names another line".into());
        }
        identity::verify(&self.key, &signed(&self.h, self.ms), &self.sig).map_err(|e| format!("origin of {}: {e}", &self.h[..8]))
    }
}

pub fn path(home: &Path) -> PathBuf {
    identity::dir(home).join("origins.jsonl")
}

/// The first record per line hash, as owned `Origin`s (`Index` holds them compact). For simulation and
/// tests; a node reads its records through its cache (cache.rs `Cache::origins`).
pub fn load(home: &Path) -> HashMap<Hash, Origin> {
    Index::read(home).origins()
}

pub fn append<'o>(home: &Path, records: impl IntoIterator<Item = &'o Origin>) -> Result<(), String> {
    let text: String = records.into_iter().map(|o| format!("{}\n", serde_json::to_string(o).unwrap())).collect();
    if text.is_empty() {
        return Ok(());
    }
    let p = path(home);
    std::fs::create_dir_all(identity::dir(home)).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    f.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", p.display()))
}

/// `Origin` borrowed from its line: the same fields, so the same lines parse. Hex holds no escape, but a
/// field that does is still read.
#[derive(Deserialize)]
struct OriginRef<'a> {
    #[serde(borrow)]
    h: Cow<'a, str>,
    #[serde(borrow)]
    key: Cow<'a, str>,
    ms: i64,
    #[serde(borrow)]
    sig: Cow<'a, str>,
}

/// One record as `Index` holds it: the signer's key interned (`Index::keys`), the time, and the signature
/// as bytes. Its `Origin` is written back from them (`Index::origin`), byte for byte what the file held.
#[derive(Debug, Clone, Copy)]
struct Record {
    key: u32,
    ms: i64,
    sig: [u8; 64],
}

/// The origin records of a store (`load`, first record per line hash), compact: a record takes 80 bytes, its
/// signer's key held once per member. Read whole once, then on from where it was left (cache.rs).
/// A record whose hash or signature is not written as `to_hex` writes them (only a peer's could be) is
/// kept whole (`odd`), so what this node gives on is what it received.
#[derive(Debug, Clone, Default)]
pub struct Index {
    keys: Vec<Box<str>>,
    key_ids: HashMap<Box<str>, u32>,
    records: HashMap<Hash, Record>,
    odd: HashMap<Hash, Origin>,
    /// The hash the unterminated last line added: taken out again before it is parsed again.
    open: Option<Hash>,
    mark: Mark,
    end: usize,
    parsed: usize,
    guard: Vec<u8>,
    generation: u64,
}

impl Index {
    /// The records of the store `home`, read whole now.
    pub fn read(home: &Path) -> Index {
        let (mark, text) = follow::whole(&path(home));
        Index::whole(mark, text)
    }

    /// Whether line `h` has a record.
    pub fn contains(&self, h: &Hash) -> bool {
        self.records.contains_key(h) || self.odd.contains_key(h)
    }

    pub fn len(&self) -> usize {
        self.records.len() + self.odd.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Who stamped line `h`, and when: its record's key and ms.
    pub fn signer(&self, h: &Hash) -> Option<(&str, i64)> {
        match self.records.get(h) {
            Some(r) => Some((&self.keys[r.key as usize], r.ms)),
            None => self.odd.get(h).map(|o| (o.key.as_str(), o.ms)),
        }
    }

    /// Whether the membership admits line `h` (a line without a record is not shared yet).
    pub fn admits(&self, m: &Membership, h: &Hash) -> bool {
        self.signer(h).is_some_and(|(key, ms)| m.admits(key, ms))
    }

    /// Line `h`'s record, as the file held it: written back from its parts for a frame.
    pub fn origin(&self, h: &Hash) -> Option<Origin> {
        match self.records.get(h) {
            Some(r) => Some(Origin { h: to_hex(h), key: self.keys[r.key as usize].to_string(), ms: r.ms, sig: to_hex(&r.sig) }),
            None => self.odd.get(h).cloned(),
        }
    }

    /// Every record, owned (`load`).
    pub fn origins(&self) -> HashMap<Hash, Origin> {
        self.records.keys().chain(self.odd.keys()).filter_map(|h| Some((*h, self.origin(h)?))).collect()
    }

    /// Same generation, same prefix: it was only extended since (cache.rs `generation`).
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Reads the lines of `text` (which starts at the file's byte `self.parsed`) into the index, the
    /// first record per hash kept, and moves `parsed` to its unterminated last line.
    fn index(&mut self, text: &str) {
        if let Some(h) = self.open.take() {
            self.records.remove(&h);
            self.odd.remove(&h);
        }
        let ends_open = !text.is_empty() && !text.ends_with('\n');
        let last = if ends_open { text.rfind('\n').map_or(0, |i| i + 1) } else { text.len() };
        for (start, l) in lines::split(text, 0) {
            if let Some(h) = self.add(l)
                && ends_open
                && start >= last
            {
                self.open = Some(h);
            }
        }
        self.guard = follow::guard_after(&self.guard, &text.as_bytes()[..last]);
        self.parsed += last;
        self.end = self.parsed + (text.len() - last);
    }

    /// Adds the record on line `l` if it is one and its hash has none yet; returns that hash.
    fn add(&mut self, l: &str) -> Option<Hash> {
        let o = serde_json::from_str::<OriginRef>(l).ok()?;
        let h = from_hex::<32>(&o.h).ok()?;
        if self.contains(&h) {
            return None;
        }
        let compact = match (from_lower_hex::<32>(&o.h), from_lower_hex::<64>(&o.sig)) {
            (Some(_), Some(sig)) => self.intern(&o.key).map(|key| Record { key, ms: o.ms, sig }),
            _ => None,
        };
        match compact {
            Some(r) => {
                self.records.insert(h, r);
            }
            None => {
                self.odd.insert(h, Origin { h: o.h.into_owned(), key: o.key.into_owned(), ms: o.ms, sig: o.sig.into_owned() });
            }
        }
        Some(h)
    }

    /// The index of `key` in `keys`, added if new (`None` past `u32` keys: the record is kept whole).
    fn intern(&mut self, key: &str) -> Option<u32> {
        if let Some(i) = self.key_ids.get(key) {
            return Some(*i);
        }
        let i = u32::try_from(self.keys.len()).ok()?;
        self.keys.push(key.into());
        self.key_ids.insert(key.into(), i);
        Some(i)
    }
}

impl Follow for Index {
    fn whole(mark: Mark, text: String) -> Index {
        let mut i = Index { mark, generation: follow::generation(), ..Index::default() };
        i.index(&text);
        i
    }

    fn held(&self) -> Held<'_> {
        Held { mark: self.mark, end: self.end, parsed: self.parsed, guard: &self.guard }
    }

    fn extend(&mut self, mark: Mark, tail: String) {
        self.mark = mark;
        self.index(&tail);
    }
}

/// What a stamp found: how many lines it stamped, and the logs that held them (this node's hooks
/// wrote to them since the last stamp).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Stamped<'l> {
    pub new: usize,
    pub grown: Vec<&'l Path>,
}

/// The lines of `set` the membership admits (`Index::admits`), borrowed from it.
pub fn admitted<'a>(set: &'a Snapshot, origins: &Index, m: &Membership) -> BTreeMap<Hash, &'a str> {
    set.iter().filter(|(h, _)| origins.admits(m, h)).map(|(h, l)| (*h, l)).collect()
}

/// Applies revocations to a log already on disk: lines whose origin the membership no longer
/// admits move to `<store>/mesh/quarantine.jsonl`, so the conscience stops asking them. Returns
/// how many moved. The log is read whole, as what is renamed over it is built from it, and forgotten in
/// `cache` once rewritten.
pub fn quarantine(home: &Path, log: &Path, m: &Membership, origins: &Index, cache: &Cache) -> Result<usize, String> {
    let refused = |l: &str| origins.signer(&lines::hash(l)).is_some_and(|(key, ms)| !m.admits(key, ms));
    // Read to rename under the log's lock, which its writers take for each append (borg live.rs
    // `locked`: the lessons' learn, the jsonl appends): a line appended meanwhile is never lost. Read
    // whole, not from the cache: a write its stat cannot tell would be reverted by the rename.
    let moved = borg::live::locked(log, || -> Result<usize, String> {
        let (_, text) = follow::whole(log);
        let (out, keep): (Vec<&str>, Vec<&str>) = text.lines().partition(|l| refused(l));
        if out.is_empty() {
            return Ok(0);
        }
        let q = identity::dir(home).join("quarantine.jsonl");
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&q).map_err(|e| format!("{}: {e}", q.display()))?;
        f.write_all(out.iter().map(|l| format!("{l}\n")).collect::<String>().as_bytes()).map_err(|e| e.to_string())?;
        let tmp = log.with_extension("jsonl.quarantine-tmp");
        std::fs::write(&tmp, keep.iter().map(|l| format!("{l}\n")).collect::<String>()).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, log).map_err(|e| format!("{}: {e}", log.display()))?;
        cache.forget(log);
        Ok(out.len())
    })?;
    if moved > 0 {
        // A removed line may have been the winning version of a lesson: make the winner among the
        // remaining lines the last one again, or the conscience would ask an older version.
        cache.settle(log)?;
    }
    Ok(moved)
}

/// `<store>/mesh/quarantined-at`: which membership (`fingerprint`) the logs were last checked against.
fn marker(home: &Path) -> PathBuf {
    identity::dir(home).join("quarantined-at")
}

/// The membership as this node applies it: the signed version and the fork revocations on top.
fn fingerprint(m: &Membership) -> String {
    to_hex(blake3::hash(&serde_json::to_vec(m).expect("membership serialises")).as_bytes())
}

/// Quarantines every log of `home` against `m` and records that it did. Returns how many lines moved.
pub fn apply(home: &Path, m: &Membership, cache: &Cache) -> Result<usize, String> {
    let origins = cache.origins(home);
    let mut moved = 0;
    for log in lines::logs(home) {
        moved += quarantine(home, &log, m, &origins, cache)?;
    }
    std::fs::create_dir_all(identity::dir(home)).map_err(|e| e.to_string())?;
    std::fs::write(marker(home), fingerprint(m)).map_err(|e| format!("{}: {e}", marker(home).display()))?;
    Ok(moved)
}

/// `apply`, unless the logs were already checked against exactly this membership.
pub fn apply_if_new(home: &Path, m: &Membership, cache: &Cache) -> Result<usize, String> {
    if std::fs::read_to_string(marker(home)).is_ok_and(|t| t.trim() == fingerprint(m)) {
        return Ok(0);
    }
    apply(home, m, cache)
}

/// `borg mesh join`: installs the documents an admin handed over (one version, or the history
/// file) and quarantines what the resulting membership no longer admits. Returns the installed
/// membership and how many lines moved.
pub fn join(home: &Path, collective: &str, docs: &[Membership], cache: &Cache) -> Result<(Membership, usize), String> {
    for d in docs {
        d.verify()?;
        if d.collective != collective {
            return Err(format!("the membership of {:?}, this directory belongs to {collective:?}", d.collective));
        }
    }
    let installed = match Membership::load(home)? {
        Some(_) => match Membership::adopt(home, docs)? {
            (m, true) => m,
            (_, false) => return Err("nothing newer than the installed membership that an admin of it signed; hand over membership-history.jsonl".into()),
        },
        None => {
            docs.iter().min_by_key(|d| d.version).ok_or("no membership document")?.save(home)?;
            Membership::adopt(home, docs)?.0
        }
    };
    let moved = apply(home, &installed, cache)?;
    Ok((installed, moved))
}

/// `borg mesh revoke`: the admin's new version, and its own logs quarantined at once (a line the
/// revoked node stamped after `now`, a clock running ahead, may already be here).
pub fn revoke(home: &Path, admin: &Identity, key: &str, now: i64, cache: &Cache) -> Result<(Membership, usize), String> {
    let mut m = Membership::load(home)?.ok_or("no membership")?;
    m.revoke(admin, key, now)?;
    m.save(home)?;
    let moved = apply(home, &m, cache)?;
    Ok((m, moved))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashSet};

    use super::*;

    /// `load` as it was: an owned `Origin` per line, the first per hash kept.
    fn old_load(home: &Path) -> HashMap<Hash, Origin> {
        let mut out = HashMap::new();
        for l in std::fs::read_to_string(path(home)).unwrap_or_default().lines() {
            let Ok(o) = serde_json::from_str::<Origin>(l) else { continue };
            let Ok(h) = from_hex::<32>(&o.h) else { continue };
            out.entry(h).or_insert(o);
        }
        out
    }

    /// `stamp` as it was: every record loaded whole, every line copied.
    fn old_stamp(home: &Path, me: &Identity, logs: &[PathBuf], now: i64) -> Result<usize, String> {
        let mut known: HashSet<Hash> = old_load(home).into_keys().collect();
        let mut new = Vec::new();
        for log in logs {
            let text = std::fs::read_to_string(log).unwrap_or_default();
            let read: BTreeSet<Hash> =
                text.lines().filter(|l| !l.trim().is_empty() && serde_json::from_str::<serde_json::Value>(l).is_ok()).map(lines::hash).collect();
            for h in read {
                if known.insert(h) {
                    new.push(Origin::new(me, &h, now));
                }
            }
        }
        append(home, &new)?;
        Ok(new.len())
    }

    /// Oracle: the borrowed pass over origins.jsonl counts exactly the hashes `load` held, and `stamp`
    /// writes the same records in the same order, on a real store's origins and logs and at the edges.
    #[test]
    fn stamping_over_hashes_writes_what_it_wrote_before() {
        let base = std::env::temp_dir().join(format!("borg-mesh-origin-oracle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (old, new) = (base.join("old"), base.join("new"));
        let me = Identity::load_or_create(&old).unwrap();
        let h = |l: &str| to_hex(&lines::hash(l));
        let edges = [
            serde_json::to_string(&Origin::new(&me, &lines::hash("{\"a\":1}"), 5)).unwrap(),
            format!(r#"{{"h":"{}","key":"k","ms":1,"sig":"s"}}"#, h("{\"b\":2}")),
            format!(r#"{{"h":"{}","key":"k","ms":1,"sig":"s","h":"{}"}}"#, h("{\"c\":3}"), h("{\"d\":4}")),
            format!(r#"{{"h":"{}","key":"k","ms":"1","sig":"s"}}"#, h("{\"e\":5}")),
            format!(r#"{{"h":"{}","key":"k","sig":"s"}}"#, h("{\"f\":6}")),
            format!(r#"{{"h":"0{}","key":"k","ms":1,"sig":"s"}}"#, &h("{\"g\":7}")[1..]),
            format!(r#"["{}","k",1,"s"]"#, h("{\"i\":8}")),
            r#"{"h":"zz","key":"k","ms":1,"sig":"s"}"#.into(),
            format!(r#"{{"h":"{}","key":"k","ms":1,"sig":"s","extra":"\ud800"}}"#, h("{\"j\":9}")),
            format!(r#"{{"h":"{}","key":"k","ms":1"#, h("{\"k\":10}")),
            String::new(),
        ];
        let real = lines::bench_store("mesh/origins.jsonl").unwrap_or_default();
        let origins: String = edges.iter().map(|l| format!("{l}\n")).collect::<String>() + &real;
        let logs = ["lessons.jsonl", "stats.jsonl", "reasoning.jsonl"];
        let edge_log =
            ["{\"a\":1}", "{\"b\":2}", "{\"c\":3}", "{\"d\":4}", "{\"e\":5}", "{\"f\":6}", "{\"g\":7}", "{\"i\":8}", "{\"j\":9}", "{\"new\":1}", "torn"];
        for home in [&old, &new] {
            std::fs::create_dir_all(identity::dir(home)).unwrap();
            if home != &old {
                std::fs::copy(identity::path(&old), identity::path(home)).unwrap();
                let _ = std::fs::copy(identity::public_path(&old), identity::public_path(home));
            }
            std::fs::write(path(home), &origins).unwrap();
            for f in logs {
                let text = lines::bench_store(f).unwrap_or_default();
                let extra = if f == "lessons.jsonl" { edge_log.join("\n") + "\n" } else { String::new() };
                std::fs::write(home.join(f), text + &extra).unwrap();
            }
        }
        assert_eq!(load(&new), old_load(&new), "the same records, each written back as the file held it");
        let me_new = Identity::load_or_create(&new).unwrap();
        assert_eq!(me_new.public(), me.public(), "one key on both copies");
        let paths = |home: &Path| logs.iter().map(|f| home.join(f)).collect::<Vec<_>>();
        let new_paths = paths(&new);
        let stamped = Cache::default().stamp(&new, &me_new, &new_paths, 9).unwrap();
        assert_eq!(stamped.new, old_stamp(&old, &me, &paths(&old), 9).unwrap());
        assert!(stamped.new >= 2, "lines without a record were stamped: {}", stamped.new);
        assert!(stamped.grown.contains(&new_paths[0].as_path()), "the log that held them grew");
        assert!(std::fs::read_to_string(path(&new)).unwrap() == std::fs::read_to_string(path(&old)).unwrap(), "the same records written");

        // Followed: a cache that stamped before stamps only what hooks appended since, and writes what a
        // fresh read writes; its index is then what `load` reads, records appended by another process too.
        let (warm, cold) = (base.join("warm"), base.join("cold"));
        let cache = Cache::default();
        for home in [&warm, &cold] {
            std::fs::create_dir_all(identity::dir(home)).unwrap();
            std::fs::write(path(home), &origins).unwrap();
            for f in logs {
                std::fs::copy(new.join(f), home.join(f)).unwrap();
            }
        }
        let (warm_paths, cold_paths) = (paths(&warm), paths(&cold));
        assert_eq!(cache.stamp(&warm, &me_new, &warm_paths, 9).unwrap().new, Cache::default().stamp(&cold, &me_new, &cold_paths, 9).unwrap().new);
        let hooked = [r#"{"hook":1}"#, r#"{"hook":2}"#, "{\"a\":1}", "torn"].map(|l| format!("{l}\n")).concat();
        for home in [&warm, &cold] {
            for f in ["lessons.jsonl", "reasoning.jsonl"] {
                std::fs::OpenOptions::new().append(true).open(home.join(f)).unwrap().write_all(hooked.as_bytes()).unwrap();
            }
            append(home, &[Origin::new(&me, &lines::hash(r#"{"hook":2}"#), 3)]).unwrap();
        }
        let before = cache.reads();
        let (w, c) = (cache.stamp(&warm, &me_new, &warm_paths, 10).unwrap(), Cache::default().stamp(&cold, &me_new, &cold_paths, 10).unwrap());
        assert_eq!((w.new, w.grown.len()), (1, 1), "the new line, once, in the log that first holds it");
        assert_eq!((w.new, w.grown.len()), (c.new, c.grown.len()));
        assert_eq!(cache.reads().whole, before.whole, "nothing read whole again");
        assert!(std::fs::read_to_string(path(&warm)).unwrap() == std::fs::read_to_string(path(&cold)).unwrap(), "the same records written");
        assert_eq!(cache.origins(&warm).origins(), old_load(&warm), "the index followed the file");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Oracle: the index a cache follows holds the records `load` read before after every append: edge
    /// records (no `Origin`, a capital or padded hash, a signature not in lowercase hex, a duplicate hash,
    /// a key with an escape, torn and unterminated lines) and a real store's records, arriving in pieces
    /// cut anywhere, mid-line included; read whole once.
    #[test]
    fn a_followed_index_holds_what_load_read() {
        let home = std::env::temp_dir().join(format!("borg-mesh-origin-follow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let me = Identity::load_or_create(&home).unwrap();
        let h = |l: &str| to_hex(&lines::hash(l));
        let sig = to_hex(&[0xab; 64]);
        let edges = [
            serde_json::to_string(&Origin::new(&me, &lines::hash("{\"a\":1}"), 5)).unwrap(),
            format!(r#"{{"h":"{}","key":"k","ms":1,"sig":"{sig}"}}"#, h("{\"b\":2}").to_uppercase()),
            format!(r#"{{"h":" {}","key":"k","ms":1,"sig":"{sig}"}}"#, h("{\"c\":3}")),
            format!(r#"{{"h":"{}","key":"k","ms":1,"sig":"{}"}}"#, h("{\"d\":4}"), sig.to_uppercase()),
            format!(r#"{{"h":"{}","key":"k","ms":1,"sig":"short"}}"#, h("{\"e\":5}")),
            format!(r#"{{"h":"{}","key":"\u006b","ms":2,"sig":"{sig}"}}"#, h("{\"f\":6}")),
            format!(r#"{{"h":"{}","key":"other","ms":3,"sig":"{sig}"}}"#, h("{\"a\":1}")),
            format!(r#"{{"h":"{}","key":"k","ms":"1","sig":"{sig}"}}"#, h("{\"g\":7}")),
            format!(r#"["{}","k",1,"{sig}"]"#, h("{\"i\":8}")),
            r#"{"h":"zz","key":"k","ms":1,"sig":"s"}"#.into(),
            String::new(),
        ];
        let text: String = edges.iter().map(|l| format!("{l}\n")).collect::<String>() + &lines::bench_store("mesh/origins.jsonl").unwrap_or_default();
        std::fs::create_dir_all(identity::dir(&home)).unwrap();
        let cuts: Vec<usize> = (1..12).map(|k| k * text.len() / 12).filter(|c| text.is_char_boundary(*c)).collect();
        std::fs::write(path(&home), &text[..cuts[0]]).unwrap();
        let cache = Cache::default();
        assert_eq!(cache.origins(&home).origins(), old_load(&home), "read whole");
        for w in cuts.windows(2).map(|w| (w[0], w[1])).chain([(cuts[cuts.len() - 1], text.len())]) {
            std::fs::OpenOptions::new().append(true).open(path(&home)).unwrap().write_all(&text.as_bytes()[w.0..w.1]).unwrap();
            assert_eq!(cache.origins(&home).origins(), old_load(&home), "after bytes {}..{}", w.0, w.1);
        }
        assert_eq!(cache.reads().whole, 1, "then followed");
        let index = cache.origins(&home);
        assert!(index.records.len() > 1 && index.odd.len() >= 4, "compact and whole records both held");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn records_verify_only_for_their_line_and_key() {
        let home = std::env::temp_dir().join(format!("borg-mesh-origin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let me = Identity::load_or_create(&home).unwrap();
        let o = Origin::new(&me, &lines::hash("{\"a\":1}"), 5);
        assert!(o.verify("{\"a\":1}").is_ok());
        assert!(o.verify("{\"a\":2}").is_err(), "another line");
        let backdated = Origin { ms: 4, ..o.clone() };
        assert!(backdated.verify("{\"a\":1}").is_err(), "the time is signed");
        let log = home.join("lessons.jsonl");
        std::fs::write(&log, "{\"a\":1}\n{\"b\":2}\n").unwrap();
        let (logs, cache) = ([log], Cache::default());
        assert_eq!(cache.stamp(&home, &me, &logs, 7).unwrap(), Stamped { new: 2, grown: vec![logs[0].as_path()] });
        assert_eq!(cache.stamp(&home, &me, &logs, 8).unwrap(), Stamped::default(), "stamped once");
        assert_eq!(Cache::default().stamp(&home, &me, &logs, 8).unwrap(), Stamped::default(), "also as read whole");
        let _ = std::fs::remove_dir_all(&home);
    }

    fn lesson(id: &str, version: u32, q: &str) -> String {
        serde_json::json!({"id": id, "origin_ms": 1, "question": q, "version": version}).to_string()
    }

    /// B holds L1 v3, then merges [v2 (bob), v4 (carol)]: the file reads v3, v2, v4. Carol is
    /// revoked, v4 is quarantined, and the conscience must ask v3 (as every other node does), not
    /// the v2 that is now last.
    #[test]
    fn quarantine_settles_so_the_version_never_goes_back() {
        let home = std::env::temp_dir().join(format!("borg-mesh-origin-settle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let (admin, bob, carol) = (
            Identity::load_or_create(&home).unwrap(),
            Identity::load_or_create(&home.join("bob")).unwrap(),
            Identity::load_or_create(&home.join("carol")).unwrap(),
        );
        let mut m = Membership::found("work", &admin);
        m.invite(&admin, &bob.public(), "member").unwrap();
        m.invite(&admin, &carol.public(), "member").unwrap();
        let log = home.join("lessons.jsonl");
        let (v3, v2, v4) = (lesson("L1", 3, "three"), lesson("L1", 2, "two"), lesson("L1", 4, "four"));
        std::fs::write(&log, format!("{v3}\n")).unwrap();
        lines::merge(&log, &[v2.clone(), v4.clone()]).unwrap();
        let o = |who: &Identity, l: &str, ms| Origin::new(who, &lines::hash(l), ms);
        append(&home, &[o(&admin, &v3, 1), o(&bob, &v2, 2), o(&carol, &v4, 50)]).unwrap();
        let asked = || borg::conscience::load(&log, None).into_iter().map(|c| c.question).collect::<Vec<_>>();
        assert_eq!(asked(), vec!["four".to_string()]);

        m.revoke(&admin, &carol.public(), 10).unwrap();
        assert_eq!(apply(&home, &m, &Cache::default()).unwrap(), 1);
        assert_eq!(asked(), vec!["three".to_string()], "the highest remaining version, not the last line");
        assert_eq!(apply_if_new(&home, &m, &Cache::default()).unwrap(), 0, "checked against this membership already");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A quarantine rewrites a log while a hook appends to it: both take the log's lock, so the
    /// rewrite waits for the writer, and the line appended is kept.
    #[test]
    fn quarantine_and_appends_share_the_log_lock() {
        let home = std::env::temp_dir().join(format!("borg-mesh-origin-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let (admin, carol) = (Identity::load_or_create(&home).unwrap(), Identity::load_or_create(&home.join("carol")).unwrap());
        let mut m = Membership::found("work", &admin);
        m.invite(&admin, &carol.public(), "member").unwrap();
        let log = home.join("lessons.jsonl");
        let bad = &lesson("L1", 1, "carol's");
        std::fs::write(&log, format!("{bad}\n")).unwrap();
        append(&home, &[Origin::new(&carol, &lines::hash(bad), 50)]).unwrap();
        m.revoke(&admin, &carol.public(), 10).unwrap();
        let wait = std::time::Duration::from_millis(300);
        // A writer holds the lock: the quarantine waits for it.
        let moved = borg::live::locked(&log, || {
            let (h, l, m) = (home.clone(), log.clone(), m.clone());
            let t = std::thread::spawn(move || quarantine(&h, &l, &m, &Index::read(&h), &Cache::default()).unwrap());
            std::thread::sleep(wait);
            assert!(!t.is_finished(), "the rewrite waits for the writer");
            std::fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(format!("{}\n", lesson("L2", 1, "kept")).as_bytes()).unwrap();
            t
        })
        .join()
        .unwrap();
        assert_eq!(moved, 1);
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().collect::<Vec<_>>(), [lesson("L2", 1, "kept")]);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Carol's outcome line, stamped after her revocation, moves to quarantine and stops counting.
    #[test]
    fn a_revoked_key_s_outcomes_are_quarantined() {
        let home = std::env::temp_dir().join(format!("borg-mesh-origin-outcomes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let (admin, carol) = (Identity::load_or_create(&home).unwrap(), Identity::load_or_create(&home.join("carol")).unwrap());
        let mut m = Membership::found("work", &admin);
        m.invite(&admin, &carol.public(), "member").unwrap();
        std::fs::write(home.join("lessons.jsonl"), format!("{}\n", lesson("L1", 1, "one"))).unwrap();
        let line = |s: &str| {
            let x = borg::outcome::Shared {
                kind: borg::outcome::CHANGED.into(),
                s: s.into(),
                lessons: vec!["L1".into()],
                ms: 5,
                node: "n".into(),
                ..Default::default()
            };
            serde_json::to_string(&x).unwrap()
        };
        let (good, bad) = (line("t1"), line("t2"));
        let team = home.join(borg::outcome::TEAM_FILE);
        std::fs::write(&team, format!("{good}\n{bad}\n")).unwrap();
        append(&home, &[Origin::new(&admin, &lines::hash(&good), 1), Origin::new(&carol, &lines::hash(&bad), 50)]).unwrap();
        let l1 = [borg::conscience::Challenge { id: "L1".into(), ..Default::default() }];
        let changed = || borg::memory::derive(&borg::outcome::read_team(&home.join("lessons.jsonl")), &l1, None)["L1"].changed;
        assert_eq!(changed(), 2);
        m.revoke(&admin, &carol.public(), 10).unwrap();
        assert_eq!(apply(&home, &m, &Cache::default()).unwrap(), 1);
        assert_eq!(changed(), 1, "the revoked key's result no longer counts");
        assert!(std::fs::read_to_string(identity::dir(&home).join("quarantine.jsonl")).unwrap().contains("\"t2\""));
        let _ = std::fs::remove_dir_all(&home);
    }
}
