//! The belief facts: what memory.rs `derive` and trust.rs `Trust::compute` read from a store's outcome
//! history, folded incrementally and kept beside the logs, so a hook reads only what was appended since
//! the last one instead of the whole history (conscience.rs `load_as`, every hook's lessons).
//!
//! **The fold.** Everything those two read is a max, a min or a union per key (memory.rs `Folded`,
//! outcome.rs `sessions`), so folding a prefix of a log and then the rest equals folding all of it, in
//! any order and with lines repeated. The facts keep the two logs apart: this machine's
//! `outcomes.jsonl` (`local`, raw strings, each distinct session and repo `tag`ged once) and the team's
//! `outcomes-team.jsonl` (`team`, by session tag). What outcome.rs `team_view` does to them is applied
//! when they are read (`Facts::view`), so the facts serve the view with sharing on and off: this
//! machine's own published copies dropped (`mine`), a peer's `tested` line counted only for lessons its
//! session was given, a regret's session the first line's (this machine's log first, then by offset).
//! Lines parse exactly as outcome.rs `read` and `team_parts` parse them. `team_parts` keeps the first of
//! duplicate team lines; the fold keeps their union, which is the same for the lines `publish` writes
//! (deterministic per record) and needs no dedup set.
//!
//! **Regrets.** Most lines of a long history are regrets (12,577 of 17,293 on one store), each its own
//! (lesson, cause) key, while `derive` needs only their sum per (lesson, deciding session) (memory.rs
//! `blame`: 76 pairs there). So the facts keep both: the sums per view (`Facts::blame`, in the snapshot,
//! all a hook reads) and the regrets behind them (`Regrets`, by lesson and a fingerprint of the cause as
//! each view names it, in `REGRETS_FILE`), read only by a fold that meets a regret line or a team session
//! that turned out to be this machine's, which then sums them again (`settle`).
//!
//! **The snapshot.** `<store>/beliefs.bin` (`FILE`): a fixed little-endian layout (magic, the fold's
//! `rules`, the regrets' generation, string table, tags, the two parts, the sums), ending in the blake3
//! of the body; `<store>/beliefs-regrets.bin` (`REGRETS_FILE`) likewise, carrying the generation it was
//! written as. Each part carries its log's checkpoint: device, inode, the offset folded up to (always a
//! line end), and the blake3 of the log's first 4 KiB and of the 4 KiB before the offset. A hook
//! verifies it, folds the complete lines after it and saves the snapshot again when it moved. A mismatch
//! (the log replaced by a rename, as borg-mesh's quarantine does, rewritten in place, or shorter) folds
//! that log again from the start; a rewrite of this machine's log folds the team's again too, for `mine`
//! may have shrunk. An unterminated last line counts for the hook that reads it, as `read` counts it, and
//! is never saved. An edit in the middle of a log that leaves both windows intact goes unnoticed: nothing
//! in borg writes one, and `borg learn`, which reads the logs whole anyway, folds them afresh
//! (`rebuild`). Lessons are never part of the fold (they and `BORG_BEFORE` apply at read), so a lessons
//! edit changes nothing here.
//!
//! **Versions.** What a line leaves in the facts depends on this binary's rules (outcome.rs
//! `SHARED_KINDS`, `is_result`, `tag`; memory.rs `ticks`, `result_of`, `is_correction_of`), and a
//! checkpoint of an append-only log still matches after an upgrade. So both files carry `rules`, the
//! hash of borg's version: a snapshot of another version fails `decode` and is folded afresh, once.
//! Every change to those rules ships with a new version.
//!
//! **Concurrency.** Hooks run as many short processes at once. None takes a lock: each writes a
//! `.<pid>.tmp` file beside the snapshot and renames it over it, the last writer winning, the regrets
//! file first.
//! Every saved snapshot is the fold of a prefix of each log, so an older one only costs the next hook a
//! longer tail. One whose regrets file was since replaced by another generation (two hooks folded at
//! once) is void as soon as a fold needs its regrets: that hook folds afresh. A file that is truncated,
//! corrupt or of another version fails to decode and is rebuilt. They are read whole, never mapped (a
//! mapped file truncated under a reader faults).
//!
//! **Place.** Local to the store, like the selection cache (select/summary.rs): borg-mesh syncs an
//! allowlist (lines.rs `SYNCED`) that names neither.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::memory::{self, Folded};
use crate::outcome::{self, Outcome, Shared};

/// The snapshot beside the logs (module doc).
pub const FILE: &str = "beliefs.bin";
/// The regrets behind the snapshot's sums (module doc).
pub const REGRETS_FILE: &str = "beliefs-regrets.bin";
const MAGIC: &[u8; 8] = b"BORGFCT2";
const REGRETS_MAGIC: &[u8; 8] = b"BORGRGT2";
/// The bytes hashed at the start of a log and before its checkpoint.
const WINDOW: u64 = 4096;

/// An interned string.
type Id = u32;
/// A cause as a view names it: the first 16 bytes of its blake3 (a regret's cause is only compared).
type Fp = u128;

fn fp(s: &str) -> Fp {
    u128::from_le_bytes(blake3::hash(s.as_bytes()).as_bytes()[..16].try_into().expect("16 bytes"))
}

/// The fold's rules (module doc "Versions"): the hash of borg's version.
fn rules() -> [u8; 32] {
    *blake3::hash(concat!("borg-facts\0", env!("CARGO_PKG_VERSION")).as_bytes()).as_bytes()
}

/// The fold's hash: rustc's FxHash (a rotate, a xor and a multiply per word), for keys that are ids
/// assigned in fold order, tuples of them and blake3 fingerprints. Never for a string of a log: a peer's
/// lines (borg-mesh syncs `outcomes-team.jsonl`) could choose strings that collide under FxHash, so every
/// map keyed by strings (`Facts::index`, memory.rs `Folded`, `View::trust`) keeps std's keyed SipHash.
#[derive(Default, Clone, Copy)]
pub struct Fx(u64);

impl Fx {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl Hasher for Fx {
    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks::<8>();
        for w in words {
            self.add(u64::from_le_bytes(*w));
        }
        if !rest.is_empty() {
            let mut w = [0u8; 8];
            w[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(w) ^ (rest.len() as u64) << 56);
        }
    }
    fn write_u8(&mut self, x: u8) {
        self.add(u64::from(x));
    }
    fn write_u32(&mut self, x: u32) {
        self.add(u64::from(x));
    }
    fn write_u64(&mut self, x: u64) {
        self.add(x);
    }
    fn write_u128(&mut self, x: u128) {
        self.add(x as u64);
        self.add((x >> 64) as u64);
    }
    fn write_usize(&mut self, x: usize) {
        self.add(x as u64);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

/// A map and a set under `Fx`.
pub type FxMap<K, V> = HashMap<K, V, BuildHasherDefault<Fx>>;
pub type FxSet<K> = HashSet<K, BuildHasherDefault<Fx>>;

/// One session of a log.
#[derive(Debug, Default, Clone, PartialEq)]
struct Sess {
    /// The first ms of a line that ticks the activity clock (memory.rs `ticks`), None for none.
    tick: Option<i64>,
    /// The non-empty repos of its ticking lines.
    repos: Vec<Id>,
    /// The ms of its corrections (memory.rs `is_correction_of`), distinct.
    corrections: Vec<i64>,
}

/// One (lesson, session) of a log.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Cell {
    /// The largest memory.rs `result_of`, -1 for none.
    r: i8,
    /// A change seen (whatever a later correction made `r`).
    used: bool,
    /// A green run of this machine's (a team line's waits for the gate, `Part::tested`).
    tested: bool,
    /// outcome.rs `sessions`: 0 none, 1 not corrected, 2 corrected; and the earliest ms of those.
    trust: u8,
    trust_ms: i64,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { r: -1, used: false, tested: false, trust: 0, trust_ms: 0 }
    }
}

/// How far a log is folded, and what it looked like there (module doc).
#[derive(Debug, Default, Clone, PartialEq)]
struct Check {
    dev: u64,
    ino: u64,
    at: u64,
    head: [u8; 32],
    tail: [u8; 32],
}

/// The fold of one log, but its regrets (`Regrets`).
#[derive(Debug, Default, Clone, PartialEq)]
struct Part {
    check: Check,
    sessions: FxMap<Id, Sess>,
    cells: FxMap<(Id, Id), Cell>,
    /// (lesson, session, repo) of each change.
    use_repos: FxSet<(Id, Id, Id)>,
    /// The team's only: each `tested` line (session, lessons), gated when read; and the (session,
    /// lesson) pairs its other lines name (outcome.rs `team_parts`).
    tested: FxSet<(Id, Vec<Id>)>,
    given: FxSet<(Id, Id)>,
}

impl Part {
    fn cell(&mut self, l: Id, s: Id) -> &mut Cell {
        self.cells.entry((l, s)).or_default()
    }
}

/// A regret as a view merges its lines: the largest credit, the offset of the first line, its session.
type Merged = (f64, u64, Id);
/// `Merged` across both logs: the place of the first line is (from the team's log, offset).
type Placed = (f64, (bool, u64), Id);

/// The regrets of both logs (module doc "Regrets").
#[derive(Debug, Default, Clone, PartialEq)]
struct Regrets {
    /// This machine's, by (lesson, cause) as the view off names the cause (raw) and as the view on
    /// does (its `tag`).
    off: FxMap<(Id, Fp), Merged>,
    on: FxMap<(Id, Fp), Merged>,
    /// The team's by (lesson, cause, session): the largest credit and the offset of the first line.
    /// Merged when summed, without the sessions that turned out to be this machine's (`mine` grows).
    team: FxMap<(Id, Fp, Id), (f64, u64)>,
}

impl Regrets {
    fn is_empty(&self) -> bool {
        self.off.is_empty() && self.on.is_empty() && self.team.is_empty()
    }
}

/// Merges a line of a regret into `m` (memory.rs `Folded::of`: the largest credit, the first session).
fn merge(m: &mut FxMap<(Id, Fp), Merged>, key: (Id, Fp), credit: f64, off: u64, s: Id) {
    let e = m.entry(key).or_insert((0.0, off, s));
    e.0 = e.0.max(credit);
    if off < e.1 {
        (e.1, e.2) = (off, s);
    }
}

/// The folded logs of one store (module doc).
#[derive(Debug, Default, Clone)]
pub struct Facts {
    strings: Vec<String>,
    /// Each string's id, built when a fold first needs one (a hook that folds nothing hashes no string).
    index: HashMap<String, Id>,
    /// Each raw session and repo of this machine's log to its outcome.rs `tag` ("" stays "").
    tags: FxMap<Id, Id>,
    local: Part,
    team: Part,
    /// The tags of this machine's sessions: the team lines `team_parts` drops. Not saved (from `local`).
    mine: FxSet<Id>,
    /// Per view (off, on): memory.rs `blame` of its regrets, (lesson, session, credit) by ids.
    blame: [Vec<(Id, Id, f64)>; 2],
    /// The generation of the regrets file the sums are of; 0 for no regrets.
    generation: u128,
    /// The regrets, None until a fold needs them (read from `regrets_file` at `generation`).
    regrets: Option<Regrets>,
    regrets_file: Option<PathBuf>,
    /// The regrets changed since they were read: written as a new generation at the next save.
    changed: bool,
    /// `blame` is to be summed again from `regrets` (`settle`).
    stale: bool,
    /// The regrets were needed and their file is not of `generation`: these facts are void.
    lost: bool,
}

/// The saved facts are equal (what neither file holds, nor the regrets, which `blame` sums).
impl PartialEq for Facts {
    fn eq(&self, o: &Self) -> bool {
        (&self.strings, &self.tags, &self.local, &self.team, &self.blame) == (&o.strings, &o.tags, &o.local, &o.team, &o.blame)
    }
}

/// One record as the fold reads it, from either log.
struct Rec<'a> {
    kind: &'a str,
    session: &'a str,
    lessons: &'a [String],
    ms: i64,
    repo: &'a str,
    channel: &'a str,
    cause: &'a str,
    credit: f64,
}

#[cfg(test)]
thread_local! {
    /// Lines a fold parsed and regrets files read, on this thread (the tests' proof of a warm load).
    static PARSED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static REGRET_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Facts {
    fn id(&mut self, s: &str) -> Id {
        if self.index.len() != self.strings.len() {
            self.index = self.strings.iter().enumerate().map(|(i, s)| (s.clone(), i as Id)).collect();
        }
        if let Some(i) = self.index.get(s) {
            return *i;
        }
        let i = self.strings.len() as Id;
        self.strings.push(s.to_string());
        self.index.insert(s.to_string(), i);
        i
    }

    /// The `tag` of a raw string of this machine's log, hashed once per string.
    fn tagged(&mut self, raw: Id) -> Id {
        if let Some(t) = self.tags.get(&raw) {
            return *t;
        }
        let s = &self.strings[raw as usize];
        let t = if s.is_empty() { String::new() } else { outcome::tag(s) };
        let t = self.id(&t);
        self.tags.insert(raw, t);
        t
    }

    /// The regrets, read on first need; None (and the facts void) when their file is not of this
    /// generation.
    fn regrets(&mut self) -> Option<&mut Regrets> {
        if self.regrets.is_none() {
            let read = match self.generation {
                0 => Some(Regrets::default()),
                generation => {
                    #[cfg(test)]
                    REGRET_READS.with(|c| c.set(c.get() + 1));
                    let bytes = self.regrets_file.as_deref().and_then(|p| std::fs::read(p).ok());
                    bytes.and_then(|b| decode_regrets(&b, generation, self.strings.len()))
                }
            };
            match read {
                Some(r) => self.regrets = Some(r),
                None => {
                    self.lost = true;
                    return None;
                }
            }
        }
        self.regrets.as_mut()
    }

    /// Drops the regrets of a log folded again from the start (this machine's: all of them, the team's
    /// is folded again too).
    fn forget(&mut self, team: bool) {
        if team {
            match self.regrets() {
                Some(r) if !r.team.is_empty() => r.team.clear(),
                _ => return,
            }
        } else {
            self.regrets = Some(Regrets::default());
        }
        (self.changed, self.stale) = (true, true);
    }

    /// Folds the lines of `bytes`, which start at offset `base` of the log.
    fn fold(&mut self, team: bool, bytes: &[u8], base: u64) {
        let mut pos = 0;
        for line in bytes.split(|b| *b == b'\n') {
            let off = base + pos as u64;
            pos += line.len() + 1;
            #[cfg(test)]
            PARSED.with(|c| c.set(c.get() + 1));
            // As `str::lines` reads a line.
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let Ok(text) = std::str::from_utf8(line) else { continue };
            if team {
                let Ok(x) = serde_json::from_str::<Shared>(text) else { continue };
                if outcome::SHARED_KINDS.contains(&x.kind.as_str()) {
                    let rec =
                        Rec { kind: &x.kind, session: &x.s, lessons: &x.lessons, ms: x.ms, repo: &x.r, channel: &x.channel, cause: &x.c, credit: x.credit };
                    self.add(true, &rec, off);
                }
            } else if let Ok(o) = serde_json::from_str::<Outcome>(text) {
                let rec = Rec {
                    kind: &o.kind,
                    session: &o.session,
                    lessons: &o.lessons,
                    ms: o.ms,
                    repo: &o.repo,
                    channel: &o.channel,
                    cause: &o.cause,
                    credit: o.credit,
                };
                self.add(false, &rec, off);
            }
        }
    }

    /// Folds one record at offset `off`: memory.rs `Folded::of` and outcome.rs `sessions` per key.
    fn add(&mut self, team: bool, o: &Rec<'_>, off: u64) {
        let s = self.id(o.session);
        if !team {
            let t = self.tagged(s);
            // A team session folded before it was this machine's: its regrets leave the view on.
            if self.mine.insert(t) && self.team.sessions.contains_key(&t) && self.regrets().is_some() {
                self.stale = true;
            }
            self.local.sessions.entry(s).or_default();
        } else if self.mine.contains(&s) {
            // A copy of this machine's own record, which `team_parts` drops. `mine` only grows between
            // two folds of the team's log (a rewrite of the local one folds it again), and `view` and
            // `settle` drop what was folded before a session became this machine's.
            return;
        }
        let lessons: Vec<Id> = o.lessons.iter().map(|l| self.id(l)).collect();
        let repo = self.id(o.repo);
        if !team {
            self.tagged(repo);
        }
        if team && o.kind != outcome::TESTED {
            self.team.given.extend(lessons.iter().map(|l| (s, *l)));
        }
        // outcome.rs `is_result`: no evidence for any reader of the fold.
        if o.channel == outcome::PLACEBO || matches!(o.kind, outcome::UNCLEAR | outcome::REMAINS | outcome::NOT_ARISEN) {
            return;
        }
        let part = if team { &mut self.team } else { &mut self.local };
        let sess = part.sessions.entry(s).or_default();
        if memory::ticks(o.kind) {
            sess.tick = Some(sess.tick.map_or(o.ms, |t| t.min(o.ms)));
            if !o.repo.is_empty() && !sess.repos.contains(&repo) {
                sess.repos.push(repo);
            }
        }
        if memory::is_correction_of(o.kind, o.cause) && !sess.corrections.contains(&o.ms) {
            sess.corrections.push(o.ms);
        }
        if o.kind == outcome::TESTED {
            if team {
                part.tested.insert((s, lessons));
            } else {
                lessons.iter().for_each(|l| part.cell(*l, s).tested = true);
            }
            return;
        }
        if o.kind == outcome::REGRETTED {
            let credit = o.credit.clamp(0.0, 1.0);
            // The cause as each view names it: the team's as written (a tag), this machine's raw (off)
            // and tagged (on, outcome.rs `team_view`).
            let named = if o.cause.is_empty() { String::new() } else { outcome::tag(o.cause) };
            let (raw, named) = (fp(o.cause), fp(&named));
            let Some(r) = self.regrets() else { return };
            for l in lessons {
                if team {
                    let e = r.team.entry((l, raw, s)).or_insert((0.0, off));
                    e.0 = e.0.max(credit);
                    e.1 = e.1.min(off);
                } else {
                    merge(&mut r.off, (l, raw), credit, off, s);
                    merge(&mut r.on, (l, named), credit, off, s);
                }
            }
            (self.changed, self.stale) = (true, true);
            return;
        }
        let trust = match o.kind {
            outcome::CORRECTED | outcome::VETOED => 2,
            outcome::NOT_CORRECTED => 1,
            _ => 0,
        };
        let r = memory::result_of(o.kind);
        if r.is_none() && trust == 0 {
            return;
        }
        for l in lessons {
            let c = part.cell(l, s);
            if let Some(r) = r {
                c.r = c.r.max(r as i8);
                c.used |= r == 1;
            }
            if trust > 0 {
                c.trust_ms = if c.trust == 0 { o.ms } else { c.trust_ms.min(o.ms) };
                c.trust = c.trust.max(trust);
            }
            if r == Some(1) {
                part.use_repos.insert((l, s, repo));
            }
        }
    }

    /// Sums the regrets per view again when they changed (module doc "Regrets"): as `view` would merge
    /// them, this machine's lines before the team's, then by offset.
    fn settle(&mut self) {
        if !self.stale {
            return;
        }
        let Some(r) = self.regrets.as_ref() else { return };
        let off = memory::blame(r.off.iter().map(|((l, _), (credit, _, s))| (*l, *s, *credit)));
        let mut on: FxMap<(Id, Fp), Placed> = FxMap::default();
        on.reserve(r.on.len() + r.team.len());
        on.extend(r.on.iter().map(|(k, (credit, off, s))| (*k, (*credit, (false, *off), *s))));
        for ((l, c, s), (credit, off)) in r.team.iter().filter(|((_, _, s), _)| !self.mine.contains(s)) {
            let order = (true, *off);
            let e = on.entry((*l, *c)).or_insert((0.0, order, *s));
            e.0 = e.0.max(*credit);
            if order < e.1 {
                (e.1, e.2) = (order, *s);
            }
        }
        let on = memory::blame(on.into_iter().map(|((l, _), (credit, _, s))| (l, s, credit)));
        self.blame = [off, on];
        self.stale = false;
    }
}

/// What `derive` and `Trust` read, as outcome.rs `team_view` would show them.
pub struct View<'a> {
    pub folded: Folded<'a>,
    /// outcome.rs `sessions`.
    pub trust: HashMap<&'a str, HashMap<&'a str, (bool, i64)>>,
}

impl Facts {
    /// The view with the team's outcomes (`on`, outcome.rs `on`) or without: O(facts), no log read.
    pub fn view(&self, on: bool) -> View<'_> {
        let st = |i: Id| self.strings[i as usize].as_str();
        // A string of this machine's log as the view names it: its tag with the team's (`team_view`).
        let local = |i: Id| if on { self.tags.get(&i).copied().unwrap_or(i) } else { i };
        let named = |team: bool, i: Id| if team { i } else { local(i) };
        let parts: &[(&Part, bool)] = if on { &[(&self.local, false), (&self.team, true)] } else { &[(&self.local, false)] };
        let keep = |team: bool, s: Id| !team || !self.mine.contains(&s);
        let mut f = Folded::default();
        let mut trust: HashMap<&str, HashMap<&str, (bool, i64)>> = HashMap::new();
        let mut ticks: FxMap<Id, (i64, FxSet<Id>)> = FxMap::default();
        for (p, team) in parts.iter().copied() {
            for (s, x) in p.sessions.iter().filter(|(s, _)| keep(team, **s)) {
                if !x.corrections.is_empty() {
                    f.corrections.entry(st(*s)).or_default().extend(x.corrections.iter().copied());
                }
                if let Some(tick) = x.tick {
                    let e = ticks.entry(*s).or_insert((tick, FxSet::default()));
                    e.0 = e.0.min(tick);
                    e.1.extend(x.repos.iter().map(|r| named(team, *r)));
                }
            }
            for ((l, s), c) in p.cells.iter().filter(|((_, s), _)| keep(team, *s)) {
                let (l, s) = (st(*l), st(*s));
                if c.r >= 0 {
                    let e = f.result.entry(l).or_default().entry(s).or_insert(c.r as u8);
                    *e = (*e).max(c.r as u8);
                }
                if c.used {
                    f.uses.entry(l).or_default().insert(s);
                }
                if c.tested {
                    f.tested.entry(l).or_default().insert(s);
                }
                if c.trust > 0 {
                    let e = trust.entry(l).or_default().entry(s).or_insert((c.trust == 2, c.trust_ms));
                    e.0 |= c.trust == 2;
                    e.1 = e.1.min(c.trust_ms);
                }
            }
            for (l, _, r) in p.use_repos.iter().filter(|(_, s, _)| keep(team, *s)) {
                f.repos.entry(st(*l)).or_default().insert(st(named(team, *r)));
            }
            for (s, ls) in p.tested.iter().filter(|(s, _)| keep(team, *s)) {
                if ls.iter().all(|l| p.given.contains(&(*s, *l))) {
                    for l in ls {
                        f.tested.entry(st(*l)).or_default().insert(st(*s));
                    }
                }
            }
        }
        let clock = ticks.into_iter().map(|(s, (ms, repos))| (st(s).to_string(), ms, repos.into_iter().map(|r| st(r).to_string()).collect()));
        f.clock = memory::Clock::from_sessions(clock);
        f.regret = self.blame[usize::from(on)].iter().map(|(l, s, credit)| (st(*l), st(*s), *credit)).collect();
        View { folded: f, trust }
    }
}

/// The blake3 of `f`'s bytes [from, to); all ones when they cannot be read (never a log's hash).
fn window(f: &mut std::fs::File, from: u64, to: u64) -> [u8; 32] {
    let mut buf = vec![0u8; (to - from) as usize];
    match f.seek(SeekFrom::Start(from)).and_then(|_| f.read_exact(&mut buf)) {
        Ok(()) => *blake3::hash(&buf).as_bytes(),
        Err(_) => [0xff; 32],
    }
}

/// Where `catch_up` left a log.
struct Caught {
    /// The part differs from the saved one.
    moved: bool,
    /// It was folded again from the start.
    rebuilt: bool,
    /// The unterminated last line, and its offset.
    rest: Vec<u8>,
    at: u64,
}

impl Facts {
    fn part(&mut self, team: bool) -> &mut Part {
        if team { &mut self.team } else { &mut self.local }
    }

    /// Brings the part of `log` up to date: verifies its checkpoint (a mismatch folds the log from the
    /// start), then folds the complete lines after it.
    fn catch_up(&mut self, team: bool, log: &Path) -> Caught {
        let Ok(mut f) = std::fs::File::open(log) else {
            let gone = *self.part(team) != Part::default();
            if gone {
                *self.part(team) = Part::default();
                self.forget(team);
            }
            return Caught { moved: gone, rebuilt: gone, rest: Vec::new(), at: 0 };
        };
        let Ok(meta) = f.metadata() else { return Caught { moved: false, rebuilt: false, rest: Vec::new(), at: 0 } };
        let len = meta.len();
        let c = self.part(team).check.clone();
        let same = c.at == 0
            || (c.dev == meta.dev()
                && c.ino == meta.ino()
                && len >= c.at
                && window(&mut f, 0, c.at.min(WINDOW)) == c.head
                && window(&mut f, c.at.saturating_sub(WINDOW), c.at) == c.tail);
        let rebuilt = !same;
        if rebuilt {
            *self.part(team) = Part::default();
            self.forget(team);
        }
        let at = self.part(team).check.at;
        let mut rest = Vec::new();
        if f.seek(SeekFrom::Start(at)).and_then(|_| f.read_to_end(&mut rest)).is_err() {
            return Caught { moved: rebuilt, rebuilt, rest: Vec::new(), at };
        }
        let done = rest.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let tail = rest.split_off(done);
        if done > 0 {
            self.fold(team, &rest[..done - 1], at);
            let to = at + done as u64;
            let (head, end) = (window(&mut f, 0, to.min(WINDOW)), window(&mut f, to.saturating_sub(WINDOW), to));
            self.part(team).check = Check { dev: meta.dev(), ino: meta.ino(), at: to, head, tail: end };
        }
        Caught { moved: rebuilt || done > 0, rebuilt, rest: tail, at: at + done as u64 }
    }

    /// Both logs beside `lessons` folded on: returns whether the facts moved and each log's unterminated
    /// last line (local, team) with its offset.
    fn catch_up_store(&mut self, lessons: &Path) -> (bool, [(Vec<u8>, u64); 2]) {
        let local = self.catch_up(false, &outcome::path_beside(lessons));
        if local.rebuilt {
            // `mine` may have shrunk: the team's lines it dropped may count now (`forget` dropped the
            // team's regrets with this machine's).
            self.team = Part::default();
            self.mine = self.local.sessions.keys().filter_map(|s| self.tags.get(s).copied()).collect();
        }
        let team = self.catch_up(true, &lessons.with_file_name(outcome::TEAM_FILE));
        (local.moved || team.moved, [(local.rest, local.at), (team.rest, team.at)])
    }

    /// The unterminated last lines, as `read` counts them (never saved).
    fn fold_rests(&mut self, rests: [(Vec<u8>, u64); 2]) {
        for (team, (rest, at)) in [false, true].into_iter().zip(rests) {
            if !rest.is_empty() {
                self.fold(team, &rest, at);
            }
        }
    }

    /// Folds what was appended since these facts (and saves them when they moved and `save`); None when
    /// they turned out void (`lost`).
    fn update(mut self, lessons: &Path, save: bool) -> Option<Facts> {
        self.regrets_file = Some(regrets_beside(lessons));
        let (moved, rests) = self.catch_up_store(lessons);
        if self.lost {
            return None;
        }
        self.settle();
        if moved && save {
            store(lessons, &mut self);
        }
        self.fold_rests(rests);
        self.settle();
        (!self.lost).then_some(self)
    }
}

/// The snapshot beside the lessons file `lessons`.
pub fn path_beside(lessons: &Path) -> PathBuf {
    lessons.with_file_name(FILE)
}

/// The regrets file beside the lessons file `lessons`.
pub fn regrets_beside(lessons: &Path) -> PathBuf {
    lessons.with_file_name(REGRETS_FILE)
}

/// The facts of the logs beside `lessons`, up to date: the snapshot plus what was appended since (all of
/// it when the snapshot is missing, does not match or turns out void), saved again when they moved.
pub fn load(lessons: &Path) -> Facts {
    let saved = std::fs::read(path_beside(lessons)).ok().and_then(|b| decode(&b)).unwrap_or_default();
    // A fold from nothing never reads a regrets file, so it is never void.
    saved.update(lessons, true).or_else(|| Facts::default().update(lessons, true)).unwrap_or_default()
}

/// The facts of the logs beside `lessons` folded from the start, ignoring the snapshot.
#[cfg(test)]
fn fresh(lessons: &Path) -> Facts {
    Facts::default().update(lessons, false).expect("a fold from nothing reads no regrets file")
}

/// Folds the logs beside `lessons` from the start and replaces the snapshot (`borg learn`, which read
/// them whole: module doc). Nothing when there is no log to fold.
pub fn rebuild(lessons: &Path) {
    let mut facts = Facts { regrets_file: Some(regrets_beside(lessons)), ..Facts::default() };
    let (moved, _) = facts.catch_up_store(lessons);
    facts.settle();
    if moved {
        store(lessons, &mut facts);
    }
}

/// Writes `bytes` to `path`: a temp file, then a rename (module doc).
fn write_atomic(path: &Path, bytes: &[u8]) -> bool {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let ok = std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, path).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
    }
    ok
}

/// A new generation of the regrets file: unique per write (process, time, a counter), never 0.
fn new_generation() -> u128 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut h = blake3::Hasher::new();
    h.update(&std::process::id().to_le_bytes());
    h.update(&std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()).to_le_bytes());
    h.update(&N.fetch_add(1, std::sync::atomic::Ordering::Relaxed).to_le_bytes());
    u128::from_le_bytes(h.finalize().as_bytes()[..16].try_into().expect("16 bytes")).max(1)
}

/// Saves the facts beside `lessons`: changed regrets first, as a new generation, then the snapshot that
/// names it. A failure costs the next hook a fold.
fn store(lessons: &Path, facts: &mut Facts) {
    if facts.changed {
        let Some(r) = facts.regrets.as_ref() else { return };
        let generation = match r.is_empty() {
            true => 0,
            false => new_generation(),
        };
        if generation != 0 && !write_atomic(&regrets_beside(lessons), &encode_regrets(r, generation)) {
            return;
        }
        (facts.generation, facts.changed) = (generation, false);
    }
    write_atomic(&path_beside(lessons), &encode(facts));
}

/// Little-endian writer of the files.
struct W(Vec<u8>);

impl W {
    fn u8(&mut self, x: u8) {
        self.0.push(x);
    }
    fn u32(&mut self, x: u32) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    fn u64(&mut self, x: u64) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    fn u128(&mut self, x: u128) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    fn len(&mut self, n: usize) {
        self.u32(n as u32);
    }
    /// The body's blake3, appended.
    fn seal(mut self) -> Vec<u8> {
        let sum = blake3::hash(&self.0);
        self.0.extend_from_slice(sum.as_bytes());
        self.0
    }
}

/// Little-endian reader of the files: None past their end.
struct R<'a>(&'a [u8]);

impl<'a> R<'a> {
    /// The body of a sealed file of `magic` and these `rules`, after them; None for anything else.
    fn open(b: &'a [u8], magic: &[u8; 8]) -> Option<Self> {
        let (body, sum) = b.split_at_checked(b.len().checked_sub(32)?)?;
        if blake3::hash(body).as_bytes() != sum {
            return None;
        }
        let mut r = R(body);
        (r.take(magic.len())? == magic && r.hash()? == rules()).then_some(r)
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (a, b) = self.0.split_at_checked(n)?;
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn u128(&mut self) -> Option<u128> {
        Some(u128::from_le_bytes(self.take(16)?.try_into().ok()?))
    }
    fn i64(&mut self) -> Option<i64> {
        self.u64().map(|x| x as i64)
    }
    fn f64(&mut self) -> Option<f64> {
        self.u64().map(f64::from_bits)
    }
    fn hash(&mut self) -> Option<[u8; 32]> {
        self.take(32)?.try_into().ok()
    }
    /// A count, at most what the rest could hold at `each` bytes per item (a corrupt count never
    /// reserves gigabytes).
    fn len(&mut self, each: usize) -> Option<usize> {
        let n = self.u32()? as usize;
        (n.checked_mul(each)? <= self.0.len()).then_some(n)
    }
    /// An id below `n`.
    fn id(&mut self, n: usize) -> Option<Id> {
        self.u32().filter(|i| (*i as usize) < n)
    }
}

fn enc_part(w: &mut W, p: &Part) {
    let c = &p.check;
    w.u64(c.dev);
    w.u64(c.ino);
    w.u64(c.at);
    w.0.extend_from_slice(&c.head);
    w.0.extend_from_slice(&c.tail);
    w.len(p.sessions.len());
    for (s, x) in &p.sessions {
        w.u32(*s);
        w.u8(u8::from(x.tick.is_some()));
        w.u64(x.tick.unwrap_or(0) as u64);
        w.len(x.repos.len());
        x.repos.iter().for_each(|r| w.u32(*r));
        w.len(x.corrections.len());
        x.corrections.iter().for_each(|m| w.u64(*m as u64));
    }
    w.len(p.cells.len());
    for ((l, s), c) in &p.cells {
        w.u32(*l);
        w.u32(*s);
        w.u8(c.r as u8);
        w.u8(u8::from(c.used) | u8::from(c.tested) << 1);
        w.u8(c.trust);
        w.u64(c.trust_ms as u64);
    }
    w.len(p.use_repos.len());
    for (l, s, r) in &p.use_repos {
        [*l, *s, *r].into_iter().for_each(|x| w.u32(x));
    }
    w.len(p.tested.len());
    for (s, ls) in &p.tested {
        w.u32(*s);
        w.len(ls.len());
        ls.iter().for_each(|l| w.u32(*l));
    }
    w.len(p.given.len());
    for (s, l) in &p.given {
        w.u32(*s);
        w.u32(*l);
    }
}

/// One part, every id checked against the `n` strings.
fn dec_part(r: &mut R<'_>, n: usize) -> Option<Part> {
    let mut p = Part { check: Check { dev: r.u64()?, ino: r.u64()?, at: r.u64()?, head: r.hash()?, tail: r.hash()? }, ..Part::default() };
    let k = r.len(21)?;
    p.sessions.reserve(k);
    for _ in 0..k {
        let s = r.id(n)?;
        let tick = match (r.u8()?, r.i64()?) {
            (0, _) => None,
            (1, ms) => Some(ms),
            _ => return None,
        };
        let repos = (0..r.len(4)?).map(|_| r.id(n)).collect::<Option<_>>()?;
        let corrections = (0..r.len(8)?).map(|_| r.i64()).collect::<Option<_>>()?;
        p.sessions.insert(s, Sess { tick, repos, corrections });
    }
    let k = r.len(19)?;
    p.cells.reserve(k);
    for _ in 0..k {
        let key = (r.id(n)?, r.id(n)?);
        let (rv, flags, trust, trust_ms) = (r.u8()? as i8, r.u8()?, r.u8()?, r.i64()?);
        p.cells.insert(key, Cell { r: rv, used: flags & 1 != 0, tested: flags & 2 != 0, trust, trust_ms });
    }
    for _ in 0..r.len(12)? {
        p.use_repos.insert((r.id(n)?, r.id(n)?, r.id(n)?));
    }
    for _ in 0..r.len(8)? {
        let s = r.id(n)?;
        let ls = (0..r.len(4)?).map(|_| r.id(n)).collect::<Option<_>>()?;
        p.tested.insert((s, ls));
    }
    for _ in 0..r.len(8)? {
        p.given.insert((r.id(n)?, r.id(n)?));
    }
    Some(p)
}

/// The snapshot's bytes (module doc).
pub fn encode(f: &Facts) -> Vec<u8> {
    let mut w = W(Vec::with_capacity(1 << 16));
    w.0.extend_from_slice(MAGIC);
    w.0.extend_from_slice(&rules());
    w.u128(f.generation);
    w.len(f.strings.len());
    for s in &f.strings {
        w.len(s.len());
        w.0.extend_from_slice(s.as_bytes());
    }
    w.len(f.tags.len());
    for (a, b) in &f.tags {
        w.u32(*a);
        w.u32(*b);
    }
    enc_part(&mut w, &f.local);
    enc_part(&mut w, &f.team);
    for blame in &f.blame {
        w.len(blame.len());
        for (l, s, credit) in blame {
            w.u32(*l);
            w.u32(*s);
            w.u64(credit.to_bits());
        }
    }
    w.seal()
}

/// The facts of a whole, valid snapshot of these `rules`; None for anything else (the caller folds
/// afresh). Their regrets stay unread (`Facts::regrets`).
pub fn decode(b: &[u8]) -> Option<Facts> {
    let mut r = R::open(b, MAGIC)?;
    let mut f = Facts { generation: r.u128()?, ..Facts::default() };
    let n = r.len(4)?;
    f.strings.reserve(n);
    for _ in 0..n {
        let len = r.len(1)?;
        f.strings.push(std::str::from_utf8(r.take(len)?).ok()?.to_string());
    }
    let k = r.len(8)?;
    f.tags.reserve(k);
    for _ in 0..k {
        f.tags.insert(r.id(n)?, r.id(n)?);
    }
    f.local = dec_part(&mut r, n)?;
    f.team = dec_part(&mut r, n)?;
    for blame in &mut f.blame {
        *blame = (0..r.len(16)?).map(|_| Some((r.id(n)?, r.id(n)?, r.f64()?))).collect::<Option<_>>()?;
    }
    f.mine = f.local.sessions.keys().map(|s| f.tags.get(s).copied()).collect::<Option<_>>()?;
    r.0.is_empty().then_some(f)
}

/// The regrets file's bytes: its generation, then the three maps (module doc).
fn encode_regrets(x: &Regrets, generation: u128) -> Vec<u8> {
    let mut w = W(Vec::with_capacity(40 * (x.off.len() + x.on.len() + x.team.len()) + 128));
    w.0.extend_from_slice(REGRETS_MAGIC);
    w.0.extend_from_slice(&rules());
    w.u128(generation);
    for m in [&x.off, &x.on] {
        w.len(m.len());
        for ((l, c), (credit, off, s)) in m {
            w.u32(*l);
            w.u128(*c);
            w.u64(credit.to_bits());
            w.u64(*off);
            w.u32(*s);
        }
    }
    w.len(x.team.len());
    for ((l, c, s), (credit, off)) in &x.team {
        w.u32(*l);
        w.u128(*c);
        w.u32(*s);
        w.u64(credit.to_bits());
        w.u64(*off);
    }
    w.seal()
}

/// The regrets of a whole, valid regrets file of `generation`, every id below `n`; None for anything
/// else (the facts that name it are void).
fn decode_regrets(b: &[u8], generation: u128, n: usize) -> Option<Regrets> {
    let mut r = R::open(b, REGRETS_MAGIC)?;
    (r.u128()? == generation).then_some(())?;
    let mut x = Regrets::default();
    for m in [&mut x.off, &mut x.on] {
        let k = r.len(40)?;
        m.reserve(k);
        for _ in 0..k {
            let key = (r.id(n)?, r.u128()?);
            m.insert(key, (r.f64()?, r.u64()?, r.id(n)?));
        }
    }
    let k = r.len(40)?;
    x.team.reserve(k);
    for _ in 0..k {
        let key = (r.id(n)?, r.u128()?, r.id(n)?);
        x.team.insert(key, (r.f64()?, r.u64()?));
    }
    r.0.is_empty().then_some(x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conscience::Challenge;
    use crate::memory::Rng;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-facts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn pick<'a>(r: &mut Rng, xs: &[&'a str]) -> &'a str {
        xs[(r.next_u64() % xs.len() as u64) as usize]
    }

    /// A random store's two logs (local, team) and lessons: every kind, placebos, regrets with odd
    /// credits, this machine's own published copies, peers' lines (a `tested` one given or not), exact
    /// duplicates, unknown kinds and lines that are no record.
    fn logs(seed: u64) -> (String, String, Vec<Challenge>) {
        let mut r = Rng::new(seed);
        let kinds = [
            outcome::ASKED,
            outcome::CHANGED,
            outcome::IGNORED,
            outcome::CORRECTED,
            outcome::NOT_CORRECTED,
            outcome::MISSED,
            outcome::VETOED,
            outcome::REGRETTED,
            outcome::TESTED,
            outcome::UNCLEAR,
            outcome::REMAINS,
            outcome::NOT_ARISEN,
            outcome::GRADED,
            outcome::ENDED,
        ];
        let lessons = ["L0", "L1", "L2", "L3", "L4", "L5"];
        let repos = ["repoA", "RepoA", "repoB", ""];
        let causes = ["", "", "R1", "R2", "V3"];
        let mut ms = 0;
        let mut record = |r: &mut Rng, session: String| {
            ms += 1 + (r.next_u64() % 3) as i64;
            let kind = pick(r, &kinds);
            outcome::Outcome {
                kind: kind.into(),
                session,
                lessons: (0..r.next_u64() % 3 + u64::from(kind != outcome::ENDED)).map(|_| pick(r, &lessons).to_string()).collect(),
                // Equal times across sessions (the clock orders them by id), and the extremes a peer's
                // line may carry.
                ms: match r.next_u64() % 24 {
                    0 => i64::MAX,
                    1 => i64::MIN,
                    2..=7 => 7,
                    _ => ms,
                },
                repo: pick(r, &repos).into(),
                channel: if r.next_u64().is_multiple_of(6) { outcome::PLACEBO.into() } else { pick(r, &["", "early", "stop"]).into() },
                cause: if kind == outcome::REGRETTED || kind == outcome::MISSED { pick(r, &causes).into() } else { String::new() },
                credit: if kind == outcome::REGRETTED { (r.next_u64() % 16) as f64 / 10.0 - 0.2 } else { 0.0 },
                ..Default::default()
            }
        };
        let (mut local, mut team): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        for _ in 0..r.next_u64() % 40 {
            let line = match r.next_u64() % 12 {
                0 => "{\"kind\":\"asked\",\"session\":".to_string(),
                1 if !local.is_empty() => local[(r.next_u64() % local.len() as u64) as usize].clone(),
                _ => {
                    let session = format!("s{}", r.next_u64() % 5);
                    let o = record(&mut r, session);
                    if let Some(x) = outcome::share(&o, "this-node").filter(|_| r.next_u64().is_multiple_of(2)) {
                        team.push(serde_json::to_string(&x).unwrap());
                    }
                    serde_json::to_string(&o).unwrap()
                }
            };
            local.push(line);
        }
        for _ in 0..r.next_u64() % 40 {
            let line = match r.next_u64() % 12 {
                0 => "not a record".to_string(),
                1 if !team.is_empty() => team[(r.next_u64() % team.len() as u64) as usize].clone(),
                _ => {
                    let mut o = record(&mut r, String::new());
                    o.channel = String::new();
                    let peer = format!("{:032x}", r.next_u64() % 4);
                    let mut x = outcome::share(&o, "peer").unwrap_or_else(|| outcome::Shared { kind: o.kind.clone(), ms: o.ms, ..Default::default() });
                    (x.s, x.lessons) = (peer, o.lessons.clone());
                    serde_json::to_string(&x).unwrap()
                }
            };
            team.push(line);
        }
        let challenge = |k: usize| Challenge {
            id: if k < 6 { lessons[k].to_string() } else { format!("N{k}") },
            origin_ms: (r.next_u64() % 60) as i64,
            reach: if k.is_multiple_of(3) { crate::reach::REPO.into() } else { String::new() },
            repo: if k.is_multiple_of(3) { pick(&mut r, &repos).into() } else { String::new() },
            engineers: (0..1 + k % 2).map(|e| format!("e{e}")).collect(),
            ..Default::default()
        };
        let cs = (0..8).map(challenge).collect();
        let lines = |v: Vec<String>| v.into_iter().map(|l| l + "\n").collect::<String>();
        (lines(local), lines(team), cs)
    }

    /// outcome.rs `sessions` as sorted rows, whatever the maps' hasher.
    fn norm<A, B>(m: &HashMap<&str, HashMap<&str, (bool, i64), B>, A>) -> Vec<(String, String, bool, i64)> {
        let mut v: Vec<(String, String, bool, i64)> = m.iter().flat_map(|(l, ss)| ss.iter().map(|(s, x)| (l.to_string(), s.to_string(), x.0, x.1))).collect();
        v.sort();
        v
    }

    /// The facts' memory and trust equal a fresh `derive` and `sessions` of the logs beside `lessons`,
    /// with the team's outcomes on and off, for every session in play and a new one.
    fn same(lessons: &Path, cs: &[Challenge], facts: &Facts, what: &str) {
        for on in [true, false] {
            let outcomes = outcome::team_view(lessons, on);
            let view = facts.view(on);
            assert_eq!(norm(&outcome::sessions(&outcomes)), norm(&view.trust), "{what}: trust, on={on}");
            let mut sessions: Vec<Option<&str>> = outcomes.iter().map(|o| Some(o.session.as_str())).collect();
            sessions.sort();
            sessions.dedup();
            sessions.extend([None, Some("a-new-session")]);
            for session in sessions {
                let want = memory::derive(&outcomes, cs, session);
                let got = memory::derive_folded(facts.view(on).folded, cs, session);
                assert_eq!(want.len(), got.len());
                for (id, a) in &want {
                    let b = &got[id];
                    let close = |x: f64, y: f64| x == y || (x - y).abs() <= 1e-9 * x.abs().max(1.0);
                    assert!(
                        (a.changed, a.ignored, a.corrected, a.vetoed, a.tested, a.consolidated, a.dormant)
                            == (b.changed, b.ignored, b.corrected, b.vetoed, b.tested, b.consolidated, b.dormant)
                            && close(a.activation, b.activation)
                            && close(a.regretted, b.regretted),
                        "{what}: on={on} session={session:?} {id}: {a:?} != {b:?}"
                    );
                }
            }
        }
    }

    /// Folded whole, folded in random pieces cut mid-line (each load resuming the last's snapshot), and
    /// round-tripped through the snapshot: always a fresh `derive`.
    #[test]
    fn the_facts_equal_a_fresh_derive_however_the_logs_grew() {
        let d = dir("grow");
        let lessons = d.join("lessons.jsonl");
        let (pl, pt) = (outcome::path_beside(&lessons), d.join(outcome::TEAM_FILE));
        for seed in 0..200 {
            let (local, team, cs) = logs(seed);
            let _ = std::fs::remove_file(path_beside(&lessons));
            std::fs::write(&pl, "").unwrap();
            std::fs::write(&pt, "").unwrap();
            let mut r = Rng::new(seed ^ 0xfac7);
            let (mut l, mut t) = (0, 0);
            while l < local.len() || t < team.len() {
                l = (l + (r.next_u64() % 1200) as usize).min(local.len());
                t = (t + (r.next_u64() % 1200) as usize).min(team.len());
                use std::io::Write;
                let have = |p: &Path| std::fs::metadata(p).unwrap().len() as usize;
                std::fs::OpenOptions::new().append(true).open(&pl).unwrap().write_all(&local.as_bytes()[have(&pl)..l]).unwrap();
                std::fs::OpenOptions::new().append(true).open(&pt).unwrap().write_all(&team.as_bytes()[have(&pt)..t]).unwrap();
                let facts = load(&lessons);
                same(&lessons, &cs, &facts, &format!("seed {seed} at {l}/{t}"));
            }
            let saved = decode(&std::fs::read(path_beside(&lessons)).unwrap_or_default());
            if let Some(mut saved) = saved {
                assert_eq!(decode(&encode(&saved)).as_ref(), Some(&saved), "seed {seed}: round trip");
                saved.regrets_file = Some(regrets_beside(&lessons));
                let n = saved.strings.len();
                let regrets = saved.regrets().cloned().expect("the regrets of the snapshot's generation");
                assert_eq!(decode_regrets(&encode_regrets(&regrets, 9), 9, n).as_ref(), Some(&regrets), "seed {seed}: regrets round trip");
            }
            same(&lessons, &cs, &fresh(&lessons), &format!("seed {seed}: fresh"));
            same(&lessons, &cs, &load(&lessons), &format!("seed {seed}: warm"));
        }
        let _ = std::fs::remove_dir_all(d);
    }

    /// Every way the logs or the snapshot change under the facts is noticed: a rename (quarantine), an
    /// in-place rewrite shorter, longer and of the same length, a removed log, a corrupt, truncated or
    /// foreign snapshot. `rebuild` replaces a snapshot that missed an edit no checkpoint can see.
    #[test]
    fn a_rewritten_log_or_a_bad_snapshot_folds_afresh() {
        let d = dir("rewrite");
        let lessons = d.join("lessons.jsonl");
        let (pl, pt) = (outcome::path_beside(&lessons), d.join(outcome::TEAM_FILE));
        let snap = path_beside(&lessons);
        for seed in 0..60 {
            let (local, team, cs) = logs(seed);
            let _ = std::fs::remove_file(&snap);
            std::fs::write(&pl, &local).unwrap();
            std::fs::write(&pt, &team).unwrap();
            same(&lessons, &cs, &load(&lessons), "cold");
            let every_other = |text: &str, k: usize| text.lines().enumerate().filter(|(i, _)| i % 3 != k).map(|(_, l)| format!("{l}\n")).collect::<String>();
            // Quarantine: a copy without some lines renamed over the team's log.
            let tmp = d.join("q.tmp");
            std::fs::write(&tmp, every_other(&team, 1)).unwrap();
            std::fs::rename(&tmp, &pt).unwrap();
            same(&lessons, &cs, &load(&lessons), "renamed");
            // In place: shorter, then longer than the checkpoint, then the same length.
            std::fs::write(&pl, every_other(&local, 0)).unwrap();
            same(&lessons, &cs, &load(&lessons), "local shorter");
            std::fs::write(&pt, every_other(&team, 2) + &team).unwrap();
            same(&lessons, &cs, &load(&lessons), "team longer");
            let lines: Vec<&str> = local.lines().collect();
            if lines.len() > 2 {
                let mut body: String = lines.iter().enumerate().filter(|(i, _)| *i != 1).map(|(_, l)| format!("{l}\n")).collect();
                body.push_str(&" ".repeat(lines[1].len()));
                body.push('\n');
                std::fs::write(&pl, &body).unwrap();
                same(&lessons, &cs, &load(&lessons), "local same length");
            }
            // A bad snapshot.
            if let Ok(mut b) = std::fs::read(&snap) {
                let n = b.len() / 2;
                b[n] ^= 0x55;
                assert!(decode(&b).is_none());
                std::fs::write(&snap, &b).unwrap();
                same(&lessons, &cs, &load(&lessons), "corrupt");
                let b = std::fs::read(&snap).unwrap();
                std::fs::write(&snap, &b[..b.len() - 1]).unwrap();
                same(&lessons, &cs, &load(&lessons), "truncated");
            }
            std::fs::write(&snap, b"BORGFCT0 an older fold").unwrap();
            same(&lessons, &cs, &load(&lessons), "foreign");
            // Removed logs.
            std::fs::remove_file(&pt).unwrap();
            same(&lessons, &cs, &load(&lessons), "team removed");
            std::fs::remove_file(&pl).unwrap();
            same(&lessons, &cs, &load(&lessons), "local removed");
            std::fs::write(&pl, &local).unwrap();
            std::fs::write(&pt, &team).unwrap();
            same(&lessons, &cs, &load(&lessons), "back");
        }
        // An edit inside a log both windows miss: a warm load keeps the snapshot's facts (it read only the
        // tail), until `rebuild`. The edited line is the only change of lesson LX.
        let (local, team): (String, String) = (0..40).map(logs).map(|(l, t, _)| (l, t)).unzip();
        let cs = logs(0).2;
        let mid = local[..local.len() / 2].rfind('\n').unwrap() + 1;
        let sentinel =
            serde_json::to_string(&Outcome { kind: outcome::CHANGED.into(), session: "sx".into(), lessons: vec!["LX".into()], ms: 5, ..Default::default() })
                .unwrap();
        let local = format!("{}{sentinel}\n{}", &local[..mid], &local[mid..]);
        assert!(mid > 2 * WINDOW as usize && local.len() - mid > 2 * WINDOW as usize);
        let _ = std::fs::remove_file(&snap);
        std::fs::write(&pl, &local).unwrap();
        std::fs::write(&pt, &team).unwrap();
        let big = load(&lessons);
        same(&lessons, &cs, &big, "big");
        let uses_lx = |f: &Facts| f.view(false).folded.uses.contains_key("LX");
        assert!(uses_lx(&big));
        let mut edited = local.clone();
        edited.replace_range(mid..mid + sentinel.len(), &" ".repeat(sentinel.len()));
        std::fs::write(&pl, &edited).unwrap();
        assert!(load(&lessons) == big, "the warm load folds only what follows its checkpoint");
        assert!(!uses_lx(&fresh(&lessons)), "the edit changed a fact");
        rebuild(&lessons);
        assert!(!uses_lx(&load(&lessons)));
        same(&lessons, &cs, &load(&lessons), "rebuilt");
        let _ = std::fs::remove_dir_all(d);
    }

    /// A warm load parses only the lines appended since the snapshot, and reads the regrets file only
    /// when a regret line (or a team session that turns out to be this machine's) comes up.
    #[test]
    fn a_warm_load_parses_only_the_appended_lines() {
        let d = dir("warm");
        let lessons = d.join("lessons.jsonl");
        let (pl, pt) = (outcome::path_beside(&lessons), d.join(outcome::TEAM_FILE));
        let (local, team, cs) = (0..40).map(logs).find(|(l, t, _)| l.contains("\"regretted\"") && t.contains("\"regretted\"")).expect("a store with regrets");
        std::fs::write(&pl, &local).unwrap();
        std::fs::write(&pt, &team).unwrap();
        let count = || (PARSED.with(|c| c.replace(0)), REGRET_READS.with(|c| c.replace(0)));
        count();
        let cold = load(&lessons);
        assert_eq!(count(), (local.lines().count() + team.lines().count(), 0), "cold: every line, no regrets file");
        assert!(cold.generation != 0 && regrets_beside(&lessons).exists());
        assert!(load(&lessons) == cold);
        assert_eq!(count(), (0, 0), "warm: nothing parsed, no regrets file read");
        let line = |kind: &str, session: &str| {
            let o = Outcome {
                kind: kind.into(),
                session: session.into(),
                lessons: vec!["L1".into()],
                ms: 99,
                cause: "R9".into(),
                credit: 0.5,
                ..Default::default()
            };
            serde_json::to_string(&o).unwrap() + "\n"
        };
        use std::io::Write;
        let append = |text: &str| std::fs::OpenOptions::new().append(true).open(&pl).unwrap().write_all(text.as_bytes()).unwrap();
        append(&line(outcome::CHANGED, "s1"));
        same(&lessons, &cs, &load(&lessons), "a change appended");
        assert_eq!(count(), (1, 0), "a change: its line, no regrets file");
        append(&line(outcome::REGRETTED, "s1"));
        same(&lessons, &cs, &load(&lessons), "a regret appended");
        assert_eq!(count(), (1, 1), "a regret: its line and the regrets file");
        same(&lessons, &cs, &load(&lessons), "warm again");
        assert_eq!(count(), (0, 0));
        let _ = std::fs::remove_dir_all(d);
    }

    /// A team session folded before this machine's log names it (its own published copy came first):
    /// its regrets and results leave the view on, as `team_parts` drops them.
    #[test]
    fn a_team_session_that_turns_out_to_be_mine_leaves_the_view() {
        let d = dir("mine");
        let lessons = d.join("lessons.jsonl");
        let (pl, pt) = (outcome::path_beside(&lessons), d.join(outcome::TEAM_FILE));
        let cs = logs(0).2;
        let o = |kind: &str, session: &str, ms: i64| Outcome {
            kind: kind.into(),
            session: session.into(),
            lessons: vec!["L1".into()],
            ms,
            repo: "repoA".into(),
            cause: if kind == outcome::REGRETTED { "R1".into() } else { String::new() },
            credit: 0.75,
            ..Default::default()
        };
        let shared = [o(outcome::CHANGED, "me", 3), o(outcome::REGRETTED, "me", 4)];
        let team: String = shared.iter().map(|x| serde_json::to_string(&outcome::share(x, "this-node").unwrap()).unwrap() + "\n").collect();
        std::fs::write(&pt, &team).unwrap();
        std::fs::write(&pl, serde_json::to_string(&o(outcome::ASKED, "other", 1)).unwrap() + "\n").unwrap();
        let before = load(&lessons);
        same(&lessons, &cs, &before, "a peer's");
        assert!(!before.view(true).folded.regret.is_empty());
        let mine: String = shared.iter().map(|x| serde_json::to_string(x).unwrap() + "\n").collect();
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(&pl).unwrap().write_all(mine.as_bytes()).unwrap();
        let after = load(&lessons);
        same(&lessons, &cs, &after, "mine");
        assert!(after.view(true).folded.regret.iter().all(|(_, s, _)| *s == "me"), "only this machine's own regret");
        let _ = std::fs::remove_dir_all(d);
    }

    /// A line whose ms is i64::MAX ticks the clock like any other: no value means "no tick".
    #[test]
    fn a_tick_at_the_largest_ms_is_a_tick() {
        let d = dir("max");
        let lessons = d.join("lessons.jsonl");
        let o = |kind: &str, session: &str, lesson: &str, ms: i64| Outcome {
            kind: kind.into(),
            session: session.into(),
            lessons: vec![lesson.into()],
            ms,
            ..Default::default()
        };
        let log = [o(outcome::CHANGED, "s1", "L1", 10), o(outcome::ASKED, "s2", "L2", i64::MAX), o(outcome::CHANGED, "s3", "L1", 20)];
        std::fs::write(outcome::path_beside(&lessons), log.iter().map(|x| serde_json::to_string(x).unwrap() + "\n").collect::<String>()).unwrap();
        let cs: Vec<Challenge> = ["L1", "L2"].iter().map(|id| Challenge { id: id.to_string(), origin_ms: 1, ..Default::default() }).collect();
        same(&lessons, &cs, &load(&lessons), "i64::MAX");
        assert_eq!(load(&lessons).view(false).folded.clock.now(None), 3, "three sessions tick");
        let _ = std::fs::remove_dir_all(d);
    }

    /// A snapshot or regrets file of another version's rules, or a regrets file of another generation
    /// (a concurrent hook's), is folded afresh.
    #[test]
    fn another_version_or_generation_folds_afresh() {
        let d = dir("version");
        let lessons = d.join("lessons.jsonl");
        let (pl, pt) = (outcome::path_beside(&lessons), d.join(outcome::TEAM_FILE));
        let (local, team, cs) = (0..20).map(logs).find(|(l, _, _)| l.contains("\"regretted\"")).expect("a store with regrets");
        std::fs::write(&pl, &local).unwrap();
        std::fs::write(&pt, &team).unwrap();
        let cold = load(&lessons);
        // The same snapshot, sealed under other rules.
        let other = |b: &[u8]| {
            let mut body = b[..b.len() - 32].to_vec();
            body[8] ^= 1;
            W(body).seal()
        };
        let snap = std::fs::read(path_beside(&lessons)).unwrap();
        assert!(decode(&snap).is_some() && decode(&other(&snap)).is_none());
        let regrets = std::fs::read(regrets_beside(&lessons)).unwrap();
        let n = cold.strings.len();
        assert!(decode_regrets(&regrets, cold.generation, n).is_some() && decode_regrets(&other(&regrets), cold.generation, n).is_none());
        std::fs::write(path_beside(&lessons), other(&snap)).unwrap();
        PARSED.with(|c| c.set(0));
        same(&lessons, &cs, &load(&lessons), "other rules");
        assert_eq!(PARSED.with(|c| c.replace(0)), local.lines().count() + team.lines().count(), "folded afresh");
        // A regrets file of another generation, met by a regret line: void, folded afresh.
        let saved = decode(&std::fs::read(path_beside(&lessons)).unwrap()).unwrap();
        let mut theirs = saved.clone();
        theirs.regrets_file = Some(regrets_beside(&lessons));
        let r = theirs.regrets().cloned().unwrap();
        std::fs::write(regrets_beside(&lessons), encode_regrets(&r, saved.generation ^ 1)).unwrap();
        let regret = local.lines().find(|l| l.contains("\"regretted\"")).unwrap();
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(&pl).unwrap().write_all(format!("{regret}\n").as_bytes()).unwrap();
        same(&lessons, &cs, &load(&lessons), "other generation");
        assert_eq!(PARSED.with(|c| c.replace(0)), 1 + local.lines().count() + 1 + team.lines().count(), "its line, then all afresh");
        same(&lessons, &cs, &load(&lessons), "warm after");
        let _ = std::fs::remove_dir_all(d);
    }

    /// Nothing written beside a lessons file without an outcome log (an eval's challenge file).
    #[test]
    fn no_log_no_snapshot() {
        let d = dir("none");
        let lessons = d.join("lessons.jsonl");
        assert_eq!(load(&lessons), Facts::default());
        rebuild(&lessons);
        assert!(!path_beside(&lessons).exists());
        let _ = std::fs::remove_dir_all(d);
    }

    /// On a real store (an ignored test, it reads files outside the repo): BORG_FACTS_STORE, a copy of a
    /// store dir (it writes the snapshot there), and BORG_FACTS_SESSIONS, a comma-separated list of
    /// sessions. Equal cold, warm and round-tripped, with the team's outcomes on and off.
    #[test]
    #[ignore]
    fn the_facts_equal_a_fresh_derive_on_a_real_store() {
        let dir = PathBuf::from(std::env::var_os("BORG_FACTS_STORE").expect("set BORG_FACTS_STORE"));
        let lessons = dir.join("lessons.jsonl");
        let cs = crate::conscience::load_store_unweighted(&lessons, None);
        let sessions = std::env::var("BORG_FACTS_SESSIONS").unwrap_or_default();
        let t = std::time::Instant::now();
        let cold = load(&lessons);
        eprintln!("cold {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let warm = load(&lessons);
        eprintln!("warm {:?}, {} bytes", t.elapsed(), encode(&warm).len());
        assert_eq!(decode(&encode(&warm)).as_ref(), Some(&warm));
        for on in [true, false] {
            let outcomes = outcome::team_view(&lessons, on);
            for session in sessions.split(',').filter(|s| !s.is_empty()).map(Some).chain([None, Some("a-new-session")]) {
                for facts in [&cold, &warm] {
                    let want = memory::derive(&outcomes, &cs, session);
                    let got = memory::derive_folded(facts.view(on).folded, &cs, session);
                    assert_eq!(want.len(), got.len());
                    for (id, a) in &want {
                        let b = &got[id];
                        assert_eq!(
                            (a.changed, a.ignored, a.corrected, a.vetoed, a.tested, a.consolidated, a.dormant),
                            (b.changed, b.ignored, b.corrected, b.vetoed, b.tested, b.consolidated, b.dormant),
                            "{id}"
                        );
                        assert!((a.activation - b.activation).abs() < 1e-9 && (a.regretted - b.regretted).abs() < 1e-9, "{id}: {a:?} != {b:?}");
                    }
                }
                eprintln!("on={on} {session:?}: {} lessons equal", cs.len());
            }
        }
        same(&lessons, &cs, &warm, "real store");
    }
}
