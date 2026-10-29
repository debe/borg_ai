//! Vector recall for the Stop step (internal evaluation "vectors", not published).
//!
//! Word matching (BM25) shortlists the lessons the consequence model reads at a turn end. It misses lessons
//! that share no rare word with the work: paraphrased or abstract ones, German work against English
//! lessons, renamed identifiers. Vectors add those, and only those:
//!
//! - **Where:** only on the Stop path, after the free gate passed and right before the consequence model
//!   call (seconds), where loading the embedder is small next to it. Prompt, early delivery and the free
//!   gate stay BM25 only.
//! - **How:** no rank fusion. The shortlist is BM25's top 20 as before, plus up to `k` vector hits not
//!   already in it, under the same scope rule and done set (`extend`). The model decides relevance;
//!   vectors only add recall. R3b's widening stays BM25-based.
//! - **Index:** an embedded SeekStorm vector index per store (`<store>/vectors/`), one vector per lesson
//!   (question + situations + consequence), updated after `borg learn` / `borg assimilate --apply`, after a
//!   mesh sync brings new lessons, and by `borg vectors --rebuild`; a missing index is built by the first
//!   update. A lesson without a vector is simply BM25-only.
//! - **Model:** Model2Vec potion-multilingual-128M from a local directory (`BORG_VECTOR_MODEL`), fetched
//!   once by `borg vectors --fetch`; never downloaded inside a hook. The embedding matrix is mapped, not
//!   read: a query touches only the rows of its own tokens.
//! - **Fail open:** no model, no index, a locked index or any error = BM25 only. On by default:
//!   `borg doctor` warns about a missing model or index (doctor.rs); `BORG_VECTORS=off` disables it; a lean
//!   build (`--no-default-features`) never has it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::conscience::{self, Challenge, Names};
use crate::trust;

pub mod doctor;
pub mod model;
#[cfg(feature = "vectors")]
mod unigram;

pub use model::{MODEL_FILES, MODEL_REPO, MODEL_REVISION, enabled, index_dir, model_dir, model_present};

/// Vector hits added to the shortlist at most (`BORG_VECTOR_K`). Baseline: the internal evaluation
/// "vectors" setting; with the whole-store pool (whole.rs) the shortlist only orders what fills the
/// budget when the pool overflows.
pub const DEFAULT_K: usize = 10;

/// BM25 lessons the Stop's consequence model sees before vector additions. Baseline: top 30 found as
/// much as top 20 plus vectors (internal evaluation "vectors"); with the whole-store pool it is the
/// overflow order's head (whole.rs `select`).
pub const SHORTLIST: usize = 30;

/// Vector hits fetched beyond `k` + the shortlist (`shortlist`). Guard: done, out-of-scope and
/// BM25-listed hits are dropped after the search, so a search of exactly `k` could come back short.
const OVER_FETCH: usize = 30;

/// SeekStorm's segment number bits of the index (2^11 segments). Method: SeekStorm's own default for an
/// index of this size; borg-search's evaluation builds its indexes with the same layout.
pub const SEGMENT_BITS: usize = 11;

/// How many vector hits the Stop step adds at most (`BORG_VECTOR_K`, default `DEFAULT_K`).
pub fn k() -> usize {
    std::env::var("BORG_VECTOR_K").ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_K)
}

/// The text a lesson is embedded from: question, situations, consequence.
pub fn lesson_text(c: &Challenge) -> String {
    format!("{}\n{}\n{}", c.question, c.situations.join("\n"), c.consequence)
}

/// The Stop shortlist: `bm25` (BM25's top 20, as `select_scoped` returns it) and then, in vector order,
/// up to `k` lessons of `ranked` (lesson ids, best first) that `bm25` lacks. Each added lesson must be in
/// `pool` (the lessons not done this session), in scope for `scope_text` (`rank_scoped`'s rule), not
/// superseded and not dormant (a dormant lesson needs a strong word match, which a vector hit is not).
pub fn extend<'a>(bm25: Vec<&'a Challenge>, ranked: &[String], pool: &'a [Challenge], scope_text: &str, k: usize) -> Vec<&'a Challenge> {
    if k == 0 || ranked.is_empty() {
        return bm25;
    }
    let have = Names::of(scope_text);
    let successors = trust::successors(pool);
    let mut seen: HashSet<&str> = bm25.iter().map(|c| c.id.as_str()).collect();
    let mut out = bm25;
    let mut added = 0;
    for id in ranked {
        if added == k {
            break;
        }
        let Some(c) = pool.iter().find(|c| &c.id == id) else { continue };
        if seen.contains(c.id.as_str()) || !conscience::in_scope(c, &have) || c.dormant || successors.contains_key(&c.id) {
            continue;
        }
        seen.insert(c.id.as_str());
        out.push(c);
        added += 1;
    }
    out
}

/// The Stop step's shortlist for the lessons file `lessons`: BM25's top `n` over `context` (scoped by
/// `scope_text`) extended by the vector index's hits (`extend`). Without vectors (off, no model, no index,
/// any error) it is exactly BM25's top `n`. Also returns how many lessons the vectors added.
pub fn shortlist<'a>(lessons: &Path, pool: &'a [Challenge], context: &str, scope_text: &str, n: usize) -> (Vec<&'a Challenge>, usize) {
    let bm25 = conscience::select_scoped(pool, context, scope_text, n);
    let k = k();
    if !enabled() || k == 0 {
        return (bm25, 0);
    }
    // Ask for more than `k`: done, out-of-scope and BM25-listed hits are dropped after the search.
    let ranked = recall(lessons, &query_tail(context), k + n + OVER_FETCH);
    let before = bm25.len();
    let out = extend(bm25, &ranked, pool, scope_text, k);
    let added = out.len() - before;
    (out, added)
}

/// What the consequence model reads of the work (predict.rs `prompt`): the last `TAIL` characters.
pub fn query_tail(context: &str) -> String {
    let chars: Vec<char> = context.chars().collect();
    chars[chars.len().saturating_sub(crate::predict::TAIL)..].iter().collect()
}

/// The ids of the lessons nearest to `text`, best first, at most `n`; empty whenever vectors are not
/// available (fail open: no model, no index, the index being written, any error or panic).
pub fn recall(lessons: &Path, text: &str, n: usize) -> Vec<String> {
    #[cfg(feature = "vectors")]
    {
        if !enabled() {
            return Vec::new();
        }
        let (index, model) = (index_dir(lessons), model_dir());
        if !index.join(engine::MANIFEST).is_file() || !model_present(&model) {
            return Vec::new();
        }
        let text = text.to_string();
        std::panic::catch_unwind(move || engine::query(&index, &model, &text, n).unwrap_or_default()).unwrap_or_default()
    }
    #[cfg(not(feature = "vectors"))]
    {
        let _ = (lessons, text, n);
        Vec::new()
    }
}

/// Bring the index of `lessons` up to date after lessons were written (`borg learn`, `borg assimilate
/// --apply`, a mesh sync), building it if it is missing (the first lesson after `--fetch` needs no
/// `--rebuild`). Quiet and fail-open: without the model or with vectors off it does nothing; an error is
/// one line on stderr. Returns the number of lessons embedded.
pub fn refresh(lessons: &Path) -> usize {
    #[cfg(feature = "vectors")]
    {
        if !enabled() || !lessons.is_file() || !model_present(&model_dir()) {
            return 0;
        }
        let lessons = lessons.to_path_buf();
        match std::panic::catch_unwind(move || engine::update(&lessons, &model_dir(), false)) {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                eprintln!("borg vectors: index not updated ({e}); `borg vectors --rebuild` retries");
                0
            }
            Err(_) => 0,
        }
    }
    #[cfg(not(feature = "vectors"))]
    {
        let _ = lessons;
        0
    }
}

#[cfg(feature = "vectors")]
const USAGE: &str = "usage: borg vectors [--store LESSONS.jsonl] [--rebuild] | borg vectors --fetch";

/// `borg vectors`: status of the model and the store's index; `--rebuild` (re)builds the index;
/// `--fetch` downloads the model (pinned revision, sha256 checked); `--query` (reached only as
/// `borg debug vectors --query`) prints the Stop step's shortlist for a context on stdin (offline
/// checks, latency). The exit code.
pub fn main(args: &[&str]) -> i32 {
    let flag = |f: &str| args.iter().position(|a| *a == f).and_then(|i| args.get(i + 1)).copied();
    // Without --store: the collective the current directory routes to (as the hooks do).
    let store = flag("--store").map(PathBuf::from).unwrap_or_else(|| {
        let cwd = std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default();
        let base = crate::learn::home();
        match crate::collective::load().ok().flatten() {
            Some(cfg) => match crate::collective::route(Some(&cfg), &cwd, &base) {
                crate::collective::Route::Collective { home, .. } => home.join("lessons.jsonl"),
                crate::collective::Route::Off => base.join("lessons.jsonl"),
            },
            None => base.join("lessons.jsonl"),
        }
    });
    #[cfg(not(feature = "vectors"))]
    {
        let _ = store;
        eprintln!("borg vectors: this borg is a lean build (--no-default-features); reinstall without that flag: cargo install borg-ai --locked --force");
        1
    }
    #[cfg(feature = "vectors")]
    {
        if args.contains(&"--fetch") {
            return match engine::fetch(&model_dir()) {
                Ok(()) => {
                    println!("model ready at {}", model_dir().display());
                    0
                }
                Err(e) => {
                    eprintln!("borg vectors --fetch: {e}");
                    1
                }
            };
        }
        if args.contains(&"--query") {
            use std::io::Read;
            let mut context = String::new();
            let _ = std::io::stdin().read_to_string(&mut context);
            let pool = conscience::load(&store, None);
            let scope_text = flag("--scope").map(str::to_string).unwrap_or_else(|| context.clone());
            let t = std::time::Instant::now();
            let (list, added) = shortlist(&store, &pool, &context, &scope_text, SHORTLIST);
            for (i, c) in list.iter().enumerate() {
                println!("{}\t{}", c.id, if i >= list.len() - added { "vector" } else { "bm25" });
            }
            eprintln!("{} bm25 + {added} vector in {:.1} ms", list.len() - added, t.elapsed().as_secs_f64() * 1000.0);
            return 0;
        }
        if args.iter().any(|a| a.starts_with("--") && !["--store", "--rebuild", "--scope"].contains(a)) {
            eprintln!("{USAGE}");
            return 2;
        }
        let model = model_dir();
        if !model_present(&model) {
            println!("model: missing at {} (`borg vectors --fetch`); the Stop step uses BM25 only", model.display());
            return i32::from(args.contains(&"--rebuild"));
        }
        println!("model: {}", model.display());
        if !enabled() {
            println!("vectors: off (BORG_VECTORS=off)");
        }
        if args.contains(&"--rebuild") {
            let t = std::time::Instant::now();
            return match engine::update(&store, &model, true) {
                Ok(n) => {
                    println!("index rebuilt: {n} lessons embedded in {:.2} s ({})", t.elapsed().as_secs_f64(), index_dir(&store).display());
                    0
                }
                Err(e) => {
                    eprintln!("borg vectors --rebuild: {e}");
                    1
                }
            };
        }
        match engine::status(&store, &model) {
            Ok(s) => println!("{s}"),
            Err(e) => println!("index: {e}"),
        }
        0
    }
}

#[cfg(feature = "vectors")]
pub mod engine {
    //! The embedder (Model2Vec on a mapped model file) and the SeekStorm index.

    use std::collections::BTreeMap;
    use std::fs::File;
    use std::path::Path;

    use seekstorm::commit::Commit;
    use seekstorm::index::{
        AccessType, Close, Clustering, Document, DocumentCompression, FrequentwordType, IndexDocuments, IndexMetaObject, LexicalSimilarity, NgramSet,
        SchemaField, StemmerType, StopwordType, TokenizerType, create_index, open_index,
    };
    use seekstorm::search::{QueryRewriting, QueryType, ResultType, Search, SearchMode};
    use seekstorm::vector::{Embedding, Inference, Precision, Quantization};
    use seekstorm::vector_similarity::{AnnMode, VectorSimilarity};
    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    pub use super::model::MANIFEST;
    use super::model::identity;
    use super::{MODEL_FILES, MODEL_REPO, MODEL_REVISION, lesson_text};
    use crate::conscience;

    #[derive(Serialize, Deserialize, Default)]
    struct Manifest {
        /// The model's identity (`identity`); another model's vectors are not comparable.
        model: String,
        dims: usize,
        /// Lesson id -> hash of the text its vector was made from.
        lessons: BTreeMap<String, String>,
    }

    /// A Model2Vec static model: tokenizer + embedding matrix, mean pooling, normalised. The matrix stays
    /// in the mapped file; only the rows of a text's tokens are read.
    pub struct Embedder {
        tokenizer: Tok,
        map: memmap2::Mmap,
        embeddings: Tensor,
        weights: Option<Tensor>,
        mapping: Option<Tensor>,
        unk: Option<u32>,
        normalize: bool,
    }

    #[allow(clippy::large_enum_variant)] // one per process
    enum Tok {
        /// Unigram models (potion-multilingual-128M): the fast path (unigram.rs).
        Fast(super::unigram::Unigram),
        Full(tokenizers::Tokenizer),
    }

    struct Tensor {
        dtype: safetensors::Dtype,
        shape: Vec<usize>,
        start: usize,
    }

    impl Embedder {
        pub fn load(dir: &Path) -> Result<Embedder, String> {
            let json = std::fs::read_to_string(dir.join("tokenizer.json")).map_err(|e| format!("tokenizer: {e}"))?;
            let tokenizer = match super::unigram::Unigram::parse(&json) {
                Some(u) => Tok::Fast(u),
                None => Tok::Full(json.parse().map_err(|e| format!("tokenizer: {e}"))?),
            };
            let file = File::open(dir.join("model.safetensors")).map_err(|e| format!("model: {e}"))?;
            // SAFETY: the model file is read-only data that borg never writes; `--fetch` replaces it by rename.
            let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| format!("model: {e}"))?;
            let (header, meta) = safetensors::SafeTensors::read_metadata(&map).map_err(|e| format!("model: {e}"))?;
            let tensor = |name: &str| meta.info(name).map(|i| Tensor { dtype: i.dtype, shape: i.shape.clone(), start: 8 + header + i.data_offsets.0 });
            let embeddings = tensor("embeddings").or_else(|| tensor("0")).or_else(|| tensor("embedding.weight")).ok_or("model: no embeddings tensor")?;
            if embeddings.shape.len() != 2 || size(embeddings.dtype).is_none() {
                return Err("model: unsupported embeddings tensor".into());
            }
            let config: Value = std::fs::read(dir.join("config.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
            let unk = match &tokenizer {
                Tok::Full(t) => match t.get_model() {
                    tokenizers::ModelWrapper::WordPiece(m) => t.token_to_id(&m.unk_token),
                    tokenizers::ModelWrapper::WordLevel(m) => t.token_to_id(&m.unk_token),
                    tokenizers::ModelWrapper::BPE(m) => m.unk_token.as_deref().and_then(|u| t.token_to_id(u)),
                    tokenizers::ModelWrapper::Unigram(_) => None,
                },
                // As model2vec-rs: a Unigram model names no unk token, and no token is dropped.
                Tok::Fast(_) => None,
            };
            Ok(Embedder {
                tokenizer,
                embeddings,
                weights: tensor("weights"),
                mapping: tensor("mapping"),
                map,
                unk,
                normalize: config["normalize"].as_bool().unwrap_or(true),
            })
        }

        /// The text's token ids (no special tokens).
        pub fn ids(&self, text: &str) -> Option<Vec<u32>> {
            match &self.tokenizer {
                Tok::Fast(u) => u.ids(text),
                Tok::Full(t) => t.encode_fast(text, false).ok().map(|e| e.get_ids().to_vec()),
            }
        }

        pub fn dims(&self) -> usize {
            self.embeddings.shape[1]
        }

        fn value(&self, t: &Tensor, i: usize) -> Option<f64> {
            let w = size(t.dtype)?;
            let b = self.map.get(t.start + i * w..t.start + (i + 1) * w)?;
            Some(match t.dtype {
                safetensors::Dtype::F64 => f64::from_le_bytes(b.try_into().ok()?),
                safetensors::Dtype::F32 => f32::from_le_bytes(b.try_into().ok()?) as f64,
                safetensors::Dtype::F16 => half::f16::from_le_bytes(b.try_into().ok()?).to_f64(),
                safetensors::Dtype::I8 => b[0] as i8 as f64,
                safetensors::Dtype::I32 => i32::from_le_bytes(b.try_into().ok()?) as f64,
                safetensors::Dtype::I64 => i64::from_le_bytes(b.try_into().ok()?) as f64,
                _ => return None,
            })
        }

        /// Model2Vec's `encode` without the 512-token cut: mean of the token rows (each scaled by its
        /// weight, if the model has weights), normalised.
        pub fn embed(&self, text: &str) -> Vec<f32> {
            let (rows, dims) = (self.embeddings.shape[0], self.dims());
            let mut sum = vec![0.0f64; dims];
            let Some(ids) = self.ids(text) else { return vec![0.0; dims] };
            let mut n = 0usize;
            for id in ids {
                if Some(id) == self.unk {
                    continue;
                }
                let tok = id as usize;
                let row = self.mapping.as_ref().and_then(|m| self.value(m, tok)).map_or(tok, |r| r as usize);
                if row >= rows {
                    continue;
                }
                let scale = self.weights.as_ref().and_then(|w| self.value(w, tok)).unwrap_or(1.0);
                for (d, s) in sum.iter_mut().enumerate() {
                    *s += self.value(&self.embeddings, row * dims + d).unwrap_or(0.0) * scale;
                }
                n += 1;
            }
            let mut v: Vec<f32> = sum.into_iter().map(|s| (s / n.max(1) as f64) as f32).collect();
            if self.normalize {
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
                v.iter_mut().for_each(|x| *x /= norm);
            }
            v
        }
    }

    fn size(d: safetensors::Dtype) -> Option<usize> {
        use safetensors::Dtype::*;
        match d {
            F64 | I64 => Some(8),
            F32 | I32 => Some(4),
            F16 => Some(2),
            I8 => Some(1),
            _ => None,
        }
    }

    /// FNV-1a over the lesson text: whether a lesson's vector is current.
    fn hash(text: &str) -> String {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in text.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        format!("{h:016x}")
    }

    fn ms(d: std::time::Duration) -> f64 {
        d.as_secs_f64() * 1000.0
    }

    /// Stdout goes to /dev/null while it lives. SeekStorm prints status lines (commits) to stdout, which
    /// is the hook's answer channel; it cannot be muted on an opened index.
    /// One redirection at a time: two overlapping ones would save and restore each other's /dev/null,
    /// leaving stdout muted for good (threads in one process, as in the test harness).
    static QUIET: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct Quiet(Option<i32>, #[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    impl Quiet {
        fn new() -> Quiet {
            use std::io::Write;
            let guard = QUIET.lock().unwrap_or_else(|e| e.into_inner());
            let _ = std::io::stdout().flush();
            // SAFETY: plain fd calls on stdout and a /dev/null descriptor this function opens and closes.
            unsafe {
                let saved = libc::dup(1);
                let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
                if saved < 0 || null < 0 || libc::dup2(null, 1) < 0 {
                    for fd in [saved, null] {
                        if fd >= 0 {
                            libc::close(fd);
                        }
                    }
                    return Quiet(None, guard);
                }
                libc::close(null);
                Quiet(Some(saved), guard)
            }
        }
    }

    impl Drop for Quiet {
        fn drop(&mut self) {
            use std::io::Write;
            let _ = std::io::stdout().flush();
            if let Some(saved) = self.0 {
                // SAFETY: restores the descriptor saved in `new`.
                unsafe {
                    libc::dup2(saved, 1);
                    libc::close(saved);
                }
            }
        }
    }

    fn runtime() -> Result<tokio::runtime::Runtime, String> {
        tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())
    }

    /// An advisory lock beside the index (`vectors.lock`): exclusive while writing, shared while reading.
    struct Lock(#[allow(dead_code)] File);

    fn lock(index: &Path, exclusive: bool, wait: bool) -> Option<Lock> {
        use std::os::fd::AsRawFd;
        let path = index.with_file_name("vectors.lock");
        let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path).ok()?;
        let op = if exclusive { libc::LOCK_EX } else { libc::LOCK_SH } | if wait { 0 } else { libc::LOCK_NB };
        // SAFETY: flock on a file descriptor this function owns; released when the file is closed.
        (unsafe { libc::flock(f.as_raw_fd(), op) } == 0).then_some(Lock(f))
    }

    fn read_manifest(index: &Path) -> Option<Manifest> {
        serde_json::from_str(&std::fs::read_to_string(index.join(MANIFEST)).ok()?).ok()
    }

    /// The query: embed `text`, search the index, map hits to lesson ids (stored with each vector).
    pub fn query(index: &Path, model: &Path, text: &str, n: usize) -> Result<Vec<String>, String> {
        // A write (after `borg learn`) takes well under a second: wait briefly rather than give up.
        let _lock = (0..15)
            .find_map(|i| {
                if i > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                lock(index, false, false)
            })
            .ok_or("index is being written")?;
        let manifest = read_manifest(index).ok_or("no manifest")?;
        if manifest.model != identity(model) {
            return Err("index was built with another model".into());
        }
        let t = std::time::Instant::now();
        let embedder = Embedder::load(model)?;
        let loaded = t.elapsed();
        let v = embedder.embed(text);
        let embedded = t.elapsed() - loaded;
        // A hook process ends right after: freeing the vocabulary would only cost time.
        std::mem::forget(embedder);
        let _quiet = Quiet::new();
        runtime()?.block_on(async {
            let idx = open_index(index).await?;
            let r = idx
                .search(
                    String::new(),
                    Some(Embedding::F32(v)),
                    QueryType::Union,
                    SearchMode::Vector { similarity_threshold: None, ann_mode: AnnMode::All },
                    false,
                    0,
                    n,
                    ResultType::Topk,
                    false,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    QueryRewriting::SearchOnly,
                )
                .await;
            let fields = std::collections::HashSet::from(["id".to_string()]);
            let mut ids = Vec::new();
            for hit in &r.results {
                let doc = idx.read().await.get_document(hit.doc_id, false, &None, &fields, &[]).await;
                if let Some(id) = doc.ok().and_then(|d| d.get("id").and_then(Value::as_str).map(str::to_string))
                    && !ids.contains(&id)
                {
                    ids.push(id);
                }
            }
            idx.close().await;
            if std::env::var_os("BORG_VECTORS_TRACE").is_some() {
                eprintln!("vectors: load {:.1} ms, embed {:.1} ms, index {:.1} ms", ms(loaded), ms(embedded), ms(t.elapsed() - loaded - embedded));
            }
            Ok(ids)
        })
    }

    /// Bring the index of `lessons` up to date: new lessons are added; a changed or removed lesson, a model
    /// change or `rebuild` rebuilds it (into a fresh directory, swapped in under the lock). The number of
    /// lessons embedded.
    pub fn update(lessons: &Path, model: &Path, rebuild: bool) -> Result<usize, String> {
        let index = super::index_dir(lessons);
        let _lock = lock(&index, true, true).ok_or("cannot lock the index")?;
        let current: Vec<conscience::Challenge> = conscience::load_unweighted(lessons, None);
        let wanted: BTreeMap<String, String> = current.iter().map(|c| (c.id.clone(), hash(&lesson_text(c)))).collect();
        let old = read_manifest(&index).filter(|_| !rebuild && super::model::built_with(lessons, model) == Some(true));
        let (fresh, mut manifest) = match old {
            Some(m) if m.lessons.iter().all(|(id, h)| wanted.get(id) == Some(h)) => (false, m),
            _ => (true, Manifest { model: identity(model), ..Default::default() }),
        };
        let add: Vec<&conscience::Challenge> = current.iter().filter(|c| !manifest.lessons.contains_key(&c.id)).collect();
        if add.is_empty() && !fresh {
            return Ok(0);
        }
        let embedder = Embedder::load(model)?;
        manifest.dims = embedder.dims();
        let docs: Vec<Document> = add
            .iter()
            .map(|c| {
                let v: Vec<Value> = embedder.embed(&lesson_text(c)).into_iter().map(|x| Value::from(x as f64)).collect();
                Document::from([("vector".to_string(), Value::Array(v)), ("id".to_string(), Value::String(c.id.clone()))])
            })
            .collect();
        let target = if fresh { index.with_file_name("vectors.new") } else { index.clone() };
        let _quiet = Quiet::new();
        runtime()?.block_on(async {
            let idx = if fresh {
                let _ = std::fs::remove_dir_all(&target);
                std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
                create_index(&target, meta(manifest.dims), &schema(), &Vec::new(), crate::vectors::SEGMENT_BITS, true, Some(1)).await?
            } else {
                open_index(&target).await?
            };
            idx.index_documents(docs).await;
            idx.commit().await;
            idx.close().await;
            Ok::<(), String>(())
        })?;
        for c in &add {
            manifest.lessons.insert(c.id.clone(), wanted[&c.id].clone());
        }
        std::fs::write(target.join(MANIFEST), serde_json::to_string(&manifest).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        if fresh {
            let _ = std::fs::remove_dir_all(&index);
            std::fs::rename(&target, &index).map_err(|e| e.to_string())?;
        }
        Ok(add.len())
    }

    fn schema() -> Vec<SchemaField> {
        serde_json::from_str(
            r#"[{"field":"vector","field_type":"Json","store":false,"index_lexical":false,"index_vector":true},
                {"field":"id","field_type":"Text","store":true,"index_lexical":false,"index_vector":false}]"#,
        )
        .expect("schema")
    }

    fn meta(dims: usize) -> IndexMetaObject {
        IndexMetaObject {
            id: 0,
            name: "borg-lesson-vectors".into(),
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
            // Vectors come normalised from the embedder: dot product = cosine.
            inference: Inference::External { dimensions: dims, precision: Precision::F32, quantization: Quantization::None, similarity: VectorSimilarity::Dot },
        }
    }

    /// One line per fact: the index's size and how many lessons lack a current vector.
    pub fn status(lessons: &Path, model: &Path) -> Result<String, String> {
        let index = super::index_dir(lessons);
        let m = read_manifest(&index).ok_or(format!("none at {} (`borg vectors --rebuild`)", index.display()))?;
        let current = conscience::load_unweighted(lessons, None);
        let stale = current.iter().filter(|c| m.lessons.get(&c.id) != Some(&hash(&lesson_text(c)))).count();
        let other = if super::model::built_with(lessons, model) == Some(false) { " (built with another model: `borg vectors --rebuild`)" } else { "" };
        Ok(format!("index: {} lessons with vectors, {stale} of {} without a current one{other} ({})", m.lessons.len(), current.len(), index.display()))
    }

    /// The offline check of the fast Unigram path (unigram.rs): the token ids of each text against
    /// `tokenizers` itself. (texts with identical ids, texts compared, the first text that differs)
    pub fn parity(model: &Path, texts: &[String]) -> Result<(usize, usize, Option<String>), String> {
        let json = std::fs::read_to_string(model.join("tokenizer.json")).map_err(|e| e.to_string())?;
        let fast = super::unigram::Unigram::parse(&json).ok_or("not a Unigram tokenizer")?;
        let full: tokenizers::Tokenizer = json.parse().map_err(|e| format!("{e}"))?;
        let (mut same, mut first) = (0, None);
        for t in texts {
            let want = full.encode_fast(t.as_str(), false).map_err(|e| e.to_string())?.get_ids().to_vec();
            if fast.ids(t).as_deref() == Some(want.as_slice()) {
                same += 1;
            } else if first.is_none() {
                first = Some(t.clone());
            }
        }
        Ok((same, texts.len(), first))
    }

    /// Download the model's files at the pinned revision into `dir`, each checked against its sha256
    /// before it replaces anything (curl; never called by a hook).
    pub fn fetch(dir: &Path) -> Result<(), String> {
        use sha2::Digest;
        use std::io::Read;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let base = std::env::var("BORG_VECTOR_URL").unwrap_or_else(|_| format!("https://huggingface.co/{MODEL_REPO}/resolve/{MODEL_REVISION}"));
        for (name, sha) in MODEL_FILES {
            let path = dir.join(name);
            let part = dir.join(format!("{name}.part"));
            if sum(&path, &mut Default::default()).as_deref() == Some(sha) {
                continue;
            }
            eprintln!("fetching {name}");
            let ok = std::process::Command::new("curl")
                .args(["-fL", "--retry", "3", "-o"])
                .arg(&part)
                .arg(format!("{base}/{name}"))
                .status()
                .map_err(|e| format!("curl: {e}"))?
                .success();
            let got = sum(&part, &mut Default::default());
            if !ok || got.as_deref() != Some(sha) {
                let _ = std::fs::remove_file(&part);
                return Err(format!("{name}: download failed or sha256 mismatch (got {}, want {sha})", got.unwrap_or_default()));
            }
            std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
        }
        std::fs::write(dir.join("REVISION"), MODEL_REVISION).map_err(|e| e.to_string())?;
        return Ok(());

        fn sum(path: &Path, h: &mut sha2::Sha256) -> Option<String> {
            let mut f = File::open(path).ok()?;
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = f.read(&mut buf).ok()?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            Some(h.clone().finalize().iter().map(|b| format!("{b:02x}")).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lesson(id: &str, text: &str) -> Challenge {
        Challenge { id: id.into(), question: text.into(), situations: vec![text.into()], consequence: text.into(), ..Default::default() }
    }

    fn ids(v: &[&Challenge]) -> Vec<String> {
        v.iter().map(|c| c.id.clone()).collect()
    }

    fn ranked(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn extend_appends_vector_hits_after_bm25_once_each_up_to_k() {
        let pool = vec![lesson("a", "x"), lesson("b", "x"), lesson("c", "x"), lesson("d", "x")];
        let bm25 = vec![&pool[0], &pool[1]];
        let r = ranked(&["b", "c", "gone", "d", "c"]);
        assert_eq!(ids(&extend(bm25.clone(), &r, &pool, "", 1)), ["a", "b", "c"]);
        assert_eq!(ids(&extend(bm25.clone(), &r, &pool, "", 10)), ["a", "b", "c", "d"]);
        assert_eq!(ids(&extend(bm25.clone(), &r, &pool, "", 0)), ["a", "b"]);
        assert_eq!(ids(&extend(bm25, &[], &pool, "", 10)), ["a", "b"]);
    }

    #[test]
    fn extend_keeps_the_scope_rule_and_drops_done_dormant_and_superseded() {
        let mut scoped = lesson("scoped", "x");
        scoped.scope = vec!["Orion".into()];
        let mut dormant = lesson("dormant", "x");
        dormant.dormant = true;
        let old = lesson("old", "x");
        let mut new = lesson("new", "x");
        new.supersedes = vec!["old".into()];
        // "done" is not in the pool: the Stop step passes only the lessons not done this session.
        let pool = vec![scoped, dormant, old, new, lesson("free", "x")];
        let r = ranked(&["done", "scoped", "dormant", "old", "new", "free"]);
        assert_eq!(ids(&extend(vec![], &r, &pool, "work in the payment service", 10)), ["new", "free"]);
        assert_eq!(ids(&extend(vec![], &r, &pool, "edit OrionConfig.java", 10)), ["scoped", "new", "free"]);
    }

    #[test]
    fn shortlist_without_an_index_is_bm25_alone() {
        let dir = std::env::temp_dir().join(format!("borg-vectors-none-{}", std::process::id()));
        let pool = vec![lesson("a", "deadlock in the pool"), lesson("b", "retry budget")];
        let (list, added) = shortlist(&dir.join("lessons.jsonl"), &pool, "a deadlock", "a deadlock", 20);
        assert_eq!((ids(&list), added), (vec!["a".to_string()], 0));
        assert!(recall(&dir.join("lessons.jsonl"), "a deadlock", 10).is_empty());
    }

    #[test]
    fn query_tail_keeps_what_the_model_reads() {
        let long = "x".repeat(crate::predict::TAIL + 5) + "end";
        let t = query_tail(&long);
        assert_eq!(t.chars().count(), crate::predict::TAIL);
        assert!(t.ends_with("end"));
    }

    /// A tiny Model2Vec model: German and English words for the same thing share a row.
    #[cfg(feature = "vectors")]
    fn fake_model(dir: &Path) {
        let words = [
            ("<unk>", [0.0f32, 0.0, 0.0, 0.0]),
            ("\u{2581}deadlock", [1.0, 0.0, 0.0, 0.0]),
            ("\u{2581}verklemmung", [1.0, 0.0, 0.0, 0.0]),
            ("\u{2581}retry", [0.0, 1.0, 0.0, 0.0]),
            ("\u{2581}wiederholung", [0.0, 1.0, 0.0, 0.0]),
            ("\u{2581}cache", [0.0, 0.0, 1.0, 0.0]),
            ("\u{2581}orion", [0.0, 0.0, 0.0, 1.0]),
            ("\u{2581}", [0.0, 0.0, 0.0, 0.0]),
        ];
        std::fs::create_dir_all(dir).unwrap();
        let vocab: Vec<serde_json::Value> = words.iter().map(|(w, _)| serde_json::json!([w, -1.0])).collect();
        let tok = serde_json::json!({"normalizer": null,
            "pre_tokenizer": {"type": "Metaspace", "replacement": "\u{2581}", "prepend_scheme": "always", "split": false},
            "model": {"type": "Unigram", "unk_id": 0, "byte_fallback": false, "vocab": vocab}});
        std::fs::write(dir.join("tokenizer.json"), tok.to_string()).unwrap();
        std::fs::write(dir.join("config.json"), r#"{"normalize": true}"#).unwrap();
        let bytes: Vec<u8> = words.iter().flat_map(|(_, v)| v.iter().flat_map(|x| x.to_le_bytes())).collect();
        let view = safetensors::tensor::TensorView::new(safetensors::Dtype::F32, vec![words.len(), 4], &bytes).unwrap();
        std::fs::write(dir.join("model.safetensors"), safetensors::serialize([("embeddings", view)], &None).unwrap()).unwrap();
    }

    #[cfg(feature = "vectors")]
    fn write_lessons(path: &Path, lessons: &[(&str, &str)]) {
        let lines: Vec<String> = lessons
            .iter()
            .enumerate()
            .map(|(i, (id, t))| serde_json::json!({"id": id, "origin_ms": i, "question": t, "situations": [t], "consequence": t}).to_string())
            .collect();
        std::fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    /// `borg doctor`'s lines for a given model directory (no environment read).
    #[cfg(feature = "vectors")]
    #[test]
    fn doctor_warns_about_a_missing_model_or_index_with_its_fix() {
        use crate::collective::{Route, Share};
        use crate::harness::Level;
        let levels = |v: &[(Level, String)]| v.iter().map(|(l, _)| *l).collect::<Vec<_>>();
        let root = std::env::temp_dir().join(format!("borg-vectors-doctor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (model, team) = (root.join("model"), root.join("team"));
        let route = |name: &str, home: PathBuf| Route::Collective { name: name.into(), home, share: Share::Local };
        let routes = vec![route("team", team.clone()), route("empty", root.join("empty")), Route::Off];
        let none = doctor::against(&routes, &root.join("no-model"));
        assert_eq!(levels(&none), [Level::Warn]);
        assert!(none[0].1.contains("borg vectors --fetch"), "{none:?}");

        fake_model(&model);
        std::fs::create_dir_all(&team).unwrap();
        let lessons = team.join("lessons.jsonl");
        write_lessons(&lessons, &[("dl", "deadlock")]);
        let missing = doctor::against(&routes, &model);
        assert_eq!(levels(&missing), [Level::Ok, Level::Warn], "only a store with lessons needs an index: {missing:?}");
        assert!(missing[1].1.contains(&format!("borg vectors --rebuild --store {}", lessons.display())), "{missing:?}");
        engine::update(&lessons, &model, false).unwrap();
        assert_eq!(levels(&doctor::against(&routes, &model)), [Level::Ok]);
        std::fs::write(model.join("REVISION"), "another").unwrap();
        let other = doctor::against(&routes, &model);
        assert!(levels(&other) == [Level::Ok, Level::Warn] && other[1].1.contains("another model"), "{other:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// One test for everything that reads the environment (BORG_VECTOR_MODEL, BORG_VECTORS), so no two
    /// tests race on it.
    #[cfg(feature = "vectors")]
    #[test]
    fn index_recalls_across_languages_updates_incrementally_and_fails_open() {
        let root = std::env::temp_dir().join(format!("borg-vectors-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (model, store) = (root.join("model"), root.join("store"));
        fake_model(&model);
        std::fs::create_dir_all(&store).unwrap();
        let lessons = store.join("lessons.jsonl");
        write_lessons(&lessons, &[("dl", "deadlock"), ("rt", "retry"), ("ca", "cache")]);
        // SAFETY: only this test sets these variables (see above).
        unsafe {
            std::env::set_var("BORG_VECTOR_MODEL", &model);
            std::env::remove_var("BORG_VECTORS");
        }
        assert_eq!(doctor::hint(), None, "the model is there");
        assert_eq!(refresh(&lessons), 3, "no index yet: the first refresh builds it");
        assert_eq!(refresh(&lessons), 0, "nothing new, nothing embedded");
        // German work, English lessons: BM25 finds nothing, the vectors find the deadlock lesson.
        let pool = conscience::load(&lessons, None);
        assert!(conscience::select_scoped(&pool, "verklemmung", "verklemmung", 20).is_empty());
        assert_eq!(recall(&lessons, "verklemmung", 1), ["dl"]);
        let (list, added) = shortlist(&lessons, &pool, "verklemmung wiederholung", "", 20);
        assert_eq!(added, 3);
        let top2: HashSet<String> = ids(&list[..2]).into_iter().collect();
        assert_eq!(top2, HashSet::from(["dl".to_string(), "rt".to_string()]));
        // A new lesson is added; a changed one rebuilds; both are found.
        write_lessons(&lessons, &[("dl", "deadlock"), ("rt", "retry"), ("ca", "cache"), ("mv", "orion")]);
        assert_eq!(refresh(&lessons), 1);
        assert_eq!(recall(&lessons, "orion", 1), ["mv"]);
        write_lessons(&lessons, &[("dl", "cache"), ("rt", "retry"), ("ca", "deadlock"), ("mv", "orion")]);
        assert_eq!(refresh(&lessons), 4, "a changed lesson rebuilds the index");
        assert_eq!(recall(&lessons, "verklemmung", 1), ["ca"]);
        // BORG_VECTORS=off: BM25 only.
        unsafe { std::env::set_var("BORG_VECTORS", "off") };
        assert!(recall(&lessons, "verklemmung", 5).is_empty());
        let (list, added) = shortlist(&lessons, &pool, "verklemmung", "", 20);
        assert_eq!((list.len(), added), (0, 0));
        let off = doctor::checks(&[]);
        assert!(off.len() == 1 && off[0].0 == crate::harness::Level::Ok && off[0].1.contains("off"), "{off:?}");
        unsafe { std::env::set_var("BORG_VECTOR_MODEL", root.join("no-model")) };
        assert_eq!(doctor::hint(), None, "off by choice: no hint");
        unsafe { std::env::set_var("BORG_VECTOR_MODEL", &model) };
        unsafe { std::env::remove_var("BORG_VECTORS") };
        // Fail open: another model's index, a missing model, a missing index.
        std::fs::write(model.join("REVISION"), "another").unwrap();
        assert!(recall(&lessons, "verklemmung", 5).is_empty(), "vectors of another model are not compared");
        std::fs::remove_file(model.join("REVISION")).unwrap();
        assert_eq!(recall(&lessons, "verklemmung", 1), ["ca"]);
        unsafe { std::env::set_var("BORG_VECTOR_MODEL", root.join("no-model")) };
        assert!(recall(&lessons, "verklemmung", 5).is_empty());
        assert_eq!(refresh(&lessons), 0);
        assert!(doctor::hint().is_some_and(|h| h.contains("borg vectors --fetch")));
        assert_eq!(doctor::checks(&[])[0].0, crate::harness::Level::Warn);
        unsafe { std::env::set_var("BORG_VECTOR_MODEL", &model) };
        std::fs::remove_dir_all(index_dir(&lessons)).unwrap();
        assert!(recall(&lessons, "verklemmung", 5).is_empty());
        // As after a mesh sync (borg-mesh sync.rs `reindex`): from a blocking thread of a running runtime.
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let path = lessons.clone();
        let n = rt.block_on(async { tokio::task::spawn_blocking(move || refresh(&path)).await.unwrap() });
        assert_eq!(n, 4, "a lost index is rebuilt by the next refresh");
        assert_eq!(recall(&lessons, "verklemmung", 1), ["ca"]);
        unsafe { std::env::remove_var("BORG_VECTOR_MODEL") };
        let _ = std::fs::remove_dir_all(&root);
    }
}
