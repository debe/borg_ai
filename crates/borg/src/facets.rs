//! Facets: one lesson phrased several ways, so keyword search finds it from different angles
//! (document expansion, "doc2query"; internal evaluation "facets", not published).
//!
//! A lesson is written once, in the words of the correction it came from. A drone meets the same
//! concern in other words: as a symptom it observes, as the edit it is about to make, in German, or
//! after a rename. One model call per lesson writes 6–8 short facets, each from a fixed angle (`ANGLES`):
//! the concept without names, the symptom, the action, the consequence, German, and the identifiers
//! as aliases. Names churn and concepts persist, so the identifiers are one facet among several,
//! never the only key. The conscience indexes every facet as its own document of the lesson
//! (conscience.rs `rank`); a drone never sees them, and the question and consequence never change.
//!
//! **When:** after `borg learn` and `borg assimilate --from-dir --apply`, for the lessons they wrote
//! (`fill` with their ids), and by `borg facets [--apply]` for a store learned before facets existed
//! (a dry run with a cost estimate by default). Every call's cost is recorded in `facets.jsonl` beside
//! the store. Off by default (the pre-registered test failed, internal evaluation "facets", not published): `BORG_FACETS=on` turns the
//! automatic step on; `borg facets --apply` works either way.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::learn::{self, Lesson};

/// The model call: (prompt, JSON schema) -> (answer, cost in USD). `learn::ask_model` in production.
pub type Ask<'a> = &'a (dyn Fn(&str, &Value) -> Option<(Value, Option<f64>)> + Sync);

/// The angles, in the order facets are stored.
pub const ANGLES: [&str; 6] = ["concept", "symptom", "action", "consequence", "german", "aliases"];

/// At most this many facets per lesson, each at most `FACET_CHARS` characters. Guard: bounds what a
/// model answer adds to a lesson (and to the search index) however much it writes; the prompt asks
/// for 6 to 8, so a good answer is never cut.
pub const MAX_FACETS: usize = 10;
/// Guard: one facet is a phrase, not a paragraph; a longer one is cut.
pub const FACET_CHARS: usize = 300;
/// A model answer with fewer usable facets is no answer (retried by the next run). Guard: a refusal
/// or a truncated answer is not stored as the lesson's facets.
const MIN_FACETS: usize = 3;

/// The cost estimate of a dry run, USD per lesson. Learned (measured once): the mean on the eval
/// stores, $0.0112 (internal evaluation "facets", not published), rounded up. Display only: what
/// `borg facets` without `--apply` says a run would cost; nothing is capped by it.
pub const EST_USD_PER_LESSON: f64 = 0.012;

const PROMPT: &str = "You index one lesson for keyword search (BM25). Coding agents will be matched against it with what \
they are working on: the engineer's prompt, file paths, shell commands, their own plan and final message, in English or \
German. The lesson's own wording is only one way to say it. Write facets: short alternative phrasings of the SAME lesson, \
each from a different angle, so the search finds it whichever words the agent uses.

Write 6 to 8 facets, each one line of at most 25 words:
- concept: the underlying concern in plain words, WITHOUT any project, system, service, class, file or tool name;
- symptom: what an agent would observe when this goes wrong (an error, a wrong number, a failing check, a review \
remark), in the words of logs, tests or reviews;
- action: what the agent is about to do when the lesson applies (the edit, task or plan step in progress), as an agent \
would describe it in its plan;
- consequence: what goes wrong, or what is required instead, in other words than the lesson's;
- german: the question in natural German, as a German-speaking engineer would type it, keeping technical terms and names;
- aliases: every identifier the lesson names (systems, services, repos, classes, files, metrics, flags), each with its \
likely other spellings (camelCase, kebab-case, snake_case, abbreviation, full name), space-separated; skip this facet \
if the lesson names none;
- at most two more, of any angle, where the lesson covers a second distinct situation.

Use natural words the lesson does not already use. Never add facts, names, numbers or advice that are not in the lesson.";

fn schema() -> Value {
    json!({"type": "object", "additionalProperties": false, "properties": {
        "facets": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {
            "angle": {"type": "string", "enum": ANGLES}, "text": {"type": "string"}}, "required": ["angle", "text"]}}},
        "required": ["facets"]})
}

/// The prompt for one lesson: what it says, nothing about where it is stored.
pub fn prompt(l: &Lesson) -> String {
    let mut s = format!("{PROMPT}\n\n## Lesson\nquestion: {}\n", l.question.trim());
    if !l.situations.is_empty() {
        s.push_str(&format!("situations: {}\n", l.situations.join(" | ")));
    }
    if !l.consequence.trim().is_empty() {
        s.push_str(&format!("consequence: {}\n", l.consequence.trim()));
    }
    if !l.scope.is_empty() {
        s.push_str(&format!("applies to: {}\n", l.scope.join(", ")));
    }
    if !l.overrides.trim().is_empty() {
        s.push_str(&format!("replaces the outside rule: {}\n", l.overrides.trim()));
    }
    s
}

/// The facets in a model answer: trimmed, clipped, unique, in `ANGLES` order (stable within an angle),
/// at most `MAX_FACETS`. None when fewer than `MIN_FACETS` remain.
pub fn parse(v: &Value) -> Option<Vec<String>> {
    let mut rows: Vec<(usize, String)> = Vec::new();
    for f in v["facets"].as_array()? {
        let angle = f["angle"].as_str().unwrap_or_default();
        let at = ANGLES.iter().position(|a| *a == angle).unwrap_or(ANGLES.len());
        let text: String = f["text"].as_str().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
        let text: String = text.chars().take(FACET_CHARS).collect();
        if !text.is_empty() && !rows.iter().any(|(_, t)| t.eq_ignore_ascii_case(&text)) {
            rows.push((at, text));
        }
    }
    rows.sort_by_key(|(at, _)| *at);
    let out: Vec<String> = rows.into_iter().map(|(_, t)| t).take(MAX_FACETS).collect();
    (out.len() >= MIN_FACETS).then_some(out)
}

/// One model call: the lesson's facets and the call's cost. None: no usable answer.
pub fn generate(l: &Lesson, ask: Ask) -> Option<(Vec<String>, Option<f64>)> {
    let (v, cost) = ask(&prompt(l), &schema())?;
    Some((parse(&v)?, cost))
}

/// One line of `facets.jsonl` beside the store: a lesson's facets were written, at what cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Generated {
    pub id: String,
    pub version: u32,
    pub facets: usize,
    pub cost_usd: Option<f64>,
    pub ms: i64,
}

/// What `fill` did or (dry run) would do.
#[derive(Debug, Default)]
pub struct Report {
    /// Lessons (latest versions) without facets, among those asked for.
    pub missing: usize,
    pub written: usize,
    pub failed: Vec<String>,
    /// Lessons that changed in the store while their facets were generated: not written, a later run retries.
    pub changed: usize,
    pub cost_usd: f64,
}

/// Ids of the lessons on lines appended to `path` after byte `from` (`len_of` before a learn run).
pub fn appended_ids(path: &Path, from: u64) -> HashSet<String> {
    let text = std::fs::read(path).unwrap_or_default();
    let tail = text.get(from as usize..).unwrap_or_default();
    String::from_utf8_lossy(tail).lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter_map(|v| v["id"].as_str().map(str::to_string)).collect()
}

/// The store's length in bytes (0 if absent).
pub fn len_of(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// True only with BORG_FACETS=on: `borg learn` and `borg assimilate` then write facets (internal evaluation "facets", not published: off by default).
pub fn enabled() -> bool {
    std::env::var("BORG_FACETS").as_deref() == Ok("on")
}

/// Give facets to the lessons of the store at `path` that have none (latest version), limited to
/// `only` when given. A dry run (`apply` false) asks nothing and writes nothing. With `apply`, `jobs`
/// calls at a time; each lesson with facets is appended as a new line of the same id and version
/// (readers keep the latest line), its question, consequence and every other field unchanged, unless the
/// lesson changed in the store meanwhile. After 3 failures without a success the run stops asking.
pub fn fill(path: &Path, only: Option<&HashSet<String>>, apply: bool, ask: Ask, jobs: usize, now: i64) -> Report {
    let todo: Vec<Lesson> = learn::lessons(path).into_iter().filter(|l| l.facets.is_empty() && only.is_none_or(|o| o.contains(&l.id))).collect();
    let mut report = Report { missing: todo.len(), ..Default::default() };
    if !apply || todo.is_empty() {
        return report;
    }
    let next = AtomicUsize::new(0);
    let (failures, successes) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let out: Mutex<Report> = Mutex::new(report);
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1).min(todo.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(l) = todo.get(i) else { break };
                    if failures.load(Ordering::SeqCst) >= crate::runner::MAX_FAILURES && successes.load(Ordering::SeqCst) == 0 {
                        out.lock().unwrap().failed.push(l.id.clone());
                        continue;
                    }
                    let got = generate(l, ask);
                    let mut r = out.lock().unwrap();
                    let Some((facets, cost)) = got else {
                        failures.fetch_add(1, Ordering::SeqCst);
                        r.failed.push(l.id.clone());
                        continue;
                    };
                    successes.fetch_add(1, Ordering::SeqCst);
                    r.cost_usd += cost.unwrap_or(0.0);
                    // Written under the lock, against the store as it is now: a lesson merged or replaced
                    // meanwhile keeps its new version, and its facets wait for the next run.
                    let now_latest = learn::lessons(path).into_iter().find(|x| x.id == l.id);
                    if now_latest.as_ref().is_none_or(|x| x.version != l.version || x.question != l.question || !x.facets.is_empty()) {
                        r.changed += 1;
                        continue;
                    }
                    let with = Lesson { facets, ..l.clone() };
                    let rec = Generated { id: l.id.clone(), version: l.version, facets: with.facets.len(), cost_usd: cost, ms: now };
                    if append(path, &with).and_then(|_| append(&beside(path), &rec)).is_ok() {
                        r.written += 1;
                    } else {
                        r.failed.push(l.id.clone());
                    }
                }
            });
        }
    });
    report = out.into_inner().unwrap();
    report.cost_usd += 0.0; // an empty f64 sum is -0.0
    report
}

/// `facets.jsonl` beside a lessons file.
pub fn beside(lessons: &Path) -> PathBuf {
    lessons.with_file_name("facets.jsonl")
}

fn append<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{}", serde_json::to_string(v).map_err(std::io::Error::other)?)
}

pub const USAGE: &str = "usage: borg facets [--apply] [--store LESSONS.jsonl] [--jobs N]";

/// `borg facets [--apply] [--store PATH] [--jobs N]`: backfill facets for a store (default: this
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
                eprintln!("borg facets: unknown argument {other}\n{USAGE}");
                return 2;
            }
        }
    }
    if !store.is_file() {
        eprintln!("borg facets: no lessons at {}", store.display());
        return 2;
    }
    let total = learn::lessons(&store).len();
    let r = fill(&store, None, apply, ask, jobs, learn::now_ms());
    if !apply {
        println!(
            "{}: {total} lessons, {} without facets; generating them is {} model calls, about ${:.2} (~${EST_USD_PER_LESSON:.2} per lesson). Dry run: nothing written; --apply to generate.",
            store.display(),
            r.missing,
            r.missing,
            r.missing as f64 * EST_USD_PER_LESSON
        );
        return 0;
    }
    println!("{}: facets written for {} of {} lessons without them, ${:.4}", store.display(), r.written, r.missing, r.cost_usd);
    if r.changed > 0 {
        println!("{} lessons changed while their facets were generated: not written, re-run to retry", r.changed);
    }
    if !r.failed.is_empty() {
        eprintln!("no usable answer from the model for {} lessons (re-run to retry): {}", r.failed.len(), r.failed.join(", "));
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str, lines: &[Lesson]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-facets-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("lessons.jsonl");
        for l in lines {
            append(&p, l).unwrap();
        }
        p
    }

    fn lesson(id: &str, q: &str) -> Lesson {
        Lesson { id: id.into(), origin_ms: 1, question: q.into(), consequence: "c".into(), version: 1, area: "other".into(), ..Default::default() }
    }

    fn answer(n: usize) -> Value {
        let angles = ["german", "concept", "aliases", "symptom", "action", "consequence", "concept", "symptom", "action", "concept", "symptom"];
        json!({"facets": (0..n).map(|i| json!({"angle": angles[i], "text": format!("  facet   {i} {}", angles[i])})).collect::<Vec<_>>()})
    }

    #[test]
    fn parse_orders_by_angle_cleans_and_needs_three() {
        let f = parse(&answer(7)).unwrap();
        assert_eq!(f[0], "facet 1 concept");
        assert_eq!(f.iter().position(|x| x.ends_with("german")), Some(5));
        assert_eq!(f.last().unwrap(), "facet 2 aliases");
        assert_eq!(parse(&answer(11)).unwrap().len(), MAX_FACETS);
        assert!(parse(&answer(2)).is_none());
        assert!(parse(&json!({"error": "rate limited"})).is_none());
        let dup = json!({"facets": [{"angle": "concept", "text": "same"}, {"angle": "symptom", "text": "SAME"}, {"angle": "action", "text": ""}, {"angle": "german", "text": "x"}]});
        assert!(parse(&dup).is_none(), "duplicates and empties do not count");
        let long = json!({"facets": [{"angle": "concept", "text": "w ".repeat(400)}, {"angle": "symptom", "text": "a"}, {"angle": "action", "text": "b"}]});
        assert!(parse(&long).unwrap()[0].chars().count() <= FACET_CHARS);
    }

    #[test]
    fn the_prompt_carries_the_lesson_not_its_store() {
        let l = Lesson {
            situations: vec!["on the Beacon replay".into()],
            scope: vec!["Beacon".into()],
            repo: "secret-repo".into(),
            ..lesson("L1", "Is fill@8 reported?")
        };
        let p = prompt(&l);
        assert!(p.contains("question: Is fill@8 reported?") && p.contains("situations: on the Beacon replay") && p.contains("applies to: Beacon"));
        assert!(!p.contains("secret-repo") && !p.contains("L1"));
    }

    #[test]
    fn dry_run_asks_nothing_apply_appends_facets_and_changes_nothing_else() {
        let mut has = lesson("L2", "Already has facets?");
        has.facets = vec!["x".into(), "y".into(), "z".into()];
        let p = store("fill", &[lesson("L1", "Is the flag guarded?"), has.clone()]);
        let calls = AtomicUsize::new(0);
        let ask = |_: &str, _: &Value| -> Option<(Value, Option<f64>)> {
            calls.fetch_add(1, Ordering::SeqCst);
            Some((answer(6), Some(0.01)))
        };
        let before = std::fs::read_to_string(&p).unwrap();
        let r = fill(&p, None, false, &ask, 2, 5);
        assert_eq!((r.missing, r.written, calls.load(Ordering::SeqCst)), (1, 0, 0));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before, "a dry run writes nothing");
        let r = fill(&p, None, true, &ask, 2, 5);
        assert_eq!((r.missing, r.written, calls.load(Ordering::SeqCst)), (1, 1, 1));
        assert!((r.cost_usd - 0.01).abs() < 1e-12);
        let all = learn::lessons(&p);
        let l1 = all.iter().find(|l| l.id == "L1").unwrap();
        assert_eq!(l1.facets.len(), 6);
        let orig = lesson("L1", "Is the flag guarded?");
        assert_eq!(serde_json::to_value(Lesson { facets: vec![], ..l1.clone() }).unwrap(), serde_json::to_value(&orig).unwrap(), "only facets change");
        // The conscience reads the latest line, with its facets.
        let cs = crate::conscience::load_unweighted(&p, None);
        assert_eq!(cs.iter().find(|c| c.id == "L1").unwrap().facets.len(), 6);
        let rec: Vec<Generated> = std::fs::read_to_string(beside(&p)).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!((rec.len(), rec[0].id.as_str(), rec[0].cost_usd), (1, "L1", Some(0.01)));
        // Nothing left: a second run asks nothing.
        let r = fill(&p, None, true, &ask, 2, 6);
        assert_eq!((r.missing, calls.load(Ordering::SeqCst)), (0, 1));
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn only_the_given_lessons_and_failures_stop_early() {
        let ls: Vec<Lesson> = (0..6).map(|i| lesson(&format!("L{i}"), &format!("question {i}"))).collect();
        let p = store("only", &ls);
        let from = len_of(&p);
        append(&p, &lesson("L9", "appended later")).unwrap();
        assert_eq!(appended_ids(&p, from), HashSet::from(["L9".to_string()]));
        let calls = AtomicUsize::new(0);
        let down = |_: &str, _: &Value| -> Option<(Value, Option<f64>)> {
            calls.fetch_add(1, Ordering::SeqCst);
            None
        };
        let r = fill(&p, None, true, &down, 1, 1);
        assert_eq!((r.written, r.failed.len(), calls.load(Ordering::SeqCst)), (0, 7, 3), "the model is down: stop after 3");
        let ok = |_: &str, _: &Value| Some((answer(6), None));
        let r = fill(&p, Some(&appended_ids(&p, from)), true, &ok, 1, 1);
        assert_eq!((r.missing, r.written), (1, 1));
        assert!(learn::lessons(&p).iter().filter(|l| !l.facets.is_empty()).map(|l| l.id.as_str()).eq(["L9"]));
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
