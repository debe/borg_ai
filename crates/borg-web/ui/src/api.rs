//! borg-web's JSON API, parsed into typed views, and the live change stream that keeps them fresh.

use std::collections::BTreeMap;

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::util::day_num;

pub async fn get(url: String) -> Option<Value> {
    let r = gloo_net::http::Request::get(&url).send().await.ok()?;
    if !r.ok() {
        return None;
    }
    r.json::<Value>().await.ok()
}

/// POSTs `body` as JSON (a CORS preflight from any other site's page, which borg web never answers);
/// whether the server took it.
pub async fn post(url: String, body: &Value) -> bool {
    let Ok(req) = gloo_net::http::Request::post(&url).json(body) else { return false };
    req.send().await.is_ok_and(|r| r.ok())
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect()
}

fn map(v: &Value) -> BTreeMap<String, f64> {
    v.as_object().into_iter().flatten().map(|(k, x)| (k.clone(), f(x))).collect()
}

/// Counts, cost and time of a set of events.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Totals {
    pub counts: BTreeMap<String, f64>,
    pub usd: f64,
    pub usd_by: BTreeMap<String, f64>,
    /// The conscience's check cost per delivery mode (`hook`, `live`): part of `usd`, never stacked with `usd_by`.
    pub usd_mode: BTreeMap<String, f64>,
    /// Events per `<kind>@<channel>` (`changed@live`, `block@stop`).
    pub channels: BTreeMap<String, f64>,
    pub dur_ms: f64,
}

impl Totals {
    fn parse(v: &Value) -> Self {
        Totals {
            counts: map(&v["counts"]),
            usd: f(&v["usd"]),
            usd_by: map(&v["usd_by"]),
            usd_mode: map(&v["usd_mode"]),
            channels: map(&v["channels"]),
            dur_ms: f(&v["dur_ms"]),
        }
    }

    pub fn n(&self, kind: &str) -> f64 {
        self.counts.get(kind).copied().unwrap_or(0.0)
    }

    /// Events of `kind` that came through `channel` (`live`, `stop`).
    pub fn via(&self, kind: &str, channel: &str) -> f64 {
        self.channels.get(&format!("{kind}@{channel}")).copied().unwrap_or(0.0)
    }

    pub fn add(&mut self, o: &Totals) {
        for (k, v) in &o.counts {
            *self.counts.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &o.usd_by {
            *self.usd_by.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &o.usd_mode {
            *self.usd_mode.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &o.channels {
            *self.channels.entry(k.clone()).or_default() += v;
        }
        self.usd += o.usd;
        self.dur_ms += o.dur_ms;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Day {
    pub day: i64,
    pub t: Totals,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Engineer {
    pub name: String,
    pub t: Totals,
    pub days: Vec<Day>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub totals: Totals,
    /// Every day from the first to the last, zero-filled.
    pub days: Vec<Day>,
    pub engineers: Vec<Engineer>,
    pub nodes: u64,
    /// `borg debug ope`'s support: logged Thompson decisions with a pool, asked lessons without p, asked
    /// records with p but no pool.
    pub ope: (u64, u64, u64),
}

impl Stats {
    /// The totals of the last `n` days (all days when fewer).
    pub fn last(&self, n: usize) -> Totals {
        let mut t = Totals::default();
        for d in self.days.iter().rev().take(n) {
            t.add(&d.t);
        }
        t
    }

    /// The totals of the `n` days before the last `n`.
    pub fn before_last(&self, n: usize) -> Totals {
        let mut t = Totals::default();
        for d in self.days.iter().rev().skip(n).take(n) {
            t.add(&d.t);
        }
        t
    }

    /// A kind's daily values over the last `n` days, oldest first.
    pub fn series(&self, kind: &str, n: usize) -> Vec<f64> {
        let k = self.days.len().saturating_sub(n);
        self.days[k..].iter().map(|d| d.t.n(kind)).collect()
    }
}

fn days(v: &Value) -> Vec<Day> {
    let mut parsed: Vec<Day> =
        v.as_array().into_iter().flatten().filter_map(|d| Some(Day { day: day_num(d["day"].as_str()?)?, t: Totals::parse(&d["totals"]) })).collect();
    parsed.sort_by_key(|d| d.day);
    // Zero-fill: a chart's x axis is time, not the days that happened to have events.
    let (Some(first), Some(last)) = (parsed.first().map(|d| d.day), parsed.last().map(|d| d.day)) else { return parsed };
    let by: BTreeMap<i64, Totals> = parsed.into_iter().map(|d| (d.day, d.t)).collect();
    (first..=last).map(|day| Day { day, t: by.get(&day).cloned().unwrap_or_default() }).collect()
}

pub fn parse_stats(v: &Value) -> Stats {
    let engineers = v["engineers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| Engineer { name: s(&e["engineer"]), t: Totals::parse(&e["totals"]), days: days(&e["days"]) })
        .collect();
    let ope = &v["ope"];
    let n = |k: &str| ope[k].as_u64().unwrap_or(0);
    Stats {
        totals: Totals::parse(&v["totals"]),
        days: days(&v["days"]),
        engineers,
        nodes: v["nodes"].as_u64().unwrap_or(0),
        ope: (n("decisions"), n("no_p"), n("no_pool")),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lesson {
    pub id: String,
    pub question: String,
    pub situations: Vec<String>,
    pub consequence: String,
    pub area: String,
    pub scope: Vec<String>,
    pub source: String,
    pub origin_ms: f64,
    pub repo: String,
    pub version: u64,
    pub engineers: Vec<String>,
    pub activation: f64,
    pub changed: f64,
    pub ignored: f64,
    pub corrected: f64,
    /// Sessions whose work the engineer vetoed after it was shown (`borg no`).
    pub vetoed: f64,
    /// The weighted share of later fixes (or review findings) of decisions it was asked before.
    pub regretted: f64,
    /// Sessions whose own tests went green after it was asked.
    pub tested: f64,
    pub dormant: bool,
    pub consolidated: bool,
    pub usage: BTreeMap<String, f64>,
    /// Who hears it: empty for the team, `repo` for its repo only (reach.rs in borg).
    pub reach: String,
    /// Held by an outside store and no longer delivered to drones (`borg memory absorb`), from `/api/memory`.
    pub absorbed: Option<Absorbed>,
}

/// A lesson's absorb (borg memory_export.rs `Absorbed`) and its recurrence.
#[derive(Clone, Debug, PartialEq)]
pub struct Absorbed {
    /// The engineer's label of where it is held now: shown as given, never read.
    pub note: String,
    pub ms: f64,
    pub by: String,
    /// Corrections merged into the lesson since its absorb; None when the export cannot tell.
    pub recurrence: Option<f64>,
}

/// The absorbed lessons of an `/api/memory` document, by id.
pub fn parse_absorbed(v: &Value) -> BTreeMap<String, Absorbed> {
    v["lessons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|l| l["absorbed"].is_object())
        .map(|l| {
            let a = &l["absorbed"];
            (s(&l["id"]), Absorbed { note: s(&a["note"]), ms: f(&a["ms"]), by: s(&a["by"]), recurrence: l["recurrence"].as_f64() })
        })
        .collect()
}

impl Lesson {
    /// The system it belongs to: its first scope, or "general".
    pub fn system(&self) -> String {
        self.scope.first().cloned().unwrap_or_else(|| "general".into())
    }

    pub fn area(&self) -> String {
        if self.area.is_empty() { "other".into() } else { self.area.clone() }
    }

    /// Mean of the usefulness belief (memory.rs: Beta(1 + changed + ½ tested, 1 + ignored + 2 corrected
    /// + 3 vetoed + regretted)).
    pub fn usefulness(&self) -> f64 {
        let a = 1.0 + self.changed + 0.5 * self.tested;
        let b = 1.0 + self.ignored + 2.0 * self.corrected + 3.0 * self.vetoed + self.regretted;
        a / (a + b)
    }

    /// Times asked: in a turn-end block or by a live check (live mode).
    pub fn asked(&self) -> f64 {
        self.usage.get("block").copied().unwrap_or(0.0) + self.usage.get("live_ask").copied().unwrap_or(0.0)
    }

    pub fn source_label(&self) -> &'static str {
        match self.source.as_str() {
            "import" => "imported",
            "queen" => "queen answer",
            _ => "learned in session",
        }
    }

    /// Words the search box matches against (an absorbed lesson's note too).
    pub fn haystack(&self) -> String {
        let mut h =
            format!("{} {} {} {} {} {}", self.question, self.consequence, self.situations.join(" "), self.scope.join(" "), self.area, self.engineers.join(" "));
        if let Some(a) = &self.absorbed {
            h = format!("{h} {}", a.note);
        }
        h.to_lowercase()
    }
}

/// The lessons of `/api/lessons`, each marked with its absorb from `memory` (`parse_absorbed`).
pub fn parse_lessons(v: &Value, memory: &BTreeMap<String, Absorbed>) -> Vec<Lesson> {
    v["lessons"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|l| Lesson {
            id: s(&l["id"]),
            question: s(&l["question"]),
            situations: strs(&l["situations"]),
            consequence: s(&l["consequence"]),
            area: s(&l["area"]),
            scope: strs(&l["scope"]),
            source: s(&l["source"]),
            origin_ms: f(&l["origin_ms"]),
            repo: s(&l["repo"]),
            version: l["version"].as_u64().unwrap_or(1),
            engineers: strs(&l["engineers"]),
            activation: f(&l["activation"]),
            changed: f(&l["changed"]),
            ignored: f(&l["ignored"]),
            corrected: f(&l["corrected"]),
            vetoed: f(&l["vetoed"]),
            regretted: f(&l["regretted"]),
            tested: f(&l["tested"]),
            dormant: l["dormant"].as_bool().unwrap_or(false),
            consolidated: l["consolidated"].as_bool().unwrap_or(false),
            usage: map(&l["usage"]),
            reach: s(&l["reach"]),
            absorbed: memory.get(&s(&l["id"])).cloned(),
        })
        // A personal lesson withdrawn to a private collective is a tombstone here: nothing to show.
        .filter(|l| l.reach != "personal")
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct FeedEvent {
    pub ms: f64,
    pub kind: String,
    pub engineer: String,
    pub repo: String,
    pub lessons: Vec<String>,
    pub n: f64,
    pub usd: f64,
    pub dur_ms: f64,
    pub why: String,
    pub text: String,
}

pub fn parse_feed(v: &Value) -> Vec<FeedEvent> {
    v["events"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| FeedEvent {
            ms: f(&e["ms"]),
            kind: s(&e["kind"]),
            engineer: s(&e["engineer"]),
            repo: s(&e["repo"]),
            lessons: strs(&e["lessons"]),
            n: f(&e["n"]),
            usd: f(&e["usd"]),
            dur_ms: f(&e["dur_ms"]),
            why: s(&e["why"]),
            text: s(&e["text"]),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueenEntry {
    pub id: String,
    pub question: String,
    pub options: Vec<String>,
    pub default: String,
    pub answer: String,
    pub answered_by: String,
    pub answered_ms: f64,
    pub ms: f64,
    pub dismissed: bool,
    pub repo: String,
    pub kind: String,
    pub engineer: String,
}

impl QueenEntry {
    pub fn state(&self) -> &'static str {
        if self.dismissed {
            "dismissed"
        } else if !self.answer.is_empty() {
            "answered"
        } else {
            "open"
        }
    }
}

fn option_text(v: &Value) -> String {
    match v {
        Value::String(t) => t.clone(),
        Value::Object(o) => o.get("label").or_else(|| o.get("text")).and_then(Value::as_str).unwrap_or_default().to_string(),
        _ => String::new(),
    }
}

pub fn parse_queen(v: &Value) -> Vec<QueenEntry> {
    v["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|q| QueenEntry {
            id: s(&q["id"]),
            question: s(&q["question"]),
            options: q["options"].as_array().into_iter().flatten().map(option_text).filter(|t| !t.is_empty()).collect(),
            default: match &q["default"] {
                Value::String(t) => t.clone(),
                other => option_text(other),
            },
            answer: s(&q["answer"]),
            answered_by: s(&q["answered_by"]),
            answered_ms: f(&q["answered_ms"]),
            ms: f(&q["ms"]),
            dismissed: q["dismissed"].as_bool().unwrap_or(false) || q["dismissed"].is_string(),
            repo: s(&q["repo"]),
            kind: s(&q["kind"]),
            engineer: s(&q["engineer"]),
        })
        .collect()
}

/// The queen's attention reward (borg escalation.rs), the team's: escalations answered "no" (+1),
/// rubber-stamped (−alpha), and "no"s after merge never escalated (−beta).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attention {
    pub answered: f64,
    pub no: f64,
    pub stamps: f64,
    pub missed: f64,
    /// The share of answered escalations that ended in a "no"; None before any answer.
    pub share: Option<f64>,
    pub reward: f64,
    pub alpha: f64,
    pub beta: f64,
    /// The server sent it (an older one does not).
    pub known: bool,
}

pub fn parse_attention(v: &Value) -> Attention {
    let a = &v["attention"];
    Attention {
        answered: f(&a["answered"]),
        no: f(&a["no"]),
        stamps: f(&a["stamps"]),
        missed: f(&a["missed"]),
        share: a["share"].as_f64(),
        reward: f(&a["reward"]),
        alpha: f(&a["alpha"]),
        beta: f(&a["beta"]),
        known: a.is_object(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Drone {
    pub designation: String,
    pub state: String,
    pub paused: bool,
    pub engineer: String,
    pub repo: String,
    pub prompt: String,
    pub files: Vec<String>,
    pub harness: String,
    pub started: f64,
    pub ms: f64,
    pub remote: bool,
    /// Messages not shown to it yet; `None` for a remote drone (its reads are claimed on its node).
    pub unread: Option<f64>,
}

pub fn parse_drones(v: &Value) -> Vec<Drone> {
    v["drones"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| Drone {
            designation: s(&d["designation"]),
            state: s(&d["state"]),
            paused: d["paused"].as_bool().unwrap_or(false),
            engineer: s(&d["engineer"]),
            repo: s(&d["repo"]),
            prompt: s(&d["prompt"]),
            files: strs(&d["files"]),
            harness: s(&d["harness"]),
            started: f(&d["started"]),
            ms: f(&d["ms"]),
            remote: !s(&d["node"]).is_empty(),
            unread: d["unread"].as_f64(),
        })
        .collect()
}

/// A message between sessions (borg msg.rs), as `/api/messages` lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub id: String,
    /// `tell` or `ask`.
    pub kind: String,
    /// The sender and recipient as `borg top` names them (a designation, `all`, the engineer); a sender
    /// from another node with that node's short key.
    pub from: String,
    pub to: String,
    /// What the recipient is, as borg msg.rs `Address` parses it: `all`, `engineer` or `session`.
    pub to_kind: String,
    pub harness: String,
    pub re: String,
    pub ms: f64,
    pub text: String,
    /// How many sessions on this node it was shown to.
    pub shown_to: f64,
}

pub fn parse_messages(v: &Value) -> Vec<Message> {
    v["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| Message {
            id: s(&m["id"]),
            kind: s(&m["kind"]),
            from: s(&m["from"]),
            to: s(&m["to_name"]),
            to_kind: s(&m["to_kind"]),
            harness: s(&m["harness"]),
            re: s(&m["re"]),
            ms: f(&m["ms"]),
            text: s(&m["text"]),
            shown_to: f(&m["shown_to"]),
        })
        .collect()
}

/// A member of the collective by version, as `/api/update` lists it (borg protocol.rs).
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    /// Its engineer when known, else its short node key.
    pub name: String,
    /// Its borg version; empty when it sent none (older than 0.7).
    pub borg: String,
    /// The protocol its last session agreed on, or the one it speaks when nothing syncs with it.
    pub protocol: f64,
    /// False when nothing syncs with it: it speaks a protocol this node does not.
    pub syncs: bool,
    /// What it misses on an older protocol, or why nothing syncs; empty on this node's footing.
    pub note: String,
}

/// borg itself (`/api/update`): this borg, the last release check and the members by version.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Update {
    pub borg: String,
    /// This node's mesh protocol (`borg/mesh/<n>`).
    pub protocol: f64,
    /// The latest release; empty before the first check.
    pub latest: String,
    pub checked_ms: f64,
    pub error: String,
    /// A newer release exists (and the check is on).
    pub update: bool,
    pub members: Vec<Member>,
}

impl Update {
    /// The members whose protocol differs from this node's: on an older one, or one nothing syncs with.
    pub fn differ(&self) -> Vec<&Member> {
        self.members.iter().filter(|m| !m.syncs || m.protocol != self.protocol).collect()
    }

    /// Whether the page shows its banner: a newer release, or a member on another protocol.
    pub fn shown(&self) -> bool {
        self.update || !self.differ().is_empty()
    }
}

pub fn parse_update(v: &Value) -> Update {
    Update {
        borg: s(&v["borg"]),
        protocol: f(&v["protocol"]),
        latest: s(&v["latest"]),
        checked_ms: f(&v["checked_ms"]),
        error: s(&v["error"]),
        update: v["update"].as_bool().unwrap_or(false),
        members: v["members"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| Member {
                name: s(&m["name"]),
                borg: s(&m["borg"]),
                protocol: f(&m["protocol"]),
                syncs: m["syncs"].as_bool().unwrap_or(true),
                note: s(&m["note"]),
            })
            .collect(),
    }
}

/// A named share, `part` of `whole`: a deviation from the fixed rule, a label rate.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Count {
    pub name: String,
    pub part: f64,
    pub whole: f64,
}

/// One cost source's spend over the sessions folded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spend {
    pub source: String,
    pub calls: f64,
    /// Extra rounds it cost the sessions (a stop block is one).
    pub rounds: f64,
    pub usd: f64,
}

/// An engineer's budget for the collective: pool tokens per session, those read per session, and the
/// price per pool token the policy weighs a larger pool against.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Budget {
    pub tokens: f64,
    pub spent: f64,
    pub price: f64,
}

/// An effect and its interval, as the policy reports them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Estimate {
    pub name: String,
    /// `points` (percentage points of P(changed and avoids)), `coefficient` (a value-model
    /// coefficient, logit scale) or `logit` (a spread).
    pub unit: String,
    pub mean: f64,
    pub lo: f64,
    pub hi: f64,
    pub n: f64,
}

/// The monitor's weighted estimate (the auto-revert's evidence) and how far it can be trusted.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Evaluation {
    /// The learned policy's sessions it read.
    pub sessions: f64,
    /// Per apply, (policy − fixed rule) + α × the fixed rule's value, and its upper bound (None until
    /// enough sessions): below 0 the store reverts to the fixed rule.
    pub mean: f64,
    pub ucb: Option<f64>,
    /// The labelled units weighed, their effective number, the largest weight against the mean, and the
    /// units whose weight was truncated.
    pub units: f64,
    pub ess: f64,
    pub max_weight: f64,
    pub truncated: f64,
}

/// The delivery rates the fit learned: a stop's and a live check's free lesson delivered, a live
/// question delivered (`ask_live`), which weigh a route's value; a lesson a live check left for later
/// delivered later and a live question still waiting at the stop (`later`, `wait`, only shown).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rates {
    pub free_stop: f64,
    pub free_live: f64,
    pub ask_live: f64,
    pub later: f64,
    pub wait: f64,
}

/// What the checks measure for the learned policy at no cost to the fixed rule's quality: the placebos
/// drawn (applies no route delivered, judged blindly) and those judged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Measurement {
    pub placebos: f64,
    pub judged: f64,
    pub tail_shown: f64,
    pub tail_applied: f64,
}

/// The pool's yield at fill positions `from..to` (0 first).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Yield {
    pub from: f64,
    pub to: f64,
    pub shown: f64,
    pub applied: f64,
    pub rate: f64,
}

/// One harness's check latency against its stop budget (None: it names none).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Latency {
    pub harness: String,
    pub checks: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub budget_ms: Option<f64>,
    pub over: f64,
}

/// The learned policy's randomised question-count test (borg top.rs `Test`), all-time as the summary
/// counts it: the risk share in force (`set`: the collective's own, else the default), the checks the
/// test drew a count for (played or refused by the gate), of them the controls (s the fixed rule's own
/// count) and those the gate refused, the sessions with one, and per k the draws of each s.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Test {
    pub risk: f64,
    pub set: bool,
    pub ran: f64,
    pub control: f64,
    pub refused: f64,
    pub sessions: f64,
    /// Of `ran`, the drawn checks the summary still holds, which the readout's estimates read:
    /// compaction drops the oldest checks, never the counts.
    pub kept: f64,
    pub by_k: Vec<Draws>,
    /// The readout: due after `due_checks` checks drawn or `due_sessions` sessions with one, and only then
    /// estimated: per number asked, the share of judged questions that changed the work (`percent
    /// changed`), and a grade 2 asked against handed over (`points changed`).
    pub due: bool,
    pub due_checks: f64,
    pub due_sessions: f64,
    pub dilution: Vec<Estimate>,
    pub ask_free: Option<Estimate>,
}

/// The test's checks at one k: how often each s from 0 to k was drawn, and those refused at that k.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Draws {
    pub k: f64,
    pub draws: Vec<f64>,
    pub refused: f64,
}

/// Lesson selection as borg's monitor reads it from the store's selection summary (`/api/select`, borg
/// top.rs `Selection`). An empty section is not measured yet, never a 0.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    /// The policy in force (`fixed`, `learned`, `cap`, `attention`), the one a hook runs without a switch,
    /// and what decides (the policy, or `reverted`).
    pub policy: String,
    pub default: String,
    pub source: String,
    /// The conservative gate's budget: the quality the learned policy may still risk below the fixed rule.
    pub gate: f64,
    pub fit: String,
    pub checks: f64,
    pub sessions: f64,
    pub deviations: Vec<Count>,
    /// Per reason a learned decision fell back to the fixed rule (`lock_busy`, `lock_failed`,
    /// `unrecorded`), of the graded checks folded.
    pub fallbacks: Vec<Count>,
    pub spend: Vec<Spend>,
    /// Only where an engineer set one.
    pub budget: Option<Budget>,
    pub effects: Vec<Estimate>,
    /// In session ticks; `Some(0)`: no forgetting.
    pub half_life: Option<f64>,
    pub labels: Vec<Count>,
    pub declaration: Option<Estimate>,
    /// None before a fit.
    pub delivery: Option<Rates>,
    pub evaluation: Option<Evaluation>,
    /// None before a graded check is folded.
    pub measurement: Option<Measurement>,
    pub pool: Vec<Yield>,
    pub latency: Vec<Latency>,
    /// None under another policy while the summary counts none of the test's checks.
    pub test: Option<Test>,
}

fn list<T>(v: &Value, one: impl Fn(&Value) -> T) -> Vec<T> {
    v.as_array().into_iter().flatten().map(one).collect()
}

fn count(c: &Value) -> Count {
    Count { name: s(&c["name"]), part: f(&c["part"]), whole: f(&c["whole"]) }
}

fn estimate(e: &Value) -> Estimate {
    Estimate { name: s(&e["name"]), unit: s(&e["unit"]), mean: f(&e["mean"]), lo: f(&e["lo"]), hi: f(&e["hi"]), n: f(&e["n"]) }
}

pub fn parse_select(v: &Value) -> Selection {
    let x = &v["selection"];
    Selection {
        policy: s(&x["policy"]),
        default: s(&x["default"]),
        source: s(&x["source"]),
        gate: f(&x["gate"]),
        fit: s(&x["fit"]),
        checks: f(&x["checks"]),
        sessions: f(&x["sessions"]),
        deviations: list(&x["deviations"], count),
        fallbacks: list(&x["fallbacks"], count),
        spend: list(&x["spend"], |c| Spend { source: s(&c["source"]), calls: f(&c["calls"]), rounds: f(&c["rounds"]), usd: f(&c["usd"]) }),
        budget: x["budget"].as_object().map(|_| {
            let b = &x["budget"];
            Budget { tokens: f(&b["tokens"]), spent: f(&b["spent"]), price: f(&b["price"]) }
        }),
        effects: list(&x["effects"], estimate),
        half_life: x["half_life"].as_f64(),
        labels: list(&x["labels"], count),
        declaration: x["declaration"].as_object().map(|_| estimate(&x["declaration"])),
        delivery: x["delivery"].as_object().map(|_| {
            let r = &x["delivery"];
            Rates { free_stop: f(&r["free_stop"]), free_live: f(&r["free_live"]), ask_live: f(&r["ask_live"]), later: f(&r["later"]), wait: f(&r["wait"]) }
        }),
        evaluation: x["evaluation"].as_object().map(|_| {
            let e = &x["evaluation"];
            Evaluation {
                sessions: f(&e["sessions"]),
                mean: f(&e["mean"]),
                ucb: e["ucb"].as_f64(),
                units: f(&e["units"]),
                ess: f(&e["ess"]),
                max_weight: f(&e["max_weight"]),
                truncated: f(&e["truncated"]),
            }
        }),
        measurement: x["measurement"].as_object().map(|_| {
            let m = &x["measurement"];
            Measurement { placebos: f(&m["placebos"]), judged: f(&m["judged"]), tail_shown: f(&m["tail_shown"]), tail_applied: f(&m["tail_applied"]) }
        }),
        pool: list(&x["pool"], |y| Yield { from: f(&y["from"]), to: f(&y["to"]), shown: f(&y["shown"]), applied: f(&y["applied"]), rate: f(&y["rate"]) }),
        latency: list(&x["latency"], |l| Latency {
            harness: s(&l["harness"]),
            checks: f(&l["checks"]),
            p50_ms: f(&l["p50_ms"]),
            p95_ms: f(&l["p95_ms"]),
            budget_ms: l["budget_ms"].as_f64(),
            over: f(&l["over"]),
        }),
        test: x["test"].as_object().map(|_| {
            let t = &x["test"];
            Test {
                risk: f(&t["risk"]),
                set: t["set"].as_bool().unwrap_or(false),
                ran: f(&t["ran"]),
                control: f(&t["control"]),
                refused: f(&t["refused"]),
                sessions: f(&t["sessions"]),
                kept: f(&t["kept"]),
                by_k: list(&t["by_k"], |d| Draws { k: f(&d["k"]), draws: list(&d["draws"], f), refused: f(&d["refused"]) }),
                due: t["due"].as_bool().unwrap_or(false),
                due_checks: f(&t["due_checks"]),
                due_sessions: f(&t["due_sessions"]),
                dilution: list(&t["dilution"], estimate),
                ask_free: t["ask_free"].as_object().map(|_| estimate(&t["ask_free"])),
            }
        }),
    }
}

/// One view's raw JSON, whether the server has it (old servers lack feed and queen), and how to load it.
#[derive(Clone, Copy)]
pub struct Source {
    pub raw: RwSignal<Value>,
    pub missing: RwSignal<bool>,
    pub path: &'static str,
}

impl Source {
    fn new(path: &'static str) -> Self {
        Source { raw: RwSignal::new(Value::Null), missing: RwSignal::new(false), path }
    }

    fn load(self, collective: String, newest: RwSignal<f64>) {
        spawn_local(async move {
            // The Knowledge view marks only the absorbed lessons: their evidence records are all it reads.
            let extra = match self.path {
                "feed" => "&limit=300",
                "memory" => "&absorbed=true",
                _ => "",
            };
            match get(format!("/api/{}?c={collective}{extra}", self.path)).await {
                Some(v) => {
                    if self.path == "feed" {
                        // Items newer than what was shown before glow briefly.
                        let top = self.raw.get_untracked()["events"][0]["ms"].as_f64().unwrap_or(0.0);
                        newest.set(top);
                    }
                    self.missing.set(false);
                    self.raw.set(v);
                }
                None => self.missing.set(true),
            }
        });
    }
}

/// Everything the views read, shared through context.
#[derive(Clone, Copy)]
pub struct Data {
    pub collective: RwSignal<String>,
    /// Name, share and delivery mode (`hook` or `live`) of each collective.
    pub collectives: RwSignal<Vec<(String, String, String)>>,
    pub live: RwSignal<bool>,
    /// The raw JSON of every view; `learning` (learning health, views/learning.rs) is read from its source,
    /// `memory` (the absorbed lessons' evidence) is joined into `lessons`.
    pub sources: [Source; 10],
    pub stats: Memo<Stats>,
    pub lessons: Memo<Vec<Lesson>>,
    pub feed: Memo<Vec<FeedEvent>>,
    pub queen: Memo<Vec<QueenEntry>>,
    /// The queen's attention reward (served with the queue).
    pub attention: Memo<Attention>,
    pub drones: Memo<Vec<Drone>>,
    /// Messages between sessions, newest first, and how many wait unread on this node.
    pub messages: Memo<Vec<Message>>,
    pub unread: Memo<f64>,
    /// Lesson selection (the Conscience tab).
    pub select: Memo<Selection>,
    /// borg itself: the release check and the members by version (the banner).
    pub update: Memo<Update>,
    /// The newest feed time before the last refresh: newer items are new.
    pub seen: RwSignal<f64>,
    /// The lesson open in the drawer.
    pub open: RwSignal<Option<String>>,
}

impl Data {
    pub fn source(&self, path: &str) -> Source {
        *self.sources.iter().find(|s| s.path == path).expect("known view")
    }

    /// The delivery mode of the shown collective (`hook` when the server does not say).
    pub fn mode(&self) -> String {
        let c = self.collective.get();
        self.collectives.with(|cs| cs.iter().find(|x| x.0 == c).map(|x| x.2.clone())).filter(|m| !m.is_empty()).unwrap_or_else(|| "hook".into())
    }

    pub fn lesson(&self, id: &str) -> Option<Lesson> {
        self.lessons.with(|ls| ls.iter().find(|l| l.id == id).cloned())
    }
}

pub fn data() -> Data {
    expect_context::<Data>()
}

/// Creates the shared data, loads it for the shown collective and follows the change stream.
pub fn provide() -> Data {
    let sources = [
        Source::new("stats"),
        Source::new("lessons"),
        Source::new("feed"),
        Source::new("queen"),
        Source::new("drones"),
        Source::new("learning"),
        Source::new("messages"),
        Source::new("select"),
        Source::new("memory"),
        Source::new("update"),
    ];
    let [st, le, fe, qu, dr, _, me, se, mo, up] = sources;
    let d = Data {
        collective: RwSignal::new(String::new()),
        collectives: RwSignal::new(Vec::new()),
        live: RwSignal::new(false),
        sources,
        stats: Memo::new(move |_| parse_stats(&st.raw.get())),
        lessons: Memo::new(move |_| parse_lessons(&le.raw.get(), &parse_absorbed(&mo.raw.get()))),
        feed: Memo::new(move |_| parse_feed(&fe.raw.get())),
        queen: Memo::new(move |_| parse_queen(&qu.raw.get())),
        attention: Memo::new(move |_| parse_attention(&qu.raw.get())),
        drones: Memo::new(move |_| parse_drones(&dr.raw.get())),
        messages: Memo::new(move |_| parse_messages(&me.raw.get())),
        unread: Memo::new(move |_| f(&me.raw.get()["unread"])),
        select: Memo::new(move |_| parse_select(&se.raw.get())),
        update: Memo::new(move |_| parse_update(&up.raw.get())),
        seen: RwSignal::new(0.0),
        open: RwSignal::new(None),
    };
    provide_context(d);

    spawn_local(async move {
        if let Some(v) = get("/api/collectives".into()).await {
            d.collectives.set(
                v["collectives"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|c| (s(&c["name"]), s(&c["share"]), s(&c["mode"])))
                    .filter(|(n, ..)| !n.is_empty())
                    .collect(),
            );
            d.collective.set(s(&v["first"]));
        }
    });

    let stream = StoredValue::new_local(None::<web_sys::EventSource>);
    Effect::new(move |_| {
        let c = d.collective.get();
        if c.is_empty() {
            return;
        }
        d.seen.set(0.0);
        for src in d.sources {
            src.load(c.clone(), d.seen);
        }
        stream.update_value(|slot| {
            if let Some(old) = slot.take() {
                old.close();
            }
            let Ok(es) = web_sys::EventSource::new(&format!("/api/events?c={c}")) else { return };
            let changed = Closure::<dyn Fn(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
                let which: Value = serde_json::from_str(&e.data().as_string().unwrap_or_default()).unwrap_or(Value::Null);
                let names: Vec<String> = which["changed"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect();
                let c = d.collective.get_untracked();
                for src in d.sources {
                    // An old server names no feed or queen view: refresh them with the lessons and stats.
                    let hit =
                        names.iter().any(|n| n == src.path) || (matches!(src.path, "feed" | "queen") && names.iter().any(|n| n == "stats" || n == "lessons"));
                    if hit {
                        src.load(c.clone(), d.seen);
                    }
                }
            });
            let _ = es.add_event_listener_with_callback("change", changed.as_ref().unchecked_ref());
            changed.forget();
            let open = Closure::<dyn Fn()>::new(move || d.live.set(true));
            es.set_onopen(Some(open.as_ref().unchecked_ref()));
            open.forget();
            let lost = Closure::<dyn Fn()>::new(move || d.live.set(false));
            es.set_onerror(Some(lost.as_ref().unchecked_ref()));
            lost.forget();
            *slot = Some(es);
        });
    });
    d
}
