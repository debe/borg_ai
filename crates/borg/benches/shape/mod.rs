//! The store the benches read (borg benches/stores.rs, borg-mesh benches/sync.rs): a copy of the store
//! `BORG_BENCH_STORE` names, else one generated from a fixed seed in the shape of a real team's store.
//!
//! The shape (`generate`) follows the volumes measured on a team collective: `stats.jsonl` with
//! 5,859 lines over 363 ids (94% of them superseded versions, 15.8 MB), 21.4k team outcomes (5 MB), 778
//! lesson lines (0.7 MB), and an origin record per line (28.5k, 8.6 MB). Its words are made up: a real
//! store holds an engineer's work and never enters the repo (it is public), so a bench reads one only
//! from the path in the env var.
//!
//! Shared by both crates' benches (`#[path]`), so they measure the same data.

use std::path::{Path, PathBuf};

/// Names a store to bench on instead of the generated one. It is only read: the benches copy it.
pub const ENV: &str = "BORG_BENCH_STORE";
/// Multiplies the generated store's lines (lessons, rollups, outcomes, reasoning, episodes, and so their
/// origins) by this whole number, to check that a cost does not grow with the store. Default 1; ignored
/// for a copied store.
pub const SCALE: &str = "BORG_BENCH_SCALE";

/// The generated store's seed: every run benches the same lines.
const SEED: u64 = 0x5eed_b0e9;

const LESSON_IDS: usize = 521;
const LESSON_LINES: usize = 778;
const STAT_IDS: usize = 363;
const STAT_LINES: usize = 5_859;
const TEAM_LINES: usize = 21_400;
const OUTCOME_LINES: usize = 22_000;
const MESSAGES: usize = 35;
const SEEN: usize = 12;
const REASONING: usize = 82;
const EPISODES: usize = 566;
const NODES: usize = 5;
const ENGINEERS: usize = 4;
/// Origin records whose line the logs no longer hold (pruned messages, quarantined lines), per mille.
const STALE_ORIGINS_PER_MILLE: usize = 10;

/// The event kinds a stats bucket counts, as borg names them (space-separated).
const KINDS: &str = "asked asked_n block block_n changed corrected early graded hacked ignored live_ask live_check not_corrected placebo queued_free regretted rejected shown shown_n stop tested turn_end verified veto veto_repeat learned consolidated imported check check_n free";
/// What a team outcome line says happened, and how often in a thousand.
const OUTCOMES: [(&str, u64); 6] =
    [("regretted", 758), ("asked_not_corrected", 130), ("asked", 57), ("asked_then_changed", 23), ("asked_ignored", 22), ("tested", 10)];
const AREAS: [&str; 6] = ["correctness", "testing", "security", "style", "process", "other"];
const WORDS: &str = "the lock merge review test cache session build retry deploy config branch query index schema token budget hook worker queue timeout commit release parser stream handler error check before after always never";

/// A directory under the system temp dir, removed when dropped.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(tag: &str) -> Scratch {
        let d = std::env::temp_dir().join(format!("borg-bench-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("bench scratch dir");
        Scratch(d)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Fills `dir` with the store to bench: a copy of `ENV`'s store (its top-level files, `stats/`, and
/// `mesh/origins.jsonl` with `mesh/node.pub`, never its key), else a generated one. Says which, and the
/// size of the logs the benches read.
pub fn fill(dir: &Path) -> String {
    std::fs::create_dir_all(dir.join("mesh")).expect("bench store dir");
    let what = match std::env::var_os(ENV) {
        Some(src) => {
            let src = PathBuf::from(src);
            copy_files(&src, dir);
            copy_files(&src.join("stats"), &dir.join("stats"));
            for f in ["origins.jsonl", "node.pub"] {
                let _ = std::fs::copy(src.join("mesh").join(f), dir.join("mesh").join(f));
            }
            format!("the store in {ENV} ({})", src.display())
        }
        None => {
            let scale = std::env::var(SCALE).map_or(1, |s| s.trim().parse().unwrap_or_else(|_| panic!("{SCALE} is a whole number, not {s:?}")));
            generate(dir, SEED, scale);
            format!("a generated store (seed {SEED:#x}, scale {scale}; set {ENV} to bench a real one)")
        }
    };
    let logs = ["lessons.jsonl", "stats.jsonl", borg::outcome::TEAM_FILE, "outcomes.jsonl", borg::msg::FILE, "reasoning.jsonl", "mesh/origins.jsonl"];
    // Counted through a small buffer: a log read whole here would be the run's heap peak, not the unit's.
    let sizes: Vec<String> = logs
        .iter()
        .filter_map(|f| {
            let file = std::fs::File::open(dir.join(f)).ok()?;
            let len = file.metadata().ok()?.len();
            let lines = std::io::BufRead::split(std::io::BufReader::new(file), b'\n').count();
            Some(format!("{f} {lines} lines {} kB", len / 1000))
        })
        .collect();
    format!("{what}: {}", sizes.join(", "))
}

fn copy_files(from: &Path, to: &Path) {
    let Ok(entries) = std::fs::read_dir(from) else { return };
    std::fs::create_dir_all(to).expect("bench store dir");
    for e in entries.flatten() {
        if e.file_type().is_ok_and(|t| t.is_file()) {
            std::fs::copy(e.path(), to.join(e.file_name())).expect("copy the bench store");
        }
    }
}

/// SplitMix64: a seeded stream, so a generated store is the same on every run and machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn hex(&mut self, n: usize) -> String {
        (0..n).map(|_| char::from_digit((self.next() % 16) as u32, 16).unwrap_or('0')).collect()
    }

    fn words(&mut self, n: usize) -> String {
        let words: Vec<&str> = WORDS.split(' ').collect();
        (0..n).map(|_| words[self.below(words.len())]).collect::<Vec<_>>().join(" ")
    }
}

/// An outcome kind, drawn by `OUTCOMES`' weights.
fn outcome_kind(r: &mut Rng) -> &'static str {
    let mut x = r.below(1000) as u64;
    for (kind, w) in OUTCOMES {
        if x < w {
            return kind;
        }
        x -= w;
    }
    "asked"
}

fn write_lines(path: &Path, lines: &[String]) {
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(path, text).expect("write the bench store");
}

/// Writes a store of the measured shape (module doc) into `dir`, origins and all, `scale` times its lines
/// (`SCALE`; the messages stay as many, as their log is bounded).
fn generate(dir: &Path, seed: u64, scale: usize) {
    use serde_json::json;
    let (lesson_ids_n, lesson_lines, stat_ids, stat_lines, team_lines, outcome_lines, reasoning_n, episodes_n) = (
        LESSON_IDS * scale,
        LESSON_LINES * scale,
        STAT_IDS * scale,
        STAT_LINES * scale,
        TEAM_LINES * scale,
        OUTCOME_LINES * scale,
        REASONING * scale,
        EPISODES * scale,
    );
    let mut r = Rng(seed);
    let now = borg::learn::now_ms();
    let day_ms = borg::clock::DAY_MS;
    let keys: Vec<String> = (0..NODES).map(|_| r.hex(64)).collect();
    let nodes: Vec<String> = (0..NODES).map(|_| r.hex(16)).collect();
    let engineers: Vec<String> = (0..ENGINEERS).map(|i| format!("dev{i}@example.com")).collect();
    let lesson_ids: Vec<String> = (0..lesson_ids_n).map(|_| format!("I{}", r.hex(12))).collect();

    // Lessons: every id once, then later versions appended as consolidations append them.
    let mut lessons = Vec::with_capacity(lesson_lines);
    let mut version = vec![0u32; lesson_ids_n];
    let order: Vec<usize> = (0..lesson_ids_n).chain((0..lesson_lines - lesson_ids_n).map(|_| r.below(lesson_ids_n))).collect();
    for i in order {
        version[i] += 1;
        let situations: Vec<String> = (0..3).map(|_| r.words(14)).collect();
        let episodes: Vec<String> = (0..3).map(|_| format!("E{}", r.hex(12))).collect();
        lessons.push(
            json!({"id": lesson_ids[i], "origin_ms": now - 40 * day_ms, "repo": format!("repo-{}", i % 9), "question": r.words(28),
                "situations": situations, "consequence": r.words(22), "support": 1 + r.below(9), "episodes": episodes, "history": [],
                "version": version[i], "engineers": [engineers[i % ENGINEERS]], "area": AREAS[i % AREAS.len()]})
            .to_string(),
        );
    }

    // Usage rollups: per (node, engineer, day) bucket a run of versions, each a little fuller, interleaved
    // in time as nodes republish them; the sizes skew as a real store's do (most small, a few huge).
    let mut runs: Vec<usize> = (0..stat_ids).map(|_| 1 + r.below(31)).collect();
    let mut total: usize = runs.iter().sum();
    while total != stat_lines {
        let i = r.below(stat_ids);
        if total < stat_lines {
            runs[i] += 1;
            total += 1;
        } else if runs[i] > 1 {
            runs[i] -= 1;
            total -= 1;
        }
    }
    let widths: Vec<usize> = (0..stat_ids).map(|_| (190.0 * r.unit().powi(2)) as usize).collect();
    let mut events: Vec<(i64, usize, usize)> = Vec::with_capacity(stat_lines);
    for (i, &n) in runs.iter().enumerate() {
        let start = now - 60 * day_ms + (r.below(55) as i64) * day_ms;
        for v in 1..=n {
            events.push((start + (v as i64) * 3_600_000 + r.below(3_600_000) as i64, i, v));
        }
    }
    events.sort_unstable();
    let stats: Vec<String> = events
        .iter()
        .map(|&(_, i, v)| {
            let node = &nodes[i % NODES];
            let engineer = &engineers[i % ENGINEERS];
            let day = format!("2026-{:02}-{:02}", 9 + i % 3, 1 + i % 28);
            let counts: serde_json::Map<String, serde_json::Value> = KINDS.split(' ').map(|k| (k.to_string(), json!(r.below(50 * v)))).collect();
            let named = widths[i] * v / runs[i];
            let per_lesson: serde_json::Map<String, serde_json::Value> = (0..named)
                .map(|j| (lesson_ids[(i * 7 + j) % lesson_ids_n].clone(), json!({"not_corrected": r.below(9), "regretted": r.below(200), "tested": 1})))
                .collect();
            json!({"id": format!("stat:{node}:{engineer}:{day}"), "version": v, "day": day, "node": node, "engineer": engineer,
                "counts": counts, "usd": r.unit() * 60.0, "usd_by": {"check": r.unit() * 5.0, "live_check": r.unit() * 50.0},
                "dur_ms": r.below(9_000_000), "lessons": per_lesson, "usd_mode": {"live": r.unit() * 50.0},
                "channels": {"block@stop": r.below(9), "changed@live": r.below(9)}, "why": {"check_ms": r.below(500_000), "check_timed": r.below(40)}})
            .to_string()
        })
        .collect();

    let team: Vec<String> = (0..team_lines)
        .map(|i| {
            let kind = outcome_kind(&mut r);
            let mut o = json!({"kind": kind, "s": r.hex(32), "lessons": [lesson_ids[r.below(lesson_ids_n)]], "ms": now - 30 * day_ms + i as i64 * 100_000,
                "node": nodes[i % NODES], "r": r.hex(32)});
            if kind == "regretted" {
                o["c"] = json!(r.hex(32));
                o["credit"] = json!(r.unit());
            }
            o.to_string()
        })
        .collect();
    let outcomes: Vec<String> = (0..outcome_lines)
        .map(|i| {
            let kind = outcome_kind(&mut r);
            let mut o = json!({"kind": kind, "session": format!("{}-{}-{}-{}-{}", r.hex(8), r.hex(4), r.hex(4), r.hex(4), r.hex(12)),
                "lessons": [lesson_ids[r.below(lesson_ids_n)]], "ms": now - 30 * day_ms + i as i64 * 100_000,
                "repo": format!("/home/dev/work/team/repository-{}", i % 9)});
            if kind == "regretted" {
                o["cause"] = json!(format!("R{}", r.hex(12)));
                o["credit"] = json!(r.unit());
            } else {
                o["channel"] = json!("stop");
                o["p"] = json!([r.unit()]);
                o["grades"] = json!([1 + r.below(3)]);
                o["ranks"] = json!([r.below(4)]);
            }
            o.to_string()
        })
        .collect();

    // Messages: tells to everyone within the last day, and the seen lines their readers wrote.
    let mut messages: Vec<String> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    for i in 0..MESSAGES {
        let from = borg::msg::Sender { by: format!("s{}", r.hex(8)), name: "One of Five".into(), harness: "claude".into(), node: keys[i % NODES].clone() };
        let m = borg::msg::Message::new(borg::msg::TELL, &from, borg::msg::ALL, None, now - day_ms + i as i64 * 60_000, &r.words(120));
        ids.push(m.id.clone());
        messages.push(serde_json::to_string(&m).expect("a message serialises"));
    }
    for i in 0..SEEN {
        let seen = borg::msg::Seen::new(&format!("s{}", r.hex(8)), &keys[(i + 1) % NODES], now - day_ms / 2 + i as i64 * 60_000, &[ids[i].as_str()]);
        messages.push(serde_json::to_string(&seen).expect("a seen line serialises"));
    }

    let reasoning: Vec<String> = (0..reasoning_n)
        .map(|i| {
            json!({"kind": "excerpt", "commit": r.hex(40), "repo": format!("repo-{}", i % 9), "session": r.hex(12), "task": r.words(30),
                "proposal": r.words(120), "episodes": [format!("E{}", r.hex(12))], "ms": now - 20 * day_ms, "collective": "bench"})
            .to_string()
        })
        .collect();
    let episodes: Vec<String> = (0..episodes_n)
        .map(|i| {
            json!({"id": format!("E{}", r.hex(12)), "ms": now - 50 * day_ms + i as i64 * 3_600_000, "repo": format!("/work/repo-{}", i % 9),
                "session": r.hex(12), "task": r.words(40), "agent": r.words(200), "rejected_tool": null, "correction": r.words(500),
                "engineer": engineers[i % ENGINEERS]})
            .to_string()
        })
        .collect();

    // One origin per line, stamped by a member (a message by the node it names), and a few whose line is
    // gone. Their signatures are placeholders: the mesh bench signs every record again with its own keys.
    let stale = (lessons.len() + stats.len() + team.len() + reasoning.len() + messages.len()) * STALE_ORIGINS_PER_MILLE / 1000;
    let mut origins: Vec<String> = Vec::new();
    let mut origin = |line: &str, key: &str, ms: i64| {
        let h = blake3::hash(line.as_bytes()).to_hex().to_string();
        origins.push(json!({"h": h, "key": key, "ms": ms, "sig": "0".repeat(128)}).to_string());
    };
    for (i, l) in lessons.iter().chain(&stats).chain(&team).chain(&reasoning).enumerate() {
        origin(l, &keys[i % NODES], now - day_ms + i as i64);
    }
    for l in &messages {
        let node = serde_json::from_str::<serde_json::Value>(l).ok().and_then(|v| v["node"].as_str().map(str::to_string)).unwrap_or_default();
        origin(l, &node, now - day_ms);
    }
    for i in 0..stale {
        let gone = format!("{{\"gone\":{i}}}");
        origin(&gone, &keys[i % NODES], now - 10 * day_ms);
    }

    write_lines(&dir.join("lessons.jsonl"), &lessons);
    write_lines(&dir.join("stats.jsonl"), &stats);
    write_lines(&dir.join(borg::outcome::TEAM_FILE), &team);
    write_lines(&dir.join("outcomes.jsonl"), &outcomes);
    write_lines(&dir.join(borg::msg::FILE), &messages);
    write_lines(&dir.join("reasoning.jsonl"), &reasoning);
    write_lines(&dir.join("episodes.jsonl"), &episodes);
    write_lines(&dir.join("mesh").join("origins.jsonl"), &origins);
    std::fs::write(dir.join("mesh").join("node.pub"), format!("{}\n", keys[0])).expect("write the bench store");
    // The team file already holds this node's shared outcomes: `outcome::publish` has nothing to add.
    let local = std::fs::metadata(dir.join("outcomes.jsonl")).map_or(0, |m| m.len());
    std::fs::write(dir.join(borg::outcome::PUBLISHED), local.to_string()).expect("write the bench store");
}
