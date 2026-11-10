//! The desktop app's preferences, `prefs.json` in the settings folder: one JSON object of
//! string values. The desktop reads and writes it; the command line and Python read it for
//! `--use-saved-secrets` / `use_saved_secrets`.

use rustgis_data::secrets::{Reporting, SecretStore};
use serde_json::{Map, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
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
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => return Err(format!("could not read preferences: {error}")),
    };
    parse_preferences(&bytes)
}

/// Parses only bytes successfully read, keeping malformed content distinct from I/O failures.
fn parse_preferences(bytes: &[u8]) -> Result<Map<String, Value>, String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid preferences JSON at line {}, column {}", error.line(), error.column()))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err("preferences must be a JSON object".into()),
    }
}

/// Updates one desktop preference, preserving all others. A malformed file is set aside (see
/// [`set_aside`]) and a new one started: returns where it was put. An unreadable one is never
/// overwritten. The new file is written under a name of its own beside the old one, synced to
/// disk, then renamed over it, so a crash or power cut leaves the old file or the new one, never
/// a partly written one, and two copies of RustGIS saving at once never share a temporary file.
pub fn write(key: &str, value: &str) -> Result<Option<PathBuf>, String> {
    let dir = rustgis_data::dirs::config_dir().ok_or("the settings folder isn't available")?;
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    write_at(&dir.join(FILE), key, value)
}

/// [`write`] to the file at `path` (a test folder in tests): returns where a malformed file went.
fn write_at(path: &Path, key: &str, value: &str) -> Result<Option<PathBuf>, String> {
    write_at_with_sync(path, key, value, sync_directory)
}

/// Separates replacement from its final directory flush; an unconfirmed flush must not claim
/// that the already-visible file was left unchanged. The callback permits a post-rename fault test.
fn write_at_with_sync(
    path: &Path,
    key: &str,
    value: &str,
    sync: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<Option<PathBuf>, String> {
    write_at_with_bytes(path, key, value, std::fs::read(path), sync)
}

/// Uses one read result for a save. Only successfully read, malformed bytes permit recovery;
/// a transient read failure cannot replace a valid file with a new one containing a single key.
fn write_at_with_bytes(
    path: &Path,
    key: &str,
    value: &str,
    bytes: std::io::Result<Vec<u8>>,
    sync: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<Option<PathBuf>, String> {
    let (mut map, set_aside) = match bytes {
        Ok(bytes) => match parse_preferences(&bytes) {
            Ok(map) => (map, None),
            Err(_) => (Map::new(), Some(set_aside(path, &bytes)?)),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Map::new(), None),
        Err(error) => return Err(format!("could not read preferences: {error}")),
    };
    map.insert(key.to_string(), Value::String(value.to_string()));
    let text = serde_json::to_vec(&map).map_err(|_| "could not serialize preferences")?;
    let dir = path.parent().ok_or("the settings folder isn't available")?;
    std::fs::create_dir_all(dir).and_then(|_| rustgis_data::io::replace_atomic(path, &text)).map_err(|error| match &set_aside {
        Some(backup) => format!("could not save preferences: {error}; malformed preferences were kept at {}", backup.display()),
        None => format!("could not save preferences: {error}"),
    })?;
    sync(path).map_err(|error| match &set_aside {
        Some(backup) => format!(
            "preferences were replaced, but their durability could not be confirmed: {error}; malformed preferences were kept at {}",
            backup.display()
        ),
        None => format!("preferences were replaced, but their durability could not be confirmed: {error}"),
    })?;
    Ok(set_aside)
}

/// How many malformed preference files are kept: `prefs.json.bad`, then `prefs.json.bad2` to
/// `prefs.json.bad9`. An identical existing copy is reused; when all contain different bytes,
/// saving refuses to replace any recovery copy or the malformed current file.
const KEPT_BAD: u32 = 9;

/// Copies the exact bytes that failed parsing to an exclusively created `.bad` file, synced before saving
/// replaces the original. Concurrent callers cannot overwrite each other's recovery files.
/// Failed copies remain reserved: another process may already have verified and reused one.
/// A copy or sync failure stops replacement and reports the possibly incomplete recovery path.
/// Retries reuse an identical regular recovery file, without spending another recovery slot.
fn set_aside(path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    set_aside_with_sync(path, bytes, |backup, target| backup.sync_all().and_then(|_| sync_directory(target)))
}

/// Copies into an exclusive recovery name and confirms it before returning. A failing creator
/// never deletes the name, because another writer can independently sync and reuse its bytes.
fn set_aside_with_sync(path: &Path, bytes: &[u8], sync: impl Fn(&std::fs::File, &Path) -> std::io::Result<()>) -> Result<PathBuf, String> {
    let name = |n: u32| path.with_extension(if n == 1 { "json.bad".to_string() } else { format!("json.bad{n}") });
    let retained = |error: std::io::Error, target: &Path| {
        format!(
            "could not confirm recovery of malformed preferences: {error}; recovery copy retained at {} and may be incomplete",
            target.display()
        )
    };
    let confirm =
        |copy: &std::fs::File, target: &Path| sync(copy, target).map(|()| target.to_path_buf()).map_err(|error| retained(error, target));
    let targets: Vec<PathBuf> = (1..=KEPT_BAD).map(name).collect();
    for target in &targets {
        if let Ok(Some(copy)) = same_contents(bytes, target) {
            return confirm(&copy, target);
        }
    }
    for target in targets {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut backup = match options.open(&target) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Another process may have completed this copy after the first scan.
                if let Ok(Some(copy)) = same_contents(bytes, &target) {
                    return confirm(&copy, &target);
                }
                continue;
            }
            Err(error) => return Err(format!("could not preserve malformed preferences: {error}")),
        };
        if let Err(error) = backup.write_all(bytes) {
            return Err(retained(error, &target));
        }
        return confirm(&backup, &target);
    }
    Err(format!(
        "all {KEPT_BAD} preference recovery files are occupied; repair or move one before saving settings; the malformed preferences were left unchanged at {}",
        path.display()
    ))
}

/// Makes a created recovery name or replaced preference name durable on Unix before success.
/// Other platforms keep the synced-file guarantee; opening directories is not portable there.
fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        std::fs::File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Returns the open copy when its bytes match, so the caller can confirm its durability.
/// Symlink paths and read errors do not establish a match; sync failures remain distinct.
fn same_contents(source: &[u8], target: &Path) -> std::io::Result<Option<std::fs::File>> {
    if !std::fs::symlink_metadata(target)?.file_type().is_file() {
        return Ok(None);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    // Linux can sync a read-only descriptor; retain the other platforms' flush access.
    #[cfg(not(target_os = "linux"))]
    options.write(true);
    let mut copy = options.open(target)?;
    if source.len() as u64 != copy.metadata()?.len() {
        return Ok(None);
    }
    let mut right = [0u8; 8192];
    for left in source.chunks(right.len()) {
        copy.read_exact(&mut right[..left.len()])?;
        if left != &right[..left.len()] {
            return Ok(None);
        }
    }
    if copy.read(&mut right[..1])? != 0 {
        return Ok(None);
    }
    Ok(Some(copy))
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

    /// Recovery preserves the malformed snapshot, even if the path now contains another save.
    #[test]
    fn recovery_keeps_the_bytes_that_failed_parsing() {
        let dir = std::env::temp_dir().join(format!("rustgis-preferences-snapshot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        let malformed = b"malformed original";
        assert!(parse_preferences(malformed).is_err());
        let current = br#"{"theme":"dark","other":"kept"}"#;
        std::fs::write(&path, current).unwrap();
        let backup = set_aside(&path, malformed).unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), malformed);
        assert_eq!(std::fs::read(&path).unwrap(), current);
        assert_eq!(set_aside(&path, malformed).unwrap(), backup);
        assert!(!path.with_extension("json.bad2").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A failed read must not be reclassified as malformed when the file is readable again.
    #[test]
    fn transient_read_failure_preserves_valid_preferences() {
        let dir = std::env::temp_dir().join(format!("rustgis-preferences-read-retry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        let original = br#"{"theme":"dark","other":"kept"}"#;
        std::fs::write(&path, original).unwrap();
        let error = write_at_with_bytes(
            &path,
            "theme",
            "light",
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "transient read failure")),
            sync_directory,
        )
        .unwrap_err();
        assert!(error.contains("could not read preferences") && error.contains("transient read failure"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(!path.with_extension("json.bad").exists());
        assert_eq!(write_at(&path, "theme", "light").unwrap(), None);
        let saved = read_at(&path).unwrap();
        assert_eq!(saved.get("theme").and_then(Value::as_str), Some("light"));
        assert_eq!(saved.get("other").and_then(Value::as_str), Some("kept"));
        assert!(!path.with_extension("json.bad").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A verified copy whose durability cannot be confirmed must not spend another recovery slot.
    #[test]
    fn repeated_recovery_sync_failures_keep_one_slot() {
        let dir = std::env::temp_dir().join(format!("rustgis-preferences-sync-retry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        std::fs::write(&path, "malformed original").unwrap();
        for _ in 0..2 {
            let error =
                set_aside_with_sync(&path, b"malformed original", |_, _| Err(std::io::Error::other("directory sync failed"))).unwrap_err();
            assert!(error.contains("directory sync failed") && error.contains("recovery copy retained"), "{error}");
            assert!(!path.with_extension("json.bad2").exists());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "malformed original");
        }
        assert_eq!(set_aside(&path, &std::fs::read(&path).unwrap()).unwrap(), path.with_extension("json.bad"));
        assert!(!path.with_extension("json.bad2").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A second writer can finish a save while the recovery creator is still confirming it.
    /// A later creator error must not remove the recovery copy that the successful writer used.
    #[test]
    fn a_creator_sync_error_keeps_a_recovery_copy_another_writer_reused() {
        let dir = std::env::temp_dir().join(format!("rustgis-preferences-reused-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        std::fs::write(&path, "malformed original").unwrap();
        let error = set_aside_with_sync(&path, b"malformed original", |_, target| {
            let reused = write_at(&path, "theme", "dark").map_err(std::io::Error::other)?;
            assert_eq!(reused.as_deref(), Some(target));
            Err(std::io::Error::other("creator sync failed"))
        })
        .unwrap_err();
        assert!(error.contains("recovery copy retained") && error.contains("creator sync failed"), "{error}");
        assert_eq!(read_at(&path).unwrap().get("theme").and_then(Value::as_str), Some("dark"));
        assert_eq!(std::fs::read_to_string(path.with_extension("json.bad")).unwrap(), "malformed original");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn full_recovery_slots_leave_the_malformed_file_and_every_backup_unchanged() {
        let dir = std::env::temp_dir().join(format!("rustgis-preferences-full-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        std::fs::write(&path, "broken preferences").unwrap();
        let mut kept = Vec::new();
        for n in 0..KEPT_BAD {
            let text = format!("broken preferences {n}");
            std::fs::write(&path, &text).unwrap();
            kept.push((set_aside(&path, &std::fs::read(&path).unwrap()).unwrap(), text));
        }
        std::fs::write(&path, "broken preferences").unwrap();
        assert!(write_at(&path, "theme", "dark").unwrap_err().contains("left unchanged"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken preferences");
        for (backup, text) in kept {
            assert_eq!(std::fs::read_to_string(backup).unwrap(), text);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn preference_updates_preserve_other_values_and_set_malformed_files_aside() {
        let dir = std::env::temp_dir().join(format!("rustgis-preferences-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        assert_eq!(read_at(&path).unwrap(), Map::new());
        assert_eq!(write_at(&path, "connections", "saved definitions").unwrap(), None);
        assert_eq!(write_at(&path, "theme", "dark").unwrap(), None);
        let map = read_at(&path).unwrap();
        assert_eq!(map.get("connections"), Some(&Value::String("saved definitions".into())));
        assert_eq!(map.get("theme"), Some(&Value::String("dark".into())));
        // A failed final flush leaves the replacement visible, with an honest durability error.
        let error = write_at_with_sync(&path, "theme", "light", |_| {
            Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "directory sync unavailable"))
        })
        .unwrap_err();
        assert!(error.contains("preferences were replaced") && error.contains("durability could not be confirmed"), "{error}");
        assert_eq!(read_at(&path).unwrap().get("theme"), Some(&Value::String("light".into())));
        let broken = r#"{"sensitive-pref":"prefs-sentinel", broken}"#;
        std::fs::write(&path, broken).unwrap();
        let error = read_at(&path).unwrap_err();
        assert!(!error.contains("prefs-sentinel"));
        assert_eq!(write_at(&path, "theme", "light").unwrap(), Some(dir.join("prefs.json.bad")));
        assert_eq!(std::fs::read_to_string(dir.join("prefs.json.bad")).unwrap(), broken);
        assert_eq!(read_at(&path).unwrap().get("theme"), Some(&Value::String("light".into())));
        // A file cut short (as a power cut can leave one) is set aside beside the first, not over it.
        std::fs::write(&path, "").unwrap();
        assert_eq!(write_at(&path, "theme", "dark").unwrap(), Some(dir.join("prefs.json.bad2")));
        assert_eq!(std::fs::read_to_string(dir.join("prefs.json.bad")).unwrap(), broken);
        assert_eq!(write_at(&path, "theme", "light").unwrap(), None);
        // Failed replacements leave the malformed original: repeated preservation reuses its copy.
        // Protecting the existing copy on Linux must not spend more recovery slots.
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.join("prefs.json.bad"), std::fs::Permissions::from_mode(0o444)).unwrap();
        }
        std::fs::write(&path, broken).unwrap();
        for _ in 0..=KEPT_BAD {
            assert_eq!(set_aside(&path, &std::fs::read(&path).unwrap()).unwrap(), dir.join("prefs.json.bad"));
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
        assert_eq!(write_at(&path, "theme", "light").unwrap(), Some(dir.join("prefs.json.bad")));
        // Invalid UTF-8 is malformed JSON too; recovery preserves the exact original bytes.
        let invalid = [0xff, 0xfe];
        std::fs::write(&path, invalid).unwrap();
        assert!(read_at(&path).is_err());
        assert_eq!(write_at(&path, "theme", "light").unwrap(), Some(dir.join("prefs.json.bad3")));
        assert_eq!(std::fs::read(dir.join("prefs.json.bad3")).unwrap(), invalid);
        // No temporary file is left behind.
        let mut names: Vec<String> =
            std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["prefs.json", "prefs.json.bad", "prefs.json.bad2", "prefs.json.bad3"]);
        std::fs::write(&path, r#"["prefs-sentinel"]"#).unwrap();
        assert!(!read_at(&path).unwrap_err().contains("prefs-sentinel"));
        assert!(read_at(&dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
