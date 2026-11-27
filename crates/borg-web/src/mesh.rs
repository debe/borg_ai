//! The Cube view (`/api/mesh`): the collective as this node knows it.
//!
//! Its machines (this node, the members it heard from, the peers it dials), the links it has itself (a
//! node never learns the links between two others: those run through the collective), the drones on
//! each machine, and what moved between them in the messages' window: every message and every time a session was shown one, every
//! lesson written and each new version of one, every absorb and release, every hand-off event, and this
//! node's mesh sessions that moved lines or failed.
//!
//! Every node key and session id is sent once, in `strs`, and named by its index everywhere else: an
//! event names four of them, a key is 64 characters. Lines are read borrowed (msg.rs `LineRef`, narrow
//! projections of the other logs), and the body is serialised from borrowed rows, never through
//! `serde_json::Value`.
//!
//! **Cost.** The page refetches the view whenever its SSE entry changes, and that entry watches
//! `presence/`, which every hook of every drone touches. So the events are built once per change of the
//! files they come from (`Sources`) and kept per store (`Built`, its strings in one arena), and a
//! request that finds them current builds only the machines, links and drones. N open pages cost one
//! build per change. The lessons and the absorb records are folded once per change of their own file
//! (`Lessons`, `Absorbs`), and the origin records, the store's largest file, are followed: read once,
//! then only what was appended, and only the records stamped within the window are held (`Origins`).
//!
//! **Deltas by arrival.** An event's `ms` is when its writer wrote it, and a line that comes in through
//! the mesh is older than its arrival. So `since` does not select by `ms`: each event is stamped with
//! when this process first held it (`Row::seen`), and `since` (the `now` of the page's last response)
//! gives the events held after it.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::io::{BufRead, Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use borg::collective::Share;
use borg::follow::Mark;
use borg::mesh::{self as log, OriginRef};
use borg::msg::{self, Address, LineRef};
use borg::presence::State;
use borg::{absorb, names, protocol, top};
use serde::de::IgnoredAny;
use serde::ser::{SerializeSeq as _, Serializer};
use serde::{Deserialize, Serialize};

/// Display: the longest text an event carries: a packet's caption, not the message (the Messages tab has it).
const EXCERPT_CHARS: usize = 160;

/// The strings the view names, each once: `get` gives its index in `list`.
#[derive(Default)]
struct Strs<'a> {
    idx: HashMap<&'a str, u32>,
    list: Vec<&'a str>,
}

impl<'a> Strs<'a> {
    fn get(&mut self, s: &'a str) -> u32 {
        if let Some(&i) = self.idx.get(s) {
            return i;
        }
        let i = u32::try_from(self.list.len()).unwrap_or(u32::MAX);
        self.idx.insert(s, i);
        self.list.push(s);
        i
    }
}

#[derive(Serialize)]
struct Node<'a> {
    key: u32,
    /// Its machine, as names.rs `Book::host` names it.
    host: String,
    engineer: Option<String>,
    me: bool,
    borg: Option<&'a str>,
    protocol: Option<u32>,
    /// False: it speaks a protocol this node does not, nothing syncs with it.
    syncs: bool,
}

/// A link this node has: `b` is a peer it dials (`dial`), or one a session completed with (`ms`).
#[derive(Serialize)]
struct Link {
    a: u32,
    b: u32,
    dial: bool,
    ms: Option<i64>,
    ok: bool,
}

#[derive(Serialize)]
struct Drone<'a> {
    session: u32,
    node: u32,
    /// top.rs `Drone::designation`.
    name: &'a str,
    number: usize,
    state: State,
    repo: &'a str,
    harness: &'a str,
    engineer: String,
    paused: bool,
    started: i64,
    ms: i64,
}

/// Where a string of an `Arena` is: `at + len` fits in a `u32` (`Arena::span`).
#[derive(Clone, Copy, Default)]
struct Span {
    at: u32,
    len: u32,
}

/// Strings one after another in one buffer, each named by its `Span`.
#[derive(Default)]
struct Arena(String);

impl Arena {
    fn with_capacity(n: usize) -> Self {
        Self(String::with_capacity(n))
    }

    fn put(&mut self, s: &str) -> Span {
        let at = self.0.len();
        self.0.push_str(s);
        self.span(at)
    }

    fn put_fmt(&mut self, args: std::fmt::Arguments<'_>) -> Span {
        let at = self.0.len();
        let _ = self.0.write_fmt(args);
        self.span(at)
    }

    /// The span of what was put from `at` on. Past 4 GiB of strings (no store's window comes near) it
    /// is taken back and the string is empty, so a span never wraps to name other text.
    fn span(&mut self, at: usize) -> Span {
        if let (Ok(a), Ok(e)) = (u32::try_from(at), u32::try_from(self.0.len())) {
            Span { at: a, len: e - a }
        } else {
            self.0.truncate(at);
            Span::default()
        }
    }

    fn at(&self, s: Span) -> &str {
        let at = s.at as usize;
        self.0.get(at..at + s.len as usize).unwrap_or_default()
    }

    fn shrink(&mut self) {
        self.0.shrink_to_fit();
    }
}

/// What kind of thing moved: the event contract's `kind` (`Ev`).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Message(msg::Kind),
    Seen,
    Lesson,
    LessonUpdate,
    Absorb,
    Release,
    Handoff,
    Sync,
    SyncFailed,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Message(k) => k.as_str(),
            Self::Seen => "seen",
            Self::Lesson => "lesson",
            Self::LessonUpdate => "lesson_update",
            Self::Absorb => "absorb",
            Self::Release => "release",
            Self::Handoff => "handoff",
            Self::Sync => "sync",
            Self::SyncFailed => "sync_failed",
        }
    }
}

/// Whom a message is to: the event contract's `to`.
#[derive(Clone, Copy)]
enum To {
    All,
    Local,
    Engineer,
    Session,
}

impl To {
    const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Local => "local",
            Self::Engineer => "engineer",
            Self::Session => "session",
        }
    }
}

/// Something that moved, as `Built` holds it: indices into `Built::strs`, texts in `Built::arena`. The
/// kinds and fields are the module's event contract (`Ev`). Small, since a busy week holds tens of
/// thousands (one `seen` per session and message): one-byte kinds, and `n` only read for a `sync`.
#[derive(Clone, Copy)]
struct Row {
    id: Span,
    ms: i64,
    /// When this process first held it (`Built::stamp` of that build): what `since` selects by.
    seen: i64,
    kind: Kind,
    from_n: u32,
    from_s: Option<u32>,
    to: Option<To>,
    to_n: Option<u32>,
    to_s: Option<u32>,
    re: Option<Span>,
    text: Span,
    lesson: Option<Span>,
    why: Option<borg::handoff::Kind>,
    n: u64,
}

/// The events of a store's window, built once per change of their files (`Sources`), sorted by `ms`.
#[derive(Default)]
struct Built {
    arena: Arena,
    /// The node keys and session ids the rows name.
    strs: Vec<Span>,
    rows: Vec<Row>,
}

impl Built {
    fn str(&self, i: u32) -> &str {
        self.strs.get(i as usize).map_or("", |&s| self.arena.at(s))
    }
}

/// `Built` as it is built: rows naming strings of the logs read, interned as they come, each stamped
/// `stamp` until `finish` finds it held before.
struct Builder<'a> {
    strs: Strs<'a>,
    arena: Arena,
    rows: Vec<Row>,
    me: &'a str,
    stamp: i64,
}

impl<'a> Builder<'a> {
    /// A node as top.rs names it: empty for this one.
    fn node(&mut self, n: &'a str) -> u32 {
        self.strs.get(if n.is_empty() { self.me } else { n })
    }

    /// A row of `kind` by `from_n` at `ms`, everything else empty, for the caller to fill.
    fn row(&mut self, id: Span, ms: i64, kind: Kind, from_n: u32) -> &mut Row {
        self.rows.push(Row {
            id,
            ms,
            seen: self.stamp,
            kind,
            from_n,
            from_s: None,
            to: None,
            to_n: None,
            to_s: None,
            re: None,
            text: Span::default(),
            lesson: None,
            why: None,
            n: 0,
        });
        let last = self.rows.len() - 1;
        &mut self.rows[last]
    }

    /// The rows in time order, each that `prev` held (by kind, id and `re`, as the page tells two
    /// apart) keeping when it was first held; the buffers cut to what they hold, since they are kept.
    fn finish(mut self, prev: Option<&Built>) -> Built {
        // The log is in append order, which a synced line from a slower clock breaks.
        self.rows.sort_by_key(|r| r.ms);
        let list = std::mem::take(&mut self.strs.list);
        let strs = list.iter().map(|s| self.arena.put(s)).collect();
        if let Some(p) = prev {
            let held: HashMap<(Kind, &str, Option<&str>), i64> =
                p.rows.iter().map(|r| ((r.kind, p.arena.at(r.id), r.re.map(|s| p.arena.at(s))), r.seen)).collect();
            for r in &mut self.rows {
                if let Some(&seen) = held.get(&(r.kind, self.arena.at(r.id), r.re.map(|s| self.arena.at(s)))) {
                    r.seen = seen;
                }
            }
        }
        self.arena.shrink();
        self.rows.shrink_to_fit();
        Built { arena: self.arena, strs, rows: self.rows }
    }
}

/// The first line of `text`, at most `EXCERPT_CHARS` characters: a slice of it.
fn excerpt(text: &str) -> &str {
    let line = text.lines().next().unwrap_or_default();
    line.char_indices().nth(EXCERPT_CHARS).map_or(line, |(i, _)| &line[..i])
}

/// A line of `lessons.jsonl`, as far as the view reads it: no lesson is materialised.
#[derive(Deserialize)]
struct LessonRef<'a> {
    #[serde(borrow)]
    id: Cow<'a, str>,
    version: u32,
    origin_ms: i64,
    #[serde(borrow)]
    question: Cow<'a, str>,
    #[serde(borrow, default)]
    reach: Cow<'a, str>,
}

/// One line of `lessons.jsonl`: its hash (the origin records name it by), and what its event shows.
struct LessonLine {
    hash: [u8; 32],
    id: Span,
    version: u32,
    origin_ms: i64,
    question: Span,
}

/// The lines of a store's `lessons.jsonl`, folded under one stat (`mark`), strings in `arena`.
#[derive(Default)]
struct Lessons {
    mark: Mark,
    arena: Arena,
    lines: Vec<LessonLine>,
}

impl Lessons {
    /// The lessons of `path`, read whole under `mark`. A tombstone (reach.rs `PERSONAL`) withdraws a
    /// lesson, it writes none, and is left out.
    fn read(path: &Path, mark: Mark) -> Self {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let mut out = Self { mark, ..Self::default() };
        for line in text.lines() {
            let Ok(l) = serde_json::from_str::<LessonRef>(line) else { continue };
            if l.reach == borg::reach::PERSONAL {
                continue;
            }
            let id = out.arena.put(&l.id);
            let question = out.arena.put(excerpt(&l.question));
            out.lines.push(LessonLine { hash: log::line_hash(line), id, version: l.version, origin_ms: l.origin_ms, question });
        }
        out.arena.shrink();
        out.lines.shrink_to_fit();
        out
    }
}

/// A line of `absorbed.jsonl`, as far as the view reads it (absorb.rs `Record`).
#[derive(Deserialize)]
struct AbsorbRef<'a> {
    #[serde(borrow)]
    id: Cow<'a, str>,
    #[serde(borrow)]
    kind: Cow<'a, str>,
    ms: i64,
    #[serde(borrow, default)]
    note: Option<Cow<'a, str>>,
}

/// An absorb or a release: its line's hash (who stamped it), and what its event shows.
struct AbsorbLine {
    hash: [u8; 32],
    release: bool,
    ms: i64,
    lesson: Span,
    note: Span,
}

/// The records of a store's `absorbed.jsonl` from a horizon on, folded under one stat (`mark`): the
/// file is never trimmed, and is read again only when it changes, not at every message line.
#[derive(Default)]
struct Absorbs {
    mark: Mark,
    arena: Arena,
    lines: Vec<AbsorbLine>,
}

impl Absorbs {
    /// The absorbs and releases of `path` from `horizon` on, read whole under `mark`: the horizon only
    /// moves on, so the ones before it are never asked for again.
    fn read(path: &Path, mark: Mark, horizon: i64) -> Self {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let mut out = Self { mark, ..Self::default() };
        for line in text.lines() {
            let Ok(a) = serde_json::from_str::<AbsorbRef>(line) else { continue };
            let release = match a.kind.as_ref() {
                absorb::ABSORB => false,
                absorb::RELEASE => true,
                _ => continue,
            };
            if a.ms < horizon || a.id.trim().is_empty() {
                continue;
            }
            let lesson = out.arena.put(&a.id);
            let note = out.arena.put(excerpt(a.note.as_deref().unwrap_or_default()));
            out.lines.push(AbsorbLine { hash: log::line_hash(line), release, ms: a.ms, lesson, note });
        }
        out.arena.shrink();
        out.lines.shrink_to_fit();
        out
    }
}

/// The origin records (borg mesh.rs `OriginRef`) stamped since `horizon`, by line hash, followed: a
/// change that appends to the file reads only what was appended. A line of the window was stamped no
/// earlier than it was written, so no record older than that is ever looked up; one older than the
/// horizon a fold moves to is dropped.
#[derive(Default)]
struct Origins {
    /// The file read (device, inode), and how far: up to its last complete line.
    file: (u64, u64),
    read: u64,
    /// The keys the records name, each once.
    keys: Vec<String>,
    /// The first record of each line hash: its key's index in `keys`, and when it was stamped.
    by_hash: HashMap<[u8; 32], (u32, i64)>,
}

impl Origins {
    /// Reads on in `path`: from where it stopped when it is the same file grown, else from the start.
    fn fold(&mut self, path: &Path, horizon: i64) {
        let Ok(mut file) = std::fs::File::open(path) else {
            *self = Self::default();
            return;
        };
        let Ok(meta) = file.metadata() else { return };
        let id = (meta.dev(), meta.ino());
        if id != self.file || meta.len() < self.read {
            *self = Self { file: id, ..Self::default() };
        }
        self.by_hash.retain(|_, (_, ms)| *ms >= horizon);
        if meta.len() == self.read || file.seek(SeekFrom::Start(self.read)).is_err() {
            return;
        }
        // Streamed through one buffer: the file can be the store's largest, and only the window is kept.
        let mut reader = std::io::BufReader::with_capacity(64 << 10, file.take(meta.len() - self.read));
        let mut line = Vec::new();
        loop {
            line.clear();
            let len = reader.read_until(b'\n', &mut line).unwrap_or(0);
            // An unterminated last line may be half written: the next fold reads it again.
            if len == 0 || line.last() != Some(&b'\n') {
                break;
            }
            self.read += len as u64;
            let Some(record) = std::str::from_utf8(&line).ok().and_then(|t| OriginRef::parse(t.trim_end())) else { continue };
            if record.ms < horizon {
                continue;
            }
            let Some(hash) = record.hash().filter(|h| !self.by_hash.contains_key(h)) else { continue };
            let i = self.keys.iter().position(|k| *k == record.key).unwrap_or_else(|| {
                self.keys.push(record.key.into_owned());
                self.keys.len() - 1
            });
            let Ok(key) = u32::try_from(i) else { continue };
            self.by_hash.insert(hash, (key, record.ms));
        }
    }

    /// Who stamped the line `h`, and when.
    fn of(&self, h: &[u8; 32]) -> Option<(&str, i64)> {
        self.by_hash.get(h).and_then(|&(k, ms)| Some((self.keys.get(k as usize)?.as_str(), ms)))
    }
}

/// The files the events come from, as one stat each saw them: the events are current while these are.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct Sources {
    messages: Mark,
    lessons: Mark,
    origins: Mark,
    absorbed: Mark,
    sessions: Mark,
}

impl Sources {
    fn at(home: &Path) -> Self {
        Self {
            messages: Mark::at(&msg::path(home)),
            lessons: Mark::at(&home.join("lessons.jsonl")),
            origins: Mark::at(&log::origins_path(home)),
            absorbed: Mark::at(&absorb::path(home)),
            sessions: Mark::at(&log::sessions_path(home)),
        }
    }
}

/// What this process holds of one store for the view.
#[derive(Default)]
struct Slot {
    origins: Origins,
    lessons: Lessons,
    absorbs: Absorbs,
    built: Option<(Sources, Arc<Built>)>,
    /// The latest `now` a request was answered at, or a build stamped: the next build stamps later, so
    /// what it adds is after every `since` a page was given.
    high: i64,
    /// How many times the events were built: a test tells a cached answer by it.
    builds: usize,
}

/// Every store this process showed the view of, each behind a lock of its own.
static SLOTS: Mutex<Vec<(PathBuf, Arc<Mutex<Slot>>)>> = Mutex::new(Vec::new());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The events of the store `home` at `now`, from `now - TTL_MS` on: the ones held while their files are
/// unchanged, else built again. The store's slot stays locked while they are built, so requests at once
/// build them once between them.
fn built(home: &Path, now: i64) -> Arc<Built> {
    let slot = {
        let mut all = lock(&SLOTS);
        if let Some((_, s)) = all.iter().find(|(p, _)| p == home) {
            Arc::clone(s)
        } else {
            let s = Arc::default();
            all.push((home.to_path_buf(), Arc::clone(&s)));
            s
        }
    };
    let mut slot = lock(&slot);
    // Stat first: a file written while the events are built changes them again at the next request.
    let sources = Sources::at(home);
    if let Some((held, b)) = &slot.built
        && *held == sources
    {
        let b = Arc::clone(b);
        slot.high = slot.high.max(now);
        return b;
    }
    let horizon = now - msg::TTL_MS;
    slot.origins.fold(&log::origins_path(home), horizon - msg::SKEW_MS);
    if slot.lessons.mark != sources.lessons {
        slot.lessons = Lessons::read(&home.join("lessons.jsonl"), sources.lessons);
    }
    if slot.absorbs.mark != sources.absorbed {
        slot.absorbs = Absorbs::read(&absorb::path(home), sources.absorbed, horizon);
    }
    let stamp = now.max(slot.high.saturating_add(1));
    slot.high = stamp;
    let prev = slot.built.take().map(|(_, b)| b);
    let b = Arc::new(build(home, &slot, horizon, stamp, prev.as_deref()));
    slot.built = Some((sources, Arc::clone(&b)));
    slot.builds += 1;
    b
}

/// How many times the events of `home` were built in this process (`built`).
#[cfg(test)]
fn builds(home: &Path) -> usize {
    let all = lock(&SLOTS);
    all.iter().find(|(p, _)| p == home).map_or(0, |(_, s)| lock(s).builds)
}

/// The events of `home` from `horizon` on, from its logs and what `slot` folded of the others, new
/// ones stamped `stamp` and the ones `prev` held as it stamped them.
fn build(home: &Path, slot: &Slot, horizon: i64, stamp: i64, prev: Option<&Built>) -> Built {
    let me = msg::node(home);
    let messages = std::fs::read_to_string(msg::path(home)).unwrap_or_default();
    let sessions = std::fs::read_to_string(log::sessions_path(home)).unwrap_or_default();
    let lines: Vec<LineRef<'_>> = messages.lines().filter_map(LineRef::parse).filter(|l| l.ms() >= horizon).collect();
    let sessions: Vec<log::Session<'_>> = log::sessions(&sessions).filter(|s| s.ms >= horizon).collect();
    // An estimate the arena seldom outgrows; `finish` cuts it to what it holds.
    let arena = Arena::with_capacity(messages.len() / 4 + 4096);
    let mut b = Builder { strs: Strs::default(), arena, rows: Vec::new(), me: &me, stamp };
    b.strs.get(&me);
    message_rows(&mut b, &lines);
    lesson_rows(&mut b, &slot.origins, &slot.lessons, horizon);
    absorb_rows(&mut b, &slot.origins, &slot.absorbs, horizon);
    session_rows(&mut b, &sessions);
    b.finish(prev)
}

/// The messages, who was shown them, and the hand-off events of the window's message lines.
fn message_rows<'a>(b: &mut Builder<'a>, lines: &'a [LineRef<'_>]) {
    use borg::handoff::Kind::Offer;
    // The node of each hand-off's offer: an outcome's packet goes back to it.
    let offers: HashMap<&str, &str> = lines
        .iter()
        .filter_map(|l| match l {
            LineRef::Handoff(h) if h.what() == Offer => Some((h.handoff(), h.node.as_ref())),
            _ => None,
        })
        .collect();
    for l in lines {
        match l {
            LineRef::Message(m) => {
                let (to, to_n, to_s) = match Address::parse(&m.to) {
                    Some(Address::All) => (To::All, None, None),
                    Some(Address::Local { node }) => (To::Local, Some(b.strs.get(node)), None),
                    Some(Address::Engineer { .. }) => (To::Engineer, None, None),
                    // Without a node, the drones say where the session runs (`view`).
                    Some(Address::Session { id, node }) => (To::Session, node.map(|n| b.strs.get(n)), Some(b.strs.get(id))),
                    None => continue,
                };
                let from_n = b.node(&m.node);
                let from_s = (!msg::is_engineer(&m.by)).then(|| b.strs.get(&m.by));
                let id = b.arena.put(&m.id);
                let re = m.re.as_deref().map(|r| b.arena.put(r));
                let text = b.arena.put(excerpt(&m.text));
                let r = b.row(id, m.ms, Kind::Message(m.kind), from_n);
                (r.from_s, r.to, r.to_n, r.to_s, r.re, r.text) = (from_s, Some(to), to_n, to_s, re, text);
            }
            LineRef::Seen(x) => {
                let from_n = b.node(&x.node);
                let from_s = (!msg::is_engineer(&x.by)).then(|| b.strs.get(&x.by));
                let id = b.arena.put(&x.by);
                for seen in &x.ids {
                    let re = b.arena.put(seen);
                    let r = b.row(id, x.ms, Kind::Seen, from_n);
                    (r.from_s, r.re) = (from_s, Some(re));
                }
            }
            LineRef::Handoff(h) => {
                let from_n = b.node(&h.node);
                let from_s = Some(b.strs.get(h.session()));
                // An offer goes to the machine it asks, an outcome to the machine that offered.
                let to = match h.what() {
                    Offer => h.to().filter(|t| msg::is_node_key(t)),
                    _ => offers.get(h.handoff()).copied().filter(|n| *n != h.node),
                };
                let to_n = to.map(|n| b.node(n));
                let id = b.arena.put_fmt(format_args!("{}:{}", h.handoff(), h.what().as_str()));
                let text = b.arena.put(excerpt(h.note()));
                let r = b.row(id, h.ms, Kind::Handoff, from_n);
                (r.from_s, r.to_n, r.why, r.text) = (from_s, to_n, Some(h.what()), text);
            }
        }
    }
}

/// A lesson line's event: when its origin record says it was stamped and by whom; a line with no record
/// is this node's own, not stamped yet (or in a store without a mesh), written when its first version
/// was. A later version without a record has no time to show and is left out.
fn lesson_rows<'a>(b: &mut Builder<'a>, origins: &'a Origins, lessons: &Lessons, horizon: i64) {
    for l in &lessons.lines {
        let (from_n, ms) = match origins.of(&l.hash) {
            Some((key, ms)) => (b.strs.get(key), ms),
            None if l.version == 1 => (0, l.origin_ms),
            None => continue,
        };
        if ms < horizon {
            continue;
        }
        let id_text = lessons.arena.at(l.id);
        let id = b.arena.put_fmt(format_args!("{id_text}@{}", l.version));
        let lesson = Some(b.arena.put(id_text));
        let text = b.arena.put(lessons.arena.at(l.question));
        let r = b.row(id, ms, if l.version > 1 { Kind::LessonUpdate } else { Kind::Lesson }, from_n);
        (r.lesson, r.text) = (lesson, text);
    }
}

/// The absorbs and releases from `horizon` on, by the node that stamped each, else this one.
fn absorb_rows<'a>(b: &mut Builder<'a>, origins: &'a Origins, absorbs: &Absorbs, horizon: i64) {
    for a in absorbs.lines.iter().filter(|a| a.ms >= horizon) {
        let from_n = origins.of(&a.hash).map_or(0, |(key, _)| b.strs.get(key));
        let lesson_text = absorbs.arena.at(a.lesson);
        let id = b.arena.put_fmt(format_args!("{lesson_text}@{}", a.ms));
        let lesson = Some(b.arena.put(lesson_text));
        let text = b.arena.put(absorbs.arena.at(a.note));
        let r = b.row(id, a.ms, if a.release { Kind::Release } else { Kind::Absorb }, from_n);
        (r.lesson, r.text) = (lesson, text);
    }
}

/// This node's mesh sessions of the window (borg mesh.rs `Session`): a sync that moved lines, or a
/// failure after a completed one.
fn session_rows<'a>(b: &mut Builder<'a>, sessions: &'a [log::Session<'_>]) {
    for s in sessions {
        let to_n = Some(b.strs.get(&s.peer));
        let id = b.arena.put_fmt(format_args!("sync:{}@{}", s.peer, s.ms));
        let (kind, n, text) = s.error.as_ref().map_or_else(|| (Kind::Sync, s.moved(), Span::default()), |e| (Kind::SyncFailed, 0, b.arena.put(excerpt(e))));
        let r = b.row(id, s.ms, kind, 0);
        (r.to_n, r.n, r.text) = (to_n, n, text);
    }
}

/// Something that moved, as the body gives it. `kind` is one of: a message's (`tell`, `ask`, `pause`,
/// `resume`, `veto`, `to` saying `all`, `local`, `engineer` or `session`, with `to_n` the node and `to_s`
/// the session where the address names them); `seen` (the session `from_s` on `from_n` was shown the
/// message `re`; `id` is who was shown it, so `(id, re)` tells two apart); `lesson` and `lesson_update`
/// (`id` is `<lesson>@<version>`, `lesson` the lesson, `text` its question); `absorb` and `release`
/// (`<lesson>@<ms>`, `text` the note); `handoff` (`<hand-off>:<what>`, `why` what happened: `offer`,
/// `taken`, `back`, `declined`, `aborted`, `done`; `to_n` the machine an offer asks, else the one that
/// offered); `sync` and `sync_failed` (`sync:<peer>@<ms>`, from this node to `to_n`, `n` the lines moved,
/// `text` the error).
#[derive(Serialize)]
struct Ev<'a> {
    id: &'a str,
    ms: i64,
    kind: &'static str,
    from_n: u32,
    from_s: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to_n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to_s: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    re: Option<&'a str>,
    #[serde(skip_serializing_if = "str::is_empty")]
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    lesson: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    why: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    n: Option<u64>,
}

/// The rows of a request's window held after `since` (`Row::seen`), serialised one by one with their
/// strings mapped to the body's table (`map`, by `Built::strs` index) and a session address's node
/// filled from the drones.
struct Events<'a> {
    built: &'a Built,
    rows: &'a [Row],
    since: i64,
    count: usize,
    map: &'a [u32],
    by_session: &'a HashMap<&'a str, u32>,
}

impl Serialize for Events<'_> {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let (b, map) = (self.built, self.map);
        let g = |i: u32| map.get(i as usize).copied().unwrap_or(u32::MAX);
        let at = |s: Span| b.arena.at(s);
        let mut seq = ser.serialize_seq(Some(self.count))?;
        for r in self.rows.iter().filter(|r| r.seen > self.since) {
            let to_n = r.to_n.map(g).or_else(|| r.to_s.and_then(|s| self.by_session.get(b.str(s)).copied()));
            seq.serialize_element(&Ev {
                id: at(r.id),
                ms: r.ms,
                kind: r.kind.as_str(),
                from_n: g(r.from_n),
                from_s: r.from_s.map(g),
                to: r.to.map(To::as_str),
                to_n,
                to_s: r.to_s.map(g),
                re: r.re.map(at),
                text: at(r.text),
                lesson: r.lesson.map(at),
                why: r.why.map(borg::handoff::Kind::as_str),
                n: (r.kind == Kind::Sync).then_some(r.n),
            })?;
        }
        seq.end()
    }
}

#[derive(Serialize)]
struct Body<'a> {
    collective: &'a str,
    now: i64,
    /// Where the window starts: the messages' lifetime (msg.rs `TTL_MS`) back from `now`.
    from: i64,
    me: u32,
    strs: Vec<&'a str>,
    nodes: Vec<Node<'a>>,
    links: Vec<Link>,
    drones: Vec<Drone<'a>>,
    events: Events<'a>,
}

/// The view's JSON for the store `home` of the collective `name` at `now`: the window's events (never
/// older than the messages live), or with `since` (the `now` of the page's last response) the ones this
/// node got after it, wherever their `ms` is; a few it sent then may come again, and the page drops the
/// ones it holds. The machines and links are the whole window's either way.
#[must_use]
pub fn view(name: &str, home: &Path, share: Share, since: Option<i64>, now: i64) -> Vec<u8> {
    let from = now - msg::TTL_MS;
    let since = since.unwrap_or(i64::MIN);
    let built = built(home, now);
    let rows = &built.rows[built.rows.partition_point(|r| r.ms < from)..];
    let count = rows.iter().filter(|r| r.seen > since).count();
    let me_key = msg::node(home);
    let (ds, book) = top::named(home, share == Share::Team, now);
    let versions = protocol::versions(home);
    let mismatches = protocol::mismatches(home);
    let synced = borg::presence::synced(home);
    let peers_buf = std::fs::read_to_string(home.join("mesh").join("peers.json")).unwrap_or_default();
    // Only the keys are read: the values (addresses) are skipped, not parsed.
    #[expect(clippy::zero_sized_map_values, reason = "a set of the map's keys, as serde reads a map")]
    let peers: HashMap<&str, IgnoredAny> = serde_json::from_str(&peers_buf).unwrap_or_default();
    let here = names::hostname();

    let mut s = Strs::default();
    let me = s.get(&me_key);
    // Machines: this one, then every member it heard of or an event names, in key order (stable for the
    // layout).
    let mut keys: BTreeSet<&str> = BTreeSet::new();
    keys.extend(versions.keys().map(String::as_str));
    keys.extend(mismatches.keys().map(String::as_str));
    keys.extend(synced.keys().map(String::as_str));
    keys.extend(peers.keys().copied());
    // A local drone names no node (top.rs): it is this one.
    keys.extend(ds.iter().filter(|d| !d.node.is_empty()).map(|d| d.node.as_str()));
    keys.extend(rows.iter().flat_map(|r| std::iter::once(r.from_n).chain(r.to_n)).map(|i| built.str(i)));
    keys.remove(me_key.as_str());

    let mut nodes = Vec::with_capacity(keys.len() + 1);
    // This machine's engineer: its drones'.
    let mine = ds.iter().find(|d| d.node.is_empty() && !d.p.engineer.is_empty()).map(|d| book.display(&d.p.engineer));
    nodes.push(Node {
        key: me,
        host: if here.is_empty() { "here".into() } else { here },
        engineer: mine,
        me: true,
        borg: Some(borg::update::VERSION),
        protocol: Some(protocol::current()),
        syncs: true,
    });
    let mut links = Vec::new();
    for &k in &keys {
        let key = s.get(k);
        let v = versions.get(k);
        let gap = mismatches.get(k);
        nodes.push(Node {
            key,
            host: book.host(k),
            engineer: v.and_then(|v| v.engineer.as_deref()).filter(|e| !e.is_empty()).map(|e| book.display(e)),
            me: false,
            borg: v.and_then(|v| v.borg.as_deref()),
            protocol: gap.map_or_else(|| v.map(|v| v.protocol), |m| protocol::number(&m.theirs)),
            syncs: gap.is_none(),
        });
        let dial = peers.contains_key(k);
        let ms = synced.get(k).copied();
        if dial || ms.is_some() || gap.is_some() {
            links.push(Link { a: me, b: key, dial, ms, ok: gap.is_none() });
        }
    }

    let mut by_session: HashMap<&str, u32> = HashMap::with_capacity(ds.len());
    let drones: Vec<Drone> = ds
        .iter()
        .map(|d| {
            let node = if d.node.is_empty() { me } else { s.get(&d.node) };
            let session = s.get(&d.p.session);
            by_session.insert(&d.p.session, node);
            Drone {
                session,
                node,
                name: &d.designation,
                number: d.number,
                state: d.state,
                repo: &d.p.repo,
                harness: &d.p.harness,
                engineer: book.display(&d.p.engineer),
                paused: d.paused,
                started: d.p.started,
                ms: d.p.ms,
            }
        })
        .collect();

    // Every string the events sent name, in the body's table.
    let mut map = vec![u32::MAX; built.strs.len()];
    for r in rows.iter().filter(|r| r.seen > since) {
        for i in [Some(r.from_n), r.from_s, r.to_n, r.to_s].into_iter().flatten() {
            if let Some(slot) = map.get_mut(i as usize)
                && *slot == u32::MAX
            {
                *slot = s.get(built.str(i));
            }
        }
    }

    let events = Events { built: &built, rows, since, count, map: &map, by_session: &by_session };
    let body = Body { collective: name, now, from, me, strs: s.list, nodes, links, drones, events };
    let mut out = Vec::with_capacity(count * 160 + 4096);
    // Serialising borrowed rows of strings and numbers cannot fail.
    let _ = serde_json::to_writer(&mut out, &body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_excerpt_is_the_first_line_cut_at_a_char_boundary() {
        assert_eq!(excerpt("one\ntwo"), "one");
        let long = "é".repeat(EXCERPT_CHARS + 5);
        assert_eq!(excerpt(&long).chars().count(), EXCERPT_CHARS);
        assert_eq!(excerpt(""), "");
    }

    /// The events are built once per change of their files: a request that finds them current, or a
    /// change of the drones alone, builds nothing again, and still shows the drones as they are now.
    #[test]
    fn the_events_are_built_once_per_change_of_their_files() {
        let home = std::env::temp_dir().join(format!("borg-web-mesh-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let now = borg::learn::now_ms();
        let from = msg::Sender { by: msg::ENGINEER.into(), name: "Ann".into(), harness: "terminal".into(), node: "a".repeat(64) };
        let tell = |text: &str, ms: i64| serde_json::to_string(&msg::Message::new(msg::TELL, &from, msg::ALL, None, ms, text)).unwrap();
        std::fs::write(msg::path(&home), tell("one", now - 1000) + "\n").unwrap();
        let events = |body: &[u8]| serde_json::from_slice::<serde_json::Value>(body).unwrap()["events"].as_array().map_or(0, Vec::len);
        assert_eq!(events(&view("c", &home, Share::Local, None, now)), 1);
        assert_eq!(events(&view("c", &home, Share::Local, None, now)), 1);
        assert_eq!(builds(&home), 1, "unchanged files: the events held");
        let ev = serde_json::json!({"session_id": "s1", "cwd": "/work/r"});
        borg::presence::touch(&home, "c", "claude", "prompt", &ev, now).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&view("c", &home, Share::Local, None, now)).unwrap();
        assert_eq!((builds(&home), body["drones"].as_array().map(Vec::len)), (1, Some(1)), "a drone alone: {body}");
        std::fs::write(msg::path(&home), tell("one", now - 1000) + "\n" + &tell("two", now - 500) + "\n").unwrap();
        assert_eq!(events(&view("c", &home, Share::Local, None, now)), 2);
        assert_eq!(builds(&home), 2);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A held row stays small: tens of thousands are kept per store for the process's life.
    #[test]
    fn a_row_is_small() {
        assert!(std::mem::size_of::<Row>() <= 96, "{}", std::mem::size_of::<Row>());
    }

    #[test]
    fn a_string_is_interned_once() {
        let mut s = Strs::default();
        assert_eq!((s.get("a"), s.get("b"), s.get("a")), (0, 1, 0));
        assert_eq!(s.list, ["a", "b"]);
    }
}
