//! Home › Selection.

use rustgis_analysis::{
    network,
    selection::{MAX_POINTS, PolygonMask},
};
use rustgis_geom::{Coord, Extent};
use rustgis_map::PopupMode;
use serde_json::{Value, json};

use super::{f, need_layer, need_layers, need_selection, ps};
use crate::{CmdResult, CommandSpec as C, Session};

#[path = "select_info.rs"]
mod info;

pub fn commands() -> Vec<C> {
    let mut commands = vec![
        C::new("select.byAttributes", "Select By Attributes", "Home › Selection", by_attributes).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_layers)
            .params(r#"{"layer"?, "where": "SQL clause", "mode"?: "new"|"add"|"remove"|"subset"}"#),
        C::new("select.byRect", "Select (rectangle)", "Home › Selection", by_rect).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"minLon", "minLat", "maxLon", "maxLat", "mode"?: "new"|"add"|"remove"}"#),
        C::new("select.byPolygon", "Select by Polygon", "Home › Selection", by_polygon).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"points": [[lon,lat],...], "mode"?: "new"|"add"|"remove"}"#),
        C::new("select.byRadius", "Select by Radius", "Home › Selection", by_radius).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"lon", "lat", "radius": metres (0 < radius <= 5000000), "mode"?: "new"|"add"|"remove"}"#),
        C::new("select.atPoint", "Select (click)", "Home › Selection", at_point).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"lon", "lat", "tolerancePx"?, "mode"?}"#),
        C::new("select.rows", "Select Rows", "Attribute Table", |s, p| {
            let id = s.layer_id(p)?;
            let rows: Vec<usize> = p
                .get("rows")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_u64()).map(|v| v as usize).collect())
                .unwrap_or_default();
            set(s, id, rows, mode(p));
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"layer"?, "rows": [number], "mode"?}"#),
        C::new("select.clear", "Clear", "Home › Selection", |s, _| {
            s.map_mut().clear_selection();
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_selection),
        C::new("select.all", "Select All", "Data › Selection", |s, p| {
            let id = s.layer_id(p)?;
            let n = s.map().layer(id).map(|l| l.dataset.features.len()).unwrap_or(0);
            set(s, id, (0..n).collect(), Mode::New);
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?: name|id (default: the active layer)}"#)
        .when(need_layer),
        C::new("select.switch", "Switch Selection", "Data › Selection", |s, p| {
            let id = s.layer_id(p)?;
            let l = s.map().layer(id).ok_or("no layer")?;
            let rows: Vec<usize> = (0..l.dataset.features.len()).filter(|r| !l.selection.contains(r)).collect();
            set(s, id, rows, Mode::New);
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?: name|id (default: the active layer)}"#)
        .when(need_layer),
        C::new("select.zoomTo", "Zoom To Selection", "Home › Selection", |s, _| {
            let e = s.map().selection_extent();
            if e.is_empty() {
                return Err("nothing with a shape is selected".into());
            }
            s.zoom_to(&e);
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .when(need_selection),
        C::new("select.layerFromSelection", "Make Layer From Selected Features", "Data › Selection", |s, p| {
            let id = s.layer_id(p)?;
            let l = s.map().layer(id).ok_or("no layer")?;
            if l.selection.is_empty() {
                return Err("nothing is selected in this layer".into());
            }
            let mut ds = rustgis_data::Dataset::new(format!("{} selection", l.name), l.geometry_type(), l.dataset.fields.clone());
            for r in &l.selection {
                if let Some(f) = l.dataset.features.get(*r) {
                    ds.push(f.geometry.clone(), f.attrs.clone());
                }
            }
            let nid = super::layer::add_dataset(s, ds);
            Ok(json!({"layer": nid}))
        }).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?: name|id (default: the active layer, which need not be the layer last selected in)}"#)
        .when(need_selection),
        C::new("identify", "Identify", "Home › Inspect", identify).roles(&[]).effects(crate::policy::Effects::NONE).view().params(
            r#"{"lon", "lat", "tolerancePx"?, "mode"?: "topmost"|"visible"|"selectable"|"selectedInContents"|"none" (the map's pop-up mode when omitted; the mode picks the layers, and every feature under the point in them, up to 100, is in "results"; topmost and visible take vector tile layers' features too, with "tileLayer" and "sourceLayer", from the tiles drawn there: one not loaded yet is asked for, and its features come on a later identify; and they list in "featureInfo" the WMS layers drawn there to ask, each {tileLayer, url} (its GetFeatureInfo request) or {tileLayer, capabilities} (its service's, when the layer doesn't know its request): identify.featureInfo reads what they answer; selectedInContents does both for the tile layer picked in the Layers pane (map.selectTiles) alone, whichever it is, a basemap or MapLibre style too)}"#,
        ),
        C::new("map.popupMode", "Pop-up Mode", "Home › Navigate", popup_mode).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"mode": "topmost"|"visible"|"selectable"|"selectedInContents"|"none"}"#),
    ];
    commands.extend(info::commands());
    commands
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    New,
    Add,
    Remove,
    Subset,
}

fn mode(p: &Value) -> Mode {
    match ps(p, "mode").unwrap_or("new").to_ascii_lowercase().as_str() {
        "add" => Mode::Add,
        "remove" => Mode::Remove,
        "subset" => Mode::Subset,
        _ => Mode::New,
    }
}

/// Area commands reject malformed modes before changing any selection. Older commands retain
/// their permissive mode parsing for compatibility.
fn area_mode(p: &Value) -> Result<Mode, String> {
    match p.get("mode") {
        None => Ok(Mode::New),
        Some(Value::String(m)) if m.eq_ignore_ascii_case("new") => Ok(Mode::New),
        Some(Value::String(m)) if m.eq_ignore_ascii_case("add") => Ok(Mode::Add),
        Some(Value::String(m)) if m.eq_ignore_ascii_case("remove") => Ok(Mode::Remove),
        _ => Err("mode must be new, add or remove".into()),
    }
}

/// Applies a selection to one layer (New clears the others too, like desktop GIS).
pub fn set(s: &mut Session, id: u32, rows: Vec<usize>, m: Mode) {
    if m == Mode::New {
        s.map_mut().clear_selection();
    }
    let Some(l) = s.map_mut().layer_mut(id) else { return };
    let n = l.dataset.features.len();
    let rows: Vec<usize> = rows.into_iter().filter(|r| *r < n).collect();
    match m {
        Mode::New => l.selection = rows,
        Mode::Add => {
            for r in rows {
                if !l.selection.contains(&r) {
                    l.selection.push(r);
                }
            }
        }
        Mode::Remove => l.selection.retain(|r| !rows.contains(r)),
        Mode::Subset => l.selection.retain(|r| rows.contains(r)),
    }
    l.normalize_selection();
}

fn by_attributes(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let w = ps(p, "where").ok_or("where is required")?;
    let rows = rustgis_data::query::select(w, &s.map().layer(id).ok_or("no layer")?.dataset)?;
    let n = rows.len();
    set(s, id, rows, mode(p));
    s.notify(format!("{n} features selected"), false);
    Ok(json!({"selected": n}))
}

fn by_rect(s: &mut Session, p: &Value) -> CmdResult {
    let (a, b, c, d) = (f(p, "minLon"), f(p, "minLat"), f(p, "maxLon"), f(p, "maxLat"));
    let (Some(a), Some(b), Some(c), Some(d)) = (a, b, c, d) else { return Err("minLon, minLat, maxLon and maxLat are required".into()) };
    let ll = Extent::from_points(Coord::new(a, b), Coord::new(c, d));
    let e = s.map().flat().extent(&ll);
    let scale = s.scale();
    let m = mode(p);
    let mut hits = s.map_mut().hits_in(&e, scale);
    // In curved projections the map-space box is larger than the lon/lat one: keep only features
    // that reach into the requested rectangle.
    if !matches!(s.map().flat(), rustgis_map::projection::Flat::Mercator) {
        for (id, rows) in &mut hits {
            if let Some(l) = s.map().layer(*id) {
                rows.retain(|r| l.dataset.features.get(*r).and_then(|f| f.geometry.as_ref()).is_some_and(|g| g.extent().intersects(&ll)));
            }
        }
        hits.retain(|(_, rows)| !rows.is_empty());
    }
    apply_hits(s, hits, m)
}

fn by_polygon(s: &mut Session, p: &Value) -> CmdResult {
    let m = area_mode(p)?;
    let input = p.get("points").and_then(Value::as_array).ok_or("points must be an array of [longitude, latitude] pairs")?;
    if input.len() > MAX_POINTS {
        return Err("Selection outline has too many points; draw a simpler area.".into());
    }
    if input.len() < 3 {
        return Err("Selection outline needs at least three distinct points.".into());
    }
    let mut points = Vec::with_capacity(input.len());
    for point in input {
        let pair = point.as_array().filter(|a| a.len() == 2).ok_or("each selection point must be a [longitude, latitude] pair")?;
        let lon = pair.first().and_then(Value::as_f64).ok_or("selection longitude must be a number")?;
        let lat = pair.get(1).and_then(Value::as_f64).ok_or("selection latitude must be a number")?;
        points.push(Coord::new(lon, lat));
    }
    let (flat, key) = (s.map().flat(), s.map().projection.code());
    flat.validate_selection_path(&points)?;
    for point in &mut points {
        *point = flat.forward_checked(*point).filter(|p| p.is_finite()).ok_or("Selection outline is outside the active projection.")?;
    }
    let mask = PolygonMask::new(&points)?;
    let scale = s.scale();
    let mut hits = Vec::new();
    for layer in s.map_mut().selectable_layers(scale) {
        let drawn = layer.drawn_rows(scale);
        let id = layer.id;
        let rows: Vec<usize> = layer
            .projected_in(&flat, &key)
            .iter()
            .enumerate()
            .filter(|(row, geometry)| drawn.get(*row) == Some(&true) && geometry.as_ref().is_some_and(|g| mask.intersects(g)))
            .map(|(row, _)| row)
            .collect();
        if !rows.is_empty() {
            hits.push((id, rows));
        }
    }
    apply_hits(s, hits, m)
}

fn by_radius(s: &mut Session, p: &Value) -> CmdResult {
    let m = area_mode(p)?;
    let lon = f(p, "lon").ok_or("lon is required")?;
    let lat = f(p, "lat").ok_or("lat is required")?;
    let radius = f(p, "radius").ok_or("radius is required in metres")?;
    if !lon.is_finite() || !lat.is_finite() || !(-180.0..=180.0).contains(&lon) || lat.abs() > rustgis_geom::MAX_LAT {
        return Err("Radius selection centre must be inside the Web Mercator longitude and latitude range.".into());
    }
    if !radius.is_finite() || radius <= 0.0 || radius > 5_000_000.0 {
        return Err("radius must be greater than zero and at most 5000000 metres".into());
    }
    let center = Coord::new(lon, lat);
    let flat = s.map().flat();
    flat.validate_selection_path(&[center])?;
    let mut boundary = [center; 4];
    for (i, point) in boundary.iter_mut().enumerate() {
        *point = network::radius_boundary_point(center, radius, i as f64 * std::f64::consts::FRAC_PI_2)
            .ok_or("Radius selection crosses a wrap seam or exceeds the supported latitude range.")?;
    }
    flat.validate_selection_path(&boundary)?;
    let scale = s.scale();
    let mut hits = Vec::new();
    let mut bounded = Vec::new();
    let mut invalid = Vec::new();
    for layer in s.map_mut().selectable_layers(scale) {
        let drawn = layer.drawn_rows(scale);
        let rows: Vec<usize> = drawn.iter().enumerate().filter_map(|(row, shown)| shown.then_some(row)).collect();
        let report = network::within_report(&layer.dataset, &rows, center, radius);
        let (near, n) = (report.rows, report.bounded);
        invalid.push((layer.name.clone(), report.invalid_latitudes, report.missing_coordinates));
        if n > 0 {
            bounded.push((layer.name.clone(), n));
        }
        let rows: Vec<usize> = near.into_iter().map(|(row, _)| row).collect();
        if !rows.is_empty() {
            hits.push((layer.id, rows));
        }
    }
    warn_bounded(s, &bounded);
    warn_radius_input(s, &invalid);
    apply_hits(s, hits, m)
}

/// One warning for the features of each layer a radius search ([`network::within_counted`])
/// counted at a bound on their distance near a pole, with their counts (#1033); none when there are
/// none.
pub(crate) fn warn_bounded(s: &mut Session, bounded: &[(String, usize)]) {
    if bounded.is_empty() {
        return;
    }
    let total: usize = bounded.iter().map(|(_, n)| n).sum();
    let layers: Vec<String> = bounded.iter().map(|(layer, n)| format!("{layer} {n}")).collect();
    let steps = rustgis_map::graphics::thousands(network::REACH_STEPS_TOTAL as f64);
    let longest = network::REACH_STEPS_MAX * network::REACH_STEP_DEG;
    s.warn(format!(
        "{total} feature(s) near a pole ({}) couldn't be measured in full (past a search's {steps} steps a layer, or along an edge over {longest}° long) and were counted at the least distance they could be: they may lie farther, even beyond the radius",
        layers.join(", ")
    ));
}

/// The hit count describes the query, regardless of whether its rows are added or removed.
fn apply_hits(s: &mut Session, hits: Vec<(u32, Vec<usize>)>, m: Mode) -> CmdResult {
    if m == Mode::New {
        s.map_mut().clear_selection();
    }
    let mut total = 0;
    for (id, rows) in hits {
        total += rows.len();
        set(s, id, rows, if m == Mode::New { Mode::Add } else { m });
    }
    Ok(json!({"selected": total}))
}

fn hit(s: &mut Session, p: &Value) -> Result<Option<(u32, usize)>, String> {
    let lon = f(p, "lon").ok_or("lon is required")?;
    let lat = f(p, "lat").ok_or("lat is required")?;
    let tol = f(p, "tolerancePx").unwrap_or(4.0).clamp(0.0, 50.0) * s.map().camera.res;
    let scale = s.scale();
    let Some(p) = s.map().to_map(Coord::new(lon, lat)) else { return Ok(None) };
    // The feature drawn on top among the selectable layers: one in a layer that isn't selectable
    // doesn't hide those below it.
    Ok(s.map_mut().hit(p, tol, scale, |l| l.selectable))
}

fn at_point(s: &mut Session, p: &Value) -> CmdResult {
    let m = mode(p);
    match hit(s, p)? {
        Some((id, row)) => {
            set(s, id, vec![row], m);
            Ok(json!({"layer": id, "row": row}))
        }
        None => {
            if m == Mode::New {
                s.map_mut().clear_selection();
            }
            Ok(Value::Null)
        }
    }
}

/// The most features one identify reports, so a click on a dense spot stays quick.
const MAX_IDENTIFIED: usize = 100;

fn identify(s: &mut Session, p: &Value) -> CmdResult {
    let mode = match ps(p, "mode") {
        Some(m) => PopupMode::from_code(m).ok_or_else(|| format!("unknown pop-up mode '{m}'"))?,
        None => s.map().popup_mode,
    };
    if mode == PopupMode::None {
        return Ok(Value::Null);
    }
    let lon = f(p, "lon").ok_or("lon is required")?;
    let lat = f(p, "lat").ok_or("lat is required")?;
    let tol_px = f(p, "tolerancePx").unwrap_or(4.0).clamp(0.0, 50.0);
    let tol = tol_px * s.map().camera.res;
    let scale = s.scale();
    let active = s.active_layer;
    let keep = |l: &rustgis_map::Layer| match mode {
        PopupMode::Selectable => l.selectable,
        PopupMode::SelectedInContents => Some(l.id) == active,
        _ => true,
    };
    // Every feature under the click in the layers the mode takes; topmost stops at the first layer.
    let first_layer = mode == PopupMode::Topmost;
    let hits = match s.map().to_map(Coord::new(lon, lat)) {
        Some(pt) => s.map_mut().hits_at(pt, tol, scale, keep, first_layer, MAX_IDENTIFIED),
        None => Vec::new(),
    };
    let map = s.map();
    // Features and the vector tile layers' features (tiles are never selectable), in the drawing
    // order of all the map's layers (#808): topmost takes the layer drawn on top of those with any.
    let order = map.stack();
    let place = |d: rustgis_map::Drawn| order.iter().position(|o| *o == d).unwrap_or(usize::MAX);
    let mut ranked: Vec<(usize, Value)> = hits
        .iter()
        .filter_map(|(id, row)| Some((place(rustgis_map::Drawn::Layer(map.layer_index(*id)?)), feature_json(map, *id, *row)?)))
        .collect();
    // A tile layer picked in the Layers pane is the one Selected in Layers asks (no map layer is
    // active then, so none of theirs was found).
    let picked = if mode == PopupMode::SelectedInContents { s.selected_tile() } else { None };
    let web = matches!(mode, PopupMode::Topmost | PopupMode::Visible) || picked.is_some();
    if web {
        // Topmost reads only those drawn over the feature found, up to the first with any.
        let found = ranked.iter().map(|(at, _)| *at).min().unwrap_or(usize::MAX);
        let keep = |i: usize| !first_layer || place(rustgis_map::Drawn::Tile(i)) < found;
        let tiles = tile_features(s, lon, lat, tol_px, MAX_IDENTIFIED, first_layer, picked, keep);
        ranked.extend(tiles.into_iter().map(|(i, v)| (place(rustgis_map::Drawn::Tile(i)), v)));
    }
    ranked.sort_by_key(|(at, _)| *at);
    let top = ranked.first().map(|(at, _)| *at);
    if first_layer {
        ranked.retain(|(at, _)| Some(*at) == top);
    }
    ranked.truncate(MAX_IDENTIFIED);
    let results: Vec<Value> = ranked.into_iter().map(|(_, v)| v).collect();
    // And the WMS layers drawn there are asked (their answers come later): for topmost, when
    // nothing was found, or the topmost one there has a place over what was (a tile layer under
    // all the map's layers, as all were before #808, isn't asked over what they found).
    let over_top =
        |i: usize| map.tiles.get(i).is_some_and(|t| t.over.is_some()) && top.is_some_and(|top| place(rustgis_map::Drawn::Tile(i)) < top);
    let asks = if web && (!first_layer || results.is_empty() || info::topmost(s, lon, lat).is_some_and(over_top)) {
        info::requests(s, lon, lat, first_layer, picked)
    } else {
        Vec::new()
    };
    let mut out = match results.first().cloned() {
        Some(first) => first,
        // Only the tile layer picked is asked, not a raster's pixel.
        None if picked.is_some() && asks.is_empty() => return Ok(Value::Null),
        None if picked.is_some() => json!({}),
        // No feature: report the pixel of the topmost raster instead.
        None if mode == PopupMode::SelectedInContents && active.and_then(|id| map.layer(id)).is_some_and(|l| !l.is_raster()) => {
            return Ok(Value::Null);
        }
        None => match super::raster::identify(s, lon, lat) {
            Some(pixel) => pixel,
            None if asks.is_empty() => return Ok(Value::Null),
            None => json!({}),
        },
    };
    if let Value::Object(o) = &mut out {
        if !results.is_empty() {
            o.insert("results".into(), Value::Array(results));
        }
        if !asks.is_empty() {
            o.insert("featureInfo".into(), Value::Array(asks));
        }
    }
    Ok(out)
}

/// Whether identify lists a tile layer's features: archives, vector tiles and overlays added as
/// data, not basemaps from the catalog or MapLibre styles, so a click on empty map doesn't list a
/// basemap's roads; one `picked` in the Layers pane whichever it is, a style too.
fn lists_features(t: &rustgis_map::tiles::TileLayer, picked: bool) -> bool {
    use rustgis_map::tiles::TileKind;
    match t.kind {
        TileKind::PmTiles | TileKind::MbTiles | TileKind::Vector => picked || t.id.is_empty() || t.overlay,
        TileKind::Style => picked,
        TileKind::Raster | TileKind::Cog => false,
    }
}

/// The features of the vector tile layers drawn under a point, topmost layer first, each with
/// its tile layer's index (only those `keep` takes; only the first layer with any when
/// `first_layer`): those [`lists_features`] takes, or with `picked` (the tile layer picked in the
/// Layers pane) only that one. The tiles looked at are those drawn there.
fn tile_features(
    s: &Session,
    lon: f64,
    lat: f64,
    tol_px: f64,
    limit: usize,
    first_layer: bool,
    picked: Option<usize>,
    keep: impl Fn(usize) -> bool,
) -> Vec<(usize, Value)> {
    use rustgis_render::rustgis_tiles::{draw, mvt};
    let map = s.map();
    let Some((at, m_per_px)) = info::merc_at(s, lon, lat).filter(|_| limit > 0) else { return Vec::new() };
    let mut out = Vec::new();
    for (i, t) in map.tiles.iter().enumerate() {
        let data = picked.is_none_or(|p| p == i) && lists_features(t, picked.is_some());
        if !data || !t.draws() || t.opacity <= 0.0 || !keep(i) {
            continue;
        }
        let picked = draw::pick(&rustgis_render::tile_spec(t), at.x, at.y, m_per_px, tol_px as f32, limit - out.len());
        for p in &picked {
            let attrs: serde_json::Map<String, Value> = p
                .props
                .iter()
                .map(|(k, v)| {
                    let text = match v {
                        mvt::Value::String(t) => t.clone(),
                        mvt::Value::Number(n) if mvt::is_whole(*n) => format!("{}", *n as i64),
                        mvt::Value::Number(n) => n.to_string(),
                        mvt::Value::Bool(b) => b.to_string(),
                    };
                    (k.clone(), json!(text))
                })
                .collect();
            out.push((i, json!({"layer": format!("{} › {}", t.name, p.source_layer), "tileLayer": t.name, "sourceLayer": p.source_layer, "attributes": attrs})));
        }
        if out.len() >= limit || (first_layer && !picked.is_empty()) {
            break;
        }
    }
    out
}

fn popup_mode(s: &mut Session, p: &Value) -> CmdResult {
    let m = ps(p, "mode").ok_or("mode is required")?;
    let mode = PopupMode::from_code(m).ok_or_else(|| format!("unknown pop-up mode '{m}'"))?;
    s.map_mut().popup_mode = mode;
    Ok(json!({"mode": mode.code()}))
}

fn feature_json(map: &rustgis_map::Map, id: u32, row: usize) -> Option<Value> {
    let l = map.layer(id)?;
    let mut attrs = serde_json::Map::new();
    for (i, fd) in l.dataset.fields.iter().enumerate() {
        attrs.insert(fd.name.clone(), json!(l.dataset.value(row, i).to_string()));
    }
    // The feature id finds the feature again after a streamed layer reads another window.
    let fid = l.dataset.features.get(row).map(|f| f.id);
    Some(json!({"layer": l.name, "layerId": id, "row": row, "fid": fid, "attributes": attrs}))
}

/// Reports invalid radius-search input once per kind, with counts for each layer.
pub(crate) fn warn_radius_input(s: &mut Session, invalid: &[(String, usize, usize)]) {
    for latitude in [true, false] {
        let layers: Vec<String> = invalid
            .iter()
            .filter_map(|(name, bad, missing)| {
                let n = if latitude { *bad } else { *missing };
                (n > 0).then(|| format!("{name} {n}"))
            })
            .collect();
        let total: usize = invalid.iter().map(|(_, bad, missing)| if latitude { *bad } else { *missing }).sum();
        if total > 0 {
            let why = if latitude {
                "have a latitude past ±90° and were left out of the radius search"
            } else {
                "have missing or infinite coordinates; those vertices and their incident edges were left out of the radius measurement"
            };
            s.warn(format!("{total} feature(s) ({}) {why}", layers.join(", ")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Project, Tool};
    use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value as DataValue};
    use rustgis_geom::{lonlat_to_merc, merc_to_lonlat};

    /// The pole warning states the search's limits from the constants that set them.
    #[test]
    fn bounded_warning_states_the_search_limits() {
        let mut s = Session::new(Project::default());
        let mark = s.notices_given();
        warn_bounded(&mut s, &[("Cities".into(), 2)]);
        let text = s.notices_since(mark).first().map(|n| n.text.as_str()).unwrap_or_default();
        assert!(
            text.contains("2 feature(s) near a pole (Cities 2)")
                && text.contains(&format!(
                    "past a search's {} steps a layer",
                    rustgis_map::graphics::thousands(network::REACH_STEPS_TOTAL as f64)
                ))
                && text.contains(&format!("along an edge over {}° long", network::REACH_STEPS_MAX * network::REACH_STEP_DEG)),
            "{text}"
        );
    }

    fn fixture() -> (Session, u32, u32) {
        let mut s = Session::new(Project::default());
        let mut ds =
            Dataset::new("Points", GeometryType::Point, vec![Field::new("n", FieldType::Integer), Field::new("date", FieldType::Text)]);
        for (i, (x, y)) in [(0.5, 0.5), (2.0, 2.0), (3.0, 0.5), (4.0, 4.0)].into_iter().enumerate() {
            ds.push(
                Some(Geometry::Point(Coord::new(x, y))),
                vec![DataValue::Int(i as i64), DataValue::Text(format!("2025-01-0{}T00:00:00Z", i + 1))],
            );
        }
        ds.push(None, vec![DataValue::Int(4), DataValue::Text("2025-01-05T00:00:00Z".into())]);
        let first = s.map_mut().add_dataset(ds.clone());
        ds.name = "Other points".into();
        let second = s.map_mut().add_dataset(ds);
        s.map_mut().layer_mut(second).unwrap().selectable = false;
        (s, first, second)
    }

    fn outline() -> Value {
        json!({"points": [[0,0],[3,0],[3,1],[1,1],[1,3],[0,3]]})
    }

    fn selected(s: &Session, id: u32) -> Vec<usize> {
        s.map().layer(id).unwrap().selection.clone()
    }

    #[test]
    fn map_area_selection_polygon_modes_and_empty_hits() {
        let (mut s, first, second) = fixture();
        let flat = s.map().flat();
        let lonlat = [
            Coord::new(0.0, 0.0),
            Coord::new(3.0, 0.0),
            Coord::new(3.0, 1.0),
            Coord::new(1.0, 1.0),
            Coord::new(1.0, 3.0),
            Coord::new(0.0, 3.0),
        ];
        let projected: Vec<_> = lonlat.iter().map(|p| flat.forward_checked(*p).unwrap()).collect();
        let mask = PolygonMask::new(&projected).unwrap();
        let boundary = flat.forward_checked(Coord::new(3.0, 0.5)).unwrap();
        assert!(mask.intersects(&Geometry::Point(boundary)));
        s.map_mut().layer_mut(second).unwrap().selection = vec![1];
        assert_eq!(s.run("select.byPolygon", &outline()).unwrap(), json!({"selected": 2}), "selection {:?}", selected(&s, first));
        assert_eq!(selected(&s, first), [0, 2]);
        assert!(selected(&s, second).is_empty());
        s.map_mut().layer_mut(first).unwrap().selection = vec![1];
        s.map_mut().layer_mut(second).unwrap().selection = vec![3];
        let mut params = outline();
        params["mode"] = json!("ADD");
        assert_eq!(s.run("select.byPolygon", &params).unwrap(), json!({"selected": 2}));
        assert_eq!(selected(&s, first), [0, 1, 2]);
        assert_eq!(selected(&s, second), [3]);
        params["mode"] = json!("Remove");
        assert_eq!(s.run("select.byPolygon", &params).unwrap(), json!({"selected": 2}));
        assert_eq!(selected(&s, first), [1]);
        assert_eq!(selected(&s, second), [3]);
        for mode in ["add", "remove", "new"] {
            let p = json!({"points": [[10,10],[11,10],[11,11],[10,11]], "mode": mode});
            assert_eq!(s.run("select.byPolygon", &p).unwrap(), json!({"selected": 0}));
            assert_eq!(selected(&s, first), if mode == "new" { vec![] } else { vec![1] });
            assert_eq!(selected(&s, second), if mode == "new" { vec![] } else { vec![3] });
        }
        for tool in [Tool::SelectPolygon, Tool::SelectLasso, Tool::SelectRadius] {
            s.run("tool.set", &json!({"tool": tool.code()})).unwrap();
            assert_eq!(s.tool, tool);
            assert!(!tool.creates());
        }
        s.run("tool.set", &json!({"tool": "select"})).unwrap();
        assert_eq!(s.tool, Tool::Select);
    }

    #[test]
    fn map_area_selection_invalid_queries_preserve_all_selections() {
        let (mut s, first, second) = fixture();
        s.map_mut().layer_mut(first).unwrap().selection = vec![1];
        s.map_mut().layer_mut(second).unwrap().selection = vec![3];
        let mut invalid = vec![
            ("select.byPolygon", json!({"points": [[0,0],[0,0],[0,0]]})),
            ("select.byPolygon", json!({"points": [[0,0],[1,0],[2,0]]})),
            ("select.byPolygon", json!({"points": [[179,0],[-179,0],[-179,1]]})),
            ("select.byPolygon", json!({"points": [[0,86],[1,86],[1,87]]})),
            ("select.byPolygon", json!({"points": [[181,0],[1,0],[1,1]]})),
            ("select.byPolygon", json!({"points": [[0,0],[1,"bad"],[1,1]]})),
            ("select.byRadius", json!({"lon": 0, "lat": 0, "radius": 0})),
            ("select.byRadius", json!({"lon": 0, "lat": 0, "radius": -1})),
            ("select.byRadius", json!({"lon": 0, "lat": 0, "radius": 5000001})),
            ("select.byRadius", json!({"lon": 179.99, "lat": 0, "radius": 100000})),
            ("select.byRadius", json!({"lon": 0, "lat": 85, "radius": 100000})),
        ];
        for mode in [json!("subset"), json!("bogus"), json!(false), Value::Null] {
            let mut polygon = outline();
            polygon["mode"] = mode.clone();
            invalid.push(("select.byPolygon", polygon));
            invalid.push(("select.byRadius", json!({"lon": 0, "lat": 0, "radius": 1000, "mode": mode})));
        }
        invalid.push(("select.byPolygon", json!({"points": vec![[0.0, 0.0]; MAX_POINTS + 1]})));
        let star: Vec<[f64; 2]> = (0..601)
            .map(|i| {
                let angle = (i * 300 % 601) as f64 * std::f64::consts::TAU / 601.0;
                [angle.cos(), angle.sin()]
            })
            .collect();
        invalid.push(("select.byPolygon", json!({"points": star})));
        for (command, params) in invalid {
            assert!(s.run(command, &params).is_err(), "{command}: {params}");
            assert_eq!(selected(&s, first), [1], "{command}: {params}");
            assert_eq!(selected(&s, second), [3], "{command}: {params}");
        }
        s.run("map.projection", &json!({"type": "robinson", "lon_0": 150})).unwrap();
        let seam = json!({"points": [[-35,5],[-25,5],[-25,15],[-35,15]]});
        assert!(s.run("select.byPolygon", &seam).is_err());
        assert_eq!(selected(&s, first), [1]);
        assert_eq!(selected(&s, second), [3]);
    }

    #[test]
    fn map_area_selection_eligibility_matches_rectangle() {
        let operations = [
            ("select.byRect", json!({"minLon": -1, "minLat": -1, "maxLon": 5, "maxLat": 5})),
            ("select.byPolygon", json!({"points": [[-1,-1],[5,-1],[5,5],[-1,5]]})),
            ("select.byRadius", json!({"lon": 2, "lat": 2, "radius": 1000000})),
        ];
        for (command, params) in operations {
            for restriction in 0..10 {
                let (mut s, first, _) = fixture();
                let scale = s.scale();
                let l = s.map_mut().layer_mut(first).unwrap();
                match restriction {
                    0 => l.visible = false,
                    1 => l.selectable = false,
                    2 => l.min_scale = Some(scale * 0.5),
                    3 => l.max_scale = Some(scale * 2.0),
                    4..=6 => {}
                    7 => l.definition_query = Some("n = 2".into()),
                    8 => {
                        l.display_filters = vec![rustgis_map::effects::DisplayFilter {
                            name: "Only 2".into(),
                            min_scale: None,
                            max_scale: None,
                            where_: "n = 2".into(),
                        }]
                    }
                    9 => {
                        let instant = rustgis_map::time::instants(&l.dataset, "date")[2].unwrap();
                        let mut binding = rustgis_map::time::TimeBinding::new("date");
                        binding.window = Some((instant, instant));
                        l.time = Some(binding);
                    }
                    _ => {}
                }
                if (4..=6).contains(&restriction) {
                    let mut group = rustgis_map::LayerGroup::new("Limited");
                    group.children.push(first);
                    match restriction {
                        4 => group.visible = false,
                        5 => group.min_scale = Some(scale * 0.5),
                        _ => group.max_scale = Some(scale * 2.0),
                    }
                    s.map_mut().groups.push(group);
                }
                s.run(command, &params).unwrap();
                let expected = if restriction < 7 { vec![] } else { vec![2] };
                assert_eq!(selected(&s, first), expected, "{command}: restriction {restriction}");
            }
        }
    }

    #[test]
    fn map_area_selection_radius_reuses_legacy_metric_and_prefilter() {
        for latitude in [0.0_f64, 60.0] {
            let center = Coord::new(0.0, latitude);
            let origin = lonlat_to_merc(center);
            let k = 1.0 / latitude.to_radians().cos().max(0.01);
            let at = |x: f64, y: f64| Coord::new(origin.x + x * k, origin.y + y * k);
            let ring = |r: f64| vec![at(-r, -r), at(r, -r), at(r, r), at(-r, r), at(-r, -r)];
            let geometries = [
                Geometry::Point(at(0.0, 0.0)),
                Geometry::Point(at(600.0, 800.0)),
                Geometry::Point(at(1200.0, 0.0)),
                Geometry::Line(vec![vec![at(-2000.0, 0.0), at(2000.0, 0.0)]]),
                Geometry::Polygon(vec![vec![ring(2000.0)]]),
                Geometry::Polygon(vec![vec![ring(3000.0), ring(2000.0)]]),
                Geometry::MultiPoint(vec![at(500.0, 0.0), at(2000.0, 0.0)]),
            ];
            let endpoint = merc_to_lonlat(at(600.0, 800.0));
            let radius = network::radius_distance_m(center, endpoint).unwrap();
            let mut ds = Dataset::new("Radius", GeometryType::Polygon, vec![]);
            for geometry in geometries {
                ds.push(Some(geometry.map_coords(&merc_to_lonlat)), vec![]);
            }
            let mut expected: Vec<usize> =
                network::within(&ds, &(0..ds.features.len()).collect::<Vec<_>>(), center, radius).into_iter().map(|(row, _)| row).collect();
            expected.sort_unstable();
            assert_eq!(expected, [0, 1, 3, 4, 6]);
            let mut s = Session::new(Project::default());
            let id = s.map_mut().add_dataset(ds);
            assert_eq!(s.run("select.byRadius", &json!({"lon": 0, "lat": latitude, "radius": radius})).unwrap(), json!({"selected": 5}));
            assert_eq!(selected(&s, id), expected);
            for mode in ["add", "remove", "new"] {
                s.map_mut().layer_mut(id).unwrap().selection = vec![0];
                let p = json!({"lon": 10, "lat": latitude, "radius": 1, "mode": mode});
                assert_eq!(s.run("select.byRadius", &p).unwrap(), json!({"selected": 0}));
                assert_eq!(selected(&s, id), if mode == "new" { vec![] } else { vec![0] });
            }
        }
        let center = Coord::new(0.0, 60.0);
        let point = Coord::new(0.0, 59.1);
        assert!(network::radius_distance_m(center, point).unwrap() < 100000.0);
        let mut ds = Dataset::new("Legacy prefilter", GeometryType::Point, vec![]);
        ds.push(Some(Geometry::Point(point)), vec![]);
        let mut s = Session::new(Project::default());
        let id = s.map_mut().add_dataset(ds);
        assert_eq!(s.run("select.byRadius", &json!({"lon": 0, "lat": 60, "radius": 100000})).unwrap(), json!({"selected": 0}));
        assert!(selected(&s, id).is_empty());
    }

    #[test]
    fn map_area_selection_polygon_uses_active_projected_geometry() {
        for projection in ["mercator", "robinson", "EPSG:32631"] {
            let mut s = Session::new(Project::default());
            s.run("map.projection", &json!({"type": projection})).unwrap();
            let flat = s.map().flat();
            let origin = flat.forward_checked(Coord::new(3.0, 50.0)).unwrap();
            let at = |x: f64, y: f64| Coord::new(origin.x + x * 10000.0, origin.y + y * 10000.0);
            let ring = |a: f64, b: f64| vec![at(a, a), at(b, a), at(b, b), at(a, b), at(a, a)];
            let geometries = [
                Geometry::Point(at(0.5, 0.5)),
                Geometry::Polygon(vec![vec![ring(-1.0, 5.0), ring(-0.5, 3.5)]]),
                Geometry::Line(vec![vec![at(-1.0, 0.5), at(4.0, 0.5)]]),
                Geometry::Polygon(vec![vec![ring(-1.0, 5.0)]]),
                Geometry::Point(at(2.0, 2.0)),
            ];
            let mut ds = Dataset::new("Projected candidates", GeometryType::Polygon, vec![]);
            for geometry in geometries {
                ds.push(Some(geometry.map_coords(&|p| flat.inverse(p).unwrap())), vec![]);
            }
            let id = s.map_mut().add_dataset(ds);
            let points: Vec<[f64; 2]> = [(0.0, 0.0), (3.0, 0.0), (3.0, 1.0), (1.0, 1.0), (1.0, 3.0), (0.0, 3.0)]
                .into_iter()
                .map(|(x, y)| {
                    let p = flat.inverse(at(x, y)).unwrap();
                    [p.x, p.y]
                })
                .collect();
            assert_eq!(s.run("select.byPolygon", &json!({"points": points})).unwrap(), json!({"selected": 3}), "{projection}");
            assert_eq!(selected(&s, id), [0, 2, 3], "{projection}");
        }
    }

    /// Identify lists the features of tile layers added as data, not basemaps or MapLibre styles,
    /// unless picked in the Layers pane; raster tiles have none.
    #[test]
    fn identify_lists_tile_features_of_data_and_of_the_layer_picked() {
        use rustgis_map::tiles::{TileKind, TileLayer};
        let tile = |kind, id: &str, overlay| TileLayer { id: id.into(), overlay, ..TileLayer::new("t", kind, "https://example.com/t") };
        for kind in [TileKind::PmTiles, TileKind::MbTiles, TileKind::Vector] {
            assert!(lists_features(&tile(kind, "", false), false), "{kind:?} added as data");
            assert!(lists_features(&tile(kind, "base", true), false), "{kind:?} overlay");
            assert!(!lists_features(&tile(kind, "base", false), false), "{kind:?} basemap");
            assert!(lists_features(&tile(kind, "base", false), true), "{kind:?} basemap picked");
        }
        assert!(!lists_features(&tile(TileKind::Style, "", false), false));
        assert!(lists_features(&tile(TileKind::Style, "", false), true));
        for kind in [TileKind::Raster, TileKind::Cog] {
            assert!(!lists_features(&tile(kind, "", false), true), "{kind:?}");
        }
    }
}
