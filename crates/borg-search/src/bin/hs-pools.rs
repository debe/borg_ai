//! The internal evaluation "seekstorm-bm25" (not published): every lexical config of the grid scored on
//! the judge-graded pools of real logged checks (pools.jsonl), for the autotuned re-test of borg's BM25
//! against SeekStorm over borg's tokens.
//!
//!   hs-pools score <collective dir> <out.jsonl>          NDCG@10 and gate score of every config per check
//!   hs-pools turns <collective dir> <out.jsonl> <cfg>... gate score of each config at every turn end
//!   hs-pools build <collective dir> <cfg> <index dir>    persist a SeekStorm config's index (borg: no-op)
//!   hs-pools query <collective dir> <cfg> <query file> [<index dir>]
//!                                                        one ranking in a fresh process, top 20
//!
//! Every check is scored against today's store (lesson text and idf as of now, not as of the check).

use std::collections::{HashMap, HashSet};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use borg::collective::Aliases;
use borg::conscience::{self, Bm25, Challenge, Compound, Fields, MIN_DF_STORE, RANKING, TokenRule, tokens_with};
use borg::{pools, prefilter, trust};
use borg_search::Lesson;
use borg_search::seek::{self, SeekFields, token_query};
use seekstorm::index::{Close, IndexArc, LexicalSimilarity};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::task::JoinSet;

/// Experiment parameter (method): the rank depth of NDCG, a check's pool shows about this many lessons.
const DEPTH: usize = 10;
/// Experiment parameter: the depth `query` prints (as hs.rs `TOP`).
const TOP: usize = 20;
/// Today's ranking (`conscience::RANKING`) as a config id. Protocol: the scorer's reference arm.
const BASELINE: &str = "borg/legacy/3/none/1.2/0.75/q";

// The grid (experiment parameters, pre-registered in the internal evaluation "seekstorm-bm25").
const MIN_LENS: [usize; 2] = [2, 3];
const COMPOUNDS: [(Compound, &str); 2] = [(Compound::Legacy, "legacy"), (Compound::Code, "code")];
const CUTS: [Option<f64>; 3] = [None, Some(0.5), Some(0.25)];
const K1S: [f64; 3] = [0.9, 1.2, 1.6];
const BS: [f64; 3] = [0.5, 0.75, 0.9];
const BORG_FIELDS: [(Fields, &str); 2] = [(Fields::Question, "q"), (Fields::QuestionSituationsConsequence, "qsc")];
const SIMS: [(Sim, &str); 2] = [(Sim::Bm25f, "bm25f"), (Sim::Prox, "prox")];
const SEEK_FIELDS: [(SeekFields, &str); 3] = [(SeekFields::Q, "q"), (SeekFields::Qsc, "qsc"), (SeekFields::QscHalf, "qsch")];

/// SeekStorm's lexical similarity (`LexicalSimilarity` is not `Copy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Sim {
    Bm25f,
    Prox,
}

impl Sim {
    fn similarity(self) -> LexicalSimilarity {
        match self {
            Sim::Bm25f => LexicalSimilarity::Bm25f,
            Sim::Prox => LexicalSimilarity::Bm25fProximity,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Engine {
    Borg { k1: f64, b: f64, fields: Fields },
    Seek { sim: Sim, fields: SeekFields },
}

/// One lexical config: tokens, the document-frequency cut on query words, the engine and its parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Config {
    rule: TokenRule,
    cut: Option<f64>,
    engine: Engine,
}

fn name<T: PartialEq + Copy>(table: &[(T, &'static str)], v: T) -> &'static str {
    table.iter().find(|(t, _)| *t == v).map(|(_, n)| *n).expect("named")
}

fn value<T: Copy>(table: &[(T, &'static str)], s: &str) -> Result<T, String> {
    table.iter().find(|(_, n)| *n == s).map(|(t, _)| *t).ok_or_else(|| format!("unknown {s:?}"))
}

fn number<T: std::str::FromStr>(s: &str) -> Result<T, String> {
    s.parse().map_err(|_| format!("not a number: {s:?}"))
}

impl Config {
    /// `borg/<legacy|code>/<min_len>/<none|cut>/<k1>/<b>/<q|qsc>` or `seek/<..>/<..>/<..>/<bm25f|prox>/<q|qsc|qsch>`.
    fn id(&self) -> String {
        let cut = self.cut.map_or("none".to_string(), |c| c.to_string());
        let head = format!("{}/{}/{cut}", name(&COMPOUNDS, self.rule.compound), self.rule.min_len);
        match self.engine {
            Engine::Borg { k1, b, fields } => format!("borg/{head}/{k1}/{b}/{}", name(&BORG_FIELDS, fields)),
            Engine::Seek { sim, fields } => format!("seek/{head}/{}/{}", name(&SIMS, sim), name(&SEEK_FIELDS, fields)),
        }
    }

    fn parse(id: &str) -> Result<Config, String> {
        let parts: Vec<&str> = id.split('/').collect();
        let [family @ ("borg" | "seek"), compound, min_len, cut, tail @ ..] = parts.as_slice() else { return Err(format!("config {id:?}")) };
        let rule = TokenRule { min_len: number(min_len)?, compound: value(&COMPOUNDS, compound)? };
        let cut = if *cut == "none" { None } else { Some(number(cut)?) };
        let engine = Engine::parse(family, tail).ok_or_else(|| format!("config {id:?}"))??;
        Ok(Config { rule, cut, engine })
    }
}

impl Engine {
    /// The engine part of a config id after the family (None: wrong family or arity).
    fn parse(family: &str, tail: &[&str]) -> Option<Result<Engine, String>> {
        let borg =
            |k1: &str, b: &str, f: &str| -> Result<Engine, String> { Ok(Engine::Borg { k1: number(k1)?, b: number(b)?, fields: value(&BORG_FIELDS, f)? }) };
        let seek = |s: &str, f: &str| -> Result<Engine, String> { Ok(Engine::Seek { sim: value(&SIMS, s)?, fields: value(&SEEK_FIELDS, f)? }) };
        match (family, tail) {
            ("borg", [k1, b, f]) => Some(borg(k1, b, f)),
            ("seek", [s, f]) => Some(seek(s, f)),
            _ => None,
        }
    }
}

/// The fixed grid in its order: per family (borg, then SeekStorm), token rules (min_len × compound),
/// then cuts, then the engine's parameters.
fn grid() -> Vec<Config> {
    let borg: Vec<Engine> =
        K1S.iter().flat_map(|&k1| BS.iter().flat_map(move |&b| BORG_FIELDS.iter().map(move |&(fields, _)| Engine::Borg { k1, b, fields }))).collect();
    let seek: Vec<Engine> = SIMS.iter().flat_map(|&(sim, _)| SEEK_FIELDS.iter().map(move |&(fields, _)| Engine::Seek { sim, fields })).collect();
    let rules: Vec<TokenRule> = MIN_LENS.iter().flat_map(|&min_len| COMPOUNDS.iter().map(move |&(compound, _)| TokenRule { min_len, compound })).collect();
    let mut out = Vec::new();
    for family in [borg, seek] {
        for &rule in &rules {
            for cut in CUTS {
                out.extend(family.iter().map(|&engine| Config { rule, cut, engine }));
            }
        }
    }
    out
}

/// conscience.rs `idf`: BM25's idf of a word `df` of `count` lessons hold.
fn idf(df: f64, count: usize) -> f64 {
    let n = count.max(1) as f64;
    (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
}

/// conscience.rs `cut`: whether `cut` drops a query word `df` of `count` lessons hold.
fn cut_drops(df: f64, count: usize, cut: Option<f64>) -> bool {
    cut.is_some_and(|c| count >= MIN_DF_STORE && df / count as f64 > c)
}

/// The store's lessons as the product loads them (`load_unweighted_with`, no aliases), and what the gate
/// needs of them.
struct Corpus {
    store: String,
    challenges: Vec<Challenge>,
    lessons: Vec<Lesson>,
    at: HashMap<String, usize>,
    gate: GateRule,
}

impl Corpus {
    fn load(dir: &Path) -> Corpus {
        let store = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        Corpus::of(store, conscience::load_unweighted_with(&dir.join("lessons.jsonl"), None, &Aliases::default()))
    }

    fn of(store: String, challenges: Vec<Challenge>) -> Corpus {
        let lessons = challenges
            .iter()
            .map(|c| Lesson {
                id: c.id.clone(),
                origin_ms: c.origin_ms,
                question: c.question.clone(),
                situations: c.situations.clone(),
                consequence: c.consequence.clone(),
                scope: c.scope.clone(),
                facets: c.facets.clone(),
            })
            .collect();
        let at = challenges.iter().enumerate().map(|(i, c)| (c.id.clone(), i)).collect();
        let gate = GateRule::of(&challenges);
        Corpus { store, challenges, lessons, at, gate }
    }

    /// prefilter.rs `score` from a config's lexical scores of the turn: the best lesson after
    /// `conscience::rank_raw`, over the store's largest idf.
    fn gate_score(&self, raw: &[f64], keep: &[bool]) -> f64 {
        let n = self.challenges.len() as f64;
        let idf_max = (1.0 + (n - 0.5) / 1.5).ln();
        if idf_max > 0.0 { self.gate.top(raw, keep) / idf_max } else { 0.0 }
    }
}

/// `conscience::rank_raw` reduced to its best score, with what does not depend on the query computed once.
struct GateRule {
    scoped: bool,
    dormant: Vec<bool>,
    /// The live successor a lesson passes its relevance on to; hidden lessons are never ranked.
    successor: Vec<Option<usize>>,
    hidden: Vec<bool>,
    factor: Vec<f64>,
}

impl GateRule {
    fn of(ch: &[Challenge]) -> GateRule {
        let successors = trust::successors(ch);
        let at: HashMap<&str, usize> = ch.iter().enumerate().map(|(i, c)| (c.id.as_str(), i)).collect();
        GateRule {
            scoped: ch.iter().any(|c| !c.scope.is_empty()),
            dormant: ch.iter().map(|c| c.dormant).collect(),
            successor: ch.iter().map(|c| successors.get(&c.id).and_then(|s| at.get(s.as_str()).copied())).collect(),
            hidden: ch.iter().map(|c| successors.contains_key(&c.id)).collect(),
            factor: ch.iter().map(|c| c.weight * c.trust / trust::NEUTRAL).collect(),
        }
    }

    /// Which lessons the scope rule keeps for `scope_text`.
    fn keep(&self, ch: &[Challenge], scope_text: &str) -> Vec<bool> {
        if !self.scoped {
            return vec![true; ch.len()];
        }
        let have = conscience::Names::of(scope_text);
        ch.iter().map(|c| conscience::in_scope(c, &have) && !c.off_system).collect()
    }

    fn top(&self, raw: &[f64], keep: &[bool]) -> f64 {
        let raw: Vec<f64> = raw.iter().zip(keep).map(|(r, k)| if *k { *r } else { 0.0 }).collect();
        let best = raw.iter().copied().fold(0.0, f64::max);
        let raw: Vec<f64> = raw.iter().zip(&self.dormant).map(|(r, d)| if *d && *r < conscience::STRONG * best { 0.0 } else { *r }).collect();
        let mut relevance = raw.clone();
        for (i, s) in self.successor.iter().enumerate() {
            if let Some(j) = *s {
                relevance[j] = relevance[j].max(raw[i]);
            }
        }
        relevance.iter().zip(&self.hidden).zip(&self.factor).filter(|((_, h), _)| !**h).map(|((r, _), f)| r * f).fold(0.0, f64::max)
    }
}

/// borg's BM25 (`conscience::bm25_groups_with`, second 0) as postings over one (rule, fields)'s
/// documents, so k1, b and the cut vary without re-tokenizing the store. Equal sums in equal order.
#[derive(Default)]
struct Postings {
    lessons: usize,
    owner: Vec<usize>,
    len: Vec<f64>,
    avg: f64,
    /// word -> (lessons holding it, (document, tf) per document holding it)
    terms: HashMap<String, (f64, Vec<(usize, f64)>)>,
}

impl Postings {
    fn of(ch: &[Challenge], rule: TokenRule, fields: Fields) -> Postings {
        let p = Bm25 { rule, fields, ..RANKING };
        let mut post = Postings { lessons: ch.len(), ..Postings::default() };
        for (l, c) in ch.iter().enumerate() {
            let mut seen: HashSet<String> = HashSet::new();
            for d in conscience::documents_with(c, &p) {
                post.add(l, &d, &mut seen);
            }
        }
        post.avg = post.len.iter().sum::<f64>() / post.owner.len().max(1) as f64;
        post
    }

    fn add(&mut self, lesson: usize, doc: &[String], seen: &mut HashSet<String>) {
        let at = self.owner.len();
        self.owner.push(lesson);
        self.len.push(doc.len() as f64);
        let mut tf: HashMap<&str, f64> = HashMap::new();
        for t in doc {
            *tf.entry(t).or_default() += 1.0;
        }
        for (t, f) in tf {
            let e = self.terms.entry(t.to_string()).or_default();
            e.1.push((at, f));
            if seen.insert(t.to_string()) {
                e.0 += 1.0;
            }
        }
    }

    /// Each lesson's score (its best document) for the distinct query words `terms`, in first-occurrence order.
    fn scores(&self, terms: &[String], k1: f64, b: f64, cut: Option<f64>) -> Vec<f64> {
        let mut doc = vec![0.0f64; self.owner.len()];
        for (df, post) in terms.iter().filter_map(|t| self.terms.get(t)) {
            if cut_drops(*df, self.lessons, cut) {
                continue;
            }
            let idf = idf(*df, self.lessons);
            for &(d, f) in post {
                let norm = k1 * (1.0 - b + b * self.len[d] / self.avg);
                doc[d] += idf * f * (k1 + 1.0) / (f + norm);
            }
        }
        let mut out = vec![0.0f64; self.lessons];
        for (d, s) in doc.into_iter().enumerate() {
            out[self.owner[d]] = out[self.owner[d]].max(s);
        }
        out
    }
}

/// The words a SeekStorm config's cut drops: in more than `cut` of the lessons, counted over its fields
/// merged per lesson (none below `MIN_DF_STORE` lessons).
fn drop_set(ch: &[Challenge], rule: TokenRule, fields: SeekFields, cut: Option<f64>) -> HashSet<String> {
    if cut.is_none() || ch.len() < MIN_DF_STORE {
        return HashSet::new();
    }
    let mut df: HashMap<String, f64> = HashMap::new();
    for c in ch {
        let text = match fields {
            SeekFields::Q => c.question.clone(),
            SeekFields::Qsc | SeekFields::QscHalf => format!("{}\n{}\n{}", c.question, c.situations.join("\n"), c.consequence),
        };
        for t in tokens_with(&text, rule).into_iter().collect::<HashSet<_>>() {
            *df.entry(t).or_default() += 1.0;
        }
    }
    df.into_iter().filter(|(_, d)| cut_drops(*d, ch.len(), cut)).map(|(t, _)| t).collect()
}

struct SeekArm {
    rule: TokenRule,
    drop: HashSet<String>,
    index: IndexArc,
    /// doc id -> lesson (corpus order), from the index's sidecar
    lesson_of: Arc<Vec<Option<usize>>>,
}

enum Arm {
    Borg { rule: TokenRule, fields: Fields, k1: f64, b: f64, cut: Option<f64> },
    Seek(Arc<SeekArm>),
}

/// Where SeekStorm indexes come from: built under a root (one per (rule, fields, sim)), or one already
/// built at a directory.
#[derive(Clone, Copy)]
enum Source<'a> {
    Build(&'a Path),
    Open(&'a Path),
}

type Terms = HashMap<TokenRule, Vec<Vec<String>>>;
/// A token index and its doc id -> lesson map (corpus order), from the index's sidecar.
type TokenIndex = (IndexArc, Arc<Vec<Option<usize>>>);

/// Every config's engine, ready to score queries against the store.
struct Engines {
    n: usize,
    rules: Vec<TokenRule>,
    postings: HashMap<(TokenRule, Fields), Postings>,
    indexes: HashMap<(TokenRule, SeekFields, Sim), TokenIndex>,
    arms: Vec<Arm>,
}

impl Engines {
    async fn new(corpus: &Corpus, cfgs: &[Config], source: Source<'_>) -> Engines {
        let rules: Vec<TokenRule> = cfgs.iter().map(|c| c.rule).collect::<HashSet<_>>().into_iter().collect();
        let mut e = Engines { n: corpus.challenges.len(), rules, postings: HashMap::new(), indexes: HashMap::new(), arms: Vec::new() };
        for cfg in cfgs {
            let arm = e.arm(corpus, cfg, source).await;
            e.arms.push(arm);
        }
        e
    }

    async fn arm(&mut self, corpus: &Corpus, cfg: &Config, source: Source<'_>) -> Arm {
        match cfg.engine {
            Engine::Borg { k1, b, fields } => {
                self.postings.entry((cfg.rule, fields)).or_insert_with(|| Postings::of(&corpus.challenges, cfg.rule, fields));
                Arm::Borg { rule: cfg.rule, fields, k1, b, cut: cfg.cut }
            }
            Engine::Seek { sim, fields } => {
                let key = (cfg.rule, fields, sim);
                if !self.indexes.contains_key(&key) {
                    let opened = token_index(corpus, key, source, self.indexes.len()).await;
                    self.indexes.insert(key, opened);
                }
                let (index, lesson_of) = self.indexes[&key].clone();
                Arm::Seek(Arc::new(SeekArm { rule: cfg.rule, drop: drop_set(&corpus.challenges, cfg.rule, fields, cfg.cut), index, lesson_of }))
            }
        }
    }

    /// Each config's lexical score of every lesson (corpus order), per text: `[config][text][lesson]`.
    async fn lexical(&self, texts: &[String]) -> Vec<Vec<Vec<f64>>> {
        let terms: Arc<Terms> = Arc::new(self.rules.iter().map(|&r| (r, texts.iter().map(|t| token_query(t, r, &HashSet::new())).collect())).collect());
        let mut searches = JoinSet::new();
        for (k, arm) in self.arms.iter().enumerate() {
            if let Arm::Seek(a) = arm {
                searches.spawn(seek_lexical(k, a.clone(), terms.clone(), self.n));
            }
        }
        let mut out: Vec<Vec<Vec<f64>>> = self.arms.iter().map(|arm| self.borg_lexical(arm, &terms)).collect();
        while let Some(done) = searches.join_next().await {
            let (k, scores) = done.expect("search task");
            out[k] = scores;
        }
        out
    }

    fn borg_lexical(&self, arm: &Arm, terms: &Terms) -> Vec<Vec<f64>> {
        let Arm::Borg { rule, fields, k1, b, cut } = *arm else { return Vec::new() };
        let post = &self.postings[&(rule, fields)];
        terms[&rule].iter().map(|t| post.scores(t, k1, b, cut)).collect()
    }

    async fn close(self) {
        for (index, _) in self.indexes.into_values() {
            index.close().await;
        }
    }
}

/// A SeekStorm token index of the store (built at `<root>/<n>` or opened), with its doc id -> lesson map.
async fn token_index(corpus: &Corpus, (rule, fields, sim): (TokenRule, SeekFields, Sim), source: Source<'_>, n: usize) -> TokenIndex {
    let dir = match source {
        Source::Build(root) => {
            let dir = root.join(n.to_string());
            seek::build_tokens(&corpus.lessons, &dir, rule, fields, sim.similarity()).await.expect("build");
            dir
        }
        Source::Open(dir) => dir.to_path_buf(),
    };
    let (index, ids) = seek::open(&dir).await.expect("open");
    // the sidecar's order is the index's doc ids
    for d in [0, ids.len() / 2, ids.len().saturating_sub(1)] {
        if !ids.is_empty() {
            assert_eq!(seek::stored_id(&index, d).await.as_deref(), Some(ids[d].as_str()), "doc id order");
        }
    }
    let lesson_of = ids.iter().map(|id| corpus.at.get(id).copied()).collect();
    (index, Arc::new(lesson_of))
}

/// One SeekStorm config's scores for every text (`search_tokens` over the whole store, absent = 0).
async fn seek_lexical(k: usize, arm: Arc<SeekArm>, terms: Arc<Terms>, n: usize) -> (usize, Vec<Vec<f64>>) {
    let mut out = Vec::new();
    for t in &terms[&arm.rule] {
        let query: Vec<String> = t.iter().filter(|w| !arm.drop.contains(*w)).cloned().collect();
        let mut scores = vec![0.0f64; n];
        for (d, s) in seek::search_tokens(&arm.index, &query, n.max(1)).await {
            if let Some(&Some(l)) = arm.lesson_of.get(d) {
                scores[l] = f64::from(s);
            }
        }
        out.push(scores);
    }
    (k, out)
}

/// A pools.jsonl line's format version, read before the rest so a line of another format counts as one.
#[derive(Deserialize)]
struct Version {
    v: u32,
}

/// What a check's pools.jsonl line (version 1) holds that the re-test reads (pools.rs `Pooled`).
#[derive(Deserialize)]
struct Line {
    check: String,
    session: String,
    ms: i64,
    #[serde(default)]
    failed: bool,
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    role: Vec<u8>,
    #[serde(default)]
    flags: Vec<u8>,
    #[serde(default)]
    grade: Vec<u8>,
    #[serde(default)]
    w: Vec<f64>,
    #[serde(default)]
    trust: Vec<f64>,
}

impl Line {
    fn aligned(&self) -> bool {
        let n = self.ids.len();
        [self.role.len(), self.flags.len(), self.grade.len(), self.w.len(), self.trust.len()].iter().all(|&l| l == n)
    }
}

/// An eligible row of a check: its lesson, the composite's factor and scope rule, and its graded gain.
struct Row {
    lesson: usize,
    factor: f64,
    scoped_out: bool,
    grade: u8,
}

impl Row {
    fn gain(&self) -> f64 {
        2f64.powi(i32::from(self.grade)) - 1.0
    }
}

/// The rows the check showed, judged now (not pending) and not already in play or delivered, whose lesson
/// the store still holds; `missing` counts shown rows whose lesson it does not.
fn rows(p: &Line, corpus: &Corpus, missing: &mut usize) -> Vec<Row> {
    let mut out = Vec::new();
    for (i, id) in p.ids.iter().enumerate() {
        if p.role[i] == pools::NOT_SHOWN || p.flags[i] & pools::PENDING != 0 {
            continue;
        }
        let Some(&lesson) = corpus.at.get(id) else {
            *missing += 1;
            continue;
        };
        if p.flags[i] & (pools::IN_PLAY | pools::DELIVERED) != 0 {
            continue;
        }
        let scoped_out = !corpus.challenges[lesson].scope.is_empty() && p.flags[i] & pools::IN_SCOPE == 0;
        out.push(Row { lesson, factor: p.w[i] * p.trust[i] / trust::NEUTRAL, scoped_out, grade: p.grade[i] });
    }
    out
}

fn dcg(gains: &[f64]) -> f64 {
    gains.iter().take(DEPTH).enumerate().map(|(r, g)| g / ((r + 2) as f64).log2()).sum()
}

/// NDCG@`DEPTH` of gains in ranked order; None when no gain is positive.
fn ndcg(ranked: &[f64]) -> Option<f64> {
    let mut ideal = ranked.to_vec();
    ideal.sort_by(|a, b| b.total_cmp(a));
    let idcg = dcg(&ideal);
    (idcg > 0.0).then(|| dcg(ranked) / idcg)
}

/// NDCG of the rows ranked by composite (lexical × w × trust / 0.5, 0 when scoped out), ties in row order.
fn ndcg_of(rows: &[Row], lexical: &[f64]) -> Option<f64> {
    let composite: Vec<f64> = rows.iter().map(|r| if r.scoped_out { 0.0 } else { lexical[r.lesson] * r.factor }).collect();
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| composite[b].total_cmp(&composite[a]));
    ndcg(&order.iter().map(|&i| rows[i].gain()).collect::<Vec<_>>())
}

/// The session state files, each read once.
struct States {
    dir: PathBuf,
    texts: HashMap<String, Option<String>>,
}

impl States {
    /// The state file before the check's `#borg-checking @<check>` line.
    fn context(&mut self, session: &str, check: &str) -> Option<&str> {
        let dir = &self.dir;
        let text = self.texts.entry(session.to_string()).or_insert_with(|| std::fs::read_to_string(dir.join(format!("{session}.ctx"))).ok()).as_deref()?;
        let marker = format!("#borg-checking @{check}");
        let mut at = 0;
        for line in text.split_inclusive('\n') {
            let l = line.trim_end_matches('\n');
            if l.strip_prefix(marker.as_str()).is_some_and(|rest| rest.is_empty() || rest.starts_with(' ')) {
                return Some(&text[..at]);
            }
            at += line.len();
        }
        None
    }
}

#[derive(Default)]
struct Counts {
    lines: usize,
    written: usize,
    unparsed: usize,
    version: usize,
    failed: usize,
    no_context: usize,
    missing_ids: usize,
}

/// A line's check and its context, or None (counted) when it cannot be scored.
fn usable<'a>(line: &str, states: &'a mut States, counts: &mut Counts) -> Option<(Line, &'a str)> {
    let Ok(Version { v }) = serde_json::from_str::<Version>(line) else {
        counts.unparsed += 1;
        return None;
    };
    if v != 1 {
        counts.version += 1;
        return None;
    }
    let Some(p) = serde_json::from_str::<Line>(line).ok().filter(Line::aligned) else {
        counts.unparsed += 1;
        return None;
    };
    if p.failed {
        counts.failed += 1;
        return None;
    }
    let Some(context) = states.context(&p.session, &p.check) else {
        counts.no_context += 1;
        return None;
    };
    Some((p, context))
}

fn scratch() -> PathBuf {
    std::env::temp_dir().join(format!("hs-pools-{}", std::process::id()))
}

fn create(path: &Path) -> BufWriter<std::fs::File> {
    BufWriter::new(std::fs::File::create(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
}

/// The check's output line: per config, NDCG@10 of its pool and the gate score of its turn.
async fn score_check(corpus: &Corpus, e: &Engines, ids: &[String], p: &Line, context: &str, missing: &mut usize) -> Value {
    let gate_text = conscience::work_text(prefilter::turn(context)).into_owned();
    let keep = corpus.gate.keep(&corpus.challenges, &gate_text);
    let lexical = e.lexical(&[conscience::work_text(context).into_owned(), gate_text]).await;
    let rows = rows(p, corpus, missing);
    let pos = rows.iter().filter(|r| r.grade >= 2).count();
    let judged = rows.iter().any(|r| r.grade > 0);
    let mut ndcgs = Map::new();
    let mut gates = Map::new();
    for (id, l) in ids.iter().zip(&lexical) {
        if judged {
            ndcgs.insert(id.clone(), json!(ndcg_of(&rows, &l[0])));
        }
        gates.insert(id.clone(), json!(corpus.gate_score(&l[1], &keep)));
    }
    let ndcg = if judged { Value::Object(ndcgs) } else { Value::Null };
    json!({"store": corpus.store, "session": p.session, "check": p.check, "ms": p.ms, "eligible": rows.len(), "pos": pos,
           "pos_check": pos > 0, "ndcg": ndcg, "gate": gates})
}

async fn score(dir: &Path, out: &Path) {
    let corpus = Corpus::load(dir);
    let grid = grid();
    let ids: Vec<String> = grid.iter().map(Config::id).collect();
    let meta = json!({"configs": ids, "baseline": BASELINE});
    std::fs::write(format!("{}.configs.json", out.display()), meta.to_string()).expect("configs");
    let tmp = scratch();
    let engines = Engines::new(&corpus, &grid, Source::Build(&tmp)).await;
    let mut states = States { dir: dir.join("state"), texts: HashMap::new() };
    let mut counts = Counts::default();
    let mut f = create(out);
    let pools = std::fs::read_to_string(dir.join("pools.jsonl")).unwrap_or_default();
    for line in pools.lines() {
        counts.lines += 1;
        if counts.lines % 100 == 0 {
            eprintln!("{} lines, {} written", counts.lines, counts.written);
        }
        let Some((p, context)) = usable(line, &mut states, &mut counts) else { continue };
        let row = score_check(&corpus, &engines, &ids, &p, context, &mut counts.missing_ids).await;
        writeln!(f, "{row}").expect("write");
        counts.written += 1;
    }
    f.flush().expect("write");
    engines.close().await;
    let _ = std::fs::remove_dir_all(&tmp);
    let c = counts;
    eprintln!(
        "lines={} written={} unparsed={} version={} failed={} no_context={} missing_ids={}",
        c.lines, c.written, c.unparsed, c.version, c.failed, c.no_context, c.missing_ids
    );
}

/// Byte offsets of the lines that close a turn (prefilter.rs `MARKERS`; an ask followed by `#borg-live`
/// came during the turn, as `prefilter::turn` reads it).
fn turn_ends(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut at = 0;
    let mut lines = text.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        let live = lines.peek().is_some_and(|next| next.starts_with("#borg-live "));
        if prefilter::MARKERS.iter().any(|m| line.starts_with(m)) && !live {
            out.push(at);
        }
        at += line.len();
    }
    out
}

fn ctx_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "ctx")).collect())
        .unwrap_or_default();
    files.sort();
    files
}

async fn turns(dir: &Path, out: &Path, cfgs: &[Config]) {
    let corpus = Corpus::load(dir);
    let ids: Vec<String> = cfgs.iter().map(Config::id).collect();
    let tmp = scratch();
    let engines = Engines::new(&corpus, cfgs, Source::Build(&tmp)).await;
    let mut f = create(out);
    let (mut files, mut written) = (0, 0);
    for path in ctx_files(&dir.join("state")) {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let session = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        for (i, at) in turn_ends(&text).into_iter().enumerate() {
            let gate_text = conscience::work_text(prefilter::turn(&text[..at])).into_owned();
            let keep = corpus.gate.keep(&corpus.challenges, &gate_text);
            let lexical = engines.lexical(&[gate_text]).await;
            let scores: Map<String, Value> = ids.iter().zip(&lexical).map(|(id, l)| (id.clone(), json!(corpus.gate_score(&l[0], &keep)))).collect();
            writeln!(f, "{}", json!({"store": corpus.store, "session": session, "i": i, "scores": scores})).expect("write");
            written += 1;
        }
        files += 1;
    }
    f.flush().expect("write");
    engines.close().await;
    let _ = std::fs::remove_dir_all(&tmp);
    eprintln!("files={files} turns={written}");
}

/// A SeekStorm config's index at `index` (replaced), as `score` builds it; nothing for a borg config.
async fn build(dir: &Path, cfg: &Config, index: &Path) {
    if let Engine::Seek { sim, fields } = cfg.engine {
        let corpus = Corpus::load(dir);
        seek::build_tokens(&corpus.lessons, index, cfg.rule, fields, sim.similarity()).await.expect("build");
    }
}

async fn query(dir: &Path, cfg: &Config, query: &Path, index: Option<&Path>) {
    let text = std::fs::read_to_string(query).expect("query file");
    let corpus = Corpus::load(dir);
    let source = match (cfg.engine, index) {
        (Engine::Borg { .. }, _) => Source::Open(Path::new(".")),
        (Engine::Seek { .. }, Some(index)) => Source::Open(index),
        (Engine::Seek { .. }, None) => usage(),
    };
    let engines = Engines::new(&corpus, std::slice::from_ref(cfg), source).await;
    let scores = engines.lexical(&[conscience::work_text(&text).into_owned()]).await.remove(0).remove(0);
    let mut ranked: Vec<(usize, f64)> = scores.into_iter().enumerate().filter(|(_, s)| *s > 0.0).collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut out = std::io::stdout().lock();
    for (l, s) in ranked.into_iter().take(TOP) {
        let _ = writeln!(out, "{}\t{s:.4}", corpus.challenges[l].id);
    }
    drop(out);
    engines.close().await;
}

fn usage() -> ! {
    eprintln!(
        "usage: hs-pools score <collective dir> <out.jsonl> | turns <collective dir> <out.jsonl> <cfg>... | build <collective dir> <cfg> <index dir> | query <collective dir> <cfg> <query file> [<index dir>]"
    );
    std::process::exit(2)
}

fn config(id: &str) -> Config {
    Config::parse(id).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2)
    })
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = |i: usize| Path::new(&args[i]);
    match (args.get(1).map(String::as_str), args.len()) {
        (Some("score"), 4) => score(path(2), path(3)).await,
        (Some("turns"), 5..) => turns(path(2), path(3), &args[4..].iter().map(|s| config(s)).collect::<Vec<_>>()).await,
        (Some("build"), 5) => build(path(2), &config(&args[3]), path(4)).await,
        (Some("query"), 5 | 6) => query(path(2), &config(&args[3]), path(4), args.get(5).map(Path::new)).await,
        _ => usage(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ndcg_at_10_of_gains_0_3_1() {
        // DCG = 0/log2 2 + 3/log2 3 + 1/log2 4; IDCG = 3/log2 2 + 1/log2 3.
        let want = (3.0 / 3f64.log2() + 0.5) / (3.0 + 1.0 / 3f64.log2());
        let got = ndcg(&[0.0, 3.0, 1.0]).unwrap();
        assert!((got - want).abs() < 1e-12, "{got} vs {want}");
        assert!((got - 0.6590018048024133).abs() < 1e-12);
        assert_eq!(ndcg(&[0.0, 0.0]), None);
    }

    #[test]
    fn the_grid_has_216_borg_and_72_seek_configs_and_the_baseline() {
        let ids: Vec<String> = grid().iter().map(Config::id).collect();
        assert_eq!(ids.len(), 288);
        assert_eq!(ids.iter().filter(|i| i.starts_with("borg/")).count(), 216);
        assert!(ids[..216].iter().all(|i| i.starts_with("borg/")));
        assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 288);
        assert!(ids.contains(&BASELINE.to_string()));
        for id in &ids {
            assert_eq!(&Config::parse(id).unwrap().id(), id);
        }
        let base = Config::parse(BASELINE).unwrap();
        assert_eq!((base.rule, base.cut), (RANKING.rule, RANKING.df_cut));
        assert_eq!(base.engine, Engine::Borg { k1: RANKING.k1, b: RANKING.b, fields: RANKING.fields });
    }

    fn challenge(id: &str, question: &str) -> Challenge {
        Challenge { id: id.into(), question: question.into(), ..Challenge::default() }
    }

    #[tokio::test]
    async fn baseline_rows_equal_conscience_raw() {
        let mut ch = vec![
            challenge("a", "Is the payment retry idempotent?"),
            challenge("b", "Does the payment retry carry an idempotency key?"),
            challenge("c", "Is the Orion config validated before deploy?"),
            challenge("d", "Are amounts sent in cents?"),
            challenge("e", "Does ForkInput reset the arcs after a retry?"),
        ];
        ch[1].supersedes = vec!["a".into()];
        ch[2].scope = vec!["Orion".into()];
        ch[3].facets = vec!["payment amounts in minor units".into()];
        ch[4].situations = vec!["a payment retry".into()];
        let corpus = Corpus::of("t".into(), ch.clone());
        let base = Config::parse(BASELINE).unwrap();
        let engines = Engines::new(&corpus, &[base], Source::Open(Path::new("."))).await;
        let context = "we touched the payment retry\n#borg-skip 1\nthe orion config and payment amounts, retry idempotent\n#borg-p x\nforkInput\n";
        let pool = conscience::work_text(context).into_owned();
        let gate = conscience::work_text(prefilter::turn(context)).into_owned();
        let lexical = engines.lexical(&[pool.clone(), gate.clone()]).await.remove(0);
        let want = conscience::raw(&ch, &pool);
        assert!(want.iter().filter(|s| **s > 0.0).count() >= 3);
        for (got, want) in lexical[0].iter().zip(&want) {
            assert!((got - want).abs() < 1e-9, "{got} vs {want}");
        }
        let keep = corpus.gate.keep(&ch, &gate);
        let (got, want) = (corpus.gate_score(&lexical[1], &keep), prefilter::score(&ch, context));
        assert!(want > 0.0 && (got - want).abs() < 1e-9, "gate {got} vs {want}");
    }
}
