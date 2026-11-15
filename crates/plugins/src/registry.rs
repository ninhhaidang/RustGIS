//! The plugin registry: a curated list of plugins (`registry.json`) and a list of pulled ones
//! (`blocklist.json`), published at <https://plugins.rustgis.app>.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::manifest::{Permission, Runtime, valid_id, version_parts};

/// The hosted registry.
pub const DEFAULT_URL: &str = "https://plugins.rustgis.app/registry.json";

/// The most entries read from a registry (or a blocklist): room for tens of thousands of plugins.
pub(crate) const MAX_ENTRIES: usize = 100_000;

/// The largest registry downloaded (a registry entry is about 1 KB, so this is ample).
pub const MAX_REGISTRY_BYTES: usize = 64 * 1024 * 1024;

/// One plugin in the registry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub categories: Vec<String>,
    /// The `.rgplugin` package: an HTTPS URL, or a path relative to the registry. Once read
    /// ([`parse`]), the package for this platform: its own from `packages` if it has one, or
    /// empty when the plugin has packages for other platforms only (see [`Entry::available`]).
    #[serde(default)]
    pub package: String,
    /// Lower-case hex SHA-256 of the package: the download must match it.
    #[serde(default)]
    pub sha256: String,
    /// The package's size in bytes.
    #[serde(default)]
    pub size: u64,
    /// A package per platform (`windows`, `macos`, `linux`), for a plugin that ships a program
    /// for each: this platform's is downloaded in place of `package`, which is then the package
    /// for the platforms not named, if the entry has one.
    #[serde(default, deserialize_with = "platform_packages", skip_serializing_if = "BTreeMap::is_empty")]
    pub packages: BTreeMap<String, PlatformPackage>,
    #[serde(default)]
    pub min_rustgis_version: String,
    #[serde(default)]
    pub permissions: Vec<Permission>,
    /// The plugin's `name` and `description` in other languages, from its manifest's `i18n`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub i18n: BTreeMap<String, BTreeMap<String, String>>,
    /// How the plugin runs, from its manifest: a process plugin is not sandboxed and doesn't
    /// run in the web app. Absent (WebAssembly) in the registry for every other plugin.
    #[serde(default, skip_serializing_if = "is_wasm")]
    pub runtime: Runtime,
    /// For a process plugin with a Python environment, its Python version (`3.12`): installing
    /// it downloads that Python and the plugin's packages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
}

/// One platform's package of a plugin: what an [`Entry`]'s `package`, `sha256` and `size` say of
/// its only one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlatformPackage {
    #[serde(default)]
    pub package: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
}

/// Reads an entry's `packages` one platform at a time: a platform whose package can't be read
/// comes back empty, for [`parse_for`] to drop as it drops any package that doesn't check out,
/// in place of failing the whole entry and hiding the plugin from every platform. `null` is no
/// packages; anything else that isn't an object fails the entry, as any field of the wrong type
/// does.
fn platform_packages<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, PlatformPackage>, D::Error> {
    let raw = Option::<BTreeMap<String, Value>>::deserialize(d)?.unwrap_or_default();
    Ok(raw.into_iter().map(|(key, v)| (key, serde_json::from_value(v).unwrap_or_default())).collect())
}

/// The platforms a registry entry can name a package for, with the names shown for them. The
/// keys are those of a manifest's `run`.
pub const PLATFORMS: [(&str, &str); 3] = [("windows", "Windows"), ("macos", "macOS"), ("linux", "Linux")];

/// This platform's key in an entry's `packages`, as a manifest's `run` picks its program; none
/// in the browser, which has no platform of its own.
pub fn this_platform() -> Option<&'static str> {
    if cfg!(target_arch = "wasm32") {
        None
    } else if cfg!(target_os = "windows") {
        Some("windows")
    } else if cfg!(target_os = "macos") {
        Some("macos")
    } else {
        Some("linux")
    }
}

/// The name shown for the platform `key` (`macOS` for `macos`), or the key.
pub fn platform_name(key: &str) -> &str {
    PLATFORMS.iter().find(|(k, _)| *k == key).map_or(key, |(_, name)| name)
}

/// The name shown for this platform; `the web app` in the browser.
pub fn this_platform_name() -> &'static str {
    this_platform().map_or("the web app", platform_name)
}

fn is_wasm(r: &Runtime) -> bool {
    *r == Runtime::Wasm
}

/// Whether `v` looks like a Python version (`3.12`, `3.12.4`), as a manifest's must.
fn python_version(v: &str) -> bool {
    v.len() <= 16
        && (v == "3" || v.starts_with("3."))
        && v.split('.').all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()))
}

impl Entry {
    /// `s` (the name or description) in language `lang`, or `s`.
    pub fn translate<'a>(&'a self, lang: &str, s: &'a str) -> &'a str {
        crate::manifest::translate(&self.i18n, lang, s)
    }

    /// Whether the plugin can be installed here: it has a package for this platform, and this
    /// RustGIS is as new as it needs (see [`Entry::needs_rustgis`]). Otherwise it is listed, not
    /// downloaded, installed or offered as an update.
    pub fn available(&self) -> bool {
        !self.package.is_empty() && self.needs_rustgis().is_none()
    }

    /// The RustGIS this release needs (its `minRustgisVersion`), when that is newer than this one.
    pub fn needs_rustgis(&self) -> Option<&str> {
        (!crate::manifest::rustgis_meets(&self.min_rustgis_version)).then_some(self.min_rustgis_version.as_str())
    }

    /// The platforms the plugin has a package for, by the names shown for them (`Windows`,
    /// `macOS`, `Linux`): for saying where a plugin that isn't available here can be installed.
    pub fn platforms(&self) -> Vec<&'static str> {
        PLATFORMS.iter().filter(|(key, _)| self.packages.contains_key(*key)).map(|(_, name)| *name).collect()
    }

    /// Why the plugin can't be installed here: it needs a newer RustGIS, or its packages are for
    /// other platforms.
    pub fn unavailable(&self) -> String {
        if let Some(min) = self.needs_rustgis() {
            return format!("{} needs RustGIS {min} or newer (this is {})", self.name, crate::manifest::RUSTGIS_VERSION);
        }
        let (here, has) = (this_platform_name(), self.platforms());
        if has.is_empty() {
            return format!("{} has no package for {here}", self.name);
        }
        format!("{} has no package for {here} (it is available for {})", self.name, has.join(", "))
    }
}

/// A pulled plugin: every version (no `sha256`), or the one package with that hash.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Blocked {
    pub id: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub reason: String,
}

/// Whether `s` is a lower-case hex SHA-256.
pub fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `rel` resolved against the URL `base` (a file in a folder): absolute URLs are kept, a path
/// replaces the last segment of `base`.
pub fn resolve(base: &str, rel: &str) -> String {
    if rel.contains("://") {
        return rel.to_string();
    }
    let Some(host) = base.find("://").map(|i| i + 3) else { return rel.to_string() };
    // The origin (`https://host`) and the folder (`https://host/a/`).
    let origin = base.get(host..).and_then(|r| r.find('/')).and_then(|i| base.get(..host + i)).unwrap_or(base);
    if let Some(abs) = rel.strip_prefix('/') {
        return format!("{origin}/{abs}");
    }
    let dir = match base.rfind('/') {
        Some(i) if i >= host => base.get(..=i).unwrap_or(base).to_string(),
        _ => format!("{base}/"),
    };
    format!("{dir}{}", rel.trim_start_matches("./"))
}

/// Whether `url` is the hosted registry.
pub fn is_default(url: &str) -> bool {
    let u = url.trim();
    u.is_empty() || u == DEFAULT_URL
}

/// Whether a package URL may be downloaded: HTTPS, or plain HTTP to this computer (for testing
/// a local registry).
pub fn allowed_url(url: &str) -> bool {
    url.starts_with("https://") || is_local(url)
}

/// Whether `url` is plain HTTP to this computer (`http://localhost`, `http://127.0.0.1`, with a
/// port or path), and not a host that merely starts like one (`localhost.example.com`) or hides
/// another host after user info (`localhost:80@example.com`).
fn is_local(url: &str) -> bool {
    ["http://localhost", "http://127.0.0.1"].iter().any(|p| {
        url.strip_prefix(p).is_some_and(|r| {
            let authority_rest = r.split(['/', '?', '#']).next().unwrap_or_default();
            (r.is_empty() || r.starts_with([':', '/', '?', '#'])) && !authority_rest.contains('@')
        })
    })
}

/// Reads a registry (an object with a `plugins` array, or a bare array) fetched from `url`,
/// resolving package paths. Entries that don't check out are skipped, not fatal: one bad entry
/// mustn't hide the rest. An entry with a package per platform comes back with this platform's
/// as its `package`, `sha256` and `size`; one with packages for other platforms only is kept,
/// with no `package`, so the list can say where it installs.
pub fn parse(text: &str, url: &str) -> Result<Vec<Entry>, String> {
    parse_for(text, url, this_platform())
}

/// [`parse`], with a notice when packages by platform were skipped: one that doesn't check out
/// (no address, a bad SHA-256, an address that isn't allowed) costs its platform the plugin, so
/// it is said, with how many and which. None for a registry read whole.
pub fn parse_with_notice(text: &str, url: &str) -> Result<(Vec<Entry>, Option<String>), String> {
    parse_noting(text, url, this_platform())
}

/// [`parse`], for `platform` (a key of `packages`; none in the browser).
pub fn parse_for(text: &str, url: &str, platform: Option<&str>) -> Result<Vec<Entry>, String> {
    parse_noting(text, url, platform).map(|(entries, _)| entries)
}

/// [`parse_with_notice`], for `platform`.
pub fn parse_noting(text: &str, url: &str, platform: Option<&str>) -> Result<(Vec<Entry>, Option<String>), String> {
    let root: Value = serde_json::from_str(text).map_err(|e| format!("the plugin registry isn't valid JSON: {e}"))?;
    // Taken by value, so each entry is converted without a copy (a large registry would
    // otherwise briefly need several times its size).
    let list = match root {
        Value::Array(a) => a,
        Value::Object(mut o) => match o.remove("plugins") {
            Some(Value::Array(a)) => a,
            _ => return Err("the plugin registry has no 'plugins' list".into()),
        },
        _ => return Err("the plugin registry isn't a list of plugins".into()),
    };
    let mut out: Vec<Entry> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // The packages by platform that were skipped: how many, and the first few by name.
    let (mut skipped, mut which) = (0usize, Vec::new());
    // The plugins that were skipped whole (unreadable, or with no package that checks out): how
    // many, how many of them have an id that can be shown, and the first few of those.
    let (mut dropped, mut named, mut dropped_ids) = (0usize, 0usize, Vec::new());
    let mut drop_entry = |id: &str| {
        dropped += 1;
        if valid_id(id, false) {
            named += 1;
            if dropped_ids.len() < 5 {
                dropped_ids.push(id.to_string());
            }
        }
    };
    for v in list.into_iter().take(MAX_ENTRIES) {
        let Ok(mut e) = serde_json::from_value::<Entry>(v) else {
            drop_entry("");
            continue;
        };
        // A `minRustgisVersion` that isn't a version is a package no RustGIS installs (its manifest
        // is refused), so it isn't listed to be downloaded and refused.
        let min_unreadable = !e.min_rustgis_version.is_empty() && version_parts(&e.min_rustgis_version).is_none();
        if !valid_id(&e.id, false) || version_parts(&e.version).is_none() || min_unreadable || e.name.trim().is_empty() {
            drop_entry(&e.id);
            continue;
        }
        // The entry named a package for this platform: its own package then isn't this
        // platform's, even when the named one doesn't check out.
        let named = platform.is_some_and(|key| e.packages.contains_key(key));
        // A platform's package that doesn't check out is dropped as an entry's only one is, and
        // so is a platform this version doesn't know: nothing here would download it. (No
        // address is not the registry's folder, which an empty path would resolve to.)
        let id = &e.id;
        e.packages.retain(|key, p| {
            // A platform this version doesn't know isn't a loss: nothing was to be read.
            if !PLATFORMS.iter().any(|(k, _)| k == key) {
                return false;
            }
            let has_address = !p.package.trim().is_empty();
            p.package = resolve(url, &p.package);
            let good = has_address && is_sha256(&p.sha256) && allowed_url(&p.package);
            if !good {
                skipped += 1;
                if which.len() < 5 {
                    which.push(format!("{id} ({})", platform_name(key)));
                }
            }
            good
        });
        let whole = !e.package.trim().is_empty() && is_sha256(&e.sha256);
        e.package = resolve(url, &e.package);
        let whole = whole && allowed_url(&e.package);
        match platform.and_then(|key| e.packages.get(key)) {
            Some(own) => (e.package, e.sha256, e.size) = (own.package.clone(), own.sha256.clone(), own.size),
            None if whole && !named => {}
            // Packages for other platforms only: listed, with nothing to download here.
            None if !e.packages.is_empty() => (e.package, e.sha256, e.size) = (String::new(), String::new(), 0),
            None => {
                drop_entry(&e.id);
                continue;
            }
        }
        // A second entry for an id is skipped (the first is listed), and said.
        if !seen.insert(e.id.clone()) {
            drop_entry(&e.id);
            continue;
        }
        if !e.homepage.starts_with("https://") && !e.homepage.starts_with("http://") {
            e.homepage.clear();
        }
        // Only a process plugin has a Python environment, and only a version is shown.
        if e.runtime != Runtime::Process || !e.python.as_deref().is_some_and(python_version) {
            e.python = None;
        }
        // Only the name and description are translated here, in at most 64 languages, each at
        // most 2,000 bytes: the registry comes from the network.
        let (name, description) = (e.name.clone(), e.description.clone());
        // Filtered before the cap, so junk languages can't take the place of real ones.
        e.i18n = std::mem::take(&mut e.i18n)
            .into_iter()
            .filter(|(lang, _)| lang.len() <= 16)
            .map(|(lang, mut t)| {
                t.retain(|k, v| (*k == name || *k == description) && v.len() <= 2_000);
                (lang, t)
            })
            .filter(|(_, t)| !t.is_empty())
            .take(64)
            .collect();
        out.push(e);
    }
    out.sort_by_key(|e| e.name.to_lowercase());
    let mut notices = Vec::new();
    if dropped > 0 {
        let more = if named > dropped_ids.len() { ", …" } else { "" };
        notices.push(format!(
            "{dropped} plugin(s) in the plugin registry were skipped (an unreadable entry, id, version or minRustgisVersion, no package that checks out, or an id listed twice): {}{more}",
            dropped_ids.join(", ")
        ));
    }
    if skipped > 0 {
        let more = if skipped > which.len() { ", …" } else { "" };
        notices.push(format!(
            "{skipped} package(s) for a platform in the plugin registry were skipped (no address, a bad SHA-256 or an address that isn't allowed): {}{more}",
            which.join(", ")
        ));
    }
    Ok((out, (!notices.is_empty()).then(|| notices.join("; "))))
}

/// The blocklist published beside a registry.
pub fn blocklist_url(registry: &str) -> String {
    resolve(registry, "blocklist.json")
}

/// Reads a blocklist (an object with a `blocked` array, or a bare array). Any other shape, an
/// entry whose `id` isn't a plugin id or whose `sha256`, when given, isn't a SHA-256 hash (either
/// would match nothing; no `sha256`, or `null`, withdraws every version), or more than 100,000
/// entries, is an error rather than a shorter list, so a malformed refresh leaves the last good
/// list in place: only `[]` or `{"blocked": []}` clears it.
pub fn parse_blocklist(text: &str) -> Result<Vec<Blocked>, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| format!("the blocklist isn't valid JSON: {e}"))?;
    let list = match root {
        Value::Array(a) => a,
        Value::Object(mut o) => match o.remove("blocked") {
            Some(Value::Array(a)) => a,
            _ => return Err("the blocklist has no 'blocked' list".into()),
        },
        _ => return Err("the blocklist isn't a list or an object with a 'blocked' list".into()),
    };
    if list.len() > MAX_ENTRIES {
        return Err(format!("the blocklist has {} entries; at most {MAX_ENTRIES} can be read", list.len()));
    }
    list.into_iter()
        .enumerate()
        .map(|(i, v)| {
            // The reason is cut short: it can quote the entry, which comes from the network.
            let b = serde_json::from_value::<Blocked>(v)
                .map_err(|e| format!("blocklist entry {}: {}", i + 1, e.to_string().chars().take(200).collect::<String>()))?;
            if !valid_id(&b.id, false) {
                return Err(format!("blocklist entry {}: id isn't a plugin id", i + 1));
            }
            // Either case: `blocked` compares hashes ignoring it.
            match &b.sha256 {
                Some(h) if !is_sha256(&h.to_ascii_lowercase()) => Err(format!("blocklist entry {}: sha256 isn't a SHA-256 hash", i + 1)),
                _ => Ok(b),
            }
        })
        .collect()
}

/// Why a package is blocked, if it is.
pub fn blocked<'a>(list: &'a [Blocked], id: &str, sha256: &str) -> Option<&'a Blocked> {
    list.iter().find(|b| b.id == id && b.sha256.as_deref().is_none_or(|h| h.eq_ignore_ascii_case(sha256)))
}

/// Downloads `url` (at most `limit` bytes). Native only: the browser fetches with its own API.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch(url: &str, limit: usize) -> Result<Vec<u8>, String> {
    fetch_with_progress(url, limit, std::time::Duration::from_secs(120), &mut |_, _| true)
}

/// Downloads `url` as [`fetch`] does, within `timeout` in all, telling `progress` the bytes read
/// so far and the size the server gave, if it did, after each piece; the download stops, as an
/// error, when `progress` answers `false` (the user cancelled). For large files: an installer.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_with_progress(
    url: &str,
    limit: usize,
    timeout: std::time::Duration,
    progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
) -> Result<Vec<u8>, String> {
    use std::io::Read;
    if !allowed_url(url) {
        return Err(rustgis_data::diagnostics::message_with_address(url, "only HTTPS addresses are allowed"));
    }
    // Redirects stay on HTTPS (a local test registry may use plain HTTP, and only locally). A
    // server that stops sending ends the download after a minute, not at `timeout`: until a read
    // returns, a cancel isn't seen.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .timeout_per_read(Some(std::time::Duration::from_secs(60)))
        .https_only(!is_local(url))
        .max_redirects(if is_local(url) { 0 } else { 5 })
        .tls_config(rustgis_data::http::tls_config())
        .build()
        .into();
    let mut resp = agent
        .get(url)
        .header("User-Agent", concat!("RustGIS/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|e| rustgis_data::diagnostics::message_with_address(url, &e.to_string()))?;
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    let mut reader = resp.body_mut().with_config().limit(limit as u64 + 1).reader();
    let mut buf = Vec::new();
    let mut piece = vec![0u8; 64 * 1024];
    loop {
        let n = match reader.read(&mut piece) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(rustgis_data::diagnostics::message_with_address(url, &e.to_string())),
        };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(piece.get(..n).unwrap_or_default());
        if buf.len() > limit {
            return Err(rustgis_data::diagnostics::message_with_address(url, &format!("is larger than {} MB", limit / (1024 * 1024))));
        }
        if !progress(buf.len() as u64, total) {
            return Err(rustgis_data::diagnostics::message_with_address(url, "cancelled"));
        }
    }
    Ok(buf)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod fetch_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    #[test]
    fn rejected_secret_addresses_keep_the_policy_reason() {
        let error = fetch("ftp://plugins.test/a?token=secret value", 1024).unwrap_err();
        let error = rustgis_data::diagnostics::redact(&rustgis_data::diagnostics::redact(&error));
        assert!(error.contains("only HTTPS addresses are allowed"), "{error}");
        assert!(!error.contains("secret") && !error.contains("value"), "{error}");
    }

    /// Serves `body` once on this computer, as an HTTP answer that gives its length.
    fn serve(body: Vec<u8>) -> std::io::Result<String> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut request = [0u8; 4096];
                let _ = s.read(&mut request);
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(&body);
            }
        });
        Ok(format!("http://127.0.0.1:{port}/installer.exe"))
    }

    #[test]
    fn a_download_reports_its_progress() {
        let url = serve(vec![7u8; 300_000]).unwrap();
        let mut seen = Vec::new();
        let got = fetch_with_progress(&url, 1 << 20, Duration::from_secs(30), &mut |done, total| {
            seen.push((done, total));
            true
        })
        .unwrap();
        assert_eq!(got.len(), 300_000);
        // Piece by piece, always more, ending with all of it and the size the server gave.
        assert!(seen.len() > 1 && seen.windows(2).all(|w| w[0].0 < w[1].0), "{seen:?}");
        assert_eq!(seen.last(), Some(&(300_000, Some(300_000))));
    }

    #[test]
    fn a_download_stops_when_cancelled() {
        let url = serve(vec![7u8; 300_000]).unwrap();
        let err = fetch_with_progress(&url, 1 << 20, Duration::from_secs(30), &mut |_, _| false).unwrap_err();
        assert!(err.ends_with("cancelled"), "{err}");
    }

    #[test]
    fn a_download_over_its_limit_is_refused() {
        let url = serve(vec![7u8; 300_000]).unwrap();
        let err = fetch_with_progress(&url, 100_000, Duration::from_secs(30), &mut |_, _| true).unwrap_err();
        assert!(err.contains("is larger than"), "{err}");
    }
}
