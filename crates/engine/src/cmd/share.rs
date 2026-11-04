//! Publish tab: export the map as PNG and layers as GeoJSON, CSV, FlatGeobuf, GeoPackage,
//! GeoParquet, Shapefile, KML/KMZ or GPX.

use std::path::Path;

use rustgis_data::io::{GpkgMode, GpkgTable, GpkgWritten};
use rustgis_data::raster::Crs;
use rustgis_render::View;
use serde_json::{Value, json};

use super::exports::{Work, Wrote};
use super::{f, need_layer, ps};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("share.exportMap", "Export Map", "Publish › Output", export_map)
            .view()
            .params(r#"{"path": "*.png", "width"?: px, "height"?: px, "dpi"?: 96..600}"#),
        C::new("share.exportLayer", "Export Features", "Publish › Output", |s, p| super::exports::write_here(s, p, "share.exportLayer"))
            .view()
            .when(need_layer)
            .params(r#"{"layer"?, "path": "*.geojson|*.csv|*.fgb|*.gpkg|*.parquet|*.shp|*.zip|*.kml|*.kmz|*.gpx", "crs"?: "EPSG:… (or WKT) for a GeoPackage, Shapefile or FlatGeobuf (default: the system the layer was read in, else EPSG:4326)", "mode"?: "newFile (default: refuses an existing file) | addTable | replaceTable, for a GeoPackage", "tableName"?: "the GeoPackage table (default: the layer's name)"}"#),
        C::new("share.exportTable", "Export Table", "Publish › Output", |s, p| {
            let path = ps(p, "path").ok_or("path is required")?;
            let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            let gpkg = gpkg_table(p, &ext)?;
            let t = s.project.tables.get(s.table_index(p)?).ok_or("no such table")?;
            // Tables may retain geometry and its import losses, just as feature layers do.
            let notes = rustgis_data::export_notes([(&t.dataset, t.dataset.extent())], path, Crs::LonLat);
            let mut dbf_notes = Vec::new();
            if matches!(ext.as_str(), "shp" | "zip") {
                dbf_notes.extend(rustgis_data::io::shapefile_names_note(&t.dataset.fields, path));
                dbf_notes.extend(rustgis_data::io::shapefile_values_notes(&t.dataset, path));
            }
            let (left_out, table) =
                rustgis_data::io::write_path_to(&t.dataset, Path::new(path), Crs::LonLat, &gpkg).map_err(|e| e.to_string())?;
            if left_out > 0 {
                s.notify(format!("{path}: {left_out} features without drawable geometry were left out"), false);
            }
            for note in notes.into_iter().chain(dbf_notes) {
                s.notify(note, false);
            }
            let mut out = json!({"path": path});
            report_table(s, path, &gpkg, table, &mut out);
            Ok(out)
        })
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
fn report_table(s: &mut Session, path: &str, gpkg: &GpkgTable, table: Option<GpkgWritten>, out: &mut Value) {
    let Some(table) = table else { return };
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

/// Renders the active map at the current view (or a requested size) to PNG bytes.
pub fn map_png(s: &mut Session, width: Option<f64>, height: Option<f64>, dpi: Option<f64>) -> Result<Vec<u8>, String> {
    let (vw, vh) = s.view_size;
    // NaN would pass through `clamp`; non-finite values fall back to the defaults.
    let fin = |v: Option<f64>| v.filter(|x| x.is_finite());
    let (width, height, dpi) = (fin(width), fin(height), fin(dpi));
    let w = width.unwrap_or(vw).clamp(16.0, 8000.0);
    let h = height.unwrap_or(vh).clamp(16.0, 8000.0);
    let ppp = (dpi.unwrap_or(96.0) / 96.0).clamp(0.5, 6.0);
    let mut camera = s.map().camera;
    // Keep the same ground extent when the size differs from the view.
    if width.is_some() || height.is_some() {
        camera.res *= (vw / w).max(vh / h);
        camera.clamp();
    }
    let view = View { width: w, height: h, pixels_per_point: ppp, camera, sky: false };
    rustgis_render::render_png(s.map_mut(), &view)
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
        super::layout::pictured_notices(s, &[], &[1]);
        let png = rustgis_render::render_png(s.map_mut(), &view)?;
        let pm = rustgis_render::decode_png(&png).ok_or("could not draw the map")?;
        rustgis_render::pdf::Page::new(&pm, w, h)
    } else {
        rustgis_render::pdf::Page::drawing(drawing, w, h)
    };
    Ok(rustgis_render::pdf::write(&[page], &s.map().name))
}

fn export_map(s: &mut Session, p: &Value) -> CmdResult {
    let path = ps(p, "path").ok_or("path is required")?.to_string();
    // A style added with its own view: the export waits for it to load, as it does for tiles.
    super::tiles::follow_style_view(s, web_time::Duration::from_secs(30));
    let png = map_png(s, f(p, "width"), f(p, "height"), f(p, "dpi"))?;
    rustgis_data::io::write_atomic(std::path::Path::new(&path), &png).map_err(|e| format!("{path}: {e}"))?;
    s.notify(format!("Exported {path}"), false);
    Ok(json!({"path": path, "bytes": png.len()}))
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
    Ok(Box::new(move || write_layer(&l, &path, asked, gpkg)))
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
    let (written, left_out, skipped, table) = match &chunks {
        Some((src, layer)) => {
            let (e, table) =
                rustgis_data::export::write_source_to(src.as_ref(), layer, Path::new(&path), crs, &gpkg).map_err(|e| e.to_string())?;
            (e.written, e.left_out, e.skipped, table)
        }
        None => {
            let (left_out, table) = rustgis_data::io::write_path_to(ds, Path::new(&path), crs, &gpkg).map_err(|e| e.to_string())?;
            (ds.features.len().saturating_sub(left_out), left_out, unread, table)
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
    if shapefile && rustgis_data::raster::wkt::to_wkt(crs).is_none() {
        said.push(format!("{path}: no .prj written, as {} has no WKT form", crs.label()));
    }
    said.push(format!("Exported {path}"));
    let answer = json!({"path": path, "features": written, "leftOut": left_out, "crs": crs.label()});
    // The table written, said and put in the answer once the export lands, where the session
    // (layers reading a replaced table) is at hand.
    let path = path.to_string();
    let then: Option<super::exports::Then> = table
        .map(|t| Box::new(move |s: &mut Session, out: &mut Value| report_table(s, &path, &gpkg, Some(t), out)) as super::exports::Then);
    Ok(Wrote { answer, notes: said, then })
}

/// Whether `a` and `b` are the same existing file.
fn same_file(a: &Path, b: &Path) -> bool {
    matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}
