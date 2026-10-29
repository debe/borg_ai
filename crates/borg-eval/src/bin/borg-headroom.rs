//! Phase 0: how much would a collective memory have saved, measured on the
//! transcripts we already have. Zero tokens spent.
//!
//! usage: borg-headroom [--claude DIR] [--omp DIR] [--min-sessions N]
//!
//! Its numbers (`--min-sessions`, bucket bounds, example counts, excerpt widths) are experiment
//! parameters of this offline measurement, not product defaults.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use borg_trace::fingerprint::command_shape;
use borg_trace::{Harness, Kind, Session, claude, jsonl_files, omp, repo_key};
use rayon::prelude::*;

struct Args {
    claude: PathBuf,
    omp: PathBuf,
    min_sessions: usize,
}

fn args() -> Args {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let mut a = Args { claude: home.join(".claude/projects"), omp: home.join(".omp/agent/sessions"), min_sessions: 5 };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let val = it.next().unwrap_or_default();
        match flag.as_str() {
            "--claude" => a.claude = val.into(),
            "--omp" => a.omp = val.into(),
            "--min-sessions" => a.min_sessions = val.parse().unwrap_or(5),
            _ => eprintln!("ignoring unknown flag {flag}"),
        }
    }
    a
}

fn pct(a: u64, b: u64) -> f64 {
    if b == 0 { 0.0 } else { 100.0 * a as f64 / b as f64 }
}

fn rel<'a>(repo: &str, path: &'a str) -> Option<&'a str> {
    path.strip_prefix(repo).and_then(|p| p.strip_prefix('/')).filter(|p| !p.is_empty())
}

#[derive(Default)]
struct Totals {
    sessions: usize,
    result_tokens: u64,
    explore_tokens: u64,
    reread_tokens: u64,
    reread_events: u64,
    failed_bash: u64,
    fingerprinted: u64,
    repeat_errors: u64,
    repeat_error_recovery_tokens: u64,
    learned_shapes: u64,
    relearned_shapes: u64,
    relearn_tokens: u64,
}

/// Tokens spent from a failing command until the same command shape succeeds
/// again in the same session (capped): what a scar/protocol would have saved.
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

fn learning_headroom(repo: &str, sessions: &[&Session], t: &mut Totals, curve: &mut BTreeMap<usize, (u64, u64)>) {
    let mut seen_paths: HashSet<&str> = HashSet::new();
    let mut seen_errors: HashSet<u64> = HashSet::new();
    let mut learned: HashSet<String> = HashSet::new();
    for (idx, s) in sessions.iter().enumerate() {
        t.sessions += 1;
        let mut explore = 0;
        let mut failed_shapes: HashSet<String> = HashSet::new();
        let mut session_learned: HashSet<String> = HashSet::new();
        for (i, e) in s.events.iter().enumerate() {
            t.result_tokens += e.result_tokens;
            match &e.kind {
                Kind::Read { path } => {
                    explore += e.result_tokens;
                    if rel(repo, path).is_some() && seen_paths.contains(path.as_str()) {
                        t.reread_tokens += e.result_tokens;
                        t.reread_events += 1;
                    }
                }
                Kind::Search => explore += e.result_tokens,
                Kind::Bash { cmd, failed, error, .. } => {
                    explore += e.result_tokens;
                    let shape = command_shape(cmd);
                    if *failed {
                        t.failed_bash += 1;
                        if let Some(fp) = error {
                            t.fingerprinted += 1;
                            if seen_errors.contains(fp) {
                                t.repeat_errors += 1;
                                t.repeat_error_recovery_tokens += recovery_tokens(s, i, &shape);
                            }
                        }
                        if !shape.is_empty() && !failed_shapes.contains(&shape) {
                            failed_shapes.insert(shape.clone());
                            if learned.contains(&shape) {
                                t.relearned_shapes += 1;
                                t.relearn_tokens += recovery_tokens(s, i, &shape);
                            }
                        }
                    } else if failed_shapes.contains(&shape) && session_learned.insert(shape.clone()) {
                        t.learned_shapes += 1;
                    }
                }
                _ => {}
            }
        }
        t.explore_tokens += explore;
        let bucket = match idx {
            0 => 1,
            1..=4 => 2,
            5..=19 => 6,
            _ => 21,
        };
        let c = curve.entry(bucket).or_default();
        c.0 += explore;
        c.1 += 1;
        // Commit this session's experience to the "collective" only after it ends.
        for e in &s.events {
            match &e.kind {
                Kind::Read { path } | Kind::Edit { path } if rel(repo, path).is_some() => {
                    seen_paths.insert(path.as_str());
                }
                Kind::Bash { error: Some(fp), .. } => {
                    seen_errors.insert(*fp);
                }
                _ => {}
            }
        }
        learned.extend(session_learned);
    }
}

/// Repo-relative touch sequence with consecutive duplicates removed.
fn touches<'a>(repo: &str, s: &'a Session) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    for e in &s.events {
        if let Kind::Read { path } | Kind::Edit { path } = &e.kind
            && let Some(p) = rel(repo, path)
            && out.last() != Some(&p)
        {
            out.push(p);
        }
    }
    out
}

#[derive(Default)]
struct Trails {
    transitions: u64,
    known_source: u64,
    trail_hit3: u64,
    popular_hit3: u64,
    new_transitions: u64,
    new_known: u64,
    new_trail_hit3: u64,
    new_popular_hit3: u64,
}

fn top3<'a>(m: &HashMap<&'a str, u64>, exclude: &str) -> Vec<&'a str> {
    let mut v: Vec<_> = m.iter().filter(|(k, _)| **k != exclude).collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    v.into_iter().take(3).map(|(k, _)| *k).collect()
}

fn trail_predictability(repo: &str, sessions: &[&Session], tr: &mut Trails) {
    let split = (sessions.len() * 4).div_ceil(5).min(sessions.len() - 1);
    let mut trail: HashMap<&str, HashMap<&str, u64>> = HashMap::new();
    let mut popular: HashMap<&str, u64> = HashMap::new();
    for s in &sessions[..split] {
        let seq = touches(repo, s);
        for w in seq.windows(2) {
            *trail.entry(w[0]).or_default().entry(w[1]).or_default() += 1;
        }
        for p in seq {
            *popular.entry(p).or_default() += 1;
        }
    }
    for s in &sessions[split..] {
        let seq = touches(repo, s);
        let mut visited: HashSet<&str> = HashSet::new();
        for w in seq.windows(2) {
            visited.insert(w[0]);
            let first_visit = !visited.contains(w[1]);
            let pop = top3(&popular, w[0]);
            let next = trail.get(w[0]).map(|m| top3(m, w[0]));
            tr.transitions += 1;
            tr.popular_hit3 += pop.contains(&w[1]) as u64;
            if let Some(n) = &next {
                tr.known_source += 1;
                tr.trail_hit3 += n.contains(&w[1]) as u64;
            }
            if first_visit {
                tr.new_transitions += 1;
                tr.new_popular_hit3 += pop.contains(&w[1]) as u64;
                if let Some(n) = &next {
                    tr.new_known += 1;
                    tr.new_trail_hit3 += n.contains(&w[1]) as u64;
                }
            }
        }
    }
}

#[derive(Default)]
struct CoEdit {
    /// Test sessions editing ≥2 files whose first edited file was seen in training.
    sessions: u64,
    /// Other edited files in those sessions, and how many were in the top-3 co-edit partners.
    targets: u64,
    hit3: u64,
}

/// Co-change sets: given the first file a session edits, do earlier sessions
/// predict the other files it will have to edit?
fn co_edit(repo: &str, sessions: &[&Session], ce: &mut CoEdit) {
    let split = (sessions.len() * 4).div_ceil(5).min(sessions.len() - 1);
    let edited = |s: &Session| {
        let mut v: Vec<String> = Vec::new();
        for e in &s.events {
            if let Kind::Edit { path } = &e.kind
                && let Some(p) = rel(repo, path)
                && !v.iter().any(|x| x == p)
            {
                v.push(p.to_string());
            }
        }
        v
    };
    let mut partners: HashMap<String, HashMap<String, u64>> = HashMap::new();
    for s in &sessions[..split] {
        let e = edited(s);
        for a in &e {
            for b in &e {
                if a != b {
                    *partners.entry(a.clone()).or_default().entry(b.clone()).or_default() += 1;
                }
            }
        }
    }
    for s in &sessions[split..] {
        let e = edited(s);
        if e.len() < 2 {
            continue;
        }
        let Some(p) = partners.get(&e[0]) else { continue };
        let mut v: Vec<_> = p.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let top: Vec<&String> = v.into_iter().take(3).map(|(k, _)| k).collect();
        ce.sessions += 1;
        for other in &e[1..] {
            ce.targets += 1;
            ce.hit3 += top.contains(&other) as u64;
        }
    }
}

#[derive(Default)]
struct Overlap {
    pairs: u64,
    pairs_shared_read: u64,
    pairs_shared_edit: u64,
    shared_edit_files: u64,
    pairs_edit_vs_read: u64,
    pairs_shared_error: u64,
}

fn concurrency(repo: &str, sessions: &[&Session], o: &mut Overlap) {
    let main: Vec<&&Session> = sessions.iter().filter(|s| !s.sidechain).collect();
    let sets: Vec<(HashSet<&str>, HashSet<&str>, HashSet<u64>)> = main
        .iter()
        .map(|s| {
            let (mut r, mut w, mut errs) = (HashSet::new(), HashSet::new(), HashSet::new());
            for e in &s.events {
                match &e.kind {
                    Kind::Read { path } => r.extend(rel(repo, path)),
                    Kind::Edit { path } => w.extend(rel(repo, path)),
                    Kind::Bash { error: Some(fp), .. } => {
                        errs.insert(*fp);
                    }
                    _ => {}
                }
            }
            (r, w, errs)
        })
        .collect();
    for i in 0..main.len() {
        for j in i + 1..main.len() {
            let (a, b) = (main[i], main[j]);
            if a.start.max(b.start) + 60_000 > a.end.min(b.end) {
                continue;
            }
            o.pairs += 1;
            let ((ra, wa, ea), (rb, wb, eb)) = (&sets[i], &sets[j]);
            o.pairs_shared_read += (!ra.is_disjoint(rb)) as u64;
            let shared_w = wa.intersection(wb).count() as u64;
            o.pairs_shared_edit += (shared_w > 0) as u64;
            o.shared_edit_files += shared_w;
            o.pairs_edit_vs_read += (!wa.is_disjoint(rb) || !wb.is_disjoint(ra)) as u64;
            o.pairs_shared_error += (!ea.is_disjoint(eb)) as u64;
        }
    }
}

fn main() {
    let a = args();
    let mut files: Vec<(Harness, PathBuf)> = jsonl_files(&a.claude).into_iter().map(|p| (Harness::Claude, p)).collect();
    files.extend(jsonl_files(&a.omp).into_iter().map(|p| (Harness::Omp, p)));
    eprintln!("parsing {} transcripts…", files.len());

    let sessions: Vec<Session> = files
        .par_iter()
        .filter_map(|(h, p)| match h {
            Harness::Claude => claude::parse(p).ok(),
            Harness::Omp => omp::parse(p).ok(),
        })
        .filter(|s| !s.is_empty() && !s.cwd.is_empty())
        .collect();

    let mut repo_cache: HashMap<String, String> = HashMap::new();
    let mut by_repo: BTreeMap<String, Vec<&Session>> = BTreeMap::new();
    for s in &sessions {
        let repo = repo_cache.entry(s.cwd.clone()).or_insert_with(|| repo_key(&s.cwd)).clone();
        by_repo.entry(repo).or_default().push(s);
    }
    for v in by_repo.values_mut() {
        v.sort_by_key(|s| s.start);
    }
    let eligible: Vec<(&String, &Vec<&Session>)> = by_repo.iter().filter(|(_, v)| v.len() >= a.min_sessions).collect();

    let (mut t, mut tr, mut o) = (Totals::default(), Trails::default(), Overlap::default());
    let mut ce = CoEdit::default();
    let mut curve: BTreeMap<usize, (u64, u64)> = BTreeMap::new();
    let mut per_repo: Vec<(String, usize, u64, u64)> = Vec::new();
    for (repo, ss) in &eligible {
        let before = (t.explore_tokens, t.reread_tokens);
        learning_headroom(repo, ss, &mut t, &mut curve);
        trail_predictability(repo, ss, &mut tr);
        co_edit(repo, ss, &mut ce);
        concurrency(repo, ss, &mut o);
        per_repo.push((repo.to_string(), ss.len(), t.explore_tokens - before.0, t.reread_tokens - before.1));
    }

    let harness_n = |h: Harness| sessions.iter().filter(|s| s.harness == h).count();
    let (r_blocks, r_nonempty, r_edits, r_narr) = sessions.iter().fold((0, 0, 0, 0), |acc, s| {
        let r = &s.reasoning;
        (acc.0 + r.thinking_blocks, acc.1 + r.thinking_nonempty, acc.2 + r.edits, acc.3 + r.edits_with_text)
    });
    let (in_tok, out_tok) = sessions.iter().fold((0u64, 0u64), |a, s| (a.0 + s.input_tokens, a.1 + s.output_tokens));

    println!("# Phase 0 — headroom report\n");
    println!(
        "Corpus: {} sessions ({} Claude Code, {} omp; {} sub-agent), {} repos; {} repos with ≥{} sessions analysed ({} sessions).",
        sessions.len(),
        harness_n(Harness::Claude),
        harness_n(Harness::Omp),
        sessions.iter().filter(|s| s.sidechain).count(),
        by_repo.len(),
        eligible.len(),
        a.min_sessions,
        t.sessions
    );
    let touches_n: usize = eligible.iter().flat_map(|(r, ss)| ss.iter().map(|s| touches(r, s).len())).sum();
    println!("In-repo file touches (Read tool + shell reads + edits, deduped consecutively): {touches_n}.");
    println!("Model tokens in corpus: {:.1}M input (incl. cache), {:.1}M output.\n", in_tok as f64 / 1e6, out_tok as f64 / 1e6);

    println!("## (a) Rediscovery across sessions (same repo, chronological)\n");
    println!("| Signal | Value |\n|---|---|");
    println!("| Tool-result tokens | {:.1}M |", t.result_tokens as f64 / 1e6);
    println!(
        "| Exploration tokens (Read/Search/Bash results) | {:.1}M ({:.0}% of tool results) |",
        t.explore_tokens as f64 / 1e6,
        pct(t.explore_tokens, t.result_tokens)
    );
    println!(
        "| Re-reads of files an earlier session already read/edited | {} events, {:.1}M tokens ({:.0}% of exploration) — *upper bound* |",
        t.reread_events,
        t.reread_tokens as f64 / 1e6,
        pct(t.reread_tokens, t.explore_tokens)
    );
    println!("| Failed shell commands (fingerprinted) | {} ({}) |", t.failed_bash, t.fingerprinted);
    println!(
        "| … same error already hit in an earlier session | {} ({:.0}% of fingerprinted), recovery {:.2}M tokens |",
        t.repeat_errors,
        pct(t.repeat_errors, t.fingerprinted),
        t.repeat_error_recovery_tokens as f64 / 1e6
    );
    println!("| Command shapes learned fail→pass | {} |", t.learned_shapes);
    println!(
        "| … re-learned although an earlier session already learned them | {} ({:.0}%), {:.2}M tokens |",
        t.relearned_shapes,
        pct(t.relearned_shapes, t.learned_shapes + t.relearned_shapes),
        t.relearn_tokens as f64 / 1e6
    );

    println!("\nVanilla learning curve — mean exploration tokens per session by session index within its repo (flat = no learning):\n");
    println!("| Session # | Sessions | Mean exploration tokens |\n|---|---|---|");
    for (b, (sum, n)) in &curve {
        let label = match b {
            1 => "1",
            2 => "2–5",
            6 => "6–20",
            _ => "21+",
        };
        println!("| {label} | {n} | {:.0} |", *sum as f64 / (*n).max(1) as f64);
    }

    println!("\n## (b) Trail predictability (train on first 80% of a repo's sessions, test on last 20%)\n");
    println!("| Next file touched | Transitions | Source file known | Trail hit@3 (when known) | Popularity hit@3 (baseline) |\n|---|---|---|---|---|");
    println!(
        "| any | {} | {:.0}% | {:.0}% | {:.0}% |",
        tr.transitions,
        pct(tr.known_source, tr.transitions),
        pct(tr.trail_hit3, tr.known_source),
        pct(tr.popular_hit3, tr.transitions)
    );
    println!(
        "| first visit in session (exploration) | {} | {:.0}% | {:.0}% | {:.0}% |",
        tr.new_transitions,
        pct(tr.new_known, tr.new_transitions),
        pct(tr.new_trail_hit3, tr.new_known),
        pct(tr.new_popular_hit3, tr.new_transitions)
    );

    println!(
        "\nCo-edit sets: {} test sessions editing ≥2 files with a known first file; {} further edited files, top-3 co-edit partners hit {:.0}%.",
        ce.sessions,
        ce.targets,
        pct(ce.hit3, ce.targets)
    );

    println!("\n## (c) Concurrent sessions in the same repo (main sessions overlapping ≥1 min)\n");
    println!("| Signal | Pairs |\n|---|---|");
    println!("| Overlapping pairs | {} |", o.pairs);
    println!("| … both read a common file | {} ({:.0}%) |", o.pairs_shared_read, pct(o.pairs_shared_read, o.pairs));
    println!("| … one edited a file the other read | {} ({:.0}%) |", o.pairs_edit_vs_read, pct(o.pairs_edit_vs_read, o.pairs));
    println!(
        "| … both edited the same file (collision risk) | {} ({:.0}%), {} files |",
        o.pairs_shared_edit,
        pct(o.pairs_shared_edit, o.pairs),
        o.shared_edit_files
    );
    println!("| … hit the same error | {} ({:.0}%) |", o.pairs_shared_error, pct(o.pairs_shared_error, o.pairs));

    println!("\n## (d) Is reasoning available for provenance?\n");
    println!("| Signal | Value |\n|---|---|");
    println!("| Thinking blocks with readable text | {} / {} ({:.0}%) |", r_nonempty, r_blocks, pct(r_nonempty as u64, r_blocks as u64));
    println!("| Edits with visible narration/intent right before | {} / {} ({:.0}%) |", r_narr, r_edits, pct(r_narr as u64, r_edits as u64));

    per_repo.sort_by_key(|r| std::cmp::Reverse(r.2));
    println!("\n## Top repos by exploration tokens\n");
    println!("| Repo | Sessions | Exploration | Re-read share |\n|---|---|---|---|");
    for (repo, n, ex, rr) in per_repo.iter().take(12) {
        let name = repo.rsplit('/').next().unwrap_or(repo);
        println!("| {name} | {n} | {:.2}M | {:.0}% |", *ex as f64 / 1e6, pct(*rr, *ex));
    }
}
