//! `borg web [--port 7979] [--dir DIR]`: a live page of the collective's knowledge and usage statistics,
//! on 127.0.0.1 only.
//!
//! It reads the stores the way `borg top` does (files, no IPC with `borg mesh serve`), so local
//! collectives show too. `/api/events` is a server-sent event stream: a 1 s watch on the store files
//! tells the page which views changed, and the page refetches only those. Once a minute it publishes
//! this node's daily rollups (borg stats.rs) so the mesh can spread them.
//!
//! `/api/update` is borg itself: this borg's version, the last release check (borg update.rs), and the
//! members by version with what an older protocol misses (borg protocol.rs). The page shows a banner
//! from it when a newer release exists or a member's protocol differs from this node's.
//!
//! The binary (main.rs) parses the command line and serves [`router`]; tests drive it in process.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use borg::collective::{self, Route, Share};
use borg::names::Book;
use borg::{absorb, conscience, install, learn, memory, memory_export, msg, outcome, presence, protocol, stats, top, update};
use futures_util::{Stream, StreamExt};

pub mod learning;
mod mesh;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const USAGE: &str = "usage: borg web [--port PORT] [--dir DIR]   (default port 7979; DIR picks the collective shown first)";

/// One collective the page can show.
#[derive(Clone)]
pub struct Mind {
    pub name: String,
    pub home: PathBuf,
    pub share: Share,
}

pub struct App {
    pub minds: Vec<Mind>,
    /// The collective the page opens with: the one the start directory routes to.
    pub first: String,
    /// The start directory: its git identity names the engineer looking at the page, whom the messages
    /// view acknowledges what it shows (`messages_seen`, borg msg.rs `ack_engineer`).
    pub cwd: String,
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
            Route::Collective { name, home, share } => Some(Mind { name, home, share }),
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
    let list: Vec<Value> = app.minds.iter().map(|m| json!({"name": m.name, "share": if m.share == Share::Team { "team" } else { "local" }})).collect();
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

/// One row per engineer, in label order: their label, their buckets summed and the buckets themselves.
/// An engineer is who `aliases` says (collective.rs `Aliases::canon`: the alias, else the email in lower
/// case), so two people who share a git user.name stay two rows. The label is the name `names` gives
/// them (names.rs), with the email after it where another row would show the same.
fn engineer_rows(buckets: &[stats::Bucket], names: &Book, aliases: &collective::Aliases) -> Vec<(String, stats::Bucket, Vec<stats::Bucket>)> {
    let mut by: BTreeMap<String, Vec<stats::Bucket>> = BTreeMap::new();
    for b in buckets {
        by.entry(aliases.canon(&b.engineer)).or_default().push(b.clone());
    }
    let label = |mine: &[stats::Bucket]| names.display_with(aliases, mine.first().map_or("", |b| b.engineer.as_str()));
    let mut count: HashMap<String, usize> = HashMap::new();
    for mine in by.values() {
        *count.entry(label(mine)).or_default() += 1;
    }
    let mut rows: Vec<(String, stats::Bucket, Vec<stats::Bucket>)> = by
        .into_iter()
        .map(|(who, mine)| {
            let name = label(&mine);
            let name = if count[&name] > 1 && name != who { format!("{name} ({who})") } else { name };
            let mut sum = stats::Bucket::default();
            for b in &mine {
                sum.add(b);
            }
            (name, sum, mine)
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

/// A bucket's totals for the page; the reasons (stats.rs `Bucket::why`) read as counts there.
fn totals(b: &stats::Bucket) -> Value {
    let mut counts = b.counts.clone();
    counts.extend(b.why.iter().map(|(k, v)| (k.clone(), *v)));
    json!({"counts": counts, "usd": b.usd, "usd_by": b.usd_by, "usd_mode": b.usd_mode, "channels": b.channels, "dur_ms": b.dur_ms})
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
    let names = Book::load(&m.home, learn::now_ms());
    let first = buckets.iter().map(|b| b.day.as_str()).min().unwrap_or_default().to_string();
    let last = buckets.iter().map(|b| b.day.as_str()).max().unwrap_or_default().to_string();
    let days = series(&buckets, &first, &last);
    let engineers: Vec<Value> = engineer_rows(&buckets, &names, collective::aliases())
        .into_iter()
        .map(|(e, t, mine)| json!({"engineer": e, "totals": totals(&t), "days": series(&mine, &first, &last)}))
        .collect();
    let nodes = buckets.iter().map(|b| b.node.as_str()).collect::<std::collections::BTreeSet<_>>().len();
    // `borg debug ope`'s support, one pass over this machine's log (pools are never shared); the estimate itself
    // derives beliefs per decision, too heavy to poll.
    let (ds, skipped) = borg::ope::decisions_in(&m.home.join("outcomes.jsonl"));
    let ope = json!({"decisions": ds.len(), "no_p": skipped.no_p, "no_pool": skipped.no_pool});
    Json(json!({"collective": m.name, "totals": totals(&all), "days": days, "engineers": engineers, "nodes": nodes, "ope": ope})).into_response()
}

/// `/api/lessons`: every lesson of the collective with its memory, usage and version, most active first.
/// The rows borrow from what one request loads ([`LessonSources`]) and are serialised once.
async fn lessons(State(app): State<Arc<App>>, Query(p): Query<Pick>) -> Response {
    let Some(m) = app.mind(p.c.as_deref()) else { return not_found(p.c.as_deref().unwrap_or_default()) };
    let s = LessonSources::load(&m.home);
    Json(LessonsBody { collective: &m.name, lessons: s.rows() }).into_response()
}

/// What `/api/lessons` reads of a store, once per request; its rows ([`LessonRow`]) borrow from it.
struct LessonSources {
    list: Vec<conscience::Challenge>,
    mem: HashMap<String, memory::Memory>,
    /// How often each lesson took part in what stats.rs counted (asked in a block, so far), team-wide.
    usage: BTreeMap<String, BTreeMap<String, u64>>,
    names: Book,
    /// Each lesson's latest version and repo (learn.rs `stored`).
    stored: HashMap<String, (u32, String)>,
}

/// A lesson without usage: its `usage` is `{}`.
static NO_USAGE: BTreeMap<String, u64> = BTreeMap::new();

impl LessonSources {
    fn load(home: &Path) -> Self {
        let path = home.join("lessons.jsonl");
        let list = conscience::load_unweighted(&path, None);
        // The outcomes as folded beside the store (borg facts.rs), as the conscience weighs them: what
        // `read_team` would read, at the cost of what was appended since the last fold.
        let facts = borg::facts::load(&path);
        let mem = memory::derive_folded(facts.view(outcome::on()).folded, &list, None);
        let mut usage: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        for b in stats::current(home) {
            for (id, kinds) in b.lessons {
                let u = usage.entry(id).or_default();
                for (k, v) in kinds {
                    *u.entry(k).or_default() += v;
                }
            }
        }
        LessonSources { list, mem, usage, names: Book::load(home, learn::now_ms()), stored: learn::stored(&path) }
    }

    /// The rows, most active first: a stable sort, so lessons of equal activation keep the store's
    /// order. O(N log N) in the lessons.
    fn rows(&self) -> Vec<LessonRow<'_>> {
        let mut out: Vec<LessonRow<'_>> = self.list.iter().map(|c| self.row(c)).collect();
        // As serde_json orders the activation it writes: a non-finite one is written as null and sorts last.
        let key = |r: &LessonRow<'_>| r.activation.is_finite().then_some(r.activation);
        out.sort_by(|a, b| key(b).partial_cmp(&key(a)).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    fn row<'a>(&'a self, c: &'a conscience::Challenge) -> LessonRow<'a> {
        let mm = self.mem.get(&c.id);
        let stored = self.stored.get(&c.id);
        LessonRow {
            id: &c.id,
            question: &c.question,
            situations: &c.situations,
            consequence: &c.consequence,
            area: &c.area,
            scope: &c.scope,
            source: &c.source,
            origin_ms: c.origin_ms,
            engineers: Engineers { emails: &c.engineers, names: &self.names },
            activation: mm.map_or(0.0, |x| x.activation),
            changed: mm.map_or(0, |x| x.changed),
            ignored: mm.map_or(0, |x| x.ignored),
            corrected: mm.map_or(0, |x| x.corrected),
            vetoed: mm.map_or(0, |x| x.vetoed),
            regretted: mm.map_or(0.0, |x| x.regretted),
            tested: mm.map_or(0, |x| x.tested),
            dormant: mm.is_some_and(|x| x.dormant),
            consolidated: mm.is_some_and(|x| x.consolidated),
            usage: self.usage.get(&c.id).unwrap_or(&NO_USAGE),
            version: stored.map_or(1, |s| s.0),
            repo: stored.map_or("", |s| s.1.as_str()),
            reach: &c.reach,
        }
    }
}

/// The body of `/api/lessons`.
#[derive(Serialize)]
struct LessonsBody<'a> {
    collective: &'a str,
    lessons: Vec<LessonRow<'a>>,
}

/// One lesson of `/api/lessons`, borrowed from [`LessonSources`]. The field order is the order the
/// page has always been served (the `json!` object this replaced, serde_json's `preserve_order` on).
#[derive(Serialize)]
struct LessonRow<'a> {
    id: &'a str,
    question: &'a str,
    situations: &'a [String],
    consequence: &'a str,
    area: &'a str,
    scope: &'a [String],
    source: &'a str,
    origin_ms: i64,
    engineers: Engineers<'a>,
    activation: f64,
    changed: u32,
    ignored: u32,
    corrected: u32,
    vetoed: u32,
    regretted: f64,
    tested: u32,
    dormant: bool,
    consolidated: bool,
    usage: &'a BTreeMap<String, u64>,
    version: u32,
    repo: &'a str,
    reach: &'a str,
}

/// A lesson's engineers for display ([`Book::display`]), named as they are written out: no list is built.
struct Engineers<'a> {
    emails: &'a [String],
    names: &'a Book,
}

impl Serialize for Engineers<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.emails.iter().map(|e| self.names.display(e)))
    }
}

#[derive(Deserialize)]
struct MemoryQuery {
    c: Option<String>,
    /// Only the absorbed lessons (`borg memory export --absorbed`).
    #[serde(default)]
    absorbed: bool,
}

/// The collective's evidence records, exactly what `borg memory export --json` prints (borg
/// memory_export.rs `document`): each lesson's memory, breadth and, when an outside store holds it, its
/// absorb and recurrence. The Knowledge view marks absorbed lessons from it.
async fn memory_of(State(app): State<Arc<App>>, Query(q): Query<MemoryQuery>) -> Response {
    let Some(m) = app.mind(q.c.as_deref()) else { return not_found(q.c.as_deref().unwrap_or_default()) };
    let filter = memory_export::Filter { absorbed: q.absorbed, ..Default::default() };
    Json(memory_export::document(&m.name, &m.home, &filter)).into_response()
}

/// What the feed shows: what people and drones did, not the routine of every turn end.
const NOTABLE: [&str; 30] = [
    stats::CORRECTION,
    stats::VETO,
    stats::VETO_REPEAT,
    stats::REVIEW_FINDING,
    stats::BLOCK,
    stats::CHANGED,
    stats::IGNORED,
    stats::CORRECTED,
    stats::REGRETTED,
    stats::TESTED,
    stats::LESSON_NEW,
    stats::LESSON_MERGED,
    stats::NOT_A_LESSON,
    stats::STEER_ISSUED,
    stats::STEER_DELIVERED,
    stats::STEER_DROPPED,
    stats::PAUSE_REFUSED,
    stats::HANDOFF,
    stats::HANDOFF_REFUSED,
    stats::QUEEN_QUEUED,
    stats::QUEEN_DELIVERED,
    stats::QUEEN_ANSWERED,
    stats::QUEEN_SKIPPED,
    stats::QUEEN_DISMISSED,
    stats::ASSIMILATED,
    stats::EARLY,
    stats::QUEUED_FREE,
    stats::MODEL_CALL,
    // Messages between sessions: each one sent, and each refused. A delivery is one row per recipient
    // (a broadcast many): `/api/messages` says whom each was shown to instead.
    stats::MSG_SENT,
    stats::MSG_DROPPED,
];

#[derive(Deserialize)]
struct FeedQuery {
    c: Option<String>,
    limit: Option<usize>,
}

/// Display: the rows a listing (`/api/feed`, `/api/messages`) returns when the page sets no `limit`.
const PAGE: usize = 200;
/// Guard: the most rows one listing returns whatever its `limit`, so one request cannot make the
/// server serialise a whole log.
const PAGE_MAX: usize = 2000;

/// This machine's notable events, newest first (peers' activity arrives as counts only).
async fn feed(State(app): State<Arc<App>>, Query(q): Query<FeedQuery>) -> Response {
    let Some(m) = app.mind(q.c.as_deref()) else { return not_found(q.c.as_deref().unwrap_or_default()) };
    stats::backfill(&m.home);
    let names = Book::load(&m.home, learn::now_ms());
    let out: Vec<Value> = stats::newest(&m.home, &NOTABLE, q.limit.unwrap_or(PAGE).min(PAGE_MAX))
        .into_iter()
        .map(|e| {
            json!({"ms": e.ms, "kind": e.kind, "engineer": names.display(&e.engineer), "repo": e.repo, "lessons": e.lessons,
                "n": e.n, "usd": e.usd, "dur_ms": e.dur_ms, "why": e.why, "text": e.text, "source": e.source})
        })
        .collect();
    Json(json!({"collective": m.name, "events": out})).into_response()
}

async fn queen(State(app): State<Arc<App>>, Query(p): Query<Pick>) -> Response {
    let Some(m) = app.mind(p.c.as_deref()) else { return not_found(p.c.as_deref().unwrap_or_default()) };
    let mut entries = borg::queen::entries(&m.home);
    entries.sort_by_key(|e| std::cmp::Reverse(e.ms));
    // Engineers by name (names.rs), who asked and who answered.
    let names = Book::load(&m.home, learn::now_ms());
    for e in &mut entries {
        e.engineer = names.display(&e.engineer);
        e.answered_by = e.answered_by.as_deref().map(|a| names.display(a));
    }
    // The queen's attention reward (escalation.rs), the team's: folded from every stats bucket.
    let attention = borg::escalation::team(&m.home);
    Json(json!({"collective": m.name, "entries": entries, "attention": attention})).into_response()
}

async fn drones(State(app): State<Arc<App>>, Query(p): Query<Pick>) -> Response {
    let Some(m) = app.mind(p.c.as_deref()) else { return not_found(p.c.as_deref().unwrap_or_default()) };
    let now = learn::now_ms();
    let (mut ds, names) = top::named(&m.home, m.share == Share::Team, now);
    for d in &mut ds {
        d.p.engineer = names.display(&d.p.engineer);
    }
    let inbox = top::inbox(&m.home, &ds, names, now);
    Json(top::to_json_with(&m.name, m.share, &ds, &inbox)).into_response()
}

/// Hand-offs (borg handoff `board`): every one this node knows, with its state, whom it asks, who took
/// or declined it and which engineers have a machine that could take it, and every machine the presence
/// shows with its borg and clones ("who can take what"). Engineers show by name (names.rs), whom an
/// offer asks too, by name or as its machine. No remote is
/// asked: what the store holds.
async fn handoffs(State(app): State<Arc<App>>, Query(p): Query<Pick>) -> Response {
    let Some(m) = app.mind(p.c.as_deref()) else { return not_found(p.c.as_deref().unwrap_or_default()) };
    let now = learn::now_ms();
    let mut v = borg::handoff::board(&m.home, now);
    let names = Book::load(&m.home, now);
    let show = |x: &mut Value| {
        if let Some(e) = x.as_str().map(|e| names.display(e)) {
            *x = json!(e);
        }
    };
    for h in v["handoffs"].as_array_mut().into_iter().flatten() {
        show(&mut h["engineer"]);
        // An ask names an engineer by email or a machine by its key.
        if let Some(to) = h["to"].as_str().map(|t| if t.contains('@') { names.display(t) } else { names.host(t) }) {
            h["to"] = json!(to);
        }
        if let Some(by) = h.get_mut("by").filter(|b| b.is_object()) {
            show(&mut by["engineer"]);
        }
        h["takers"].as_array_mut().into_iter().flatten().for_each(&show);
        h["declined"].as_array_mut().into_iter().flatten().for_each(|d| show(&mut d["engineer"]));
    }
    for x in v["machines"].as_array_mut().into_iter().flatten() {
        x["engineers"].as_array_mut().into_iter().flatten().for_each(&show);
    }
    v["collective"] = json!(m.name);
    Json(v).into_response()
}

/// Messages between sessions (borg msg.rs) on this node, newest first: who sent each to whom, as `borg
/// top` names them (a sender as its drone is labelled now, from another node with that node's machine:
/// the one part the mesh checked), what the recipient is (`to_kind`: `all` for a broadcast, `local` or `collective`,
/// `engineer` or `session`, from msg.rs `Address`; `to_name`, top.rs `recipient`, tells them apart),
/// and how many sessions here it was shown to. Listing acknowledges nothing: the page fetches every
/// view whichever is open, so the messages view says what it showed (`messages_seen`).
/// `from_key`/`to_key` name each end as `<session>@<node>` (a session `to` without a node takes its
/// drone's) or `engineer` for any engineer address, so a tell to `engineer` and an `engineer:<email>`
/// reply meet, and `conversation` is the broadcast address or both keys sorted and space-joined, so
/// A→B and B→A share one; `mine` is a line sent from this node. `status` is `broadcast`, `engineer`,
/// `read`/`unread`/`missed` for a session here (shown, waiting on a live one, never shown and ended or
/// unknown) or `elsewhere` for one on another node. `from_*`/`to_*` carry each end's drone context as
/// borg top lists it (repo, engineer by name, state), empty when it lists none. `unread` is how many
/// running sessions here it still waits for (a broadcast may wait for several); the rows sum to the
/// top-level `unread`, the Unread now tile.
async fn messages(State(app): State<Arc<App>>, Query(q): Query<FeedQuery>) -> Response {
    let Some(m) = app.mind(q.c.as_deref()) else { return not_found(q.c.as_deref().unwrap_or_default()) };
    let now = learn::now_ms();
    let (ds, names) = top::named(&m.home, m.share == Share::Team, now);
    let shown = msg::seen_by(&m.home);
    let here = msg::node(&m.home);
    let mut all: Vec<msg::Message> = msg::read(&m.home).into_iter().filter(|x| now - x.ms <= msg::TTL_MS).collect();
    // Per message, the live sessions here it still waits for (`unread_of` per message, from the same
    // seen records): a broadcast unread by eight sessions counts eight, so the rows sum to `unread`.
    let mut waiting: HashMap<String, usize> = HashMap::new();
    for d in ds.iter().filter(|d| d.node.is_empty() && d.state != presence::State::Ended) {
        let session = presence::safe(&d.p.session);
        for x in msg::for_session(&all, &here, &d.p.session, d.p.started, now) {
            if !shown.get(&presence::safe(&x.id)).is_some_and(|v| v.contains(&session)) {
                *waiting.entry(x.id.clone()).or_default() += 1;
            }
        }
    }
    let unread: usize = waiting.values().sum();
    all.sort_by(|a, b| (b.ms, &b.id).cmp(&(a.ms, &a.id)));
    all.truncate(q.limit.unwrap_or(PAGE).min(PAGE_MAX));
    /// A drone's (repo, engineer by name, state) as a row lists them; empty strings without one.
    fn context<'a>(d: Option<&'a top::Drone>, names: &Book) -> (&'a str, String, &'static str) {
        d.map_or(("", String::new(), ""), |d| (d.p.repo.as_str(), names.display(&d.p.engineer), d.state.name()))
    }
    let out: Vec<Value> = all
        .iter()
        .map(|x| {
            let to = shown.get(&presence::safe(&x.id)).map_or(0, Vec::len);
            let waits = waiting.get(&x.id).copied().unwrap_or(0);
            let addr = msg::Address::parse(&x.to);
            let (sd, rd) = (top::sender_drone(&ds, &here, x), top::recipient_drone(&ds, &here, &x.to));
            let broadcast = addr.as_ref().is_some_and(msg::Address::broadcast);
            let engineer = matches!(addr, Some(msg::Address::Engineer { .. }));
            let to_kind = if broadcast {
                "all"
            } else if engineer {
                "engineer"
            } else {
                "session"
            };
            let from_key: Cow<str> = match msg::Address::parse(&x.by) {
                Some(msg::Address::Engineer { .. }) => Cow::Borrowed(msg::ENGINEER),
                _ => Cow::Owned(format!("{}@{}", x.by, x.node)),
            };
            // A session named without its node keys as the drone it resolves to, so A→B and B→A meet.
            let to_key: Cow<str> = match (&addr, rd) {
                (Some(msg::Address::Engineer { .. }), _) => Cow::Borrowed(msg::ENGINEER),
                (Some(msg::Address::Session { node: None, .. }), Some(d)) => {
                    Cow::Owned(format!("{}@{}", d.p.session, if d.node.is_empty() { here.as_str() } else { d.node.as_str() }))
                }
                _ => Cow::Borrowed(x.to.as_str()),
            };
            let conversation: Cow<str> = if broadcast {
                Cow::Borrowed(x.to.as_str())
            } else {
                let mut ends = [&*from_key, &*to_key];
                ends.sort_unstable();
                Cow::Owned(ends.join(" "))
            };
            let local = rd.is_some_and(|d| d.node.is_empty()) || matches!(addr, Some(msg::Address::Session { node: Some(n), .. }) if n == here);
            let status = if broadcast {
                "broadcast"
            } else if engineer {
                "engineer"
            } else if !local {
                "elsewhere"
            } else if to > 0 {
                "read"
            } else if rd.is_some_and(|d| d.node.is_empty() && d.state != presence::State::Ended) {
                "unread"
            } else {
                "missed"
            };
            let ((from_repo, from_engineer, from_state), (to_repo, to_engineer, to_state)) = (context(sd, &names), context(rd, &names));
            json!({"id": x.id, "kind": x.kind, "by": x.by, "from": top::sender(&ds, &names, &here, x), "harness": x.harness, "node": x.node,
                "to": x.to, "to_name": top::recipient(&ds, &names, &here, &x.to), "to_kind": to_kind, "re": x.re, "ms": x.ms, "text": x.text, "shown_to": to, "unread": waits,
                "mine": x.node == here, "from_key": from_key, "to_key": to_key, "conversation": conversation, "status": status,
                "from_repo": from_repo, "from_engineer": from_engineer, "from_state": from_state,
                "to_repo": to_repo, "to_engineer": to_engineer, "to_state": to_state})
        })
        .collect();
    Json(json!({"collective": m.name, "messages": out, "unread": unread})).into_response()
}

#[derive(Deserialize)]
struct Shown {
    ids: Vec<String>,
}

/// The page's messages view showed the engineer the messages `ids` names: those to the engineer among
/// them are acknowledged as the engineer of the start directory (borg msg.rs `ack_engineer`), as `borg
/// inbox` and `borg top` acknowledge what they show; any other id is ignored. Only the page itself can
/// call it: a JSON body needs a CORS preflight from any other site's page, which this server never
/// answers, and the Host check (`loopback_only`) keeps a rebound name out. At most `PAGE_MAX` ids count.
async fn messages_seen(State(app): State<Arc<App>>, Query(p): Query<Pick>, Json(shown): Json<Shown>) -> Response {
    let Some(m) = app.mind(p.c.as_deref()) else { return not_found(p.c.as_deref().unwrap_or_default()) };
    let now = learn::now_ms();
    let ids: std::collections::HashSet<&str> = shown.ids.iter().take(PAGE_MAX).map(String::as_str).collect();
    let all: Vec<msg::Message> = msg::read(&m.home).into_iter().filter(|x| now - x.ms <= msg::TTL_MS && ids.contains(x.id.as_str())).collect();
    let shown: Vec<&msg::Message> = all.iter().collect();
    match msg::ack_engineer(&m.home, &app.cwd, &shown, now) {
        Ok(()) => Json(json!({"collective": m.name})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("borg web: acknowledging: {e}")).into_response(),
    }
}

/// Lesson selection (borg select.rs) as `borg status` and `borg top` show it, in full (top.rs
/// `Selection`): what decides and the gate's budget, deviations from the fixed rule, spend per cost
/// source, effects with intervals, label and evaluation health, pool yield, latency against the stop
/// budget. Read from the store's selection summary, never recomputed here.
async fn select_of(State(app): State<Arc<App>>, Query(p): Query<Pick>) -> Response {
    let Some(m) = app.mind(p.c.as_deref()) else { return not_found(p.c.as_deref().unwrap_or_default()) };
    let s = top::selection(&m.home.join("lessons.jsonl"), borg::select::policy());
    Json(json!({"collective": m.name, "selection": s})).into_response()
}

/// Whether learning works in the collective, week by week (learning.rs), backfilled from its logs.
async fn learning_of(State(app): State<Arc<App>>, Query(p): Query<Pick>) -> Response {
    let Some(m) = app.mind(p.c.as_deref()) else { return not_found(p.c.as_deref().unwrap_or_default()) };
    Json(learning::view(&m.name, &m.home, m.share)).into_response()
}

/// What `/api/update` serves for the collective `name` at `home`: this borg (`borg`, its mesh protocol
/// `protocol`), the last release check (`latest`, `checked_ms`, `error`; all null before the first) and
/// whether its release is newer (`update`, false while `check` is off: `BORG_UPDATE_CHECK=off`), then
/// the members this node knows of. Each member has its `key`, its `engineer` when known (by name,
/// names.rs) and `name` (the engineer, else its short key), `host` (its machine, else its short key), `borg` (null: older than 0.7, it sent none), the `protocol` its last
/// session agreed on, `syncs` (false: nothing syncs with it, it speaks a protocol this node does not)
/// and `note`: what it misses on an older protocol (protocol.rs `NOTES`), or why nothing syncs; null
/// on this node's footing. Without `mesh/` there are no members.
pub fn update_view(name: &str, home: &Path) -> Value {
    let checked = update::checked_at(home);
    let latest = checked.as_ref().and_then(|c| c.latest.clone());
    let on = update::on();
    let newer = on && latest.as_deref().is_some_and(|l| update::newer(l, update::VERSION));
    let ours = protocol::current();
    let names = Book::load(home, learn::now_ms());
    let versions = protocol::versions(home);
    let mismatches = protocol::mismatches(home);
    let engineer = |v: Option<&protocol::PeerVersion>| v.and_then(|v| v.engineer.as_deref()).filter(|e| !e.is_empty()).map(|e| names.display(e));
    let named = |key: &str, e: &Option<String>| e.clone().unwrap_or_else(|| protocol::member(key, None));
    // A member nothing syncs with is told by its mismatch, the newer state: its version record is from
    // the last session that completed.
    let mut members: Vec<Value> = versions
        .iter()
        .filter(|(key, _)| !mismatches.contains_key(*key))
        .map(|(key, v)| {
            let missed = protocol::misses(v.protocol, ours);
            let e = engineer(Some(v));
            json!({"key": key, "engineer": e, "name": named(key, &e), "host": names.host(key), "borg": v.borg, "protocol": v.protocol, "ms": v.ms, "syncs": true,
                "note": (!missed.is_empty()).then_some(missed)})
        })
        .collect();
    members.extend(mismatches.iter().map(|(key, m)| {
        let v = versions.get(key);
        let e = engineer(v);
        json!({"key": key, "engineer": e, "name": named(key, &e), "host": names.host(key), "borg": v.and_then(|v| v.borg.clone()), "protocol": protocol::number(&m.theirs),
            "ms": m.ms, "syncs": false, "note": m.line(key)})
    }));
    json!({
        "collective": name, "borg": update::VERSION, "protocol": ours, "check": on,
        "latest": latest, "checked_ms": checked.as_ref().map(|c| c.checked_ms), "error": checked.and_then(|c| c.error),
        "update": newer, "members": members,
    })
}

async fn update_of(State(app): State<Arc<App>>, Query(p): Query<Pick>) -> Response {
    let Some(m) = app.mind(p.c.as_deref()) else { return not_found(p.c.as_deref().unwrap_or_default()) };
    Json(update_view(&m.name, &m.home)).into_response()
}

#[derive(Deserialize)]
struct MeshQuery {
    c: Option<String>,
    since: Option<i64>,
}

/// The Cube view (mesh.rs): machines, links, drones and what moved between them; with `since` (the
/// `now` of the last response), only what this node got after it.
async fn mesh_of(State(app): State<Arc<App>>, Query(q): Query<MeshQuery>) -> Response {
    let Some(m) = app.mind(q.c.as_deref()) else { return not_found(q.c.as_deref().unwrap_or_default()) };
    let body = mesh::view(&m.name, &m.home, m.share, q.since, learn::now_ms());
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
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
        // The evidence records fold the lessons, the outcomes, this machine's episodes and the absorb log.
        (
            "memory",
            vec![home.join("lessons.jsonl"), home.join("outcomes.jsonl"), home.join(outcome::TEAM_FILE), home.join("episodes.jsonl"), absorb::path(home)],
        ),
        ("stats", vec![home.join("stats.jsonl"), stats::events_path(home)]),
        // A drone's unread count moves with the log and the shown-once claims.
        ("drones", vec![home.join("presence"), presence::remote_path(home), msg::path(home), msg::seen_path(home)]),
        ("messages", vec![msg::path(home), msg::seen_path(home)]),
        // The hand-off log, the peers' lines derived from the messages, the remote's answers and the
        // fences (borg handoff/), and the presence that says who can take what.
        ("handoffs", vec![home.join("handoffs.jsonl"), home.join("handoff"), home.join("presence"), presence::remote_path(home)]),
        ("feed", vec![stats::events_path(home)]),
        ("queen", vec![home.join("queen"), home.join("stats.jsonl"), stats::events_path(home)]),
        // Learning health folds the lessons, the rollups and the local events.
        ("learning", vec![home.join("lessons.jsonl"), home.join("stats.jsonl"), stats::events_path(home)]),
        // The selection summary's cache, and the outcome log whose tail it folds on every read; the pool
        // log its miss causes fold (borg pools.rs).
        ("select", vec![home.join(borg::select::summary::CACHE), home.join("outcomes.jsonl"), borg::pools::path(home)]),
        // The Cube (mesh.rs): the messages, who was shown them and the hand-offs, the lessons and who
        // stamped them, the absorbs, this node's sessions, the drones and its links. Not the stats event
        // log, which changes on every hook and moves none of it.
        (
            "mesh",
            vec![
                msg::path(home),
                home.join("lessons.jsonl"),
                borg::mesh::origins_path(home),
                absorb::path(home),
                borg::mesh::sessions_path(home),
                home.join("presence"),
                presence::remote_path(home),
                presence::synced_path(home),
                home.join("mesh").join("peers.json"),
                protocol::versions_path(home),
                protocol::protocols_path(home),
            ],
        ),
        // The release check and what the mesh last heard of the members' versions.
        ("update", vec![update::path_for(home), protocol::versions_path(home), protocol::protocols_path(home)]),
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
        .route("/api/memory", get(memory_of))
        .route("/api/drones", get(drones))
        .route("/api/feed", get(feed))
        .route("/api/queen", get(queen))
        .route("/api/messages", get(messages))
        .route("/api/messages/seen", post(messages_seen))
        .route("/api/handoffs", get(handoffs))
        .route("/api/learning", get(learning_of))
        .route("/api/select", get(select_of))
        .route("/api/update", get(update_of))
        .route("/api/mesh", get(mesh_of))
        .route("/api/events", get(events))
        .layer(axum::middleware::from_fn(loopback_only))
        .with_state(app)
}

/// Answers only a request whose Host names this machine's loopback. The 127.0.0.1 bind keeps other
/// machines out but not a page in the engineer's browser whose name re-resolves to 127.0.0.1 (DNS
/// rebinding): to the browser the port is then that page's own origin, CORS does not apply, and its
/// script could read the lessons, messages and spend. Such a request carries that name.
async fn loopback_only(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    match req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).is_some_and(loopback) {
        true => next.run(req).await,
        false => (StatusCode::FORBIDDEN, "borg web: served to a loopback Host only (127.0.0.1, localhost, [::1])\n").into_response(),
    }
}

/// `127.0.0.1`, `localhost` or `[::1]`, with a numeric port or none.
fn loopback(host: &str) -> bool {
    let (name, port) = match host.strip_prefix('[') {
        Some(v6) => match v6.split_once(']') {
            Some(("::1", port)) => ("::1", port),
            _ => return false,
        },
        None => host.find(':').map_or((host, ""), |i| host.split_at(i)),
    };
    let port = port.is_empty() || port.strip_prefix(':').is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    port && (name == "::1" || name == "127.0.0.1" || name.eq_ignore_ascii_case("localhost"))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `/api/lessons` as it was built before [`LessonRow`]: a `json!` object per lesson, sorted as values.
    /// The reference the borrowed rows must match byte for byte.
    fn lessons_by_value(collective: &str, s: &LessonSources) -> Value {
        let LessonSources { list, mem, usage, names, stored } = s;
        let mut out: Vec<Value> = list
            .iter()
            .map(|c| {
                let mm = mem.get(&c.id);
                json!({
                    "id": c.id, "question": c.question, "situations": c.situations, "consequence": c.consequence,
                    "area": c.area, "scope": c.scope, "source": c.source, "origin_ms": c.origin_ms,
                    "engineers": c.engineers.iter().map(|e| names.display(e)).collect::<Vec<_>>(),
                    "activation": mm.map_or(0.0, |x| x.activation), "changed": mm.map_or(0, |x| x.changed),
                    "ignored": mm.map_or(0, |x| x.ignored), "corrected": mm.map_or(0, |x| x.corrected), "vetoed": mm.map_or(0, |x| x.vetoed),
                    "regretted": mm.map_or(0.0, |x| x.regretted),
                    "tested": mm.map_or(0, |x| x.tested),
                    "dormant": mm.is_some_and(|x| x.dormant), "consolidated": mm.is_some_and(|x| x.consolidated),
                    "usage": usage.get(&c.id).cloned().unwrap_or_default(),
                    "version": stored.get(&c.id).map_or(1, |s| s.0), "repo": stored.get(&c.id).map(|s| s.1.clone()).unwrap_or_default(),
                    "reach": c.reach,
                })
            })
            .collect();
        out.sort_by(|a, b| b["activation"].as_f64().partial_cmp(&a["activation"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
        json!({"collective": collective, "lessons": out})
    }

    /// The borrowed rows serialise to the very bytes the `json!` values did, on a store whose lessons
    /// differ in every field: outcomes that order them by activation (ties among the unused ones), a
    /// second version in another repo, usage from a peer's rollup, named and unnamed engineers.
    #[test]
    fn the_lessons_view_serialises_as_the_json_values_did() {
        let home = std::env::temp_dir().join(format!("borg-web-lessons-bytes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let lesson = |id: &str, version: u32, repo: &str| learn::Lesson {
            id: id.into(),
            origin_ms: 1_000 + i64::from(version),
            repo: repo.into(),
            question: format!("Does {id} \"quote\" its inputs?"),
            version,
            ..Default::default()
        };
        let full = learn::Lesson {
            situations: vec!["a merge".into(), "a rebase".into()],
            consequence: "lost work".into(),
            area: "git".into(),
            engineers: vec!["ann@example.com".into(), "bo@example.com".into()],
            source: "queen".into(),
            scope: vec!["api".into()],
            reach: "repo".into(),
            ..lesson("L1", 1, "app")
        };
        let lines = [full, lesson("L2", 1, "app"), lesson("L2", 2, "web"), lesson("L3", 1, ""), lesson("L4", 1, "app"), lesson("L5", 1, "app")];
        std::fs::write(home.join("lessons.jsonl"), lines.iter().map(|l| serde_json::to_string(l).unwrap() + "\n").collect::<String>()).unwrap();
        let o = |kind: &str, session: &str, id: &str, ms: i64| outcome::Outcome {
            kind: kind.into(),
            session: session.into(),
            lessons: vec![id.into()],
            ms,
            ..Default::default()
        };
        for x in [
            o(outcome::ASKED, "s1", "L4", 1),
            o(outcome::CHANGED, "s1", "L4", 2),
            o(outcome::ASKED, "s2", "L2", 3),
            o(outcome::IGNORED, "s2", "L2", 4),
            o(outcome::ASKED, "s3", "L5", 5),
            o(outcome::CORRECTED, "s3", "L5", 6),
        ] {
            outcome::append(&home.join("outcomes.jsonl"), &x).unwrap();
        }
        let peer = json!({"id": "stat:peer:b@x:2026-10-13", "version": 1, "day": "2026-10-13", "node": "peer", "engineer": "b@x",
            "counts": {"asked": 3}, "usd": 0.0, "dur_ms": 0, "lessons": {"L1": {"asked": 2, "block": 1}, "L4": {"asked": 1}}});
        std::fs::write(home.join("stats.jsonl"), format!("{peer}\n")).unwrap();
        // No derived history: the usage is the peer's alone, and L2 has none.
        std::fs::create_dir_all(home.join("stats")).unwrap();
        std::fs::write(home.join("stats/backfill.jsonl"), "").unwrap();

        let mut s = LessonSources::load(&home);
        s.names = Book::from_parts([("ann@example.com".to_string(), "Ann".to_string())].into(), &[]);
        let want = serde_json::to_string(&lessons_by_value("corp", &s)).unwrap();
        let got = serde_json::to_string(&LessonsBody { collective: "corp", lessons: s.rows() }).unwrap();
        assert_eq!(got, want);
        // The store exercised what the rows carry: the sort moved lessons, and every borrowed field shows.
        let v: Value = serde_json::from_str(&got).unwrap();
        let ids: Vec<&str> = v["lessons"].as_array().unwrap().iter().filter_map(|l| l["id"].as_str()).collect();
        assert_ne!(ids, ["L1", "L2", "L3", "L4", "L5"], "{v}");
        let l1 = v["lessons"].as_array().unwrap().iter().find(|l| l["id"] == "L1").unwrap();
        let l2 = v["lessons"].as_array().unwrap().iter().find(|l| l["id"] == "L2").unwrap();
        assert_eq!((&l1["engineers"], &l1["usage"], &l1["reach"]), (&json!(["Ann", "bo@example.com"]), &json!({"asked": 2, "block": 1}), &json!("repo")));
        assert_eq!((&l2["version"], &l2["repo"], &l2["usage"]), (&json!(2), &json!("web"), &json!({})));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The UI (a WASM crate of its own, borg-web/ui) matches the detached check's protocol words by
    /// their text: its feed every `why` of a check not run, its cost view the stats mode.
    #[test]
    fn the_ui_matches_the_detached_checks_words() {
        let feed = include_str!("../ui/src/feed.rs");
        assert!(feed.contains(&format!("\"{}\" if e.why", borg::stats::CHECK_DROPPED)), "the feed's event kind");
        // `superseded` is the feed's last arm for the kind, matched by no word.
        for why in borg::check_async::WHYS.iter().filter(|w| **w != borg::check_async::WHY_SUPERSEDED) {
            assert!(feed.contains(&format!("e.why == \"{why}\"")), "the feed matches {why}");
        }
        let cost = include_str!("../ui/src/views/cost.rs");
        assert!(cost.contains(&format!("\"{}\"", borg::check_async::MODE)), "the cost view's mode");
    }

    /// Two engineers who share a git user.name stay two rows, told apart by their emails; the emails of
    /// one engineer the alias config joins (or one email in two cases) are one row.
    #[test]
    fn engineers_who_share_a_name_stay_apart() {
        let bucket =
            |engineer: &str, turns: u64| stats::Bucket { engineer: engineer.into(), counts: [("turn_end".to_string(), turns)].into(), ..Default::default() };
        let buckets = [
            bucket("david.a@example.com", 1),
            bucket("david.b@example.com", 2),
            bucket("ann@example.com", 4),
            bucket("Ann@home.example", 8),
            bucket("ann@home.example", 16),
        ];
        let names =
            Book::from_parts([("david.a@example.com", "David"), ("david.b@example.com", "David")].map(|(e, n)| (e.to_string(), n.to_string())).into(), &[]);
        let aliases =
            collective::Aliases::new(&[collective::Engineer { name: "Ann".into(), emails: vec!["ann@example.com".into(), "ann@home.example".into()] }]);
        let rows: Vec<(String, u64, usize)> =
            engineer_rows(&buckets, &names, &aliases).into_iter().map(|(e, t, mine)| (e, t.counts["turn_end"], mine.len())).collect();
        assert_eq!(rows, [("Ann".into(), 28, 3), ("David (david.a@example.com)".into(), 1, 1), ("David (david.b@example.com)".into(), 2, 1)]);
    }

    /// The page refetches the selection when its summary cache, the outcome log the summary folds, or
    /// the pool log changes; the cache and the pool log alone change no other view.
    #[test]
    fn the_selection_view_follows_its_summary_and_the_outcomes() {
        let home = std::env::temp_dir().join(format!("borg-web-watch-select-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let changed = |a: &Fingerprint, b: &Fingerprint| a.iter().zip(b).filter(|(x, y)| x != y).map(|(x, _)| x.0).collect::<Vec<_>>();
        let before = fingerprint(&home);
        std::fs::write(home.join(borg::select::summary::CACHE), "{\"sv\":1,\"at\":0}").unwrap();
        let cached = fingerprint(&home);
        assert_eq!(changed(&before, &cached), ["select"]);
        std::fs::write(home.join("outcomes.jsonl"), "{}\n").unwrap();
        let logged = fingerprint(&home);
        assert!(changed(&cached, &logged).contains(&"select"));
        // The pool log its miss causes fold.
        std::fs::write(borg::pools::path(&home), "{}\n").unwrap();
        assert_eq!(changed(&logged, &fingerprint(&home)), ["select"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A release check changes the update view only; a member's version also the Cube, which shows
    /// its machine's version.
    #[test]
    fn the_update_view_follows_the_release_check_and_the_members_versions() {
        let home = std::env::temp_dir().join(format!("borg-web-watch-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let changed = |a: &Fingerprint, b: &Fingerprint| a.iter().zip(b).filter(|(x, y)| x != y).map(|(x, _)| x.0).collect::<Vec<_>>();
        let before = fingerprint(&home);
        std::fs::write(update::path_for(&home), "{\"checked_ms\":1,\"latest\":\"0.7.0\",\"error\":null}").unwrap();
        let checked = fingerprint(&home);
        assert_eq!(changed(&before, &checked), ["update"]);
        let v = protocol::PeerVersion { borg: Some("0.7.0".into()), protocol: protocol::current(), ms: 1, engineer: None };
        protocol::note_version(&home, "k", &v).unwrap();
        assert_eq!(changed(&checked, &fingerprint(&home)), ["mesh", "update"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// An absorb or release changes the memory view and the Cube, which plays it: the lessons
    /// themselves did not change.
    #[test]
    fn the_memory_view_follows_the_absorb_log() {
        let home = std::env::temp_dir().join(format!("borg-web-watch-memory-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let changed = |a: &Fingerprint, b: &Fingerprint| a.iter().zip(b).filter(|(x, y)| x != y).map(|(x, _)| x.0).collect::<Vec<_>>();
        let before = fingerprint(&home);
        std::fs::write(absorb::path(&home), "{\"id\":\"L1\",\"kind\":\"absorb\",\"ms\":1,\"by\":\"a@x\"}\n").unwrap();
        assert_eq!(changed(&before, &fingerprint(&home)), ["memory", "mesh"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The Cube follows who stamped the synced lines and this node's mesh sessions, which no other view
    /// shows; the stats event log, which every hook appends to, changes the views that count it and not
    /// the Cube.
    #[test]
    fn the_cube_follows_the_origins_and_the_sessions_and_not_the_event_log() {
        let home = std::env::temp_dir().join(format!("borg-web-watch-mesh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("mesh")).unwrap();
        let changed = |a: &Fingerprint, b: &Fingerprint| a.iter().zip(b).filter(|(x, y)| x != y).map(|(x, _)| x.0).collect::<Vec<_>>();
        let before = fingerprint(&home);
        std::fs::write(borg::mesh::origins_path(&home), "{\"h\":\"00\",\"key\":\"k\",\"ms\":1,\"sig\":\"00\"}\n").unwrap();
        let stamped = fingerprint(&home);
        assert_eq!(changed(&before, &stamped), ["mesh"]);
        let s = borg::mesh::Session { ms: 1, peer: "k".into(), dial: true, sent: 1, received: 0, error: None };
        borg::mesh::note(&home, &s, 1).unwrap();
        let logged = fingerprint(&home);
        assert_eq!(changed(&stamped, &logged), ["mesh"]);
        std::fs::create_dir_all(home.join("stats")).unwrap();
        std::fs::write(stats::events_path(&home), "{}\n").unwrap();
        let counted = changed(&logged, &fingerprint(&home));
        assert!(!counted.is_empty() && !counted.contains(&"mesh"), "{counted:?}");
        let _ = std::fs::remove_dir_all(&home);
    }
}
