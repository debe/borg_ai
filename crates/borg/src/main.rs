//! borg: collective mind for coding-agent drones.
//!
//! usage: borg hook <claude|omp|opencode|codex> <prompt|pre_tool|post_tool|post_tool_failure|stop|end>   (hook JSON on stdin)
//!        Every harness speaks one canonical, Claude-shaped event (session_id, cwd, prompt, tool_name,
//!        tool_input, error, last_assistant_message, transcript_path, stop_hook_active) and gets
//!        Claude-shaped output ({"decision":"block","reason"} at stop); adapters translate both ways.
//!        borg learn [--session ID]                   (generalise captured corrections into lessons)
//!        borg no [--session ID|DRONE] [--dir DIR] [--json] <reason>
//!                                                     (the engineer's veto: shown lessons lose credit, the reason
//!                                                      becomes a lesson or a proposed constraint; veto.rs)
//!        borg check --session ID --transcript FILE --cwd DIR
//!                                                     (internal: live mode's check of the work so far, spawned by
//!                                                      the PostToolUse hook; live.rs)
//!        borg collective [DIR]                       (which collective DIR belongs to; see collective.rs)
//!        borg setup [--yes] [--no-vectors] [...]      (guided first setup, nothing written unconfirmed; install/setup.rs)
//!        borg install|uninstall <claude|omp|opencode|codex|all> [--apply] [--home DIR]
//!                                                     (wire borg into a harness's user config; dry run by default; install.rs)
//!        borg status | borg doctor [--home DIR]       (each collective's store; what is wired; what is broken)
//!        borg assimilate [--apply] [--claude DIR] [--omp DIR] [--opencode DIR] [--since DATE] [--limit N]
//!                                                     (bootstrap the collectives from past Claude Code, omp and opencode sessions;
//!                                                      dry run unless --apply; assimilate.rs)
//!        borg assimilate --from-dir DIR [--apply] [--plan FILE] [--knowledge|--retros|--families|--decisions GLOB]...
//!                                                     (a team's written knowledge -> scoped lessons, open decisions ->
//!                                                      queen queue; dry run unless --apply; assimilate_dir.rs)
//!        borg mesh <id|init|invite|invites|join|revoke|members|peer|serve|sync> [--dir DIR]
//!                                                     (peer-to-peer sync of a team collective; runs borg-mesh)
//!        borg vectors [--rebuild] [--store LESSONS.jsonl] | borg vectors --fetch | borg vectors --query
//!                                                     (the Stop step's vector index: status, rebuild, model
//!                                                      download, shortlist for a context on stdin; vectors.rs)
//!        borg notes attach [<commit>] [--session ID] [--dir DIR]
//!                                                     (redacted reasoning of a session's commits as git notes in
//!                                                      refs/notes/borg, never pushed; notes.rs)
//!        borg regress [--session ID] [--replay <A..B>] [--any-origin] [--explain] [--json] [--dir DIR]
//!                                                     (dry run: earlier decisions later commits fixed; notes/regress.rs)
//!        borg review-fix [--home DIR] [--since YYYY-MM-DD] [--json] [--apply [--only ID,…]]
//!                                                     (sub-agent review findings and whether a fix step fixed them,
//!                                                      no commit needed, 0 tokens; review_fix.rs)
//!        borg queen [--json] [--repo NAME] | show <n|id> | ask | answer <n|id> <text|a|b> | dismiss <n|id> [reason]
//!                   | skip <n|id> | next --json | dedupe [--apply] | notify   [--dir DIR | --collective NAME]
//!                                                     (open questions to the engineer, numbered; an answer becomes a
//!                                                      lesson, a dismissal none; --json for drones; queen.rs)
//!        borg hive init|board|join|leave|open|claim|progress|release|block|done|verify|contract [--session ID] [--dir DIR]
//!                                                     (many drones, one goal, local spike: a board of leased tasks, contracts
//!                                                      between drones, done gated by the goal's oracle; hive/mod.rs)
//!        borg trust [--dir DIR]                      (trust per engineer x area in the collective DIR routes to; trust.rs)
//!        borg top [--dir DIR] [--watch] [--json]      (the drones of each collective, local and remote; top.rs)
//!        borg steer <drone|session|all> "<text>" | --pause | --resume [--dir DIR]
//!                                                     (signed steering of a running drone; runs borg-mesh; steer.rs)
//!        borg why <path>[:<line>[-<line>]] | <commit> [--full] [--json] [--dir DIR]
//!                                                     (the reasoning behind code: blame, borg notes, linked lessons;
//!                                                      on demand only, prints nothing in an off repo; why.rs)
//!        borg recall <challenges.jsonl> [before_ms]   (context on stdin; prints `id score`, best first)
//!        borg memory <lessons.jsonl> [--session ID]   (each lesson's activation, usefulness belief, weight,
//!                                                      consolidated / dormant, from the outcomes beside it; memory.rs)
//!        borg resolve <lessons.jsonl>                 ({"asked", "before", "edits", "final"} on stdin: the word rule's
//!                                                      changed/ignored, memory.rs `resolve`; offline replay and its
//!                                                      control only, never an outcome)
//!        borg prefilter <challenges.jsonl> [before_ms] (state + final message on stdin; the Stop gate's
//!                                                      score as JSON; prefilter.rs)
//!        borg expert <repo> --at <commit> [--version <v>] --q "<text>"
//!                                                     (at most 3 cited claims from <repo> as of <commit>; expert.rs;
//!                                                      BORG_EXPERT_CEILING=<hash> caps --at to that commit and its ancestors)
//!
//! Environment:
//!   BORG_STATE       per-session scratch dir (default /tmp/borg)
//!   BORG_CONFIG      collectives config (default ~/.config/borg/collectives.json; see collective.rs):
//!                    work / private / off per repo path; an off repo leaves no trace
//!   BORG_HOME        collective store (default ~/.local/share/borg): episodes.jsonl, lessons.jsonl,
//!                    outcomes.jsonl (what followed each asked lesson, outcome.rs)
//!   BORG_CONSCIENCE  lessons JSONL for the conscience at Stop (default $BORG_HOME/lessons.jsonl);
//!                    weighted by the outcomes.jsonl beside it, if any, and by its authors' trust (trust.rs)
//!   BORG_LEARN       "off" disables live capture of corrections and outcomes (eval drones)
//!   BORG_REGRESS     decision → fix links at session end (notes/regress.rs): "links" (default) records
//!                    them in both notes and the store; "learn" also makes strong ones regret episodes,
//!                    which `borg learn` words as a question at the decision point; "off" records none
//!   BORG_BEFORE      only use challenges learned before this epoch ms (eval replay)
//!   BORG_CRITIC      "generic" = ask a generic self-review instead (control arm)
//!   BORG_ONLY        comma-separated challenge ids to ask (eval oracle: perfect retrieval)
//!   BORG_MATCH       default: word matching shortlists 20, the consequence model decides (predict.rs);
//!                    "bm25" = word matching alone (no model call): block only when a lesson not yet done
//!                    scores >= 0.8 x the best match; then ask every lesson >= 0.3 x best, in-scope first,
//!                    at most 3 generic, 10 in all (conscience.rs `budget`)
//!   BORG_PREFILTER   default path only: the free gate before the model call (prefilter.rs). Unset = the
//!                    calibrated threshold; a number = that threshold; "off" = always ask the model.
//!                    A skipped turn end logs "#borg-skip <score>" in the session state
//!   BORG_STOP_GATE   "edits" replaces that gate: ask the model at every turn end whose turn edited files or ran a
//!                    state-changing command (prefilter.rs `changes`); read-only or talk-only turns skip
//!                    ("#borg-skip noedit"). Unset = the lexical gate
//!   BORG_STOP_POOL   default: the model sees every lesson the Stop allows (gate, scope, done, dormancy,
//!                    supersession), one compact line each, lessons before the work (whole.rs); over the
//!                    budget BORG_STOP_POOL_TOKENS (default 12000) the shortlist order picks. "shortlist" =
//!                    the shortlist. BORG_STOP_FORMAT=classic shows that pool in the shortlist's layout
//!                    (work first, situations -> consequence)
//!   BORG_STOP_GRADED default: the model grades each lesson it applies (3 certain, 2 likely, 1 worth a thought);
//!                    the Stop blocks only for a 3 and then asks the 3s, 2s and 1s up to 10 (conscience.rs
//!                    `graded`). Without a block, the 2s and 1s go free with the next prompt or touch (as
//!                    much as one early delivery fits, the rest dropped). "off" = no grades:
//!                    any apply blocks, and the block widens over the low bar
//!   BORG_EARLY      "off" disables early delivery: a prompt or touch that first names a lesson's scope
//!                    gets that scope's top lessons as context at once (Claude Code only; conscience.rs `early`)
//!   BORG_SYSTEM_GATE "off" disables the system gate: when the session's repo (or a touched path's repo)
//!                    maps to a known system, lessons scoped to other systems score 0 (systems.rs)
//!   BORG_CONCEPTS    "on" (spike) boosts lessons sharing engineering concepts with the work (concepts.rs);
//!                    BORG_CONCEPTS_BETA its strength (default 0.5)
//!   BORG_HIVE        a hive goal id: the session joins that goal at its first hook (hive/mod.rs; the
//!                    hive runner sets it on each drone); unknown goals are ignored
//!   BORG_QUEEN       "on" lets the Stop hook ask the engineer (queen.rs); off by default (the queen
//!                    test failed its pre-registered bar, internal evaluation "queen-test", not published)
//!   BORG_QUEEN_ASK  "off" disables queen questions in the harness: the first prompt of a session in a repo
//!                    with an open queen question gets that one question as context, to ask the engineer and
//!                    record with `borg queen answer` (claude, omp, opencode; 0 model calls; queen.rs `deliver`);
//!                    BORG_LEARN=off disables them too, and a prompt payload with `"context": false` (an
//!                    adapter that will not wait for the answer) gets no context at all
//!   BORG_NOTIFY      "off" disables the desktop notification (notify-send) when questions are queued (notify.rs)
//!   BORG_PREDICT_MODEL  model for the consequence model: unset = through the drone's own harness (runner.rs;
//!                    the claude CLI runs sonnet, omp and opencode their own default), `<runner>:<model>` pins
//!                    one runner and model (`claude:claude-sonnet-5-5`), "off" = no model call
//!   BORG_MODE        delivery mode, overriding the collective's `mode` (live.rs): "hook" judges the
//!                    work at the turn end; "live" (default) also judges it during the turn, after state-changing tool
//!                    calls, in a detached `borg check` (every harness). Anything else is live
//!   BORG_LIVE_DEBOUNCE_S  live mode: the least time between the starts of two checks (default 45)
//!   BORG_VECTORS     "off" disables vector recall at the Stop step (vectors.rs; builds with the `vectors`
//!                    feature): BORG_VECTOR_MODEL (model dir, default ~/.local/share/borg/models/
//!                    potion-multilingual-128M), BORG_VECTOR_K (hits added to the shortlist, default 10)
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

/// `borg help`: the commands grouped by what they are for, one line each. `HARNESS` stands for the
/// harnesses' names (harness/mod.rs `ALL`).
const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Everyday",
        &[
            ("status", "each collective's lessons, outcomes and wiring"),
            ("doctor", "what is broken: config, hooks, adapters, the model CLI"),
            ("top [--watch] [--json] [--dir DIR]", "the drones running across the team"),
            ("web [--port PORT] [--dir DIR]", "a live page of knowledge and stats on 127.0.0.1"),
            ("why <path>[:<line>[-<line>]] | <commit>", "the reasoning behind code  [--full] [--json]"),
            ("queen [show|ask|answer|skip|dismiss|next]", "open decisions waiting for you"),
            ("no [--session ID] <reason>", "say no to a drone's work: shown lessons lose credit, the reason becomes a lesson"),
        ],
    ),
    (
        "Setup",
        &[
            ("setup [--yes] [--no-vectors]", "guided setup: collectives, team, vectors, wiring"),
            ("install <HARNESS|all> [--apply]", "wire the hooks (a dry run without --apply)"),
            ("uninstall <HARNESS|all> [--apply]", "unwire them"),
            ("collective [DIR]", "which collective a directory belongs to"),
            ("vectors [--fetch|--rebuild|--query] [--store LESSONS]", "the vector model and index"),
            ("completions <bash|zsh|fish>", "a shell completion script"),
        ],
    ),
    (
        "Knowledge",
        &[
            ("learn [--session ID]", "turn waiting corrections into lessons now"),
            ("assimilate [--apply] [--since DATE] [--limit N]", "import corrections from past sessions"),
            ("assimilate --from-dir DIR [--plan FILE] [--apply]", "import lessons from team documents"),
            ("reach [--plan FILE] [--apply] [--jobs N]", "sort lessons into team, repo-only and personal"),
            ("trust [--dir DIR]", "trust per engineer and area"),
            ("notes attach|fetch [<commit>...]", "reasoning notes behind commits"),
            ("regress [--replay A..B] [--explain] [--json]", "which earlier decisions later commits fixed (dry run)"),
            ("review-fix [--since DATE] [--json]", "bugs workflow reviews found and fix steps fixed, before any commit"),
        ],
    ),
    (
        "Team",
        &[
            ("mesh <command>", "sync with team members (borg mesh --help)"),
            ("steer <drone|all> <text> | --pause | --resume", "a signed instruction to running drones"),
            ("hive init|board|join|claim|done|...", "many drones, one goal: the board, claims and contracts"),
        ],
    ),
    (
        "Debugging and evaluation",
        &[
            ("memory <lessons.jsonl> [--session ID]", "each lesson's activation and usefulness"),
            ("recall <lessons.jsonl> [before_ms]", "rank lessons for text on stdin"),
            ("prefilter <state> [before_ms]", "the free gate's score for a turn on stdin"),
            ("facets [--apply] [--store FILE] [--jobs N]", "search facets for lessons"),
            ("expert <repo> --at <commit> --q <text>", "ask an expert repo"),
            ("hook <HARNESS> <event>", "what the harness runs; not for people"),
        ],
    ),
];

/// Whether file descriptor `fd` is a terminal (bold headings only there, and never with NO_COLOR).
fn atty(fd: i32) -> bool {
    // SAFETY: isatty only inspects the descriptor.
    std::env::var_os("NO_COLOR").is_none() && unsafe { libc::isatty(fd) } == 1
}

fn help(color: bool) -> String {
    let bold = |t: &str| if color { format!("\x1b[1m{t}\x1b[0m") } else { t.to_string() };
    let names = harness::names("|");
    let cmd = |c: &str| c.replace("HARNESS", &names);
    let width = HELP.iter().flat_map(|(_, rows)| rows.iter().map(|(c, _)| cmd(c).chars().count())).max().unwrap_or(0);
    let mut out = format!("{}\n\n{} borg <command> [options]\n", bold("borg: a shared memory for coding agents"), bold("Usage:"));
    for (group, rows) in HELP {
        out.push_str(&format!("\n{}\n", bold(group)));
        for (c, what) in *rows {
            out.push_str(&format!("  {:<width$}  {what}\n", cmd(c)));
        }
    }
    out.push_str("\nMore: docs/commands.md, or https://github.com/debe/borg_ai\n");
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
                "vetoed": m.vetoed, "regretted": m.regretted, "mean": m.mean(), "weight": m.weight(&c.id, session), "consolidated": m.consolidated, "dormant": m.dormant,
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        // The harness's event on stdin (hook.rs); an unknown harness fails open, like any hook error.
        ["hook", name, event] => borg::hook::main(name, event),
        ["collective", rest @ ..] => collective_cmd(rest),
        [cmd @ ("mesh" | "steer"), rest @ ..] => {
            // The mesh is its own binary (crates/borg-mesh): the networking stack stays out of the hook.
            // So is steering: it signs with the member key and checks the membership (steer.rs).
            let args: Vec<&str> = if *cmd == "steer" { std::iter::once("steer").chain(rest.iter().copied()).collect() } else { rest.to_vec() };
            exec("mesh", &args)
        }
        // The live page (crates/borg-web): its web stack stays out of the hook binary too.
        ["web", rest @ ..] => exec("web", rest),
        ["learn", rest @ ..] => learn_cmd(rest),
        ["check", rest @ ..] => check_cmd(rest),
        ["vectors", rest @ ..] => std::process::exit(borg::vectors::main(rest)),
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
        // The Stop gate's score for a state file + final message on stdin (eval/preempt/stop_prefilter.py).
        ["prefilter", path, rest @ ..] => {
            let (challenges, context) = challenges_and_stdin(path, rest);
            println!("{}", prefilter::features(&challenges, &context));
        }
        ["memory", path, rest @ ..] => memory_cmd(path, rest),
        ["resolve", path] => resolve_cmd(path),
        ["recall", path, rest @ ..] => {
            let (challenges, context) = challenges_and_stdin(path, rest);
            for (score, c) in conscience::rank(&challenges, &context) {
                println!("{} {score:.3}", c.id);
            }
        }
        ["expert", repo, rest @ ..] => std::process::exit(expert::main(repo, rest)),
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
        ["hive", rest @ ..] => std::process::exit(borg::hive::main(rest)),
        ["why", rest @ ..] => std::process::exit(borg::why::main(rest)),
        [cmd @ ("install" | "uninstall" | "status" | "doctor"), rest @ ..] => std::process::exit(install::main(cmd, rest)),
        [] | ["help" | "-h" | "--help"] => print!("{}", help(atty(1))),
        _ => {
            eprint!("{}", help(atty(2)));
            std::process::exit(2);
        }
    }
}
