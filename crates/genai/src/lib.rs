//! Generative image editing for PhotoCraft: a client for Google's Gemini image models ("Nano
//! Banana") through the Gemini API's `models.generateContent` endpoint.
//!
//! The crate knows nothing about documents. The engine renders the canvas and the selection
//! mask to PNGs; this crate wraps them with an instruction ([`fill_instruction`],
//! [`art_pass_instruction`]), builds the request ([`request_body`]), and reads the edited image
//! back out of the response ([`parse_response`]). Gemini has no pixel-mask parameter, so the mask
//! is sent as a second image and described in the instruction; the engine then keeps only the
//! masked pixels of the result.
//!
//! The HTTPS transport ([`Gemini::generate`]) needs the `http` feature and a native target (`ureq`,
//! rustls); without it `generate` returns an error. Everything else is plain data and builds for
//! wasm.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use base64::Engine as _;
use serde_json::{Value, json};

/// The model used when the preference is empty: Nano Banana 2.1, Google's recommended
/// image-editing workhorse (checked against ai.google.dev, October 2026).
pub const DEFAULT_MODEL: &str = "gemini-nano-banana-2.1";

/// The Gemini image models offered in Preferences › Integrations, best first: Nano Banana 2.1,
/// Nano Banana Pro, Nano Banana 2, Nano Banana 2 Lite (fastest, 1K only) and the legacy Nano Banana.
pub const MODEL_IDS: &[&str] = &["gemini-nano-banana-2.1", "gemini-3-pro-image", "gemini-3.1-flash-image", "gemini-3.1-flash-lite-image", "gemini-2.5-flash-image"];

/// API base URL.
pub const ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/models";

/// Largest response body read (a 4K PNG in base64 is well under this).
pub const MAX_RESPONSE_BYTES: u64 = 96 * 1024 * 1024;

/// Longest prompt accepted from the user.
pub const MAX_PROMPT_CHARS: usize = 2000;

/// An image sent to or returned by the model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageData {
    /// IANA type such as `image/png`.
    pub mime: String,
    pub bytes: Vec<u8>,
}

impl ImageData {
    pub fn png(bytes: Vec<u8>) -> Self {
        Self { mime: "image/png".into(), bytes }
    }
}

/// One image-editing request. It carries the same edit two ways: Gemini reads `instruction` and
/// `images` (the mask described in words); a diffusion backend (ComfyUI) reads the user's own
/// `prompt`, the `source` image, its pixel `mask` and the `strength`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditRequest {
    /// The full instruction text (see [`fill_instruction`] / [`art_pass_instruction`]).
    pub instruction: String,
    /// Images in the order the instruction refers to them.
    pub images: Vec<ImageData>,
    /// One of the API's aspect ratios ([`nearest_aspect_ratio`]); `None` lets the model follow
    /// the first image.
    pub aspect_ratio: Option<&'static str>,
    /// `512`, `1K`, `2K` or `4K` ([`image_size_for`]).
    pub image_size: Option<&'static str>,
    /// The user's prompt as typed.
    pub prompt: String,
    /// The image to edit (the first of `images`).
    pub source: Option<ImageData>,
    /// Its mask, the same size, white where to repaint; `None` repaints everything (art pass).
    pub mask: Option<ImageData>,
    /// How far a diffusion model may move from the source, 0..1 (1 = repaint fully).
    pub strength: f32,
}

/// Why a request failed, worded for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GenError {
    /// No key, or the key was rejected (401/403).
    Auth(String),
    /// Rate limit or quota (429).
    Quota(String),
    /// The prompt or the result was blocked by Google's safety filters.
    Blocked(String),
    /// The model answered without an image (it may have replied in text).
    NoImage(String),
    /// Any other HTTP error status.
    Http { status: u16, message: String },
    /// Network failure or timeout.
    Transport(String),
    /// The response could not be read.
    BadResponse(String),
    /// A parameter was invalid before anything was sent.
    Invalid(String),
}

impl std::fmt::Display for GenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenError::Auth(m) => write!(f, "Gemini rejected the API key: {m}"),
            GenError::Quota(m) => write!(f, "Gemini quota or rate limit reached: {m}"),
            GenError::Blocked(m) => write!(f, "Gemini blocked the request: {m}"),
            GenError::NoImage(m) => write!(f, "Gemini returned no image{}", if m.is_empty() { String::new() } else { format!(": {m}") }),
            GenError::Http { status, message } => write!(f, "Gemini request failed ({status}): {message}"),
            GenError::Transport(m) => write!(f, "Could not reach Gemini: {m}"),
            GenError::BadResponse(m) => write!(f, "Unexpected answer from Gemini: {m}"),
            GenError::Invalid(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for GenError {}

// ------------------------------------------------------------------ instructions

/// The instruction for repainting the masked area. Image 1 is what to edit, image 2 its mask
/// (white = repaint); with `has_context`, image 3 is the whole canvas the crop was taken from.
pub fn fill_instruction(prompt: &str, has_context: bool) -> String {
    let prompt = prompt.trim();
    let context = if has_context {
        " Image 1 is a crop of a larger painting; image 3 shows the whole painting for context, so the \
         edit fits the scene. Return only the edited version of image 1."
    } else {
        ""
    };
    format!(
        "You are retouching an image in a professional image editor. Image 1 is the image to edit. \
         Image 2 is a mask of the same size: white marks the area to repaint, black marks pixels that \
         must stay exactly as they are.{context}\n\n\
         Apply this change to the white area only: {prompt}\n\n\
         Match the surrounding style, medium, brushwork, colours, lighting, perspective and level of \
         detail so the edit blends seamlessly with its edges. Keep the framing, size and aspect ratio of \
         image 1, leave everything outside the white area unchanged, and add no borders, text or \
         watermarks."
    )
}

/// The instruction for an art pass over the whole image (no selection).
pub fn art_pass_instruction(prompt: &str) -> String {
    let prompt = prompt.trim();
    format!(
        "You are doing an art pass over a work-in-progress image in a professional image editor. \
         Image 1 is the current canvas.\n\n\
         Apply this to the image: {prompt}\n\n\
         Keep the composition, subjects, shapes, poses, framing and aspect ratio exactly as they are; \
         refine the rendering, lighting, colour and detail. Add no borders, text or watermarks."
    )
}

// ------------------------------------------------------------------ sizes

/// The API's aspect ratios, as (label, width / height).
const ASPECTS: &[(&str, f64)] = &[
    ("1:1", 1.0),
    ("4:5", 0.8),
    ("5:4", 1.25),
    ("3:4", 0.75),
    ("4:3", 4.0 / 3.0),
    ("2:3", 2.0 / 3.0),
    ("3:2", 1.5),
    ("9:16", 9.0 / 16.0),
    ("16:9", 16.0 / 9.0),
    ("21:9", 21.0 / 9.0),
    ("1:4", 0.25),
    ("4:1", 4.0),
    ("1:8", 0.125),
    ("8:1", 8.0),
];

/// The supported aspect ratio closest (in log space) to `w × h`; `1:1` for an empty size.
pub fn nearest_aspect_ratio(w: u32, h: u32) -> &'static str {
    if w == 0 || h == 0 {
        return "1:1";
    }
    let r = (f64::from(w) / f64::from(h)).ln();
    ASPECTS.iter().map(|(l, a)| (*l, (a.ln() - r).abs())).fold(("1:1", f64::INFINITY), |best, c| if c.1 < best.1 { c } else { best }).0
}

/// The output size to ask for, given the longest side of the area being edited.
pub fn image_size_for(longest_side: u32) -> &'static str {
    match longest_side {
        0..=1100 => "1K",
        1101..=2200 => "2K",
        _ => "4K",
    }
}

// ------------------------------------------------------------------ request / response

/// Whether `model` is a plausible model id (it goes into the URL path).
pub fn valid_model(model: &str) -> bool {
    !model.is_empty() && model.len() <= 128 && model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
}

/// The JSON body for `models/{model}:generateContent`.
pub fn request_body(req: &EditRequest) -> Value {
    // (Gemini has no strength or pixel mask: `prompt`, `source`, `mask` are for diffusion backends.)
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut parts = vec![json!({"text": req.instruction})];
    parts.extend(req.images.iter().map(|i| json!({"inlineData": {"mimeType": i.mime, "data": b64.encode(&i.bytes)}})));
    let mut image_config = serde_json::Map::new();
    if let Some(a) = req.aspect_ratio {
        image_config.insert("aspectRatio".into(), json!(a));
    }
    if let Some(s) = req.image_size {
        image_config.insert("imageSize".into(), json!(s));
    }
    let mut config = json!({"responseModalities": ["IMAGE"]});
    if !image_config.is_empty()
        && let Some(c) = config.as_object_mut()
    {
        c.insert("imageConfig".into(), Value::Object(image_config));
    }
    json!({"contents": [{"role": "user", "parts": parts}], "generationConfig": config})
}

/// The last image in a `generateContent` response (thinking models may send interim images
/// first; the last one is the final render).
pub fn parse_response(body: &[u8]) -> Result<ImageData, GenError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| GenError::BadResponse(e.to_string()))?;
    if let Some(reason) = v.pointer("/promptFeedback/blockReason").and_then(Value::as_str) {
        return Err(GenError::Blocked(format!("the prompt was blocked ({reason})")));
    }
    let candidates = v.get("candidates").and_then(Value::as_array).ok_or_else(|| GenError::NoImage("the response had no candidates".into()))?;
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut text = String::new();
    let mut last: Option<ImageData> = None;
    let mut finish = None;
    for c in candidates {
        finish = c.get("finishReason").and_then(Value::as_str).or(finish);
        for part in c.pointer("/content/parts").and_then(Value::as_array).into_iter().flatten() {
            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            if let Some(d) = part.get("inlineData").or_else(|| part.get("inline_data")) {
                let mime = d.get("mimeType").or_else(|| d.get("mime_type")).and_then(Value::as_str).unwrap_or("image/png");
                let Some(data) = d.get("data").and_then(Value::as_str) else { continue };
                if !mime.starts_with("image/") {
                    continue;
                }
                let bytes = b64.decode(data).map_err(|e| GenError::BadResponse(format!("image data: {e}")))?;
                last = Some(ImageData { mime: mime.to_string(), bytes });
            } else if let Some(t) = part.get("text").and_then(Value::as_str)
                && text.len() < 400
            {
                text.push_str(t.trim());
            }
        }
    }
    if let Some(img) = last {
        return Ok(img);
    }
    match finish {
        Some(r @ ("SAFETY" | "IMAGE_SAFETY" | "PROHIBITED_CONTENT" | "IMAGE_PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII" | "RECITATION")) => {
            Err(GenError::Blocked(format!("the result was blocked ({r})")))
        }
        _ => Err(GenError::NoImage(shorten(&text, 300))),
    }
}

/// The error for a non-success HTTP status, with the API's own message when it sent one.
pub fn error_for_status(status: u16, body: &[u8]) -> GenError {
    let message = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.pointer("/error/message").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| shorten(&String::from_utf8_lossy(body), 300));
    match status {
        401 | 403 => GenError::Auth(message),
        429 => GenError::Quota(message),
        _ => GenError::Http { status, message },
    }
}

fn shorten(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", s.get(..i).unwrap_or(s)),
        None => s.to_string(),
    }
}

// ------------------------------------------------------------------ transport

/// A Gemini API client (API key + model).
#[derive(Clone)]
pub struct Gemini {
    // Read only by the transport (`http` feature).
    #[cfg_attr(not(all(feature = "http", not(target_arch = "wasm32"))), allow(dead_code))]
    api_key: String,
    model: String,
    timeout: std::time::Duration,
}

impl std::fmt::Debug for Gemini {
    // The key is never printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gemini").field("model", &self.model).field("timeout", &self.timeout).finish_non_exhaustive()
    }
}

impl Gemini {
    /// A client for `model` (empty = [`DEFAULT_MODEL`]).
    pub fn new(api_key: impl Into<String>, model: &str) -> Result<Self, GenError> {
        let api_key: String = api_key.into().trim().to_string();
        if api_key.is_empty() {
            return Err(GenError::Auth("no API key is set".into()));
        }
        let model = model.trim();
        let model = if model.is_empty() { DEFAULT_MODEL } else { model };
        let model = model.strip_prefix("models/").unwrap_or(model);
        if !valid_model(model) {
            return Err(GenError::Invalid(format!("`{model}` is not a valid Gemini model id")));
        }
        Ok(Self { api_key, model: model.to_string(), timeout: std::time::Duration::from_secs(180) })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// The request URL.
    pub fn url(&self) -> String {
        format!("{ENDPOINT}/{}:generateContent", self.model)
    }

    /// Send `req` and wait for the edited image (blocking; run it off the UI thread).
    #[cfg(all(feature = "http", not(target_arch = "wasm32")))]
    pub fn generate(&self, req: &EditRequest) -> Result<ImageData, GenError> {
        let body = request_body(req).to_string();
        let agent: ureq::Agent =
            ureq::Agent::config_builder().timeout_global(Some(self.timeout)).http_status_as_error(false).user_agent("PhotoCraft").build().into();
        let mut resp = agent
            .post(&self.url())
            .header("x-goog-api-key", &self.api_key)
            .header("Content-Type", "application/json")
            .send(body)
            .map_err(|e| GenError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let bytes = resp.body_mut().with_config().limit(MAX_RESPONSE_BYTES).read_to_vec().map_err(|e| GenError::Transport(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(error_for_status(status, &bytes));
        }
        parse_response(&bytes)
    }

    /// Without the transport (the web build, headless tools).
    #[cfg(not(all(feature = "http", not(target_arch = "wasm32"))))]
    pub fn generate(&self, _req: &EditRequest) -> Result<ImageData, GenError> {
        Err(GenError::Invalid("this build of PhotoCraft cannot reach Gemini (no network transport)".into()))
    }
}

pub mod comfy;

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(b: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(b)
    }

    #[test]
    fn request_carries_the_instruction_images_and_image_config() {
        let req = EditRequest {
            instruction: fill_instruction("a metallic ship hull", false),
            images: vec![ImageData::png(vec![1, 2, 3]), ImageData::png(vec![4])],
            aspect_ratio: Some("16:9"),
            image_size: Some("2K"),
            ..Default::default()
        };
        let v = request_body(&req);
        let parts = v.pointer("/contents/0/parts").and_then(Value::as_array).unwrap();
        assert_eq!(parts.len(), 3);
        assert!(parts[0]["text"].as_str().unwrap().contains("a metallic ship hull"));
        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
        assert_eq!(parts[1]["inlineData"]["data"], b64(&[1, 2, 3]));
        assert_eq!(parts[2]["inlineData"]["data"], b64(&[4]));
        assert_eq!(v["generationConfig"]["responseModalities"], json!(["IMAGE"]));
        assert_eq!(v["generationConfig"]["imageConfig"], json!({"aspectRatio": "16:9", "imageSize": "2K"}));
        // Without size hints there is no imageConfig at all.
        let bare = request_body(&EditRequest { aspect_ratio: None, image_size: None, ..req.clone() });
        assert!(bare["generationConfig"].get("imageConfig").is_none());
    }

    #[test]
    fn instructions_name_the_images_and_the_prompt() {
        let f = fill_instruction("  riveted steel  ", true);
        assert!(f.contains("riveted steel\n") && f.contains("image 3"));
        assert!(!fill_instruction("x", false).contains("image 3"));
        assert!(art_pass_instruction("warmer light").contains("warmer light"));
    }

    #[test]
    fn the_last_non_thought_image_is_the_result() {
        let body = json!({"candidates": [{"content": {"parts": [
            {"text": "Here you go"},
            {"inlineData": {"mimeType": "image/png", "data": b64(b"interim")}, "thought": true},
            {"inlineData": {"mimeType": "image/png", "data": b64(b"first")}},
            {"inlineData": {"mimeType": "image/jpeg", "data": b64(b"final")}},
        ]}, "finishReason": "STOP"}]});
        let img = parse_response(body.to_string().as_bytes()).unwrap();
        assert_eq!(img, ImageData { mime: "image/jpeg".into(), bytes: b"final".to_vec() });
    }

    #[test]
    fn blocked_and_imageless_answers_are_errors() {
        let blocked = json!({"promptFeedback": {"blockReason": "PROHIBITED_CONTENT"}});
        assert!(matches!(parse_response(blocked.to_string().as_bytes()), Err(GenError::Blocked(_))));
        let safety = json!({"candidates": [{"finishReason": "IMAGE_SAFETY", "content": {"parts": []}}]});
        assert!(matches!(parse_response(safety.to_string().as_bytes()), Err(GenError::Blocked(_))));
        let text = json!({"candidates": [{"content": {"parts": [{"text": "I can't edit that"}]}, "finishReason": "STOP"}]});
        assert_eq!(parse_response(text.to_string().as_bytes()), Err(GenError::NoImage("I can't edit that".into())));
        assert!(matches!(parse_response(b"not json"), Err(GenError::BadResponse(_))));
        assert!(matches!(parse_response(b"{}"), Err(GenError::NoImage(_))));
        let bad_b64 = json!({"candidates": [{"content": {"parts": [{"inlineData": {"mimeType": "image/png", "data": "@@@"}}]}}]});
        assert!(matches!(parse_response(bad_b64.to_string().as_bytes()), Err(GenError::BadResponse(_))));
    }

    #[test]
    fn statuses_map_to_readable_errors() {
        let body = json!({"error": {"code": 403, "message": "API key not valid"}}).to_string();
        assert_eq!(error_for_status(403, body.as_bytes()), GenError::Auth("API key not valid".into()));
        assert!(matches!(error_for_status(429, b"slow down"), GenError::Quota(m) if m == "slow down"));
        assert!(matches!(error_for_status(500, b""), GenError::Http { status: 500, .. }));
        // A huge error page is shortened, at a character boundary.
        let long = "é".repeat(2000);
        assert!(matches!(error_for_status(502, long.as_bytes()), GenError::Http { message, .. } if message.chars().count() == 301));
    }

    #[test]
    fn aspect_ratios_and_sizes() {
        assert_eq!(nearest_aspect_ratio(1920, 1080), "16:9");
        assert_eq!(nearest_aspect_ratio(1000, 1000), "1:1");
        assert_eq!(nearest_aspect_ratio(800, 1000), "4:5");
        assert_eq!(nearest_aspect_ratio(100, 10_000), "1:8");
        assert_eq!(nearest_aspect_ratio(0, 5), "1:1");
        assert_eq!((image_size_for(800), image_size_for(2000), image_size_for(5000)), ("1K", "2K", "4K"));
    }

    #[test]
    fn client_validates_key_and_model_and_hides_the_key() {
        assert!(matches!(Gemini::new("  ", ""), Err(GenError::Auth(_))));
        assert!(matches!(Gemini::new("k", "a/b?c"), Err(GenError::Invalid(_))));
        let g = Gemini::new("secret-key", "").unwrap();
        assert_eq!(g.model(), DEFAULT_MODEL);
        assert_eq!(g.url(), format!("{ENDPOINT}/{DEFAULT_MODEL}:generateContent"));
        assert_eq!(Gemini::new("k", "models/gemini-3-pro-image").unwrap().model(), "gemini-3-pro-image");
        assert!(!format!("{g:?}").contains("secret-key"));
        assert!(MODEL_IDS.iter().all(|id| valid_model(id)));
        assert_eq!(MODEL_IDS[0], DEFAULT_MODEL);
    }
}
