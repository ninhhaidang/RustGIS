//! Analyze tab, imagery groups: georeferencing, change detection, spectral profiles, classification (k-means and
//! minimum distance), indices, processing (clip, resample), the pixel editor, mensuration, and
//! ortho and reality mapping workspaces (mosaics of georeferenced images, DSM meshes).

use std::sync::Arc;

use rustgis_analysis::georef::{self, Gcp, Kind};
use rustgis_analysis::rasterops::{self, NODATA};
use rustgis_analysis::warp;
use rustgis_data::raster::{self, ColorHint, Crs, GeoTransform, Raster};
use rustgis_data::{FieldType, Geometry, GeometryType};
use rustgis_geom::{Coord, Extent};
use rustgis_map::RasterLayer;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{b, f, ps, uint};
use crate::{CmdResult, CommandSpec as C, Session};

/// An imagery workspace: images mosaicked into one product.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub name: String,
    /// "ortho" (an orthomosaic of images) or "reality" (a surface model from elevation rasters).
    pub kind: String,
    pub images: Vec<String>,
    /// Output cell size in degrees (the finest image's when unset).
    #[serde(default)]
    pub cell_size: Option<f64>,
    /// Where images overlap: "first" (the first image wins) or "mean".
    #[serde(default = "first")]
    pub method: String,
}

fn first() -> String {
    "first".into()
}

pub fn commands() -> Vec<C> {
    vec![
        C::new("raster.georeference", "Georeference", "Analyze › Alignment", georeference).inputs(raster_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layer": raster, "points"?: [[column, row, lon, lat], …] (two or more; with "transform": [column, row, x, y, on?]), "extent"?: [minLon, minLat, maxLon, maxLat], "transform"?: "similarity"|"affine"|"projective"|"polynomial2"|"polynomial3"|"spline" (fits the points and warps the raster onto a north-up grid covering the warped image; without it the points and extent only shift and scale the grid), "pointsCrs"?: EPSG code or "EPSG:n" of the points' x and y (default 4326), "crs"?: the output's (default pointsCrs), "resampling"?: "nearest"|"bilinear"|"cubic" (default bilinear), "cellSize"?: output units (default about the source pixel's size on the ground), "name"?: new layer for the result (default replaces the layer's raster), "path"?: "*.tif" (also writes the warp as a cloud-optimised GeoTIFF)}"#,
        ),
        C::new("raster.project", "Project Raster", "Analyze › Alignment", project_raster).inputs(project_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layer": raster, "crs"?: EPSG code or "EPSG:n" of the output (default the layer's), "cellSize"?: number or [x, y] in the output's units (default about the layer's pixel there), "origin"?: [x, y] a cell corner falls on (default [0, 0], so grids of the same cellSize line up), "extent"?: polygon layer (its selected or all features' bounds) | [west, south, east, north] in degrees | "view" (default the layer's footprint), "grid"?: raster layer whose coordinate system, cells and extent are taken exactly (instead of crs, cellSize, origin and extent), "resampling"?: "nearest" (default) | "bilinear" | "cubic" | "average" (the mean of the pixels whose centres fall in a cell) | "mode" (their most frequent value, the least of those tied), "name"?: new layer (default "<layer> projected"), "path"?: GeoTIFF to write it to}"#,
        ),
        C::new("raster.rasterize", "Polygons to Raster", "Analyze › Alignment", rasterize).inputs(rasterize_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layer": polygon layer (its selected or all features; a later one over an earlier), "field"?: numeric field (its values) or text field (codes 1 to n in sorted order of its values, listed in a table named after the result with classes), "value"?: number every polygon gives when there is no field (default 1), "nodata"?: number of the cells under no polygon (default 0), "crs"?: EPSG code or "EPSG:n" of the output (default the layer's), "cellSize"?: number or [x, y] in the output's units (required without grid), "origin"?: [x, y] a cell corner falls on (default [0, 0], so grids of the same cellSize line up), "extent"?: polygon layer (its selected or all features' bounds) | [west, south, east, north] in degrees | "view"; default the polygons' bounds, "grid"?: raster layer whose coordinate system, cells and extent are taken exactly (instead of crs, cellSize, origin and extent), "name"?: new layer (default "<layer> raster"), "path"?: GeoTIFF to write it to}"#,
        ),
        // A fit is a measurement of the points, not a project change: no undo step.
        C::new("raster.gcpFit", "Control Point Fit", "Analyze › Alignment", gcp_fit).roles(&[]).effects(crate::policy::Effects::NONE).view().params(
            r#"{"points": [[column, row, x, y, on?], …] (on: 1/0 or true/false, default on), "transform"?: "similarity"|"affine"|"projective"|"polynomial2"|"polynomial3"|"spline" (default: similarity under 3 points, else affine), "compare"?: bool (fit every transform and recommend one; transform and leaveOneOut are then ignored), "leaveOneOut"?: bool (default true)}"#,
        ),
        C::new("raster.change", "Change Detection", "Analyze › Image Analysis", change).inputs(change_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"before": raster, "after": raster, "band"?: 1-based, "method"?: "difference"|"ratio"|"percent", "name"?, "ran"?: (internal: a raster the app has worked out)}"#),
        C::new("raster.spectral", "Spectral Analysis", "Analyze › Image Analysis", spectral).inputs(spectral_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .view()
            .params(r#"{"layer": raster, "lon"?, "lat"?, "zone"?: polygon layer (the selected or first polygon)}"#),
        C::new("raster.classify", "Classification Wizard", "Analyze › Image Classification", |s, p| {
            super::workflows::made_or_ran(s, p, "raster.classify")
        }).inputs(raster_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"layer": raster, "classes"?: 2..50, "bands"?: [1-based], "name"?: output, "ran"?: (internal: classes the app has worked out)}"#),
        C::new("raster.classifySupervised", "Classification Tools", "Analyze › Image Classification", supervised).inputs(supervised_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"layer": raster, "training": polygon layer, "classField": field, "bands"?: [1-based], "name"?: output, "ran"?: (internal: a raster the app has worked out)}"#),
        C::new("raster.index", "Indices", "Analyze › Imagery Tools", index).inputs(raster_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layer": raster, "index": "ndvi"|"ndwi"|"savi"|"nbr"|"ndbi"|"evi", "red"?, "green"?, "blue"?, "nir"?, "swir"?: band numbers, "name"?: output, "ran"?: (internal: a raster the app has worked out)}"#,
        ),
        C::new("raster.process", "Process", "Analyze › Imagery Tools", process).inputs(process_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"layer": raster, "operation": "clip"|"resample", "extent"?: [minLon, minLat, maxLon, maxLat] (clip; the view when omitted), "polygon"?: polygon layer (clip to its selected or all polygons; cells outside become no-data), "selectedOnly"?: bool (clip to selected polygons when true, all when false), "factor"?: n (resample: n×n pixels become one), "name"?: output, "ran"?: (internal: a raster the app has worked out)}"#),
        C::new("raster.mosaic", "Mosaic Rasters", "Analyze › Imagery Tools", raster_mosaic).inputs(mosaic_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).params(
            r#"{"inputs": [raster layer or file path, …] (two or more, in one coordinate system and cell size; where they overlap the first wins), "name"?: output, "path"?: GeoTIFF to write the mosaic to}"#,
        ),
        C::new("raster.editPixels", "Pixel Editor", "Analyze › Imagery Tools", edit_pixels).inputs(raster_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"layer": raster, "polygon": [[lon, lat], …], "value"?: number (no-data when omitted), "band"?: 1-based (all when omitted)}"#),
        // Measurements are session results, not project changes: no undo step.
        C::new("raster.measure", "Mensuration Tools", "Analyze › Mensuration", measure).inputs(measure_inputs).roles(&[]).effects(crate::policy::Effects::NONE).view().params(
            r#"{"layer"?: elevation raster, "kind": "distance"|"area"|"height"|"volume", "points": [[lon, lat], …], "base"?: metres (volume)}"#,
        ),
        C::new("raster.measurements", "Results", "Analyze › Mensuration", |s, p| {
            if b(p, "clear").unwrap_or(false) {
                s.measurements.clear();
            }
            Ok(Value::Array(s.measurements.clone()))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .params(r#"{"clear"?: bool}"#),
        C::new("imagery.workspace", "New Workspace", "Analyze › Ortho Mapping", new_workspace).inputs(workspace_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"kind": "ortho"|"reality", "name"?: string, "images": [raster layers], "cellSize"?: degrees, "method"?: "first"|"mean"}"#,
        ),
        C::new("imagery.workspaceOptions", "Workspace Options", "Analyze › Ortho Mapping", |s, p| {
            let n = ps(p, "name").ok_or("name is required")?;
            let w = s.project.workspaces.iter_mut().find(|w| w.name.eq_ignore_ascii_case(n)).ok_or_else(|| format!("no workspace '{n}'"))?;
            if p.get("cellSize").is_some() {
                w.cell_size = cell_size(p)?;
            }
            if ps(p, "method").is_some() {
                w.method = method(p, "first")?;
            }
            serde_json::to_value(&*w).map_err(|e| e.to_string())
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name": workspace, "cellSize"?: degrees|null, "method"?: "first"|"mean"}"#),
        C::new("imagery.mosaic", "Build Mosaic", "Analyze › Ortho Mapping", mosaic).inputs(workspace_mosaic_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"name": workspace, "ran"?: (internal: a raster the app has worked out)}"#),
        C::new("imagery.mesh", "Export Mesh", "Analyze › Reality Mapping", mesh).inputs(raster_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"layer": elevation raster, "path"?: "*.obj" (the text is returned without one), "step"?: pixels between vertices}"#),
    ]
}

fn raster_named(input: &impl super::InputSource, n: &str) -> Result<Arc<Raster>, String> {
    let context = input.input_context();
    let s = &context;
    s.map().layer_by_name(n).and_then(|l| l.raster.as_ref()).and_then(|r| r.data.clone()).ok_or_else(|| format!("no raster layer '{n}'"))
}

pub(crate) fn raster(s: &Session, p: &Value) -> Result<Arc<Raster>, String> {
    raster_named(s, raster_name(p)?)
}

pub(crate) fn name(p: &Value, d: String) -> String {
    ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(|n| n.chars().take(200).collect()).unwrap_or(d)
}

pub(crate) fn bands_param(p: &Value, r: &Raster) -> Result<Vec<usize>, String> {
    match p.get("bands").and_then(Value::as_array) {
        Some(a) => a
            .iter()
            .map(|v| {
                v.as_u64()
                    .map(|b| b as usize)
                    .filter(|b| *b >= 1 && *b <= r.band_count())
                    .map(|b| b - 1)
                    .ok_or_else(|| format!("{} has bands 1 to {}", r.name, r.band_count()))
            })
            .collect(),
        None => Ok((0..r.band_count().min(4)).collect()),
    }
}

/// Embeds a raster as GeoTIFF bytes (colour rasters keep their bands; others band 1).
fn layer_of(r: Raster) -> Result<rustgis_map::RasterLayer, String> {
    let bytes = rustgis_data::raster::geotiff::write(&r, 0).map_err(|e| e.to_string())?;
    let fname = format!("{}.tif", r.name);
    Ok(rustgis_map::RasterLayer::from_bytes(r, fname, bytes))
}

/// Adds a raster layer; classes get unique-value colours.
pub(crate) fn add(s: &mut Session, r: Raster, classes: bool) -> CmdResult {
    let n = r.name.clone();
    let layer = if classes { class_layer(r)? } else { layer_of(r)? };
    super::raster::add_raster(s, &n, layer)
}

/// A raster of classes as a layer, each class a colour of its own.
pub(crate) fn class_layer(r: Raster) -> Result<rustgis_map::RasterLayer, String> {
    let mut layer = layer_of(r)?;
    if let Some(d) = layer.data.clone() {
        layer.style.renderer = rustgis_map::raster::unique_values(&d, 0, rustgis_map::Ramp::Harbour);
    }
    Ok(layer)
}

/// What the Classification Wizard (k-means) reads of the session, and the work that makes its
/// classes.
pub(crate) fn classify_work(s: &Session, p: &Value) -> Result<super::workflows::Work, String> {
    let r = raster(s, p)?;
    let bands = bands_param(p, &r)?;
    if bands.is_empty() {
        return Err("give at least one band".into());
    }
    let k = match p.get("classes") {
        None => 5,
        Some(_) => uint(p, "classes").filter(|k| (2..=50).contains(k)).ok_or("classes is 2 to 50")? as usize,
    };
    let out = name(p, format!("{}_classes", r.name));
    Ok(Box::new(move || {
        let classes = rasterops::kmeans(&r, &bands, k, 20)?;
        let layer = class_layer(rasterops::derive(&r, &out, vec![classes])?)?;
        Ok(super::workflows::made_layer(out, layer, None, None))
    }))
}

/// Replaces a raster layer's pixels, keeping its look.
fn replace(s: &mut Session, layer: &str, r: Raster) -> CmdResult {
    replace_layer(s, layer, layer_of(r)?)
}

/// [`replace`], with the new pixels already in a layer (kept in a file, say, not embedded).
fn replace_layer(s: &mut Session, layer: &str, mut new: rustgis_map::RasterLayer) -> CmdResult {
    let id = s.map().layer_by_name(layer).map(|l| l.id).ok_or_else(|| format!("no raster layer '{layer}'"))?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    if let Some(old) = &l.raster {
        new.style = old.style.clone();
    }
    l.raster = Some(new);
    Ok(json!({"layer": id}))
}

/// A copy of `r`'s bands, whole, their memory reserved first: a raster the machine can't hold
/// twice is an error, not a crash (#1291). A band of the wrong length is copied as it is, for
/// `Raster::new` to refuse.
fn copied(r: &Raster) -> Result<Vec<Vec<f32>>, String> {
    let (w, h) = (r.width as usize, r.height as usize);
    r.bands
        .iter()
        .map(|b| {
            let mut copy = raster::room(b.data.len(), w, h)?;
            copy.extend_from_slice(&b.data);
            Ok(copy)
        })
        .collect::<Result<_, rustgis_data::DataError>>()
        .map_err(|e| e.to_string())
}

fn rebuild(r: &Raster, transform: GeoTransform, crs: Crs, bands: Vec<Vec<f32>>) -> Result<Raster, String> {
    let mut out = Raster::new(r.name.clone(), r.width, r.height, bands, transform, crs, r.nodata, r.color.clone(), r.data_type.clone())
        .map_err(|e| e.to_string())?;
    for (o, i) in out.bands.iter_mut().zip(&r.bands) {
        o.name = i.name.clone();
    }
    Ok(out)
}

/// Fits a north-up longitude/latitude grid to control points (least squares, separately in x
/// and y), or stretches the raster over an extent.
fn georeference(s: &mut Session, p: &Value) -> CmdResult {
    let layer = ps(p, "layer").ok_or("layer is required")?.to_string();
    let r = raster(s, p)?;
    if p.get("transform").is_some() {
        return warp_to_fit(s, p, &layer, &r);
    }
    let gt = if let Some(e) = p.get("extent").and_then(Value::as_array) {
        let n: Vec<f64> = e.iter().filter_map(Value::as_f64).collect();
        let [x0, y0, x1, y1] = n.as_slice() else { return Err("extent is [minLon, minLat, maxLon, maxLat]".into()) };
        if !(x0 < x1 && y0 < y1) {
            return Err("extent is [minLon, minLat, maxLon, maxLat]".into());
        }
        GeoTransform { origin_x: *x0, origin_y: *y1, pixel_width: (x1 - x0) / r.width as f64, pixel_height: -(y1 - y0) / r.height as f64 }
    } else {
        let pts: Vec<[f64; 4]> = p
            .get("points")
            .and_then(Value::as_array)
            .ok_or("give control points or an extent")?
            .iter()
            .take(1000)
            .map(|v| {
                let a: Vec<f64> =
                    v.as_array().map(|a| a.iter().filter_map(Value::as_f64).filter(|x| x.is_finite()).collect()).unwrap_or_default();
                if let [c, rr, x, y] = a.as_slice() {
                    Ok([*c, *rr, *x, *y])
                } else {
                    Err("each control point is [column, row, lon, lat]".to_string())
                }
            })
            .collect::<Result<_, _>>()?;
        if pts.len() < 2 {
            return Err("give at least two control points [column, row, lon, lat]".into());
        }
        // y = a·t + b by least squares.
        let fit = |ts: &[f64], ys: &[f64]| -> Option<(f64, f64)> {
            let n = ts.len() as f64;
            let (mt, my) = (ts.iter().sum::<f64>() / n, ys.iter().sum::<f64>() / n);
            let stt: f64 = ts.iter().map(|t| (t - mt).powi(2)).sum();
            if stt < 1e-12 {
                return None;
            }
            let a = ts.iter().zip(ys).map(|(t, y)| (t - mt) * (y - my)).sum::<f64>() / stt;
            Some((a, my - a * mt))
        };
        let (cols, lons): (Vec<f64>, Vec<f64>) = pts.iter().map(|p| (p[0], p[2])).unzip();
        let (rows, lats): (Vec<f64>, Vec<f64>) = pts.iter().map(|p| (p[1], p[3])).unzip();
        let (ax, bx) = fit(&cols, &lons).ok_or("the control points need different columns")?;
        let (ay, by) = fit(&rows, &lats).ok_or("the control points need different rows")?;
        if ax <= 0.0 || ay >= 0.0 {
            return Err("the points must keep north up (longitude rising with columns, latitude falling with rows)".into());
        }
        GeoTransform { origin_x: bx, origin_y: by, pixel_width: ax, pixel_height: ay }
    };
    let mut out = rebuild(&r, gt, Crs::LonLat, copied(&r)?)?;
    // The same pixels, so the same values rounded when the file was read.
    out.rounded = r.rounded.clone();
    out.rounded_some = r.rounded_some.clone();
    let e = out.extent();
    let v = replace(s, &layer, out)?;
    Ok(json!({"layer": v["layer"], "extent": [e.min_x, e.min_y, e.max_x, e.max_y]}))
}

/// A coordinate system given as an EPSG code or any text [`Crs::parse`] reads.
fn crs_param(s: &mut Session, p: &Value, key: &str) -> Result<Option<Crs>, String> {
    let bad = || format!("{key} is an EPSG code such as 32613 or \"EPSG:32613\"");
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(t)) => super::parse_crs(s, t).map(Some),
        Some(v) => {
            let code = v.as_u64().filter(|c| *c <= u64::from(u32::MAX)).ok_or_else(bad)?;
            Crs::from_epsg(code as u32).map(Some).ok_or_else(|| format!("unknown coordinate system EPSG:{code}"))
        }
    }
}

/// `raster.georeference` with a transform: fits the control points and warps the raster onto a
/// north-up grid in the output coordinate system (see `rustgis_analysis::warp`).
fn warp_to_fit(s: &mut Session, p: &Value, layer: &str, r: &Raster) -> CmdResult {
    let kind = ps(p, "transform")
        .and_then(Kind::parse)
        .ok_or("transform is similarity, affine, projective, polynomial2, polynomial3 or spline")?;
    let fit = georef::fit(kind, &gcps(p)?)?;
    // Points are longitude/latitude unless said otherwise, and the output stays in their system
    // unless asked to reproject.
    let points_crs = crs_param(s, p, "pointsCrs")?.unwrap_or(Crs::LonLat);
    let crs = crs_param(s, p, "crs")?.unwrap_or(points_crs);
    let resampling = match p.get("resampling") {
        None | Some(Value::Null) => warp::Resampling::Bilinear,
        Some(v) => v.as_str().and_then(warp::Resampling::parse).ok_or("resampling is nearest, bilinear or cubic")?,
    };
    let cell = match p.get("cellSize") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_f64().filter(|c| c.is_finite() && *c > 0.0).ok_or("cellSize is a positive number, in the output's units")?),
    };
    let path = match p.get("path") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str().filter(|t| is_tif(t)).ok_or("path is a *.tif or *.tiff file")?.to_string()),
    };
    let new_layer = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(|n| n.chars().take(200).collect::<String>());
    let out_name = new_layer.clone().unwrap_or_else(|| r.name.clone());
    let mut out = warp::warp(r, &fit, &warp::Options { crs, points_crs, resampling, cell, name: out_name })?;
    let used = warp::Method::Sample(resampling).used_on(r);
    report_resampled_classes(s, r, &out, used.id());
    // Resampled from values rounded when the file was read, some or all of them.
    out.keep_rounded_mark(r);
    if let Some((fw, fh)) = r.overview {
        let note =
            format!("{}: warped at reduced resolution, from its {} × {} overview of the {fw} × {fh} raster", r.name, r.width, r.height);
        s.notify(note, false);
    }
    let mut info = json!({
        "transform": kind.id(), "crs": crs.label(), "pointsCrs": points_crs.label(), "resampling": used.id(),
        "width": out.width, "height": out.height, "bands": out.band_count(), "cellSize": out.transform.pixel_width,
    });
    if let Some((fw, fh)) = r.overview {
        info["warpedFromOverview"] = json!({"width": r.width, "height": r.height, "of": [fw, fh]});
    }
    // A file written is the layer's source; without one the pixels are kept in the project.
    let rl = match &path {
        Some(path) => {
            let wrote = super::raster::write_and_report(s, &out, 0, true, path)?;
            info["path"] = json!(path);
            info["cog"] = wrote.get("cog").cloned().unwrap_or(Value::Null);
            let e = out.extent();
            info["extent"] = json!([e.min_x, e.min_y, e.max_x, e.max_y]);
            RasterLayer::from_path(out, path.clone())
        }
        None => {
            let e = out.extent();
            info["extent"] = json!([e.min_x, e.min_y, e.max_x, e.max_y]);
            layer_of(out)?
        }
    };
    let v = match new_layer {
        Some(n) => super::raster::add_raster(s, &n, rl)?,
        None => replace_layer(s, layer, rl)?,
    };
    if let (Some(o), Some(i)) = (info.as_object_mut(), v.as_object()) {
        for (k, val) in i {
            o.entry(k.clone()).or_insert_with(|| val.clone());
        }
    }
    Ok(info)
}

/// What an `extent` that isn't one is told.
const EXTENT: &str = "extent is a polygon layer's name, [west, south, east, north] in degrees, or \"view\"";

/// `raster.project`: the layer on a grid of another coordinate system, cell size, origin and
/// extent, or on another raster's grid, each cell sampled at its centre or taken from every
/// pixel whose centre falls in it (see `rustgis_analysis::warp::project`). A new layer.
fn project_raster(s: &mut Session, p: &Value) -> CmdResult {
    let r = raster(s, p)?;
    r.require_coordinates()?;
    let method = match p.get("resampling") {
        None | Some(Value::Null) => warp::Method::Sample(warp::Resampling::Nearest),
        Some(v) => v.as_str().and_then(warp::Method::parse).ok_or("resampling is nearest, bilinear, cubic, average or mode")?,
    };
    let grid = grid_param(s, p, r.crs, |crs| warp::footprint_in(&r, crs).map(|(bounds, pixel)| (bounds, Some(pixel))))?;
    let path = tif_path(p)?;
    let out_name = name(p, format!("{} projected", r.name));
    let out = warp::project(&r, &grid, method, out_name.clone())?;
    let used = method.used_on(&r);
    report_resampled_classes(s, &r, &out, used.id());
    let mut info = json!({
        "name": out_name, "crs": grid.crs.label(), "resampling": used.id(), "width": out.width, "height": out.height, "bands": out.band_count(),
        "cellSize": [grid.cell_x, grid.cell_y], "corner": [grid.min_x, grid.max_y],
    });
    if let Some((fw, fh)) = r.overview {
        let note =
            format!("{}: projected at reduced resolution, from its {} × {} overview of the {fw} × {fh} raster", r.name, r.width, r.height);
        s.notify(note, false);
        info["projectedFromOverview"] = json!({"width": r.width, "height": r.height, "of": [fw, fh]});
    }
    add_output(s, out, path, false, info)
}

/// Report categorical labels removed by value-mixing resampling, with bounded band numbers.
fn report_resampled_classes(s: &mut Session, source: &Raster, out: &Raster, method: &str) {
    let mut count = 0usize;
    let mut bands = Vec::new();
    for (i, band) in source.bands.iter().enumerate() {
        if band.classes.iter().any(|name| !name.is_empty()) && out.bands.get(i).is_some_and(|b| b.classes.is_empty()) {
            count += 1;
            if bands.len() < 16 {
                bands.push((i + 1).to_string());
            }
        }
    }
    if count > 0 {
        let more = if count > bands.len() { format!(", … ({} more)", count - bands.len()) } else { String::new() };
        s.notify(
            format!(
                "{}: class names of {count} band(s) ({}{more}) not kept under {method}: resampling can mix their values",
                source.name,
                bands.join(", ")
            ),
            false,
        );
    }
}

/// The largest whole number a raster's 32-bit float pixels hold exactly.
const EXACT_WHOLE: f64 = 16_777_216.0;

/// `raster.rasterize`: a polygon layer's selected (or all) features burned onto a grid given as
/// `raster.project`'s is, each cell whose centre falls in a polygon taking its field's value, a
/// text field's code or one `value` (see `rustgis_analysis::warp::rasterize`). A new layer, and
/// for a text field a table of its codes.
fn rasterize(s: &mut Session, p: &Value) -> CmdResult {
    let ln = rasterize_name(p)?;
    let l = s.map().layer_by_name(ln).filter(|l| l.raster.is_none()).ok_or_else(|| format!("no vector layer '{ln}'"))?;
    let ds = &l.dataset;
    if ds.geometry_type != GeometryType::Polygon {
        return Err(format!("{ln} is not a polygon layer"));
    }
    if ds.windowed {
        return Err(format!("{ln}: load all its features first (Layers › Load All Features)"));
    }
    // What a polygon gives: a field's value (the field and its type), or one value.
    let (field, value) = match (p.get("field").filter(|v| !v.is_null()), p.get("value").filter(|v| !v.is_null())) {
        (Some(_), Some(_)) => return Err("give a field or a value, not both".into()),
        (Some(f), None) => {
            let f = f.as_str().ok_or("field is a field's name")?;
            let i = ds.field_index(f).ok_or_else(|| format!("{ln}: no field '{f}'"))?;
            let ty = ds.fields.get(i).map(|f| f.ty).ok_or_else(|| format!("{ln}: no field '{f}'"))?;
            (Some((i, ty)), 1.0)
        }
        (None, Some(v)) => (None, v.as_f64().filter(|v| v.is_finite() && v.abs() <= f64::from(f32::MAX)).ok_or("value is a number")?),
        (None, None) => (None, 1.0),
    };
    let nodata = match p.get("nodata") {
        None | Some(Value::Null) => 0.0,
        Some(v) => v.as_f64().filter(|v| v.is_finite() && v.abs() <= f64::from(f32::MAX)).ok_or("nodata is a number")?,
    };
    let path = tif_path(p)?;
    let out_name = name(p, format!("{ln} raster"));
    // The features burned: the selection, else all, in the layer's order (a later one wins).
    let mut rows: Vec<usize> = if l.selection.is_empty() { (0..ds.features.len()).collect() } else { l.selection.clone() };
    rows.sort_unstable();
    rows.dedup();
    let (mut no_value, mut no_polygon) = (0, 0);
    let mut burns: Vec<(Vec<Vec<Vec<Coord>>>, rustgis_data::Value)> = Vec::new();
    for row in rows {
        let Some(feature) = ds.features.get(row) else { continue };
        let parts = match &feature.geometry {
            Some(Geometry::Polygon(polys)) => rasterops::transform_feature_polygons_to_lonlat(polys, None).ok(),
            _ => None,
        };
        let Some(parts) = parts else {
            no_polygon += 1;
            continue;
        };
        let v = match field {
            Some((i, _)) => ds.value(row, i).clone(),
            None => rustgis_data::Value::Float(value),
        };
        burns.push((parts, v));
    }
    // A text field's values become codes 1 to n, in sorted order.
    let text = field.is_some_and(|(_, ty)| ty == FieldType::Text);
    let classes: Vec<String> = if text {
        let set: std::collections::BTreeSet<String> = burns.iter().filter_map(|(_, v)| text_of(v)).collect();
        set.into_iter().collect()
    } else {
        Vec::new()
    };
    // Whole numbers: codes, one value, or a field that isn't a floating-point one.
    let whole_field = field.is_none_or(|(_, ty)| ty != FieldType::Double);
    let what = field.and_then(|(i, _)| ds.fields.get(i)).map_or("value", |f| f.name.as_str()).to_string();
    let mut shapes = Vec::with_capacity(burns.len());
    let mut rounded = 0;
    for (parts, v) in burns {
        let x = if text { text_of(&v).and_then(|t| classes.binary_search(&t).ok()).map(|i| i as f64 + 1.0) } else { v.as_f64() };
        let Some(x) = x else {
            no_value += 1;
            continue;
        };
        if x.abs() > f64::from(f32::MAX) {
            return Err(format!("{ln}: {what} {x} is past the range of a raster's 32-bit pixels"));
        }
        if whole_field && x.fract() == 0.0 && x.abs() > EXACT_WHOLE {
            // Only a numeric field's numbers can be given as text instead.
            let hint = if field.is_some() && !text { "; a text field gives codes instead" } else { "" };
            return Err(format!("{ln}: {what} {x} is more than a raster's pixels hold exactly ({EXACT_WHOLE}){hint}"));
        }
        // As a pixel holds them: a value that rounds to the no-data value would be lost.
        if x as f32 == nodata as f32 {
            let of = if text { format!(", the code of {:?},", text_of(&v).unwrap_or_default()) } else { String::new() };
            return Err(format!("{ln}: {what} {x}{of} is the no-data value: give another nodata"));
        }
        rounded += usize::from(f64::from(x as f32) != x);
        shapes.push(warp::Shape { parts, value: x as f32 });
    }
    if shapes.is_empty() {
        return Err(format!("{ln} has no polygons with a value to burn"));
    }
    // By default, the layer's coordinate system and the polygons' bounds.
    let lonlat = shapes.iter().flat_map(|s| s.parts.iter().flatten().flatten()).fold(Extent::EMPTY, |mut e, c| {
        e.expand(*c);
        e
    });
    let crs = ds.crs.unwrap_or(Crs::LonLat);
    let grid = grid_param(s, p, crs, |crs| Ok((warp::bounds_in([lonlat.min_x, lonlat.min_y, lonlat.max_x, lonlat.max_y], crs)?, None)))?;
    // The smallest pixel type the GeoTIFF writer keeps that holds every value and the no-data.
    let whole = |max: f64| shapes.iter().map(|s| f64::from(s.value)).chain([nodata]).all(|v| v.fract() == 0.0 && (0.0..=max).contains(&v));
    let data_type = if whole(255.0) {
        "UInt8"
    } else if whole(65_535.0) {
        "UInt16"
    } else {
        "Float32"
    };
    let burned = warp::rasterize(&grid, &shapes, nodata as f32)?;
    if burned.cells == 0 {
        return Err(format!("no cell centre of the grid falls inside {ln}'s polygons: give a smaller cellSize, or a grid over them"));
    }
    let (w, h) = (u32::try_from(grid.width).map_err(|e| e.to_string())?, u32::try_from(grid.height).map_err(|e| e.to_string())?);
    let out = Raster::new(&out_name, w, h, vec![burned.data], grid.transform(), grid.crs, Some(nodata), ColorHint::Gray, data_type)
        .map_err(|e| e.to_string())?;
    let mut info = json!({
        "name": out_name, "crs": grid.crs.label(), "width": out.width, "height": out.height, "dataType": data_type, "nodata": nodata,
        "cellSize": [grid.cell_x, grid.cell_y], "corner": [grid.min_x, grid.max_y], "features": burned.burned, "cells": burned.cells,
    });
    if rounded > 0 {
        s.notify(format!("{ln}: {rounded} value(s) of {what} rounded to the raster's 32-bit pixels"), false);
    }
    let skipped = [("noValue", no_value), ("noPolygon", no_polygon), ("unplaced", burned.unplaced.len()), ("noCell", burned.missed)];
    if skipped.iter().any(|(_, n)| *n > 0) {
        info["skipped"] =
            skipped.iter().filter(|(_, n)| *n > 0).map(|(k, n)| (k.to_string(), json!(n))).collect::<serde_json::Map<_, _>>().into();
        let mut why = Vec::new();
        if no_value > 0 {
            why.push(format!("{no_value} with no value"));
        }
        if no_polygon > 0 {
            why.push(format!("{no_polygon} with no valid polygon"));
        }
        if !burned.unplaced.is_empty() {
            why.push(format!("{} with a part that can't be placed on the grid (any other parts were burned)", burned.unplaced.len()));
        }
        if burned.missed > 0 {
            why.push(format!("{} that hold no cell centre (narrower than a cell, or off the grid)", burned.missed));
        }
        s.notify(format!("{ln}: features skipped: {}", why.join(", ")), false);
    }
    // Whole numbers few enough to list are classes, drawn with unique values.
    let distinct = out.bands.first().is_some_and(|b| b.stats.categories.is_some());
    let mut info = add_output(s, out, path, whole_field && distinct, info)?;
    if text {
        let layer = info.get("name").and_then(Value::as_str).unwrap_or(&out_name).to_string();
        let mut table = rustgis_data::Dataset::new(
            free_table_name(s, &format!("{layer} classes")),
            GeometryType::None,
            vec![rustgis_data::Field::new("code", FieldType::Integer), rustgis_data::Field::new("value", FieldType::Text)],
        );
        for (i, c) in classes.iter().enumerate() {
            table.push_next(None, vec![rustgis_data::Value::Int(i as i64 + 1), rustgis_data::Value::Text(c.clone())]);
        }
        info["classTable"] = json!(table.name);
        s.project.add_table(table);
        info["classCount"] = json!(classes.len());
        // All the codes are in the table; here, as many as a unique-value legend shows.
        info["classes"] = classes.iter().take(256).enumerate().map(|(i, c)| json!({"code": i + 1, "value": c})).collect();
    }
    Ok(info)
}

/// `base`, or `base (2)`, `base (3)`… : the first no table of the project has (whatever its case).
fn free_table_name(s: &Session, base: &str) -> String {
    let taken = |n: &str| s.project.tables.iter().any(|t| t.name.eq_ignore_ascii_case(n));
    let mut name = base.to_string();
    let mut i = 1;
    while taken(&name) {
        i += 1;
        name = format!("{base} ({i})");
    }
    name
}

/// A text field's value, `None` for null or blank.
fn text_of(v: &rustgis_data::Value) -> Option<String> {
    let t = match v {
        rustgis_data::Value::Null => return None,
        rustgis_data::Value::Text(t) => t.clone(),
        rustgis_data::Value::Int(i) => i.to_string(),
        rustgis_data::Value::Float(f) => f.to_string(),
        rustgis_data::Value::Bool(b) => b.to_string(),
    };
    (!t.trim().is_empty()).then_some(t)
}

/// A `path` to write a GeoTIFF to, if one is given.
fn tif_path(p: &Value) -> Result<Option<String>, String> {
    match p.get("path") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => Ok(Some(v.as_str().filter(|t| is_tif(t)).ok_or("path is a *.tif or *.tiff file")?.to_string())),
    }
}

/// Adds `out` as a new layer, its classes drawn with unique values when `classes`; with a `path`,
/// written there first as a cloud-optimised GeoTIFF, which becomes the layer's source. `info`,
/// with the extent, the file and the name the layer got, is the result.
fn add_output(s: &mut Session, out: Raster, path: Option<String>, classes: bool, mut info: Value) -> CmdResult {
    let owner = s.continuation()?;
    owner.check()?;
    let _progress = rustgis_analysis::control::install_progress(None);
    let out_name = out.name.clone();
    let e = out.extent();
    info["extent"] = json!([e.min_x, e.min_y, e.max_x, e.max_y]);
    // A file written is the layer's source; without one the pixels are kept in the project.
    let mut rl = match &path {
        Some(path) => {
            let wrote = super::raster::write_and_report(s, &out, 0, true, path)?;
            info["path"] = json!(path);
            info["cog"] = wrote.get("cog").cloned().unwrap_or(Value::Null);
            RasterLayer::from_path(out, path.clone())
        }
        None => layer_of(out)?,
    };
    if classes && let Some(d) = rl.data.clone() {
        rl.style.renderer = rustgis_map::raster::unique_values(&d, 0, rustgis_map::Ramp::Harbour);
    }
    s.check_continuation(&owner)?;
    let v = super::raster::add_raster(s, &out_name, rl)?;
    if let (Some(o), Some(i)) = (info.as_object_mut(), v.as_object()) {
        for (k, val) in i {
            o.entry(k.clone()).or_insert_with(|| val.clone());
        }
    }
    // The name the layer got (another may hold the one asked for: "… projected (2)").
    let id = v.get("layer").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok());
    if let Some(name) = id.and_then(|id| s.map().layer(id)).map(|l| l.name.clone()) {
        info["name"] = json!(name);
    }
    Ok(info)
}

/// The grid of `raster.project` and `raster.rasterize`: another raster's (`grid`), taken exactly,
/// or one of `crs` (by default `crs`), `cellSize`, `origin` and `extent`. `natural` gives, in the
/// grid's system, the bounds taken without an extent and the cell size taken without a cellSize
/// (`None`: a cellSize is required).
fn grid_param(
    s: &mut Session,
    p: &Value,
    crs: Crs,
    natural: impl FnOnce(Crs) -> Result<([f64; 4], Option<f64>), String>,
) -> Result<warp::Grid, String> {
    // A grid named as anything but a raster layer's name is refused rather than ignored.
    let template = project_grid(p)?;
    Ok(match template {
        Some(template) => {
            if ["crs", "cellSize", "origin", "extent"].iter().any(|k| p.get(*k).is_some_and(|v| !v.is_null())) {
                return Err(
                    "grid takes that raster's coordinate system, cells and extent: leave out crs, cellSize, origin and extent".into()
                );
            }
            let t = raster_named(s, template)?;
            t.require_coordinates()?;
            if let Some((fw, fh)) = t.overview {
                s.notify(format!("{template}: the grid is its {} × {} overview's, of the {fw} × {fh} raster", t.width, t.height), false);
            }
            warp::Grid::of(&t)
        }
        None => {
            let crs = crs_param(s, p, "crs")?.unwrap_or(crs);
            let (footprint, pixel) = natural(crs)?;
            let positive = |v: &Value| v.as_f64().filter(|c| c.is_finite() && *c > 0.0);
            let bad_cell = "cellSize is a positive number, or [x, y], in the output's units";
            let cell = match p.get("cellSize") {
                None | Some(Value::Null) => {
                    pixel.map(|c| (c, c)).ok_or("cellSize is required: a number, or [x, y], in the output's units, or a grid")?
                }
                Some(Value::Array(a)) => match a.as_slice() {
                    [x, y] => (positive(x).ok_or(bad_cell)?, positive(y).ok_or(bad_cell)?),
                    _ => return Err(bad_cell.into()),
                },
                Some(v) => positive(v).map(|c| (c, c)).ok_or(bad_cell)?,
            };
            let origin = match p.get("origin") {
                None | Some(Value::Null) => (0.0, 0.0),
                Some(v) => {
                    // Every entry a number: one that isn't refuses the origin rather than drop out.
                    let n: Option<Vec<f64>> = v.as_array().map(|a| a.iter().map(Value::as_f64).collect()).unwrap_or_default();
                    match n.as_deref().unwrap_or_default() {
                        [x, y] if x.is_finite() && y.is_finite() => (*x, *y),
                        _ => return Err("origin is [x, y] in the output's units".into()),
                    }
                }
            };
            let bounds = match p.get("extent") {
                None | Some(Value::Null) => footprint,
                Some(Value::String(_)) if project_extent_name(p).is_some() => {
                    let l = project_extent_name(p).ok_or("extent layer is required")?;
                    let layer = s.map().layer_by_name(l).filter(|l| l.raster.is_none()).ok_or_else(|| format!("no vector layer '{l}'"))?;
                    let rows: Vec<usize> =
                        if layer.selection.is_empty() { (0..layer.dataset.features.len()).collect() } else { layer.selection.clone() };
                    let e = rows
                        .iter()
                        .filter_map(|row| layer.dataset.features.get(*row).and_then(|f| f.geometry.as_ref()))
                        .fold(Extent::EMPTY, |e, g| e.union(&g.extent()));
                    if e.is_empty() {
                        return Err(format!("{l} has no features to take an extent from"));
                    }
                    warp::bounds_in([e.min_x, e.min_y, e.max_x, e.max_y], crs)?
                }
                Some(v @ (Value::Array(_) | Value::String(_))) => {
                    let area = super::catalogs::area(s, &json!({"bbox": v})).map_err(|e| e.replacen("bbox", "extent", 1))?;
                    if area[0] > area[2] {
                        return Err("an extent across the antimeridian: give it one side at a time".into());
                    }
                    warp::bounds_in(area, crs)?
                }
                // A number, a flag or an object isn't an extent: refused, not read as the view.
                Some(_) => return Err(EXTENT.into()),
            };
            warp::Grid::snapped(crs, bounds, cell, origin)?
        }
    })
}

fn is_tif(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff"))
}

/// Control points from `[column, row, x, y, on?]` entries.
fn gcps(p: &Value) -> Result<Vec<Gcp>, String> {
    const SHAPE: &str = "each control point is [column, row, x, y] or [column, row, x, y, on]";
    let a = p.get("points").and_then(Value::as_array).ok_or("points is required: [[column, row, x, y], …]")?;
    if a.len() > georef::MAX_POINTS {
        return Err(format!("a fit takes at most {} control points", georef::MAX_POINTS));
    }
    a.iter()
        .map(|v| {
            let e = v.as_array().filter(|e| e.len() == 4 || e.len() == 5).ok_or(SHAPE)?;
            let num = |i: usize| e.get(i).and_then(Value::as_f64).filter(|x| x.is_finite()).ok_or(SHAPE);
            let on = match e.get(4) {
                None => true,
                Some(Value::Bool(b)) => *b,
                Some(v) => v.as_f64().ok_or(SHAPE)? != 0.0,
            };
            Ok(Gcp { col: num(0)?, row: num(1)?, x: num(2)?, y: num(3)?, on })
        })
        .collect()
}

fn pair(e: (f64, f64)) -> Value {
    json!({"pixels": e.0, "map": e.1})
}

/// A report's errors (no per-point residuals).
fn errors(r: &georef::Report) -> Value {
    json!({
        "transform": r.fit.kind.id(),
        "used": r.used,
        "minimum": r.minimum,
        "rmse": pair(r.rmse),
        "meanError": pair(r.mean_error),
        "leaveOneOut": r.leave_one_out_rmse.map(pair),
        "leaveOneOutNote": r.leave_one_out_note,
    })
}

/// Fits a transform to control points and reports how well it holds: each point's residual in
/// pixels and map units, the RMSE and QGIS's mean error, and each point's error when left out.
fn gcp_fit(_: &mut Session, p: &Value) -> CmdResult {
    let pts = gcps(p)?;
    if b(p, "compare").unwrap_or(false) {
        let (reports, best) = georef::compare(&pts);
        if reports.iter().all(|(_, r)| r.is_err()) {
            return Err("the points can't fix any transform: give at least two, not on top of each other".into());
        }
        let transforms: Vec<Value> = reports
            .iter()
            .map(|(k, r)| match r {
                Ok(r) => errors(r),
                Err(e) => json!({"transform": k.id(), "error": e}),
            })
            .collect();
        return Ok(json!({"recommended": best.map(Kind::id), "transforms": transforms}));
    }
    let kind = match ps(p, "transform") {
        Some(t) => Kind::parse(t).ok_or_else(|| format!("unknown transform '{t}'"))?,
        None if pts.iter().filter(|g| g.on).count() < 3 => Kind::Similarity,
        None => Kind::Affine,
    };
    let r = georef::assess(kind, &pts, b(p, "leaveOneOut").unwrap_or(true))?;
    let mut out = errors(&r);
    out["residuals"] = r
        .residuals
        .iter()
        .enumerate()
        .map(|(i, res)| {
            let left_out = r.leave_one_out.as_ref().and_then(|l| l.get(i).copied().flatten());
            res.map_or(
                Value::Null,
                |x| json!({"pixels": [x.pixels.0, x.pixels.1], "map": [x.map.0, x.map.1], "leaveOneOut": left_out.map(pair)}),
            )
        })
        .collect();
    out["affine"] = json!(r.fit.affine());
    Ok(out)
}

fn change(s: &mut Session, p: &Value) -> CmdResult {
    super::workflows::made_or_ran(s, p, "raster.change")
}

/// What a change detection reads of the session, and the work that compares the rasters.
pub(crate) fn change_work(s: &Session, p: &Value) -> Result<super::workflows::Work, String> {
    let [before, after] = change_names(p)?;
    let a = raster_named(s, before)?;
    let bfr = raster_named(s, after)?;
    // The method is checked before any pixels are resampled.
    let method = ps(p, "method").unwrap_or("difference").to_string();
    if !matches!(method.as_str(), "difference" | "ratio" | "percent") {
        return Err(format!("unknown method '{method}'"));
    }
    let band = uint(p, "band").unwrap_or(1).saturating_sub(1) as usize;
    a.bands.get(band).ok_or("no such band")?;
    bfr.bands.get(band).ok_or_else(|| format!("{} has no band {}", bfr.name, band + 1))?;
    let out_name = name(p, format!("{}_change", a.name));
    Ok(Box::new(move || {
        let av = &a.bands.get(band).ok_or("no such band")?.data;
        let bv = rasterops::resample_to(&bfr, band, &a)?;
        let n = a.width as usize * a.height as usize;
        let out = rasterops::per_index(
            n,
            a.width as usize,
            1,
            || (),
            |_, i| {
                let (Some(x), Some(y)) = (av.get(i).copied(), bv.get(i).copied()) else { return NODATA };
                if !a.is_valid(x) || y == NODATA {
                    return NODATA;
                }
                let v = match method.as_str() {
                    "ratio" if x != 0.0 => y / x,
                    "percent" if x != 0.0 => (y - x) / x * 100.0,
                    "difference" => y - x,
                    _ => f32::NAN,
                };
                if v.is_finite() { v } else { NODATA }
            },
        )?;
        super::workflows::made(rasterops::derive(&a, &out_name, vec![out])?, None)
    }))
}

fn spectral(s: &mut Session, p: &Value) -> CmdResult {
    let r = raster(s, p)?;
    let all: Vec<usize> = (0..r.band_count()).collect();
    let names: Vec<String> = r.bands.iter().map(|b| b.name.clone()).collect();
    if let Some(z) = spectral_zone(p) {
        let l = s
            .map()
            .layer_by_name(z)
            .filter(|l| l.geometry_type() == GeometryType::Polygon)
            .ok_or_else(|| format!("no polygon layer '{z}'"))?;
        let row = l.selection.first().copied().unwrap_or(0);
        let Some(Geometry::Polygon(polys)) = l.dataset.features.get(row).and_then(|f| f.geometry.as_ref()) else {
            return Err("the zone has no polygon".into());
        };
        let rings = polys.first().ok_or("the zone has no polygon")?;
        let (means, n) = rasterops::zonal_means(&r, &all, rings).ok_or("no pixels fall in the zone")?;
        return Ok(json!({"bands": names, "values": means, "pixels": n}));
    }
    let c = Coord::new(f(p, "lon").ok_or("give lon and lat, or a zone")?, f(p, "lat").ok_or("lat is required")?);
    let (col, row) = r.pixel_at(c).ok_or("the point is outside the raster")?;
    let vals: Vec<Option<f32>> = all.iter().map(|b| r.value(*b, col, row)).collect();
    Ok(json!({"bands": names, "values": vals, "pixel": [col, row]}))
}

fn supervised(s: &mut Session, p: &Value) -> CmdResult {
    super::workflows::made_or_ran(s, p, "raster.classifySupervised")
}

/// What a classification from training polygons reads of the session (the raster, and each
/// polygon with its class), and the work that classifies it.
pub(crate) fn supervised_work(s: &Session, p: &Value) -> Result<super::workflows::Work, String> {
    let r = raster(s, p)?;
    // Training polygons on the map mean nothing on a scan laid there to look at.
    r.require_coordinates()?;
    let bands = bands_param(p, &r)?;
    // With none, every pixel would be equally near every class.
    if bands.is_empty() {
        return Err("give at least one band".into());
    }
    let tl = training_name(p)?;
    let l = s
        .map()
        .layer_by_name(tl)
        .filter(|l| l.geometry_type() == GeometryType::Polygon)
        .ok_or_else(|| format!("no polygon layer '{tl}'"))?;
    let fi = l.dataset.field_index(ps(p, "classField").ok_or("classField is required")?).ok_or("no such class field")?;
    let mut polygons = Vec::new();
    for (row, ft) in l.dataset.features.iter().enumerate() {
        let Some(Geometry::Polygon(polys)) = &ft.geometry else { continue };
        let key = l.dataset.value(row, fi).to_string();
        polygons.extend(polys.iter().map(|rings| (key.clone(), rings.clone())));
    }
    let out = name(p, format!("{}_classified", r.name));
    Ok(Box::new(move || {
        // Mean of each class's training pixels.
        let classes = rasterops::class_means(&r, &bands, &polygons)?;
        if classes.len() < 2 {
            return Err("the training polygons need at least two classes with pixels under them".into());
        }
        let means: Vec<(f32, Vec<f64>)> = classes.iter().enumerate().map(|(i, c)| ((i + 1) as f32, c.1.clone())).collect();
        let pixels = rasterops::classify_min_distance(&r, &bands, &means)?;
        let layer = class_layer(rasterops::derive(&r, &out, vec![pixels])?)?;
        let legend: Vec<Value> = classes.iter().enumerate().map(|(i, c)| json!({"value": i + 1, "class": c.0, "pixels": c.2})).collect();
        Ok(super::workflows::made_layer(out, layer, Some(json!({"classes": legend})), None))
    }))
}

fn index(s: &mut Session, p: &Value) -> CmdResult {
    super::workflows::made_or_ran(s, p, "raster.index")
}

/// What a spectral index reads of the session, and the work that makes it.
pub(crate) fn index_work(s: &Session, p: &Value) -> Result<super::workflows::Work, String> {
    let r = raster(s, p)?;
    let bn = |k: &str, d: u64| uint(p, k).unwrap_or(d);
    let (blue, green, red, nir, swir) = (bn("blue", 1), bn("green", 2), bn("red", 3), bn("nir", 4), bn("swir", 5));
    let idx = ps(p, "index").ok_or("index is required")?;
    let expr = match idx {
        "ndvi" => format!("(b{nir} - b{red}) / (b{nir} + b{red})"),
        "ndwi" => format!("(b{green} - b{nir}) / (b{green} + b{nir})"),
        "savi" => format!("(b{nir} - b{red}) * 1.5 / (b{nir} + b{red} + 0.5)"),
        "nbr" => format!("(b{nir} - b{swir}) / (b{nir} + b{swir})"),
        "ndbi" => format!("(b{swir} - b{nir}) / (b{swir} + b{nir})"),
        "evi" => format!("2.5 * (b{nir} - b{red}) / (b{nir} + 6 * b{red} - 7.5 * b{blue} + 1)"),
        o => return Err(format!("unknown index '{o}'")),
    };
    let out = name(p, format!("{}_{idx}", r.name));
    Ok(Box::new(move || super::workflows::made(rasterops::derive(&r, &out, vec![rasterops::band_math(&r, &expr)?])?, None)))
}

/// Processes a raster via clipping or resampling.
///
/// ### Polygon Clipping Semantics (`operation: "clip"`, `"polygon": "<layer>"`)
/// - **Selection Propagation (`selectedOnly`)**:
///   - When `Some(true)`: Clips strictly using features currently selected in the polygon layer.
///     If the selection is empty, returns an actionable error (`"<layer>: no features are selected"`).
///   - When `Some(false)`: Clips using all features in the polygon layer, ignoring any selection.
///   - When omitted (`None` or `null`): For backward compatibility, clips to the selection if non-empty,
///     otherwise clips using all features.
///   - Non-boolean values are rejected with an explicit validation error.
/// - **Per-Zone and Overlap Semantics**:
///   - Every polygon geometry (including multipart geometries) is already WGS 84 lon/lat.
///   - Polygon exterior rings and interior holes are strictly validated; invalid or failed holes reject
///     the containing polygon rather than distorting the boundary.
///   - Polygons are combined into a composite scanline mask over the raster. Holes cut out cells using the
///     even-odd rule within each polygon. Where multiple polygons overlap, their coverage is combined
///     (union: any cell falling inside at least one polygon is retained).
///   - The raster is cropped to the tight bounding box of valid selected polygons, and cells outside the
///     polygons receive NoData.
/// - **Partial Failure Reporting**:
///   - Features with invalid geometry are skipped and reported in the result messages.
///   - Polygon parts that cannot be projected onto the raster grid are reported separately.
///   - If all features fail, the operation fails and reports the specific failure cause of the first feature.
fn process(s: &mut Session, p: &Value) -> CmdResult {
    super::workflows::made_or_ran(s, p, "raster.process")
}

/// What a clip or resample reads of the session (the raster, and the polygons or extent it is
/// clipped to), and the work that makes it.
pub(crate) fn process_work(s: &Session, p: &Value) -> Result<super::workflows::Work, String> {
    let r = raster(s, p)?;
    match ps(p, "operation").ok_or("operation is required")? {
        "clip" if p.get("polygon").is_some_and(|v| !v.is_null()) => {
            // Before the mask, which places the polygons on the raster's coordinates.
            r.require_coordinates()?;
            let pl = clip_polygon(p).ok_or("polygon is a polygon layer's name")?;
            let l = s
                .map()
                .layer_by_name(pl)
                .filter(|l| l.geometry_type() == GeometryType::Polygon)
                .ok_or_else(|| format!("no polygon layer '{pl}'"))?;
            let rows: Vec<usize> = match p.get("selectedOnly") {
                Some(Value::Bool(true)) => {
                    if l.selection.is_empty() {
                        return Err(format!("{pl}: no features are selected"));
                    }
                    l.selection.clone()
                }
                Some(Value::Bool(false)) => (0..l.dataset.features.len()).collect(),
                None | Some(Value::Null) => {
                    // Backward-compatible default: if features are selected, clip to selection; otherwise clip all.
                    if l.selection.is_empty() { (0..l.dataset.features.len()).collect() } else { l.selection.clone() }
                }
                Some(_) => {
                    return Err("parameter 'selectedOnly' must be a boolean (true or false)".into());
                }
            };
            if rows.is_empty() {
                return Err(format!("{pl}: has no features to clip by"));
            }
            let mut polygons: Vec<Vec<Vec<Coord>>> = Vec::new();
            let mut failed_features = Vec::new();
            for &row in &rows {
                let Some(f) = l.dataset.features.get(row) else { continue };
                match &f.geometry {
                    Some(Geometry::Polygon(polys)) => match rasterops::transform_feature_polygons_to_lonlat(polys, None) {
                        Ok(transformed) => {
                            polygons.extend(transformed);
                        }
                        Err(e) => {
                            failed_features.push((f.id, e));
                        }
                    },
                    _ => {
                        failed_features
                            .push((f.id, rasterops::PolygonTransformError::InvalidExterior("missing or non-polygon geometry".into())));
                    }
                }
            }
            if polygons.is_empty() {
                if let Some((first_id, first_err)) = failed_features.first() {
                    return Err(format!("{pl}: has no valid polygon geometries (feature {first_id}: {first_err})"));
                }
                return Err(format!("{pl}: has no valid polygon geometries"));
            }
            if !failed_features.is_empty() {
                log::warn!("{pl}: {} feature(s) failed polygon validation during clipping", failed_features.len());
            }
            let (out, pl, skipped) = (name(p, format!("{}_clip", r.name)), pl.to_string(), failed_features.len());
            Ok(Box::new(move || {
                let mut failed_parts = Vec::new();
                let (cols, rows, mask) = rasterops::polygon_mask_with_failures(&r, &polygons, &mut failed_parts)?.ok_or_else(|| {
                    format!("no cell of {} falls inside {pl} ({} polygon part(s) failed raster projection)", r.name, failed_parts.len())
                })?;
                let clipped = clip_window(&r, cols, rows, Some(&mask), out)?;
                let mut messages = Vec::new();
                if skipped > 0 {
                    messages.push(format!("{pl}: skipped {skipped} feature(s) with invalid polygon geometry"));
                }
                if !failed_parts.is_empty() {
                    messages.push(format!(
                        "{pl}: skipped {} polygon part(s) that could not be projected onto the raster grid; valid parts were retained",
                        failed_parts.len()
                    ));
                }
                super::workflows::made(clipped, (!messages.is_empty()).then(|| json!({"messages": messages})))
            }))
        }
        "clip" => {
            // Before the extent is placed on the raster's coordinates.
            r.require_coordinates()?;
            let e = match p.get("extent").and_then(Value::as_array) {
                Some(a) => {
                    let n: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
                    let [x0, y0, x1, y1] = n.as_slice() else { return Err("extent is [minLon, minLat, maxLon, maxLat]".into()) };
                    Extent::new(*x0, *y0, *x1, *y1)
                }
                None => {
                    let m = s.map();
                    let (w, h) = s.view_size;
                    let ce = m.camera.extent(w, h);
                    let mut e = Extent::EMPTY;
                    for c in [Coord::new(ce.min_x, ce.min_y), Coord::new(ce.max_x, ce.max_y)] {
                        if let Some(ll) = m.to_lonlat(c) {
                            e.expand(ll);
                        }
                    }
                    e
                }
            };
            // The pixel window covering the extent's corners.
            let corners =
                [Coord::new(e.min_x, e.min_y), Coord::new(e.max_x, e.max_y), Coord::new(e.min_x, e.max_y), Coord::new(e.max_x, e.min_y)];
            let px: Vec<(f64, f64)> =
                corners.iter().filter_map(|c| r.transformer().from_lonlat(*c)).map(|w| r.transform.to_pixel(w)).collect();
            if px.is_empty() {
                return Err("the extent can't be placed on the raster".into());
            }
            let c0 = px.iter().map(|p| p.0).fold(f64::INFINITY, f64::min).floor().max(0.0) as u32;
            let c1 = px.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max).ceil().min(r.width as f64) as u32;
            let r0 = px.iter().map(|p| p.1).fold(f64::INFINITY, f64::min).floor().max(0.0) as u32;
            let r1 = px.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max).ceil().min(r.height as f64) as u32;
            if c1 <= c0 || r1 <= r0 {
                return Err("the extent doesn't overlap the raster".into());
            }
            let out = name(p, format!("{}_clip", r.name));
            Ok(Box::new(move || super::workflows::made(clip_window(&r, c0..c1, r0..r1, None, out)?, None)))
        }
        "resample" => {
            // Never coarser than the raster itself; trailing columns and rows short of a full
            // block are left out.
            r.require_coordinates()?;
            let k = (uint(p, "factor").unwrap_or(2).clamp(2, 64) as u32).min(r.width.min(r.height)).max(1);
            let (w, h) = ((r.width / k).max(1), (r.height / k).max(1));
            let out = name(p, format!("{}_resampled", r.name));
            Ok(Box::new(move || {
                // Each output pixel is the mean of the valid pixels it covers.
                let bands = r
                    .bands
                    .iter()
                    .map(|b| {
                        // A cell reads its k × k block; the cells are counted and cancellable.
                        let cost = (k * k) as usize;
                        rasterops::per_index(
                            w as usize * h as usize,
                            w as usize,
                            cost,
                            || (),
                            |_, i| {
                                let (row, col) = ((i / w as usize) as u32, (i % w as usize) as u32);
                                let (mut sum, mut n) = (0.0f64, 0u32);
                                for dy in 0..k {
                                    for dx in 0..k {
                                        let (x, y) = (col * k + dx, row * k + dy);
                                        if x >= r.width || y >= r.height {
                                            continue;
                                        }
                                        if let Some(v) = b.data.get((y * r.width + x) as usize).copied().filter(|v| r.is_valid(*v)) {
                                            sum += v as f64;
                                            n += 1;
                                        }
                                    }
                                }
                                if n > 0 { (sum / n as f64) as f32 } else { NODATA }
                            },
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let gt = GeoTransform {
                    pixel_width: r.transform.pixel_width * k as f64,
                    pixel_height: r.transform.pixel_height * k as f64,
                    ..r.transform
                };
                let nodata = r.nodata.or(Some(NODATA as f64));
                let color = if matches!(r.color, ColorHint::Palette(_)) { ColorHint::Gray } else { r.color.clone() };
                let mut out = Raster::new(out, w, h, bands, gt, r.crs, nodata, color, r.data_type.clone()).map_err(|e| e.to_string())?;
                // The blocks average values rounded when the file was read, some or all of them.
                out.keep_rounded_mark(&r);
                super::workflows::made(out, None)
            }))
        }
        o => Err(format!("unknown operation '{o}'")),
    }
}

/// Most inputs a mosaic takes.
const MAX_MOSAIC_INPUTS: usize = 10_000;

/// A mosaic input's name, trimmed.
fn mosaic_name(v: &Value) -> Result<&str, &'static str> {
    v.as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or("each input is a raster layer's name or a file path")
}

/// The raster layer a mosaic input names, with its cells in memory. An error means the input isn't
/// one.
fn mosaic_layer(input: &impl super::InputSource, v: &Value) -> Result<Arc<Raster>, String> {
    raster_named(input, mosaic_name(v)?)
}

/// Whether [`raster_mosaic`] reads a mosaic input from a file: it isn't a [`mosaic_layer`] and it
/// is a raster file name. The command and [`mosaic_stays_in_project`] both ask this, so the
/// permission check can't fall out of step with what the command reads.
fn mosaic_reads_file(s: &Session, v: &Value) -> bool {
    mosaic_layer(s, v).is_err() && mosaic_name(v).is_ok_and(rustgis_data::raster::is_raster_name)
}

/// Whether a mosaic stays in the project: no input is read from a file ([`mosaic_reads_file`]),
/// and nothing else names a place (no `path`, nor a name that looks like one). Any other input
/// that isn't a layer (a typo, a vector layer, a tile still downloading) fails in the command with
/// "no raster layer", touching no file, as does a call over the input limit, which is refused
/// before any lookup. So do absent, empty or non-list `inputs`: the command rejects them before
/// reading anything, with its own error. A layer named like a path (`~dem`, `a/../b`) counts as a
/// place: that errs towards asking for `files`.
pub(crate) fn mosaic_stays_in_project(s: &Session, p: &Value) -> bool {
    let inputs =
        p.get("inputs").and_then(Value::as_array).is_none_or(|a| a.len() > MAX_MOSAIC_INPUTS || !a.iter().any(|v| mosaic_reads_file(s, v)));
    inputs && !super::items::names_place(p, super::plugins::PLACE_KEYS, 0)
}

/// Merges rasters on one grid (tiles of an elevation model, say) into one, keeping their
/// coordinate system and cell size; where they overlap, the first input with data wins.
fn raster_mosaic(s: &mut Session, p: &Value) -> CmdResult {
    let inputs = p.get("inputs").and_then(Value::as_array).ok_or("inputs is a list of raster layers or file paths")?;
    if inputs.len() < 2 {
        return Err("a mosaic needs two or more rasters".into());
    }
    if inputs.len() > MAX_MOSAIC_INPUTS {
        return Err(format!("a mosaic takes at most {MAX_MOSAIC_INPUTS} inputs"));
    }
    let max = rustgis_data::raster::max_samples() as i64;
    let mut rs: Vec<Arc<Raster>> = Vec::with_capacity(inputs.len());
    // Each input's place on the first one's grid, and the bounds of them all so far.
    let mut places: Vec<(i64, i64)> = Vec::with_capacity(inputs.len());
    let (mut c0, mut r0, mut c1, mut r1) = (0i64, 0i64, 0i64, 0i64);
    // Each input is checked as it is read, so a mismatched or oversized set fails before the
    // rest are loaded. What reading a file left out is said once they are all read.
    let mut lost: Vec<(String, String)> = Vec::new();
    for v in inputs {
        let n = mosaic_name(v)?;
        let r = if mosaic_reads_file(s, v) {
            let r = rustgis_data::raster::read_path(std::path::Path::new(n)).map_err(|e| format!("{n}: {e}"))?;
            lost.extend(r.lost.iter().map(|l| (n.to_string(), l.clone())));
            Arc::new(r)
        } else {
            mosaic_layer(s, v)?
        };
        // An overview would quietly coarsen the whole mosaic.
        if let Some((fw, fh)) = r.overview {
            return Err(format!(
                "{} is open as a {} × {} overview of the {fw} × {fh} raster; give its file path, or mosaic smaller tiles",
                r.name, r.width, r.height
            ));
        }
        let first = rs.first().unwrap_or(&r);
        mosaic_matches(first, &r)?;
        // Snapped to whole cells only when it is already within a hundredth of one.
        let (c, rr) = first.transform.to_pixel(r.transform.to_world(0.0, 0.0));
        let limit = f64::from(u32::MAX);
        if !(c.is_finite() && rr.is_finite() && c.abs() < limit && rr.abs() < limit) {
            return Err(format!("{} is too far from {} to mosaic", r.name, first.name));
        }
        if (c - c.round()).abs() > 0.01 || (rr - rr.round()).abs() > 0.01 {
            return Err(format!("{}'s cells don't line up with {}'s: resample them onto one grid first", r.name, first.name));
        }
        let (pc, pr) = (c.round() as i64, rr.round() as i64);
        (c0, r0) = if rs.is_empty() { (pc, pr) } else { (c0.min(pc), r0.min(pr)) };
        (c1, r1) = (c1.max(pc + i64::from(r.width)), r1.max(pr + i64::from(r.height)));
        let (w, h) = (c1 - c0, r1 - r0);
        let samples = w.saturating_mul(h).saturating_mul(r.band_count().max(1) as i64);
        if samples > max || w > i64::from(u32::MAX) || h > i64::from(u32::MAX) {
            return Err(format!(
                "the mosaic would be {w} × {h} cells × {} bands, more than the {max} values that fit in memory: clip the tiles or mosaic fewer at a time",
                r.band_count()
            ));
        }
        places.push((pc, pr));
        rs.push(r);
    }
    super::raster::say_losses(s, lost);
    let first = rs.first().ok_or("a mosaic needs two or more rasters")?;
    let (w, h) = (c1 - c0, r1 - r0);
    if w <= 0 || h <= 0 {
        return Err("the mosaic would be empty".into());
    }
    let (w, h) = (w as u32, h as u32);
    let nodata = first.nodata.or(Some(NODATA as f64));
    let fill = nodata.map(|v| v as f32).unwrap_or(NODATA);
    let mut bands = (0..first.band_count())
        .map(|_| raster::filled(w as usize, h as usize, fill))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for (r, (pc, pr)) in rs.iter().zip(&places) {
        let (dc, dr) = ((pc - c0) as usize, (pr - r0) as usize);
        for (out, b) in bands.iter_mut().zip(&r.bands) {
            for row in 0..r.height as usize {
                let (Some(src), Some(dst)) = (
                    b.data.get(row * r.width as usize..(row + 1) * r.width as usize),
                    out.get_mut((dr + row) * w as usize + dc..(dr + row) * w as usize + dc + r.width as usize),
                ) else {
                    continue;
                };
                for (d, v) in dst.iter_mut().zip(src) {
                    // Earlier inputs keep their cells; later ones only fill the gaps.
                    if (*d == fill || d.is_nan()) && r.is_valid(*v) {
                        *d = *v;
                    }
                }
            }
        }
    }
    let o = first.transform.to_world(c0 as f64, r0 as f64);
    let gt = GeoTransform { origin_x: o.x, origin_y: o.y, ..first.transform };
    let mut out = Raster::new(
        name(p, format!("{}_mosaic", first.name)),
        w,
        h,
        bands,
        gt,
        first.crs,
        nodata,
        first.color.clone(),
        first.data_type.clone(),
    )
    .map_err(|e| e.to_string())?;
    // Band and class names are the first input's, as its cells are where they overlap; an input
    // naming its bands or classes otherwise is said.
    out.bands.iter_mut().zip(&first.bands).for_each(|(o, i)| {
        o.name = i.name.clone();
        o.classes = i.classes.clone();
    });
    out.copyright = first.copyright.clone();
    // The first input's metadata accompanies its pixels; report other credits rather than
    // silently dropping them or attributing every input to the first one's owner.
    super::raster::say_losses(
        s,
        rs.iter()
            .skip(1)
            .filter(|r| r.copyright.is_some() && r.copyright != first.copyright)
            .map(|r| (r.name.clone(), "its copyright differs from the first input's and is not kept in the mosaic".to_string()))
            .collect(),
    );
    // Cells rounded when an input was read may be among those the mosaic keeps.
    out.keep_rounded_marks(rs.iter().map(|r| &**r));
    let other: Vec<&str> = rs
        .iter()
        .skip(1)
        .filter(|r| r.bands.iter().zip(&first.bands).any(|(b, f)| b.name != f.name || b.classes != f.classes))
        .map(|r| r.name.as_str())
        .collect();
    if !other.is_empty() {
        s.notify(format!("{}: band or class names other than {}'s, which the mosaic keeps", other.join(", "), first.name), false);
    }
    let path = ps(p, "path").map(str::trim).filter(|p| !p.is_empty()).map(str::to_string);
    if let Some(path) = &path {
        super::raster::write_and_report(s, &out, 0, false, path)?;
    }
    let mut v = add(s, out, false)?;
    if let (Some(o), Some(path)) = (v.as_object_mut(), path) {
        o.insert("path".into(), json!(path));
    }
    Ok(v)
}

/// Whether `r` can join a mosaic on `first`'s grid: one coordinate system, cell size and band count.
fn mosaic_matches(first: &Raster, r: &Raster) -> Result<(), String> {
    first.require_coordinates()?;
    r.require_coordinates()?;
    let (pw, ph) = (first.transform.pixel_width, first.transform.pixel_height);
    let same = |a: f64, b: f64| (a - b).abs() <= 1e-6 * a.abs().max(b.abs());
    if r.crs != first.crs {
        return Err(format!(
            "{} is in {}, {} in {}: reproject them to one coordinate system first",
            r.name,
            r.crs.label(),
            first.name,
            first.crs.label()
        ));
    }
    if !same(r.transform.pixel_width, pw) || !same(r.transform.pixel_height, ph) {
        return Err(format!(
            "{} has {} × {} cells, {} {} × {}: resample them to one cell size first",
            r.name,
            r.transform.pixel_width,
            r.transform.pixel_height.abs(),
            first.name,
            pw,
            ph.abs()
        ));
    }
    if r.band_count() != first.band_count() {
        return Err(format!("{} has {} bands, {} {}", r.name, r.band_count(), first.name, first.band_count()));
    }
    // The mosaic takes the first input's colour, so a last band that is alpha in one input and
    // data in another (RGBA and RGB with near-infrared) would take one role for both.
    let alpha = |r: &Raster| matches!(r.color, ColorHint::Rgb { alpha: true } | ColorHint::GrayAlpha);
    if alpha(r) != alpha(first) {
        let (with, without) = if alpha(r) { (r, first) } else { (first, r) };
        return Err(format!("{}'s last band is transparency, {}'s is data: use rasters with matching band roles", with.name, without.name));
    }
    Ok(())
}

/// The cells of `r` in a column and row window, as a new raster on the same grid. With a mask
/// (row-major over the window), the cells it leaves out become no-data.
fn clip_window(
    r: &Raster,
    cols: std::ops::Range<u32>,
    rows: std::ops::Range<u32>,
    mask: Option<&[bool]>,
    name: String,
) -> Result<Raster, String> {
    r.require_coordinates()?;
    let (w, h) = (cols.end.saturating_sub(cols.start), rows.end.saturating_sub(rows.start));
    let nodata = if mask.is_some() { r.nodata.or(Some(NODATA as f64)) } else { r.nodata };
    let fill = nodata.map(|v| v as f32).unwrap_or(NODATA);
    // Each band's cells counted and cancellable, as in a run on another thread.
    let (wu, width) = (w as usize, r.width as usize);
    let bands = r
        .bands
        .iter()
        .map(|b| {
            rasterops::per_index(
                wu * h as usize,
                wu,
                1,
                || (),
                |_, i| match mask.map(|m| m.get(i).copied().unwrap_or(false)) {
                    Some(false) => fill,
                    _ => {
                        let (row, col) = (rows.start as usize + i / wu.max(1), cols.start as usize + i % wu.max(1));
                        b.data.get(row * width + col).copied().unwrap_or(fill)
                    }
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let o = r.transform.to_world(f64::from(cols.start), f64::from(rows.start));
    let gt = GeoTransform { origin_x: o.x, origin_y: o.y, ..r.transform };
    let mut out = Raster::new(name, w, h, bands, gt, r.crs, nodata, r.color.clone(), r.data_type.clone()).map_err(|e| e.to_string())?;
    out.bands.iter_mut().zip(&r.bands).for_each(|(o, i)| o.name = i.name.clone());
    // Some of the values rounded when the file was read may be in the window: how many isn't known.
    out.keep_rounded_mark(r);
    Ok(out)
}

/// A point given as [lon, lat]; anything else is an error (a dropped point would give a wrong
/// answer that looks right).
fn point(v: &Value) -> Result<Coord, String> {
    match (v.get(0).and_then(Value::as_f64), v.get(1).and_then(Value::as_f64)) {
        (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Ok(Coord::new(x, y)),
        _ => Err(format!("each point is [lon, lat], not {v}")),
    }
}

/// The columns and rows of a raster under a longitude/latitude box, or `None` when it misses.
fn pixel_window(r: &Raster, e: &Extent) -> Option<(std::ops::Range<u32>, std::ops::Range<u32>)> {
    let tr = r.transformer();
    let (mut c0, mut r0, mut c1, mut r1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for corner in [Coord::new(e.min_x, e.min_y), Coord::new(e.max_x, e.min_y), Coord::new(e.max_x, e.max_y), Coord::new(e.min_x, e.max_y)] {
        if let Some(w) = tr.from_lonlat(corner) {
            let (c, rr) = r.transform.to_pixel(w);
            (c0, r0, c1, r1) = (c0.min(c), r0.min(rr), c1.max(c), r1.max(rr));
        }
    }
    if c0 > c1 || c1 < 0.0 || r1 < 0.0 || c0 > f64::from(r.width) || r0 > f64::from(r.height) {
        return None;
    }
    let cols = (c0.floor().max(0.0) as u32).min(r.width)..(c1.ceil().max(0.0) as u32 + 1).min(r.width);
    let rows = (r0.floor().max(0.0) as u32).min(r.height)..(r1.ceil().max(0.0) as u32 + 1).min(r.height);
    Some((cols, rows))
}

fn edit_pixels(s: &mut Session, p: &Value) -> CmdResult {
    let layer = ps(p, "layer").ok_or("layer is required")?.to_string();
    let r = raster(s, p)?;
    r.require_coordinates()?;
    let ring: Vec<Coord> = p
        .get("polygon")
        .and_then(Value::as_array)
        .ok_or("polygon is required")?
        .iter()
        .take(10_000)
        .map(point)
        .collect::<Result<_, _>>()?;
    if ring.len() < 3 {
        return Err("the polygon needs at least three points".into());
    }
    let mut e = Extent::EMPTY;
    ring.iter().for_each(|c| e.expand(*c));
    let rings = vec![ring];
    let value = match f(p, "value") {
        Some(v) => v as f32,
        None => r.nodata.map(|v| v as f32).unwrap_or(NODATA),
    };
    let only = match p.get("band") {
        None => None,
        Some(_) => Some(
            uint(p, "band")
                .filter(|b| *b >= 1 && *b <= r.band_count() as u64)
                .ok_or_else(|| format!("{} has bands 1 to {}", r.name, r.band_count()))? as usize
                - 1,
        ),
    };
    let mut bands = copied(&r)?;
    let mut n = 0usize;
    // Only the pixels under the polygon's box are visited.
    let (cols, rows) = pixel_window(&r, &e).ok_or("the polygon is outside the raster")?;
    let tr = r.transformer();
    for row in rows {
        for col in cols.clone() {
            let w = r.transform.to_world(col as f64 + 0.5, row as f64 + 0.5);
            let Some(ll) = tr.to_lonlat(w) else { continue };
            if !e.contains(ll) || !rustgis_data::geometry::polygon_contains(&rings, ll) {
                continue;
            }
            for (bi, b) in bands.iter_mut().enumerate() {
                if only.is_none_or(|o| o == bi)
                    && let Some(v) = b.get_mut((row * r.width + col) as usize)
                {
                    *v = value;
                }
            }
            n += 1;
        }
    }
    if n == 0 {
        return Err("no pixel centres fall in the polygon".into());
    }
    let nodata = if f(p, "value").is_none() && r.nodata.is_none() { Some(NODATA as f64) } else { r.nodata };
    let mut out = Raster::new(r.name.clone(), r.width, r.height, bands, r.transform, r.crs, nodata, r.color.clone(), r.data_type.clone())
        .map_err(|e| e.to_string())?;
    out.bands.iter_mut().zip(&r.bands).for_each(|(o, i)| o.name = i.name.clone());
    // The edit may have overwritten some of the values rounded when the file was read.
    out.keep_rounded_mark(&r);
    replace(s, &layer, out)?;
    Ok(json!({"pixels": n}))
}

fn measure(s: &mut Session, p: &Value) -> CmdResult {
    let pts: Vec<Coord> =
        p.get("points").and_then(Value::as_array).ok_or("points is required")?.iter().take(10_000).map(point).collect::<Result<_, _>>()?;
    let kind = ps(p, "kind").ok_or("kind is required")?;
    let dem = ps(p, "layer").map(|n| raster_named(s, n)).transpose()?;
    // A scan's heights would be read at the made-up place it is laid.
    if let Some(d) = &dem {
        d.require_coordinates()?;
    }
    let z = |c: Coord| dem.as_ref().and_then(|d| rasterops::elevation_at(d, 0, c));
    // Ground lengths and areas on the map's body.
    let body = s.map().body();
    let out = match kind {
        "distance" => {
            if pts.len() < 2 {
                return Err("a distance needs two or more points".into());
            }
            let flat = body.length(rustgis_geom::geodesic_length_m(&pts));
            // Along the surface when there's an elevation raster.
            let surface = dem.as_ref().map(|d| {
                let prof = rasterops::profile(d, 0, &pts, 500);
                prof.windows(2)
                    .map(|w| match w {
                        [a, c] => {
                            let dh = match (a.2, c.2) {
                                (Some(x), Some(y)) => y - x,
                                _ => 0.0,
                            };
                            (body.length(c.0 - a.0).powi(2) + dh * dh).sqrt()
                        }
                        _ => 0.0,
                    })
                    .sum::<f64>()
            });
            json!({"kind": "distance", "metres": flat, "surfaceMetres": surface})
        }
        "area" => {
            if pts.len() < 3 {
                return Err("an area needs three or more points".into());
            }
            let mut ring = pts.clone();
            if let Some(first) = pts.first() {
                ring.push(*first);
            }
            if rustgis_geom::ring_area_is_ambiguous(&ring) {
                return Err("the area is ambiguous: 1 ring winds beyond one whole surface".into());
            }
            let area = body.area(rustgis_geom::geodesic_ring_area_m2(&ring));
            if !area.is_finite() {
                return Err("the area cannot be measured from these coordinates".into());
            }
            json!({"kind": "area", "squareMetres": area, "perimeterMetres": body.length(rustgis_geom::geodesic_ring_perimeter_m(&ring))})
        }
        "height" => {
            let (Some(a), Some(c)) = (pts.first(), pts.get(1)) else { return Err("a height needs two points (base, top)".into()) };
            let (za, zc) = (z(*a).ok_or("the base is off the elevation raster")?, z(*c).ok_or("the top is off the elevation raster")?);
            json!({"kind": "height", "metres": zc - za, "base": za, "top": zc})
        }
        "volume" => {
            let d = dem.as_ref().ok_or("a volume needs an elevation raster (layer)")?;
            if pts.len() < 3 {
                return Err("a volume needs a polygon of three or more points".into());
            }
            let base = f(p, "base");
            let rings = vec![pts.clone()];
            let (cx, cy) = d.cell_size_m();
            // A geographic raster's cells are degrees, measured on the Earth: on the map's body.
            let (cx, cy) = if d.crs.is_geographic() { (body.length(cx), body.length(cy)) } else { (cx, cy) };
            let (mut above, mut below, mut n, mut lowest) = (0.0, 0.0, 0usize, f64::INFINITY);
            let mut cells = Vec::new();
            let mut e = Extent::EMPTY;
            pts.iter().for_each(|c| e.expand(*c));
            let (cols, rows) = pixel_window(d, &e).ok_or("the polygon is outside the raster")?;
            let tr = d.transformer();
            for row in rows.map(i64::from) {
                for col in cols.clone().map(i64::from) {
                    let w = d.transform.to_world(col as f64 + 0.5, row as f64 + 0.5);
                    let Some(ll) = tr.to_lonlat(w) else { continue };
                    if !e.contains(ll) || !rustgis_data::geometry::polygon_contains(&rings, ll) {
                        continue;
                    }
                    if let Some(v) = d.value(0, col, row) {
                        cells.push(v as f64);
                        lowest = lowest.min(v as f64);
                    }
                }
            }
            let base = base.unwrap_or(lowest);
            for v in &cells {
                let dz = v - base;
                if dz > 0.0 {
                    above += dz * cx * cy;
                } else {
                    below -= dz * cx * cy;
                }
                n += 1;
            }
            if n == 0 {
                return Err("no cells fall in the polygon".into());
            }
            json!({"kind": "volume", "cubicMetresAbove": above, "cubicMetresBelow": below, "base": base, "cells": n})
        }
        k => return Err(format!("unknown measurement '{k}'")),
    };
    s.measurements.push(out.clone());
    if s.measurements.len() > 500 {
        s.measurements.remove(0);
    }
    Ok(out)
}

/// A workspace's cell size (degrees, or null for the finest image's), checked the same way when
/// a workspace is made and when its options change.
fn cell_size(p: &Value) -> Result<Option<f64>, String> {
    match p.get("cellSize") {
        None | Some(Value::Null) => Ok(None),
        Some(_) => f(p, "cellSize")
            .filter(|c| *c > 0.0 && *c < 10.0)
            .map(Some)
            .ok_or_else(|| "cellSize is degrees, above 0 and below 10".to_string()),
    }
}

/// A workspace's mosaic method: first or mean.
fn method(p: &Value, default: &str) -> Result<String, String> {
    match ps(p, "method").unwrap_or(default) {
        m @ ("first" | "mean") => Ok(m.to_string()),
        m => Err(format!("method is first or mean, not '{m}'")),
    }
}

fn new_workspace(s: &mut Session, p: &Value) -> CmdResult {
    let kind = ps(p, "kind").ok_or("kind is required")?;
    if !matches!(kind, "ortho" | "reality") {
        return Err("kind is ortho or reality".into());
    }
    let images = workspace_images(s, p)?;
    if images.is_empty() {
        return Err("give at least one image".into());
    }
    let base = ps(p, "name")
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(if kind == "ortho" { "Ortho Mapping" } else { "Reality Mapping" })
        .to_string();
    let mut nm = base.clone();
    let mut i = 1;
    while s.project.workspaces.iter().any(|w| w.name.eq_ignore_ascii_case(&nm)) {
        i += 1;
        nm = format!("{base} {i}");
    }
    let method = method(p, if kind == "reality" { "mean" } else { "first" })?;
    let cell_size = cell_size(p)?;
    s.project.workspaces.push(Workspace { name: nm.clone(), kind: kind.into(), images, cell_size, method });
    Ok(json!({"workspace": nm}))
}

/// Mosaics a workspace's images onto one longitude/latitude grid.
fn mosaic(s: &mut Session, p: &Value) -> CmdResult {
    super::workflows::made_or_ran(s, p, "imagery.mosaic")
}

/// What a workspace's mosaic reads of the session (its images and options), and the work that
/// lays them on one grid.
pub(crate) fn mosaic_work(s: &Session, p: &Value) -> Result<super::workflows::Work, String> {
    let w = workspace_of(s, p)?.clone();
    let imgs: Vec<Arc<Raster>> = w.images.iter().map(|i| raster_named(s, i)).collect::<Result<_, _>>()?;
    let mut e = Extent::EMPTY;
    for r in &imgs {
        r.require_coordinates()?;
        e = e.union(&r.extent());
    }
    let finest = imgs
        .iter()
        .map(|r| {
            let (cx, _) = r.cell_size_m();
            cx / (111_320.0 * r.extent().center().y.to_radians().cos().abs().max(0.01))
        })
        .fold(f64::INFINITY, f64::min);
    let cell = w.cell_size.unwrap_or(finest).max(1e-7);
    let (cols, rows) = ((e.width() / cell).ceil().max(1.0) as usize, (e.height() / cell).ceil().max(1.0) as usize);
    if cols.saturating_mul(rows) > rasterops::max_pixels() / 2 {
        return Err(format!("the mosaic would be {cols} × {rows} cells; set a larger cell size in the workspace options"));
    }
    let nb = imgs.iter().map(|r| r.band_count()).min().unwrap_or(1).min(if w.kind == "reality" { 1 } else { 4 });
    let omitted: Vec<String> = w
        .images
        .iter()
        .zip(&imgs)
        .filter(|(_, r)| r.band_count() > nb)
        .map(|(name, r)| {
            let bands = ((nb + 1)..=r.band_count()).map(|b| b.to_string()).collect::<Vec<_>>().join(", ");
            format!("'{name}': band(s) {bands}")
        })
        .collect();
    let extra = (!omitted.is_empty()).then(|| {
        json!({"messages": [format!(
            "{} image(s) have bands omitted from the {nb}-band mosaic: {}",
            omitted.len(), omitted.join("; ")
        )]})
    });
    Ok(Box::new(move || {
        let refs: Vec<&Raster> = imgs.iter().map(|r| &**r).collect();
        let mut bands = rasterops::mosaic(&refs, &e, cell, (cols, rows), nb, w.method == "mean")?;
        // Colour only when every image is colour. A mosaic of 8-bit images stays 8-bit (means
        // rounded), so it displays as they do; anything else is Float32.
        let all_rgb = imgs.iter().all(|r| matches!(r.color, ColorHint::Rgb { .. }));
        let all_alpha = imgs.iter().all(|r| matches!(r.color, ColorHint::Rgb { alpha: true }));
        if nb == 4 && !all_alpha && imgs.iter().any(|r| matches!(r.color, ColorHint::Rgb { alpha: true })) {
            return Err("RGBA images cannot share a mosaic's fourth band with data bands: use images with matching band roles".into());
        }
        let color = if matches!(nb, 3 | 4) && all_rgb { ColorHint::Rgb { alpha: nb == 4 && all_alpha } } else { ColorHint::Gray };
        let all_u8 = imgs.iter().all(|r| r.data_type == "UInt8");
        let dt = if matches!(color, ColorHint::Rgb { .. }) && all_u8 { "UInt8" } else { "Float32" };
        if dt == "UInt8" {
            for v in bands.iter_mut().flatten().filter(|v| **v != NODATA) {
                *v = v.round().clamp(0.0, 255.0);
            }
        }
        let gt = GeoTransform { origin_x: e.min_x, origin_y: e.max_y, pixel_width: cell, pixel_height: -cell };
        let suffix = if w.kind == "reality" { "DSM" } else { "Orthomosaic" };
        let out =
            Raster::new(format!("{} {suffix}", w.name), cols as u32, rows as u32, bands, gt, Crs::LonLat, Some(NODATA as f64), color, dt)
                .map_err(|e| e.to_string())?;
        super::workflows::made(out, extra)
    }))
}

/// A surface model as a triangle mesh (Wavefront OBJ), in metres from its south-west corner.
fn mesh(s: &mut Session, p: &Value) -> CmdResult {
    if ps(p, "path").is_some_and(|path| !path.to_ascii_lowercase().ends_with(".obj")) {
        return Err("the mesh is written to an .obj file".into());
    }
    let r = raster(s, p)?;
    // The mesh is laid out in metres on the ground, which a scan has none of.
    r.require_coordinates()?;
    let step = uint(p, "step").unwrap_or(((r.width.max(r.height) / 300).max(1)) as u64).clamp(1, 1000) as i64;
    let (cx, cy) = r.cell_size_m();
    let (nc, nr) = ((r.width as i64 - 1) / step + 1, (r.height as i64 - 1) / step + 1);
    if nc * nr > 1_000_000 {
        return Err("the mesh would be too large; use a larger step".into());
    }
    let mut obj = format!("# {} surface mesh exported by RustGIS\no {}\n", r.name, r.name.replace(char::is_whitespace, "_"));
    let mut ids = vec![0usize; (nc * nr) as usize];
    let mut v = 0usize;
    for j in 0..nr {
        for i in 0..nc {
            if let Some(z) = r.value(0, i * step, j * step) {
                v += 1;
                if let Some(slot) = ids.get_mut((j * nc + i) as usize) {
                    *slot = v;
                }
                obj.push_str(&format!("v {:.2} {:.2} {:.2}\n", i as f64 * step as f64 * cx, z, j as f64 * step as f64 * cy));
            }
        }
    }
    let id = |i: i64, j: i64| ids.get((j * nc + i) as usize).copied().unwrap_or(0);
    let mut faces = 0;
    for j in 0..nr - 1 {
        for i in 0..nc - 1 {
            let (a, b2, c, d) = (id(i, j), id(i + 1, j), id(i + 1, j + 1), id(i, j + 1));
            if a > 0 && b2 > 0 && c > 0 {
                obj.push_str(&format!("f {a} {c} {b2}\n"));
                faces += 1;
            }
            if a > 0 && c > 0 && d > 0 {
                obj.push_str(&format!("f {a} {d} {c}\n"));
                faces += 1;
            }
        }
    }
    if faces == 0 {
        return Err("the raster has no surface to mesh".into());
    }
    match ps(p, "path") {
        Some(path) => {
            rustgis_data::io::write_atomic(std::path::Path::new(path), obj.as_bytes()).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!({"path": path, "vertices": v, "faces": faces}))
        }
        None => Ok(json!({"text": obj, "vertices": v, "faces": faces})),
    }
}

/// The literal raster/secondary inputs used by this module, without reading any pixels/files.
fn raster_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    super::Inputs::named(s.map(), [raster_name(p)?])
}
/// Measures distance and area without a raster, or binds the explicitly selected elevation raster.
fn measure_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    super::Inputs::named(input.map(), ps(p, "layer"))
}
/// Binds both literal before/after rasters used by change detection.
fn change_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    super::Inputs::named(s.map(), change_names(p)?)
}
/// Binds the primary raster and optional literal zone polygon.
fn spectral_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    super::Inputs::named(s.map(), std::iter::once(raster_name(p)?).chain(spectral_zone(p)))
}
/// Binds the primary raster and the actual training polygon selector.
fn supervised_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    super::Inputs::named(s.map(), [raster_name(p)?, training_name(p)?])
}
/// Binds a clip polygon only when the selected processing operation consumes one.
fn process_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let clip = clip_polygon(p);
    super::Inputs::named(s.map(), std::iter::once(raster_name(p)?).chain(clip))
}
/// Binds the grid template when present, otherwise the optional extent layer.
fn project_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    super::Inputs::named(s.map(), std::iter::once(raster_name(p)?).chain(project_grid(p)?.or_else(|| project_extent_name(p))))
}
/// The rasterizer's literal vector selector, shared with pure dependency planning.
fn rasterize_name(p: &Value) -> Result<&str, String> {
    ps(p, "layer").ok_or("layer is required: a polygon layer".into())
}
/// Binds the exact vector selector and only the grid or extent the rasterizer consumes.
fn rasterize_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let mut inputs = super::Inputs::named(input.map(), [rasterize_name(p)?])?;
    if let Some(name) = project_grid(p)?.or_else(|| project_extent_name(p)) {
        inputs.extend(super::Inputs::named(input.map(), [name])?);
    }
    Ok(inputs)
}
/// Retains in-project raster precedence over paths using the executor's pure lookup.
fn mosaic_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let inputs = p.get("inputs").and_then(Value::as_array).ok_or("inputs are raster names or files")?;
    let mut names = Vec::new();
    for v in inputs {
        let name = mosaic_name(v)?;
        if mosaic_layer(input, v).is_ok() {
            names.push(name);
        }
    }
    super::Inputs::named(s.map(), names)
}
/// Binds raster images validated for a newly created imagery workspace.
fn workspace_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let names = workspace_images(input, p)?;
    super::Inputs::named(s.map(), names.iter().map(String::as_str))
}
/// Resolves the stored workspace and its image names without mosaicking pixels.
fn workspace_mosaic_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let workspace = workspace_of(input, p)?;
    super::Inputs::named(s.map(), workspace.images.iter().map(String::as_str))
}

/// The literal primary raster selector shared by handlers and their pure input metadata.
pub(crate) fn raster_name(p: &Value) -> Result<&str, String> {
    ps(p, "layer").ok_or("layer is required".into())
}
/// The literal training polygons used by imagery and deep learning.
pub(crate) fn training_name(p: &Value) -> Result<&str, String> {
    ps(p, "training").ok_or("training (a polygon layer) is required".into())
}
/// Parses the same required literal names used by change detection.
fn change_names(p: &Value) -> Result<[&str; 2], String> {
    Ok([ps(p, "before").ok_or("before is required")?, ps(p, "after").ok_or("after is required")?])
}
/// Selects only a string zone, as the spectral executor does.
fn spectral_zone(p: &Value) -> Option<&str> {
    ps(p, "zone")
}
/// Only the clip operation consumes the polygon selector.
fn clip_polygon(p: &Value) -> Option<&str> {
    (ps(p, "operation") == Some("clip")).then(|| ps(p, "polygon")).flatten()
}
/// A string extent other than view names a literal project vector layer.
fn project_extent_name(p: &Value) -> Option<&str> {
    ps(p, "extent").filter(|n| *n != "view")
}

/// Validates literal raster names in the workspace's declared image order.
fn workspace_images(input: &impl super::InputSource, p: &Value) -> Result<Vec<String>, String> {
    let context = input.input_context();
    let s = &context;
    p.get("images")
        .and_then(Value::as_array)
        .ok_or("images is required")?
        .iter()
        .map(|v| {
            let n = v.as_str().ok_or("images are raster layer names")?;
            s.map().layer_by_name(n).filter(|l| l.is_raster()).map(|l| l.name.clone()).ok_or_else(|| format!("no raster layer '{n}'"))
        })
        .collect::<Result<_, String>>()
}

/// Resolves the stored workspace by its case-insensitive name.
fn workspace_of<'a>(input: &'a impl super::InputSource, p: &Value) -> Result<&'a Workspace, String> {
    let context = input.input_context();
    let s = &context;
    let name = ps(p, "name").ok_or("name is required")?;
    s.project.workspaces.iter().find(|w| w.name.eq_ignore_ascii_case(name)).ok_or_else(|| format!("no workspace '{name}'"))
}

/// Distinguishes omitted/null grid from a required literal raster name.
fn project_grid(p: &Value) -> Result<Option<&str>, String> {
    match p.get("grid") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_str().map(Some).ok_or_else(|| "grid is a raster layer's name".into()),
    }
}
