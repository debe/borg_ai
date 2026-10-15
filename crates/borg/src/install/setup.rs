//! `borg setup`: from a fresh install to a working borg in one guided command.
//!
//! usage: borg setup [--yes] [--no-vectors] [--harness LIST] [--private DIR...]
//!                   [--team NAME --paths DIR... [--relay URL|disabled] | --join TICKET [--paths DIR...]] [--home DIR]
//!
//! Steps, each saying what it will do: what is here (harness CLIs, the model CLI, git identity, an
//! existing config), the collectives (setup/collectives.rs: personal, team or company, off paths), the
//! wiring (`install::plan`: hooks or adapter, and the `borg` skill). Then one plan: the config diff, the
//! mesh command, the vector model, the wiring diff, and nothing is written before the engineer confirms
//! it. After that (setup/steps.rs): the config, with a backup (setup/config.rs: an existing one is only
//! ever extended); `borg mesh init` or `join`; the vector model and indexes, on by default (a failure is a
//! WARN, never the end of setup); the wiring; an optional import; `borg doctor`.
//!
//! Questions read stdin (setup/prompt.rs); without a terminal setup refuses unless `--yes` takes every
//! default. Flags answer their question. A second run changes nothing. `--home DIR` isolates as `borg
//! install --home` does (`super::home_overrides`), so a test never reaches a real config or store.

mod collectives;
mod config;
mod prompt;
mod steps;

use std::io::IsTerminal;
use std::path::Path;

use crate::harness::{self, Harness};
use crate::install::{self, Action, Change, Targets};
use crate::{collective, learn, runner};
use collectives::Mesh;
use prompt::Prompt;

const USAGE: &str = "usage: borg setup [--yes] [--no-vectors] [--harness HARNESS,...|all] [--private DIR...] \
[--team NAME --paths DIR... [--relay URL|disabled]] [--join TICKET [--paths DIR...]] [--home DIR]";

/// The flags (completions.rs lists these).
pub const FLAGS: &[&str] = &["--yes", "--no-vectors", "--harness", "--private", "--team", "--paths", "--relay", "--join", "--home"];

/// The flags; each answers its question.
#[derive(Debug, Default)]
pub struct Opts {
    yes: bool,
    no_vectors: bool,
    harness: Option<String>,
    join: Option<String>,
    team: Option<String>,
    relay: Option<String>,
    home: Option<String>,
    /// Some when `--private` was given.
    private: Option<Vec<String>>,
    paths: Vec<String>,
}

impl Opts {
    /// Sets `flag` from the arguments after it; how many of them it took.
    fn set(&mut self, flag: &str, rest: &[&str]) -> Result<usize, String> {
        let values: Vec<String> = rest.iter().take_while(|a| !a.starts_with("--")).map(|a| (*a).to_string()).collect();
        match flag {
            "--yes" | "-y" => self.yes = true,
            "--no-vectors" => self.no_vectors = true,
            "--private" => self.private = Some(values.clone()),
            "--paths" => self.paths.clone_from(&values),
            _ => {
                let slot = self.value(flag).ok_or(format!("unknown flag {flag}"))?;
                *slot = Some(values.first().ok_or(format!("{flag} needs a value"))?.clone());
                return Ok(1);
            }
        }
        Ok(if matches!(flag, "--private" | "--paths") { values.len() } else { 0 })
    }

    fn value(&mut self, flag: &str) -> Option<&mut Option<String>> {
        match flag {
            "--harness" => Some(&mut self.harness),
            "--join" => Some(&mut self.join),
            "--team" => Some(&mut self.team),
            "--relay" => Some(&mut self.relay),
            "--home" => Some(&mut self.home),
            _ => None,
        }
    }

    /// Flags that only make sense together.
    fn check(&self) -> Result<(), String> {
        if self.team.is_some() && self.join.is_some() {
            return Err("--team founds a collective, --join joins one: not both".into());
        }
        if !self.paths.is_empty() && self.team.is_none() && self.join.is_none() {
            return Err("--paths names a team collective's repos: with --team or --join".into());
        }
        if self.yes && self.team.is_some() && self.paths.is_empty() {
            return Err("--yes --team NAME needs --paths DIR...: a team collective without repo directories routes no repo".into());
        }
        self.values()
    }

    /// The values: a relay only for a founded collective, an http(s) one; a ticket that looks like one.
    fn values(&self) -> Result<(), String> {
        if self.relay.is_some() && self.team.is_none() {
            return Err("--relay is the founded collective's (--team); a joiner gets it from the ticket".into());
        }
        self.join.as_deref().map_or(Ok(()), collectives::ticket)?;
        self.relay.as_deref().map_or(Ok(None), collectives::relay_value).map(drop)
    }
}

fn parse(args: &[&str]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut i = 0;
    while let Some(flag) = args.get(i) {
        i += 1 + o.set(flag, &args[i + 1..])?;
    }
    o.check()?;
    Ok(o)
}

/// `borg setup`; the exit code.
#[must_use]
pub fn main(args: &[&str]) -> i32 {
    if args.iter().any(|a| matches!(*a, "-h" | "--help")) {
        println!("{USAGE}");
        return 0;
    }
    let o = match parse(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("borg setup: {e}\n{USAGE}");
            return 2;
        }
    };
    if !o.yes && !std::io::stdin().is_terminal() {
        eprintln!("borg setup asks questions and needs a terminal; --yes takes every default without asking");
        return 2;
    }
    if let Some(h) = &o.home {
        install::isolate("setup", Path::new(h));
    }
    let mut stdin = std::io::stdin().lock();
    let mut ask = Prompt::new(o.yes, &mut stdin);
    run(&o, &mut ask).unwrap_or_else(|e| {
        eprintln!("borg setup: {e}");
        1
    })
}

/// A harness and the changes that wire it.
type Wire = (&'static dyn Harness, Vec<Change>);

/// Everything setup will do, shown before the confirmation.
struct Plan {
    config: Option<Change>,
    mesh: Option<Mesh>,
    /// What the vector step will do, if anything.
    vectors: Option<String>,
    wiring: Vec<Wire>,
}

fn run(o: &Opts, ask: &mut Prompt) -> Result<i32, String> {
    let t = o.home.as_deref().map_or_else(Targets::from_env, |h| Targets::under(Path::new(h)));
    ask.heading("1. What is here");
    let found = detect(&t);
    ask.heading("2. Collectives");
    let (want, mesh) = collectives::choose(o, ask, collective::config_path().exists())?;
    let config = config::change(&collective::config_path(), &want)?;
    ask.heading("3. Wiring");
    let wiring = wiring(&harnesses(o, ask, &found)?, &t)?;
    let mesh = mesh.filter(|m| !steps::done(m));
    let plan = Plan { config, mesh, vectors: steps::vectors_plan(o.no_vectors), wiring };
    ask.heading("4. The plan");
    if !show(&plan, &t) {
        println!("nothing to change");
    } else if !ask.yes_no("Do all of this?", true) {
        println!("nothing written");
        return Ok(0);
    }
    let stamp = format!("{}Z", install::utc(learn::now_ms()));
    let mut warns = Vec::new();
    // Wiring without the config would put every repo into the implicit `default` team collective.
    let (code, mesh) = match plan.config.as_ref().map_or(0, |c| steps::config(c, &stamp)) {
        0 => act(o, &plan, ask, &stamp, &mut warns),
        code => {
            warns
                .push("nothing else was done (mesh, vectors, wiring): the collectives config could not be written; fix that and run `borg setup` again".into());
            (code, None)
        }
    };
    ask.heading("5. Check");
    Ok(verify(o, &warns, mesh).max(code))
}

/// After the config is written: the mesh command, the vectors, the wiring, the import. The exit code (1
/// for a failed mesh command or write) and the mesh command that finished.
fn act<'p>(o: &Opts, plan: &'p Plan, ask: &mut Prompt, stamp: &str, warns: &mut Vec<String>) -> (i32, Option<&'p Mesh>) {
    let mesh = plan.mesh.as_ref().filter(|m| steps::mesh(m, ask, warns));
    let failed = i32::from(plan.mesh.is_some() && mesh.is_none());
    steps::vectors(o.no_vectors, ask, warns);
    let code = steps::wire(&plan.wiring, stamp).max(failed);
    steps::import(ask);
    (code, mesh)
}

/// Step 1: harness CLIs on PATH (the ones wired by default), the model CLI, git identity, the config.
fn detect(t: &Targets) -> Vec<&'static dyn Harness> {
    let found: Vec<&'static dyn Harness> = harness::visible().filter(|h| install::which(h.probe().cli).is_some()).collect();
    for h in harness::visible() {
        let cli = if found.iter().any(|f| f.name() == h.name()) { "installed" } else { "not installed" };
        println!("{:<9} {cli}; {}", h.name(), install::wiring(h, t));
    }
    let spec = std::env::var("BORG_PREDICT_MODEL").ok();
    println!("{}", runner::availability(spec.as_deref(), &runner::on_path).unwrap_or_else(|e| e));
    let cwd = std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default();
    match learn::engineer(&cwd) {
        e if e.is_empty() => println!("git: no user.email; lessons will name no author (git config --global user.email ...)"),
        e => println!("git: you are {e}"),
    }
    let path = collective::config_path();
    if path.exists() {
        println!("config: {} exists; setup only adds to it", path.display());
    } else {
        println!("config: none yet at {}", path.display());
    }
    found
}

/// The harnesses to wire: `--harness` (an unknown name is an error), else the answer (asked again after
/// an unknown one), by default the installed ones.
fn harnesses(o: &Opts, ask: &mut Prompt, found: &[&'static dyn Harness]) -> Result<Vec<&'static dyn Harness>, String> {
    if let Some(h) = &o.harness {
        println!("wire borg into: {h}");
        return pick(h);
    }
    let installed = found.iter().map(|h| h.name()).collect::<Vec<_>>().join(",");
    let q = format!("Wire borg into ({}, all, or - for none)", harness::names(", "));
    loop {
        match pick(&ask.line(&q, if installed.is_empty() { "-" } else { &installed })) {
            Err(e) if !ask.yes() => println!("{e}"),
            picked => return picked,
        }
    }
}

/// The harnesses an answer names (`all`, names, `-`), each once.
fn pick(answer: &str) -> Result<Vec<&'static dyn Harness>, String> {
    let mut out: Vec<&'static dyn Harness> = Vec::new();
    for w in prompt::split(answer) {
        let hs: Vec<_> = if w == "all" { harness::visible().collect() } else { vec![harness::by_name(&w).ok_or(format!("unknown harness {w:?}"))?] };
        out.extend(hs.into_iter().filter(|h| !out.iter().any(|o| o.name() == h.name())).collect::<Vec<_>>());
    }
    Ok(out)
}

/// Step 3: each harness's install plan (hooks or adapter, and the skill); its notes printed now.
fn wiring(hs: &[&'static dyn Harness], t: &Targets) -> Result<Vec<Wire>, String> {
    let bin = install::bin();
    let mut out = Vec::new();
    for &h in hs {
        let (changes, notes) = install::plan(Action::Install, h, t, &bin).map_err(|e| format!("{}: {e}", h.name()))?;
        for n in &notes {
            println!("{}: {n}", h.name());
        }
        if changes.is_empty() {
            println!("{}: already wired", h.name());
        } else {
            out.push((h, changes));
        }
    }
    Ok(out)
}

/// Prints the plan; whether there is anything to do. Files that are borg's own text (the skill's, a new
/// adapter) get one line each, not their diff (`borg install` shows it); the engineer's files get a diff.
fn show(p: &Plan, t: &Targets) -> bool {
    p.config.iter().for_each(|c| print!("{}", install::diff(&c.path, c.before.as_deref(), c.after.as_deref())));
    for (h, changes) in &p.wiring {
        let skill = t.skill_dir(*h);
        for c in changes {
            if own(*h, c, skill.as_deref()) {
                let n = c.after.as_deref().map_or(0, |a| a.lines().count());
                println!("{}: {} ({n} lines of borg's own; `borg install {}` shows them)", h.name(), c.path.display(), h.name());
            } else {
                print!("{}", install::diff(&c.path, c.before.as_deref(), c.after.as_deref()));
            }
        }
    }
    if let Some(m) = &p.mesh {
        println!("then: borg mesh {}", m.args().join(" "));
    }
    p.vectors.iter().for_each(|v| println!("then: {v}"));
    p.config.is_some() || !p.wiring.is_empty() || p.mesh.is_some() || p.vectors.is_some()
}

/// Whether `c` writes borg's own compiled-in text: a skill file, or an adapter where there was none.
fn own(h: &dyn Harness, c: &Change, skill: Option<&Path>) -> bool {
    skill.is_some_and(|d| c.path.starts_with(d)) || (c.before.is_none() && c.after.as_deref() == Some(h.wiring().adapter()))
}

/// Step 5: `borg doctor`, the WARNs of the steps above, what to do next. Doctor's exit code.
fn verify(o: &Opts, warns: &[String], mesh: Option<&Mesh>) -> i32 {
    let home: Vec<&str> = o.home.as_deref().map(|h| vec!["--home", h]).unwrap_or_default();
    let code = install::main("doctor", &home);
    for w in warns {
        println!("WARN {w}");
    }
    println!("\nnext:\n  borg status                 what each collective holds\n  borg collective [DIR]       which collective a repo belongs to");
    match mesh {
        Some(Mesh::Init(name)) => {
            println!("  borg mesh serve --collective {name}   keep it running (a service); then `borg mesh invite --collective {name}`");
        }
        Some(Mesh::Join { .. }) => println!("  borg mesh serve             keep it running inside one of the team's repos"),
        None => {}
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_parse_and_conflicts_are_refused() {
        let o = parse(&["--yes", "--private", "~/a", "~/b", "--team", "corp", "--paths", "~/w", "--relay", "disabled", "--no-vectors"]).unwrap();
        assert!(o.yes && o.no_vectors);
        assert_eq!(o.private.as_deref(), Some(&["~/a".to_string(), "~/b".to_string()][..]));
        assert_eq!((o.team.as_deref(), o.paths.as_slice(), o.relay.as_deref()), (Some("corp"), &["~/w".to_string()][..], Some("disabled")));
        assert!(parse(&["--team", "a", "--join", "BORG1X"]).is_err());
        assert!(parse(&["--paths", "~/w"]).is_err());
        assert!(parse(&["--join", "BORG1X", "--relay", "disabled"]).is_err());
        assert!(parse(&["--team", "a", "--relay", "relay.corp"]).is_err());
        assert!(parse(&["--yes", "--team", "a"]).unwrap_err().contains("--paths"), "--yes founds no team without repos");
        assert!(parse(&["--join", "notaticket"]).unwrap_err().contains("BORG1"));
        assert!(parse(&["--team"]).unwrap_err().contains("needs a value"));
        assert!(parse(&["--bogus"]).unwrap_err().contains("unknown flag"));
    }

    #[test]
    fn every_listed_flag_parses() {
        for f in FLAGS {
            let rest: &[&str] = if matches!(*f, "--yes" | "--no-vectors") { &[] } else { &["x"] };
            let mut o = Opts::default();
            assert!(o.set(f, rest).is_ok(), "{f}");
        }
    }
}
