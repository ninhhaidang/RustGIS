//! A catalog of data cubes (opengeos/RustGIS#683): entries ready to open, each a STAC collection's
//! band or a Zarr store's variable, with its provider, themes, area, period, step, resolution and
//! licence. Finding a cube then needs no reading of a provider's catalog for the collection, band
//! and range. The list is read and checked here; opening an entry is the engine's (`cube.open`,
//! through `stac.addCube` or `layer.addZarr`).
//!
//! The list is built into the app, and kept up to date with the app's other assets ([`HOSTED`]),
//! which is used once read.
//!
//! An entry, as the list holds it:
//!
//! ```json
//! {"id": "bdc-mod13q1-ndvi", "name": "MODIS NDVI, 16-day composites (MOD13Q1 v6.1)",
//!  "provider": "Brazil Data Cube (INPE)", "theme": ["vegetation"], "bbox": [-100, -60, -21.3, 20],
//!  "start": "2000-02-18", "end": null, "step": "16 days", "resolution": "232 m",
//!  "format": "stac", "url": "https://data.inpe.br/bdc/stac/v1/", "collection": "mod13q1-6.1",
//!  "asset": "NDVI", "range": [0, 0.9], "cellSize": 232, "maxCloud": null,
//!  "license": "NASA LP DAAC: no restrictions on use", "about": "https://data.inpe.br/bdc/"}
//! ```
//!
//! A Zarr entry has `"format": "zarr"` and `variable` in place of `collection` and `asset`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Value, json};

/// The list built into the app.
const BUILT_IN: &str = include_str!("cubes.json");
/// The themes an entry is filed under: few, so they filter well and translate easily.
pub const THEMES: &[&str] = &["vegetation", "land cover", "water", "climate", "ocean", "atmosphere", "elevation"];
/// The most a list's text may be.
pub const MAX_BYTES: usize = 1 << 20;
/// The most entries a list may have.
const MAX_ENTRIES: usize = 500;

/// What an entry opens.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A STAC API's or a static catalog's collection, and the asset (a band) read from its items.
    Stac { collection: String, asset: String },
    /// A Zarr store's variable.
    Zarr { variable: String },
}

/// A cube ready to open.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub themes: Vec<String>,
    /// Where it has data: [west, south, east, north] in degrees.
    pub bbox: [f64; 4],
    /// Its first date, and its last (none while it is kept up to date), as `YYYY-MM-DD`.
    pub start: String,
    pub end: Option<String>,
    /// How often it has a date, and its cells' size, as people read them ("16 days", "232 m").
    pub step: String,
    pub resolution: String,
    /// The STAC API, the static collection's `collection.json`, or the Zarr store.
    pub url: String,
    pub source: Source,
    /// The values shown from the low colour to the high one, in the band's units (STAC).
    pub range: Option<[f64; 2]>,
    /// The cell size in the files' units, which a trend over the collection reads at (STAC).
    pub cell_size: Option<f64>,
    /// The cloud cover cap a collection of scenes is searched with (STAC).
    pub max_cloud: Option<f64>,
    pub license: String,
    /// The provider's page about it.
    pub about: String,
}

impl Entry {
    /// Whether its area meets `area` ([west, south, east, north]; west past east crosses the
    /// antimeridian).
    pub fn overlaps(&self, area: [f64; 4]) -> bool {
        let [w, s, e, n] = self.bbox;
        let [aw, a_s, ae, an] = area;
        if a_s > n || an < s {
            return false;
        }
        let meets = |lo: f64, hi: f64| lo <= e && hi >= w;
        if aw <= ae { meets(aw, ae) } else { meets(aw, 180.0) || meets(-180.0, ae) }
    }

    /// Whether every word is in its id, name or provider (letter case aside).
    pub fn has_words(&self, words: &[String]) -> bool {
        let text = format!("{} {} {}", self.id, self.name, self.provider).to_lowercase();
        words.iter().all(|w| text.contains(w.as_str()))
    }

    /// Its last full calendar year as of `this_year`: the year before it while the entry is kept
    /// up to date (the current year isn't over, and the newest dates come in late), else its end's
    /// year; never before its start's.
    pub fn last_year(&self, this_year: i32) -> i32 {
        let year = |d: &str| d.get(..4).and_then(|y| y.parse::<i32>().ok());
        let first = year(&self.start).unwrap_or(this_year);
        let last = self.end.as_deref().and_then(year).unwrap_or(this_year - 1);
        last.max(first)
    }

    /// The entry as the list holds it.
    pub fn json(&self) -> Value {
        let mut v = json!({
            "id": self.id, "name": self.name, "provider": self.provider, "theme": self.themes,
            "bbox": self.bbox, "start": self.start, "end": self.end, "step": self.step,
            "resolution": self.resolution, "url": self.url, "license": self.license, "about": self.about,
        });
        match &self.source {
            Source::Stac { collection, asset } => {
                v["format"] = json!("stac");
                v["collection"] = json!(collection);
                v["asset"] = json!(asset);
            }
            Source::Zarr { variable } => {
                v["format"] = json!("zarr");
                v["variable"] = json!(variable);
            }
        }
        for (k, x) in [
            ("range", self.range.map(|r| json!(r))),
            ("cellSize", self.cell_size.map(|c| json!(c))),
            ("maxCloud", self.max_cloud.map(|c| json!(c))),
        ] {
            if let Some(x) = x {
                v[k] = x;
            }
        }
        v
    }
}

/// The list built into the app.
pub fn built_in() -> &'static [Entry] {
    built_in_arc().as_slice()
}

fn built_in_arc() -> &'static Arc<Vec<Entry>> {
    static LIST: OnceLock<Arc<Vec<Entry>>> = OnceLock::new();
    // Checked by a test, so never empty in a build.
    LIST.get_or_init(|| Arc::new(parse(BUILT_IN).unwrap_or_default()))
}

/// The list in use: the hosted one ([`fetch_hosted`]) once it has been read, else the built-in
/// one.
pub fn list() -> Arc<Vec<Entry>> {
    hosted().lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(|| built_in_arc().clone())
}

/// The entry with this id, in the list in use.
pub fn find(id: &str) -> Result<Entry, String> {
    let id = id.trim();
    list()
        .iter()
        .find(|e| e.id == id)
        .cloned()
        .ok_or_else(|| format!("no data cube '{}' in the catalog (cube.catalog lists them)", clip(id, 80)))
}

/// Where the list is kept up to date between releases (opengeos/rustgis-assets), so a cube is
/// added, or an address fixed, without a new version of the app.
pub const HOSTED: &str = "https://assets.rustgis.app/data/data-cubes.json";

fn hosted() -> &'static Mutex<Option<Arc<Vec<Entry>>>> {
    static HOSTED_LIST: OnceLock<Mutex<Option<Arc<Vec<Entry>>>>> = OnceLock::new();
    HOSTED_LIST.get_or_init(Mutex::default)
}

/// Reads the hosted list once a run of the app, in the background (a thread natively, the
/// browser's fetch on the web), the first time the catalog is shown. Until it is read, and when
/// it can't be (offline, or a list that doesn't read whole: [`take_hosted`]), the built-in list
/// is used.
pub fn fetch_hosted() {
    static ASKED: AtomicBool = AtomicBool::new(false);
    if ASKED.swap(true, Ordering::Relaxed) {
        return;
    }
    crate::cache::download_limited(
        HOSTED,
        MAX_BYTES,
        Box::new(|r| {
            if let Err(e) = r.and_then(|bytes| take_hosted(&bytes)) {
                log::info!("the hosted data cube list wasn't read, so the built-in one is used: {e}");
            }
        }),
    );
}

/// Takes a hosted list in place of the built-in one: only one that reads whole (every entry
/// checked, as [`parse`] does) and lists some cube; otherwise the list in use stays, and why is
/// said. Its number of entries.
pub fn take_hosted(bytes: &[u8]) -> Result<usize, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("the data cube list is larger than {} KB", MAX_BYTES >> 10));
    }
    let list = parse(std::str::from_utf8(bytes).map_err(|_| "the data cube list isn't text")?)?;
    if list.is_empty() {
        return Err("the data cube list has no entries".into());
    }
    let n = list.len();
    *hosted().lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(list));
    Ok(n)
}

/// The entries of `list` filed under `theme`, whose area meets `area`, with all the words of
/// `query`; each optional.
pub fn select<'a>(list: &'a [Entry], theme: Option<&str>, area: Option<[f64; 4]>, query: Option<&str>) -> Result<Vec<&'a Entry>, String> {
    let theme = match theme.map(str::trim).filter(|t| !t.is_empty()) {
        None => None,
        Some(t) => {
            let t = t.to_lowercase();
            Some(THEMES.iter().find(|k| **k == t).ok_or_else(|| format!("theme is one of {}", THEMES.join(", ")))?)
        }
    };
    let words: Vec<String> = query.unwrap_or("").split_whitespace().take(20).map(str::to_lowercase).collect();
    Ok(list
        .iter()
        .filter(|e| theme.is_none_or(|t| e.themes.iter().any(|k| k == t)))
        .filter(|e| area.is_none_or(|a| e.overlaps(a)))
        .filter(|e| e.has_words(&words))
        .collect())
}

/// A list's entries, each checked: every field its format needs, of the right kind, its area
/// within the globe, its dates in order, its addresses on the web, its id once in the list. A list
/// with one entry wrong is refused whole, naming it.
pub fn parse(text: &str) -> Result<Vec<Entry>, String> {
    if text.len() > MAX_BYTES {
        return Err(format!("the data cube list is larger than {} KB", MAX_BYTES >> 10));
    }
    let v: Value = serde_json::from_str(text).map_err(|e| format!("the data cube list isn't JSON: {e}"))?;
    let list = v.as_array().ok_or("the data cube list is a JSON array of entries")?;
    if list.len() > MAX_ENTRIES {
        return Err(format!("the data cube list has more than {MAX_ENTRIES} entries"));
    }
    let mut out: Vec<Entry> = Vec::with_capacity(list.len());
    for (i, item) in list.iter().enumerate() {
        let name = item.get("id").and_then(Value::as_str).map_or_else(|| format!("entry {}", i + 1), |id| format!("'{}'", clip(id, 80)));
        let e = entry(item).map_err(|why| format!("data cube {name}: {why}"))?;
        if out.iter().any(|o| o.id == e.id) {
            return Err(format!("data cube {name} is listed twice"));
        }
        out.push(e);
    }
    Ok(out)
}

/// One entry, checked.
fn entry(v: &Value) -> Result<Entry, String> {
    if !v.is_object() {
        return Err("an entry is a JSON object".into());
    }
    let text = |k: &str, max: usize| -> Result<String, String> {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty() && t.chars().count() <= max)
            .map(str::to_string)
            .ok_or_else(|| format!("{k} is text of 1 to {max} characters"))
    };
    let id = text("id", 64)?;
    if !id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        return Err("id is lower-case letters, digits and hyphens".into());
    }
    let themes: Vec<String> = match v.get("theme").and_then(Value::as_array) {
        Some(a) if !a.is_empty() && a.len() <= THEMES.len() => a
            .iter()
            .map(|t| t.as_str().filter(|t| THEMES.contains(t)).map(str::to_string))
            .collect::<Option<_>>()
            .ok_or_else(|| format!("theme is a list of {}", THEMES.join(", ")))?,
        _ => return Err(format!("theme is a list of {}", THEMES.join(", "))),
    };
    let bbox = match v.get("bbox").and_then(Value::as_array).map(|a| a.iter().map(Value::as_f64).collect::<Option<Vec<_>>>()) {
        Some(Some(b)) => match b.as_slice() {
            [w, s, e, n]
                if (-180.0..=180.0).contains(w)
                    && (-180.0..=180.0).contains(e)
                    && w < e
                    && (-90.0..=90.0).contains(s)
                    && (-90.0..=90.0).contains(n)
                    && s < n =>
            {
                [*w, *s, *e, *n]
            }
            _ => return Err("bbox is [west, south, east, north] in degrees, west below east and south below north".into()),
        },
        _ => return Err("bbox is [west, south, east, north] in degrees".into()),
    };
    let start = date(v.get("start")).ok_or("start is a date, YYYY-MM-DD")?;
    let end = match v.get("end") {
        None | Some(Value::Null) => None,
        d => Some(date(d).ok_or("end is a date, YYYY-MM-DD, or null while the cube is kept up to date")?),
    };
    if end.as_ref().is_some_and(|e| *e < start) {
        return Err("end is before start".into());
    }
    let url = web(&text("url", 2000)?).ok_or("url is a web address (https or http)")?;
    let source = match text("format", 10)?.as_str() {
        "stac" => Source::Stac { collection: text("collection", 200)?, asset: text("asset", 200)? },
        "zarr" => Source::Zarr { variable: text("variable", 200)? },
        f => return Err(format!("format is stac or zarr, not {}", clip(f, 20))),
    };
    let stac = matches!(source, Source::Stac { .. });
    let number = |k: &str| v.get(k).filter(|x| !x.is_null());
    let range = match number("range") {
        None => None,
        Some(r) => match r.as_array().map(|a| a.iter().map(|x| x.as_f64().filter(|x| x.is_finite())).collect::<Option<Vec<_>>>()) {
            Some(Some(r)) if stac && r.len() == 2 && r[0] < r[1] => Some([r[0], r[1]]),
            _ => return Err("range is [low, high], low below high, for a STAC entry".into()),
        },
    };
    let cell_size = match number("cellSize") {
        None => None,
        Some(c) => Some(c.as_f64().filter(|c| stac && c.is_finite() && *c > 0.0).ok_or("cellSize is a size above 0, for a STAC entry")?),
    };
    let max_cloud = match number("maxCloud") {
        None => None,
        Some(c) => Some(c.as_f64().filter(|c| stac && (0.0..=100.0).contains(c)).ok_or("maxCloud is a percent, for a STAC entry")?),
    };
    Ok(Entry {
        id,
        name: text("name", 200)?,
        provider: text("provider", 100)?,
        themes,
        bbox,
        start,
        end,
        step: text("step", 40)?,
        resolution: text("resolution", 40)?,
        url,
        source,
        range,
        cell_size,
        max_cloud,
        license: text("license", 200)?,
        about: web(&text("about", 2000)?).ok_or("about is a web address (https or http)")?,
    })
}

/// A date written `YYYY-MM-DD`, and a real day.
fn date(v: Option<&Value>) -> Option<String> {
    let d = v?.as_str()?.trim();
    let shaped = d.len() == 10 && d.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() });
    (shaped && rustgis_data::time::parse_text(d).is_some_and(|ms| rustgis_data::time::format(ms, "YYYY-MM-DD") == d)).then(|| d.to_string())
}

/// A web address (https or http), as given.
fn web(u: &str) -> Option<String> {
    let lower = u.to_ascii_lowercase();
    ((lower.starts_with("https://") || lower.starts_with("http://"))
        && u.len() > lower.find("://").map_or(0, |i| i + 3)
        && !u.chars().any(char::is_whitespace))
    .then(|| u.to_string())
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The built-in list reads whole, and every entry has what its format needs.
    #[test]
    fn the_built_in_list_is_sound() {
        let list = parse(BUILT_IN).unwrap();
        assert_eq!(built_in().len(), list.len());
        assert!(list.len() >= 10, "{}", list.len());
        for e in &list {
            match &e.source {
                Source::Stac { .. } => {
                    assert!(e.range.is_some() && e.cell_size.is_some(), "{}: a STAC entry gives its range and cell size", e.id)
                }
                Source::Zarr { .. } => assert!(e.range.is_none() && e.max_cloud.is_none(), "{}", e.id),
            }
            // Read back as the list holds it, it is the same entry.
            assert_eq!(entry(&e.json()).unwrap(), *e);
        }
        assert!(list.iter().any(|e| matches!(e.source, Source::Zarr { .. })));
        assert_eq!(find(" bdc-mod13q1-ndvi ").unwrap().cell_size, Some(232.0));
        assert!(find("nothing").is_err());
    }

    /// The catalog filters by theme, area (across the antimeridian too) and words.
    #[test]
    fn entries_are_selected_by_theme_area_and_words() {
        let list = built_in();
        let ids = |v: Vec<&Entry>| v.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
        let veg = ids(select(list, Some("Vegetation"), None, None).unwrap());
        assert!(veg.contains(&"bdc-mod13q1-ndvi".to_string()) && !veg.iter().any(|i| i.starts_with("era5")), "{veg:?}");
        assert!(select(list, Some("forests"), None, None).is_err());
        // Over Piauí: the Brazil Data Cube's, Earth Search's and ERA5's; over Europe, not the BDC's.
        let piaui = ids(select(list, None, Some([-46.0, -11.0, -40.0, -2.5]), None).unwrap());
        assert!(piaui.contains(&"bdc-s2-16d-ndvi".to_string()) && piaui.contains(&"era5-2m-temperature".to_string()), "{piaui:?}");
        let europe = ids(select(list, None, Some([5.0, 45.0, 15.0, 50.0]), None).unwrap());
        assert!(!europe.iter().any(|i| i.starts_with("bdc")) && europe.contains(&"earth-search-s2-nir".to_string()), "{europe:?}");
        // Water over Lake Victoria: Digital Earth Africa's, not Australia's.
        let victoria = ids(select(list, Some("water"), Some([32.6, -0.9, 32.8, -0.7]), Some("water observations")).unwrap());
        assert_eq!(victoria, ["deafrica-water-observations-annual"]);
        // A box across the antimeridian meets an entry on either side of it.
        let e = list.iter().find(|e| e.id == "bdc-mod13q1-ndvi").unwrap();
        assert!(e.overlaps([170.0, -10.0, -90.0, 0.0]) && !e.overlaps([170.0, -10.0, -150.0, 0.0]));
        // Words: all of them, in the name, provider or id, letter case aside.
        assert_eq!(ids(select(list, None, None, Some("modis evi")).unwrap()), ["bdc-mod13q1-evi"]);
        assert_eq!(select(list, Some("climate"), None, Some("precipitation")).unwrap().len(), 1);
        assert!(select(list, None, None, Some("brazil cube sentinel")).unwrap().iter().all(|e| e.provider.contains("Brazil")));
    }

    /// A list with an entry wrong is refused whole, saying which entry and why.
    #[test]
    fn a_wrong_entry_is_refused_by_name() {
        let good = built_in()[0].json();
        let with = |k: &str, v: Value| {
            let mut e = good.clone();
            if v.is_null() && k != "end" {
                e.as_object_mut().unwrap().remove(k);
            } else {
                e[k] = v;
            }
            format!("[{e}]")
        };
        for (k, v, why) in [
            ("id", json!("Has Spaces"), "id is"),
            ("theme", json!(["forests"]), "theme is"),
            ("theme", json!([]), "theme is"),
            ("bbox", json!([10, 0, 5, 1]), "bbox is"),
            ("bbox", json!([-200, 0, 5, 1]), "bbox is"),
            ("start", json!("2024-02-30"), "start is"),
            ("start", json!("18/02/2000"), "start is"),
            ("end", json!("1999-01-01"), "end is before start"),
            ("url", json!("s3://bucket/stac"), "url is"),
            ("url", json!("file:///etc"), "url is"),
            ("format", json!("netcdf"), "format is"),
            ("asset", Value::Null, "asset is"),
            ("range", json!([1, 0]), "range is"),
            ("cellSize", json!(-1), "cellSize is"),
            ("maxCloud", json!(120), "maxCloud is"),
            ("about", json!("ftp://x"), "about is"),
            ("name", Value::Null, "name is"),
        ] {
            let e = parse(&with(k, v)).unwrap_err();
            // Named by its id as given (a wrong id too).
            assert!((k == "id" || e.contains("'bdc-mod13q1-ndvi'")) && e.contains(why), "{k}: {e}");
        }
        // A Zarr entry needs its variable, and takes no band range.
        let mut z = built_in().iter().find(|e| matches!(e.source, Source::Zarr { .. })).unwrap().json();
        z["range"] = json!([0, 1]);
        assert!(parse(&format!("[{z}]")).unwrap_err().contains("range is"));
        z.as_object_mut().unwrap().remove("range");
        z.as_object_mut().unwrap().remove("variable");
        assert!(parse(&format!("[{z}]")).unwrap_err().contains("variable is"));
        // The same id twice, a list that isn't one, and one too large.
        assert!(parse(&format!("[{good}, {good}]")).unwrap_err().contains("twice"));
        assert!(parse("{}").is_err() && parse("not json").is_err());
        assert!(parse(&format!("[{}]", " ".repeat(MAX_BYTES))).unwrap_err().contains("larger"));
    }

    /// A hosted list is taken only when it reads whole and lists some cube: a malformed, empty or
    /// oversized one leaves the list in use as it was.
    #[test]
    fn a_hosted_list_is_taken_only_whole() {
        let before = list().len();
        for bad in [&b"not json"[..], b"[]", b"{}", b"[{\"id\": \"x\"}]", &[0xff, 0xfe][..]] {
            assert!(take_hosted(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        assert!(take_hosted(&vec![b' '; MAX_BYTES + 1]).unwrap_err().contains("larger"));
        assert_eq!(list().len(), before);
        // A sound one is taken (the built-in list's own text, so other tests see the same entries).
        assert_eq!(take_hosted(BUILT_IN.as_bytes()).unwrap(), built_in().len());
        assert_eq!(list().len(), built_in().len());
        assert!(HOSTED.starts_with("https://assets.rustgis.app/"));
    }

    /// The last full year: the one before this while the cube runs on, its end's once it ended,
    /// never before its start.
    #[test]
    fn an_entrys_last_full_year() {
        let mut e = built_in()[0].clone();
        assert_eq!(e.last_year(2026), 2025);
        e.end = Some("2022-06-30".into());
        assert_eq!(e.last_year(2026), 2022);
        e.end = None;
        e.start = "2026-03-01".into();
        assert_eq!(e.last_year(2026), 2026, "a cube that began this year: this year");
    }
}
