//! `borg web [--port 7979] [--dir DIR]`: a live page of the collective's knowledge and usage statistics,
//! on 127.0.0.1 only.
//!
//! It reads the stores the way `borg top` does (files, no IPC with `borg mesh serve`), so local
//! collectives show too. `/api/events` is a server-sent event stream: a 1 s watch on the store files
//! tells the page which views changed, and the page refetches only those. Once a minute it publishes
//! this node's daily rollups (borg stats.rs) so the mesh can spread them.
//!
//! The binary (main.rs) parses the command line and serves [`router`]; tests drive it in process.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use borg::collective::{self, Route, Share};
use borg::{conscience, install, learn, memory, outcome, presence, stats, top};
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};

pub const USAGE: &str = "usage: borg web [--port PORT] [--dir DIR]   (default port 7979; DIR picks the collective shown first)";

/// One collective the page can show.
#[derive(Clone)]
pub struct Mind {
    pub name: String,
    pub home: PathBuf,
    pub share: Share,
    /// The delivery mode its drones run in (borg live.rs: `hook` or `live`).
    pub mode: &'static str,
}

pub struct App {
    pub minds: Vec<Mind>,
    /// The collective the page opens with: the one the start directory routes to.
    pub first: String,
    /// Turns true on Ctrl-C. Event streams end on it, else graceful shutdown waits for every open page.
    pub stop: tokio::sync::watch::Receiver<bool>,
}

impl App {
    fn mind(&self, name: Option<&str>) -> Option<&Mind> {
        let name = name.unwrap_or(&self.first);
        self.minds.iter().find(|m| m.name == name)
    }
}

#[derive(Deserialize)]
struct Pick {
    c: Option<String>,
}

/// Every collective on this machine, and the one `dir` routes to (else the first).
pub fn minds(dir: &str) -> Result<(Vec<Mind>, String), String> {
    let cfg = collective::load()?;
    let base = learn::home();
    let minds: Vec<Mind> = install::collectives(cfg.as_ref(), &base)
        .into_iter()
        .filter_map(|r| match r {
            Route::Collective { name, home, share } => {
                let mode = borg::live::of(cfg.as_ref(), &name).mode.name();
                Some(Mind { name, home, share, mode })
            }
            Route::Off => None,
        })
        .collect();
    let first = match collective::route(cfg.as_ref(), dir, &base) {
        Route::Collective { name, .. } => name,
        Route::Off => minds.first().map(|m| m.name.clone()).unwrap_or_default(),
    };
    Ok((minds, first))
}

fn not_found(what: &str) -> Response {
    (StatusCode::NOT_FOUND, format!("no collective {what}")).into_response()
}

async fn collectives(State(app): State<Arc<App>>) -> Json<Value> {
    let list: Vec<Value> =
        app.minds.iter().map(|m| json!({"name": m.name, "share": if m.share == Share::Team { "team" } else { "local" }, "mode": m.mode})).collect();
    Json(json!({"first": app.first, "collectives": list}))
}

/// Buckets summed per key (day or engineer), in key order.
fn sum_by(buckets: &[stats::Bucket], key: impl Fn(&stats::Bucket) -> String) -> Vec<(String, stats::Bucket)> {
    let mut out: BTreeMap<String, stats::Bucket> = BTreeMap::new();
    for b in buckets {
        out.entry(key(b)).or_default().add(b);
    }
    out.into_iter().collect()
}

fn totals(b: &stats::Bucket) -> Value {
    json!({"counts": b.counts, "usd": b.usd, "usd_by": b.usd_by, "usd_mode": b.usd_mode, "channels": b.channels, "dur_ms": b.dur_ms})
}

/// One entry per day from `first` to `last`, zero where nothing was counted.
fn series(buckets: &[stats::Bucket], first: &str, last: &str) -> Vec<Value> {
    let by: BTreeMap<String, stats::Bucket> = sum_by(buckets, |b| b.day.clone()).into_iter().collect();
    stats::days_between(first, last)
        .into_iter()
        .map(|d| {
            let t = by.get(&d).map(totals).unwrap_or_else(|| totals(&stats::Bucket::default()));
            json!({"day": d, "totals": t})
        })
        .collect()
}

async fn stats_of(State(app): State<Arc<App>>, Query(p): Query<Pick>) -> Response {
    let Some(m) = app.mind(p.c.as_deref()) else { return not_found(p.c.as_deref().unwrap_or_default()) };
    let buckets = stats::current(&m.home);
    let mut all = stats::Bucket::default();
    for b in &buckets {
        all.add(b);
    }
    let aliases = collective::aliases();
    let first = buckets.iter().map(|b| b.day.as_str()).min().unwrap_or_default().to_string();
    let last = buckets.iter().map(|b| b.day.as_str()).max().unwrap_or_default().to_string();
    let days = series(&buckets, &first, &last);
    let engineers: Vec<Value> = sum_by(&buckets, |b| aliases.display(&b.engineer))
        .into_iter()
        .map(|(e, t)| {
            let mine: Vec<stats::Bucket> = buckets.iter().filter(|b| aliases.display(&b.engineer) == e).cloned().collect();
            json!({"engineer": e, "totals": totals(&t), "days": series(&mine, &first, &last)})
        })
        .collect();
    let nodes = buckets.iter().map(|b| b.node.as_str()).collect::<std::collections::BTreeSet<_>>().len();
    Json(json!({"collective": m.name, "totals": totals(&all), "days": days, "engineers": engineers, "nodes": nodes})).into_response()
}

async fn lessons(State(app): State<Arc<App>>, Query(p): Query<Pick>) -> Response {
    let Some(m) = app.mind(p.c.as_deref()) else { return not_found(p.c.as_deref().unwrap_or_default()) };
    let path = m.home.join("lessons.jsonl");
    let list = conscience::load_unweighted(&path, None);
    let outcomes = outcome::read_team(&path);
    let mem = memory::derive(&outcomes, &list, None);
    // How often each lesson took part in what stats.rs counted (asked in a block, so far), team-wide.
    let mut usage: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for b in stats::current(&m.home) {
        for (id, kinds) in b.lessons {
            let u = usage.entry(id).or_default();
            for (k, v) in kinds {
                *u.entry(k).or_default() += v;
            }
        }
    }
    let aliases = collective::aliases();
    let stored: BTreeMap<String, (u32, String)> = learn::lessons(&path).into_iter().map(|l| (l.id, (l.version, l.repo))).collect();
    let mut out: Vec<Value> = list
        .iter()
        .map(|c| {
            let mm = mem.get(&c.id);
            json!({
                "id": c.id, "question": c.question, "situations": c.situations, "consequence": c.consequence,
                "area": c.area, "scope": c.scope, "source": c.source, "origin_ms": c.origin_ms,
                "engineers": c.engineers.iter().map(|e| aliases.display(e)).collect::<Vec<_>>(),
                "activation": mm.map_or(0.0, |x| x.activation), "changed": mm.map_or(0, |x| x.changed),
                "ignored": mm.map_or(0, |x| x.ignored), "corrected": mm.map_or(0, |x| x.corrected), "vetoed": mm.map_or(0, |x| x.vetoed),
                "regretted": mm.map_or(0.0, |x| x.regretted),
                "dormant": mm.is_some_and(|x| x.dormant), "consolidated": mm.is_some_and(|x| x.consolidated),
                "usage": usage.get(&c.id).cloned().unwrap_or_default(),
                "version": stored.get(&c.id).map_or(1, |s| s.0), "repo": stored.get(&c.id).map(|s| s.1.clone()).unwrap_or_default(),
                "reach": c.reach,
            })
        })
        .collect();
    out.sort_by(|a, b| b["activation"].as_f64().partial_cmp(&a["activation"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
    Json(json!({"collective": m.name, "lessons": out})).into_response()
}

/// What the feed shows: what people and drones did, not the routine of every turn end.
const NOTABLE: [&str; 25] = [
    stats::CORRECTION,
    stats::VETO,
    stats::VETO_REPEAT,
    stats::REVIEW_FINDING,
    stats::BLOCK,
    stats::CHANGED,
    stats::IGNORED,
    stats::CORRECTED,
    stats::REGRETTED,
    stats::LESSON_NEW,
    stats::LESSON_MERGED,
    stats::NOT_A_LESSON,
    stats::STEER_ISSUED,
    stats::STEER_DELIVERED,
    stats::STEER_DROPPED,
    stats::PAUSE_REFUSED,
    stats::QUEEN_QUEUED,
    stats::QUEEN_DELIVERED,
    stats::QUEEN_ANSWERED,
    stats::QUEEN_SKIPPED,
    stats::QUEEN_DISMISSED,
    stats::ASSIMILATED,
    stats::EARLY,
    stats::QUEUED_FREE,
    stats::MODEL_CALL,
];

#[derive(Deserialize)]
struct FeedQuery {
    c: Option<String>,
    limit: Option<usize>,
}

/// This machine's notable events, newest first (peers' activity arrives as counts only).
async fn feed(State(app): State<Arc<App>>, Query(q): Query<FeedQuery>) -> Response {
    let Some(m) = app.mind(q.c.as_deref()) else { return not_found(q.c.as_deref().unwrap_or_default()) };
    stats::backfill(&m.home);
    let aliases = collective::aliases();
    let mut evs: Vec<stats::Event> = stats::events(&m.home).into_iter().filter(|e| NOTABLE.contains(&e.kind.as_str())).collect();
    evs.sort_by_key(|e| std::cmp::Reverse(e.ms));
    evs.truncate(q.limit.unwrap_or(200).min(2000));
    let out: Vec<Value> = evs
        .into_iter()
        .map(|e| {
            json!({"ms": e.ms, "kind": e.kind, "engineer": aliases.display(&e.engineer), "repo": e.repo, "lessons": e.lessons,
                "n": e.n, "usd": e.usd, "dur_ms": e.dur_ms, "why": e.why, "text": e.text})
        })
        .collect();
    Json(json!({"collective": m.name, "events": out})).into_response()
}

async fn queen(State(app): State<Arc<App>>, Query(p): Query<Pick>) -> Response {
    let Some(m) = app.mind(p.c.as_deref()) else { return not_found(p.c.as_deref().unwrap_or_default()) };
    let mut entries = borg::queen::entries(&m.home);
    entries.sort_by_key(|e| std::cmp::Reverse(e.ms));
    Json(json!({"collective": m.name, "entries": entries})).into_response()
}

async fn drones(State(app): State<Arc<App>>, Query(p): Query<Pick>) -> Response {
    let Some(m) = app.mind(p.c.as_deref()) else { return not_found(p.c.as_deref().unwrap_or_default()) };
    let now = learn::now_ms();
    let mut ds = top::drones(&m.home, m.share == Share::Team, now);
    for d in &mut ds {
        d.p.engineer = collective::aliases().display(&d.p.engineer);
    }
    Json(top::to_json(&m.name, m.share, &ds)).into_response()
}

/// Size and modification time of `path`, or of every file directly in it.
fn stamp(path: &Path) -> Vec<(u64, u128)> {
    let one = |p: &Path| std::fs::metadata(p).ok().map(|m| (m.len(), mtime(&m)));
    match std::fs::read_dir(path) {
        Ok(rd) => {
            let mut v: Vec<(u64, u128)> = rd.filter_map(|e| e.ok()).filter_map(|e| one(&e.path())).collect();
            v.sort();
            v
        }
        Err(_) => one(path).into_iter().collect(),
    }
}

fn mtime(m: &std::fs::Metadata) -> u128 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos())
}

/// The views of the page and the store files each depends on.
fn watched(home: &Path) -> Vec<(&'static str, Vec<PathBuf>)> {
    vec![
        (
            "lessons",
            vec![home.join("lessons.jsonl"), home.join("outcomes.jsonl"), home.join(outcome::TEAM_FILE), home.join("stats.jsonl"), stats::events_path(home)],
        ),
        ("stats", vec![home.join("stats.jsonl"), stats::events_path(home)]),
        ("drones", vec![home.join("presence"), presence::remote_path(home)]),
        ("feed", vec![stats::events_path(home)]),
        ("queen", vec![home.join("queen")]),
    ]
}

/// Per view, the stamps of the files it depends on: a view changed when its stamps did.
type Fingerprint = Vec<(&'static str, Vec<Vec<(u64, u128)>>)>;

fn fingerprint(home: &Path) -> Fingerprint {
    watched(home).into_iter().map(|(view, paths)| (view, paths.iter().map(|p| stamp(p)).collect())).collect()
}

async fn events(State(app): State<Arc<App>>, Query(p): Query<Pick>) -> Response {
    let Some(m) = app.mind(p.c.as_deref()) else { return not_found(p.c.as_deref().unwrap_or_default()) };
    let home = m.home.clone();
    let start = fingerprint(&home);
    let stream = futures_util::stream::unfold((home, start), |(home, mut last)| async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let now = fingerprint(&home);
            let changed: Vec<&str> = now.iter().zip(&last).filter(|(a, b)| a != b).map(|(a, _)| a.0).collect();
            if !changed.is_empty() {
                let ev = Event::default().event("change").data(json!({"changed": changed}).to_string());
                last = now;
                return Some((Ok::<Event, Infallible>(ev), (home, last)));
            }
            last = now;
        }
    });
    let mut stop = app.stop.clone();
    let stream = stream.take_until(async move {
        let _ = stop.wait_for(|s| *s).await;
    });
    Sse::new(Box::pin(stream) as std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>).keep_alive(KeepAlive::default()).into_response()
}

/// The built Leptos client (dist/, built from ui/, see ui/README.md).
const INDEX: &str = include_str!("../dist/index.html");
const JS: &[u8] = include_bytes!("../dist/borg-web-ui.js");
const WASM: &[u8] = include_bytes!("../dist/borg-web-ui_bg.wasm");

fn asset(kind: &'static str, body: &'static [u8]) -> Response {
    ([(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(|| async { ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], INDEX).into_response() }))
        .route("/borg-web-ui.js", get(|| async { asset("text/javascript", JS) }))
        .route("/borg-web-ui_bg.wasm", get(|| async { asset("application/wasm", WASM) }))
        .route("/api/collectives", get(collectives))
        .route("/api/stats", get(stats_of))
        .route("/api/lessons", get(lessons))
        .route("/api/drones", get(drones))
        .route("/api/feed", get(feed))
        .route("/api/queen", get(queen))
        .route("/api/events", get(events))
        .with_state(app)
}

/// Publishes this node's rollups and redacted outcomes of every collective once a minute (stats.rs
/// `rollup`, outcome.rs `publish`).
pub async fn publish(minds: Vec<Mind>) {
    loop {
        for m in &minds {
            stats::rollup(&m.home);
            outcome::publish(&m.home);
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}
