//! Identify on WMS layers: what a service says is at a point (GetFeatureInfo). Identify lists the
//! requests for the WMS tile layers drawn there; the app fetches them in the background and passes
//! each answer to `identify.featureInfo` as `"text"`; the command line, MCP and the assistant let
//! `identify.featureInfo` fetch (waiting for the answer).
//!
//! A layer added from the service knows its request from the capabilities; one from a project
//! (which doesn't keep it) is asked for its service's capabilities first.
//!
//! Both requests are made within the layer's reach ([`reach_of`]): a layer a document read from
//! the internet listed, or whose capabilities were read from there, asks only there, however the
//! service's names resolve; the listed requests say so (`"public": true`) for the app's fetch.

use rustgis_data::ogc::{self, FeatureInfo, InfoFeature};
use rustgis_geom::Coord;
use rustgis_map::tiles::TileKind;
use serde_json::{Value, json};

use super::super::{b, f, ps};
use crate::render::rustgis_tiles::{address::Reach, cache};
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

/// How far a WMS tile layer's identify requests may reach: only the internet when its template is
/// held there (a document read from the internet listed the layer) or its capabilities were read
/// from there ([`cache::ceiling_for`] records both), else as far as the user's own address.
///
/// A request learned from a document read from the internet keeps that reach (`feature_info_public`),
/// whatever the records say by the time it is asked.
fn reach_of(t: &rustgis_map::tiles::TileLayer) -> Reach {
    let caps = ogc::wms_layer_of(&t.url).map_or(Reach::Local, |(caps, _)| cache::ceiling_for(&caps));
    let learned = if t.feature_info.is_some() && t.feature_info_public { Reach::Public } else { Reach::Local };
    cache::ceiling_for(&t.url).min(caps).min(learned)
}

/// What to fetch to ask a WMS tile layer about a point: the GetFeatureInfo request, or the
/// service's capabilities when the layer doesn't know it. `None` when it can't be asked there;
/// an error, naming the address and why, when what it would ask reaches past the layer.
fn next_fetch(t: &rustgis_map::tiles::TileLayer, lon: f64, lat: f64, at: Coord, m_per_px: f64) -> Option<Result<Value, String>> {
    if t.bounds.is_some_and(|[w, s, e, n]| !(w <= lon && lon <= e && s <= lat && lat <= n)) {
        return None;
    }
    match &t.feature_info {
        Some(fi) => ask(t, fi, at, m_per_px),
        // The capabilities address comes from the layer's template (a project's, untrusted too).
        None => ogc::wms_layer_of(&t.url).map(|(caps, _)| fetch_value(t, "capabilities", caps)),
    }
}

/// A WMS tile layer's GetFeatureInfo request for a point, in `fi`'s terms.
fn ask(t: &rustgis_map::tiles::TileLayer, fi: &FeatureInfo, at: Coord, m_per_px: f64) -> Option<Result<Value, String>> {
    // The request's address comes from the capabilities (hostile input): it must not reach past
    // the layer's reach. At the time the layer draws (a WMS layer with a time dimension).
    fi.request(at.x, at.y, m_per_px).map(|url| ogc::at_map_time(url, &t.url)).map(|url| fetch_value(t, "url", url))
}

/// What identify lists for a layer to fetch (`key`: `url` or `capabilities`), when `url` is within
/// the layer's reach ([`reach_of`]), with `"public"` when that is only the internet; otherwise why
/// not, naming the address.
fn fetch_value(t: &rustgis_map::tiles::TileLayer, key: &str, url: String) -> Result<Value, String> {
    let reach = reach_of(t);
    crate::render::rustgis_tiles::address::check_reach(&url, reach)?;
    let mut out = json!({"tileLayer": t.name, key: url});
    if reach == Reach::Public {
        out["public"] = json!(true);
    }
    Ok(out)
}

/// The WMS tile layers drawn at a point to ask what is there, topmost first as drawn (a higher
/// depth priority on top, #2488; only the topmost when `first_layer`), each with what to fetch.
/// One whose request reaches past it isn't asked ([`refused`] says so).
pub(super) fn requests(s: &Session, lon: f64, lat: f64, first_layer: bool, picked: Option<usize>) -> Vec<Value> {
    let Some((at, m_per_px)) = merc_at(s, lon, lat) else { return Vec::new() };
    let wanted = if first_layer { 1 } else { MAX_ASKED };
    let map = s.map();
    let mut asks: Vec<Value> = map
        .tiles_in_draw_order()
        .into_iter()
        .filter_map(|i| Some((i, map.tiles.get(i)?)))
        .filter(|(i, t)| picked.is_none_or(|p| p == *i) && askable(t, picked.is_some()))
        .filter_map(|(_, t)| next_fetch(t, lon, lat, at, m_per_px)?.ok())
        .take(wanted)
        .collect();
    if !s.policy_user() {
        for ask in &mut asks {
            crate::sources::redact_url_fields(ask);
        }
    }
    asks
}

/// The notice for the WMS tile layers drawn at a point whose request reaches past the layer, so
/// they are never asked there, whichever layer identify reports: one notice, naming the first of
/// them, its address and why, with how many.
pub(super) fn refused(s: &Session, lon: f64, lat: f64, picked: Option<usize>) -> Option<String> {
    let (at, m_per_px) = merc_at(s, lon, lat)?;
    let map = s.map();
    let refused: Vec<(String, String)> = map
        .tiles_in_draw_order()
        .into_iter()
        .filter_map(|i| Some((i, map.tiles.get(i)?)))
        .filter(|(i, t)| picked.is_none_or(|p| p == *i) && askable(t, picked.is_some()))
        .filter_map(|(_, t)| next_fetch(t, lon, lat, at, m_per_px)?.err().map(|e| (t.name.clone(), e)))
        .collect();
    let (layer, e) = refused.first()?;
    Some(match refused.len() {
        1 => format!("{layer} wasn't asked what is here: {e}"),
        n => format!("{n} WMS layers weren't asked what is here, such as {layer}: {e}"),
    })
}

/// The index of the topmost WMS tile layer, as drawn, that can be asked what is at a point (it
/// covers it).
pub(super) fn topmost(s: &Session, lon: f64, lat: f64) -> Option<usize> {
    let (at, m_per_px) = merc_at(s, lon, lat)?;
    let map = s.map();
    map.tiles_in_draw_order()
        .into_iter()
        .find(|i| map.tiles.get(*i).is_some_and(|t| askable(t, false) && next_fetch(t, lon, lat, at, m_per_px).is_some_and(|r| r.is_ok())))
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
///
/// Capabilities read from the internet (`public`: as the fetch reading them observed it, or a
/// caller that can't say where they were read; or as their record says now) leave each layer's
/// request held there for good (`feature_info_public`). The capabilities' own address keeps its
/// record: it is the layer's.
fn learn(s: &mut Session, index: usize, capabilities: &[u8], public: bool) -> Result<(), String> {
    let Some(t) = s.map().tiles.get(index) else { return Ok(()) };
    let Some((caps_url, _)) = ogc::wms_layer_of(&t.url) else { return Ok(()) };
    let public = public || cache::ceiling_for(&caps_url) == Reach::Public;
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
            t.feature_info_public = public || cache::ceiling_for(&t.url) == Reach::Public;
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
        // Capabilities supplied by anything but the app say nothing trustworthy about where they
        // were read: the requests they name are asked only on the internet. Nor do they replace
        // a request the layer already knows, or its reach.
        let untrusted = !s.policy_user();
        if !(untrusted && s.map().tiles.get(index).is_some_and(|t| t.feature_info.is_some())) {
            learn(s, index, caps.as_bytes(), untrusted).map_err(named)?;
        }
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
                let mut retry = match (asked, point) {
                    (Some(request), Some((lon, lat, (at, m_per_px)))) if ogc::may_answer_in_another_format(text.as_bytes()) => {
                        retry(s, index, request, lon, lat, at, m_per_px)
                    }
                    _ => None,
                };
                if !s.policy_user()
                    && let Some(Ok(out)) = &mut retry
                {
                    crate::sources::redact_url_fields(out);
                }
                match retry {
                    Some(next) => next.map_err(named),
                    None => Err(named(e)),
                }
            }
        };
    }
    let lon = f(p, "lon").ok_or("lon is required")?;
    let lat = f(p, "lat").ok_or("lat is required")?;
    let (at, m_per_px) = merc_at(s, lon, lat).ok_or("lon and lat must be numbers")?;
    let next = |s: &Session| s.map().tiles.get(index).and_then(|t| next_fetch(t, lon, lat, at, m_per_px));
    if b(p, "request") == Some(true) {
        // Nothing to ask (not queryable, or outside the layer): no results.
        let mut out = next(s).transpose().map_err(named)?.unwrap_or_else(|| json!({"results": []}));
        if !s.policy_user() {
            crate::sources::redact_url_fields(&mut out);
        }
        return Ok(out);
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = next;
        Err(format!("{name}: in the web app, the map's Identify asks the service"))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Within the layer's reach, which the listed step says.
        let fetch = |step: &Value, url: &str| {
            let ceiling = if step["public"] == true { Reach::Public } else { Reach::Local };
            cache::download_blocking_with_origin_within(url, ogc::MAX_INFO_BYTES, cache::BLOCKING_TIMEOUT, &[], ceiling)
                .map_err(cache::large_file_hint)
                .map_err(|e| format!("{name}: {e}"))
        };
        let mut step = next(s).transpose().map_err(named)?;
        if let Some(v) = &step
            && let Some(caps) = v["capabilities"].as_str()
        {
            let fetched = fetch(v, caps)?;
            // Capabilities read from the internet hold the requests they name there.
            if fetched.origin.reach() == Reach::Public {
                cache::restrict_to_public(caps);
            }
            learn(s, index, &fetched.bytes, fetched.origin.reach() == Reach::Public).map_err(named)?;
            step = next(s).transpose().map_err(named)?;
        }
        // Each of the service's errors asks in a format read after the last, so this ends.
        loop {
            let Some(v) = step.as_ref() else { return Ok(json!({"results": []})) };
            let Some(url) = v["url"].as_str().map(str::to_string) else {
                return Ok(json!({"results": []}));
            };
            let answer = fetch(v, &url)?.bytes;
            match ogc::read_feature_info(&answer) {
                Ok(features) => {
                    answered(s, index, &url);
                    return Ok(results(&name, features));
                }
                Err(e) => match retry(s, index, &url, lon, lat, at, m_per_px) {
                    Some(next) if ogc::may_answer_in_another_format(&answer) => step = Some(next.map_err(named)?),
                    _ => return Err(named(e)),
                },
            }
        }
    }
}

/// After the service answered `request` with its error: the request in the next answer format
/// the layer offers, when there is one. The layer keeps its own until another format answers.
fn retry(s: &Session, index: usize, request: &str, lon: f64, lat: f64, at: Coord, m_per_px: f64) -> Option<Result<Value, String>> {
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// Capabilities supplied by anything but the app (the Assistant, MCP, a plugin) say nothing
    /// trustworthy about where they were read: the GetFeatureInfo request they name on the local
    /// network is refused, naming it. The app's own are asked there, as read where it read them.
    #[test]
    fn capabilities_supplied_by_anything_but_the_app_hold_their_requests_to_the_internet() {
        use crate::policy::{Effects, Revocation, Scope};
        struct Live;
        impl Revocation for Live {
            fn allows(&self, _: Effects) -> bool {
                true
            }
        }
        for (n, app) in [(1, true), (2, false)] {
            let host = format!("identify-supplied-{n}.example");
            let caps = format!(
                r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink">
<Service><Title>Test WMS</Title></Service>
<Capability><Request><GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="https://{host}/wms?"/></Get></HTTP></DCPType></GetMap>
<GetFeatureInfo><Format>application/geo+json</Format><DCPType><HTTP><Get><OnlineResource xlink:href="http://192.168.{n}.5/info?"/></Get></HTTP></DCPType></GetFeatureInfo></Request>
<Layer queryable="1"><Title>Root</Title><CRS>EPSG:3857</CRS><Layer><Name>roads</Name><Title>Roads</Title></Layer></Layer></Capability></WMS_Capabilities>"#
            );
            let mut s = Session::new(crate::Project::default());
            let template = format!(
                "https://{host}/wms?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS=roads&STYLES=&CRS=EPSG:3857&BBOX={{bbox-epsg-3857}}&WIDTH=256&HEIGHT=256&FORMAT=image/png"
            );
            s.run("map.addTiles", &json!({"url": template, "name": "Roads"})).unwrap();
            s.run("map.goToXY", &json!({"lon": 8.5, "lat": 47.4, "scale": 10000})).unwrap();
            let p = json!({"layer": "Roads", "lon": 8.5, "lat": 47.4, "capabilities": caps, "request": true});
            let r = if app {
                feature_info(&mut s, &p)
            } else {
                s.with_scope(Scope::assistant(4_201_303, std::sync::Arc::new(Live)), |s| feature_info(s, &p)).unwrap()
            };
            if app {
                assert!(r.unwrap()["url"].as_str().is_some_and(|u| u.starts_with(&format!("http://192.168.{n}.5/info?"))));
                // Capabilities the fetch observed on the internet hold what they name there, whatever
                // the capabilities address's record says by then.
                learn(&mut s, 0, caps.as_bytes(), true).unwrap();
                assert!(s.map().tiles[0].feature_info_public);
                let e = feature_info(&mut s, &json!({"layer": "Roads", "lon": 8.5, "lat": 47.4, "request": true})).unwrap_err();
                assert!(e.contains("local network"), "{e}");
            } else {
                let e = r.unwrap_err();
                assert!(e.contains(&format!("http://192.168.{n}.5/info")) && e.contains("local network"), "{e}");
                // The layer's own capabilities address keeps its record: only the requests the
                // supplied document names are held.
                let caps_url = ogc::wms_layer_of(&template).unwrap().0;
                assert_eq!(cache::ceiling_for(&caps_url), Reach::Local, "{caps_url}");
                // The request keeps the reach it was learned at, though the user names the service
                // directly afterwards (which clears the addresses' records).
                cache::allow_named(&caps_url);
                cache::allow_named(&template);
                let p = json!({"layer": "Roads", "lon": 8.5, "lat": 47.4, "request": true});
                let e = feature_info(&mut s, &p).unwrap_err();
                assert!(e.contains(&format!("http://192.168.{n}.5/info")) && e.contains("local network"), "{e}");
            }
        }
        // A layer whose request the app learned keeps it, and its reach, when the Assistant
        // supplies capabilities for it afterwards.
        let host = "identify-supplied-3.example";
        let caps = format!(
            r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink">
<Service><Title>Test WMS</Title></Service>
<Capability><Request><GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="https://{host}/wms?"/></Get></HTTP></DCPType></GetMap>
<GetFeatureInfo><Format>application/geo+json</Format><DCPType><HTTP><Get><OnlineResource xlink:href="http://192.168.3.5/info?"/></Get></HTTP></DCPType></GetFeatureInfo></Request>
<Layer queryable="1"><Title>Root</Title><CRS>EPSG:3857</CRS><Layer><Name>roads</Name><Title>Roads</Title></Layer></Layer></Capability></WMS_Capabilities>"#
        );
        let mut s = Session::new(crate::Project::default());
        let template = format!(
            "https://{host}/wms?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS=roads&STYLES=&CRS=EPSG:3857&BBOX={{bbox-epsg-3857}}&WIDTH=256&HEIGHT=256&FORMAT=image/png"
        );
        s.run("map.addTiles", &json!({"url": template, "name": "Mine"})).unwrap();
        s.run("map.goToXY", &json!({"lon": 8.5, "lat": 47.4, "scale": 10000})).unwrap();
        let p = json!({"layer": "Mine", "lon": 8.5, "lat": 47.4, "capabilities": caps, "request": true});
        feature_info(&mut s, &p).unwrap();
        let scope = Scope::assistant(4_201_308, std::sync::Arc::new(Live));
        let _ = s.with_scope(scope, |s| feature_info(s, &p)).unwrap();
        assert!(!s.map().tiles[0].feature_info_public);
        let asked = feature_info(&mut s, &json!({"layer": "Mine", "lon": 8.5, "lat": 47.4, "request": true})).unwrap();
        assert!(asked["url"].as_str().is_some_and(|u| u.starts_with("http://192.168.3.5/info?")), "{asked}");
    }
}
