//! Decision → fix links: which earlier decision did a fix correct? (PLAN.md "Milestone B")
//!
//! For each commit a session landed (notes.rs `landed`), after its note is written: the lines the
//! commit removed or changed are blamed at its parent (`blame -w -M -C`, so a reformat or a move in
//! between does not hide who wrote them), and each origin commit with borg reasoning from another
//! session, old enough (`BORG_REGRESS_MIN_AGE`, default 2d, by committer times) and not linked yet,
//! becomes a link: listed in both notes (links.rs) and, when the collective may sync, appended to
//! its `reasoning.jsonl` (kind `regress`). Pure additions, formatter hunks, moved lines and
//! generated files (by name, or `linguist-generated` / `-diff` in .gitattributes) blame nobody
//! (`regress_diff.rs`); a fix that touches more than 40 files or 2000 lines is a refactor and is
//! skipped whole. A link is strong (`fix`) when the signals say it fixed something
//! (`regress_signals.rs`), else weak (`revisited`): stored, never learned from. A fix that changed
//! several decisions' lines is weak for each decision it only renamed in.
//!
//! **Privacy**: an off repo gets nothing (notes.rs refuses it first); a local collective's links
//! stay in the repo's notes; every task and subject is redacted before it is written.
//!
//! **Fail open**: every git call has a timeout (`BORG_REGRESS_TIMEOUT_S`, default 10 s) inside a
//! 60 s budget per run (`regress_git.rs`; the regret step that follows shares it); a call past either, a root or merge commit, a shallow graft or a parse
//! failure means fewer links, never an error in the end-of-session job. At most 5 links per
//! session, strong ones first. `BORG_REGRESS=off|links|learn` (default `links`) gates it all.

use std::collections::{BTreeMap, HashSet};
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::notes::links::{self, Link, LinkRef, Side, Touched};
use crate::notes::reasoning;
use crate::notes::regress_cosmetic as cosmetic;
use crate::notes::regress_diff::{self as diff, Hunk};
pub use crate::notes::regress_git::Git;
use crate::notes::regress_signals as signals;
use crate::notes::{self, Excerpt, redact};
use crate::{collective, learn};

/// Free text in a link (a task's first line, a subject) is cut to this many characters. Guard: bounds
/// a note's line whatever the task said.
const TEXT_CAP: usize = 240;
/// At most this many lines of one file are blamed. Guard: a fix that rewrote a whole file costs one
/// bounded `git blame`, not one per line of it.
const SPAN_CAP: usize = 400;
/// A link lists at most this many files. Guard: a sweeping fix (a rename, a reformat) still writes a
/// short link.
const FILES_CAP: usize = 10;
/// At most this many line ranges per file of a link. Guard, as `FILES_CAP`.
const RANGES_CAP: usize = 20;
/// A day in seconds (git's commit times). Derived: clock.rs `DAY_MS`.
const DAY: i64 = crate::clock::DAY_MS / 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    /// Links are recorded; no regret episode is written (learning is a later step).
    Links,
    Learn,
}

/// Limits and the rollout gate, from the environment (module doc).
#[derive(Debug, Clone)]
pub struct Cfg {
    pub mode: Mode,
    pub min_age_s: i64,
    pub max_links: usize,
    pub max_files: usize,
    pub max_lines: usize,
    pub call_timeout: Duration,
    pub budget: Duration,
}

impl Default for Cfg {
    fn default() -> Cfg {
        Cfg {
            mode: Mode::Links,
            min_age_s: 2 * DAY,
            max_links: 5,
            max_files: 40,
            max_lines: 2000,
            call_timeout: Duration::from_secs(10),
            budget: Duration::from_secs(60),
        }
    }
}

impl Cfg {
    pub fn from_env() -> Cfg {
        let var = |k: &str| std::env::var(k).ok();
        let mode = match var("BORG_REGRESS").as_deref() {
            Some("off") => Mode::Off,
            Some("learn") => Mode::Learn,
            _ => Mode::Links,
        };
        let d = Cfg::default();
        Cfg {
            mode,
            min_age_s: var("BORG_REGRESS_MIN_AGE").and_then(|s| signals::parse_age(&s)).unwrap_or(d.min_age_s),
            call_timeout: var("BORG_REGRESS_TIMEOUT_S").and_then(|s| s.trim().parse().ok()).map_or(d.call_timeout, Duration::from_secs),
            ..d
        }
    }

    /// A git runner with this run's limits.
    #[must_use]
    pub fn git<'a>(&self, repo: &'a Path) -> Git<'a> {
        Git::new(repo, self.call_timeout, self.budget)
    }
}

/// The collective the work routes to: its name, its store and whether it may sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub here: String,
    pub store: PathBuf,
    pub sync: bool,
}

impl Scope {
    /// `None` for an off route.
    #[must_use]
    pub fn of(route: &collective::Route, store: &Path) -> Option<Scope> {
        let collective::Route::Collective { name, .. } = route else { return None };
        Some(Scope { here: name.clone(), store: store.to_path_buf(), sync: collective::may_sync(route) })
    }
}

/// The fixing commit and what its session left behind.
#[derive(Debug, Clone, Default)]
pub struct Fix {
    pub sha: String,
    /// Empty when unknown (a replayed commit without a note).
    pub session: String,
    pub task: String,
    pub subject: String,
    pub body: String,
    /// Committer time, epoch seconds.
    pub secs: i64,
    pub date: String,
    pub email: String,
    /// The engineer's own words (prompts, task): the `words` signal reads only these.
    pub session_text: String,
    /// `<state>/<session>.tests`, when the session ran tests.
    pub tests: Option<String>,
    pub windows: Vec<(i64, i64)>,
}

/// A candidate left out, and why (`borg regress --explain`; never stored).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    pub fix: String,
    /// The origin commit, or `path:line` for a hunk dropped before blame.
    pub origin: String,
    pub reason: String,
}

/// What detection gave.
#[derive(Debug, Clone, Default)]
pub struct Detected {
    pub links: Vec<Link>,
    pub skipped: Vec<Skipped>,
}

impl Detected {
    fn skip(&mut self, fix: &str, origin: &str, reason: impl Into<String>) {
        self.skipped.push(Skipped { fix: fix.into(), origin: origin.into(), reason: reason.into() });
    }
}

fn first_line(s: &str, cap: usize) -> String {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    redact(line).chars().take(cap).collect()
}

/// How a detection run is configured.
pub struct Run<'a> {
    pub git: &'a Git<'a>,
    pub scope: &'a Scope,
    pub cfg: &'a Cfg,
    /// Replay only: keep origins without borg reasoning (measures the detector on repos without notes).
    pub any_origin: bool,
    /// The (decision, fix) pairs already linked.
    pub seen: &'a HashSet<(String, String)>,
}

/// An origin commit: the lines of it the fix changed, per file at the fix's parent.
#[derive(Debug, Default)]
struct Origin {
    sha: String,
    lines: Vec<(String, u32)>,
    secs: i64,
    date: String,
    /// Every line of it the fix changed was in a rename's hunk (`regress_cosmetic::renamed_only`).
    renamed: bool,
}

/// The fix's single parent, or why the commit is skipped whole (root, merge, too large).
fn shape(git: &Git, cfg: &Cfg, sha: &str) -> Result<String, String> {
    let parents = git.run(&["rev-parse", &format!("{sha}^@")]).ok_or("timeout")?;
    let parents: Vec<&str> = parents.lines().filter(|l| !l.is_empty()).collect();
    let parent = match parents[..] {
        [] => return Err("no-parent".into()),
        [p] => p.to_string(),
        _ => return Err("merge".into()),
    };
    let numstat = git.run(&["diff", "--numstat", "-z", "-M", "--no-ext-diff", &parent, sha]).ok_or("timeout")?;
    let (files, lines) = diff::size(&numstat);
    if files > cfg.max_files || lines > cfg.max_lines {
        return Err(format!("too-large ({files} files, {lines} lines)"));
    }
    Ok(parent)
}

/// The paths of `hunks` .gitattributes marks generated.
fn generated_by_attr(git: &Git, hunks: &[Hunk]) -> HashSet<String> {
    let mut args = vec!["check-attr", "-z", "linguist-generated", "diff", "--"];
    args.extend(hunks.iter().map(|h| h.path.as_str()).filter(|p| !p.is_empty()));
    git.run(&args).map(|out| diff::generated_by_attr(&out)).unwrap_or_default()
}

/// Why hunk `h` blames nobody before any move is looked at, if it does not.
fn unblamed(h: &Hunk) -> Option<(String, &'static str)> {
    let at = |n: u32| format!("{}:{n}", if h.path.is_empty() { "(new file)" } else { &h.path });
    match h.removed.first() {
        None => Some((at(h.added_at), "additions-only")),
        Some(&(n, _)) if h.path.is_empty() => Some((at(n), "unparsed-path")),
        Some(&(n, _)) => cosmetic::formatting_only(h).then(|| (at(n), "format")),
    }
}

/// The paths of `all` skipped whole, and why: generated (by name or .gitattributes), or only
/// reflowed.
fn skipped_files(git: &Git, all: &[Hunk]) -> BTreeMap<String, &'static str> {
    let mut files: BTreeMap<String, &'static str> = cosmetic::reflowed_files(all).into_iter().map(|p| (p, "format")).collect();
    let named = all.iter().map(|h| h.path.clone()).filter(|p| diff::generated(p));
    files.extend(named.chain(generated_by_attr(git, all)).map(|p| (p, "generated")));
    files
}

/// The fix's hunks that removed or changed lines: pure additions, generated files, formatter hunks
/// and moved lines dropped, each with its reason.
fn changed(git: &Git, parent: &str, sha: &str, d: &mut Detected) -> Vec<Hunk> {
    let Some(all) = git.hunks(parent, sha) else {
        d.skip(sha, "", "timeout");
        return Vec::new();
    };
    let files = skipped_files(git, &all);
    for (path, reason) in &files {
        d.skip(sha, path, *reason);
    }
    // A hunk that blames nobody keeps its added lines: they show where a removed line moved to.
    let rest = all.into_iter().map(|mut h| {
        let why = if files.contains_key(&h.path) { None } else { unblamed(&h) };
        if let Some((at, reason)) = &why {
            d.skip(sha, at, *reason);
        }
        if why.is_some() || files.contains_key(&h.path) {
            h.removed.clear();
        }
        h
    });
    let (kept, moved) = cosmetic::drop_moves(rest.collect());
    if moved > 0 {
        d.skip(sha, &format!("{moved} line(s)"), "move");
    }
    kept
}

/// The commits listed in the repo's `shallow` file (the grafts of a shallow clone).
fn shallow_roots(git: &Git) -> HashSet<String> {
    let Some(p) = git.run(&["rev-parse", "--git-path", "shallow"]) else { return HashSet::new() };
    let p = git.repo().join(p.trim());
    std::fs::read_to_string(p).unwrap_or_default().lines().map(|l| l.trim().to_string()).collect()
}

/// Whether line `n` of `path` (at the parent) is in a rename's hunk.
fn in_rename(hunks: &[Hunk], path: &str, n: u32) -> bool {
    hunks.iter().any(|h| h.path == path && h.removed.iter().any(|(m, _)| *m == n) && cosmetic::renamed_only(h))
}

/// The origin commits of `hunks`' removed lines at `parent`, one blame per file.
fn origins(git: &Git, parent: &str, hunks: &[Hunk], d: &mut Detected) -> BTreeMap<String, Origin> {
    let shallow = shallow_roots(git);
    let mut out: BTreeMap<String, Origin> = BTreeMap::new();
    for (path, mut lines) in diff::removed_by_path(hunks) {
        lines.truncate(SPAN_CAP);
        let spans: Vec<String> = diff::ranges(&lines).iter().map(|(a, b)| format!("-L{a},{b}")).collect();
        let mut args = vec!["blame", "--root", "-w", "-M", "-C", "--porcelain"];
        args.extend(spans.iter().map(String::as_str));
        args.extend([parent, "--", path.as_str()]);
        let Some(porcelain) = git.run(&args) else {
            d.skip(parent, &path, "blame-failed");
            continue;
        };
        for (sha, line) in diff::blame_lines(&porcelain).into_iter().filter(|(s, _)| !shallow.contains(s)) {
            let o = out.entry(sha.clone()).or_insert_with(|| Origin { sha, renamed: true, ..Origin::default() });
            o.renamed &= in_rename(hunks, &path, line);
            o.lines.push((path.clone(), line));
        }
    }
    out
}

/// Committer time and date of each origin, one `git show`.
fn dated(git: &Git, origins: &mut BTreeMap<String, Origin>) {
    let mut args = vec!["show", "-s", "--format=%H %ct %cs"];
    args.extend(origins.keys().map(String::as_str));
    for l in git.run(&args).unwrap_or_default().lines() {
        let mut f = l.split(' ');
        if let (Some(sha), Some(ct), Some(cs)) = (f.next(), f.next(), f.next())
            && let Some(o) = origins.get_mut(sha)
        {
            (o.secs, o.date) = (ct.parse().unwrap_or(i64::MAX), cs.to_string());
        }
    }
}

fn days(secs: i64) -> String {
    if secs % DAY == 0 { format!("{}d", secs / DAY) } else { format!("{}h", secs / 3_600) }
}

/// Why origin `o` gives no link, if it does not (module doc).
fn verdict(run: &Run, fix: &Fix, o: &Origin, why: Option<&Excerpt>) -> Result<(), String> {
    if o.sha == fix.sha {
        return Err("self".into());
    }
    if why.is_none() && !run.any_origin {
        return Err("no-reasoning".into());
    }
    if why.is_some_and(|e| !fix.session.is_empty() && e.session == fix.session) {
        return Err("same-session".into());
    }
    let age = fix.secs.saturating_sub(o.secs);
    if age < run.cfg.min_age_s {
        return Err(format!("too-young ({} < {})", days(age.max(0)), days(run.cfg.min_age_s)));
    }
    if run.seen.contains(&(o.sha.clone(), fix.sha.clone())) || why.is_some_and(|e| e.fixed_by.iter().any(|r| r.commit == fix.sha)) {
        return Err("duplicate".into());
    }
    if run.git.run(&["merge-base", "--is-ancestor", &o.sha, &fix.sha]).is_none() {
        return Err("not-ancestor".into());
    }
    Ok(())
}

/// The signals that say `fix` fixed something.
#[must_use]
pub fn signals_of(fix: &Fix) -> Vec<String> {
    let checks = [
        (signals::REVERT, signals::revert_signal(&fix.subject, &fix.body)),
        (signals::SUBJECT, signals::subject_signal(&fix.subject)),
        (signals::TESTS, fix.tests.as_deref().is_some_and(|t| signals::tests_signal(t, &fix.windows, fix.secs.saturating_mul(1000).saturating_add(999)))),
        (signals::WORDS, signals::words_signal(&fix.session_text)),
    ];
    checks.iter().filter(|(_, on)| *on).map(|(s, _)| (*s).to_string()).collect()
}

/// The files and line ranges of `lines`, at most `FILES_CAP` files of `RANGES_CAP` ranges each.
fn touched(lines: &[(String, u32)]) -> Vec<Touched> {
    let mut out: Vec<Touched> = Vec::new();
    for (path, _) in lines {
        if out.len() < FILES_CAP && !out.iter().any(|t| t.path == *path) {
            let mine: Vec<u32> = lines.iter().filter(|(p, _)| p == path).map(|(_, n)| *n).collect();
            out.push(Touched { path: path.clone(), lines: diff::ranges(&mine).into_iter().take(RANGES_CAP).collect() });
        }
    }
    out
}

fn repo_name(repo: &Path) -> String {
    repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn link(run: &Run, fix: &Fix, o: &Origin, why: Option<&Excerpt>, found: &[String]) -> Link {
    let age = fix.secs.saturating_sub(o.secs).max(0) / DAY;
    Link {
        kind: links::KIND.into(),
        commit: o.sha.clone(),
        fix: fix.sha.clone(),
        repo: repo_name(run.git.repo()),
        collective: run.scope.here.clone(),
        strength: if signals::strong(found) { links::FIX } else { links::REVISITED }.into(),
        signals: found.to_vec(),
        decision: Side {
            session: why.map(|e| e.session.clone()).unwrap_or_default(),
            task: why.map(|e| first_line(&e.task, TEXT_CAP)).unwrap_or_default(),
            subject: String::new(),
            date: o.date.clone(),
            // `dated` leaves i64::MAX for a time it could not parse.
            secs: if o.secs == i64::MAX { 0 } else { o.secs },
        },
        fixed: Side {
            session: fix.session.clone(),
            task: first_line(&fix.task, TEXT_CAP),
            subject: first_line(&fix.subject, TEXT_CAP),
            date: fix.date.clone(),
            secs: fix.secs,
        },
        files: touched(&o.lines),
        lines: o.lines.len(),
        age_days: u32::try_from(age).unwrap_or(u32::MAX),
        ms: learn::now_ms(),
    }
}

/// The links `fix` makes to the earlier decisions whose lines it changed, and every candidate left
/// out.
#[must_use]
pub fn detect(run: &Run, fix: &Fix) -> Detected {
    let mut d = Detected::default();
    let parent = match shape(run.git, run.cfg, &fix.sha) {
        Ok(p) => p,
        Err(reason) => {
            d.skip(&fix.sha, "", reason);
            return d;
        }
    };
    let hunks = changed(run.git, &parent, &fix.sha, &mut d);
    let mut found = origins(run.git, &parent, &hunks, &mut d);
    if found.is_empty() {
        return d;
    }
    dated(run.git, &mut found);
    let shas: Vec<String> = found.keys().cloned().collect();
    let why = reasoning::reasoning_for(run.git.repo(), &run.scope.store, &run.scope.here, &shas);
    let strength = signals_of(fix);
    // A fix that changed several decisions' lines is no fix of those it only renamed in.
    let several = found.len() > 1;
    for o in found.values() {
        let signals = if several && o.renamed { &[][..] } else { &strength[..] };
        match verdict(run, fix, o, why.get(&o.sha)) {
            Ok(()) => d.links.push(link(run, fix, o, why.get(&o.sha), signals)),
            Err(reason) => d.skip(&fix.sha, &o.sha, reason),
        }
    }
    d
}

/// One link per (decision, fix session), from the first (newest) fix; then at most `max`, strong
/// ones first, then by lines changed. The rest are skipped as `duplicate` or `cap`.
#[must_use]
pub fn dedupe_cap(d: Detected, max: usize) -> Detected {
    let Detected { links, mut skipped } = d;
    let mut keys = HashSet::new();
    let mut kept: Vec<Link> = Vec::new();
    for l in links {
        let by = if l.fixed.session.is_empty() { &l.fix } else { &l.fixed.session };
        if keys.insert((l.commit.clone(), by.clone())) {
            kept.push(l);
        } else {
            skipped.push(Skipped { fix: l.fix, origin: l.commit, reason: "duplicate".into() });
        }
    }
    kept.sort_by_key(|l| (!l.strong(), std::cmp::Reverse(l.lines)));
    for l in kept.split_off(max.min(kept.len())) {
        skipped.push(Skipped { fix: l.fix, origin: l.commit, reason: "cap".into() });
    }
    Detected { links: kept, skipped }
}

/// The link records in `store`'s reasoning.jsonl that are visible in `here`.
fn stored_links(store: &Path, here: &str) -> Vec<Link> {
    let text = std::fs::read_to_string(store.join("reasoning.jsonl")).unwrap_or_default();
    links::records(&text).into_iter().filter(|l| reasoning::visible(&l.collective, here)).collect()
}

/// The links in `store` whose decision or fix is one of `shas` (for `borg why`).
pub fn links_of<S: BuildHasher>(store: &Path, here: &str, shas: &HashSet<&str, S>) -> Vec<Link> {
    stored_links(store, here).into_iter().filter(|l| shas.contains(l.commit.as_str()) || shas.contains(l.fix.as_str())).collect()
}

fn seen_in(store: &Path, here: &str) -> HashSet<(String, String)> {
    stored_links(store, here).into_iter().map(|l| (l.commit, l.fix)).collect()
}

/// The fix as git describes `sha` (subject, body, committer time and date, author email).
fn commit_info(git: &Git, sha: &str) -> Option<Fix> {
    let text = git.run(&["show", "-s", "--format=%ct%x00%cs%x00%ae%x00%s%x00%b", sha])?;
    let f: Vec<&str> = text.splitn(5, '\0').collect();
    let [secs, date, email, subject, body] = f[..] else { return None };
    Some(Fix {
        sha: sha.into(),
        secs: secs.trim().parse().ok()?,
        date: date.into(),
        email: email.into(),
        subject: subject.into(),
        body: body.into(),
        ..Fix::default()
    })
}

/// `<state>/<session>.<ext>` of the landed session, if there is one.
pub(crate) fn session_file(l: &notes::Landed, ext: &str) -> Option<String> {
    std::fs::read_to_string(l.dir.join(format!("{}.{ext}", notes::safe(l.session)))).ok()
}

/// What a session left in the state dir that detection reads.
fn session_fix(l: &notes::Landed, git: &Git, sha: &str) -> Option<Fix> {
    let read = |ext: &str| session_file(l, ext);
    let task = read("task").unwrap_or_default();
    Some(Fix {
        session: l.session.into(),
        session_text: format!("{}\n{task}", read("prompts").unwrap_or_default()),
        task,
        tests: read("tests"),
        windows: l.start.map(|st| notes::windows(l.dir, l.session, st)).unwrap_or_default(),
        ..commit_info(git, sha)?
    })
}

/// The links the session's landed commits make (newest first, deduplicated and capped), without
/// writing anything.
#[must_use]
pub fn session_links(l: &notes::Landed, scope: &Scope, cfg: &Cfg) -> Detected {
    links_with(&cfg.git(l.repo), l, scope, cfg)
}

/// `session_links` on `git`, inside its deadline.
#[must_use]
pub fn links_with(git: &Git, l: &notes::Landed, scope: &Scope, cfg: &Cfg) -> Detected {
    let seen = seen_in(&scope.store, &scope.here);
    let run = Run { git, scope, cfg, any_origin: false, seen: &seen };
    let mut all = Detected::default();
    for c in l.commits {
        let Some(fix) = session_fix(l, git, c) else { continue };
        let d = detect(&run, &fix);
        all.links.extend(d.links);
        all.skipped.extend(d.skipped);
    }
    dedupe_cap(all, cfg.max_links)
}

/// What `record` wrote.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Recorded {
    pub links: usize,
    pub strong: usize,
    /// Records appended to the store (0 for a local collective).
    pub replicated: usize,
    /// Regret episodes written (`Mode::Learn` only).
    pub episodes: usize,
    pub errors: Vec<String>,
}

/// Adds `r` to the note of `commit` (`fixed_by` on the decision, `fixes` on the fix); whether the
/// note changed. Without a note, the decision's is restored from the store first; the fix's is then
/// left alone.
fn add_ref(repo: &Path, scope: &Scope, commit: &str, r: LinkRef, decision: bool) -> Result<bool, String> {
    let e = match notes::note_of(repo, commit) {
        Some(e) => Some(e),
        None if decision => reasoning::stored_of(&scope.store, &scope.here, &HashSet::from([commit])).remove(commit),
        None => None,
    };
    let Some(mut e) = e.filter(|e| reasoning::visible(&e.collective, &scope.here)) else { return Ok(false) };
    let list = if decision { &mut e.fixed_by } else { &mut e.fixes };
    if links::push_ref(list, r) { notes::write_note(repo, &e).map(|()| true) } else { Ok(false) }
}

/// Writes `links`: both notes, and the store's record when the collective may sync. A failure in
/// one is collected and the rest still go out.
pub fn record(repo: &Path, scope: &Scope, links: &[Link]) -> Recorded {
    let mut done = Recorded::default();
    for l in links {
        let (fixed_by, fixes) = links::refs(l);
        let results = [add_ref(repo, scope, &l.commit, fixed_by, true), add_ref(repo, scope, &l.fix, fixes, false)];
        done.errors.extend(results.into_iter().filter_map(Result::err));
        if scope.sync {
            match learn::append_jsonl(&scope.store.join("reasoning.jsonl"), l) {
                Ok(()) => done.replicated += 1,
                Err(e) => done.errors.push(format!("{}: {e}", scope.store.display())),
            }
        }
        done.links += 1;
        done.strong += usize::from(l.strong());
    }
    done
}

/// The fix `sha` as replay sees it: session and task from its note, if any.
fn replay_fix(git: &Git, sha: &str) -> Option<Fix> {
    let note = notes::note_of(git.repo(), sha);
    let task = note.as_ref().map(|e| e.task.clone()).unwrap_or_default();
    Some(Fix { session: note.map(|e| e.session).unwrap_or_default(), session_text: task.clone(), task, ..commit_info(git, sha)? })
}

/// What a replay found.
#[derive(Debug, Default)]
pub struct Replayed {
    pub found: Detected,
    pub scanned: usize,
    /// More commits were in the range than were scanned.
    pub capped: bool,
    pub shallow: bool,
}

/// At most this many commits of a replayed range are scanned. Guard: `borg notes regress --replay`
/// over a long history ends in bounded time; a longer range says it was cut.
pub const REPLAY_CAP: usize = 500;

/// Every non-merge commit of `range` taken for a fix (dry run: nothing is written). Each commit
/// gets the run's budget of its own, and the cap applies per fix commit.
///
/// # Errors
/// When `range` names no commits git can list.
pub fn replay(repo: &Path, range: &str, scope: &Scope, cfg: &Cfg, any_origin: bool) -> Result<Replayed, String> {
    let list = cfg.git(repo).run(&["rev-list", "--no-merges", "--reverse", range, "--"]).ok_or(format!("{range} is not a commit range here"))?;
    let commits: Vec<&str> = list.lines().filter(|l| !l.is_empty()).collect();
    let seen = seen_in(&scope.store, &scope.here);
    let mut out = Replayed { capped: commits.len() > REPLAY_CAP, ..Replayed::default() };
    for sha in commits.into_iter().take(REPLAY_CAP) {
        let git = cfg.git(repo);
        out.shallow |= out.scanned == 0 && !shallow_roots(&git).is_empty();
        let run = Run { git: &git, scope, cfg, any_origin, seen: &seen };
        let Some(fix) = replay_fix(&git, sha) else { continue };
        let d = dedupe_cap(detect(&run, &fix), cfg.max_links);
        out.found.links.extend(d.links);
        out.found.skipped.extend(d.skipped);
        out.scanned += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(commit: &str, fix: &str, session: &str, strong: bool, lines: usize) -> Link {
        Link {
            commit: commit.into(),
            fix: fix.into(),
            strength: if strong { links::FIX } else { links::REVISITED }.into(),
            fixed: Side { session: session.into(), ..Side::default() },
            lines,
            ..Link::default()
        }
    }

    #[test]
    fn dedupe_then_cap_strong_first() {
        let mut links = vec![l("d1", "f2", "s-7", false, 1), l("d1", "f1", "s-7", true, 9)];
        links.extend((2..8).map(|i| l(&format!("d{i}"), "f2", "s-7", i % 2 == 0, i)));
        let d = dedupe_cap(Detected { links, skipped: vec![] }, 5);
        let kept: Vec<(&str, bool)> = d.links.iter().map(|x| (x.commit.as_str(), x.strong())).collect();
        assert_eq!(kept, [("d6", true), ("d4", true), ("d2", true), ("d7", false), ("d5", false)]);
        let reasons: Vec<(&str, &str)> = d.skipped.iter().map(|s| (s.origin.as_str(), s.reason.as_str())).collect();
        assert_eq!(reasons, [("d1", "duplicate"), ("d3", "cap"), ("d1", "cap")], "the newest fix of a session keeps the link");
    }

    #[test]
    fn cfg_from_env_defaults() {
        let d = Cfg::default();
        assert_eq!((d.mode, d.min_age_s, d.max_links, d.max_files, d.max_lines), (Mode::Links, 172_800, 5, 40, 2000));
        assert_eq!(days(172_800), "2d");
        assert_eq!(days(43_200), "12h");
    }
}
