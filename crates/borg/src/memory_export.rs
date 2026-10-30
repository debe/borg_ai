//! `borg memory`: the evidence behind each lesson, for an outside store to take over long-term knowledge.
//!
//! - `borg memory export [--json] [--dir DIR] [--absorbed] [--since YYYY-MM-DD]`: read-only, 0 tokens.
//!   One evidence record per lesson of the collective DIR (default: the current directory) routes to;
//!   an off directory prints nothing. `--absorbed` lists the absorbed lessons only, `--since` the lessons
//!   whose last change (`Evidence::last_change_ms`) is on or after that UTC day.
//! - `borg memory absorb <id>... [--note TEXT]` and `borg memory release <id>...`: the lesson is now
//!   held outside Borg, or no longer (absorb.rs, which also says what an absorb does to delivery).
//!
//! **The record** (`--json`: `{"schema": SCHEMA, "collective", "lessons": [...]}`, `document`): the
//! lesson as stored (id, version, repo, question, situations, consequence, area, scope, reach `team` or
//! `repo`, overrides, supersedes and the computed `superseded_by`, source, origin, support, engineers
//! under this machine's aliases, history: the old versions' wording); its memory exactly as the
//! conscience weighs it (memory.rs `derive` over outcome.rs `read_team`, as conscience.rs `load`): the
//! Beta belief's α, β, mean and 95% equal-tailed credible interval, the per-session result counts,
//! activation, consolidated and dormant; its breadth: the distinct sessions that changed their work on
//! it and the distinct repos of those changes (team lines carry hashed repos, so counts, never names);
//! the raw counts of its placebo and tail-sample judgements from this machine's outcome log (`Judged`),
//! no estimate and no threshold; whether it is absorbed (`{note, ms, by, version}`, else null) and,
//! absorbed, its recurrence: the episodes merged into it since the absorbed version.
//!
//! Every text field but the ids (`id`, `supersedes`, `superseded_by`) and `reach` goes through notes.rs
//! `redact`, the engineers and an absorb's `by` too. A lesson withdrawn to a private collective (reach
//! `personal`, a tombstone) is left out. A field the logs do not hold is omitted, never guessed:
//! `version`, `support` and `history` for a lesson line without them (an older or hand-written line),
//! `placebo` and `tail` when the log holds no such judgement of the lesson, `recurrence` when the
//! absorb named no version (the lesson was not in the store yet) or the store no longer holds that
//! version's line.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use crate::absorb::{self, Record};
use crate::conscience::{self, Challenge};
use crate::notes::redact;
use crate::outcome::{self, Outcome};
use crate::{collective, learn, reach};

/// Protocol: the export's schema version. A change to a field's meaning or a removed field raises it;
/// an added field does not.
pub const SCHEMA: u32 = 1;

/// Which lessons `evidence` lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Filter {
    /// Only absorbed lessons.
    pub absorbed: bool,
    /// Only lessons whose last change is at or after this epoch ms.
    pub since_ms: Option<i64>,
}

/// One lesson's evidence record (module doc).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Evidence {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    pub repo: String,
    pub question: String,
    pub situations: Vec<String>,
    pub consequence: String,
    pub area: String,
    pub scope: Vec<String>,
    /// `team` or `repo` (reach.rs).
    pub reach: String,
    pub overrides: String,
    pub supersedes: Vec<String>,
    /// The lessons that list this one in their `supersedes`, sorted.
    pub superseded_by: Vec<String>,
    pub source: String,
    pub origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub support: Option<u64>,
    pub engineers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<Value>>,
    pub memory: MemoryView,
    pub breadth: Breadth,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placebo: Option<Judged>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail: Option<Judged>,
    pub absorbed: Option<Absorbed>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<usize>,
    /// The lesson's last change, epoch ms: the latest of its first correction, each later engineer
    /// joining, each of its episodes this machine holds, each outcome naming it, and its last absorb or
    /// release.
    pub last_change_ms: i64,
}

/// The memory the conscience weighs the lesson by (memory.rs `Memory`), rounded to 4 decimals.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemoryView {
    pub alpha: f64,
    pub beta: f64,
    pub mean: f64,
    /// The Beta(α, β) belief's 2.5% and 97.5% quantiles.
    pub ci95: [f64; 2],
    pub changed: u32,
    pub ignored: u32,
    pub corrected: u32,
    pub vetoed: u32,
    pub regretted: f64,
    pub tested: u32,
    pub activation: f64,
    pub consolidated: bool,
    pub dormant: bool,
}

/// The distinct sessions that changed their work on the lesson, and the distinct repos of those
/// changes: what memory.rs counts to consolidate it.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct Breadth {
    pub sessions: usize,
    pub repos: usize,
}

/// Raw judgement counts. Placebo: the judge's verdicts on the lesson where it was routed nowhere
/// (outcome.rs `PLACEBO`), and of them the ones that found the drone changed its work on its point
/// anyway. Tail: the graded checks whose tail sample showed the lesson past the pool budget (outcome.rs
/// `Outcome::tail`), and of them the ones whose model graded it as applying.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct Judged {
    pub count: u32,
    pub applied: u32,
}

/// The lesson's absorb (absorb.rs `Record`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Absorbed {
    pub note: Option<String>,
    pub ms: i64,
    pub by: String,
    pub version: Option<u32>,
}

/// Every line of the lessons file per id, in file order: the last is the lesson as the conscience
/// reads it (conscience.rs `load_unweighted`).
fn lines_by_id(lessons: &Path) -> HashMap<String, Vec<Value>> {
    let mut by: HashMap<String, Vec<Value>> = HashMap::new();
    for v in std::fs::read_to_string(lessons).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if let Some(id) = v["id"].as_str().filter(|i| !i.is_empty()) {
            by.entry(id.to_string()).or_default().push(v);
        }
    }
    by
}

/// The lesson's current version (its last line's `version`), None for an id the lines lack or a line
/// without one.
fn current_version(lines: &HashMap<String, Vec<Value>>, id: &str) -> Option<u32> {
    lines.get(id).and_then(|v| v.last()).and_then(|l| l["version"].as_u64()).and_then(|n| u32::try_from(n).ok())
}

fn episodes(line: &Value) -> impl Iterator<Item = &str> {
    line["episodes"].as_array().into_iter().flatten().filter_map(Value::as_str)
}

/// The episodes merged into the lesson since its absorb: the latest line's episodes beyond those of the
/// last line at the absorbed version. None when the absorb named no version or no line has it.
fn recurrence(lines: &[Value], r: &Record) -> Option<usize> {
    let v = u64::from(r.version?);
    let at = lines.iter().rev().find(|l| l["version"].as_u64() == Some(v))?;
    let then: HashSet<&str> = episodes(at).collect();
    Some(lines.last().map_or(0, |l| episodes(l).filter(|e| !then.contains(e)).count()))
}

/// The ms of this machine's episodes (`episodes.jsonl`), by id.
fn episode_ms(store: &Path) -> HashMap<String, i64> {
    let text = std::fs::read_to_string(store.join("episodes.jsonl")).unwrap_or_default();
    text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter_map(|v| Some((v["id"].as_str()?.to_string(), v["ms"].as_i64()?))).collect()
}

fn redact_value(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(redact(s)),
        Value::Array(a) => Value::Array(a.iter().map(redact_value).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), redact_value(v))).collect()),
        other => other.clone(),
    }
}

/// The placebo and tail judgements of every lesson in this machine's outcome log (`Judged`).
fn judgements(local: &[Outcome]) -> (HashMap<&str, Judged>, HashMap<&str, Judged>) {
    let (mut placebo, mut tail): (HashMap<&str, Judged>, HashMap<&str, Judged>) = Default::default();
    for o in local {
        if o.channel == outcome::PLACEBO && (o.kind == outcome::CHANGED || o.kind == outcome::IGNORED) {
            for l in &o.lessons {
                let j = placebo.entry(l.as_str()).or_default();
                j.count += 1;
                j.applied += u32::from(o.kind == outcome::CHANGED);
            }
        }
        if o.kind == outcome::GRADED {
            for l in &o.tail {
                let j = tail.entry(l.as_str()).or_default();
                j.count += 1;
                j.applied += u32::from(o.lessons.contains(l));
            }
        }
    }
    (placebo, tail)
}

/// The evidence records of the store `store` (the directory of its `lessons.jsonl`) that `filter`
/// lets through, by first correction then id. Never fails: missing logs are no evidence.
pub fn evidence(store: &Path, filter: &Filter) -> Vec<Evidence> {
    let path = store.join("lessons.jsonl");
    let lessons: Vec<Challenge> = conscience::load_unweighted(&path, None);
    let outcomes = outcome::read_team(&path);
    let memory = crate::memory::derive(&outcomes, &lessons, None);
    let local = outcome::read(&outcome::path_beside(&path));
    let (placebo, tail) = judgements(&local);
    let lines = lines_by_id(&path);
    let log = absorb::latest(&absorb::path(store), None);
    let episode_ms = episode_ms(store);
    // Per lesson: the sessions and repos of its changes, and its outcomes' latest ms.
    let mut changes: HashMap<&str, (HashSet<&str>, HashSet<&str>)> = HashMap::new();
    let mut last_outcome: HashMap<&str, i64> = HashMap::new();
    for o in &outcomes {
        for l in &o.lessons {
            let m = last_outcome.entry(l.as_str()).or_insert(o.ms);
            *m = (*m).max(o.ms);
            if o.kind == outcome::CHANGED && outcome::is_result(o) {
                let (s, r) = changes.entry(l.as_str()).or_default();
                s.insert(o.session.as_str());
                r.insert(o.repo.as_str());
            }
        }
    }
    let live: Vec<&Challenge> = lessons.iter().filter(|c| c.reach != reach::PERSONAL).collect();
    let mut by: HashMap<&str, BTreeSet<String>> = HashMap::new();
    for c in &live {
        for s in &c.supersedes {
            by.entry(s.as_str()).or_default().insert(c.id.clone());
        }
    }
    let mut out: Vec<(i64, Evidence)> = live
        .iter()
        .filter_map(|c| {
            let rec = log.get(&c.id);
            let absorbed = rec.filter(|r| r.absorbed());
            if filter.absorbed && absorbed.is_none() {
                return None;
            }
            let all = lines.get(&c.id).map(Vec::as_slice).unwrap_or_default();
            let line = all.last().cloned().unwrap_or(Value::Null);
            let mut last = c.origin_ms.max(c.joined.values().copied().max().unwrap_or(0));
            last = last.max(episodes(&line).filter_map(|e| episode_ms.get(e).copied()).max().unwrap_or(0));
            last = last.max(last_outcome.get(c.id.as_str()).copied().unwrap_or(0)).max(rec.map_or(0, |r| r.ms));
            if filter.since_ms.is_some_and(|s| last < s) {
                return None;
            }
            let m = &memory[&c.id];
            let (alpha, beta) = (m.alpha(), m.beta());
            let (lo, hi) = interval(alpha, beta, 0.95);
            let breadth = changes.get(c.id.as_str()).map_or_else(Breadth::default, |(s, r)| Breadth { sessions: s.len(), repos: r.len() });
            Some((
                c.origin_ms,
                Evidence {
                    id: c.id.clone(),
                    version: line["version"].as_u64().and_then(|n| u32::try_from(n).ok()),
                    repo: redact(&c.repo),
                    question: redact(&c.question),
                    situations: c.situations.iter().map(String::as_str).map(redact).collect(),
                    consequence: redact(&c.consequence),
                    area: redact(&c.area),
                    scope: c.scope.iter().map(String::as_str).map(redact).collect(),
                    reach: if c.reach == reach::REPO { reach::REPO.into() } else { "team".into() },
                    overrides: redact(&c.overrides),
                    supersedes: c.supersedes.clone(),
                    superseded_by: by.get(c.id.as_str()).map(|s| s.iter().cloned().collect()).unwrap_or_default(),
                    source: redact(&c.source),
                    origin: redact(line["origin"].as_str().unwrap_or_default()),
                    support: line["support"].as_u64(),
                    engineers: c.engineers.iter().map(String::as_str).map(redact).collect(),
                    history: line["history"].as_array().map(|h| h.iter().map(redact_value).collect()),
                    memory: MemoryView {
                        alpha: outcome::round_p(alpha),
                        beta: outcome::round_p(beta),
                        mean: outcome::round_p(m.mean()),
                        ci95: [outcome::round_p(lo), outcome::round_p(hi)],
                        changed: m.changed,
                        ignored: m.ignored,
                        corrected: m.corrected,
                        vetoed: m.vetoed,
                        regretted: outcome::round_p(m.regretted),
                        tested: m.tested,
                        activation: outcome::round_p(m.activation),
                        consolidated: m.consolidated,
                        dormant: m.dormant,
                    },
                    breadth,
                    placebo: placebo.get(c.id.as_str()).copied(),
                    tail: tail.get(c.id.as_str()).copied(),
                    absorbed: absorbed.map(|r| Absorbed { note: r.note.as_deref().map(redact), ms: r.ms, by: redact(&r.by), version: r.version }),
                    recurrence: absorbed.and_then(|r| recurrence(all, r)),
                    last_change_ms: last,
                },
            ))
        })
        .collect();
    out.sort_by(|a, b| (a.0, &a.1.id).cmp(&(b.0, &b.1.id)));
    out.into_iter().map(|(_, e)| e).collect()
}

/// The export of the store `store` as one JSON document, `{"schema": SCHEMA, "collective", "lessons"}`:
/// what `borg memory export --json` prints and borg-web's `/api/memory` serves.
pub fn document(collective: &str, store: &Path, filter: &Filter) -> Value {
    json!({"schema": SCHEMA, "collective": collective, "lessons": evidence(store, filter)})
}

/// `borg status`'s line of a store: its absorbed lessons, and how many of them recurred (a correction
/// merged into one since its absorb, `recurrence`). An absorbed id the store does not hold yet is not
/// counted.
pub fn summary(store: &Path) -> (usize, usize) {
    let lines = lines_by_id(&store.join("lessons.jsonl"));
    let state = absorb::state(&absorb::path(store), None);
    let held: Vec<(&Record, &[Value])> = state
        .values()
        .filter_map(|r| lines.get(&r.id).map(|l| (r, l.as_slice())))
        .filter(|(_, l)| l.last().is_some_and(|x| x["reach"].as_str() != Some(reach::PERSONAL)))
        .collect();
    (held.len(), held.iter().filter(|(r, l)| recurrence(l, r).is_some_and(|n| n > 0)).count())
}

// ---------------------------------------------------------------- the Beta interval

/// ln Γ(x) for x > 0 (Lanczos, g = 7, nine terms: about 15 significant digits).
fn ln_gamma(x: f64) -> f64 {
    const G: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        // Reflection: Γ(x) Γ(1 − x) = π / sin(πx).
        return (std::f64::consts::PI / (std::f64::consts::PI * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let a = G[1..].iter().enumerate().fold(G[0], |s, (i, g)| s + g / (x + i as f64 + 1.0));
    let t = x + 7.5;
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

/// The continued fraction of the incomplete beta function (modified Lentz, as in Numerical Recipes'
/// `betacf`).
fn beta_cf(a: f64, b: f64, x: f64) -> f64 {
    // Method: TINY keeps a denominator off zero; EPS is double precision; the fraction needs about
    // √max(a, b) terms, so MAX_TERMS covers beliefs of millions of outcomes and is never the stop.
    const TINY: f64 = 1e-300;
    const EPS: f64 = 1e-15;
    const MAX_TERMS: u32 = 10_000;
    let (qab, qap, qam) = (a + b, a + 1.0, a - 1.0);
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    d = if d.abs() < TINY { TINY } else { d };
    d = 1.0 / d;
    let mut h = d;
    for m in 1..=MAX_TERMS {
        let m = f64::from(m);
        let m2 = 2.0 * m;
        for aa in [m * (b - m) * x / ((qam + m2) * (a + m2)), -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2))] {
            d = 1.0 + aa * d;
            d = if d.abs() < TINY { TINY } else { d };
            c = 1.0 + aa / c;
            c = if c.abs() < TINY { TINY } else { c };
            d = 1.0 / d;
            h *= d * c;
        }
        if (d * c - 1.0).abs() < EPS {
            break;
        }
    }
    h
}

/// The regularised incomplete beta function I_x(a, b): the Beta(a, b) CDF at x.
fn beta_cdf(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let front = (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    if x < (a + 1.0) / (a + b + 2.0) { front * beta_cf(a, b, x) / a } else { 1.0 - front * beta_cf(b, a, 1.0 - x) / b }
}

/// The Beta(a, b) quantile at `p`, by bisection on the CDF. Method: the CDF is monotone, so 60 halvings
/// place it within 2⁻⁶⁰ of the root, finer than the 4 decimals the export shows.
fn beta_quantile(a: f64, b: f64, p: f64) -> f64 {
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if beta_cdf(a, b, mid) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// The equal-tailed `level` credible interval of Beta(a, b).
pub fn interval(a: f64, b: f64, level: f64) -> (f64, f64) {
    let tail = (1.0 - level) / 2.0;
    (beta_quantile(a, b, tail), beta_quantile(a, b, 1.0 - tail))
}

// ---------------------------------------------------------------- the command

/// `borg memory`'s subcommands and flags (completions.rs).
pub const SUBS: &[&str] = &["export", "absorb", "release"];
pub const FLAGS: &[&str] = &["--json", "--dir", "--absorbed", "--since", "--note"];

const USAGE: &str = "usage: borg memory export [--json] [--dir DIR] [--absorbed] [--since YYYY-MM-DD]
       borg memory absorb <id>... [--note TEXT] [--dir DIR] [--json]
       borg memory release <id>... [--dir DIR] [--json]
  export: each lesson's evidence (0 tokens); absorb: a lesson is now held outside Borg, which stops
  delivering it; release: Borg delivers it again";

/// The parsed arguments after the subcommand.
#[derive(Debug, Default, PartialEq)]
struct Args<'a> {
    ids: Vec<&'a str>,
    dir: Option<&'a str>,
    note: Option<&'a str>,
    since: Option<&'a str>,
    json: bool,
    absorbed: bool,
}

fn parse<'a>(args: &[&'a str]) -> Option<Args<'a>> {
    let mut a = Args::default();
    let mut it = args.iter();
    while let Some(w) = it.next() {
        match *w {
            "--json" => a.json = true,
            "--absorbed" => a.absorbed = true,
            "--dir" => a.dir = Some(it.next()?),
            "--note" => a.note = Some(it.next()?),
            "--since" => a.since = Some(it.next()?),
            w if w.starts_with("--") => return None,
            w => a.ids.push(w),
        }
    }
    Some(a)
}

fn usage() -> i32 {
    eprintln!("{USAGE}");
    2
}

/// `borg memory <export|absorb|release> ...`; the exit code.
pub fn main(args: &[&str]) -> i32 {
    let Some((&sub, rest)) = args.split_first() else { return usage() };
    let Some(a) = parse(rest) else { return usage() };
    let valid = match sub {
        "export" => a.ids.is_empty() && a.note.is_none(),
        "absorb" => !a.ids.is_empty() && !a.absorbed && a.since.is_none(),
        "release" => !a.ids.is_empty() && !a.absorbed && a.since.is_none() && a.note.is_none(),
        _ => false,
    };
    if !valid {
        return usage();
    }
    // Routing compares path text, so a relative --dir is made absolute first, as hooks' cwd is.
    let dir = a.dir.map_or_else(|| std::env::current_dir().unwrap_or_default(), PathBuf::from);
    let cwd = std::path::absolute(&dir).unwrap_or(dir).display().to_string();
    let route = match collective::load() {
        Ok(cfg) => collective::route(cfg.as_ref(), &cwd, &learn::home()),
        Err(e) => {
            eprintln!("borg memory: invalid collectives config ({e}); everything is off");
            return 1;
        }
    };
    let collective::Route::Collective { name, home, .. } = &route else {
        if sub == "export" {
            return 0;
        }
        eprintln!("off: {cwd} belongs to no collective; borg records nothing here");
        return 1;
    };
    match sub {
        "export" => export_cmd(name, home, &a),
        _ => absorb_cmd(sub, home, &cwd, &a),
    }
}

fn export_cmd(name: &str, home: &Path, a: &Args) -> i32 {
    let since_ms = match a.since.map(|d| (d.len() == 10).then(|| borg_trace::parse_ts(&format!("{d}T00:00:00Z"))).filter(|ms| *ms > 0)) {
        Some(None) => {
            eprintln!("borg memory export: --since wants YYYY-MM-DD\n{USAGE}");
            return 2;
        }
        s => s.flatten(),
    };
    let filter = Filter { absorbed: a.absorbed, since_ms };
    if a.json {
        println!("{}", serde_json::to_string_pretty(&document(name, home, &filter)).unwrap_or_else(|_| "null".into()));
        return 0;
    }
    print!("{}", render(name, &evidence(home, &filter)));
    0
}

/// The compact listing: a header, then per lesson its belief, breadth and absorb, and its question.
fn render(name: &str, records: &[Evidence]) -> String {
    let absorbed = records.iter().filter(|e| e.absorbed.is_some()).count();
    let mut s = format!("collective {name}: {} lessons, {absorbed} absorbed (borg memory export --json: every field)\n", records.len());
    for e in records {
        let version = e.version.map_or_else(String::new, |v| format!(" v{v}"));
        let m = &e.memory;
        let held = match &e.absorbed {
            None => String::new(),
            Some(x) => {
                let note = x.note.as_deref().map_or_else(String::new, |n| format!(": {}", crate::stats::excerpt(n, 60)));
                let recurred = e.recurrence.map_or_else(String::new, |n| format!(", recurred {n}"));
                format!("  absorbed{note}{recurred}")
            }
        };
        s.push_str(&format!(
            "{}{version}  {}  mean {:.2} ({:.2}-{:.2})  changed {} ({} sessions, {} repos)  corrected {}{held}\n",
            e.id, e.reach, m.mean, m.ci95[0], m.ci95[1], m.changed, e.breadth.sessions, e.breadth.repos, m.corrected
        ));
        s.push_str(&format!("    {}\n", crate::stats::excerpt(&e.question, 160)));
    }
    s
}

fn absorb_cmd(sub: &str, home: &Path, cwd: &str, a: &Args) -> i32 {
    let note = match absorb::note(a.note) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("borg memory absorb: {e}");
            return 2;
        }
    };
    let kind = if sub == "absorb" { absorb::ABSORB } else { absorb::RELEASE };
    let by = learn::engineer(cwd);
    // Under learn's lock on the lessons: no merge lands between reading a lesson's version and the
    // absorb that names it, and two commands here stamp in the order they write.
    let (lines, before, written) = crate::live::locked(&home.join("lessons.jsonl"), || {
        let lines = lines_by_id(&home.join("lessons.jsonl"));
        let seen = absorb::latest(&absorb::path(home), None);
        let before: HashSet<String> = seen.values().filter(|r| r.absorbed()).map(|r| r.id.clone()).collect();
        // Stamped after what it sees: a peer's clock a little ahead never makes this command lose to it.
        let now = absorb::stamp(&seen, &a.ids, learn::now_ms());
        let written = absorb::write(home, &a.ids, kind, note.as_deref(), &by, now, &|id| current_version(&lines, id));
        (lines, before, written)
    });
    let written = match written {
        Ok(w) => w,
        Err(e) => {
            eprintln!("borg memory {sub}: cannot write {}: {e}", absorb::path(home).display());
            return 1;
        }
    };
    for r in &written {
        if !lines.contains_key(&r.id) {
            eprintln!("borg memory {sub}: {} is not a lesson of this collective (yet): recorded, it applies when the lesson arrives", r.id);
        } else if kind == absorb::RELEASE && !before.contains(r.id.as_str()) {
            eprintln!("borg memory release: {} was not absorbed here (yet): recorded", r.id);
        }
    }
    if a.json {
        let out: Vec<Value> = written
            .iter()
            .map(|r| {
                let mut v = serde_json::to_value(r).unwrap_or_default();
                v["known"] = json!(lines.contains_key(&r.id));
                v
            })
            .collect();
        println!("{}", Value::Array(out));
        return 0;
    }
    for r in &written {
        match (kind, &r.note) {
            (absorb::RELEASE, _) => println!("released {}: borg delivers it again", r.id),
            (_, Some(n)) => {
                println!("absorbed {} (version {}): held in {n}; borg no longer delivers it", r.id, r.version.map_or("unknown".into(), |v| v.to_string()))
            }
            (_, None) => println!("absorbed {} (version {}): borg no longer delivers it", r.id, r.version.map_or("unknown".into(), |v| v.to_string())),
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-memory-export-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn the_beta_interval_matches_closed_forms() {
        assert!(close(ln_gamma(5.0), 24f64.ln()) && close(ln_gamma(0.5), std::f64::consts::PI.sqrt().ln()));
        // Beta(1, 1) is uniform; Beta(2, 1) has CDF x²; Beta(1, 2) has CDF 1 − (1 − x)².
        let (lo, hi) = interval(1.0, 1.0, 0.95);
        assert!(close(lo, 0.025) && close(hi, 0.975), "{lo} {hi}");
        let (lo, hi) = interval(2.0, 1.0, 0.95);
        assert!(close(lo, 0.025f64.sqrt()) && close(hi, 0.975f64.sqrt()), "{lo} {hi}");
        let (lo, hi) = interval(1.0, 2.0, 0.95);
        assert!(close(lo, 1.0 - 0.975f64.sqrt()) && close(hi, 1.0 - 0.025f64.sqrt()), "{lo} {hi}");
        // Symmetric beliefs give symmetric intervals; more evidence narrows them; fractional counts work.
        let (lo, hi) = interval(6.0, 6.0, 0.95);
        assert!(close(lo, 1.0 - hi));
        assert!(interval(60.0, 60.0, 0.95).1 - interval(60.0, 60.0, 0.95).0 < hi - lo);
        let (lo, hi) = interval(1.5, 3.0, 0.95);
        assert!(close(beta_cdf(1.5, 3.0, lo), 0.025) && close(beta_cdf(1.5, 3.0, hi), 0.975));
    }

    fn write_lines(path: &Path, lines: &[Value]) {
        std::fs::write(path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    }

    fn o(kind: &str, session: &str, lessons: &[&str], ms: i64, repo: &str, channel: &str) -> Value {
        json!({"kind": kind, "session": session, "lessons": lessons, "ms": ms, "repo": repo, "channel": channel})
    }

    #[test]
    fn the_export_holds_the_lesson_its_memory_breadth_judgements_and_absorb() {
        let store = temp("record");
        write_lines(
            &store.join("lessons.jsonl"),
            &[
                json!({"id": "La", "origin_ms": 10, "repo": "app", "question": "Is the lock held across the await?", "situations": ["send under a lock"],
                    "consequence": "a slow client stalls senders", "support": 1, "episodes": ["e1"], "history": [], "version": 1, "engineers": ["a@x"]}),
                json!({"id": "La", "origin_ms": 10, "repo": "app", "question": "Is the send lock held across any await?", "situations": ["send under a lock"],
                    "consequence": "a slow client stalls senders", "support": 2, "episodes": ["e1", "e2"],
                    "history": [{"question": "Is the lock held across the await? token ghp_0123456789abcdefghijABCDEFGHIJ012345", "consequence": "c", "version": 1}],
                    "version": 2, "engineers": ["a@x"], "area": "concurrency"}),
                json!({"id": "Lb", "origin_ms": 20, "repo": "app", "question": "Is the queue drained by one writer?", "supersedes": ["La"], "reach": "repo"}),
                json!({"id": "Lgone", "origin_ms": 30, "question": "(withdrawn)", "reach": "personal"}),
            ],
        );
        write_lines(
            &store.join("outcomes.jsonl"),
            &[
                o("asked", "s1", &["La"], 100, "app", "stop"),
                o("asked_then_changed", "s1", &["La"], 110, "app", "stop"),
                o("asked_then_changed", "s2", &["La"], 120, "web", "stop"),
                o("asked_then_corrected", "s3", &["La"], 130, "app", "stop"),
                // A placebo's verdicts are no result: counted raw, never in the belief.
                o("asked_then_changed", "s4", &["La"], 140, "app", "placebo"),
                o("asked_ignored", "s5", &["La"], 150, "app", "placebo"),
                json!({"kind": "graded", "session": "s6", "lessons": ["La"], "tail": ["La", "Lb"], "ms": 160, "repo": "app"}),
            ],
        );
        absorb::write(&store, &["La"], absorb::ABSORB, Some("skills/review@2026-10-30"), "lead@x", 200, &|_| Some(1)).unwrap();
        let all = evidence(&store, &Filter::default());
        assert_eq!(all.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["La", "Lb"], "a personal tombstone is left out");
        let a = &all[0];
        assert_eq!((a.version, a.support, a.area.as_str(), a.reach.as_str()), (Some(2), Some(2), "concurrency", "team"));
        assert_eq!(a.superseded_by, ["Lb"]);
        assert_eq!((a.memory.changed, a.memory.corrected), (2, 1));
        assert_eq!((a.memory.alpha, a.memory.beta), (3.0, 3.0), "Beta(1 + 2 changed, 1 + 2 × 1 corrected)");
        assert!(a.memory.ci95[0] < a.memory.mean && a.memory.mean < a.memory.ci95[1]);
        assert_eq!(a.breadth, Breadth { sessions: 2, repos: 2 });
        assert_eq!(a.placebo, Some(Judged { count: 2, applied: 1 }));
        assert_eq!(a.tail, Some(Judged { count: 1, applied: 1 }));
        assert_eq!(all[1].tail, Some(Judged { count: 1, applied: 0 }));
        assert_eq!(all[1].placebo, None, "no placebo judgement: omitted");
        let held = a.absorbed.as_ref().unwrap();
        assert_eq!((held.note.as_deref(), held.by.as_str(), held.version, held.ms), (Some("skills/review@2026-10-30"), "lead@x", Some(1), 200));
        assert_eq!(a.recurrence, Some(1), "e2 merged after version 1");
        assert_eq!(a.last_change_ms, 200);
        assert!(!serde_json::to_string(&a.history).unwrap().contains("ghp_0123"), "history is redacted");
        // Lb: no version, support or history on its line: omitted, not guessed; not absorbed: null.
        let b = serde_json::to_value(&all[1]).unwrap();
        assert!(b.get("version").is_none() && b.get("support").is_none() && b.get("history").is_none() && b.get("recurrence").is_none(), "{b}");
        assert_eq!((b["absorbed"].clone(), b["reach"].as_str()), (Value::Null, Some("repo")));
        // The filters, and the document.
        let only = evidence(&store, &Filter { absorbed: true, ..Filter::default() });
        assert_eq!(only.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["La"]);
        assert_eq!(evidence(&store, &Filter { since_ms: Some(170), ..Filter::default() }).len(), 1, "Lb last changed at its first correction, 20");
        let doc = document("corp", &store, &Filter::default());
        assert_eq!((doc["schema"].as_u64(), doc["collective"].as_str(), doc["lessons"].as_array().map(Vec::len)), (Some(1), Some("corp"), Some(2)));
        assert_eq!(summary(&store), (1, 1));
        // Released: no longer absorbed, nothing recurred.
        absorb::write(&store, &["La"], absorb::RELEASE, None, "lead@x", 300, &|_| None).unwrap();
        assert!(evidence(&store, &Filter::default())[0].absorbed.is_none());
        assert_eq!(summary(&store), (0, 0));
        let _ = std::fs::remove_dir_all(store);
    }

    #[test]
    fn recurrence_needs_the_absorbed_version() {
        let lines = vec![json!({"id": "L", "version": 1, "episodes": ["e1"]}), json!({"id": "L", "version": 3, "episodes": ["e1", "e2", "e3"]})];
        let r = |v: Option<u32>| Record { id: "L".into(), kind: absorb::ABSORB.into(), ms: 1, by: String::new(), note: None, version: v };
        assert_eq!(recurrence(&lines, &r(Some(1))), Some(2));
        assert_eq!(recurrence(&lines, &r(Some(3))), Some(0));
        assert_eq!(recurrence(&lines, &r(Some(2))), None, "no line at that version: not guessed");
        assert_eq!(recurrence(&lines, &r(None)), None, "absorbed before the lesson arrived");
    }

    #[test]
    fn a_correction_learned_after_the_absorb_merges_into_it_and_recurs() {
        let store = temp("learn");
        let lesson = json!({"id": "La", "origin_ms": 1, "repo": "app", "question": "Is the send lock held across any await?", "situations": ["send under a lock"],
            "consequence": "a slow client stalls senders", "support": 1, "episodes": ["e1"], "history": [], "version": 1});
        write_lines(&store.join("lessons.jsonl"), &[lesson]);
        absorb::write(&store, &["La"], absorb::ABSORB, Some("skills/review@1"), "lead@x", 5, &|id| {
            current_version(&lines_by_id(&store.join("lessons.jsonl")), id)
        })
        .unwrap();
        let e = learn::Episode {
            id: "e9".into(),
            ms: 10,
            repo: "app".into(),
            session: "s9".into(),
            task: "send frames".into(),
            agent: "I hold the send lock across the await".into(),
            correction: "no, the send lock must never be held across an await".into(),
            engineer: "b@x".into(),
            ..Default::default()
        };
        learn::record_episode_in(&store, &e).unwrap();
        // Learning still sees the absorbed lesson: it is offered for merging.
        let ask = |p: &str| {
            assert!(p.contains("La"), "the absorbed lesson is a merge candidate: {p}");
            Some(json!({"is_lesson": true, "question": "Is any lock held across an await?", "situations": ["send under a lock"],
                "consequence": "a slow client stalls senders", "same_as": "La", "area": "concurrency", "supersedes": [], "reach": "team"}))
        };
        assert_eq!(learn::learn_in(&store, None, &ask, false, 20), (0, 1, 0));
        let a = evidence(&store, &Filter::default()).remove(0);
        assert_eq!((a.version, a.recurrence), (Some(2), Some(1)));
        assert_eq!(summary(&store), (1, 1));
        let _ = std::fs::remove_dir_all(store);
    }

    #[test]
    fn the_arguments_parse() {
        assert_eq!(
            parse(&["La", "Lb", "--note", "skills/x", "--dir", "/w", "--json"]),
            Some(Args { ids: vec!["La", "Lb"], dir: Some("/w"), note: Some("skills/x"), json: true, ..Args::default() })
        );
        assert_eq!(parse(&["--since"]), None);
        assert_eq!(parse(&["--frobnicate"]), None);
    }
}
