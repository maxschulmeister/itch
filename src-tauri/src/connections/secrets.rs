//! API tokens for connections.
//!
//! Release builds use the OS keychain. Debug builds use a JSON file in the app
//! data folder, because macOS asks for keychain access again after each rebuild
//! of an unsigned binary.

use tauri::AppHandle;

#[cfg(not(debug_assertions))]
const SERVICE: &str = "itch-connection";

#[cfg(not(debug_assertions))]
pub fn set(_app: &AppHandle, id: &str, token: &str) -> Result<(), String> {
    keyring::Entry::new(SERVICE, id)
        .and_then(|entry| entry.set_password(token))
        .map_err(|e| format!("Could not save the token in the keychain: {e}"))
}

#[cfg(not(debug_assertions))]
pub fn get(_app: &AppHandle, id: &str) -> Result<String, String> {
    keyring::Entry::new(SERVICE, id)
        .and_then(|entry| entry.get_password())
        .map_err(|e| format!("Could not read the token from the keychain: {e}"))
}

#[cfg(not(debug_assertions))]
pub fn delete(_app: &AppHandle, id: &str) -> Result<(), String> {
    match keyring::Entry::new(SERVICE, id).and_then(|entry| entry.delete_credential()) {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("Could not delete the token from the keychain: {e}")),
    }
}

#[cfg(debug_assertions)]
mod dev_store {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tauri::{AppHandle, Manager};

    fn path(app: &AppHandle) -> Result<PathBuf, String> {
        let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(dir.join("dev-secrets.json"))
    }

    pub fn load(app: &AppHandle) -> Result<HashMap<String, String>, String> {
        match std::fs::read_to_string(path(app)?) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn save(app: &AppHandle, secrets: &HashMap<String, String>) -> Result<(), String> {
        let file = path(app)?;
        let text = serde_json::to_string_pretty(secrets).map_err(|e| e.to_string())?;
        std::fs::write(&file, text).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

#[cfg(debug_assertions)]
pub fn set(app: &AppHandle, id: &str, token: &str) -> Result<(), String> {
    let mut secrets = dev_store::load(app)?;
    secrets.insert(id.to_string(), token.to_string());
    dev_store::save(app, &secrets)
}

#[cfg(debug_assertions)]
pub fn get(app: &AppHandle, id: &str) -> Result<String, String> {
    dev_store::load(app)?
        .remove(id)
        .ok_or_else(|| "No token is stored for this connection".to_string())
}

#[cfg(debug_assertions)]
pub fn delete(app: &AppHandle, id: &str) -> Result<(), String> {
    let mut secrets = dev_store::load(app)?;
    secrets.remove(id);
    dev_store::save(app, &secrets)
}
