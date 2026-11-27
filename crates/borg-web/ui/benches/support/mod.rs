//! What the Cube's frame tests and its bench share: an allocator that counts this thread's
//! allocations, a painter that counts what a frame asks of the canvas, and a busy world that keeps a
//! fixed number of effects running. The app's unit tests include it as `paint::record`; the bench
//! (benches/scene.rs) as `support`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use crate::model::{Ev, Model};
use crate::paint::{Painter, paint};
use crate::scene::{Handed, Scene, Tone};

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

/// The system allocator, counting each allocation and reallocation on the thread that asks: tests on
/// other threads do not count.
pub struct Counting;

fn bump() {
    let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
}

/// Allocations this thread made so far.
pub fn allocs() -> u64 {
    ALLOCS.try_with(Cell::get).unwrap_or(0)
}

// SAFETY: every call goes to `System` with the caller's arguments unchanged; counting touches only a
// const-initialised thread local, which never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump();
        // SAFETY: forwarded as is.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump();
        // SAFETY: forwarded as is.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        bump();
        // SAFETY: forwarded as is.
        unsafe { System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded as is.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Counts what frames ask of the canvas, and sums the coordinates so the optimiser keeps the maths.
#[derive(Default, Debug)]
pub struct Count {
    pub calls: usize,
    pub strokes: usize,
    pub fills: usize,
    pub glows: usize,
    pub texts: usize,
    pub sum: f64,
}

impl Count {
    fn at(&mut self, x: f32, y: f32) {
        self.calls += 1;
        self.sum += f64::from(x) + f64::from(y);
    }
}

impl Painter for Count {
    fn clear(&mut self, _: f32, _: f32) {
        self.calls += 1;
    }
    fn style(&mut self, _: Tone, alpha: f32, _: f32) {
        self.at(alpha, 0.0);
    }
    fn begin(&mut self) {
        self.calls += 1;
    }
    fn move_to(&mut self, x: f32, y: f32) {
        self.at(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.at(x, y);
    }
    fn circle(&mut self, x: f32, y: f32, r: f32) {
        self.at(x + r, y);
    }
    fn stroke(&mut self) {
        self.calls += 1;
        self.strokes += 1;
    }
    fn fill(&mut self) {
        self.calls += 1;
        self.fills += 1;
    }
    fn glow(&mut self, _: Tone, x: f32, y: f32, r: f32, _: f32) {
        self.at(x + r, y);
        self.glows += 1;
    }
    fn text(&mut self, s: &str, x: f32, y: f32, _: Tone, _: f32) {
        self.at(x, y + s.len() as f32);
        self.texts += 1;
    }
}

/// A collective of `nodes` machines (the first this node, every other linked to it) and `drones`
/// drones spread over them, their states cycling, with a day of mixed traffic: every kind of event,
/// between machines and drones picked by a fixed generator. Parsed as `/api/mesh` sends it.
pub fn model(nodes: usize, drones: usize, events: usize) -> Model {
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut rnd = move |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        usize::try_from(seed >> 33).unwrap_or(0) % n.max(1)
    };
    let mut strs: Vec<String> = (0..nodes).map(|i| format!("\"key-{i}\"")).collect();
    strs.extend((0..drones).map(|i| format!("\"sess-{i}\"")));
    let ns: Vec<String> = (0..nodes).map(|i| format!(r#"{{"key":{i},"host":"host-{i}","me":{},"syncs":true}}"#, i == 0)).collect();
    let ls: Vec<String> = (1..nodes).map(|i| format!(r#"{{"a":0,"b":{i},"dial":true,"ms":900,"ok":{}}}"#, i % 7 != 0)).collect();
    let states = ["working", "waiting", "idle", "ended"];
    let ds: Vec<String> = (0..drones)
        .map(|i| format!(r#"{{"session":{},"node":{},"name":"d{i}","number":{i},"state":"{}"}}"#, nodes + i, i % nodes.max(1), states[i % 4]))
        .collect();
    let session = |d: usize| (nodes + d, d % nodes.max(1));
    // Messages most, then deliveries, lessons, syncs and hand-offs.
    let kinds = [
        "tell",
        "tell",
        "tell",
        "tell",
        "ask",
        "ask",
        "seen",
        "seen",
        "seen",
        "lesson",
        "lesson_update",
        "absorb",
        "release",
        "handoff",
        "handoff",
        "sync",
        "sync",
        "sync_failed",
        "pause",
        "resume",
        "veto",
    ];
    let whys = ["offer", "taken", "back", "declined", "aborted", "done"];
    let evs: Vec<String> = (0..events)
        .map(|i| {
            let kind = kinds[rnd(kinds.len())];
            let (fs, fnode) = session(rnd(drones));
            let (ts, tnode) = session(rnd(drones));
            match kind {
                "seen" => format!(r#"{{"id":"e{i}","ms":{i},"kind":"seen","from_n":{fnode},"from_s":{fs},"re":"e{}"}}"#, i.saturating_sub(1)),
                "lesson" | "lesson_update" | "absorb" | "release" => {
                    format!(r#"{{"id":"e{i}","ms":{i},"kind":"{kind}","from_n":{fnode},"lesson":"L{}","text":"why"}}"#, rnd(40))
                }
                "handoff" => {
                    format!(r#"{{"id":"e{i}","ms":{i},"kind":"handoff","from_n":{fnode},"from_s":{fs},"to_n":{tnode},"why":"{}"}}"#, whys[rnd(whys.len())])
                }
                "sync" | "sync_failed" => format!(r#"{{"id":"e{i}","ms":{i},"kind":"{kind}","from_n":0,"to_n":{},"n":{}}}"#, 1 + rnd(nodes - 1), rnd(5000)),
                // One in forty messages to the whole collective.
                _ if rnd(40) == 0 => format!(r#"{{"id":"e{i}","ms":{i},"kind":"{kind}","from_n":{fnode},"from_s":{fs},"to":"all","text":"all"}}"#),
                _ => {
                    format!(r#"{{"id":"e{i}","ms":{i},"kind":"{kind}","from_n":{fnode},"from_s":{fs},"to":"session","to_n":{tnode},"to_s":{ts},"text":"hi"}}"#)
                }
            }
        })
        .collect();
    let text = format!(
        r#"{{"now":1000,"me":0,"strs":[{}],"nodes":[{}],"links":[{}],"drones":[{}],"events":[{}]}}"#,
        strs.join(","),
        ns.join(","),
        ls.join(","),
        ds.join(","),
        evs.join(",")
    );
    let mut m = Model::default();
    let _ = m.merge(&text);
    m
}

/// How a hand-off event went, as the view reads it.
fn handed(e: &Ev) -> Handed {
    e.why.as_deref().map_or(Handed::Moves, Handed::parse)
}

/// A scene kept busy: before each frame it plays the model's events in turn until `live` effects run.
pub struct World {
    pub model: Model,
    pub scene: Scene,
    pub pen: Count,
    pub t: f64,
    pub frames: usize,
    pub played: usize,
    live: usize,
}

impl World {
    pub fn new(nodes: usize, drones: usize, live: usize) -> Self {
        let model = model(nodes, drones, 4096);
        let mut scene = Scene::default();
        scene.sync(&model);
        Self { model, scene, pen: Count::default(), t: 0.0, frames: 0, played: 0, live }
    }

    /// Tops the effects up to `live`, advances the clock a sixtieth of a second and paints the frame.
    pub fn step(&mut self) {
        let evs = &self.model.events;
        // At most one round of the events, should none of them play.
        for _ in 0..evs.len() {
            if self.scene.fx.len() >= self.live {
                break;
            }
            if let Some(e) = evs.get(self.played % evs.len()) {
                self.scene.play_handed(e.play, handed(e));
            }
            self.played += 1;
        }
        self.t += 1.0 / 60.0;
        self.frames += 1;
        self.scene.frame(self.t);
        paint(&self.scene, &mut self.pen);
    }
}
