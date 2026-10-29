//! Eval only (internal evaluation "whole-store", not published): the Stop model step over today's shortlist (arm A) vs the whole
//! store (arm B), on recorded Stop contexts with known relevant lessons.
//!
//!   hs-whole plan <queries.jsonl> <vector-rank.jsonl> <out.jsonl>
//!       0 tokens: per query and arm, the lessons the model would be shown, their prompt size, and the
//!       relevant lessons among them (the arm's ceiling), plus the product's lexical gate score
//!   hs-whole run <queries.jsonl> <vector-rank.jsonl> <out.jsonl> <ids.txt> <cap_usd> [model] [arms]
//!       one model call per (query in ids.txt, arm in arms, default AB) through the product's predict path
//!       (`predict::predict`, `predict::predict_whole`; `claude -p --model <model>`, default sonnet), appended to out.jsonl with
//!       cost and latency. Resumes: (query, arm) pairs already answered are skipped. Stops before a call
//!       whose estimated cost (the arm's most expensive call so far) would cross the cap.
//!
//! Both arms see the same pool as the hook would: the store before the query's cut-off, facets dropped (the
//! product generates none by default), no outcome weights (none exist in these stores), the system gate for
//! the query's `repos`, nothing done yet, the query as context and as scope text.
//! - A: BM25 top 30 (`select_scoped`) + up to 10 vector hits (`vectors::extend`) from the vector ranking
//!   recorded for the same query and corpus (internal evaluation "vectors", not published, rank.jsonl `vector`): the shortlist the
//!   product builds with the `vectors` feature, without loading the model here.
//! - B: `whole::eligible` + `whole::select` at the default budget (`whole::TOKENS`), arm A's list as the
//!   fallback order.
//! - C: B's lessons in A's layout (`predict::prompt`: work first, `situations -> consequence`), what
//!   `BORG_STOP_POOL=all BORG_STOP_FORMAT=classic` asks.
//! - D: B with graded applies (`BORG_STOP_POOL=all BORG_STOP_GRADED=on`): B's prompt plus the grading
//!   sentence; each row also records the grades and what the product's rule (`conscience::graded`) does
//!   with them: `asked` (the block, if any) and `free` (handed over at the next prompt or touch, as much
//!   as one early delivery fits, `conscience::render_early`).

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use borg::collective::Aliases;
use borg::conscience::{self, Challenge};
use borg::{predict, prefilter, vectors, whole};
use serde_json::{Value, json};

/// Experiment parameter: arm A's shortlist, BM25's top this many (the product's
/// `vectors::SHORTLIST` when the eval ran).
const BM25_N: usize = 30;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let a = |i: usize| args.get(i).map(String::as_str);
    match (a(1), a(2), a(3), a(4)) {
        (Some("plan"), Some(q), Some(r), Some(out)) => plan(q, r, out),
        (Some("run"), Some(q), Some(r), Some(out)) if args.len() >= 7 => {
            run(q, r, out, &args[5], args[6].parse().expect("cap"), a(7).unwrap_or("sonnet"), a(8).unwrap_or("AB"))
        }
        _ => {
            eprintln!("usage: hs-whole plan <queries> <vector-rank> <out> | run <queries> <vector-rank> <out> <ids.txt> <cap_usd> [model] [arms]");
            std::process::exit(2);
        }
    }
}

fn lines(path: &str) -> Vec<Value> {
    std::fs::read_to_string(path).expect(path).lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).expect("json")).collect()
}

/// Query id -> its recorded vector ranking (lesson ids, best first).
fn vector_ranks(path: &str) -> HashMap<String, Vec<String>> {
    lines(path)
        .into_iter()
        .filter(|r| r["file"].as_str().is_some_and(|f| f.ends_with("hybrid-search/queries.jsonl")))
        .map(|r| (r["query"].as_str().unwrap().to_string(), r["vector"].as_array().unwrap().iter().filter_map(|v| v.as_str().map(String::from)).collect()))
        .collect()
}

/// The pool as the hook would see it for query `q`: gated, nothing done.
fn pool(q: &Value) -> Vec<Challenge> {
    let before = q["before_ms"].as_i64().filter(|&b| b > 0);
    let mut all = conscience::load_unweighted_with(Path::new(q["store"].as_str().unwrap()), before, &Aliases::default());
    for c in &mut all {
        c.facets.clear();
    }
    if borg::systems::on() {
        let repos: Vec<String> = q["repos"].as_array().map(|a| a.iter().filter_map(|r| r.as_str().map(String::from)).collect()).unwrap_or_default();
        borg::systems::gate(&mut all, &repos);
    }
    all.into_iter().filter(|c| !c.off_system).collect()
}

/// What arm A and arm B show the model for `q`, and whether B's budget forced a choice.
fn shown<'a>(q: &Value, pool: &'a [Challenge], vec: &[String]) -> (Vec<&'a Challenge>, Vec<&'a Challenge>, bool) {
    let text = q["query"].as_str().unwrap();
    let a = vectors::extend(conscience::select_scoped(pool, text, text, BM25_N), vec, pool, text, vectors::DEFAULT_K);
    let ranked = conscience::rank_scoped(pool, text, text);
    let eligible = whole::eligible(pool, &ranked, text);
    let (b, fell) = whole::select(eligible, || a.clone(), &ranked, whole::TOKENS);
    (a, b, fell)
}

fn ids(v: &[&Challenge]) -> Vec<String> {
    v.iter().map(|c| c.id.clone()).collect()
}

fn relevant(q: &Value) -> HashSet<String> {
    q["relevant"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default()
}

fn head(q: &Value) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    for k in ["id", "source", "cluster", "kind", "variant"] {
        m.insert(k.into(), q[k].clone());
    }
    m
}

fn plan(queries: &str, ranks: &str, out: &str) {
    let vr = vector_ranks(ranks);
    let mut f = std::fs::File::create(out).expect("out");
    for q in lines(queries) {
        let id = q["id"].as_str().unwrap();
        let p = pool(&q);
        let (a, b, fell) = shown(&q, &p, vr.get(id).map(Vec::as_slice).unwrap_or(&[]));
        let rel = relevant(&q);
        let text = q["query"].as_str().unwrap();
        let mut row = head(&q);
        row.insert("vectors".into(), json!(vr.contains_key(id)));
        row.insert("pool".into(), json!(p.len()));
        row.insert("gate_score".into(), json!(prefilter::score(&p, text)));
        for (arm, list, prompt) in [
            ("A", &a, predict::prompt(text, &a, &[], false)),
            ("B", &b, predict::prompt_whole(text, &b, &[], false)),
            ("C", &b, predict::prompt(text, &b, &[], false)),
            ("D", &b, predict::prompt_whole(text, &b, &[], true)),
        ] {
            row.insert(format!("{arm}_shown"), json!(ids(list)));
            row.insert(format!("{arm}_relevant_shown"), json!(list.iter().filter(|c| rel.contains(&c.id)).count()));
            row.insert(format!("{arm}_prompt_chars"), json!(prompt.chars().count()));
        }
        row.insert("B_fell_back".into(), json!(fell));
        row.insert("n_relevant".into(), json!(rel.len()));
        writeln!(f, "{}", Value::Object(row)).unwrap();
    }
}

fn run(queries: &str, ranks: &str, out: &str, ids_file: &str, cap: f64, model: &str, arms: &str) {
    let arms: Vec<String> = arms.chars().map(String::from).collect();
    assert!(!arms.is_empty() && arms.iter().all(|a| ["A", "B", "C", "D"].contains(&a.as_str())), "arms: letters of ABCD");
    let vr = vector_ranks(ranks);
    let wanted: Vec<String> = std::fs::read_to_string(ids_file).expect("ids").lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    let all: HashMap<String, Value> = lines(queries).into_iter().map(|q| (q["id"].as_str().unwrap().to_string(), q)).collect();
    let done: Vec<Value> = std::fs::read_to_string(out).map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default();
    let mut spent: f64 = done.iter().map(|r| r["cost"].as_f64().unwrap_or(0.0)).sum();
    let mut worst: HashMap<String, f64> = HashMap::new();
    for r in &done {
        let arm = r["arm"].as_str().unwrap_or_default().to_string();
        let c = r["cost"].as_f64().unwrap_or(0.0);
        worst.entry(arm).and_modify(|w| *w = w.max(c)).or_insert(c);
    }
    let answered: HashSet<(String, String)> =
        done.iter().filter(|r| r["ok"] == true).map(|r| (r["id"].as_str().unwrap_or_default().into(), r["arm"].as_str().unwrap_or_default().into())).collect();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(out).expect("out");
    for (i, id) in wanted.iter().enumerate() {
        let q = all.get(id).unwrap_or_else(|| panic!("unknown query {id}"));
        let text = q["query"].as_str().unwrap();
        let p = pool(q);
        let (a, b, fell) = shown(q, &p, vr.get(id).map(Vec::as_slice).unwrap_or(&[]));
        // Alternate which arm goes first, so neither always meets a warm cache or a quiet machine first.
        let mut order = arms.clone();
        if i % 2 == 1 {
            order.reverse();
        }
        for arm in order.iter().map(String::as_str) {
            if answered.contains(&(id.clone(), arm.to_string())) {
                continue;
            }
            let estimate = worst.get(arm).copied().unwrap_or(0.15);
            if spent + estimate > cap {
                eprintln!("cap: spent ${spent:.4}, next {arm} call estimated ${estimate:.4} > ${cap}; stopping");
                return;
            }
            let list = if arm == "A" { &a } else { &b };
            let started = std::time::Instant::now();
            let call = || match arm {
                "B" => predict::predict_whole(text, list, &[], model, false, predict::timeout()),
                "D" => predict::predict_whole(text, list, &[], model, true, predict::timeout()),
                _ => predict::predict(text, list, &[], model, false, predict::timeout()),
            };
            let mut pred = call();
            let mut tries = 1;
            if pred.is_none() {
                tries = 2;
                pred = call();
            }
            let ms = started.elapsed().as_millis();
            let cost = pred.as_ref().map_or(0.0, |p| p.cost);
            spent += cost;
            worst.entry(arm.to_string()).and_modify(|w| *w = w.max(cost)).or_insert(cost);
            let mut row = head(q);
            row.insert("arm".into(), json!(arm));
            row.insert("model".into(), json!(model));
            row.insert("ok".into(), json!(pred.is_some()));
            row.insert("tries".into(), json!(tries));
            row.insert("shown".into(), json!(ids(list)));
            row.insert("fell_back".into(), json!(arm != "A" && fell));
            row.insert("applies".into(), json!(pred.as_ref().map(|p| p.applies.clone()).unwrap_or_default()));
            if arm == "D" {
                let p = pred.as_ref();
                let applied: Vec<(&Challenge, u8)> = p
                    .map(|p| p.applies.iter().zip(&p.grades).filter_map(|(i, g)| list.iter().find(|c| &c.id == i).map(|c| (*c, *g))).collect())
                    .unwrap_or_default();
                let (asked, free) = conscience::graded(&applied);
                let n = conscience::render_early(&free, &[]).map_or(0, |(_, n)| n);
                row.insert("grades".into(), json!(p.map(|p| p.grades.clone()).unwrap_or_default()));
                row.insert("asked".into(), json!(ids(&asked)));
                row.insert("free".into(), json!(ids(&free[..n])));
                row.insert("free_dropped".into(), json!(ids(&free[n..])));
            }
            row.insert("cost".into(), json!(cost));
            row.insert("ms".into(), json!(ms));
            row.insert("spent".into(), json!(spent));
            writeln!(f, "{}", Value::Object(row)).unwrap();
            f.flush().unwrap();
            eprintln!("{id} {arm}: ok={} applies={} ${cost:.4} {ms} ms (spent ${spent:.4})", pred.is_some(), pred.as_ref().map_or(0, |p| p.applies.len()));
        }
    }
}
