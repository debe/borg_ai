//! Arms C, D and E: SeekStorm embedded in the process (lexical BM25F, vector with Model2Vec, hybrid RRF).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

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

/// Build a persisted index of the lessons at `dir` (replacing it). `model` = a Model2Vec directory for
/// vector fields, None for a lexical-only index. One shard: the corpora are small, and every shard is a
/// thread and files to open at query time. Lesson ids go to a sidecar in index order.
pub async fn build(lessons: &[Lesson], dir: &Path, model: Option<&Path>) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let vector = model.is_some();
    let mut schema = vec![SchemaField::new("id".into(), true, false, false, FieldType::Text, false, false, 1.0, false, false)];
    for f in FIELDS {
        schema.push(SchemaField::new(f.into(), false, true, vector, FieldType::Text, false, false, 1.0, false, false));
    }
    let meta = IndexMetaObject {
        id: 0,
        name: "borg-lessons".into(),
        lexical_similarity: LexicalSimilarity::Bm25f,
        tokenizer: TokenizerType::UnicodeAlphanumeric,
        stemmer: StemmerType::None,
        stop_words: StopwordType::None,
        frequent_words: FrequentwordType::None,
        ngram_indexing: NgramSet::SingleTerm as u8,
        document_compression: DocumentCompression::Snappy,
        access_type: AccessType::Mmap,
        spelling_correction: None,
        query_completion: None,
        clustering: Clustering::None,
        inference: match model {
            Some(p) => Inference::Model2VecCustom { path: p.to_string_lossy().into(), chunk_size: CHUNK, quantization: Quantization::None },
            None => Inference::None,
        },
    };
    let index = create_index(dir, meta, &schema, &Vec::new(), SEGMENT_BITS, true, Some(1)).await?;
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
    index.index_documents(docs).await;
    index.commit().await;
    index.close().await;
    let ids: Vec<&str> = lessons.iter().map(|l| l.id.as_str()).collect();
    std::fs::write(dir.join(IDS), serde_json::to_string(&ids).unwrap()).map_err(|e| e.to_string())
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
    let r = index
        .search(
            query_text(query),
            vector.map(Embedding::F32),
            QueryType::Union,
            mode.search_mode(),
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
