//! Draws a `Scene` through a `Painter`: the canvas in the browser (views/cube.rs), a recorder in tests
//! and the bench. Strokes are batched by style, one path per colour and alpha level, and glows are
//! sprites the painter renders once (no `shadowBlur`), so the canvas calls of a frame grow with the
//! machines and drones and the effects' heads, never with the segments of their trails. Effects come
//! grouped by style (`Scene::frame`), each group drawn as one path per alpha level. Nothing here
//! allocates, and nothing is projected twice: anchors come projected from the scene, machine cubes are
//! offset from their projected centre.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::suboptimal_flops,
    reason = "f32 screen graphics: small counters to f32 and alpha levels back; f32::mul_add is a libm call on wasm32 without FMA"
)]

use crate::scene::{Anchor, CORE, CORNERS, DroneState, EDGES, FACES, Fx, FxKind, ORBIT, P2, Scene, TRAIL, TRAIL_STEP, Tone, face_axis, on_face};

/// What a frame draws with: a 2D canvas's few primitives.
pub trait Painter {
    fn clear(&mut self, w: f32, h: f32);
    /// Sets stroke and fill colour, opacity and line width for the strokes and fills that follow.
    fn style(&mut self, tone: Tone, alpha: f32, width: f32);
    fn begin(&mut self);
    fn move_to(&mut self, x: f32, y: f32);
    fn line_to(&mut self, x: f32, y: f32);
    /// Adds a circle as its own subpath.
    fn circle(&mut self, x: f32, y: f32, r: f32);
    fn stroke(&mut self);
    fn fill(&mut self);
    /// A soft glow of radius `r` centred on the point. It leaves the path being built and the style
    /// alone, so a path's glows may be drawn while it is built.
    fn glow(&mut self, tone: Tone, x: f32, y: f32, r: f32, alpha: f32);
    /// A label centred on `x`, its top at `y`; it leaves the style alone.
    fn text(&mut self, s: &str, x: f32, y: f32, tone: Tone, alpha: f32);
}

/// Far things fade: 0.3 at the back of the machines' sphere, 1 at its front.
fn depth(z: f32) -> f32 {
    (0.3 + 0.7 * (z + ORBIT) / (2.0 * ORBIT)).clamp(0.2, 1.0)
}

fn seg<P: Painter>(pen: &mut P, a: P2, b: P2) {
    pen.move_to(a.x, a.y);
    pen.line_to(b.x, b.y);
}

/// Smoothstep of `k` (0 to 1).
fn ease(k: f32) -> f32 {
    let k = k.clamp(0.0, 1.0);
    k * k * (3.0 - 2.0 * k)
}

/// A glow, unless it is invisible or wholly off the canvas.
fn glow<P: Painter>(pen: &mut P, s: &Scene, tone: Tone, at: P2, r: f32, alpha: f32) {
    if alpha > 0.01 && r > 0.5 && at.x + r >= 0.0 && at.y + r >= 0.0 && at.x - r <= s.cam.w && at.y - r <= s.cam.h {
        pen.glow(tone, at.x, at.y, r, alpha);
    }
}

/// Whether the cube edge between corners `a` and `b` borders a face that looks at the camera: the
/// edge lies in the two faces whose axis both corners share (their coordinates there, each ±1, agree).
fn edge_seen(rot: &[f32; 9], a: usize, b: usize) -> bool {
    (0..3).any(|axis| CORNERS[a][axis] * CORNERS[b][axis] > 0.0 && rot[6 + axis] * CORNERS[a][axis] > 0.0)
}

/// Whether face `f` of a cube looks at the camera.
fn facing(rot: &[f32; 9], f: usize) -> bool {
    let (axis, side) = face_axis(f);
    rot[6 + axis] * side > 0.0
}

/// A quadrilateral as its own subpath.
fn quad<P: Painter>(pen: &mut P, q: [P2; 4]) {
    pen.move_to(q[0].x, q[0].y);
    for c in &q[1..] {
        pen.line_to(c.x, c.y);
    }
}

/// One face of the core: dark plating, then its grid of `GRID` cells.
fn face<P: Painter>(pen: &mut P, s: &Scene, rot: &[f32; 9], f: usize, fill: f32, grid: f32) {
    const GRID: usize = 8;
    let at = |a: f32, b: f32| s.cam.project(rot, on_face(f, a, b).map(|c| c * CORE));
    pen.style(Tone::Hull, fill, 1.0);
    pen.begin();
    quad(pen, FACES[f].map(|k| s.p_core[k]));
    pen.fill();
    pen.style(Tone::Green, grid, 1.0);
    pen.begin();
    for line in 1..GRID {
        let u = line as f32 / GRID as f32 * 2.0 - 1.0;
        seg(pen, at(u, -1.0), at(u, 1.0));
        seg(pen, at(-1.0, u), at(1.0, u));
    }
    pen.stroke();
}

/// Alpha levels batched effects are drawn at: one path per group and level.
const LEVELS: f32 = 8.0;

pub fn paint<P: Painter>(s: &Scene, pen: &mut P) {
    let rot = s.cam.matrix();
    pen.clear(s.cam.w, s.cam.h);
    core(s, pen, &rot);
    wiring(s, pen);
    machines(s, pen, &rot);
    drones(s, pen);
    let mut start = 0;
    for run in s.fx.chunk_by(|a, b| a.batch() == b.batch()) {
        effects(s, pen, run, start);
        start += run.len();
    }
}

/// The core: its far plating, the glow inside that breathes with the traffic, then the near plating
/// with its conduits and blinking lights, then its edges.
fn core<P: Painter>(s: &Scene, pen: &mut P, rot: &[f32; 9]) {
    let cam = &s.cam;
    // Far faces first; a face's depth is its normal's (it is the cube's centre plus the normal).
    let mut faces = [(0usize, 0.0f32); 6];
    for (f, slot) in faces.iter_mut().enumerate() {
        let (axis, side) = face_axis(f);
        *slot = (f, rot[6 + axis] * side);
    }
    faces.sort_unstable_by(|a, b| a.1.total_cmp(&b.1));
    for &(f, z) in &faces {
        if z <= 0.0 {
            face(pen, s, rot, f, 0.4, 0.07);
        }
    }
    // Edges no near face shares, faint and behind the near plating.
    pen.style(Tone::Green, 0.22, 1.0);
    pen.begin();
    for (a, b) in EDGES {
        if !edge_seen(rot, a, b) {
            seg(pen, s.p_core[a], s.p_core[b]);
        }
    }
    pen.stroke();
    let c = s.p_center;
    glow(pen, s, Tone::Green, c, c.s * CORE * (1.7 + 0.9 * s.flash), 0.22 + 0.55 * s.flash);
    let blink = s.blink();
    for &(f, z) in &faces {
        if z <= 0.0 {
            continue;
        }
        face(pen, s, rot, f, 0.93 - 0.2 * s.flash, 0.2);
        pen.style(Tone::Green, 0.7 + 0.3 * s.flash, 1.4);
        pen.begin();
        for (cf, a, b) in &s.conduits {
            if usize::from(*cf) == f {
                seg(pen, cam.project(rot, *a), cam.project(rot, *b));
            }
        }
        pen.stroke();
        for (lf, at, phase) in &s.lights {
            if usize::from(*lf) == f && (s.still || (blink * 0.7 + phase).fract() < 0.55) {
                let q = cam.project(rot, *at);
                glow(pen, s, Tone::Bright, q, q.s * 0.16, 0.9);
            }
        }
    }
    pen.style(Tone::Green, 0.8 + 0.2 * s.flash, 1.6);
    pen.begin();
    for (a, b) in EDGES {
        if edge_seen(rot, a, b) {
            seg(pen, s.p_core[a], s.p_core[b]);
        }
    }
    pen.stroke();
}

/// Every machine's conduit to the core, then this node's links, one path per tone.
fn wiring<P: Painter>(s: &Scene, pen: &mut P) {
    pen.style(Tone::Dim, 0.28, 1.0);
    pen.begin();
    for n in &s.p_node {
        seg(pen, s.p_center, *n);
    }
    pen.stroke();
    for tone in [Tone::Green, Tone::Amber, Tone::Red] {
        if !s.links.iter().any(|l| l.2 == tone) {
            continue;
        }
        pen.style(tone, 0.7, 1.4);
        pen.begin();
        for (a, b, t) in &s.links {
            if *t == tone {
                seg(pen, s.anchor_p(Anchor::Node(*a)), s.anchor_p(Anchor::Node(*b)));
            }
        }
        pen.stroke();
    }
}

/// Machines, far to near: a glow, a small cube, the host below it. A machine's cube is small against
/// the camera's distance, so its corners are its projected centre plus the rotated corner offsets.
fn machines<P: Painter>(s: &Scene, pen: &mut P, rot: &[f32; 9]) {
    let turned = CORNERS.map(|c| [rot[0] * c[0] + rot[1] * c[1] + rot[2] * c[2], rot[3] * c[0] + rot[4] * c[1] + rot[5] * c[2]]);
    let near: [bool; 6] = std::array::from_fn(|f| facing(rot, f));
    for &i in &s.order {
        let i = i as usize;
        let (Some(&n), Some(&me), Some(&ok)) = (s.p_node.get(i), s.node_me.get(i), s.node_ok.get(i)) else { continue };
        let fade = depth(n.z);
        let size = if me { 0.17 } else { 0.12 };
        let tone = if !ok {
            Tone::Red
        } else if me {
            Tone::Bright
        } else {
            Tone::Green
        };
        glow(pen, s, tone, n, n.s * size * 3.2, 0.22 * fade);
        let half = size * n.s;
        let corner = turned.map(|c| P2 { x: n.x + c[0] * half, y: n.y - c[1] * half, ..n });
        // Its near faces plated, so the cube reads solid.
        pen.style(Tone::Hull, 0.85 * fade, 1.0);
        pen.begin();
        for (f, corners) in FACES.iter().enumerate() {
            if near[f] {
                quad(pen, corners.map(|c| corner[c]));
            }
        }
        pen.fill();
        pen.style(tone, 0.9 * fade, if me { 1.6 } else { 1.2 });
        pen.begin();
        for (x, y) in EDGES {
            seg(pen, corner[x], corner[y]);
        }
        pen.stroke();
        if let Some(label) = s.node_label.get(i) {
            pen.text(label, n.x, n.y + half * 1.9 + 4.0, if me { Tone::Bright } else { Tone::Green }, fade);
        }
    }
}

/// Drones, one path per state; the working ones glow.
fn drones<P: Painter>(s: &Scene, pen: &mut P) {
    for (state, tone, alpha) in [
        (DroneState::Ended, Tone::Dim, 0.35),
        (DroneState::Idle, Tone::Dim, 0.8),
        (DroneState::Waiting, Tone::Amber, 0.95),
        (DroneState::Working, Tone::Bright, 1.0),
    ] {
        if !s.drone_state.contains(&state) {
            continue;
        }
        let size = if state == DroneState::Ended { 0.018 } else { 0.03 };
        pen.style(tone, alpha, 1.0);
        pen.begin();
        for (d, st) in s.p_drone.iter().zip(&s.drone_state) {
            if *st == state {
                if state == DroneState::Working {
                    glow(pen, s, Tone::Green, *d, (d.s * 0.12).clamp(5.0, 16.0), 0.5);
                }
                pen.circle(d.x, d.y, (d.s * size).clamp(1.2, 5.0));
            }
        }
        pen.fill();
    }
}

/// Whether a path is filled or stroked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ink {
    Stroke,
    Fill,
}

/// Draws a group of effects of one style as one path per alpha level: `alpha` says each one's at its
/// progress, `shape` adds it to the path.
fn levelled<P: Painter>(
    pen: &mut P,
    s: &Scene,
    run: &[Fx],
    (tone, width, ink): (Tone, f32, Ink),
    alpha: impl Fn(&Fx, f32) -> f32,
    mut shape: impl FnMut(&mut P, &Fx, f32),
) {
    let level = |f: &Fx| {
        let u = f.progress(s.t);
        if !(0.0..=1.0).contains(&u) {
            return None;
        }
        // A whole number in 0..=LEVELS.
        let l = (alpha(f, u).clamp(0.0, 1.0) * LEVELS).round() as u32;
        (l > 0).then_some((l, u))
    };
    let mut mask = 0u32;
    for f in run {
        if let Some((l, _)) = level(f) {
            mask |= 1 << l;
        }
    }
    while mask != 0 {
        let l = mask.trailing_zeros();
        mask &= mask - 1;
        pen.style(tone, l as f32 / LEVELS, width);
        pen.begin();
        for f in run {
            if let Some((k, u)) = level(f)
                && k == l
            {
                shape(pen, f, u);
            }
        }
        match ink {
            Ink::Stroke => pen.stroke(),
            Ink::Fill => pen.fill(),
        }
    }
}

/// The progress of an effect that shows now.
fn live(f: &Fx, t: f64) -> Option<f32> {
    let u = f.progress(t);
    (0.0..=1.0).contains(&u).then_some(u)
}

/// A group of effects of one kind and tone, the first of them `fx[start]`.
fn effects<P: Painter>(s: &Scene, pen: &mut P, run: &[Fx], start: usize) {
    let Some(first) = run.first() else { return };
    let tone = first.tone;
    match first.kind {
        FxKind::Packet => packets(s, pen, run, start, tone),
        FxKind::Ring => levelled(
            pen,
            s,
            run,
            (tone, 1.5, Ink::Stroke),
            |_, u| 0.8 * (1.0 - u),
            |pen, f, u| {
                let at = s.anchor_p(f.path[0]);
                pen.circle(at.x, at.y, at.s * (0.08 + 0.9 * u));
            },
        ),
        FxKind::Flash => {
            for f in run {
                if let Some(u) = live(f, s.t) {
                    let at = s.anchor_p(f.path[0]);
                    glow(pen, s, f.tone, at, at.s * (0.1 + 0.25 * u), 0.9 * (1.0 - u));
                }
            }
        }
        FxKind::Shock => levelled(pen, s, run, (tone, 1.8, Ink::Stroke), |_, u| 0.95 * (1.0 - u) * (1.0 - u), |pen, f, u| shock(pen, s, f, u)),
        FxKind::Burst | FxKind::Collapse => {
            let out = first.kind == FxKind::Burst;
            let alpha = move |_: &Fx, u: f32| if out { 0.85 * (1.0 - u) } else { 0.25 + 0.65 * u };
            levelled(pen, s, run, (tone, 2.0, Ink::Stroke), alpha, |pen, _, u| {
                let (inner, outer) = (CORE * 1.25, ORBIT * 0.92);
                let k = if out { ease(u) } else { 1.0 - ease(u) };
                let c = s.p_center;
                pen.circle(c.x, c.y, c.s * (inner + (outer - inner) * k));
            });
        }
        FxKind::Glyph => levelled(pen, s, run, (tone, 1.0, Ink::Fill), glyph_alpha, |pen, f, u| {
            let h = s.along(f, u);
            let r = (h.s * 0.06).clamp(4.0, 9.0);
            glow(pen, s, f.tone, h, r * 2.6, 0.7 * glyph_alpha(f, u));
            pen.move_to(h.x, h.y - r);
            pen.line_to(h.x + 0.7 * r, h.y);
            pen.line_to(h.x, h.y + r);
            pen.line_to(h.x - 0.7 * r, h.y);
        }),
        FxKind::Stream => streams(s, pen, run, tone),
        FxKind::Flicker => {
            let t = s.t;
            let alpha = move |f: &Fx, u: f32| {
                // A fast, uneven flicker, each failure on its own beat, fading.
                let beat = ((t - f.t0) * 15.0 + f.t0 * 7.3).fract();
                (1.0 - 0.6 * u) * if beat < 0.55 { 0.95 } else { 0.15 }
            };
            levelled(pen, s, run, (tone, 2.4, Ink::Stroke), alpha, |pen, f, _| seg(pen, s.anchor_p(f.path[0]), s.anchor_p(f.path[1])));
        }
        FxKind::Lift => {}
    }
}

/// A hand-off glyph's opacity: it fades as it lands, a turned-back one sooner.
fn glyph_alpha(f: &Fx, u: f32) -> f32 {
    let from = if f.reach < 1.0 { 0.6 } else { 0.85 };
    1.0 - ease((u - from) / (1.0 - from))
}

/// An assimilation: a hexagon widening and turning, a new lesson's (`n` 2) with a ring inside it.
fn shock<P: Painter>(pen: &mut P, s: &Scene, f: &Fx, u: f32) {
    const HEX: [(f32, f32); 6] = [(1.0, 0.0), (0.5, 0.866), (-0.5, 0.866), (-1.0, 0.0), (-0.5, -0.866), (0.5, -0.866)];
    let at = s.anchor_p(f.path[0]);
    let big = f.n >= 2;
    let radius = at.s * (0.15 + 0.85 * ease(u)) * if big { 1.0 } else { 0.6 };
    let (sn, cs) = (u * 0.6).sin_cos();
    let corner = |(hx, hy): (f32, f32)| (at.x + (hx * cs - hy * sn) * radius, at.y + (hx * sn + hy * cs) * radius);
    let start = corner(HEX[5]);
    pen.move_to(start.0, start.1);
    for c in HEX {
        let next = corner(c);
        pen.line_to(next.0, next.1);
    }
    if big {
        pen.circle(at.x, at.y, radius * 0.62);
    }
}

/// Packets of one tone, the first `fx[start]`: their trails, one stroke per trail segment's alpha,
/// then their heads, a glow each and one path of bright dots. Their points come from the scene's
/// `trail` when it matches the effects.
fn packets<P: Painter>(s: &Scene, pen: &mut P, run: &[Fx], start: usize, tone: Tone) {
    let trail = if s.trail.len() == s.fx.len() { s.trail.get(start..start + run.len()) } else { None };
    let point = |j: usize, f: &Fx, u: f32, k: usize| trail.and_then(|t| t.get(j)).map_or_else(|| s.along(f, u - TRAIL_STEP * k as f32), |t| t[k]);
    if !s.still {
        // Segment k runs from point k-1 to point k; a segment TRAIL would be drawn at alpha 0.
        for k in 1..TRAIL {
            pen.style(tone, 0.6 * (1.0 - k as f32 / TRAIL as f32), 2.0);
            pen.begin();
            for (j, f) in run.iter().enumerate() {
                let Some(u) = live(f, s.t) else { continue };
                if u - TRAIL_STEP * (k - 1) as f32 > 0.0 {
                    seg(pen, point(j, f, u, k - 1), point(j, f, u, k));
                }
            }
            pen.stroke();
        }
    }
    pen.style(Tone::Bright, 1.0, 1.0);
    pen.begin();
    for (j, f) in run.iter().enumerate() {
        let Some(u) = live(f, s.t) else { continue };
        let head = point(j, f, u, 0);
        glow(pen, s, tone, head, (head.s * 0.16).clamp(6.0, 22.0), 0.95);
        pen.circle(head.x, head.y, (head.s * 0.022).clamp(1.5, 3.5));
    }
    pen.fill();
}

/// Syncs of one tone: each a line of `n` dots running the link, the lead one glowing.
fn streams<P: Painter>(s: &Scene, pen: &mut P, run: &[Fx], tone: Tone) {
    pen.style(tone, 0.9, 1.0);
    pen.begin();
    for f in run {
        let Some(u) = live(f, s.t) else { continue };
        let count = f32::from(f.n.max(1));
        // The dots span up to half the link, closer the more there are.
        let gap = 0.5 / count.max(2.0);
        let span = gap * (count - 1.0);
        let path = s.path_of(f);
        for nth in 0..f.n.max(1) {
            let at = u * (1.0 + span) - gap * f32::from(nth);
            if (0.0..=1.0).contains(&at) {
                let dot = path.at(at);
                if nth == 0 {
                    glow(pen, s, tone, dot, (dot.s * 0.12).clamp(5.0, 14.0), 0.8);
                }
                pen.circle(dot.x, dot.y, (dot.s * 0.02).clamp(1.3, 3.0));
            }
        }
    }
    pen.fill();
}

/// The counting painter and allocator, and a busy world, shared with the bench.
#[cfg(test)]
#[allow(dead_code, reason = "what only the bench reads")]
#[path = "../benches/support/mod.rs"]
pub mod record;

#[cfg(test)]
mod tests {
    use super::record::Count;
    use super::*;
    use crate::model::{EvKind, To};
    use crate::scene::tests::{ev, id, model};

    /// Strokes and fills of a frame with `packets` messages in flight between two drones.
    fn frame_with(packets: usize) -> Count {
        let m = model(6, 12);
        let mut s = Scene::default();
        s.sync(&m);
        s.frame(0.0);
        let (from, to) = (id(&m, "sess-0"), id(&m, "sess-5"));
        for i in 0..packets {
            s.t = i as f64 * 0.001;
            s.play(ev(EvKind::Tell, id(&m, "key-0"), Some(from), To::Session, Some(id(&m, "key-5")), Some(to), 0));
        }
        s.frame(0.5);
        let mut c = Count::default();
        paint(&s, &mut c);
        c
    }

    #[test]
    fn a_warm_frame_allocates_nothing() {
        let mut w = super::record::World::new(20, 120, 400);
        // Warm: the plating made, the effect pool at its high-water mark.
        for _ in 0..240 {
            w.step();
        }
        let before = super::record::allocs();
        for _ in 0..120 {
            w.step();
        }
        let allocs = super::record::allocs() - before;
        assert_eq!(allocs, 0, "{allocs} allocations in 120 frames");
        assert!(w.scene.fx.len() > 300 && w.pen.glows > 0 && w.pen.texts > 0, "a busy scene: {} effects", w.scene.fx.len());
        // And the counter counts.
        let probe = super::record::allocs();
        drop(std::hint::black_box(vec![1u8; 8]));
        assert_eq!(super::record::allocs(), probe + 1);
    }

    #[test]
    fn trails_drawn_from_the_frame_match_trails_worked_out() {
        let mut w = super::record::World::new(8, 40, 200);
        for _ in 0..30 {
            w.step();
        }
        let (mut cached, mut fresh) = (Count::default(), Count::default());
        paint(&w.scene, &mut cached);
        w.scene.trail.clear();
        paint(&w.scene, &mut fresh);
        assert_eq!((cached.calls, cached.strokes), (fresh.calls, fresh.strokes));
        assert!((cached.sum - fresh.sum).abs() < 1e-6 * cached.sum.abs().max(1.0), "{} {}", cached.sum, fresh.sum);
    }

    #[test]
    fn strokes_do_not_grow_with_the_packets_in_flight() {
        let (few, many) = (frame_with(3), frame_with(400));
        assert_eq!(few.strokes, many.strokes);
        assert_eq!(few.fills, many.fills);
        // A head is one glow each.
        assert!(many.glows >= few.glows + 397);
    }

    #[test]
    fn levels_batch_by_alpha_and_skip_what_has_not_started() {
        let m = model(3, 0);
        let mut s = Scene::default();
        s.sync(&m);
        s.frame(0.0);
        for _ in 0..50 {
            s.play(ev(EvKind::Absorb, 0, None, To::Session, None, None, 0));
        }
        s.frame(0.1);
        let mut c = Count::default();
        paint(&s, &mut c);
        let base = {
            let mut s2 = Scene::default();
            s2.sync(&m);
            s2.frame(0.1);
            let mut b = Count::default();
            paint(&s2, &mut b);
            b
        };
        // Fifty rings at one progress: one level, one stroke.
        assert_eq!(c.strokes, base.strokes + 1);
        // A lift waiting for its time draws nothing.
        s.fx.clear();
        s.play(ev(EvKind::Release, 0, None, To::Session, None, None, 0));
        s.fx.retain(|f| f.kind == crate::scene::FxKind::Lift);
        let mut l = Count::default();
        paint(&s, &mut l);
        assert_eq!(l.calls, base.calls);
    }

    #[test]
    fn off_canvas_glows_are_culled() {
        let m = model(3, 0);
        let mut s = Scene::default();
        s.sync(&m);
        s.frame(0.0);
        let mut on = Count::default();
        paint(&s, &mut on);
        // So near that the machines and most of the core's lights are far off the canvas.
        s.cam.zoom = 60.0;
        s.frame(0.0);
        let mut off = Count::default();
        paint(&s, &mut off);
        assert!(off.glows < on.glows, "{} {}", off.glows, on.glows);
    }
}
