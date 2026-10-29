//! Gate test 3, offline: would layer-1 execution memory (scars + protocols) have handed a drone
//! the fix for an error an earlier session already solved? Replays every real session per repo
//! in time order; each session may only recall what sessions that ENDED before it started learned
//! (the same `scars::learn` / `scars::recall` the live hook uses). Zero tokens spent.
//!
//! A recall counts as **correct** when the session's own eventual fix for that failure matches
//! what was recalled (same command, or an overlapping edited file); otherwise it is noise.
//!
//! usage: borg-rediscovery [--claude DIR] [--omp DIR] [--min-sessions N]
//!
//! Its numbers (`--min-sessions`, bucket bounds, example counts, excerpt widths) are experiment
//! parameters of this offline measurement, not product defaults.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use borg_trace::fingerprint::command_shape;
use borg_trace::scars::{self, Memory};
use borg_trace::{Harness, Kind, Session, claude, jsonl_files, omp, repo_key};
use rayon::prelude::*;

#[derive(Default)]
struct T {
    sessions: usize,
    failures: u64,
    solved_in_session: u64,
    recalled: u64,
    recalled_scar: u64,
    recalled_protocol: u64,
    correct: u64,
    wrong: u64,
    unverifiable: u64,
    addressable: u64,
    saved_tokens: u64,
    injected_tokens: u64,
}

fn recovery_tokens(s: &Session, i: usize, shape: &str) -> u64 {
    let mut sum = s.events[i].result_tokens;
    for e in s.events.iter().skip(i + 1).take(40) {
        sum += e.result_tokens;
        if let Kind::Bash { cmd, failed: false, .. } = &e.kind
            && command_shape(cmd) == shape
        {
            return sum;
        }
    }
    sum
}

fn base(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// Does the fix this session found for (fp, shape) agree with what was recalled?
fn agrees(own: &[Memory], recalled: &Memory) -> Option<bool> {
    match recalled {
        Memory::Scar { fp, fix_cmd, fix_edits, .. } => own.iter().find_map(|m| match m {
            Memory::Scar { fp: f, fix_cmd: c, fix_edits: e, .. } if f == fp => {
                Some((fix_cmd.is_some() && fix_cmd == c) || e.iter().any(|x| fix_edits.iter().any(|y| base(x) == base(y))))
            }
            _ => None,
        }),
        Memory::Protocol { shape, cmd, .. } => own.iter().find_map(|m| match m {
            Memory::Protocol { shape: s, cmd: c, .. } if s == shape => Some(borg_trace::fingerprint::invocation(c) == borg_trace::fingerprint::invocation(cmd)),
            _ => None,
        }),
    }
}

fn main() {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let (mut claude_dir, mut omp_dir, mut min) = (home.join(".claude/projects"), home.join(".omp/agent/sessions"), 5usize);
    let mut it = std::env::args().skip(1);
    while let Some(f) = it.next() {
        let v = it.next().unwrap_or_default();
        match f.as_str() {
            "--claude" => claude_dir = v.into(),
            "--omp" => omp_dir = v.into(),
            "--min-sessions" => min = v.parse().unwrap_or(5),
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
    let mut t = T::default();
    let mut per_repo: Vec<(String, u64, u64, u64)> = Vec::new();
    let mut examples: Vec<String> = Vec::new();
    for (repo, ss) in by_repo.iter_mut().filter(|(_, v)| v.len() >= min) {
        ss.sort_by_key(|s| s.start);
        let learned: Vec<(i64, Vec<Memory>)> = ss.iter().map(|s| (s.end, scars::learn(s, repo))).collect();
        let (f0, c0, w0) = (t.failures, t.correct, t.wrong);
        for s in ss.iter() {
            t.sessions += 1;
            // What the collective knew when this session started: sessions that had ended.
            let known = scars::latest(learned.iter().filter(|(end, _)| *end < s.start).flat_map(|(_, m)| m.iter().cloned()));
            let own = scars::learn(s, repo);
            for (i, e) in s.events.iter().enumerate() {
                let Kind::Bash { cmd, failed: true, error_line, .. } = &e.kind else { continue };
                t.failures += 1;
                let shape = command_shape(cmd);
                let solved = s.events[i + 1..].iter().any(|x| matches!(&x.kind, Kind::Bash { cmd: c, failed: false, .. } if command_shape(c) == shape));
                t.solved_in_session += solved as u64;
                let text = error_line.clone().unwrap_or_default();
                let Some(line) = scars::recall(&known, cmd, &text) else { continue };
                t.recalled += 1;
                t.injected_tokens += (line.len() / borg_trace::CHARS_PER_TOKEN) as u64;
                // Which memory produced the line (same precedence as recall: scar first).
                let fp = borg_trace::fingerprint::error_fingerprint(&text).map(|f| format!("{f:016x}"));
                let used = known
                    .iter()
                    .find(|m| matches!(m, Memory::Scar { fp: f, .. } if Some(f) == fp.as_ref()))
                    .or_else(|| known.iter().find(|m| matches!(m, Memory::Protocol { shape: sh, .. } if *sh == shape)))
                    .cloned();
                match &used {
                    Some(Memory::Scar { .. }) => t.recalled_scar += 1,
                    _ => t.recalled_protocol += 1,
                }
                match used.as_ref().and_then(|m| agrees(&own, m)) {
                    Some(true) => {
                        t.correct += 1;
                        t.saved_tokens += recovery_tokens(s, i, &shape);
                        if examples.len() < 12 {
                            examples.push(format!("- `{}` → {}", cmd.chars().take(70).collect::<String>().replace('`', "'"), line));
                        }
                    }
                    Some(false) => {
                        t.wrong += 1;
                        if std::env::var("SHOW_WRONG").is_ok() && t.wrong % 6 == 1 {
                            let ownfix: Vec<String> = own
                                .iter()
                                .filter_map(|m| match m {
                                    Memory::Scar { fix_cmd, fix_edits, shape: sh, .. } if *sh == shape => Some(format!(
                                        "scar fix_cmd={:?} edits={:?}",
                                        fix_cmd.as_ref().map(|c| c.chars().take(90).collect::<String>()),
                                        fix_edits
                                    )),
                                    Memory::Protocol { cmd: c, shape: sh, .. } if *sh == shape => {
                                        Some(format!("protocol `{}`", c.chars().take(90).collect::<String>()))
                                    }
                                    _ => None,
                                })
                                .collect();
                            eprintln!(
                                "FAILED: {}\n  ERR: {}\n  RECALL: {}\n  OWN: {:?}\n",
                                cmd.chars().take(110).collect::<String>(),
                                text.chars().take(120).collect::<String>(),
                                line.chars().take(220).collect::<String>(),
                                ownfix
                            );
                        }
                    }
                    None => t.unverifiable += 1,
                }
                if solved {
                    t.addressable += 1;
                }
            }
        }
        per_repo.push((repo.clone(), t.failures - f0, t.correct - c0, t.wrong - w0));
    }
    let pct = |a: u64, b: u64| if b == 0 { 0.0 } else { 100.0 * a as f64 / b as f64 };
    println!("# Rediscovery replay: layer-1 execution memory (scars + protocols)\n");
    println!("Sessions {} in repos with ≥{min} sessions. Each session recalls only what earlier, finished sessions learned. 0 tokens.\n", t.sessions);
    println!("| Metric | Value |\n|---|---|");
    println!("| Failed commands | {} |", t.failures);
    println!("| … solved later in the same session | {} ({:.0}%) |", t.solved_in_session, pct(t.solved_in_session, t.failures));
    println!(
        "| Recall fired | {} ({:.1}% of failures; scars {}, protocols {}) |",
        t.recalled,
        pct(t.recalled, t.failures),
        t.recalled_scar,
        t.recalled_protocol
    );
    println!("| Recall correct (session's own fix matched) | {} ({:.0}% of verifiable) |", t.correct, pct(t.correct, t.correct + t.wrong));
    println!("| Recall wrong (session fixed it differently) | {} |", t.wrong);
    println!("| Recall unverifiable (session never fixed it) | {} |", t.unverifiable);
    println!("| Tokens the correct recalls could have saved | {:.2}M |", t.saved_tokens as f64 / 1e6);
    println!("| Tokens injected by all recalls | {:.1}k |", t.injected_tokens as f64 / 1e3);
    println!("\n| Repo | Failures | Correct | Wrong |\n|---|---|---|---|");
    for (r, f, c, w) in &per_repo {
        println!("| {} | {f} | {c} | {w} |", r.rsplit('/').next().unwrap_or(r));
    }
    println!("\nExamples of correct recalls:\n{}", examples.join("\n"));
}
