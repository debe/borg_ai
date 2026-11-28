//! Draws a `Scene` through a `Painter`: the canvas in the browser (views/cube.rs), a recorder in tests
//! and the bench. Strokes are batched by style, one path per colour and alpha level, and glows are
//! sprites the painter renders once (no `shadowBlur`), so the canvas calls of a frame grow with the
//! machines and drones and the effects' heads, never with the segments of their trails. The core's
//! interior (interior.rs) comes ordered as it is drawn (`Scene::drawn`): far to near, between the
//! shell's faces that look away and those that look at the camera, each grouped by plating. Each
//! primitive is a subpath of the path its brush builds, consecutive ones of one brush sharing it, so a
//! frame fills and strokes at most once per primitive; plating is one fill of a colour the painter made
//! once (`Painter::plate`). Effects come grouped by style (`Scene::add` keeps `fx` in batch order),
//! each group drawn as one path per alpha level, in a dive in at most two passes (those running to a
//! lesson, and the others faded). Nothing here allocates, and nothing is projected twice: anchors and
//! the interior's points come projected from the scene, machine cubes are offset from their projected
//! centre.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::suboptimal_flops,
    reason = "f32 screen graphics: small counters to f32 and alpha levels back; f32::mul_add is a libm call on wasm32 without FMA"
)]

use crate::interior::{self, Stroke};
use crate::scene::{
    Anchor, CORE, CORNERS, DroneState, EDGES, FACES, Fx, FxKind, ORBIT, P2, Prim, Scene, TRAIL, TRAIL_STEP, Tone, ease, face_axis, facing, lesson_px, on_face,
};

/// What a frame draws with: a 2D canvas's few primitives.
pub trait Painter {
    fn clear(&mut self, w: f32, h: f32);
    /// Sets stroke and fill colour, opacity and line width for the strokes and fills that follow.
    fn style(&mut self, tone: Tone, alpha: f32, width: f32);
    /// Sets the fill to plating at `level` (`plate_level`, one of `PLATES` colours the painter makes
    /// once) and opacity `alpha`, for the fills that follow; strokes keep the style.
    fn plate(&mut self, level: u8, alpha: f32);
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

/// Display: the empty core's near plating, its opacity out of a dive (it fades as the glow flares) and
/// at the bottom of one: a pane in front of the empty inside.
const HULL: f32 = 0.93;
const DIVE_HULL: f32 = 0.06;
/// Display: a consolidated lesson's halo, its opacity.
const HALO: f32 = 0.22;
/// Display: an area's name shows when the area is at least `LABEL_MIN_PX` wide on screen and
/// `LABEL_FIT` of its name's box, so the small areas of a store of many areas do not bury the view in
/// names; the box is `LABEL_CHAR_PX` a character wide (canvas.rs's 11 px monospace font, about 0.6 em)
/// and `LABEL_PAD_PX` more each side.
const LABEL_MIN_PX: f32 = 48.0;
const LABEL_FIT: f32 = 0.6;
const LABEL_CHAR_PX: f32 = 6.6;
const LABEL_PAD_PX: f32 = 6.0;
/// Display: a line of that font, pixels: the height a machine's host name takes below its cube.
const LABEL_LINE_PX: f32 = 13.0;
/// Guard: the most names one frame keeps apart (`labels`), on the stack.
const LABELS: usize = 64;
/// Display: a lesson's light, its opacity: `LIGHT_MIN` for the weakest lesson up to 1 for the strongest,
/// `LIGHT_DORMANT` for a dormant one, at `LIGHT_LEVELS` levels so lights of a tone batch.
const LIGHT_MIN: f32 = 0.35;
const LIGHT_DORMANT: f32 = 0.1;
const LIGHT_LEVELS: f32 = 8.0;
/// Display: the glow round a lesson's light, as a share of its opacity, and its radius in world units,
/// at least `SPILL_PX` pixels.
const SPILL: f32 = 0.6;
const SPILL_R: f32 = 0.035;
const SPILL_PX: f32 = 5.0;
/// Display: a grating's opacity: the deck shows what lies under it.
const GRATING: f32 = 0.55;
/// Display: the shades and the fog levels the core's plating is drawn in: `PLATES` colours in all
/// (`plate_level`), made once by the painter. Shades step by their square root, so the dark plating
/// the shell is made of keeps its variety.
pub const PLATE_SHADES: usize = 24;
pub const PLATE_FOGS: usize = 8;
/// Derived: `PLATE_SHADES` × `PLATE_FOGS`.
pub const PLATES: usize = PLATE_SHADES * PLATE_FOGS;
/// Derived: the depths where plating steps up a fog level (`face_fog`): where `fog` reaches the middle
/// between two of the levels `PLATE_FOGS` rounds it to, (k + 0.5) / 7 for k = 1 to 6 (level 1 is the
/// least: `fog` is never under 0.15).
const FOG_STEPS: [f32; PLATE_FOGS - 2] = [-1.015_748, -0.454_840_8, 0.014_927_79, 0.441_122_8, 0.839_31, 1.217_186];
/// Derived: `fog` at the depth of each fog level a line is drawn at (`fog_level`), worked out once:
/// 0.15 + 0.85 (k / 7)^1.3 for k = 0 to 7, so no frame calls `powf` (a software routine on wasm32).
const FOG_OF: [f32; PLATE_FOGS] = [0.15, 0.217_731_6, 0.316_774_8, 0.432_52, 0.560_647_8, 0.698_848_5, 0.845_645_7, 1.0];
/// Guard: the glows of lessons' lights a batch holds back until its fill (`Batch`), on the stack.
const PENDING: usize = 32;

/// The shade level of `shade` (0 dark, 1 fully lit): its square root in `PLATE_SHADES` steps. Worked
/// out once, when the interior is built (interior.rs `Look::plate`).
#[must_use]
pub fn shade_level(shade: f32) -> u8 {
    (shade.clamp(0.0, 1.0).sqrt() * (PLATE_SHADES - 1) as f32).round() as u8
}

/// The level of plating at shade level `shade` (`shade_level`) and fog level `fog` (0 far to
/// `PLATE_FOGS` - 1 near), for `Painter::plate`.
#[must_use]
pub fn plate_level(shade: u8, fog: u8) -> u8 {
    let (s, f) = (usize::from(shade).min(PLATE_SHADES - 1), usize::from(fog).min(PLATE_FOGS - 1));
    u8::try_from(s * PLATE_FOGS + f).unwrap_or(u8::MAX)
}

/// The shade and fog a plating level stands for (`plate_level` undone).
#[must_use]
pub fn plate_of(level: u8) -> (f32, f32) {
    let (s, f) = (usize::from(level) / PLATE_FOGS, usize::from(level) % PLATE_FOGS);
    let s = s as f32 / (PLATE_SHADES - 1) as f32;
    (s * s, f as f32 / (PLATE_FOGS - 1) as f32)
}

/// Where depth `z` lies in the haze: 0 at the back of the core, 1 at its front.
fn fog_t(z: f32) -> f32 {
    ((z + 1.4) / 2.8).clamp(0.0, 1.0)
}

/// How much of its colour a thing at depth `z` keeps in the haze: 0.15 at the back of the core, 1 at
/// its front. Frames read it rounded (`FOG_OF`, `face_fog`); the tests check those against it.
#[cfg(test)]
fn fog(z: f32) -> f32 {
    0.15 + 0.85 * fog_t(z).powf(1.3)
}

/// `fog` at the middle of fog level `level` of `PLATE_FOGS` (`FOG_OF`), and the level of a depth for
/// a line or a glow.
fn fog_of(level: u8) -> f32 {
    FOG_OF[usize::from(level).min(PLATE_FOGS - 1)]
}

fn fog_level(z: f32) -> u8 {
    (fog_t(z) * (PLATE_FOGS - 1) as f32).round() as u8
}

/// The fog level of plating at depth `z`: `fog` rounded to `PLATE_FOGS` levels, by comparing `z` with
/// `FOG_STEPS` rather than working out `fog`. O(`PLATE_FOGS`), no transcendental call.
fn face_fog(z: f32) -> u8 {
    1 + FOG_STEPS.iter().map(|&step| u8::from(z >= step)).sum::<u8>()
}

/// The plating level (`plate_level`) a face of shade level `shade` at depth `z` is filled with: its
/// brush, by which scene.rs also groups the shell's panels. O(`PLATE_FOGS`).
#[must_use]
pub fn plate_at(shade: u8, z: f32) -> u8 {
    plate_level(shade, face_fog(z))
}

/// Draws the frame. Paint order: the core (far plating, the knowledge inside, near plating), the
/// wiring, machines and drones, the effects, then the core's area names over all of it. In a dive what
/// lies outside the core fades by `Scene::outside` and is skipped when that is 0.
pub fn paint<P: Painter>(s: &Scene, pen: &mut P) {
    let rot = s.cam.matrix();
    pen.clear(s.cam.w, s.cam.h);
    core(s, pen, &rot);
    let out = s.outside();
    if out > 0.0 {
        wiring(s, pen, out);
        machines(s, pen, &rot, out);
        drones(s, pen, out);
    }
    let mut start = 0;
    for run in s.fx.chunk_by(|a, b| a.batch() == b.batch()) {
        passes(s, pen, run, start, out);
        start += run.len();
    }
    labels(s, pen);
}

/// The core. Holding lessons, the glow inside that breathes with the traffic and the Borg cube its
/// interior builds (`knowledge`), its shell included. Empty, its far plating, the edges behind, the
/// glow, then the near plating with its conduits and blinking lights, then its edges.
fn core<P: Painter>(s: &Scene, pen: &mut P, rot: &[f32; 9]) {
    let c = s.p_center;
    let breath = |pen: &mut P| glow(pen, s, Tone::Green, c, c.s * CORE * (1.7 + 0.9 * s.flash), (0.22 + 0.55 * s.flash) * (1.0 - 0.5 * s.dive));
    if !s.vol.is_empty() {
        backdrop(s, pen, rot);
        knowledge(s, pen, rot);
        return;
    }
    let faces = far_to_near(rot);
    for &(f, z) in &faces {
        if z <= 0.0 {
            face(pen, s, rot, f, 0.4, 0.07);
        }
    }
    // Edges no near face shares, faint and behind the near plating.
    edges(s, pen, rot, false, (0.22, 1.0));
    breath(pen);
    near(s, pen, rot, &faces);
    edges(s, pen, rot, true, (0.8 + 0.2 * s.flash, 1.6));
}

/// Behind the knowledge, the core's far faces in the darkest plating, opaque: the shell's panels leave
/// seams, and through them shows this black, not the lit page behind the canvas. Three fills.
fn backdrop<P: Painter>(s: &Scene, pen: &mut P, rot: &[f32; 9]) {
    for (f, z) in far_to_near(rot) {
        if z < 0.0 {
            pen.plate(plate_level(0, 0), 1.0);
            pen.begin();
            quad(pen, FACES[f].map(|k| s.p_core[k]));
            pen.fill();
        }
    }
}

/// The core's faces far to near, each with its depth: its normal's (a face is the cube's centre plus
/// its normal); those at 0 or below look away.
fn far_to_near(rot: &[f32; 9]) -> [(usize, f32); 6] {
    let mut faces = [(0usize, 0.0f32); 6];
    for (f, slot) in faces.iter_mut().enumerate() {
        let (axis, side) = face_axis(f);
        *slot = (f, rot[6 + axis] * side);
    }
    faces.sort_unstable_by(|a, b| a.1.total_cmp(&b.1));
    faces
}

/// The core's edges that border a face looking at the camera (`seen`), or the others, as one path.
fn edges<P: Painter>(s: &Scene, pen: &mut P, rot: &[f32; 9], seen: bool, (alpha, width): (f32, f32)) {
    pen.style(Tone::Green, alpha, width);
    pen.begin();
    for (a, b) in EDGES {
        if edge_seen(rot, a, b) == seen {
            seg(pen, s.p_core[a], s.p_core[b]);
        }
    }
    pen.stroke();
}

/// The empty core's near plating, a pane in a dive; its conduits and lights fade with the outside.
fn near<P: Painter>(s: &Scene, pen: &mut P, rot: &[f32; 9], faces: &[(usize, f32); 6]) {
    let hull = (HULL - 0.2 * s.flash).max(0.0);
    let fill = hull + (DIVE_HULL - hull) * s.dive;
    let out = s.outside();
    for &(f, z) in faces {
        if z > 0.0 {
            face(pen, s, rot, f, fill, 0.2 * (1.0 - 0.6 * s.dive));
            if out > 0.0 {
                conduits(s, pen, rot, f, out);
                lights(s, pen, rot, f, out);
            }
        }
    }
}

/// The conduits on face `f`, as one path, at `fade`.
fn conduits<P: Painter>(s: &Scene, pen: &mut P, rot: &[f32; 9], f: usize, fade: f32) {
    pen.style(Tone::Green, (0.7 + 0.3 * s.flash) * fade, 1.4);
    pen.begin();
    for (cf, a, b) in &s.conduits {
        if usize::from(*cf) == f {
            seg(pen, s.cam.project(rot, *a), s.cam.project(rot, *b));
        }
    }
    pen.stroke();
}

/// The lights on face `f` that are on at the blink clock (all of them when motion is reduced), at `fade`.
fn lights<P: Painter>(s: &Scene, pen: &mut P, rot: &[f32; 9], f: usize, fade: f32) {
    let blink = s.blink();
    for (lf, at, phase) in &s.lights {
        if usize::from(*lf) == f && (s.still || (blink * 0.7 + phase).fract() < 0.55) {
            let q = s.cam.project(rot, *at);
            glow(pen, s, Tone::Bright, q, q.s * 0.16, 0.9 * fade);
        }
    }
}

/// How a run of the interior's primitives is drawn: plating at a level (a grating see-through), a
/// lesson's light in a tone at a level, or a line in a stroke at a fog level.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Brush {
    Plate { level: u8, see: bool },
    Light { tone: Tone, level: u8 },
    Line { stroke: Stroke, fog: u8 },
}

impl Brush {
    fn set<P: Painter>(self, pen: &mut P) {
        match self {
            Self::Plate { level, see } => pen.plate(level, if see { GRATING } else { 1.0 }),
            Self::Light { tone, level } => pen.style(tone, f32::from(level) / LIGHT_LEVELS, 1.0),
            Self::Line { stroke, fog } => {
                let (tone, alpha, width) = stroke.look();
                pen.style(tone, if tone == Tone::Hull { alpha } else { alpha * fog_of(fog) }, width);
            }
        }
    }
}

/// The path being built of consecutive primitives of one brush and one winding on screen, and the glows
/// of its lights, held back until it is filled so they lie over it: a new brush, a face wound the other
/// way, a glow primitive or a full hold ends it with one fill or stroke. So a frame's fills and strokes
/// are at most its primitives drawn. A path holds faces of one winding only: under the canvas's
/// nonzero fill, two faces wound opposite ways would leave open where they overlap.
struct Batch {
    brush: Option<(Brush, bool)>,
    held: [(Tone, P2, f32, f32); PENDING],
    n: usize,
}

impl Batch {
    const fn new() -> Self {
        Self { brush: None, held: [(Tone::Green, P2 { x: 0.0, y: 0.0, s: 0.0, z: 0.0 }, 0.0, 0.0); PENDING], n: 0 }
    }

    /// Continues the path when `brush` and the winding `cw` are the ones it is built with, else ends it
    /// and starts one.
    fn take<P: Painter>(&mut self, pen: &mut P, brush: Brush, cw: bool) {
        if self.brush != Some((brush, cw)) {
            self.end(pen);
            brush.set(pen);
            pen.begin();
            self.brush = Some((brush, cw));
        }
    }

    /// Fills or strokes the path, then draws the glows held back.
    fn end<P: Painter>(&mut self, pen: &mut P) {
        match self.brush.take() {
            Some((Brush::Line { .. }, _)) => pen.stroke(),
            Some(_) => pen.fill(),
            None => {}
        }
        for (tone, at, r, alpha) in &self.held[..self.n] {
            pen.glow(*tone, at.x, at.y, *r, *alpha);
        }
        self.n = 0;
    }

    /// Holds a glow back until the path is filled; a full hold ends the path, and the next primitive
    /// starts its own.
    fn hold<P: Painter>(&mut self, pen: &mut P, g: (Tone, P2, f32, f32)) {
        if self.n == PENDING {
            self.end(pen);
        }
        self.held[self.n] = g;
        self.n += 1;
    }
}

/// The knowledge inside the core: the haze low in it, then every primitive of its interior this frame
/// shows in the order it is drawn (`Scene::drawn`: far to near, the nearer covering the farther, the
/// shell around it), fogged by depth; the glows of
/// the lessons that have one. The names come last (`labels`). O(primitives drawn); consecutive
/// primitives of one brush are one path, so the fills and strokes are at most the primitives drawn, the
/// glows at most the glow primitives and the lights drawn.
fn knowledge<P: Painter>(s: &Scene, pen: &mut P, rot: &[f32; 9]) {
    let (c, k) = (s.p_center, s.p_center.s * CORE);
    glow(pen, s, Tone::Green, P2 { y: c.y + 2.1 * k, ..c }, 4.0 * k, 0.22);
    glow(pen, s, Tone::Green, P2 { y: c.y + 0.5 * k, ..c }, 2.5 * k, 0.1);
    let mut batch = Batch::new();
    let inner = &s.vol.inner;
    let facing: [bool; 6] = std::array::from_fn(|f| facing(rot, f));
    for d in &s.drawn {
        match d.prim {
            Prim::Face(i) => {
                if let Some(f) = inner.faces.get(i as usize) {
                    face_of(s, pen, &mut batch, f, d.z);
                }
            }
            Prim::Line(i) => {
                if let Some(l) = inner.lines.get(i as usize) {
                    line_of(s, pen, (&mut batch, &facing), l, d.z);
                }
            }
            Prim::Glow(i) => {
                batch.end(pen);
                if let Some(g) = inner.glows.get(i as usize) {
                    let p = s.p_pts.get(g.at as usize).copied().unwrap_or_default();
                    glow(pen, s, g.tone, p, g.r * p.s, g.alpha * fog_of(fog_level(d.z)));
                }
            }
            Prim::Label(_) => {}
        }
    }
    batch.end(pen);
    shines(s, pen, rot);
}

/// A face of the interior: its outline (and its hole) as a subpath of its brush's path, fogged by its
/// depth `z`; a lesson's light that spills (`Look::Light`) holds its glow back until the fill.
fn face_of<P: Painter>(s: &Scene, pen: &mut P, batch: &mut Batch, f: &interior::Face, z: f32) {
    let (brush, light) = match f.look {
        interior::Look::Plate(shade) => (Brush::Plate { level: plate_at(shade, z), see: false }, None),
        interior::Look::Grating(shade) => (Brush::Plate { level: plate_at(shade, z), see: true }, None),
        interior::Look::Light { lesson, dim, spill } => {
            let (tone, alpha) = light_of(s, lesson as usize, dim);
            (Brush::Light { tone, level: (alpha * LIGHT_LEVELS).round().clamp(1.0, LIGHT_LEVELS) as u8 }, (spill > 0.0).then_some((tone, alpha * spill)))
        }
    };
    let first = f.first as usize;
    let (n, hole) = (usize::from(f.n), usize::from(f.hole));
    let corners = s.vol.inner.corners.get(first..first + n + hole).unwrap_or_default();
    let (outline, inner) = corners.split_at(n.min(corners.len()));
    batch.take(pen, brush, clockwise(&s.p_pts, outline));
    let mid = subpath(pen, &s.p_pts, outline);
    let _ = subpath(pen, &s.p_pts, inner);
    if let (Some((tone, alpha)), Some(p)) = (light, mid) {
        batch.hold(pen, (tone, p, (p.s * SPILL_R).max(SPILL_PX), SPILL * alpha));
    }
}

/// Whether the points `ix` (as projected, `p`) run clockwise on screen (y down): the sign of the area
/// they outline. O(points).
fn clockwise(p: &[P2], ix: &[u32]) -> bool {
    let at = |i: u32| p.get(i as usize).copied().unwrap_or_default();
    let mut area = 0.0;
    for (k, &i) in ix.iter().enumerate() {
        let (a, b) = (at(i), at(ix[(k + 1) % ix.len()]));
        area += a.x * b.y - b.x * a.y;
    }
    area > 0.0
}

/// The points `ix` (as projected, `p`) as a subpath; returns their middle, if there are any.
fn subpath<P: Painter>(pen: &mut P, p: &[P2], ix: &[u32]) -> Option<P2> {
    let at = |i: u32| p.get(i as usize).copied().unwrap_or_default();
    let first = at(*ix.first()?);
    pen.move_to(first.x, first.y);
    let mut sum = (first.x, first.y);
    for &i in &ix[1..] {
        let q = at(i);
        pen.line_to(q.x, q.y);
        sum = (sum.0 + q.x, sum.1 + q.y);
    }
    let k = 1.0 / ix.len() as f32;
    Some(P2 { x: sum.0 * k, y: sum.1 * k, ..first })
}

/// A line of the interior as a segment of its brush's path: its stroke, or its other stroke on a shell
/// face that looks away (`facing` says which do not); green ones fogged by its depth `z`.
fn line_of<P: Painter>(s: &Scene, pen: &mut P, (batch, facing): (&mut Batch, &[bool; 6]), l: &interior::Line, z: f32) {
    let stroke = if l.gate.away(facing) { l.alt } else { l.style };
    batch.take(pen, Brush::Line { stroke, fog: fog_level(z) }, false);
    let at = |i: u32| s.p_pts.get(i as usize).copied().unwrap_or_default();
    seg(pen, at(l.a), at(l.b));
}

/// The tone and opacity of lesson `i`'s light, `dim` of its full light: as bright as it is strong, out
/// when dormant, full while an event's light on it lasts.
fn light_of(s: &Scene, i: usize, dim: f32) -> (Tone, f32) {
    let v = &s.vol;
    let (Some(&tone), Some(&strength), Some(&dormant)) = (v.tone.get(i), v.strength.get(i), v.dormant.get(i)) else { return (Tone::Dim, 0.0) };
    let base = if dormant { LIGHT_DORMANT } else { (LIGHT_MIN + (1.0 - LIGHT_MIN) * strength) * dim };
    let lit = v.lit.get(i).copied().unwrap_or_default() * dim;
    (lesson_ink(tone), base.max(lit))
}

/// The colour a lesson of `tone` is drawn in: one an outside store holds (the hull's tone) dim.
fn lesson_ink(tone: Tone) -> Tone {
    if tone == Tone::Hull { Tone::Dim } else { tone }
}

/// Each lesson's glow, where it has one (`shine`). O(lessons), and a glow call only for a lesson that
/// is hovered, lit or consolidated and on the canvas.
fn shines<P: Painter>(s: &Scene, pen: &mut P, rot: &[f32; 9]) {
    let view = [rot[6], rot[7], rot[8]];
    for (i, p) in s.p_lesson.iter().enumerate() {
        if let Some((alpha, r)) = shine(s, i, p, view)
            && let Some(&tone) = s.vol.tone.get(i)
        {
            glow(pen, s, lesson_ink(tone), *p, r, alpha);
        }
    }
}

/// A lesson's glow, its opacity and radius, by the light's own drawn size (`lesson_px`): bright when
/// hovered, else the light of an event that named it while that outshines its halo, else a faint halo
/// when it is consolidated and its light looks at the camera (`view`, the camera's depth axis). An
/// event's light is as wide as the halo, 8 px at least, so a burst of them stays lights, not fog.
fn shine(s: &Scene, i: usize, p: &P2, view: [f32; 3]) -> Option<(f32, f32)> {
    let seat = s.vol.inner.seats.get(i).copied().unwrap_or_default();
    let px = lesson_px(seat.r, p.s, s.dive);
    if s.hovered.is_some_and(|h| h as usize == i) {
        return Some((0.9, (px * 4.0).max(10.0)));
    }
    let halo = if seat.looks(view) && s.vol.consolidated.get(i).copied().unwrap_or_default() { HALO } else { 0.0 };
    let lit = s.vol.lit.get(i).copied().unwrap_or_default();
    if lit > halo { Some((lit, (px * 3.0).max(8.0))) } else { (halo > 0.0).then_some((halo, px * 3.0)) }
}

/// The names of the areas, nearest first, each in a dark box over the middle of its top where the area
/// is wide enough on screen for it (`LABEL_MIN_PX`, `LABEL_FIT`), the box lies in the band the text
/// over the canvas leaves free (`Camera::top`, `Camera::bottom`), and it would cover neither an area's
/// name already written nor a machine's host while the machines show (`on_host`). Drawn last, over
/// everything else, brighter in a dive. O(primitives drawn + labels · (`LABELS` + machines)), no
/// allocation.
fn labels<P: Painter>(s: &Scene, pen: &mut P) {
    let mut taken = [(0.0f32, 0.0f32, 0.0f32); LABELS];
    let mut n = 0;
    let alpha = 0.75 + 0.25 * s.dive;
    let (top, bottom) = (s.cam.top, s.cam.h - s.cam.bottom);
    for d in s.drawn.iter().rev() {
        let Prim::Label(i) = d.prim else { continue };
        let Some(l) = s.vol.inner.labels.get(i as usize) else { continue };
        let p = s.p_pts.get(l.at as usize).copied().unwrap_or_default();
        let half = 0.5 * l.title.chars().count() as f32 * LABEL_CHAR_PX + LABEL_PAD_PX;
        if n == LABELS
            || l.span * p.s < LABEL_MIN_PX.max(LABEL_FIT * 2.0 * half)
            || p.y - 11.0 < top
            || p.y + 4.0 > bottom
            || taken[..n].iter().any(|(x, y, w)| (p.x - x).abs() < half + w && (p.y - y).abs() < 16.0)
            || on_host(s, p, half)
        {
            continue;
        }
        taken[n] = (p.x, p.y, half);
        n += 1;
        let q = [(p.x - half, p.y - 11.0), (p.x + half, p.y - 11.0), (p.x + half, p.y + 4.0), (p.x - half, p.y + 4.0)].map(|(x, y)| P2 { x, y, ..p });
        pen.style(Tone::Hull, 0.85 * alpha, 1.0);
        pen.begin();
        quad(pen, q);
        pen.fill();
        pen.style(Tone::Green, 0.9 * alpha, 1.0);
        pen.begin();
        quad(pen, q);
        pen.line_to(q[0].x, q[0].y);
        pen.stroke();
        pen.text(&l.title, p.x, p.y - 9.5, Tone::Green, alpha);
    }
}

/// Whether an area's name box `half` wide either side of `p` (its text's top 9.5 px over `p`) would
/// cover a machine's host name (`host_y`), while the machines show (`Scene::outside`). O(machines +
/// the hosts' characters).
fn on_host(s: &Scene, p: P2, half: f32) -> bool {
    s.outside() > 0.0
        && s.p_node.iter().zip(&s.node_me).zip(&s.node_label).any(|((n, &me), host)| {
            let (top, wide) = (host_y(*n, me), 0.5 * host.chars().count() as f32 * LABEL_CHAR_PX);
            (p.x - n.x).abs() < half + wide && p.y - 11.0 < top + LABEL_LINE_PX && top < p.y + 4.0
        })
}

/// The top of machine `n`'s host name, below its cube (`machine`): the larger cube for this node (`me`).
fn host_y(n: P2, me: bool) -> f32 {
    n.y + machine_size(me) * n.s * 1.9 + 4.0
}

/// A machine's cube's half edge, world units: larger for this node.
const fn machine_size(me: bool) -> f32 {
    if me { 0.17 } else { 0.12 }
}

/// Every machine's conduit to the core, then this node's links, one path per tone, at `fade`.
fn wiring<P: Painter>(s: &Scene, pen: &mut P, fade: f32) {
    pen.style(Tone::Dim, 0.28 * fade, 1.0);
    pen.begin();
    for n in &s.p_node {
        seg(pen, s.p_center, *n);
    }
    pen.stroke();
    for tone in [Tone::Green, Tone::Amber, Tone::Red] {
        if !s.links.iter().any(|l| l.2 == tone) {
            continue;
        }
        pen.style(tone, 0.7 * fade, 1.4);
        pen.begin();
        for (a, b, t) in &s.links {
            if *t == tone {
                seg(pen, s.anchor_p(Anchor::Node(*a)), s.anchor_p(Anchor::Node(*b)));
            }
        }
        pen.stroke();
    }
}

/// What every machine's cube shares in a frame: its corners turned to the camera (x and y, per unit
/// of half edge), and which of its faces look at it.
struct Shape {
    turned: [[f32; 2]; 8],
    near: [bool; 6],
}

/// Machines, far to near, at `fade`. A machine's cube is small against the camera's distance, so its
/// corners are its projected centre plus the rotated corner offsets.
fn machines<P: Painter>(s: &Scene, pen: &mut P, rot: &[f32; 9], fade: f32) {
    let shape = Shape {
        turned: CORNERS.map(|c| [rot[0] * c[0] + rot[1] * c[1] + rot[2] * c[2], rot[3] * c[0] + rot[4] * c[1] + rot[5] * c[2]]),
        near: std::array::from_fn(|f| facing(rot, f)),
    };
    for &i in &s.order {
        machine(s, pen, i as usize, &shape, fade);
    }
}

/// A machine's tone: red when it does not sync, bright for this node, else green.
const fn machine_tone(me: bool, ok: bool) -> Tone {
    if !ok {
        Tone::Red
    } else if me {
        Tone::Bright
    } else {
        Tone::Green
    }
}

/// Machine `i`: a glow, a small cube with its near faces plated, the host below it; farther is fainter.
fn machine<P: Painter>(s: &Scene, pen: &mut P, i: usize, shape: &Shape, fade: f32) {
    let (Some(&n), Some(&me), Some(&ok)) = (s.p_node.get(i), s.node_me.get(i), s.node_ok.get(i)) else { return };
    let fade = depth(n.z) * fade;
    let size = machine_size(me);
    let tone = machine_tone(me, ok);
    glow(pen, s, tone, n, n.s * size * 3.2, 0.22 * fade);
    let half = size * n.s;
    let corner = shape.turned.map(|c| P2 { x: n.x + c[0] * half, y: n.y - c[1] * half, ..n });
    // Its near faces plated, so the cube reads solid.
    pen.style(Tone::Hull, 0.85 * fade, 1.0);
    pen.begin();
    for (corners, _) in FACES.iter().zip(shape.near).filter(|(_, near)| *near) {
        quad(pen, corners.map(|c| corner[c]));
    }
    pen.fill();
    pen.style(tone, 0.9 * fade, if me { 1.6 } else { 1.2 });
    pen.begin();
    for (x, y) in EDGES {
        seg(pen, corner[x], corner[y]);
    }
    pen.stroke();
    if let Some(label) = s.node_label.get(i) {
        pen.text(label, n.x, host_y(n, me), if me { Tone::Bright } else { Tone::Green }, fade);
    }
}

/// Drones, one path per state, at `fade`; the working ones glow.
fn drones<P: Painter>(s: &Scene, pen: &mut P, fade: f32) {
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
        pen.style(tone, alpha * fade, 1.0);
        pen.begin();
        for (d, st) in s.p_drone.iter().zip(&s.drone_state) {
            if *st == state {
                if state == DroneState::Working {
                    glow(pen, s, Tone::Green, *d, (d.s * 0.12).clamp(5.0, 16.0), 0.5 * fade);
                }
                pen.circle(d.x, d.y, (d.s * size).clamp(1.2, 5.0));
            }
        }
        pen.fill();
    }
}

/// Which effects of a group a pass draws, and at what opacity.
#[derive(Clone, Copy)]
struct Look {
    fade: f32,
    /// Only those that run to a lesson (`Some(true)`), only the others, or all.
    lessons: Option<bool>,
}

impl Look {
    const ALL: Self = Self { fade: 1.0, lessons: None };

    fn takes(self, f: &Fx) -> bool {
        self.lessons.is_none_or(|l| f.runs_to_lesson() == l)
    }
}

/// A group of effects of one style, the first `fx[start]`: out of a dive in one pass; in one, those
/// that run to a lesson in full and the others faded by `out` (skipped at 0), so the canvas calls of
/// a group at most double.
fn passes<P: Painter>(s: &Scene, pen: &mut P, run: &[Fx], start: usize, out: f32) {
    if out >= 1.0 {
        effects(s, pen, run, start, Look::ALL);
        return;
    }
    if run.iter().any(Fx::runs_to_lesson) {
        effects(s, pen, run, start, Look { fade: 1.0, lessons: Some(true) });
    }
    if out > 0.0 {
        effects(s, pen, run, start, Look { fade: out, lessons: Some(false) });
    }
}

/// Whether a path is filled or stroked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ink {
    Stroke,
    Fill,
}

/// Draws the effects of a group of one style that `look` takes as one path per alpha level: `alpha`
/// says each one's at its progress (faded by `look`), `shape` adds it to the path.
fn levelled<P: Painter>(
    pen: &mut P,
    s: &Scene,
    run: &[Fx],
    (tone, width, ink, look): (Tone, f32, Ink, Look),
    alpha: impl Fn(&Fx, f32) -> f32,
    mut shape: impl FnMut(&mut P, &Fx, f32),
) {
    let level = |f: &Fx| {
        let u = f.progress(s.t);
        if !(0.0..=1.0).contains(&u) || !look.takes(f) {
            return None;
        }
        // A whole number in 0..=LEVELS.
        let l = (alpha(f, u).clamp(0.0, 1.0) * look.fade * LEVELS).round() as u32;
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

/// The progress of an effect of a group that shows now and that `look` takes.
fn shows(f: &Fx, t: f64, look: Look) -> Option<f32> {
    live(f, t).filter(|_| look.takes(f))
}

/// The effects of a group of one kind and tone that `look` takes, the first of the group `fx[start]`.
fn effects<P: Painter>(s: &Scene, pen: &mut P, run: &[Fx], start: usize, look: Look) {
    let Some(first) = run.first() else { return };
    let tone = first.tone;
    match first.kind {
        FxKind::Packet => packets(s, pen, (run, start), tone, look),
        FxKind::Ring => levelled(
            pen,
            s,
            run,
            (tone, 1.5, Ink::Stroke, look),
            |_, u| 0.8 * (1.0 - u),
            |pen, f, u| {
                let at = s.anchor_p(f.path[0]);
                pen.circle(at.x, at.y, at.s * (0.08 + 0.9 * u));
            },
        ),
        FxKind::Flash => flashes(s, pen, run, look),
        FxKind::Shock => levelled(pen, s, run, (tone, 1.8, Ink::Stroke, look), |_, u| 0.95 * (1.0 - u) * (1.0 - u), |pen, f, u| shock(pen, s, f, u)),
        FxKind::Burst | FxKind::Collapse => {
            let out = first.kind == FxKind::Burst;
            let alpha = move |_: &Fx, u: f32| if out { 0.85 * (1.0 - u) } else { 0.25 + 0.65 * u };
            levelled(pen, s, run, (tone, 2.0, Ink::Stroke, look), alpha, |pen, _, u| {
                let (inner, outer) = (CORE * 1.25, ORBIT * 0.92);
                let k = if out { ease(u) } else { 1.0 - ease(u) };
                let c = s.p_center;
                pen.circle(c.x, c.y, c.s * (inner + (outer - inner) * k));
            });
        }
        FxKind::Glyph => levelled(pen, s, run, (tone, 1.0, Ink::Fill, look), glyph_alpha, |pen, f, u| {
            let h = s.along(f, u);
            let r = (h.s * 0.06).clamp(4.0, 9.0);
            glow(pen, s, f.tone, h, r * 2.6, 0.7 * glyph_alpha(f, u) * look.fade);
            pen.move_to(h.x, h.y - r);
            pen.line_to(h.x + 0.7 * r, h.y);
            pen.line_to(h.x, h.y + r);
            pen.line_to(h.x - 0.7 * r, h.y);
        }),
        FxKind::Stream => streams(s, pen, run, tone, look),
        FxKind::Flicker => {
            let t = s.t;
            let alpha = move |f: &Fx, u: f32| {
                // A fast, uneven flicker, each failure on its own beat, fading.
                let beat = ((t - f.t0) * 15.0 + f.t0 * 7.3).fract();
                (1.0 - 0.6 * u) * if beat < 0.55 { 0.95 } else { 0.15 }
            };
            levelled(pen, s, run, (tone, 2.4, Ink::Stroke, look), alpha, |pen, f, _| seg(pen, s.anchor_p(f.path[0]), s.anchor_p(f.path[1])));
        }
        FxKind::Lift => {}
    }
}

/// Deliveries: a glow each, at the first anchor.
fn flashes<P: Painter>(s: &Scene, pen: &mut P, run: &[Fx], look: Look) {
    for f in run {
        if let Some(u) = shows(f, s.t, look) {
            let at = s.anchor_p(f.path[0]);
            glow(pen, s, f.tone, at, at.s * (0.1 + 0.25 * u), 0.9 * (1.0 - u) * look.fade);
        }
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

/// Packets of one tone that `look` takes, the first of the group `fx[start]`: their trails, one stroke
/// per trail segment's alpha, then their heads, a glow each and one path of bright dots. Their points
/// come from the scene's `trail` when it matches the effects.
fn packets<P: Painter>(s: &Scene, pen: &mut P, (run, start): (&[Fx], usize), tone: Tone, look: Look) {
    let trail = if s.trail.len() == s.fx.len() { s.trail.get(start..start + run.len()) } else { None };
    let point = |j: usize, f: &Fx, u: f32, k: usize| trail.and_then(|t| t.get(j)).map_or_else(|| s.along(f, u - TRAIL_STEP * k as f32), |t| t[k]);
    if !s.still {
        // Segment k runs from point k-1 to point k; a segment TRAIL would be drawn at alpha 0.
        for k in 1..TRAIL {
            pen.style(tone, 0.6 * (1.0 - k as f32 / TRAIL as f32) * look.fade, 2.0);
            pen.begin();
            for (j, f) in run.iter().enumerate() {
                let Some(u) = shows(f, s.t, look) else { continue };
                if u - TRAIL_STEP * (k - 1) as f32 > 0.0 {
                    seg(pen, point(j, f, u, k - 1), point(j, f, u, k));
                }
            }
            pen.stroke();
        }
    }
    pen.style(Tone::Bright, look.fade, 1.0);
    pen.begin();
    for (j, f) in run.iter().enumerate() {
        let Some(u) = shows(f, s.t, look) else { continue };
        let head = point(j, f, u, 0);
        glow(pen, s, tone, head, (head.s * 0.16).clamp(6.0, 22.0), 0.95 * look.fade);
        pen.circle(head.x, head.y, (head.s * 0.022).clamp(1.5, 3.5));
    }
    pen.fill();
}

/// Syncs of one tone that `look` takes: each a line of `n` dots running the link, the lead one glowing.
fn streams<P: Painter>(s: &Scene, pen: &mut P, run: &[Fx], tone: Tone, look: Look) {
    pen.style(tone, 0.9 * look.fade, 1.0);
    pen.begin();
    for f in run {
        let Some(u) = shows(f, s.t, look) else { continue };
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
                    glow(pen, s, tone, dot, (dot.s * 0.12).clamp(5.0, 14.0), 0.8 * look.fade);
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
    use crate::scene::tests::{ev, id, model, with_lessons};

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

    /// Allocations in `frames` frames of the world.
    fn allocs_in(w: &mut super::record::World, frames: usize) -> u64 {
        let before = super::record::allocs();
        for _ in 0..frames {
            w.step();
        }
        super::record::allocs() - before
    }

    #[test]
    fn a_warm_frame_allocates_nothing() {
        let mut w = super::record::World::new(20, 120, 400);
        // Warm: the plating made, the effect pool at its high-water mark.
        let _ = allocs_in(&mut w, 240);
        let allocs = allocs_in(&mut w, 120);
        assert_eq!(allocs, 0, "{allocs} allocations in 120 frames");
        assert!(w.scene.fx.len() > 300 && w.pen.glows > 0 && w.pen.texts > 0, "a busy scene: {} effects", w.scene.fx.len());
        // And the counter counts.
        let probe = super::record::allocs();
        drop(std::hint::black_box(vec![1u8; 8]));
        assert_eq!(super::record::allocs(), probe + 1);
    }

    #[test]
    fn a_warm_frame_with_a_thousand_lessons_allocates_nothing_outside_or_in_a_dive() {
        let mut w = super::record::World::new(20, 120, 400).with_lessons(1000);
        let _ = allocs_in(&mut w, 240);
        let (moves, texts) = (w.pen.moves, w.pen.texts);
        let outside = allocs_in(&mut w, 120);
        assert_eq!(outside, 0, "{outside} allocations in 120 frames");
        // The shell and a light of about every other lesson (those that look at the camera), every frame.
        assert!(w.pen.moves - moves >= 120 * 1500, "the lessons drawn: {} a frame", (w.pen.moves - moves) / 120);
        assert!(
            w.scene.fx.iter().any(|f| f.path.contains(&Anchor::Lesson(0)) || f.path[1..].iter().any(|a| matches!(a, Anchor::Lesson(_)))),
            "packets run to lessons"
        );
        // Into the dive and at its bottom: the lens, the fading, the passes, the labels.
        w.scene.set_dive(true);
        let (texts_outside, texts) = (w.pen.texts - texts, w.pen.texts);
        let dive = allocs_in(&mut w, 120);
        assert_eq!(dive, 0, "{dive} allocations in 120 frames of a dive");
        assert!(w.scene.dive > DIVE_HIDE_TEST, "{}", w.scene.dive);
        assert!(w.pen.texts > texts && texts_outside > 0, "labels drawn");
        w.scene.hovered = Some(3);
        assert_eq!(allocs_in(&mut w, 60), 0, "hovered");
    }

    const DIVE_HIDE_TEST: f32 = crate::scene::DIVE_HIDE;

    /// A frame of `n` lessons and three machines, nothing running, hovered or lit, at dive `dive` (0 or 1).
    fn lessons_frame(n: usize, dive: bool) -> Count {
        let (_, s) = with_lessons(3, n);
        frame_of(s, dive)
    }

    fn frame_of(mut s: Scene, dive: bool) -> Count {
        s.still = true;
        s.set_dive(dive);
        s.frame(2.0);
        let mut c = Count::default();
        paint(&s, &mut c);
        c
    }

    /// The fills and strokes of the knowledge in a frame of `n` lessons (the frame less the same frame
    /// with an empty core's plating), and the primitives it drew.
    fn knowledge_calls(n: usize, dive: bool) -> (usize, usize) {
        let (_, mut s) = with_lessons(3, n);
        s.still = true;
        s.set_dive(dive);
        s.frame(2.0);
        let mut c = Count::default();
        paint(&s, &mut c);
        let drawn = s.drawn.iter().filter(|d| !matches!(d.prim, Prim::Label(_) | Prim::Glow(_))).count();
        (c.fills + c.strokes, drawn)
    }

    #[test]
    fn a_frame_fills_and_strokes_at_most_once_per_primitive_and_grows_with_the_lessons() {
        for dive in [false, true] {
            let mut per = Vec::new();
            for n in [100, 1000, 3000] {
                let (calls, drawn) = knowledge_calls(n, dive);
                // The machines, drones and labels add their own; the interior at most one per primitive.
                assert!(calls <= drawn + 64 * 2 + 40, "dive {dive}, {n} lessons: {calls} fills and strokes for {drawn} primitives");
                per.push((calls, n));
            }
            // Linear in the lessons: past the shell's fixed share, a bounded number a lesson.
            let (c1, n1) = per[1];
            let (c2, n2) = per[2];
            assert!((c2 - c1) / (n2 - n1) <= 40, "dive {dive}: {} a lesson", (c2 - c1) / (n2 - n1));
        }
    }

    #[test]
    fn consecutive_primitives_of_one_brush_share_a_path() {
        let (_, mut s) = with_lessons(3, 600);
        s.still = true;
        s.set_dive(true);
        s.frame(2.0);
        let mut c = Count::default();
        knowledge(&s, &mut c, &s.cam.matrix());
        let drawn = s.drawn.iter().filter(|d| matches!(d.prim, Prim::Face(_) | Prim::Line(_))).count();
        let labels = s.drawn.iter().filter(|d| matches!(d.prim, Prim::Label(_))).count();
        assert!(c.fills + c.strokes < drawn + 2 * labels, "{} fills and strokes, {drawn} primitives", c.fills + c.strokes);
        assert!(c.fills + c.strokes < drawn * 9 / 10, "runs of one brush merge: {} for {drawn}", c.fills + c.strokes);
    }

    #[test]
    fn faces_wound_the_other_way_on_screen_are_filled_apart() {
        use crate::interior::{Face, Gate, Interior, Look, Pin};
        use crate::scene::Drawn;
        let square = |first: u32| Face { first, n: 4, hole: 0, look: Look::plate(0.3), normal: [0.0; 3], gate: Gate::Always, fine: false, pin: Pin::default() };
        // Two squares of one plating that overlap: wound alike they share a fill; wound apart, the
        // nonzero rule would leave their overlap open, so each takes its own.
        let fills = |second: [u32; 4]| {
            let mut s = Scene::default();
            let corners = [0, 1, 2, 3].into_iter().chain(second).collect();
            s.vol.inner = Interior { points: vec![[0.0; 3]; 8], corners, faces: vec![square(0), square(4)], ..Interior::default() };
            s.p_pts = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0), (5.0, 5.0), (15.0, 5.0), (15.0, 15.0), (5.0, 15.0)]
                .map(|(x, y)| P2 { x, y, s: 1.0, z: 0.0 })
                .to_vec();
            s.drawn = vec![Drawn { prim: Prim::Face(0), z: 0.0, key: 0 }, Drawn { prim: Prim::Face(1), z: 0.0, key: 0 }];
            let mut c = Count::default();
            knowledge(&s, &mut c, &s.cam.matrix());
            c.fills
        };
        assert_eq!(fills([4, 5, 6, 7]), 1);
        assert_eq!(fills([7, 6, 5, 4]), 2);
    }

    /// The gate of a primitive of `s`'s interior.
    fn gate_of(s: &Scene, p: Prim) -> interior::Gate {
        let inner = &s.vol.inner;
        match p {
            Prim::Face(i) => inner.faces[i as usize].gate,
            Prim::Line(i) => inner.lines[i as usize].gate,
            Prim::Glow(i) => inner.glows[i as usize].gate,
            Prim::Label(_) => interior::Gate::Always,
        }
    }

    #[test]
    fn the_inside_is_drawn_far_to_near_between_the_shells_far_and_near_faces() {
        let (_, mut s) = with_lessons(3, 400);
        s.still = true;
        s.frame(2.0);
        let rot = s.cam.matrix();
        // The shell's faces that look away first, then the rest, then the faces that look at the camera.
        let side = |d: &crate::scene::Drawn| match gate_of(&s, d.prim) {
            interior::Gate::Shell { face, .. } => 2 * u8::from(facing(&rot, usize::from(face))),
            _ => 1,
        };
        let sides: Vec<u8> = s.drawn.iter().map(side).collect();
        assert!(sides.windows(2).all(|w| w[0] <= w[1]), "far shell, inside, near shell");
        assert!(sides.contains(&0) && sides.contains(&2));
        let z: Vec<usize> = s.drawn.iter().filter(|d| side(d) == 1).map(|d| crate::scene::depth_bucket(d.z)).collect();
        assert!(z.windows(2).all(|w| w[0] <= w[1]), "far first");
        assert!(z.len() > 1000, "{}", z.len());
        // Every light that looks at the camera is among them, a lesson's light after its plating.
        let view = [rot[6], rot[7], rot[8]];
        let lights = s.vol.inner.faces.iter().filter(|f| matches!(f.look, interior::Look::Light { .. }) && f.gate.shows(f.normal, view, &[false; 6])).count();
        let drawn =
            s.drawn.iter().filter(|d| matches!(d.prim, Prim::Face(i) if matches!(s.vol.inner.faces[i as usize].look, interior::Look::Light { .. }))).count();
        assert_eq!(drawn, lights);
    }

    #[test]
    fn a_shell_face_is_filled_once_per_plating_level_it_shows() {
        for dive in [false, true] {
            let (_, mut s) = with_lessons(3, 1000);
            s.still = true;
            s.set_dive(dive);
            s.frame(2.0);
            // Each panel the frame draws, by its face and the plating it is filled with, in draw order.
            let panels: Vec<(u8, u8)> = s
                .drawn
                .iter()
                .filter_map(|d| {
                    let Prim::Face(i) = d.prim else { return None };
                    let f = &s.vol.inner.faces[i as usize];
                    let (interior::Gate::Shell { face, .. }, interior::Look::Plate(shade)) = (f.gate, f.look) else { return None };
                    Some((face, plate_at(shade, d.z)))
                })
                .collect();
            let runs = 1 + panels.windows(2).filter(|w| w[0] != w[1]).count();
            let mut kinds = panels.clone();
            kinds.sort_unstable();
            kinds.dedup();
            assert_eq!(runs, kinds.len(), "dive {dive}: the panels of a face and level drawn apart");
            assert!(panels.len() > 3 * kinds.len(), "dive {dive}: {} panels in {} fills", panels.len(), kinds.len());
            // And so the frame's fills: the panels' runs, not the panels.
            let mut c = Count::default();
            knowledge(&s, &mut c, &s.cam.matrix());
            let inside = s.drawn.iter().filter(|d| !matches!(gate_of(&s, d.prim), interior::Gate::Shell { .. })).count();
            assert!(c.fills <= inside + kinds.len(), "dive {dive}: {} fills, {inside} inside, {} shell runs", c.fills, kinds.len());
        }
    }

    #[test]
    fn fine_detail_shows_only_where_the_core_is_large_on_screen() {
        let (_, mut s) = with_lessons(3, 500);
        s.still = true;
        let fine = |s: &Scene| s.drawn.iter().filter(|d| is_fine(s, d.prim)).count();
        // Out of a dive the core is small: no fine detail, every light still.
        s.frame(2.0);
        assert!(s.p_center.s * CORE < crate::scene::FINE_PX);
        let coarse = s.drawn.len();
        assert_eq!(fine(&s), 0);
        let lights = |s: &Scene| {
            s.drawn.iter().filter(|d| matches!(d.prim, Prim::Face(i) if matches!(s.vol.inner.faces[i as usize].look, interior::Look::Light { .. }))).count()
        };
        let lit = lights(&s);
        // The same camera zoomed past the threshold: the same, and the fine detail.
        s.cam.zoom = 3.0;
        s.frame(2.0);
        assert!(s.p_center.s * CORE >= crate::scene::FINE_PX);
        assert!(fine(&s) > 1000 && s.drawn.len() == coarse + fine(&s), "{} fine of {}", fine(&s), s.drawn.len());
        assert_eq!(lights(&s), lit);
    }

    fn is_fine(s: &Scene, p: Prim) -> bool {
        match p {
            Prim::Face(i) => s.vol.inner.faces[i as usize].fine,
            Prim::Line(i) => s.vol.inner.lines[i as usize].fine,
            Prim::Glow(_) | Prim::Label(_) => false,
        }
    }

    #[test]
    fn the_shell_opens_the_faces_that_look_at_the_camera() {
        let (_, mut s) = with_lessons(3, 300);
        s.still = true;
        s.frame(2.0);
        let rot = s.cam.matrix();
        let panels = |near: bool| {
            s.drawn
                .iter()
                .filter(|d| matches!(d.prim, Prim::Face(i) if matches!(s.vol.inner.faces[i as usize].gate, interior::Gate::Shell { face, .. } if facing(&rot, usize::from(face)) == near)))
                .count()
        };
        // Three faces look at the camera and three away: the far ones nearly whole, the near ones a ring.
        assert!(panels(false) > 3 * 230 && panels(true) < 3 * 160, "{} far, {} near", panels(false), panels(true));
    }

    #[test]
    fn plating_levels_round_trip_and_brighten_with_shade_and_nearness() {
        let fogs = u8::try_from(PLATE_FOGS - 1).unwrap_or(0);
        for level in 0..u8::try_from(PLATES).unwrap_or(u8::MAX) {
            let (shade, f) = plate_of(level);
            assert_eq!(plate_level(shade_level(shade), (f * f32::from(fogs)).round() as u8), level);
        }
        assert!(plate_level(shade_level(0.9), 7) > plate_level(shade_level(0.1), 7) && plate_level(shade_level(0.5), 7) > plate_level(shade_level(0.5), 1));
        assert!(fog(1.4) > 0.99 && (fog(-1.4) - 0.15).abs() < 1e-6 && fog_of(fog_level(0.3)) > fog_of(fog_level(-0.3)));
    }

    #[test]
    fn plating_takes_the_fog_level_its_depth_rounds_to_without_working_out_the_fog() {
        let fogs = (PLATE_FOGS - 1) as f32;
        for k in -2000..=2000 {
            let z = k as f32 * 0.001;
            let exact = fog(z) * fogs;
            // At a level's edge rounding may go either way.
            if (exact - exact.floor() - 0.5).abs() > 1e-3 {
                assert_eq!(face_fog(z), exact.round() as u8, "z {z}");
            }
        }
    }

    #[test]
    fn a_lines_fog_is_worked_out_once_per_level() {
        for k in 0..u8::try_from(PLATE_FOGS).unwrap_or(0) {
            let exact = 0.15 + 0.85 * (f32::from(k) / (PLATE_FOGS - 1) as f32).powf(1.3);
            assert!((fog_of(k) - exact).abs() < 1e-6, "level {k}: {} for {exact}", fog_of(k));
        }
        assert_eq!(fog_of(200), 1.0, "past the nearest level, the nearest");
    }

    /// An event's light on a lesson glows as wide as the lesson's own light draws (its halo's size, 8 px
    /// at least), not by the core's size: a replayed burst lighting hundreds of lessons in a close dive
    /// stays a field of lights and does not bury the interior in coloured fog.
    #[test]
    fn an_events_glow_is_sized_by_its_lesson_not_by_the_core() {
        let (_, mut s) = with_lessons(3, 200);
        s.set_dive(true);
        s.cam.zoom = 4.0;
        s.frame(3.0);
        let view = {
            let rot = s.cam.matrix();
            [rot[6], rot[7], rot[8]]
        };
        for i in 0..200 {
            s.vol.light(u32::try_from(i).unwrap_or(0));
        }
        s.frame(3.01);
        for (i, p) in s.p_lesson.iter().enumerate() {
            let seat = s.vol.inner.seats[i];
            let Some((_, r)) = shine(&s, i, p, view) else { continue };
            let px = lesson_px(seat.r, p.s, s.dive);
            assert!(r <= (px * 4.0).max(10.0) + 1e-3, "lesson {i}: a glow {r} px wide round a light {px} px wide");
        }
    }

    #[test]
    fn a_lit_lesson_shines_at_full_light_and_a_dormant_one_dims() {
        let (_, mut s) = with_lessons(3, 50);
        let i = (0..50).find(|&i| !s.vol.dormant[i] && s.vol.strength[i] < 0.5).expect("a weak awake lesson");
        let (_, before) = light_of(&s, i, 1.0);
        assert!(before < 0.7);
        s.vol.light(u32::try_from(i).unwrap_or(0));
        s.frame(s.t + 0.01);
        assert!((light_of(&s, i, 1.0).1 - 1.0).abs() < 0.01, "lit");
        let d = (0..50).find(|&i| s.vol.dormant[i]).expect("a dormant lesson");
        assert!((light_of(&s, d, 1.0).1 - LIGHT_DORMANT).abs() < 1e-6);
    }

    #[test]
    fn deep_in_a_dive_the_outside_is_not_drawn() {
        let (outside, dive) = (lessons_frame(300, false), lessons_frame(300, true));
        // No machine cubes, wiring or drones; the knowledge stays, in its fine detail now. So the number
        // of machines makes no difference there.
        assert!(dive.texts < outside.texts + 30, "{} {}", dive.texts, outside.texts);
        let (_, more) = with_lessons(9, 300);
        let more = frame_of(more, true);
        assert_eq!((more.calls, more.glows, more.texts), (dive.calls, dive.glows, dive.texts));
        // Nor does the plating's detail.
        let (_, mut bare) = with_lessons(3, 300);
        bare.still = true;
        bare.set_dive(true);
        bare.frame(2.0);
        (bare.conduits, bare.lights) = (Vec::new(), Vec::new());
        let mut c = Count::default();
        paint(&bare, &mut c);
        assert_eq!((c.calls, c.glows), (dive.calls, dive.glows));
        assert!(dive.moves > outside.moves && outside.moves >= 300, "{} {}", dive.moves, outside.moves);
        // Halfway, all of it is drawn, fainter.
        let (_, mut s) = with_lessons(3, 300);
        s.set_dive(true);
        s.frame(1.0 + 0.35);
        assert!(s.dive > 0.2 && s.dive < 0.8 && s.outside() > 0.0, "{}", s.dive);
        let mut half = Count::default();
        paint(&s, &mut half);
        let mut names = Texts::default();
        paint(&s, &mut names);
        let hosts = names.texts.iter().filter(|t| s.node_label.contains(&t.0)).count();
        // Its machines, their wiring and their names among it.
        s.order.clear();
        s.p_node.clear();
        let mut bare = Count::default();
        paint(&s, &mut bare);
        assert!(half.strokes > bare.strokes && hosts == 3, "{} {}, {hosts} hosts", half.strokes, bare.strokes);
    }

    #[test]
    fn in_a_dive_only_the_effects_that_run_to_a_lesson_are_drawn() {
        let (m, mut s) = with_lessons(3, 20);
        s.still = true;
        s.set_dive(true);
        s.frame(2.0);
        let count = |s: &Scene| {
            let mut c = Count::default();
            paint(s, &mut c);
            c
        };
        let none = count(&s);
        let (k0, k2) = (id(&m, "key-0"), id(&m, "key-2"));
        s.play(ev(EvKind::Tell, k0, None, To::Local, Some(k2), None, 0));
        s.play(ev(EvKind::Absorb, k0, None, To::Session, None, None, 0));
        s.frame(2.3);
        assert_eq!(count(&s).calls, none.calls, "a message and a burst outside are not drawn");
        s.play(crate::model::Play { lesson: Some(id(&m, "L4")), ..ev(EvKind::Lesson, id(&m, "key-1"), None, To::Session, None, None, 0) });
        s.frame(2.8);
        let lesson = count(&s);
        assert!(lesson.calls > none.calls && lesson.glows > none.glows, "its packet is");
        // On the way out the others show again, faded, in a pass of their own.
        s.still = false;
        s.set_dive(false);
        let mut t = 2.8;
        while s.outside() <= 0.0 {
            t += 1.0 / 60.0;
            s.frame(t);
        }
        assert!(s.dive > 0.5 && t < 3.1, "{} at {t}", s.dive);
        let all = count(&s);
        s.fx.retain(Fx::runs_to_lesson);
        s.trail.clear();
        assert!(all.calls > count(&s).calls);
    }

    #[test]
    fn a_hovered_lesson_glows_and_consolidated_ones_have_a_halo() {
        let (_, mut s) = with_lessons(3, 200);
        s.still = true;
        s.frame(3.0);
        let glows = |s: &Scene| {
            let mut c = Count::default();
            paint(s, &mut c);
            c.glows
        };
        let base = glows(&s);
        // A halo for each consolidated lesson whose light looks at the camera.
        let rot = s.cam.matrix();
        let looks = |n: [f32; 3]| n[0] * rot[6] + n[1] * rot[7] + n[2] * rot[8] >= 0.0;
        let held = s.vol.consolidated.iter().zip(&s.vol.inner.seats).filter(|(c, seat)| **c && looks(seat.normal)).count();
        assert!(held > 5 && held < s.vol.consolidated.iter().filter(|c| **c).count(), "{held}");
        s.vol.consolidated.fill(false);
        assert_eq!(glows(&s) + held, base, "a halo per consolidated lesson on the canvas that looks at it");
        s.hovered = Some(5);
        assert_eq!(glows(&s), base - held + 1, "the hovered one glows");
        // Off the canvas, no glow: so near in a dive that most lessons are outside it.
        s.vol.consolidated.fill(true);
        s.set_dive(true);
        s.cam.zoom = 4.0;
        s.frame(3.1);
        let on = s.p_lesson.iter().filter(|p| p.x >= 0.0 && p.y >= 0.0 && p.x <= s.cam.w && p.y <= s.cam.h).count();
        let all = glows(&s);
        s.vol.consolidated.fill(false);
        assert!(on < 150 && all - glows(&s) <= on, "{} halos, {on} lessons on the canvas", all - glows(&s));
    }

    #[test]
    fn boxes_are_named_where_they_are_wide_enough() {
        let texts = |dive: bool, zoom: f32| {
            let (_, mut s) = with_lessons(3, 600);
            s.still = true;
            s.set_dive(dive);
            s.cam.zoom = zoom;
            // A view too small to name anything.
            if zoom < 0.5 {
                (s.cam.w, s.cam.h) = (120.0, 90.0);
            }
            s.frame(2.0);
            let mut c = Count::default();
            paint(&s, &mut c);
            // The machines' hosts are named too, while the outside shows.
            let hosts = if s.outside() > 0.0 { 3 } else { 0 };
            (c.texts - hosts, s.vol.inner.labels.len())
        };
        let (out, boxes) = texts(false, 1.0);
        let (named, _) = texts(true, 1.0);
        assert!(named > 0 && named <= boxes, "{named} of {boxes}");
        assert!(named >= out, "the dive brings the boxes nearer: {named} against {out}");
        assert!(texts(true, 4.0).0 > 0, "nearer, the boxes on the canvas");
        assert_eq!(texts(true, 0.35).0, 0, "too small to name");
    }

    /// Each text a frame writes, its top, where among the frame's calls it came, and its middle.
    #[derive(Default)]
    struct Texts {
        calls: usize,
        texts: Vec<(String, f32, usize, f32)>,
    }

    impl Painter for Texts {
        fn clear(&mut self, _: f32, _: f32) {}
        fn style(&mut self, _: Tone, _: f32, _: f32) {}
        fn plate(&mut self, _: u8, _: f32) {}
        fn begin(&mut self) {}
        fn move_to(&mut self, _: f32, _: f32) {}
        fn line_to(&mut self, _: f32, _: f32) {}
        fn circle(&mut self, _: f32, _: f32, _: f32) {}
        fn stroke(&mut self) {
            self.calls += 1;
        }
        fn fill(&mut self) {
            self.calls += 1;
        }
        fn glow(&mut self, _: Tone, _: f32, _: f32, _: f32, _: f32) {
            self.calls += 1;
        }
        fn text(&mut self, s: &str, x: f32, y: f32, _: Tone, _: f32) {
            self.calls += 1;
            self.texts.push((s.to_owned(), y, self.calls, x));
        }
    }

    #[test]
    fn area_names_are_written_over_everything_and_only_where_the_text_over_the_canvas_leaves_room() {
        let (_, mut s) = with_lessons(3, 600);
        s.still = true;
        s.cam.zoom = 2.0;
        s.frame(2.0);
        let written = |s: &Scene| {
            let mut t = Texts::default();
            paint(s, &mut t);
            let area = |x: &str| s.vol.inner.labels.iter().any(|l| l.title == x);
            let (names, hosts): (Vec<_>, Vec<_>) = t.texts.into_iter().partition(|x| area(&x.0));
            (names, hosts)
        };
        let (names, hosts) = written(&s);
        assert!(names.len() >= 3 && hosts.len() == 3, "{} names, {} hosts", names.len(), hosts.len());
        // After the machines and their hosts, so no machine covers a name.
        assert!(names.iter().all(|n| hosts.iter().all(|h| n.2 > h.2)), "names before machines");
        // The counters over the top third, the deck over the bottom third: no name under either.
        (s.cam.top, s.cam.bottom) = (s.cam.h / 3.0, s.cam.h / 3.0);
        let (names, _) = written(&s);
        let band = s.cam.top..s.cam.h - s.cam.bottom;
        assert!(names.iter().all(|n| band.contains(&(n.1 - 1.5)) && band.contains(&(n.1 + 13.5))), "{names:?}");
    }

    #[test]
    fn an_area_name_does_not_cover_a_machines_host() {
        let (_, mut s) = with_lessons(12, 600);
        s.still = true;
        let mut seen = 0;
        for k in 0..60 {
            // Out of a dive, near enough that the areas are named among the machines.
            s.cam.zoom = 1.0 + 0.1 * (k % 20) as f32;
            s.cam.yaw = 0.37 * k as f32;
            s.frame(2.0);
            let mut t = Texts::default();
            paint(&s, &mut t);
            let area = |x: &str| s.vol.inner.labels.iter().any(|l| l.title == x);
            let (names, hosts): (Vec<_>, Vec<_>) = t.texts.into_iter().partition(|x| area(&x.0));
            let wide = |x: &str| 0.5 * x.chars().count() as f32 * LABEL_CHAR_PX;
            for n in &names {
                // A name's box: 1.5 px over its text's top to 13.5 px under it.
                let covers = |h: &(String, f32, usize, f32)| {
                    (n.3 - h.3).abs() < wide(&n.0) + LABEL_PAD_PX + wide(&h.0) && n.1 - 1.5 < h.1 + LABEL_LINE_PX && h.1 < n.1 + 13.5
                };
                assert!(!hosts.iter().any(covers), "zoom {} yaw {}: {} over a host", s.cam.zoom, s.cam.yaw, n.0);
            }
            seen += names.len().min(1) * hosts.len().min(1);
        }
        assert!(seen > 20, "{seen} views with names and hosts");
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
