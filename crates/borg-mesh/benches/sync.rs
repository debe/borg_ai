//! What a sync session costs: one whole session and its parts, the baseline the mesh's CPU and memory
//! work is measured against.
//!
//! `exchange/one_new_line` is a whole session, `Node::sync` against `Node::accept`, between two nodes in
//! this process over loopback QUIC with relays off, so nothing leaves the machine. Both stores hold the
//! same lines but one lesson the dialler's hook just wrote: each iteration appends a new one, so every
//! session moves exactly one line, as the session after a hook's write does. It runs the product's own
//! `Node::exchange`, so whatever a node caches between sessions counts here too.
//! `exchange/two_at_once` is two such sessions served at the same time, Bob's and Carol's, each moving the
//! line its own hook wrote: what one session holds of the listener's cache while the other stores (a copy
//! of what is held) counts there.
//!
//! The parts run first, on the listener's store with every line stamped: the one-shot readers that read a
//! file whole (`lines::read`, `merge` with nothing new, `settle`, `origin::load` and the compact `Index` it
//! is built from, a stamp with nothing new through a new cache), and what a session pays through a node's cache that read the store before
//! (`cache/*`: a log and the records unchanged, a stamp with nothing new, one appended lesson followed and
//! settled, the admitted lines of a log unchanged; `sync::offered` decided whole over the cache's
//! snapshot), and the message window (`msg::within_windows`).
//!
//! The data is a copy of the store `BORG_BENCH_STORE` names, else a generated one of a real team's shape
//! (shape/mod.rs). Its origin records are signed again with keys made here, one per member key the store
//! names, so the bench's membership admits every line as the real one does, with the same hashes, times
//! and sizes. Message lines still name their real node, which no key here holds, so they do not travel;
//! deciding that still runs.
//!
//! Criterion times it, short by default; `BORG_BENCH_ITERS=N` runs a fixed number of each instead, for
//! callgrind, `perf stat`, heaptrack and peak RSS (runner/mod.rs). A fixed run opens the sessions only when
//! it measures the exchange: under valgrind a session outlives QUIC's timeouts.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::{Path, PathBuf};

use borg::msg::LineRef;
use borg_mesh::cache::Cache;
use borg_mesh::identity::{self, Identity};
use borg_mesh::lines::{self, REASONING, SYNCED};
use borg_mesh::membership::Membership;
use borg_mesh::origin::{self, Origin};
use borg_mesh::sync::{self, Node, Place, Report};

#[path = "../../borg/benches/runner/mod.rs"]
mod runner;
#[path = "../../borg/benches/shape/mod.rs"]
mod shape;

use runner::Runner;

/// The collective both nodes are members of.
const COLLECTIVE: &str = "bench";

/// One machine: its collectives config, and the collective's store it routes to.
struct Machine {
    place: Place,
    home: PathBuf,
}

/// A machine under `root` whose repo routes to `COLLECTIVE`, shared with the team, relays off.
fn machine(root: &Path) -> Machine {
    let place = Place { config: root.join("collectives.json"), base: root.to_path_buf(), dir: root.join("repo").display().to_string(), collective: None };
    let home = borg::collective::store(root, COLLECTIVE);
    std::fs::create_dir_all(identity::dir(&home)).expect("bench store");
    let cfg = serde_json::json!({"default": "off", "collectives": [{"name": COLLECTIVE, "paths": [place.dir], "share": "team", "relay": "disabled"}]});
    std::fs::write(&place.config, cfg.to_string()).expect("bench config");
    Machine { place, home }
}

/// Alice's, Bob's and Carol's machines over the same lines, origins and membership: Alice stands for the
/// store's own node, Bob and Carol are other members. Also says where the lines came from.
fn crew(scratch: &Path) -> (Machine, Machine, Machine, String) {
    let source = scratch.join("source");
    let what = shape::fill(&source);
    let (a, b, c) = (machine(&scratch.join("alice")), machine(&scratch.join("bob")), machine(&scratch.join("carol")));
    let alice = Identity::load_or_create(&a.home).expect("alice's key");
    let bob = Identity::load_or_create(&b.home).expect("bob's key");
    let carol = Identity::load_or_create(&c.home).expect("carol's key");
    let text = std::fs::read_to_string(origin::path(&source)).unwrap_or_default();
    let records: Vec<Origin> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let own = std::fs::read_to_string(identity::public_path(&source)).unwrap_or_default().trim().to_string();
    let mut signers: BTreeMap<&str, Identity> = BTreeMap::new();
    for o in records.iter().filter(|o| o.key != own) {
        if !signers.contains_key(o.key.as_str()) {
            let key = Identity::load_or_create(&scratch.join("keys").join(signers.len().to_string())).expect("a member's key");
            signers.insert(&o.key, key);
        }
    }
    let mut m = Membership::found(COLLECTIVE, &alice);
    for key in signers.values().map(Identity::public).chain([bob.public(), carol.public()]) {
        m.invite(&alice, &key, "member").expect("invite");
    }
    let signed: String = records
        .iter()
        .filter_map(|o| {
            let h = borg_mesh::from_hex::<32>(&o.h).ok()?;
            let by = signers.get(o.key.as_str()).unwrap_or(&alice);
            serde_json::to_string(&Origin::new(by, &h, o.ms)).ok().map(|l| l + "\n")
        })
        .collect();
    for x in [&a, &b, &c] {
        m.save(&x.home).expect("membership");
        std::fs::write(origin::path(&x.home), &signed).expect("origins");
        for f in SYNCED.iter().chain(&[REASONING, "outcomes.jsonl", borg::outcome::PUBLISHED]) {
            let _ = std::fs::copy(source.join(f), x.home.join(f));
        }
    }
    (a, b, c, what)
}

/// The three nodes, bound on loopback, and the lessons Bob's and Carol's hooks wrote so far.
struct Session {
    rt: tokio::runtime::Runtime,
    alice: Node,
    bob: Node,
    carol: Node,
    ticket: String,
    bob_lessons: PathBuf,
    carol_lessons: PathBuf,
    written: Cell<usize>,
}

impl Session {
    fn open(a: &Machine, b: &Machine, c: &Machine) -> Session {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
        let local = "127.0.0.1:0".parse().expect("loopback");
        let bind = |m: &Machine, who: &str| rt.block_on(Node::bind(&m.place, local)).unwrap_or_else(|e| panic!("{who}'s node: {e}"));
        let (alice, bob, carol) = (bind(a, "alice"), bind(b, "bob"), bind(c, "carol"));
        let ticket = alice.ticket();
        let (bob_lessons, carol_lessons) = (b.home.join("lessons.jsonl"), c.home.join("lessons.jsonl"));
        Session { rt, alice, bob, carol, ticket, bob_lessons, carol_lessons, written: Cell::new(0) }
    }

    /// A lesson Bob's hook just wrote: a line no store held.
    fn write(&self) {
        self.write_to(&self.bob_lessons);
    }

    /// A lesson the hook of the machine whose lessons are at `lessons` just wrote.
    fn write_to(&self, lessons: &Path) {
        let n = self.written.replace(self.written.get() + 1);
        let line = serde_json::json!({"id": format!("bench{n}"), "origin_ms": 1, "question": "a lesson written between two sessions?",
            "situations": ["a hook appended it"], "consequence": "the next session moves it", "support": 1, "episodes": [], "history": [],
            "version": 1});
        let mut f = std::fs::OpenOptions::new().append(true).open(lessons).expect("the lessons");
        std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes()).expect("append a lesson");
    }

    /// One session: Bob dials, Alice serves. Returns what Alice and Bob moved.
    fn exchange(&self) -> (Report, Report) {
        self.rt.block_on(async {
            let (served, synced) = tokio::join!(self.alice.accept(), self.bob.sync(&self.ticket));
            (served.expect("alice serves"), synced.expect("bob syncs"))
        })
    }

    /// Two sessions at once: Bob and Carol dial, Alice serves both. Returns what Alice moved in each.
    fn two_at_once(&self) -> (Report, Report) {
        self.rt.block_on(async {
            let (one, two, bob, carol) = tokio::join!(self.alice.accept(), self.alice.accept(), self.bob.sync(&self.ticket), self.carol.sync(&self.ticket));
            bob.expect("bob syncs");
            carol.expect("carol syncs");
            (one.expect("alice serves"), two.expect("alice serves"))
        })
    }

    fn close(self) {
        self.rt.block_on(async {
            self.alice.close().await;
            self.bob.close().await;
            self.carol.close().await;
        });
    }
}

/// What a session calls, one at a time, on `home` once its node has stamped the lines no origin covers
/// yet, as its first session does.
fn parts(r: &mut Runner, home: &Path) {
    let me = Identity::load_or_create(home).expect("the node's key");
    let m = Membership::load(home).ok().flatten().expect("the membership");
    let now = borg::learn::now_ms();
    let logs = lines::logs(home);
    let cache = Cache::default();
    cache.stamp(home, &me, &logs, now).expect("stamp");
    let held: Vec<&str> = SYNCED.iter().copied().filter(|f| home.join(f).exists()).collect();

    for f in held.iter().chain(&[REASONING]) {
        let path = home.join(f);
        r.bench("lines", &format!("read/{f}"), || lines::read(black_box(&path)));
    }
    for f in held.iter().filter(|f| **f != borg::msg::FILE) {
        let path = home.join(f);
        r.bench("lines", &format!("merge_nothing/{f}"), || lines::merge(black_box(&path), &[]).expect("merge"));
    }
    for f in ["lessons.jsonl", "stats.jsonl"] {
        let path = home.join(f);
        r.bench("lines", &format!("settle/{f}"), || lines::settle(black_box(&path)).expect("settle"));
    }

    r.bench("origin", "load", || origin::load(black_box(home)));
    r.bench("origin", "index_read", || origin::Index::read(black_box(home)).len());
    r.bench("origin", "stamp_nothing_new", || Cache::default().stamp(black_box(home), &me, &logs, now).expect("stamp").new);

    for f in &held {
        let path = home.join(f);
        r.bench("cache", &format!("log/{f}"), || cache.log(black_box(&path)).len());
    }
    r.bench("cache", "origins", || cache.origins(black_box(home)).len());
    r.bench("cache", "stamp_nothing_new", || cache.stamp(black_box(home), &me, &logs, now).expect("stamp").new);
    let lessons = home.join("lessons.jsonl");
    let written = Cell::new(0usize);
    r.bench("cache", "append_one/lessons.jsonl", || {
        let n = written.replace(written.get() + 1);
        let line = serde_json::json!({"id": format!("part{n}"), "origin_ms": 1, "question": "appended?", "version": 1});
        let mut f = std::fs::OpenOptions::new().append(true).open(&lessons).expect("the lessons");
        std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes()).expect("append a lesson");
        cache.settle(black_box(&lessons)).expect("settle");
        cache.log(&lessons).len()
    });

    for f in held.iter().filter(|f| ![borg::msg::FILE, lines::ABSORBED].contains(f)) {
        let path = home.join(f);
        r.bench("cache", &format!("admitted/{f}"), || cache.admitted(black_box(&path), &cache.log(&path), &cache.origins(home), &m).len());
    }
    for f in &held {
        let path = home.join(f);
        r.bench("sync", &format!("offered/{f}"), || sync::offered(black_box(&path), cache.log(&path), &cache.origins(home), &m, now, false).len());
    }

    let text = std::fs::read_to_string(home.join(borg::msg::FILE)).unwrap_or_default();
    let messages: Vec<LineRef> = text.lines().filter_map(LineRef::parse).collect();
    r.bench("msg", "within_windows", || borg::msg::within_windows(black_box(&messages)).len());
}

fn main() {
    let scratch = shape::Scratch::new("mesh");
    let (a, b, c, what) = crew(&scratch.0);
    eprintln!("bench store: {what}");
    let mut r = Runner::from_env();
    parts(&mut r, &a.home);
    if r.wants("exchange", "one_new_line") {
        let s = Session::open(&a, &b, &c);
        // The first session checks the logs against the membership: once, as on a node that has run
        // before. Then the bench's premise: a session moves one new line.
        s.exchange();
        s.write();
        assert_eq!(s.exchange().0.received, 1, "a session moves the one line the dialler wrote");
        // Bob's hook writes the line inside the measured work: one append, next to a session's reads.
        r.bench("exchange", "one_new_line", || {
            s.write();
            let (served, _) = s.exchange();
            assert_eq!(served.received, 1, "each session moves the one new line");
        });
        s.close();
    }
    if r.wants("exchange", "two_at_once") {
        let s = Session::open(&a, &b, &c);
        // Each dialler's first session, one at a time, as in `one_new_line`; then both at once, each
        // moving the line its hook wrote.
        s.two_at_once();
        r.bench("exchange", "two_at_once", || {
            s.write_to(&s.bob_lessons);
            s.write_to(&s.carol_lessons);
            let (one, two) = s.two_at_once();
            assert_eq!(one.received + two.received, 2, "both new lines moved to Alice, each once");
        });
        s.close();
    }
    r.finish();
}
