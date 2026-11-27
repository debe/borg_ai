//! The browser's `Painter` (paint.rs): a 2D canvas. Each tone's CSS colour and glow sprite are made
//! once at start from one palette (`rgb`). The style is lazy: `style` only notes what is wanted, and a
//! stroke, fill or glow sets on the context just what it uses and what differs from what the context
//! has, so a run of glows or of fills never touches the stroke colour or line width. Every glow is one
//! `drawImage` of its tone's sprite.
//! Labels are drawn centred by the context (`textAlign`), never measured.

use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

use crate::paint::Painter;
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

/// A glow sprite's side in pixels.
const SPRITE: u32 = 64;

/// What the context has set, as far as this painter knows; `None` (or a negative number) is unknown.
#[derive(Clone, Copy)]
struct Set {
    stroke: Option<Tone>,
    fill: Option<Tone>,
    alpha: f32,
    width: f32,
}

const UNKNOWN: Set = Set { stroke: None, fill: None, alpha: -1.0, width: -1.0 };

pub struct Canvas {
    ctx: CanvasRenderingContext2d,
    /// Each tone's glow sprite and CSS colour, by `Tone::ix`.
    sprites: [HtmlCanvasElement; Tone::ALL.len()],
    css: [String; Tone::ALL.len()],
    dpr: f64,
    /// The style `style` asked for: tone, alpha, width.
    want: (Tone, f32, f32),
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
        let css = Tone::ALL.map(|t| {
            let (red, green, blue) = rgb(t);
            format!("#{red:02x}{green:02x}{blue:02x}")
        });
        let mut c = Self { ctx, sprites, css, dpr, want: (Tone::Green, 1.0, 1.0), set: UNKNOWN };
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

    fn fill_tone(&mut self, t: Tone) {
        if self.set.fill != Some(t) {
            self.ctx.set_fill_style_str(&self.css[t.ix()]);
            self.set.fill = Some(t);
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
            self.ctx.set_stroke_style_str(&self.css[tone.ix()]);
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
        let (tone, alpha, _) = self.want;
        self.fill_tone(tone);
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
        self.fill_tone(tone);
        self.alpha(alpha);
        let _ = self.ctx.fill_text(s, f64::from(x), f64::from(y));
    }
}
