//! The pool's shadow log (0.7.7): what each detached check (check_async.rs) showed its model, and how
//! other orders would have ranked the same lessons, one line per check in `pools.jsonl` beside the
//! store's outcomes.
//!
//! The stop pool fills its budget in one fixed order (whole.rs `select_filled`: the shortlist, then BM25 ×
//! weight × trust, then newest first). Whether another order finds more of what the judge applies can
//! only be measured on lessons the model was shown, so each check logs, per eligible lesson in its fill
//! order (`Pooled::ids`), its role (prefix, boundary draw, tail sample, not shown), its exact chance to
//! be shown (`pi`: 1 in the prefix, pt + (1 − pt)·π_b past it, whole.rs `explore`) and the scores of the
//! candidate orders the pre-registered switch test compares (internal evaluation "learn-retrieval", not
//! published):
//!
//! - `bm25q`: today's word match (conscience.rs `raw`), before weight and trust; `w`, `trust`, `act`:
//!   the memory weight, the authors' trust, the activation behind the weight (memory.rs).
//! - `bm25f`: BM25 of question, situations and consequence against the whole work, idf over the store's
//!   lessons at the stop (E3-fb).
//! - `cue`: the best BM25 of the lesson's retrieval cues (cues.rs) against the work's tail, idf over every
//!   cue of the store (E5); null without cues.
//! - `mv`: BM25 of situations and cues against the design moves of the session's latest check that
//!   finished before this stop (predict.rs `moves`; E6); null without such moves.
//! - `q`: the lesson's online value from its earlier verdicts, Q ← Q + 0.2(r − Q), changed +1, ignored
//!   −0.5, prior 0, placebos skipped (E2).
//! - `cos`: the cosine of the lesson's vector to the work's tail (vectors.rs `scored`); null without
//!   vectors.
//! - `rrf`: reciprocal rank fusion of bm25f, cue, mv (bm25f where mv is null) and q over the rows the
//!   model was shown (E7): Σ 1/(60 + rank), ranks 1-based, ties at their mean, a missing score last.
//!
//! **Logged only.** None of these reaches the pool, its order or the prompt (the shadow-purity test
//! pins it); the order changes only when the switch test passes. Local: never synced (borg-mesh
//! `SYNCED`), never shared, kept whole until that test reads it (its size is in `borg status`). Nothing
//! with learning off. The miss causes in `borg status` and `borg web` fold from it (`misses`).

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read, Seek};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::conscience::{self, Challenge};

/// Protocol: the version of a `pools.jsonl` line's format (`Pooled::v`).
pub const VERSION: u32 = 1;

/// The log beside a store's outcomes.
#[must_use]
pub fn path(home: &Path) -> PathBuf {
    home.join("pools.jsonl")
}

/// RRF's rank offset. Method: the standard constant (Cormack, Clarke and Büttcher, "Reciprocal Rank
/// Fusion outperforms Condorcet and individual Rank Learning Methods", 2009), as the offline scorer.
pub const RRF_K: f64 = 60.0;

/// The online value's step and rewards (`q`). Method: the offline scorer's E2, fixed before the switch
/// test was registered: a verdict moves Q a fifth of the way; a change is worth +1, an ignored question
/// −0.5; unclear and not arisen leave it.
pub const Q_STEP: f64 = 0.2;
pub const Q_CHANGED: f64 = 1.0;
pub const Q_IGNORED: f64 = -0.5;

/// `Pooled::flags` bits: in the shortlist (BM25's top `vectors::SHORTLIST` plus vector hits, computed
/// even when the pool fits), added by the vectors, in scope for where the session works, delivered in
/// the session already (hook.rs `had_in`), pending a verdict this check judges, in play (applied by an
/// earlier check of the session, or delivered).
pub const SHORTLIST: u8 = 1;
pub const VECTOR: u8 = 2;
pub const IN_SCOPE: u8 = 4;
pub const DELIVERED: u8 = 8;
pub const PENDING: u8 = 16;
pub const IN_PLAY: u8 = 32;

/// `Pooled::role` values.
pub const NOT_SHOWN: u8 = 0;
pub const PREFIX: u8 = 1;
pub const BOUNDARY: u8 = 2;
pub const TAIL: u8 = 3;

/// One check's line of `pools.jsonl`: per-check fields, then columns aligned with `ids` (every eligible
/// lesson in fill order: the prefix's order, then the lessons it dropped as the fill met them), floats
/// at 4 decimals, null where a score does not apply.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Pooled {
    pub v: u32,
    pub check: String,
    pub session: String,
    pub ms: i64,
    /// The check's index among the session's checks that called the model (check_async.rs `index`).
    pub idx: u64,
    /// The lesson line format (whole.rs `Line`), the model and its prompt template (predict.rs
    /// `template_hash`); whether the call failed (no applies then).
    pub line: String,
    pub model: String,
    pub prompt_hash: String,
    pub failed: bool,
    /// The pool's budget B in tokens, the prefix's lessons, the exploration share s, τ, the tail's k of
    /// the n the prefix left, and pt = k / n (whole.rs `Explored`).
    pub budget: usize,
    pub prefix: usize,
    pub s: f64,
    pub tau: usize,
    pub k: usize,
    pub n_rem: usize,
    pub pt: f64,
    /// The check whose moves `mv` read; None without.
    pub moves_from: Option<String>,
    pub ids: Vec<String>,
    /// `id@v<version>#<key>`: which wording was shown (cues.rs `key`).
    pub ver: Vec<String>,
    pub role: Vec<u8>,
    pub pb: Vec<Option<f64>>,
    pub pi: Vec<f64>,
    pub bm25q: Vec<f64>,
    pub bm25f: Vec<Option<f64>>,
    pub cos: Vec<Option<f64>>,
    pub w: Vec<f64>,
    pub trust: Vec<f64>,
    pub act: Vec<f64>,
    pub q: Vec<f64>,
    pub cue: Vec<Option<f64>>,
    pub mv: Vec<Option<f64>>,
    pub rrf: Vec<Option<f64>>,
    pub flags: Vec<u8>,
    /// The grade the model gave each lesson it applied, 0 for the rest: the miss causes' positives.
    pub grade: Vec<u8>,
}

/// What the pool step knows of its rows (hook.rs `pool`): the eligible lessons in fill order, their
/// roles and chances, today's word match, and the exploration's sizes.
#[derive(Debug, Clone)]
pub struct Rows<'a> {
    pub rows: Vec<&'a Challenge>,
    pub prefix: usize,
    pub role: Vec<u8>,
    pub pb: Vec<Option<f64>>,
    pub pi: Vec<f64>,
    pub bm25q: Vec<f64>,
    pub budget: usize,
    pub s: f64,
    pub tau: usize,
    pub k: usize,
    pub n_rem: usize,
    pub pt: f64,
}

impl<'a> Rows<'a> {
    /// The rows of the pool `e` filled from `challenges`, whose word matches are `raw` (conscience.rs
    /// `raw`, aligned with `challenges`), within `budget` tokens.
    #[must_use]
    pub fn of(e: &crate::whole::Explored<'a>, challenges: &[Challenge], raw: &[f64], budget: usize) -> Rows<'a> {
        let prefix = e.filled.order.len();
        let rows: Vec<&'a Challenge> = e.filled.order.iter().chain(&e.filled.dropped).copied().collect();
        let mut role = vec![NOT_SHOWN; rows.len()];
        role[..prefix].fill(PREFIX);
        for (at, _) in &e.boundary {
            role[*at] = BOUNDARY;
        }
        for (at, _) in &e.tail {
            role[*at] = TAIL;
        }
        let (pb_r, pi_r) = e.chances();
        let pb = std::iter::repeat_n(None, prefix).chain(pb_r).collect();
        let pi = std::iter::repeat_n(1.0, prefix).chain(pi_r).collect();
        let by: HashMap<&str, f64> = challenges.iter().zip(raw).map(|(c, r)| (c.id.as_str(), *r)).collect();
        let bm25q = rows.iter().map(|c| by.get(c.id.as_str()).copied().unwrap_or(0.0)).collect();
        Rows { rows, prefix, role, pb, pi, bm25q, budget, s: e.s, tau: e.tau, k: e.k, n_rem: e.n, pt: e.pt }
    }
}

/// What the check adds after its call (hook.rs `settle`).
pub struct Check<'c> {
    pub session: &'c str,
    pub check: &'c str,
    pub idx: u64,
    /// When its stop ran: the store, the verdicts and the moves it reads are those before it.
    pub stop_ms: i64,
    pub line: &'c str,
    pub model: &'c str,
    pub prompt_hash: &'c str,
    pub failed: bool,
    /// The applies with their grades; the lessons whose verdict it judged.
    pub applied: &'c [(&'c Challenge, u8)],
    pub pending: &'c [String],
    /// The work the pool read (conscience.rs `work_text`), where the session works, the lessons the
    /// check could ask.
    pub text: &'c str,
    pub scope_text: &'c str,
    pub challenges: &'c [Challenge],
    /// The lessons delivered in the session, and those in play (delivered or applied by an earlier check).
    pub delivered: &'c HashSet<&'c str>,
    pub in_play: &'c HashSet<&'c str>,
    /// The store's lessons file, and the outcomes the verdicts and the moves are read from.
    pub lessons: &'c Path,
    pub outcomes: &'c Path,
}

/// Appends the check's line (`pooled`) to `pools.jsonl` in `home`. Nothing with learning off; fails
/// quietly (a log, never in the way of the check).
pub fn log(home: &Path, rows: &Rows, c: &Check) {
    if !crate::learn::learning() {
        return;
    }
    if let Ok(line) = serde_json::to_string(&pooled(rows, c)) {
        let _ = crate::learn::append_line(&path(home), &line);
    }
}

/// Rounded to 4 decimals, as every float of the log (never `-0`).
fn r4(x: f64) -> f64 {
    (x * 1e4).round() / 1e4 + 0.0
}

/// The check's line: `rows` scored as the module doc says.
#[must_use]
pub fn pooled(rows: &Rows, c: &Check) -> Pooled {
    // The store as the session's lessons came from it: the team's and this engineer's own (hook.rs
    // `loaded`), and the cues written before the stop.
    let corpus = conscience::load_store_unweighted(c.lessons, Some(c.stop_ms));
    let cues = crate::cues::load_before(c.lessons, Some(c.stop_ms));
    let cues_of = |l: &Challenge| cues.get(&crate::cues::key(&l.question, &l.situations, &l.consequence));
    let (q, moves) = read_outcomes(c.outcomes, c.session, c.stop_ms, &rows.rows.iter().map(|l| l.id.as_str()).collect());
    let at: HashMap<&str, usize> = corpus.iter().enumerate().map(|(i, l)| (l.id.as_str(), i)).collect();
    let in_corpus = |scores: &[f64]| -> Vec<Option<f64>> { rows.rows.iter().map(|l| at.get(l.id.as_str()).map(|&i| scores[i])).collect() };
    // E3-fb: one document per lesson of the store.
    let fb_docs: Vec<Vec<String>> =
        corpus.iter().map(|l| conscience::tokens(&format!("{}\n{}\n{}", l.question, l.situations.join("\n"), l.consequence))).collect();
    let bm25f = in_corpus(&conscience::bm25(&fb_docs, c.text));
    // E5: each cue its own document; a lesson scores its best cue.
    let tail = crate::vectors::query_tail(c.text);
    let (mut cue_docs, mut owner) = (Vec::new(), Vec::new());
    for (i, l) in corpus.iter().enumerate() {
        for cue in cues_of(l).into_iter().flatten() {
            cue_docs.push(conscience::tokens(cue));
            owner.push(i);
        }
    }
    let mut best: Vec<Option<f64>> = vec![None; corpus.len()];
    for (s, i) in conscience::bm25(&cue_docs, &tail).into_iter().zip(owner) {
        best[i] = Some(best[i].map_or(s, |b: f64| b.max(s)));
    }
    let cue: Vec<Option<f64>> = rows.rows.iter().map(|l| at.get(l.id.as_str()).and_then(|&i| best[i])).collect();
    // E6: situations and cues against the latest earlier check's moves.
    let mv: Vec<Option<f64>> = match &moves {
        Some((_, said)) => {
            let docs: Vec<Vec<String>> = corpus
                .iter()
                .map(|l| conscience::tokens(&l.situations.iter().chain(cues_of(l).into_iter().flatten()).cloned().collect::<Vec<_>>().join("\n")))
                .collect();
            in_corpus(&conscience::bm25(&docs, &said.join("\n")))
        }
        None => vec![None; rows.rows.len()],
    };
    let qs: Vec<f64> = rows.rows.iter().map(|l| q.get(l.id.as_str()).copied().unwrap_or(0.0)).collect();
    // Vectors: every lesson the index holds, so each eligible row has its cosine.
    let n = corpus.len() + c.challenges.len();
    let hits = crate::vectors::scored(c.lessons, &tail, n);
    let cos: Vec<Option<f64>> = match &hits {
        Some(h) => {
            let by: HashMap<&str, f32> = h.iter().map(|(id, s)| (id.as_str(), *s)).collect();
            rows.rows.iter().map(|l| by.get(l.id.as_str()).map(|s| f64::from(*s))).collect()
        }
        None => vec![None; rows.rows.len()],
    };
    // The shortlist as the fill would take it (vectors.rs `shortlist`), from the same hits.
    let ranked: Vec<String> =
        hits.iter().flatten().take(crate::vectors::k() + crate::vectors::SHORTLIST + crate::vectors::OVER_FETCH).map(|(id, _)| id.clone()).collect();
    let (list, added) = crate::vectors::shortlist_from(c.challenges, c.text, c.scope_text, crate::vectors::SHORTLIST, &ranked);
    let listed: HashSet<&str> = list.iter().map(|l| l.id.as_str()).collect();
    let vector: HashSet<&str> = list[list.len() - added..].iter().map(|l| l.id.as_str()).collect();
    let have = conscience::Names::of(c.scope_text);
    let pending: HashSet<&str> = c.pending.iter().map(String::as_str).collect();
    let flags = rows
        .rows
        .iter()
        .map(|l| {
            let id = l.id.as_str();
            [
                (listed.contains(id), SHORTLIST),
                (vector.contains(id), VECTOR),
                (conscience::in_scope(l, &have), IN_SCOPE),
                (c.delivered.contains(id), DELIVERED),
                (pending.contains(id), PENDING),
                (c.in_play.contains(id), IN_PLAY),
            ]
            .iter()
            .filter(|(on, _)| *on)
            .fold(0, |f, (_, bit)| f | bit)
        })
        .collect();
    // E7 over the rows the model was shown.
    let shown: Vec<usize> = (0..rows.rows.len()).filter(|&i| rows.role[i] != NOT_SHOWN).collect();
    let pick = |v: &[Option<f64>]| shown.iter().map(|&i| v[i]).collect::<Vec<_>>();
    let mv_or_fb: Vec<Option<f64>> = if moves.is_some() { mv.clone() } else { bm25f.clone() };
    let lists = [pick(&bm25f), pick(&cue), pick(&mv_or_fb), shown.iter().map(|&i| Some(qs[i])).collect()];
    let mut rrf = vec![None; rows.rows.len()];
    let fused = lists.iter().map(|l| ranks(l)).fold(vec![0.0; shown.len()], |acc, r| acc.iter().zip(&r).map(|(a, r)| a + 1.0 / (RRF_K + r)).collect());
    for (&i, f) in shown.iter().zip(fused) {
        rrf[i] = Some(r4(f));
    }
    let grade = rows.rows.iter().map(|l| c.applied.iter().find(|(a, _)| a.id == l.id).map_or(0, |(_, g)| *g)).collect();
    let opt = |v: Vec<Option<f64>>| v.into_iter().map(|x| x.map(r4)).collect::<Vec<_>>();
    Pooled {
        v: VERSION,
        check: c.check.into(),
        session: c.session.into(),
        ms: crate::learn::now_ms(),
        idx: c.idx,
        line: c.line.into(),
        model: c.model.into(),
        prompt_hash: c.prompt_hash.into(),
        failed: c.failed,
        budget: rows.budget,
        prefix: rows.prefix,
        s: rows.s,
        tau: rows.tau,
        k: rows.k,
        n_rem: rows.n_rem,
        pt: rows.pt,
        moves_from: moves.map(|(check, _)| check),
        ids: rows.rows.iter().map(|l| l.id.clone()).collect(),
        ver: rows.rows.iter().map(|l| format!("{}@v{}#{}", l.id, l.version, crate::cues::key(&l.question, &l.situations, &l.consequence))).collect(),
        role: rows.role.clone(),
        pb: opt(rows.pb.clone()),
        pi: rows.pi.iter().map(|x| r4(*x)).collect(),
        bm25q: rows.bm25q.iter().map(|x| r4(*x)).collect(),
        bm25f: opt(bm25f),
        cos: opt(cos),
        w: rows.rows.iter().map(|l| r4(l.weight)).collect(),
        trust: rows.rows.iter().map(|l| r4(l.trust)).collect(),
        act: rows.rows.iter().map(|l| r4(l.activation)).collect(),
        q: qs.into_iter().map(r4).collect(),
        cue: opt(cue),
        mv: opt(mv),
        rrf,
        flags,
        grade,
    }
}

/// 1-based ranks of `v`, best (largest) first, ties at their mean rank, a missing value below every
/// present one (tied with the other missing ones), as the offline scorer ranks.
#[must_use]
pub fn ranks(v: &[Option<f64>]) -> Vec<f64> {
    let key = |x: &Option<f64>| x.unwrap_or(f64::NEG_INFINITY);
    let mut sorted: Vec<f64> = v.iter().map(key).collect();
    sorted.sort_by(|a, b| b.total_cmp(a));
    v.iter()
        .map(|x| {
            let x = key(x);
            let lo = sorted.partition_point(|s| *s > x);
            let hi = sorted.partition_point(|s| *s >= x);
            (lo + 1 + hi) as f64 / 2.0
        })
        .collect()
}

/// A `graded` record's check and its design moves (predict.rs `moves`).
type Moves = (String, Vec<String>);

/// From the outcomes at `log`, read line by line: each of `rows`' Q before `stop_ms` (strictly, in time
/// order: changed +1, ignored −0.5, placebos and ignores by a judge before the never-came-up verdict
/// skipped, outcome.rs `ambiguous`), and the moves of `session`'s latest `graded`
/// record with moves that finished before `stop_ms`, with its check.
fn read_outcomes(log: &Path, session: &str, stop_ms: i64, rows: &HashSet<&str>) -> (HashMap<String, f64>, Option<Moves>) {
    #[derive(Deserialize)]
    struct Line {
        kind: String,
        #[serde(default)]
        session: String,
        #[serde(default)]
        ms: i64,
        #[serde(default)]
        lessons: Vec<String>,
        #[serde(default)]
        channel: String,
        #[serde(default)]
        check: String,
        #[serde(default)]
        moves: Vec<String>,
        #[serde(default)]
        judge: u8,
    }
    let mut verdicts: Vec<(i64, f64, Vec<String>)> = Vec::new();
    let mut moves: Option<(i64, String, Vec<String>)> = None;
    if let Ok(f) = std::fs::File::open(log) {
        for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
            let verdict = line.contains(crate::outcome::CHANGED) || line.contains(crate::outcome::IGNORED);
            if !verdict && !line.contains("\"moves\"") {
                continue;
            }
            let Ok(o) = serde_json::from_str::<Line>(&line) else { continue };
            if o.ms >= stop_ms {
                continue;
            }
            let r = match o.kind.as_str() {
                crate::outcome::CHANGED => Q_CHANGED,
                crate::outcome::IGNORED => Q_IGNORED,
                crate::outcome::GRADED if o.session == session && !o.moves.is_empty() => {
                    if moves.as_ref().is_none_or(|(ms, _, _)| o.ms >= *ms) {
                        moves = Some((o.ms, o.check, o.moves));
                    }
                    continue;
                }
                _ => continue,
            };
            if o.channel == crate::outcome::PLACEBO || crate::outcome::ambiguous(&o.kind, o.judge) {
                continue;
            }
            let mine: Vec<String> = o.lessons.into_iter().filter(|l| rows.contains(l.as_str())).collect();
            if !mine.is_empty() {
                verdicts.push((o.ms, r, mine));
            }
        }
    }
    verdicts.sort_by_key(|(ms, _, _)| *ms);
    let mut q: HashMap<String, f64> = HashMap::new();
    for (_, r, ls) in verdicts {
        for l in ls {
            let v = q.entry(l).or_insert(0.0);
            *v += Q_STEP * (r - *v);
        }
    }
    (q, moves.map(|(_, check, said)| (check, said)))
}

/// The miss causes (`borg status`, `borg web`): of the first applies at grade ≥ 2 the checks logged
/// (not pending, not in play), those the fill order ranked low, at a fill position past half the
/// prefix or shown only by a draw (boundary or tail), each with its mechanical cause (`cause`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Misses {
    /// The bytes of `pools.jsonl` folded (the cache's offset) and its size.
    pub at: u64,
    /// The checks folded, the positives among their rows, and the low ones (`causes` counts them).
    pub checks: u64,
    pub positives: u64,
    pub low: u64,
    /// Per cause (`CAUSES`), the low positives it explains.
    pub causes: Vec<(String, u64)>,
    /// Of the low positives, those shown by a draw past the prefix.
    pub drawn: u64,
}

/// The mechanical causes, in the order `cause` tries them: out of scope for where the session works;
/// no word of the work matched it but the vectors found it; no word matched; its words matched in the
/// pool's top half but weight and trust put it in the bottom half (demoted); its words matched less
/// than half the pool's (weak words); otherwise (the shortlist, ties, newest first).
pub const CAUSES: [&str; 6] = ["scope", "vector_only", "no_word_match", "demoted", "weak_words", "other"];

/// The cache of `misses` beside the log.
fn cache(home: &Path) -> PathBuf {
    home.join("misses.json")
}

/// The miss causes of the store `home` (`Misses`): the cache, then the log's lines past its offset folded
/// in and the cache saved again; a log shorter than the cache's offset (replaced) is folded anew.
#[must_use]
pub fn misses(home: &Path) -> Misses {
    let log = path(home);
    let len = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
    let mut m: Misses = std::fs::read_to_string(cache(home)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if m.at > len || m.causes.len() != CAUSES.len() {
        m = Misses { causes: CAUSES.iter().map(|c| (c.to_string(), 0)).collect(), ..Misses::default() };
    }
    if m.at == len {
        return m;
    }
    let Ok(mut f) = std::fs::File::open(&log) else { return m };
    if f.seek(std::io::SeekFrom::Start(m.at)).is_err() {
        return m;
    }
    let mut r = std::io::BufReader::new(f.by_ref());
    let mut buf = String::new();
    while r.read_line(&mut buf).is_ok_and(|n| n > 0) {
        // A line still being written is folded at the next read.
        if !buf.ends_with('\n') {
            break;
        }
        m.at += buf.len() as u64;
        if let Ok(p) = serde_json::from_str::<Pooled>(&buf) {
            fold(&mut m, &p);
        }
        buf.clear();
    }
    if let Ok(text) = serde_json::to_string(&m) {
        let tmp = cache(home).with_extension(format!("json.{}", std::process::id()));
        if std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, cache(home))).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    m
}

/// One check's line folded into `m`.
fn fold(m: &mut Misses, p: &Pooled) {
    m.checks += 1;
    for i in 0..p.ids.len() {
        let (g, f) = (p.grade.get(i).copied().unwrap_or(0), p.flags.get(i).copied().unwrap_or(0));
        if g < 2 || f & (PENDING | IN_PLAY) != 0 {
            continue;
        }
        m.positives += 1;
        let role = p.role.get(i).copied().unwrap_or(NOT_SHOWN);
        if !(role == BOUNDARY || role == TAIL || (role == PREFIX && 2 * i >= p.prefix)) {
            continue;
        }
        m.low += 1;
        m.drawn += u64::from(role != PREFIX);
        let why = cause(p, i);
        if let Some(c) = m.causes.iter_mut().find(|(c, _)| c == why) {
            c.1 += 1;
        }
    }
}

/// Why row `i` of `p` sat low in the fill order (`CAUSES`), from its logged scores; the rank shares are
/// within the check's rows, 1-based ranks over their number, ties at their mean (the offline miss report).
#[must_use]
pub fn cause(p: &Pooled, i: usize) -> &'static str {
    let f = p.flags.get(i).copied().unwrap_or(0);
    let bm25 = p.bm25q.get(i).copied().unwrap_or(0.0);
    if f & IN_SCOPE == 0 {
        return "scope";
    }
    if bm25 <= 0.0 {
        return if f & VECTOR != 0 { "vector_only" } else { "no_word_match" };
    }
    // Every column read by row, missing cells as 0 (a line shorter in one column never panics a reader).
    let n = p.ids.len().max(i + 1);
    let share = |v: Vec<Option<f64>>| ranks(&v)[i] / n as f64;
    let q = |j: usize| p.bm25q.get(j).copied().unwrap_or(0.0);
    let words = share((0..n).map(|j| Some(q(j))).collect());
    let score = share((0..n).map(|j| Some(q(j) * p.w.get(j).unwrap_or(&1.0) * p.trust.get(j).unwrap_or(&0.5) / crate::trust::NEUTRAL)).collect());
    if words <= 0.5 && score > 0.5 {
        "demoted"
    } else if words > 0.5 {
        "weak_words"
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line whose columns are shorter than its ids (an edited file, a later format) gives a cause, never
    /// a panic in `borg status` or the web's selection view.
    #[test]
    fn a_short_column_never_panics_the_cause() {
        let p = Pooled { ids: vec!["A".into(), "B".into(), "C".into()], bm25q: vec![1.0], flags: vec![IN_SCOPE; 3], ..Default::default() };
        assert_eq!(cause(&p, 2), "no_word_match");
        let p = Pooled { bm25q: vec![1.0, 2.0], ..p };
        assert!(CAUSES.contains(&cause(&p, 1)));
    }

    #[test]
    fn ranks_are_one_based_descending_with_mean_ties_and_missing_last() {
        assert_eq!(ranks(&[Some(3.0), Some(1.0), Some(3.0), None, Some(2.0), None]), [1.5, 4.0, 1.5, 5.5, 3.0, 5.5]);
        assert!(ranks(&[]).is_empty());
    }

    /// Q from the verdicts before the stop, in time order, placebos and other lessons skipped; the
    /// moves of the session's latest check that finished before it.
    #[test]
    fn q_and_moves_are_read_as_of_the_stop() {
        let d = std::env::temp_dir().join(format!("borg-pools-outcomes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let log = d.join("outcomes.jsonl");
        let lines = [
            r#"{"kind":"asked_ignored","session":"x","ms":20,"lessons":["A"]}"#,
            r#"{"kind":"asked_ignored","session":"x","ms":30,"lessons":["A"],"judge":3}"#,
            r#"{"kind":"asked_then_changed","session":"x","ms":10,"lessons":["A","Z"]}"#,
            r#"{"kind":"asked_then_changed","session":"x","ms":20,"lessons":["A"],"channel":"placebo"}"#,
            r#"{"kind":"asked_then_changed","session":"x","ms":99,"lessons":["A"]}"#,
            r#"{"kind":"graded","session":"s","ms":40,"check":"c2","moves":["later"]}"#,
            r#"{"kind":"graded","session":"s","ms":35,"check":"c1","moves":["earlier"]}"#,
            r#"{"kind":"graded","session":"t","ms":45,"check":"c3","moves":["another session"]}"#,
            r#"{"kind":"graded","session":"s","ms":60,"check":"c4","moves":["after the stop"]}"#,
        ];
        for l in lines {
            crate::learn::append_line(&log, l).unwrap();
        }
        let rows: HashSet<&str> = ["A", "B"].into();
        let (q, moves) = read_outcomes(&log, "s", 50, &rows);
        // A: changed at 10 (0 → 0.2), ignored at 30 (0.2 + 0.2·(−0.5 − 0.2) = 0.06); the placebo and the
        // verdict after the stop count for nothing, nor does the ignore at 20 by a judge before 0.7.5.
        assert!((q["A"] - 0.06).abs() < 1e-12, "{q:?}");
        assert!(!q.contains_key("B") && !q.contains_key("Z"));
        assert_eq!(moves, Some(("c2".to_string(), vec!["later".to_string()])));
        assert_eq!(read_outcomes(&log, "s", 36, &rows).1.map(|m| m.0), Some("c1".into()));
        assert_eq!(read_outcomes(&log, "s", 35, &rows).1, None, "strictly before the stop");
        let _ = std::fs::remove_dir_all(&d);
    }

    fn row(p: &mut Pooled, id: &str, role: u8, bm25: f64, w: f64, flags: u8, grade: u8) {
        p.ids.push(id.into());
        p.role.push(role);
        p.bm25q.push(bm25);
        p.w.push(w);
        p.trust.push(0.5);
        p.flags.push(flags);
        p.grade.push(grade);
    }

    /// A check's low first applies get their causes; pending and in-play applies and those high in
    /// the fill are no misses; the fold resumes at its offset.
    #[test]
    fn misses_fold_the_low_first_applies_by_cause() {
        let mut p = Pooled { prefix: 6, ..Pooled::default() };
        row(&mut p, "top", PREFIX, 9.0, 1.0, IN_SCOPE, 3); // high in the fill: no miss
        row(&mut p, "a", PREFIX, 8.0, 1.0, IN_SCOPE | PENDING, 3); // pending
        row(&mut p, "b", PREFIX, 7.0, 1.0, IN_SCOPE | IN_PLAY, 2); // in play
        row(&mut p, "dem", PREFIX, 6.0, 0.01, IN_SCOPE, 2); // words in the top half, weight sinks it
        row(&mut p, "x", PREFIX, 5.0, 1.0, IN_SCOPE, 0);
        row(&mut p, "vec", PREFIX, 0.0, 1.0, IN_SCOPE | VECTOR, 2);
        row(&mut p, "weak", TAIL, 0.5, 1.0, IN_SCOPE, 2);
        row(&mut p, "none", BOUNDARY, 0.0, 1.0, IN_SCOPE, 3);
        row(&mut p, "low1", NOT_SHOWN, 0.1, 1.0, IN_SCOPE, 0);
        let home = std::env::temp_dir().join(format!("borg-pools-misses-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let line = serde_json::to_string(&p).unwrap();
        crate::learn::append_line(&path(&home), &line).unwrap();
        let m = misses(&home);
        let by = |c: &str| m.causes.iter().find(|(x, _)| x == c).unwrap().1;
        assert_eq!((m.checks, m.positives, m.low, m.drawn), (1, 5, 4, 2), "{m:?}");
        assert_eq!((by("demoted"), by("vector_only"), by("weak_words"), by("no_word_match"), by("other"), by("scope")), (1, 1, 1, 1, 0, 0));
        // The next read folds only what was appended.
        crate::learn::append_line(&path(&home), &line).unwrap();
        let m2 = misses(&home);
        assert_eq!((m2.checks, m2.low, m2.at), (2, 8, std::fs::metadata(path(&home)).unwrap().len()));
        let _ = std::fs::remove_dir_all(&home);
    }
}
