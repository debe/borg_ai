//! The Cube's frame cost under fixed work: 50 machines, 300 drones and 1000 effects running (every
//! kind, topped up from a day of mixed traffic before each frame), 600 frames each advanced, played
//! and painted into a counting painter. Prints the canvas calls per frame and the allocations of the
//! measured frames, and fails when there are any. For instruction counts run it under callgrind,
//! collecting `measure` only:
//!
//! ```text
//! CARGO_PROFILE_BENCH_DEBUG=true cargo bench --bench scene --no-run
//! valgrind --tool=callgrind --cache-sim=yes --branch-sim=yes --toggle-collect='*measure*' target/release/deps/scene-*
//! ```

#![allow(dead_code, unused_imports, reason = "the app's modules, of which the bench drives the scene only; their test imports")]
#![allow(clippy::cast_precision_loss, reason = "counts to f64 for the per-frame averages")]

#[path = "../src/model.rs"]
mod model;
#[path = "../src/paint.rs"]
mod paint;
#[path = "../src/scene.rs"]
mod scene;
// Cargo builds benches with `cfg(test)`, where paint.rs already includes the support module.
#[cfg(not(test))]
mod support;
#[cfg(test)]
use paint::record as support;

use support::{World, allocs};

const MACHINES: usize = 50;
const DRONES: usize = 300;
const LIVE: usize = 1000;
const WARM: usize = 300;
const FRAMES: usize = 600;

/// The measured frames; returns the effects running summed over them.
#[inline(never)]
fn measure(w: &mut World, frames: usize) -> usize {
    let mut live = 0;
    for _ in 0..frames {
        w.step();
        live += w.scene.fx.len();
    }
    live
}

fn main() {
    let mut w = World::new(MACHINES, DRONES, LIVE);
    // Warm: the plating made, the effect pool at its high-water mark.
    for _ in 0..WARM {
        w.step();
    }
    let (calls, strokes, fills, glows, texts) = (w.pen.calls, w.pen.strokes, w.pen.fills, w.pen.glows, w.pen.texts);
    let (played, before) = (w.played, allocs());
    let live = measure(&mut w, FRAMES);
    let allocated = allocs() - before;
    let per = |n: usize| n as f64 / FRAMES as f64;
    println!(
        "scene: {MACHINES} machines, {DRONES} drones, {:.0} effects live on average, {:.1} events played per frame, {FRAMES} frames",
        per(live),
        per(w.played - played)
    );
    println!(
        "per frame: {:.0} canvas calls, {:.1} strokes, {:.1} fills, {:.0} glows, {:.0} labels",
        per(w.pen.calls - calls),
        per(w.pen.strokes - strokes),
        per(w.pen.fills - fills),
        per(w.pen.glows - glows),
        per(w.pen.texts - texts)
    );
    println!("allocations in the measured frames: {allocated} (checksum {:.0})", std::hint::black_box(w.pen.sum));
    if allocated > 0 {
        eprintln!("a warm frame allocated");
        std::process::exit(1);
    }
}
