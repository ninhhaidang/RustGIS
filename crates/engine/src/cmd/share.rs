//! Publish tab: export the map as PNG or GeoTIFF and layers as GeoJSON, CSV, FlatGeobuf, GeoPackage,
//! GeoParquet, Shapefile, KML/KMZ or GPX.

use std::path::Path;

use rustgis_data::io::{GpkgMode, GpkgTable, GpkgWritten};
use rustgis_data::raster::Crs;
use rustgis_render::View;
use serde_json::{Value, json};

use super::exports::{Work, Wrote};
use super::{f, need_layer, ps};
use crate::{CommandSpec as C, Session};

/// The map export binds exactly the map and tile routes the worker will copy.
fn map_inputs(s: &super::InputContext<'_>, _: &Value) -> Result<super::Inputs, String> {
    Ok(super::tiles::render_inputs(s.map()))
}

pub fn commands() -> Vec<C> {
    vec![
        C::new("share.exportMap", "Export Map", "Publish › Output", |s, p| {
            // A style added with its own view: the export waits for it to load, as it does for
            // tiles. The app runs it here, not on a thread, while one loads.
            super::tiles::follow_style_view(s, web_time::Duration::from_secs(30));
            super::exports::write_here(s, p, "share.exportMap")
        }).inputs(map_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .view()
            .params(r#"{"path": "*.png|*.tif|*.tiff", "width"?: "points (pixels at 96 dpi), the view fitted in it", "height"?: points, "dpi"?: "48..600 (default 96)", "fit"?: "true to lower the dpi until a side fits 8192 pixels, saying so", "cog"?: "true for a cloud-optimised GeoTIFF"}"#),
        C::new("share.exportLayer", "Export Features", "Publish › Output", |s, p| super::exports::write_here(s, p, "share.exportLayer")).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .view()
            .when(need_layer)
            .params(r#"{"layer"?, "path": "*.geojson|*.csv|*.fgb|*.gpkg|*.parquet|*.shp|*.zip|*.kml|*.kmz|*.gpx", "crs"?: "EPSG:… (or WKT) for a GeoPackage, Shapefile or FlatGeobuf (default: the system the layer was read in, else EPSG:4326)", "mode"?: "newFile (default: refuses an existing file) | addTable | replaceTable, for a GeoPackage", "tableName"?: "the GeoPackage table (default: the layer's name)"}"#),
        C::new("share.exportTable", "Export Table", "Publish › Output", |s, p| {
            let owner = s.continuation()?;
            owner.check()?;
            let _progress = rustgis_analysis::control::install_progress(None);
            let path = ps(p, "path").ok_or("path is required")?;
            let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            let gpkg = gpkg_table(p, &ext)?;
            let t = s.project.tables.get(s.table_index(p)?).ok_or("no such table")?;
            let default = t.dataset.name.clone();
            // Tables may retain geometry and its import losses, just as feature layers do.
            let notes = rustgis_data::export_notes([(&t.dataset, t.dataset.extent())], path, Crs::LonLat);
            let mut dbf_notes = Vec::new();
            if matches!(ext.as_str(), "shp" | "zip") {
                dbf_notes.extend(rustgis_data::io::shapefile_names_note(&t.dataset.fields, path));
                dbf_notes.extend(rustgis_data::io::shapefile_values_notes(&t.dataset, path));
            }
            if matches!(ext.as_str(), "parquet" | "geoparquet") {
                dbf_notes.extend(rustgis_data::source::parquet::values_note(&t.dataset, path));
            }
            let (left_out, table) =
                rustgis_data::io::write_path_to(&t.dataset, Path::new(path), Crs::LonLat, &gpkg).map_err(|e| e.to_string())?;
            s.check_continuation(&owner).map_err(|why| super::exports::completion_refused(Some(path), why))?;
            if left_out > 0 {
                s.notify(format!("{path}: {left_out} features without drawable geometry were left out"), false);
            }
            for note in notes.into_iter().chain(dbf_notes) {
                s.notify(note, false);
            }
            let mut out = json!({"path": path});
            report_table(s, path, &gpkg, &default, table, &mut out);
            Ok(out)
        }).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .params(
            r#"{"table": name|id (a standalone table), "path": "*.geojson|*.csv|*.fgb|*.gpkg|*.parquet|*.shp|*.zip|*.kml|*.kmz|*.gpx", "mode"?: "newFile (default: refuses an existing file) | addTable | replaceTable, for a GeoPackage", "tableName"?: "the GeoPackage table (default: the table's name)"}"#,
        )
        .view(),
    ]
}

/// A GeoPackage export's table, from `mode` and `tableName`, which only a GeoPackage (extension
/// `ext`) takes; the front end checks a request with it before asking for a file.
pub fn gpkg_table(p: &Value, ext: &str) -> Result<GpkgTable, String> {
    // Not a string is refused, not taken for the default.
    let text = |key: &str| match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_str().map(|s| Some(s.trim())).ok_or_else(|| format!("{key} must be a string")),
    };
    let mode = match text("mode")? {
        None | Some("newFile") => GpkgMode::NewFile,
        Some("addTable") => GpkgMode::AddTable,
        Some("replaceTable") => GpkgMode::ReplaceTable,
        Some(other) => return Err(format!("unknown mode '{other}' (newFile, addTable or replaceTable)")),
    };
    let name = text("tableName")?.filter(|n| !n.is_empty()).map(String::from);
    if ext != "gpkg" && (mode != GpkgMode::NewFile || name.is_some()) {
        return Err("mode and tableName are for a GeoPackage (.gpkg) export".into());
    }
    Ok(GpkgTable { mode, name })
}

/// Puts the table a GeoPackage export wrote, and whether it replaced one, in its result `out`;
/// an export into an existing GeoPackage also says so, what went with a replaced table, and
/// which views and triggers elsewhere name it. Layers read in place from a replaced table read
/// it again.
fn report_table(s: &mut Session, path: &str, gpkg: &GpkgTable, default: &str, table: Option<GpkgWritten>, out: &mut Value) {
    let Some(table) = table else { return };
    // A new file's table is said only when it isn't the name asked for, or by default the
    // layer's (a reserved name is prefixed, control characters and edge spaces dropped); one
    // added to or replaced in a file is always named below.
    let asked = gpkg.name.as_deref().unwrap_or(default);
    if gpkg.mode == GpkgMode::NewFile && asked != table.table {
        let note = if asked.trim().is_empty() {
            format!("{path}: wrote the table {}, as there was no name to give it", table.table)
        } else {
            // Escaped: a control character in the name would otherwise reach the terminal.
            let asked: String =
                asked.chars().map(|c| if c.is_control() { c.escape_default().to_string() } else { c.to_string() }).collect();
            format!(
                "{path}: wrote the table {} instead of '{asked}' (GeoPackage table names can't start with gpkg_, sqlite_ or rtree_, and lose control characters and edge spaces)",
                table.table
            )
        };
        s.notify(note, false);
    }
    if gpkg.mode != GpkgMode::NewFile {
        let (reread, failed) =
            if table.replaced { super::gpkg::reset_sources(s, Path::new(path), &table.table, true) } else { (0, Vec::new()) };
        let note = if table.replaced {
            format!(
                "{path}: replaced its table {}, dropping the old table's indexes, triggers, column descriptions, metadata links and extension records",
                table.table
            )
        } else {
            format!("{path}: added the table {}", table.table)
        };
        s.notify(note, false);
        if reread > 0 {
            let layers = if reread == 1 { "layer reads" } else { "layers read" };
            s.notify(format!("{path}: {reread} {layers} {} in place, now the new table", table.table), false);
        }
        let held = if table.replaced { super::gpkg::editing(s, Path::new(path), &table.table) } else { Vec::new() };
        if !held.is_empty() {
            let n = held.len();
            let (layers, show) = if n == 1 { ("layer", "shows") } else { ("layers", "show") };
            let note = format!(
                "{path}: {n} {layers} loaded for editing from {} still {show} the old table's rows; Discard Edits loads the new one: {}",
                table.table,
                held.join(", ")
            );
            s.notify(note, false);
        }
        if !failed.is_empty() {
            let layers = if failed.len() == 1 { "layer" } else { "layers" };
            let note = format!(
                "{path}: {} {layers} reading {} in place couldn't open the new table: {}",
                failed.len(),
                table.table,
                failed.join(", ")
            );
            s.notify(note, true);
        }
        if !table.dependents.is_empty() {
            let n = table.dependents.len();
            s.notify(
                format!(
                    "{path}: {n} view{} or trigger{} naming {} may no longer work if a column it uses is gone: {}",
                    if n == 1 { "" } else { "s" },
                    if n == 1 { "" } else { "s" },
                    table.table,
                    table.dependents.join(", ")
                ),
                false,
            );
        }
    }
    out["table"] = json!(table.table);
    out["replaced"] = json!(table.replaced);
}

/// The active map's view as PNG bytes at `dpi`, or, for a view too large for it, at the most that
/// fits 8192 pixels a side, said: for the app's Export Map download and Capture To Clipboard,
/// which offer no size.
pub fn map_png_fitted(s: &mut Session, dpi: f64) -> Result<Vec<u8>, String> {
    let view = export_view(s.map().camera, s.view_size, None, None, Some(dpi));
    let fitted = fitted_dpi(&view, dpi, false);
    let png = map_png(s, None, None, Some(fitted.unwrap_or(dpi)))?;
    // A view past 8000 points is cut to its middle, as Export Map's is: said too, with where a
    // size can be given (this image takes none).
    let (vw, vh) = s.view_size;
    if vw > MOST_POINTS || vh > MOST_POINTS {
        s.notify(
            format!(
                "The map image: the view is {vw:.0} × {vh:.0} points, more than the {MOST_POINTS:.0} a side an image holds, so it is cut to its middle: Export Map (share.exportMap) with a width or height of at most {MOST_POINTS:.0} draws it whole"
            ),
            false,
        );
    }
    if let Some(lower) = fitted {
        s.notify(
            format!("The map is drawn at {lower} DPI, the most an image of this view holds ({} pixels a side)", rustgis_render::MAX_SIDE),
            false,
        );
    }
    Ok(png)
}

/// Renders the active map at the current view (or a requested size) to PNG bytes.
pub fn map_png(s: &mut Session, width: Option<f64>, height: Option<f64>, dpi: Option<f64>) -> Result<Vec<u8>, String> {
    let view_size = s.view_size;
    png_of(s.map_mut(), view_size, width, height, dpi)
}

/// Renders `map` at its camera in a view of `(vw, vh)` points (or a requested size) to PNG bytes.
fn png_of(
    map: &mut rustgis_map::Map,
    view_size: (f64, f64),
    width: Option<f64>,
    height: Option<f64>,
    dpi: Option<f64>,
) -> Result<Vec<u8>, String> {
    let view = export_view(map.camera, view_size, width, height, dpi);
    // Past the canvas's limit the picture would be cut off at its top left: refused instead.
    fits_pixels(&view, false)?;
    rustgis_render::render_png(map, &view)
}

/// Refuses an export of `view` with a side past the [`rustgis_render::MAX_SIDE`] pixels a canvas
/// holds, saying how many it would be (and, for the box around a `turned` view, that the box is
/// why).
fn fits_pixels(view: &View, turned: bool) -> Result<(), String> {
    let (pw, ph) = ((view.width * view.pixels_per_point).round(), (view.height * view.pixels_per_point).round());
    let most = f64::from(rustgis_render::MAX_SIDE);
    if pw <= most && ph <= most {
        return Ok(());
    }
    let why = if turned { " (the box around the turned view; or set the bearing to 0)" } else { "" };
    Err(format!("{pw} × {ph} pixels{why} is more than the {most} a side an export draws: give a smaller width, height or dpi"))
}

/// The most characters of the credits a GeoTIFF export's notice shows; its answer has them all.
const CREDITS_SHOWN: usize = 200;

/// The most points a side an export of the map draws; a larger view is cut to its middle.
pub const MOST_POINTS: f64 = 8000.0;

/// What [`export_view`]'s limit of [`MOST_POINTS`] a side does to an export to `path` of a view
/// of `(vw, vh)` points, for it to say: with no size given, a larger view is cut to its middle;
/// a size given past the limit is drawn at the limit: the view still fits whole, with less ground
/// around it than the size asked for.
fn size_notes(
    path: &str,
    (vw, vh): (f64, f64),
    width: Option<f64>,
    height: Option<f64>,
) -> Vec<(rustgis_data::diagnostics::Severity, String)> {
    let fin = |v: Option<f64>| v.filter(|x| x.is_finite());
    let (width, height) = (fin(width), fin(height));
    let mut notes = Vec::new();
    let note = |text: String| (rustgis_data::diagnostics::Severity::Info, format!("{path}: {text}"));
    if width.is_none() && height.is_none() && (vw > MOST_POINTS || vh > MOST_POINTS) {
        notes.push(note(format!(
            "the view is {vw:.0} × {vh:.0} points, more than the {MOST_POINTS:.0} a side an export holds, so it is cut to its middle: give a width or height of at most {MOST_POINTS:.0} to draw it whole"
        )));
    }
    for (name, v) in [("width", width), ("height", height)] {
        if let Some(v) = v.filter(|v| *v > MOST_POINTS) {
            notes.push(note(format!(
                "a {name} of {v:.0} points is more than the {MOST_POINTS:.0} an export holds, so it is drawn at {MOST_POINTS:.0}, holding less ground than asked (the whole view still fits): a higher dpi gives more pixels"
            )));
        }
        if let Some(v) = v.filter(|v| *v < LEAST_POINTS) {
            notes.push(note(format!(
                "a {name} of {v} points is less than the {LEAST_POINTS:.0} an export draws, so it is drawn at {LEAST_POINTS:.0}"
            )));
        }
    }
    notes
}

/// The fewest points a side an export of the map draws.
const LEAST_POINTS: f64 = 16.0;
/// The DPI an export of the map draws at, at least and at most.
const DPI_RANGE: (f64, f64) = (48.0, 600.0);

/// What the DPI range does to an export to `path` asked at `dpi`, for it to say.
fn dpi_note(path: &str, dpi: Option<f64>) -> Option<(rustgis_data::diagnostics::Severity, String)> {
    let dpi = dpi.filter(|d| d.is_finite())?;
    let (least, most) = DPI_RANGE;
    let drawn = dpi.clamp(least, most);
    (drawn != dpi).then(|| {
        (
            rustgis_data::diagnostics::Severity::Info,
            format!("{path}: a dpi of {dpi} is outside {least:.0} to {most:.0}, so it is drawn at {drawn:.0}"),
        )
    })
}

/// With `fit`, the DPI an export of the view (at `dpi`) is lowered to so that no side passes the
/// [`rustgis_render::MAX_SIDE`] pixels a canvas holds, a GeoTIFF of a turned view counting the
/// box around it: the most whole DPI that fits, `None` when `dpi` already does.
fn fitted_dpi(view: &View, dpi: f64, tif: bool) -> Option<f64> {
    let (w, h) =
        if tif && is_turned(view.camera.bearing) { view.camera.rotated_size(view.width, view.height) } else { (view.width, view.height) };
    let side = w.max(h);
    let most = f64::from(rustgis_render::MAX_SIDE);
    // As the export sizes its canvas: a turned GeoTIFF's box rounded out, else rounded.
    let px = side * view.pixels_per_point;
    let px = if tif && is_turned(view.camera.bearing) { px.ceil() } else { px.round() };
    if !side.is_finite() || side <= 0.0 || px <= most {
        return None;
    }
    // Never above the DPI asked, nor below the least: `max` keeps the bounds in order (clamp
    // panics otherwise) for a caller asking below it.
    Some((most * 96.0 / side).floor().clamp(DPI_RANGE.0, dpi.max(DPI_RANGE.0)))
}

/// Whether a view at `bearing` is turned (not north up).
fn is_turned(bearing: f64) -> bool {
    let b = bearing.rem_euclid(360.0);
    b.min(360.0 - b) > 1e-9
}

/// The view an export of the map at `camera` draws, in a view of `(vw, vh)` points: a requested
/// size keeps the same ground extent, and `dpi` sets the pixels per point.
fn export_view(
    camera: rustgis_map::camera::Camera,
    (vw, vh): (f64, f64),
    width: Option<f64>,
    height: Option<f64>,
    dpi: Option<f64>,
) -> View {
    // NaN would pass through `clamp`; non-finite values fall back to the defaults.
    let fin = |v: Option<f64>| v.filter(|x| x.is_finite());
    let (width, height, dpi) = (fin(width), fin(height), fin(dpi));
    let w = width.unwrap_or(vw).clamp(LEAST_POINTS, MOST_POINTS);
    let h = height.unwrap_or(vh).clamp(LEAST_POINTS, MOST_POINTS);
    let ppp = dpi.unwrap_or(96.0).clamp(DPI_RANGE.0, DPI_RANGE.1) / 96.0;
    let mut camera = camera;
    // Keep the same ground extent when the size differs from the view.
    if width.is_some() || height.is_some() {
        camera.res *= (vw / w).max(vh / h);
        camera.clamp();
    }
    View { width: w, height: h, pixels_per_point: ppp, camera, sky: false }
}

/// The map view on one PDF page (for printing): the page is the view's shape at 96 points per
/// inch of screen, kept as vectors with what is pixels by nature drawn at `dpi`.
pub fn map_pdf(s: &mut Session, dpi: f64) -> Result<Vec<u8>, String> {
    let (vw, vh) = s.view_size;
    let ppp = (dpi / 96.0).clamp(0.5, 6.0);
    let view =
        View { width: vw.clamp(16.0, 8000.0), height: vh.clamp(16.0, 8000.0), pixels_per_point: ppp, camera: s.map().camera, sky: false };
    let drawing = rustgis_render::render_drawing(s.map_mut(), &view).ok_or("could not draw the map")?;
    let mm = |pt: f64| pt * 25.4 / 96.0;
    let (w, h) = (mm(view.width), mm(view.height));
    let page = if drawing.overflowed() {
        // Too large for vectors: the map as one picture, as it is drawn on screen.
        let png = rustgis_render::render_png(s.map_mut(), &view)?;
        let pm = rustgis_render::decode_png(&png).ok_or("could not draw the map")?;
        // Said once the picture is drawn. The budget counts the drawing's pictures (rasters,
        // tiles) as well as its shapes.
        let (mb, items) = (rustgis_render::vector::MAX_COST >> 20, rustgis_render::vector::MAX_ITEMS / 1_000_000);
        s.warn(format!(
            "The map is drawn as one picture, not vectors: kept as vectors, it would pass the drawing limit ({mb} MB or {items} million items)"
        ));
        rustgis_render::pdf::Page::new(&pm, w, h)
    } else {
        rustgis_render::pdf::Page::drawing(drawing, w, h)
    };
    Ok(rustgis_render::pdf::write(&[page], &s.map().name))
}

/// `share.exportMap`: draws the active map as it is now to a PNG, or a GeoTIFF for a `.tif` or
/// `.tiff` path ([`geotiff_work`]). It draws a copy of the map, so
/// the map can move meanwhile; a cancelled export stops waiting for tiles and writes nothing.
pub(crate) fn map_work(s: &Session, p: &Value) -> Result<Work, String> {
    let path = ps(p, "path").ok_or("path is required")?.to_string();
    let (width, height, dpi) = (f(p, "width"), f(p, "height"), f(p, "dpi"));
    let ext = Path::new(&path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let cog = match p.get("cog") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or("cog must be true or false")?,
    };
    let tif = matches!(ext.as_str(), "tif" | "tiff");
    let fit = match p.get("fit") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or("fit must be true or false")?,
    };
    let mut sized = size_notes(&path, s.view_size, width, height);
    sized.extend(dpi_note(&path, dpi));
    // `fit`: a view too large for the DPI asked is drawn at the most that fits, and says so.
    let asked = dpi.filter(|d| d.is_finite()).unwrap_or(96.0).clamp(DPI_RANGE.0, DPI_RANGE.1);
    let dpi = match fit.then(|| fitted_dpi(&export_view(s.map().camera, s.view_size, width, height, Some(asked)), asked, tif)).flatten() {
        Some(lower) => {
            sized.push((
                rustgis_data::diagnostics::Severity::Info,
                format!("{path}: drawn at {lower} DPI, the most an export of this view holds ({} pixels a side)", rustgis_render::MAX_SIDE),
            ));
            Some(lower)
        }
        None => dpi,
    };
    if tif {
        return geotiff_work(s, path, export_view(s.map().camera, s.view_size, width, height, dpi), cog, sized);
    }
    if cog {
        return Err("cog is for a GeoTIFF (.tif or .tiff) export".into());
    }
    let (mut map, view_size) = (s.map().detached(), s.view_size);
    Ok(Box::new(move || {
        let written = (|| {
            let png = png_of(&mut map, view_size, width, height, dpi)?;
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            rustgis_data::io::write_atomic(Path::new(&path), &png).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!({"path": path, "bytes": png.len()}))
        })();
        // What the size limit did is said of a file written.
        let sized = if written.is_ok() { sized } else { Vec::new() };
        super::exports::drawn(std::slice::from_ref(&map), sized, written, path)
    }))
}

/// `share.exportMap` to a GeoTIFF: the view drawn as data (its layers and labels, without the
/// scale bar and credits, which sit on the screen), RGB (a drawn view is opaque), placed on the
/// ground in the map's projection, the credits in its Copyright tag. What may have left gaps in
/// it (tiles still on their way, tile layers that failed) is said: on the ground a gap looks like
/// data. A turned view is drawn north up over the box around it, and says so; a tilted view, the
/// globe and a map of another body are refused, as no grid of square pixels in one coordinate
/// system holds them.
fn geotiff_work(
    s: &Session,
    path: String,
    view: View,
    cog: bool,
    sized: Vec<(rustgis_data::diagnostics::Severity, String)>,
) -> Result<Work, String> {
    let m = s.map();
    let crs = map_crs(m)?;
    if view.camera.pitch.abs() > 1e-6 {
        return Err(format!(
            "the map is tilted {:.0}°: a GeoTIFF needs it looking straight down (pitch 0), or export a PNG",
            view.camera.pitch
        ));
    }
    let mut view = view;
    let mut notices = Vec::new();
    let bearing = view.camera.bearing;
    let turned = is_turned(bearing);
    if turned {
        // North up over the box around the turned view: everything it shows, and more at the
        // corners.
        // In points whose pixels, once rounded, still cover the box.
        let (bw, bh) = view.camera.rotated_size(view.width, view.height);
        let ppp = view.pixels_per_point;
        view.width = (bw * ppp).ceil() / ppp;
        view.height = (bh * ppp).ceil() / ppp;
        view.camera.bearing = 0.0;
        notices.push((
            rustgis_data::diagnostics::Severity::Info,
            format!("{path}: the map is turned {bearing:.0}°, so the GeoTIFF is drawn north up over the box around the view"),
        ));
    }
    // A turned view's box is larger than the view: said, as the fix differs.
    fits_pixels(&view, turned)?;
    let name = m.name.clone();
    let mut map = m.detached();
    Ok(Box::new(move || {
        let mut notices = notices;
        let written = (|| {
            let placed = rustgis_render::render_placed(&mut map, &view)?;
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            for gap in &placed.gaps {
                notices.push((rustgis_data::diagnostics::Severity::Warning, format!("{path}: {gap}")));
            }
            let credits = placed.credits;
            // The drawn picture is dropped once its bands are made, before the raster's
            // statistics and the file's encoding.
            let mut r = placed_raster(&name, placed.pixmap, &view, crs)?;
            r.copyright = (!credits.is_empty()).then(|| credits.join(" | "));
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            let (bytes, tiled) = if cog {
                // Tiles go to the file as they are made; its tiling is in the answer, as Export
                // Raster gives it.
                let c = rustgis_data::raster::cogwrite::write_file(&r, 0, Path::new(&path)).map_err(|e| format!("{path}: {e}"))?;
                (c.bytes, Some(json!({"overviews": c.levels.len().saturating_sub(1), "bigTiff": c.big})))
            } else {
                let bytes = rustgis_data::raster::geotiff::write(&r, 0).map_err(|e| format!("{path}: {e}"))?;
                rustgis_data::io::write_atomic(Path::new(&path), &bytes).map_err(|e| format!("{path}: {e}"))?;
                (bytes.len() as u64, None)
            };
            let mut out = json!({"path": path, "width": r.width, "height": r.height, "crs": crs.label(), "bytes": bytes});
            if let Some(t) = tiled {
                out["cog"] = t;
            }
            if let Some(c) = &r.copyright {
                out["copyright"] = json!(c);
                if let Some(note) = rustgis_data::raster::tag_text_note(c) {
                    // Quoted (control characters escaped, so a service's credits can't reach a
                    // terminal raw) and cut short: the answer holds them whole.
                    let shown: String = c.chars().take(CREDITS_SHOWN).collect();
                    let more = if c.chars().count() > CREDITS_SHOWN { "…" } else { "" };
                    notices.push((rustgis_data::diagnostics::Severity::Info, format!("{path}: {note} (the credits: {shown:?}{more})")));
                }
            }
            Ok(out)
        })();
        // What the size limit did is said of a file written, first.
        if written.is_ok() {
            notices.splice(0..0, sized);
        }
        super::exports::drawn(std::slice::from_ref(&map), notices, written, path)
    }))
}

/// The coordinate system a GeoTIFF of map `m`'s view is in: its flat projection's.
fn map_crs(m: &rustgis_map::Map) -> Result<Crs, String> {
    let body = m.body();
    if !body.is_earth() {
        return Err(format!("a map of {} can't be saved as a GeoTIFF yet: export a PNG", body.name));
    }
    match &m.projection {
        rustgis_map::Projection::Globe => {
            Err("the globe can't be saved as a GeoTIFF: switch to a flat projection (View › Projection), or export a PNG".into())
        }
        rustgis_map::Projection::WebMercator => Ok(Crs::WebMercator),
        rustgis_map::Projection::Custom(code) => {
            let (proj4, label) = rustgis_map::projection::lookup(code)?;
            // An EPSG system is written by its code; any other by its definition.
            if let Some(n) = label.strip_prefix("EPSG:").and_then(|n| n.parse::<u32>().ok())
                && let Some(crs) = Crs::from_epsg(n)
            {
                return Ok(crs);
            }
            let shown = label.clone();
            let def = rustgis_data::raster::crs::CustomCrs { proj4, label, geokeys: Vec::new(), doubles: Vec::new(), ascii: String::new() };
            let crs = Crs::custom(def)?;
            if !rustgis_data::raster::geotiff::describes(crs) {
                return Err(format!(
                    "the map's projection, {shown}, can't be described in a GeoTIFF: switch the map to an EPSG projection, or export a PNG"
                ));
            }
            Ok(crs)
        }
    }
}

/// The drawn view `pm` as an RGB raster placed on the ground: the view's centre at the image's
/// centre, each pixel `res / pixels_per_point` map units a side. A drawn view is opaque (its
/// background is filled), so it has no alpha band. While the bands are filled the picture is
/// held too, 16 bytes a pixel in all (about 1 GiB at 8192 a side); a machine that can't hold
/// them gets an error, not a crash.
fn placed_raster(name: &str, pm: tiny_skia::Pixmap, view: &View, crs: Crs) -> Result<rustgis_data::raster::Raster, String> {
    let (w, h) = (pm.width(), pm.height());
    let mut bands: Vec<Vec<f32>> = (0..3)
        .map(|_| rustgis_data::raster::grid::<f32>(w as usize, h as usize))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{name}: {e}"))?;
    for px in pm.pixels() {
        // tiny-skia keeps colours premultiplied; a file keeps them straight.
        let c = px.demultiply();
        for (band, v) in bands.iter_mut().zip([c.red(), c.green(), c.blue()]) {
            band.push(f32::from(v));
        }
    }
    drop(pm);
    let cam = view.camera;
    let cell = cam.res / view.pixels_per_point;
    // The renderer puts the centre at half the view's width in points, times the pixels per
    // point, unrounded: the corner is half the view from it, whatever whole number of pixels the
    // picture is.
    let transform = rustgis_data::raster::GeoTransform {
        origin_x: cam.center.x - view.width / 2.0 * cam.res,
        origin_y: cam.center.y + view.height / 2.0 * cam.res,
        pixel_width: cell,
        pixel_height: -cell,
    };
    let color = rustgis_data::raster::ColorHint::Rgb { alpha: false };
    rustgis_data::raster::Raster::new(name, w, h, bands, transform, crs, None, color, "UInt8").map_err(|e| e.to_string())
}

/// `share.exportLayer`: writes a layer to a file by its extension. A streamed layer exports its
/// whole source, a chunk at a time where the source and format allow (see `data::export`). Says
/// what the file couldn't hold: features without drawable geometry, a missing `.prj`, Z or M
/// values dropped on import, positions that are approximate or aren't longitude/latitude, field
/// names a Shapefile cuts.
pub(crate) fn layer_work(s: &Session, p: &Value) -> Result<Work, String> {
    let id = s.layer_id(p)?;
    let path = ps(p, "path").ok_or("path is required")?.to_string();
    // The layer as it is now: its features are shared, and a streamed one's source is read on the
    // export's thread.
    let l = s.map().layer(id).ok_or("no layer")?.clone();
    let asked = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty()).map(Crs::parse).transpose()?;
    // Checked now, so a bad table option is refused before the export starts.
    let ext = Path::new(&path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let gpkg = gpkg_table(p, &ext)?;
    let compatibility =
        ps(p, "crs").and_then(|text| asked.and_then(|crs| rustgis_data::raster::wkt::projection_compatibility_note(text, crs)));
    Ok(Box::new(move || {
        let mut wrote = write_layer(&l, &path, asked, gpkg)?;
        if let Some(note) = compatibility {
            wrote.notes.insert(0, format!("crs: {note}"));
        }
        Ok(wrote)
    }))
}

/// Writes layer `l` to `path`, in `asked` or the system the format defaults to; a GeoPackage's
/// table goes where `gpkg` says.
fn write_layer(l: &rustgis_map::Layer, path: &str, asked: Option<Crs>, gpkg: GpkgTable) -> Result<Wrote, String> {
    let ext = Path::new(&path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    // A layer read in place from the GeoPackage it goes into is read whole first: SQLite can't
    // write a file while the same export is reading it.
    let from_target = gpkg.mode != GpkgMode::NewFile
        && l.source.as_ref().and_then(|src| src.path.as_deref()).is_some_and(|src| same_file(Path::new(src), Path::new(&path)));
    // A streamed layer exports its whole source, not the features read for the view: a chunk at
    // a time where the source and format allow, so its size isn't limited by memory.
    let chunks = if l.is_streamed() && rustgis_data::export::streams(&ext) && !from_target { l.chunk_export()? } else { None };
    let (whole, unread) = if l.is_streamed() && chunks.is_none() { l.load_all_counted().map(|(ds, n)| (Some(ds), n))? } else { (None, 0) };
    let ds = chunks.as_ref().map(|(_, layer)| layer).or(whole.as_ref()).unwrap_or(&l.dataset);
    // GeoJSON, KML and GPX are longitude/latitude; the others keep the layer's own system unless
    // told another.
    let crs = asked.unwrap_or_else(|| rustgis_data::io::default_export_crs(ds, &ext));
    // Another body's layer projected onto another ellipsoid would be wrong data, not a wrong name.
    if let Some(e) = rustgis_data::export_error(ds, crs) {
        return Err(format!("{path}: {e}"));
    }
    let parquet = matches!(ext.as_str(), "parquet" | "geoparquet");
    let (written, left_out, skipped, table, unfit) = match &chunks {
        Some((src, layer)) => {
            let (e, table) =
                rustgis_data::export::write_source_to(src.as_ref(), layer, Path::new(&path), crs, &gpkg).map_err(|e| e.to_string())?;
            // Counted as the chunks were written, as the layer holds none of them.
            let unfit = rustgis_data::source::parquet::unfit_note(&layer.fields, &e.unfit, path);
            (e.written, e.left_out, e.skipped, table, unfit)
        }
        None => {
            let (left_out, table) = rustgis_data::io::write_path_to(ds, Path::new(&path), crs, &gpkg).map_err(|e| e.to_string())?;
            let unfit = if parquet { rustgis_data::source::parquet::values_note(ds, path) } else { None };
            (ds.features.len().saturating_sub(left_out), left_out, unread, table, unfit)
        }
    };
    // A streamed layer's extent is its whole source's.
    let notes = rustgis_data::export_notes([(ds, l.extent())], path, crs);
    // A Shapefile (`.shp`, or zipped to `.zip`) cuts field names to dBase's.
    let shapefile = matches!(ext.as_str(), "shp" | "zip");
    let mut dbf_notes = Vec::new();
    if shapefile {
        dbf_notes.extend(rustgis_data::io::shapefile_names_note(&ds.fields, path));
        dbf_notes.extend(rustgis_data::io::shapefile_values_notes(ds, path));
    }
    // FlatGeobuf, KML, GPX and Shapefile hold features with geometry only (a Shapefile, only
    // geometry it can draw).
    let mut said = Vec::new();
    if left_out > 0 {
        said.push(format!("{path}: {left_out} features without drawable geometry were left out"));
    }
    if skipped > 0 {
        said.push(format!("{path}: {skipped} rows couldn't be read and were left out"));
    }
    said.extend(notes);
    said.extend(dbf_notes);
    said.extend(unfit);
    if shapefile && rustgis_data::raster::wkt::to_wkt(crs).is_none() {
        said.push(format!("{path}: no .prj written, as {} has no WKT form", crs.label()));
    }
    said.push(format!("Exported {path}"));
    let answer = json!({"path": path, "features": written, "leftOut": left_out, "crs": crs.label()});
    // The table written, said and put in the answer once the export lands, where the session
    // (layers reading a replaced table) is at hand.
    let written_path = Some(path.to_string());
    let path = path.to_string();
    let default = ds.name.clone();
    let then: Option<super::exports::Then> = table.map(|t| {
        Box::new(move |s: &mut Session, out: &mut Value| {
            report_table(s, &path, &gpkg, &default, Some(t), out);
            Ok(())
        }) as super::exports::Then
    });
    Ok(Wrote {
        answer,
        notes: said,
        written_path,
        then,
        #[cfg(not(target_arch = "wasm32"))]
        owner: None,
    })
}

/// Whether `a` and `b` are the same existing file.
fn same_file(a: &Path, b: &Path) -> bool {
    matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}
