//! Reasoning travels with the code: a redacted excerpt per landed commit, as a git note.
//!
//! `borg notes attach [<commit>]` writes, for each commit a drone's session landed (or the one
//! named), the session's task, the drone's last proposal and the session's episode ids to
//! `refs/notes/borg` in that repo. Notes are local refs: borg never pushes them (an engineer who
//! wants them on a remote pushes `refs/notes/borg` like any ref). The hook's `end` event runs it in
//! the background when the session started in a git repo (`<state>/<session>.head` holds the repo
//! and its HEAD at the session's first prompt).
//!
//! **Only the session's own commits** (`landed`): a commit gets this session's note only if HEAD's
//! reflog says `commit` (or `commit (amend)`, `commit (initial)`, `revert`, `cherry-pick`) created it while the session was
//! working (between one of its prompts and the stop that followed, `<state>/<session>.active`) and
//! it is still on HEAD. A `git pull`, a merge, a checkout of another branch, or a commit made while
//! this session was idle (the engineer, or another session in the same repo) gets nothing from it.
//! Two sessions working in the same worktree at the same second cannot be told apart.
//!
//! **Redaction first** (`redact`): tokens, keys, passwords and `.env`-style values are replaced
//! before an excerpt is written anywhere, by a conservative regex list (it rather blanks a harmless
//! value than lets a secret through).
//!
//! **Replication** (PLAN.md "Availability"): the same excerpt is appended to the collective's
//! `reasoning.jsonl`, which the mesh (borg-mesh, sync.rs) hands to two member replicas, so the "why"
//! survives the author being offline. Only when `collective::may_sync` allows it: a local (private)
//! collective's excerpts stay in that repo's notes, and an off repo gets nothing at all.
//! `borg notes fetch [<commit>...]` reads it back: it asks the known members (borg-mesh `fetch`,
//! author or replica) and writes what they hold as notes (restore.rs).
//!
//! **Links** (regress.rs): after `attach`, the landed commits are matched against the earlier
//! decisions whose lines they changed; a note then lists them under `borg fixed in:` (on the
//! decision) and `borg fixes:` (on the fix), links.rs.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{collective, learn};

pub mod links;
pub mod reasoning;
pub mod regress;
pub mod regress_cli;
pub mod regress_cosmetic;
pub mod regress_diff;
pub mod regress_git;
pub mod regress_learn;
pub mod regress_signals;
pub mod regret;
pub mod restore;

use links::LinkRef;
pub use restore::{restore, wanted};

pub const NOTES_REF: &str = "refs/notes/borg";
/// At most this many commits get a note per run (a session that landed more is unusual; the
/// newest are the ones whose reasoning is still in the session).
const MAX_COMMITS: usize = 20;

const REDACTED: &str = "[REDACTED]";

/// (pattern, replacement): applied in order.
static SECRETS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        // PEM private keys, also when cut off before their END line.
        (r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----(?s:.*?)(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)", "[REDACTED PRIVATE KEY]"),
        // ... and when the text starts inside one (a window cut before its BEGIN line): an END
        // left over blanks everything before it.
        (r"(?s)\A.*?-----END [A-Z0-9 ]*PRIVATE KEY-----", "[REDACTED PRIVATE KEY]"),
        // Provider tokens with a recognisable shape.
        (r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", REDACTED),
        (r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}|\bgithub_pat_[A-Za-z0-9_]{20,}", REDACTED),
        (r"\bglpat-[A-Za-z0-9_-]{20,}", REDACTED),
        (r"\bxox[abposr]-[A-Za-z0-9-]{10,}", REDACTED),
        (r"\bsk-(?:ant-|proj-)?[A-Za-z0-9_-]{20,}", REDACTED),
        (r"\bAIza[0-9A-Za-z_-]{35}", REDACTED),
        (r"\bnpm_[A-Za-z0-9]{36}\b", REDACTED),
        (r"\bhf_[A-Za-z0-9]{30,}", REDACTED),
        (r"\b(?:sk|rk|pk)_(?:live|test)_[A-Za-z0-9]{16,}", REDACTED),
        (r"\bya29\.[A-Za-z0-9_-]{20,}", REDACTED),
        (r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}", REDACTED),
        // Credentials in URLs and auth headers.
        (r"(?i)\b([a-z][a-z0-9+.-]*://[^\s:/@]+):[^\s@/]+@", "$1:[REDACTED]@"),
        (r"(?i)\b(bearer|basic|token)(\s+)[A-Za-z0-9._~+/=-]{12,}", "$1$2[REDACTED]"),
        // A password glued to its flag on a database client's command line (`mysql -pS3cret`).
        (r"(?i)\b((?:mysql|mysqldump|mysqladmin|mariadb|psql)\b[^\n]*?\s-p)[^\s-]\S*", "${1}[REDACTED]"),
        // key = value / "key": "value" where the key names a secret: also `pass`, `pw` and any name
        // ending in `key` (`signing_key`, `secretKey`), whatever the value looks like (lowercase hex too).
        (
            r#"(?i)\b([A-Za-z0-9_.-]*?(?:password|passwd|passphrase|pass|pwd|pw|secret|token|api[_-]?key|apikey|access[_-]?key|private[_-]?key|key|credentials?|authorization|auth)(?:[_.-][A-Za-z0-9_.-]*|(?-i:[A-Z])[A-Za-z0-9_.-]*)?)(["']?\s*[:=]\s*)("[^"\n]*"|'[^'\n]*'|[^\s,;}]+)"#,
            "$1$2[REDACTED]",
        ),
        // .env lines: any upper-case name assigned at the start of a line.
        (r"(?m)^(\s*(?:export\s+)?[A-Z][A-Z0-9_]*\s*=\s*)\S.*$", "$1[REDACTED]"),
    ]
    .into_iter()
    .map(|(p, r)| (Regex::new(p).expect("secret pattern"), r))
    .collect()
});

/// Candidates for an unlabelled random token (checked by `looks_random`).
static TOKENISH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/_=-]{32,}").unwrap());

/// A long run of mixed case and digits that is not a path or an identifier: some `/`-free part of
/// at least 24 characters with upper, lower and at least two digits; or, since base64 secrets
/// contain `/` too, a whole run of 40 or more with upper, lower, a digit and at least 30 % of its
/// letters upper case (paths and CamelCase names have far fewer). Lowercase hex (commit ids,
/// hashes) and UUIDs are kept: they identify, they do not authorise.
fn looks_random(s: &str) -> bool {
    let mixed = |p: &str, digits: usize| {
        p.chars().any(|c| c.is_ascii_uppercase()) && p.chars().any(|c| c.is_ascii_lowercase()) && p.chars().filter(|c| c.is_ascii_digit()).count() >= digits
    };
    let upper = s.chars().filter(|c| c.is_ascii_uppercase()).count();
    let letters = s.chars().filter(|c| c.is_ascii_alphabetic()).count();
    (s.len() >= 40 && mixed(s, 1) && upper * 10 >= letters * 3) || s.split('/').any(|p| p.len() >= 24 && mixed(p, 2))
}

/// `text` with every secret the list recognises replaced (module doc).
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for (re, with) in SECRETS.iter() {
        out = re.replace_all(&out, *with).into_owned();
    }
    TOKENISH.replace_all(&out, |c: &Captures| if looks_random(&c[0]) { REDACTED.to_string() } else { c[0].to_string() }).into_owned()
}

/// The per-session scratch directory (as the hook uses it).
pub fn state_dir() -> PathBuf {
    PathBuf::from(std::env::var("BORG_STATE").unwrap_or_else(|_| "/tmp/borg".into()))
}

/// `session` as a file name in the state dir.
pub(crate) fn safe(session: &str) -> String {
    session.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect()
}

fn state(dir: &Path, session: &str, ext: &str) -> PathBuf {
    dir.join(format!("{}.{ext}", safe(session)))
}

pub(crate) fn git(repo: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Where a session started: the repo's top level and its HEAD then.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Start {
    pub repo: String,
    pub head: String,
    pub ms: i64,
}

/// Records where `session` started, once (hook, first prompt in a git repo).
pub fn remember_start(dir: &Path, session: &str, cwd: &str) {
    let p = state(dir, session, "head");
    if cwd.is_empty() || p.exists() {
        return;
    }
    let start = git(Path::new(cwd), &["rev-parse", "--show-toplevel"])
        .and_then(|repo| Some(Start { head: git(Path::new(&repo), &["rev-parse", "HEAD"])?, repo, ms: learn::now_ms() }));
    let _ = std::fs::create_dir_all(dir);
    // Outside a git repo (or before its first commit) a `null` marker keeps later prompts from
    // asking git again; `end` then has nothing to do.
    let _ = std::fs::write(p, serde_json::to_string(&start).unwrap());
}

/// Marks the session working (`prompt`) or idle again (`stop`), for `landed` (hook).
pub fn mark(dir: &Path, session: &str, what: &str) {
    use std::io::Write;
    let _ = std::fs::create_dir_all(dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(state(dir, session, "active")) {
        let _ = writeln!(f, "{what} {}", learn::now_ms());
    }
}

/// When the session was working, as `[from, to]` epoch ms: from each prompt to the stop after it
/// (open-ended while it works). Without marks, everything since the session started.
pub fn windows(dir: &Path, session: &str, start: &Start) -> Vec<(i64, i64)> {
    let text = std::fs::read_to_string(state(dir, session, "active")).unwrap_or_default();
    let mut out = Vec::new();
    let mut open: Option<i64> = None;
    for (what, ms) in text.lines().filter_map(|l| l.split_once(' ')).filter_map(|(w, m)| Some((w, m.trim().parse::<i64>().ok()?))) {
        match what {
            "prompt" => {
                open.get_or_insert(ms);
            }
            "stop" => {
                if let Some(from) = open.take() {
                    out.push((from, ms));
                }
            }
            _ => {}
        }
    }
    if let Some(from) = open {
        out.push((from, i64::MAX));
    }
    if out.is_empty() {
        out.push((start.ms, i64::MAX));
    }
    out
}

/// Keeps the drone's latest proposal for the excerpt (hook, at stop and end): redacted whole, then
/// cut to its last 2000 characters, so a cut never lands inside a secret that was not redacted.
pub fn remember_proposal(dir: &Path, session: &str, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    let text = redact(text);
    let tail: String = text.chars().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect();
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(state(dir, session, "proposal"), tail);
}

/// The redacted reasoning of one landed commit.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Excerpt {
    pub kind: String,
    pub commit: String,
    pub repo: String,
    pub session: String,
    pub task: String,
    pub proposal: String,
    pub episodes: Vec<String>,
    pub ms: i64,
    /// The collective the session worked in (`borg why` shows a note only inside it); empty in
    /// excerpts from before it was recorded.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub collective: String,
    /// Later commits that fixed (or revisited) this commit's lines (regress.rs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixed_by: Vec<LinkRef>,
    /// The earlier decisions this commit fixed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixes: Vec<LinkRef>,
}

impl Excerpt {
    /// What `git notes show` prints.
    pub fn note(&self) -> String {
        let within = if self.collective.is_empty() { String::new() } else { format!(", collective {}", self.collective) };
        let mut s = format!("borg reasoning (session {}{within})\n\nTask:\n{}\n\nLast proposal:\n{}\n", self.session, self.task.trim(), self.proposal.trim());
        if !self.episodes.is_empty() {
            s.push_str(&format!("\nCorrections (episodes): {}\n", self.episodes.join(", ")));
        }
        s.push_str(&links::sections(&self.fixed_by, &self.fixes));
        s
    }

    /// The excerpt a borg note holds (the inverse of `note`), for `commit`; `None` if the text is
    /// not a borg reasoning note.
    pub fn from_note(commit: &str, text: &str) -> Option<Excerpt> {
        let (text, fixed_by, fixes) = links::split(text);
        let (head, rest) = text.split_once('\n')?;
        let inner = head.trim().strip_prefix("borg reasoning (session ")?.strip_suffix(')')?;
        let (session, collective) = inner.split_once(", collective ").unwrap_or((inner, ""));
        // git drops the blank lines an empty task or proposal leaves, and `git` trims the end.
        let rest = rest.trim_start().strip_prefix("Task:")?;
        let (task, rest) = rest.split_once("\nLast proposal:").map_or((rest, ""), |(t, p)| (t, p.strip_prefix('\n').unwrap_or(p)));
        let (proposal, episodes) = match rest.rsplit_once("\nCorrections (episodes): ") {
            Some((p, e)) => (p, e.trim().split(", ").filter(|e| !e.is_empty()).map(str::to_string).collect()),
            None => (rest, Vec::new()),
        };
        Some(Excerpt {
            kind: "reasoning".into(),
            commit: commit.into(),
            repo: String::new(),
            session: session.into(),
            task: task.trim().into(),
            proposal: proposal.trim().into(),
            episodes,
            ms: 0,
            collective: collective.into(),
            fixed_by,
            fixes,
        })
    }
}

/// The session's task, last proposal and episode ids, redacted, for `commit` of `repo`.
pub fn excerpt(dir: &Path, session: &str, repo: &str, commit: &str, now: i64) -> Excerpt {
    let read = |ext| std::fs::read_to_string(state(dir, session, ext)).unwrap_or_default();
    let episodes = read("ctx").lines().filter_map(|l| l.strip_prefix("#borg-episode ")).map(|s| s.trim().to_string()).collect();
    Excerpt {
        kind: "reasoning".into(),
        commit: commit.into(),
        repo: repo.into(),
        session: session.into(),
        task: redact(&read("task").chars().take(3000).collect::<String>()),
        proposal: redact(&read("proposal")),
        episodes,
        ms: now,
        ..Excerpt::default()
    }
}

/// The commits the session itself made (module doc), newest first, at most `MAX_COMMITS`: HEAD's
/// reflog entries of a `commit` within one of `windows`, still reachable from HEAD but not from
/// the session's starting HEAD.
pub fn landed(repo: &Path, start: &Start, windows: &[(i64, i64)]) -> Vec<String> {
    let on_head: std::collections::HashSet<String> =
        git(repo, &["rev-list", "--max-count=1000", &format!("{}..HEAD", start.head)]).map(|t| t.lines().map(str::to_string).collect()).unwrap_or_default();
    // `HEAD@{<unix seconds>}` with --date=unix; the reflog keeps seconds, the marks milliseconds.
    let within = |secs: i64| windows.iter().any(|&(from, to)| secs >= from.div_euclid(1000) && secs <= to.saturating_add(999) / 1000);
    let reflog = git(repo, &["log", "-g", "-n", "1000", "--date=unix", "--format=%H%x09%gd%x09%gs", "HEAD"]).unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    for l in reflog.lines() {
        let mut f = l.splitn(3, '\t');
        let (Some(c), Some(at), Some(subject)) = (f.next(), f.next(), f.next()) else { continue };
        let own = ["commit:", "commit (amend):", "commit (initial):", "revert:", "cherry-pick:"].iter().any(|p| subject.starts_with(p));
        let secs = at.rsplit_once('{').and_then(|(_, t)| t.trim_end_matches('}').parse::<i64>().ok());
        if own && secs.is_some_and(within) && on_head.contains(c) && !out.iter().any(|o| o == c) {
            out.push(c.to_string());
        }
        if out.len() >= MAX_COMMITS {
            break;
        }
    }
    out
}

/// The commits `session` landed so far (`landed`, from its start recorded in `dir`); empty when it
/// started outside git or left no record here.
pub fn landed_by(dir: &Path, session: &str) -> Vec<String> {
    start_of(dir, session).map(|s| landed(Path::new(&s.repo), &s, &windows(dir, session, &s))).unwrap_or_default()
}

/// Where `session` started (`remember_start`, recorded in `dir`); None outside git or without a record.
pub fn start_of(dir: &Path, session: &str) -> Option<Start> {
    std::fs::read_to_string(state(dir, session, "head")).ok().and_then(|t| serde_json::from_str::<Start>(&t).ok())
}

/// The top level of the git repo `dir` is in.
pub fn toplevel(dir: &str) -> Option<PathBuf> {
    git(Path::new(dir), &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

/// The borg note of `commit`, parsed; `None` without one.
#[must_use]
pub fn note_of(repo: &Path, commit: &str) -> Option<Excerpt> {
    Excerpt::from_note(commit, &git(repo, &["notes", &format!("--ref={NOTES_REF}"), "show", commit])?)
}

pub(crate) fn has_note(repo: &Path, commit: &str) -> bool {
    git(repo, &["notes", &format!("--ref={NOTES_REF}"), "list", commit]).is_some()
}

/// Writes `e` as the borg note of its commit (replacing an older borg note). Never pushes.
pub fn write_note(repo: &Path, e: &Excerpt) -> Result<(), String> {
    use std::io::Write;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["notes", &format!("--ref={NOTES_REF}"), "add", "-f", "-F", "-", &e.commit])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("git notes: {err}"))?;
    child.stdin.take().ok_or("git notes: no stdin")?.write_all(e.note().as_bytes()).map_err(|err| format!("git notes: {err}"))?;
    let out = child.wait_with_output().map_err(|err| format!("git notes: {err}"))?;
    if !out.status.success() {
        return Err(format!("git notes add {}: {}", e.commit, String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// What one run did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Attached {
    pub notes: Vec<String>,
    /// Excerpts queued for the mesh's replicas (0 for a local collective).
    pub replicated: usize,
}

/// Notes for `l.commits` from `l.session` in the collective `l.route` names (an off route attaches
/// nothing); excerpts go to `l.store`'s reasoning.jsonl only if the route may sync. `force`
/// replaces an existing borg note (an explicitly named commit), keeping the links it lists.
///
/// # Errors
/// A note git would not write, or a store that cannot be appended to.
pub fn attach(l: &Landed, force: bool) -> Result<Attached, String> {
    let collective::Route::Collective { name: within, .. } = l.route else { return Ok(Attached::default()) };
    let may_sync = collective::may_sync(l.route);
    let name = l.repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut done = Attached::default();
    for c in l.commits {
        if !force && has_note(l.repo, c) {
            continue;
        }
        let mut e = Excerpt { collective: within.clone(), ..excerpt(l.dir, l.session, &name, c, learn::now_ms()) };
        if let Some(old) = note_of(l.repo, c).filter(|_| force) {
            (e.fixed_by, e.fixes) = (old.fixed_by, old.fixes);
        }
        write_note(l.repo, &e)?;
        if may_sync {
            append_reasoning(l.store, &e).map_err(|err| format!("{}: {err}", l.store.display()))?;
            done.replicated += 1;
        }
        done.notes.push(c.clone());
    }
    Ok(done)
}

pub(crate) fn append_reasoning(store: &Path, e: &Excerpt) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(store)?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(store.join("reasoning.jsonl"))?;
    writeln!(f, "{}", serde_json::to_string(e).unwrap())
}

/// The session that last started in `repo`, from the `.head` files in `dir`.
#[must_use]
pub fn latest_session(dir: &Path, repo: &str) -> Option<(String, Start)> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|f| {
            let p = f.path();
            let session = p.file_name()?.to_str()?.strip_suffix(".head")?.to_string();
            let start: Start = serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
            (start.repo == repo).then_some((session, start))
        })
        .max_by_key(|(_, s)| s.ms)
}

/// Hook, `end`: if the session started in a git repo, attach notes in the background (never delays
/// the drone; any failure is silent), and `borg learn --session` after them (`--then-learn`), so
/// learning sees the regret episodes the links made. False when no job was started: the caller
/// then starts `borg learn` itself.
#[must_use]
pub fn on_end(session: &str, input: &Value) -> bool {
    let dir = state_dir();
    let started = std::fs::read_to_string(state(&dir, session, "head")).ok().and_then(|t| serde_json::from_str::<Start>(&t).ok());
    if started.is_none() {
        return false;
    }
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
    let (Ok(exe), Some(store)) = (std::env::current_exe(), std::env::var_os("BORG_HOME")) else { return false };
    Command::new(exe)
        .args(["notes", "attach", "--session", session, "--dir", cwd, "--then-learn", "--store"])
        .arg(store)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

/// `--then-learn`: `borg learn [--session ID]` in the background once attach is done, whatever it
/// did, on the store the run used (`--store`, else the caller's `BORG_HOME`).
fn then_learn(args: &[&str]) {
    let flag = |f: &str| args.iter().position(|a| *a == f).and_then(|i| args.get(i + 1)).copied();
    let Ok(exe) = std::env::current_exe() else { return };
    let mut c = Command::new(exe);
    c.arg("learn").args(flag("--session").map(|s| ["--session", s]).into_iter().flatten());
    if let Some(store) = flag("--store") {
        c.env("BORG_HOME", store);
    }
    let _ = c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

/// A run's commits and where they go: what `attach` notes, and what runs after it (main.rs passes
/// `regress_learn.rs`, which links them to the earlier decisions they fixed).
pub struct Landed<'a> {
    pub dir: &'a Path,
    pub session: &'a str,
    pub repo: &'a Path,
    pub store: &'a Path,
    pub route: &'a collective::Route,
    pub commits: &'a [String],
    /// Where the session started (its work windows come from it).
    pub start: Option<&'a Start>,
}

/// Runs after `attach` for the session's landed commits (not for a named one); its text ends the
/// run's message.
pub type After = fn(&Landed) -> String;

/// `borg notes attach [<commit>] [--session ID] [--dir DIR] [--store DIR] [--then-learn]`, or
/// `borg notes fetch [<commit>...] [--dir DIR]` (runs `borg-mesh fetch`: the network is there).
pub fn main(args: &[&str], after: Option<After>) -> i32 {
    if args.first() == Some(&"fetch") {
        let beside = std::env::current_exe().ok().map(|e| e.with_file_name("borg-mesh")).filter(|p| p.exists());
        return match Command::new(beside.unwrap_or_else(|| "borg-mesh".into())).args(args).status() {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                eprintln!("borg notes: cannot run borg-mesh ({e}); cargo install borg-mesh --locked");
                1
            }
        };
    }
    // A panic in what runs after attach must not keep `--then-learn` from starting learning.
    let ran = std::panic::catch_unwind(|| run(args, after)).unwrap_or_else(|_| Err("internal error; the notes written so far are kept".into()));
    let code = match ran {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(e) => {
            eprintln!("borg notes: {e}");
            1
        }
    };
    if args.contains(&"--then-learn") {
        then_learn(args);
    }
    code
}

const USAGE: &str = "usage: borg notes attach [<commit>] [--session ID] [--dir DIR] | borg notes fetch [<commit>...] [--dir DIR]";

/// Where an attach run works: the collective `cwd` routes to, its store and state dir, the repo.
struct Place {
    route: collective::Route,
    store: PathBuf,
    dir: PathBuf,
    repo: String,
}

/// The place of `--dir` (else the cwd), with `--store` in place of the collective's home.
fn place(dir: Option<&str>, store: Option<&str>) -> Result<Place, String> {
    let cwd = dir.map(str::to_string).unwrap_or_else(|| std::env::current_dir().unwrap_or_default().display().to_string());
    // The gate is the config as it is now: the share decides replication whatever the caller says.
    let cfg = collective::load().map_err(|e| format!("invalid config, nothing is attached: {e}"))?;
    let route = collective::route(cfg.as_ref(), &cwd, &learn::home());
    let collective::Route::Collective { home, .. } = &route else {
        return Err(format!("{cwd} belongs to no collective (off): nothing is attached"));
    };
    let store = store.map_or_else(|| home.clone(), PathBuf::from);
    let dir = std::env::var_os("BORG_STATE").map_or_else(|| store.join("state"), PathBuf::from);
    let repo = git(Path::new(&cwd), &["rev-parse", "--show-toplevel"]).ok_or(format!("{cwd} is not in a git repo"))?;
    Ok(Place { route, store, dir, repo })
}

/// The session named (`--session`), else the latest one that started in the repo, and its start.
fn session_of(named: Option<&str>, p: &Place) -> Result<(String, Option<Start>), String> {
    match named {
        Some(s) => Ok((s.to_string(), std::fs::read_to_string(state(&p.dir, s, "head")).ok().and_then(|t| serde_json::from_str::<Start>(&t).ok()))),
        None => latest_session(&p.dir, &p.repo).map(|(s, st)| (s, Some(st))).ok_or_else(|| "no drone session started in this repo; pass --session ID".into()),
    }
}

/// The commit named, else the session's landed commits.
fn commits_of(named: Option<&str>, start: Option<&Start>, p: &Place, session: &str) -> Result<Vec<String>, String> {
    match (named, start) {
        (Some(c), _) => Ok(vec![git(Path::new(&p.repo), &["rev-parse", "--verify", &format!("{c}^{{commit}}")]).ok_or(format!("{c} is not a commit"))?]),
        (None, Some(st)) if st.repo == p.repo => Ok(landed(Path::new(&p.repo), st, &windows(&p.dir, session, st))),
        _ => Err("the session did not start in this repo; name a commit".into()),
    }
}

/// The run's message: the notes written, where the excerpts went, and what ran after (`then`).
fn message(done: &Attached, route: &collective::Route, then: &str) -> String {
    let queued = if collective::may_sync(route) {
        format!("{} excerpt(s) queued for the mesh's replicas", done.replicated)
    } else {
        "local collective: nothing leaves this machine".into()
    };
    format!("{} note(s) in {NOTES_REF} (not pushed); {queued}{then}", done.notes.len())
}

fn run(args: &[&str], after: Option<After>) -> Result<String, String> {
    let flag = |f: &str| args.iter().position(|a| *a == f).and_then(|i| args.get(i + 1)).copied();
    if args.first() != Some(&"attach") {
        return Err(USAGE.into());
    }
    let commit = args.get(1).filter(|a| !a.starts_with("--")).copied();
    let p = place(flag("--dir"), flag("--store"))?;
    let (session, start) = session_of(flag("--session"), &p)?;
    let commits = commits_of(commit, start.as_ref(), &p, &session)?;
    let repo = Path::new(&p.repo);
    let landed = Landed { dir: &p.dir, session: &session, repo, store: &p.store, route: &p.route, commits: &commits, start: start.as_ref() };
    let done = attach(&landed, commit.is_some())?;
    // What runs after (regress.rs) is for the session's landed commits, not for a named one.
    let then = after.filter(|_| commit.is_none()).map(|f| f(&landed)).unwrap_or_default();
    Ok(message(&done, &p.route, &then))
}

#[cfg(test)]
mod tests {
    use super::*;
    use links::Link;

    #[test]
    fn redacts_secrets_and_keeps_the_reasoning() {
        let cases = [
            ("export AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY", "wJalr"),
            ("use AKIAIOSFODNN7EXAMPLE for s3", "AKIAIOSFODNN7EXAMPLE"),
            ("token ghp_abcdefghijklmnopqrstuvwxyz0123456789 in CI", "ghp_abc"),
            ("set ANTHROPIC key sk-ant-api03-abcdefghijklmnopqrstuvwx", "sk-ant-api03"),
            ("curl -H 'Authorization: Bearer abc.def.ghijklmnopqrstu'", "abc.def"),
            ("postgres://admin:hunter2@db:5432/app", "hunter2"),
            (r#"{"password": "correct horse battery"}"#, "correct horse"),
            ("db_password = s3cr3t!", "s3cr3t"),
            ("API_KEY: 12345abcdef", "12345abcdef"),
            ("DATABASE_URL=mysql://u@h/db\nNODE_ENV=production", "production"),
            ("-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----", "MIIEow"),
            ("-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk (truncated", "b3Blbn"),
            ("jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N", "eyJhbGci"),
            ("random Zx9Qk2LmPw8RtYv3NbHs6JdFgKa1Bc4D blob", "Zx9Qk2"),
            ("slack xoxb-123456789012-abcdefghij", "xoxb-1234"),
            // Labels the first list missed.
            ("const config = { secretKey: 'hunter2hunter2' }", "hunter2"),
            (r#"{"user": "ann", "pass": "hunter2"}"#, "hunter2"),
            ("db_pass=hunter2", "hunter2"),
            ("then set DB_PASS=hunter2 and rerun", "hunter2"),
            ("mysql -u root -pS3cretPass app", "S3cretPass"),
            ("signing_key = 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08", "9f86d0"),
            ("pw: tr0ub4dor", "tr0ub4dor"),
            // Unlabelled shapes.
            ("the secret is wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY ok", "wJalrXUtnFEMI"),
            ("HF hf_abcdefghijklmnopqrstuvwxyzABCDEFGH works", "hf_abc"),
            ("stripe sk_live_abcdefghijklmnopqrstuvwx and rk_live_abcdefghijklmnopqrstuvwx", "_live_abc"),
            ("publishable pk_test_abcdefghijklmnopqrstuvwx", "pk_test_abc"),
            ("google ya29.a0AfH6SMBabcdefghijklmnopqrstuv", "ya29.a0Af"),
            // A window that starts inside a key: no BEGIN line left, only its body and END.
            ("bnBlbnNzaC1rZXktdjEAAAAABG5vbmU\nAAAAAAAAAAEAAAAzAAAAC3NzaC1lZDI1\n-----END OPENSSH PRIVATE KEY-----\nthen I rotated it", "bnBlbn"),
        ];
        for (text, secret) in cases {
            let r = redact(text);
            assert!(!r.contains(secret), "{text:?} -> {r:?}");
            assert!(r.contains("[REDACTED"), "{text:?} -> {r:?}");
        }
        let plain = "Take the lock before reading the flag (commit 3f2a9c1e5b7d4f6a8c0e2b4d6f8a0c2e4b6d8f0a, \
                     file /home/user/src/borg-ai/crates/borg-mesh/src/sync.rs, author: Ann, 12 tokens: fine). \
                     id 123e4567-e89b-12d3-a456-426614174000; HashMapWithLongCamelCaseName stays.";
        assert_eq!(redact(plain), plain, "reasoning without secrets is untouched");
        assert!(redact("-----END RSA PRIVATE KEY-----\nthen I rotated it").ends_with("then I rotated it"), "what follows the key stays");
        let paths = "see /Users/Ann/Projects/MyApp/src/main.rs and crates/borg-mesh/src/sync.rs for Lock2Order";
        assert_eq!(redact(paths), paths, "paths are not secrets");
    }

    /// The proposal is redacted whole before it is cut to its last 2000 characters: a cut inside a
    /// PEM block (BEGIN gone) never leaves the key body behind.
    #[test]
    fn a_long_proposal_is_redacted_before_it_is_cut() {
        let dir = std::env::temp_dir().join(format!("borg-notes-cut-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let body: String = (0..60).map(|i| format!("b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAAB{i:02}\n")).collect();
        let text = format!("-----BEGIN OPENSSH PRIVATE KEY-----\n{body}-----END OPENSSH PRIVATE KEY-----\nI will use the deploy key above.");
        assert!(text.len() > 2000 && !text[text.len() - 2000..].contains("BEGIN"), "the cut lands inside the key");
        remember_proposal(&dir, "s-cut", &text);
        let kept = std::fs::read_to_string(dir.join("s-cut.proposal")).unwrap();
        assert!(!kept.contains("b3BlbnNzaC1rZXkt"), "{kept}");
        assert!(kept.contains("I will use the deploy key above."));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn route(name: &str, share: collective::Share) -> collective::Route {
        collective::Route::Collective { name: name.into(), home: PathBuf::from("/unused"), share }
    }

    fn sh(repo: &Path, args: &[&str]) -> String {
        let o = Command::new("git").arg("-C").arg(repo).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn repo(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("borg-notes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (repo, state, store) = (base.join("repo"), base.join("state"), base.join("store"));
        std::fs::create_dir_all(&repo).unwrap();
        sh(&repo, &["init", "-q"]);
        sh(&repo, &["config", "user.email", "ann@example.com"]);
        sh(&repo, &["config", "user.name", "Ann"]);
        sh(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
        (repo, state, store)
    }

    #[test]
    fn notes_attach_writes_a_redacted_note_per_landed_commit() {
        let (repo, dir, store) = repo("attach");
        remember_start(&dir, "s-1", &repo.display().to_string());
        let start: Start = serde_json::from_str(&std::fs::read_to_string(dir.join("s-1.head")).unwrap()).unwrap();
        std::fs::write(dir.join("s-1.task"), "fix the login; the staging password=hunter22 must not leak").unwrap();
        std::fs::write(dir.join("s-1.ctx"), "src/login.rs\n#borg-episode s-1-100\n#borg-asked L1\n#borg-episode s-1-200\n").unwrap();
        remember_proposal(&dir, "s-1", "I will rotate ghp_abcdefghijklmnopqrstuvwxyz0123456789 and take the lock first.");
        sh(&repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
        sh(&repo, &["commit", "-q", "--allow-empty", "-m", "two"]);
        let commits = landed(&repo, &start, &windows(&dir, "s-1", &start));
        assert_eq!(commits.len(), 2, "only the session's commits");

        let corp = route("corp", collective::Share::Team);
        let l = Landed { dir: &dir, session: "s-1", repo: &repo, store: &store, route: &corp, commits: &commits, start: None };
        let done = attach(&l, false).unwrap();
        assert_eq!((done.notes.len(), done.replicated), (2, 2));
        let note = sh(&repo, &["notes", "--ref=refs/notes/borg", "show", &commits[0]]);
        assert!(note.contains("fix the login") && note.contains("take the lock first") && note.contains("s-1-100, s-1-200"), "{note}");
        let back = Excerpt::from_note(&commits[0], &note).unwrap();
        assert_eq!((back.session.as_str(), back.collective.as_str(), back.episodes.len()), ("s-1", "corp", 2));
        assert!(back.task.starts_with("fix the login") && back.proposal.ends_with("take the lock first."), "{back:?}");
        assert!(!note.contains("hunter22") && !note.contains("ghp_abc"), "{note}");
        let lines = std::fs::read_to_string(store.join("reasoning.jsonl")).unwrap();
        assert_eq!(lines.lines().count(), 2);
        assert!(!lines.contains("hunter22"));
        assert_eq!(sh(&repo, &["remote"]), "", "nothing to push to, nothing pushed");

        // Idempotent: a second run (another `end`) adds nothing.
        assert_eq!(attach(&l, false).unwrap(), Attached::default());
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    /// The veto's "after merge" (escalation.rs `merged`): a session that landed a commit of its own.
    #[test]
    fn landed_by_reads_the_sessions_recorded_start() {
        let (repo, dir, store) = repo("landed-by");
        assert!(landed_by(&dir, "s-3").is_empty(), "no start recorded");
        remember_start(&dir, "s-3", &repo.display().to_string());
        assert!(landed_by(&dir, "s-3").is_empty(), "nothing landed yet");
        assert!(!crate::escalation::merged(&store, &dir, "s-3", i64::MAX));
        sh(&repo, &["commit", "-q", "--allow-empty", "-m", "landed"]);
        assert_eq!(landed_by(&dir, "s-3"), vec![sh(&repo, &["rev-parse", "HEAD"])]);
        assert!(crate::escalation::merged(&store, &dir, "s-3", i64::MAX));
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn a_private_collective_keeps_its_notes_local() {
        let (repo, dir, store) = repo("private");
        remember_start(&dir, "s-2", &repo.display().to_string());
        std::fs::write(dir.join("s-2.task"), "private refactor").unwrap();
        sh(&repo, &["commit", "-q", "--allow-empty", "-m", "private work"]);
        let head = sh(&repo, &["rev-parse", "HEAD"]);
        let private = route("private", collective::Share::Local);
        let l = Landed { dir: &dir, session: "s-2", repo: &repo, store: &store, route: &private, commits: std::slice::from_ref(&head), start: None };
        let done = attach(&l, false).unwrap();
        assert_eq!((done.notes.len(), done.replicated), (1, 0));
        assert!(sh(&repo, &["notes", "--ref=refs/notes/borg", "show", &head]).contains("private refactor"), "the note itself is written");
        assert!(!store.join("reasoning.jsonl").exists(), "nothing is queued for any peer");
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    /// The CLI decides replication from the config, not from its caller.
    #[test]
    fn share_comes_from_the_config() {
        let cfg: collective::Config =
            serde_json::from_str(r#"{"collectives": [{"name": "private", "paths": ["/p"], "share": "local"}, {"name": "corp", "paths": ["/c"]}]}"#).unwrap();
        let base = Path::new("/irrelevant");
        assert!(!collective::may_sync(&collective::route(Some(&cfg), "/p/repo", base)));
        assert!(collective::may_sync(&collective::route(Some(&cfg), "/c/repo", base)));
    }

    /// git as `sh`, at a fixed time (the reflog records the committer date).
    fn sh_at(repo: &Path, secs: i64, args: &[&str]) {
        let date = format!("@{secs} +0000");
        let o = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_AUTHOR_DATE", &date)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    /// Only commits this session made get its note: not a colleague's commits a fast-forward
    /// brought in, not a commit made while the session was idle (another session, the engineer),
    /// not a commit on a branch checked out and left.
    #[test]
    fn only_the_sessions_own_commits_land() {
        let (repo, dir, _) = repo("own");
        let t0 = 1_700_000_000i64;
        let main = sh(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
        sh_at(&repo, t0 + 10, &["checkout", "-q", "-b", "colleague"]);
        sh_at(&repo, t0 + 20, &["commit", "-q", "--allow-empty", "-m", "colleague's work"]);
        let theirs = sh(&repo, &["rev-parse", "HEAD"]);
        sh_at(&repo, t0 + 30, &["checkout", "-q", &main]);
        let start = Start { repo: repo.display().to_string(), head: sh(&repo, &["rev-parse", "HEAD"]), ms: (t0 + 100) * 1000 };
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("s-3.active"), format!("prompt {}\nstop {}\nprompt {}\n", (t0 + 100) * 1000, (t0 + 200) * 1000 + 500, (t0 + 300) * 1000))
            .unwrap();
        let w = windows(&dir, "s-3", &start);
        assert_eq!(w, vec![((t0 + 100) * 1000, (t0 + 200) * 1000 + 500), ((t0 + 300) * 1000, i64::MAX)]);

        sh_at(&repo, t0 + 150, &["merge", "-q", "--ff-only", "colleague"]); // git pull, in effect
        sh_at(&repo, t0 + 160, &["commit", "-q", "--allow-empty", "-m", "one"]);
        let one = sh(&repo, &["rev-parse", "HEAD"]);
        sh_at(&repo, t0 + 250, &["commit", "-q", "--allow-empty", "-m", "another session's"]);
        sh_at(&repo, t0 + 310, &["checkout", "-q", "-b", "side"]);
        sh_at(&repo, t0 + 320, &["commit", "-q", "--allow-empty", "-m", "abandoned"]);
        sh_at(&repo, t0 + 330, &["checkout", "-q", &main]);
        sh_at(&repo, t0 + 340, &["commit", "-q", "--allow-empty", "-m", "three"]);
        let three = sh(&repo, &["rev-parse", "HEAD"]);
        sh_at(&repo, t0 + 350, &["commit", "-q", "--amend", "--allow-empty", "-m", "three, amended"]);
        let amended = sh(&repo, &["rev-parse", "HEAD"]);

        let got = landed(&repo, &start, &w);
        assert_eq!(got, vec![amended.clone(), one.clone()], "only the session's commits still on HEAD");
        assert!(!got.contains(&theirs) && !got.contains(&three));
        // Without marks: everything the session committed since it started, still not the pull.
        let all = landed(&repo, &start, &[(start.ms, i64::MAX)]);
        assert_eq!(all.len(), 3, "{all:?}");
        assert!(!all.contains(&theirs));
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    /// The link sections survive a note's round trip; a note without them parses as before, and a
    /// proposal that merely says "fixed in" is left alone.
    #[test]
    fn link_sections_round_trip() {
        let r =
            |c: &str, s: &str| LinkRef { commit: c.into(), session: "s-7".into(), task: "fix the race".into(), date: "2026-10-12".into(), strength: s.into() };
        let e = Excerpt {
            kind: "reasoning".into(),
            commit: "c".into(),
            session: "s-1".into(),
            task: "take the lock".into(),
            proposal: "Fixed in the next commit, maybe.".into(),
            episodes: vec!["s-1-1".into()],
            collective: "work".into(),
            fixed_by: vec![r("f1", links::FIX), r("f2", links::REVISITED)],
            fixes: vec![r("d0", links::FIX)],
            ..Excerpt::default()
        };
        assert_eq!(Excerpt::from_note("c", &e.note()), Some(e.clone()));
        let old = Excerpt { fixed_by: vec![], fixes: vec![], ..e.clone() };
        assert_eq!(Excerpt::from_note("c", &old.note()), Some(old.clone()));
        assert_eq!(Excerpt::from_note("c", old.note().trim_end()).map(|x| x.episodes), Some(vec!["s-1-1".to_string()]));
        // As git hands it back: an empty proposal (or task) loses its blank lines, the end is trimmed.
        let empty = Excerpt { proposal: String::new(), episodes: vec![], ..old.clone() };
        let stored = empty.note().replace("\n\n\n", "\n\n");
        assert_eq!(Excerpt::from_note("c", stored.trim_end()), Some(empty.clone()), "{stored:?}");
        let linked = Excerpt { fixes: vec![r("d0", links::FIX)], ..empty.clone() };
        assert_eq!(Excerpt::from_note("c", linked.note().replace("\n\n\n", "\n\n").trim_end()), Some(linked));
        let no_task = Excerpt { task: String::new(), ..empty };
        assert_eq!(Excerpt::from_note("c", no_task.note().replace("\n\n\n", "\n\n").trim_end()), Some(no_task));
    }

    /// A forced re-attach (a named commit) rewrites the reasoning and keeps the links the note lists;
    /// a `git revert` the session made is one of its commits.
    #[test]
    fn a_forced_attach_keeps_the_links_and_a_revert_lands() {
        let (repo, dir, store) = repo("force");
        remember_start(&dir, "s-1", &repo.display().to_string());
        let start: Start = serde_json::from_str(&std::fs::read_to_string(dir.join("s-1.head")).unwrap()).unwrap();
        std::fs::write(dir.join("s-1.task"), "use a ttl cache").unwrap();
        std::fs::write(repo.join("cache.rs"), "ttl\n").unwrap();
        sh(&repo, &["add", "cache.rs"]);
        sh(&repo, &["commit", "-q", "-m", "use a ttl cache"]);
        sh(&repo, &["revert", "--no-edit", "HEAD"]);
        let head = sh(&repo, &["rev-parse", "HEAD"]);
        assert_eq!(landed(&repo, &start, &windows(&dir, "s-1", &start)).first(), Some(&head), "the revert is the session's");
        let r = |c: &str| LinkRef { commit: c.into(), session: "s-7".into(), task: "fix it".into(), date: "2026-10-12".into(), strength: links::FIX.into() };
        let linked = Excerpt {
            kind: "reasoning".into(),
            commit: head.clone(),
            session: "s-0".into(),
            fixed_by: vec![r("f1")],
            fixes: vec![r("d0")],
            ..Excerpt::default()
        };
        write_note(&repo, &linked).unwrap();
        let route = route("private", collective::Share::Local);
        let l = Landed { dir: &dir, session: "s-1", repo: &repo, store: &store, route: &route, commits: std::slice::from_ref(&head), start: None };
        attach(&l, true).unwrap();
        let e = note_of(&repo, &head).unwrap();
        assert_eq!((e.session.as_str(), e.task.as_str()), ("s-1", "use a ttl cache"));
        assert_eq!((e.fixed_by, e.fixes), (vec![r("f1")], vec![r("d0")]));
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    /// `restore` writes the excerpts the store holds for the asked commits as notes, once.
    #[test]
    fn restore_writes_held_excerpts_as_notes() {
        let (repo, _, store) = repo("restore");
        let head = sh(&repo, &["rev-parse", "HEAD"]);
        let e = Excerpt {
            kind: "reasoning".into(),
            commit: head.clone(),
            repo: "repo".into(),
            session: "s-4".into(),
            task: "why the lock".into(),
            proposal: "take it first".into(),
            episodes: vec![],
            ms: 1,
            ..Excerpt::default()
        };
        let other = Excerpt { commit: "0".repeat(40), ..e.clone() };
        append_reasoning(&store, &other).unwrap();
        append_reasoning(&store, &e).unwrap();
        assert_eq!(wanted(&repo, &[]).unwrap(), vec![head.clone()]);
        assert_eq!(restore(&repo, &store, std::slice::from_ref(&head)).unwrap(), vec![head.clone()]);
        assert!(sh(&repo, &["notes", "--ref=refs/notes/borg", "show", &head]).contains("take it first"));
        assert!(restore(&repo, &store, std::slice::from_ref(&head)).unwrap().is_empty(), "an existing note is kept");
        assert_eq!(wanted(&repo, &[]).unwrap(), vec![head.clone()], "noted commits are asked too: links to them come later");
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn restore_brings_the_fetched_links_into_both_notes() {
        let (repo, _, store) = repo("relink");
        let mut shas = Vec::new();
        for msg in ["decide", "fix", "other"] {
            sh(&repo, &["commit", "-q", "--allow-empty", "-m", msg]);
            shas.push(sh(&repo, &["rev-parse", "HEAD"]));
        }
        let (dec, fix, other) = (&shas[0], &shas[1], &shas[2]);
        let e = |c: &str, session: &str| Excerpt {
            kind: "reasoning".into(),
            commit: c.into(),
            session: session.into(),
            task: "take the lock".into(),
            collective: "work".into(),
            ..Excerpt::default()
        };
        // What `borg notes fetch` received: the decision's excerpt and its links, one of another collective.
        append_reasoning(&store, &e(dec, "s-1")).unwrap();
        for (to, collective) in [(fix, "work"), (other, "private")] {
            let side = links::Side { session: "s-7".into(), task: "fix it".into(), date: "2026-10-12".into(), ..links::Side::default() };
            let l = Link {
                kind: links::KIND.into(),
                commit: dec.clone(),
                fix: to.clone(),
                collective: collective.into(),
                strength: links::FIX.into(),
                fixed: side,
                ..Link::default()
            };
            learn::append_jsonl(&store.join("reasoning.jsonl"), &l).unwrap();
        }
        // The fix's note is there already (this machine wrote it); the decision's is not.
        write_note(&repo, &e(fix, "s-7")).unwrap();
        write_note(&repo, &e(other, "s-8")).unwrap();
        assert_eq!(restore(&repo, &store, std::slice::from_ref(dec)).unwrap(), [dec.clone(), fix.clone()]);
        let decision = note_of(&repo, dec).unwrap();
        assert_eq!((decision.fixed_by.len(), decision.fixed_by[0].commit.as_str(), decision.fixed_by[0].task.as_str()), (1, fix.as_str(), "fix it"));
        assert_eq!(note_of(&repo, fix).unwrap().fixes[0].commit, *dec);
        assert!(note_of(&repo, other).unwrap().fixes.is_empty(), "a record of another collective changes no note here");
        assert!(restore(&repo, &store, std::slice::from_ref(dec)).unwrap().is_empty(), "once");
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }
}
