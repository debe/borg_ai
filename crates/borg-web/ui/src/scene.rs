//! The Cube's world, without the browser: where the machines and drones are, the camera that projects
//! them, and the effects that play what moved (a packet from one drone to another, a flash where one
//! was shown it, a lesson assimilated, a hand-off, a mesh sync). `paint.rs` draws it; `views/cube.rs`
//! feeds it the model and the clock.
//!
//! The collective is the core cube at the origin. Machines sit on a sphere around it, each at a place
//! its key alone decides (`direction`), so a machine keeps its place when others join; this node sits
//! in front. Drones orbit their machine. An effect runs along anchors (drone, machine, core) projected
//! once per frame, so it follows them as the cube turns.
//!
//! Steady state allocates nothing: `frame` projects into buffers sized by `sync`, and effects live in
//! a pool, kept in style order as they join, that only grows past its high-water mark.
//! `needs_draw` says when a frame would look the same as the last one drawn, so the view can skip it.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::suboptimal_flops,
    reason = "f32 screen graphics: the casts are bounded index and clock maths; f32::mul_add is a libm call on wasm32 without FMA"
)]

use std::collections::HashMap;

use crate::model::{EvKind, Model, Play, To};

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
/// Display: the core's lights change state this often per second, so an idle view redraws for them at this
/// rate, not every frame.
const BLINK: f64 = 4.0;
/// Display: motion below a pixel is not worth a frame: the auto-rotation moves the outermost machine 10 to 20
/// pixels a second, so an idle view redraws at about that rate, not at the display's.
const PX: f32 = 1.0;
/// Guard: the most machines or drones the scene keeps, so every index fits a `u32` (wasm32's `usize` is no
/// wider, so in the browser this never cuts anything).
const MAX: usize = u32::MAX as usize;

/// An index as the scene keeps it; `MAX` makes the conversion lossless.
fn ix(i: usize) -> u32 {
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
    /// Lifts the core's glow by `n` tenths when its start comes (a lesson arriving); never drawn.
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
    pub zoom: f32,
    pub w: f32,
    pub h: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self { yaw: 0.6, pitch: 0.38, zoom: 1.0, w: 800.0, h: 520.0 }
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

    /// Pixels per world unit at the origin.
    #[must_use]
    pub fn focal(&self) -> f32 {
        self.w.min(self.h) * 1.02 * self.zoom
    }

    #[must_use]
    pub fn project(&self, rot: &[f32; 9], at: [f32; 3]) -> P2 {
        let x = rot[0] * at[0] + rot[1] * at[1] + rot[2] * at[2];
        let y = rot[3] * at[0] + rot[4] * at[1] + rot[5] * at[2];
        let z = rot[6] * at[0] + rot[7] * at[1] + rot[8] * at[2];
        let scale = self.focal() * DISTANCE / (DISTANCE - z).max(0.5) / DISTANCE;
        // A little below the middle: the counters take the top.
        P2 { x: self.w * 0.5 + x * scale, y: self.h * 0.54 - y * scale, s: scale, z }
    }

    /// Keeps the pitch off the poles and the zoom sane.
    pub const fn clamp(&mut self) {
        self.pitch = self.pitch.clamp(-1.35, 1.35);
        self.zoom = self.zoom.clamp(0.35, 4.0);
    }

    /// Whether two cameras differ in anything but the yaw (which `needs_draw` weighs in pixels).
    fn same_but_yaw(&self, o: &Self) -> bool {
        [self.pitch, self.zoom, self.w, self.h].map(f32::to_bits) == [o.pitch, o.zoom, o.w, o.h].map(f32::to_bits)
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

/// The core's surface detail, the same every time: conduits along its plating and lights that blink.
/// Each is on a face, so `paint` draws only the faces that look at the camera.
#[must_use]
pub fn plating() -> (Vec<Conduit>, Vec<Light>) {
    const CELLS: f32 = 8.0;
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 40) as f32 / (1u64 << 24) as f32
    };
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
}

impl Projected<'_> {
    /// Where an anchor is (the core's centre for one that names nothing).
    fn get(&self, a: Anchor) -> P2 {
        match a {
            Anchor::Core => self.center,
            Anchor::Node(i) => self.node.get(i as usize).copied().unwrap_or(self.center),
            Anchor::Drone(i) => self.drone.get(i as usize).copied().unwrap_or(self.center),
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
    /// Each effect's trail (`TRAIL`), as `frame` last projected it: a packet's head and the points
    /// behind it, default for other kinds. Paint reads it when it matches `fx`, else works it out.
    pub trail: Vec<[P2; TRAIL]>,
    /// The core's glow, decaying: a lesson or a burst of traffic lifts it.
    pub flash: f32,
    /// Motion is reduced: no auto-rotation, no orbits, no trails.
    pub still: bool,
    /// The core's conduits and lights (`plating`), made on the first frame.
    pub conduits: Vec<Conduit>,
    pub lights: Vec<Light>,
    /// Pixels the outermost machine moves per radian of yaw, and the fastest drone per second, as
    /// last projected: what `needs_draw` weighs motion by.
    spin_px: f32,
    orbit_px: f32,
    by_session: HashMap<u32, u32>,
    by_key: HashMap<u32, u32>,
}

impl Scene {
    /// Takes the machines and drones of `m`; the camera, clock and running effects stay, each effect
    /// still on the machines and drones it named (by key and session, since their indices change),
    /// and ended when one of them is gone.
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
        self.fx.retain_mut(|f| {
            f.path[..usize::from(f.len).min(PATH)].iter_mut().all(|a| {
                let moved = match *a {
                    Anchor::Core => Some(Anchor::Core),
                    Anchor::Node(i) => old_keys.get(i as usize).and_then(|k| by_key.get(k)).map(|n| Anchor::Node(*n)),
                    Anchor::Drone(i) => old_sessions.get(i as usize).and_then(|s| by_session.get(s)).map(|d| Anchor::Drone(*d)),
                };
                let Some(n) = moved else { return false };
                *a = n;
                true
            })
        });
        self.trail.clear();
        self.place();
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
        Projected { center: self.p_center, node: &self.p_node, drone: &self.p_drone }
    }

    /// Advances to the clock `t`: turns, lifts and decays the core's glow, ends finished effects,
    /// projects everything and works out the packets' trails.
    pub fn frame(&mut self, t: f64) {
        if self.conduits.is_empty() {
            (self.conduits, self.lights) = plating();
        }
        let dt = (t - self.t).clamp(0.0, 0.25) as f32;
        self.t = t;
        if !self.still {
            self.cam.yaw += SPIN * dt;
        }
        self.flash = (self.flash - dt * 0.8).max(0.0);
        for f in &self.fx {
            if f.kind == FxKind::Lift && f.t0 <= t {
                self.flash += f32::from(f.n) * 0.1;
            }
        }
        self.flash = self.flash.min(1.0);
        self.fx.retain(|f| if f.kind == FxKind::Lift { f.t0 > t } else { f.progress(t) <= 1.0 });
        self.place();
        // Each packet's trail once, for every alpha level paint strokes it at.
        self.trail.resize(self.fx.len(), [P2::default(); TRAIL]);
        let at = Projected { center: self.p_center, node: &self.p_node, drone: &self.p_drone };
        for (pts, f) in self.trail.iter_mut().zip(&self.fx) {
            if f.kind == FxKind::Packet {
                let (run, u) = (at.run(f), f.progress(t));
                for (k, p) in pts.iter_mut().enumerate() {
                    *p = run.at(u - TRAIL_STEP * k as f32);
                }
            }
        }
    }

    /// Projects the core, machines and drones at the camera and clock, orders the machines far to
    /// near, and measures how fast the view moves on screen for `needs_draw`.
    fn place(&mut self) {
        let m = self.cam.matrix();
        for (p, c) in self.p_core.iter_mut().zip(CORNERS) {
            *p = self.cam.project(&m, c);
        }
        self.p_center = self.cam.project(&m, [0.0; 3]);
        let mut spin = CORE * std::f32::consts::SQRT_2 * self.p_center.s;
        for (p, w) in self.p_node.iter_mut().zip(&self.node_pos) {
            *p = self.cam.project(&m, *w);
            // A point turns about the vertical axis at its distance from it; its drones a ring further.
            spin = spin.max((w[0].hypot(w[2]) + Self::ring(0) * 1.5) * p.s);
        }
        let mut orbit = 0.0f32;
        for i in 0..self.drone_node.len() {
            let w = self.drone_world(i);
            if let Some(p) = self.p_drone.get_mut(i) {
                *p = self.cam.project(&m, w);
                let speed = self.drone_state.get(i).map_or(0.0, |s| s.speed());
                let count = self.drone_slot.get(i).map_or(0, |s| s.1);
                orbit = orbit.max(speed * Self::ring(count) * p.s);
            }
        }
        self.spin_px = spin;
        self.orbit_px = if self.still { 0.0 } else { orbit };
        let p = &self.p_node;
        let z = |i: u32| p.get(i as usize).map_or(0.0, |q| q.z);
        self.order.sort_unstable_by(|a, b| z(*a).total_cmp(&z(*b)));
    }

    /// The clock the core's lights blink by: it steps `BLINK` times a second.
    #[must_use]
    pub fn blink(&self) -> f32 {
        ((self.t * BLINK).floor() / BLINK) as f32
    }

    /// What this frame shows; keep it when the frame is drawn, for `needs_draw`.
    #[must_use]
    pub const fn shown(&self) -> Shown {
        Shown { drawn: true, t: self.t, cam: self.cam }
    }

    /// Whether a frame drawn now would look different from `last` (a `shown` of the last drawn frame;
    /// `Shown::default()` before any): an effect runs, the core's glow decays, the camera's pitch,
    /// zoom or size changed, the yaw (auto-rotation or a drag) moved the outermost machine a pixel or
    /// more, a drone's orbit moved it a pixel or more, or the core's lights stepped. Call it
    /// after `frame`. Resizes and new data the view knows of itself.
    #[must_use]
    pub fn needs_draw(&self, last: &Shown) -> bool {
        if !last.drawn || !self.fx.is_empty() || self.flash > 0.0 || !self.cam.same_but_yaw(&last.cam) {
            return true;
        }
        let dt = (self.t - last.t).max(0.0) as f32;
        (self.cam.yaw - last.cam.yaw).abs() * self.spin_px >= PX
            || self.orbit_px * dt >= PX
            || (!self.still && (self.t * BLINK).floor().to_bits() != (last.t * BLINK).floor().to_bits())
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
            Anchor::Core => None,
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
        match ev.kind {
            EvKind::Tell | EvKind::Ask | EvKind::Pause | EvKind::Resume | EvKind::Veto => self.message(ev),
            EvKind::Seen => {
                if let Some(a) = self.at(ev.from_s, Some(ev.from_n)) {
                    self.push(FxKind::Flash, Tone::Cyan, &[a], 0.9);
                }
            }
            EvKind::Lesson | EvKind::LessonUpdate => self.lesson(ev.from_n, ev.kind == EvKind::LessonUpdate),
            EvKind::Absorb => self.push(FxKind::Burst, Tone::Bright, &[Anchor::Core], 1.6),
            EvKind::Release => {
                self.push(FxKind::Collapse, Tone::Green, &[Anchor::Core], 1.4);
                self.lift(1.4, 3);
            }
            EvKind::Handoff => self.handoff(ev, how),
            EvKind::Sync | EvKind::SyncFailed => self.mesh(ev),
        }
    }

    /// Lifts the core's glow by `tenths` in `delay` seconds.
    fn lift(&mut self, delay: f32, tenths: u8) {
        let f = Fx { t0: self.t + f64::from(delay), n: tenths, ..self.make(FxKind::Lift, Tone::Bright, &[Anchor::Core], 0.0) };
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

    /// A lesson written on machine `key`: an assimilation there, a bright packet into the core, and the
    /// core flares when it arrives. A new version is the smaller.
    fn lesson(&mut self, key: u32, update: bool) {
        let (rings, tenths) = if update { (1, 3) } else { (2, 6) };
        // A machine the scene does not have: the core assimilates it alone.
        let origin = self.by_key.get(&key).map(|n| Anchor::Node(*n));
        let shock = Fx { n: rings, ..self.make(FxKind::Shock, Tone::Bright, &[origin.unwrap_or(Anchor::Core)], 1.3) };
        self.add(shock);
        let travel = 1.1;
        if let Some(o) = origin {
            self.push(FxKind::Packet, Tone::Bright, &[o, Anchor::Core], travel);
        }
        self.lift(if origin.is_some() { travel } else { 0.0 }, tenths);
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

    /// The machine or drone under the screen point, nearest first within its drawn size; else the core
    /// when the point is within its projected half-diagonal.
    #[must_use]
    pub fn hit(&self, x: f32, y: f32) -> Option<Anchor> {
        let near = |p: &P2, r: f32| {
            let d = (p.x - x).hypot(p.y - y);
            (d <= r).then_some(d)
        };
        let drone = self.p_drone.iter().enumerate().filter_map(|(i, p)| near(p, (p.s * 0.07).max(7.0)).map(|d| (d, Anchor::Drone(ix(i)))));
        let node = self.p_node.iter().enumerate().filter_map(|(i, p)| near(p, (p.s * 0.2).max(12.0)).map(|d| (d, Anchor::Node(ix(i)))));
        let core = || near(&self.p_center, CORE * std::f32::consts::SQRT_2 * self.p_center.s).map(|_| Anchor::Core);
        drone.chain(node).min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, a)| a).or_else(core)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A model of `nodes` machines (the first is this node, all linked to it) and `drones` drones
    /// spread over them in turn, their states cycling; parsed as `/api/mesh` sends it.
    pub fn model(nodes: usize, drones: usize) -> Model {
        let mut strs: Vec<String> = (0..nodes).map(|i| format!("\"key-{i}\"")).collect();
        strs.extend((0..drones).map(|i| format!("\"sess-{i}\"")));
        let ns: Vec<String> = (0..nodes).map(|i| format!(r#"{{"key":{i},"host":"host-{i}","me":{},"syncs":true}}"#, i == 0)).collect();
        let ls: Vec<String> = (1..nodes).map(|i| format!(r#"{{"a":0,"b":{i},"dial":true,"ms":900,"ok":{}}}"#, i % 5 != 0)).collect();
        let states = ["working", "waiting", "idle", "ended"];
        let ds: Vec<String> = (0..drones)
            .map(|i| format!(r#"{{"session":{},"node":{},"name":"d{i}","number":{i},"state":"{}"}}"#, nodes + i, i % nodes.max(1), states[i % 4]))
            .collect();
        let text = format!(
            r#"{{"now":1000,"me":0,"strs":[{}],"nodes":[{}],"links":[{}],"drones":[{}],"events":[]}}"#,
            strs.join(","),
            ns.join(","),
            ls.join(","),
            ds.join(",")
        );
        let mut m = Model::default();
        assert!(m.merge(&text).is_some(), "the test model parses");
        m
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
        Play { kind, from_n, from_s, to, to_n, to_s, n }
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
                Anchor::Core => true,
                Anchor::Node(i) => (*i as usize) < s.node_pos.len(),
                Anchor::Drone(i) => (*i as usize) < s.drone_node.len(),
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
}
