//! The Cube's world, without the browser: where the machines and drones are, the camera that projects
//! them, and the effects that play what moved (a packet from one drone to another, a flash where one
//! was shown it, a lesson assimilated, a hand-off, a mesh sync). `paint.rs` draws it; `views/cube.rs`
//! feeds it the model and the clock.
//!
//! The collective is the core cube at the origin. Machines sit on a sphere around it, each at a place
//! its key alone decides (`direction`), so a machine keeps its place when others join; this node sits
//! in front. Drones orbit their machine. The collective's lessons fill the core, which is then built as
//! a Borg cube with a light for each lesson (volume.rs, interior.rs). An effect runs along anchors
//! (drone, machine, core, lesson) projected once per frame, so it follows them as the cube turns; a
//! lesson written runs to the lesson's light and lights it.
//!
//! A dive takes the camera into the core (`set_dive`): the lens nears it, the spin slows, and what lies
//! outside fades (`outside`), so the lessons inside are what shows and what a click hits.
//!
//! Steady state allocates nothing: `advance` and `prepare` project into buffers sized by `sync` and
//! `lessons`, the interior's primitives are ordered by counting (`prepare`: far to near, the shell's
//! faces around the rest), and effects live in a pool, kept in style order as they join, that only
//! grows past its high-water mark.
//! `needs_draw` says when a frame would look the same as the last one drawn, so the view can skip it.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::suboptimal_flops,
    reason = "f32 screen graphics: the casts are bounded index and clock maths; f32::mul_add is a libm call on wasm32 without FMA"
)]

use std::collections::HashMap;

use crate::interior::{Face, Gate, Interior, Line, Look, Rng};
use crate::model::{EvKind, Model, Play, To};
use crate::paint::{PLATES, plate_at};
use crate::volume::{Item, Volume};

/// Display: world units, the core cube's half edge is 1.
pub const CORE: f32 = 1.0;
/// Display: the machines' sphere.
pub const ORBIT: f32 = 3.4;
/// Display: the camera's distance from the origin.
const DISTANCE: f32 = 11.0;
/// Display: how far a drone's ring sits from its machine, and how it widens per drone.
const RING: f32 = 0.34;
const RING_STEP: f32 = 0.012;
/// Display: auto-rotation, radians per second: slow enough to read labels.
pub const SPIN: f32 = 0.06;
/// Display: the lights of a core without lessons change state this often per second, so an idle view
/// redraws for them at this rate, not every frame (`moved`).
const BLINK: f64 = 4.0;
/// Display: motion below a pixel is not worth a frame: the auto-rotation moves the outermost machine 10 to 20
/// pixels a second, so an idle view redraws at about that rate, not at the display's.
const PX: f32 = 1.0;
/// Display: how much nearer a dive brings the core: its bounding sphere then spans about 85% of the
/// shorter side of the band the text over the canvas leaves free (2·√3 · 1.02 · 2.65 / `DISTANCE` ≈ 0.85).
pub const DIVE_ZOOM: f32 = 2.65;
/// Guard: the least share of the canvas's height a dive frames the core in, however much of it the text
/// over the canvas covers (`Camera::top`, `Camera::bottom`).
const BAND_MIN: f32 = 0.3;
/// Display: a dive's progress per second, in or out: about two thirds of a second end to end.
const DIVE_RATE: f32 = 1.5;
/// Display: the share of the auto-rotation a full dive takes away, so lessons hold still enough to point at.
const DIVE_SPIN: f32 = 0.8;
/// Display: past this far into a dive, what lies outside the core is not drawn at all (`Scene::outside`).
pub const DIVE_HIDE: f32 = 0.97;
/// Display: a lesson's light's radius in pixels, as a click and its glow reach it: at least
/// `LESSON_MIN_PX`, at most `LESSON_MAX_PX` out of a dive and `LESSON_DIVE_PX` in one, where projection
/// alone makes them larger.
const LESSON_MIN_PX: f32 = 0.8;
const LESSON_MAX_PX: f32 = 7.0;
const LESSON_DIVE_PX: f32 = 12.0;
/// Guard: the most machines or drones the scene keeps, so every index fits a `u32` (wasm32's `usize` is no
/// wider, so in the browser this never cuts anything).
const MAX: usize = u32::MAX as usize;

/// Display: the depth buckets the core's primitives are counted into to draw them far to near: 0.00085
/// world units each, a fifth of the nearer sort a lesson's light takes over its surface
/// (interior.rs `LIGHT_BIAS`), so a light never sorts behind the plating it sits on.
pub const DEPTHS: usize = 4096;
/// Derived: the groups a shell face's primitives are drawn in, in order (`Sight::key`): the backings
/// and then the green of the ribs under its panels, a group per plating level of its panels
/// (`PLATES`), the backings and the green of the lines over them, its hull lights.
const SHELL_GROUPS: usize = PLATES + 5;
/// Derived: the buckets a frame's primitives are counted into (`count_sort`): the six shell faces'
/// groups for the faces that look away, the depths, the same again for the faces that look at the
/// camera. Within a `u16` (`Drawn::key`).
const BUCKETS: usize = DEPTHS + 12 * SHELL_GROUPS;
const _: () = assert!(BUCKETS <= 1 << 16, "a bucket fits a u16");
/// Display: the core's half edge on screen, pixels, below which its fine detail (seams, rails, ribs
/// inside the areas, the alcoves' frames, the tubes over them) is not drawn: out of a dive the core
/// spans less (about 83 px on a 900 px high canvas), a dive brings it past (about 220 px).
pub const FINE_PX: f32 = 120.0;

/// Smoothstep of `k` (0 to 1).
#[must_use]
pub fn ease(k: f32) -> f32 {
    let k = k.clamp(0.0, 1.0);
    k * k * (3.0 - 2.0 * k)
}

/// A lesson's light's radius in pixels, from its world radius `r` (interior.rs `Seat::r`), the scale
/// `s` it projects at, and how far the view has dived: projection makes it larger in a dive, and the
/// clamp lets it.
#[must_use]
pub fn lesson_px(r: f32, s: f32, dive: f32) -> f32 {
    (r * s).clamp(LESSON_MIN_PX, LESSON_MAX_PX + (LESSON_DIVE_PX - LESSON_MAX_PX) * dive)
}

/// The depth bucket of a projected depth `z` (0 farthest): the core's bounding sphere cut into `DEPTHS`.
#[must_use]
pub fn depth_bucket(z: f32) -> usize {
    let reach = CORE * 3.0f32.sqrt();
    (((z + reach) / (2.0 * reach) * DEPTHS as f32).floor().max(0.0) as usize).min(DEPTHS - 1)
}

/// An index as the scene keeps it; `MAX` makes the conversion lossless.
pub fn ix(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

/// A colour of the palette (paint.rs maps it to CSS).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Borg green: links, the core, working drones, tells.
    Green,
    /// Bright green-white: highlights, this machine, lessons.
    Bright,
    /// Waiting, stale, an ask, a declined hand-off.
    Amber,
    /// Veto, pause, failure, mismatch.
    Red,
    /// A delivery, a sync.
    Cyan,
    /// Idle and ended drones, the far side.
    Dim,
    /// The cubes' dark plating.
    Hull,
}

impl Tone {
    pub const ALL: [Self; 7] = [Self::Green, Self::Bright, Self::Amber, Self::Red, Self::Cyan, Self::Dim, Self::Hull];

    /// Its place in `ALL`: the canvas's glow sprite, an effect's batch.
    #[must_use]
    pub const fn ix(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DroneState {
    Working,
    Waiting,
    Idle,
    Ended,
}

impl DroneState {
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "working" => Self::Working,
            "waiting" => Self::Waiting,
            "idle" => Self::Idle,
            _ => Self::Ended,
        }
    }

    /// How fast it turns on its ring, radians per second: working drones circle, idle ones rest.
    const fn speed(self) -> f32 {
        match self {
            Self::Working => 0.5,
            Self::Waiting => 0.12,
            Self::Idle | Self::Ended => 0.0,
        }
    }
}

/// A point an effect passes: it moves with what it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    Core,
    Node(u32),
    Drone(u32),
    /// A lesson in the core's volume, by its index there (`Scene::vol`).
    Lesson(u32),
    /// A lesson an event named that the volume does not hold yet, by the model's string id of its id:
    /// at the core's centre until the lessons that hold it come (`Scene::lessons`), then that lesson.
    Awaited(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FxKind {
    /// A glowing packet with a trail along the anchors (a message, a lesson into the core).
    Packet,
    /// A ring that widens at the first anchor (a broadcast, a drone joining).
    Ring,
    /// A flash at the first anchor (a delivery).
    Flash,
    /// An assimilation at the first anchor: a widening hexagon, and for a new lesson (`n` 2) a ring
    /// behind it.
    Shock,
    /// A ring the core sends out (a lesson absorbed by an outside store).
    Burst,
    /// A ring that falls into the core (a lesson released back).
    Collapse,
    /// A hand-off: a diamond running the path, as far as `reach`.
    Glyph,
    /// A sync: `n` dots running the link.
    Stream,
    /// A failed sync: the link between the first two anchors flickers.
    Flicker,
    /// Lifts the core's glow by `n` tenths when its start comes (a lesson arriving), and lights the
    /// lesson its anchor names, if it names one; never drawn.
    Lift,
}

/// Up to this many anchors an effect passes: drone, machine, (core), machine, drone.
pub const PATH: usize = 5;
/// A packet's head and the points of its trail behind it, each `TRAIL_STEP` of its path further back.
pub const TRAIL: usize = 6;
pub const TRAIL_STEP: f32 = 0.012;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fx {
    pub kind: FxKind,
    pub tone: Tone,
    pub path: [Anchor; PATH],
    pub len: u8,
    /// Start and length, in seconds of the animation clock; a start ahead of the clock waits.
    pub t0: f64,
    pub dur: f32,
    /// How far along its path it gets, 0 to 1: a declined hand-off turns back on the way.
    pub reach: f32,
    /// A count its kind reads: a stream's dots, a shock's rings, a lift's tenths.
    pub n: u8,
}

impl Fx {
    /// Its progress at `t`: below 0 it has not started, above 1 it is over.
    #[must_use]
    pub fn progress(&self, t: f64) -> f32 {
        (t - self.t0) as f32 / self.dur.max(1e-3)
    }

    /// Effects with the same batch draw with the same style, so the scene keeps them together and
    /// paint draws each batch as one path per alpha level.
    #[must_use]
    pub const fn batch(&self) -> u8 {
        self.kind as u8 * 8 + self.tone as u8
    }

    /// Whether it runs to a lesson in the volume, or to one it waits for. O(`PATH`).
    #[must_use]
    pub fn runs_to_lesson(&self) -> bool {
        self.path[..usize::from(self.len).min(PATH)].iter().any(|a| matches!(a, Anchor::Lesson(_) | Anchor::Awaited(_)))
    }
}

/// A projected point: screen position, scale (pixels per world unit there) and depth (larger is nearer).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct P2 {
    pub x: f32,
    pub y: f32,
    pub s: f32,
    pub z: f32,
}

impl P2 {
    /// The point `k` (0 to 1) of the way to `b`. Screen-space, so it lies on the line drawn between them.
    #[must_use]
    pub fn lerp(self, b: Self, k: f32) -> Self {
        Self { x: self.x + (b.x - self.x) * k, y: self.y + (b.y - self.y) * k, s: self.s + (b.s - self.s) * k, z: self.z + (b.z - self.z) * k }
    }
}

/// Yaw about the vertical axis, then pitch about the horizontal one, as a row-major 3×3 matrix.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    /// The user's zoom (wheel, keys); the lens's applies on top.
    pub zoom: f32,
    pub w: f32,
    pub h: f32,
    /// How far a dive has come, 0 to 1 (the scene sets it each frame): the focal length grows to
    /// `DIVE_ZOOM` times and the core moves to the middle of the band between `top` and `bottom`.
    pub lens: f32,
    /// How far the text over the canvas reaches down from its top edge and up from its bottom one,
    /// pixels (the view measures it): a dive frames the core in the band between.
    pub top: f32,
    pub bottom: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self { yaw: 0.6, pitch: 0.38, zoom: 1.0, w: 800.0, h: 520.0, lens: 0.0, top: 0.0, bottom: 0.0 }
    }
}

impl Camera {
    #[must_use]
    pub fn matrix(&self) -> [f32; 9] {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        // Rx(pitch) · Ry(yaw)
        [cy, 0.0, sy, sp * sy, cp, -sp * cy, -cp * sy, sp, cp * cy]
    }

    /// The height of the band between `top` and `bottom`, at least `BAND_MIN` of the canvas.
    fn band(&self) -> f32 {
        (self.h - self.top - self.bottom).max(self.h * BAND_MIN)
    }

    /// Pixels per world unit at the origin: out of a dive by the canvas's shorter side, in one by the
    /// shorter side of the band.
    #[must_use]
    pub fn focal(&self) -> f32 {
        let (all, band) = (self.w.min(self.h), self.w.min(self.band()));
        (all + (band - all) * self.lens) * 1.02 * self.zoom * (1.0 + (DIVE_ZOOM - 1.0) * self.lens)
    }

    #[must_use]
    pub fn project(&self, rot: &[f32; 9], at: [f32; 3]) -> P2 {
        self.project_at(rot, self.focal(), at)
    }

    /// `project` with the focal length (`focal`) worked out once, for many points.
    #[must_use]
    pub fn project_at(&self, rot: &[f32; 9], focal: f32, at: [f32; 3]) -> P2 {
        let x = rot[0] * at[0] + rot[1] * at[1] + rot[2] * at[2];
        let y = rot[3] * at[0] + rot[4] * at[1] + rot[5] * at[2];
        let z = rot[6] * at[0] + rot[7] * at[1] + rot[8] * at[2];
        let scale = focal * DISTANCE / (DISTANCE - z).max(0.5) / DISTANCE;
        // A little below the middle, the counters take the top; in a dive, the band's middle.
        let (over, dive) = (self.h * 0.54, 0.5 * (self.top + self.h - self.bottom));
        P2 { x: self.w * 0.5 + x * scale, y: over + (dive - over) * self.lens - y * scale, s: scale, z }
    }

    /// Keeps the pitch off the poles and the zoom sane. The user's zoom keeps its range in a dive too:
    /// zoomed out as far as it goes, a full dive shows the core about as large as the overview does.
    pub const fn clamp(&mut self) {
        self.pitch = self.pitch.clamp(-1.35, 1.35);
        self.zoom = self.zoom.clamp(0.35, 4.0);
        self.lens = self.lens.clamp(0.0, 1.0);
    }

    /// Whether two cameras differ in anything but the yaw (which `needs_draw` weighs in pixels).
    fn same_but_yaw(&self, o: &Self) -> bool {
        [self.pitch, self.zoom, self.w, self.h, self.lens, self.top, self.bottom].map(f32::to_bits)
            == [o.pitch, o.zoom, o.w, o.h, o.lens, o.top, o.bottom].map(f32::to_bits)
    }
}

/// A 64-bit FNV-1a hash (util.rs `hue` uses the same).
#[must_use]
pub fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// A unit vector `key` alone decides, away from the front cap this node takes.
#[must_use]
pub fn direction(key: &str) -> [f32; 3] {
    let hash = fnv(key);
    let low = (hash & 0xffff_ffff) as f32 / u32::MAX as f32;
    let high = (hash >> 32) as f32 / u32::MAX as f32;
    // y in [-0.8, 0.8], azimuth anywhere but within ±0.5 rad of the front (+z).
    let y = low * 1.6 - 0.8;
    let az = high * (std::f32::consts::TAU - 1.0) + 0.5;
    let r = (1.0 - y * y).sqrt();
    [r * az.sin(), y, r * az.cos()]
}

fn norm(p: [f32; 3]) -> [f32; 3] {
    let l = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt().max(1e-6);
    [p[0] / l, p[1] / l, p[2] / l]
}

/// Machines spread on the sphere: each starts at its `direction` and is pushed off the ones too near,
/// a fixed number of rounds, so the same keys always land in the same places.
#[must_use]
pub fn layout(keys: &[&str], me: usize) -> Vec<[f32; 3]> {
    let mut dirs: Vec<[f32; 3]> = keys.iter().enumerate().map(|(i, k)| if i == me { norm([0.0, 0.18, 1.0]) } else { direction(k) }).collect();
    let count = dirs.len();
    // The nearest two machines may be, as an angle's chord on the unit sphere.
    let want = (4.0 / (count.max(1) as f32)).sqrt().clamp(0.35, 0.9);
    for _ in 0..40 {
        for i in 0..count {
            if i == me {
                continue;
            }
            let here = dirs[i];
            let mut push = [0.0f32; 3];
            for (j, other) in dirs.iter().enumerate() {
                if i == j {
                    continue;
                }
                let dx = [here[0] - other[0], here[1] - other[1], here[2] - other[2]];
                let dist = (dx[0] * dx[0] + dx[1] * dx[1] + dx[2] * dx[2]).sqrt();
                if dist < want {
                    let force = (want - dist) / dist.max(1e-4) * if j == me { 1.0 } else { 0.5 };
                    push = [push[0] + dx[0] * force, push[1] + dx[1] * force, push[2] + dx[2] * force];
                }
            }
            let mut moved = norm([here[0] + push[0], here[1] + push[1], here[2] + push[2]]);
            // Never over the poles, where labels collide.
            moved[1] = moved[1].clamp(-0.85, 0.85);
            dirs[i] = norm(moved);
        }
    }
    dirs.into_iter().map(|p| [p[0] * ORBIT, p[1] * ORBIT, p[2] * ORBIT]).collect()
}

/// The cube's corners and edges (index pairs), half edge 1.
pub const CORNERS: [[f32; 3]; 8] = [[-1., -1., -1.], [1., -1., -1.], [1., 1., -1.], [-1., 1., -1.], [-1., -1., 1.], [1., -1., 1.], [1., 1., 1.], [-1., 1., 1.]];
pub const EDGES: [(usize, usize); 12] = [(0, 1), (1, 2), (2, 3), (3, 0), (4, 5), (5, 6), (6, 7), (7, 4), (0, 4), (1, 5), (2, 6), (3, 7)];
/// Each face's corners in turn, in the order `on_face` walks them: (-1,-1), (1,-1), (1,1), (-1,1).
pub const FACES: [[usize; 4]; 6] = [[0, 3, 7, 4], [1, 2, 6, 5], [0, 4, 5, 1], [3, 7, 6, 2], [0, 1, 2, 3], [4, 5, 6, 7]];

/// A face of a cube: the axis it is normal to and on which side, as `axis * 2 + (side > 0)`.
#[must_use]
pub const fn face_axis(f: usize) -> (usize, f32) {
    (f / 2, if f % 2 == 1 { 1.0 } else { -1.0 })
}

/// Whether face `f` of a cube looks at a camera turned by `rot`.
#[must_use]
pub fn facing(rot: &[f32; 9], f: usize) -> bool {
    let (axis, side) = face_axis(f);
    rot[6 + axis] * side > 0.0
}

/// A point on face `f` of the unit cube at face coordinates `(a, b)`, each in -1..1.
#[must_use]
pub const fn on_face(f: usize, a: f32, b: f32) -> [f32; 3] {
    let (axis, side) = face_axis(f);
    let mut p = [0.0; 3];
    p[axis] = side;
    p[(axis + 1) % 3] = a;
    p[(axis + 2) % 3] = b;
    p
}

/// A conduit on the core: its face and ends.
pub type Conduit = (u8, [f32; 3], [f32; 3]);
/// A light on the core: its face, place and blink phase.
pub type Light = (u8, [f32; 3], f32);

/// The empty core's surface detail, the same every time: conduits along its plating and lights that
/// blink (a core with lessons is plated by its interior's shell instead). Each is on a face, so `paint`
/// draws only the faces that look at the camera.
#[must_use]
pub fn plating() -> (Vec<Conduit>, Vec<Light>) {
    const CELLS: f32 = 8.0;
    let mut rng = Rng::new(0x9e37_79b9_7f4a_7c15);
    let mut rnd = move || rng.f();
    let at = |c: f32| c / CELLS * 2.0 - 1.0;
    let mut lines = Vec::with_capacity(6 * 22);
    let mut lights = Vec::with_capacity(6 * 7);
    for f in 0..6u8 {
        let fu = usize::from(f);
        for _ in 0..22 {
            let along = (rnd() * (CELLS - 1.0)).floor() + 1.0;
            let start = (rnd() * (CELLS - 1.0)).floor();
            let len = 1.0 + (rnd() * 3.0).floor();
            let (a0, b0, a1, b1) = if rnd() < 0.5 {
                (at(along), at(start), at(along), at((start + len).min(CELLS)))
            } else {
                (at(start), at(along), at((start + len).min(CELLS)), at(along))
            };
            lines.push((f, on_face(fu, a0, b0), on_face(fu, a1, b1)));
        }
        for _ in 0..7 {
            let a = at((rnd() * (CELLS - 1.0)).floor() + 1.0);
            let b = at((rnd() * (CELLS - 1.0)).floor() + 1.0);
            lights.push((f, on_face(fu, a, b), rnd() * 10.0));
        }
    }
    (lines, lights)
}

/// How a hand-off event went (the model's `why`): the glyph's colour and how far it gets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Handed {
    /// Offered or taken: the work moves to the machine asked.
    #[default]
    Moves,
    /// Finished there: it arrives with a flash.
    Done,
    /// Taken back by the one who offered it.
    Back,
    /// Turned down: it turns back on the way.
    Declined,
    /// Called off: it dies early.
    Aborted,
}

impl Handed {
    /// From the event's `why` (`offer`, `taken`, `back`, `declined`, `aborted`, `done`).
    #[must_use]
    pub fn parse(why: &str) -> Self {
        match why {
            "done" => Self::Done,
            "back" => Self::Back,
            "declined" => Self::Declined,
            "aborted" => Self::Aborted,
            _ => Self::Moves,
        }
    }

    /// Its tone and how much of the path it runs.
    const fn look(self) -> (Tone, f32) {
        match self {
            Self::Moves => (Tone::Bright, 1.0),
            Self::Done => (Tone::Green, 1.0),
            Self::Back => (Tone::Cyan, 1.0),
            Self::Declined => (Tone::Amber, 0.45),
            Self::Aborted => (Tone::Red, 0.3),
        }
    }
}

/// A sync's dots: one, and one more for every doubling of the lines it moved.
fn dots(lines: u32) -> u8 {
    u8::try_from((u64::from(lines) + 1).ilog2() + 1).unwrap_or(u8::MAX)
}

/// The anchors as last projected: where an effect's path runs on screen.
struct Projected<'a> {
    center: P2,
    node: &'a [P2],
    drone: &'a [P2],
    lesson: &'a [P2],
}

impl Projected<'_> {
    /// Where an anchor is (the core's centre for one that names nothing).
    fn get(&self, a: Anchor) -> P2 {
        match a {
            Anchor::Core | Anchor::Awaited(_) => self.center,
            Anchor::Node(i) => self.node.get(i as usize).copied().unwrap_or(self.center),
            Anchor::Drone(i) => self.drone.get(i as usize).copied().unwrap_or(self.center),
            Anchor::Lesson(i) => self.lesson.get(i as usize).copied().unwrap_or(self.center),
        }
    }

    /// An effect's path with its anchors looked up once, for the points along it.
    fn run(&self, f: &Fx) -> Run {
        let len = usize::from(f.len).clamp(1, PATH);
        let mut at = [self.center; PATH];
        for (p, a) in at.iter_mut().zip(&f.path[..len]) {
            *p = self.get(*a);
        }
        Run { at, len, reach: f.reach.clamp(0.0, 1.0) }
    }
}

/// An effect's path on screen: its projected anchors, how many, and how far along it the effect gets.
pub struct Run {
    at: [P2; PATH],
    len: usize,
    reach: f32,
}

impl Run {
    /// The point `u` (0 to 1) of the way, eased within each leg: packets slow into a machine and speed
    /// out of it.
    #[must_use]
    pub fn at(&self, u: f32) -> P2 {
        if self.len < 2 {
            return self.at[0];
        }
        let seg = u.clamp(0.0, 1.0) * self.reach * (self.len - 1) as f32;
        let i = (seg as usize).min(self.len - 2);
        let part = seg - i as f32;
        self.at[i].lerp(self.at[i + 1], part * part * (3.0 - 2.0 * part))
    }
}

/// What a drawn frame showed: `Scene::needs_draw` weighs the scene now against it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shown {
    drawn: bool,
    t: f64,
    cam: Camera,
    hovered: Option<u32>,
}

/// Everything `paint` draws, in world space and as last projected.
#[derive(Default)]
pub struct Scene {
    pub cam: Camera,
    /// The animation clock, seconds.
    pub t: f64,
    /// Machines: world position, label, whether it is this node, whether it syncs.
    pub node_pos: Vec<[f32; 3]>,
    pub node_label: Vec<String>,
    pub node_me: Vec<bool>,
    pub node_ok: Vec<bool>,
    /// The model's string id of each machine's key.
    pub node_key: Vec<u32>,
    /// Links this node has: machine indices and tone.
    pub links: Vec<(u32, u32, Tone)>,
    /// Drones: machine, state, ring slot (index, ring size) and phase.
    pub drone_node: Vec<u32>,
    pub drone_state: Vec<DroneState>,
    pub drone_slot: Vec<(u32, u32)>,
    pub drone_phase: Vec<f32>,
    pub drone_session: Vec<u32>,
    /// Projected this frame.
    pub p_core: [P2; 8],
    pub p_center: P2,
    pub p_node: Vec<P2>,
    pub p_drone: Vec<P2>,
    /// Draw order of the machines, far to near (indices into `p_node`).
    pub order: Vec<u32>,
    /// The running effects, kept in `Fx::batch` order: each joins at the end of its batch.
    pub fx: Vec<Fx>,
    /// Each effect's trail (`TRAIL`), as `prepare` last projected it: a packet's head and the points
    /// behind it, default for other kinds. Paint reads it when it matches `fx`, else works it out.
    pub trail: Vec<[P2; TRAIL]>,
    /// The core's glow, decaying: a lesson or a burst of traffic lifts it.
    pub flash: f32,
    /// Motion is reduced: no auto-rotation, no orbits, no trails.
    pub still: bool,
    /// The empty core's conduits and lights (`plating`), made on the first frame.
    pub conduits: Vec<Conduit>,
    pub lights: Vec<Light>,
    /// The collective's knowledge inside the core (volume.rs), and each lesson's light as projected
    /// for the last frame drawn (`prepare`): what is on screen, and so what a click hits.
    pub vol: Volume,
    pub p_lesson: Vec<P2>,
    /// The interior's points (`vol.inner`), as projected for the last frame drawn.
    pub p_pts: Vec<P2>,
    /// The interior's primitives the last frame drawn shows, in the order they are drawn (`Sight::key`),
    /// each with its depth: ordered by counting in `prepare` from `keys`, both sized when the lessons
    /// change.
    pub drawn: Vec<Drawn>,
    keys: Vec<Drawn>,
    /// Whether the core is large enough on screen for its fine detail (`FINE_PX`), as last placed.
    fine: bool,
    /// Whether a running effect is drawn, as `advance` last found: in a deep dive only those that run
    /// to a lesson are.
    fx_shown: bool,
    /// The lesson under the pointer, by its index in `vol` (the view sets it): paint gives it a glow.
    pub hovered: Option<u32>,
    /// How far the view has dived into the core, 0 (the collective) to 1 (its knowledge), eased: as
    /// drawn. `set_dive` says where it heads; `advance` moves it there.
    pub dive: f32,
    /// The dive's progress before easing, and whether it heads in.
    dive_k: f32,
    dive_on: bool,
    /// Each lesson of the volume by the model's string id of its own id, for the events that name it.
    by_lesson: HashMap<u32, u32>,
    /// Pixels the outermost drawn point (a corner of the core, or a machine) moves per radian of yaw,
    /// and the fastest drone per second, as last projected: what `needs_draw` weighs motion by.
    spin_px: f32,
    orbit_px: f32,
    by_session: HashMap<u32, u32>,
    by_key: HashMap<u32, u32>,
}

impl Scene {
    /// Takes the machines and drones of `m`; the camera, clock and running effects stay, each effect
    /// still on the machines and drones it named (by key and session, since their indices change),
    /// and ended when one of them is gone. Projects the machines and drones (`place`), which a hit out
    /// of a dive reads; the lessons and the interior stay as the last frame drew them, for the next
    /// frame the view draws to make ready (`prepare`). O(machines + drones + effects + lessons).
    pub fn sync(&mut self, m: &Model) {
        let nodes = || m.nodes.iter().take(MAX);
        let keys: Vec<&str> = nodes().map(|n| m.str(n.key)).collect();
        let me = nodes().position(|n| n.me).unwrap_or(0);
        let old_keys = std::mem::take(&mut self.node_key);
        let old_sessions = std::mem::take(&mut self.drone_session);
        self.node_pos = layout(&keys, me);
        self.node_label = nodes().map(|n| n.host.clone()).collect();
        self.node_me = nodes().map(|n| n.me).collect();
        self.node_ok = nodes().map(|n| n.syncs).collect();
        self.node_key = nodes().map(|n| n.key).collect();
        self.by_key = self.node_key.iter().enumerate().map(|(i, k)| (*k, ix(i))).collect();
        self.links = m
            .links
            .iter()
            .filter_map(|l| {
                let a = *self.by_key.get(&l.a)?;
                let b = *self.by_key.get(&l.b)?;
                let tone = if !l.ok {
                    Tone::Red
                } else if l.ms.is_some_and(|ms| m.now - ms < 3_600_000.0) {
                    Tone::Green
                } else {
                    Tone::Amber
                };
                Some((a, b, tone))
            })
            .collect();
        self.drones(m);
        self.p_node.resize(self.node_pos.len(), P2::default());
        self.p_drone.resize(self.drone_node.len(), P2::default());
        self.order.clear();
        self.order.extend(0..ix(self.node_pos.len()));
        let (by_key, by_session) = (&self.by_key, &self.by_session);
        follow(&mut self.fx, |a| match a {
            // The volume changes in `lessons`, not here.
            Anchor::Core | Anchor::Lesson(_) | Anchor::Awaited(_) => Some(a),
            Anchor::Node(i) => old_keys.get(i as usize).and_then(|k| by_key.get(k)).map(|n| Anchor::Node(*n)),
            Anchor::Drone(i) => old_sessions.get(i as usize).and_then(|s| by_session.get(s)).map(|d| Anchor::Drone(*d)),
        });
        self.trail.clear();
        self.bind(m);
        self.place();
    }

    /// Finds the volume's lessons among the model's strings: an event names its lesson by one.
    fn bind(&mut self, m: &Model) {
        self.by_lesson.clear();
        for (i, id) in self.vol.ids.iter().enumerate() {
            if let Some(s) = m.id(id) {
                self.by_lesson.insert(s, ix(i));
            }
        }
    }

    /// Takes the collective's lessons into the core: the interior is built anew (volume.rs `build`),
    /// each lesson at its light, and the buffers a frame projects and sorts into are sized for
    /// it. An effect on a lesson, and the hovered lesson, follow it to its new index, and end when it
    /// is gone; one waiting for a lesson these bring runs to it from now on, and its lift lights it.
    /// Allocates; O(lessons · log lessons) and the interior's build.
    pub fn lessons(&mut self, items: &[Item], m: &Model) {
        let was = self.vol.build(items);
        self.bind(m);
        let (vol, by_lesson) = (&self.vol, &self.by_lesson);
        let now = |i: u32| was.get(i as usize).and_then(|id| vol.find(id));
        follow(&mut self.fx, |a| match a {
            Anchor::Lesson(i) => now(i).map(Anchor::Lesson),
            Anchor::Awaited(s) => Some(by_lesson.get(&s).map_or(a, |i| Anchor::Lesson(*i))),
            _ => Some(a),
        });
        self.hovered = self.hovered.and_then(now);
        self.trail.clear();
        self.p_lesson.resize(self.vol.len(), P2::default());
        self.p_pts.resize(self.vol.inner.points.len(), P2::default());
        let prims = self.vol.inner.prims();
        (self.keys, self.drawn) = (Vec::with_capacity(prims), Vec::with_capacity(prims));
        self.place();
        self.prepare();
    }

    /// The drones of `m` on their machines: slot i of n on its machine's ring, in start order.
    fn drones(&mut self, m: &Model) {
        let mut per: Vec<u32> = vec![0; self.node_pos.len()];
        self.drone_node.clear();
        self.drone_state.clear();
        self.drone_slot.clear();
        self.drone_phase.clear();
        self.drone_session.clear();
        self.by_session.clear();
        for d in m.drones.iter().take(MAX) {
            let Some(&n) = self.by_key.get(&d.node) else { continue };
            let Some(slot) = per.get_mut(n as usize) else { continue };
            self.by_session.insert(d.session, ix(self.drone_node.len()));
            self.drone_node.push(n);
            self.drone_state.push(DroneState::parse(&d.state));
            self.drone_slot.push((*slot, 0));
            self.drone_phase.push((fnv(m.str(d.session)) % 6283) as f32 / 1000.0);
            self.drone_session.push(d.session);
            *slot += 1;
        }
        for (slot, n) in self.drone_slot.iter_mut().zip(&self.drone_node) {
            slot.1 = per.get(*n as usize).copied().unwrap_or(1);
        }
    }

    /// A drone's ring radius around its machine, in world units.
    fn ring(count: u32) -> f32 {
        RING + RING_STEP * count as f32
    }

    /// A drone's world position at the clock: on its machine's ring, turning while it works.
    fn drone_world(&self, i: usize) -> [f32; 3] {
        let centre = self.drone_node.get(i).and_then(|n| self.node_pos.get(*n as usize)).copied().unwrap_or_default();
        let (slot, count) = self.drone_slot.get(i).copied().unwrap_or((0, 1));
        let state = self.drone_state.get(i).copied().unwrap_or(DroneState::Ended);
        let turned = if self.still { 0.0 } else { (self.t as f32) * state.speed() };
        let phase = self.drone_phase.get(i).copied().unwrap_or(0.0);
        let angle = std::f32::consts::TAU * slot as f32 / count.max(1) as f32 + phase * 0.15 + turned;
        // Ended drones fall to an inner, lower ring.
        let (r, dy) = if state == DroneState::Ended { (Self::ring(count) * 0.6, -0.22) } else { (Self::ring(count), 0.0) };
        let (sin, cos) = angle.sin_cos();
        [centre[0] + cos * r, centre[1] + sin * r * 0.3 + dy, centre[2] + sin * r]
    }

    /// Where an anchor was projected this frame (the core's centre for one that names nothing).
    #[must_use]
    pub fn anchor_p(&self, a: Anchor) -> P2 {
        self.projected().get(a)
    }

    /// The point `u` (0 to 1) of an effect's run along its path, between the anchors as projected this
    /// frame: on the drawn lines, and no projection per point.
    #[must_use]
    pub fn along(&self, f: &Fx, u: f32) -> P2 {
        self.projected().run(f).at(u)
    }

    /// An effect's path on screen, its anchors looked up once: for many points along it.
    #[must_use]
    pub fn path_of(&self, f: &Fx) -> Run {
        self.projected().run(f)
    }

    fn projected(&self) -> Projected<'_> {
        Projected { center: self.p_center, node: &self.p_node, drone: &self.p_drone, lesson: &self.p_lesson }
    }

    /// Heads into the core (`true`) or back out; `advance` moves `dive` there at `DIVE_RATE`.
    pub const fn set_dive(&mut self, on: bool) {
        self.dive_on = on;
    }

    /// Whether the view heads into the core (`set_dive`), wherever the dive is now.
    #[must_use]
    pub const fn diving(&self) -> bool {
        self.dive_on
    }

    /// How much of what lies outside the core shows, 0 to 1: the machines, their wiring, the drones,
    /// the effects that do not run to a lesson, the core's conduits and lights. All of it out of a
    /// dive, fading as it goes in, none past `DIVE_HIDE`, where paint skips it.
    #[must_use]
    pub fn outside(&self) -> f32 {
        if self.dive > DIVE_HIDE { 0.0 } else { 1.0 - self.dive }
    }

    /// Moves the dive toward where it heads by `DIVE_RATE` per second (at once when motion is reduced),
    /// and the camera's lens with it.
    fn glide(&mut self, dt: f32) {
        let to = if self.dive_on { 1.0 } else { 0.0 };
        let step = if self.still { 1.0 } else { DIVE_RATE * dt };
        self.dive_k += (to - self.dive_k).clamp(-step, step);
        self.dive = ease(self.dive_k);
        self.cam.lens = self.dive;
    }

    /// Advances to the clock `t` and makes ready a frame to draw: `advance`, then `prepare`. For the
    /// tests and the bench (built with `cfg(test)`), which draw every frame; the view decides per frame.
    #[cfg(test)]
    pub fn frame(&mut self, t: f64) {
        self.advance(t);
        self.prepare();
    }

    /// Advances to the clock `t`: turns, lifts and decays the core's glow, ends finished effects and
    /// notes whether one left is drawn (deep in a dive only those that run to a lesson are), and
    /// projects the core, machines and drones: all `needs_draw` reads. The view calls it on
    /// every animation frame and `prepare` only for a frame it draws, so a frame it skips costs no
    /// work per lesson. O(machines + drones + effects); O(lessons) only while a lesson's light fades
    /// (`Volume::step`).
    pub fn advance(&mut self, t: f64) {
        if self.conduits.is_empty() {
            (self.conduits, self.lights) = plating();
        }
        let dt = (t - self.t).clamp(0.0, 0.25) as f32;
        self.t = t;
        self.glide(dt);
        if !self.still {
            self.cam.yaw += SPIN * (1.0 - DIVE_SPIN * self.dive) * dt;
        }
        self.flash = (self.flash - dt * 0.8).max(0.0);
        self.vol.step(t);
        self.lifts(t);
        self.fx.retain(|f| if f.kind == FxKind::Lift { f.t0 > t } else { f.progress(t) <= 1.0 });
        self.place();
        let out = self.outside() > 0.0;
        self.fx_shown = self.fx.iter().any(|f| f.kind != FxKind::Lift && (out || f.runs_to_lesson()));
    }

    /// Makes ready the frame `advance` reached for paint: projects the lessons and the interior's
    /// points (only those of what is not fine detail while the core is small on screen), orders what
    /// of the interior shows as it is drawn (`sort_prims`), and works out the trail of each packet drawn
    /// once, for every alpha level paint strokes it at. Allocates nothing once the lessons and effects
    /// are sized. O(lessons + the interior's points and primitives + `BUCKETS` + effects).
    pub fn prepare(&mut self) {
        self.place_lessons();
        self.sort_prims();
        self.trail.resize(self.fx.len(), [P2::default(); TRAIL]);
        let out = self.outside() > 0.0;
        let at = Projected { center: self.p_center, node: &self.p_node, drone: &self.p_drone, lesson: &self.p_lesson };
        for (pts, f) in self.trail.iter_mut().zip(&self.fx) {
            if f.kind == FxKind::Packet && (out || f.runs_to_lesson()) {
                let (run, u) = (at.run(f), f.progress(self.t));
                for (k, p) in pts.iter_mut().enumerate() {
                    *p = run.at(u - TRAIL_STEP * k as f32);
                }
            }
        }
    }

    /// Lifts the core's glow for each lift whose start came, and lights the lesson it names.
    fn lifts(&mut self, t: f64) {
        for f in &self.fx {
            if f.kind == FxKind::Lift && f.t0 <= t {
                self.flash += f32::from(f.n) * 0.1;
                if let Anchor::Lesson(i) = f.path[0] {
                    self.vol.light(i);
                }
            }
        }
        self.flash = self.flash.min(1.0);
    }

    /// Projects the core, machines and drones at the camera and clock, orders the machines far to near,
    /// and measures how fast the view moves on screen for `needs_draw`: the farthest drawn point from
    /// the vertical axis, in pixels per radian of yaw, and the fastest drone. The core's corners stand
    /// for its lessons: every lesson lies inside the core (`volume::FILL`), nearer the vertical axis
    /// than a corner, and the nearest corner has the largest scale of any point in the box, so a turn
    /// that moves no corner a pixel moves no lesson one either. O(machines + drones).
    fn place(&mut self) {
        let m = self.cam.matrix();
        for (p, c) in self.p_core.iter_mut().zip(CORNERS) {
            *p = self.cam.project(&m, c);
        }
        self.p_center = self.cam.project(&m, [0.0; 3]);
        let corner = self.p_core.iter().map(|p| p.s).fold(0.0f32, f32::max);
        let machines = self.place_machines(&m);
        let orbit = self.place_drones(&m);
        // What is not drawn does not move on screen.
        let out = self.outside() > 0.0;
        self.spin_px = (CORE * std::f32::consts::SQRT_2 * corner).max(if out { machines } else { 0.0 });
        self.orbit_px = if self.still || !out { 0.0 } else { orbit };
        let p = &self.p_node;
        let z = |i: u32| p.get(i as usize).map_or(0.0, |q| q.z);
        self.order.sort_unstable_by(|a, b| z(*a).total_cmp(&z(*b)));
    }

    /// Projects the lessons and the interior's points at the camera, its matrix and focal length
    /// worked out once: of the points, only those what is not fine detail uses (`Interior::coarse`)
    /// while the core is under `FINE_PX` on screen, since nothing drawn then uses the others.
    /// O(lessons + points).
    fn place_lessons(&mut self) {
        let (m, focal) = (self.cam.matrix(), self.cam.focal());
        self.fine = self.p_center.s * CORE >= FINE_PX;
        for (p, seat) in self.p_lesson.iter_mut().zip(&self.vol.inner.seats) {
            *p = self.cam.project_at(&m, focal, seat.at);
        }
        let inner = &self.vol.inner;
        let points = if self.fine { &inner.points[..] } else { inner.points.get(..inner.coarse).unwrap_or_default() };
        for (p, w) in self.p_pts.iter_mut().zip(points) {
            *p = self.cam.project_at(&m, focal, *w);
        }
    }

    /// Projects the machines; returns the pixels the farthest from the vertical axis, with its drones,
    /// moves per radian of yaw.
    fn place_machines(&mut self, m: &[f32; 9]) -> f32 {
        let mut spin = 0.0f32;
        for (p, w) in self.p_node.iter_mut().zip(&self.node_pos) {
            *p = self.cam.project(m, *w);
            // A point turns about the vertical axis at its distance from it; its drones a ring further.
            spin = spin.max((w[0].hypot(w[2]) + Self::ring(0) * 1.5) * p.s);
        }
        spin
    }

    /// Projects the drones; returns the pixels the fastest moves per second on its ring.
    fn place_drones(&mut self, m: &[f32; 9]) -> f32 {
        let mut orbit = 0.0f32;
        for i in 0..self.drone_node.len() {
            let w = self.drone_world(i);
            if let Some(p) = self.p_drone.get_mut(i) {
                *p = self.cam.project(m, w);
                let speed = self.drone_state.get(i).map_or(0.0, |s| s.speed());
                let count = self.drone_slot.get(i).map_or(0, |s| s.1);
                orbit = orbit.max(speed * Self::ring(count) * p.s);
            }
        }
        orbit
    }

    /// Orders what of the interior this frame shows as it is drawn (`Sight::key`): gathers each
    /// primitive its gate shows (a face looking at the camera, a shell panel kept for its face's side)
    /// with its depth and bucket, leaving fine detail out where the core is under `FINE_PX` on screen,
    /// then counts them into `BUCKETS`. O(primitives + `BUCKETS`), no comparisons, no allocation (`keys`
    /// and `drawn` hold every primitive, the counts live on the stack).
    fn sort_prims(&mut self) {
        let rot = self.cam.matrix();
        self.keys.clear();
        gather(&self.vol.inner, &self.p_pts, &Sight::new([rot[6], rot[7], rot[8]], self.fine), &mut self.keys);
        count_sort(&self.keys, &mut self.drawn);
    }

    /// The clock the core's lights blink by: it steps `BLINK` times a second.
    #[must_use]
    pub fn blink(&self) -> f32 {
        ((self.t * BLINK).floor() / BLINK) as f32
    }

    /// What this frame shows; keep it when the frame is drawn, for `needs_draw`.
    #[must_use]
    pub const fn shown(&self) -> Shown {
        Shown { drawn: true, t: self.t, cam: self.cam, hovered: self.hovered }
    }

    /// Whether a frame drawn now would look different from `last` (a `shown` of the last drawn frame;
    /// `Shown::default()` before any): an effect that is drawn runs, the core's glow decays, a lesson's light
    /// fades, another lesson is hovered, the camera's pitch, zoom, size or lens changed (a dive under
    /// way), the yaw (auto-rotation or a drag) moved the outermost drawn point a pixel or more, a
    /// drone's orbit moved it a pixel or more, or the empty core's lights stepped. Call it after `advance`.
    /// Resizes and new data the view knows of itself. O(1).
    #[must_use]
    pub fn needs_draw(&self, last: &Shown) -> bool {
        self.changed(last) || self.moved(last)
    }

    /// Whether anything but the motion `moved` weighs changed since `last`.
    fn changed(&self, last: &Shown) -> bool {
        !last.drawn || self.fx_shown || self.flash > 0.0 || self.vol.busy() || self.hovered != last.hovered || !self.cam.same_but_yaw(&last.cam)
    }

    /// Whether the yaw or an orbit moved something a pixel since `last`, or the empty core's lights
    /// (drawn while the outside is) stepped.
    fn moved(&self, last: &Shown) -> bool {
        let dt = (self.t - last.t).max(0.0) as f32;
        let blinks = !self.still && self.outside() > 0.0 && self.vol.is_empty();
        (self.cam.yaw - last.cam.yaw).abs() * self.spin_px >= PX
            || self.orbit_px * dt >= PX
            || (blinks && (self.t * BLINK).floor().to_bits() != (last.t * BLINK).floor().to_bits())
    }

    /// An effect starting now along `path` (its first `PATH` anchors).
    fn make(&self, kind: FxKind, tone: Tone, path: &[Anchor], dur: f32) -> Fx {
        let mut p = [Anchor::Core; PATH];
        let len = path.len().min(PATH);
        p[..len].copy_from_slice(&path[..len]);
        Fx { kind, tone, path: p, len: len as u8, t0: self.t, dur, reach: 1.0, n: 1 }
    }

    fn push(&mut self, kind: FxKind, tone: Tone, path: &[Anchor], dur: f32) {
        let f = self.make(kind, tone, path, dur);
        self.add(f);
    }

    /// Adds an effect at the end of its batch, so `fx` stays in batch order without a sort.
    fn add(&mut self, f: Fx) {
        let at = self.fx.partition_point(|g| g.batch() <= f.batch());
        self.fx.insert(at, f);
    }

    /// Where a session is: its drone, else its machine.
    fn at(&self, session: Option<u32>, node: Option<u32>) -> Option<Anchor> {
        if let Some(d) = session.and_then(|s| self.by_session.get(&s)) {
            return Some(Anchor::Drone(*d));
        }
        node.and_then(|n| self.by_key.get(&n)).map(|n| Anchor::Node(*n))
    }

    fn machine(&self, a: Anchor) -> Option<u32> {
        match a {
            Anchor::Core | Anchor::Lesson(_) | Anchor::Awaited(_) => None,
            Anchor::Node(n) => Some(n),
            Anchor::Drone(d) => self.drone_node.get(d as usize).copied(),
        }
    }

    fn linked(&self, a: u32, b: u32) -> bool {
        self.links.iter().any(|(x, y, _)| (*x == a && *y == b) || (*x == b && *y == a))
    }

    /// The way from one anchor to another: up to the sender's machine, across this node's link when
    /// there is one (else through the core), down to the recipient.
    fn route(&self, from: Anchor, to: Anchor) -> ([Anchor; PATH], usize) {
        let (fm, tm) = (self.machine(from), self.machine(to));
        let mut path = [Anchor::Core; PATH];
        let mut n = 0;
        let mut add = |a: Anchor| {
            if n < PATH && (n == 0 || path[n - 1] != a) {
                path[n] = a;
                n += 1;
            }
        };
        add(from);
        if let Some(f) = fm {
            add(Anchor::Node(f));
        }
        match (fm, tm) {
            (Some(a), Some(b)) if a == b || self.linked(a, b) => {}
            _ => add(Anchor::Core),
        }
        if let Some(t) = tm {
            add(Anchor::Node(t));
        }
        add(to);
        (path, n)
    }

    /// Plays the event `ev` of the model (a hand-off as one that moves on; `play_handed` says how it went).
    #[cfg(test)]
    pub fn play(&mut self, ev: Play) {
        self.play_handed(ev, Handed::Moves);
    }

    /// Plays the event `ev`: a packet for a message, a flash where one was shown, an assimilation for a
    /// lesson, a ring out of or into the core for an absorb or a release, a drone glyph for a hand-off
    /// (`how` it went), a stream of dots or a flickering link for a mesh sync. What names no machine or
    /// drone the scene has plays where it can, or not at all.
    pub fn play_handed(&mut self, ev: Play, how: Handed) {
        let lesson = ev.lesson.and_then(|l| self.by_lesson.get(&l)).copied();
        let written = matches!(ev.kind, EvKind::Lesson | EvKind::LessonUpdate);
        // A lesson written lights when its packet arrives (`lesson`); one absorbed or released at once.
        if let Some(i) = lesson.filter(|_| !written) {
            self.vol.light(i);
        }
        match ev.kind {
            EvKind::Tell | EvKind::Ask | EvKind::Pause | EvKind::Resume | EvKind::Veto => self.message(ev),
            EvKind::Seen => {
                if let Some(a) = self.at(ev.from_s, Some(ev.from_n)) {
                    self.push(FxKind::Flash, Tone::Cyan, &[a], 0.9);
                }
            }
            EvKind::Lesson | EvKind::LessonUpdate => {
                let to = lesson.map(Anchor::Lesson).or_else(|| ev.lesson.map(Anchor::Awaited)).unwrap_or(Anchor::Core);
                self.lesson(ev.from_n, ev.kind == EvKind::LessonUpdate, to);
            }
            EvKind::Absorb => self.push(FxKind::Burst, Tone::Bright, &[Anchor::Core], 1.6),
            EvKind::Release => {
                self.push(FxKind::Collapse, Tone::Green, &[Anchor::Core], 1.4);
                self.lift(1.4, 3, Anchor::Core);
            }
            EvKind::Handoff => self.handoff(ev, how),
            EvKind::Sync | EvKind::SyncFailed => self.mesh(ev),
        }
    }

    /// Lifts the core's glow by `tenths` in `delay` seconds, and then lights the lesson `at` names.
    fn lift(&mut self, delay: f32, tenths: u8, at: Anchor) {
        let f = Fx { t0: self.t + f64::from(delay), n: tenths, ..self.make(FxKind::Lift, Tone::Bright, &[at], 0.0) };
        self.add(f);
    }

    /// A message: a packet from its sender to its recipient, a ring for a broadcast.
    fn message(&mut self, ev: Play) {
        let Some(from) = self.at(ev.from_s, Some(ev.from_n)) else { return };
        let tone = match ev.kind {
            EvKind::Ask => Tone::Amber,
            EvKind::Pause | EvKind::Veto => Tone::Red,
            EvKind::Resume => Tone::Bright,
            _ => Tone::Green,
        };
        let fm = self.machine(from);
        match ev.to {
            To::All => {
                self.push(FxKind::Ring, tone, &[from], 1.4);
                for n in 0..ix(self.node_pos.len()) {
                    if Some(n) != fm {
                        let path = [from, Anchor::Core, Anchor::Node(n)];
                        self.push(FxKind::Packet, tone, if fm.is_some() { &path } else { &path[1..] }, 2.2);
                    }
                }
                self.flash = (self.flash + 0.35).min(1.0);
            }
            // To whoever reads the inbox, on its machine.
            To::Engineer => self.push(FxKind::Ring, tone, &[from], 1.2),
            To::Local | To::Session => {
                let Some(to) = self.at(ev.to_s, ev.to_n) else {
                    self.push(FxKind::Ring, tone, &[from], 1.2);
                    return;
                };
                let (path, n) = self.route(from, to);
                let legs = n.saturating_sub(1).max(1) as f32;
                self.push(FxKind::Packet, tone, &path[..n], 0.7 + 0.55 * legs);
            }
        }
    }

    /// A lesson written on machine `key`: an assimilation there, a bright packet to the lesson's point
    /// in the volume (`to`: the lesson, one the volume waits for, or the core's centre for an event
    /// that names none), and when it arrives the core flares and the lesson lights. A new version is
    /// the smaller.
    fn lesson(&mut self, key: u32, update: bool, to: Anchor) {
        let (rings, tenths) = if update { (1, 3) } else { (2, 6) };
        // A machine the scene does not have: the core assimilates it alone.
        let origin = self.by_key.get(&key).map(|n| Anchor::Node(*n));
        let shock = Fx { n: rings, ..self.make(FxKind::Shock, Tone::Bright, &[origin.unwrap_or(Anchor::Core)], 1.3) };
        self.add(shock);
        let travel = 1.1;
        if let Some(o) = origin {
            self.push(FxKind::Packet, Tone::Bright, &[o, to], travel);
        }
        self.lift(if origin.is_some() { travel } else { 0.0 }, tenths, to);
    }

    /// A hand-off: a drone glyph from the session that acted to the machine asked or taking (through the
    /// core when this node has no link to it, into the core when the machine is not known). Declined
    /// and aborted ones turn back early.
    fn handoff(&mut self, ev: Play, how: Handed) {
        let Some(from) = self.at(ev.from_s, Some(ev.from_n)) else { return };
        let to = ev.to_n.and_then(|n| self.by_key.get(&n)).map_or(Anchor::Core, |n| Anchor::Node(*n));
        let (mut path, mut n) = self.route(from, to);
        if n < 2 {
            // The same machine: out to the core and back.
            path[1] = Anchor::Core;
            path[2] = from;
            n = 3;
        }
        let (tone, reach) = how.look();
        let legs = (n - 1) as f32;
        let dur = 0.9 + 0.6 * legs * reach.max(0.5);
        let f = Fx { reach, ..self.make(FxKind::Glyph, tone, &path[..n], dur) };
        self.add(f);
        if how == Handed::Done {
            let flash = Fx { t0: self.t + f64::from(dur), ..self.make(FxKind::Flash, Tone::Green, &[path[n - 1]], 0.9) };
            self.add(flash);
        }
    }

    /// A mesh session of this node with a peer: dots along the link, more the more lines it moved, or
    /// the link flickering red when it failed. With one end unknown, a ring at the other.
    fn mesh(&mut self, ev: Play) {
        let failed = ev.kind == EvKind::SyncFailed;
        let tone = if failed { Tone::Red } else { Tone::Cyan };
        let me = self.by_key.get(&ev.from_n).copied();
        let peer = ev.to_n.and_then(|k| self.by_key.get(&k)).copied();
        match (me, peer) {
            (Some(a), Some(b)) if a != b => {
                let path = [Anchor::Node(a), Anchor::Node(b)];
                if failed {
                    self.push(FxKind::Flicker, tone, &path, 1.5);
                } else {
                    let k = dots(ev.n);
                    let f = Fx { n: k, ..self.make(FxKind::Stream, tone, &path, 1.2 + 0.08 * f32::from(k)) };
                    self.add(f);
                }
            }
            (Some(a), _) | (None, Some(a)) => self.push(FxKind::Ring, tone, &[Anchor::Node(a)], 1.2),
            (None, None) => {}
        }
    }

    /// What is under the screen point. Past halfway into a dive, only lessons: the one whose light is
    /// drawn there on top, else the nearest on screen of the lights that look at the camera within their
    /// drawn radius and 4 pixels (8 at least) and that no plating covers there (`lesson_at`), else nothing (the space between them is not the collective's,
    /// whose machines and drones the dive hides). Out of a dive, the machine or drone nearest within its
    /// drawn size, else the core (`in_core`). O(the interior's primitives drawn and their corners +
    /// machines + drones): it runs on pointer events, not on frames.
    #[must_use]
    pub fn hit(&self, x: f32, y: f32) -> Option<Anchor> {
        if self.dive > 0.5 {
            return self.lesson_at(x, y);
        }
        let near = |p: &P2, r: f32| {
            let d = (p.x - x).hypot(p.y - y);
            (d <= r).then_some(d)
        };
        let drone = self.p_drone.iter().enumerate().filter_map(|(i, p)| near(p, (p.s * 0.07).max(7.0)).map(|d| (d, Anchor::Drone(ix(i)))));
        let node = self.p_node.iter().enumerate().filter_map(|(i, p)| near(p, (p.s * 0.2).max(12.0)).map(|d| (d, Anchor::Node(ix(i)))));
        let core = || self.in_core(x, y).then_some(Anchor::Core);
        drone.chain(node).min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, a)| a).or_else(core)
    }

    /// Whether the screen point lies within the core's projected half-diagonal. O(1).
    #[must_use]
    pub fn in_core(&self, x: f32, y: f32) -> bool {
        (self.p_center.x - x).hypot(self.p_center.y - y) <= CORE * std::f32::consts::SQRT_2 * self.p_center.s
    }

    /// Whether a double-click at the screen point goes into the core or back out: out of a dive on the
    /// core, not on a machine or drone in front of it; in a dive anywhere on the core but on a lesson,
    /// which a click opens (its second click lands on the drawer). O(`hit`).
    #[must_use]
    pub fn toggles_dive(&self, x: f32, y: f32) -> bool {
        match self.hit(x, y) {
            Some(Anchor::Core) => true,
            Some(Anchor::Lesson(_) | Anchor::Awaited(_) | Anchor::Node(_) | Anchor::Drone(_)) => false,
            None => self.dive > 0.5 && self.in_core(x, y),
        }
    }

    /// The lesson whose light is under the screen point, as the last frame drew it, walking what it drew
    /// from near to far down to the first opaque plating that covers the point: the first lesson whose
    /// light's face covers the point (it is drawn on top there); else, of the lessons whose light lies
    /// over that plating (drawn after it) within their drawn radius and 4 pixels (8 at least) and looks
    /// at the camera (or every way), the nearest on screen, the nearer in depth of two as near. So a
    /// light is hit where it shows, plating in front of a light hides it from the pointer as it does
    /// from the eye, and a small light is hit near it too. O(primitives drawn and their corners).
    fn lesson_at(&self, x: f32, y: f32) -> Option<Anchor> {
        let rot = self.cam.matrix();
        let view = [rot[6], rot[7], rot[8]];
        let inner = &self.vol.inner;
        let mut best: Option<(f32, f32, u32)> = None;
        for d in self.drawn.iter().rev() {
            let Prim::Face(i) = d.prim else { continue };
            let Some(f) = inner.faces.get(i as usize) else { continue };
            let over = || covers(inner, &self.p_pts, f, (x, y));
            match f.look {
                Look::Light { lesson, .. } if over() => return Some(Anchor::Lesson(lesson)),
                Look::Light { lesson, .. } => {
                    if let Some((dist, z)) = self.reaches(lesson, view, (x, y))
                        && best.is_none_or(|(bd, bz, _)| dist < bd || (dist == bd && z > bz))
                    {
                        best = Some((dist, z, lesson));
                    }
                }
                Look::Plate(_) if over() => break,
                Look::Plate(_) | Look::Grating(_) => {}
            }
        }
        best.map(|(_, _, i)| Anchor::Lesson(i))
    }

    /// Lesson `i`'s distance on screen from the point and its depth, if its light looks at the camera
    /// (`view`) and lies within reach of the point: its drawn radius and 4 pixels, 8 at least. O(1).
    fn reaches(&self, i: u32, view: [f32; 3], (x, y): (f32, f32)) -> Option<(f32, f32)> {
        let (p, seat) = (self.p_lesson.get(i as usize)?, self.vol.inner.seats.get(i as usize)?);
        let reach = (lesson_px(seat.r, p.s, self.dive) + 4.0).max(8.0);
        let dist = (p.x - x).hypot(p.y - y);
        (seat.looks(view) && dist <= reach).then_some((dist, p.z))
    }
}

/// Whether face `f` (its points projected, `p`) covers the screen point: inside its outline and not
/// inside its hole. O(corners).
fn covers(inner: &Interior, p: &[P2], f: &Face, at: (f32, f32)) -> bool {
    let first = f.first as usize;
    let (n, hole) = (usize::from(f.n), usize::from(f.hole));
    let Some(corners) = inner.corners.get(first..first + n + hole) else { return false };
    let (outline, inside) = corners.split_at(n);
    contains(p, outline, at) && !(hole > 0 && contains(p, inside, at))
}

/// Whether the polygon of points `ix` (as projected, `p`) contains the screen point: an odd number of
/// its edges cross the ray from it to the right. O(points).
fn contains(p: &[P2], ix: &[u32], (x, y): (f32, f32)) -> bool {
    let at = |i: u32| p.get(i as usize).copied().unwrap_or_default();
    let mut odd = false;
    for (k, &i) in ix.iter().enumerate() {
        let (a, b) = (at(i), at(ix[(k + 1) % ix.len()]));
        if (a.y > y) != (b.y > y) && x < a.x + (y - a.y) / (b.y - a.y) * (b.x - a.x) {
            odd = !odd;
        }
    }
    odd
}

/// A primitive of the interior (`vol.inner`), by its index in its list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Prim {
    Face(u32),
    Line(u32),
    Glow(u32),
    Label(u32),
}

/// A primitive a frame shows, its depth (larger is nearer) and the bucket it is drawn in (`Sight::key`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drawn {
    pub prim: Prim,
    pub z: f32,
    pub key: u16,
}

impl Default for Drawn {
    fn default() -> Self {
        Self { prim: Prim::Face(0), z: 0.0, key: 0 }
    }
}

/// What a frame shows of the interior: the camera's depth axis, which shell faces look at the camera,
/// each shell face's place among the six far to near, and whether fine detail shows.
struct Sight {
    view: [f32; 3],
    facing: [bool; 6],
    rank: [u8; 6],
    fine: bool,
}

impl Sight {
    /// Along the camera's depth axis `view`: a face looks at the camera where its normal points along
    /// it, and ranks by how far it does (ties by number), counted rather than sorted. O(1).
    fn new(view: [f32; 3], fine: bool) -> Self {
        let depth = |f: usize| {
            let (axis, side) = face_axis(f);
            view[axis] * side
        };
        let facing = std::array::from_fn(|f| depth(f) > 0.0);
        let rank = std::array::from_fn(|f| (0..6).filter(|&g| depth(g) < depth(f) || (depth(g) == depth(f) && g < f)).count() as u8);
        Self { view, facing, rank, fine }
    }

    /// The bucket a primitive under `gate` at depth `z` is drawn in (`count_sort`). The cube is convex
    /// and its interior lies inside the shell, so a shell face that looks away lies behind all of the
    /// interior and one that looks at the camera in front of it; a face's panels lie side by side and do
    /// not overlap. So the shell is not sorted by depth: the faces that look away come first, far to
    /// near, each in its groups (`SHELL_GROUPS`, the one `group` picks, told whether the face looks
    /// away); then the rest by depth (`depth_bucket`); then the faces that look at the camera as the
    /// first. A face's panels of one plating level are then one fill, a face's lines of one kind
    /// mostly one stroke. O(1).
    fn key(&self, gate: Gate, z: f32, group: impl FnOnce(bool) -> usize) -> u16 {
        let shell = 6 * SHELL_GROUPS;
        let k = match gate {
            Gate::Shell { face, .. } => {
                let f = usize::from(face) % 6;
                let slot = usize::from(self.rank[f]) * SHELL_GROUPS + group(!self.facing[f]).min(SHELL_GROUPS - 1);
                if self.facing[f] { shell + DEPTHS + slot } else { slot }
            }
            Gate::Always | Gate::Cull => shell + depth_bucket(z),
        };
        u16::try_from(k).unwrap_or(u16::MAX)
    }
}

/// A shell line's group (`SHELL_GROUPS`), on a face that looks away (`away`) or not: under the
/// panels a rib on the face that looks away, seen from inside (`Gate::Shell` `surface`), else over
/// them; its dark backing before its green. O(1).
fn line_group(l: &Line, away: bool) -> usize {
    let under = away && matches!(l.gate, Gate::Shell { surface: true, .. });
    let stroke = if away { l.alt } else { l.style };
    let green = usize::from(stroke.look().0 != Tone::Hull);
    if under { green } else { PLATES + 2 + green }
}

/// The mean depth of points `ix` as projected (`p`).
fn mean_z(p: &[P2], ix: &[u32]) -> f32 {
    let sum: f32 = ix.iter().map(|&i| p.get(i as usize).map_or(0.0, |q| q.z)).sum();
    sum / ix.len().max(1) as f32
}

/// Each of the interior's primitives that `sight` shows, with its depth and its bucket (`Sight::key`):
/// a face's depth the mean of its outline's, a line's its middle's, either at the point it is pinned to
/// instead and its pin's bias nearer (interior.rs `Pin`); a glow's its point's and bias, a label's its
/// point's. Pushes within `out`'s capacity. O(primitives + corners).
fn gather(inner: &Interior, p: &[P2], sight: &Sight, out: &mut Vec<Drawn>) {
    gather_faces(inner, p, sight, out);
    let z = |i: u32| p.get(i as usize).map_or(0.0, |q| q.z);
    for (i, l) in inner.lines.iter().enumerate() {
        if (!l.fine || sight.fine) && l.gate.shows([0.0; 3], sight.view, &sight.facing) {
            let own = l.pin.at.map_or_else(|| 0.5 * (z(l.a) + z(l.b)), z) + l.pin.bias;
            out.push(Drawn { prim: Prim::Line(ix(i)), z: own, key: sight.key(l.gate, own, |away| line_group(l, away)) });
        }
    }
    for (i, g) in inner.glows.iter().enumerate() {
        if g.gate.shows([0.0; 3], sight.view, &sight.facing) {
            let own = z(g.at) + g.bias;
            out.push(Drawn { prim: Prim::Glow(ix(i)), z: own, key: sight.key(g.gate, own, |_| PLATES + 4) });
        }
    }
    for (i, l) in inner.labels.iter().enumerate() {
        out.push(Drawn { prim: Prim::Label(ix(i)), z: z(l.at), key: sight.key(Gate::Always, z(l.at), |_| 0) });
    }
}

/// The faces of `gather`.
fn gather_faces(inner: &Interior, p: &[P2], sight: &Sight, out: &mut Vec<Drawn>) {
    for (i, f) in inner.faces.iter().enumerate() {
        if (f.fine && !sight.fine) || !f.gate.shows(f.normal, sight.view, &sight.facing) {
            continue;
        }
        let first = f.first as usize;
        let outline = inner.corners.get(first..first + usize::from(f.n)).unwrap_or_default();
        let own = f.pin.at.map_or_else(|| mean_z(p, outline), |at| p.get(at as usize).map_or(0.0, |q| q.z)) + f.pin.bias;
        let level = match f.look {
            Look::Plate(shade) | Look::Grating(shade) => usize::from(plate_at(shade, own)),
            Look::Light { .. } => 0,
        };
        out.push(Drawn { prim: Prim::Face(ix(i)), z: own, key: sight.key(f.gate, own, |_| 2 + level) });
    }
}

/// `keys` in the order they are drawn into `out`, by counting them into their `BUCKETS` (`Drawn::key`):
/// keys of one bucket keep their order. O(keys + `BUCKETS`); `out` is resized within its capacity.
fn count_sort(keys: &[Drawn], out: &mut Vec<Drawn>) {
    let mut at = [0u32; BUCKETS + 1];
    let bucket = |k: &Drawn| usize::from(k.key).min(BUCKETS - 1);
    for k in keys {
        at[bucket(k) + 1] += 1;
    }
    for b in 1..=BUCKETS {
        at[b] += at[b - 1];
    }
    out.clear();
    out.resize(keys.len(), Drawn::default());
    for k in keys {
        let b = bucket(k);
        if let Some(slot) = out.get_mut(at[b] as usize) {
            *slot = *k;
        }
        at[b] += 1;
    }
}

/// Moves each effect's anchors by `to`, and ends an effect when `to` says one of its anchors is gone.
/// Keeps the effects' order, and so their batches.
fn follow(fx: &mut Vec<Fx>, to: impl Fn(Anchor) -> Option<Anchor>) {
    fx.retain_mut(|f| {
        f.path[..usize::from(f.len).min(PATH)].iter_mut().all(|a| {
            let Some(n) = to(*a) else { return false };
            *a = n;
            true
        })
    });
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A model of `nodes` machines (the first is this node, all linked to it) and `drones` drones
    /// spread over them in turn, their states cycling; parsed as `/api/mesh` sends it.
    pub fn model(nodes: usize, drones: usize) -> Model {
        model_with(nodes, drones, &[])
    }

    /// `model`, with the events `evs` (JSON objects as `/api/mesh` sends them).
    pub fn model_with(nodes: usize, drones: usize, evs: &[String]) -> Model {
        let mut strs: Vec<String> = (0..nodes).map(|i| format!("\"key-{i}\"")).collect();
        strs.extend((0..drones).map(|i| format!("\"sess-{i}\"")));
        let ns: Vec<String> = (0..nodes).map(|i| format!(r#"{{"key":{i},"host":"host-{i}","me":{},"syncs":true}}"#, i == 0)).collect();
        let ls: Vec<String> = (1..nodes).map(|i| format!(r#"{{"a":0,"b":{i},"dial":true,"ms":900,"ok":{}}}"#, i % 5 != 0)).collect();
        let states = ["working", "waiting", "idle", "ended"];
        let ds: Vec<String> = (0..drones)
            .map(|i| format!(r#"{{"session":{},"node":{},"name":"d{i}","number":{i},"state":"{}"}}"#, nodes + i, i % nodes.max(1), states[i % 4]))
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
        assert!(m.merge(&text).is_some(), "the test model parses");
        m
    }

    /// A scene of `nodes` machines with `n` lessons (`L0` on) settled in its core, and its model, whose
    /// events name every lesson (each written on key-1), so the scene knows them by the model's ids.
    pub fn with_lessons(nodes: usize, n: usize) -> (Model, Scene) {
        let evs: Vec<String> = (0..n).map(|i| format!(r#"{{"id":"e{i}","ms":{i},"kind":"lesson","from_n":1,"lesson":"L{i}","text":"why"}}"#)).collect();
        let m = model_with(nodes, 0, &evs);
        let mut s = Scene::default();
        s.sync(&m);
        let ids = crate::paint::record::lesson_ids(n);
        s.lessons(&crate::paint::record::items(&ids), &m);
        s.frame(1.0);
        (m, s)
    }

    /// Frames at 60 Hz from the scene's clock for `secs` seconds; returns the clock at the end.
    fn run(s: &mut Scene, secs: f64) -> f64 {
        let t0 = s.t;
        for i in 1..=(secs * 60.0).round() as u32 {
            s.frame(t0 + f64::from(i) / 60.0);
        }
        s.t
    }

    /// A lesson event of `kind` written on machine `key`, naming the model's string `lesson`.
    fn lesson_ev(m: &Model, kind: EvKind, key: &str, lesson: &str) -> Play {
        Play { lesson: Some(id(m, lesson)), ..ev(kind, id(m, key), None, To::Session, None, None, 0) }
    }

    /// The model's id of a string it holds (keys, sessions).
    pub fn id(m: &Model, s: &str) -> u32 {
        (0..m.strs.len()).map(ix).find(|i| m.str(*i) == s).unwrap_or(u32::MAX)
    }

    pub const KINDS: [EvKind; 13] = [
        EvKind::Tell,
        EvKind::Ask,
        EvKind::Pause,
        EvKind::Resume,
        EvKind::Veto,
        EvKind::Seen,
        EvKind::Lesson,
        EvKind::LessonUpdate,
        EvKind::Absorb,
        EvKind::Release,
        EvKind::Handoff,
        EvKind::Sync,
        EvKind::SyncFailed,
    ];

    pub const fn ev(kind: EvKind, from_n: u32, from_s: Option<u32>, to: To, to_n: Option<u32>, to_s: Option<u32>, n: u32) -> Play {
        Play { kind, from_n, from_s, to, to_n, to_s, n, lesson: None }
    }

    fn scene(nodes: usize, drones: usize) -> (Model, Scene) {
        let m = model(nodes, drones);
        let mut s = Scene::default();
        s.sync(&m);
        s.frame(0.0);
        (m, s)
    }

    fn valid(s: &Scene) -> bool {
        s.fx.iter().all(|f| {
            f.path[..usize::from(f.len)].iter().all(|a| match a {
                Anchor::Core | Anchor::Awaited(_) => true,
                Anchor::Node(i) => (*i as usize) < s.node_pos.len(),
                Anchor::Drone(i) => (*i as usize) < s.drone_node.len(),
                Anchor::Lesson(i) => (*i as usize) < s.vol.len(),
            })
        })
    }

    #[test]
    fn a_machine_keeps_its_place_by_its_key() {
        let a = layout(&["me", "k1", "k2"], 0);
        let b = layout(&["me", "k1", "k2"], 0);
        assert_eq!(a, b);
        // In front: +z, at the orbit.
        assert!(a[0][2] > ORBIT * 0.9);
        for p in &a {
            let r = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
            assert!((r - ORBIT).abs() < 1e-3, "on the sphere: {r}");
        }
    }

    #[test]
    fn machines_are_pushed_apart() {
        let keys: Vec<String> = (0..12).map(|i| format!("key-{i}")).collect();
        let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        let p = layout(&refs, 0);
        for i in 0..p.len() {
            for j in i + 1..p.len() {
                let d = ((p[i][0] - p[j][0]).powi(2) + (p[i][1] - p[j][1]).powi(2) + (p[i][2] - p[j][2]).powi(2)).sqrt();
                assert!(d > 0.6, "{i} and {j} {d}");
            }
        }
    }

    #[test]
    fn the_origin_projects_to_the_middle_below_the_counters() {
        let c = Camera::default();
        let p = c.project(&c.matrix(), [0.0; 3]);
        assert!((p.x - c.w / 2.0).abs() < 1e-3 && (p.y - c.h * 0.54).abs() < 1e-3);
        // Nearer is larger.
        let m = Camera { pitch: 0.0, ..c }.matrix();
        assert!(c.project(&m, [0.0, 0.0, 2.0]).s > c.project(&m, [0.0, 0.0, -2.0]).s);
    }

    #[test]
    fn faces_walk_their_corners_as_on_face_does() {
        for (f, corners) in FACES.iter().enumerate() {
            for (k, (a, b)) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].into_iter().enumerate() {
                assert_eq!(on_face(f, a, b), CORNERS[corners[k]], "face {f} corner {k}");
            }
        }
    }

    #[test]
    fn an_effect_ends_after_its_length_and_waits_for_its_start() {
        let f = Fx { kind: FxKind::Flash, tone: Tone::Cyan, path: [Anchor::Core; PATH], len: 1, t0: 1.0, dur: 2.0, reach: 1.0, n: 1 };
        assert!((f.progress(2.0) - 0.5).abs() < 1e-6);
        assert!(f.progress(3.5) > 1.0);
        assert!(f.progress(0.5) < 0.0);
    }

    #[test]
    fn every_kind_plays_without_panic_whoever_it_names() {
        let (m, mut s) = scene(4, 9);
        let (k1, s0, s3) = (id(&m, "key-1"), id(&m, "sess-0"), id(&m, "sess-3"));
        let senders = [(id(&m, "key-0"), Some(s0)), (k1, None), (k1, Some(u32::MAX)), (u32::MAX, None), (u32::MAX, Some(s3))];
        let targets =
            [(Some(k1), Some(s3)), (Some(k1), None), (None, Some(s0)), (Some(u32::MAX), Some(u32::MAX)), (None, None), (Some(id(&m, "key-0")), Some(s0))];
        let hows = [Handed::Moves, Handed::Done, Handed::Back, Handed::Declined, Handed::Aborted];
        for kind in KINDS {
            for (fn_, fs) in senders {
                for (tn, ts) in targets {
                    for to in [To::All, To::Local, To::Engineer, To::Session] {
                        for how in hows {
                            s.play_handed(ev(kind, fn_, fs, to, tn, ts, 7), how);
                        }
                    }
                }
            }
        }
        assert!(valid(&s));
        assert!(s.fx.iter().all(|f| f.len >= 1 && usize::from(f.len) <= PATH));
        // Every kind left something to see.
        for kind in [
            FxKind::Packet,
            FxKind::Ring,
            FxKind::Flash,
            FxKind::Shock,
            FxKind::Burst,
            FxKind::Collapse,
            FxKind::Glyph,
            FxKind::Stream,
            FxKind::Flicker,
            FxKind::Lift,
        ] {
            assert!(s.fx.iter().any(|f| f.kind == kind), "{kind:?}");
        }
        let mut c = crate::paint::record::Count::default();
        for i in 0..200 {
            s.frame(f64::from(i) * 0.05);
            crate::paint::paint(&s, &mut c);
        }
        // All of it is over after its time, waiting ones included.
        assert!(s.fx.is_empty(), "{:?}", s.fx.first());
    }

    #[test]
    fn a_lesson_flares_the_core_when_it_arrives() {
        let (m, mut s) = scene(3, 0);
        s.play(ev(EvKind::Lesson, id(&m, "key-2"), None, To::Session, None, None, 0));
        assert!(s.fx.iter().any(|f| f.kind == FxKind::Shock && f.n == 2 && f.path[0] == Anchor::Node(2)));
        assert!(s.fx.iter().any(|f| f.kind == FxKind::Packet && f.path[..2] == [Anchor::Node(2), Anchor::Core]));
        s.frame(0.5);
        assert!(s.flash.abs() < f32::EPSILON, "not yet: {}", s.flash);
        s.frame(1.2);
        assert!(s.flash > 0.4, "arrived: {}", s.flash);
        // An update is the smaller.
        s.flash = 0.0;
        s.play(ev(EvKind::LessonUpdate, id(&m, "key-1"), None, To::Session, None, None, 0));
        assert!(s.fx.iter().any(|f| f.kind == FxKind::Shock && f.n == 1));
        s.frame(2.4);
        assert!(s.flash > 0.1 && s.flash < 0.4, "{}", s.flash);
    }

    #[test]
    fn a_sync_streams_more_dots_the_more_it_moved_and_a_failure_flickers_its_link() {
        let (m, mut s) = scene(3, 0);
        let (me, peer) = (id(&m, "key-0"), id(&m, "key-2"));
        let k = |s: &mut Scene, n: u32| {
            s.fx.clear();
            s.play(ev(EvKind::Sync, me, None, To::Session, Some(peer), None, n));
            s.fx.iter().find(|f| f.kind == FxKind::Stream).map(|f| (f.n, f.path[..2] == [Anchor::Node(0), Anchor::Node(2)]))
        };
        assert_eq!(k(&mut s, 0), Some((1, true)));
        assert_eq!(k(&mut s, 1), Some((2, true)));
        assert_eq!(k(&mut s, 1000), Some((10, true)));
        assert_eq!(k(&mut s, u32::MAX), Some((33, true)));
        s.fx.clear();
        s.play(ev(EvKind::SyncFailed, me, None, To::Session, Some(peer), None, 0));
        let f = s.fx.first().copied();
        assert_eq!(f.map(|f| (f.kind, f.tone)), Some((FxKind::Flicker, Tone::Red)));
        assert!(f.is_some_and(|f| (f.dur - 1.5).abs() < 1e-6));
        // An unknown peer: a ring at this node.
        s.fx.clear();
        s.play(ev(EvKind::Sync, me, None, To::Session, Some(u32::MAX), None, 5));
        assert_eq!(s.fx.first().map(|f| (f.kind, f.path[0])), Some((FxKind::Ring, Anchor::Node(0))));
    }

    #[test]
    fn a_declined_hand_off_turns_back_early() {
        let (m, mut s) = scene(3, 3);
        let (from, to) = (id(&m, "sess-0"), id(&m, "key-1"));
        let play = |s: &mut Scene, how| {
            s.fx.clear();
            s.play_handed(ev(EvKind::Handoff, id(&m, "key-0"), Some(from), To::Session, Some(to), None, 0), how);
            s.fx.iter().find(|f| f.kind == FxKind::Glyph).copied()
        };
        let moves = play(&mut s, Handed::Moves);
        assert!(moves.is_some_and(|f| f.reach >= 1.0 && f.path[usize::from(f.len) - 1] == Anchor::Node(1)), "{moves:?}");
        let declined = play(&mut s, Handed::Declined);
        assert!(declined.is_some_and(|f| f.reach < 1.0 && f.tone == Tone::Amber));
        assert!(play(&mut s, Handed::Aborted).is_some_and(|f| f.tone == Tone::Red));
        // Done flashes where it lands, when it lands.
        let _ = play(&mut s, Handed::Done);
        assert!(s.fx.iter().any(|f| f.kind == FxKind::Flash && f.t0 > s.t));
    }

    #[test]
    fn effects_follow_their_drones_when_the_scene_shrinks() {
        let m = model(4, 8);
        let mut s = Scene::default();
        s.sync(&m);
        s.frame(0.0);
        // sess-7 is on key-3; sess-6 on key-2.
        let (k0, s7, s6) = (id(&m, "key-0"), id(&m, "sess-7"), id(&m, "sess-6"));
        s.play(ev(EvKind::Tell, k0, Some(id(&m, "sess-0")), To::Session, Some(id(&m, "key-3")), Some(s7), 0));
        s.play(ev(EvKind::Seen, id(&m, "key-2"), Some(s6), To::Session, None, None, 0));
        s.play(ev(EvKind::Sync, k0, None, To::Session, Some(id(&m, "key-3")), None, 3));
        assert_eq!(s.fx.len(), 3);
        // The next response drops key-3 and the drones before sess-6, so the indices shift.
        let mut m2 = model(4, 8);
        let (k3, s6, s0) = (id(&m2, "key-3"), id(&m2, "sess-6"), id(&m2, "sess-0"));
        m2.nodes.retain(|n| n.key != k3);
        m2.drones.retain(|d| d.session == s6 || d.session == s0);
        s.sync(&m2);
        assert!(valid(&s));
        // Only the flash survives, still on sess-6.
        assert_eq!(s.fx.len(), 1);
        let Some(Anchor::Drone(d)) = s.fx.first().map(|f| f.path[0]) else { panic!("{:?}", s.fx) };
        assert_eq!(s.drone_session[d as usize], s6);
        s.frame(0.1);
        // An empty model ends everything but what is on the core.
        s.play(ev(EvKind::Absorb, k0, None, To::Session, None, None, 0));
        s.sync(&Model::default());
        assert!(s.fx.iter().all(|f| f.kind == FxKind::Burst));
        s.frame(0.2);
        // Only the core is left to hit.
        assert_eq!(s.hit(s.p_center.x, s.p_center.y), Some(Anchor::Core));
        assert!(s.hit(5.0, 5.0).is_none());
    }

    #[test]
    fn a_click_hits_a_drone_or_machine_before_the_core() {
        let (_, s) = scene(3, 4);
        let (c, r) = (s.p_center, CORE * std::f32::consts::SQRT_2 * s.p_center.s);
        assert_eq!(s.hit(c.x, c.y), Some(Anchor::Core));
        assert_eq!(s.hit(c.x + r * 0.9, c.y), Some(Anchor::Core));
        assert_ne!(s.hit(c.x + r * 1.1, c.y), Some(Anchor::Core));
        for p in s.p_node.iter().chain(&s.p_drone) {
            assert!(matches!(s.hit(p.x, p.y), Some(Anchor::Node(_) | Anchor::Drone(_))), "{p:?}");
        }
    }

    #[test]
    fn an_idle_view_draws_only_when_it_visibly_moved() {
        let (_, mut s) = scene(5, 0);
        let mut t = 0.0;
        let mut last = Shown::default();
        assert!(s.needs_draw(&last));
        last = s.shown();
        // Nothing runs and no drone turns: the slow spin needs a few frames to move a pixel, and the
        // lights step four times a second.
        let mut drawn = 0;
        for _ in 0..240 {
            t += 1.0 / 60.0;
            s.frame(t);
            if s.needs_draw(&last) {
                drawn += 1;
                last = s.shown();
            }
        }
        assert!(drawn > 4 && drawn < 120, "{drawn} of 240");
        // Still: only a camera change draws.
        s.still = true;
        s.frame(t);
        last = s.shown();
        s.frame(t + 3.0);
        assert!(!s.needs_draw(&last));
        s.cam.pitch += 0.01;
        assert!(s.needs_draw(&last));
        // Any effect draws.
        s.cam.pitch -= 0.01;
        last = s.shown();
        s.play(ev(EvKind::Absorb, 0, None, To::Session, None, None, 0));
        s.frame(t + 3.0);
        assert!(s.needs_draw(&last));
    }

    /// Idle frames drawn per second of a 60 Hz clock over ten seconds.
    fn idle_rate(nodes: usize, drones: usize, w: f32, h: f32) -> f64 {
        let (_, mut s) = scene(nodes, drones);
        (s.cam.w, s.cam.h) = (w, h);
        let (mut last, mut drawn) = (Shown::default(), 0u32);
        for i in 1..=600 {
            s.frame(f64::from(i) / 60.0);
            if s.needs_draw(&last) {
                drawn += 1;
                last = s.shown();
            }
        }
        f64::from(drawn) / 10.0
    }

    #[test]
    fn an_idle_view_redraws_at_its_pixel_rate_not_the_displays() {
        for (nodes, w, h) in [(3, 800.0, 520.0), (3, 1180.0, 640.0), (8, 1600.0, 900.0)] {
            let rate = idle_rate(nodes, 0, w, h);
            // A pixel of spin (10 to 20 a second) and the lights' four steps share frames; half a pixel
            // drew 20 to 32.
            assert!(rate <= 22.0, "{nodes} machines at {w}x{h}: {rate} a second");
        }
    }

    #[test]
    fn working_drones_draw_at_their_pace() {
        let (_, mut s) = scene(2, 4);
        s.still = false;
        let last = s.shown();
        s.frame(0.001);
        assert!(!s.needs_draw(&Shown { t: s.t, cam: s.cam, ..last }), "no time passed, nothing moved");
        s.frame(0.5);
        assert!(s.needs_draw(&last));
    }
    #[test]
    fn a_lesson_event_ends_at_its_point_and_lights_it() {
        let (m, mut s) = with_lessons(3, 20);
        let i = s.vol.find("L7").expect("in the volume");
        s.play(lesson_ev(&m, EvKind::Lesson, "key-2", "L7"));
        let packet = s.fx.iter().find(|f| f.kind == FxKind::Packet).copied().expect("a packet");
        assert_eq!(packet.path[..usize::from(packet.len)], [Anchor::Node(2), Anchor::Lesson(i)]);
        assert!(s.fx.iter().any(|f| f.kind == FxKind::Shock && f.path[0] == Anchor::Node(2)), "assimilated where it was written");
        // It lights when the packet arrives, not before.
        s.frame(1.5);
        assert!(s.vol.lit[i as usize].abs() < f32::EPSILON, "not yet");
        let end = s.along(&packet, 1.0);
        assert!((end.x - s.p_lesson[i as usize].x).abs() < 1e-3 && (end.y - s.p_lesson[i as usize].y).abs() < 1e-3, "it ends at the lesson");
        s.frame(2.2);
        assert!(s.vol.lit[i as usize] > 0.9, "lit on arrival: {}", s.vol.lit[i as usize]);
        assert!(s.vol.lit.iter().enumerate().all(|(j, l)| j == i as usize || *l == 0.0), "only it");
        assert!(s.flash > 0.4, "the core still flares");
        // An update runs there too.
        s.fx.clear();
        s.play(lesson_ev(&m, EvKind::LessonUpdate, "key-1", "L3"));
        let to = s.vol.find("L3").map(Anchor::Lesson);
        assert!(s.fx.iter().any(|f| f.kind == FxKind::Packet && Some(f.path[1]) == to));
    }

    #[test]
    fn an_unknown_lesson_runs_to_the_cores_centre() {
        let (m, mut s) = with_lessons(3, 5);
        // A string the model holds that is no lesson of the volume, one it does not hold, and none.
        for lesson in [Some(id(&m, "key-0")), Some(u32::MAX), None] {
            s.fx.clear();
            s.play(Play { lesson, ..ev(EvKind::Lesson, id(&m, "key-2"), None, To::Session, None, None, 0) });
            let to = lesson.map_or(Anchor::Core, Anchor::Awaited);
            let packet = s.fx.iter().find(|f| f.kind == FxKind::Packet).copied().expect("a packet");
            assert_eq!(packet.path[..2], [Anchor::Node(2), to], "{lesson:?}");
            assert_eq!(s.along(&packet, 1.0), s.p_center, "{lesson:?}");
            assert!(s.fx.iter().filter(|f| f.kind == FxKind::Lift).all(|f| f.path[0] == to));
        }
        s.frame(5.0);
        assert!(s.vol.lit.iter().all(|l| *l == 0.0), "nothing lit");
        // An absorb lights its lesson at once.
        s.play(lesson_ev(&m, EvKind::Absorb, "key-0", "L2"));
        let i = s.vol.find("L2").expect("in the volume") as usize;
        assert!(s.vol.lit[i] > 0.9);
    }

    #[test]
    fn a_lesson_effect_follows_its_lesson_when_the_volume_rebuilds() {
        let (m, mut s) = with_lessons(3, 10);
        s.play(lesson_ev(&m, EvKind::Lesson, "key-1", "L4"));
        s.play(lesson_ev(&m, EvKind::Lesson, "key-2", "L6"));
        s.hovered = s.vol.find("L4");
        // The store loses L0 to L5 but L4: indices shift, L6 stays, L4 stays.
        let ids = crate::paint::record::lesson_ids(10);
        let all = crate::paint::record::items(&ids);
        let kept: Vec<Item> = all.iter().copied().filter(|it| !matches!(it.id, "L0" | "L1" | "L2" | "L3" | "L5")).collect();
        s.lessons(&kept, &m);
        assert!(valid(&s));
        let (l4, l6) = (s.vol.find("L4").expect("kept"), s.vol.find("L6").expect("kept"));
        assert_ne!(l6, 6, "the index moved");
        let ends: Vec<Anchor> = s.fx.iter().filter(|f| f.kind == FxKind::Packet).map(|f| f.path[1]).collect();
        assert_eq!(ends, [Anchor::Lesson(l4), Anchor::Lesson(l6)]);
        assert_eq!(s.hovered, Some(l4));
        // L6 gone: its packet and lift end, the rest play on, and sync keeps them.
        let without: Vec<Item> = kept.iter().copied().filter(|it| it.id != "L6").collect();
        s.lessons(&without, &m);
        let l4 = s.vol.find("L4").expect("kept");
        assert!(s.fx.iter().filter(|f| to_any_lesson(f)).all(|f| f.path[..usize::from(f.len)].contains(&Anchor::Lesson(l4))));
        assert_eq!(s.fx.iter().filter(|f| f.kind == FxKind::Packet).count(), 1);
        s.sync(&m);
        assert_eq!(s.fx.iter().filter(|f| f.kind == FxKind::Packet).count(), 1, "sync keeps an effect on a lesson");
        s.lessons(&[], &m);
        assert!(!s.fx.iter().any(to_any_lesson) && s.hovered.is_none());
    }

    #[test]
    fn a_lesson_that_comes_after_its_event_takes_the_packet_and_lights() {
        // The model knows L20 (an event named it), the volume does not hold it yet: the event's mesh
        // answer came before the lessons'.
        let (m, mut s) = with_lessons(3, 21);
        let ids = crate::paint::record::lesson_ids(21);
        s.lessons(&crate::paint::record::items(&ids[..20]), &m);
        assert_eq!(s.vol.find("L20"), None);
        s.play(lesson_ev(&m, EvKind::Lesson, "key-2", "L20"));
        let t = s.t;
        s.frame(t + 0.5);
        // Its lessons come while the packet is on its way: it runs on to the lesson, which lights.
        s.lessons(&crate::paint::record::items(&ids), &m);
        let i = s.vol.find("L20").expect("in the volume now");
        let packet = s.fx.iter().find(|f| f.kind == FxKind::Packet).copied().expect("a packet");
        assert_eq!(packet.path[..2], [Anchor::Node(2), Anchor::Lesson(i)]);
        s.frame(t + 1.2);
        assert!(s.vol.lit[i as usize] > 0.9, "lit on arrival: {}", s.vol.lit[i as usize]);
    }

    fn to_any_lesson(f: &Fx) -> bool {
        f.path[..usize::from(f.len)].iter().any(|a| matches!(a, Anchor::Lesson(_)))
    }

    /// What a click hits, a glow and an effect run to is where the light is drawn, from the first frame
    /// after a rebuild that moves the lessons (more of them re-cut the areas) and brings new ones.
    #[test]
    fn a_rebuild_puts_each_lessons_point_at_its_light_at_once() {
        let (m, mut s) = with_lessons(3, 200);
        s.set_dive(true);
        run(&mut s, 1.0);
        let before: Vec<[f32; 3]> = s.vol.inner.seats.iter().map(|seat| seat.at).collect();
        let ids = crate::paint::record::lesson_ids(260);
        s.lessons(&crate::paint::record::items(&ids), &m);
        assert!(s.vol.inner.seats.iter().zip(&before).any(|(seat, b)| seat.at != *b), "the rebuild moved lessons");
        let rot = s.cam.matrix();
        for (i, (p, seat)) in s.p_lesson.iter().zip(&s.vol.inner.seats).enumerate() {
            let q = s.cam.project(&rot, seat.at);
            assert!((p.x - q.x).abs() < 1e-3 && (p.y - q.y).abs() < 1e-3, "lesson {i} at {p:?}, its light at {q:?}");
        }
    }

    #[test]
    fn a_dive_hit_picks_the_lesson_under_the_pointer_and_out_of_it_the_core() {
        let (_, mut s) = with_lessons(3, 200);
        s.set_dive(true);
        run(&mut s, 1.0);
        assert!(s.dive > 0.99, "{}", s.dive);
        let rot = s.cam.matrix();
        let looks = |i: usize| s.vol.inner.seats[i].looks([rot[6], rot[7], rot[8]]);
        // A lesson whose light looks only at the camera and is hit at its point, and one whose light looks away.
        let hit = |i: usize| s.vol.inner.seats[i].normal != [0.0; 3] && s.hit(s.p_lesson[i].x + 0.5, s.p_lesson[i].y).is_some();
        let pick = (0..200).find(|&i| looks(i) && hit(i)).expect("a light toward the camera");
        let away = (0..200).find(|&i| !looks(i)).expect("a light looking away");
        let p = s.p_lesson[pick];
        let got = s.hit(p.x + 0.5, p.y);
        let Some(Anchor::Lesson(g)) = got else { panic!("{got:?}") };
        assert_eq!(Some(g as usize), expected_pick(&s, (p.x + 0.5, p.y)), "the light on top, else the nearest on screen");
        // A light that looks away is not hit where it alone is.
        let q = s.p_lesson[away];
        let others = |i: usize| i != away && looks(i);
        let alone = !(0..200).filter(|&i| others(i)).any(|i| (s.p_lesson[i].x - q.x).hypot(s.p_lesson[i].y - q.y) <= reach(&s, i));
        assert!(!alone || s.hit(q.x, q.y).is_none(), "a light looking away is hit");
        assert_ne!(s.hit(q.x, q.y), Some(Anchor::Lesson(ix(away))));
        // Out of the dive the lessons are not hit: the core is.
        s.set_dive(false);
        run(&mut s, 1.0);
        let p = s.p_lesson[pick];
        assert_eq!(s.hit(p.x, p.y), Some(Anchor::Core));
        s.set_dive(true);
        run(&mut s, 1.0);
        let p = s.p_lesson[pick];
        // Far from every lesson (a corner of the canvas), nothing; the machines are not drawn.
        assert_eq!(s.hit(1.0, 1.0), None);
        let n = s.p_node[1];
        assert!(!matches!(s.hit(n.x, n.y), Some(Anchor::Node(_))));
        // Between the lessons, inside the core: nothing either, not the collective.
        let gap = (0..40)
            .flat_map(|i| (0..40).map(move |j| (s.cam.w * f32::from(i as u8) / 40.0, s.cam.h * f32::from(j as u8) / 40.0)))
            .find(|&(x, y)| s.in_core(x, y) && s.hit(x, y).is_none())
            .expect("a gap between lessons inside the core");
        assert_eq!(s.hit(gap.0, gap.1), None);
        // A double-click leaves the dive between the lessons, not on one (a click opens it) and not off
        // the core.
        assert!(!s.toggles_dive(p.x, p.y) && s.toggles_dive(gap.0, gap.1));
        assert!(!s.toggles_dive(1.0, 1.0));
        // Back out: the core again, and a double-click there goes in; on a machine it does not.
        s.set_dive(false);
        run(&mut s, 1.0);
        assert!(s.dive < 0.01);
        let p = s.p_lesson[pick];
        assert_eq!(s.hit(p.x, p.y), Some(Anchor::Core));
        assert!(s.toggles_dive(p.x, p.y));
        let n = s.p_node[1];
        assert!(matches!(s.hit(n.x, n.y), Some(Anchor::Node(_))) && !s.toggles_dive(n.x, n.y));
        assert!(!s.toggles_dive(1.0, 1.0));
    }

    /// Where in the last frame's draw order the nearest opaque plating over the screen point lies, plus
    /// one (0: none covers it).
    fn cover(s: &Scene, x: f32, y: f32) -> usize {
        let inner = &s.vol.inner;
        s.drawn
            .iter()
            .rposition(|d| matches!(d.prim, Prim::Face(i) if matches!(inner.faces[i as usize].look, Look::Plate(_)) && covers(inner, &s.p_pts, &inner.faces[i as usize], (x, y))))
            .map_or(0, |k| k + 1)
    }

    /// Whether a light of lesson `i` is drawn at or after `k` in the last frame's draw order.
    fn lit_after(s: &Scene, i: usize, k: usize) -> bool {
        s.drawn[k..]
            .iter()
            .any(|d| matches!(d.prim, Prim::Face(f) if matches!(s.vol.inner.faces[f as usize].look, Look::Light { lesson, .. } if lesson as usize == i)))
    }

    /// Lesson `i`'s reach as a hit finds it: its drawn radius and 4 pixels, 8 at least.
    fn reach(s: &Scene, i: usize) -> f32 {
        (lesson_px(s.vol.inner.seats[i].r, s.p_lesson[i].s, s.dive) + 4.0).max(8.0)
    }

    /// The lesson a hit at the screen point should find in a dive: the light drawn on top there, else of
    /// the lights within reach that look at the camera and lie over the plating in front of the point,
    /// the nearest on screen.
    fn expected_pick(s: &Scene, (x, y): (f32, f32)) -> Option<usize> {
        let rot = s.cam.matrix();
        let d = |i: usize| (s.p_lesson[i].x - x).hypot(s.p_lesson[i].y - y);
        let k = cover(s, x, y);
        let near = (0..s.p_lesson.len()).filter(|&i| s.vol.inner.seats[i].looks([rot[6], rot[7], rot[8]]) && d(i) <= reach(s, i) && lit_after(s, i, k));
        on_top(s, x, y).or_else(|| near.min_by(|a, b| d(*a).total_cmp(&d(*b))))
    }

    /// The lesson whose light is drawn on top at the screen point, as the last frame drew it: the first
    /// face from near to far that covers it, gratings seen through, if that is a light.
    fn on_top(s: &Scene, x: f32, y: f32) -> Option<usize> {
        let inner = &s.vol.inner;
        let top = s.drawn.iter().rev().filter_map(|d| if let Prim::Face(i) = d.prim { Some(&inner.faces[i as usize]) } else { None });
        let face = top.filter(|f| !matches!(f.look, Look::Grating(_))).find(|f| covers(inner, &s.p_pts, f, (x, y)))?;
        if let Look::Light { lesson, .. } = face.look { Some(lesson as usize) } else { None }
    }

    #[test]
    fn a_sync_leaves_the_lessons_and_the_interior_to_the_next_frame() {
        let (m, mut s) = with_lessons(3, 300);
        let (drawn, lessons) = (s.drawn.clone(), s.p_lesson.clone());
        // The camera turned since the last frame: a sync does not order or project for it.
        s.cam.yaw += 1.0;
        s.sync(&m);
        assert!(s.drawn == drawn && s.p_lesson == lessons, "a sync made a frame ready");
        // The next frame does.
        s.frame(s.t);
        assert!(s.drawn != drawn && s.p_lesson != lessons);
    }

    #[test]
    fn a_dive_hits_a_lesson_where_its_light_shows() {
        for (n, w, h) in [(200, 800.0, 520.0), (600, 800.0, 520.0), (600, 1400.0, 900.0)] {
            let (_, mut s) = with_lessons(3, n);
            (s.cam.w, s.cam.h) = (w, h);
            s.set_dive(true);
            run(&mut s, 1.0);
            // At each lesson's own point and the middle of each of its lights drawn, where that light is
            // what shows on top.
            let inner = &s.vol.inner;
            let lights = s.drawn.iter().filter_map(|d| {
                let Prim::Face(i) = d.prim else { return None };
                let f = &inner.faces[i as usize];
                let Look::Light { lesson, .. } = f.look else { return None };
                let ix = &inner.corners[f.first as usize..f.first as usize + usize::from(f.n)];
                let k = 1.0 / ix.len() as f32;
                let mid = ix.iter().fold((0.0, 0.0), |(x, y), &c| (x + s.p_pts[c as usize].x * k, y + s.p_pts[c as usize].y * k));
                Some((lesson as usize, mid))
            });
            let points: Vec<(usize, (f32, f32))> = (0..n).map(|i| (i, (s.p_lesson[i].x, s.p_lesson[i].y))).chain(lights).collect();
            let mut tried = 0;
            for (i, (x, y)) in points {
                if (0.0..w).contains(&x) && (0.0..h).contains(&y) && on_top(&s, x, y) == Some(i) {
                    tried += 1;
                    assert_eq!(s.hit(x, y), Some(Anchor::Lesson(ix(i))), "{n} lessons on {w}x{h}: lesson {i} at ({x}, {y})");
                }
            }
            assert!(tried > 60, "{n} lessons on {w}x{h}: {tried} tried");
        }
    }

    #[test]
    fn a_dive_hit_skips_a_lesson_behind_plating() {
        let (_, mut s) = with_lessons(3, 600);
        s.set_dive(true);
        run(&mut s, 1.0);
        let rot = s.cam.matrix();
        let view = [rot[6], rot[7], rot[8]];
        // Lessons whose light looks at the camera, but at whose point plating is drawn over every light
        // of theirs: the shell's near ring, another bay's posts and columns.
        let (mut hidden, mut shown) = (0, 0);
        for i in 0..600 {
            let p = s.p_lesson[i];
            if !s.vol.inner.seats[i].looks(view) || !(0.0..s.cam.w).contains(&p.x) || !(0.0..s.cam.h).contains(&p.y) {
                continue;
            }
            let k = cover(&s, p.x, p.y);
            if lit_after(&s, i, k) {
                shown += usize::from(s.hit(p.x, p.y).is_some());
            } else {
                hidden += 1;
                assert_ne!(s.hit(p.x, p.y), Some(Anchor::Lesson(ix(i))), "lesson {i} behind plating");
            }
        }
        assert!(hidden > 20 && shown > 50, "{hidden} hidden, {shown} shown");
    }

    /// Views from all round, none along an axis.
    const VIEWS: [[f32; 3]; 4] = [[0.5, 0.3, 0.81], [-0.6, -0.4, 0.69], [0.1, 0.7, 0.71], [-0.36, 0.48, -0.8]];

    /// What of `inner` a frame along `view` draws, in the order it is drawn (as `gather` and
    /// `count_sort` order it), fine detail included; and where in that order each primitive is.
    fn sorted(inner: &Interior, view: [f32; 3]) -> HashMap<Prim, usize> {
        let p: Vec<P2> = inner.points.iter().map(|q| P2 { z: crate::interior::dot(*q, view), ..P2::default() }).collect();
        let (mut keys, mut out) = (Vec::with_capacity(inner.prims()), Vec::with_capacity(inner.prims()));
        gather(inner, &p, &Sight::new(view, true), &mut keys);
        count_sort(&keys, &mut out);
        out.iter().enumerate().map(|(k, d)| (d.prim, k)).collect()
    }

    /// The middle of face `f`'s outline, in world space.
    fn centre(inner: &Interior, f: &Face) -> [f32; 3] {
        let ix = &inner.corners[f.first as usize..f.first as usize + usize::from(f.n)];
        let sum = ix.iter().fold([0.0; 3], |a, &i| std::array::from_fn(|k| a[k] + inner.points[i as usize][k]));
        sum.map(|c| c / ix.len() as f32)
    }

    fn near(a: [f32; 3], b: [f32; 3], d: f32) -> bool {
        (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f32>().sqrt() <= d
    }

    /// Each fan of `inner`: its housing (an octagon of plating), and its blades (plating) and rim (fan
    /// strokes) found by where they lie: on the housing's disc.
    fn fans(inner: &Interior) -> Vec<(Prim, Vec<Prim>)> {
        use crate::interior::Stroke;
        let housings = inner.faces.iter().enumerate().filter(|(_, f)| f.n == 8 && matches!(f.look, Look::Plate(_)));
        housings
            .map(|(h, f)| {
                let c = centre(inner, f);
                let r = (0..3).map(|k| (inner.points[inner.corners[f.first as usize] as usize][k] - c[k]).powi(2)).sum::<f32>().sqrt();
                let on = |p: u32| near(inner.points[p as usize], c, r * 1.01);
                let blade =
                    |g: &Face| g.n == 4 && matches!(g.look, Look::Plate(_)) && inner.corners[g.first as usize..g.first as usize + 4].iter().all(|&p| on(p));
                let blades = inner.faces.iter().enumerate().filter(|(i, g)| *i != h && blade(g)).map(|(i, _)| Prim::Face(ix(i)));
                let rim = inner.lines.iter().enumerate().filter(|(_, l)| l.style == Stroke::Fan && on(l.a) && on(l.b)).map(|(i, _)| Prim::Line(ix(i)));
                (Prim::Face(ix(h)), blades.chain(rim).collect())
            })
            .collect()
    }

    #[test]
    fn a_fans_rim_and_blades_lie_over_its_housing_however_it_turns() {
        use crate::interior::{Kind, Room, kinds};
        let key = (0..100).map(|i| format!("a{i}")).find(|k| kinds(&[k.as_str()]) == [Kind::Vents]).expect("a key of the vents");
        let room = Room { min: [-0.5; 3], max: [0.5; 3], key, title: String::new(), lessons: (0..40).collect() };
        let inner = Interior::build(&[room], &[], 40);
        let fans = fans(&inner);
        assert!(fans.len() >= 4, "{} fans", fans.len());
        for view in VIEWS {
            let at = sorted(&inner, view);
            let mut over = 0;
            for (housing, parts) in fans.iter().filter(|(h, _)| at.contains_key(h)) {
                let drawn: Vec<usize> = parts.iter().filter_map(|p| at.get(p).copied()).collect();
                assert!(drawn.iter().all(|k| *k > at[housing]), "a blade or rim segment under its housing along {view:?}");
                over += drawn.len();
            }
            assert!(over > 60, "along {view:?}: {over} blades and rim segments");
        }
    }

    /// The shell's seams and hull lights, each with the panel it sits on: the one whose middle is
    /// nearest, within the 0.012 a light or a seam stood off its panel before.
    fn on_panels(inner: &Interior) -> Vec<(Prim, Prim)> {
        use crate::interior::{Gate, Stroke};
        let panels: Vec<(usize, [f32; 3])> =
            inner.faces.iter().enumerate().filter(|(_, f)| matches!(f.gate, Gate::Shell { .. })).map(|(i, f)| (i, centre(inner, f))).collect();
        let dist = |c: [f32; 3], p: [f32; 3]| (0..3).map(|k| (c[k] - p[k]).powi(2)).sum::<f32>();
        let panel = |p: [f32; 3]| {
            let (i, c) = panels.iter().min_by(|a, b| dist(a.1, p).total_cmp(&dist(b.1, p)))?;
            near(*c, p, 0.012).then_some(Prim::Face(ix(*i)))
        };
        let mid = |l: &crate::interior::Line| std::array::from_fn(|k| 0.5 * (inner.points[l.a as usize][k] + inner.points[l.b as usize][k]));
        let seams = inner.lines.iter().enumerate().filter(|(_, l)| l.style == Stroke::Seam).filter_map(|(i, l)| Some((Prim::Line(ix(i)), panel(mid(l))?)));
        let lights = inner.glows.iter().enumerate().filter_map(|(i, g)| Some((Prim::Glow(ix(i)), panel(inner.points[g.at as usize])?)));
        seams.chain(lights).collect()
    }

    #[test]
    fn a_shell_panels_seam_and_light_lie_over_it_from_outside_and_inside() {
        let inner = Interior::build(&[], &[], 0);
        let on = on_panels(&inner);
        for view in VIEWS {
            let at = sorted(&inner, view);
            let drawn: Vec<(Prim, usize, usize)> = on.iter().filter_map(|(p, under)| Some((*p, *at.get(p)?, *at.get(under)?))).collect();
            assert!(drawn.iter().all(|(_, k, under)| k > under), "a seam or a hull light under its panel along {view:?}");
            let lights = drawn.iter().filter(|d| matches!(d.0, Prim::Glow(_))).count();
            assert!(drawn.len() - lights > 300 && lights > 40, "along {view:?}: {} seams, {lights} lights", drawn.len() - lights);
        }
    }

    #[test]
    fn while_the_core_is_small_the_points_only_fine_detail_uses_are_not_projected() {
        let (_, mut s) = with_lessons(3, 400);
        s.still = true;
        s.frame(2.0);
        assert!(s.p_center.s * CORE < FINE_PX);
        let coarse = s.vol.inner.coarse;
        assert!(coarse < s.p_pts.len() && s.p_pts[coarse..].iter().all(|p| *p == P2::default()));
        s.cam.zoom = 3.0;
        s.frame(2.0);
        assert!(s.p_pts[coarse..].iter().all(|p| p.s > 0.0));
    }

    #[test]
    fn a_dive_frames_the_core_between_the_text_over_the_canvas() {
        let (_, mut s) = with_lessons(3, 50);
        let free = s.p_center;
        // The counters cover the top 100 pixels, the deck the bottom 200: out of a dive, that changes nothing.
        (s.cam.top, s.cam.bottom) = (100.0, 200.0);
        s.frame(s.t + 0.01);
        assert!((s.p_center.y - free.y).abs() < 1e-3 && (s.p_center.s - free.s).abs() < 1e-3);
        s.set_dive(true);
        run(&mut s, 1.0);
        let band = s.cam.h - 300.0;
        assert!((s.p_center.y - (100.0 + band * 0.5)).abs() < 1e-3, "in the band's middle: {}", s.p_center.y);
        let span = 2.0 * 3f32.sqrt() * CORE * s.p_center.s / s.cam.w.min(band);
        assert!((0.8..0.9).contains(&span), "{span} of the band");
        // Text over nearly all of it: the core still gets `BAND_MIN` of the canvas.
        (s.cam.top, s.cam.bottom) = (s.cam.h * 0.6, s.cam.h * 0.6);
        s.frame(s.t + 0.01);
        let span = 2.0 * 3f32.sqrt() * CORE * s.p_center.s / (s.cam.h * BAND_MIN);
        assert!((0.8..0.9).contains(&span), "{span} of the least band");
    }

    #[test]
    fn a_dive_goes_in_and_out_and_settles() {
        let (_, mut s) = with_lessons(3, 50);
        let (f0, c0, yaw0) = (s.cam.focal(), s.p_center, s.cam.yaw);
        assert!(!s.diving() && s.dive == 0.0);
        s.set_dive(true);
        assert!(s.diving());
        let mut t = 1.0;
        let mut last = s.dive;
        let mut steps = 0;
        while s.dive < 1.0 && steps < 600 {
            t += 1.0 / 60.0;
            s.frame(t);
            assert!(s.dive >= last, "it only goes in");
            last = s.dive;
            steps += 1;
        }
        // About two thirds of a second.
        assert!((30..=50).contains(&steps), "{steps} frames");
        assert!((s.cam.focal() / f0 - DIVE_ZOOM).abs() < 1e-4, "the lens: {}", s.cam.focal() / f0);
        assert!((s.p_center.y - s.cam.h * 0.5).abs() < 1e-3 && c0.y > s.p_center.y, "centred");
        // The core spans about 85% of the shorter side.
        let span = 2.0 * 3f32.sqrt() * CORE * s.p_center.s / s.cam.w.min(s.cam.h);
        assert!((0.8..0.9).contains(&span), "{span}");
        // The user's zoom still applies on top.
        s.cam.zoom = 2.0;
        s.frame(t + 0.01);
        assert!((s.cam.focal() / f0 - 2.0 * DIVE_ZOOM).abs() < 1e-3);
        s.cam.zoom = 1.0;
        // Settled: nothing redraws for the dive, and the spin is slower.
        s.still = true;
        s.frame(t + 0.1);
        let shown = s.shown();
        s.frame(t + 2.0);
        assert!(!s.needs_draw(&shown));
        s.still = false;
        let y = s.cam.yaw;
        s.frame(t + 2.1);
        assert!(((s.cam.yaw - y) - SPIN * 0.2 * 0.1).abs() < 1e-5, "slowed: {}", s.cam.yaw - y);
        assert!(s.cam.yaw > yaw0);
        // Out again, all the way.
        s.set_dive(false);
        let mut out = 0;
        while s.dive > 0.0 && out < 600 {
            s.frame(s.t + 1.0 / 60.0);
            out += 1;
        }
        assert!((30..=50).contains(&out), "{out} frames");
        assert!((s.cam.focal() - f0).abs() < 1e-3 && s.cam.lens == 0.0);
        // Reduced motion jumps.
        s.still = true;
        s.set_dive(true);
        s.frame(s.t + 0.01);
        assert!((s.dive - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn redraws_follow_a_dive_and_stop_when_it_settles() {
        let (_, mut s) = with_lessons(3, 50);
        s.still = true;
        s.frame(5.0);
        let mut last = s.shown();
        s.set_dive(true);
        let mut drawn = 0;
        for i in 1..=120 {
            s.frame(5.0 + f64::from(i) / 60.0);
            if s.needs_draw(&last) {
                drawn += 1;
                last = s.shown();
            }
        }
        // Still: it jumps, one frame.
        assert_eq!(drawn, 1);
        s.still = false;
        s.set_dive(false);
        drawn = 0;
        for i in 121..=180 {
            s.frame(5.0 + f64::from(i) / 60.0);
            if s.needs_draw(&last) {
                drawn += 1;
                last = s.shown();
            }
        }
        // Every frame of the way out draws.
        assert!(drawn >= 40, "{drawn}");
    }

    #[test]
    fn an_idle_scene_with_lessons_stops_redrawing_once_its_lights_settle() {
        let (m, mut s) = with_lessons(3, 300);
        s.still = true;
        let ids = crate::paint::record::lesson_ids(300);
        let mut last = Shown::default();
        let frames = |s: &mut Scene, from: f64, last: &mut Shown| {
            let mut drawn_at = Vec::new();
            for i in 1..=300 {
                let t = from + f64::from(i) / 60.0;
                s.frame(t);
                if s.needs_draw(last) {
                    drawn_at.push(t);
                    *last = s.shown();
                }
            }
            (drawn_at.len(), drawn_at.last().copied().unwrap_or_default())
        };
        let (n, end) = frames(&mut s, 1.0, &mut last);
        assert!(n >= 1 && end < 1.1, "{n} frames, the last at {end}");
        // A rebuild moves its lessons at once: no frame after the one the view draws for the new data
        // itself (views/cube.rs `dirty`).
        s.lessons(&crate::paint::record::items(&ids[..280]), &m);
        assert_eq!(frames(&mut s, 6.0, &mut last).0, 0, "nothing moves on");
        // An absorb's ring (1.6 s) and its light (2 s) draw, then nothing.
        s.play(lesson_ev(&m, EvKind::Absorb, "key-0", "L9"));
        let (n, end) = frames(&mut s, 11.0, &mut last);
        assert!(n > 60 && (12.9..13.2).contains(&end), "{n} frames, the last at {end}");
        // And a hover draws once.
        s.hovered = Some(3);
        assert!(s.needs_draw(&last));
        last = s.shown();
        s.frame(17.0);
        assert!(!s.needs_draw(&last));
        // Turning, lessons keep the idle rate an empty core has.
        s.still = false;
        let (mut last, mut drawn) = (s.shown(), 0);
        for i in 1..=600 {
            s.frame(17.0 + f64::from(i) / 60.0);
            if s.needs_draw(&last) {
                drawn += 1;
                last = s.shown();
            }
        }
        assert!(f64::from(drawn) / 10.0 <= 22.0, "{drawn} in ten seconds");
    }

    #[test]
    fn a_frame_that_is_not_drawn_projects_nothing_of_the_core() {
        let (_, mut s) = with_lessons(3, 200);
        let (drawn, pts, order) = (s.p_lesson.clone(), s.p_pts.clone(), s.drawn.clone());
        s.advance(1.5);
        assert_eq!(s.p_lesson, drawn, "what a click hits is what was drawn");
        assert!(s.p_pts == pts && s.drawn == order, "nor the interior");
        s.prepare();
        assert_ne!(s.p_lesson, drawn, "the drawn frame projects them at the turned camera");
        assert_ne!(s.p_pts, pts);
    }

    #[test]
    fn the_cores_corners_bound_how_far_a_turn_moves_any_lesson() {
        let (_, mut s) = with_lessons(3, 400);
        for (dive, yaw, pitch, zoom) in [(false, 0.3, 0.2, 1.0), (true, 1.1, -0.6, 2.5), (true, 2.0, 1.3, 4.0)] {
            s.set_dive(dive);
            (s.cam.yaw, s.cam.pitch, s.cam.zoom) = (yaw, pitch, zoom);
            run(&mut s, 1.0);
            for (p, w) in s.p_lesson.iter().zip(s.vol.inner.seats.iter().map(|seat| seat.at)) {
                assert!(w[0].hypot(w[2]) * p.s <= s.spin_px + 1e-3, "a lesson outruns the bound {}", s.spin_px);
            }
        }
    }

    #[test]
    fn deep_in_a_dive_the_outside_stops_redrawing() {
        let (m, mut s) = with_lessons(6, 50);
        s.set_dive(true);
        let t = run(&mut s, 1.0);
        assert!(s.outside() == 0.0 && s.dive > DIVE_HIDE);
        // Drones and lights would draw many frames a second; deep in a dive only the slowed spin of
        // the core moves.
        let (mut last, mut drawn) = (s.shown(), 0u32);
        for i in 1..=600 {
            s.frame(t + f64::from(i) / 60.0);
            if s.needs_draw(&last) {
                drawn += 1;
                last = s.shown();
            }
        }
        // The core's corners turn at a fifth of the spin: a pixel about every half second. The lights'
        // four steps a second, or the machines' spin, would draw more.
        let rate = f64::from(drawn) / 10.0;
        assert!(rate < 3.0 && rate < idle_rate(6, 0, s.cam.w, s.cam.h) / 4.0, "{rate} a second");
        // Nor does traffic outside the core, which a deep dive does not draw: a message between
        // machines every second keeps an effect running throughout.
        let t = s.t;
        let (k0, k2) = (id(&m, "key-0"), id(&m, "key-2"));
        let (mut last, mut drawn) = (s.shown(), 0u32);
        for i in 1..=600 {
            if i % 60 == 1 {
                s.play(ev(EvKind::Tell, k0, None, To::Local, Some(k2), None, 0));
            }
            s.frame(t + f64::from(i) / 60.0);
            if s.needs_draw(&last) {
                drawn += 1;
                last = s.shown();
            }
        }
        assert!(!s.fx.is_empty() && f64::from(drawn) / 10.0 < 3.0, "{} a second with traffic outside", f64::from(drawn) / 10.0);
        // A packet to a lesson is drawn there, and draws.
        s.play(lesson_ev(&m, EvKind::Lesson, "key-1", "L3"));
        s.frame(s.t + 1.0 / 60.0);
        assert!(s.needs_draw(&last));
    }
}
