//! Edit › Generative Fill… (custom): the prompt dialog, the Gemini API key, and keeping the
//! session's image model in step with Preferences › Integrations (Google Gemini, or a local
//! ComfyUI server).
//!
//! The key never goes into preferences (agents read those through `prefs.get`): the desktop app
//! keeps it in the OS credential store and hands this crate closures to read its status, change
//! it and build a model from it ([`GenerativeService`]). The web build has none, and the dialog
//! says so.
use std::sync::Arc;

use photocraft_engine::generative_cmds::{COMMAND, ImageGenerator};
use serde_json::{Map, Value, json};

use crate::{PhotocraftApp, state::DialogKind};

const MARK: &str = "__generativeFill";
/// Prompts remembered for the dialog's recent list.
const RECENT: usize = 8;

/// Where the API key is kept, for display (never the key itself).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInfo {
    /// e.g. "Windows Credential Manager" or "the GEMINI_API_KEY environment variable".
    pub source: String,
    /// The key's last characters, e.g. "…k3Qa".
    pub hint: String,
    /// Whether "Remove" can delete it (an environment variable can't be).
    pub removable: bool,
}

/// Stores a key (`Some`) or deletes it (`None`).
pub type SetKeyFn = Box<dyn Fn(Option<&str>) -> Result<(), String>>;
/// Builds the model for the Generative Fill preferences (Gemini with the stored key, or ComfyUI).
pub type GeneratorFn = Box<dyn Fn(&GenConfig) -> Result<Arc<dyn ImageGenerator>, String>>;

/// The Generative Fill preferences a model is built from (Preferences › Integrations).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GenConfig {
    /// `gemini` or `comfyui`.
    pub provider: String,
    pub gemini_model: String,
    pub comfyui_url: String,
    pub comfyui_checkpoint: String,
    pub comfyui_workflow: String,
}

impl GenConfig {
    fn of(app: &PhotocraftApp) -> Self {
        let i = &app.session.prefs().integrations;
        Self {
            provider: i.generative_provider.clone(),
            gemini_model: i.generative_model.clone(),
            comfyui_url: i.comfyui_url.clone(),
            comfyui_checkpoint: i.comfyui_checkpoint.clone(),
            comfyui_workflow: i.comfyui_workflow.clone(),
        }
    }

    /// Whether the local ComfyUI server is the model.
    pub fn is_comfyui(&self) -> bool {
        self.provider == "comfyui"
    }
}

/// The platform's key storage and model factory (desktop only).
pub struct GenerativeService {
    /// The stored key's status; `None` without a key.
    pub key_status: Box<dyn Fn() -> Option<KeyInfo>>,
    pub set_key: SetKeyFn,
    pub generator: GeneratorFn,
}

/// What [`sync`] last installed.
#[derive(Clone, Debug, Default)]
pub struct GenerativeState {
    synced: Option<GenConfig>,
    key: Option<KeyInfo>,
    error: Option<String>,
    recent: Vec<String>,
    /// The strength last used for a fill and for an art pass (ComfyUI).
    strength: Option<(f64, f64)>,
}

/// Install the model for the current preferences (once per change; cheap per frame).
pub fn sync(app: &mut PhotocraftApp) {
    let config = GenConfig::of(app);
    if app.generative.synced.as_ref() == Some(&config) {
        return;
    }
    app.generative.synced = Some(config.clone());
    let Some(svc) = app.services.generative.as_ref() else {
        app.generative.key = None;
        app.session.set_image_generator(None);
        return;
    };
    app.generative.key = (svc.key_status)();
    // Gemini needs the key; a local ComfyUI doesn't.
    let (generator, error) = match (config.is_comfyui(), &app.generative.key) {
        (false, None) => (None, None),
        _ => match (svc.generator)(&config) {
            Ok(g) => (Some(g), None),
            Err(e) => (None, Some(e)),
        },
    };
    app.generative.error = error;
    app.session.set_image_generator(generator);
}

/// Re-read the key and rebuild the model on the next [`sync`].
fn resync(app: &mut PhotocraftApp) {
    app.generative.synced = None;
    sync(app);
}

pub fn owns(fields: &Map<String, Value>) -> bool {
    fields.contains_key(MARK)
}

/// Open the dialog (Edit › Generative Fill… and the selection's context menu).
pub fn open(app: &mut PhotocraftApp) -> Result<Value, String> {
    resync(app);
    let st = app.session.active().ok_or("no document")?;
    let has_selection = st.doc.selection.is_some();
    let mut fields = Map::new();
    fields.insert(MARK.into(), json!(true));
    fields.insert("__command".into(), json!(COMMAND));
    fields.insert("__label".into(), json!("Generative Fill"));
    fields.insert("__hasSelection".into(), json!(has_selection));
    fields.insert("__document".into(), json!(st.doc.id.0));
    fields.insert("prompt".into(), json!(app.generative.recent.first().cloned().unwrap_or_default()));
    fields.insert("output".into(), json!(if has_selection { "current" } else { "new" }));
    let (fill, art) = app.generative.strength.unwrap_or((1.0, 0.45));
    fields.insert("strength".into(), json!(if has_selection { fill } else { art }));
    fields.insert("__key".into(), json!(""));
    let id = app.ui.open_dialog(DialogKind::Command, fields);
    Ok(json!({"dialog": id}))
}

pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, fields: &mut Map<String, Value>) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let has_selection = fields.get("__hasSelection").and_then(Value::as_bool).unwrap_or(false);
    let dim = |s: &str| egui::RichText::new(s).color(t.text_dim);
    ui.set_width(440.0);
    ui.label(dim(if has_selection {
        "Describe what the selected area should become. The model sees the whole visible canvas (all layers) and repaints only the selection."
    } else {
        "No selection: an art pass over the whole image. Describe the polish, lighting or style to apply."
    }));
    ui.add_space(6.0);
    let mut prompt = fields.get("prompt").and_then(Value::as_str).unwrap_or("").to_string();
    let hint = if has_selection { "e.g. a weathered metallic ship hull with riveted steel plates" } else { "e.g. dramatic rim lighting, richer colour, crisper details" };
    let edit = egui::TextEdit::multiline(&mut prompt).hint_text(hint).desired_rows(4).desired_width(f32::INFINITY).char_limit(photocraft_genai::MAX_PROMPT_CHARS);
    let r = ui.add(edit);
    if ui.memory(|m| m.focused().is_none()) && fields.get("__focused").is_none() {
        r.request_focus();
        fields.insert("__focused".into(), json!(true));
    }
    // Recent prompts.
    if !app.generative.recent.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(dim("Recent:"));
            for p in app.generative.recent.clone() {
                let short: String = p.chars().take(32).collect();
                let short = if short.len() < p.len() { format!("{short}…") } else { short };
                if ui.small_button(short).on_hover_text(&p).clicked() {
                    prompt = p.clone();
                }
            }
        });
    }
    fields.insert("prompt".into(), json!(prompt));
    ui.add_space(6.0);
    let mut output = fields.get("output").and_then(Value::as_str).unwrap_or("current").to_string();
    ui.horizontal(|ui| {
        ui.label("Paint into:");
        ui.radio_value(&mut output, "current".to_string(), "Active layer");
        ui.radio_value(&mut output, "new".to_string(), "New layer");
    });
    fields.insert("output".into(), json!(output));
    // A diffusion model can stay close to the canvas: the art pass's polish without a redraw.
    if GenConfig::of(app).is_comfyui() {
        let mut strength = fields.get("strength").and_then(Value::as_f64).unwrap_or(1.0) as f32;
        ui.horizontal(|ui| {
            ui.label("Strength:");
            ui.add(egui::Slider::new(&mut strength, 0.05..=1.0).fixed_decimals(2));
        });
        ui.label(dim(if has_selection { "1.00 repaints the selection fully; lower keeps more of what's there." } else { "Around 0.3–0.5 polishes without redrawing; higher changes more." }));
        fields.insert("strength".into(), json!(strength));
    }
    ui.add_space(8.0);
    crate::widgets::hairline(ui);
    ui.add_space(6.0);
    key_rows(app, ui, fields);
}

/// The model in use and the API key: its source and Change/Remove, or a field to enter one.
fn key_rows(app: &mut PhotocraftApp, ui: &mut egui::Ui, fields: &mut Map<String, Value>) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let faint = |s: &str| egui::RichText::new(s).color(t.text_faint);
    if app.services.generative.is_none() {
        ui.label(faint("Generative Fill needs the desktop app (it calls Google Gemini or ComfyUI over the network)."));
        return;
    }
    let config = GenConfig::of(app);
    if config.is_comfyui() {
        let what = app.session.image_generator_name().unwrap_or_else(|| "ComfyUI".into());
        ui.label(faint(&format!("{what} at {} (change it in Preferences › Integrations)", config.comfyui_url)));
        if let Some(e) = &app.generative.error {
            ui.colored_label(t.danger, e);
        }
        ui.label(faint("OK sends the visible canvas (and the selection mask) to ComfyUI; nothing leaves your computer for a local address."));
        return;
    }
    let model = config.gemini_model;
    let editing = fields.get("__editKey").and_then(Value::as_bool).unwrap_or(false);
    match app.generative.key.clone() {
        Some(k) if !editing => {
            ui.horizontal_wrapped(|ui| {
                ui.label(faint(&format!("Google Gemini · {model} · key {} in {}", k.hint, k.source)));
                if ui.small_button("Change key").clicked() {
                    fields.insert("__editKey".into(), json!(true));
                }
                if k.removable && ui.small_button("Remove key").clicked() {
                    store_key(app, None);
                }
            });
            if let Some(e) = &app.generative.error {
                ui.colored_label(t.danger, e);
            }
        }
        _ => {
            ui.label(faint(&format!("Google Gemini · {model} (change the model in Preferences › Integrations)")));
            ui.horizontal(|ui| {
                ui.label("Gemini API key:");
                let mut key = fields.get("__key").and_then(Value::as_str).unwrap_or("").to_string();
                ui.add(egui::TextEdit::singleline(&mut key).password(true).desired_width(220.0).hint_text("paste your key"));
                let save = ui.add_enabled(!key.trim().is_empty(), egui::Button::new("Save key")).clicked();
                if save && store_key(app, Some(key.trim())) {
                    key.clear();
                    fields.insert("__editKey".into(), json!(false));
                }
                fields.insert("__key".into(), json!(key));
            });
            ui.horizontal(|ui| {
                ui.label(faint("Get a key at aistudio.google.com/apikey. It's stored in your system's credential store, never in settings."));
            });
        }
    }
    // The canvas leaves the computer: say so where the request is made.
    ui.label(faint("OK sends the visible canvas (and the selection mask) to Google."));
}

/// Store or remove the key, then rebuild the model. False (with the status bar message) when the
/// store refused.
fn store_key(app: &mut PhotocraftApp, key: Option<&str>) -> bool {
    let Some(svc) = app.services.generative.as_ref() else { return false };
    match (svc.set_key)(key) {
        Ok(()) => {
            resync(app);
            app.ui.status = if key.is_some() { "Gemini API key saved".into() } else { "Gemini API key removed".into() };
            app.ui.status_error = false;
            true
        }
        Err(e) => {
            app.ui.status = format!("Could not store the API key: {e}");
            app.ui.status_error = true;
            false
        }
    }
}

pub fn confirm(app: &mut PhotocraftApp, fields: &Map<String, Value>) -> Result<Value, String> {
    let st = app.session.active().ok_or("The document is closed")?;
    if fields.get("__document").and_then(Value::as_u64) != Some(st.doc.id.0) {
        return Err("The document changed. Choose Generative Fill again.".into());
    }
    // A key typed but not saved yet is saved on OK.
    if let Some(key) = fields.get("__key").and_then(Value::as_str).map(str::trim).filter(|k| !k.is_empty())
        && !store_key(app, Some(key))
    {
        return Err(app.ui.status.clone());
    }
    sync(app);
    let prompt = fields.get("prompt").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if prompt.is_empty() {
        return Err("Describe what to paint first.".into());
    }
    app.generative.recent.retain(|p| *p != prompt);
    app.generative.recent.insert(0, prompt.clone());
    app.generative.recent.truncate(RECENT);
    let output = fields.get("output").and_then(Value::as_str).unwrap_or("current");
    let mut params = json!({"prompt": prompt, "output": output});
    if GenConfig::of(app).is_comfyui()
        && let Some(s) = fields.get("strength").and_then(Value::as_f64).filter(|s| s.is_finite())
    {
        let s = s.clamp(0.05, 1.0);
        params["strength"] = json!(s);
        let (fill, art) = app.generative.strength.unwrap_or((1.0, 0.45));
        let has_selection = fields.get("__hasSelection").and_then(Value::as_bool).unwrap_or(false);
        app.generative.strength = Some(if has_selection { (s, art) } else { (fill, s) });
    }
    app.run_command(COMMAND, params)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use photocraft_genai::{EditRequest, GenError, ImageData};

    use super::*;

    /// Answers every request with a solid green image.
    struct Green;
    impl ImageGenerator for Green {
        fn describe(&self) -> String {
            "Green".into()
        }
        fn generate(&self, _: &EditRequest, _: &dyn Fn() -> bool) -> Result<ImageData, GenError> {
            let img = photocraft_codecs::Image::from_u8(64, 64, photocraft_codecs::ChannelLayout::Rgb, [0u8, 255, 0].repeat(64 * 64)).unwrap();
            Ok(ImageData::png(photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap()))
        }
    }

    /// A key store in memory, with the model it builds.
    fn service(store: Rc<RefCell<Option<String>>>) -> GenerativeService {
        let (a, b, c) = (store.clone(), store.clone(), store);
        GenerativeService {
            key_status: Box::new(move || a.borrow().clone().map(|k| KeyInfo { source: "memory".into(), hint: format!("…{}", &k[k.len() - 2..]), removable: true })),
            set_key: Box::new(move |k| {
                *b.borrow_mut() = k.map(str::to_string);
                Ok(())
            }),
            generator: Box::new(move |cfg| {
                if cfg.is_comfyui() || c.borrow().is_some() { Ok(Arc::new(Green) as Arc<dyn ImageGenerator>) } else { Err("no key".into()) }
            }),
        }
    }

    fn dialog_fields(app: &PhotocraftApp) -> Map<String, Value> {
        app.ui.dialogs.iter().find(|d| owns(&d.fields)).map(|d| d.fields.clone()).expect("the dialog is open")
    }

    #[test]
    fn a_key_typed_in_the_dialog_is_stored_and_ok_fills_the_selection() {
        let store = Rc::new(RefCell::new(None));
        let services = crate::Services { generative: Some(service(store.clone())), ..Default::default() };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width": 40, "height": 30})).unwrap();
        app.run("select.rect", json!({"x": 10, "y": 10, "width": 10, "height": 10})).unwrap();
        sync(&mut app);
        assert!(app.session.image_generator_name().is_none(), "no key, no model");
        open(&mut app).unwrap();
        let mut fields = dialog_fields(&app);
        assert_eq!(fields["output"], "current");
        // OK without a prompt is refused (and stores the key typed so far).
        fields.insert("__key".into(), json!("  secret-key-xy  "));
        assert!(confirm(&mut app, &fields).is_err());
        assert_eq!(store.borrow().as_deref(), Some("secret-key-xy"));
        assert_eq!(app.session.image_generator_name().as_deref(), Some("Green"));
        assert_eq!(app.generative.key.as_ref().map(|k| k.hint.as_str()), Some("…xy"));
        fields.insert("__key".into(), json!(""));
        fields.insert("prompt".into(), json!("green moss"));
        confirm(&mut app, &fields).unwrap();
        let st = app.session.active().unwrap();
        let id = st.active_layer.unwrap();
        let surf = st.doc.layer(id).unwrap().surface().unwrap();
        let px = surf.to_interleaved(photocraft_geom::Rect::new(15, 15, 16, 16));
        assert_eq!(&px[..3], &[0, 255, 0], "the selection took the model's green");
        assert_eq!(app.generative.recent, vec!["green moss".to_string()]);
        // The next dialog starts from the last prompt; Remove key uninstalls the model.
        app.ui.dialogs.clear();
        open(&mut app).unwrap();
        assert_eq!(dialog_fields(&app)["prompt"], "green moss");
        assert!(store_key(&mut app, None));
        assert!(app.session.image_generator_name().is_none());
    }

    #[test]
    fn without_the_service_nothing_is_installed_and_the_document_must_match() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 20, "height": 20})).unwrap();
        sync(&mut app);
        assert!(app.session.image_generator_name().is_none());
        open(&mut app).unwrap();
        let mut fields = dialog_fields(&app);
        assert_eq!(fields["output"], "new", "no selection: an art pass into a new layer");
        fields.insert("prompt".into(), json!("x"));
        let e = confirm(&mut app, &fields).unwrap_err();
        assert!(e.contains("API key"), "{e}");
        fields.insert("__document".into(), json!(9999));
        assert!(confirm(&mut app, &fields).unwrap_err().contains("document changed"));
    }

    #[test]
    fn comfyui_needs_no_key_and_passes_the_strength() {
        let store = Rc::new(RefCell::new(None));
        let services = crate::Services { generative: Some(service(store.clone())), ..Default::default() };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.session.edit_prefs(|p| p.integrations.generative_provider = "comfyui".into());
        app.run("file.new", json!({"width": 40, "height": 30})).unwrap();
        sync(&mut app);
        assert_eq!(app.session.image_generator_name().as_deref(), Some("Green"), "no key needed");
        open(&mut app).unwrap();
        let mut fields = dialog_fields(&app);
        assert_eq!(fields["strength"], json!(0.45), "an art pass starts gentle");
        fields.insert("prompt".into(), json!("warm light"));
        fields.insert("strength".into(), json!(0.3));
        confirm(&mut app, &fields).unwrap();
        assert_eq!(app.generative.strength, Some((1.0, 0.3)));
        assert!(store.borrow().is_none(), "no key was stored");
        // Back to Gemini without a key: no model.
        app.session.edit_prefs(|p| p.integrations.generative_provider = "gemini".into());
        sync(&mut app);
        assert!(app.session.image_generator_name().is_none());
    }
}
