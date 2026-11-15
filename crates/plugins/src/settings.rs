//! Settings a plugin keeps between calls and between runs of RustGIS: `host.settings` reads them
//! and `host.setSettings` changes them. A plugin's commands have them, whether it is a
//! WebAssembly module, a process plugin or a session program between its commands; its tools
//! don't (they run off the UI thread, in models and on copies, so their output must not depend
//! on what was saved). They need no permission: a plugin sees only its own, by the id in its
//! manifest, never one a request names.
//!
//! One JSON object per plugin, at most [`MAX_BYTES`] of compact JSON (keys included). A
//! `host.setSettings {"values": {...}}` request merges its values in at the top level: a value
//! replaces the whole value saved under its key, `null` removes the key, other keys stay. The
//! answer is the whole object as saved. A change that would pass the limit, or can't be saved,
//! is an error and leaves what was saved as it was. Saved settings that can't be read are an
//! error too, and a change never replaces them. Each is saved as `{"version": 1, "values":
//! {...}}`: a RustGIS refuses a newer version than it reads and never writes over it.
//!
//! Settings are preferences, not part of the project: a change holds as soon as the request
//! succeeds, whatever happens to the command later and whatever undo does. They survive
//! restarts, updates of the plugin and turning it off and on, and are erased only when the user
//! uninstalls it ([`remove`]).
//!
//! - The desktop, the command line and Python keep `settings/<id>.json` in the plugins folder
//!   ([`crate::store::dir`]: `RUSTGIS_PLUGINS_DIR`, or `plugins` in the data folder), so they
//!   follow it. Each change is read, merged and written under one lock, and the file is replaced
//!   atomically. RustGIS processes running at once share the files: the last write wins.
//! - The web app keeps them where its host page says ([`set_browser_storage`]): localStorage,
//!   or memory only when embedded.
//!
//! Errors say what was being done and to which plugin, never what the settings hold or where
//! they are kept: the plugin sees them.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::{Host, manifest};

/// The most a plugin's settings may take, as compact JSON: 64 KiB.
pub const MAX_BYTES: usize = 64 * 1024;

/// The version of the saved record this RustGIS reads and writes.
const VERSION: u64 = 1;
/// What a saved record holds before the plugin's object: `{"version":1,"values":<object>}`.
const HEAD: &str = "{\"version\":1,\"values\":";
/// The most a saved record takes: the plugin's object and what is around it.
const MAX_SAVED: usize = MAX_BYTES + HEAD.len() + 1;

/// The request that reads a plugin's settings.
pub const READ: &str = "host.settings";
/// The request that changes a plugin's settings.
pub const WRITE: &str = "host.setSettings";

/// Where settings are kept: one saved text per plugin id.
trait Backing {
    /// The text saved for `id`, or `None` when nothing was. Filesystem reads stop one byte past
    /// [`MAX_SAVED`]; browser storage already holds the whole string. [`parse`] checks both.
    fn load(&self, id: &str) -> Result<Option<String>, String>;
    /// Replaces the text saved for `id`; a failure leaves what was saved as it was.
    fn save(&self, id: &str, text: &str) -> Result<(), String>;
    /// Erases what was saved for `id`; nothing saved is not an error.
    fn delete(&self, id: &str) -> Result<(), String>;
}

fn check_id(id: &str) -> Result<(), String> {
    if manifest::valid_id(id, false) { Ok(()) } else { Err("plugin settings: not a valid plugin id".into()) }
}

fn damaged(id: &str) -> String {
    format!("the saved settings of '{id}' are damaged or larger than {MAX_BYTES} bytes: uninstalling the plugin erases them")
}

fn newer(id: &str) -> String {
    format!("the saved settings of '{id}' are from a newer RustGIS: this one leaves them as they are")
}

fn too_big() -> String {
    format!("plugin settings exceed {MAX_BYTES} bytes")
}

/// The record saved for a plugin whose object is `values` (compact JSON).
fn record(values: &str) -> String {
    format!("{HEAD}{values}}}")
}

/// A writer that keeps what it is given up to [`MAX_BYTES`] and fails past it, so a hostile value
/// is never written out whole.
struct Capped(Vec<u8>);

impl std::io::Write for Capped {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        // `self.0` never holds more than MAX_BYTES.
        if data.len() > MAX_BYTES - self.0.len() {
            return Err(std::io::Error::other("over the limit"));
        }
        self.0.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `value` as compact JSON, or `None` when that takes more than [`MAX_BYTES`].
fn compact(value: &impl serde::Serialize) -> Option<String> {
    let mut out = Capped(Vec::new());
    serde_json::to_writer(&mut out, value).ok()?;
    String::from_utf8(out.0).ok()
}

/// What was saved for `id`: the plugin's object, and apart from it any other top-level keys of
/// the record (version 1 writes none, so only by hand), which a change writes back as they were.
/// It must be a record of this version, within the limit as written and, its object, as compact
/// JSON (a hand-edited file may spell its numbers longer than they are saved). A newer version is
/// refused, so it is never written over.
fn parse(id: &str, text: &str) -> Result<(Map<String, Value>, Map<String, Value>), String> {
    if text.len() > MAX_SAVED {
        return Err(damaged(id));
    }
    let Ok(Value::Object(mut saved)) = serde_json::from_str::<Value>(text) else { return Err(damaged(id)) };
    match saved.get("version").and_then(Value::as_u64) {
        Some(VERSION) => {}
        Some(v) if v > VERSION => return Err(newer(id)),
        _ => return Err(damaged(id)),
    }
    let Some(Value::Object(map)) = saved.remove("values") else { return Err(damaged(id)) };
    compact(&map).ok_or_else(|| damaged(id))?;
    saved.remove("version");
    Ok((map, saved))
}

fn saved(backing: &dyn Backing, id: &str) -> Result<(Map<String, Value>, Map<String, Value>), String> {
    match backing.load(id)? {
        Some(text) => parse(id, &text),
        None => Ok((Map::new(), Map::new())),
    }
}

fn current(backing: &dyn Backing, id: &str) -> Result<Map<String, Value>, String> {
    saved(backing, id).map(|(values, _)| values)
}

/// The record for `values` (compact JSON) with the other keys a saved record had, within what a
/// record may take.
fn record_with(values: &str, rest: &Map<String, Value>) -> Result<String, String> {
    let mut text = record(values);
    if rest.is_empty() {
        return Ok(text);
    }
    text.pop();
    for (key, value) in rest {
        let key = serde_json::to_string(key).map_err(|e| e.to_string())?;
        let value = serde_json::to_string(value).map_err(|e| e.to_string())?;
        text.push_str(&format!(",{key}:{value}"));
    }
    text.push('}');
    if text.len() > MAX_SAVED { Err(too_big()) } else { Ok(text) }
}

/// Merges `params.values` into what is saved for `id` and saves the result, checking the limit
/// on the merged object before anything is written.
fn patch_in(backing: &dyn Backing, id: &str, params: &Value) -> Result<Value, String> {
    let updates = params.get("values").and_then(Value::as_object).ok_or("values must be a JSON object")?;
    let (saved, rest) = saved(backing, id)?;
    // Borrowed, not cloned: the values asked for may be as large as a request can be.
    let mut merged: BTreeMap<&str, &Value> = BTreeMap::new();
    for (key, value) in &saved {
        if !updates.contains_key(key) {
            merged.insert(key, value);
        }
    }
    for (key, value) in updates {
        if !value.is_null() {
            merged.insert(key, value);
        }
    }
    let text = compact(&merged).ok_or_else(too_big)?;
    backing.save(id, &record_with(&text, &rest)?)?;
    // Within the limit by now: cheap to own.
    Ok(Value::Object(merged.into_iter().map(|(k, v)| (k.to_string(), v.clone())).collect()))
}

/// `id`'s settings: `{}` when it has saved none.
pub fn read(id: &str) -> Result<Value, String> {
    check_id(id)?;
    with_backing(|b| current(b, id)).map(Value::Object)
}

/// Changes `id`'s settings as `host.setSettings` asks (`params` is `{"values": {...}}`, see the
/// module) and returns the whole object saved.
pub fn patch(id: &str, params: &Value) -> Result<Value, String> {
    check_id(id)?;
    with_backing(|b| patch_in(b, id, params))
}

/// Erases `id`'s settings (the plugin was uninstalled). Nothing saved, or a saved record that is
/// damaged, are both fine.
pub fn remove(id: &str) -> Result<(), String> {
    check_id(id)?;
    remove_saved(id)
}

/// A plugin's view of the host: its settings requests answered here, by its own id, and every
/// other request, log line, progress line and cancel check passed on to `host`.
pub(crate) struct Scoped<'a> {
    id: &'a str,
    host: &'a mut dyn Host,
}

impl<'a> Scoped<'a> {
    pub(crate) fn new(id: &'a str, host: &'a mut dyn Host) -> Self {
        Self { id, host }
    }
}

impl Host for Scoped<'_> {
    fn call(&mut self, op: &str, params: &Value) -> Result<Value, String> {
        match op {
            READ => read(self.id),
            WRITE => patch(self.id, params),
            _ => self.host.call(op, params),
        }
    }

    fn log(&mut self, text: &str) {
        self.host.log(text);
    }

    fn progress(&mut self, text: &str) {
        self.host.progress(text);
    }

    fn cancelled(&self) -> bool {
        self.host.cancelled()
    }
}

#[cfg(not(target_arch = "wasm32"))]
use native::{remove_saved, with_backing};

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use super::{Backing, MAX_SAVED, damaged};

    /// Held while settings are read, changed or erased, so one change never interleaves with
    /// another in this process (a command and a session program can both write). Other RustGIS
    /// processes: the last to write wins, as with the preferences.
    static LOCK: Mutex<()> = Mutex::new(());

    /// The folder holding one file per plugin: `settings` in the plugins folder, so it follows
    /// `RUSTGIS_PLUGINS_DIR` and `store::set_dir`.
    fn dir() -> Option<PathBuf> {
        crate::store::dir().map(|d| d.join("settings"))
    }

    pub(super) struct Dir<'a>(pub(super) &'a Path);

    impl Dir<'_> {
        fn file(&self, id: &str) -> PathBuf {
            self.0.join(format!("{id}.json"))
        }
    }

    impl Backing for Dir<'_> {
        fn load(&self, id: &str) -> Result<Option<String>, String> {
            use std::io::Read;
            let path = self.file(id);
            // A regular file only: a named pipe would hold the read.
            match std::fs::metadata(&path) {
                Ok(m) if m.is_file() => {}
                Ok(_) => return Err(format!("the saved settings of '{id}' aren't a file: remove what is in its place by hand")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(format!("couldn't read the settings of '{id}': {e}")),
            }
            let file = std::fs::File::open(&path).map_err(|e| format!("couldn't read the settings of '{id}': {e}"))?;
            let mut bytes = Vec::new();
            file.take(MAX_SAVED as u64 + 1).read_to_end(&mut bytes).map_err(|e| format!("couldn't read the settings of '{id}': {e}"))?;
            String::from_utf8(bytes).map(Some).map_err(|_| damaged(id))
        }

        fn save(&self, id: &str, text: &str) -> Result<(), String> {
            let fail = |e: std::io::Error| format!("couldn't save the settings of '{id}': {e}");
            std::fs::create_dir_all(self.0).map_err(fail)?;
            // Replaces a link at the file rather than writing through it to wherever it points.
            rustgis_data::io::replace_atomic(&self.file(id), text.as_bytes()).map_err(fail)
        }

        fn delete(&self, id: &str) -> Result<(), String> {
            match std::fs::remove_file(self.file(id)) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("couldn't erase the saved settings of '{id}': {e}")),
            }
        }
    }

    pub(super) fn with_backing<T>(f: impl FnOnce(&dyn Backing) -> Result<T, String>) -> Result<T, String> {
        let dir = dir().ok_or("there is no folder for plugins")?;
        let _held = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        f(&Dir(&dir))
    }

    /// Without a plugins folder nothing could have been saved: there is nothing to erase.
    pub(super) fn remove_saved(id: &str) -> Result<(), String> {
        let Some(dir) = dir() else { return Ok(()) };
        let _held = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Dir(&dir).delete(id)
    }
}

#[cfg(target_arch = "wasm32")]
use browser::{remove_saved, with_backing};

#[cfg(target_arch = "wasm32")]
pub use browser::{BrowserStorage, set_browser_storage};

#[cfg(target_arch = "wasm32")]
mod browser {
    use std::cell::RefCell;

    use super::Backing;

    /// Where the web app keeps settings, by plugin id. Each function fails with the reason
    /// (storage unavailable, full, refused), which must not include what was being saved.
    pub struct BrowserStorage {
        pub read: Box<dyn Fn(&str) -> Result<Option<String>, String>>,
        pub write: Box<dyn Fn(&str, &str) -> Result<(), String>>,
        pub remove: Box<dyn Fn(&str) -> Result<(), String>>,
    }

    // The web app runs on one thread and its closures hold browser objects: not `Send`, so not a
    // `static`.
    thread_local! {
        static STORAGE: RefCell<Option<BrowserStorage>> = const { RefCell::new(None) };
    }

    /// Sets where settings are kept. Until it is called there is no place for them, and every
    /// request about them is an error rather than a pretence of keeping them.
    pub fn set_browser_storage(storage: BrowserStorage) {
        // Only a request borrows the store, and the app sets it before any plugin runs: the
        // borrow can't fail here.
        STORAGE.with(|s| {
            if let Ok(mut s) = s.try_borrow_mut() {
                *s = Some(storage);
            }
        });
    }

    struct Browser<'a>(&'a BrowserStorage);

    impl Backing for Browser<'_> {
        fn load(&self, id: &str) -> Result<Option<String>, String> {
            (self.0.read)(id).map_err(|e| format!("couldn't read the settings of '{id}': {e}"))
        }

        fn save(&self, id: &str, text: &str) -> Result<(), String> {
            (self.0.write)(id, text).map_err(|e| format!("couldn't save the settings of '{id}': {e}"))
        }

        fn delete(&self, id: &str) -> Result<(), String> {
            (self.0.remove)(id).map_err(|e| format!("couldn't erase the saved settings of '{id}': {e}"))
        }
    }

    pub(super) fn with_backing<T>(f: impl FnOnce(&dyn Backing) -> Result<T, String>) -> Result<T, String> {
        STORAGE.with(|s| match s.try_borrow().as_deref() {
            Ok(Some(storage)) => f(&Browser(storage)),
            _ => Err("the browser's storage isn't available for plugin settings".into()),
        })
    }

    pub(super) fn remove_saved(id: &str) -> Result<(), String> {
        with_backing(|b| b.delete(id))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    /// Saved texts in memory, failing on request: the same contract as a folder or localStorage.
    #[derive(Default)]
    struct Memory {
        saved: RefCell<HashMap<String, String>>,
        fail_save: Cell<bool>,
    }

    impl Backing for Memory {
        fn load(&self, id: &str) -> Result<Option<String>, String> {
            Ok(self.saved.borrow().get(id).cloned())
        }
        fn save(&self, id: &str, text: &str) -> Result<(), String> {
            if self.fail_save.get() {
                return Err(format!("couldn't save the settings of '{id}': full"));
            }
            self.saved.borrow_mut().insert(id.into(), text.into());
            Ok(())
        }
        fn delete(&self, id: &str) -> Result<(), String> {
            self.saved.borrow_mut().remove(id);
            Ok(())
        }
    }

    fn set(b: &dyn Backing, id: &str, values: Value) -> Result<Value, String> {
        patch_in(b, id, &json!({"values": values}))
    }

    /// The patch values `{"x": text}`: with `text` of `n` one-byte characters, `n + 8` bytes saved.
    fn x(text: &str) -> Value {
        json!({"x": text})
    }

    #[test]
    fn plugin_settings_merge_at_the_top_level() {
        let b = Memory::default();
        assert_eq!(current(&b, "a").unwrap(), Map::new());
        let first = json!({"favourites": ["l:123", "l:456"], "unit": "m", "nested": {"x": null}});
        // What is returned is what is saved.
        assert_eq!(set(&b, "a", first.clone()).unwrap(), first);
        // A value replaces the whole value; null removes the key; the rest stays.
        let after = set(&b, "a", json!({"unit": null, "limit": 5, "nested": {"y": 1}})).unwrap();
        assert_eq!(after, json!({"favourites": ["l:123", "l:456"], "limit": 5, "nested": {"y": 1}}));
        assert_eq!(Value::Object(current(&b, "a").unwrap()), after);
        // Nulls inside a value are data; removing a key that isn't there changes nothing.
        let after = set(&b, "a", json!({"list": [null, 1], "missing": null})).unwrap();
        assert_eq!(after["list"], json!([null, 1]));
        assert!(after.get("missing").is_none());
        // An empty update is valid and changes nothing.
        assert_eq!(set(&b, "a", json!({})).unwrap(), after);
        // Each plugin has its own.
        assert_eq!(Value::Object(current(&b, "b").unwrap()), json!({}));
        set(&b, "b", json!({"unit": "ft"})).unwrap();
        assert_eq!(current(&b, "a").unwrap().get("unit"), None);
        // Other keys of a saved record (put there by hand) are written back, not dropped.
        b.saved.borrow_mut().insert("c".into(), "{\"version\":1,\"note\":\"by hand\",\"values\":{\"a\":1}}".into());
        assert_eq!(set(&b, "c", json!({"b": 2})).unwrap(), json!({"a": 1, "b": 2}));
        let saved: Value = serde_json::from_str(&b.saved.borrow()["c"]).unwrap();
        assert_eq!(saved, json!({"version": 1, "values": {"a": 1, "b": 2}, "note": "by hand"}));
    }

    #[test]
    fn plugin_settings_values_must_be_an_object() {
        let b = Memory::default();
        for params in [json!({}), json!(null), json!({"values": null}), json!({"values": []}), json!({"values": "x"}), json!([])] {
            assert_eq!(patch_in(&b, "a", &params).unwrap_err(), "values must be a JSON object", "{params}");
        }
        assert!(b.saved.borrow().is_empty());
    }

    #[test]
    fn plugin_settings_stop_at_the_limit_counting_bytes() {
        let b = Memory::default();
        // `{"x":"` and `"}` are 8 bytes: exactly the limit fits.
        let exactly = "a".repeat(MAX_BYTES - 8);
        assert_eq!(set(&b, "a", x(&exactly)).unwrap(), x(&exactly));
        assert_eq!(b.saved.borrow()["a"].len(), MAX_SAVED);
        // One more byte doesn't, and what was saved stays.
        let e = set(&b, "a", x(&format!("{exactly}a"))).unwrap_err();
        assert_eq!(e, format!("plugin settings exceed {MAX_BYTES} bytes"));
        assert_eq!(b.saved.borrow()["a"].len(), MAX_SAVED);
        // Another key past the limit is refused the same way.
        assert!(set(&b, "a", json!({"y": 1})).is_err());
        // What is replaced is measured as replaced: smaller in place is fine, and a patch
        // removing one large value to add another is measured as it ends up.
        assert!(set(&b, "a", x("small")).is_ok());
        assert!(set(&b, "a", json!({"x": null, "y": exactly.clone()})).is_ok());
        assert!(set(&b, "a", json!({"x": exactly.clone()})).is_err(), "y is still there");
        // Escapes and multibyte text count as saved: a quote is two bytes, é two, 😀 four.
        for (unit, bytes) in [("\"", 2), ("\\", 2), ("\n", 2), ("\u{1}", 6), ("é", 2), ("€", 3), ("😀", 4)] {
            let b = Memory::default();
            let fit = (MAX_BYTES - 8) / bytes;
            assert!(set(&b, "a", x(&unit.repeat(fit))).is_ok(), "{unit:?} x {fit}");
            let used = b.saved.borrow()["a"].len() - (MAX_SAVED - MAX_BYTES);
            assert!(used <= MAX_BYTES && used > MAX_BYTES - 8 - bytes, "{unit:?}: {used}");
            // Counted as characters it would still fit; counted as bytes it doesn't.
            assert!(set(&b, "a", x(&unit.repeat(fit + 1))).is_err(), "{unit:?} x {}", fit + 1);
        }
    }

    #[test]
    fn plugin_settings_failures_leave_what_was_saved() {
        let b = Memory::default();
        set(&b, "a", json!({"keep": 1})).unwrap();
        let before = b.saved.borrow()["a"].clone();
        b.fail_save.set(true);
        let e = set(&b, "a", json!({"keep": 2, "more": 3})).unwrap_err();
        assert!(e.contains("couldn't save the settings of 'a'"), "{e}");
        assert_eq!(b.saved.borrow()["a"], before);
        b.fail_save.set(false);
        assert_eq!(set(&b, "a", json!({"more": 3})).unwrap(), json!({"keep": 1, "more": 3}));
    }

    #[test]
    fn plugin_settings_that_are_damaged_are_errors_not_empty() {
        let b = Memory::default();
        let big = record(&format!("{{\"x\":\"{}\"}}", "a".repeat(MAX_BYTES)));
        // Not a record, not of a version this reads, not holding an object, or too large.
        let records = [
            "{\"a\":1}",
            "{\"version\":\"1\",\"values\":{}}",
            "{\"version\":0,\"values\":{}}",
            "{\"version\":1}",
            "{\"version\":1,\"values\":[]}",
        ];
        for text in ["", "not json", "[1]", "null", "\"s\"", "{\"a\":", &big].into_iter().chain(records) {
            b.saved.borrow_mut().insert("a".into(), text.to_string());
            let e = current(&b, "a").unwrap_err();
            assert!(e.contains("'a'") && e.contains("damaged"), "{e}");
            // Never overwritten by a patch: the error comes first.
            assert!(set(&b, "a", json!({"y": 1})).is_err());
            assert_eq!(b.saved.borrow()["a"], text);
            // But erased by an uninstall.
            b.delete("a").unwrap();
            assert!(b.saved.borrow().get("a").is_none());
        }
        // Errors never repeat what was saved.
        b.saved.borrow_mut().insert("a".into(), "{\"secret\": tr".into());
        assert!(!current(&b, "a").unwrap_err().contains("secret"));
        // Saved by hand with numbers spelled shorter than they are written back: over the limit.
        let numbers = record(&format!("{{\"n\":[{}]}}", vec!["1e2"; (MAX_BYTES - 8) / 4].join(",")));
        assert!(numbers.len() <= MAX_SAVED);
        b.saved.borrow_mut().insert("a".into(), numbers);
        assert!(current(&b, "a").is_err());
        // Saved by a newer RustGIS: refused, never written over, still erased by an uninstall.
        let newer = "{\"version\":2,\"values\":{\"a\":1},\"more\":true}";
        b.saved.borrow_mut().insert("a".into(), newer.into());
        assert!(current(&b, "a").unwrap_err().contains("from a newer RustGIS"));
        assert!(set(&b, "a", json!({"y": 1})).is_err());
        assert_eq!(b.saved.borrow()["a"], newer);
        b.delete("a").unwrap();
        // Missing records are fine to erase.
        assert!(b.delete("nobody").is_ok());
    }

    #[test]
    fn plugin_settings_need_a_plugin_id() {
        for id in ["", "../x", "A", "a/b", "a.b", "1a", &"a".repeat(65)] {
            assert!(read(id).is_err() && remove(id).is_err() && patch(id, &json!({"values": {}})).is_err(), "{id:?}");
        }
    }

    /// A folder of its own, removed when dropped.
    #[cfg(not(target_arch = "wasm32"))]
    struct Folder(std::path::PathBuf);

    #[cfg(not(target_arch = "wasm32"))]
    impl Folder {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("rustgis-test-settings-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn plugin_settings_are_kept_in_a_file_per_plugin() {
        use native::Dir;
        let folder = Folder::new("files");
        // The folder is made on the first save, not before.
        let dir = Dir(&folder.0);
        assert_eq!(current(&dir, "one").unwrap(), Map::new());
        assert!(!folder.0.exists());
        let saved = json!({"favourites": ["l:123", "l:456"], "unit": "m", "nested": {"x": null}});
        set(&dir, "one", saved.clone()).unwrap();
        set(&dir, "two", json!({"unit": "ft"})).unwrap();
        // A compact record in `<id>.json`; a new store over the same folder reads the same.
        let text = std::fs::read_to_string(folder.0.join("one.json")).unwrap();
        assert_eq!(text, record(&compact(&saved.as_object().unwrap()).unwrap()));
        assert_eq!(Value::Object(current(&Dir(&folder.0), "one").unwrap()), saved);
        assert_eq!(Value::Object(current(&dir, "two").unwrap()), json!({"unit": "ft"}));
        // Erasing one leaves the other, and erasing twice is fine.
        dir.delete("one").unwrap();
        dir.delete("one").unwrap();
        assert!(!folder.0.join("one.json").exists() && folder.0.join("two.json").exists());
        assert_eq!(current(&dir, "one").unwrap(), Map::new());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn plugin_settings_files_that_are_bad_are_errors_and_stay() {
        use native::Dir;
        let folder = Folder::new("bad");
        std::fs::create_dir_all(&folder.0).unwrap();
        let dir = Dir(&folder.0);
        let write = |bytes: &[u8]| std::fs::write(folder.0.join("p.json"), bytes).unwrap();
        // Larger than the limit: refused, and only a byte past it was read.
        write(&vec![b' '; MAX_BYTES * 4]);
        assert!(current(&dir, "p").unwrap_err().contains("damaged or larger"));
        assert!(set(&dir, "p", json!({"a": 1})).is_err());
        assert_eq!(std::fs::metadata(folder.0.join("p.json")).unwrap().len(), (MAX_BYTES * 4) as u64);
        // Not UTF-8, not JSON, not an object.
        for bytes in [&[0xff, 0xfe, 0x00][..], b"{", b"[]"] {
            write(bytes);
            assert!(current(&dir, "p").is_err());
            assert!(set(&dir, "p", json!({"a": 1})).is_err());
            assert_eq!(std::fs::read(folder.0.join("p.json")).unwrap(), bytes);
        }
        // A folder where the file should be is no settings either, and erasing it is an error
        // rather than a silent pass.
        std::fs::remove_file(folder.0.join("p.json")).unwrap();
        std::fs::create_dir(folder.0.join("p.json")).unwrap();
        assert!(current(&dir, "p").unwrap_err().contains("aren't a file"));
        assert!(dir.delete("p").is_err());
        // Erasing a damaged record works.
        std::fs::remove_dir(folder.0.join("p.json")).unwrap();
        write(b"{");
        dir.delete("p").unwrap();
        assert_eq!(current(&dir, "p").unwrap(), Map::new());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn plugin_settings_that_cannot_be_written_keep_the_old_file() {
        use native::Dir;
        let folder = Folder::new("blocked");
        let dir = Dir(&folder.0);
        set(&dir, "p", json!({"keep": 1})).unwrap();
        let before = std::fs::read(folder.0.join("p.json")).unwrap();
        // A read-only folder can't take the temporary file: the old one stays.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&folder.0).unwrap().permissions();
            std::fs::set_permissions(&folder.0, std::fs::Permissions::from_mode(0o500)).unwrap();
            // Running as root ignores the mode: only check when it was honoured.
            let writable = std::fs::File::create(folder.0.join("probe")).is_ok();
            if !writable {
                let e = set(&dir, "p", json!({"keep": 2})).unwrap_err();
                assert!(e.contains("couldn't save the settings of 'p'"), "{e}");
                assert!(!e.contains(&folder.0.display().to_string()), "no path in {e}");
            }
            std::fs::set_permissions(&folder.0, mode).unwrap();
            let _ = std::fs::remove_file(folder.0.join("probe"));
        }
        assert_eq!(std::fs::read(folder.0.join("p.json")).unwrap(), before);
        // A file where the folder should be: nothing can be saved there at all.
        let file = Folder::new("blocked-by-file");
        std::fs::write(&file.0, b"x").unwrap();
        let e = set(&Dir(&file.0), "p", json!({"a": 1})).unwrap_err();
        assert!(e.contains("the settings of 'p'"), "{e}");
    }
}
