//! Fencing the original: once a session's hand-off is claimed by another session, its edits in the
//! handed-off repos are refused at the tool call (steer.rs `around`, the pause path), reads pass, and
//! its other repos are untouched. An abort after the take fences the taker too, on the hand-off's
//! branch, until the hand-off is done; an abort of an open offer fences nobody (its owner keeps the
//! work), and a done hand-off fences nobody.
//!
//! `<store>/handoff/fences.jsonl` lists, per session of this machine, the repo roots it may not edit,
//! the claim that decided it and the refusal it gets. It is derived, never edited: `refresh` rebuilds it
//! whole, under its lock, from the hand-off log's outcomes (log.rs, each citing its claim) and this
//! machine's own operation logs (wal.rs, where the offer's roots and the take's work trees are), and
//! removes it when nothing is fenced. The hook reads it only: one failed `stat` at every tool call
//! while nothing is fenced (`active`).
//!
//! It fails closed once this machine knows of a claim: lines that cite different claims, or name
//! different writers for one, fence the original unless the remote, asked by `borg handoff` (log.rs
//! `Known`), says the claim is the original's own; so a late or made-up line lifts no fence. A peer's
//! line arriving over the mesh fences at once (`super::merged`), before anyone asked the remote, and is
//! kept in this machine's log (log.rs `keep_outcomes`), so the message log's TTL and bound lift no fence
//! either; a take-back that lost learns of the winner from the remote and fences at once (mod.rs
//! `learned`).
//!
//! What counts as an edit, by the paths a call names, resolved against its directory through symlinks:
//! every tool but the ones that only read (`READ_TOOLS`) naming a path under a fenced root (an edit's
//! files, a patch's joined ones, an MCP tool's path arguments), and a shell command that runs in one
//! (`cd` included) or names a path in one, unless it only reads (`reads_only`: a command borg cannot
//! tell reads only counts as a write).

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{log, wal};

/// One fenced repo of one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Fence<'a> {
    #[serde(borrow)]
    pub id: Cow<'a, str>,
    #[serde(borrow)]
    pub session: Cow<'a, str>,
    #[serde(borrow)]
    pub root: Cow<'a, str>,
    /// The claim that holds, or the first of those the lines cite while the remote was not asked.
    #[serde(borrow)]
    pub claim: Cow<'a, str>,
    /// Only while the root's HEAD is on this branch (a taker's checkout after an abort).
    #[serde(borrow, default, deserialize_with = "crate::msg::cow_opt", skip_serializing_if = "Option::is_none")]
    pub branch: Option<Cow<'a, str>>,
    /// The refusal the session gets, one line.
    #[serde(borrow)]
    pub say: Cow<'a, str>,
}

fn path(home: &Path) -> PathBuf {
    wal::dir(home).join("fences.jsonl")
}

/// Whether any session of this store is fenced: the hook's one `stat`.
pub(crate) fn active(home: &Path) -> bool {
    std::fs::metadata(path(home)).is_ok()
}

/// The refusal of the original once `by` holds the work with `claim`: the repo, and the changes of
/// this tree after the state taken (`after`, when known).
fn taken_say(id: &str, by: &str, claim: &str, root: &str, after: &[String]) -> String {
    let changes = match after {
        [] => String::new(),
        a => format!(" This tree's changes after the state taken stay here and are not in the take: {}.", a.join(", ")),
    };
    format!(
        "[borg] Hand-off {id} of this work was taken by {by} (claim {}): this session's edits in {root} are refused from now on; reading still works.{changes} Stop editing this repo and tell the engineer who has the work now; do not retry another way.",
        super::short(claim)
    )
}

/// The refusal on both sides after an abort that followed the take.
fn aborted_say(id: &str, a: &log::PostAbort, root: &str, branch: Option<&str>) -> String {
    let by = a.by.map_or_else(|| format!("the abort {} on the remote", super::short(a.sha)), super::who);
    let why = a.by.map(|e| e.why.as_ref()).filter(|w| !w.is_empty()).map_or_else(String::new, |w| format!(" ({w})"));
    let at = branch.map_or_else(|| root.to_string(), |b| format!("branch {b} in {root}"));
    format!(
        "[borg] Hand-off {id} was aborted after its take by {by}{why}: edits on {at} are refused on both sides until it is resolved (`borg handoff done {id}`); reading still works and nothing was deleted. Tell the engineer; do not retry another way."
    )
}

/// Rebuilds the fences from the log and this machine's operations (module doc): a session is fenced
/// for the repos of its offer while a claim it did not make may hold (the claim the remote was seen
/// holding, log.rs `Known`, else every claim a line cites that the remote did not refute), unless that
/// claim is an abort; after an abort that followed the take, the taking sessions here are fenced on
/// their checkouts too; nothing once it is done. Under the file's lock, reading the log there: the run
/// that writes last read the newest log.
pub(crate) fn refresh(home: &Path) {
    let p = path(home);
    let _ = std::fs::create_dir_all(wal::dir(home));
    crate::live::locked(&p, || {
        let text = log::read(home);
        let known = log::known(home);
        let here = crate::msg::node(home);
        let mut rows = Vec::new();
        let mut push = |f: Fence| rows.push(serde_json::to_string(&f).unwrap_or_default());
        for h in log::fold(&text).iter() {
            let k = known.get(h.id());
            if h.closed(k) {
                continue;
            }
            let own = wal::read(home, h.id());
            if let Some(a) = h.post_abort(k) {
                let claim = match h.end_with(k) {
                    log::End::Claim { claim, .. } => claim.to_string(),
                    _ => continue,
                };
                for (session, root, branch) in taken_here(&own) {
                    let say = aborted_say(h.id(), &a, &root, Some(&branch));
                    push(Fence {
                        id: h.id().into(),
                        session: session.into(),
                        root: root.into(),
                        claim: claim.as_str().into(),
                        branch: Some(branch.into()),
                        say: say.into(),
                    });
                }
            }
            if h.offer.node != here || h.verb(k) == Some(super::wal::Verb::Abort) {
                continue;
            }
            let o = &h.offer;
            let theirs = |e: &log::Event| !(e.node == o.node && e.session == o.session);
            let (claim, by) = match h.end_with(k) {
                log::End::Open => continue,
                log::End::Claim { by: Some(e), .. } if !theirs(e) => continue,
                log::End::Claim { claim, by: Some(e), .. } => (claim.to_string(), super::who(e)),
                log::End::Claim { claim, by: None, confirmed: true } => (claim.to_string(), format!("the claim {} on the remote", super::short(claim))),
                // Fails closed: lines that cite the claim name different writers.
                log::End::Claim { claim, by: None, confirmed: false } => (
                    claim.to_string(),
                    format!("whoever made claim {} ({} lines name different writers; `borg handoff` asks the remote)", super::short(claim), h.named(claim)),
                ),
                // Fails closed: unless every claim cited is the offering session's own.
                log::End::Contested(n) => match h.ends.iter().find(|e| theirs(e)) {
                    None => continue,
                    Some(e) => (
                        e.claim.as_deref().unwrap_or_default().to_string(),
                        format!("one of {n} claims the log cites (`borg handoff` asks the remote which holds)"),
                    ),
                },
            };
            let roots = wal::ops(&own).into_iter().flat_map(|(_, steps)| steps).find_map(|s| match s {
                wal::Step::Offer { by, repos, .. } if by.session == o.session => Some(repos.into_iter().map(|r| r.root.into_owned()).collect::<Vec<_>>()),
                _ => None,
            });
            let after = k.map(|k| k.after.as_slice()).unwrap_or_default();
            let aborted = h.post_abort(k);
            for root in roots.into_iter().flatten() {
                let say = match &aborted {
                    Some(a) => aborted_say(h.id(), a, &root, None),
                    None => taken_say(h.id(), &by, &claim, &root, after),
                };
                push(Fence { id: h.id().into(), session: o.session.clone(), root: root.into(), claim: claim.as_str().into(), branch: None, say: say.into() });
            }
        }
        if rows.is_empty() {
            let _ = std::fs::remove_file(&p);
            return;
        }
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        if std::fs::read_to_string(&p).ok().as_deref() != Some(body.as_str()) {
            crate::facts::write_atomic(&p, body.as_bytes());
        }
    });
}

/// The sessions of this machine that took the hand-off of the log `own`, with each work tree they
/// checked it out in and its branch.
fn taken_here(own: &str) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    for (_, steps) in wal::ops(own) {
        let Some(wal::Step::Take { by, .. }) = steps.first() else { continue };
        for s in &steps {
            if let wal::Step::Checkout { branch, path, .. } = s
                && !out.iter().any(|(se, p, _)| *se == by.session && *p == *path)
            {
                out.push((by.session.to_string(), path.to_string(), branch.to_string()));
            }
        }
    }
    out
}

/// The branch the work tree at `root` is on, from its HEAD file (no git process: the hook runs this).
fn head_branch(root: &Path) -> Option<String> {
    let dot = root.join(".git");
    let dir = if dot.is_dir() {
        dot
    } else {
        let text = std::fs::read_to_string(&dot).ok()?;
        let g = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        if g.is_absolute() { g } else { root.join(g) }
    };
    let head = std::fs::read_to_string(dir.join("HEAD")).ok()?;
    head.trim().strip_prefix("ref: refs/heads/").map(str::to_string)
}

/// The hand-offs for which this machine fences a session now.
pub(crate) fn ids(home: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path(home)).unwrap_or_default();
    let mut v: Vec<String> = text.lines().filter_map(|l| serde_json::from_str::<Fence>(l).ok()).map(|f| f.id.into_owned()).collect();
    v.dedup();
    v
}

/// Whether this machine fences `session` for some hand-off: one failed `stat` while it fences nobody.
pub(crate) fn fences(home: &Path, session: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path(home)) else { return false };
    text.lines().filter_map(|l| serde_json::from_str::<Fence>(l).ok()).any(|f| f.session == session)
}

/// Tools that only read whatever they name (canonical names, as the adapters map theirs).
const READ_TOOLS: &[&str] = &["read", "grep", "glob", "ls", "webfetch", "websearch", "todowrite", "task", "notebookread"];

/// The refusal of this tool call (`input`, the canonical event) of `session`, if it edits a repo the
/// session handed off and someone else took.
pub(crate) fn refusal(home: &Path, session: &str, input: &Value) -> Option<String> {
    let text = std::fs::read_to_string(path(home)).ok()?;
    let fences: Vec<Fence> = text.lines().filter_map(|l| serde_json::from_str::<Fence>(l).ok()).filter(|f| f.session == session).collect();
    if fences.is_empty() {
        return None;
    }
    let tool = input.get("tool_name").and_then(Value::as_str).unwrap_or_default().to_ascii_lowercase();
    if READ_TOOLS.contains(&tool.as_str()) {
        return None;
    }
    let ti = input.get("tool_input").unwrap_or(&Value::Null);
    let cwd = resolve(Path::new("/"), input.get("cwd").and_then(Value::as_str).unwrap_or_default());
    // A fence on a branch holds while its work tree is on it.
    let roots: Vec<(PathBuf, &Fence)> = fences
        .iter()
        .map(|f| (resolve(Path::new("/"), &f.root), f))
        .filter(|(r, f)| f.branch.as_deref().is_none_or(|b| head_branch(r).as_deref() == Some(b)))
        .collect();
    let under = |p: &Path| roots.iter().find(|(r, _)| p.starts_with(r)).map(|(_, f)| *f);
    let f = if tool == "bash" {
        let cmd = ti.get("command").and_then(Value::as_str).unwrap_or_default();
        let (dirs, words) = places(cmd, &cwd, |_| true);
        let hit = dirs.iter().find_map(|d| under(d)).or_else(|| words.iter().find_map(|w| under(w)));
        // A fence on a branch lets its work tree leave the branch: then it holds no more.
        hit.filter(|f| !reads_only(cmd) && !(f.branch.is_some() && switches_branch(cmd, &cwd)))
    } else {
        let mut named = Vec::new();
        paths_in(ti, false, &mut named);
        named.iter().find_map(|p| under(&resolve(&cwd, p)))
    }?;
    Some(f.say.to_string())
}

/// Where a tool call (`input`, the canonical event) may have written, as an edit counts for the fence:
/// nothing for a tool that only reads or a shell command that only reads (`reads_only`); an edit's
/// paths; a shell command's directories and the words that name a path (with a `/`). Resolved against
/// the call's directory: mod.rs `session_repos` offers the repos a session wrote in.
pub(crate) fn written(input: &Value) -> Vec<PathBuf> {
    let tool = input.get("tool_name").and_then(Value::as_str).unwrap_or_default().to_ascii_lowercase();
    if READ_TOOLS.contains(&tool.as_str()) {
        return Vec::new();
    }
    let ti = input.get("tool_input").unwrap_or(&Value::Null);
    let cwd = resolve(Path::new("/"), input.get("cwd").and_then(Value::as_str).unwrap_or_default());
    if tool == "bash" {
        let cmd = ti.get("command").and_then(Value::as_str).unwrap_or_default();
        if reads_only(cmd) {
            return Vec::new();
        }
        let (mut dirs, words) = places(cmd, &cwd, |w| w.contains('/'));
        dirs.extend(words);
        return dirs;
    }
    let mut named = Vec::new();
    paths_in(ti, false, &mut named);
    named.iter().map(|p| resolve(&cwd, p)).collect()
}

/// Whether a key of a tool's input names a path (`file_path`, `path`, `relative_path`, `notebook_path`,
/// `dir`, `cwd`, a target or destination).
fn path_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    ["path", "file", "dir", "cwd", "root", "target", "dest"].iter().any(|w| k.contains(w))
}

/// The paths a tool's input names: every whitespace-separated part of a string under a path key (and
/// the string whole: a joined list of files, or a name with spaces), and any absolute path elsewhere.
fn paths_in<'a>(v: &'a Value, keyed: bool, out: &mut Vec<&'a str>) {
    match v {
        Value::String(s) if keyed => {
            out.push(s.trim());
            out.extend(s.split_whitespace());
        }
        Value::String(s) => out.extend(s.split_whitespace().filter(|w| w.starts_with('/'))),
        Value::Array(a) => a.iter().for_each(|x| paths_in(x, keyed, out)),
        Value::Object(m) => m.iter().for_each(|(k, x)| paths_in(x, keyed || path_key(k), out)),
        _ => {}
    }
}

/// `p` (`~` the home directory) against `base`, without `.` and `..`, through every symlink of its
/// longest part that exists: a path names the same place however it is spelled, even before the file
/// exists.
fn resolve(base: &Path, p: &str) -> PathBuf {
    let p = match p.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            std::env::var_os("HOME").map_or_else(|| PathBuf::from(p), |h| PathBuf::from(h).join(rest.trim_start_matches('/')))
        }
        _ => PathBuf::from(p),
    };
    let mut lexical = PathBuf::new();
    for c in base.join(p).components() {
        match c {
            Component::ParentDir => {
                lexical.pop();
            }
            Component::CurDir => {}
            c => lexical.push(c),
        }
    }
    let mut rest = Vec::new();
    let mut at = lexical.as_path();
    loop {
        if let Ok(real) = std::fs::canonicalize(at) {
            return rest.iter().rev().fold(real, |acc, c| acc.join(c));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                at = parent;
            }
            _ => return lexical,
        }
    }
}

/// A shell command as words, segment by segment (split at unquoted `;`, `|`, `&`, newlines), quotes
/// and escapes removed; and whether it redirects output anywhere but /dev/null or another stream.
struct Shell {
    segments: Vec<Vec<String>>,
    writes: bool,
}

fn shell(cmd: &str) -> Shell {
    let (mut segments, mut seg, mut word) = (Vec::new(), Vec::new(), None::<String>);
    let mut writes = false;
    let mut it = cmd.chars().peekable();
    // A redirect's target, once its word ends.
    let mut target = false;
    let end = |word: &mut Option<String>, seg: &mut Vec<String>, target: &mut bool, writes: &mut bool| {
        if let Some(w) = word.take() {
            if std::mem::take(target) {
                *writes |= w != "/dev/null";
            } else {
                seg.push(w);
            }
        }
    };
    while let Some(c) = it.next() {
        match c {
            '\'' => {
                let w = word.get_or_insert_with(String::new);
                for q in it.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    w.push(q);
                }
            }
            '"' => {
                let w = word.get_or_insert_with(String::new);
                while let Some(q) = it.next() {
                    match q {
                        '"' => break,
                        '\\' => w.extend(it.next()),
                        q => w.push(q),
                    }
                }
            }
            '\\' => word.get_or_insert_with(String::new).extend(it.next()),
            '>' => {
                // `2>`: the stream's number is no word.
                if word.as_deref().is_some_and(|w| w.chars().all(|c| c.is_ascii_digit())) {
                    word = None;
                }
                end(&mut word, &mut seg, &mut target, &mut writes);
                if it.peek() == Some(&'>') {
                    it.next();
                }
                if it.peek() == Some(&'&') {
                    // `>&2`: into another stream.
                    it.next();
                    while it.peek().is_some_and(char::is_ascii_digit) {
                        it.next();
                    }
                } else {
                    target = true;
                }
            }
            '&' if it.peek() == Some(&'>') => {
                end(&mut word, &mut seg, &mut target, &mut writes);
            }
            ';' | '|' | '&' | '\n' => {
                end(&mut word, &mut seg, &mut target, &mut writes);
                if !seg.is_empty() {
                    segments.push(std::mem::take(&mut seg));
                }
            }
            c if c.is_whitespace() => end(&mut word, &mut seg, &mut target, &mut writes),
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    end(&mut word, &mut seg, &mut target, &mut writes);
    writes |= target;
    if !seg.is_empty() {
        segments.push(seg);
    }
    Shell { segments, writes }
}

/// Where a shell command acts: the directories it runs in (`cwd`, then each `cd`'s), and every word
/// (and each part of a `--flag=value` or `NAME=value`) that `word` keeps, as a path from the directory
/// it runs in.
fn places(cmd: &str, cwd: &Path, word: impl Fn(&str) -> bool) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let (mut dirs, mut words) = (vec![cwd.to_path_buf()], Vec::new());
    let mut here = cwd.to_path_buf();
    for seg in shell(cmd).segments {
        if seg.first().map(String::as_str) == Some("cd")
            && let Some(to) = seg.get(1)
        {
            here = resolve(&here, to);
            dirs.push(here.clone());
        }
        for w in &seg {
            for part in w.split('=').filter(|p| !p.is_empty() && !p.starts_with('-') && word(p)) {
                words.push(resolve(&here, part));
            }
        }
    }
    (dirs, words)
}

/// Whether the shell command `cmd` (run in `cwd`) only moves its work tree to another branch: one `git
/// [-C <dir>] switch|checkout [-q|--quiet] <branch>`, nothing else run, nothing forced, created or
/// detached, no pathspec; for `checkout`, `<branch>` names no path in its directory (else it restores
/// files). git refuses a switch that would overwrite changes.
fn switches_branch(cmd: &str, cwd: &Path) -> bool {
    if cmd.contains('`') || cmd.contains("$(") || cmd.contains("<(") || cmd.contains(">(") {
        return false;
    }
    let sh = shell(cmd);
    let [seg] = sh.segments.as_slice() else { return false };
    if sh.writes || seg.first().map(|p| p.rsplit('/').next().unwrap_or(p)) != Some("git") {
        return false;
    }
    let (mut dir, mut it) = (cwd.to_path_buf(), seg[1..].iter().map(String::as_str));
    let sub = loop {
        match it.next() {
            Some("-C") => match it.next() {
                Some(d) => dir = resolve(&dir, d),
                None => return false,
            },
            Some(a) if a.starts_with('-') => return false,
            other => break other,
        }
    };
    let rest: Vec<&str> = it.filter(|a| !matches!(*a, "-q" | "--quiet")).collect();
    match (sub, rest.as_slice()) {
        (Some("switch"), [b]) => !b.starts_with('-'),
        (Some("checkout"), [b]) => !b.starts_with('-') && std::fs::symlink_metadata(dir.join(b)).is_err(),
        _ => false,
    }
}

/// Programs that only read (or only print) whatever their arguments, unless a flag below makes them
/// write or run something (`writes_with`): borg's own commands, the readers borg-trace knows
/// (fingerprint.rs `only_reads`), and listing and comparing tools.
const READERS: &[&str] = &[
    "borg", "cat", "sed", "head", "tail", "nl", "bat", "wc", "jq", "rg", "grep", "cd", "ls", "echo", "printf", "find", "tree", "pwd", "stat", "file", "du",
    "df", "diff", "cmp", "sort", "cut", "tr", "basename", "dirname", "realpath", "readlink", "true", "test", "which", "od",
];

/// git's subcommands that change nothing in the repo they read.
const GIT_READS: &[&str] =
    &["status", "log", "diff", "show", "ls-files", "ls-tree", "rev-parse", "blame", "describe", "cat-file", "shortlog", "grep", "rev-list", "for-each-ref"];

/// Whether a sed script only prints: each of its commands (split at `;`) an address and one of the
/// commands that print, delete from the output, quit or move between lines, or a substitution whose
/// flags neither write (`w`) nor run (`e`). Anything sed could write or run with fails.
fn sed_prints(script: &str) -> bool {
    script.split([';', '\n']).map(str::trim).filter(|c| !c.is_empty()).all(|c| {
        let mut rest = c.trim_start_matches(|ch: char| ch.is_ascii_digit() || "$,~+! ".contains(ch));
        // A /regex/ address.
        while let Some(r) = rest.strip_prefix('/') {
            let Some(close) = r.find('/') else { return false };
            rest = r[close + 1..].trim_start_matches(|ch: char| ch.is_ascii_digit() || "$,~+! ".contains(ch));
        }
        let mut chars = rest.chars();
        match chars.next() {
            Some('p' | 'P' | 'd' | 'D' | 'n' | 'N' | 'l' | 'g' | 'G' | 'h' | 'H' | 'x' | 'z' | '=' | '{' | '}') => chars.as_str().trim().is_empty(),
            Some('q' | 'Q') => chars.as_str().trim().chars().all(|c| c.is_ascii_digit()),
            Some('s') => {
                let Some(d) = chars.next() else { return false };
                let body = chars.as_str();
                let mut parts = body.split(d);
                let (Some(_), Some(_), Some(flags), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else { return false };
                flags.chars().all(|f| "gpiImM0123456789".contains(f))
            }
            _ => false,
        }
    })
}

/// Whether a reader's arguments make it write a file or run a program: `sed -i` or a script that
/// writes or runs, `sort -o` or a compressor, `find -delete` or `-exec`, `tree -o`, `rg --pre`,
/// `file -C`.
fn writes_with(prog: &str, args: &[String]) -> bool {
    let any = |f: &dyn Fn(&str) -> bool| args.iter().any(|a| f(a));
    match prog {
        "sed" => {
            let in_place = any(&|a| a.starts_with("--in-place") || (a.starts_with('-') && !a.starts_with("--") && a.contains('i')));
            let mut scripts = Vec::new();
            let mut it = args.iter();
            let mut from_file = false;
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-e" | "--expression" => scripts.extend(it.next().cloned()),
                    "-f" | "--file" => from_file = true,
                    a if a.starts_with("--expression=") => scripts.push(a["--expression=".len()..].to_string()),
                    a if a.starts_with("-e") => scripts.push(a[2..].to_string()),
                    a if a.starts_with("--file=") || (a.starts_with("-f") && a.len() > 2) => from_file = true,
                    _ => {}
                }
            }
            if scripts.is_empty()
                && let Some(s) = args.iter().find(|a| !a.starts_with('-'))
            {
                scripts.push(s.clone());
            }
            in_place || from_file || !scripts.iter().all(|s| sed_prints(s))
        }
        "sort" => any(&|a| a == "-o" || a.starts_with("--output") || a.starts_with("--compress-program") || (a.starts_with("-o") && !a.starts_with("--"))),
        "find" => any(&|a| matches!(a, "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir" | "-fprint" | "-fprint0" | "-fprintf" | "-fls")),
        "tree" => any(&|a| a == "-o" || a.starts_with("--output")),
        "rg" => any(&|a| a.starts_with("--pre")),
        "file" => any(&|a| a == "-C" || a == "--compile"),
        _ => false,
    }
}

/// Whether the shell command `cmd` only reads: no substitution or process substitution, no redirect
/// other than to /dev/null or between streams, and every part a reader (`READERS`, `GIT_READS`) without
/// a flag that writes or runs a program (git: no `-c`, `-O`, `--output` or external diff). Anything
/// else counts as a write.
pub(crate) fn reads_only(cmd: &str) -> bool {
    if cmd.contains('`') || cmd.contains("$(") || cmd.contains("<(") || cmd.contains(">(") || cmd.contains("<<") {
        return false;
    }
    let sh = shell(cmd);
    !sh.writes
        && sh.segments.iter().all(|seg| {
            let toks: Vec<&String> = seg.iter().skip_while(|t| t.contains('=') && !t.starts_with('-')).collect();
            let Some((first, args)) = toks.split_first() else { return true };
            let prog = first.rsplit('/').next().unwrap_or(first);
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            if prog == "git" {
                let mut it = args.iter();
                let sub = loop {
                    match it.next().map(String::as_str) {
                        Some("-c") => return false,
                        Some("-C") => {
                            it.next();
                        }
                        Some(a) if a.starts_with('-') => {}
                        other => break other,
                    }
                };
                let runs = it.any(|a| a.starts_with("-O") || a.starts_with("--open-files-in-pager") || a.starts_with("--output") || a == "--ext-diff");
                return sub.is_none_or(|s| GIT_READS.contains(&s)) && !runs;
            }
            READERS.contains(&prog) && !writes_with(prog, &args)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_pass_and_writes_count() {
        for r in [
            "cat src/a.rs | head -n 20",
            "git -C /w status --short && git log -3",
            "rg -n foo src 2>/dev/null",
            "ls -la; pwd",
            "borg handoff",
            "sed -n 1,20p a.rs",
            "sed -n '/fn main/p' a.rs",
            "sed 's/a/b/g' a.rs",
            "find . -name '*.rs'",
            "grep '>' a.rs",
            "cat a.rs 2>&1 | wc -l",
        ] {
            assert!(reads_only(r), "{r}");
        }
        for w in [
            "echo x > a.rs",
            "echo x >>a.rs",
            "sed -i s/a/b/ a.rs",
            "cargo build",
            "git commit -am x",
            "rm -rf src",
            "cat a | tee b",
            "find . -delete",
            "echo $(rm x)",
            "sort -o a a",
            "git checkout .",
            "sed 'w f' a",
            "sed 's/a/b/w f' a",
            "sed -n '1e rm x' a",
            "awk 'BEGIN{system(\"rm x\")}'",
            "uniq in out",
            "xxd in out",
            "tree -o f",
            "rg --pre ./x foo",
            "git grep -Ocode foo",
            "git diff --output=f",
            "git log --output f",
            "git -c core.pager=x log",
            "sort --compress-program=sh a",
            "cat <<EOF > a\nx\nEOF",
        ] {
            assert!(!reads_only(w), "{w}");
        }
    }

    fn home(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-handoff-fence-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(wal::dir(&d)).unwrap();
        d
    }

    /// A taker fenced on its branch may leave it (and only leave it); off the branch nothing is fenced.
    #[test]
    fn a_fence_on_a_branch_lets_the_work_tree_leave_it() {
        let h = home("branch");
        let w = h.join("w");
        std::fs::create_dir_all(w.join(".git")).unwrap();
        std::fs::write(w.join("lib.rs"), "").unwrap();
        std::fs::write(w.join(".git/HEAD"), "ref: refs/heads/borg/handoff/h0123456789ab\n").unwrap();
        let ws = w.to_string_lossy().into_owned();
        let f = Fence {
            id: "h0123456789ab".into(),
            session: "s2".into(),
            root: ws.as_str().into(),
            claim: "c".repeat(40).into(),
            branch: Some("borg/handoff/h0123456789ab".into()),
            say: "aborted".into(),
        };
        std::fs::write(path(&h), format!("{}\n", serde_json::to_string(&f).unwrap())).unwrap();
        let bash = |cmd: &str| refusal(&h, "s2", &json!({"session_id": "s2", "cwd": ws, "tool_name": "Bash", "tool_input": {"command": cmd}})).is_some();
        for leave in ["git switch main", "git checkout -q main", "git -C . switch --quiet main"] {
            assert!(!bash(leave), "{leave}");
        }
        for write in [
            "git checkout lib.rs",
            "git switch -f main",
            "git checkout -B main",
            "git switch main && touch x",
            "git checkout main -- lib.rs",
            "git switch -c x",
        ] {
            assert!(bash(write), "{write}");
        }
        std::fs::write(w.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(!bash("touch lib.rs"), "off the branch: not fenced");
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn edits_under_a_fenced_root_are_refused_and_nothing_else() {
        let h = home("refuse");
        // A real tree, so paths resolve through the symlink `link` to it.
        let w = h.join("w");
        for d in ["app/src", "app-web", "other"] {
            std::fs::create_dir_all(w.join(d)).unwrap();
        }
        std::os::unix::fs::symlink(w.join("app"), h.join("link")).unwrap();
        let app = w.join("app").to_string_lossy().into_owned();
        let say = taken_say("h0123456789ab", "bob@y", &"c".repeat(40), &app, &[]);
        let f =
            Fence { id: "h0123456789ab".into(), session: "s1".into(), root: app.as_str().into(), claim: "c".repeat(40).into(), branch: None, say: say.into() };
        std::fs::write(path(&h), format!("{}\n", serde_json::to_string(&f).unwrap())).unwrap();
        let ws = w.to_string_lossy().into_owned();
        let at = |p: &str| format!("{ws}/{p}");
        let ev = |tool: &str, ti: Value, cwd: &str| json!({"session_id": "s1", "cwd": cwd, "tool_name": tool, "tool_input": ti});
        let refused = |tool: &str, ti: Value, cwd: &str| refusal(&h, "s1", &ev(tool, ti, cwd)).is_some();
        assert!(refused("Edit", json!({"file_path": at("app/src/a.rs")}), &app));
        assert!(refused("Write", json!({"file_path": "src/new.rs"}), &app), "relative to the cwd");
        assert!(refused("Edit", json!({"file_path": at("other/x"), "edits": [{"file_path": at("app/b.rs")}]}), &ws), "a patch's further file");
        assert!(refused("Edit", json!({"file_path": format!("{} {}", at("other/x"), at("app/y"))}), &ws), "a patch's joined files");
        assert!(refused("Edit", json!({"file_path": h.join("link/a.rs")}), "/"), "through a symlink");
        assert!(refused("mcp__serena__replace_content", json!({"relative_path": "app/src/a.rs", "needle": "x"}), &ws), "an MCP edit");
        assert!(refused("write_file", json!({"path": at("app/x")}), "/"), "a tool of another name");
        assert!(refused("Bash", json!({"command": "cargo fmt"}), &at("app/src")));
        assert!(refused("Bash", json!({"command": format!("rm {}", at("app/x"))}), "/tmp"), "a command naming the root");
        assert!(refused("Bash", json!({"command": "rm -rf app/src"}), &ws), "a relative path");
        assert!(refused("Bash", json!({"command": "cd app && cargo fmt"}), &ws), "after a cd");
        assert!(refused("Bash", json!({"command": "git -C ../app commit -m x"}), &at("other")));
        assert!(refused("Bash", json!({"command": "cargo fmt --manifest-path=app/Cargo.toml"}), &ws));
        assert!(refused("Bash", json!({"command": "touch link/x"}), &h.to_string_lossy()), "through a symlink");
        assert!(!refused("Bash", json!({"command": "git status"}), &app), "reads pass");
        assert!(!refused("Read", json!({"file_path": at("app/src/a.rs")}), &app));
        assert!(!refused("Grep", json!({"path": at("app")}), &app));
        assert!(!refused("Edit", json!({"file_path": at("app2/a.rs")}), &ws), "a sibling repo whose name starts alike");
        assert!(!refused("Bash", json!({"command": format!("touch {}", at("app-web/x"))}), "/tmp"), "a sibling named alike");
        assert!(!refused("Edit", json!({"file_path": at("app/../other/a.rs")}), &ws));
        assert!(!refused("Bash", json!({"command": "cargo build"}), &at("other")), "another repo");
        assert!(!refused("mcp__x__send", json!({"text": "fixed app/src/a.rs"}), &ws), "a relative path in free text");
        assert!(refusal(&h, "s2", &ev("Edit", json!({"file_path": at("app/a.rs")}), &app)).is_none(), "another session");
        assert!(refused("Bash", json!({"command": "git switch main"}), &app), "a fence on the repo holds whatever the branch");
        let why = refusal(&h, "s1", &ev("Edit", json!({"file_path": at("app/a.rs")}), &app)).unwrap();
        assert!(why.contains("taken by bob@y") && why.contains("h0123456789ab") && !why.contains('\n'), "{why}");
        let _ = std::fs::remove_dir_all(&h);
    }
}
