//! Lesson retrieval arms for the internal evaluation "hybrid-search" (not published; eval only; not used by the borg binary).
//!
//! Arms A and B (borg's own BM25) live here and need no extra dependency. The SeekStorm arms (C, D, E)
//! are in `seek` behind the `seekstorm` feature, so the default workspace build stays as it was.

use std::collections::HashMap;
use std::path::Path;

use borg::collective::Aliases;
use borg::conscience;
use serde::Deserialize;

#[cfg(feature = "seekstorm")]
pub mod seek;

/// A lesson as the retrieval arms see it. Same selection as `conscience::load_unweighted_with`:
/// latest version per id, only lessons learned before the cut-off, in file order.
#[derive(Debug, Clone, Deserialize)]
pub struct Lesson {
    pub id: String,
    pub origin_ms: i64,
    pub question: String,
    #[serde(default)]
    pub situations: Vec<String>,
    #[serde(default)]
    pub consequence: String,
    #[serde(default)]
    pub scope: Vec<String>,
    /// Search facets (internal evaluation "facets", not published); empty in stores without them.
    #[serde(default)]
    pub facets: Vec<String>,
}

impl Lesson {
    /// The fields arm B indexes, in one text: question + situations + consequence + scope.
    pub fn all_fields(&self) -> String {
        format!("{}\n{}\n{}\n{}", self.question, self.situations.join("\n"), self.consequence, self.scope.join(" "))
    }
}

/// `before_ms` = 0 keeps every lesson.
pub fn load(path: &Path, before_ms: i64) -> Vec<Lesson> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut out: Vec<Lesson> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for l in text.lines().filter_map(|l| serde_json::from_str::<Lesson>(l).ok()) {
        if before_ms > 0 && l.origin_ms >= before_ms {
            continue;
        }
        match at.get(&l.id) {
            Some(&i) => out[i] = l,
            None => {
                at.insert(l.id.clone(), out.len());
                out.push(l);
            }
        }
    }
    out
}

/// Arm A: the product's ranking (`conscience::rank`, BM25 over the question), without outcomes, trust or
/// the machine's aliases. Every lesson with score > 0, best first.
pub fn rank_a(path: &Path, before_ms: i64, query: &str) -> Vec<(String, f64)> {
    let before = (before_ms > 0).then_some(before_ms);
    let challenges = conscience::load_unweighted_with(path, before, &Aliases::default());
    conscience::rank(&challenges, query).into_iter().map(|(s, c)| (c.id.clone(), s)).collect()
}

/// Arm B: the same `conscience::bm25` over question + situations + consequence + scope. Ties broken
/// like `rank` (newer lesson first); score 0 is not retrieved.
pub fn rank_b(lessons: &[Lesson], query: &str) -> Vec<(String, f64)> {
    let docs: Vec<Vec<String>> = lessons.iter().map(|l| conscience::tokens(&l.all_fields())).collect();
    let mut scored: Vec<(f64, &Lesson)> = conscience::bm25(&docs, query).into_iter().zip(lessons).filter(|(s, _)| *s > 0.0).collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.origin_ms.cmp(&a.1.origin_ms)));
    scored.into_iter().map(|(s, l)| (l.id.clone(), s)).collect()
}

/// The facet arms of the internal evaluation "facets" (not published): each lesson a group of documents, scored by
/// `conscience::bm25_groups` (best document + `second` × second best, idf over lessons), no scope rule,
/// ties newer first. `F`: question + facets; `FB`: arm B's all-fields text + facets.
pub fn rank_groups(lessons: &[Lesson], all_fields: bool, second: f64, query: &str) -> Vec<(String, f64)> {
    let groups: Vec<Vec<Vec<String>>> = lessons
        .iter()
        .map(|l| {
            let base = if all_fields { l.all_fields() } else { l.question.clone() };
            std::iter::once(conscience::tokens(&base)).chain(l.facets.iter().map(|f| conscience::tokens(f)).filter(|d| !d.is_empty())).collect()
        })
        .collect();
    sorted(lessons, conscience::bm25_groups(&groups, query, second))
}

/// Facet arm FD: as F, but every document counts in the document frequency (plain `bm25` over all
/// documents), lesson = its best document.
pub fn rank_doc_idf(lessons: &[Lesson], query: &str) -> Vec<(String, f64)> {
    let mut owner = Vec::new();
    let mut docs = Vec::new();
    for (i, l) in lessons.iter().enumerate() {
        for (j, t) in std::iter::once(&l.question).chain(&l.facets).enumerate() {
            let d = conscience::tokens(t);
            if j == 0 || !d.is_empty() {
                owner.push(i);
                docs.push(d);
            }
        }
    }
    let mut best = vec![0.0f64; lessons.len()];
    for (s, i) in conscience::bm25(&docs, query).into_iter().zip(owner) {
        best[i] = best[i].max(s);
    }
    sorted(lessons, best)
}

fn sorted(lessons: &[Lesson], scores: Vec<f64>) -> Vec<(String, f64)> {
    let mut scored: Vec<(f64, &Lesson)> = scores.into_iter().zip(lessons).filter(|(s, _)| *s > 0.0).collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.origin_ms.cmp(&a.1.origin_ms)));
    scored.into_iter().map(|(s, l)| (l.id.clone(), s)).collect()
}

/// The product's ranking (`conscience::rank`: scope rule, supersession; no weights) and gate score
/// (`prefilter::score`) on a store, with its facets (`facets` true) or with them removed.
pub fn product(path: &Path, before_ms: i64, facets: bool, query: &str) -> (Vec<(String, f64)>, f64) {
    let before = (before_ms > 0).then_some(before_ms);
    let mut challenges = conscience::load_unweighted_with(path, before, &Aliases::default());
    if !facets {
        challenges.iter_mut().for_each(|c| c.facets.clear());
    }
    let ranked = conscience::rank(&challenges, query).into_iter().map(|(s, c)| (c.id.clone(), s)).collect();
    (ranked, borg::prefilter::score(&challenges, query))
}

/// A query row of queries.jsonl / german.jsonl (only what the arms need).
#[derive(Debug, Clone, Deserialize)]
pub struct Query {
    pub id: String,
    pub query: String,
    pub store: String,
    pub before_ms: i64,
}

pub fn read_queries(path: &Path) -> Vec<Query> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Query text for SeekStorm's lexical parser: alphanumeric words only, each once, in order. SeekStorm reads
/// `+ - "` as operators, and a Stop context is full of shell commands.
pub fn plain_words(text: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).filter(|w| seen.insert(w.clone())).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_words_drops_operators_and_repeats() {
        assert_eq!(plain_words("git -C \"repo\" +x git"), "git c repo x");
    }

    #[test]
    fn arm_b_finds_a_word_only_in_the_consequence() {
        let l = |id: &str, q: &str, c: &str| Lesson {
            id: id.into(),
            origin_ms: 0,
            question: q.into(),
            situations: vec![],
            consequence: c.into(),
            scope: vec![],
            facets: vec![],
        };
        let lessons = vec![l("a", "Is the plan tested?", "Untested reconnect logic broke"), l("b", "Is the plan tested?", "nothing")];
        let r = rank_b(&lessons, "reconnect");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, "a");
    }
}
