//! Edit › Generative Fill on the desktop (custom): the Gemini API key in the OS credential store
//! and the model built from it.
//!
//! The key is kept in Windows Credential Manager or the macOS Keychain (the `keyring` crate). On
//! Linux, where a Secret Service may not be running, it is a file only the user can read in the
//! config directory. Without a stored key the `GEMINI_API_KEY` (or `GOOGLE_API_KEY`) environment
//! variable is used. The key never goes into preferences, which agents can read.

use std::sync::Arc;

use photocraft_engine::generative_cmds::ImageGenerator;
use photocraft_ui_egui::generative_ui::{GenerativeService, KeyInfo};

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
        generator: Box::new(|model| {
            let (key, _, _) = current_key().ok_or("no Gemini API key is set")?;
            let gemini = photocraft_genai::Gemini::new(key, model).map_err(|e| e.to_string())?;
            Ok(Arc::new(gemini) as Arc<dyn ImageGenerator>)
        }),
    }
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
