//! STAC: an API's collections, a search for items (with their footprints as a layer), and adding
//! an item's asset by its web address.

use serde_json::{Value, json};

use super::{b, f, ps};
#[cfg(not(target_arch = "wasm32"))]
use crate::render::rustgis_tiles::s3;
use crate::render::rustgis_tiles::stac::{self, Item, Search};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("stac.collections", "List STAC Collections", "Home › Add › Web Data › STAC Search", collections).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network))
            .view()
            .params(r#"{"url"?: "a STAC API (default: Earth Search, https://earth-search.aws.element84.com/v1; Planetary Computer: https://planetarycomputer.microsoft.com/api/stac/v1), or a static catalog's catalog.json or collection.json (its child catalogs and collections are listed)", "connection"?: "as service.add: the saved web-service connection whose login the request uses"}"#),
        C::new("stac.search", "STAC Search", "Home › Add › Web Data › STAC Search", search).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"url"?: "a STAC API (default: Earth Search), or a static catalog's catalog.json or collection.json (walked from there, at most 400 documents)", "collections"?: id | [ids] (APIs), "bbox"?: [west, south, east, north] | "view" (the map's extent), "datetime"?: "2024-06-01" | "2024-06-01/2024-06-30", "limit"?: 1–200 (default 10), "maxCloud"?: percent, "sortby"?: "eo:cloud_cover" | "-datetime" (APIs that sort), "phase"?: "pre" | "post" (static catalogs of events), "properties"?: bool (every property of each item, not only its date and cloud cover; default false), "footprints"?: bool (adds the items' footprints as a layer, default true), "connection"?: "as service.add: the saved web-service connection whose login the request uses"}"#,
        ),
        C::new("stac.addFootprints", "Add STAC Footprints", "Home › Add › Web Data › STAC Search", add_footprints_cmd).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"footprints": GeoJSON FeatureCollection (items' footprints, as STAC Search finds them), "name"?: text}"#),
        C::new("stac.addCube", "Add STAC Data Cube", "Home › Add › Web Data › STAC Search", add_cube).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"url"?: a STAC API (default: Earth Search), or the collection.json of a static collection laid out by date (JAXA Earth's), "collection": id, "asset": key (a band, such as NDVI), "bbox"?: [west, south, east, north] | "view" (default: the map's extent), "start", "end": dates, "maxCloud"?: percent, "range"?: [low, high] (the values shown from black to white, in the asset's own units after its raster:bands scale and offset, the same for every date and tile; default: each file's 2nd–98th percentile), "name"?: text, "connection"?: "as service.add: the saved web-service connection whose login the request uses"} → a tile layer per tile of the grid the items cover, each a file per date following the time slider, which turns on over their dates when it is off (desktop)"#,
        ),
        C::new("stac.addAsset", "Add STAC Asset", "Home › Add › Web Data › STAC Search", add_asset).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"item": a STAC item (as stac.search returns it) | "url": an item's address (Planetary Computer assets are signed), "asset"?: key (default: visual, else the first GeoTIFF, else the first data asset), "name"?: text, "connection"?: "as service.add"}"#,
        ),
    ]
}

fn request_api(s: &mut Session, p: &Value) -> Result<String, String> {
    let bound = crate::service_logins::request_params(s, p)?;
    let api = stac::api(ps(bound.as_ref(), "url"))?;
    crate::service_logins::request(s, ps(bound.as_ref(), "connection"), &api)
}

/// The search a command describes (`view` is the map's current extent).
pub fn search_params(s: &Session, p: &Value) -> Result<Search, String> {
    let collections = match p.get("collections") {
        Some(Value::String(c)) => c.split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect(),
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        _ => Vec::new(),
    };
    let bbox = match p.get("bbox") {
        Some(Value::String(v)) if v.trim().eq_ignore_ascii_case("view") => Some(view(s)?),
        Some(v @ Value::Array(_)) => {
            // Four numbers, every one: `[10, 45, 11, 46, "extra"]` is refused, not read without its last.
            let n: Option<Vec<f64>> = v.as_array().map(|a| a.iter().map(Value::as_f64).collect()).unwrap_or_default();
            match n.as_deref() {
                Some([w, south, e, n]) => Some([*w, *south, *e, *n]),
                _ => return Err("bbox is [west, south, east, north], or \"view\"".into()),
            }
        }
        None | Some(Value::Null) if b(p, "view").unwrap_or(false) => Some(view(s)?),
        None | Some(Value::Null) => None,
        // Anything else ("nowhere", a number) is refused rather than read as no area at all.
        Some(_) => return Err("bbox is [west, south, east, north], or \"view\"".into()),
    };
    Ok(Search {
        collections,
        bbox,
        datetime: ps(p, "datetime").map(str::to_string),
        limit: f(p, "limit").map(|l| l.clamp(1.0, stac::MAX_ITEMS as f64) as usize).unwrap_or(10),
        max_cloud: f(p, "maxCloud"),
        sortby: ps(p, "sortby").map(str::to_string),
    })
}

/// The map's extent in longitude/latitude, within the globe.
fn view(s: &Session) -> Result<[f64; 4], String> {
    let e = super::workspace::view_extent(s);
    if e.is_empty() {
        return Err("the map's extent isn't known; give bbox".into());
    }
    Ok([e.min_x.max(-180.0), e.min_y.max(-90.0), e.max_x.min(180.0), e.max_y.min(90.0)])
}

/// Adds items' footprints as a polygon layer; its id.
pub fn add_footprints(s: &mut Session, items: &[Item], name: &str) -> Result<Option<u32>, String> {
    add_collection(s, &stac::footprints_geojson(items), name)
}

/// `stac.addFootprints`: the footprints the app found (a GeoJSON FeatureCollection) as a layer,
/// through the command so the change is undoable like any other.
fn add_footprints_cmd(s: &mut Session, p: &Value) -> CmdResult {
    let fc = p.get("footprints").ok_or("footprints is a GeoJSON FeatureCollection")?;
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or("STAC footprints");
    match add_collection(s, fc, name)? {
        Some(id) => Ok(serde_json::json!({"layer": id})),
        None => Err("no footprints to add".into()),
    }
}

pub(crate) fn add_collection(s: &mut Session, fc: &Value, name: &str) -> Result<Option<u32>, String> {
    if fc["features"].as_array().is_none_or(Vec::is_empty) {
        return Ok(None);
    }
    let text = fc.to_string();
    if text.len() > 64 << 20 {
        return Err("the footprints are too large".into());
    }
    let (ds, _) = rustgis_data::io::read_geojson(&text, name).map_err(|e| e.to_string())?;
    let id = super::layer::add_dataset(s, ds);
    // Outlines over a faint fill, so the map (and the scenes added later) show through.
    if let Some(l) = s.map_mut().layer_mut(id)
        && let rustgis_map::Renderer::Single(sym) = &mut l.renderer
    {
        let [r, g, b, _] = sym.fill.0;
        sym.fill = rustgis_map::Rgba::new(r, g, b, 25);
        sym.stroke = rustgis_map::Rgba::new(r, g, b, 255);
        sym.stroke_width = 1.5;
    }
    Ok(Some(id))
}

pub(super) fn collections(s: &mut Session, p: &Value) -> CmdResult {
    let api = request_api(s, p)?;
    #[cfg(target_arch = "wasm32")]
    {
        let _ = api;
        Err("in the web app, browse STAC with Home › Add › Web Data › STAC Search".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let url = if stac::catalog::is_static(&api) {
            crate::render::rustgis_tiles::address::check(&api, None)?;
            api.clone()
        } else {
            stac::collections_url(&api)
        };
        let bytes = crate::render::rustgis_tiles::cache::download_blocking(&url)?;
        collections_json(&api, &bytes)
    }
}

pub(super) fn collections_json(api: &str, bytes: &[u8]) -> CmdResult {
    if stac::catalog::is_static(api) {
        let c = stac::catalog::parse_children(bytes, api)?;
        return Ok(
            json!({"count": c.len(), "static": true, "collections": c.iter().map(|c| json!({"id": c.url, "title": c.title})).collect::<Vec<_>>()}),
        );
    }
    let c = stac::parse_collections(bytes)?;
    Ok(json!({"count": c.len(), "collections": c.iter().map(|c| json!({"id": c.id, "title": c.title})).collect::<Vec<_>>()}))
}

fn search(s: &mut Session, p: &Value) -> CmdResult {
    let api = request_api(s, p)?;
    if stac::catalog::is_static(&api) {
        return search_static(s, p, &api);
    }
    let url = stac::search_url(&api, &search_params(s, p)?)?;
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, url);
        Err("in the web app, search STAC with Home › Add › Web Data › STAC Search".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let bytes = crate::render::rustgis_tiles::cache::download_blocking(&url)?;
        let (items, next) = stac::parse_items(&bytes, &url)?;
        let all = b(p, "properties").unwrap_or(false);
        let mut out =
            json!({"count": items.len(), "items": items.iter().map(|i| stac::item_json_with(i, all)).collect::<Vec<_>>(), "next": next});
        if b(p, "footprints").unwrap_or(true)
            && let Some(id) = add_footprints(s, &items, "STAC footprints")?
        {
            out["layer"] = json!(id);
            if let Some(e) = s.map().layer(id).map(|l| l.extent()) {
                s.zoom_to(&e);
            }
        }
        Ok(out)
    }
}

/// A search of a static catalog: a walk from its document, keeping the items that match.
fn search_static(s: &mut Session, p: &Value, root: &str) -> CmdResult {
    crate::render::rustgis_tiles::address::check(root, None)?;
    let search = search_params(s, p)?;
    // A static catalog has no search: it is walked in its own order, and its nodes are its
    // collections.
    let mut ignored = Vec::new();
    if search.sortby.as_deref().is_some_and(|b| !b.trim().is_empty()) {
        ignored.push("sortby");
    }
    if !search.collections.is_empty() {
        ignored.push("collections");
    }
    let note = (!ignored.is_empty()).then(|| {
        format!(
            "{} ignored: a static catalog is walked in its own order (the first items found that match, then sorted by date); give a collection's collection.json as url",
            ignored.join(" and ")
        )
    });
    let filter = stac::catalog::Filter::from_search(&search, ps(p, "phase"))?;
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, filter, root, note);
        Err("in the web app, search STAC with Home › Add › Web Data › STAC Search".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let walk = stac::catalog::walk_blocking(root, filter, search.limit);
        let complete = walk.complete();
        let (read, failed) = (walk.reads, walk.failed);
        let items = walk.into_items();
        let all = b(p, "properties").unwrap_or(false);
        let mut out = json!({"count": items.len(), "items": items.iter().map(|i| stac::item_json_with(i, all)).collect::<Vec<_>>(),
            "complete": complete, "read": read, "failed": failed});
        if let Some(n) = note {
            out["note"] = json!(n);
        }
        if b(p, "footprints").unwrap_or(true)
            && let Some(id) = add_footprints(s, &items, "STAC footprints")?
        {
            out["layer"] = json!(id);
            if let Some(e) = s.map().layer(id).map(|l| l.extent()) {
                s.zoom_to(&e);
            }
        }
        Ok(out)
    }
}

/// The item a command names: given whole, or by its address.
fn item(p: &Value) -> Result<Item, String> {
    if let Some(v) = p.get("item").filter(|v| v.is_object()) {
        // Given whole (by the assistant or a plugin, maybe from another answer): it came from no
        // address, so its assets may only be public web addresses.
        return stac::item_from_json(v, None).ok_or_else(|| "item isn't a STAC item".to_string());
    }
    let url = ps(p, "url").map(str::trim).filter(|u| !u.is_empty()).ok_or("give item (a STAC item) or url (an item's address)")?;
    #[cfg(target_arch = "wasm32")]
    {
        let _ = url;
        Err("in the web app, add STAC assets with Home › Add › Web Data › STAC Search".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let bytes = crate::render::rustgis_tiles::cache::download_blocking(url)?;
        let mut i = stac::parse_item(&bytes, url)?;
        i.url.get_or_insert_with(|| url.to_string());
        Ok(i)
    }
}

fn add_asset(s: &mut Session, p: &Value) -> CmdResult {
    let bound = crate::service_logins::request_params(s, p)?;
    let p = bound.as_ref();
    let i = item(p)?;
    let (key, href) = i.asset_href(ps(p, "asset"))?;
    // Codes for a failed retrieval (JAXA Earth's `dn.error`), which a data cube skips but a
    // single file draws as values: of the asset `asset_href` picked, by the same selector.
    let codes = i.pick(ps(p, "asset")).ok().map(|a| (a.invalid.clone(), a.codes_unread)).unwrap_or_default();
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| format!("{} {key}", i.id));
    // A Planetary Computer asset is read with its storage container's token.
    #[cfg(not(target_arch = "wasm32"))]
    let href = stac::sign_blocking(&href)?;
    let mut add = json!({"url": href, "name": name});
    if let Some(connection) = p.get("connection") {
        add["connection"] = connection.clone();
    }
    let added = super::layer::add_url(s, &add)?;
    match codes {
        (_, true) => s.warn(format!(
            "{name}: its metadata's codes for a failed retrieval (je:rasters dn.error) couldn't be read whole (not a list of numbers, or too many), so the map draws any such codes in the file as values"
        )),
        (invalid, false) if !invalid.is_empty() => {
            // The first few, large ones (a float's lowest, -3.4e38) in exponent form.
            const SHOWN: usize = 5;
            let code = |v: &f64| if v.abs() >= 1e6 { format!("{v:e}") } else { v.to_string() };
            let mut listed = invalid.iter().take(SHOWN).map(code).collect::<Vec<_>>().join(", ");
            if invalid.len() > SHOWN {
                listed.push_str(&format!(" and {} more", invalid.len() - SHOWN));
            }
            s.warn(format!(
                "{name}: its metadata marks failed retrievals with {} code(s) ({listed}), which the map draws as values",
                invalid.len()
            ));
        }
        _ => {}
    }
    Ok(added)
}

/// The most items a data cube on the time slider takes (its search is paged up to this).
pub const MAX_CUBE_ITEMS: usize = 2000;
/// The most items a trend or a summary over a STAC collection takes (`cube.trend`, `cube.reduce`)
/// where only a shell waits on it ([`Session::long_runs`]): their reads are bounded by their
/// cells and dates (pieces, pairs), not their files, so a state's four MODIS tiles over 25 years
/// (2,304) fit. In the app, which waits for the search with no Cancel, and in Python, they take
/// [`MAX_CUBE_ITEMS`].
pub const MAX_SERIES_ITEMS: usize = 10_000;
/// The most pages of a search a data cube reads, whatever they hold.
pub const MAX_CUBE_PAGES: usize = 100;

/// A STAC collection's band as files: by grid tile, then by day, the address of the first item's
/// asset found (the search's order), with the asset's scale and offset. What `stac.addCube` adds
/// as tile layers and `cube.trend` reads as a data cube (#559, #579).
#[cfg(not(target_arch = "wasm32"))]
pub struct CubeFiles {
    pub tiles: std::collections::BTreeMap<String, std::collections::BTreeMap<i64, String>>,
    /// The first asset's (scale, offset), from `raster:bands` or `eo:bands`.
    pub scale: Option<(f64, f64)>,
    /// Each file's own (scale, offset), by its address: a trend reads each date's values as its
    /// own metadata says to (#579).
    pub scales: std::collections::HashMap<String, (f64, f64)>,
    /// Each file's stored values that aren't values, from its asset's metadata, where it names
    /// them (MODIS NDVI's -3000 at the Brazil Data Cube, JAXA Earth's codes for a failed
    /// retrieval), skipped as the file's own header no-data is.
    pub nodata: std::collections::HashMap<String, Vec<f64>>,
    /// The items found, and those left out (no date, or no such asset).
    pub items: usize,
    pub without: usize,
    /// Whether any item names its grid tile (otherwise each footprint counts as one).
    pub named_tiles: bool,
    /// Items left out because their tile and day already had a file.
    pub duplicates: usize,
    /// Whether the assets' scale or offset differ from the first one's.
    pub scales_differ: bool,
    /// Files whose metadata names codes for a failed retrieval (JAXA Earth's `dn.error`).
    pub coded: usize,
    pub collection: String,
    pub asset: String,
}

/// The first moment after a period given as a year (`"2024"`) or a month (`"2024-03"`), so an
/// end given so takes all of it; `None` for a date (an end on that day) or anything else.
pub(crate) fn period_after(v: &Value) -> Option<f64> {
    let given = v.as_str().map_or_else(|| v.to_string(), |s| s.trim().to_string());
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    let next = match given.split_once('-') {
        None if given.len() == 4 && digits(&given) => given.parse::<i32>().ok().map(|y| format!("{}-01-01", y + 1)),
        Some((y, m)) if y.len() == 4 && m.len() == 2 && digits(y) && digits(m) => match (y.parse::<i32>(), m.parse::<u32>()) {
            (Ok(y), Ok(12)) => Some(format!("{}-01-01", y + 1)),
            (Ok(y), Ok(m)) if (1..12).contains(&m) => Some(format!("{y:04}-{:02}-01", m + 1)),
            _ => None,
        },
        _ => None,
    };
    next.and_then(|n| rustgis_data::time::parse_text(&n))
}

/// Searches a collection over an area and period, every page up to `max_items`, and
/// groups the items' asset by tile and day: `{"url"?, "collection", "asset", "bbox"? | "view",
/// "start", "end", "maxCloud"?}`.
#[cfg(not(target_arch = "wasm32"))]
pub fn cube_files(s: &mut Session, p: &Value, max_items: usize) -> Result<CubeFiles, String> {
    let api = request_api(s, p)?;
    let collection = ps(p, "collection").map(str::trim).filter(|c| !c.is_empty()).ok_or("collection is required")?;
    let asset = ps(p, "asset").map(str::trim).filter(|a| !a.is_empty()).ok_or("asset is required (a band, such as NDVI)")?;
    let date = |k: &str| {
        p.get(k)
            .and_then(rustgis_data::time::parse_input)
            .ok_or_else(|| format!("{k} is a date (2024-06-01, a year, or milliseconds since 1970)"))
    };
    let (start, mut end) = (date("start")?, date("end")?);
    // An end given as a year or a month is the end of it: "end": "2024" takes all of 2024.
    if let Some(after) = p.get("end").and_then(period_after) {
        end = after - 86_400_000.0;
    }
    if end < start {
        return Err("end is before start".into());
    }
    let day = |ms: f64| rustgis_data::time::format(ms, "YYYY-MM-DD");
    // RFC 3339, as STAC asks: the whole of the last day included.
    let mut q = json!({"collections": collection, "datetime": format!("{}T00:00:00Z/{}T23:59:59Z", day(start), day(end)), "limit": 200});
    for k in ["bbox", "maxCloud"] {
        if let Some(v) = p.get(k).filter(|v| !v.is_null()) {
            q[k] = v.clone();
        }
    }
    if q.get("bbox").is_none() {
        q["bbox"] = json!("view");
    }
    let search = search_params(s, &q)?;
    // Every page of the search, up to the cap.
    let mut items: Vec<Item> = Vec::new();
    // An empty page, or a next link back to a page already read, ends it; so does the page cap.
    let mut next = if stac::catalog::is_static(&api) {
        // A static collection has no search: read by its layout, and no pages follow.
        crate::render::rustgis_tiles::address::check(&api, None)?;
        let filter = stac::catalog::Filter::from_search(&search, None)?;
        let found = stac::dated::search_blocking(&api, &filter, (&day(start), &day(end)), max_items)?;
        // Its items say which collection it is; another name given is a mistake.
        if let Some(other) = found.items.iter().map(|i| i.collection.as_str()).find(|c| !c.is_empty() && *c != collection) {
            return Err(format!("{api} holds {other}, not {collection}"));
        }
        if found.unread > 0 {
            s.warn(format!(
                "{collection}: {} document(s) couldn't be read, were refused or weren't read whole, so some of its dates or tiles may be missing from the cube",
                found.unread
            ));
        }
        items = found.items;
        None
    } else {
        Some(stac::search_url(&api, &search)?)
    };
    let mut read: Vec<String> = Vec::new();
    // Pages are read from the API's own host: a next link elsewhere ends the search.
    // Scheme, user part, host and port compared: `https://host:443`, never another host or http.
    let host = |u: &str| {
        let u = crate::render::rustgis_tiles::logins::inner(u);
        u.split_once("://").map(|(scheme, r)| {
            let authority = r.split(['/', '?', '#']).next().unwrap_or("");
            let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h).to_ascii_lowercase();
            let scheme = scheme.to_ascii_lowercase();
            // The scheme's own port written out is the same host.
            let default = if scheme == "https" { ":443" } else { ":80" };
            format!("{scheme}://{}", host.strip_suffix(default).unwrap_or(&host))
        })
    };
    // An address with no scheme has no host to match: a next link is never taken on it.
    let api_host = host(&api);
    while let Some(url) = next.take() {
        if read.contains(&url) || (!read.is_empty() && (api_host.is_none() || host(&url) != api_host)) {
            break;
        }
        if read.len() >= MAX_CUBE_PAGES {
            return Err(format!("more than {MAX_CUBE_PAGES} pages of results: take a shorter period or a smaller area"));
        }
        let bytes = crate::render::rustgis_tiles::cache::download_blocking(&url)?;
        let (page, more) = stac::parse_items(&bytes, &url)?;
        read.push(url);
        if page.is_empty() {
            break;
        }
        items.extend(page);
        if items.len() > max_items {
            return Err(format!("more than {max_items} items: take a shorter period or a smaller area"));
        }
        next = more;
    }
    if items.is_empty() {
        return Err(format!("no item of {collection} in the area and period"));
    }
    // By tile, then by day: the first item found for a tile and day (the search's order).
    let mut tiles: std::collections::BTreeMap<String, std::collections::BTreeMap<i64, String>> = Default::default();
    let mut scale: Option<(f64, f64)> = None;
    let mut scales: std::collections::HashMap<String, (f64, f64)> = Default::default();
    let mut nodata: std::collections::HashMap<String, Vec<f64>> = Default::default();
    let mut scales_differ = false;
    let mut coded = 0usize;
    let mut codes_unread = 0usize;
    let mut duplicates = 0usize;
    let mut without = 0usize;
    // Each public bucket's region, found once ([`public_web`]).
    let mut regions: std::collections::HashMap<String, Option<String>> = Default::default();
    for i in &items {
        let Some(ms) = rustgis_data::time::parse_text(&i.datetime) else {
            without += 1;
            continue;
        };
        let Some(a) = i.assets.iter().find(|a| a.key.eq_ignore_ascii_case(asset)) else {
            without += 1;
            continue;
        };
        // Its codes for a failed retrieval not read whole: its values can't be told from them.
        if a.codes_unread {
            codes_unread += 1;
            without += 1;
            continue;
        }
        let key = i.tile.clone().unwrap_or_else(|| match i.bbox {
            Some([w, so, e, n]) => format!("{w:.2},{so:.2},{e:.2},{n:.2}"),
            None => i.id.clone(),
        });
        let d = (ms / 86_400_000.0).floor() as i64;
        let by_day = tiles.entry(key).or_default();
        if by_day.contains_key(&d) {
            // A tile and day already have a file; this one is left out (and counted), its scale too.
            duplicates += 1;
            continue;
        }
        // The file's address without a fragment of its own, so the range look is the only one.
        let href = crate::render::rustgis_tiles::logins::inherit(&api, a.href.split('#').next().unwrap_or(&a.href));
        // A public bucket's `s3://` file (Digital Earth Australia's, the Copernicus DEM's) is read
        // anonymously at its web address, as a cube's files are kept.
        let href = if s3::is_s3(&href) { public_web(&href, &mut regions)? } else { href };
        // No scale or offset in the metadata reads as 1 and 0, the values as stored.
        let this = (a.scale.unwrap_or(1.0), a.offset.unwrap_or(0.0));
        scales.insert(href.clone(), this);
        let stored: Vec<f64> = a.nodata.iter().chain(&a.invalid).copied().collect();
        if !stored.is_empty() {
            nodata.insert(href.clone(), stored);
        }
        coded += usize::from(!a.invalid.is_empty());
        by_day.insert(d, href);
        scales_differ |= scale.is_some_and(|s| s != this);
        scale.get_or_insert(this);
    }
    // Items left out for their codes: said, in the error when nothing else is left.
    let unread_codes = (codes_unread > 0).then(|| {
        format!(
            "{codes_unread} item(s) left out, as their codes for a failed retrieval couldn't be read whole (one isn't a number, or there are more than {} of them)",
            stac::MAX_INVALID
        )
    });
    if tiles.is_empty() {
        return Err(match unread_codes {
            Some(why) => format!("{collection}: {why}"),
            None => format!("no item of {collection} has an asset {asset}"),
        });
    }
    if let Some(why) = unread_codes {
        s.warn(format!("{collection}: {why}"));
    }
    // Files reached through signed addresses (Planetary Computer's) expire within hours, so a
    // cube of them couldn't be kept in a project or read a day later.
    // The files' addresses come from the API's answer: each is checked as one that answer points
    // to (http or https, never a forbidden address, and not into the local network from a public
    // API), before a cube on the map or a trend reads it.
    if let Some(e) = tiles
        .values()
        .flat_map(|by_day| by_day.values())
        .find_map(|href| crate::render::rustgis_tiles::address::check(href, Some(&api)).err())
    {
        return Err(e);
    }
    if tiles.values().flat_map(|by_day| by_day.values()).any(|href| stac::needs_signing(href)) {
        return Err(format!(
            "{collection}'s files are reached through signed addresses that expire within hours, which neither a data cube on the map nor a trend reads: use an API with public files (Earth Search, the Brazil Data Cube)"
        ));
    }
    Ok(CubeFiles {
        tiles,
        scale,
        scales,
        nodata,
        items: items.len(),
        named_tiles: items.iter().any(|i| i.tile.is_some()),
        without,
        duplicates,
        scales_differ,
        coded,
        collection: collection.to_string(),
        asset: asset.to_string(),
    })
}

/// The web address a public bucket's `s3://` file is read at, anonymously: AWS's global endpoint,
/// or the bucket's region's when the global one doesn't reach it (Digital Earth Africa's
/// af-south-1), found by reading one byte of the bucket's first file. `regions` keeps each
/// bucket's, so a cube's thousands of files cost one read a bucket.
#[cfg(not(target_arch = "wasm32"))]
fn public_web(href: &str, regions: &mut std::collections::HashMap<String, Option<String>>) -> Result<String, String> {
    let (bucket, _) = s3::parse(href)?;
    let region = match regions.get(&bucket) {
        Some(r) => r.clone(),
        None => {
            let r = s3::public_region(href, crate::render::rustgis_tiles::cache::probe_blocking)?;
            regions.insert(bucket, r.clone());
            r
        }
    };
    s3::public_url(href, region.as_deref())
}

/// The most cells a STAC collection's trend covers: its four outputs take 16 bytes a cell, 400 MB
/// at the cap (the dates are read in pieces, each within [`super::cube::MAX_VALUES`]).
#[cfg(not(target_arch = "wasm32"))]
pub const MAX_TREND_CELLS: usize = 25_000_000;

/// Dates a STAC collection's trend reads at once: its files are read over the network.
#[cfg(not(target_arch = "wasm32"))]
const STAC_READERS: usize = 8;

/// Tiles' headers a STAC collection's trend reads, beyond the first file's, looking for one that
/// covers the area: each can take up to two minutes on a stalled server.
#[cfg(not(target_arch = "wasm32"))]
const COVER_PROBES: usize = 8;

/// The most pixels of the files one slice of a piece reads: 1 million. A reader holds them (4
/// bytes each) with its cells' sums, counts and values (16 bytes a cell, as many cells as pixels
/// at most), about 20 MB, so the [`STAC_READERS`] hold about 160 MB together; the slices read and
/// waiting to go into the block (as many as readers) keep only their values, 4 MB each, 32 MB
/// more. The block itself is within [`super::cube::MAX_VALUES`]. A trend over few
/// dates would otherwise make pieces of many cells, and averaging reads `factor²` pixels a cell.
#[cfg(not(target_arch = "wasm32"))]
const MAX_PIECE_PIXELS: u64 = 1_000_000;

#[cfg(test)]
thread_local! {
    /// For tests: the values a piece holds at most, in place of [`super::cube::MAX_VALUES`], so a
    /// small cube is read in several pieces.
    pub(crate) static PIECE_VALUES: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// A STAC collection's band as a data cube for `cube.trend` (#579): each date's files (a grid
/// tile each, MODIS's h13v09 and h13v10 for one date), read in place from their tiled GeoTIFFs
/// over an area, at one level of their overviews, and put together on one grid. A large area is
/// read in pieces, bands of rows each within a run's block, so a state's 25 years of 16-day
/// composites can be worked out whole.
#[cfg(not(target_arch = "wasm32"))]
pub struct StacCube {
    dates: Vec<f64>,
    /// Each date's files, with their (scale, offset) and the stored values their metadata says
    /// aren't values.
    files: Vec<Vec<(String, (f64, f64), Vec<f64>)>>,
    /// The level read (0: the full images), and how many of its pixels a cell's side takes: 1
    /// when the cells are that level's own pixels (`"resampling": "nearest"`, an overview as the
    /// file stores it), more when a cell is the mean of the full images' pixels in it.
    level: usize,
    factor: u32,
    /// The area on that level's grid: its georeferencing (from its top-left corner), columns and
    /// rows, in the files' coordinate system.
    transform: rustgis_data::raster::GeoTransform,
    width: u32,
    height: u32,
    crs: rustgis_data::raster::Crs,
    /// Rows a piece reads.
    rows: u32,
    /// Whether any file met a piece read: the tiles past the cover probes may all miss the area.
    met: std::sync::atomic::AtomicBool,
    label: String,
}

/// Where a grid's columns and rows fall in a file's: the offsets (in the file's pixels) of the
/// grid's top-left corner, when both are on one grid of pixels (the same size, corners a whole
/// number of pixels apart); `None` otherwise.
#[cfg(not(target_arch = "wasm32"))]
fn offset_in(grid: &rustgis_data::raster::GeoTransform, file: &rustgis_data::raster::GeoTransform) -> Option<(i64, i64)> {
    let same = |a: f64, b: f64| (a - b).abs() <= a.abs().max(b.abs()) * 1e-6;
    if !same(grid.pixel_width, file.pixel_width) || !same(grid.pixel_height, file.pixel_height) {
        return None;
    }
    let dx = (grid.origin_x - file.origin_x) / file.pixel_width;
    let dy = (grid.origin_y - file.origin_y) / file.pixel_height;
    let whole = |v: f64| v.is_finite() && (v - v.round()).abs() <= 1e-3 && v.abs() < 1e9;
    (whole(dx) && whole(dy)).then(|| (dx.round() as i64, dy.round() as i64))
}

/// The rows a piece reads: as many as `max` values hold over `width` columns and `dates` dates,
/// a whole number of the files' tile rows when that many fit (so a tile is read by at most the two
/// pieces its rows straddle, the area's top not being on a tile's), at least one. When fewer rows
/// than a tile's fit (Piauí at 1 km over 575 dates: ~170 rows against 512), each tile row is read
/// by every piece within it, for each date: the cost of the area's size. `None` when a single row is too many.
#[cfg(not(target_arch = "wasm32"))]
pub fn piece_rows(width: u32, height: u32, dates: usize, tile_height: u32, max: u64) -> Option<u32> {
    let per_row = u64::from(width.max(1)).saturating_mul(dates.max(1) as u64);
    let fit = max / per_row;
    if fit == 0 {
        return None;
    }
    // The whole area in one piece when it fits, however its rows fall on the tiles.
    let height = height.max(1);
    if fit >= u64::from(height) {
        return Some(height);
    }
    let fit = u32::try_from(fit).unwrap_or(u32::MAX);
    let th = tile_height.max(1);
    Some(if fit >= th { fit / th * th } else { fit })
}

/// Prepares a STAC collection's band as a cube: the search (as `stac.addCube` makes it), the
/// overview level for `cellSize`, the area's grid, and the pieces it is read in.
#[cfg(not(target_arch = "wasm32"))]
pub fn stac_cube(s: &mut Session, p: &Value) -> Result<StacCube, String> {
    use crate::render::rustgis_tiles::cog;
    // The parameters read here, before the search and the first header are waited for.
    let size = match p.get("cellSize").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => {
            Some(v.as_f64().filter(|c| c.is_finite() && *c > 0.0).ok_or("cellSize is a size in the files' units (metres for most)")?)
        }
    };
    // An overview is often made by taking one pixel in each block (the Brazil Data Cube's are,
    // GDAL's NEAREST), so a cell of it is one of the full images' pixels, which can differ much
    // from the place's mean: "average" reads the full images and takes the mean of each cell's.
    let average = match p.get("resampling").filter(|v| !v.is_null()) {
        None => false,
        Some(v) => match v.as_str().map(str::trim) {
            Some("") | Some("nearest") => false,
            Some("average") => true,
            Some(x) => return Err(format!("resampling is nearest or average, not {x}")),
            // A number or anything else isn't read as the default: said.
            None => return Err("resampling is nearest or average".into()),
        },
    };
    // The area, in degrees.
    let bbox = match p.get("bbox").filter(|v| !v.is_null()) {
        Some(v) if v.as_str().is_some_and(|t| t.trim().eq_ignore_ascii_case("view")) => view(s)?,
        None => view(s)?,
        Some(v) => {
            let b: Option<Vec<f64>> =
                v.as_array().filter(|a| a.len() == 4).and_then(|a| a.iter().map(|x| x.as_f64().filter(|x| x.is_finite())).collect());
            match b.as_deref() {
                Some([w, so, e, n]) if w < e && so < n => [*w, *so, *e, *n],
                _ => return Err("bbox is [west, south, east, north] in degrees, or \"view\"".into()),
            }
        }
    };
    // The search covers the area as read here: "view" (in any case or spacing) or none given is
    // the map's view, resolved, so the search and the grid agree.
    let mut searched = p.clone();
    if let Some(o) = searched.as_object_mut() {
        o.insert("bbox".into(), json!(bbox));
    }
    let max_items = if s.long_runs.is_some() { MAX_SERIES_ITEMS } else { MAX_CUBE_ITEMS };
    let CubeFiles { tiles, scales, nodata, collection, asset, .. } = cube_files(s, &searched, max_items)?;
    // Every tile's file for a date, by day.
    let mut by_day: std::collections::BTreeMap<i64, Vec<(String, (f64, f64), Vec<f64>)>> = Default::default();
    for days in tiles.values() {
        for (d, href) in days {
            let scale = scales.get(href).copied().unwrap_or((1.0, 0.0));
            by_day.entry(*d).or_default().push((href.clone(), scale, nodata.get(href).cloned().unwrap_or_default()));
        }
    }
    if by_day.len() > super::cube::MAX_DATES {
        return Err(format!("{} dates; a trend or summary takes {} at most: take a shorter period", by_day.len(), super::cube::MAX_DATES));
    }
    // The first file's header gives the coordinate system, the grid and its overviews.
    let first = by_day.values().flatten().next().map(|(h, ..)| h.clone()).ok_or("no file to read")?;
    let info = cog::info_blocking(&first)?;
    let layout = &info.layout;
    if layout.transform.pixel_height > 0.0 {
        return Err(crate::sources::message_with_address(&first, "stored bottom up, which a trend doesn't read yet"));
    }
    // The level whose cells are nearest the size asked, by ratio (`1000` takes MODIS's 926.6 m
    // overview, not its 463 m one); the full images by default.
    let level = match size {
        None => 0,
        Some(size) => {
            let off = |l: &rustgis_data::raster::cog::Level| (layout.transform_of(l).pixel_width.abs() / size).ln().abs();
            (0..layout.levels.len()).min_by(|a, b| off(&layout.levels[*a]).total_cmp(&off(&layout.levels[*b]))).unwrap_or(0)
        }
    };
    let lv = layout.levels.get(level).ok_or("the file has no image")?;
    let lt = layout.transform_of(lv);
    let full = layout.levels.first().ok_or("the file has no image")?;
    let (read, factor) = if average && level > 0 {
        let f = (f64::from(full.width) / f64::from(lv.width.max(1))).round();
        // Averaging takes `f` × `f` full pixels a cell: the overview has to be exactly the full
        // image's size divided by `f` (rounded up), or its cells aren't those blocks.
        let fits = |full: u32, ov: u32| ov.checked_mul(f as u32) == Some(full);
        if !(1.0..=64.0).contains(&f) || !fits(full.width, lv.width) || !fits(full.height, lv.height) {
            return Err(crate::sources::message_with_address(
                &first,
                &format!(
                    "its overview ({} × {}) isn't its full image ({} × {}) divided by a whole number, so its cells can't be averaged from it: take \"resampling\": \"nearest\"",
                    lv.width, lv.height, full.width, full.height
                ),
            ));
        }
        (0, f as u32)
    } else {
        (level, 1)
    };
    let read_tile_rows = layout.levels.get(read).map_or(1, |l| l.tile_height).div_ceil(factor).max(1);
    // The area, in the files' coordinates: the box's edges, each at 32 points, as a curved grid
    // (MODIS's sinusoidal) bends them.
    let tr = layout.crs.transformer()?;
    let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    let [w, so, e, n] = bbox;
    for k in 0..=32 {
        let t = f64::from(k) / 32.0;
        for (lon, lat) in [(w + (e - w) * t, so), (w + (e - w) * t, n), (w, so + (n - so) * t), (e, so + (n - so) * t)] {
            if let Some(c) = tr.from_lonlat(rustgis_geom::Coord::new(lon, lat)) {
                (x0, y0, x1, y1) = (x0.min(c.x), y0.min(c.y), x1.max(c.x), y1.max(c.y));
            }
        }
    }
    if !(x0 < x1 && y0 < y1) {
        return Err("the area is outside the files' coordinate system".into());
    }
    // Snapped outwards onto the level's pixels.
    let (pw, ph) = (lt.pixel_width, lt.pixel_height);
    let c0 = ((x0 - lt.origin_x) / pw).floor();
    let c1 = ((x1 - lt.origin_x) / pw).ceil();
    let r0 = ((y1 - lt.origin_y) / ph).floor();
    let r1 = ((y0 - lt.origin_y) / ph).ceil();
    let (width, height) = (c1 - c0, r1 - r0);
    if !(width >= 1.0 && height >= 1.0) || width * height > MAX_TREND_CELLS as f64 {
        return Err(format!(
            "the area is {width:.0} × {height:.0} cells at {:.0}-unit cells; a trend covers {MAX_TREND_CELLS} at most: take a smaller area or a larger cellSize",
            pw.abs()
        ));
    }
    let (width, height) = (width as u32, height as u32);
    let transform = rustgis_data::raster::GeoTransform { origin_x: lt.origin_x + c0 * pw, origin_y: lt.origin_y + r0 * ph, ..lt };
    // Some file has to cover part of the area, or every cell would come out empty: each tile's
    // first file in turn until one does (the first file's header, read already, most often), at
    // most [`COVER_PROBES`] more headers, a header that can't be read ending it as reading would.
    let span = |a: f64, b: f64| (a.min(b), a.max(b));
    let (ax, ay) = (span(c0 * pw, c1 * pw), span(r0 * ph, r1 * ph));
    let (ax, ay) = ((ax.0 + lt.origin_x, ax.1 + lt.origin_x), (ay.0 + lt.origin_y, ay.1 + lt.origin_y));
    let covers = |i: &cog::Info| {
        let (t, Some(full)) = (&i.layout.transform, i.layout.levels.first()) else { return false };
        let x = span(t.origin_x, t.origin_x + f64::from(full.width) * t.pixel_width);
        let y = span(t.origin_y, t.origin_y + f64::from(full.height) * t.pixel_height);
        i.layout.crs == layout.crs && x.0 < ax.1 && ax.0 < x.1 && y.0 < ay.1 && ay.0 < y.1
    };
    let mut others = tiles.values().filter_map(|days| days.values().next()).filter(|h| **h != first);
    let mut covered = covers(&info);
    let mut probed = 0;
    while !covered && probed < COVER_PROBES {
        let Some(h) = others.next() else { break };
        covered = covers(&*cog::info_blocking(h)?);
        probed += 1;
    }
    // Tiles beyond the probes are left to the reading, which leaves what none covers empty.
    if !covered && others.next().is_none() {
        return Err(format!("no file of {collection}'s {asset} covers the area, only comes near it: take another bbox"));
    }
    // Within a run's block, and each slice within what a reader holds.
    let per_slice = (MAX_PIECE_PIXELS / u64::from(factor).pow(2)).max(1);
    #[cfg(test)]
    let block = PIECE_VALUES.with(|v| v.get()).unwrap_or(super::cube::MAX_VALUES);
    #[cfg(not(test))]
    let block = super::cube::MAX_VALUES;
    let max = block.min(per_slice.saturating_mul(by_day.len() as u64));
    let rows = piece_rows(width, height, by_day.len(), read_tile_rows, max).ok_or_else(|| {
        format!(
            "a row of {width} cells × {} dates is more than a run's block: take a smaller area, a larger cellSize or fewer dates",
            by_day.len()
        )
    })?;
    let dates = by_day.keys().map(|d| *d as f64 * 86_400_000.0).collect();
    let files = by_day.into_values().collect();
    Ok(StacCube {
        dates,
        files,
        level: read,
        factor,
        transform,
        width,
        height,
        crs: layout.crs,
        rows,
        met: Default::default(),
        label: format!("{collection} {asset}"),
    })
}

#[cfg(not(target_arch = "wasm32"))]
impl super::cube::TimeCube for StacCube {
    fn dates(&self) -> &[f64] {
        &self.dates
    }
    /// The whole slice, when the area is one piece; read in pieces, a date's whole slice would be
    /// the block the pieces exist to avoid, so it is refused rather than cut to the top rows.
    fn slice(&self, i: usize) -> Result<Option<(std::sync::Arc<rustgis_data::raster::Raster>, usize)>, String> {
        if self.pieces() > 1 {
            return Err(format!("this STAC cube is read in {} pieces: read it piece by piece (slice_of)", self.pieces()));
        }
        self.slice_of(i, 0)
    }
    fn label(&self) -> String {
        self.label.clone()
    }
    fn pieces(&self) -> usize {
        self.height.div_ceil(self.rows.max(1)) as usize
    }
    fn size(&self) -> Option<(usize, usize)> {
        Some((self.width as usize, self.height as usize))
    }
    fn readers(&self) -> usize {
        STAC_READERS
    }
    fn nothing_read(&self) -> Option<String> {
        let met = self.met.load(std::sync::atomic::Ordering::Relaxed);
        (!met).then(|| format!("no file of {} covers the area, only comes near it: take another bbox", self.label))
    }
    /// Rows `piece × rows` on of the area at date `i`: each file's part of them, its values
    /// scaled as its metadata says, a cell the mean of the pixels read in it (one, or a block of
    /// the full images'); a cell no file covers (or none has a value for) is NaN.
    fn slice_of(&self, i: usize, piece: usize) -> Result<Option<(std::sync::Arc<rustgis_data::raster::Raster>, usize)>, String> {
        use crate::render::rustgis_tiles::cog;
        let files = self.files.get(i).ok_or_else(|| format!("no date {i}"))?;
        let top = u32::try_from(piece).ok().and_then(|p| p.checked_mul(self.rows)).filter(|t| *t < self.height).ok_or("no such piece")?;
        let h = self.rows.min(self.height - top);
        let w = self.width;
        let f = self.factor.max(1);
        // Each cell's sum and count of the pixels read in it.
        let mut sum = vec![0.0f64; w as usize * h as usize];
        let mut count = vec![0u32; w as usize * h as usize];
        // The area's grid at the level read: a cell is `f` × `f` of its pixels.
        let grid = rustgis_data::raster::GeoTransform {
            pixel_width: self.transform.pixel_width / f64::from(f),
            pixel_height: self.transform.pixel_height / f64::from(f),
            ..self.transform
        };
        let (wf, top_f, hf) = (i64::from(w) * i64::from(f), i64::from(top) * i64::from(f), i64::from(h) * i64::from(f));
        for (href, (k, o), fill) in files {
            let fill: Vec<f32> = fill.iter().map(|v| *v as f32).collect();
            // Cached per address for the process (`with_info`, a failure too, for a while), so a
            // file's header is fetched once however many pieces and dates read it.
            let info = cog::info_blocking(href)?;
            if info.layout.crs != self.crs {
                return Err(crate::sources::message_with_address(href, "in another coordinate system than the first file"));
            }
            let lv = info
                .layout
                .levels
                .get(self.level)
                .ok_or_else(|| crate::sources::message_with_address(href, "has fewer overviews than the first file"))?;
            let lt = info.layout.transform_of(lv);
            let (dx, dy) = offset_in(&grid, &lt)
                .ok_or_else(|| crate::sources::message_with_address(href, "on another grid of pixels than the first file"))?;
            // Averaged, a cell is a block of `f` × `f` of the file's pixels: the file's corner
            // has to fall on a block's, or the blocks would straddle cells.
            if dx.rem_euclid(i64::from(f)) != 0 || dy.rem_euclid(i64::from(f)) != 0 {
                return Err(crate::sources::message_with_address(
                    href,
                    "its pixels don't fall in whole cells of the area: take \"resampling\": \"nearest\"",
                ));
            }
            // The piece's columns and rows in the file's pixels, within the file.
            let (fc0, fc1) = (dx.max(0), (dx + wf).min(i64::from(lv.width)));
            let (fr0, fr1) = ((dy + top_f).max(0), (dy + top_f + hf).min(i64::from(lv.height)));
            if fc1 <= fc0 || fr1 <= fr0 {
                continue;
            }
            self.met.store(true, std::sync::atomic::Ordering::Relaxed);
            let cols = (fc0 as u32, fc1 as u32);
            let rows = (fr0 as u32, fr1 as u32);
            let read = cog::read_values_blocking(&info, href, self.level, 0, cols, rows)?;
            let across = (fc1 - fc0) as usize;
            for (r, line) in read.chunks(across).enumerate() {
                let row = ((fr0 - dy - top_f) as usize + r) / f as usize;
                for (c, v) in line.iter().enumerate() {
                    let col = ((fc0 - dx) as usize + c) / f as usize;
                    let cell = row * w as usize + col;
                    if v.is_finite()
                        && !fill.contains(v)
                        && let (Some(s), Some(n)) = (sum.get_mut(cell), count.get_mut(cell))
                    {
                        *s += f64::from(*v) * k + o;
                        *n += 1;
                    }
                }
            }
        }
        let vals: Vec<f32> = sum.iter().zip(&count).map(|(s, n)| if *n > 0 { (s / f64::from(*n)) as f32 } else { f32::NAN }).collect();
        let transform = rustgis_data::raster::GeoTransform {
            origin_y: self.transform.origin_y + f64::from(top) * self.transform.pixel_height,
            ..self.transform
        };
        let r = rustgis_data::raster::Raster::new(
            &self.label,
            w,
            h,
            vec![vals],
            transform,
            self.crs,
            None,
            rustgis_data::raster::ColorHint::Gray,
            "Float32",
        )
        .map_err(|e| e.to_string())?;
        Ok(Some((std::sync::Arc::new(r), 0)))
    }
}

/// `stac.addCube` (opengeos/RustGIS#559): a STAC collection's band as a data cube on the time
/// slider. The items found are grouped by the grid tile they cover (the tile their properties
/// name, else their bounding box) and by day; each tile becomes a tile layer that reads a file
/// per date in place, the slider showing the last date in its window. A range, when given, is
/// the same for every date and tile, so a grey means one value throughout.
fn add_cube(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("in the web app, a STAC data cube can't be added yet".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let bound = crate::service_logins::request_params(s, p)?;
        let p = bound.as_ref();
        let range = match p.get("range").filter(|v| !v.is_null()) {
            None => None,
            Some(v) => {
                // Two finite numbers, nothing else: a value that isn't one is refused, not skipped.
                let r: Option<Vec<f64>> =
                    v.as_array().filter(|a| a.len() == 2).and_then(|a| a.iter().map(|x| x.as_f64().filter(|x| x.is_finite())).collect());
                match r.as_deref() {
                    Some([lo, hi]) if lo < hi => Some((*lo, *hi)),
                    _ => return Err("range is [low, high], low below high".into()),
                }
            }
        };
        let CubeFiles {
            tiles,
            scale,
            scales: _,
            nodata: _,
            items,
            named_tiles,
            without,
            duplicates,
            scales_differ,
            coded,
            collection,
            asset,
        } = cube_files(s, p, MAX_CUBE_ITEMS)?;
        // A tile layer's look has no way to name them, so on the map they are values.
        if coded > 0 {
            s.warn(format!(
                "{collection}: {coded} file(s) mark failed retrievals with codes, which the map draws as values (inside its range or stretch); Trend Analysis and cube summaries skip them"
            ));
        }
        // One range can't hold if the files store their values alike no longer (a processing
        // change of scale or offset partway).
        if range.is_some() && scales_differ {
            return Err(format!(
                "the {asset} files don't all have the same scale and offset, so one range can't fit them all: leave range out"
            ));
        }
        // The range in the files' stored values.
        let look = match (range, scale) {
            (Some(_), Some((k, o))) if k == 0.0 || !k.is_finite() || !o.is_finite() => {
                return Err(format!(
                    "the {asset} files' metadata gives a scale of {k} and an offset of {o}, so a range in the band's units can't be set: leave range out"
                ));
            }
            (Some((lo, hi)), Some((k, o))) => {
                let (a, b) = ((lo - o) / k, (hi - o) / k);
                format!("#range={},{}", a.min(b), a.max(b))
            }
            (Some((lo, hi)), None) => format!("#range={lo},{hi}"),
            (None, _) => String::new(),
        };
        let base = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map_or_else(|| format!("{collection} {asset}"), str::to_string);
        let mut dates: Vec<f64> = Vec::new();
        let many = tiles.len() > 1;
        let mut added = Vec::new();
        // What the map had: a cube that can't be added whole puts its tile layers back as they
        // were (those it replaced, the same cube added before, included) and takes out the layers
        // it added. The rest (the view a file framed, the slider) comes back with the project
        // snapshot the command runs under (`Session::guarded`), as for any command that fails.
        let (tiles_before, layers_before): (Vec<rustgis_map::TileLayer>, Vec<u32>) =
            (s.map().tiles.clone(), s.map().layers.iter().map(|l| l.id).collect());
        let undo = |s: &mut Session| {
            let m = s.map_mut();
            m.tiles = tiles_before.clone();
            m.layers.retain(|l| layers_before.contains(&l.id));
        };
        // A tile layer per grid tile, each reading its first file's header, within the map's room
        // for tile layers: a catalog without grid tiles (keyed by footprint) could make hundreds.
        let room = rustgis_map::tiles::MAX_TILE_LAYERS.saturating_sub(s.map().tiles.len());
        if tiles.len() > room {
            // Items naming no grid tile are told apart by footprint, which can differ scene by scene.
            let why = if !named_tiles {
                format!("{collection}'s items name no grid tile, so each footprint counts as one")
            } else {
                "take a smaller area".to_string()
            };
            return Err(format!(
                "{} grid tiles, and the map has room for {room} more tile layers (at most {}): {why}",
                tiles.len(),
                rustgis_map::tiles::MAX_TILE_LAYERS
            ));
        }
        // Every file a web address of sane length, as a project keeps them (TileLayer::repair),
        // so the cube reopens as it was made.
        if let Some(bad) = tiles.values().flat_map(|by_day| by_day.values()).find(|h| {
            let plain = crate::render::rustgis_tiles::logins::inner(h);
            let lower = plain.to_ascii_lowercase();
            !(lower.starts_with("https://") || lower.starts_with("http://")) || plain.len() > 4000
        }) {
            return Err(format!(
                "{}: a data cube's files are read in place from web addresses (http, https)",
                bad.chars().take(200).collect::<String>()
            ));
        }
        for (tile, by_day) in &tiles {
            let urls: Vec<(f64, String)> = by_day.iter().map(|(d, u)| (*d as f64 * 86_400_000.0, format!("{u}{look}"))).collect();
            dates.extend(urls.iter().map(|(d, _)| *d));
            let Some((_, first)) = urls.first() else { continue };
            let name = if many { format!("{base} {tile}") } else { base.clone() };
            let r = match super::layer::add_url(s, &json!({"url": first, "name": name, "cube": true, "connection": p.get("connection")})) {
                Ok(r) => r,
                Err(e) => {
                    undo(s);
                    return Err(e);
                }
            };
            // The tile layer add_url put in (at the index it gives, and the file's address) follows
            // the slider through its dates; a file read whole instead (a GeoTIFF that isn't
            // cloud-optimised) can't.
            let m = s.map_mut();
            let file = first.split('#').next();
            let at = r.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok());
            match at.and_then(|i| m.tiles.get_mut(i)).filter(|t| t.url.split('#').next() == file) {
                Some(t) => {
                    t.time_urls = urls;
                    t.name = name;
                    added.push(r);
                }
                None => {
                    undo(s);
                    return Err(format!(
                        "{}: not a cloud-optimised GeoTIFF read in place; a data cube needs those",
                        first.split('#').next().unwrap_or(first)
                    ));
                }
            }
        }
        dates.sort_by(f64::total_cmp);
        dates.dedup();
        // At most MAX_CUBE_ITEMS dates, well under the slider's steps; were it ever more, the cube
        // is refused rather than left with files the slider can't reach.
        if dates.len() > rustgis_map::time::MAX_STEPS {
            undo(s);
            return Err(format!("{} dates; the time slider takes {} at most", dates.len(), rustgis_map::time::MAX_STEPS));
        }
        // The slider over the cube's dates: on over them when it is off; when it is on, taking
        // them in (its own dates and the cube's, or its span widened to theirs), so every file
        // can be reached.
        let slider = match s.map().time.as_ref() {
            None => {
                let d: Vec<Value> = dates.iter().map(|d| json!(d)).collect();
                json!({"dates": d})
            }
            Some(t) if !t.dates.is_empty() => {
                let mut all: Vec<f64> = t.dates.iter().chain(&dates).copied().collect();
                all.sort_by(f64::total_cmp);
                all.dedup();
                if all.len() > rustgis_map::time::MAX_STEPS {
                    undo(s);
                    return Err(format!(
                        "the time slider's dates and the cube's make {}, more than its {} steps: take a shorter period",
                        all.len(),
                        rustgis_map::time::MAX_STEPS
                    ));
                }
                json!({"dates": all, "date": t.current})
            }
            Some(t) => {
                let (first, last) = (dates.first().copied().unwrap_or(t.start), dates.last().copied().unwrap_or(t.end));
                json!({"start": t.start.min(first), "end": t.end.max(last), "date": t.current})
            }
        };
        if let Err(e) = super::time::set(s, &slider) {
            undo(s);
            return Err(e);
        }
        let mut out = json!({"items": items, "tiles": tiles.len(), "dates": dates.len(), "layers": added.len()});
        if without > 0 {
            out["skipped"] = json!(without);
        }
        if duplicates > 0 {
            out["duplicates"] = json!(duplicates);
        }
        Ok(out)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use rustgis_data::raster::GeoTransform;

    /// A piece's rows: as many as fit, whole tiles of rows when one fits, at least one; none when
    /// a single row is too many. Piauí at ~1 km over 25 years of 16-day composites (645 × 984
    /// cells, 576 dates) takes about a sixth of the rows a piece.
    #[test]
    fn pieces_fit_a_block_in_whole_tiles() {
        assert_eq!(super::piece_rows(512, 512, 6, 256, 512 * 300 * 6), Some(256));
        assert_eq!(super::piece_rows(512, 512, 6, 256, 512 * 600 * 6), Some(512), "never more than the area's rows");
        assert_eq!(super::piece_rows(512, 512, 6, 256, 512 * 100 * 6), Some(100), "fewer than a tile's rows: as many as fit");
        assert_eq!(super::piece_rows(512, 512, 6, 256, 100), None);
        let rows = super::piece_rows(645, 984, 576, 512, crate::cmd::cube::MAX_VALUES).unwrap();
        assert!(rows < 984 && (rows.is_multiple_of(512) || rows < 512), "{rows}");
        assert_eq!(984u32.div_ceil(rows), 6);
    }

    /// Grids on one lattice of pixels line up by whole pixels (MODIS's tiles on its sinusoidal
    /// grid); another pixel size, or corners a fraction of a pixel apart, don't.
    #[test]
    fn files_line_up_on_one_grid() {
        let file = GeoTransform { origin_x: -5559752.598, origin_y: 0.0, pixel_width: 926.625433, pixel_height: -926.625433 };
        let grid = GeoTransform { origin_x: file.origin_x + 10.0 * 926.625433, origin_y: -3.0 * 926.625433, ..file };
        assert_eq!(super::offset_in(&grid, &file), Some((10, 3)));
        let above = GeoTransform { origin_y: 2.0 * 926.625433, ..grid };
        assert_eq!(super::offset_in(&above, &file), Some((10, -2)), "a grid above the file starts before its first row");
        assert_eq!(super::offset_in(&GeoTransform { origin_x: grid.origin_x + 400.0, ..grid }, &file), None);
        assert_eq!(super::offset_in(&GeoTransform { pixel_width: 463.3, ..grid }, &file), None);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::unwrap_used)]
mod deferred_asset_tests {
    use super::*;

    #[test]
    fn a_deferred_asset_download_keeps_its_connection_on_the_finished_layer() {
        let mut s = Session::new(crate::Project::default());
        let connection = s
            .run(
                "connection.new",
                &json!({
                    "kind": "stac", "name": "Public catalog", "auth": "none",
                    "address": "https://deferred757.example.org/stac"
                }),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let pending = s
            .run_leaving_downloads(
                "stac.addAsset",
                &json!({
                    "connection": connection, "asset": "data", "name": "Downloaded asset", "as": "not-forwarded",
                    "item": {"type": "Feature", "stac_version": "1.0.0", "id": "scene",
                        "geometry": {"type": "Point", "coordinates": [1, 2]},
                        "properties": {"datetime": "2025-01-01T00:00:00Z"},
                        "links": [], "assets": {"data": {
                            "href": "https://deferred757.example.org/data.geojson",
                            "type": "application/geo+json", "roles": ["data"]
                        }}}
                }),
            )
            .unwrap();
        assert_eq!(pending["pending"], true);
        let mut requests = s.take_url_requests();
        assert_eq!(requests.len(), 1);
        let request = requests.remove(0);
        let mut params = request.params().clone();
        let download = s.download_started_for(&request).unwrap();
        assert!(
            s.download_landed(
                download,
                json!({
                    "type": "FeatureCollection", "features": [{
                        "type": "Feature", "geometry": {"type": "Point", "coordinates": [1, 2]},
                        "properties": {"name": "landed"}
                    }]
                })
                .to_string()
                .into_bytes()
            )
        );
        params["id"] = json!(download);
        let added = s.complete_download(download, &params).unwrap();
        let layer = s.map().layer(added["layer"].as_u64().unwrap() as u32).unwrap();
        assert_eq!(layer.connection, connection);
        assert_eq!(layer.name, "Downloaded asset");
        assert_eq!(layer.dataset.features.len(), 1);
    }
}
