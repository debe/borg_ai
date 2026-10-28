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
            if paths.is_empty() { cmd.chars().take(200).collect() } else { paths.join(" ") }
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

/// Which turn the session is in: the size of its prompts file, which every prompt grows. A live check
/// records the turn it started in (live.rs `Mark`), so it covers that turn's stop only.
fn turn_id(session: &str) -> u64 {
    std::fs::metadata(prompts_file(session)).map_or(0, |m| m.len())
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

/// The session's first task prompt, never overwritten: what a plain session's commits set out to do
/// (playbook.rs `plain_win`), where its task file holds the latest prompt ("commit it", "thanks").
fn first_task_file(session: &str) -> PathBuf {
    state_file(session).with_extension("first-task")
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
            record(session, input, task, proposal(h, input), Some(tool), said.chars().take(1500).collect());
            return;
        }
    }
    if !learn::is_correction(prompt) || task.is_empty() {
        let task: String = prompt.chars().take(3000).collect();
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).create_new(true).open(first_task_file(session)) {
            let _ = f.write_all(task.as_bytes());
        }
        let _ = std::fs::write(task_file(session), task);
        return;
    }
    record(session, input, task, proposal(h, input), None, prompt.chars().take(1500).collect());
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
        input.get("tool_input").map(|v| v.to_string()).unwrap_or_default().chars().take(1500).collect::<String>()
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

/// The drone's last proposal, its last 2000 characters (`harness::last_message`: the adapter's, else
/// the transcript's).
fn proposal(h: &dyn Harness, input: &Value) -> String {
    let t = full_proposal(h, input);
    t.chars().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect()
}

/// The drone's whole last proposal (`proposal` keeps its last 2000 characters): the reasoning note
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
/// weighted by their memory with this session's usefulness sample (memory.rs).
fn lessons(session: &str) -> Vec<conscience::Challenge> {
    let before = std::env::var("BORG_BEFORE").ok().and_then(|v| v.parse().ok());
    let mut all = conscience::load_for(&lessons_path(), before, Some(session));
    // The system gate (systems.rs): where the repo says which system this is, lessons scoped to other
    // systems score 0, however much the conversation names them.
    let where_text = std::fs::read_to_string(where_file(session)).unwrap_or_default();
    let repos = crate::systems::work_repos(&where_text);
    if crate::systems::on() {
        crate::systems::gate(&mut all, &repos);
    }
    // Reach (reach.rs): a withdrawn personal lesson is never delivered, a repo convention only in its repo.
    all.retain(|c| crate::reach::deliverable(c, &repos));
    // Spike (concepts.rs): lessons sharing engineering concepts with the code written, the touched paths
    // and the latest prompt rank higher; never a filter.
    if crate::concepts::on() {
        let where_text = std::fs::read_to_string(where_file(session)).unwrap_or_default();
        let edits = std::fs::read_to_string(edits_file(session)).unwrap_or_default();
        let latest = std::fs::read_to_string(state_file(session).with_extension("lastprompt")).unwrap_or_default();
        crate::concepts::apply(&mut all, &crate::concepts::of_work(&edits, &where_text, &latest), crate::concepts::beta());
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
        out.extend(crate::memory::resolve(&asked, all, &before, &after.join("\n"), final_msg, crate::memory::NOVEL_WORDS).0);
    }
    out
}

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
    Some(text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(600).collect())
}

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
        let _ = writeln!(f, "#borg-answer {}", one_line(&full_proposal(h, input), 4000));
    }
}

/// Pending lessons sent to the judge per call at most (the most recently asked): one full block
/// (conscience.rs caps a block at 10), whose lessons share their evidence in the prompt.
const MAX_PENDING: usize = 10;
/// A lesson whose verdicts stay pending (unclear, missing) is sent to the judge this often at most;
/// after that it waits for `borg learn` to settle it as `asked_not_corrected`.
const MAX_JUDGE_TRIES: usize = 2;

/// The markers of a delivery of lessons to the drone, in the state and .edits files: asked at a Stop or
/// live (`#borg-asked`), delivered early, handed over free (`#borg-queued`).
const DELIVERED: [&str; 3] = ["#borg-asked ", "#borg-early ", "#borg-queued "];

/// R1: lessons delivered earlier in this session (asked at a Stop or live, delivered early, handed over
/// free) that have no verdict yet, with the evidence for the consequence model (predict.rs `Pending`):
/// the work before the delivery (at a Stop, the blocked message), and after it the drone's answer (a
/// question's only: a free delivery asks nothing), its edits and later messages since, and this turn's
/// message (`final_msg`), each part capped at `predict::JUDGE_PART` characters. `#borg-judged
/// <id>=<label>` lines in the state file record verdicts, each for the lesson's latest delivery before
/// it: a lesson delivered early, judged, then asked at a Stop is judged again on the question.
/// `answered_only` (a live check, mid-turn): questions the drone has not answered yet (no `#borg-answer`
/// after them, written at the stop) wait for the stop's judge, which sees the answer, as in hook mode;
/// free deliveries wait for it too. Of the `MAX_PENDING` slots, questions take theirs first (the newest),
/// free deliveries what is left: a free hand-over never pushes a question out of its judge. The flag:
/// whether a question is among them.
fn pending<'a>(session: &str, state: &str, all: &'a [conscience::Challenge], final_msg: &str, answered_only: bool) -> (Vec<predict::Pending<'a>>, bool) {
    let mut tries: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut settled = std::collections::HashSet::new();
    for l in state.lines() {
        if let Some(l) = l.strip_prefix("#borg-judged ") {
            let (id, label) = l.split_once('=').unwrap_or((l, ""));
            *tries.entry(id).or_default() += 1;
            if label.trim() != "pending" {
                settled.insert(id);
            }
        } else if let Some(ids) = DELIVERED.iter().find_map(|m| l.strip_prefix(m)) {
            for id in ids.split(',').map(str::trim) {
                tries.remove(id);
                settled.remove(id);
            }
        }
    }
    let edits = std::fs::read_to_string(edits_file(session)).unwrap_or_default();
    let lines: Vec<&str> = edits.lines().collect();
    let part = predict::JUDGE_PART;
    // Each with whether it was a question (`#borg-asked`), not a free delivery.
    let mut out: Vec<(predict::Pending, bool)> = Vec::new();
    for (p, l) in lines.iter().enumerate() {
        let Some((marker, ids)) = DELIVERED.iter().find_map(|m| l.strip_prefix(m).map(|ids| (*m, ids))) else { continue };
        let before = p.checked_sub(1).map(|b| lines[b]).filter(|b| !b.starts_with("#borg-")).unwrap_or_default();
        let later = &lines[p + 1..];
        let answers: Vec<&str> = if marker == "#borg-asked " { later.iter().filter_map(|l| l.strip_prefix("#borg-answer ")).collect() } else { Vec::new() };
        if answered_only && answers.is_empty() {
            continue;
        }
        let work: Vec<&str> = later.iter().copied().filter(|l| !l.starts_with("#borg-")).collect();
        let after = format!(
            "answer: {}\nwork since: {}\nthis turn: {}",
            predict::clip(&answers.join(" "), part),
            predict::clip(&work.join(" | "), part),
            predict::clip(&one_line(final_msg, part + 1), part)
        );
        let said = answers.join("\n").to_lowercase();
        for id in ids.split(',').map(str::trim).filter(|i| !i.is_empty()) {
            if settled.contains(id) || tries.get(id).copied().unwrap_or(0) >= MAX_JUDGE_TRIES {
                continue;
            }
            let Some(c) = all.iter().find(|c| c.id == id) else { continue };
            // A later delivery of the same lesson is the one judged: its evidence replaces the earlier one's.
            out.retain(|x| x.0.lesson.id != id);
            let declared = crate::memory::declared(&said, &id.to_lowercase());
            out.push((predict::Pending { lesson: c, before: before.to_string(), after: after.clone(), declared }, marker == "#borg-asked "));
        }
    }
    let questions = out.iter().filter(|x| x.1).count().min(MAX_PENDING);
    let mut room = [MAX_PENDING - questions, questions];
    let mut kept: Vec<(predict::Pending, bool)> = Vec::new();
    for x in out.into_iter().rev() {
        let r = &mut room[usize::from(x.1)];
        if *r > 0 {
            *r -= 1;
            kept.push(x);
        }
    }
    kept.reverse();
    (kept.into_iter().map(|x| x.0).collect(), questions > 0)
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
        // One record per channel: the statistics split what the deliveries led to by how they were made.
        let edits = std::fs::read_to_string(edits_file(session)).unwrap_or_default();
        let by = channels(&edits);
        for (kind, lessons) in [(outcome::CHANGED, changed), (outcome::IGNORED, ignored)] {
            for channel in ["live", "stop", "early", "queued"] {
                let lessons: Vec<String> = lessons.iter().filter(|id| by.get(id.as_str()).copied().unwrap_or("stop") == channel).cloned().collect();
                if !lessons.is_empty() {
                    let o = outcome::Outcome {
                        kind: kind.into(),
                        session: session.into(),
                        lessons,
                        ms: learn::now_ms(),
                        repo: repo_of(cwd),
                        channel: channel.into(),
                        ..Default::default()
                    };
                    let _ = outcome::append(&learn::home().join("outcomes.jsonl"), &o);
                }
            }
        }
    }
}

/// The channel of each lesson's latest delivery in the .edits file `text`: `stop` (`#borg-asked`),
/// `live` (a live check's, `#borg-live <ids> ms=<epoch ms>` right after its `#borg-asked` line), `early`
/// or `queued`.
fn channels(text: &str) -> std::collections::HashMap<&str, &'static str> {
    let mut out = std::collections::HashMap::new();
    for l in text.lines() {
        let (ids, channel) = if let Some(ids) = l.strip_prefix("#borg-asked ") {
            (ids, "stop")
        } else if let Some(rest) = l.strip_prefix("#borg-live ") {
            (rest.split_whitespace().next().unwrap_or_default(), "live")
        } else if let Some(ids) = l.strip_prefix("#borg-early ") {
            (ids, "early")
        } else if let Some(ids) = l.strip_prefix("#borg-queued ") {
            (ids, "queued")
        } else {
            continue;
        };
        for id in ids.split(',').map(str::trim).filter(|i| !i.is_empty()) {
            out.insert(id, channel);
        }
    }
    out
}

/// Ids listed on the state file's `marker` lines (`#borg-asked a,b`).
pub(crate) fn listed<'a>(context: &'a str, marker: &str) -> std::collections::HashSet<&'a str> {
    context.lines().filter_map(|l| l.strip_prefix(marker)).flat_map(|ids| ids.split(',')).map(str::trim).filter(|i| !i.is_empty()).collect()
}

/// Early delivery (conscience.rs `early`): once the session's prompts and touches name a lesson scope
/// (a topic) and that scope's lessons score on them, the top ones go to the drone at once, as context,
/// instead of waiting for the Stop checkpoint: at the prompt or the first touch on the topic (`text` is
/// this event's, already in the state file). Each lesson once per session (`#borg-early`; Stop does not
/// ask it again) and an `asked` outcome with channel early (`record_delivery`). A topic is settled (`#borg-topic`) once a delivery covered it or none of its lessons is
/// left; until then each prompt or touch looks again. At most `MAX_EARLY_DELIVERIES` per session. Only
/// where the harness takes it (`Caps::early_delivery`, Claude Code so far): omp and opencode take a prompt's
/// and a tool call's context too (live mode), but early delivery to them waits for an evaluation, so for
/// them the lessons stay with Stop. Standing lessons (ratified constraints, conscience.rs `standing_pool`)
/// are no ranking guess and need no evaluation: they come on every harness that takes the context (a
/// prompt's, or a tool call's for a named scope), whenever their scope is named (an unscoped one at a
/// prompt), settled topic or not and past `MAX_EARLY_DELIVERIES`, until each came once. `BORG_EARLY=off`
/// disables it; so do the eval modes BORG_ONLY and BORG_CRITIC.
/// Early deliveries per session at most (each up to `conscience::render_early`'s lesson cap).
const MAX_EARLY_DELIVERIES: usize = 3;

fn early(h: &dyn Harness, hook_event: &str, session: &str, input: &Value, text: &str) -> Option<String> {
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
    let mut done = listed(&context, "#borg-asked ");
    done.extend(listed(&context, "#borg-early "));
    done.extend(listed(&context, "#borg-queued "));
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
    append(session, &format!("#borg-early {ids}"));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(edits_file(session)) {
        let _ = writeln!(f, "#borg-early {ids}");
    }
    let p = conscience::early_propensity(&pool, session, &shown_topics);
    record_delivery(session, input, delivered, "early", delivered.iter().map(|c| p.get(&c.id).copied().unwrap_or(0.0)).collect());
    log_cost(session, &format!("free event={hook_event} lessons={} chars={}", delivered.len(), text.len()));
    crate::stats::count(&learn::home(), crate::stats::EARLY, session, |e| {
        e.n = delivered.len() as u64;
        e.lessons = delivered.iter().map(|c| c.id.clone()).collect();
        e.channel = "early".into();
    });
    Some(text)
}

/// The `asked` outcome of a free delivery (`channel` early or queued) of `delivered`, with each one's
/// propensity `p`: delivered as context, the lessons wait for a result as a Stop's questions do. The
/// next Stop's judge (`pending`) labels them changed or ignored, a merged correction corrected
/// (outcome.rs `contradicted`), and `borg learn` settles the rest. Nothing with learning off.
fn record_delivery(session: &str, input: &Value, delivered: &[&conscience::Challenge], channel: &str, p: Vec<f64>) {
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
        ..Default::default()
    };
    let _ = outcome::append(&learn::home().join("outcomes.jsonl"), &asked);
}

/// Graded Stop (default): ids graded 2 or 1 at a turn end that did not block, one comma-separated line
/// per such Stop, waiting for the session's next prompt or touch (`queued`). In live mode a check during
/// the turn (`live_check`) adds its 2s and 1s the same way, and its 3s as a `live <ids>` line.
fn queued_file(session: &str) -> PathBuf {
    state_file(session).with_extension("queued")
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

/// Live mode, at the stop: the live checks' asks still waiting in the `.queued` file (`live <ids>`), taken
/// out of it; the 2s and 1s stay for the next prompt or touch.
fn take_live(session: &str) -> Vec<String> {
    let Some(text) = take_queue(session) else { return Vec::new() };
    let (live, rest): (Vec<&str>, Vec<&str>) = text.lines().partition(|l| l.starts_with("live "));
    for line in rest {
        enqueue(session, line);
    }
    live.iter().flat_map(|l| l["live ".len()..].split(',')).map(str::trim).filter(|i| !i.is_empty()).map(String::from).collect()
}

/// The waiting graded lessons as free context, once: those not asked, delivered early or handed over
/// since, as many as one early delivery fits (`conscience::render_early`); the rest are dropped. Marked
/// `#borg-queued` in the state and .edits files, so neither this queue nor early delivery repeats them,
/// and Stop treats them as early lessons (done once acted on); an `asked` outcome with channel queued
/// (`record_delivery`). A live check's asks (`live` lines) come
/// first, as questions to answer (`ask_live`).
fn queued(h: &dyn Harness, hook_event: &str, session: &str, input: &Value) -> Option<String> {
    let ids = take_queue(session)?;
    let all = lessons(session);
    let state = std::fs::read_to_string(state_file(session)).unwrap_or_default();
    let mut done = listed(&state, "#borg-asked ");
    done.extend(listed(&state, "#borg-early "));
    done.extend(listed(&state, "#borg-queued "));
    let (mut firm, mut wanted): (Vec<&conscience::Challenge>, Vec<&conscience::Challenge>) = (Vec::new(), Vec::new());
    for line in ids.lines() {
        let (live, list) = line.strip_prefix("live ").map_or((false, line), |l| (true, l));
        for id in list.split(',').map(str::trim) {
            if let Some(c) = all.iter().find(|c| c.id == id)
                && !done.contains(id)
                && !firm.iter().chain(&wanted).any(|w| w.id == id)
            {
                if live { firm.push(c) } else { wanted.push(c) }
            }
        }
    }
    let mut parts: Vec<String> = Vec::new();
    if !firm.is_empty() {
        parts.push(ask_live(h, hook_event, session, input, &mut firm));
    }
    if let Some((text, n)) = conscience::render_early(&wanted, &[]) {
        let ids = wanted[..n].iter().map(|c| c.id.as_str()).collect::<Vec<_>>().join(",");
        append(session, &format!("#borg-queued {ids}"));
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(edits_file(session)) {
            let _ = writeln!(f, "#borg-queued {ids}");
        }
        // Handed over as the graded check left them: no draw, so each was certain to come (p 1).
        record_delivery(session, input, &wanted[..n], "queued", vec![1.0; n]);
        log_cost(session, &format!("free event={hook_event} lessons={n} chars={} graded dropped={}", text.len(), wanted.len() - n));
        crate::stats::count(&learn::home(), crate::stats::QUEUED_FREE, session, |e| {
            e.n = n as u64;
            e.lessons = wanted[..n].iter().map(|c| c.id.clone()).collect();
            e.channel = "queued".into();
        });
        parts.push(text);
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// Live mode: a live check's asks, delivered now (conscience.rs `render_live`). Asked is recorded here, at
/// delivery, where the drone sees them: `#borg-asked <ids>` and `#borg-live <ids> ms=<now>` in the state
/// file, the same in .edits after the work so far (the judge's evidence before them, `pending`), the
/// outcome `asked` with channel live, and the stat `live_ask`. No extra round: a free delivery in the cost.
fn ask_live(h: &dyn Harness, hook_event: &str, session: &str, input: &Value, firm: &mut [&conscience::Challenge]) -> String {
    crate::memory::shuffle(firm, session);
    let ids = firm.iter().map(|c| c.id.as_str()).collect::<Vec<_>>().join(",");
    let now = learn::now_ms();
    // One write: `prefilter::turn` and E3 read the `#borg-live` line right after its `#borg-asked`.
    append(session, &format!("#borg-asked {ids}\n#borg-live {ids} ms={now}"));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(edits_file(session)) {
        let _ = writeln!(f, "{}\n#borg-asked {ids}\n#borg-live {ids} ms={now}", one_line(&work_so_far(h, input), 4000));
    }
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
    if learning() {
        let lessons: Vec<String> = firm.iter().map(|c| c.id.clone()).collect();
        let (grades, ranks, applied) = graded_of(session, &lessons);
        let p = propensity_of(session, &lessons);
        let (pool, base, scoped) = pool_of(session, &lessons);
        let asked = outcome::Outcome {
            kind: outcome::ASKED.into(),
            session: session.into(),
            lessons,
            ms: now,
            repo: repo_of(cwd),
            groups: conscience::disagreement_ids(firm),
            channel: "live".into(),
            grades,
            ranks,
            applied,
            p,
            pool,
            base,
            scoped,
            ..Default::default()
        };
        let _ = outcome::append(&learn::home().join("outcomes.jsonl"), &asked);
    }
    let text = conscience::render_live(firm);
    log_cost(session, &format!("free event={hook_event} lessons={} chars={} live", firm.len(), text.len()));
    stat(session, input, crate::stats::LIVE_ASK, |e| {
        e.n = firm.len() as u64;
        e.lessons = firm.iter().map(|c| c.id.clone()).collect();
        e.channel = "live".into();
    });
    text
}

/// Live mode: this turn's work so far (harness `work_so_far`): from the transcript where the harness
/// writes one, else the turn's tool calls (`turn_file`): no prose.
fn work_so_far(h: &dyn Harness, input: &Value) -> String {
    let session = input.get("session_id").and_then(Value::as_str).unwrap_or("unknown");
    harness::work_so_far(h.caps(), input, &turn_file(session), LIVE_WORK_CHARS)
}

/// This turn's tool calls, one `[tool] touch` line each, and their edit text: the work so far where the
/// harness sends no transcript. Emptied at each prompt.
fn turn_file(session: &str) -> PathBuf {
    state_file(session).with_extension("turn")
}

/// How much of the turn so far a live check reads: as much as the blocked message `pending` keeps.
const LIVE_WORK_CHARS: usize = 4000;

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
    let parts: Vec<String> = [queued(h, hook_event, session, input), early(h, hook_event, session, input, text), extra].into_iter().flatten().collect();
    (!parts.is_empty()).then(|| json!({"hookSpecificOutput": {"hookEventName": hook_event, "additionalContext": parts.join("\n\n")}}))
}

/// What one model check of the work decided (`check`).
enum Verdict {
    /// The free gate skipped the model call: the score, or `noedit`.
    Skip(String),
    /// The model gave no answer: fail open.
    Failed,
    /// The ids to ask: in a block at the stop, with the next prompt or tool call in live mode.
    Ask(Vec<String>),
}

/// The work a check judges, as the stop or a live check read it.
struct Work<'a> {
    /// Every lesson of the session (the judge's pending lessons and the ungraded widening look here).
    all: &'a [conscience::Challenge],
    /// The lessons that may be asked: not done, not of another system.
    challenges: &'a [conscience::Challenge],
    done: &'a std::collections::HashSet<&'a str>,
    /// The state file as read before the check.
    state: &'a str,
    /// The state file plus the drone's work text: the gate finds the turn in it by its markers; the pool
    /// and the model read it without them (`conscience::work_text`).
    context: &'a str,
    scope_text: &'a str,
    /// The drone's message for the judge (`pending`): its final message at the stop, the turn so far live.
    message: &'a str,
    /// BORG_STOP_GATE=edits: whether the turn changed anything.
    changed: bool,
    /// Whether this check judges the pending lessons: not while another check (holding the session's
    /// lock) may be judging the same ones.
    judge: bool,
    /// The harness's stop budget (none: the model's own limit) and when the hook started: the model call
    /// gets what is left of it when it starts (live.rs `call_limit`).
    budget: Option<std::time::Duration>,
    started: std::time::Instant,
}

/// The consequence model's check of `w`, shared by the stop and live mode's `borg check` (`live`): the
/// free gate, the pool, one graded call that also judges the pending lessons, and the graded split. The
/// 2s and 1s of a graded call wait in the `.queued` file; what to ask is returned.
/// A live call is logged as `live_check` with a `live usd=… lessons=…` cost line, the stop's as `check`.
fn check(session: &str, input: &Value, w: &Work, live: bool) -> Verdict {
    // The free gate (prefilter.rs): a turn whose words touch no lesson costs no model call; the conscience
    // then asked nothing. BORG_STOP_GATE=edits instead: a turn that changed nothing costs no model call.
    // A live check has no gate: it runs only after a change, the debounce bounds it, and a turn half done
    // has too few words for the gate (the E3 smoke skipped every mid-turn check at 2.8 and 7.7 of 8).
    let skip = if live {
        None
    } else if prefilter::edits_gate() {
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
    let shortlist = if whole {
        let ranked = conscience::rank_scoped(w.challenges, text, w.scope_text);
        let eligible = crate::whole::eligible(w.challenges, &ranked, w.scope_text);
        let n = eligible.len();
        let (shown, fell) = crate::whole::select(eligible, shortlist_now, &ranked, crate::whole::tokens());
        log_cost(session, &format!("pool mode=all eligible={n} shown={} budget={}", shown.len(), if fell { "full" } else { "fits" }));
        shown
    } else {
        shortlist_now()
    };
    let model = std::env::var("BORG_PREDICT_MODEL").unwrap_or_default();
    // R1: the same call judges what the drone did with lessons asked earlier this session.
    let (pend, question) = if w.judge { pending(session, w.state, w.all, w.message, live) } else { (Vec::new(), false) };
    // Nothing to ask and no question to judge (no lesson in the pool, none pending): no model call. A live
    // check has no free gate, so without this every change of a session with no lessons paid for one.
    // Free deliveries alone never pay for a call: they wait for the next one, or `borg learn` settles them.
    if shortlist.is_empty() && !question {
        return Verdict::Skip("empty".into());
    }
    let extra = predict::judge_section(&pend).len();
    let graded = crate::whole::graded();
    let timeout = live::call_limit(w.budget, w.started.elapsed());
    let called = std::time::Instant::now();
    let pred = if whole && !crate::whole::classic() {
        predict::predict_whole(text, &shortlist, &pend, &model, graded, timeout)
    } else {
        predict::predict(text, &shortlist, &pend, &model, graded, timeout)
    };
    stat(session, input, if live { crate::stats::LIVE_CHECK } else { crate::stats::CHECK }, |e| {
        e.n = shortlist.len() as u64;
        e.dur_ms = called.elapsed().as_millis() as u64;
        e.usd = pred.as_ref().map_or(0.0, |p| p.cost);
        if pred.is_none() {
            e.why = "failed".into();
        }
    });
    let Some(pred) = pred else { return Verdict::Failed };
    // Live calls log this line too: the judge's call numbering counts every model call of the session.
    append(session, &format!("#borg-predict-cost {:.4}", pred.cost));
    if live {
        log_cost(session, &format!("live usd={:.4} lessons={}", pred.cost, shortlist.len()));
    }
    if !pend.is_empty() {
        let call = w.state.lines().filter(|l| l.starts_with("#borg-predict-cost")).count() + 1;
        record_verdicts(session, input, &pend, &pred.resolved, extra, call);
    }
    let applied: Vec<(&conscience::Challenge, u8)> =
        pred.applies.iter().zip(&pred.grades).filter_map(|(i, g)| shortlist.iter().find(|c| &c.id == i).map(|c| (*c, *g))).collect();
    let ids = |cs: &[&conscience::Challenge]| cs.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
    if graded {
        // Graded (default): ask only for a lesson graded 3; a check without one hands its 2s and 1s over
        // free at the next prompt or touch (conscience.rs `graded`).
        let (ask, free, learned, p) = crate::attention::graded(&applied, &learn::home());
        append(session, &graded_line(&applied));
        append_p(session, applied.iter().zip(&p).filter(|((c, _), _)| ask.iter().any(|a| a.id == c.id)).map(|((c, _), p)| (c.id.as_str(), *p)), None);
        if learning() && !applied.is_empty() {
            let graded = outcome::Outcome {
                kind: outcome::GRADED.into(),
                session: session.into(),
                lessons: applied.iter().map(|(c, _)| c.id.clone()).collect(),
                ms: learn::now_ms(),
                repo: repo_of(input.get("cwd").and_then(Value::as_str).unwrap_or_default()),
                channel: if live { "live" } else { "stop" }.into(),
                grades: applied.iter().map(|(_, g)| *g).collect(),
                ranks: (0..applied.len() as u16).collect(),
                applied: applied.len() as u16,
                chosen: ids(&ask),
                explore: learned.as_ref().is_some_and(|c| c.source == "explore"),
                p: p.iter().map(|x| outcome::round_p(*x)).collect(),
                ..Default::default()
            };
            let _ = outcome::append(&learn::home().join("outcomes.jsonl"), &graded);
        }
        if let Some(cut) = &learned {
            append(session, &crate::attention::line(cut, applied.len()));
        }
        let queue = !free.is_empty();
        log_cost(session, &format!("graded applied={} ask={} free={} {}", applied.len(), ask.len(), free.len(), if queue { "queued" } else { "dropped" }));
        if queue {
            enqueue(session, &free.iter().map(|c| c.id.as_str()).collect::<Vec<_>>().join(","));
        }
        Verdict::Ask(ids(&ask))
    } else {
        // R3b: the model decides whether to ask (at the stop, an extra round); once it does, the questions
        // also carry every other lesson over the low bar (conscience.rs `widen`).
        let predicted: Vec<&conscience::Challenge> = applied.iter().map(|(c, _)| *c).collect();
        let pool = conscience::widen_pool(&conscience::rank_scoped(w.all, text, w.scope_text), &predicted, w.done);
        let widened: Vec<&conscience::Challenge> =
            conscience::in_scope_first(conscience::explored(pool.iter().copied())).into_iter().filter(|c| w.challenges.iter().any(|x| x.id == c.id)).collect();
        append_p_of(session, &pool, &widened);
        Verdict::Ask(ids(&widened))
    }
}

/// Live mode (live.rs), in the PostToolUse hook: counts a state-changing tool call as a change and,
/// when no check runs, something changed since the last one and the debounce has passed, starts `borg
/// check` detached. The hook returns at once; the drone waits for nothing.
fn live_trigger(session: &str, input: &Value, tool: &str, changed: bool) {
    let ctx = state_file(session);
    if let Some(why) = live::unsupported() {
        // Said once per session: the hook would otherwise repeat it after every change.
        if std::fs::OpenOptions::new().write(true).create_new(true).open(ctx.with_extension("live.off")).is_ok() {
            eprintln!("borg: live mode needs the consequence model, which {why} replaces; this session stays in hook mode");
        }
        return;
    }
    if changed {
        live::bump(&ctx, tool);
    }
    let now = learn::now_ms();
    // A paused drone (steer.rs) is waiting for its engineer, not working: nothing to check.
    let home = learn::home();
    if crate::steer::inbox(&home).exists() {
        let started = crate::presence::read(&home, session).map_or(i64::MAX, |p| p.started);
        if crate::steer::pending(&home, session, started, now).paused.is_some() {
            return;
        }
    }
    let lock = live::lock_state(&ctx, now, live::stale_ms(), live::alive);
    if live::decide(live::Mode::Live, lock, live::count(&ctx), live::read_mark(&ctx), now, live::debounce_ms()).is_err() {
        return;
    }
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    // As `end` starts `borg learn`: this binary, detached, inheriting the collective's store and state.
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe)
            .args(["check", "--session", session, "--transcript", &s("transcript_path"), "--cwd", &s("cwd")])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// `borg check` (live.rs): one check of the session's work so far, in the background. Takes the
/// session's lock (or leaves: a check runs), decides again under it, then runs the stop's check on this
/// turn so far: the 3s wait in the `.queued` file as a `live <ids>` line, the 2s and 1s as at the stop.
/// It writes no `#borg-asked` (that happens at delivery, `ask_live`) and no `#borg-skip` (that closes a
/// stop's turn): a gate skip is `#borg-live-skip`. The mark records the changes a completed check covered.
pub fn live_check(h: &dyn Harness, session: &str, input: &Value) {
    let ctx = state_file(session);
    let started = learn::now_ms();
    let Some(_lock) = live::acquire(&ctx, started) else { return };
    let n = live::count(&ctx);
    let mark = live::read_mark(&ctx);
    if live::unsupported().is_some() || live::decide(live::current(), live::Lock::Free, n, mark, started, live::debounce_ms()).is_err() {
        return;
    }
    let turn = turn_id(session);
    let all = lessons(session);
    let state = std::fs::read_to_string(&ctx).unwrap_or_default();
    let message = work_so_far(h, input);
    // Done as at the stop, and also what still waits for delivery: it is decided already.
    let acted = early_acted(session, &all, &message);
    let mut done = listed(&state, "#borg-asked ");
    done.extend(listed(&state, "#borg-early ").into_iter().chain(listed(&state, "#borg-queued ")).filter(|i| acted.contains(*i)));
    let waiting = std::fs::read_to_string(queued_file(session)).unwrap_or_default();
    done.extend(waiting.lines().flat_map(|l| l.strip_prefix("live ").unwrap_or(l).split(',')).map(str::trim).filter(|i| !i.is_empty()));
    let challenges: Vec<conscience::Challenge> = all.iter().filter(|c| !done.contains(c.id.as_str()) && !c.off_system).cloned().collect();
    let context = format!("{state}{message}");
    let scope_text = format!("{}\n{message}", where_now(session));
    let work = Work {
        all: &all,
        challenges: &challenges,
        done: &done,
        state: &state,
        context: &context,
        scope_text: &scope_text,
        message: &message,
        changed: true,
        judge: true,
        budget: None,
        started: std::time::Instant::now(),
    };
    let before = mark.map_or(0, |m| m.covered);
    let covered = match check(session, input, &work, true) {
        // A skipped or failed check covered nothing: the stop decides for itself.
        Verdict::Skip(score) => {
            append(session, &format!("#borg-live-skip {score}"));
            before
        }
        Verdict::Failed => before,
        Verdict::Ask(ids) => {
            if !ids.is_empty() {
                enqueue(session, &format!("live {}", ids.join(",")));
            }
            n
        }
    };
    live::write_mark(&ctx, live::Mark { covered, started_ms: started, turn: Some(turn) });
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
    // An adapter that will not wait for the answer (`"context": false`), or a harness whose model never
    // sees a prompt's context, gets none: what is handed over here is marked delivered and would be lost.
    let out = (h.caps().prompt_context && input.get("context") != Some(&Value::Bool(false))).then(|| {
        let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
        let ask = queen_ask(h, session, cwd);
        let playbook =
            learning().then(|| crate::playbook::for_prompt(&learn::home(), session, &crate::queen::repo_name(cwd), prompt, learn::now_ms())).flatten();
        let extra: Vec<String> = [playbook, ask, vetoed.as_deref().map(crate::veto::render)].into_iter().flatten().collect();
        free_context_with(h, "UserPromptSubmit", session, input, prompt, (!extra.is_empty()).then(|| extra.join("\n\n")))
    });
    // A new turn's work starts, after a live ask handed over here took the last turn's as its evidence.
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
    // No transcript (omp, opencode): the turn's tool calls are the work so far a live check reads.
    if harness::transcript(h.caps(), input).is_none()
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(turn_file(session))
    {
        let _ = writeln!(f, "[{tool}] {touch}");
        if let Some(edit) = &edit {
            let _ = writeln!(f, "{edit}");
        }
    }
    // Live mode (live.rs): a change starts a check of the work so far, in the background. Any later
    // tool call starts one too while changes wait unchecked, so a change inside the debounce is
    // checked once it has passed, not only at the stop.
    if live::current() == live::Mode::Live {
        live_trigger(session, input, tool, prefilter::changes(tool, tool_input));
    }
    // Context only where the harness's model reads it after a tool call; otherwise it waits for the next
    // prompt or the stop.
    h.caps().tool_context.then(|| free_context(h, "PostToolUse", session, input, &touch)).flatten()
}

/// Live mode at the stop: a check still running finishes first (bounded), so this stop reads what it
/// wrote. Then the stop holds the session's lock itself (`guard`, for the whole stop), so no check
/// starts beside it; when a check still holds it, the stop leaves the pending lessons to that check's
/// judge. The wait and the model call share the harness's budget (omp gives a stop 30 s).
struct StopLive {
    on: bool,
    budget: Option<std::time::Duration>,
    lock: live::Lock,
    guard: Option<live::Guard>,
    /// A live check's 3s that no tool call delivered yet: this stop asks them (it would have).
    waiting: Vec<String>,
}

impl StopLive {
    fn take(h: &dyn Harness, session: &str) -> StopLive {
        let on = live::current() == live::Mode::Live;
        let budget = crate::expert_route::stop_budget(h);
        let lock = if on { live::wait_free(&state_file(session), live::stop_wait(budget).as_millis() as u64) } else { live::Lock::Free };
        let guard = if on && lock != live::Lock::Held { live::acquire(&state_file(session), learn::now_ms()) } else { None };
        let lock = if on && guard.is_none() { live::Lock::Held } else { lock };
        let waiting = if on { take_live(session) } else { Vec::new() };
        StopLive { on, budget, lock, guard, waiting }
    }
}

fn on_stop(h: &dyn Harness, session: &str, input: &Value) -> Option<Value> {
    let started = std::time::Instant::now();
    if learning() {
        // The drone's latest proposal, for the reasoning note of what it commits (notes.rs).
        crate::notes::remember_proposal(&crate::notes::state_dir(), session, &full_proposal(h, input));
        crate::notes::mark(&crate::notes::state_dir(), session, "stop");
    }
    let second_round = input.get("stop_hook_active").and_then(Value::as_bool) == Some(true);
    if std::env::var("BORG_CRITIC").as_deref() == Ok("generic") {
        return (!second_round).then(|| json!({"decision": "block", "reason": conscience::GENERIC}));
    }
    let all = lessons(session);
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
    let live = StopLive::take(h, session);
    let state = std::fs::read_to_string(state_file(session)).unwrap_or_default();
    // A lesson is asked once per session: the drone answered it at that turn end, and asking it again
    // at every later one is noise (and a model call spent on it). A lesson delivered early
    // (`#borg-early`) is not asked again either.
    let mut done = std::collections::HashSet::new();
    // R3b: an early lesson the drone acted on is done; one it did not act on may still be asked.
    let proposal = full_proposal(h, input);
    let acted = early_acted(session, &all, &proposal);
    if std::env::var_os("BORG_ONLY").is_none() {
        done = listed(&state, "#borg-asked ");
        done.extend(listed(&state, "#borg-early ").into_iter().chain(listed(&state, "#borg-queued ")).filter(|i| acted.contains(*i)));
    }
    // Lessons of other systems (systems.rs) are neither shortlisted nor asked.
    let challenges: Vec<conscience::Challenge> = all.iter().filter(|c| !done.contains(c.id.as_str()) && !c.off_system).cloned().collect();
    let message = input.get("last_assistant_message").and_then(Value::as_str).unwrap_or_default();
    let context = format!("{state}{message}");
    // Scopes at Stop: where the session works plus this turn's own message, not the whole history.
    let scope_text = format!("{}\n{message}", where_now(session));
    let work = Work {
        all: &all,
        challenges: &challenges,
        done: &done,
        state: &state,
        context: &context,
        scope_text: &scope_text,
        message: &proposal,
        changed,
        judge: !live.on || live.guard.is_some(),
        budget: live.budget,
        started,
    };
    match stop_select(h, session, input, &work, &live) {
        Ok(selected) => stop_ask(h, session, input, selected),
        Err(out) => out,
    }
}

/// The lessons this stop asks; `Err` with the hook's answer when it asks none (a gate skip still lets
/// the queen ask).
fn stop_select<'a>(h: &dyn Harness, session: &str, input: &Value, w: &Work<'a>, live: &StopLive) -> Result<Vec<&'a conscience::Challenge>, Option<Value>> {
    match std::env::var("BORG_ONLY") {
        // BORG_ONLY: eval oracle, perfect retrieval = exactly these challenge ids.
        Ok(ids) => Ok(w.challenges.iter().filter(|c| ids.split(',').any(|i| i == c.id)).collect()),
        // Word matching alone always finds *some* lesson, so it asked at every turn end; the
        // consequence model may abstain, and it is the mechanism the sealed eval measured.
        Err(_) if std::env::var("BORG_MATCH").as_deref() != Ok("bm25") => stop_check(h, session, input, w, live),
        // R3b (conscience.rs `budget`): block only for a lesson near the best match not yet done;
        // then ask everything over the low bar, in-scope first.
        Err(_) => {
            let pool = conscience::budget_pool(&conscience::rank_scoped(w.all, &conscience::work_text(w.context), w.scope_text), w.done);
            let asked: Vec<&conscience::Challenge> = conscience::in_scope_first(conscience::explored(pool.iter().copied()))
                .into_iter()
                .filter_map(|c| w.challenges.iter().find(|x| x.id == c.id))
                .collect();
            append_p_of(session, &pool, &asked);
            Ok(asked)
        }
    }
}

/// The consequence model's verdict at the stop, unless a live check covered the turn; a live check's
/// waiting asks join it.
fn stop_check<'a>(h: &dyn Harness, session: &str, input: &Value, w: &Work<'a>, live: &StopLive) -> Result<Vec<&'a conscience::Challenge>, Option<Value>> {
    let skip = |why: &str| {
        append(session, &format!("#borg-skip {why}"));
        stat(session, input, crate::stats::GATE_SKIP, |e| e.why = why.to_string());
        let prompts = std::fs::read_to_string(prompts_file(session)).unwrap_or_default();
        Err(crate::queen::stop(session, input, &|| harness::plan(h.caps(), input), &prompts, &[]))
    };
    let waiting: Vec<&conscience::Challenge> = live.waiting.iter().filter_map(|i| w.challenges.iter().find(|c| &c.id == i)).collect();
    // Live mode: a check that started in this turn covered every change and none came since: its
    // verdict stands (its waiting asks are asked now), and this stop costs no model call (live.rs `covered`).
    let ctx = state_file(session);
    if live.on && live::covered(live.lock, live::count(&ctx), live::read_mark(&ctx), turn_id(session)) {
        return if waiting.is_empty() { skip("live_covered") } else { Ok(waiting) };
    }
    let mut asked: Vec<&conscience::Challenge> = match check(session, input, w, false) {
        Verdict::Skip(_) | Verdict::Failed if !waiting.is_empty() => Vec::new(),
        Verdict::Skip(score) => return skip(&score),
        Verdict::Failed => return Err(None),
        Verdict::Ask(ids) => ids.iter().filter_map(|i| w.challenges.iter().find(|c| &c.id == i)).collect(),
    };
    for c in waiting {
        if !asked.iter().any(|a| a.id == c.id) {
            asked.push(c);
        }
    }
    Ok(asked)
}

/// `#borg-graded n=<applied> <id>:<grade>:<rank>,…`: a graded check's applies in the model's order, so
/// the `asked` outcome can carry each asked lesson's grade and rank (`graded_of`).
fn graded_line(applied: &[(&conscience::Challenge, u8)]) -> String {
    let items: Vec<String> = applied.iter().enumerate().map(|(r, (c, g))| format!("{}:{g}:{r}", c.id)).collect();
    format!("#borg-graded n={} {}", applied.len(), items.join(","))
}

/// `#borg-p <id>=<p>,…[ @<check>]`: the propensity (outcome.rs `p`) of each lesson a check chose to ask,
/// so every entry is above 0 and the latest entry per id is the check that asked it; the `asked`
/// outcome carries them (`propensity_of`). `check` names the pool line the draw was over (`append_p_of`);
/// none for a graded or learned-cut check. Nothing when no lesson was chosen.
fn append_p<'a>(session: &str, chosen: impl IntoIterator<Item = (&'a str, f64)>, check: Option<&str>) {
    let items: Vec<String> = chosen.into_iter().map(|(id, p)| format!("{id}={}", outcome::round_p(p))).collect();
    if !items.is_empty() {
        let tag = check.map_or_else(String::new, |c| format!(" @{c}"));
        append(session, &format!("#borg-p {}{tag}", items.join(",")));
    }
}

/// `append_p` for lessons `budget`/`widen` asked from `pool`: their chance of a slot under the
/// session's posterior (conscience.rs `propensity`). Then the pool itself, `#borg-pool
/// <id>=<base>[:s],… @<check>` in ranking order (`:s` a scoped lesson, base conscience.rs `pool_base`,
/// unrounded), so the `asked` record can carry it (`pool_of`) and `borg debug ope` can replay the draw. Both
/// lines carry the same `check` (a hash of the pool line and its p line), which ties the p of an asked
/// lesson to the pool its draw was over.
fn append_p_of(session: &str, pool: &[(f64, &conscience::Challenge)], asked: &[&conscience::Challenge]) {
    if asked.is_empty() {
        return;
    }
    let p = conscience::propensity(pool, session);
    let chosen: Vec<(&str, f64)> = asked.iter().filter_map(|c| p.get(&c.id).map(|p| (c.id.as_str(), *p))).collect();
    let items: Vec<String> =
        pool.iter().map(|(s, c)| format!("{}={}{}", c.id, conscience::pool_base(*s, c), if c.scope.is_empty() { "" } else { ":s" })).collect();
    let items = items.join(",");
    let mut h = blake3::Hasher::new();
    h.update(items.as_bytes());
    for (id, p) in &chosen {
        h.update(format!("\0{id}={}", outcome::round_p(*p)).as_bytes());
    }
    let check = h.finalize().to_hex()[..12].to_string();
    append_p(session, chosen, Some(&check));
    append(session, &format!("#borg-pool {items} @{check}"));
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
fn pool_of(session: &str, ids: &[String]) -> (Vec<String>, Vec<f64>, Vec<bool>) {
    pool_in(&std::fs::read_to_string(state_file(session)).unwrap_or_default(), ids)
}

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

/// For each of `ids`, its propensity in the session's latest `#borg-p` entry for it; empty when any of
/// them has none (unknown, never a guess).
fn propensity_of(session: &str, ids: &[String]) -> Vec<f64> {
    propensity_in(&std::fs::read_to_string(state_file(session)).unwrap_or_default(), ids)
}

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

/// The stop asks `selected`: one block (the queen's, when it asks too), recorded as asked.
fn stop_ask(h: &dyn Harness, session: &str, input: &Value, mut selected: Vec<&conscience::Challenge>) -> Option<Value> {
    // Shown in an order stable within the session but different across sessions: no lesson always
    // sits first (position bias; memory.rs `shuffle`).
    crate::memory::shuffle(&mut selected, session);
    let log = selected.iter().map(|c| c.id.as_str()).collect::<Vec<_>>().join(",");
    append(session, &format!("#borg-asked {log}"));
    if !selected.is_empty()
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(edits_file(session))
    {
        // The blocked message is the work before the questions (R1 `pending`).
        let _ = writeln!(f, "{}\n#borg-asked {log}", one_line(&full_proposal(h, input), 4000));
    }
    // L4 (queen.rs): a rare, budgeted question to the engineer when the conscience had nothing
    // or its lessons contradict each other (its block then carries the lessons too).
    let prompts = std::fs::read_to_string(prompts_file(session)).unwrap_or_default();
    let queen = crate::queen::stop(session, input, &|| harness::plan(h.caps(), input), &prompts, &selected);
    if selected.is_empty() {
        return queen;
    }
    if learning() {
        // Outcome tracking (outcome.rs): `borg learn` later sees whether a correction followed.
        let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
        let lessons: Vec<String> = selected.iter().map(|c| c.id.clone()).collect();
        let (grades, ranks, applied) = graded_of(session, &lessons);
        let p = propensity_of(session, &lessons);
        let (pool, base, scoped) = pool_of(session, &lessons);
        let asked = outcome::Outcome {
            kind: outcome::ASKED.into(),
            session: session.into(),
            lessons,
            ms: learn::now_ms(),
            repo: repo_of(cwd),
            groups: conscience::disagreement_ids(&selected),
            channel: "stop".into(),
            grades,
            ranks,
            applied,
            p,
            pool,
            base,
            scoped,
            ..Default::default()
        };
        let _ = outcome::append(&learn::home().join("outcomes.jsonl"), &asked);
    }
    let out = queen.or_else(|| Some(json!({"decision": "block", "reason": conscience::render(&selected)})));
    let chars = out.as_ref().and_then(|o| o["reason"].as_str()).map_or(0, str::len);
    log_cost(session, &format!("block questions={} chars={chars}", selected.len()));
    stat(session, input, crate::stats::BLOCK, |e| {
        e.n = selected.len() as u64;
        e.lessons = selected.iter().map(|c| c.id.clone()).collect();
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
    // The delivery mode (live.rs): BORG_MODE, else the collective's `mode`, else live. Exported as the
    // mode this drone runs in, which the statistics record.
    let mode = live::of(cfg.as_ref().ok().and_then(Option::as_ref), &name).mode;
    // SAFETY: single-threaded at this point; everything below (and the background `borg learn`
    // or `borg check` it spawns) inherits the collective's own store, scratch state and mode.
    unsafe {
        std::env::set_var("BORG_HOME", &home);
        std::env::set_var("BORG_MODE", mode.name());
        // borg's own model calls go through the drone's harness (runner.rs); `borg check` reads it too.
        std::env::set_var("BORG_HARNESS", h.name());
        if std::env::var_os("BORG_STATE").is_none() {
            std::env::set_var("BORG_STATE", home.join("state"));
        }
    }
    // Who is working where (presence.rs, for `borg top`): one small atomic write per hook.
    let now = learn::now_ms();
    // A session's start and a halted turn (hooks of an older install) read and write nothing.
    if matches!(event, "start" | "halted") {
        return;
    }
    // Not at pre_tool (every tool call): there only steering reads, once someone steered.
    let session = input.get("session_id").and_then(Value::as_str).unwrap_or("unknown");
    let me = match event {
        "pre_tool" => {
            if !crate::steer::inbox(&home).exists() {
                return;
            }
            crate::presence::read(&home, session)
        }
        _ => crate::presence::touch(&home, &name, h.name(), event, &input, now),
    };
    // Signed steering (steer.rs) wraps the answer: shown once, and a pause holds tools and the stop.
    // At a stop, the routed collective's experts join the conscience's block (expert_route.rs).
    let started = me.map(|p| p.started);
    let out = crate::steer::around(&home, h.caps().steer_in_context, event, &input, started, now, || {
        crate::expert_route::checkpoint(h, event, &input, || dispatch(h, event, &input))
    });
    // Messages from other sessions (msg.rs) join a prompt's or tool call's context, each once.
    if let Some(out) = crate::msg::deliver(&home, h.caps(), event, &input, started, now, out) {
        println!("{out}");
    }
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
    fn propensity_of_takes_the_latest_entry_and_nothing_when_one_is_missing() {
        let state = "#borg-p A=1,B=0.25\n#borg-asked A,B\n#borg-p B=0.5,C=0.1\n";
        let ids = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(propensity_in(state, &ids(&["B", "A"])), [0.5, 1.0]);
        assert_eq!(propensity_in(state, &ids(&["C"])), [0.1]);
        assert!(propensity_in(state, &ids(&["A", "D"])).is_empty(), "D unknown: no guess");
        assert!(propensity_in("", &ids(&["A"])).is_empty());
    }

    #[test]
    fn pool_of_takes_the_pool_of_the_check_that_logged_each_p() {
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

    /// Track R's merged ask: a live check drew A and B from one pool, a stop check drew A again from
    /// another; the stop merges the waiting B into its ask of A. Their p come from two draws, so the
    /// record carries no pool (counted as no_pool by `borg debug ope`), never one draw's pool with both p.
    #[test]
    fn a_merged_ask_of_two_draws_carries_no_pool() {
        let session = format!("pool-merge-{}", std::process::id());
        let _ = std::fs::remove_file(state_file(&session));
        let c = |id: &str| conscience::Challenge { id: id.into(), ..Default::default() };
        let (a, b, cc, d, e, f) = (c("A"), c("B"), c("C"), c("D"), c("E"), c("F"));
        let live: Vec<(f64, &conscience::Challenge)> = vec![(1.0, &a), (0.9, &b), (0.8, &cc), (0.7, &d)];
        append_p_of(&session, &live, &[&a, &b]);
        let stop: Vec<(f64, &conscience::Challenge)> = vec![(1.0, &a), (0.9, &e), (0.8, &f)];
        append_p_of(&session, &stop, &[&a]);
        let merged = vec!["A".to_string(), "B".to_string()];
        assert_eq!(propensity_of(&session, &merged).len(), 2, "each id has its latest p");
        assert_eq!(pool_of(&session, &merged), Default::default());
        assert_eq!(pool_of(&session, &["A".to_string()]).0, ["A", "E", "F"], "A alone: the stop check's pool");
        assert_eq!(pool_of(&session, &["B".to_string()]).0, ["A", "B", "C", "D"], "B alone: the live check's");
        let _ = std::fs::remove_file(state_file(&session));
    }
}
