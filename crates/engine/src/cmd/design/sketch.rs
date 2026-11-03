//! Editing with a drawn line: Cut Polygon, Reshape, and tracing along a feature's edge while
//! drawing. The geometry is worked out in `rustgis_analysis::sketch_edit`.

use rustgis_analysis::sketch_edit;
use rustgis_data::Geometry;
use rustgis_geom::Coord;
use serde_json::{Value, json};

use super::{Change, apply, apply_with, selection_layer};
use crate::{CmdResult, Session};

/// Most vertices a drawn line may have (Cut Polygon, Reshape); a sketch stops tracing at it.
pub const MAX_LINE: usize = 10_000;

/// The `line` parameter: a GeoJSON LineString in longitude/latitude.
fn line(p: &Value) -> Result<Vec<Coord>, String> {
    let g = p.get("line").and_then(rustgis_data::io::geometry_from_geojson).ok_or("line must be a GeoJSON LineString")?;
    match g {
        Geometry::Line(mut parts) if parts.len() == 1 => {
            let l = parts.pop().unwrap_or_default();
            if l.len() < 2 || l.len() > MAX_LINE {
                return Err("the line needs 2 to 10,000 points (trace a shorter stretch of a detailed edge)".into());
            }
            // Longitudes as far as ±360°, as `edit.vertex` takes (a feature unwrapped across
            // the antimeridian).
            if l.iter().any(|c| !c.is_finite() || c.x.abs() > 360.0 || c.y.abs() > 90.0) {
                return Err("the line's points are [lon, lat] in degrees".into());
            }
            Ok(l)
        }
        _ => Err("line must be one GeoJSON LineString".into()),
    }
}

/// A `[lon, lat]` parameter.
fn point(p: &Value, key: &str) -> Result<Coord, String> {
    p.get(key)
        .and_then(Value::as_array)
        .and_then(|a| Some(Coord::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?)))
        .filter(|c| c.is_finite() && c.x.abs() <= 360.0 && c.y.abs() <= 90.0)
        .ok_or_else(|| format!("{key} is [lon, lat]"))
}

/// Cuts the selected polygons along a line drawn across them. Each piece keeps the attributes of
/// the feature it was cut from; the original row keeps the largest piece (and any parts the line
/// missed), and the pieces end up selected. Every cut is worked out before anything changes, so a
/// refused one leaves the layer as it was. Says how many selected features the line didn't cut.
pub(super) fn cut_polygon(s: &mut Session, p: &Value) -> CmdResult {
    let line = line(p)?;
    let id = selection_layer(s)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let mut cuts = Vec::new();
    let (mut missed, mut work) = (0, 0);
    for &r in &l.selection {
        let Some(f) = l.dataset.features.get(r) else { continue };
        match f.geometry.as_ref().map(|g| sketch_edit::cut_polygon(g, &line, &mut work)).transpose()?.flatten() {
            Some(pieces) => {
                for g in &pieces {
                    super::super::edit::validate(g)?;
                }
                cuts.push((r, f.clone(), pieces));
            }
            None => missed += 1,
        }
    }
    if cuts.is_empty() {
        return Err("the line doesn't cut a selected polygon: draw it across the polygon, from outside to outside".into());
    }
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    let (mut changes, mut selection) = (Vec::new(), Vec::new());
    for (r, before, pieces) in cuts {
        let mut pieces = pieces.into_iter();
        if let (Some(first), Some(feat)) = (pieces.next(), l.dataset.features.get_mut(r)) {
            feat.geometry = Some(first);
        }
        changes.push(Change { before: Some(before.clone()), row: Some(r) });
        selection.push(r);
        for g in pieces {
            l.dataset.push(Some(g), before.attrs.clone());
            let row = l.dataset.features.len() - 1;
            changes.push(Change { before: None, row: Some(row) });
            selection.push(row);
        }
    }
    let cut = changes.iter().filter(|c| c.before.is_some()).count();
    let pieces = selection.len();
    l.selection = selection;
    l.touch_geometry();
    // The pieces are the feature cut up, not new features: no creation defaults.
    apply_with(s, id, changes, false)?;
    if missed > 0 {
        s.notify(format!("Cut Polygon left {missed} selected feature(s) the line didn't cut"), false);
    }
    Ok(json!({"cut": cut, "pieces": pieces, "missed": missed}))
}

/// Reshapes the one selected line or polygon with a line drawn across its boundary.
pub(super) fn reshape(s: &mut Session, p: &Value) -> CmdResult {
    let line = line(p)?;
    let id = selection_layer(s)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    let [r] = l.selection.as_slice() else { return Err("select one feature to reshape".into()) };
    let r = *r;
    let f = l.dataset.features.get(r).cloned().ok_or("no such row")?;
    let done = sketch_edit::reshape(f.geometry.as_ref().ok_or("the feature has no shape")?, &line)?;
    super::super::edit::validate(&done.geometry)?;
    if let Some(feat) = l.dataset.features.get_mut(r) {
        feat.geometry = Some(done.geometry);
    }
    l.touch_geometry();
    apply(s, id, vec![Change { before: Some(f), row: Some(r) }])?;
    let n = done.holes_removed;
    if n > 0 {
        s.notify(format!("Reshape removed {n} hole(s) left outside the new boundary of row {r}"), false);
    }
    Ok(json!({"row": r, "holesRemoved": n}))
}

/// The path along a feature's edge from `from` to `to`: a given feature's (`layer`, `row`), or
/// else the nearest line or polygon edge passing within the tolerance of both points (in `layer`
/// when it is given). Returns how far (Web Mercator metres) each point was from the edge it was
/// moved onto.
pub(super) fn trace(s: &mut Session, p: &Value) -> CmdResult {
    let (from, to) = (point(p, "from")?, point(p, "to")?);
    let (id, row, part) = match p.get("row") {
        Some(r) => {
            let row = r.as_u64().and_then(|r| usize::try_from(r).ok()).ok_or("row must be a whole number")?;
            (s.layer_id(p)?, row, None)
        }
        None => {
            let px = super::f(p, "tolerancePx").unwrap_or(s.map().editing.snapping.tolerance_px).clamp(0.0, 100.0);
            // A given layer limits the search to its features.
            let only = if p.get("layer").is_some() { Some(s.layer_id(p)?) } else { None };
            let (id, row, part) = nearest_edge(s, from, to, px, only).ok_or("no line or polygon edge passes near both points")?;
            // The part chosen on the map, so the path follows the same one. When the projection
            // split the feature, that part can't be named in the source: with more than one to
            // choose from, ask for the row (the map's Trace doesn't follow it either).
            let parts =
                s.map().layer(id).and_then(|l| l.dataset.features.get(row)).and_then(|f| f.geometry.as_ref()).map_or(0, |g| match g {
                    Geometry::Line(p) => p.len(),
                    Geometry::Polygon(p) => p.iter().map(Vec::len).sum(),
                    _ => 0,
                });
            if part.is_none() && parts > 1 {
                return Err("that feature is split where the map projection cuts it: give its layer and row to trace it".into());
            }
            (id, row, part)
        }
    };
    let g = s
        .map()
        .layer(id)
        .and_then(|l| l.dataset.features.get(row))
        .ok_or("no such row")?
        .geometry
        .as_ref()
        .ok_or("the feature has no shape")?;
    let t = sketch_edit::trace(g, from, to, part)?;
    let path: Vec<[f64; 2]> = t.path.iter().map(|c| [c.x, c.y]).collect();
    // How far each point was moved onto the edge, so a far-off point isn't taken silently.
    Ok(json!({"layer": id, "row": row, "path": path, "fromOffM": t.from_m, "toOffM": t.to_m}))
}

/// The path along the nearest drawn line or polygon edge passing within the snapping tolerance of
/// both points, if any: Trace while drawing. Read-only, so a click away from an edge is just an
/// ordinary vertex rather than a failed command.
pub fn trace_between(s: &mut Session, from: Coord, to: Coord) -> Option<Vec<Coord>> {
    let px = s.map().editing.snapping.tolerance_px;
    // A feature the projection split at its edge: its parts on the map aren't the source's, so
    // the ring under the clicks can't be named. Not traced; the click is an ordinary vertex.
    let (id, row, part) = nearest_edge(s, from, to, px, None)?;
    let part = part?;
    let g = s.map().layer(id)?.dataset.features.get(row)?.geometry.as_ref()?;
    sketch_edit::trace(g, from, to, Some(part)).ok().map(|t| t.path)
}

/// The drawn line or polygon feature with a ring or part passing nearest both points, each within
/// `px` pixels, in layer `only` when given: (layer id, row, part or ring when the map's parts are
/// the source's). Measured on the map as drawn, like snapping; rows a definition
/// query or the time window hides aren't candidates.
fn nearest_edge(s: &mut Session, from: Coord, to: Coord, px: f64, only: Option<u32>) -> Option<(u32, usize, Option<usize>)> {
    let (a, b) = (s.map().to_map(from)?, s.map().to_map(to)?);
    let tol = px * s.map().camera.res;
    let scale = s.scale();
    let (flat, key) = (s.map().flat(), s.map().projection.code());
    let held = s.map().held_by_group(scale);
    let mut best: Option<(f64, u32, usize, Option<usize>)> = None;
    for l in s.map_mut().layers.iter_mut().filter(|l| l.visible && !l.is_raster() && !l.graphics && only.is_none_or(|o| o == l.id)) {
        if !l.in_scale_range(scale) || held.contains(&l.id) {
            continue;
        }
        let id = l.id;
        let drawn = l.drawn_rows(scale);
        l.projected_in(&flat, &key);
        for (row, g) in l.projected_cache().iter().enumerate() {
            let Some(g) = g.as_ref().filter(|_| drawn.get(row) == Some(&true)) else { continue };
            let e = g.extent().padded(0.0, tol);
            if !e.contains(a) || !e.contains(b) {
                continue;
            }
            let paths: Vec<&Vec<Coord>> = match g {
                Geometry::Line(parts) => parts.iter().collect(),
                Geometry::Polygon(polys) => polys.iter().flatten().collect(),
                _ => continue,
            };
            // Both points near the same ring or part: the one the path will follow.
            let near =
                |p: &Vec<Coord>, c: Coord| p.windows(2).filter_map(|w| Some(off(c, *w.first()?, *w.get(1)?))).fold(f64::MAX, f64::min);
            let Some((part, d)) = paths.iter().map(|p| near(p, a).max(near(p, b))).enumerate().min_by(|x, y| x.1.total_cmp(&y.1)) else {
                continue;
            };
            // A projection that cuts features at its edge can split or drop parts, so the map's
            // part numbers may not be the source's. The chosen part is passed on only when it is
            // the source part of that number, projected (same length, same first, middle and
            // last points); otherwise the trace picks the part itself.
            let source: Vec<&Vec<Coord>> = match l.dataset.features.get(row).and_then(|f| f.geometry.as_ref()) {
                Some(Geometry::Line(parts)) => parts.iter().collect(),
                Some(Geometry::Polygon(polys)) => polys.iter().flatten().collect(),
                _ => Vec::new(),
            };
            let same = |src: &Vec<Coord>, drawn: &Vec<Coord>| {
                let n = src.len();
                n == drawn.len()
                    && [0, n / 2, n.saturating_sub(1)].iter().all(|&i| match (src.get(i).and_then(|c| flat.forward(*c)), drawn.get(i)) {
                        (Some(a), Some(b)) => (a.x - b.x).abs() + (a.y - b.y).abs() <= 1e-6 * (1.0 + a.x.abs() + a.y.abs()),
                        _ => false,
                    })
            };
            let part = (source.len() == paths.len() && matches!((source.get(part), paths.get(part)), (Some(s), Some(d)) if same(s, d)))
                .then_some(part);
            if d <= tol && best.is_none_or(|x| d < x.0) {
                best = Some((d, id, row, part));
            }
        }
    }
    best.map(|(_, id, row, part)| (id, row, part))
}

/// How far `c` is from segment `a`–`b`.
fn off(c: Coord, a: Coord, b: Coord) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((c.x - a.x) * dx + (c.y - a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (c.x - a.x - t * dx).hypot(c.y - a.y - t * dy)
}
