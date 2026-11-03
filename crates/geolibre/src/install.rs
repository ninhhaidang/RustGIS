//! One-click install of the `geolibre` runner: the latest geolibre-rust release's archive for
//! this platform, downloaded over HTTPS, checked against its published SHA-256 and unpacked into
//! RustGIS's data folder (where [`crate::runner`] looks for it).
//!
//! The checksum comes from the same release, so it guards against a corrupted or truncated
//! download, not against a compromised release: trust rests on HTTPS and on the opengeos
//! GitHub releases, as when downloading the runner by hand.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

/// The GitHub API address of the latest release.
const LATEST: &str = "https://api.github.com/repos/opengeos/geolibre-rust/releases/latest";

/// Archives larger than this are refused (the runner is about 19 MB).
const MAX_BYTES: u64 = 300 * 1024 * 1024;

/// The release target this build runs on, as the archives name it.
pub fn target() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => return None,
    })
}

/// Where an installed runner lives: a `geolibre` folder in the app's data folder.
pub fn install_dir() -> Option<PathBuf> {
    rustgis_data::dirs::app_data_dir().map(|b| b.join("geolibre"))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(600)))
        .tls_config(rustgis_data::http::tls_config())
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    agent.get(url).header("User-Agent", concat!("RustGIS/", env!("CARGO_PKG_VERSION"))).call().map_err(|e| format!("{url}: {e}"))
}

/// The latest release's tag and the archive and checksum URLs for `target`.
fn find_asset(agent: &ureq::Agent, target: &str) -> Result<(String, String, String, String), String> {
    let text = get(agent, LATEST)?.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let release: Value = serde_json::from_str(&text).map_err(|e| format!("unexpected release information: {e}"))?;
    let tag = release.get("tag_name").and_then(Value::as_str).unwrap_or("latest").to_string();
    let assets = release.get("assets").and_then(Value::as_array).cloned().unwrap_or_default();
    let url_of = |name: &str| {
        assets
            .iter()
            .find(|a| a.get("name").and_then(Value::as_str) == Some(name))
            .and_then(|a| a.get("browser_download_url").and_then(Value::as_str))
            .map(str::to_string)
    };
    let ext = if target.contains("windows") { "zip" } else { "tar.gz" };
    let name = format!("geolibre-{tag}-{target}.{ext}");
    let archive = url_of(&name).ok_or_else(|| format!("release {tag} has no runner for {target}"))?;
    let sums = url_of(&format!("{name}.sha256")).ok_or_else(|| format!("release {tag} has no checksum for {name}"))?;
    Ok((tag, name, archive, sums))
}

/// Downloads the runner, checks it and installs it; returns the installed program. `progress`
/// gets the bytes received and the expected total.
pub fn install(progress: &dyn Fn(u64, Option<u64>)) -> Result<PathBuf, String> {
    let target = target().ok_or("there is no GeoLibre runner for this platform")?;
    let dir = install_dir().ok_or("can't find a folder to install the GeoLibre runner in")?;
    let agent = agent();
    let (tag, name, archive_url, sums_url) = find_asset(&agent, target)?;

    let sums = get(&agent, &sums_url)?.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let expected = sums.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{name}: unreadable checksum"));
    }

    let mut resp = get(&agent, &archive_url)?;
    let total = resp.body().content_length();
    if total.is_some_and(|t| t > MAX_BYTES) {
        return Err(format!("{name} is unexpectedly large"));
    }
    let mut reader = resp.body_mut().as_reader();
    let mut bytes = Vec::with_capacity(total.unwrap_or(0).min(MAX_BYTES) as usize);
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("{name}: {e}"))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(buf.get(..n).unwrap_or_default());
        if bytes.len() as u64 > MAX_BYTES {
            return Err(format!("{name} is unexpectedly large"));
        }
        progress(bytes.len() as u64, total);
    }
    let actual: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if actual != expected {
        return Err(format!("{name}: the download doesn't match its published checksum"));
    }

    // Unpack into a fresh folder beside the install, then move the program into place.
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let staging = dir.join(format!(".download-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let unpacked = unpack(&bytes, target, &staging);
    let exe_name = if target.contains("windows") { "geolibre.exe" } else { "geolibre" };
    let found = unpacked.and_then(|_| find_file(&staging, exe_name, 3).ok_or_else(|| format!("{name} has no {exe_name}")));
    let result = found.and_then(|exe| {
        let dest = dir.join(exe_name);
        // Replace an older install without ever losing it: set it aside, move the new one in,
        // and put the old one back if that fails.
        let old = dir.join(format!("{exe_name}.old"));
        let _ = std::fs::remove_file(&old);
        let had_old = dest.exists() && std::fs::rename(&dest, &old).is_ok();
        if let Err(e) = std::fs::rename(&exe, &dest).map_err(|e| format!("{}: {e}", dest.display())).and_then(|_| make_executable(&dest)) {
            if had_old {
                let _ = std::fs::rename(&old, &dest);
            }
            return Err(e);
        }
        // A copy that's running (Windows) can't be deleted now; the next install clears it.
        let _ = std::fs::remove_file(&old);
        Ok(dest)
    });
    let _ = std::fs::remove_dir_all(&staging);
    let dest = result?;
    crate::runner::version_of(&dest).ok_or_else(|| format!("the installed {tag} runner doesn't start"))?;
    Ok(dest)
}

/// Whether `exe` is the runner [`install`] put in place (rather than one found elsewhere).
pub fn is_installed(exe: &Path) -> bool {
    install_dir().is_some_and(|d| exe == d.join(crate::runner::exe_name()))
}

/// Removes what [`install`] put in place: the program (and an older copy set aside while
/// replacing it), then the folder if nothing else is in it. Other files there, and a runner
/// found elsewhere (chosen with Locate…, or on the `PATH`), are the user's and are left alone.
pub fn uninstall() -> Result<(), String> {
    let dir = install_dir().ok_or("can't find the GeoLibre install folder")?;
    let exe = dir.join(crate::runner::exe_name());
    if !exe.is_file() {
        return Err("the GeoLibre runner isn't installed".into());
    }
    std::fs::remove_file(&exe).map_err(|e| format!("{}: {e}", exe.display()))?;
    let _ = std::fs::remove_file(dir.join(format!("{}.old", crate::runner::exe_name())));
    // Fails, keeping the folder, when something else is in it.
    let _ = std::fs::remove_dir(&dir);
    Ok(())
}

fn unpack(bytes: &[u8], target: &str, into: &Path) -> Result<(), String> {
    if target.contains("windows") {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
        // `extract` keeps entries inside the folder (it rejects paths that escape it).
        zip.extract(into).map_err(|e| e.to_string())
    } else {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        // `unpack` keeps entries inside the folder too.
        tar.unpack(into).map_err(|e| e.to_string())
    }
}

/// A file named `name` within `dir`, at most `depth` folders down.
fn find_file(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let entries: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    if let Some(f) = entries.iter().find(|p| p.is_file() && p.file_name().is_some_and(|n| n == name)) {
        return Some(f.clone());
    }
    if depth == 0 {
        return None;
    }
    entries.iter().filter(|p| p.is_dir()).find_map(|d| find_file(d, name, depth - 1))
}

#[cfg(unix)]
fn make_executable(p: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("{}: {e}", p.display()))
}

#[cfg(not(unix))]
fn make_executable(_: &Path) -> Result<(), String> {
    Ok(())
}
