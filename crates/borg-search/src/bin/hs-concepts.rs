//! Eval only (internal evaluation "concepts", not published): the product ranking (`conscience::rank_scoped`: BM25 over questions,
//! scope rule, supersession, no memory weights, facets dropped as in the product's stores) today vs with
//! the concept boost (concepts.rs) at several strengths, 0 tokens.
//!
//!   hs-concepts <queries.jsonl> <out.jsonl> [betas, default 0.25,0.5,1]
//!
//! A query: {id, store, before_ms, query}. The work's concepts come from the query text (the prompt; the
//! labelled set has no edits). Out per query: the work's concepts; per lesson its concepts, today's score,
//! the overlap and, when the query names its `repos`, whether the system gate (systems.rs) drops it; the
//! top 20 of today, of each beta and of the descriptive additive arms `add<alpha>`.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use borg::collective::Aliases;
use borg::concepts;
use borg::conscience;
use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(input), Some(out)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: hs-concepts <queries.jsonl> <out.jsonl> [betas]");
        std::process::exit(2);
    };
    let betas: Vec<f64> = args.get(3).map(String::as_str).unwrap_or("0.25,0.5,1").split(',').filter_map(|b| b.parse().ok()).collect();
    let mut f = std::fs::File::create(out).expect("out");
    for line in std::fs::read_to_string(input).expect("queries").lines().filter(|l| !l.trim().is_empty()) {
        let q: Value = serde_json::from_str(line).expect("query json");
        let before = q["before_ms"].as_i64().filter(|&b| b > 0);
        let mut base = conscience::load_unweighted_with(Path::new(q["store"].as_str().unwrap()), before, &Aliases::default());
        for c in &mut base {
            c.facets.clear();
        }
        let repos: Vec<String> = q["repos"].as_array().map(|a| a.iter().filter_map(|r| r.as_str().map(String::from)).collect()).unwrap_or_default();
        let mut gated = base.clone();
        borg::systems::gate(&mut gated, &repos);
        let text = q["query"].as_str().unwrap_or_default();
        let work = concepts::of_work("", "", text);
        let sets: Vec<BTreeSet<usize>> = base.iter().map(concepts::of_lesson).collect();
        let rarity = concepts::Rarity::of(&sets);
        let overlaps: Vec<f64> = sets.iter().map(|s| concepts::overlap(s, &work, &rarity)).collect();
        let ranked = |pool: &[conscience::Challenge]| -> Vec<(String, f64)> {
            conscience::rank_scoped(pool, text, text).into_iter().map(|(s, l)| (l.id.clone(), s)).collect()
        };
        let today = ranked(&base);
        let mut arms = serde_json::Map::new();
        for &b in &betas {
            let mut pool = base.clone();
            for (c, o) in pool.iter_mut().zip(&overlaps) {
                c.weight *= 1.0 + b * o;
            }
            arms.insert(format!("{b}"), json!(ranked(&pool).iter().take(20).map(|(i, _)| i.clone()).collect::<Vec<_>>()));
        }
        // Descriptive: an additive arm, which can bring back a lesson BM25 scores 0 (renamed identifiers):
        // today's score + alpha × overlap × the best score, for lessons in scope and not superseded.
        let top = today.first().map_or(0.0, |r| r.1);
        let have = conscience::Names::of(text);
        let successors = borg::trust::successors(&base);
        for alpha in [0.1, 0.2] {
            let mut scored: Vec<(String, f64)> = base
                .iter()
                .zip(&overlaps)
                .filter(|(c, _)| conscience::in_scope(c, &have) && !successors.contains_key(&c.id))
                .map(|(c, o)| (c.id.clone(), today.iter().find(|(i, _)| *i == c.id).map_or(0.0, |r| r.1) + alpha * o * top))
                .filter(|(_, s)| *s > 0.0)
                .collect();
            scored.sort_by(|a, b| b.1.total_cmp(&a.1));
            arms.insert(format!("add{alpha}"), json!(scored.iter().take(20).map(|(i, _)| i.clone()).collect::<Vec<_>>()));
        }
        let score = |id: &str| today.iter().find(|(i, _)| i == id).map_or(0.0, |(_, s)| *s);
        let labels = |s: &BTreeSet<usize>| s.iter().map(|&i| concepts::label(i)).collect::<Vec<_>>();
        let lessons: Vec<Value> = base
            .iter()
            .zip(&sets)
            .zip(&overlaps)
            .zip(&gated)
            .map(|(((c, s), o), g)| json!({"id": c.id, "repo": c.repo, "scope": c.scope, "concepts": labels(s), "bm25": (score(&c.id) * 1e4).round() / 1e4, "overlap": (o * 1e4).round() / 1e4, "off_system": g.off_system}))
            .collect();
        let row = json!({
            "id": q["id"], "work": labels(&work), "today": today.iter().take(20).map(|(i, _)| i.clone()).collect::<Vec<_>>(),
            "boost": arms, "lessons": lessons,
        });
        writeln!(f, "{row}").expect("write");
    }
}
