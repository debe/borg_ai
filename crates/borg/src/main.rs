//! borg: collective mind for coding-agent drones.
//!
//! usage: borg hook <claude|omp|opencode> <prompt|pre_tool|post_tool|post_tool_failure|stop|end>   (hook JSON on stdin)
//!        Every harness speaks one canonical, Claude-shaped event (session_id, cwd, prompt, tool_name,
//!        tool_input, error, last_assistant_message, transcript_path, stop_hook_active) and gets
//!        Claude-shaped output ({"decision":"block","reason"} at stop); adapters translate both ways.
//!        borg learn [--session ID]                   (generalise captured corrections into lessons)
//!        borg collective [DIR]                       (which collective DIR belongs to; see collective.rs)
//!        borg install|uninstall <claude|omp|opencode|all> [--apply] [--home DIR]
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
//!        borg queen [--json] [--repo NAME] | show <n|id> | ask | answer <n|id> <text|a|b> | dismiss <n|id> [reason]
//!                   | skip <n|id> | next --json | dedupe [--apply] | notify   [--dir DIR | --collective NAME]
//!                                                     (open questions to the engineer, numbered; an answer becomes a
//!                                                      lesson, a dismissal none; --json for drones; queen.rs)
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
//!                    `graded`). Without a block, the 2s and 1s go free with the next prompt or touch (Claude
//!                    Code only; as much as one early delivery fits, the rest dropped). "off" = no grades:
//!                    any apply blocks, and the block widens over the low bar
//!   BORG_EARLY      "off" disables early delivery: a prompt or touch that first names a lesson's scope
//!                    gets that scope's top lessons as context at once (Claude Code only; conscience.rs `early`)
//!   BORG_SYSTEM_GATE "off" disables the system gate: when the session's repo (or a touched path's repo)
//!                    maps to a known system, lessons scoped to other systems score 0 (systems.rs)
//!   BORG_CONCEPTS    "on" (spike) boosts lessons sharing engineering concepts with the work (concepts.rs);
//!                    BORG_CONCEPTS_BETA its strength (default 0.5)
//!   BORG_QUEEN       "on" lets the Stop hook ask the engineer (queen.rs); off by default (the queen
//!                    test failed its pre-registered bar, reports/queen-test.md)
//!   BORG_QUEEN_ASK  "off" disables queen questions in the harness: the first prompt of a session in a repo
//!                    with an open queen question gets that one question as context, to ask the engineer and
//!                    record with `borg queen answer` (claude, omp, opencode; 0 model calls; queen.rs `deliver`);
//!                    BORG_LEARN=off disables them too, and a prompt payload with `"context": false` (an
//!                    adapter that will not wait for the answer) gets no context at all
//!   BORG_NOTIFY      "off" disables the desktop notification (notify-send) when questions are queued (notify.rs)
//!   BORG_PREDICT_MODEL  model for the consequence model (default sonnet)
//!   BORG_VECTORS     "off" disables vector recall at the Stop step (vectors.rs; builds with the `vectors`
//!                    feature): BORG_VECTOR_MODEL (model dir, default ~/.local/share/borg/models/
//!                    potion-multilingual-128M), BORG_VECTOR_K (hits added to the shortlist, default 10)
//!
//! Hooks always fail open: any error lets the drone continue untouched.

use std::io::{Read, Write};
use std::path::PathBuf;

use borg::learn::learning;
use borg::{collective, conscience, expert, install, learn, outcome, predict, prefilter};
use borg_trace::fingerprint::bash_read_paths;
use serde_json::{Value, json};

fn state_file(session: &str) -> PathBuf {
    let dir = PathBuf::from(std::env::var("BORG_STATE").unwrap_or_else(|_| "/tmp/borg".into()));
    let _ = std::fs::create_dir_all(&dir);
    let safe: String = session.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    dir.join(format!("{safe}.ctx"))
}

fn append(session: &str, text: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(state_file(session)) {
        let _ = writeln!(f, "{text}");
    }
}

/// What a tool call touched, as text for matching: paths and search patterns.
fn touched(input: &Value, tool: &str) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    match tool {
        "Bash" => {
            let cmd = s("command");
            let paths = bash_read_paths(&cmd);
            if paths.is_empty() { cmd.chars().take(200).collect() } else { paths.join(" ") }
        }
        "Grep" => format!("{} {}", s("pattern"), s("path")),
        "Glob" => s("pattern"),
        _ => [s("file_path"), s("notebook_path"), s("path")].join(" ").trim().to_string(),
    }
}

fn repo_of(cwd: &str) -> String {
    borg_trace::repo_key(cwd).rsplit('/').next().unwrap_or_default().to_string()
}

/// Counts one event of this session in the collective's statistics (stats.rs); `fill` adds details.
fn stat(session: &str, input: &Value, kind: &str, fill: impl FnOnce(&mut borg::stats::Event)) {
    let home = learn::home();
    let mut ev = borg::stats::Event::new(kind, learn::now_ms());
    ev.session = session.into();
    ev.repo = repo_of(input.get("cwd").and_then(Value::as_str).unwrap_or_default());
    // The engineer as presence.rs already resolved it at this hook: no second `git config` call.
    ev.engineer = borg::presence::read(&home, session).map(|p| p.engineer).unwrap_or_default();
    fill(&mut ev);
    borg::stats::record(&home, &ev);
}

/// The engineer's prompts this session (the queen's criterion 3; the state file mixes in touches).
fn prompts_file(session: &str) -> PathBuf {
    state_file(session).with_extension("prompts")
}

/// Where the session works: the repo it runs in and the paths it touched (`where_now` adds the latest
/// prompt). Scopes are judged here only, so a system merely mentioned in a command's text or in an
/// earlier part of the conversation is not a topic.
fn where_file(session: &str) -> PathBuf {
    state_file(session).with_extension("where")
}

/// Where the session works now: the repo and touched paths so far, plus the engineer's latest prompt
/// (an earlier prompt's system is not this turn's work).
fn where_now(session: &str) -> String {
    let paths = std::fs::read_to_string(where_file(session)).unwrap_or_default();
    let latest = std::fs::read_to_string(state_file(session).with_extension("lastprompt")).unwrap_or_default();
    format!("{paths}\n{latest}")
}

fn note_where(session: &str, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(where_file(session)) {
        let _ = writeln!(f, "{text}");
    }
}

fn task_file(session: &str) -> PathBuf {
    state_file(session).with_extension("task")
}

/// A tool call refused without words, waiting for the engineer's next prompt (their reason).
/// Pending only while the drone waits: a later successful tool call or the session's end clears it,
/// because omp lets the model carry on after a denial and the next prompt may be about anything.
fn denied_file(session: &str) -> PathBuf {
    state_file(session).with_extension("denied")
}

/// A correcting prompt becomes an episode (task, the drone's last proposal, the correction);
/// so does the first prompt after a wordless tool denial. Any other prompt becomes the session's task.
fn capture_prompt(session: &str, input: &Value, prompt: &str) {
    if !learning() {
        return;
    }
    let task = std::fs::read_to_string(task_file(session)).unwrap_or_default();
    if let Ok(tool) = std::fs::read_to_string(denied_file(session)) {
        let _ = std::fs::remove_file(denied_file(session));
        let said = prompt.trim();
        if said.len() > 3 && !said.starts_with('<') {
            record(session, input, task, proposal(input), Some(tool), said.chars().take(1500).collect());
            return;
        }
    }
    if !learn::is_correction(prompt) || task.is_empty() {
        let _ = std::fs::write(task_file(session), prompt.chars().take(3000).collect::<String>());
        return;
    }
    record(session, input, task, proposal(input), None, prompt.chars().take(1500).collect());
}

/// A rejected tool call or plan that carries the engineer's words becomes an episode; a wordless
/// denial waits for the next prompt (`capture_prompt`).
fn capture_rejection(session: &str, input: &Value) {
    if !learning() {
        return;
    }
    let error = input.get("error").and_then(Value::as_str).unwrap_or_default();
    let tool = format!(
        "{} {}",
        input.get("tool_name").and_then(Value::as_str).unwrap_or_default(),
        input.get("tool_input").map(|v| v.to_string()).unwrap_or_default().chars().take(1500).collect::<String>()
    );
    let Some(said) = learn::rejection(error) else {
        if learn::denied(error) {
            let _ = std::fs::write(denied_file(session), tool);
        }
        return;
    };
    let task = std::fs::read_to_string(task_file(session)).unwrap_or_default();
    record(session, input, task, proposal(input), Some(tool), said);
}

/// The drone's last proposal: given directly by the adapter, else read from a Claude transcript.
fn proposal(input: &Value) -> String {
    match input.get("last_assistant_message").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) {
        Some(t) => t.chars().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect(),
        None => learn::last_assistant_text(input.get("transcript_path").and_then(Value::as_str).unwrap_or_default().as_ref()),
    }
}

/// The drone's whole last proposal (`proposal` keeps its last 2000 characters): the reasoning note
/// redacts it before it cuts (notes.rs).
fn full_proposal(input: &Value) -> String {
    match input.get("last_assistant_message").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) {
        Some(t) => t.to_string(),
        None => learn::last_assistant_full(input.get("transcript_path").and_then(Value::as_str).unwrap_or_default().as_ref()),
    }
}

fn record(session: &str, input: &Value, task: String, agent: String, rejected_tool: Option<String>, correction: String) {
    let ms = learn::now_ms();
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
    let e = learn::Episode {
        id: format!("{}-{ms}", &session.chars().take(8).collect::<String>()),
        ms,
        repo: repo_of(cwd),
        session: session.into(),
        task,
        agent,
        rejected_tool,
        correction,
        engineer: learn::engineer(cwd),
    };
    let _ = learn::record_episode(&e);
    append(session, &format!("#borg-episode {}", e.id));
}

/// The lessons the conscience reads (BORG_CONSCIENCE, the BORG_BEFORE cut-off) as `session` sees them:
/// weighted by their memory with this session's usefulness sample (memory.rs).
fn lessons(session: &str) -> Vec<conscience::Challenge> {
    let before = std::env::var("BORG_BEFORE").ok().and_then(|v| v.parse().ok());
    let mut all = conscience::load_for(&lessons_path(), before, Some(session));
    // The system gate (systems.rs): where the repo says which system this is, lessons scoped to other
    // systems score 0, however much the conversation names them.
    let where_text = std::fs::read_to_string(where_file(session)).unwrap_or_default();
    let repos = borg::systems::work_repos(&where_text);
    if borg::systems::on() {
        borg::systems::gate(&mut all, &repos);
    }
    // Reach (reach.rs): a withdrawn personal lesson is never delivered, a repo convention only in its repo.
    all.retain(|c| borg::reach::deliverable(c, &repos));
    // Spike (concepts.rs): lessons sharing engineering concepts with the code written, the touched paths
    // and the latest prompt rank higher; never a filter.
    if borg::concepts::on() {
        let where_text = std::fs::read_to_string(where_file(session)).unwrap_or_default();
        let edits = std::fs::read_to_string(edits_file(session)).unwrap_or_default();
        let latest = std::fs::read_to_string(state_file(session).with_extension("lastprompt")).unwrap_or_default();
        borg::concepts::apply(&mut all, &borg::concepts::of_work(&edits, &where_text, &latest), borg::concepts::beta());
    }
    all
}

/// The lessons file the conscience reads: BORG_CONSCIENCE, default $BORG_HOME/lessons.jsonl.
fn lessons_path() -> PathBuf {
    std::env::var_os("BORG_CONSCIENCE").map(PathBuf::from).unwrap_or_else(|| learn::home().join("lessons.jsonl"))
}

/// What the drone did, for R1 (`pending`, `early_acted`): one line per Edit/Write/MultiEdit/NotebookEdit
/// (path and the start of the new text); at each Stop block the blocked message and `#borg-asked <ids>`;
/// the drone's answer at the turn end after it (`#borg-answer <message>`); `#borg-early <ids>` per early
/// delivery. Kept out of the state file so the drone's code never feeds the conscience's matching.
fn edits_file(session: &str) -> PathBuf {
    state_file(session).with_extension("edits")
}

/// What Borg cost this session (R3b), one line per delivery: `block questions=N chars=M` for a Stop
/// block (an extra round), `free event=E lessons=N chars=M` for context on a hook that forces no round.
/// `borg status` sums them (prefilter.rs `tally`).
fn cost_file(session: &str) -> PathBuf {
    state_file(session).with_extension("cost")
}

/// BORG_STOP_GATE=edits: one line per tool call this turn that changed state (prefilter.rs `changes`);
/// the Stop removes it.
fn changes_file(session: &str) -> PathBuf {
    state_file(session).with_extension("changes")
}

fn log_cost(session: &str, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(cost_file(session)) {
        let _ = writeln!(f, "{line}");
    }
}

/// Lessons delivered early (`#borg-early` markers in the .edits file, and graded lessons handed over free,
/// `#borg-queued`) that the drone acted on since: its work after the delivery brings in the lesson's
/// words (memory.rs `resolve`, `final_msg` included). Stop leaves them alone; an early lesson not acted
/// on can still be asked at Stop.
fn early_acted(session: &str, all: &[conscience::Challenge], final_msg: &str) -> std::collections::HashSet<String> {
    let edits = std::fs::read_to_string(edits_file(session)).unwrap_or_default();
    let state = std::fs::read_to_string(state_file(session)).unwrap_or_default();
    let lines: Vec<&str> = edits.lines().collect();
    let mut out = std::collections::HashSet::new();
    for (p, l) in lines.iter().enumerate() {
        let Some(ids) = l.strip_prefix("#borg-early ").or_else(|| l.strip_prefix("#borg-queued ")) else { continue };
        let ids: std::collections::HashSet<&str> = ids.split(',').collect();
        let asked: Vec<&conscience::Challenge> = all.iter().filter(|c| ids.contains(c.id.as_str())).collect();
        let after: Vec<&str> = lines[p + 1..].iter().copied().filter(|l| !l.starts_with("#borg-")).collect();
        let upto = state.find(&format!("{l}\n")).map_or(state.as_str(), |i| &state[..i]);
        let before = format!("{upto}\n{}", lines[..p].join("\n"));
        out.extend(borg::memory::resolve(&asked, all, &before, &after.join("\n"), final_msg, borg::memory::NOVEL_WORDS).0);
    }
    out
}

/// An edit tool's path and the start of its new text, on one line; None for other tools.
fn edit_text(tool: &str, input: &Value) -> Option<String> {
    if !["edit", "write", "multiedit", "notebookedit"].contains(&tool.to_ascii_lowercase().as_str()) {
        return None;
    }
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let mut text = [s(input, "file_path"), s(input, "notebook_path"), s(input, "new_string"), s(input, "content"), s(input, "new_source")].join(" ");
    for e in input.get("edits").and_then(Value::as_array).into_iter().flatten() {
        text.push(' ');
        text.push_str(&s(e, "new_string"));
    }
    Some(text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(600).collect())
}

/// `text` on one line, at most `n` characters.
fn one_line(text: &str, n: usize) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(n).collect()
}

/// R1: the first turn end after a Stop block is the drone's answer to it. Its message (with any
/// `<id>: applies|skip`) goes to the .edits file as `#borg-answer`: evidence for the judge (`pending`),
/// never a label by itself.
fn note_answer(session: &str, input: &Value) {
    let edits = std::fs::read_to_string(edits_file(session)).unwrap_or_default();
    let lines: Vec<&str> = edits.lines().collect();
    let Some(i) = lines.iter().rposition(|l| l.starts_with("#borg-asked ")) else { return };
    if lines[i + 1..].iter().any(|l| l.starts_with("#borg-answer ")) {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(edits_file(session)) {
        let _ = writeln!(f, "#borg-answer {}", one_line(&full_proposal(input), 4000));
    }
}

/// Pending lessons sent to the judge per call at most (the most recently asked): one full block
/// (conscience.rs caps a block at 10), whose lessons share their evidence in the prompt.
const MAX_PENDING: usize = 10;
/// A lesson whose verdicts stay pending (unclear, missing) is sent to the judge this often at most;
/// after that it waits for `borg learn` to settle it as `asked_not_corrected`.
const MAX_JUDGE_TRIES: usize = 2;

/// R1: lessons asked at an earlier Stop of this session that have no verdict yet, with the evidence for
/// the consequence model (predict.rs `Pending`): the blocked message (before), and after it the drone's
/// answer, its edits and later messages since, and this turn's message (`final_msg`), each part capped at
/// `predict::JUDGE_PART` characters. `#borg-judged <id>=<label>` lines in the state file record verdicts.
fn pending<'a>(session: &str, state: &str, all: &'a [conscience::Challenge], final_msg: &str) -> Vec<predict::Pending<'a>> {
    let mut tries: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut settled = std::collections::HashSet::new();
    for l in state.lines().filter_map(|l| l.strip_prefix("#borg-judged ")) {
        let (id, label) = l.split_once('=').unwrap_or((l, ""));
        *tries.entry(id).or_default() += 1;
        if label.trim() != "pending" {
            settled.insert(id);
        }
    }
    let edits = std::fs::read_to_string(edits_file(session)).unwrap_or_default();
    let lines: Vec<&str> = edits.lines().collect();
    let part = predict::JUDGE_PART;
    let mut out: Vec<predict::Pending> = Vec::new();
    for (p, l) in lines.iter().enumerate() {
        let Some(ids) = l.strip_prefix("#borg-asked ") else { continue };
        let before = p.checked_sub(1).map(|b| lines[b]).filter(|b| !b.starts_with("#borg-")).unwrap_or_default();
        let later = &lines[p + 1..];
        let answers: Vec<&str> = later.iter().filter_map(|l| l.strip_prefix("#borg-answer ")).collect();
        let work: Vec<&str> = later.iter().copied().filter(|l| !l.starts_with("#borg-")).collect();
        let after = format!(
            "answer: {}\nwork since: {}\nthis turn: {}",
            predict::clip(&answers.join(" "), part),
            predict::clip(&work.join(" | "), part),
            predict::clip(&one_line(final_msg, part + 1), part)
        );
        let said = answers.join("\n").to_lowercase();
        for id in ids.split(',').map(str::trim).filter(|i| !i.is_empty()) {
            if settled.contains(id) || tries.get(id).copied().unwrap_or(0) >= MAX_JUDGE_TRIES || out.iter().any(|x| x.lesson.id == id) {
                continue;
            }
            let Some(c) = all.iter().find(|c| c.id == id) else { continue };
            let declared = borg::memory::declared(&said, &id.to_lowercase());
            out.push(predict::Pending { lesson: c, before: before.to_string(), after: after.clone(), declared });
        }
    }
    let n = out.len();
    out.split_off(n.saturating_sub(MAX_PENDING))
}

/// R1: the consequence model's verdicts on the pending lessons it was shown (predict.rs `Judged`).
/// `asked_then_changed` only for a change that avoids the consequence, `asked_ignored` for no change;
/// unclear or missing verdicts stay pending. Logs the call's judging cost in the .cost file:
/// `judge call=<nth model call of the session> pending=N resolved=K chars=<prompt chars added>`.
fn record_verdicts(session: &str, input: &Value, pending: &[predict::Pending], resolved: &[predict::Judged], chars: usize, call: usize) {
    let (mut changed, mut ignored) = (Vec::new(), Vec::new());
    for p in pending {
        let label = resolved.iter().find(|j| j.id == p.lesson.id).and_then(predict::Judged::label);
        let word = match label {
            Some(outcome::CHANGED) => "changed",
            Some(_) => "ignored",
            None => "pending",
        };
        append(session, &format!("#borg-judged {}={word}", p.lesson.id));
        match label {
            Some(outcome::CHANGED) => changed.push(p.lesson.id.clone()),
            Some(_) => ignored.push(p.lesson.id.clone()),
            None => {}
        }
    }
    log_cost(session, &format!("judge call={call} pending={} resolved={} chars={chars}", pending.len(), changed.len() + ignored.len()));
    if learning() {
        let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
        for (kind, lessons) in [(outcome::CHANGED, changed), (outcome::IGNORED, ignored)] {
            if !lessons.is_empty() {
                let o = outcome::Outcome { kind: kind.into(), session: session.into(), lessons, ms: learn::now_ms(), repo: repo_of(cwd), ..Default::default() };
                let _ = outcome::append(&learn::home().join("outcomes.jsonl"), &o);
            }
        }
    }
}

/// Ids listed on the state file's `marker` lines (`#borg-asked a,b`).
fn listed<'a>(context: &'a str, marker: &str) -> std::collections::HashSet<&'a str> {
    context.lines().filter_map(|l| l.strip_prefix(marker)).flat_map(|ids| ids.split(',')).map(str::trim).filter(|i| !i.is_empty()).collect()
}

/// Early delivery (conscience.rs `early`): once the session's prompts and touches name a lesson scope
/// (a topic) and that scope's lessons score on them, the top ones go to the drone at once, as context,
/// instead of waiting for the Stop checkpoint: at the prompt or the first touch on the topic (`text` is
/// this event's, already in the state file). Each lesson once per session (`#borg-early`; Stop does not
/// ask it again). A topic is settled (`#borg-topic`) once a delivery covered it or none of its lessons is
/// left; until then each prompt or touch looks again. At most `MAX_EARLY_DELIVERIES` per session. Claude
/// Code only: omp and opencode read borg's answer at a stop alone, so for them the lessons stay with Stop. `BORG_EARLY=off` disables it;
/// so do the eval modes BORG_ONLY and BORG_CRITIC.
/// Early deliveries per session at most (each up to `conscience::render_early`'s lesson cap).
const MAX_EARLY_DELIVERIES: usize = 3;

fn early(harness: &str, hook_event: &str, session: &str, text: &str) -> Option<String> {
    if harness != "claude"
        || text.trim().is_empty()
        || std::env::var("BORG_EARLY").is_ok_and(|v| v.trim().eq_ignore_ascii_case("off"))
        || std::env::var_os("BORG_ONLY").is_some()
        || std::env::var_os("BORG_CRITIC").is_some()
    {
        return None;
    }
    let challenges = lessons(session);
    if challenges.iter().all(|c| c.scope.is_empty() && !c.consolidated) {
        return None;
    }
    let context = std::fs::read_to_string(state_file(session)).unwrap_or_default();
    // Free, but not free of noise: a long session naming many systems would otherwise get a delivery
    // at every touch until each topic's lessons ran out.
    if context.lines().filter(|l| l.starts_with("#borg-early ")).count() >= MAX_EARLY_DELIVERIES {
        return None;
    }
    let settled = listed(&context, "#borg-topic ");
    let mut topics: Vec<String> = conscience::named_scopes(&challenges, &where_now(session)).into_iter().filter(|t| !settled.contains(t.as_str())).collect();
    // Consolidated lessons (memory.rs) need no scope: the first prompt is their topic (`ANY`).
    if topics.is_empty() && hook_event == "UserPromptSubmit" && !settled.contains(conscience::ANY) && challenges.iter().any(|c| c.consolidated) {
        topics.push(conscience::ANY.into());
    }
    if topics.is_empty() {
        return None;
    }
    let mut done = listed(&context, "#borg-asked ");
    done.extend(listed(&context, "#borg-early "));
    done.extend(listed(&context, "#borg-queued "));
    let selected = conscience::early(&challenges, &context, &topics, &done);
    let shown = conscience::render_early(&selected, &topics);
    let delivered = &selected[..shown.as_ref().map_or(0, |(_, n)| *n)];
    let now_settled: Vec<&str> = topics
        .iter()
        .filter(|t| {
            // One delivery per topic: whatever was delivered now covers every topic looked at now.
            !delivered.is_empty()
                || !challenges.iter().any(|c| {
                    (c.scope.contains(t) || (t.as_str() == conscience::ANY && c.consolidated))
                        && !done.contains(c.id.as_str())
                        && !delivered.iter().any(|d| d.id == c.id)
                })
        })
        .map(String::as_str)
        .collect();
    if !now_settled.is_empty() {
        append(session, &format!("#borg-topic {}", now_settled.join(",")));
    }
    let (text, _) = shown?;
    let ids = delivered.iter().map(|c| c.id.as_str()).collect::<Vec<_>>().join(",");
    append(session, &format!("#borg-early {ids}"));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(edits_file(session)) {
        let _ = writeln!(f, "#borg-early {ids}");
    }
    log_cost(session, &format!("free event={hook_event} lessons={} chars={}", delivered.len(), text.len()));
    borg::stats::count(&learn::home(), borg::stats::EARLY, session, |e| {
        e.n = delivered.len() as u64;
        e.lessons = delivered.iter().map(|c| c.id.clone()).collect();
    });
    Some(text)
}

/// Graded Stop (default): ids graded 2 or 1 at a turn end that did not block, one
/// comma-separated line per such Stop, waiting for the session's next prompt or touch (`queued`).
fn queued_file(session: &str) -> PathBuf {
    state_file(session).with_extension("queued")
}

/// The waiting graded lessons as free context, once: those not asked, delivered early or handed over
/// since, as many as one early delivery fits (`conscience::render_early`); the rest are dropped. Marked
/// `#borg-queued` in the state and .edits files, so neither this queue nor early delivery repeats them,
/// and Stop treats them as early lessons (done once acted on).
fn queued(hook_event: &str, session: &str) -> Option<String> {
    let ids = std::fs::read_to_string(queued_file(session)).ok()?;
    let _ = std::fs::remove_file(queued_file(session));
    let all = lessons(session);
    let state = std::fs::read_to_string(state_file(session)).unwrap_or_default();
    let mut done = listed(&state, "#borg-asked ");
    done.extend(listed(&state, "#borg-early "));
    done.extend(listed(&state, "#borg-queued "));
    let mut wanted: Vec<&conscience::Challenge> = Vec::new();
    for id in ids.lines().flat_map(|l| l.split(',')).map(str::trim) {
        if let Some(c) = all.iter().find(|c| c.id == id)
            && !done.contains(id)
            && !wanted.iter().any(|w| w.id == id)
        {
            wanted.push(c);
        }
    }
    let (text, n) = conscience::render_early(&wanted, &[])?;
    let ids = wanted[..n].iter().map(|c| c.id.as_str()).collect::<Vec<_>>().join(",");
    append(session, &format!("#borg-queued {ids}"));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(edits_file(session)) {
        let _ = writeln!(f, "#borg-queued {ids}");
    }
    log_cost(session, &format!("free event={hook_event} lessons={n} chars={} graded dropped={}", text.len(), wanted.len() - n));
    borg::stats::count(&learn::home(), borg::stats::QUEUED_FREE, session, |e| {
        e.n = n as u64;
        e.lessons = wanted[..n].iter().map(|c| c.id.clone()).collect();
    });
    Some(text)
}

/// One open queen question for the engineer (queen.rs `deliver`), at the first prompt of the session
/// where one of its work repos (the cwd's and the touched ones, systems.rs `work_repos`) has one: never
/// another repo's. Its commands run this binary in the session's `cwd`, wherever the drone's shell has
/// gone since. 0 model calls; fails open. Off with `BORG_QUEEN_ASK=off` or `BORG_LEARN=off` (nothing
/// recorded; eval drones cannot answer), and in the eval modes BORG_ONLY and BORG_CRITIC.
fn queen_ask(harness: &str, session: &str, cwd: &str) -> Option<String> {
    if !learning() || !borg::queen::ask_enabled() || std::env::var_os("BORG_ONLY").is_some() || std::env::var_os("BORG_CRITIC").is_some() {
        return None;
    }
    let repos = || {
        let mut repos = borg::systems::work_repos(&std::fs::read_to_string(where_file(session)).unwrap_or_default());
        let here = repo_of(cwd);
        if !here.is_empty() && !repos.contains(&here) {
            repos.insert(0, here);
        }
        repos
    };
    let scope = if cwd.is_empty() { String::new() } else { format!(" --dir {}", borg::queen::sh_quote(cwd)) };
    let mut q = borg::queen::deliver(&learn::home(), session, repos, &scope, learn::now_ms())?;
    if let Ok(exe) = std::env::current_exe() {
        q = q.run_by(&borg::queen::sh_quote(&exe.display().to_string()));
    }
    let text = borg::queen::render_delivery(&q, harness);
    log_cost(session, &format!("free event=UserPromptSubmit queen={} chars={}", q.id, text.len()));
    Some(text)
}

/// The free context of a prompt or touch: waiting graded lessons first, then an early delivery.
fn free_context(harness: &str, hook_event: &str, session: &str, text: &str) -> Option<Value> {
    free_context_with(harness, hook_event, session, text, None)
}

/// `free_context` plus `extra` (a queen question) last.
fn free_context_with(harness: &str, hook_event: &str, session: &str, text: &str, extra: Option<String>) -> Option<Value> {
    let parts: Vec<String> = [queued(hook_event, session), early(harness, hook_event, session, text), extra].into_iter().flatten().collect();
    (!parts.is_empty()).then(|| json!({"hookSpecificOutput": {"hookEventName": hook_event, "additionalContext": parts.join("\n\n")}}))
}

fn claude_hook(harness: &str, event: &str, input: &Value) -> Option<Value> {
    let session = input.get("session_id").and_then(Value::as_str).unwrap_or("unknown");
    match event {
        "prompt" => {
            let prompt = input.get("prompt").and_then(Value::as_str).unwrap_or_default();
            append(session, prompt);
            if learning() {
                // Where the session started, once: its commits later get reasoning notes (notes.rs).
                borg::notes::remember_start(&borg::notes::state_dir(), session, input.get("cwd").and_then(Value::as_str).unwrap_or_default());
                borg::notes::mark(&borg::notes::state_dir(), session, "prompt");
            }
            // The engineer's own words, separately from the drone's touches (the queen's criterion 3).
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(prompts_file(session)) {
                let _ = writeln!(f, "{prompt}");
            }
            capture_prompt(session, input, prompt);
            note_where(session, &repo_of(input.get("cwd").and_then(Value::as_str).unwrap_or_default()));
            let _ = std::fs::write(state_file(session).with_extension("lastprompt"), prompt);
            // An adapter that will not wait for the answer (`"context": false`) gets none: what is handed
            // over here is marked delivered and would be lost.
            if input.get("context") == Some(&Value::Bool(false)) {
                return None;
            }
            let ask = queen_ask(harness, session, input.get("cwd").and_then(Value::as_str).unwrap_or_default());
            free_context_with(harness, "UserPromptSubmit", session, prompt, ask)
        }
        "post_tool_failure" => {
            capture_rejection(session, input);
            None
        }
        "end" => {
            let _ = std::fs::remove_file(denied_file(session));
            if learning() {
                // The session's landed commits get a redacted reasoning note, in the background.
                borg::notes::remember_proposal(&borg::notes::state_dir(), session, &full_proposal(input));
                borg::notes::mark(&borg::notes::state_dir(), session, "stop");
                borg::notes::on_end(session, input);
            }
            // Generalise this session's corrections in the background; never delay the drone.
            if learning()
                && let Ok(exe) = std::env::current_exe()
            {
                let _ = std::process::Command::new(exe)
                    .args(["learn", "--session", session])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }
            None
        }
        "post_tool" => {
            // The drone moved on from a denial: the next prompt is no longer the engineer's reason.
            let _ = std::fs::remove_file(denied_file(session));
            let tool = input.get("tool_name").and_then(Value::as_str).unwrap_or_default();
            let tool_input = input.get("tool_input").unwrap_or(&Value::Null);
            let touch = touched(tool_input, tool);
            append(session, &touch);
            // A path the drone touched says where it works; a command's free text does not.
            note_where(
                session,
                &if tool == "Bash" { bash_read_paths(tool_input.get("command").and_then(Value::as_str).unwrap_or_default()).join(" ") } else { touch.clone() },
            );
            // BORG_STOP_GATE=edits: this turn changed something, so its Stop asks the model.
            if prefilter::edits_gate() && prefilter::changes(tool, tool_input) {
                let _ = std::fs::OpenOptions::new().create(true).append(true).open(changes_file(session)).and_then(|mut f| writeln!(f, "{tool}"));
            }
            if let Some(edit) = edit_text(tool, input.get("tool_input").unwrap_or(&Value::Null))
                && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(edits_file(session))
            {
                let _ = writeln!(f, "{edit}");
            }
            free_context(harness, "PostToolUse", session, &touch)
        }
        "stop" => {
            if learning() {
                // The drone's latest proposal, for the reasoning note of what it commits (notes.rs).
                borg::notes::remember_proposal(&borg::notes::state_dir(), session, &full_proposal(input));
                borg::notes::mark(&borg::notes::state_dir(), session, "stop");
            }
            if std::env::var("BORG_CRITIC").as_deref() == Ok("generic") {
                if input.get("stop_hook_active").and_then(Value::as_bool) == Some(true) {
                    return None;
                }
                return Some(json!({"decision": "block", "reason": conscience::GENERIC}));
            }
            let all = lessons(session);
            // R1: the drone's answer to the last block's questions, at the first turn end after it: evidence
            // for the judge at the next model call (`pending`), not a label.
            note_answer(session, input);
            // BORG_STOP_GATE=edits: whether this turn changed anything; every Stop consumes the marker, so
            // the rework after a block does not count for the next turn.
            let changed = std::fs::remove_file(changes_file(session)).is_ok();
            // One round of self-questioning per stop; never loop.
            if input.get("stop_hook_active").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            stat(session, input, borg::stats::TURN_END, |_| {});
            let mut context = std::fs::read_to_string(state_file(session)).unwrap_or_default();
            // A lesson is asked once per session: the drone answered it at that turn end, and asking it again
            // at every later one is noise (and a model call spent on it). A lesson delivered early
            // (`#borg-early`) is not asked again either.
            let state = context.clone();
            let mut done = std::collections::HashSet::new();
            // R3b: an early lesson the drone acted on is done; one it did not act on may still be asked.
            let acted = early_acted(session, &all, &full_proposal(input));
            if std::env::var_os("BORG_ONLY").is_none() {
                done = listed(&state, "#borg-asked ");
                done.extend(listed(&state, "#borg-early ").into_iter().chain(listed(&state, "#borg-queued ")).filter(|i| acted.contains(*i)));
            }
            // Lessons of other systems (systems.rs) are neither shortlisted nor asked.
            let challenges: Vec<conscience::Challenge> = all.iter().filter(|c| !done.contains(c.id.as_str()) && !c.off_system).cloned().collect();
            let message = input.get("last_assistant_message").and_then(Value::as_str).unwrap_or_default();
            context.push_str(message);
            // Scopes at Stop: where the session works plus this turn's own message, not the whole history.
            let scope_text = format!("{}\n{message}", where_now(session));
            // BORG_ONLY: eval oracle, perfect retrieval = exactly these challenge ids.
            let mut selected: Vec<&conscience::Challenge> = match std::env::var("BORG_ONLY") {
                Ok(ids) => challenges.iter().filter(|c| ids.split(',').any(|i| i == c.id)).collect(),
                // Word matching alone always finds *some* lesson, so it asked at every turn end; the
                // consequence model may abstain, and it is the mechanism the sealed eval measured.
                Err(_) if std::env::var("BORG_MATCH").as_deref() != Ok("bm25") => {
                    // The free gate (prefilter.rs): a turn whose words touch no lesson costs no model
                    // call; the conscience then asked nothing, and the queen (0 tokens) decides as usual.
                    // BORG_STOP_GATE=edits instead: a turn that changed nothing costs no model call.
                    let skip = if prefilter::edits_gate() {
                        (!changed).then(|| "noedit".to_string())
                    } else {
                        prefilter::skip(&challenges, &context).map(|score| format!("{score:.2}"))
                    };
                    if let Some(score) = skip {
                        append(session, &format!("#borg-skip {score}"));
                        stat(session, input, borg::stats::GATE_SKIP, |e| e.why = score.clone());
                        let prompts = std::fs::read_to_string(prompts_file(session)).unwrap_or_default();
                        return borg::queen::stop(session, input, &prompts, &[]);
                    }
                    // BM25's top 30, plus lessons the vector index finds that word matching missed (vectors.rs;
                    // here only, where the model call dwarfs loading the embedder). Top 30 found as much as
                    // top 20 + vectors; both together find the most (reports/vectors.md).
                    let started = std::time::Instant::now();
                    let shortlist_now = || {
                        let (list, added) = borg::vectors::shortlist(&lessons_path(), &challenges, &context, &scope_text, borg::vectors::SHORTLIST);
                        if borg::vectors::enabled() {
                            log_cost(session, &format!("vectors added={added} ms={}", started.elapsed().as_millis()));
                        }
                        list
                    };
                    // The whole pool (whole.rs, default): every lesson the Stop allows, up to a token budget; the
                    // shortlist's order decides only when they do not all fit. BORG_STOP_FORMAT=classic asks
                    // about that pool in the shortlist's layout (predict::prompt).
                    let whole = borg::whole::on();
                    let shortlist = if whole {
                        let ranked = conscience::rank_scoped(&challenges, &context, &scope_text);
                        let eligible = borg::whole::eligible(&challenges, &ranked, &scope_text);
                        let n = eligible.len();
                        let (shown, fell) = borg::whole::select(eligible, shortlist_now, &ranked, borg::whole::tokens());
                        log_cost(session, &format!("pool mode=all eligible={n} shown={} budget={}", shown.len(), if fell { "full" } else { "fits" }));
                        shown
                    } else {
                        shortlist_now()
                    };
                    let model = std::env::var("BORG_PREDICT_MODEL").unwrap_or_default();
                    // R1: the same call judges what the drone did with lessons asked earlier this session.
                    let pend = pending(session, &state, &all, &full_proposal(input));
                    let extra = predict::judge_section(&pend).len();
                    let graded = borg::whole::graded();
                    let called = std::time::Instant::now();
                    let pred = if whole && !borg::whole::classic() {
                        predict::predict_whole(&context, &shortlist, &pend, &model, graded)
                    } else {
                        predict::predict(&context, &shortlist, &pend, &model, graded)
                    };
                    stat(session, input, borg::stats::CHECK, |e| {
                        e.n = shortlist.len() as u64;
                        e.dur_ms = called.elapsed().as_millis() as u64;
                        e.usd = pred.as_ref().map_or(0.0, |p| p.cost);
                        if pred.is_none() {
                            e.why = "failed".into();
                        }
                    });
                    let pred = pred?;
                    append(session, &format!("#borg-predict-cost {:.4}", pred.cost));
                    if !pend.is_empty() {
                        let call = state.lines().filter(|l| l.starts_with("#borg-predict-cost")).count() + 1;
                        record_verdicts(session, input, &pend, &pred.resolved, extra, call);
                    }
                    let applied: Vec<(&conscience::Challenge, u8)> =
                        pred.applies.iter().zip(&pred.grades).filter_map(|(i, g)| shortlist.iter().find(|c| &c.id == i).map(|c| (*c, *g))).collect();
                    if graded {
                        // Graded (default): block only for a lesson graded 3; a turn without a block hands
                        // its 2s and 1s over free at the next prompt or touch (conscience.rs `graded`).
                        let (ask, free) = conscience::graded(&applied);
                        let queue = !free.is_empty() && harness == "claude";
                        log_cost(
                            session,
                            &format!("graded applied={} ask={} free={} {}", applied.len(), ask.len(), free.len(), if queue { "queued" } else { "dropped" }),
                        );
                        if queue && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(queued_file(session)) {
                            let _ = writeln!(f, "{}", free.iter().map(|c| c.id.as_str()).collect::<Vec<_>>().join(","));
                        }
                        ask
                    } else {
                        // R3b: the model decides whether to block (an extra round); once it does, the block
                        // also carries every other lesson over the low bar (conscience.rs `widen`).
                        let predicted: Vec<&conscience::Challenge> = applied.iter().map(|(c, _)| *c).collect();
                        let widened = conscience::widen(&conscience::rank_scoped(&all, &context, &scope_text), &predicted, &done);
                        widened.into_iter().filter_map(|c| challenges.iter().find(|x| x.id == c.id)).collect()
                    }
                }
                // R3b (conscience.rs `budget`): block only for a lesson near the best match not yet done;
                // then ask everything over the low bar, in-scope first.
                Err(_) => conscience::budget(&conscience::rank_scoped(&all, &context, &scope_text), &done)
                    .into_iter()
                    .filter_map(|c| challenges.iter().find(|x| x.id == c.id))
                    .collect(),
            };
            // Shown in an order stable within the session but different across sessions: no lesson always
            // sits first (position bias; memory.rs `shuffle`).
            borg::memory::shuffle(&mut selected, session);
            let log = selected.iter().map(|c| c.id.as_str()).collect::<Vec<_>>().join(",");
            append(session, &format!("#borg-asked {log}"));
            if !selected.is_empty()
                && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(edits_file(session))
            {
                // The blocked message is the work before the questions (R1 `pending`).
                let _ = writeln!(f, "{}\n#borg-asked {log}", one_line(&full_proposal(input), 4000));
            }
            // L4 (queen.rs): a rare, budgeted question to the engineer when the conscience had nothing
            // or its lessons contradict each other (its block then carries the lessons too).
            let prompts = std::fs::read_to_string(prompts_file(session)).unwrap_or_default();
            let queen = borg::queen::stop(session, input, &prompts, &selected);
            if selected.is_empty() {
                return queen;
            }
            if learning() {
                // Outcome tracking (outcome.rs): `borg learn` later sees whether a correction followed.
                let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
                let asked = outcome::Outcome {
                    kind: outcome::ASKED.into(),
                    session: session.into(),
                    lessons: selected.iter().map(|c| c.id.clone()).collect(),
                    ms: learn::now_ms(),
                    repo: repo_of(cwd),
                    groups: conscience::disagreement_ids(&selected),
                };
                let _ = outcome::append(&learn::home().join("outcomes.jsonl"), &asked);
            }
            let out = queen.or_else(|| Some(json!({"decision": "block", "reason": conscience::render(&selected)})));
            let chars = out.as_ref().and_then(|o| o["reason"].as_str()).map_or(0, str::len);
            log_cost(session, &format!("block questions={} chars={chars}", selected.len()));
            stat(session, input, borg::stats::BLOCK, |e| {
                e.n = selected.len() as u64;
                e.lessons = selected.iter().map(|c| c.id.clone()).collect();
            });
            out
        }
        _ => None,
    }
}

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

/// `borg help`: the commands grouped by what they are for, one line each.
const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Everyday",
        &[
            ("status", "each collective's lessons, outcomes and wiring"),
            ("doctor", "what is broken: config, hooks, adapters, the claude CLI"),
            ("top [--watch] [--json] [--dir DIR]", "the drones running across the team"),
            ("web [--port PORT] [--dir DIR]", "a live page of knowledge and stats on 127.0.0.1"),
            ("why <path>[:<line>[-<line>]] | <commit>", "the reasoning behind code  [--full] [--json]"),
            ("queen [show|ask|answer|skip|dismiss|next]", "open decisions waiting for you"),
        ],
    ),
    (
        "Setup",
        &[
            ("install <claude|omp|opencode|all> [--apply]", "wire the hooks (a dry run without --apply)"),
            ("uninstall <claude|omp|opencode|all> [--apply]", "unwire them"),
            ("collective [DIR]", "which collective a directory belongs to"),
            ("vectors [--fetch|--rebuild|--query]", "the vector model and index"),
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
        ],
    ),
    (
        "Team",
        &[
            ("mesh <command>", "sync with team members (borg mesh --help)"),
            ("steer <drone|all> <text> | --pause | --resume", "a signed instruction to running drones"),
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
            ("hook <claude|omp|opencode> <event>", "what the harness runs; not for people"),
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
    let width = HELP.iter().flat_map(|(_, rows)| rows.iter().map(|(c, _)| c.chars().count())).max().unwrap_or(0);
    let mut out = format!("{}\n\n{} borg <command> [options]\n", bold("borg: a shared memory for coding agents"), bold("Usage:"));
    for (group, rows) in HELP {
        out.push_str(&format!("\n{}\n", bold(group)));
        for (cmd, what) in *rows {
            out.push_str(&format!("  {cmd:<width$}  {what}\n"));
        }
    }
    out.push_str("\nMore: docs/commands.md, or https://github.com/debe/borg_ai\n");
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["hook", harness @ ("claude" | "omp" | "opencode"), event] => {
            let mut raw = String::new();
            let _ = std::io::stdin().read_to_string(&mut raw);
            let input: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            // Which mind is this repo part of? Decided before anything is read or written: an off repo
            // (or a broken config) leaves no trace at all.
            let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
            let route = match collective::load() {
                Ok(cfg) => collective::route(cfg.as_ref(), cwd, &learn::home()),
                Err(_) => collective::Route::Off,
            };
            let collective::Route::Collective { home, name, .. } = route else { return };
            // SAFETY: single-threaded at this point; everything below (and the background `borg learn`
            // it spawns) inherits the collective's own store and scratch state.
            unsafe {
                std::env::set_var("BORG_HOME", &home);
                // borg's own model calls go through the drone's harness (runner.rs).
                std::env::set_var("BORG_HARNESS", harness);
                if std::env::var_os("BORG_STATE").is_none() {
                    std::env::set_var("BORG_STATE", home.join("state"));
                }
            }
            // Who is working where (presence.rs, for `borg top`): one small atomic write per hook.
            let now = learn::now_ms();
            // Not at pre_tool (every tool call): there only steering reads, and only once someone steered.
            let session = input.get("session_id").and_then(Value::as_str).unwrap_or("unknown");
            let me = match *event {
                "pre_tool" if borg::steer::inbox(&home).exists() => borg::presence::read(&home, session),
                "pre_tool" => return,
                _ => borg::presence::touch(&home, &name, harness, event, &input, now),
            };
            // Signed steering (steer.rs) wraps the answer: shown once, and a pause holds tools and the stop.
            // At a stop, the routed collective's experts join the conscience's block (expert_route.rs).
            if let Some(out) = borg::steer::around(&home, harness, event, &input, me.map(|p| p.started), now, || {
                borg::expert_route::checkpoint(event, &input, || claude_hook(harness, event, &input))
            }) {
                println!("{out}");
            }
        }
        ["collective", rest @ ..] => {
            // Which collective a directory belongs to (default: the current one), and whether it may sync.
            // Routing compares path text, so a relative argument ("." or "../x") is made absolute first, as hooks' cwd is.
            let dir = rest.first().map(std::path::PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let cwd = std::path::absolute(&dir).unwrap_or(dir).display().to_string();
            match collective::load() {
                Err(e) => println!("off (invalid config: {e})"),
                Ok(cfg) => match collective::route(cfg.as_ref(), &cwd, &learn::home()) {
                    collective::Route::Off => println!("off: {cwd} belongs to no collective; hooks do nothing here"),
                    r @ collective::Route::Collective { .. } => {
                        let collective::Route::Collective { name, home, share } = &r else { unreachable!() };
                        println!("{name} ({share:?}, may sync: {}) store {}", collective::may_sync(&r), home.display());
                    }
                },
            }
        }
        [cmd @ ("mesh" | "steer"), rest @ ..] => {
            // The mesh is its own binary (crates/borg-mesh): the networking stack stays out of the hook.
            // So is steering: it signs with the member key and checks the membership (steer.rs).
            use std::os::unix::process::CommandExt;
            let beside = std::env::current_exe().ok().map(|e| e.with_file_name("borg-mesh")).filter(|p| p.exists());
            let args: Vec<&str> = if *cmd == "steer" { std::iter::once("steer").chain(rest.iter().copied()).collect() } else { rest.to_vec() };
            let err = std::process::Command::new(beside.unwrap_or_else(|| "borg-mesh".into())).args(args).exec();
            eprintln!("borg mesh: cannot run borg-mesh ({err}); cargo install --path crates/borg-mesh");
            std::process::exit(1);
        }
        ["web", rest @ ..] => {
            // The live page (crates/borg-web): its web stack stays out of the hook binary too.
            use std::os::unix::process::CommandExt;
            let beside = std::env::current_exe().ok().map(|e| e.with_file_name("borg-web")).filter(|p| p.exists());
            let err = std::process::Command::new(beside.unwrap_or_else(|| "borg-web".into())).args(rest).exec();
            eprintln!("borg web: cannot run borg-web ({err}); cargo install --path crates/borg-web");
            std::process::exit(1);
        }
        ["learn", rest @ ..] => {
            borg::stats::set_purpose("learn");
            let session = rest.iter().position(|a| *a == "--session").and_then(|i| rest.get(i + 1)).copied();
            let store = learn::home().join("lessons.jsonl");
            let from = borg::facets::len_of(&store);
            let (new, merged, waiting) =
                learn::learn_in_reporting(&learn::home(), session, &learn::ask_claude, learn::learning(), learn::now_ms(), &mut learn::print_step);
            println!("{new} new lessons, {merged} merged into existing ones");
            if waiting > 0 {
                eprintln!("borg learn: the model gave no answer; {waiting} episodes still pending, re-run `borg learn` later");
            }
            facets_for_written(&store, from);
            borg::vectors::refresh(&store);
            // Session end runs this in the background: publish today's counts (stats.rs) for the mesh.
            borg::stats::rollup(&learn::home());
        }
        ["vectors", rest @ ..] => std::process::exit(borg::vectors::main(rest)),
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
        ["prefilter", path, rest @ ..] => {
            // The Stop gate's score for a state file + final message on stdin (eval/preempt/stop_prefilter.py).
            let before = rest.first().and_then(|v| v.parse().ok());
            let challenges = conscience::load(path.as_ref(), before);
            let mut context = String::new();
            let _ = std::io::stdin().read_to_string(&mut context);
            println!("{}", prefilter::features(&challenges, &context));
        }
        ["memory", path, rest @ ..] => {
            // Each lesson's memory (memory.rs) from the outcomes beside it, as JSON lines; with --session,
            // that session's view (its clock tick and usefulness sample).
            let session = rest.iter().position(|a| *a == "--session").and_then(|i| rest.get(i + 1)).copied();
            let path = std::path::Path::new(path);
            let lessons = conscience::load_unweighted(path, None);
            let outcomes = outcome::read(&outcome::path_beside(path));
            let memory = borg::memory::derive(&outcomes, &lessons, session);
            let weights = outcome::weights(&outcomes);
            for c in &lessons {
                let m = &memory[&c.id];
                println!(
                    "{}",
                    json!({"id": c.id, "activation": m.activation, "changed": m.changed, "ignored": m.ignored, "corrected": m.corrected,
                        "mean": m.mean(), "weight": m.weight(&c.id, session), "consolidated": m.consolidated, "dormant": m.dormant,
                        "outcome_weight": weights.get(&c.id).copied().unwrap_or(1.0)})
                );
            }
        }
        ["resolve", path] => {
            // R1 offline: {"asked": [ids], "before": text, "edits": text, "final": text, "min_new": n} on stdin
            // -> {"changed", "ignored"}.
            let mut raw = String::new();
            let _ = std::io::stdin().read_to_string(&mut raw);
            let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            let all = conscience::load_unweighted(path.as_ref(), None);
            let ids: Vec<&str> = v["asked"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let asked: Vec<&conscience::Challenge> = ids.iter().filter_map(|i| all.iter().find(|c| c.id == *i)).collect();
            let min = v["min_new"].as_u64().map_or(borg::memory::NOVEL_WORDS, |n| n as usize);
            let s = |k: &str| v[k].as_str().unwrap_or_default();
            let (changed, ignored) = borg::memory::resolve(&asked, &all, s("before"), s("edits"), s("final"), min);
            println!("{}", json!({"changed": changed, "ignored": ignored}));
        }
        ["recall", path, rest @ ..] => {
            let before = rest.first().and_then(|v| v.parse().ok());
            let challenges = conscience::load(path.as_ref(), before);
            let mut context = String::new();
            let _ = std::io::stdin().read_to_string(&mut context);
            for (score, c) in conscience::rank(&challenges, &context) {
                println!("{} {score:.3}", c.id);
            }
        }
        ["expert", repo, rest @ ..] => {
            let flag = |f: &str| rest.iter().position(|a| *a == f).and_then(|i| rest.get(i + 1)).copied();
            let Some(at) = flag("--at") else {
                eprintln!("borg expert: --at <commit> is required");
                std::process::exit(2);
            };
            let query = match flag("--q") {
                Some(q) => q.to_string(),
                None => {
                    let mut q = String::new();
                    let _ = std::io::stdin().read_to_string(&mut q);
                    q
                }
            };
            match expert::ask(repo.as_ref(), at, flag("--version"), &query, std::env::var(expert::CEILING_ENV).ok().filter(|c| !c.is_empty()).as_deref()) {
                Ok(answer) => print!("{}", answer.render()),
                Err(e) => {
                    eprintln!("borg expert: {e}");
                    std::process::exit(1);
                }
            }
        }
        ["queen", rest @ ..] => std::process::exit(borg::queen::main(rest)),
        ["notes", rest @ ..] => std::process::exit(borg::notes::main(rest)),
        ["trust", rest @ ..] => std::process::exit(borg::trust::main(rest)),
        ["top", rest @ ..] => std::process::exit(borg::top::main(rest)),
        ["why", rest @ ..] => std::process::exit(borg::why::main(rest)),
        [cmd @ ("install" | "uninstall" | "status" | "doctor"), rest @ ..] => std::process::exit(install::main(cmd, rest)),
        [] | ["help" | "-h" | "--help"] => print!("{}", help(atty(1))),
        _ => {
            eprint!("{}", help(atty(2)));
            std::process::exit(2);
        }
    }
}
