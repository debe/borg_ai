//! The offline check of reports/vectors.md: what the consequence model would see at the Stop step.
//!
//!   hs-vectors rank <out.jsonl> <corpus root> <queries.jsonl>...
//!       per query: BM25's top 30 (the product's `select_scoped`, the query as its own scope text), the
//!       vector ranking (top 60), the Stop shortlist (`vectors::extend`: BM25 top 20 + vector top 10) and,
//!       descriptive, BM25 top 30 + vector top 10
//!   hs-vectors parity <queries.jsonl>...
//!       the fast Unigram tokenizer against `tokenizers` on every lesson and query text
//!
//! The model is `BORG_VECTOR_MODEL`. Each corpus (store, cut-off) is written to `<corpus root>/cNNN/`
//! without facets (the product generates none by default) and gets its own vector index there.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use borg::{conscience, vectors};
use borg_search::read_queries;
use serde_json::Value;

const SHORT: usize = 20;
const WIDE: usize = 30;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("rank") if args.len() >= 5 => rank(Path::new(&args[2]), Path::new(&args[3]), &args[4..]),
        Some("parity") if args.len() >= 3 => parity(&args[2..]),
        _ => {
            eprintln!("usage: hs-vectors rank <out.jsonl> <corpus root> <queries.jsonl>... | parity <queries.jsonl>...");
            std::process::exit(2);
        }
    }
}

/// The store's lesson lines learned before `before_ms` (0: all), facets removed.
fn corpus_lines(store: &str, before_ms: i64) -> Vec<String> {
    std::fs::read_to_string(store)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| before_ms <= 0 || v["origin_ms"].as_i64().is_some_and(|o| o < before_ms))
        .map(|mut v| {
            if let Some(o) = v.as_object_mut() {
                o.remove("facets");
            }
            v.to_string()
        })
        .collect()
}

fn rank(out: &Path, root: &Path, files: &[String]) {
    let model = vectors::model_dir();
    let queries: Vec<_> = files.iter().flat_map(|f| read_queries(Path::new(f)).into_iter().map(move |q| (f.clone(), q))).collect();
    let raw: Vec<Value> =
        files.iter().flat_map(|f| std::fs::read_to_string(f).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect::<Vec<_>>()).collect();
    let mut corpora: BTreeMap<(String, i64), PathBuf> = BTreeMap::new();
    let mut f = std::fs::File::create(out).expect("out");
    for (i, (file, q)) in queries.iter().enumerate() {
        let n = corpora.len();
        let lessons = corpora
            .entry((q.store.clone(), q.before_ms))
            .or_insert_with(|| {
                let dir = root.join(format!("c{n:03}"));
                std::fs::create_dir_all(&dir).unwrap();
                let path = dir.join("lessons.jsonl");
                std::fs::write(&path, corpus_lines(&q.store, q.before_ms).join("\n") + "\n").unwrap();
                let t = std::time::Instant::now();
                let k = vectors::engine::update(&path, &model, true).expect("index");
                eprintln!("corpus {n}: {k} lessons indexed in {:.2} s", t.elapsed().as_secs_f64());
                path
            })
            .clone();
        let pool = conscience::load(&lessons, None);
        let bm: Vec<&conscience::Challenge> = conscience::select_scoped(&pool, &q.query, &q.query, WIDE);
        let t = std::time::Instant::now();
        let vec = vectors::engine::query(&vectors::index_dir(&lessons), &model, &vectors::query_tail(&q.query), 60).expect("query");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let short = vectors::extend(bm.iter().take(SHORT).copied().collect(), &vec, &pool, &q.query, vectors::DEFAULT_K);
        // descriptive: BM25 top 30 + the vector hits
        let wide = vectors::extend(bm.clone(), &vec, &pool, &q.query, vectors::DEFAULT_K);
        let ids = |v: &[&conscience::Challenge]| v.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        let row = serde_json::json!({
            "query": q.id, "file": file, "n": pool.len(), "bm25": ids(&bm), "vector": vec, "stop": ids(&short), "stop30": ids(&wide), "vector_ms": ms,
            "kind": raw[i]["kind"], "cluster": raw[i]["cluster"], "relevant": raw[i]["relevant"], "source": raw[i]["source"],
        });
        writeln!(f, "{row}").unwrap();
    }
}

fn parity(files: &[String]) {
    let model = vectors::model_dir();
    let mut texts: Vec<String> = Vec::new();
    let mut stores = std::collections::BTreeSet::new();
    for q in files.iter().flat_map(|f| read_queries(Path::new(f))) {
        texts.push(vectors::query_tail(&q.query));
        stores.insert(q.store.clone());
    }
    for s in stores {
        texts.extend(conscience::load_unweighted(Path::new(&s), None).iter().map(vectors::lesson_text));
    }
    let (same, n, first) = vectors::engine::parity(&model, &texts).expect("parity");
    println!("{}", serde_json::json!({"identical": same, "texts": n, "first_difference": first}));
}
