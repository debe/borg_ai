//! `borg review-fix`: the bugs a review step found and a fix step fixed, inside a session, before
//! anything was committed. Most bugs are caught there (a build → review → verify → fix workflow), and a
//! commit-based signal (regress links: a later commit from another session) never sees them.
//!
//! Source: each harness's runs of sub-agents (`Harness::sub_runs`: a Claude Code workflow run, an omp
//! session's task agents, an opencode session's subagents), each agent with its label, phase, kind,
//! start, prompt, work and output. Workflow scripts and prompts are the engineer's own, so output
//! shapes vary; this reads them tolerantly from each agent's output (Claude's last `StructuredOutput`,
//! omp's yields, else a JSON object in its text, [`output_of`]):
//! - **finding**: an object in a `findings` or `issues` array with a severity (or a `priority`) and a
//!   text (`title`, `claim`, `problem`, `description`, `issue` or `body`), and no verdict of its own;
//! - **verdict**: an output with a boolean `real`, or items of a `verdicts`/`results` array with `real`
//!   or a `verdict` word (confirmed, refuted, …), linked to a finding of the same run by title (or, for
//!   a one-finding verifier, by its label or prompt naming the title);
//! - **fixed**: a fix agent of the run ([`is_fix`]), started after the finding, whose tool
//!   calls or final text name the finding's file. Agents edit mostly through Bash, so tool inputs of
//!   every kind count, not only Edit/Write.
//!
//! A finding is **strong** when it was fixed and no verifier rejected it. The negative control asks the
//! same "fixed" question of a fix agent from another session's run: its rate is how often the file rule
//! matches by chance.
//!
//! 0 tokens: nothing calls a model. Every finding is routed by its agent's cwd (`collective::route`); an
//! off repo is only counted. Claude Code's plain sub-agents outside workflows (the Agent tool) are not
//! read yet.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::collective::{self, Config, Route};
use crate::harness::{self, Harness, Run, SubAgent};
use crate::learn::{self, Episode, regret};

/// Its flags (completions.rs).
pub const FLAGS: &[&str] = &["--home", "--since", "--json", "--apply", "--only"];

const USAGE: &str = "usage: borg review-fix [--home DIR] [--since YYYY-MM-DD] [--json] [--apply [--only ID,…]]";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Confirmed,
    Rejected,
    /// No verifier's verdict links to it.
    Unchecked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fixed {
    Yes,
    /// The run had a fix step after the finding, and it never named the finding's file.
    No,
    /// The run had no fix step after the finding.
    NoFixStep,
    /// The finding names no file to look for.
    Unlocated,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// `<runId>#<n>`, n counting the run's findings in agent start order: stable across replays.
    pub id: String,
    pub session: String,
    pub run: String,
    pub workflow: String,
    /// The reviewing agent's label (`review:rust`).
    pub agent: String,
    pub ms: i64,
    pub cwd: String,
    pub file: Option<String>,
    pub title: String,
    /// The reviewer's explanation (`detail`, `evidence` or `reason`), at most `DETAIL_CHARS`.
    pub detail: String,
    pub severity: String,
    pub verdict: Verdict,
    pub fixed: Fixed,
    /// The prompt of the run's builder (its first agent that neither reviews, verifies nor fixes),
    /// at most `DETAIL_CHARS` × 2; empty without one.
    pub task: String,
    /// The end of the builder's own account of its work, at most `DETAIL_CHARS` × 2.
    pub built: String,
}

impl Finding {
    /// Fixed, and no verifier said it was not real.
    pub fn strong(&self) -> bool {
        self.fixed == Fixed::Yes && self.verdict != Verdict::Rejected
    }

    /// The episode `borg learn` generalises with the review regret prompt (learn/regret.rs). Its id is
    /// `V` + the finding's id, the dedupe key; its engineer is whoever works in the finding's repo.
    /// Every text is redacted before it is written.
    pub fn episode(&self, engineer: &str) -> Episode {
        let red = |t: &str| crate::notes::redact(t);
        let verdict = match self.verdict {
            Verdict::Confirmed => "confirmed",
            _ => "unchecked",
        };
        let detail = match &self.file {
            Some(f) => format!("{f}: {}", self.detail),
            None => self.detail.clone(),
        };
        let r = regret::Regret {
            decision: self.run.clone(),
            decision_session: self.session.clone(),
            decision_task: red(&self.task),
            decision_proposal: red(&self.built),
            fix: self.id.clone(),
            fix_subject: red(&self.title),
            diff: red(&detail),
            signals: vec![regret::REVIEW.into(), verdict.into()],
            // The builder decided before the reviewer found it.
            decision_ms: self.ms,
            ..regret::Regret::default()
        };
        Episode {
            id: format!("V{}", self.id),
            ms: self.ms,
            repo: borg_trace::repo_key(&self.cwd).rsplit('/').next().unwrap_or_default().to_string(),
            session: self.session.clone(),
            task: r.decision_task.clone(),
            agent: r.decision_proposal.clone(),
            rejected_tool: None,
            correction: format!("Review finding ({}, {verdict}), fixed before any commit: {}\n{}", self.severity, r.fix_subject, r.diff),
            engineer: engineer.to_string(),
            kind: regret::KIND.into(),
            regret: Some(r),
        }
    }
}

/// The lowercase words of a label, phase or kind: split at non-alphanumerics and at CamelCase humps
/// (`MappingValidityFix` → mapping, validity, fix).
fn words(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if !c.is_alphanumeric() {
            prev_lower = false;
            out.push(String::new());
            continue;
        }
        if c.is_uppercase() && prev_lower || out.is_empty() {
            out.push(String::new());
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        out.last_mut().expect("pushed").extend(c.to_lowercase());
    }
    out.retain(|w| !w.is_empty());
    out
}

/// Says fix or repair as a whole word (`fix`, `harness-fix`, `DeriveFacetsRepairs`), and neither
/// review, verify nor implement.
fn says_fix(s: &str) -> bool {
    let w = words(s);
    w.iter().any(|w| ["fix", "fixes", "fixer", "fixing", "repair", "repairs", "repairer"].contains(&w.as_str()))
        && !w.iter().any(|w| ["review", "verif", "impl"].iter().any(|n| w.starts_with(n)))
}

/// A fix step: its phase, its label's first segment (`fix:rust`, not `verify:fix-correctness`) or its
/// kind says fix (or repair), it is in no review, verify or implement phase, and its output has no
/// findings (not even an empty list) and no verdicts.
fn is_fix(a: &SubAgent) -> bool {
    let phase = words(&a.phase);
    if phase.iter().any(|w| ["review", "verif", "impl"].iter().any(|n| w.starts_with(n))) {
        return false;
    }
    let first = a.label.split([':', '#']).next().unwrap_or_default();
    let listed = a.output.as_ref().is_some_and(|o| ["findings", "issues"].iter().any(|k| o.get(*k).is_some_and(Value::is_array)));
    (says_fix(&a.phase) || says_fix(first) || says_fix(&a.kind)) && !listed && verdicts_of(a.output.as_ref()).is_empty()
}

/// An agent's output: its structured one, else the last JSON object in its final text that has a
/// `findings` or `issues` array and starts a line (a fenced block's too). A harness without structured
/// output (opencode) yields findings only from a reviewer told to answer in JSON; tool inputs are never
/// read, so a fix agent repeating the findings stays a fix agent.
fn output_of(a: &SubAgent) -> Option<Value> {
    if a.output.is_some() {
        return a.output.clone();
    }
    let w = a.final_text.as_str();
    let listed = |v: &Value| ["findings", "issues"].iter().any(|k| v.get(*k).is_some_and(Value::is_array));
    let starts = w.match_indices('{').map(|(i, _)| i).filter(|&i| w[w[..i].rfind('\n').map_or(0, |n| n + 1)..i].trim().is_empty());
    starts.rev().find_map(|i| serde_json::Deserializer::from_str(&w[i..]).into_iter::<Value>().next()?.ok().filter(listed))
}

/// A finding's detail is cut to this many characters.
pub const DETAIL_CHARS: usize = 800;

const TEXT_KEYS: &[&str] = &["title", "claim", "problem", "description", "issue", "body"];
const DETAIL_KEYS: &[&str] = &["detail", "evidence", "reason", "body"];
const FILE_KEYS: &[&str] = &["file", "path", "file_path", "fileLines", "where"];

fn field<'a>(o: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| o.get(*k).and_then(Value::as_str)).map(str::trim).filter(|s| !s.is_empty())
}

/// A verdict word or flag (`real`, `keep`, `refuted`), when `o` carries one.
fn verdict_of(o: &Value) -> Option<Verdict> {
    let flag = |k: &str| o.get(k).and_then(Value::as_bool);
    if let Some(r) = flag("real").or_else(|| flag("keep")).or_else(|| flag("refuted").map(|r| !r)) {
        return Some(if r { Verdict::Confirmed } else { Verdict::Rejected });
    }
    let w = o.get("verdict").and_then(Value::as_str)?.to_lowercase();
    // A plausible finding survived the attempt to refute it.
    if ["confirm", "real", "valid", "holds", "true", "plausible"].iter().any(|k| w.starts_with(k)) {
        Some(Verdict::Confirmed)
    } else if ["refut", "reject", "false", "invalid", "not"].iter().any(|k| w.starts_with(k)) {
        Some(Verdict::Rejected)
    } else {
        None
    }
}

/// A finding as a reviewer's output states it.
#[derive(Debug, PartialEq)]
struct Raw {
    id: Option<String>,
    title: String,
    detail: String,
    severity: String,
    file: Option<String>,
}

/// The findings in an agent's output.
fn findings_of(output: Option<&Value>) -> Vec<Raw> {
    let Some(o) = output else { return Vec::new() };
    ["findings", "issues"]
        .iter()
        .filter_map(|k| o.get(*k).and_then(Value::as_array))
        .flatten()
        .filter(|f| f.is_object() && verdict_of(f).is_none())
        .filter_map(|f| {
            let title = field(f, TEXT_KEYS)?;
            let severity = severity_of(f)?;
            let detail = field(f, DETAIL_KEYS).filter(|d| *d != title).unwrap_or_default().chars().take(DETAIL_CHARS).collect();
            Some(Raw { id: field(f, &["id"]).map(String::from), title: title.to_string(), detail, severity, file: field(f, FILE_KEYS).and_then(path_in) })
        })
        .collect()
}

/// A finding's severity, lowercase: its `severity`, else its `priority` (0 to 3, `P1`) as `p0` to `p3`.
fn severity_of(f: &Value) -> Option<String> {
    if let Some(s) = field(f, &["severity"]) {
        return Some(s.to_lowercase());
    }
    match f.get("priority")? {
        Value::Number(n) => n.as_u64().map(|n| format!("p{n}")),
        Value::String(s) => {
            let s = s.trim().to_lowercase();
            let n = s.strip_prefix('p').unwrap_or(&s);
            Some(if n.parse::<u8>().is_ok() { format!("p{n}") } else { s }).filter(|s| !s.is_empty())
        }
        _ => None,
    }
}

/// A verdict and what it names: a finding's id or title, or nothing (one verifier per finding).
#[derive(Debug)]
struct Said {
    id: Option<String>,
    title: Option<String>,
    verdict: Verdict,
}

/// The verdicts in an agent's output.
fn verdicts_of(output: Option<&Value>) -> Vec<Said> {
    let Some(o) = output else { return Vec::new() };
    if o.get("findings").is_none()
        && let Some(verdict) = verdict_of(o)
    {
        return vec![Said { id: None, title: None, verdict }];
    }
    ["verdicts", "results"]
        .iter()
        .filter_map(|k| o.get(*k).and_then(Value::as_array))
        .flatten()
        .filter_map(|x| Some(Said { id: field(x, &["id"]).map(String::from), title: field(x, TEXT_KEYS).map(String::from), verdict: verdict_of(x)? }))
        .collect()
}

/// The part of a label after its last `:` (`verify:rust` → `rust`), the dimension both sides share.
fn dim(label: &str) -> &str {
    label.rsplit(':').next().unwrap_or_default()
}

/// The first path-like token of `s` (`src/a.rs:12-14 (…)` → `src/a.rs`).
fn path_in(s: &str) -> Option<String> {
    s.split(|c: char| c.is_whitespace() || "`'\"(),;".contains(c))
        .map(|t| t.split(':').next().unwrap_or(t).trim_end_matches(['.', '§']))
        .find(|t| t.contains('.') && t.len() > 2 && !t.starts_with("http") && t.chars().any(char::is_alphabetic))
        .map(String::from)
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// `a` and `b` name the same finding: one's normalised text starts with the other's (at least 12
/// characters, so a label's truncated title still links).
fn same_title(a: &str, b: &str) -> bool {
    let (a, b) = (norm(a), norm(b));
    let n = a.len().min(b.len());
    n >= 12 && a[..n] == b[..n]
}

/// Does `work` name `file`? The path as written, or its last two components.
fn names(work: &str, file: &str) -> bool {
    let tail: Vec<&str> = file.rsplit('/').take(2).collect();
    let short = tail.iter().rev().copied().collect::<Vec<_>>().join("/");
    work.contains(file) || (short.len() > 3 && work.contains(&short))
}

/// The findings of one run, with their verdicts and whether a fix step fixed them.
fn run_findings(run: &Run) -> Vec<Finding> {
    let agents = &run.agents;
    let verdicts: Vec<(&SubAgent, Said)> = agents.iter().flat_map(|a| verdicts_of(a.output.as_ref()).into_iter().map(move |v| (a, v))).collect();
    let raws: Vec<(&SubAgent, Raw)> = agents.iter().flat_map(|a| findings_of(a.output.as_ref()).into_iter().map(move |r| (a, r))).collect();
    // The builder: the first agent that produced neither findings nor verdicts and is no fix step.
    let builder = agents.iter().find(|b| !is_fix(b) && findings_of(b.output.as_ref()).is_empty() && verdicts_of(b.output.as_ref()).is_empty());
    let n = DETAIL_CHARS * 2;
    let builder_repo = builder.map(|b| borg_trace::repo_key(&b.cwd));
    let (task, built) = builder.map_or_else(Default::default, |b| {
        let w: Vec<char> = b.work.chars().collect();
        (b.prompt.chars().take(n).collect::<String>(), w[w.len().saturating_sub(n)..].iter().collect::<String>())
    });
    let mut out = Vec::new();
    for (k, (a, Raw { id, title, detail, severity, file })) in raws.iter().enumerate() {
        let head: String = title.chars().take(60).collect();
        // An id links when it names one finding of the run, or one of the verifier's own dimension
        // (reviewers number their findings F1, F2, … each); a title by its text; a verifier that
        // names neither checks one finding, named by its label or prompt.
        let same_id = |v: &str| raws.iter().filter(|(_, r)| r.id.as_deref() == Some(v)).count();
        let linked = verdicts.iter().filter(|(va, said)| match (&said.id, &said.title) {
            (Some(v), _) if id.as_deref() == Some(v.as_str()) => same_id(v) == 1 || dim(&va.label) == dim(&a.label),
            (_, Some(t)) => same_title(t, title),
            (Some(_), None) => false,
            (None, None) => same_title(dim(&va.label), title) || va.prompt.contains(&head),
        });
        let verdict =
            linked.fold(
                Verdict::Unchecked,
                |acc, (_, said)| if said.verdict == Verdict::Rejected || acc == Verdict::Rejected { Verdict::Rejected } else { said.verdict },
            );
        let fixers: Vec<&SubAgent> = agents.iter().filter(|f| is_fix(f) && f.started >= a.started).collect();
        let fixed = match file {
            _ if fixers.is_empty() => Fixed::NoFixStep,
            None => Fixed::Unlocated,
            Some(f) if fixers.iter().any(|x| names(&x.work, f)) => Fixed::Yes,
            Some(_) => Fixed::No,
        };
        // The builder's text only when it worked in the reviewer's repo: the episode is routed by
        // the reviewer's, and another repo's text must not land in its collective.
        let same_repo = builder_repo.as_deref() == Some(borg_trace::repo_key(&a.cwd).as_str());
        out.push(Finding {
            id: format!("{}#{k}", run.id),
            session: run.session.clone(),
            run: run.id.clone(),
            workflow: run.name.clone(),
            agent: a.label.clone(),
            ms: a.started,
            cwd: a.cwd.clone(),
            file: file.clone(),
            title: title.clone(),
            detail: detail.clone(),
            severity: severity.clone(),
            verdict,
            fixed,
            task: if same_repo { task.clone() } else { String::new() },
            built: if same_repo { built.clone() } else { String::new() },
        });
    }
    out
}

/// The runs of `harnesses` under `home` (of `session` only, when given), oldest first, each agent's
/// output read from its text where it has no structured one ([`output_of`]).
fn runs_of<'h>(harnesses: impl Iterator<Item = &'h dyn Harness>, home: &Path, since: Option<i64>, session: Option<&str>) -> Vec<Run> {
    prepared(harnesses.flat_map(|h| h.sub_runs(home, session, since)).collect())
}

fn prepared(mut runs: Vec<Run>) -> Vec<Run> {
    for a in runs.iter_mut().flat_map(|r| r.agents.iter_mut()) {
        a.output = output_of(a);
    }
    runs.sort_by_key(|r| r.started);
    runs
}

/// The findings of `runs`, and every run with a fix step, for the control.
fn scan(runs: &[Run]) -> (Vec<Finding>, Vec<Fixers>) {
    let mut findings = Vec::new();
    let mut fixers = Vec::new();
    for run in runs {
        findings.extend(run_findings(run));
        let fix: Vec<&SubAgent> = run.agents.iter().filter(|a| is_fix(a)).collect();
        if let Some(first) = fix.first() {
            let repo = borg_trace::repo_key(&first.cwd);
            fixers.push(Fixers { session: run.session.clone(), run: run.id.clone(), repo, work: fix.iter().map(|a| a.work.clone()).collect() });
        }
    }
    (findings, fixers)
}

/// A run's fix steps: what they did, and the repo they did it in.
#[derive(Debug)]
struct Fixers {
    session: String,
    run: String,
    repo: String,
    work: Vec<String>,
}

/// Counts for one collective, or for the off repos (only counted).
#[derive(Debug, Default, Clone, Serialize)]
pub struct Tally {
    pub runs: usize,
    pub findings: usize,
    pub confirmed: usize,
    pub rejected: usize,
    pub unchecked: usize,
    pub fixed: usize,
    pub not_fixed: usize,
    pub no_fix_step: usize,
    pub unlocated: usize,
    pub strong: usize,
}

impl Tally {
    fn add(&mut self, f: &Finding) {
        self.findings += 1;
        match f.verdict {
            Verdict::Confirmed => self.confirmed += 1,
            Verdict::Rejected => self.rejected += 1,
            Verdict::Unchecked => self.unchecked += 1,
        }
        match f.fixed {
            Fixed::Yes => self.fixed += 1,
            Fixed::No => self.not_fixed += 1,
            Fixed::NoFixStep => self.no_fix_step += 1,
            Fixed::Unlocated => self.unlocated += 1,
        }
        self.strong += usize::from(f.strong());
    }
}

/// The negative control: of the located findings, how often the fix step of another session's run in
/// the same repo names the finding's file (chance: the same files keep being worked on), against how
/// often the run's own fix step does. Findings with no such other run are `unpaired`.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Control {
    pub unpaired: usize,
    pub located: usize,
    pub own_named: usize,
    pub other_named: usize,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub collectives: BTreeMap<String, Tally>,
    pub off: Tally,
    pub control: Control,
    /// The findings in repos that route to a collective (never off ones).
    pub findings: Vec<Finding>,
}

/// Route, count and control the findings of every harness's runs under `home`.
pub fn run(home: &Path, since: Option<i64>, cfg: Option<&Config>, base: &Path) -> Report {
    report(&runs_of(harness::ALL.iter().copied(), home, since, None), cfg, base)
}

/// Route, count and control the findings of `runs`.
fn report(runs: &[Run], cfg: Option<&Config>, base: &Path) -> Report {
    let (findings, fixers) = scan(runs);
    let mut r = Report::default();
    let mut runs: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
    for f in findings {
        let name = match collective::route(cfg, &f.cwd, base) {
            Route::Off => None,
            Route::Collective { name, .. } => Some(name),
        };
        let Some(name) = name else {
            r.off.add(&f);
            continue;
        };
        if let Some(file) = f.file.as_deref().filter(|_| f.fixed != Fixed::NoFixStep) {
            // The control pairs each finding with the next run, in time order (wrapping), of another
            // session in the same repo that has a fix step.
            let repo = borg_trace::repo_key(&f.cwd);
            let at = fixers.iter().position(|x| x.run == f.run).map_or(0, |i| i + 1);
            match fixers.iter().cycle().skip(at).take(fixers.len()).find(|x| x.session != f.session && x.repo == repo) {
                Some(other) => {
                    r.control.located += 1;
                    r.control.own_named += usize::from(f.fixed == Fixed::Yes);
                    r.control.other_named += usize::from(other.work.iter().any(|w| names(w, file)));
                }
                None => r.control.unpaired += 1,
            }
        }
        runs.entry(name.clone()).or_default().insert(f.run.clone());
        r.collectives.entry(name).or_default().add(&f);
        r.findings.push(f);
    }
    for (name, set) in runs {
        r.collectives.entry(name).or_default().runs = set.len();
    }
    r
}

fn pct(n: usize, d: usize) -> String {
    if d == 0 { "-".into() } else { format!("{:.0}%", 100.0 * n as f64 / d as f64) }
}

pub fn render(r: &Report) -> String {
    let mut s = String::new();
    for (name, t) in &r.collectives {
        s.push_str(&format!(
            "{name}: {} findings in {} workflow runs; verdict: {} confirmed, {} rejected, {} unchecked; fix: {} fixed, {} not named by the fix step, {} without a fix step, {} without a file; {} strong\n",
            t.findings, t.runs, t.confirmed, t.rejected, t.unchecked, t.fixed, t.not_fixed, t.no_fix_step, t.unlocated, t.strong
        ));
    }
    if r.collectives.is_empty() {
        s.push_str("no workflow findings in repos of a collective\n");
    }
    let c = &r.control;
    s.push_str(&format!(
        "control: of {} located findings, the run's own fix step names the file {} ({}), another session's fix step in the same repo {} ({}); {} without such a session\n",
        c.located,
        c.own_named,
        pct(c.own_named, c.located),
        c.other_named,
        pct(c.other_named, c.located),
        c.unpaired
    ));
    s.push_str(&format!("off: {} findings skipped (not shown)\n", r.off.findings));
    s
}

/// Record the strong findings (those in `only`, when given) as episodes in their collectives' stores,
/// skipping known ones, then learn each store that got new episodes. Returns (recorded, learned
/// lessons, merged).
pub fn apply(report: &Report, only: Option<&[String]>, cfg: Option<&Config>, base: &Path, ask: &dyn Fn(&str) -> Option<Value>) -> (usize, usize, usize) {
    let (recorded, homes) = record(report.findings.iter().filter(|f| only.is_none_or(|o| o.contains(&f.id))), cfg, base);
    let (mut new, mut merged) = (0, 0);
    for home in &homes {
        let (n, m, _) = learn::learn_in_reporting(home, None, ask, learn::learning(), learn::now_ms(), &mut learn::print_step);
        new += n;
        merged += m;
    }
    (recorded, new, merged)
}

/// Record the strong ones of `findings` as episodes in the stores their repos route to, skipping
/// known ones and off repos. Returns how many were recorded and the stores that got any.
fn record<'a>(findings: impl Iterator<Item = &'a Finding>, cfg: Option<&Config>, base: &Path) -> (usize, Vec<PathBuf>) {
    let mut homes: Vec<PathBuf> = Vec::new();
    let mut recorded = 0;
    let mut engineers: BTreeMap<String, String> = BTreeMap::new();
    for f in findings.filter(|f| f.strong()) {
        let Route::Collective { home, .. } = collective::route(cfg, &f.cwd, base) else { continue };
        let engineer = engineers.entry(f.cwd.clone()).or_insert_with(|| learn::engineer(&f.cwd)).clone();
        let e = f.episode(&engineer);
        if crate::notes::regret::known(&home, &e.id) {
            continue;
        }
        if learn::record_episode_in(&home, &e).is_ok() {
            recorded += 1;
            crate::stats::count(&home, crate::stats::REVIEW_FINDING, &f.session, |ev| {
                ev.engineer = engineer.clone();
                ev.repo = e.repo.clone();
                ev.why = f.severity.clone();
                ev.text = e.regret.as_ref().map(|r| r.fix_subject.clone()).unwrap_or_default();
            });
            if !homes.contains(&home) {
                homes.push(home);
            }
        }
    }
    (recorded, homes)
}

/// Session end records the session's strong review findings as episodes before `borg learn`
/// generalises them: on by default (the value eval, README), `BORG_REVIEW_FIX=off` turns it off.
pub fn on() -> bool {
    std::env::var("BORG_REVIEW_FIX").as_deref() != Ok("off")
}

/// At the end of `session`: record the strong findings of its runs under `home`, as the drone's harness
/// writes them (`BORG_HARNESS`; every harness when unset). Returns how many were recorded and the
/// stores that got any, so the caller learns each.
pub fn record_session(home: &Path, session: &str, cfg: Option<&Config>, base: &Path) -> (usize, Vec<PathBuf>) {
    // A session id names a directory or a file, never a path.
    if session.is_empty() || session.contains(['/', '\\']) || session.starts_with('.') {
        return (0, Vec::new());
    }
    let runs = match harness::named_current() {
        Some(h) => runs_of(std::iter::once(h), home, None, Some(session)),
        None => runs_of(harness::ALL.iter().copied(), home, None, Some(session)),
    };
    record(scan(&runs).0.iter(), cfg, base)
}

/// The engineer's home, where every harness keeps its data.
pub fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

/// `borg review-fix [--home DIR] [--since YYYY-MM-DD] [--json] [--apply [--only ID,…]]`; the exit code.
pub fn main(args: &[&str], ask: &dyn Fn(&str) -> Option<Value>) -> i32 {
    let mut home = None;
    let mut since = None;
    let mut json = false;
    let mut apply_it = false;
    let mut only: Option<Vec<String>> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--json" => json = true,
            "--apply" => apply_it = true,
            "--only" => only = it.next().map(|v| v.split(',').map(str::to_string).collect()),
            "--home" => home = it.next().map(PathBuf::from),
            "--since" => match it.next().map(|d| borg_trace::parse_ts(&format!("{d}T00:00:00Z"))) {
                Some(ms) if ms > 0 => since = Some(ms),
                _ => {
                    eprintln!("borg review-fix: --since wants YYYY-MM-DD\n{USAGE}");
                    return 2;
                }
            },
            _ => {
                eprintln!("{USAGE}");
                return 2;
            }
        }
    }
    let home = home.unwrap_or_else(home_dir);
    // A broken config routes everything off, exactly as in the hook.
    let cfg = match collective::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("borg review-fix: invalid collectives config ({e}): every repo is off");
            return 1;
        }
    };
    let report = run(&home, since, cfg.as_ref(), &learn::home());
    if apply_it {
        let (recorded, new, merged) = apply(&report, only.as_deref(), cfg.as_ref(), &learn::home(), ask);
        println!("{recorded} review findings recorded as episodes; {new} new lessons, {merged} merged");
        return 0;
    }
    if json {
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        print!("{}", render(&report));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The Claude Code workflow runs under `projects`, as `borg review-fix` reads them.
    fn claude_runs(projects: &Path, session: Option<&str>) -> Vec<Run> {
        prepared(harness::claude::workflow_runs(projects, session, None))
    }

    fn found(projects: &Path) -> Vec<Finding> {
        scan(&claude_runs(projects, None)).0
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-review-fix-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// A transcript: the prompt, then tool calls (name, input), then the structured output, if any.
    fn transcript(prompt: &str, tools: &[(&str, Value)], output: Option<Value>) -> String {
        let mut lines = vec![json!({"type": "user", "cwd": "/w/app", "message": {"role": "user", "content": prompt}})];
        for (name, input) in tools {
            lines.push(json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "name": name, "input": input}]}}));
        }
        if let Some(o) = output {
            lines.push(json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "StructuredOutput", "input": o}]}}));
        }
        lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")
    }

    /// One run in `projects/<slug>/<session>` with agents (id, label, phase, started, transcript).
    fn write_run(projects: &Path, session: &str, run: &str, agents: &[(&str, &str, &str, i64, String)]) {
        let dir = projects.join("-w-app").join(session);
        let ad = dir.join("subagents/workflows").join(run);
        std::fs::create_dir_all(&ad).unwrap();
        std::fs::create_dir_all(dir.join("workflows")).unwrap();
        let progress: Vec<Value> = agents
            .iter()
            .map(|(id, label, phase, at, t)| {
                std::fs::write(ad.join(format!("agent-{id}.jsonl")), t).unwrap();
                json!({"type": "workflow_agent", "label": label, "phaseTitle": phase, "agentId": id, "startedAt": at.to_string()})
            })
            .collect();
        let r = json!({"runId": run, "workflowName": "build", "startTime": agents[0].3, "workflowProgress": progress});
        std::fs::write(dir.join("workflows").join(format!("{run}.json")), r.to_string()).unwrap();
    }

    const TITLE: &str = "Pre-aborted signal returns before the error listener is attached";

    fn review() -> String {
        transcript("review the diff", &[], Some(json!({"findings": [{"file": "adapters/omp/borg.ts", "line": 153, "severity": "Medium", "title": TITLE}]})))
    }

    #[test]
    fn a_confirmed_finding_the_fix_step_touches_is_strong() {
        let p = temp("strong");
        let verify = transcript(&format!("Verify: {TITLE}"), &[], Some(json!({"real": true, "reason": "probe crashed"})));
        let fix = transcript("apply the findings", &[("Bash", json!({"command": "sed -i 's/a/b/' adapters/omp/borg.ts"}))], None);
        write_run(
            &p,
            "s1",
            "wf_1",
            &[
                ("a1", "review:concurrency", "Review", 10, review()),
                ("a2", "verify:concurrency:Pre-aborted signal ret", "Verify", 20, verify),
                ("a3", "fix", "Fix", 30, fix),
            ],
        );
        let f = found(&p);
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].verdict, f[0].fixed, f[0].severity.as_str()), (Verdict::Confirmed, Fixed::Yes, "medium"));
        assert!(f[0].strong());
        assert_eq!(f[0].cwd, "/w/app");
        let _ = std::fs::remove_dir_all(p);
    }

    #[test]
    fn a_rejected_or_unfixed_finding_is_not_strong() {
        let p = temp("weak");
        let verify = transcript("check it", &[], Some(json!({"verdicts": [{"title": TITLE, "verdict": "refuted"}]})));
        let fix = transcript("apply the findings", &[("Edit", json!({"file_path": "src/other.rs"}))], None);
        write_run(&p, "s1", "wf_1", &[("a1", "review", "Review", 10, review()), ("a2", "verify", "Verify", 20, verify), ("a3", "fix", "Fix", 30, fix)]);
        let f = found(&p);
        assert_eq!((f[0].verdict, f[0].fixed), (Verdict::Rejected, Fixed::No));
        assert!(!f[0].strong());
        let _ = std::fs::remove_dir_all(p);
    }

    #[test]
    fn a_review_without_a_later_fix_step_or_a_file_is_counted_apart() {
        let p = temp("nofix");
        write_run(&p, "s1", "wf_1", &[("a1", "review", "Review", 10, review())]);
        let unlocated = transcript("review", &[], Some(json!({"issues": [{"severity": "high", "problem": "the budget is spent on noise"}]})));
        write_run(&p, "s2", "wf_2", &[("b1", "review:queen", "Review", 10, unlocated), ("b2", "fix", "Fix", 20, transcript("fix", &[], None))]);
        let f = found(&p);
        assert_eq!(f.iter().map(|x| x.fixed).collect::<Vec<_>>(), [Fixed::NoFixStep, Fixed::Unlocated]);
        assert_eq!(f[1].verdict, Verdict::Unchecked);
        let _ = std::fs::remove_dir_all(p);
    }

    #[test]
    fn string_findings_and_gate_results_are_not_findings() {
        let out = json!({"findings": ["X1 the voice speaks the commentary 38/38 times"], "results": [{"command": "npm run check", "passed": true}]});
        assert!(findings_of(Some(&out)).is_empty());
        assert!(verdicts_of(Some(&out)).is_empty());
        // A verify output's items carry a verdict, so they are verdicts, not new findings.
        let v = json!({"findings": [{"title": TITLE, "severity": "low", "real": false}]});
        assert!(findings_of(Some(&v)).is_empty());
    }

    #[test]
    fn verdicts_link_by_id_within_the_verifiers_dimension() {
        let p = temp("ids");
        let f =
            |id: &str, file: &str| json!({"id": id, "file": file, "severity": "minor", "claim": format!("{file} drops the queued lessons on a late answer")});
        let rust = transcript("review", &[], Some(json!({"findings": [f("F1", "src/a.rs")]})));
        let docs = transcript("review", &[], Some(json!({"findings": [f("F1", "docs/b.md")]})));
        let verify = transcript("verify", &[], Some(json!({"verdicts": [{"id": "F1", "refuted": true}]})));
        let keep = transcript("verify", &[], Some(json!({"results": [{"id": "F1", "keep": true}]})));
        write_run(
            &p,
            "s1",
            "wf_1",
            &[
                ("a1", "review:rust", "Review", 10, rust),
                ("a2", "review:docs", "Review", 11, docs),
                ("a3", "verify:rust", "Verify", 20, verify),
                ("a4", "verify:docs", "Verify", 21, keep),
            ],
        );
        let got = found(&p);
        assert_eq!(
            got.iter().map(|x| (x.agent.as_str(), x.verdict)).collect::<Vec<_>>(),
            [("review:rust", Verdict::Rejected), ("review:docs", Verdict::Confirmed)]
        );
        assert_eq!(verdict_of(&json!({"verdict": "PLAUSIBLE"})), Some(Verdict::Confirmed));
        assert_eq!(verdict_of(&json!({"verdict": "needs more info"})), None);
        let _ = std::fs::remove_dir_all(p);
    }

    #[test]
    fn a_strong_finding_becomes_a_review_regret_episode() {
        let p = temp("episode");
        let build = transcript("Port the voice lane to gpt-live.", &[], None);
        let fix = transcript("apply", &[("Bash", json!({"command": "sed -i x adapters/omp/borg.ts"}))], None);
        write_run(&p, "s1", "wf_1", &[("a0", "build", "Build", 5, build), ("a1", "review", "Review", 10, review()), ("a2", "fix", "Fix", 20, fix)]);
        let f = found(&p);
        assert_eq!((f[0].id.as_str(), f[0].task.as_str()), ("wf_1#0", "Port the voice lane to gpt-live."));
        let e = f[0].episode("dev@x");
        assert_eq!((e.id.as_str(), e.kind.as_str(), e.session.as_str(), e.engineer.as_str()), ("Vwf_1#0", regret::KIND, "s1", "dev@x"));
        let r = e.regret.unwrap();
        assert!(regret::is_review(&r) && r.signals == ["review", "unchecked"]);
        assert_eq!(r.decision_ms, e.ms, "decided before the finding: asks up to it are blamed");
        assert!(r.diff.starts_with("adapters/omp/borg.ts: ") && r.fix_subject == TITLE, "{r:?}");
        let _ = std::fs::remove_dir_all(p);
    }

    #[test]
    fn paths_come_out_of_location_text() {
        assert_eq!(path_in("src/main/java/Tq.java:72-79 (resets at L74)").as_deref(), Some("src/main/java/Tq.java"));
        assert_eq!(path_in("sliceA-spec.md §G.3/§G.8 (ii)").as_deref(), Some("sliceA-spec.md"));
        assert_eq!(path_in("the whole design"), None);
        assert!(names("cat > crates/borg/src/live.rs <<EOF", "/home/x/crates/borg/src/live.rs") || names("x src/live.rs", "crates/borg/src/live.rs"));
        assert!(!names("edits main.rs", "src/live.rs"));
    }

    #[test]
    fn the_control_pairs_a_finding_with_another_sessions_fix_step() {
        let p = temp("control");
        let fix = |file: &str| transcript("fix", &[("Bash", json!({"command": format!("vim {file}")}))], None);
        write_run(&p, "s1", "wf_1", &[("a1", "review", "Review", 10, review()), ("a2", "fix", "Fix", 20, fix("adapters/omp/borg.ts"))]);
        write_run(&p, "s2", "wf_2", &[("b1", "build", "Build", 30, transcript("build", &[], None)), ("b2", "fix", "Fix", 40, fix("src/other.rs"))]);
        let r = report(&claude_runs(&p, None), None, &p.join("store"));
        assert_eq!((r.control.located, r.control.own_named, r.control.other_named), (1, 1, 0));
        assert_eq!(r.collectives.values().map(|t| (t.findings, t.strong, t.runs)).collect::<Vec<_>>(), [(1, 1, 1)]);
        let _ = std::fs::remove_dir_all(p);
    }

    #[test]
    fn session_end_records_only_that_sessions_strong_findings_once() {
        let p = temp("session");
        let fix = transcript("apply the findings", &[("Bash", json!({"command": "sed -i 's/a/b/' adapters/omp/borg.ts"}))], None);
        for (s, run) in [("s1", "wf_1"), ("s2", "wf_2")] {
            write_run(&p, s, run, &[("a1", "review:concurrency", "Review", 10, review()), ("a3", "fix", "Fix", 30, fix.clone())]);
        }
        let store = p.join("store");
        let session = |s: &str| record(scan(&claude_runs(&p, Some(s))).0.iter(), None, &store);
        let (n, homes) = session("s1");
        assert_eq!((n, homes), (1, vec![store.clone()]));
        let episodes = std::fs::read_to_string(store.join("episodes.jsonl")).unwrap();
        assert!(episodes.contains("Vwf_1#") && !episodes.contains("wf_2"), "{episodes}");
        // Each recorded finding is counted for the page (stats.rs), with its severity; a skipped one is not.
        let evs = crate::stats::events(&store);
        assert_eq!(
            evs.iter().filter(|e| e.kind == crate::stats::REVIEW_FINDING).map(|e| (e.session.as_str(), e.why.as_str())).collect::<Vec<_>>(),
            [("s1", "medium")]
        );
        // Known episodes are skipped; a session id is never a path.
        assert_eq!(session("s1").0, 0);
        assert_eq!(record_session(&p, "../-w-app/s2", None, &store).0, 0);
        assert_eq!(record_session(&p, "", None, &store).0, 0);
        let _ = std::fs::remove_dir_all(p);
    }

    fn fixture(p: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/review_fix").join(p)
    }

    /// (title, severity, fixed, strong) of each finding.
    fn rows(f: &[Finding]) -> Vec<(&str, &str, Fixed, bool)> {
        f.iter().map(|x| (x.title.as_str(), x.severity.as_str(), x.fixed, x.strong())).collect()
    }

    #[test]
    fn a_claude_workflow_run_gives_its_strong_finding() {
        let f = found(&fixture("claude/projects"));
        assert_eq!(rows(&f), [("The budget place is reset by an arc instead of drained", "high", Fixed::Yes, true)]);
        assert_eq!((f[0].session.as_str(), f[0].file.as_deref(), f[0].workflow.as_str()), ("s1", Some("src/net.rs"), "build"));
    }

    #[test]
    fn omp_findings_yielded_one_by_one_and_a_fix_task_give_one_strong_finding() {
        let runs = prepared(harness::omp::session_runs(&fixture("omp/sessions"), Some("01a0a00d-0000-7000-8000-000000000001"), None));
        let f = scan(&runs).0;
        // The array the schema rejected is not counted: only the accepted yields, one finding each.
        assert_eq!(
            rows(&f),
            [
                ("Facet mapping drops the unit of a numeric range", "p1", Fixed::Yes, true),
                ("Empty facet list renders a stray separator", "p3", Fixed::No, false)
            ]
        );
        let x = &f[0];
        assert_eq!(
            (x.agent.as_str(), x.session.as_str(), x.cwd.as_str(), x.workflow.as_str()),
            ("MappingReview", "01a0a00d-0000-7000-8000-000000000001", "/w/app", "Map the facets")
        );
        assert_eq!(x.file.as_deref(), Some("/w/app/src/facets/mapping.ts"));
        assert!(x.detail.starts_with("`mapRange` in src/facets/mapping.ts") && x.task == "Implement the facet mapping in src/facets/mapping.ts.", "{x:?}");
    }

    #[test]
    fn omp_findings_in_one_yielded_object_are_all_read() {
        let runs = prepared(harness::omp::session_runs(&fixture("omp/sessions"), Some("01a0a00d-0000-7000-8000-000000000002"), None));
        let f = scan(&runs).0;
        assert_eq!(
            rows(&f),
            [
                ("Retry loop never backs off", "p0", Fixed::NoFixStep, false),
                ("Timeout is read in seconds and used as milliseconds", "p2", Fixed::NoFixStep, false)
            ]
        );
        // Every session, oldest first.
        let all = harness::omp::session_runs(&fixture("omp/sessions"), None, None);
        assert_eq!(all.iter().map(|r| r.agents.len()).collect::<Vec<_>>(), [3, 2]);
    }

    /// opencode exports by id from the fixture's v1 and v2 directories; the query lists both roots.
    fn opencode_runs(session: Option<&str>) -> Vec<Run> {
        let export = |id: &str, _: Option<&Path>| {
            ["v1", "v2"]
                .iter()
                .find_map(|v| std::fs::read(fixture("opencode").join(v).join(format!("{id}.json"))).ok())
                .and_then(|b| serde_json::from_slice(&b).ok())
        };
        let query = |_: &str| Some(vec![json!({"id": "ses_1root0000000000000000000"}), json!({"id": "ses_2root0000000000000000000"})]);
        prepared(harness::opencode::session_runs(session, None, &export, &query))
    }

    #[test]
    fn opencode_findings_come_from_a_reviewers_json_text_and_a_fix_child_fixes_them() {
        let f = scan(&opencode_runs(Some("ses_1root0000000000000000000"))).0;
        assert_eq!(rows(&f), [("Cache key ignores the tenant id", "high", Fixed::Yes, true)]);
        assert_eq!((f[0].agent.as_str(), f[0].session.as_str(), f[0].cwd.as_str()), ("review cache", "ses_1root0000000000000000000", "/w/app"));
        // 2.x: a bare object with `issues`; the repair child's export is missing, so its final text
        // as the parent saw it names the file.
        let all = scan(&opencode_runs(None)).0;
        assert_eq!(rows(&all)[1], ("Queue bound is checked after the push", "medium", Fixed::Yes, true));
        // A session id that is no opencode one asks nothing.
        assert!(opencode_runs(Some("01a0a00d-0000-7000-8000-000000000001")).is_empty());
    }

    #[test]
    fn harnesses_without_sub_agent_transcripts_give_no_runs() {
        let home = fixture("omp");
        let none: Vec<&str> = harness::ALL.iter().filter(|h| h.sub_runs(&home, None, None).is_empty()).map(|h| h.name()).collect();
        assert!(none.contains(&"codex") && none.contains(&"sim"), "{none:?}");
    }

    #[test]
    fn severity_comes_from_a_priority_and_text_from_a_body() {
        let f = |v: Value| findings_of(Some(&json!({"findings": [v]}))).into_iter().map(|r| (r.title, r.severity, r.detail, r.file)).next();
        assert_eq!(f(json!({"body": "src/a.rs drops it", "priority": "P2"})), Some(("src/a.rs drops it".into(), "p2".into(), String::new(), None)));
        assert_eq!(
            f(json!({"title": "t", "body": "b", "priority": 0, "file_path": "src/a.rs"})).map(|x| (x.1, x.2, x.3)),
            Some(("p0".into(), "b".into(), Some("src/a.rs".into())))
        );
        assert_eq!(f(json!({"title": "no severity"})), None);
        let text = |w: &str| output_of(&SubAgent { final_text: w.into(), ..Default::default() });
        assert_eq!(text("{\"command\": \"x\"}\nDone: {\"findings\": []} inline"), None, "only an object that starts a line");
        assert!(text("Report\n```json\n{\"issues\": [{\"problem\": \"p\"}]}\n```").is_some());
    }

    #[test]
    fn only_a_fix_step_is_a_fix_step_whatever_its_label_mentions() {
        let a = |label: &str, phase: &str, kind: &str, output: Option<Value>| SubAgent {
            label: label.into(),
            phase: phase.into(),
            kind: kind.into(),
            output,
            ..Default::default()
        };
        let fix = |label: &str, phase: &str| is_fix(&a(label, phase, "", None));
        // Labels of real workflow runs: fix in a later segment is what is checked, built or reviewed.
        assert!(!fix("verify:fix-correctness", "Verify"));
        assert!(!fix("review:harness-fix#0", "Review"));
        assert!(!fix("impl:harness-fix", "Implement"));
        assert!(!fix("verify:fix-correctness", ""), "the label's first segment decides");
        assert!(fix("fix", "Fix") && fix("fix:rust", "") && fix("fix#1", "") && fix("anything", "Repair"));
        assert!(fix("MappingValidityFix", "") && fix("DeriveFacetsRepairs", "") && !fix("Prefixed", "") && !fix("MappingReviewFix", ""));
        assert!(fix("fix", "") && !fix("fix", "Review"), "a review phase is never a fix step");
        assert!(is_fix(&a("ses", "", "fixer", None)), "an omp kind");
        // An output with a findings list, even an empty one, or a verdict is a reviewer's or a verifier's.
        assert!(!is_fix(&a("fix", "Fix", "", Some(json!({"findings": []})))));
        assert!(!is_fix(&a("fix", "Fix", "", Some(json!({"real": true})))));
        // The implementer is the builder, not a fix step.
        let p = temp("labels");
        let build = transcript("Build the harness fix.", &[], None);
        let fix_it = transcript("apply", &[("Bash", json!({"command": "sed -i x adapters/omp/borg.ts"}))], None);
        let verify = transcript("verify", &[("Bash", json!({"command": "cat adapters/omp/borg.ts"}))], Some(json!({"real": true})));
        write_run(
            &p,
            "s1",
            "wf_1",
            &[
                ("a0", "impl:harness-fix", "Implement", 5, build),
                ("a1", "review:harness-fix#0", "Review", 10, review()),
                ("a2", "verify:fix-correctness", "Verify", 20, verify),
            ],
        );
        let f = found(&p);
        assert_eq!((f[0].fixed, f[0].task.as_str()), (Fixed::NoFixStep, "Build the harness fix."));
        write_run(&p, "s2", "wf_2", &[("b1", "review:harness-fix#0", "Review", 10, review()), ("b2", "fix:rust", "", 30, fix_it)]);
        assert_eq!(found(&p)[1].fixed, Fixed::Yes);
        let _ = std::fs::remove_dir_all(p);
    }

    #[test]
    fn findings_come_only_from_the_final_text_never_from_a_tool_input() {
        let echo = json!({"findings": [{"file": "src/a.rs", "severity": "high", "title": "the findings this fix step works through"}]});
        // A fix step that passes the findings to a tool stays a fix step, whatever its text says after.
        for last in ["", "Fixed src/a.rs."] {
            let a = SubAgent { label: "fix".into(), work: format!("{{\"command\":\"cargo test\"}}\n{echo}\n{last}\n"), final_text: last.into(), ..Default::default() };
            let a = SubAgent { output: output_of(&a), ..a };
            assert!(a.output.is_none() && is_fix(&a), "{last:?}");
        }
        // A reviewer's answer is read, fenced or compact (keys in any order).
        let fenced = format!("Done.\n```json\n{}\n```\n", serde_json::to_string_pretty(&echo).unwrap());
        assert_eq!(output_of(&SubAgent { final_text: fenced, ..Default::default() }), Some(echo.clone()));
        assert_eq!(output_of(&SubAgent { final_text: echo.to_string(), ..Default::default() }), Some(echo));
    }

    #[test]
    fn a_builder_in_another_repo_gives_no_task_text() {
        let p = temp("builder-repo");
        let fix = transcript("apply the findings", &[("Bash", json!({"command": "sed -i 's/a/b/' adapters/omp/borg.ts"}))], None);
        let build = transcript("build the secret feature", &[], None).replace("/w/app", "/w/private");
        write_run(&p, "s1", "wf_1", &[("a0", "build", "Build", 5, build), ("a1", "review:concurrency", "Review", 10, review()), ("a3", "fix", "Fix", 30, fix)]);
        let f = found(&p);
        assert_eq!(f.len(), 1);
        assert!(f[0].task.is_empty() && f[0].built.is_empty(), "{:?}", f[0].task);
        let _ = std::fs::remove_dir_all(p);
    }
}
