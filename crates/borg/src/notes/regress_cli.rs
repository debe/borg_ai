//! `borg regress`: the decision → fix links regress.rs would make, as a dry run (nothing is
//! written: no note, no record).
//!
//! Without `--replay`, the latest session that started in this repo (or `--session ID`), over the
//! commits it landed. `--replay <A..B>` takes every non-merge commit of the range for a fix (at most
//! `regress::REPLAY_CAP`), its session and task from its note; `--any-origin` keeps origins without
//! borg reasoning, which measures the detector on a repo without notes. `--explain` lists every
//! candidate left out and why, `--json` prints `{"links", "skipped", "scanned"}`.
//!
//! **Privacy** as `borg why`: an off cwd, an invalid config, or a repo that routes elsewhere prints
//! nothing and exits 0.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::collective::{self, Config, Route};
use crate::learn;
use crate::notes::links::Link;
use crate::notes::regress::{self, Detected, Scope, Skipped};
use crate::notes::{self, redact};

/// Its flags (completions.rs).
pub const FLAGS: &[&str] = &["--session", "--replay", "--any-origin", "--explain", "--json", "--dir"];

const USAGE: &str = "usage: borg regress [--session ID] [--replay <A..B>] [--any-origin] [--explain] [--json] [--dir DIR]";

#[derive(Debug, Default, PartialEq, Eq)]
struct Opts {
    session: Option<String>,
    replay: Option<String>,
    json: bool,
    explain: bool,
    any_origin: bool,
    dir: Option<String>,
}

fn parse(args: &[&str]) -> Option<Opts> {
    let mut o = Opts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().map(|v| (*v).to_string());
        match *a {
            "--json" => o.json = true,
            "--explain" => o.explain = true,
            "--any-origin" => o.any_origin = true,
            "--session" => o.session = Some(value()?),
            "--replay" => o.replay = Some(value()?),
            "--dir" => o.dir = Some(value()?),
            _ => return None,
        }
    }
    Some(o)
}

/// What a dry run found.
#[derive(Debug, Default, serde::Serialize)]
pub struct Report {
    pub links: Vec<Link>,
    pub skipped: Vec<Skipped>,
    pub scanned: usize,
    #[serde(skip)]
    pub notes: Vec<String>,
}

fn resolve(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The repo `cwd` is in and its collective, or `None` when either routes off or they route apart.
fn scoped(cfg: Option<&Config>, base: &Path, cwd: &str) -> Result<Option<(PathBuf, Route)>, String> {
    let cwd = resolve(Path::new(cwd)).to_string_lossy().into_owned();
    let route = collective::route(cfg, &cwd, base);
    if route == Route::Off {
        return Ok(None);
    }
    let repo = notes::toplevel(&cwd).ok_or(format!("{cwd} is not in a git repo"))?;
    let repo = resolve(&repo);
    Ok((collective::route(cfg, &repo.to_string_lossy(), base) == route).then_some((repo, route)))
}

/// The links the latest (or the named) session's landed commits would make.
fn session(repo: &Path, scope: &Scope, route: &Route, o: &Opts, cfg: &regress::Cfg) -> Result<Report, String> {
    let dir = std::env::var_os("BORG_STATE").map_or_else(|| scope.store.join("state"), PathBuf::from);
    let here = repo.to_string_lossy();
    let (session, start) = match &o.session {
        Some(s) => (s.clone(), std::fs::read_to_string(dir.join(format!("{s}.head"))).ok().and_then(|t| serde_json::from_str::<notes::Start>(&t).ok())),
        None => notes::latest_session(&dir, &here).map(|(s, st)| (s, Some(st))).ok_or("no drone session started in this repo; pass --session ID")?,
    };
    let start = start.filter(|st| st.repo == here).ok_or("the session did not start in this repo")?;
    let commits = notes::landed(repo, &start, &notes::windows(&dir, &session, &start));
    let landed = notes::Landed { dir: &dir, session: &session, repo, store: &scope.store, route, commits: &commits, start: Some(&start) };
    let Detected { links, skipped } = regress::session_links(&landed, scope, cfg);
    Ok(Report { links, skipped, scanned: commits.len(), notes: Vec::new() })
}

fn replayed(repo: &Path, scope: &Scope, range: &str, o: &Opts, cfg: &regress::Cfg) -> Result<Report, String> {
    let r = regress::replay(repo, range, scope, cfg, o.any_origin)?;
    let mut notes = Vec::new();
    if r.capped {
        notes.push(format!("only the first {} commits of {range} were scanned", regress::REPLAY_CAP));
    }
    if r.shallow {
        notes.push("repo is shallow: links stop at the graft".into());
    }
    Ok(Report { links: r.found.links, skipped: r.found.skipped, scanned: r.scanned, notes })
}

/// The dry run from `cwd`, or `None` when it routes off or elsewhere (module doc).
///
/// # Errors
/// Bad arguments, a cwd outside git, a session that did not start here, a range git cannot list.
pub fn run(cfg: Option<&Config>, base: &Path, cwd: &str, args: &[&str]) -> Result<Option<String>, String> {
    let o = parse(args).ok_or(USAGE)?;
    let Some((repo, route)) = scoped(cfg, base, o.dir.as_deref().unwrap_or(cwd))? else { return Ok(None) };
    let Route::Collective { home, .. } = &route else { return Ok(None) };
    let scope = Scope::of(&route, home).ok_or(USAGE)?;
    let rcfg = regress::Cfg::from_env();
    let report = match &o.replay {
        Some(range) => replayed(&repo, &scope, range, &o, &rcfg)?,
        None => session(&repo, &scope, &route, &o, &rcfg)?,
    };
    Ok(Some(if o.json { serde_json::to_string(&report).map_err(|e| e.to_string())? } else { render(&report, o.explain) }))
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn spans(l: &Link) -> String {
    let span = |(a, b): &(u32, u32)| if a == b { a.to_string() } else { format!("{a}-{b}") };
    l.files.iter().map(|t| format!("{}:{}", t.path, t.lines.iter().map(span).collect::<Vec<_>>().join(","))).collect::<Vec<_>>().join(" ")
}

fn link_line(l: &Link) -> String {
    let (verb, strength) = if l.strong() { ("fixes", format!("fix [{}]", l.signals.join(","))) } else { ("revisits", "revisited".to_string()) };
    let mut s = format!(
        "{} {} {:?} {verb} {} {} ({}d)  {strength}  {} line(s)  {}\n",
        short(&l.fix),
        l.fixed.date,
        redact(&l.fixed.subject),
        short(&l.commit),
        l.decision.date,
        l.age_days,
        l.lines,
        spans(l)
    );
    for (label, task) in [("decided:", &l.decision.task), ("fixed:  ", &l.fixed.task)] {
        if !task.is_empty() {
            let _ = writeln!(s, "  {label} {}", redact(task));
        }
    }
    s
}

/// The report as text: one line per link, then (`explain`) every candidate left out.
pub fn render(r: &Report, explain: bool) -> String {
    let mut s: String = r.links.iter().map(link_line).collect();
    if explain && !r.skipped.is_empty() {
        s.push_str("--explain:\n");
        for k in &r.skipped {
            let from = match k.origin.len() {
                0 => String::new(),
                40 => format!(" <- {}", short(&k.origin)),
                _ => format!(" <- {}", k.origin),
            };
            let _ = writeln!(s, "  skip {}{from}: {}", short(&k.fix), k.reason);
        }
    }
    for n in &r.notes {
        let _ = writeln!(s, "{n}");
    }
    let strong = r.links.iter().filter(|l| l.strong()).count();
    let _ = writeln!(s, "{} link(s), {strong} fix; {} commit(s) scanned (dry run: nothing written)", r.links.len(), r.scanned);
    s
}

/// `borg regress [--session ID] [--replay <A..B>] [--any-origin] [--explain] [--json] [--dir DIR]`.
#[must_use]
pub fn main(args: &[&str]) -> i32 {
    if parse(args).is_none() {
        eprintln!("{USAGE}");
        return 2;
    }
    let cwd = std::env::current_dir().unwrap_or_default().display().to_string();
    // An invalid config routes everything off: nothing is shown.
    let Ok(cfg) = collective::load() else { return 0 };
    match run(cfg.as_ref(), &learn::home(), &cwd, args) {
        Ok(None) => 0,
        Ok(Some(out)) => {
            print!("{out}");
            if !out.ends_with('\n') {
                println!();
            }
            0
        }
        Err(e) => {
            eprintln!("borg regress: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags() {
        let o = parse(&["--replay", "a..b", "--json", "--explain", "--any-origin", "--dir", "/r"]).unwrap();
        assert_eq!((o.replay.as_deref(), o.json, o.explain, o.any_origin, o.dir.as_deref()), (Some("a..b"), true, true, true, Some("/r")));
        assert!(parse(&["--replay"]).is_none() && parse(&["--bogus"]).is_none());
        assert_eq!(parse(&[]).unwrap(), Opts::default());
    }
}
