//! The plugin registry: a curated list of plugins (`registry.json`) and a list of pulled ones
//! (`blocklist.json`), published at <https://plugins.rustgis.app>.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
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
    /// The `.rgplugin` package: an HTTPS URL, or a path relative to the registry.
    pub package: String,
    /// Lower-case hex SHA-256 of the package: the download must match it.
    pub sha256: String,
    /// The package's size in bytes.
    #[serde(default)]
    pub size: u64,
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
/// mustn't hide the rest.
pub fn parse(text: &str, url: &str) -> Result<Vec<Entry>, String> {
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
    for v in list.into_iter().take(MAX_ENTRIES) {
        let Ok(mut e) = serde_json::from_value::<Entry>(v) else { continue };
        if !valid_id(&e.id, false) || version_parts(&e.version).is_none() || !is_sha256(&e.sha256) || e.name.trim().is_empty() {
            continue;
        }
        e.package = resolve(url, &e.package);
        if !allowed_url(&e.package) || !seen.insert(e.id.clone()) {
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
    Ok(out)
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
        return Err(format!("{url}: only HTTPS addresses are allowed"));
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
    let mut resp =
        agent.get(url).header("User-Agent", concat!("RustGIS/", env!("CARGO_PKG_VERSION"))).call().map_err(|e| format!("{url}: {e}"))?;
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    let mut reader = resp.body_mut().with_config().limit(limit as u64 + 1).reader();
    let mut buf = Vec::new();
    let mut piece = vec![0u8; 64 * 1024];
    loop {
        let n = match reader.read(&mut piece) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("{url}: {e}")),
        };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(piece.get(..n).unwrap_or_default());
        if buf.len() > limit {
            return Err(format!("{url} is larger than {} MB", limit / (1024 * 1024)));
        }
        if !progress(buf.len() as u64, total) {
            return Err(format!("{url}: cancelled"));
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
