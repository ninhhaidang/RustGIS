//! The desktop app's preferences, `prefs.json` in the settings folder: one JSON object of
//! string values. The desktop reads and writes it; the command line and Python read it for
//! `--use-saved-secrets` / `use_saved_secrets`.

use rustgis_data::secrets::{Reporting, SecretStore};
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub const FILE: &str = "prefs.json";

static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Reads the desktop preferences. A missing file is normal; unreadable or malformed files
/// return an error without including any preference values in the diagnostic.
pub fn read() -> Result<Map<String, Value>, String> {
    match rustgis_data::dirs::config_dir() {
        Some(dir) => read_at(&dir.join(FILE)),
        None => Ok(Map::new()),
    }
}

fn read_at(path: &Path) -> Result<Map<String, Value>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => return Err(format!("could not read preferences: {error}")),
    };
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| format!("invalid preferences JSON at line {}, column {}", error.line(), error.column()))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err("preferences must be a JSON object".into()),
    }
}

/// Updates one desktop preference, preserving all others. A malformed file is moved aside to
/// `prefs.json.bad` (kept for the user to repair) and a new one started; an unreadable one is
/// never overwritten. A temporary file and rename avoid truncating the old file on a crash.
pub fn write(key: &str, value: &str) -> Result<(), String> {
    let dir = rustgis_data::dirs::config_dir().ok_or("the settings folder isn't available")?;
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    write_at(&dir.join(FILE), key, value)
}

fn write_at(path: &Path, key: &str, value: &str) -> Result<(), String> {
    let mut map = match read_at(path) {
        Ok(map) => map,
        Err(_) if path.is_file() && std::fs::read_to_string(path).is_ok() => {
            std::fs::rename(path, path.with_extension("json.bad"))
                .map_err(|error| format!("could not set aside the malformed preferences: {error}"))?;
            Map::new()
        }
        Err(error) => return Err(error),
    };
    map.insert(key.to_string(), Value::String(value.to_string()));
    let text = serde_json::to_vec(&map).map_err(|_| "could not serialize preferences")?;
    let dir = path.parent().ok_or("the settings folder isn't available")?;
    let temporary = path.with_extension("json.tmp");
    std::fs::create_dir_all(dir)
        .and_then(|_| std::fs::write(&temporary, text))
        .and_then(|_| std::fs::rename(&temporary, path))
        .map_err(|error| format!("could not save preferences: {error}"))
}

/// Opts a native caller into the desktop's saved connections and lazy credential store, which
/// also keeps the Earthdata Login token remembered on this computer.
/// Constructing the store and loading connection definitions never reads a credential.
/// Both preference failures and once-only store failures use the caller's diagnostic sink.
/// Returns the same preference snapshot for assistant provider/key selection.
pub fn opt_in(
    s: &mut crate::Session,
    store: impl SecretStore + 'static,
    warn: impl Fn(String) + Send + Sync + 'static,
) -> Map<String, Value> {
    let prefs = read().unwrap_or_else(|error| {
        warn(format!("{error}; going on without saved preferences"));
        Map::new()
    });
    let store = Reporting::new(store, move |error| {
        warn(format!("saved secrets can't be used ({error}); going on without them"));
    });
    let store: Arc<dyn SecretStore> = Arc::new(store);
    crate::render::rustgis_tiles::earthdata::set_secret_store(Some(store.clone()));
    s.saved_secrets_opted_in = true;
    use_saved_secrets(s, &prefs, store);
    prefs
}

/// Gives `s` the desktop's saved connections and their keychain. Connection commands use the
/// desktop's matching rules (same id, kind and address or endpoint; typed passwords and keys
/// win), with a database client's environment password (`PGPASSWORD`…) before the saved one.
/// Changes to the connections aren't written back. Missing preferences never import legacy
/// connections.
pub fn use_saved_secrets(s: &mut crate::Session, prefs: &Map<String, Value>, store: Arc<dyn SecretStore>) {
    if let Some(text) = prefs.get(crate::connections::PREF).and_then(Value::as_str) {
        s.connections = crate::connections::Connections::from_json(text);
    }
    s.database_logins.set_store(Some(store));
    s.database_logins.prefer_environment_passwords();
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn preference_updates_preserve_other_values_and_set_malformed_files_aside() {
        let dir = std::env::temp_dir().join(format!("rustgis-preferences-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        assert_eq!(read_at(&path).unwrap(), Map::new());
        write_at(&path, "connections", "saved definitions").unwrap();
        write_at(&path, "theme", "dark").unwrap();
        let map = read_at(&path).unwrap();
        assert_eq!(map.get("connections"), Some(&Value::String("saved definitions".into())));
        assert_eq!(map.get("theme"), Some(&Value::String("dark".into())));
        let broken = r#"{"sensitive-pref":"prefs-sentinel", broken}"#;
        std::fs::write(&path, broken).unwrap();
        let error = read_at(&path).unwrap_err();
        assert!(!error.contains("prefs-sentinel"));
        write_at(&path, "theme", "light").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("prefs.json.bad")).unwrap(), broken);
        assert_eq!(read_at(&path).unwrap().get("theme"), Some(&Value::String("light".into())));
        std::fs::write(&path, r#"["prefs-sentinel"]"#).unwrap();
        assert!(!read_at(&path).unwrap_err().contains("prefs-sentinel"));
        assert!(read_at(&dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
