//! JAXA Earth's catalog, read live (opengeos/RustGIS#1532): every dataset of the JAXA Earth API
//! (precipitation, land and sea surface temperature, vegetation, soil moisture, aerosols,
//! elevation, land cover…) as data cubes the Data Cubes pane lists beside its own, one entry per
//! band. JAXA publishes the list as Markdown ([`CATALOG`]): a "Dataset List" section where each
//! `##` heading is a dataset, with `* key: value` lines (`id`, `title`, `bands`, `bbox`, `epsg`,
//! `spatialResolution` in km, `startDate`, `endDate` (`present` while it is kept up to date),
//! `temporalResolution`, `normal`…). Each dataset is a static STAC collection laid out by date,
//! which [`crate::stac::dated`] reads.
//!
//! The catalog is read once a run of the app, in the background, the first time the pane is shown;
//! until it is (and when it can't be: offline), the pane says so and lists its own entries. A
//! dataset that can't be a cube here is said, with why, rather than dropped: a polar projection,
//! a climatology (its dates have no year) or an hourly series. JAXA's entries give no range for
//! their bands: `cube.open` works one out from a sample of the files it adds. Each collection
//! names its own licence (JAXA's terms for most, NASA's or Copernicus's for some), read from its
//! `collection.json` when the pane shows the entry or the cube is added ([`licence`]); the credit
//! JAXA's terms ask for is JAXA and the dataset's name ([`cubes::Entry::credit`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;

use crate::cubes::{self, Entry, Source};

/// JAXA's list of every dataset of the JAXA Earth API.
pub const CATALOG: &str = "https://data.earth.jaxa.jp/app/mcp/catalog.v2.md";
/// Where the datasets' collections are: an entry naming another place is left out.
const BUCKETS: &[&str] =
    &["https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/", "https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/"];
/// JAXA's terms of use for the data.
pub const TERMS: &str = "https://earth.jaxa.jp/en/data/policy/";
/// The most the catalog's text may be (it is about 100 KB).
const MAX_BYTES: usize = 1 << 20;
/// The most datasets read, and bands of one.
const MAX_DATASETS: usize = 500;
const MAX_BANDS: usize = 16;
/// The most characters a dataset's field keeps (descriptions run to about 850).
const MAX_FIELD: usize = 2000;
/// The most cuts noted one by one.
const MAX_CUTS: usize = 500;
/// The most a collection's document may be, read for its licence.
const MAX_COLLECTION_BYTES: usize = 1 << 20;
/// Kilometres in a degree of longitude at the equator: JAXA's resolutions are given in km.
const KM_PER_DEGREE: f64 = 111.32;

/// A dataset of the catalog that isn't listed as a cube, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct LeftOut {
    pub id: String,
    pub title: String,
    pub why: String,
}

/// The catalog, read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    /// A cube for each band of each dataset that can be one.
    pub entries: Vec<Entry>,
    /// The datasets that can't, each once, and each listing of a dataset listed again (so these
    /// and the cubes' datasets add up to [`Catalog::datasets`]).
    pub left_out: Vec<LeftOut>,
    /// How many datasets the catalog lists.
    pub datasets: usize,
    /// The catalog's text that was too long and is cut, where and by how much (each cut name ends
    /// in "…"), and the fields that aren't read, and why.
    pub cut: Vec<String>,
}

/// Where the catalog is in a run of the app.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Not asked for yet (the pane hasn't been shown).
    Unread,
    Reading,
    Read(Arc<Catalog>),
    /// It couldn't be read, and why: asked for again only when someone asks ([`retry`]).
    Failed(String),
}

fn status_cell() -> &'static Mutex<Status> {
    static STATUS: OnceLock<Mutex<Status>> = OnceLock::new();
    STATUS.get_or_init(|| Mutex::new(Status::Unread))
}

/// Where the catalog is.
pub fn status() -> Status {
    status_cell().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Its cubes, once read.
pub fn entries() -> Option<Arc<Vec<Entry>>> {
    static LAST: Mutex<Option<(Arc<Catalog>, Arc<Vec<Entry>>)>> = Mutex::new(None);
    let Status::Read(c) = status() else { return None };
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    match last.as_ref() {
        Some((read, list)) if Arc::ptr_eq(read, &c) => Some(list.clone()),
        _ => {
            let list = Arc::new(c.entries.clone());
            *last = Some((c, list.clone()));
            Some(list)
        }
    }
}

/// Reads the catalog in the background (a thread natively, the browser's fetch on the web), once
/// a run: again only after [`retry`].
pub fn fetch() {
    {
        let mut s = status_cell().lock().unwrap_or_else(|e| e.into_inner());
        if *s != Status::Unread {
            return;
        }
        *s = Status::Reading;
    }
    crate::cache::download_limited(
        CATALOG,
        MAX_BYTES,
        Box::new(|r| {
            if let Err(e) = r.and_then(|bytes| take(&bytes)) {
                log::info!("JAXA Earth's catalog wasn't read: {e}");
                *status_cell().lock().unwrap_or_else(|e| e.into_inner()) = Status::Failed(e);
            }
        }),
    );
}

/// Asks for the catalog again after it failed.
pub fn retry() {
    {
        let mut s = status_cell().lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(*s, Status::Failed(_)) {
            return;
        }
        *s = Status::Unread;
    }
    fetch();
}

/// Takes the catalog's text as read: its number of cubes. One that doesn't read, or lists no
/// dataset that can be a cube, is refused (the status says why).
pub fn take(bytes: &[u8]) -> Result<usize, String> {
    let c = parse(std::str::from_utf8(bytes).map_err(|_| "JAXA Earth's catalog isn't text")?)?;
    if c.entries.is_empty() {
        return Err(format!("JAXA Earth's catalog lists {} datasets, none of which can be a data cube", c.datasets));
    }
    let n = c.entries.len();
    *status_cell().lock().unwrap_or_else(|e| e.into_inner()) = Status::Read(Arc::new(c));
    Ok(n)
}

/// The catalog's datasets as cubes, and those that can't be one. Hostile input: its size, its
/// datasets and their bands are bounded, every field is checked, and a collection is read only
/// from JAXA's buckets.
pub fn parse(text: &str) -> Result<Catalog, String> {
    if text.len() > MAX_BYTES {
        return Err(format!("JAXA Earth's catalog is larger than {} KB", MAX_BYTES >> 10));
    }
    let mut section = String::new();
    let mut cut = Vec::new();
    let mut datasets: Vec<(String, HashMap<String, String>)> = Vec::new();
    let mut open = false;
    for line in text.lines().map(str::trim) {
        if let Some(h) = line.strip_prefix("# ") {
            section = h.trim().to_ascii_lowercase();
            open = false;
        } else if let Some(h) = line.strip_prefix("## ") {
            open = section.starts_with("dataset list");
            if open {
                if datasets.len() == MAX_DATASETS {
                    return Err(format!("JAXA Earth's catalog lists more than {MAX_DATASETS} datasets"));
                }
                let h = h.trim();
                cut_note(&mut cut, h, "heading", h, 200);
                datasets.push((clip(h, 200), HashMap::new()));
            }
        } else if open
            && let Some((k, v)) = line.strip_prefix("* ").and_then(|f| f.split_once(':'))
            && let Some((heading, fields)) = datasets.last_mut()
        {
            let k = k.trim();
            if !(k.len() <= 40 && k.bytes().all(|b| b.is_ascii_alphanumeric())) {
                let k = clip(k, 40);
                note(
                    &mut cut,
                    format!("{}: its field \"{k}\" isn't read (a field's name is 1 to 40 letters and digits)", clip(heading, 120)),
                );
            } else if fields.contains_key(k) {
                note(&mut cut, format!("{}: its {k} is given again; the first is read", clip(heading, 120)));
            } else {
                cut_note(&mut cut, heading, k, v.trim(), MAX_FIELD);
                fields.insert(k.to_string(), clip(v.trim(), MAX_FIELD));
            }
        }
    }
    if datasets.is_empty() {
        return Err("JAXA Earth's catalog has no dataset list".into());
    }
    let mut c = Catalog { datasets: datasets.len(), cut, ..Catalog::default() };
    // The datasets read as cubes so far, by id.
    let mut read = std::collections::HashSet::new();
    for (heading, f) in &datasets {
        let id = f.get("id").map(String::as_str).unwrap_or("");
        let title = f.get("title").map(String::as_str).filter(|t| !t.is_empty()).unwrap_or(heading.as_str());
        // Listed again once read as cubes, it is one dataset: the listing again is left out, and
        // says so. One whose earlier listing couldn't be a cube is read on its own.
        let leave_out = |c: &mut Catalog, why: String| {
            // A heading's cut is noted where it is read; a title of its own, here.
            if title != heading.as_str() {
                cut_note(&mut c.cut, id, "title", title, 200);
            }
            c.left_out.push(LeftOut { id: clip(id, 120), title: clip(title, 200), why });
        };
        if !id.is_empty() && read.contains(id) {
            leave_out(&mut c, "listed again; its first listing, further up the catalog, stands for it".to_string());
            continue;
        }
        match cubes_of(id, title, f, &mut c.cut) {
            Ok(es) => {
                read.insert(id);
                c.entries.extend(es)
            }
            Err(why) => leave_out(&mut c, why),
        }
    }
    Ok(c)
}

#[cfg(test)]
impl Entry {
    /// The JAXA Earth dataset it reads, if it is one of the catalog's.
    fn source_dataset(&self) -> Option<&str> {
        match &self.source {
            Source::Stac { collection, .. } if is_collection(&self.url) => Some(collection),
            _ => None,
        }
    }
}

/// Whether `url` is a JAXA Earth collection's `collection.json`.
pub fn is_collection(url: &str) -> bool {
    BUCKETS.iter().any(|b| url.strip_prefix(b).and_then(|rest| rest.strip_suffix("/collection.json")).is_some_and(is_id))
}

/// A dataset id: letters, digits, `.`, `_` and `-`, starting with a letter or digit, no `..`.
fn is_id(id: &str) -> bool {
    (1..=120).contains(&id.len())
        && id.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !id.contains("..")
}

/// A dataset's cubes, a band each; why it can't be one otherwise.
fn cubes_of(id: &str, title: &str, f: &HashMap<String, String>, cut: &mut Vec<String>) -> Result<Vec<Entry>, String> {
    if !is_id(id) {
        return Err("it has no id of letters, digits, dots, underscores and hyphens".into());
    }
    let get = |k: &str| f.get(k).map(String::as_str).unwrap_or("");
    // What a data cube here can't read, said first.
    match get("epsg").trim() {
        "4326" => {}
        "3031" | "3995" => {
            return Err(format!("its grid is polar (EPSG:{}), and a data cube reads JAXA's EPSG:4326 ones", get("epsg").trim()));
        }
        e => return Err(format!("its grid is EPSG:{}, and a data cube reads JAXA's EPSG:4326 ones", clip(e, 20))),
    }
    if get("normal").trim().eq_ignore_ascii_case("true") {
        return Err("it is a climatology (an average of past years), whose dates have no year".into());
    }
    let step = get("temporalResolution").trim().to_ascii_lowercase();
    if step.contains("hour") || id.to_ascii_lowercase().ends_with("_hourly") {
        return Err("it is hourly, and a data cube has one file a day".into());
    }
    let mut bands: Vec<&str> = Vec::new();
    for b in get("bands").split(',').map(str::trim).filter(|b| !b.is_empty()) {
        // A band named twice is one cube (its id is the dataset's and the band's).
        if bands.contains(&b) {
            note(cut, format!("{}: its band {} is listed twice; it is one cube", clip(id, 120), clip(b, 40)));
        } else {
            bands.push(b);
        }
    }
    if bands.is_empty()
        || bands.len() > MAX_BANDS
        || !bands.iter().all(|b| b.len() <= 40 && b.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')))
    {
        return Err(format!("its bands aren't 1 to {MAX_BANDS} names of letters, digits, underscores and hyphens"));
    }
    let bbox = bbox(get("bbox")).ok_or("its bbox isn't [west, south, east, north] in degrees")?;
    let start = cubes::day(get("startDate").get(..10).unwrap_or("")).ok_or("its startDate isn't a date")?;
    let end = match get("endDate").trim() {
        "present" => None,
        d => Some(cubes::day(d.get(..10).unwrap_or("")).ok_or("its endDate isn't a date or present")?),
    };
    if end.as_ref().is_some_and(|e| *e < start) {
        return Err("its endDate is before its startDate".into());
    }
    let given = get("spatialResolution").trim();
    let km = given.parse::<f64>().ok().filter(|k| k.is_finite() && *k > 0.0 && *k < 1000.0);
    if km.is_none() && !given.is_empty() {
        let v = clip(given, 40);
        note(
            cut,
            format!(
                "{}: its spatialResolution \"{v}\" isn't read (kilometres, above 0 and below 1000), so its resolution is not given",
                clip(id, 120)
            ),
        );
    }
    let url = match get("collection").trim() {
        "" => format!("{}{id}/collection.json", BUCKETS[0]),
        u if is_collection(u) && u.ends_with(&format!("/{id}/collection.json")) => u.to_string(),
        u => return Err(format!("its collection {} isn't in JAXA Earth's buckets", clip(u, 120))),
    };
    cut_note(cut, id, "dataProvider", get("dataProvider").trim(), 60);
    cut_note(cut, id, "title", title.trim(), 160);
    cut_note(cut, id, "temporalResolution", &step, MAX_STEP);
    let provider = match clip(get("dataProvider").trim(), 60) {
        p if p.is_empty() => "JAXA Earth API".to_string(),
        p => format!("JAXA Earth API ({p})"),
    };
    let title = clip(title.trim(), 160);
    let words = [get("keywords"), get("bands"), id, step.as_str()].join(" ");
    let themes = themes(&format!("{title} {words}"));
    let many = bands.len() > 1;
    Ok(bands
        .iter()
        .map(|band| Entry {
            id: format!("{id}/{band}"),
            name: if many { format!("{title} · {band}") } else { title.clone() },
            provider: provider.clone(),
            themes: themes.clone(),
            bbox,
            start: start.clone(),
            end: end.clone(),
            step: step_text(&step),
            resolution: km.map_or_else(|| "not given".to_string(), resolution_text),
            url: url.clone(),
            source: Source::Stac { collection: id.to_string(), asset: band.to_string() },
            // Worked out from the files when the cube is added (cube.open).
            range: None,
            cell_size: km.map(|k| k / KM_PER_DEGREE),
            max_cloud: None,
            license: "the collection's own (JAXA's terms for most datasets)".into(),
            about: format!("https://data.earth.jaxa.jp/en/datasets/#/id/{id}"),
            credit: format!("JAXA, {title}"),
            // Whole: its fields are bounded already, and a cut would lose words to search by.
            words: words.clone(),
        })
        .collect())
}

/// `[west, south, east, north]` in degrees, within the globe.
fn bbox(text: &str) -> Option<[f64; 4]> {
    let v: Vec<f64> = serde_json::from_str::<Vec<f64>>(text.trim()).ok()?;
    match v.as_slice() {
        [w, s, e, n]
            if (-180.0..=180.0).contains(w)
                && (-180.0..=180.0).contains(e)
                && w < e
                && (-90.0..=90.0).contains(s)
                && (-90.0..=90.0).contains(n)
                && s < n =>
        {
            Some([*w, *s, *e, *n])
        }
        _ => None,
    }
}

/// How often it has a date, as people read it.
fn step_text(step: &str) -> String {
    match step {
        "daily" => "1 day".into(),
        "8-day" => "8 days".into(),
        "half-monthly" => "half a month".into(),
        "monthly" => "1 month".into(),
        "yearly" => "1 year".into(),
        "" | "none" | "null" => "not given".into(),
        s => clip(s, MAX_STEP),
    }
}

/// A cell's size, as people read it: JAXA gives it in km, at the equator.
fn resolution_text(km: f64) -> String {
    if km < 1.0 {
        format!("about {} m", (km * 1000.0).round())
    } else if km < 10.0 {
        format!("about {km:.1} km")
    } else {
        format!("about {} km", km.round())
    }
}

/// The themes a dataset is filed under, from the words of its title, keywords, bands and id.
fn themes(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let has = |names: &[&str]| names.iter().any(|n| if n.contains(' ') { lower.contains(n) } else { words.contains(n) });
    let mut out = Vec::new();
    for (theme, names) in [
        ("vegetation", &["ndvi", "vegetation", "forest", "fnf", "evapotranspiration"][..]),
        ("land cover", &["land cover", "landcover", "lulc", "hrlulc", "lccs", "fnf"][..]),
        ("water", &["precipitation", "precip", "rainfall", "gsmap", "smc", "soil moisture", "evapotranspiration", "spi", "flood"][..]),
        ("climate", &["lst", "land surface temperature", "precipitation", "rainfall", "spi", "swr", "shortwave", "evapotranspiration"][..]),
        ("ocean", &["sst", "sea surface", "chla", "chlorophyll", "ssw"][..]),
        ("atmosphere", &["aerosol", "aod", "arot", "swr", "shortwave"][..]),
        ("elevation", &["dsm", "elevation", "aw3d30", "surface model"][..]),
    ] {
        if has(names) {
            out.push(theme.to_string());
        }
    }
    out
}

/// The most characters of a step not among the ones named in [`step_text`].
const MAX_STEP: usize = 40;
/// The most characters of a licence link's title.
const MAX_LICENCE_TITLE: usize = 120;

/// `s`, at most `n` characters: one cut ends in "…".
fn clip(s: &str, n: usize) -> String {
    if s.chars().nth(n).is_none() {
        return s.to_string();
    }
    s.chars().take(n.saturating_sub(1)).chain(['…']).collect()
}

/// Notes in `cut` that `what` of `dataset` is longer than `n` characters and is cut.
fn cut_note(cut: &mut Vec<String>, dataset: &str, what: &str, s: &str, n: usize) {
    let len = s.chars().count();
    if len > n {
        note(cut, format!("{}: its {what} ({len} characters) is cut to {n}", clip(dataset, 120)));
    }
}

/// Adds a note to `cut`: the first [`MAX_CUTS`] of them, then one note that there are more.
fn note(cut: &mut Vec<String>, text: String) {
    match cut.len() {
        k if k < MAX_CUTS => cut.push(text),
        MAX_CUTS => cut.push(format!("more of the catalog's text is cut or not read than these {MAX_CUTS} notes list")),
        _ => {}
    }
}

/// A collection's licence: the page its `license` link names, and that link's title, if any.
#[derive(Clone, Debug, PartialEq)]
pub struct Licence {
    pub url: String,
    pub title: Option<String>,
}

/// A collection's licence from its `collection.json`: its `rel: license` link (on the web), else
/// its `license` field when that names an SPDX licence.
pub fn licence_of(collection: &[u8]) -> Result<Licence, String> {
    let v: Value = serde_json::from_slice(collection).map_err(|_| "the collection isn't JSON")?;
    let link = v.get("links").and_then(Value::as_array).into_iter().flatten().find(|l| {
        l.get("rel").and_then(Value::as_str) == Some("license")
            && l.get("href").and_then(Value::as_str).and_then(cubes::web).is_some_and(|h| h.len() <= 500)
    });
    if let Some(l) = link {
        let title = l.get("title").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()).map(|t| {
            let len = t.chars().count();
            if len > MAX_LICENCE_TITLE {
                format!("{} (its title, {len} characters, is cut to {MAX_LICENCE_TITLE})", clip(t, MAX_LICENCE_TITLE))
            } else {
                t.to_string()
            }
        });
        return Ok(Licence { url: l.get("href").and_then(Value::as_str).unwrap_or_default().to_string(), title });
    }
    match v.get("license").and_then(Value::as_str).map(str::trim) {
        Some(id)
            if !id.is_empty()
                && id.len() <= 60
                && !["proprietary", "other", "various"].contains(&id.to_ascii_lowercase().as_str())
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+')) =>
        {
            Ok(Licence { url: format!("https://spdx.org/licenses/{id}.html"), title: Some(id.to_string()) })
        }
        _ => Err("the collection names no licence link".into()),
    }
}

/// The licences read, by collection: `None` while one is read.
fn licences() -> &'static Mutex<HashMap<String, Option<Result<Licence, String>>>> {
    static LICENCES: OnceLock<Mutex<HashMap<String, Option<Result<Licence, String>>>>> = OnceLock::new();
    LICENCES.get_or_init(Mutex::default)
}

/// A JAXA Earth collection's licence, if it has been read ([`fetch_licence`], [`licence_blocking`]).
pub fn licence(collection: &str) -> Option<Result<Licence, String>> {
    licences().lock().unwrap_or_else(|e| e.into_inner()).get(collection).cloned().flatten()
}

/// Reads a JAXA Earth collection's licence in the background, once: [`licence`] has it then.
pub fn fetch_licence(collection: &str) {
    if !is_collection(collection) {
        return;
    }
    {
        let mut all = licences().lock().unwrap_or_else(|e| e.into_inner());
        if all.contains_key(collection) {
            return;
        }
        all.insert(collection.to_string(), None);
    }
    let key = collection.to_string();
    #[cfg(not(target_arch = "wasm32"))]
    let asked = std::time::Instant::now();
    crate::cache::download_limited(
        collection,
        MAX_COLLECTION_BYTES,
        Box::new(move |r| {
            let l = r.and_then(|b| licence_of(&b));
            // A slow failure here waits as one in `licence_blocking` does.
            #[cfg(not(target_arch = "wasm32"))]
            note_read(&key, l.is_err(), asked.elapsed());
            licences().lock().unwrap_or_else(|e| e.into_inner()).insert(key, Some(l));
        }),
    );
}

/// A failed licence read that took at least this long (a server that stalls)...
#[cfg(not(target_arch = "wasm32"))]
const SLOW_READ: std::time::Duration = std::time::Duration::from_secs(5);
/// ...is asked for again only after this long, so each `cube.open` doesn't wait for it.
#[cfg(not(target_arch = "wasm32"))]
const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// When each collection's licence last failed to read, slowly.
#[cfg(not(target_arch = "wasm32"))]
fn slow_failures() -> &'static Mutex<HashMap<String, std::time::Instant>> {
    static SLOW: OnceLock<Mutex<HashMap<String, std::time::Instant>>> = OnceLock::new();
    SLOW.get_or_init(Mutex::default)
}

/// Whether a licence that failed slowly at `failed` still waits at `now` to be asked for again.
#[cfg(not(target_arch = "wasm32"))]
fn waits(failed: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    failed.is_some_and(|t| now.saturating_duration_since(t) < RETRY_AFTER)
}

/// A collection's licence, waiting for it (desktop): once read, kept for the run. One that
/// couldn't be read (here or by [`fetch_licence`]) is asked for again: at once after a quick
/// failure (such as a 404), and [`RETRY_AFTER`] after one that took [`SLOW_READ`] or more, its
/// error given until then.
#[cfg(not(target_arch = "wasm32"))]
pub fn licence_blocking(collection: &str) -> Result<Licence, String> {
    match licence(collection) {
        Some(Ok(l)) => return Ok(l),
        Some(Err(why)) => {
            let failed = slow_failures().lock().unwrap_or_else(|e| e.into_inner()).get(collection).copied();
            if waits(failed, std::time::Instant::now()) {
                let m = RETRY_AFTER.as_secs() / 60;
                return Err(format!("{why}; that read was slow, so it is asked for again {m} minutes after it"));
            }
        }
        None => {}
    }
    let asked = std::time::Instant::now();
    let l = crate::cache::download_blocking_limited(collection, MAX_COLLECTION_BYTES).and_then(|b| licence_of(&b));
    note_read(collection, l.is_err(), asked.elapsed());
    licences().lock().unwrap_or_else(|e| e.into_inner()).insert(collection.to_string(), Some(l.clone()));
    l
}

/// Notes how a licence read of `collection` ended: a failure after [`SLOW_READ`] or more starts
/// the wait before it is asked for again; any other end clears it.
#[cfg(not(target_arch = "wasm32"))]
fn note_read(collection: &str, failed: bool, took: std::time::Duration) {
    let mut slow = slow_failures().lock().unwrap_or_else(|e| e.into_inner());
    if failed && took >= SLOW_READ {
        slow.insert(collection.to_string(), std::time::Instant::now());
    } else {
        slow.remove(collection);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A copy of JAXA's catalog as read on 2026-11-08 (updated by JAXA on 2026-07-01).
    const FIXTURE: &str = include_str!("../tests/data/jaxa_catalog.v2.md");

    /// Every dataset of the catalog is a cube a band, or said with why it isn't one.
    #[test]
    fn the_catalog_reads_as_cubes_and_says_what_it_leaves_out() {
        let c = parse(FIXTURE).unwrap();
        assert_eq!(c.datasets, 119);
        // Today's catalog: nothing cut, every field read.
        assert!(c.cut.is_empty(), "{:?}", c.cut);
        // Each dataset once: listed as cubes, or left out.
        let mut ids: Vec<&str> = c.entries.iter().filter_map(|e| e.source_dataset()).collect();
        ids.dedup();
        assert_eq!(ids.len() + c.left_out.len(), c.datasets);
        // Two polar, 31 climatologies, 8 hourly series.
        let count = |w: &str| c.left_out.iter().filter(|l| l.why.contains(w)).count();
        assert_eq!((count("polar"), count("climatology"), count("hourly")), (2, 31, 8), "{:?}", c.left_out);
        assert_eq!(c.left_out.len(), 41);
        // 78 datasets, two of them with two bands (AW3D30's DSM and MSK).
        assert_eq!((ids.len(), c.entries.len()), (78, 80));
        let gsmap = c.entries.iter().find(|e| e.id == "JAXA.EORC_GSMaP_standard.Gauge.00Z-23Z.v6_monthly/PRECIP").unwrap();
        assert_eq!(
            gsmap.url,
            "https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.EORC_GSMaP_standard.Gauge.00Z-23Z.v6_monthly/collection.json"
        );
        assert_eq!(
            gsmap.source,
            Source::Stac { collection: "JAXA.EORC_GSMaP_standard.Gauge.00Z-23Z.v6_monthly".into(), asset: "PRECIP".into() }
        );
        assert_eq!((gsmap.start.as_str(), gsmap.end.as_deref(), gsmap.step.as_str()), ("2000-03-01", None, "1 month"));
        assert_eq!((gsmap.bbox, gsmap.range), ([-180.0, -60.0, 180.0, 60.0], None));
        assert!((gsmap.cell_size.unwrap() - 0.1).abs() < 1e-6, "{:?}", gsmap.cell_size);
        assert_eq!(gsmap.resolution, "about 11 km");
        assert!(gsmap.themes.contains(&"water".to_string()) && gsmap.themes.contains(&"climate".to_string()), "{:?}", gsmap.themes);
        assert!(
            gsmap.credit.starts_with("JAXA, ") && gsmap.provider == "JAXA Earth API (JAXA EORC)",
            "{} {}",
            gsmap.credit,
            gsmap.provider
        );
        // A collection in JAXA's second bucket is read there.
        let spi = c.entries.iter().find(|e| e.id.starts_with("JAXA.EORC_GSMaP_SPI.climate.gnrt6_monthly/")).unwrap();
        assert!(spi.url.starts_with("https://s3.ap-northeast-1.wasabisys.com/je-pds2/"), "{}", spi.url);
        // Two bands: two cubes, named apart.
        let dsm: Vec<&Entry> = c.entries.iter().filter(|e| e.id.starts_with("JAXA.EORC_ALOS.PRISM_AW3D30.v4.1_global/")).collect();
        assert_eq!(dsm.len(), 2);
        assert!(dsm[0].name.ends_with("· DSM") && dsm[0].themes.contains(&"elevation".to_string()), "{} {:?}", dsm[0].name, dsm[0].themes);
        // Every cube can be found by theme, and each id once.
        assert!(
            c.entries.iter().all(|e| !e.themes.is_empty()),
            "{:?}",
            c.entries.iter().filter(|e| e.themes.is_empty()).map(|e| &e.id).collect::<Vec<_>>()
        );
        let mut all: Vec<&str> = c.entries.iter().map(|e| e.id.as_str()).collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), c.entries.len());
        // By its keywords and bands too: GCOM-C's NDVI.
        let sgli = cubes::select(&c.entries, Some("vegetation"), Some([-50.0, -10.0, -45.0, -5.0]), Some("ndvi sgli")).unwrap();
        assert!(!sgli.is_empty() && sgli.iter().all(|e| e.words.contains("SGLI")), "{sgli:?}");
    }

    /// What isn't JAXA's, or can't be read, is left out with why; a catalog that isn't one is
    /// refused.
    #[test]
    fn a_wrong_dataset_is_left_out_and_said() {
        let one = |fields: &str| parse(&format!("# Dataset List\n\n## A dataset\n{fields}\n")).unwrap();
        let good = "* id: JAXA.Test_daily\n* title: Test\n* bands: B1\n* bbox: [-10,-10,10,10]\n* epsg: 4326\n* spatialResolution: 5.566\n* startDate: 2020-01-01T00:00:00Z\n* endDate: present\n* temporalResolution: daily\n* normal: false";
        let c = one(good);
        assert_eq!((c.entries.len(), c.left_out.len()), (1, 0));
        assert_eq!(c.entries[0].url, format!("{}JAXA.Test_daily/collection.json", BUCKETS[0]));
        for (from, to, why) in [
            ("* epsg: 4326", "* epsg: 3031", "polar"),
            ("* normal: false", "* normal: true", "climatology"),
            ("* temporalResolution: daily", "* temporalResolution: hourly", "hourly"),
            ("* id: JAXA.Test_daily", "* id: ../etc", "no id"),
            ("* bands: B1", "* bands: B 1", "bands"),
            ("* bbox: [-10,-10,10,10]", "* bbox: [10,-10,-10,10]", "bbox"),
            ("* startDate: 2020-01-01T00:00:00Z", "* startDate: soon", "startDate"),
            ("* endDate: present", "* endDate: 2019-01-01T00:00:00Z", "before"),
        ] {
            let c = one(&good.replace(from, to));
            assert!(c.entries.is_empty() && c.left_out.len() == 1 && c.left_out[0].why.contains(why), "{to}: {:?}", c.left_out);
        }
        // A collection elsewhere than JAXA's buckets, or another dataset's, is never read.
        for u in ["https://example.com/JAXA.Test_daily/collection.json", &format!("{}Other/collection.json", BUCKETS[1])] {
            let c = one(&format!("{good}\n* collection: {u}"));
            assert!(c.entries.is_empty() && c.left_out[0].why.contains("buckets"), "{u}");
        }
        assert!(parse("# Something else\n## x\n* id: y").is_err());
        assert!(parse(&" ".repeat(MAX_BYTES + 1)).unwrap_err().contains("larger"));
        // Listed twice: one dataset.
        let twice = parse(&format!("# Dataset List\n## A\n{good}\n## B\n{good}\n")).unwrap();
        assert_eq!((twice.datasets, twice.entries.len(), twice.left_out.len()), (2, 1, 1));
        assert!(twice.left_out[0].id == "JAXA.Test_daily" && twice.left_out[0].why.contains("listed again"), "{:?}", twice.left_out);
        // A first listing that can't be a cube doesn't stand for a later one that can.
        let polar = good.replace("* epsg: 4326", "* epsg: 3031");
        let later = parse(&format!("# Dataset List\n## A\n{polar}\n## B\n{good}\n")).unwrap();
        assert_eq!((later.entries.len(), later.left_out.len()), (1, 1));
        assert!(later.left_out[0].why.contains("polar"), "{:?}", later.left_out);
        // A listing again keeps its title, cut and said, as any left out does.
        let long = good.replace("* title: Test", &format!("* title: {}", "T".repeat(210)));
        let again = parse(&format!(
            "# Dataset List
## A
{good}
## B
{long}
"
        ))
        .unwrap();
        assert_eq!(again.left_out[0].title.chars().count(), 200);
        assert_eq!(again.cut, ["JAXA.Test_daily: its title (210 characters) is cut to 200"]);
        assert!(c.left_out.is_empty(), "{:?}", c.left_out);
        assert!(c.cut.is_empty() && twice.cut.is_empty());
        // Text too long is cut, said where and by how much, and its name ends in "…"; its keywords
        // are all searched still.
        let long = format!(
            "{}\n* keywords: {} lastword",
            good.replace("* title: Test", &format!("* title: {}", "T".repeat(170))),
            "w ".repeat(400)
        );
        let c = one(&long);
        assert_eq!(c.entries[0].name.chars().count(), 160);
        assert!(c.entries[0].name.ends_with('…'));
        assert!(c.entries[0].has_words(&["lastword".to_string()]));
        assert_eq!(c.cut, ["JAXA.Test_daily: its title (170 characters) is cut to 160"]);
        let c = one(&format!("{good}\n* description: {}", "d".repeat(MAX_FIELD + 5)));
        assert_eq!(c.cut, [format!("A dataset: its description ({} characters) is cut to {MAX_FIELD}", MAX_FIELD + 5)]);
        let c = one(&good.replace("* temporalResolution: daily", &format!("* temporalResolution: {}", "d".repeat(45))));
        assert_eq!(c.entries[0].step.chars().count(), MAX_STEP);
        assert_eq!(c.cut, [format!("JAXA.Test_daily: its temporalResolution (45 characters) is cut to {MAX_STEP}")]);
        // A dataset left out keeps its title, cut and said, as a listed one does.
        let c = one(&good.replace("* title: Test", &format!("* title: {}", "T".repeat(210))).replace("* epsg: 4326", "* epsg: 3031"));
        assert_eq!(c.left_out[0].title.chars().count(), 200);
        assert_eq!(c.cut, ["JAXA.Test_daily: its title (210 characters) is cut to 200"]);
        // Past the notes listed one by one, one more says there are more.
        let mut cut = Vec::new();
        for _ in 0..MAX_CUTS + 3 {
            cut_note(&mut cut, "x", "title", "too long", 3);
        }
        assert_eq!(cut.len(), MAX_CUTS + 1);
        assert!(cut[MAX_CUTS].contains("more of the catalog's text is cut or not read"), "{}", cut[MAX_CUTS]);
        cut_note(&mut cut, "x", "title", "short", 10);
        assert_eq!(cut.len(), MAX_CUTS + 1);
        // Fields that aren't read are said: a resolution that isn't kilometres, a field given
        // again, a field whose name isn't letters and digits. A clean dataset gives none (above).
        let c = one(&good.replace("* spatialResolution: 5.566", "* spatialResolution: ten"));
        assert_eq!(c.entries[0].resolution, "not given");
        assert_eq!(
            c.cut,
            [
                "JAXA.Test_daily: its spatialResolution \"ten\" isn't read (kilometres, above 0 and below 1000), so its resolution is not given"
            ]
        );
        let c = one(&good.replace("* bands: B1", "* bands: B1, B2, B1"));
        assert_eq!(c.entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["JAXA.Test_daily/B1", "JAXA.Test_daily/B2"]);
        assert_eq!(c.cut, ["JAXA.Test_daily: its band B1 is listed twice; it is one cube"]);
        let c = one(&format!("{good}\n* title: Again\n* data provider: X"));
        assert_eq!(c.entries[0].name, "Test", "the first title is read");
        assert_eq!(
            c.cut,
            [
                "A dataset: its title is given again; the first is read",
                "A dataset: its field \"data provider\" isn't read (a field's name is 1 to 40 letters and digits)",
            ]
        );
    }

    /// A licence that failed slowly waits a while before it is asked for again; none waits else.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_slow_licence_failure_waits_before_it_is_asked_again() {
        let t = std::time::Instant::now();
        assert!(!waits(None, t));
        assert!(waits(Some(t), t) && waits(Some(t), t + RETRY_AFTER - std::time::Duration::from_secs(1)));
        assert!(!waits(Some(t), t + RETRY_AFTER));
        // Either read (the pane's in the background, or cube.open's) starts the wait when it fails
        // slowly, and a quick failure or a read clears it.
        let key = "test://slow-licence";
        let at = || slow_failures().lock().unwrap().get(key).copied();
        note_read(key, true, SLOW_READ);
        assert!(waits(at(), std::time::Instant::now()));
        note_read(key, true, std::time::Duration::from_millis(10));
        assert!(at().is_none());
        note_read(key, true, SLOW_READ);
        note_read(key, false, SLOW_READ);
        assert!(at().is_none());
    }

    /// A collection's licence: its license link, else an SPDX id; JAXA's collections are told
    /// from other addresses.
    #[test]
    fn a_collections_licence_is_its_link() {
        let l = licence_of(br#"{"license": "proprietary", "links": [{"rel": "self", "href": "x"}, {"rel": "license", "href": "https://earth.jaxa.jp/en/data/policy/", "title": "JAXA terms"}]}"#).unwrap();
        assert_eq!((l.url.as_str(), l.title.as_deref()), ("https://earth.jaxa.jp/en/data/policy/", Some("JAXA terms")));
        assert_eq!(licence_of(br#"{"license": "CC-BY-4.0", "links": []}"#).unwrap().url, "https://spdx.org/licenses/CC-BY-4.0.html");
        // A title too long is cut, and says so.
        let long = format!(
            r#"{{"links": [{{"rel": "license", "href": "https://earth.jaxa.jp/en/data/policy/", "title": "{}"}}]}}"#,
            "t".repeat(130)
        );
        let title = licence_of(long.as_bytes()).unwrap().title.unwrap();
        assert!(title.ends_with("… (its title, 130 characters, is cut to 120)"), "{title}");
        for bad in
            [&br#"{"license": "proprietary"}"#[..], br#"{"links": [{"rel": "license", "href": "javascript:alert(1)"}]}"#, b"not json"]
        {
            assert!(licence_of(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        assert!(is_collection("https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JAXA.X_daily/collection.json"));
        assert!(!is_collection("https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/../x/collection.json"));
        assert!(!is_collection("https://example.com/je-pds/cog/v1/JAXA.X/collection.json"));
        assert_eq!(
            (resolution_text(0.309), resolution_text(5.566), resolution_text(22.264)),
            ("about 309 m".into(), "about 5.6 km".into(), "about 22 km".into())
        );
    }
}
