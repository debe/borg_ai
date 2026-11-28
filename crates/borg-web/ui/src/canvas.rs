//! The browser's `Painter` (paint.rs): a 2D canvas. Each tone's CSS colour and glow sprite, and each
//! level of the core's plating (`plate_rgb`), are made once at start from one palette (`rgb`), the
//! colours as JS strings the context takes by reference (`Styles`): a change of colour
//! passes no string from wasm to decode, so a frame makes none on the JS heap either. The style is
//! lazy: `style` only notes what is wanted, and a stroke, fill or glow sets on the context just what it
//! uses and what differs from what the context has, so a run of glows or of fills never touches the
//! stroke colour or line width. Every glow is one `drawImage` of its tone's sprite.
//! Labels are drawn centred by the context (`textAlign`), never measured.

use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

#[wasm_bindgen]
extern "C" {
    /// The 2D context, for its `fillStyle` and `strokeStyle` set to a colour made once (`Canvas::new`):
    /// web-sys's own setters take a `&str`, a string to pass over each time.
    type Styles;
    #[wasm_bindgen(method, setter = fillStyle)]
    fn set_fill(this: &Styles, colour: &JsValue);
    #[wasm_bindgen(method, setter = strokeStyle)]
    fn set_stroke(this: &Styles, colour: &JsValue);
}

use crate::paint::{PLATES, Painter, plate_of};
use crate::scene::Tone;

/// The palette: strokes, fills and the glow sprites all take their colour from here.
const fn rgb(t: Tone) -> (u8, u8, u8) {
    match t {
        Tone::Green => (57, 255, 136),
        Tone::Bright => (217, 255, 233),
        Tone::Amber => (255, 181, 71),
        Tone::Red => (255, 77, 94),
        Tone::Cyan => (79, 216, 255),
        Tone::Dim => (63, 127, 94),
        Tone::Hull => (14, 47, 30),
    }
}

/// The colour of plating level `level` (paint.rs `plate_level`): the hull darkened where the light
/// misses it, then tinted green where it falls and the haze leaves it near.
fn plate_rgb(level: u8) -> (u8, u8, u8) {
    let (shade, fog) = plate_of(level);
    let (dark, green) = (0.55 * (1.0 - shade), 0.02 + 0.16 * shade * fog);
    let (h, g) = (rgb(Tone::Hull), rgb(Tone::Green));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a mix of two channels stays in 0..=255")]
    let mix = |h: u8, g: u8| (f32::from(h) * (1.0 - dark) * (1.0 - green) + f32::from(g) * green).round().clamp(0.0, 255.0) as u8;
    (mix(h.0, g.0), mix(h.1, g.1), mix(h.2, g.2))
}

fn hex((red, green, blue): (u8, u8, u8)) -> String {
    format!("#{red:02x}{green:02x}{blue:02x}")
}

/// A glow sprite's side in pixels.
const SPRITE: u32 = 64;

/// A fill colour: a tone's, or a level of plating's.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ink {
    Tone(Tone),
    Plate(u8),
}

/// What the context has set, as far as this painter knows; `None` (or a negative number) is unknown.
#[derive(Clone, Copy)]
struct Set {
    stroke: Option<Tone>,
    fill: Option<Ink>,
    alpha: f32,
    width: f32,
}

const UNKNOWN: Set = Set { stroke: None, fill: None, alpha: -1.0, width: -1.0 };

pub struct Canvas {
    ctx: CanvasRenderingContext2d,
    /// Each tone's glow sprite and CSS colour, by `Tone::ix`.
    sprites: [HtmlCanvasElement; Tone::ALL.len()],
    css: [JsValue; Tone::ALL.len()],
    /// Each plating level's CSS colour, by level.
    plates: Vec<JsValue>,
    dpr: f64,
    /// The style `style` asked for: tone, alpha, width; and the fill `plate` asked for since, its level
    /// and alpha, which fills take instead.
    want: (Tone, f32, f32),
    plate: Option<(u8, f32)>,
    set: Set,
}

fn context(c: &HtmlCanvasElement) -> Option<CanvasRenderingContext2d> {
    c.get_context("2d").ok()??.dyn_into().ok()
}

fn sprite(doc: &web_sys::Document, t: Tone) -> Option<HtmlCanvasElement> {
    let el: HtmlCanvasElement = doc.create_element("canvas").ok()?.dyn_into().ok()?;
    el.set_width(SPRITE);
    el.set_height(SPRITE);
    let ctx = context(&el)?;
    let half = f64::from(SPRITE) / 2.0;
    let grad = ctx.create_radial_gradient(half, half, 0.0, half, half, half).ok()?;
    let (red, green, blue) = rgb(t);
    for (at, alpha) in [(0.0, 1.0), (0.18, 0.55), (0.45, 0.16), (1.0, 0.0)] {
        let _ = grad.add_color_stop(at, &format!("rgba({red},{green},{blue},{alpha})"));
    }
    ctx.set_fill_style_canvas_gradient(&grad);
    ctx.fill_rect(0.0, 0.0, f64::from(SPRITE), f64::from(SPRITE));
    Some(el)
}

impl Canvas {
    #[must_use]
    pub fn new(canvas: &HtmlCanvasElement, dpr: f64) -> Option<Self> {
        let ctx = context(canvas)?;
        let doc = web_sys::window()?.document()?;
        let sprites = Tone::ALL.iter().map(|t| sprite(&doc, *t)).collect::<Option<Vec<_>>>()?.try_into().ok()?;
        let css = Tone::ALL.map(|t| JsValue::from_str(&hex(rgb(t))));
        let plates = (0..PLATES).map(|l| JsValue::from_str(&hex(plate_rgb(u8::try_from(l).unwrap_or(u8::MAX))))).collect();
        let mut c = Self { ctx, sprites, css, plates, dpr, want: (Tone::Green, 1.0, 1.0), plate: None, set: UNKNOWN };
        c.reset();
        Some(c)
    }

    /// Takes the device pixel ratio after the canvas was resized. Resizing a canvas resets its whole
    /// context, so the text settings are set again and nothing is assumed set.
    pub fn set_dpr(&mut self, dpr: f64) {
        self.dpr = dpr;
        self.reset();
    }

    fn reset(&mut self) {
        self.ctx.set_font("11px ui-monospace, SFMono-Regular, Menlo, monospace");
        self.ctx.set_text_align("center");
        self.ctx.set_text_baseline("top");
        self.ctx.set_line_cap("round");
        self.set = UNKNOWN;
    }

    fn alpha(&mut self, a: f32) {
        if (a - self.set.alpha).abs() > f32::EPSILON {
            self.ctx.set_global_alpha(f64::from(a.clamp(0.0, 1.0)));
            self.set.alpha = a;
        }
    }

    fn fill_ink(&mut self, ink: Ink) {
        if self.set.fill != Some(ink) {
            let colour = match ink {
                Ink::Tone(t) => &self.css[t.ix()],
                Ink::Plate(l) => self.plates.get(usize::from(l)).unwrap_or(&self.css[Tone::Hull.ix()]),
            };
            self.ctx.unchecked_ref::<Styles>().set_fill(colour);
            self.set.fill = Some(ink);
        }
    }
}

impl Painter for Canvas {
    fn clear(&mut self, w: f32, h: f32) {
        let _ = self.ctx.set_transform(self.dpr, 0.0, 0.0, self.dpr, 0.0, 0.0);
        self.ctx.clear_rect(0.0, 0.0, f64::from(w), f64::from(h));
    }

    fn style(&mut self, tone: Tone, alpha: f32, width: f32) {
        self.want = (tone, alpha, width);
        self.plate = None;
    }

    fn plate(&mut self, level: u8, alpha: f32) {
        self.plate = Some((level, alpha));
    }

    fn begin(&mut self) {
        self.ctx.begin_path();
    }

    fn move_to(&mut self, x: f32, y: f32) {
        self.ctx.move_to(f64::from(x), f64::from(y));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.ctx.line_to(f64::from(x), f64::from(y));
    }

    fn circle(&mut self, x: f32, y: f32, r: f32) {
        let (x, y, r) = (f64::from(x), f64::from(y), f64::from(r.max(0.1)));
        self.ctx.move_to(x + r, y);
        let _ = self.ctx.arc(x, y, r, 0.0, std::f64::consts::TAU);
    }

    fn stroke(&mut self) {
        let (tone, alpha, width) = self.want;
        if self.set.stroke != Some(tone) {
            self.ctx.unchecked_ref::<Styles>().set_stroke(&self.css[tone.ix()]);
            self.set.stroke = Some(tone);
        }
        if (width - self.set.width).abs() > f32::EPSILON {
            self.ctx.set_line_width(f64::from(width));
            self.set.width = width;
        }
        self.alpha(alpha);
        self.ctx.stroke();
    }

    fn fill(&mut self) {
        let (ink, alpha) = match self.plate {
            Some((level, alpha)) => (Ink::Plate(level), alpha),
            None => (Ink::Tone(self.want.0), self.want.1),
        };
        self.fill_ink(ink);
        self.alpha(alpha);
        self.ctx.fill();
    }

    fn glow(&mut self, tone: Tone, x: f32, y: f32, r: f32, alpha: f32) {
        if alpha <= 0.01 || r <= 0.5 {
            return;
        }
        self.alpha(alpha);
        let (x, y, r) = (f64::from(x), f64::from(y), f64::from(r));
        let sprite = &self.sprites[tone.ix()];
        let _ = self.ctx.draw_image_with_html_canvas_element_and_dw_and_dh(sprite, x - r, y - r, 2.0 * r, 2.0 * r);
    }

    fn text(&mut self, s: &str, x: f32, y: f32, tone: Tone, alpha: f32) {
        self.fill_ink(Ink::Tone(tone));
        self.alpha(alpha);
        let _ = self.ctx.fill_text(s, f64::from(x), f64::from(y));
    }
}
