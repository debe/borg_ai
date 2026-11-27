//! A made-up team collective, to try `borg web` without a store of your own and to film the Cube for the
//! README: six machines, their engineers' drones, and a week of messages, lessons, absorbs, hand-offs
//! and mesh syncs, busiest in the last eight hours (what the Cube replays when it opens). With
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

use borg::msg::{self, Message, Seen, Sender};
use borg::presence::{self, Presence, Remote};
use borg::protocol::{self, PeerVersion};
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
const LESSONS: [&str; 10] = [
    "Does this migration lock a large table while it runs?",
    "Is the retry idempotent if the first call did go through?",
    "Did the full suite run, not only the changed test?",
    "Does the cache key include everything the result depends on?",
    "Is the money computed in integer cents, not floats?",
    "Would this log line print a secret or a customer's data?",
    "Does the new endpoint check who may call it?",
    "Is the timeout shorter than the caller's?",
    "Does the feature flag default to off in production?",
    "Was the generated client rebuilt after the schema changed?",
];

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
    lessons: usize,
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
    /// names its machine.
    fn lesson(&mut self, machine: usize, ms: i64) {
        let update = self.lessons > 0 && self.rng.chance(20);
        let (n, version) = if update { (self.rng.below(self.lessons) + 1, 2) } else { (self.lessons + 1, 1) };
        if !update {
            self.lessons += 1;
        }
        let repo = REPOS[self.rng.below(REPOS.len())];
        let line = json!({"id": format!("L{n}"), "origin_ms": ms, "repo": repo, "question": LESSONS[(n - 1) % LESSONS.len()],
            "situations": ["a change of this kind"], "consequence": "a failure the engineer corrected once", "support": 1,
            "episodes": [format!("E{n}")], "version": version, "engineers": [MACHINES[machine].2], "area": "other"})
        .to_string();
        let h = borg::mesh::line_hash(&line).iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        });
        append(&self.home.join("lessons.jsonl"), &line);
        append(&borg::mesh::origins_path(&self.home), &json!({"h": h, "key": self.keys[machine], "ms": ms, "sig": "00"}).to_string());
        if !update && self.rng.chance(15) {
            let absorbed =
                json!({"id": format!("L{n}"), "kind": "absorb", "ms": ms + 20 * MINUTE, "by": MACHINES[machine].2, "note": "skills/review", "version": 1});
            append(&borg::absorb::path(&self.home), &absorbed.to_string());
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
            0..=59 => self.message(ms, out),
            60..=74 => {
                let machine = self.rng.below(MACHINES.len());
                self.lesson(machine, ms);
            }
            75..=78 => self.handoff(ms, out),
            _ => self.sync(ms, now),
        }
    }
}

fn main() {
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

    // Two to four drones a machine, each on a repo with a harness, started over the last two days.
    let mut rng = Rng(0x0b02_9a11_c0de_cafe);
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
                presence::write(&home, &p).expect("a local drone");
            } else {
                let machine = json!({"borg": env!("CARGO_PKG_VERSION"), "handoff": 1, "clones": [], "host": host, "names": [{"email": email, "name": name}]});
                let r = Remote { p, key: keys[m].clone(), sig: "00".into(), machine: Some(machine.to_string()), msig: String::new() };
                append(&presence::remote_path(&home), &serde_json::to_string(&r).expect("a remote drone"));
            }
            drones.push(Drone { machine: m, session, repo, harness });
        }
    }
    borg::names::note(&home, MACHINES[0].2, MACHINES[0].1).expect("this engineer's name");

    // The members: this node dials them all, synced with each a minute ago, all on this version.
    let peers: serde_json::Map<String, serde_json::Value> =
        keys[1..].iter().enumerate().map(|(i, k)| (k.clone(), json!(format!("10.0.0.{}:4433", i + 2)))).collect();
    std::fs::write(home.join("mesh/peers.json"), serde_json::Value::Object(peers).to_string()).expect("the peers");
    let synced: serde_json::Map<String, serde_json::Value> = keys[1..].iter().map(|k| (k.clone(), json!(now - MINUTE))).collect();
    std::fs::write(presence::synced_path(&home), serde_json::Value::Object(synced).to_string()).expect("the syncs");
    for (m, k) in keys.iter().enumerate().skip(1) {
        let v =
            PeerVersion { borg: Some(env!("CARGO_PKG_VERSION").into()), protocol: protocol::current(), ms: now - MINUTE, engineer: Some(MACHINES[m].2.into()) };
        protocol::note_version(&home, k, &v).expect("a member's version");
    }

    // The week: an event every few minutes in working hours, one about every two minutes in the last
    // eight hours, so the opening replay is busy.
    let mut demo = Demo { home: home.clone(), keys, drones, rng, lessons: 0 };
    let mut lines = Vec::new();
    let mut ms = now - 7 * DAY + HOUR;
    while ms < now - MINUTE {
        let recent = now - ms < 8 * HOUR;
        let hour = (ms / HOUR) % 24;
        if recent || (7..19).contains(&hour) {
            demo.event(ms, now, &mut lines);
        }
        let gap = if recent { 2 * MINUTE } else { 25 * MINUTE };
        ms += gap / 2 + i64::try_from(demo.rng.below(usize::try_from(gap).unwrap_or(1))).unwrap_or(0);
    }
    lines.sort_by_key(|(ms, _)| *ms);
    let path = msg::path(&home);
    for (_, l) in &lines {
        append(&path, l);
    }
    println!("demo store: {} (collective {COLLECTIVE}, {} drones, {} lines)", home.display(), demo.drones.len(), lines.len());

    // Live: a message about every second, now and then a lesson or a sync. A line stamped later (a
    // recipient shown a message) waits until its time.
    let end = std::time::Instant::now() + std::time::Duration::from_secs(live);
    let mut waiting: Vec<(i64, String)> = Vec::new();
    while std::time::Instant::now() < end {
        std::thread::sleep(std::time::Duration::from_millis(700 + demo.rng.next() % 600));
        let now = borg::learn::now_ms();
        demo.event(now, now, &mut waiting);
        waiting.sort_by_key(|(ms, _)| *ms);
        let due = waiting.partition_point(|(ms, _)| *ms <= now);
        for (_, l) in waiting.drain(..due) {
            append(&path, &l);
        }
    }
}
