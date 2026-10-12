//! `borg why`: the reasoning behind code, on demand (PLAN.md "Recall mechanics").
//!
//! `borg why <path>[:<line>[-<line>]]` blames the span (the whole file without a range) and
//! `borg why <commit>` takes that commit. For each distinct commit, newest first: short sha, date,
//! author and subject, then the drone's reasoning (task, last proposal, episode ids) and the lessons
//! learned from those episodes (question and consequence, one line each).
//!
//! **Where the reasoning comes from**: the commit's borg note (`refs/notes/borg`, notes.rs); if the
//! repo lacks it, the excerpts the collective's store holds (`reasoning.jsonl`: this machine's own
//! and those the mesh replicated to it from peers). No network and no model call: `borg notes fetch`
//! is the step that asks peers. Lessons come from the routed collective's `lessons.jsonl` only.
//!
//! **Privacy**: decided before git is asked anything. A cwd (or target) that routes off, or an
//! invalid config, prints nothing and exits 0. A note or excerpt that names another collective is
//! never shown (a private collective's note only where the cwd routes to that same collective);
//! an excerpt from before notes named their collective is treated as this repo's. Everything shown
//! is redacted again at display time (`notes::redact`), whatever the note says. Paths are routed
//! resolved (absolute, symlinks followed, as git sees them), and the repo git reads must route to
//! the cwd's collective too: a symlink or submodule into an off or other repo prints nothing.
//!
//! **Cost**: the blame headers give each commit's time, so the cap is applied before any
//! per-commit work; then one `git log`, one `git notes list` plus one `cat-file --batch`, and one
//! read of `reasoning.jsonl` (only if a shown commit lacks a note).
//!
//! **Compact by default**: at most `MAX_COMMITS` commits, the task's first line and the proposal's
//! last `PROPOSAL_CAP` characters, `MAX_LESSONS` lessons per commit. `--full` shows everything,
//! `--json` prints the same (capped unless `--full`) for scripts. It costs tokens only when a drone
//! runs it: nothing injects it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::collective::{self, Config, Route};
use crate::learn;
use crate::notes::{self, Excerpt, redact};

const MAX_COMMITS: usize = 8;
const MAX_LESSONS: usize = 3;
const TASK_CAP: usize = 240;
const PROPOSAL_CAP: usize = 400;
const LESSON_CAP: usize = 240;

const USAGE: &str = "usage: borg why <path>[:<line>[-<line>]] | <commit> [--full] [--json] [--dir DIR]";

/// What to explain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A file, optionally a 1-based inclusive line range.
    Span {
        path: String,
        lines: Option<(u32, u32)>,
    },
    Commit(String),
}

/// `path`, `path:12` or `path:12-20` (a commit is told apart later, by what exists).
pub fn parse_span(arg: &str) -> Result<(String, Option<(u32, u32)>), String> {
    let Some((path, range)) = arg.rsplit_once(':') else { return Ok((arg.to_string(), None)) };
    if path.is_empty() || range.is_empty() || !range.chars().all(|c| c.is_ascii_digit() || c == '-') {
        // Not a line range (`a:b` as a file name, `HEAD:path`): the whole argument.
        return Ok((arg.to_string(), None));
    }
    let num = |s: &str| s.parse::<u32>().map_err(|_| format!("bad line range {range:?} in {arg:?}"));
    let (a, b) = match range.split_once('-') {
        Some((a, b)) => (num(a)?, num(b)?),
        None => (num(range)?, num(range)?),
    };
    if a == 0 || b < a {
        return Err(format!("bad line range {range:?} in {arg:?}: lines start at 1 and the end is not before the start"));
    }
    Ok((path.to_string(), Some((a, b))))
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    git_in(dir, args, None).map(|o| String::from_utf8_lossy(&o).into_owned())
}

/// git in `dir`, with `input` on stdin (written from a thread so a large output cannot block it).
fn git_in(dir: &Path, args: &[&str], input: Option<String>) -> Result<Vec<u8>, String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let writer = match (input, child.stdin.take()) {
        (Some(text), Some(mut stdin)) => Some(std::thread::spawn(move || {
            use std::io::Write;
            let _ = stdin.write_all(text.as_bytes());
        })),
        _ => None,
    };
    let o = child.wait_with_output().map_err(|e| format!("cannot run git: {e}"))?;
    if let Some(w) = writer {
        let _ = w.join();
    }
    if !o.status.success() {
        return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
    }
    Ok(o.stdout)
}

/// `p` absolute with symlinks resolved (routing compares path text, git follows the real path);
/// lexically absolute when it does not exist.
fn resolve(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).or_else(|_| std::path::absolute(p)).unwrap_or_else(|_| p.to_path_buf())
}

fn is_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The distinct commits of `git blame --porcelain` output with their author time, in order of
/// first appearance, and the number of lines not committed yet.
pub fn blamed(porcelain: &str) -> (Vec<(String, i64)>, usize) {
    let (mut out, mut uncommitted) = (Vec::<(String, i64)>::new(), 0);
    let mut seen = HashSet::<String>::new();
    // The commit whose header lines follow (only its first appearance carries them).
    let mut current: Option<usize> = None;
    for l in porcelain.lines().filter(|l| !l.starts_with('\t')) {
        if let Some(t) = l.strip_prefix("author-time ") {
            if let Some(i) = current {
                out[i].1 = t.trim().parse().unwrap_or(0);
            }
            continue;
        }
        let mut f = l.split(' ');
        let (Some(sha), Some(_), Some(_)) = (f.next(), f.next(), f.next()) else { continue };
        if !is_sha(sha) {
            continue;
        }
        if sha.bytes().all(|b| b == b'0') {
            current = None;
            // One header per line of an uncommitted group would overcount; a group header has 4 fields.
            uncommitted += f.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
        } else if !seen.insert(sha.to_string()) {
            current = None;
        } else {
            out.push((sha.to_string(), 0));
            current = Some(out.len() - 1);
        }
    }
    (out, uncommitted)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LessonLine {
    pub id: String,
    pub question: String,
    pub consequence: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Reasoning {
    /// "note" (the repo's borg note) or "store" (the collective's reasoning.jsonl, replicated).
    pub source: String,
    pub session: String,
    pub task: String,
    pub proposal: String,
    pub episodes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Why {
    pub sha: String,
    pub short: String,
    pub author: String,
    pub date: String,
    pub subject: String,
    pub reasoning: Option<Reasoning>,
    pub lessons: Vec<LessonLine>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Report {
    pub target: String,
    pub collective: String,
    pub commits: Vec<Why>,
    /// Commits left out by the compact cap.
    pub more: usize,
    /// Lines of the span not committed yet.
    pub uncommitted: usize,
}

fn first_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

fn last_chars(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len <= n { s.to_string() } else { format!("…{}", s.chars().skip(len - n).collect::<String>()) }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether an excerpt tagged `tag` may be shown in collective `here` (module doc).
fn visible(tag: &str, here: &str) -> bool {
    tag.is_empty() || tag == here
}

/// The borg notes of `shas` that are visible in `here`, by commit: one `notes list` for the note
/// objects and one `cat-file --batch` for the notes of `shas` that have one.
fn notes_of(repo: &Path, here: &str, shas: &[String]) -> HashMap<String, Excerpt> {
    let list = git(repo, &["notes", &format!("--ref={}", notes::NOTES_REF), "list"]).unwrap_or_default();
    let wanted: HashSet<&str> = shas.iter().map(String::as_str).collect();
    let blobs: Vec<(&str, &str)> = list.lines().filter_map(|l| l.split_once(' ')).filter(|(_, commit)| wanted.contains(commit)).collect();
    if blobs.is_empty() {
        return HashMap::new();
    }
    let input: String = blobs.iter().map(|(blob, _)| format!("{blob}\n")).collect();
    let Ok(out) = git_in(repo, &["cat-file", "--batch"], Some(input)) else { return HashMap::new() };
    let mut found = HashMap::new();
    let mut rest = &out[..];
    for (_, commit) in blobs {
        // "<oid> blob <size>\n<content>\n", or "<oid> missing\n".
        let Some(nl) = rest.iter().position(|&b| b == b'\n') else { break };
        let head = String::from_utf8_lossy(&rest[..nl]).into_owned();
        rest = &rest[nl + 1..];
        let Some(size) = head.split(' ').nth(2).and_then(|n| n.parse::<usize>().ok()) else { continue };
        if rest.len() < size {
            break;
        }
        let text = String::from_utf8_lossy(&rest[..size]).into_owned();
        rest = rest.get(size + 1..).unwrap_or_default();
        if let Some(e) = Excerpt::from_note(commit, &text).filter(|e| visible(&e.collective, here)) {
            found.insert(commit.to_string(), e);
        }
    }
    found
}

/// The newest excerpt visible in `here` for each of `shas`, from the store's reasoning.jsonl,
/// read once.
fn stored_of(store: &Path, here: &str, shas: &HashSet<&str>) -> HashMap<String, Excerpt> {
    let mut found: HashMap<String, Excerpt> = HashMap::new();
    if shas.is_empty() {
        return found;
    }
    let text = std::fs::read_to_string(store.join("reasoning.jsonl")).unwrap_or_default();
    for e in text.lines().filter_map(|l| serde_json::from_str::<Excerpt>(l).ok()) {
        if e.kind != "reasoning" || !shas.contains(e.commit.as_str()) || !visible(&e.collective, here) {
            continue;
        }
        if found.get(&e.commit).is_none_or(|old| e.ms > old.ms) {
            found.insert(e.commit.clone(), e);
        }
    }
    found
}

/// Defence in depth: whatever wrote the note, it is redacted again before anyone sees it.
fn shown(e: Excerpt, source: &str) -> Reasoning {
    Reasoning {
        source: source.into(),
        session: e.session,
        task: redact(&e.task),
        proposal: redact(&e.proposal),
        episodes: e.episodes.iter().map(|x| redact(x)).collect(),
    }
}

/// The lessons in `lessons` learned from any of `episodes`.
fn lessons_of(lessons: &[learn::Lesson], episodes: &[String]) -> Vec<LessonLine> {
    lessons
        .iter()
        .filter(|l| l.episodes.iter().any(|e| episodes.contains(e)))
        .map(|l| LessonLine { id: l.id.clone(), question: redact(&l.question), consequence: redact(&l.consequence) })
        .collect()
}

/// The explanation of `target` (an argument as typed) from `cwd`, or `None` when `cwd`, the
/// target or the repo they lie in routes off or elsewhere. `base` is the root store (`BORG_HOME`).
/// Paths are routed resolved (absolute, symlinks followed), as git sees them.
pub fn explain(cfg: Option<&Config>, base: &Path, cwd: &str, target: &str, full: bool) -> Result<Option<Report>, String> {
    if cwd.trim().is_empty() {
        return Ok(None);
    }
    let cwd_path = resolve(Path::new(cwd));
    let cwd = cwd_path.to_string_lossy().into_owned();
    let route = collective::route(cfg, &cwd, base);
    let Route::Collective { name, home, .. } = &route else { return Ok(None) };
    let (path, lines) = parse_span(target)?;
    let file = resolve(&cwd_path.join(&path));
    let target_kind = if file.is_file() {
        // The file itself may lie under an off path or in another collective than the cwd.
        match collective::route(cfg, &file.to_string_lossy(), base) {
            Route::Off => return Ok(None),
            r if r != route => return Err(format!("{path} belongs to another collective than {cwd}; run borg why from its repo")),
            _ => Target::Span { path: path.clone(), lines },
        }
    } else if lines.is_some() {
        return Err(format!("{path}: no such file"));
    } else {
        Target::Commit(target.to_string())
    };
    let dir = match &target_kind {
        Target::Span { .. } => file.parent().map(Path::to_path_buf).unwrap_or_else(|| cwd_path.clone()),
        Target::Commit(_) => cwd_path.clone(),
    };
    let repo = git(&dir, &["rev-parse", "--show-toplevel"]).map(|s| PathBuf::from(s.trim())).map_err(|_| format!("{} is not in a git repo", dir.display()))?;
    // The repo itself must route where the cwd does: a symlinked directory or a submodule may put
    // the history git reads in an off or another collective's repo.
    let repo = resolve(&repo);
    if collective::route(cfg, &repo.to_string_lossy(), base) != route {
        return Ok(None);
    }
    let (mut shas, uncommitted) = match &target_kind {
        Target::Span { lines, .. } => {
            let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let mut args = vec!["blame".to_string(), "--porcelain".into()];
            if let Some((a, b)) = lines {
                args.push(format!("-L{a},{b}"));
            }
            args.extend(["--".into(), name]);
            let out = git(&dir, &args.iter().map(String::as_str).collect::<Vec<_>>()).map_err(|e| format!("git blame {path}: {e}"))?;
            blamed(&out)
        }
        Target::Commit(c) => {
            let sha = git(&repo, &["rev-parse", "--verify", "--quiet", &format!("{c}^{{commit}}")])
                .map_err(|_| format!("{c} is neither a file nor a commit in {}", repo.display()))?;
            (vec![(sha.trim().to_string(), 0)], 0)
        }
    };
    // Newest first by author time (from the blame headers), and the cap before any per-commit work.
    shas.sort_by_key(|(_, ts)| std::cmp::Reverse(*ts));
    let mut more = 0;
    if !full {
        more = shas.len().saturating_sub(MAX_COMMITS);
        shas.truncate(MAX_COMMITS);
    }
    let shas: Vec<String> = shas.into_iter().map(|(s, _)| s).collect();
    if shas.is_empty() {
        return Ok(Some(Report { target: target.into(), collective: name.clone(), commits: Vec::new(), more, uncommitted }));
    }
    // One `git log` for the shown commits' headers.
    let input: String = shas.iter().map(|s| format!("{s}\n")).collect();
    let log = git_in(&repo, &["log", "--no-walk=unsorted", "--stdin", "--date=short", "--format=%H%x1f%h%x1f%an%x1f%ad%x1f%s"], Some(input))
        .map_err(|e| format!("git log: {e}"))?;
    let mut info: HashMap<String, [String; 4]> = HashMap::new();
    for l in String::from_utf8_lossy(&log).lines() {
        let f: Vec<&str> = l.splitn(5, '\x1f').collect();
        if let [sha, short, author, date, subject] = f[..] {
            info.insert(sha.to_string(), [short.into(), author.into(), date.into(), subject.into()]);
        }
    }
    let mut from_notes = notes_of(&repo, name, &shas);
    let missing: HashSet<&str> = shas.iter().map(String::as_str).filter(|s| !from_notes.contains_key(*s)).collect();
    let mut from_store = stored_of(home, name, &missing);
    let lessons = learn::lessons(&home.join("lessons.jsonl"));
    let mut commits = Vec::new();
    for sha in &shas {
        let Some([short, author, date, subject]) = info.remove(sha) else { return Err(format!("git log {sha}: unexpected output")) };
        let reasoning = match from_notes.remove(sha) {
            Some(e) => Some(shown(e, "note")),
            None => from_store.remove(sha).map(|e| shown(e, "store")),
        };
        let lessons = reasoning.as_ref().map(|r| lessons_of(&lessons, &r.episodes)).unwrap_or_default();
        commits.push(Why { sha: sha.clone(), short, author, date, subject: redact(&subject), reasoning, lessons });
    }
    if !full {
        for w in &mut commits {
            w.lessons.truncate(MAX_LESSONS);
            for l in &mut w.lessons {
                l.question = first_chars(&one_line(&l.question), LESSON_CAP);
                l.consequence = first_chars(&one_line(&l.consequence), LESSON_CAP);
            }
            if let Some(r) = &mut w.reasoning {
                r.task = first_chars(r.task.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim(), TASK_CAP);
                r.proposal = last_chars(&one_line(&r.proposal), PROPOSAL_CAP);
            }
        }
    }
    Ok(Some(Report { target: target.into(), collective: name.clone(), commits, more, uncommitted }))
}

/// The report as text.
pub fn render(r: &Report) -> String {
    let mut s = String::new();
    for w in &r.commits {
        s.push_str(&format!("{} {} {} {}\n", w.short, w.date, w.author, w.subject));
        match &w.reasoning {
            None => s.push_str("  (no borg reasoning recorded)\n"),
            Some(x) => {
                let from = if x.source == "note" { String::new() } else { " (from the collective's store; `borg notes fetch` writes it as a note)".into() };
                s.push_str(&format!("  task: {}{from}\n", x.task.trim()));
                if !x.proposal.trim().is_empty() {
                    s.push_str(&format!("  proposal: {}\n", x.proposal.trim()));
                }
                if !x.episodes.is_empty() {
                    s.push_str(&format!("  episodes: {}\n", x.episodes.join(", ")));
                }
            }
        }
        for l in &w.lessons {
            s.push_str(&format!("  lesson {}: {} -> {}\n", l.id, l.question.trim(), l.consequence.trim()));
        }
    }
    if r.more > 0 {
        s.push_str(&format!("... {} older commit(s) not shown (--full)\n", r.more));
    }
    if r.uncommitted > 0 {
        s.push_str(&format!("{} line(s) not committed yet\n", r.uncommitted));
    }
    if r.commits.is_empty() {
        s.push_str(&format!("no commits behind {}\n", r.target));
    } else if r.commits.iter().all(|w| w.reasoning.is_none()) {
        s.push_str(&format!("no borg reasoning found for {} (in collective {}; `borg notes fetch` asks the mesh)\n", r.target, r.collective));
    }
    s
}

/// `borg why <target> [--full] [--json] [--dir DIR]`.
pub fn main(args: &[&str]) -> i32 {
    let (mut full, mut json, mut dir, mut target) = (false, false, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--full" => full = true,
            "--json" => json = true,
            "--dir" => match it.next() {
                Some(d) => dir = Some(d.to_string()),
                None => return usage(),
            },
            t if target.is_none() && !t.starts_with("--") => target = Some(t.to_string()),
            _ => return usage(),
        }
    }
    let Some(target) = target else { return usage() };
    let cwd = dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_default().display().to_string());
    // An invalid config routes everything off: nothing is shown.
    let Ok(cfg) = collective::load() else { return 0 };
    match explain(cfg.as_ref(), &learn::home(), &cwd, &target, full) {
        Ok(None) => 0,
        Ok(Some(r)) => {
            if json {
                println!("{}", serde_json::to_string(&r).unwrap());
            } else {
                print!("{}", render(&r));
            }
            0
        }
        Err(e) => {
            eprintln!("borg why: {e}");
            1
        }
    }
}

fn usage() -> i32 {
    eprintln!("{USAGE}");
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// git as `sh`, as `who`, at a fixed time.
    fn commit_as(dir: &Path, who: &str, secs: i64, msg: &str) -> String {
        let date = format!("@{secs} +0000");
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", &format!("user.name={who}"), "-c", &format!("user.email={who}@example.com"), "commit", "-q", "-am", msg])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_AUTHOR_DATE", &date)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        sh(dir, &["rev-parse", "HEAD"])
    }

    struct Fx {
        base: PathBuf,
        work: PathBuf,
        repo: PathBuf,
        store: PathBuf,
        cfg: Config,
        one: String,
        two: String,
    }

    /// `<base>/work/app` (collective "corp", team) with `src/lib.rs`: lines 1-3 by Ann (commit
    /// `one`), line 2 rewritten by Bob (commit `two`). Also `<base>/private` (local) and
    /// `<base>/work/secret` (off).
    fn fixture(tag: &str) -> Fx {
        let base = std::env::temp_dir().join(format!("borg-why-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // Resolved, as `explain` routes it (the temp dir may be a symlink).
        let base = std::fs::canonicalize(&base).unwrap();
        let (work, store_base) = (base.join("work"), base.join("store"));
        let repo = work.join("app");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        sh(&repo, &["init", "-q"]);
        std::fs::write(repo.join("src/lib.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
        sh(&repo, &["add", "."]);
        let one = commit_as(&repo, "Ann", 1_700_000_000, "add a b c");
        std::fs::write(repo.join("src/lib.rs"), "fn a() {}\nfn b() { lock(); }\nfn c() {}\n").unwrap();
        let two = commit_as(&repo, "Bob", 1_700_100_000, "take the lock in b");
        let cfg: Config = serde_json::from_str(&format!(
            r#"{{"default": "off", "off": ["{0}/work/secret"], "collectives": [
                {{"name": "corp", "paths": ["{0}/work"], "share": "team"}},
                {{"name": "private", "paths": ["{0}/private"], "share": "local"}} ]}}"#,
            base.display()
        ))
        .unwrap();
        let store = collective::store(&store_base, "corp");
        Fx { base, work, repo, store, cfg, one, two }
    }

    impl Fx {
        fn why(&self, cwd: &Path, target: &str, full: bool) -> Result<Option<Report>, String> {
            explain(Some(&self.cfg), &self.base.join("store"), &cwd.to_string_lossy(), target, full)
        }
        fn note(&self, repo: &Path, commit: &str, collective: &str, episodes: &[&str]) {
            let e = Excerpt {
                kind: "reasoning".into(),
                commit: commit.into(),
                repo: "app".into(),
                session: "s-1".into(),
                task: "make b safe under concurrency\nsecond line".into(),
                proposal: "Take the lock first; the token is ghp_abcdefghijklmnopqrstuvwxyz0123456789.".into(),
                episodes: episodes.iter().map(|s| s.to_string()).collect(),
                ms: 1,
                collective: collective.into(),
            };
            notes::write_note(repo, &e).unwrap();
        }
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn parses_line_ranges() {
        assert_eq!(parse_span("src/a.rs").unwrap(), ("src/a.rs".into(), None));
        assert_eq!(parse_span("src/a.rs:12").unwrap(), ("src/a.rs".into(), Some((12, 12))));
        assert_eq!(parse_span("src/a.rs:12-20").unwrap(), ("src/a.rs".into(), Some((12, 20))));
        assert_eq!(parse_span("HEAD~1").unwrap(), ("HEAD~1".into(), None));
        assert_eq!(parse_span("HEAD:src/a.rs").unwrap(), ("HEAD:src/a.rs".into(), None), "not a range");
        assert!(parse_span("a.rs:0").is_err());
        assert!(parse_span("a.rs:20-10").is_err());
        assert!(parse_span("a.rs:1-").is_err());
        assert!(parse_span("a.rs:1-2-3").is_err());
        assert!(parse_span("a.rs:99999999999").is_err());
    }

    #[test]
    fn blame_of_a_span_spanning_two_commits() {
        let fx = fixture("span");
        fx.note(&fx.repo, &fx.two, "corp", &[]);
        let r = fx.why(&fx.repo, "src/lib.rs:1-2", false).unwrap().unwrap();
        let shas: Vec<&str> = r.commits.iter().map(|w| w.sha.as_str()).collect();
        assert_eq!(shas, [fx.two.as_str(), fx.one.as_str()], "both commits, newest first");
        assert_eq!((r.commits[0].author.as_str(), r.commits[0].subject.as_str()), ("Bob", "take the lock in b"));
        let x = r.commits[0].reasoning.as_ref().unwrap();
        assert_eq!((x.source.as_str(), x.task.as_str()), ("note", "make b safe under concurrency"), "compact: the task's first line");
        assert!(!x.proposal.contains("ghp_abc") && x.proposal.contains("[REDACTED]"), "redacted again at display: {}", x.proposal);
        assert!(r.commits[1].reasoning.is_none());
        let text = render(&r);
        assert!(text.contains("take the lock in b") && text.contains("add a b c") && text.contains("(no borg reasoning recorded)"), "{text}");
        // One line only: one commit.
        let r = fx.why(&fx.repo, "src/lib.rs:3", false).unwrap().unwrap();
        assert_eq!(r.commits.len(), 1);
        assert_eq!(r.commits[0].sha, fx.one);
        // Uncommitted lines are counted, not blamed on a commit.
        std::fs::write(fx.repo.join("src/lib.rs"), "fn a() {}\nfn b() { lock(); }\nfn c() {}\nfn d() {}\n").unwrap();
        let r = fx.why(&fx.repo, "src/lib.rs:3-4", false).unwrap().unwrap();
        assert_eq!((r.commits.len(), r.uncommitted), (1, 1));
        assert!(fx.why(&fx.repo, "src/lib.rs:9-12", false).is_err(), "past the end of the file");
    }

    #[test]
    fn commit_form_and_nothing_found() {
        let fx = fixture("commit");
        let r = fx.why(&fx.repo, &fx.one[..10], false).unwrap().unwrap();
        assert_eq!(r.commits.len(), 1);
        assert_eq!(r.commits[0].sha, fx.one);
        assert!(render(&r).contains("no borg reasoning found for"), "{}", render(&r));
        let r = fx.why(&fx.repo, "HEAD", true).unwrap().unwrap();
        assert_eq!(r.commits[0].sha, fx.two);
        let err = fx.why(&fx.repo, "no-such-thing", false).unwrap_err();
        assert!(err.contains("neither a file nor a commit"), "{err}");
        assert!(fx.why(&fx.repo, "missing.rs:3", false).unwrap_err().contains("no such file"));
    }

    #[test]
    fn falls_back_to_replicated_lines_and_links_lessons_by_episode() {
        let fx = fixture("replica");
        std::fs::create_dir_all(&fx.store).unwrap();
        let line = |commit: &str, collective: &str, task: &str| {
            serde_json::json!({"kind": "reasoning", "commit": commit, "repo": "app", "session": "s-9", "task": task,
                "proposal": "lock before the flag; password=hunter22", "episodes": ["s-9-100"], "ms": 5, "collective": collective})
            .to_string()
        };
        let lesson = |id: &str, ep: &str, q: &str| {
            serde_json::json!({"id": id, "origin_ms": 1, "repo": "app", "question": q, "situations": [], "consequence": "a race corrupts sessions",
                "support": 1, "episodes": [ep], "history": [], "version": 1})
            .to_string()
        };
        std::fs::write(
            fx.store.join("reasoning.jsonl"),
            [line(&fx.two, "corp", "replicated why"), line(&fx.one, "other", "another collective's")].join("\n") + "\n",
        )
        .unwrap();
        std::fs::write(
            fx.store.join("lessons.jsonl"),
            [lesson("L1", "s-9-100", "Is the lock taken before the flag is read?"), lesson("L2", "s-0-1", "unrelated")].join("\n") + "\n",
        )
        .unwrap();
        let r = fx.why(&fx.repo, "src/lib.rs", false).unwrap().unwrap();
        let x = r.commits[0].reasoning.as_ref().unwrap();
        assert_eq!((x.source.as_str(), x.task.as_str()), ("store", "replicated why"));
        assert!(!x.proposal.contains("hunter22"));
        assert_eq!(r.commits[0].lessons.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), ["L1"], "linked by episode id");
        assert!(r.commits[1].reasoning.is_none(), "an excerpt tagged with another collective is not shown");
        let text = render(&r);
        assert!(text.contains("lesson L1: Is the lock taken before the flag is read? -> a race corrupts sessions"), "{text}");
        assert!(text.contains("borg notes fetch"), "{text}");
        // A local note wins over the store.
        fx.note(&fx.repo, &fx.two, "corp", &["s-9-100"]);
        let r = fx.why(&fx.repo, &fx.two, false).unwrap().unwrap();
        assert_eq!(r.commits[0].reasoning.as_ref().unwrap().source, "note");
        assert_eq!(r.commits[0].lessons.len(), 1);
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["commits"][0]["lessons"][0]["id"], "L1");
        assert!(json["commits"][0].get("ts").is_none());
    }

    #[test]
    fn private_notes_stay_in_their_collective_and_off_prints_nothing() {
        let fx = fixture("private");
        // The same repo history moved under the private path: its note is tagged "private".
        let private = fx.base.join("private/app");
        std::fs::create_dir_all(private.parent().unwrap()).unwrap();
        sh(&fx.base, &["clone", "-q", &fx.repo.to_string_lossy(), &private.to_string_lossy()]);
        fx.note(&private, &fx.two, "private", &[]);
        let r = fx.why(&private, "src/lib.rs:2", false).unwrap().unwrap();
        assert_eq!(r.collective, "private");
        assert!(r.commits[0].reasoning.is_some(), "shown where the cwd routes to the same private collective");
        // A private note that ends up in a team repo (copied refs) is not shown there.
        fx.note(&fx.repo, &fx.two, "private", &[]);
        let r = fx.why(&fx.repo, "src/lib.rs:2", false).unwrap().unwrap();
        assert!(r.commits[0].reasoning.is_none(), "a private note never shows in another collective");
        // Off: nothing at all, before git is asked (also for a target inside an off path).
        let secret = fx.work.join("secret");
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::write(secret.join("x.rs"), "x").unwrap();
        assert_eq!(fx.why(&secret, "x.rs", false).unwrap(), None);
        assert_eq!(fx.why(&fx.repo, "../secret/x.rs", false).unwrap(), None, "a target under an off path");
        assert_eq!(fx.why(&fx.base.join("elsewhere"), "HEAD", false).unwrap(), None, "default off");
        // A file in another collective than the cwd is refused.
        assert!(fx.why(&fx.repo, "../../private/app/src/lib.rs", false).unwrap_err().contains("another collective"));
    }

    #[test]
    fn routes_resolved_paths_and_the_repo_itself() {
        let fx = fixture("resolve");
        fx.note(&fx.repo, &fx.two, "corp", &[]);
        // An off repo with a note, reached through symlinks from inside the team tree.
        let secret = fx.work.join("secret");
        std::fs::create_dir_all(&secret).unwrap();
        sh(&fx.base, &["clone", "-q", &fx.repo.to_string_lossy(), &secret.to_string_lossy()]);
        sh(&fx.repo, &["push", "-q", &secret.to_string_lossy(), "refs/notes/borg:refs/notes/borg"]);
        std::os::unix::fs::symlink(&secret, fx.repo.join("link")).unwrap();
        std::os::unix::fs::symlink(&secret, fx.work.join("alias")).unwrap();
        assert_eq!(fx.why(&fx.repo, "link/src/lib.rs", false).unwrap(), None, "a symlinked file in an off repo");
        assert_eq!(fx.why(&fx.repo.join("link"), "src/lib.rs:2", false).unwrap(), None, "a symlinked cwd");
        assert_eq!(fx.why(&fx.work.join("alias"), "HEAD", false).unwrap(), None, "a symlinked cwd, commit form");
        assert_eq!(fx.why(&fx.repo.join("src/../link"), "HEAD", false).unwrap(), None);
        // The cwd routes to a collective, but the repo git reads (its toplevel) routes elsewhere.
        let mut cfg = fx.cfg.clone();
        cfg.collectives[1].paths.push(fx.repo.join("src").to_string_lossy().into_owned());
        let r = explain(Some(&cfg), &fx.base.join("store"), &fx.repo.join("src").to_string_lossy(), "lib.rs:2", false).unwrap();
        assert_eq!(r, None, "a private subdirectory of a team repo");
        let r = explain(Some(&cfg), &fx.base.join("store"), &fx.repo.join("src").to_string_lossy(), "HEAD", false).unwrap();
        assert_eq!(r, None);
        // The real paths still work.
        assert!(fx.why(&fx.repo, "src/lib.rs:2", false).unwrap().unwrap().commits[0].reasoning.is_some());
    }

    #[test]
    fn blame_headers_give_the_order_and_the_cap_comes_first() {
        let porcelain = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 1 1 1
author A
author-time 100
summary old
\tx
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb 2 2 1
author B
author-time 300
summary new
\ty
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 3 3 1
\tz
0000000000000000000000000000000000000000 4 4 2
author Not Committed Yet
author-time 999
\tw
";
        let (c, u) = blamed(porcelain);
        assert_eq!(c, [("a".repeat(40), 100), ("b".repeat(40), 300)]);
        assert_eq!(u, 2);

        // Twelve commits, each owning one line: compact keeps the 8 newest, the store is read once
        // for those, and an excerpt of a cut commit costs nothing.
        let fx = fixture("cap");
        let file = fx.repo.join("src/lib.rs");
        let mut text = std::fs::read_to_string(&file).unwrap();
        let mut made = Vec::new();
        for i in 0..12 {
            text.push_str(&format!("fn f{i}() {{}}\n"));
            std::fs::write(&file, &text).unwrap();
            made.push(commit_as(&fx.repo, "Cy", 1_800_000_000 + i, &format!("f{i}")));
        }
        std::fs::create_dir_all(&fx.store).unwrap();
        let line = |c: &str, ms: i64, task: &str| {
            serde_json::json!({"kind": "reasoning", "commit": c, "repo": "app", "session": "s", "task": task, "proposal": "", "episodes": [], "ms": ms})
                .to_string()
        };
        let lines = [line(&made[11], 1, "older"), line(&made[11], 2, "newest"), line(&made[0], 1, "cut")];
        std::fs::write(fx.store.join("reasoning.jsonl"), lines.join("\n") + "\n").unwrap();
        fx.note(&fx.repo, &made[10], "corp", &[]);
        let r = fx.why(&fx.repo, "src/lib.rs", false).unwrap().unwrap();
        let subjects: Vec<&str> = r.commits.iter().map(|w| w.subject.as_str()).collect();
        assert_eq!(subjects, ["f11", "f10", "f9", "f8", "f7", "f6", "f5", "f4"]);
        assert_eq!(r.more, 6, "f3..f0, two and one");
        assert_eq!(r.commits[0].reasoning.as_ref().map(|x| (x.source.as_str(), x.task.as_str())), Some(("store", "newest")));
        assert_eq!(r.commits[1].reasoning.as_ref().map(|x| x.source.as_str()), Some("note"));
        assert_eq!((r.commits[0].author.as_str(), r.commits[0].short.len() >= 7), ("Cy", true));
        let full = fx.why(&fx.repo, "src/lib.rs", true).unwrap().unwrap();
        assert_eq!((full.commits.len(), full.more), (14, 0));
        assert_eq!(full.commits[11].reasoning.as_ref().map(|x| x.task.as_str()), Some("cut"));
        assert_eq!(full.commits[12].sha, fx.two);
    }

    #[test]
    fn a_dir_without_git_is_a_clear_error() {
        let fx = fixture("nogit");
        let plain = fx.work.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("a.txt"), "x\n").unwrap();
        // GIT_CEILING_DIRECTORIES is not needed: /tmp/... is not inside a repo.
        let err = fx.why(&plain, "a.txt:1", false).unwrap_err();
        assert!(err.contains("is not in a git repo"), "{err}");
        let err = fx.why(&plain, "HEAD", false).unwrap_err();
        assert!(err.contains("is not in a git repo"), "{err}");
    }

    #[test]
    fn compact_caps_and_full() {
        let fx = fixture("caps");
        let long = "x".repeat(2000);
        let e = Excerpt {
            kind: "reasoning".into(),
            commit: fx.two.clone(),
            repo: "app".into(),
            session: "s".into(),
            task: "t".into(),
            proposal: format!("start {long} end"),
            episodes: vec![],
            ms: 1,
            collective: String::new(),
        };
        notes::write_note(&fx.repo, &e).unwrap();
        let compact = fx.why(&fx.repo, "HEAD", false).unwrap().unwrap();
        let p = &compact.commits[0].reasoning.as_ref().unwrap().proposal;
        assert!(p.chars().count() <= PROPOSAL_CAP + 1 && p.ends_with("end"), "the proposal's conclusion is kept");
        let full = fx.why(&fx.repo, "HEAD", true).unwrap().unwrap();
        assert!(full.commits[0].reasoning.as_ref().unwrap().proposal.starts_with("start"), "an untagged (older) note shows in the repo's collective");
    }
}
