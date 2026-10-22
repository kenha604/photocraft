//! Edit › Generative Fill: repaint the selection (or, with no selection, do an art pass over the
//! whole image) with a generative image model, from a text prompt.
//!
//! The model sees what the user sees: the flattened visible canvas (every visible layer, not just
//! the active one), converted to sRGB. With a selection it also gets the selection as a
//! black-and-white mask, and when the selection is small, the canvas around it is sent at a
//! higher resolution with the whole canvas as a third, context image. The edited image comes back
//! in sRGB; it is converted to the document's colours and blended into the layer through the
//! selection's soft coverage, so pixels outside the selection never change, whatever the model did
//! to them. One undo step.
//!
//! The engine never touches the network: the desktop app installs an [`ImageGenerator`] (Google
//! Gemini, `photocraft-genai`) in [`Session::image_generator`]. Without one (the web build, the
//! CLI, no API key) the command reports what to set up.

use std::sync::Arc;

use photocraft_cms::{Intent, Transform, builtin::Builtin};
use photocraft_color::PixelFormat;
use photocraft_doc::{Document, Layer, LayerContent, LayerId};
use photocraft_genai::{EditRequest, GenError, ImageData};
use photocraft_geom::Rect;
use photocraft_raster::{Surface, from_rgba_into};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::jobs::JobCtx;
use crate::{EngineError, Result, Session};

/// The command id (Photoshop's Edit › Generative Fill…).
pub const COMMAND: &str = "edit.generativeFill";

/// Longest side of an image sent to the model.
pub const MAX_SEND_SIDE: u32 = 2048;
/// Longest side of the whole-canvas context image that goes with a crop.
pub const CONTEXT_SIDE: u32 = 1024;
/// A crop around a small selection is at least this big (when the canvas is).
const MIN_CROP_SIDE: i32 = 1024;
/// Largest image decoded from the model's answer, per side.
const MAX_RESULT_SIDE: u32 = 16_384;
/// Largest area read at full resolution for one request (the window around the selection).
const MAX_WINDOW_PIXELS: u64 = 400_000_000;
/// The flat colour transparent canvas areas are shown on (the model gets opaque images).
const BACKGROUND: [f32; 3] = [1.0, 1.0, 1.0];

/// A generative image model the session can call. Implementations block (the command runs it on
/// a background job's worker thread) and must not panic.
pub trait ImageGenerator: Send + Sync {
    /// Shown in the dialog and in errors, e.g. "Google Gemini (gemini-nano-banana-2.1)".
    fn describe(&self) -> String;
    /// Run one edit; the answer is an encoded image (PNG, JPEG, ...).
    fn generate(&self, req: &EditRequest) -> std::result::Result<ImageData, GenError>;
}

impl ImageGenerator for photocraft_genai::Gemini {
    fn describe(&self) -> String {
        format!("Google Gemini ({})", self.model())
    }
    fn generate(&self, req: &EditRequest) -> std::result::Result<ImageData, GenError> {
        photocraft_genai::Gemini::generate(self, req)
    }
}

impl Session {
    /// Install (or with `None`, remove) the model Generative Fill uses.
    pub fn set_image_generator(&mut self, generator: Option<Arc<dyn ImageGenerator>>) {
        self.image_generator = generator;
    }

    /// What Generative Fill would use, e.g. "Google Gemini (…)"; `None` when nothing is set up.
    pub fn image_generator_name(&self) -> Option<String> {
        self.image_generator.as_ref().map(|g| g.describe())
    }
}

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: COMMAND.into(), msg: msg.into() }
}

/// The active layer, when it's an unlocked pixel layer.
fn pixel_layer(s: &Session) -> std::result::Result<LayerId, String> {
    let d = s.active().ok_or("no document open")?;
    let id = d.active_layer.ok_or("no active layer")?;
    let l = d.doc.layer(id).ok_or("no active layer")?;
    if !matches!(l.content, LayerContent::Raster(_)) {
        return Err(format!("active layer is {} {} layer, not a pixel layer", l.content.article(), l.content.kind_name()));
    }
    let locks = d.doc.effective_locks(id);
    if locks.all || locks.pixels {
        return Err(format!("the layer \"{}\" is locked", l.name));
    }
    Ok(id)
}

fn can_fill(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    // An art pass with output "new" only needs a document; the dialog asks for the rest.
    if d.doc.selection.is_some() { pixel_layer(s).map(|_| ()) } else { Ok(()) }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: COMMAND,
        label: "Generative Fill…",
        menu: &["Edit"],
        shortcut: None,
        params: r##"{"prompt":string (what the selected area should become, or with no selection what the art pass should do; ≤2000 characters),"output":"current|new"? (into the active pixel layer, or a new layer; default current with a selection, new without)} → {layer, window:[x,y,w,h], model} (sends the visible canvas to the installed image model — Google Gemini on the desktop with an API key — and blends its answer into the selected pixels only; one undo step; runs as a background job)"##,
        enabled: can_fill,
        journal: true,
        run: generative_fill,
    }]
}

/// Where the result goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Output {
    Current,
    New,
}

/// What the work step hands to the apply step: the model's pixels resampled over the edited
/// rectangle (in the document's composite RGB, linear-free, 0..1) and the coverage there.
struct Generated {
    /// `rect` of the document the edit covers (the selection's bounds, or the canvas).
    rect: Rect,
    /// RGB per pixel of `rect`, in the composite profile's colours.
    rgb: Vec<[f32; 3]>,
    /// Coverage per pixel of `rect` (1 everywhere for an art pass).
    cover: Vec<f32>,
    model: String,
}

fn generative_fill(s: &mut Session, p: &Value) -> Result<Value> {
    let prompt = p.get("prompt").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
    if prompt.is_empty() {
        return Err(bad("`prompt` must say what to paint"));
    }
    if prompt.chars().count() > photocraft_genai::MAX_PROMPT_CHARS {
        return Err(bad(format!("`prompt` is longer than {} characters", photocraft_genai::MAX_PROMPT_CHARS)));
    }
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let doc = st.doc.clone();
    let bounds = doc.bounds();
    if bounds.is_empty() {
        return Err(EngineError::Other("the canvas is empty".into()));
    }
    let selection = doc.selection.clone();
    let output = match p.get("output").and_then(Value::as_str) {
        None => {
            if selection.is_some() {
                Output::Current
            } else {
                Output::New
            }
        }
        Some("current") => Output::Current,
        Some("new") => Output::New,
        Some(other) => return Err(bad(format!("`output` must be current|new, not `{other}`"))),
    };
    // The target layer (current) or the layer the new one goes above (new).
    let target = match output {
        Output::Current => Some(pixel_layer(s).map_err(EngineError::Other)?),
        Output::New => st.active_layer,
    };
    let fill_area = match &selection {
        Some(sel) => {
            let hb = sel.content_bounds().intersect(&bounds);
            if hb.is_empty() {
                return Err(EngineError::Other("the selection is empty or outside the canvas".into()));
            }
            Some(hb)
        }
        None => None,
    };
    let generator = s.image_generator.clone().ok_or_else(|| {
        EngineError::Other("Generative Fill needs an image model: set a Gemini API key in Preferences › Integrations (Generative Fill)".into())
    })?;
    let fmt = target.and_then(|t| doc.layer(t)).and_then(|l| l.surface()).map(Surface::format).unwrap_or_else(|| doc.pixel_format());
    let composite = crate::color_cmds::composite_profile(&doc);
    let label = if selection.is_some() { "Generative Fill" } else { "Generative Art Pass" };
    crate::jobs::run(
        s,
        label,
        true,
        move |ctx| {
            ctx.progress(0.0, "Preparing the canvas");
            let to_srgb = Transform::new(&composite, Builtin::Srgb.profile(), Intent::Perceptual, true)
                .map_err(|e| EngineError::Other(format!("colour management: {e}")))?;
            let from_srgb = Transform::new(Builtin::Srgb.profile(), &composite, Intent::Perceptual, true)
                .map_err(|e| EngineError::Other(format!("colour management: {e}")))?;
            let job = Job { doc: &doc, ctx, to_srgb: &to_srgb, from_srgb: &from_srgb };
            let generated = match (&selection, fill_area) {
                (Some(sel), Some(area)) => job.fill(sel, area, &prompt, generator.as_ref())?,
                _ => job.art_pass(&prompt, generator.as_ref())?,
            };
            Ok(Generated { model: generator.describe(), ..generated })
        },
        move |s, g| apply(s, label, output, target, fmt, g),
    )
}

/// The work step's read-only inputs.
struct Job<'a> {
    doc: &'a Document,
    ctx: &'a JobCtx,
    to_srgb: &'a Transform,
    from_srgb: &'a Transform,
}

impl Job<'_> {
    fn cancelled(&self) -> Result<()> {
        self.ctx.check()
    }

    /// Repaint `area` (the selection's bounds) of the canvas.
    fn fill(&self, sel: &Surface, area: Rect, prompt: &str, generator: &dyn ImageGenerator) -> Result<Generated> {
        let bounds = self.doc.bounds();
        let window = edit_window(area, bounds);
        if window.width() as u64 * window.height() as u64 > MAX_WINDOW_PIXELS {
            return Err(EngineError::Other("the selection is too large for Generative Fill; select a smaller area".into()));
        }
        let crop = window != bounds;
        let (sw, sh) = fit(window.width(), window.height(), MAX_SEND_SIDE);
        let ratio = photocraft_genai::nearest_aspect_ratio(sw, sh);
        let frame = Frame::new(sw, sh, ratio);
        // The canvas around the selection, downscaled to what is sent.
        let rgb = self.render_scaled(window, sw, sh)?;
        let cancel = || self.ctx.cancelled();
        let cover_window = crate::fill_cmds::read_window_mask(sel, window, &cancel).ok_or(EngineError::Cancelled)?;
        let mask_small = bin_scalar(&cover_window, window.width() as usize, window.height() as usize, sw as usize, sh as usize);
        drop(cover_window);
        self.cancelled()?;
        let mut images = vec![frame.png_rgb(&rgb, BACKGROUND)?, frame.png_mask(&mask_small)?];
        if crop {
            let ctx_img = self.context_image()?;
            images.push(ctx_img);
        }
        let req = EditRequest {
            instruction: photocraft_genai::fill_instruction(prompt, crop),
            images,
            aspect_ratio: Some(ratio),
            image_size: Some(photocraft_genai::image_size_for(sw.max(sh))),
        };
        let result = self.call(generator, &req)?;
        // Only the selection's bounds are written; resample the answer over them.
        let cover = crate::fill_cmds::read_window_mask(sel, area, &cancel).ok_or(EngineError::Cancelled)?;
        let rgb = self.resample_back(&result, &frame, window, area)?;
        Ok(Generated { rect: area, rgb, cover, model: String::new() })
    }

    /// An art pass over the whole canvas.
    fn art_pass(&self, prompt: &str, generator: &dyn ImageGenerator) -> Result<Generated> {
        let bounds = self.doc.bounds();
        if bounds.width() as u64 * bounds.height() as u64 > MAX_WINDOW_PIXELS {
            return Err(EngineError::Other("the canvas is too large for an art pass".into()));
        }
        let buf = photocraft_compose::thumbnail_buffer(self.doc, MAX_SEND_SIDE);
        self.cancelled()?;
        let (sw, sh) = (buf.rect.width() as u32, buf.rect.height() as u32);
        let rgb = self.flatten_to_srgb(&buf.px);
        let ratio = photocraft_genai::nearest_aspect_ratio(sw, sh);
        let frame = Frame::new(sw, sh, ratio);
        let req = EditRequest {
            instruction: photocraft_genai::art_pass_instruction(prompt),
            images: vec![frame.png_rgb(&rgb, BACKGROUND)?],
            aspect_ratio: Some(ratio),
            image_size: Some(photocraft_genai::image_size_for(sw.max(sh))),
        };
        let result = self.call(generator, &req)?;
        let rgb = self.resample_back(&result, &frame, bounds, bounds)?;
        let n = rgb.len();
        Ok(Generated { rect: bounds, rgb, cover: vec![1.0; n], model: String::new() })
    }

    fn call(&self, generator: &dyn ImageGenerator, req: &EditRequest) -> Result<photocraft_codecs::Image> {
        self.cancelled()?;
        self.ctx.progress(0.15, &format!("Waiting for {}", generator.describe()));
        let answer = generator.generate(req).map_err(|e| EngineError::Other(e.to_string()))?;
        // The model can't be interrupted mid-request: a Cancel during it is honoured here.
        self.cancelled()?;
        self.ctx.progress(0.85, "Blending the result");
        let limits = photocraft_codecs::DecodeOptions::default();
        let img = photocraft_codecs::decode_with(&answer.bytes, &limits).map_err(|e| EngineError::Other(format!("could not read the generated image: {e}")))?;
        if img.width() == 0 || img.height() == 0 || img.width() > MAX_RESULT_SIDE || img.height() > MAX_RESULT_SIDE {
            return Err(EngineError::Other(format!("the generated image has an unusable size ({}×{})", img.width(), img.height())));
        }
        Ok(img)
    }

    /// The whole canvas, small, for context.
    fn context_image(&self) -> Result<ImageData> {
        let buf = photocraft_compose::thumbnail_buffer(self.doc, CONTEXT_SIDE);
        self.cancelled()?;
        let rgb = self.flatten_to_srgb(&buf.px);
        let (w, h) = (buf.rect.width() as u32, buf.rect.height() as u32);
        Frame::new(w, h, "").png_rgb(&rgb, BACKGROUND)
    }

    /// Straight-alpha composite pixels over [`BACKGROUND`], in sRGB.
    fn flatten_to_srgb(&self, px: &[[f32; 4]]) -> Vec<[f32; 3]> {
        let mut flat: Vec<f32> = px.iter().flat_map(|p| over(*p)).collect();
        self.to_srgb.apply(&mut flat, 3);
        flat.as_chunks::<3>().0.to_vec()
    }

    /// `rect` of the composite, box-filtered down to `w × h` and converted to sRGB. Rendered in
    /// bands, so a large window never sits in memory at full size.
    fn render_scaled(&self, rect: Rect, w: u32, h: u32) -> Result<Vec<[f32; 3]>> {
        const BAND: i32 = 256;
        let (rw, rh) = (rect.width().max(1) as usize, rect.height().max(1) as usize);
        let (w, h) = (w.max(1) as usize, h.max(1) as usize);
        let mut sum = vec![[0.0f32; 4]; w * h];
        let mut y = rect.y0;
        while y < rect.y1 {
            self.cancelled()?;
            let band = Rect::new(rect.x0, y, rect.x1, (y + BAND).min(rect.y1));
            let buf = photocraft_compose::render(self.doc, band);
            if buf.px.len() != band.width() as usize * band.height() as usize {
                return Err(EngineError::Other("the canvas could not be rendered".into()));
            }
            for (row, line) in buf.px.chunks_exact(band.width().max(1) as usize).enumerate() {
                let sy = (y - rect.y0) as usize + row;
                let dy = (sy * h / rh).min(h - 1);
                for (sx, p) in line.iter().enumerate() {
                    let dx = (sx * w / rw).min(w - 1);
                    if let Some(acc) = sum.get_mut(dy * w + dx) {
                        let f = over(*p);
                        acc[0] += f[0];
                        acc[1] += f[1];
                        acc[2] += f[2];
                        acc[3] += 1.0;
                    }
                }
            }
            y = band.y1;
            self.ctx.progress(0.1 * (y - rect.y0) as f32 / rh as f32, "Preparing the canvas");
        }
        let mut flat: Vec<f32> = sum.iter().flat_map(|a| if a[3] > 0.0 { [a[0] / a[3], a[1] / a[3], a[2] / a[3]] } else { BACKGROUND }).collect();
        self.to_srgb.apply(&mut flat, 3);
        Ok(flat.as_chunks::<3>().0.to_vec())
    }

    /// The model's answer sampled over `out` (a part of `window`, which the sent image showed in
    /// `frame`), converted to the composite profile.
    fn resample_back(&self, img: &photocraft_codecs::Image, frame: &Frame, window: Rect, out: Rect) -> Result<Vec<[f32; 3]>> {
        let rgba = img.to_rgba8();
        let (iw, ih) = (img.width() as usize, img.height() as usize);
        if rgba.len() < iw * ih * 4 {
            return Err(EngineError::Other("the generated image is truncated".into()));
        }
        // Frame pixels → answer pixels (the model may answer at another resolution).
        let (kx, ky) = (iw as f32 / frame.width as f32, ih as f32 / frame.height as f32);
        // Window pixels → frame pixels.
        let (wx, wy) = (frame.inner_w as f32 / window.width().max(1) as f32, frame.inner_h as f32 / window.height().max(1) as f32);
        let sample = |x: f32, y: f32| -> [f32; 3] {
            let x = x.clamp(0.0, (iw - 1) as f32);
            let y = y.clamp(0.0, (ih - 1) as f32);
            let (x0, y0) = (x.floor() as usize, y.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(iw - 1), (y0 + 1).min(ih - 1));
            let (fx, fy) = (x - x0 as f32, y - y0 as f32);
            let px = |xx: usize, yy: usize, c: usize| rgba.get((yy * iw + xx) * 4 + c).map_or(0.0, |v| f32::from(*v) / 255.0);
            std::array::from_fn(|c| {
                let top = px(x0, y0, c) * (1.0 - fx) + px(x1, y0, c) * fx;
                let bottom = px(x0, y1, c) * (1.0 - fx) + px(x1, y1, c) * fx;
                top * (1.0 - fy) + bottom * fy
            })
        };
        let (ow, oh) = (out.width() as usize, out.height() as usize);
        let mut flat = Vec::with_capacity(ow * oh * 3);
        for y in 0..oh {
            if y % 256 == 0 {
                self.cancelled()?;
            }
            let fy = (frame.off_y as f32 + ((out.y0 - window.y0) as f32 + y as f32 + 0.5) * wy) * ky - 0.5;
            for x in 0..ow {
                let fx = (frame.off_x as f32 + ((out.x0 - window.x0) as f32 + x as f32 + 0.5) * wx) * kx - 0.5;
                flat.extend(sample(fx, fy));
            }
        }
        self.from_srgb.apply(&mut flat, 3);
        Ok(flat.as_chunks::<3>().0.iter().map(|c| c.map(|v| v.clamp(0.0, 1.0))).collect())
    }
}

/// A straight-alpha pixel over [`BACKGROUND`].
fn over(p: [f32; 4]) -> [f32; 3] {
    let a = if p[3].is_finite() { p[3].clamp(0.0, 1.0) } else { 0.0 };
    std::array::from_fn(|i| {
        let v = if p[i].is_finite() { p[i] } else { 0.0 };
        v * a + BACKGROUND[i] * (1.0 - a)
    })
}

/// The part of the canvas sent for a selection with bounds `area`: the selection with room
/// around it (so the model sees its surroundings), at least [`MIN_CROP_SIDE`] where the canvas
/// allows. A window that would cover most of the canvas becomes the whole canvas.
fn edit_window(area: Rect, bounds: Rect) -> Rect {
    let margin = i32::try_from((area.width().max(area.height()) / 2).max(48)).unwrap_or(i32::MAX / 4);
    let mut w = area.inflate(margin).intersect(&bounds);
    // Grow a small window around its centre (shifted back inside the canvas).
    let grow = |lo: i32, hi: i32, min_lo: i32, max_hi: i32| -> (i32, i32) {
        let want = MIN_CROP_SIDE.min(max_hi - min_lo);
        if hi - lo >= want {
            return (lo, hi);
        }
        let mid = lo + (hi - lo) / 2;
        let lo = (mid - want / 2).clamp(min_lo, max_hi - want);
        (lo, lo + want)
    };
    let (x0, x1) = grow(w.x0, w.x1, bounds.x0, bounds.x1);
    let (y0, y1) = grow(w.y0, w.y1, bounds.y0, bounds.y1);
    w = Rect::new(x0, y0, x1, y1);
    let (wa, ba) = (w.width() as u64 * w.height() as u64, bounds.width() as u64 * bounds.height() as u64);
    if wa * 10 >= ba * 6 { bounds } else { w }
}

/// `w × h` scaled down (never up) so the longest side is at most `max`.
fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    let long = w.max(h).max(1);
    if long <= max {
        return (w.max(1), h.max(1));
    }
    let k = max as f64 / long as f64;
    (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1))
}

/// A `w × h` scalar image box-filtered to `nw × nh`.
fn bin_scalar(v: &[f32], w: usize, h: usize, nw: usize, nh: usize) -> Vec<f32> {
    let (nw, nh) = (nw.max(1), nh.max(1));
    let mut sum = vec![(0.0f32, 0u32); nw * nh];
    if w == 0 || h == 0 {
        return vec![0.0; nw * nh];
    }
    for (i, x) in v.iter().take(w * h).enumerate() {
        let (sx, sy) = (i % w, i / w);
        let (dx, dy) = ((sx * nw / w).min(nw - 1), (sy * nh / h).min(nh - 1));
        if let Some(s) = sum.get_mut(dy * nw + dx) {
            s.0 += x;
            s.1 += 1;
        }
    }
    sum.into_iter().map(|(s, n)| if n > 0 { s / n as f32 } else { 0.0 }).collect()
}

/// The image sent: the edited area (`inner_w × inner_h`) centred on a canvas padded to one of
/// the API's aspect ratios, so the answer maps back exactly.
struct Frame {
    width: u32,
    height: u32,
    inner_w: u32,
    inner_h: u32,
    off_x: u32,
    off_y: u32,
}

impl Frame {
    /// `ratio` like "16:9" (empty = no padding).
    fn new(w: u32, h: u32, ratio: &str) -> Self {
        let (w, h) = (w.max(1), h.max(1));
        let parsed = ratio.split_once(':').and_then(|(a, b)| Some((a.parse::<f64>().ok()?, b.parse::<f64>().ok()?))).filter(|(a, b)| *a > 0.0 && *b > 0.0);
        let (fw, fh) = match parsed {
            Some((a, b)) => {
                let r = a / b;
                if f64::from(w) / f64::from(h) < r {
                    (((f64::from(h) * r).round() as u32).max(w), h)
                } else {
                    (w, ((f64::from(w) / r).round() as u32).max(h))
                }
            }
            None => (w, h),
        };
        Self { width: fw, height: fh, inner_w: w, inner_h: h, off_x: (fw - w) / 2, off_y: (fh - h) / 2 }
    }

    fn png(&self, layout: photocraft_codecs::ChannelLayout, data: Vec<u8>) -> Result<ImageData> {
        let image = photocraft_codecs::Image::from_u8(self.width, self.height, layout, data).map_err(|e| EngineError::Other(format!("could not encode the canvas: {e}")))?;
        let png = photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default())
            .map_err(|e| EngineError::Other(format!("could not encode the canvas: {e}")))?;
        Ok(ImageData::png(png))
    }

    /// `rgb` (`inner_w × inner_h`, sRGB) on a `pad`-coloured frame.
    fn png_rgb(&self, rgb: &[[f32; 3]], pad: [f32; 3]) -> Result<ImageData> {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let mut data = Vec::with_capacity(self.width as usize * self.height as usize * 3);
        for y in 0..self.height {
            for x in 0..self.width {
                let inside = x.checked_sub(self.off_x).filter(|ix| *ix < self.inner_w).zip(y.checked_sub(self.off_y).filter(|iy| *iy < self.inner_h));
                let c = inside.and_then(|(ix, iy)| rgb.get(iy as usize * self.inner_w as usize + ix as usize)).copied().unwrap_or(pad);
                data.extend(c.map(q));
            }
        }
        self.png(photocraft_codecs::ChannelLayout::Rgb, data)
    }

    /// The mask (white = repaint) on a black frame.
    fn png_mask(&self, cover: &[f32]) -> Result<ImageData> {
        let mut data = Vec::with_capacity(self.width as usize * self.height as usize);
        for y in 0..self.height {
            for x in 0..self.width {
                let inside = x.checked_sub(self.off_x).filter(|ix| *ix < self.inner_w).zip(y.checked_sub(self.off_y).filter(|iy| *iy < self.inner_h));
                let c = inside.and_then(|(ix, iy)| cover.get(iy as usize * self.inner_w as usize + ix as usize)).copied().unwrap_or(0.0);
                data.push((c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            }
        }
        self.png(photocraft_codecs::ChannelLayout::Gray, data)
    }
}

/// Write the result (the job's apply step), as one undo step.
fn apply(s: &mut Session, label: &str, output: Output, target: Option<LayerId>, fmt: PixelFormat, g: Generated) -> Result<Value> {
    let Generated { rect, rgb, cover, model } = g;
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    if rgb.len() != w * h || cover.len() != w * h {
        return Err(EngineError::Other("internal error: the generated pixels don't match the area".into()));
    }
    let layer = s.edit(label, |doc, active| {
        match output {
            Output::New => {
                let lf = PixelFormat { alpha: true, ..fmt };
                let mut l = Layer::raster(doc.next_layer_name(label), lf);
                let ln = lf.channels();
                let mut px = vec![0.0f32; w * h * ln];
                let mut enc = [0.0f32; 8];
                for ((dst, c), cv) in px.chunks_exact_mut(ln).zip(&rgb).zip(&cover) {
                    if *cv > 0.0 {
                        let n = from_rgba_into(&lf, [c[0], c[1], c[2], *cv], &mut enc).min(ln);
                        if let (Some(d), Some(e)) = (dst.get_mut(..n), enc.get(..n)) {
                            d.copy_from_slice(e);
                        }
                    }
                }
                let surf = crate::pixels_mut(&mut l)?;
                surf.write_region(rect, &px);
                surf.prune();
                // An art pass is a flattened image: on top, so nothing above it is shown twice.
                let above = if cover.iter().all(|c| *c >= 1.0) && w * h > 0 { doc.top_layer() } else { target };
                let nid = doc.insert_above(above, l);
                *active = Some(nid);
                Ok(nid)
            }
            Output::Current => {
                let id = target.ok_or(EngineError::Other("no active layer".into()))?;
                let locks = doc.effective_locks(id);
                let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
                if locks.all || locks.pixels {
                    return Err(EngineError::Other(format!("Could not complete your request because the layer \"{}\" is locked", l.name)));
                }
                let surf = l.surface_mut().ok_or_else(|| EngineError::Other("not a pixel layer".into()))?;
                let lf = surf.format();
                let n = lf.channels();
                let none = || false;
                let mut px = crate::fill_cmds::read_window(surf, rect, &none).ok_or(EngineError::Cancelled)?;
                let mut enc = [0.0f32; 8];
                for ((dst, c), cv) in px.chunks_exact_mut(n).zip(&rgb).zip(&cover) {
                    if *cv <= 0.0 {
                        continue;
                    }
                    let k = from_rgba_into(&lf, [c[0], c[1], c[2], 1.0], &mut enc).min(n);
                    // The original blended towards the result by the coverage (feathered edges).
                    for (d, e) in dst.iter_mut().zip(enc.iter().take(k)) {
                        *d += (*e - *d) * cv.min(1.0);
                    }
                }
                surf.write_region(rect, &px);
                Ok(id)
            }
        }
    })?;
    Ok(json!({"layer": layer.0, "window": [rect.x0, rect.y0, rect.width(), rect.height()], "model": model}))
}

#[cfg(test)]
#[path = "generative_cmds_tests.rs"]
mod tests;
