//! The Cube: the collective live on a canvas. The core cube is the collective, the machines float
//! around it, the drones orbit their machine, and what moves between them plays as it happens: a
//! packet for every message, a flash where one was shown, a lesson, a sync, a hand-off. Opening the
//! tab replays the last day fast, so the view starts with what the collective did (not under reduced
//! motion: then it shows the state and lists the newest events).
//!
//! The world and its effects are scene.rs, the drawing paint.rs through canvas.rs; this is the glue:
//! the fetch (`/api/mesh`, from `since` on each change the stream names), the playback clock and the
//! frame loop, the input, and the text around the canvas in the DOM: the counters, the ticker (the
//! live region), the toasts, the legend chips that filter, the timeline that scrubs, the hover and
//! the selected machine or drone. What it names and counts is kinds.rs and tally.rs, pure.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::suboptimal_flops,
    clippy::while_float,
    reason = "f32 graphics and f64 times in ms; f32::mul_add is a libm call on wasm32 without FMA"
)]

mod kinds;
mod tally;

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::api::{self, Data, data};
use crate::canvas::Canvas;
use crate::model::{Ev, EvKind, Model, Play};
use crate::paint::paint;
use crate::scene::{Anchor, Camera, Handed, Scene, Shown};
use crate::util::{DAY_MS, ago, clock, day_label, keep, kept, local_midnight, now_ms, weekday};

use kinds::{Cat, Filter};
use tally::{HOUR_MS, HOURS, Hud, TL_H, WEEK_MS};

/// Display: the replay rates the timeline offers, fastest first: how many times the real clock (600×: ten
/// minutes of the store's time pass in each real second).
const RATES: [f64; 4] = [3600.0, 600.0, 60.0, 1.0];
/// Display: when the tab opens it replays this many real seconds at the fastest rate, then plays live.
const CATCHUP_S: f64 = 8.0;
/// Derived: the history that opening replay shows, `CATCHUP_S` × `RATES[0]` (8 hours).
const CATCHUP_MS: f64 = CATCHUP_S * 1000.0 * RATES[0];
/// Guard: a frame this long after the last (seconds: the tab was hidden) moves the clock all the same, but
/// plays only the newest events it passed, as the ticker lists them.
const GAP_S: f64 = 1.0;
/// Display: live events that arrive together play this far apart, seconds; closer when more wait than
/// `LAG_S` holds, so live plays at most this far behind what the ticker lists.
const STAGGER: f64 = 0.15;
const LAG_S: f64 = 1.0;
/// Display: ticker lines shown, and toasts on screen at once and how long each stays.
const TICKER: usize = 7;
const TOASTS: usize = 3;
const TOAST_MS: u64 = 4000;
/// Display: during a replay only lessons toast, at most this often (seconds of the page's clock).
const TOAST_GAP: f64 = 1.5;
/// Derived: the playhead moves in steps of this much store time: a fraction of a pixel of the timeline.
const HEAD_STEP: f64 = WEEK_MS / 2000.0;
const FILTER_KEY: &str = "borg.cube.filter";
/// The height the bottom edge was dragged to, kept for the next visit.
const HEIGHT_KEY: &str = "borg.cube.height";
/// Display: the view is no lower than this (px), where the counters, ticker and timeline still fit,
/// and a key press on its bottom edge moves it `STEP_H`.
const MIN_H: f64 = 360.0;
const STEP_H: f64 = 40.0;

/// A number in words up to 99 ("Twenty-Five"), in digits from 100 on.
fn word(k: usize) -> String {
    const ONES: [&str; 20] = [
        "Zero",
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
    ];
    const TENS: [&str; 8] = ["Twenty", "Thirty", "Forty", "Fifty", "Sixty", "Seventy", "Eighty", "Ninety"];
    match (ONES.get(k), TENS.get((k / 10).wrapping_sub(2))) {
        (Some(w), _) => (*w).to_string(),
        (None, Some(t)) if k.is_multiple_of(10) => (*t).to_string(),
        (None, Some(t)) => format!("{t}-{}", ONES[k % 10]),
        (None, None) => k.to_string(),
    }
}

/// "Three of Nine": a drone's number among the collective's, in words.
pub fn of(n: u32, total: usize) -> String {
    format!("{} of {}", word(n as usize), word(total))
}

#[derive(Clone, Debug, PartialEq)]
struct Tick {
    id: u64,
    ms: f64,
    kind: &'static str,
    tone: &'static str,
    line: String,
    /// The lesson a lesson's line opens in the drawer.
    lesson: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct Toast {
    id: u64,
    tone: &'static str,
    text: String,
}

/// What a click selected, by the model's ids: the panel reads it anew from every response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sel {
    Core,
    Node(u32),
    Drone(u32),
}

/// The hover's tip: what it names. Where it is (`Ui::tip_at`) changes on every move, this only
/// with what is under the pointer.
#[derive(Clone, Debug, PartialEq)]
struct Pick {
    title: String,
    lines: Vec<String>,
}

/// What the panel shows of a selection.
#[derive(Clone, Debug, PartialEq)]
struct Detail {
    title: String,
    facts: Vec<(&'static str, String)>,
    /// A drone's machine, to select it.
    machine: Option<(u32, String)>,
    /// A machine's drones, to select one.
    drones: Vec<(u32, String)>,
    messages: bool,
}

/// The playback clock: the store's time the view shows and how fast it runs; speed 0 is live, where
/// it follows the store's present and new events play as they arrive. A replay runs on the real
/// clock: its time is where it started plus the real time since, times its speed, so slow or dropped
/// frames never slow it down.
#[derive(Default)]
struct Clock {
    sim: f64,
    speed: f64,
    /// The frame time (seconds) and store time it runs from; `None`: from the next frame on.
    from: Option<(f64, f64)>,
}

impl Clock {
    const fn at(sim: f64, speed: f64) -> Self {
        Self { sim, speed, from: None }
    }

    fn live(&self) -> bool {
        self.speed <= 0.0
    }

    /// Moves a replay to frame time `t` (seconds), no later than `edge`.
    fn run(&mut self, t: f64, edge: f64) {
        let (t0, sim0) = *self.from.get_or_insert((t, self.sim));
        self.sim = (sim0 + (t - t0) * 1000.0 * self.speed).min(edge);
    }
}

/// What a frame or a response hands the DOM.
#[derive(Default)]
struct Out {
    /// Oldest first.
    ticks: Vec<Tick>,
    toasts: Vec<(&'static str, String)>,
    hud: Option<Hud>,
}

/// How far the full fetch got: until a full response is taken, the events from a `since` on would
/// all be new.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Got {
    #[default]
    Nothing,
    Asked,
    Held,
}

#[derive(Default)]
struct Live {
    model: Model,
    scene: Scene,
    canvas: Option<Canvas>,
    clock: Clock,
    /// Live events waiting their turn, with how a hand-off went, and how far apart they play.
    pending: VecDeque<(Play, Handed)>,
    next_pop: f64,
    gap: f64,
    got: Got,
    /// The server's clock minus the browser's at the last response: the live edge is `now_ms() + skew`.
    skew: f64,
    /// Peers seen syncing in this view: a sync with another says a link was established.
    synced: HashSet<u32>,
    /// During a replay, the next lesson toasts no sooner than this (seconds of the frame clock).
    next_toast: f64,
    /// The local midnight the counters count from, and the next one, when they count anew.
    midnight: f64,
    day_end: f64,
    /// The timeline is being dragged: the clock holds still.
    scrubbing: bool,
    ids: u64,
    /// The last frame's time (seconds), and what the last painted frame showed.
    last_t: f64,
    /// The wall clock's minute at the last frame: the ticker's "ago" reads anew on each.
    minute: f64,
    shown: Shown,
    dirty: bool,
    drag: Option<(f32, f32, f32)>,
    hover: Option<Anchor>,
}

fn tick(m: &Model, total: usize, e: &Ev, id: u64) -> Tick {
    let k = e.play.kind;
    let lesson = if Cat::of(k) == Cat::Lessons { e.lesson.map(|l| m.str(l).to_owned()) } else { None };
    Tick { id, ms: e.ms, kind: k.label(), tone: kinds::tone(k), line: kinds::line(m, total, e), lesson }
}

/// How an event plays: a hand-off by how it went.
fn handed(e: &Ev) -> Handed {
    e.why.as_deref().map_or(Handed::Moves, Handed::parse)
}

/// Whether this frame paints: when the scene would look different from the last painted frame
/// (scene.rs weighs motion by the pixels it moves), or the view changed what it knows of itself
/// (new data, a resize, the camera by a key or the wheel).
fn needs_draw(l: &Live) -> bool {
    l.dirty || l.scene.needs_draw(&l.shown)
}

impl Live {
    fn edge(&self) -> f64 {
        now_ms() + self.skew
    }

    /// Where the timeline starts: the server's window back from its last response.
    fn start(&self) -> f64 {
        self.model.now - WEEK_MS
    }

    /// Counts from the viewer's local midnight anew.
    fn new_day(&mut self) -> Hud {
        self.midnight = local_midnight(now_ms());
        self.day_end = local_midnight(self.midnight + 1.5 * DAY_MS);
        tally::hud(&self.model, self.midnight)
    }

    /// The ticker as of the clock: the newest events shown, newest first.
    fn recent(&mut self, f: Filter) -> Vec<Tick> {
        let ev = &self.model.events;
        let b = ev.partition_point(|e| e.ms <= self.clock.sim);
        let total = kinds::total(&self.model);
        let mut out = Vec::with_capacity(TICKER);
        for e in ev[..b].iter().rev().filter(|e| f.shows(e.play.kind)).take(TICKER) {
            self.ids += 1;
            out.push(tick(&self.model, total, e, self.ids));
        }
        out
    }

    /// Plays the events the clock passed since `from`; lists the newest; toasts lessons, spaced.
    fn replay(&mut self, from: f64, f: Filter, t: f64, burst: bool, out: &mut Out) {
        let Self { model, scene, clock, ids, next_toast, .. } = self;
        let ev = &model.events;
        let range = &ev[ev.partition_point(|e| e.ms <= from)..ev.partition_point(|e| e.ms <= clock.sim)];
        if range.is_empty() {
            return;
        }
        // After a hidden tab only the newest play: what the ticker lists.
        let skip = if burst { range.iter().filter(|e| f.shows(e.play.kind)).count().saturating_sub(TICKER) } else { 0 };
        for e in range.iter().filter(|e| f.shows(e.play.kind)).skip(skip) {
            scene.play_handed(e.play, handed(e));
            if e.play.kind == EvKind::Lesson
                && t >= *next_toast
                && let Some(s) = kinds::toast(e)
            {
                out.toasts.push(("lesson", s));
                *next_toast = t + TOAST_GAP;
            }
        }
        let total = kinds::total(model);
        let first = out.ticks.len();
        for e in range.iter().rev().filter(|e| f.shows(e.play.kind)).take(TICKER) {
            *ids += 1;
            out.ticks.push(tick(model, total, e, *ids));
        }
        out.ticks[first..].reverse();
    }

    /// A response: machines and drones anew, new events joined. On the first, the catch-up starts; on
    /// a later one, live, what is new plays, lists and toasts (a replay reaches it by the clock).
    fn take(&mut self, text: &str, full: bool, f: Filter, still: bool) -> Option<Out> {
        let before: HashSet<u32> = if full { HashSet::new() } else { self.model.drones.iter().map(|d| d.session).collect() };
        self.model.merge(text)?;
        if full {
            self.got = Got::Held;
        }
        self.skew = self.model.now - now_ms();
        self.scene.sync(&self.model);
        self.dirty = true;
        let mut out = Out { hud: Some(self.new_day()), ..Out::default() };
        if full {
            self.synced = self.model.events.iter().filter(|e| e.play.kind == EvKind::Sync).filter_map(|e| e.play.to_n).collect();
            self.pending.clear();
            let edge = self.edge();
            self.clock = if still { Clock::at(edge, 0.0) } else { Clock::at(edge - CATCHUP_MS, RATES[0]) };
            out.ticks = self.recent(f);
            out.ticks.reverse();
            return Some(out);
        }
        let live = self.clock.live();
        let Self { model, synced, pending, ids, gap, .. } = self;
        let total = kinds::total(model);
        for e in model.added() {
            let new_link = e.play.kind == EvKind::Sync && e.play.to_n.is_some_and(|p| synced.insert(p));
            if !live || !f.shows(e.play.kind) {
                continue;
            }
            if new_link && let Some(p) = e.play.to_n {
                out.toasts.push(("sync", format!("Link established with {}", kinds::host(model, p))));
            }
            pending.push_back((e.play, handed(e)));
            *ids += 1;
            out.ticks.push(tick(model, total, e, *ids));
            if let Some(s) = kinds::toast(e) {
                out.toasts.push((kinds::tone(e.play.kind), s));
            }
        }
        // What waits plays within `LAG_S`, however much came at once (a sync's batch, a hidden tab).
        *gap = STAGGER.min(LAG_S / pending.len().max(1) as f64);
        if live {
            for d in model.drones.iter().filter(|d| d.state != "ended" && !before.contains(&d.session)) {
                out.toasts.push(("join", format!("{} joined the collective", kinds::drone_name(d, total))));
            }
        }
        Some(out)
    }

    /// Plays from `ms` at `rate`, or live from the store's present on; lists what was shown by then.
    fn seek(&mut self, ms: f64, rate: f64, f: Filter) -> Vec<Tick> {
        let edge = self.edge();
        let ms = ms.clamp(self.start(), edge);
        self.pending.clear();
        self.clock = Clock::at(ms, if ms >= edge { 0.0 } else { rate });
        self.recent(f)
    }

    fn sel_of(&self, a: Anchor) -> Option<Sel> {
        match a {
            Anchor::Core => Some(Sel::Core),
            Anchor::Node(i) => self.scene.node_key.get(i as usize).map(|k| Sel::Node(*k)),
            Anchor::Drone(i) => self.scene.drone_session.get(i as usize).map(|s| Sel::Drone(*s)),
        }
    }
}

/// "3m ago", or a dash for a time the server does not know (0).
fn when(ms: f64) -> String {
    if ms > 0.0 { ago(ms) } else { "—".into() }
}

/// The panel's facts of a selection, from the model as it is now.
fn detail(m: &Model, sel: Sel) -> Option<Detail> {
    let total = kinds::total(m);
    let active = |on: Option<u32>| m.drones.iter().filter(|d| d.state != "ended" && on.is_none_or(|n| d.node == n)).count();
    Some(match sel {
        Sel::Core => Detail {
            title: "The collective".into(),
            facts: vec![("Machines", m.nodes.len().to_string()), ("Drones", format!("{} active of {}", active(None), m.drones.len()))],
            machine: None,
            drones: Vec::new(),
            messages: false,
        },
        Sel::Node(key) => {
            let node = m.node(key)?;
            let mut facts = vec![("Role", if node.me { "this machine" } else { "a member" }.to_string())];
            if let Some(e) = &node.engineer {
                facts.push(("Engineer", e.clone()));
            }
            facts.push((
                "Version",
                match (&node.borg, node.protocol) {
                    (Some(b), Some(p)) => format!("borg {b} · protocol {p}"),
                    (None, Some(p)) => format!("borg before 0.7 · protocol {p}"),
                    _ => "unknown".into(),
                },
            ));
            if !node.syncs {
                facts.push(("Protocol", "another one: nothing syncs".into()));
            }
            let links: Vec<_> = m.links.iter().filter(|l| l.a == key || l.b == key).collect();
            facts.push((
                "Last sync",
                match links.iter().filter_map(|l| l.ms).reduce(f64::max) {
                    Some(ms) => ago(ms),
                    None if links.iter().any(|l| l.dial) => "dialled, never synced".into(),
                    None if links.is_empty() && !node.me => "reached through the collective".into(),
                    None => "never".into(),
                },
            ));
            let mut ds: Vec<_> = m.drones.iter().filter(|d| d.node == key).collect();
            ds.sort_by_key(|d| (d.state == "ended", d.number));
            facts.push(("Drones", format!("{} active, {} ended", active(Some(key)), ds.len() - active(Some(key)))));
            Detail {
                title: node.host.clone(),
                facts,
                machine: None,
                drones: ds.into_iter().map(|d| (d.session, format!("{} · {}", kinds::drone_name(d, total), d.state))).collect(),
                messages: false,
            }
        }
        Sel::Drone(session) => {
            let d = m.drone(session)?;
            let or = |v: &str| if v.is_empty() { "—".to_string() } else { v.to_string() };
            Detail {
                title: of(d.number, total),
                facts: vec![
                    ("Repo", or(&d.repo)),
                    ("State", format!("{}{}", d.state, if d.paused { " · paused" } else { "" })),
                    ("Harness", or(&d.harness)),
                    ("Engineer", or(&d.engineer)),
                    ("Started", when(d.started)),
                    ("Last heard", when(d.ms)),
                ],
                machine: m.node(d.node).map(|n| (n.key, n.host.clone())),
                drones: Vec::new(),
                messages: true,
            }
        }
    })
}

/// A listener on the canvas: the element and its callback, to remove it again.
type Wheel = (web_sys::HtmlCanvasElement, Closure<dyn FnMut(web_sys::WheelEvent)>);

/// The frame loop's callback, its pending frame, the resize observer and the wheel listener,
/// dropped with the view.
#[derive(Default)]
struct Looper {
    frame: Option<Closure<dyn FnMut(f64)>>,
    raf: Option<i32>,
    ro: Option<web_sys::ResizeObserver>,
    on_resize: Option<Closure<dyn FnMut()>>,
    wheel: Option<Wheel>,
}

fn reduced_motion() -> bool {
    web_sys::window().and_then(|w| w.match_media("(prefers-reduced-motion: reduce)").ok().flatten()).is_some_and(|q| q.matches())
}

/// The rate a replay runs at, as the timeline's buttons say it.
fn rate_label(r: f64) -> String {
    format!("{r:.0}×")
}

/// The view's signals and state, all `Copy`: the canvas, the text around it and the frame loop
/// share them.
#[derive(Clone, Copy)]
struct Ui {
    d: Data,
    /// Motion is reduced: no catch-up, no count-up, toasts without animation.
    still: bool,
    st: StoredValue<Live, LocalStorage>,
    looper: StoredValue<Looper, LocalStorage>,
    hud: RwSignal<Hud>,
    /// The pulse now and the week's busiest minute.
    pulse: RwSignal<(usize, usize)>,
    ticker: RwSignal<Vec<Tick>>,
    toasts: RwSignal<Vec<Toast>>,
    toast_ids: StoredValue<u64>,
    hover: RwSignal<Option<Pick>>,
    /// Where the tip is, and whether it opens to the left.
    tip_at: RwSignal<(f32, f32, bool)>,
    /// The wall clock's minute, for the ticker's "ago".
    minute: RwSignal<f64>,
    /// A panel button changed the selection: the new panel takes the focus.
    refocus: StoredValue<bool>,
    picked: RwSignal<Option<Sel>>,
    /// Bumped on every response: the panel reads the model anew.
    rev: RwSignal<u32>,
    filter: RwSignal<Filter>,
    /// The rate a replay plays at; the clock's speed (None: live) and its playhead.
    rate: RwSignal<f64>,
    speed: RwSignal<Option<f64>>,
    head: RwSignal<f64>,
    /// The timeline's start and its hours by chip.
    hist: RwSignal<(f64, Vec<[u32; 6]>)>,
    canvas: NodeRef<leptos::html::Canvas>,
    strip: NodeRef<leptos::html::Div>,
    /// The view itself (resized and made full screen), the height its bottom edge was dragged to
    /// (`None`: the page's own), and where a drag of the edge started: pointer y and height.
    root: NodeRef<leptos::html::Section>,
    height: RwSignal<Option<f64>>,
    grip: StoredValue<Option<(f64, f64)>>,
}

impl Ui {
    fn new() -> Self {
        Self {
            d: data(),
            still: reduced_motion(),
            st: StoredValue::new_local(Live::default()),
            looper: StoredValue::new_local(Looper::default()),
            hud: RwSignal::new(Hud::default()),
            pulse: RwSignal::new((0, 0)),
            ticker: RwSignal::new(Vec::new()),
            toasts: RwSignal::new(Vec::new()),
            toast_ids: StoredValue::new(0),
            hover: RwSignal::new(None),
            tip_at: RwSignal::new((0.0, 0.0, false)),
            minute: RwSignal::new(0.0),
            refocus: StoredValue::new(false),
            picked: RwSignal::new(None),
            rev: RwSignal::new(0),
            filter: RwSignal::new(Filter::from_stored(&kept(FILTER_KEY).unwrap_or_default())),
            rate: RwSignal::new(RATES[0]),
            speed: RwSignal::new(None),
            head: RwSignal::new(0.0),
            hist: RwSignal::new((0.0, Vec::new())),
            canvas: NodeRef::new(),
            strip: NodeRef::new(),
            root: NodeRef::new(),
            height: RwSignal::new(kept(HEIGHT_KEY).and_then(|h| h.parse::<f64>().ok()).filter(|h| *h >= MIN_H)),
            grip: StoredValue::new(None),
        }
    }

    /// Hands what a frame or a response made to the DOM: ticker lines, toasts, the counters.
    fn show(self, out: Out) {
        if !out.ticks.is_empty() {
            self.ticker.update(|v| {
                for t in out.ticks {
                    v.insert(0, t);
                }
                v.truncate(TICKER);
            });
        }
        for (tone, text) in out.toasts {
            let Some(id) = self.toast_ids.try_update_value(|n| {
                *n += 1;
                *n
            }) else {
                return;
            };
            self.toasts.update(|v| {
                v.push(Toast { id, tone, text });
                if v.len() > TOASTS {
                    v.remove(0);
                }
            });
            let toasts = self.toasts;
            set_timeout(
                move || {
                    let _ = toasts.try_update(|v| v.retain(|t| t.id != id));
                },
                Duration::from_millis(TOAST_MS),
            );
        }
        if let Some(h) = out.hud
            && self.hud.try_with_untracked(|v| *v != h).unwrap_or(false)
        {
            self.hud.set(h);
        }
    }

    /// A response. The view may be gone by when it came: its signals are disposed then, and the
    /// `try_` reads say so instead of panicking.
    fn take(self, text: &str, full: bool) {
        let Some(f) = self.filter.try_get_untracked() else { return };
        let still = self.still;
        let Some(Some((out, peak, hist))) = self.st.try_update_value(|l| {
            let out = l.take(text, full, f, still)?;
            let start = l.start();
            Some((out, tally::peak(&l.model.events), (start, tally::hours(&l.model.events, start))))
        }) else {
            return;
        };
        if full {
            self.ticker.set(Vec::new());
        }
        self.show(out);
        self.pulse.update(|p| p.1 = peak);
        self.hist.set(hist);
        self.rev.update(|r| *r = r.wrapping_add(1));
    }

    /// Fetches the Cube's data: all of it, or what reached the store since the last response (`since`
    /// is that response's `now`; the server filters by arrival, so an event synced late or stamped by a
    /// clock that runs ahead still comes, and a resent one is dropped by id in `merge`). Until a
    /// full response is taken a refetch is a full one, or none while one is out: a week of events
    /// would otherwise all play as new.
    fn fetch(self, full: bool) {
        let c = self.d.collective.get_untracked();
        if c.is_empty() {
            return;
        }
        let Some(Some(since)) = self.st.try_update_value(|l| match (full, l.got) {
            (false, Got::Held) => Some(Some(l.model.now)),
            (false, Got::Asked) => None,
            _ => {
                l.got = Got::Asked;
                Some(None)
            }
        }) else {
            return;
        };
        let full = since.is_none();
        spawn_local(async move {
            let url = since.map_or_else(|| format!("/api/mesh?c={c}"), |s| format!("/api/mesh?c={c}&since={s:.0}"));
            let text = api::text(url).await;
            if self.d.collective.get_untracked() != c {
                return;
            }
            if full {
                let _ = self.st.try_update_value(|l| {
                    if l.got == Got::Asked {
                        l.got = Got::Nothing;
                    }
                });
            }
            if let Some(text) = text {
                self.take(&text, full);
            }
        });
    }

    fn schedule(self) {
        let id = self
            .looper
            .try_with_value(|lp| lp.frame.as_ref().and_then(|f| web_sys::window()?.request_animation_frame(f.as_ref().unchecked_ref()).ok()))
            .flatten();
        if let Some(id) = id {
            self.looper.update_value(|lp| lp.raf = Some(id));
        }
    }

    /// The canvas is in the page: size it, listen to it, start the frame loop.
    fn start(self, el: web_sys::HtmlCanvasElement) {
        let st = self.st;
        let dpr = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());
        st.update_value(|l| {
            l.canvas = Canvas::new(&el, dpr);
            l.scene.still = self.still;
        });
        // Size the backing store to the element, in device pixels.
        let size = {
            let el = el.clone();
            move || {
                let (w, h) = (el.client_width().max(1), el.client_height().max(1));
                let dpr = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());
                el.set_width((f64::from(w) * dpr) as u32);
                el.set_height((f64::from(h) * dpr) as u32);
                st.update_value(|l| {
                    l.scene.cam.w = w as f32;
                    l.scene.cam.h = h as f32;
                    if let Some(c) = l.canvas.as_mut() {
                        c.set_dpr(dpr);
                    }
                    l.dirty = true;
                });
            }
        };
        size();
        let on_resize = Closure::<dyn FnMut()>::new(size);
        let ro = web_sys::ResizeObserver::new(on_resize.as_ref().unchecked_ref()).ok();
        if let Some(ro) = &ro {
            ro.observe(&el);
        }
        // The wheel on the canvas itself and not passive: leptos delegates `on:wheel` to the
        // document, where Chromium makes it passive and ignores the `preventDefault` that keeps the
        // page from scrolling while zooming.
        let wheel = Closure::<dyn FnMut(web_sys::WheelEvent)>::new(move |e: web_sys::WheelEvent| {
            e.prevent_default();
            // Lines and pages (Firefox) as pixels.
            let unit = match e.delta_mode() {
                1 => 16.0,
                2 => 400.0,
                _ => 1.0,
            };
            st.update_value(|l| {
                l.scene.cam.zoom *= (-(e.delta_y() * unit) as f32 * 0.0012).exp();
                l.scene.cam.clamp();
                l.dirty = true;
            });
        });
        let opts = web_sys::AddEventListenerOptions::new();
        opts.set_passive(false);
        let _ = el.add_event_listener_with_callback_and_add_event_listener_options("wheel", wheel.as_ref().unchecked_ref(), &opts);
        let frame = Closure::<dyn FnMut(f64)>::new(move |ts: f64| self.frame(ts / 1000.0));
        self.looper.update_value(|lp| *lp = Looper { frame: Some(frame), raf: None, ro, on_resize: Some(on_resize), wheel: Some((el, wheel)) });
        self.schedule();
    }

    /// A frame at `t` seconds: the clock runs, what it passed plays, the scene paints if it changed.
    fn frame(self, t: f64) {
        let Some(f) = self.filter.try_get_untracked() else { return };
        let alive = self.st.try_update_value(|l| {
            let gap = t - l.last_t;
            let dt = gap.clamp(0.0, 0.25);
            l.last_t = t;
            let mut out = Out::default();
            let edge = l.edge();
            if l.clock.live() {
                l.clock.sim = edge;
            } else if !l.scrubbing {
                let from = l.clock.sim;
                l.clock.run(t, edge);
                l.replay(from, f, t, gap > GAP_S, &mut out);
                if l.clock.sim >= edge {
                    l.clock.speed = 0.0;
                }
            }
            // Each waiting event on its turn, more than one a frame when they play close; no burst
            // for the time nothing waited.
            let mut due = l.next_pop.max(t - dt);
            while due <= t
                && let Some((p, how)) = l.pending.pop_front()
            {
                l.scene.play_handed(p, how);
                due += l.gap;
            }
            l.next_pop = due;
            let wall = now_ms();
            if l.day_end > 0.0 && wall >= l.day_end {
                out.hud = Some(l.new_day());
            }
            let minute = (wall / 60_000.0).floor();
            let new_minute = minute.to_bits() != l.minute.to_bits();
            l.minute = minute;
            l.scene.frame(t);
            if needs_draw(l) {
                if let Some(c) = l.canvas.as_mut() {
                    paint(&l.scene, c);
                }
                l.shown = l.scene.shown();
                l.dirty = false;
            }
            let speed = (!l.clock.live()).then_some(l.clock.speed);
            (out, speed, (l.clock.sim / HEAD_STEP).round() * HEAD_STEP, tally::pulse(&l.model.events, l.clock.sim), new_minute.then_some(minute))
        });
        let Some((out, speed, at, pulse, minute)) = alive else { return };
        self.show(out);
        if let Some(m) = minute {
            self.minute.set(m);
        }
        if self.speed.get_untracked() != speed {
            self.speed.set(speed);
        }
        if self.head.get_untracked().to_bits() != at.to_bits() {
            self.head.set(at);
        }
        if self.pulse.with_untracked(|p| p.0) != pulse {
            self.pulse.update(|p| p.0 = pulse);
        }
        self.schedule();
    }

    /// The view is gone. The pending frame is cancelled before its closure is dropped (a frame that
    /// fired after would call into a dropped closure), and so is the wheel listener.
    fn stop(self) {
        let _ = self.looper.try_update_value(|lp| {
            if let (Some(id), Some(w)) = (lp.raf.take(), web_sys::window()) {
                let _ = w.cancel_animation_frame(id);
            }
            if let Some(ro) = lp.ro.take() {
                ro.disconnect();
            }
            if let Some((el, wheel)) = lp.wheel.take() {
                let _ = el.remove_event_listener_with_callback("wheel", wheel.as_ref().unchecked_ref());
            }
            lp.frame = None;
            lp.on_resize = None;
        });
    }

    fn pick_at(self, x: f32, y: f32) -> Option<Pick> {
        self.st
            .try_with_value(|l| {
                let s = l.sel_of(l.scene.hit(x, y)?)?;
                let det = detail(&l.model, s)?;
                let lines = det.facts.into_iter().take(2).map(|(k, v)| format!("{k}: {v}")).collect();
                Some(Pick { title: det.title, lines })
            })
            .flatten()
    }

    fn pointer_down(self, e: &web_sys::PointerEvent) {
        if let Some(t) = e.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) {
            let _ = t.set_pointer_capture(e.pointer_id());
        }
        let (x, y) = point(e);
        self.st.update_value(|l| l.drag = Some((x, y, 0.0)));
    }

    /// A drag turns the cube; else the hover names what is under the pointer.
    fn pointer_move(self, e: &web_sys::PointerEvent) {
        let (x, y) = point(e);
        let dragging = self
            .st
            .try_update_value(|l| {
                let (px, py, dist) = l.drag?;
                l.scene.cam.yaw += (x - px) * 0.008;
                l.scene.cam.pitch += (y - py) * 0.008;
                l.scene.cam.clamp();
                l.drag = Some((x, y, dist + (x - px).abs() + (y - py).abs()));
                Some(())
            })
            .flatten()
            .is_some();
        if dragging {
            self.hover.set(None);
            return;
        }
        let Some((a, flip)) = self.st.try_with_value(|l| (l.scene.hit(x, y), x > l.scene.cam.w * 0.62)) else { return };
        if self.st.try_with_value(|l| l.hover) != Some(a) {
            self.st.update_value(|l| l.hover = a);
            self.hover.set(a.and_then(|_| self.pick_at(x, y)));
        }
        if self.hover.with_untracked(Option::is_some) {
            self.tip_at.set((x, y, flip));
        }
    }

    /// A press that hardly moved is a click: it selects what is under it, or nothing.
    fn pointer_up(self, e: &web_sys::PointerEvent) {
        let drag = self.st.try_update_value(|l| l.drag.take()).flatten();
        if drag.is_some_and(|(_, _, dist)| dist < 5.0) {
            let (x, y) = point(e);
            self.picked.set(self.st.try_with_value(|l| l.scene.hit(x, y).and_then(|a| l.sel_of(a))).flatten());
        }
    }

    fn reset(self) {
        self.st.update_value(|l| {
            let (w, h) = (l.scene.cam.w, l.scene.cam.h);
            l.scene.cam = Camera { w, h, ..Camera::default() };
            l.dirty = true;
        });
    }

    /// Selects the next (or the previous) of the collective, its machines and its drones, in the
    /// scene's order: the keyboard's way to what a click selects.
    fn cycle(self, forward: bool) {
        let Some(all) = self.st.try_with_value(|l| {
            let nodes = l.scene.node_key.iter().map(|k| Sel::Node(*k));
            std::iter::once(Sel::Core).chain(nodes).chain(l.scene.drone_session.iter().map(|s| Sel::Drone(*s))).collect::<Vec<_>>()
        }) else {
            return;
        };
        let at = self.picked.get_untracked().and_then(|p| all.iter().position(|s| *s == p));
        let n = all.len();
        let next = match (at, forward) {
            (None, true) => 0,
            (None, false) => n.saturating_sub(1),
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
        };
        self.picked.set(all.get(next).copied());
    }

    /// Keys on the focused canvas: arrows turn, + and - zoom, 0 resets, ] and [ select the next or
    /// the previous machine or drone, Escape closes the panel. The page does not scroll under the
    /// keys the cube takes.
    fn key(self, e: &web_sys::KeyboardEvent) {
        let k = e.key();
        let step = 0.12;
        let turned = self
            .st
            .try_update_value(|l| {
                let c = &mut l.scene.cam;
                match k.as_str() {
                    "ArrowLeft" => c.yaw -= step,
                    "ArrowRight" => c.yaw += step,
                    "ArrowUp" => c.pitch -= step,
                    "ArrowDown" => c.pitch += step,
                    "+" | "=" => c.zoom *= 1.15,
                    "-" | "_" => c.zoom /= 1.15,
                    _ => return false,
                }
                c.clamp();
                l.dirty = true;
                true
            })
            .unwrap_or(false);
        let other = match k.as_str() {
            "0" => {
                self.reset();
                true
            }
            "Escape" => {
                self.picked.set(None);
                true
            }
            "]" => {
                self.cycle(true);
                true
            }
            "[" => {
                self.cycle(false);
                true
            }
            _ => false,
        };
        if turned || other {
            e.prevent_default();
        }
    }

    /// Plays from `ms` at the chosen rate (live from the present on) and lists what was shown by then.
    fn seek(self, ms: f64, rate: f64) {
        let f = self.filter.get_untracked();
        if let Some(ticks) = self.st.try_update_value(|l| l.seek(ms, rate, f)) {
            self.ticker.set(ticks);
        }
    }

    fn set_rate(self, r: f64) {
        self.rate.set(r);
        self.st.update_value(|l| {
            if !l.clock.live() {
                l.clock = Clock::at(l.clock.sim, r);
            }
        });
    }

    /// Dragging the bottom edge sets the view's height; letting go keeps it for the next visit.
    fn grip_down(self, e: &web_sys::PointerEvent) {
        if let Some(t) = e.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) {
            let _ = t.set_pointer_capture(e.pointer_id());
        }
        let h = self.root.get_untracked().map_or(MIN_H, |r| f64::from(r.offset_height()));
        self.grip.set_value(Some((f64::from(e.client_y()), h)));
    }

    fn grip_move(self, e: &web_sys::PointerEvent) {
        if let Some((y, h)) = self.grip.get_value() {
            self.height.set(Some((h + f64::from(e.client_y()) - y).max(MIN_H)));
        }
    }

    fn grip_up(self) {
        if self.grip.get_value().is_some() {
            self.grip.set_value(None);
            self.keep_height();
        }
    }

    /// The edge as a separator: arrows move it, Home goes back to the page's own height.
    fn grip_key(self, e: &web_sys::KeyboardEvent) {
        let h = self.root.get_untracked().map_or(MIN_H, |r| f64::from(r.offset_height()));
        let to = match e.key().as_str() {
            "ArrowUp" => Some((h - STEP_H).max(MIN_H)),
            "ArrowDown" => Some(h + STEP_H),
            "Home" => None,
            _ => return,
        };
        e.prevent_default();
        self.height.set(to);
        self.keep_height();
    }

    fn keep_height(self) {
        keep(HEIGHT_KEY, &self.height.get_untracked().map_or_else(String::new, |h| format!("{h:.0}")));
    }

    /// Full screen and back (Esc leaves it too); the canvas follows its new size.
    fn fullscreen(self) {
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else { return };
        if doc.fullscreen_element().is_some() {
            doc.exit_fullscreen();
        } else if let Some(r) = self.root.get_untracked() {
            let _ = r.request_fullscreen();
        }
    }

    fn toggle(self, c: Cat) {
        self.filter.update(|f| *f = f.toggle(c));
        let f = self.filter.get_untracked();
        keep(FILTER_KEY, &f.stored());
        if let Some(ticks) = self.st.try_update_value(|l| {
            l.pending.retain(|(p, _)| f.shows(p.kind));
            l.recent(f)
        }) {
            self.ticker.set(ticks);
        }
    }

    /// The store's time under the pointer on the timeline. The strip's children take no pointer, so
    /// the offset is the strip's own.
    fn strip_ms(self, e: &web_sys::MouseEvent) -> Option<f64> {
        let el = self.strip.get_untracked()?;
        let u = (f64::from(e.offset_x()) / f64::from(el.client_width().max(1))).clamp(0.0, 1.0);
        Some(u * WEEK_MS + self.hist.with_untracked(|h| h.0))
    }

    /// Dragging the timeline holds the clock where it is; letting go plays on from there.
    fn strip_down(self, e: &web_sys::PointerEvent) {
        if let Some(t) = e.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) {
            let _ = t.set_pointer_capture(e.pointer_id());
        }
        self.st.update_value(|l| l.scrubbing = true);
        if let Some(ms) = self.strip_ms(e) {
            self.seek(ms, self.rate.get_untracked());
        }
    }

    fn strip_move(self, e: &web_sys::PointerEvent) {
        if self.st.try_with_value(|l| l.scrubbing).unwrap_or(false)
            && let Some(ms) = self.strip_ms(e)
        {
            self.seek(ms, self.rate.get_untracked());
        }
    }

    /// The timeline as a slider: arrows an hour, pages a day, Home its start, End live.
    fn strip_key(self, e: &web_sys::KeyboardEvent) {
        let Some((sim, start)) = self.st.try_with_value(|l| (l.clock.sim, l.start())) else { return };
        let to = match e.key().as_str() {
            "ArrowLeft" | "ArrowDown" => sim - HOUR_MS,
            "ArrowRight" | "ArrowUp" => sim + HOUR_MS,
            "PageDown" => sim - DAY_MS,
            "PageUp" => sim + DAY_MS,
            "Home" => start,
            "End" => f64::INFINITY,
            _ => return,
        };
        e.prevent_default();
        self.seek(to, self.rate.get_untracked());
    }

    /// Where the playback clock is, as the timeline says it.
    fn at_text(self) -> String {
        self.speed.get().map_or_else(
            || "live".to_string(),
            |s| {
                let h = self.head.get();
                format!("{} {} · {}", day_label(h), clock(h), rate_label(s))
            },
        )
    }

    fn head_pct(self) -> f64 {
        ((self.head.get() - self.hist.with(|h| h.0)) / WEEK_MS * 100.0).clamp(0.0, 100.0)
    }

    fn canvas_view(self) -> impl IntoView {
        view! {
            <canvas
                node_ref=self.canvas
                class="cube-canvas"
                tabindex="0"
                role="img"
                aria-label=move || self.hud.with(|h| format!("{} machines, {} drones active, {} tells and asks today. Arrow keys turn, plus and minus zoom, 0 resets, ] and [ select a machine or drone.", h.machines, h.drones, h.tx_today))
                on:pointerdown=move |e| self.pointer_down(&e)
                on:pointermove=move |e| self.pointer_move(&e)
                on:pointerup=move |e| self.pointer_up(&e)
                on:pointercancel=move |_| self.st.update_value(|l| l.drag = None)
                on:pointerleave=move |_| self.hover.set(None)
                on:dblclick=move |_| self.reset()
                on:keydown=move |e| self.key(&e)
            ></canvas>
        }
    }

    /// The counters. Each title says exactly what its number counts, also to a screen reader; the
    /// number counts up in CSS.
    fn hud_view(self) -> impl IntoView {
        let hud = self.hud;
        let tile = move |label: &'static str, title: &'static str, f: fn(&Hud) -> usize| {
            view! {
                <div class="cube-stat" title=title>
                    <b style=move || format!("--n:{}", hud.with(f)) aria-hidden="true"></b>
                    <span class="cube-vh">{move || hud.with(f)}</span>
                    <span>{label}</span>
                    <span class="cube-vh">{title}</span>
                </div>
            }
        };
        let pulse = self.pulse;
        let meter = move || pulse.with(|p| if p.1 == 0 { 0.0 } else { p.0 as f64 / p.1 as f64 * 100.0 });
        view! {
            <div class="cube-hud">
                <div class="cube-title">"UNIMATRIX · " {move || self.d.collective.get().to_uppercase()}</div>
                <div class="cube-stats">
                    {tile("machines", "machines in the collective: this one and every member it heard of", |h| h.machines)}
                    {tile("links", "machines this one dials or has synced with, on its protocol", |h| h.links)}
                    {tile("drones active", "drone sessions not ended, on every machine", |h| h.drones)}
                    {tile("working", "drone sessions working right now, on every machine", |h| h.working)}
                    {tile("sent today", "tells and asks sent since local midnight (today, your time), on every machine", |h| h.tx_today)}
                    {tile("received today", "times a session was shown a message since local midnight (today, your time)", |h| h.shown_today)}
                    {tile("lessons today", "new lessons written to the collective since local midnight (today, your time); a new version of a lesson is not counted", |h| h.lessons_today)}
                    <div class="cube-stat" title="events of every kind in the last minute of the playback clock; the bar is that against the busiest minute of the last 7 days">
                        <b style=move || format!("--n:{}", pulse.with(|p| p.0)) aria-hidden="true"></b>
                        <span class="cube-vh">{move || pulse.with(|p| p.0)}</span>
                        <span>"pulse / min"</span>
                        <span class="cube-vh">"events of every kind in the last minute of the playback clock, against the busiest minute of the last 7 days"</span>
                        <i class="cube-meter" aria-hidden="true"><i style=move || format!("width:{:.0}%", meter())></i></i>
                    </div>
                </div>
            </div>
        }
    }

    /// The middle: the ticker (a lesson's line opens it), the toasts, the panel. The ticker is the
    /// live region on the live clock only: a replay rewrites it every frame, too fast to announce.
    /// Its tones are prefixed (`tk-`, `toast-`): a bare `lesson` is the Knowledge view's card.
    fn mid_view(self) -> impl IntoView {
        let (open, minute) = (self.d.open, self.minute);
        let since = move |ms: f64| {
            move || {
                minute.track();
                ago(ms)
            }
        };
        let shown = Memo::new(move |_| {
            self.rev.track();
            self.st.try_with_value(|l| detail(&l.model, self.picked.get()?)).flatten()
        });
        view! {
            <div class="cube-mid">
                <ol class="cube-ticker" aria-live=move || if self.speed.get().is_some() { "off" } else { "polite" } aria-label="what moved, newest first">
                    <For each=move || self.ticker.get() key=|t| t.id let:t>
                        {match t.lesson.clone() {
                            Some(id) => view! {
                                <li class=format!("tk tk-{} open", t.tone)>
                                    <button on:click=move |_| open.set(Some(id.clone())) title="open the lesson">
                                        <span class="tk-kind">{t.kind}</span>{t.line.clone()}<span class="tk-ago">{since(t.ms)}</span>
                                    </button>
                                </li>
                            }.into_any(),
                            None => view! {
                                <li class=format!("tk tk-{}", t.tone)><span class="tk-kind">{t.kind}</span>{t.line.clone()}<span class="tk-ago">{since(t.ms)}</span></li>
                            }.into_any(),
                        }}
                    </For>
                </ol>
                <div class="cube-toasts" aria-hidden="true">
                    <For each=move || self.toasts.get() key=|t| t.id let:t>
                        <div class=format!("cube-toast toast-{}", t.tone)>{t.text}</div>
                    </For>
                </div>
                {move || shown.get().map(|det| self.panel_view(det))}
            </div>
        }
    }

    /// The selected machine, drone or the collective; it links to what it names. Built anew only
    /// when what it shows changed (a `Memo` of the `Detail`), so the focus in it stays; a link in it
    /// that selects another hands the focus to the new panel's heading.
    fn panel_view(self, det: Detail) -> impl IntoView {
        let (picked, refocus) = (self.picked, self.refocus);
        let go = move |s: Sel| {
            refocus.set_value(true);
            picked.set(Some(s));
        };
        let head = NodeRef::<leptos::html::H3>::new();
        head.on_load(move |el| {
            if refocus.try_update_value(std::mem::take).unwrap_or(false) {
                let _ = el.focus();
            }
        });
        let label = format!("selected: {}", det.title);
        view! {
            <aside class="cube-panel" aria-label=label>
                <button class="cube-close" on:click=move |_| picked.set(None) aria-label="close">"×"</button>
                <h3 node_ref=head tabindex="-1">{det.title}</h3>
                <dl>
                    {det.facts.into_iter().map(|(k, v)| view! { <dt>{k}</dt><dd>{v}</dd> }).collect_view()}
                    {det.machine.map(|(k, host)| view! {
                        <dt>"Machine"</dt>
                        <dd><button class="cube-link" on:click=move |_| go(Sel::Node(k))>{host}</button></dd>
                    })}
                </dl>
                {(!det.drones.is_empty()).then(|| view! {
                    <ul class="cube-drones" aria-label="its drones">
                        {det.drones.into_iter().map(|(s, name)| view! {
                            <li><button class="cube-link" on:click=move |_| go(Sel::Drone(s))>{name}</button></li>
                        }).collect_view()}
                    </ul>
                })}
                {det.messages.then(|| view! { <a class="cube-link" href="#messages">"Messages →"</a> })}
            </aside>
        }
    }

    /// The bottom: the legend chips that filter, the timeline that scrubs.
    fn deck_view(self) -> impl IntoView {
        let filter = self.filter;
        view! {
            <div class="cube-deck">
                <div class="cube-legend" role="group" aria-label="show">
                    {Cat::ALL.into_iter().map(|c| view! {
                        <button
                            class=move || format!("lg {}{}", c.key(), if filter.get().has(c) { "" } else { " off" })
                            aria-pressed=move || filter.get().has(c).to_string()
                            title=c.help()
                            on:click=move |_| self.toggle(c)
                        >{c.label()}</button>
                    }).collect_view()}
                    <span class="lg-help">"drag to turn · scroll to zoom · double-click to reset"</span>
                    <button class="lg-fs" on:click=move |_| self.fullscreen() title="full screen (Esc leaves it)">"⛶ full screen"</button>
                </div>
                {self.timeline_view()}
            </div>
        }
    }

    /// The 7 days as hours stacked by chip, the local midnights, the playhead, and the rates.
    fn timeline_view(self) -> impl IntoView {
        let (speed, rate, hist) = (self.speed, self.rate, self.hist);
        let bars = Memo::new(move |_| hist.with(|h| tally::bars(&h.1, self.filter.get())));
        let days = Memo::new(move |_| midnights(hist.with(|h| h.0)));
        // The rate chosen stays lit, live or not: a click on the strip plays at it.
        let chosen = move |r: f64| rate.get().to_bits() == r.to_bits();
        view! {
            <div class="cube-tl">
                <div class="tl-btns" role="group" aria-label="playback">
                    {RATES.into_iter().map(|r| view! {
                        <button
                            on:click=move |_| self.set_rate(r)
                            aria-pressed=move || chosen(r).to_string()
                            title=format!("replays run at {} the real clock: click the timeline to play from there", rate_label(r))
                        >{rate_label(r)}</button>
                    }).collect_view()}
                    <button class="tl-live" on:click=move |_| self.seek(f64::INFINITY, 0.0) aria-pressed=move || speed.get().is_none().to_string() title="back to what happens now">"● LIVE"</button>
                </div>
                <div
                    node_ref=self.strip
                    class="tl-strip"
                    role="slider"
                    tabindex="0"
                    aria-label="playback time in the last 7 days"
                    aria-valuemin="0"
                    aria-valuemax=HOURS.to_string()
                    aria-valuenow=move || format!("{:.0}", self.head_pct() / 100.0 * HOURS as f64)
                    aria-valuetext=move || self.at_text()
                    on:pointerdown=move |e| self.strip_down(&e)
                    on:pointermove=move |e| self.strip_move(&e)
                    on:pointerup=move |_| self.st.update_value(|l| l.scrubbing = false)
                    on:pointercancel=move |_| self.st.update_value(|l| l.scrubbing = false)
                    on:keydown=move |e| self.strip_key(&e)
                >
                    <svg viewBox=format!("0 0 {HOURS} {TL_H}") preserveAspectRatio="none" aria-hidden="true">
                        {move || bars.get().into_iter().map(|(c, d)| view! { <path class=c.key() d=d /> }).collect_view()}
                    </svg>
                    {move || days.get().into_iter().map(|(x, w)| view! { <span class="tl-day" style=format!("left:{x:.2}%")>{w}</span> }).collect_view()}
                    <span class="tl-head" style=move || format!("left:{:.2}%", self.head_pct())></span>
                </div>
                <span class="tl-at" aria-hidden="true">{move || self.at_text()}</span>
            </div>
        }
    }

    /// The hover's tip: built when what is under the pointer changes, moved by its style alone.
    fn tip_view(self) -> impl IntoView {
        let at = self.tip_at;
        move || {
            self.hover.get().map(|h| {
                view! {
                    <div
                        class=move || if at.with(|a| a.2) { "cube-tip flip" } else { "cube-tip" }
                        style=move || at.with(|a| format!("left:{}px;top:{}px", a.0 as i32, a.1 as i32))
                    >
                        <b>{h.title}</b>
                        {h.lines.into_iter().map(|l| view! { <div>{l}</div> }).collect_view()}
                    </div>
                }
            })
        }
    }
}

/// A pointer's place on the canvas: `offsetX` is in the canvas's own CSS pixels, what the camera's
/// `w` and `h` are, also under a CSS transform of it; nothing over the canvas takes the pointer.
fn point(e: &web_sys::MouseEvent) -> (f32, f32) {
    (e.offset_x() as f32, e.offset_y() as f32)
}

/// The local midnights within the timeline from `start`: where each sits (percent) and its weekday.
fn midnights(start: f64) -> Vec<(f64, &'static str)> {
    let mut out = Vec::new();
    if start <= 0.0 {
        return out;
    }
    // A day and a half on from a midnight is always in the next day, whatever DST does.
    let mut m = local_midnight(local_midnight(start) + 1.5 * DAY_MS);
    while m < start + WEEK_MS {
        out.push(((m - start) / WEEK_MS * 100.0, weekday(m)));
        m = local_midnight(m + 1.5 * DAY_MS);
    }
    out
}

#[component]
pub fn Cube() -> impl IntoView {
    let u = Ui::new();
    // Another collective: start over.
    Effect::new(move |_| {
        let _ = u.d.collective.get();
        u.st.update_value(|l| {
            l.model.clear();
            l.pending.clear();
            l.synced.clear();
            l.scene.fx.clear();
            l.got = Got::Nothing;
        });
        u.ticker.set(Vec::new());
        u.toasts.set(Vec::new());
        u.picked.set(None);
        u.fetch(true);
    });
    // The stream named the Cube's files.
    Effect::new(move |prev: Option<u32>| {
        let n = u.d.mesh.get();
        if prev.is_some() {
            u.fetch(false);
        }
        n
    });
    u.canvas.on_load(move |el| u.start(el));
    on_cleanup(move || u.stop());

    view! {
        <section
            node_ref=u.root
            class="cube"
            style=move || u.height.get().map(|h| format!("height:{h:.0}px")).unwrap_or_default()
            aria-label="The collective as a cube: machines, drones and what moves between them, live"
        >
            {u.canvas_view()}
            <div class="cube-ui">
                {u.hud_view()}
                {u.mid_view()}
                {u.deck_view()}
            </div>
            {u.tip_view()}
            <div
                class="cube-grip"
                role="separator"
                aria-orientation="horizontal"
                aria-label="the view's height: drag, or arrow keys; Home or a double-click for the default"
                title="drag to resize · double-click for the default height"
                tabindex="0"
                on:pointerdown=move |e| u.grip_down(&e)
                on:pointermove=move |e| u.grip_move(&e)
                on:pointerup=move |_| u.grip_up()
                on:pointercancel=move |_| u.grip_up()
                on:dblclick=move |_| {
                    u.height.set(None);
                    u.keep_height();
                }
                on:keydown=move |e| u.grip_key(&e)
            ></div>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::{Clock, RATES, of};

    /// A replay runs on the real clock: at 600× ten minutes pass in each real second, however the
    /// frames fall (uneven, dropped, a hidden tab's gap), and a new rate runs on from where it is.
    #[test]
    fn a_replay_keeps_the_real_clock() {
        let edge = f64::INFINITY;
        let mut c = Clock::at(0.0, 600.0);
        for t in [10.0, 10.016, 10.05, 10.3, 12.0, 16.0] {
            c.run(t, edge);
        }
        assert!((c.sim - 6.0 * 600_000.0).abs() < 1e-6, "{}", c.sim);
        c = Clock::at(c.sim, RATES[0]);
        c.run(16.5, edge);
        c.run(17.5, edge);
        assert!((c.sim - (3_600_000.0 + 3_600_000.0)).abs() < 1e-6, "{}", c.sim);
        c.run(1000.0, 5_000_000.0);
        assert!((c.sim - 5_000_000.0).abs() < 1e-6, "stops at the edge");
        assert_eq!(RATES, {
            let mut r = RATES;
            r.sort_by(|a, b| b.total_cmp(a));
            r
        });
    }

    #[test]
    fn a_drone_is_named_one_of_many() {
        assert_eq!(of(7, 9), "Seven of Nine");
        assert_eq!(of(25, 39), "Twenty-Five of Thirty-Nine");
        assert_eq!(of(20, 90), "Twenty of Ninety");
        assert_eq!(of(99, 100), "Ninety-Nine of 100");
    }
}
