//! What Borg did, counted: the collective's usage statistics.
//!
//! Two files per collective store:
//! - `stats/events.jsonl` (local, never synced): one line per event, written by the hook or command
//!   where it happened (`record`). It holds session ids and repos, so it stays on this machine.
//! - `stats.jsonl` (synced by the mesh, `borg-mesh` lines.rs `SYNCED`): one rollup per (node, engineer,
//!   UTC day), folded from the local events (`rollup`). A bucket is a versioned record (`id` +
//!   `version`), so the mesh's `settle` keeps its latest version on every machine with no merge logic
//!   of its own. Only the node that owns a bucket writes it, so buckets never conflict. A rollup carries
//!   counts, cost and lesson ids, never prompts or sessions.
//!
//! Recording fails open and costs one append: a broken store never disturbs the drone.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::follow::{Follow, Held, Mark, Read, follow};
use crate::jsonl::{Scalar, Str, Text, slot};

/// A turn end the conscience saw (one per Stop that was not the rework after its own block).
pub const TURN_END: &str = "turn_end";
/// The free gate skipped the model call (`why`: the score, or `noedit`).
pub const GATE_SKIP: &str = "gate_skip";
/// One turn-end model call (`n`: lessons shown, `usd`, `dur_ms`; `why: "failed"` when it gave no answer):
/// a stop's own, or a detached check's (`mode` `async`, with `rss_kb` and `limit_ms`, check_async.rs).
pub const CHECK: &str = "check";
/// A detached check that never called the model (check_async.rs `not_run`; `why`: `superseded`, a newer
/// stop's job replaced it while another check ran; `gone`, the session ended before it ran). Counted
/// apart from `check`, which counts model calls.
pub const CHECK_DROPPED: &str = "check_dropped";
/// A block: one extra round of agent work (`n`: questions, `lessons`: the asked ids).
pub const BLOCK: &str = "block";
/// Live mode, before 0.7.6 (removed; history only): one model call during the turn (`n`, `usd`,
/// `dur_ms`, `why: "failed"`, as `check`).
pub const LIVE_CHECK: &str = "live_check";
/// Grade-3 lessons a check asked, delivered with the next prompt or tool call (`n`, `lessons`): a
/// detached check's (check_async.rs, since 0.7.7), or a live check's before 0.7.6. Never an extra round:
/// the drone answers as it works.
pub const LIVE_ASK: &str = "live_ask";
/// Lessons handed over free at a prompt or touch (`n`, `lessons`).
pub const EARLY: &str = "early";
/// Grade-2 and grade-1 lessons queued to arrive free at the next prompt (`n`, `lessons`).
pub const QUEUED_FREE: &str = "queued_free";
/// Outcomes of asked lessons (outcome.rs): used, ignored, corrected anyway, not corrected.
pub const CHANGED: &str = "changed";
pub const IGNORED: &str = "ignored";
pub const CORRECTED: &str = "corrected";
pub const NOT_CORRECTED: &str = "not_corrected";
/// A later fix, or a review finding, counted against a lesson asked before the decision (outcome.rs
/// `regretted`; `lessons`: the blamed lesson).
pub const REGRETTED: &str = "regretted";
/// `borg learn` credited a lesson asked before the session's own tests went green (outcome.rs `tested`;
/// `lessons`: that lesson).
pub const TESTED: &str = "tested";
/// The engineer corrected the drone (an episode): `why` is `rejected_tool`, `pushback`, `regret` or
/// `answer` (learn.rs `ANSWER`: an answer to the drone's own question), `text` a snippet. An answer is
/// rolled up under [`ANSWERED`], never as a correction: the corrections tiles keep counting rejected
/// tool calls and pushback.
pub const CORRECTION: &str = "correction";
/// The rollup count of the answers to a drone's own questions (a `CORRECTION` event whose `why` is
/// learn.rs `ANSWER`; `fold`).
pub const ANSWERED: &str = "answer";
/// The engineer said no (`borg no`, veto.rs): `lessons` the lessons shown so far (credited `vetoed`),
/// `n` their number, `why` `cli`, `prompt` or `mesh`, `text` the reason.
pub const VETO: &str = "veto";
/// `borg learn` merged a veto into a lesson an earlier veto made: the same "no" said twice, dated when
/// that veto was said (its episode's ms), so it falls in the week of its own `veto`.
pub const VETO_REPEAT: &str = "veto_repeat";
/// A lesson written (`why`: its source, session/import/queen/veto) or merged into an existing one.
pub const LESSON_NEW: &str = "lesson_new";
pub const LESSON_MERGED: &str = "lesson_merged";
/// A review step's finding that a fix step fixed before any commit, recorded as an episode
/// (review_fix.rs; `why`: its severity, `text`: its redacted title, for this machine's feed only).
pub const REVIEW_FINDING: &str = "review_finding";
/// `borg learn` judged an episode not to be a lesson.
pub const NOT_A_LESSON: &str = "not_a_lesson";
/// A model call outside the turn end (`why`: learn, assimilate, facets, expert, judge: the session-end
/// judge, hook.rs `judge_session`, with `n` the lessons sent to the judge; `usd`, `dur_ms`).
pub const MODEL_CALL: &str = "model_call";
/// Steering: a directive issued, shown to a drone (`why`: say/pause/resume), refused on receipt
/// (`why`: the reason), and a tool call refused because the drone was paused (`why`: the tool).
pub const STEER_ISSUED: &str = "steer_issued";
pub const STEER_DELIVERED: &str = "steer_delivered";
pub const STEER_DROPPED: &str = "steer_dropped";
pub const PAUSE_REFUSED: &str = "pause_refused";
/// Messages between sessions (msg.rs): one sent (`why`: tell/ask), shown to a session (`why`: its kind),
/// and dropped (`why`: `window`, the sender's window to its recipient was full, msg.rs `window_for`;
/// events before 0.6.0 say `rate`; `bounds`, `n` lines the log's bound, msg.rs `MEMBER_BYTES`, cut).
pub const MSG_SENT: &str = "msg_sent";
pub const MSG_DELIVERED: &str = "msg_delivered";
pub const MSG_DROPPED: &str = "msg_dropped";
/// The queen's queue: a question queued, handed to a drone, answered (`dur_ms`: time to answer),
/// skipped, dismissed.
pub const QUEEN_QUEUED: &str = "queen_queued";
pub const QUEEN_DELIVERED: &str = "queen_delivered";
pub const QUEEN_ANSWERED: &str = "queen_answered";
pub const QUEEN_SKIPPED: &str = "queen_skipped";
pub const QUEEN_DISMISSED: &str = "queen_dismissed";
/// The queen's attention reward (escalation.rs), once per event: an escalation answered by changing
/// course, one answered "yes, fine", and a "no" after merge from a session never escalated (`why`: the
/// kind, or `veto`/`regret`; `text` starts with the question, episode or link id).
pub const QUEEN_NO: &str = "queen_no";
pub const QUEEN_STAMP: &str = "queen_stamp";
pub const QUEEN_MISSED: &str = "queen_missed";
/// One imported chunk of team documents (`n`: lessons, `usd`).
pub const ASSIMILATED: &str = "assimilated";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub ms: i64,
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub engineer: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lessons: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub n: u64,
    #[serde(default, skip_serializing_if = "is_zero_f")]
    pub usd: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dur_ms: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    /// A short excerpt for this machine's own feed (a correction's words). Never folded into a rollup.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// How a check ran: `async` for a detached check (check_async.rs, 0.7.7 on); before 0.7.6 the
    /// delivery mode the hook ran in (`hook` or `live`). Empty for a stop's own check and outside a
    /// check; folded into `Bucket::usd_mode`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mode: String,
    /// How an asked lesson reached the drone: `stop` (a block), `early`, `queued`, or `live` (a live
    /// check, before 0.7.6).
    /// On asks and on their results (`changed`, `ignored`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel: String,
    /// On a correction: the harness, and its version where known, whose own transcript it was read from
    /// (learn.rs `Episode::harness`; sweep.rs, `borg assimilate`), e.g. `claude 2.1.312`; empty when a
    /// hook captured it, or `borg assimilate` before 0.7.2 imported it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// On a detached check's `check` (check_async.rs): its process's peak resident memory, KiB.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rss_kb: u64,
    /// On a detached check's `check`: the limit its model call had (check_async.rs `call_limit`), ms; a
    /// failed call that took it all timed out (`async_tally`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub limit_ms: u64,
}

/// An episode's [`Event::source`]: its `harness` and `harness_version`, joined; empty for the hooks' (and
/// imports from before 0.7.2).
pub fn source(harness: &str, version: &str) -> String {
    format!("{harness} {version}").trim().to_string()
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

fn is_zero_f(v: &f64) -> bool {
    *v == 0.0
}

impl Event {
    /// An event now.
    pub fn new(kind: &str, ms: i64) -> Self {
        Event { kind: kind.into(), ms, ..Default::default() }
    }
}

/// One node's counts for one engineer on one UTC day: a line of `stats.jsonl`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Bucket {
    pub id: String,
    pub version: u64,
    pub day: String,
    pub node: String,
    pub engineer: String,
    /// Events per kind; `<kind>_n` sums their `n` (questions asked, lessons shown).
    pub counts: BTreeMap<String, u64>,
    pub usd: f64,
    /// Cost per kind (turn-end checks, other model calls, imports).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub usd_by: BTreeMap<String, f64>,
    pub dur_ms: u64,
    /// Events per kind for each lesson they named.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lessons: BTreeMap<String, BTreeMap<String, u64>>,
    /// The conscience's model cost (`check` and `live_check`) per mode: `async` for detached checks
    /// (0.7.7 on), `live` for live mode's events before 0.7.6, `hook` for every other (an event without a
    /// mode: a stop's own check). A split of part of `usd`, so never stacked with `usd_by`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub usd_mode: BTreeMap<String, f64>,
    /// Events per `<kind>@<channel>` for events that name a channel (`block@stop`, `changed@live`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub channels: BTreeMap<String, u64>,
    /// Events per `<kind>:<why>` (why a turn end passed free, why a check was dropped, failed checks,
    /// a detached check's asks) and the turn-end checks' time (`check_ms`, `check_timed`), from 0.7.9.
    /// Apart from `counts`, so a bucket an older borg published compares equal on its side (its `same`
    /// ignores this field) and two versions on one store do not republish every day by turns.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub why: BTreeMap<String, u64>,
}

impl Bucket {
    /// Equal counts, cost and lessons: nothing to publish (`id` and `version` aside).
    fn same(&self, other: &Bucket) -> bool {
        self.counts == other.counts
            && self.lessons == other.lessons
            && self.channels == other.channels
            && self.why == other.why
            && self.dur_ms == other.dur_ms
            && (self.usd - other.usd).abs() < 1e-9
    }

    /// Adds `other`'s counts (team totals across nodes and engineers).
    pub fn add(&mut self, other: &Bucket) {
        for (k, v) in &other.counts {
            *self.counts.entry(k.clone()).or_default() += v;
        }
        for (id, kinds) in &other.lessons {
            let mine = self.lessons.entry(id.clone()).or_default();
            for (k, v) in kinds {
                *mine.entry(k.clone()).or_default() += v;
            }
        }
        for (k, v) in &other.usd_by {
            *self.usd_by.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &other.usd_mode {
            *self.usd_mode.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &other.channels {
            *self.channels.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &other.why {
            *self.why.entry(k.clone()).or_default() += v;
        }
        self.usd += other.usd;
        self.dur_ms += other.dur_ms;
    }
}

pub fn events_path(home: &Path) -> PathBuf {
    home.join("stats").join("events.jsonl")
}

pub fn rollups_path(home: &Path) -> PathBuf {
    home.join("stats.jsonl")
}

/// Appends `ev` to the local event log. Fails open; records nothing with BORG_LEARN=off (eval drones).
pub fn record(home: &Path, ev: &Event) {
    if !crate::learn::learning() {
        return;
    }
    let _ = crate::learn::append_jsonl(&events_path(home), ev);
}

/// Records one event in the store `home` now; `fill` adds the details. The engineer defaults to the
/// one presence.rs recorded for the session, so callers need not ask git again.
pub fn count(home: &Path, kind: &str, session: &str, fill: impl FnOnce(&mut Event)) {
    let mut ev = Event::new(kind, crate::learn::now_ms());
    ev.session = session.into();
    fill(&mut ev);
    if ev.engineer.is_empty() && !session.is_empty() {
        ev.engineer = crate::presence::read(home, session).map(|p| p.engineer).unwrap_or_default();
    }
    record(home, &ev);
}

/// Display: the characters of a feed excerpt (`excerpt`): one line of borg web's activity feed.
pub const FEED_CHARS: usize = 160;

/// The first `max` characters of `text` on one line (a feed excerpt, `FEED_CHARS`).
pub fn excerpt(text: &str, max: usize) -> String {
    let one = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match one.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &one[..i]),
        None => one,
    }
}

static PURPOSE: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// What this process's model calls are for (learn, assimilate, facets), set once by the command.
pub fn set_purpose(purpose: &'static str) {
    let _ = PURPOSE.set(purpose);
}

pub fn purpose() -> &'static str {
    PURPOSE.get().copied().unwrap_or("model")
}

fn read_events(path: &Path) -> Vec<Event> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Every local event: the history derived once from the older logs (`backfill`), the live ones and
/// the ignores live counting dropped (`recount_ignores`).
pub fn events(home: &Path) -> Vec<Event> {
    let mut all = read_events(&backfill_path(home));
    all.extend(read_events(&events_path(home)));
    all.extend(read_events(&ignores_path(home)));
    all
}

/// The newest `limit` of the events `events` returns whose kind is one of `kinds`, newest first, those of
/// one ms in `events`' order: the feed. Reads each line's leading `ms` and `kind` first (`jsonl::lead`)
/// and parses whole only the lines it returns (a line that is no `Event` gives way to the next).
pub fn newest(home: &Path, kinds: &[&str], limit: usize) -> Vec<Event> {
    let texts = [backfill_path(home), events_path(home), ignores_path(home)].map(|p| std::fs::read_to_string(p).unwrap_or_default());
    let mut heads: Vec<(i64, &str)> = texts
        .iter()
        .flat_map(|t| t.lines())
        .filter_map(|l| match crate::jsonl::lead(l, ["ms", "kind"])? {
            [ms, Scalar::Str(kind)] if kinds.contains(&kind.as_ref()) => Some((ms.as_i64()?, l)),
            _ => None,
        })
        .collect();
    heads.sort_by_key(|(ms, _)| std::cmp::Reverse(*ms));
    heads.into_iter().filter_map(|(_, l)| serde_json::from_str(l).ok()).take(limit).collect()
}

/// The live events only (stats/events.jsonl), without the derived history.
pub fn live(home: &Path) -> Vec<Event> {
    read_events(&events_path(home))
}

/// The live events `live` reads, each borrowed from its line and handed to `f` as read: a reader that only
/// folds them keeps none.
pub fn each_live(home: &Path, f: impl FnMut(&EventRef)) {
    each_in(&events_path(home), f);
}

/// Every event of the log at `path` that `read_events` reads, borrowed from its line, handed to `f`.
fn each_in(path: &Path, mut f: impl FnMut(&EventRef)) {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    for e in text.lines().filter_map(|l| serde_json::from_str::<EventRef>(l).ok()) {
        f(&e);
    }
}

/// When live counting began in `home`: its first live event (None: none yet). A repeated veto is
/// dated when it was said, earlier than its line, so it never marks the start. What `backfill`
/// derives is before it; a series backfill cannot derive (merges, repeated vetoes) is unknown there.
pub fn live_since(home: &Path) -> Option<i64> {
    let mut first: Option<i64> = None;
    each_live(home, |e| {
        if e.kind != VETO_REPEAT {
            first = Some(first.map_or(e.ms, |f| f.min(e.ms)));
        }
    });
    first
}

pub fn backfill_path(home: &Path) -> PathBuf {
    home.join("stats").join("backfill.jsonl")
}

/// The ignores live counting dropped (`recount_ignores`), derived once.
pub fn ignores_path(home: &Path) -> PathBuf {
    home.join("stats").join("ignores.jsonl")
}

/// Once per store: the judge's ignores 0.7.10 and 0.7.11 left out of live counting. The judge counted its
/// verdicts without its version, so `count_result` took today's ignores for an old judge's and dropped
/// them, and the asked→changed share read 100%. Derived from outcomes.jsonl as `backfill` derives
/// history: each ignore since live counting began whose (session, lesson) no live `ignored` event has.
/// Returns how many events it wrote.
pub fn recount_ignores(home: &Path) -> usize {
    let path = ignores_path(home);
    if path.exists() {
        return 0;
    }
    let live = live(home);
    let Some(from) = live.iter().filter(|e| e.kind != VETO_REPEAT).map(|e| e.ms).min() else { return 0 };
    let counted: std::collections::HashSet<(&str, &str)> =
        live.iter().filter(|e| e.kind == IGNORED).flat_map(|e| e.lessons.iter().map(|l| (e.session.as_str(), l.as_str()))).collect();
    let mut out: Vec<Event> = derive_history(home)
        .into_iter()
        .filter(|e| e.kind == IGNORED && e.ms >= from)
        .filter_map(|mut e| {
            e.lessons.retain(|l| !counted.contains(&(e.session.as_str(), l.as_str())));
            (!e.lessons.is_empty()).then_some(e)
        })
        .collect();
    out.sort_by_key(|e| e.ms);
    let text: String = out.iter().map(|e| serde_json::to_string(e).unwrap_or_default() + "\n").collect();
    if std::fs::create_dir_all(path.parent().unwrap_or(home)).is_err() || std::fs::write(&path, text).is_err() {
        return 0;
    }
    out.len()
}

fn lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Derives, once per store, the events that happened before live counting began from the logs Borg
/// kept anyway: corrections (episodes), outcomes, lessons, the queen's queue, imports, expert and
/// facet calls, steering drops, and the turn ends recorded in the session state (dated by the state
/// file's time). Only what happened before the first live event counts, so nothing is counted twice.
/// Returns how many events it wrote; 0 when it ran before.
pub fn backfill(home: &Path) -> usize {
    let path = backfill_path(home);
    if path.exists() {
        return 0;
    }
    let cutoff = live_since(home).unwrap_or_else(crate::learn::now_ms);
    let mut out = derive_history(home);
    out.retain(|e| e.ms > 0 && e.ms < cutoff);
    out.sort_by_key(|e| e.ms);
    let text: String = out.iter().map(|e| serde_json::to_string(e).unwrap_or_default() + "\n").collect();
    if std::fs::create_dir_all(path.parent().unwrap_or(home)).is_err() || std::fs::write(&path, text).is_err() {
        return 0;
    }
    out.len()
}

fn derive_history(home: &Path) -> Vec<Event> {
    use serde_json::Value;
    let s = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
    let ms = |v: &Value, k: &str| v[k].as_i64().unwrap_or(0);
    let ids = |v: &Value, k: &str| v[k].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect::<Vec<_>>();
    let mut out = Vec::new();
    // Who worked in which session: corrections name their engineer; outcomes do not.
    let mut who: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for e in lines(&home.join("episodes.jsonl")) {
        let tool = s(&e, "rejected_tool");
        if !s(&e, "engineer").is_empty() {
            who.insert(s(&e, "session"), s(&e, "engineer"));
        }
        let snippet = excerpt(&s(&e, "correction"), FEED_CHARS);
        // A veto (veto.rs) is counted as one, never as a correction.
        if s(&e, "kind") == crate::veto::KIND {
            out.push(Event {
                ms: ms(&e, "ms"),
                kind: VETO.into(),
                session: s(&e, "session"),
                engineer: s(&e, "engineer"),
                repo: s(&e, "repo"),
                text: snippet,
                ..Default::default()
            });
            continue;
        }
        let answer = s(&e, "via") == crate::learn::ANSWER;
        out.push(Event {
            ms: ms(&e, "ms"),
            kind: CORRECTION.into(),
            session: s(&e, "session"),
            engineer: s(&e, "engineer"),
            repo: s(&e, "repo"),
            why: if answer {
                ANSWERED.into()
            } else if tool.is_empty() {
                "pushback".into()
            } else {
                "rejected_tool".into()
            },
            text: if tool.is_empty() || answer { snippet } else { format!("{tool}: {snippet}") },
            source: source(&s(&e, "harness"), &s(&e, "harness_version")),
            ..Default::default()
        });
    }
    let engineer = |session: &str| who.get(session).cloned().unwrap_or_default();
    // One call writes one record per routing check (a stop's block, a live ask, the judge's verdicts of
    // a kind) and counts them live as one event: such records of one session, kind, channel and stamp
    // are one event here too, their lessons joined.
    let mut call: std::collections::HashMap<(String, &str, String, i64), usize> = std::collections::HashMap::new();
    for o in lines(&home.join("outcomes.jsonl")) {
        // A placebo's result is no result (outcome.rs `is_result`), as live counting leaves it out.
        if o["channel"] == crate::outcome::PLACEBO {
            continue;
        }
        let live = o["channel"] == "live";
        let kind = match o["kind"].as_str().unwrap_or_default() {
            // A free delivery counts once, as its own event (`early`, `queued_free`), never as a block.
            crate::outcome::ASKED if o["channel"] == "early" || o["channel"] == "queued" => continue,
            crate::outcome::ASKED if live => LIVE_ASK,
            crate::outcome::ASKED => BLOCK,
            crate::outcome::CHANGED => CHANGED,
            // An ignore by a judge before the never-came-up verdict is none (outcome.rs `ambiguous`).
            crate::outcome::IGNORED if crate::outcome::ambiguous(crate::outcome::IGNORED, o["judge"].as_u64().map_or(0, |j| j.min(255) as u8)) => continue,
            crate::outcome::IGNORED => IGNORED,
            crate::outcome::CORRECTED => CORRECTED,
            crate::outcome::NOT_CORRECTED => NOT_CORRECTED,
            crate::outcome::REGRETTED => REGRETTED,
            crate::outcome::TESTED => TESTED,
            // Measurements, never results (outcome.rs `is_result`): no event, as live counting skips them.
            crate::outcome::UNCLEAR | crate::outcome::REMAINS | crate::outcome::NOT_ARISEN => continue,
            _ => continue,
        };
        let lessons = ids(&o, "lessons");
        let session = s(&o, "session");
        let asks = kind == BLOCK || kind == LIVE_ASK;
        let key = (session.clone(), kind, s(&o, "channel"), ms(&o, "ms"));
        let merges = [BLOCK, LIVE_ASK, CHANGED, IGNORED].contains(&kind);
        if merges
            && let Some(&i) = call.get(&key)
            && let Some(e) = out.get_mut(i)
        {
            e.n += if asks { lessons.len() as u64 } else { 0 };
            e.lessons.extend(lessons);
            continue;
        }
        if merges {
            call.insert(key, out.len());
        }
        out.push(Event {
            ms: ms(&o, "ms"),
            kind: kind.into(),
            engineer: engineer(&session),
            session,
            repo: s(&o, "repo"),
            n: if asks { lessons.len() as u64 } else { 0 },
            lessons,
            channel: s(&o, "channel"),
            ..Default::default()
        });
    }
    // A lesson's first version is when it was learned; later versions carry no time of their own.
    let mut seen = std::collections::HashSet::new();
    for l in lines(&home.join("lessons.jsonl")) {
        let id = s(&l, "id");
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        let source = Some(s(&l, "source")).filter(|x| !x.is_empty()).unwrap_or_else(|| "session".into());
        out.push(Event {
            ms: ms(&l, "origin_ms"),
            kind: LESSON_NEW.into(),
            engineer: ids(&l, "engineers").into_iter().next().unwrap_or_default(),
            repo: s(&l, "repo"),
            lessons: vec![id],
            why: source,
            ..Default::default()
        });
    }
    let mut queen: BTreeMap<String, Value> = BTreeMap::new();
    for q in lines(&home.join("queen").join("queue.jsonl")) {
        queen.insert(s(&q, "id"), q);
    }
    for q in queen.values() {
        let base = || Event { engineer: s(q, "engineer"), session: s(q, "session"), repo: s(q, "repo"), lessons: vec![s(q, "id")], ..Default::default() };
        out.push(Event { ms: ms(q, "ms"), kind: QUEEN_QUEUED.into(), text: excerpt(&s(q, "question"), FEED_CHARS), ..base() });
        if let Some(at) = q["answered_ms"].as_i64() {
            let dur = u64::try_from(at - ms(q, "ms")).unwrap_or(0);
            out.push(Event {
                ms: at,
                kind: QUEEN_ANSWERED.into(),
                dur_ms: dur,
                engineer: s(q, "answered_by"),
                text: excerpt(&s(q, "answer"), FEED_CHARS),
                ..base()
            });
        }
        if let Some(at) = q["dismissed_ms"].as_i64() {
            out.push(Event { ms: at, kind: QUEEN_DISMISSED.into(), text: s(q, "dismissed"), ..base() });
        }
    }
    for (file, kind) in [("delivered.jsonl", QUEEN_DELIVERED), ("skips.jsonl", QUEEN_SKIPPED)] {
        for m in lines(&home.join("queen").join(file)) {
            let session = s(&m, "session");
            out.push(Event { ms: ms(&m, "ms"), kind: kind.into(), engineer: engineer(&session), session, lessons: vec![s(&m, "id")], ..Default::default() });
        }
    }
    for a in lines(&home.join("assimilated.jsonl")) {
        out.push(Event {
            ms: ms(&a, "ms"),
            kind: ASSIMILATED.into(),
            repo: s(&a, "source"),
            n: ids(&a, "lessons").len() as u64,
            usd: a["cost_usd"].as_f64().unwrap_or(0.0),
            ..Default::default()
        });
    }
    for (file, why) in [("experts.jsonl", "expert"), ("facets.jsonl", "facets")] {
        for c in lines(&home.join(file)) {
            let session = s(&c, "session");
            out.push(Event {
                ms: ms(&c, "ms"),
                kind: MODEL_CALL.into(),
                engineer: engineer(&session),
                session,
                repo: s(&c, "repo"),
                usd: c["cost_usd"].as_f64().unwrap_or(0.0),
                why: why.into(),
                ..Default::default()
            });
        }
    }
    // History: an older store's directives (the mesh outbox, the steer inbox). Nothing writes these
    // files any more; steering now counts as it happens (msg.rs `send`, `deliver`).
    for (file, kind) in [(home.join("mesh").join("directives.jsonl"), STEER_ISSUED), (home.join("steer").join("inbox.jsonl"), STEER_DELIVERED)] {
        for d in lines(&file) {
            out.push(Event {
                ms: ms(&d, "ms"),
                kind: kind.into(),
                engineer: s(&d, "engineer"),
                why: s(&d, "kind"),
                text: excerpt(&s(&d, "text"), FEED_CHARS),
                ..Default::default()
            });
        }
    }
    for d in lines(&crate::steer::dropped_path(home)) {
        out.push(Event { ms: ms(&d, "ms"), kind: STEER_DROPPED.into(), engineer: s(&d, "engineer"), why: s(&d, "why"), ..Default::default() });
    }
    out.extend(turns_from_state(&home.join("state"), &who));
    out
}

/// Turn ends, gate skips and model checks from the per-session state markers, dated by the state
/// file's last change (the markers carry no time of their own).
fn turns_from_state(dir: &Path, who: &std::collections::HashMap<String, String>) -> Vec<Event> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for f in rd.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "ctx")) {
        let Some(at) = std::fs::metadata(&f).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()) else {
            continue;
        };
        let session = f.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let base =
            || Event { ms: at.as_millis() as i64, session: session.clone(), engineer: who.get(&session).cloned().unwrap_or_default(), ..Default::default() };
        for line in std::fs::read_to_string(&f).unwrap_or_default().lines() {
            if let Some(score) = line.strip_prefix("#borg-skip ") {
                out.push(Event { kind: TURN_END.into(), ..base() });
                out.push(Event { kind: GATE_SKIP.into(), why: score.trim().into(), ..base() });
            } else if let Some(cost) = line.strip_prefix("#borg-predict-cost ") {
                out.push(Event { kind: TURN_END.into(), ..base() });
                out.push(Event { kind: CHECK.into(), usd: cost.trim().parse().unwrap_or(0.0), ..base() });
            }
        }
    }
    out
}

/// This machine's id in the collective's statistics, created once (`stats/node`). Not the mesh key:
/// stats work in local collectives too, and nothing here should need the member key.
pub fn node(home: &Path) -> String {
    let path = home.join("stats").join("node");
    if let Ok(id) = std::fs::read_to_string(&path)
        && !id.trim().is_empty()
    {
        return id.trim().to_string();
    }
    // Created whole and only once: the id is written to a file of its own, then hard-linked into
    // place, which fails when another process got there first; its id wins. A plain write could hand
    // two first callers two ids, and a reader an empty file between its truncate and write.
    let id = fresh_id();
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(home));
    let tmp = path.with_extension(format!("{id}.tmp"));
    let linked = std::fs::write(&tmp, &id).and_then(|()| std::fs::hard_link(&tmp, &path));
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::read_to_string(&path).map(|s| s.trim().to_string()).ok().filter(|s| !s.is_empty()).unwrap_or(id)
        }
        _ => id,
    }
}

fn fresh_id() -> String {
    let mut bytes = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut bytes);
    }
    if bytes == [0; 8] {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_i64(crate::learn::now_ms());
        h.write_u32(std::process::id());
        bytes = h.finish().to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `YYYY-MM-DD` (UTC) of an epoch-ms time.
pub fn day(ms: i64) -> String {
    let s = crate::install::utc(ms);
    format!("{}-{}-{}", &s[0..4], &s[4..6], &s[6..8])
}

/// Every `YYYY-MM-DD` from `first` to `last` (UTC), both included; empty if either is malformed.
pub fn days_between(first: &str, last: &str) -> Vec<String> {
    let (Some(a), Some(b)) = (day_number(first), day_number(last)) else { return Vec::new() };
    (a..=b).map(|d| day(d * crate::clock::DAY_MS)).collect()
}

/// What the detached checks (check_async.rs) did, from the local events (`async_tally`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AsyncTally {
    /// Model calls, and of them those that gave no answer, and those that took their whole limit.
    pub checks: u64,
    pub failed: u64,
    pub timed_out: u64,
    /// Jobs that never called the model: a newer stop's replaced them, the session had ended, or their
    /// job could not be written, started or placed (check_async.rs `not_run`).
    pub superseded: u64,
    pub gone: u64,
    pub unstarted: u64,
    /// The latest call's limit (ms), the largest peak memory of a check (KiB), and the lessons a check
    /// showed its model, on average.
    pub limit_ms: u64,
    pub rss_kb: u64,
    pub lessons: f64,
    pub usd: f64,
}

/// Grace: a failed call this close to its limit (ms) took it all. The runner kills it at the limit; its
/// own start and teardown are within this.
const LIMIT_GRACE_MS: u64 = 1_000;

/// The detached checks among `events` (`mode` `async`), and their checks that never ran.
#[must_use]
pub fn async_tally(events: &[Event]) -> AsyncTally {
    let mut t = AsyncTally::default();
    let mut shown = 0u64;
    let mut latest = i64::MIN;
    for e in events {
        match e.kind.as_str() {
            CHECK if e.mode == crate::check_async::MODE => {
                t.checks += 1;
                shown += e.n;
                t.usd += e.usd;
                t.rss_kb = t.rss_kb.max(e.rss_kb);
                if e.why == "failed" {
                    t.failed += 1;
                    t.timed_out += u64::from(e.limit_ms > 0 && e.dur_ms + LIMIT_GRACE_MS >= e.limit_ms);
                }
                if e.limit_ms > 0 && e.ms >= latest {
                    (latest, t.limit_ms) = (e.ms, e.limit_ms);
                }
            }
            CHECK_DROPPED if e.why == crate::check_async::WHY_SUPERSEDED => t.superseded += 1,
            CHECK_DROPPED if e.why == crate::check_async::WHY_GONE => t.gone += 1,
            CHECK_DROPPED if e.why == crate::check_async::WHY_UNSTARTED || e.why == crate::check_async::WHY_UNPLACED => t.unstarted += 1,
            _ => {}
        }
    }
    if t.checks > 0 {
        t.lessons = shown as f64 / t.checks as f64;
    }
    t
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` date (clock.rs `days_from_civil`).
fn day_number(date: &str) -> Option<i64> {
    let mut it = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (it.next()??, it.next()??, it.next()??);
    Some(crate::clock::days_from_civil(y, m, d))
}

/// One session's corrections and vetoes (`per_session`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Said {
    /// The engineer's corrections: a rejected tool call or a pushback (`CORRECTION`, a regret aside:
    /// a regret is a later fix, counted in the session that fixed, not by the engineer in this one; an
    /// answer to the drone's own question aside too: the drone asked for it).
    pub corrections: u64,
    /// `borg no` vetoes (`VETO`).
    pub vetoes: u64,
}

/// Per session, what the engineer said against its work: every correction and veto, whether or not
/// `borg learn` merged it into a lesson (a merge is what outcome.rs sees; most corrections merge
/// into none). The denominator for how often a mistake was seen at all, beside the results.
pub fn per_session(events: &[Event]) -> BTreeMap<String, Said> {
    let mut out: BTreeMap<String, Said> = BTreeMap::new();
    for e in events.iter().filter(|e| !e.session.is_empty()) {
        match e.kind.as_str() {
            CORRECTION if e.why != "regret" && e.why != crate::learn::ANSWER => out.entry(e.session.clone()).or_default().corrections += 1,
            VETO => out.entry(e.session.clone()).or_default().vetoes += 1,
            _ => {}
        }
    }
    out
}

/// The local events folded into this node's buckets, keyed by bucket id (version 0).
pub fn fold(node: &str, events: &[Event]) -> BTreeMap<String, Bucket> {
    let mut f = Folder::new(node);
    for e in events {
        f.add(&Head {
            ms: e.ms,
            kind: &e.kind,
            engineer: &e.engineer,
            lessons: &e.lessons,
            n: e.n,
            usd: e.usd,
            dur_ms: e.dur_ms,
            why: &e.why,
            mode: &e.mode,
            channel: &e.channel,
        });
    }
    f.out
}

/// What `fold` reads of an event, from an owned `Event` or borrowed from its line (`EventRef`).
struct Head<'e, L> {
    ms: i64,
    kind: &'e str,
    engineer: &'e str,
    lessons: &'e [L],
    n: u64,
    usd: f64,
    dur_ms: u64,
    why: &'e str,
    mode: &'e str,
    channel: &'e str,
}

/// A local event as a fold reads it (`current`, `each_live`), borrowed from its line (a string with an
/// escape is copied): every field `Event` reads, in its order and with its types, so the two take the same
/// lines; the strings no fold reads are checked, not kept.
#[derive(Deserialize)]
pub struct EventRef<'a> {
    pub ms: i64,
    #[serde(borrow)]
    pub kind: Cow<'a, str>,
    #[serde(default, rename = "session")]
    _session: Str,
    #[serde(default, borrow)]
    pub engineer: Cow<'a, str>,
    #[serde(default, rename = "repo")]
    _repo: Str,
    #[serde(default, borrow)]
    pub lessons: Vec<Text<'a>>,
    #[serde(default)]
    pub n: u64,
    #[serde(default)]
    pub usd: f64,
    #[serde(default)]
    pub dur_ms: u64,
    #[serde(default, borrow)]
    pub why: Cow<'a, str>,
    #[serde(default, rename = "text")]
    _text: Str,
    #[serde(default, borrow)]
    pub mode: Cow<'a, str>,
    #[serde(default, borrow)]
    pub channel: Cow<'a, str>,
    #[serde(default, rename = "source")]
    _source: Str,
    #[serde(default, rename = "rss_kb")]
    _rss_kb: u64,
    #[serde(default, rename = "limit_ms")]
    _limit_ms: u64,
}

impl EventRef<'_> {
    fn head(&self) -> Head<'_, Text<'_>> {
        Head {
            ms: self.ms,
            kind: &self.kind,
            engineer: &self.engineer,
            lessons: &self.lessons,
            n: self.n,
            usd: self.usd,
            dur_ms: self.dur_ms,
            why: &self.why,
            mode: &self.mode,
            channel: &self.channel,
        }
    }
}

/// `fold` one event at a time. An event allocates only for a bucket, a key or a lesson it is the first
/// of: its bucket id and `<kind>…` keys are written into reused buffers, and the UTC day is made once per
/// run of events on one day (events are logged in time order), not per event.
struct Folder<'n> {
    node: &'n str,
    out: BTreeMap<String, Bucket>,
    /// The last event's day: its number since 1970-01-01 (`day` depends on nothing else) and `day` of it.
    day: Option<(i64, String)>,
    id: String,
    key: String,
}

impl<'n> Folder<'n> {
    fn new(node: &'n str) -> Self {
        Folder { node, out: BTreeMap::new(), day: None, id: String::new(), key: String::new() }
    }

    fn add<L: std::ops::Deref<Target = str>>(&mut self, e: &Head<'_, L>) {
        let Folder { node, out, day: last, id, key } = self;
        let number = e.ms.div_euclid(crate::clock::DAY_MS);
        let date = match last {
            Some((at, d)) if *at == number => d,
            _ => &last.insert((number, day(e.ms))).1,
        };
        id.clear();
        let _ = write!(id, "stat:{node}:{}:{date}", e.engineer);
        let b = slot(out, id, Bucket::default);
        // A new bucket: no id is empty.
        if b.id.is_empty() {
            *b = Bucket { id: id.clone(), day: date.clone(), node: (*node).into(), engineer: e.engineer.into(), ..Default::default() };
        }
        // An answer to the drone's question is counted apart, so "correction" keeps its meaning.
        let kind = if e.kind == CORRECTION && e.why == crate::learn::ANSWER { ANSWERED } else { e.kind };
        *slot(&mut b.counts, kind, Default::default) += 1;
        if e.n > 0 {
            key.clear();
            let _ = write!(key, "{}_n", e.kind);
            *slot(&mut b.counts, key, Default::default) += e.n;
        }
        // Reasons folded as `<kind>:<why>` into `why`, so the page can say why a turn end passed free,
        // why a check was dropped, how many failed and which asks a detached check made (the raw `why`
        // stays on the event; a score folds to `score`, `live_covered` is live mode's skip before 0.7.6,
        // a skip without a reason folds to none).
        let why = match (e.kind, e.why) {
            (GATE_SKIP, w @ ("noedit" | "empty" | "live_covered")) => w,
            (GATE_SKIP, w) if w.parse::<f64>().is_ok() => "score",
            (GATE_SKIP, w) if !w.is_empty() => "other",
            (CHECK | LIVE_CHECK, "failed") => "failed",
            (CHECK_DROPPED, w) if !w.is_empty() => w,
            (LIVE_ASK, _) if e.mode == crate::check_async::MODE => crate::check_async::MODE,
            _ => "",
        };
        if !why.is_empty() {
            key.clear();
            let _ = write!(key, "{}:{why}", e.kind);
            *slot(&mut b.why, key, Default::default) += 1;
        }
        // A turn-end check's time, summed with the checks that report one: the page's average.
        if e.kind == CHECK && e.dur_ms > 0 {
            *slot(&mut b.why, "check_ms", Default::default) += e.dur_ms;
            *slot(&mut b.why, "check_timed", Default::default) += 1;
        }
        b.usd += e.usd;
        if e.usd > 0.0 {
            *slot(&mut b.usd_by, e.kind, Default::default) += e.usd;
        }
        // Checks from before modes existed were all turn-end checks: hook mode.
        if e.usd > 0.0 && (e.kind == CHECK || e.kind == LIVE_CHECK) {
            *slot(&mut b.usd_mode, if e.mode.is_empty() { "hook" } else { e.mode }, Default::default) += e.usd;
        }
        if !e.channel.is_empty() {
            key.clear();
            let _ = write!(key, "{}@{}", e.kind, e.channel);
            *slot(&mut b.channels, key, Default::default) += 1;
        }
        b.dur_ms += e.dur_ms;
        for l in e.lessons {
            *slot(slot(&mut b.lessons, l, BTreeMap::new), e.kind, Default::default) += 1;
        }
    }
}

/// The latest version of every bucket in `stats.jsonl` (every node's, after the mesh merged them). Two
/// lines of one version (two rollups at once) are ranked as borg-mesh lines.rs `settle` ranks them, by
/// the line's blake3 hash, so every node shows the same one whatever order the lines arrived in; a line
/// that is no `Bucket` is passed over.
///
/// The file is read whole once per process and then followed (`latest`): borg-web asks on every request,
/// and reads only what was appended since.
pub fn rollups(home: &Path) -> BTreeMap<String, Bucket> {
    latest(home).buckets().map(|b| (b.id.clone(), b.clone())).collect()
}

/// A line's `id` and `version`, the head `Rollups::fold` ranks a line by before it parses it whole.
fn head(line: &str) -> Option<(Cow<'_, str>, u64)> {
    match crate::jsonl::lead(line, ["id", "version"])? {
        [Scalar::Str(id), version] => Some((id, version.as_u64()?)),
        _ => None,
    }
}

/// An id's latest bucket (`Rollups`), and its line's blake3 hash, which ranks lines of one version.
#[derive(Debug, Clone)]
struct Latest {
    hash: [u8; 32],
    bucket: Bucket,
}

impl Latest {
    /// The bucket on `line`, if its head reads and it parses whole.
    fn of(line: &str) -> Option<Latest> {
        head(line)?;
        let bucket = serde_json::from_str(line).ok()?;
        Some(Latest { hash: *blake3::hash(line.as_bytes()).as_bytes(), bucket })
    }

    /// Whether a line of `version` and `hash` ranks above this one.
    fn outranked_by(&self, version: u64, hash: &[u8; 32]) -> bool {
        (version, hash) > (self.bucket.version, &self.hash)
    }
}

/// `stats.jsonl` folded to the latest bucket per id (`rollups`), followed (follow.rs): read whole once,
/// then only what was appended. It keeps no text, only each id's winner and the unterminated last line's
/// bucket, which the next tail reads again.
#[derive(Debug, Clone, Default)]
struct Rollups {
    latest: BTreeMap<String, Latest>,
    /// The bucket on the unterminated last line, if it is one: ranked with the others when they are read,
    /// and dropped when the line is read again with the tail.
    open: Option<Latest>,
    mark: Mark,
    end: usize,
    parsed: usize,
    guard: Vec<u8>,
}

impl Rollups {
    /// Folds the lines of `text` (which starts at the file's byte `self.parsed`) in, and moves `parsed`
    /// to its unterminated last line.
    ///
    /// Most lines are superseded versions (94% on one store), so it reads each line's `head` first and
    /// parses whole only the lines that could win: per id, from the newest version down, the lines of a
    /// version by hash, until one is a `Bucket` that ranks above the id's winner so far.
    fn fold(&mut self, text: &str) {
        let last = if text.ends_with('\n') { text.len() } else { text.rfind('\n').map_or(0, |i| i + 1) };
        let (done, open) = text.split_at(last);
        for (id, mut lines) in crate::jsonl::by_id(done, head) {
            let bar = self.latest.get(&*id).map(|l| (l.bucket.version, l.hash));
            lines.retain(|(v, _)| bar.is_none_or(|(at, _)| *v >= at));
            lines.sort_by_key(|(v, _)| std::cmp::Reverse(*v));
            let won = lines.chunk_by(|a, b| a.0 == b.0).find_map(|tied| {
                let mut ranked: Vec<([u8; 32], &str)> = tied.iter().map(|(_, l)| (*blake3::hash(l.as_bytes()).as_bytes(), *l)).collect();
                ranked.sort_by_key(|(h, _)| std::cmp::Reverse(*h));
                ranked.into_iter().filter(|(h, _)| bar.is_none_or(|(at, ah)| (tied[0].0, h) > (at, &ah))).find_map(|(hash, l)| {
                    let bucket = serde_json::from_str::<Bucket>(l).ok()?;
                    Some(Latest { hash, bucket })
                })
            });
            if let Some(w) = won {
                self.latest.insert(w.bucket.id.clone(), w);
            }
        }
        self.open = open.lines().next().and_then(Latest::of);
        self.guard = crate::follow::guard_after(&self.guard, done.as_bytes());
        self.parsed += last;
        self.end = self.parsed + open.len();
    }

    /// Every id's latest bucket, the unterminated last line's among them.
    fn buckets(&self) -> impl Iterator<Item = &Bucket> {
        let open = self.open.as_ref().filter(|o| self.latest.get(&o.bucket.id).is_none_or(|l| l.outranked_by(o.bucket.version, &o.hash)));
        self.latest.values().filter(move |l| open.is_none_or(|o| o.bucket.id != l.bucket.id)).chain(open).map(|l| &l.bucket)
    }
}

impl Follow for Rollups {
    fn whole(mark: Mark, text: String) -> Rollups {
        let mut r = Rollups { mark, ..Rollups::default() };
        r.fold(&text);
        r
    }

    fn held(&self) -> Held<'_> {
        Held { mark: self.mark, end: self.end, parsed: self.parsed, guard: &self.guard }
    }

    fn extend(&mut self, mark: Mark, tail: String) {
        self.mark = mark;
        self.fold(&tail);
    }
}

/// What this process holds of one store's `stats.jsonl` (`latest`), and how often it read the file whole
/// and in part.
#[derive(Default)]
struct Followed {
    rollups: Option<Arc<Rollups>>,
    whole: usize,
    tail: usize,
}

/// The `stats.jsonl` of every store this process read (`latest`), each behind a lock of its own.
static FOLLOWED: Mutex<Vec<(PathBuf, Arc<Mutex<Followed>>)>> = Mutex::new(Vec::new());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The rollups of the store `home` as its `stats.jsonl` is now: read whole on first use, then a stat per
/// call and a read of what was appended (follow.rs). The file's slot stays locked while the file is read,
/// so requests at once read it whole once between them, not once each; a caller that still holds an
/// older snapshot keeps it, and the next extension is a copy.
fn latest(home: &Path) -> Arc<Rollups> {
    let path = rollups_path(home);
    let slot = {
        let mut all = lock(&FOLLOWED);
        match all.iter().find(|(p, _)| *p == path) {
            Some((_, s)) => Arc::clone(s),
            None => {
                let s = Arc::default();
                all.push((path.clone(), Arc::clone(&s)));
                s
            }
        }
    };
    let mut f = lock(&slot);
    let r = match f.rollups.take() {
        None => {
            f.whole += 1;
            let (mark, text) = crate::follow::whole(&path);
            Arc::new(Rollups::whole(mark, text))
        }
        Some(mut r) => match follow(&path, r.held()) {
            Read::Same => r,
            Read::Tail(mark, tail) => {
                f.tail += 1;
                Arc::make_mut(&mut r).extend(mark, tail);
                r
            }
            Read::Whole(mark, text) => {
                f.whole += 1;
                Arc::new(Rollups::whole(mark, text))
            }
        },
    };
    f.rollups = Some(Arc::clone(&r));
    r
}

/// Publishes this node's buckets whose counts changed since their last line in `stats.jsonl`, each
/// with the next version. Returns how many lines it appended (0 when nothing changed).
pub fn rollup(home: &Path) -> usize {
    backfill(home);
    recount_ignores(home);
    // No events, nothing to publish: and no node id created in a store that never counted anything.
    let evs = events(home);
    if evs.is_empty() {
        return 0;
    }
    let me = node(home);
    let latest = latest(home);
    let published: HashMap<&str, &Bucket> = latest.buckets().map(|b| (b.id.as_str(), b)).collect();
    // This engineer's personal lessons (reach.rs) are no team's business, not even by id.
    let personal = crate::reach::personal_ids(home);
    let fresh: Vec<Bucket> = fold(&me, &evs)
        .into_values()
        .filter_map(|mut b| {
            b.lessons.retain(|id, _| !personal.contains(id));
            let old = published.get(b.id.as_str());
            if old.is_some_and(|o| o.same(&b)) {
                return None;
            }
            b.version = old.map_or(1, |o| o.version + 1);
            Some(b)
        })
        .collect();
    if fresh.is_empty() {
        return 0;
    }
    let lines: String = fresh.iter().map(|b| serde_json::to_string(b).unwrap_or_default() + "\n").collect();
    let path = rollups_path(home);
    let ok = std::fs::OpenOptions::new().create(true).append(true).open(&path).and_then(|mut f| f.write_all(lines.as_bytes()));
    if ok.is_ok() { fresh.len() } else { 0 }
}

/// Every bucket the page shows: other nodes' published rollups, and this node's own counts folded
/// fresh from its local events, so local activity shows before the next rollup.
pub fn current(home: &Path) -> Vec<Bucket> {
    backfill(home);
    recount_ignores(home);
    let me = node(home);
    let mut all: BTreeMap<String, Bucket> = latest(home).buckets().filter(|b| b.node != me).map(|b| (b.id.clone(), b.clone())).collect();
    // `fold(&me, &events(home))`, folded as read: one log's text at a time, no event kept.
    let mut f = Folder::new(&me);
    for path in [backfill_path(home), events_path(home), ignores_path(home)] {
        each_in(&path, |e| f.add(&e.head()));
    }
    all.extend(f.out);
    all.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrections_and_vetoes_count_per_session_merged_or_not() {
        let ev = |kind: &str, session: &str, why: &str| Event { kind: kind.into(), session: session.into(), why: why.into(), ..Default::default() };
        let events = [
            ev(CORRECTION, "a", "pushback"),
            ev(CORRECTION, "a", "rejected_tool"),
            ev(CORRECTION, "a", "regret"),
            ev(CORRECTION, "a", crate::learn::ANSWER),
            ev(VETO, "a", "prompt"),
            ev(VETO, "b", "cli"),
            ev(CHANGED, "b", ""),
            ev(CORRECTION, "", "pushback"),
        ];
        let got = per_session(&events);
        assert_eq!(got["a"], Said { corrections: 2, vetoes: 1 }, "an answer to the drone's own question is no correction");
        assert_eq!(got["b"], Said { corrections: 0, vetoes: 1 });
        assert_eq!(got.len(), 2);
        // Rolled up, an answer has a count of its own: the corrections tiles keep their meaning.
        let buckets = fold("n", &events);
        let b = buckets.values().next().unwrap();
        assert_eq!((b.counts[CORRECTION], b.counts[ANSWERED]), (4, 1));
    }

    #[test]
    fn the_history_derived_from_episodes_tells_answers_apart() {
        let home = std::env::temp_dir().join(format!("borg-stats-answer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let eps = [
            serde_json::json!({"id":"e1","ms":1,"repo":"r","session":"s","task":"t","agent":"Which backoff?","rejected_tool":null,"correction":"Exponential","via":"answer"}),
            serde_json::json!({"id":"e2","ms":2,"repo":"r","session":"s","task":"t","agent":"a","rejected_tool":null,"correction":"no, not like that"}),
        ];
        std::fs::write(home.join("episodes.jsonl"), eps.iter().map(|e| e.to_string() + "\n").collect::<String>()).unwrap();
        let whys: Vec<String> = derive_history(&home).into_iter().filter(|e| e.kind == CORRECTION).map(|e| e.why).collect();
        assert_eq!(whys, [ANSWERED, "pushback"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// An ignore by a judge before the never-came-up verdict is no event (outcome.rs `ambiguous`), as
    /// live counting leaves it out; today's judge's ignore is one.
    #[test]
    fn the_history_counts_no_ignore_from_before_the_never_came_up_verdict() {
        let home = std::env::temp_dir().join(format!("borg-stats-old-ignore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let rec = |s: &str, judge: u8| serde_json::json!({"kind":"asked_ignored","session":s,"lessons":["L"],"ms":5,"channel":"stop","judge":judge});
        let log = [rec("old", 0), rec("v2", 2), rec("today", 3)];
        std::fs::write(home.join("outcomes.jsonl"), log.iter().map(|e| e.to_string() + "\n").collect::<String>()).unwrap();
        let got: Vec<String> = derive_history(&home).into_iter().filter(|e| e.kind == IGNORED).map(|e| e.session).collect();
        assert_eq!(got, ["today"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn concurrent_first_calls_agree_on_one_node() {
        // The node id stamps the outcomes and stats that sync: two hooks creating it at once must
        // agree on one.
        for round in 0..20 {
            let home = std::env::temp_dir().join(format!("borg-stats-node-{}-{round}", std::process::id()));
            let _ = std::fs::remove_dir_all(&home);
            let ids: Vec<String> = std::thread::scope(|s| {
                let hs: Vec<_> = (0..16).map(|_| s.spawn(|| node(&home))).collect();
                hs.into_iter().map(|h| h.join().unwrap()).collect()
            });
            assert!(ids.iter().all(|id| *id == ids[0] && id.len() == 16), "{ids:?}");
            assert_eq!(node(&home), ids[0]);
            let _ = std::fs::remove_dir_all(&home);
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-stats-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn ev(kind: &str, ms: i64, engineer: &str, lessons: &[&str], n: u64) -> Event {
        Event { engineer: engineer.into(), lessons: lessons.iter().map(|s| s.to_string()).collect(), n, ..Event::new(kind, ms) }
    }

    /// Derived: one day (clock.rs `DAY_MS`), the tests' unit.
    const DAY: i64 = crate::clock::DAY_MS;

    #[test]
    fn days_between_fills_every_day() {
        assert_eq!(days_between("2026-02-27", "2026-03-02"), ["2026-02-27", "2026-02-28", "2026-03-01", "2026-03-02"]);
        assert_eq!(days_between("1970-01-01", "1970-01-01"), ["1970-01-01"]);
        assert!(days_between("x", "1970-01-01").is_empty());
    }

    #[test]
    fn day_is_utc_date() {
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(day(DAY - 1), "1970-01-01");
        assert_eq!(day(DAY), "1970-01-02");
    }

    #[test]
    fn fold_counts_per_engineer_and_day() {
        let evs = [
            ev(TURN_END, 10, "a@x", &[], 0),
            ev(CHECK, 20, "a@x", &[], 7),
            ev(BLOCK, 30, "a@x", &["L1", "L2"], 2),
            ev(TURN_END, DAY + 5, "a@x", &[], 0),
            ev(TURN_END, 40, "b@x", &[], 0),
        ];
        let b = fold("n1", &evs);
        assert_eq!(b.len(), 3, "two days for a, one for b");
        let a = &b["stat:n1:a@x:1970-01-01"];
        assert_eq!((a.counts[TURN_END], a.counts[CHECK], a.counts["check_n"], a.counts["block_n"]), (1, 1, 7, 2));
        assert_eq!(a.lessons["L1"][BLOCK], 1);
    }

    #[test]
    fn fold_splits_check_cost_by_mode_and_asks_by_channel() {
        let with = |kind: &str, usd: f64, mode: &str, channel: &str| Event { usd, mode: mode.into(), channel: channel.into(), ..ev(kind, 10, "a@x", &[], 0) };
        let evs = [
            with(CHECK, 0.05, "", ""),
            with(CHECK, 0.04, "live", ""),
            with(LIVE_CHECK, 0.03, "live", ""),
            with(MODEL_CALL, 0.5, "live", ""),
            with(BLOCK, 0.0, "live", "stop"),
            with(LIVE_ASK, 0.0, "live", "live"),
            with(CHANGED, 0.0, "", "live"),
        ];
        let b = &fold("n1", &evs)["stat:n1:a@x:1970-01-01"];
        assert!((b.usd_mode["hook"] - 0.05).abs() < 1e-9 && (b.usd_mode["live"] - 0.07).abs() < 1e-9, "{:?}", b.usd_mode);
        assert_eq!(b.usd_mode.len(), 2, "only the conscience's checks: {:?}", b.usd_mode);
        assert_eq!((b.channels["block@stop"], b.channels["live_ask@live"], b.channels["changed@live"]), (1, 1, 1));
        assert_eq!((b.counts[CHECK], b.counts[LIVE_CHECK]), (2, 1), "live checks are counted apart from turn-end checks");
    }

    #[test]
    fn fold_counts_skip_and_drop_reasons_failed_checks_and_check_time() {
        let why = |kind: &str, why: &str, dur_ms: u64| Event { why: why.into(), dur_ms, ..ev(kind, 10, "a@x", &[], 0) };
        let evs = [
            why(GATE_SKIP, "noedit", 0),
            why(GATE_SKIP, "0.12", 0),
            why(GATE_SKIP, "0.30", 0),
            why(GATE_SKIP, "empty", 0),
            why(GATE_SKIP, "live_covered", 0),
            why(GATE_SKIP, "", 0),
            why(CHECK, "failed", 4_000),
            why(CHECK, "", 6_000),
            why(CHECK, "", 0),
            why(CHECK_DROPPED, "superseded", 0),
            why(CHECK_DROPPED, "", 0),
            Event { mode: crate::check_async::MODE.into(), ..why(LIVE_ASK, "wake", 0) },
            why(LIVE_ASK, "", 0),
        ];
        let b = &fold("n1", &evs)["stat:n1:a@x:1970-01-01"];
        let n = |k: &str| b.counts.get(k).or(b.why.get(k)).copied().unwrap_or(0);
        assert!(
            b.counts.keys().all(|k| !k.contains(':') && !k.starts_with("check_ms") && k != "check_timed"),
            "counts stay as 0.7.8 folds them: {:?}",
            b.counts
        );
        assert_eq!((n("gate_skip:noedit"), n("gate_skip:score"), n("gate_skip:empty"), n("gate_skip:live_covered")), (1, 2, 1, 1), "{:?}", b.counts);
        assert_eq!(n(GATE_SKIP), 6, "a skip without a reason is counted, with no reason");
        assert_eq!((n("check:failed"), n("check_dropped:superseded")), (1, 1));
        assert_eq!((n("check_ms"), n("check_timed")), (10_000, 2), "an untimed check is not averaged");
        assert_eq!((n(LIVE_ASK), n("live_ask:async")), (2, 1), "a detached check's asks apart from live mode's");
        assert!(!b.why.keys().any(|k| k.ends_with(':') || k == "gate_skip:other"), "no empty reason: {:?}", b.why);
    }

    #[test]
    fn the_async_tally_counts_detached_checks_and_the_jobs_that_never_ran() {
        let check = |ms: i64, n: u64, why: &str, dur_ms: u64, limit_ms: u64, rss_kb: u64| Event {
            mode: crate::check_async::MODE.into(),
            why: why.into(),
            dur_ms,
            limit_ms,
            rss_kb,
            usd: 0.01,
            ..ev(CHECK, ms, "a@x", &[], n)
        };
        let dropped = |why: &str| Event { why: why.into(), mode: crate::check_async::MODE.into(), ..ev(CHECK_DROPPED, 5, "a@x", &[], 0) };
        let evs = [
            check(10, 40, "", 9_000, 45_000, 30_000),
            check(30, 20, "failed", 44_500, 45_000, 52_000),
            check(20, 30, "failed", 2_000, 60_000, 31_000),
            ev(CHECK, 40, "a@x", &[], 99),
            dropped("superseded"),
            dropped("superseded"),
            dropped("gone"),
        ];
        let t = async_tally(&evs);
        assert_eq!((t.checks, t.failed, t.timed_out, t.superseded, t.gone), (3, 2, 1, 2, 1), "{t:?}");
        assert_eq!((t.limit_ms, t.rss_kb), (45_000, 52_000), "the latest limit, the largest peak");
        assert!((t.lessons - 30.0).abs() < 1e-9 && (t.usd - 0.03).abs() < 1e-9, "a stop's own check is not counted: {t:?}");
        assert_eq!(async_tally(&[]), AsyncTally::default());
        // A job that never ran is no model check in the rollup either.
        let b = &fold("n1", &evs)["stat:n1:a@x:1970-01-01"];
        assert_eq!((b.counts[CHECK], b.counts[CHECK_DROPPED]), (4, 3));
        assert!((b.usd_mode["async"] - 0.03).abs() < 1e-9, "{:?}", b.usd_mode);
    }

    #[test]
    fn rollup_versions_only_on_change_and_is_idempotent() {
        let home = tmp("rollup");
        record(&home, &ev(TURN_END, 10, "a@x", &[], 0));
        assert_eq!(rollup(&home), 1);
        assert_eq!(rollup(&home), 0, "nothing changed: nothing appended");
        record(&home, &ev(GATE_SKIP, 11, "a@x", &[], 0));
        assert_eq!(rollup(&home), 1);
        let r = rollups(&home);
        let b = r.values().next().unwrap();
        assert_eq!((b.version, b.counts[TURN_END], b.counts[GATE_SKIP]), (2, 1, 1));
        let text = std::fs::read_to_string(rollups_path(&home)).unwrap();
        assert!(!text.contains("session"), "no session ids leave the machine");
    }

    /// A store shared with an older borg (0.7.8, no `why`): the older one published this node's day
    /// without reasons; this one publishes it once with them and then settles, and its line's `counts`
    /// equal the older fold's, so the older borg's `same` (counts, lessons, channels, time, cost) sees
    /// no change either: neither republishes the day by turns.
    #[test]
    fn reasons_publish_once_beside_an_older_borgs_buckets() {
        let home = tmp("rollup-why");
        record(&home, &Event { why: "noedit".into(), ..ev(GATE_SKIP, 10, "a@x", &[], 0) });
        record(&home, &Event { why: "failed".into(), dur_ms: 900, ..ev(CHECK, 11, "a@x", &[], 0) });
        let mut older = fold(&node(&home), &events(&home)).into_values().next().unwrap();
        older.why.clear();
        older.version = 1;
        std::fs::write(rollups_path(&home), serde_json::to_string(&older).unwrap() + "\n").unwrap();
        assert_eq!(rollup(&home), 1, "the reasons are published once");
        assert_eq!(rollup(&home), 0, "then nothing changes");
        let mine = rollups(&home).into_values().next().unwrap();
        assert_eq!((mine.version, &mine.counts), (2, &older.counts), "counts stay as the older borg folds them");
        assert_eq!((mine.why["gate_skip:noedit"], mine.why["check:failed"]), (1, 1));
    }

    #[test]
    fn current_prefers_own_fresh_counts_and_keeps_peers() {
        let home = tmp("current");
        record(&home, &ev(TURN_END, 10, "a@x", &[], 0));
        rollup(&home);
        record(&home, &ev(TURN_END, 12, "a@x", &[], 0));
        let peer = Bucket {
            id: "stat:other:b@x:1970-01-01".into(),
            version: 3,
            day: "1970-01-01".into(),
            node: "other".into(),
            engineer: "b@x".into(),
            ..Default::default()
        };
        let mut f = std::fs::OpenOptions::new().append(true).open(rollups_path(&home)).unwrap();
        writeln!(f, "{}", serde_json::to_string(&peer).unwrap()).unwrap();
        let cur = current(&home);
        assert_eq!(cur.len(), 2);
        let mine = cur.iter().find(|b| b.engineer == "a@x").unwrap();
        assert_eq!(mine.counts[TURN_END], 2, "own counts are folded fresh, not the stale rollup");
    }

    /// Two v+1 lines of one bucket (two rollups at once) reach peers in either order: every node shows
    /// the same one, the one mesh `settle` makes last (higher hash).
    #[test]
    fn rollups_break_version_ties_by_hash_whatever_the_order() {
        let bucket = |n: u64| {
            let mut b = Bucket { id: "stat:n:a@x:1970-01-01".into(), version: 2, ..Default::default() };
            b.counts.insert(TURN_END.into(), n);
            serde_json::to_string(&b).unwrap()
        };
        let (a, b) = (bucket(1), bucket(2));
        let shown = |name: &str, first: &str, second: &str| {
            let home = tmp(name);
            std::fs::write(rollups_path(&home), format!("{first}\n{second}\n")).unwrap();
            rollups(&home).into_values().next().unwrap().counts[TURN_END]
        };
        assert_eq!(shown("ties-ab", &a, &b), shown("ties-ba", &b, &a));
    }

    #[test]
    fn record_fails_open() {
        let home = tmp("open");
        std::fs::write(home.join("stats"), "a file where the directory should be").unwrap();
        record(&home, &ev(TURN_END, 1, "a@x", &[], 0));
        assert!(events(&home).is_empty());
    }

    #[test]
    fn backfill_derives_history_before_live_counting_once() {
        let home = tmp("backfill");
        let w = |f: &str, lines: &[serde_json::Value]| {
            let p = home.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, lines.iter().map(|l| l.to_string() + "\n").collect::<String>()).unwrap();
        };
        use serde_json::json;
        w(
            "episodes.jsonl",
            &[
                json!({"id":"e1","ms":100,"repo":"app","session":"s1","task":"t","agent":"a","rejected_tool":"Bash","correction":"no, not like that","engineer":"a@x"}),
                json!({"id":"e2","ms":5000,"repo":"app","session":"s2","task":"t","agent":"a","rejected_tool":null,"correction":"after live counting began","engineer":"a@x"}),
            ],
        );
        w(
            "outcomes.jsonl",
            &[
                json!({"kind":"asked","session":"s1","lessons":["L1","L2"],"ms":110,"repo":"app"}),
                // Free deliveries are counted live as `early` and `queued_free`: never a block here.
                json!({"kind":"asked","session":"s1","lessons":["L3"],"ms":112,"repo":"app","channel":"early"}),
                json!({"kind":"asked","session":"s1","lessons":["L4"],"ms":114,"repo":"app","channel":"queued"}),
                json!({"kind":"asked_then_changed","session":"s1","lessons":["L1"],"ms":120,"repo":"app"}),
            ],
        );
        w(
            "lessons.jsonl",
            &[json!({"id":"L1","origin_ms":90,"question":"q","version":1,"engineers":["a@x"]}), json!({"id":"L1","origin_ms":90,"question":"q2","version":2})],
        );
        w("queen/queue.jsonl", &[json!({"id":"Q1","ms":130,"question":"which?","engineer":"a@x","answered_ms":190,"answer":"this","answered_by":"a@x"})]);
        record(&home, &ev(TURN_END, 1000, "a@x", &[], 0));
        let n = backfill(&home);
        let all = read_events(&backfill_path(&home));
        assert_eq!(n, all.len());
        let kinds: Vec<&str> = all.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, [LESSON_NEW, CORRECTION, BLOCK, CHANGED, QUEEN_QUEUED, QUEEN_ANSWERED], "ordered by time, the later correction left out");
        let c = &all[1];
        assert_eq!((c.why.as_str(), c.text.as_str(), c.engineer.as_str()), ("rejected_tool", "Bash: no, not like that", "a@x"));
        assert_eq!(all[3].engineer, "a@x", "outcomes take the session's engineer from its corrections");
        assert_eq!(all[5].dur_ms, 60, "time to answer");
        assert_eq!(backfill(&home), 0, "once");
        assert_eq!(events(&home).len(), all.len() + 1, "history and live events together");
    }

    /// 0.7.10 and 0.7.11 counted the judge's changes live but not its ignores. The recount adds each ignore
    /// by today's judge since live counting began that no live event has, once; an old judge's ignore,
    /// a placebo's and one counted live stay out.
    #[test]
    fn recount_adds_the_ignores_live_counting_dropped_once() {
        let home = tmp("recount");
        use serde_json::json;
        let rec = |kind: &str, s: &str, lessons: &[&str], ms: i64, channel: &str, judge: u8| json!({"kind": kind, "session": s, "lessons": lessons, "ms": ms, "repo": "app", "channel": channel, "judge": judge});
        let log = [
            rec("asked_ignored", "s0", &["L0"], 50, "stop", 3),
            rec("asked_then_changed", "s1", &["L1"], 1100, "live", 3),
            rec("asked_ignored", "s1", &["L2", "L3"], 1100, "live", 3),
            rec("asked_ignored", "s1", &["L4"], 1100, "queued", 3),
            rec("asked_ignored", "s1", &["L5"], 1200, "live", 2),
            rec("asked_ignored", "s1", &["L6"], 1300, crate::outcome::PLACEBO, 3),
        ];
        std::fs::write(home.join("outcomes.jsonl"), log.iter().map(|l| l.to_string() + "\n").collect::<String>()).unwrap();
        record(&home, &ev(TURN_END, 1000, "a@x", &[], 0));
        record(&home, &Event { session: "s1".into(), ..ev(CHANGED, 1101, "a@x", &["L1"], 0) });
        record(&home, &Event { session: "s1".into(), ..ev(IGNORED, 1101, "a@x", &["L3"], 0) });
        assert_eq!(recount_ignores(&home), 2);
        let got: Vec<(i64, Vec<String>)> = read_events(&ignores_path(&home)).into_iter().map(|e| (e.ms, e.lessons)).collect();
        assert_eq!(got, [(1100, vec!["L2".to_string()]), (1100, vec!["L4".to_string()])], "before live counting, counted live, old judge, placebo: out");
        assert_eq!(recount_ignores(&home), 0, "once");
        assert_eq!(events(&home).iter().filter(|e| e.kind == IGNORED).count(), 3, "live and recounted together");
    }

    /// A stop's block asked lessons that two checks routed, a live ask delivered two checks' questions,
    /// and the judge's call wrote one verdict record per check: each is one record per check in the log
    /// and one event live, so one event in the history too, its lessons joined. Records of separate
    /// calls (another stamp) and results counted per record stay apart.
    #[test]
    fn backfill_counts_one_event_per_call_as_live_counting_does() {
        let home = tmp("backfill-call");
        use serde_json::json;
        let rec = |kind: &str, lessons: &[&str], ms: i64, channel: &str, check: &str| json!({"kind": kind, "session": "s1", "lessons": lessons, "ms": ms, "repo": "app", "channel": channel, "check": check});
        let log = [
            rec("asked", &["L1", "L2"], 110, "stop", "c1"),
            rec("asked", &["L3"], 110, "stop", "c2"),
            rec("asked", &["L4"], 120, "live", "c3"),
            rec("asked", &["L5"], 120, "live", "c4"),
            rec("asked_then_changed", &["L1"], 130, "stop", "c1"),
            rec("asked_then_changed", &["L3"], 130, "stop", "c2"),
            rec("asked", &["L6"], 140, "stop", "c5"),
            rec("asked_not_corrected", &["L2"], 150, "stop", "c1"),
            rec("asked_not_corrected", &["L6"], 150, "stop", "c5"),
        ];
        std::fs::write(home.join("outcomes.jsonl"), log.iter().map(|l| l.to_string() + "\n").collect::<String>()).unwrap();
        record(&home, &ev(TURN_END, 1000, "a@x", &[], 0));
        backfill(&home);
        let all: Vec<(String, i64, u64, Vec<String>)> = read_events(&backfill_path(&home)).into_iter().map(|e| (e.kind, e.ms, e.n, e.lessons)).collect();
        let row = |kind: &str, ms: i64, n: u64, lessons: &[&str]| (kind.to_string(), ms, n, lessons.iter().map(|l| l.to_string()).collect::<Vec<_>>());
        assert_eq!(
            all,
            [
                row(BLOCK, 110, 3, &["L1", "L2", "L3"]),
                row(LIVE_ASK, 120, 2, &["L4", "L5"]),
                row(CHANGED, 130, 0, &["L1", "L3"]),
                row(BLOCK, 140, 1, &["L6"]),
                row(NOT_CORRECTED, 150, 0, &["L2"]),
                row(NOT_CORRECTED, 150, 0, &["L6"]),
            ]
        );
    }

    /// `rollups` before its two passes: every line parsed whole, the newest version per id kept, ties by hash.
    fn rollups_whole(text: &str) -> BTreeMap<String, Bucket> {
        let mut out: BTreeMap<String, (Bucket, [u8; 32])> = BTreeMap::new();
        for l in text.lines() {
            let Ok(b) = serde_json::from_str::<Bucket>(l) else { continue };
            let h = *blake3::hash(l.as_bytes()).as_bytes();
            if out.get(&b.id).is_none_or(|(old, oh)| (b.version, h) > (old.version, *oh)) {
                out.insert(b.id.clone(), (b, h));
            }
        }
        out.into_iter().map(|(id, (b, _))| (id, b)).collect()
    }

    /// `stats.jsonl` lines at every edge a head and a whole parse could decide apart.
    fn bucket_edges() -> Vec<String> {
        let b = |id: &str, version: u64, n: u64| {
            serde_json::json!({"id": id, "version": version, "day": "2026-11-01", "node": "n", "engineer": "e", "counts": {"turn_end": n}, "usd": 0.5, "dur_ms": 1})
                .to_string()
        };
        let mut v = vec![
            b("A", 1, 1),
            b("A", 2, 2),
            b("A", 2, 3),
            b("A", 2, 4),
            // Newest, but no `Bucket` (no `counts`): the whole parse never sees it, so version 2 wins.
            r#"{"id":"A","version":9,"day":"d","node":"n","engineer":"e","usd":0,"dur_ms":0}"#.into(),
            // Newest twice, both no `Bucket`: the next version down.
            r#"{"id":"B","version":5,"counts":{}}"#.into(),
            r#"{"id":"B","version":5,"day":"d"}"#.into(),
            b("B", 4, 1),
            b("B", 3, 1),
            // Escaped id: the same id as `C`.
            b("C", 1, 1),
            b("C", 1, 1).replace(r#""id":"C""#, &format!(r#""id":"{}""#, crate::jsonl::escaped("C"))).replace(r#""version":1"#, r#""version":2"#),
            // Duplicate keys: a typed parse refuses them.
            b("D", 1, 1),
            b("D", 1, 1).replacen('{', r#"{"version":7,"#, 1),
            b("D", 1, 1).replacen('{', r#"{"id":"D","#, 1),
            b("D", 1, 1).replacen('{', r#"{"counts":{},"#, 1),
            // Version above u32, above u64, negative, a float, a string; an id that is no string.
            b("E", 5_000_000_000, 1),
            b("E", 1, 1).replace(r#""version":1"#, r#""version":18446744073709551616"#),
            b("E", 1, 1).replace(r#""version":1"#, r#""version":-1"#),
            b("E", 1, 1).replace(r#""version":1"#, r#""version":9.0"#),
            b("E", 1, 1).replace(r#""version":1"#, r#""version":"9""#),
            b("E", 1, 1).replace(r#""id":"E""#, r#""id":5"#),
            // A lone surrogate in a field only the whole parse reads, and in one neither reads.
            b("F", 1, 1),
            b("F", 2, 1).replace(r#""engineer":"e""#, r#""engineer":"\ud800""#),
            b("F", 3, 1).replacen('{', r#"{"x":"\ud800","#, 1),
            // Truncated, trailing characters, whitespace, empty, not an object.
            b("G", 1, 1),
            b("G", 2, 1).trim_end_matches('}').to_string(),
            b("G", 3, 1) + " x",
            b("G", 4, 1) + " ",
            " ".into(),
            "\t".into(),
            String::new(),
            "{}".into(),
            "[]".into(),
            "null".into(),
            r#"[{"id":"G","version":9}]"#.into(),
        ];
        // Identical lines of one version.
        v.extend([b("H", 1, 1), b("H", 1, 1)]);
        v
    }

    /// The two passes keep exactly what a whole parse of every line keeps, on the edges and on every
    /// line of a real store (`BORG_BENCH_STORE`), in file order and reversed. Each text is renamed in as a
    /// new file, so each is read whole: written in place at the same size within one ctime tick, the
    /// reversed text would be taken for the one before it.
    #[test]
    fn rollups_keep_what_a_whole_parse_keeps() {
        let home = tmp("rollups-oracle");
        let put = |text: &str| {
            let before = reads(&home).0;
            std::fs::write(home.join("tmp"), text).unwrap();
            std::fs::rename(home.join("tmp"), rollups_path(&home)).unwrap();
            let got = rollups(&home);
            assert_eq!(reads(&home).0, before + 1, "read whole");
            got
        };
        let real = crate::jsonl::store_lines(&["stats.jsonl"]);
        for mut lines in [bucket_edges(), real.clone(), [real, bucket_edges()].concat()] {
            for _ in 0..2 {
                let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
                assert_eq!(put(&text), rollups_whole(&text));
                lines.reverse();
            }
        }
        let got = put(&bucket_edges().iter().map(|l| format!("{l}\n")).collect::<String>());
        let got: Vec<(String, u64)> = got.into_iter().map(|(id, b)| (id, b.version)).collect();
        let got: Vec<(&str, u64)> = got.iter().map(|(id, v)| (id.as_str(), *v)).collect();
        assert_eq!(got, [("A", 2), ("B", 4), ("C", 2), ("D", 1), ("E", 5_000_000_000), ("F", 3), ("G", 4), ("H", 1)]);
    }

    /// `rollups` before it followed the file: read whole on every call, each line's head read, each id's
    /// newest line parsed whole, a newest line that is no `Bucket` giving way to the next.
    fn rollups_two_pass(text: &str) -> BTreeMap<String, Bucket> {
        let head = |l| match crate::jsonl::lead(l, ["id", "version"])? {
            [Scalar::Str(id), version] => Some((id, version.as_u64()?)),
            _ => None,
        };
        let mut out = BTreeMap::new();
        for (_, mut lines) in crate::jsonl::by_id(text, head) {
            lines.sort_by_key(|(v, _)| std::cmp::Reverse(*v));
            for tied in lines.chunk_by_mut(|a, b| a.0 == b.0) {
                if tied.len() > 1 {
                    tied.sort_by_cached_key(|(_, l)| std::cmp::Reverse(*blake3::hash(l.as_bytes()).as_bytes()));
                }
                if let Some(b) = tied.iter().find_map(|(_, l)| serde_json::from_str::<Bucket>(l).ok()) {
                    out.insert(b.id.clone(), b);
                    break;
                }
            }
        }
        out
    }

    /// How often this process read the `stats.jsonl` of `home` whole, and in part (`latest`).
    fn reads(home: &Path) -> (usize, usize) {
        let path = rollups_path(home);
        lock(&FOLLOWED).iter().find(|(p, _)| *p == path).map_or((0, 0), |(_, s)| {
            let f = lock(s);
            (f.whole, f.tail)
        })
    }

    fn append(path: &Path, text: &str) {
        std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap().write_all(text.as_bytes()).unwrap();
    }

    /// Oracle: followed as it grows, the rollups are what the two passes over the whole file give after
    /// every append, on the edges and on the lines of a real store (`BORG_BENCH_STORE`) appended in pieces
    /// that cut lines anywhere (torn lines, CRLF ends), then on an unterminated newest line completed,
    /// spoilt and ended later. In file order and reversed, so lines tied on a version reach a tail in both
    /// hash orders. The file is read whole once, then only in part.
    #[test]
    fn followed_rollups_keep_what_the_two_passes_keep() {
        let home = tmp("rollups-follow");
        let path = rollups_path(&home);
        let real = crate::jsonl::store_lines(&["stats.jsonl"]);
        let bucket = |id: &str, version: u64| serde_json::to_string(&Bucket { id: id.into(), version, ..Default::default() }).unwrap();
        let both = |lines: Vec<String>| [lines.clone(), lines.into_iter().rev().collect()];
        for lines in [both(bucket_edges()), both([real, bucket_edges()].concat())].into_iter().flatten() {
            let text: String = lines.iter().enumerate().map(|(i, l)| format!("{l}{}", if i % 7 == 3 { "\r\n" } else { "\n" })).collect();
            let _ = std::fs::remove_file(&path);
            append(&path, "");
            let (whole, tail) = reads(&home);
            assert_eq!(rollups(&home), BTreeMap::new());
            let (mut at, mut pieces) = (0, 0);
            while at < text.len() {
                let mut to = (at + text.len() / 40 + 13 * (pieces % 9) + 1).min(text.len());
                while !text.is_char_boundary(to) {
                    to += 1;
                }
                append(&path, &text[at..to]);
                (at, pieces) = (to, pieces + 1);
                assert_eq!(rollups(&home), rollups_two_pass(&text[..at]), "after {at} bytes");
            }
            let steps = [
                bucket("A", 100),
                "\n".into(),
                bucket("A", 101),
                " x".into(),
                "\n".into(),
                bucket("A", 102).trim_end_matches('}').into(),
                "}\r".into(),
                "\n".into(),
                bucket("Z", 1),
            ];
            for s in &steps {
                append(&path, s);
                let text = std::fs::read_to_string(&path).unwrap();
                assert_eq!(rollups(&home), rollups_two_pass(&text), "after {s:?}");
            }
            assert_eq!(reads(&home), (whole + 1, tail + pieces + steps.len()), "read whole once, then in part");
        }
    }

    /// The rollups follow `stats.jsonl` as borg-web's requests see it change: unchanged, nothing is read;
    /// appended to (a peer's merge, this node's `rollup`), only the tail; renamed over (the quarantine) or
    /// written in place at its size with a new mtime, all of it. A caller keeps the rollups it was given.
    #[test]
    fn rollups_read_an_append_in_part_and_a_rename_whole() {
        let home = tmp("rollups-reads");
        let path = rollups_path(&home);
        let line = |id: &str, version: u64, n: u64| {
            let mut b = Bucket { id: id.into(), version, node: "peer".into(), ..Default::default() };
            b.counts.insert(TURN_END.into(), n);
            serde_json::to_string(&b).unwrap() + "\n"
        };
        std::fs::write(&path, line("A", 1, 1)).unwrap();
        let held = latest(&home);
        let step = |what: &str, change: &dyn Fn(), want: (usize, usize)| {
            let before = reads(&home);
            change();
            assert_eq!(rollups(&home), rollups_two_pass(&std::fs::read_to_string(&path).unwrap()), "{what}");
            let after = reads(&home);
            assert_eq!((after.0 - before.0, after.1 - before.1), want, "{what}: (whole, tail) reads");
        };
        step("unchanged", &|| (), (0, 0));
        step("a line appended", &|| append(&path, &line("A", 2, 2)), (0, 1));
        step(
            "this node's rollup",
            &|| {
                record(&home, &ev(TURN_END, 10, "a@x", &[], 0));
                assert_eq!(rollup(&home), 1);
            },
            (0, 1),
        );
        step(
            "renamed over",
            &|| {
                std::fs::write(home.join("tmp"), line("A", 3, 3)).unwrap();
                std::fs::rename(home.join("tmp"), &path).unwrap();
            },
            (1, 0),
        );
        step(
            "the same size in place, a new mtime",
            &|| {
                let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                (&f).write_all(line("B", 3, 4).as_bytes()).unwrap();
                f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(1)).unwrap();
            },
            (1, 0),
        );
        assert_eq!(held.buckets().map(|b| (b.id.as_str(), b.version)).collect::<Vec<_>>(), [("A", 1)], "kept as given");
    }

    /// Requests at once on a store not read yet read its `stats.jsonl` whole once between them.
    #[test]
    fn requests_at_once_read_the_rollups_whole_once() {
        let home = tmp("rollups-at-once");
        let line = |i: u64| serde_json::to_string(&Bucket { id: format!("b{}", i % 20), version: i, ..Default::default() }).unwrap() + "\n";
        let text: String = (0..200).map(line).collect();
        std::fs::write(rollups_path(&home), &text).unwrap();
        let got: Vec<_> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..8).map(|_| s.spawn(|| rollups(&home))).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(got.iter().all(|r| *r == rollups_two_pass(&text)));
        assert_eq!(reads(&home), (1, 0));
    }

    /// The feed before `newest`: every event parsed whole, filtered, sorted, cut.
    fn newest_whole(home: &Path, kinds: &[&str], limit: usize) -> Vec<Event> {
        let mut evs: Vec<Event> = events(home).into_iter().filter(|e| kinds.contains(&e.kind.as_str())).collect();
        evs.sort_by_key(|e| std::cmp::Reverse(e.ms));
        evs.truncate(limit);
        evs
    }

    /// `newest` returns exactly what parsing every event whole returns, on edge lines and on the event
    /// logs of a real store (`BORG_BENCH_STORE`), for every limit around the logs' size.
    #[test]
    fn newest_is_the_whole_feed_cut() {
        let home = tmp("newest-oracle");
        let e = |kind: &str, ms: i64, why: &str| serde_json::to_string(&Event { why: why.into(), ..Event::new(kind, ms) }).unwrap();
        let edges = [
            e(CHANGED, 10, "first file"),
            e(CHANGED, 10, "tie"),
            e(BLOCK, 30, ""),
            // Newest, but no `Event` (`lessons` no array): gives way to the next.
            r#"{"ms":99,"kind":"changed","lessons":"L1"}"#.into(),
            // Escaped kind: a shown kind.
            format!(r#"{{"ms":40,"kind":"{}"}}"#, crate::jsonl::escaped(CHANGED)),
            // Duplicate keys, a kind that is no string, a missing kind, a float ms, truncated, empty.
            r#"{"ms":50,"kind":"changed","kind":"block"}"#.into(),
            r#"{"ms":50,"ms":51,"kind":"changed"}"#.into(),
            r#"{"ms":60,"kind":5}"#.into(),
            r#"{"ms":60}"#.into(),
            r#"{"ms":60.5,"kind":"changed"}"#.into(),
            r#"{"ms":61,"kind":"changed""#.into(),
            String::new(),
            " ".into(),
            e(TURN_END, 70, "not shown"),
        ];
        let real = |f: &str| crate::jsonl::store_lines(&[f]);
        let files = [backfill_path(&home), events_path(&home), ignores_path(&home)];
        std::fs::create_dir_all(home.join("stats")).unwrap();
        let sets = [
            [edges[..2].to_vec(), edges[1..].to_vec(), edges.to_vec()],
            [real("stats/backfill.jsonl"), real("stats/events.jsonl"), real("stats/ignores.jsonl")],
        ];
        let kinds = [CORRECTION, VETO, BLOCK, CHANGED, IGNORED, LESSON_NEW, MODEL_CALL, MSG_SENT, QUEEN_QUEUED];
        for set in sets {
            for (f, lines) in files.iter().zip(&set) {
                std::fs::write(f, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
            }
            let n: usize = set.iter().map(Vec::len).sum();
            for limit in [0, 1, 2, 5, 200, 2000, n, n + 1] {
                assert_eq!(newest(&home, &kinds, limit), newest_whole(&home, &kinds, limit), "limit {limit}");
            }
        }
    }

    #[test]
    fn node_id_is_stable() {
        let home = tmp("node");
        let a = node(&home);
        assert_eq!(a.len(), 16);
        assert_eq!(node(&home), a);
    }

    /// `fold` before `Folder`: a `format!` of the day and the bucket id per event, a key copied per count.
    fn fold_owned(node: &str, events: &[Event]) -> BTreeMap<String, Bucket> {
        let mut out: BTreeMap<String, Bucket> = BTreeMap::new();
        for e in events {
            let day = day(e.ms);
            let id = format!("stat:{node}:{}:{day}", e.engineer);
            let b = out.entry(id.clone()).or_insert_with(|| Bucket { id, day, node: node.into(), engineer: e.engineer.clone(), ..Default::default() });
            let kind = if e.kind == CORRECTION && e.why == crate::learn::ANSWER { ANSWERED } else { e.kind.as_str() };
            *b.counts.entry(kind.to_string()).or_default() += 1;
            if e.n > 0 {
                *b.counts.entry(format!("{}_n", e.kind)).or_default() += e.n;
            }
            let why = match (e.kind.as_str(), e.why.as_str()) {
                (GATE_SKIP, w @ ("noedit" | "empty" | "live_covered")) => w,
                (GATE_SKIP, w) if w.parse::<f64>().is_ok() => "score",
                (GATE_SKIP, w) if !w.is_empty() => "other",
                (CHECK | LIVE_CHECK, "failed") => "failed",
                (CHECK_DROPPED, w) if !w.is_empty() => w,
                (LIVE_ASK, _) if e.mode == crate::check_async::MODE => crate::check_async::MODE,
                _ => "",
            };
            if !why.is_empty() {
                *b.why.entry(format!("{}:{why}", e.kind)).or_default() += 1;
            }
            if e.kind == CHECK && e.dur_ms > 0 {
                *b.why.entry("check_ms".into()).or_default() += e.dur_ms;
                *b.why.entry("check_timed".into()).or_default() += 1;
            }
            b.usd += e.usd;
            if e.usd > 0.0 {
                *b.usd_by.entry(e.kind.clone()).or_default() += e.usd;
            }
            if e.usd > 0.0 && (e.kind == CHECK || e.kind == LIVE_CHECK) {
                let mode = if e.mode.is_empty() { "hook" } else { e.mode.as_str() };
                *b.usd_mode.entry(mode.into()).or_default() += e.usd;
            }
            if !e.channel.is_empty() {
                *b.channels.entry(format!("{}@{}", e.kind, e.channel)).or_default() += 1;
            }
            b.dur_ms += e.dur_ms;
            for l in &e.lessons {
                *b.lessons.entry(l.clone()).or_default().entry(e.kind.clone()).or_default() += 1;
            }
        }
        out
    }

    /// `current` before it folded as read: every local event kept in a `Vec<Event>`, then `fold_owned`.
    fn current_owned(home: &Path) -> Vec<Bucket> {
        backfill(home);
        recount_ignores(home);
        let me = node(home);
        let mut all: BTreeMap<String, Bucket> = rollups(home).into_iter().filter(|(_, b)| b.node != me).collect();
        all.extend(fold_owned(&me, &events(home)));
        all.into_values().collect()
    }

    /// Event lines at every edge `Event` and `EventRef` could decide apart, and through every branch of the
    /// fold.
    fn event_edges() -> Vec<String> {
        let e = r#"{"ms":1700000000000,"kind":"check","engineer":"a@x","lessons":["L1","L2"],"n":2,"usd":0.5,"dur_ms":300,"why":"failed","mode":"async","channel":"stop"}"#;
        let esc = crate::jsonl::escaped;
        let with = |field: &str| e.replacen('{', &format!("{{{field},"), 1);
        let mut out: Vec<String> = vec![
            e.into(),
            // Escapes in every string the fold reads.
            format!(
                r#"{{"ms":1700000000001,"kind":"{}","engineer":"{}","lessons":["{}"],"why":"{}","mode":"{}","channel":"{}"}}"#,
                esc("check"),
                esc("a@x"),
                esc("L1"),
                esc("failed"),
                esc("async"),
                esc("stop")
            ),
            // Duplicate keys, read and unread and unknown.
            with(r#""kind":"veto""#),
            with(r#""text":"a","text":"b""#),
            with(r#""x":1,"x":2"#),
            // Wrong types in fields read and unread; numbers past their type.
            with(r#""session":5"#),
            with(r#""text":null"#),
            with(r#""repo":[]"#),
            with(r#""source":{}"#),
            with(r#""text":"\ud800""#),
            e.replace(r#""ms":1700000000000"#, r#""ms":"1""#),
            e.replace(r#""ms":1700000000000"#, r#""ms":1.5"#),
            e.replace(r#""ms":1700000000000"#, r#""ms":9223372036854775808"#),
            e.replace(r#""ms":1700000000000"#, r#""ms":9223372036854775807"#),
            e.replace(r#""ms":1700000000000"#, r#""ms":-86400001"#),
            e.replace(r#""n":2"#, r#""n":-1"#),
            e.replace(r#""n":2"#, r#""n":5000000000"#),
            e.replace(r#""n":2"#, r#""n":18446744073709551616"#),
            e.replace(r#""usd":0.5"#, r#""usd":"1""#),
            e.replace(r#""usd":0.5"#, r#""usd":-0.0"#),
            with(r#""rss_kb":-1"#),
            with(r#""limit_ms":1e3"#),
            e.replace(r#"["L1","L2"]"#, r#""L1""#),
            e.replace(r#"["L1","L2"]"#, r#"["L1",2]"#),
            e.replace(r#""kind":"check","#, ""),
            e.replace(r#""kind":"check""#, r#""kind":null"#),
            // The positional form serde reads a struct from too.
            r#"[1700000000000,"check"]"#.into(),
            r#"[1700000000000,"check","s","a@x","r",["L1"],3]"#.into(),
            r#"[1700000000000]"#.into(),
            // Truncated, trailing characters, whitespace, empty, not an object.
            e.trim_end_matches('}').into(),
            format!("{e} x"),
            format!("{e} "),
            " ".into(),
            String::new(),
            "{}".into(),
            "[]".into(),
            "null".into(),
        ];
        // Each branch of the reasons, costs and channels.
        for (kind, why, mode, usd) in [
            (GATE_SKIP, "noedit", "", 0.0),
            (GATE_SKIP, "0.25", "", 0.0),
            (GATE_SKIP, "zzz", "", 0.0),
            (GATE_SKIP, "", "", 0.0),
            (CHECK_DROPPED, crate::check_async::WHY_GONE, "", 0.0),
            (LIVE_ASK, "", crate::check_async::MODE, 0.0),
            (LIVE_CHECK, "failed", "", 0.25),
            (CHECK, "", "", 0.125),
            (CORRECTION, crate::learn::ANSWER, "", 0.0),
            (MODEL_CALL, "", "", 0.75),
        ] {
            out.push(
                serde_json::json!({"ms": 1_700_086_400_000_i64, "kind": kind, "engineer": "b@x", "why": why, "mode": mode, "usd": usd, "dur_ms": 5})
                    .to_string(),
            );
        }
        out
    }

    /// The local event logs of the store `BORG_BENCH_STORE` names, line by line (none when unset).
    const LOCAL: [&str; 3] = ["stats/backfill.jsonl", "stats/events.jsonl", "stats/ignores.jsonl"];

    /// `EventRef` takes exactly the lines `Event` takes, and `Folder` folds each, and all of them in a row,
    /// exactly as `fold_owned` did, on the edges and every local event line of a real store.
    #[test]
    fn the_borrowed_fold_folds_what_the_owned_one_did() {
        let lines = [event_edges(), crate::jsonl::store_lines(&LOCAL)].concat();
        let mut all = Vec::new();
        let mut f = Folder::new("n");
        for line in &lines {
            let owned = serde_json::from_str::<Event>(line).ok();
            let borrowed = serde_json::from_str::<EventRef>(line).ok();
            assert_eq!(owned.is_some(), borrowed.is_some(), "{line:?}");
            if let (Some(o), Some(r)) = (owned, borrowed) {
                let mut one = Folder::new("n");
                one.add(&r.head());
                assert_eq!(one.out, fold_owned("n", std::slice::from_ref(&o)), "{line:?}");
                f.add(&r.head());
                all.push(o);
            }
        }
        assert_eq!(f.out, fold_owned("n", &all));
        assert_eq!(fold("n", &all), fold_owned("n", &all), "the owned events' fold");
    }

    /// `current` gives exactly what it gave when it kept every event, on a store of the edges beside a
    /// peer's rollup, and on a copy of a real store's rollups and local events (`BORG_BENCH_STORE`).
    #[test]
    fn current_folds_as_read_what_it_folded_whole() {
        let home = tmp("current-oracle");
        std::fs::create_dir_all(home.join("stats")).unwrap();
        std::fs::write(home.join("stats").join("node"), "me").unwrap();
        std::fs::write(backfill_path(&home), "").unwrap();
        std::fs::write(ignores_path(&home), event_edges().join("\n")).unwrap();
        std::fs::write(events_path(&home), event_edges().join("\n") + "\n").unwrap();
        let peer = Bucket { id: "stat:other:b@x:2023-11-14".into(), version: 1, node: "other".into(), ..Default::default() };
        std::fs::write(rollups_path(&home), serde_json::to_string(&peer).unwrap() + "\n").unwrap();
        let edges = current(&home);
        assert_eq!(edges, current_owned(&home));
        assert!(edges.len() > 2 && edges.iter().any(|b| b.node == "other"), "{edges:?}");
        if let Some(src) = std::env::var_os("BORG_BENCH_STORE").map(PathBuf::from) {
            let real = tmp("current-oracle-real");
            std::fs::create_dir_all(real.join("stats")).unwrap();
            for f in LOCAL.iter().chain(&["stats/node", "stats.jsonl"]) {
                let _ = std::fs::copy(src.join(f), real.join(f));
            }
            assert_eq!(current(&real), current_owned(&real));
            let _ = std::fs::remove_dir_all(real);
        }
        let _ = std::fs::remove_dir_all(home);
    }

    /// `each_live` hands over exactly the events `live` reads, in order, and `live_since` finds what it
    /// found from them (a repeated veto, dated earlier than its line, never the start), on the edges and
    /// a real store's live log (`BORG_BENCH_STORE`).
    #[test]
    fn the_live_events_read_borrowed_are_the_owned_ones() {
        let home = tmp("each-live");
        std::fs::create_dir_all(home.join("stats")).unwrap();
        // Dated before every other line, so only skipping it keeps it from being the start.
        let said = -10_i64.pow(15);
        let repeat = serde_json::to_string(&Event::new(VETO_REPEAT, said)).unwrap();
        let real = crate::jsonl::store_lines(&["stats/events.jsonl"]);
        for (i, lines) in [event_edges(), [vec![repeat], event_edges(), real].concat()].into_iter().enumerate() {
            std::fs::write(events_path(&home), lines.join("\n")).unwrap();
            let mut got = Vec::new();
            each_live(&home, |e| got.push((e.ms, e.kind.to_string(), e.why.to_string())));
            let live = live(&home);
            assert_eq!(got, live.iter().map(|e| (e.ms, e.kind.clone(), e.why.clone())).collect::<Vec<_>>());
            assert_eq!(live_since(&home), live.iter().filter(|e| e.kind != VETO_REPEAT).map(|e| e.ms).min());
            assert_ne!(live_since(&home), Some(said), "the repeated veto");
            if i == 0 {
                assert_eq!(live_since(&home), Some(-86_400_001), "the earliest edge");
            }
        }
        let _ = std::fs::remove_dir_all(home);
    }
}
