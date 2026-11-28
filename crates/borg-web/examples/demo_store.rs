//! A made-up team collective, to try `borg web` without a store of your own and to film the Cube for the
//! README: six machines, their engineers' drones, and a week of messages, lessons, absorbs, hand-offs
//! and mesh syncs, busiest in the last eight hours (what the Cube replays when it opens). Before the
//! week, two months of older lessons and sessions, so the core holds about two hundred lessons: each
//! scoped to one of five systems, in one of borg's concern areas (trust.rs `AREAS`), and asked in
//! sessions whose results (changed, ignored, corrected, vetoed) are written as borg writes them
//! (`outcomes.jsonl`, `stats/events.jsonl`). Every lesson has a habit that decides those results, so
//! the core shows every colour, and lessons nobody used for long enough go dormant. With
//! `--live SECONDS` it then keeps writing a message about every second, so the Cube also plays live.
//! Every name in it is invented.
//!
//! ```sh
//! cargo run -p borg-web --example demo_store -- /tmp/borg-demo --live 60 &
//! BORG_HOME=/tmp/borg-demo/home BORG_CONFIG=/tmp/borg-demo/collectives.json \
//!     borg-web --port 7990 --dir /tmp/borg-demo/repos/shop
//! ```
//!
//! This machine shows by the system's hostname. To name it too, run the page in a UTS namespace of its
//! own: `unshare --user --map-root-user --uts sh -c 'hostname unimatrix-01; exec borg-web …'`.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use borg::learn::Lesson;
use borg::msg::{self, Message, Seen, Sender};
use borg::outcome::{self, Outcome};
use borg::presence::{self, Presence, Remote};
use borg::protocol::{self, PeerVersion};
use borg::stats::{self, Event};
use serde_json::json;

const COLLECTIVE: &str = "zero-one";
const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

/// The machines, first this one: host, engineer's name, email.
const MACHINES: [(&str, &str, &str); 6] = [
    ("unimatrix-01", "Seven", "seven@borg.example"),
    ("unimatrix-02", "Hugh", "hugh@borg.example"),
    ("vinculum", "Icheb", "icheb@borg.example"),
    ("transwarp-hub", "One", "one@borg.example"),
    ("regen-alcove", "Locutus", "locutus@borg.example"),
    ("tactical-node", "Annika", "annika@borg.example"),
];
const REPOS: [&str; 6] = ["shop", "api", "infra", "docs", "billing", "mobile"];
const HARNESSES: [&str; 4] = ["claude", "codex", "omp", "opencode"];
const TELLS: [&str; 12] = [
    "Heads-up: /orders paginates now, shop needs the cursor",
    "Rebased on main, CI is green again",
    "The flaky checkout test was a shared fixture; fixed in api",
    "I'm changing the schema of invoices, hold your migrations",
    "Done with the retry wrapper, it's idempotent now",
    "Moved the feature flag to config, no redeploy needed",
    "Can you review the cache change before I merge?",
    "Release notes for 2.4 are in docs/changes.md",
    "The staging deploy is blocked on the cert renewal",
    "Found the leak: a listener never removed on unmount",
    "Benchmarks: p95 down from 210 to 140 ms",
    "Pausing the rollout until billing confirms the totals",
];
const ASKS: [&str; 6] = [
    "Who owns the payment webhooks?",
    "Is the old auth endpoint still used anywhere?",
    "Does infra pin the Postgres version?",
    "Which branch has the new price table?",
    "Can I drop the legacy CSV export?",
    "Is anyone touching the router right now?",
];
/// The systems the lessons are scoped to, with their share of the lessons in percent (so the core's
/// systems come out of different sizes).
const SYSTEMS: [(&str, u64); 5] = [("billing", 30), ("search", 24), ("catalog", 20), ("infra", 16), ("accounts", 10)];
/// The lessons' questions, each with its concern area (one of borg's trust.rs `AREAS`).
const QUESTIONS: [(&str, &str); 24] = [
    ("concurrency", "Can two workers pick up the same job at once?"),
    ("concurrency", "Is the counter written under the same lock that reads it?"),
    ("spec-conformance", "Does the response use the status codes the API spec names?"),
    ("spec-conformance", "Are dates sent in the format the spec names?"),
    ("reuse-existing", "Is there already a helper for this in the shared crate?"),
    ("reuse-existing", "Does the existing retry wrapper cover this call?"),
    ("architecture-boundary", "Does the handler reach into the database instead of the service?"),
    ("architecture-boundary", "Should this rule live in the domain layer, not the handler?"),
    ("verification", "Did the full suite run, not only the changed test?"),
    ("verification", "Was the generated client rebuilt after the schema changed?"),
    ("minimality", "Does the change touch more files than the fix needs?"),
    ("minimality", "Is the new config option needed, or is the default enough?"),
    ("protocol-correctness", "Is the retry idempotent if the first call did go through?"),
    ("protocol-correctness", "Is the timeout shorter than the caller's?"),
    ("data-source", "Does the cache key include everything the result depends on?"),
    ("data-source", "Is the price read from the price table, not from the cart?"),
    ("security", "Would this log line print a secret or a customer's data?"),
    ("security", "Does the new endpoint check who may call it?"),
    ("dependency-version", "Does the lock file pin the version the code was tested with?"),
    ("dependency-version", "Does the upgrade change a default the code relies on?"),
    ("scope", "Does the feature flag default to off in production?"),
    ("scope", "Is this change part of the task, or a separate one?"),
    ("other", "Is the money computed in integer cents, not floats?"),
    ("other", "Does this migration lock a large table while it runs?"),
];

/// What sessions do with a lesson, which decides its colour in the core (ui volume.rs `Item::tone`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Habit {
    /// Mostly acted on: green.
    Followed,
    /// Mostly not acted on: dim.
    Ignored,
    /// Corrected or vetoed more often than acted on: red.
    Contested,
    /// Never asked: cyan.
    Untried,
    /// Absorbed into an outside store soon after it was learned, never asked: a ring.
    Held,
}

/// A fixed hash of lesson `n` and a `salt` (splitmix64), so a lesson's traits are the same in every
/// line written of it.
fn mix(n: usize, salt: u64) -> u64 {
    let mut z = (n as u64).wrapping_add(salt.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn habit(n: usize) -> Habit {
    match mix(n, 1) % 100 {
        0..=37 => Habit::Followed,
        38..=59 => Habit::Ignored,
        60..=75 => Habit::Contested,
        76..=89 => Habit::Untried,
        _ => Habit::Held,
    }
}

/// How likely a session that draws lesson `n` asks it, percent: some lessons match often, some rarely.
fn popularity(n: usize) -> u64 {
    25 + mix(n, 2) % 75
}

fn system(n: usize) -> &'static str {
    let mut roll = mix(n, 3) % 100;
    for (name, share) in SYSTEMS {
        if roll < share {
            return name;
        }
        roll -= share;
    }
    SYSTEMS[0].0
}

fn question(n: usize) -> (&'static str, &'static str) {
    QUESTIONS[usize::try_from(mix(n, 4) % QUESTIONS.len() as u64).unwrap_or(0)]
}

/// A small deterministic generator, so every run writes the same week.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap_or(0)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

struct Drone {
    machine: usize,
    session: String,
    repo: &'static str,
    harness: &'static str,
}

struct Demo {
    home: PathBuf,
    keys: Vec<String>,
    drones: Vec<Drone>,
    rng: Rng,
    /// Each lesson's first `origin_ms`, by number less one: a new version keeps it, as a merge does.
    born: Vec<i64>,
}

fn key(host: &str) -> String {
    blake3::hash(host.as_bytes()).to_hex().to_string()
}

fn append(path: &Path, line: &str) {
    std::fs::create_dir_all(path.parent().expect("a store file has a directory")).expect("the store directory");
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).expect("a store file");
    writeln!(f, "{line}").expect("a write to the store");
}

impl Demo {
    fn sender(&self, d: usize) -> Sender {
        let d = &self.drones[d];
        let (_, name, _) = MACHINES[d.machine];
        Sender { by: d.session.clone(), name: format!("{name} - {} - {}", d.repo, d.harness), harness: d.harness.into(), node: self.keys[d.machine].clone() }
    }

    /// Two drones on different machines.
    fn pair(&mut self) -> (usize, usize) {
        let a = self.rng.below(self.drones.len());
        loop {
            let b = self.rng.below(self.drones.len());
            if self.drones[b].machine != self.drones[a].machine {
                return (a, b);
            }
        }
    }

    /// One message line, and when its recipient was shown it.
    fn message(&mut self, ms: i64, out: &mut Vec<(i64, String)>) {
        let (a, b) = self.pair();
        let from = self.sender(a);
        let m = if self.rng.chance(8) {
            Message::new(msg::TELL, &from, msg::ALL, None, ms, TELLS[self.rng.below(TELLS.len())])
        } else {
            let to = format!("{}@{}", self.drones[b].session, self.keys[self.drones[b].machine]);
            let ask = self.rng.chance(25);
            let text = if ask { ASKS[self.rng.below(ASKS.len())] } else { TELLS[self.rng.below(TELLS.len())] };
            Message::new(if ask { msg::ASK } else { msg::TELL }, &from, &to, None, ms, text)
        };
        let shown = ms + i64::try_from(self.rng.below(90)).unwrap_or(0) * 1000 + 5000;
        let d = &self.drones[b];
        let seen = Seen::new(&d.session, &self.keys[d.machine], shown, &[&m.id]);
        out.push((ms, serde_json::to_string(&m).expect("a message line")));
        out.push((shown, serde_json::to_string(&seen).expect("a seen line")));
    }

    /// A lesson written on `machine` at `ms` (or a new version of one), with the origin record that
    /// names its machine, and its absorb when its habit is to be held elsewhere.
    fn lesson(&mut self, machine: usize, ms: i64) {
        let update = !self.born.is_empty() && self.rng.chance(20);
        let (n, version) = if update { (self.rng.below(self.born.len()) + 1, 2) } else { (self.born.len() + 1, 1) };
        if !update {
            self.born.push(ms);
        }
        let (area, text) = question(n);
        let l = Lesson {
            id: format!("L{n}"),
            origin_ms: self.born[n - 1],
            repo: REPOS[self.rng.below(REPOS.len())].into(),
            question: text.into(),
            situations: vec!["a change of this kind".into()],
            consequence: "a failure the engineer corrected once".into(),
            support: version,
            episodes: (1..=version).map(|v| format!("E{n}.{v}")).collect(),
            version,
            engineers: vec![MACHINES[machine].2.into()],
            area: area.into(),
            scope: vec![system(n).into()],
            ..Lesson::default()
        };
        let line = serde_json::to_string(&l).expect("a lesson line");
        let h = borg::mesh::line_hash(&line).iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        });
        append(&self.home.join("lessons.jsonl"), &line);
        append(&borg::mesh::origins_path(&self.home), &json!({"h": h, "key": self.keys[machine], "ms": ms, "sig": "00"}).to_string());
        if !update && habit(n) == Habit::Held {
            let absorbed =
                json!({"id": format!("L{n}"), "kind": "absorb", "ms": ms + 20 * MINUTE, "by": MACHINES[machine].2, "note": "skills/review", "version": 1});
            append(&borg::absorb::path(&self.home), &absorbed.to_string());
        }
    }

    /// Up to three lessons a session at `ms` is asked: drawn at random, each kept by its popularity;
    /// never one nobody asks (untried, or held by an outside store).
    fn draw(&mut self) -> Vec<usize> {
        let want = 1 + self.rng.below(3);
        let mut picked = Vec::with_capacity(want);
        for _ in 0..40 {
            let n = self.rng.below(self.born.len()) + 1;
            let asked = !matches!(habit(n), Habit::Untried | Habit::Held);
            if asked && !picked.contains(&n) && self.rng.chance(popularity(n)) {
                picked.push(n);
                if picked.len() == want {
                    break;
                }
            }
        }
        picked
    }

    /// What the session did with an asked lesson, by its habit: an outcome kind (outcome.rs) and the
    /// usage statistic it counts as (stats.rs).
    fn result(&mut self, n: usize) -> (&'static str, &'static str) {
        let roll = self.rng.below(100);
        let changed = (outcome::CHANGED, stats::CHANGED);
        let ignored = (outcome::IGNORED, stats::IGNORED);
        let unjudged = (outcome::NOT_CORRECTED, stats::NOT_CORRECTED);
        match (habit(n), roll) {
            (Habit::Followed, 0..=69) | (Habit::Ignored, 65..=84) | (Habit::Contested, 55..=84) => changed,
            (Habit::Followed, 70..=84) | (Habit::Ignored, 0..=64) | (Habit::Contested, 85..) => ignored,
            (Habit::Contested, 0..=39) => (outcome::CORRECTED, stats::CORRECTED),
            (Habit::Contested, 40..=54) => (outcome::VETOED, stats::VETO),
            _ => unjudged,
        }
    }

    /// A drone's session at `ms` in a random repo: a turn-end block that asks a few lessons, and a few
    /// minutes later each one's result, as the hook and `borg learn` record them.
    fn session(&mut self, ms: i64) {
        if self.born.is_empty() {
            return;
        }
        let picked = self.draw();
        if picked.is_empty() {
            return;
        }
        let session = format!("s{:012x}", self.rng.next() & 0xffff_ffff_ffff);
        let repo = REPOS[self.rng.below(REPOS.len())];
        let engineer = MACHINES[self.rng.below(MACHINES.len())].2;
        let ids: Vec<String> = picked.iter().map(|n| format!("L{n}")).collect();
        let outcomes = outcome::path_beside(&self.home.join("lessons.jsonl"));
        let events = stats::events_path(&self.home);
        let record = |kind: &str, lessons: Vec<String>, at: i64, channel: &str| Event {
            ms: at,
            kind: kind.into(),
            session: session.clone(),
            engineer: engineer.into(),
            repo: repo.into(),
            n: lessons.len() as u64,
            lessons,
            channel: channel.into(),
            ..Event::default()
        };
        let asked = Outcome {
            kind: outcome::ASKED.into(),
            session: session.clone(),
            lessons: ids.clone(),
            ms,
            repo: repo.into(),
            channel: "stop".into(),
            ..Outcome::default()
        };
        append(&outcomes, &serde_json::to_string(&asked).expect("an asked line"));
        append(&events, &serde_json::to_string(&record(stats::BLOCK, ids.clone(), ms, "stop")).expect("a block event"));
        for (n, id) in picked.into_iter().zip(ids) {
            let (kind, stat) = self.result(n);
            let at = ms + i64::try_from(3 + self.rng.below(20)).unwrap_or(3) * MINUTE;
            // The judge's verdicts carry its channel and version (an ignore without one is censored).
            let judged = kind == outcome::CHANGED || kind == outcome::IGNORED;
            let channel = if judged { "stop" } else { "" };
            let o = Outcome {
                kind: kind.into(),
                session: session.clone(),
                lessons: vec![id.clone()],
                ms: at,
                repo: repo.into(),
                channel: channel.into(),
                judge: if judged { borg::predict::JUDGE_VERSION } else { 0 },
                ..Outcome::default()
            };
            append(&outcomes, &serde_json::to_string(&o).expect("an outcome line"));
            append(&events, &serde_json::to_string(&record(stat, vec![id], at, channel)).expect("a result event"));
        }
    }

    /// A hand-off offered by drone `a` to drone `b`'s machine, and taken there.
    fn handoff(&mut self, ms: i64, out: &mut Vec<(i64, String)>) {
        let (a, b) = self.pair();
        let (na, nb) = (self.keys[self.drones[a].machine].clone(), self.keys[self.drones[b].machine].clone());
        let (sa, sb) = (self.drones[a].session.clone(), self.drones[b].session.clone());
        let nonce = format!("n{ms}");
        let mut h = blake3::Hasher::new();
        for part in [na.as_str(), sa.as_str(), nonce.as_str()] {
            h.update(part.as_bytes());
            h.update(b"\0");
        }
        let id = format!("h{}", &h.finalize().to_hex()[..12]);
        let repo = self.drones[a].repo;
        let offer = json!({"v": 1, "id": id, "kind": "offer", "node": na, "session": sa, "ms": ms, "to": nb, "nonce": nonce,
            "repos": [{"name": repo, "remote": format!("/srv/{repo}"), "branch": "main", "head": "1".repeat(40), "state": "2".repeat(40), "tree": "3".repeat(40)}],
            "brief": "4".repeat(40), "note": "take over the release branch"});
        let taken =
            json!({"v": 1, "id": id, "kind": "taken", "node": nb, "session": sb, "ms": ms + 4 * MINUTE, "claim": "5".repeat(40), "state": "2".repeat(40)});
        out.push((ms, json!({"v": 1, "kind": "handoff", "node": na, "ms": ms, "event": offer}).to_string()));
        out.push((ms + 4 * MINUTE, json!({"v": 1, "kind": "handoff", "node": nb, "ms": ms + 4 * MINUTE, "event": taken}).to_string()));
    }

    /// This node's session with a member at `ms`: lines moved, or (rarely) a failure.
    fn sync(&mut self, ms: i64, now: i64) {
        let peer = self.keys[1 + self.rng.below(self.keys.len() - 1)].clone();
        let failed = self.rng.chance(4);
        let moved = u64::try_from(1 + self.rng.below(40)).unwrap_or(1);
        let s = borg::mesh::Session {
            ms,
            peer: peer.into(),
            dial: self.rng.chance(50),
            sent: if failed { 0 } else { moved / 2 },
            received: if failed { 0 } else { moved - moved / 2 },
            error: failed.then(|| "no answer within 30s".into()),
        };
        borg::mesh::note(&self.home, &s, now).expect("the session log");
    }

    /// One event of any kind at `ms`.
    fn event(&mut self, ms: i64, now: i64, out: &mut Vec<(i64, String)>) {
        match self.rng.below(100) {
            0..=49 => self.message(ms, out),
            50..=64 => {
                let machine = self.rng.below(MACHINES.len());
                self.lesson(machine, ms);
            }
            65..=79 => self.session(ms),
            80..=83 => self.handoff(ms, out),
            _ => self.sync(ms, now),
        }
    }
}

/// Two to four drones a machine, each on a repo with a harness, started over the last two days: this
/// machine's as local presence, the others' as remote presence with their machine record.
fn drones(home: &Path, keys: &[String], now: i64, rng: &mut Rng) -> Vec<Drone> {
    let mut drones = Vec::new();
    for (m, (host, name, email)) in MACHINES.iter().enumerate() {
        for i in 0..2 + rng.below(3) {
            let session = format!("{:08x}-{m}{i}", rng.next() & 0xffff_ffff);
            let (repo, harness) = (REPOS[rng.below(REPOS.len())], HARNESSES[rng.below(HARNESSES.len())]);
            let state = rng.below(10);
            // Working (a prompt a minute ago), waiting (stopped), or idle (an hour without a hook).
            let (event, ms) = match state {
                0..=4 => ("prompt", now - MINUTE),
                5..=7 => ("stop", now - 5 * MINUTE),
                _ => ("stop", now - HOUR),
            };
            let started = now - i64::try_from(rng.below(48)).unwrap_or(0) * HOUR - HOUR;
            let p = Presence {
                session: session.clone(),
                harness: harness.into(),
                engineer: (*email).into(),
                repo: repo.into(),
                collective: COLLECTIVE.into(),
                prompt: "keep the collective in step".into(),
                files: Vec::new(),
                event: event.into(),
                ms,
                started,
                pid: None,
            };
            if m == 0 {
                presence::write(home, &p).expect("a local drone");
            } else {
                let machine = json!({"borg": env!("CARGO_PKG_VERSION"), "handoff": 1, "clones": [], "host": host, "names": [{"email": email, "name": name}]});
                let r = Remote { p, key: keys[m].clone(), sig: "00".into(), machine: Some(machine.to_string()), msig: String::new() };
                append(&presence::remote_path(home), &serde_json::to_string(&r).expect("a remote drone"));
            }
            drones.push(Drone { machine: m, session, repo, harness });
        }
    }
    drones
}

/// The members: this node dials them all, synced with each a minute ago, all on this version.
fn members(home: &Path, keys: &[String], now: i64) {
    let peers: serde_json::Map<String, serde_json::Value> =
        keys[1..].iter().enumerate().map(|(i, k)| (k.clone(), json!(format!("10.0.0.{}:4433", i + 2)))).collect();
    std::fs::write(home.join("mesh/peers.json"), serde_json::Value::Object(peers).to_string()).expect("the peers");
    let synced: serde_json::Map<String, serde_json::Value> = keys[1..].iter().map(|k| (k.clone(), json!(now - MINUTE))).collect();
    std::fs::write(presence::synced_path(home), serde_json::Value::Object(synced).to_string()).expect("the syncs");
    for (m, k) in keys.iter().enumerate().skip(1) {
        let v =
            PeerVersion { borg: Some(env!("CARGO_PKG_VERSION").into()), protocol: protocol::current(), ms: now - MINUTE, engineer: Some(MACHINES[m].2.into()) };
        protocol::note_version(home, k, &v).expect("a member's version");
    }
}

impl Demo {
    /// Two months before the week: lessons and the sessions that asked them, one every few hours.
    fn backlog(&mut self, now: i64) {
        let mut ms = now - 60 * DAY;
        while ms < now - 7 * DAY {
            if self.rng.chance(40) {
                let machine = self.rng.below(MACHINES.len());
                self.lesson(machine, ms);
            } else {
                self.session(ms);
            }
            ms += 2 * HOUR + i64::try_from(self.rng.below(4)).unwrap_or(0) * HOUR;
        }
    }

    /// The week: an event every few minutes in working hours, one about every two minutes in the last
    /// eight hours, so the opening replay is busy. Returns the message lines in time order.
    fn week(&mut self, now: i64) -> Vec<(i64, String)> {
        let mut lines = Vec::new();
        let mut ms = now - 7 * DAY + HOUR;
        while ms < now - MINUTE {
            let recent = now - ms < 8 * HOUR;
            let hour = (ms / HOUR) % 24;
            if recent || (7..19).contains(&hour) {
                self.event(ms, now, &mut lines);
            }
            let gap = if recent { 2 * MINUTE } else { 25 * MINUTE };
            ms += gap / 2 + i64::try_from(self.rng.below(usize::try_from(gap).unwrap_or(1))).unwrap_or(0);
        }
        lines.sort_by_key(|(ms, _)| *ms);
        lines
    }

    /// Live for `secs`: a message about every second, now and then a lesson, a session or a sync. A
    /// line stamped later (a recipient shown a message) waits until its time.
    fn live(&mut self, path: &Path, secs: u64) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        let mut waiting: Vec<(i64, String)> = Vec::new();
        while std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(700 + self.rng.next() % 600));
            let now = borg::learn::now_ms();
            self.event(now, now, &mut waiting);
            waiting.sort_by_key(|(ms, _)| *ms);
            let due = waiting.partition_point(|(ms, _)| *ms <= now);
            for (_, l) in waiting.drain(..due) {
                append(path, &l);
            }
        }
    }
}

fn main() {
    assert!(QUESTIONS.iter().all(|(area, _)| borg::trust::AREAS.contains(area)), "every question's area is one of borg's");
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().unwrap_or_else(|| "/tmp/borg-demo".into()));
    let live: u64 = match (args.next().as_deref(), args.next()) {
        (Some("--live"), Some(s)) => s.parse().expect("--live takes seconds"),
        _ => 0,
    };
    let _ = std::fs::remove_dir_all(&root);
    let repos = root.join("repos");
    for r in REPOS {
        std::fs::create_dir_all(repos.join(r)).expect("a repo directory");
    }
    let config = json!({"collectives": [{"name": COLLECTIVE, "paths": [repos], "share": "team"}]});
    std::fs::write(root.join("collectives.json"), config.to_string()).expect("the config");
    let home = root.join("home").join("collectives").join(COLLECTIVE);
    std::fs::create_dir_all(home.join("mesh")).expect("the store");

    let now = borg::learn::now_ms();
    let keys: Vec<String> = MACHINES.iter().map(|(h, ..)| key(h)).collect();
    std::fs::write(home.join("mesh/node.pub"), format!("{}\n", keys[0])).expect("this node's key");
    let mut rng = Rng(0x0b02_9a11_c0de_cafe);
    let drones = drones(&home, &keys, now, &mut rng);
    borg::names::note(&home, MACHINES[0].2, MACHINES[0].1).expect("this engineer's name");
    members(&home, &keys, now);

    let mut demo = Demo { home: home.clone(), keys, drones, rng, born: Vec::new() };
    demo.backlog(now);
    let lines = demo.week(now);
    let path = msg::path(&home);
    for (_, l) in &lines {
        append(&path, l);
    }
    println!("demo store: {} (collective {COLLECTIVE}, {} drones, {} lessons, {} lines)", home.display(), demo.drones.len(), demo.born.len(), lines.len());
    demo.live(&path, live);
}
