//! Retrieval cues (0.7.7): per lesson, ten short texts in the words of the work where it matters again,
//! written once when the lesson is learned, and logged as a shadow score only (pools.rs `cue`, `mv`).
//!
//! A lesson is written in the words of its correction; the work that needs it again is written in the
//! words of prompts, paths, commands, proposals and identifiers. One model call per new or merged lesson
//! writes cues in that vocabulary (`PROMPT`), with the incident the lesson came from. The offline
//! measure found them the strongest single word channel of the candidate orders, alone and fused
//! (internal evaluation "learn-retrieval", not published); whether they rank the pool better is the
//! pre-registered switch test's to say, so until it passes they change nothing a drone sees.
//!
//! **Where:** `cues.jsonl` beside the store (`Cued`), local, never synced, never a facet: facets are
//! BM25 documents of the lesson (conscience.rs `documents`) and travel over the mesh, so cues stored
//! there would change the pool before the test. A cue line is keyed by the lesson's text (`key`): a
//! merge that rewrites the lesson asks again, a version that keeps the text keeps its cues.
//!
//! **When:** after `borg learn` for the lessons it wrote (main.rs `learn_store`), on by default
//! (`BORG_CUES=off` turns it off), and by `borg cues [--apply]` for lessons learned before 0.7.7 (a dry run
//! with its cost by default). Each call's cost is on its line and on the `model_call` event (purpose
//! `cues`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::facets::Ask;
use crate::learn::{self, Lesson};

/// The kinds a model answer's cues are kept under, in the order they are stored.
pub const KINDS: [&str; 5] = ["prompt", "path_or_command", "proposal", "identifier", "move"];

/// Guard: at most this many cues per lesson, each at most `CUE_CHARS` characters; the prompt asks for
/// exactly 10, so a good answer is never cut.
pub const MAX_CUES: usize = 10;
pub const CUE_CHARS: usize = 300;
/// Guard: a model answer with fewer usable cues is no answer (a later run retries).
const MIN_CUES: usize = 3;

/// The incident's excerpts in the prompt, each at most this many characters and ` …` (learn.rs
/// `EXCERPT_CHARS`: what an episode keeps of a correction).
pub const INCIDENT_CHARS: usize = learn::EXCERPT_CHARS;

/// The dry run's estimate per lesson before any call measured a cost, in tokens: the offline measure,
/// 1,415,298 tokens for 678 lessons, about 2.1k each (internal evaluation "learn-retrieval", 2026-11-02).
/// Display only; once `cues.jsonl` holds a measured cost, its mean is shown instead.
pub const EST_TOKENS_PER_LESSON: usize = 2_100;

/// The cue prompt, fixed with the offline measure it was evaluated in.
pub const PROMPT: &str = "You write retrieval cues for one lesson in an engineering team's memory. The lesson was learned when \
an engineer corrected a coding agent. Later, a word-matching search (BM25) compares the lesson's cues against the text of a \
coding agent's ongoing work: the engineer's prompts, the agent's messages and plans, the file paths, commands and code it \
touches. The cues must make the lesson come up at the moments in such work where it matters again.

Write 10 short cues, each at most 25 words, in the vocabulary of that WORK, not paraphrases of the lesson. Write exactly:
- 2 \"prompt\": user prompts an engineer might type when asking for work where this lesson matters;
- 2 \"path_or_command\": file paths, globs or shell commands an agent would touch or run in such work;
- 2 \"proposal\": sentences an agent might write when proposing the flawed move the lesson warns against;
- 2 \"identifier\": code identifiers or API calls typical of the situation;
- 2 \"move\": bare abstract design moves in the grammar \"<action> on/in <kind of state> [under <condition>]\", without project or class names.
At least one cue must come from a different domain or repository than the incident below, where the same kind of mistake \
could happen. Do not reuse the lesson's own sentences; use the words people and tools write while doing the work.";

/// On unless `BORG_CUES=off`.
#[must_use]
pub fn enabled() -> bool {
    !std::env::var("BORG_CUES").is_ok_and(|v| v.trim().eq_ignore_ascii_case("off"))
}

fn schema() -> Value {
    json!({"type": "object", "additionalProperties": false, "properties": {
        "cues": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {
            "kind": {"type": "string", "enum": KINDS}, "text": {"type": "string"}}, "required": ["kind", "text"]}}},
        "required": ["cues"]})
}

/// A lesson text's key: the first 8 bytes of the BLAKE3 hash of `[question, situations, consequence]`
/// as JSON, in hex. Its cues and the pool log's `ver` name the wording by it.
#[must_use]
pub fn key(question: &str, situations: &[String], consequence: &str) -> String {
    let text = serde_json::to_string(&(question, situations, consequence)).unwrap_or_default();
    blake3::hash(text.as_bytes()).as_bytes()[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// `s` trimmed, at most `INCIDENT_CHARS` characters, then ` …` when cut.
fn clip(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= INCIDENT_CHARS { s.to_string() } else { format!("{} …", s.chars().take(INCIDENT_CHARS).collect::<String>()) }
}

/// The incident a lesson came from: the first of its episodes in `episodes` with a correction, as the
/// agent's proposal and the engineer's correction; `(no incident recorded)` without one (a synced peer's
/// lesson: its episodes stay on the peer).
#[must_use]
pub fn incident(l: &Lesson, episodes: &HashMap<String, learn::Episode>) -> String {
    match l.episodes.iter().find_map(|e| episodes.get(e).filter(|e| !e.correction.trim().is_empty())) {
        Some(e) => format!("agent's proposal: {}\nengineer's correction: {}", clip(&e.agent), clip(&e.correction)),
        None => "(no incident recorded)".into(),
    }
}

/// The prompt for one lesson: what it says and its incident, nothing about where it is stored.
#[must_use]
pub fn prompt(l: &Lesson, incident: &str) -> String {
    let situations: Vec<String> = l.situations.iter().map(|s| format!("- {s}")).collect();
    format!(
        "{PROMPT}\n\n## Lesson\nquestion: {}\nsituations:\n{}\nconsequence: {}\n\n## Incident the lesson was learned from\n{incident}",
        l.question,
        situations.join("\n"),
        l.consequence
    )
}

/// One cue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    pub kind: String,
    pub text: String,
}

/// The cues in a model answer: flattened, clipped, unique, in `KINDS` order (stable within a kind), at
/// most `MAX_CUES`. None when fewer than `MIN_CUES` remain.
#[must_use]
pub fn parse(v: &Value) -> Option<Vec<Cue>> {
    let mut rows: Vec<(usize, Cue)> = Vec::new();
    for c in v["cues"].as_array()? {
        let kind = c["kind"].as_str().unwrap_or_default();
        let at = KINDS.iter().position(|k| *k == kind).unwrap_or(KINDS.len());
        let text: String = c["text"].as_str().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
        let text: String = text.chars().take(CUE_CHARS).collect();
        if !text.is_empty() && !rows.iter().any(|(_, x)| x.text.eq_ignore_ascii_case(&text)) {
            rows.push((at, Cue { kind: kind.into(), text }));
        }
    }
    rows.sort_by_key(|(at, _)| *at);
    let out: Vec<Cue> = rows.into_iter().map(|(_, c)| c).take(MAX_CUES).collect();
    (out.len() >= MIN_CUES).then_some(out)
}

/// One line of `cues.jsonl`: a lesson's cues for the text `key` names, at what cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cued {
    pub id: String,
    pub version: u32,
    pub key: String,
    pub cues: Vec<Cue>,
    pub cost_usd: Option<f64>,
    pub ms: i64,
}

/// `cues.jsonl` beside a lessons file.
#[must_use]
pub fn beside(lessons: &Path) -> PathBuf {
    lessons.with_file_name("cues.jsonl")
}

/// Every lesson text's cues in the store of `lessons`, by `key` (the latest line of a key wins), read
/// line by line.
#[must_use]
pub fn load(lessons: &Path) -> HashMap<String, Vec<String>> {
    load_before(lessons, None)
}

/// `load` of the cues written before `before_ms` (a check's stop: what the store held then).
#[must_use]
pub fn load_before(lessons: &Path, before_ms: Option<i64>) -> HashMap<String, Vec<String>> {
    use std::io::BufRead;
    let mut out = HashMap::new();
    let Ok(f) = std::fs::File::open(beside(lessons)) else { return out };
    for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
        if let Ok(c) = serde_json::from_str::<Cued>(&line)
            && before_ms.is_none_or(|b| c.ms < b)
        {
            out.insert(c.key, c.cues.into_iter().map(|c| c.text).collect());
        }
    }
    out
}

/// What `fill` did or (dry run) would do.
#[derive(Debug, Default)]
pub struct Report {
    /// Lessons (latest versions) whose text has no cues, among those asked for.
    pub missing: usize,
    pub written: usize,
    pub failed: Vec<String>,
    pub cost_usd: f64,
}

/// Give cues to the lessons of the store at `path` whose text has none (`key`), limited to `only` when
/// given. A dry run (`apply` false) asks nothing and writes nothing. With `apply`, `jobs` calls at a time;
/// each answer is appended to `cues.jsonl` as one line. After `runner::MAX_FAILURES` failures without
/// a success the run stops asking.
pub fn fill(path: &Path, only: Option<&HashSet<String>>, apply: bool, ask: Ask, jobs: usize, now: i64) -> Report {
    let have = load(path);
    let todo: Vec<Lesson> = learn::lessons(path)
        .into_iter()
        .filter(|l| !l.question.trim().is_empty() && l.question != crate::reach::MOVED)
        .filter(|l| only.is_none_or(|o| o.contains(&l.id)) && !have.contains_key(&key(&l.question, &l.situations, &l.consequence)))
        .collect();
    let report = Report { missing: todo.len(), ..Report::default() };
    if !apply || todo.is_empty() {
        return report;
    }
    let episodes: HashMap<String, learn::Episode> = learn::episodes_of(&path.with_file_name("episodes.jsonl"));
    let next = AtomicUsize::new(0);
    let (failures, successes) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let out: Mutex<Report> = Mutex::new(report);
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1).min(todo.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(l) = todo.get(i) else { break };
                    let lock = || out.lock().unwrap_or_else(|e| e.into_inner());
                    if failures.load(Ordering::SeqCst) >= crate::runner::MAX_FAILURES && successes.load(Ordering::SeqCst) == 0 {
                        lock().failed.push(l.id.clone());
                        continue;
                    }
                    let got = ask(&prompt(l, &incident(l, &episodes)), &schema()).and_then(|(v, cost)| Some((parse(&v)?, cost)));
                    let mut r = lock();
                    let Some((cues, cost)) = got else {
                        failures.fetch_add(1, Ordering::SeqCst);
                        r.failed.push(l.id.clone());
                        continue;
                    };
                    successes.fetch_add(1, Ordering::SeqCst);
                    r.cost_usd += cost.unwrap_or(0.0);
                    let rec =
                        Cued { id: l.id.clone(), version: l.version, key: key(&l.question, &l.situations, &l.consequence), cues, cost_usd: cost, ms: now };
                    match serde_json::to_string(&rec).map_err(std::io::Error::other).and_then(|line| learn::append_line(&beside(path), &line)) {
                        Ok(()) => r.written += 1,
                        Err(_) => r.failed.push(l.id.clone()),
                    }
                }
            });
        }
    });
    let mut report = out.into_inner().unwrap_or_else(|e| e.into_inner());
    report.cost_usd += 0.0; // an empty f64 sum is -0.0
    report
}

/// The measured mean cost of a lesson's cues in the store of `lessons` (USD), None before any call
/// reported one.
#[must_use]
pub fn mean_cost(lessons: &Path) -> Option<f64> {
    use std::io::BufRead;
    let f = std::fs::File::open(beside(lessons)).ok()?;
    let costs: Vec<f64> = std::io::BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str::<Cued>(&l).ok()?.cost_usd).collect();
    (!costs.is_empty()).then(|| costs.iter().sum::<f64>() / costs.len() as f64)
}

pub const USAGE: &str = "usage: borg cues [--apply] [--store FILE] [--jobs N]";

/// `borg cues [--apply] [--store PATH] [--jobs N]`: backfill cues for a store (default: this
/// collective's, `BORG_HOME`). The exit code.
pub fn main(args: &[&str], ask: Ask) -> i32 {
    let (mut store, mut jobs, mut apply) = (learn::home().join("lessons.jsonl"), crate::runner::JOBS, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--apply" => apply = true,
            "--store" => match it.next() {
                Some(p) => store = PathBuf::from(p),
                None => {
                    eprintln!("{USAGE}");
                    return 2;
                }
            },
            "--jobs" => match it.next().and_then(|j| j.parse().ok()) {
                Some(j) => jobs = j,
                None => {
                    eprintln!("{USAGE}");
                    return 2;
                }
            },
            other => {
                eprintln!("borg cues: unknown argument {other}\n{USAGE}");
                return 2;
            }
        }
    }
    if !store.is_file() {
        eprintln!("borg cues: no lessons at {}", store.display());
        return 2;
    }
    let total = learn::lessons(&store).len();
    let r = fill(&store, None, apply, ask, jobs, learn::now_ms());
    if !apply {
        let cost = match mean_cost(&store) {
            Some(usd) => format!("about ${:.2} (${usd:.4} per lesson, measured)", r.missing as f64 * usd),
            None => format!("about {} tokens (~{EST_TOKENS_PER_LESSON} per lesson, the offline measure)", r.missing * EST_TOKENS_PER_LESSON),
        };
        println!(
            "{}: {total} lessons, {} without cues; generating them is {} model calls, {cost}. Dry run: nothing written; --apply to generate.",
            store.display(),
            r.missing,
            r.missing
        );
        return 0;
    }
    println!("{}: cues written for {} of {} lessons without them, ${:.4}", store.display(), r.written, r.missing, r.cost_usd);
    if !r.failed.is_empty() {
        eprintln!("no usable answer from the model for {} lessons (re-run to retry): {}", r.failed.len(), r.failed.join(", "));
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lesson(id: &str, q: &str) -> Lesson {
        Lesson { id: id.into(), origin_ms: 1, question: q.into(), consequence: "c".into(), version: 1, area: "other".into(), ..Default::default() }
    }

    fn answer(n: usize) -> Value {
        json!({"cues": (0..n).map(|i| json!({"kind": KINDS[KINDS.len() - 1 - i % KINDS.len()], "text": format!("  cue   {i}  ")})).collect::<Vec<_>>()})
    }

    /// A check reads the cues the store held at its stop: one written after (a `borg learn` meanwhile)
    /// is not in its pool log's scores.
    #[test]
    fn cues_are_read_as_of_a_time() {
        let d = std::env::temp_dir().join(format!("borg-cues-before-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let lessons = d.join("lessons.jsonl");
        let line = |key: &str, text: &str, ms: i64| {
            let c = Cued { id: key.into(), version: 1, key: key.into(), cues: vec![Cue { kind: "move".into(), text: text.into() }], cost_usd: None, ms };
            format!("{}\n", serde_json::to_string(&c).unwrap())
        };
        std::fs::write(beside(&lessons), line("k1", "old", 10) + &line("k2", "later", 30) + &line("k1", "newer", 40)).unwrap();
        let at = load_before(&lessons, Some(30));
        assert_eq!((at.len(), at["k1"].clone()), (1, vec!["old".to_string()]));
        assert_eq!(load(&lessons)["k1"], ["newer"], "all of them, the latest of a key winning");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn parse_orders_by_kind_cleans_and_needs_three() {
        let c = parse(&answer(10)).unwrap();
        assert_eq!(c.len(), 10);
        assert_eq!((c[0].kind.as_str(), c[0].text.as_str()), ("prompt", "cue 4"));
        assert_eq!(c.last().unwrap().kind, "move");
        assert_eq!(parse(&answer(14)).unwrap().len(), MAX_CUES);
        assert!(parse(&answer(2)).is_none());
        assert!(parse(&json!({"error": "rate limited"})).is_none());
    }

    #[test]
    fn the_prompt_carries_the_lesson_and_its_incident_not_its_store() {
        let l = Lesson {
            situations: vec!["on a replay".into()],
            repo: "secret-repo".into(),
            episodes: vec!["E1".into(), "E2".into()],
            ..lesson("L1", "Is the budget drained?")
        };
        let mut eps = HashMap::new();
        eps.insert(
            "E2".to_string(),
            learn::Episode { id: "E2".into(), agent: "I reset it".into(), correction: format!("no, drain it {}", "x".repeat(2000)), ..Default::default() },
        );
        let inc = incident(&l, &eps);
        assert!(inc.starts_with("agent's proposal: I reset it\nengineer's correction: no, drain it x") && inc.ends_with(" …"), "{inc}");
        let p = prompt(&l, &inc);
        assert!(p.contains("question: Is the budget drained?\nsituations:\n- on a replay\nconsequence: c") && p.contains(&inc));
        assert!(!p.contains("secret-repo") && !p.contains("L1"));
        assert_eq!(incident(&lesson("L2", "q"), &eps), "(no incident recorded)");
    }

    /// A dry run asks nothing; `--apply` writes one line per lesson, keyed by its text; a lesson whose
    /// text has cues is not asked again; a merge that rewrites it is.
    #[test]
    fn fill_writes_cues_by_text_and_skips_those_with_them() {
        let d = std::env::temp_dir().join(format!("borg-cues-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("lessons.jsonl");
        for l in [lesson("A", "first?"), lesson("B", "second?")] {
            learn::append_line(&p, &serde_json::to_string(&l).unwrap()).unwrap();
        }
        let calls = AtomicUsize::new(0);
        let ask = |_: &str, _: &Value| {
            calls.fetch_add(1, Ordering::SeqCst);
            Some((answer(10), Some(0.002)))
        };
        let r = fill(&p, None, false, &ask, 2, 5);
        assert_eq!((r.missing, r.written, calls.load(Ordering::SeqCst)), (2, 0, 0), "a dry run asks nothing");
        let r = fill(&p, None, true, &ask, 2, 5);
        assert_eq!((r.written, calls.load(Ordering::SeqCst)), (2, 2));
        let have = load(&p);
        assert_eq!(have.get(&key("first?", &[], "c")).map(Vec::len), Some(10));
        assert!((mean_cost(&p).unwrap() - 0.002).abs() < 1e-12);
        assert_eq!(fill(&p, None, true, &ask, 2, 5).missing, 0, "nothing left");
        let merged = Lesson { version: 2, question: "first, reworded?".into(), ..lesson("A", "") };
        learn::append_line(&p, &serde_json::to_string(&merged).unwrap()).unwrap();
        let r = fill(&p, None, true, &ask, 2, 5);
        assert_eq!((r.missing, r.written), (1, 1), "a new wording is asked again");
        let _ = std::fs::remove_dir_all(&d);
    }
}
