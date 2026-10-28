//! Plan approval and intent cards (docs/design/hive.md "The engineer's role", C1;
//! L7b): the engineer approves a goal's plan once, through the queen, before
//! `borg debug hive run` starts a drone, and a task the plan marks high-risk escalates once when it is claimed.
//!
//! - **Intent cards** ([`cards`]): one per work or plan task (a fix task an integration opened is the
//!   approved scope's, never a card): its intent (the title, redacted when it was written), files,
//!   deps, checks and a risk class. `high` when the queen's classifier (queen.rs `classify`, the four
//!   checkpoint kinds, deterministic, 0 tokens) finds a high-consequence move in the title: the card
//!   names the kind. `low` when every file in scope is docs or tests (hive.md's auto-approved
//!   class). `normal` otherwise. Only `high` routes anything; `low` and `normal` are shown.
//! - **The plan question** ([`gate`]): `borg debug hive run` raises one `Kind::Plan` queen question whose
//!   payload holds the cards, keyed by the goal and the cards ([`key`]), and waits: the engineer answers
//!   it at the run's terminal (`y`, or why not), with `borg queen answer`, or wherever the queue is
//!   answered. Until then no worktree is made, no run line written and nothing claimed. An approval
//!   starts the drones; anything else, a dismissal included, stops the run. `--yes` is the engineer
//!   approving at the command line: the run neither asks nor waits (borg-eval's runs pass it). A plan
//!   decided once is not asked again: a rerun of the same cards goes ahead (or stops) at once; a new
//!   task makes new cards and a new question.
//! - **The decision** ([`ratify`], from queen.rs `answer`): a line of `plan.jsonl` (this node's, never
//!   synced), which `borg top`, `/api/hive` and the Hive tab show ([`row`]). A "no" in the engineer's
//!   own words is a veto (veto.rs `record`): an episode `borg learn` turns into a lesson, and the `veto`
//!   stat. A bare "no" teaches nothing and records only the decision.
//! - **Escalation at claim** ([`escalate`]): the first claim of a high-risk task queues one question of
//!   its card's kind (an escalation the queen's attention reward counts, escalation.rs), naming the
//!   claiming session; its drone is told once and goes on with the kind's default (an external
//!   mutation holds). Never asked again for the task, whoever claims it later.
//!
//! A drone never answers either question (queen.rs `answer` refuses under `BORG_HIVE`), and none is
//! delivered to a drone's session.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Board, Goal, Role, Task, TaskId};
use crate::queen::{self, Entry, Kind};
use crate::{learn, stats, veto};

/// The plan question's answers: the default starts nothing.
pub const APPROVE_OPTION: &str = "approve: start the drones";
pub const REFUSE_OPTION: &str = "no: start nothing and claim nothing";
/// How often a waiting run looks at the queue.
const POLL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Low,
    Normal,
    High,
}

impl Risk {
    pub fn name(self) -> &'static str {
        match self {
            Risk::Low => "low",
            Risk::Normal => "normal",
            Risk::High => "high",
        }
    }
}

/// One task as the engineer approves it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub task: TaskId,
    pub intent: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub deps: Vec<TaskId>,
    /// `name=cmd` of each of its own checks; empty: the goal's oracle.
    #[serde(default)]
    pub checks: Vec<String>,
    pub risk: Risk,
    /// Why it has its class: the kind's label, "docs and tests only" or "ordinary work".
    pub why: String,
    /// The checkpoint kind of a high-risk task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
}

/// A path of docs or tests: a docs extension or directory, a test directory, or a test file's name.
fn docs_or_tests(path: &str) -> bool {
    let p = path.to_lowercase();
    let name = p.rsplit('/').next().unwrap_or_default();
    let dirs = p.split('/').rev().skip(1).any(|d| ["docs", "doc", "test", "tests", "spec", "__tests__"].contains(&d));
    let ext = [".md", ".rst", ".adoc"].iter().any(|e| name.ends_with(e));
    let test = name.starts_with("test_") || ["_test.", ".test.", "_spec.", ".spec."].iter().any(|t| name.contains(t));
    dirs || ext || test
}

/// The card of task `t`.
pub fn card(t: &Task) -> Card {
    let kind = queen::classify(&t.title, &t.repo).first().map(|d| d.kind);
    let (risk, why) = match kind {
        Some(k) => (Risk::High, k.label().to_string()),
        None if !t.files.is_empty() && t.files.iter().all(|f| docs_or_tests(f)) => (Risk::Low, "docs and tests only".into()),
        None => (Risk::Normal, "ordinary work".into()),
    };
    Card {
        task: t.id.clone(),
        intent: t.title.clone(),
        files: t.files.clone(),
        deps: t.deps.clone(),
        checks: t.accept.iter().map(|c| format!("{}={}", c.name, c.cmd)).collect(),
        risk,
        why,
        kind,
    }
}

/// The plan of board `b`: a card per work or plan task, fix tasks left out, in id order.
pub fn cards(b: &Board) -> Vec<Card> {
    b.tasks.values().filter(|s| super::board::needs_plan(&s.task)).map(|s| card(&s.task)).collect()
}

/// The cards of `b` the engineer has not approved yet (board.rs `Board::plan_approved`): the plan an
/// interactive goal still waits on (phase g G5).
pub fn unratified(b: &Board) -> Vec<Card> {
    b.tasks.values().filter(|s| !b.plan_approved(&s.task)).map(|s| card(&s.task)).collect()
}

/// The plan's decision key: the goal and every card's task, intent, files, deps and checks. The same
/// plan is one decision; another task, or a changed one, is a new plan.
pub fn key(goal: &str, cards: &[Card]) -> String {
    let what: Vec<String> =
        cards.iter().map(|c| format!("{}|{}|{}|{}|{}", c.task, c.intent, c.files.join(","), c.deps.join(","), c.checks.join(","))).collect();
    queen::decision_key(Kind::Plan, goal, &what.join(";"))
}

/// One card as one line.
pub fn render_card(c: &Card) -> String {
    let mut s = format!("{}  {}", c.task, c.intent);
    if !c.files.is_empty() {
        s.push_str(&format!(" · files {}", c.files.join(", ")));
    }
    if !c.deps.is_empty() {
        s.push_str(&format!(" · after {}", c.deps.join(", ")));
    }
    if !c.checks.is_empty() {
        s.push_str(&format!(" · checks {}", c.checks.join("; ")));
    }
    format!("{s} · risk {} ({})", c.risk.name(), c.why)
}

/// A plan decided: a line of `plan.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub v: u8,
    pub ms: i64,
    pub key: String,
    pub approved: bool,
    pub by: String,
    /// `queen` (an answer to the plan question, the run's terminal included), `yes` (`--yes`) or `ratify`
    /// (`borg hive ratify`, phase g G5).
    pub via: String,
    #[serde(default)]
    pub answer: String,
    /// The plan question's id; empty for `--yes` without one.
    #[serde(default)]
    pub question: String,
    pub tasks: usize,
    pub high: usize,
}

pub fn path(home: &Path, goal: &str) -> PathBuf {
    super::dir(home, goal).join("plan.jsonl")
}

/// The latest decision of plan `key` of `goal`.
pub fn decided(home: &Path, goal: &str, key: &str) -> Option<Decision> {
    super::read_lines::<Decision>(&path(home, goal)).into_iter().rfind(|d| d.key == key)
}

/// Whether an answer to the plan question approves it: it starts with the approve option's word, or
/// says only yes.
pub fn approves(answer: &str) -> bool {
    let a = answer.trim().to_lowercase();
    a.starts_with("approve") || crate::escalation::yes(&a)
}

/// The reason in a "no": what is left of the answer without a leading no-word, when it is more than
/// the option or a bare "no".
fn reason(answer: &str) -> Option<String> {
    let a = answer.trim();
    if a.eq_ignore_ascii_case(REFUSE_OPTION) {
        return None;
    }
    // The no-words are ASCII: matched on the answer itself, never on a lowercased copy whose byte
    // offsets differ (`'\u{212A}'.to_lowercase()` is one byte shorter).
    let rest = ["no", "nope", "n"]
        .iter()
        .find_map(|w| {
            a.get(..w.len()).filter(|p| p.eq_ignore_ascii_case(w)).map(|_| &a[w.len()..]).filter(|r| r.is_empty() || !r.starts_with(char::is_alphanumeric))
        })
        .unwrap_or(a);
    let rest = rest.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
    (rest.chars().count() >= 4).then(|| rest.to_string())
}

fn record(home: &Path, goal: &str, d: &Decision) -> Result<(), String> {
    super::append_line(&path(home, goal), d).map_err(|e| e.to_string())?;
    stats::count(home, stats::HIVE_PLAN, &plan_session(goal), |ev| {
        ev.ms = d.ms;
        ev.engineer = d.by.clone();
        ev.why = goal.into();
        ev.n = u64::from(d.approved);
        ev.text = stats::excerpt(&format!("{} via {}: {}", if d.approved { "approved" } else { "refused" }, d.via, d.answer), 160);
    });
    Ok(())
}

/// The session a plan's question and veto name: the goal's plan, no drone's.
fn plan_session(goal: &str) -> String {
    format!("hive-plan-{goal}")
}

/// The answer to a plan question (queen.rs `answer`): its decision in `plan.jsonl`, and a veto for a
/// "no" with a reason (learning on). Returns whether it approves.
///
/// # Errors
/// A question without its plan, or a write failure.
pub fn ratify(store: &Path, e: &Entry, answer: &str, author: &str, now: i64) -> Result<bool, String> {
    let p = e.payload.as_ref().ok_or_else(|| format!("{} carries no plan", e.id))?;
    let goal = p["goal"].as_str().filter(|g| super::goal::valid_id(g)).ok_or_else(|| format!("{}: no valid goal in its plan", e.id))?;
    // The budget question (`ask_budget`): a raise is a line of the goal's board, every node's budget.
    if let Some(spent) = p["budget"].as_f64() {
        return match raise_of(answer, spent) {
            Some(usd) => raise_budget(store, goal, usd, author, now).map(|_| true).map_err(|err| err.to_string()),
            None => Ok(false),
        };
    }
    let cards: Vec<Card> = serde_json::from_value(p["cards"].clone()).map_err(|err| format!("{}: unreadable plan ({err})", e.id))?;
    let approved = approves(answer);
    // An interactive goal's plan (`ask_opener`): approved, its tasks open to the drones on every node.
    if approved && p["interactive"] == true {
        let ids: Vec<TaskId> = cards.iter().map(|c| c.task.clone()).collect();
        ratify_board(store, goal, &ids, author, now).map_err(|err| err.to_string())?;
    }
    let d = Decision {
        v: 1,
        ms: now,
        key: e.decision_key.clone(),
        approved,
        by: author.into(),
        via: "queen".into(),
        answer: answer.trim().into(),
        question: e.id.clone(),
        tasks: cards.len(),
        high: cards.iter().filter(|c| c.risk == Risk::High).count(),
    };
    record(store, goal, &d)?;
    if let Some(why) = reason(answer).filter(|_| !approved && learn::learning()) {
        let spec = super::goal::read(store, goal).map(|g| g.spec).unwrap_or_default();
        let task = format!("the plan of hive goal {goal}: {}", stats::excerpt(&spec, 300));
        let plan: Vec<String> = cards.iter().map(render_card).collect();
        let session = plan_session(goal);
        let said = veto::Said { session: &session, repo: &e.repo, engineer: author, task: &task, agent: &plan.join("\n"), reason: &why, ms: now, via: "plan" };
        veto::record(store, &said).map_err(|err| err.to_string())?;
    }
    Ok(approved)
}

/// Says on `goal`'s board that the engineer `author` approved the plan of `tasks` (board.rs
/// `ratified_note`), under the board lock, naming only those not approved yet: none, no line. Every
/// waiting interactive drone's wait wakes on the line (its board file changed) and may claim them.
/// The number of tasks it approved now.
///
/// # Errors
/// The board cannot be written.
pub fn ratify_board(home: &Path, goal: &str, tasks: &[TaskId], author: &str, now: i64) -> std::io::Result<usize> {
    let here = super::node(home);
    let mut n = 0;
    super::write_if(home, goal, |events| {
        let b = super::board::fold_on(events, &here);
        let new: Vec<TaskId> = tasks.iter().filter(|t| !b.ratified.contains(*t)).cloned().collect();
        n = new.len();
        (!new.is_empty())
            .then(|| vec![super::Event::new(super::Ev::Heartbeat, "", &format!("engineer:{author}"), now).with_note(&super::board::ratified_note(&new))])
    })?;
    Ok(n)
}

/// The decision of `borg hive ratify` (mod.rs `ratify_cmd`) for `cards`, in plan.jsonl: `via` `ratify`,
/// shown as `borg debug hive run`'s and the queen's decisions are (`row`).
pub fn note_ratified(home: &Path, goal: &str, cards: &[Card], author: &str, now: i64) {
    let d = Decision {
        v: 1,
        ms: now,
        key: key(goal, cards),
        approved: true,
        by: author.into(),
        via: "ratify".into(),
        answer: String::new(),
        question: String::new(),
        tasks: cards.len(),
        high: cards.iter().filter(|c| c.risk == Risk::High).count(),
    };
    let _ = record(home, goal, &d);
}

/// Says on `goal`'s board that the engineer `author` raised its budget to `usd` (board.rs `budget_note`).
///
/// # Errors
/// The board cannot be written.
pub fn raise_budget(home: &Path, goal: &str, usd: f64, author: &str, now: i64) -> std::io::Result<()> {
    super::write(home, goal, vec![super::Event::new(super::Ev::Heartbeat, "", &format!("engineer:{author}"), now).with_note(&super::board::budget_note(usd))])
        .map(drop)
}

/// The budget an answer to "raise the budget?" sets, the goal having `spent` so far: the first number
/// standing as a word of its own (`20`, `$12.5`, `20.`), when it is above what was spent; a plain yes
/// doubles it; a refusal ("no, 20 dollars is too much", `refuses`) and anything else raise nothing.
fn raise_of(answer: &str, spent: f64) -> Option<f64> {
    if refuses(answer) {
        return None;
    }
    let number = answer.split_whitespace().find_map(|w| {
        let w = w.trim_end_matches(['.', ',', ';', ':', '!', '?', ')']).trim_start_matches('(');
        let w = w.strip_prefix('$').unwrap_or(w);
        w.strip_suffix('$').unwrap_or(w).parse::<f64>().ok()
    });
    match number {
        Some(n) => (n.is_finite() && n > spent).then_some(n),
        None if approves(answer) || answer.trim().to_lowercase().starts_with("raise") => Some((spent * 2.0).max(1.0)),
        None => None,
    }
}

/// The answers of the budget question.
pub const RAISE_OPTION: &str = "raise: to twice what is spent (or name the dollars)";
pub const KEEP_OPTION: &str = "no: keep the budget, claim nothing more";

/// The one question of a goal whose metered spend reached its budget (phase g G5, `spent` of `budget`
/// dollars): "raise the budget of G?", once per budget (a raise makes a new budget and, once it is spent,
/// a new question). Interactive drones claim nothing of the goal meanwhile (mod.rs `next_open`).
pub fn ask_budget(home: &Path, g: &Goal, spent: f64, budget: f64, cwd: &str, now: i64) {
    let key = queen::decision_key(Kind::Plan, &g.id, &format!("budget {budget}"));
    let e = Entry {
        id: format!("q{}", &key[..8]),
        decision_key: key,
        kind: Kind::Plan,
        question: format!(
            "Hive goal {}: its drones spent ${spent:.2} of its ${budget:.2} budget (metered sessions only) and claim nothing more. Raise the budget of {}?",
            g.id, g.id
        ),
        options: vec![RAISE_OPTION.into(), KEEP_OPTION.into()],
        default: KEEP_OPTION.into(),
        evidence: Vec::new(),
        session: opener(home, &g.id).unwrap_or_else(|| plan_session(&g.id)),
        cwd: cwd.into(),
        repo: queen::repo_name(cwd),
        engineer: learn::engineer(cwd),
        ms: now,
        answer: None,
        answered_by: None,
        answered_ms: None,
        disagreeing: Vec::new(),
        dismissed: None,
        dismissed_ms: None,
        payload: Some(json!({"goal": g.id, "budget": spent, "interactive": true})),
    };
    // The opener's prompts are read for the answer (`at_prompt`), its plan's marker long removed.
    if queen::ask_once(home, &e).is_ok() && !e.session.starts_with("hive-plan-") {
        let _ = std::fs::write(opener_path(&e.session), &g.id);
    }
}

/// Whether `e` is a plan or budget question of an interactive goal (`ask_opener`, `ask_budget`): answered
/// at the engineer's own prompt in the session that opened the goal, or from a terminal, never by a
/// command a session's model runs (queen.rs `main`), and never delivered to another session (queen.rs
/// `deliver`).
pub fn at_own_prompt(e: &Entry) -> bool {
    e.kind == Kind::Plan && e.payload.as_ref().is_some_and(|p| p["interactive"] == true)
}

/// The marker of a session that opened an interactive goal's plan: its prompts are read for the
/// engineer's answer (`at_prompt`); a session without it costs one failed stat.
fn opener_path(session: &str) -> PathBuf {
    crate::hook::state_file(session).with_extension("hive-plan")
}

/// The session that opened goal `goal`'s newest plan question, if any.
fn opener(home: &Path, goal: &str) -> Option<String> {
    queen::entries(home)
        .into_iter()
        .rev()
        .find(|e| at_own_prompt(e) && e.payload.as_ref().is_some_and(|p| p["goal"] == goal && p["cards"].is_array()))
        .map(|e| e.session)
        .filter(|s| !s.starts_with("hive-plan-"))
}

/// The plan question of an interactive goal `g` (board `b`) opened by `session` in `cwd` (`borg hive
/// init` or `open` run by its model, phase g G5): one `Kind::Plan` question of the cards not approved yet,
/// naming the session, which the engineer answers at their own prompt there (`at_prompt`). An older
/// open question of the goal's plan is dismissed: the plan it asked about changed. None with nothing to
/// approve.
pub fn ask_opener(home: &Path, g: &Goal, b: &Board, session: &str, cwd: &str, now: i64) -> Option<(Entry, Vec<Card>)> {
    let cards = unratified(b);
    if cards.is_empty() {
        return None;
    }
    let key = key(&g.id, &cards);
    let mut e = question(g, &cards, &key, &queen::repo_name(cwd), cwd, &learn::engineer(cwd), now);
    e.session = session.into();
    e.question = format!(
        "Hive goal {}: approve its plan? No drone takes a task of it before. {} task(s): {}",
        g.id,
        cards.len(),
        stats::excerpt(
            &cards.iter().map(|c| format!("{} \"{}\" ({})", c.task, stats::excerpt(&c.intent, 80), c.risk.name())).collect::<Vec<_>>().join("; "),
            600
        )
    );
    e.payload = Some(json!({"goal": g.id, "cards": cards, "interactive": true}));
    for old in queen::open(home)
        .into_iter()
        .filter(|o| o.decision_key != key && at_own_prompt(o) && o.payload.as_ref().is_some_and(|p| p["goal"] == g.id.as_str() && p["cards"].is_array()))
    {
        let _ = queen::dismiss(home, &old.id, &format!("the plan changed: {} asks it again", e.id), now);
    }
    queen::ask_once(home, &e).ok()?;
    // Queued now or before and still open: a plan asked and answered once is not asked again.
    let e = queen::open(home).into_iter().find(|o| o.decision_key == key)?;
    let _ = std::fs::write(opener_path(session), &g.id);
    Some((e, cards))
}

/// Whether an answer refuses: it starts with a no-word ("no", "nope", "n") on its own.
fn refuses(answer: &str) -> bool {
    let a = answer.trim();
    ["no", "nope", "n"]
        .iter()
        .any(|w| a.get(..w.len()).is_some_and(|p| p.eq_ignore_ascii_case(w)) && a[w.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric()))
}

/// The engineer's prompt in `session` while a plan or budget question it opened waits (`ask_opener`,
/// `ask_budget`), phase g G5: a yes approves it, a "no, because ..." refuses it (a veto, `ratify`), each
/// answered in the engineer's own words at their own prompt, never by a command the model runs. What the
/// session is told: the outcome, and after an approval how many drones wait for the work; anything else
/// the engineer says leaves the question open and reminds the session of it. None for a session that
/// opened no plan (one failed stat).
pub fn at_prompt(home: &Path, session: &str, prompt: &str, now: i64) -> Option<String> {
    if !opener_path(session).exists() {
        return None;
    }
    let open: Vec<Entry> = queen::open(home).into_iter().filter(|e| at_own_prompt(e) && e.session == session).collect();
    let Some(e) = open.last() else {
        let _ = std::fs::remove_file(opener_path(session));
        return None;
    };
    let p = e.payload.clone().unwrap_or_default();
    let goal = p["goal"].as_str().unwrap_or_default().to_string();
    let budget = p["budget"].is_number();
    let cwd = e.cwd.clone();
    let engineer = learn::engineer(&cwd);
    let said = prompt.trim();
    let answer = if refuses(said) {
        Some(said.to_string())
    } else if approves(said) || (budget && raise_of(said, p["budget"].as_f64().unwrap_or(0.0)).is_some()) {
        Some(if budget { said.to_string() } else { APPROVE_OPTION.to_string() })
    } else {
        None
    };
    let Some(answer) = answer else {
        return Some(format!(
            "[borg hive] {} of hive goal {goal} still waits for the engineer (queen question {}): ask them to answer at their prompt, `yes` {}, or `no, because <why>`. Never answer it yourself.",
            if budget { "The budget question" } else { "The plan" },
            e.id,
            if budget { "or the new budget in dollars to raise it" } else { "to approve it" }
        ));
    };
    if let Err(err) = queen::answer(home, &e.id, &answer, &engineer, now) {
        return Some(format!("[borg hive] the engineer's answer to queen question {} was not recorded: {err}", e.id));
    }
    if open.len() == 1 {
        let _ = std::fs::remove_file(opener_path(session));
    }
    let b = super::board::fold_on(&super::board::read(&super::tasks_path(home, &goal)), &super::node(home));
    let (drones, nodes) = waiting(home, &goal, &b);
    let waiting = format!("{drones} drone(s) on {nodes} node(s) waiting");
    let yes = match budget {
        true => raise_of(&answer, p["budget"].as_f64().unwrap_or(0.0)).is_some(),
        false => approves(&answer),
    };
    Some(match (budget, yes) {
        (true, true) => {
            format!("[borg hive] The engineer raised the budget of hive goal {goal}: its drones claim again; {waiting}. Tell the engineer so in one line.")
        }
        (true, false) => {
            format!("[borg hive] The engineer kept the budget of hive goal {goal}: its drones claim nothing more. Tell the engineer so in one line.")
        }
        (false, true) => format!(
            "[borg hive] The engineer approved the plan of hive goal {goal}: its tasks are open to the drones now; {waiting}. Tell the engineer \"{waiting}\" in one line and go on with their request."
        ),
        (false, false) => format!(
            "[borg hive] The engineer refused the plan of hive goal {goal}: \"{}\". No drone takes a task of it. Revise the plan with them: open the changed or new tasks (`borg hive open {goal} --id ... --title ... --accept NAME=CMD`), and the plan is asked again; never answer it yourself.",
            stats::excerpt(said, 300)
        ),
    })
}

/// The interactive drones waiting for work of `goal` (board `b`): this node's registered drones that
/// serve it, and each other node's takers its node worker rostered while its membership is open. (drones,
/// nodes).
pub fn waiting(home: &Path, goal: &str, b: &Board) -> (usize, usize) {
    let here = super::node(home);
    let local = super::interactive::live(home).into_iter().filter(|e| e.serves(goal)).count();
    let others: Vec<usize> =
        b.rosters.iter().filter(|(n, _)| **n != here && super::interactive::member_open(b, n)).map(|(_, (_, names))| names.len()).filter(|n| *n > 0).collect();
    (local + others.iter().sum::<usize>(), usize::from(local > 0) + others.len())
}

/// The open plan question of `key`, if one is queued.
fn open_question(store: &Path, key: &str) -> Option<Entry> {
    queen::open(store).into_iter().find(|e| e.kind == Kind::Plan && e.decision_key == key)
}

fn question(g: &Goal, cards: &[Card], key: &str, repo: &str, cwd: &str, engineer: &str, now: i64) -> Entry {
    let high = cards.iter().filter(|c| c.risk == Risk::High).count();
    let list: Vec<String> = cards.iter().map(|c| format!("{} \"{}\" ({})", c.task, stats::excerpt(&c.intent, 80), c.risk.name())).collect();
    Entry {
        id: format!("q{}", &key[..8]),
        decision_key: key.into(),
        kind: Kind::Plan,
        question: format!(
            "Hive goal {}: approve its plan before drones start? {} task(s){}: {}",
            g.id,
            cards.len(),
            if high > 0 { format!(", {high} high-risk") } else { String::new() },
            stats::excerpt(&list.join("; "), 600)
        ),
        options: vec![APPROVE_OPTION.into(), REFUSE_OPTION.into()],
        default: REFUSE_OPTION.into(),
        evidence: cards.iter().map(render_card).collect(),
        session: plan_session(&g.id),
        cwd: cwd.into(),
        repo: repo.into(),
        engineer: engineer.into(),
        ms: now,
        answer: None,
        answered_by: None,
        answered_ms: None,
        disagreeing: Vec::new(),
        dismissed: None,
        dismissed_ms: None,
        payload: Some(json!({"goal": g.id, "cards": cards})),
    }
}

/// What the plan gate decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Gate {
    /// No task to approve (an empty board, or only fix tasks).
    Nothing,
    Approved(Decision),
    Refused(Decision),
}

/// The plan gate of `borg debug hive run` for goal `g` (board `b`) in the repo at `root`: approved already,
/// approved by `--yes` (`yes`), or the plan question raised (or found queued) and waited for, answered
/// at this terminal or anywhere the queue is. Under the run lock, before any worktree is made.
///
/// # Errors
/// The question or the decision cannot be written.
pub fn gate(home: &Path, g: &Goal, b: &Board, root: &Path, yes: bool) -> Result<Gate, String> {
    let cards = cards(b);
    if cards.is_empty() {
        return Ok(Gate::Nothing);
    }
    let key = key(&g.id, &cards);
    let cwd = root.display().to_string();
    let engineer = learn::engineer(&cwd);
    let done = |d: Decision| if d.approved { Gate::Approved(d) } else { Gate::Refused(d) };
    match decided(home, &g.id, &key) {
        Some(d) if d.approved || !yes => return Ok(done(d)),
        _ => {}
    }
    if yes {
        // The engineer approved at the command line: a queued question of this plan is answered so.
        if let Some(e) = open_question(home, &key) {
            queen::answer(home, &e.id, APPROVE_OPTION, &engineer, learn::now_ms())?;
            return decided(home, &g.id, &key).map(done).ok_or_else(|| format!("{}: the approval was not recorded", e.id));
        }
        let high = cards.iter().filter(|c| c.risk == Risk::High).count();
        let d = Decision {
            v: 1,
            ms: learn::now_ms(),
            key,
            approved: true,
            by: engineer,
            via: "yes".into(),
            answer: String::new(),
            question: String::new(),
            tasks: cards.len(),
            high,
        };
        record(home, &g.id, &d)?;
        return Ok(Gate::Approved(d));
    }
    let e = match open_question(home, &key) {
        Some(e) => e,
        None => {
            let e = question(g, &cards, &key, &queen::repo_name(&cwd), &cwd, &engineer, learn::now_ms());
            queen::ask_once(home, &e).map_err(|err| err.to_string())?;
            e
        }
    };
    println!("hive run {}: the plan waits for the engineer's approval (queen question {}); no drone starts before it", g.id, e.id);
    for c in &cards {
        println!("  {}", render_card(c));
    }
    println!("  answer: y here, or `borg queen answer {} a` to approve, `borg queen answer {} \"<why not>\"` to stop the run", e.id, e.id);
    let typed = terminal();
    if typed.is_some() {
        print!("approve the plan? [y/N, or why not] ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
    loop {
        if let Some(d) = decided(home, &g.id, &key) {
            return Ok(done(d));
        }
        match queen::entries(home).into_iter().find(|x| x.id == e.id) {
            Some(x) if x.dismissed.is_some() => {
                let d = Decision {
                    v: 1,
                    ms: learn::now_ms(),
                    key,
                    approved: false,
                    by: String::new(),
                    via: "queen".into(),
                    answer: format!("dismissed: {}", x.dismissed.unwrap_or_default()),
                    question: e.id.clone(),
                    tasks: cards.len(),
                    high: cards.iter().filter(|c| c.risk == Risk::High).count(),
                };
                record(home, &g.id, &d)?;
                return Ok(Gate::Refused(d));
            }
            // Answered, but its decision is not in plan.jsonl (removed by hand): the answer decides.
            Some(x) if x.answer.is_some() && x.dismissed.is_none() => {
                let a = x.answer.unwrap_or_default();
                let d = Decision {
                    v: 1,
                    ms: x.answered_ms.unwrap_or_else(learn::now_ms),
                    key,
                    approved: approves(&a),
                    by: x.answered_by.unwrap_or_default(),
                    via: "queen".into(),
                    answer: a,
                    question: e.id.clone(),
                    tasks: cards.len(),
                    high: cards.iter().filter(|c| c.risk == Risk::High).count(),
                };
                return Ok(done(d));
            }
            _ => {}
        }
        if let Some(line) = typed.as_ref().and_then(|rx| rx.try_recv().ok()) {
            let line = line.trim();
            let text = match line {
                "" | "n" | "N" => Ok(REFUSE_OPTION.to_string()),
                l if approves(l) => Ok(APPROVE_OPTION.to_string()),
                l => queen::option_answer(&e, l),
            };
            // A typo is said and asked again; an answer lands in plan.jsonl, which the loop reads next.
            if let Err(err) = text.and_then(|t| queen::answer(home, &e.id, &t, &engineer, learn::now_ms())) {
                eprint!("borg debug hive run: {err}\napprove the plan? [y/N, or why not] ");
            }
            continue;
        }
        std::thread::sleep(POLL);
    }
}

/// Lines typed at this terminal, read on a thread of their own (the queue may be answered elsewhere
/// first; the run then ends with the thread still reading). None without a terminal.
fn terminal() -> Option<std::sync::mpsc::Receiver<String>> {
    use std::io::{BufRead, IsTerminal};
    if !std::io::stdin().is_terminal() {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    Some(rx)
}

/// Escalates the claim of a high-risk task `t` of goal `g` by `session` in `cwd`, once per task (a
/// group's attempts are one task) ever: one queen question of its card's kind, naming the session (escalation.rs counts its answer). What the
/// drone is told, only when it was queued now.
pub fn escalate(home: &Path, g: &Goal, t: &Task, session: &str, cwd: &str, now: i64) -> Option<String> {
    if !matches!(t.role, Role::Work | Role::Plan) || t.from.is_some() {
        return None;
    }
    let c = card(t);
    let kind = c.kind?;
    let repo = queen::repo_name(cwd);
    // A group's attempts at one task (`--group K`, group.rs) are one decision: one question for all.
    let decision = t.group.as_ref().unwrap_or(&t.id);
    let key = queen::decision_key(kind, &g.id, &format!("hive {} {decision}", g.id));
    let (default, other) = kind.options();
    let drone = super::drone_of(session);
    let who = if drone.is_empty() { session.chars().take(8).collect() } else { drone };
    let e = Entry {
        id: format!("q{}", &key[..8]),
        decision_key: key,
        kind,
        question: format!(
            "Hive goal {}: drone {who} claimed {} \"{}\", which its plan marks high-risk: {}. Is this the right call, or should it be done differently?",
            g.id,
            t.id,
            stats::excerpt(&t.title, 200),
            kind.label()
        ),
        options: vec![default.into(), other.into()],
        default: default.into(),
        evidence: vec![render_card(&c)],
        session: session.into(),
        cwd: cwd.into(),
        repo,
        engineer: learn::engineer(cwd),
        ms: now,
        answer: None,
        answered_by: None,
        answered_ms: None,
        disagreeing: Vec::new(),
        dismissed: None,
        dismissed_ms: None,
        payload: Some(json!({"goal": g.id, "task": t.id})),
    };
    let told = || {
        format!("[borg hive] {} is high-risk ({}): the engineer is asked about it (queen question {}). Until they answer: {default}.", t.id, kind.label(), e.id)
    };
    match queen::ask_once(home, &e) {
        Ok(true) => Some(told()),
        // Another attempt of the group raised it: this one's drone is told too while it is open.
        Ok(false) if t.group.is_some() => queen::entries(home)
            .into_iter()
            .rev()
            .find(|x| x.decision_key == e.decision_key)
            .filter(|x| x.is_open() && x.payload.as_ref().is_some_and(|p| p["task"] != t.id.as_str()))
            .map(|_| told()),
        _ => None,
    }
}

/// The plan as `borg top` and borg-web show it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Row {
    /// `approved`, `refused`, `waiting` (its question is open) or `not asked` (no run asked yet: the
    /// tasks were opened and worked through the CLI).
    pub state: String,
    pub by: String,
    pub via: String,
    pub ms: i64,
    pub question: String,
    pub answer: String,
    pub cards: Vec<Card>,
    /// An interactive goal's plan (phase g G5): approved on the board, or asked at the planning session's
    /// prompt; the engineer answers it there or with `borg hive ratify`, never with `borg queen answer`.
    pub interactive: bool,
}

/// The plan of goal `goal` (board `b`) and its decision; None without a card.
pub fn row(home: &Path, goal: &str, b: &Board) -> Option<Row> {
    let cards = cards(b);
    if cards.is_empty() {
        return None;
    }
    // An interactive goal's plan is approved task by task on the board (phase g G5): the plan still
    // waiting is its cards not approved yet, and with none left it is approved.
    let pending = unratified(b);
    let key = match b.ratified.is_empty() {
        true => key(goal, &cards),
        false => key(goal, &pending),
    };
    let open = open_question(home, &key);
    let interactive = !b.ratified.is_empty() || open.as_ref().is_some_and(at_own_prompt);
    let r = |state: &str, by: String, via: String, ms: i64, question: String, answer: String| Row {
        state: state.into(),
        by,
        via,
        ms,
        question,
        answer,
        cards: cards.clone(),
        interactive,
    };
    if !b.ratified.is_empty() && pending.is_empty() {
        let d = super::read_lines::<Decision>(&path(home, goal)).into_iter().rfind(|d| d.approved);
        return Some(match d {
            Some(d) => r("approved", d.by, d.via, d.ms, d.question, d.answer),
            None => r("approved", String::new(), "ratify".into(), 0, String::new(), String::new()),
        });
    }
    Some(match (decided(home, goal, &key), open) {
        (Some(d), _) => r(if d.approved { "approved" } else { "refused" }, d.by, d.via, d.ms, d.question, d.answer),
        (None, Some(e)) => r("waiting", String::new(), "queen".into(), e.ms, e.id, String::new()),
        (None, None) => r("not asked", String::new(), String::new(), 0, String::new(), String::new()),
    })
}

#[cfg(test)]
mod tests {
    use super::super::{Event, goal};
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-hive-plan-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn task(id: &str, title: &str, files: &[&str]) -> Task {
        Task {
            id: id.into(),
            role: Role::Work,
            title: title.into(),
            repo: String::new(),
            files: files.iter().map(|f| f.to_string()).collect(),
            deps: vec![],
            accept: vec![],
            of: None,
            not_by: vec![],
            flags: vec![],
            from: None,
            group: None,
        }
    }

    fn seed(home: &Path, tasks: Vec<Task>) -> Goal {
        let g = Goal {
            v: 1,
            id: "g".into(),
            spec: "ship it".into(),
            oracle: vec![goal::Check::parse("ok=true", 60).unwrap()],
            budget_usd: 1.0,
            repos: vec![],
            author: "e@x".into(),
            created_ms: 1,
            sig: String::new(),
            hidden: None,
            held_out: false,
        };
        goal::create(home, &g).unwrap();
        let events: Vec<Event> = tasks.into_iter().map(|t| Event::open(t, "engineer:e", 10)).collect();
        super::super::board::append(&super::super::tasks_path(home, "g"), &events).unwrap();
        g
    }

    fn board(home: &Path) -> Board {
        super::super::board::fold(&super::super::board::read(&super::super::tasks_path(home, "g")))
    }

    #[test]
    fn a_card_takes_its_risk_from_the_classifier_or_its_scope() {
        let high = card(&task("t1", "drop the users table and rebuild it", &["db/schema.sql"]));
        assert_eq!((high.risk, high.kind, high.why.as_str()), (Risk::High, Some(Kind::Irreversible), "an irreversible change"));
        let low = card(&task("t2", "document the parser", &["docs/parser.md", "tests/parse_test.rs", "src/lexer_test.go"]));
        assert_eq!((low.risk, low.kind), (Risk::Low, None));
        let normal = card(&task("t3", "add a lexer", &["src/lexer.rs", "README.md"]));
        assert_eq!((normal.risk, normal.why.as_str()), (Risk::Normal, "ordinary work"), "one file of logic makes it ordinary");
        assert_eq!(card(&task("t4", "add a lexer", &[])).risk, Risk::Normal, "no scope is no proof of docs only");
        assert_eq!(card(&task("t5", "deploy to production", &[])).kind, Some(Kind::ExternalMutation));
        assert_eq!(render_card(&high), "t1  drop the users table and rebuild it · files db/schema.sql · risk high (an irreversible change)");
    }

    #[test]
    fn the_plan_is_its_work_tasks_and_a_changed_task_is_a_new_plan() {
        let home = temp("key");
        let mut fix = task("integrate.1.fix", "fix the integration", &[]);
        fix.from = Some("abc".into());
        let mut verify = task("t1.v1", "verify t1", &[]);
        verify.role = Role::Verify;
        verify.of = Some("t1".into());
        seed(&home, vec![task("t1", "a", &["a.rs"]), task("t2", "b", &["b.rs"]), fix, verify]);
        let cs = cards(&board(&home));
        assert_eq!(cs.iter().map(|c| c.task.as_str()).collect::<Vec<_>>(), ["t1", "t2"], "no fix or verify task is a card");
        let k = key("g", &cs);
        assert_eq!(k, key("g", &cards(&board(&home))), "stable");
        let mut other = cs.clone();
        other[1].files = vec!["c.rs".into()];
        assert_ne!(k, key("g", &other));
        assert_ne!(k, key("h", &cs));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn an_answer_approves_only_when_it_says_yes() {
        for a in [APPROVE_OPTION, "approve", "y", "Yes", "ok, go ahead", "lgtm"] {
            assert!(approves(a), "{a}");
        }
        for a in [REFUSE_OPTION, "no", "n", "", "yes but split t2 first", "not yet"] {
            assert!(!approves(a), "{a}");
        }
        assert_eq!(reason(REFUSE_OPTION), None);
        assert_eq!(reason("no"), None);
        assert_eq!(reason("No: t2 must come before t1").as_deref(), Some("t2 must come before t1"));
        assert_eq!(reason("nothing in docs/ yet").as_deref(), Some("nothing in docs/ yet"), "a word starting with no is not a no");
        // Characters whose lowercase has another byte length: no panic, the reason intact.
        assert_eq!(reason("no \u{212A}elvin is too cold").as_deref(), Some("\u{212A}elvin is too cold"));
        assert_eq!(reason("NO: \u{130}stanbul first").as_deref(), Some("\u{130}stanbul first"));
        assert_eq!(reason("\u{1E9E}chema first").as_deref(), Some("\u{1E9E}chema first"));
    }

    #[test]
    fn a_no_with_a_reason_is_a_veto_and_the_decision_is_recorded() {
        let home = temp("veto");
        let g = seed(&home, vec![task("t1", "drop the users table", &["db.sql"]), task("t2", "b", &["b.rs"])]);
        let cs = cards(&board(&home));
        let k = key("g", &cs);
        let e = question(&g, &cs, &k, "app", "/w/app", "e@x", 5);
        assert!(
            e.question.starts_with("Hive goal g: approve its plan before drones start? 2 task(s), 1 high-risk: t1 \"drop the users table\" (high)"),
            "{}",
            e.question
        );
        assert!(queen::ask_once(&home, &e).unwrap());
        assert!(!queen::ask_once(&home, &e).unwrap(), "one question per plan");
        assert_eq!(row(&home, "g", &board(&home)).unwrap().state, "waiting");
        // Learning is on: this suite never sets BORG_LEARN=off.
        queen::answer(&home, &e.id, "no: back up the table first", "e@x", 9).unwrap();
        let d = decided(&home, "g", &k).unwrap();
        assert_eq!((d.approved, d.via.as_str(), d.tasks, d.high, d.question.as_str()), (false, "queen", 2, 1, e.id.as_str()));
        let r = row(&home, "g", &board(&home)).unwrap();
        assert_eq!((r.state.as_str(), r.by.as_str(), r.cards.len()), ("refused", "e@x", 2));
        let episodes = std::fs::read_to_string(home.join("episodes.jsonl")).unwrap_or_default();
        assert!(episodes.contains("back up the table first") && episodes.contains("\"kind\":\"veto\""), "{episodes}");
        assert!(!home.join("lessons.jsonl").exists(), "a plan's answer writes no decision lesson");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn yes_approves_without_a_question_and_answers_a_queued_one() {
        let home = temp("yes");
        let g = seed(&home, vec![task("t1", "a", &["a.rs"])]);
        let b = board(&home);
        let Gate::Approved(d) = gate(&home, &g, &b, &home, true).unwrap() else { panic!("approved") };
        assert_eq!((d.via.as_str(), d.question.as_str()), ("yes", ""));
        assert!(queen::entries(&home).is_empty(), "nothing asked");
        assert_eq!(gate(&home, &g, &b, &home, false).unwrap(), Gate::Approved(d), "decided once, not asked again");
        // A queued question of a new plan is answered by --yes.
        super::super::board::append(&super::super::tasks_path(&home, "g"), &[Event::open(task("t2", "b", &["b.rs"]), "engineer:e", 11)]).unwrap();
        let b = board(&home);
        let cs = cards(&b);
        let e = question(&g, &cs, &key("g", &cs), "app", "", "", 12);
        queen::ask_once(&home, &e).unwrap();
        let Gate::Approved(d) = gate(&home, &g, &b, &home, true).unwrap() else { panic!("approved") };
        assert_eq!((d.via.as_str(), d.question.as_str()), ("queen", e.id.as_str()));
        assert!(queen::open(&home).is_empty());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn an_answer_raises_the_budget_or_refuses_only_when_it_says_so() {
        assert_eq!(raise_of("20", 5.0), Some(20.0));
        assert_eq!(raise_of("raise it to $12.5", 5.0), Some(12.5));
        assert_eq!(raise_of("yes", 5.0), Some(10.0), "a yes doubles it");
        assert_eq!(raise_of(RAISE_OPTION, 0.2), Some(1.0));
        assert_eq!(raise_of("3", 5.0), None, "not above what is spent");
        assert_eq!(raise_of(KEEP_OPTION, 5.0), None);
        assert_eq!(raise_of("no, because 20 dollars is too much", 5.0), None, "a refusal with a number raises nothing");
        assert_eq!(raise_of("go t1", 0.5), None, "a digit inside a word is no number");
        assert_eq!(raise_of("ok, 20.", 5.0), Some(20.0));
        assert_eq!(raise_of("(30$)", 5.0), Some(30.0));
        for a in ["no", "No, because t1 first", "n", "nope: later"] {
            assert!(refuses(a), "{a}");
        }
        for a in ["not yet", "nothing to add", "yes", "what does t1 do?"] {
            assert!(!refuses(a), "{a}");
        }
    }

    /// Phase g G5: an interactive goal's plan question goes to the session that opened it and to no
    /// other (queen.rs `deliver`), and is asked once per plan; a changed plan dismisses the old one.
    #[test]
    fn an_interactive_plan_is_asked_in_its_opening_session_only() {
        let home = temp("opener");
        let g = seed(&home, vec![task("t1", "a", &["a.rs"])]);
        let (e, cards) = ask_opener(&home, &g, &board(&home), "sess-q", "/w/app", 1).expect("asked");
        assert_eq!((e.session.as_str(), cards.len(), at_own_prompt(&e)), ("sess-q", 1, true));
        assert!(queen::deliver(&home, "sess-other", "/w/app", || vec!["app".into()], "", 2).is_none(), "never another session's");
        super::super::board::append(&super::super::tasks_path(&home, "g"), &[Event::open(task("t2", "b", &["b.rs"]), "engineer:e", 11)]).unwrap();
        let (e2, cards) = ask_opener(&home, &g, &board(&home), "sess-q", "/w/app", 3).expect("asked again");
        assert_eq!(cards.len(), 2);
        let open = queen::open(&home);
        assert_eq!(open.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), [e2.id.as_str()], "the old plan's question is dismissed");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_high_risk_claim_escalates_once_and_others_never() {
        let home = temp("escalate");
        let g = seed(&home, vec![]);
        let risky = task("t1", "force-push the release branch", &[]);
        let told = escalate(&home, &g, &risky, "sess-a", "/w/app", 1).expect("escalated");
        assert!(told.contains("t1 is high-risk (an irreversible change)") && told.contains("Until they answer: proceed as planned."), "{told}");
        assert_eq!(escalate(&home, &g, &risky, "sess-b", "/w/app", 2), None, "once per task, whoever claims it");
        assert_eq!(escalate(&home, &g, &task("t2", "add a lexer", &[]), "sess-a", "/w/app", 3), None);
        let mut verify = risky.clone();
        verify.role = Role::Verify;
        verify.id = "t1.v1".into();
        assert_eq!(escalate(&home, &g, &verify, "sess-c", "/w/app", 4), None, "a verify task is not the risky work");
        let q = queen::entries(&home);
        assert_eq!(q.len(), 1);
        assert_eq!((q[0].kind, q[0].session.as_str(), q[0].kind.escalates()), (Kind::Irreversible, "sess-a", true));
        // `--group 2`: one question for the group's attempts; the second attempt's drone is told too.
        let member = |n: usize| Task { id: format!("t3.g{n}"), group: Some("t3".into()), ..task("t3.g1", "drop the users table", &[]) };
        assert!(escalate(&home, &g, &member(1), "sess-d", "/w/app", 5).is_some());
        let told = escalate(&home, &g, &member(2), "sess-e", "/w/app", 6).expect("told");
        assert!(told.starts_with("[borg hive] t3.g2 is high-risk"), "{told}");
        assert_eq!(escalate(&home, &g, &member(1), "sess-f", "/w/app", 7), None, "the attempt that raised it is told once");
        assert_eq!(queen::entries(&home).len(), 2, "one question for the group");
        let _ = std::fs::remove_dir_all(home);
    }
}
