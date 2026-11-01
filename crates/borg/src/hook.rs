//! The hook: every harness's events, one canonical, Claude-shaped event each (session_id, cwd, prompt,
//! tool_name, tool_input, error, last_assistant_message, transcript_path, stop_hook_active), answered
//! Claude-shaped ({"decision":"block","reason"} at stop, `hookSpecificOutput` with context at a prompt
//! or tool call); adapters translate both ways (harness/). What differs per harness comes from its
//! [`Harness`]: capabilities, the turn's work and the drone's last message, never its name.
//!
//! `main` routes and records presence; [`dispatch`] has one function per event. Hooks always fail open:
//! any error lets the drone continue untouched.

use std::io::{Read, Write};
use std::path::PathBuf;

use borg_trace::fingerprint::bash_read_paths;
use serde_json::{Value, json};

use crate::harness::{self, Harness};
use crate::learn::learning;
use crate::{collective, conscience, learn, live, outcome, predict, prefilter};

pub(crate) fn state_file(session: &str) -> PathBuf {
    let dir = std::env::var_os("BORG_STATE").map_or_else(|| PathBuf::from("/tmp/borg"), PathBuf::from);
    let _ = std::fs::create_dir_all(&dir);
    let safe: String = session.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    dir.join(format!("{safe}.ctx"))
}

/// One line (or several, kept together) at the end of the state file, in one write: a check running in
/// the background appends to the same file, and its lines never land between these.
pub(crate) fn append(session: &str, text: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(state_file(session)) {
        let _ = f.write_all(format!("{text}\n").as_bytes());
    }
}

/// What a tool call touched, as text for matching: paths and search patterns.
pub(crate) fn touched(input: &Value, tool: &str) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    match tool {
        "Bash" => {
            let cmd = s("command");
            let paths = bash_read_paths(&cmd);
            if paths.is_empty() { cmd.chars().take(COMMAND_CHARS).collect() } else { paths.join(" ") }
        }
        "Grep" => format!("{} {}", s("pattern"), s("path")),
        "Glob" => s("pattern"),
        _ => {
            let mut t = [s("file_path"), s("notebook_path"), s("path")].join(" ").trim().to_string();
            // An edit of several files (a Codex patch) names the further ones in its `edits`.
            for p in further_files(input) {
                if !t.is_empty() {
                    t.push(' ');
                }
                t.push_str(p);
            }
            t
        }
    }
}

/// A shell command's text kept as its touch when it names no path (`touched`). Guard: bounds one
/// state-file line (a heredoc would otherwise go in whole). borg-trace's `TOUCH_CHARS`, which a replay
/// of this hook mirrors.
const COMMAND_CHARS: usize = borg_trace::dialogue::TOUCH_CHARS;

/// The files an edit names in its `edits` beside its own `file_path` (a Codex patch of several files;
/// Claude's MultiEdit names none there). They reach `.ctx` and `.edits`; presence (`borg top`) shows
/// one file per drone, the edit's own `file_path`.
pub(crate) fn further_files(input: &Value) -> impl Iterator<Item = &str> {
    input.get("edits").and_then(Value::as_array).into_iter().flatten().filter_map(|e| e.get("file_path").and_then(Value::as_str)).filter(|p| !p.is_empty())
}

pub(crate) fn repo_of(cwd: &str) -> String {
    borg_trace::repo_key(cwd).rsplit('/').next().unwrap_or_default().to_string()
}

/// Counts one event of this session in the collective's statistics (stats.rs); `fill` adds details.
fn stat(session: &str, input: &Value, kind: &str, fill: impl FnOnce(&mut crate::stats::Event)) {
    let home = learn::home();
    let mut ev = crate::stats::Event::new(kind, learn::now_ms());
    ev.session = session.into();
    ev.repo = repo_of(input.get("cwd").and_then(Value::as_str).unwrap_or_default());
    // The engineer as presence.rs already resolved it at this hook: no second `git config` call.
    ev.engineer = crate::presence::read(&home, session).map(|p| p.engineer).unwrap_or_default();
    fill(&mut ev);
    crate::stats::record(&home, &ev);
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
fn capture_prompt(h: &dyn Harness, session: &str, input: &Value, prompt: &str) {
    if !learning() {
        return;
    }
    let task = std::fs::read_to_string(task_file(session)).unwrap_or_default();
    if let Ok(tool) = std::fs::read_to_string(denied_file(session)) {
        let _ = std::fs::remove_file(denied_file(session));
        let said = prompt.trim();
        if said.len() > 3 && !said.starts_with('<') {
            record(session, input, task, proposal(h, input), Some(tool), said.chars().take(learn::EXCERPT_CHARS).collect());
            return;
        }
    }
    if !learn::is_correction(prompt) || task.is_empty() {
        let task: String = prompt.chars().take(learn::TASK_CHARS).collect();
        let _ = std::fs::write(task_file(session), task);
        return;
    }
    record(session, input, task, proposal(h, input), None, prompt.chars().take(learn::EXCERPT_CHARS).collect());
}

/// A veto said in the session (veto.rs): its episode, the shown lessons' credit and its stat. A pending
/// wordless denial is answered by it, so it never becomes a correction later.
fn veto(h: &dyn Harness, session: &str, input: &Value, reason: &str) {
    let _ = std::fs::remove_file(denied_file(session));
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
    let task = std::fs::read_to_string(task_file(session)).unwrap_or_default();
    let said = crate::veto::Said {
        session,
        repo: &repo_of(cwd),
        engineer: &learn::engineer(cwd),
        task: &task,
        agent: &proposal(h, input),
        reason,
        ms: learn::now_ms(),
        via: "prompt",
    };
    if let Ok(v) = crate::veto::record(&learn::home(), &said) {
        append(session, &format!("#borg-episode {}", v.episode));
    }
}

/// A rejected tool call or plan that carries the engineer's words becomes an episode; a wordless
/// denial waits for the next prompt (`capture_prompt`).
fn capture_rejection(h: &dyn Harness, session: &str, input: &Value) {
    if !learning() {
        return;
    }
    let error = input.get("error").and_then(Value::as_str).unwrap_or_default();
    let tool = format!(
        "{} {}",
        input.get("tool_name").and_then(Value::as_str).unwrap_or_default(),
        input.get("tool_input").map(|v| v.to_string()).unwrap_or_default().chars().take(learn::EXCERPT_CHARS).collect::<String>()
    );
    let Some(said) = learn::rejection(error) else {
        if learn::denied(error) {
            let _ = std::fs::write(denied_file(session), tool);
        }
        return;
    };
    let task = std::fs::read_to_string(task_file(session)).unwrap_or_default();
    record(session, input, task, proposal(h, input), Some(tool), said);
}

/// The drone's last proposal, its last `learn::PROPOSAL_CHARS` characters (`harness::last_message`: the
/// adapter's, else the transcript's).
fn proposal(h: &dyn Harness, input: &Value) -> String {
    let t = full_proposal(h, input);
    t.chars().rev().take(learn::PROPOSAL_CHARS).collect::<Vec<_>>().into_iter().rev().collect()
}

/// The drone's whole last proposal (`proposal` keeps its end): the reasoning note
/// redacts it before it cuts (notes.rs).
fn full_proposal(h: &dyn Harness, input: &Value) -> String {
    harness::last_message(h.caps(), input)
}

fn record(session: &str, input: &Value, task: String, agent: String, rejected_tool: Option<String>, correction: String) {
    let ms = learn::now_ms();
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
    let e = learn::Episode {
        id: format!("{}-{ms}", session.chars().take(8).collect::<String>()),
        ms,
        repo: repo_of(cwd),
        session: session.into(),
        task,
        agent,
        rejected_tool,
        correction,
        engineer: learn::engineer(cwd),
        ..learn::Episode::default()
    };
    let _ = learn::record_episode(&e);
    append(session, &format!("#borg-episode {}", e.id));
}

/// The lessons the conscience reads (BORG_CONSCIENCE, the BORG_BEFORE cut-off) as `session` sees them:
/// weighted by their memory with this session's usefulness sample (memory.rs), without the ones it may
/// not deliver here (systems, reach, absorbed). Every hook path that hands lessons to a drone reads them
/// here.
fn lessons(session: &str) -> Vec<conscience::Challenge> {
    loaded(session).0
}

/// Every lesson the judge gives a verdict on: `all` and the absorbed ones withheld from it (`loaded`).
/// A delivery made before its lesson was absorbed is evidence about the lesson, not a delivery, so the
/// judge still sees it (`Work::judged`).
fn judged(all: &[conscience::Challenge], withheld: Vec<conscience::Challenge>) -> std::borrow::Cow<'_, [conscience::Challenge]> {
    if withheld.is_empty() { all.into() } else { all.iter().cloned().chain(withheld).collect::<Vec<_>>().into() }
}

/// `lessons`, and the lessons the absorbed filter withheld from it (absorb.rs `withhold`).
fn loaded(session: &str) -> (Vec<conscience::Challenge>, Vec<conscience::Challenge>) {
    let before = std::env::var("BORG_BEFORE").ok().and_then(|v| v.parse().ok());
    // The routed collective's lessons and this engineer's personal ones of it (reach.rs), nothing else.
    let mut all = conscience::load_store_for(&lessons_path(), before, Some(session));
    // The system gate (systems.rs): where the repo says which system this is, lessons scoped to other
    // systems score 0, however much the conversation names them.
    let where_text = std::fs::read_to_string(where_file(session)).unwrap_or_default();
    let repos = crate::systems::work_repos(&where_text);
    if crate::systems::on() {
        crate::systems::gate(&mut all, &repos);
    }
    // Reach (reach.rs): a tombstone is never delivered, a repo convention only in its repo.
    all.retain(|c| crate::reach::deliverable(c, &repos));
    // Absorbed (absorb.rs): held outside Borg now, so delivered on no path; every path reads this list.
    let withheld = crate::absorb::withhold_beside(&lessons_path(), &mut all, before);
    // Spike (concepts.rs): lessons sharing engineering concepts with the code written, the touched paths
    // and the latest prompt rank higher; never a filter.
    if crate::concepts::on() {
        let where_text = std::fs::read_to_string(where_file(session)).unwrap_or_default();
        let edits = std::fs::read_to_string(edits_file(session)).unwrap_or_default();
        let latest = std::fs::read_to_string(state_file(session).with_extension("lastprompt")).unwrap_or_default();
        crate::concepts::apply(&mut all, &crate::concepts::of_work(&edits, &where_text, &latest), crate::concepts::beta());
    }
    (all, withheld)
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
/// `#borg-queued`) that the drone acted on since: its work after the delivery brings in the lesson's words
/// (memory.rs `resolve`, `final_msg` included). Stop leaves them alone; an early lesson not acted on can
/// still be asked at Stop. A later delivery's own work line (`WORK`: a free delivery's, a placebo's; a live
/// ask's before 0.7.6) is neither: it is the work before that delivery, written whatever its route, so how
/// a later lesson was routed never decides whether this one was acted on.
fn early_acted(session: &str, all: &[conscience::Challenge], final_msg: &str) -> std::collections::HashSet<String> {
    let edits = std::fs::read_to_string(edits_file(session)).unwrap_or_default();
    let state = std::fs::read_to_string(state_file(session)).unwrap_or_default();
    let lines: Vec<&str> = edits.lines().filter(|l| !l.starts_with(WORK)).collect();
    let mut out = std::collections::HashSet::new();
    for (p, l) in lines.iter().enumerate() {
        let Some(list) = l.strip_prefix("#borg-early ").or_else(|| l.strip_prefix("#borg-queued ")) else { continue };
        let ids: std::collections::HashSet<&str> = ids_in(list).collect();
        let asked: Vec<&conscience::Challenge> = all.iter().filter(|c| ids.contains(c.id.as_str())).collect();
        let after: Vec<&str> = lines[p + 1..].iter().copied().filter(|l| !l.starts_with("#borg-")).collect();
        let upto = state.find(&format!("{l}\n")).map_or(state.as_str(), |i| &state[..i]);
        let before = format!("{upto}\n{}", lines[..p].join("\n"));
        out.extend(crate::memory::resolve(&asked, all, &before, &after.join("\n"), final_msg, crate::memory::NOVEL_WORDS).0);
    }
    out
}

/// Done in this session: asked as a question (at a stop, or live before 0.7.6), or handed over (early,
/// free) and acted on since (`early_acted`, its `acted`). The one definition a stop's pool leaves out: a
/// lesson handed over but not acted on may be graded again, and when its grade asks it the question
/// reaches the drone.
fn done_in<'a>(state: &'a str, acted: &std::collections::HashSet<String>) -> std::collections::HashSet<&'a str> {
    let mut done = listed(state, "#borg-asked ");
    done.extend(listed(state, "#borg-early ").into_iter().chain(listed(state, "#borg-queued ")).filter(|i| acted.contains(*i)));
    done
}

/// Asked or handed over in this session, acted on or not: free context comes once per session, so
/// none of these goes free again (`queued`, `early`), done or not.
fn had_in(state: &str) -> std::collections::HashSet<&str> {
    let mut had = listed(state, "#borg-asked ");
    had.extend(listed(state, "#borg-early "));
    had.extend(listed(state, "#borg-queued "));
    had
}

/// The .edits line before a free delivery's or a placebo's markers (a live ask's before 0.7.6): the work so far
/// (`work_so_far`), the evidence `pending` reads before it as a question's blocked message. Not work
/// itself: `early_acted` and a judge's "work since" skip it, as every `#borg-` line, so an earlier
/// delivery's evidence never depends on how a later lesson was routed (`note_work`).
const WORK: &str = "#borg-work ";

/// An edit tool's paths (every file it names, so the cut never drops one) and the start of its new
/// text, on one line; None for other tools.
pub(crate) fn edit_text(tool: &str, input: &Value) -> Option<String> {
    if !["edit", "write", "multiedit", "notebookedit"].contains(&tool.to_ascii_lowercase().as_str()) {
        return None;
    }
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let mut text = [s(input, "file_path"), s(input, "notebook_path")].join(" ");
    for p in further_files(input) {
        text.push(' ');
        text.push_str(p);
    }
    for k in ["new_string", "content", "new_source"] {
        text.push(' ');
        text.push_str(&s(input, k));
    }
    for e in input.get("edits").and_then(Value::as_array).into_iter().flatten() {
        text.push(' ');
        text.push_str(&s(e, "new_string"));
    }
    Some(text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(EDIT_CHARS).collect())
}

/// An edit's line in the `.edits` and turn files (`edit_text`): its paths and the start of its new text.
/// Guard: bounds one evidence line (a whole written file would otherwise go in).
const EDIT_CHARS: usize = 600;

/// The longest drone message one evidence line of the `.edits` file keeps: the blocked message before
/// a question, the work so far before a free delivery, the answer after a block (`pending`).
/// Guard: bounds a line of a file every judge call reads; at least what the judge reads after a
/// delivery (`predict::JUDGE_PART` three times), so the cut never takes evidence the judge would see.
/// Not a context budget: none of it is injected into the drone.
const EVIDENCE_CHARS: usize = 4000;
const _: () = assert!(EVIDENCE_CHARS >= 3 * predict::JUDGE_PART);

/// `text` on one line, at most `n` characters.
pub(crate) fn one_line(text: &str, n: usize) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(n).collect()
}

/// R1: the first turn end after a Stop block is the drone's answer to it. Its message (with any
/// `<id>: applies|skip`) goes to the .edits file as `#borg-answer`: evidence for the judge (`pending`),
/// never a label by itself.
fn note_answer(h: &dyn Harness, session: &str, input: &Value) {
    let edits = std::fs::read_to_string(edits_file(session)).unwrap_or_default();
    let lines: Vec<&str> = edits.lines().collect();
    let Some(i) = lines.iter().rposition(|l| l.starts_with("#borg-asked ")) else { return };
    if lines[i + 1..].iter().any(|l| l.starts_with("#borg-answer ")) {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(edits_file(session)) {
        let _ = writeln!(f, "#borg-answer {}", one_line(&full_proposal(h, input), EVIDENCE_CHARS));
    }
}

/// Pending lessons sent to a check's judge per call at most (the most recently delivered). Guard: bounds
/// one check's judge section, whatever a session delivered, so the stop's latency stays bounded; derived
/// from one full block (conscience.rs `CAP`), whose lessons share their evidence in the prompt. A lesson
/// it leaves out is logged (`#borg-unjudged <ids> why=slots`), so its hit rate is measured, and is judged
/// at the session's end at the latest, which has no slots (`judge_session`).
const MAX_PENDING: usize = conscience::CAP;
/// A lesson whose verdicts stay pending (unclear, missing) is sent to a check's judge this often at
/// most; after that it waits for the session-end judge (`judge_session`), which judges it once more
/// whatever its tries, then for `borg learn` to settle it as `asked_not_corrected` (censored, reason
/// `tries`). Guard: a lesson the judge never settles would otherwise ride on every later call. A failed
/// call is no try: its lessons stay pending as they were (`#borg-unjudged … why=failed`).
const MAX_JUDGE_TRIES: usize = 2;

/// The session-end judge's prompt budget (`end_budget`) before any evidence of the judge's effective
/// length: characters of a batch's whole prompt (`predict::judge_prompt`). Baseline (0.7.6
/// pre-registration, 2026-11-01): 0.7.5's 24,000 characters of work and its 40 lessons at
/// `predict::JUDGE_PART` (1,200) each. Not the largest prompt 0.7.5's one session-end call could send:
/// with every lesson's full evidence (1,200 before, 3,600 after, its question and consequence) that was
/// about 220,000; the pre-registered value is kept, well below it, so batching never asks more of the
/// judge than 0.7.5 could before the measured effective length says it may.
pub(crate) const END_START_BUDGET: usize = 72_000;

/// A session-end batch's lessons (`predict::judge_section`) take at most 1/this of its budget; the
/// session's work gets the rest (`batches`). Guard: the work, every lesson's evidence, is never crowded
/// out by the lessons beside it.
const END_LESSONS_SHARE: usize = 2;

/// The session-end judge's budget for one pass (`end_budget`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EndBudget {
    /// Characters a batch's whole prompt may take (`predict::judge_prompt`).
    pub chars: usize,
    /// When the pass drew whether to explore a larger budget: the chance it explored and whether it
    /// did (the cost line's `psi=` and `explore=`).
    pub draw: Option<(f64, bool)>,
}

/// The session-end judge's prompt budget for this pass, for the judge `model` (`judge_model`): its
/// measured effective length from the store's judge cost lines (judge_length.rs `length`, the `.cost`
/// files of `state`), or, with the chance `psi` the estimate gives while the step above it is open, that
/// step, drawn from `seed` and logged (`draw`). `BORG_END_BUDGET=<chars>` pins it (an evaluation switch:
/// no estimate, no draw).
pub(crate) fn end_budget(state: &std::path::Path, model: &str, seed: &str) -> EndBudget {
    if let Some(pinned) = std::env::var("BORG_END_BUDGET").ok().and_then(|v| v.trim().parse::<usize>().ok()).filter(|n| *n > 0) {
        return EndBudget { chars: pinned, draw: None };
    }
    let len = crate::judge_length::length(state, model);
    let mut h = blake3::Hasher::new();
    h.update(b"borg-end-explore-v1\0");
    h.update(seed.as_bytes());
    let bytes: [u8; 8] = h.finalize().as_bytes()[..8].try_into().unwrap_or_default();
    let u = crate::memory::Rng::new(u64::from_le_bytes(bytes)).unit();
    match len.explore {
        Some(up) if u < len.psi => EndBudget { chars: up, draw: Some((len.psi, true)) },
        _ => EndBudget { chars: len.chars, draw: Some((len.psi, false)) },
    }
}

/// The judge's model as cost lines name it (`model=`): the first runner `spec` picks and the model it
/// asks (runner.rs `Choice::label`); empty when none.
pub(crate) fn judge_model(spec: &str) -> String {
    crate::runner::choose(Some(spec)).first().map(crate::runner::Choice::label).unwrap_or_default()
}

/// How a session-end batch cuts a part of the session's work longer than its share of the budget
/// (`BORG_END_WORK`): its newest characters (`newest`, 0.7.5's), from the batch's earliest delivery on
/// (`after`), or half right after that delivery and half the end (`ends`). A part without delivery
/// markers (the agent's messages) keeps its newest characters under each. Which one reads more verdicts
/// out of a long session was the 0.7.6 backtest's question (docs/evidence.md, Lesson selection, 0.7.6
/// pre-registration): `ends` answered the most on lessons delivered long before the end (196 of 196,
/// `newest` 195, `after` 192), so it is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cut {
    Newest,
    After,
    Ends,
}

impl Cut {
    /// `BORG_END_WORK`, else `ends`; an unknown word is the default too.
    fn from_env() -> Cut {
        match std::env::var("BORG_END_WORK").unwrap_or_default().trim().to_ascii_lowercase().as_str() {
            "newest" => Cut::Newest,
            "after" => Cut::After,
            _ => Cut::Ends,
        }
    }

    /// Its word on the cost line (`cut=`).
    fn word(self) -> &'static str {
        match self {
            Cut::Newest => "newest",
            Cut::After => "after",
            Cut::Ends => "ends",
        }
    }

    /// `n` of `text`'s characters (`n` < its length), `at` the batch's earliest delivery in it (none:
    /// the newest), each cut marked `…`. `after`: from `at` on, its start first, or the newest `n` when
    /// what follows `at` is shorter; `ends`: half from `at`, half the end, or `after`'s when the two meet.
    fn window(self, text: &[char], n: usize, at: Option<usize>) -> String {
        let all = text.len();
        let s = |r: std::ops::Range<usize>| text[r].iter().collect::<String>();
        let mark = |cut: bool| if cut { "…" } else { "" };
        let at = at.map(|a| a.min(all));
        match (self, at) {
            (Cut::After, Some(a)) => {
                let start = a.min(all - n);
                format!("{}{}{}", mark(start > 0), s(start..start + n), mark(start + n < all))
            }
            (Cut::Ends, Some(a)) if a + n / 2 < all - (n - n / 2) => format!("{}{}…{}", mark(a > 0), s(a..a + n / 2), s(all - (n - n / 2)..all)),
            (Cut::Ends, Some(_)) => Cut::After.window(text, n, at),
            _ => format!("…{}", s(all - n..all)),
        }
    }
}

/// How long one session-end judge call (`judge_session`) may take: a learn-side call's limit
/// (`BORG_LEARN_TIMEOUT_S`), at most twice the check's (`BORG_PREDICT_TIMEOUT_S`). Derived: the session
/// lock it holds reads stale after twice the check's limit plus a slack (live.rs `stale_ms`); the
/// pass renews the lock before each call (live.rs `Guard::renew`), so a stop of a session resumed
/// meanwhile never takes the lock over and judges the same deliveries again.
fn end_timeout() -> std::time::Duration {
    learn::learn_timeout().min(predict::timeout().saturating_mul(2))
}

/// After a session-end judge call, in the state file: `#borg-end-judged <ids>`, the lessons it was
/// shown. Their latest deliveries are settled by it whatever it said (`pending`), so neither a later
/// check nor a second session-end pass judges them again; one it left pending settles at `borg learn`
/// as having used its tries (`unjudged`). A later delivery of the lesson opens it again.
const END_JUDGED: &str = "#borg-end-judged ";

/// When a judge call runs (`pending`), which decides what it judges: a turn end (a stop's check) or the
/// session's end (`judge_session`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum When {
    Stop,
    End,
}

/// The markers of a delivery of lessons to the drone, in the state and .edits files: asked at a Stop
/// (`#borg-asked`; live, before 0.7.6, with a `#borg-live` line after it), delivered early, handed over
/// free (`#borg-queued`); and a placebo (`#borg-placebo`), never shown but judged as a free delivery is.
/// Each ends in ` @<check>`, the check that routed its lessons (select.rs `check_id`), once checks had ids.
const DELIVERED: [&str; 4] = ["#borg-asked ", "#borg-early ", "#borg-queued ", "#borg-placebo "];

/// Right after a stop's `#borg-asked` line: `#borg-routed <id>=<check>,…`, the lessons of the block
/// another check routed (a live check's ask still waiting at the stop, before 0.7.6), where the block's
/// own tag is the stop's check. Read, no longer written.
const ROUTED: &str = "#borg-routed ";

/// Whether `line` is part of a delivery's markers (`DELIVERED`, `#borg-live`, `ROUTED`): one delivery
/// writes its work line once, then a marker per routing check.
fn is_marker(line: &str) -> bool {
    DELIVERED.iter().any(|m| line.starts_with(m)) || line.starts_with("#borg-live ") || line.starts_with(ROUTED)
}

/// Whether the judge sees question `lesson` of `check` without the drone's `<id>: applies|skip`: a
/// fair coin from blake3 over both, so a retry strips alike and a replay knows it (outcome.rs
/// `stripped`; a test predicts a judge prompt by it). Method: a random half, to measure what the
/// declaration adds to a verdict.
#[must_use]
pub fn stripped(check: &str, lesson: &str) -> bool {
    let mut h = blake3::Hasher::new();
    h.update(b"borg-strip-v1\0");
    h.update(format!("{check}\0{lesson}").as_bytes());
    h.finalize().as_bytes()[0] & 1 == 1
}

/// Of the lessons delivered in a session's state file `state`, since each one's latest delivery: how many
/// verdicts it had (its tries) and whether one settled it (a label other than pending, or a session-end
/// judge's `END_JUDGED`). A delivery opens its lesson again.
fn verdicts_in(state: &str) -> (std::collections::HashMap<&str, usize>, std::collections::HashSet<&str>) {
    let mut tries: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut settled = std::collections::HashSet::new();
    for l in state.lines() {
        if let Some(l) = l.strip_prefix("#borg-judged ") {
            let (id, label) = l.split_once('=').unwrap_or((l, ""));
            *tries.entry(id).or_default() += 1;
            if label.trim() != "pending" {
                settled.insert(id);
            }
        } else if let Some(list) = l.strip_prefix(END_JUDGED) {
            settled.extend(ids_in(list));
        } else if let Some(list) = DELIVERED.iter().find_map(|m| l.strip_prefix(m)) {
            for id in ids_in(list) {
                tries.remove(id);
                settled.remove(id);
            }
        }
    }
    (tries, settled)
}

/// The lessons of `state` whose latest delivery no verdict settled yet: what a session-end judge
/// (`judge_session`) judges, all of them.
fn open_in(state: &str) -> std::collections::HashSet<&str> {
    let (_, settled) = verdicts_in(state);
    let mut open: std::collections::HashSet<&str> = DELIVERED.iter().flat_map(|m| listed(state, m)).collect();
    open.retain(|id| !settled.contains(id));
    open
}

/// What one judge call takes (`pending`): the pending lessons with their evidence, whether a question
/// is among them, and the ids the call has no slot for (`MAX_PENDING`; none at the session's end).
struct Judging<'a> {
    pending: Vec<predict::Pending<'a>>,
    question: bool,
    crowded: Vec<String>,
}

/// R1: lessons delivered earlier in this session (asked at a Stop, or live before 0.7.6, delivered early,
/// handed over free, or a placebo) that have no verdict yet, with the evidence for the consequence model
/// (predict.rs `Pending`), the same for every route (judge v2): the work before the delivery (at a Stop the
/// blocked message, before a free delivery or a placebo the work so far), and after it "work since" (a
/// question's answer, then the drone's edits and later messages) and this turn's message (`final_msg`),
/// each capped by `predict::JUDGE_PART`. A random half of the questions (`stripped`) lose the drone's
/// declaration from all of the call's evidence: their own, every sibling's, and this turn's message, which
/// may be the answer itself (`check` takes it out of the work the call shows too). `#borg-judged
/// <id>=<label>` lines in the state file record verdicts, each for the lesson's latest delivery before it:
/// a lesson delivered early, judged, then asked at a Stop is judged again on the question. A delivery still
/// open after a stop's call (or at a stop that makes none) waits for a later stop, and at the latest for
/// the session-end judge (`judge_session`). Of the `MAX_PENDING` slots, questions take theirs first (the
/// newest), free deliveries what is left: a free hand-over never pushes a question out of its judge. At the
/// session's end (`When::End`) nothing is held, a lesson's tries do not bar it (`MAX_JUDGE_TRIES`) and
/// there are no slots: every open delivery is judged, in batches (`batches`), in delivery order (the order
/// of the result). A lesson a session-end judge was shown (`END_JUDGED`) is settled for every later call.
/// `judging`: this call's check, the coin's seed for a delivery from before checks had ids.
fn pending<'a>(session: &str, state: &str, all: &'a [conscience::Challenge], final_msg: &str, when: When, judging: &str) -> Judging<'a> {
    let (tries, settled) = verdicts_in(state);
    let edits = std::fs::read_to_string(edits_file(session)).unwrap_or_default();
    let lines: Vec<&str> = edits.lines().collect();
    let part = predict::JUDGE_PART;
    /// One delivery's evidence, unclipped, and its fresh lessons: (id, routing check, stripped).
    struct Delivery<'l> {
        channel: &'static str,
        /// A question (`#borg-asked`), not a free delivery or a placebo.
        question: bool,
        before: &'l str,
        answers: Vec<&'l str>,
        work: Vec<&'l str>,
        fresh: Vec<(&'l str, &'l str, bool)>,
    }
    let mut deliveries: Vec<Delivery> = Vec::new();
    for (p, l) in lines.iter().enumerate() {
        let Some((marker, list)) = DELIVERED.iter().find_map(|m| l.strip_prefix(m).map(|rest| (*m, rest))) else { continue };
        let question = marker == "#borg-asked ";
        let later = &lines[p + 1..];
        let channel = match marker {
            "#borg-asked " if later.first().is_some_and(|n| n.starts_with("#borg-live ")) => "live",
            "#borg-asked " => "stop",
            "#borg-early " => "early",
            "#borg-queued " => "queued",
            _ => outcome::PLACEBO,
        };
        let answers: Vec<&str> = if question { later.iter().filter_map(|l| l.strip_prefix("#borg-answer ")).collect() } else { Vec::new() };
        let work: Vec<&str> = later.iter().copied().filter(|l| !l.starts_with("#borg-")).collect();
        let (list, tag) = tagged(list);
        // The work before: the line before this delivery's markers, a delivery's own work line (`WORK`)
        // read as its text; nothing when that is another bookkeeping line.
        let before = match lines[..p].iter().rev().find(|b| !is_marker(b)) {
            Some(b) if b.starts_with(WORK) => &b[WORK.len()..],
            Some(b) if !b.starts_with("#borg-") => b,
            _ => "",
        };
        let routed: std::collections::HashMap<&str, &str> =
            later.first().and_then(|n| n.strip_prefix(ROUTED)).into_iter().flat_map(|r| r.split(',')).filter_map(|it| it.split_once('=')).collect();
        let fresh: Vec<&str> = list
            .split(',')
            .map(str::trim)
            .filter(|i| !i.is_empty() && !settled.contains(i) && (when == When::End || tries.get(i).copied().unwrap_or(0) < MAX_JUDGE_TRIES))
            .filter(|i| all.iter().any(|c| c.id == *i))
            .collect();
        let check_of = |id: &str| routed.get(id).copied().or(tag).unwrap_or_default();
        let fresh =
            fresh.into_iter().map(|id| (id, check_of(id), question && stripped(if check_of(id).is_empty() { judging } else { check_of(id) }, id))).collect();
        deliveries.push(Delivery { channel, question, before, answers, work, fresh });
    }
    // Every stripped question's declaration leaves all of this call's evidence before any cut could halve
    // one: its own, a sibling's (a later blocked message may hold the answer) and this turn's message.
    let strip: Vec<&str> = deliveries.iter().flat_map(|d| &d.fresh).filter(|f| f.2).map(|f| f.0).collect();
    let blind = |text: &str| strip.iter().fold(text.to_string(), |t, id| crate::memory::strip_declaration(&t, id));
    let turn = predict::clip(&one_line(&blind(final_msg), part + 1), part);
    // Each with whether it was a question.
    let mut out: Vec<(predict::Pending, bool)> = Vec::new();
    for d in deliveries {
        // One "after" for the whole delivery, so lessons delivered together share it.
        let answer = blind(&d.answers.join(" "));
        let said = answer.to_lowercase();
        let since: Vec<&str> = std::iter::once(answer.as_str()).filter(|a| !a.is_empty()).chain(d.work.iter().copied()).collect();
        let after = format!("work since: {}\nthis turn: {turn}", predict::clip(&blind(&since.join(" | ")), 2 * part));
        let before = blind(d.before);
        for (id, check, stripped) in d.fresh {
            let Some(c) = all.iter().find(|c| c.id == id) else { continue };
            // A later delivery of the same lesson is the one judged: its evidence replaces the earlier one's.
            out.retain(|x| x.0.lesson.id != id);
            let declared = if stripped { None } else { crate::memory::declared(&said, &id.to_lowercase()) };
            let pending =
                predict::Pending { lesson: c, before: before.clone(), after: after.clone(), declared, check: check.to_string(), channel: d.channel, stripped };
            out.push((pending, d.question));
        }
    }
    let slots = if when == When::End { usize::MAX } else { MAX_PENDING };
    let questions = out.iter().filter(|x| x.1).count().min(slots);
    let mut room = [slots - questions, questions];
    let (mut kept, mut crowded): (Vec<(predict::Pending, bool)>, Vec<String>) = (Vec::new(), Vec::new());
    for x in out.into_iter().rev() {
        let r = &mut room[usize::from(x.1)];
        if *r > 0 {
            *r -= 1;
            kept.push(x);
        } else {
            crowded.push(x.0.lesson.id.clone());
        }
    }
    kept.reverse();
    crowded.sort();
    Judging { pending: kept.into_iter().map(|x| x.0).collect(), question: questions > 0, crowded }
}

/// `text` without the declarations of the stripped questions among `pending` (`stripped`): the work a
/// judge call shows beside them, which may hold the answer to one (this turn's message at the stop).
fn without_declarations<'t>(text: &'t str, pending: &[predict::Pending]) -> std::borrow::Cow<'t, str> {
    pending.iter().filter(|p| p.stripped).fold(std::borrow::Cow::Borrowed(text), |t, p| crate::memory::strip_declaration(&t, &p.lesson.id).into())
}

/// `#borg-unjudged <ids> why=<reason>`: pending lessons a judge call did not judge, and why
/// (outcome.rs `UNJUDGED_SLOTS`, `UNJUDGED_FAILED`), so `borg learn` can say why each settled without
/// a verdict (`unjudged`). Nothing for no ids.
fn note_unjudged(session: &str, ids: &[String], why: &str) {
    if !ids.is_empty() {
        append(session, &format!("#borg-unjudged {} why={why}", ids.join(",")));
    }
}

/// Why each lesson delivered in a session's state file `state` has no verdict, as `borg learn` settles
/// it (outcome.rs `settle_with`): its tries ran out on unclear or missing verdicts since its latest
/// delivery, the session-end judge's included (`END_JUDGED`; `tries`), else the last judge call that
/// left it out (`slots`, `failed`), else no call came after its delivery (`no_call`).
pub(crate) fn unjudged(state: &str) -> std::collections::HashMap<String, &'static str> {
    let mut why: std::collections::HashMap<String, &'static str> = std::collections::HashMap::new();
    let mut tries: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for l in state.lines() {
        if let Some(list) = DELIVERED.iter().find_map(|m| l.strip_prefix(m)) {
            for id in ids_in(list) {
                tries.remove(id);
                why.insert(id.to_string(), outcome::UNJUDGED_NO_CALL);
            }
        } else if let Some(l) = l.strip_prefix("#borg-judged ") {
            let (id, label) = l.split_once('=').unwrap_or((l, ""));
            if label.trim() == "pending" {
                let n = tries.entry(id).or_default();
                *n += 1;
                if *n >= MAX_JUDGE_TRIES {
                    why.insert(id.to_string(), outcome::UNJUDGED_TRIES);
                }
            }
        } else if let Some(list) = l.strip_prefix(END_JUDGED) {
            // The session-end judge was its last try, whatever its count.
            for id in ids_in(list) {
                tries.insert(id, MAX_JUDGE_TRIES);
                why.insert(id.to_string(), outcome::UNJUDGED_TRIES);
            }
        } else if let Some(rest) = l.strip_prefix("#borg-unjudged ") {
            let (list, reason) = rest.rsplit_once(" why=").unwrap_or((rest, ""));
            let reason = match reason.trim() {
                "slots" => outcome::UNJUDGED_SLOTS,
                "failed" => outcome::UNJUDGED_FAILED,
                _ => continue,
            };
            for id in list.split(',').map(str::trim).filter(|i| !i.is_empty()) {
                if tries.get(id).copied().unwrap_or(0) < MAX_JUDGE_TRIES {
                    why.insert(id.to_string(), reason);
                }
            }
        }
    }
    why
}

/// R1: the consequence model's verdicts on the pending lessons it was shown (predict.rs `Judged`), each
/// recorded under its own kind (predict.rs `Verdict::kind`): `asked_then_changed` for a change that
/// avoids the consequence, `asked_ignored` for no change on a point that came up, `asked_not_arisen`
/// for a point that never came up (censored for beliefs); these three settle the delivery
/// (`#borg-judged <id>=<verdict>`), a `not_arisen` only at the session's end (`at` outcome.rs
/// `AT_END`): at a check the point may still come up later, so it stays pending for later stops and
/// the session-end judge (`judge_session`), a try as an unclear one is. An unclear verdict (`asked_unclear`) and a change that still leads
/// to the consequence (`asked_consequence_remains`) are measurements only and stay pending
/// (`#borg-judged <id>=pending`), as a missing verdict does. Each result names its delivery's channel
/// (outcome.rs `PLACEBO` for a placebo, never a result about the lesson), the check that routed it, the
/// judge's version (predict.rs `JUDGE_VERSION`), whether the declaration was stripped, per lesson the
/// judge's redacted reason (`reasons`), the session's `repo`, and where it was judged (`at`: empty at a
/// check, outcome.rs `AT_END` at the session's end). Logs the call's judging cost in the .cost file
/// (`judge_line`, then ` model=<judge>`, then ` at=<at>` when not at a check, then `logged`'s fields).
fn record_verdicts(session: &str, repo: &str, at: &str, pending: &[predict::Pending], pred: &predict::Prediction, logged: &Logged) {
    // One record per (kind, channel, check, stripped): the statistics split what the deliveries led to by
    // how they were made. Each lesson with its reason.
    type Key<'k> = (&'k str, &'k str, &'k str, bool);
    let mut records: std::collections::BTreeMap<Key, Vec<(String, String)>> = std::collections::BTreeMap::new();
    for p in pending {
        let judged = pred.resolved.iter().find(|j| j.id == p.lesson.id);
        // A point that has not come up by a check may still come up later in the session: only the
        // session-end judge's `not_arisen`, with the whole session in view, settles the delivery.
        let word = match judged {
            Some(j) if j.label().is_some() && (j.verdict != predict::Verdict::NotArisen || at == outcome::AT_END) => j.verdict.word(),
            _ => "pending",
        };
        append(session, &format!("#borg-judged {}={word}", p.lesson.id));
        let Some(j) = judged else { continue };
        records.entry((j.verdict.kind(), p.channel, p.check.as_str(), p.stripped)).or_default().push((p.lesson.id.clone(), j.reason.clone()));
    }
    let at_field = if at.is_empty() { String::new() } else { format!(" at={at}") };
    let line = judge_line(logged.call, pending, &pred.resolved, logged.chars, &pred.returned);
    log_cost(session, &format!("{line} model={}{at_field}{}", logged.model, logged.fields));
    if learning() {
        let (home, ms) = (learn::home(), learn::now_ms());
        // The statistics count one event per kind and channel, whatever checks routed its lessons.
        let mut counted: std::collections::BTreeMap<(&str, &str), Vec<String>> = std::collections::BTreeMap::new();
        for ((kind, channel, check, stripped), judged) in records {
            let (lessons, reasons): (Vec<String>, Vec<String>) = judged.into_iter().unzip();
            counted.entry((kind, channel)).or_default().extend(lessons.iter().cloned());
            let o = outcome::Outcome {
                kind: kind.into(),
                session: session.into(),
                lessons,
                ms,
                repo: repo.into(),
                channel: channel.into(),
                check: check.into(),
                judge: predict::JUDGE_VERSION,
                stripped,
                at: at.into(),
                // None at all: left out, as an older record.
                reasons: if reasons.iter().all(String::is_empty) { Vec::new() } else { reasons },
                ..Default::default()
            };
            let _ = learn::append_jsonl(&home.join("outcomes.jsonl"), &o);
        }
        for ((kind, channel), lessons) in counted {
            outcome::count_result(
                &home,
                &outcome::Outcome { kind: kind.into(), session: session.into(), lessons, ms, repo: repo.into(), channel: channel.into(), ..Default::default() },
            );
        }
    }
}

/// What a judge call's cost line says besides its verdicts (`record_verdicts`): the call's number
/// among the session's model calls, its prompt's characters (`judge_line`), the judge's model
/// (`judge_model`), and fields of its own after ` at=` (a check's whole prompt's characters, `prompt=`; a
/// session-end batch's `batch=`, `budget=`, `psi=` and `explore=` when it drew, `cut=`).
struct Logged {
    call: usize,
    chars: usize,
    model: String,
    fields: String,
}

/// A judge call's cost line (`record_verdicts`): `judge call=<nth model call of the session> pending=N
/// resolved=K chars=<prompt chars added>`, K the verdicts that settle (changed, ignored, not_arisen);
/// then the ids the answer named (`returned=a,b`, predict.rs `Prediction::returned`) and those of them
/// no pending lesson has (`unmatched=`), each empty for none; then the verdicts on pending lessons by
/// word, every word in `Verdict::WORDS` order (`verdicts=changed:1,ignored:0,…`), and the characters
/// of their reasons (`reason_chars=`, what the answer added). A pending lesson in neither list was left
/// out by the model; one returned but not resolved got a malformed verdict. Zero tokens, no prompt text.
fn judge_line(call: usize, pending: &[predict::Pending], resolved: &[predict::Judged], chars: usize, returned: &[String]) -> String {
    let unmatched: Vec<&str> = returned.iter().map(String::as_str).filter(|id| !pending.iter().any(|p| p.lesson.id == *id)).collect();
    let mine: Vec<&predict::Judged> = resolved.iter().filter(|j| pending.iter().any(|p| p.lesson.id == j.id)).collect();
    let settled = mine.iter().filter(|j| j.label().is_some()).count();
    let verdicts: Vec<String> = predict::Verdict::ALL.iter().map(|v| format!("{}:{}", v.word(), mine.iter().filter(|j| j.verdict == *v).count())).collect();
    let reason_chars: usize = mine.iter().map(|j| j.reason.chars().count()).sum();
    format!(
        "judge call={call} pending={} resolved={settled} chars={chars} returned={} unmatched={} verdicts={} reason_chars={reason_chars}",
        pending.len(),
        returned.join(","),
        unmatched.join(","),
        verdicts.join(",")
    )
}

/// The session-end judge (0.7.6), run by `borg learn --session` before it learns (main.rs `learn_cmd`):
/// every delivery of `session` (any channel) whose latest delivery no verdict settled yet (none came, or
/// only unclear, consequence_remains or missing ones, its tries spent or not) is judged, beside as much of
/// the session's work as the prompt's budget leaves (`end_budget`, `session_sources`, `within`). The
/// lessons go in batches by delivery order, each prompt within the budget (`batches`), each batch its
/// own model call, `runner::JOBS` at a time, each within `end_timeout`, under the session lock,
/// renewed before each call (live.rs `Guard::renew`). Each batch records as soon as its call returns,
/// while the others still run, so a pass stopped partway keeps what it was already told (and paid for).
/// An answered batch records as a check's judge does (`record_verdicts`, `at` outcome.rs `AT_END`, its
/// cost line naming `batch=<i>/<n>`, `budget=` and `cut=`) and writes the state lines `#borg-judged`,
/// then its own `END_JUDGED`: what it judged settles there, and `borg learn` then settles the rest as
/// `asked_not_corrected` with its reason (`unjudged`). Each call is counted as a `model_call` with why
/// `judge`. A failed batch is no try, for its own lessons alone (`#borg-unjudged … why=failed`), and its
/// cost line `judge-failed` keeps its lessons as sent and unanswered for the judge's effective length
/// (judge_length.rs); after `runner::MAX_FAILURES` failed calls before the first answer the judge is
/// unavailable, and the batches not yet asked fail alike, without a call. A pass whose lock was taken over
/// asks nothing more; what it did not ask stays open for the lock's new holder, and `borg learn` settles
/// no session whose lock another holds (learn.rs). A lock held by another (a stop, or an earlier pass
/// of a resumed session) is waited for, as long as it is renewed: it reads stale `live::stale_ms`
/// after its holder stops. Nothing runs, and nothing is written, without learning (`BORG_LEARN=off`),
/// without a runner for the consequence model (`BORG_PREDICT_MODEL=off`, none installed), or with
/// nothing open. Returns how many lessons it judged.
pub fn judge_session(session: &str) -> usize {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    if !learning() {
        return 0;
    }
    let ctx = state_file(session);
    let opened = |state: &str| state.lines().any(|l| DELIVERED.iter().any(|m| l.starts_with(m)));
    if !opened(&std::fs::read_to_string(&ctx).unwrap_or_default()) {
        return 0;
    }
    let model = std::env::var("BORG_PREDICT_MODEL").unwrap_or_default();
    if crate::runner::choose(Some(&model)).is_empty() {
        return 0;
    }
    // A stop, or an earlier pass of this session (a resumed session's end), judges the same lessons:
    // wait for it while it holds the lock. A pass renews it before each call, so it may hold it longer
    // than one wait; a holder that stops renewing lets it go stale within one.
    let wait = u64::try_from(live::stale_ms()).unwrap_or(u64::MAX);
    while live::wait_free(&ctx, wait) == live::Lock::Held {}
    let Some(guard) = live::acquire(&ctx, learn::now_ms()) else { return 0 };
    let guard = std::sync::Mutex::new(guard);
    let state = std::fs::read_to_string(&ctx).unwrap_or_default();
    let (all, withheld) = loaded(session);
    let judged = judged(&all, withheld);
    let last = crate::notes::proposal(&crate::notes::state_dir(), session);
    let check = crate::select::check_id(session, "end", learn::now_ms(), std::process::id());
    let judging = pending(session, &state, &judged, &last, When::End, &check);
    let pend = &judging.pending;
    if pend.is_empty() {
        return 0;
    }
    let label = judge_model(&model);
    // The store's judge cost lines are beside the session's: its .cost file is in the same directory.
    let budget = end_budget(ctx.parent().unwrap_or(std::path::Path::new(".")), &label, &check);
    let cut = Cut::from_env();
    let sources = session_sources(session, &state, pend, budget.chars);
    let groups = batches(pend, budget.chars);
    let (next, failures, answers, lost) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicBool::new(false));
    let repo = where_repo(session);
    let n = groups.len();
    let draw = budget.draw.map_or(String::new(), |(psi, explored)| format!(" psi={psi:.4} explore={}", u8::from(explored)));
    // The judge's call numbering counts every answered model call of the session: the checks', then these
    // in the order they answer (`batch=` names each one's place).
    let mut call = state.lines().filter(|l| l.starts_with("#borg-predict-cost")).count();
    let mut done = 0;
    let mut asked = vec![false; n];
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Batch)>();
    std::thread::scope(|s| {
        for _ in 0..crate::runner::JOBS.min(n) {
            let tx = tx.clone();
            let (groups, next, failures, answers, lost, guard, sources, model) = (&groups, &next, &failures, &answers, &lost, &guard, &sources, &model);
            s.spawn(move || {
                while !lost.load(Ordering::SeqCst) && (answers.load(Ordering::SeqCst) > 0 || failures.load(Ordering::SeqCst) < crate::runner::MAX_FAILURES) {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(range) = groups.get(i) else { break };
                    // Taken over: another holds the session now. Not written (I/O): still ours, ask on.
                    if guard.lock().map_or(true, |mut g| g.renew(learn::now_ms()) == live::Renewal::Lost) {
                        lost.store(true, Ordering::SeqCst);
                        break;
                    }
                    let b = ask_batch(&pend[range.clone()], sources, budget.chars, cut, model);
                    (if b.pred.is_some() { answers } else { failures }).fetch_add(1, Ordering::SeqCst);
                    if tx.send((i, b)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        // One writer: each batch records here as its call returns, while the others still run.
        for (i, b) in rx {
            asked[i] = true;
            let batch = &pend[groups[i].clone()];
            let ids: Vec<String> = batch.iter().map(|p| p.lesson.id.clone()).collect();
            crate::stats::count(&learn::home(), crate::stats::MODEL_CALL, session, |ev| {
                ev.why = "judge".into();
                ev.repo = repo.clone();
                ev.n = batch.len() as u64;
                ev.usd = b.pred.as_ref().map_or(0.0, |p| p.cost);
                ev.dur_ms = b.ms;
                if b.pred.is_none() {
                    ev.text = "no answer".into();
                }
            });
            let fields = format!(" batch={}/{n} budget={}{draw} cut={}", i + 1, budget.chars, if b.cut { cut.word() } else { "none" });
            let Some(pred) = b.pred else {
                // Sent and not answered: the judge's effective length counts it (judge_length.rs).
                log_cost(session, &format!("judge-failed pending={} chars={} model={label} at={}{fields}", batch.len(), b.chars, outcome::AT_END));
                note_unjudged(session, &ids, "failed");
                continue;
            };
            call += 1;
            record_verdicts(session, &repo, outcome::AT_END, batch, &pred, &Logged { call, chars: b.chars, model: label.clone(), fields });
            append(session, &format!("{END_JUDGED}{}", ids.join(",")));
            done += ids.len();
        }
    });
    // Not asked: the judge was unavailable (failed, no try), or the lock was lost (open for its holder).
    if !lost.load(Ordering::SeqCst) {
        let unasked: Vec<String> =
            groups.iter().zip(&asked).filter(|(_, a)| !**a).flat_map(|(r, _)| pend[r.clone()].iter().map(|p| p.lesson.id.clone())).collect();
        note_unjudged(session, &unasked, "failed");
    }
    done
}

/// The session-end judge's batches of `pending` (`judge_session`, in delivery order), as ranges of it:
/// each takes the next lessons while their judge section (`predict::judge_section`) stays within
/// 1/`END_LESSONS_SHARE` of `budget`, and at least one. Consecutive, so each batch covers its own span of
/// the session, from its earliest delivery on. A lesson whose evidence alone exceeds that share goes
/// alone (its evidence is bounded by `predict::JUDGE_PART`).
fn batches(pending: &[predict::Pending], budget: usize) -> Vec<std::ops::Range<usize>> {
    let room = budget / END_LESSONS_SHARE;
    let mut out = Vec::new();
    let mut start = 0;
    for end in 2..=pending.len() {
        if end - start > 1 && predict::judge_section(&pending[start..end]).chars().count() > room {
            out.push(start..end - 1);
            start = end - 1;
        }
    }
    if start < pending.len() {
        out.push(start..pending.len());
    }
    out
}

/// One session-end batch's call (`ask_batch`): its prompt's characters (`predict::judge_prompt`), whether
/// the session's work was cut to fit, the judge's answer (None: failed) and how long it took.
struct Batch {
    chars: usize,
    cut: bool,
    pred: Option<predict::Prediction>,
    ms: u64,
}

/// Asks the session-end judge about `batch` (`judge_session`): beside its lessons, the session's work
/// in what `budget` leaves (`within`), cut by `cut` at the batch's earliest delivery, within
/// `end_timeout`.
fn ask_batch(batch: &[predict::Pending], sources: &[Source], budget: usize, cut: Cut, model: &str) -> Batch {
    let fixed = predict::judge_prompt("", batch).chars().count();
    let ids: Vec<&str> = batch.iter().map(|p| p.lesson.id.as_str()).collect();
    let anchors: Vec<Option<usize>> = sources.iter().map(|s| s.anchor(&ids)).collect();
    let (work, was_cut) = within(sources, &anchors, budget.saturating_sub(fixed), cut);
    let chars = fixed + work.chars().count();
    let started = std::time::Instant::now();
    let pred = predict::judge(&work, batch, model, end_timeout());
    Batch { chars, cut: was_cut, pred, ms: started.elapsed().as_millis() as u64 }
}

/// One source of the session's work as the session-end judge reads it (`session_sources`): its label,
/// its text, and the delivery markers (`DELIVERED`) that stood in it: where (a character offset of the
/// text) and the lessons each delivered.
struct Source {
    label: &'static str,
    text: Vec<char>,
    marks: Vec<(usize, Vec<String>)>,
}

impl Source {
    /// The lines of `text` that `keep` gives, each delivery marker a mark between them; each run of lines
    /// between two marks without the declarations of the stripped questions among `pending`
    /// (`without_declarations`). Empty when only whitespace is left.
    fn of<'t>(label: &'static str, text: &'t str, keep: impl Fn(&'t str) -> Option<&'t str>, pending: &[predict::Pending]) -> Source {
        let mut out = Source { label, text: Vec::new(), marks: Vec::new() };
        let mut run: Vec<&str> = Vec::new();
        for l in text.lines() {
            if let Some(list) = DELIVERED.iter().find_map(|m| l.strip_prefix(m)) {
                out.push(&run, pending);
                run.clear();
                // Where the next run starts: after the newline that will set it apart.
                out.marks.push((out.text.len() + usize::from(!out.text.is_empty()), ids_in(list).map(str::to_string).collect()));
            } else if let Some(kept) = keep(l) {
                run.push(kept);
            }
        }
        out.push(&run, pending);
        if out.text.iter().all(|c| c.is_whitespace()) {
            out.text.clear();
        }
        out
    }

    /// Appends a run of `lines`, on lines of their own, without the stripped declarations.
    fn push(&mut self, lines: &[&str], pending: &[predict::Pending]) {
        if lines.is_empty() {
            return;
        }
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        self.text.extend(without_declarations(&lines.join("\n"), pending).chars());
    }

    /// Where the earliest of `ids`' latest deliveries stood in the text (the delivery each is judged on,
    /// `pending`); None when no marker names one.
    fn anchor(&self, ids: &[&str]) -> Option<usize> {
        ids.iter().filter_map(|id| self.marks.iter().rev().find(|(_, l)| l.iter().any(|x| x == id)).map(|m| m.0)).min()
    }
}

/// The session's whole work as the session-end judge reads it (`judge_session`), by source: the
/// engineer's prompts and the agent's touches (the state file without its bookkeeping), the agent's
/// edits and its answers to questions (the .edits file), and its messages (its harness's transcript,
/// harness.rs `session_texts`, where it writes one: its newest `cap` characters, more than any batch
/// reads). Each without the declarations of the stripped questions among `pending`
/// (`without_declarations`); the first two with where each delivery stood in them (`Source`).
fn session_sources(session: &str, state: &str, pending: &[predict::Pending], cap: usize) -> Vec<Source> {
    let edits = std::fs::read_to_string(edits_file(session)).unwrap_or_default();
    let said = harness::session_texts(harness::current(), session, cap);
    vec![
        Source::of("The engineer's prompts and the agent's touches", state, |l| (!l.starts_with("#borg-")).then_some(l), pending),
        Source::of("The agent's edits and answers", &edits, |l| l.strip_prefix("#borg-answer ").or_else(|| (!l.starts_with("#borg-")).then_some(l)), pending),
        Source { label: "The agent's messages", text: without_declarations(&said, pending).chars().collect(), marks: Vec::new() },
    ]
}

/// `sources` in `budget` characters in all, framing included, each as `### <label>` and its text: all
/// whole when they fit; else each an equal share of what is left, the shortest first, so a part shorter
/// than its share leaves the rest to the others, and a longer one is cut by `cut` (`Cut::window`) at its
/// `anchors` entry (the batch's earliest delivery in it). An empty part is left out. With whether a part
/// was cut.
fn within(sources: &[Source], anchors: &[Option<usize>], budget: usize, cut: Cut) -> (String, bool) {
    let len: Vec<usize> = sources.iter().map(|s| s.text.len()).collect();
    let mut order: Vec<usize> = (0..sources.len()).filter(|i| len[*i] > 0).collect();
    // `### <label>\n` and the closing newline of each part; a cut part's marks (`…`), two at most.
    let frame: usize = order.iter().map(|&i| sources[i].label.chars().count() + 6).sum();
    let total: usize = len.iter().sum();
    let mut left = if frame + total <= budget { total } else { budget.saturating_sub(frame + 2 * order.len()) };
    order.sort_by_key(|i| len[*i]);
    let mut keep = vec![0; sources.len()];
    for (k, &i) in order.iter().enumerate() {
        keep[i] = len[i].min(left / (order.len() - k));
        left -= keep[i];
    }
    let (mut out, mut was_cut) = (String::new(), false);
    for (i, s) in sources.iter().enumerate() {
        let n = keep[i];
        if n == 0 {
            continue;
        }
        let text = if n < len[i] {
            was_cut = true;
            cut.window(&s.text, n, anchors.get(i).copied().flatten())
        } else {
            s.text.iter().collect()
        };
        out.push_str(&format!("### {}\n{text}\n", s.label));
    }
    (out, was_cut)
}

/// The repo the session works in: the latest repo the hook noted at a prompt (`.where`, a repo name on
/// a line of its own, systems.rs `work_repos`); empty when none.
fn where_repo(session: &str) -> String {
    let text = std::fs::read_to_string(where_file(session)).unwrap_or_default();
    text.lines().map(str::trim).rfind(|l| !l.is_empty() && !l.contains(['/', ' '])).unwrap_or_default().to_string()
}

/// Ids listed on the state file's `marker` lines (`#borg-asked a,b @<check>`).
pub(crate) fn listed<'a>(context: &'a str, marker: &str) -> std::collections::HashSet<&'a str> {
    context.lines().filter_map(|l| l.strip_prefix(marker)).flat_map(ids_in).collect()
}

/// The ids of a marker's or a queue line's list, without its ` @<check>` tag (`tagged`).
fn ids_in(list: &str) -> impl Iterator<Item = &str> {
    tagged(list).0.split(',').map(str::trim).filter(|i| !i.is_empty())
}

/// ` @<check>` after a list, or nothing for a check without an id (a line of an older binary's state).
fn tag(check: &str) -> String {
    if check.is_empty() { String::new() } else { format!(" @{check}") }
}

/// Early delivery (conscience.rs `early`): once the session's prompts and touches name a lesson scope
/// (a topic) and that scope's lessons score on them, the top ones go to the drone at once, as context,
/// instead of waiting for the Stop checkpoint: at the prompt or the first touch on the topic (`text` is
/// this event's, already in the state file). Each lesson once per session (`#borg-early`; Stop does not
/// ask it again) and an `asked` outcome with channel early (`record_delivery`). A topic is settled (`#borg-topic`) once a delivery covered it or none of its lessons is
/// left; until then each prompt or touch looks again. At most `MAX_EARLY_DELIVERIES` per session. Only
/// where the harness takes it (`Caps::early_delivery`, Claude Code so far): omp and opencode take a prompt's
/// and a tool call's context too, but early delivery to them waits for an evaluation, so for
/// them the lessons stay with Stop. Standing lessons (ratified constraints, conscience.rs `standing_pool`)
/// are no ranking guess and need no evaluation: they come on every harness that takes the context (a
/// prompt's, or a tool call's for a named scope), whenever their scope is named (an unscoped one at a
/// prompt), settled topic or not and past `MAX_EARLY_DELIVERIES`, until each came once. `BORG_EARLY=off`
/// disables it; so do the eval modes BORG_ONLY and BORG_CRITIC.
/// Early deliveries per session at most (each up to `conscience::render_early`'s lesson cap). Guard: a
/// long session naming many systems would otherwise get a delivery at every touch until each topic's
/// lessons ran out (standing lessons are exempt).
const MAX_EARLY_DELIVERIES: usize = 3;

fn early(h: &dyn Harness, so_far: &SoFar, hook_event: &str, session: &str, input: &Value, text: &str) -> Option<String> {
    if text.trim().is_empty()
        || std::env::var("BORG_EARLY").is_ok_and(|v| v.trim().eq_ignore_ascii_case("off"))
        || std::env::var_os("BORG_ONLY").is_some()
        || std::env::var_os("BORG_CRITIC").is_some()
    {
        return None;
    }
    let challenges = lessons(session);
    let context = std::fs::read_to_string(state_file(session)).unwrap_or_default();
    // Free, but not free of noise: a long session naming many systems would otherwise get a delivery
    // at every touch until each topic's lessons ran out. Standing lessons are exempt.
    let plain = h.caps().early_delivery && context.lines().filter(|l| l.starts_with("#borg-early ")).count() < MAX_EARLY_DELIVERIES;
    let standing = challenges.iter().any(conscience::Challenge::standing);
    if !standing && (!plain || challenges.iter().all(|c| c.scope.is_empty() && !c.consolidated)) {
        return None;
    }
    let settled = listed(&context, "#borg-topic ");
    let named = conscience::named_scopes(&challenges, &where_now(session));
    let mut topics: Vec<String> = if plain { named.iter().filter(|t| !settled.contains(t.as_str())).cloned().collect() } else { Vec::new() };
    // Consolidated lessons (memory.rs) need no scope: the first prompt is their topic (`ANY`).
    if plain
        && topics.is_empty()
        && hook_event == "UserPromptSubmit"
        && !settled.contains(conscience::ANY)
        && challenges.iter().any(|c| c.consolidated && !c.standing())
    {
        topics.push(conscience::ANY.into());
    }
    // A standing lesson's topic is every named scope, settled or not, and any prompt for an unscoped one.
    let mut standing_topics = named;
    if hook_event == "UserPromptSubmit" {
        standing_topics.push(conscience::ANY.into());
    }
    let done = had_in(&context);
    let mut pool = if standing { conscience::standing_pool(&challenges, &standing_topics, &done) } else { Vec::new() };
    if !topics.is_empty() {
        pool.extend(conscience::early_pool(&challenges, &conscience::work_text(&context), &topics, &done));
    }
    // The header names what came: the plain topics, and the scopes of the standing lessons beside them.
    let mut shown_topics = topics.clone();
    for t in standing_topics.into_iter().filter(|t| pool.iter().any(|(_, c)| c.standing() && c.scope.contains(t))) {
        if !shown_topics.contains(&t) {
            shown_topics.push(t);
        }
    }
    // Today's early caps (`EARLY_CAP`, `EARLY_CHARS`, `RELATIVE`), kept until a learned early rule has
    // data: each early delivery's record carries every lesson's score relative to the best (`early_rel`).
    let selected: Vec<&conscience::Challenge> = conscience::explored(pool.iter().copied()).into_iter().take(conscience::EARLY_CAP).collect();
    let shown = conscience::render_early(&selected, &shown_topics);
    let delivered = &selected[..shown.as_ref().map_or(0, |(_, n)| *n)];
    // Plain topics only: a standing lesson left over (past `EARLY_CAP` or `EARLY_CHARS`) comes at the next
    // prompt or touch on its scope whatever is settled.
    let now_settled: Vec<&str> = topics
        .iter()
        .filter(|t| {
            // One delivery per topic: whatever was delivered now covers every topic looked at now.
            !delivered.is_empty()
                || !challenges.iter().any(|c| {
                    !c.standing()
                        && (c.scope.contains(t) || (t.as_str() == conscience::ANY && c.consolidated))
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
    // An early delivery is its own decision: its id ties its markers to its record.
    let check = crate::select::check_id(session, "early", learn::now_ms(), std::process::id());
    let line = format!("#borg-early {ids} @{check}");
    append(session, &line);
    note_work(session, so_far, &line);
    let p = conscience::early_propensity(&pool, session, &shown_topics);
    let p: Vec<f64> = delivered.iter().map(|c| p.get(&c.id).copied().unwrap_or(0.0)).collect();
    record_delivery(session, input, delivered, "early", &check, p, early_rel(&challenges, &conscience::work_text(&context), &topics, delivered));
    log_cost(session, &format!("free event={hook_event} lessons={} chars={}", delivered.len(), text.len()));
    crate::stats::count(&learn::home(), crate::stats::EARLY, session, |e| {
        e.n = delivered.len() as u64;
        e.lessons = delivered.iter().map(|c| c.id.clone()).collect();
        e.channel = "early".into();
    });
    Some(text)
}

/// Each of `delivered`'s word-match score (conscience.rs `rank` over `work`) as a share of the best
/// on-topic match of `topics`, done or not (the match `RELATIVE` bars against: `early_pool` over no done
/// lesson starts with it), rounded as a propensity is: what a learned early rule needs of each early
/// delivery (outcome.rs `rel`). A standing lesson's too, though no score gates it (0 when it matches no
/// word). Empty when no plain lesson is on topic to scale by (a standing lesson's delivery alone).
fn early_rel(challenges: &[conscience::Challenge], work: &str, topics: &[String], delivered: &[&conscience::Challenge]) -> Vec<f64> {
    let Some(&(top, _)) = conscience::early_pool(challenges, work, topics, &std::collections::HashSet::new()).first() else { return Vec::new() };
    let ranked = conscience::rank(challenges, work);
    delivered.iter().map(|c| outcome::round_p(ranked.iter().find(|(_, x)| x.id == c.id).map_or(0.0, |(s, _)| *s) / top)).collect()
}

/// A delivery's markers (`lines`: a free delivery's, a placebo's) in the .edits file, in
/// one write after the work so far (`WORK`): the evidence before it is the same whatever the route
/// (`pending`), and no route's line reads as work after an earlier delivery.
fn note_work(session: &str, so_far: &SoFar, lines: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(edits_file(session)) {
        let _ = writeln!(f, "{WORK}{}\n{lines}", one_line(so_far.get(), EVIDENCE_CHARS));
    }
}

/// The `asked` outcome of a free delivery (`channel` early or queued) of `delivered`, routed by `check`,
/// with each one's propensity `p` and, early, its relative score `rel` (`early_rel`): delivered as
/// context, the lessons wait for a result as a Stop's questions do. The next judge (`pending`) labels
/// them changed or ignored, a merged correction corrected (outcome.rs `contradicted`), and `borg learn`
/// settles the rest. Nothing with learning off.
fn record_delivery(session: &str, input: &Value, delivered: &[&conscience::Challenge], channel: &str, check: &str, p: Vec<f64>, rel: Vec<f64>) {
    if !learning() || delivered.is_empty() {
        return;
    }
    let lessons: Vec<String> = delivered.iter().map(|c| c.id.clone()).collect();
    let (grades, ranks, applied) = graded_of(session, &lessons);
    let asked = outcome::Outcome {
        kind: outcome::ASKED.into(),
        session: session.into(),
        lessons,
        ms: learn::now_ms(),
        repo: repo_of(input.get("cwd").and_then(Value::as_str).unwrap_or_default()),
        groups: conscience::disagreement_ids(delivered),
        channel: channel.into(),
        grades,
        ranks,
        applied,
        p: p.into_iter().map(outcome::round_p).collect(),
        check: check.into(),
        rel,
        ..Default::default()
    };
    let _ = outcome::append(&learn::home().join("outcomes.jsonl"), &asked);
}

/// One `delivery` record (outcome.rs `DELIVERY`): what delivery did with `lessons`, routed by `check`:
/// `route` (outcome.rs `ROUTES`) and why. Nothing with learning off or without lessons.
fn record_route(session: &str, input: &Value, check: &str, lessons: Vec<String>, channel: &str, route: &str, reason: &str) {
    if !learning() || lessons.is_empty() {
        return;
    }
    let o = outcome::Outcome {
        kind: outcome::DELIVERY.into(),
        session: session.into(),
        lessons,
        ms: learn::now_ms(),
        repo: repo_of(input.get("cwd").and_then(Value::as_str).unwrap_or_default()),
        channel: channel.into(),
        check: check.into(),
        route: route.into(),
        reason: reason.into(),
        ..Default::default()
    };
    let _ = outcome::append(&learn::home().join("outcomes.jsonl"), &o);
}

/// `record_route` for each check of `routed` (lesson, check) pairs, in first-seen order.
fn record_routes(session: &str, input: &Value, routed: &[(&conscience::Challenge, &str)], channel: &str, route: &str, reason: &str) {
    for (check, ids) in by_check(routed) {
        record_route(session, input, check, ids.into_iter().map(String::from).collect(), channel, route, reason);
    }
}

/// `routed` (lesson, check) pairs grouped by check, checks in first-seen order, lessons in theirs.
fn by_check<'a>(routed: &[(&'a conscience::Challenge, &'a str)]) -> Vec<(&'a str, Vec<&'a str>)> {
    let mut out: Vec<(&str, Vec<&str>)> = Vec::new();
    for (c, check) in routed {
        match out.iter_mut().find(|(k, _)| k == check) {
            Some((_, ids)) => ids.push(c.id.as_str()),
            None => out.push((check, vec![c.id.as_str()])),
        }
    }
    out
}

/// Graded Stop (default): what a graded check left for later, one line per check, waiting for the session's
/// next prompt or touch (`queued`): `<ids> @<check>`, the 2s and 1s of a check that did not block, to hand
/// over free; `placebo <ids> @<check>`, a check's placebo draws (select.rs `Routed`); and `instead-next
/// <ids> @<check>`, a check's free lessons handed over in place of questions the fixed rule asks
/// (`INSTEAD_NEXT`). A line of an older binary has no tag, and may be a live check's (`LEGACY_ASK`,
/// `LEGACY_INSTEAD`).
fn queued_file(session: &str) -> PathBuf {
    state_file(session).with_extension("queued")
}

/// What a queue line routes its lessons to (`queued_file`), in the order delivery takes them (`queued`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Queued {
    /// A live check's ask from before 0.7.6 (`LEGACY_ASK`): handed over, recorded as the ask it was.
    Ask,
    /// Free, in place of a question the fixed rule asks (`INSTEAD_NEXT`, `LEGACY_INSTEAD`).
    Instead,
    Free,
    Placebo,
}

/// Protocol: the prefix of the free lessons a check hands over in place of questions the fixed rule
/// asks (`queued_file`): the evidence rule's on a check with a 3 (select/policy.rs `EVIDENCE`), which
/// valued them as hand-overs (their chance of arriving included), and any of a stop's. A free delivery
/// carries them whatever its budget (`queued`, conscience.rs `render_owing`), as the questions they
/// replace would have been carried; no stop asks them.
const INSTEAD_NEXT: &str = "instead-next ";

/// Protocol: the prefixes of a live check's asks from before 0.7.6 (live mode is gone). A session that
/// runs across the upgrade still has them waiting: the next prompt or tool call hands them over in
/// place of their questions, all and first, and records each as the ask its check decided (channel
/// live, `#borg-asked` then `#borg-live`, as 0.7.5 did), so the selection fold and the judge join it;
/// no stop blocks for them.
const LEGACY_ASK: [&str; 2] = ["live ", "live-next "];

/// Protocol: the prefix of a live check's question-count test hand-overs in place of questions, from
/// before 0.7.6: free, as an `INSTEAD_NEXT` line is.
const LEGACY_INSTEAD: &str = "instead ";

/// A queue line's route, ids and check (empty on an older binary's line).
fn queue_line(line: &str) -> (Queued, Vec<&str>, &str) {
    let ask = LEGACY_ASK.iter().find_map(|p| line.strip_prefix(p));
    let instead = line.strip_prefix(INSTEAD_NEXT).or_else(|| line.strip_prefix(LEGACY_INSTEAD));
    let (route, list) = match (ask, line.strip_prefix("placebo "), instead) {
        (Some(l), _, _) => (Queued::Ask, l),
        (_, Some(l), _) => (Queued::Placebo, l),
        (_, _, Some(l)) => (Queued::Instead, l),
        _ => (Queued::Free, line),
    };
    (route, ids_in(list).collect(), tagged(list).1.unwrap_or_default())
}

/// One line at the end of the `.queued` file, under its lock (live.rs `locked`): never into a file a
/// hook is taking at the same moment.
fn enqueue(session: &str, line: &str) {
    let q = queued_file(session);
    live::locked(&q, || {
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&q).and_then(|mut f| f.write_all(format!("{line}\n").as_bytes()));
    });
}

/// The whole `.queued` file, taken under its lock (a rename, then read): what a check appends after
/// starts a new file, delivered next time.
fn take_queue(session: &str) -> Option<String> {
    let q = queued_file(session);
    let taken = state_file(session).with_extension(format!("queued.{}", std::process::id()));
    live::locked(&q, || std::fs::rename(&q, &taken)).ok()?;
    let text = std::fs::read_to_string(&taken).unwrap_or_default();
    let _ = std::fs::remove_file(&taken);
    Some(text)
}

/// The lessons a `.queued` file's `text` holds to be handed over (every line but a placebo's): decided
/// already by the check that queued them, so no other check decides them again (the stop's pool,
/// `decide`'s `had`). A placebo waiting was routed nowhere: it stays eligible.
fn waiting_in(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(queue_line).filter(|(route, _, _)| *route != Queued::Placebo).flat_map(|(_, ids, _)| ids)
}

/// At the session's end: whatever still waits in the `.queued` file was never delivered
/// (`never_delivered`, outcome.rs `DELIVERY`). Read, not taken: a resumed session still gets it at its
/// next prompt, and that delivery's record supersedes this one.
fn note_never_delivered(session: &str, input: &Value) {
    let text = std::fs::read_to_string(queued_file(session)).unwrap_or_default();
    for line in text.lines() {
        let (route, ids, check) = queue_line(line);
        let channel = match route {
            Queued::Ask => "live",
            Queued::Instead | Queued::Free => "queued",
            Queued::Placebo => outcome::PLACEBO,
        };
        record_route(session, input, check, ids.into_iter().map(String::from).collect(), channel, outcome::ROUTE_NEVER, "session ended");
    }
}

/// For each of `ids`, its value on the latest `prefix` line (`#borg-p `, `#borg-pf `) tagged `check`;
/// empty when any of them has none there (unknown, never a guess).
fn tagged_p(state: &str, prefix: &str, check: &str, ids: &[&str]) -> Vec<f64> {
    let mut seen: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    for line in state.lines().filter_map(|l| l.strip_prefix(prefix)) {
        let (items, at) = tagged(line);
        if at != Some(check) {
            continue;
        }
        for (id, p) in items.split(',').filter_map(|it| it.rsplit_once('=')) {
            if let Ok(p) = p.trim().parse::<f64>() {
                seen.insert(id.trim(), p);
            }
        }
    }
    ids.iter().map(|i| seen.get(i).copied()).collect::<Option<Vec<f64>>>().unwrap_or_default()
}

/// Lessons, each with the check that routed it.
type Pairs<'a> = Vec<(&'a conscience::Challenge, &'a str)>;

/// Where a lesson the queue drops at delivery came from and why: its check, channel, route (outcome.rs
/// `ROUTES`) and reason.
type Drop<'a> = (&'a str, &'static str, &'static str, &'static str);

/// The waiting graded lessons, once (`queued_file`): a live check's asks from before 0.7.6 (`LEGACY_ASK`)
/// unless done (`done_in`, the definition its check's pool used), handed over all and first and marked
/// and recorded as asks on channel live (`record_asked`); the free lessons not asked or handed over in
/// the session yet (`had_in`), those in place of a question (`INSTEAD_NEXT`, `LEGACY_INSTEAD`) all and next, the
/// others as many as one early delivery fits beside them (`conscience::render_owing`); and last the
/// placebos not shown meanwhile (asked or handed over, now or before; one the budget cut stays a placebo),
/// never shown but marked for the judge (`#borg-placebo`, judged as a free delivery is). Free deliveries
/// are marked `#borg-queued <ids> @<check>` in the state and .edits files (after the work so far, `WORK`),
/// so neither this queue nor early delivery repeats them, and Stop treats them as early lessons (done once
/// acted on); an `asked` outcome per routing check with channel queued and that check's free propensity
/// (`#borg-pf`, `record_delivery`). Every lesson of the queue gets its `delivery` record: asked, free,
/// placebo, or dropped and why.
fn queued(so_far: &SoFar, hook_event: &str, session: &str, input: &Value) -> Option<String> {
    let text = take_queue(session)?;
    let all = lessons(session);
    let state = std::fs::read_to_string(state_file(session)).unwrap_or_default();
    let had = had_in(&state);
    let mut done: Option<std::collections::HashSet<&str>> = None;
    let mut lines: Vec<(Queued, Vec<&str>, &str)> = text.lines().map(queue_line).collect();
    // Legacy asks, then free lessons in place of questions, then the other free ones, then placebos
    // (`Queued`'s order), each in its lines' order.
    lines.sort_by_key(|l| l.0);
    let placebos = lines.iter().position(|l| l.0 == Queued::Placebo).unwrap_or(lines.len());
    let (mut wanted, mut placebo): (Pairs, Pairs) = (Vec::new(), Vec::new());
    let mut dropped: Vec<(Drop, &str)> = Vec::new();
    // The first `owed` of `wanted`: the legacy asks (its first `asks`) and the free lessons in place of
    // questions, which sort first.
    let (mut owed, mut asks) = (0, 0);
    for (route, ids, check) in &lines[..placebos] {
        let channel = if *route == Queued::Ask { "live" } else { "queued" };
        for &id in ids {
            let Some(c) = all.iter().find(|c| c.id == id) else {
                dropped.push(((check, channel, outcome::ROUTE_DROPPED_DONE, "not deliverable"), id));
                continue;
            };
            let why = if wanted.iter().any(|(c, _)| c.id == id) {
                Some("queued twice")
            } else if *route == Queued::Ask {
                done.get_or_insert_with(|| done_in(&state, &early_acted(session, &all, ""))).contains(id).then_some("done")
            } else {
                had.contains(id).then_some("given before")
            };
            match why {
                Some(why) => dropped.push(((check, channel, outcome::ROUTE_DROPPED_DONE, why), id)),
                None => {
                    owed += usize::from(*route != Queued::Free);
                    asks += usize::from(*route == Queued::Ask);
                    wanted.push((c, check));
                }
            }
        }
    }
    let free: Vec<&conscience::Challenge> = wanted.iter().map(|(c, _)| *c).collect();
    let shown = conscience::render_owing(&free, &[], owed);
    let n = shown.as_ref().map_or(0, |(_, n)| *n);
    dropped.extend(wanted[n..].iter().map(|(c, check)| ((*check, "queued", outcome::ROUTE_DROPPED_CHARS, "budget"), c.id.as_str())));
    // A placebo is a lesson the drone was not shown: decided against what this delivery shows (the free
    // lessons within the budget) and what it was shown before.
    for (_, ids, check) in &lines[placebos..] {
        for &id in ids {
            let Some(c) = all.iter().find(|c| c.id == id) else {
                dropped.push(((check, outcome::PLACEBO, outcome::ROUTE_DROPPED_DONE, "not deliverable"), id));
                continue;
            };
            let why = if placebo.iter().any(|(c, _)| c.id == id) {
                Some("queued twice")
            } else {
                (wanted[..n].iter().any(|(c, _)| c.id == id) || had.contains(id)).then_some("delivered")
            };
            match why {
                Some(why) => dropped.push(((check, outcome::PLACEBO, outcome::ROUTE_DROPPED_DONE, why), id)),
                None => placebo.push((c, check)),
            }
        }
    }
    let mut parts: Vec<String> = Vec::new();
    let mut markers: Vec<String> = Vec::new();
    if let Some((text, n)) = shown {
        // The legacy asks, all shown (owed): one write per check, `#borg-live` right after its
        // `#borg-asked` (`pending` reads the pair as a live ask).
        let now = learn::now_ms();
        for (check, ids) in by_check(&wanted[..asks]) {
            let list = ids.join(",");
            markers.push(format!("#borg-asked {list}{}\n#borg-live {list} ms={now}", tag(check)));
            let members: Vec<&conscience::Challenge> = ids.iter().filter_map(|i| free.iter().find(|c| c.id == *i).copied()).collect();
            record_asked(session, input, &members, "live", check);
        }
        for (check, ids) in by_check(&wanted[asks..n]) {
            markers.push(format!("#borg-queued {}{}", ids.join(","), tag(check)));
            let delivered: Vec<&conscience::Challenge> = ids.iter().filter_map(|i| free.iter().find(|c| c.id == *i).copied()).collect();
            // The routing check's chance of handing each over free; 1 on an older binary's line (the
            // fixed rule's certain hand-over).
            let p = if check.is_empty() { vec![1.0; ids.len()] } else { tagged_p(&state, "#borg-pf ", check, &ids) };
            record_delivery(session, input, &delivered, "queued", check, p, Vec::new());
        }
        record_routes(session, input, &wanted[asks..n], "queued", outcome::ROUTE_FREE, "");
        log_cost(session, &format!("free event={hook_event} lessons={n} chars={} graded dropped={}", text.len(), wanted.len() - n));
        crate::stats::count(&learn::home(), crate::stats::QUEUED_FREE, session, |e| {
            e.n = n as u64;
            e.lessons = wanted[..n].iter().map(|(c, _)| c.id.clone()).collect();
            e.channel = "queued".into();
        });
        parts.push(text);
    }
    // Placebos: nothing reaches the drone; the next judge call sees each as it would a free delivery.
    for (check, ids) in by_check(&placebo) {
        markers.push(format!("#borg-placebo {}{}", ids.join(","), tag(check)));
    }
    record_routes(session, input, &placebo, outcome::PLACEBO, outcome::ROUTE_PLACEBO, "");
    if !markers.is_empty() {
        let markers = markers.join("\n");
        append(session, &markers);
        note_work(session, so_far, &markers);
    }
    let mut groups: Vec<(Drop, Vec<String>)> = Vec::new();
    for (key, id) in dropped {
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, ids)) => ids.push(id.to_string()),
            None => groups.push((key, vec![id.to_string()])),
        }
    }
    for ((check, channel, route, why), ids) in groups {
        record_route(session, input, check, ids, channel, route, why);
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// This turn's work so far (harness `work_so_far`): from the transcript where the harness
/// writes one, else the turn's tool calls (`turn_file`): no prose.
fn work_so_far(h: &dyn Harness, input: &Value) -> String {
    let session = input.get("session_id").and_then(Value::as_str).unwrap_or("unknown");
    harness::work_so_far(h.caps(), input, &turn_file(session), EVIDENCE_CHARS)
}

/// One hook's work so far (`work_so_far`), read when a delivery first needs it and then shared: the free
/// deliveries and placebos of one prompt or tool call write the same work line (`note_work`)
/// from one read of the transcript, and a hook that delivers nothing reads none.
struct SoFar<'a> {
    h: &'a dyn Harness,
    input: &'a Value,
    text: std::cell::OnceCell<String>,
}

impl<'a> SoFar<'a> {
    fn new(h: &'a dyn Harness, input: &'a Value) -> Self {
        SoFar { h, input, text: std::cell::OnceCell::new() }
    }

    fn get(&self) -> &str {
        self.text.get_or_init(|| work_so_far(self.h, self.input))
    }
}

/// This turn's tool calls, one `[tool] touch` line each, and their edit text: the work so far where the
/// harness sends no transcript. Emptied at each prompt.
fn turn_file(session: &str) -> PathBuf {
    state_file(session).with_extension("turn")
}

/// One open queen question for the engineer (queen.rs `deliver`), at the first prompt of the session
/// where one of its work repos (the cwd's and the touched ones, systems.rs `work_repos`) has one: never
/// another repo's. Its commands run this binary in the session's `cwd`, wherever the drone's shell has
/// gone since. 0 model calls; fails open. Off with `BORG_QUEEN_ASK=off` or `BORG_LEARN=off` (nothing
/// recorded; eval drones cannot answer), and in the eval modes BORG_ONLY and BORG_CRITIC.
fn queen_ask(h: &dyn Harness, session: &str, cwd: &str) -> Option<String> {
    if !learning() || !crate::queen::ask_enabled() || std::env::var_os("BORG_ONLY").is_some() || std::env::var_os("BORG_CRITIC").is_some() {
        return None;
    }
    let repos = || {
        let mut repos = crate::systems::work_repos(&std::fs::read_to_string(where_file(session)).unwrap_or_default());
        let here = repo_of(cwd);
        if !here.is_empty() && !repos.contains(&here) {
            repos.insert(0, here);
        }
        repos
    };
    let scope = if cwd.is_empty() { String::new() } else { format!(" --dir {}", crate::queen::sh_quote(cwd)) };
    let mut q = crate::queen::deliver(&learn::home(), session, repos, &scope, learn::now_ms())?;
    if let Ok(exe) = std::env::current_exe() {
        q = q.run_by(&crate::queen::sh_quote(&exe.display().to_string()));
    }
    let text = crate::queen::render_delivery(&q, h.caps().question_dialog);
    log_cost(session, &format!("free event=UserPromptSubmit queen={} chars={}", q.id, text.len()));
    Some(text)
}

/// The free context of a prompt or touch: waiting graded lessons first, then an early delivery.
fn free_context(h: &dyn Harness, hook_event: &str, session: &str, input: &Value, text: &str) -> Option<Value> {
    free_context_with(h, hook_event, session, input, text, None)
}

/// `free_context` plus `extra` (a queen question) last.
fn free_context_with(h: &dyn Harness, hook_event: &str, session: &str, input: &Value, text: &str, extra: Option<String>) -> Option<Value> {
    let so_far = SoFar::new(h, input);
    let parts: Vec<String> =
        [queued(&so_far, hook_event, session, input), early(h, &so_far, hook_event, session, input, text), extra].into_iter().flatten().collect();
    (!parts.is_empty()).then(|| json!({"hookSpecificOutput": {"hookEventName": hook_event, "additionalContext": parts.join("\n\n")}}))
}

/// What one model check of the work decided (`check`).
enum Verdict {
    /// The free gate skipped the model call: the score, or `noedit`.
    Skip(String),
    /// The model gave no answer: fail open.
    Failed,
    /// The ids to ask, in a block at the stop.
    Ask(Vec<String>),
}

/// The work a check judges, as the stop reads it.
struct Work<'a> {
    /// Every lesson of the session it may deliver (the ungraded widening looks here).
    all: &'a [conscience::Challenge],
    /// `all` and the absorbed lessons withheld from it (`judged`): the judge's pending lessons look here,
    /// so a delivery made before an absorb still gets its verdict.
    judged: &'a [conscience::Challenge],
    /// The lessons that may be asked: not done (`done_in`), not decided already (waiting in the queue to
    /// be asked or handed over, `waiting_in`), not of another system.
    challenges: &'a [conscience::Challenge],
    done: &'a std::collections::HashSet<&'a str>,
    /// The state file as read before the check.
    state: &'a str,
    /// The state file plus the drone's work text: the gate finds the turn in it by its markers; the pool
    /// and the model read it without them (`conscience::work_text`).
    context: &'a str,
    scope_text: &'a str,
    /// The drone's message for the judge (`pending`): its final message at the stop.
    message: &'a str,
    /// BORG_STOP_GATE=edits: whether the turn changed anything.
    changed: bool,
    /// Whether this check judges the pending lessons: not while another (the session-end judge, holding
    /// the session lock) may be judging the same ones.
    judge: bool,
    /// The harness's stop budget (none: the model's own limit) and when the hook started: the model call
    /// gets what is left of it when it starts (live.rs `call_limit`).
    budget: Option<std::time::Duration>,
    started: std::time::Instant,
    /// This check's id (select.rs `check_id`), computed once when it started: on its `graded` record and
    /// on every marker of what it routed.
    check: &'a str,
    /// The selection summary beside the lessons file, read at the check's start (select/summary.rs
    /// `refresh`): what sizes the pool before the model call, and what a decision by a policy that reads
    /// no gate reads; a learned decision reads it again, under the store's decision lock (`decide`).
    summary: &'a crate::select::summary::Summary,
}

/// What a check's `graded` record says of its pool and its model call (`record_graded`).
struct Call<'a> {
    /// Lessons the pool allowed, and those shown in the order they filled its budget (whole.rs
    /// `select`; the shortlist's order without the whole pool).
    eligible: usize,
    order: &'a [&'a conscience::Challenge],
    /// The tail sample shown after the pool (whole.rs `tail`): each lesson with its place in the full
    /// fill order (`order.len()` + its place among the lessons the budget dropped), and each one's
    /// chance of the draw (0 without a tail).
    tail: &'a [(usize, &'a conscience::Challenge)],
    pt: f64,
    /// The pool's token budget the check filled to (0 without the whole pool), outcome.rs `budget`.
    budget: usize,
    model: &'a str,
    prompt_hash: String,
    dur_ms: u64,
    /// The call's cost as the CLI reported it (0 for a failed call).
    usd: f64,
}

/// The consequence model's check of `w` at the stop: the free gate, the pool (with its tail sample,
/// whole.rs `tail`), one graded call that also judges the pending lessons, and the graded split (select.rs
/// `graded_under`), decided and recorded as one step (`decide`: under the store's decision lock for the
/// learned policy, which carries out a decision only once its record landed where its gate is folded from).
/// Its decision is logged before anything carries it out (the `graded` record, then `#borg-graded`,
/// `#borg-select`, `#borg-p`, `#borg-pf` in one write, then the queue): a hook killed in between asks
/// nothing and leaves no marker of an unlogged decision. The free lessons and the placebos wait in the
/// `.queued` file; what to ask is returned. Logged as `check`.
fn check(session: &str, input: &Value, w: &Work) -> Verdict {
    // The free gate (prefilter.rs): a turn whose words touch no lesson costs no model call; the conscience
    // then asked nothing. BORG_STOP_GATE=edits instead: a turn that changed nothing costs no model call.
    let skip = if prefilter::edits_gate() {
        (!w.changed).then(|| "noedit".to_string())
    } else {
        prefilter::skip(w.challenges, w.context).map(|score| format!("{score:.2}"))
    };
    if let Some(score) = skip {
        return Verdict::Skip(score);
    }
    // What the pool and the model read: the work, without the state file's bookkeeping lines.
    let text = conscience::work_text(w.context);
    let text = text.as_ref();
    // BM25's top 30, plus lessons the vector index finds that word matching missed (vectors.rs; here only,
    // where the model call dwarfs loading the embedder). Top 30 found as much as top 20 + vectors; both
    // together find the most (internal evaluation "vectors", not published).
    let started = std::time::Instant::now();
    let shortlist_now = || {
        let (list, added) = crate::vectors::shortlist(&lessons_path(), w.challenges, text, w.scope_text, crate::vectors::SHORTLIST);
        if crate::vectors::enabled() {
            log_cost(session, &format!("vectors added={added} ms={}", started.elapsed().as_millis()));
        }
        list
    };
    // The whole pool (whole.rs, default): every lesson the Stop allows, up to a token budget; the
    // shortlist's order decides only when they do not all fit. BORG_STOP_FORMAT=classic asks about that
    // pool in the shortlist's layout (predict::prompt).
    let whole = crate::whole::on();
    let (shortlist, eligible, order, tail, pt, budget) = if whole {
        let ranked = conscience::rank_scoped(w.challenges, text, w.scope_text);
        let eligible = crate::whole::eligible(w.challenges, &ranked, w.scope_text);
        let n = eligible.len();
        // The pool's budget: BORG_STOP_POOL_TOKENS when set, else the policy's (select.rs `pool_tokens`:
        // today's `whole::TOKENS` unless the learned policy's gate may risk a larger one that fits the
        // model call's limit).
        let tokens = match std::env::var_os("BORG_STOP_POOL_TOKENS") {
            Some(_) => crate::whole::tokens(),
            None => {
                let limit = live::call_limit(w.budget, w.started.elapsed());
                crate::select::pool_tokens(w.summary, Some(u64::try_from(limit.as_millis()).unwrap_or(u64::MAX)), session)
            }
        };
        let f = crate::whole::select_filled(eligible, shortlist_now, &ranked, tokens);
        // The tail sample (plan W3b "Pool"), under every policy: beside a pool the budget cut, a few of
        // the lessons it dropped, drawn by the check, shown after the pool (its stable prefix stays), so
        // the yield of fill positions past the budget is measured. Never beside an empty pool: the
        // sample alone would pay for a model call the budget does not.
        let (drawn, pt) = if f.order.is_empty() { (Vec::new(), 0.0) } else { crate::whole::tail(&f.dropped, tail_seed(session, w.check, w.summary)) };
        let tail: Vec<(usize, &conscience::Challenge)> = drawn.into_iter().map(|(i, c)| (f.order.len() + i, c)).collect();
        let sampled = if tail.is_empty() { String::new() } else { format!(" tail={}", tail.len()) };
        log_cost(session, &format!("pool mode=all eligible={n} shown={} budget={}{sampled}", f.shown.len(), if f.fell { "full" } else { "fits" }));
        let mut shown = f.shown;
        shown.extend(tail.iter().map(|(_, c)| *c));
        (shown, n, f.order, tail, pt, tokens)
    } else {
        let list = shortlist_now();
        (list.clone(), list.len(), list, Vec::new(), 0.0, 0)
    };
    let model = std::env::var("BORG_PREDICT_MODEL").unwrap_or_default();
    // R1: the same call judges what the drone did with lessons delivered earlier this session.
    let judging = if w.judge {
        pending(session, w.state, w.judged, w.message, When::Stop, w.check)
    } else {
        Judging { pending: Vec::new(), question: false, crowded: Vec::new() }
    };
    let pend = &judging.pending;
    // Nothing to ask and no question to judge (no lesson in the pool, none pending): no model call.
    // Free deliveries alone never pay for a call: they wait for the next one, or `borg learn` settles them.
    if shortlist.is_empty() && !judging.question {
        return Verdict::Skip("empty".into());
    }
    let extra = predict::judge_section(pend).len();
    let graded = crate::whole::graded();
    let compact = whole && !crate::whole::classic();
    let timeout = live::call_limit(w.budget, w.started.elapsed());
    // The work the call shows holds no stripped question's declaration either: this turn's message may
    // be the drone's answer to it.
    let shown_work = without_declarations(text, pend);
    let called = std::time::Instant::now();
    let pred = if compact {
        predict::predict_whole(&shown_work, &shortlist, pend, &model, graded, timeout)
    } else {
        predict::predict(&shown_work, &shortlist, pend, &model, graded, timeout)
    };
    let dur_ms = called.elapsed().as_millis() as u64;
    stat(session, input, crate::stats::CHECK, |e| {
        e.n = shortlist.len() as u64;
        e.dur_ms = dur_ms;
        e.usd = pred.as_ref().map_or(0.0, |p| p.cost);
        if pred.is_none() {
            e.why = "failed".into();
        }
    });
    let usd = pred.as_ref().map_or(0.0, |p| p.cost);
    let call = Call { eligible, order: &order, tail: &tail, pt, budget, model: &model, prompt_hash: predict::template_hash(compact, graded), dur_ms, usd };
    note_unjudged(session, &judging.crowded, "slots");
    let Some(pred) = pred else {
        // Fail open: nothing asked, the pending lessons stay pending (no try counted).
        if graded {
            record_graded(session, input, w, &[], None, &call);
        }
        note_unjudged(session, &pend.iter().map(|p| p.lesson.id.clone()).collect::<Vec<_>>(), "failed");
        return Verdict::Failed;
    };
    // The judge's call numbering counts every model call of the session.
    append(session, &format!("#borg-predict-cost {:.4}", pred.cost));
    if !pend.is_empty() {
        let call = w.state.lines().filter(|l| l.starts_with("#borg-predict-cost")).count() + 1;
        // The whole prompt's characters (`prompt=`): the judge's effective length is measured by it (judge_length.rs).
        let logged = Logged { call, chars: extra, model: judge_model(&model), fields: format!(" prompt={}", pred.prompt) };
        record_verdicts(session, &repo_of(input.get("cwd").and_then(Value::as_str).unwrap_or_default()), "", pend, &pred, &logged);
    }
    let applied: Vec<(&conscience::Challenge, u8)> =
        pred.applies.iter().zip(&pred.grades).filter_map(|(i, g)| shortlist.iter().find(|c| &c.id == i).map(|c| (*c, *g))).collect();
    let ids = |cs: &[&conscience::Challenge]| cs.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
    if graded {
        // Graded (default): select.rs decides what of the applies to ask, hand over free at the next
        // prompt or touch, or judge as a placebo (the fixed rule: ask only for a 3, else the 2s and 1s
        // free, conscience.rs `graded_without` with the session's `had`).
        let lessons = lessons_path();
        // Decided at the pool tokens the record logs, which the fit and the gate's ledger read; and, under
        // the learned policy, with the store's other checks one at a time (`decide`).
        let at = Decide {
            session,
            check: w.check,
            channel: "stop",
            lessons: &lessons,
            tokens: crate::select::shown_tokens(call.order),
            recorded: records_reach_gate(&lessons),
        };
        let r = decide(crate::select::policy(), &applied, &at, w.summary, DECIDE_WAIT, |r, basis| {
            record_graded(session, input, w, &applied, Some((r, basis)), &call)
        });
        let routed = |cs: &[&conscience::Challenge], p: &[f64]| -> Vec<(&str, f64)> {
            applied.iter().zip(p).filter(|((c, _), _)| cs.iter().any(|x| x.id == c.id)).map(|((c, _), p)| (c.id.as_str(), *p)).collect()
        };
        let mut lines = vec![graded_line(&applied), r.log.line(applied.len(), w.check)];
        lines.extend(r.log.attention.clone());
        lines.extend(p_line("#borg-p", routed(&r.ask, &r.p), Some(w.check)));
        lines.extend(p_line("#borg-pf", routed(&r.free, &r.pf), Some(w.check)));
        append(session, &lines.join("\n"));
        let queue = !r.free.is_empty();
        log_cost(session, &format!("graded applied={} ask={} free={} {}", applied.len(), r.ask.len(), r.free.len(), if queue { "queued" } else { "dropped" }));
        if queue {
            // The learned policy's hand-overs of lessons the fixed rule asks (on a check with a 3: the
            // evidence rule's) are marked (`INSTEAD_NEXT`), so the free delivery's budget never cuts what
            // a question would have carried.
            let asks = if r.log.policy == crate::select::Policy::Learned.name() { conscience::graded_without(&applied, None, &[]).0 } else { Vec::new() };
            let (owed, rest): (Vec<&conscience::Challenge>, Vec<&conscience::Challenge>) = r.free.iter().partition(|c| asks.iter().any(|a| a.id == c.id));
            if !owed.is_empty() {
                enqueue(session, &format!("{INSTEAD_NEXT}{}{}", ids(&owed).join(","), tag(w.check)));
            }
            if !rest.is_empty() {
                enqueue(session, &format!("{}{}", ids(&rest).join(","), tag(w.check)));
            }
        }
        if !r.placebo.is_empty() {
            enqueue(session, &format!("placebo {}{}", ids(&r.placebo).join(","), tag(w.check)));
        }
        Verdict::Ask(ids(&r.ask))
    } else {
        // R3b: the model decides whether to ask (at the stop, an extra round); once it does, the questions
        // also carry every other lesson over the low bar (conscience.rs `widen`).
        let predicted: Vec<&conscience::Challenge> = applied.iter().map(|(c, _)| *c).collect();
        let pool = conscience::widen_pool(&conscience::rank_scoped(w.all, text, w.scope_text), &predicted, w.done);
        let widened: Vec<&conscience::Challenge> =
            conscience::in_scope_first(conscience::explored(pool.iter().copied())).into_iter().filter(|c| w.challenges.iter().any(|x| x.id == c.id)).collect();
        append_p_of(session, &pool, &widened, w.check);
        // Ungraded, the stop asks whatever it widens to: so it would have asked these.
        Verdict::Ask(ids(&widened))
    }
}

/// Where a graded check decides (`decide`): the parts of its select.rs `Ctx` the summary does not hold.
struct Decide<'x> {
    session: &'x str,
    check: &'x str,
    channel: &'static str,
    lessons: &'x std::path::Path,
    /// The check's pool tokens (select.rs `Ctx::tokens`).
    tokens: u32,
    /// Whether the check's `graded` record lands in the log its gate is folded from
    /// (`records_reach_gate`): a learned decision whose record would not is never debited.
    recorded: bool,
}

/// What a graded check's decision read (`decide`), for its record: the selection summary, the lessons
/// the session already had or has waiting in its queue (select.rs `Ctx::had`), and why it fell back to
/// the fixed rule (outcome.rs `DECIDED_BUSY`, `DECIDED_NO_LOCK`, `DECIDED_NO_RECORD`; empty when it
/// decided by the policy in force).
struct Basis<'s> {
    summary: &'s crate::select::summary::Summary,
    had: &'s [String],
    why: &'static str,
}

/// How long a learned check waits for the store's decision lock (`decide`) at most; past it, the check
/// takes the fixed rule and its record says why. Guard: a check that holds the lock long (a summary
/// rebuilt from a long log, a stopped process) would otherwise hold up every other learned check's
/// answer. Derived: a third of
/// what the stop's model call leaves for the hook's records and answer (live.rs `AFTER_CALL`), so the
/// wait never costs a stop its answer.
const DECIDE_WAIT: std::time::Duration = std::time::Duration::from_millis(live::AFTER_CALL.as_millis() as u64 / 3);

/// Method: how often a check waiting for the decision lock tries it again (`lock_within`).
const DECIDE_POLL: std::time::Duration = std::time::Duration::from_millis(5);

/// The store's decision lock (`decide`), beside the selection summary it guards.
fn decide_lock(lessons: &std::path::Path) -> PathBuf {
    crate::select::summary::cache_path(lessons).with_extension("json.lock")
}

/// `file` locked exclusively, tried every `DECIDE_POLL` for at most `wait` (once for none); unlocked when
/// the handle drops. Err with why when it stays held (`DECIDED_BUSY`) or cannot be taken at all
/// (`DECIDED_NO_LOCK`).
fn lock_within(file: &std::path::Path, wait: std::time::Duration) -> Result<std::fs::File, &'static str> {
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(file).map_err(|_| outcome::DECIDED_NO_LOCK)?;
    let until = std::time::Instant::now() + wait;
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < until => std::thread::sleep(DECIDE_POLL),
            Err(std::fs::TryLockError::WouldBlock) => return Err(outcome::DECIDED_BUSY),
            Err(std::fs::TryLockError::Error(_)) => return Err(outcome::DECIDED_NO_LOCK),
        }
    }
}

/// A graded check's decision and its record (`record`, which answers whether the record landed) as one
/// step. Only the learned policy reads the gate (select/policy.rs `choose`): its checks decide one at a
/// time under the store's decision lock (`decide_lock`), never during the model call, since learned
/// checks that decide at once (the checks of several sessions on one store) would each take all the gate
/// may still risk. Under the lock the summary is loaded again
/// (select/summary.rs `refresh`: the log's tail since the check started, other checks' records and their
/// ledger entries included), decided on (select.rs `graded_under`) and recorded with that summary as the
/// one the decision read (outcome.rs `seen`); then released. A learned decision is carried out only once
/// its record landed in the log its gate is folded from, the gate's only debit: one whose record would
/// not land there (`Decide::recorded`) or did not (`record`'s answer) takes the fixed rule instead
/// (`DECIDED_NO_RECORD`), as one that cannot take the lock within `wait` does (on `stale`, the summary
/// it read at its start), and its record says why (`Basis::why`). The fixed rule and the evaluation
/// switches read no gate: they decide by themselves on `stale`, without the lock, whatever other checks
/// hold. Every way, the session's lessons delivered by now (`had_in`, read again: a tool call may have
/// handed one over during the model call) or waiting in its queue (`waiting_in`) never go free.
fn decide<'a>(
    policy: crate::select::Policy,
    applied: &[(&'a conscience::Challenge, u8)],
    at: &Decide,
    stale: &crate::select::summary::Summary,
    wait: std::time::Duration,
    mut record: impl FnMut(&crate::select::Routed<'a>, &Basis) -> bool,
) -> crate::select::Routed<'a> {
    use crate::select::Policy;
    let state = std::fs::read_to_string(state_file(at.session)).unwrap_or_default();
    let queue = std::fs::read_to_string(queued_file(at.session)).unwrap_or_default();
    let mut had: Vec<String> = had_in(&state).into_iter().chain(waiting_in(&queue)).map(str::to_string).collect();
    had.sort_unstable();
    had.dedup();
    // The collective's tolerance, read once: every way below decides and logs under it (select.rs `Ctx::alpha`).
    let alpha = crate::select::alpha(at.lessons);
    // Decided under `policy` on `summary` and recorded with `why`; whether the record landed.
    let mut on = |summary: &crate::select::summary::Summary, policy: Policy, why: &'static str| {
        let ctx =
            crate::select::Ctx { session: at.session, check: at.check, channel: at.channel, lessons: at.lessons, summary, tokens: at.tokens, had: &had, alpha };
        let r = crate::select::graded_under(policy, applied, &ctx);
        let landed = record(&r, &Basis { summary, had: &had, why });
        (r, landed)
    };
    if policy != Policy::Learned {
        return on(stale, policy, "").0;
    }
    if !at.recorded {
        return on(stale, Policy::Fixed, outcome::DECIDED_NO_RECORD).0;
    }
    match lock_within(&decide_lock(at.lessons), wait) {
        Ok(_held) => {
            let summary = crate::select::summary::refresh(at.lessons);
            match on(&summary, policy, "") {
                (r, true) => r,
                // Still under the lock: no other check decided on the gate this one left as it was.
                (_, false) => on(&summary, Policy::Fixed, outcome::DECIDED_NO_RECORD).0,
            }
        }
        Err(why) => on(stale, Policy::Fixed, why).0,
    }
}

/// Whether a check's `graded` record (`record_graded`) lands in the log its gate is folded from, the
/// outcomes beside `lessons` (select/summary.rs `refresh`): learning on, and `lessons` in the store the
/// hook records to. A `BORG_CONSCIENCE` elsewhere reads the gate of its own store, which no record of
/// this hook reaches.
fn records_reach_gate(lessons: &std::path::Path) -> bool {
    learning() && outcome::path_beside(lessons) == graded_log()
}

/// Where a check's `graded` record goes: beside the store's other outcomes.
fn graded_log() -> PathBuf {
    learn::home().join("outcomes.jsonl")
}

/// The seed of a check's tail sample (whole.rs `tail`): its own stream of the check's seed (select.rs
/// `seed`, which reads only the session, channel and check; the tag spells "tailpool").
fn tail_seed(session: &str, check: &str, summary: &crate::select::summary::Summary) -> u64 {
    let ctx = crate::select::Ctx {
        session,
        check,
        channel: "stop",
        lessons: std::path::Path::new(""),
        summary,
        tokens: 0,
        had: &[],
        alpha: crate::select::policy::ALPHA,
    };
    crate::select::seed(&ctx) ^ u64::from_be_bytes(*b"tailpool")
}

/// The `graded` record of one check (outcome.rs `GRADED`, `graded_outcome`), appended beside the store's
/// other outcomes (`graded_log`); whether it landed. Nothing with learning off.
fn record_graded(
    session: &str,
    input: &Value,
    w: &Work,
    applied: &[(&conscience::Challenge, u8)],
    decided: Option<(&crate::select::Routed, &Basis)>,
    call: &Call,
) -> bool {
    if !learning() {
        return false;
    }
    let repo = repo_of(input.get("cwd").and_then(Value::as_str).unwrap_or_default());
    let o = graded_outcome(session, &repo, w.check, applied, decided, w.summary, call);
    outcome::append(&graded_log(), &o).is_ok()
}

/// The `graded` record of one check: every check that called the model, one with no applies and a
/// failed one (`decided` none; its fit is that of `read`, the summary the check read at its start)
/// included, so each decision and each pool can be replayed. Per apply its grade, rank, fill position
/// (`Call::order`; a tail lesson's past the budget's, `Call::tail`), propensities to be asked (`p`) and
/// handed over free (`pf`); the routes (`chosen`, `free`, `placebo`, and ψ, each apply's chance of the
/// placebo draw among those routed nowhere, select.rs `Log::psi`); the applies that could not go free
/// because the session had them (`nofree`); the question-count test's k and s on a check it considered
/// (`xk`, `xs`, select.rs `Log::xk`); the tail sample (`tail`, `pt`, and after the applies' fill
/// positions each tail lesson's, outcome.rs `fill`); the policy, its fit, the summary the decision read
/// (`Basis`) and why it fell back to the fixed rule (`reason`); and the call.
#[allow(clippy::too_many_arguments)] // reason: one record of a check, from the parts the check holds apart
fn graded_outcome(
    session: &str,
    repo: &str,
    check: &str,
    applied: &[(&conscience::Challenge, u8)],
    decided: Option<(&crate::select::Routed, &Basis)>,
    read: &crate::select::summary::Summary,
    call: &Call,
) -> outcome::Outcome {
    let ids = |cs: &[&conscience::Challenge]| cs.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
    let rounded = |p: &[f64]| p.iter().map(|x| outcome::round_p(*x)).collect::<Vec<_>>();
    let (routed, basis) = (decided.map(|d| d.0), decided.map(|d| d.1));
    let shown: u32 = call.order.len().try_into().unwrap_or(u32::MAX);
    let place = |id: &str| call.order.iter().position(|x| x.id == id).or_else(|| call.tail.iter().find(|(_, x)| x.id == id).map(|(at, _)| *at));
    outcome::Outcome {
        kind: outcome::GRADED.into(),
        session: session.into(),
        lessons: applied.iter().map(|(c, _)| c.id.clone()).collect(),
        ms: learn::now_ms(),
        repo: repo.into(),
        channel: "stop".into(),
        grades: applied.iter().map(|(_, g)| *g).collect(),
        ranks: (0..applied.len()).map(|r| u16::try_from(r).unwrap_or(u16::MAX)).collect(),
        applied: u16::try_from(applied.len()).unwrap_or(u16::MAX),
        chosen: routed.map(|r| ids(&r.ask)).unwrap_or_default(),
        explore: routed.is_some_and(|r| r.log.source == "explore"),
        p: routed.map(|r| rounded(&r.p)).unwrap_or_default(),
        check: check.into(),
        policy: routed.map_or_else(|| crate::select::policy().name(), |r| r.log.policy).into(),
        fit: routed.map_or_else(|| read.fit.key.clone(), |r| r.log.fit.clone()),
        // The summary the decision's `Ctx` read (`decide`), not the log at this append.
        seen: basis.map(|b| b.summary.key()).unwrap_or_default(),
        source: routed.map(|r| r.log.source).unwrap_or_default().into(),
        free: routed.map(|r| ids(&r.free)).unwrap_or_default(),
        pf: routed.map(|r| rounded(&r.pf)).unwrap_or_default(),
        placebo: routed.map(|r| ids(&r.placebo)).unwrap_or_default(),
        psi: routed.map_or(0.0, |r| r.log.psi),
        nofree: basis.map(|b| crate::select::nofree(applied, b.had)).unwrap_or_default(),
        // The question-count test's k and s (select.rs `Log::xk`, `xs`): None outside it.
        xk: routed.and_then(|r| r.log.xk),
        xs: routed.and_then(|r| r.log.xs),
        // The tolerance it was decided under (select.rs `Log::alpha`): None at the default.
        alpha: routed.and_then(|r| r.log.alpha),
        tail: call.tail.iter().map(|(_, c)| c.id.clone()).collect(),
        pt: call.pt,
        reason: basis.map(|b| b.why).unwrap_or_default().into(),
        eligible: call.eligible.try_into().unwrap_or(u32::MAX),
        shown,
        tokens: crate::select::shown_tokens(call.order),
        budget: call.budget.try_into().unwrap_or(u32::MAX),
        // The applies' places, then each tail lesson's (aligned with `tail`, outcome.rs `fill`).
        fill: applied
            .iter()
            .map(|(c, _)| place(&c.id))
            .chain(call.tail.iter().map(|(at, _)| Some(*at)))
            .map(|i| i.map_or(u16::MAX, |i| u16::try_from(i).unwrap_or(u16::MAX)))
            .collect(),
        model: call.model.into(),
        prompt_hash: call.prompt_hash.clone(),
        harness: std::env::var("BORG_HARNESS").unwrap_or_default(),
        version: env!("CARGO_PKG_VERSION").into(),
        dur_ms: call.dur_ms,
        failed: routed.is_none(),
        usd: call.usd,
        ..Default::default()
    }
}

/// One hook event, canonical already (harness `normalise`), for the drone `h` runs: what borg answers
/// (Claude-shaped), or None. One function per event.
pub fn dispatch(h: &dyn Harness, event: &str, input: &Value) -> Option<Value> {
    let session = input.get("session_id").and_then(Value::as_str).unwrap_or("unknown");
    match event {
        "prompt" => on_prompt(h, session, input),
        "post_tool_failure" => {
            capture_test(session, input, true);
            if h.caps().deny_capture {
                capture_rejection(h, session, input);
            }
            None
        }
        "end" => {
            on_end(h, session, input);
            None
        }
        "post_tool" => {
            capture_test(session, input, false);
            on_post_tool(h, session, input)
        }
        "stop" => on_stop(h, session, input),
        _ => None,
    }
}

fn on_prompt(h: &dyn Harness, session: &str, input: &Value) -> Option<Value> {
    let prompt = input.get("prompt").and_then(Value::as_str).unwrap_or_default();
    append(session, prompt);
    if learning() {
        // Where the session started, once: its commits later get reasoning notes (notes.rs).
        crate::notes::remember_start(&crate::notes::state_dir(), session, input.get("cwd").and_then(Value::as_str).unwrap_or_default());
        crate::notes::mark(&crate::notes::state_dir(), session, "prompt");
    }
    // The engineer's own words, separately from the drone's touches (the queen's criterion 3).
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(prompts_file(session)) {
        let _ = writeln!(f, "{prompt}");
    }
    // "borg no <reason>": the engineer's veto (veto.rs), never the session's task nor a correction.
    let vetoed = (learning() && crate::veto::enabled()).then(|| crate::veto::reason_in(prompt)).flatten();
    match &vetoed {
        Some(reason) => veto(h, session, input, reason),
        None => capture_prompt(h, session, input, prompt),
    }
    note_where(session, &repo_of(input.get("cwd").and_then(Value::as_str).unwrap_or_default()));
    let _ = std::fs::write(state_file(session).with_extension("lastprompt"), prompt);
    // A newer borg (update.rs): the daily release check, started detached when `update.json` is stale
    // (one read of it when fresh), and what the drone hears of it below.
    let checked = crate::update::maybe_check(learn::now_ms());
    // An adapter that will not wait for the answer (`"context": false`), or a harness whose model never
    // sees a prompt's context, gets none: what is handed over here is marked delivered and would be lost.
    let out = (h.caps().prompt_context && input.get("context") != Some(&Value::Bool(false))).then(|| {
        let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
        let ask = queen_ask(h, session, cwd);
        // Once per session per state (`<session>.update`): a newer release, a member nothing syncs with,
        // a member on a newer borg; the routed store's mesh/ is read only when it exists.
        let update = crate::update::notice(checked.as_ref(), &learn::home(), &state_file(session).with_extension("update"));
        let extra: Vec<String> = [ask, vetoed.as_deref().map(crate::veto::render), update].into_iter().flatten().collect();
        free_context_with(h, "UserPromptSubmit", session, input, prompt, (!extra.is_empty()).then(|| extra.join("\n\n")))
    });
    // A new turn's work starts, after a free delivery here took the last turn's as its evidence.
    if harness::transcript(h.caps(), input).is_none() {
        let _ = std::fs::write(turn_file(session), "");
    }
    out.flatten()
}

/// A shell command that ran tests leaves `fail <ms>` or `pass <ms>` in `<state>/<session>.tests`: the
/// regress `tests` signal (`notes/regress_signals.rs`) and `borg learn`'s `tested` credit, whether or not
/// the harness reports denials. A call that did not fail and shows no output (an adapter that sends
/// none) leaves nothing: its result is unknown, not a pass.
fn capture_test(session: &str, input: &Value, failed: bool) {
    if !learning() || input.get("tool_name").and_then(Value::as_str) != Some("Bash") {
        return;
    }
    let command = input.pointer("/tool_input/command").and_then(Value::as_str).unwrap_or_default();
    if !crate::notes::regress_signals::runs_tests(command) {
        return;
    }
    let output = crate::notes::regress_signals::tool_output(input);
    if !failed && output.trim().is_empty() {
        return;
    }
    if let Some(what) = crate::notes::regress_signals::test_outcome(command, &output, failed)
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(state_file(session).with_extension("tests"))
    {
        let _ = writeln!(f, "{what} {}", learn::now_ms());
    }
}

fn on_end(h: &dyn Harness, session: &str, input: &Value) {
    let _ = std::fs::remove_file(denied_file(session));
    if !learning() {
        return;
    }
    // The session's landed commits get a redacted reasoning note, in the background.
    crate::notes::remember_proposal(&crate::notes::state_dir(), session, &full_proposal(h, input));
    crate::notes::mark(&crate::notes::state_dir(), session, "stop");
    // What still waits in the queue was never delivered (a resumed session's delivery supersedes it);
    // then the end itself, where the selection summary counts the session's deliveries (outcome.rs
    // `ENDED`), whatever it logged before.
    note_never_delivered(session, input);
    let end = outcome::Outcome {
        kind: outcome::ENDED.into(),
        session: session.into(),
        ms: learn::now_ms(),
        repo: repo_of(input.get("cwd").and_then(Value::as_str).unwrap_or_default()),
        ..Default::default()
    };
    let _ = outcome::append(&learn::home().join("outcomes.jsonl"), &end);
    // Generalise this session's corrections in the background; never delay the drone. The notes job
    // starts it once its links are recorded, so learning sees their regret episodes.
    if !crate::notes::on_end(session, input)
        && let Ok(exe) = std::env::current_exe()
    {
        let _ = std::process::Command::new(exe)
            .args(["learn", "--session", session])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

fn on_post_tool(h: &dyn Harness, session: &str, input: &Value) -> Option<Value> {
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
    let edit = edit_text(tool, tool_input);
    if let Some(edit) = &edit
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(edits_file(session))
    {
        let _ = writeln!(f, "{edit}");
    }
    // No transcript (omp, opencode): the turn's tool calls are the work so far a free delivery reads.
    if harness::transcript(h.caps(), input).is_none()
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(turn_file(session))
    {
        let _ = writeln!(f, "[{tool}] {touch}");
        if let Some(edit) = &edit {
            let _ = writeln!(f, "{edit}");
        }
    }
    // Context only where the harness's model reads it after a tool call; otherwise it waits for the next
    // prompt or the stop. Never at a subagent's tool call: its context goes to the subagent, so what the
    // queue hands over there would be recorded delivered to a drone that never saw it.
    (h.caps().tool_context && !subagent(input)).then(|| free_context(h, "PostToolUse", session, input, &touch)).flatten()
}

/// Whether the event is a subagent's (`agent_id`, as msg.rs `deliver` reads it): the session's id, but
/// Claude gives the hook's context to the subagent, not to the session's drone.
fn subagent(input: &Value) -> bool {
    input.get("agent_id").and_then(Value::as_str).is_some_and(|a| !a.is_empty())
}

/// The stop holds the session lock (live.rs) for its whole check, so the session-end judge of a session
/// resumed meanwhile never judges the same deliveries beside it (`judge_session`). It never waits for
/// it: when the session-end judge holds the lock, the stop still checks the work, but leaves the
/// pending lessons to that judge (`Work::judge`) and asks none of them (`open_in`).
struct StopLock {
    budget: Option<std::time::Duration>,
    guard: Option<live::Guard>,
}

impl StopLock {
    fn take(h: &dyn Harness, session: &str) -> StopLock {
        StopLock { budget: crate::expert_route::stop_budget(h), guard: live::acquire(&state_file(session), learn::now_ms()) }
    }
}

fn on_stop(h: &dyn Harness, session: &str, input: &Value) -> Option<Value> {
    let started = std::time::Instant::now();
    let started_ms = learn::now_ms();
    if learning() {
        // The drone's latest proposal, for the reasoning note of what it commits (notes.rs).
        crate::notes::remember_proposal(&crate::notes::state_dir(), session, &full_proposal(h, input));
        crate::notes::mark(&crate::notes::state_dir(), session, "stop");
        // What this turn added to the harness's own transcript: its corrections and answers (sweep.rs).
        crate::sweep::turn_end(h, session, input);
    }
    let second_round = input.get("stop_hook_active").and_then(Value::as_bool) == Some(true);
    if std::env::var("BORG_CRITIC").as_deref() == Ok("generic") {
        return (!second_round).then(|| json!({"decision": "block", "reason": conscience::GENERIC}));
    }
    let (all, withheld) = loaded(session);
    let judged = judged(&all, withheld);
    // R1: the drone's answer to the last block's questions, at the first turn end after it: evidence
    // for the judge at the next model call (`pending`), not a label.
    note_answer(h, session, input);
    // BORG_STOP_GATE=edits: whether this turn changed anything; every Stop consumes the marker, so
    // the rework after a block does not count for the next turn.
    let changed = std::fs::remove_file(changes_file(session)).is_ok();
    // One round of self-questioning per turn (the harness's stop_hook_active); never loop.
    if second_round {
        return None;
    }
    stat(session, input, crate::stats::TURN_END, |_| {});
    let lock = StopLock::take(h, session);
    let state = std::fs::read_to_string(state_file(session)).unwrap_or_default();
    // A lesson is asked once per session: the drone answered it at that turn end, and asking it again
    // at every later one is noise (and a model call spent on it). R3b: a lesson delivered early or
    // handed over free (`#borg-early`, `#borg-queued`) is done once the drone acted on it; one it did
    // not act on may still be asked (`done_in`).
    let proposal = full_proposal(h, input);
    let acted = early_acted(session, &judged, &proposal);
    let only = std::env::var_os("BORG_ONLY").is_some();
    let done = if only { std::collections::HashSet::new() } else { done_in(&state, &acted) };
    // What still waits is decided already: the free lessons of a check whose line no delivery took yet
    // (an earlier stop's at a harness without prompt context; `queued` hands them over). Out of this
    // stop's pool (`waiting_in`), so one check decides each.
    let queue = if only { String::new() } else { std::fs::read_to_string(queued_file(session)).unwrap_or_default() };
    let waiting: std::collections::HashSet<&str> = if only { Default::default() } else { waiting_in(&queue).collect() };
    // Without the lock, the session-end judge of a resumed session holds it and judges every open
    // delivery: a lesson it may be judging is not delivered again here, or its verdict on the earlier
    // delivery (`#borg-judged`, `END_JUDGED`) would land on this stop's question and settle it unjudged.
    let ending: std::collections::HashSet<&str> = if lock.guard.is_some() { Default::default() } else { open_in(&state) };
    // Lessons of other systems (systems.rs) are neither shortlisted nor asked.
    let challenges: Vec<conscience::Challenge> = all
        .iter()
        .filter(|c| !done.contains(c.id.as_str()) && !waiting.contains(c.id.as_str()) && !ending.contains(c.id.as_str()) && !c.off_system)
        .cloned()
        .collect();
    let message = input.get("last_assistant_message").and_then(Value::as_str).unwrap_or_default();
    let context = format!("{state}{message}");
    // Scopes at Stop: where the session works plus this turn's own message, not the whole history.
    let scope_text = format!("{}\n{message}", where_now(session));
    let check_id = crate::select::check_id(session, "stop", started_ms, std::process::id());
    // Folds the log's new tail and saves it, so the next check reads only what came after.
    let summary = crate::select::summary::refresh(&lessons_path());
    let work = Work {
        all: &all,
        judged: &judged,
        challenges: &challenges,
        done: &done,
        state: &state,
        context: &context,
        scope_text: &scope_text,
        message: &proposal,
        changed,
        judge: lock.guard.is_some(),
        budget: lock.budget,
        started,
        check: &check_id,
        summary: &summary,
    };
    let out = match stop_select(h, session, input, &work) {
        Ok(selected) => stop_ask(h, session, input, selected, &check_id),
        Err(out) => out,
    };
    drop(lock);
    out
}

/// The lessons this stop asks; `Err` with the hook's answer when it asks none (a gate skip still lets
/// the queen ask).
fn stop_select<'a>(h: &dyn Harness, session: &str, input: &Value, w: &Work<'a>) -> Result<Vec<&'a conscience::Challenge>, Option<Value>> {
    match std::env::var("BORG_ONLY") {
        // BORG_ONLY: eval oracle, perfect retrieval = exactly these challenge ids.
        Ok(ids) => Ok(w.challenges.iter().filter(|c| ids.split(',').any(|i| i == c.id)).collect()),
        // Word matching alone always finds *some* lesson, so it asked at every turn end; the
        // consequence model may abstain, and it is the mechanism the sealed eval measured.
        Err(_) if std::env::var("BORG_MATCH").as_deref() != Ok("bm25") => stop_check(h, session, input, w),
        // R3b (conscience.rs `budget`): block only for a lesson near the best match not yet done;
        // then ask everything over the low bar, in-scope first.
        Err(_) => {
            let pool = conscience::budget_pool(&conscience::rank_scoped(w.all, &conscience::work_text(w.context), w.scope_text), w.done);
            let asked: Vec<&conscience::Challenge> = conscience::in_scope_first(conscience::explored(pool.iter().copied()))
                .into_iter()
                .filter_map(|c| w.challenges.iter().find(|x| x.id == c.id))
                .collect();
            append_p_of(session, &pool, &asked, w.check);
            Ok(asked)
        }
    }
}

/// The consequence model's verdict at the stop.
fn stop_check<'a>(h: &dyn Harness, session: &str, input: &Value, w: &Work<'a>) -> Result<Vec<&'a conscience::Challenge>, Option<Value>> {
    match check(session, input, w) {
        Verdict::Skip(why) => {
            append(session, &format!("#borg-skip {why}"));
            stat(session, input, crate::stats::GATE_SKIP, |e| e.why.clone_from(&why));
            let prompts = std::fs::read_to_string(prompts_file(session)).unwrap_or_default();
            Err(crate::queen::stop(session, input, &|| harness::plan(h.caps(), input), &prompts, &[]))
        }
        Verdict::Failed => Err(None),
        Verdict::Ask(ids) => Ok(ids.iter().filter_map(|i| w.challenges.iter().find(|c| &c.id == i)).collect()),
    }
}

/// `#borg-graded n=<applied> <id>:<grade>:<rank>,…`: a graded check's applies in the model's order, so
/// the `asked` outcome can carry each asked lesson's grade and rank (`graded_of`).
fn graded_line(applied: &[(&conscience::Challenge, u8)]) -> String {
    let items: Vec<String> = applied.iter().enumerate().map(|(r, (c, g))| format!("{}:{g}:{r}", c.id)).collect();
    format!("#borg-graded n={} {}", applied.len(), items.join(","))
}

/// `#borg-p <id>=<p>,… @<check>`: the propensity (outcome.rs `p`) of each lesson a check chose to ask,
/// so every entry is above 0 and the latest entry per id is the check that asked it; the `asked`
/// outcome carries them (`tagged_p`, `propensity_in`). A graded check's `#borg-pf` line is the same for
/// what it hands over free. `check` is the check's id (select.rs `check_id`); none on an older
/// binary's line. Nothing when no lesson was chosen.
fn p_line<'a>(prefix: &str, chosen: impl IntoIterator<Item = (&'a str, f64)>, check: Option<&str>) -> Option<String> {
    let items: Vec<String> = chosen.into_iter().map(|(id, p)| format!("{id}={}", outcome::round_p(p))).collect();
    (!items.is_empty()).then(|| format!("{prefix} {}{}", items.join(","), tag(check.unwrap_or_default())))
}

/// `p_line` for lessons `budget`/`widen` asked from `pool`: their chance of a slot under the
/// session's posterior (conscience.rs `propensity`). Then the pool itself, `#borg-pool
/// <id>=<base>[:s],… @<check>` in ranking order (`:s` a scoped lesson, base conscience.rs `pool_base`,
/// unrounded), so the `asked` record can carry it (`pool_in`) and `borg debug ope` can replay the draw. Both
/// lines carry the check that drew (its id, one draw per check), which ties the p of an asked lesson
/// to the pool its draw was over.
fn append_p_of(session: &str, pool: &[(f64, &conscience::Challenge)], asked: &[&conscience::Challenge], check: &str) {
    if asked.is_empty() {
        return;
    }
    let p = conscience::propensity(pool, session);
    let chosen: Vec<(&str, f64)> = asked.iter().filter_map(|c| p.get(&c.id).map(|p| (c.id.as_str(), *p))).collect();
    let items: Vec<String> =
        pool.iter().map(|(s, c)| format!("{}={}{}", c.id, conscience::pool_base(*s, c), if c.scope.is_empty() { "" } else { ":s" })).collect();
    let pool = format!("#borg-pool {}{}", items.join(","), tag(check));
    append(session, &p_line("#borg-p", chosen, Some(check)).map_or(pool.clone(), |p| format!("{p}\n{pool}")));
}

/// Splits `line` into its items and its ` @<check>` tag (None without one).
fn tagged(line: &str) -> (&str, Option<&str>) {
    match line.rsplit_once(" @") {
        Some((items, check)) if !check.is_empty() && check.bytes().all(|b| b.is_ascii_hexdigit()) => (items, Some(check)),
        _ => (line, None),
    }
}

/// The pool behind `ids` (`append_p_of`): the `#borg-pool` line of the check that wrote the latest
/// `#borg-p` entry of every one of them, as (ids, bases, scoped); empty when their latest entries come
/// from different checks, from a check without a pool, or from a line before checks were tagged (never
/// a guess: the p of one draw is never paired with another draw's pool).
fn pool_in(state: &str, ids: &[String]) -> (Vec<String>, Vec<f64>, Vec<bool>) {
    let parse = |line: &str| -> Option<(Vec<String>, Vec<f64>, Vec<bool>)> {
        let mut out = (Vec::new(), Vec::new(), Vec::new());
        for item in line.split(',') {
            let (id, rest) = item.rsplit_once('=')?;
            let (base, scoped) = match rest.strip_suffix(":s") {
                Some(b) => (b, true),
                None => (rest, false),
            };
            out.0.push(id.trim().to_string());
            out.1.push(base.trim().parse::<f64>().ok()?);
            out.2.push(scoped);
        }
        Some(out)
    };
    // The check of each id's latest p entry.
    let mut check: std::collections::HashMap<&str, Option<&str>> = std::collections::HashMap::new();
    for line in state.lines().filter_map(|l| l.strip_prefix("#borg-p ")) {
        let (items, tag) = tagged(line);
        for (id, _) in items.split(',').filter_map(|it| it.rsplit_once('=')) {
            check.insert(id.trim(), tag);
        }
    }
    let Some(Some(tag)) = ids.first().and_then(|i| check.get(i.as_str())).copied() else { return Default::default() };
    if !ids.iter().all(|i| check.get(i.as_str()).copied().flatten() == Some(tag)) {
        return Default::default();
    }
    state
        .lines()
        .rev()
        .filter_map(|l| l.strip_prefix("#borg-pool "))
        .map(tagged)
        .filter(|(_, t)| *t == Some(tag))
        .find_map(|(items, _)| parse(items).filter(|(pool, _, _)| ids.iter().all(|i| pool.contains(i))))
        .unwrap_or_default()
}

/// For each of `ids`, its propensity in the state file's latest `#borg-p` entry for it, whatever check
/// wrote it (a line of an older binary has no tag); empty when any of them has none (unknown, never a
/// guess).
fn propensity_in(state: &str, ids: &[String]) -> Vec<f64> {
    let mut seen: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    for line in state.lines().filter_map(|l| l.strip_prefix("#borg-p ")) {
        for (id, p) in tagged(line).0.split(',').filter_map(|it| it.rsplit_once('=')) {
            if let Ok(p) = p.trim().parse::<f64>() {
                seen.insert(id.trim(), p);
            }
        }
    }
    ids.iter().map(|i| seen.get(i.as_str()).copied()).collect::<Option<Vec<f64>>>().unwrap_or_default()
}

/// For each of `ids`: its grade and rank in the session's latest graded check that graded it, and that
/// check's number of applies (the largest when the ids come from several checks); (0, 0) and 0 when none.
fn graded_of(session: &str, ids: &[String]) -> (Vec<u8>, Vec<u16>, u16) {
    let state = std::fs::read_to_string(state_file(session)).unwrap_or_default();
    let mut seen: std::collections::HashMap<String, (u8, u16, u16)> = std::collections::HashMap::new();
    for line in state.lines().filter_map(|l| l.strip_prefix("#borg-graded n=")) {
        let Some((n, items)) = line.split_once(' ') else { continue };
        let n: u16 = n.parse().unwrap_or(0);
        for it in items.split(',') {
            let mut p = it.rsplitn(3, ':');
            let (Some(r), Some(g), Some(id)) = (p.next(), p.next(), p.next()) else { continue };
            seen.insert(id.to_string(), (g.parse().unwrap_or(0), r.parse().unwrap_or(0), n));
        }
    }
    let got: Vec<(u8, u16, u16)> = ids.iter().map(|i| seen.get(i).copied().unwrap_or((0, 0, 0))).collect();
    (got.iter().map(|g| g.0).collect(), got.iter().map(|g| g.1).collect(), got.iter().map(|g| g.2).max().unwrap_or(0))
}

/// Outcome tracking (outcome.rs) of `lessons` asked on `channel` (stop; live, a check's ask from before
/// 0.7.6), routed by `check`: `borg learn` later sees whether a correction followed. The `asked` record
/// names the check and carries its p and pool, so a verdict and the settle land on the check that decided
/// the lesson; then its `delivery` record. Nothing with learning off or without lessons.
fn record_asked(session: &str, input: &Value, lessons: &[&conscience::Challenge], channel: &str, check: &str) {
    if !learning() || lessons.is_empty() {
        return;
    }
    let state = std::fs::read_to_string(state_file(session)).unwrap_or_default();
    let ids: Vec<String> = lessons.iter().map(|c| c.id.clone()).collect();
    let members: Vec<&str> = ids.iter().map(String::as_str).collect();
    let (grades, ranks, applied) = graded_of(session, &ids);
    let p = if check.is_empty() { propensity_in(&state, &ids) } else { tagged_p(&state, "#borg-p ", check, &members) };
    let (pool, base, scoped) = pool_in(&state, &ids);
    let asked = outcome::Outcome {
        kind: outcome::ASKED.into(),
        session: session.into(),
        lessons: ids.clone(),
        ms: learn::now_ms(),
        repo: repo_of(input.get("cwd").and_then(Value::as_str).unwrap_or_default()),
        // The asked lessons' disagreements (outcome.rs `contradicted` reads them across the session's records).
        groups: conscience::disagreement_ids(lessons),
        channel: channel.into(),
        grades,
        ranks,
        applied,
        p,
        pool,
        base,
        scoped,
        check: check.into(),
        ..Default::default()
    };
    let _ = outcome::append(&learn::home().join("outcomes.jsonl"), &asked);
    record_route(session, input, check, ids, channel, outcome::ROUTE_ASKED, "");
}

/// The stop asks `selected`, routed by its `check`: one block (the queen's, when it asks too),
/// `#borg-asked <ids> @<check>`, its `asked` record and its `delivery` record.
fn stop_ask(h: &dyn Harness, session: &str, input: &Value, mut lessons: Vec<&conscience::Challenge>, check: &str) -> Option<Value> {
    // Shown in an order stable within the session but different across sessions: no lesson always
    // sits first (position bias; memory.rs `shuffle`).
    crate::memory::shuffle(&mut lessons, session);
    let log = lessons.iter().map(|c| c.id.as_str()).collect::<Vec<_>>().join(",");
    // An empty `#borg-asked` still ends the turn (prefilter.rs `turn`): it carries no tag.
    let marker = if lessons.is_empty() { "#borg-asked ".to_string() } else { format!("#borg-asked {log}{}", tag(check)) };
    append(session, &marker);
    if !lessons.is_empty()
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(edits_file(session))
    {
        // The blocked message is the work before the questions (R1 `pending`).
        let _ = writeln!(f, "{}\n{marker}", one_line(&full_proposal(h, input), EVIDENCE_CHARS));
    }
    // L4 (queen.rs): a rare, budgeted question to the engineer when the conscience had nothing
    // or its lessons contradict each other (its block then carries the lessons too).
    let prompts = std::fs::read_to_string(prompts_file(session)).unwrap_or_default();
    let queen = crate::queen::stop(session, input, &|| harness::plan(h.caps(), input), &prompts, &lessons);
    if lessons.is_empty() {
        return queen;
    }
    record_asked(session, input, &lessons, "stop", check);
    let out = queen.or_else(|| Some(json!({"decision": "block", "reason": conscience::render(&lessons)})));
    let chars = out.as_ref().and_then(|o| o["reason"].as_str()).map_or(0, str::len);
    log_cost(session, &format!("block questions={} chars={chars}", lessons.len()));
    stat(session, input, crate::stats::BLOCK, |e| {
        e.n = lessons.len() as u64;
        e.lessons = lessons.iter().map(|c| c.id.clone()).collect();
        e.channel = "stop".into();
    });
    out
}

/// `borg hook <harness> <event>`, the event on stdin: routes the repo to its collective (an off repo
/// leaves no trace), records presence, then answers through steering, the experts and [`dispatch`],
/// with the messages from other sessions (msg.rs).
/// An unknown harness, like any failure here, lets the drone continue untouched.
pub fn main(name: &str, event: &str) {
    let Some(h) = harness::by_name(name) else { return };
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let mut input = h.normalise(serde_json::from_str(&raw).unwrap_or(Value::Null));
    // A pid only from a harness that sends its own: presence reads a gone one as an ended drone.
    if !h.caps().sends_pid
        && let Some(m) = input.as_object_mut()
    {
        m.shift_remove("pid");
    }
    // Which mind is this repo part of? Decided before anything is read or written: an off repo
    // (or a broken config) leaves no trace at all.
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
    let cfg = collective::load();
    let route = match &cfg {
        Ok(cfg) => collective::route(cfg.as_ref(), cwd, &learn::home()),
        Err(_) => collective::Route::Off,
    };
    let collective::Route::Collective { home, name, .. } = route else { return };
    // SAFETY: single-threaded at this point; everything below (and the background `borg learn` it
    // spawns) inherits the collective's own store and scratch state.
    unsafe {
        std::env::set_var("BORG_HOME", &home);
        // borg's own model calls go through the drone's harness (runner.rs); `borg learn` reads it too.
        std::env::set_var("BORG_HARNESS", h.name());
        if std::env::var_os("BORG_STATE").is_none() {
            std::env::set_var("BORG_STATE", home.join("state"));
        }
    }
    // Who is working where (presence.rs, for `borg top`): one small atomic write per hook.
    let now = learn::now_ms();
    let session = input.get("session_id").and_then(Value::as_str).unwrap_or("unknown");
    // The turn of a session that listens for wakes (listen.rs): one stat of its opt-in for any other, at
    // a prompt, a tool call's start or end, a halted turn and the end. With steering's stat below, an idle
    // tool call costs two failed stats: the opt-in is per session, the admissions are per store.
    crate::listen::touch(&home, event, session, now);
    // A session's start (a hook of an older install) and a halted turn read and write nothing more.
    if matches!(event, "start" | "halted") {
        return;
    }
    // Not at pre_tool (every tool call): there only steering reads, once someone steered (steer.rs
    // `active`, the second stat).
    let me = match event {
        "pre_tool" => {
            if !crate::steer::active(&home) {
                return;
            }
            crate::presence::read(&home, session)
        }
        _ => crate::presence::touch(&home, &name, h.name(), event, &input, now),
    };
    // Steering (steer.rs) wraps the answer: a pause refuses tools and lets the stop wait.
    // At a stop, the routed collective's experts join the conscience's block (expert_route.rs).
    let started = me.map(|p| p.started);
    // A listening session's stop holds its wakes off until its answer is out (listen.rs).
    let stopping = crate::listen::stopping(&home, event, session, now);
    let out = crate::steer::around(&home, event, &input, started, now, || crate::expert_route::checkpoint(h, event, &input, || dispatch(h, event, &input)));
    // A listening session's stop starts its waiter, or asks for it (listen.rs).
    let out = crate::listen::after_stop(&home, h, stopping.as_ref(), &input, out);
    // An adapter that asked learns its wait budgets from the answer (harness.rs `Budgets`).
    let out = harness::with_budgets(h, &input, out);
    // Messages from other sessions (msg.rs) join a prompt's or tool call's context, each once.
    if let Some(out) = crate::msg::deliver(&home, h.caps(), event, &input, started, now, out) {
        println!("{}", for_harness(h, out));
    }
}

/// The answer as `h` reads it: `"shown_ids"` (which messages it showed) is for an adapter, which drops a
/// held wake (listen.rs) once every message it names was shown; a harness that parses the answer itself
/// (Claude Code, Codex) gets only the keys it defines.
fn for_harness(h: &dyn Harness, mut out: Value) -> Value {
    if matches!(h.wiring(), harness::Wiring::CommandHooks { .. })
        && let Some(o) = out.as_object_mut()
    {
        o.shift_remove("shown_ids");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_of_an_edit_survives_the_cut_of_its_text() {
        let long = "x".repeat(700);
        let input = json!({"file_path": "/w/a.rs", "new_string": long, "edits": [{"file_path": "/w/b.rs", "new_string": "more"}]});
        let text = edit_text("Edit", &input).unwrap();
        assert!(text.starts_with("/w/a.rs /w/b.rs x"), "{}", &text[..40]);
        assert_eq!(text.chars().count(), 600);
        let multi = json!({"file_path": "/w/a.rs", "edits": [{"old_string": "a", "new_string": "b"}, {"old_string": "c", "new_string": "d"}]});
        assert_eq!(edit_text("MultiEdit", &multi).as_deref(), Some("/w/a.rs b d"), "Claude's MultiEdit reads as before");
        assert_eq!(touched(&input, "Edit"), "/w/a.rs /w/b.rs");
    }

    #[test]
    fn propensity_in_takes_the_latest_entry_and_nothing_when_one_is_missing() {
        let state = "#borg-p A=1,B=0.25\n#borg-asked A,B\n#borg-p B=0.5,C=0.1\n";
        let ids = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(propensity_in(state, &ids(&["B", "A"])), [0.5, 1.0]);
        assert_eq!(propensity_in(state, &ids(&["C"])), [0.1]);
        assert!(propensity_in(state, &ids(&["A", "D"])).is_empty(), "D unknown: no guess");
        assert!(propensity_in("", &ids(&["A"])).is_empty());
    }

    #[test]
    fn pool_in_takes_the_pool_of_the_check_that_logged_each_p() {
        let state = "#borg-p A=0.6,B=0.4 @c1\n#borg-pool A=1.5:s,B=0.25,C=2 @c1\n#borg-asked A,B\n#borg-p B=0.5 @c2\n#borg-pool B=0.5,D=0.125:s @c2\n";
        let ids = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(pool_in(state, &ids(&["B"])), (ids(&["B", "D"]), vec![0.5, 0.125], vec![false, true]), "B's latest p is check 2's");
        assert_eq!(pool_in(state, &ids(&["A"])), (ids(&["A", "B", "C"]), vec![1.5, 0.25, 2.0], vec![true, false, false]));
        assert_eq!(pool_in(state, &ids(&["A", "B"])), Default::default(), "A's p from check 1, B's from check 2: no pairing");
        assert_eq!(pool_in("", &ids(&["A"])), Default::default());
        assert_eq!(pool_in(state, &[]), Default::default());
        // A graded check's p has no pool: a later pool that happens to hold the lesson is not its.
        let graded = "#borg-p A=1\n#borg-p B=0.5 @c3\n#borg-pool A=1,B=1 @c3\n";
        assert_eq!(pool_in(graded, &ids(&["A"])), Default::default());
        assert_eq!(pool_in(graded, &ids(&["B"])).0, ids(&["A", "B"]));
        // A state file from before checks were tagged: no pool.
        assert_eq!(pool_in("#borg-p A=1\n#borg-pool A=1,B=2\n", &ids(&["A"])), Default::default());
    }

    #[test]
    fn a_queue_line_names_its_route_its_ids_and_its_check() {
        let c = "0123456789ab";
        assert_eq!(queue_line(&format!("A,B @{c}")), (Queued::Free, vec!["A", "B"], c));
        assert_eq!(queue_line(&format!("placebo X,Y @{c}")), (Queued::Placebo, vec!["X", "Y"], c));
        assert_eq!(queue_line(&format!("instead G0,G1 @{c}")), (Queued::Instead, vec!["G0", "G1"], c));
        assert_eq!(queue_line(&format!("instead-next G2 @{c}")), (Queued::Instead, vec!["G2"], c));
        // An older binary's lines: no check.
        assert_eq!(queue_line("A,B"), (Queued::Free, vec!["A", "B"], ""));
        // A live check's lines, written before 0.7.6: handed over in place of a question.
        assert_eq!(queue_line(&format!("live A @{c}")), (Queued::Ask, vec!["A"], c));
        assert_eq!(queue_line("live A"), (Queued::Ask, vec!["A"], ""));
        assert_eq!(queue_line(&format!("live-next A,B @{c}")), (Queued::Ask, vec!["A", "B"], c));
        assert_eq!(listed(&format!("#borg-asked A,B @{c}\n#borg-asked C\n"), "#borg-asked "), ["A", "B", "C"].into_iter().collect());
    }

    #[test]
    fn a_tagged_propensity_is_its_checks_never_another_checks() {
        let state = "#borg-pf A=1,B=0.5 @c1\n#borg-pf A=0.25 @c2\n#borg-p A=0.75 @c1\n#borg-pf B=0.9\n";
        assert_eq!(tagged_p(state, "#borg-pf ", "c1", &["A", "B"]), [1.0, 0.5]);
        assert_eq!(tagged_p(state, "#borg-pf ", "c2", &["A"]), [0.25]);
        assert!(tagged_p(state, "#borg-pf ", "c2", &["A", "B"]).is_empty(), "B has none at c2: no guess");
        assert_eq!(tagged_p(state, "#borg-p ", "c1", &["A"]), [0.75], "#borg-p and #borg-pf are separate lines");
        assert_eq!(p_line("#borg-pf", [("A", 1.0), ("B", 0.33333)], Some("c1")).as_deref(), Some("#borg-pf A=1,B=0.3333 @c1"));
        assert_eq!(p_line("#borg-p", [], Some("c1")), None);
    }

    #[test]
    fn unjudged_says_why_each_delivered_lesson_has_no_verdict() {
        let c = "0123456789ab";
        let state = format!(
            "#borg-asked A,B,C,D @{c}\n#borg-judged A=pending\n#borg-unjudged B why=slots\n#borg-judged A=pending\n#borg-unjudged D why=failed\n\
             #borg-queued E @{c}\n#borg-judged B=pending\n#borg-asked D\n"
        );
        let why = unjudged(&state);
        assert_eq!(why["A"], outcome::UNJUDGED_TRIES, "two unclear tries");
        assert_eq!(why["B"], outcome::UNJUDGED_SLOTS, "one try is not all of them; the last call left it out");
        assert_eq!(why["C"], outcome::UNJUDGED_NO_CALL);
        assert_eq!(why["D"], outcome::UNJUDGED_NO_CALL, "delivered again after its failed call");
        assert_eq!(why["E"], outcome::UNJUDGED_NO_CALL);
    }

    #[test]
    fn the_declaration_coin_is_fixed_per_check_and_lesson_and_fair() {
        assert_eq!(stripped("0123456789ab", "L1"), stripped("0123456789ab", "L1"));
        let heads = (0..1000).filter(|i| stripped("0123456789ab", &format!("L{i}"))).count();
        assert!((400..600).contains(&heads), "{heads} of 1000");
        let checks = (0..1000).filter(|i| stripped(&format!("{i:012x}"), "L1")).count();
        assert!((400..600).contains(&checks), "{checks} of 1000");
    }

    /// Judge v2's evidence: a free delivery and a placebo get the work so far before them and the work
    /// since after, as a question does; each lesson carries the check that routed it (a stop's block from
    /// before 0.7.6 may carry a live check's waiting ask); a stripped declaration is gone from the shared
    /// evidence.
    #[test]
    fn pending_gives_every_route_the_same_evidence_and_its_own_check() {
        let session = format!("pending-v2-{}", std::process::id());
        let c = |id: &str| conscience::Challenge { id: id.into(), ..Default::default() };
        let all = [c("A"), c("B"), c("C"), c("D"), c("E")];
        let edits = "Plan: the blocked message\n#borg-asked A,B @0000000000c1\n#borg-routed B=0000000000c2\n#borg-answer A: applies. B: skip.\n\
                     edit one\n#borg-work the work so far\n#borg-queued C @0000000000c3\n#borg-placebo D @0000000000c4\nedit two\n\
                     #borg-work later work\n#borg-asked E @0000000000c5\n";
        let _ = std::fs::write(edits_file(&session), edits);
        let state: String = edits.lines().filter(|l| DELIVERED.iter().any(|m| l.starts_with(m))).map(|l| format!("{l}\n")).collect();
        let j = pending(&session, &state, &all, "the final message", When::Stop, "00000000cafe");
        let got: Vec<(&str, &str, &str)> = j.pending.iter().map(|p| (p.lesson.id.as_str(), p.channel, p.check.as_str())).collect();
        assert_eq!(
            got,
            [
                ("A", "stop", "0000000000c1"),
                ("B", "stop", "0000000000c2"),
                ("C", "queued", "0000000000c3"),
                ("D", outcome::PLACEBO, "0000000000c4"),
                ("E", "stop", "0000000000c5")
            ]
        );
        let p = |id: &str| j.pending.iter().find(|p| p.lesson.id == id).unwrap();
        assert_eq!(
            (p("A").before.as_str(), p("C").before.as_str(), p("D").before.as_str()),
            ("Plan: the blocked message", "the work so far", "the work so far")
        );
        assert_eq!(p("C").after, "work since: edit two\nthis turn: the final message");
        assert_eq!(p("D").after, p("C").after, "a placebo is judged as a free delivery");
        assert!(p("A").after.starts_with("work since: ") && p("A").after.contains("edit one | edit two"), "{}", p("A").after);
        assert_eq!(p("A").after, p("B").after, "delivered together, one shared evidence");
        for (id, word) in [("A", "applies"), ("B", "skip")] {
            let x = p(id);
            assert_eq!(x.stripped, stripped(&x.check, id));
            match x.stripped {
                true => assert!(x.declared.is_none() && !x.after.contains(&format!("{id}: {word}")), "{x:?}"),
                false => assert!(x.declared.is_some() && x.after.contains(&format!("{id}: {word}")), "{x:?}"),
            }
        }
        assert!(!p("C").stripped && !p("D").stripped, "only questions are stripped");
        assert!(j.question && j.crowded.is_empty());
        let _ = std::fs::remove_file(edits_file(&session));
    }

    /// History (live mode, before 0.7.6): a lesson handed over free at check A, then asked live at check B,
    /// in a session that runs across the upgrade. The live question still reads as channel live and is
    /// the lesson's latest delivery: the lesson is judged once, on the question, answered or not.
    #[test]
    fn a_live_question_from_before_the_upgrade_is_still_its_lessons_latest_delivery() {
        let session = format!("pending-later-{}", std::process::id());
        let c = |id: &str| conscience::Challenge { id: id.into(), ..Default::default() };
        let all = [c("L"), c("M")];
        let (a, b) = ("00000000000a", "00000000000b");
        let asked = format!("#borg-work so far\n#borg-queued L,M @{a}\nedit one\n#borg-work more\n#borg-asked L @{b}\n#borg-live L ms=1\nedit two\n");
        let state_of = |edits: &str| edits.lines().filter(|l| DELIVERED.iter().any(|m| l.starts_with(m))).map(|l| format!("{l}\n")).collect::<String>();
        let answered = format!("{asked}#borg-answer L: applies\n");
        for edits in [&asked, &answered] {
            let _ = std::fs::write(edits_file(&session), edits);
            for when in [When::Stop, When::End] {
                let j = pending(&session, &state_of(edits), &all, "", when, "00000000cafe");
                let got: Vec<(String, &str, String)> = j.pending.iter().map(|p| (p.lesson.id.clone(), p.channel, p.check.clone())).collect();
                assert_eq!(got, [("M".to_string(), "queued", a.to_string()), ("L".to_string(), "live", b.to_string())], "{when:?}: {edits}");
            }
        }
        let _ = std::fs::remove_file(edits_file(&session));
    }

    /// Judge timing (0.7.5): a stop's judge call judges a free delivery, an early one and a placebo, work
    /// after it or not, and so does the session-end judge.
    #[test]
    fn a_free_delivery_or_placebo_is_judged_at_the_turn_end() {
        let session = format!("pending-work-{}", std::process::id());
        let c = |id: &str| conscience::Challenge { id: id.into(), ..Default::default() };
        let all = [c("A"), c("C"), c("D"), c("E")];
        let state_of = |edits: &str| edits.lines().filter(|l| DELIVERED.iter().any(|m| l.starts_with(m))).map(|l| format!("{l}\n")).collect::<String>();
        let ids = |j: &Judging| j.pending.iter().map(|p| p.lesson.id.clone()).collect::<Vec<_>>();
        let fresh = "Plan: blocked\n#borg-asked A @0000000000c1\n#borg-answer A: applies\n#borg-work so far\n#borg-queued C @0000000000c3\n#borg-placebo D @0000000000c4\n";
        for edits in [fresh.to_string(), format!("{fresh}edit one\n"), format!("{fresh}edit one\n#borg-work later\n#borg-early E @0000000000c5\nedit two\n")] {
            let _ = std::fs::write(edits_file(&session), &edits);
            let state = state_of(&edits);
            let early = edits.contains("#borg-early ");
            for when in [When::Stop, When::End] {
                let want: &[&str] = if early { &["A", "C", "D", "E"] } else { &["A", "C", "D"] };
                assert_eq!(ids(&pending(&session, &state, &all, "", when, "00000000cafe")), want, "{when:?}: {edits}");
            }
        }
        let _ = std::fs::remove_file(edits_file(&session));
    }

    /// The session-end judge (`When::End`): every delivery no verdict settled, a question not answered
    /// and a lesson whose check tries ran out included, never one a verdict settled; what it was shown
    /// (`END_JUDGED`) is settled for every later call, until the lesson is delivered again, and settles
    /// at `borg learn` as having used its tries.
    #[test]
    fn the_session_end_judge_never_outlives_its_lock() {
        let (end, stale) = (end_timeout(), live::stale_ms());
        assert!(i64::try_from(end.as_millis()).unwrap() < stale, "{end:?} against a lock stale after {stale} ms");
        assert!(end <= learn::learn_timeout());
    }

    #[test]
    fn the_session_end_judge_takes_every_open_delivery_once() {
        let session = format!("pending-end-{}", std::process::id());
        let c = |id: &str| conscience::Challenge { id: id.into(), ..Default::default() };
        let all = [c("A"), c("B"), c("C"), c("D"), c("Q")];
        let ids = |j: &Judging| j.pending.iter().map(|p| p.lesson.id.clone()).collect::<Vec<_>>();
        let edits = "Plan\n#borg-asked A,B @0000000000c1\n#borg-answer A: applies\nedit\n#borg-work w\n#borg-queued C,D @0000000000c3\nedit two\nPlan two\n#borg-asked Q @0000000000c4\n";
        let _ = std::fs::write(edits_file(&session), edits);
        let delivered: String = edits.lines().filter(|l| DELIVERED.iter().any(|m| l.starts_with(m))).map(|l| format!("{l}\n")).collect();
        // A changed, B unclear twice (tries spent), C unclear once, D not judged, Q never answered.
        let state = format!("{delivered}#borg-judged A=changed\n#borg-judged B=pending\n#borg-judged C=pending\n#borg-judged B=pending\n");
        assert_eq!(ids(&pending(&session, &state, &all, "", When::Stop, "00000000cafe")), ["C", "D", "Q"], "a check: B's tries are spent");
        assert_eq!(ids(&pending(&session, &state, &all, "", When::End, "00000000cafe")), ["B", "C", "D", "Q"], "the end: every open one");
        let ended = format!("{state}#borg-judged B=pending\n#borg-judged C=changed\n{END_JUDGED}B,C,D,Q\n");
        for when in [When::Stop, When::End] {
            assert!(pending(&session, &ended, &all, "", when, "00000000cafe").pending.is_empty(), "{when:?}: judged at the end once");
        }
        let why = unjudged(&ended);
        assert_eq!((why["B"], why["D"], why["Q"]), (outcome::UNJUDGED_TRIES, outcome::UNJUDGED_TRIES, outcome::UNJUDGED_TRIES));
        // Delivered again after the end (a resumed session): open again.
        let again = format!("{ended}#borg-queued D @0000000000c5\n");
        let _ = std::fs::write(edits_file(&session), format!("{edits}#borg-queued D @0000000000c5\n"));
        assert_eq!(ids(&pending(&session, &again, &all, "", When::End, "00000000cafe")), ["D"]);
        assert_eq!(unjudged(&again)["D"], outcome::UNJUDGED_NO_CALL);
        let _ = std::fs::remove_file(edits_file(&session));
    }

    /// A source of the session's work by its own text, `marks` where each delivery stood.
    fn source(label: &'static str, text: &str, marks: &[(usize, &str)]) -> Source {
        Source { label, text: text.chars().collect(), marks: marks.iter().map(|(at, id)| (*at, vec![id.to_string()])).collect() }
    }

    /// The session's work for the session-end judge: each source labelled, whole when all fit; else the
    /// budget, framing included, shared so a short source leaves the rest to the longer ones, each cut
    /// its newest characters (`newest`, and wherever a source has no delivery in it).
    #[test]
    fn the_session_work_shares_its_budget_and_is_read_whole_when_it_fits() {
        let parts = [source("short", "abc", &[]), source("long", "0123456789", &[]), source("none", "", &[])];
        let none = [None, None, None];
        let whole = "### short\nabc\n### long\n0123456789\n";
        assert_eq!(within(&parts, &none, 100, Cut::After), (whole.to_string(), false));
        assert_eq!(within(&parts, &none, whole.chars().count(), Cut::After), (whole.to_string(), false), "exactly fits: whole");
        // Framing: `### short\n` + `\n` (11) and `### long\n` + `\n` (10), two marks each: 25 of 33 leave 8.
        assert_eq!(within(&parts, &none, 33, Cut::Newest), ("### short\nabc\n### long\n…56789\n".to_string(), true), "short keeps its 3, long the other 5");
        assert_eq!(within(&parts, &none, 29, Cut::After), ("### short\n…bc\n### long\n…89\n".to_string(), true), "two each when neither fits its share");
        for budget in 0..60 {
            for cut in [Cut::Newest, Cut::After, Cut::Ends] {
                let (text, _) = within(&parts, &[None, Some(3), None], budget, cut);
                assert!(text.chars().count() <= budget, "{budget} {cut:?}: {text:?}");
            }
        }
    }

    /// A long session's work cut for a batch: `newest` its end; `after` from the batch's earliest
    /// delivery on, its start first; `ends` half from there, half the end; a source without the batch's
    /// delivery in it, and every cut when what follows the delivery fits, the newest.
    #[test]
    fn a_long_sessions_work_is_cut_at_the_batchs_earliest_delivery() {
        let text: Vec<char> = "0123456789abcdefghij".chars().collect();
        assert_eq!(Cut::Newest.window(&text, 6, Some(4)), "…efghij");
        assert_eq!(Cut::After.window(&text, 6, Some(4)), "…456789…", "from the delivery on, its start first");
        assert_eq!(Cut::After.window(&text, 6, Some(0)), "012345…");
        assert_eq!(Cut::Ends.window(&text, 6, Some(4)), "…456…hij", "half after the delivery, half the end");
        assert_eq!(Cut::After.window(&text, 6, Some(16)), "…efghij", "what follows fits: the newest");
        assert_eq!(Cut::Ends.window(&text, 6, Some(15)), "…efghij", "the halves meet: the newest");
        for cut in [Cut::Newest, Cut::After, Cut::Ends] {
            assert_eq!(cut.window(&text, 6, None), "…efghij", "{cut:?}: no delivery in it");
        }
        // Within a batch's budget: the edits part from the batch's delivery on, the messages their end.
        let parts = [source("edits", "0123456789abcdefghij", &[(4, "L")]), source("said", "ABCDEFGHIJKLMNOPQRST", &[])];
        let (work, cut) = within(&parts, &[parts[0].anchor(&["L"]), parts[1].anchor(&["L"])], 34, Cut::After);
        assert!(cut);
        assert_eq!(work, "### edits\n…4567…\n### said\n…PQRST\n");
    }

    /// Where each delivery stood in a source: its latest marker naming the lesson (the delivery it is
    /// judged on), the earliest of a batch's; the markers and bookkeeping are no work, a stripped
    /// question's declaration is gone from it, an answer to a question is.
    #[test]
    fn a_source_marks_where_each_delivery_stood_without_stripped_declarations() {
        let c = |id: &str| conscience::Challenge { id: id.into(), ..Default::default() };
        let (a, f) = (c("A"), c("F"));
        let p = |lesson, stripped| predict::Pending {
            lesson,
            before: String::new(),
            after: String::new(),
            declared: None,
            check: String::new(),
            channel: "stop",
            stripped,
        };
        let pend = [p(&a, false), p(&f, true)];
        let edits = "one\n#borg-work w\n#borg-asked A,F @0000000000c1\n#borg-answer A: applies. F: applies.\ntwo\n#borg-queued A @0000000000c2\nthree\n";
        let s = Source::of("edits", edits, |l| l.strip_prefix("#borg-answer ").or_else(|| (!l.starts_with("#borg-")).then_some(l)), &pend);
        let text: String = s.text.iter().collect();
        assert_eq!(text, "one\nA: applies. .\ntwo\nthree");
        assert_eq!((s.anchor(&["F"]), s.anchor(&["A"]), s.anchor(&["A", "F"]), s.anchor(&["Z"])), (Some(4), Some(22), Some(4), None));
        assert!(Source::of("blank", "#borg-asked A\n \n", Some, &pend).text.is_empty(), "only whitespace: empty");
    }

    /// A session-end pass's budget is the judge's measured length (judge_length.rs), or the step above
    /// on the chance the estimate gives while that step is open: drawn per pass from its seed, the
    /// chance and the draw logged (`draw`); the same seed draws the same.
    #[test]
    fn a_pass_explores_the_step_above_on_its_logged_chance() {
        let d = std::env::temp_dir().join(format!("borg-end-budget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let draws: Vec<EndBudget> = (0..500).map(|i| end_budget(&d, "m:x", &format!("check-{i}"))).collect();
        let psi = crate::judge_length::EXPLORE_SHARE;
        for b in &draws {
            let explored = b.draw.expect("drawn, logged").1;
            assert_eq!(*b, EndBudget { chars: if explored { 108_000 } else { END_START_BUDGET }, draw: Some((psi, explored)) });
        }
        let share = draws.iter().filter(|b| b.draw.is_some_and(|d| d.1)).count() as f64 / draws.len() as f64;
        assert!((share - psi).abs() < 0.06, "explored {share} of passes at chance {psi}");
        assert_eq!(end_budget(&d, "m:x", "check-7"), draws[7], "the same seed, the same draw");
        // The step above decided as failing: explored at the floor's chance, logged.
        let line = |chars: usize, n: u64, a: u64| {
            format!("judge call=1 pending={n} resolved={a} chars={chars} verdicts=changed:{a},unclear:{} model=m:x at=end\n", n - a).repeat(10)
        };
        std::fs::write(d.join("s.cost"), line(30_000, 40, 20) + &line(100_000, 40, 5)).unwrap();
        let floor = crate::judge_length::FAILED_EXPLORE_SHARE;
        let draws: Vec<EndBudget> = (0..2000).map(|i| end_budget(&d, "m:x", &format!("check-{i}"))).collect();
        for b in &draws {
            let explored = b.draw.expect("drawn, logged").1;
            assert_eq!(*b, EndBudget { chars: if explored { 108_000 } else { END_START_BUDGET }, draw: Some((floor, explored)) });
        }
        let share = draws.iter().filter(|b| b.draw.is_some_and(|d| d.1)).count() as f64 / draws.len() as f64;
        assert!((share - floor).abs() < 0.02, "explored {share} of passes at chance {floor}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The session-end judge's batches: every open lesson in exactly one, in delivery order; each batch's
    /// lessons within half the budget (one alone may exceed it), and its whole prompt, with as much of a
    /// long session's work as is left, within the budget.
    #[test]
    fn every_lesson_lands_in_one_batch_whose_prompt_stays_within_the_budget() {
        let lessons: Vec<conscience::Challenge> = (0..278)
            .map(|i| conscience::Challenge {
                id: format!("L{i:03}"),
                question: format!("Question {i} {}", "q".repeat(i * 7 % 400)),
                consequence: format!("consequence {i}"),
                ..Default::default()
            })
            .collect();
        let pend: Vec<predict::Pending> = lessons
            .iter()
            .enumerate()
            .map(|(i, l)| predict::Pending {
                lesson: l,
                // Lessons delivered together share their evidence.
                before: format!("before {} {}", i / 3, "b".repeat(i * 13 % 1300)),
                after: format!("after {} {}", i / 3, "a".repeat(i * 31 % 4000)),
                declared: None,
                check: String::new(),
                channel: "queued",
                stripped: false,
            })
            .collect();
        let long: String = (0..4000).map(|i| format!("edit {i:04} of the long session\n")).collect();
        let marks: Vec<(usize, &str)> = lessons.iter().enumerate().map(|(i, l)| (i * 100, l.id.as_str())).collect();
        let sources = [source("prompts", &"p".repeat(30_000), &[]), source("edits", &long, &marks), source("said", "", &[])];
        for budget in [END_START_BUDGET, 20_000, 200_000] {
            let groups = batches(&pend, budget);
            assert!(groups.len() > 1, "{budget}: {groups:?}");
            assert_eq!(groups.iter().flat_map(Clone::clone).collect::<Vec<_>>(), (0..pend.len()).collect::<Vec<_>>(), "each once, in order");
            for g in &groups {
                let batch = &pend[g.clone()];
                assert!(batch.len() == 1 || predict::judge_section(batch).chars().count() <= budget / END_LESSONS_SHARE, "{budget}: {g:?}");
                let fixed = predict::judge_prompt("", batch).chars().count();
                let ids: Vec<&str> = batch.iter().map(|p| p.lesson.id.as_str()).collect();
                let anchors: Vec<Option<usize>> = sources.iter().map(|s| s.anchor(&ids)).collect();
                let (work, cut) = within(&sources, &anchors, budget.saturating_sub(fixed), Cut::After);
                assert!(cut, "a long session is cut");
                assert!(predict::judge_prompt(&work, batch).chars().count() <= budget, "{budget}: {g:?}");
                // `after`: the edits part starts at the batch's first delivery (or at the newest share).
                let first = g.start * 100;
                let body = work.split("### edits\n").nth(1).unwrap().trim_end_matches('\n');
                let body = body.strip_prefix('…').unwrap_or(body).trim_end_matches('…');
                let at = long.find(body).unwrap();
                assert!(at == first || (at <= first && at + body.len() + 1 >= long.len()), "{budget}: {g:?} at {at}");
            }
            // A lesson whose evidence alone exceeds half the budget goes alone.
            assert!(batches(&pend[..3], 100).iter().all(|g| g.len() == 1));
        }
    }

    /// The judge's cost line names the ids the answer returned and those that matched no pending lesson,
    /// after the fields `borg status` sums (prefilter.rs `tally`).
    #[test]
    fn the_judge_cost_line_names_the_returned_and_unmatched_ids() {
        fn p(lesson: &conscience::Challenge) -> predict::Pending<'_> {
            predict::Pending { lesson, before: String::new(), after: String::new(), declared: None, check: String::new(), channel: "stop", stripped: false }
        }
        let c = |id: &str| conscience::Challenge { id: id.into(), ..Default::default() };
        let (a, b) = (c("La"), c("Lb"));
        let pend = [p(&a), p(&b)];
        let returned = ["La".to_string(), "Lz".to_string()];
        let j = |id: &str, verdict, reason: &str| predict::Judged { id: id.into(), verdict, reason: reason.into() };
        let one = [j("La", predict::Verdict::NotArisen, "only docs"), j("Lz", predict::Verdict::Changed, "not pending")];
        assert_eq!(
            judge_line(3, &pend, &one, 900, &returned),
            "judge call=3 pending=2 resolved=1 chars=900 returned=La,Lz unmatched=Lz \
             verdicts=changed:0,ignored:0,not_arisen:1,consequence_remains:0,unclear:0 reason_chars=9"
        );
        let pair = [j("La", predict::Verdict::Unclear, ""), j("Lb", predict::Verdict::ConsequenceRemains, "still awaits")];
        assert_eq!(
            judge_line(4, &pend, &pair, 10, &[]),
            "judge call=4 pending=2 resolved=0 chars=10 returned= unmatched= \
             verdicts=changed:0,ignored:0,not_arisen:0,consequence_remains:1,unclear:1 reason_chars=12"
        );
    }

    /// A live question answered at the stop: the stop's own message is the answer, so a stripped
    /// question's declaration must leave "this turn" too; and when that stop blocks with a new question,
    /// its blocked message (that question's work before, and work since for the live one) as well. A
    /// question that keeps its declaration keeps it.
    #[test]
    fn a_stripped_declaration_leaves_every_evidence_of_the_call() {
        let session = format!("pending-strip-{}", std::process::id());
        let c = |id: &str| conscience::Challenge { id: id.into(), ..Default::default() };
        let all = [c("A"), c("F"), c("G")];
        let check = |lesson: &str, strip: bool| (0u64..).map(|i| format!("{i:012x}")).find(|k| stripped(k, lesson) == strip).unwrap();
        let (ca, cf, cg) = (check("A", false), check("F", true), check("G", false));
        let answer = "Fixed the arc. F: applies";
        let edits = format!(
            "Plan: the blocked message\n#borg-asked A @{ca}\n#borg-answer A: applies\nedit one\n#borg-work the work so far\n\
             #borg-asked F @{cf}\n#borg-live F ms=1\n#borg-answer {answer}\n{answer}\n#borg-asked G @{cg}\n"
        );
        let _ = std::fs::write(edits_file(&session), &edits);
        let state: String = edits.lines().filter(|l| DELIVERED.iter().any(|m| l.starts_with(m))).map(|l| format!("{l}\n")).collect();
        let j = pending(&session, &state, &all, answer, When::Stop, "00000000cafe");
        let p = |id: &str| j.pending.iter().find(|p| p.lesson.id == id).unwrap();
        assert!(p("F").stripped && p("F").declared.is_none(), "{:?}", p("F"));
        for x in &j.pending {
            assert!(!format!("{} {}", x.before, x.after).to_lowercase().contains("f: applies"), "{x:?}");
        }
        assert!(p("F").after.contains("this turn: Fixed the arc."), "the rest of the message stays: {}", p("F").after);
        assert!(!p("A").stripped && p("A").declared == Some(true) && p("A").after.contains("A: applies"), "{:?}", p("A"));
        // The work a call shows loses it too.
        assert_eq!(without_declarations(&format!("work\n{answer}"), &j.pending), "work\nFixed the arc. ");
        assert_eq!(without_declarations("A: applies", &j.pending), "A: applies");
        let _ = std::fs::remove_file(edits_file(&session));
    }

    /// Track R's merged ask: a live check drew A and B from one pool, a stop check drew A again from
    /// another, and the stop's block holds the waiting B beside its A. The block's `asked` records are
    /// one per routing check (`stop_ask`), each with its own draw's pool; ids of two draws together
    /// carry none (counted as no_pool by `borg debug ope`), never one draw's pool with both p.
    #[test]
    fn a_merged_ask_of_two_draws_carries_no_pool() {
        let session = format!("pool-merge-{}", std::process::id());
        let _ = std::fs::remove_file(state_file(&session));
        let c = |id: &str| conscience::Challenge { id: id.into(), ..Default::default() };
        let (a, b, cc, d, e, f) = (c("A"), c("B"), c("C"), c("D"), c("E"), c("F"));
        let live: Vec<(f64, &conscience::Challenge)> = vec![(1.0, &a), (0.9, &b), (0.8, &cc), (0.7, &d)];
        append_p_of(&session, &live, &[&a, &b], "c1");
        let stop: Vec<(f64, &conscience::Challenge)> = vec![(1.0, &a), (0.9, &e), (0.8, &f)];
        append_p_of(&session, &stop, &[&a], "c2");
        let state = std::fs::read_to_string(state_file(&session)).unwrap_or_default();
        let merged = vec!["A".to_string(), "B".to_string()];
        assert_eq!(propensity_in(&state, &merged).len(), 2, "each id has its latest p");
        assert_eq!(pool_in(&state, &merged), Default::default());
        assert_eq!(pool_in(&state, &["A".to_string()]).0, ["A", "E", "F"], "A alone: the stop check's pool");
        assert_eq!(pool_in(&state, &["B".to_string()]).0, ["A", "B", "C", "D"], "B alone: the live check's");
        assert_eq!(tagged_p(&state, "#borg-p ", "c1", &["B"]).len(), 1, "B's record carries its own check's p");
        let _ = std::fs::remove_file(state_file(&session));
    }

    /// An early delivery's relative scores (`early_rel`): each lesson's match as a share of the best
    /// on-topic match, which a lesson done already may hold; a standing lesson's on the same scale (0
    /// without a word in common); nothing to scale by without a plain lesson on the topic.
    #[test]
    fn an_early_lessons_relative_score_is_its_share_of_the_best_on_topic_match() {
        let c = |id: &str, q: &str| conscience::Challenge { id: id.into(), question: q.into(), scope: vec!["Orion".into()], ..Default::default() };
        let standing = conscience::Challenge { source: conscience::CONSTRAINT.into(), ..c("S", "harpsichord tuning") };
        let all = vec![c("A", "orion budget place reset arc drain"), c("B", "orion budget place"), c("C", "telescope glaciers"), standing];
        let work = "drain the orion budget place, never with a reset arc";
        let topics = vec!["Orion".to_string()];
        let ranked = conscience::rank(&all, work);
        let score = |id: &str| ranked.iter().find(|(_, x)| x.id == id).map_or(0.0, |(s, _)| *s);
        assert!(score("A") > score("B") && score("B") > 0.0, "{ranked:?}");
        // A, the best match, was delivered before (done): B's share is of A's score all the same.
        let rel = early_rel(&all, work, &topics, &[&all[1], &all[3]]);
        assert_eq!(rel, [outcome::round_p(score("B") / score("A")), 0.0]);
        assert!(rel[0] < 1.0);
        assert_eq!(early_rel(&all, work, &topics, &[&all[0]]), [1.0]);
        // Only a standing lesson on a topic no plain lesson matches: nothing to scale by.
        assert!(early_rel(&all, work, &["Beacon".to_string()], &[&all[3]]).is_empty());
    }

    /// A store for `decide`: its lessons file (empty) and outcomes beside it, and a saved selection summary
    /// whose fit is sure that handing over beats asking, with a gate open wide: a learned live check
    /// deviates on evidence, and its record moves the gate's ledger.
    fn decide_store(name: &str) -> std::path::PathBuf {
        decide_store_with(name, true)
    }

    /// `decide_store`, its fit `sure` that handing over beats asking, or (not `sure`) with no evidence
    /// either way, asking and handing over worth the same: a learned live check then keeps the fixed rule
    /// on evidence and runs the question-count test, which the open gate admits.
    fn decide_store_with(name: &str, sure: bool) -> std::path::PathBuf {
        use crate::select::{policy, summary};
        let d = std::env::temp_dir().join(format!("borg-decide-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let lessons = d.join("lessons.jsonl");
        std::fs::write(&lessons, "").unwrap();
        std::fs::write(outcome::path_beside(&lessons), "").unwrap();
        let at = |name: &str| policy::NAMES.iter().position(|n| *n == name).unwrap();
        let mut beta = vec![0.0; policy::P];
        beta[at("intercept")] = -0.5;
        if sure {
            beta[at("ask")] = -3.0;
            beta[at("not")] = -1.0;
        }
        let mut chol = vec![0.0; policy::P * policy::P];
        for k in 0..policy::P {
            chol[k * policy::P + k] = 0.05;
        }
        let model = policy::Model { beta, chol, sd: vec![1.0; policy::P], spread: [0.1; 3], ..policy::Model::default() };
        let rates = policy::Rates { free_stop: 1.0, free_live: 1.0, later: 0.0, wait: 0.0, ask_live: 1.0 };
        let fit = policy::Fit { key: "9-abcdef01".into(), model: Some(model), rates, ..policy::Fit::default() };
        let gate = summary::Gate { gain: vec![0.0; policy::DRAWS], allowance: 5.0, ..summary::Gate::default() };
        summary::save(&lessons, &summary::Summary { fit, gate, ..summary::Summary::default() }).unwrap();
        lessons
    }

    /// `record` for `decide` in a test: the hook's own graded record (`graded_outcome`), beside `lessons`;
    /// whether it landed, as `record_graded` answers.
    fn record_beside(lessons: &std::path::Path, applied: &[(&conscience::Challenge, u8)], check: &str, r: &crate::select::Routed, basis: &Basis) -> bool {
        let order: Vec<&conscience::Challenge> = applied.iter().map(|(c, _)| *c).collect();
        let call = Call { eligible: order.len(), order: &order, tail: &[], pt: 0.0, budget: 0, model: "", prompt_hash: String::new(), dur_ms: 1, usd: 0.0 };
        let o = graded_outcome("decide-s", "r", check, applied, Some((r, basis)), basis.summary, &call);
        outcome::append(&outcome::path_beside(lessons), &o).is_ok()
    }

    /// The question-count test's k and s reach the graded record from the decision's log, the s = 0
    /// draw included; a check outside the test and a failed one log neither.
    #[test]
    fn the_graded_record_carries_the_question_count_tests_draw() {
        use crate::select::{Log, Routed, policy, summary};
        let c = |id: &str| conscience::Challenge { id: id.into(), ..Default::default() };
        let ls = [c("A"), c("B")];
        let applied: Vec<(&conscience::Challenge, u8)> = ls.iter().zip([2, 1]).collect();
        let order: Vec<&conscience::Challenge> = ls.iter().collect();
        let call = Call { eligible: 2, order: &order, tail: &[], pt: 0.0, budget: 16_000, model: "", prompt_hash: String::new(), dur_ms: 1, usd: 0.0 };
        let s = summary::Summary::default();
        let basis = Basis { summary: &s, had: &[], why: "" };
        let routed = |source: &'static str, xk: Option<u32>, xs: Option<u32>| Routed {
            ask: Vec::new(),
            free: vec![&ls[0], &ls[1]],
            placebo: Vec::new(),
            p: vec![0.0, 0.0],
            pf: vec![1.0, 1.0],
            log: Log { policy: "learned", source, xk, xs, ..Log::default() },
        };
        let record = |r: Option<&Routed>| graded_outcome("s", "r", "0123456789ab", &applied, r.map(|r| (r, &basis)), &s, &call);
        let drawn = record(Some(&routed(policy::EXPERIMENT_CONTROL, Some(2), Some(0))));
        assert_eq!((drawn.source.as_str(), drawn.xk, drawn.xs), (policy::EXPERIMENT_CONTROL, Some(2), Some(0)));
        let outside = record(Some(&routed(policy::EVIDENCE, None, None)));
        assert_eq!((outside.source.as_str(), outside.xk, outside.xs), (policy::EVIDENCE, None, None));
        let failed = record(None);
        assert_eq!((failed.failed, failed.xk, failed.xs), (true, None, None));
        // The pool budget the check filled to, whatever it decided.
        assert_eq!((drawn.budget, failed.budget), (16_000, 16_000));
    }

    fn graded_records(lessons: &std::path::Path) -> Vec<outcome::Outcome> {
        outcome::read(&outcome::path_beside(lessons)).into_iter().filter(|o| o.kind == outcome::GRADED).collect()
    }

    /// A check decides on the summary it loads under the store's decision lock, never on the one it read
    /// at its start: B started before A decided, yet B's decision reads A's ledger entry and says so.
    #[test]
    fn a_check_decides_on_the_summary_under_the_lock_with_every_decision_before_it() {
        use crate::select::{Policy, policy, summary};
        let lessons = decide_store("fresh");
        // What both checks read at their start, before either decided.
        let stale = summary::refresh(&lessons);
        let c = |id: &str| conscience::Challenge { id: id.into(), ..Default::default() };
        let ls = [c("A"), c("B"), c("C"), c("D")];
        let applied: Vec<(&conscience::Challenge, u8)> = ls.iter().zip([3, 1, 2, 1]).collect();
        let mut gates = Vec::new();
        // One after the other, with the hook's own wait: a process spawned by another test while the
        // first held the lock keeps a copy of its descriptor, so the flock, until it execs.
        for check in ["0000000000a1", "0000000000b2"] {
            let at = Decide { session: "decide-fresh", check, channel: "live", lessons: &lessons, tokens: 0, recorded: true };
            let r = decide(Policy::Learned, &applied, &at, &stale, DECIDE_WAIT, |r, b| record_beside(&lessons, &applied, check, r, b));
            gates.push(r.log.gate);
        }
        let g = graded_records(&lessons);
        assert_eq!(g.len(), 2);
        assert!(g.iter().all(|o| o.policy == "learned" && o.reason.is_empty()), "{g:?}");
        assert_ne!(g[0].free, Vec::<String>::new(), "the first deviates: everything free at a live check, {:?}", g[0]);
        assert_eq!(g[0].seen, stale.key(), "nothing was decided before the first");
        // The second's summary is the start's with the first's record folded in, its ledger entry too.
        let log = std::fs::read_to_string(outcome::path_beside(&lessons)).unwrap();
        let first = log.lines().next().unwrap();
        let after = summary::fold(&format!("{first}\n"), stale.clone());
        assert_eq!(after.gate.checks, stale.gate.checks + 1, "the first's decision is in the gate's ledger");
        assert_eq!(g[1].seen, after.key(), "decided on the summary loaded under the lock, not the one read at its start");
        assert_eq!(gates[1], policy::remaining(&after.gate).max(0.0), "against the gate with the first's entry");
        let _ = std::fs::remove_dir_all(lessons.parent().unwrap());
    }

    /// Two checks deciding at once take turns: while one holds the store's decision lock (deciding and
    /// recording), the other, past its wait, takes the fixed rule, which reads no gate, and says why;
    /// never both against the same gate. So for a deviation on evidence and for the question-count test
    /// alike (its draw logged on the learned record only). A lock that cannot be taken at all is the
    /// fixed rule too.
    #[test]
    fn two_checks_deciding_at_once_never_both_take_the_gate() {
        use crate::select::{Policy, policy, summary};
        let c = |id: &str| conscience::Challenge { id: id.into(), ..Default::default() };
        let ls = [c("A"), c("B"), c("C"), c("D")];
        let applied: Vec<(&conscience::Challenge, u8)> = ls.iter().zip([3, 1, 2, 1]).collect();
        let mut stores = Vec::new();
        for (name, sure, sources) in [("race", true, &[policy::EVIDENCE][..]), ("race-test", false, &[policy::EXPERIMENT, policy::EXPERIMENT_CONTROL][..])] {
            let lessons = decide_store_with(name, sure);
            let stale = summary::refresh(&lessons);
            // Each records only once both have decided: the first to take the lock holds it until the
            // other decided without it.
            let both = std::sync::Barrier::new(2);
            std::thread::scope(|s| {
                for check in ["0000000000c1", "0000000000c2"] {
                    let (applied, lessons, stale, both) = (&applied, &lessons, &stale, &both);
                    s.spawn(move || {
                        let at = Decide { session: "decide-race", check, channel: "live", lessons, tokens: 0, recorded: true };
                        decide(Policy::Learned, applied, &at, stale, std::time::Duration::ZERO, |r, b| {
                            both.wait();
                            record_beside(lessons, applied, check, r, b)
                        });
                    });
                }
            });
            let mut got: Vec<(String, String, String, bool)> =
                graded_records(&lessons).into_iter().map(|o| (o.policy, o.source, o.reason, o.xk.is_some())).collect();
            got.sort();
            assert_eq!(got.len(), 2, "{name}: {got:?}");
            assert_eq!(got[0], ("fixed".to_string(), "fixed".to_string(), outcome::DECIDED_BUSY.to_string(), false), "{name}: the other took the fixed rule");
            let (policy, source, reason, drawn) = &got[1];
            assert!(policy == "learned" && sources.contains(&source.as_str()) && reason.is_empty(), "{name}: one decided under the lock: {got:?}");
            assert_eq!(*drawn, !sure, "{name}: the test's draw on its record alone");
            assert!(policy::remaining(&summary::refresh(&lessons).gate) >= -1e-9, "{name}: the gate never overspent");
            stores.push(lessons);
        }
        // No lock to be had (the store's directory is gone): the fixed rule, and why.
        let stale = summary::refresh(&stores[0]);
        let gone = std::path::Path::new("/nonexistent/borg-decide/lessons.jsonl");
        let at = Decide { session: "decide-race", check: "0000000000c3", channel: "live", lessons: gone, tokens: 0, recorded: true };
        let mut why = "";
        let r = decide(Policy::Learned, &applied, &at, &stale, DECIDE_WAIT, |_, b| {
            why = b.why;
            true
        });
        assert_eq!((r.log.policy, why), ("fixed", outcome::DECIDED_NO_LOCK));
        for lessons in stores {
            let _ = std::fs::remove_dir_all(lessons.parent().unwrap());
        }
    }

    /// The fixed rule and the evaluation switches read no gate: while another check holds the store's
    /// decision lock, each decides by itself at once, on the summary its check read at its start, and its
    /// record gives no reason; only a learned check waits for the lock or falls back.
    #[test]
    fn a_policy_that_reads_no_gate_decides_by_itself_whoever_holds_the_lock() {
        use crate::select::{Ctx, Policy, graded_under, summary};
        let lessons = decide_store("nolock");
        let stale = summary::refresh(&lessons);
        let c = |id: &str| conscience::Challenge { id: id.into(), ..Default::default() };
        let ls = [c("A"), c("B"), c("C"), c("D")];
        let applied: Vec<(&conscience::Challenge, u8)> = ls.iter().zip([3, 1, 3, 2]).collect();
        let ids = |cs: &[&conscience::Challenge]| cs.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        let held = lock_within(&decide_lock(&lessons), std::time::Duration::ZERO).unwrap();
        let check = "0000000000e1";
        let at = Decide { session: "decide-nolock", check, channel: "stop", lessons: &lessons, tokens: 0, recorded: true };
        let ctx = Ctx {
            session: at.session,
            check,
            channel: at.channel,
            lessons: &lessons,
            summary: &stale,
            tokens: 0,
            had: &[],
            alpha: crate::select::alpha(&lessons),
        };
        let fixed = ids(&graded_under(Policy::Fixed, &applied, &ctx).ask);
        for policy in [Policy::Fixed, Policy::Cap(1), Policy::Attention] {
            let own = graded_under(policy, &applied, &ctx);
            let mut got = None;
            let r = decide(policy, &applied, &at, &stale, std::time::Duration::ZERO, |r, b| {
                got = Some((r.log.policy, b.why, b.summary.key()));
                true
            });
            assert_eq!(got, Some((policy.name(), "", stale.key())), "{policy:?}");
            assert_eq!((ids(&r.ask), ids(&r.free)), (ids(&own.ask), ids(&own.free)), "{policy:?}: its own rule");
        }
        // Not vacuous: the cap's block differs from the fixed rule's, which a fallback would have asked.
        assert_ne!(ids(&graded_under(Policy::Cap(1), &applied, &ctx).ask), fixed);
        let mut why = "";
        let r = decide(Policy::Learned, &applied, &at, &stale, std::time::Duration::ZERO, |_, b| {
            why = b.why;
            true
        });
        assert_eq!((r.log.policy, why), ("fixed", outcome::DECIDED_BUSY), "the learned policy alone waits for the lock");
        drop(held);
        let _ = std::fs::remove_dir_all(lessons.parent().unwrap());
    }

    /// A learned decision is carried out only once its record landed in the log its gate is folded from,
    /// the gate's only debit. One whose record would not land there (learning off, the lessons of
    /// another store: `Decide::recorded`) or did not (a failed append) takes the fixed rule and says why,
    /// so checks one after another never each risk the whole allowance of a gate none of them debited.
    #[test]
    fn a_learned_decision_whose_record_cannot_land_takes_the_fixed_rule() {
        use crate::select::{Ctx, Policy, graded_under, policy, summary};
        let lessons = decide_store("unrecorded");
        let c = |id: &str| conscience::Challenge { id: id.into(), ..Default::default() };
        let ls = [c("A"), c("B"), c("C"), c("D")];
        let applied: Vec<(&conscience::Challenge, u8)> = ls.iter().zip([3, 1, 2, 1]).collect();
        let ids = |cs: &[&conscience::Challenge]| cs.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        let at = |check: &'static str, recorded: bool| Decide { session: "decide-unrecorded", check, channel: "live", lessons: &lessons, tokens: 0, recorded };
        let fixed = |check: &str, s: &summary::Summary| {
            let ctx = Ctx {
                session: "decide-unrecorded",
                check,
                channel: "live",
                lessons: &lessons,
                summary: s,
                tokens: 0,
                had: &[],
                alpha: crate::select::alpha(&lessons),
            };
            let r = graded_under(Policy::Fixed, &applied, &ctx);
            (ids(&r.ask), ids(&r.free), ids(&r.placebo))
        };
        // Its record lands: the learned policy deviates (not vacuous).
        let stale = summary::refresh(&lessons);
        let r = decide(Policy::Learned, &applied, &at("0000000000f1", true), &stale, std::time::Duration::ZERO, |r, b| {
            record_beside(&lessons, &applied, "0000000000f1", r, b)
        });
        assert_eq!(r.log.source, policy::EVIDENCE, "{:?}", r.log);
        let (ask, free, _) = fixed("0000000000f1", &stale);
        assert_ne!((ids(&r.ask), ids(&r.free)), (ask, free), "it deviated");
        let gate = summary::refresh(&lessons).gate;
        // Two checks whose records would not land, one after the other: both the fixed rule, never tried.
        for check in ["0000000000f2", "0000000000f3"] {
            let stale = summary::refresh(&lessons);
            let mut tried = Vec::new();
            let r = decide(Policy::Learned, &applied, &at(check, false), &stale, std::time::Duration::ZERO, |r, b| {
                tried.push((r.log.policy, b.why));
                false
            });
            assert_eq!(tried, [("fixed", outcome::DECIDED_NO_RECORD)], "{check}");
            assert_eq!((ids(&r.ask), ids(&r.free), ids(&r.placebo)), fixed(check, &stale), "{check}");
        }
        // A check whose record failed to append: decided again by the fixed rule under the lock, and that
        // carried out; the store's log and its gate are as they were.
        let stale = summary::refresh(&lessons);
        let mut tried = Vec::new();
        // After the first check's lock, so with the hook's wait (as in the test above).
        let r = decide(Policy::Learned, &applied, &at("0000000000f4", true), &stale, DECIDE_WAIT, |r, b| {
            tried.push((r.log.policy, b.why));
            false
        });
        assert_eq!(tried, [("learned", ""), ("fixed", outcome::DECIDED_NO_RECORD)]);
        assert_eq!((ids(&r.ask), ids(&r.free), ids(&r.placebo)), fixed("0000000000f4", &stale));
        assert_eq!(graded_records(&lessons).len(), 1, "only the first check's record");
        assert_eq!(summary::refresh(&lessons).gate, gate, "no unrecorded decision moved the gate");
        let _ = std::fs::remove_dir_all(lessons.parent().unwrap());
    }

    /// A lesson another check queued to be asked or handed over, not delivered yet, is had at a decision
    /// as a delivered one is (`waiting_in`): it never takes a free slot (the next lesson does) nor a
    /// placebo's, and the record names it (`nofree`), so delivery never drops a second copy as queued
    /// twice. A lesson waiting as a placebo was routed nowhere: it may still go free.
    #[test]
    fn a_lesson_waiting_in_the_queue_never_takes_a_free_or_placebo_slot() {
        use crate::select::{Policy, summary};
        let lessons = decide_store("queued");
        let stale = summary::refresh(&lessons);
        let session = format!("decide-queued-{}", std::process::id());
        let _ = std::fs::remove_file(state_file(&session));
        std::fs::write(queued_file(&session), "X @0000000000d0\nlive Y @0000000000d0\nplacebo Z @0000000000d0\n").unwrap();
        let c = |id: &str| conscience::Challenge { id: id.into(), ..Default::default() };
        let named: Vec<String> = (1..=10).map(|i| format!("L{i:02}")).collect();
        let ls: Vec<conscience::Challenge> = ["X", "Y", "Z"].iter().map(|i| c(i)).chain(named.iter().map(|i| c(i))).collect();
        let ids = |cs: &[&conscience::Challenge]| cs.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        // No 3, so the fixed rule hands over up to `CAP`: X and Y first in line (the slots they would take
        // go to the next), or last, past ten 2s, where a placebo is drawn among what is left.
        let first: Vec<(&conscience::Challenge, u8)> = ls.iter().map(|c| (c, if ["X", "Y"].contains(&c.id.as_str()) { 2 } else { 1 })).collect();
        let last: Vec<(&conscience::Challenge, u8)> = ls.iter().map(|c| (c, if c.id.starts_with('L') { 2 } else { 1 })).collect();
        let waits = |l: &[String]| l.iter().any(|i| i == "X" || i == "Y");
        for (check, applied) in [("0000000000d1", &first), ("0000000000d2", &last)] {
            let at = Decide { session: &session, check, channel: "stop", lessons: &lessons, tokens: 0, recorded: true };
            let r = decide(Policy::Fixed, applied, &at, &stale, std::time::Duration::ZERO, |r, b| record_beside(&lessons, applied, check, r, b));
            let (free, placebo) = (ids(&r.free), ids(&r.placebo));
            assert_eq!(free.len(), conscience::CAP, "{check}: every free slot taken: {free:?}");
            assert!(!waits(&free) && !waits(&placebo), "{check}: free {free:?}, placebo {placebo:?}");
        }
        assert_eq!(graded_records(&lessons)[1].placebo, ["Z"], "waiting as a placebo, Z is still drawn");
        assert!(graded_records(&lessons)[0].free.contains(&"Z".to_string()), "and still goes free");
        let g = graded_records(&lessons);
        assert_eq!(g.iter().map(|o| o.nofree.clone()).collect::<Vec<_>>(), [["X", "Y"], ["X", "Y"]], "{g:?}");
        let _ = std::fs::remove_file(queued_file(&session));
        let _ = std::fs::remove_dir_all(lessons.parent().unwrap());
    }

    #[test]
    fn only_an_adapter_reads_which_messages_an_answer_showed() {
        let out = json!({"hookSpecificOutput": {"additionalContext": "[borg] Message from s1: hi"}, "shown_ids": ["m0123456789ab"]});
        for name in ["claude", "codex"] {
            let h = harness::by_name(name).unwrap();
            assert_eq!(for_harness(h, out.clone()), json!({"hookSpecificOutput": out["hookSpecificOutput"]}), "{name}");
        }
        for name in ["omp", "opencode"] {
            assert_eq!(for_harness(harness::by_name(name).unwrap(), out.clone()), out, "{name}");
        }
    }
}
