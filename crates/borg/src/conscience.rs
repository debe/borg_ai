//! The conscience: challenges learned from engineers' corrections, asked at
//! decision points when what the drone is working on matches.
//!
//! A challenge is a question, never an answer. It is recalled only when it
//! matches the drone's prompt, touches and plan (BM25 over the question text),
//! so a drone working on something unrelated pays zero tokens.

use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;

use crate::collective::{self, Aliases};
use crate::trust;

#[derive(Debug, Clone, Deserialize)]
pub struct Challenge {
    pub id: String,
    /// Epoch ms of the correction this challenge was learned from.
    pub origin_ms: i64,
    pub question: String,
    /// World-model form of the lesson (see predict.rs); empty for question-only lessons.
    #[serde(default)]
    pub situations: Vec<String>,
    #[serde(default)]
    pub consequence: String,
    /// Kind of concern (trust.rs `AREAS`); "other" for lessons learned before areas existed.
    #[serde(default)]
    pub area: String,
    /// Engineers whose corrections the lesson was learned from (git user.email).
    #[serde(default)]
    pub engineers: Vec<String>,
    /// When each later engineer joined the lesson (learn.rs `Lesson::joined`).
    #[serde(default)]
    pub joined: std::collections::BTreeMap<String, i64>,
    /// Lessons this one replaces or contradicts (set by `borg learn` or a queen answer).
    #[serde(default)]
    pub supersedes: Vec<String>,
    /// "queen" for a queen answer (queen.rs `lesson`), empty otherwise.
    #[serde(default)]
    pub source: String,
    /// Names the lesson is limited to (learn.rs `Lesson::scope`); empty: unscoped.
    #[serde(default)]
    pub scope: Vec<String>,
    /// The outside rule this lesson replaces (learn.rs `Lesson::overrides`), shown with the question.
    #[serde(default)]
    pub overrides: String,
    /// Alternative phrasings for search (learn.rs `Lesson::facets`): each is its own BM25 document of
    /// this lesson (`rank`), never shown.
    #[serde(default)]
    pub facets: Vec<String>,
    /// The repo the lesson was learned in (for an import: where the knowledge was written down).
    #[serde(default)]
    pub repo: String,
    /// Who should hear it (reach.rs): empty for the team, `repo`, or `personal` (a withdrawn tombstone).
    #[serde(default)]
    pub reach: String,
    /// Scoped to systems none of which is the work's known system (systems.rs `gate`): scores 0.
    #[serde(skip)]
    pub off_system: bool,
    /// Memory weight (memory.rs: activation × usefulness belief), multiplies the BM25 score; 1 for a
    /// new lesson without outcomes.
    #[serde(skip, default = "one")]
    pub weight: f64,
    /// Dormant (memory.rs): recalled only on a strong match (`rank`).
    #[serde(skip)]
    pub dormant: bool,
    /// Consolidated (memory.rs): may be delivered early without a scope (`early`).
    #[serde(skip)]
    pub consolidated: bool,
    /// This session's Thompson draw of the lesson's usefulness relative to its mean (memory.rs): orders
    /// the lessons that cleared the bar when there are more than the slots (`budget`, `early`); 1
    /// without a session.
    #[serde(skip, default = "one")]
    pub explore: f64,
    /// The usefulness belief behind `explore` (memory.rs `Memory::alpha`, `beta`): Beta(α, β), (1, 1)
    /// without outcomes. For the propensity of a slot (`propensity`).
    #[serde(skip, default = "uniform")]
    pub belief: (f64, f64),
    /// Mean trust of the lesson's authors in its area (trust.rs), 0.5 without evidence.
    #[serde(skip, default = "neutral")]
    pub trust: f64,
}

fn one() -> f64 {
    1.0
}

fn uniform() -> (f64, f64) {
    (1.0, 1.0)
}

fn neutral() -> f64 {
    trust::NEUTRAL
}

impl Default for Challenge {
    fn default() -> Self {
        Challenge {
            id: String::new(),
            origin_ms: 0,
            question: String::new(),
            situations: Vec::new(),
            consequence: String::new(),
            area: trust::OTHER.into(),
            engineers: Vec::new(),
            joined: Default::default(),
            supersedes: Vec::new(),
            source: String::new(),
            scope: Vec::new(),
            overrides: String::new(),
            facets: Vec::new(),
            repo: String::new(),
            reach: String::new(),
            off_system: false,
            weight: 1.0,
            dormant: false,
            consolidated: false,
            explore: 1.0,
            belief: uniform(),
            trust: trust::NEUTRAL,
        }
    }
}

/// Lessons known before `before_ms`. The store is append-only: when a lesson was consolidated,
/// a later line carries the same id, and the latest version wins. Each carries its weight from
/// `outcomes.jsonl` beside the lessons file and the team's synced outcomes (outcome.rs `read_team`;
/// absent, e.g. for eval challenge files: all 1) and its authors' trust in its area, computed from
/// this store alone (trust.rs). Engineers are
/// canonical under this machine's aliases (collective.rs "Engineers").
pub fn load(path: &Path, before_ms: Option<i64>) -> Vec<Challenge> {
    load_with(path, before_ms, collective::aliases())
}

/// `load` under explicit aliases.
pub fn load_with(path: &Path, before_ms: Option<i64>, aliases: &Aliases) -> Vec<Challenge> {
    load_as(path, before_ms, aliases, None)
}

/// `load` as seen from `session`: each weight takes that session's Thompson sample of the lesson's
/// usefulness (memory.rs), stable within the session. Without a session, the belief's mean.
pub fn load_for(path: &Path, before_ms: Option<i64>, session: Option<&str>) -> Vec<Challenge> {
    load_as(path, before_ms, collective::aliases(), session)
}

fn load_as(path: &Path, before_ms: Option<i64>, aliases: &Aliases, session: Option<&str>) -> Vec<Challenge> {
    let outcomes = crate::outcome::read_team(path);
    let mut out = load_unweighted_with(path, before_ms, aliases);
    let memory = crate::memory::derive(&outcomes, &out, session);
    let trust = trust::Trust::compute(&out, &outcomes);
    for c in &mut out {
        if let Some(m) = memory.get(&c.id) {
            // The bar (`RELATIVE`) is set on the expected weight; the session's draw only decides which
            // of the lessons over the bar get the limited slots, so a new lesson's uncertainty cannot drop
            // a strong match below the bar.
            c.weight = m.weight(&c.id, None);
            c.explore = if session.is_some() { m.weight(&c.id, session) / c.weight } else { 1.0 };
            c.belief = (m.alpha(), m.beta());
            c.dormant = m.dormant;
            c.consolidated = m.consolidated;
        }
        c.trust = trust.lesson(c);
    }
    out.extend(constraints(path, before_ms, aliases, &out));
    out
}

/// `source` of a ratified constraint loaded as a lesson (`constraints`).
pub const CONSTRAINT: &str = "constraint";

impl Challenge {
    /// A standing lesson: a constraint an engineer ratified (veto.rs `ratify`). Consolidated, never
    /// dormant, its weight and trust fixed (no outcome or author moves them), and delivered early
    /// whenever its scope is in play, whatever the match, on every harness that takes prompt context
    /// (`standing_pool`, hook.rs `early`).
    pub fn standing(&self) -> bool {
        self.source == CONSTRAINT
    }
}

/// The ratified constraints beside the lessons file `path` (veto.rs `constraints_path`) known before
/// `before_ms`, as standing lessons: the invariant is the question, the scope and repo are those of the
/// lesson it was proposed from (in `lessons`), its reach the narrower of the line's and the lesson's
/// (reach.rs `narrower`): a lesson withdrawn or made a repo convention takes its constraint along. With
/// the lesson unknown here (quarantined, not synced yet, after `before_ms`), the line's own scope and
/// repo, bound to that repo; a line that names no repo either is left out, never delivered everywhere.
/// One per id, the earliest ratification (by time, then author), so nodes that both ratified it read
/// the same line after a sync, in whatever order it came.
fn constraints(path: &Path, before_ms: Option<i64>, aliases: &Aliases, lessons: &[Challenge]) -> Vec<Challenge> {
    let Some(dir) = path.parent() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(crate::veto::constraints_path(dir)) else { return Vec::new() };
    let mut by: std::collections::BTreeMap<String, (i64, String, Challenge)> = std::collections::BTreeMap::new();
    for v in text.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()) {
        let s = |k: &str| v[k].as_str().unwrap_or_default().trim().to_string();
        let (id, question, ms) = (s("id"), s("text"), v["ms"].as_i64().unwrap_or(0));
        if id.is_empty() || question.is_empty() || before_ms.is_some_and(|b| ms >= b) {
            continue;
        }
        let author = aliases.canon(&s("by"));
        if by.get(&id).is_some_and(|(m, a, _)| (*m, a.as_str()) <= (ms, author.as_str())) {
            continue;
        }
        let line_scope: Vec<String> = v["scope"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).map(str::to_string).collect();
        let (scope, repo, reach, area) = match lessons.iter().find(|l| l.id == s("lesson")) {
            Some(l) => {
                let repo = if l.repo.is_empty() { s("repo") } else { l.repo.clone() };
                (l.scope.clone(), repo, crate::reach::narrower(&s("reach"), &l.reach).to_string(), l.area.clone())
            }
            None if s("repo").is_empty() => continue,
            None => (line_scope, s("repo"), crate::reach::narrower(&s("reach"), crate::reach::REPO).to_string(), trust::OTHER.into()),
        };
        let c = Challenge {
            id: id.clone(),
            origin_ms: ms,
            question,
            source: CONSTRAINT.into(),
            engineers: Some(author.clone()).filter(|a| !a.is_empty()).into_iter().collect(),
            scope,
            repo,
            reach,
            area,
            consolidated: true,
            ..Challenge::default()
        };
        by.insert(id, (ms, author, c));
    }
    by.into_values().map(|(_, _, c)| c).collect()
}

/// The lessons as stored (latest version per id, area normalised, engineers canonical), without
/// weight or trust.
pub fn load_unweighted(path: &Path, before_ms: Option<i64>) -> Vec<Challenge> {
    load_unweighted_with(path, before_ms, collective::aliases())
}

/// `load_unweighted` under explicit aliases.
pub fn load_unweighted_with(path: &Path, before_ms: Option<i64>, aliases: &Aliases) -> Vec<Challenge> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    // Index by id: a linear scan per line made loading O(n²) (29 s at 100k lessons).
    let mut out: Vec<Challenge> = Vec::new();
    let mut at: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for mut c in text.lines().filter_map(|l| serde_json::from_str::<Challenge>(l).ok()) {
        c.area = trust::area(&c.area).into();
        canonical(&mut c, aliases);
        if before_ms.is_some_and(|b| c.origin_ms >= b) {
            continue;
        }
        match at.get(&c.id) {
            Some(&i) => out[i] = c,
            None => {
                at.insert(c.id.clone(), out.len());
                out.push(c);
            }
        }
    }
    out
}

/// Map a lesson's engineers to who they are (`Aliases::canon`), at read time: the store keeps the
/// raw emails. Several emails of one engineer become one author; that author joined when the
/// earliest of them did, and not at all (counts from the start) if any of them is the first
/// contributor.
fn canonical(c: &mut Challenge, aliases: &Aliases) {
    let mut engineers: Vec<String> = Vec::new();
    for e in c.engineers.iter().map(|e| aliases.canon(e)).filter(|e| !e.is_empty()) {
        if !engineers.contains(&e) {
            engineers.push(e);
        }
    }
    let joined = engineers
        .iter()
        .filter_map(|who| {
            let raws = c.engineers.iter().filter(|r| aliases.canon(r) == *who);
            let at: Option<Vec<i64>> = raws.map(|r| c.joined.get(r).copied()).collect();
            Some((who.clone(), at?.into_iter().min()?))
        })
        .collect();
    (c.engineers, c.joined) = (engineers, joined);
}

/// Lowercased words of 3+ characters, plus the parts of camelCase words.
pub fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if raw.len() < 3 {
            continue;
        }
        out.push(raw.to_lowercase());
        let mut word = String::new();
        for ch in raw.chars() {
            if ch.is_uppercase() && !word.is_empty() {
                if word.len() >= 3 && word.len() < raw.len() {
                    out.push(word.to_lowercase());
                }
                word.clear();
            }
            word.push(ch);
        }
        if word.len() >= 3 && word.len() < raw.len() {
            out.push(word.to_lowercase());
        }
    }
    out
}

/// BM25 over the challenges' own question text, queried with the drone's
/// context (prompt, touches, its plan). Evaluated against keyword-IDF, dense
/// and reranked retrieval on graded qrels (internal evaluation "retrieval-plan-queries", not published):
/// BM25 ranked the relevant lessons highest. Each score is multiplied by the
/// challenge's outcome weight, so a lesson that keeps being asked and then
/// corrected anyway sinks below equally relevant ones, and by its authors'
/// trust in its area relative to neutral (trust / 0.5: no evidence leaves the
/// score unchanged). A lesson hidden by its successor is never ranked; the live
/// successor inherits its relevance (trust.rs `successors`), so the replacing
/// claim is asked where the replaced one would have been. A supersession that
/// does not hide (a less trusted engineer contradicting another) keeps both
/// live, and `render` shows them as a disagreement. A scoped lesson whose scope
/// the context never names scores 0 (`in_scope`): it is neither asked nor passes
/// its relevance on. Returns every challenge with score > 0, best first.
///
/// A lesson with facets (facets.rs) is several documents: its question and each facet. Its score is its
/// best document's (`bm25_groups`), with idf counted over lessons, so a store without facets ranks
/// exactly as BM25 over the questions, and the prefilter's normalisation by the largest idf stays valid.
pub fn rank<'a>(challenges: &'a [Challenge], context: &str) -> Vec<(f64, &'a Challenge)> {
    rank_scoped(challenges, context, context)
}

/// The drone's work in `context` (a session's state file plus its work text) as word matching, the
/// vectors and the consequence model read it: without borg's own bookkeeping lines (`#borg-asked`,
/// `#borg-p`, `#borg-pool`, every `#borg-` marker). They carry lesson ids, scores and costs, not work.
pub fn work_text(context: &str) -> std::borrow::Cow<'_, str> {
    if !context.contains("#borg-") {
        return std::borrow::Cow::Borrowed(context);
    }
    std::borrow::Cow::Owned(context.split_inclusive('\n').filter(|l| !l.starts_with("#borg-")).collect())
}

/// `rank`, with scopes judged against `scope_text` (where the session works) instead of the whole
/// context: a system the conversation only talks about is not one the drone works on.
pub fn rank_scoped<'a>(challenges: &'a [Challenge], context: &str, scope_text: &str) -> Vec<(f64, &'a Challenge)> {
    let groups: Vec<Vec<Vec<String>>> = challenges.iter().map(documents).collect();
    let mut raw = bm25_groups(&groups, context, 0.0);
    if challenges.iter().any(|c| !c.scope.is_empty()) {
        let have = Names::of(scope_text);
        for (r, c) in raw.iter_mut().zip(challenges) {
            if !in_scope(c, &have) || c.off_system {
                *r = 0.0;
            }
        }
    }
    // A dormant lesson (memory.rs) is recalled only on a strong match: `STRONG` × the best match.
    let best = raw.iter().copied().fold(0.0, f64::max);
    for (r, c) in raw.iter_mut().zip(challenges) {
        if c.dormant && *r < STRONG * best {
            *r = 0.0;
        }
    }
    let successors = trust::successors(challenges);
    let mut relevance = raw.clone();
    if !successors.is_empty() {
        let at: std::collections::HashMap<&str, usize> = challenges.iter().enumerate().map(|(i, c)| (c.id.as_str(), i)).collect();
        for (i, c) in challenges.iter().enumerate() {
            if let Some(&j) = successors.get(&c.id).and_then(|s| at.get(s.as_str())) {
                relevance[j] = relevance[j].max(raw[i]);
            }
        }
    }
    let mut scored: Vec<(f64, &Challenge)> = relevance
        .into_iter()
        .zip(challenges)
        .filter(|(_, c)| !successors.contains_key(&c.id))
        .map(|(s, c)| (s * c.weight * c.trust / trust::NEUTRAL, c))
        .filter(|(s, _)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.origin_ms.cmp(&a.1.origin_ms)));
    scored
}

/// A dormant lesson needs at least this share of the best match to be recalled (`rank`). Baseline:
/// design-chosen with dormancy (memory.rs `DORMANT_BELOW`), part of today's ranking; the same value as
/// `BLOCK`, a different bar.
pub const STRONG: f64 = 0.8;

/// Whether a lesson applies to a context: unscoped, or one of its scope names occurs in it as a
/// contiguous phrase or identifier (`Names::contains`). "Answer Guard" needs "answer guard" (or
/// `AnswerGuard`), never "output" in one sentence and "guard" in another; "Orion" matches a path
/// `orion-text/` and `OrionConfig`; no part of a name is ever dropped for being short.
pub fn in_scope(c: &Challenge, have: &Names) -> bool {
    c.scope.is_empty() || c.scope.iter().any(|s| have.contains(s))
}

/// Lowercased name parts in order: split at every non-alphanumeric character and at camelCase
/// humps (`HTTPClientFactory` -> http, client, factory), digits kept with their letters (`v2`, `sr3`).
pub fn segments(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()).filter(|r| !r.is_empty()) {
        let cs: Vec<char> = raw.chars().collect();
        let mut word = String::new();
        for (i, &ch) in cs.iter().enumerate() {
            let hump = i > 0
                && ch.is_uppercase()
                && (cs[i - 1].is_lowercase() || cs[i - 1].is_numeric() || (cs[i - 1].is_uppercase() && cs.get(i + 1).is_some_and(|n| n.is_lowercase())));
            if hump && !word.is_empty() {
                out.push(std::mem::take(&mut word).to_lowercase());
            }
            word.push(ch);
        }
        out.push(word.to_lowercase());
    }
    out
}

/// A text as its name parts, indexed for contiguous lookups.
pub struct Names {
    seq: Vec<String>,
    at: std::collections::HashMap<String, Vec<usize>>,
}

impl Names {
    pub fn of(text: &str) -> Self {
        Self::of_all([text])
    }

    /// Several texts, none adjacent to the next: a name never matches across two of them.
    pub fn of_all<'a>(texts: impl IntoIterator<Item = &'a str>) -> Self {
        let mut seq = Vec::new();
        for t in texts {
            seq.extend(segments(t));
            seq.push(String::new()); // a part no name has
        }
        let mut at: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
        for (i, s) in seq.iter().enumerate() {
            at.entry(s.clone()).or_default().push(i);
        }
        Names { seq, at }
    }

    /// Whether every part of `name` occurs here, in order and adjacent. A name without parts never matches.
    pub fn contains(&self, name: &str) -> bool {
        let need = segments(name);
        let Some(first) = need.first() else { return false };
        self.at.get(first).is_some_and(|starts| starts.iter().any(|&i| self.seq.get(i..i + need.len()).is_some_and(|w| w == need.as_slice())))
    }
}

/// Okapi BM25's term-frequency saturation and length normalisation (`bm25`, `bm25_groups`). Method:
/// the standard parameters (Robertson and Zaragoza, "The Probabilistic Relevance Framework: BM25 and
/// Beyond", 2009); never tuned on borg's data.
pub const BM25_K1: f64 = 1.2;
pub const BM25_B: f64 = 0.75;

/// Okapi BM25 (`BM25_K1`, `BM25_B`) of each tokenised document against the query text, in document order.
pub fn bm25(docs: &[Vec<String>], query: &str) -> Vec<f64> {
    let (k1, b) = (BM25_K1, BM25_B);
    let n = docs.len().max(1) as f64;
    let avg = docs.iter().map(Vec::len).sum::<usize>() as f64 / n;
    let mut df: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    for d in docs {
        for t in d.iter().map(String::as_str).collect::<HashSet<_>>() {
            *df.entry(t).or_default() += 1.0;
        }
    }
    let query: HashSet<String> = tokens(query).into_iter().collect();
    docs.iter()
        .map(|d| {
            let mut tf: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
            for t in d {
                *tf.entry(t.as_str()).or_default() += 1.0;
            }
            query
                .iter()
                .filter_map(|t| tf.get(t.as_str()).map(|f| (t, *f)))
                .map(|(t, f)| {
                    let dft = df[t.as_str()];
                    let idf = (1.0 + (n - dft + 0.5) / (dft + 0.5)).ln();
                    idf * f * (k1 + 1.0) / (f + k1 * (1.0 - b + b * d.len() as f64 / avg))
                })
                .sum()
        })
        .collect()
}

/// A lesson's search documents: its question, then each facet (`Challenge::facets`).
pub fn documents(c: &Challenge) -> Vec<Vec<String>> {
    std::iter::once(tokens(&c.question)).chain(c.facets.iter().map(|f| tokens(f)).filter(|d| !d.is_empty())).collect()
}

/// BM25 (`BM25_K1`, `BM25_B`) of groups of documents, one group per lesson, against the query text: a
/// group scores its best document plus `second` × its second best (0: the best alone). Document
/// frequencies count groups (a word in three facets of one lesson is one lesson), so idf means what it
/// means in `bm25`; term frequency and length are per document. Groups of one document score as `bm25`
/// does. Only query words are counted, which keeps a 1k-lesson store with facets fast.
pub fn bm25_groups(groups: &[Vec<Vec<String>>], query: &str, second: f64) -> Vec<f64> {
    let (k1, b) = (BM25_K1, BM25_B);
    let n = groups.len().max(1) as f64;
    let ndocs = groups.iter().map(Vec::len).sum::<usize>().max(1) as f64;
    let avg = groups.iter().flatten().map(Vec::len).sum::<usize>() as f64 / ndocs;
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for t in tokens(query) {
        let next = index.len();
        index.entry(t).or_insert(next);
    }
    // Per document: (query word, tf), ordered by query word, so equal documents sum in the same order.
    let counted: Vec<Vec<Vec<(usize, f64)>>> = groups
        .iter()
        .map(|g| {
            g.iter()
                .map(|d| {
                    let mut tf: Vec<(usize, f64)> = Vec::new();
                    for q in d.iter().filter_map(|t| index.get(t.as_str())) {
                        match tf.iter_mut().find(|(x, _)| x == q) {
                            Some((_, f)) => *f += 1.0,
                            None => tf.push((*q, 1.0)),
                        }
                    }
                    tf.sort_by_key(|(q, _)| *q);
                    tf
                })
                .collect()
        })
        .collect();
    let mut df = vec![0.0f64; index.len()];
    let mut seen: Vec<usize> = Vec::new();
    for g in &counted {
        seen.clear();
        for &(q, _) in g.iter().flatten() {
            if !seen.contains(&q) {
                seen.push(q);
                df[q] += 1.0;
            }
        }
    }
    let idf: Vec<f64> = df.iter().map(|&d| (1.0 + (n - d + 0.5) / (d + 0.5)).ln()).collect();
    counted
        .iter()
        .zip(groups)
        .map(|(g, docs)| {
            let (mut best, mut next) = (0.0f64, 0.0f64);
            for (tf, d) in g.iter().zip(docs) {
                let norm = k1 * (1.0 - b + b * d.len() as f64 / avg);
                let s: f64 = tf.iter().map(|&(q, f)| idf[q] * f * (k1 + 1.0) / (f + norm)).sum();
                if s > best {
                    (best, next) = (s, best);
                } else if s > next {
                    next = s;
                }
            }
            best + second * next
        })
        .collect()
}

/// The top `k` ranked challenges.
pub fn select<'a>(challenges: &'a [Challenge], context: &str, k: usize) -> Vec<&'a Challenge> {
    select_scoped(challenges, context, context, k)
}

/// `select` with scopes judged against `scope_text` (`rank_scoped`).
pub fn select_scoped<'a>(challenges: &'a [Challenge], context: &str, scope_text: &str, k: usize) -> Vec<&'a Challenge> {
    rank_scoped(challenges, context, scope_text).into_iter().take(k).map(|(_, c)| c).collect()
}

/// An early delivery takes lessons scoring at least this share of the best on-topic match (`early`).
/// Baseline: today's early rule (R3b, internal evaluation "phase-r"), kept until a learned early rule
/// is fitted on the share each early delivery's record carries per lesson (outcome.rs `rel`, hook.rs
/// `early_rel`; empty when no plain lesson was on topic). Those records hold only delivered lessons, and
/// every plain one scored at least this bar, so a rule fitted on them alone can learn to raise it, not
/// what a lower bar would admit.
pub const RELATIVE: f64 = 0.5;
/// The Stop blocks only when a lesson not yet done scores at least this share of the best match
/// (`budget`): a block is an extra round, the expensive part. Baseline: the R3b word-matching rule
/// (`BORG_MATCH=bm25`), not the default path.
pub const BLOCK: f64 = 0.8;
/// Once the Stop blocks, every lesson not done scoring at least this share of the best match is asked
/// (`budget`): more questions in a round that happens anyway cost little. Baseline: as `BLOCK`.
pub const LOW: f64 = 0.3;
/// At most this many lessons per Stop block. Baseline: the fixed graded rule's block and free hand-over
/// (`graded_capped`), the reference the learned policy is gated against (select.rs); it also bounds a
/// block's length.
pub const CAP: usize = 10;
/// At most this many unscoped (generic) lessons per block: generic hygiene does not fill the block.
/// Baseline: part of the R3b rule (`in_scope_first`).
pub const GENERIC_CAP: usize = 3;

/// The Stop's two decisions over a ranking of the whole store (best first, as `rank` returns it; the
/// top is the store's best match, done or not), with `done` the lessons this session already asked,
/// or delivered early and acted on (R3b, internal evaluation "phase-r", not published):
/// - **whether to block** (the cost: one more round): only when a lesson not done scores at least
///   `BLOCK` × the top. A context whose best lessons were delivered early and acted on does not block.
/// - **what to ask once blocking** (cheap): every lesson not done scoring at least `LOW` × the top,
///   scoped lessons first (`rank` already dropped those out of scope), at most `GENERIC_CAP` unscoped,
///   `CAP` in all, ordered by score × the session's usefulness draw (R1, `explored`).
pub fn budget<'a>(ranked: &[(f64, &'a Challenge)], done: &HashSet<&str>) -> Vec<&'a Challenge> {
    in_scope_first(explored(budget_pool(ranked, done)))
}

/// `budget`'s pool before the slots: every lesson not done over `LOW` × the top, with its score; empty
/// when none not done clears `BLOCK` × the top.
pub fn budget_pool<'a>(ranked: &[(f64, &'a Challenge)], done: &HashSet<&str>) -> Vec<(f64, &'a Challenge)> {
    let Some(&(top, _)) = ranked.first() else { return Vec::new() };
    if !ranked.iter().any(|(s, c)| *s >= BLOCK * top && !done.contains(c.id.as_str())) {
        return Vec::new();
    }
    ranked.iter().copied().filter(|(s, c)| *s >= LOW * top && !done.contains(c.id.as_str())).collect()
}

/// The consequence model decided to block (`predicted`, non-empty): ask its lessons and every other
/// lesson not done over `LOW` × the best match, in-scope first, ordered by score × draw, at most `CAP`.
pub fn widen<'a>(ranked: &[(f64, &'a Challenge)], predicted: &[&'a Challenge], done: &HashSet<&str>) -> Vec<&'a Challenge> {
    in_scope_first(explored(widen_pool(ranked, predicted, done)))
}

/// `widen`'s pool before the slots: the model's picks (at least the top score) and every other lesson
/// not done over `LOW` × the top, with their scores; empty when the model picked none.
pub fn widen_pool<'a>(ranked: &[(f64, &'a Challenge)], predicted: &[&'a Challenge], done: &HashSet<&str>) -> Vec<(f64, &'a Challenge)> {
    if predicted.is_empty() {
        return Vec::new();
    }
    let top = ranked.first().map_or(0.0, |r| r.0);
    let score = |c: &Challenge| ranked.iter().find(|(_, x)| x.id == c.id).map_or(0.0, |r| r.0);
    // The model's picks stay in even below the bar; they lead ties.
    let mut pool: Vec<(f64, &Challenge)> = predicted.iter().map(|c| (score(c).max(top), *c)).collect();
    for &(s, c) in ranked {
        if s >= LOW * top && !done.contains(c.id.as_str()) && !pool.iter().any(|(_, x)| x.id == c.id) {
            pool.push((s, c));
        }
    }
    pool
}

/// The graded Stop decision (the default, unless `BORG_STOP_GRADED=off`; internal evaluation "whole-store", "Graded follow-up", not published) over the
/// model's graded applies, in its order (3 certain, 2 likely, 1 worth a thought). Returns what to ask in
/// a block and what to hand over free:
/// - **block** (an extra round) only when at least one lesson is graded 3; then ask every 3 and 2, and
///   the 1s while the block has fewer than `CAP` lessons. 2s and 1s alone never block.
/// - **without a block**, the 2s and then the 1s, at most `CAP`, go to the drone free as early context at
///   its next prompt or touch (hook.rs `queued`).
pub fn graded<'a>(applied: &[(&'a Challenge, u8)]) -> (Vec<&'a Challenge>, Vec<&'a Challenge>) {
    graded_capped(applied, ask_cap())
}

/// `BORG_STOP_ASK_CAP=N` (evaluation only): a block asks only the 3s, the first `N` in the model's
/// order; without a 3 nothing changes. Unset, a block asks as `graded` says.
pub fn ask_cap() -> Option<usize> {
    std::env::var("BORG_STOP_ASK_CAP").ok().and_then(|v| v.trim().parse().ok()).filter(|n| *n > 0)
}

/// `graded` with an explicit cap on a block (`ask_cap`).
pub fn graded_capped<'a>(applied: &[(&'a Challenge, u8)], cap: Option<usize>) -> (Vec<&'a Challenge>, Vec<&'a Challenge>) {
    graded_without(applied, cap, &[])
}

/// `graded_capped` for a session that already had the lessons `had` delivered (ids in any order,
/// select.rs `Ctx::had`): such a lesson may still be asked, but never takes a free slot, which the
/// delivery would drop as given before (hook.rs `queued`); the next 2 or 1 takes it. With `had` empty
/// this is `graded_capped`, the rule every record before select.rs `nofree` was decided by.
pub fn graded_without<'a>(applied: &[(&'a Challenge, u8)], cap: Option<usize>, had: &[String]) -> (Vec<&'a Challenge>, Vec<&'a Challenge>) {
    let of = |g: u8| applied.iter().filter(move |(_, x)| *x == g).map(|(c, _)| *c);
    if of(3).next().is_none() {
        return (Vec::new(), of(2).chain(of(1)).filter(|c| !had.contains(&c.id)).take(CAP).collect());
    }
    if let Some(n) = cap {
        return (of(3).take(n).collect(), Vec::new());
    }
    let mut ask: Vec<&Challenge> = of(3).chain(of(2)).collect();
    let room = CAP.saturating_sub(ask.len());
    ask.extend(of(1).take(room));
    (ask, Vec::new())
}

/// Lessons that cleared the bar, in the order of score × the session's usefulness draw (`explore`):
/// Thompson sampling over the slots. Without a session every draw is 1 and this is the score order.
pub fn explored<'a>(passed: impl IntoIterator<Item = (f64, &'a Challenge)>) -> Vec<&'a Challenge> {
    let mut v: Vec<(f64, &Challenge)> = passed.into_iter().map(|(s, c)| (s * c.explore, c)).collect();
    v.sort_by(|a, b| b.0.total_cmp(&a.0));
    v.into_iter().map(|(_, c)| c).collect()
}

/// Candidates in their order, scoped ones first, at most `GENERIC_CAP` unscoped, `CAP` in all.
pub fn in_scope_first<'a>(candidates: impl IntoIterator<Item = &'a Challenge>) -> Vec<&'a Challenge> {
    let (scoped, generic): (Vec<&Challenge>, Vec<&Challenge>) = candidates.into_iter().partition(|c| !c.scope.is_empty());
    scoped.into_iter().chain(generic.into_iter().take(GENERIC_CAP)).take(CAP).collect()
}

/// Method: the draws behind a propensity estimate (`propensity`): precision, not a tuning knob
/// (standard error ≤ 1/(2√P_DRAWS) ≈ 0.022).
pub const P_DRAWS: usize = 512;

/// Each pooled lesson's chance of a slot under the session's posterior (`budget`, `widen`): the share of
/// `P_DRAWS` alternative draws in which `in_scope_first`, over the pool ordered by score × draw, keeps
/// it. A draw is the ratio `explore` is (memory.rs `Memory::weight`): (0.5 + x) / (0.5 + mean), x from
/// the lesson's belief, seeded by (`session#p<i>`, id). Exactly 1 for every lesson when the pool fits the
/// slots, without a draw. Deterministic.
pub fn propensity(pool: &[(f64, &Challenge)], session: &str) -> std::collections::HashMap<String, f64> {
    if pool.iter().filter(|(_, c)| c.scope.is_empty()).count() <= GENERIC_CAP && pool.len() <= CAP {
        return pool.iter().map(|(_, c)| (c.id.clone(), 1.0)).collect();
    }
    kept_share(pool, session, |order| in_scope_first(order).into_iter().map(|c| c.id.clone()).collect())
}

/// The share of `P_DRAWS` draws (`propensity`) in which `keep`, given the pool ordered by score × draw,
/// keeps each pooled lesson.
fn kept_share(pool: &[(f64, &Challenge)], session: &str, keep: impl Fn(Vec<&Challenge>) -> Vec<String>) -> std::collections::HashMap<String, f64> {
    let draw = |c: &Challenge, i: usize| {
        let (a, b) = c.belief;
        let x = crate::memory::draw(a, b, crate::memory::seed(&format!("{session}#p{i}"), &c.id));
        (0.5 + x) / (0.5 + a / (a + b))
    };
    let mut kept: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for i in 0..P_DRAWS {
        let mut v: Vec<(f64, &Challenge)> = pool.iter().map(|&(s, c)| (s * draw(c, i), c)).collect();
        v.sort_by(|a, b| b.0.total_cmp(&a.0));
        for id in keep(v.into_iter().map(|(_, c)| c).collect()) {
            *kept.entry(id).or_default() += 1;
        }
    }
    pool.iter().map(|(_, c)| (c.id.clone(), kept.get(&c.id).copied().unwrap_or(0) as f64 / P_DRAWS as f64)).collect()
}

/// A pooled lesson's score without its usefulness factor: s / (0.5 + mean belief) = relevance ×
/// squash(A) × trust / NEUTRAL (`rank`, memory.rs `Memory::weight`). Under another belief the score is
/// base × (0.5 + its mean), and `propensity` orders by base × (0.5 + draw), so `borg debug ope` (ope.rs) can
/// replay a slot under other weights.
pub fn pool_base(s: f64, c: &Challenge) -> f64 {
    let (a, b) = c.belief;
    s / (0.5 + a / (a + b))
}

/// At most this many lessons per early delivery (`early`). Baseline: today's early rule, as `RELATIVE`.
pub const EARLY_CAP: usize = 5;
/// An early delivery's text stops before this many characters (about 500 tokens), a free hand-over's
/// too (hook.rs `queued`). Guard: bounds the context one delivery adds to the drone's turn; what it
/// cuts is logged (`dropped_chars`, outcome.rs `DELIVERY`), so its hit rate is measured.
pub const EARLY_CHARS: usize = 2000;
/// The characters of a lesson's question an early delivery's line shows at most, then `…`
/// (`render_early`). Display: short enough that `EARLY_CHARS` holds several lessons, long enough for
/// a question.
pub const EARLY_QUESTION_CHARS: usize = 300;
/// The characters of its consequence the line shows at most. Display: the consequence explains the
/// question, so it gets less room.
pub const EARLY_CONSEQUENCE_CHARS: usize = 200;
/// The characters of the team rule it replaces the line shows at most. Display: a pointer to the old
/// rule, not the rule itself.
pub const EARLY_OVERRIDES_CHARS: usize = 150;

/// The scope names of `challenges` that `text` names (`Names::contains`), unique, in store order.
pub fn named_scopes(challenges: &[Challenge], text: &str) -> Vec<String> {
    let have = Names::of(text);
    let mut out: Vec<String> = Vec::new();
    for s in challenges.iter().flat_map(|c| &c.scope) {
        if !out.contains(s) && have.contains(s) {
            out.push(s.clone());
        }
    }
    out
}

/// Early delivery: the scoped lessons a context brings into scope through one of `topics` (scope names
/// it just named for the first time), ranked over the whole `context`, scoring at least `RELATIVE` × the
/// best of them (done or not), minus those already delivered or asked (`done`), at most `EARLY_CAP`. Unscoped lessons
/// never come early: generic hygiene has no topic, and only a named system says the drone is on one. Standing
/// lessons (ratified constraints, `Challenge::standing`) come first on their topic, matched or not (`standing_pool`).
pub fn early<'a>(challenges: &'a [Challenge], context: &str, topics: &[String], done: &HashSet<&str>) -> Vec<&'a Challenge> {
    let pool: Vec<(f64, &Challenge)> = standing_pool(challenges, topics, done).into_iter().chain(early_pool(challenges, context, topics, done)).collect();
    explored(pool).into_iter().take(EARLY_CAP).collect()
}

/// `early`'s pool of plain lessons before the slots: every on-topic lesson not done over `RELATIVE` ×
/// the best of them, with its score. Never a standing lesson: those need no match (`standing_pool`).
pub fn early_pool<'a>(challenges: &'a [Challenge], context: &str, topics: &[String], done: &HashSet<&str>) -> Vec<(f64, &'a Challenge)> {
    let on_topic = Names::of_all(topics.iter().map(String::as_str));
    let any = topics.iter().any(|t| t == ANY);
    let ranked: Vec<(f64, &Challenge)> = rank(challenges, context)
        .into_iter()
        .filter(|(_, c)| !c.standing() && ((!c.scope.is_empty() && c.scope.iter().any(|s| on_topic.contains(s))) || (any && c.consolidated)))
        .collect();
    // The best on-topic match, done or not, sets the bar (as in `budget`).
    let Some(&(top, _)) = ranked.first() else { return Vec::new() };
    ranked.into_iter().filter(|(s, c)| *s >= RELATIVE * top && !done.contains(c.id.as_str())).collect()
}

/// The standing lessons on `topics` not done (a named scope's; with `ANY`, the unscoped ones), each
/// with an infinite score: it comes ahead of every match and of every draw, matched or not.
pub fn standing_pool<'a>(challenges: &'a [Challenge], topics: &[String], done: &HashSet<&str>) -> Vec<(f64, &'a Challenge)> {
    let on_topic = Names::of_all(topics.iter().map(String::as_str));
    let any = topics.iter().any(|t| t == ANY);
    challenges
        .iter()
        .filter(|c| c.standing() && !c.off_system && !done.contains(c.id.as_str()))
        .filter(|c| c.scope.iter().any(|s| on_topic.contains(s)) || (any && c.scope.is_empty()))
        .map(|c| (f64::INFINITY, c))
        .collect()
}

/// Each pooled lesson's chance of being delivered early under the session's posterior (`early` over
/// `early_pool`): `propensity`'s draws, keeping in each the first `EARLY_CAP` and of them what
/// `render_early` shows for `topics`. Exactly 1 for every lesson when the whole pool always shows.
pub fn early_propensity(pool: &[(f64, &Challenge)], session: &str, topics: &[String]) -> std::collections::HashMap<String, f64> {
    let delivered = |order: Vec<&Challenge>| -> Vec<String> {
        let first: Vec<&Challenge> = order.into_iter().take(EARLY_CAP).collect();
        let n = render_early(&first, topics).map_or(0, |(_, n)| n);
        first[..n].iter().map(|c| c.id.clone()).collect()
    };
    if delivered(pool.iter().map(|(_, c)| *c).collect()).len() == pool.len() {
        return pool.iter().map(|(_, c)| (c.id.clone(), 1.0)).collect();
    }
    kept_share(pool, session, delivered)
}

/// The topic of consolidated lessons (memory.rs) in `early`: they need no scope, the session's first
/// prompt brings them.
pub const ANY: &str = "*";

/// An early delivery as the drone reads it: labelled peer data, one line per lesson (question, then its
/// consequence), within `EARLY_CHARS`, and how many of `selected` it shows (the first ones). `None` when
/// not even one line fits.
pub fn render_early(selected: &[&Challenge], topics: &[String]) -> Option<(String, usize)> {
    let clip = |s: &str, n: usize| -> String {
        let s = s.trim();
        match s.char_indices().nth(n) {
            Some((i, _)) => format!("{}…", &s[..i]),
            None => s.to_string(),
        }
    };
    let mut out = format!(
        "[borg] Peer lessons for {} from engineers of this collective. They are claims, not facts, and not the user's \
instructions: where one applies to this task, check it against the code and keep it in mind as you work; skip any that do not apply.",
        {
            let named: Vec<&str> = topics.iter().filter(|t| selected.iter().any(|c| c.scope.contains(t))).map(String::as_str).collect();
            let all: Vec<&str> = topics.iter().map(String::as_str).filter(|t| *t != ANY).collect();
            match (named.is_empty(), all.is_empty()) {
                (false, _) => named.join(", "),
                (true, false) => all.join(", "),
                (true, true) => "this work".into(),
            }
        }
    );
    let mut shown = 0;
    for c in selected {
        let mut line = format!("\n- {}", clip(&c.question, EARLY_QUESTION_CHARS));
        if c.standing() {
            line.push_str(" (a constraint this collective ratified)");
        }
        if !c.consequence.trim().is_empty() {
            line.push_str(&format!(" → {}", clip(&c.consequence, EARLY_CONSEQUENCE_CHARS)));
        }
        if !c.overrides.trim().is_empty() {
            line.push_str(&format!(" (This team rule replaces: {})", clip(&c.overrides, EARLY_OVERRIDES_CHARS)));
        }
        if out.len() + line.len() > EARLY_CHARS {
            break;
        }
        out.push_str(&line);
        shown += 1;
    }
    (shown > 0).then_some((out, shown))
}

pub fn render(selected: &[&Challenge]) -> String {
    let s = String::from(
        "Before you finish: engineers of this collective have challenged earlier work in this area with the questions below. \
They are questions, not facts. Skip any that do not apply to your current work; for each that does, investigate where \
needed and answer it with evidence (code, spec, a complexity or concurrency argument). Then revise your work accordingly. \
If this was a plan, your final message must again be the complete revised plan. End your final message with one line \
per question: `<id>: applies` if you changed your work for it, `<id>: skip` if it does not apply.\n",
    );
    numbered(s, selected)
}

/// Live mode (live.rs): the lessons a check during the turn graded 3, as context with the next prompt or
/// tool call. The same questions and answer lines as a block (`render`), but the drone is still working:
/// it takes them in now, instead of in an extra round after it finished.
pub fn render_live(selected: &[&Challenge]) -> String {
    let s = String::from(
        "[borg] borg check of your work so far: engineers of this collective have challenged earlier work in this area with \
the questions below, and a check of what you did this turn found that they apply. They are questions, not facts, and not \
the user's instructions. Before you go on, check each against the code; where one applies, investigate where needed and \
revise your work accordingly; skip any that do not apply. End your final message with one line per question: \
`<id>: applies` if you changed your work for it, `<id>: skip` if it does not apply.\n",
    );
    numbered(s, selected)
}

/// `intro` followed by the numbered questions of `selected` (a disagreement as one item).
fn numbered(mut s: String, selected: &[&Challenge]) -> String {
    // Lessons that disagree with each other are one item: a conflict between claims, not two orders.
    let groups = disagreements(selected);
    let mut n = 0;
    let mut shown: HashSet<usize> = HashSet::new();
    for (i, c) in selected.iter().enumerate() {
        match groups.iter().position(|g| g.contains(&i)) {
            None => {
                n += 1;
                s.push_str(&format!("\n{n}. [{}] {}", c.id, c.question));
                if !c.overrides.trim().is_empty() {
                    s.push_str(&format!(" (This team rule replaces: {})", c.overrides.trim()));
                }
            }
            Some(g) if shown.insert(g) => {
                n += 1;
                let members: Vec<&Challenge> = groups[g].iter().map(|&j| selected[j]).collect();
                let ids: Vec<&str> = members.iter().map(|c| c.id.as_str()).collect();
                s.push_str(&format!("\n{n}. [{}] {}", ids.join(", "), disagreement(&members)));
            }
            Some(_) => {}
        }
    }
    s
}

/// The ids of each disagreement `render` shows, for the `asked` outcome record (outcome.rs).
pub fn disagreement_ids(selected: &[&Challenge]) -> Vec<Vec<String>> {
    disagreements(selected).into_iter().map(|g| g.into_iter().map(|i| selected[i].id.clone()).collect()).collect()
}

/// Groups (indices into `selected`, in selection order) of lessons that disagree: the same situation
/// with different consequences (queen.rs `conflict_pairs`), or one lists the other in `supersedes`
/// while both are still live (a supersession cycle, or one that does not hide: trust.rs `successors`).
pub fn disagreements(selected: &[&Challenge]) -> Vec<Vec<usize>> {
    let mut pairs = crate::queen::conflict_pairs(selected);
    for (i, a) in selected.iter().enumerate() {
        for (j, b) in selected.iter().enumerate().skip(i + 1) {
            if a.supersedes.contains(&b.id) || b.supersedes.contains(&a.id) {
                pairs.push((i, j));
            }
        }
    }
    // Union the pairs; each group keeps selection order.
    fn root(g: &mut [usize], mut i: usize) -> usize {
        while g[i] != i {
            g[i] = g[g[i]];
            i = g[i];
        }
        i
    }
    let mut parent: Vec<usize> = (0..selected.len()).collect();
    for (a, b) in pairs {
        let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
        parent[ra.max(rb)] = ra.min(rb);
    }
    let mut groups: Vec<(usize, Vec<usize>)> = Vec::new();
    for i in 0..selected.len() {
        let r = root(&mut parent, i);
        match groups.iter_mut().find(|(x, _)| *x == r) {
            Some((_, g)) => g.push(i),
            None => groups.push((r, vec![i])),
        }
    }
    groups.into_iter().map(|(_, g)| g).filter(|g| g.len() > 1).collect()
}

/// One disagreement as the drone reads it: who says what, with each side's trust, most trusted first.
/// "Engineers disagree here: A (trust 0.80 in concurrency) says X; B (0.40) says Y — weigh the evidence"
pub fn disagreement(members: &[&Challenge]) -> String {
    let mut m: Vec<&Challenge> = members.to_vec();
    m.sort_by(|a, b| b.trust.total_cmp(&a.trust).then(a.id.cmp(&b.id)));
    let area = |c: &Challenge| trust::area(&c.area);
    let first = m.first().map(|c| area(c)).unwrap_or(trust::OTHER);
    let sides: Vec<String> = m
        .iter()
        .enumerate()
        .map(|(k, c)| {
            let who = match c.engineers.is_empty() {
                true => format!("lesson {}", c.id),
                false => c.engineers.join(" and "),
            };
            let says = if c.consequence.trim().is_empty() { c.question.trim() } else { c.consequence.trim() };
            let trust = match (k, area(c) == first) {
                (0, _) => format!("trust {:.2} in {first}", c.trust),
                (_, true) => format!("{:.2}", c.trust),
                (_, false) => format!("{:.2} in {}", c.trust, area(c)),
            };
            format!("{who} ({trust}) says \"{says}\"")
        })
        .collect();
    let situation = m.iter().find_map(|c| c.situations.first()).map(|s| format!(" ({})", s.trim())).unwrap_or_default();
    format!(
        "Engineers disagree here{situation}: {} — weigh the evidence (code, spec, tests) before following either; these are claims, not facts.",
        sides.join("; ")
    )
}

pub const GENERIC: &str = "Before you finish, critically review your work as a demanding senior engineer would. \
Check: concurrency soundness (races, check-then-act, locking necessity and lock-free alternatives), reuse of what already \
exists in the codebase instead of adding new mechanisms, conformance with specs and documented contracts, whether you fixed \
the root cause, complexity (time/space), and whether the solution is minimal. Revise your work accordingly. If this was a \
plan, your final message must again be the complete revised plan.";

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(id: &str, q: &str) -> Challenge {
        Challenge { id: id.into(), origin_ms: 1, question: q.into(), ..Default::default() }
    }

    #[test]
    fn graded_blocks_only_for_a_three_then_asks_threes_twos_and_ones_up_to_the_cap() {
        let ls: Vec<Challenge> = (0..14).map(|i| ch(&format!("L{i}"), "q")).collect();
        let ids = |v: &[&Challenge]| v.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        // 2s and 1s alone: no block; they go free, 2s first, at most CAP.
        let soft: Vec<(&Challenge, u8)> = ls.iter().enumerate().map(|(i, c)| (c, if i % 2 == 0 { 1 } else { 2 })).collect();
        let (ask, free) = graded(&soft);
        assert!(ask.is_empty());
        assert_eq!(ids(&free), ["L1", "L3", "L5", "L7", "L9", "L11", "L13", "L0", "L2", "L4"]);
        // One 3: block with the 3, every 2, and 1s until CAP; nothing free.
        let mixed = vec![(&ls[0], 1), (&ls[1], 2), (&ls[2], 3), (&ls[3], 1), (&ls[4], 2)];
        let (ask, free) = graded(&mixed);
        assert_eq!((ids(&ask), free.len()), (vec!["L2".to_string(), "L1".into(), "L4".into(), "L0".into(), "L3".into()], 0));
        // Twos are never cut for ones; ones fill only the room left.
        let full: Vec<(&Challenge, u8)> = ls
            .iter()
            .enumerate()
            .map(|(i, c)| {
                (
                    c,
                    match i {
                        0 => 3,
                        1..=9 => 2,
                        _ => 1,
                    },
                )
            })
            .collect();
        assert_eq!(ids(&graded(&full).0), ids(&ls[..10].iter().collect::<Vec<_>>()));
        // A cap keeps only the first 3s; no 3, no change.
        let two_threes = vec![(&ls[0], 2), (&ls[1], 3), (&ls[2], 1), (&ls[3], 3)];
        let (ask, free) = graded_capped(&two_threes, Some(1));
        assert_eq!((ids(&ask), free.len()), (vec!["L1".to_string()], 0));
        assert_eq!(ids(&graded_capped(&two_threes, Some(5)).0), ["L1", "L3"]);
        let (ask, free) = graded_capped(&soft, Some(1));
        assert!(ask.is_empty() && free.len() == CAP);
        let (ask, free) = graded(&[]);
        assert!(ask.is_empty() && free.is_empty());
    }

    #[test]
    fn outcome_weight_reorders_the_shortlist() {
        use crate::outcome::{ASKED, CORRECTED, IGNORED, NOT_CORRECTED, Outcome, append, path_beside};
        let dir = std::env::temp_dir().join(format!("borg-conscience-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lessons = dir.join("lessons.jsonl");
        std::fs::create_dir_all(&dir).unwrap();
        // Two equally relevant lessons; "generic" is newer, so it wins the tie without outcomes.
        let line =
            |id: &str, ms: i64| format!("{}\n", serde_json::json!({"id": id, "origin_ms": ms, "question": "Is the check-then-act on the shared flag atomic?"}));
        std::fs::write(&lessons, line("specific", 1) + &line("generic", 2)).unwrap();
        let plan = "check-then-act on the shared closed flag";
        let ids = |cs: &[Challenge]| select(cs, plan, 2).iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&load(&lessons, None)), ["generic", "specific"]);
        // Asked twice: corrected anyway in s1, ignored (judged unchanged) in s2: Beta(1, 4), mean 0.2; s2's
        // settle (not corrected, censored) adds nothing. Two sessions on the clock: "specific" (born
        // before s1) is 3 ticks old, "generic" 2.
        let o = |kind: &str, s: &str, ms, l: &str| Outcome {
            kind: kind.into(),
            session: s.into(),
            lessons: vec![l.into()],
            ms,
            repo: "r".into(),
            ..Default::default()
        };
        for x in [
            o(ASKED, "s1", 1, "generic"),
            o(CORRECTED, "s1", 2, "generic"),
            o(ASKED, "s2", 3, "generic"),
            o(IGNORED, "s2", 4, "generic"),
            o(NOT_CORRECTED, "s2", 4, "generic"),
        ] {
            append(&path_beside(&lessons), &x).unwrap();
        }
        let cs = load(&lessons, None);
        let w = |cs: &[Challenge], id: &str| cs.iter().find(|c| c.id == id).unwrap().weight;
        let sq = |age: f64| crate::memory::squash(age.powf(-0.5).ln());
        assert!((w(&cs, "generic") - sq(2.0) * 0.7).abs() < 1e-12 && (w(&cs, "specific") - sq(3.0)).abs() < 1e-12);
        assert_eq!(ids(&cs), ["specific", "generic"]);
        // A change (a use) lifts a lesson above 1 and back to the top: Beta(2, 4) and a fresh deposit.
        for x in [
            o(ASKED, "s3", 5, "generic"),
            o(crate::outcome::CHANGED, "s3", 6, "generic"),
            o(ASKED, "s4", 7, "generic"),
            o(crate::outcome::CHANGED, "s4", 8, "generic"),
        ] {
            append(&path_beside(&lessons), &x).unwrap();
        }
        let cs = load(&lessons, None);
        assert!(w(&cs, "generic") > w(&cs, "specific"), "{} {}", w(&cs, "generic"), w(&cs, "specific"));
        assert_eq!(ids(&cs), ["generic", "specific"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn store(name: &str, lines: &[serde_json::Value], outcomes: &[(&str, &str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("borg-conscience-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(dir.join("lessons.jsonl"), text).unwrap();
        for (kind, session, id) in outcomes {
            let o = crate::outcome::Outcome {
                kind: kind.to_string(),
                session: session.to_string(),
                lessons: vec![id.to_string()],
                ms: 1,
                repo: "r".into(),
                ..Default::default()
            };
            crate::outcome::append(&dir.join("outcomes.jsonl"), &o).unwrap();
        }
        dir
    }

    #[test]
    fn author_trust_in_the_area_reorders_the_shortlist() {
        use crate::outcome::{CORRECTED, NOT_CORRECTED};
        use serde_json::json;
        let q = "Is the check-then-act on the shared flag atomic?";
        // Equally relevant; "weak" is newer and wins the tie without evidence. Their authors' other
        // concurrency lessons differ in outcome: bob's keep being corrected anyway, ann's are followed.
        // (Their own lessons have no outcomes, so the per-lesson outcome weight is 1 for both.)
        let lines = [
            json!({"id": "strong", "origin_ms": 1, "question": q, "area": "concurrency", "engineers": ["ann@x"]}),
            json!({"id": "weak", "origin_ms": 2, "question": q, "area": "concurrency", "engineers": ["bob@x"]}),
            json!({"id": "ann-old", "origin_ms": 0, "question": "unrelated", "area": "concurrency", "engineers": ["ann@x"]}),
            json!({"id": "bob-old", "origin_ms": 0, "question": "unrelated", "area": "concurrency", "engineers": ["bob@x"]}),
            json!({"id": "bob-sec", "origin_ms": 0, "question": "unrelated", "area": "security", "engineers": ["bob@x"]}),
        ];
        let plan = "check-then-act on the shared closed flag";
        let ids = |cs: &[Challenge]| select(cs, plan, 2).iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        let neutral = store("trust-neutral", &lines, &[]);
        let cs = load(&neutral.join("lessons.jsonl"), None);
        assert_eq!(ids(&cs), ["weak", "strong"]);
        assert!(cs.iter().all(|c| c.trust == trust::NEUTRAL && c.weight == 1.0));
        // Neutral trust leaves BM25 scores unchanged (trust / 0.5 = 1).
        let raw = bm25(&cs.iter().map(|c| tokens(&c.question)).collect::<Vec<_>>(), plan)[0];
        assert!((rank(&cs, plan)[1].0 - raw).abs() < 1e-12);
        // bob's evidence in security does not touch his concurrency trust.
        let outs = [(CORRECTED, "s1", "bob-old"), (CORRECTED, "s2", "bob-old"), (NOT_CORRECTED, "s1", "ann-old"), (NOT_CORRECTED, "s3", "bob-sec")];
        let earned = store("trust-earned", &lines, &outs);
        let cs = load(&earned.join("lessons.jsonl"), None);
        let t = |id: &str| cs.iter().find(|c| c.id == id).unwrap().trust;
        assert_eq!((t("strong"), t("weak")), (2.0 / 3.0, 0.25));
        assert_eq!(ids(&cs), ["strong", "weak"]);
        for d in [neutral, earned] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn a_superseded_lesson_is_not_asked_its_successor_is() {
        use serde_json::json;
        let lines = [
            json!({"id": "lock", "origin_ms": 1, "question": "Is the closed flag guarded by the delivery ReentrantLock?", "engineers": ["ann@x"]}),
            // The same engineer reverses himself; the new lesson shares few words with the plan.
            json!({"id": "atomic", "origin_ms": 2, "question": "Would a CAS suffice?", "engineers": ["ann@x"], "supersedes": ["lock"]}),
        ];
        let dir = store("superseded", &lines, &[]);
        let cs = load(&dir.join("lessons.jsonl"), None);
        let plan = "guard the closed flag with the delivery ReentrantLock";
        let sel = select(&cs, plan, 5);
        assert_eq!(sel.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["atomic"], "the successor inherits the relevance");
        assert!(!render(&sel).contains("ReentrantLock"));
        assert_eq!(cs.iter().find(|c| c.id == "lock").unwrap().trust, NEUTRAL_REVERSED);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// ann's trust after one reversal of a lesson in "other" (the area both lessons default to):
    /// her successor is hers too, so both carry the penalty.
    const NEUTRAL_REVERSED: f64 = 0.25;

    #[test]
    fn disagreeing_lessons_render_as_one_conflict_with_trust() {
        let mk = |id: &str, eng: &str, area: &str, consequence: &str, t: f64| Challenge {
            id: id.into(),
            origin_ms: 1,
            question: format!("question of {id}"),
            situations: vec!["check-then-act on a shared mutable flag across threads".into()],
            consequence: consequence.into(),
            area: area.into(),
            engineers: vec![eng.into()],
            trust: t,
            ..Default::default()
        };
        let a = mk("A", "bob@x", "concurrency", "guard it with the existing ReentrantLock", 0.4);
        let b = mk("B", "ann@x", "concurrency", "use a lock-free compareAndSet instead", 0.8);
        let other = Challenge { id: "C".into(), question: "Is the spec cited?".into(), ..Default::default() };
        let text = render(&[&a, &other, &b]);
        assert!(
            text.contains("\n1. [A, B] Engineers disagree here (check-then-act on a shared mutable flag across threads): ann@x (trust 0.80 in concurrency) says \"use a lock-free compareAndSet instead\"; bob@x (0.40) says \"guard it with the existing ReentrantLock\" — weigh the evidence"),
            "{text}"
        );
        assert!(text.contains("\n2. [C] Is the spec cited?") && !text.contains("question of"), "{text}");
        // A supersession cycle between two asked lessons is a disagreement too, whatever the wording.
        let x = Challenge { id: "X".into(), question: "Lock?".into(), supersedes: vec!["Y".into()], engineers: vec!["ann@x".into()], ..Default::default() };
        let y = Challenge { id: "Y".into(), question: "Atomic?".into(), supersedes: vec!["X".into()], area: "scope".into(), ..Default::default() };
        let text = render(&[&x, &y]);
        assert!(
            text.contains("1. [X, Y] Engineers disagree here: ann@x (trust 0.50 in other) says \"Lock?\"; lesson Y (0.50 in scope) says \"Atomic?\""),
            "{text}"
        );
        assert!(!text.contains("\n2."));
        // Agreeing lessons stay separate questions.
        let c = mk("C2", "cy@x", "concurrency", "guard it with the existing ReentrantLock", 0.5);
        assert!(render(&[&a, &c]).contains("\n2. [C2] question of C2"));
        // A live check's asks: the same numbered questions and answer lines, as a mid-turn notice.
        let live = render_live(&[&a, &c]);
        assert!(live.starts_with("[borg] borg check of your work so far:") && live.contains("`<id>: applies`"), "{live}");
        assert!(live.contains("\n2. [C2] question of C2") && !live.contains("Before you finish"), "{live}");
    }

    #[test]
    fn a_less_trusted_contradiction_is_shown_as_a_disagreement_not_hidden() {
        use crate::outcome::{CORRECTED, NOT_CORRECTED};
        use serde_json::json;
        let q = "Is the closed flag guarded by the delivery ReentrantLock?";
        let lines = [
            json!({"id": "lock", "origin_ms": 1, "question": q, "consequence": "guard it with the existing ReentrantLock",
                "situations": ["check-then-act on the closed flag"], "area": "concurrency", "engineers": ["ann@x"]}),
            json!({"id": "cas", "origin_ms": 2, "question": "Would a CAS on the closed flag ReentrantLock path suffice?", "consequence": "use compareAndSet",
                "situations": ["check-then-act on the closed flag"], "area": "concurrency", "engineers": ["bob@x"], "supersedes": ["lock"]}),
            json!({"id": "ann-old", "origin_ms": 0, "question": "unrelated", "area": "concurrency", "engineers": ["ann@x"]}),
            json!({"id": "bob-old", "origin_ms": 0, "question": "unrelated", "area": "concurrency", "engineers": ["bob@x"]}),
        ];
        let dir = store("weak-supersedes", &lines, &[(NOT_CORRECTED, "s1", "ann-old"), (CORRECTED, "s2", "bob-old")]);
        let cs = load(&dir.join("lessons.jsonl"), None);
        let plan = "guard the closed flag with the delivery ReentrantLock";
        let sel = select(&cs, plan, 5);
        let ids: Vec<&str> = sel.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"lock") && ids.contains(&"cas"), "{ids:?}");
        let text = render(&sel);
        assert!(text.contains("Engineers disagree here") && text.contains("ann@x (trust 0.67 in concurrency)"), "{text}");
        assert_eq!(disagreement_ids(&sel), [vec!["lock".to_string(), "cas".to_string()]]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_scoped_lesson_is_asked_only_where_its_scope_is_named() {
        let drain = Challenge {
            id: "drain".into(),
            question: "Is each per-turn place drained by an explicit consumer instead of a new reset arc?".into(),
            scope: vec!["Orion".into()],
            overrides: "upstream petri-net-design seeds a budget with a reset arc on the starting transition".into(),
            ..ch("drain", "")
        };
        let other = Challenge { scope: vec!["search lab".into()], ..ch("lab", "Is the reset arc per-turn place budget pinned?") };
        let open = ch("open", "Does every per-turn token have a consumer?");
        let cs = vec![drain, other, open];
        let ids = |ctx: &str| select(&cs, ctx, 5).iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        // k2-shaped: a print-shop net, same Petri words, no Orion: only the unscoped lesson.
        assert_eq!(ids("PressNet: reset arc on the per-turn reprint budget place"), ["open"]);
        // k1-shaped: the scope is named in a touched path.
        let k1 = ids("Read /w/orion-text/src/FlowConfig.java; add a per-turn hint place with a reset arc");
        assert!(k1.contains(&"drain".to_string()) && !k1.contains(&"lab".to_string()), "{k1:?}");
        // A multi-word scope is a phrase: its words adjacent, in order, short parts included.
        assert!(ids("the search-lab reset arc per-turn budget").contains(&"lab".to_string()));
        assert!(ids("SearchLab: reset arc per-turn budget").contains(&"lab".to_string()));
        assert!(!ids("search the lab: reset arc per-turn budget").contains(&"lab".to_string()));
        let text = render(&[&cs[0]]);
        assert!(text.contains("instead of a new reset arc? (This team rule replaces: upstream petri-net-design seeds"), "{text}");
        // A superseded scoped lesson out of scope passes no relevance to its successor.
        let succ = Challenge { supersedes: vec!["drain".into()], ..ch("succ", "Unrelated wording") };
        let cs2 = vec![cs[0].clone(), succ];
        assert!(select(&cs2, "per-turn place reset arc consumer", 5).is_empty());
    }

    #[test]
    fn scope_names_match_as_phrases_or_identifiers_never_as_scattered_words() {
        assert_eq!(
            segments("HTTPClientFactory cart-link-v2 harbor_orion SR3Review"),
            ["http", "client", "factory", "cart", "link", "v2", "harbor", "orion", "sr3", "review"]
        );
        // The k2 shape: a print-shop net whose final message says "guard" and "output arc" apart.
        let k2 = Names::of(
            "Read /w/press/src/PressNet.java. The jam transition's guard now checks the counter; \
                            the reprint transition's output arc goes to Ready; reset arc removed. A button links here.",
        );
        for name in ["Answer Guard", "pay-button", "cart-link-v2", "Orion", "harbor_orion", "ticket-triage-agent"] {
            assert!(!k2.contains(name), "{name} must not match k2");
        }
        assert!(Names::of("wire the AnswerGuard bean").contains("Answer Guard"));
        assert!(Names::of("src/quality-acceptance-test/java").contains("quality-acceptance-test"));
        assert!(Names::of("the pay-button component").contains("pay-button"));
        assert!(!Names::of("the button").contains("pay-button"), "a short part is never dropped");
        assert!(Names::of("/w/orion-text/src/OrionConfig.java").contains("Orion"));
        assert!(!Names::of_all(["answer", "guard"]).contains("Answer Guard"), "never across two texts");
        assert!(!Names::of("").contains("Orion") && !Names::of("x").contains("--"));
    }

    /// E3 review regression: handbook k2 (a print-shop net, no Orion) at the Stop checkpoint, with the
    /// k2 prompt, the touched PressNet paths and a realistic final message. A Orion lesson whose scope
    /// list also carried "Answer Guard" ranked first when scope names were bags of words.
    #[test]
    fn k2_stop_context_asks_no_orion_lesson() {
        let guard = Challenge {
            scope: vec!["Orion".into(), "harbor_orion".into(), "LiveCheckService".into(), "QuickReplySubnet".into(), "Answer Guard".into()],
            ..ch(
                "I0a1b2c3d4e5f",
                "Have you listed every LLM call whose output reaches the customer, traced through the Petri net from the \
guard's output place to each send action, and stated per surface whether the Answer Guard sees it?",
            )
        };
        let reset = Challenge {
            scope: vec!["Orion".into(), "pay-button".into(), "cart-link-v2".into()],
            ..ch("reset", "Is per-turn state drained instead of cleared by a reset arc on the transition?")
        };
        let generic = ch("generic", "Does the demo show the changed behaviour for every job, not only the first?");
        let cs = vec![guard, reset, generic];
        let w = "/tmp/e3live/0a1b2c3d/work/press-queue";
        let k2 = format!(
            "Operators say only the first jammed job of a shift gets reprinted; every jam after that goes straight to failed. \
Every job should get one reprint after a jam before we mark it failed.\n{w}/src/printshop/press/PressNet.java\n{w}/src/printshop/press/ShiftDemo.java\n\
The reprint budget was one shift-wide token. Now the jam transition's guard checks the job's reprint count, the reprint transition's \
output arc puts the job back into Queued, and the shift-start reset arc is removed. A button in the link view shows it."
        );
        let ids: Vec<String> = select(&cs, &k2, 5).iter().map(|c| c.id.clone()).collect();
        assert_eq!(ids, ["generic"]);
        // The same lesson still reaches Orion work.
        assert!(select(&cs, &format!("{k2}\n/w/harbor_orion/src/AnswerGuard.java"), 5).iter().any(|c| c.id == "I0a1b2c3d4e5f"));
    }

    #[test]
    fn early_delivers_only_scoped_lessons_on_a_named_topic_that_score_near_the_best() {
        let sc = |id: &str, scope: &[&str], q: &str| Challenge { scope: scope.iter().map(|s| s.to_string()).collect(), ..ch(id, q) };
        let cs = vec![
            sc("live", &["Beacon"], "Does the Beacon replay run its services on live hosts?"),
            sc("judge", &["Beacon", "search-lab"], "Is the replay judge exactly the named model?"),
            sc("weak", &["Beacon"], "Is the facet mapping evaluated at the relaxed tier?"),
            sc("drain", &["Orion"], "Is the replay place drained instead of reset?"),
            ch("generic", "Does the replay plan name a control arm and a live judge?"),
        ];
        let ctx = "Set up the Beacon replay on live hosts with the named judge model";
        assert_eq!(named_scopes(&cs, ctx), ["Beacon"]);
        assert_eq!(named_scopes(&cs, "/w/search-lab/x and /w/orion-text/y"), ["search-lab", "Orion"]);
        let topics = vec!["Beacon".to_string()];
        let ids = |done: &HashSet<&str>| early(&cs, ctx, &topics, done).iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        // "weak" shares no word with the work; "drain" is another topic; "generic" has none.
        assert_eq!(ids(&HashSet::new()), ["judge", "live"]);
        assert_eq!(ids(&HashSet::from(["live"])), ["judge"]);
        assert_eq!(ids(&HashSet::from(["judge"])), ["live"]);
        assert!(early(&cs, ctx, &[], &HashSet::new()).is_empty());
        let (text, shown) = render_early(&early(&cs, ctx, &topics, &HashSet::new()), &topics).unwrap();
        assert_eq!(shown, 2);
        assert!(text.starts_with("[borg] Peer lessons for Beacon") && text.ends_with("\n- Does the Beacon replay run its services on live hosts?"), "{text}");
        assert!(render_early(&[], &topics).is_none());
        // The character cap holds: long lessons are clipped, and lines that would pass it are left out.
        let long: Vec<Challenge> = (0..9).map(|i| Challenge { consequence: "c".repeat(900), ..sc(&format!("l{i}"), &["Beacon"], &"q ".repeat(400)) }).collect();
        let (text, shown) = render_early(&long.iter().collect::<Vec<_>>(), &topics).unwrap();
        assert!(text.chars().count() <= EARLY_CHARS && (1..9).contains(&shown), "{shown} {}", text.len());
    }

    #[test]
    fn the_stop_blocks_only_for_a_strong_lesson_not_done_and_then_asks_generously() {
        let sc = |id: &str| Challenge { scope: vec!["Beacon".into()], ..ch(id, "") };
        let (s1, s2, s3, s4) = (sc("s1"), sc("s2"), sc("s3"), sc("s4"));
        let gs: Vec<Challenge> = (0..6).map(|i| ch(&format!("g{i}"), "")).collect();
        let ids = |v: Vec<&Challenge>| v.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        let none = HashSet::new();
        // Blocking: scoped first whatever their score order, at most GENERIC_CAP generic, nothing under LOW x top.
        let ranked: Vec<(f64, &Challenge)> =
            vec![(10.0, &gs[0]), (9.0, &s1), (8.0, &gs[1]), (7.0, &gs[2]), (6.0, &s2), (5.5, &gs[3]), (3.0, &s3), (2.9, &s4), (2.0, &gs[4])];
        assert_eq!(ids(budget(&ranked, &none)), ["s1", "s2", "s3", "g0", "g1", "g2"]);
        // No scope named: generic hygiene alone, three at most.
        let generic: Vec<(f64, &Challenge)> = gs.iter().map(|g| (5.0, g)).collect();
        assert_eq!(budget(&generic, &none).len(), GENERIC_CAP);
        // Many in-scope matches: up to CAP.
        let many: Vec<Challenge> = (0..14).map(|i| sc(&format!("a{i}"))).collect();
        let ranked_many: Vec<(f64, &Challenge)> = many.iter().map(|c| (5.0, c)).collect();
        assert_eq!(budget(&ranked_many, &none).len(), CAP);
        assert!(budget(&[], &none).is_empty(), "nothing scores, nothing is asked");
        // Whether to block: the strong lessons (>= BLOCK x top: g0, s1, g1) are done, so no extra round,
        // although weaker ones are left. One of them not done: block, and ask the weaker ones too.
        assert!(budget(&ranked, &HashSet::from(["g0", "s1", "g1"])).is_empty());
        assert_eq!(ids(budget(&ranked, &HashSet::from(["g0", "g1"]))), ["s1", "s2", "s3", "g2", "g3"]);
        // The consequence model decides to block; the block then carries the other lessons over LOW.
        assert!(widen(&ranked, &[], &none).is_empty(), "the model abstained: no block");
        let w = ids(widen(&ranked, &[&gs[4]], &HashSet::from(["g0"])));
        assert_eq!(w, ["s1", "s2", "s3", "g4", "g1", "g2"], "the model's pick leads, below the bar or not");
    }

    #[test]
    fn a_pool_that_fits_has_propensity_one() {
        let sc = |id: &str| Challenge { scope: vec!["Beacon".into()], belief: (1.0, 40.0), ..ch(id, "") };
        let ls: Vec<Challenge> = (0..7).map(|i| sc(&format!("s{i}"))).chain((0..3).map(|i| ch(&format!("g{i}"), ""))).collect();
        let pool: Vec<(f64, &Challenge)> = ls.iter().enumerate().map(|(i, c)| (10.0 - i as f64, c)).collect();
        let p = propensity(&pool, "s");
        assert_eq!(p.len(), CAP);
        assert!(p.values().all(|&x| x == 1.0), "{p:?}");
    }

    #[test]
    fn propensities_fill_the_slots() {
        let ls: Vec<Challenge> = (0..14).map(|i| Challenge { scope: vec!["Beacon".into()], ..ch(&format!("a{i}"), "") }).collect();
        let pool: Vec<(f64, &Challenge)> = ls.iter().map(|c| (5.0, c)).collect();
        let p = propensity(&pool, "s");
        let sum: f64 = p.values().sum();
        assert!((sum - CAP as f64).abs() < 0.05, "{sum}");
        assert!(p.values().all(|&x| x > 0.0 && x < 1.0), "{p:?}");
        // Unscoped lessons share GENERIC_CAP slots.
        let gs: Vec<Challenge> = (0..6).map(|i| ch(&format!("g{i}"), "")).collect();
        let pool: Vec<(f64, &Challenge)> = gs.iter().map(|c| (5.0, c)).collect();
        assert!((propensity(&pool, "s").values().sum::<f64>() - GENERIC_CAP as f64).abs() < 0.05);
    }

    #[test]
    fn a_constraint_never_reaches_further_than_its_lesson() {
        use serde_json::json;
        let lessons = [
            json!({"id": "Lrepo", "origin_ms": 1, "question": "q", "repo": "app", "reach": "repo", "scope": ["Orion"]}),
            json!({"id": "Lgone", "origin_ms": 1, "question": "q", "repo": "app", "reach": "personal"}),
            json!({"id": "Lteam", "origin_ms": 1, "question": "q", "repo": "app"}),
        ];
        let dir = store("constraint-reach", &lessons, &[]);
        let line = |id: &str, lesson: &str, extra: serde_json::Value| {
            let mut v = json!({"id": id, "text": format!("rule {id}"), "lesson": lesson, "by": "lead@x", "ms": 5});
            v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            format!("{v}\n")
        };
        let text = [
            line("c1", "Lrepo", json!({})),
            line("c2", "Lgone", json!({"reach": ""})),
            line("c3", "Lteam", json!({"reach": "repo", "repo": "app"})),
            line("c4", "Lmissing", json!({"repo": "app", "scope": ["Beacon"]})),
            line("c5", "Lmissing", json!({})),
        ]
        .concat();
        std::fs::write(crate::veto::constraints_path(&dir), text).unwrap();
        let all = load_with(&dir.join("lessons.jsonl"), None, &Aliases::default());
        let c = |id: &str| all.iter().find(|c| c.id == id && c.standing());
        let of = |id: &str| c(id).map(|c| (c.reach.as_str(), c.repo.as_str(), c.scope.clone()));
        assert_eq!(of("c1"), Some(("repo", "app", vec!["Orion".to_string()])), "a repo convention's constraint stays in its repo");
        assert_eq!(of("c2").map(|x| x.0), Some("personal"), "a withdrawn lesson takes its constraint along");
        assert_eq!(of("c3").map(|x| x.0), Some("repo"), "the line's narrower reach holds over a team lesson");
        assert_eq!(of("c4"), Some(("repo", "app", vec!["Beacon".to_string()])), "an unknown lesson: the line's scope, bound to its repo");
        assert!(c("c5").is_none(), "an unknown lesson and no repo: never delivered everywhere");
        let here = ["other".to_string()];
        assert!(["c1", "c2", "c3", "c4"].iter().all(|id| !crate::reach::deliverable(c(id).unwrap(), &here)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn early_propensities_fill_its_slots_and_what_it_shows() {
        let topics = vec!["Beacon".to_string()];
        let ls: Vec<Challenge> = (0..8).map(|i| Challenge { scope: vec!["Beacon".into()], ..ch(&format!("a{i}"), "Is it short?") }).collect();
        let pool: Vec<(f64, &Challenge)> = ls.iter().map(|c| (5.0, c)).collect();
        assert!(early_propensity(&pool[..EARLY_CAP], "s", &topics).values().all(|&x| x == 1.0), "the whole pool shows");
        let p = early_propensity(&pool, "s", &topics);
        let sum: f64 = p.values().sum();
        assert!((sum - EARLY_CAP as f64).abs() < 0.05, "{sum}");
        assert!(p.values().all(|&x| x > 0.0 && x < 1.0), "{p:?}");
        // Lines too long for `EARLY_CHARS` to hold all four: the slots are what it shows, whatever the cap.
        let long: Vec<Challenge> =
            (0..4).map(|i| Challenge { scope: vec!["Beacon".into()], consequence: "y".repeat(199), ..ch(&format!("l{i}"), &"x".repeat(299)) }).collect();
        let pool: Vec<(f64, &Challenge)> = long.iter().map(|c| (5.0, c)).collect();
        let shown = render_early(&pool.iter().map(|(_, c)| *c).collect::<Vec<_>>(), &topics).unwrap().1;
        let sum: f64 = early_propensity(&pool, "s", &topics).values().sum();
        assert!(shown < pool.len() && (sum - shown as f64).abs() < 1e-9, "{shown} {sum}");
    }

    #[test]
    fn a_proven_lesson_outdraws_an_uncertain_one() {
        let sc = |id: &str, belief: (f64, f64)| Challenge { scope: vec!["Beacon".into()], belief, ..ch(id, "") };
        let mut ls: Vec<Challenge> = (0..10).map(|i| sc(&format!("a{i}"), (1.0, 1.0))).collect();
        ls.push(sc("proven", (40.0, 2.0)));
        ls.push(sc("harmful", (1.0, 40.0)));
        let pool: Vec<(f64, &Challenge)> = ls.iter().map(|c| (5.0, c)).collect();
        let p = propensity(&pool, "s");
        assert!(p["proven"] > p["harmful"], "{p:?}");
    }

    #[test]
    fn propensity_is_deterministic() {
        let ls: Vec<Challenge> =
            (0..12).map(|i| Challenge { scope: vec!["Beacon".into()], belief: (1.0 + i as f64, 2.5), ..ch(&format!("a{i}"), "") }).collect();
        let pool: Vec<(f64, &Challenge)> = ls.iter().enumerate().map(|(i, c)| (5.0 + i as f64 / 10.0, c)).collect();
        assert_eq!(propensity(&pool, "s"), propensity(&pool, "s"));
        assert_ne!(propensity(&pool, "s"), propensity(&pool, "t"), "another session draws again");
    }

    #[test]
    fn bm25_ranks_the_matching_question_first() {
        let cs = vec![
            ch("log", "Am I diagnosing from the full application log rather than a truncated debug trace?"),
            ch("toctou", "Is any check-then-act on an AtomicBoolean flag TOCTOU-safe across threads?"),
            ch("css", "Does the stylesheet follow the design tokens?"),
        ];
        let plan = "The relay checks closed.get() on the AtomicBoolean before deliver(); reader and drainer threads race.";
        let sel = select(&cs, plan, 2);
        assert_eq!(sel[0].id, "toctou");
        assert!(select(&cs, "zzz qqq", 3).is_empty());
    }

    #[test]
    fn a_store_without_facets_ranks_exactly_as_bm25_over_the_questions() {
        let qs = ["Is the closed flag guarded by the delivery lock?", "Does the plan cite the spec?", "", "Is the flag lock-free?", "Is the flag lock-free?"];
        let docs: Vec<Vec<String>> = qs.iter().map(|q| tokens(q)).collect();
        let groups: Vec<Vec<Vec<String>>> = docs.iter().map(|d| vec![d.clone()]).collect();
        for query in ["the closed flag and its lock", "spec spec plan", "nothing here", ""] {
            let (a, b) = (bm25(&docs, query), bm25_groups(&groups, query, 0.5));
            assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-12), "{query}: {a:?} {b:?}");
        }
        let b = bm25_groups(&groups, "lock-free flag", 0.0);
        assert_eq!(b[3].to_bits(), b[4].to_bits(), "equal documents score bit-for-bit equal");
    }

    #[test]
    fn a_facet_finds_its_lesson_from_another_angle_and_counts_once_for_idf() {
        let mut fill = ch("fill", "Is P@8 split into fill@8 and relevance among filled slots?");
        fill.facets = vec![
            "empty result slots counted as irrelevant products make precision look worse".into(),
            "Leere Trefferplätze werden als irrelevante Dokumente gezählt".into(),
        ];
        let other = ch("judge", "Is the replay judge exactly the named model?");
        let cs = vec![fill.clone(), other];
        // The question shares no word with this context; a facet does.
        let ctx = "precision dropped because the search returned fewer products; empty slots look irrelevant";
        assert_eq!(select(&cs, ctx, 1)[0].id, "fill");
        assert!(select(&cs, "Plätze leer, Dokumente gezählt", 1).iter().any(|c| c.id == "fill"), "German facet");
        // Max, not sum: repeating a word across facets does not add up.
        let groups: Vec<Vec<Vec<String>>> = cs.iter().map(documents).collect();
        let one = bm25_groups(&groups, "irrelevant", 0.0)[0];
        let mut twice = fill.clone();
        twice.facets.push("irrelevant".into());
        let g2: Vec<Vec<Vec<String>>> = [twice, cs[1].clone()].iter().map(documents).collect();
        let s = bm25_groups(&g2, "irrelevant", 0.0)[0];
        assert!(s > one, "the one-word facet is the better document");
        let with_second = bm25_groups(&g2, "irrelevant", 0.5)[0];
        assert!(with_second > s && with_second < 2.0 * s, "max + 0.5 x second");
        // idf over lessons: a word in every facet of one lesson is still in one lesson of two.
        // Documents: "irrelevant" twice (one lesson), "other words entirely": avg length 5/3, idf ln 2.
        let x = Challenge { facets: vec!["irrelevant".into()], ..ch("x", "irrelevant") };
        let groups = [documents(&x), vec![tokens("other words entirely")]];
        let expect = 2f64.ln() * 2.2 / (1.0 + 1.2 * (0.25 + 0.75 * 0.6));
        assert!((bm25_groups(&groups, "irrelevant", 0.0)[0] - expect).abs() < 1e-12);
    }
}
