//! `borg top [--dir DIR] [--once] [--json]`: the drones of each collective, one screen.
//!
//! Local drones come from presence records (presence.rs), remote ones from what the mesh daemon
//! last heard from members (only for a collective that may sync). Drones are designated by start
//! order across both, Borg style: "Two of Five". `--dir` shows only the collective DIR routes to;
//! otherwise every collective on this machine that has drones. On a terminal it is a live screen until
//! `q` (top_live.rs); piped, or with `--once`, it prints the text once; `--json` prints the same data
//! for scripts.
//!
//! Messages (msg.rs): each local drone's unread count (what its next prompt or tool call shows), and
//! the collective's last few messages, one line each. A remote drone's reads are claimed on its node.
//! A sender is named by what its line claims, with the node the mesh checked it against (`sender`).
//! The messages to the engineer the screen shows some of the text of are acknowledged as theirs when no
//! session runs it, as `borg inbox` acknowledges what it lists from a terminal (`engineer_looks`,
//! `acknowledge`): a drone's `borg top`, printed or live, acknowledges nothing; `--json` acknowledges
//! nothing.
//!
//! Lesson selection (select.rs): one line per collective once its summary has folded a graded check,
//! or while a policy other than the default (a switch) or the monitor's revert decides (`Selection`);
//! `--json` carries it in full as `select`. `borg status` prints the same line, borg web the rest.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::Serialize;
use serde_json::{Value, json};

use crate::clock::{DAY_MS, ago};
use crate::collective::{self, Route, Share};
use crate::msg::{self, Address, Message};
use crate::presence::{self, Presence, State};
use crate::{install, learn, select, steer};

#[derive(Debug, Clone, Serialize)]
pub struct Drone {
    /// "Two of Five".
    pub designation: String,
    /// 1-based start order.
    pub number: usize,
    /// The member key of the node it runs on; empty for this machine.
    pub node: String,
    pub state: State,
    /// Paused by a steering directive (local drones only).
    pub paused: bool,
    #[serde(flatten)]
    pub p: Presence,
}

/// The messages side of a collective's screen.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Inbox {
    /// Per local drone's session, the messages not shown to it yet.
    pub unread: BTreeMap<String, usize>,
    /// The newest `RECENT` messages, newest first.
    pub recent: Vec<Message>,
    /// The messages sent in the last day.
    pub day: usize,
    /// This node as messages name it (msg.rs `node`).
    #[serde(skip)]
    pub here: String,
    /// Per local drone's session that listens for wakes (listen.rs), whether its waiter runs.
    pub listen: BTreeMap<String, crate::listen::Status>,
}

/// Display: how many messages the screen lists.
pub const RECENT: usize = 5;

const NUMBERS: [&str; 20] = [
    "One",
    "Two",
    "Three",
    "Four",
    "Five",
    "Six",
    "Seven",
    "Eight",
    "Nine",
    "Ten",
    "Eleven",
    "Twelve",
    "Thirteen",
    "Fourteen",
    "Fifteen",
    "Sixteen",
    "Seventeen",
    "Eighteen",
    "Nineteen",
    "Twenty",
];

pub fn word(n: usize) -> String {
    NUMBERS.get(n.wrapping_sub(1)).map(|s| s.to_string()).unwrap_or_else(|| n.to_string())
}

/// The drones of the collective stored at `home` (pure over its inputs, for tests).
pub fn assemble(local: Vec<Presence>, remote: Vec<presence::Remote>, now: i64, alive: impl Fn(u32) -> bool, paused: impl Fn(&Presence) -> bool) -> Vec<Drone> {
    let mut all: Vec<(String, Presence, State, bool)> = local
        .into_iter()
        .map(|p| {
            let s = presence::state(&p, now, &alive);
            let z = s != State::Ended && paused(&p);
            (String::new(), p, s, z)
        })
        .collect();
    // A remote pid means nothing here: its state comes from its events and age only.
    all.extend(remote.into_iter().map(|r| {
        let s = presence::state(&r.p, now, |_| true);
        (r.key, r.p, s, false)
    }));
    all.sort_by(|a, b| (a.1.started, &a.0, &a.1.session).cmp(&(b.1.started, &b.0, &b.1.session)));
    let n = all.len();
    all.into_iter()
        .enumerate()
        .map(|(i, (node, p, state, paused))| Drone { designation: format!("{} of {}", word(i + 1), word(n)), number: i + 1, node, state, paused, p })
        .collect()
}

/// The drones of the collective stored at `home` as of `now`; remote ones only if it may sync.
pub fn drones(home: &Path, may_sync: bool, now: i64) -> Vec<Drone> {
    let remote = if may_sync { presence::remote(home, now) } else { Vec::new() };
    assemble(presence::local(home, now), remote, now, presence::alive, |p| steer::paused(home, &p.session, Some(p.started), now))
}

/// The messages of the store at `home` for its `drones` as of `now` (one read of the log, one
/// directory listing per local drone that has a message).
pub fn inbox(home: &Path, drones: &[Drone], now: i64) -> Inbox {
    let listen = listen_of(home, drones);
    if std::fs::metadata(msg::path(home)).is_err() {
        return Inbox { listen, ..Inbox::default() };
    }
    let all: Vec<Message> = msg::read(home).into_iter().filter(|m| now - m.ms <= msg::TTL_MS).collect();
    let here = msg::node(home);
    let unread = unread_of(home, drones, &here, &all, now);
    let day = all.iter().filter(|m| now - m.ms <= DAY_MS).count();
    let mut recent = all;
    recent.sort_by(|a, b| (b.ms, &b.id).cmp(&(a.ms, &a.id)));
    recent.truncate(RECENT);
    Inbox { unread, recent, day, here, listen }
}

/// Acknowledges the messages to the engineer among `shown`, the ones a screen of the store at `home`
/// put in front of them, as the engineer of the repo at `cwd` (msg.rs `ack_engineer`), as `borg inbox`
/// does for what it lists. `done` holds the ids an earlier frame of the same screen already passed on,
/// so a live screen asks git for the engineer and reads the log's seen lines once per new message to
/// an engineer, never on every frame; the ids are recorded whether the write worked or not (best
/// effort, as the inbox's: unacknowledged, a message holds its sender's window until it expires).
/// Only for a screen the engineer looks at (`engineer_looks`); `--json` is for scripts, not the
/// engineer's eyes, and acknowledges nothing.
pub fn acknowledge(home: &Path, cwd: &str, shown: &[&Message], done: &mut HashSet<String>, now: i64) {
    let to_engineer: Vec<&Message> =
        shown.iter().copied().filter(|m| matches!(Address::parse(&m.to), Some(Address::Engineer { .. })) && !done.contains(&m.id)).collect();
    if to_engineer.is_empty() {
        return;
    }
    let _ = msg::ack_engineer(home, cwd, &to_engineer, now);
    done.extend(to_engineer.iter().map(|m| m.id.clone()));
}

/// Whether the engineer is the one looking at a screen of the collectives whose drones are `drones`:
/// `caller` (msg.rs `caller`, the session whose shell runs this command) finds no session over any of
/// them, as `borg inbox` acknowledges as the engineer only when it finds none. A harness runs a drone's
/// commands piped, or on a pseudo-terminal where a plain `borg top` is the live screen (top_live.rs),
/// and in the engineer's repo, so without this test a drone's `borg top` would acknowledge as the
/// engineer what only its model saw: its senders' windows to the engineer opened,
/// the messages read on every node. A shell naming sessions `caller` cannot tell apart is a session's
/// too. Every collective's drones count, not only those of the one acknowledged in: opencode's shell
/// names its session only through the drone of its pid, which may be another collective's.
pub fn engineer_looks<'a>(caller: impl Fn(&[Drone]) -> Result<Option<(String, &'static str)>, String>, mut drones: impl Iterator<Item = &'a [Drone]>) -> bool {
    drones.all(|ds| matches!(caller(ds), Ok(None)))
}

/// The directory whose git identity names the engineer looking at the screen: `dir` made absolute, else
/// the current directory (as `borg inbox` takes it).
pub(crate) fn engineer_dir(dir: Option<&str>) -> String {
    let d = dir.map_or_else(|| std::env::current_dir().unwrap_or_default(), std::path::PathBuf::from);
    std::path::absolute(&d).unwrap_or(d).display().to_string()
}

/// Per local drone's session that listens for wakes, whether its waiter runs (one stat for any other).
fn listen_of(home: &Path, drones: &[Drone]) -> BTreeMap<String, crate::listen::Status> {
    drones
        .iter()
        .filter(|d| d.node.is_empty() && d.state != State::Ended)
        .filter_map(|d| crate::listen::status(home, &d.p.session).map(|s| (d.p.session.clone(), s)))
        .collect()
}

/// A drone's wake as the screen shows it: `on` (it listens, the next stop starts its waiter), `waiter`
/// (its waiter runs), `-` (it is never woken, or this node cannot tell).
pub fn wake_cell(inbox: &Inbox, d: &Drone) -> &'static str {
    inbox.listen.get(&d.p.session).map_or("-", |s| s.name())
}

/// Per live local drone's session, how many of the already-read `all` messages were not shown to it
/// (`here`: this node, msg.rs `node`).
pub fn unread_of(home: &Path, drones: &[Drone], here: &str, all: &[Message], now: i64) -> BTreeMap<String, usize> {
    drones
        .iter()
        .filter(|d| d.node.is_empty() && d.state != State::Ended)
        .map(|d| (d.p.session.clone(), msg::unread(home, all, here, &d.p.session, d.p.started, now)))
        .collect()
}

/// The drone of `drones` that is `session` on `node` (`here`: this node).
fn drone_at<'a>(drones: &'a [Drone], here: &str, node: &str, session: &str) -> Option<&'a Drone> {
    drones.iter().find(|d| d.p.session == session && if d.node.is_empty() { node == here } else { d.node == node })
}

/// Who sent `m`, as the screen says it: the drone its (node, `by`) pair is, else its label (msg.rs
/// `sender_label`: the name it was sent under, else the start of `by`); the engineer by email. Every
/// part but the node is the sender's claim, so a line from another node than `here` carries that node's
/// short key (as msg.rs `line`).
pub fn sender(drones: &[Drone], here: &str, m: &Message) -> String {
    let name = match Address::parse(&m.by) {
        Some(Address::Engineer { email }) if email.is_empty() => msg::ENGINEER.into(),
        Some(Address::Engineer { email }) => collective::aliases().display(&email),
        _ => match drone_at(drones, here, &m.node, &m.by) {
            Some(d) => d.designation.clone(),
            None => msg::sender_label(m),
        },
    };
    if m.node == here { name } else { format!("{name} @{}", msg::short_node(&m.node)) }
}

/// Whom `to` names, as the screen says it: `local` (this machine's sessions), `collective` (every
/// member's), the engineer, the drone it is, else the start of the session id (msg.rs `short_session`).
/// Another node's `local` (it never travels, msg.rs `stays_here`) is named with that node's short key
/// (msg.rs `short_node`).
pub fn recipient(drones: &[Drone], here: &str, to: &str) -> String {
    match Address::parse(to) {
        Some(Address::Engineer { email }) if !email.is_empty() => collective::aliases().display(&email),
        Some(Address::Local { node }) if node == here => msg::LOCAL.into(),
        Some(Address::Local { node }) => format!("{} @{}", msg::LOCAL, msg::short_node(&node)),
        Some(Address::All) => msg::COLLECTIVE.into(),
        Some(Address::Session { id, node }) => {
            let d = match node {
                Some(n) => drone_at(drones, here, &n, &id),
                None => drones.iter().find(|d| d.p.session == id),
            };
            d.map_or_else(|| msg::short_session(&id), |d| d.designation.clone())
        }
        Some(a) => a.to_string(),
        None => msg::short_session(to),
    }
}

/// One message as one screen line (without its age): `sender -> recipient: text` (msg.rs keeps the
/// text on one line, its whitespace collapsed).
pub fn message_line(drones: &[Drone], here: &str, m: &Message) -> String {
    let ask = if m.asks() { " asks" } else { "" };
    format!("{}{ask} -> {}: {}", sender(drones, here, m), recipient(drones, here, &m.to), m.text)
}

/// A drone's unread count as the screen shows it: `-` where this node cannot tell (a remote or ended one).
pub fn unread_cell(inbox: &Inbox, d: &Drone) -> String {
    inbox.unread.get(&d.p.session).map_or_else(|| "-".into(), usize::to_string)
}

/// Which drone `target` names: "local", "collective" or "all", a designation ("Two", "Two of Five", "2") or a session
/// id (or a unique prefix of at least 4 characters). `Ok(None)` is every drone. Live first, then local:
/// an id or prefix that matches a running drone names it over an ended one (which never reads a message),
/// and among those one on this machine over one on another node; a remote drone is named only when
/// none here matches as well (or by its number, which names one drone of all).
pub fn resolve<'a>(drones: &'a [Drone], target: &str) -> Result<Option<&'a Drone>, String> {
    let t = target.trim();
    if [msg::ALL, msg::COLLECTIVE, msg::LOCAL].iter().any(|w| t.eq_ignore_ascii_case(w)) {
        return Ok(None);
    }
    let first = t.split_whitespace().next().unwrap_or_default();
    let number = first.parse::<usize>().ok().or_else(|| NUMBERS.iter().position(|w| w.eq_ignore_ascii_case(first)).map(|i| i + 1));
    if let Some(n) = number {
        return drones.iter().find(|d| d.number == n).map(Some).ok_or_else(|| format!("no drone {target}: there are {} (`borg top`)", drones.len()));
    }
    let mut hits: Vec<&Drone> = drones.iter().filter(|d| d.p.session == t || (t.len() >= 4 && d.p.session.starts_with(t))).collect();
    if hits.iter().any(|d| d.state != State::Ended) {
        hits.retain(|d| d.state != State::Ended);
    }
    if hits.iter().any(|d| d.node.is_empty()) {
        hits.retain(|d| d.node.is_empty());
    }
    match hits.as_slice() {
        [d] => Ok(Some(d)),
        [] => Err(format!("no drone or session {target:?} (`borg top`)")),
        _ => Err(format!("{target:?} names {} sessions: give more of the id", hits.len())),
    }
}

/// Display: the characters of a node key in a drone table's column (here, and top_live.rs's `NODE`
/// column): enough to tell a collective's few members apart, short enough for the column. A message
/// names its node longer (msg.rs `NODE_CHARS`), where the key is the one part the mesh checked.
pub const COLUMN_NODE_CHARS: usize = 6;

/// A node key as a drone table's column shows it: its first `COLUMN_NODE_CHARS`.
pub fn column_node(node: &str) -> String {
    node.chars().take(COLUMN_NODE_CHARS).collect()
}

/// Display: the characters of one message line on the printed screen (`render`): about one wide
/// terminal line after the age column; `borg inbox --show` prints the whole text.
const MESSAGE_CHARS: usize = 120;

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n - 1).collect::<String>()) }
}

/// Whether a line of `total` columns, cut to `width` as `cut` cuts it (its first `width - 1` and `…`;
/// top_live.rs cuts its lines the same), still shows any of its last `last` columns: all of them when
/// the line fits, else those before the `…`. A message's line ends with its text, and one cut before
/// any of it names only who wrote to whom.
pub fn shows_last(total: usize, last: usize, width: usize) -> bool {
    total <= width || total.saturating_sub(last) < width.saturating_sub(1)
}

/// Whether the printed screen's line for `m` (`render`: its `message_line` cut to `MESSAGE_CHARS`)
/// shows some of its text, as a live screen's must (top_live.rs `frame`): only then did the engineer
/// read it there, and only then does the printed screen acknowledge it (`screen_of`).
fn prints_text(drones: &[Drone], here: &str, m: &Message) -> bool {
    shows_last(message_line(drones, here, m).chars().count(), m.text.chars().count(), MESSAGE_CHARS)
}

/// One collective's screen.
pub fn render(name: &str, share: Share, drones: &[Drone], inbox: &Inbox, now: i64) -> String {
    let working = drones.iter().filter(|d| matches!(d.state, State::Working | State::Waiting)).count();
    let remote = drones.iter().filter(|d| !d.node.is_empty()).count();
    let share = match share {
        Share::Team => "team",
        Share::Local => "local, never leaves this machine",
    };
    let mut s = format!("{name} ({share}): {} drone(s), {working} active, {remote} remote\n", drones.len());
    if !drones.is_empty() {
        s.push_str(&drone_table(drones, inbox, now));
    }
    if !inbox.recent.is_empty() {
        s.push_str(&format!("  messages: {} in the last day, {} unread\n", inbox.day, inbox.unread.values().sum::<usize>()));
        for m in &inbox.recent {
            s.push_str(&format!("  {:>6}  {}\n", ago(now - m.ms), cut(&message_line(drones, &inbox.here, m), MESSAGE_CHARS)));
        }
    }
    s
}

fn drone_table(drones: &[Drone], inbox: &Inbox, now: i64) -> String {
    let mut s = format!(
        "  {:<18} {:<8} {:<24} {:<14} {:<8} {:<6} {:>6} {:>6}  {:<40} {}\n",
        "DRONE", "SESSION", "ENGINEER", "REPO", "STATE", "WAKE", "IDLE", "UNREAD", "DOING", "FILES"
    );
    for d in drones {
        let state = if d.paused { "paused" } else { d.state.name() };
        let engineer = if d.node.is_empty() { d.p.engineer.clone() } else { format!("{} @{}", d.p.engineer, column_node(&d.node)) };
        let line = format!(
            "  {:<18} {:<8} {:<24} {:<14} {:<8} {:<6} {:>6} {:>6}  {:<40} {}",
            d.designation,
            cut(&d.p.session, 8),
            cut(if engineer.is_empty() { "?" } else { &engineer }, 24),
            cut(&d.p.repo, 14),
            state,
            wake_cell(inbox, d),
            ago(now - d.p.ms),
            unread_cell(inbox, d),
            cut(&d.p.prompt, 40),
            d.p.files.join(", ")
        );
        s.push_str(line.trim_end());
        s.push('\n');
    }
    s
}

pub fn to_json(name: &str, share: Share, drones: &[Drone]) -> Value {
    json!({"collective": name, "share": if share == Share::Team { "team" } else { "local" }, "drones": drones})
}

/// `to_json` with the messages side: each local drone's `unread`, and the collective's `messages`.
pub fn to_json_with(name: &str, share: Share, drones: &[Drone], inbox: &Inbox) -> Value {
    let mut v = to_json(name, share, drones);
    if let Some(ds) = v["drones"].as_array_mut() {
        for (o, d) in ds.iter_mut().zip(drones) {
            if let Some(n) = inbox.unread.get(&d.p.session) {
                o["unread"] = json!(n);
            }
            if let Some(l) = inbox.listen.get(&d.p.session) {
                o["listen"] = json!(l);
            }
        }
    }
    v["messages"] = json!({"day": inbox.day, "recent": inbox.recent});
    v
}

// ---------------------------------------------------------------- lesson selection

/// Lesson selection (select.rs) as the monitor shows it: one line per collective in `borg status` and
/// `borg top`, in full on borg web's Conscience tab (`/api/select`). Every number is read from the
/// store's selection summary (select/summary.rs `load`: its counts, its fit, its monitor) and the
/// policy's own functions, never refitted here; only shares, the declaration's difference and the
/// latency quantiles are taken of the summary's counts. A section the summary does not carry yet is
/// empty, which every surface shows as not measured yet, never as 0. Spend is measured and shown, and
/// priced only under a budget an engineer set for the collective (`budget`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Selection {
    /// The policy a hook started from this environment runs under (select.rs `Policy::name`): `fixed`,
    /// `learned` (opt-in: `BORG_SELECT=learned`), `cap` or `attention` under an evaluation switch.
    pub policy: &'static str,
    /// The policy a hook runs under when no switch is set (select.rs `precedence` without one): the
    /// fixed rule until the pre-registered checks pass (tasks/todo.md, amendment 2026-10-29).
    pub default: &'static str,
    /// What decides: the policy, or `reverted` (select.rs `REVERTED_SOURCE`) when the monitor saw harm
    /// beyond the gate's share and put the fixed rule back (sticky).
    pub source: &'static str,
    /// The conservative gate's budget (select.rs `Log::gate`): the replayed quality (mistakes avoided)
    /// the learned policy may still risk below the fixed rule. Not money. 0 at cold start, where every
    /// decision is the fixed rule's.
    pub gate: f64,
    /// The fit decisions draw from (select/policy.rs `Fit::key`); empty: the prior.
    pub fit: String,
    /// The bytes of `outcomes.jsonl` the summary has folded.
    pub at: u64,
    /// The graded checks and the sessions folded.
    pub checks: u64,
    pub sessions: u64,
    /// Per decision type the summary counts, the decisions that differed from the fixed rule's (`part`)
    /// of those made (`whole`): `route`, the learned checks under the current fit whose routes did
    /// (select/summary.rs `Gate`). The pool's size and early delivery are not counted yet.
    pub deviations: Vec<Count>,
    /// Per reason a learned decision fell back to the fixed rule (select/summary.rs `FELL_BACK`, named as
    /// the record's `reason`: `lock_busy`, `lock_failed`, `unrecorded`), the graded checks it did
    /// (`part`) of those folded (`whole`, `checks`), every reason listed: a guard that binds often shows.
    pub fallbacks: Vec<Count>,
    /// What selection spent per cost source: its model calls, `stop` and `live` checks (select/summary.rs
    /// `Spent`). Judging rides in them: a check's call also judges its session's pending questions and
    /// placebos (predict.rs `judge_section`). `borg learn`'s own calls are not selection's; `borg stats`
    /// counts them (stats.rs `MODEL_CALL`).
    pub spend: Vec<Spend>,
    /// The spend budget an engineer set for the collective and its price; None without one.
    pub budget: Option<Budget>,
    /// Effects with their intervals: grade × route, ask vs free, free vs placebo, dilution, the spread
    /// between sessions and between lessons.
    pub effects: Vec<Estimate>,
    /// The forgetting half-life chosen, in session ticks; `Some(0)`: no forgetting.
    pub half_life: Option<u32>,
    /// Label health: the judged share per route, the unclear share, the placebo rate.
    pub labels: Vec<Count>,
    /// Label health: how the apply/skip declaration moves the judge (a random half is judged without it).
    pub declaration: Option<Estimate>,
    /// The delivery rates the fit learned (select/policy.rs `Rates`): `free_stop`, `free_live` and
    /// `ask_live` weigh a route's value, `later` and `wait` are only shown. None before a fit.
    pub delivery: Option<select::policy::Rates>,
    /// The monitor's off-policy estimate and its health; None before it weighed a labelled apply.
    pub evaluation: Option<Evaluation>,
    /// What the checks measure for the learned policy at no cost to the fixed rule's quality; None
    /// before a graded check is folded.
    pub measurement: Option<Measurement>,
    /// The pool's yield by fill position.
    pub pool: Vec<Yield>,
    /// Stop check latency per harness against its stop budget.
    pub latency: Vec<Latency>,
}

/// `part` of `whole` (a share), named.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Count {
    pub name: String,
    pub part: u64,
    pub whole: u64,
}

/// One cost source's spend over the sessions folded (`Selection::sessions`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Spend {
    /// `stop` or `live`.
    pub source: String,
    pub calls: u64,
    /// The extra rounds it cost the sessions (a stop block is one).
    pub rounds: u64,
    pub usd: f64,
}

/// An engineer's budget for the collective (`collectives.json` `budget`, select.rs `budget_of`): the
/// pool tokens its checks may read per session on average, what they read per session, and the price
/// per pool token the policy weighs a larger pool against (select/policy.rs `price`, λ).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Budget {
    pub tokens: f64,
    pub spent: f64,
    pub price: f64,
}

/// An effect and its interval, as the policy reports them.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Estimate {
    pub name: String,
    /// `points` (percentage points of P(changed and avoids)), `coefficient` (a value-model coefficient,
    /// on the logit scale) or `logit` (a spread's standard deviation).
    pub unit: String,
    pub mean: f64,
    pub lo: f64,
    pub hi: f64,
    /// The labelled units behind it.
    pub n: u64,
}

/// The monitor's weighted estimate (select/policy.rs `Monitor`), the auto-revert's evidence, and how
/// far it can be trusted.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Evaluation {
    /// The learned policy's sessions it read.
    pub sessions: u64,
    /// Per apply, (policy − fixed rule) + α × the fixed rule's value, and the confidence sequence's
    /// upper bound on it (None until enough sessions): below 0 the store reverts to the fixed rule.
    pub mean: f64,
    pub ucb: Option<f64>,
    /// The labelled units weighed, and their effective number ((Σw)² / Σw²).
    pub units: u64,
    pub ess: f64,
    /// The largest weight against the mean weight.
    pub max_weight: f64,
    /// The units whose weight was truncated.
    pub truncated: u64,
}

/// What the checks measure for the learned policy without costing the fixed rule any quality: under the
/// fixed default the placebo collects the not-now labels the fixed rule's own history cannot give
/// (tasks/todo.md, amendment 2026-10-29). Counted over the applies the summary keeps, as `labels`
/// counts its placebo rows (select/summary.rs `Apply`, its route and label).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Measurement {
    /// Placebos drawn: applies no route delivered, judged blindly where a free delivery would be.
    pub placebos: u64,
    /// Of them, those the judge gave a verdict that says something (changed or ignored).
    pub judged: u64,
    /// The tail samples' lessons shown past the pool budget, and those that applied (whole.rs `tail`,
    /// select/summary.rs `Pool::tail`): the yield past the budget, learned under every policy.
    pub tail_shown: u64,
    pub tail_applied: u64,
}

/// The pool's yield at fill positions `from..to` (0 first): of the lessons shown there, those applied,
/// and the fitted chance (monotone over positions).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Yield {
    pub from: u32,
    pub to: u32,
    pub shown: u64,
    pub applied: u64,
    pub rate: f64,
}

/// One harness's stop check latency against its stop budget, over the stops' model calls the summary
/// keeps (select/summary.rs `Pool::stops`, the newest `LATENCY_KEEP`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Latency {
    pub harness: String,
    pub checks: u64,
    /// The calls' median and 95th percentile (nearest rank, `QUANTILES`).
    pub p50_ms: u64,
    pub p95_ms: u64,
    /// Its stop budget (harness `Caps::stop_budget`); None: it names none.
    pub budget_ms: Option<u64>,
    /// Calls that failed: they answered nothing (ran out of the time the budget left them, or the CLI
    /// erred), so their checks asked nothing.
    pub over: u64,
}

/// Display: the latency quantiles shown, the median and the 95th percentile (the tail a stop budget
/// must hold, as the pool's latency model reads it: select/policy.rs `pool_feasible`).
const QUANTILES: [f64; 2] = [0.50, 0.95];

/// The nearest-rank `q` quantile of the ascending `sorted`; 0 for none.
fn quantile(sorted: &[u64], q: f64) -> u64 {
    let rank = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len().max(1));
    sorted.get(rank - 1).copied().unwrap_or(0)
}

impl Selection {
    /// What `summary` says, under `policy`: copied from the summary, its fit and its monitor, never
    /// refitted. What it does not hold yet stays empty: not measured.
    #[must_use]
    pub fn of(summary: &select::summary::Summary, policy: select::Policy) -> Selection {
        use select::summary::{LIVE, Label, STOP};
        let s = summary;
        let learned = policy == select::Policy::Learned;
        let reverted = learned && s.monitor.reverted;
        let default = select::precedence(None, None, false).name();
        let count = |name: &str, part: usize, whole: usize| Count { name: name.into(), part: part as u64, whole: whole as u64 };
        // Every session folded, the compacted ones too, as the spend and pool tokens are.
        let (sessions, checks) = s.checked();
        let deviations = match s.gate.checks {
            0 => Vec::new(),
            n => vec![count("route", s.gate.deviated as usize, n as usize)],
        };
        // Folded with every graded check, as `checks` is: none before one is.
        let fallbacks = select::summary::FELL_BACK
            .iter()
            .zip(s.fell_back)
            .filter(|_| checks > 0)
            .map(|(reason, n)| Count { name: (*reason).into(), part: n, whole: checks })
            .collect();
        let spend = [("stop", STOP), ("live", LIVE)]
            .into_iter()
            .filter(|(_, i)| s.spend[*i].calls > 0)
            .map(|(source, i)| Spend { source: source.into(), calls: s.spend[i].calls, rounds: s.spend[i].rounds, usd: s.spend[i].usd })
            .collect();
        let budget = s.fit.budget.map(|tokens| Budget { tokens, spent: s.pool.tokens as f64 / sessions.max(1) as f64, price: s.fit.lambda });
        let judged = |l: Label| matches!(l, Label::Changed | Label::Ignored);
        // Per route, its applies and those judged: the label health's rows, and the placebo's the
        // measurement's. A free apply of a lesson its session had is dropped at delivery as given before,
        // so no verdict can come of it: out of its row, as it is out of the summary's delivery tallies
        // (select/summary.rs `Apply::had`). A question of one is delivered unless done by then and
        // judged as any other, and a placebo is judged blind, never delivered: in, had or not.
        let of = |route: select::Route| {
            let of: Vec<_> = s.applies.iter().filter(|a| a.route == route && (route != select::Route::Free || !a.had)).collect();
            (of.iter().filter(|a| judged(a.label)).count(), of.len())
        };
        let mut labels = Vec::new();
        for (name, route) in [("judged, asked", select::Route::Ask), ("judged, free", select::Route::Free), ("judged, placebo", select::Route::Placebo)] {
            let (part, whole) = of(route);
            labels.push(count(name, part, whole));
        }
        let verdicts = s.applies.iter().filter(|a| judged(a.label) || a.label == Label::Unclear).count();
        labels.push(count("unclear", s.applies.iter().filter(|a| a.label == Label::Unclear).count(), verdicts));
        let later = s.applies.iter().filter(|a| matches!(a.route, select::Route::Not | select::Route::Placebo)).count();
        let (placebos_judged, placebos) = of(select::Route::Placebo);
        labels.push(count("placebo", placebos, later));
        labels.retain(|c| c.whole > 0);
        let measurement = (checks > 0).then_some(Measurement {
            placebos: placebos as u64,
            judged: placebos_judged as u64,
            tail_shown: s.pool.tail.0,
            tail_applied: s.pool.tail.1,
        });
        // The declaration's effect on the judge: the changed share of judged questions with it minus without.
        let share = |stripped: bool| {
            let of: Vec<_> = s.applies.iter().filter(|a| a.route == select::Route::Ask && judged(a.label) && a.stripped == stripped).collect();
            (of.iter().filter(|a| a.label == Label::Changed).count() as f64, of.len() as f64)
        };
        let ((c1, n1), (c0, n0)) = (share(false), share(true));
        let declaration = (n1 > 0.0 && n0 > 0.0).then(|| {
            let (r1, r0) = (c1 / n1, c0 / n0);
            let se = (r1 * (1.0 - r1) / n1 + r0 * (1.0 - r0) / n0).sqrt();
            let mean = 100.0 * (r1 - r0);
            Estimate { name: "declaration".into(), unit: "points".into(), mean, lo: mean - 100.0 * Z95 * se, hi: mean + 100.0 * Z95 * se, n: (n1 + n0) as u64 }
        });
        let pool = s
            .pool
            .bins
            .iter()
            .enumerate()
            .filter(|(_, (shown, _))| *shown > 0)
            .map(|(b, &(shown, applied))| Yield {
                from: (1u32 << b) - 1,
                to: (1u32 << (b + 1)) - 1,
                shown,
                applied,
                rate: s.fit.pool.yields.get(b).map_or(applied as f64 / shown as f64, |(a, b)| a / (a + b)),
            })
            .collect();
        let mon = &s.monitor;
        let evaluation = (mon.labelled > 0).then(|| Evaluation {
            sessions: u64::from(mon.sessions),
            mean: mon.mean,
            ucb: mon.ucb,
            units: u64::from(mon.labelled),
            ess: mon.ess,
            max_weight: mon.max_weight,
            truncated: u64::from(mon.truncated),
        });
        let latency = s
            .pool
            .stops
            .iter()
            .filter(|(_, calls)| !calls.is_empty())
            .map(|(harness, calls)| {
                let mut ms: Vec<u64> = calls.iter().map(|&(ms, _)| u64::from(ms)).collect();
                ms.sort_unstable();
                let budget = crate::harness::ALL.iter().find(|h| h.name() == harness).and_then(|h| h.caps().stop_budget);
                Latency {
                    harness: harness.clone(),
                    checks: ms.len() as u64,
                    p50_ms: quantile(&ms, QUANTILES[0]),
                    p95_ms: quantile(&ms, QUANTILES[1]),
                    budget_ms: budget.map(|b| u64::try_from(b.as_millis()).unwrap_or(u64::MAX)),
                    over: calls.iter().filter(|(_, failed)| *failed).count() as u64,
                }
            })
            .collect();
        Selection {
            policy: policy.name(),
            default,
            source: if reverted { select::REVERTED_SOURCE } else { policy.name() },
            gate: if learned && !reverted { select::policy::remaining(&s.gate).max(0.0) } else { 0.0 },
            fit: s.fit.key.clone(),
            at: s.at,
            checks,
            sessions,
            deviations,
            fallbacks,
            spend,
            budget,
            effects: s.fit.model.as_ref().map(|m| effects(m, s.fit.labels)).unwrap_or_default(),
            half_life: s.fit.model.as_ref().map(|_| s.fit.half_life.unwrap_or(0)),
            labels,
            declaration,
            delivery: (!s.fit.key.is_empty()).then(|| s.fit.rates.clone()),
            evaluation,
            measurement,
            pool,
            latency,
        }
    }

    /// Whether `borg top` shows the line: once the summary has folded a graded check, or while another
    /// policy than the default (a switch) or the monitor's revert decides.
    #[must_use]
    pub fn shown(&self) -> bool {
        self.checks > 0 || self.source != self.default
    }
}

/// Method: the normal quantile of a 95% interval, for the estimates the monitoring shows.
const Z95: f64 = 1.96;

/// The value model's coefficients with their 95% intervals (unit `coefficient`, logit scale), from `n`
/// labels, then the spread of the session, lesson and judge effects (unit `logit`, a standard
/// deviation, shown as a point).
fn effects(m: &select::policy::Model, n: u32) -> Vec<Estimate> {
    let p = select::policy::P;
    if m.beta.len() != p || m.chol.len() != p * p {
        return Vec::new();
    }
    // The covariance is chol · cholᵀ: a coefficient's variance is the sum of squares of its row.
    let sd = |k: usize| m.chol[k * p..=k * p + k].iter().map(|x| x * x).sum::<f64>().sqrt();
    let mut out: Vec<Estimate> = (0..p)
        .map(|k| Estimate {
            name: select::policy::NAMES[k].into(),
            unit: "coefficient".into(),
            mean: m.beta[k],
            lo: m.beta[k] - Z95 * sd(k),
            hi: m.beta[k] + Z95 * sd(k),
            n: u64::from(n),
        })
        .collect();
    for (name, v) in ["session spread", "lesson spread", "judge spread"].into_iter().zip(m.spread) {
        out.push(Estimate { name: name.into(), unit: "logit".into(), mean: v, lo: v, hi: v, n: u64::from(n) });
    }
    out
}

/// The selection of the store whose lessons file is `lessons`, under `policy` (select.rs `policy`).
#[must_use]
pub fn selection(lessons: &Path, policy: select::Policy) -> Selection {
    Selection::of(&select::summary::load(lessons), policy)
}

/// `s` as one line (`borg status`, `borg top`): what decides (the default named as such, a policy
/// opted into by its switch), then the checks folded, how often the policy deviated from the fixed
/// rule, the learned decisions the fixed rule took over per reason (only those that happened), what
/// the checks measure for the learned policy (placebos), the monitor's reading (the
/// auto-revert's evidence) with its weights' effective number and largest weight, the stops' latency
/// per harness against its stop budget, and what selection spent per session; a budget and its price
/// only where an engineer set one.
#[must_use]
pub fn selection_line(s: &Selection) -> String {
    let default = s.source == s.default;
    let head = match s.source {
        select::REVERTED_SOURCE => "REVERTED to the fixed rule: the monitor saw harm beyond the gate's share (sticky)".to_string(),
        "learned" => {
            let opt = if default { "" } else { " (opt-in, BORG_SELECT=learned)" };
            if s.gate > 0.0 {
                format!("learned{opt}, gate {:.3} (quality it may still risk below the fixed rule)", s.gate)
            } else {
                format!("learned{opt}, cold start: gate 0, every decision the fixed rule's")
            }
        }
        "fixed" if default => "the fixed rule (the default; BORG_SELECT=learned opts in to the learned policy)".into(),
        "fixed" => "the fixed rule (BORG_SELECT=fixed)".into(),
        "cap" => "the fixed rule with an ask cap (BORG_STOP_ASK_CAP, evaluation only)".into(),
        "attention" => "attention's cut (BORG_STOP_ATTENTION=on, evaluation only)".into(),
        other => other.to_string(),
    };
    if s.checks == 0 {
        return format!("{head} · no graded check folded yet");
    }
    let mut parts = vec![head, format!("{} checks", s.checks)];
    if !s.deviations.is_empty() {
        parts.push(format!("deviated: {}", s.deviations.iter().map(|c| format!("{} {}/{}", c.name, c.part, c.whole)).collect::<Vec<_>>().join(", ")));
    }
    let fell: Vec<String> = s.fallbacks.iter().filter(|c| c.part > 0).map(|c| format!("{} {}", c.name, c.part)).collect();
    if !fell.is_empty() {
        parts.push(format!("learned decisions the fixed rule took over: {}", fell.join(", ")));
    }
    if let Some(m) = &s.measurement {
        parts.push(format!("placebos {} drawn, {} judged", m.placebos, m.judged));
        if m.tail_shown > 0 {
            parts.push(format!("tail {} shown past the pool, {} applied", m.tail_shown, m.tail_applied));
        }
    }
    if let Some(e) = &s.evaluation {
        let reading = match e.ucb {
            Some(u) => format!("monitor {:+.3}, bound {u:+.3} over {} sessions (below 0 reverts)", e.mean, e.sessions),
            None => format!("monitor {:+.3} over {} sessions, no bound yet", e.mean, e.sessions),
        };
        parts.push(format!("{reading}, ESS {:.0} of {} weighed, largest weight {:.1}× the mean", e.ess, e.units, e.max_weight));
    }
    if !s.latency.is_empty() {
        let secs = |ms: u64| format!("{:.1} s", ms as f64 / 1000.0);
        let each: Vec<String> = s
            .latency
            .iter()
            .map(|l| {
                let budget = l.budget_ms.map_or_else(String::new, |b| format!(" of {}", secs(b)));
                let failed = if l.over > 0 { format!(", {} failed", l.over) } else { String::new() };
                format!("{} {}{budget}{failed}", l.harness, secs(l.p95_ms))
            })
            .collect();
        parts.push(format!("stop p95 {}", each.join("; ")));
    }
    if s.sessions > 0 && !s.spend.is_empty() {
        let n = s.sessions as f64;
        let (usd, rounds) = s.spend.iter().fold((0.0, 0), |(u, r), x| (u + x.usd, r + x.rounds));
        parts.push(format!("${:.2} and {:.1} extra rounds per session ({} sessions)", usd / n, rounds as f64 / n, s.sessions));
    }
    if let Some(b) = &s.budget {
        parts.push(format!("budget {:.0} pool tokens per session: {:.0} read, price {:.2e} per token", b.tokens, b.spent, b.price));
    }
    parts.join(" · ")
}

/// `render` with the collective's selection line under it, when `borg top` shows one (`Selection::shown`).
#[must_use]
pub fn render_with(name: &str, share: Share, drones: &[Drone], inbox: &Inbox, select: Option<&Selection>, now: i64) -> String {
    let mut s = render(name, share, drones, inbox, now);
    if let Some(sel) = select.filter(|s| s.shown()) {
        s.push_str(&format!("  selection: {}\n", selection_line(sel)));
    }
    s
}

/// The collectives to show: the one `dir` routes to, or every one on this machine.
fn routes(dir: Option<&str>) -> Result<Vec<Route>, String> {
    let cfg = collective::load()?;
    let base = learn::home();
    Ok(match dir {
        Some(d) => vec![collective::route(cfg.as_ref(), d, &base)].into_iter().filter(|r| *r != Route::Off).collect(),
        None => install::collectives(cfg.as_ref(), &base),
    })
}

/// Every collective to show with its drones (top_live.rs).
pub fn gather(dir: Option<&str>, now: i64) -> Result<Vec<crate::top_live::Coll>, String> {
    let mut out = Vec::new();
    for r in routes(dir)? {
        let Route::Collective { name, home, share } = &r else { continue };
        presence::prune(home, now);
        let mut ds = drones(home, collective::may_sync(&r), now);
        for d in &mut ds {
            d.p.engineer = collective::aliases().display(&d.p.engineer);
        }
        let inbox = inbox(home, &ds, now);
        if ds.is_empty() && inbox.recent.is_empty() && dir.is_none() {
            continue;
        }
        // Every frame: the cache and its tail, never a rebuild of the whole log (select/summary.rs `peek`);
        // the checks' hooks keep the cache current.
        let summary = select::summary::peek(&home.join("lessons.jsonl"));
        let select = Some(Selection::of(&summary, select::policy())).filter(Selection::shown).map(|s| selection_line(&s));
        out.push(crate::top_live::Coll { name: name.clone(), home: home.clone(), share: *share, drones: ds, inbox, select });
    }
    Ok(out)
}

fn screen(dir: Option<&str>, json_out: bool, now: i64) -> Result<String, String> {
    Ok(screen_of(&routes(dir)?, dir.is_some(), &engineer_dir(dir), msg::caller, json_out, now))
}

/// The printed screen (or `--json`) of the collectives `routes`, `one` when `--dir` named the one to
/// show (shown even with nothing in it); when `caller` finds no session (`engineer_looks`), the text
/// screen acknowledges the messages to the engineer of the repo at `cwd` whose text it prints
/// (`prints_text`, `acknowledge`).
fn screen_of(
    routes: &[Route],
    one: bool,
    cwd: &str,
    caller: impl Fn(&[Drone]) -> Result<Option<(String, &'static str)>, String>,
    json_out: bool,
    now: i64,
) -> String {
    let mut text = String::new();
    let mut all = Vec::new();
    // Every collective's drones, and what each store's text screen printed: whether the engineer is
    // the one looking, and so acknowledges it, is known once all are gathered.
    let (mut gathered, mut printed) = (Vec::new(), Vec::new());
    for r in routes {
        let Route::Collective { name, home, share } = r else { continue };
        presence::prune(home, now);
        let mut ds = drones(home, collective::may_sync(r), now);
        // An engineer's several emails show as their name (collective.rs "Engineers"), peers' too.
        for d in &mut ds {
            d.p.engineer = collective::aliases().display(&d.p.engineer);
        }
        let inbox = inbox(home, &ds, now);
        let sel = selection(&home.join("lessons.jsonl"), select::policy());
        if json_out {
            let mut v = to_json_with(name, *share, &ds, &inbox);
            v["select"] = json!(sel);
            all.push(v);
        } else if !ds.is_empty() || !inbox.recent.is_empty() || one {
            text.push_str(&render_with(name, *share, &ds, &inbox, Some(&sel), now));
            text.push('\n');
            printed.push((home, inbox.recent.into_iter().filter(|m| prints_text(&ds, &inbox.here, m)).collect::<Vec<_>>()));
        }
        gathered.push(ds);
    }
    if !printed.is_empty() && engineer_looks(caller, gathered.iter().map(Vec::as_slice)) {
        let mut done = HashSet::new();
        for (home, recent) in &printed {
            acknowledge(home, cwd, &recent.iter().collect::<Vec<_>>(), &mut done, now);
        }
    }
    if json_out {
        return serde_json::to_string_pretty(&all).unwrap_or_default() + "\n";
    }
    if text.is_empty() {
        text = "no drones (borg top lists sessions whose hooks ran in the last day)\n".into();
    }
    text
}

pub fn main(args: &[&str]) -> i32 {
    let (mut dir, mut once, mut json_out) = (None, false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--dir" if dir.is_none() => match it.next() {
                Some(d) => dir = Some(*d),
                None => return usage("--dir needs a value"),
            },
            "--once" => once = true,
            "--json" => json_out = true,
            a => return usage(&format!("unknown flag {a}")),
        }
    }
    // SAFETY: isatty only inspects the descriptor.
    if !json_out && !once && unsafe { libc::isatty(1) } == 1 {
        return crate::top_live::run(dir);
    }
    match screen(dir, json_out, learn::now_ms()) {
        Ok(s) => {
            print!("{s}");
            0
        }
        Err(e) => {
            eprintln!("borg top: {e}");
            1
        }
    }
}

fn usage(why: &str) -> i32 {
    eprintln!("borg top: {why}\nusage: borg top [--once] [--json] [--dir DIR]");
    2
}

#[cfg(test)]
pub(crate) fn tests_fixture() -> Vec<Drone> {
    tests::fixture()
}

#[cfg(test)]
pub(crate) fn tests_inbox() -> Inbox {
    tests::inbox_fixture()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(session: &str, engineer: &str, started: i64, ms: i64, event: &str, prompt: &str, files: &[&str]) -> Presence {
        Presence {
            session: session.into(),
            harness: "claude".into(),
            engineer: engineer.into(),
            repo: "borg-ai".into(),
            collective: "corp".into(),
            prompt: prompt.into(),
            files: files.iter().map(|f| f.to_string()).collect(),
            event: event.into(),
            ms,
            started,
            pid: None,
        }
    }

    const NOW: i64 = 10_000_000;

    pub(crate) fn fixture() -> Vec<Drone> {
        let local = vec![
            p("aaaa1111-x", "alice@x", 100, NOW - 5_000, "post_tool", "fix the flaky sync test", &["crates/borg-mesh/src/sync.rs", "src/recon.rs"]),
            p("cccc3333-x", "alice@x", 300, NOW - presence::IDLE_MS - 60_000, "stop", "write the release notes for the mesh daemon and more words", &[]),
        ];
        let remote = vec![presence::Remote {
            p: p("bbbb2222-y", "bob@y", 200, NOW - 120_000, "stop", "review PR 42", &["README.md"]),
            key: "beefcafe".repeat(8),
            sig: String::new(),
        }];
        assemble(local, remote, NOW, |_| true, |p| p.session.starts_with("aaaa"))
    }

    fn message(by: &str, name: &str, to: &str, kind: msg::Kind, ms: i64, text: &str) -> Message {
        message_on("n1", by, name, to, kind, ms, text)
    }

    fn message_on(node: &str, by: &str, name: &str, to: &str, kind: msg::Kind, ms: i64, text: &str) -> Message {
        let from = msg::Sender { by: by.into(), name: name.into(), harness: "claude".into(), node: node.into() };
        Message::new(kind, &from, to, None, ms, text)
    }

    pub(crate) fn inbox_fixture() -> Inbox {
        Inbox {
            unread: [("aaaa1111-x".to_string(), 2), ("cccc3333-x".to_string(), 0)].into_iter().collect(),
            recent: vec![
                message("cccc3333-x", "Three of Three", "aaaa1111-x", msg::ASK, NOW - 60_000, "which   test\nflakes?"),
                message("engineer:alice@x", "alice@x", msg::ALL, msg::TELL, NOW - 120_000, "release at five"),
                message_on(&"feed".repeat(16), "gone-session", "Six of Six", "zzzz9999-q", msg::TELL, NOW - 180_000, "done"),
            ],
            day: 3,
            here: "n1".into(),
            listen: [("aaaa1111-x".to_string(), crate::listen::Status::Waits), ("cccc3333-x".to_string(), crate::listen::Status::Listens)]
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn renders_one_screen_per_collective() {
        let out = render("corp", Share::Team, &fixture(), &inbox_fixture(), NOW);
        let expected = "\
corp (team): 3 drone(s), 2 active, 1 remote
  DRONE              SESSION  ENGINEER                 REPO           STATE    WAKE     IDLE UNREAD  DOING                                    FILES
  One of Three       aaaa111… alice@x                  borg-ai        paused   waiter     5s      2  fix the flaky sync test                  crates/borg-mesh/src/sync.rs, src/recon.rs
  Two of Three       bbbb222… bob@y @beefca            borg-ai        waiting  -          2m      -  review PR 42                             README.md
  Three of Three     cccc333… alice@x                  borg-ai        idle     on        31m      0  write the release notes for the mesh da…
  messages: 3 in the last day, 2 unread
      1m  Three of Three asks -> One of Three: which test flakes?
      2m  alice@x -> collective: release at five
      3m  Six of Six @feedfeedfeed -> zzzz9999: done
";
        assert_eq!(out, expected, "\n{out}");
        let none = Inbox::default();
        assert_eq!(render("private", Share::Local, &[], &none, NOW), "private (local, never leaves this machine): 0 drone(s), 0 active, 0 remote\n");
    }

    /// A store's selection after 480 checks: the learned policy opted into, with gate budget, its route
    /// deviations, the placebos the checks drew, the monitor's reading and weights, spend from both
    /// check channels.
    fn learned() -> Selection {
        let spend = |source: &str, rounds, usd| Spend { source: source.into(), calls: 1, rounds, usd };
        let evaluation = Evaluation { sessions: 12, mean: 0.004, ucb: Some(0.031), units: 300, ess: 180.0, max_weight: 6.5, truncated: 3 };
        Selection {
            policy: "learned",
            default: "fixed",
            source: "learned",
            gate: 0.12,
            checks: 480,
            sessions: 12,
            deviations: vec![Count { name: "route".into(), part: 5, whole: 400 }],
            spend: vec![spend("stop", 4, 3.0), spend("live", 0, 2.04)],
            evaluation: Some(evaluation),
            measurement: Some(Measurement { placebos: 40, judged: 31, tail_shown: 12, tail_applied: 3 }),
            ..Selection::default()
        }
    }

    #[test]
    fn the_selection_line_says_what_decides_and_prices_spend_only_under_a_budget() {
        use select::Policy;
        use select::summary::Summary;
        let fresh = |policy, default| Selection { default, ..Selection::of(&Summary::default(), policy) };
        // The fixed rule as the default names itself so and the opt-in; the learned policy opted into says so.
        let fixed = fresh(Policy::Fixed, "fixed");
        assert_eq!(selection_line(&fixed), "the fixed rule (the default; BORG_SELECT=learned opts in to the learned policy) · no graded check folded yet");
        assert!(!fixed.shown(), "the default with nothing folded: nothing to show on the drones screen");
        let opted = fresh(Policy::Learned, "fixed");
        assert_eq!(
            selection_line(&opted),
            "learned (opt-in, BORG_SELECT=learned), cold start: gate 0, every decision the fixed rule's · no graded check folded yet"
        );
        assert!(opted.shown(), "a switch decides: shown");
        // Where the learned policy is the default, the fixed rule is the switch.
        assert_eq!(selection_line(&fresh(Policy::Fixed, "learned")), "the fixed rule (BORG_SELECT=fixed) · no graded check folded yet");
        assert!(!fresh(Policy::Learned, "learned").shown() && fresh(Policy::Fixed, "learned").shown());
        let d = select::precedence(None, None, false);
        assert_eq!(Selection::of(&Summary::default(), Policy::Cap(2)).default, d.name(), "the default is select.rs's, no switch set");
        assert!(!Selection::of(&Summary::default(), d).shown());
        let mut s = learned();
        let line = selection_line(&s);
        assert_eq!(
            line,
            "learned (opt-in, BORG_SELECT=learned), gate 0.120 (quality it may still risk below the fixed rule) · 480 checks · deviated: route 5/400 · placebos 40 drawn, 31 judged · tail 12 shown past the pool, 3 applied · monitor +0.004, bound +0.031 over 12 sessions (below 0 reverts), ESS 180 of 300 weighed, largest weight 6.5× the mean · $0.42 and 0.3 extra rounds per session (12 sessions)"
        );
        assert!(!line.contains("budget") && !line.contains("price"), "no budget set: spend measured, never priced: {line}");
        let mut early = learned();
        early.evaluation = Some(Evaluation { ucb: None, sessions: 3, units: 40, ess: 12.4, max_weight: 3.0, ..Evaluation::default() });
        let line = selection_line(&early);
        assert!(line.contains(" · monitor +0.000 over 3 sessions, no bound yet, ESS 12 of 40 weighed, largest weight 3.0× the mean · "), "{line}");
        early.latency = vec![
            Latency { harness: "codex".into(), checks: 20, p50_ms: 10_000, p95_ms: 19_000, budget_ms: Some(30_000), over: 1 },
            Latency { harness: "claude".into(), checks: 1, p50_ms: 4_000, p95_ms: 4_000, budget_ms: None, over: 0 },
        ];
        let line = selection_line(&early);
        assert!(line.contains(" · stop p95 codex 19.0 s of 30.0 s, 1 failed; claude 4.0 s · $"), "per harness, against its stop budget: {line}");
        s.budget = Some(Budget { tokens: 30_000.0, spent: 24_000.0, price: 0.0002 });
        assert!(selection_line(&s).ends_with(" · budget 30000 pool tokens per session: 24000 read, price 2.00e-4 per token"), "{}", selection_line(&s));
        s.source = select::REVERTED_SOURCE;
        assert!(selection_line(&s).starts_with("REVERTED to the fixed rule"), "{}", selection_line(&s));
    }

    /// `borg status`'s line, read from a store: one with nothing folded decides by the fixed rule under
    /// whichever policy; a switch names itself; no budget is mentioned.
    #[test]
    fn a_store_says_how_its_lessons_are_selected() {
        use select::Policy;
        let home = std::env::temp_dir().join(format!("borg-top-select-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let lessons = home.join("lessons.jsonl");
        std::fs::write(&lessons, "").unwrap();
        let line = |lessons: &Path, policy| selection_line(&selection(lessons, policy));
        assert!(line(&lessons, Policy::Learned).ends_with("cold start: gate 0, every decision the fixed rule's · no graded check folded yet"));
        assert!(line(&lessons, Policy::Fixed).starts_with("the fixed rule ("), "{}", line(&lessons, Policy::Fixed));
        assert!(line(&lessons, Policy::Cap(2)).contains("(BORG_STOP_ASK_CAP, evaluation only)"));
        let absent = home.join("absent").join("lessons.jsonl");
        assert!(line(&absent, Policy::Attention).contains("(BORG_STOP_ATTENTION=on, evaluation only)"), "no store: the prior");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `borg top`'s screen keeps `render`'s text and adds the selection line under it only when shown.
    #[test]
    fn the_selection_line_follows_the_screen_when_shown() {
        let (ds, inbox) = (fixture(), inbox_fixture());
        let plain = render("corp", Share::Team, &ds, &inbox, NOW);
        assert_eq!(render_with("corp", Share::Team, &ds, &inbox, None, NOW), plain);
        let cold = Selection::of(&select::summary::Summary::default(), select::precedence(None, None, false));
        assert_eq!(render_with("corp", Share::Team, &ds, &inbox, Some(&cold), NOW), plain, "the default at cold start: no line");
        let s = learned();
        assert_eq!(render_with("corp", Share::Team, &ds, &inbox, Some(&s), NOW), format!("{plain}  selection: {}\n", selection_line(&s)));
    }

    /// The fields borg web's Conscience tab reads (ui api.rs `parse_select`); a budget is null unless set.
    #[test]
    fn the_selection_serialises_every_section_for_the_page() {
        let v = serde_json::to_value(learned()).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        let want = "at budget checks declaration default delivery deviations effects evaluation fallbacks fit gate half_life labels latency measurement policy pool sessions source spend";
        assert_eq!(keys, want.split(' ').collect::<Vec<_>>());
        assert_eq!((&v["budget"], &v["spend"][0]["source"], &v["deviations"][0]["whole"]), (&Value::Null, &json!("stop"), &json!(400)));
        let mut keys: Vec<&str> = v["evaluation"].as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["ess", "max_weight", "mean", "sessions", "truncated", "ucb", "units"]);
        assert_eq!((&v["default"], &v["measurement"]), (&json!("fixed"), &json!({"placebos": 40, "judged": 31, "tail_shown": 12, "tail_applied": 3})));
    }

    /// Label health's judged share of a route counts the applies a verdict can come of: a free apply of
    /// a lesson its session already had (an older fixed rule's, delivery dropped it as given before)
    /// is out of it, as it is out of the summary's delivery tallies; a question of a lesson handed over
    /// and not acted on is delivered and judged, so in.
    #[test]
    fn the_judged_share_leaves_out_what_its_session_already_had() {
        use crate::outcome::{self, Outcome};
        use select::summary::{Summary, fold};
        let rec = |kind: &str, check: &str, channel: &str, ms: i64| Outcome {
            kind: kind.into(),
            session: "s".into(),
            lessons: vec!["A".into()],
            check: check.into(),
            channel: channel.into(),
            ms,
            ..Default::default()
        };
        let graded = |check: &str, ms: i64| Outcome {
            grades: vec![2],
            ranks: vec![0],
            applied: 1,
            free: vec!["A".into()],
            pf: vec![1.0],
            policy: "fixed".into(),
            shown: 20,
            tokens: 6_000,
            ..rec(outcome::GRADED, check, "live", ms)
        };
        let log = [
            graded("c1", 10),
            Outcome { route: outcome::ROUTE_FREE.into(), ..rec(outcome::DELIVERY, "c1", "", 11) },
            Outcome { judge: 2, ..rec(outcome::CHANGED, "c1", "queued", 12) },
            graded("c2", 20),
            Outcome { route: outcome::ROUTE_DROPPED_DONE.into(), ..rec(outcome::DELIVERY, "c2", "", 21) },
            // B, handed over early and not acted on, is asked at c3: delivered, then judged.
            Outcome { lessons: vec!["B".into()], ..rec(outcome::ASKED, "", "early", 25) },
            Outcome { lessons: vec!["B".into()], grades: vec![3], chosen: vec!["B".into()], free: Vec::new(), p: vec![1.0], pf: vec![0.0], ..graded("c3", 30) },
            Outcome { lessons: vec!["B".into()], route: outcome::ROUTE_ASKED.into(), ..rec(outcome::DELIVERY, "c3", "", 31) },
            Outcome { lessons: vec!["B".into()], judge: 2, ..rec(outcome::CHANGED, "c3", "live", 32) },
        ];
        let text: String = log.iter().map(|o| format!("{}\n", serde_json::to_string(o).unwrap())).collect();
        let s = fold(&text, Summary::default());
        assert_eq!(s.applies.iter().map(|a| a.had).collect::<Vec<_>>(), [false, true, true], "c2's A was handed over at c1, c3's B early");
        let sel = Selection::of(&s, select::Policy::Fixed);
        let row = |name: &str| sel.labels.iter().find(|c| c.name == name).map(|c| (c.part, c.whole));
        assert_eq!(row("judged, free"), Some((1, 1)), "c1's delivered and judged; c2's had, out: {:?}", sel.labels);
        assert_eq!(row("judged, asked"), Some((1, 1)), "c3's question of what B's session had, judged: {:?}", sel.labels);
    }

    /// The placebos a fixed-rule store's checks drew, and those judged, as the summary folds them:
    /// a placebo judged ignored counts, one without a verdict is drawn only, a free lesson's verdict is
    /// not a placebo's; a tail sample's lessons shown and applied; a store with nothing folded has no
    /// measurement.
    #[test]
    fn the_measurement_counts_the_placebos_the_fixed_rule_drew_and_judged() {
        use crate::outcome::{self, Outcome};
        use select::summary::{Summary, fold};
        let rec = |kind: &str, lessons: &[&str], check: &str, channel: &str, ms: i64| Outcome {
            kind: kind.into(),
            session: "s".into(),
            lessons: lessons.iter().map(|l| l.to_string()).collect(),
            check: check.into(),
            channel: channel.into(),
            ms,
            ..Default::default()
        };
        let graded = Outcome {
            grades: vec![1, 1, 2],
            ranks: vec![0, 1, 2],
            applied: 3,
            free: vec!["C".into()],
            placebo: vec!["A".into(), "B".into()],
            psi: 0.5,
            policy: "fixed".into(),
            shown: 20,
            tokens: 6_000,
            ..rec(outcome::GRADED, &["A", "B", "C"], "c1", "stop", 10)
        };
        // A pool the budget cut, its tail sample of two past it, T2 of them applied.
        let tailed = Outcome {
            grades: vec![1],
            ranks: vec![0],
            applied: 1,
            policy: "fixed".into(),
            shown: 20,
            tokens: 6_000,
            tail: vec!["T1".into(), "T2".into()],
            pt: 0.5,
            fill: vec![21, 25, 21],
            ..rec(outcome::GRADED, &["T2"], "c2", "stop", 14)
        };
        let log = [
            graded,
            tailed,
            Outcome { route: outcome::ROUTE_PLACEBO.into(), ..rec(outcome::DELIVERY, &["A"], "c1", "", 11) },
            Outcome { route: outcome::ROUTE_PLACEBO.into(), ..rec(outcome::DELIVERY, &["B"], "c1", "", 11) },
            Outcome { judge: 2, ..rec(outcome::IGNORED, &["A"], "c1", outcome::PLACEBO, 12) },
            Outcome { judge: 2, ..rec(outcome::CHANGED, &["C"], "c1", "queued", 13) },
        ];
        let text: String = log.iter().map(|o| format!("{}\n", serde_json::to_string(o).unwrap())).collect();
        let sel = Selection::of(&fold(&text, Summary::default()), select::Policy::Fixed);
        assert_eq!(sel.measurement, Some(Measurement { placebos: 2, judged: 1, tail_shown: 2, tail_applied: 1 }), "{sel:?}");
        assert!(selection_line(&sel).contains(" · 2 checks · placebos 2 drawn, 1 judged · tail 2 shown past the pool, 1 applied"), "{}", selection_line(&sel));
        assert_eq!(Selection::of(&Summary::default(), select::Policy::Fixed).measurement, None, "nothing folded: not measured");
    }

    /// The learned decisions the fixed rule took over, per reason, as the summary folds them from the
    /// checks' records: every reason a row of the graded checks folded, the line naming those that
    /// happened; nothing folded, no rows (not measured), and none happened, nothing on the line.
    #[test]
    fn the_selection_counts_the_learned_decisions_the_fixed_rule_took_over_by_reason() {
        use crate::outcome::{self, Outcome};
        use select::summary::{Summary, fold};
        let graded = |check: &str, policy: &str, reason: &str, ms: i64| Outcome {
            kind: outcome::GRADED.into(),
            session: "s".into(),
            lessons: vec!["A".into()],
            grades: vec![1],
            ranks: vec![0],
            applied: 1,
            free: vec!["A".into()],
            pf: vec![1.0],
            channel: "live".into(),
            check: check.into(),
            policy: policy.into(),
            reason: reason.into(),
            ms,
            ..Default::default()
        };
        let log = [
            graded("c1", "learned", "", 1),
            graded("c2", "fixed", outcome::DECIDED_BUSY, 2),
            graded("c3", "fixed", outcome::DECIDED_NO_RECORD, 3),
            graded("c4", "fixed", outcome::DECIDED_BUSY, 4),
        ];
        let lines = |log: &[Outcome]| log.iter().map(|o| format!("{}\n", serde_json::to_string(o).unwrap())).collect::<String>();
        let sel = Selection::of(&fold(&lines(&log), Summary::default()), select::Policy::Learned);
        let rows: Vec<(&str, u64, u64)> = sel.fallbacks.iter().map(|c| (c.name.as_str(), c.part, c.whole)).collect();
        assert_eq!(rows, [("lock_busy", 2, 4), ("lock_failed", 0, 4), ("unrecorded", 1, 4)]);
        let line = selection_line(&sel);
        assert!(line.contains(" · 4 checks · learned decisions the fixed rule took over: lock_busy 2, unrecorded 1 · "), "{line}");
        let none = Selection::of(&fold(&lines(&log[..1]), Summary::default()), select::Policy::Learned);
        assert!(none.fallbacks.iter().all(|c| c.part == 0) && none.fallbacks.len() == 3, "{none:?}");
        assert!(!selection_line(&none).contains("took over"), "{}", selection_line(&none));
        assert!(Selection::of(&Summary::default(), select::Policy::Learned).fallbacks.is_empty(), "nothing folded: not measured");
    }

    /// The sections the summary holds are filled from it: per known harness its stops' latency against
    /// the harness's stop budget (failed calls counted, live and unknown harnesses' left out), the
    /// monitor's reading and weights, the fitted delivery rates; a summary without them leaves them
    /// empty.
    #[test]
    fn the_selection_fills_latency_evaluation_and_delivery_from_the_summary() {
        use select::summary::{Summary, fold};
        let call = |harness: &str, channel: &str, dur_ms: u64, failed: bool| {
            let o = crate::outcome::Outcome {
                kind: "graded".into(),
                session: "s".into(),
                channel: channel.into(),
                harness: harness.into(),
                dur_ms,
                failed,
                tokens: 6_000,
                ..Default::default()
            };
            format!("{}\n", serde_json::to_string(&o).unwrap())
        };
        let mut log: String = (1..=20).map(|k| call("codex", "stop", k * 1_000, k == 20)).collect();
        log += &call("claude", "stop", 4_000, false);
        log += &call("codex", "live", 99_000, false);
        log += &call("nonesuch", "stop", 5_000, false);
        let mut s = fold(&log, Summary::default());
        s.monitor = select::policy::Monitor {
            sessions: 12,
            units: 400,
            mean: 0.004,
            ucb: Some(0.031),
            labelled: 300,
            ess: 180.0,
            max_weight: 6.5,
            truncated: 3,
            ..Default::default()
        };
        s.fit.key = "fit".into();
        s.fit.rates = select::policy::Rates { free_stop: 0.6, free_live: 0.8, later: 0.4, wait: 0.25, ask_live: 1.0 };
        let sel = Selection::of(&s, select::Policy::Learned);
        let budget = |h: &str| crate::harness::ALL.iter().find(|x| x.name() == h).and_then(|x| x.caps().stop_budget).map(|b| b.as_millis() as u64);
        assert!(budget("codex").is_some() && budget("claude").is_none(), "codex names a stop budget, claude none");
        let want = vec![
            Latency { harness: "claude".into(), checks: 1, p50_ms: 4_000, p95_ms: 4_000, budget_ms: None, over: 0 },
            Latency { harness: "codex".into(), checks: 20, p50_ms: 10_000, p95_ms: 19_000, budget_ms: budget("codex"), over: 1 },
        ];
        assert_eq!(sel.latency, want);
        let e = sel.evaluation.as_ref().expect("the monitor weighed labelled applies");
        assert_eq!(*e, Evaluation { sessions: 12, mean: 0.004, ucb: Some(0.031), units: 300, ess: 180.0, max_weight: 6.5, truncated: 3 });
        assert_eq!(sel.delivery.as_ref().map(|r| r.wait), Some(0.25));
        // Guard: a harness's newest `LATENCY_KEEP` stops are kept, the cache never grows past them.
        let keep = select::summary::LATENCY_KEEP as u64;
        let many: String = (1..=keep + 1).map(|k| call("claude", "stop", k, false)).collect();
        let kept = Selection::of(&fold(&many, Summary::default()), select::Policy::Learned);
        assert_eq!((kept.latency[0].checks, kept.latency[0].p50_ms), (keep, 1 + keep / 2), "the oldest call dropped");
        let cold = Selection::of(&Summary::default(), select::Policy::Learned);
        assert!(cold.latency.is_empty() && cold.evaluation.is_none() && cold.delivery.is_none(), "{cold:?}");
        assert_eq!((quantile(&[], 0.95), quantile(&[7], 0.5)), (0, 7));
    }

    /// The unread count is what the next prompt would show: a message shown once no longer counts,
    /// broadcasts only to drones running when sent, never its own; no log costs no read.
    #[test]
    fn unread_counts_follow_the_shown_once_claims() {
        let home = std::env::temp_dir().join(format!("borg-top-inbox-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let ds = fixture();
        assert!(inbox(&home, &ds, NOW).recent.is_empty());
        let here = msg::node(&home);
        msg::send(&home, message_on(&here, "cccc3333-x", "Three of Three", "aaaa1111-x", msg::TELL, NOW - 3_000, "look at recon"), NOW).unwrap();
        msg::send(&home, message_on(&here, "cccc3333-x", "Three of Three", msg::ALL, msg::TELL, NOW - 2_000, "lunch"), NOW).unwrap();
        let i = inbox(&home, &ds, NOW);
        assert_eq!(i.unread.get("aaaa1111-x"), Some(&2), "{i:?}");
        assert_eq!(i.unread.get("cccc3333-x"), Some(&0), "its own broadcast");
        assert!(!i.unread.contains_key(&ds[1].p.session), "remote: claimed on its node");
        assert_eq!((i.day, i.recent[0].text.as_str()), (2, "lunch"));
        let caps = crate::harness::by_name("claude").expect("a harness").caps();
        let ev = json!({"session_id": "aaaa1111-x"});
        assert!(msg::deliver(&home, caps, "prompt", &ev, Some(100), NOW, None).is_some());
        assert_eq!(inbox(&home, &ds, NOW).unread.get("aaaa1111-x"), Some(&0));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The acknowledgements (`by`, ids) in the store's messages log.
    fn seen_lines(home: &Path) -> Vec<(String, Vec<String>)> {
        msg::lines(home)
            .into_iter()
            .filter_map(|l| match l {
                msg::Line::Seen(s) => Some((s.by, s.ids)),
                msg::Line::Message(_) => None,
            })
            .collect()
    }

    /// The calling session msg.rs `caller` tells from a shell whose session variables are `sessions` and
    /// harness pids `pids` (its own `pick`), whatever the shell running the tests sets.
    fn shell(sessions: Vec<(String, &'static str)>, pids: Vec<(u32, &'static str)>) -> impl Fn(&[Drone]) -> Result<Option<(String, &'static str)>, String> {
        move |ds| msg::pick(sessions.clone(), &pids, ds)
    }

    /// A store at `base`/`store` and a repo at `base`/`work` whose engineer is ann@x: one message to the
    /// engineer, one to a session. Returns the store, the repo and the message to the engineer.
    fn acked_fixture(base: &Path) -> (std::path::PathBuf, String, Message) {
        let _ = std::fs::remove_dir_all(base);
        let (home, work) = (base.join("store"), base.join("work"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let git = |a: &[&str]| std::process::Command::new("git").arg("-C").arg(&work).args(a).output().is_ok_and(|o| o.status.success());
        assert!(git(&["init", "-q"]) && git(&["config", "user.email", "ann@x"]));
        let here = msg::node(&home);
        let to_me = message_on(&here, "aaaa1111-x", "One of Three", msg::ENGINEER, msg::TELL, NOW - 3_000, "the build is green");
        let to_drone = message_on(&here, "cccc3333-x", "Three of Three", "aaaa1111-x", msg::TELL, NOW - 2_000, "look at recon");
        msg::send(&home, to_me.clone(), NOW).unwrap();
        msg::send(&home, to_drone, NOW).unwrap();
        (home, work.display().to_string(), to_me)
    }

    /// The printed screen acknowledges a message to the engineer it shows as the engineer of the repo
    /// it runs in, as `borg inbox` does from a terminal; a message to a session is never the engineer's
    /// to acknowledge, and `--json`, for scripts, acknowledges nothing.
    #[test]
    fn the_screen_acknowledges_the_messages_to_the_engineer_it_prints() {
        let base = std::env::temp_dir().join(format!("borg-top-ack-{}", std::process::id()));
        let (home, cwd, to_me) = acked_fixture(&base);
        let routes = [Route::Collective { name: "corp".into(), home: home.clone(), share: Share::Local }];
        let terminal = || shell(Vec::new(), Vec::new());
        let json = screen_of(&routes, true, &cwd, terminal(), true, NOW);
        assert!(json.contains("the build is green") && seen_lines(&home).is_empty(), "--json acknowledges nothing: {json}");
        let text = screen_of(&routes, true, &cwd, terminal(), false, NOW);
        assert!(text.contains("the build is green") && text.contains("look at recon"), "{text}");
        assert_eq!(seen_lines(&home), [("engineer:ann@x".to_string(), vec![to_me.id.clone()])], "the engineer's message only, as theirs");
        screen_of(&routes, true, &cwd, terminal(), false, NOW + 1_000);
        assert_eq!(seen_lines(&home).len(), 1, "acknowledged once: a later look writes no second line");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A drone's shell runs the screen piped and in the engineer's repo: it prints the message to the
    /// engineer and acknowledges nothing as them, as `borg inbox` would not. A session variable, two
    /// that no presence tells apart, or opencode's pid naming a drone of another collective on the
    /// screen: each is a session's; the same screen from a terminal acknowledges it.
    #[test]
    fn a_session_running_the_screen_acknowledges_nothing_as_the_engineer() {
        let base = std::env::temp_dir().join(format!("borg-top-ack-session-{}", std::process::id()));
        let (home, cwd, to_me) = acked_fixture(&base);
        let other = base.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let pid = std::process::id();
        let oc = Presence { harness: "opencode".into(), pid: Some(pid), ..p("dddd4444-x", "ann@x", NOW - 9_000, NOW - 1_000, "prompt", "docs", &[]) };
        presence::write(&other, &oc).unwrap();
        let routes = [
            Route::Collective { name: "corp".into(), home: home.clone(), share: Share::Local },
            Route::Collective { name: "side".into(), home: other.clone(), share: Share::Local },
        ];
        let shells = [
            ("Claude Code", shell(vec![("eeee5555-x".into(), "claude")], Vec::new())),
            ("Codex in Claude Code, neither with presence", shell(vec![("eeee5555-x".into(), "claude"), ("ffff6666-x".into(), "codex")], Vec::new())),
            ("opencode, its drone in the other collective", shell(Vec::new(), vec![(pid, "opencode")])),
        ];
        for (what, caller) in shells {
            let text = screen_of(&routes, false, &cwd, caller, false, NOW);
            assert!(text.contains("the build is green"), "{what}: shown {text}");
            assert!(seen_lines(&home).is_empty() && seen_lines(&other).is_empty(), "{what}: acknowledged nothing as the engineer");
        }
        screen_of(&routes, false, &cwd, shell(Vec::new(), vec![(pid + 1, "opencode")]), false, NOW);
        assert_eq!(seen_lines(&home), [("engineer:ann@x".to_string(), vec![to_me.id.clone()])], "a terminal (no drone of its pid): the engineer's");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The printed screen acknowledges a message to the engineer only when its line, cut to
    /// `MESSAGE_CHARS`, still shows some of its text (`prints_text`), as the live screen does: a line
    /// whose sender and recipient fill it names only who wrote to whom, and its sender's window stays
    /// closed until the engineer reads the text elsewhere (`borg inbox`).
    #[test]
    fn the_printed_screen_acknowledges_only_the_messages_whose_text_it_prints() {
        let base = std::env::temp_dir().join(format!("borg-top-ack-cut-{}", std::process::id()));
        let (home, cwd, to_me) = acked_fixture(&base);
        // The engineer's address takes 60 columns, so a line's text starts after a sender's name + 66.
        let email = format!("{}@x", "a".repeat(58));
        assert!(std::process::Command::new("git").args(["-C", &cwd, "config", "user.email", &email]).output().is_ok_and(|o| o.status.success()));
        let (here, to) = (msg::node(&home), format!("{}:{email}", msg::ENGINEER));
        let one = message_on(&here, "bbbb2222-x", &"n".repeat(52), &to, msg::TELL, NOW - 1_500, "one character shows");
        let none = message_on(&here, "dddd4444-x", &"n".repeat(53), &to, msg::TELL, NOW - 1_000, "none shows");
        msg::send(&home, one.clone(), NOW).unwrap();
        msg::send(&home, none.clone(), NOW).unwrap();
        let routes = [Route::Collective { name: "corp".into(), home: home.clone(), share: Share::Local }];
        let text = screen_of(&routes, true, &cwd, shell(Vec::new(), Vec::new()), false, NOW);
        assert!(text.contains(&format!("{} -> {email}: o…", one.name)) && text.contains(&format!("{} -> {email}: …", none.name)), "{text}");
        let mut acked: Vec<String> = seen_lines(&home).into_iter().flat_map(|(by, ids)| ids.into_iter().map(move |id| format!("{by} {id}"))).collect();
        acked.sort();
        let mut want = [format!("{to} {}", to_me.id), format!("{to} {}", one.id)];
        want.sort();
        assert_eq!(acked, want, "the line that shows one character of its text is read; the one cut before it is not");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Guard (Q1 pacing): a live screen acknowledges a message once per screen, then skips it without
    /// asking git or reading the log's seen lines again, whatever the log holds by then; a message
    /// that arrives later is acknowledged at the frame that first shows it.
    #[test]
    fn a_live_screen_acknowledges_each_message_once() {
        let base = std::env::temp_dir().join(format!("borg-top-ack-once-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let home = base.clone();
        let here = msg::node(&home);
        let to_me = |by: &str, ms: i64, text: &str| message_on(&here, by, "a drone", msg::ENGINEER, msg::TELL, ms, text);
        let first = to_me("aaaa1111-x", NOW - 3_000, "the build is green");
        msg::send(&home, first.clone(), NOW).unwrap();
        let cwd = base.display().to_string();
        let mut done = HashSet::new();
        let recent = |now: i64| inbox(&home, &[], now).recent;
        acknowledge(&home, &cwd, &recent(NOW).iter().collect::<Vec<_>>(), &mut done, NOW);
        assert_eq!(seen_lines(&home).len(), 1, "{:?}", seen_lines(&home));
        assert!(done.contains(&first.id));
        // The log loses the acknowledgement (as a bound or a TTL may cut it): the next frame passes
        // the message by, its id already done.
        let kept: String = std::fs::read_to_string(msg::path(&home)).unwrap().lines().filter(|l| !l.contains("\"seen\"")).map(|l| format!("{l}\n")).collect();
        std::fs::write(msg::path(&home), kept).unwrap();
        acknowledge(&home, &cwd, &recent(NOW + 1_000).iter().collect::<Vec<_>>(), &mut done, NOW + 1_000);
        assert!(seen_lines(&home).is_empty(), "skipped: no second ack of a done id");
        // From another sender: the first's window holds its sender's next one until acknowledged.
        let second = to_me("cccc3333-x", NOW + 2_000, "and the docs");
        msg::send(&home, second.clone(), NOW + 2_000).unwrap();
        acknowledge(&home, &cwd, &recent(NOW + 3_000).iter().collect::<Vec<_>>(), &mut done, NOW + 3_000);
        assert_eq!(seen_lines(&home).into_iter().map(|(_, ids)| ids).collect::<Vec<_>>(), [vec![second.id.clone()]], "only the new one");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A line's `by` and `name` are its sender's claims: one from another node that names a local drone's
    /// session is not shown as that drone, and carries its node's key.
    #[test]
    fn a_sender_is_tied_to_the_node_its_line_names() {
        let ds = fixture();
        let own = message("aaaa1111-x", "anything", msg::ALL, msg::TELL, NOW, "hi");
        assert_eq!(sender(&ds, "n1", &own), "One of Three", "this node's drone");
        let forged = message_on("evil0123456789", "aaaa1111-x", "One of Three", msg::ALL, msg::TELL, NOW, "hi");
        assert_eq!(sender(&ds, "n1", &forged), "One of Three @evil01234567", "the claim, with the node it came from");
        let remote = message_on(&"beefcafe".repeat(8), &ds[1].p.session, "x", msg::ALL, msg::TELL, NOW, "hi");
        assert_eq!(sender(&ds, "n1", &remote), "Two of Three @beefcafebeef", "the remote drone on its own node");
        assert_eq!(recipient(&ds, "n1", "aaaa1111-x@n1"), "One of Three");
        assert_eq!(recipient(&ds, "n1", "aaaa1111-x@evil"), "aaaa1111", "not the local drone of that id");
        assert_eq!((recipient(&ds, "n1", "engineer"), recipient(&ds, "n1", msg::ALL)), ("engineer".into(), "collective".into()), "an `all` line");
        assert_eq!(recipient(&ds, "n1", "local@n1"), "local");
        assert_eq!(recipient(&ds, "n1", "local@beefcafebeefcafe00"), "local @beefcafebeef");
    }

    #[test]
    fn json_for_scripts() {
        let v = to_json_with("corp", Share::Team, &fixture(), &inbox_fixture());
        assert_eq!((v["drones"][0]["unread"].as_u64(), v["drones"][1].get("unread")), (Some(2), None));
        assert_eq!((v["messages"]["day"].as_u64(), v["messages"]["recent"][0]["kind"].as_str()), (Some(3), Some("ask")));
        assert_eq!(v["collective"], "corp");
        let d = &v["drones"];
        assert_eq!(d.as_array().unwrap().len(), 3);
        assert_eq!(d[0]["designation"], "One of Three");
        assert_eq!(d[0]["session"], "aaaa1111-x");
        assert_eq!(d[0]["state"], "working");
        assert_eq!(d[0]["paused"], true);
        assert_eq!(d[1]["node"], "beefcafe".repeat(8));
        assert_eq!(d[1]["state"], "waiting");
        assert_eq!(d[2]["state"], "idle");
        assert_eq!(d[2]["files"], json!([]));
    }

    /// Local first: a prefix or id that matches a drone here and one of a member names the one here.
    #[test]
    fn a_name_matching_here_and_on_a_member_names_the_drone_here() {
        let local = vec![p("aaaa1111-x", "alice@x", 100, NOW - 5_000, "post_tool", "a", &[])];
        let remote = ["aaaa1111-x", "aaaa2222-z"].map(|s| presence::Remote {
            p: p(s, "bob@y", 50, NOW - 5_000, "post_tool", "b", &[]),
            key: "beefcafe".repeat(8),
            sig: String::new(),
        });
        let ds = assemble(local, remote.into(), NOW, |_| true, |_| false);
        for t in ["aaaa", "aaaa1111", "aaaa1111-x"] {
            let d = resolve(&ds, t).unwrap().unwrap();
            assert!(d.node.is_empty() && d.p.session == "aaaa1111-x", "{t}");
        }
        assert_eq!(resolve(&ds, "aaaa2").unwrap().unwrap().node, "beefcafe".repeat(8), "only a member's matches");
        assert!(!resolve(&ds, "One").unwrap().unwrap().node.is_empty(), "a number names one drone of all, wherever it runs");
    }

    /// An ended session here does not shadow a live one on a member's machine: it would never read the message.
    #[test]
    fn a_name_matching_an_ended_drone_here_and_a_live_member_names_the_live_one() {
        let local = vec![p("aaaa1111-x", "alice@x", 100, NOW - 5_000, "end", "a", &[])];
        let remote =
            vec![presence::Remote { p: p("aaaa2222-z", "bob@y", 50, NOW - 5_000, "post_tool", "b", &[]), key: "beefcafe".repeat(8), sig: String::new() }];
        let ds = assemble(local, remote, NOW, |_| true, |_| false);
        let d = resolve(&ds, "aaaa").unwrap().unwrap();
        assert_eq!((d.p.session.as_str(), d.node.as_str()), ("aaaa2222-z", "beefcafe".repeat(8).as_str()), "the live one, though remote");
        assert!(resolve(&ds, "aaaa1111").unwrap().unwrap().node.is_empty(), "an ended drone is still named by its own id");
    }

    #[test]
    fn resolves_drones_by_designation_or_session() {
        let ds = fixture();
        assert_eq!(resolve(&ds, "Two").unwrap().unwrap().p.session, "bbbb2222-y");
        assert_eq!(resolve(&ds, "two of three").unwrap().unwrap().p.session, "bbbb2222-y");
        assert_eq!(resolve(&ds, "3").unwrap().unwrap().p.session, "cccc3333-x");
        assert_eq!(resolve(&ds, "aaaa").unwrap().unwrap().p.session, "aaaa1111-x");
        assert!(resolve(&ds, "all").unwrap().is_none());
        assert!(resolve(&ds, "Four").is_err());
        assert!(resolve(&ds, "zzzz").is_err());
        assert!(resolve(&ds, "aa").is_err(), "too short a prefix");
        assert!(["collective", "local"].iter().all(|w| resolve(&ds, w).unwrap().is_none()));
        assert_eq!(resolve(&ds, "bbbb").unwrap().unwrap().p.session, "bbbb2222-y", "nothing here matches: the member's");
        assert_eq!(word(21), "21");
    }
}
