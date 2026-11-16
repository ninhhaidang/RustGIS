//! The GeoLibre tools in the interface's language, from GeoLibre's language packs (#2224).
//!
//! GeoLibre keeps the translations of its tool catalogue (each tool's name and description, its
//! parameters' labels and help, the toolbox names) out of its builds, in packs of about 2 MB a
//! language that it downloads when asked
//! ([geolibre-language-packs](https://github.com/opengeos/geolibre-language-packs)). RustGIS reads
//! the same packs: [`download`] fetches the one for a language, [`import`] reads one from a file,
//! and either is kept in the app's data folder, where [`translate`] finds it the first time that
//! language asks for a GeoLibre string. A pack is untrusted input, checked as GeoLibre checks it
//! ([`parse`]): its format, version and scope, string values only, at most [`MAX_BYTES`].
//!
//! [`translate`] maps the English text the tools show (the [`ToolSpec`] strings) to the pack's,
//! by tool id and parameter name, so the interface's catalogue lookup can fall back to it like
//! a plugin's `i18n`. The English names stay the tools' names everywhere else (scripts, the
//! command line, layer names, search by the English words). Because the lookup is keyed by the
//! English text, two tools that share an English string get one translation of it: the first
//! tool's, in the tool list's order. The packs rarely disagree on one: the Portuguese pack has two
//! translations for one of its 7,068 English strings (`Ripley's K Function`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use rustgis_analysis::ToolSpec;
use serde_json::{Map, Value};

/// Where GeoLibre publishes its packs: `{BASE_URL}/v1/whitebox/{locale}.json`.
pub const BASE_URL: &str = "https://languages.geolibre.app";

/// The largest pack read (GeoLibre's limit); the published ones are about 2 MB.
pub const MAX_BYTES: usize = 5 * 1024 * 1024;

/// The most strings a pack may hold (GeoLibre's limit).
const MAX_LEAVES: usize = 100_000;

/// The languages GeoLibre publishes a pack for (its `v1/index.json`, the English template left
/// out, and `zh-HK`/`zh-MO`, which RustGIS has no language for), by the codes RustGIS's languages
/// use: lower case (`zh-tw` for GeoLibre's `zh-TW`).
pub const LOCALES: &[&str] =
    &["ar", "az", "de", "es", "fa", "fr", "he", "hi", "id", "it", "ja", "ka", "ko", "nl", "pt", "ru", "th", "tr", "vi", "zh", "zh-tw"];

/// The address of the published pack for `locale`, if GeoLibre publishes one. GeoLibre names a
/// pack by its code with the region in capitals (`zh-TW.json`), and the address is case-sensitive.
pub fn url(locale: &str) -> Option<String> {
    LOCALES.contains(&locale).then(|| {
        let published: Vec<String> = locale
            .split('-')
            .enumerate()
            .map(|(i, part)| if i > 0 && part.len() == 2 { part.to_ascii_uppercase() } else { part.to_string() })
            .collect();
        format!("{BASE_URL}/v1/whitebox/{}.json", published.join("-"))
    })
}

/// One tool's strings in a pack.
#[derive(Debug, Default)]
struct ToolText {
    name: Option<String>,
    description: Option<String>,
    /// By parameter name: its label and its description.
    params: HashMap<String, (Option<String>, Option<String>)>,
}

/// A language pack, checked.
#[derive(Debug)]
pub struct Pack {
    /// The language it translates into, as RustGIS writes it: lower case (`pt`, `zh-tw` for a
    /// `zh-TW` pack).
    pub locale: String,
    /// By tool id (without [`crate::PREFIX`]).
    tools: HashMap<String, ToolText>,
    /// Toolbox names by GeoLibre's key: `terrain`, `lidarAnalysisMetrics` (`LiDAR - …`).
    categories: BTreeMap<String, String>,
    /// Subcategory names by GeoLibre's key: `sampling_gridding`.
    subcategories: HashMap<String, String>,
}

impl Pack {
    /// How many tools it names.
    pub fn tools(&self) -> usize {
        self.tools.values().filter(|t| t.name.is_some()).count()
    }
}

/// Whether `code` is a language code as GeoLibre accepts one: two or three lower-case letters,
/// then `-` and 2 to 8 letters or digits, any number of times (`pt`, `zh-Hant`). It also keeps a
/// pack's file name to these characters.
fn valid_locale(code: &str) -> bool {
    let mut parts = code.split('-');
    let first = parts.next().unwrap_or("");
    (2..=3).contains(&first.len())
        && first.bytes().all(|b| b.is_ascii_lowercase())
        && parts.all(|p| (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Counts the strings in `v`, which must hold only objects and strings; past [`MAX_LEAVES`]
/// in all (`count` carries the running total), it's an error.
fn count_strings(v: &Value, count: &mut usize) -> Result<(), String> {
    match v {
        Value::String(_) => {
            *count += 1;
            if *count > MAX_LEAVES {
                return Err(format!("the language pack holds more than {MAX_LEAVES} strings"));
            }
            Ok(())
        }
        Value::Object(m) => m.values().try_for_each(|c| count_strings(c, count)),
        _ => Err("a language pack holds only objects and strings".into()),
    }
}

/// `v` as an object whose keys are all in `allowed`, else the error `what`.
fn object_of<'a>(v: &'a Value, allowed: &[&str], what: &str) -> Result<&'a Map<String, Value>, String> {
    v.as_object().filter(|m| m.keys().all(|k| allowed.contains(&k.as_str()))).ok_or_else(|| what.to_string())
}

fn string_at(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn strings_of(v: Option<&Value>) -> impl Iterator<Item = (String, String)> + '_ {
    v.and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.trim().to_string())))
        .filter(|(_, v)| !v.is_empty())
}

/// Reads and checks a pack, as GeoLibre does: the `geolibre-language-pack` format, version 1,
/// the `whitebox` scope, a valid language code, only the Processing translations (tool metadata,
/// toolbox and menu names), strings only, and at most [`MAX_BYTES`] and [`MAX_LEAVES`] strings.
pub fn parse(text: &str) -> Result<Pack, String> {
    if text.len() > MAX_BYTES {
        return Err(format!("the language pack is larger than {} MB", MAX_BYTES / (1024 * 1024)));
    }
    let raw: Value = serde_json::from_str(text).map_err(|e| format!("the language pack isn't valid JSON: {e}"))?;
    let root = raw.as_object().ok_or("the language pack isn't a JSON object")?;
    if root.get("format").and_then(Value::as_str) != Some("geolibre-language-pack")
        || root.get("scope").and_then(Value::as_str) != Some("whitebox")
    {
        return Err("this isn't a GeoLibre language pack for the Whitebox tools".into());
    }
    if root.get("formatVersion").and_then(Value::as_u64) != Some(1) {
        return Err(format!("language pack format {} isn't supported (only 1)", root.get("formatVersion").unwrap_or(&Value::Null)));
    }
    // Language codes ignore case; RustGIS's are lower case, so a `zh-TW` pack serves `zh-tw`.
    let locale = root
        .get("locale")
        .and_then(Value::as_str)
        .filter(|l| valid_locale(l))
        .map(str::to_ascii_lowercase)
        .ok_or("the language pack has no valid language code")?;
    let translations = object_of(
        root.get("translations").unwrap_or(&Value::Null),
        &["processing"],
        "the language pack may hold only Processing translations",
    )?;
    let processing = object_of(
        translations.get("processing").unwrap_or(&Value::Null),
        &["toolMeta", "whitebox"],
        "the language pack's Processing translations aren't valid",
    )?;
    let mut count = 0;
    let meta = match processing.get("toolMeta") {
        Some(m) => {
            let m = object_of(m, &["whitebox"], "the language pack may hold only Whitebox tool metadata")?;
            let w = m.get("whitebox").filter(|w| w.is_object()).ok_or("the language pack's tool metadata isn't valid")?;
            count_strings(w, &mut count)?;
            Some(w)
        }
        None => None,
    };
    let menus = match processing.get("whitebox") {
        Some(w) => {
            object_of(w, &["categories", "menuTool", "menuSubcategory"], "the language pack's Whitebox menus hold keys it may not")?;
            count_strings(w, &mut count)?;
            Some(w)
        }
        None => None,
    };
    if count == 0 {
        return Err("the language pack holds no translations".into());
    }
    let tools = meta
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(id, t)| {
            let params = t
                .get("params")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .map(|(p, v)| (p.clone(), (string_at(v, "label"), string_at(v, "description"))))
                .collect();
            (id.clone(), ToolText { name: string_at(t, "name"), description: string_at(t, "description"), params })
        })
        .collect();
    Ok(Pack {
        locale: locale.to_string(),
        tools,
        categories: strings_of(menus.and_then(|m| m.get("categories"))).collect(),
        subcategories: strings_of(menus.and_then(|m| m.get("menuSubcategory"))).collect(),
    })
}

/// The words of `s`: its runs of ASCII letters and digits (`I/O & Management` → `I`, `O`,
/// `Management`), as GeoLibre's keys are made from its English names.
fn words(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_ascii_lowercase)
}

/// `Remote Sensing` → `remoteSensing`, a category's key.
fn camel(s: &str) -> String {
    words(s)
        .enumerate()
        .map(|(i, w)| if i == 0 { w } else { w.get(..1).unwrap_or("").to_ascii_uppercase() + w.get(1..).unwrap_or("") })
        .collect()
}

/// `Sampling & Gridding` → `sampling_gridding`, a subcategory's key.
fn snake(s: &str) -> String {
    words(s).collect::<Vec<_>>().join("_")
}

/// A top toolbox's name (`Terrain`, `LiDAR`): its own key's, else the first part of a
/// `LiDAR - …` category's (the pack names some tops only with their subcategory).
fn category_name<'a>(pack: &'a Pack, top: &str) -> Option<&'a str> {
    let key = camel(top);
    if key.is_empty() {
        return None;
    }
    if let Some(name) = pack.categories.get(&key) {
        return Some(name);
    }
    pack.categories
        .range(key.clone()..)
        .take_while(|(k, _)| k.starts_with(&key))
        .find(|(k, _)| k.get(key.len()..key.len() + 1).is_some_and(|c| c.bytes().all(|b| b.is_ascii_uppercase())))
        .and_then(|(_, v)| v.split(" - ").next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// English text → the pack's, for the strings `tools` show: names, summaries, parameter labels
/// and help, and toolbox names. The first translation of a string wins.
fn english_to_pack(pack: &Pack, tools: &[ToolSpec]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut put = |en: &str, local: Option<&str>| {
        if let Some(local) = local
            && !en.trim().is_empty()
            && local != en
        {
            map.entry(en.to_string()).or_insert_with(|| local.to_string());
        }
    };
    for spec in tools {
        let mut levels = spec.toolbox.split(" › ").skip(1);
        if let Some(top) = levels.next() {
            put(top, category_name(pack, top));
        }
        if let Some(sub) = levels.next() {
            put(sub, pack.subcategories.get(&snake(sub)).map(String::as_str));
        }
        let Some(text) = pack.tools.get(spec.id.trim_start_matches(crate::PREFIX)) else { continue };
        put(spec.name, text.name.as_deref());
        put(spec.summary, text.description.as_deref());
        for p in spec.params {
            let Some((label, help)) = text.params.get(p.name) else { continue };
            put(p.label, label.as_deref());
            put(p.help, help.as_deref());
        }
    }
    map
}

/// The pack in use, with its lookup for the tools listed when it was made.
struct Active {
    pack: Pack,
    map: HashMap<String, String>,
    /// The tool list the lookup was made for: its address and length.
    tools: (usize, usize),
}

static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);

/// Counts changes to what [`translate`] gives: a pack put in use or removed, the tool list
/// reloaded.
static REVISION: AtomicU64 = AtomicU64::new(0);

/// Languages whose pack was looked for in the data folder and isn't there.
static MISSING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Held across reading a kept pack and putting it in use, and across keeping a new one and putting
/// it in use, so a read that started before a download or an import kept a newer pack can't put
/// the older one back in use after it.
#[cfg(not(target_arch = "wasm32"))]
static LIFECYCLE: Mutex<()> = Mutex::new(());

/// Why the pack kept for a language couldn't be read, by language.
static LOAD_ERRORS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Why the pack kept for `locale` can't be used (a damaged file, another format version), if
/// it was tried and failed: the Tools pane and `gp.geolibre` say it.
pub fn load_error(locale: &str) -> Option<String> {
    LOAD_ERRORS.lock().ok()?.as_ref()?.get(locale).cloned()
}

/// Every language whose kept pack couldn't be read, with the reason.
pub fn load_errors() -> Vec<(String, String)> {
    let mut v: Vec<_> = LOAD_ERRORS.lock().ok().and_then(|e| e.clone()).unwrap_or_default().into_iter().collect();
    v.sort();
    v
}

fn published() -> &'static [ToolSpec] {
    rustgis_analysis::published_external_tools_of(crate::SOURCE)
}

fn tools_key(tools: &[ToolSpec]) -> (usize, usize) {
    (tools.as_ptr() as usize, tools.len())
}

/// Puts `pack` in use (`None`: none), in place of the one in use.
pub fn activate(pack: Option<Pack>) {
    if let Ok(mut a) = ACTIVE.lock() {
        *a = pack.map(|pack| {
            let tools = published();
            Active { map: english_to_pack(&pack, tools), tools: tools_key(tools), pack }
        });
    }
    if let Ok(mut m) = MISSING.lock() {
        *m = None;
    }
    if let Ok(mut e) = LOAD_ERRORS.lock() {
        *e = None;
    }
    REVISION.fetch_add(1, Ordering::Relaxed);
}

/// The language of the pack in use.
pub fn active_locale() -> Option<String> {
    ACTIVE.lock().ok().and_then(|a| a.as_ref().map(|a| a.pack.locale.clone()))
}

/// Changes whenever [`translate`] may give something else for the same text, so a cache of its
/// answers knows to start over.
pub fn revision() -> u64 {
    if let Ok(mut a) = ACTIVE.lock()
        && let Some(a) = a.as_mut()
    {
        let tools = published();
        if a.tools != tools_key(tools) {
            a.map = english_to_pack(&a.pack, tools);
            a.tools = tools_key(tools);
            REVISION.fetch_add(1, Ordering::Relaxed);
        }
    }
    REVISION.load(Ordering::Relaxed)
}

/// Whether `locale`'s pack is in use: the one kept for it (natively, read the first time this
/// is asked) or one just downloaded or imported.
pub fn in_use(locale: &str) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    load_kept(locale);
    active_locale().as_deref() == Some(locale)
}

/// `english`, a GeoLibre tool's text, in `locale`, from that language's pack: the one in use,
/// else (natively) the one kept in the data folder, read the first time it is asked for.
pub fn translate(locale: &str, english: &str) -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    load_kept(locale);
    let a = ACTIVE.lock().ok()?;
    let a = a.as_ref().filter(|a| a.pack.locale == locale)?;
    a.map.get(english).cloned()
}

/// Puts the pack kept for `locale` in use, unless one for it is already, or it was looked for
/// and wasn't there. One file read, of about 2 MB, once per language switch.
#[cfg(not(target_arch = "wasm32"))]
fn load_kept(locale: &str) {
    let settled = || {
        active_locale().as_deref() == Some(locale) || MISSING.lock().ok().is_some_and(|m| m.as_ref().is_some_and(|m| m.contains(locale)))
    };
    if settled() {
        return;
    }
    let Ok(_lifecycle) = LIFECYCLE.lock() else { return };
    // A download or an import may have kept and put in use a pack for it while this waited.
    if settled() {
        return;
    }
    match load(locale) {
        Ok(Some(pack)) => activate(Some(pack)),
        Ok(None) => {
            if let Ok(mut m) = MISSING.lock() {
                m.get_or_insert_with(Default::default).insert(locale.to_string());
            }
        }
        Err(e) => {
            log::warn!("GeoLibre tool translations ({locale}): {e}");
            if let Ok(mut m) = MISSING.lock() {
                m.get_or_insert_with(Default::default).insert(locale.to_string());
            }
            if let Ok(mut errors) = LOAD_ERRORS.lock() {
                errors.get_or_insert_with(Default::default).insert(locale.to_string(), e);
            }
        }
    }
}

/// Where packs are kept: a `geolibre-languages` folder in the app's data folder, one
/// `<locale>.json` each.
#[cfg(not(target_arch = "wasm32"))]
pub fn dir() -> Option<std::path::PathBuf> {
    #[cfg(test)]
    if let Some(d) = tests::DIR.lock().ok().and_then(|d| d.clone()) {
        return Some(d);
    }
    rustgis_data::dirs::app_data_dir().map(|d| d.join("geolibre-languages"))
}

#[cfg(not(target_arch = "wasm32"))]
fn file(locale: &str) -> Result<std::path::PathBuf, String> {
    if !valid_locale(locale) {
        return Err(format!("'{locale}' isn't a language code"));
    }
    dir().map(|d| d.join(format!("{locale}.json"))).ok_or_else(|| "can't find the app's data folder".into())
}

/// The languages whose packs are kept, sorted.
#[cfg(not(target_arch = "wasm32"))]
pub fn kept() -> Vec<String> {
    let Some(dir) = dir() else { return Vec::new() };
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").filter(|l| valid_locale(l)).map(str::to_string))
        .collect();
    out.sort();
    out
}

/// The pack kept for `locale` (`None`: none is).
#[cfg(not(target_arch = "wasm32"))]
fn load(locale: &str) -> Result<Option<Pack>, String> {
    let path = file(locale)?;
    if !path.is_file() {
        return Ok(None);
    }
    let text = read_limited(&path)?;
    let pack = parse(&text)?;
    if pack.locale != locale {
        return Err(format!("{} holds the '{}' pack", path.display(), pack.locale));
    }
    Ok(Some(pack))
}

/// Reads at most [`MAX_BYTES`] of `path` (one byte more tells a larger file), whatever its size
/// says: a pipe or a device (`/dev/zero`) reports none.
#[cfg(not(target_arch = "wasm32"))]
fn read_limited(path: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut bytes = Vec::new();
    std::fs::File::open(path).map_err(err)?.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(err)?;
    if bytes.len() > MAX_BYTES {
        return Err(format!("the language pack is larger than {} MB", MAX_BYTES / (1024 * 1024)));
    }
    String::from_utf8(bytes).map_err(|_| format!("{}: the language pack isn't UTF-8 text", path.display()))
}

/// How many of the listed GeoLibre tools `pack` names: what a download or an import reports, so
/// a pack made for another runner's tools shows as translating few.
#[cfg(not(target_arch = "wasm32"))]
fn matched(pack: &Pack) -> usize {
    published().iter().filter(|t| pack.tools.get(t.id.trim_start_matches(crate::PREFIX)).is_some_and(|x| x.name.is_some())).count()
}

/// Keeps `text`, a checked pack for `locale`, and puts it in use.
#[cfg(not(target_arch = "wasm32"))]
fn keep(pack: Pack, text: &str) -> Result<usize, String> {
    let path = file(&pack.locale)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let n = matched(&pack);
    let _lifecycle = LIFECYCLE.lock().map_err(|_| "the tool translations are in an unusable state".to_string())?;
    rustgis_data::io::write_atomic(&path, text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
    activate(Some(pack));
    Ok(n)
}

/// Downloads GeoLibre's pack for `locale`, keeps it and puts it in use; returns how many of the
/// listed tools it names. One request of about 2 MB, which gives up after a minute.
#[cfg(not(target_arch = "wasm32"))]
pub fn download(locale: &str) -> Result<usize, String> {
    let url = url(locale).ok_or_else(|| format!("GeoLibre publishes no tool translations for '{locale}'"))?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(60)))
        .tls_config(rustgis_data::http::tls_config())
        .build()
        .into();
    let mut resp = agent
        .get(&url)
        .header("User-Agent", concat!("RustGIS/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/json")
        .call()
        .map_err(|e| rustgis_data::diagnostics::message_with_address(&url, &e.to_string()))?;
    let text = resp
        .body_mut()
        .with_config()
        .limit(MAX_BYTES as u64)
        .read_to_string()
        .map_err(|e| rustgis_data::diagnostics::message_with_address(&url, &e.to_string()))?;
    let pack = parse(&text)?;
    if pack.locale != locale {
        return Err(format!("{url} holds the '{}' pack", pack.locale));
    }
    keep(pack, &text)
}

/// Reads a pack from `path`, keeps it for the language it names and puts it in use; returns
/// that language and how many of the listed tools it names.
#[cfg(not(target_arch = "wasm32"))]
pub fn import(path: &std::path::Path) -> Result<(String, usize), String> {
    let text = read_limited(path)?;
    let pack = parse(&text)?;
    let locale = pack.locale.clone();
    keep(pack, &text).map(|n| (locale, n))
}

/// Deletes the pack kept for `locale`, and stops using it.
#[cfg(not(target_arch = "wasm32"))]
pub fn remove(locale: &str) -> Result<(), String> {
    let path = file(locale)?;
    if !path.is_file() {
        return Err(format!("no tool translations are kept for '{locale}'"));
    }
    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if active_locale().as_deref() == Some(locale) {
        activate(None);
    }
    if let Ok(mut errors) = LOAD_ERRORS.lock()
        && let Some(errors) = errors.as_mut()
    {
        errors.remove(locale);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_analysis::{ParamKind, ParamSpec};

    /// Where [`dir`] keeps packs in these tests, instead of the app's data folder.
    pub(super) static DIR: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

    fn pack_text(locale: &str) -> String {
        serde_json::json!({
            "format": "geolibre-language-pack", "formatVersion": 1, "scope": "whitebox", "locale": locale,
            "translations": {"processing": {
                "toolMeta": {"whitebox": {"contours_from_raster": {
                    "name": "Curvas de nível a partir de raster",
                    "description": "Cria polilinhas de curvas de nível.",
                    "params": {"interval": {"label": "Intervalo", "description": "Equidistância."}}
                }}},
                "whitebox": {
                    "categories": {"vector": "Vetor", "remoteSensingSar": "Sensoriamento remoto - SAR"},
                    "menuSubcategory": {"sampling_gridding": "Amostragem e grades"},
                    "menuTool": {"contours_from_raster": "Curvas de nível a partir de raster"}
                }
            }}
        })
        .to_string()
    }

    static PARAMS: [ParamSpec; 1] = [ParamSpec {
        name: "interval",
        label: "Interval",
        kind: ParamKind::Number,
        required: false,
        default: "",
        help: "Contour interval value.",
    }];

    fn no_run(_: &dyn rustgis_analysis::Inputs, _: &Value) -> Result<rustgis_analysis::ToolResult, String> {
        Err("not run".into())
    }

    fn tools() -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                id: "geolibre:contours_from_raster",
                name: "Contours From Raster",
                toolbox: "GeoLibre › Vector › Sampling & Gridding",
                summary: "Creates contour polylines from a raster surface model.",
                params: &PARAMS,
                run: no_run,
            },
            ToolSpec {
                id: "geolibre:sar_info",
                name: "SAR Info",
                toolbox: "GeoLibre › Remote Sensing › SAR",
                summary: "",
                params: &[],
                run: no_run,
            },
        ]
    }

    #[test]
    fn a_pack_maps_the_tools_english_text_to_its_own() {
        let pack = parse(&pack_text("pt")).unwrap();
        assert_eq!(pack.locale, "pt");
        assert_eq!(pack.tools(), 1);
        let map = english_to_pack(&pack, &tools());
        assert_eq!(map.get("Contours From Raster").map(String::as_str), Some("Curvas de nível a partir de raster"));
        assert_eq!(
            map.get("Creates contour polylines from a raster surface model.").map(String::as_str),
            Some("Cria polilinhas de curvas de nível.")
        );
        assert_eq!(map.get("Interval").map(String::as_str), Some("Intervalo"));
        assert_eq!(map.get("Contour interval value.").map(String::as_str), Some("Equidistância."));
        assert_eq!(map.get("Vector").map(String::as_str), Some("Vetor"));
        assert_eq!(map.get("Sampling & Gridding").map(String::as_str), Some("Amostragem e grades"));
        // A top named only with its subcategories: the first part of theirs.
        assert_eq!(map.get("Remote Sensing").map(String::as_str), Some("Sensoriamento remoto"));
        assert!(!map.contains_key("SAR"), "the same as the English, or missing: left out");
        assert!(!map.contains_key("SAR Info"), "a tool the pack doesn't name stays in English");
    }

    #[test]
    fn keys_follow_geolibres_names() {
        assert_eq!(camel("Remote Sensing"), "remoteSensing");
        assert_eq!(camel("LiDAR"), "lidar");
        assert_eq!(snake("I/O & Management"), "i_o_management");
        assert_eq!(snake("Streams - Network Extraction"), "streams_network_extraction");
    }

    #[test]
    fn a_pack_is_checked_as_geolibre_checks_it() {
        let good: Value = serde_json::from_str(&pack_text("pt")).unwrap();
        let with = |f: &dyn Fn(&mut Value)| {
            let mut v = good.clone();
            f(&mut v);
            parse(&v.to_string())
        };
        assert!(with(&|v| v["format"] = "other".into()).is_err());
        assert!(with(&|v| v["formatVersion"] = 2.into()).is_err());
        assert!(with(&|v| v["scope"] = "ui".into()).is_err());
        assert!(with(&|v| v["locale"] = "../x".into()).is_err());
        assert!(with(&|v| v["locale"] = "PT".into()).is_err());
        assert!(with(&|v| v["translations"]["menu"] = serde_json::json!({})).is_err(), "only Processing");
        assert!(with(&|v| v["translations"]["processing"]["whitebox"]["toolbar"] = serde_json::json!({})).is_err());
        assert!(with(&|v| v["translations"]["processing"]["whitebox"]["categories"]["vector"] = 3.into()).is_err(), "strings only");
        assert!(with(&|v| v["translations"]["processing"] = serde_json::json!({})).is_err(), "empty");
        assert!(parse("[").is_err());
        assert!(parse(&" ".repeat(MAX_BYTES + 1)).is_err());
        assert!(valid_locale("zh-Hant") && valid_locale("pt") && !valid_locale("p") && !valid_locale("pt-"));
    }

    /// A pack kept, read again in a later session, damaged (and said so), then removed.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_kept_pack_is_read_back_said_when_damaged_and_removed() {
        let dir = std::env::temp_dir().join(format!("rustgis-langpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        *DIR.lock().unwrap() = Some(dir.clone());
        let src = dir.with_extension("json");
        std::fs::write(&src, pack_text("pt")).unwrap();
        let (locale, _) = import(&src).unwrap();
        assert_eq!(locale, "pt");
        assert!(dir.join("pt.json").is_file() && kept() == ["pt"]);
        assert!(in_use("pt"));
        // A later session: nothing in use, the kept file read the first time it's asked for.
        let before = revision();
        activate(None);
        assert!(revision() > before, "a change of pack starts a cache over");
        assert!(in_use("pt") && load_error("pt").is_none());
        // Damaged: said, and not in use.
        activate(None);
        std::fs::write(dir.join("pt.json"), "{").unwrap();
        assert!(!in_use("pt"));
        assert!(load_error("pt").is_some_and(|e| e.contains("JSON")), "{:?}", load_error("pt"));
        assert_eq!(load_errors().len(), 1);
        remove("pt").unwrap();
        assert!(kept().is_empty() && load_error("pt").is_none() && !in_use("pt"));
        assert!(remove("pt").is_err(), "nothing left to remove");
        // A pipe or a device reports no size: the read is bounded all the same.
        #[cfg(unix)]
        assert!(import(std::path::Path::new("/dev/zero")).is_err());
        *DIR.lock().unwrap() = None;
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&src);
    }

    #[test]
    fn only_published_languages_have_an_address() {
        assert_eq!(url("pt").as_deref(), Some("https://languages.geolibre.app/v1/whitebox/pt.json"));
        assert_eq!(url("en"), None);
        assert_eq!(url("zh-tw").as_deref(), Some("https://languages.geolibre.app/v1/whitebox/zh-TW.json"));
        assert_eq!(url("zh-hk"), None);
        assert_eq!(url("zh-TW"), None, "RustGIS's own codes only");
        // GeoLibre's `zh-TW` pack serves RustGIS's `zh-tw`.
        assert_eq!(parse(&pack_text("zh-TW")).unwrap().locale, "zh-tw");
    }
}
