//! Engineering concepts (spike, behind `BORG_CONCEPTS=on`, internal evaluation "concepts", not published): what a lesson and the
//! current work are about in engineering terms (lock, atomics, retry/backoff, reset arc, schema
//! migration ...), from a small hierarchical taxonomy in data/concepts.json, by rules alone (0 tokens).
//!
//! - A lesson's concepts come from its question, situations, consequence, overrides and scope.
//! - The work's concepts come from the code the drone wrote (the `.edits` file: path and new text per
//!   edit), the paths it touched (file names split into words: `RetryPolicy.java` says retry) and the
//!   engineer's latest prompt. Read files' content never reaches a hook.
//!
//! Concepts only boost, never filter: a lesson's score is multiplied by `1 + beta × overlap`, where a
//! shared leaf counts its rarity over the pool (`ln(1 + n/df) / ln(1 + n)`) and a shared parent without a
//! shared leaf under it counts half its rarity. A lesson or work without concepts ranks as today.

use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

use serde_json::Value;

use crate::conscience::{Challenge, segments};

pub struct Leaf {
    pub parent: String,
    pub name: String,
    words: Vec<String>,
    code: Vec<String>,
}

pub fn taxonomy() -> &'static [Leaf] {
    static T: OnceLock<Vec<Leaf>> = OnceLock::new();
    T.get_or_init(|| {
        let v: Value = serde_json::from_str(include_str!("../data/concepts.json")).expect("data/concepts.json");
        let list = |x: &Value, lower: bool| -> Vec<String> {
            x.as_array()
                .map(|a| a.iter().filter_map(|s| s.as_str().map(|s| if lower { s.to_lowercase() } else { s.to_string() })).collect())
                .unwrap_or_default()
        };
        let mut out = Vec::new();
        for (parent, leaves) in v.as_object().into_iter().flatten().filter(|(k, _)| !k.starts_with('_')) {
            for (name, l) in leaves.as_object().into_iter().flatten() {
                out.push(Leaf { parent: parent.clone(), name: name.clone(), words: list(&l["words"], true), code: list(&l["code"], false) });
            }
        }
        out
    })
}

/// A leaf as `parent/leaf`.
pub fn label(i: usize) -> String {
    let l = &taxonomy()[i];
    format!("{}/{}", l.parent, l.name)
}

/// Whether `kw` occurs in `text` at a word start (when `kw` starts with an alphanumeric character), as a
/// prefix, or as the whole word when `kw` ends in '='.
pub fn hit(text: &str, kw: &str) -> bool {
    let (kw, whole) = match kw.strip_suffix('=') {
        Some(k) => (k, true),
        None => (kw, false),
    };
    if kw.is_empty() {
        return false;
    }
    let starts_word = kw.chars().next().is_some_and(char::is_alphanumeric);
    let ends_word = kw.chars().last().is_some_and(char::is_alphanumeric);
    text.match_indices(kw).any(|(i, _)| {
        let before_ok = !starts_word || text[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let after_ok = !whole || !ends_word || text[i + kw.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric());
        before_ok && after_ok
    })
}

/// The leaves a text names: its words (lowercase, plus identifiers split into words) against the leaves'
/// keywords, its original spelling against their API identifiers.
pub fn of_text(text: &str) -> BTreeSet<usize> {
    if text.trim().is_empty() {
        return BTreeSet::new();
    }
    let mut lower = text.to_lowercase();
    lower.push('\n');
    lower.push_str(&segments(text).join(" "));
    taxonomy().iter().enumerate().filter(|(_, l)| l.words.iter().any(|w| hit(&lower, w)) || l.code.iter().any(|c| hit(text, c))).map(|(i, _)| i).collect()
}

/// A lesson's concepts.
pub fn of_lesson(c: &Challenge) -> BTreeSet<usize> {
    of_text(&format!("{}\n{}\n{}\n{}\n{}", c.question, c.situations.join("\n"), c.consequence, c.overrides, c.scope.join(" ")))
}

/// The work's concepts: the `.edits` file (code written; Borg's own `#borg` lines skipped), the touched
/// paths (`.where`) and the latest prompt.
pub fn of_work(edits: &str, where_text: &str, prompt: &str) -> BTreeSet<usize> {
    let code: String = edits.lines().filter(|l| !l.starts_with("#borg")).collect::<Vec<_>>().join("\n");
    of_text(&format!("{code}\n{where_text}\n{prompt}"))
}

/// Concept frequencies over a pool's lessons (leaves, and parents as `parent/`), with the pool size.
pub struct Rarity {
    df: HashMap<String, usize>,
    n: usize,
}

impl Rarity {
    pub fn of(pool: &[BTreeSet<usize>]) -> Self {
        let mut df: HashMap<String, usize> = HashMap::new();
        for set in pool {
            let mut parents = BTreeSet::new();
            for &i in set {
                *df.entry(label(i)).or_default() += 1;
                parents.insert(taxonomy()[i].parent.clone());
            }
            for p in parents {
                *df.entry(format!("{p}/")).or_default() += 1;
            }
        }
        Rarity { df, n: pool.len() }
    }

    fn weight(&self, key: &str) -> f64 {
        let n = self.n.max(1) as f64;
        let df = self.df.get(key).copied().unwrap_or(1).max(1) as f64;
        (1.0 + n / df).ln() / (1.0 + n).ln().max(f64::MIN_POSITIVE)
    }
}

/// The overlap of a lesson's and the work's concepts: rarity of each shared leaf, plus half the rarity of
/// each shared parent with no shared leaf under it.
pub fn overlap(lesson: &BTreeSet<usize>, work: &BTreeSet<usize>, rarity: &Rarity) -> f64 {
    let t = taxonomy();
    let shared: Vec<usize> = lesson.intersection(work).copied().collect();
    let mut score: f64 = shared.iter().map(|&i| rarity.weight(&label(i))).sum();
    let parents = |s: &BTreeSet<usize>| s.iter().map(|&i| t[i].parent.as_str()).collect::<BTreeSet<&str>>();
    let covered: BTreeSet<&str> = shared.iter().map(|&i| t[i].parent.as_str()).collect();
    for p in parents(lesson).intersection(&parents(work)).filter(|p| !covered.contains(*p)) {
        score += 0.5 * rarity.weight(&format!("{p}/"));
    }
    score
}

/// `BORG_CONCEPTS=on` turns the concept boost on (spike: default off).
pub fn on() -> bool {
    std::env::var("BORG_CONCEPTS").is_ok_and(|v| v.trim().eq_ignore_ascii_case("on"))
}

/// The boost's strength (`BORG_CONCEPTS_BETA`, default `BETA`). Baseline: the spike's untuned
/// starting point (half a lesson's score per fully shared concept); an experiment parameter while
/// the boost is off by default, for the eval that would decide it.
pub const BETA: f64 = 0.5;

pub fn beta() -> f64 {
    std::env::var("BORG_CONCEPTS_BETA").ok().and_then(|v| v.parse().ok()).unwrap_or(BETA)
}

/// Multiplies each lesson's weight by `1 + beta × overlap` with the work's concepts.
pub fn apply(challenges: &mut [Challenge], work: &BTreeSet<usize>, beta: f64) {
    if work.is_empty() {
        return;
    }
    let sets: Vec<BTreeSet<usize>> = challenges.iter().map(of_lesson).collect();
    let rarity = Rarity::of(&sets);
    for (c, s) in challenges.iter_mut().zip(&sets) {
        c.weight *= 1.0 + beta * overlap(s, work, &rarity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(s: &BTreeSet<usize>) -> Vec<String> {
        s.iter().map(|&i| label(i)).collect()
    }

    #[test]
    fn the_taxonomy_stays_small_and_hierarchical() {
        let t = taxonomy();
        let parents: BTreeSet<&str> = t.iter().map(|l| l.parent.as_str()).collect();
        assert!((40..=60).contains(&t.len()), "{} leaves", t.len());
        assert!((8..=14).contains(&parents.len()), "{} parents", parents.len());
    }

    #[test]
    fn code_and_words_in_two_languages_name_concepts() {
        let java = names(&of_text("private final AtomicBoolean closed = new AtomicBoolean(); synchronized (sendLock) { socket.sendText(json, true).get(); }"));
        assert!(java.contains(&"concurrency/atomics".into()) && java.contains(&"concurrency/lock".into()), "{java:?}");
        let rust = names(&of_text("let (tx, rx) = mpsc::channel(); tokio::spawn(async move { tx.send(1) });"));
        assert!(rust.contains(&"messaging/channel-queue".into()) && rust.contains(&"concurrency/task-spawn".into()), "{rust:?}");
        let de = names(&of_text("Die Stelle wird mit einer Reset-Kante geleert, das ist eine Wettlaufsituation"));
        assert!(de.contains(&"petri-net/place-token".into()) && de.contains(&"concurrency/race".into()), "{de:?}");
        let path = names(&of_text("src/main/java/RetryPolicy.java"));
        assert!(path.contains(&"algorithm/retry-backoff".into()), "{path:?}");
        // Case matters for API identifiers: a Petri arc is not Rust's Arc.
        assert!(!names(&of_text("an arc of the net")).contains(&"concurrency/visibility".into()));
        assert!(names(&of_text("Arc<Mutex<State>>")).contains(&"concurrency/visibility".into()));
    }

    #[test]
    fn overlap_counts_leaves_and_half_parents_by_rarity() {
        let idx = |name: &str| taxonomy().iter().position(|l| format!("{}/{}", l.parent, l.name) == name).unwrap();
        let (lock, atomics, timeout) = (idx("concurrency/lock"), idx("concurrency/atomics"), idx("reliability/timeout"));
        let pool = vec![BTreeSet::from([lock]), BTreeSet::from([atomics]), BTreeSet::from([timeout])];
        let r = Rarity::of(&pool);
        let leaf = overlap(&BTreeSet::from([lock]), &BTreeSet::from([lock]), &r);
        let parent = overlap(&BTreeSet::from([atomics]), &BTreeSet::from([lock]), &r);
        assert!(leaf > 0.0 && parent > 0.0 && parent < leaf, "{leaf} {parent}");
        assert_eq!(overlap(&BTreeSet::from([timeout]), &BTreeSet::from([lock]), &r), 0.0);
        assert_eq!(overlap(&BTreeSet::new(), &BTreeSet::from([lock]), &r), 0.0);
    }

    #[test]
    fn untagged_work_or_lessons_rank_as_today() {
        let mut pool = vec![
            Challenge { id: "a".into(), question: "Is the ReentrantLock held?".into(), ..Default::default() },
            Challenge { id: "b".into(), question: "Is the spec cited?".into(), ..Default::default() },
        ];
        apply(&mut pool, &BTreeSet::new(), 0.5);
        assert!(pool.iter().all(|c| c.weight == 1.0));
        apply(&mut pool, &of_text("synchronized (lock) {}"), 0.5);
        assert!(pool[0].weight > 1.0 && pool[1].weight == 1.0, "{} {}", pool[0].weight, pool[1].weight);
    }
}
