//! Identify on WMS layers: what a service says is at a point (GetFeatureInfo). Identify lists the
//! requests for the WMS tile layers drawn there; the app fetches them in the background and passes
//! each answer to `identify.featureInfo` as `"text"`; the command line, MCP and the assistant let
//! `identify.featureInfo` fetch (waiting for the answer).
//!
//! A layer added from the service knows its request from the capabilities; one from a project
//! (which doesn't keep it) is asked for its service's capabilities first.

use rustgis_data::ogc::{self, FeatureInfo, InfoFeature};
use rustgis_geom::Coord;
use rustgis_map::tiles::TileKind;
use serde_json::{Value, json};

use super::super::{b, f, ps};
use crate::{CmdResult, CommandSpec as C, Session};

/// Most WMS layers one identify asks.
const MAX_ASKED: usize = 8;
/// Most bytes of an answer or capabilities passed as text: what may be downloaded, as UTF-8 (a
/// Latin-1 answer grows to at most twice its size).
const MAX_TEXT: usize = 2 * ogc::MAX_INFO_BYTES;

pub(super) fn commands() -> Vec<C> {
    vec![
        C::new("identify.featureInfo", "Identify WMS Layer", "Home › Inspect", feature_info).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).view().params(
            r#"{"layer": "a WMS tile layer's name", "lon"?, "lat"?: the point (for a request), "text"?: "the GetFeatureInfo answer, when already fetched (identify lists the requests)", "url"?: "the request text answers: with lon and lat, the service's error in its format gives the request in the next format the layer offers ({url}), and features answered in a later format keep the layer asking in it", "capabilities"?: "the service's capabilities document, when the layer doesn't know its request (identify then asks for it)", "request"?: bool (only say what to fetch next: {url} or {capabilities})} → results, as identify's (with "tileLayer"), or with request or after an error, what to fetch"#,
        ),
    ]
}

/// Web Mercator metres per pixel at a point, as tiles are drawn (in another projection, the
/// distance of a pixel there; in Web Mercator, the camera's resolution), and the point there.
pub(super) fn merc_at(s: &Session, lon: f64, lat: f64) -> Option<(Coord, f64)> {
    let map = s.map();
    if !(lon.is_finite() && lat.is_finite()) {
        return None;
    }
    let at = rustgis_geom::lonlat_to_merc(Coord::new(lon, lat.clamp(-85.06, 85.06)));
    let res = map.camera.res;
    let m_per_px = map
        .to_map(Coord::new(lon, lat))
        .and_then(|m| map.to_lonlat(Coord::new(m.x + res, m.y)))
        .map(|c| rustgis_geom::lonlat_to_merc(Coord::new(c.x, c.y.clamp(-85.06, 85.06))))
        .map(|b| (b.x - at.x).hypot(b.y - at.y))
        .filter(|d| d.is_finite() && *d > 0.0)
        .unwrap_or(res);
    Some((at, m_per_px))
}

/// Whether a tile layer is a WMS layer that can be asked: drawn, and queryable or not known yet.
/// One `picked` in the Layers pane is asked whichever it is.
fn askable(t: &rustgis_map::tiles::TileLayer, picked: bool) -> bool {
    // As for vector tiles' features: a basemap (one from the catalog) isn't asked, an added layer is.
    t.kind == TileKind::Raster
        && (picked || t.id.is_empty() || t.overlay)
        && t.draws()
        && t.opacity > 0.0
        && match &t.feature_info {
            Some(fi) => fi.queryable(),
            None => ogc::wms_layer_of(&t.url).is_some(),
        }
}

/// What to fetch to ask a WMS tile layer about a point: the GetFeatureInfo request, or the
/// service's capabilities when the layer doesn't know it. `None` when it can't be asked there.
fn next_fetch(t: &rustgis_map::tiles::TileLayer, lon: f64, lat: f64, at: Coord, m_per_px: f64) -> Option<Value> {
    if t.bounds.is_some_and(|[w, s, e, n]| !(w <= lon && lon <= e && s <= lat && lat <= n)) {
        return None;
    }
    match &t.feature_info {
        Some(fi) => ask(t, fi, at, m_per_px),
        // The capabilities address comes from the layer's template (a project's, untrusted too).
        None => ogc::wms_layer_of(&t.url)
            .filter(|(caps, _)| crate::render::rustgis_tiles::address::check(caps, Some(&t.url)).is_ok())
            .map(|(caps, _)| json!({"tileLayer": t.name, "capabilities": caps})),
    }
}

/// A WMS tile layer's GetFeatureInfo request for a point, in `fi`'s terms.
fn ask(t: &rustgis_map::tiles::TileLayer, fi: &FeatureInfo, at: Coord, m_per_px: f64) -> Option<Value> {
    // The request's address comes from the capabilities (hostile input): it must not reach into
    // the local network from a service on the web.
    // At the time the layer draws (a WMS layer with a time dimension).
    fi.request(at.x, at.y, m_per_px)
        .map(|url| ogc::at_map_time(url, &t.url))
        .filter(|url| crate::render::rustgis_tiles::address::check(url, Some(&t.url)).is_ok())
        .map(|url| json!({"tileLayer": t.name, "url": url}))
}

/// The WMS tile layers drawn at a point to ask what is there, topmost first (only the topmost
/// when `first_layer`), each with what to fetch.
pub(super) fn requests(s: &Session, lon: f64, lat: f64, first_layer: bool, picked: Option<usize>) -> Vec<Value> {
    let Some((at, m_per_px)) = merc_at(s, lon, lat) else { return Vec::new() };
    let wanted = if first_layer { 1 } else { MAX_ASKED };
    s.map()
        .tiles
        .iter()
        .enumerate()
        .filter(|(i, t)| picked.is_none_or(|p| p == *i) && askable(t, picked.is_some()))
        .filter_map(|(_, t)| next_fetch(t, lon, lat, at, m_per_px))
        .take(wanted)
        .collect()
}

/// The index of the topmost WMS tile layer that can be asked what is at a point (it covers it).
pub(super) fn topmost(s: &Session, lon: f64, lat: f64) -> Option<usize> {
    let (at, m_per_px) = merc_at(s, lon, lat)?;
    s.map().tiles.iter().position(|t| askable(t, false) && next_fetch(t, lon, lat, at, m_per_px).is_some())
}

/// The answer's features as identify's results.
fn results(layer: &str, features: Vec<InfoFeature>) -> Value {
    let list: Vec<Value> = features
        .into_iter()
        .map(|f| {
            let attrs: serde_json::Map<String, Value> = f.attributes.into_iter().map(|(k, v)| (k, json!(v))).collect();
            let name = match &f.layer {
                Some(l) if l != layer => format!("{layer} › {l}"),
                _ => layer.to_string(),
            };
            json!({"layer": name, "tileLayer": layer, "sourceLayer": f.layer, "attributes": attrs})
        })
        .collect();
    json!({"results": list})
}

/// The index of the WMS tile layer named `name`.
fn wms_layer(s: &Session, name: &str) -> Result<usize, String> {
    s.map()
        .tiles
        .iter()
        .position(|t| t.name == name && (t.feature_info.is_some() || ogc::wms_layer_of(&t.url).is_some()))
        .ok_or_else(|| format!("no WMS tile layer '{name}' (map.tiles lists them)"))
}

/// Reads the layer's request from its service's capabilities; a layer the service doesn't list,
/// or doesn't offer for identify, isn't asked again. The map's other layers from the same service
/// that don't know theirs learn it too, so the capabilities are read once.
fn learn(s: &mut Session, index: usize, capabilities: &[u8]) -> Result<(), String> {
    let Some(t) = s.map().tiles.get(index) else { return Ok(()) };
    let Some((caps_url, _)) = ogc::wms_layer_of(&t.url) else { return Ok(()) };
    // Read as an answer is (Latin-1 from older servers), in the app and natively alike.
    let service = ogc::parse(&caps_url, ogc::answer_text(capabilities)?.as_bytes())?;
    let info_of = |layer: String| {
        service
            .layers
            .iter()
            .find(|l| l.name == layer)
            .and_then(|l| l.feature_info.clone())
            .unwrap_or_else(|| FeatureInfo { layer, ..FeatureInfo::default() })
    };
    for (i, t) in s.map_mut().tiles.iter_mut().enumerate() {
        if i != index && t.feature_info.is_some() {
            continue;
        }
        if let Some((caps, layer)) = ogc::wms_layer_of(&t.url)
            && caps == caps_url
        {
            t.feature_info = Some(info_of(layer));
        }
    }
    Ok(())
}

fn feature_info(s: &mut Session, p: &Value) -> CmdResult {
    let name = ps(p, "layer").ok_or("layer is required (a WMS tile layer's name)")?.to_string();
    let index = wms_layer(s, &name)?;
    // Errors name the layer: the app shows them as they are.
    let named = |e: String| format!("{name}: {e}");
    if let Some(caps) = ps(p, "capabilities") {
        if caps.len() > MAX_TEXT {
            return Err(named(format!("the capabilities are larger than {} MB", MAX_TEXT >> 20)));
        }
        learn(s, index, caps.as_bytes()).map_err(named)?;
    }
    if let Some(text) = ps(p, "text") {
        if text.len() > MAX_TEXT {
            return Err(named(format!("the answer is larger than {} MB", MAX_TEXT >> 20)));
        }
        let asked = ps(p, "url");
        return match ogc::read_feature_info(text.as_bytes()) {
            Ok(features) => {
                if let Some(request) = asked {
                    answered(s, index, request);
                }
                Ok(results(&name, features))
            }
            // The service's error in the format asked in: the request in the next, for the point.
            Err(e) => {
                let point = f(p, "lon").zip(f(p, "lat")).and_then(|(lon, lat)| merc_at(s, lon, lat).map(|m| (lon, lat, m)));
                let retry = match (asked, point) {
                    (Some(request), Some((lon, lat, (at, m_per_px)))) if ogc::may_answer_in_another_format(text.as_bytes()) => {
                        retry(s, index, request, lon, lat, at, m_per_px)
                    }
                    _ => None,
                };
                retry.ok_or_else(|| named(e))
            }
        };
    }
    let lon = f(p, "lon").ok_or("lon is required")?;
    let lat = f(p, "lat").ok_or("lat is required")?;
    let (at, m_per_px) = merc_at(s, lon, lat).ok_or("lon and lat must be numbers")?;
    let next = |s: &Session| s.map().tiles.get(index).and_then(|t| next_fetch(t, lon, lat, at, m_per_px));
    if b(p, "request") == Some(true) {
        // Nothing to ask (not queryable, or outside the layer): no results.
        return Ok(next(s).unwrap_or_else(|| json!({"results": []})));
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = next;
        Err(format!("{name}: in the web app, the map's Identify asks the service"))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let fetch = |url: &str| {
            crate::render::rustgis_tiles::cache::download_blocking_limited(url, ogc::MAX_INFO_BYTES).map_err(|e| format!("{name}: {e}"))
        };
        let mut step = next(s);
        if let Some(caps) = step.as_ref().and_then(|v| v["capabilities"].as_str()).map(str::to_string) {
            learn(s, index, &fetch(&caps)?).map_err(named)?;
            step = next(s);
        }
        // Each of the service's errors asks in a format read after the last, so this ends.
        loop {
            let Some(url) = step.as_ref().and_then(|v| v["url"].as_str()).map(str::to_string) else {
                return Ok(json!({"results": []}));
            };
            let answer = fetch(&url)?;
            match ogc::read_feature_info(&answer) {
                Ok(features) => {
                    answered(s, index, &url);
                    return Ok(results(&name, features));
                }
                Err(e) => match retry(s, index, &url, lon, lat, at, m_per_px) {
                    Some(next) if ogc::may_answer_in_another_format(&answer) => step = Some(next),
                    _ => return Err(named(e)),
                },
            }
        }
    }
}

/// After the service answered `request` with its error: the request in the next answer format
/// the layer offers, when there is one. The layer keeps its own until another format answers.
fn retry(s: &Session, index: usize, request: &str, lon: f64, lat: f64, at: Coord, m_per_px: f64) -> Option<Value> {
    let t = s.map().tiles.get(index)?;
    if t.bounds.is_some_and(|[w, s, e, n]| !(w <= lon && lon <= e && s <= lat && lat <= n)) {
        return None;
    }
    let next = t.feature_info.as_ref()?.retry_after(request)?;
    ask(t, &next, at, m_per_px)
}

/// After the service answered `request` with features (or none): a layer asked in a later format
/// than its own (a retry that worked) asks in that one for the rest of the session.
fn answered(s: &mut Session, index: usize, request: &str) {
    let Some(t) = s.map_mut().tiles.get_mut(index) else { return };
    if let Some(next) = t.feature_info.as_ref().and_then(|fi| fi.answered(request)) {
        t.feature_info = Some(next);
    }
}
