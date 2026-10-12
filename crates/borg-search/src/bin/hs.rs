//! SeekStorm arms (C lexical, D vector, E hybrid) for reports/hybrid-search.md.
//!
//!   hs rank-all <out.jsonl> <index root> <queries.jsonl>...   top 20 of every SeekStorm arm for every query
//!   hs build <lessons.jsonl> <before_ms> <index dir> <lex|model name>
//!                                                             build a persisted index, print seconds
//!   hs query <index dir> <lex|vec|hyb> <query file>           one hook-like query in a fresh process
//!
//! Models are directories under HS_MODELS (default ~/.local/share/borg/eval/hybrid-search/models).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use borg_search::seek::{self, Embedder, Mode};
use borg_search::{load, read_queries};

const TOP: usize = 20;
const MODELS: [(&str, &str); 2] = [("8", "potion-base-8M"), ("ML", "potion-multilingual-128M")];

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("rank-all") => rank_all(Path::new(&args[2]), Path::new(&args[3]), &args[4..]).await,
        Some("build") => {
            let lessons = load(Path::new(&args[2]), args[3].parse().expect("before_ms"));
            let model = (args[5] != "lex").then(|| seek::model_dir(&args[5]));
            let t = Instant::now();
            seek::build(&lessons, Path::new(&args[4]), model.as_deref()).await.expect("build");
            println!("{} lessons {:.3} s", lessons.len(), t.elapsed().as_secs_f64());
        }
        Some("query") => {
            let query = std::fs::read_to_string(&args[4]).expect("query file");
            let (index, ids) = seek::open(Path::new(&args[2])).await.expect("open");
            let hits = seek::search(&index, &query, Mode::parse(&args[3]), None, TOP).await;
            let mut out = std::io::stdout().lock();
            for (d, s) in hits {
                let _ = writeln!(out, "{}\t{s:.4}", ids.get(d).map(String::as_str).unwrap_or("?"));
            }
        }
        _ => {
            eprintln!("usage: hs rank-all <out> <index root> <queries>... | build <lessons> <before_ms> <dir> <lex|model> | query <dir> <lex|vec|hyb> <file>");
            std::process::exit(2);
        }
    }
}

/// Every corpus (store, cut-off) gets a lexical index and one index per model under `root`, built once.
async fn rank_all(out: &Path, root: &Path, query_files: &[String]) {
    let queries: Vec<_> = query_files.iter().flat_map(|f| read_queries(Path::new(f))).collect();
    let mut corpora: BTreeMap<(String, i64), Vec<usize>> = BTreeMap::new();
    for (i, q) in queries.iter().enumerate() {
        corpora.entry((q.store.clone(), q.before_ms)).or_default().push(i);
    }
    let embedders: Vec<(&str, Embedder)> = MODELS.iter().map(|(k, m)| (*k, Embedder::load(&seek::model_dir(m)))).collect();
    let mut f = std::fs::File::create(out).expect("out");
    let mut builds = std::fs::File::create(out.with_extension("builds.jsonl")).expect("builds");
    for (c, ((store, before), members)) in corpora.iter().enumerate() {
        let lessons = load(Path::new(store), *before);
        let dir = root.join(format!("c{c:03}"));
        let mut variants: Vec<(&str, PathBuf, Option<&Embedder>)> = vec![("lex", dir.join("lex"), None)];
        for (k, e) in &embedders {
            variants.push((k, dir.join(format!("m{k}")), Some(e)));
        }
        for (k, (name, path, emb)) in variants.iter().enumerate() {
            let model = (k > 0).then(|| seek::model_dir(MODELS[k - 1].1));
            let t = Instant::now();
            seek::build(&lessons, path, model.as_deref()).await.expect("build");
            let secs = t.elapsed().as_secs_f64();
            writeln!(builds, "{}", serde_json::json!({"corpus": c, "store": store, "before_ms": before, "variant": name, "n": lessons.len(), "build_s": secs}))
                .unwrap();
            let (index, ids) = seek::open(path).await.expect("open");
            // the sidecar's order is the index's doc ids
            for d in [0, lessons.len() / 2, lessons.len().saturating_sub(1)] {
                if !lessons.is_empty() {
                    assert_eq!(seek::stored_id(&index, d).await.as_deref(), Some(ids[d].as_str()), "doc id order");
                }
            }
            let arms: Vec<(String, Mode, bool)> = if *name == "lex" {
                vec![("C".into(), Mode::Lexical, false)]
            } else {
                vec![
                    (format!("D{name}"), Mode::Vector, false),
                    (format!("E{name}"), Mode::Hybrid, false),
                    (format!("D{name}f"), Mode::Vector, true),
                    (format!("E{name}f"), Mode::Hybrid, true),
                ]
            };
            for &i in members {
                let q = &queries[i];
                for (arm, mode, full) in &arms {
                    let vector = if *full { emb.map(|e| e.embed(&q.query)) } else { None };
                    let hits = seek::search(&index, &q.query, *mode, vector, TOP).await;
                    let top: Vec<_> = hits.iter().map(|(d, s)| serde_json::json!([ids[*d], s])).collect();
                    writeln!(f, "{}", serde_json::json!({"query": q.id, "arm": arm, "n": lessons.len(), "top": top})).unwrap();
                }
            }
            seekstorm::index::Close::close(&index).await;
        }
        eprintln!("corpus {c}: {} lessons, {} queries", lessons.len(), members.len());
    }
}
