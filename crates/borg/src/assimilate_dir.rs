//! `borg assimilate --from-dir <dir>`: a team's written knowledge becomes lessons of its collective.
//!
//! A team that already keeps hand-curated knowledge (a knowledge skill and its references,
//! retrospectives with "Prevention for future" rows, an audit of recurring failure families, documents
//! that leave decisions open) should not wait for live corrections to teach the same facts again.
//! This reads that knowledge from a git checkout and turns it into lessons in the standard schema
//! (question + situations -> consequence, area, scope, overrides, origin `path:line`, engineers from
//! `git log` of the file), so drones get it the way they get learned lessons: recalled on touch at the
//! checkpoint, scoped, never broadcast. Open decisions become queen queue entries (queen.rs
//! `Kind::OpenDecision`), never lessons: a question nobody answered is not a rule.
//!
//! It is part of `borg` rather than the eval harness because it is how a team joins: it writes the
//! routed collective's store exactly as `borg learn` does (the E3 eval only points `BORG_HOME` at a
//! scratch store).
//!
//! **Sources** (tracked files only; globs relative to the directory; defaults fit a handbook layout):
//! - knowledge `skills/*-knowledge/SKILL.md`, `skills/*-knowledge/references/*.md`: one chunk per file
//!   (split at `## ` headings past `CHUNK_LINES`);
//! - retros `docs/*retrospectives/*.md`: the rows of every table whose header mentions prevention;
//! - families `docs/**/*coverage-audit*.md`: the rows of every table headed "Failure family", minus
//!   columns naming eval cases (the importer must not carry an eval's own case list into lessons);
//! - decisions `docs/**/*.md`: the blocks around "Your decision", "Open conflict" and similar markers.
//!
//! **Generalisation** uses the same model call as learn.rs (`learn::ask_model`, `BORG_LEARN_MODEL`,
//! default sonnet): one call per chunk, `BORG_ASSIMILATE_JOBS` (default 4) at a time; every call's cost
//! is printed and, with `--apply`, recorded in `<store>/assimilated.jsonl`. Scope guards are part of
//! the contract: a source that limits a rule ("Orion nets only") must give a scoped lesson, and a rule
//! that replaces outside advice names it in `overrides` (in an earlier eval the drain rule only won once it
//! named the upstream reset idiom it replaces).
//!
//! **Dry run by default**: prints the lessons and decisions, writes nothing (`--plan FILE` saves them
//! for review; `--apply --plan FILE` then applies exactly those and never asks the model: a plan that
//! cannot be read stops the run, and a chunk the plan does not answer, or whose source changed since,
//! is listed and skipped with a non-zero exit).
//!
//! **Target**: lessons go to the collective the source directory routes to, unless `--collective NAME`
//! or `--for PATH` (routed like a drone working there) says otherwise: a team's knowledge repo is
//! rarely where its drones work. The output names the receiving collective and the repos routed to it.
//!
//! **Scope guards** (deterministic, after the model): the repo the knowledge is kept in and ticket
//! prefixes are never scopes; a one-word common English word is never a scope; a scope name must occur
//! in an identifier or path of the source (`guard`). A lesson left without a scope name is dropped,
//! never widened to unscoped. The conscience matches a scope name as a phrase (conscience.rs `in_scope`). Re-running `--apply` costs nothing: a chunk already imported with the same content hash is
//! skipped. A chunk whose source changed since its import is reported, not re-imported (retiring the
//! lessons of the old wording is not built). Anything mentioning a word of `BORG_ASSIMILATE_SKIP` is dropped
//! (assimilate.rs `skip_words`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::collective::{self, Config, Route};
use crate::learn::{self, Lesson};
use crate::queen::{self, Entry, Kind};
use crate::trust;

/// A knowledge file longer than this is split at `## ` headings.
const CHUNK_LINES: usize = 150;

/// The model call: (prompt, JSON schema) -> (answer, cost in USD). `learn::ask_model` in production.
pub type Ask<'a> = &'a (dyn Fn(&str, &Value) -> Option<(Value, Option<f64>)> + Sync);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Part {
    Knowledge,
    Retro,
    Family,
    Decision,
}

impl Part {
    fn guidance(self) -> &'static str {
        match self {
            Part::Knowledge => "The chunk is a team-knowledge reference: facts and decided house rules the code does not show.",
            Part::Retro => {
                "The chunk lists problems from a plan or refinement retrospective, with whether each was preventable and \
how to prevent it in future. Turn each preventable item into at most one lesson that generalises its prevention: instructions \
generalise, incidents do not (no ticket ids, dates, product ids or one-off names in the question). Merge items that express \
the same prevention. Skip items that were not preventable or are purely one-off."
            }
            Part::Family => {
                "The chunk is a table of failure families that recur across the team's retrospectives and sessions. Write \
one lesson per family: the question that would have prevented it."
            }
            Part::Decision => {
                "The chunk shows places where the team's documents leave a decision open. Return each genuinely open \
decision about the team's product, code or process rules as an open decision. Do not return questions about whether to keep, \
drop or change an evaluation case or a test fixture, and nothing that the chunk shows as already decided. Return no lessons."
            }
        }
    }
}

/// Source globs per part, relative to the directory.
#[derive(Debug, Clone)]
pub struct Spec {
    pub knowledge: Vec<String>,
    pub retros: Vec<String>,
    pub families: Vec<String>,
    pub decisions: Vec<String>,
}

impl Default for Spec {
    fn default() -> Self {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect();
        Spec {
            knowledge: v(&["skills/*-knowledge/SKILL.md", "skills/*-knowledge/references/*.md"]),
            retros: v(&["docs/*retrospectives/*.md"]),
            families: v(&["docs/**/*coverage-audit*.md"]),
            decisions: v(&["docs/**/*.md"]),
        }
    }
}

/// One unit sent to the model: a part of one file, its lines numbered.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub part: Part,
    /// `path` for a whole file, `path#n` for its n-th piece, `path#families` / `path#decisions`.
    pub key: String,
    pub path: String,
    /// The source lines it holds (1-based), in order.
    pub lines: Vec<usize>,
    /// The text as the model sees it: `NNN| line`.
    pub text: String,
    /// For a knowledge file: the scope its knowledge base states for itself (the skill's description).
    pub about: String,
    /// Hash of `text` and `about`: an import is redone only when one of them changed.
    pub sha: String,
}

/// FNV-1a 64 as hex: stable ids and content hashes without a dependency.
fn fnv(s: &str) -> String {
    format!("{:016x}", s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)))
}

/// A glob (`*`, `?`, `**`) as an anchored regex over `/`-separated relative paths.
pub fn glob(pattern: &str) -> Regex {
    let mut re = String::from("^");
    let mut it = pattern.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '*' if it.peek() == Some(&'*') => {
                it.next();
                if it.peek() == Some(&'/') {
                    it.next();
                    re.push_str("(?:.*/)?");
                } else {
                    re.push_str(".*");
                }
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    Regex::new(&re).unwrap_or_else(|_| Regex::new("^$").unwrap())
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The tracked files of `dir` (untracked notes are someone's private scratch, not team knowledge).
fn tracked(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = git(dir, &["ls-files"]).unwrap_or_default().lines().map(str::to_string).collect();
    v.sort();
    v
}

/// A source line: its 1-based number and its text as the model sees it.
type Line = (usize, String);

fn chunk(part: Part, key: String, path: &str, lines: &[Line]) -> Chunk {
    let text: String = lines.iter().map(|(n, l)| format!("{n:>4}| {l}\n")).collect();
    Chunk { part, key, path: path.into(), lines: lines.iter().map(|(n, _)| *n).collect(), sha: fnv(&text), text, about: String::new() }
}

/// A markdown table: its header line, header cells and body rows (separator dropped).
struct Table<'a> {
    head: (usize, &'a str),
    cells: Vec<String>,
    rows: Vec<(usize, &'a str)>,
}

/// Every run of consecutive `|` lines with a header and at least one more line.
fn tables<'a>(all: &[(usize, &'a str)]) -> Vec<Table<'a>> {
    let cells = |l: &str| l.trim().trim_matches('|').split('|').map(|c| c.trim().to_string()).collect::<Vec<_>>();
    let mut out = Vec::new();
    let mut i = 0;
    while i < all.len() {
        if !all[i].1.trim_start().starts_with('|') {
            i += 1;
            continue;
        }
        let start = i;
        while i < all.len() && all[i].1.trim_start().starts_with('|') {
            i += 1;
        }
        let block = &all[start..i];
        if block.len() >= 2 {
            let rows = block[1..].iter().filter(|(_, l)| !l.chars().all(|c| "|-: ".contains(c))).copied().collect();
            out.push(Table { head: block[0], cells: cells(block[0].1), rows });
        }
    }
    out
}

/// Keep only the columns of a table row whose header passes `keep` (the header itself included).
fn columns(line: &str, header: &[String], keep: &dyn Fn(&str) -> bool) -> String {
    let cells: Vec<&str> = line.trim().trim_matches('|').split('|').map(str::trim).collect();
    let kept: Vec<&str> = cells.iter().enumerate().filter(|(i, _)| header.get(*i).is_none_or(|h| keep(h))).map(|(_, c)| *c).collect();
    format!("| {} |", kept.join(" | "))
}

static MARKER: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"(?i)your decision|open conflict|open decision|skill conflicts?\b|no rule covers|until this is decided|\bundecided\b").unwrap()
});

/// The chunks of one file for one part.
pub fn chunks_of(part: Part, path: &str, text: &str) -> Vec<Chunk> {
    let all: Vec<(usize, &str)> = text.lines().enumerate().map(|(i, l)| (i + 1, l)).collect();
    let owned = |ls: &[(usize, &str)]| ls.iter().map(|(n, l)| (*n, l.to_string())).collect::<Vec<Line>>();
    let title: Vec<Line> = owned(&all.iter().find(|(_, l)| l.starts_with("# ")).copied().into_iter().collect::<Vec<_>>());
    match part {
        Part::Knowledge => {
            if all.len() <= CHUNK_LINES {
                return vec![chunk(part, path.into(), path, &owned(&all))];
            }
            // Split at `## ` headings, packing sections up to CHUNK_LINES (a longer section stays whole).
            let mut pieces: Vec<Vec<Line>> = vec![Vec::new()];
            for &(n, l) in &all {
                if l.starts_with("## ") && pieces.last().is_some_and(|p| p.len() >= CHUNK_LINES / 2) {
                    pieces.push(Vec::new());
                }
                pieces.last_mut().unwrap().push((n, l.to_string()));
            }
            pieces.iter().enumerate().map(|(i, p)| chunk(part, format!("{path}#{i}"), path, p)).collect()
        }
        Part::Retro => {
            let mut rows: Vec<Line> = Vec::new();
            for t in tables(&all).into_iter().filter(|t| t.cells.iter().any(|c| c.to_lowercase().contains("prevent"))) {
                rows.push((t.head.0, t.head.1.to_string()));
                rows.extend(owned(&t.rows));
            }
            if rows.is_empty() {
                return Vec::new();
            }
            vec![chunk(part, format!("{path}#prevention"), path, &[title, rows].concat())]
        }
        Part::Family => {
            let not_cases = |h: &str| !h.to_lowercase().contains("case");
            let mut rows: Vec<Line> = Vec::new();
            for t in tables(&all).into_iter().filter(|t| t.cells.first().is_some_and(|c| c.to_lowercase().contains("failure family"))) {
                rows.push((t.head.0, columns(t.head.1, &t.cells, &not_cases)));
                rows.extend(t.rows.iter().map(|(n, l)| (*n, columns(l, &t.cells, &not_cases))));
            }
            if rows.is_empty() {
                return Vec::new();
            }
            vec![chunk(part, format!("{path}#families"), path, &[title, rows].concat())]
        }
        Part::Decision => {
            // Each marker with its context: back to the nearest heading (at most 25 lines), forward to
            // the next heading or blank line (at most 8 lines).
            let mut keep: Vec<bool> = vec![false; all.len()];
            for (i, (_, l)) in all.iter().enumerate() {
                if !MARKER.is_match(l) {
                    continue;
                }
                let from = (i.saturating_sub(25)..=i).rev().find(|&j| all[j].1.starts_with('#')).unwrap_or(i.saturating_sub(25));
                let mut to = i;
                while to + 1 < all.len() && to < i + 8 && !all[to + 1].1.starts_with('#') && !all[to + 1].1.trim().is_empty() {
                    to += 1;
                }
                keep[from..=to].iter_mut().for_each(|k| *k = true);
            }
            let lines: Vec<Line> = owned(&all.iter().zip(&keep).filter(|(_, k)| **k).map(|(l, _)| *l).collect::<Vec<_>>());
            if lines.is_empty() {
                return Vec::new();
            }
            vec![chunk(part, format!("{path}#decisions"), path, &[title.into_iter().filter(|t| !lines.contains(t)).collect(), lines].concat())]
        }
    }
}

/// Every chunk of `dir` under `spec`, in a stable order (a file matched by two parts gives both).
pub fn chunks(dir: &Path, spec: &Spec) -> Vec<Chunk> {
    let files = tracked(dir);
    let mut out = Vec::new();
    let skip = crate::assimilate::skip_words();
    // What the team's knowledge base says it covers frames every other source too: a retro or a failure
    // family learned on that system's work belongs to it (its own wording often sounds general).
    let knowledge: Vec<Regex> = spec.knowledge.iter().map(|g| glob(g)).collect();
    let mut team: Vec<String> = Vec::new();
    for f in files.iter().filter(|f| knowledge.iter().any(|r| r.is_match(f))) {
        let d = skill_description(dir, f);
        if !d.is_empty() && !team.contains(&d) {
            team.push(d);
        }
    }
    let team = team.join(" ");
    for (part, globs) in [(Part::Knowledge, &spec.knowledge), (Part::Retro, &spec.retros), (Part::Family, &spec.families), (Part::Decision, &spec.decisions)] {
        let res: Vec<Regex> = globs.iter().map(|g| glob(g)).collect();
        for f in files.iter().filter(|f| f.ends_with(".md") && res.iter().any(|r| r.is_match(f))) {
            let Ok(text) = std::fs::read_to_string(dir.join(f)) else { continue };
            if crate::assimilate::mentions(&text, &skip) {
                continue;
            }
            let about = match part {
                Part::Knowledge => skill_description(dir, f),
                _ => team.clone(),
            };
            out.extend(chunks_of(part, f, &text).into_iter().map(|mut c| {
                if !about.is_empty() {
                    c.sha = fnv(&format!("{}\n{about}", c.text));
                    c.about = about.clone();
                }
                c
            }));
        }
    }
    out
}

const PROMPT: &str = "You turn a software team's written knowledge into lessons for the team's coding agents. An agent \
is asked a lesson's question at a checkpoint (before it finishes a plan or a change) only when its work matches; it sees \
only the question, plus the outside rule the lesson replaces if any. So:
- question: ONE question the agent should ask itself. For a team fact the code does not show, the question must carry the \
fact itself (e.g. \"Do the replayed services all run on their live hosts, since develop lacks the products?\"), so an agent \
that reads only the question can apply it. Keep the names an agent working there would see (systems, services, classes, \
places, commands); drop ticket ids, dates and incident details;
- situations: 1-3 lines, each `<action> on/in <kind of state or component> [under <condition>]`;
- consequence: one line, what goes wrong or what the team requires instead;
- area: the one kind of concern: concurrency, spec-conformance, reuse-existing, architecture-boundary, verification, \
minimality, protocol-correctness, data-source, security, dependency-version, scope, or other;
- scope: when the source limits the rule to a system, list every name by which an agent would recognise that it works \
inside that system: the system itself and the repos, modules and services the source ties to it, as they appear in paths, \
code or prompts (e.g. a rule for Orion's search experiments: [\"Orion\", \"harbor_orion\", \"Falcon\", \"Beacon\", \
\"Heron\", \"Pyxis\"]; a rule for \"Orion's text and voice nets only\": [\"Orion\", \"harbor_orion\"]). An agent \
is asked the lesson only where one of these names occurs, so never list a generic word (search, net, test, plan, \
experiment), never a library, framework or tool the system merely uses (flowlib, Gradle, Prometheus: other systems use \
them too), never the team or company name, never a ticket key or ticket prefix, and never the repository the knowledge \
is written in. A rule learned on one system's work (its retrospectives, its incidents) is scoped to that system even when \
its wording sounds general, unless the source says it holds everywhere. When a file's title or scope line ties all of it to a system (\"Experiments on Orion search\"), every \
lesson from that file gets the same full list of that system's names, not only the names its own line mentions. Never \
widen a scope the source states: a rule for one system must not carry the names of systems it does not apply to. Empty \
when the rule holds for any of the team's work; a prevention learned on one ticket that holds for any work is unscoped;
- overrides: when the source says this rule replaces, changes or takes precedence over an upstream, vendored or general \
rule, idiom or skill advice, name that rule, where it comes from and why it is replaced, in one sentence; otherwise empty;
- line: the number of the source line the lesson rests on.
Write only lessons that would change what an agent does. Skip navigation, tables of contents, plain definitions and \
anything already obvious to a capable engineer.
If the chunk states a decision the team has left open (undecided, a conflict waiting for a ruling), add it to \
open_decisions: question, the options, the interim default the documents prescribe (or \"none stated\"), a short evidence \
quote, and its line. A decided rule is a lesson, not an open decision.";

fn schema() -> Value {
    json!({"type": "object", "additionalProperties": false, "required": ["lessons", "open_decisions"], "properties": {
        "lessons": {"type": "array", "items": {"type": "object", "additionalProperties": false,
            "required": ["question", "situations", "consequence", "area", "scope", "overrides", "line"], "properties": {
            "question": {"type": "string"}, "situations": {"type": "array", "items": {"type": "string"}},
            "consequence": {"type": "string"}, "area": {"type": "string", "enum": trust::AREAS},
            "scope": {"type": "array", "items": {"type": "string"}}, "overrides": {"type": "string"},
            "line": {"type": "integer"}}}},
        "open_decisions": {"type": "array", "items": {"type": "object", "additionalProperties": false,
            "required": ["question", "options", "default", "evidence", "line"], "properties": {
            "question": {"type": "string"}, "options": {"type": "array", "items": {"type": "string"}},
            "default": {"type": "string"}, "evidence": {"type": "string"}, "line": {"type": "integer"}}}}}})
}

pub fn prompt(repo: &str, c: &Chunk) -> String {
    let about = match c.about.is_empty() {
        true => String::new(),
        false => format!("\n\nThe team's knowledge base states its own scope as: {}", c.about),
    };
    format!("{PROMPT}\n\n{}{about}\n\n## Source: {repo}/{} (line numbers before `|`)\n{}", c.part.guidance(), c.path, c.text)
}

/// The `description:` of the skill a knowledge file belongs to (`skills/<name>/SKILL.md` frontmatter).
fn skill_description(dir: &Path, path: &str) -> String {
    let mut parts = path.split('/');
    let (Some(root), Some(name)) = (parts.next(), parts.next()) else { return String::new() };
    let text = std::fs::read_to_string(dir.join(root).join(name).join("SKILL.md")).unwrap_or_default();
    let mut lines = text.lines();
    if lines.next() != Some("---") {
        return String::new();
    }
    lines.take_while(|l| *l != "---").find_map(|l| l.strip_prefix("description:")).map(|d| d.trim().to_string()).unwrap_or_default()
}

/// Provenance of one source file: its engineers (git log author emails, first contributor first) and
/// the time of its last commit (epoch ms; 0 if unknown).
#[derive(Debug, Clone, Default)]
pub struct Provenance {
    pub engineers: Vec<String>,
    pub ms: i64,
}

fn provenance(dir: &Path, path: &str) -> Provenance {
    let log = git(dir, &["log", "--format=%ae %ct", "--", path]).unwrap_or_default();
    let mut engineers: Vec<String> = Vec::new();
    let mut ms = 0;
    for (i, l) in log.lines().enumerate() {
        let (who, t) = l.rsplit_once(' ').unwrap_or((l, "0"));
        if i == 0 {
            ms = t.parse::<i64>().unwrap_or(0) * 1000;
        }
        engineers.push(who.to_string());
    }
    engineers.reverse();
    let mut seen = HashSet::new();
    engineers.retain(|e| !e.is_empty() && seen.insert(e.clone()));
    Provenance { engineers, ms }
}

/// What one chunk became.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Done {
    pub key: String,
    pub sha: String,
    pub lessons: Vec<Lesson>,
    pub decisions: Vec<Entry>,
    pub cost_usd: Option<f64>,
    /// Questions of lessons dropped because none of their scope names survived `guard`.
    #[serde(default)]
    pub dropped: Vec<String>,
    /// Scope names `guard` rejected.
    #[serde(default)]
    pub rejected: Vec<String>,
}

/// The common English words (data/common-words.txt): a one-word scope name among them is rejected.
fn common_word(w: &str) -> bool {
    static WORDS: std::sync::OnceLock<HashSet<&'static str>> = std::sync::OnceLock::new();
    WORDS.get_or_init(|| include_str!("../data/common-words.txt").lines().filter(|l| !l.starts_with('#')).collect()).contains(w)
}

/// The identifiers and paths a text shows: code spans, and words shaped like code (`_`, `/`, `.` or
/// `-` inside, letters with digits, or a camelCase hump), never plain prose words.
fn identifiers(text: &str) -> Vec<&str> {
    static SPAN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static WORD: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let span = SPAN.get_or_init(|| Regex::new(r"`([^`\n]+)`").unwrap());
    let word = WORD.get_or_init(|| Regex::new(r"[A-Za-z0-9_./-]+").unwrap());
    let mut out: Vec<&str> = span.captures_iter(text).filter_map(|c| c.get(1)).map(|m| m.as_str()).collect();
    for w in word.find_iter(text).map(|m| m.as_str().trim_matches(['.', '-', '/'])) {
        let cs: Vec<char> = w.chars().collect();
        let hump = cs.windows(2).any(|p| p[0].is_lowercase() && p[1].is_uppercase());
        let mixed = cs.iter().any(char::is_ascii_digit) && cs.iter().any(|c| c.is_alphabetic());
        if hump || mixed || w.contains(['_', '/', '.', '-']) {
            out.push(w);
        }
    }
    out
}

/// Keep only scope names a drone's context cannot hit by accident. A name is rejected when:
/// - it is one common word the source also uses as a word: written in lowercase in its prose
///   ("guidance", "conversation"; a host `heron.live…` is not prose), or in capitals (`SCRIPTED`, an enum constant). A common word the
///   team only ever writes as a name ("Beacon", its search step) stays a name;
/// - no identifier or path of the source shows it (a prose-only name: "Answer Guard", a dependency
///   named in passing).
///
/// `known` holds the repo's tracked paths and the identifiers of the chunk and of the knowledge base's
/// scope statement; `prose` is the chunk's text and that statement. A lesson whose every name is
/// rejected is dropped: dropping a scope must never widen a team rule to unscoped.
fn guard(d: &mut Done, known: &crate::conscience::Names, prose: &str) {
    let used_as_word = |n: &str| {
        let lower = n.to_lowercase();
        n.chars().all(|c| !c.is_lowercase())
            || Regex::new(&format!(r#"(?m)(?:^|[\s(\["'*]){}(?:[\s,;:!?)\]"'*]|\.(?:\s|$)|$)"#, regex::escape(&lower))).is_ok_and(|r| r.is_match(prose))
    };
    let mut kept = Vec::new();
    for mut l in std::mem::take(&mut d.lessons) {
        if l.scope.is_empty() {
            kept.push(l);
            continue;
        }
        let (ok, bad): (Vec<String>, Vec<String>) = l.scope.drain(..).partition(|n| {
            let word = match crate::conscience::segments(n).as_slice() {
                [] => return false,
                [one] => common_word(one) && used_as_word(n),
                _ => false,
            };
            !word && known.contains(n)
        });
        for b in bad {
            if !d.rejected.contains(&b) {
                d.rejected.push(b);
            }
        }
        if ok.is_empty() {
            d.dropped.push(l.question);
        } else {
            l.scope = ok;
            kept.push(l);
        }
    }
    d.lessons = kept;
}

/// The names `guard` checks scopes against: tracked paths, and the identifiers of the chunk and its frame.
fn known_names(paths: &[String], c: &Chunk) -> crate::conscience::Names {
    crate::conscience::Names::of_all(paths.iter().map(String::as_str).chain(identifiers(&c.text)).chain(identifiers(&c.about)))
}

/// Whether `term`, spelled as given, occurs in `text` only as a ticket key prefix (`LUMEN-213`).
fn ticket_prefix(term: &str, text: &str) -> bool {
    let t = regex::escape(term);
    let as_key = Regex::new(&format!(r"\b{t}-\d")).is_ok_and(|r| r.is_match(text));
    let alone = Regex::new(&format!(r"\b{t}\b(?:[^-]|-\D|$)")).is_ok_and(|r| r.is_match(text));
    as_key && !alone
}

/// Turn the model's answer for `c` into lessons and queue entries. `None` for a malformed answer.
pub fn build(repo: &str, cwd: &str, c: &Chunk, v: &Value, prov: &Provenance, now: i64) -> Option<(Vec<Lesson>, Vec<Entry>)> {
    let line = |x: &Value| x["line"].as_u64().map(|n| n as usize).filter(|n| c.lines.contains(n)).unwrap_or(c.lines.first().copied().unwrap_or(1));
    let s = |x: &Value, k: &str| x[k].as_str().unwrap_or_default().trim().to_string();
    let strs = |x: &Value, k: &str| {
        x[k].as_array().into_iter().flatten().filter_map(|y| y.as_str()).map(|y| y.trim().to_string()).filter(|y| !y.is_empty()).collect::<Vec<_>>()
    };
    let mut lessons = Vec::new();
    // A decision chunk yields no lessons even if the model wrote some.
    for x in v["lessons"].as_array()?.iter().filter(|_| c.part != Part::Decision) {
        let question = s(x, "question");
        if question.is_empty() {
            continue;
        }
        let origin = format!("{}:{}", c.path, line(x));
        lessons.push(Lesson {
            id: format!("I{}", &fnv(&format!("{repo}\n{origin}\n{question}"))[..12]),
            origin_ms: if prov.ms > 0 { prov.ms } else { now },
            repo: repo.into(),
            question,
            situations: strs(x, "situations"),
            consequence: s(x, "consequence"),
            support: 1,
            episodes: Vec::new(),
            history: Vec::new(),
            version: 1,
            engineers: prov.engineers.clone(),
            joined: Default::default(),
            area: trust::area(&s(x, "area")).into(),
            supersedes: Vec::new(),
            source: "import".into(),
            reach: String::new(),
            // The repository the knowledge is written in is where it was kept, not where it applies; a
            // ticket prefix (the source says `LUMEN-213`) names a tracker project, not a system.
            scope: strs(x, "scope").into_iter().filter(|t| !t.eq_ignore_ascii_case(repo) && !ticket_prefix(t, &c.text)).collect(),
            overrides: s(x, "overrides"),
            origin,
            facets: Vec::new(),
        });
    }
    let mut decisions = Vec::new();
    for x in v["open_decisions"].as_array()? {
        let question = s(x, "question");
        if question.is_empty() {
            continue;
        }
        let key = queen::open_decision_key(repo, &question);
        let default = Some(s(x, "default")).filter(|d| !d.is_empty()).unwrap_or_else(|| "none stated".into());
        decisions.push(Entry {
            id: format!("q{}", &key[..8]),
            decision_key: key,
            kind: Kind::OpenDecision,
            question,
            options: strs(x, "options"),
            default,
            evidence: vec![format!("{}:{}: {}", c.path, line(x), s(x, "evidence"))],
            session: "import".into(),
            cwd: cwd.into(),
            repo: repo.into(),
            engineer: prov.engineers.last().cloned().unwrap_or_default(),
            ms: now,
            answer: None,
            answered_by: None,
            answered_ms: None,
            disagreeing: Vec::new(),
            dismissed: None,
            dismissed_ms: None,
        });
    }
    let skip = crate::assimilate::skip_words();
    let obsolete = |t: &str| crate::assimilate::mentions(t, &skip);
    lessons.retain(|l| ![&l.question, &l.consequence, &l.overrides].iter().any(|t| obsolete(t)) && !l.situations.iter().any(|t| obsolete(t)));
    decisions.retain(|d| !obsolete(&d.question) && !d.evidence.iter().any(|t| obsolete(t)));
    Some((lessons, decisions))
}

/// One line of `<store>/assimilated.jsonl`: a chunk imported, what it gave and what it cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Imported {
    pub source: String,
    pub key: String,
    pub sha: String,
    pub lessons: Vec<String>,
    pub decisions: Vec<String>,
    pub cost_usd: Option<f64>,
    pub ms: i64,
}

pub struct Options {
    pub dir: PathBuf,
    pub spec: Spec,
    pub apply: bool,
    pub plan: Option<PathBuf>,
    pub jobs: usize,
    pub target: Target,
}

/// Which collective receives the lessons. A team's knowledge repo is rarely where its drones work, so
/// routing by the source directory is only the default.
#[derive(Debug, Clone, Default)]
pub enum Target {
    /// The collective the source directory routes to.
    #[default]
    Source,
    /// The collective a drone working in this path routes to (`--for PATH`).
    For(PathBuf),
    /// A collective by name (`--collective NAME`).
    Named(String),
}

#[derive(Debug, Default)]
pub struct Report {
    /// None: the directory routes off and nothing was read.
    pub collective: Option<(String, PathBuf)>,
    pub chunks: usize,
    /// Imported earlier with the same content: skipped, no call.
    pub known: usize,
    /// Imported earlier, source changed since: reported, not re-imported.
    pub changed: Vec<String>,
    pub calls: usize,
    /// Chunks the model gave no usable answer for (retried by the next run).
    pub failed: Vec<String>,
    pub cost_usd: f64,
    pub done: Vec<Done>,
    pub new_lessons: usize,
    pub new_decisions: usize,
    /// The queue entries this run added (one desktop notification for all of them, notify.rs).
    pub queued: Vec<Entry>,
    /// Which repos route to the receiving collective, in words.
    pub routed: String,
    /// `--apply --plan`: chunks the plan does not answer (absent, or their source changed since the dry
    /// run). Skipped, never sent to the model.
    pub unplanned: Vec<String>,
    /// Why nothing was done (an unreadable plan, a target that routes off or does not exist).
    pub error: Option<String>,
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Vec<T> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn append_jsonl<T: Serialize>(path: &Path, rows: &[T]) -> std::io::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    for r in rows {
        writeln!(f, "{}", serde_json::to_string(r).map_err(std::io::Error::other)?)?;
    }
    Ok(())
}

/// The repo name lessons carry: the git top-level's directory name.
fn repo_name(dir: &Path) -> String {
    let top = git(dir, &["rev-parse", "--show-toplevel"]).map(|t| PathBuf::from(t.trim())).unwrap_or_else(|| dir.to_path_buf());
    top.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Read, generalise and (with `apply`) store. `cfg`/`base` route the directory like a drone's cwd.
pub fn run(opts: &Options, cfg: Option<&Config>, base: &Path, ask: Ask, now: i64) -> Report {
    let mut report = Report::default();
    let dir = opts.dir.canonicalize().unwrap_or_else(|_| opts.dir.clone());
    let cwd = dir.display().to_string();
    // An off directory is never read, whichever collective would receive it.
    let source = collective::route(cfg, &cwd, base);
    if source == Route::Off {
        return report;
    }
    let target = match &opts.target {
        Target::Source => source,
        Target::For(p) => collective::route(cfg, &p.canonicalize().unwrap_or_else(|_| p.clone()).display().to_string(), base),
        Target::Named(n) => match cfg {
            None if n == "default" => Route::Collective { name: n.clone(), home: base.to_path_buf(), share: collective::Share::Team },
            None => Route::Off,
            Some(c) => c.collectives.iter().find(|e| &e.name == n).map_or(Route::Off, |e| Route::Collective {
                name: e.name.clone(),
                home: collective::store(base, &e.name),
                share: e.share,
            }),
        },
    };
    let Route::Collective { name, home, .. } = target else {
        report.error = Some(match &opts.target {
            Target::Named(n) => format!("no collective named {n:?} in the collectives config: nothing read or written"),
            _ => "the target routes off: no collective receives these lessons; nothing read or written".into(),
        });
        return report;
    };
    report.routed = match cfg {
        None => "every repo (no collectives config)".into(),
        Some(c) => {
            let mut paths: Vec<String> = c.collectives.iter().filter(|e| e.name == name).flat_map(|e| e.paths.clone()).collect();
            if c.default == name {
                paths.push("every repo no rule matches".into());
            }
            if paths.is_empty() { "no repo".into() } else { paths.join(", ") }
        }
    };
    report.collective = Some((name, home.clone()));
    // `--apply --plan` applies exactly the reviewed plan: a plan that cannot be read stops the run.
    let planned: Option<HashMap<(String, String), Done>> = match opts.plan.as_ref().filter(|_| opts.apply) {
        None => None,
        Some(p) => {
            let read = std::fs::read_to_string(p).map_err(|e| e.to_string()).and_then(|t| {
                let v: Value = serde_json::from_str(&t).map_err(|e| e.to_string())?;
                serde_json::from_value::<Vec<Done>>(v["chunks"].clone()).map_err(|e| e.to_string())
            });
            match read {
                Ok(ds) => Some(ds.into_iter().map(|d| ((d.key.clone(), d.sha.clone()), d)).collect()),
                Err(e) => {
                    report.error = Some(format!("cannot read the plan {} ({e}): nothing written", p.display()));
                    return report;
                }
            }
        }
    };
    let repo = repo_name(&dir);
    let all = chunks(&dir, &opts.spec);
    report.chunks = all.len();
    let paths = tracked(&dir);

    let before: HashMap<String, String> =
        read_jsonl::<Imported>(&home.join("assimilated.jsonl")).into_iter().filter(|i| i.source == repo).map(|i| (i.key, i.sha)).collect();
    let mut todo: Vec<&Chunk> = Vec::new();
    for c in &all {
        match before.get(&c.key) {
            Some(sha) if *sha == c.sha => report.known += 1,
            Some(_) => report.changed.push(c.key.clone()),
            None => todo.push(c),
        }
    }

    let mut prov: HashMap<String, Provenance> = HashMap::new();
    for c in &todo {
        prov.entry(c.path.clone()).or_insert_with(|| provenance(&dir, &c.path));
    }
    // With a reviewed plan, its answers are taken as they are (scope guards re-applied) and nothing goes to
    // the model: a chunk the plan does not answer is listed and skipped. Without one, everything new is asked.
    let (from_plan, ask_for): (Vec<&Chunk>, Vec<&Chunk>) = match &planned {
        Some(p) => {
            let (yes, no): (Vec<&Chunk>, Vec<&Chunk>) = todo.iter().partition(|c| p.contains_key(&(c.key.clone(), c.sha.clone())));
            report.unplanned = no.iter().map(|c| c.key.clone()).collect();
            (yes, Vec::new())
        }
        None => (Vec::new(), todo.clone()),
    };
    let planned = planned.unwrap_or_default();
    let mut done: Vec<Done> = from_plan
        .iter()
        .map(|c| {
            let mut d = planned[&(c.key.clone(), c.sha.clone())].clone();
            guard(&mut d, &known_names(&paths, c), &format!("{}\n{}", c.text, c.about));
            d
        })
        .collect();
    report.calls = ask_for.len();
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, Result<Done, ()>)>> = Mutex::new(Vec::new());
    let (failures, successes) = (AtomicUsize::new(0), AtomicUsize::new(0));
    std::thread::scope(|s| {
        for _ in 0..opts.jobs.max(1).min(ask_for.len().max(1)) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(c) = ask_for.get(i) else { break };
                    // The model is unavailable (quota, login): stop asking instead of burning the queue.
                    if failures.load(Ordering::SeqCst) >= 3 && successes.load(Ordering::SeqCst) == 0 {
                        results.lock().unwrap().push((i, Err(())));
                        continue;
                    }
                    let got = ask(&prompt(&repo, c), &schema()).and_then(|(v, cost)| build(&repo, &cwd, c, &v, &prov[&c.path], now).map(|(l, d)| (l, d, cost)));
                    let r = match got {
                        Some((lessons, decisions, cost_usd)) => {
                            successes.fetch_add(1, Ordering::SeqCst);
                            let mut d = Done { key: c.key.clone(), sha: c.sha.clone(), lessons, decisions, cost_usd, ..Default::default() };
                            guard(&mut d, &known_names(&paths, c), &format!("{}\n{}", c.text, c.about));
                            eprintln!("  {} -> {} lessons, {} open decisions, ${:.4}", c.key, d.lessons.len(), d.decisions.len(), cost_usd.unwrap_or(0.0));
                            Ok(d)
                        }
                        None => {
                            failures.fetch_add(1, Ordering::SeqCst);
                            eprintln!("  {} -> no usable answer from the model", c.key);
                            Err(())
                        }
                    };
                    results.lock().unwrap().push((i, r));
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|(i, _)| *i);
    for (i, r) in results {
        match r {
            Ok(d) => done.push(d),
            Err(()) => report.failed.push(ask_for[i].key.clone()),
        }
    }
    // `+ 0.0`: an empty f64 sum is -0.0, printed as "$-0.0000".
    report.cost_usd = done.iter().filter(|d| !planned.contains_key(&(d.key.clone(), d.sha.clone()))).filter_map(|d| d.cost_usd).sum::<f64>() + 0.0;

    if !opts.apply {
        if let Some(p) = &opts.plan {
            let plan = json!({"source": repo, "dir": cwd, "chunks": done});
            if std::fs::write(p, serde_json::to_string_pretty(&plan).unwrap_or_default()).is_err() {
                eprintln!("borg assimilate: cannot write the plan to {}", p.display());
            }
        }
        report.done = done;
        return report;
    }
    let known_lessons: HashSet<String> = learn::lessons(&home.join("lessons.jsonl")).into_iter().map(|l| l.id).collect();
    for d in &done {
        let fresh: Vec<&Lesson> = d.lessons.iter().filter(|l| !known_lessons.contains(&l.id)).collect();
        if append_jsonl(&home.join("lessons.jsonl"), &fresh).is_err() {
            eprintln!("borg assimilate: cannot write {}", home.join("lessons.jsonl").display());
            continue;
        }
        report.new_lessons += fresh.len();
        for l in &fresh {
            crate::stats::count(&home, crate::stats::LESSON_NEW, "", |ev| {
                ev.lessons = vec![l.id.clone()];
                ev.repo = repo.clone();
                ev.why = "import".into();
            });
        }
        for e in &d.decisions {
            if queen::seed(&home, e).unwrap_or(false) {
                report.new_decisions += 1;
                report.queued.push(e.clone());
            }
        }
        // Recorded last: a chunk whose lessons could not be written is retried by the next run.
        let rec = Imported {
            source: repo.clone(),
            key: d.key.clone(),
            sha: d.sha.clone(),
            lessons: d.lessons.iter().map(|l| l.id.clone()).collect(),
            decisions: d.decisions.iter().map(|e| e.id.clone()).collect(),
            cost_usd: d.cost_usd,
            ms: now,
        };
        let _ = append_jsonl(&home.join("assimilated.jsonl"), &[rec]);
        // Its model calls are counted as they happen (learn.rs `ask_model`); this counts the chunk.
        crate::stats::count(&home, crate::stats::ASSIMILATED, "", |ev| {
            ev.repo = repo.clone();
            ev.n = fresh.len() as u64;
        });
    }
    report.done = done;
    report
}

pub fn render(r: &Report, apply: bool) -> String {
    if let Some(e) = &r.error {
        return format!("borg assimilate: {e}\n");
    }
    let Some((name, home)) = &r.collective else {
        return "off: this directory belongs to no collective; nothing read or written\n".into();
    };
    let mut s = String::new();
    let mut by_part: BTreeMap<String, usize> = BTreeMap::new();
    for d in &r.done {
        for l in &d.lessons {
            *by_part
                .entry(d.key.split('#').nth(1).map_or("knowledge", |p| if p.parse::<usize>().is_ok() { "knowledge" } else { p }).to_string())
                .or_default() += 1;
            s.push_str(&format!("[{}] {}\n", l.area, l.question));
            if !l.situations.is_empty() {
                s.push_str(&format!("    situations: {}\n", l.situations.join(" | ")));
            }
            s.push_str(&format!("    -> {}\n", l.consequence));
            if !l.scope.is_empty() {
                s.push_str(&format!("    scope: {}\n", l.scope.join(", ")));
            }
            if !l.overrides.is_empty() {
                s.push_str(&format!("    overrides: {}\n", l.overrides));
            }
            s.push_str(&format!("    origin: {} ({})\n", l.origin, l.engineers.join(", ")));
        }
        for e in &d.decisions {
            s.push_str(&format!(
                "[open decision {}] {}\n    options: {}\n    default: {}\n    evidence: {}\n",
                e.id,
                e.question,
                e.options.join(" / "),
                e.default,
                e.evidence.join("; ")
            ));
        }
    }
    let lessons: usize = r.done.iter().map(|d| d.lessons.len()).sum();
    let decisions: usize = r.done.iter().map(|d| d.decisions.len()).sum();
    s.push_str(&format!(
        "\n{name} {}: {} chunks; {} imported before; {} model calls (${:.4}); {} lessons, {} open decisions{}\n",
        home.display(),
        r.chunks,
        r.known,
        r.calls,
        r.cost_usd,
        lessons,
        decisions,
        if apply {
            format!("; written: {} new lessons, {} new queue entries", r.new_lessons, r.new_decisions)
        } else {
            " (dry run: nothing written; --apply to assimilate)".into()
        }
    ));
    if !by_part.is_empty() {
        s.push_str(&format!("by source: {}\n", by_part.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ")));
    }
    s.push_str(&format!("collective {name} receives these lessons; repos routed to it: {}\n", r.routed));
    let rejected: BTreeMap<&str, ()> = r.done.iter().flat_map(|d| &d.rejected).map(|n| (n.as_str(), ())).collect();
    if !rejected.is_empty() {
        s.push_str(&format!(
            "scope names rejected (a common word, or in no identifier or path of the source): {}\n",
            rejected.into_keys().collect::<Vec<_>>().join(", ")
        ));
    }
    let dropped: Vec<&String> = r.done.iter().flat_map(|d| &d.dropped).collect();
    if !dropped.is_empty() {
        s.push_str(&format!("dropped {} lessons whose every scope name was rejected:\n", dropped.len()));
        for q in dropped {
            s.push_str(&format!("    {q}\n"));
        }
    }
    if !r.unplanned.is_empty() {
        s.push_str(&format!("not in the plan or changed since it (skipped, nothing asked; re-run the dry run): {}\n", r.unplanned.join(", ")));
    }
    if !r.changed.is_empty() {
        s.push_str(&format!("changed since their import (not re-imported): {}\n", r.changed.join(", ")));
    }
    if !r.failed.is_empty() {
        s.push_str(&format!("no answer from the model for {} chunks (re-run to retry): {}\n", r.failed.len(), r.failed.join(", ")));
    }
    s
}

pub const USAGE: &str = "usage: borg assimilate --from-dir DIR [--apply] [--plan FILE] [--collective NAME | --for PATH] \
[--knowledge GLOB]... [--retros GLOB]... [--families GLOB]... [--decisions GLOB]... [--jobs N]";

/// `borg assimilate --from-dir DIR ...`; the exit code.
pub fn main(args: &[&str], ask: Ask) -> i32 {
    let mut spec = Spec::default();
    let mut given: HashSet<&str> = HashSet::new();
    let (mut dir, mut plan, mut jobs) = (None, None, std::env::var("BORG_ASSIMILATE_JOBS").ok().and_then(|j| j.parse().ok()).unwrap_or(4));
    let mut target = Target::Source;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().map(|v| v.to_string());
        match *a {
            "--from-dir" => dir = val(),
            "--plan" => plan = val().map(PathBuf::from),
            "--collective" | "--for" if !matches!(target, Target::Source) => {
                eprintln!("borg assimilate: give one of --collective and --for\n{USAGE}");
                return 2;
            }
            "--collective" | "--for" => {
                let Some(v) = val() else {
                    eprintln!("{USAGE}");
                    return 2;
                };
                target = if *a == "--for" { Target::For(PathBuf::from(v)) } else { Target::Named(v) };
            }
            "--jobs" => match val().and_then(|v| v.parse().ok()) {
                Some(j) => jobs = j,
                None => {
                    eprintln!("borg assimilate: --jobs wants a number\n{USAGE}");
                    return 2;
                }
            },
            flag @ ("--knowledge" | "--retros" | "--families" | "--decisions") => {
                let Some(g) = val() else {
                    eprintln!("{USAGE}");
                    return 2;
                };
                let list = match flag {
                    "--knowledge" => &mut spec.knowledge,
                    "--retros" => &mut spec.retros,
                    "--families" => &mut spec.families,
                    _ => &mut spec.decisions,
                };
                if given.insert(flag) {
                    list.clear(); // a given glob replaces that part's defaults
                }
                list.push(g);
            }
            "--apply" => {}
            other => {
                eprintln!("borg assimilate: unknown argument {other}\n{USAGE}");
                return 2;
            }
        }
    }
    let Some(dir) = dir.map(PathBuf::from).filter(|d| d.is_dir()) else {
        eprintln!("borg assimilate: --from-dir wants a directory\n{USAGE}");
        return 2;
    };
    let cfg = match collective::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("borg assimilate: invalid collectives config ({e}): every repo is off, nothing to assimilate");
            return 1;
        }
    };
    let opts = Options { dir, spec, apply: args.contains(&"--apply"), plan, jobs, target };
    let report = run(&opts, cfg.as_ref(), &learn::home(), ask, learn::now_ms());
    print!("{}", render(&report, opts.apply));
    crate::notify::queued(&report.queued);
    // The imported lessons get their search facets (facets.rs), one more call each.
    if let Some((_, home)) = report.collective.as_ref().filter(|_| opts.apply && report.new_lessons > 0 && crate::facets::enabled()) {
        let ids: HashSet<String> = report.done.iter().flat_map(|d| d.lessons.iter().map(|l| l.id.clone())).collect();
        let f = crate::facets::fill(&home.join("lessons.jsonl"), Some(&ids), true, ask, opts.jobs, learn::now_ms());
        println!(
            "facets: written for {} of {} lessons, ${:.4}{}",
            f.written,
            f.missing,
            f.cost_usd,
            if f.failed.is_empty() { String::new() } else { format!("; {} failed, `borg facets --apply` retries", f.failed.len()) }
        );
    }
    // ... and their vectors (vectors.rs), without a model call.
    if let Some((_, home)) = report.collective.as_ref().filter(|_| opts.apply && report.new_lessons > 0) {
        crate::vectors::refresh(&home.join("lessons.jsonl"));
    }
    if report.error.is_some() {
        return 2;
    }
    i32::from(!report.failed.is_empty() || !report.unplanned.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-assimilate-dir-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // Resolved: collective paths match as written, and macOS's temp dir sits behind /var -> /private/var.
        std::fs::canonicalize(&d).unwrap()
    }

    fn sh(dir: &Path, args: &[&str]) {
        let ok = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").output().unwrap().status.success();
        assert!(ok, "git {args:?}");
    }

    const KNOWLEDGE: &str = "# Orion Petri nets\n\n**Scope of this whole file:** Orion's text and voice nets. Do not apply it to other flowlib projects.\n\n\
| Orion rule | Upstream rule it changes | Why |\n|---|---|---|\n| Per-turn state is drained, not reset. | SKILL seeds a budget with a reset on the starting transition. | Resets hide where a token dies. |\n\n\
**Open conflict.** `SplitInput` still resets `RETRY_BUDGET`. Until this is decided, treat these resets as legacy.\n";

    const RETRO: &str = "# Plan Retrospective: X-1\n\n| Metric | Score |\n|---|---|\n| Scope | 67% |\n\n\
| Problem | Impact | Preventable? |\n|---|---|---|\n| Premise not checked against develop | HIGH | Yes - one curl would have shown it |\n| Vendor outage | LOW | No |\n";

    const AUDIT: &str = "# Audit\n\n## 3. Real failures\n\n| Failure family | Evidence | Frequency | Case before this audit | New case |\n|---|---|---|---|---|\n\
| Premise not checked | PR-1 | 9 of 17 retros | ws9 | ws15 |\n";

    const REVIEW: &str =
        "# Review\n\n### ws13: epic readiness\n- Opinion: a house rule.\n- **Your decision:** is EPIC_READINESS, as written, your rule?\n\n## Other\ntext\n";

    /// A git repo shaped like the handbook, two engineers on the knowledge file.
    fn fixture(root: &Path) -> PathBuf {
        let dir = root.join("team_handbook");
        std::fs::create_dir_all(dir.join("skills/team-knowledge/references")).unwrap();
        std::fs::create_dir_all(dir.join("docs/plan-retrospectives")).unwrap();
        std::fs::create_dir_all(dir.join("docs/skill-evaluations")).unwrap();
        sh(&dir, &["init", "-q"]);
        let commit = |who: &str, msg: &str| {
            sh(&dir, &["add", "-A"]);
            sh(&dir, &["-c", &format!("user.email={who}"), "-c", "user.name=x", "commit", "-qm", msg]);
        };
        std::fs::write(dir.join("skills/team-knowledge/references/orion-petri.md"), "# Orion Petri nets\n").unwrap();
        std::fs::write(
            dir.join("skills/team-knowledge/SKILL.md"),
            "---\nname: team-knowledge\ndescription: Team facts for Orion. Not for other flowlib work.\n---\n# Team knowledge\n",
        )
        .unwrap();
        commit("ann@x", "start");
        std::fs::write(dir.join("skills/team-knowledge/references/orion-petri.md"), KNOWLEDGE).unwrap();
        std::fs::write(dir.join("docs/plan-retrospectives/X-1-plan-retrospective.md"), RETRO).unwrap();
        std::fs::write(dir.join("docs/skill-evaluations/eval-coverage-audit-1.md"), AUDIT).unwrap();
        std::fs::write(dir.join("docs/review.md"), REVIEW).unwrap();
        commit("bob@x", "knowledge");
        // Untracked notes are not team knowledge.
        std::fs::write(dir.join("skills/team-knowledge/references/scratch.md"), "# mine\nsecret plan\n").unwrap();
        dir
    }

    /// The model: one lesson per chunk, scoped and overriding for the knowledge file, and the open
    /// conflict as a decision; a decision chunk's stray lesson must be dropped.
    fn model(calls: &AtomicUsize) -> impl Fn(&str, &Value) -> Option<(Value, Option<f64>)> + Sync + '_ {
        move |p: &str, _s: &Value| {
            calls.fetch_add(1, Ordering::SeqCst);
            assert!(!p.contains("ws15") && !p.contains("secret plan"), "eval case ids and untracked notes never reach the model");
            let src = p.split("## Source: ").nth(1)?.lines().next()?.to_string();
            // The team knowledge base's own scope frames every source, retros and families included.
            assert!(p.contains("states its own scope as: Team facts for Orion. Not for other flowlib work."), "{src}");
            let v = if src.contains("orion-petri") && !p.contains("open decision about the team") {
                json!({"lessons": [{"question": "Is each per-turn place drained by an explicit consumer, not cleared by a new reset arc?",
                    "situations": ["adding a per-turn fact to a net"], "consequence": "resets hide where a token dies", "area": "protocol-correctness",
                    "scope": ["Orion"], "overrides": "upstream petri-net-design seeds a budget with a reset on the starting transition", "line": 7}],
                    "open_decisions": [{"question": "Should SplitInput keep resetting RETRY_BUDGET?", "options": ["reset", "drain"],
                    "default": "treat as legacy", "evidence": "Open conflict", "line": 9}]})
            } else if p.contains("open decision about the team") {
                json!({"lessons": [{"question": "stray", "situations": [], "consequence": "", "area": "other", "scope": [], "overrides": "", "line": 1}],
                    "open_decisions": [{"question": "Should SplitInput keep resetting RETRY_BUDGET?", "options": [], "default": "", "evidence": "x", "line": 999}]})
            } else {
                json!({"lessons": [{"question": format!("Was the premise checked against real data? ({src})"), "situations": [],
                    "consequence": "a milestone is built on a wrong premise", "area": "Data Source", "scope": ["team_handbook"], "overrides": "", "line": 999}],
                    "open_decisions": []})
            };
            Some((v, Some(0.01)))
        }
    }

    #[test]
    fn chunks_keep_prevention_rows_and_drop_case_columns() {
        let retro = chunks_of(Part::Retro, "r.md", RETRO);
        assert_eq!(retro.len(), 1);
        assert!(retro[0].text.contains("Premise not checked against develop") && !retro[0].text.contains("67%"), "{}", retro[0].text);
        assert!(retro[0].lines.contains(&9) && retro[0].text.contains("   9| | Premise"), "{}", retro[0].text);
        let fam = chunks_of(Part::Family, "a.md", AUDIT);
        assert!(fam[0].text.contains("| Premise not checked | PR-1 | 9 of 17 retros |") && !fam[0].text.contains("ws15"), "{}", fam[0].text);
        let dec = chunks_of(Part::Decision, "k.md", KNOWLEDGE);
        assert!(dec[0].text.contains("RETRY_BUDGET") && dec[0].text.contains("# Orion Petri nets"), "{}", dec[0].text);
        let dec = chunks_of(Part::Decision, "r.md", REVIEW);
        assert_eq!(dec[0].lines, [1, 3, 4, 5], "the title, then the block from its heading to the marker");
        assert!(chunks_of(Part::Decision, "k.md", RETRO).is_empty(), "no marker, no chunk");
        assert!(chunks_of(Part::Retro, "k.md", KNOWLEDGE).is_empty());
        assert!(ticket_prefix("LUMEN", "# Retro: LUMEN-213\n| a LUMEN-12 row |"));
        assert!(!ticket_prefix("Lumen", "# Retro: LUMEN-213\nthe Lumen environment"), "a proper noun stays");
        assert!(!ticket_prefix("SR3", "SR3 summaries; SR3-1 is a ticket"), "named on its own too");
        // Globs: `**/` spans directories, `*` stays within one.
        assert!(glob("docs/**/*coverage-audit*.md").is_match("docs/skill-evaluations/eval-coverage-audit-1.md"));
        assert!(glob("docs/**/*.md").is_match("docs/a.md"));
        assert!(!glob("skills/*-knowledge/SKILL.md").is_match("skills/a/b-knowledge/SKILL.md"));
    }

    #[test]
    fn dry_run_prints_scoped_lessons_and_writes_nothing() {
        let root = temp("dry");
        let dir = fixture(&root);
        let base = root.join("store");
        let calls = AtomicUsize::new(0);
        let plan = root.join("plan.json");
        let opts = Options { dir: dir.clone(), spec: Spec::default(), apply: false, plan: Some(plan.clone()), jobs: 2, target: Target::Source };
        let r = run(&opts, None, &base, &model(&calls), 5);
        assert!(!base.exists(), "a dry run writes nothing to the store");
        // SKILL.md and a reference, retro, families, and the decision block of docs/review.md.
        assert_eq!((r.chunks, r.calls, calls.load(Ordering::SeqCst)), (5, 5, 5));
        let lessons: Vec<&Lesson> = r.done.iter().flat_map(|d| &d.lessons).collect();
        let drain = lessons.iter().find(|l| l.question.contains("drained")).unwrap();
        assert_eq!(drain.scope, ["Orion"]);
        assert!(drain.overrides.contains("reset on the starting transition"));
        assert_eq!(drain.origin, "skills/team-knowledge/references/orion-petri.md:7");
        assert_eq!(
            (drain.engineers.clone(), drain.source.as_str(), drain.area.as_str()),
            (vec!["ann@x".to_string(), "bob@x".to_string()], "import", "protocol-correctness")
        );
        assert!(drain.origin_ms > 0, "the file's last commit time");
        let retro = lessons.iter().find(|l| l.origin.starts_with("docs/plan-retrospectives")).unwrap();
        assert_eq!((retro.area.as_str(), retro.origin.split(':').nth(1)), ("data-source", Some("1")), "an unknown line falls back to the chunk's first");
        assert!(retro.scope.is_empty(), "the repo the knowledge is kept in is not a scope");
        assert!(!lessons.iter().any(|l| l.question == "stray"), "a decision chunk gives no lessons");
        let decisions: Vec<&Entry> = r.done.iter().flat_map(|d| &d.decisions).collect();
        assert_eq!(decisions.len(), 2, "found twice; the queue dedups by key");
        assert_eq!((decisions[0].kind, decisions[0].decision_key == decisions[1].decision_key), (Kind::OpenDecision, true));
        let text = render(&r, false);
        assert!(text.contains("scope: Orion") && text.contains("overrides: upstream") && text.contains("dry run") && text.contains("$0.0500"), "{text}");
        assert!(plan.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn apply_writes_the_routed_store_once_and_reuses_a_reviewed_plan() {
        let root = temp("apply");
        let dir = fixture(&root);
        let base = root.join("store");
        let calls = AtomicUsize::new(0);
        let plan = root.join("plan.json");
        run(
            &Options { dir: dir.clone(), spec: Spec::default(), apply: false, plan: Some(plan.clone()), jobs: 1, target: Target::Source },
            None,
            &base,
            &model(&calls),
            5,
        );
        // --apply --plan: the reviewed answers are applied without asking again.
        let opts = Options { dir: dir.clone(), spec: Spec::default(), apply: true, plan: Some(plan), jobs: 1, target: Target::Source };
        let r = run(&opts, None, &base, &|_, _| panic!("the plan answers every chunk"), 6);
        assert_eq!((r.new_lessons, r.new_decisions, r.calls), (4, 1, 0));
        let stored = learn::lessons(&base.join("lessons.jsonl"));
        assert_eq!(stored.len(), 4);
        let queue = queen::open(&base);
        assert_eq!((queue.len(), queue[0].kind, queue[0].session.as_str()), (1, Kind::OpenDecision, "import"));
        // A seeded decision is not a message: the engineer's queen budget is untouched.
        assert!(queen::within_budget(&base, &queue[0].engineer, "s1"));
        // The conscience asks the scoped lesson only where Orion is named.
        let cs = crate::conscience::load(&base.join("lessons.jsonl"), None);
        let asked = |ctx: &str| crate::conscience::select(&cs, ctx, 5).iter().any(|c| c.question.contains("drained"));
        assert!(asked("Orion text net: add a per-turn hint place, drained or reset arc?"));
        assert!(!asked("PressNet: per-turn reprint budget place, drained or reset arc?"));
        // Re-running costs nothing and writes nothing.
        let before = std::fs::read_to_string(base.join("lessons.jsonl")).unwrap();
        let again = run(&Options { plan: None, ..opts }, None, &base, &|_, _| panic!("nothing new"), 7);
        assert_eq!((again.known, again.calls, again.new_lessons), (5, 0, 0));
        assert_eq!(std::fs::read_to_string(base.join("lessons.jsonl")).unwrap(), before);
        // A changed source is reported, not silently re-imported.
        std::fs::write(dir.join("docs/plan-retrospectives/X-1-plan-retrospective.md"), RETRO.replace("HIGH", "MEDIUM")).unwrap();
        sh(&dir, &["-c", "user.email=cy@x", "-c", "user.name=x", "commit", "-qam", "edit"]);
        let changed = run(
            &Options { dir: dir.clone(), spec: Spec::default(), apply: true, plan: None, jobs: 1, target: Target::Source },
            None,
            &base,
            &|_, _| panic!("changed chunks are not re-asked"),
            8,
        );
        assert_eq!(changed.changed, ["docs/plan-retrospectives/X-1-plan-retrospective.md#prevention"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_off_directory_is_never_read_and_failures_retry() {
        let root = temp("off");
        let dir = fixture(&root);
        let base = root.join("store");
        let off: Config = serde_json::from_str(&format!(r#"{{"default": "off", "off": ["{}"]}}"#, root.display())).unwrap();
        let r = run(
            &Options { dir: dir.clone(), spec: Spec::default(), apply: true, plan: None, jobs: 1, target: Target::Source },
            Some(&off),
            &base,
            &|_, _| panic!("off"),
            1,
        );
        assert!(r.collective.is_none() && !base.exists());
        assert!(render(&r, true).starts_with("off:"));
        // The model is down: nothing recorded, the run stops asking after three failures, a re-run retries.
        let calls = AtomicUsize::new(0);
        let down = |_: &str, _: &Value| {
            calls.fetch_add(1, Ordering::SeqCst);
            None
        };
        let r = run(&Options { dir: dir.clone(), spec: Spec::default(), apply: true, plan: None, jobs: 1, target: Target::Source }, None, &base, &down, 2);
        assert_eq!((r.failed.len(), calls.load(Ordering::SeqCst)), (5, 3));
        assert!(!base.join("assimilated.jsonl").exists());
        let ok = AtomicUsize::new(0);
        let r = run(&Options { dir, spec: Spec::default(), apply: true, plan: None, jobs: 3, target: Target::Source }, None, &base, &model(&ok), 3);
        assert_eq!((r.calls, r.failed.len(), r.new_lessons), (5, 0, 4));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn scope_guard_rejects_common_words_and_prose_only_names_and_never_widens() {
        let c = Chunk {
            text: "  3| Orion's `OutputFilter` and harbor_orion; the Answer Guard gives guidance; Draco, Heron and Beacon are neighbours\n  4| `SCRIPTED` mode; heron.live.example/settings; an beacon of maps\n".into(),
            ..chunk(Part::Knowledge, "k".into(), "k.md", &[])
        };
        let paths = vec!["skills/team-knowledge/references/orion-petri.md".to_string(), "docs/beacon-runs/x.md".into(), "docs/guidance/y.md".into()];
        let lesson = |q: &str, scope: &[&str]| Lesson { question: q.into(), scope: scope.iter().map(|s| s.to_string()).collect(), ..Default::default() };
        let mut d = Done {
            lessons: vec![
                lesson("drain?", &["Orion", "harbor_orion", "Answer Guard", "Guidance", "Draco", "OutputFilter", "Heron"]),
                lesson("beacon?", &["Beacon", "SCRIPTED"]),
                lesson("any?", &[]),
            ],
            ..Default::default()
        };
        guard(&mut d, &known_names(&paths, &c), &c.text);
        let qs: Vec<(&str, Vec<String>)> = d.lessons.iter().map(|l| (l.question.as_str(), l.scope.clone())).collect();
        // "Heron": a common word the source only writes as a name, shown in a host: kept.
        assert_eq!(qs, [("drain?", vec!["Orion".to_string(), "harbor_orion".into(), "OutputFilter".into(), "Heron".into()]), ("any?", vec![])]);
        assert_eq!(d.dropped, ["beacon?"], "a lesson left without a scope is dropped, never unscoped");
        for n in ["Answer Guard", "Guidance", "Draco", "Beacon", "SCRIPTED"] {
            assert!(d.rejected.contains(&n.to_string()), "{n}: {:?}", d.rejected);
        }
    }

    #[test]
    fn apply_with_a_plan_never_asks_the_model() {
        let root = temp("strict");
        let dir = fixture(&root);
        let base = root.join("store");
        let opts =
            |plan: &Path| Options { dir: dir.clone(), spec: Spec::default(), apply: true, plan: Some(plan.to_path_buf()), jobs: 1, target: Target::Source };
        // A typo in the plan path stops the run: nothing asked, nothing written.
        let r = run(&opts(&root.join("no-such-plan.json")), None, &base, &|_, _| panic!("never asked"), 1);
        assert!(r.error.as_deref().is_some_and(|e| e.contains("cannot read the plan")) && !base.exists(), "{:?}", r.error);
        assert!(render(&r, true).contains("cannot read the plan"));
        std::fs::write(root.join("bad.json"), "{not json").unwrap();
        assert!(run(&opts(&root.join("bad.json")), None, &base, &|_, _| panic!("never asked"), 1).error.is_some());
        // A plan that misses a chunk (or whose chunk changed since): that chunk is listed and skipped.
        let calls = AtomicUsize::new(0);
        let plan = root.join("plan.json");
        run(&Options { apply: false, ..opts(&plan) }, None, &base, &model(&calls), 2);
        let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&plan).unwrap()).unwrap();
        v["chunks"].as_array_mut().unwrap().retain(|d| !d["key"].as_str().unwrap().contains("retrospective"));
        std::fs::write(&plan, v.to_string()).unwrap();
        let r = run(&opts(&plan), None, &base, &|_, _| panic!("never asked"), 3);
        assert_eq!((r.calls, r.unplanned.clone()), (0, vec!["docs/plan-retrospectives/X-1-plan-retrospective.md#prevention".to_string()]));
        assert_eq!(r.new_lessons, 3);
        assert!(render(&r, true).contains("not in the plan or changed since it"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lessons_go_to_the_collective_the_drones_work_in() {
        let root = temp("target");
        let dir = fixture(&root);
        let base = root.join("store");
        let code = root.join("code");
        std::fs::create_dir_all(code.join("harbor_orion")).unwrap();
        let cfg: Config = serde_json::from_str(&format!(
            r#"{{"default": "off", "collectives": [{{"name": "docs", "paths": ["{}"]}}, {{"name": "team", "paths": ["{}"]}}]}}"#,
            dir.display(),
            code.display()
        ))
        .unwrap();
        let calls = AtomicUsize::new(0);
        let opts = |target: Target| Options { dir: dir.clone(), spec: Spec::default(), apply: false, plan: None, jobs: 1, target };
        let by_source = run(&opts(Target::Source), Some(&cfg), &base, &model(&calls), 1);
        assert_eq!(by_source.collective.as_ref().unwrap().0, "docs");
        let r = run(&opts(Target::For(code.join("harbor_orion"))), Some(&cfg), &base, &model(&calls), 1);
        assert_eq!(r.collective.clone().unwrap(), ("team".to_string(), collective::store(&base, "team")));
        assert!(
            render(&r, false).contains(&format!("collective team receives these lessons; repos routed to it: {}", code.display())),
            "{}",
            render(&r, false)
        );
        assert_eq!(run(&opts(Target::Named("team".into())), Some(&cfg), &base, &model(&calls), 1).collective.unwrap().0, "team");
        let none = run(&opts(Target::Named("nope".into())), Some(&cfg), &base, &|_, _| panic!("no target, no call"), 1);
        assert!(none.error.as_deref().is_some_and(|e| e.contains("no collective named")));
        let off = run(&opts(Target::For(root.join("elsewhere"))), Some(&cfg), &base, &|_, _| panic!("no target, no call"), 1);
        assert!(off.error.as_deref().is_some_and(|e| e.contains("routes off")));
        assert!(!base.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
