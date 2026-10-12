//! Layer 0 test, offline: does awareness of concurrent drones prevent real harm?
//!
//! Overlap alone is not harm. The harm proxy here is a **stale edit**: session A read file X,
//! a concurrent session B then edited X, and A edited X afterwards without re-reading it — A
//! changed the file from a view that no longer existed (clobber risk). An awareness line at A's
//! next touch of X ("B changed X n min ago") would have arrived exactly before that edit.
//! Also counted: **lost updates** where A's stale edit came after B's edit to the same file and
//! B's change disappeared from the final state is not observable here, so stale edits are an
//! upper bound on clobbers. Zero tokens.
//!
//! usage: borg-awareness [--claude DIR] [--omp DIR]

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use borg_trace::{Harness, Kind, Session, claude, jsonl_files, omp, repo_key};
use rayon::prelude::*;

fn main() {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let (mut claude_dir, mut omp_dir) = (home.join(".claude/projects"), home.join(".omp/agent/sessions"));
    let mut it = std::env::args().skip(1);
    while let Some(f) = it.next() {
        let v = it.next().unwrap_or_default();
        match f.as_str() {
            "--claude" => claude_dir = v.into(),
            "--omp" => omp_dir = v.into(),
            _ => eprintln!("ignoring {f}"),
        }
    }
    let mut files: Vec<(Harness, PathBuf)> = jsonl_files(&claude_dir).into_iter().map(|p| (Harness::Claude, p)).collect();
    files.extend(jsonl_files(&omp_dir).into_iter().map(|p| (Harness::Omp, p)));
    let sessions: Vec<Session> = files
        .par_iter()
        .filter_map(|(h, p)| match h {
            Harness::Claude => claude::parse(p).ok(),
            Harness::Omp => omp::parse(p).ok(),
        })
        .filter(|s| !s.is_empty() && !s.cwd.is_empty() && !s.sidechain)
        .collect();
    let mut by_repo: BTreeMap<String, Vec<&Session>> = BTreeMap::new();
    let mut cache: HashMap<String, String> = HashMap::new();
    for s in &sessions {
        let r = cache.entry(s.cwd.clone()).or_insert_with(|| repo_key(&s.cwd)).clone();
        by_repo.entry(r).or_default().push(s);
    }
    let (mut pairs, mut shared_edit_pairs, mut stale_edits, mut stale_pairs, mut reread_saves) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut gaps: Vec<i64> = Vec::new();
    let mut examples: Vec<String> = Vec::new();
    for (repo, ss) in &by_repo {
        for (i, a) in ss.iter().enumerate() {
            for b in ss.iter().skip(i + 1) {
                if a.end < b.start || b.end < a.start {
                    continue;
                }
                pairs += 1;
                let mut pair_stale = 0;
                let mut shared = false;
                for (x, y) in [(a, b), (b, a)] {
                    // y's edits per path with time
                    let mut y_edits: HashMap<&str, Vec<i64>> = HashMap::new();
                    for e in &y.events {
                        if let Kind::Edit { path } = &e.kind {
                            y_edits.entry(path.as_str()).or_default().push(e.ts);
                        }
                    }
                    let mut last_read: HashMap<&str, i64> = HashMap::new();
                    for e in &x.events {
                        match &e.kind {
                            Kind::Read { path } => {
                                last_read.insert(path.as_str(), e.ts);
                            }
                            Kind::Edit { path } => {
                                let Some(ye) = y_edits.get(path.as_str()) else {
                                    last_read.insert(path.as_str(), e.ts); // own edit = current view
                                    continue;
                                };
                                shared = true;
                                let seen = last_read.get(path.as_str()).copied().unwrap_or(x.start);
                                // a concurrent edit by y landed after x last saw the file, before x edits it
                                if let Some(t) = ye.iter().copied().filter(|t| *t > seen && *t < e.ts).max() {
                                    stale_edits += 1;
                                    pair_stale += 1;
                                    gaps.push((e.ts - t) / 1000);
                                    if examples.len() < 10 {
                                        examples.push(format!(
                                            "- {}: `{}` edited by another session {}s before this session's edit, unseen",
                                            repo.rsplit('/').next().unwrap_or(repo),
                                            path.rsplit('/').next().unwrap_or(path),
                                            (e.ts - t) / 1000
                                        ));
                                    }
                                } else if ye.iter().any(|t| *t < seen) {
                                    reread_saves += 1; // x re-read after y's edit: it noticed by itself
                                }
                                last_read.insert(path.as_str(), e.ts);
                            }
                            _ => {}
                        }
                    }
                }
                shared_edit_pairs += shared as u64;
                stale_pairs += (pair_stale > 0) as u64;
            }
        }
    }
    gaps.sort();
    let med = gaps.get(gaps.len() / 2).copied().unwrap_or(0);
    let pct = |a: u64, b: u64| if b == 0 { 0.0 } else { 100.0 * a as f64 / b as f64 };
    println!("# Awareness replay: stale edits between concurrent sessions\n");
    println!("| Metric | Value |\n|---|---|");
    println!("| Concurrent session pairs (same repo) | {pairs} |");
    println!("| … both edited at least one same file | {shared_edit_pairs} ({:.1}%) |", pct(shared_edit_pairs, pairs));
    println!("| … with ≥1 stale edit (edited a file another session changed since it last looked) | {stale_pairs} ({:.1}%) |", pct(stale_pairs, pairs));
    println!("| Stale edits | {stale_edits} (median {med}s after the other session's change) |");
    println!("| Edits where the session re-read after the other's change by itself | {reread_saves} |");
    println!("\nExamples:\n{}", examples.join("\n"));
}
