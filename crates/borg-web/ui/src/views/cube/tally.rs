//! What the Cube counts: the counters over the held events, the collective's pulse, and the
//! timeline's hours stacked by legend chip. Pure over the model's events (oldest first), so it is
//! tested natively; the local midnight comes in from the browser.

#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::while_float, reason = "times are f64 ms, an hour's index is small and not negative")]

use std::fmt::Write as _;

use crate::model::{Ev, EvKind, Model};
use crate::util::DAY_MS;

use super::kinds::{Cat, Filter};

pub const MINUTE_MS: f64 = 60_000.0;
pub const HOUR_MS: f64 = 60.0 * MINUTE_MS;
/// The window the server keeps (its messages' 7-day TTL), which the timeline shows.
pub const WEEK_MS: f64 = 7.0 * DAY_MS;
/// The timeline's bins: one an hour.
pub const HOURS: usize = 168;
/// The timeline's height in its SVG units.
pub const TL_H: f64 = 40.0;

/// The counters. Each tile's title says exactly what its number counts (views/cube.rs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hud {
    pub machines: usize,
    pub links: usize,
    pub drones: usize,
    pub working: usize,
    /// Since the viewer's local midnight: tells and asks, times a session was shown one, new lessons.
    pub tx_today: usize,
    pub shown_today: usize,
    pub lessons_today: usize,
}

pub fn hud(m: &Model, midnight: f64) -> Hud {
    let today = &m.events[m.events.partition_point(|e| e.ms < midnight)..];
    let count = |f: fn(EvKind) -> bool| today.iter().filter(|e| f(e.play.kind)).count();
    Hud {
        machines: m.nodes.len(),
        links: m.links.iter().filter(|l| l.ok).count(),
        drones: m.drones.iter().filter(|d| d.state != "ended").count(),
        working: m.drones.iter().filter(|d| d.state == "working").count(),
        tx_today: count(EvKind::transmits),
        shown_today: count(|k| k == EvKind::Seen),
        lessons_today: count(|k| k == EvKind::Lesson),
    }
}

/// Events of every kind in the minute up to `at`: `(at - 1 min, at]`.
pub fn pulse(ev: &[Ev], at: f64) -> usize {
    ev.partition_point(|e| e.ms <= at) - ev.partition_point(|e| e.ms <= at - MINUTE_MS)
}

/// The busiest minute of the held events, by the same window as `pulse`, so `pulse <= peak`.
pub fn peak(ev: &[Ev]) -> usize {
    let mut i = 0;
    let mut best = 0;
    for j in 0..ev.len() {
        while ev[j].ms - ev[i].ms >= MINUTE_MS {
            i += 1;
        }
        best = best.max(j + 1 - i);
    }
    best
}

/// Events per hour from `start` on, by chip (`Cat::ALL` order).
pub fn hours(ev: &[Ev], start: f64) -> Vec<[u32; 6]> {
    let mut bins = vec![[0u32; 6]; HOURS];
    for e in &ev[ev.partition_point(|e| e.ms < start)..] {
        let h = ((e.ms - start) / HOUR_MS).floor();
        if h >= 0.0
            && let Some(b) = bins.get_mut((h as usize).min(HOURS - 1))
        {
            b[Cat::of(e.play.kind) as usize] += 1;
        }
    }
    bins
}

/// The timeline's bars: an SVG path per chip that is on, stacked in `Cat::ALL` order, the busiest
/// hour of the chips on at full height. x is the hour, y runs down from `TL_H`.
pub fn bars(bins: &[[u32; 6]], f: Filter) -> Vec<(Cat, String)> {
    let shown = |b: &[u32; 6]| Cat::ALL.iter().filter(|c| f.has(**c)).map(|c| f64::from(b[*c as usize])).sum::<f64>();
    let max = bins.iter().map(shown).fold(0.0, f64::max);
    if max <= 0.0 {
        return Vec::new();
    }
    let mut base = vec![TL_H; bins.len()];
    Cat::ALL
        .iter()
        .filter(|c| f.has(**c))
        .filter_map(|c| {
            let mut d = String::new();
            for (x, (b, y)) in bins.iter().zip(base.iter_mut()).enumerate() {
                let v = b[*c as usize];
                if v > 0 {
                    let h = f64::from(v) / max * TL_H;
                    *y -= h;
                    let _ = write!(d, "M{x} {:.2}h1v{h:.2}h-1z", *y);
                }
            }
            (!d.is_empty()).then_some((*c, d))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: &[(f64, &str)]) -> Model {
        let evs: Vec<String> = ms.iter().enumerate().map(|(i, (t, k))| format!(r#"{{"id":"e{i}","ms":{t},"kind":"{k}","from_n":0}}"#)).collect();
        let body = format!(
            r#"{{"now":0,"me":0,"strs":["K"],"nodes":[{{"key":0,"host":"h","me":true}}],
               "drones":[{{"session":0,"node":0,"state":"working"}},{{"session":0,"node":0,"state":"ended"}}],"events":[{}]}}"#,
            evs.join(",")
        );
        let mut m = Model::default();
        assert!(m.merge(&body).is_some());
        m
    }

    #[test]
    fn the_counters_count_from_midnight() {
        let m = at(&[(5.0, "tell"), (100.0, "tell"), (101.0, "ask"), (102.0, "seen"), (103.0, "lesson"), (104.0, "lesson_update")]);
        let h = hud(&m, 100.0);
        assert_eq!((h.tx_today, h.shown_today, h.lessons_today), (2, 1, 1));
        assert_eq!((h.drones, h.working, h.machines), (1, 1, 1));
    }

    #[test]
    fn the_pulse_is_the_last_minute_and_never_above_the_peak() {
        let m = at(&[(0.0, "tell"), (30_000.0, "seen"), (59_000.0, "tell"), (60_000.0, "tell"), (200_000.0, "sync")]);
        assert_eq!(pulse(&m.events, 60_000.0), 3, "(0, 60 s]: the event at 0 is out");
        assert_eq!(pulse(&m.events, 199_999.0), 0);
        assert_eq!(peak(&m.events), 3, "a minute holds three: 0 and 60 s are a whole minute apart");
        assert_eq!(peak(&[]), 0);
    }

    #[test]
    fn hours_stack_by_chip_and_skip_the_chips_off() {
        let m = at(&[(-1.0, "tell"), (10.0, "tell"), (20.0, "seen"), (HOUR_MS * 2.5, "lesson"), (WEEK_MS + 1.0, "veto")]);
        let b = hours(&m.events, 0.0);
        assert_eq!(b.len(), HOURS);
        assert_eq!(b[0], [1, 1, 0, 0, 0, 0], "the event before the window is out");
        assert_eq!(b[2][Cat::Lessons as usize], 1);
        assert_eq!(b[HOURS - 1][Cat::Steering as usize], 1, "the newest is in the last hour");
        let all = bars(&b, Filter::default());
        assert_eq!(all.iter().map(|(c, _)| *c).collect::<Vec<_>>(), [Cat::Messages, Cat::Received, Cat::Lessons, Cat::Steering]);
        assert_eq!(all[0].1, "M0 20.00h1v20.00h-1z", "half the busiest hour, on the floor");
        let off = bars(&b, Filter::default().toggle(Cat::Messages));
        assert_eq!(off[0], (Cat::Received, "M0 0.00h1v40.00h-1z".into()), "alone in its hour: full height");
        assert_eq!(bars(&[[0; 6]; 3], Filter::default()), Vec::new(), "no events, no bars");
    }
}
