//! Edit › Generative Fill on the desktop (custom): the Gemini API key in the OS credential store,
//! and the model the preferences ask for (Gemini, or a local ComfyUI server).
//!
//! The key is kept in Windows Credential Manager or the macOS Keychain (the `keyring` crate). On
//! Linux, where a Secret Service may not be running, it is a file only the user can read in the
//! config directory. Without a stored key the `GEMINI_API_KEY` (or `GOOGLE_API_KEY`) environment
//! variable is used. The key never goes into preferences, which agents can read.

use std::sync::Arc;

use photocraft_engine::generative_cmds::ImageGenerator;
use photocraft_ui_egui::generative_ui::{GenConfig, GenerativeService, KeyInfo};

/// The credential's service and account names.
#[cfg(any(target_os = "windows", target_os = "macos"))]
const SERVICE: &str = "ai.storyteller.photocraft";
#[cfg(any(target_os = "windows", target_os = "macos"))]
const ACCOUNT: &str = "gemini-api-key";
const ENV_VARS: [&str; 2] = ["GEMINI_API_KEY", "GOOGLE_API_KEY"];

/// The service the UI uses.
pub fn service() -> GenerativeService {
    GenerativeService {
        key_status: Box::new(|| current_key().map(|(key, source, removable)| KeyInfo { source: source.to_string(), hint: hint(&key), removable })),
        set_key: Box::new(|key| match key {
            Some(k) => store::save(k.trim()),
            None => store::delete(),
        }),
        generator: Box::new(|config| {
            if config.is_comfyui() {
                return comfyui(config);
            }
            let (key, _, _) = current_key().ok_or("no Gemini API key is set")?;
            let gemini = photocraft_genai::Gemini::new(key, &config.gemini_model).map_err(|e| e.to_string())?;
            Ok(Arc::new(gemini) as Arc<dyn ImageGenerator>)
        }),
    }
}

/// A local ComfyUI server running the built-in SDXL inpainting workflow, or the workflow file
/// set in Preferences › Integrations.
fn comfyui(config: &GenConfig) -> Result<Arc<dyn ImageGenerator>, String> {
    use photocraft_genai::comfy::{ComfyUi, Workflow};
    let path = config.comfyui_workflow.trim().trim_matches('"');
    let workflow = if path.is_empty() {
        Workflow::SdxlInpaint { checkpoint: config.comfyui_checkpoint.trim().to_string() }
    } else {
        // Workflows are small JSON files; refuse anything that isn't.
        let meta = std::fs::metadata(path).map_err(|e| format!("ComfyUI workflow file {path}: {e}"))?;
        if meta.len() > 16 * 1024 * 1024 {
            return Err(format!("ComfyUI workflow file {path} is too large to be a workflow"));
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("ComfyUI workflow file {path}: {e}"))?;
        let graph: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("ComfyUI workflow file {path}: not JSON ({e})"))?;
        let name = std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("custom workflow").to_string();
        Workflow::Custom { name, graph }
    };
    let comfy = ComfyUi::new(&config.comfyui_url, workflow).map_err(|e| e.to_string())?;
    Ok(Arc::new(comfy) as Arc<dyn ImageGenerator>)
}

/// The key in use: (key, where it came from, whether Remove can delete it).
fn current_key() -> Option<(String, &'static str, bool)> {
    if let Some(k) = store::load().filter(|k| !k.trim().is_empty()) {
        return Some((k, store::NAME, true));
    }
    ENV_VARS.iter().find_map(|v| std::env::var(v).ok().filter(|k| !k.trim().is_empty()).map(|k| (k, if *v == "GEMINI_API_KEY" { "GEMINI_API_KEY" } else { "GOOGLE_API_KEY" }, false)))
}

/// "…k3Qa": enough to recognise a key, never enough to use it.
fn hint(key: &str) -> String {
    let chars: Vec<char> = key.trim().chars().collect();
    let tail: String = chars.iter().skip(chars.len().saturating_sub(4)).collect();
    format!("…{tail}")
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod store {
    use super::{ACCOUNT, SERVICE};

    #[cfg(target_os = "windows")]
    pub const NAME: &str = "Windows Credential Manager";
    #[cfg(target_os = "macos")]
    pub const NAME: &str = "the macOS Keychain";

    fn entry() -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| e.to_string())
    }

    pub fn load() -> Option<String> {
        entry().ok()?.get_password().ok()
    }

    pub fn save(key: &str) -> Result<(), String> {
        if key.is_empty() {
            return Err("the key is empty".into());
        }
        entry()?.set_password(key).map_err(|e| e.to_string())
    }

    pub fn delete() -> Result<(), String> {
        match entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod store {
    pub const NAME: &str = "a private file in the settings folder";

    fn path() -> Option<std::path::PathBuf> {
        crate::app_dirs::config_dir().map(|d| d.join("gemini-api-key"))
    }

    pub fn load() -> Option<String> {
        std::fs::read_to_string(path()?).ok().map(|s| s.trim().to_string())
    }

    pub fn save(key: &str) -> Result<(), String> {
        if key.is_empty() {
            return Err("the key is empty".into());
        }
        let path = path().ok_or("no settings folder")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        #[cfg(unix)]
        {
            use std::io::Write as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path).map_err(|e| e.to_string())?;
            f.write_all(key.as_bytes()).map_err(|e| e.to_string())
        }
        #[cfg(not(unix))]
        std::fs::write(&path, key).map_err(|e| e.to_string())
    }

    pub fn delete() -> Result<(), String> {
        match path().map(std::fs::remove_file) {
            None | Some(Ok(())) => Ok(()),
            Some(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Some(Err(e)) => Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn hints_show_only_the_end_of_the_key() {
        assert_eq!(super::hint("AIzaSyExampleKey1234"), "…1234");
        assert_eq!(super::hint("ab"), "…ab");
        assert_eq!(super::hint(""), "…");
    }
}
