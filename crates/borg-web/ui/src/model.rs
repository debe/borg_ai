//! The Cube's data: `/api/mesh` (borg-web mesh.rs) parsed borrowed from the response text and kept as
//! one owned model. Strings the server names by index (node keys, sessions) and lesson ids are
//! interned once into `strs`; a refetch with `since` brings the machines and drones anew and the
//! events from `since` on, which `merge` joins without the ones it already has. The model holds the
//! server's window and no more: an event that falls out of it is forgotten.

use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;

use serde::Deserialize;

#[derive(Deserialize)]
struct Body<'a> {
    #[serde(default)]
    now: f64,
    #[serde(default)]
    me: u32,
    #[serde(borrow, default)]
    strs: Vec<Cow<'a, str>>,
    #[serde(borrow, default)]
    nodes: Vec<NodeIn<'a>>,
    #[serde(default)]
    links: Vec<LinkIn>,
    #[serde(borrow, default)]
    drones: Vec<DroneIn<'a>>,
    #[serde(borrow, default)]
    events: Vec<EvIn<'a>>,
}

#[derive(Deserialize)]
struct NodeIn<'a> {
    key: u32,
    #[serde(borrow, default)]
    host: Cow<'a, str>,
    #[serde(borrow, default)]
    engineer: Option<Cow<'a, str>>,
    #[serde(default)]
    me: bool,
    #[serde(borrow, default)]
    borg: Option<Cow<'a, str>>,
    #[serde(default)]
    protocol: Option<u32>,
    #[serde(default = "yes")]
    syncs: bool,
}

const fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct LinkIn {
    a: u32,
    b: u32,
    #[serde(default)]
    dial: bool,
    #[serde(default)]
    ms: Option<f64>,
    #[serde(default = "yes")]
    ok: bool,
}

#[derive(Deserialize)]
struct DroneIn<'a> {
    session: u32,
    node: u32,
    #[serde(borrow, default)]
    name: Cow<'a, str>,
    #[serde(default)]
    number: u32,
    #[serde(borrow, default)]
    state: Cow<'a, str>,
    #[serde(borrow, default)]
    repo: Cow<'a, str>,
    #[serde(borrow, default)]
    harness: Cow<'a, str>,
    #[serde(borrow, default)]
    engineer: Cow<'a, str>,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    started: f64,
    #[serde(default)]
    ms: f64,
}

#[derive(Deserialize)]
struct EvIn<'a> {
    #[serde(borrow)]
    id: Cow<'a, str>,
    ms: f64,
    #[serde(borrow)]
    kind: Cow<'a, str>,
    from_n: u32,
    #[serde(default)]
    from_s: Option<u32>,
    #[serde(borrow, default)]
    to: Option<Cow<'a, str>>,
    #[serde(default)]
    to_n: Option<u32>,
    #[serde(default)]
    to_s: Option<u32>,
    #[serde(borrow, default)]
    re: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    text: Cow<'a, str>,
    /// A lesson's id (`lesson`, `lesson_update`, `absorb`, `release`).
    #[serde(borrow, default)]
    lesson: Option<Cow<'a, str>>,
    /// What a hand-off event did (`offer`, `taken`, `back`, `declined`, `aborted`, `done`).
    #[serde(borrow, default)]
    why: Option<Cow<'a, str>>,
    /// Lines a mesh session moved (`sync`): the server counts them in u64.
    #[serde(default)]
    n: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub key: u32,
    pub host: String,
    pub engineer: Option<String>,
    pub me: bool,
    pub borg: Option<String>,
    pub protocol: Option<u32>,
    pub syncs: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub a: u32,
    pub b: u32,
    pub dial: bool,
    pub ms: Option<f64>,
    pub ok: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Drone {
    pub session: u32,
    pub node: u32,
    pub name: String,
    pub number: u32,
    pub state: String,
    pub repo: String,
    pub harness: String,
    pub engineer: String,
    pub paused: bool,
    pub started: f64,
    pub ms: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EvKind {
    Tell,
    Ask,
    Pause,
    Resume,
    Veto,
    Seen,
    /// A lesson written to the collective, new or a new version of one (`lesson_update`).
    Lesson,
    LessonUpdate,
    /// A lesson handed over to an outside store, or taken back (absorb.rs).
    Absorb,
    Release,
    /// A hand-off event (`why` says which).
    Handoff,
    /// A mesh session of this node with `to_n` that moved `n` lines, or failed.
    Sync,
    SyncFailed,
}

impl EvKind {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "tell" => Self::Tell,
            "ask" => Self::Ask,
            "pause" => Self::Pause,
            "resume" => Self::Resume,
            "veto" => Self::Veto,
            "seen" => Self::Seen,
            "lesson" => Self::Lesson,
            "lesson_update" => Self::LessonUpdate,
            "absorb" => Self::Absorb,
            "release" => Self::Release,
            "handoff" => Self::Handoff,
            "sync" => Self::Sync,
            "sync_failed" => Self::SyncFailed,
            _ => return None,
        })
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Tell => "tell",
            Self::Ask => "ask",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Veto => "veto",
            Self::Seen => "shown",
            Self::Lesson => "lesson",
            Self::LessonUpdate => "updated",
            Self::Absorb => "absorbed",
            Self::Release => "released",
            Self::Handoff => "hand-off",
            Self::Sync => "sync",
            Self::SyncFailed => "sync failed",
        }
    }

    /// A message between sessions (not a delivery): what the transmissions counter counts.
    pub const fn transmits(self) -> bool {
        matches!(self, Self::Tell | Self::Ask)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum To {
    All,
    Local,
    Engineer,
    Session,
}

/// What the scene needs to play an event: who sent it to whom, no text (scene.rs `play`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Play {
    pub kind: EvKind,
    pub from_n: u32,
    pub from_s: Option<u32>,
    pub to: To,
    pub to_n: Option<u32>,
    pub to_s: Option<u32>,
    /// Lines a sync moved; 0 otherwise.
    pub n: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ev {
    pub ms: f64,
    pub text: String,
    /// The lesson an event names (interned), and what a hand-off event did.
    pub lesson: Option<u32>,
    pub why: Option<String>,
    pub play: Play,
    /// The merge that added it (`Model::added`).
    seq: u32,
}

/// Derived: the server's window, the messages' lifetime (msg.rs `TTL_MS`), 7 days.
const WINDOW_MS: f64 = 7.0 * 86_400_000.0;

#[derive(Default)]
pub struct Model {
    /// Shared with `idx`: each string is kept once.
    pub strs: Vec<Rc<str>>,
    idx: HashMap<Rc<str>, u32>,
    pub now: f64,
    pub me: u32,
    pub nodes: Vec<Node>,
    pub links: Vec<Link>,
    pub drones: Vec<Drone>,
    /// Oldest first.
    pub events: Vec<Ev>,
    /// The events held by kind, id and what they answer (`key`), with their time to forget them with
    /// the event: a refetch's overlap adds nothing. Event ids stay out of `strs`, which never shrinks.
    held: HashMap<Box<str>, f64>,
    /// The last merge, and the oldest event it added.
    seq: u32,
    added_from: f64,
}

/// An event's identity: its kind, its id, and the message it answers (a `seen` names who was shown
/// in `id`). Written into `buf`, so an event already held costs no allocation.
fn key<'b>(buf: &'b mut String, kind: &str, id: &str, re: Option<&str>) -> &'b str {
    buf.clear();
    for part in [kind, id, re.unwrap_or("")] {
        buf.push_str(part);
        buf.push('\0');
    }
    buf
}

impl Model {
    pub fn str(&self, i: u32) -> &str {
        self.strs.get(i as usize).map_or("", |s| s)
    }

    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.idx.get(s) {
            return i;
        }
        let i = u32::try_from(self.strs.len()).unwrap_or(u32::MAX);
        let s: Rc<str> = Rc::from(s);
        self.strs.push(Rc::clone(&s));
        self.idx.insert(s, i);
        i
    }

    /// The events the last `merge` added, oldest first.
    pub fn added(&self) -> impl Iterator<Item = &Ev> {
        let from = self.events.partition_point(|e| e.ms < self.added_from);
        self.events[from..].iter().filter(move |e| e.seq == self.seq)
    }

    /// Takes a response: machines, links and drones replace the held ones, new events join the held in
    /// time order (`added` lists them), and events older than the window are forgotten. Returns how
    /// many it added, or None when the text is no view.
    pub fn merge(&mut self, text: &str) -> Option<usize> {
        let b: Body = serde_json::from_str(text).ok()?;
        // The response's string table in the model's.
        let map: Vec<u32> = b.strs.iter().map(|s| self.intern(s)).collect();
        let g = |i: u32| map.get(i as usize).copied().unwrap_or(u32::MAX);
        self.now = b.now;
        self.me = g(b.me);
        self.nodes = b
            .nodes
            .into_iter()
            .map(|n| Node {
                key: g(n.key),
                host: n.host.into_owned(),
                engineer: n.engineer.map(Cow::into_owned),
                me: n.me,
                borg: n.borg.map(Cow::into_owned),
                protocol: n.protocol,
                syncs: n.syncs,
            })
            .collect();
        self.links = b.links.into_iter().map(|l| Link { a: g(l.a), b: g(l.b), dial: l.dial, ms: l.ms, ok: l.ok }).collect();
        self.drones = b
            .drones
            .into_iter()
            .map(|d| Drone {
                session: g(d.session),
                node: g(d.node),
                name: d.name.into_owned(),
                number: d.number,
                state: d.state.into_owned(),
                repo: d.repo.into_owned(),
                harness: d.harness.into_owned(),
                engineer: d.engineer.into_owned(),
                paused: d.paused,
                started: d.started,
                ms: d.ms,
            })
            .collect();
        let first = self.events.len();
        self.seq = self.seq.wrapping_add(1);
        self.added_from = f64::INFINITY;
        let mut buf = String::new();
        for e in b.events {
            let Some(kind) = EvKind::parse(&e.kind) else { continue };
            let k = key(&mut buf, &e.kind, &e.id, e.re.as_deref());
            if self.held.contains_key(k) {
                continue;
            }
            self.held.insert(k.into(), e.ms);
            self.added_from = self.added_from.min(e.ms);
            let lesson = e.lesson.as_deref().map(|l| self.intern(l));
            let to = match e.to.as_deref() {
                Some("all") => To::All,
                Some("local") => To::Local,
                Some("engineer") => To::Engineer,
                _ => To::Session,
            };
            self.events.push(Ev {
                ms: e.ms,
                text: e.text.into_owned(),
                lesson,
                why: e.why.map(Cow::into_owned),
                play: Play {
                    kind,
                    from_n: g(e.from_n),
                    from_s: e.from_s.map(g),
                    to,
                    to_n: e.to_n.map(g),
                    to_s: e.to_s.map(g),
                    n: u32::try_from(e.n).unwrap_or(u32::MAX),
                },
                seq: self.seq,
            });
        }
        let added = self.events.len() - first;
        // A response holds events older than the newest held: ones that reached the store late (a
        // sync), or a slower clock's. The sort is stable, and linear on runs already in order.
        if first > 0 && self.events[first..].iter().any(|e| e.ms < self.events[first - 1].ms) {
            self.events.sort_by(|a, b| a.ms.total_cmp(&b.ms));
        }
        let cut = self.now - WINDOW_MS;
        let gone = self.events.partition_point(|e| e.ms < cut);
        if gone > 0 {
            self.events.drain(..gone);
            self.held.retain(|_, ms| *ms >= cut);
        }
        Some(added)
    }

    /// Forgets everything (another collective).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn node(&self, key: u32) -> Option<&Node> {
        self.nodes.iter().find(|n| n.key == key)
    }

    pub fn drone(&self, session: u32) -> Option<&Drone> {
        self.drones.iter().find(|d| d.session == session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = r#"{"collective":"c","now":1000,"from":0,"me":0,"strs":["K","s1","K2","s2"],
        "nodes":[{"key":0,"host":"here","me":true,"syncs":true},{"key":2,"host":"there","syncs":true}],
        "links":[{"a":0,"b":2,"dial":true,"ms":900,"ok":true}],
        "drones":[{"session":1,"node":0,"name":"A · r · claude","number":1,"state":"working","repo":"r","harness":"claude","engineer":"a","paused":false,"started":1,"ms":2}],
        "events":[{"id":"m1","ms":10,"kind":"tell","from_n":0,"from_s":1,"to":"session","to_n":2,"to_s":3,"text":"hi"},
                  {"id":"s2","ms":12,"kind":"seen","from_n":2,"from_s":3,"re":"m1"}]}"#;

    #[test]
    fn a_response_is_interned_and_merged_once() {
        let mut m = Model::default();
        assert_eq!(m.merge(ONE), Some(2));
        assert_eq!(m.added().count(), 2);
        assert_eq!(m.events.len(), 2);
        assert_eq!(m.str(m.nodes[1].key), "K2");
        assert_eq!(m.events[0].play.to_s.map(|s| m.str(s).to_string()), Some("s2".into()));
        // The same events again (a refetch from `since`) add nothing.
        assert_eq!(m.merge(ONE), Some(0));
        assert_eq!(m.added().count(), 0);
        assert_eq!(m.events.len(), 2);
        // The string table stays one entry per string, and holds no event ids.
        assert_eq!(m.strs.iter().filter(|s| &***s == "K").count(), 1);
        assert!(!m.strs.iter().any(|s| &**s == "m1"));
    }

    #[test]
    fn a_late_event_is_added_in_time_order() {
        let mut m = Model::default();
        m.merge(ONE);
        let late = r#"{"now":1000,"me":0,"strs":["K"],"events":[{"id":"m0","ms":11,"kind":"tell","from_n":0,"to":"all","n":99999999999},
            {"id":"m1","ms":10,"kind":"tell","from_n":0,"to":"session"}]}"#;
        assert_eq!(m.merge(late), Some(1));
        let ms: Vec<f64> = m.events.iter().map(|e| e.ms).collect();
        assert_eq!(ms, [10.0, 11.0, 12.0]);
        let added: Vec<f64> = m.added().map(|e| e.ms).collect();
        assert_eq!(added, [11.0]);
        assert_eq!(m.events[1].play.n, u32::MAX, "a count beyond u32 saturates");
    }

    #[test]
    fn events_past_the_window_are_forgotten() {
        let mut m = Model::default();
        m.merge(ONE);
        let later = format!(r#"{{"now":{},"me":0,"strs":["K"],"events":[]}}"#, WINDOW_MS + 11.0);
        assert_eq!(m.merge(&later), Some(0));
        assert_eq!(m.events.len(), 1, "the tell at 10 is out, the seen at 12 is in");
        assert_eq!(m.held.len(), 1);
    }

    #[test]
    fn no_view_is_none() {
        assert!(Model::default().merge("not json").is_none());
    }
}
