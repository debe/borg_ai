//! Arms C, D and E: SeekStorm embedded in the process (lexical BM25F, vector with Model2Vec, hybrid RRF),
//! and the token index of the internal evaluation "seekstorm-bm25" (not published): borg's tokens
//! indexed whitespace-separated, so SeekStorm ranks exactly the words borg's BM25 does.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use borg::conscience::{TokenRule, tokens_with};
use seekstorm::commit::Commit;
use seekstorm::index::{
    AccessType, Close, Clustering, Document, DocumentCompression, FieldType, FrequentwordType, IndexArc, IndexDocuments, IndexMetaObject, LexicalSimilarity,
    NgramSet, SchemaField, StemmerType, StopwordType, TokenizerType, create_index, open_index,
};
use seekstorm::search::{QueryRewriting, QueryType, ResultType, Search, SearchMode};
use seekstorm::vector::{Embedding, Inference, Quantization};
use seekstorm::vector_similarity::AnnMode;
// SeekStorm's segment number bits: borg vectors.rs's, so the eval measures the product's index layout.
use borg::vectors::SEGMENT_BITS;
use serde_json::Value;

use crate::Lesson;

pub const FIELDS: [&str; 4] = ["question", "situations", "consequence", "scope"];
const IDS: &str = "borg-ids.json";
/// The most terms one SeekStorm union query scores exactly. SeekStorm cuts a query after 100 terms
/// (`MAX_QUERY_TERM_NUMBER`, seekstorm 3.3.13 `index.rs`), but its union over more than 8 terms
/// (`union_scan_32`, `union.rs`) keeps only 32 of the terms the index holds, and documents gain score from
/// words they lack beyond them (measured: 50 known terms gave 103.9 where BM25 gives 64.5). So
/// `search_tokens` searches in chunks of 32. Protocol.
const MAX_QUERY_TERMS: usize = 32;
/// Model2Vec's chunk size for embedding a field. Experiment parameter: the value of SeekStorm's README
/// example.
const CHUNK: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Lexical,
    Vector,
    Hybrid,
}

impl Mode {
    pub fn parse(s: &str) -> Mode {
        match s {
            "lex" => Mode::Lexical,
            "vec" => Mode::Vector,
            "hyb" => Mode::Hybrid,
            _ => panic!("mode: lex | vec | hyb"),
        }
    }

    fn search_mode(self) -> SearchMode {
        match self {
            Mode::Lexical => SearchMode::Lexical,
            Mode::Vector => SearchMode::Vector { similarity_threshold: None, ann_mode: AnnMode::All },
            Mode::Hybrid => SearchMode::Hybrid { similarity_threshold: None, ann_mode: AnnMode::All },
        }
    }
}

/// The model directory under `HS_MODELS` (default ~/.local/share/borg/eval/hybrid-search/models).
pub fn model_dir(name: &str) -> PathBuf {
    let base = std::env::var_os("HS_MODELS").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".local/share/borg").join(["ev", "al"].concat()).join("hybrid-search/models")
    });
    base.join(name)
}

/// The index settings every index here shares: one name, tokenizer, similarity and inference apart.
fn meta(name: &str, tokenizer: TokenizerType, sim: LexicalSimilarity, inference: Inference) -> IndexMetaObject {
    IndexMetaObject {
        id: 0,
        name: name.into(),
        lexical_similarity: sim,
        tokenizer,
        stemmer: StemmerType::None,
        stop_words: StopwordType::None,
        frequent_words: FrequentwordType::None,
        ngram_indexing: NgramSet::SingleTerm as u8,
        document_compression: DocumentCompression::Snappy,
        access_type: AccessType::Mmap,
        spelling_correction: None,
        query_completion: None,
        clustering: Clustering::None,
        inference,
    }
}

/// Create the index at `dir` (replacing it), index `docs` (one per lesson, in order), commit, close, and
/// write the lesson ids to the sidecar in index order. One shard: the corpora are small, and every shard
/// is a thread and files to open at query time.
async fn create(dir: &Path, meta: IndexMetaObject, schema: Vec<SchemaField>, docs: Vec<Document>, lessons: &[Lesson]) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let index = create_index(dir, meta, &schema, &Vec::new(), SEGMENT_BITS, true, Some(1)).await?;
    index.index_documents(docs).await;
    index.commit().await;
    index.close().await;
    let ids: Vec<&str> = lessons.iter().map(|l| l.id.as_str()).collect();
    std::fs::write(dir.join(IDS), serde_json::to_string(&ids).unwrap()).map_err(|e| e.to_string())
}

/// Build a persisted index of the lessons at `dir` (replacing it). `model` = a Model2Vec directory for
/// vector fields, None for a lexical-only index.
pub async fn build(lessons: &[Lesson], dir: &Path, model: Option<&Path>) -> Result<(), String> {
    let vector = model.is_some();
    let mut schema = vec![SchemaField::new("id".into(), true, false, false, FieldType::Text, false, false, 1.0, false, false)];
    for f in FIELDS {
        schema.push(SchemaField::new(f.into(), false, true, vector, FieldType::Text, false, false, 1.0, false, false));
    }
    let inference = match model {
        Some(p) => Inference::Model2VecCustom { path: p.to_string_lossy().into(), chunk_size: CHUNK, quantization: Quantization::None },
        None => Inference::None,
    };
    let docs: Vec<Document> = lessons
        .iter()
        .map(|l| {
            Document::from([
                ("id".to_string(), Value::String(l.id.clone())),
                ("question".to_string(), Value::String(l.question.clone())),
                ("situations".to_string(), Value::String(l.situations.join("\n"))),
                ("consequence".to_string(), Value::String(l.consequence.clone())),
                ("scope".to_string(), Value::String(l.scope.join(" "))),
            ])
        })
        .collect();
    create(dir, meta("borg-lessons", TokenizerType::UnicodeAlphanumeric, LexicalSimilarity::Bm25f, inference), schema, docs, lessons).await
}

/// Which lesson fields a token index holds: the question; or question, situations and consequence as
/// three fields of weight 1; or the same with situations and consequence at boost 0.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SeekFields {
    Q,
    Qsc,
    QscHalf,
}

impl SeekFields {
    /// The fields besides the question and their boost (`build_tokens`).
    fn rest(self) -> Option<f32> {
        match self {
            SeekFields::Q => None,
            SeekFields::Qsc => Some(1.0),
            SeekFields::QscHalf => Some(0.5),
        }
    }
}

/// A query as token-index terms: `tokens_with(text, rule)`, each once in first-occurrence order, minus `drop`.
pub fn token_query(text: &str, rule: TokenRule, drop: &HashSet<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    tokens_with(text, rule).into_iter().filter(|t| !drop.contains(t) && seen.insert(t.clone())).collect()
}

/// Build a token index of the lessons at `dir` (replacing it): each field holds its text's
/// `tokens_with(.., rule)` joined by spaces (situations joined by newlines first), indexed by SeekStorm's
/// whitespace tokenizer under `sim`. A lesson without tokens stays an empty document, so doc ids keep
/// matching the sidecar.
pub async fn build_tokens(lessons: &[Lesson], dir: &Path, rule: TokenRule, fields: SeekFields, sim: LexicalSimilarity) -> Result<(), String> {
    let field = |name: &str, boost: f32| SchemaField::new(name.into(), false, true, false, FieldType::Text, false, false, boost, false, false);
    let mut schema = vec![SchemaField::new("id".into(), true, false, false, FieldType::Text, false, false, 1.0, false, false), field("question", 1.0)];
    if let Some(boost) = fields.rest() {
        schema.extend([field("situations", boost), field("consequence", boost)]);
    }
    let text = |t: &str| Value::String(tokens_with(t, rule).join(" "));
    let docs: Vec<Document> = lessons
        .iter()
        .map(|l| {
            let mut d = Document::from([("id".to_string(), Value::String(l.id.clone())), ("question".to_string(), text(&l.question))]);
            if fields.rest().is_some() {
                d.insert("situations".to_string(), text(&l.situations.join("\n")));
                d.insert("consequence".to_string(), text(&l.consequence));
            }
            d
        })
        .collect();
    create(dir, meta("borg-lessons-tokens", TokenizerType::WhitespaceLowercase, sim, Inference::None), schema, docs, lessons).await
}

/// Every (doc id, score) of a token-index search for `terms` (`token_query`), at most `k` per chunk of
/// `MAX_QUERY_TERMS`, scores summed per document over the chunks (BM25 adds over disjoint terms; the
/// proximity similarity misses pairs across a chunk border), best first, then by doc id.
pub async fn search_tokens(index: &IndexArc, terms: &[String], k: usize) -> Vec<(usize, f32)> {
    let mut sum: HashMap<usize, f32> = HashMap::new();
    for chunk in terms.chunks(MAX_QUERY_TERMS) {
        for (d, s) in hits(index, chunk.join(" "), None, SearchMode::Lexical, k).await {
            *sum.entry(d).or_default() += s;
        }
    }
    let mut out: Vec<(usize, f32)> = sum.into_iter().collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

pub async fn open(dir: &Path) -> Result<(IndexArc, Vec<String>), String> {
    let ids: Vec<String> = serde_json::from_str(&std::fs::read_to_string(dir.join(IDS)).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok((open_index(dir).await?, ids))
}

/// The lexical query text: words only (SeekStorm reads `+ - "` as operators), order and repeats kept, so
/// the built-in embedding sees the same words in the same order.
pub fn query_text(text: &str) -> String {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Top `k` (doc id, score). `vector` = a query embedding computed outside SeekStorm (the full-length
/// variant); None lets SeekStorm embed the query text itself (Model2Vec `encode`: first 512 tokens).
pub async fn search(index: &IndexArc, query: &str, mode: Mode, vector: Option<Vec<f32>>, k: usize) -> Vec<(usize, f32)> {
    hits(index, query_text(query), vector.map(Embedding::F32), mode.search_mode(), k).await
}

/// Top `k` (doc id, score) of one union query, as SeekStorm parses `query`.
async fn hits(index: &IndexArc, query: String, vector: Option<Embedding>, mode: SearchMode, k: usize) -> Vec<(usize, f32)> {
    let r = index
        .search(
            query,
            vector,
            QueryType::Union,
            mode,
            false,
            0,
            k,
            ResultType::Topk,
            false,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            QueryRewriting::SearchOnly,
        )
        .await;
    r.results.iter().map(|x| (x.doc_id, x.score)).collect()
}

/// The stored lesson id of a document (to check the sidecar's order).
pub async fn stored_id(index: &IndexArc, doc_id: usize) -> Option<String> {
    let fields: HashSet<String> = ["id".to_string()].into();
    let doc = index.read().await.get_document(doc_id, false, &None, &fields, &[]).await.ok()?;
    doc.get("id").and_then(Value::as_str).map(str::to_string)
}

/// Full-length query embedding with the same model files (no 512-token cut), for the descriptive `f` arms.
pub struct Embedder(model2vec_rs::model::StaticModel);

impl Embedder {
    pub fn load(dir: &Path) -> Embedder {
        Embedder(model2vec_rs::model::StaticModel::from_pretrained(dir, None, None, None).expect("model"))
    }

    pub fn embed(&self, text: &str) -> Vec<f32> {
        self.0.encode_with_args(&[query_text(text)], None, 1024).remove(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use borg::conscience::{Bm25, Compound, Fields, LEGACY, bm25_with};

    fn lesson(id: &str, question: &str) -> Lesson {
        Lesson { id: id.into(), origin_ms: 0, question: question.into(), situations: vec![], consequence: String::new(), scope: vec![], facets: vec![] }
    }

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("borg-seek-{}-{name}", std::process::id()))
    }

    #[tokio::test]
    async fn tokens_rank_as_borg_bm25() {
        let lessons = vec![
            lesson("a", "Is the RISK_URL read from config.yaml?"),
            lesson("b", "Does ForkInput reset the arcs?"),
            lesson("c", "Are amounts sent in cents?"),
        ];
        let query = "risk_url forkInput config reset";
        for (n, rule) in [LEGACY, TokenRule { min_len: 3, compound: Compound::Code }].into_iter().enumerate() {
            let dir = scratch(&format!("rank{n}"));
            build_tokens(&lessons, &dir, rule, SeekFields::Q, LexicalSimilarity::Bm25f).await.unwrap();
            let (index, ids) = open(&dir).await.unwrap();
            let hits = search_tokens(&index, &token_query(query, rule, &HashSet::new()), lessons.len()).await;
            let docs: Vec<Vec<String>> = lessons.iter().map(|l| tokens_with(&l.question, rule)).collect();
            let p = Bm25 { k1: 1.2, b: 0.75, df_cut: None, rule, fields: Fields::Question };
            let mut want: Vec<(usize, f64)> = bm25_with(&docs, query, &p).into_iter().enumerate().filter(|(_, s)| *s > 0.0).collect();
            want.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), want.iter().map(|w| w.0).collect::<Vec<_>>(), "{rule:?}: {hits:?} vs {want:?}");
            for ((d, s), (_, w)) in hits.iter().zip(&want) {
                assert!((f64::from(*s) - w).abs() <= 0.05 * w, "{rule:?}: doc {d} {s} vs {w}");
                assert_eq!(stored_id(&index, *d).await.as_deref(), Some(ids[*d].as_str()));
            }
            index.close().await;
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[tokio::test]
    async fn search_tokens_sums_chunks() {
        // Every one of the 250 query terms is in the index, so each chunk's union holds the most terms it can.
        let word = |j: usize| format!("word{j:03}");
        let mut lessons: Vec<Lesson> = (0..10)
            .map(|i| {
                let words: Vec<String> = (0..250).filter(|j| j % 10 == i || j % 13 == i).map(word).collect();
                lesson(&format!("l{i}"), &words.join(" "))
            })
            .collect();
        lessons.push(lesson("none", "nothing matches here"));
        let dir = scratch("chunks");
        build_tokens(&lessons, &dir, LEGACY, SeekFields::Q, LexicalSimilarity::Bm25f).await.unwrap();
        let (index, _) = open(&dir).await.unwrap();
        let terms: Vec<String> = (0..250).map(word).collect();
        let mut by_hand: HashMap<usize, f32> = HashMap::new();
        for chunk in terms.chunks(MAX_QUERY_TERMS) {
            for (d, s) in hits(&index, chunk.join(" "), None, SearchMode::Lexical, lessons.len()).await {
                *by_hand.entry(d).or_default() += s;
            }
        }
        let summed = search_tokens(&index, &terms, lessons.len()).await;
        assert_eq!(summed.len(), 10);
        assert_eq!(summed.len(), by_hand.len());
        let docs: Vec<Vec<String>> = lessons.iter().map(|l| tokens_with(&l.question, LEGACY)).collect();
        let p = Bm25 { k1: 1.2, b: 0.75, df_cut: None, rule: LEGACY, fields: Fields::Question };
        let bm25 = bm25_with(&docs, &terms.join(" "), &p);
        for (d, s) in summed {
            assert!((s - by_hand[&d]).abs() <= 1e-5 * s, "doc {d}: {s} vs {}", by_hand[&d]);
            // a union over more than 32 known terms would score words the document lacks
            assert!((f64::from(s) - bm25[d]).abs() <= 0.05 * bm25[d], "doc {d}: {s} vs BM25 {}", bm25[d]);
        }
        index.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
