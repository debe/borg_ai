//! The Cube's frame cost under fixed work: 50 machines, 300 drones, 1000 lessons in the core and 1000
//! effects running (every kind, topped up from a day of mixed traffic before each frame), 600 frames
//! each advanced, played and painted into a counting painter. The lessons lie in eight areas, so the
//! core builds each of its eight kinds of area once. Prints the canvas calls per frame (and the fill
//! colour changes among them), the core's primitives drawn per frame and the allocations of the
//! measured frames, and fails when there are any. `LESSONS=<n>` sets the lessons (0 for none), `DIVE=1` measures at the bottom of a dive.
//! For instruction counts run it under callgrind, collecting `measure` only:
//!
//! ```text
//! CARGO_PROFILE_BENCH_DEBUG=true cargo bench --bench scene --no-run
//! valgrind --tool=callgrind --cache-sim=yes --branch-sim=yes --toggle-collect='*measure*' target/release/deps/scene-*
//! ```

#![allow(dead_code, unused_imports, reason = "the app's modules, of which the bench drives the scene only; their test imports")]
#![allow(clippy::cast_precision_loss, reason = "counts to f64 for the per-frame averages")]

#[path = "../src/interior.rs"]
mod interior;
#[path = "../src/model.rs"]
mod model;
#[path = "../src/paint.rs"]
mod paint;
#[path = "../src/scene.rs"]
mod scene;
#[path = "../src/volume.rs"]
mod volume;
// Cargo builds benches with `cfg(test)`, where paint.rs already includes the support module.
#[cfg(not(test))]
mod support;
#[cfg(test)]
use paint::record as support;

use support::{World, allocs};

const MACHINES: usize = 50;
const DRONES: usize = 300;
const LIVE: usize = 1000;
const LESSONS: usize = 1000;
const WARM: usize = 300;
const FRAMES: usize = 600;

/// The measured frames; returns the effects running and the core's primitives drawn, summed over them.
#[inline(never)]
fn measure(w: &mut World, frames: usize) -> (usize, usize) {
    let (mut live, mut drawn) = (0, 0);
    for _ in 0..frames {
        w.step();
        live += w.scene.fx.len();
        drawn += w.scene.drawn.len();
    }
    (live, drawn)
}

/// A whole number from the environment, else `default`.
fn env(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn main() {
    let (lessons, dive) = (env("LESSONS", LESSONS), env("DIVE", 0) > 0);
    let kinds = interior::kinds(&support::AREAS);
    assert!(interior::Kind::ALL.iter().all(|k| kinds.contains(k)), "the bench builds every kind of area");
    let mut w = World::new(MACHINES, DRONES, LIVE).with_lessons(lessons);
    w.scene.set_dive(dive);
    // Warm: the plating made, the effect pool at its high-water mark, the dive at its bottom.
    for _ in 0..WARM {
        w.step();
    }
    let (calls, strokes, fills, arcs, glows, texts) = (w.pen.calls, w.pen.strokes, w.pen.fills, w.pen.arcs, w.pen.glows, w.pen.texts);
    let fill_styles = w.pen.fill_styles;
    let (played, before) = (w.played, allocs());
    let (live, drawn) = measure(&mut w, FRAMES);
    let allocated = allocs() - before;
    let per = |n: usize| n as f64 / FRAMES as f64;
    println!(
        "scene: {MACHINES} machines, {DRONES} drones, {lessons} lessons{}, {:.0} effects live on average, {:.1} events played per frame, {FRAMES} frames",
        if dive { " (in a dive)" } else { "" },
        per(live),
        per(w.played - played)
    );
    println!(
        "per frame: {:.0} canvas calls, {:.1} strokes, {:.1} fills, {:.1} fill colours set, {:.0} arcs, {:.0} glows, {:.0} labels",
        per(w.pen.calls - calls),
        per(w.pen.strokes - strokes),
        per(w.pen.fills - fills),
        per(w.pen.fill_styles - fill_styles),
        per(w.pen.arcs - arcs),
        per(w.pen.glows - glows),
        per(w.pen.texts - texts)
    );
    println!("core: {} primitives built, {:.0} drawn per frame", w.scene.vol.inner.prims(), per(drawn));
    println!("allocations in the measured frames: {allocated} (checksum {:.0})", std::hint::black_box(w.pen.sum));
    if allocated > 0 {
        eprintln!("a warm frame allocated");
        std::process::exit(1);
    }
}
