//! A client for a local [ComfyUI](https://github.com/comfyanonymous/ComfyUI) server: Generative
//! Fill with your own diffusion models, on your own GPU.
//!
//! The source image and its pixel mask are uploaded (`/upload/image`), a workflow graph in
//! ComfyUI's API format is queued (`/prompt`), the history is polled until it finishes
//! (`/history/{id}`), and the result is downloaded (`/view`).
//!
//! Two kinds of workflow:
//! - [`Workflow::SdxlInpaint`], built in: any SDXL inpainting checkpoint (Juggernaut XL
//!   Inpainting and the like), scaled to about one megapixel, conditioned on the mask, with
//!   Differential Diffusion so soft mask edges blend, and the denoise set by the strength.
//! - [`Workflow::Custom`]: any graph exported from ComfyUI with *Save (API Format)*. PhotoCraft
//!   finds its nodes by title and fills them in: `PC Prompt` (a text encoder's `text`),
//!   `PC Negative` (optional), `PC Image` and `PC Mask` (`LoadImage` nodes), `PC Sampler`
//!   (optional: its `denoise` and `seed`/`noise_seed`) and `PC Output` (the `SaveImage` whose
//!   image comes back). So FLUX Fill, Qwen-Image-Edit or tomorrow's model work without code.

use serde_json::{Value, json};

use crate::{EditRequest, GenError, ImageData};

/// ComfyUI's default address.
pub const DEFAULT_URL: &str = "http://127.0.0.1:8188";
/// The built-in workflow's negative prompt.
pub const DEFAULT_NEGATIVE: &str = "blurry, low quality, jpeg artifacts, watermark, text, signature, deformed, disfigured, bad anatomy";

/// Node titles a custom workflow marks for PhotoCraft.
pub const TITLE_PROMPT: &str = "PC Prompt";
pub const TITLE_NEGATIVE: &str = "PC Negative";
pub const TITLE_IMAGE: &str = "PC Image";
pub const TITLE_MASK: &str = "PC Mask";
pub const TITLE_SAMPLER: &str = "PC Sampler";
pub const TITLE_OUTPUT: &str = "PC Output";

/// Uploads go to this subfolder of ComfyUI's `input` folder.
const SUBFOLDER: &str = "photocraft";
/// The longest a workflow may run.
#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
const MAX_RUN: std::time::Duration = std::time::Duration::from_secs(15 * 60);
/// Largest result downloaded.
#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
const MAX_IMAGE_BYTES: u64 = 256 * 1024 * 1024;

/// Which graph runs.
#[derive(Clone, Debug, PartialEq)]
pub enum Workflow {
    /// The built-in SDXL inpainting graph; an empty checkpoint picks the first installed one
    /// whose name says "inpaint".
    SdxlInpaint { checkpoint: String },
    /// A graph exported in ComfyUI's API format, with nodes titled as described above.
    Custom { name: String, graph: Value },
}

/// What one run fills into a graph.
#[derive(Clone, Debug, PartialEq)]
pub struct RunParams<'a> {
    pub prompt: &'a str,
    pub negative: &'a str,
    /// Uploaded names, as `LoadImage` takes them (`subfolder/name`).
    pub image: &'a str,
    pub mask: &'a str,
    pub strength: f32,
    pub seed: u64,
}

/// An output image ComfyUI saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRef {
    pub filename: String,
    pub subfolder: String,
    pub kind: String,
}

/// Where a queued prompt is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum History {
    Pending,
    Done(ImageRef),
    Failed(String),
}

/// The built-in SDXL inpainting graph. Its output node is `"13"`.
pub fn sdxl_inpaint_graph(checkpoint: &str, p: &RunParams) -> Value {
    json!({
        "1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": checkpoint}},
        "2": {"class_type": "LoadImage", "inputs": {"image": p.image}, "_meta": {"title": TITLE_IMAGE}},
        "3": {"class_type": "LoadImage", "inputs": {"image": p.mask}, "_meta": {"title": TITLE_MASK}},
        // SDXL works at about a megapixel; PhotoCraft scales the answer back itself.
        "4": {"class_type": "ImageScaleToTotalPixels", "inputs": {"image": ["2", 0], "upscale_method": "lanczos", "megapixels": 1.0, "resolution_steps": 8}},
        "5": {"class_type": "ImageScaleToTotalPixels", "inputs": {"image": ["3", 0], "upscale_method": "bilinear", "megapixels": 1.0, "resolution_steps": 8}},
        "6": {"class_type": "ImageToMask", "inputs": {"image": ["5", 0], "channel": "red"}},
        "7": {"class_type": "CLIPTextEncode", "inputs": {"text": p.prompt, "clip": ["1", 1]}, "_meta": {"title": TITLE_PROMPT}},
        "8": {"class_type": "CLIPTextEncode", "inputs": {"text": p.negative, "clip": ["1", 1]}, "_meta": {"title": TITLE_NEGATIVE}},
        "9": {"class_type": "DifferentialDiffusion", "inputs": {"model": ["1", 0]}},
        "10": {"class_type": "InpaintModelConditioning", "inputs": {"positive": ["7", 0], "negative": ["8", 0], "vae": ["1", 2], "pixels": ["4", 0], "mask": ["6", 0], "noise_mask": true}},
        "11": {"class_type": "KSampler", "inputs": {
            "model": ["9", 0], "seed": p.seed, "steps": 30, "cfg": 5.0, "sampler_name": "dpmpp_2m_sde", "scheduler": "karras",
            "positive": ["10", 0], "negative": ["10", 1], "latent_image": ["10", 2], "denoise": p.strength.clamp(0.01, 1.0)
        }, "_meta": {"title": TITLE_SAMPLER}},
        "12": {"class_type": "VAEDecode", "inputs": {"samples": ["11", 0], "vae": ["1", 2]}},
        "13": {"class_type": "SaveImage", "inputs": {"images": ["12", 0], "filename_prefix": "photocraft/genfill"}, "_meta": {"title": TITLE_OUTPUT}},
    })
}

/// The id of the node titled `title`.
pub fn node_titled(graph: &Value, title: &str) -> Option<String> {
    graph.as_object()?.iter().find(|(_, n)| n.pointer("/_meta/title").and_then(Value::as_str).is_some_and(|t| t.trim().eq_ignore_ascii_case(title))).map(|(k, _)| k.clone())
}

/// Check a custom workflow file: an API-format graph with the nodes PhotoCraft needs.
pub fn check_custom(graph: &Value) -> Result<(), GenError> {
    let obj = graph.as_object().filter(|o| !o.is_empty()).ok_or_else(|| GenError::Invalid("the workflow is not a ComfyUI graph".into()))?;
    if obj.contains_key("nodes") && obj.contains_key("links") {
        return Err(GenError::Invalid("this is a ComfyUI UI workflow; export it with Workflow › Export (API) instead".into()));
    }
    if !obj.values().all(|n| n.get("class_type").and_then(Value::as_str).is_some() && n.get("inputs").is_some_and(Value::is_object)) {
        return Err(GenError::Invalid("the workflow is not in ComfyUI's API format (Workflow › Export (API))".into()));
    }
    for t in [TITLE_PROMPT, TITLE_IMAGE, TITLE_OUTPUT] {
        if node_titled(graph, t).is_none() {
            return Err(GenError::Invalid(format!("the workflow has no node titled \"{t}\"")));
        }
    }
    Ok(())
}

/// A custom graph with this run's values filled in.
pub fn fill_custom(graph: &Value, p: &RunParams) -> Result<Value, GenError> {
    check_custom(graph)?;
    let mut g = graph.clone();
    let mut set = |title: &str, key: &str, value: Value| {
        if let Some(id) = node_titled(&g, title)
            && let Some(inputs) = g.get_mut(&id).and_then(|n| n.get_mut("inputs")).and_then(Value::as_object_mut)
        {
            inputs.insert(key.into(), value);
        }
    };
    set(TITLE_PROMPT, "text", json!(p.prompt));
    set(TITLE_NEGATIVE, "text", json!(p.negative));
    set(TITLE_IMAGE, "image", json!(p.image));
    set(TITLE_MASK, "image", json!(p.mask));
    set(TITLE_SAMPLER, "denoise", json!(p.strength.clamp(0.01, 1.0)));
    // KSampler calls it `seed`, KSamplerAdvanced and RandomNoise `noise_seed`: set what's there.
    if let Some(id) = node_titled(&g, TITLE_SAMPLER)
        && let Some(inputs) = g.get_mut(&id).and_then(|n| n.get_mut("inputs")).and_then(Value::as_object_mut)
    {
        for k in ["seed", "noise_seed"] {
            if inputs.contains_key(k) {
                inputs.insert(k.into(), json!(p.seed));
            }
        }
    }
    Ok(g)
}

/// The first installed checkpoint that looks like an inpainting model (SDXL ones first).
pub fn pick_inpaint_checkpoint(names: &[String]) -> Option<String> {
    let inpaint = |n: &&String| n.to_lowercase().contains("inpaint");
    names.iter().filter(inpaint).find(|n| n.to_lowercase().contains("xl")).or_else(|| names.iter().find(inpaint)).cloned()
}

/// A `multipart/form-data` body uploading `bytes` as the `image` field.
pub fn multipart(boundary: &str, filename: &str, bytes: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(bytes.len() + 512);
    for (k, v) in [("type", "input"), ("subfolder", SUBFOLDER), ("overwrite", "true")] {
        b.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes());
    }
    b.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{filename}\"\r\nContent-Type: image/png\r\n\r\n").as_bytes());
    b.extend(bytes);
    b.extend(format!("\r\n--{boundary}--\r\n").as_bytes());
    b
}

/// The name `LoadImage` takes for an upload answer (`subfolder/name`).
pub fn parse_upload(body: &[u8]) -> Result<String, GenError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| GenError::BadResponse(format!("upload: {e}")))?;
    let name = v.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).ok_or_else(|| GenError::BadResponse("upload: no file name".into()))?;
    Ok(match v.get("subfolder").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(sub) => format!("{sub}/{name}"),
        None => name.to_string(),
    })
}

/// The prompt id from a `/prompt` answer, or the graph's validation errors.
pub fn parse_queue(status: u16, body: &[u8]) -> Result<String, GenError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| GenError::BadResponse(format!("queue ({status}): {e}")))?;
    if let Some(id) = v.get("prompt_id").and_then(Value::as_str).filter(|_| (200..300).contains(&status)) {
        return Ok(id.to_string());
    }
    let mut msg = v.pointer("/error/message").and_then(Value::as_str).unwrap_or("the workflow was refused").to_string();
    // The first node error says which input is wrong (a missing model, an unknown node).
    if let Some((node, e)) = v.get("node_errors").and_then(Value::as_object).and_then(|o| o.iter().next()) {
        let class = e.get("class_type").and_then(Value::as_str).unwrap_or("node");
        let detail = e.pointer("/errors/0/details").or_else(|| e.pointer("/errors/0/message")).and_then(Value::as_str).unwrap_or("");
        msg = format!("{msg}: {class} (node {node}) {detail}");
    }
    Err(GenError::Http { status, message: msg.trim().to_string() })
}

/// Where prompt `id` is, from a `/history/{id}` answer; the image of node `output` (any
/// image, when that node saved none).
pub fn parse_history(body: &[u8], id: &str, output: Option<&str>) -> Result<History, GenError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| GenError::BadResponse(format!("history: {e}")))?;
    let Some(entry) = v.get(id) else { return Ok(History::Pending) };
    if entry.pointer("/status/status_str").and_then(Value::as_str) == Some("error") {
        let detail = entry
            .pointer("/status/messages")
            .and_then(Value::as_array)
            .and_then(|m| m.iter().find(|x| x.get(0).and_then(Value::as_str) == Some("execution_error")))
            .and_then(|x| x.get(1))
            .map(|e| {
                let node = e.get("node_type").and_then(Value::as_str).unwrap_or("a node");
                let msg = e.get("exception_message").and_then(Value::as_str).unwrap_or("failed");
                format!("{node}: {}", msg.trim())
            })
            .unwrap_or_else(|| "the workflow failed".into());
        return Ok(History::Failed(detail));
    }
    let outputs = entry.get("outputs").and_then(Value::as_object);
    let image_of = |n: &Value| -> Option<ImageRef> {
        let i = n.get("images")?.as_array()?.iter().find(|i| i.get("type").and_then(Value::as_str) != Some("temp"))?;
        Some(ImageRef {
            filename: i.get("filename")?.as_str()?.to_string(),
            subfolder: i.get("subfolder").and_then(Value::as_str).unwrap_or("").to_string(),
            kind: i.get("type").and_then(Value::as_str).unwrap_or("output").to_string(),
        })
    };
    let found = outputs.and_then(|o| output.and_then(|k| o.get(k)).and_then(image_of).or_else(|| o.values().find_map(image_of)));
    let completed = entry.pointer("/status/completed").and_then(Value::as_bool).unwrap_or(outputs.is_some());
    Ok(match (found, completed) {
        (Some(img), _) => History::Done(img),
        (None, true) => History::Failed("the workflow finished without saving an image (is its output a SaveImage titled \"PC Output\"?)".into()),
        (None, false) => History::Pending,
    })
}

/// The `/view` query for an output image.
pub fn view_query(img: &ImageRef) -> String {
    format!("filename={}&subfolder={}&type={}", pct(&img.filename), pct(&img.subfolder), pct(&img.kind))
}

fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A ComfyUI server and the workflow to run on it.
#[derive(Clone, Debug)]
pub struct ComfyUi {
    base: String,
    workflow: Workflow,
    negative: String,
}

impl ComfyUi {
    /// `url` like `http://127.0.0.1:8188` (empty = [`DEFAULT_URL`]).
    pub fn new(url: &str, workflow: Workflow) -> Result<Self, GenError> {
        let url = url.trim();
        let url = if url.is_empty() { DEFAULT_URL } else { url };
        if !(url.starts_with("http://") || url.starts_with("https://")) || url.contains(char::is_whitespace) {
            return Err(GenError::Invalid(format!("`{url}` is not a ComfyUI address (like {DEFAULT_URL})")));
        }
        if let Workflow::Custom { graph, .. } = &workflow {
            check_custom(graph)?;
        }
        Ok(Self { base: url.trim_end_matches('/').to_string(), workflow, negative: DEFAULT_NEGATIVE.to_string() })
    }

    pub fn url(&self) -> &str {
        &self.base
    }

    pub fn workflow(&self) -> &Workflow {
        &self.workflow
    }

    /// The negative prompt the built-in workflow (and a `PC Negative` node) gets.
    pub fn negative(&self) -> &str {
        &self.negative
    }

    /// e.g. "ComfyUI (SDXL Inpaint: juggernautXLInpainting.safetensors)".
    pub fn describe(&self) -> String {
        match &self.workflow {
            Workflow::SdxlInpaint { checkpoint } if checkpoint.is_empty() => "ComfyUI (SDXL Inpaint)".into(),
            Workflow::SdxlInpaint { checkpoint } => format!("ComfyUI (SDXL Inpaint: {checkpoint})"),
            Workflow::Custom { name, .. } => format!("ComfyUI ({name})"),
        }
    }

    /// The graph for one run and the id of its output node.
    pub fn graph(&self, checkpoint: &str, p: &RunParams) -> Result<(Value, Option<String>), GenError> {
        Ok(match &self.workflow {
            Workflow::SdxlInpaint { .. } => (sdxl_inpaint_graph(checkpoint, p), Some("13".into())),
            Workflow::Custom { graph, .. } => {
                let g = fill_custom(graph, p)?;
                let out = node_titled(&g, TITLE_OUTPUT);
                (g, out)
            }
        })
    }
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
impl ComfyUi {
    fn agent(timeout: std::time::Duration) -> ureq::Agent {
        ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).user_agent("PhotoCraft").build().into()
    }

    fn unreachable(&self, e: impl std::fmt::Display) -> GenError {
        GenError::Transport(format!("ComfyUI at {} ({e}). Is it running?", self.base))
    }

    fn get(&self, path: &str, limit: u64) -> Result<(u16, Vec<u8>), GenError> {
        let mut r = Self::agent(std::time::Duration::from_secs(60)).get(&format!("{}{path}", self.base)).call().map_err(|e| self.unreachable(e))?;
        let status = r.status().as_u16();
        let body = r.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| self.unreachable(e))?;
        Ok((status, body))
    }

    fn post(&self, path: &str, content_type: &str, body: &[u8]) -> Result<(u16, Vec<u8>), GenError> {
        let mut r = Self::agent(std::time::Duration::from_secs(120))
            .post(&format!("{}{path}", self.base))
            .header("Content-Type", content_type)
            .send(body)
            .map_err(|e| self.unreachable(e))?;
        let status = r.status().as_u16();
        let body = r.body_mut().with_config().limit(4 * 1024 * 1024).read_to_vec().map_err(|e| self.unreachable(e))?;
        Ok((status, body))
    }

    /// The checkpoints ComfyUI has installed.
    pub fn checkpoints(&self) -> Result<Vec<String>, GenError> {
        let (status, body) = self.get("/models/checkpoints", 4 * 1024 * 1024)?;
        if status != 200 {
            return Err(GenError::Http { status, message: "could not list the checkpoints".into() });
        }
        serde_json::from_slice(&body).map_err(|e| GenError::BadResponse(format!("checkpoints: {e}")))
    }

    fn upload(&self, filename: &str, png: &[u8]) -> Result<String, GenError> {
        let boundary = format!("photocraft{:x}", seed());
        let (status, body) = self.post("/upload/image", &format!("multipart/form-data; boundary={boundary}"), &multipart(&boundary, filename, png))?;
        if status != 200 {
            return Err(GenError::Http { status, message: format!("upload refused: {}", String::from_utf8_lossy(&body).chars().take(200).collect::<String>()) });
        }
        parse_upload(&body)
    }

    /// Run `req` (its `prompt`, `source`, `mask` and `strength`) and wait for the image.
    pub fn generate(&self, req: &EditRequest, cancel: &dyn Fn() -> bool) -> Result<ImageData, GenError> {
        let source = req.source.as_ref().ok_or_else(|| GenError::Invalid("no image to edit".into()))?;
        let checkpoint = match &self.workflow {
            Workflow::SdxlInpaint { checkpoint } if checkpoint.is_empty() => pick_inpaint_checkpoint(&self.checkpoints()?).ok_or_else(|| {
                GenError::Invalid("ComfyUI has no inpainting checkpoint installed; set one in Preferences › Integrations (ComfyUI checkpoint)".into())
            })?,
            Workflow::SdxlInpaint { checkpoint } => checkpoint.clone(),
            Workflow::Custom { .. } => String::new(),
        };
        let run = seed();
        let image = self.upload(&format!("pc-{run:x}-image.png"), &source.bytes)?;
        let mask = match &req.mask {
            Some(m) => self.upload(&format!("pc-{run:x}-mask.png"), &m.bytes)?,
            None => return Err(GenError::Invalid("no mask to edit with".into())),
        };
        if cancel() {
            return Err(GenError::Invalid("cancelled".into()));
        }
        let params = RunParams { prompt: req.prompt.trim(), negative: &self.negative, image: &image, mask: &mask, strength: req.strength, seed: run % (1 << 48) };
        let (graph, output) = self.graph(&checkpoint, &params)?;
        let body = json!({"prompt": graph, "client_id": "photocraft"}).to_string();
        let (status, answer) = self.post("/prompt", "application/json", body.as_bytes())?;
        let id = parse_queue(status, &answer)?;
        let start = std::time::Instant::now();
        let img = loop {
            if cancel() {
                // Stop the GPU work too: drop it from the queue, interrupt it if it's running.
                let _ = self.post("/queue", "application/json", json!({"delete": [id]}).to_string().as_bytes());
                let _ = self.post("/interrupt", "application/json", b"{}");
                return Err(GenError::Invalid("cancelled".into()));
            }
            if start.elapsed() > MAX_RUN {
                return Err(GenError::Transport("ComfyUI took longer than 15 minutes".into()));
            }
            std::thread::sleep(std::time::Duration::from_millis(400));
            let (_, h) = self.get(&format!("/history/{}", pct(&id)), 64 * 1024 * 1024)?;
            match parse_history(&h, &id, output.as_deref())? {
                History::Pending => continue,
                History::Failed(e) => return Err(GenError::Http { status: 500, message: format!("ComfyUI: {e}") }),
                History::Done(img) => break img,
            }
        };
        let (status, bytes) = self.get(&format!("/view?{}", view_query(&img)), MAX_IMAGE_BYTES)?;
        if status != 200 || bytes.is_empty() {
            return Err(GenError::Http { status, message: "could not download the result".into() });
        }
        Ok(ImageData { mime: "image/png".into(), bytes })
    }
}

#[cfg(not(all(feature = "http", not(target_arch = "wasm32"))))]
impl ComfyUi {
    /// Without the transport (the web build, headless tools).
    pub fn generate(&self, _req: &EditRequest, _cancel: &dyn Fn() -> bool) -> Result<ImageData, GenError> {
        Err(GenError::Invalid("this build of PhotoCraft cannot reach ComfyUI (no network transport)".into()))
    }

    pub fn checkpoints(&self) -> Result<Vec<String>, GenError> {
        Err(GenError::Invalid("this build of PhotoCraft cannot reach ComfyUI (no network transport)".into()))
    }
}

/// A fresh seed per run (and a unique upload name).
#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
fn seed() -> u64 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    (t as u64) ^ ((t >> 64) as u64).rotate_left(17)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> RunParams<'static> {
        RunParams { prompt: "rusty hull", negative: "blurry", image: "photocraft/a.png", mask: "photocraft/m.png", strength: 0.6, seed: 42 }
    }

    #[test]
    fn the_built_in_graph_wires_prompt_images_and_strength() {
        let g = sdxl_inpaint_graph("juggernautXLInpainting.safetensors", &params());
        assert_eq!(g["1"]["inputs"]["ckpt_name"], "juggernautXLInpainting.safetensors");
        assert_eq!(g["2"]["inputs"]["image"], "photocraft/a.png");
        assert_eq!(g["3"]["inputs"]["image"], "photocraft/m.png");
        assert_eq!(g["7"]["inputs"]["text"], "rusty hull");
        assert_eq!(g["8"]["inputs"]["text"], "blurry");
        assert_eq!(g["11"]["inputs"]["denoise"].as_f64().unwrap() as f32, 0.6);
        assert_eq!(g["11"]["inputs"]["seed"], 42);
        assert_eq!(node_titled(&g, TITLE_OUTPUT).as_deref(), Some("13"));
        // Every link points at a node that exists.
        for (id, n) in g.as_object().unwrap() {
            for v in n["inputs"].as_object().unwrap().values() {
                if let Some(link) = v.as_array() {
                    assert!(g.get(link[0].as_str().unwrap()).is_some(), "node {id} links to a missing node");
                }
            }
        }
    }

    #[test]
    fn custom_workflows_are_filled_by_node_title() {
        let graph = json!({
            "3": {"class_type": "KSampler", "inputs": {"seed": 1, "denoise": 1.0}, "_meta": {"title": "PC Sampler"}},
            "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "old"}, "_meta": {"title": "pc prompt"}},
            "10": {"class_type": "LoadImage", "inputs": {"image": "x.png"}, "_meta": {"title": "PC Image"}},
            "11": {"class_type": "LoadImage", "inputs": {"image": "y.png"}, "_meta": {"title": "PC Mask"}},
            "20": {"class_type": "SaveImage", "inputs": {"filename_prefix": "out"}, "_meta": {"title": "PC Output"}},
        });
        let g = fill_custom(&graph, &params()).unwrap();
        assert_eq!(g["6"]["inputs"]["text"], "rusty hull");
        assert_eq!(g["10"]["inputs"]["image"], "photocraft/a.png");
        assert_eq!(g["11"]["inputs"]["image"], "photocraft/m.png");
        assert_eq!(g["3"]["inputs"]["seed"], 42);
        assert_eq!(g["3"]["inputs"]["denoise"].as_f64().unwrap() as f32, 0.6);
        let c = ComfyUi::new("", Workflow::Custom { name: "flux-fill.json".into(), graph: graph.clone() }).unwrap();
        let (_, out) = c.graph("", &params()).unwrap();
        assert_eq!(out.as_deref(), Some("20"));
        assert_eq!(c.describe(), "ComfyUI (flux-fill.json)");
    }

    #[test]
    fn bad_custom_workflows_say_what_is_wrong() {
        let ui_format = json!({"nodes": [], "links": []});
        assert!(matches!(check_custom(&ui_format), Err(GenError::Invalid(m)) if m.contains("Export (API)")));
        assert!(check_custom(&json!([])).is_err());
        assert!(check_custom(&json!({"1": {"inputs": {}}})).is_err());
        let no_output = json!({
            "1": {"class_type": "CLIPTextEncode", "inputs": {}, "_meta": {"title": "PC Prompt"}},
            "2": {"class_type": "LoadImage", "inputs": {}, "_meta": {"title": "PC Image"}},
        });
        assert!(matches!(check_custom(&no_output), Err(GenError::Invalid(m)) if m.contains("PC Output")));
        assert!(ComfyUi::new("localhost:8188", Workflow::SdxlInpaint { checkpoint: String::new() }).is_err());
        assert_eq!(ComfyUi::new("http://127.0.0.1:8188/", Workflow::SdxlInpaint { checkpoint: String::new() }).unwrap().url(), "http://127.0.0.1:8188");
    }

    #[test]
    fn an_inpainting_checkpoint_is_picked_sdxl_first() {
        let names: Vec<String> = ["flux_dev.safetensors", "sd15-inpainting.ckpt", "juggernautXLInpainting_xiInpainting.safetensors"].map(String::from).to_vec();
        assert_eq!(pick_inpaint_checkpoint(&names).as_deref(), Some("juggernautXLInpainting_xiInpainting.safetensors"));
        assert_eq!(pick_inpaint_checkpoint(&names[..2]).as_deref(), Some("sd15-inpainting.ckpt"));
        assert_eq!(pick_inpaint_checkpoint(&names[..1]), None);
    }

    #[test]
    fn server_answers_are_read() {
        assert_eq!(parse_upload(br#"{"name": "pc-1-image.png", "subfolder": "photocraft", "type": "input"}"#).unwrap(), "photocraft/pc-1-image.png");
        assert_eq!(parse_upload(br#"{"name": "a.png", "subfolder": ""}"#).unwrap(), "a.png");
        assert!(parse_upload(b"{}").is_err());
        assert_eq!(parse_queue(200, br#"{"prompt_id": "abc", "number": 3, "node_errors": {}}"#).unwrap(), "abc");
        let refused = br#"{"error": {"message": "Prompt outputs failed validation"}, "node_errors": {"1": {"class_type": "CheckpointLoaderSimple", "errors": [{"details": "ckpt_name: 'x' not in list"}]}}}"#;
        assert!(matches!(parse_queue(400, refused), Err(GenError::Http { status: 400, message }) if message.contains("CheckpointLoaderSimple") && message.contains("not in list")));
        assert_eq!(parse_history(b"{}", "abc", None).unwrap(), History::Pending);
        let done = br#"{"abc": {"outputs": {"9": {"images": [{"filename": "p.png", "subfolder": "", "type": "temp"}]}, "13": {"images": [{"filename": "genfill_0001_.png", "subfolder": "photocraft", "type": "output"}]}}, "status": {"status_str": "success", "completed": true}}}"#;
        let img = ImageRef { filename: "genfill_0001_.png".into(), subfolder: "photocraft".into(), kind: "output".into() };
        assert_eq!(parse_history(done, "abc", Some("13")).unwrap(), History::Done(img.clone()));
        assert_eq!(parse_history(done, "abc", Some("99")).unwrap(), History::Done(img.clone()), "any saved image when the output node saved none");
        let failed = br#"{"abc": {"outputs": {}, "status": {"status_str": "error", "completed": false, "messages": [["execution_error", {"node_type": "KSampler", "exception_message": "CUDA out of memory\n"}]]}}}"#;
        assert_eq!(parse_history(failed, "abc", None).unwrap(), History::Failed("KSampler: CUDA out of memory".into()));
        let empty = br#"{"abc": {"outputs": {}, "status": {"status_str": "success", "completed": true}}}"#;
        assert!(matches!(parse_history(empty, "abc", None).unwrap(), History::Failed(_)));
        assert!(parse_history(b"nope", "abc", None).is_err());
        assert_eq!(view_query(&img), "filename=genfill_0001_.png&subfolder=photocraft&type=output");
        assert_eq!(pct("a b/ü"), "a%20b%2F%C3%BC");
    }

    #[test]
    fn uploads_are_multipart() {
        let b = multipart("XYZ", "pc.png", b"\x89PNG");
        let text = String::from_utf8_lossy(&b);
        assert!(text.starts_with("--XYZ\r\n"));
        assert!(text.contains("name=\"image\"; filename=\"pc.png\"\r\nContent-Type: image/png\r\n\r\n\u{FFFD}PNG\r\n--XYZ--\r\n"));
        assert!(text.contains("name=\"subfolder\"\r\n\r\nphotocraft\r\n"));
        assert!(text.contains("name=\"overwrite\"\r\n\r\ntrue\r\n"));
    }
}
