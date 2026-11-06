//! Data sources the user returns to: favourites, the ones added recently, saved services,
//! databases and STAC APIs, and a library of well-known public ones. Kept for the session; the
//! desktop and web apps keep them with their preferences. Never a secret: addresses are kept
//! without their passwords, tokens or signatures.

use rustgis_data::source::credentials::{redact_query, secret_key};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How many recent sources are kept.
pub const MAX_RECENT: usize = 20;
/// How many favourites and saved sources are kept.
pub const MAX_KEPT: usize = 200;
/// The longest address or name kept.
const MAX_TEXT: usize = 2048;

/// The kinds of source, by what adds them.
pub const KINDS: &[&str] = &["file", "url", "tiles", "service", "database", "zarr", "stac", "sample", "s3"];

/// A data source.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Source {
    /// One of [`KINDS`].
    pub kind: String,
    /// What it is called.
    pub name: String,
    /// Its path, web address, connection label or name.
    pub address: String,
    /// What else adds it: a service's type or layer, a database's table, a Zarr variable.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub extra: Value,
}

/// The sources kept.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sources {
    #[serde(default)]
    pub favourites: Vec<Source>,
    #[serde(default)]
    pub recent: Vec<Source>,
    #[serde(default)]
    pub saved: Vec<Source>,
    /// Bumped on every change, so the app knows when to keep them.
    #[serde(skip)]
    pub generation: u64,
}

impl Sources {
    /// Sources read back from the app's preferences: each one checked and cleaned again (the
    /// preferences are a file anyone could have edited).
    pub fn from_json(text: &str) -> Sources {
        let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        // Entry by entry: one that doesn't read is dropped, not the whole list.
        let read = |k: &str| -> Vec<Source> {
            v.get(k)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| serde_json::from_value::<Source>(x.clone()).ok()).filter_map(|x| clean(x).ok()).collect())
                .unwrap_or_default()
        };
        let mut s = Sources { favourites: read("favourites"), recent: read("recent"), saved: read("saved"), generation: 0 };
        s.favourites.truncate(MAX_KEPT);
        s.saved.truncate(MAX_KEPT);
        s.recent.truncate(MAX_RECENT);
        // Preferences carry provenance, never authority from an earlier process's login.
        crate::s3_connections::clear_bindings(
            s.favourites.iter_mut().chain(&mut s.recent).chain(&mut s.saved).map(|source| &mut source.address),
        );
        s
    }

    /// Takes in the sources kept with the preferences, keeping what this session already has:
    /// in-memory entries stay (and first), stored ones are added after them unless the same source
    /// ([`Source::is`]) is there already.
    pub fn merge(&mut self, stored: Sources) {
        fn join(now: &mut Vec<Source>, stored: Vec<Source>, cap: usize) {
            for s in stored {
                if now.len() >= cap {
                    break;
                }
                if !now.iter().any(|n| n.is(&s.kind, &s.address)) {
                    now.push(s);
                }
            }
        }
        join(&mut self.favourites, stored.favourites, MAX_KEPT);
        join(&mut self.saved, stored.saved, MAX_KEPT);
        join(&mut self.recent, stored.recent, MAX_RECENT);
        self.generation += 1;
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Notes a source just added (the most recent first, each once).
    pub fn note(&mut self, s: Source) {
        let Ok(s) = clean(s) else { return };
        self.recent.retain(|r| !same(r, &s));
        self.recent.insert(0, s);
        self.recent.truncate(MAX_RECENT);
        self.generation += 1;
    }

    /// Keeps a source in `list` (`favourites` or `saved`), replacing the one with the same
    /// identity ([`Source::is`]: kind and address), whose name and `extra` the new one's take over.
    pub fn keep(&mut self, list: &str, s: Source) -> Result<Source, String> {
        let s = clean(s)?;
        let l = self.list_mut(list)?;
        // The same source keeps its place.
        if let Some(slot) = l.iter_mut().find(|r| same(r, &s)) {
            *slot = s.clone();
        } else {
            if l.len() >= MAX_KEPT {
                return Err(format!("up to {MAX_KEPT} sources in {list}"));
            }
            l.push(s.clone());
        }
        self.generation += 1;
        // What was stored (cleaned): the caller reports it, wherever it sits in the list.
        Ok(s)
    }

    /// Forgets the source of `kind` at `address` from `list`; whether there was one.
    pub fn forget(&mut self, list: &str, kind: &str, address: &str) -> Result<bool, String> {
        let l = self.list_mut(list)?;
        let before = l.len();
        l.retain(|r| !r.is(kind, address));
        let gone = l.len() != before;
        if gone {
            self.generation += 1;
        }
        Ok(gone)
    }

    fn list_mut(&mut self, list: &str) -> Result<&mut Vec<Source>, String> {
        match list {
            "favourites" => Ok(&mut self.favourites),
            "saved" => Ok(&mut self.saved),
            "recent" => Ok(&mut self.recent),
            other => Err(format!("unknown list '{other}' (favourites, saved or recent)")),
        }
    }
}

fn same(a: &Source, b: &Source) -> bool {
    a.is(&b.kind, &b.address)
}

impl Source {
    /// A source's identity: its kind and address. Everywhere a list is matched (kept, forgotten,
    /// noted as recent, shown as a favourite) it is by this; `extra` (a layer, a service type)
    /// is what the last one kept said.
    pub fn is(&self, kind: &str, address: &str) -> bool {
        self.kind == kind && self.address == address
    }
}

/// A source made safe to keep: a known kind, bounded text, and an address without secrets.
pub fn clean(mut s: Source) -> Result<Source, String> {
    if !KINDS.contains(&s.kind.as_str()) {
        return Err(format!("unknown kind '{}' ({})", s.kind, KINDS.join(", ")));
    }
    s.address = redact(s.address.trim());
    // Provenance has its own structural bound; it must not consume the destination's limit.
    let (head, archive) = template_archive(&s.address).unwrap_or(("", s.address.as_str()));
    let bounded = match rustgis_data::s3_address::parse(archive) {
        Ok(Some(bound)) => head.len().saturating_add(bound.url.len()) <= MAX_TEXT,
        Ok(None) => s.address.len() <= MAX_TEXT,
        Err(_) => false,
    };
    if s.address.is_empty() || !bounded || s.address.chars().any(char::is_control) {
        return Err("a source needs an address (a path, web address or connection)".into());
    }
    s.name = s.name.trim().chars().filter(|c| !c.is_control()).take(200).collect();
    if s.name.is_empty() {
        s.name = default_name(&s.address);
    }
    // What else adds it: a small object of short strings, never anything that looks secret.
    s.extra = match s.extra {
        Value::Object(o) => Value::Object(
            o.into_iter().filter(|(k, v)| k.len() <= 32 && !secret_key(k) && v.as_str().is_some_and(|t| t.len() <= 512)).take(8).collect(),
        ),
        _ => Value::Null,
    };
    if s.extra.as_object().is_some_and(|o| o.is_empty()) {
        s.extra = Value::Null;
    }
    Ok(s)
}

/// A name from an address: its last part, without the extension.
pub fn default_name(address: &str) -> String {
    let base = address.split(['?', '#']).next().unwrap_or(address).trim_end_matches(['/', '\\']);
    let last = base.rsplit(['/', '\\']).next().unwrap_or(base);
    let stem = last.rsplit_once('.').map(|(s, _)| s).filter(|s| !s.is_empty()).unwrap_or(last);
    let n: String = stem.chars().take(80).collect();
    if n.is_empty() { address.chars().take(80).collect() } else { n }
}

/// Whether command parameters hold something that gives access: a non-empty value under a key
/// that names a password, key, secret or token ([`secret_key`]), or an address or PostgreSQL
/// connection that [`redact`] would change. Recording a task leaves such a command out, since a
/// task is saved with the project and shared as a file.
pub fn holds_secret(params: &serde_json::Value) -> bool {
    // Keys holding a file's contents.
    fn contents_key(k: &str) -> bool {
        k.eq_ignore_ascii_case("data") || k.eq_ignore_ascii_case("text")
    }
    // A PEM private key (`-----BEGIN … PRIVATE KEY-----`), wherever it was pasted.
    fn private_key(t: &str) -> bool {
        t.contains("PRIVATE KEY-----")
    }
    fn walk(v: &serde_json::Value, depth: usize) -> bool {
        use serde_json::Value;
        match v {
            Value::Object(o) if depth < 16 => o.iter().any(|(k, v)| {
                // A switch (`"token": false`) holds no secret; a number might (a PIN).
                let filled = match v {
                    Value::String(t) => !t.trim().is_empty(),
                    Value::Null | Value::Bool(_) => false,
                    Value::Object(o) => !o.is_empty(),
                    Value::Array(a) => !a.is_empty(),
                    Value::Number(_) => true,
                };
                // A bare `key` beside a web address is that service's key; elsewhere it's as likely
                // a field or a lookup key (a `?key=…` in an address `redact` reads in the string).
                // (The address may sit inside a sibling: `{"source": {"url": …}, "key": …}`; a file's
                // contents, under `data` or `text`, isn't one.)
                fn address(v: &Value, depth: usize) -> bool {
                    match v {
                        Value::String(t) => t.trim_start().get(..4).is_some_and(|p| p.eq_ignore_ascii_case("http")),
                        Value::Object(o) if depth < 4 => o.iter().any(|(k, v)| !contents_key(k) && address(v, depth + 1)),
                        Value::Array(a) if depth < 4 => a.iter().any(|v| address(v, depth + 1)),
                        _ => false,
                    }
                }
                let beside_address = || o.iter().any(|(sibling, v)| sibling != k && !contents_key(sibling) && address(v, 0));
                // A file's contents (Add Data's `data`, a GeoJSON `text`) isn't an address to read
                // through, only checked for a private key pasted in.
                let contents = contents_key(k);
                let inside = match v {
                    // (Short ones, which can't be much of a file, are read as any string.)
                    Value::String(t) if contents && t.len() > 1024 => private_key(t),
                    _ => walk(v, depth + 1),
                };
                let plain_key = k.eq_ignore_ascii_case("key");
                let key_like = || v.as_str().is_some_and(looks_like_a_key);
                (secret_key(k) && (!plain_key || beside_address() || key_like()) && filled) || inside
            }),
            Value::Array(a) if depth < 16 => a.iter().any(|v| walk(v, depth + 1)),
            Value::Object(_) | Value::Array(_) => true,
            Value::String(t) => private_key(t) || redact(t) != *t,
            _ => false,
        }
    }
    walk(params, 0)
}

/// Whether `t` reads as an access key rather than a name: long, with no spaces, and changing
/// between lower case, upper case and digits at a good share of its letters and digits, as a
/// random token does (`sk-4f9aB27c…`, `pk.eyJ1Ijoi…`, a hex hash or id), unlike a field or a
/// lookup key, whose words and numbers come in runs (`parcel_id`, `municipalityCode2020`,
/// `area_2010_2020_km2_total`). A hex id counts as a key on purpose: it may be one, and a step
/// left out (with a notice) costs less than a key shared.
pub(crate) fn looks_like_a_key(t: &str) -> bool {
    #[derive(PartialEq)]
    enum Class {
        Lower,
        Upper,
        Digit,
    }
    let t = t.trim();
    let class = |c: char| {
        if c.is_ascii_digit() {
            Some(Class::Digit)
        } else if c.is_ascii_lowercase() {
            Some(Class::Lower)
        } else if c.is_ascii_uppercase() {
            Some(Class::Upper)
        } else {
            None
        }
    };
    let classes: Vec<Option<Class>> = t.chars().map(class).collect();
    let alnum = classes.iter().filter(|c| c.is_some()).count();
    // Changes between two letters or digits side by side (a separator breaks the pair). An upper
    // case letter followed by lower case is a capitalised word going on (`Land` in camelCase), so
    // each word boundary counts once, at its capital.
    let changes =
        classes.windows(2).filter(|w| matches!(w, [Some(a), Some(b)] if a != b && !(*a == Class::Upper && *b == Class::Lower))).count();
    t.chars().count() >= 20 && !t.contains(char::is_whitespace) && changes * 10 >= alnum * 3
}

/// Whether command parameters set a plain `key` (at any depth), which [`holds_secret`] lets
/// through: as often a field or a lookup key as an access key, so a recording keeps it and says so.
pub fn holds_plain_key(params: &serde_json::Value) -> bool {
    fn walk(v: &serde_json::Value, depth: usize) -> bool {
        use serde_json::Value;
        match v {
            Value::Object(o) if depth < 16 => o
                .iter()
                .any(|(k, v)| (k.eq_ignore_ascii_case("key") && v.as_str().is_some_and(|t| !t.trim().is_empty())) || walk(v, depth + 1)),
            Value::Array(a) if depth < 16 => a.iter().any(|v| walk(v, depth + 1)),
            _ => false,
        }
    }
    walk(params, 0)
}

/// Text with every web address in it redacted (`sources::redact`): an error's text may quote the
/// address asked for, or one a redirect led to, keys and all. Each address is redacted alone, so
/// the words around it stay; punctuation that ends a sentence isn't taken as part of it.
pub fn redact_text(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|piece| {
            let word = piece.trim_end();
            let Some(at) = word.find("://") else { return piece.to_string() };
            // The address starts at its scheme, after any quote or bracket before it.
            let scheme = |c: &char| c.is_ascii_alphanumeric() || "+.-".contains(*c);
            let start = word.get(..at).and_then(|b| b.char_indices().rev().find(|(_, c)| !scheme(c))).map_or(0, |(i, c)| i + c.len_utf8());
            let end = word.trim_end_matches([':', ',', ';', '.', ')', ']', '>', '"', '\'', '»', '›', '”', '’']).len().max(at + 3);
            let (before, address, after) =
                (word.get(..start).unwrap_or(""), word.get(start..end).unwrap_or(""), word.get(end..).unwrap_or(""));
            format!("{before}{}{after}{}", redact(address), piece.get(word.len()..).unwrap_or(""))
        })
        .collect()
}

/// A PMTiles or COG tile template (`pmtiles:{z}/{x}/{y}@archive`, `cog:…@archive`): what comes
/// before its archive (up to and with the first `@`), and the archive, whose address is the one
/// that holds credentials and names a saved S3 connection.
pub(crate) fn template_archive(address: &str) -> Option<(&str, &str)> {
    use crate::render::rustgis_tiles::{cog, pmtiles};
    let rest = [pmtiles::SCHEME, cog::SCHEME].iter().find_map(|s| address.strip_prefix(s))?;
    let (_, archive) = rest.split_once('@')?;
    let head = address.get(..address.len() - archive.len())?;
    Some((head, archive))
}

/// An address without what would give someone else the user's access: the user and password of
/// a web address, query parameters that are keys, tokens or signatures (a signed blob's SAS, an
/// S3 presigned URL), and a PostgreSQL connection string's password. A MySQL, MariaDB or SQL Server
/// address keeps its user (the one to connect as), never its password. An address read through a
/// saved S3 connection is redacted inside (its inner address), and its length-prefixed form
/// rebuilt; a tile template, in its archive. Web-login runtime bindings are never published.
pub fn redact(address: &str) -> String {
    if let Some((head, archive)) = template_archive(address) {
        return format!("{head}{}", redact(archive));
    }
    let inner = crate::render::rustgis_tiles::logins::inner(address);
    if inner != address {
        return redact(inner);
    }
    if let Ok(Some(b)) = rustgis_data::s3_address::parse(address)
        && let Ok(rebuilt) = rustgis_data::s3_address::bind(b.binding, b.endpoint, &redact(b.url))
    {
        return rebuilt;
    }
    if let Some((public, _)) = rustgis_data::source::myconn::split_password(address) {
        return public;
    }
    if let Some((public, _)) = rustgis_data::source::msconn::split_password(address) {
        return public;
    }
    let lower = address.to_ascii_lowercase();
    // A PostgreSQL connection (`postgres:` or `postgresql:` keyword strings, GDAL's `PG:`, URIs):
    // libpq's own rules (quoted values, escapes, `key = value`), without its secrets.
    if ["postgres:", "postgresql:", "pg:"].iter().any(|p| lower.starts_with(p)) {
        // One that can't be taken apart isn't kept (an empty address, refused by `clean`).
        return rustgis_data::source::pgconn::public_label(address).unwrap_or_default();
    }
    let Some((scheme, rest)) = address.split_once("://") else { return address.to_string() };
    // The user and password go first, before anything splits the address: a password may hold
    // an unencoded `/`, `?`, `#` or `@`. They end at the last `@` before the first `/`, or (a `/`
    // in the password) at the last `@` with a `:` before it.
    let cut = rustgis_data::source::pgconn::userinfo_end(rest);
    let rest = match cut {
        Some(i) => rest.get(i + 1..).unwrap_or(""),
        None => rest,
    };
    // The fragment never: OAuth and SAS-style links carry tokens there, and nothing that adds a
    // source needs it.
    let rest = rest.split('#').next().unwrap_or(rest);
    let mut out = format!("{scheme}://{rest}");
    redact_query(&mut out);
    out
}

fn text<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// The source a successful command added, if it adds one.
pub fn added_by(id: &str, p: &Value) -> Option<Source> {
    let src = |kind: &str, address: &str, name: Option<&str>, extra: Value| Source {
        kind: kind.into(),
        name: name.unwrap_or_default().to_string(),
        address: address.to_string(),
        extra,
    };
    let layer = |k: &str| text(p, k).map(|l| json!({k: l})).unwrap_or(Value::Null);
    match id {
        "layer.addData" => Some(src("file", text(p, "path")?, text(p, "name"), layer("layer"))),
        "layer.addUrl" | "layer.addDownload" => Some(src("url", text(p, "url")?, text(p, "name"), layer("layer"))),
        "map.addTiles" => {
            let url = text(p, "url")?;
            let mut extra = serde_json::Map::new();
            if let Some(k) = text(p, "kind") {
                extra.insert("kind".into(), json!(k));
            }
            Some(src("tiles", url, text(p, "name"), Value::Object(extra)))
        }
        "service.add" | "service.import" => {
            let mut extra = serde_json::Map::new();
            for k in ["layer", "type"] {
                if let Some(v) = text(p, k) {
                    extra.insert(k.into(), json!(v));
                }
            }
            Some(src("service", text(p, "url")?, None, Value::Object(extra)))
        }
        "layer.addDatabase" => Some(src("database", text(p, "database")?, None, layer("layer"))),
        "layer.addZarr" => {
            let address = text(p, "url").or_else(|| text(p, "path"))?;
            Some(src("zarr", address, None, text(p, "variable").map(|v| json!({"variable": v})).unwrap_or(Value::Null)))
        }
        "stac.addAsset" => Some(src("stac", text(p, "url")?, None, layer("asset"))),
        "stac.addCube" => {
            let mut extra = serde_json::Map::new();
            for k in ["collection", "asset"] {
                if let Some(v) = text(p, k) {
                    extra.insert(k.into(), json!(v));
                }
            }
            // No API given: Earth Search, as the command takes.
            Some(src("stac", text(p, "url").unwrap_or(crate::render::rustgis_tiles::stac::EARTH_SEARCH), None, Value::Object(extra)))
        }
        "layer.addSample" => Some(src("sample", text(p, "name")?, None, Value::Null)),
        _ => None,
    }
}

/// Well-known public sources, ready to add: basemap tiles, map and feature services, STAC APIs
/// and the sample data.
pub fn library() -> &'static [(&'static str, Source)] {
    static LIBRARY: std::sync::OnceLock<Vec<(&'static str, Source)>> = std::sync::OnceLock::new();
    LIBRARY.get_or_init(build_library)
}

fn build_library() -> Vec<(&'static str, Source)> {
    let s = |kind: &str, name: &str, address: &str, extra: Value| Source {
        kind: kind.into(),
        name: name.into(),
        address: address.into(),
        extra,
    };
    vec![
        (
            "Basemaps",
            s(
                "tiles",
                "OpenStreetMap",
                "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
                json!({"attribution": "© OpenStreetMap contributors", "maxZoom": "19"}),
            ),
        ),
        (
            "Imagery",
            s("service", "EOX Sentinel-2 cloudless", "https://tiles.maps.eox.at/wmts/1.0.0/WMTSCapabilities.xml", json!({"type": "wmts"})),
        ),
        (
            "Imagery",
            s(
                "service",
                "NASA GIBS (Web Mercator)",
                "https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/1.0.0/WMTSCapabilities.xml",
                json!({"type": "wmts"}),
            ),
        ),
        (
            "Imagery",
            s(
                "service",
                "USGS Imagery",
                "https://basemap.nationalmap.gov/arcgis/rest/services/USGSImageryOnly/MapServer",
                json!({"type": "arcgis"}),
            ),
        ),
        (
            "Maps",
            s("service", "USGS Topo", "https://basemap.nationalmap.gov/arcgis/rest/services/USGSTopo/MapServer", json!({"type": "arcgis"})),
        ),
        (
            "Maps",
            s("service", "USGS Hydrography", "https://hydro.nationalmap.gov/arcgis/rest/services/nhd/MapServer", json!({"type": "arcgis"})),
        ),
        (
            "Maps",
            s("service", "GEBCO bathymetry", "https://wms.gebco.net/mapserv?request=getcapabilities&service=wms", json!({"type": "wms"})),
        ),
        (
            "Maps",
            s(
                "service",
                "swisstopo WMTS (geo.admin.ch)",
                "https://wmts.geo.admin.ch/EPSG/3857/1.0.0/WMTSCapabilities.xml",
                json!({"type": "wmts"}),
            ),
        ),
        // The same maps as images drawn on request: unlike the tiles, most layers answer Identify.
        (
            "Maps",
            s(
                "service",
                "swisstopo WMS (geo.admin.ch)",
                "https://wms.geo.admin.ch/?SERVICE=WMS&REQUEST=GetCapabilities&VERSION=1.3.0",
                json!({"type": "wms"}),
            ),
        ),
        ("Catalogs", s("stac", "Earth Search (AWS)", "https://earth-search.aws.element84.com/v1", json!({"api": "true"}))),
        (
            "Catalogs",
            s("stac", "Microsoft Planetary Computer", "https://planetarycomputer.microsoft.com/api/stac/v1", json!({"api": "true"})),
        ),
        ("Sample data", s("sample", "World Countries", "countries", Value::Null)),
        ("Sample data", s("sample", "Populated Places", "cities", Value::Null)),
        ("Sample data", s("sample", "Major Rivers", "rivers", Value::Null)),
        ("Sample data", s("sample", "Major Lakes", "lakes", Value::Null)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_sources_never_reuse_a_previous_process_login_binding() {
        let address = rustgis_data::s3_address::bind("0123456789abcdef", "https://s3.example.org", "s3://data/a.fgb").unwrap();
        let source = Source { kind: "url".into(), name: "Saved".into(), address, extra: Value::Null };
        let text = serde_json::json!({"favourites": [source.clone()], "saved": [source.clone()], "recent": [source]}).to_string();
        let restored = Sources::from_json(&text);
        for source in restored.favourites.iter().chain(&restored.saved).chain(&restored.recent) {
            let bound = rustgis_data::s3_address::parse(&source.address).unwrap().unwrap();
            assert_eq!(bound.binding, "");
            assert_eq!(bound.endpoint, "https://s3.example.org");
            assert_eq!(bound.url, "s3://data/a.fgb");
        }
    }

    #[test]
    fn a_saved_archive_still_bounds_its_template_head() {
        let address = rustgis_data::s3_address::bind("", "", "s3://data/a.pmtiles").unwrap();
        let source = Source {
            kind: "url".into(),
            name: "Huge template".into(),
            address: format!("pmtiles:{}@{address}", "x".repeat(MAX_TEXT)),
            extra: Value::Null,
        };
        assert!(clean(source).is_err());
    }

    #[test]
    fn secrets_in_command_parameters_are_noticed() {
        use serde_json::json;
        for p in [
            json!({"database": "postgres:host=db dbname=gis user=ana", "password": "s3cret"}),
            json!({"database": "postgres:host=db dbname=gis user=ana password=s3cret"}),
            json!({"url": "https://ana:s3cret@example.com/data.geojson"}),
            json!({"url": "https://example.com/tiles/{z}/{x}/{y}.png?access_token=pk.abc"}),
            json!({"url": "https://maps.example.com/tiles/{z}/{x}/{y}.png?key=abc"}),
            json!({"url": "https://api.example.com/tiles", "key": "abc"}),
            json!({"source": {"url": "https://api.example.com/tiles"}, "key": "abc"}),
            // A plain key with no address near it, but reading as an access key.
            json!({"provider": "x", "key": "sk-4f9aB27c81dE0e5f3a6b9C"}),
            json!({"key": "pk.eyJ1IjoiYW5hIiwiYSI6ImNsMnQ4"}),
            // Mostly letters, mixed case, as random tokens are.
            json!({"key": "kXqPzLmNvBtRwYsHaJdFgQ"}),
            // A hex hash or id counts as a key on purpose (see `looks_like_a_key`).
            json!({"key": "3fa85f64-5717-4562-b3fc-2c963f66afa6"}),
            json!({"source": {"headers": {"X-API-Key": "abc"}}}),
            json!({"url": "https://example.com/data.geojson", "headers": {"Authorization": "Bearer abc"}}),
            json!({"headers": {"Cookie": "session=abc"}}),
            json!({"text": "https://ana:s3cret@example.com/x"}),
            json!({"url": format!("https://example.com/{}?sig=abc&sv=2024", "a".repeat(10_000))}),
            json!({"certificate": "-----BEGIN RSA PRIVATE KEY-----
MIIE
-----END RSA PRIVATE KEY-----"}),
        ] {
            assert!(holds_secret(&p), "{p}");
        }
        for p in [
            json!({"layer": "Lakes", "color": "#336699"}),
            json!({"tool": "buffer", "params": {"input": "rivers", "distance": 30, "output": "APP"}}),
            json!({"url": "https://example.com/data.geojson"}),
            json!({"database": "postgres:host=db dbname=gis user=ana", "password": ""}),
            json!({"url": "https://example.com/data.geojson", "token": false}),
            json!({"token": {}, "password": []}),
            json!({"layer": "Parcels", "key": "parcel_id"}),
            json!({"fileName": "big.geojson", "data": "A".repeat(100_000)}),
            // A file's contents, past what an address could be, isn't read as one.
            json!({"fileName": "a.geojson", "text": format!("{{\"type\": \"FeatureCollection\", \"note\": \"https://x.example/?token=t\", \"pad\": \"{}\"}}", "x".repeat(2000))}),
        ] {
            assert!(!holds_secret(&p), "{p}");
        }
        // A plain key is let through, and noticed so the user is told.
        assert!(holds_plain_key(&json!({"layer": "Parcels", "key": "parcel_id"})));
        // A file's contents beside a plain key is no address for it.
        assert!(!holds_secret(&json!({"key": "parcel_id", "source": {"data": "https://example.com/list"}})));
        assert!(!holds_secret(&json!({"key": "parcel_id", "text": "https://example.com/list"})));
        // Names stay plain keys, long or not.
        for name in [
            "parcel_id",
            "municipality_code_2020",
            "municipalityCode2020Total",
            "parcelLandUseAreaCode2020",
            "ParcelLandUseAreaCode",
            "CD_MUN",
            "nome do município com espaços longos",
            "area_2010_2020_km2_total",
        ] {
            assert!(!holds_secret(&json!({"layer": "Parcels", "key": name})), "{name}");
        }
        assert!(holds_plain_key(&json!({"source": [{"key": "abc"}]})));
        assert!(!holds_plain_key(&json!({"layer": "Parcels", "key": ""})));
        assert!(!holds_plain_key(&json!({"api_key": "abc"})));
    }

    #[test]
    fn postgres_connections_lose_every_secret() {
        // Quoted values with spaces and escapes, `key = value`, other secret keys, every form.
        for (given, kept) in [
            ("postgres:host=h password='my secret' user=u", "postgres:host=h user=u"),
            ("postgres:host=h password = 'a \\'b c' dbname=g", "postgres:host=h dbname=g"),
            ("postgresql:host=h sslpassword='k k' passfile=/p user=u", "postgres:host=h user=u"),
            ("PG:host=h dbname=g password=pw", "postgres:host=h dbname=g"),
            ("postgres:host=h password='unterminated user=u", "postgres:host=h"),
            ("postgresql://u:p w@h:5432/g?sslmode=require&password=x", "postgresql://u@h:5432/g?sslmode=require"),
        ] {
            let r = redact(given);
            assert_eq!(r, kept, "{given}");
            for secret in ["secret", "pw", "'b", "k k", "p w", "=x", "unterminated"] {
                assert!(!r.contains(secret), "{secret} in {r}");
            }
        }
    }

    #[test]
    fn stored_sources_join_what_the_session_has() {
        // A favourite made (and a source added) before the stored lists are read survives them.
        let mut s = Sources::default();
        s.keep("favourites", Source { kind: "file".into(), address: "/new.gpkg".into(), ..Source::default() }).unwrap();
        s.note(Source { kind: "file".into(), address: "/added.csv".into(), ..Source::default() });
        let stored = Sources::from_json(
            r#"{"favourites":[{"kind":"file","name":"","address":"/old.gpkg"},{"kind":"file","name":"","address":"/new.gpkg"}],"recent":[{"kind":"file","name":"","address":"/older.csv"}],"saved":[{"kind":"stac","name":"","address":"https://s/v1"}]}"#,
        );
        let before = s.generation;
        s.merge(stored);
        let fav: Vec<&str> = s.favourites.iter().map(|f| f.address.as_str()).collect();
        assert_eq!(fav, ["/new.gpkg", "/old.gpkg"]);
        let recent: Vec<&str> = s.recent.iter().map(|f| f.address.as_str()).collect();
        assert_eq!(recent, ["/added.csv", "/older.csv"]);
        assert_eq!(s.saved.len(), 1);
        assert!(s.generation > before);
    }

    #[test]
    fn identity_is_kind_and_address() {
        let mut s = Sources::default();
        let svc = |extra: Value| Source { kind: "service".into(), name: "W".into(), address: "https://h/wms".into(), extra };
        s.keep("saved", svc(json!({"type": "wms"}))).unwrap();
        s.keep("saved", svc(json!({"type": "wms", "layer": "roads"}))).unwrap();
        // The same kind and address: one entry, with the last one's extra.
        assert_eq!(s.saved.len(), 1);
        assert_eq!(s.saved[0].extra, json!({"type": "wms", "layer": "roads"}));
        // Saved again, a source keeps its place in the list.
        s.keep("saved", Source { kind: "file".into(), address: "/z.gpkg".into(), ..Source::default() }).unwrap();
        // What `keep` gives back is the entry stored, even when it sits earlier in the list.
        let kept = s.keep("saved", svc(json!({"type": "wmts"}))).unwrap();
        assert_eq!((kept.address.as_str(), &kept.extra), ("https://h/wms", &json!({"type": "wmts"})));
        assert_eq!(s.saved.last().map(|l| l.address.as_str()), Some("/z.gpkg"));
        assert_eq!(s.saved[0].address, "https://h/wms");
        assert_eq!(s.saved[0].extra, json!({"type": "wmts"}));
        assert!(s.forget("saved", "file", "/z.gpkg").unwrap());
        // Another kind at the same address is another source.
        s.keep("saved", Source { kind: "url".into(), address: "https://h/wms".into(), ..Source::default() }).unwrap();
        assert_eq!(s.saved.len(), 2);
        assert!(s.forget("saved", "service", "https://h/wms").unwrap());
        assert_eq!(s.saved.len(), 1);
        s.note(svc(Value::Null));
        s.note(svc(json!({"layer": "x"})));
        assert_eq!(s.recent.len(), 1);
        assert!(s.recent[0].is("service", "https://h/wms"));
    }

    #[test]
    fn secrets_are_never_kept() {
        assert_eq!(redact("https://me:pw@host.example/a?token=abc&layer=x&X-Amz-Signature=1#k"), "https://host.example/a?layer=x");
        // A port and an `@` later in the address: an ordinary address, left whole.
        assert_eq!(redact("https://host.example:8080/@user/x"), "https://host.example:8080/@user/x");
        assert_eq!(redact("https://h:80/x?email=a@b"), "https://h:80/x?email=a@b");
        assert_eq!(redact("https://[::1]:8080/@a"), "https://[::1]:8080/@a");
        // An empty port is a port too.
        assert_eq!(redact("https://host:/x?email=a@b"), "https://host:/x?email=a@b");
        // A PostgreSQL URI's password, whatever it holds; its fragment never; libpq's hosts kept.
        for (given, kept) in [
            ("postgresql://u:p/w@db.example/gis", "postgresql://u@db.example/gis"),
            ("postgresql://u:p?w@db.example/gis", "postgresql://u@db.example/gis"),
            ("postgresql://u:p#w@db.example/gis", "postgresql://u@db.example/gis"),
            ("postgresql://u:pw@db.example:5432/gis#x", "postgresql://u@db.example:5432/gis"),
            ("postgresql://db.example:5432/gis", "postgresql://db.example:5432/gis"),
            ("postgresql://h1:5432,h2:5432/db?options=a@b", "postgresql://h1:5432,h2:5432/db?options=a@b"),
            // A password of digits with an unencoded `/` reads like a port: a database's name
            // holds no `@`, so the password still goes.
            ("postgresql://u:123/w@db.example/gis", "postgresql://u@db.example/gis"),
            // No credentials: the fragment goes all the same.
            ("postgresql://db.example/gis#x", "postgresql://db.example/gis"),
        ] {
            assert_eq!(redact(given), kept, "{given}");
        }
        // The user and password go, whatever the password holds (an unencoded `?`, `/`, `#`, `@`).
        for (given, kept) in [
            ("https://u:p?w@host.example/x", "https://host.example/x"),
            ("https://u:p/w@host.example/x", "https://host.example/x"),
            ("https://u:p#w@host.example/x", "https://host.example/x"),
            ("https://u:p@w@host.example/x", "https://host.example/x"),
            ("https://u@host.example/x", "https://host.example/x"),
            ("https://host.example/@user/x", "https://host.example/@user/x"),
        ] {
            assert_eq!(redact(given), kept, "{given}");
        }
        // Short keys are secrets only where they are: an ArcGIS `sr` and a WMS `code` stay; an Azure
        // SAS's fields and an OAuth redirect's code go.
        assert_eq!(
            redact("https://h/arcgis/rest/services/x/MapServer/export?bbox=1&sr=3857&f=image"),
            "https://h/arcgis/rest/services/x/MapServer/export?bbox=1&sr=3857&f=image"
        );
        assert_eq!(redact("https://h/wms?service=WMS&code=EPSG:4326"), "https://h/wms?service=WMS&code=EPSG:4326");
        assert_eq!(
            redact("https://a.blob.core.windows.net/c/x.tif?sv=2024&sr=b&sp=r&se=2026&sig=abc&layer=1"),
            "https://a.blob.core.windows.net/c/x.tif?layer=1"
        );
        assert_eq!(redact("https://h/cb?code=xyz&state=s&tile=1"), "https://h/cb?state=s&tile=1");
        for k in ["Refresh_Token", "x-api-key", "API-KEY", "client_secret", "accessKey", "X-Amz-Credential", "x-amz-security-token", "pwd"]
        {
            assert_eq!(redact(&format!("https://h/x?{k}=v&a=1")), "https://h/x?a=1", "{k}");
        }
        // A fragment is never kept (OAuth and SAS-style links put tokens there).
        for f in ["#Access_Token=t", "#sig=s", "#password=p", "#id_token=i", "#map=4/1/2"] {
            assert_eq!(redact(&format!("https://h.example/x.fgb{f}")), "https://h.example/x.fgb", "{f}");
        }
        // A connection with nothing to take apart isn't kept.
        for bare in ["postgres:dbname", "PG:", "postgres:password='x"] {
            assert!(clean(Source { kind: "database".into(), address: bare.into(), ..Source::default() }).is_err(), "{bare}");
        }
        assert_eq!(
            redact("https://acct.blob.core.windows.net/c/a.tif?se=2026&sp=r&sv=1&sig=zzz"),
            "https://acct.blob.core.windows.net/c/a.tif"
        );
        assert_eq!(
            redact("https://m.example/cgi-bin/mapserv?map=/m.map&SERVICE=WMS"),
            "https://m.example/cgi-bin/mapserv?map=/m.map&SERVICE=WMS"
        );
        assert_eq!(redact("postgres:host=h dbname=g password=pw user=u"), "postgres:host=h dbname=g user=u");
        assert_eq!(redact("postgresql://u:pw@h:5432/g"), "postgresql://u@h:5432/g");
        assert_eq!(redact("/data/places.gpkg"), "/data/places.gpkg");
        let s = clean(Source {
            kind: "service".into(),
            name: String::new(),
            address: "https://x.example/wms?apikey=1".into(),
            extra: json!({"type": "wms", "accessToken": "t", "n": 3}),
        })
        .unwrap();
        assert_eq!((s.address.as_str(), s.name.as_str()), ("https://x.example/wms", "wms"));
        assert_eq!(s.extra, json!({"type": "wms"}));
        assert!(clean(Source { kind: "nope".into(), address: "x".into(), ..Source::default() }).is_err());
        assert!(clean(Source { kind: "file".into(), address: " ".into(), ..Source::default() }).is_err());
    }

    #[test]
    fn recent_keeps_each_once_newest_first() {
        let mut s = Sources::default();
        for a in ["/a.geojson", "/b.geojson", "/a.geojson"] {
            s.note(Source { kind: "file".into(), address: a.into(), ..Source::default() });
        }
        let names: Vec<&str> = s.recent.iter().map(|r| r.address.as_str()).collect();
        assert_eq!(names, ["/a.geojson", "/b.geojson"]);
        for i in 0..40 {
            s.note(Source { kind: "file".into(), address: format!("/{i}.csv"), ..Source::default() });
        }
        assert_eq!(s.recent.len(), MAX_RECENT);
        // Read back from preferences someone edited: bad entries are dropped, secrets removed.
        let text = r#"{"favourites":[{"kind":"url","name":"x","address":"https://h/x.fgb?token=s"},{"kind":"bad","name":"","address":"y"}],"recent":[1,2]}"#;
        let back = Sources::from_json(text);
        assert_eq!(back.favourites.len(), 1);
        assert_eq!(back.favourites[0].address, "https://h/x.fgb");
        assert!(back.recent.is_empty());
        assert_eq!(Sources::from_json("not json"), Sources::default());
    }

    #[test]
    fn commands_name_what_they_added() {
        let s =
            added_by("layer.addDatabase", &json!({"database": "postgres:host=h dbname=g password=pw", "password": "pw", "layer": "roads"}))
                .unwrap();
        let s = clean(s).unwrap();
        assert_eq!(s.address, "postgres:host=h dbname=g");
        assert_eq!(s.extra, json!({"layer": "roads"}));
        assert!(!serde_json::to_string(&s).unwrap().contains("pw"));
        // A MySQL address keeps its user, never its password (in the user part or the query).
        let s = added_by("layer.addDatabase", &json!({"database": "mysql://ana:p%40w#x@db:3306/gis?pass=pw2", "password": "pw"})).unwrap();
        let s = clean(s).unwrap();
        assert_eq!(s.address, "mysql://ana@db:3306/gis");
        assert!(!serde_json::to_string(&s).unwrap().contains("pw"));
        assert!(holds_secret(&json!({"database": "mariadb://ana:pw@db/gis"})));
        assert!(!holds_secret(&json!({"database": "mariadb://ana@db/gis"})));
        let s = added_by(
            "layer.addDatabase",
            &json!({"database": "mssql://ana:p%40w@db:1433/gis?password=pw2&encrypt=strict", "password": "pw"}),
        )
        .unwrap();
        let s = clean(s).unwrap();
        assert_eq!(s.address, "mssql://ana@db:1433/gis?encrypt=strict");
        assert!(!serde_json::to_string(&s).unwrap().contains("pw"));
        assert!(holds_secret(&json!({"database": "mssql://ana:pw@db/gis"})));
        assert!(!holds_secret(&json!({"database": "mssql://ana@db/gis"})));
        for key in ["pwd", "passwd", "token", "access_token", "client_secret"] {
            let params = json!({"database": format!("mssql://ana@db/gis?{key}=query-secret&encrypt=strict")});
            assert!(holds_secret(&params), "{key}");
            let saved = clean(added_by("layer.addDatabase", &params).unwrap()).unwrap();
            assert_eq!(saved.address, "mssql://ana@db/gis?encrypt=strict", "{key}");
            assert!(!serde_json::to_string(&saved).unwrap().contains("query-secret"), "{key}");
        }
        assert_eq!(
            added_by("service.add", &json!({"url": "https://h/wms", "layer": "a", "type": "wms"})).unwrap().extra,
            json!({"layer": "a", "type": "wms"})
        );
        assert!(added_by("layer.remove", &json!({})).is_none());
        assert!(added_by("layer.addData", &json!({})).is_none());
        assert!(library().iter().all(|(_, s)| clean(s.clone()).is_ok()));
    }
}
