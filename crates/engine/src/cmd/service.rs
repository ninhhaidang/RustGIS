//! Web map and feature services: connect to a WMS, WMTS, WFS, WCS, OGC API Features or ArcGIS
//! REST service, list what it offers (from its capabilities), and add a layer — map layers as
//! tile layers, feature types and collections as data downloaded as GeoJSON (an ArcGIS layer in
//! protocol buffer pages when it lists them; the whole layer, or the features crossing an area),
//! coverages as rasters
//! (a GeoTIFF of the view from GetCoverage). And search a catalogue (CSW) for records and the
//! services and files they link to.
//!
//! The app fetches the capabilities and features in the background and passes the capabilities
//! here as `"text"`; the command line, MCP and the desktop assistant let these commands fetch
//! (waiting for the answer).

use rustgis_data::ogc::{Coverage, Offer, Service, SliderSteps};
use serde_json::{Value, json};

use super::ps;
use crate::{CmdResult, CommandSpec as C, Session};

#[path = "service_group.rs"]
mod group;

/// The total feature response bytes accepted by a grouped import or refresh.
pub const MAX_GROUP_BYTES: usize = 64 << 20;

pub fn commands() -> Vec<C> {
    let mut commands = vec![
        C::new("service.layers", "Service Layers", "Home › Add", layers).view().params(
            r#"{"url": "a WMS, WMTS or WFS address (GetCapabilities is added), an OGC API collections address or landing page (its map tiles and linked collections), or an ArcGIS FeatureServer, MapServer, ImageServer or VectorTileServer, or an ArcGIS services directory or folder (its folders and services, each listed by its own url)", "type"?: "wms"|"wmts"|"wfs"|"ogcapi"|"arcgis" (guessed from the address), "text"?: "the capabilities document, when already fetched", "documents"?: [{"url": "a discovered document address", "text": "its fetched JSON"}]} → layers with source_url, and next when supplied documents need another fetch"#,
        ),
        C::new("service.add", "Add Service Layer", "Home › Add", add).params(
            r#"{"url": "as service.layers", "type"?, "layer": "the layer's name (service.layers lists them)", "name"?: text, "time"?: "a time-enabled WMS or WMTS layer: by default (or \"slider\", true) it follows the time slider, turned on with the layer's times when it is off; or one of its dates (2024-06-01), or \"default\" (false): its service's default", "text"?: "the capabilities document, when already fetched", "documents"?: "additional fetched documents, as service.layers", "source_url"?: "the listed layer's source_url", "describe"?: "a WCS coverage's DescribeCoverage document, when already fetched", "tiles"?: "an OGC API layer's tile template, when already read from its tileset (the app's)", "bbox"?: [west, south, east, north] | "view" | "full": a WCS coverage's area (the view by default); for a WFS, OGC API or ArcGIS feature layer, only the features that cross it (the whole layer, "full", by default)}"#,
        ),
        C::new("service.coverage", "WCS Coverage Request", "Home › Add", coverage).view().params(
            r#"{"url": "a WCS address", "type"?: "wcs", "layer": "the coverage's name", "text"?: "the capabilities document, when already fetched", "documents"?: "additional fetched documents, as service.layers", "source_url"?: "the listed coverage's source_url", "describe"?: "its DescribeCoverage answer, when already fetched", "bbox"?: [west, south, east, north] | "view" (the default) | "full"} → the GetCoverage address of a GeoTIFF (layer.addUrl adds it)"#,
        ),
        C::new("catalog.search", "Search Catalogue", "Home › Add", catalog_search).view().params(
            r#"{"url": "a catalogue's CSW address", "query"?: "words anywhere in the records", "bbox"?: [west, south, east, north] | "view" (none by default), "start"?: "the first record's position (a page's next)", "text"?: "the GetRecords answer, when already fetched"} → records with their service and download links (service.add or layer.addUrl add them)"#,
        ),
    ];
    commands.extend(group::commands());
    commands
}

fn url_of(p: &Value) -> Result<&str, String> {
    let url = ps(p, "url").map(str::trim).filter(|u| !u.is_empty()).ok_or("url is required")?;
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err(format!("{url}: a service is given by its http:// or https:// address"));
    }
    Ok(url)
}

/// `p` with its ArcGIS service address's `?token=…` taken out and kept for the session (it then
/// goes, as the request's credential, to every request to that server: the service's layers, tiles and feature
/// pages), so the address the layer keeps has no token in it. `None` when there is none.
pub(crate) fn without_token(p: &Value) -> Result<Option<Value>, String> {
    use crate::render::rustgis_tiles::arcgis_auth;
    let Some(url) = ps(p, "url").map(str::trim) else { return Ok(None) };
    let has_token = url
        .split_once('?')
        .is_some_and(|(_, q)| q.split('&').any(|kv| kv.split('=').next().is_some_and(|k| k.eq_ignore_ascii_case("token"))));
    // ArcGIS REST addresses only: another kind of service's `token` stays in its address.
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    let arcgis = match ps(p, "type") {
        Some("arcgis") => true,
        None | Some("auto") => path.contains("/rest/services") || path.contains("/sharing/rest"),
        Some(_) => false,
    };
    if !has_token || !arcgis || arcgis_auth::server_root(url).is_none() {
        return Ok(None);
    }
    let before = arcgis_auth::authorization(url);
    let mut owned = p.clone();
    owned["url"] = json!(arcgis_auth::adopt_url_token(url)?);
    // What was cached unauthorized (refused tiles) is asked for again with the new token.
    if arcgis_auth::authorization(url) != before {
        crate::render::rustgis_tiles::cache::clear();
    }
    Ok(Some(owned))
}

/// Capability discovery is shared by supplied-document GUI requests and native commands.
/// Sources stay alongside layers so a landing page and a collection may use the same name.
#[derive(Default)]
struct Discovery {
    service: Service,
    sources: Vec<String>,
    next: Option<String>,
    error: Option<String>,
    excluded_layers: bool,
    /// A document left collections out (past its listing limit, or unreadable items links).
    incomplete: bool,
}

impl Discovery {
    fn failed(mut self, error: String) -> Result<Self, String> {
        if self.service.layers.is_empty() {
            Err(error)
        } else {
            self.error = Some(error.clone());
            self.service.notes.push(error);
            self.next = None;
            Ok(self)
        }
    }
}

const MAX_SERVICE_DOCUMENTS: usize = 4;

fn capabilities(p: &Value) -> Result<Discovery, String> {
    let original = url_of(p)?;
    let mut address = rustgis_data::ogc::capabilities_url(original, ps(p, "type"));
    let supplied = ps(p, "text");
    let documents = p.get("documents").map(|v| v.as_array().ok_or("documents must be an array")).transpose()?;
    if let Some(docs) = documents {
        if supplied.is_none() || docs.len() >= MAX_SERVICE_DOCUMENTS {
            return Err("documents requires text and at most three additional documents".into());
        }
        for doc in docs {
            if ps(doc, "url").is_none() || ps(doc, "text").is_none() {
                return Err("each supplied document requires url and text".into());
            }
        }
    }
    let mut result = Discovery::default();
    let mut seen = std::collections::HashSet::new();
    let mut parent = original.to_string();
    loop {
        if let Err(e) = crate::render::rustgis_tiles::address::check(&address, Some(&parent)) {
            return result.failed(e);
        }
        if seen.contains(&address) {
            return result.failed("the service's collections links form a loop".into());
        }
        if seen.len() >= MAX_SERVICE_DOCUMENTS {
            return result.failed("the service's collections discovery exceeds four documents".into());
        }
        let parsed = if let Some(root) = supplied {
            let text = if seen.is_empty() {
                Some(root)
            } else {
                documents.and_then(|docs| docs.iter().find(|d| ps(d, "url") == Some(address.as_str()))).and_then(|d| ps(d, "text"))
            };
            let Some(text) = text else {
                result.next = Some(address);
                return Ok(result);
            };
            rustgis_data::ogc::parse(&address, text.as_bytes())
        } else {
            fetch_capabilities(original, &address)
        };
        let service = match parsed {
            Ok(s) => {
                let before = s.layers.len();
                result.incomplete |= s.incomplete;
                let vetted = vet(s, &address);
                result.excluded_layers |= vetted.layers.len() != before;
                vetted
            }
            Err(e) => return result.failed(e),
        };
        seen.insert(address.clone());
        if result.service.kind.is_empty() {
            result.service.kind = service.kind.clone();
            result.service.title = service.title.clone();
        }
        // Parsers bound each document's collections/layers; visited documents bound the merge.
        // Distinct offers may share a name (including generated tile names and collection ids).
        for layer in service.layers {
            result.service.layers.push(layer);
            result.sources.push(address.clone());
        }
        result.service.notes.extend(service.notes);
        let features = result.service.layers.iter().any(|l| matches!(l.offer, Offer::Features { .. }));
        let tiles = result.service.layers.iter().any(|l| matches!(l.offer, Offer::Tiles { .. } | Offer::VectorTiles { .. }));
        if result.service.kind.starts_with("OGC API") {
            result.service.kind = match (features, tiles) {
                (true, true) => "OGC API Features and Tiles",
                (_, true) => "OGC API Tiles",
                _ => "OGC API Features",
            }
            .into();
        }
        let Some(next) = service.collections_url else { return Ok(result) };
        parent = address;
        address = next;
    }
}

#[cfg(target_arch = "wasm32")]
fn fetch_capabilities(url: &str, _caps: &str) -> Result<Service, String> {
    Err(format!("{url}: in the web app, use Home › Add › Add Data › From Service"))
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch_capabilities(url: &str, caps: &str) -> Result<Service, String> {
    let bytes = crate::render::rustgis_tiles::cache::download_blocking(caps).map_err(|e| format!("{url}: {e}"))?;
    rustgis_data::ogc::parse(caps, &bytes).map_err(|e| format!("{url}: {e}"))
}

/// Leaves out the layers whose addresses a document on the web may not send RustGIS to: the
/// capabilities are hostile input, and their endpoints and links must not reach into the local
/// network (unless the service itself is there) or a forbidden address.
fn vet(mut svc: Service, caps: &str) -> Service {
    let before = svc.layers.len();
    svc.layers.retain(|l| {
        let u = match &l.offer {
            Offer::Tiles { template } | Offer::VectorTiles { template, .. } => template,
            Offer::Features { url } | Offer::Style { url } | Offer::Service { url } => url,
            Offer::Coverage(Coverage { endpoint, .. }) => endpoint,
        };
        crate::render::rustgis_tiles::address::check(u, Some(caps)).is_ok()
    });
    let dropped = before - svc.layers.len();
    if dropped > 0 {
        svc.notes.push(format!("{dropped} layers point to addresses RustGIS won't fetch for a service on the web and are left out"));
    }
    svc
}

/// A service's GeoJSON answer made ready to read: latitude-first coordinates turned round, judged
/// against the layer's longitude/latitude `bounds` from its capabilities (see
/// [`fix_axes`](rustgis_data::ogc::fix_axes); none: left as they come), and a cut-off answer
/// pointed out.
pub fn features_answer(s: &mut Session, name: &str, bytes: Vec<u8>, bounds: Option<[f64; 4]>) -> Vec<u8> {
    if let Some(note) = rustgis_data::ogc::partial_note(&bytes) {
        s.notify(format!("{name}: {note}"), false);
    }
    rustgis_data::ogc::fix_axes(&bytes, bounds).unwrap_or(bytes)
}

/// The layers an add's result `r` names (`layer`, `layers`) marked as read from the service
/// address `url`, not the file name their download was read under: Add From Service then knows
/// them as added, to remove.
pub(crate) fn mark_service_source(s: &mut Session, r: &Value, url: &str) {
    let ids: Vec<u32> = r
        .get("layers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(r.get("layer"))
        .filter_map(Value::as_u64)
        .filter_map(|i| u32::try_from(i).ok())
        .collect();
    for id in ids {
        if let Some(l) = s.map_mut().layer_mut(id) {
            l.dataset.source = Some(url.to_string());
        }
    }
}

/// A layer's longitude/latitude bounds as a listing or a parameter gives them (`[west, south,
/// east, north]`), for [`features_answer`]: none when absent or null.
pub(crate) fn bounds_of(v: Option<&Value>) -> Result<Option<[f64; 4]>, String> {
    let Some(v) = v.filter(|v| !v.is_null()) else { return Ok(None) };
    let b: [f64; 4] = serde::Deserialize::deserialize(v).map_err(|_| "bounds must be [west, south, east, north]")?;
    if !rustgis_map::tiles::valid_bounds(&b) {
        return Err("bounds must be a finite, ordered geographic rectangle".into());
    }
    Ok(Some(b))
}

/// A service's features at `url`, with the pages that follow (its `next` links, or an ArcGIS
/// layer's pages, its description read first, see [`begin`](rustgis_data::ogc::begin)): at most
/// `max` bytes of features. The description waits at most
/// [`DESCRIPTION_TIMEOUT`](rustgis_data::ogc::DESCRIPTION_TIMEOUT) and takes at most
/// [`DESCRIPTION_MAX`](rustgis_data::ogc::DESCRIPTION_MAX) bytes, apart; the features
/// [`BLOCKING_TIMEOUT`](crate::render::rustgis_tiles::cache::BLOCKING_TIMEOUT) in all after it
/// (75 s at most). A page is fetched only where the page before it may point.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn download_features(s: &mut Session, name: &str, url: &str, max: usize) -> Result<Vec<u8>, String> {
    match download_layer(s, name, url, max, false)? {
        Downloaded::Bytes(bytes) => Ok(bytes),
        // Not given without protocol buffer pages.
        Downloaded::Read(..) => Err(format!("{url}: the pages were read as protocol buffers")),
    }
}

/// A service's features as [`download_features`] downloads them.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) enum Downloaded {
    /// Its answer, as it came (or its pages joined).
    Bytes(Vec<u8>),
    /// An ArcGIS layer's protocol buffer pages, read and joined (see
    /// [`begin_with_pbf`](rustgis_data::ogc::begin_with_pbf)).
    Read(crate::LandedRead),
}

/// A service's features at `url`, as [`download_features`] downloads them; with `pbf`, an ArcGIS
/// layer whose description lists protocol buffers is read in them, in pages.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn download_layer(s: &mut Session, name: &str, url: &str, max: usize, pbf: bool) -> Result<Downloaded, String> {
    use crate::render::rustgis_tiles::{address, cache};
    use rustgis_data::ogc::{DESCRIPTION_MAX, DESCRIPTION_TIMEOUT, Step, begin, begin_with_pbf, max_paged_features};
    let mut until = std::time::Instant::now() + cache::BLOCKING_TIMEOUT;
    let mut left = max;
    let mut step = if pbf { begin_with_pbf(url, max_paged_features()) } else { begin(url, max_paged_features()) };
    let (downloaded, note) = loop {
        let pages = match step {
            Step::Done(bytes, note) => break (Downloaded::Bytes(bytes), note),
            Step::Decoded(dataset, report, note) => break (Downloaded::Read(Box::new((*dataset, report))), note),
            // A first answer that timed out says how to add a large layer instead.
            Step::Failed(e) => return Err(format!("{url}: {}", cache::large_file_hint(e))),
            Step::Fetch(pages) => pages,
        };
        let describing = pages.describing();
        let answer = address::check(pages.next_url(), Some(pages.referrer())).and_then(|()| {
            let wait = until.saturating_duration_since(std::time::Instant::now());
            // A layer's description is small: one that hangs is given up on, and its features
            // asked for as they are.
            let wait = if describing { wait.min(DESCRIPTION_TIMEOUT) } else { wait };
            // Said as a timeout, so its error says how to add the layer instead.
            if wait.is_zero() {
                return Err("the pages timed out".to_string());
            }
            // A description has a small budget of its own.
            let budget = if describing { left.min(DESCRIPTION_MAX) } else { left };
            let page = cache::download_blocking_within(pages.next_url(), budget, wait)?;
            if page.len() > left {
                return Err("the pages are larger than RustGIS downloads".into());
            }
            // The description isn't the features: it leaves them the whole budget.
            if !describing {
                left -= page.len();
            }
            Ok(page)
        });
        step = (*pages).land(answer);
        // Nor their time: the features have all of it from when the description is in.
        if describing {
            until = std::time::Instant::now() + cache::BLOCKING_TIMEOUT;
        }
    };
    if let Some(note) = note {
        s.notify(format!("{name}: {note}"), false);
    }
    Ok(downloaded)
}

/// A tile template given for a service's layer: a web address with `{z}`, `{x}` and `{y}`, that
/// the service's document may point to.
fn checked_template(service: &str, t: &str) -> Result<String, String> {
    let t = t.trim();
    if !(t.contains("{z}") && t.contains("{x}") && t.contains("{y}")) || t.len() > 2000 {
        return Err(format!("{t}: not a tile template"));
    }
    crate::render::rustgis_tiles::address::check(t, Some(service))?;
    Ok(t.to_string())
}

/// The tile template an OGC API tileset list advertises (its WebMercatorQuad tileset's), read now
/// (the list, then the tileset when the list doesn't carry it). `None` when it can't be read.
#[cfg(not(target_arch = "wasm32"))]
fn advertised_template(list: &str, vector: bool) -> Option<String> {
    use rustgis_data::ogc::{TilesStep, resolve_tiles};
    let mut doc = list.to_string();
    for _ in 0..2 {
        crate::render::rustgis_tiles::address::check(&doc, Some(list)).ok()?;
        let body = crate::render::rustgis_tiles::cache::download_blocking(&doc).ok()?;
        match resolve_tiles(list, &doc, &body, vector)? {
            TilesStep::Template(t) => return checked_template(list, &t).ok(),
            TilesStep::Next(next) => doc = next,
        }
    }
    None
}

/// In the browser nothing waits: the app reads the template itself and passes it as `"tiles"`.
#[cfg(target_arch = "wasm32")]
fn advertised_template(_: &str, _: bool) -> Option<String> {
    None
}

/// Of a layer's times, those `service.layers` lists from each end of a long list.
const SHOWN_TIMES: usize = 12;

/// A service as the app and other callers see it.
pub fn service_json(svc: &Service) -> Value {
    let layers: Vec<Value> = svc
        .layers
        .iter()
        .map(|l| {
            let offer = match &l.offer {
                Offer::Tiles { template } => json!({"type": "tiles", "url": template}),
                Offer::Features { url } => json!({"type": "features", "url": url}),
                Offer::Style { url } => json!({"type": "style", "url": url}),
                Offer::VectorTiles { template, layers } => json!({"type": "vectorTiles", "url": template, "layers": layers}),
                Offer::Coverage(c) => json!({"type": "coverage", "url": c.endpoint, "describe": rustgis_data::ogc::describe_coverage_url(c), "version": c.version}),
                Offer::Service { url } => json!({"type": "service", "url": url}),
            };
            let mut layer = json!({"name": l.name, "title": l.title, "bbox": l.bbox, "offer": offer, "tilesets": l.tilesets});
            if let Some((min, max)) = l.zooms {
                layer["minZoom"] = json!(min);
                layer["maxZoom"] = json!(max);
            }
            // A layer that lists one time is listed as a plain map: there is no other to pick.
            if let Some(t) = l.time.as_ref().filter(|t| t.changes()) {
                // The first and last few of a long list, and how many (the slider takes them all).
                let values: Vec<&String> = match t.values.len() {
                    n if n > 2 * SHOWN_TIMES => t.values.iter().take(SHOWN_TIMES).chain(t.values.iter().skip(n - SHOWN_TIMES)).collect(),
                    _ => t.values.iter().collect(),
                };
                layer["time"] = json!({"default": t.default, "values": values, "count": t.values.len(), "multiple": t.multiple});
            }
            layer
        })
        .collect();
    json!({"kind": svc.kind, "title": svc.title, "count": layers.len(), "layers": layers, "notes": svc.notes})
}

pub(crate) fn layers(_s: &mut Session, p: &Value) -> CmdResult {
    let owned = without_token(p)?;
    let p = owned.as_ref().unwrap_or(p);
    let found = capabilities(p)?;
    let mut out = service_json(&found.service);
    if let Some(layers) = out.get_mut("layers").and_then(Value::as_array_mut) {
        for (layer, source) in layers.iter_mut().zip(found.sources) {
            layer["source_url"] = json!(source);
        }
    }
    if let Some(next) = found.next {
        out["next"] = json!(next);
    }
    Ok(out)
}

/// Adds one of a service's layers: map layers as tiles (an overlay), a style as a style, features
/// downloaded.
pub(crate) fn add(s: &mut Session, p: &Value) -> CmdResult {
    let owned = without_token(p)?;
    let p = owned.as_ref().unwrap_or(p);
    let url = url_of(p)?.to_string();
    let want = ps(p, "layer").map(str::trim).filter(|l| !l.is_empty()).ok_or("layer is required")?;
    let found = capabilities(p)?;
    let candidates =
        || found.service.layers.iter().zip(&found.sources).filter(|(_, source)| ps(p, "source_url").is_none_or(|s| s == source.as_str()));
    let (layer, source) = candidates()
        .find(|(l, _)| l.name == want)
        .or_else(|| candidates().find(|(l, _)| l.name.eq_ignore_ascii_case(want) || l.title.eq_ignore_ascii_case(want)))
        .ok_or_else(|| {
            // A layer the listing left out, and why (a collection of 3D Tiles, say).
            let quoted = format!("'{}'", want.to_lowercase());
            let left_out = |n: &&String| n.contains(rustgis_data::ogc::LEFT_OUT_3D) && n.to_lowercase().contains(&quoted);
            match found.service.notes.iter().find(left_out) {
                Some(why) => format!("{url}: no layer '{want}': {why}"),
                None => format!("{url}: no layer '{want}' (service.layers lists them)"),
            }
        })?;
    // A time-enabled layer follows the time slider, at each of its steps, unless a time is asked
    // for: one of its times (the layer named with it), or "default" (`false`), the service's.
    // (`"time": true`, the slider, as for a Zarr variable, `layer.addZarr`'s.)
    // A year given as a number is that time; an empty one, as before, the service's default.
    let given: Option<String> = match p.get("time") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(true)) => Some("slider".into()),
        Some(Value::Bool(false)) => Some("default".into()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::String(t)) if t.trim().is_empty() => Some("default".into()),
        Some(Value::String(t)) => Some(t.trim().to_string()),
        Some(_) => return Err(format!("{}: time is a date, \"slider\" or \"default\"", layer.title)),
    };
    let asked = given.as_deref();
    let pinned = asked.is_some_and(|a| a.eq_ignore_ascii_case("default"));
    let slider = match (&layer.time, asked) {
        (Some(t), Some(a)) if a.eq_ignore_ascii_case("slider") => Some(t.slider().ok_or_else(|| match t.changes() {
            true => format!("{}: its times aren't dates the time slider can step to (give one of them as time)", layer.title),
            false => format!(
                "{}: it lists only one time, {}: nothing for the time slider to step through",
                layer.title,
                t.values.first().unwrap_or(&t.default)
            ),
        })?),
        (None, Some(a)) if a.eq_ignore_ascii_case("slider") => {
            return Err(format!("{}: the layer has no time to follow the time slider with", layer.title));
        }
        // None asked: on the slider when its times are dates it steps to, at its default if not.
        (Some(t), None) => t.slider(),
        _ => None,
    };
    let asked = asked.filter(|_| slider.is_none() && !pinned);
    let time = match (&layer.time, asked) {
        // A WMS layer's: a list of times or an interval when it takes several (WMS 1.3.0 Annex C).
        (Some(l), Some(t)) if l.multiple => Some(rustgis_data::ogc::checked_wms_time(t)?),
        (Some(_), Some(t)) => Some(rustgis_data::ogc::checked_time(t)?),
        (Some(_), None) if slider.is_some() => None,
        (Some(t), None) => Some(t.default.clone()),
        (None, Some(_)) => return Err(format!("{}: the layer has no time to pick (leave time out)", layer.title)),
        (None, None) => None,
    };
    let title = match (&time, asked) {
        (Some(t), Some(_)) => format!("{} {t}", layer.title),
        _ => layer.title.clone(),
    };
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&title).to_string();
    let zoom = |s: &mut Session| {
        if let Some([w, so, e, n]) = layer.bbox {
            s.zoom_to(&rustgis_geom::Extent::new(w, so, e, n));
        }
    };
    // An OGC API layer's tiles: the template its tileset advertises (given, or read now), else
    // the usual path.
    let advertised = match (&layer.tilesets, ps(p, "tiles")) {
        (Some(_), Some(t)) => Some(checked_template(source, t)?),
        (Some(list), None) => advertised_template(list, matches!(layer.offer, Offer::VectorTiles { .. })),
        _ => None,
    };
    match &layer.offer {
        Offer::Tiles { template } => {
            let template = advertised.as_deref().unwrap_or(template);
            let template = match (&time, &slider) {
                // Its time written as the layer writes its times, filled at each step.
                (_, Some(st)) => template.replace("{Time}", &format!("{{date:{}}}", st.format)),
                (Some(t), None) => rustgis_data::ogc::with_time(template, t),
                (None, None) => template.to_string(),
            };
            // With its legends, as its service gives them (none: no key, as before).
            let mut add = json!({"url": template, "kind": "raster", "name": name, "overlay": true, "bounds": layer.bbox});
            // A layer its service draws on request (a WMS, an ArcGIS map service) is asked for
            // as one picture of the view, so its symbols and labels aren't cut at tile edges
            // (#1162); `map.setTiles` turns it back to tiles.
            if rustgis_map::tiles::drawn_on_request(&template) {
                add["singleImage"] = json!(true);
            }
            // The zooms its service draws it at (a WMS layer's scale hints, a WMTS layer's matrices):
            // closer in, its finest tiles are stretched rather than asked for.
            if let Some((min, max)) = layer.zooms {
                add["minZoom"] = json!(min);
                add["maxZoom"] = json!(max);
            }
            // The time slider, when it is off, turned on with the layer's steps (one that is on
            // keeps its own); it starts at the layer's default.
            let fresh = s.map().time.is_none();
            if let Some(st) = slider.as_ref().filter(|_| fresh) {
                match &st.steps {
                    SliderSteps::Dates(dates) => add["dates"] = json!(dates),
                    SliderSteps::Every { start, end, unit, interval } => {
                        add["start"] = json!(start);
                        add["end"] = json!(end);
                        add["unit"] = json!(unit.code());
                        add["interval"] = json!(interval);
                    }
                }
            }
            // (Those the layer wouldn't keep, an address too long, are left out, not refused.)
            let legends = rustgis_map::tiles::checked_legends(layer.legends.iter().cloned());
            if !legends.is_empty() {
                add["legend"] = json!(legends);
            }
            let mut r = super::tiles::add(s, &add)?;
            // A WMS layer's GetFeatureInfo request, for identify.
            let added = r["index"].as_u64().and_then(|i| usize::try_from(i).ok());
            if let Some(t) = added.and_then(|i| s.map_mut().tiles.get_mut(i)) {
                t.feature_info = layer.feature_info.clone();
            }
            if let Some(at) = slider.as_ref().filter(|_| fresh).and_then(|st| st.at) {
                super::time::set(s, &json!({"date": at}))?;
                // Its address at the step the slider went to.
                if let Some(t) = added.and_then(|i| s.map().tiles.get(i)) {
                    r["url"] = json!(t.url);
                }
            }
            // A slider already on keeps its steps: the service may have no map for some of them.
            let current = s.map().time.as_ref().map(|t| t.current);
            if let (Some(st), Some(now), false) = (&slider, current, fresh)
                && !st.has(now)
            {
                s.notify(
                    format!(
                        "{name}: follows the time slider's steps, and its service lists other times, so it may draw nothing at some of them"
                    ),
                    false,
                );
            }
            zoom(s);
            Ok(r)
        }
        Offer::VectorTiles { template, layers } => {
            let template = advertised.as_deref().unwrap_or(template);
            let r = super::tiles::add(
                s,
                &json!({"url": template, "kind": "vector", "name": name, "layers": layers, "overlay": true, "bounds": layer.bbox}),
            )?;
            zoom(s);
            Ok(r)
        }
        Offer::Style { url: style } => {
            let r = super::tiles::add(s, &json!({"url": style, "kind": "style", "name": name, "bounds": layer.bbox}))?;
            zoom(s);
            Ok(r)
        }
        Offer::Service { url: open } => {
            Err(format!("{url}: '{want}' is a folder or service the directory lists: list its layers (service.layers with url {open})"))
        }
        Offer::Coverage(c) => {
            let (get, _) = coverage_request(s, p, c)?;
            #[cfg(target_arch = "wasm32")]
            {
                let _ = get;
                Err(format!("{url}: in the web app, Home › Add › Add Data › From Service downloads the coverage"))
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let bytes = crate::render::rustgis_tiles::cache::download_blocking(&get).map_err(|e| format!("{name}: {e}"))?;
                coverage_check(&name, &bytes)?;
                let r = super::raster::add_named_bytes(s, &json!({"name": name}), format!("{name}.tif"), bytes)?;
                mark_service_source(s, &r, &get);
                Ok(r)
            }
        }
        Offer::Features { url: items } => {
            let area = features_area(s, p)?;
            let asked = area.and_then(|a| rustgis_data::ogc::features_in_area(items, a));
            #[cfg(target_arch = "wasm32")]
            {
                let _ = (items, asked);
                Err(format!("{url}: in the web app, Home › Add › Add Data › From Service downloads the features"))
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                // An area asked for, whether or not the request could carry it: a slow whole layer
                // then says so, and doesn't suggest the `bbox` already given.
                let given = p.get("bbox").is_some_and(|b| !b.is_null() && *b != "full");
                if given && asked.is_none() {
                    let why = if area.is_none() { "an area across the antimeridian" } else { "its request can't carry an area" };
                    s.notify(format!("{name}: {why}, so the whole layer is asked for"), false);
                }
                let max = crate::render::rustgis_tiles::cache::MAX_DOWNLOAD;
                let from = asked.as_deref().unwrap_or(items);
                let mut r = match download_layer(s, &name, from, max, true).map_err(|e| slow_whole_layer(e, given))? {
                    Downloaded::Bytes(bytes) => {
                        let bytes = features_answer(s, &name, bytes, layer.bbox);
                        super::raster::add_named_bytes(s, &json!({"name": name}), format!("{name}.geojson"), bytes)?
                    }
                    // Read in protocol buffer pages: their positions as the service's system
                    // places them, so there are no axes to check.
                    Downloaded::Read(read) => super::layer::add_read(s, &json!({"name": name}), *read)?,
                };
                mark_service_source(s, &r, from);
                // Over an area, the features crossing it: the view stays, or goes to a box given
                // (the whole layer's bounds would show a country to fill one state).
                match (asked.is_some(), area, p.get("bbox")) {
                    (true, Some([w, so, e, n]), Some(Value::Array(_))) => s.zoom_to(&rustgis_geom::Extent::new(w, so, e, n)),
                    (true, ..) => {}
                    _ => zoom(s),
                }
                if let (Some(_), Some(out)) = (&asked, r.as_object_mut()) {
                    out.insert("area".into(), json!(area));
                }
                Ok(r)
            }
        }
    }
}

/// The area a service's features are asked for over: a `bbox` given (`[west, south, east,
/// north]`, or `"view"`, the map's), else none, for the whole layer (`"full"` says so). A box
/// across the antimeridian asks for the whole layer too, as `catalog.search` does.
fn features_area(s: &Session, p: &Value) -> Result<Option<[f64; 4]>, String> {
    match p.get("bbox") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(f)) if f == "full" => Ok(None),
        Some(_) => Ok(Some(super::catalogs::area(s, p)?).filter(|a| a[0] <= a[2])),
    }
}

/// A whole layer too slow to download while a command waits: the error says an area can be asked
/// for instead, unless one was (`area_given`).
#[cfg(not(target_arch = "wasm32"))]
fn slow_whole_layer(e: String, area_given: bool) -> String {
    if !area_given && (e.contains("timeout") || e.contains("timed out")) {
        format!("{e}; or ask for the features in an area only, with \"bbox\": [west, south, east, north] or \"view\"")
    } else {
        e
    }
}

/// The area a coverage is asked for over: `bbox` given, `"full"` (its bounds), or the map's view
/// (the default); within its bounds when they are known.
fn coverage_area(s: &Session, p: &Value, info: &rustgis_data::ogc::CoverageInfo, id: &str) -> Result<[f64; 4], String> {
    let area = match p.get("bbox") {
        Some(Value::String(f)) if f == "full" => {
            info.bbox.ok_or_else(|| format!("{id}: its bounds aren't known; give bbox or use the view"))?
        }
        _ => super::catalogs::area(s, p)?,
    };
    if area[0] > area[2] {
        return Err(format!("{id}: an area across the antimeridian; give it one side at a time"));
    }
    rustgis_data::ogc::coverage_area(info, area).ok_or_else(|| format!("{id}: the coverage has no data in the area"))
}

/// A coverage's GetCoverage address and the area it covers: its description from `"describe"`, or
/// fetched (waiting for it).
fn coverage_request(s: &Session, p: &Value, c: &Coverage) -> Result<(String, [f64; 4]), String> {
    let describe = rustgis_data::ogc::describe_coverage_url(c);
    let info = match ps(p, "describe") {
        Some(text) => rustgis_data::ogc::parse_describe(c, text.as_bytes())?,
        None => fetch_describe(c, &describe)?,
    };
    let area = coverage_area(s, p, &info, &c.id)?;
    let get = rustgis_data::ogc::get_coverage_url(c, &info, area);
    crate::render::rustgis_tiles::address::check(&get, Some(&c.endpoint))?;
    Ok((get, area))
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch_describe(c: &Coverage, describe: &str) -> Result<rustgis_data::ogc::CoverageInfo, String> {
    let bytes = crate::render::rustgis_tiles::cache::download_blocking(describe).map_err(|e| format!("{}: {e}", c.id))?;
    rustgis_data::ogc::parse_describe(c, &bytes)
}

#[cfg(target_arch = "wasm32")]
fn fetch_describe(c: &Coverage, _: &str) -> Result<rustgis_data::ogc::CoverageInfo, String> {
    Err(format!("{}: in the web app, the description is read first (give it as \"describe\")", c.id))
}

/// `service.coverage`: the GetCoverage address of a WCS coverage over an area, for the app (which
/// downloads it in the background) and for scripts.
fn coverage(s: &mut Session, p: &Value) -> CmdResult {
    let url = url_of(p)?.to_string();
    let want = ps(p, "layer").map(str::trim).filter(|l| !l.is_empty()).ok_or("layer is required")?;
    let found = capabilities(p)?;
    let layer = found
        .service
        .layers
        .iter()
        .zip(&found.sources)
        .filter(|(_, source)| ps(p, "source_url").is_none_or(|s| s == source.as_str()))
        .map(|(layer, _)| layer)
        .find(|l| l.name == want || l.name.eq_ignore_ascii_case(want))
        .ok_or_else(|| format!("{url}: no coverage '{want}' (service.layers lists them)"))?;
    let Offer::Coverage(c) = &layer.offer else { return Err(format!("{url}: '{want}' isn't a WCS coverage")) };
    let (get, area) = coverage_request(s, p, c)?;
    Ok(json!({"url": get, "name": layer.title, "bbox": area, "describe": rustgis_data::ogc::describe_coverage_url(c)}))
}

/// The GetRecords request `catalog.search` makes of its parameters (the app fetches it first).
pub fn records_request(s: &Session, p: &Value) -> Result<String, String> {
    let url = url_of(p)?;
    let words = ps(p, "query").unwrap_or("");
    let bbox = match p.get("bbox") {
        None | Some(Value::Null) => None,
        // A box across the antimeridian isn't asked for: the whole catalogue is searched.
        Some(_) => Some(super::catalogs::area(s, p)?).filter(|a| a[0] <= a[2]),
    };
    let start = p.get("start").and_then(Value::as_u64).unwrap_or(1).clamp(1, 1_000_000);
    Ok(rustgis_data::ogc::records_url(url, words, bbox, start))
}

/// A WCS coverage's answer is a GeoTIFF; anything else (an XML exception, an HTML page, plain
/// text) is the service's error, its start reported.
pub(crate) fn coverage_check(name: &str, bytes: &[u8]) -> Result<(), String> {
    // TIFF's and BigTIFF's byte-order marks with their magic numbers.
    if [&b"II*\0"[..], b"MM\0*", b"II+\0", b"MM\0+"].iter().any(|m| bytes.starts_with(m)) {
        return Ok(());
    }
    let text = String::from_utf8_lossy(bytes.get(..bytes.len().min(400)).unwrap_or_default()).into_owned();
    Err(format!("{name}: the service answered with an error: {text}"))
}

/// `catalog.search`: a page of a catalogue's (CSW) records matching words and an area, with the
/// links RustGIS can follow (those pointing where a document on the web may not send it left out).
pub(super) fn catalog_search(s: &mut Session, p: &Value) -> CmdResult {
    let url = url_of(p)?;
    let request = records_request(s, p)?;
    let page = match ps(p, "text") {
        Some(text) => rustgis_data::ogc::parse_records(&request, text.as_bytes()).map_err(|e| format!("{url}: {e}"))?,
        None => fetch_records(url, &request)?,
    };
    let mut dropped = 0usize;
    let records: Vec<Value> = page
        .records
        .iter()
        .map(|r| {
            let links: Vec<Value> = r
                .links
                .iter()
                .filter(|l| {
                    let ok = crate::render::rustgis_tiles::address::check(&l.url, Some(&request)).is_ok();
                    dropped += usize::from(!ok);
                    ok
                })
                .map(|l| json!({"url": l.url, "kind": l.kind, "name": l.name, "protocol": l.protocol}))
                .collect();
            json!({"id": r.id, "title": r.title, "abstract": r.summary, "bbox": r.bbox, "links": links})
        })
        .collect();
    let mut out = json!({"matched": page.matched, "count": records.len(), "next": page.next, "records": records, "request": request});
    if dropped > 0 {
        out["notes"] =
            json!([format!("{dropped} links point to addresses RustGIS won't fetch for a catalogue on the web and are left out")]);
    }
    Ok(out)
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch_records(url: &str, request: &str) -> Result<rustgis_data::ogc::RecordPage, String> {
    let bytes = crate::render::rustgis_tiles::cache::download_blocking(request).map_err(|e| format!("{url}: {e}"))?;
    rustgis_data::ogc::parse_records(request, &bytes).map_err(|e| format!("{url}: {e}"))
}

#[cfg(target_arch = "wasm32")]
fn fetch_records(url: &str, _: &str) -> Result<rustgis_data::ogc::RecordPage, String> {
    Err(format!("{url}: in the web app, use Home › Add › Add Data › From Service › Search Catalogue"))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    /// A whole layer past the wait is pointed to `bbox`, unless a `bbox` was given (one the request
    /// couldn't carry, a WFS 1.0's or across the antimeridian); other errors are left as they are.
    #[test]
    fn a_slow_whole_layer_suggests_bbox_only_when_none_was_given() {
        let slow = "https://x.org/wfs: timed out reading the answer".to_string();
        assert!(super::slow_whole_layer(slow.clone(), false).ends_with("with \"bbox\": [west, south, east, north] or \"view\""));
        assert_eq!(super::slow_whole_layer(slow.clone(), true), slow);
        assert_eq!(super::slow_whole_layer("404 Not Found".into(), false), "404 Not Found");
    }
}
