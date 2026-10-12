//! Arms A and B (borg's BM25), no SeekStorm.
//!
//!   hs-bm25 rank-all <queries.jsonl> <out.jsonl>          top 20 of A and B for every query
//!   hs-bm25 query <lessons.jsonl> <before_ms> <query file> [a|b]
//!                                                         one hook-like query in a fresh process

use std::io::Write;
use std::path::Path;

use borg_search::{load, rank_a, rank_b, read_queries};

const TOP: usize = 20;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("rank-all") => rank_all(Path::new(&args[2]), Path::new(&args[3])),
        Some("query") => {
            let query = std::fs::read_to_string(&args[4]).expect("query file");
            let before: i64 = args[3].parse().expect("before_ms");
            let ranked = match args.get(5).map(String::as_str) {
                Some("b") => rank_b(&load(Path::new(&args[2]), before), &query),
                _ => rank_a(Path::new(&args[2]), before, &query),
            };
            let mut out = std::io::stdout().lock();
            for (id, s) in ranked.iter().take(TOP) {
                let _ = writeln!(out, "{id}\t{s:.4}");
            }
        }
        _ => {
            eprintln!("usage: hs-bm25 rank-all <queries.jsonl> <out.jsonl> | query <lessons> <before_ms> <query file> [a|b]");
            std::process::exit(2);
        }
    }
}

fn rank_all(queries: &Path, out: &Path) {
    let mut f = std::fs::File::create(out).expect("out");
    for q in read_queries(queries) {
        let lessons = load(Path::new(&q.store), q.before_ms);
        for (arm, ranked) in [("A", rank_a(Path::new(&q.store), q.before_ms, &q.query)), ("B", rank_b(&lessons, &q.query))] {
            let top: Vec<_> = ranked.iter().take(TOP).map(|(id, s)| serde_json::json!([id, s])).collect();
            let row = serde_json::json!({"query": q.id, "arm": arm, "n": lessons.len(), "top": top});
            writeln!(f, "{row}").expect("write");
        }
    }
}
