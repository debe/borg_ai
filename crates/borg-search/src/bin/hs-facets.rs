//! The arms of reports/facets.md over query sets whose stores carry facets.
//!
//!   hs-facets rank-all <out.jsonl> <queries.jsonl>...   top 20 of every arm, and the product gate scores
//!
//! Arms: A (product ranking, facets removed), B (all fields, one document), F (question + facets, max),
//! F2 (best + 0.5 × second), FB (all fields + facets), FD (idf over documents), PF (product ranking with
//! facets: scope rule included). Gate rows: gA / gF = `prefilter::score` without / with facets.

use std::io::Write;
use std::path::Path;

use borg_search::{load, product, rank_b, rank_doc_idf, rank_groups, read_queries};

const TOP: usize = 20;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("rank-all") || args.len() < 4 {
        eprintln!("usage: hs-facets rank-all <out.jsonl> <queries.jsonl>...");
        std::process::exit(2);
    }
    let mut f = std::fs::File::create(&args[2]).expect("out");
    for qpath in &args[3..] {
        for q in read_queries(Path::new(qpath)) {
            let lessons = load(Path::new(&q.store), q.before_ms);
            let (a, ga) = product(Path::new(&q.store), q.before_ms, false, &q.query);
            let (pf, gf) = product(Path::new(&q.store), q.before_ms, true, &q.query);
            let arms = [
                ("A", a),
                ("B", rank_b(&lessons, &q.query)),
                ("F", rank_groups(&lessons, false, 0.0, &q.query)),
                ("F2", rank_groups(&lessons, false, 0.5, &q.query)),
                ("FB", rank_groups(&lessons, true, 0.0, &q.query)),
                ("FD", rank_doc_idf(&lessons, &q.query)),
                ("PF", pf),
            ];
            for (arm, ranked) in arms {
                let top: Vec<_> = ranked.iter().take(TOP).map(|(id, s)| serde_json::json!([id, s])).collect();
                writeln!(f, "{}", serde_json::json!({"query": q.id, "arm": arm, "n": lessons.len(), "top": top})).expect("write");
            }
            writeln!(f, "{}", serde_json::json!({"query": q.id, "gate": {"A": ga, "F": gf}, "n": lessons.len()})).expect("write");
        }
    }
}
