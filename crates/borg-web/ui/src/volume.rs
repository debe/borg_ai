//! The collective's knowledge inside the Cube's core. Each concern area is a box whose volume is its
//! share of the lessons, the areas together filling the core, built as a part of a Borg cube with a
//! light for each of its lessons (interior.rs). An area holds its lessons of every system, a system's
//! together: one area, one bay, however many systems share it, so a store of several systems still
//! shows a few large bays, not one per system and area. The cuts take the areas largest first, cut the
//! longest side in the ratio of the two halves' lessons, so boxes of alike areas stay near cubic, but
//! never leave a half thinner than `MIN_SIDE` (a small area beside a large one gets a box its kind can
//! be built in, not a slab), and leave a shaft between the halves (`split`), wide at the first cut and
//! narrower at each deeper one, where corridors run. A lesson's colour is what sessions did with it;
//! its strength shows as brightness (paint.rs), never as a place.
//!
//! `build` runs when the lessons change and may allocate; `step` runs every frame and does not.
//! Lessons keep their order within their area (by their system, then their id's hash). A change in
//! the store re-cuts the boxes, so lessons may move a little: the interior is built anew, and each
//! lesson's point, where its light is (`Interior::seats`: what effects run to, what a click hits),
//! moves with it at once. What a lesson shows follows its id through a rebuild: its place, and the
//! light an event gave it.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "f32 screen graphics: lesson counts and ranks to f32, f64 activations and weights to f32 (indices go through scene.rs `ix`)"
)]

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use crate::interior::{Interior, Room, Shaft, V3};
use crate::scene::{CORE, Tone, fnv, ix};

/// Display: half the edge of the cube the systems fill, world units: clear of the shell's recessed
/// plating (interior.rs `panel`, at most 0.06 deep).
pub const FILL: f32 = CORE * 0.9;
/// Display: how fast a lesson's light fades once an event lit it, per second.
const FADE: f32 = 0.5;
/// Display: the shaft the first cut of a split leaves between its halves, world units, and how much
/// narrower each deeper cut's is: wide main shafts, narrow passages, never an even spacing. At most a
/// quarter of the smaller half.
const SHAFT: f32 = 0.34;
const SHAFT_DECAY: f32 = 0.55;
/// Guard: the thinnest half a cut leaves, world units, where the box is twice as long (else it halves):
/// with the shaft taken off, a box at least 0.1 thick, as wide as an alcove's frame and a spire's node
/// need. Today's stores' cuts leave 0.37 and more, so it only holds a new, small area beside large ones.
const MIN_SIDE: f32 = 0.12;

/// A lesson as the volume reads it, borrowed from what the page parsed (api.rs `Lesson`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Item<'a> {
    pub id: &'a str,
    /// Its system (first scope, "general" without one) and concern area ("other" without one), as the
    /// Knowledge tab names them: the core groups by area, a system's lessons together in it.
    pub system: &'a str,
    pub area: &'a str,
    pub activation: f64,
    pub changed: f64,
    pub ignored: f64,
    pub corrected: f64,
    pub vetoed: f64,
    pub dormant: bool,
    pub consolidated: bool,
    /// An outside store holds it (`borg memory absorb`).
    pub absorbed: bool,
}

impl Item<'_> {
    /// The colour of what sessions did with it: held elsewhere, not judged yet, corrected more than
    /// acted on, acted on in at least half the judged sessions, else mostly ignored.
    #[must_use]
    pub fn tone(&self) -> Tone {
        let judged = self.changed + self.ignored + self.corrected + self.vetoed;
        if self.absorbed {
            Tone::Hull
        } else if judged < 1.0 {
            Tone::Cyan
        } else if self.corrected + self.vetoed > self.changed {
            Tone::Red
        } else if 2.0 * self.changed >= judged {
            Tone::Green
        } else {
            Tone::Dim
        }
    }
}

/// A box in the core, as its lowest and highest corner.
type Cell = (V3, V3);

/// The lessons in the volume, one entry per lesson in each array.
#[derive(Default)]
pub struct Volume {
    /// Each lesson's id, the one copy `by_id` shares.
    pub ids: Vec<Rc<str>>,
    pub tone: Vec<Tone>,
    /// Strength, 0 (weakest) to 1 (strongest): the rank of its activation.
    pub strength: Vec<f32>,
    pub dormant: Vec<bool>,
    pub consolidated: Vec<bool>,
    /// Light from an event that named it (1 when lit), fading.
    pub lit: Vec<f32>,
    /// When each lesson was lit, on the scene's clock: `-inf` if never, `+inf` until the next `step`,
    /// which counts the light from its own clock.
    lit_at: Vec<f64>,
    /// The structure the lessons sit in, and each one's light (`Interior::seats`): its point.
    pub inner: Interior,
    by_id: HashMap<Rc<str>, u32>,
    /// Whether a light is on: idle frames read this, not every lesson's light.
    fading: bool,
}

/// Groups of keys with their members, largest first, then by key: the Knowledge tab's order.
fn grouped<'a, T: Copy>(items: impl Iterator<Item = (&'a str, T)>) -> Vec<(&'a str, Vec<T>)> {
    let mut m: BTreeMap<&str, Vec<T>> = BTreeMap::new();
    for (k, v) in items {
        m.entry(k).or_default().push(v);
    }
    let mut g: Vec<(&str, Vec<T>)> = m.into_iter().collect();
    g.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    g
}

/// Boxes for `weights` (largest first) filling `b`, in the same order: a binary split treemap in 3D.
/// The list is cut where its two halves weigh most alike, the box across its longest side in that
/// ratio but neither half thinner than `MIN_SIDE` (half the side, on a side under twice it), leaving a
/// shaft of `shaft` between the halves (narrower at each deeper cut, `SHAFT_DECAY`), and both halves
/// again: boxes of alike weights stay near cubic, and a light one next to heavy ones is no slab. Each
/// shaft is added to `shafts`. O(n log n).
fn split(weights: &[f64], b: Cell, shaft: f32, shafts: &mut Vec<Shaft>) -> Vec<Cell> {
    let mut prefix = Vec::with_capacity(weights.len() + 1);
    prefix.push(0.0);
    for w in weights {
        prefix.push(prefix[prefix.len() - 1] + w.max(0.0));
    }
    let mut out = vec![b; weights.len()];
    cut(&prefix, (0, weights.len()), b, shaft, (&mut out, shafts));
    out
}

/// The cut of `split` for the weights `lo..hi` (as `prefix` sums them) in `b`.
fn cut(prefix: &[f64], (lo, hi): (usize, usize), b: Cell, shaft: f32, out: (&mut [Cell], &mut Vec<Shaft>)) {
    let (cells, shafts) = out;
    if hi - lo <= 1 {
        if let Some(o) = cells.get_mut(lo) {
            *o = b;
        }
        return;
    }
    let (base, total) = (prefix[lo], prefix[hi] - prefix[lo]);
    // The first split point whose left half weighs at least half, or the one before it if that is nearer.
    let half = base + total / 2.0;
    let mut m = lo + 1 + prefix[lo + 1..hi].partition_point(|p| *p < half);
    if m >= hi || (m > lo + 1 && half - prefix[m - 1] < prefix[m] - half) {
        m -= 1;
    }
    let side = |a: usize| b.1[a] - b.0[a];
    // The longest side, the first of equal ones.
    let axis = (0..3).rev().max_by(|a, c| side(*a).total_cmp(&side(*c))).unwrap_or(0);
    let thin = (MIN_SIDE / side(axis)).min(0.5);
    let ratio = if total > 0.0 { ((prefix[m] - base) / total) as f32 } else { 0.5 }.clamp(thin, 1.0 - thin);
    let at = b.0[axis] + side(axis) * ratio;
    let gap = shaft.min(0.25 * side(axis) * ratio.min(1.0 - ratio));
    let (mut left, mut right) = (b, b);
    left.1[axis] = at - gap * 0.5;
    right.0[axis] = at + gap * 0.5;
    shafts.push(Shaft { axis, lo: left.1[axis], hi: right.0[axis], min: b.0, max: b.1 });
    cut(prefix, (lo, m), left, shaft * SHAFT_DECAY, (cells, shafts));
    cut(prefix, (m, hi), right, shaft * SHAFT_DECAY, (cells, shafts));
}

/// Each activation's rank as a share: 0 for the weakest, 1 for the strongest; equal activations rank alike.
fn ranks(items: &[Item]) -> Vec<f32> {
    let mut sorted: Vec<f64> = items.iter().map(|i| i.activation).collect();
    sorted.sort_unstable_by(f64::total_cmp);
    let top = sorted.len().saturating_sub(1).max(1) as f32;
    items.iter().map(|i| sorted.partition_point(|a| a.total_cmp(&i.activation).is_lt()) as f32 / top).collect()
}

/// What `nest` lays out for the interior: the rooms (one per area) and the shafts between them.
struct Nest {
    rooms: Vec<Room>,
    shafts: Vec<Shaft>,
}

/// Cuts the core into the areas, largest first, each holding its lessons by their system and then
/// their id's hash, every key worked out once. O(lessons · log lessons).
fn nest(items: &[Item]) -> Nest {
    let mut shafts = Vec::new();
    let areas = grouped(items.iter().enumerate().map(|(i, it)| (it.area, i)));
    let weights: Vec<f64> = areas.iter().map(|g| g.1.len() as f64).collect();
    let boxes = split(&weights, ([-FILL; 3], [FILL; 3]), SHAFT, &mut shafts);
    let rooms = areas
        .into_iter()
        .zip(boxes)
        .map(|((area, ls), (min, max))| {
            let mut keyed: Vec<(&str, u64, usize)> = ls.iter().map(|&i| (items[i].system, fnv(items[i].id), i)).collect();
            keyed.sort_unstable();
            let title = format!("{area} · {}", ls.len());
            Room { min, max, key: area.to_owned(), title, lessons: keyed.into_iter().map(|(_, _, i)| ix(i)).collect() }
        })
        .collect();
    Nest { rooms, shafts }
}

impl Volume {
    #[must_use]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The index of the lesson with this id. O(1).
    #[must_use]
    pub fn find(&self, id: &str) -> Option<u32> {
        self.by_id.get(id).copied()
    }

    /// Lays out `items`: each lesson at its light in the interior built anew. A lesson already in the
    /// volume keeps the light an event gave it, and one no longer among `items` is gone. Returns the
    /// ids it held before, by their old index, so what named a lesson by index can find it again
    /// (`find`). O(lessons · log lessons), and the interior's build (interior.rs `Interior::build`).
    pub fn build(&mut self, items: &[Item]) -> Vec<Rc<str>> {
        let kept: Vec<Option<usize>> = items.iter().map(|i| self.by_id.get(i.id).map(|&j| j as usize)).collect();
        let was = std::mem::take(&mut self.ids);
        let strength = ranks(items);
        let laid = nest(items);
        // An empty core keeps its plain plating (paint.rs `core`): nothing to build.
        self.inner = if items.is_empty() { Interior::default() } else { Interior::build(&laid.rooms, &laid.shafts, items.len()) };
        self.ids = items.iter().map(|i| Rc::from(i.id)).collect();
        self.lit = kept.iter().map(|k| k.map_or(0.0, |j| self.lit[j])).collect();
        self.lit_at = kept.iter().map(|k| k.map_or(f64::NEG_INFINITY, |j| self.lit_at[j])).collect();
        self.tone = items.iter().map(Item::tone).collect();
        self.strength = strength;
        self.dormant = items.iter().map(|i| i.dormant).collect();
        self.consolidated = items.iter().map(|i| i.consolidated).collect();
        self.by_id = self.ids.iter().enumerate().map(|(i, id)| (Rc::clone(id), ix(i))).collect();
        was
    }

    /// Fades the lessons' lights at the clock `t`. Allocates nothing; O(lessons) while a light is on,
    /// O(1) at rest.
    pub fn step(&mut self, t: f64) {
        if self.fading {
            self.fade(t);
        }
    }

    /// Each light by the clock: 1 when lit, less `FADE` for every second since. A light waiting for
    /// this step starts now. Timed by the clock and not by summed frame steps, so a frame gap (a hidden
    /// tab, a hitch) does not leave a light on. O(lessons).
    fn fade(&mut self, t: f64) {
        let mut newest = f64::NEG_INFINITY;
        for (l, at) in self.lit.iter_mut().zip(&mut self.lit_at) {
            *at = at.min(t);
            newest = newest.max(*at);
            *l = (1.0 - FADE * (t - *at) as f32).max(0.0);
        }
        self.fading = t < newest + f64::from(FADE.recip());
    }

    /// Lights a lesson: an event named it. The light counts from the next `step`'s clock, so an event
    /// played before the frame that shows it (one waiting while the tab was hidden) starts at full light
    /// and fades as any other. O(1).
    pub fn light(&mut self, i: u32) {
        if let (Some(l), Some(at)) = (self.lit.get_mut(i as usize), self.lit_at.get_mut(i as usize)) {
            *l = 1.0;
            *at = f64::INFINITY;
            self.fading = true;
        }
    }

    /// Whether a frame now would differ from the last: a light fades. O(1).
    #[must_use]
    pub fn busy(&self) -> bool {
        self.fading
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item<'a>(id: &'a str, system: &'a str, area: &'a str, activation: f64) -> Item<'a> {
        Item { id, system, area, activation, ..Item::default() }
    }

    fn settled(items: &[Item]) -> Volume {
        let mut v = Volume::default();
        let _ = v.build(items);
        v
    }

    /// Where each lesson's light is.
    fn places(v: &Volume) -> Vec<V3> {
        v.inner.seats.iter().map(|s| s.at).collect()
    }

    fn store(n: usize) -> Vec<(String, f64)> {
        (0..n).map(|i| (format!("lesson-{i}"), (i * 7919 % 101) as f64 / 10.0 - 3.0)).collect()
    }

    fn items(ids: &[(String, f64)]) -> Vec<Item<'_>> {
        let systems = ["billing", "search", "general", "infra"];
        let areas = ["testing", "data", "api"];
        ids.iter().enumerate().map(|(i, (id, a))| item(id, systems[i % 4], areas[i % 3], *a)).collect()
    }

    fn inside(p: V3, (min, max): Cell) -> bool {
        (0..3).all(|a| p[a] >= min[a] - 1e-3 && p[a] <= max[a] + 1e-3)
    }

    fn overlap(a: Cell, b: Cell) -> f32 {
        (0..3).map(|k| (a.1[k].min(b.1[k]) - a.0[k].max(b.0[k])).max(0.0)).product()
    }

    fn volume(b: Cell) -> f32 {
        (0..3).map(|k| b.1[k] - b.0[k]).product()
    }

    #[test]
    fn every_lesson_lies_inside_its_area_and_an_area_holds_every_systems_lessons_together() {
        let ids = store(500);
        let its = items(&ids);
        let v = settled(&its);
        assert_eq!(v.len(), 500);
        let laid = nest(&its);
        // Four systems, three areas: three bays, not twelve.
        assert_eq!(laid.rooms.len(), 3);
        let mut seen = vec![false; 500];
        for r in &laid.rooms {
            assert!(inside(r.min, ([-FILL; 3], [FILL; 3])) && inside(r.max, ([-FILL; 3], [FILL; 3])), "{r:?} outside the core");
            for &i in &r.lessons {
                let it = &its[i as usize];
                assert!(r.key == it.area, "{} in {}", it.id, r.key);
                assert!(inside(v.inner.seats[i as usize].at, (r.min, r.max)), "{} outside its area", it.id);
                seen[i as usize] = true;
            }
            // A system's lessons sit together in the area.
            let systems: Vec<&str> = r.lessons.iter().map(|&i| its[i as usize].system).collect();
            assert!(systems.windows(2).all(|w| w[0] <= w[1]), "{} mixes its systems", r.key);
        }
        assert!(seen.iter().all(|s| *s), "every lesson has its area");
    }

    #[test]
    fn areas_do_not_overlap_and_are_sized_by_their_lessons() {
        let ids = store(400);
        let its = items(&ids);
        let laid = nest(&its);
        let cells: Vec<Cell> = laid.rooms.iter().map(|r| (r.min, r.max)).collect();
        for (i, a) in cells.iter().enumerate() {
            for b in &cells[i + 1..] {
                assert!(overlap(*a, *b) < 1e-6, "{a:?} overlaps {b:?}");
            }
        }
        // The areas' volumes are their shares of the core, less their shafts: a larger area never has
        // the smaller box.
        for a in &laid.rooms {
            for b in &laid.rooms {
                if a.lessons.len() > b.lessons.len() {
                    assert!(volume((a.min, a.max)) > volume((b.min, b.max)), "{} smaller than {}", a.title, b.title);
                }
            }
        }
    }

    #[test]
    fn the_cuts_keep_boxes_near_cubic_and_leave_shafts_between() {
        let core = ([-1.0; 3], [1.0; 3]);
        let weights = [30.0, 22.0, 15.0, 12.0, 9.0, 6.0, 4.0, 2.0];
        let total: f64 = weights.iter().sum();
        let mut shafts = Vec::new();
        let boxes = split(&weights, core, 0.0, &mut shafts);
        for (b, w) in boxes.iter().zip(weights) {
            assert!((f64::from(volume(*b)) - w / total * 8.0).abs() < 1e-4, "volume is the share");
            let sides = [0, 1, 2].map(|a| b.1[a] - b.0[a]);
            let (lo, hi) = (sides.iter().copied().fold(f32::MAX, f32::min), sides.iter().copied().fold(0.0, f32::max));
            if w / total >= 1.0 / 8.0 {
                assert!(lo >= hi / 4.0, "{b:?} is a slab");
            }
        }
        assert_eq!(shafts.len(), weights.len() - 1, "a shaft per cut");
        assert!(split(&[], core, 0.0, &mut shafts).is_empty());
        // A new area of one lesson beside a store's large ones (today's stores' sizes) gets a box, not a
        // slab: an even cut would leave it 0.03 thick (and 0.02 in the other store).
        for mut store in [
            vec![117.0, 111.0, 83.0, 60.0, 55.0, 44.0, 33.0, 22.0, 20.0, 18.0, 18.0],
            vec![52.0, 45.0, 45.0, 42.0, 42.0, 42.0, 41.0, 40.0, 39.0, 38.0, 36.0, 36.0],
        ] {
            store.push(1.0);
            let mut shafts = Vec::new();
            for b in split(&store, ([-FILL; 3], [FILL; 3]), SHAFT, &mut shafts) {
                assert!((0..3).all(|a| b.1[a] - b.0[a] >= 0.1), "{b:?} is a slab");
            }
        }
        assert_eq!(split(&[5.0], core, 0.0, &mut shafts)[0], core);
        // With shafts: the first is the widest, each lies between its halves.
        let mut shafts = Vec::new();
        let boxes = split(&weights, core, SHAFT, &mut shafts);
        let width = |s: &Shaft| s.hi - s.lo;
        assert!(width(&shafts[0]) > 0.1 && shafts.iter().all(|s| width(s) <= width(&shafts[0]) + 1e-6 && width(s) > 0.0), "{shafts:?}");
        for (i, a) in boxes.iter().enumerate() {
            for b in &boxes[i + 1..] {
                assert!(overlap(*a, *b) < 1e-6);
            }
        }
    }

    #[test]
    fn a_change_of_strength_moves_no_lesson() {
        let ids = store(150);
        let its = items(&ids);
        let mut stronger = its.clone();
        for (k, it) in stronger.iter_mut().enumerate() {
            it.activation = -(k as f64);
            it.dormant = k % 5 == 0;
        }
        assert_eq!(places(&settled(&its)), places(&settled(&stronger)));
    }

    #[test]
    fn the_same_lessons_take_the_same_places() {
        let ids = store(120);
        assert_eq!(places(&settled(&items(&ids))), places(&settled(&items(&ids))));
    }

    #[test]
    fn a_place_depends_on_the_lessons_not_on_the_order_they_came_in() {
        let ids = store(90);
        let its = items(&ids);
        let mut rev = its.clone();
        rev.reverse();
        let (a, b) = (settled(&its), settled(&rev));
        for (i, it) in its.iter().enumerate() {
            let j = b.find(it.id).expect("in both") as usize;
            assert_eq!(a.inner.seats[i].at, b.inner.seats[j].at, "{} moved with the order", it.id);
        }
    }

    #[test]
    fn a_rebuild_hands_back_the_ids_it_held_and_finds_the_new_ones() {
        let ids = store(10);
        let mut v = settled(&items(&ids));
        let mut more = ids.clone();
        more.push(("fresh".into(), 2.0));
        let was = v.build(&items(&more));
        assert_eq!(was.len(), 10, "it hands back the ids it held");
        assert_eq!(&*was[3], ids[3].0.as_str());
        assert!(!v.busy(), "nothing to redraw after the frame the rebuild draws");
        assert_eq!(v.find("fresh"), Some(10));
        assert_eq!(v.find("gone"), None);
    }

    #[test]
    fn an_absorb_changes_the_colour_only() {
        let ids = store(30);
        let mut its = items(&ids);
        let mut v = settled(&its);
        let at = places(&v);
        its[2].absorbed = true;
        let _ = v.build(&its);
        assert_eq!(v.tone[2], Tone::Hull);
        assert_eq!(places(&v), at, "no place changes");
    }

    #[test]
    fn a_light_fades_and_then_the_volume_rests() {
        let ids = store(3);
        let mut v = settled(&items(&ids));
        v.light(1);
        assert!(v.busy());
        v.step(11.0);
        assert!((v.lit[1] - 1.0).abs() < 1e-6, "full on its first frame");
        v.step(12.0);
        assert!((v.lit[1] - 0.5).abs() < 1e-6 && v.busy());
        v.step(13.0);
        assert!(v.lit[1] == 0.0 && !v.busy());
        v.light(99);
        assert!(!v.busy(), "a lesson it does not hold lights nothing");
    }

    #[test]
    fn a_frame_gap_inside_the_fade_leaves_no_light_on() {
        let ids = store(3);
        let mut v = settled(&items(&ids));
        v.light(1);
        v.step(10.0);
        v.step(10.5);
        assert!((v.lit[1] - 0.75).abs() < 1e-6);
        // A hidden tab: the next frame comes 2.5 s later.
        v.step(13.0);
        assert!(v.lit[1] == 0.0 && !v.busy(), "lit {}", v.lit[1]);
    }

    #[test]
    fn a_light_played_before_a_late_frame_counts_from_that_frame() {
        let ids = store(3);
        let mut v = settled(&items(&ids));
        // An event that waited while the tab was hidden plays before the first frame after it.
        v.light(2);
        v.step(20.0);
        assert!((v.lit[2] - 1.0).abs() < 1e-6 && v.busy());
        v.step(21.0);
        assert!((v.lit[2] - 0.5).abs() < 1e-6);
        v.step(22.0);
        assert!(v.lit[2] == 0.0 && !v.busy());
    }

    #[test]
    fn a_rebuild_keeps_a_lessons_light_and_its_fade() {
        let ids = store(10);
        let mut v = settled(&items(&ids));
        v.light(4);
        v.step(11.0);
        v.step(11.5);
        let mut more = vec![("fresh".to_owned(), 9.0)];
        more.extend(ids.iter().cloned());
        let _ = v.build(&items(&more));
        let i = v.find(&ids[4].0).expect("kept") as usize;
        assert_eq!(i, 5, "at a new index");
        assert!((v.lit[i] - 0.75).abs() < 1e-6, "lit {}", v.lit[i]);
        assert!(v.lit.iter().enumerate().all(|(j, l)| j == i || *l == 0.0), "only it");
        v.step(12.0);
        assert!((v.lit[i] - 0.5).abs() < 1e-6, "still fading: {}", v.lit[i]);
        v.step(13.0);
        assert!(v.lit[i] == 0.0 && !v.busy());
    }

    #[test]
    fn the_colour_says_what_sessions_did_with_it() {
        let base = item("a", "s", "x", 0.0);
        assert_eq!(base.tone(), Tone::Cyan, "not judged yet");
        assert_eq!(Item { changed: 3.0, ignored: 2.0, ..base }.tone(), Tone::Green);
        assert_eq!(Item { changed: 1.0, ignored: 4.0, ..base }.tone(), Tone::Dim);
        assert_eq!(Item { changed: 1.0, corrected: 1.0, vetoed: 1.0, ..base }.tone(), Tone::Red);
        assert_eq!(Item { changed: 5.0, absorbed: true, ..base }.tone(), Tone::Hull);
    }

    #[test]
    fn steady_steps_allocate_nothing() {
        use crate::paint::record::allocs;
        let ids = store(1000);
        let mut v = Volume::default();
        let _ = v.build(&items(&ids));
        v.light(5);
        v.step(0.1);
        let before = allocs();
        for k in 0..120 {
            v.step(0.1 + f64::from(k) / 60.0);
        }
        assert_eq!(allocs(), before);
    }
}
