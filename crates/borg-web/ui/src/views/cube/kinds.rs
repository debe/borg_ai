//! What the Cube calls things: the filter categories of the event kinds, the Borg names of drones
//! and machines, and what the ticker and the toasts say of an event. Pure, so it is tested natively.

use std::fmt::Write as _;

use crate::model::{Drone, Ev, EvKind, Model, To};
use crate::util::trunc;

use super::of;

/// A legend chip: the kinds it plays, lists and stacks in the timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cat {
    Messages,
    Received,
    Lessons,
    Syncs,
    Handoffs,
    Steering,
}

impl Cat {
    /// In legend order, which is also the timeline's stacking order, bottom up.
    pub const ALL: [Self; 6] = [Self::Messages, Self::Received, Self::Lessons, Self::Syncs, Self::Handoffs, Self::Steering];

    pub const fn of(k: EvKind) -> Self {
        match k {
            EvKind::Tell | EvKind::Ask => Self::Messages,
            EvKind::Seen => Self::Received,
            EvKind::Lesson | EvKind::LessonUpdate | EvKind::Absorb | EvKind::Release => Self::Lessons,
            EvKind::Sync | EvKind::SyncFailed => Self::Syncs,
            EvKind::Handoff => Self::Handoffs,
            EvKind::Pause | EvKind::Resume | EvKind::Veto => Self::Steering,
        }
    }

    /// Its CSS class and stored name.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Messages => "messages",
            Self::Received => "received",
            Self::Lessons => "lessons",
            Self::Syncs => "syncs",
            Self::Handoffs => "handoffs",
            Self::Steering => "steering",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Handoffs => "hand-offs",
            c => c.key(),
        }
    }

    /// The kinds it holds, as its chip's title says them.
    pub const fn help(self) -> &'static str {
        match self {
            Self::Messages => "tells and asks between sessions",
            Self::Received => "a session shown a message",
            Self::Lessons => "lessons written or sharpened, handed to an outside store or taken back",
            Self::Syncs => "mesh syncs with a peer, and the ones that failed",
            Self::Handoffs => "hand-offs offered, taken, taken back, declined, aborted or done",
            Self::Steering => "pause, resume and veto",
        }
    }

    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// The chips that are on, a bit per `Cat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Filter(u8);

impl Default for Filter {
    fn default() -> Self {
        Self(Cat::ALL.iter().fold(0, |b, c| b | c.bit()))
    }
}

impl Filter {
    pub const fn has(self, c: Cat) -> bool {
        self.0 & c.bit() != 0
    }

    pub const fn shows(self, k: EvKind) -> bool {
        self.has(Cat::of(k))
    }

    #[must_use]
    pub const fn toggle(self, c: Cat) -> Self {
        Self(self.0 ^ c.bit())
    }

    /// As the browser keeps it: the chips that are off, so a chip added later starts on.
    pub fn stored(self) -> String {
        Cat::ALL.iter().filter(|c| !self.has(**c)).map(|c| c.key()).collect::<Vec<_>>().join(",")
    }

    pub fn from_stored(s: &str) -> Self {
        let off = |c: &Cat| s.split(',').any(|k| k == c.key());
        Cat::ALL.iter().filter(|c| off(c)).fold(Self::default(), |f, c| f.toggle(*c))
    }
}

/// The CSS tone of an event's line, dot and toast.
pub const fn tone(k: EvKind) -> &'static str {
    match k {
        EvKind::Tell => "tell",
        EvKind::Ask => "ask",
        EvKind::Pause | EvKind::Veto | EvKind::SyncFailed => "veto",
        EvKind::Resume => "resume",
        EvKind::Seen => "seen",
        EvKind::Lesson | EvKind::LessonUpdate | EvKind::Absorb | EvKind::Release => "lesson",
        EvKind::Handoff => "handoff",
        EvKind::Sync => "sync",
    }
}

/// The collective's size for "of": its drones, or the highest number one was given when ended ones
/// are no longer listed.
pub fn total(m: &Model) -> usize {
    m.drones.iter().map(|d| d.number as usize).max().unwrap_or(0).max(m.drones.len())
}

/// "Two of Twelve · `shop`": a drone by its number among the collective's and its repo.
pub fn drone_name(d: &Drone, total: usize) -> String {
    if d.repo.is_empty() { of(d.number, total) } else { format!("{} · {}", of(d.number, total), d.repo) }
}

/// A machine by its host.
pub fn host(m: &Model, key: u32) -> &str {
    m.node(key).map_or("an unknown machine", |n| n.host.as_str())
}

/// Who an event names: a session's drone, else the session's short id on its machine; no session,
/// the engineer on the machine.
pub fn who(m: &Model, total: usize, session: Option<u32>, node: u32) -> String {
    match (session, session.and_then(|s| m.drone(s))) {
        (_, Some(d)) => drone_name(d, total),
        (Some(s), None) => format!("session {} on {}", m.str(s).chars().take(8).collect::<String>(), host(m, node)),
        (None, None) => format!("the engineer on {}", host(m, node)),
    }
}

/// What the ticker says of an event.
pub fn line(m: &Model, total: usize, e: &Ev) -> String {
    let p = &e.play;
    let from = || who(m, total, p.from_s, p.from_n);
    // What it says, and whether its text follows.
    let (mut s, text) = match p.kind {
        EvKind::Seen => (format!("{} received a transmission", from()), false),
        EvKind::Lesson => (format!("Lesson assimilated from {}", host(m, p.from_n)), true),
        EvKind::LessonUpdate => (format!("Lesson sharpened on {}", host(m, p.from_n)), true),
        EvKind::Absorb => (format!("Lesson handed to an outside store by {}", host(m, p.from_n)), true),
        EvKind::Release => (format!("Lesson taken back into the collective by {}", host(m, p.from_n)), true),
        EvKind::Handoff => {
            let mut s = format!("{} · hand-off {}", from(), e.why.as_deref().unwrap_or("event"));
            if let Some(t) = p.to_n {
                let _ = write!(s, " → {}", host(m, t));
            }
            (s, true)
        }
        EvKind::SyncFailed => (format!("{} ⇄ {} failed", host(m, p.from_n), p.to_n.map_or("a peer", |t| host(m, t))), true),
        EvKind::Sync => {
            let mut s = format!("{} ⇄ {}", host(m, p.from_n), p.to_n.map_or("a peer", |t| host(m, t)));
            if p.n == 0 {
                s.push_str(": in step");
            } else {
                let _ = write!(s, ": {} line{} moved", p.n, if p.n == 1 { "" } else { "s" });
            }
            (s, false)
        }
        EvKind::Tell | EvKind::Ask | EvKind::Pause | EvKind::Resume | EvKind::Veto => {
            let to = match p.to {
                To::All => "the collective".to_string(),
                To::Engineer => "the engineer".to_string(),
                To::Local => format!("every drone on {}", p.to_n.map_or("its machine", |n| host(m, n))),
                To::Session => p.to_s.map_or_else(|| "a session".into(), |t| who(m, total, Some(t), p.to_n.unwrap_or(u32::MAX))),
            };
            (format!("{} → {to}", from()), true)
        }
    };
    if text && !e.text.is_empty() {
        let _ = write!(s, ": {}", trunc(&e.text, 90));
    }
    s
}

/// What a toast says of an event, for the kinds that get one by themselves (a new link needs what
/// the view has seen, views/cube.rs says it).
pub fn toast(e: &Ev) -> Option<String> {
    match e.play.kind {
        EvKind::Lesson => Some(if e.text.is_empty() { "Lesson assimilated".into() } else { format!("Lesson assimilated: {}", trunc(&e.text, 70)) }),
        EvKind::Handoff => {
            let why = e.why.as_deref().unwrap_or("event");
            Some(if e.text.is_empty() { format!("Hand-off {why}") } else { format!("Hand-off {why} · {}", trunc(&e.text, 60)) })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = r#"{"now":1000,"me":0,"strs":["K","s1","K2","s2","s3"],
        "nodes":[{"key":0,"host":"here","me":true},{"key":2,"host":"there"}],
        "drones":[{"session":1,"node":0,"name":"alice · shop · claude #2","number":2,"state":"working","repo":"shop"},
                  {"session":3,"node":2,"name":"x","number":12,"state":"ended","repo":""}],
        "events":[{"id":"m1","ms":10,"kind":"tell","from_n":0,"from_s":1,"to":"session","to_n":2,"to_s":3,"text":"hi"},
                  {"id":"sync:K2@11","ms":11,"kind":"sync","from_n":0,"to_n":2,"n":3},
                  {"id":"sync:K2@12","ms":12,"kind":"sync_failed","from_n":0,"to_n":2,"text":"refused"},
                  {"id":"L@2","ms":13,"kind":"lesson","from_n":2,"lesson":"L","text":"Is the test real?"},
                  {"id":"h:taken","ms":14,"kind":"handoff","from_n":0,"from_s":4,"to_n":2,"why":"taken","text":"the rest"}]}"#;

    fn model() -> Model {
        let mut m = Model::default();
        assert!(m.merge(BODY).is_some());
        m
    }

    #[test]
    fn drones_are_named_by_number_and_repo() {
        let m = model();
        assert_eq!(total(&m), 12, "the highest number when ended drones are fewer");
        assert_eq!(drone_name(&m.drones[0], 12), "Two of Twelve · shop");
        assert_eq!(drone_name(&m.drones[1], 12), "Twelve of Twelve");
    }

    #[test]
    fn the_ticker_says_each_kind() {
        let m = model();
        let t = total(&m);
        let lines: Vec<String> = m.events.iter().map(|e| line(&m, t, e)).collect();
        assert_eq!(lines[0], "Two of Twelve · shop → Twelve of Twelve: hi");
        assert_eq!(lines[1], "here ⇄ there: 3 lines moved");
        assert_eq!(lines[2], "here ⇄ there failed: refused");
        assert_eq!(lines[3], "Lesson assimilated from there: Is the test real?");
        assert_eq!(lines[4], "session s3 on here · hand-off taken → there: the rest");
        assert_eq!(toast(&m.events[3]).as_deref(), Some("Lesson assimilated: Is the test real?"));
        assert_eq!(toast(&m.events[4]).as_deref(), Some("Hand-off taken · the rest"));
        assert_eq!(toast(&m.events[0]), None);
    }

    #[test]
    fn a_filter_keeps_the_chips_that_are_off() {
        let f = Filter::default().toggle(Cat::Received).toggle(Cat::Steering);
        assert!(!f.shows(EvKind::Seen) && !f.shows(EvKind::Veto) && f.shows(EvKind::Lesson) && f.shows(EvKind::SyncFailed));
        assert_eq!(f.stored(), "received,steering");
        assert_eq!(Filter::from_stored(&f.stored()), f);
        assert_eq!(Filter::from_stored(""), Filter::default());
        assert_eq!(Filter::from_stored("nonsense"), Filter::default());
    }
}
