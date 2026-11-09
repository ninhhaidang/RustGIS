//! Installed plugins on disk (desktop and command line): packages in RustGIS's data folder,
//! loaded at startup, plus plugin folders added for development.
//!
//! ```text
//! <data>/plugins/<id>.rgplugin    installed packages
//! <data>/plugins/unpacked/<id>/   an installed process plugin's files, where its program runs
//! <data>/plugins/state.json       {"disabled": [ids], "folders": [plugin folders]}
//! <data>/plugins/blocklist.json   the registry's blocklist, as last fetched
//! ```

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::{Package, Plugin, Runtime, registry};

/// The most a process plugin's files may take unpacked (a guard against zip bombs).
const MAX_UNPACKED: u64 = 1024 * 1024 * 1024;
/// The file in an unpacked folder naming the package it came from (its SHA-256).
const UNPACKED_FROM: &str = ".rustgis-package";

/// The environment variable that names the plugins folder, ahead of the usual place.
pub const ENV: &str = "RUSTGIS_PLUGINS_DIR";

static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Uses `dir` as the plugins folder (tests, portable installs); `None`: the usual place.
pub fn set_dir(dir: Option<PathBuf>) {
    if let Ok(mut d) = DIR.lock() {
        *d = dir;
    }
}

/// The plugins folder: `plugins` in the app's data folder.
pub fn dir() -> Option<PathBuf> {
    if let Ok(d) = DIR.lock()
        && let Some(d) = d.clone()
    {
        return Some(d);
    }
    if let Some(d) = std::env::var_os(ENV).filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    rustgis_data::dirs::app_data_dir().map(|b| b.join("plugins"))
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    disabled: Vec<String>,
    #[serde(default)]
    folders: Vec<String>,
}

fn state_path() -> Option<PathBuf> {
    dir().map(|d| d.join("state.json"))
}

fn read_state() -> State {
    state_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn write_state(st: &State) -> Result<(), String> {
    let path = state_path().ok_or("there is no folder for plugins")?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let text = serde_json::to_string_pretty(st).map_err(|e| e.to_string())?;
    rustgis_data::io::replace_atomic(&path, text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

/// Loads every installed package and plugin folder (and the cached blocklist); returns what
/// couldn't be loaded.
pub fn load_installed() -> Vec<String> {
    let mut errors = Vec::new();
    let Some(dir) = dir() else { return errors };
    // A cache that can't be read (or one an older version saved that this one refuses) is
    // reported, not trusted; there being none is fine.
    let cached = dir.join("blocklist.json");
    let list = match std::fs::read_to_string(&cached) {
        Ok(text) => registry::parse_blocklist(&text).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    };
    match list {
        Ok(Some(list)) => {
            crate::set_blocklist(list);
        }
        Ok(None) => {}
        Err(e) => errors.push(format!(
            "{}: {e}; it was ignored, so withdrawn plugins may not be blocked until the blocklist is fetched again",
            cached.display()
        )),
    }
    let st = read_state();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("rgplugin"))).collect())
        .unwrap_or_default();
    paths.sort();
    // Folders after packages: a development copy wins over an installed one.
    paths.extend(st.folders.iter().map(PathBuf::from));
    for path in paths {
        match read_package(&path).and_then(|pkg| crate::load(pkg, &path.display().to_string())) {
            Ok(p) => {
                if st.disabled.contains(&p.manifest.id) {
                    let _ = crate::set_enabled(&p.manifest.id, false);
                }
            }
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }
    errors
}

/// A package file or a plugin folder, a process plugin's package unpacked to run.
fn read_package(path: &Path) -> Result<Package, String> {
    if path.is_dir() {
        return Package::from_dir(path);
    }
    let len = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
    if len > crate::package::MAX_PACKAGE as u64 {
        return Err(format!("{} is larger than {} MB", path.display(), crate::package::MAX_PACKAGE / (1024 * 1024)));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut pkg = Package::from_zip(&bytes)?;
    unpack(&bytes, &mut pkg)?;
    Ok(pkg)
}

/// Where an installed process plugin's files are unpacked.
fn unpacked_dir(id: &str) -> Option<PathBuf> {
    dir().map(|d| d.join("unpacked").join(id))
}

/// Unpacks a process plugin's package (`bytes`) to run from, unless that copy is already there,
/// and points `pkg` at it. Other plugins need nothing unpacked.
pub fn unpack(bytes: &[u8], pkg: &mut Package) -> Result<(), String> {
    if pkg.manifest.runtime != Runtime::Process {
        return Ok(());
    }
    let target = unpacked_dir(&pkg.manifest.id).ok_or("there is no folder for plugins")?;
    unpack_to(bytes, &pkg.sha256, &target, &pkg.manifest.id)?;
    pkg.dir = Some(target);
    Ok(())
}

/// Unpacks a package to `target`, the folder a process plugin runs from, unless that copy is
/// already there.
fn unpack_to(bytes: &[u8], sha256: &str, target: &Path, id: &str) -> Result<(), String> {
    match stage(bytes, sha256, target)? {
        Some(staged) => {
            // A session plugin's program runs from these files (on Windows they can't be
            // replaced while it does): it stops first.
            let was_running = crate::session::is_running(id);
            crate::session::stop(id);
            swap_in(&staged, target).map_err(|e| if was_running { format!("{e} (its running program was stopped first)") } else { e })
        }
        None => Ok(()),
    }
}

/// Unpacks a package beside `target`, ready to [`swap_in`], unless `target` already holds it
/// (`None`). Nothing in `target` changes, so a package that can't be unpacked leaves it as it is.
fn stage(bytes: &[u8], sha256: &str, target: &Path) -> Result<Option<PathBuf>, String> {
    if std::fs::read_to_string(target.join(UNPACKED_FROM)).is_ok_and(|s| s.trim() == sha256) {
        return Ok(None);
    }
    let parent = target.parent().ok_or("there is no folder for plugins")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = parent.join(format!(".{name}-{nanos}"));
    let res = extract(bytes, &tmp)
        .and_then(|_| rustgis_data::io::replace_atomic(&tmp.join(UNPACKED_FROM), sha256.as_bytes()).map_err(|e| e.to_string()));
    if let Err(e) = res {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    Ok(Some(tmp))
}

/// Moves a staged folder to `target`. The old copy is moved aside, not deleted, until the new
/// one is in place, and put back on a failure: there is always a folder to run from.
fn swap_in(staged: &Path, target: &Path) -> Result<(), String> {
    let mut old = staged.as_os_str().to_os_string();
    old.push("-old");
    let old = PathBuf::from(old);
    let had_old = target.exists();
    if had_old && let Err(e) = std::fs::rename(target, &old) {
        let _ = std::fs::remove_dir_all(staged);
        return Err(format!("{}: {e}", target.display()));
    }
    if let Err(e) = std::fs::rename(staged, target) {
        if had_old {
            let _ = std::fs::rename(&old, target);
        }
        let _ = std::fs::remove_dir_all(staged);
        return Err(format!("{}: {e}", target.display()));
    }
    if had_old {
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(())
}

/// Writes a package's files (those under its `plugin.json`'s folder) into `to`: only paths
/// inside it, at most [`MAX_UNPACKED`] in all, keeping Unix file modes (executables).
fn extract(bytes: &[u8], to: &Path) -> Result<(), String> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("not a plugin package: {e}"))?;
    let root = crate::package::root_of(&zip)?;
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
        // `enclosed_name` refuses absolute paths and `..`: nothing lands outside `to`.
        let Some(name) = f.enclosed_name() else { return Err(format!("the package has an unsafe path: {}", f.name().unwrap_or_default())) };
        let Ok(rel) = name.strip_prefix(&root) else { continue };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let path = to.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            continue;
        }
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        let mut out = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        // Counted as written, not as the archive claims.
        let n = std::io::copy(&mut (&mut f).take(MAX_UNPACKED - total + 1), &mut out).map_err(|e| format!("{}: {e}", path.display()))?;
        total += n;
        if total > MAX_UNPACKED {
            return Err(format!("the package unpacks to more than {} MB", MAX_UNPACKED / (1024 * 1024)));
        }
        #[cfg(unix)]
        if let Some(mode) = f.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            // The owner can always read and write; others' bits as packed, never set-id.
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode((mode & 0o777) | 0o600));
        }
    }
    Ok(())
}

/// Installs a package's bytes: checks it, copies it into the plugins folder (replacing an
/// older copy) and loads it.
pub fn install_bytes(bytes: &[u8]) -> Result<Arc<Plugin>, String> {
    let mut pkg = Package::from_zip(bytes)?;
    // A process plugin is checked (blocklist, platform) before its files replace an installed
    // copy's: a package that won't load leaves the old one working.
    let unpack_into = match pkg.manifest.runtime {
        Runtime::Process => Some(unpacked_dir(&pkg.manifest.id).ok_or("there is no folder for plugins")?),
        Runtime::Wasm | Runtime::Skills => None,
    };
    pkg.dir = unpack_into.clone();
    let sha256 = pkg.sha256.clone();
    let dir = dir().ok_or("there is no folder for plugins")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{}.rgplugin", pkg.manifest.id));
    // Check and compile it, keep it, then load it: a package that doesn't load isn't kept, and
    // one that can't be kept isn't loaded.
    let plugin = crate::prepare(pkg, &path.display().to_string())?;
    // Unpacked (staged) first: a package that can't be is refused before anything changes.
    // Then the package is kept, and only then do its files replace the old ones.
    let staged = match &unpack_into {
        Some(target) => stage(bytes, &sha256, target)?.map(|s| (s, target)),
        None => None,
    };
    if let Err(e) = rustgis_data::io::replace_atomic(&path, bytes) {
        if let Some((s, _)) = &staged {
            let _ = std::fs::remove_dir_all(s);
        }
        return Err(format!("{}: {e}", path.display()));
    }
    if let Some((s, target)) = &staged {
        // A session plugin's program runs from these files (on Windows they can't be replaced
        // while it does): it stops first.
        let was_running = crate::session::is_running(&plugin.manifest.id);
        crate::session::stop(&plugin.manifest.id);
        swap_in(s, target).map_err(|e| if was_running { format!("{e} (its running program was stopped first)") } else { e })?;
    }
    Ok(crate::register(plugin))
}

/// Installs from a path: a `.rgplugin` file is copied in; a plugin folder (for development) is
/// remembered and loaded from where it is.
pub fn install_path(path: &Path) -> Result<Arc<Plugin>, String> {
    if !path.is_dir() {
        // Size-checked before reading: a huge file isn't read into memory.
        let len = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
        if len > crate::package::MAX_PACKAGE as u64 {
            return Err(format!("{} is larger than {} MB", path.display(), crate::package::MAX_PACKAGE / (1024 * 1024)));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        return install_bytes(&bytes);
    }
    let pkg = Package::from_dir(path)?;
    let plugin = crate::load(pkg, &path.display().to_string())?;
    let mut st = read_state();
    let p = path.display().to_string();
    if !st.folders.contains(&p) {
        st.folders.push(p);
        write_state(&st)?;
    }
    Ok(plugin)
}

/// Uninstalls a plugin: unloads it, deletes its package, or forgets its folder.
pub fn uninstall(id: &str) -> Result<(), String> {
    let plugin = crate::get(id).ok_or_else(|| format!("no plugin '{id}'"))?;
    let mut st = read_state();
    st.folders.retain(|f| *f != plugin.source);
    st.disabled.retain(|d| d != id);
    // Forget it first, so a failure leaves nothing half-done; then delete the package only when
    // the loaded plugin is that package (uninstalling a development folder that shadows an
    // installed copy leaves the copy).
    write_state(&st)?;
    crate::unload(id);
    if let Some(d) = dir() {
        let path = d.join(format!("{id}.rgplugin"));
        if path.is_file() && path.display().to_string() == plugin.source {
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if let Some(unpacked) = unpacked_dir(id).filter(|u| u.is_dir()) {
                std::fs::remove_dir_all(&unpacked).map_err(|e| format!("{}: {e}", unpacked.display()))?;
            }
            crate::pyenv::remove(id)?;
        }
    }
    Ok(())
}

/// Enables or disables a plugin, and remembers it.
pub fn set_enabled(id: &str, enabled: bool) -> Result<(), String> {
    crate::set_enabled(id, enabled)?;
    let mut st = read_state();
    st.disabled.retain(|d| d != id);
    if !enabled {
        st.disabled.push(id.to_string());
    }
    write_state(&st)
}

/// Applies the blocklist fetched beside the registry at `registry_url`, and keeps the hosted
/// registry's for offline starts (see [`crate::apply_blocklist`]).
pub fn save_blocklist(registry_url: &str, text: &str) -> Result<Vec<String>, String> {
    let list = registry::parse_blocklist(text)?;
    if registry::is_default(registry_url)
        && let Some(d) = dir()
    {
        let _ = std::fs::create_dir_all(&d);
        let _ = rustgis_data::io::replace_atomic(&d.join("blocklist.json"), text.as_bytes());
    }
    Ok(crate::apply_blocklist(registry_url, list))
}
