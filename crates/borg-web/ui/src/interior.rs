//! The inside of the Cube's core when it holds lessons, built as a Borg cube is: an outer shell of dark
//! plating on every face, and inside it one area per cluster of lessons (a concern area), sized by its
//! lessons (volume.rs cuts the core into them), with corridors in the shafts between them.
//!
//! The shell is plated all over: 16 by 16 panels a face, each recessed at its own depth, green seams,
//! structural ribs every 4 panels and a few hull lights. A face turned away from the camera shows all
//! its plating (the dark back of the cube); a face turned toward it is broken open from its edges, a
//! ragged ring of panels around an open centre, so the inside shows. Which faces are open follows the
//! camera: every panel is built once with a flag for each case (`Gate::Shell`), and the frame picks.
//!
//! Each area is one of eight Borg kinds (`Kind`), handed out in the order of the areas' name hashes so
//! the areas of one view differ: tiers of regeneration alcoves, alcoves ringed round a glowing column,
//! a gallery of two alcove walls round a lit shaft, spires of tube bundles with lit nodes, a honeycomb
//! of hex cells, maturation chambers with glowing slits, a vinculum column with light panels, or a wall
//! of vents with status lights. Each lesson is one light of its area: an alcove's glowing back and its
//! bright slit, a node, a cell, a slit, a panel or a status light, in the order of its id's hash
//! (volume.rs); `seats` says where its light is. Frame posts stand just outside the areas' corners,
//! risers climb the shafts, and walkways with a lit strip cross them.
//!
//! Built in world space when the lessons change (it allocates): points, faces over them, lines, glows
//! and labels. The areas' solids (boxes, cylinders, alcoves, Borg skin, hex cells) are shaded once by a
//! fixed world light (`LIGHT`); the shell's panels each take a shade of their own at random (0.12 to
//! 0.42), and walkways, fans and ledges fixed shades. Every shade is quantised once (paint.rs
//! `shade_level`), so a frame only projects the points, culls, orders (scene.rs `Sight::key`) and fills
//! (scene.rs, paint.rs). A line along points a face already has runs over them rather than new copies.
//! Fine detail (seams, rails, ribs inside the areas, the tubes over the alcoves, the alcoves' frames) is
//! flagged `fine`, and the points only it uses come last (`Interior::coarse`), so a frame where the core
//! is small on screen neither draws nor projects it. The geometry grows by a bounded number of
//! primitives per lesson, plus the shell's fixed share and a bounded number per area (a honeycomb's
//! cells: `HEX_ROOM`): O(lessons).

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::suboptimal_flops,
    reason = "f32 world geometry: counts and grid indices to f32, positive extents to grid sides; f32::mul_add is a libm call on wasm32 without FMA"
)]

use std::f32::consts::{PI, TAU};

use crate::paint::shade_level;
use crate::scene::{CORE, Tone, face_axis, fnv, ix};

/// A point or a direction in world space.
pub type V3 = [f32; 3];

/// Display: the world light the areas' solids are shaded by (`lit`), from above and a little to the
/// side, unit length. Fixed: the camera turns, the light stays with the cube.
const LIGHT: V3 = [0.350_88, 0.902_26, 0.250_63];
/// Display: the panels along an edge of a shell face, and a structural rib every `RIB_EVERY` of them.
const PANELS: usize = 16;
const RIB_EVERY: usize = 4;
/// Display: a regeneration alcove's opening, wide and high, world units (the core's half edge is 1),
/// before an area too full for them shrinks them (`fit`).
const ALCOVE_W: f32 = 0.05;
const ALCOVE_H: f32 = 0.1;
/// Display: a hex cell's radius at least and at most, world units: as large as leaves about three cells
/// a lesson on the area's walls, within these, unless the walls would then hold more than
/// `HEX_PER_LESSON` cells a lesson and `HEX_ROOM` more: an area that large for its few lessons gets
/// cells past `HEX_MAX`, so its cells grow with its lessons, not with its walls. A store's areas keep
/// `HEX_MAX` up to some 250 cells on their walls, past 12 a lesson only in a store of few lessons.
const HEX_MIN: f32 = 0.014;
const HEX_MAX: f32 = 0.04;
const HEX_PER_LESSON: usize = 12;
const HEX_ROOM: usize = 256;
/// Display: how much nearer than the surface it sits on a lesson's light sorts, world units: about five
/// depth buckets (scene.rs `DEPTHS`), so a light is drawn after its own frame.
pub const LIGHT_BIAS: f32 = 0.004;
/// Derived: the glow an alcove's slit spills (`Look::Light`): its own and its back's, whose light is
/// 0.55 of it and spills none, as the two glows one over the other did.
const ALCOVE_SPILL: f32 = 1.55;
/// Derived: the glow a node's side spills, of the two opposite ones that do: the sides of a node a
/// turning camera sees on average (4 / π), which spilled one glow each.
const NODE_SPILL: f32 = 4.0 / PI;
/// Guard: the smallest scale `fit` shrinks an area's lights to: far below a pixel, so every lesson of
/// any store finds a seat long before it.
const MIN_SCALE: f32 = 0.01;
/// Derived: the Borg kinds an area is built as (`Kind::ALL`).
pub const KINDS: usize = 8;
/// Guard: the bit that marks a point only fine detail uses while the interior is built (`Build::pt`):
/// the points of any store number far fewer.
const FINE_PT: u32 = 1 << 31;

/// How an area is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Tiers of regeneration alcoves on its four faces, a narrow ledge in front of each tier.
    Alcoves,
    /// Tiers of alcoves ringed round a glowing column.
    Ring,
    /// Two walls of alcoves round a lit shaft, bridges across it.
    Gallery,
    /// Bundles of vertical tubes, lessons lit nodes clamped on them.
    Spire,
    /// Walls of hex cells, lessons lit cells.
    Honeycomb,
    /// Maturation chambers: cylinders with glowing slit windows.
    Chambers,
    /// A vinculum: a column with light panels round it.
    Vinculum,
    /// A wall of fans on two faces, lessons status lights.
    Vents,
}

impl Kind {
    pub const ALL: [Self; KINDS] = [Self::Alcoves, Self::Ring, Self::Gallery, Self::Spire, Self::Honeycomb, Self::Chambers, Self::Vinculum, Self::Vents];

    /// Whether a patch of Borg skin covers part of one of its faces.
    const fn skinned(self) -> bool {
        matches!(self, Self::Spire | Self::Chambers | Self::Vents)
    }
}

/// The kind of each area of `keys`: handed out in turn in the order of the keys' hashes, from the kind
/// the first hash picks, so the areas of one view differ (all of them while there are at most `KINDS`)
/// and an area keeps its kind while the others stay. O(areas · log areas).
#[must_use]
pub fn kinds(keys: &[&str]) -> Vec<Kind> {
    let mut by: Vec<(u64, usize)> = keys.iter().enumerate().map(|(i, k)| (fnv(k), i)).collect();
    by.sort_unstable();
    let first = by.first().map_or(0, |b| (b.0 % KINDS as u64) as usize);
    let mut out = vec![Kind::Alcoves; keys.len()];
    for (rank, (_, i)) in by.iter().enumerate() {
        out[*i] = Kind::ALL[(first + rank) % KINDS];
    }
    out
}

/// A deterministic xorshift generator: the same seed, the same interior.
pub struct Rng(u64);

impl Rng {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9e37_79b9_7f4a_7c15 } else { seed })
    }

    /// The next number in 0..1.
    pub fn f(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// What a face shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Look {
    /// Opaque plating, at its shade level (paint.rs `shade_level`).
    Plate(u8),
    /// A see-through deck grating, at its shade level.
    Grating(u8),
    /// A lesson's light, by the lesson's index, `dim` of its full light (an alcove's back is dimmer
    /// than its slit), and how much of a glow spills round it (paint.rs `SPILL`), 0 for none. Of each
    /// lesson's faces a camera sees at most one spills, so a lesson costs one glow: an alcove's slit
    /// spills for its back too (`ALCOVE_SPILL`), one pair of a node's opposite sides for all four
    /// (`NODE_SPILL`).
    Light { lesson: u32, dim: f32, spill: f32 },
}

impl Look {
    /// Plating at `shade`: 0 dark, 1 fully lit.
    #[must_use]
    pub fn plate(shade: f32) -> Self {
        Self::Plate(shade_level(shade))
    }

    /// A grating at `shade`.
    #[must_use]
    pub fn grating(shade: f32) -> Self {
        Self::Grating(shade_level(shade))
    }
}

/// Where a face or a line sorts among the interior's primitives (scene.rs `gather`): at its own depth
/// (a face's outline's mean, a line's middle) or at point `at`'s, `bias` nearer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pin {
    pub at: Option<u32>,
    pub bias: f32,
}

/// When a primitive shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Always,
    /// When the face's normal looks at the camera.
    Cull,
    /// Part of shell face `face` (numbered as scene.rs `face_axis` does): shown when that face looks at
    /// the camera if `near`, when it looks away if `far`. On its outer surface (`surface`: a rib), not
    /// on a panel recessed in from it: from inside the cube, where the face looks away, behind the
    /// panels; from outside over them (scene.rs `Sight::key`).
    Shell {
        face: u8,
        near: bool,
        far: bool,
        surface: bool,
    },
}

impl Gate {
    /// Whether it shows: `normal` (a face's) against the camera's depth axis `view`, `facing` which
    /// shell faces look at the camera. O(1).
    #[must_use]
    pub fn shows(self, normal: V3, view: V3, facing: &[bool; 6]) -> bool {
        match self {
            Self::Always => true,
            Self::Cull => dot(normal, view) > 0.0,
            Self::Shell { face, near, far, .. } => {
                if facing[usize::from(face) % 6] {
                    near
                } else {
                    far
                }
            }
        }
    }

    /// Whether it lies on a shell face that looks away (`facing` as for `shows`): a line there takes
    /// its `alt` style.
    #[must_use]
    pub fn away(self, facing: &[bool; 6]) -> bool {
        matches!(self, Self::Shell { face, .. } if !facing[usize::from(face) % 6])
    }
}

/// How a line is drawn: dark hull backings of a width, green seams, rails and rims, bright glints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stroke {
    Hull3,
    Hull4,
    Hull5,
    Hull6,
    Hull8,
    /// The green sheen along a tube.
    Sheen,
    Seam,
    Rail,
    Rib,
    RibFar,
    /// A corridor's lit strip.
    Strip,
    Ring,
    /// A bright highlight on a lit column.
    Glint,
    Hex,
    Fan,
    Rim,
    /// A rib across a corridor.
    Cross,
}

impl Stroke {
    /// Its tone, opacity and width in pixels. A hull backing is opaque and never fogged.
    #[must_use]
    pub const fn look(self) -> (Tone, f32, f32) {
        match self {
            Self::Hull3 => (Tone::Hull, 1.0, 3.0),
            Self::Hull4 => (Tone::Hull, 1.0, 4.0),
            Self::Hull5 => (Tone::Hull, 1.0, 5.0),
            Self::Hull6 => (Tone::Hull, 1.0, 6.0),
            Self::Hull8 => (Tone::Hull, 1.0, 8.0),
            Self::Sheen => (Tone::Green, 0.35, 1.0),
            Self::Seam => (Tone::Green, 0.5, 1.0),
            Self::Rail => (Tone::Green, 0.25, 1.0),
            Self::Rib => (Tone::Green, 0.55, 1.2),
            Self::RibFar => (Tone::Green, 0.25, 1.2),
            Self::Strip => (Tone::Green, 0.75, 1.4),
            Self::Ring => (Tone::Green, 0.5, 1.2),
            Self::Glint => (Tone::Bright, 0.35, 1.5),
            Self::Hex => (Tone::Green, 0.3, 1.0),
            Self::Fan => (Tone::Green, 0.7, 1.6),
            Self::Rim => (Tone::Green, 0.6, 2.0),
            Self::Cross => (Tone::Green, 0.4, 1.0),
        }
    }

    /// The hull backing nearest `width` pixels.
    fn hull(width: f32) -> Self {
        match width {
            w if w < 3.5 => Self::Hull3,
            w if w < 4.5 => Self::Hull4,
            w if w < 5.5 => Self::Hull5,
            w if w < 7.0 => Self::Hull6,
            _ => Self::Hull8,
        }
    }
}

/// A flat face: its outline `corners[first..first + n]`, then `hole` corners more outlining a hole in it,
/// wound the other way so a fill leaves it open.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Face {
    pub first: u32,
    pub n: u8,
    pub hole: u8,
    pub look: Look,
    /// Its outward normal, for `Gate::Cull`.
    pub normal: V3,
    pub gate: Gate,
    /// Fine detail, left out where the core is small on screen.
    pub fine: bool,
    pub pin: Pin,
}

/// A line between two points: `style`, or `alt` on a shell face that looks away.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line {
    pub a: u32,
    pub b: u32,
    pub style: Stroke,
    pub alt: Stroke,
    pub gate: Gate,
    pub fine: bool,
    pub pin: Pin,
}

/// A soft light at a point: its tone, opacity, world radius, and how much nearer than the point it
/// sorts (a lamp in front of what it lights, a haze behind).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glow {
    pub at: u32,
    pub tone: Tone,
    pub alpha: f32,
    pub r: f32,
    pub bias: f32,
    pub gate: Gate,
}

/// An area's name over its top: the point, the text, and the area's width in world units (a label
/// shows where that is wide enough on screen).
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub at: u32,
    pub title: String,
    pub span: f32,
}

/// Where a lesson's light is: its centre, its half size, and the way it faces (zero: every way).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Seat {
    pub at: V3,
    pub r: f32,
    pub normal: V3,
}

impl Seat {
    /// Whether the light looks at a camera whose depth axis is `view` (one that faces every way does).
    #[must_use]
    pub fn looks(&self, view: V3) -> bool {
        dot(self.normal, view) >= 0.0
    }
}

/// An area to build: its box, the key its kind and its random layout come from, its label, and its
/// lessons in the order they take their lights.
#[derive(Clone, Debug, PartialEq)]
pub struct Room {
    pub min: V3,
    pub max: V3,
    pub key: String,
    pub title: String,
    pub lessons: Vec<u32>,
}

/// A shaft a cut of the core left: across `axis` from `lo` to `hi`, through the box it cut.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shaft {
    pub axis: usize,
    pub lo: f32,
    pub hi: f32,
    pub min: V3,
    pub max: V3,
}

/// The built interior, in world space.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Interior {
    /// The points: the first `coarse` are those the primitives that are not fine detail use, the rest
    /// only fine detail's.
    pub points: Vec<V3>,
    pub coarse: usize,
    /// The faces' corners, as indices into `points`.
    pub corners: Vec<u32>,
    pub faces: Vec<Face>,
    pub lines: Vec<Line>,
    pub glows: Vec<Glow>,
    pub labels: Vec<Label>,
    /// Each lesson's light, by its index.
    pub seats: Vec<Seat>,
}

impl Interior {
    /// Builds the shell, each room as its kind with its lessons' lights and its label, the risers, and a
    /// corridor in each shaft. O(lessons + rooms · log rooms + shafts).
    #[must_use]
    pub fn build(rooms: &[Room], shafts: &[Shaft], lessons: usize) -> Self {
        let mut b = Build::new(lessons);
        let keys: Vec<&str> = rooms.iter().map(|r| r.key.as_str()).collect();
        for (room, kind) in rooms.iter().zip(kinds(&keys)) {
            b.room(room, kind);
        }
        b.risers(rooms);
        for s in shafts {
            b.corridor(s);
        }
        b.shell();
        b.finish()
    }

    /// The faces, lines, glows and labels: what a frame sorts.
    #[must_use]
    pub fn prims(&self) -> usize {
        self.faces.len() + self.lines.len() + self.glows.len() + self.labels.len()
    }
}

/// The dot product.
#[must_use]
pub fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn add(a: V3, b: V3, k: f32) -> V3 {
    [a[0] + b[0] * k, a[1] + b[1] * k, a[2] + b[2] * k]
}

/// How much of the world light a face of `normal` takes.
fn lit(normal: V3) -> f32 {
    0.08 + 0.92 * dot(normal, LIGHT).max(0.0).powf(1.6)
}

/// The largest scale of 1, 0.9, 0.81 … at which `cap` seats `n`, at least `MIN_SCALE`.
fn fit(n: usize, cap: impl Fn(f32) -> usize) -> f32 {
    let mut s = 1.0;
    while cap(s) < n && s > MIN_SCALE {
        s *= 0.9;
    }
    s
}

/// A room's box.
#[derive(Clone, Copy)]
struct Area {
    min: V3,
    max: V3,
}

impl Area {
    fn w(&self) -> f32 {
        self.max[0] - self.min[0]
    }

    fn h(&self) -> f32 {
        self.max[1] - self.min[1]
    }

    fn d(&self) -> f32 {
        self.max[2] - self.min[2]
    }

    fn mid(&self) -> V3 {
        [0, 1, 2].map(|a| (self.min[a] + self.max[a]) * 0.5)
    }

    /// The length of the wall facing along x (`nx` not 0) or along z.
    fn wall(&self, nx: f32) -> f32 {
        if nx == 0.0 { self.w() } else { self.d() }
    }

    /// The centre of the wall facing `(nx, nz)`, `off` along it, at height `y`.
    fn on_wall(&self, (nx, nz): (f32, f32), off: f32, y: f32) -> V3 {
        let m = self.mid();
        if nx == 0.0 { [m[0] + off, y, m[2] + nz * self.d() * 0.5] } else { [m[0] + nx * self.w() * 0.5, y, m[2] + off] }
    }

    /// The `k`th of `count` slots of a grid over its floor, as near square as `count` lets: where a
    /// chamber or a spire stands, so none stands in another.
    fn slot(&self, count: usize, k: usize) -> Self {
        let cols = ((count as f32 * self.w() / self.d()).sqrt().ceil() as usize).clamp(1, count.max(1));
        let (sw, sd) = (self.w() / cols as f32, self.d() / count.div_ceil(cols).max(1) as f32);
        let min = [self.min[0] + sw * (k % cols) as f32, self.min[1], self.min[2] + sd * (k / cols) as f32];
        Self { min, max: [min[0] + sw, self.max[1], min[2] + sd] }
    }
}

/// The four walls' outward directions (x, z).
const SIDES: [(f32, f32); 4] = [(0.0, -1.0), (1.0, 0.0), (0.0, 1.0), (-1.0, 0.0)];

/// The tiers of alcoves `scale` high, with `gap` between, that fit in `h`: none in a bay too flat for
/// one at that scale, so `fit` shrinks them.
fn tiers(h: f32, scale: f32, gap: f32) -> usize {
    (h / ((ALCOVE_H + gap) * scale)) as usize
}

/// The alcoves `scale` wide, with `gap` between, that fit along a wall of `len`.
fn per_wall(len: f32, scale: f32, gap: f32) -> usize {
    (((len - 0.04 * scale) / ((ALCOVE_W + gap) * scale)) as usize).max(1)
}

/// The hex cells of radius `r` on a wall of `len` and height `h`: each one's offset along the wall
/// from its middle, and its height over the floor; rows offset by half a cell, 0.01 clear of the wall's
/// edges (a quarter of a side under 0.04, so a wall of any size has room for cells small enough).
fn hex_cells(len: f32, h: f32, r: f32) -> impl Iterator<Item = (f32, f32)> {
    let (dx, dy) = (r * 1.75, r * 1.52);
    let (mx, my) = (0.01f32.min(0.25 * len), 0.01f32.min(0.25 * h));
    let cols = ((len - 2.0 * mx) / dx).max(0.0) as usize;
    let rows = ((h - 2.0 * my) / dy).max(0.0) as usize;
    (0..rows).flat_map(move |row| {
        let shift = (row % 2) as f32 * dx * 0.5;
        (0..cols).map(move |c| (-len * 0.5 + mx + r + c as f32 * dx + shift, my + r + row as f32 * dy)).filter(move |(off, _)| *off <= len * 0.5 - r)
    })
}

/// The hex cells of radius `r` on the four walls of `a`.
fn hex_total(a: &Area, r: f32) -> usize {
    SIDES.iter().map(|&(nx, _)| hex_cells(a.wall(nx), a.h(), r).count()).sum()
}

/// Whether the `j`th of `total` cells takes one of `n` lessons: exactly `n` of them do, spread evenly.
const fn takes(j: usize, n: usize, total: usize) -> bool {
    total > 0 && (j + 1) * n / total > j * n / total
}

/// The builder: the interior so far, the points only fine detail uses so far (`FINE_PT`), whether what
/// it adds now is fine detail, and where it sorts.
struct Build {
    it: Interior,
    fine_pts: Vec<V3>,
    fine: bool,
    pin: Pin,
}

impl Build {
    fn new(lessons: usize) -> Self {
        Self { it: Interior { seats: vec![Seat::default(); lessons], ..Interior::default() }, fine_pts: Vec::new(), fine: false, pin: Pin::default() }
    }

    /// The interior, the points only fine detail uses put after the others and every index to them
    /// moved there. O(points + corners + primitives).
    fn finish(mut self) -> Interior {
        let coarse = self.it.points.len();
        let at = |i: u32| if i & FINE_PT == 0 { i } else { ix(coarse + (i & !FINE_PT) as usize) };
        self.it.points.append(&mut self.fine_pts);
        self.it.coarse = coarse;
        for c in &mut self.it.corners {
            *c = at(*c);
        }
        for l in &mut self.it.lines {
            (l.a, l.b, l.pin.at) = (at(l.a), at(l.b), l.pin.at.map(at));
        }
        for f in &mut self.it.faces {
            f.pin.at = f.pin.at.map(at);
        }
        self.it
    }

    /// A new point: among the fine detail's while that is being added.
    fn pt(&mut self, p: V3) -> u32 {
        if self.fine {
            self.fine_pts.push(p);
            ix(self.fine_pts.len() - 1) | FINE_PT
        } else {
            self.coarse_pt(p)
        }
    }

    /// A new point that what is not fine detail uses: a glow's, a label's, what a primitive sorts by.
    fn coarse_pt(&mut self, p: V3) -> u32 {
        self.it.points.push(p);
        ix(self.it.points.len() - 1)
    }

    /// What `f` adds is fine detail.
    fn detail(&mut self, f: impl FnOnce(&mut Self)) {
        let was = std::mem::replace(&mut self.fine, true);
        f(self);
        self.fine = was;
    }

    /// The faces and lines `f` adds sort by `pin`.
    fn pinned(&mut self, pin: Pin, f: impl FnOnce(&mut Self)) {
        let was = std::mem::replace(&mut self.pin, pin);
        f(self);
        self.pin = was;
    }

    /// A face over points already added: `outer`, and `hole` outlining a hole in it.
    fn face_ix(&mut self, outer: &[u32], hole: &[u32], normal: V3, look: Look, gate: Gate) {
        let first = ix(self.it.corners.len());
        self.it.corners.extend_from_slice(outer);
        self.it.corners.extend_from_slice(hole);
        let (n, hole) = (u8::try_from(outer.len()).unwrap_or(u8::MAX), u8::try_from(hole.len()).unwrap_or(u8::MAX));
        self.it.faces.push(Face { first, n, hole, look, normal, gate, fine: self.fine, pin: self.pin });
    }

    /// A face over new points; returns where its corners start in `corners`.
    fn poly(&mut self, pts: &[V3], normal: V3, look: Look, gate: Gate) -> usize {
        let first = self.it.corners.len();
        for p in pts {
            let i = self.pt(*p);
            self.it.corners.push(i);
        }
        let n = u8::try_from(pts.len()).unwrap_or(u8::MAX);
        self.it.faces.push(Face { first: ix(first), n, hole: 0, look, normal, gate, fine: self.fine, pin: self.pin });
        first
    }

    /// Plating facing `normal`, `base` bright where the world light falls full on it.
    fn plate(&mut self, pts: &[V3], normal: V3, base: f32) -> usize {
        self.poly(pts, normal, Look::plate(base * lit(normal)), Gate::Cull)
    }

    /// A lesson's light on a surface facing `normal`, `dim` of its full light, `spill` of a glow
    /// round it: it sorts `LIGHT_BIAS` nearer than its surface.
    fn light(&mut self, pts: &[V3], normal: V3, lesson: u32, (dim, spill): (f32, f32)) -> usize {
        let pin = Pin { bias: LIGHT_BIAS, ..self.pin };
        let mut first = 0;
        self.pinned(pin, |b| first = b.poly(pts, normal, Look::Light { lesson, dim, spill }, Gate::Cull));
        first
    }

    fn seat(&mut self, lesson: u32, at: V3, r: f32, normal: V3) {
        if let Some(s) = self.it.seats.get_mut(lesson as usize) {
            *s = Seat { at, r, normal };
        }
    }

    /// An axis-aligned box of plating, its six faces over its eight corners.
    fn cube(&mut self, min: V3, max: V3, base: f32) {
        let c: [u32; 8] = std::array::from_fn(|i| {
            let p = [0, 1, 2].map(|a| if i >> a & 1 == 1 { max[a] } else { min[a] });
            self.pt(p)
        });
        const SIDES: [([usize; 4], V3); 6] = [
            ([0, 2, 6, 4], [-1.0, 0.0, 0.0]),
            ([1, 3, 7, 5], [1.0, 0.0, 0.0]),
            ([0, 1, 5, 4], [0.0, -1.0, 0.0]),
            ([2, 3, 7, 6], [0.0, 1.0, 0.0]),
            ([0, 1, 3, 2], [0.0, 0.0, -1.0]),
            ([4, 5, 7, 6], [0.0, 0.0, 1.0]),
        ];
        for (q, n) in SIDES {
            self.face_ix(&q.map(|k| c[k]), &[], n, Look::plate(base * lit(n)), Gate::Cull);
        }
    }

    fn line(&mut self, a: V3, b: V3, style: Stroke) {
        self.line_gated(a, b, (style, style), Gate::Always);
    }

    fn line_gated(&mut self, a: V3, b: V3, styles: (Stroke, Stroke), gate: Gate) {
        let (a, b) = (self.pt(a), self.pt(b));
        self.line_ix(a, b, styles, gate);
    }

    /// A line between points already added.
    fn line_ix(&mut self, a: u32, b: u32, (style, alt): (Stroke, Stroke), gate: Gate) {
        self.it.lines.push(Line { a, b, style, alt, gate, fine: self.fine, pin: self.pin });
    }

    /// Lines round the closed loop of points already added `ring`, all of `style`.
    fn loop_ix(&mut self, ring: &[u32], style: Stroke) {
        for (k, &a) in ring.iter().enumerate() {
            let b = ring[(k + 1) % ring.len()];
            self.line_ix(a, b, (style, style), Gate::Always);
        }
    }

    /// A tube: a dark backing `width` pixels wide, a green sheen along it.
    fn tube(&mut self, a: V3, b: V3, width: f32) {
        self.line(a, b, Stroke::hull(width));
        let off = [0.003, 0.0, 0.003];
        self.line(add(a, off, 1.0), add(b, off, 1.0), Stroke::Sheen);
    }

    fn glow(&mut self, at: V3, (tone, alpha): (Tone, f32), r: f32, bias: f32) {
        self.glow_gated(at, (tone, alpha), r, bias, Gate::Always);
    }

    fn glow_gated(&mut self, at: V3, (tone, alpha): (Tone, f32), r: f32, bias: f32, gate: Gate) {
        let at = self.coarse_pt(at);
        self.it.glows.push(Glow { at, tone, alpha, r, bias, gate });
    }

    fn label(&mut self, at: V3, title: &str, span: f32) {
        let at = self.coarse_pt(at);
        self.it.labels.push(Label { at, title: title.to_owned(), span });
    }

    /// A lamp: a bright core and a green spill below it.
    fn lamp(&mut self, p: V3, big: bool) {
        let k = if big { 1.0 } else { 0.65 };
        self.glow(p, (Tone::Bright, 1.0), 0.04 * k, 0.01);
        self.glow(p, (Tone::Bright, 0.9), 0.01, 0.011);
        self.glow(p, (Tone::Green, 0.35 * k), 0.24 * k, -0.02);
    }

    /// A vertical prism of `seg` sides round `(cx, cz)` from `y0` to `y1`, its top capped, a glint
    /// down each side the world light falls on.
    fn cylinder(&mut self, (cx, cz, r): (f32, f32, f32), (y0, y1): (f32, f32), base: f32, seg: usize) {
        let at = |k: usize, y: f32| {
            let a = k as f32 / seg as f32 * TAU;
            [cx + a.cos() * r, y, cz + a.sin() * r]
        };
        for k in 0..seg {
            let am = (k as f32 + 0.5) / seg as f32 * TAU;
            let n = [am.cos(), 0.0, am.sin()];
            self.plate(&[at(k, y0), at(k + 1, y0), at(k + 1, y1), at(k, y1)], n, base);
            if dot(n, LIGHT) > 0.3 {
                let p = |y: f32| [cx + n[0] * r * 1.005, y, cz + n[2] * r * 1.005];
                self.detail(|b| b.line(p(y0 + 0.01), p(y1 - 0.01), Stroke::Glint));
            }
        }
        let cap: Vec<V3> = (0..seg).map(|k| at(k, y1)).collect();
        self.plate(&cap, [0.0, 1.0, 0.0], base * 1.1);
    }

    /// A horizontal ring of lines round `(cx, cz)` at `y`.
    fn ring_line(&mut self, (cx, cz, r): (f32, f32, f32), y: f32, style: Stroke) {
        const SEG: usize = 14;
        let ring: [u32; SEG] = std::array::from_fn(|k| {
            let a = k as f32 / SEG as f32 * TAU;
            self.pt([cx + a.cos() * r, y, cz + a.sin() * r])
        });
        self.loop_ix(&ring, style);
    }

    /// A regeneration alcove standing at `y` on the floor point `c`, opening toward `n` (horizontal),
    /// its opening `w` by `h`: a dark frame round the opening, the recess's walls, its back glowing in
    /// the lesson's light with a bright slit, tubes over it. The lesson's seat is the slit.
    fn alcove(&mut self, c: V3, y: f32, n: V3, (w, h): (f32, f32), lesson: u32) {
        let u = [-n[2], 0.0, n[0]];
        // Its depth, and the step its frame stands proud and its lights stand off the back, by its size.
        let (dep, off) = (0.22 * h, 0.02 * h);
        let at = |a: f32, b: f32, o: f32| [c[0] + u[0] * a + n[0] * o, y + b, c[2] + u[2] * a + n[2] * o];
        let (hw, rim, cap) = (w * 0.5, 0.12 * w, 0.14 * h);
        self.detail(|b| {
            let outer = [at(-hw - rim, -rim, 2.0 * off), at(hw + rim, -rim, 2.0 * off), at(hw + rim, h + cap, 2.0 * off), at(-hw - rim, h + cap, 2.0 * off)];
            let front = [at(-hw, 0.0, 0.0), at(hw, 0.0, 0.0), at(hw, h, 0.0), at(-hw, h, 0.0)];
            let back = [at(-hw, 0.0, -dep), at(hw, 0.0, -dep), at(hw, h, -dep), at(-hw, h, -dep)];
            let (o, f, k) = (outer.map(|p| b.pt(p)), front.map(|p| b.pt(p)), back.map(|p| b.pt(p)));
            b.face_ix(&o, &[f[0], f[3], f[2], f[1]], n, Look::plate(0.7 * lit(n)), Gate::Cull);
            // The recess: its walls face into the opening.
            for (q, nn) in [
                ([f[0], k[0], k[3], f[3]], u),
                ([f[1], k[1], k[2], f[2]], u.map(|x| -x)),
                ([f[3], k[3], k[2], f[2]], [0.0, -1.0, 0.0]),
                ([f[0], k[0], k[1], f[1]], [0.0, 1.0, 0.0]),
            ] {
                b.face_ix(&q, &[], nn, Look::plate(0.5 * lit(nn)), Gate::Cull);
            }
            let top = at(0.0, h + cap, -dep * 0.5);
            b.tube(top, [top[0], top[1] + 0.3 * h, top[2]], 3.0);
        });
        let o = -dep + off;
        self.light(&[at(-hw, 2.0 * off, o), at(hw, 2.0 * off, o), at(hw, h - 2.0 * off, o), at(-hw, h - 2.0 * off, o)], n, lesson, (0.55, 0.0));
        let (sw, o) = (0.12 * w, -dep + 2.0 * off);
        self.light(&[at(-sw, 0.25 * h, o), at(sw, 0.25 * h, o), at(sw, 0.92 * h, o), at(-sw, 0.92 * h, o)], n, lesson, (1.0, ALCOVE_SPILL));
        self.seat(lesson, at(0.0, 0.585 * h, o), 0.34 * h, n);
    }

    /// A narrow catwalk in front of a tier, from `a` to `b`, sticking out toward `n`, a rail on it.
    fn ledge(&mut self, a: V3, b: V3, n: V3) {
        let (a2, b2) = (add(a, n, 0.03), add(b, n, 0.03));
        let first = self.poly(&[a, b, b2, a2], [0.0, 1.0, 0.0], Look::grating(0.3), Gate::Always);
        let (ia2, ib2) = (self.it.corners[first + 3], self.it.corners[first + 2]);
        self.detail(|k| {
            k.line_ix(ia2, ib2, (Stroke::Rib, Stroke::Rib), Gate::Always);
            k.line(add(a2, [0.0, 0.04, 0.0], 1.0), add(b2, [0.0, 0.04, 0.0], 1.0), Stroke::Rail);
        });
    }

    /// A fan on a wall facing `n` (along z): its housing, its rim, its blades turned at random, the
    /// green light behind it. The rim and the blades sort at the fan's centre, just nearer than the
    /// housing, so they all lie over it however it turns: by their own points, those across its far
    /// side would sort behind the housing and be covered by it.
    fn fan(&mut self, c: V3, n: V3, r: f32, rng: &mut Rng) {
        let at = |a: f32, b: f32, o: f32| [c[0] + a, c[1] + b, c[2] + n[2] * o];
        let ring = |k: f32, s: f32, o: f32| at((k * TAU).cos() * s, (k * TAU).sin() * s, o);
        let housing: [V3; 8] = std::array::from_fn(|k| ring((k as f32 + 0.5) / 8.0, r * 1.18, 0.002));
        let centre = self.coarse_pt(at(0.0, 0.0, 0.002));
        self.poly(&housing, n, Look::plate(0.35), Gate::Cull);
        self.pinned(Pin { at: Some(centre), bias: 0.004 }, |b| {
            b.detail(|b| {
                let rim: [u32; 20] = std::array::from_fn(|k| b.pt(ring(k as f32 / 20.0, r, 0.004)));
                b.loop_ix(&rim, Stroke::Fan);
            });
            let (blades, spin) = (6 + (rng.f() * 3.0) as usize, rng.f());
            for k in 0..blades {
                let (a, a2) = (spin + k as f32 / blades as f32, spin + k as f32 / blades as f32 + 0.067);
                let q = [ring(a, r * 0.22, 0.005), ring(a, r * 0.93, 0.005), ring(a2, r * 0.93, 0.005), ring(a2, r * 0.26, 0.005)];
                b.poly(&q, n, Look::plate(1.0), Gate::Cull);
            }
        });
        self.glow(at(0.0, 0.0, -0.02), (Tone::Green, 0.4), r * 1.7, -0.01);
    }

    /// Frame posts just outside the area's four corners, so nothing in the area stands in one, tubes
    /// climbing most of them.
    fn posts(&mut self, a: &Area, rng: &mut Rng) {
        /// Display: a post's thickness, world units.
        const POST: f32 = 0.02;
        for (x, z, sx, sz) in
            [(a.min[0], a.min[2], -1.0, -1.0), (a.max[0], a.min[2], 1.0, -1.0), (a.min[0], a.max[2], -1.0, 1.0), (a.max[0], a.max[2], 1.0, 1.0)]
        {
            let (x1, z1) = (x + sx * POST, z + sz * POST);
            self.cube([x.min(x1), a.min[1], z.min(z1)], [x.max(x1), a.max[1], z.max(z1)], 0.6);
            if rng.f() < 0.7 {
                self.detail(|b| b.post_tubes(x, z, a, rng));
            }
        }
    }

    fn post_tubes(&mut self, x: f32, z: f32, a: &Area, rng: &mut Rng) {
        for _ in 0..2 + (rng.f() * 3.0) as usize {
            let (ox, oz) = ((rng.f() - 0.5) * 0.05, (rng.f() - 0.5) * 0.05);
            let (y0, y1) = (a.min[1] + a.h() * rng.f() * 0.3, a.max[1] - a.h() * rng.f() * 0.3);
            let width = 3.0 + (rng.f() * 4.0).floor();
            self.tube([x + ox, y0, z + oz], [x + ox, y1, z + oz], width);
        }
    }

    /// A patch of Borg skin on the face of `a` normal to `axis` (x or z), on its high side if `high`:
    /// about a third of its cells plated, some with a seam.
    fn skin(&mut self, a: &Area, (axis, high): (usize, bool), rng: &mut Rng) {
        let u = if axis == 0 { 2 } else { 0 };
        let at = if high { a.max[axis] } else { a.min[axis] };
        let mut n = [0.0; 3];
        n[axis] = if high { 1.0 } else { -1.0 };
        let (a0, a1, y0, y1) = (a.min[u], a.max[u], a.min[1], a.max[1]);
        let cols = ((a1 - a0) / 0.06).round().max(2.0) as usize;
        let rows = ((y1 - y0) / 0.06).round().max(2.0) as usize;
        let p = |along: f32, y: f32| {
            let mut q = [0.0, y, 0.0];
            q[axis] = at;
            q[u] = along;
            q
        };
        for c in 0..cols * rows {
            if rng.f() < 0.68 {
                continue;
            }
            let (i, j) = ((c % cols) as f32, (c / cols) as f32);
            let (l, r) = (a0 + (a1 - a0) * i / cols as f32, a0 + (a1 - a0) * (i + 1.0) / cols as f32);
            let (lo, hi) = (y0 + (y1 - y0) * j / rows as f32, y0 + (y1 - y0) * (j + 1.0) / rows as f32);
            self.plate(&[p(l + 0.002, lo + 0.002), p(r - 0.002, lo + 0.002), p(r - 0.002, hi - 0.002), p(l + 0.002, hi - 0.002)], n, 0.6 + 0.4 * rng.f());
            if rng.f() < 0.3 {
                self.detail(|b| b.line(p(l + 0.006, (lo + hi) * 0.5), p(r - 0.006, (lo + hi) * 0.5), Stroke::Seam));
            }
        }
    }

    /// Room `r` built as `kind`, its posts and its label.
    fn room(&mut self, r: &Room, kind: Kind) {
        let a = Area { min: r.min, max: r.max };
        let mut rng = Rng::new(fnv(&r.key));
        self.posts(&a, &mut rng);
        if kind.skinned() {
            let face = (if rng.f() < 0.5 { 0 } else { 2 }, rng.f() < 0.5);
            self.skin(&a, face, &mut rng);
        }
        let ls = &r.lessons;
        match kind {
            Kind::Alcoves => self.alcoves(&a, ls),
            Kind::Ring => self.ring(&a, ls, &mut rng),
            Kind::Gallery => self.gallery(&a, ls, &mut rng),
            Kind::Spire => self.spire(&a, ls, &mut rng),
            Kind::Honeycomb => self.honeycomb(&a, ls, &mut rng),
            Kind::Chambers => self.chambers(&a, ls, &mut rng),
            Kind::Vinculum => self.vinculum(&a, ls, &mut rng),
            Kind::Vents => self.vents(&a, ls, &mut rng),
        }
        let m = a.mid();
        self.label([m[0], a.max[1] + 0.03, m[2]], &r.title, a.w().max(a.d()));
    }

    /// Tiers of alcoves on the four walls, a ledge in front of each tier, a lamp in every other: as
    /// large as lets every lesson have one.
    fn alcoves(&mut self, a: &Area, ls: &[u32]) {
        let cap = |s: f32| tiers(a.h(), s, 0.035) * SIDES.iter().map(|&(nx, _)| per_wall(a.wall(nx), s, 0.016)).sum::<usize>();
        let s = fit(ls.len(), cap);
        let (w, h) = (ALCOVE_W * s, ALCOVE_H * s);
        let mut li = 0;
        for t in 0..tiers(a.h(), s, 0.035) {
            let y = a.min[1] + 0.012 * s + t as f32 * (h + 0.035 * s);
            for side in SIDES {
                li += self.alcove_wall(a, side, (y, s), (w, h), ls.get(li..).unwrap_or_default());
            }
            if t % 2 == 0 {
                let m = a.mid();
                self.lamp([m[0], y + h + 0.02 * s, m[2]], false);
            }
        }
    }

    /// A tier of alcoves along the wall facing `side` at height `y` (scale `s`), for the first lessons of
    /// `ls`, and its ledge; returns how many it seated.
    fn alcove_wall(&mut self, a: &Area, side: (f32, f32), (y, s): (f32, f32), size: (f32, f32), ls: &[u32]) -> usize {
        let len = a.wall(side.0);
        let k = per_wall(len, s, 0.016);
        let placed = k.min(ls.len());
        let n = [side.0, 0.0, side.1];
        for (j, &lesson) in ls.iter().take(placed).enumerate() {
            let off = -len * 0.5 + 0.02 * s + (j as f32 + 0.5) * (len - 0.04 * s) / k as f32;
            self.alcove(a.on_wall(side, off, 0.0), y, n, size, lesson);
        }
        if placed > 0 {
            let along = if side.0 == 0.0 { [1.0, 0.0, 0.0] } else { [0.0, 0.0, 1.0] };
            let c = a.on_wall(side, 0.0, y - 0.006 * s);
            self.ledge(add(c, along, -len * 0.5), add(c, along, len * 0.5), n);
        }
        placed
    }

    /// A glowing column up the middle of `a`, `rc` wide, bright rings up it.
    fn column(&mut self, a: &Area, rc: f32, rng: &mut Rng) {
        let m = a.mid();
        self.cylinder((m[0], m[2], rc), (a.min[1], a.max[1]), 1.0, 16);
        self.glow(m, (Tone::Green, 0.55), rc * 3.0, 0.02);
        let mut y = a.min[1] + 0.06;
        while y < a.max[1] {
            self.detail(|b| b.ring_line((m[0], m[2], rc * 1.08), y, Stroke::Glint));
            y += 0.06 + 0.06 * rng.f();
        }
    }

    /// Tiers of alcoves ringed round a glowing column, spokes from it to each tier: as high as lets a
    /// tier stand in the bay and its alcoves' backs (a fifth of their height in from `r0`) stay clear of
    /// the column's glints (1.08 `rc`), as narrow as lets a tier's alcoves go round.
    fn ring(&mut self, a: &Area, ls: &[u32], rng: &mut Rng) {
        let (m, side) = (a.mid(), a.w().min(a.d()));
        let (rc, r0) = (side * 0.16, side * 0.42);
        self.column(a, rc, rng);
        let s = fit(1, |s| tiers(a.h(), s, 0.04)).min(1.2 * side / ALCOVE_H);
        let (h, gap) = (ALCOVE_H * s, 0.04 * s);
        let per = ls.len().div_ceil(tiers(a.h(), s, 0.04).max(1)).max(1);
        for (t, tier) in ls.chunks(per).enumerate() {
            let y = a.min[1] + 0.012 * s + t as f32 * (h + gap);
            self.detail(|b| b.ring_line((m[0], m[2], r0 + 0.03), y - 0.006 * s, Stroke::Ring));
            let w = (ALCOVE_W * s).min(TAU * r0 / per as f32 * 0.7);
            for (j, &lesson) in tier.iter().enumerate() {
                let ang = j as f32 / per as f32 * TAU + t as f32 * 0.3;
                let n = [ang.cos(), 0.0, ang.sin()];
                self.alcove(add(m, n, r0), y, n, (w, h), lesson);
            }
            self.detail(|b| b.spokes((m[0], m[2]), (rc, r0), [y + h; 6]));
        }
    }

    /// Six tubes out from a column of radius `rc` to `r0`, at the heights `ys`.
    fn spokes(&mut self, (cx, cz): (f32, f32), (rc, r0): (f32, f32), ys: [f32; 6]) {
        for (k, y) in ys.into_iter().enumerate() {
            let (s, c) = (k as f32 / 6.0 * TAU).sin_cos();
            self.tube([cx + c * rc, y, cz + s * rc], [cx + c * r0, y, cz + s * r0], 3.0);
        }
    }

    /// A vinculum: a column, lessons light panels round it in levels 0.05 apart from 0.03 over the
    /// floor (nearer it, and the panels lower, in a bay too flat for one level), tubes out from it.
    fn vinculum(&mut self, a: &Area, ls: &[u32], rng: &mut Rng) {
        let (m, side) = (a.mid(), a.w().min(a.d()));
        let rc = side * 0.16;
        self.column(a, rc, rng);
        let (below, hh) = (0.03f32.min(0.5 * a.h()), 0.012f32.min(0.25 * a.h()));
        let levels = ((a.h() - below - hh) / 0.05).max(0.0) as usize + 1;
        let around = ls.len().div_ceil(levels).max(8);
        let half = 0.009f32.min(PI * rc / around as f32 * 0.8);
        for (j, &lesson) in ls.iter().enumerate() {
            let ang = (j % around) as f32 / around as f32 * TAU;
            let y = a.min[1] + below + (j / around) as f32 * 0.05;
            let n = [ang.cos(), 0.0, ang.sin()];
            let (p, u) = ([m[0] + n[0] * rc * 1.01, y, m[2] + n[2] * rc * 1.01], [-n[2], 0.0, n[0]]);
            self.light(
                &[
                    add(add(p, u, -half), [0.0, -hh, 0.0], 1.0),
                    add(add(p, u, half), [0.0, -hh, 0.0], 1.0),
                    add(add(p, u, half), [0.0, hh, 0.0], 1.0),
                    add(add(p, u, -half), [0.0, hh, 0.0], 1.0),
                ],
                n,
                lesson,
                (1.0, 1.0),
            );
            self.seat(lesson, p, hh, n);
        }
        let ys = [0; 6].map(|_| a.min[1] + a.h() * rng.f());
        self.detail(|b| b.spokes((m[0], m[2]), (rc, side * 0.42), ys));
    }

    /// Spires: bundles of five tubes, lessons lit nodes clamped up them, each lit on all four sides, a
    /// lamp on top; each in its own slot of the floor, so no spire's nodes stand in another's.
    fn spire(&mut self, a: &Area, ls: &[u32], rng: &mut Rng) {
        /// Display: the room a lit node (0.048 across) takes, world units.
        const NODE: f32 = 0.05;
        // Each in its own slot (`Area::slot`), at most as many as have room for a node.
        let want = ls.len().div_ceil(12).clamp(2, 7);
        let spires = (1..=want).rev().find(|&c| a.slot(c, 0).w().min(a.slot(c, 0).d()) >= NODE).unwrap_or(1);
        let per = ls.len().div_ceil(spires).max(1);
        for k in 0..spires {
            let sl = a.slot(spires, k);
            let jitter = |lo: f32, len: f32, u: f32| lo + 0.5 * len + (u - 0.5) * (len - NODE).max(0.0);
            let (x, z) = (jitter(sl.min[0], sl.w(), rng.f()), jitter(sl.min[2], sl.d(), rng.f()));
            let top = a.min[1] + a.h() * (0.6 + 0.4 * rng.f());
            let base = [x, a.min[1], z];
            for t in 0..5 {
                let (s, c) = (t as f32 / 5.0 * TAU).sin_cos();
                self.tube([base[0] + c * 0.016, a.min[1], base[2] + s * 0.016], [base[0] + c * 0.016, top, base[2] + s * 0.016], 5.0);
            }
            let nodes = ls.get(k * per..((k + 1) * per).min(ls.len())).unwrap_or_default();
            self.nodes(base, (a.min[1], top), per, nodes);
            self.lamp([base[0], top + 0.01, base[2]], true);
        }
    }

    /// The lit nodes of a spire standing on `base`, from `y0` to `top`, spaced for `per`: clear of the
    /// floor and the top by a share of the height in a flat bay, so each node has its own place and
    /// height there too.
    fn nodes(&mut self, base: V3, (y0, top): (f32, f32), per: usize, ls: &[u32]) {
        let span = (top - y0).max(0.0);
        let (below, above) = (0.03f32.min(0.3 * span), 0.02f32.min(0.2 * span));
        let step = (span - below - above) / per as f32;
        let hh = 0.008f32.min(0.35 * step);
        for (j, &lesson) in ls.iter().enumerate() {
            let y = y0 + below + step * (j as f32 + 0.5);
            let c = [base[0], y, base[2]];
            self.cube([c[0] - 0.024, y - hh, c[2] - 0.024], [c[0] + 0.024, y + hh, c[2] + 0.024], 0.85);
            // Of two opposite sides the camera sees one (but square on): those spill, for all four.
            for (nx, nz) in SIDES {
                let (f, u) = ([c[0] + nx * 0.0245, y, c[2] + nz * 0.0245], [-nz, 0.0, nx]);
                let lift = [0.0, hh * 0.5, 0.0];
                self.light(
                    &[
                        add(add(f, u, -0.016), lift, -1.0),
                        add(add(f, u, 0.016), lift, -1.0),
                        add(add(f, u, 0.016), lift, 1.0),
                        add(add(f, u, -0.016), lift, 1.0),
                    ],
                    [nx, 0.0, nz],
                    lesson,
                    (1.0, if nx == 0.0 { 0.0 } else { NODE_SPILL }),
                );
            }
            self.seat(lesson, c, 0.024, [0.0; 3]);
        }
    }

    /// Walls of hex cells on the four faces, exactly as many lit as there are lessons, spread evenly: at
    /// most `HEX_PER_LESSON` cells a lesson and `HEX_ROOM` more (a cell of radius r takes about 2.66 r²
    /// of wall, `hex_cells`).
    fn honeycomb(&mut self, a: &Area, ls: &[u32], rng: &mut Rng) {
        let (wall, n) = (2.0 * (a.w() + a.d()) * a.h(), ls.len().max(1));
        let sparse = (wall / (2.66 * (HEX_PER_LESSON * n + HEX_ROOM) as f32)).sqrt();
        let want = (wall / (3.0 * n as f32 * 2.6)).sqrt().clamp(HEX_MIN, HEX_MAX).max(sparse);
        let r = want * fit(ls.len(), |s| hex_total(a, want * s));
        let total = hex_total(a, r);
        let (mut j, mut li) = (0, 0);
        for side in [(0.0, 1.0), (0.0, -1.0), (1.0, 0.0), (-1.0, 0.0)] {
            for (off, y) in hex_cells(a.wall(side.0), a.h(), r) {
                let lesson = takes(j, ls.len(), total).then(|| ls.get(li).copied()).flatten();
                li += usize::from(lesson.is_some());
                self.cell(a.on_wall(side, off, a.min[1] + y), [side.0, 0.0, side.1], r, (lesson, rng));
                j += 1;
            }
        }
    }

    /// A hex cell of radius `r` centred on `c` on a wall facing `n`: a lesson's light, or plating; its
    /// outline runs over its corners.
    fn cell(&mut self, c: V3, n: V3, r: f32, (lesson, rng): (Option<u32>, &mut Rng)) {
        let u = [-n[2], 0.0, n[0]];
        let hex: [V3; 6] = std::array::from_fn(|k| {
            let (s, co) = (k as f32 / 6.0 * TAU + PI / 6.0).sin_cos();
            [c[0] + u[0] * co * r - n[0] * 0.002, c[1] + s * r, c[2] + u[2] * co * r - n[2] * 0.002]
        });
        let first = match lesson {
            Some(l) => {
                self.seat(l, add(c, n, -0.002), r * 0.8, n);
                self.light(&hex, n, l, (0.9, 1.0))
            }
            None => self.plate(&hex, n, 0.5 + 0.3 * rng.f()),
        };
        let corners: [u32; 6] = std::array::from_fn(|k| self.it.corners[first + k]);
        self.detail(|b| b.loop_ix(&corners, Stroke::Hex));
    }

    /// Maturation chambers: cylinders with rims, lessons glowing slits round them in levels, a tube up.
    /// Each stands somewhere in its own slot (`Area::slot`), rim and all, so none stands in another and
    /// buries its slits.
    fn chambers(&mut self, a: &Area, ls: &[u32], rng: &mut Rng) {
        let count = ((ls.len() as f32 / 12.0).round() as usize).clamp(1, 6);
        let per = ls.len().div_ceil(count).max(1);
        for (k, chunk) in ls.chunks(per).enumerate() {
            let sl = a.slot(count, k);
            let r = (a.w().min(a.d()) * (0.1 + 0.08 * rng.f())).min(0.45 * sl.w().min(sl.d()));
            let cx = sl.min[0] + 1.1 * r + (sl.w() - 2.2 * r) * rng.f();
            let cz = sl.min[2] + 1.1 * r + (sl.d() - 2.2 * r) * rng.f();
            let top = a.min[1] + a.h() * (0.5 + 0.5 * rng.f());
            self.cylinder((cx, cz, r), (a.min[1], top), 1.0, 14);
            self.ring_line((cx, cz, r * 1.1), top, Stroke::Rim);
            self.ring_line((cx, cz, r * 1.1), a.min[1] + 0.02, Stroke::Rim);
            self.slits((cx, cz, r), (a.min[1], top), chunk);
            self.tube([cx, top, cz], [cx, a.max[1], cz], 6.0);
        }
    }

    /// A chamber's slits round `(cx, cz)` at radius `r`, 0.04 tall in levels 0.06 apart from 0.03 over
    /// `y0` up to `top` (nearer `y0`, and shorter, in a chamber too low for one level).
    fn slits(&mut self, (cx, cz, r): (f32, f32, f32), (y0, top): (f32, f32), ls: &[u32]) {
        let span = top - y0;
        let (below, tall) = (0.03f32.min(0.25 * span), 0.04f32.min(0.5 * span));
        let levels = ((span - below - tall) / 0.06).max(0.0) as usize + 1;
        let per = ls.len().div_ceil(levels).max(6);
        let half = 0.004f32.min(PI * r / per as f32 * 0.6);
        for (j, &lesson) in ls.iter().enumerate() {
            let lvl = j / per;
            let ang = (j % per) as f32 / per as f32 * TAU + lvl as f32 * 0.25;
            let y = y0 + below + lvl as f32 * 0.06;
            let (n, u) = ([ang.cos(), 0.0, ang.sin()], [-ang.sin(), 0.0, ang.cos()]);
            let p = [cx + n[0] * r * 1.005, y, cz + n[2] * r * 1.005];
            let up = [0.0, tall, 0.0];
            self.light(&[add(p, u, -half), add(p, u, half), add(add(p, u, half), up, 1.0), add(add(p, u, -half), up, 1.0)], n, lesson, (1.0, 1.0));
            self.seat(lesson, add(p, up, 0.5), 0.02, n);
        }
    }

    /// A gallery: two walls of alcove tiers facing out from a lit shaft, a ledge along each tier,
    /// bridges across, tubes up the shaft.
    fn gallery(&mut self, a: &Area, ls: &[u32], rng: &mut Rng) {
        let per = ls.len().div_ceil(2);
        let s = fit(per, |s| tiers(a.h(), s, 0.03) * per_wall(a.w() + 0.01 * s, s, 0.012));
        let m = a.mid();
        self.glow(m, (Tone::Green, 0.45), a.w().max(a.h()) * 0.5, 0.0);
        let (left, right) = ls.split_at(per.min(ls.len()));
        for (side, wall) in [(-1.0, left), (1.0, right)] {
            let z = if side < 0.0 { a.min[2] + 0.02 } else { a.max[2] - 0.02 };
            self.gallery_wall(a, (z, side), s, wall, rng);
        }
        for _ in 0..3 {
            let x = a.min[0] + a.w() * rng.f();
            self.tube([x, a.min[1], m[2]], [x, a.max[1], m[2]], 7.0);
        }
    }

    /// One gallery wall at `z` facing `side` (along z), scale `s`, its lessons `ls`.
    fn gallery_wall(&mut self, a: &Area, (z, side): (f32, f32), s: f32, ls: &[u32], rng: &mut Rng) {
        let (w, h) = (ALCOVE_W * s, ALCOVE_H * s);
        let k = per_wall(a.w() + 0.01 * s, s, 0.012);
        for (t, tier) in ls.chunks(k).enumerate() {
            let y = a.min[1] + 0.012 * s + t as f32 * (h + 0.03 * s);
            for (j, &lesson) in tier.iter().enumerate() {
                let x = a.min[0] + 0.015 * s + (j as f32 + 0.5) * (a.w() - 0.03 * s) / k as f32;
                self.alcove([x, 0.0, z], y, [0.0, 0.0, side], (w, h), lesson);
            }
            self.ledge([a.min[0], y - 0.006, z], [a.max[0], y - 0.006, z], [0.0, 0.0, side]);
            if t % 2 == 1 {
                let x = a.min[0] + a.w() * rng.f();
                let (p0, p1) = ([x, y - 0.006, a.min[2] + 0.03], [x, y - 0.006, a.max[2] - 0.03]);
                self.line(p0, p1, Stroke::Hull5);
                self.line(add(p0, [0.0, 0.004, 0.0], 1.0), add(p1, [0.0, 0.004, 0.0], 1.0), Stroke::Seam);
            }
        }
    }

    /// Vents: fans on the two faces along z, lessons status lights in rows over them.
    fn vents(&mut self, a: &Area, ls: &[u32], rng: &mut Rng) {
        let fans = ((ls.len() as f32 / 9.0).round() as usize).clamp(2, 8);
        let cols = ((fans as f32 * a.w() / a.h()).sqrt().ceil() as usize).max(1);
        let rows = fans.div_ceil(cols);
        let r = (a.w() / cols as f32).min(a.h() / rows as f32) * 0.36;
        for f in 0..fans {
            let x = a.min[0] + ((f % cols) as f32 + 0.5) * a.w() / cols as f32;
            let y = a.min[1] + ((f / cols) as f32 + 0.5) * a.h() / rows as f32;
            self.fan([x, y, a.max[2] - 0.008], [0.0, 0.0, 1.0], r, rng);
            self.fan([x, y, a.min[2] + 0.008], [0.0, 0.0, -1.0], r * 0.85, rng);
        }
        let per = ls.len().div_ceil(2);
        let (front, back) = ls.split_at(per.min(ls.len()));
        self.statuses(a, (a.max[2] - 0.005, 1.0), per, front);
        self.statuses(a, (a.min[2] + 0.005, -1.0), per, back);
    }

    /// Status lights in rows from the top of the face at `z` facing `side`, spaced so `per` fit: the
    /// top row 0.02 under the top at full size, halfway down a face too low for that.
    fn statuses(&mut self, a: &Area, (z, side): (f32, f32), per: usize, ls: &[u32]) {
        let grid = |sp: f32| (((a.w() - 0.02) / sp).max(1.0) as usize, ((a.h() - 0.02) / sp).max(1.0) as usize);
        let sp = 0.03 * fit(per, |s| grid(0.03 * s).0 * grid(0.03 * s).1);
        let (cols, k) = (grid(sp).0, sp / 0.03);
        let below_top = (0.02 * k).min(0.5 * a.h());
        for (j, &lesson) in ls.iter().enumerate() {
            let x = a.min[0] + 0.015 * k + (j % cols) as f32 * sp;
            let y = a.max[1] - below_top - (j / cols) as f32 * sp;
            let (hx, hy) = (0.007 * k, 0.005 * k);
            self.light(&[[x - hx, y - hy, z], [x + hx, y - hy, z], [x + hx, y + hy, z], [x - hx, y + hy, z]], [0.0, 0.0, side], lesson, (1.0, 1.0));
            self.seat(lesson, [x, y, z], hx, [0.0, 0.0, side]);
        }
    }

    /// Risers up the shafts beside the rooms, a few with an elbow that stays within the outermost
    /// risers (`west` to `east`, never out through the shell), and the haze low in the core.
    fn risers(&mut self, rooms: &[Room]) {
        let mut rng = Rng::new(fnv("risers").wrapping_add(rooms.len() as u64));
        let lo = rooms.iter().map(|r| r.min[1]).fold(0.0f32, f32::min);
        let hi = rooms.iter().map(|r| r.max[1]).fold(0.0f32, f32::max);
        let (west, east) = rooms.iter().fold((0.0f32, 0.0f32), |(w, e), r| (w.min(r.min[0] - 0.03), e.max(r.max[0] + 0.03)));
        for _ in 0..(rooms.len() * 2).min(20) {
            let Some(r) = rooms.get((rng.f() * rooms.len() as f32) as usize) else { continue };
            let x = if rng.f() < 0.5 { r.min[0] - 0.03 } else { r.max[0] + 0.03 };
            let z = r.min[2] + (r.max[2] - r.min[2]) * rng.f();
            let width = 4.0 + (rng.f() * 6.0).floor();
            self.tube([x, lo, z], [x, hi * (0.3 + 0.7 * rng.f()), z], width);
            if rng.f() < 0.3 {
                let y = lo + (hi - lo) * rng.f();
                self.tube([x, y, z], [(x + (rng.f() - 0.5) * 0.4).clamp(west, east), y, z], width * 0.8);
            }
        }
        for k in 0..7 {
            let p = [-0.6 + 1.2 * k as f32 / 6.0, -0.5, 0.05 * (k % 3) as f32];
            self.glow(p, (Tone::Green, 0.18), 0.55, -0.5);
        }
    }

    /// A corridor in a shaft across x or z: walkways at several heights with a lit strip and rails,
    /// ribbed frames across it.
    fn corridor(&mut self, s: &Shaft) {
        let gap = s.hi - s.lo;
        if s.axis == 1 || !(0.04..=0.5).contains(&gap) {
            return;
        }
        let o = if s.axis == 0 { 2 } else { 0 };
        let (lo, hi) = ((s.min[o], s.min[1]), (s.max[o], s.max[1]));
        if hi.0 - lo.0 < 0.1 || hi.1 - lo.1 < 0.1 {
            return;
        }
        let mid = (s.lo + s.hi) * 0.5;
        let at = |along: f32, y: f32, across: f32| {
            let mut p = [0.0, y, 0.0];
            p[o] = along;
            p[s.axis] = mid + across;
            p
        };
        let mut rng = Rng::new(fnv("corridor") ^ (mid.to_bits() as u64) << 32 ^ u64::from(lo.0.to_bits()));
        let mut y = lo.1 + 0.03;
        while y < hi.1 - 0.03 {
            self.walkway(&at, (lo.0, hi.0), y, (gap * 0.4).min(0.035));
            y += 0.14 + 0.08 * rng.f();
        }
        let mut x = lo.0 + 0.02;
        while x < hi.0 {
            let half = gap * 0.5;
            self.detail(|b| {
                let [r0, r1, r2, r3] = [at(x, lo.1, -half), at(x, hi.1, -half), at(x, hi.1, half), at(x, lo.1, half)].map(|p| b.pt(p));
                for (p, q, s) in [(r0, r1, Stroke::Hull4), (r1, r2, Stroke::Hull5), (r2, r3, Stroke::Hull4), (r1, r2, Stroke::Cross)] {
                    b.line_ix(p, q, (s, s), Gate::Always);
                }
            });
            x += 0.07 + 0.05 * rng.f();
        }
    }

    /// A walkway along a corridor from `a0` to `a1` at height `y`, `ww` either side of its middle.
    fn walkway(&mut self, at: &impl Fn(f32, f32, f32) -> V3, (a0, a1): (f32, f32), y: f32, ww: f32) {
        self.poly(&[at(a0, y, -ww), at(a1, y, -ww), at(a1, y, ww), at(a0, y, ww)], [0.0, 1.0, 0.0], Look::plate(0.35), Gate::Always);
        self.line(at(a0, y + 0.002, 0.0), at(a1, y + 0.002, 0.0), Stroke::Strip);
        self.glow(at((a0 + a1) * 0.5, y + 0.01, 0.0), (Tone::Green, 0.18), 0.25f32.min((a1 - a0) * 0.4), 0.0);
        self.detail(|b| {
            for side in [-1.0, 1.0] {
                b.line(at(a0, y + 0.035, side * ww), at(a1, y + 0.035, side * ww), Stroke::Rail);
            }
        });
    }

    /// The outer shell: every face of the core plated (`PANELS` a side), a structural rib every
    /// `RIB_EVERY` panels and along the edges.
    fn shell(&mut self) {
        let mut rng = Rng::new(fnv("shell"));
        for f in 0..6u8 {
            for c in 0..PANELS * PANELS {
                self.panel(f, (c % PANELS, c / PANELS), &mut rng);
            }
            self.ribs(f);
        }
    }

    /// A point on shell face `f` at face coordinates `(a, b)`, `depth` in from it.
    fn on_shell(f: u8, a: f32, b: f32, depth: f32) -> V3 {
        let (axis, side) = face_axis(usize::from(f));
        let (u, v) = match axis {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        let mut p = [0.0; 3];
        p[axis] = side * (CORE - depth);
        p[u] = a;
        p[v] = b;
        p
    }

    /// Panel `(i, j)` of shell face `f`, recessed at its own depth: kept on a face that looks at the
    /// camera only in the ragged ring along its edges, on one that looks away nearly always; a seam
    /// across a third of them, a light on a few. Seams and lights lie on their panel and sort just
    /// nearer than it, so they show over it from outside (a face that looks at the camera) and from
    /// inside (one that looks away) alike.
    fn panel(&mut self, f: u8, (i, j): (usize, usize), rng: &mut Rng) {
        let ring = i.min(PANELS - 1 - i).min(j).min(PANELS - 1 - j) as f32;
        let ragged = 1.6 + 0.7 * (i as f32 * 1.7 + j as f32 * 0.9 + f32::from(f)).sin() + 0.8 * rng.f();
        let (near, far) = (ring < ragged, rng.f() < 0.93);
        let depth = 0.01 + 0.05 * rng.f();
        let (seam, light) = (rng.f(), rng.f());
        if !(near || far) {
            return;
        }
        let step = 2.0 * CORE / PANELS as f32;
        let (a0, b0) = (-CORE + step * i as f32, -CORE + step * j as f32);
        let (a1, b1, g) = (a0 + step, b0 + step, 0.004);
        let p = |a: f32, b: f32| Self::on_shell(f, a, b, depth);
        let gate = Gate::Shell { face: f, near, far, surface: false };
        self.poly(&[p(a0 + g, b0 + g), p(a1 - g, b0 + g), p(a1 - g, b1 - g), p(a0 + g, b1 - g)], [0.0; 3], Look::plate(0.12 + 0.3 * rng.f()), gate);
        if seam < 0.35 {
            let (s0, s1) = (p(a0 + 0.01, (b0 + b1) * 0.5), p(a1 - 0.01, (b0 + b1) * 0.5));
            self.detail(|b| b.pinned(Pin { at: None, bias: 0.002 }, |b| b.line_gated(s0, s1, (Stroke::Seam, Stroke::Sheen), gate)));
        }
        if light < 0.07 {
            let tone = if rng.f() < 0.7 { Tone::Green } else { Tone::Bright };
            let at = p((a0 + a1) * 0.5, (b0 + b1) * 0.5);
            self.glow_gated(at, (tone, 0.8), 0.02, 0.002, Gate::Shell { face: f, near, far: far && light < 0.04, surface: false });
        }
    }

    /// Shell face `f`'s structural ribs, every `RIB_EVERY` panels both ways, its edges among them:
    /// bolder on a face that looks at the camera.
    fn ribs(&mut self, f: u8) {
        let gate = Gate::Shell { face: f, near: true, far: true, surface: true };
        for i in (0..=PANELS).step_by(RIB_EVERY) {
            let a = -CORE + 2.0 * CORE * i as f32 / PANELS as f32;
            for (p, q) in
                [(Self::on_shell(f, a, -CORE, 0.0), Self::on_shell(f, a, CORE, 0.0)), (Self::on_shell(f, -CORE, a, 0.0), Self::on_shell(f, CORE, a, 0.0))]
            {
                let (p, q) = (self.pt(p), self.pt(q));
                self.line_ix(p, q, (Stroke::Hull5, Stroke::Hull3), gate);
                self.line_ix(p, q, (Stroke::Rib, Stroke::RibFar), gate);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(key: &str, n: u32, min: V3, max: V3) -> Room {
        Room { min, max, key: key.to_owned(), title: format!("{key} · {n}"), lessons: (0..n).collect() }
    }

    /// One room of `kind` holding `n` lessons, built alone.
    fn alone(kind: Kind, n: u32, min: V3, max: V3) -> Interior {
        let mut b = Build::new(n as usize);
        b.room(&room("area", n, min, max), kind);
        b.finish()
    }

    fn inside(p: V3, min: V3, max: V3) -> bool {
        (0..3).all(|a| p[a] >= min[a] - 1e-3 && p[a] <= max[a] + 1e-3)
    }

    /// The area of face `f`, in world units.
    fn area(it: &Interior, f: &Face) -> f32 {
        let ix = &it.corners[f.first as usize..f.first as usize + usize::from(f.n)];
        let mut n = [0.0f32; 3];
        for (k, &i) in ix.iter().enumerate() {
            let (a, b) = (it.points[i as usize], it.points[ix[(k + 1) % ix.len()] as usize]);
            n = [n[0] + a[1] * b[2] - a[2] * b[1], n[1] + a[2] * b[0] - a[0] * b[2], n[2] + a[0] * b[1] - a[1] * b[0]];
        }
        0.5 * dot(n, n).sqrt()
    }

    /// What a lesson's light may be buried in: a cylinder (its cap: 14 or more corners facing up; its
    /// axis, radius and top; it stands on the room's floor) or a box (`Build::cube`'s six faces in a row).
    enum Solid {
        Cylinder { x: f32, z: f32, r: f32, top: f32 },
        Box { min: V3, max: V3 },
    }

    fn corners_of<'a>(it: &'a Interior, f: &Face) -> impl Iterator<Item = V3> + 'a {
        it.corners[f.first as usize..f.first as usize + usize::from(f.n)].iter().map(|&i| it.points[i as usize])
    }

    fn solids(it: &Interior) -> Vec<Solid> {
        const CUBE: [V3; 6] = [[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0], [0.0, 0.0, 1.0]];
        let mut out = Vec::new();
        for (k, f) in it.faces.iter().enumerate() {
            if f.n >= 14 && f.normal == [0.0, 1.0, 0.0] {
                let pts: Vec<V3> = corners_of(it, f).collect();
                let (x, z) = (pts.iter().map(|p| p[0]).sum::<f32>() / pts.len() as f32, pts.iter().map(|p| p[2]).sum::<f32>() / pts.len() as f32);
                out.push(Solid::Cylinder { x, z, r: (pts[0][0] - x).hypot(pts[0][2] - z), top: pts[0][1] });
            }
            let six = it.faces.get(k..k + 6).unwrap_or_default();
            if six.len() == 6 && six.iter().zip(CUBE).all(|(f, n)| f.normal == n && f.n == 4) {
                let pts: Vec<V3> = six.iter().flat_map(|f| corners_of(it, f)).collect();
                let (min, max) = (
                    [0, 1, 2].map(|a| pts.iter().map(|p| p[a]).fold(f32::MAX, f32::min)),
                    [0, 1, 2].map(|a| pts.iter().map(|p| p[a]).fold(f32::MIN, f32::max)),
                );
                out.push(Solid::Box { min, max });
            }
        }
        out
    }

    /// Whether `p` lies inside one of `solids` (cylinders standing on `floor`), not just on its surface.
    fn buried(p: V3, solids: &[Solid], floor: f32) -> bool {
        solids.iter().any(|s| match *s {
            Solid::Cylinder { x, z, r, top } => (p[0] - x).hypot(p[2] - z) < 0.98 * r && p[1] > floor + 1e-4 && p[1] < top - 1e-4,
            Solid::Box { min, max } => (0..3).all(|a| p[a] > min[a] + 1e-4 && p[a] < max[a] - 1e-4),
        })
    }

    /// Every kind in every bay, flat and thin ones too, seats each lesson inside the bay with a light of
    /// its own there that no solid of the room buries or covers just in front of it (a chamber standing
    /// in another, an alcove's back sunk into the ring's column, a post, a slit over a chamber's top).
    #[test]
    fn every_kind_seats_every_lesson_inside_its_room_and_lights_it() {
        let boxes = [
            ([-0.5, -0.4, -0.3], [0.2, 0.5, 0.4]),
            ([0.0, 0.0, 0.0], [0.25, 0.3, 0.2]),
            ([0.0, 0.0, 0.0], [0.12, 0.08, 0.15]),
            ([-0.9, -0.9, -0.9], [0.9, 0.9, 0.9]),
            // Flat bays, as a store of many areas cuts.
            ([0.0, 0.0, 0.0], [0.15, 0.05, 0.15]),
            ([0.0, 0.0, 0.0], [0.3, 0.06, 0.3]),
            ([0.0, 0.0, 0.0], [0.13, 0.07, 0.14]),
            // Slabs a cut of a small area beside large ones once made.
            ([0.0, 0.0, 0.0], [0.034, 0.41, 0.43]),
            ([0.17, 0.88, 0.35], [0.9, 0.898, 0.9]),
        ];
        for kind in Kind::ALL {
            for (min, max) in boxes {
                for n in [1, 4, 7, 12, 40, 300] {
                    let it = alone(kind, n, min, max);
                    let solids = solids(&it);
                    let mut lights = vec![0usize; n as usize];
                    for f in &it.faces {
                        if let Look::Light { lesson, .. } = f.look
                            && area(&it, f) > 0.0
                        {
                            let c = corners_of(&it, f).fold([0.0; 3], |c, p| add(c, p, 1.0 / f32::from(f.n)));
                            // Seen from in front: the point just off it is clear of every solid.
                            lights[lesson as usize] += usize::from(inside(c, min, max) && !buried(add(c, f.normal, 1e-3), &solids, min[1]));
                        }
                    }
                    let mut seats: Vec<[u32; 3]> = it.seats.iter().map(|s| s.at.map(f32::to_bits)).collect();
                    for (i, s) in it.seats.iter().enumerate() {
                        assert!(s.r > 0.0 && inside(s.at, min, max), "{kind:?} {n} in {min:?}..{max:?}: lesson {i} at {:?}", s.at);
                        assert!(lights[i] >= 1, "{kind:?} {n} in {min:?}..{max:?}: lesson {i} has no light to see in its bay");
                    }
                    seats.sort_unstable();
                    seats.dedup();
                    assert_eq!(seats.len(), n as usize, "{kind:?} {n} in {min:?}..{max:?}: lessons share a seat");
                }
            }
        }
    }

    /// A lesson's light glows round it once: of its light faces, those that spill are such that a
    /// camera sees at most one of them, from any side.
    #[test]
    fn a_lesson_spills_its_glow_once_from_any_side() {
        let views: Vec<V3> =
            (0..24).map(|k| (k as f32 / 24.0 * TAU + 0.1, 0.3 * (k % 5) as f32 - 0.6)).map(|(y, p)| [y.cos() * p.cos(), p.sin(), y.sin() * p.cos()]).collect();
        for kind in Kind::ALL {
            let it = alone(kind, 40, [0.0; 3], [0.4; 3]);
            for view in &views {
                let mut spills = [0u8; 40];
                for f in &it.faces {
                    if let Look::Light { lesson, spill, .. } = f.look
                        && spill > 0.0
                        && f.gate.shows(f.normal, *view, &[false; 6])
                    {
                        spills[lesson as usize] += 1;
                    }
                }
                assert!(spills.iter().all(|&k| k <= 1), "{kind:?} from {view:?}: {spills:?}");
            }
        }
    }

    /// No riser, its elbow included, leaves the space the rooms and the shafts fill: the shell's plating
    /// (0.94 to 0.99 from the centre) stays clear of tubes from inside. Rooms that reach the core's
    /// sides on both ends, in many numbers, so every riser stands at an edge.
    #[test]
    fn the_inside_stays_clear_of_the_shell() {
        for n in 1..=12u32 {
            let rooms: Vec<Room> = (0..n)
                .map(|k| {
                    let z = -0.9 + 1.8 * k as f32 / n as f32;
                    room(&format!("area-{k}"), 10, [-0.9, -0.9, z], [0.9, 0.9, z + 1.8 / n as f32 - 0.02])
                })
                .collect();
            let mut b = Build::new(10);
            for (r, kind) in rooms.iter().zip(Kind::ALL.iter().cycle()) {
                b.room(r, *kind);
            }
            b.risers(&rooms);
            let it = b.finish();
            let out = it.points.iter().map(|p| p.iter().fold(0.0f32, |m, c| m.max(c.abs()))).fold(0.0f32, f32::max);
            assert!(out <= CORE - 0.06, "{n} rooms: a point {out} from the centre");
        }
    }

    #[test]
    fn the_geometry_grows_by_a_bounded_number_of_primitives_per_lesson() {
        // Rooms of a size: the walls' cells and the posts are per room, the lights per lesson.
        for kind in Kind::ALL {
            let small = alone(kind, 10, [0.0; 3], [0.4; 3]);
            let large = alone(kind, 1000, [0.0; 3], [0.4; 3]);
            let per = (large.prims() + large.points.len()) as f32 / 1000.0;
            assert!(per < 60.0, "{kind:?}: {per} primitives and points a lesson");
            assert!(small.prims() < 4000, "{kind:?}: {} for ten lessons", small.prims());
        }
    }

    #[test]
    fn a_honeycomb_of_few_lessons_has_few_cells_however_large_its_area() {
        for n in [1, 3, 10] {
            let it = alone(Kind::Honeycomb, n, [-0.9; 3], [0.9; 3]);
            let cells = it.faces.iter().filter(|f| f.n == 6).count();
            assert!(cells <= HEX_PER_LESSON * n as usize + HEX_ROOM, "{n} lessons: {cells} cells");
            assert!(it.points.len() < 4 * HEX_ROOM * 6, "{n} lessons: {} points", it.points.len());
        }
        // A store's area keeps the size of cell it looks right at.
        let it = alone(Kind::Honeycomb, 18, [0.0; 3], [0.53; 3]);
        let first = it.faces.iter().find(|f| f.n == 6).expect("a cell");
        let (a, b) = (it.points[it.corners[first.first as usize] as usize], it.points[it.corners[first.first as usize + 3] as usize]);
        assert!((0.5 * (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f32>().sqrt() - HEX_MAX).abs() < 1e-4, "{a:?} {b:?}");
    }

    #[test]
    fn a_line_along_a_faces_points_runs_over_them() {
        let rooms: Vec<Room> = Kind::ALL
            .iter()
            .enumerate()
            .map(|(k, _)| {
                let x = -0.9 + 0.225 * k as f32;
                room(&format!("area-{k}"), 40, [x, -0.9, -0.9], [x + 0.2, 0.9, 0.9])
            })
            .collect();
        let it = Interior::build(&rooms, &[], 40 * 8);
        let mut distinct: Vec<[u32; 3]> = it.points.iter().map(|p| p.map(f32::to_bits)).collect();
        distinct.sort_unstable();
        distinct.dedup();
        assert!(distinct.len() * 100 >= it.points.len() * 93, "{} distinct of {}", distinct.len(), it.points.len());
    }

    #[test]
    fn the_points_only_fine_detail_uses_come_last() {
        let rooms = [room("a", 300, [-0.9; 3], [0.0, 0.9, 0.9]), room("b", 120, [0.1, -0.9, -0.9], [0.9; 3])];
        let it = Interior::build(&rooms, &[Shaft { axis: 0, lo: 0.0, hi: 0.1, min: [-0.9; 3], max: [0.9; 3] }], 420);
        let mut used = vec![false; it.points.len()];
        for f in it.faces.iter().filter(|f| !f.fine) {
            for &c in &it.corners[f.first as usize..f.first as usize + usize::from(f.n + f.hole)] {
                used[c as usize] = true;
            }
            f.pin.at.inspect(|&a| used[a as usize] = true);
        }
        for l in it.lines.iter().filter(|l| !l.fine) {
            (used[l.a as usize], used[l.b as usize]) = (true, true);
            l.pin.at.inspect(|&a| used[a as usize] = true);
        }
        it.glows.iter().for_each(|g| used[g.at as usize] = true);
        it.labels.iter().for_each(|l| used[l.at as usize] = true);
        assert!(used[it.coarse..].iter().all(|u| !u), "a point past `coarse` that is not fine detail's");
        // Fine detail's own, left unprojected while the core is small.
        assert!(it.coarse < it.points.len(), "{} coarse of {}", it.coarse, it.points.len());
        // And every index lies among the points.
        let n = it.points.len();
        assert!(it.corners.iter().all(|&c| (c as usize) < n) && it.lines.iter().all(|l| (l.a as usize) < n && (l.b as usize) < n));
    }

    #[test]
    fn the_kinds_of_one_view_differ() {
        let names = ["verification", "spec-conformance", "data-source", "other", "scope", "security", "concurrency", "minimality", "reuse-existing"];
        for n in 1..=names.len() {
            let ks = kinds(&names[..n]);
            for k in Kind::ALL {
                let uses = ks.iter().filter(|x| **x == k).count();
                assert!(uses <= n.div_ceil(KINDS), "{n} areas: {k:?} {uses} times");
            }
        }
        // An area keeps its kind by its name, whatever order the areas come in.
        let mut rev = names;
        rev.reverse();
        let (a, b) = (kinds(&names), kinds(&rev));
        for (i, k) in a.iter().enumerate() {
            assert_eq!(*k, b[names.len() - 1 - i]);
        }
    }

    #[test]
    fn the_same_rooms_build_the_same_interior() {
        let rooms = [room("a", 30, [-0.9; 3], [0.0, 0.9, 0.9]), room("b", 12, [0.1, -0.9, -0.9], [0.9; 3])];
        let shafts = [Shaft { axis: 0, lo: 0.0, hi: 0.1, min: [-0.9; 3], max: [0.9; 3] }];
        let one = Interior::build(&rooms, &shafts, 42);
        assert_eq!(one, Interior::build(&rooms, &shafts, 42));
        assert!(one.lines.iter().any(|l| l.style == Stroke::Strip), "a corridor in the shaft");
        assert_eq!(one.labels.len(), 2);
    }

    #[test]
    fn the_shell_plates_every_face_and_opens_the_near_ones_from_their_edges() {
        let it = Interior::build(&[], &[], 0);
        for f in 0..6u8 {
            let panels: Vec<&Face> = it.faces.iter().filter(|x| matches!(x.gate, Gate::Shell { face, .. } if face == f)).collect();
            let far = panels.iter().filter(|x| matches!(x.gate, Gate::Shell { far: true, .. })).count();
            let near = panels.iter().filter(|x| matches!(x.gate, Gate::Shell { near: true, .. })).count();
            assert!(far > PANELS * PANELS * 85 / 100, "face {f}: {far} panels when it looks away");
            assert!(near > PANELS * 4 && near < PANELS * PANELS * 6 / 10, "face {f}: {near} panels when it looks at the camera");
            // The middle of a near face is open.
            let centre = |x: &&&Face| {
                let p = it.points[it.corners[x.first as usize] as usize];
                p.iter().filter(|c| c.abs() < 0.4).count() == 2
            };
            assert!(!panels.iter().filter(|x| matches!(x.gate, Gate::Shell { near: true, .. })).any(|x| centre(&x)), "face {f}");
        }
        // Every point lies within the core.
        assert!(it.points.iter().all(|p| p.iter().all(|c| c.abs() <= CORE + 1e-5)));
    }

    #[test]
    fn the_shells_panels_read_as_plates_of_several_shades() {
        let it = Interior::build(&[], &[], 0);
        let mut levels: Vec<u8> =
            it.faces.iter().filter(|f| matches!(f.gate, Gate::Shell { .. })).filter_map(|f| if let Look::Plate(l) = f.look { Some(l) } else { None }).collect();
        levels.sort_unstable();
        levels.dedup();
        // As light as the approved look's 0.12 to 0.42, at least, so the ring of panels stands out from the
        // dark space and the shadowed sides behind it, in shades that tell one panel from the next.
        assert!(levels.first().is_some_and(|l| *l >= shade_level(0.12)) && levels.len() >= 7, "{levels:?}");
    }

    #[test]
    fn a_gate_shows_by_the_camera() {
        let view = [0.0, 0.0, 1.0];
        let mut facing = [false; 6];
        facing[5] = true;
        assert!(Gate::Cull.shows([0.0, 0.0, 1.0], view, &facing) && !Gate::Cull.shows([0.0, 0.0, -1.0], view, &facing));
        let g = Gate::Shell { face: 5, near: true, far: false, surface: false };
        assert!(g.shows([0.0; 3], view, &facing) && !g.away(&facing));
        let g = Gate::Shell { face: 4, near: true, far: false, surface: false };
        assert!(!g.shows([0.0; 3], view, &facing) && g.away(&facing));
    }
}
