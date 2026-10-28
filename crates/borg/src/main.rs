//! borg: a shared memory for coding agents.
//!
//! The command line. Every command, its help rows and its completions come from one table
//! (completions.rs); `borg help` lists them and `borg debug help` the evaluation tools. Hooks run
//! `borg hook <harness> <event>` with the harness's event on stdin; every harness speaks one canonical,
//! Claude-shaped event and gets Claude-shaped output, adapters translate both ways. The environment
//! variables are in docs/configuration.md.
//!
//! Hooks always fail open: any error lets the drone continue untouched.

use std::io::Read;

use borg::{collective, conscience, expert, harness, install, learn, outcome, prefilter};
use serde_json::{Value, json};

/// The search facets (facets.rs) of the lessons a learn run appended to `store` after byte `from`: one
/// model call each, the new wording of a merge included (a merge clears the old facets).
fn facets_for_written(store: &std::path::Path, from: u64) {
    let ids = borg::facets::appended_ids(store, from);
    if ids.is_empty() || !borg::facets::enabled() {
        return;
    }
    let r = borg::facets::fill(store, Some(&ids), true, &|prompt: &str, schema: &Value| learn::ask_model(prompt, schema), 4, learn::now_ms());
    if r.written > 0 || !r.failed.is_empty() {
        println!(
            "facets written for {} lessons (${:.4}){}",
            r.written,
            r.cost_usd,
            if r.failed.is_empty() { String::new() } else { format!("; {} failed, `borg facets --apply` retries", r.failed.len()) }
        );
    }
}

/// Whether file descriptor `fd` is a terminal (bold headings only there, and never with NO_COLOR).
fn atty(fd: i32) -> bool {
    // SAFETY: isatty only inspects the descriptor.
    std::env::var_os("NO_COLOR").is_none() && unsafe { libc::isatty(fd) } == 1
}

/// `borg help` (`debug`: `borg debug help`): the table's commands by group, one line per help row.
/// `HARNESS` stands for the harnesses' names (harness/mod.rs `ALL`).
fn help(color: bool, debug: bool) -> String {
    use borg::completions::{Command, DEBUG, GROUPS};
    let bold = |t: &str| if color { format!("\x1b[1m{t}\x1b[0m") } else { t.to_string() };
    let names = harness::names("|");
    let line = |c: &Command, args: &str| format!("{} {args}", c.name).trim_end().replace("HARNESS", &names);
    let groups: Vec<(&str, &[Command])> = if debug { vec![("Debugging and evaluation", DEBUG)] } else { GROUPS.to_vec() };
    let width = groups.iter().flat_map(|(_, cmds)| cmds.iter().flat_map(|c| c.help.iter().map(|(args, _)| line(c, args).chars().count()))).max().unwrap_or(0);
    let (title, usage) = match debug {
        true => ("borg debug: evaluation and debugging tools, not for everyday use", "borg debug <command> [options]"),
        false => ("borg: a shared memory for coding agents", "borg <command> [options]"),
    };
    let mut out = format!("{}\n\n{} {usage}\n", bold(title), bold("Usage:"));
    for (group, cmds) in &groups {
        out.push_str(&format!("\n{}\n", bold(group)));
        for c in *cmds {
            for (args, what) in c.help {
                out.push_str(&format!("  {:<width$}  {what}\n", line(c, args)));
            }
        }
    }
    out.push('\n');
    if !debug {
        out.push_str("Evaluation and debugging tools: borg debug help\n");
    }
    out.push_str("More: docs/commands.md, or https://github.com/debe/borg_ai\n");
    out
}

/// Runs `borg-<part>` (beside this borg, else on PATH) in place of this process.
fn exec(part: &str, args: &[&str]) -> ! {
    use std::os::unix::process::CommandExt;
    let bin = format!("borg-{part}");
    let err = std::process::Command::new(borg::install::sibling(part).unwrap_or_else(|| bin.clone().into())).args(args).exec();
    eprintln!("borg {part}: cannot run {bin} ({err}); cargo install {bin} --locked");
    std::process::exit(1);
}

/// `borg collective [DIR]`: which collective a directory belongs to (default: the current one), and
/// whether it may sync. Routing compares path text, so a relative argument ("." or "../x") is made
/// absolute first, as hooks' cwd is.
fn collective_cmd(rest: &[&str]) {
    let dir = rest.first().map_or_else(|| std::env::current_dir().unwrap_or_default(), std::path::PathBuf::from);
    let cwd = std::path::absolute(&dir).unwrap_or(dir).display().to_string();
    match collective::load() {
        Err(e) => println!("off (invalid config: {e})"),
        Ok(cfg) => match &collective::route(cfg.as_ref(), &cwd, &learn::home()) {
            collective::Route::Off => println!("off: {cwd} belongs to no collective; hooks do nothing here"),
            r @ collective::Route::Collective { name, home, share } => {
                println!("{name} ({share:?}, may sync: {}) store {}", collective::may_sync(r), home.display());
            }
        },
    }
}

/// The challenges of the state file `path` (before the time `rest` may name), and stdin.
fn challenges_and_stdin(path: &str, rest: &[&str]) -> (Vec<conscience::Challenge>, String) {
    let before = rest.first().and_then(|v| v.parse().ok());
    let challenges = conscience::load(path.as_ref(), before);
    let mut context = String::new();
    let _ = std::io::stdin().read_to_string(&mut context);
    (challenges, context)
}

/// `borg learn [--session ID]`.
fn learn_cmd(rest: &[&str]) {
    borg::stats::set_purpose("learn");
    let session = rest.iter().position(|a| *a == "--session").and_then(|i| rest.get(i + 1)).copied();
    // Unless BORG_REVIEW_FIX=off: the ended session's review → fix findings (its harness's sub-agent
    // runs) become episodes first, each in the store its repo routes to; a store other than this one
    // is learned here too, for this session.
    let mut others = Vec::new();
    if let Some(s) = session
        && learn::learning()
        && borg::review_fix::on()
        && let Ok(cfg) = borg::collective::load()
    {
        let root = borg::collective::root_of(&learn::home());
        let (_, homes) = borg::review_fix::record_session(&borg::review_fix::home_dir(), s, cfg.as_ref(), &root);
        others = homes.into_iter().filter(|h| *h != learn::home()).collect();
    }
    for home in &others {
        learn_store(home, session);
    }
    let (new, merged, waiting) = learn_store(&learn::home(), session);
    println!("{new} new lessons, {merged} merged into existing ones");
    if waiting > 0 {
        eprintln!("borg learn: the model gave no answer; {waiting} episodes still pending, re-run `borg learn` later");
    }
}

/// Learn one store's waiting episodes, then its facets, vectors and today's counts.
fn learn_store(home: &std::path::Path, session: Option<&str>) -> (usize, usize, usize) {
    let store = home.join("lessons.jsonl");
    let from = borg::facets::len_of(&store);
    let got = learn::learn_in_reporting(home, session, &learn::ask_claude, learn::learning(), learn::now_ms(), &mut learn::print_step);
    facets_for_written(&store, from);
    borg::vectors::refresh(&store);
    // Session end runs this in the background: publish today's counts (stats.rs) and the redacted
    // outcomes (outcome.rs `publish`) for the mesh.
    borg::stats::rollup(home);
    borg::outcome::publish(home);
    got
}

/// Live mode's check (live.rs), spawned by the `PostToolUse` hook with the hook's environment.
fn check_cmd(rest: &[&str]) {
    let flag = |f: &str| rest.iter().position(|a| *a == f).and_then(|i| rest.get(i + 1)).copied().unwrap_or_default();
    let session = flag("--session");
    if session.is_empty() {
        eprintln!("borg check: --session ID is required (internal: the PostToolUse hook runs it in live mode)");
        std::process::exit(2);
    }
    let input = json!({"session_id": session, "cwd": flag("--cwd"), "transcript_path": flag("--transcript")});
    borg::hook::live_check(harness::current(), session, &input);
}

/// Each lesson's memory (memory.rs) from the outcomes beside it, as JSON lines; with --session,
/// that session's view (its clock tick and usefulness sample).
fn memory_cmd(path: &str, rest: &[&str]) {
    let session = rest.iter().position(|a| *a == "--session").and_then(|i| rest.get(i + 1)).copied();
    let path = std::path::Path::new(path);
    let lessons = conscience::load_unweighted(path, None);
    let outcomes = outcome::read_team(path);
    let memory = borg::memory::derive(&outcomes, &lessons, session);
    let weights = outcome::weights(&outcomes);
    for c in &lessons {
        let m = &memory[&c.id];
        println!(
            "{}",
            json!({"id": c.id, "activation": m.activation, "changed": m.changed, "ignored": m.ignored, "corrected": m.corrected,
                "vetoed": m.vetoed, "regretted": m.regretted, "tested": m.tested, "mean": m.mean(), "weight": m.weight(&c.id, session), "consolidated": m.consolidated, "dormant": m.dormant,
                "outcome_weight": weights.get(&c.id).copied().unwrap_or(1.0)})
        );
    }
}

/// R1 offline: `{"asked": [ids], "before": text, "edits": text, "final": text, "min_new": n}` on
/// stdin -> `{"changed", "ignored"}`.
fn resolve_cmd(path: &str) {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let all = conscience::load_unweighted(path.as_ref(), None);
    let ids: Vec<&str> = v["asked"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let asked: Vec<&conscience::Challenge> = ids.iter().filter_map(|i| all.iter().find(|c| c.id == *i)).collect();
    let min = v["min_new"].as_u64().map_or(borg::memory::NOVEL_WORDS, |n| usize::try_from(n).unwrap_or(usize::MAX));
    let s = |k: &str| v[k].as_str().unwrap_or_default();
    let (changed, ignored) = borg::memory::resolve(&asked, &all, s("before"), s("edits"), s("final"), min);
    println!("{}", json!({"changed": changed, "ignored": ignored}));
}

/// `borg debug <command>`: the evaluation and debugging tools (completions.rs `DEBUG`).
fn debug_cmd(args: &[&str]) {
    match args {
        // The Stop gate's score for a state file + final message on stdin.
        ["prefilter", path, rest @ ..] => {
            let (challenges, context) = challenges_and_stdin(path, rest);
            println!("{}", prefilter::features(&challenges, &context));
        }
        ["memory", path, rest @ ..] => memory_cmd(path, rest),
        ["resolve", path] => resolve_cmd(path),
        ["recall", path, rest @ ..] => {
            let (challenges, context) = challenges_and_stdin(path, rest);
            for (score, c) in conscience::rank(&challenges, &conscience::work_text(&context)) {
                println!("{} {score:.3}", c.id);
            }
        }
        ["expert", repo, rest @ ..] => std::process::exit(expert::main(repo, rest)),
        ["vectors", rest @ ..] if rest.contains(&"--query") => std::process::exit(borg::vectors::main(rest)),
        ["ope", rest @ ..] => std::process::exit(borg::ope::main(rest)),
        [] | ["help" | "-h" | "--help"] => print!("{}", help(atty(1), true)),
        _ => {
            eprint!("{}", help(atty(2), true));
            std::process::exit(2);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    // The table is the CLI: a word it does not name is no command, whatever an arm below would take.
    if let Some(cmd) = args.first().filter(|c| !borg::completions::known(c)) {
        eprintln!("borg: unknown command {cmd} (borg help lists them)");
        std::process::exit(2);
    }
    match args.as_slice() {
        // The harness's event on stdin (hook.rs); an unknown harness fails open, like any hook error.
        ["hook", name, event] => borg::hook::main(name, event),
        ["collective", rest @ ..] => collective_cmd(rest),
        // The mesh is its own binary (crates/borg-mesh): the networking stack stays out of the hook.
        ["mesh", rest @ ..] => exec("mesh", rest),
        // The live page (crates/borg-web): its web stack stays out of the hook binary too.
        ["web", rest @ ..] => exec("web", rest),
        ["learn", rest @ ..] => learn_cmd(rest),
        ["check", rest @ ..] => check_cmd(rest),
        ["vectors", rest @ ..] if rest.contains(&"--query") => {
            eprintln!("borg vectors: unknown flag --query (borg debug vectors --query)");
            std::process::exit(2);
        }
        ["vectors", rest @ ..] => std::process::exit(borg::vectors::main(rest)),
        ["debug", rest @ ..] => debug_cmd(rest),
        ["setup", rest @ ..] => std::process::exit(borg::install::setup::main(rest)),
        ["completions", rest @ ..] => std::process::exit(borg::completions::main(rest)),
        ["reach", rest @ ..] => {
            borg::stats::set_purpose("reach");
            std::process::exit(borg::reach::main(rest))
        }
        ["assimilate", rest @ ..] if rest.contains(&"--from-dir") => {
            borg::stats::set_purpose("assimilate");
            std::process::exit(borg::assimilate_dir::main(rest, &|prompt: &str, schema: &Value| learn::ask_model(prompt, schema)))
        }
        ["facets", rest @ ..] => {
            borg::stats::set_purpose("facets");
            std::process::exit(borg::facets::main(rest, &|prompt: &str, schema: &Value| learn::ask_model(prompt, schema)))
        }
        ["assimilate", rest @ ..] => {
            borg::stats::set_purpose("assimilate");
            std::process::exit(borg::assimilate::main(rest, &learn::ask_claude))
        }
        ["queen", rest @ ..] => std::process::exit(borg::queen::main(rest)),
        ["notes", rest @ ..] => std::process::exit(borg::notes::main(rest, Some(borg::notes::regress_learn::after_notes))),
        ["regress", rest @ ..] => std::process::exit(borg::notes::regress_cli::main(rest)),
        ["review-fix", rest @ ..] => {
            borg::stats::set_purpose("learn");
            std::process::exit(borg::review_fix::main(rest, &learn::ask_claude))
        }
        ["trust", rest @ ..] => std::process::exit(borg::trust::main(rest)),
        ["top", rest @ ..] => std::process::exit(borg::top::main(rest)),
        ["no", rest @ ..] => std::process::exit(borg::veto::main(rest)),
        [cmd @ ("tell" | "ask" | "pause" | "resume" | "inbox"), rest @ ..] => std::process::exit(borg::msg::main(cmd, rest)),
        ["listen", rest @ ..] => std::process::exit(borg::listen::main(rest)),
        ["why", rest @ ..] => std::process::exit(borg::why::main(rest)),
        [cmd @ ("install" | "uninstall" | "status" | "doctor"), rest @ ..] => std::process::exit(install::main(cmd, rest)),
        [] | ["help" | "-h" | "--help"] => print!("{}", help(atty(1), false)),
        _ => {
            eprint!("{}", help(atty(2), false));
            std::process::exit(2);
        }
    }
}
