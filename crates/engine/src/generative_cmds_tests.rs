use std::sync::{Arc, Mutex};

use photocraft_genai::{EditRequest, GenError, ImageData};
use serde_json::json;

use super::*;

/// A model that paints its whole answer one colour and remembers what it was sent.
struct Solid {
    color: [u8; 3],
    size: (u32, u32),
    seen: Mutex<Vec<EditRequest>>,
    answer: Option<Vec<u8>>,
}

impl Solid {
    fn new(color: [u8; 3]) -> Arc<Self> {
        Arc::new(Self { color, size: (300, 200), seen: Mutex::new(Vec::new()), answer: None })
    }
}

impl ImageGenerator for Solid {
    fn describe(&self) -> String {
        "Test model".into()
    }
    fn generate(&self, req: &EditRequest, _: &dyn Fn() -> bool) -> std::result::Result<ImageData, GenError> {
        self.seen.lock().unwrap().push(req.clone());
        if let Some(a) = &self.answer {
            return Ok(ImageData { mime: "image/png".into(), bytes: a.clone() });
        }
        let (w, h) = self.size;
        let data: Vec<u8> = (0..w * h).flat_map(|_| self.color).collect();
        let img = photocraft_codecs::Image::from_u8(w, h, photocraft_codecs::ChannelLayout::Rgb, data).unwrap();
        Ok(ImageData::png(photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap()))
    }
}

struct Failing;
impl ImageGenerator for Failing {
    fn describe(&self) -> String {
        "Failing".into()
    }
    fn generate(&self, _: &EditRequest, _: &dyn Fn() -> bool) -> std::result::Result<ImageData, GenError> {
        Err(GenError::Quota("try later".into()))
    }
}

/// A `w × h` document at `depth` whose background is red, with an empty layer on top (active).
fn red_doc(w: u32, h: u32, depth: u32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": w, "height": h, "depth": depth})).unwrap();
    let bg = s.active().unwrap().active_layer.unwrap();
    s.edit("paint red", |doc, _| {
        let rect = doc.bounds();
        let surf = doc.layer_mut(bg).unwrap().surface_mut().unwrap();
        let f = surf.format();
        let n = f.channels();
        let mut enc = [0.0f32; 8];
        from_rgba_into(&f, [1.0, 0.0, 0.0, 1.0], &mut enc);
        let px: Vec<f32> = (0..rect.width() * rect.height()).flat_map(|_| enc[..n].to_vec()).collect();
        surf.write_region(rect, &px);
        Ok(())
    })
    .unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s
}

/// RGBA of `layer` at (x, y).
fn pixel(s: &Session, layer: LayerId, x: i32, y: i32) -> [f32; 4] {
    let surf = s.active().unwrap().doc.layer(layer).unwrap().surface().unwrap();
    let px = crate::fill_cmds::read_window(surf, Rect::new(x, y, x + 1, y + 1), &|| false).unwrap();
    photocraft_raster::to_rgba(&surf.format(), &px)
}

fn decode(img: &ImageData) -> photocraft_codecs::Image {
    photocraft_codecs::decode(&img.bytes).unwrap()
}

#[test]
fn fill_paints_only_the_selection_into_the_active_layer() {
    let mut s = red_doc(80, 60, 8);
    let top = s.active().unwrap().active_layer.unwrap();
    s.execute("select.rect", json!({"x": 20, "y": 10, "width": 30, "height": 20})).unwrap();
    let gen_ = Solid::new([0, 0, 255]);
    s.set_image_generator(Some(gen_.clone()));
    let r = s.execute(COMMAND, json!({"prompt": "a metallic ship hull"})).unwrap();
    assert_eq!(r["layer"], json!(top.0));
    assert_eq!(r["window"], json!([20, 10, 30, 20]));
    assert_eq!(r["model"], "Test model");
    // Inside: the model's blue, opaque, in the layer that was active.
    let inside = pixel(&s, top, 30, 20);
    assert!(inside[2] > 0.99 && inside[0] < 0.01 && inside[3] > 0.99, "{inside:?}");
    // Outside: untouched (still transparent), though the model painted its whole answer blue.
    assert_eq!(pixel(&s, top, 5, 5)[3], 0.0);
    assert_eq!(pixel(&s, top, 60, 40)[3], 0.0);
    // One undo step restores the layer.
    assert_eq!(s.active().unwrap().history.undo_label(), Some("Generative Fill"));
    // The model saw the flattened canvas (the red background under the empty active layer),
    // the selection as a mask, and the prompt.
    let seen = gen_.seen.lock().unwrap();
    let req = &seen[0];
    assert_eq!(req.images.len(), 2, "the small canvas is sent whole: no context image");
    assert!(req.instruction.contains("a metallic ship hull"));
    let canvas = decode(&req.images[0]);
    let rgba = canvas.to_rgba8();
    assert_eq!(&rgba[..3], &[255, 0, 0], "the composite, not the empty active layer");
    let mask = decode(&req.images[1]).to_rgba8();
    let (mw, mh) = (canvas.width() as usize, canvas.height() as usize);
    let at = |x: usize, y: usize| mask[(y * mw + x) * 4];
    // Padded to an API aspect ratio, the selection's centre is white and the corners black.
    let (ox, oy) = ((mw - 80) / 2, (mh - 60) / 2);
    assert_eq!(at(ox + 35, oy + 20), 255);
    assert_eq!(at(0, 0), 0);
    assert_eq!(at(mw - 1, mh - 1), 0);
    assert!(req.aspect_ratio.is_some() && req.image_size == Some("1K"));
}

#[test]
fn a_feathered_selection_blends_its_edge() {
    let mut s = red_doc(64, 64, 8);
    let top = s.active().unwrap().active_layer.unwrap();
    // The layer holds green; a soft-edged selection.
    s.edit("green", |doc, _| {
        let surf = doc.layer_mut(top).unwrap().surface_mut().unwrap();
        let f = surf.format();
        let mut enc = [0.0f32; 8];
        let n = from_rgba_into(&f, [0.0, 1.0, 0.0, 1.0], &mut enc);
        surf.write_region(Rect::new(0, 0, 64, 64), &(0..64 * 64).flat_map(|_| enc[..n].to_vec()).collect::<Vec<_>>());
        Ok(())
    })
    .unwrap();
    s.execute("select.rect", json!({"x": 16, "y": 16, "width": 32, "height": 32})).unwrap();
    s.execute("select.modify.feather", json!({"radius": 4})).unwrap();
    s.set_image_generator(Some(Solid::new([0, 0, 255])));
    s.execute(COMMAND, json!({"prompt": "blue"})).unwrap();
    let centre = pixel(&s, top, 32, 32);
    assert!(centre[2] > 0.95 && centre[1] < 0.05, "{centre:?}");
    // On the feathered edge: part green, part blue.
    let edge = pixel(&s, top, 16, 32);
    assert!(edge[1] > 0.1 && edge[2] > 0.1, "{edge:?}");
    // Far outside: still green.
    assert_eq!(pixel(&s, top, 2, 2), [0.0, 1.0, 0.0, 1.0]);
}

#[test]
fn no_selection_is_an_art_pass_on_a_new_top_layer() {
    let mut s = red_doc(50, 40, 8);
    // Make the background active: the art pass still goes on top of everything.
    let bg = s.active().unwrap().doc.layers[0].id;
    s.active_mut().unwrap().active_layer = Some(bg);
    let gen_ = Solid::new([0, 255, 0]);
    s.set_image_generator(Some(gen_.clone()));
    let r = s.execute(COMMAND, json!({"prompt": "warmer light"})).unwrap();
    let id = LayerId(r["layer"].as_u64().unwrap());
    let st = s.active().unwrap();
    assert_eq!(st.doc.top_layer(), Some(id));
    assert!(st.doc.layer(id).unwrap().name.starts_with("Generative Art Pass"));
    assert_eq!(st.active_layer, Some(id));
    let p = pixel(&s, id, 25, 20);
    assert!(p[1] > 0.99 && p[3] > 0.99, "{p:?}");
    let seen = gen_.seen.lock().unwrap();
    assert_eq!(seen[0].images.len(), 1);
    assert!(seen[0].instruction.contains("art pass") && seen[0].instruction.contains("warmer light"));
}

#[test]
fn a_small_selection_on_a_large_canvas_sends_a_crop_and_the_whole_canvas() {
    let mut s = red_doc(3000, 2000, 8);
    s.execute("select.rect", json!({"x": 100, "y": 100, "width": 40, "height": 40})).unwrap();
    let gen_ = Solid::new([0, 0, 255]);
    s.set_image_generator(Some(gen_.clone()));
    s.execute(COMMAND, json!({"prompt": "a lamp"})).unwrap();
    let seen = gen_.seen.lock().unwrap();
    let req = &seen[0];
    assert_eq!(req.images.len(), 3);
    assert!(req.instruction.contains("image 3"));
    let crop = decode(&req.images[0]);
    assert!(crop.width() >= 1024 && crop.height() >= 1024, "{}×{}", crop.width(), crop.height());
    let ctx = decode(&req.images[2]);
    assert_eq!(ctx.width().max(ctx.height()), CONTEXT_SIDE);
    let top = s.active().unwrap().active_layer.unwrap();
    assert!(pixel(&s, top, 120, 120)[2] > 0.99);
    assert_eq!(pixel(&s, top, 90, 90)[3], 0.0);
}

#[test]
fn deep_documents_take_the_result() {
    for depth in [16, 32] {
        let mut s = red_doc(40, 30, depth);
        let top = s.active().unwrap().active_layer.unwrap();
        s.execute("select.rect", json!({"x": 5, "y": 5, "width": 10, "height": 10})).unwrap();
        s.set_image_generator(Some(Solid::new([0, 0, 255])));
        s.execute(COMMAND, json!({"prompt": "blue"})).unwrap();
        let p = pixel(&s, top, 8, 8);
        assert!(p[2] > 0.9 && p[0] < 0.1 && p[3] > 0.99, "{depth}-bit: {p:?}");
        assert_eq!(pixel(&s, top, 30, 20)[3], 0.0, "{depth}-bit outside");
    }
}

#[test]
fn bad_input_and_failing_models_are_errors_and_change_nothing() {
    let mut s = red_doc(40, 30, 8);
    s.execute("select.rect", json!({"x": 5, "y": 5, "width": 10, "height": 10})).unwrap();
    // No model installed.
    let e = s.execute(COMMAND, json!({"prompt": "x"})).unwrap_err().to_string();
    assert!(e.contains("API key"), "{e}");
    s.set_image_generator(Some(Solid::new([0, 0, 255])));
    for p in [json!({}), json!({"prompt": "   "}), json!({"prompt": 5}), json!({"prompt": "x", "output": "elsewhere"}), json!({"prompt": "y".repeat(2001)})] {
        assert!(s.execute(COMMAND, p.clone()).is_err(), "{p}");
    }
    // The model fails, or answers with something that isn't an image.
    s.set_image_generator(Some(Arc::new(Failing)));
    let e = s.execute(COMMAND, json!({"prompt": "x"})).unwrap_err().to_string();
    assert!(e.contains("quota"), "{e}");
    for answer in [vec![], b"not an image".to_vec(), vec![0x89, b'P', b'N', b'G', 0, 0]] {
        s.set_image_generator(Some(Arc::new(Solid { answer: Some(answer), ..Arc::try_unwrap(Solid::new([0; 3])).ok().unwrap() })));
        assert!(s.execute(COMMAND, json!({"prompt": "x"})).is_err());
    }
    // The last step is still the selection: nothing was recorded.
    assert_eq!(s.active().unwrap().history.undo_label(), Some("Rectangular Marquee"));
    let top = s.active().unwrap().active_layer.unwrap();
    assert_eq!(pixel(&s, top, 8, 8)[3], 0.0, "nothing was written");
    // A selection outside the canvas, and a locked layer.
    s.set_image_generator(Some(Solid::new([0, 0, 255])));
    s.execute("layer.lockLayers", json!({"pixels": true})).unwrap();
    assert!(s.execute(COMMAND, json!({"prompt": "x"})).is_err(), "a locked layer");
    // An art pass into a new layer still works on a locked active layer.
    s.execute("select.deselect", json!({})).unwrap();
    assert!(s.execute(COMMAND, json!({"prompt": "x"})).is_ok());
}

#[test]
fn edit_windows_and_frames() {
    let canvas = Rect::new(0, 0, 4000, 3000);
    // A small selection gets a 1024² window around it, inside the canvas.
    let w = edit_window(Rect::new(10, 10, 50, 50), canvas);
    assert_eq!((w.width(), w.height()), (1024, 1024));
    assert!(w.x0 >= 0 && w.y0 >= 0);
    // A big one becomes the whole canvas.
    assert_eq!(edit_window(Rect::new(100, 100, 3500, 2800), canvas), canvas);
    // A small canvas is always sent whole.
    let small = Rect::new(0, 0, 300, 200);
    assert_eq!(edit_window(Rect::new(10, 10, 20, 20), small), small);
    assert_eq!(fit(4000, 3000, 2048), (2048, 1536));
    assert_eq!(fit(100, 50, 2048), (100, 50));
    // 4:3 inside a 16:9 frame is padded left and right.
    let f = Frame::new(1200, 900, "16:9");
    assert_eq!((f.width, f.height, f.off_x, f.off_y), (1600, 900, 200, 0));
    let f = Frame::new(900, 1200, "1:1");
    assert_eq!((f.width, f.height, f.off_x, f.off_y), (1200, 1200, 150, 0));
    let f = Frame::new(5, 5, "junk");
    assert_eq!((f.width, f.height), (5, 5));
    assert_eq!(bin_scalar(&[1.0, 0.0, 1.0, 0.0], 2, 2, 1, 1), vec![0.5]);
}

#[test]
fn diffusion_backends_get_the_prompt_a_pixel_mask_and_the_strength() {
    let mut s = red_doc(80, 60, 8);
    s.execute("select.rect", json!({"x": 20, "y": 10, "width": 30, "height": 20})).unwrap();
    let gen_ = Solid::new([0, 0, 255]);
    s.set_image_generator(Some(gen_.clone()));
    s.execute(COMMAND, json!({"prompt": "  rusty hull ", "strength": 0.7})).unwrap();
    // No selection: an art pass sends an all-white mask and the gentler default strength.
    s.execute("select.deselect", json!({})).unwrap();
    s.execute(COMMAND, json!({"prompt": "warm light"})).unwrap();
    let seen = gen_.seen.lock().unwrap();
    let (fill, art) = (&seen[0], &seen[1]);
    assert_eq!(fill.prompt, "rusty hull");
    assert_eq!(fill.strength, 0.7);
    assert_eq!(fill.source.as_ref(), Some(&fill.images[0]));
    assert_eq!(fill.mask.as_ref(), Some(&fill.images[1]));
    assert_eq!(art.strength, 0.45);
    let mask = decode(art.mask.as_ref().unwrap());
    let px = mask.to_rgba8();
    let (mw, mh) = (mask.width() as usize, mask.height() as usize);
    assert_eq!(px[((mh / 2) * mw + mw / 2) * 4], 255, "the whole picture may change");
    drop(seen);
    for bad in [json!(-0.1), json!(1.5), json!("strong")] {
        assert!(s.execute(COMMAND, json!({"prompt": "x", "strength": bad})).is_err(), "{bad}");
    }
}

/// Against a real ComfyUI with an SDXL inpainting checkpoint:
/// `PHOTOCRAFT_COMFYUI_LIVE=1 cargo test -p photocraft-engine --lib live_comfyui -- --ignored --nocapture`
#[test]
#[ignore = "needs a running ComfyUI"]
fn live_comfyui_fills_a_selection() {
    if std::env::var_os("PHOTOCRAFT_COMFYUI_LIVE").is_none() {
        return;
    }
    let url = std::env::var("PHOTOCRAFT_COMFYUI_URL").unwrap_or_else(|_| photocraft_genai::comfy::DEFAULT_URL.into());
    let comfy = photocraft_genai::comfy::ComfyUi::new(&url, photocraft_genai::comfy::Workflow::SdxlInpaint { checkpoint: String::new() }).unwrap();
    let mut s = red_doc(512, 384, 8);
    let top = s.active().unwrap().active_layer.unwrap();
    s.execute("select.rect", json!({"x": 160, "y": 112, "width": 192, "height": 160})).unwrap();
    s.set_image_generator(Some(Arc::new(comfy)));
    let t = std::time::Instant::now();
    let r = s.execute(COMMAND, json!({"prompt": "a bright yellow sunflower, detailed petals"})).unwrap();
    println!("ComfyUI fill took {:.1}s: {r}", t.elapsed().as_secs_f32());
    let inside = pixel(&s, top, 256, 192);
    println!("centre pixel {inside:?}");
    assert!(inside[3] > 0.99, "the selection was painted");
    assert_eq!(pixel(&s, top, 10, 10)[3], 0.0, "outside untouched");
}
