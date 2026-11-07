//! USGS NLDI (the Network-Linked Data Index): a point's flowline, the basin draining to it, and
//! navigation up- or downstream to the flowlines and linked sites on the way, each added as a
//! GeoJSON layer. Requests and answers are `rustgis_tiles::nldi`'s; the app fetches off the frame
//! and passes the answers, the command line waits for them.

use serde_json::{Value, json};

use super::{f, ps, uint};
use crate::render::rustgis_tiles::nldi;
use crate::{CmdResult, CommandSpec as C, Session};

const WHERE: &str = "Home › Add › Web Data › United States › USGS NLDI";

pub fn commands() -> Vec<C> {
    vec![
        C::new("nldi.flowline", "NLDI Flowline", WHERE, flowline).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"lon": number, "lat": number (a point in the United States), "direction"?: "none" (the whole flowline, default) | "up" (its part upstream of the point) | "down", "name"?: text, "answer"?: "the flowtrace's GeoJSON, or a COMID's flowline, when already fetched", "snapped"?: [lon, lat] (with a COMID's flowline: where the point meets it)} — adds the flowline and the raindrop's path to it; returns its COMID"#,
        ),
        C::new("nldi.comid", "NLDI COMID at a Point", WHERE, comid_cmd).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network))
            .view()
            .params(r#"{"lon": number, "lat": number} — the nearest NHDPlus flowline's COMID, and where the point snaps to it"#),
        C::new("nldi.basin", "NLDI Basin", WHERE, basin).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"comid"?: number (an NHDPlus flowline's), "lon"?: number, "lat"?: number (or a point: its flowline's COMID), "name"?: text, "answer"?: "the basin's GeoJSON, when already fetched"} — adds the basin draining to the flowline"#,
        ),
        C::new("nldi.sources", "NLDI Navigation Sources", WHERE, sources).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).view().params(
            r#"{"comid"?: number (an NHDPlus flowline's), "lon"?: number, "lat"?: number (or a point: its flowline's COMID), "mode"?: "UM" (upstreamMain, default) | "UT" (upstreamTributaries) | "DM" (downstreamMain) | "DD" (downstreamDiversions), "answer"?: text} — the linked data a navigation can list (flowlines, nwissite, wqp, huc12pp…)"#,
        ),
        C::new("nldi.navigate", "NLDI Navigation", WHERE, navigate).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"comid"?: number (an NHDPlus flowline's), "lon"?: number, "lat"?: number (or a point: its flowline's COMID), "mode"?: "UM" (upstreamMain, default) | "UT" | "DM" | "DD", "distance"?: km 1–9999 (500), "source"?: "flowlines" (default) or a source nldi.sources lists (nwissite, wqp…), "name"?: text, "answer"?: "the navigation's GeoJSON, when already fetched"} — adds the flowlines or sites along the way"#,
        ),
    ]
}

/// The point a command names.
fn point(p: &Value) -> Result<(f64, f64), String> {
    match (f(p, "lon"), f(p, "lat")) {
        (Some(lon), Some(lat)) => nldi::point(lon, lat),
        _ => Err("give lon and lat (a point in the United States), or comid".into()),
    }
}

/// The COMID a command names, or its point's.
fn comid(p: &Value) -> Result<u64, String> {
    if let Some(c) = uint(p, "comid").filter(|c| *c > 0) {
        return Ok(c);
    }
    if p.get("comid").is_some_and(|c| !c.is_null()) {
        return Err("comid is a whole number (an NHDPlus flowline's)".into());
    }
    let (lon, lat) = point(p)?;
    Ok(nldi::parse_hydrolocation(&fetch(&nldi::hydrolocation_url(lon, lat))?)?.0)
}

fn name(p: &Value, default: String) -> String {
    ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or(default)
}

/// How an NLDI layer is drawn.
#[derive(Clone, Copy)]
enum Look {
    Flowline,
    Raindrop,
    Basin,
    Navigation,
    Sites,
}

/// Adds a FeatureCollection as a layer drawn as `look`; none when it has no features.
fn add(s: &mut Session, fc: &Value, name: &str, look: Look) -> Result<Option<u32>, String> {
    if fc["features"].as_array().is_none_or(Vec::is_empty) {
        return Ok(None);
    }
    let text = fc.to_string();
    if text.len() > 256 << 20 {
        return Err(format!("{name}: the answer is too large"));
    }
    let (ds, _) = rustgis_data::io::read_geojson(&text, name).map_err(|e| format!("{name}: {e}"))?;
    let id = super::layer::add_dataset(s, ds);
    if let Some(l) = s.map_mut().layer_mut(id)
        && let rustgis_map::Renderer::Single(sym) = &mut l.renderer
    {
        let rgba = rustgis_map::Rgba::new;
        match look {
            Look::Flowline | Look::Navigation => {
                let c = if matches!(look, Look::Flowline) { rgba(0, 92, 230, 255) } else { rgba(0, 150, 200, 255) };
                sym.stroke = c;
                sym.fill = c;
                sym.stroke_width = if matches!(look, Look::Flowline) { 3.0 } else { 2.0 };
            }
            Look::Raindrop => {
                sym.stroke = rgba(230, 120, 0, 255);
                sym.fill = sym.stroke;
                sym.stroke_width = 1.6;
            }
            Look::Basin => {
                sym.fill = rgba(80, 160, 230, 50);
                sym.stroke = rgba(20, 90, 170, 255);
                sym.stroke_width = 1.5;
            }
            Look::Sites => {
                sym.fill = rgba(220, 60, 60, 255);
                sym.stroke = rgba(255, 255, 255, 255);
                sym.size = 7.0;
            }
        }
    }
    Ok(Some(id))
}

fn flowline(s: &mut Session, p: &Value) -> CmdResult {
    let (lon, lat) = point(p)?;
    let direction = nldi::direction(ps(p, "direction").unwrap_or("none"))?;
    let snapped = p.get("snapped").and_then(Value::as_array).and_then(|a| Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?]));
    let (trace, fallback) = match ps(p, "answer") {
        Some(a) => (nldi::parse_flowtrace(a.as_bytes())?, snapped),
        None => {
            match fetch_post(&nldi::flowtrace_url(), &nldi::flowtrace_body(lon, lat, direction)).and_then(|b| nldi::parse_flowtrace(&b)) {
                Ok(t) => (t, None),
                // The flowtrace failed: the nearest flowline (whole) and the way to it stand in.
                Err(e) => {
                    log::info!("NLDI flowtrace failed ({e}); using the hydrolocation");
                    let (c, at) = nldi::parse_hydrolocation(&fetch(&nldi::hydrolocation_url(lon, lat))?)?;
                    (nldi::parse_flowtrace(&fetch(&nldi::comid_url(c))?)?, at)
                }
            }
        }
    };
    let mut trace = trace;
    if trace.raindrop.is_none()
        && let Some(at) = fallback
    {
        trace.raindrop = Some(nldi::snapped_path(lon, lat, at));
    }
    let label = trace.comid.map(|c| format!("NLDI flowline {c}")).unwrap_or_else(|| "NLDI flowline".into());
    let base = name(p, label);
    let line = add(s, &trace.flowline, &base, Look::Flowline)?;
    let drop = match &trace.raindrop {
        Some(r) => add(s, r, &format!("{base} raindrop path"), Look::Raindrop)?,
        None => None,
    };
    Ok(json!({"comid": trace.comid, "layer": line, "raindrop": drop}))
}

fn comid_cmd(_s: &mut Session, p: &Value) -> CmdResult {
    let (lon, lat) = point(p)?;
    let (c, at) = nldi::parse_hydrolocation(&fetch(&nldi::hydrolocation_url(lon, lat))?)?;
    Ok(json!({"comid": c, "snapped": at}))
}

fn basin(s: &mut Session, p: &Value) -> CmdResult {
    let c = comid(p)?;
    let bytes = match ps(p, "answer") {
        Some(a) => a.as_bytes().to_vec(),
        None => fetch(&nldi::basin_url(c))?,
    };
    let fc = nldi::collection(&bytes)?;
    let id =
        add(s, &fc, &name(p, format!("NLDI basin {c}")), Look::Basin)?.ok_or_else(|| format!("the NLDI has no basin for COMID {c}"))?;
    Ok(json!({"comid": c, "layer": id}))
}

fn sources(_s: &mut Session, p: &Value) -> CmdResult {
    let mode = nldi::mode(ps(p, "mode").unwrap_or("UM"))?;
    let c = comid(p)?;
    let bytes = match ps(p, "answer") {
        Some(a) => a.as_bytes().to_vec(),
        None => fetch(&nldi::sources_url(c, mode))?,
    };
    let list = nldi::parse_sources(&bytes)?;
    Ok(json!({"comid": c, "mode": mode, "sources": list.iter().map(|s| json!({"id": s.id, "name": s.name})).collect::<Vec<_>>()}))
}

fn navigate(s: &mut Session, p: &Value) -> CmdResult {
    let mode = nldi::mode(ps(p, "mode").unwrap_or("UM"))?;
    let distance = nldi::clamp_distance(f(p, "distance").unwrap_or(500.0));
    let source = ps(p, "source").map(str::trim).filter(|s| !s.is_empty()).unwrap_or("flowlines").to_ascii_lowercase();
    // The source is checked before anything is fetched.
    nldi::navigation_url(1, mode, &source, distance)?;
    let c = comid(p)?;
    let bytes = match ps(p, "answer") {
        Some(a) => a.as_bytes().to_vec(),
        None => fetch(&nldi::navigation_url(c, mode, &source, distance)?)?,
    };
    let fc = nldi::collection(&bytes)?;
    let n = fc["features"].as_array().map_or(0, Vec::len);
    let look = if source == "flowlines" { Look::Navigation } else { Look::Sites };
    let label = format!("NLDI {} {source} ({distance} km)", nldi::mode_label(mode).to_lowercase());
    let id = add(s, &fc, &name(p, label), look)?;
    if id.is_none() {
        s.notify(format!("the NLDI found no {source} {} within {distance} km of COMID {c}", nldi::mode_label(mode).to_lowercase()), false);
    }
    Ok(json!({"comid": c, "mode": mode, "source": source, "distance": distance, "features": n, "layer": id}))
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    crate::render::rustgis_tiles::cache::download_blocking(url).map_err(nldi::explain)
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch_post(url: &str, body: &str) -> Result<Vec<u8>, String> {
    crate::render::rustgis_tiles::cache::post_json_blocking(url, body)
}

#[cfg(target_arch = "wasm32")]
fn fetch(_url: &str) -> Result<Vec<u8>, String> {
    Err("in the web app, use Home › Add › Web Data › United States › USGS NLDI".into())
}

#[cfg(target_arch = "wasm32")]
fn fetch_post(_url: &str, _body: &str) -> Result<Vec<u8>, String> {
    Err("in the web app, use Home › Add › Web Data › United States › USGS NLDI".into())
}
