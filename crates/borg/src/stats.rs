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

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A turn end the conscience saw (one per Stop that was not the rework after its own block).
pub const TURN_END: &str = "turn_end";
/// The free gate skipped the model call (`why`: the score, or `noedit`).
pub const GATE_SKIP: &str = "gate_skip";
/// One turn-end model call (`n`: lessons shown, `usd`, `dur_ms`; `why: "failed"` when it gave no answer).
pub const CHECK: &str = "check";
/// A block: one extra round of agent work (`n`: questions, `lessons`: the asked ids).
pub const BLOCK: &str = "block";
/// Live mode (live.rs): one model call during the turn (`n`, `usd`, `dur_ms`, `why: "failed"`, as `check`).
pub const LIVE_CHECK: &str = "live_check";
/// Live mode: grade-3 lessons a live check asked, delivered with the next prompt or tool call (`n`,
/// `lessons`). Never an extra round: the drone answers as it works.
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
/// The engineer corrected the drone (an episode): `why` is `rejected_tool` or `pushback`, `text` a snippet.
pub const CORRECTION: &str = "correction";
/// The engineer said no (`borg no`, veto.rs): `lessons` the lessons shown so far (credited `vetoed`),
/// `n` their number, `why` `cli`, `prompt` or `mesh`, `text` the reason.
pub const VETO: &str = "veto";
/// `borg learn` merged a veto into a lesson an earlier veto made: the same "no" said twice.
pub const VETO_REPEAT: &str = "veto_repeat";
/// A lesson written (`why`: its source, session/import/queen/veto) or merged into an existing one.
pub const LESSON_NEW: &str = "lesson_new";
pub const LESSON_MERGED: &str = "lesson_merged";
/// A review step's finding that a fix step fixed before any commit, recorded as an episode
/// (review_fix.rs; `why`: its severity, `text`: its redacted title, for this machine's feed only).
pub const REVIEW_FINDING: &str = "review_finding";
/// `borg learn` judged an episode not to be a lesson.
pub const NOT_A_LESSON: &str = "not_a_lesson";
/// A model call outside the turn end (`why`: learn, assimilate, facets, expert; `usd`, `dur_ms`).
pub const MODEL_CALL: &str = "model_call";
/// Steering: a directive issued, shown to a drone (`why`: say/pause/resume), refused on receipt
/// (`why`: the reason), and a tool call refused because the drone was paused (`why`: the tool).
pub const STEER_ISSUED: &str = "steer_issued";
pub const STEER_DELIVERED: &str = "steer_delivered";
pub const STEER_DROPPED: &str = "steer_dropped";
pub const PAUSE_REFUSED: &str = "pause_refused";
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
/// The hive (hive/mod.rs; `why`: the goal, `text`: the task): a claim taken, a task's oracle passed and
/// its `done` published, a stop blocked because the oracle failed, a claim released at the session's
/// end, and board context shown to a drone (`lessons`: the contract ids shown).
pub const HIVE_CLAIM: &str = "hive_claim";
/// L1 playbooks (playbook.rs): versions shown to a drone (`lessons`: their keys, `n`), an edit proposed
/// to the engineer from verified work, and one promoted by an engineer's answer (`lessons`: the
/// playbook, `why`: add/modify/remove, `text`: the step).
pub const PLAYBOOK_SHOWN: &str = "playbook_shown";
pub const PLAYBOOK_PROPOSED: &str = "playbook_proposed";
pub const PLAYBOOK_PROMOTED: &str = "playbook_promoted";
pub const HIVE_DONE: &str = "hive_done";
pub const HIVE_ORACLE_FAIL: &str = "hive_oracle_fail";
pub const HIVE_RELEASED: &str = "hive_released";
pub const HIVE_SHOWN: &str = "hive_shown";
/// A message between hive drones sent (`borg hive ask|tell`, hive/message.rs; `why`: the goal, `text`:
/// the kind, recipient and an excerpt).
pub const HIVE_MESSAGE: &str = "hive_message";
/// An integration of a hive goal (hive/integrate.rs; `why`: the goal, `text`: the integrate task and its
/// note): the accepted attempts merged and the oracle run on the combined tree, `n` 1 on a pass, 0 on a
/// failure.
pub const HIVE_INTEGRATE: &str = "hive_integrate";
/// A hive goal's plan decided (hive/plan.rs; `why`: the goal, `n` 1 approved or 0 refused, `text` how
/// and the answer): by the engineer's answer to its queen question, or `--yes`.
pub const HIVE_PLAN: &str = "hive_plan";
/// A hive task's verdict, once per attempt (hive/verdict.rs; `why`: the goal, `text`: the task and an
/// excerpt of the note, `lessons`: the lessons credited with it, `n` their number): verified, rejected
/// (by its verifier or the hidden oracle), or hacked (weakened tests or changed CI, or the oracle
/// touched). With a hidden oracle, counted when it scored. `task_hacked` over all three is the hacked
/// rate (borg-web's overview).
pub const TASK_VERIFIED: &str = "task_verified";
pub const TASK_REJECTED: &str = "task_rejected";
pub const TASK_HACKED: &str = "task_hacked";

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
    /// The delivery mode the hook ran in (live.rs: `hook` or `live`); empty outside a hook.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mode: String,
    /// How an asked lesson reached the drone: `stop` (a block), `live` (a live check), `early`, `queued`.
    /// On asks and on their results (`changed`, `ignored`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel: String,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

fn is_zero_f(v: &f64) -> bool {
    *v == 0.0
}

impl Event {
    /// An event now, in the delivery mode of this process (the hook exports `BORG_MODE`, main.rs).
    pub fn new(kind: &str, ms: i64) -> Self {
        let mode = std::env::var("BORG_MODE").map(|_| crate::live::current().name().to_string()).unwrap_or_default();
        Event { kind: kind.into(), ms, mode, ..Default::default() }
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
    /// The conscience's model cost (`check` and `live_check`) per delivery mode (`hook`, `live`). A
    /// split of part of `usd`, so never stacked with `usd_by`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub usd_mode: BTreeMap<String, f64>,
    /// Events per `<kind>@<channel>` for events that name a channel (`block@stop`, `changed@live`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub channels: BTreeMap<String, u64>,
}

impl Bucket {
    /// Equal counts, cost and lessons: nothing to publish (`id` and `version` aside).
    fn same(&self, other: &Bucket) -> bool {
        self.counts == other.counts
            && self.lessons == other.lessons
            && self.channels == other.channels
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

/// The first `max` characters of `text` on one line (a feed excerpt).
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

/// Every local event: the history derived once from the older logs (`backfill`) and the live ones.
pub fn events(home: &Path) -> Vec<Event> {
    let mut all = read_events(&backfill_path(home));
    all.extend(read_events(&events_path(home)));
    all
}

pub fn backfill_path(home: &Path) -> PathBuf {
    home.join("stats").join("backfill.jsonl")
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
    let cutoff = read_events(&events_path(home)).iter().map(|e| e.ms).min().unwrap_or_else(crate::learn::now_ms);
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
        let snippet = excerpt(&s(&e, "correction"), 160);
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
        out.push(Event {
            ms: ms(&e, "ms"),
            kind: CORRECTION.into(),
            session: s(&e, "session"),
            engineer: s(&e, "engineer"),
            repo: s(&e, "repo"),
            why: if tool.is_empty() { "pushback".into() } else { "rejected_tool".into() },
            text: if tool.is_empty() { snippet } else { format!("{tool}: {snippet}") },
            ..Default::default()
        });
    }
    let engineer = |session: &str| who.get(session).cloned().unwrap_or_default();
    for o in lines(&home.join("outcomes.jsonl")) {
        let live = o["channel"] == "live";
        let kind = match o["kind"].as_str().unwrap_or_default() {
            crate::outcome::ASKED if live => LIVE_ASK,
            crate::outcome::ASKED => BLOCK,
            crate::outcome::CHANGED => CHANGED,
            crate::outcome::IGNORED => IGNORED,
            crate::outcome::CORRECTED => CORRECTED,
            crate::outcome::NOT_CORRECTED => NOT_CORRECTED,
            crate::outcome::REGRETTED => REGRETTED,
            _ => continue,
        };
        let lessons = ids(&o, "lessons");
        let session = s(&o, "session");
        out.push(Event {
            ms: ms(&o, "ms"),
            kind: kind.into(),
            engineer: engineer(&session),
            session,
            repo: s(&o, "repo"),
            n: if kind == BLOCK || kind == LIVE_ASK { lessons.len() as u64 } else { 0 },
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
        out.push(Event { ms: ms(q, "ms"), kind: QUEEN_QUEUED.into(), text: excerpt(&s(q, "question"), 160), ..base() });
        if let Some(at) = q["answered_ms"].as_i64() {
            let dur = u64::try_from(at - ms(q, "ms")).unwrap_or(0);
            out.push(Event { ms: at, kind: QUEEN_ANSWERED.into(), dur_ms: dur, engineer: s(q, "answered_by"), text: excerpt(&s(q, "answer"), 160), ..base() });
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
    // Directives this node issued (the mesh outbox) and those it accepted (the inbox).
    for (file, kind) in [(home.join("mesh").join("directives.jsonl"), STEER_ISSUED), (home.join("steer").join("inbox.jsonl"), STEER_DELIVERED)] {
        for d in lines(&file) {
            out.push(Event {
                ms: ms(&d, "ms"),
                kind: kind.into(),
                engineer: s(&d, "engineer"),
                why: s(&d, "kind"),
                text: excerpt(&s(&d, "text"), 160),
                ..Default::default()
            });
        }
    }
    for d in lines(&home.join("steer").join("dropped.jsonl")) {
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
    (a..=b).map(|d| day(d * 86_400_000)).collect()
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` date (Howard Hinnant's days_from_civil).
fn day_number(date: &str) -> Option<i64> {
    let mut it = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (it.next()??, it.next()??, it.next()??);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// The local events folded into this node's buckets, keyed by bucket id (version 0).
pub fn fold(node: &str, events: &[Event]) -> BTreeMap<String, Bucket> {
    let mut out: BTreeMap<String, Bucket> = BTreeMap::new();
    for e in events {
        let day = day(e.ms);
        let id = format!("stat:{node}:{}:{day}", e.engineer);
        let b = out.entry(id.clone()).or_insert_with(|| Bucket { id, day, node: node.into(), engineer: e.engineer.clone(), ..Default::default() });
        *b.counts.entry(e.kind.clone()).or_default() += 1;
        if e.n > 0 {
            *b.counts.entry(format!("{}_n", e.kind)).or_default() += e.n;
        }
        b.usd += e.usd;
        if e.usd > 0.0 {
            *b.usd_by.entry(e.kind.clone()).or_default() += e.usd;
        }
        // Checks from before modes existed were all turn-end checks: hook mode.
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

/// The latest version of every bucket in `stats.jsonl` (every node's, after the mesh merged them). Two
/// lines of one version (two rollups at once) are ranked as borg-mesh lines.rs `settle` ranks them, by
/// the line's blake3 hash, so every node shows the same one whatever order the lines arrived in.
pub fn rollups(home: &Path) -> BTreeMap<String, Bucket> {
    let mut out: BTreeMap<String, (Bucket, [u8; 32])> = BTreeMap::new();
    for l in std::fs::read_to_string(rollups_path(home)).unwrap_or_default().lines() {
        let Ok(b) = serde_json::from_str::<Bucket>(l) else { continue };
        let h = *blake3::hash(l.as_bytes()).as_bytes();
        if out.get(&b.id).is_none_or(|(old, oh)| (b.version, h) > (old.version, *oh)) {
            out.insert(b.id.clone(), (b, h));
        }
    }
    out.into_iter().map(|(id, (b, _))| (id, b)).collect()
}

/// Publishes this node's buckets whose counts changed since their last line in `stats.jsonl`, each
/// with the next version. Returns how many lines it appended (0 when nothing changed).
pub fn rollup(home: &Path) -> usize {
    backfill(home);
    // No events, nothing to publish: and no node id created in a store that never counted anything.
    let evs = events(home);
    if evs.is_empty() {
        return 0;
    }
    let me = node(home);
    let published = rollups(home);
    let fresh: Vec<Bucket> = fold(&me, &evs)
        .into_values()
        .filter_map(|mut b| {
            let old = published.get(&b.id);
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
    let me = node(home);
    let mut all: BTreeMap<String, Bucket> = rollups(home).into_iter().filter(|(_, b)| b.node != me).collect();
    all.extend(fold(&me, &events(home)));
    all.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_first_calls_agree_on_one_node() {
        // Board lines are stamped with the node and only this node's claims are released after a
        // killed run: a runner and a drone's hook creating the id at once must not get two.
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

    const DAY: i64 = 86_400_000;

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

    #[test]
    fn node_id_is_stable() {
        let home = tmp("node");
        let a = node(&home);
        assert_eq!(a.len(), 16);
        assert_eq!(node(&home), a);
    }
}
