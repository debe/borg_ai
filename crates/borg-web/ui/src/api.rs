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
    pub dur_ms: f64,
}

impl Totals {
    fn parse(v: &Value) -> Self {
        Totals { counts: map(&v["counts"]), usd: f(&v["usd"]), usd_by: map(&v["usd_by"]), dur_ms: f(&v["dur_ms"]) }
    }

    pub fn n(&self, kind: &str) -> f64 {
        self.counts.get(kind).copied().unwrap_or(0.0)
    }

    pub fn add(&mut self, o: &Totals) {
        for (k, v) in &o.counts {
            *self.counts.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &o.usd_by {
            *self.usd_by.entry(k.clone()).or_default() += v;
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
    Stats { totals: Totals::parse(&v["totals"]), days: days(&v["days"]), engineers, nodes: v["nodes"].as_u64().unwrap_or(0) }
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
    pub dormant: bool,
    pub consolidated: bool,
    pub usage: BTreeMap<String, f64>,
    /// Who hears it: empty for the team, `repo` for its repo only (reach.rs in borg).
    pub reach: String,
}

impl Lesson {
    /// The system it belongs to: its first scope, or "general".
    pub fn system(&self) -> String {
        self.scope.first().cloned().unwrap_or_else(|| "general".into())
    }

    pub fn area(&self) -> String {
        if self.area.is_empty() { "other".into() } else { self.area.clone() }
    }

    /// Mean of the usefulness belief (memory.rs: Beta(1 + changed, 1 + ignored + 2 corrected)).
    pub fn usefulness(&self) -> f64 {
        let a = 1.0 + self.changed;
        let b = 1.0 + self.ignored + 2.0 * self.corrected;
        a / (a + b)
    }

    pub fn asked(&self) -> f64 {
        self.usage.get("block").copied().unwrap_or(0.0)
    }

    pub fn source_label(&self) -> &'static str {
        match self.source.as_str() {
            "import" => "imported",
            "queen" => "queen answer",
            _ => "learned in session",
        }
    }

    /// Words the search box matches against.
    pub fn haystack(&self) -> String {
        format!("{} {} {} {} {} {}", self.question, self.consequence, self.situations.join(" "), self.scope.join(" "), self.area, self.engineers.join(" "))
            .to_lowercase()
    }
}

pub fn parse_lessons(v: &Value) -> Vec<Lesson> {
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
            dormant: l["dormant"].as_bool().unwrap_or(false),
            consolidated: l["consolidated"].as_bool().unwrap_or(false),
            usage: map(&l["usage"]),
            reach: s(&l["reach"]),
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
        })
        .collect()
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
            let extra = if self.path == "feed" { "&limit=300" } else { "" };
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
    pub collectives: RwSignal<Vec<(String, String)>>,
    pub live: RwSignal<bool>,
    pub sources: [Source; 5],
    pub stats: Memo<Stats>,
    pub lessons: Memo<Vec<Lesson>>,
    pub feed: Memo<Vec<FeedEvent>>,
    pub queen: Memo<Vec<QueenEntry>>,
    pub drones: Memo<Vec<Drone>>,
    /// The newest feed time before the last refresh: newer items are new.
    pub seen: RwSignal<f64>,
    /// The lesson open in the drawer.
    pub open: RwSignal<Option<String>>,
}

impl Data {
    pub fn source(&self, path: &str) -> Source {
        *self.sources.iter().find(|s| s.path == path).expect("known view")
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
    let sources = [Source::new("stats"), Source::new("lessons"), Source::new("feed"), Source::new("queen"), Source::new("drones")];
    let [st, le, fe, qu, dr] = sources;
    let d = Data {
        collective: RwSignal::new(String::new()),
        collectives: RwSignal::new(Vec::new()),
        live: RwSignal::new(false),
        sources,
        stats: Memo::new(move |_| parse_stats(&st.raw.get())),
        lessons: Memo::new(move |_| parse_lessons(&le.raw.get())),
        feed: Memo::new(move |_| parse_feed(&fe.raw.get())),
        queen: Memo::new(move |_| parse_queen(&qu.raw.get())),
        drones: Memo::new(move |_| parse_drones(&dr.raw.get())),
        seen: RwSignal::new(0.0),
        open: RwSignal::new(None),
    };
    provide_context(d);

    spawn_local(async move {
        if let Some(v) = get("/api/collectives".into()).await {
            d.collectives
                .set(v["collectives"].as_array().into_iter().flatten().map(|c| (s(&c["name"]), s(&c["share"]))).filter(|(n, _)| !n.is_empty()).collect());
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
