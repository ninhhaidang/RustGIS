//! Home › Add (Add Data), Layers pane actions, and the Feature Layer / Labeling / Data tabs.

use std::path::Path;

use rustgis_data::{Dataset, GeometryType, photo};
use rustgis_map::symbology::{ClassRef, ClassStyle, LABEL_SIZE, MAX_STROKES};
use rustgis_map::{
    ClassMethod, Glyph, HiddenFills, LineCap, LineJoin, LineMark, LineMarkers, LineStroke, Marker, NoFill, Picture, Placements, Ramp,
    Renderer, Rgba,
};
use serde_json::{Value, json};

use super::{b, f, need_feature_layer, need_layer, ps, uint};
use crate::{CmdResult, CommandSpec as C, Session};

/// The most characters a layer's name has (`layer.rename`, once trimmed): a longer one is refused,
/// not cut. The Layer Properties pane's name box takes no more.
pub const MAX_LAYER_NAME: usize = 200;

/// Registers layer commands shared by the UI, CLI and command search.
pub fn commands() -> Vec<C> {
    vec![
        C::new("layer.addData", "Add Data", "Home › Add", add_data).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .params(r#"{"path": string, "name"?: string, "layer"?: "GeoPackage, GDAL, DuckDB or DXF layer, or a PDF's page number (the first when left out or blank)", "paperSpace"?: "bool: a DXF's layout entities too", "blocks"?: "bool: a DXF's block (图块) contents, true by default", "stream"?: "bool: true links the file instead of copying it (read in place, a view at a time; the project keeps its path, not its features), false copies it into the project; left out, decided by its size or feature count", "as"?: "scan" (a TIFF, PNG, JPEG or PDF page, even a photo, a GeoTIFF or a GeoPDF, as a picture with no coordinates), "crs"?: "EPSG:… (or WKT) for Shapefile coordinates (overrides .prj), a CSV, a DXF (which never says), a CityJSON or CityGML model (it replaces the system a model file names; in a zip, it is used for a model naming none RustGIS knows), or a GDAL or DuckDB file that doesn't say (GDAL takes an authority code such as EPSG:2263 or the exact names WGS84, WGS72, NAD27, NAD83, not WKT or a file)", "x"?: "CSV x/longitude column", "y"?: "CSV y/latitude column", "wkt"?: "CSV WKT geometry column", "delimiter"?: ","|";"|"\\t"|"|"}"#),
        C::new("layer.addPhotos", "Add Photos", "Home › Add", add_photos).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).params(
            r#"{"paths"?: [JPEG photo files], "folder"?: "a folder of photos (every JPEG in it)", "name"?: "the layer (default Photos; photos join it when it exists)"}"#,
        ),
        C::new("info.fileLayers", "List Layers in File", "Home › Add › Add Data", file_layers).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"path": string, "crs"?: "EPSG:… for a CityJSON model (.city.json, a .json, or in a zip) or a CityGML model (.gml, .xml, or in a zip): it replaces the system a model file names; in a zip, it is used for a model naming none RustGIS knows (as layer.addData)", "link"?: "bool: for a file Add Data can link instead of copying (GeoPackage, GeoParquet, FlatGeobuf, Shapefile, .geojson; desktop), also say how it would take it, as link: linked (what it does without stream; for a .geojson, by its size alone, so Add Data sends the answer, as layer.addData unasked also looks at its count) and copy (whether it can be read whole); it opens each table, so only when asked"}"#),
        C::new("info.fileColumns", "List Columns in File", "Home › Add › Add Data › Delimited Text", file_columns).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).view().params(
            r#"{"path": "a delimited text file (CSV, TSV, TXT, DAT, or gzip-compressed text) or a spreadsheet", "delimiter"?: ","|";"|"\\t"|"|", "layer"?: "a spreadsheet's sheet (the first by default)", "crs"?: "EPSG:… (or WKT), as layer.addData: a projected system keeps the columns found by name"}"#,
        ),
        C::new("layer.addSql", "Add Data From SQL", "Home › Add", add_sql)
            .roles(&[
                crate::roles::ParamRole { key: "sql", role: crate::roles::Role::Sql },
                crate::roles::ParamRole { key: "query", role: crate::roles::Role::Sql },
            ])
            .effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files).with(crate::policy::Effect::Network))
            .params(
                r#"{"sql": "one DuckDB SELECT; DuckDB Spatial's ST_ functions, read_parquet('…'), web addresses", "name"?: string, "database"?: "a .duckdb file or a PostgreSQL connection (postgres:host=… dbname=…) whose tables the query names", "password"?: "the connection's, kept for this session only", "crs"?: "EPSG:… of a geometry that doesn't say", "stream"?: bool, "ran"?: "a result the app ran (the SQL pane)"}"#,
            ),
        C::new("layer.addDatabase", "Add Data From Database", "Home › Add › Add Data", add_database).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network).with(crate::policy::Effect::Files)).params(
            r#"{"database": "postgres:host=… dbname=… user=… (or a postgresql:// address; PostGIS tables are read live), a mysql://user@host:port/database address (MySQL or MariaDB, read live; mariadb:// too), an mssql://user@host:1433/database address (SQL Server or Azure SQL, read live; ?encrypt=strict|false, ?trust-server-certificate=true for a self-signed certificate, ?instance=NAME; on Windows DOMAIN\\user or no user signs in with Windows), or a .duckdb file", "password"?: "kept for this session only, never saved", "layer"?: "a table, as table or schema.table (table:column for one of several geometry columns; default: every table with a geometry column). On a SQL Server enterprise geodatabase, a feature class by name, alias-less short name, schema.table or Pro's db.schema.table, and a registered non-spatial table becomes a standalone table; without layer every class is added and registered tables are not", "crs"?: "EPSG:… of a geometry that doesn't say", "name"?: string, "stream"?: bool}"#,
        ),
        C::new("info.databaseTables", "List Database Tables", "Home › Add › Add Data › Database", database_tables).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network).with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"database": "as layer.addDatabase", "password"?: string} → {count, layers: [labels of the tables with a geometry column], and for an Esri enterprise geodatabase on SQL Server: tables: [labels of its registered tables without one], items: [{label, item: "featureClass"|"table"|"other", schema, name, column?, kind?, dataset, alias, versioning: "none"|"traditional"|"branch", display, geometry?: "points"|"lines"|"polygons"|null (a featureClass's; other items have none)}], geodatabase: {owner, release, note? (when its catalog or its list of spatial columns was cut short)}}"#),
        C::new("layer.defineProjection", "Define Projection", "Layers", define_projection).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_feature_layer)
            .params(r#"{"layer"?: name|id, "crs": "EPSG:… or WKT; reinterpret native coordinates, without rewriting the source file"}"#),
        C::new("layer.loadAll", "Load All Features", "Layers", load_all).roles(&[]).effects(crate::policy::Effects::NONE).when(need_streamed).params(r#"{"layer"?: name|id}"#),
        C::new("layer.addUrl", "Add Data From URL", "Home › Add › Add Data", add_url).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"url": "https://…/data.geojson|.csv|.parquet|.fgb|.gpkg|.zip|.shp (its .dbf fetched beside it, and its .shx, .prj and .cpg when there)|.kml|.kmz|.gpx|.tif|.grb|.grib|.grb2|.grib2 (GRIB)|.jp2 (JPEG 2000: desktop, through GDAL) (the format from the address, else the contents) | a .pmtiles archive", "bytes"?: "[first, last]: only these octets of a GRIB file (counted from 0, both included, as HTTP's Range and curl -r write them; at most 64 MB), read as the file (desktop and command line): a record found in its wgrib inventory", "format"?: "grib: with bytes, the address is a GRIB file whatever its name says", "name"?: text, "layer"?: "one layer of a zip, KML or GPX", "zoom"?: "bool: true frames everything added; false keeps the view, even on an empty map; omitted, the first content on an empty map is framed (a GeoParquet, FlatGeobuf or cloud-optimised GeoTIFF read in place likewise; a PMTiles or MBTiles archive ignores zoom and is framed on its bounds unless it covers the whole world)", "x"?, "y"?, "wkt"?, "crs"?, "delimiter"?, "paperSpace"?, "blocks"?, "stream"?, "as"? (as layer.addData), "whole"?: "bool: download a cloud-optimised GeoTIFF whole (as a raster layer) instead of reading it in place (an address without a file name is read in place when its first bytes are a cloud-optimised GeoTIFF)", "cog"?: "bool: the address is a cloud-optimised GeoTIFF whatever its name says (the app's probe found it so)", "bbox"?: "[west, south, east, north] in degrees | "view": a cloud-optimised GeoTIFF's window as a raster layer (desktop and command line), read by range at full resolution, in the file's coordinate system, with its colour table and class names; at most 64 million values (all bands)", "cellSize"?: "with bbox: the coarsest overview whose cells are at most this size is read (the file's units; metres for longitude/latitude, a degree taken as 111.32 km)", "cube"?: "bool: a data cube's file: a tile layer that follows the time slider (stac.addCube adds them), kept one when the project opens again", "start"?, "end"?, "unit"?, "interval"?, "dates"?: "a GeoTIFF address with date placeholders ({date:YYYY}, {YYYY}{MM}{DD}), one file per date: the dates it spans and steps through (as time.set), turning the time slider on", "connection"?: "the id of the saved web-service connection it comes from (as service.add)", "service"?: "bool: a web service's features (an ArcGIS query, OGC API Features items): read in pages as service.add reads them, known by the address as its layers are, and an ArcGIS layer drawn as its description says", "bounds"?: "with service: [west, south, east, north], the layer's longitude/latitude bounds its axes are checked against", "from"?: "the web document address that named url (internal: enforces public-address reach)", "fromDocument"?: bool, "anonymous"?: bool (internal: a plugin-origin download must never adopt a saved login), "coverage"?: bool (a WCS coverage's GetCoverage address, service.coverage's: a GeoTIFF, else the service's error; one that doesn't say its system is read in the one the address asks for), "coverageCrs"?: "EPSG:<code> (with coverage: the system it was asked in, service.coverage's coverageCrs, when the address doesn't say it)", "request"?: "number (internal: the download a plugin command left to the app, #1202)"}"#,
        ),
        C::new("layer.addDownload", "Add Downloaded Data", "Home › Add", add_download).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved).with(crate::policy::Effect::Files))
            .params(r#"{"id": a download the app finished (Home › Add › Add Data › From URL), "url": its address, "name"?: text, "layer"?: text, "zoom"?: bool, "x"?, "y"?, "wkt"?, "crs"?, "delimiter"?, "paperSpace"?, "blocks"?, "stream"?, "as"? (as layer.addData), "service"?: bool (a service's features: their axes (when they came as GeoJSON) and paging are checked, and the address is kept as the layer's source), "bounds"?: [west, south, east, north] (with service: the layer's longitude/latitude bounds from its capabilities, which its axes are checked against; without bounds they are left as they come; ignored without service), "coverage"?: bool (a WCS coverage: a GeoTIFF, else the service's error, and its request kept as the layer's source), "coverageCrs"?: "EPSG:<code>" (internal, with coverage: the system service.coverage's request asks it in (its coverageCrs), for an answer that doesn't say its own, when the address doesn't say it), "drawingInfo"?: object (internal, with service: an ArcGIS layer's drawingInfo from its description, which the layer is drawn with), "geolibreProject"?: bool (internal: downloaded project JSON), "geolibreLayer"?: {"token", "record"} (internal: reserved imported layer and its saved style), "geolibreZarr"?: bool (internal: an imported Zarr slice as GeoTIFF), "connection"?: "as layer.addUrl", "anonymous"?: bool (internal: preserves a plugin-origin download's anonymous reads), "request"?: number (internal: the download a plugin command left to the app, #1202), "window"?: {"bbox", "cellSize"?, "cog"?} (internal: what a cloud-optimised GeoTIFF's window the app read off the frame asked for, for a task to read it again, #1330)}"#),
        C::new("layer.addEncodedPolyline", "Add Encoded Polyline", "Home › Add", add_encoded_polyline).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"polyline": "encoded polyline text" | [texts] (one line each), "precision"?: 5 (Google) | 6 (OSRM, Valhalla), "name"?: text}"#,
        ),
        C::new("layer.addDataText", "Add Data (from text)", "Home › Add", add_data_text).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"fileName": "name.geojson|.csv|.tsv|.city.json|.gml|.dxf (CityJSON and CityGML: all their layers)", "text": string, "name"?: text, "x"?, "y"?, "wkt"?, "crs"?, "delimiter"? (as layer.addData, for delimited text; crs also for a CityJSON or CityGML model, and for a DXF), "layer"?, "paperSpace"?, "blocks"? (as layer.addData, for a DXF)}"#),
        C::new("layer.addSample", "Add Sample Data", "Home › Add › Add Data", add_sample).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"name": "countries"|"cities"|"rivers"|"lakes"}"#),
        C::new("layer.remove", "Remove", "Layers", remove).roles(&[]).effects(crate::policy::Effects::NONE)
            .key("Delete")
            .when(need_layer)
            .params(r#"{"layer"?: name|id | [name|id] (default: the layer selection)}"#),
        C::new("layer.select", "Select Layer", "Layers", select).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"layer"?: name|id | [name|id] (default: the active layer; the last of a list becomes the active layer), "mode"?: "replace" (only these; the default) | "toggle" (each in or out of the layer selection; the last one out leaves no active layer) | "extend" (added to it)}"#)
            .view(),
        C::new("layer.rename", "Rename", "Layers", |s, p| {
            let n = s_req(p, "name")?.trim().to_string();
            if n.is_empty() {
                return Err("name is required".into());
            }
            // Refused rather than cut, so the name a script or the Rename… dialog gives is never
            // changed without a word (#1817).
            let chars = n.chars().count();
            if chars > MAX_LAYER_NAME {
                return Err(format!("name is {chars} characters: a layer's name takes at most {MAX_LAYER_NAME}"));
            }
            let id = s.layer_id(p)?;
            let old = s.map().layer(id).map(|l| l.name.clone()).unwrap_or_default();
            // The terrain keeps its DEM by name: did it name this layer, and was the name its own?
            let on_old = terrain_dem(s).is_some_and(|d| d.eq_ignore_ascii_case(&old));
            let old_was_shared = named_layers(s, &old) > 1;
            let r = with(s, p, |l| {
                l.name = n.clone();
                l.dataset.name = n.clone();
            })?;
            // A rename to the same name (case aside) changes nothing the terrain reads.
            if let Some(dem) = terrain_dem(s).filter(|_| !n.eq_ignore_ascii_case(&old)) {
                if on_old && old_was_shared {
                    // Two layers had the DEM's name: which one the terrain was on can't be told.
                    s.map_mut().view.terrain = None;
                    s.notify(format!("Terrain turned off: its name '{dem}' was shared by two layers, so which one gave the heights can't be told; turn Terrain on again from the layer you want"), false);
                } else if on_old && named_layers(s, &n) == 1 {
                    // The DEM itself, renamed to a name of its own: the terrain follows it.
                    set_terrain_dem(s, n.clone());
                } else if on_old || n.eq_ignore_ascii_case(&dem) {
                    // The DEM renamed onto another layer's name, or another layer given the DEM's:
                    // the first layer of that name would give the heights.
                    let shared = if on_old { n.clone() } else { dem };
                    let said = if named_layers(s, &shared) > 1 {
                        format!("Terrain turned off: two layers are now called '{shared}'; rename one and turn Terrain on again")
                    } else {
                        // The DEM was gone (removed), and this layer took its name.
                        format!("Terrain turned off: its DEM '{shared}' was missing, and a layer now has that name; turn Terrain on again to use it")
                    };
                    s.map_mut().view.terrain = None;
                    s.notify(said, false);
                }
            }
            // So do legends that leave it out.
            legends_follow_layer(s, &old, Some(&n));
            references_follow_layer(s, &old, &n);
            Ok(r)
        }).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?, "name": string (at most 200 characters once trimmed; a longer one is refused)}"#)
        .when(need_layer),
        C::new("layer.visibility", "Visibility", "Layers", |s, p| {
            let ids = layer_ids(s, p)?;
            // Toggled together: every layer takes the opposite of the active layer's state (or the
            // first one's), not each its own, which would swap shown and hidden ones.
            let on = b(p, "visible").unwrap_or_else(|| {
                let lead = s.active_layer.filter(|a| ids.contains(a)).or(ids.first().copied());
                !lead.and_then(|id| s.map().layer(id)).is_some_and(|l| l.visible)
            });
            for id in ids {
                if let Some(l) = s.map_mut().layer_mut(id) {
                    l.visible = on;
                }
            }
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?: name|id | [name|id] (default: the layer selection), "visible"?: bool (toggles when omitted: all take the opposite of the active layer's state)}"#)
        .when(need_layer),
        C::new("layer.moveUp", "Move Up", "Layers", |s, p| move_layer(s, p, -1)).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?: name|id (default: the active layer)}"#).when(need_layer),
        C::new("layer.moveDown", "Move Down", "Layers", |s, p| move_layer(s, p, 1)).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?: name|id (default: the active layer)}"#).when(need_layer),
        C::new("layer.move", "Move", "Layers", move_to).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_layer)
            .params(r#"{"layer"?: name|id | [name|id] (default: the layer selection), "to": position (0 = top; past the end, the bottom). Several keep their order, the topmost landing at "to" as one layer would, and go only to the top or the bottom (one in a group, to its group's): with 4 layers and 2 of them, 2 or more is the bottom, 1 is refused}"#),
        C::new("layer.zoomTo", "Zoom To Layer", "Layers", |s, p| {
            let ids = layer_ids(s, p)?;
            if s.zoom_to_layers(&ids, rustgis_map::ExtentOf::Shown).is_none() {
                return Err(if ids.len() > 1 { "the layers have no features" } else { "the layer has no features" }.into());
            }
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?: name|id | [name|id] (default: the layer selection; zooms to all of them)}"#)
        .view()
        .when(need_layer),
        C::new("layer.symbology", "Symbology", "Style › Drawing", symbology).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_feature_layer)
            .params(r#"{"layer"?, "type": "single"|"unique"|"graduated"|"proportional", "field"?, "expression"?: expression|null (unique: class by this expression's values instead of a field; blank or null: by a field; with neither key, Unique Values keep the layer's expression), "classes"?: 1..12, "method"?: "naturalBreaks"|"equalInterval"|"quantile"|"geometricInterval"|"standardDeviation"|"arithmeticProgression"|"headTailBreaks"|"manual", "breaks"?: [numbers] (manual: the classes' upper bounds, closed by the field's largest value; on a layer read in place, the first class takes every value up to its bound and one more class every value past the last), "ramp"?: string}"#),
        C::new("layer.ramp", "Color Scheme", "Style › Drawing", |s, p| {
            let r = s.ramp(s_req(p, "ramp")?)?;
            let mut dropped = None;
            let out = with(s, p, |l| match &mut l.raster {
                Some(rl) => rl.style.set_ramp(r),
                None => {
                    let g = l.geometry_type();
                    l.renderer.apply_ramp(r, g);
                    // New colours after No Fill are drawn (#1301).
                    dropped = dropped_by_no_fill(l);
                    if take_no_fill(l) == Some(NoFill::Symbols) {
                        l.renderer.map_symbols(show_fill);
                    }
                }
            })?;
            notify_dropped(s, dropped);
            Ok(out)
        }).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?, "ramp": colour ramp name}"#)
        .when(need_layer),
        C::new("layer.invertRamp", "Invert Color Ramp", "Style › Drawing", |s, p| {
            use rustgis_map::symbology::RampReversal;
            let id = s.layer_id(p)?;
            let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
            if l.graphics && l.raster.is_none() {
                return Err("a graphics layer draws each feature in its own style, so it has no color scheme to invert".into());
            }
            let raster = l.raster.is_some();
            let by_expression = !raster && (l.symbol_by.fill.is_some() || l.symbol_by.stroke.is_some());
            let (done, layer_stack) = match &mut l.raster {
                Some(rl) => (rl.style.invert_ramp().then_some(RampReversal { reversed: 0, kept: 0, on_layer_stack: 0 }), None),
                None => {
                    let g = l.geometry_type();
                    let stack = layer_stack_hiding_symbol(l, g);
                    (l.renderer.reverse_ramp(g), stack)
                }
            };
            let Some(done) = done else {
                return Err("the layer has no color scheme to invert: use Graduated or Unique Values (or a Stretch raster)".into());
            };
            // A raster's Invert is a switch, so it has no classes to count. Classes with fewer than two
            // to change places, and none left for their own stacks (which say so below), change nothing.
            if !raster && done.reversed == 0 && done.kept == 0 {
                return Err("the layer has fewer than two classes showing the color scheme, so there is nothing to invert".into());
            }
            if done.kept > 0 {
                // Their own stacks are drawn instead of the symbol, so the reversal can't reach them.
                let kept = done.kept;
                s.notify(
                    format!(
                        "{kept} class(es) drawn with fills or strokes of their own were left as they are: \
                         edit those with their fill and stroke editors"
                    ),
                    false,
                );
            }
            if let Some(what) = layer_stack.filter(|_| done.on_layer_stack > 0) {
                // The classes without a stack of their own are drawn from the layer's, in colors of its own.
                s.notify(
                    format!(
                        "The layer's stacked {what} set their own colors, so the classes show those and little of the color \
                         scheme: the colors were reversed, but change the {what} to see them"
                    ),
                    false,
                );
            }
            if by_expression && done.reversed > 0 {
                // Drawn over the class colors, wherever the expression gives a color.
                s.notify(
                    "The layer's Symbol by Expression sets some features' colors, so the reversed colors show only where it gives none",
                    false,
                );
            }
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?}"#)
        .when(need_layer),
        C::new("layer.classStyle", "Class Style", "Style › Drawing", class_style).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_feature_layer)
            .params(
                r##"{"layer"?, "value"?: text|number (a Unique Values class), "index"?: number (a Graduated Colors class, 0 = the lowest), "other"?: true (Unique Values' <all other values>), "color"?: "#RRGGBB" (preserves polygon translucency, minimum alpha 150)|"#RRGGBBAA" (sets alpha exactly), "outline"?: "#RRGGBB"|"#RRGGBBAA", "label"?: text (the legend label; "" goes back to the value or range)}"##,
            ),
        C::new("layer.color", "Symbol Color", "Style › Drawing", |s, p| {
            let hex = s_req(p, "color")?;
            let c = Rgba::from_hex(hex).ok_or("color must be #RRGGBB or #RRGGBBAA")?;
            // `#RRGGBBAA` sets the fill's alpha exactly; `#RRGGBB` keeps the layer's translucency.
            let explicit_alpha = hex.trim().trim_start_matches('#').len() == 8;
            let mut dropped = None;
            let out = with(s, p, |l| {
                let g = l.geometry_type();
                let fill = |sym: &rustgis_map::Symbol| {
                    let mut out = sym.with_fill(c, g);
                    if g == GeometryType::Polygon {
                        // The outline is its own setting (`layer.outline`); only the fill changes.
                        out.stroke = sym.stroke;
                        if explicit_alpha {
                            out.fill = c;
                        }
                    }
                    out
                };
                match &mut l.renderer {
                    Renderer::Single(sym) => *sym = fill(sym),
                    Renderer::Proportional { base, .. } => *base = fill(base),
                    other => *other = Renderer::Single(fill(&other.base_symbol())),
                }
                // A fill colour after No Fill is drawn, unless it is transparent itself (#1301).
                dropped = dropped_by_no_fill(l);
                take_no_fill(l);
            })?;
            notify_dropped(s, dropped);
            Ok(out)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_feature_layer)
        .params(r##"{"layer"?, "color": "#RRGGBB"|"#RRGGBBAA" (the fill; a polygon's outline is left as it is, set it with layer.outline)}"##),
        C::new("layer.noFill", "No Fill", "Style › Drawing", |s, p| {
            // On again: what was shown since (a class's fills, a symbol's fill) is hidden too, by
            // the mode it hides by already. With nothing shown since, nothing changes, so it is no
            // step to undo (and leaves redo and the saved state alone).
            let id = s.layer_id(p)?;
            let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
            if l.no_fill.is_some() {
                match hide_again(l) {
                    None => s.request_only = true,
                    Some(0) => {}
                    Some(n) => {
                        let text = format!(
                            "{}: No Fill kept {n} fill stack(s) shown since in place of the ones it hid before, which Restore Fill can't put back now",
                            l.name
                        );
                        s.notify(text, false);
                    }
                }
                return Ok(Value::Null);
            }
            with(s, p, |l| {
                // What it hides is kept for Restore Fill.
                let mut hidden = HiddenFills::default();
                if l.source.is_some() || l.has_geometry(GeometryType::Point) || l.has_geometry(GeometryType::Polyline) {
                    // Symbols are shared by geometry families. A polygon-only fill override
                    // leaves point markers and line strokes (including their markers) intact.
                    // A streamed layer may have other families outside the loaded window.
                    l.no_fill = Some(NoFill::Override);
                    hidden.fills = std::mem::replace(&mut l.fill_layers, vec![no_fill_override()]);
                    hidden.class_fills = take_class_fills(&mut l.renderer);
                } else {
                    // Fill layers in a colour of their own draw whatever the symbol's opacity, so
                    // the layer's and the classes' stacks are taken off here too.
                    hidden.fills = std::mem::take(&mut l.fill_layers);
                    hidden.class_fills = take_class_fills(&mut l.renderer);
                    l.renderer.map_symbols(|sym| {
                        hidden.alphas.push(sym.fill.a());
                        sym.fill = sym.fill.with_alpha(0);
                    });
                    l.no_fill = Some(NoFill::Symbols);
                }
                l.hidden_fills = Some(hidden);
            })
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_polygon_features)
        .params(r#"{"layer"?: name|id (default: the active layer containing polygons; hides polygon fills, keeping outlines and point/line symbols)}"#),
        C::new("layer.restoreFill", "Restore Fill", "Style › Drawing", |s, p| {
            let id = s.layer_id(p)?;
            let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
            // `need_no_fill` already refuses a named or active layer without No Fill; this only
            // guards a layer `layer_id` resolves differently: nothing changes, no step to undo.
            if l.no_fill.is_none() {
                s.request_only = true;
                return Ok(Value::Null);
            }
            for lost in restore_fill(l)? {
                s.notify(lost, false);
            }
            Ok(Value::Null)
        })
        .roles(&[])
        .effects(crate::policy::Effects::NONE)
        .when(need_no_fill)
        .params(r#"{"layer"?: name|id (default: the active layer containing polygons; puts back the fills layer.noFill hid, with their colours and opacities, the layer's own fills and each class's, except what was changed since)}"#),
        C::new("layer.outline", "Outline", "Style › Drawing", |s, p| {
            let c = s.get_color(p, "color")?;
            let w = f(p, "width").map(|w| w.clamp(0.0, 20.0) as f32);
            with(s, p, |l| {
                l.renderer.map_symbols(|sym| {
                    if let Some(c) = c {
                        sym.stroke = c;
                    }
                    if let Some(w) = w {
                        sym.stroke_width = w;
                    }
                })
            })
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_feature_layer)
        .params(r##"{"layer"?, "color"?: "#RRGGBB"|"#RRGGBBAA", "width"?: number}"##),
        C::new("layer.lineStrokes", "Line Strokes", "Style › Drawing", line_strokes).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_lines_or_points)
            .params(
                r##"{"layer"?, "strokes": [{"color"?: "#RRGGBB"|"#RRGGBBAA"|null (null: the renderer's line colour), "width"?: points|null (null: the renderer's width), "offset"?: points (positive: right of the line's direction, negative: left), "cap"?: "round"|"flat"|"square", "join"?: "round"|"miter"|"bevel", "level"?: 0..255|null (with layer.strokeLevels: the level it draws at; null: its place in the stack), "dash"?: [points, …]|null (dash, gap, … in turn, at most 8; null: solid), "markers"?: {"mark"?: "circle"|"square"|"triangle"|"diamond"|"star"|"character"|"image"|"hash" (a mark across the line), "char"?: text (a character of a font as the marker instead of a shape), "font"?: text (the installed family it is drawn from; none: the label font), "image"?: text (a PNG, JPEG or SVG image as base64 of at most 2 MiB and 2048 × 2048 pixels (an SVG: as much to draw, with no filters, text, images, animation or links to other files), as the marker instead of a shape: as wide as its size, as tall as its proportions make it; the stroke's colour and the outline don't show), "size"?: points (a marker's size, a hash mark's length; default 4), "at"?: "interval"|"vertices"|"innerVertices"|"first"|"last"|"middle"|"segmentMiddles" or a list of them (default "interval"), "interval"?: points (default 12), "offsetAlong"?: points (from the start, back from the end, on from the middle), "rotate"?: bool (turned with the line; default true), "averageAngle"?: points (the stretch of line a marker at an interval turns with; 0: its segment), "angle"?: degrees clockwise (a hash mark at 0 crosses the line square), "outline"?: "#RRGGBB"|"#RRGGBBAA" (the markers' outline, as wide as the stroke)}|null (markers or hash marks along the line in the stroke's colour, instead of a line; cap, join and dash don't apply)}] (drawn bottom first; [] goes back to the single stroke, or for a class to the layer's strokes; on a point layer they are its marks: each stroke's markers drawn at the point in place of the symbol's, "offsetAlong" shifting a mark right and "offset" down, both turned with its "angle", and "rotate" turning it by layer.markerRotation's field), "value"?: text|number (a Unique Values class's own strokes), "index"?: number (a Graduated Colors class's, 0 = the lowest), "other"?: true (Unique Values' <all other values>)}"##,
            ),
        C::new("layer.fillLayers", "Fill Layers", "Style › Drawing", fill_layers).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_polygon_features)
            .params(
                r##"{"layer"?, "value"?|"index"?|"other"?: a class of Unique Values (its value, or other: true) or Graduated Colors (its index) to set its own fills (none: the layer's), "fills": [{"color"?: "#RRGGBB"|"#RRGGBBAA"|null (null: the feature's fill colour, so the fills follow the classes), "pattern"?: "solid" (default)|"hatch" (parallel lines)|"dots" (markers on a grid)|"gradient" (from color to color2 across the feature)|"shapeburst" (from color at the edge to color2 inside), "angle"?: degrees anticlockwise from east (hatch), "spacing"?: points between lines (hatch, 1 to 500; default 8) or points across and down between markers, a number or [across, down] (dots), "width"?: a hatch line's width in points (0 to 50; default 1), "offset"?: points across the lines (hatch) or [across, down] (dots), "marker"?: "circle"|"square"|"triangle"|"diamond"|"star"|"image" (dots), "char"?: text (dots: a character of a font as the marker instead of a shape), "font"?: text (the installed family it is drawn from; none: the label font), "image"?: text (dots: a PNG, JPEG or SVG image as base64 of at most 2 MiB and 2048 × 2048 pixels (an SVG: as much to draw, with no filters, text, images, animation or links to other files), as the marker instead of a shape: as wide as its size, as tall as its proportions make it; the colour and the outline don't show), "size"?: a marker's size in points (default 3), "outline"?: a marker's outline colour or null, "outlineWidth"?: points, "displacement"?: [across, down] points every other row and column are shifted by (dots), "color2"?: "#RRGGBB"|"#RRGGBBAA" (gradient, shapeburst: the second colour; default white), "stops"?: [[0..1, "#RRGGBB"|"#RRGGBBAA"], …] (gradient, shapeburst: colours between the two, at most 16), "discrete"?: bool (each colour held until the next stop), "shape"?: "linear" (default)|"radial"|"conical" (gradient), "from"?: [across, down]|"centroid" (gradient: where color is, 0..1 in the feature's box; default [0.5, 0], the top middle), "to"?: [across, down]|"centroid" (where color2 is; default [0.5, 1]), "spread"?: "pad" (default)|"reflect"|"repeat" (gradient: past its points), "angle"?: degrees anticlockwise (gradient: turned round its box's middle), "inView"?: bool (gradient: from and to in the map view's box, not the feature's), "distance"?: points|null (shapeburst: how deep it shades; null or none: to the point deepest inside), "blur"?: 0..17 (shapeburst: pixels), "ignoreRings"?: bool (shapeburst: holes' edges left out)}] (bottom first, at most 8; [] for the symbol's single fill; the outline is drawn over them)}"##,
            ),
        C::new("layer.markerRotation", "Marker Rotation", "Style › Drawing", |s, p| {
            let field = match p.get("field") {
                None | Some(Value::Null) => None,
                Some(Value::String(f)) if f.is_empty() => None,
                Some(Value::String(f)) => Some(f.clone()),
                Some(_) => return Err("field is a field's name, or null for none".into()),
            };
            let counterclockwise = match p.get("counterclockwise") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => return Err("counterclockwise is true or false".into()),
            };
            let id = s.layer_id(p)?;
            let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
            if l.geometry_type() != GeometryType::Point || l.graphics {
                return Err("this works on point layers".into());
            }
            if let Some(f) = &field {
                let fi = l.dataset.field_index(f).ok_or_else(|| format!("no field named '{f}'"))?;
                if !l.dataset.fields.get(fi).is_some_and(|fd| fd.ty.is_numeric()) {
                    return Err(format!("'{f}' is not a numeric field"));
                }
            }
            l.marker_rotation = field.map(|field| rustgis_map::MarkerRotation { field, counterclockwise });
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_points)
        .params(r#"{"layer"?, "field": numeric field name|null (each point's marker turned by the angle in it, in degrees; null: not turned), "counterclockwise"?: bool (the angles run counterclockwise, as in mathematics; default clockwise, as on a compass)}"#),
        C::new("layer.sizeBy", "Size by Field", "Style › Drawing", size_by).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_points)
        .params(r#"{"layer"?, "field"?: numeric field name|null (each marker sized by its value, keeping its Unique Values or Graduated Colors class's colours; null: the classes' own sizes; left out: the mapping's field, to change its other settings), "min"?: number (the value drawn smallest; default the field's smallest, or the mapping's), "max"?: number (the value drawn largest, above min; default the field's largest, or the mapping's), "minSize"?: points (default 4, or the mapping's), "maxSize"?: points (default 24, or the mapping's), "scaling"?: "area"|"radius" (area: the marker's area grows with the value, as Proportional Symbols; radius: its size grows in a straight line, as MapLibre's; default area, or the mapping's)} (values past min or max take the end's size; a null or non-numeric value keeps its class's size)"#),
        C::new("layer.symbolBy", "Symbol by Expression", "Style › Drawing", symbol_by).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_feature_layer)
            .params(r#"{"layer"?, "size"?: expression|null (a point's marker size in points), "fill"?: expression|null (the fill colour: '#RRGGBB', a CSS colour name or 'r,g,b[,a]' as color_rgb gives it), "stroke"?: expression|null (the outline colour, a line's colour), "strokeWidth"?: expression|null (the outline's width in points, a line's width), "angle"?: expression|null (point layers: each marker turned by it, in degrees clockwise, in place of layer.markerRotation's field)} (each expression in the language of queries, per feature, @map_scale the map's scale, @page_feature_id, @page_name and @page_number the map series page (null off a series page); a key left out keeps its expression, null or "" removes it; where an expression gives null the symbol's own value is drawn)"#),
        C::new("layer.strokeLevels", "Stroke Levels", "Style › Drawing", |s, p| {
            let on = b(p, "on").ok_or("on is true or false")?;
            let id = s.layer_id(p)?;
            let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
            if l.geometry_type() != GeometryType::Polyline || l.graphics {
                return Err("this works on line layers".into());
            }
            l.stroke_levels = on;
            Ok(json!({"on": on}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_lines)
        .params(r#"{"layer"?, "on": bool (draw every feature's stacked strokes of one level before the next, so casings join under the cores)}"#),
        C::new("layer.marker", "Marker", "Style › Drawing", |s, p| {
            let shape = ps(p, "shape").map(|n| Marker::ALL.into_iter().find(|m| m.label().eq_ignore_ascii_case(n)).ok_or("unknown marker shape")).transpose()?;
            let size = f(p, "size").map(|v| v.clamp(1.0, 80.0) as f32);
            let (ch, font) = (character(p)?, ps(p, "font"));
            // Refused before the image is kept: a call that changes nothing keeps nothing.
            not_empty(p.get("image"))?;
            if shape.is_some() && ch.is_some() {
                return Err("give a shape or a char, not both".into());
            }
            if p.get("image").is_some_and(|v| !v.is_null()) && (shape.is_some() || ch.is_some()) {
                return Err("give a shape, a char or an image, only one".into());
            }
            let id = s.layer_id(p)?;
            s.map().layer(id).ok_or("no such layer")?;
            // Decoded here, on the UI thread, as any command runs: at most 2 MiB of base64, whose
            // declared pixels are checked before the decoder runs (`render::MARKER_PIXELS` each,
            // some 80 ms; `render::RUN_MARKER_PIXELS` in all until RustGIS restarts, some 5 s, the
            // most any number of calls can take). A call refused after the check sets none of its
            // images on the layer, but their decoding stays charged, and the decoded copies kept:
            // it took the time it stands for.
            let mut new = Vec::new();
            let mut image = picture(p, &mut new)?;
            keep_pictures(image.iter_mut(), new)?;
            with(s, p, |l| {
                l.renderer.map_symbols(|sym| {
                    if let Some(m) = shape {
                        sym.marker = m;
                    }
                    if let Some(img) = image {
                        sym.marker = Marker::Picture(img);
                    }
                    let was = match sym.marker {
                        Marker::Char(g) => Some(g),
                        _ => None,
                    };
                    // A new character keeps the family of the one before; a family alone changes a character's.
                    match (ch, was, font) {
                        (Some(c), _, _) => sym.marker = Marker::Char(Glyph::new(c, font.unwrap_or(was.map_or("", |g| g.font.as_str())))),
                        (None, Some(g), Some(fam)) => sym.marker = Marker::Char(Glyph::new(g.ch, fam)),
                        _ => {}
                    }
                    if let Some(sz) = size {
                        sym.size = sz;
                    }
                })
            })
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_feature_layer)
        .params(r#"{"layer"?, "shape"?: "circle"|"square"|"triangle"|"diamond"|"star", "char"?: text (a character of a font as the marker, its first; sized by the font's em), "font"?: text (the installed family the character is drawn from; "" or one not installed: the label font), "image"?: text (a PNG, JPEG or SVG image as base64 of at most 2 MiB and 2048 × 2048 pixels (an SVG: as much to draw, with no filters, text, images, animation or links to other files), as the marker: as wide as the size, as tall as its proportions make it; its fill and outline don't show), "size"?: number}"#),
        C::new("layer.transparency", "Transparency", "Style › Effects", |s, p| {
            let t = f(p, "value").ok_or("value (0–100) is required")?.clamp(0.0, 100.0) as f32;
            with(s, p, |l| l.transparency = t)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_layer)
        .params(r#"{"layer"?, "value": 0..100}"#),
        C::new("layer.scaleRange", "Visibility Range", "Style › Visibility Range", |s, p| {
            let min = f(p, "min").filter(|v| *v > 0.0);
            let max = f(p, "max").filter(|v| *v > 0.0);
            with(s, p, |l| {
                l.min_scale = min;
                l.max_scale = max;
            })
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_layer)
        .params(r#"{"layer"?, "min"?: scale (out beyond), "max"?: scale (in beyond)}"#),
        C::new("layer.referenceScale", "Reference Scale", "Style › Drawing", |s, p| {
            let (lo, hi) = rustgis_map::REFERENCE_SCALES;
            let scale = match p.get("scale") {
                None | Some(Value::Null) => None,
                Some(Value::String(t)) if t.eq_ignore_ascii_case("current") => {
                    let now = s.map().scale().round();
                    if !now.is_finite() {
                        return Err("the map has no scale here".into());
                    }
                    Some(now.clamp(lo, hi))
                }
                Some(_) => Some(f(p, "scale").filter(|v| (lo..=hi).contains(v)).ok_or("scale is a number from 1 to 1000000000, \"current\" or null")?),
            };
            let id = s.layer_id(p)?;
            let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
            if l.is_raster() || l.graphics {
                return Err("this works on feature layers".into());
            }
            l.reference_scale = scale;
            Ok(json!({"scale": scale}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_feature_layer)
        .params(r#"{"layer"?, "scale": 1:N the symbols' and labels' sizes are set for (drawn at another scale they grow and shrink with the map)|"current" (the map's scale)|null (sizes stay as set on screen)}"#),
        C::new("layer.selectable", "Selectable", "Layers", |s, p| {
            let ids = layer_ids(s, p)?;
            let on = b(p, "on").unwrap_or_else(|| {
                let lead = s.active_layer.filter(|a| ids.contains(a)).or(ids.first().copied());
                !lead.and_then(|id| s.map().layer(id)).is_some_and(|l| l.selectable)
            });
            for id in ids {
                if let Some(l) = s.map_mut().layer_mut(id).filter(|l| !l.is_raster()) {
                    l.selectable = on;
                }
            }
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?: name|id | [name|id] (default: the layer selection), "on"?: bool (toggles when omitted: all take the opposite of the active layer's state)}"#)
        .when(need_feature_layer),
        C::new("layer.expand", "Expand Legend", "Layers", |s, p| {
            let on = b(p, "on");
            with(s, p, |l| l.expanded = on.unwrap_or(!l.expanded))
        }).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?, "on"?: bool (toggles when omitted)}"#)
        .view()
        .when(need_layer),
        C::new("labels.enable", "Label", "Labeling › Labels", |s, p| {
            let on = b(p, "on");
            with(s, p, |l| l.labels.enabled = on.unwrap_or(!l.labels.enabled))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_feature_layer)
        .params(r#"{"layer"?, "on"?: bool}"#),
        C::new("labels.field", "Label Field", "Labeling › Label Class", |s, p| {
            let name = s_req(p, "field")?.to_string();
            let id = s.layer_id(p)?;
            let fname = s.map().layer(id).and_then(|l| l.dataset.field_index(&name).and_then(|i| l.dataset.fields.get(i)).map(|f| f.name.clone()));
            let fname = fname.ok_or_else(|| format!("no field named '{name}'"))?;
            super::labels::with_labels(s, p, |labels| {
                super::labels::class_in(labels, p)?.field = fname;
                labels.enabled = true;
                Ok(())
            })
        }).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?, "class"?: name (none: the Default class, by whatever name it has), "field": name}"#)
        .when(need_feature_layer),
        C::new("labels.style", "Text Symbol", "Labeling › Text Symbol", |s, p| {
            let size = f(p, "size").map(|v| v.clamp(*LABEL_SIZE.start(), *LABEL_SIZE.end()) as f32);
            let color = s.get_color(p, "color")?;
            let (halo, bold, italic) = (b(p, "halo"), b(p, "bold"), b(p, "italic"));
            // A family by name, whether or not this machine has it: the project may be drawn on
            // one that does. Blank goes back to the label font.
            let font = ps(p, "font").map(rustgis_map::symbology::font_name);
            // A halo's own colour ("" or null: one standing out from the text), its width in points
            // (null: the thin halo), and the letter spacing.
            let halo_color = match p.get("haloColor") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(Value::String(c)) if c.trim().is_empty() => Some(None),
                Some(_) => Some(Some(s.get_color(p, "haloColor")?.ok_or("haloColor must be #RRGGBB or #RRGGBBAA")?)),
            };
            let hw = rustgis_map::symbology::HALO_WIDTH;
            let sp = (rustgis_map::layout::MIN_LETTER_SPACING, rustgis_map::layout::MAX_LETTER_SPACING);
            let halo_width = match p.get("haloWidth") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(_) => {
                    let w = f(p, "haloWidth").ok_or("haloWidth must be a number of points, or null")?;
                    Some(Some(w.clamp(*hw.start(), *hw.end()) as f32))
                }
            };
            let spacing = match p.get("spacing") {
                None => None,
                Some(_) => Some(f(p, "spacing").ok_or("spacing must be a number of points")?.clamp(sp.0, sp.1) as f32),
            };
            // How much of a line's room a fitted label is spread over (#2265; null: the default).
            let stretch = match p.get("stretch") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(_) => {
                    let st = rustgis_map::symbology::LABEL_STRETCH;
                    let v = f(p, "stretch").filter(|v| v.is_finite()).ok_or("stretch must be a share of the line, 0.1 to 1, or null")?;
                    Some(Some((v as f32).clamp(*st.start(), *st.end())))
                }
            };
            let background = match p.get("background") {
                None => None,
                Some(v) => Some(super::labels::background_change(v)?),
            };
            // Wrapping: a character to break lines at ("" or null: none), a longest line in
            // characters (0: none), and how the lines line up.
            let wrap_char = match p.get("wrapChar") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(Value::String(c)) => Some(rustgis_map::symbology::wrap_char(c)),
                Some(_) => return Err("wrapChar must be text (\"\" for none)".into()),
            };
            let wrap_length = match p.get("wrapLength") {
                None => None,
                Some(Value::Null) => Some(0),
                Some(_) => {
                    let n = f(p, "wrapLength").filter(|n| *n >= 0.0).ok_or("wrapLength must be a number of characters (0 for none)")?;
                    Some(n.min(f64::from(rustgis_map::symbology::MAX_WRAP_LENGTH)) as u32)
                }
            };
            let line_align = match ps(p, "lineAlign") {
                None if p.get("lineAlign").is_some() => return Err("lineAlign must be left, center or right".into()),
                None => None,
                Some(a) => Some(
                    rustgis_map::layout::TextAlign::ALL
                        .into_iter()
                        .find(|t| t.code().eq_ignore_ascii_case(a.trim()))
                        .ok_or_else(|| format!("unknown lineAlign '{a}' (left, center or right)"))?,
                ),
            };
            super::labels::with_class(s, p, |c| {
                if let Some(change) = &background {
                    c.background = change(c.background.clone());
                }
                if let Some(v) = wrap_char.clone() {
                    c.wrap_char = v;
                }
                if let Some(v) = wrap_length {
                    c.wrap_length = v;
                }
                if let Some(v) = line_align {
                    c.line_align = v;
                }
                if let Some(v) = halo_color {
                    c.halo_color = v;
                }
                if let Some(v) = halo_width {
                    c.halo_width = v;
                }
                if let Some(v) = spacing {
                    c.spacing = v;
                }
                if let Some(v) = stretch {
                    c.stretch = v;
                }
                if let Some(name) = font.clone() {
                    c.font = name;
                }
                if let Some(v) = size {
                    c.size = v;
                }
                if let Some(col) = color {
                    c.color = col;
                }
                if let Some(h) = halo {
                    c.halo = h;
                }
                if let Some(bb) = bold {
                    c.bold = bb;
                }
                if let Some(it) = italic {
                    c.italic = it;
                }
            })
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_feature_layer)
        .params(r##"{"layer"?, "class"?: name (none: the Default class, by whatever name it has), "size"?: number, "color"?: "#RRGGBB"|"#RRGGBBAA", "halo"?: bool, "haloColor"?: "#RRGGBB"|"#RRGGBBAA"|"" (""/null: white or dark, whichever stands out from the text), "haloWidth"?: points 0-10|null (null: the thin halo), "spacing"?: points between letters (-5 to 50), "stretch"?: share of a line's room, 0.1 to 1: a label placed curved with "fit": "auto" spreads its letters over it when the line has room to spare (null: 0.6); any other curved label (along a line, a polygon's centre line or a guide line) is letter-spaced to cover it, within spacing's range (null: its spacing), "bold"?: bool, "italic"?: bool, "font"?: installed family name (labels.fonts lists them; "" is the label font), "background"?: false|null (none) | true (white, a thin dark outline) | {"fill"?: "#RRGGBB"|"#RRGGBBAA"|"" ("": none), "stroke"?: colour|"" (the outline; "": none), "strokeWidth"?: points 0-10, "padding"?: [across, up and down] points 0-50, "radius"?: corner radius in points 0-50} (a rectangle behind the text; what isn't given is kept, or the default's), "wrapChar"?: text (a line break wherever it occurs, left out; a space puts each word on its own line; "" for none), "wrapLength"?: characters (a longest line, broken between words; 0 for none; at most 500), "lineAlign"?: "left"|"center"|"right" (how a label's lines line up)}"##),
        C::new("layer.definitionQuery", "Definition Query", "Data › Definition Query", |s, p| {
            let q = ps(p, "where").map(str::trim).unwrap_or("").to_string();
            let id = s.layer_id(p)?;
            if !q.is_empty()
                && let Some(l) = s.map().layer(id)
            {
                rustgis_data::query::parse(&q, &l.dataset)?;
            }
            // A source the view can't filter as it reads filters a sample of a crowded view.
            let sampled = match s.map().layer(id) {
                Some(l) if !q.is_empty() => {
                    l.source.as_ref().and_then(|src| src.query_sample(&q, &l.dataset.fields)).map(|n| (l.name.clone(), n))
                }
                _ => None,
            };
            if let Some((name, (count, most))) = sampled {
                let read = match count {
                    Some(count) => format!("at most {most} of the layer's {count} features"),
                    None => format!("at most {most} features; the layer doesn't say how many it holds"),
                };
                s.notify(
                    format!(
                        "{name}: the query filters the features each view reads ({read}), a sample where a view holds more, so some matches may not show (Load All Features filters every one)"
                    ),
                    false,
                );
            }
            with(s, p, |l| l.definition_query = if q.is_empty() { None } else { Some(q.clone()) })
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_feature_layer)
        .params(r#"{"layer"?, "where": sql (empty to clear)}"#),
        C::new("table.open", "Attribute Table", "Data › Table", |s, p| {
            s.table_layer = Some(s.layer_id(p)?);
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?: name|id (default: the active layer)}"#)
        .view()
        .key("Ctrl+T")
        .when(need_feature_layer),
        C::new("table.statistics", "Column Statistics", "Data › Table", super::table_stats::run)
            .roles(&[])
            .effects(crate::policy::Effects::NONE)
            .view()
            .params(r#"{"layer"?: name|id (default: the active layer), "field": name (the first of that name, not told apart by case) | "index": the field's position in dataset order, from 0 (not the displayed order after Move Left/Right) (tells apart columns of the same name; wins over "field"), "selected"?: bool (only the layer's selected features; default: all)} → a numeric field: {layer, field, kind: "number", selected, rows (features considered), count (numbers used), nulls (null or empty), skipped (not finite numbers: NaN, infinity, or text that isn't a number; said in a notice), sum, mean, min, max (whole numbers are added up exactly: past 2^53 a sum, min or max is a whole number or its digits as text, never rounded), stdDev (the population one, divided by n, as Summary Statistics' STD)}; a sum or other number too large for a decimal is null, said in a notice; a text, date or other field: {…, kind: "text"|"date", count, nulls, distinct (bounded by 100,000 entries and 8 MiB of distinct text: then distinctCapped is true and a notice gives the lower bound), min, max (text by character code, dates by date)}"#),
        C::new("table.close", "Close Table", "Data › Table", |s, _| {
            s.table_layer = None;
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        C::new("table.remove", "Remove Table", "Data › Table", |s, p| {
            // The position this command took before tables had ids: refused, rather than
            // removing the first table of the name where another was meant.
            if p.get("at").is_some_and(|at| !at.is_null()) {
                return Err("\"at\" is no longer read: give the table's id as \"table\" (info.project lists the ids)".into());
            }
            let i = s.table_index(p)?;
            s.project.tables.remove(i);
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"table": name|id (a standalone table; of tables sharing a name, the name is the first)} (the old position, "at", is refused)"#),
        C::new("layer.fields", "Fields", "Data › Data Design", |s, p| {
            let id = s.layer_id(p)?;
            let l = s.map().layer(id).ok_or("no layer")?;
            Ok(json!(l.dataset.fields.iter().map(|f| json!({"name": f.name, "type": f.ty.label(), "alias": f.alias})).collect::<Vec<_>>()))
        }).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?: name|id (default: the active layer)}"#)
        .view()
        .when(need_feature_layer),
    ]
}

impl Session {
    fn get_color(&self, p: &Value, k: &str) -> Result<Option<Rgba>, String> {
        match ps(p, k) {
            Some(c) => Rgba::from_hex(c).map(Some).ok_or_else(|| format!("{k} must be #RRGGBB or #RRGGBBAA")),
            None => Ok(None),
        }
    }
}

/// Polygon styling also applies to the polygon features of a mixed layer.
fn need_polygon_features(s: &Session) -> Option<&'static str> {
    match s.active_layer.and_then(|id| s.map().layer(id)) {
        Some(l) if !l.is_raster() && l.has_geometry(GeometryType::Polygon) && !l.graphics => None,
        Some(_) => Some("this works on layers containing polygons"),
        None => Some("select a layer containing polygons in the Layers pane first"),
    }
}

/// A layer whose polygon fills No Fill hides is active.
fn need_no_fill(s: &Session) -> Option<&'static str> {
    need_polygon_features(s).or_else(|| {
        s.active_layer.and_then(|id| s.map().layer(id)).filter(|l| l.no_fill.is_none()).map(|_| "No Fill isn't on for this layer")
    })
}

/// A line layer is active.
fn need_lines_or_points(s: &Session) -> Option<&'static str> {
    match s.active_layer.and_then(|id| s.map().layer(id)) {
        Some(l) if matches!(l.geometry_type(), GeometryType::Polyline | GeometryType::Point) && !l.graphics => None,
        Some(_) => Some("this works on line and point layers"),
        None => Some("select a layer in the Layers pane first"),
    }
}

fn need_points(s: &Session) -> Option<&'static str> {
    match s.active_layer.and_then(|id| s.map().layer(id)) {
        Some(l) if l.geometry_type() == GeometryType::Point && !l.graphics => None,
        Some(_) => Some("this works on point layers"),
        None => Some("select a layer in the Layers pane first"),
    }
}

fn need_lines(s: &Session) -> Option<&'static str> {
    match s.active_layer.and_then(|id| s.map().layer(id)) {
        Some(l) if l.geometry_type() == GeometryType::Polyline && !l.graphics => None,
        Some(_) => Some("this works on line layers"),
        None => Some("select a layer in the Layers pane first"),
    }
}

/// `layer.sizeBy`: sets (or removes) a classified point layer's marker size from a numeric
/// field ([`rustgis_map::Layer::size_by`]). What isn't given keeps the mapping's, else the
/// field's range, 4 to 24 points and area scaling. Desktop range scans use the background read
/// queue with progress and Cancel; synchronous adapters must dispatch off their window thread.
fn size_by(s: &mut Session, p: &Value) -> CmdResult {
    use rustgis_map::symbol_by::{MAX_SIZE, SizeBy, SizeScaling};
    // Left out, the mapping's own field (to change its other settings); null or "" removes it.
    let field = match p.get("field") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(f)) if f.is_empty() => Some(None),
        Some(Value::String(f)) => Some(Some(f.clone())),
        Some(_) => return Err("field is a field's name, or null for none".into()),
    };
    let num = |k: &str| match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => f(p, k).map(Some).ok_or_else(|| format!("{k} is a number")),
    };
    let (min, max, min_size, max_size) = (num("min")?, num("max")?, num("minSize")?, num("maxSize")?);
    let scaling = match p.get("scaling") {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) => Some(SizeScaling::from_code(t).ok_or("scaling is \"area\" or \"radius\"")?),
        Some(_) => return Err("scaling is \"area\" or \"radius\"".into()),
    };
    let id = s.layer_id(p)?;
    let ahead = super::reads::take::<SizeRangeAhead>(s, &format!("layer.sizeBy:{id}"));
    #[cfg(not(target_arch = "wasm32"))]
    let on_window = s.statistics_window_thread == Some(std::thread::current().id());
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    if l.geometry_type() != GeometryType::Point || l.graphics {
        return Err("this works on point layers".into());
    }
    let field = match field {
        Some(field) => field,
        None => Some(l.size_by.as_ref().map(|b| b.field.clone()).ok_or("give a field: the layer has no size mapping to change")?),
    };
    let Some(field) = field else {
        l.size_by = None;
        return Ok(Value::Null);
    };
    if !matches!(l.renderer, rustgis_map::Renderer::Unique { .. } | rustgis_map::Renderer::Graduated { .. }) {
        return Err(
            "size by field goes with Unique Values or Graduated Colors; a single symbol sized by a field is Proportional Symbols".into()
        );
    }
    let fi = l.dataset.field_index(&field).ok_or_else(|| format!("no field named '{field}'"))?;
    if !l.dataset.fields.get(fi).is_some_and(|fd| fd.ty.is_numeric()) {
        return Err(format!("'{field}' is not a numeric field"));
    }
    let field = l.dataset.fields.get(fi).map(|fd| fd.name.clone()).unwrap_or(field);
    // The mapping's own where it is of the same field, else the field's range.
    let had = l.size_by.as_ref().filter(|h| l.dataset.field_index(&h.field) == Some(fi));
    let (min, max) = (min.or(had.map(|h| h.min)), max.or(had.map(|h| h.max)));
    // Both bounds from the field's own range: a field with one value has no spread to scale over.
    let both_from_range = min.is_none() && max.is_none();
    let range = if min.is_none() || max.is_none() {
        // A streamed layer holds the view's features only, so their range would be the view's.
        if l.is_streamed() {
            return Err("a size range needs all of a streamed layer: layer.loadAll first, or give min and max".into());
        }
        #[cfg(not(target_arch = "wasm32"))]
        if ahead.is_none() && on_window {
            return Err(
                "size range detection needs the desktop background read queue; give min and max or run layer.sizeBy in the background"
                    .into(),
            );
        }
        match ahead {
            Some(a) if a.field == fi && a.dataset.fields == l.dataset.fields && a.dataset.features.shares(&l.dataset.features) => a.range?,
            Some(a) if a.field != fi => return Err("the range read was for another field: ask again".into()),
            Some(_) => return Err("the layer changed while its size range was read: ask again".into()),
            None => size_range(&l.dataset, fi)?,
        }
    } else {
        None
    };
    let min = min.or(range.map(|r| r.0)).ok_or_else(|| format!("'{field}' has no numeric values: give min and max"))?;
    let max = max.or(range.map(|r| r.1)).ok_or_else(|| format!("'{field}' has no numeric values: give min and max"))?;
    if max <= min && both_from_range {
        return Err(format!("'{field}' has the same value ({min}) in every row, so give min and max"));
    }
    if max <= min {
        return Err(format!("max ({max}) must be above min ({min})"));
    }
    if !(max - min).is_finite() {
        return Err(format!("the range from {min} to {max} is too wide to scale sizes over"));
    }
    let size = |v: Option<f64>, had: Option<f32>, default: f32, k: &str| -> Result<f32, String> {
        match v {
            Some(v) if (0.0..=f64::from(MAX_SIZE)).contains(&v) => Ok(v as f32),
            Some(_) => Err(format!("{k} is from 0 to {MAX_SIZE} points")),
            None => Ok(had.unwrap_or(default)),
        }
    };
    let prev = l.size_by.as_ref();
    let min_size = size(min_size, prev.map(|h| h.min_size), 4.0, "minSize")?;
    let max_size = size(max_size, prev.map(|h| h.max_size), 24.0, "maxSize")?;
    let scaling = scaling.or(prev.map(|h| h.scaling)).unwrap_or_default();
    let by = SizeBy { field, min, max, min_size, max_size, scaling };
    let out = json!({"field": by.field, "min": by.min, "max": by.max, "minSize": by.min_size, "maxSize": by.max_size, "scaling": by.scaling.code()});
    l.size_by = Some(by);
    size_hidden_notice(s, id);
    Ok(out)
}

/// Says when a layer's size mapping doesn't show because its points are aggregated into bins,
/// which are drawn instead of them. Called by `layer.sizeBy` and `layer.aggregation`, whichever
/// comes second, as for a diagram ([`super::diagram::hidden_notice`]).
pub(crate) fn size_hidden_notice(s: &mut Session, id: u32) {
    let text = s.map().layer(id).filter(|l| l.aggregation.is_some() && rustgis_map::symbol_by::SizeBy::applies_to(l)).and_then(|l| {
        let by = l.size_by.as_ref()?;
        Some(format!(
            "Layer '{}': its sizes by {} don't show while its points are aggregated into bins; turn Aggregation off to see them",
            crate::project::short_name(&l.name),
            by.field
        ))
    });
    if let Some(text) = text {
        s.notify(text, false);
    }
}

/// A size range prepared on shared rows; landing refuses edits to those rows or their fields.
struct SizeRangeAhead {
    dataset: Dataset,
    field: usize,
    range: Result<Option<(f64, f64)>, String>,
}

/// Scans one numeric field once, checking Cancel and advancing progress every 1024 rows.
fn size_range(dataset: &Dataset, field: usize) -> Result<Option<(f64, f64)>, String> {
    let count = dataset.features.len();
    rustgis_data::progress::begin(count);
    let mut range: Option<(f64, f64)> = None;
    for (row, feature) in dataset.features.iter().enumerate() {
        if row % 1024 == 0 {
            rustgis_data::progress::step((count - row).min(1024)).map_err(|e| e.to_string())?;
        }
        if let Some(value) = feature.attrs.get(field).and_then(rustgis_data::Value::as_f64).filter(|v| v.is_finite()) {
            range = Some(range.map_or((value, value), |(lo, hi)| (lo.min(value), hi.max(value))));
        }
    }
    Ok(range)
}

/// Prepares range detection off the desktop frame only when the mapping needs bounds. Invalid
/// requests are left to the command to explain without scanning their data.
pub(crate) fn size_by_ahead(s: &Session, p: &Value) -> Option<super::reads::ReadWork> {
    let id = s.layer_id(p).ok()?;
    let layer = s.map().layer(id)?;
    let field = layer.dataset.field_index(ps(p, "field")?)?;
    if !rustgis_map::symbol_by::SizeBy::applies_to(layer) || layer.is_streamed() || !layer.dataset.fields.get(field)?.ty.is_numeric() {
        return None;
    }
    if (f(p, "min").is_some() && f(p, "max").is_some())
        || layer.size_by.as_ref().is_some_and(|by| layer.dataset.field_index(&by.field) == Some(field))
    {
        return None;
    }
    let dataset = layer.dataset.clone();
    Some(super::reads::ahead("Size by Field", format!("layer.sizeBy:{id}"), move || {
        let range = size_range(&dataset, field);
        SizeRangeAhead { dataset, field, range }
    }))
}

/// `layer.symbolBy`: sets (or removes) the expressions a layer's symbol takes per feature, each
/// checked against its fields first.
fn symbol_by(s: &mut Session, p: &Value) -> CmdResult {
    const KEYS: [&str; 5] = ["size", "fill", "stroke", "strokeWidth", "angle"];
    let id = s.layer_id(p)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    if l.is_raster() || l.graphics {
        return Err("this works on feature layers".into());
    }
    let mut by = l.symbol_by.clone();
    for key in KEYS {
        let text = match p.get(key) {
            None => continue,
            Some(Value::Null) => None,
            Some(Value::String(t)) if t.trim().is_empty() => None,
            Some(Value::String(t)) => Some(t.clone()),
            Some(_) => return Err(format!("{key} is an expression's text, or null for none")),
        };
        if let Some(t) = &text {
            if matches!(key, "size" | "angle") && l.geometry_type() != GeometryType::Point {
                return Err(format!("{key} works on point layers"));
            }
            rustgis_data::query::parse(t, &l.dataset).map_err(|e| format!("{key}: {e}"))?;
        }
        let slot = match key {
            "size" => &mut by.size,
            "fill" => &mut by.fill,
            "stroke" => &mut by.stroke,
            "strokeWidth" => &mut by.stroke_width,
            _ => &mut by.angle,
        };
        *slot = text;
    }
    l.symbol_by = by;
    let b = &l.symbol_by;
    Ok(json!({"size": b.size, "fill": b.fill, "stroke": b.stroke, "strokeWidth": b.stroke_width, "angle": b.angle}))
}

/// `layer.lineStrokes`: sets a line layer's stacked strokes, or one class's own.
fn line_strokes(s: &mut Session, p: &Value) -> CmdResult {
    let class = class_ref(p)?;
    let list = p.get("strokes").and_then(Value::as_array).ok_or("strokes is a list of strokes ([] for the single stroke)")?;
    if list.len() > MAX_STROKES {
        return Err(format!("at most {MAX_STROKES} strokes"));
    }
    // Refused before the strokes are read: a call on the wrong layer or class keeps none of their
    // images.
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let points = l.geometry_type() == GeometryType::Point;
    if !(points || l.geometry_type() == GeometryType::Polyline) || l.graphics {
        return Err("this works on line and point layers".into());
    }
    if let Some(c) = &class {
        l.renderer.class_strokes(c)?;
    }
    let (mut strokes, mut new) = (Vec::with_capacity(list.len()), Vec::new());
    for (i, v) in list.iter().enumerate() {
        let o = v.as_object().ok_or_else(|| format!("stroke {} is not an object", i + 1))?;
        let color = match o.get("color") {
            None | Some(Value::Null) => None,
            Some(Value::String(c)) => {
                Some(Rgba::from_hex(c).ok_or_else(|| format!("stroke {}: color must be #RRGGBB or #RRGGBBAA", i + 1))?)
            }
            Some(_) => return Err(format!("stroke {}: color must be #RRGGBB or #RRGGBBAA", i + 1)),
        };
        let num = |k: &str| -> Result<Option<f32>, String> {
            match o.get(k) {
                None | Some(Value::Null) => Ok(None),
                // Brought into range before narrowing, so 1e300 is the largest width, not infinity.
                Some(v) => v
                    .as_f64()
                    .filter(|x| x.is_finite())
                    .map(|x| Some(x.clamp(-1e6, 1e6) as f32))
                    .ok_or_else(|| format!("stroke {}: {k} is a number", i + 1)),
            }
        };
        let cap = match o.get("cap") {
            None | Some(Value::Null) => LineCap::default(),
            Some(Value::String(c)) => LineCap::from_code(c).ok_or_else(|| format!("stroke {}: unknown cap '{c}'", i + 1))?,
            Some(_) => return Err(format!("stroke {}: cap is \"round\", \"flat\" or \"square\"", i + 1)),
        };
        let join = match o.get("join") {
            None | Some(Value::Null) => LineJoin::default(),
            Some(Value::String(j)) => LineJoin::from_code(j).ok_or_else(|| format!("stroke {}: unknown join '{j}'", i + 1))?,
            Some(_) => return Err(format!("stroke {}: join is \"round\", \"miter\" or \"bevel\"", i + 1)),
        };
        let level = match o.get("level") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_u64()
                    .and_then(|l| u8::try_from(l).ok())
                    .ok_or_else(|| format!("stroke {}: level is a whole number from 0 to 255", i + 1))?,
            ),
        };
        let bad_dash = || {
            format!(
                "stroke {}: dash is a list of 1 to {} lengths in points (dash, gap, …), each 0 to {}, not all 0",
                i + 1,
                rustgis_map::symbology::MAX_DASH_STEPS,
                rustgis_map::symbology::MAX_DASH_STEP
            )
        };
        // A pattern out of range is an error here, so a script learns of it; a project file's is
        // brought into range instead (`de_dash`), so a hand-edited file still opens.
        let dash = match o.get("dash") {
            None | Some(Value::Null) => None,
            Some(Value::Array(a)) => {
                let steps: Vec<f32> =
                    a.iter().map(|v| v.as_f64().map(|x| x.clamp(-1e6, 1e6) as f32)).collect::<Option<_>>().ok_or_else(bad_dash)?;
                Some(rustgis_map::symbology::Dash::new(&steps).ok_or_else(bad_dash)?)
            }
            Some(_) => return Err(bad_dash()),
        };
        // A point draws marks only: a line stroke would draw nothing there.
        if points && o.get("markers").is_none_or(Value::is_null) {
            return Err(format!("stroke {}: a point layer's strokes are markers", i + 1));
        }
        let markers = match o.get("markers") {
            None | Some(Value::Null) => None,
            Some(m) => Some(line_markers(m, &mut new).map_err(|e| format!("stroke {}: markers: {e}", i + 1))?),
        };
        strokes.push(
            LineStroke { color, width: num("width")?, offset: num("offset")?.unwrap_or(0.0), cap, join, level, dash, markers }.clamped(),
        );
    }
    // Every stroke checked: their new images are kept.
    let slots = strokes.iter_mut().filter_map(|st: &mut LineStroke| match st.markers.as_mut().map(|m| &mut m.mark) {
        Some(LineMark::Marker(Marker::Picture(p))) => Some(p),
        _ => None,
    });
    keep_pictures(slots, new)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    let n = strokes.len();
    match class {
        Some(c) => l.renderer.set_class_strokes(&c, strokes)?,
        None => l.line_strokes = strokes,
    }
    Ok(json!({"strokes": n}))
}

/// The alpha a polygon fill gets back when No Fill is turned off: a new polygon symbol's.
fn shown_fill_alpha() -> u8 {
    rustgis_map::Symbol::default_for(GeometryType::Polygon, 0).fill.a()
}

/// The one transparent fill No Fill lays over a layer's symbols.
fn no_fill_override() -> rustgis_map::fills::FillLayer {
    rustgis_map::fills::FillLayer { color: Some(Rgba::rgb(0, 0, 0).with_alpha(0)), ..Default::default() }
}

/// Takes the classes' own fill stacks off the renderer, for No Fill to keep.
fn take_class_fills(r: &mut Renderer) -> Vec<(ClassRef, Vec<rustgis_map::fills::FillLayer>)> {
    match r {
        Renderer::Unique { class_fills, other_fills, .. } => std::mem::take(class_fills)
            .into_iter()
            .map(|(v, f)| (ClassRef::Value(v), f))
            .chain([(ClassRef::Other, std::mem::take(other_fills))])
            .filter(|(_, f)| !f.is_empty())
            .collect(),
        Renderer::Graduated { class_fills, .. } => std::mem::take(class_fills).into_iter().map(|(i, f)| (ClassRef::Index(i), f)).collect(),
        _ => Vec::new(),
    }
}

/// No Fill on again: hides what was shown since it was turned on, keeping it for Restore Fill in
/// place of what was kept for the same class or symbol. `None` when nothing was shown, else how
/// many kept fill stacks (the layer's, a class's) the newer ones replaced, for a notice. Nothing
/// shown leaves the saved state as it was, so a project saved by an earlier RustGIS keeps saying
/// that nothing was kept.
fn hide_again(l: &mut rustgis_map::Layer) -> Option<usize> {
    // A class's own fills, set since: hidden on either path (they draw over the symbol's fill).
    let shown = take_class_fills(&mut l.renderer);
    // On the symbols path the layer's own fill layers are drawn, so those shown since are hidden
    // too (the override path keeps its own, which No Fill replaces).
    let symbols = l.no_fill == Some(NoFill::Symbols);
    let mut symbol_shown = false;
    if symbols {
        l.renderer.map_symbols(|sym| symbol_shown |= sym.fill.a() > 0);
    }
    let stack_shown = symbols && !l.fill_layers.is_empty();
    if shown.is_empty() && !symbol_shown && !stack_shown {
        return None;
    }
    // Started over none (a project saved with No Fill on by a RustGIS that kept nothing), the
    // record holds only what this hides, so Restore Fill still says the rest wasn't kept.
    let hidden = l.hidden_fills.get_or_insert_with(|| HiddenFills { partial: true, ..HiddenFills::default() });
    let mut replaced = 0;
    for (class, f) in shown {
        match hidden.class_fills.iter_mut().find(|(c, _)| *c == class) {
            Some(kept) => {
                replaced += usize::from(!kept.1.is_empty());
                kept.1 = f;
            }
            None => hidden.class_fills.push((class, f)),
        }
    }
    if symbols {
        if stack_shown {
            replaced += usize::from(!hidden.fills.is_empty());
            hidden.fills = std::mem::take(&mut l.fill_layers);
        }
        // A symbol's fill shown since: hidden again, at the opacity it was shown at.
        let mut now = 0;
        l.renderer.map_symbols(|_| now += 1);
        let matched = hidden.alphas.len() == now;
        let mut i = 0;
        l.renderer.map_symbols(|sym| {
            if sym.fill.a() > 0 {
                if let Some(a) = hidden.alphas.get_mut(i).filter(|_| matched) {
                    *a = sym.fill.a();
                }
                sym.fill = sym.fill.with_alpha(0);
            }
            i += 1;
        });
    }
    Some(replaced)
}

/// What turning No Fill off drops, for a notice: the fills and opacities it hid, which Restore
/// Fill would put back. `None` when it hid none.
fn dropped_by_no_fill(l: &rustgis_map::Layer) -> Option<String> {
    let hidden = l.hidden_fills.as_ref()?;
    let shown_opacities = hidden.alphas.iter().filter(|a| **a > 0).count();
    let n = hidden.class_fills.len() + usize::from(!hidden.fills.is_empty()) + shown_opacities;
    (n > 0).then(|| format!("{}: turning No Fill off drops {n} hidden fill(s) that Restore Fill can't put back", l.name))
}

/// Says what [`dropped_by_no_fill`] found, if anything.
fn notify_dropped(s: &mut Session, dropped: Option<String>) {
    if let Some(text) = dropped {
        s.notify(text, false);
    }
}

/// Turns No Fill off for a command that gives the layer a new renderer, whose own fills are drawn:
/// what it hid no longer matches, so it is dropped. The notice for that, if anything was hidden.
pub(crate) fn end_no_fill_for_new_renderer(l: &mut rustgis_map::Layer) -> Option<String> {
    let dropped = dropped_by_no_fill(l);
    take_no_fill(l);
    dropped
}

/// Turns No Fill off, dropping its override if it laid one, and says how it was on. The caller
/// shows the symbols' fills on the other path, where only it knows which.
fn take_no_fill(l: &mut rustgis_map::Layer) -> Option<NoFill> {
    l.hidden_fills = None;
    let hidden = l.no_fill.take();
    if hidden == Some(NoFill::Override) && is_no_fill_override(&l.fill_layers) {
        l.fill_layers.clear();
    }
    hidden
}

/// Whether the fills are No Fill's own, rather than a transparent fill chosen with `layer.fillLayers`.
fn is_no_fill_override(fills: &[rustgis_map::fills::FillLayer]) -> bool {
    let o = no_fill_override();
    matches!(fills, [f] if matches!(f.pattern, rustgis_map::fills::FillPattern::Solid) && f.color == o.color)
}

/// The symbol of one class, for its own fills.
fn class_symbol<'a>(r: &'a mut Renderer, class: &ClassRef) -> Option<&'a mut rustgis_map::Symbol> {
    match (r, class) {
        (Renderer::Unique { classes, .. }, ClassRef::Value(v)) => classes.iter_mut().find(|(k, _)| k == v).map(|(_, s)| s),
        (Renderer::Unique { other, .. }, ClassRef::Other) => Some(other),
        (Renderer::Graduated { symbols, .. }, ClassRef::Index(i)) => symbols.get_mut(*i),
        _ => None,
    }
}

/// A symbol's fill No Fill made transparent, shown again in its own colour.
pub(super) fn show_fill(sym: &mut rustgis_map::Symbol) {
    if sym.fill.a() == 0 {
        sym.fill = sym.fill.with_alpha(shown_fill_alpha());
    }
}

/// Restore Fill: what No Fill hid comes back from what it kept, except what was set since: the
/// layer's own fills and each class's where it has none now, or, on the symbols path, each fill
/// still transparent at the opacity it had. Says what it couldn't put back, as when No Fill was
/// turned on by a RustGIS that kept nothing.
fn restore_fill(l: &mut rustgis_map::Layer) -> Result<Vec<String>, String> {
    let hidden = l.hidden_fills.take();
    let kept = hidden.as_ref().is_some_and(|h| !h.partial);
    let HiddenFills { alphas, fills, class_fills, .. } = hidden.unwrap_or_default();
    let mut notes = Vec::new();
    let mode = take_no_fill(l);
    if mode.is_some() {
        let mut kept_own = 0;
        for (class, f) in class_fills {
            // A class gone since, or given fills of its own, stays as it is.
            if l.renderer.class_fills(&class).is_ok_and(<[_]>::is_empty) {
                l.renderer.set_class_fills(&class, f)?;
            } else {
                kept_own += 1;
            }
        }
        if kept_own > 0 {
            notes.push(format!(
                "{}: Restore Fill didn't put back the fills No Fill hid for {kept_own} class(es), as each has fills of its own now or is gone",
                l.name
            ));
        }
    }
    match mode {
        Some(NoFill::Override) => {
            put_back_layer_fills(l, fills, &mut notes);
            if !kept {
                // Whether there were any isn't known either.
                notes.push(format!(
                    "{}: Restore Fill couldn't put back fill layers the layer or its classes may have had before No Fill, as the project didn't keep them; add any again with Fill Layers",
                    l.name
                ));
            }
        }
        Some(NoFill::Symbols) => {
            // The layer's own fill layers, hidden by a No Fill on again, come back where none are set.
            put_back_layer_fills(l, fills, &mut notes);
            let mut now = 0;
            l.renderer.map_symbols(|_| now += 1);
            // Kept for other symbols (classes changed since), or none: a new polygon symbol's.
            let changed_since = kept && alphas.len() != now;
            let mut was = if alphas.len() == now { alphas } else { Vec::new() }.into_iter();
            let mut guessed = 0;
            l.renderer.map_symbols(|s| {
                let a = was.next();
                if s.fill.a() == 0 {
                    guessed += usize::from(a.is_none());
                    s.fill = s.fill.with_alpha(a.unwrap_or_else(shown_fill_alpha));
                }
            });
            if guessed > 0 {
                let why = if changed_since {
                    "the layer's classes changed since No Fill, so the opacities it kept don't match them"
                } else {
                    // Kept by none of the No Fills: from before the first (an earlier RustGIS's),
                    // or shown and hidden again since.
                    "the opacity they had before No Fill hid them wasn't kept"
                };
                notes.push(format!(
                    "{}: Restore Fill shows {guessed} fill(s) at the default opacity ({}), as {why}",
                    l.name,
                    shown_fill_alpha()
                ));
            }
        }
        None => {}
    }
    Ok(notes)
}

/// The layer's renderer with what No Fill hid put back, as Restore Fill would: a style saved from
/// the layer keeps its fills (No Fill is a way of drawing the layer, not part of its symbology).
/// Also how many it couldn't put back: class stacks (the class has fills of its own now, or is
/// gone) and symbols' opacities (not kept, or kept for other classes), for a notice.
pub(crate) fn renderer_with_fills(l: &rustgis_map::Layer) -> (Renderer, usize) {
    let mut r = l.renderer.clone();
    if l.no_fill.is_none() {
        return (r, 0);
    }
    let mut left_out = 0;
    let (class_fills, alphas) = match &l.hidden_fills {
        Some(h) => (h.class_fills.as_slice(), h.alphas.as_slice()),
        None => (&[][..], &[][..]),
    };
    for (class, f) in class_fills {
        let placed = r.class_fills(class).is_ok_and(<[_]>::is_empty) && r.set_class_fills(class, f.clone()).is_ok();
        left_out += usize::from(!placed);
    }
    if l.no_fill == Some(NoFill::Symbols) {
        let mut now = 0;
        r.map_symbols(|_| now += 1);
        let mut was = alphas.iter().filter(|_| alphas.len() == now);
        r.map_symbols(|s| {
            let a = was.next();
            if s.fill.a() == 0 {
                match a {
                    Some(a) => s.fill = s.fill.with_alpha(*a),
                    None => left_out += 1,
                }
            }
        });
    }
    (r, left_out)
}

/// The opacity a colour swatch shows for a fill No Fill made transparent: the one Restore Fill
/// gives it back (the default when that wasn't kept), so that a colour picked from the swatch is
/// drawn as Restore Fill would draw it, not kept at No Fill's 0 (#2308). The layer's base symbol,
/// or `class`'s. `None` for a fill No Fill doesn't hide that way, which the swatch shows as it is.
/// It reads in place, without a copy of the renderer: the swatches ask for it every frame.
pub fn hidden_fill_opacity(l: &rustgis_map::Layer, class: Option<&ClassRef>) -> Option<u8> {
    if l.no_fill != Some(NoFill::Symbols) {
        return None;
    }
    let (at, count, now) = symbol_slot(&l.renderer, class)?;
    // A fill shown since (given a colour of its own while No Fill was on) is drawn as it is.
    if now > 0 {
        return None;
    }
    // Kept only while they still match the symbols, as Restore Fill needs.
    let kept = l.hidden_fills.as_ref().filter(|h| h.alphas.len() == count).and_then(|h| h.alphas.get(at).copied());
    Some(kept.unwrap_or_else(shown_fill_alpha))
}

/// Where the layer's base symbol (as [`Renderer::base_symbol`] picks it), or `class`'s, comes
/// among [`Renderer::map_symbols`]', which is the order No Fill keeps their opacities in; how many
/// symbols that walks; and the symbol's fill opacity now.
fn symbol_slot(r: &Renderer, class: Option<&ClassRef>) -> Option<(usize, usize, u8)> {
    let at = match (r, class) {
        (_, Some(c)) => symbol_position(r, c)?,
        (Renderer::Graduated { symbols, .. }, None) => symbols.len().checked_sub(1)?,
        (_, None) => 0,
    };
    let (symbol, count) = match r {
        Renderer::Single(s) | Renderer::Proportional { base: s, .. } => ((at == 0).then_some(s), 1),
        Renderer::Unique { classes, other, .. } => {
            (if at == classes.len() { Some(other) } else { classes.get(at).map(|(_, s)| s) }, classes.len() + 1)
        }
        Renderer::Graduated { symbols, .. } => (symbols.get(at), symbols.len()),
    };
    Some((at, count, symbol?.fill.a()))
}

/// The layer's own fill layers No Fill hid come back unless it has fill layers of its own now;
/// those stay, and a note says the hidden ones weren't put back.
fn put_back_layer_fills(l: &mut rustgis_map::Layer, fills: Vec<rustgis_map::fills::FillLayer>, notes: &mut Vec<String>) {
    if l.fill_layers.is_empty() {
        l.fill_layers = fills;
    } else if !fills.is_empty() {
        notes.push(format!(
            "{}: Restore Fill didn't put back the layer's {} fill layer(s) No Fill hid, as the layer has fill layers of its own now",
            l.name,
            fills.len()
        ));
    }
}

/// Where a class's symbol comes among [`Renderer::map_symbols`]' (as [`class_symbol`] finds it).
fn symbol_position(r: &Renderer, class: &ClassRef) -> Option<usize> {
    match (r, class) {
        (Renderer::Unique { classes, .. }, ClassRef::Value(v)) => classes.iter().position(|(k, _)| k == v),
        (Renderer::Unique { classes, .. }, ClassRef::Other) => Some(classes.len()),
        (Renderer::Graduated { symbols, .. }, ClassRef::Index(i)) => (*i < symbols.len()).then_some(*i),
        _ => None,
    }
}

/// "fills" or "strokes" when the layer's own stack (`fill_layers` of a polygon layer, `line_strokes` of a
/// line or point layer), which the classes without a stack of their own are drawn from, hides the
/// symbol's colors, which a color scheme sets ([`rustgis_map::symbology::stack_hides_symbol`]).
fn layer_stack_hiding_symbol(l: &rustgis_map::Layer, g: GeometryType) -> Option<&'static str> {
    rustgis_map::symbology::stack_hides_symbol(g, &l.fill_layers, &l.line_strokes).then_some(if g == GeometryType::Polygon {
        "fills"
    } else {
        "strokes"
    })
}

/// `layer.fillLayers`: a polygon layer's stacked fills (or a class's own, with `value`, `index`
/// or `other`), bottom first; `[]` goes back to the symbol's single fill.
fn fill_layers(s: &mut Session, p: &Value) -> CmdResult {
    use rustgis_map::fills::{FillLayer, FillPattern, GradientPoint, GradientShape, MAX_FILLS, Spread, Stop, Stops};
    let class = class_ref(p)?;
    let list = p.get("fills").and_then(Value::as_array).ok_or("fills is a list of fills ([] for the symbol's single fill)")?;
    if list.len() > MAX_FILLS {
        return Err(format!("at most {MAX_FILLS} fills"));
    }
    // Refused before the fills are read: a call on the wrong layer or class keeps none of their
    // images.
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    if !l.has_geometry(GeometryType::Polygon) || l.graphics || l.is_raster() {
        return Err("this works on layers containing polygons".into());
    }
    if let Some(c) = &class {
        l.renderer.class_fills(c)?;
    }
    let (mut fills, mut new) = (Vec::with_capacity(list.len()), Vec::new());
    for (i, v) in list.iter().enumerate() {
        let n = i + 1;
        let o = v.as_object().ok_or_else(|| format!("fill {n} is not an object"))?;
        let hex = |k: &str| -> Result<Option<Rgba>, String> {
            match o.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(c)) => Rgba::from_hex(c).map(Some).ok_or_else(|| format!("fill {n}: {k} must be #RRGGBB or #RRGGBBAA")),
                Some(_) => Err(format!("fill {n}: {k} must be #RRGGBB or #RRGGBBAA")),
            }
        };
        // Brought into range before narrowing, so 1e300 is the largest, not infinity.
        let number = |v: &Value| v.as_f64().filter(|x| x.is_finite()).map(|x| x.clamp(-1e6, 1e6) as f32);
        let num = |k: &str, default: f32| -> Result<f32, String> {
            match o.get(k) {
                None | Some(Value::Null) => Ok(default),
                Some(v) => number(v).ok_or_else(|| format!("fill {n}: {k} is a number")),
            }
        };
        // A number for both ways, or [across, down].
        let pair = |k: &str, default: [f32; 2]| -> Result<[f32; 2], String> {
            match o.get(k) {
                None | Some(Value::Null) => Ok(default),
                Some(Value::Array(a)) => match a.as_slice() {
                    [x, y] => match (number(x), number(y)) {
                        (Some(x), Some(y)) => Ok([x, y]),
                        _ => Err(format!("fill {n}: {k} is a number or [across, down]")),
                    },
                    _ => Err(format!("fill {n}: {k} is a number or [across, down]")),
                },
                Some(v) => number(v).map(|x| [x, x]).ok_or_else(|| format!("fill {n}: {k} is a number or [across, down]")),
            }
        };
        not_empty(o.get("image")).map_err(|e| format!("fill {n}: {e}"))?;
        // An image is a dots fill's marker: another pattern would drop it.
        if o.get("image").is_some_and(|v| !v.is_null()) && o.get("pattern").and_then(Value::as_str) != Some("dots") {
            return Err(format!("fill {n}: an image is a dots fill's marker"));
        }
        let pattern = match o.get("pattern").filter(|v| !v.is_null()).map(|v| v.as_str()) {
            None | Some(Some("solid")) => FillPattern::Solid,
            Some(Some("hatch")) => FillPattern::Hatch {
                angle: num("angle", 0.0)?,
                spacing: num("spacing", 8.0)?,
                width: num("width", 1.0)?,
                offset: num("offset", 0.0)?,
            },
            Some(Some("dots")) => {
                let image = o.get("image").is_some_and(|v| !v.is_null());
                let marker = match o.get("marker") {
                    None | Some(Value::Null) => Marker::Circle,
                    // Its `image` says which.
                    Some(Value::String(m)) if m.trim().eq_ignore_ascii_case("image") => {
                        if !image {
                            return Err(format!("fill {n}: an image marker needs its image"));
                        }
                        Marker::Circle
                    }
                    Some(Value::String(m)) => Marker::ALL
                        .into_iter()
                        .find(|s| s.label().eq_ignore_ascii_case(m.trim()))
                        .ok_or_else(|| format!("fill {n}: unknown marker '{m}'"))?,
                    Some(_) => {
                        return Err(format!(
                            "fill {n}: marker is \"circle\", \"square\", \"triangle\", \"diamond\", \"star\" or \"image\""
                        ));
                    }
                };
                image_alone(o, "marker").map_err(|e| format!("fill {n}: {e}"))?;
                // A character of a font instead of a shape, as `layer.marker` takes it.
                let marker = match character(v).map_err(|e| format!("fill {n}: {e}"))? {
                    Some(_) if o.get("marker").is_some_and(|m| !m.is_null()) => {
                        return Err(format!("fill {n}: give a marker or a char, not both"));
                    }
                    Some(c) => Marker::Char(Glyph::new(c, o.get("font").and_then(Value::as_str).unwrap_or_default())),
                    None => match picture(v, &mut new).map_err(|e| format!("fill {n}: {e}"))? {
                        Some(img) => Marker::Picture(img),
                        None => marker,
                    },
                };
                FillPattern::Dots {
                    marker,
                    size: num("size", 3.0)?,
                    outline: hex("outline")?,
                    outline_width: num("outlineWidth", 0.0)?,
                    spacing: pair("spacing", [8.0, 8.0])?,
                    displacement: pair("displacement", [0.0, 0.0])?,
                    offset: pair("offset", [0.0, 0.0])?,
                }
            }
            Some(Some(kind @ ("gradient" | "shapeburst"))) => {
                let color2 = hex("color2")?.unwrap_or(Rgba::rgb(255, 255, 255));
                let stops = match o.get("stops") {
                    None | Some(Value::Null) => Stops::default(),
                    Some(Value::Array(a)) if a.len() <= rustgis_map::fills::MAX_STOPS => {
                        let mut list = Vec::with_capacity(a.len());
                        for st in a {
                            let bad = || format!("fill {n}: each stop is [0..1, \"#RRGGBB\" or \"#RRGGBBAA\"]");
                            let [at, c] = st.as_array().map(Vec::as_slice).unwrap_or_default() else { return Err(bad()) };
                            let at = number(at).ok_or_else(bad)?;
                            let color = c.as_str().and_then(Rgba::from_hex).ok_or_else(bad)?;
                            list.push(Stop { at, color });
                        }
                        Stops::new(list)
                    }
                    Some(_) => {
                        return Err(format!("fill {n}: stops is a list of at most {} [0..1, \"#RRGGBB\"]", rustgis_map::fills::MAX_STOPS));
                    }
                };
                let flag = |k: &str| -> Result<bool, String> {
                    match o.get(k) {
                        None | Some(Value::Null) => Ok(false),
                        Some(Value::Bool(b)) => Ok(*b),
                        Some(_) => Err(format!("fill {n}: {k} is true or false")),
                    }
                };
                let discrete = flag("discrete")?;
                if kind == "shapeburst" {
                    let blur = num("blur", 0.0)?;
                    if !(0.0..=f32::from(rustgis_map::fills::MAX_BURST_BLUR)).contains(&blur) {
                        return Err(format!("fill {n}: blur is 0 to {}", rustgis_map::fills::MAX_BURST_BLUR));
                    }
                    let distance = match o.get("distance") {
                        None | Some(Value::Null) => None,
                        Some(v) => Some(
                            number(v).filter(|d| *d >= 0.0).ok_or_else(|| format!("fill {n}: distance is points (0 or more) or null"))?,
                        ),
                    };
                    FillPattern::Shapeburst {
                        color2,
                        stops,
                        discrete,
                        distance,
                        blur: blur.round() as u8,
                        ignore_rings: flag("ignoreRings")?,
                    }
                } else {
                    let point = |k: &str, default: [f32; 2]| -> Result<GradientPoint, String> {
                        match o.get(k) {
                            None | Some(Value::Null) => Ok(GradientPoint::At(default)),
                            Some(Value::String(c)) if c.eq_ignore_ascii_case("centroid") => Ok(GradientPoint::CENTROID),
                            Some(_) => pair(k, default)
                                .map(GradientPoint::At)
                                .map_err(|_| format!("fill {n}: {k} is [across, down] (0..1 in the box) or \"centroid\"")),
                        }
                    };
                    let word = |k: &str, words: &[&str]| -> Result<usize, String> {
                        match o.get(k) {
                            None | Some(Value::Null) => Ok(0),
                            Some(Value::String(w)) => words.iter().position(|x| x.eq_ignore_ascii_case(w.trim())).ok_or_else(|| {
                                format!("fill {n}: {k} is {}", words.iter().map(|w| format!("\"{w}\"")).collect::<Vec<_>>().join(", "))
                            }),
                            Some(_) => {
                                Err(format!("fill {n}: {k} is {}", words.iter().map(|w| format!("\"{w}\"")).collect::<Vec<_>>().join(", ")))
                            }
                        }
                    };
                    let shape = [GradientShape::Linear, GradientShape::Radial, GradientShape::Conical]
                        .get(word("shape", &["linear", "radial", "conical"])?)
                        .copied()
                        .unwrap_or_default();
                    let spread = [Spread::Pad, Spread::Reflect, Spread::Repeat]
                        .get(word("spread", &["pad", "reflect", "repeat"])?)
                        .copied()
                        .unwrap_or_default();
                    FillPattern::Gradient {
                        shape,
                        color2,
                        stops,
                        discrete,
                        from: point("from", [0.5, 0.0])?,
                        to: point("to", [0.5, 1.0])?,
                        spread,
                        angle: num("angle", 0.0)?,
                        in_view: flag("inView")?,
                    }
                }
            }
            Some(_) => return Err(format!("fill {n}: pattern is \"solid\", \"hatch\", \"dots\", \"gradient\" or \"shapeburst\"")),
        };
        fills.push(FillLayer { color: hex("color")?, pattern }.clamped());
    }
    // Every fill checked: their new images are kept.
    let slots = fills.iter_mut().filter_map(|f: &mut FillLayer| match &mut f.pattern {
        FillPattern::Dots { marker: Marker::Picture(p), .. } => Some(p),
        _ => None,
    });
    keep_pictures(slots, new)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    // A fill in the symbol's colour, or the symbol's own fill ([]), asked for after No Fill: the
    // symbols' fills are shown again, or they would draw nothing (#1301). A class's fills show
    // that class's alone.
    let in_symbol_colour = fills.is_empty() || fills.iter().any(|f| f.color.is_none());
    let hidden = l.no_fill;
    let count = fills.len();
    let mut dropped = None;
    match class {
        Some(c) => {
            // Without fills of its own the class takes the layer's, which No Fill's override
            // hides: a solid fill in the symbol's colour instead.
            let fills =
                if fills.is_empty() && hidden == Some(NoFill::Override) { vec![rustgis_map::fills::FillLayer::default()] } else { fills };
            l.renderer.set_class_fills(&c, fills)?;
            if in_symbol_colour && hidden == Some(NoFill::Symbols) {
                class_symbol(&mut l.renderer, &c).into_iter().for_each(show_fill);
            }
        }
        None => {
            if in_symbol_colour && hidden == Some(NoFill::Symbols) {
                l.renderer.map_symbols(show_fill);
                dropped = dropped_by_no_fill(l);
                take_no_fill(l);
            }
            // Fills set here replace No Fill's own, even transparent ones.
            if hidden == Some(NoFill::Override) {
                dropped = dropped_by_no_fill(l);
                take_no_fill(l);
            }
            l.fill_layers = fills;
        }
    }
    notify_dropped(s, dropped);
    Ok(json!({"fills": count}))
}

/// A character marker's `char` (its first character), if given.
fn character(p: &Value) -> Result<Option<char>, String> {
    match p.get("char") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(c)) => c.chars().next().map(Some).ok_or_else(|| "char is a character".into()),
        Some(_) => Err("char is a character".into()),
    }
}

/// A line marker's or a dot fill's `image` named with a shape, a character or a hash mark under
/// `key` (`"mark"`, `"marker"`), or with a `char`: refused before the image is kept.
fn image_alone(o: &serde_json::Map<String, Value>, key: &str) -> Result<(), String> {
    not_empty(o.get("image"))?;
    let other = o.get("char").is_some_and(|c| !c.is_null())
        || o.get(key).and_then(Value::as_str).is_some_and(|m| !m.trim().eq_ignore_ascii_case("image"));
    if o.get("image").is_some_and(|v| !v.is_null()) && other {
        return Err(format!("give a {key}, a char or an image, only one"));
    }
    Ok(())
}

/// An `image` given as `""` (or blanks): refused as empty, before anything else it's named with
/// is weighed against it.
fn not_empty(image: Option<&Value>) -> Result<(), String> {
    if image.and_then(Value::as_str).is_some_and(|b64| b64.trim().is_empty()) {
        return Err("image is empty: give a PNG, JPEG or SVG image as base64, or leave it out".into());
    }
    Ok(())
}

/// An `image` (`layer.marker`'s, a line marker's, a dot fill's): a PNG, JPEG or SVG image as
/// base64, which must decode. One kept already is returned where it draws; a new one is only
/// checked, its base64 put in `new` and an empty picture returned in its place, for
/// [`keep_pictures`] to keep once the whole call is checked: a call refused for a later stroke or
/// fill keeps none of its images.
fn picture<'a>(p: &'a Value, new: &mut Vec<&'a str>) -> Result<Option<Picture>, String> {
    const WHAT: &str = "image is a PNG, JPEG or SVG image as base64, of at most 2048 × 2048 pixels or as much to draw";
    let b64 = match p.get("image") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(b64)) => b64.trim(),
        Some(_) => return Err(WHAT.into()),
    };
    if b64.len() > rustgis_map::symbology::MAX_PICTURE_BYTES {
        return Err(format!("image is at most {} MiB of base64", rustgis_map::symbology::MAX_PICTURE_BYTES >> 20));
    }
    // Checking decodes an image once for the run: one given again (the editor sends a stroke's or
    // a fill's image with every change, a slider every frame) is found decoded. One met earlier in
    // the call was checked then.
    let refused = |why: rustgis_render::MarkerRefusal| format!("{WHAT}: this one {}", why.what());
    if let Some(kept) = Picture::kept(b64) {
        // One refused only for what the run has spent is kept as it is, drawn as a circle until a
        // restart, so an edit that sends it again (a script's) isn't refused for it.
        return match rustgis_render::picture_marker_kept_refusal(kept) {
            Some(why) => Err(refused(why)),
            None => Ok(Some(kept)),
        };
    }
    if !new.contains(&b64) {
        rustgis_render::picture_marker_check(b64).map_err(refused)?;
    }
    new.push(b64);
    Ok(Some(Picture::default()))
}

/// Keeps the images [`picture`] checked (`new`, in the order it met them) in the empty pictures it
/// left in their places (`slots`, in the same order): all of them, or none when the session can't
/// keep them all.
fn keep_pictures<'a>(slots: impl Iterator<Item = &'a mut Picture>, new: Vec<&str>) -> Result<(), String> {
    if new.is_empty() {
        return Ok(());
    }
    let kept = Picture::new_all(&new).ok_or("no more images can be kept this session: restart RustGIS to use another")?;
    for (slot, p) in slots.filter(|p| p.is_default()).zip(kept) {
        *slot = p;
    }
    Ok(())
}

/// A stroke's `markers`: what it draws along the line instead of a line, and where.
fn line_markers<'a>(v: &'a Value, new: &mut Vec<&'a str>) -> Result<LineMarkers, String> {
    let o = v.as_object().ok_or("an object, such as {\"mark\": \"circle\", \"interval\": 12}")?;
    let num = |k: &str| -> Result<Option<f32>, String> {
        match o.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => {
                v.as_f64().filter(|x| x.is_finite()).map(|x| Some(x.clamp(-1e6, 1e6) as f32)).ok_or_else(|| format!("{k} is a number"))
            }
        }
    };
    let d = LineMarkers::default();
    let image = o.get("image").is_some_and(|v| !v.is_null());
    let mark = match o.get("mark") {
        None | Some(Value::Null) => d.mark,
        Some(Value::String(m)) if m.trim().eq_ignore_ascii_case("hash") => LineMark::Hash,
        // Its `image` says which.
        Some(Value::String(m)) if m.trim().eq_ignore_ascii_case("image") => {
            if !image {
                return Err("an image mark needs its image".into());
            }
            d.mark
        }
        // Its `char` says which.
        Some(Value::String(m)) if m.trim().eq_ignore_ascii_case("character") => {
            if character(v)?.is_none() {
                return Err("a character mark needs its char".into());
            }
            d.mark
        }
        Some(Value::String(m)) => LineMark::Marker(
            Marker::ALL.into_iter().find(|s| s.label().eq_ignore_ascii_case(m.trim())).ok_or_else(|| format!("unknown mark '{m}'"))?,
        ),
        Some(_) => {
            return Err("mark is \"circle\", \"square\", \"triangle\", \"diamond\", \"star\", \"character\", \"image\" or \"hash\"".into());
        }
    };
    image_alone(o, "mark")?;
    // A character of a font instead of a shape.
    let mark = match (character(v)?, o.get("font")) {
        (Some(_), _) if mark == LineMark::Hash => return Err("hash marks take no char".into()),
        // As `layer.marker`: a shape named with a char is a mistake, not the char.
        (Some(_), _) if o.get("mark").and_then(Value::as_str).is_some_and(|m| !m.trim().eq_ignore_ascii_case("character")) => {
            return Err("give a shape or a char, not both".into());
        }
        (Some(c), font) => LineMark::Marker(Marker::Char(Glyph::new(c, font.and_then(Value::as_str).unwrap_or_default()))),
        (None, _) => picture(v, new)?.map_or(mark, |img| LineMark::Marker(Marker::Picture(img))),
    };
    let bad_at = || format!("at is a placement or a list of them: {}", Placements::all_codes());
    let at = match o.get("at") {
        None | Some(Value::Null) => d.at,
        Some(Value::String(c)) => Placements::from_code(c).ok_or_else(bad_at)?,
        Some(Value::Array(a)) if !a.is_empty() && a.len() <= 16 => a
            .iter()
            .map(|c| c.as_str().and_then(Placements::from_code).ok_or_else(bad_at))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .fold(Placements::NONE, Placements::with),
        Some(_) => return Err(bad_at()),
    };
    let rotate = match o.get("rotate") {
        None | Some(Value::Null) => d.rotate,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("rotate is true or false".into()),
    };
    let outline = match o.get("outline") {
        None | Some(Value::Null) => None,
        Some(Value::String(c)) => Some(Rgba::from_hex(c).ok_or("outline must be #RRGGBB or #RRGGBBAA")?),
        Some(_) => return Err("outline must be #RRGGBB or #RRGGBBAA".into()),
    };
    Ok(LineMarkers {
        mark,
        size: num("size")?.unwrap_or(d.size),
        at,
        interval: num("interval")?.unwrap_or(d.interval),
        offset_along: num("offsetAlong")?.unwrap_or(d.offset_along),
        rotate,
        average_angle: num("averageAngle")?.unwrap_or(d.average_angle),
        angle: num("angle")?.unwrap_or(d.angle),
        outline,
    }
    .clamped())
}

pub(crate) fn s_req<'a>(p: &'a Value, k: &str) -> Result<&'a str, String> {
    ps(p, k).ok_or_else(|| format!("{k} is required"))
}

/// Applies `f` to the layer named by `p` (default: active layer).
pub(crate) fn with(s: &mut Session, p: &Value, f: impl FnOnce(&mut rustgis_map::Layer)) -> CmdResult {
    let id = s.layer_id(p)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    f(l);
    Ok(Value::Null)
}

/// Adds a dataset to the active map, selects it and returns its id.
pub fn add_dataset(s: &mut Session, mut ds: rustgis_data::Dataset) -> u32 {
    // Unique layer names keep name-based parameters unambiguous.
    let base = ds.name.clone();
    let mut i = 1;
    while s.map().layer_by_name(&ds.name).is_some() {
        i += 1;
        ds.name = format!("{base} ({i})");
    }
    let id = s.map_mut().add_dataset(ds);
    s.active_layer = Some(id);
    id
}

/// The photo layer single photos join (added by Add Data, dropped or uploaded one at a time).
pub(crate) const PHOTOS: &str = "Photos";

/// Adds photos as points: to the photo layer named `name` when there is one, else to a new one.
/// `feed` hands the reader one photo at a time, so only one file's bytes are held at once; the
/// reader stops taking photos when the layer is full. Photos without a GPS position are left out
/// and named in a notice. The map zooms to the photos added when it was empty, or when `zoom` is
/// `Some(true)`; `Some(false)` keeps the view.
pub(crate) fn add_photos_to(s: &mut Session, name: &str, zoom: Option<bool>, feed: impl FnOnce(&mut photo::Reader)) -> CmdResult {
    let empty_before = super::map_is_empty(s);
    let existing = s.map().layers.iter().find(|l| l.name == name && photo::is_photo_layer(&l.dataset)).map(|l| l.id);
    let held = existing.and_then(|id| s.map().layer(id)).map_or(0, |l| l.dataset.features.len());
    let room = photo::MAX_PHOTOS.saturating_sub(held);
    let mut reader = photo::Reader::new(name, room);
    feed(&mut reader);
    let (ds, report) = reader.finish();
    for m in &report.messages {
        s.say(m.clone());
    }
    if ds.features.is_empty() {
        return Err(report.messages.first().map(|m| m.text.clone()).unwrap_or_else(|| "no photo with a GPS position".into()));
    }
    // The reader took no more than there is room for; `take` keeps that so if it ever changes.
    let added = ds.features.len().min(room);
    let own =
        ds.features.iter().take(added).filter_map(|f| f.geometry.as_ref()).fold(rustgis_geom::Extent::EMPTY, |e, g| e.union(&g.extent()));
    let extent = ds.extent_to_lonlat(&own);
    let id = match existing {
        Some(id) => {
            let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
            // Read in longitude/latitude, held in the layer's system: through WGS 84, as a photo's
            // position is data, not a place on the map.
            let mut photos = Vec::new();
            let to = rustgis_data::raster::Crs::LonLat.to(l.dataset.coord_crs()).map_err(|e| format!("{}: {e}", l.name))?;
            let none = rustgis_data::HeldPlaces::default();
            for f in ds.features.into_iter().take(added) {
                let g = f
                    .geometry
                    .map(|g| rustgis_data::Dataset::edit_from_lonlat_by(&to, g, &none))
                    .transpose()
                    .map_err(|e| format!("{}: {e}", l.name))?;
                photos.push((g, f.attrs));
            }
            for (g, attrs) in photos {
                l.dataset.push(g, attrs);
            }
            l.touch_geometry();
            s.active_layer = Some(id);
            id
        }
        None => add_dataset(s, ds),
    };
    let mut out = json!({"layer": id, "photos": added, "skipped": report.skipped});
    // Framed on the photos this added, `zoomed` says whether the view moved to them.
    if zoom.unwrap_or(empty_before) {
        out["zoomed"] = json!(s.zoom_to(&extent));
    }
    Ok(out)
}

/// The most folder entries `layer.addPhotos` lists (it keeps the first photos by name).
#[cfg(not(target_arch = "wasm32"))]
const MAX_LISTED: usize = 100_000;

/// `layer.addPhotos`: photo files (or every JPEG in a folder) as points.
fn add_photos(s: &mut Session, p: &Value) -> CmdResult {
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or(PHOTOS).to_string();
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, name);
        Err("in the web app, add photos by uploading them (Add Data)".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut paths: Vec<std::path::PathBuf> = p
            .get("paths")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(std::path::PathBuf::from)
            .collect();
        if let Some(folder) = ps(p, "folder").map(str::trim).filter(|f| !f.is_empty()) {
            let entries = std::fs::read_dir(folder).map_err(|e| format!("{folder}: {e}"))?;
            // Sorted by name before the reader takes the first photos, so which ones are kept
            // doesn't depend on the order the folder lists them in.
            let mut found: Vec<std::path::PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| photo::is_photo_name(&p.to_string_lossy()) && p.is_file())
                .take(MAX_LISTED)
                .collect();
            found.sort();
            paths.extend(found);
        }
        if paths.is_empty() {
            return Err("give \"paths\" (photo files) or a \"folder\" of photos".into());
        }
        add_photos_to(s, &name, None, |reader| {
            for (i, path) in paths.iter().enumerate() {
                if reader.is_full() {
                    reader.left_over(paths.len() - i);
                    break;
                }
                let shown = path.display().to_string();
                match std::fs::metadata(path) {
                    Ok(m) if m.len() > photo::MAX_BYTES as u64 => {
                        reader.leave_out(format!("{shown}: larger than {} MB", photo::MAX_BYTES >> 20))
                    }
                    // Read, parsed and thumbnailed, then the bytes are dropped before the next.
                    Ok(_) => match rustgis_data::provenance::read(path) {
                        Ok(bytes) => reader.add(&shown, &bytes),
                        Err(e) => reader.leave_out(format!("{shown}: {e}")),
                    },
                    Err(e) => reader.leave_out(format!("{shown}: {e}")),
                }
            }
        })
    }
}

/// A file picked inside a File Geodatabase stands for the geodatabase (file dialogs pick files,
/// not the folder): `layer.addData` adds it and `info.fileLayers` lists its layers.
fn gdb_path(path: &str) -> std::borrow::Cow<'_, str> {
    match rustgis_data::source::gdb_of(Path::new(path)) {
        Some(d) => d.display().to_string().into(),
        None => path.into(),
    }
}

/// Adds a File Geodatabase's feature classes and tables (`"layer"` picks one), read without GDAL;
/// through GDAL, when it is installed, if they can't be (compressed tables, raster fields).
#[cfg(not(target_arch = "wasm32"))]
fn add_geodatabase(s: &mut Session, p: &Value, path: &str) -> CmdResult {
    let layer = ps(p, "layer").map(str::trim).filter(|l| !l.is_empty());
    let read = match rustgis_data::filegdb::read_dir(Path::new(path), layer) {
        Ok(read) => read,
        Err(e) if rustgis_data::source::gdal::available() => {
            return add_gdal(s, p, path).map_err(|g| format!("{path}: {e}; through GDAL: {g}"));
        }
        Err(e) => return Err(format!("{path}: {e}")),
    };
    let reads =
        read.layers.into_iter().map(|(dataset, report)| rustgis_data::io::LayerRead { dataset, report, styles: Vec::new() }).collect();
    // The tables read, then those it couldn't read (compressed, raster fields), through GDAL when it
    // is installed; else, or if GDAL can't either, said. The second go on whether the first added.
    let mut results: Vec<Value> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    match add_reads(s, p, path, reads, Vec::new(), None) {
        Ok(r) => results.push(r),
        Err(e) => failed.push(e),
    }
    let gdal = rustgis_data::source::gdal::available();
    for (table, why) in read.unread {
        let mut q = p.clone();
        if let Some(o) = q.as_object_mut() {
            o.insert("layer".into(), json!(table));
            o.remove("name");
        }
        match gdal.then(|| add_gdal(s, &q, path)) {
            Some(Ok(r)) => results.push(r),
            Some(Err(g)) => s.notify(format!("{path}: {table} not read: {why}; through GDAL: {g}"), true),
            None if rustgis_data::RUNS_PROGRAMS => {
                s.notify(format!("{path}: {table} not read: {why} (GDAL, when installed, reads it)"), true)
            }
            // Installing GDAL is no help where no program runs (a phone, #1902).
            None => s.notify(
                format!("{path}: {table} not read: {why} (GDAL reads it, a program {} can't run)", rustgis_data::no_programs_here()),
                true,
            ),
        }
    }
    if results.is_empty() {
        return Err(if failed.is_empty() { format!("{path}: nothing could be added") } else { failed.join("; ") });
    }
    // Something was added (through GDAL): the layers read natively that couldn't be are still said.
    for e in failed {
        s.notify(e, true);
    }
    // One result, as adding a single file gives it: every layer, the first, and the features.
    let layers: Vec<Value> = results.iter().filter_map(|r| r.get("layers").and_then(Value::as_array)).flatten().cloned().collect();
    let tables: Vec<Value> = results.iter().filter_map(|r| r.get("tables").and_then(Value::as_array)).flatten().cloned().collect();
    let features: u64 = results.iter().filter_map(|r| r.get("features").and_then(Value::as_u64)).sum();
    let mut out = json!({"layers": layers, "tables": tables, "features": features});
    if let (Some(o), Some(first)) = (out.as_object_mut(), layers.first()) {
        o.insert("layer".into(), first.clone());
    }
    Ok(out)
}

/// Opens a file through its format's reader. A Shapefile CRS override is applied before any
/// conversion in both whole-file and streamed paths; the source file is never rewritten.
/// DBF picks with geometry follow the full Shapefile import path.
fn add_data(s: &mut Session, p: &Value) -> CmdResult {
    let path = s_req(p, "path")?;
    #[cfg(not(target_arch = "wasm32"))]
    if Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("dbf")) {
        if let Some(read) = super::reads::take::<WholeRead>(s, &dbf_shapefile_key(path)) {
            if let Some(crs) = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty()) {
                super::parse_crs(s, crs)?;
            }
            return add_read(s, p, read?, None);
        }
        // A standalone table already read stays a table if a companion appears meanwhile.
        if !super::reads::placed_for(s, &whole_key(path))
            && let Some(shp) = rustgis_data::io::shapefile_for_dbf(Path::new(path))
        {
            let mut q = p.clone();
            q["path"] = json!(shp.display().to_string());
            return add_data(s, &q);
        }
    }
    let path = &*gdb_path(path);
    // `"as": "scan"` opens a TIFF, PNG or JPEG as a picture with no coordinates, a photo too.
    let scan = super::raster::wants_scan(p, path)?;
    // A photo: a point in the photo layer (one without a GPS position is named and left out). In the
    // browser `add_photos` asks for an upload, so a JPEG isn't taken for a scan there either. A
    // JPEG with a world file beside it (`.jgw`) is a picture placed by it, a raster (#2075).
    if !scan && rustgis_data::photo::is_photo_name(path) && !rustgis_data::raster::world::places(Path::new(path)) {
        let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or(PHOTOS).to_string();
        return add_photos(s, &json!({"paths": [path], "name": name}));
    }
    if rustgis_data::raster::is_raster_name(path) {
        let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty());
        let page = super::raster::pdf_page(path, p)?;
        // An SRTM tile or DTED file has no subdataset or coordinate system to choose: said.
        let ignored = rustgis_data::raster::elevation_ignores(path, ps(p, "layer"), ps(p, "crs"));
        return super::raster::add_path(s, path, name, page.as_deref(), scan, ignored);
    }
    // A Zarr store's folder: its first variable, or the one "layer" names.
    #[cfg(not(target_arch = "wasm32"))]
    if rustgis_data::raster::zarr::is_store(Path::new(path)) {
        let mut q = json!({"path": path});
        for (from, to) in [("layer", "variable"), ("name", "name"), ("crs", "crs")] {
            if let Some(v) = p.get(from) {
                q[to] = v.clone();
            }
        }
        return super::zarr::add(s, &q);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if rustgis_data::raster::gdal::is_gdal_raster_name(path) {
        return super::raster::add_gdal_path(s, p, path);
    }
    if path.to_ascii_lowercase().ends_with(".rgislyr") {
        return add_layer_file(s, path);
    }
    if rustgis_data::source::is_source_name(path) {
        return add_source(s, p, Origin::Path(path), None);
    }
    // Only a possible city model is read whole; ordinary GeoJSON keeps its streaming path.
    if path.to_ascii_lowercase().ends_with(".json")
        && !rustgis_data::cityjson::is_cityjson_name(path)
        && ["crs", "layer"].iter().any(|k| ps(p, k).is_some_and(|c| !c.trim().is_empty()))
        && std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= rustgis_data::io::BYTES_FLOOR)
        && may_be_city_model_path(path)
    {
        let bytes = rustgis_data::provenance::read(path).map_err(|e| format!("{path}: {e}"))?;
        if rustgis_data::io::is_cityjson(path, &bytes) {
            let full = std::path::absolute(path).map(|p| p.display().to_string()).unwrap_or_else(|_| path.to_string());
            return add_layers(s, p, &full, &bytes, None, true);
        }
    }
    // A large GeoJSON FeatureCollection (any, with `"stream": true`) is read in place, streamed
    // when it holds many features; another GeoJSON document, or one that can't be read so, is
    // read whole below.
    #[cfg(not(target_arch = "wasm32"))]
    if is_geojson(path)
        && !rustgis_data::cityjson::is_cityjson_name(path)
        // One read whole ahead keeps that route, though the file grew since.
        && !super::reads::placed_for(s, &whole_key(path))
        && streams_geojson(p, path)
    {
        let name = Path::new(path).file_stem().and_then(|n| n.to_str()).unwrap_or("layer");
        // Opened ahead in the desktop app ([`in_place_ahead`]).
        let Opened { source, whole, .. } = take_opened(s, path, None).unwrap_or_else(|| {
            Opened::here(|| Ok(Box::new(rustgis_data::source::geojson::GeoJsonSource::open_path(Path::new(path), name)?)))
        });
        match source {
            Ok(src) => return add_source_read(s, p, Origin::Path(path), Some(src), whole, None),
            // Not a FeatureCollection: read whole below, on this thread (the UI thread in the
            // desktop app, as only small GeoJSON files are read ahead), so within the size every
            // computer reads, as before #2408, not the larger limit: the window isn't held longer.
            Err(rustgis_data::DataError::Unsupported(_)) => {
                let floor = rustgis_data::io::BYTES_FLOOR;
                if let Ok(m) = std::fs::metadata(path)
                    && m.len() > floor
                {
                    return Err(format!(
                        "{path}: a GeoJSON document that isn't a FeatureCollection is read whole, so at most {} MiB; it is {} MiB (a CityJSON model named .json: rename it .city.json)",
                        floor >> 20,
                        m.len().div_ceil(1 << 20)
                    ));
                }
            }
            Err(e) => return Err(format!("{path}: {e}")),
        }
    }
    // A Shapefile past what the whole-file reader takes is kept in place however few its records
    // (#1055), so it never has to fit in memory whole; "stream": false or Load All loads it.
    #[cfg(not(target_arch = "wasm32"))]
    let past_cap = past_cap_shapefile(path, p);
    #[cfg(not(target_arch = "wasm32"))]
    let p = past_cap.as_ref().unwrap_or(p);
    #[cfg(not(target_arch = "wasm32"))]
    if path.to_ascii_lowercase().ends_with(".shp")
        && let Some(crs) = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty())
    {
        let crs = super::parse_crs(s, crs)?;
        let name = Path::new(path).file_stem().and_then(|n| n.to_str()).unwrap_or("layer");
        let stream = streams_shapefile(p, path);
        if stream {
            // Opened ahead in the desktop app ([`in_place_ahead`]), in the same system.
            let Opened { source, whole, .. } = take_opened(s, path, ps(p, "crs").map(str::trim)).unwrap_or_else(|| {
                Opened::here(|| Ok(Box::new(rustgis_data::source::shp::ShpSource::open_path_in(Path::new(path), name, Some(crs))?)))
            });
            return add_source_read(s, p, Origin::Path(path), Some(source.map_err(|e| e.to_string())?), whole, None);
        }
        // Read ahead in the desktop app ([`add_data_ahead`]).
        let read = match super::reads::take(s, &whole_key(path)) {
            Some(read) => read,
            None => read_shapefile_with(path, name, crs),
        }?;
        return add_read(s, p, read, None);
    }
    // A large Shapefile is read in place, as a GeoPackage is; smaller ones are loaded whole.
    #[cfg(not(target_arch = "wasm32"))]
    if path.to_ascii_lowercase().ends_with(".shp") && streams_shapefile(p, path) {
        // Opened ahead in the desktop app ([`in_place_ahead`]).
        return match take_opened(s, path, None) {
            Some(Opened { source, whole, .. }) => {
                add_source_read(s, p, Origin::Path(path), Some(source.map_err(|e| format!("{path}: {e}"))?), whole, None)
            }
            None => add_source(s, p, Origin::Path(path), None),
        };
    }
    // A PMTiles archive is a tile layer (raster or vector, as its header says).
    if path.to_ascii_lowercase().ends_with(".pmtiles") {
        if cfg!(target_arch = "wasm32") {
            return Err(format!("{path}: the web app reads PMTiles archives from web addresses (Home › Add › Basemap › Add Tile Layer)"));
        }
        let full = std::path::absolute(path).map(|p| p.display().to_string()).map_err(|e| format!("{path}: {e}"))?;
        let mut q = json!({"url": full, "kind": "pmtiles", "top": true});
        if let Some(n) = ps(p, "name") {
            q["name"] = json!(n);
        }
        return super::tiles::add(s, &q);
    }
    // So is an MBTiles archive, read in place a tile at a time.
    if rustgis_data::mbtiles::is_mbtiles_name(path) {
        if cfg!(target_arch = "wasm32") {
            return Err(format!("{path}: in the web app, add the MBTiles file from this computer (Home › Add › Add Data)"));
        }
        let full = std::path::absolute(path).map(|p| p.display().to_string()).map_err(|e| format!("{path}: {e}"))?;
        // Refuse a promptly unreadable file now; a slow header is deferred without holding
        // the command thread past the same one-second budget used by map.addTiles.
        crate::render::rustgis_tiles::mbtiles::info_ready(&full, web_time::Duration::from_secs(1))?;
        let mut q = json!({"url": full, "kind": "mbtiles", "top": true});
        if let Some(n) = ps(p, "name") {
            q["name"] = json!(n);
        }
        return super::tiles::add_with_metadata_wait(s, &q, web_time::Duration::ZERO);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if rustgis_data::filegdb::is_geodatabase(Path::new(path)) {
        return add_geodatabase(s, p, path);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if rustgis_data::source::gdal::is_gdal_name(path) {
        return add_gdal(s, p, path);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if rustgis_data::source::duckdb::is_duckdb_name(path) {
        return add_tables(s, p, &rustgis_data::source::duckdb::Database::File(path.into()));
    }
    // A spreadsheet: one sheet ("layer"), read like delimited text.
    if rustgis_data::sheets::is_sheet_name(path) {
        let read = match super::reads::take(s, &whole_key(path)) {
            Some(read) => read,
            None => read_sheet_file(p, path),
        }?;
        return add_read(s, p, read, None);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if large_osm(path) {
        return add_gdal(s, p, path);
    }
    if rustgis_data::dxf::is_dxf_name(path) {
        let (full, reads) = match super::reads::take::<LayersRead>(s, &whole_key(path)) {
            Some(read) => read,
            None => read_dxf_file(p, path),
        }?;
        if let Some(crs) = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty()) {
            super::say_crs_note(s, crs);
        }
        return add_reads(s, p, &full, reads, Vec::new(), None);
    }
    if rustgis_data::io::is_multi_layer_name(path) {
        // A file read ahead on the desktop ([`read_ahead_whole`]): add the prepared layers without
        // reading the file again on the UI thread.
        if let Some(read) = super::reads::take::<LayersRead>(s, &layers_key(path)) {
            if let Some(crs) = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty()) {
                super::say_crs_note(s, crs);
            }
            let (full, reads) = read?;
            // City models and OSM keep the file-wide report on their first layer. A feature-service
            // PBF has only one layer, so this flag does not transfer any report for it.
            #[cfg(not(target_arch = "wasm32"))]
            let whole = is_city_model_path(path) || path.to_ascii_lowercase().ends_with(".pbf");
            #[cfg(target_arch = "wasm32")]
            let whole = !path.to_ascii_lowercase().ends_with(".zip");
            return add_picked(s, p, &full, reads, whole, Vec::new(), None);
        }
        let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        // A file the desktop app doesn't read ahead (a KML or KMZ, with its overlays; a LandXML;
        // RSS or Atom) is read here, on the UI thread there: within the size every computer reads,
        // as before #2408, so the window isn't held longer; said first, as it is the limit that
        // applies. The others ([`read_ahead_whole`]) take the larger limit: the desktop app reads
        // them ahead (`readjob`, for drops, pickers and Add Data), so this branch reads them for
        // the command line, Python, MCP and the tests, which hold no window.
        if let Some(e) = past_read_as_added(path, meta.len()) {
            return Err(e);
        }
        let max = rustgis_data::io::max_bytes();
        if meta.len() > max {
            return Err(rustgis_data::io::file_size_error(path, max));
        }
        let bytes = rustgis_data::provenance::read(path).map_err(|e| format!("{path}: {e}"))?;
        let full = std::path::absolute(path).map(|p| p.display().to_string()).unwrap_or_else(|_| path.to_string());
        return add_layers(s, p, &full, &bytes, None, true);
    }
    let read = match super::reads::take(s, &whole_key(path)) {
        Some(read) => read,
        None => read_whole(p, path),
    }?;
    add_read(s, p, read, None)
}

/// A file Add Data reads whole: the features, and what reading them said.
type WholeRead = Result<(rustgis_data::Dataset, rustgis_data::ReadReport), String>;

/// A companion Shapefile read belongs to the initiating DBF pick even if the files go away.
#[cfg(not(target_arch = "wasm32"))]
fn dbf_shapefile_key(path: &str) -> String {
    format!("dbf-shapefile:{path}")
}

/// The key Add Data's whole read of `path` is taken by (see [`super::reads`]).
fn whole_key(path: &str) -> String {
    format!("whole:{path}")
}

/// A city model's layers in the coordinate system given, over the one it names: what
/// [`add_layers`] reads, and Add Data's read ahead ([`read_layers_file`]).
fn city_reads(file_name: &str, bytes: &[u8], crs: Option<rustgis_data::raster::Crs>) -> Result<Vec<rustgis_data::io::LayerRead>, String> {
    Ok(rustgis_data::io::read_city_model(file_name, bytes, crs)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(mut dataset, report)| {
            dataset.source.get_or_insert_with(|| file_name.to_string());
            rustgis_data::io::LayerRead { dataset, report, styles: Vec::new() }
        })
        .collect())
}

/// A city model or ZIP's layers, read whole, with the full path they are read under.
type LayersRead = Result<(String, Vec<rustgis_data::io::LayerRead>), String>;

/// The key Add Data's read of the layers at `path` is taken by.
fn layers_key(path: &str) -> String {
    format!("layers:{path}")
}

/// The refusal of a file of several layers of `len` bytes that the desktop app reads as it is
/// added, on the UI thread (not [`read_ahead_whole`]: a KML or KMZ, a LandXML, RSS, Atom or
/// GeoRSS), past the size every computer reads, as before #2408, so the window isn't held longer.
/// Said before the larger limit, as it is the one that applies.
fn past_read_as_added(path: &str, len: u64) -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    let ahead = read_ahead_whole(path);
    #[cfg(target_arch = "wasm32")]
    let ahead = false;
    let floor = rustgis_data::io::BYTES_FLOOR;
    (!ahead && len > floor).then(|| rustgis_data::io::file_size_error(path, floor))
}

/// Whether the desktop app reads a file of several layers whole ahead, off the UI thread
/// ([`add_data_ahead`], [`read_layers_file`]): a ZIP, a city model, a GML, GPX or TopoJSON file,
/// an OpenStreetMap extract (`.pbf`; a large one goes to GDAL instead where it is installed, and
/// isn't read ahead, [`large_osm`]).
#[cfg(not(target_arch = "wasm32"))]
fn read_ahead_whole(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".zip", ".gml", ".gpx", ".topojson", ".pbf"].iter().any(|e| lower.ends_with(e))
        || (rustgis_data::io::is_multi_layer_name(path) && is_city_model_path(path))
}

/// Whether Add Data reads `path` as a city model's layers: a CityJSON model named `.city.json`, or
/// a `.gml` or `.xml` file whose root (in its first bytes, read here) is a CityGML city model.
#[cfg(not(target_arch = "wasm32"))]
fn is_city_model_path(path: &str) -> bool {
    use std::io::Read;
    if rustgis_data::cityjson::is_cityjson_name(path) {
        return true;
    }
    let lower = path.to_ascii_lowercase();
    if !(lower.ends_with(".gml") || lower.ends_with(".xml")) {
        return false;
    }
    let mut head = Vec::new();
    rustgis_data::provenance::open(path)
        .and_then(|f| f.take(rustgis_data::citygml::HEAD_BYTES as u64).read_to_end(&mut head))
        .is_ok_and(|_| rustgis_data::citygml::looks_like_citygml(&head))
}

/// A city model or ZIP's layers, as [`add_layers`] reads them. The desktop runs this off the
/// UI thread; a ZIP's CRS applies to Shapefiles, text tables and unnamed city models, as when read inline.
/// Reading the compressed file and inflating one ZIP member cannot be interrupted mid-read.
#[cfg(not(target_arch = "wasm32"))]
fn read_layers_file(p: &Value, path: &str) -> LayersRead {
    let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    if let Some(e) = past_read_as_added(path, meta.len()) {
        return Err(e);
    }
    let max = rustgis_data::io::max_bytes();
    if meta.len() > max {
        return Err(rustgis_data::io::file_size_error(path, max));
    }
    let bytes = rustgis_data::provenance::read(path).map_err(|e| format!("{path}: {e}"))?;
    let full = std::path::absolute(path).map(|p| p.display().to_string()).unwrap_or_else(|_| path.to_string());
    let crs = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty());
    let reads = match crs {
        Some(text) if full.to_ascii_lowercase().ends_with(".zip") => {
            rustgis_data::io::read_zip_in(&full, &bytes, Some(rustgis_data::raster::Crs::parse(text)?))
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|(dataset, report)| rustgis_data::io::LayerRead { dataset, report, styles: Vec::new() })
                .collect()
        }
        Some(text) if rustgis_data::io::is_city_model(&full, &bytes) => {
            city_reads(&full, &bytes, Some(rustgis_data::raster::Crs::parse(text)?))?
        }
        _ => rustgis_data::io::read_layers(&full, &bytes).map_err(|e| e.to_string())?,
    };
    Ok((full, reads))
}

/// A DXF drawing read whole with its import options. Parsing checks Cancel between entities;
/// decoding and the bounded file read complete before cancellation can be observed.
fn read_dxf_file(p: &Value, path: &str) -> LayersRead {
    use std::io::Read;
    let file = rustgis_data::provenance::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut bytes = Vec::new();
    let max = rustgis_data::io::max_bytes();
    // One that says it is too large is refused before any of it is read.
    if let Ok(meta) = file.metadata()
        && meta.len() > max
    {
        return Err(rustgis_data::io::file_size_error(path, max));
    }
    (&file).take(max + 1).read_to_end(&mut bytes).map_err(|e| format!("{path}: {e}"))?;
    if bytes.len() as u64 <= max {
        rustgis_data::provenance::buffer(Path::new(path), &file, &bytes);
    }
    let full = std::path::absolute(path).map(|p| p.display().to_string()).unwrap_or_else(|_| path.to_string());
    let crs = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty()).map(rustgis_data::raster::Crs::parse).transpose()?;
    Ok((full.clone(), dxf_reads(p, &full, &bytes, crs)?))
}

/// Reads drawing layers with the same limits and options for paths, uploads and text.
fn dxf_reads(
    p: &Value,
    name: &str,
    bytes: &[u8],
    crs: Option<rustgis_data::raster::Crs>,
) -> Result<Vec<rustgis_data::io::LayerRead>, String> {
    let max = rustgis_data::io::max_bytes();
    if bytes.len() as u64 > max {
        return Err(rustgis_data::io::file_size_error(name, max));
    }
    let opts = rustgis_data::dxf::Options {
        crs,
        layer: ps(p, "layer").map(str::trim).filter(|l| !l.is_empty()),
        paper_space: b(p, "paperSpace").unwrap_or(false),
        blocks: b(p, "blocks").unwrap_or(true),
    };
    Ok(rustgis_data::dxf::read(name, bytes, &opts)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(dataset, report)| rustgis_data::io::LayerRead { dataset, report, styles: Vec::new() })
        .collect())
}

/// A spreadsheet's sheet ("layer"), read like delimited text.
fn read_sheet_file(p: &Value, path: &str) -> WholeRead {
    let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    if meta.len() > super::raster::MAX_UPLOAD_BYTES as u64 {
        return Err(rustgis_data::io::file_size_error(path, super::raster::MAX_UPLOAD_BYTES as u64));
    }
    let bytes = rustgis_data::provenance::read(path).map_err(|e| format!("{path}: {e}"))?;
    // Read from disk: the table keeps the workbook's path, so an export can't replace it.
    let mut read = read_sheet(p, path, &bytes)?;
    read.0.source = Some(std::path::absolute(path).map_or_else(|_| path.to_string(), |a| a.display().to_string()));
    Ok(read)
}

/// Delimited text with the options `p` gives, or another file its format's reader reads whole.
fn read_whole(p: &Value, path: &str) -> WholeRead {
    let delimited = read_delimited(p, path, || {
        let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        // The limit the text readers keep without options too.
        let max = rustgis_data::io::max_bytes();
        if meta.len() > max {
            return Err(rustgis_data::io::file_size_error(path, max));
        }
        rustgis_data::provenance::read(path).map_err(|e| format!("{path}: {e}"))
    })?;
    if let Some(read) = delimited {
        return Ok(read);
    }
    let read = || rustgis_data::io::read_path(Path::new(path)).map_err(|e| whole_read_error(path, e));
    // A file its edits can be written back to: the files as read, to compare with before then.
    #[cfg(not(target_arch = "wasm32"))]
    if rustgis_data::sync::is_syncable(path) {
        return read_synced(path, read);
    }
    read()
}

/// A Shapefile read whole in coordinate system `crs`, which replaces its `.prj`, with the record
/// of what was read ([`read_synced`]).
#[cfg(not(target_arch = "wasm32"))]
fn read_shapefile_with(path: &str, name: &str, crs: rustgis_data::raster::Crs) -> WholeRead {
    read_synced(path, || {
        let mut read = rustgis_data::io::read_shapefile_in(Path::new(path), name, Some(crs)).map_err(|e| e.to_string())?;
        read.0.source = Some(path.to_string());
        Ok(read)
    })
}

/// `read`, the read of the file at `path`, with the record of what it read: the files hashed
/// just before ([`rustgis_data::sync::FileSync::around`]) and the digest of the rows read. Its
/// work grows with the file, so the desktop app makes it off the UI thread ([`add_data_ahead`]).
/// A large GeoJSON document that isn't a FeatureCollection, without `"stream": false`, is read
/// whole on the calling thread instead, and so kept within [`rustgis_data::io::BYTES_FLOOR`]
/// (`add_data`), as are the files of several layers that aren't read ahead.
/// When the files couldn't be hashed the layer has no record, and a note says it can't be
/// compared with its file later.
#[cfg(not(target_arch = "wasm32"))]
fn read_synced(path: &str, read: impl FnOnce() -> WholeRead) -> WholeRead {
    // A file past what the readers take (a Shapefile's `.dbf` too) is refused by the read: not
    // hashed first.
    let over = |p: &Path| std::fs::metadata(p).is_ok_and(|m| m.len() > rustgis_data::io::max_bytes());
    let shp = path.to_ascii_lowercase().ends_with(".shp");
    if over(Path::new(path)) || (shp && over(&Path::new(path).with_extension("dbf"))) {
        return read();
    }
    let (read, files) = rustgis_data::sync::FileSync::around(Path::new(path), read).map_err(|e| format!("{path}: {e}"))?;
    let (mut ds, mut report) = read?;
    // Kept whole, as the record checks it later: a relative path would be taken from wherever
    // RustGIS runs then, not where the file was read.
    if files.is_some()
        && let Ok(full) = std::path::absolute(path)
    {
        ds.source = Some(full.display().to_string());
    }
    report.synced = match files {
        Some(files) => Some(rustgis_data::sync::Synced::record(files, &ds).map_err(|e| format!("{path}: {e}"))?),
        None => {
            report.messages.push(format!(
                "{path}: a part of it couldn't be read to record it, so RustGIS can't tell later whether the file changed since; add it again to compare it"
            ));
            None
        }
    };
    Ok((ds, report))
}

/// Whether Add Data reads the Shapefile at `path` in place: as `p` says (the callers pass it with
/// [`past_cap_shapefile`] applied), else when it holds more records than
/// [`STREAM_ABOVE`](rustgis_map::source::STREAM_ABOVE).
#[cfg(not(target_arch = "wasm32"))]
fn streams_shapefile(p: &Value, path: &str) -> bool {
    b(p, "stream")
        .unwrap_or_else(|| rustgis_data::source::shp::record_count(Path::new(path)).is_some_and(|n| n > rustgis_map::source::STREAM_ABOVE))
}

/// Whether Add Data reads the GeoJSON file at `path` in place (when it is a FeatureCollection): as
/// `p` says, else when it is larger than
/// [`IN_PLACE_ABOVE`](rustgis_data::source::geojson::IN_PLACE_ABOVE).
#[cfg(not(target_arch = "wasm32"))]
fn streams_geojson(p: &Value, path: &str) -> bool {
    b(p, "stream")
        .unwrap_or_else(|| std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > rustgis_data::source::geojson::IN_PLACE_ABOVE))
}

/// Whether Add Data reads a GeoPackage table, GeoParquet or FlatGeobuf file holding `count`
/// features in place, without `"stream"`: when it holds more than
/// [`STREAM_ABOVE`](rustgis_map::source::STREAM_ABOVE), or doesn't say how many.
fn streams_by_count(count: Option<u64>) -> bool {
    count.is_none_or(|c| c > rustgis_map::source::STREAM_ABOVE)
}

/// What Add Data's question, copy a file into the project or link to it (#2350), starts from:
/// `{"linked", "copy"}`, or `None` for a file it doesn't ask about. Asked about: a `.geojson`
/// file, a Shapefile, a GeoPackage (one choice for all of `tables`), GeoParquet and FlatGeobuf.
/// Not asked: LiDAR (always linked), a `.json` (it may be a city model, decided by size as
/// before), files read whole, and those it can't open (`layer.addData` then says why).
/// `linked` is what `layer.addData` does without `"stream"`, by the same rules
/// ([`streams_geojson`], [`past_cap_shapefile`] and [`streams_shapefile`], [`streams_by_count`],
/// linked when any table would be), except a `.geojson` file: by its size alone, as reading its
/// count means parsing it, so Add Data always sends the answer (`layer.addData` unasked, once it
/// reads such a file in place, also copies one of few features). A Cancel stops it between
/// tables, with no question. `copy` is whether it can be read whole: a GeoJSON file or a
/// Shapefile (and its `.dbf`) within [`rustgis_data::io::max_bytes`]; a source's whole read is
/// bounded where it is read.
#[cfg(not(target_arch = "wasm32"))]
fn link_choice(path: &str, tables: &[String]) -> Option<Value> {
    let lower = path.to_ascii_lowercase();
    let unset = json!({});
    let fits = |p: &Path| std::fs::metadata(p).is_ok_and(|m| m.len() <= rustgis_data::io::max_bytes());
    let (linked, copy) = if lower.ends_with(".geojson") {
        (streams_geojson(&unset, path), fits(Path::new(path)))
    } else if lower.ends_with(".shp") {
        let shp = Path::new(path);
        let linked = past_cap_shapefile(path, &unset).is_some() || streams_shapefile(&unset, path);
        (linked, fits(shp) && fits(&rustgis_data::io::dbf_sidecar(shp)))
    } else if rustgis_data::source::is_source_name(path) && !lower.ends_with(".las") && !lower.ends_with(".laz") {
        let gpkg = lower.ends_with(".gpkg");
        let mut linked = false;
        for table in tables.iter().map(|t| gpkg.then_some(t.as_str())) {
            // Cancel (the chooser closed) is looked at between tables: each open counts its rows.
            rustgis_data::progress::check().ok()?;
            let src = rustgis_data::source::open_path(Path::new(path), table).ok()?;
            linked |= streams_by_count(src.info().count);
        }
        (linked, true)
    } else {
        return None;
    };
    Some(json!({"linked": linked, "copy": copy}))
}

/// A large Shapefile or GeoJSON file, read in place, as Add Data opens it ahead (#2446).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Opened {
    /// The size and modification time of the file and those beside it of its name (a
    /// Shapefile's `.dbf`, `.prj` and `.cpg`) as it was opened: an opening of files changed since
    /// isn't taken, as its features' places, boxes and fields are those of what they held then.
    stamp: Option<Vec<(std::ffi::OsString, u64, Option<std::time::SystemTime>)>>,
    source: Result<Box<dyn rustgis_data::source::FeatureSource>, rustgis_data::DataError>,
    /// Its features read whole, when Add Data loads the file instead of keeping it in place (it
    /// holds few enough, and `"stream"` isn't given), so the window doesn't wait for that read.
    pub(crate) whole: Option<rustgis_data::Result<(rustgis_data::Dataset, rustgis_data::ReadReport)>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Opened {
    /// Opens a file where the command runs (the command line, Python, or no opening made ahead).
    fn here(open: impl FnOnce() -> rustgis_data::Result<Box<dyn rustgis_data::source::FeatureSource>>) -> Self {
        Self { stamp: None, source: open(), whole: None }
    }

    /// Opens `path` on the read thread, and reads its features whole there too when `may_load`
    /// and it holds no more than [`STREAM_ABOVE`](rustgis_map::source::STREAM_ABOVE), as
    /// [`add_source_read`] then loads it rather than keeping it in place.
    fn ahead(
        path: &str,
        may_load: bool,
        open: impl FnOnce() -> rustgis_data::Result<Box<dyn rustgis_data::source::FeatureSource>>,
    ) -> Self {
        let stamp = file_stamp(path);
        let source = open();
        let whole = match &source {
            Ok(src) if may_load && !streams_by_count(src.info().count) => Some(rustgis_data::source::materialize(src.as_ref())),
            _ => None,
        };
        Self { stamp, source, whole }
    }
}

/// The size and modification time (where the file system keeps one) of `path` and of the files
/// beside it with the same name before the extension, in any case (a Shapefile's parts), by name.
/// One that names no file (a dangling link) is left out, as the opening can't read it either;
/// `None` when the folder can't be listed or another can't be looked at.
#[cfg(not(target_arch = "wasm32"))]
fn file_stamp(path: &str) -> Option<Vec<(std::ffi::OsString, u64, Option<std::time::SystemTime>)>> {
    let path = Path::new(path);
    let stem = path.file_stem()?.to_str()?;
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut stamp = Vec::new();
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        if !Path::new(&name).file_stem().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case(stem)) {
            continue;
        }
        // The file a link names: its own size and time are what change.
        match std::fs::metadata(entry.path()) {
            Ok(m) if m.is_file() => stamp.push((name, m.len(), m.modified().ok())),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    stamp.sort();
    Some(stamp)
}

/// Add Data's opening of `path` in the system `crs` names, made ahead in the desktop app
/// ([`in_place_ahead`]), while the file is as it was then; `None` otherwise (the command line,
/// Python, or a file changed since), for the command to open it itself.
#[cfg(not(target_arch = "wasm32"))]
fn take_opened(s: &mut Session, path: &str, crs: Option<&str>) -> Option<Opened> {
    let opened = super::reads::take::<Opened>(s, &in_place_key(path, crs))?;
    (opened.stamp.is_some() && opened.stamp == file_stamp(path)).then_some(opened)
}

/// The key of Add Data's opening of `path` in place, in the coordinate system `crs` names (see
/// [`super::reads`]).
#[cfg(not(target_arch = "wasm32"))]
fn in_place_key(path: &str, crs: Option<&str>) -> String {
    format!("in-place:{}", json!([path, crs]))
}

/// The opening of a large Shapefile or GeoJSON file that Add Data reads in place, made ahead on a
/// thread of its own (#2446): it reads the whole file once, to index a Shapefile's records or
/// find a GeoJSON file's features, which takes minutes for a file of millions. As [`add_data`]
/// decides ([`streams_geojson`], [`streams_shapefile`]); a possible city model is sniffed from
/// its head and left for the city-model path.
/// Cancel is looked at every few thousand features or records; reading a GeoJSON file of at most
/// 256 MiB into memory, before its text is scanned, is one step without a look.
#[cfg(not(target_arch = "wasm32"))]
fn in_place_ahead(p: &Value, path: &str) -> Option<super::reads::ReadWork> {
    const NAME: &str = "Add Data";
    let lower = path.to_ascii_lowercase();
    let name = Path::new(path).file_stem().and_then(|n| n.to_str()).unwrap_or("layer").to_string();
    let given = |k: &str| ps(p, k).is_some_and(|v| !v.trim().is_empty());
    if is_geojson(path)
        && !rustgis_data::cityjson::is_cityjson_name(path)
        // Match add_data's bounded city-model probe before opening GeoJSON in place.
        && !(lower.ends_with(".json")
            && (given("crs") || given("layer"))
            && std::fs::metadata(path).is_ok_and(|m| m.len() <= rustgis_data::io::BYTES_FLOOR)
            && may_be_city_model_path(path))
        && streams_geojson(p, path)
    {
        let (path, may_load) = (path.to_string(), b(p, "stream").is_none());
        return Some(super::reads::ahead(NAME, in_place_key(&path, None), move || {
            Opened::ahead(&path, may_load, || {
                Ok(Box::new(rustgis_data::source::geojson::GeoJsonSource::open_path(Path::new(&path), &name)?))
            })
        }));
    }
    if !lower.ends_with(".shp") {
        return None;
    }
    let q = past_cap_shapefile(path, p).unwrap_or_else(|| p.clone());
    let stream = streams_shapefile(&q, path);
    if !stream {
        return None;
    }
    let text = ps(&q, "crs").map(str::trim).filter(|c| !c.is_empty());
    // One add_data can't parse it refuses as it runs.
    let crs = text.map(rustgis_data::raster::Crs::parse).transpose().ok()?;
    let (key, path, may_load) = (in_place_key(path, text), path.to_string(), b(&q, "stream").is_none());
    Some(super::reads::ahead(NAME, key, move || {
        Opened::ahead(&path, may_load, || Ok(Box::new(rustgis_data::source::shp::ShpSource::open_path_in(Path::new(&path), &name, crs)?)))
    }))
}

/// Add Data's read of the file `p` names, made before it runs ([`super::reads`]): a raster read
/// whole or converted by GDAL, a GeoJSON read whole, delimited text, a spreadsheet or a city model
/// (CityJSON or CityGML) or DXF drawing read into its layers, a Shapefile read whole, a large
/// Shapefile or GeoJSON file opened in place ([`in_place_ahead`]), or a standalone dBASE table.
/// `None` for any other file, which [`add_data`] reads as it adds it. Each is picked only where
/// `add_data` takes that read, so a read made is never wasted; a few it makes whole are left to it
/// (a possible CityJSON `.json` with a coordinate system or layer given, or a large GeoJSON
/// that isn't a FeatureCollection).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn add_data_ahead(p: &Value) -> Option<super::reads::ReadWork> {
    use super::reads::ahead;
    const NAME: &str = "Add Data";
    let path = gdb_path(ps(p, "path")?).into_owned();
    if let Some(shp) = rustgis_data::io::shapefile_for_dbf(Path::new(&path)) {
        let mut q = p.clone();
        q["path"] = json!(shp.display().to_string());
        // Opened in place, it is taken by its own key as add_data goes on to the Shapefile.
        if let Some(work) = in_place_ahead(&q, &shp.display().to_string()) {
            return Some(work);
        }
        return add_data_ahead(&q).map(|work| work.with_key(dbf_shapefile_key(&path)));
    }
    if let Some(work) = in_place_ahead(p, &path) {
        return Some(work);
    }
    let scan = super::raster::wants_scan(p, &path).ok()?;
    if !scan && rustgis_data::photo::is_photo_name(&path) && !rustgis_data::raster::world::places(Path::new(&path)) {
        return None;
    }
    if rustgis_data::raster::is_raster_name(&path) {
        // A page that isn't one is refused when the command runs.
        let page = super::raster::pdf_page(&path, p).ok()?;
        let key = super::raster::read_key(&path, page.as_deref(), scan);
        return Some(ahead(NAME, key, move || super::raster::read_raster_as(&path, page.as_deref(), scan)));
    }
    if rustgis_data::raster::zarr::is_store(Path::new(&path)) {
        return None;
    }
    if rustgis_data::raster::gdal::is_gdal_raster_name(&path) {
        let text = |k: &str| ps(p, k).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
        let (key, layer, crs) = (super::raster::gdal_key(&path), text("layer"), text("crs"));
        return Some(ahead(NAME, key, move || super::raster::read_gdal(&path, layer.as_deref(), crs.as_deref())));
    }
    if rustgis_data::dxf::is_dxf_name(&path) {
        let (key, q) = (whole_key(&path), p.clone());
        return Some(ahead(NAME, key, move || read_dxf_file(&q, &path)));
    }
    let given = |k: &str| ps(p, k).is_some_and(|v| !v.trim().is_empty());
    let lower = path.to_ascii_lowercase();
    // A GeoJSON read whole: small with no "stream" given (which add_data decides by), or any size
    // (within the reader's limit) with `"stream": false`; and not a CityJSON model (named so, or a
    // .json whose head could identify CityJSON, with a coordinate system or a layer given).
    let geojson = is_geojson(&path)
        && !rustgis_data::cityjson::is_cityjson_name(&path)
        && b(p, "stream") != Some(true)
        && !(lower.ends_with(".json") && (given("crs") || given("layer")) && may_be_city_model_path(&path))
        && std::fs::metadata(&path)
            .is_ok_and(|m| m.is_file() && (b(p, "stream") == Some(false) || m.len() <= rustgis_data::source::geojson::IN_PLACE_ABOVE));
    // A ZIP, a city model, a GML or a GPX file: its layers, read whole. A model named `.json` (not
    // `.city.json`) is a file of several layers only with options given, which Add Data reads as
    // it runs. A KML or KMZ (with its ground overlays) and a LandXML file are read as they are added.
    if read_ahead_whole(&path) && !large_osm(&path) {
        let (key, q) = (layers_key(&path), p.clone());
        return Some(ahead(NAME, key, move || read_layers_file(&q, &path)));
    }
    // A Shapefile read whole, in the coordinate system given if one is: as add_data reads it (one
    // past the whole-file reader's cap, or with more records than it takes, is read in place).
    if lower.ends_with(".shp") {
        let q = past_cap_shapefile(&path, p).unwrap_or_else(|| p.clone());
        let stream = streams_shapefile(&q, &path);
        if stream {
            return None;
        }
        let key = whole_key(&path);
        return match ps(&q, "crs").map(str::trim).filter(|c| !c.is_empty()) {
            Some(text) => {
                // One add_data can't parse it refuses as it runs.
                let crs = rustgis_data::raster::Crs::parse(text).ok()?;
                let name = Path::new(&path).file_stem().and_then(|n| n.to_str()).unwrap_or("layer").to_string();
                Some(ahead(NAME, key, move || read_shapefile_with(&path, &name, crs)))
            }
            None => Some(ahead(NAME, key, move || read_whole(&q, &path))),
        };
    }
    let (key, q) = (whole_key(&path), p.clone());
    if rustgis_data::sheets::is_sheet_name(&path) {
        return Some(ahead(NAME, key, move || read_sheet_file(&q, &path)));
    }
    (geojson || is_delimited_name(&path) || lower.ends_with(".dbf")).then(|| ahead(NAME, key, move || read_whole(&q, &path)))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn add_data_ahead(_: &Value) -> Option<super::reads::ReadWork> {
    None
}

/// Adds every layer of a file that holds several: the Shapefiles (and other files) of a zip, a KML
/// or KMZ's points, lines and polygons drawn in its styles, a GPX's waypoints, routes, tracks and
/// track points. `"layer"` picks one by name; a name given applies to the first.
pub(crate) fn add_layers(s: &mut Session, p: &Value, file_name: &str, bytes: &[u8], zoom: Option<bool>, local_file: bool) -> CmdResult {
    // A LandXML file in a coordinate system it doesn't say.
    let crs = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty());
    let landxml = rustgis_data::landxml::is_landxml_name(file_name)
        || (file_name.to_ascii_lowercase().ends_with(".xml")
            && rustgis_data::landxml::looks_like_landxml(&String::from_utf8_lossy(bytes.get(..bytes.len().min(4096)).unwrap_or(bytes))));
    let lower = file_name.to_ascii_lowercase();
    let kml = lower.ends_with(".kml") || lower.ends_with(".kmz");
    // A KML's ground overlays (pictures) are raster layers, added after its placemarks.
    let overlays = if kml && ps(p, "layer").is_none() { ground_overlays(s, file_name, bytes, local_file) } else { Vec::new() };
    // A DXF drawing: its coordinate system (it never says) and one of its own layers.
    let dxf = rustgis_data::dxf::is_dxf_name(file_name);
    let reads = match crs.filter(|_| landxml) {
        _ if lower.ends_with(".zip") && crs.is_some() => {
            let crs = crs.map(|text| super::parse_crs(s, text)).transpose()?;
            rustgis_data::io::read_zip_in(file_name, bytes, crs)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|(dataset, report)| rustgis_data::io::LayerRead { dataset, report, styles: Vec::new() })
                .collect()
        }
        // A city model (CityJSON or CityGML) with a coordinate system given, which takes the place
        // of any it names.
        _ if crs.is_some() && rustgis_data::io::is_city_model(file_name, bytes) => {
            let crs = crs.map(|text| super::parse_crs(s, text)).transpose()?;
            city_reads(file_name, bytes, crs)?
        }
        _ if dxf => dxf_reads(p, file_name, bytes, crs.map(|text| super::parse_crs(s, text)).transpose()?)?,
        Some(c) => {
            let crs = super::parse_crs(s, c)?;
            rustgis_data::landxml::read(file_name, bytes, Some(crs))
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|(dataset, report)| rustgis_data::io::LayerRead { dataset, report, styles: Vec::new() })
                .collect()
        }
        None => match rustgis_data::io::read_layers(file_name, bytes) {
            Ok(r) => r,
            // A document of overlays only.
            Err(_) if !overlays.is_empty() => Vec::new(),
            Err(e) => return Err(e.to_string()),
        },
    };
    if dxf {
        return add_reads(s, p, file_name, reads, overlays, zoom);
    }
    let whole = rustgis_data::io::is_city_model(file_name, bytes) || rustgis_data::osm::looks_like_osm_pbf(bytes);
    add_picked(s, p, file_name, reads, whole, overlays, zoom)
}

/// Adds the layers a file gave, or the one `"layer"` picks. `whole` carries the first layer's
/// file-wide report for city models and OSM; OSM has no layer-local height message to remove.
fn add_picked(
    s: &mut Session,
    p: &Value,
    file_name: &str,
    mut reads: Vec<rustgis_data::io::LayerRead>,
    whole: bool,
    overlays: Vec<(String, rustgis_data::raster::Raster)>,
    zoom: Option<bool>,
) -> CmdResult {
    let lower = file_name.to_ascii_lowercase();
    if let Some(want) = ps(p, "layer").map(str::trim).filter(|t| !t.is_empty()) {
        let names: Vec<String> = reads.iter().map(|r| r.dataset.name.clone()).collect();
        // By its full name or its last word ("tracks" for "walk tracks").
        let pick = |n: &str| n.eq_ignore_ascii_case(want) || n.rsplit(' ').next().is_some_and(|w| w.eq_ignore_ascii_case(want));
        // A city model's first layer holds what the file as a whole says, after its own one
        // notice: that its heights were dropped (it has no other). OSM has no such height notice.
        // In a zip, a model is the one member that gives several layers, and it says so for its own layers only.
        let same = |a: &rustgis_data::io::LayerRead, b: &rustgis_data::io::LayerRead| a.dataset.source == b.dataset.source;
        let file_wide: Vec<(Option<String>, rustgis_data::ReadReport)> = reads
            .iter()
            .enumerate()
            .filter(|(i, r)| {
                !pick(&r.dataset.name)
                    && !reads.iter().take(*i).any(|o| same(o, r))
                    && (whole || (lower.ends_with(".zip") && reads.iter().skip(i + 1).any(|o| same(o, r))))
            })
            .map(|(_, r)| {
                let mut said = r.report.clone();
                // The layer's own Z/M note, by its code: that layer says it.
                if r.dataset.zm_dropped
                    && let Some(at) = said.messages.iter().position(|n| n.loss == Some(rustgis_data::loss::LossCode::DroppedZm))
                {
                    said.messages.drain(at..=at);
                }
                (r.dataset.source.clone(), said)
            })
            .collect();
        reads.retain(|r| pick(&r.dataset.name));
        if reads.is_empty() {
            return Err(format!("{file_name}: no layer '{want}' (layers: {})", names.join(", ")));
        }
        for (source, said) in file_wide {
            if let Some(kept) = reads.iter_mut().find(|r| r.dataset.source == source) {
                kept.report.messages.extend(said.messages);
                kept.report.skipped += said.skipped;
            }
        }
    }
    add_reads(s, p, file_name, reads, overlays, zoom)
}

/// Adds the layers a file gave and its ground overlays: a layer that can't be added is skipped
/// and said, the others are still added. A name given goes to the first layer added. The
/// overlays go in first and the layers bottom first ([`stack_rank`]), so no image or polygon
/// covers the file's points.
fn add_reads(
    s: &mut Session,
    p: &Value,
    file_name: &str,
    mut reads: Vec<rustgis_data::io::LayerRead>,
    overlays: Vec<(String, rustgis_data::raster::Raster)>,
    zoom: Option<bool>,
) -> CmdResult {
    let mut added = Vec::new();
    let mut failed = Vec::new();
    // Listed after the layers in the result, as before they went in first. On an empty map the
    // first layer added is what is framed, as before, not the first image.
    let mut images = Vec::new();
    let frame_first = zoom.is_none() && !reads.is_empty() && super::map_is_empty(s);
    let image_zoom = if frame_first { Some(false) } else { zoom };
    let mut read_zoom = if frame_first { Some(true) } else { zoom };
    for (name, raster) in overlays {
        match super::raster::add_overlay(s, &name, raster, image_zoom) {
            Ok(r) => images.push(r),
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    reads.sort_by_key(|r| stack_rank(r.dataset.geometry_type));
    let mut q = p.clone();
    for read in reads {
        let name = read.dataset.name.clone();
        let styles = read.styles;
        // A layer that can't be added is skipped and said; the others are still added. A name
        // given goes to the first layer actually added.
        match add_read(s, &q, (read.dataset, read.report), read_zoom) {
            Ok(r) => {
                if let Some(id) = r.get("layer").and_then(Value::as_u64).and_then(|i| u32::try_from(i).ok()) {
                    apply_styles(s, id, &styles);
                }
                // A standalone table isn't drawn, so it frames nothing.
                if r.get("layer").is_some() {
                    read_zoom = zoom;
                }
                added.push(r);
                if let Some(o) = q.as_object_mut() {
                    o.remove("name");
                }
            }
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    // No layer was added to frame the empty map: the first image frames it, as it did before.
    if read_zoom == Some(true)
        && let Some(first) = images.first()
    {
        zoom_to_added(s, first);
    }
    added.extend(images);
    if added.is_empty() {
        return Err(failed.join("; "));
    }
    for f in &failed {
        s.notify(format!("{file_name}: {f}"), true);
    }
    let layers: Vec<Value> = added.iter().filter_map(|r| r.get("layer").cloned()).collect();
    let tables: Vec<Value> = added.iter().filter_map(|r| r.get("table").cloned()).collect();
    let features: u64 = added.iter().filter_map(|r| r.get("features").and_then(Value::as_u64)).sum();
    let mut out = json!({"layers": layers, "tables": tables, "features": features});
    // The first layer, as the result of adding a single file says it.
    if let Some(first) = layers.first() {
        out["layer"] = first.clone();
    }
    // `"zoom": true` frames everything added, not only the file's last layer (each `add_read`
    // above framed its own).
    if zoom == Some(true) {
        zoom_to_added(s, &out);
    }
    Ok(out)
}

/// A KML or KMZ's ground overlays as rasters: pictures inside a KMZ, or files beside a KML on
/// disk (by a relative path that stays in its folder). Overlays that can't be drawn are said.
fn ground_overlays(s: &mut Session, file_name: &str, bytes: &[u8], local_file: bool) -> Vec<(String, rustgis_data::raster::Raster)> {
    match rustgis_data::kml::ground_overlays(file_name, bytes) {
        Ok(overlays) => overlays_drawn(s, file_name, overlays, local_file),
        Err(_) => Vec::new(),
    }
}

/// The ground overlays of [`rustgis_data::kml::ground_overlays`] drawn, and those that can't be
/// said in one notice.
fn overlays_drawn(
    s: &mut Session,
    file_name: &str,
    (found, undrawn, over_budget): rustgis_data::kml::Overlays,
    local_file: bool,
) -> Vec<(String, rustgis_data::raster::Raster)> {
    let mut out = Vec::new();
    let mut notes = Vec::new();
    let dir = local_file.then(|| Path::new(file_name).parent()).flatten().filter(|d| d.is_absolute());
    for (g, picture) in found {
        let href = g.href.trim();
        // `Err(None)`: not in the file or beside it; `Err(Some(why))`: beside it but unreadable.
        let picture = picture.ok_or(None::<String>).or_else(|_| {
            // Beside the document: no web addresses, absolute paths or climbing out of its folder.
            let rel = Path::new(href);
            let safe = !href.contains("://")
                    && rel.components().next().is_some()
                    // Only names (no root, drive prefix or `..`), so it stays in the folder on
                    // Windows too (`C:x`, `\\x`).
                    && rel.components().all(|c| matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir));
            if !safe {
                return Err(None);
            }
            let p = dir.ok_or(None)?.join(rel);
            // Only a missing picture isn't beside it; one that can't be looked at says why.
            // (Followed as the read follows it: a link to nothing is missing too.)
            match std::fs::metadata(&p) {
                Ok(_) => {}
                // (A file named as a folder on its way, `a.png/b.png`, isn't beside it either.)
                Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => return Err(None),
                Err(e) => return Err(Some(e.to_string())),
            }
            crate::project::read_file(&p, (256 << 20) - 1).map_err(Some)
        });
        let picture = match picture {
            Ok(p) => p,
            Err(None) => {
                notes.push(format!("{} (its picture {href} isn't in the file or beside it)", g.name));
                continue;
            }
            Err(Some(why)) => {
                notes.push(format!("{} (its picture {href} can't be read: {why})", g.name));
                continue;
            }
        };
        match rustgis_data::raster::from_image(&g.name, &picture, g.bbox, g.rotation) {
            Ok(r) => out.push((g.name.clone(), r)),
            Err(e) => notes.push(format!("{} ({e})", g.name)),
        }
    }
    if undrawn > 0 {
        notes.push(format!("{undrawn} ground overlays without a picture or a box (LatLonBox)"));
    }
    if over_budget > 0 {
        notes.push(format!(
            "{over_budget} ground overlays left out: their pictures don't fit beside the archive in the {} MiB a KMZ is expanded within",
            rustgis_data::io::BYTES_FLOOR >> 20
        ));
    }
    if !notes.is_empty() {
        s.notify(format!("{file_name}: {}", notes.join("; ")), false);
    }
    out
}

/// Draws a layer in the styles its file declares (KML): unique values of its `style` field, each
/// style's colours and widths, the layer's default symbol for anything a style leaves unset.
fn apply_styles(s: &mut Session, id: u32, styles: &[(String, rustgis_data::kml::Style)]) {
    use rustgis_map::Symbol;
    // A legend of hundreds of styles helps no one; such files keep the default symbol.
    if styles.is_empty() || styles.len() > 64 {
        return;
    }
    let Some(layer) = s.map_mut().layer_mut(id) else { return };
    let g = layer.dataset.geometry_type;
    let base = match &layer.renderer {
        Renderer::Single(sym) => *sym,
        _ => Symbol::default_for(g, id),
    };
    let rgba = |c: [u8; 4]| Rgba::new(c[0], c[1], c[2], c[3]);
    let classes = styles
        .iter()
        .map(|(name, st)| {
            let mut sym = base;
            match g {
                GeometryType::Point => {
                    if let Some(c) = st.icon {
                        sym.fill = rgba(c);
                    }
                    if let Some(k) = st.scale {
                        sym.size = (base.size * k).clamp(2.0, 40.0);
                    }
                }
                GeometryType::Polyline => {
                    if let Some(c) = st.stroke {
                        sym.stroke = rgba(c);
                        sym.fill = rgba(c);
                    }
                    if let Some(w) = st.width {
                        sym.stroke_width = w.clamp(0.3, 20.0);
                    }
                }
                _ => {
                    if let Some(c) = st.fill {
                        sym.fill = rgba(c);
                    }
                    if !st.filled {
                        sym.fill = Rgba::TRANSPARENT;
                    }
                    if let Some(c) = st.stroke {
                        sym.stroke = rgba(c);
                    }
                    if let Some(w) = st.width {
                        sym.stroke_width = w.clamp(0.0, 20.0);
                    }
                    if !st.outlined {
                        sym.stroke = Rgba::TRANSPARENT;
                    }
                }
            }
            (name.clone(), sym)
        })
        .collect();
    layer.renderer = Renderer::Unique {
        field: "style".into(),
        classes,
        other: base,
        ramp: Default::default(),
        class_labels: Default::default(),
        other_label: None,
        class_strokes: Default::default(),
        other_strokes: Vec::new(),
        class_fills: Default::default(),
        other_fills: Vec::new(),
        expression: None,
    };
}

/// The layers a file holds, without adding them: a GeoPackage's feature tables, a GDAL dataset's
/// layers, or the one layer of other files. `layer.addData` takes a name with `"layer"`. A file
/// Add Data can link instead of copying says how it would take it, in `"link"`, when `"link": true`
/// asks ([`link_choice`]): it opens each of a GeoPackage's tables, which the other callers (List
/// Tables, Repair Data Source) don't need.
fn file_layers(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    let mut v = listed_layers(s, p)?;
    #[cfg(not(target_arch = "wasm32"))]
    if b(p, "link") == Some(true)
        && let Some(path) = ps(p, "path")
    {
        let tables: Vec<String> = v["layers"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
        if let Some(link) = link_choice(path, &tables) {
            v["link"] = link;
        }
    }
    Ok(v)
}

/// [`file_layers`]' listing.
fn listed_layers(s: &mut Session, p: &Value) -> CmdResult {
    let path = s_req(p, "path")?;
    let path = &*gdb_path(path);
    let err = |e: rustgis_data::DataError| format!("{path}: {e}");
    // A geodatabase read without GDAL; through GDAL if it can't be and GDAL is installed.
    #[cfg(not(target_arch = "wasm32"))]
    if rustgis_data::filegdb::is_geodatabase(Path::new(path)) {
        match rustgis_data::filegdb::layers(Path::new(path)) {
            Ok(layers) => return Ok(json!({"count": layers.len(), "layers": layers})),
            Err(_) if rustgis_data::source::gdal::available() => {}
            Err(e) => return Err(err(e)),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    if rustgis_data::source::gdal::is_gdal_name(path) {
        let layers = rustgis_data::source::gdal::layers(Path::new(path)).map_err(err)?;
        return Ok(json!({"count": layers.len(), "layers": layers}));
    }
    #[cfg(not(target_arch = "wasm32"))]
    if rustgis_data::source::duckdb::is_duckdb_name(path) {
        let tables = rustgis_data::source::duckdb::tables(Path::new(path)).map_err(err)?;
        let layers: Vec<String> = tables.iter().map(|t| t.label()).collect();
        return Ok(json!({"count": layers.len(), "layers": layers}));
    }
    if rustgis_data::source::is_source_name(path) {
        let layers = rustgis_data::source::layers(Path::new(path)).map_err(err)?;
        return Ok(json!({"count": layers.len(), "layers": layers}));
    }
    if !Path::new(path).is_file() {
        return Err(format!("{path}: no such file"));
    }
    if rustgis_data::sheets::is_sheet_name(path) {
        let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        if meta.len() > super::raster::MAX_UPLOAD_BYTES as u64 {
            return Err(rustgis_data::io::file_size_error(path, super::raster::MAX_UPLOAD_BYTES as u64));
        }
        let bytes = rustgis_data::provenance::read(path).map_err(|e| format!("{path}: {e}"))?;
        let layers = rustgis_data::sheets::sheet_names(&bytes).map_err(err)?;
        return Ok(json!({"count": layers.len(), "layers": layers}));
    }
    #[cfg(not(target_arch = "wasm32"))]
    if large_osm(path) {
        let layers = rustgis_data::source::gdal::layers(Path::new(path)).map_err(err)?;
        return Ok(json!({"count": layers.len(), "layers": layers}));
    }
    // A DXF drawing's own layers (the ones it draws on), picked with "layer".
    if rustgis_data::dxf::is_dxf_name(path) {
        let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        let max = rustgis_data::io::max_bytes();
        if meta.len() > max {
            return Err(rustgis_data::io::file_size_error(path, max));
        }
        let bytes = rustgis_data::provenance::read(path).map_err(|e| format!("{path}: {e}"))?;
        let layers = rustgis_data::dxf::layers(path, &bytes).map_err(err)?;
        return Ok(json!({"count": layers.len(), "layers": layers}));
    }
    // Sniff a plain .json before considering a whole-file city-model read.
    let crs = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty());
    let model_json = path.to_ascii_lowercase().ends_with(".json")
        && std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= rustgis_data::io::BYTES_FLOOR)
        && may_be_city_model_path(path);
    if rustgis_data::io::is_multi_layer_name(path) || model_json {
        let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        // Listed within the size Add Data takes it at.
        if let Some(e) = past_read_as_added(path, meta.len()) {
            return Err(e);
        }
        let max = rustgis_data::io::max_bytes();
        if meta.len() > max {
            return Err(rustgis_data::io::file_size_error(path, max));
        }
        let bytes = rustgis_data::provenance::read(path).map_err(|e| format!("{path}: {e}"))?;
        let zip = path.to_ascii_lowercase().ends_with(".zip");
        let layers: Vec<String> = match crs.filter(|_| zip || rustgis_data::io::is_city_model(path, &bytes)) {
            Some(crs) => {
                let crs = Some(super::parse_crs(s, crs)?);
                let read = if zip {
                    rustgis_data::io::read_zip_in(path, &bytes, crs)
                } else {
                    rustgis_data::io::read_city_model(path, &bytes, crs)
                };
                read.map_err(err)?.into_iter().map(|(ds, _)| ds.name).collect()
            }
            None if rustgis_data::io::is_multi_layer_name(path) || rustgis_data::io::is_cityjson(path, &bytes) => {
                rustgis_data::io::read_layers(path, &bytes).map_err(err)?.into_iter().map(|l| l.dataset.name).collect()
            }
            // Not CityJSON: a single layer, listed below.
            None => Vec::new(),
        };
        if !layers.is_empty() || rustgis_data::io::is_multi_layer_name(path) {
            return Ok(json!({"count": layers.len(), "layers": layers}));
        }
    }
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if !rustgis_data::io::READ_EXTENSIONS.contains(&ext.as_str()) && !rustgis_data::raster::is_raster_name(path) {
        return Err(format!("{path}: not a data file RustGIS reads"));
    }
    let name = Path::new(path).file_stem().and_then(|n| n.to_str()).unwrap_or("layer");
    Ok(json!({"count": 1, "layers": [name]}))
}

/// Reads at most 64 KiB to route a plain .json without allocating the whole file.
/// An inconclusive head keeps the existing whole-file fallback (within its size cap), so a
/// CityJSON root type after a large metadata value or extension member still works. A root type
/// or CityObjects key lets us stop before its payload. A features key alone is inconclusive:
/// CityJSON permits extension members, and its root type may follow them.
fn may_be_city_model_path(path: &str) -> bool {
    use std::io::Read;
    let mut head = Vec::new();
    match rustgis_data::provenance::open(path).and_then(|f| f.take(64 << 10).read_to_end(&mut head)) {
        Ok(_) => json_city_model_head(&head),
        Err(_) => true, // The existing reader reports the I/O error.
    }
}

/// Inspects root keys only; nested properties and strings cannot identify a city model.
fn json_city_model_head(head: &[u8]) -> bool {
    use serde::Deserializer;
    use serde::de::{IgnoredAny, MapAccess, Visitor};
    struct Head<'a>(&'a std::cell::Cell<Option<bool>>);
    impl<'de> Visitor<'de> for Head<'_> {
        type Value = bool;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a JSON document's root object")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<bool, M::Error> {
            while let Some(key) = map.next_key::<String>()? {
                match key.as_str() {
                    "type" => self.0.set(Some(map.next_value::<String>()? == "CityJSON")),
                    "CityObjects" => self.0.set(Some(true)),
                    _ => {
                        map.next_value::<IgnoredAny>()?;
                        continue;
                    }
                }
                // Stop without consuming the payload. serde otherwise requires the closing
                // brace after visit_map; the decision deliberately survives this sentinel error.
                return Err(serde::de::Error::custom("JSON format decided"));
            }
            self.0.set(Some(false));
            Ok(false)
        }
    }
    let head = head.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(head);
    let decided = std::cell::Cell::new(None);
    let _ = serde_json::Deserializer::from_slice(head).deserialize_map(Head(&decided));
    decided.get().unwrap_or(true)
}

/// Whether a file name is GeoJSON's (`.geojson` or `.json`).
#[cfg(not(target_arch = "wasm32"))]
fn is_geojson(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".geojson") || lower.ends_with(".json")
}

/// Where a source file is: on disk, or uploaded (its name and bytes).
pub(crate) enum Origin<'a> {
    Path(&'a str),
    Bytes(&'a str, std::sync::Arc<Vec<u8>>),
    /// A PostGIS table, always opened by the caller (desktop only).
    #[cfg_attr(target_arch = "wasm32", expect(dead_code, reason = "database sources are desktop only"))]
    Postgis(&'a rustgis_map::source::PostgisOrigin),
    /// A MySQL or MariaDB table, always opened by the caller (desktop only).
    #[cfg_attr(target_arch = "wasm32", expect(dead_code, reason = "database sources are desktop only"))]
    Mysql(&'a rustgis_map::source::MysqlOrigin),
    /// A SQL Server table, always opened by the caller (desktop only).
    #[cfg_attr(target_arch = "wasm32", expect(dead_code, reason = "database sources are desktop only"))]
    Sqlserver(&'a rustgis_map::source::SqlServerOrigin),
}

/// Most GeoPackage tables added at once.
const MAX_TABLES: usize = 32;

/// Most skipped tables named in `layer.addData`'s result (`skippedCount` counts them all, and
/// `info.fileLayers` lists them).
const MAX_SKIPPED_NAMED: usize = 100;

/// Adds a GeoPackage, GeoParquet or FlatGeobuf file: each feature table becomes a layer. Large
/// sources (or any, with `"stream": true`) are streamed, read in place a view at a time; smaller
/// ones are loaded whole, so they can be edited.
pub(crate) fn add_source(s: &mut Session, p: &Value, origin: Origin<'_>, zoom: Option<bool>) -> CmdResult {
    add_source_with(s, p, origin, None, zoom)
}

/// [`add_source`], with the source already open (its one table).
fn add_source_with(
    s: &mut Session,
    p: &Value,
    origin: Origin<'_>,
    opened: Option<Box<dyn rustgis_data::source::FeatureSource>>,
    zoom: Option<bool>,
) -> CmdResult {
    add_source_read(s, p, origin, opened, None, zoom)
}

/// [`add_source_with`], with the open source's features already read whole (`whole`, made ahead
/// on the read thread, #2446) when it is loaded rather than kept in place.
fn add_source_read(
    s: &mut Session,
    p: &Value,
    origin: Origin<'_>,
    mut opened: Option<Box<dyn rustgis_data::source::FeatureSource>>,
    mut whole: Option<rustgis_data::Result<(rustgis_data::Dataset, rustgis_data::ReadReport)>>,
    zoom: Option<bool>,
) -> CmdResult {
    use rustgis_data::source;
    let label = match &origin {
        Origin::Path(p) | Origin::Bytes(p, _) => p.to_string(),
        Origin::Postgis(o) => o.database.clone(),
        Origin::Mysql(o) => o.database.clone(),
        Origin::Sqlserver(o) => o.database.clone(),
    };
    let err = |e: rustgis_data::DataError| format!("{label}: {e}");
    // Tables past the cap, said in the result as well as in a notice, so the CLI, the MCP server
    // and the AI Assistant know the file holds more than was added.
    let mut skipped: Vec<String> = Vec::new();
    let mut total = None;
    let selected = ps(p, "layer").map(str::trim).filter(|t| !t.is_empty());
    // A desktop upload is copied once; inspection and opening share its SQLite connection.
    #[cfg(not(target_arch = "wasm32"))]
    let upload = match &origin {
        Origin::Bytes(name, bytes) if name.to_ascii_lowercase().ends_with(".gpkg") && opened.is_none() => {
            Some(source::gpkg::GpkgUpload::new(bytes.as_slice()).map_err(err)?)
        }
        _ => None,
    };
    // Report unsupported contents for a whole-file add, not a deliberate table selection.
    let contents = match (&origin, selected.is_none()) {
        #[cfg(not(target_arch = "wasm32"))]
        (Origin::Path(path), true) if path.to_ascii_lowercase().ends_with(".gpkg") => {
            source::gpkg::unsupported_table_counts(Path::new(path)).map_err(err)?
        }
        #[cfg(not(target_arch = "wasm32"))]
        (Origin::Bytes(name, bytes), true) if name.to_ascii_lowercase().ends_with(".gpkg") => match &upload {
            Some(upload) => upload.unsupported_table_counts().map_err(err)?,
            None => source::gpkg::unsupported_table_counts_bytes(bytes.as_slice()).map_err(err)?,
        },
        #[cfg(target_arch = "wasm32")]
        (Origin::Bytes(name, bytes), true) if name.to_ascii_lowercase().ends_with(".gpkg") => {
            source::gpkg_mem::unsupported_table_counts(source::Shared(bytes.clone())).map_err(err)?
        }
        _ => Vec::new(),
    };
    let omitted: Vec<String> = contents
        .into_iter()
        .map(|(kind, count)| {
            if kind == "attributes" {
                format!("{count} attribute-only table(s) not added: GeoPackage attribute tables are not supported")
            } else if kind == "features" {
                format!("{count} feature table(s) not added: no geometry column is registered")
            } else {
                format!("{count} {kind} table(s) not added: only GeoPackage feature tables are supported")
            }
        })
        .collect();
    let attribute_note = (!omitted.is_empty()).then(|| omitted.join("; "));
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(upload) = upload {
        opened = Some(Box::new(upload.open(selected).map_err(|e| match &attribute_note {
            Some(note) => format!("{}; {note}", err(e)),
            None => err(e),
        })?));
    }
    let tables: Vec<Option<String>> = match (selected, &origin) {
        (Some(t), _) => vec![Some(t.to_string())],
        (None, Origin::Path(path)) if path.to_ascii_lowercase().ends_with(".gpkg") => {
            let mut all = source::layers(Path::new(path)).map_err(err)?;
            if all.len() > MAX_TABLES {
                total = Some(all.len());
                skipped = all.split_off(MAX_TABLES);
            }
            all.into_iter().map(Some).collect()
        }
        // In the browser an uploaded GeoPackage adds every table, listed by the pure-Rust reader.
        // The desktop adds the first feature table from one private SQLite copy shared with
        // inspection, reporting other tables through the source notes; files on disk add all.
        #[cfg(target_arch = "wasm32")]
        (None, Origin::Bytes(name, bytes)) if name.to_ascii_lowercase().ends_with(".gpkg") => {
            let mut all = source::gpkg_mem::layers(source::Shared(bytes.clone())).map_err(err)?;
            if all.len() > MAX_TABLES {
                total = Some(all.len());
                skipped = all.split_off(MAX_TABLES);
            }
            all.into_iter().map(Some).collect()
        }
        _ => vec![None],
    };
    if tables.is_empty() {
        return Err(match &attribute_note {
            Some(note) => format!("{label}: no feature tables; {note}"),
            None => format!("{label}: no feature tables"),
        });
    }
    let empty_before = super::map_is_empty(s);
    let mut ids = Vec::new();
    let mut streamed = false;
    let mut features = 0u64;
    for table in tables {
        let src = match opened.take() {
            Some(src) => src,
            None => match &origin {
                Origin::Path(path) => source::open_path(Path::new(path), table.as_deref()),
                Origin::Bytes(name, bytes) => source::open_shared(name, bytes.clone(), table.as_deref()),
                Origin::Postgis(_) | Origin::Mysql(_) | Origin::Sqlserver(_) => {
                    Err(rustgis_data::DataError::Format("the table isn't open".into()))
                }
            }
            .map_err(|e| match &attribute_note {
                Some(note) => format!("{}; {note}", err(e)),
                None => err(e),
            })?,
        };
        let info = src.info().clone();
        // A point cloud is always read in place, so a project keeps its file's path, not millions
        // of points; its colours come from a sample of it.
        let lidar = info.format == "LiDAR";
        let style = if lidar { lidar_renderer(src.as_ref(), s.map().layers.len() as u32) } else { None };
        let stream = b(p, "stream").unwrap_or(lidar || streams_by_count(info.count));
        // A name given applies to a single table.
        let name = match ps(p, "name").map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) if ids.is_empty() => n.to_string(),
            _ => info.name.clone(),
        };
        if stream {
            let mut layer = match &origin {
                Origin::Path(path) => {
                    let full = std::path::absolute(path).map(|p| p.display().to_string()).unwrap_or_else(|_| path.to_string());
                    rustgis_map::source::SourceLayer::from_path(full, table.clone(), src)
                }
                Origin::Bytes(file, bytes) => rustgis_map::source::SourceLayer::from_bytes(*file, bytes.clone(), table.clone(), src),
                Origin::Postgis(o) => rustgis_map::source::SourceLayer::from_postgis((*o).clone(), src),
                Origin::Mysql(o) => rustgis_map::source::SourceLayer::from_mysql((*o).clone(), src),
                Origin::Sqlserver(o) => rustgis_map::source::SourceLayer::from_sqlserver((*o).clone(), src),
            };
            if info.format == "Shapefile" {
                layer.shapefile_crs =
                    ps(p, "crs").map(str::trim).filter(|c| !c.is_empty()).map(rustgis_data::raster::Crs::parse).transpose()?;
            }
            let unique = unique_name(s, &name);
            for n in &info.notes {
                s.say(n.clone().prefixed(&unique));
            }
            let id = s.map_mut().add_streamed(unique.clone(), layer);
            if let Some(l) = s.map_mut().layer_mut(id) {
                // Kept with the layer, as a materialized read keeps it (see Dataset::zm_dropped).
                l.dataset.zm_dropped = info.zm_dropped;
                if let Some(r) = style.clone() {
                    l.renderer = r;
                }
            }
            s.active_layer = Some(id);
            ids.push(id);
            streamed = true;
            features += info.count.unwrap_or(0);
            s.notify(
                format!(
                    "{unique}: {} features, read in place a view at a time (Load All Features loads them into memory)",
                    info.count.map(|c| c.to_string()).unwrap_or_else(|| "many".into())
                ),
                false,
            );
        } else {
            let (mut ds, report) = whole.take().unwrap_or_else(|| source::materialize(src.as_ref())).map_err(err)?;
            ds.name = name;
            ds.source = Some(label.clone());
            features += ds.features.len() as u64;
            let id = add_dataset(s, ds);
            if let (Some(r), Some(l)) = (style.clone(), s.map_mut().layer_mut(id)) {
                l.renderer = r;
            }
            ids.push(id);
            // The source names itself by its file (a DuckDB result's cache file); the layer has the
            // name the user knows.
            let shown = s.map().layer(id).map_or_else(|| info.name.clone(), |l| l.name.clone());
            let prefix = format!("{}: ", info.name);
            for m in report.messages {
                // As the layer is named: its name may differ from the source's.
                s.say(m.map_text(|t| t.strip_prefix(&prefix).map_or_else(|| t.clone(), |rest| format!("{shown}: {rest}"))));
            }
        }
    }
    if zoom.unwrap_or(empty_before) {
        s.zoom_to_layers(&ids, rustgis_map::ExtentOf::All);
    }
    if let Some(note) = attribute_note {
        s.notify(format!("{label}: {note}"), false);
    }
    let first = ids.first().copied();
    let mut out = json!({"layer": first, "layers": ids, "features": features, "streamed": streamed});
    // Said once the tables are in: a failure above is the error instead.
    if let Some(total) = total {
        s.notify(
            format!(
                "{label}: added the first {MAX_TABLES} of {total} tables; add others by name with \"layer\" (info.fileLayers lists them)"
            ),
            false,
        );
        out["tables"] = json!(total);
        out["skippedCount"] = json!(skipped.len());
        // Named up to a bound, so a file of thousands of tables can't flood an agent's context.
        out["skipped"] = json!(skipped.iter().take(MAX_SKIPPED_NAMED).collect::<Vec<_>>());
    }
    Ok(out)
}

/// ASPRS's standard classes, coloured as LiDAR viewers commonly show them.
const ASPRS: [(i64, [u8; 3]); 13] = [
    (0, [0xA8, 0xA8, 0xA8]),
    (1, [0xA8, 0xA8, 0xA8]),
    (2, [0xA6, 0x7C, 0x52]),
    (3, [0xB5, 0xDB, 0x7E]),
    (4, [0x5F, 0xB0, 0x4A]),
    (5, [0x1F, 0x6E, 0x2C]),
    (6, [0xE0, 0x5A, 0x3C]),
    (7, [0xFF, 0x3D, 0xF2]),
    (9, [0x3C, 0x8C, 0xDC]),
    (10, [0x80, 0x60, 0x40]),
    (11, [0x55, 0x55, 0x55]),
    (17, [0xE6, 0xB8, 0x3A]),
    (18, [0xFF, 0x3D, 0xF2]),
];

/// A point cloud's style: small dots by ASPRS class when it has several, else by elevation; from a
/// sample of its points.
fn lidar_renderer(src: &dyn rustgis_data::source::FeatureSource, seed: u32) -> Option<Renderer> {
    let info = src.info();
    let sample = src.read(&rustgis_data::source::Request { extent: None, limit: 100_000, own: None }).ok()?;
    let mut ds = rustgis_data::Dataset::new(info.name.clone(), GeometryType::Point, info.fields.clone());
    ds.features = sample.features.into();
    let mut base = rustgis_map::Symbol::default_for(GeometryType::Point, seed);
    base.size = 2.0;
    base.stroke_width = 0.0;
    let classes = ds.field_index("classification").map(|i| ds.unique_values(i, &[], 64).len()).unwrap_or(0);
    if classes >= 2 {
        let mut r = Renderer::unique(&ds, &[], "classification", Ramp::Harbour, base).ok()?;
        if let Renderer::Unique { classes, .. } = &mut r {
            for (value, sym) in classes.iter_mut() {
                if let Some((_, [red, g, b_])) = ASPRS.iter().find(|(c, _)| value.parse::<i64>().ok() == Some(*c)) {
                    sym.fill = Rgba::rgb(*red, *g, *b_);
                }
            }
        }
        Some(r)
    } else {
        Renderer::graduated(&ds, &[], "z", 7, ClassMethod::Quantile, Ramp::Terrain, base).ok()
    }
}

/// The parameters with `"stream": true` for a Shapefile whose `.shp` or `.dbf` is past what the
/// whole-file reader takes on every computer ([`rustgis_data::io::BYTES_FLOOR`]), when they don't
/// say whether to stream; `None` otherwise. Read in place, it never needs memory for it whole, so a
/// computer whose reader takes more streams it too.
#[cfg(not(target_arch = "wasm32"))]
fn past_cap_shapefile(path: &str, p: &Value) -> Option<Value> {
    let shp = Path::new(path);
    let past = |f: &Path| std::fs::metadata(f).is_ok_and(|m| m.len() > rustgis_data::io::BYTES_FLOOR);
    if !path.to_ascii_lowercase().ends_with(".shp") || b(p, "stream").is_some() || !(past(shp) || past(&shp.with_extension("dbf"))) {
        return None;
    }
    let mut q = p.clone();
    q.as_object_mut()?.insert("stream".into(), json!(true));
    Some(q)
}

/// An OpenStreetMap extract past what the native reader takes on every computer
/// ([`rustgis_data::io::BYTES_FLOOR`]), opened through GDAL instead when it is installed
/// (country-size extracts): GDAL converts it once to a file read in place.
#[cfg(not(target_arch = "wasm32"))]
fn large_osm(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".pbf")
        && std::fs::metadata(path).is_ok_and(|m| m.len() > rustgis_data::io::BYTES_FLOOR)
        && rustgis_data::source::gdal::available()
}

/// Adds a file GDAL reads (KML, GPX, File Geodatabase, DXF…): each layer is converted once into an
/// indexed FlatGeobuf in the cache, then added like one (streamed when large). A layer mixing
/// geometry types becomes a layer per type.
#[cfg(not(target_arch = "wasm32"))]
fn add_gdal(s: &mut Session, p: &Value, path: &str) -> CmdResult {
    let crs = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty());
    let (converted, failed) =
        rustgis_data::source::gdal::convert(Path::new(path), ps(p, "layer"), crs).map_err(|e| format!("{path}: {e}"))?;
    // Layers GDAL couldn't convert are said, so the file isn't taken for whole.
    for f in failed {
        s.notify(format!("{path}: {f}"), true);
    }
    // What the conversion dropped of the parts' Z or M values, as a fresh conversion says it.
    let (lost, unknown) = zm_notices(path, &converted);
    if let Some(text) = lost {
        s.notify_loss(rustgis_data::loss::LossCode::DroppedZm, text.into());
    }
    if let Some(text) = unknown {
        s.notify(text, false);
    }
    let original = std::path::absolute(path).map(|p| p.display().to_string()).unwrap_or_else(|_| path.to_string());
    let parts = gdal_parts(converted, &original, crs);
    add_converted(s, p, path, &original, parts)
}

/// What GDAL's conversion of `path` says of its parts' Z or M values, once for each kind: the
/// parts that had some, with their total (a loss), and the parts that couldn't be counted. A part
/// alone is named in a fresh conversion's words; several are counted and named, the first ten.
#[cfg(not(target_arch = "wasm32"))]
fn zm_notices(path: &str, converted: &[rustgis_data::source::gdal::Converted]) -> (Option<String>, Option<String>) {
    let named = |parts: &[&rustgis_data::source::gdal::Converted]| {
        let mut names: Vec<String> = parts.iter().take(10).map(|c| format!("'{}'", c.name)).collect();
        if parts.len() > 10 {
            names.push(format!("{} more", parts.len() - 10));
        }
        names.join(", ")
    };
    let lost: Vec<_> = converted.iter().filter(|c| c.zm_dropped > 0).collect();
    let unknown: Vec<_> = converted.iter().filter(|c| c.zm_dropped == 0 && c.zm_unknown).collect();
    let lost = match lost.as_slice() {
        [] => None,
        [one] => Some(format!(
            "{path}: GDAL: '{}' contains Z or M coordinates on {} source feature(s); conversion keeps only XY coordinates",
            one.name, one.zm_dropped
        )),
        many => Some(format!(
            "{path}: GDAL: {} layers ({}) contain Z or M coordinates on {} source feature(s); conversion keeps only XY coordinates",
            many.len(),
            named(many),
            many.iter().map(|c| c.zm_dropped).fold(0u64, u64::saturating_add)
        )),
    };
    let unknown = match unknown.as_slice() {
        [] => None,
        [one] => Some(format!(
            "{path}: GDAL: '{}': an unknown number of source feature(s) may have had Z or M coordinates dropped; conversion keeps only XY coordinates",
            one.name
        )),
        many => Some(format!(
            "{path}: GDAL: {} layers ({}): an unknown number of source feature(s) may have had Z or M coordinates dropped; conversion keeps only XY coordinates",
            many.len(),
            named(many)
        )),
    };
    (lost, unknown)
}

/// GDAL's conversions of the file `original` as the layers to add: the layers in the file's order,
/// each layer split by geometry type with its parts bottom first ([`bottom_first`]). A layer of
/// one type (no family) is a part alone, so the sort leaves it as it is.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn gdal_parts(mut converted: Vec<rustgis_data::source::gdal::Converted>, original: &str, crs: Option<&str>) -> Vec<Converted> {
    for layer in converted.chunk_by_mut(|a, b| a.layer == b.layer) {
        layer.sort_by_key(|c| c.family.map(bottom_first));
    }
    converted
        .into_iter()
        .map(|c| Converted {
            path: c.path,
            name: c.name,
            unprojected: c.unprojected,
            in_lonlat: c.in_lonlat,
            // A declared layer whose count failed is taken as having lost them, as a fresh
            // conversion takes it.
            zm_dropped: c.zm_dropped > 0 || (c.zm_unknown && c.zm_declared),
            origin: ConvertedOrigin::Gdal(rustgis_map::source::GdalOrigin {
                path: original.to_string(),
                layer: c.layer,
                family: c.family,
                crs: crs.map(str::to_string),
            }),
        })
        .collect()
}

/// Where a layer of this geometry type goes among the layers one file or source gives: polygons
/// first, then lines, then points, and standalone tables (which aren't drawn) last, so a name
/// given goes to a map layer. Each new layer goes on top, so the points end on top and none of
/// them hides under the source's own polygons. Every command that adds a source as several layers
/// adds them in this order (#1488).
pub(crate) fn stack_rank(geometry: GeometryType) -> u8 {
    match geometry {
        GeometryType::Polygon => 0,
        GeometryType::Polyline => 1,
        GeometryType::Point => 2,
        GeometryType::None => 3,
    }
}

/// [`stack_rank`] for one part of a source split by geometry type.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn bottom_first(family: rustgis_data::source::Family) -> u8 {
    use rustgis_data::source::Family;
    stack_rank(match family {
        Family::Polygons => GeometryType::Polygon,
        Family::Lines => GeometryType::Polyline,
        Family::Points => GeometryType::Point,
    })
}

/// The database a command names in `database`, with the `password` given remembered for the
/// session.
#[cfg(not(target_arch = "wasm32"))]
pub fn database_param(p: &Value) -> Option<rustgis_data::source::duckdb::Database> {
    use rustgis_data::source::duckdb::{Database, remember_password};
    let d = ps(p, "database").map(str::trim).filter(|d| !d.is_empty())?;
    let db = match Database::parse(d) {
        Database::File(f) => Database::File(std::path::absolute(&f).unwrap_or(f)),
        pg => pg,
    };
    if let (Database::Postgres(dsn) | Database::Mysql(dsn) | Database::Sqlserver(dsn), Some(pw)) =
        (&db, ps(p, "password").filter(|pw| !pw.is_empty()))
    {
        remember_password(dsn, pw);
    }
    Some(db)
}

/// The database an SQL query names in `database` ([`database_param`]): a DuckDB file or a
/// PostgreSQL connection, never a MySQL, MariaDB or SQL Server one (read live instead).
#[cfg(not(target_arch = "wasm32"))]
pub fn sql_database_param(p: &Value) -> Result<Option<rustgis_data::source::duckdb::Database>, String> {
    match database_param(p) {
        Some(rustgis_data::source::duckdb::Database::Mysql(_) | rustgis_data::source::duckdb::Database::Sqlserver(_)) => {
            Err("SQL queries don't run on MySQL, MariaDB or SQL Server databases: add their tables with Home › Add › Add Data › Database…"
                .into())
        }
        db => Ok(db),
    }
}

/// The tables of a database (a live connection or a DuckDB file) that have a geometry column.
pub(super) fn database_tables(_s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = p;
        Err("databases are read through DuckDB in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let db = database_param(p).ok_or("database is required")?;
        let err = |e: rustgis_data::DataError| format!("{}: {e}", db.short());
        match LiveDb::connect(&db) {
            // An Esri geodatabase lists its classes and registered tables too: `items` has the
            // rows the connection browser shows, `tables` the labels of the non-spatial ones.
            Some(Ok(LiveDb::Sqlserver(conn))) => {
                let (rows, geodatabase, problem) = super::connections::sqlserver_listing(&conn).map_err(err)?;
                let mut out = super::connections::listing_result(&rows, geodatabase, problem);
                if rows.iter().any(|r| r.get("item").is_some()) {
                    let tables: Vec<&str> =
                        rows.iter().filter(|r| r["item"] == "table").filter_map(|r| r.get("label").and_then(Value::as_str)).collect();
                    out["tables"] = json!(tables);
                    out["items"] = json!(rows);
                }
                Ok(out)
            }
            Some(live) => {
                let layers: Vec<String> = live.map_err(err)?.tables().map_err(err)?.iter().map(LiveTable::label).collect();
                Ok(json!({"count": layers.len(), "layers": layers}))
            }
            None => {
                let layers: Vec<String> = rustgis_data::source::duckdb::tables_of(&db).map_err(err)?.iter().map(|t| t.label()).collect();
                Ok(json!({"count": layers.len(), "layers": layers}))
            }
        }
    }
}

/// Adds the tables of a database (a PostgreSQL connection or a DuckDB file) that have a geometry
/// column, or the one `layer` names.
fn add_database(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("databases are read through DuckDB in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let db = database_param(p).ok_or("database is required")?;
        add_tables(s, p, &db)
    }
}

/// Adds the tables of a database that have a geometry column (or the one `layer` names): a
/// DuckDB file's each converted once into GeoParquet in the cache, a connection's afresh.
#[cfg(not(target_arch = "wasm32"))]
fn add_tables(s: &mut Session, p: &Value, db: &rustgis_data::source::duckdb::Database) -> CmdResult {
    use rustgis_data::source::duckdb::{self, Database};
    if let Some(live) = LiveDb::connect(db) {
        return add_live(s, p, db, live);
    }
    let crs = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty());
    let (path, original) = match db {
        Database::File(f) => {
            (f.display().to_string(), std::path::absolute(f).map(|p| p.display().to_string()).unwrap_or_else(|_| f.display().to_string()))
        }
        Database::Postgres(_) | Database::Mysql(_) | Database::Sqlserver(_) => (db.short(), db.label()),
    };
    let path = path.as_str();
    let err = |e: rustgis_data::DataError| format!("{path}: {e}");
    let tables = match ps(p, "layer").map(str::trim).filter(|l| !l.is_empty()) {
        Some(l) => vec![duckdb::find_table_of(db, l).map_err(err)?],
        None => duckdb::tables_of(db).map_err(err)?,
    };
    if tables.is_empty() {
        return Err(format!("{path}: no tables with a geometry column"));
    }
    // Tables past the cap, said in the result as well as in a notice (as for a GeoPackage).
    let total = tables.len();
    let mut tables = tables;
    let skipped: Vec<String> =
        if total > duckdb::MAX_TABLES { tables.split_off(duckdb::MAX_TABLES).iter().map(|t| t.label()).collect() } else { Vec::new() };
    let mut parts = Vec::new();
    let mut empty = 0usize;
    let single = tables.len() == 1;
    for t in tables {
        let out = match duckdb::convert_table_of(db, &t, crs) {
            Ok(o) => o,
            // Empty tables (common in a PostGIS database: its TIGER geocoder's) are left out quietly.
            Err(rustgis_data::DataError::Format(m)) if !single && m == duckdb::NO_ROWS => {
                empty += 1;
                continue;
            }
            // A table that fails doesn't stop the others.
            Err(e) if !single => {
                s.notify(format!("{path}: {}: {e}", t.label()), true);
                continue;
            }
            Err(e) => return Err(err(e)),
        };
        let origin = |family| rustgis_map::source::DuckdbOrigin {
            sql: None,
            database: Some(original.clone()),
            table: Some(t.label()),
            crs: crs.map(str::to_string),
            family,
        };
        parts.extend(duckdb_parts(s, &t.label(), out, origin));
    }
    if parts.is_empty() && empty > 0 {
        return Err(format!("{path}: the tables with a geometry column are empty"));
    }
    let mut out = add_converted(s, p, path, &original, parts)?;
    // Said once the tables are in: a failure above is the error instead.
    if empty > 0 {
        s.notify(format!("{path}: {empty} empty tables were left out"), false);
    }
    if !skipped.is_empty() {
        s.notify(
            format!(
                "{path}: added the first {} of {total} tables; add others by name with \"layer\" (info.fileLayers lists them)",
                duckdb::MAX_TABLES
            ),
            false,
        );
        out["tables"] = json!(total);
        out["skippedCount"] = json!(skipped.len());
        out["skipped"] = json!(skipped.iter().take(MAX_SKIPPED_NAMED).collect::<Vec<_>>());
    }
    Ok(out)
}

/// A database whose tables are read live: PostGIS, MySQL, MariaDB or SQL Server.
#[cfg(not(target_arch = "wasm32"))]
enum LiveDb {
    Postgis(std::sync::Arc<rustgis_data::source::postgis::Conn>),
    Mysql(std::sync::Arc<rustgis_data::source::mysql::Conn>),
    Sqlserver(std::sync::Arc<rustgis_data::source::sqlserver::Conn>),
}

/// A geometry column of a [`LiveDb`]'s table.
#[cfg(not(target_arch = "wasm32"))]
enum LiveTable {
    Postgis(rustgis_data::source::postgis::Table),
    Mysql(rustgis_data::source::mysql::Table),
    Sqlserver(rustgis_data::source::sqlserver::Table),
}

#[cfg(not(target_arch = "wasm32"))]
impl LiveTable {
    fn label(&self) -> String {
        match self {
            LiveTable::Postgis(t) => t.label(),
            LiveTable::Mysql(t) => t.label(),
            LiveTable::Sqlserver(t) => t.label(),
        }
    }

    /// The name of the layer the table gives: a geodatabase class's alias, else its short name
    /// (as the connection tree shows it); any other table's label.
    fn display(&self) -> String {
        match self {
            LiveTable::Sqlserver(t) => t.alias.clone().or_else(|| t.short.clone()).unwrap_or_else(|| t.label()),
            t => t.label(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl LiveDb {
    /// Connects to a database read live (the user adding it, so the client's own credentials
    /// serve); `None` for a DuckDB file.
    fn connect(db: &rustgis_data::source::duckdb::Database) -> Option<rustgis_data::Result<LiveDb>> {
        use rustgis_data::source::{duckdb::Database, mysql, postgis, sqlserver};
        match db {
            Database::Postgres(dsn) => Some(postgis::connect(dsn, true).map(LiveDb::Postgis)),
            Database::Mysql(address) => Some(mysql::connect(address, true).map(LiveDb::Mysql)),
            Database::Sqlserver(address) => Some(sqlserver::connect(address, true).map(LiveDb::Sqlserver)),
            Database::File(_) => None,
        }
    }

    fn tables(&self) -> rustgis_data::Result<Vec<LiveTable>> {
        use rustgis_data::source::{mysql, postgis, sqlserver};
        Ok(match self {
            LiveDb::Postgis(c) => postgis::tables(c)?.into_iter().map(LiveTable::Postgis).collect(),
            LiveDb::Mysql(c) => mysql::tables(c)?.into_iter().map(LiveTable::Mysql).collect(),
            LiveDb::Sqlserver(c) => sqlserver::tables(c)?.into_iter().map(LiveTable::Sqlserver).collect(),
        })
    }

    fn find(&self, name: &str) -> rustgis_data::Result<LiveTable> {
        use rustgis_data::source::{mysql, postgis};
        Ok(match self {
            LiveDb::Postgis(c) => LiveTable::Postgis(postgis::find_table(c, name)?),
            LiveDb::Mysql(c) => LiveTable::Mysql(mysql::find_table(c, name)?),
            LiveDb::Sqlserver(c) => LiveTable::Sqlserver(find_sqlserver(c, name)?),
        })
    }

    fn families(&self, t: &LiveTable) -> rustgis_data::Result<Vec<rustgis_data::source::Family>> {
        use rustgis_data::source::{mysql, postgis, sqlserver};
        match (self, t) {
            (LiveDb::Postgis(c), LiveTable::Postgis(t)) => postgis::families(c, t),
            (LiveDb::Mysql(c), LiveTable::Mysql(t)) => mysql::families(c, t),
            (LiveDb::Sqlserver(c), LiveTable::Sqlserver(t)) => sqlserver::families(c, t),
            _ => Err(rustgis_data::DataError::Format("the table is of another database".into())),
        }
    }

    fn open(
        &self,
        t: &LiveTable,
        family: rustgis_data::source::Family,
        crs: Option<&str>,
    ) -> rustgis_data::Result<Box<dyn rustgis_data::source::FeatureSource>> {
        use rustgis_data::source::{mysql, postgis, sqlserver};
        Ok(match (self, t) {
            (LiveDb::Postgis(c), LiveTable::Postgis(t)) => Box::new(postgis::PostgisSource::open(c.clone(), t.clone(), Some(family), crs)?),
            (LiveDb::Mysql(c), LiveTable::Mysql(t)) => Box::new(mysql::MysqlSource::open(c.clone(), t.clone(), Some(family), crs)?),
            (LiveDb::Sqlserver(c), LiveTable::Sqlserver(t)) => {
                Box::new(sqlserver::SqlServerSource::open(c.clone(), t.clone(), Some(family), crs)?)
            }
            _ => return Err(rustgis_data::DataError::Format("the table is of another database".into())),
        })
    }
}

/// A SQL Server table by name. In an enterprise geodatabase the catalog reads the name first, so
/// a class is found by its short name or Pro's `db.schema.table` too, and its table carries the
/// class alias, feature dataset and short name; a class whose table has no native spatial column
/// (SDEBINARY or other storage) is refused, saying so. Any other name is looked up as the
/// database's own.
#[cfg(not(target_arch = "wasm32"))]
fn find_sqlserver(
    conn: &rustgis_data::source::sqlserver::Conn,
    name: &str,
) -> rustgis_data::Result<rustgis_data::source::sqlserver::Table> {
    use rustgis_data::source::{geodatabase::ItemKind, sqlserver};
    if let Some(catalog) = sqlserver::geodatabase(conn) {
        let class = |n: &str| catalog.find(n).filter(|i| i.kind == ItemKind::FeatureClass);
        // The name as given first; then as a table of several spatial columns is labelled,
        // `table:column`.
        let (base, column) = split_column(name);
        let found = class(name).map(|i| (i, None)).or_else(|| column.and_then(|c| class(base).map(|i| (i, Some(c)))));
        if let Some((item, column)) = found {
            return class_table(item, &sqlserver::tables(conn)?, column);
        }
    }
    sqlserver::find_table(conn, name)
}

/// A table's name and the column it names after `:`, as [`rustgis_data::source::sqlserver::Table::label`]
/// writes a table of several spatial columns.
#[cfg(not(target_arch = "wasm32"))]
fn split_column(name: &str) -> (&str, Option<&str>) {
    match name.rsplit_once(':') {
        Some((table, column)) if !column.trim().is_empty() => (table, Some(column.trim())),
        _ => (name, None),
    }
}

/// The spatial column of the database's `tables` a registered feature class reads, carrying the
/// class alias, feature dataset and short name. A `column` asked for by name is the one read,
/// whichever of the table's it is (the definition speaks for the shape column only), and an
/// error when the table has no such spatial column. Without one it is the class's shape column,
/// else its table's first ([`fallbacks_note`] says so). A class with none is stored in a format
/// RustGIS doesn't read (SDEBINARY or other storage).
#[cfg(not(target_arch = "wasm32"))]
fn class_table(
    item: &rustgis_data::source::geodatabase::Item,
    tables: &[rustgis_data::source::sqlserver::Table],
    column: Option<&str>,
) -> rustgis_data::Result<rustgis_data::source::sqlserver::Table> {
    let full = format!("{}.{}", item.schema, item.table);
    let named = |n: &str| tables.iter().find(|t| t.is_named(n));
    let found = match column {
        Some(column) => Some(
            named(&format!("{full}:{column}"))
                .ok_or_else(|| rustgis_data::DataError::Format(format!("{} has no geometry or geography column '{column}'", item.name)))?,
        ),
        None => {
            let shape = item.shape_column.as_deref();
            // A cut list may hold another column of the table and not its shape column: that one
            // isn't read in its place.
            let cut = shape.is_some() && rustgis_data::source::sqlserver::tables_cut(tables);
            shape.and_then(|shape| named(&format!("{full}:{shape}"))).or_else(|| named(&full).filter(|_| !cut))
        }
    };
    let Some(t) = found else {
        // A cut list can't say the table has no spatial column.
        let why = if rustgis_data::source::sqlserver::tables_cut(tables) {
            format!("{}: its spatial column wasn't found: {}", item.name, rustgis_data::source::sqlserver::tables_cut_note())
        } else {
            format!("{} is {UNREAD_STORAGE}", item.name)
        };
        return Err(rustgis_data::DataError::Unsupported(why));
    };
    Ok(rustgis_data::source::sqlserver::Table {
        alias: item.alias.clone(),
        dataset: item.dataset.clone(),
        short: Some(item.short_name().to_string()),
        ..t.clone()
    })
}

/// Why a class whose table holds no native spatial column isn't read.
#[cfg(not(target_arch = "wasm32"))]
const UNREAD_STORAGE: &str = "stored in a format RustGIS doesn't read (SDEBINARY or another non-native storage)";

/// A class read from a column other than the shape column it registers, which isn't among its
/// table's spatial columns, as [`fallbacks_note`] names it; `None` when it is read from its shape
/// column or registers none.
#[cfg(not(target_arch = "wasm32"))]
fn fallback(item: &rustgis_data::source::geodatabase::Item, table: &rustgis_data::source::sqlserver::Table) -> Option<String> {
    let shape = item.shape_column.as_deref().filter(|shape| !shape.eq_ignore_ascii_case(&table.column))?;
    Some(format!("{} ({}, not {shape})", item.name, table.column))
}

/// The first few of `names`, and how many more there are.
#[cfg(not(target_arch = "wasm32"))]
fn first_names(names: &[String]) -> String {
    const NAMED: usize = 5;
    let shown = names.iter().take(NAMED).map(String::as_str).collect::<Vec<_>>().join(", ");
    match names.len().saturating_sub(NAMED) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}

/// The one notice for the classes read from another column than their shape column
/// ([`fallback`]), with their count.
#[cfg(not(target_arch = "wasm32"))]
fn fallbacks_note(classes: &[String]) -> Option<String> {
    let names = first_names(classes);
    match classes.len() {
        0 => None,
        1 => Some(format!(
            "1 feature class is read from a spatial column other than the shape column it registers, which isn't a geometry or geography column: {names}"
        )),
        n => Some(format!(
            "{n} feature classes are read from a spatial column other than the shape column they register, which isn't a geometry or geography column: {names}"
        )),
    }
}

/// What adding a whole geodatabase leaves out or reads another way.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct GdbLeft {
    /// The registered tables, which are added by name.
    tables: usize,
    /// The classes whose tables hold no native spatial column, by name.
    unread: Vec<String>,
    /// The classes read from another column than their shape column ([`fallback`]).
    fallbacks: Vec<String>,
    /// That the database's list of spatial columns was cut, when it was.
    cut: Option<String>,
}

/// The layers adding a whole geodatabase adds (its classes with a spatial column and its other
/// spatial tables), and what it leaves.
#[cfg(not(target_arch = "wasm32"))]
fn geodatabase_layers(listing: rustgis_data::source::sqlserver::GdbListing) -> (Vec<rustgis_data::source::sqlserver::Table>, GdbLeft) {
    let mut left = GdbLeft { tables: listing.tables.len(), cut: listing.cut_note(), ..Default::default() };
    let mut tables = Vec::new();
    for (item, table) in listing.classes {
        match table {
            Some(table) => {
                left.fallbacks.extend(fallback(&item, &table));
                tables.push(table);
            }
            None => left.unread.push(item.name),
        }
    }
    tables.extend(listing.other);
    (tables, left)
}

/// The notice for the classes a whole-geodatabase add leaves out for want of a spatial column,
/// naming the first few. With the list of spatial columns `cut`, that isn't put down to their
/// storage: their columns may be past the cut.
#[cfg(not(target_arch = "wasm32"))]
fn unread_note(unread: &[String], cut: bool) -> Option<String> {
    let names = first_names(unread);
    match (unread.len(), cut) {
        (0, _) => None,
        (1, false) => Some(format!("1 feature class was left out: {UNREAD_STORAGE}: {names}")),
        (n, false) => Some(format!("{n} feature classes were left out: {UNREAD_STORAGE}: {names}")),
        (1, true) => Some(format!("1 feature class was left out: no spatial column was found for it among those read: {names}")),
        (n, true) => Some(format!("{n} feature classes were left out: no spatial column was found for them among those read: {names}")),
    }
}

/// The notice for the registered tables a whole-geodatabase add leaves to be named.
#[cfg(not(target_arch = "wasm32"))]
fn unadded_note(tables: usize) -> String {
    format!("{tables} tables not added; add one by name with \"layer\"")
}

/// An add's error with what the add left (`notes`) after it.
#[cfg(not(target_arch = "wasm32"))]
fn with_left(error: String, notes: &[String]) -> String {
    if notes.is_empty() { error } else { format!("{error}; {}", notes.join("; ")) }
}

/// The fewest rows a registered table is read to, however little memory there is.
#[cfg(not(target_arch = "wasm32"))]
const TABLE_ROWS_FLOOR: usize = 10_000;

/// What a row of a registered table is planned at: a row of a hundred fields, about 5 KiB loaded
/// (AGENTS.md, "Size for real data").
#[cfg(not(target_arch = "wasm32"))]
const TABLE_ROW_BYTES: u64 = 5 << 10;

/// The most rows of a registered table read at once: `MAX_EDITABLE`, fewer once RustGIS holds
/// more than half the memory, never below the floor ([`rustgis_data::memory::planned`], #2431).
/// A table with more rows is read to the limit, and the read says so with its row count.
#[cfg(not(target_arch = "wasm32"))]
fn table_rows() -> usize {
    rustgis_data::memory::planned(TABLE_ROWS_FLOOR, rustgis_data::source::sqlserver::MAX_EDITABLE, TABLE_ROW_BYTES)
}

/// Adds a registered non-spatial table of an enterprise geodatabase as a standalone table,
/// loaded whole (through its versioned view when it has one), up to [`table_rows`]: one copy of
/// its rows, as any loaded table holds. It runs on the calling thread as the rest of
/// `layer.addDatabase` does: the work is one bounded statement the server answers or times out,
/// with no loop of its own between whose items Cancel could be looked at.
#[cfg(not(target_arch = "wasm32"))]
fn add_geodatabase_table(
    s: &mut Session,
    p: &Value,
    conn: &rustgis_data::source::sqlserver::Conn,
    item: &rustgis_data::source::geodatabase::Item,
    label: &str,
) -> CmdResult {
    let read = rustgis_data::source::sqlserver::read_table(conn, item, table_rows()).map_err(|e| format!("{label}: {e}"))?;
    let rows = read.0.features.len();
    let added = add_read(s, p, read, None)?;
    Ok(table_result(&added["id"], rows))
}

/// What adding a registered table answers with: no layer, the table's id, and the rows read as
/// its `features`.
#[cfg(not(target_arch = "wasm32"))]
fn table_result(id: &Value, rows: usize) -> Value {
    json!({"layer": null, "layers": [], "tables": [id], "features": rows})
}

/// What to say of a class added by the name `wanted` when it is read from another column than
/// its shape column ([`fallback`]); nothing when `wanted` names the column, which is then the
/// one asked for.
#[cfg(not(target_arch = "wasm32"))]
fn named_fallback(
    wanted: &str,
    item: &rustgis_data::source::geodatabase::Item,
    table: &rustgis_data::source::sqlserver::Table,
) -> Option<String> {
    if split_column(wanted).1.is_some() { None } else { fallback(item, table) }
}

/// Adds the tables of a PostGIS, MySQL, MariaDB or SQL Server database that have a geometry
/// column (or the one `layer` names), read live: large ones a view at a time, others loaded
/// whole. A column of mixed geometry types becomes a layer per type.
#[cfg(not(target_arch = "wasm32"))]
fn add_live(s: &mut Session, p: &Value, db: &rustgis_data::source::duckdb::Database, live: rustgis_data::Result<LiveDb>) -> CmdResult {
    use rustgis_data::source::duckdb::MAX_TABLES;
    let label = db.short();
    let original = db.label();
    let err = |e: rustgis_data::DataError| format!("{label}: {e}");
    let crs = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty());
    let live = live.map_err(err)?;
    let wanted = ps(p, "layer").map(str::trim).filter(|l| !l.is_empty());
    // An enterprise geodatabase: a registered table is added whole, as a standalone table; all
    // its classes are added without a name, and the tables it registers are left to be named.
    // A class that can't be read, or is read from another column than its shape column, is said.
    let mut unadded = 0;
    let mut said: Vec<String> = Vec::new();
    let mut tables = match (&live, wanted) {
        (LiveDb::Sqlserver(conn), Some(l)) => {
            let table = rustgis_data::source::sqlserver::geodatabase(conn)
                .and_then(|catalog| catalog.find(l).filter(|i| i.kind == rustgis_data::source::geodatabase::ItemKind::Table).cloned());
            if let Some(item) = table {
                return add_geodatabase_table(s, p, conn, &item, &label);
            }
            let found = live.find(l).map_err(err)?;
            // A column asked for by name is the one wanted, so only the class's own choice is said.
            if let LiveTable::Sqlserver(t) = &found
                && let Some(catalog) = rustgis_data::source::sqlserver::geodatabase(conn)
                && let Some(item) = catalog.find(l).filter(|i| i.kind == rustgis_data::source::geodatabase::ItemKind::FeatureClass)
            {
                said.extend(fallbacks_note(&named_fallback(l, item, t).into_iter().collect::<Vec<_>>()));
            }
            vec![found]
        }
        (LiveDb::Sqlserver(conn), None) => match rustgis_data::source::sqlserver::geodatabase_tables(conn).map_err(err)? {
            Some(listing) => {
                let (tables, left) = geodatabase_layers(listing);
                unadded = left.tables;
                said.extend(left.cut.clone());
                said.extend(unread_note(&left.unread, left.cut.is_some()));
                said.extend(fallbacks_note(&left.fallbacks));
                tables.into_iter().map(LiveTable::Sqlserver).collect()
            }
            None => live.tables().map_err(err)?,
        },
        (_, Some(l)) => vec![live.find(l).map_err(err)?],
        (_, None) => live.tables().map_err(err)?,
    };
    if unadded > 0 {
        said.push(unadded_note(unadded));
    }
    // Said once the add succeeds; a failed command's notices are withdrawn, so a failure carries
    // them in its error.
    if tables.is_empty() {
        return Err(with_left(format!("{label}: no tables with a geometry column"), &said));
    }
    let total = tables.len();
    let skipped: Vec<String> =
        if total > MAX_TABLES { tables.split_off(MAX_TABLES).iter().map(|t| t.label()).collect() } else { Vec::new() };
    let single = tables.len() == 1;
    let empty_before = super::map_is_empty(s);
    let (mut ids, mut features, mut streamed, mut empty) = (Vec::new(), 0u64, false, 0usize);
    for t in tables {
        // A table that fails doesn't stop the others.
        let fail = |s: &mut Session, e: String| -> Result<(), String> {
            if single {
                // The only one: its failure is the command's, carrying what the add left.
                return Err(with_left(e, &said));
            }
            s.notify(format!("{label}: {}: {e}", t.label()), true);
            Ok(())
        };
        let mut families = match live.families(&t) {
            Ok(f) => f,
            Err(e) => {
                fail(s, e.to_string())?;
                continue;
            }
        };
        let mixed = families.len() > 1;
        families.sort_by_key(|f| bottom_first(*f));
        // A table is empty when none of its families has rows, not when they failed.
        let (mut added, mut failed) = (false, false);
        for family in families {
            let src = match live.open(&t, family, crs) {
                Ok(src) => src,
                Err(e) => {
                    failed = true;
                    fail(s, e.to_string())?;
                    continue;
                }
            };
            if src.info().count == Some(0) {
                continue;
            }
            let name = match ps(p, "name").map(str::trim).filter(|n| !n.is_empty() && single && !mixed) {
                Some(n) => n.to_string(),
                None if mixed => format!("{} ({})", t.display(), family.label()),
                None => t.display(),
            };
            let (database, table, family, crs) = (original.clone(), t.label(), Some(family), crs.map(str::to_string));
            let (pg, my, ms);
            let origin = match live {
                LiveDb::Postgis(_) => {
                    pg = rustgis_map::source::PostgisOrigin { database, table, family, crs };
                    Origin::Postgis(&pg)
                }
                LiveDb::Mysql(_) => {
                    my = rustgis_map::source::MysqlOrigin { database, table, family, crs };
                    Origin::Mysql(&my)
                }
                LiveDb::Sqlserver(_) => {
                    ms = rustgis_map::source::SqlServerOrigin { database, table, family, crs };
                    Origin::Sqlserver(&ms)
                }
            };
            let mut q = json!({"name": name});
            if let Some(v) = p.get("stream") {
                q["stream"] = v.clone();
            }
            let r = match add_source_with(s, &q, origin, Some(src), None) {
                Ok(r) => r,
                Err(e) => {
                    failed = true;
                    fail(s, e)?;
                    continue;
                }
            };
            added = true;
            features += r["features"].as_u64().unwrap_or(0);
            streamed |= r["streamed"].as_bool().unwrap_or(false);
            ids.extend(r["layers"].as_array().into_iter().flatten().filter_map(Value::as_u64).filter_map(|i| u32::try_from(i).ok()));
        }
        if !added && !failed {
            empty += 1;
        }
    }
    if ids.is_empty() {
        let error = if empty > 0 && single {
            format!("{label}: the table is empty")
        } else if empty > 0 {
            format!("{label}: the tables with a geometry column are empty")
        } else {
            format!("{label}: none of its tables could be added")
        };
        return Err(with_left(error, &said));
    }
    if empty_before {
        s.zoom_to_layers(&ids, rustgis_map::ExtentOf::All);
    }
    // Empty tables (common in a PostGIS database: its TIGER geocoder's) are left out quietly.
    if empty > 0 {
        s.notify(format!("{label}: {empty} empty tables were left out"), false);
    }
    for note in said {
        s.notify(format!("{label}: {note}"), false);
    }
    let mut out = json!({"layer": ids.first(), "layers": ids, "features": features, "streamed": streamed});
    if !skipped.is_empty() {
        s.notify(format!("{label}: added the first {MAX_TABLES} of {total} tables; add others by name with \"layer\" (info.databaseTables lists them)"), false);
        out["tables"] = json!(total);
        out["skippedCount"] = json!(skipped.len());
        out["skipped"] = json!(skipped.iter().take(MAX_SKIPPED_NAMED).collect::<Vec<_>>());
    }
    Ok(out)
}

/// Runs a DuckDB query and adds its result as a layer (a layer per geometry type it mixes).
fn add_sql(s: &mut Session, p: &Value) -> CmdResult {
    let sql = s_req(p, "sql")?;
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, sql);
        Err("SQL queries run through DuckDB in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_data::source::duckdb;
        let crs = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty());
        let database = sql_database_param(p)?;
        let full = database.as_ref().map(|d| d.label());
        // A result the app ran off the frame (the SQL pane), taken only for the query and database
        // it came from (the layer saves them); or the query run now.
        let ran = p.get("ran").and_then(Value::as_u64);
        let out = match ran {
            Some(id) => {
                let (_, ran_sql, ran_db, out) =
                    s.sql_results.iter().find(|(r, ..)| *r == id).ok_or("that query result is no longer kept: run the query again")?;
                if ran_sql != sql || *ran_db != full {
                    return Err("that query result came from another query or database: run the query again".into());
                }
                out.clone()
            }
            None => duckdb::run(&duckdb::Query { sql, database: database.as_ref(), crs }).map_err(|e| e.to_string())?,
        };
        let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Query");
        let origin = |family| rustgis_map::source::DuckdbOrigin {
            sql: Some(sql.to_string()),
            database: full.clone(),
            table: None,
            crs: crs.map(str::to_string),
            family,
        };
        let parts = duckdb_parts(s, name, out, origin);
        // The name given names the layer, or the layers by geometry type.
        let mut q = p.clone();
        if let Some(o) = q.as_object_mut() {
            o.remove("name");
        }
        let added = add_converted(s, &q, "the query", full.as_deref().unwrap_or("DuckDB query"), parts)?;
        // The result is used: forgotten once its layers are in (kept if adding failed).
        if let Some(id) = ran {
            s.sql_results.retain(|(r, ..)| *r != id);
        }
        Ok(added)
    }
}

/// The layers of a DuckDB result: named `name`, or `name (points)` and so on when it mixes
/// geometry types, then bottom first ([`bottom_first`]).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn duckdb_parts(
    s: &mut Session,
    name: &str,
    mut out: rustgis_data::source::duckdb::Output,
    origin: impl Fn(rustgis_data::source::Family) -> rustgis_map::source::DuckdbOrigin,
) -> Vec<Converted> {
    if out.skipped > 0 {
        s.notify(format!("{name}: {} rows without a geometry were left out", out.skipped), false);
    }
    // Z and M values are dropped as DuckDB writes the result: one notice for all its parts.
    let zm = out.parts.iter().fold(0u64, |n, p| n.saturating_add(p.zm_dropped));
    if zm > 0 {
        s.notify_loss(rustgis_data::loss::LossCode::DroppedZm, format!("{name}: {}", rustgis_data::zm_note(Some(zm))).into());
    }
    let mixed = out.parts.len() > 1;
    out.parts.sort_by_key(|p| bottom_first(p.family));
    out.parts
        .into_iter()
        .map(|part| Converted {
            path: part.path,
            name: if mixed { format!("{name} ({})", part.family.label()) } else { name.to_string() },
            // A geometry that says no coordinate system is usually longitude/latitude: only
            // coordinates past it are pointed out (in add_converted).
            unprojected: false,
            in_lonlat: false,
            zm_dropped: part.zm_dropped > 0,
            origin: ConvertedOrigin::Duckdb(origin(part.family)),
        })
        .collect()
}

/// A layer another program (GDAL, DuckDB) converted into the cache.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Converted {
    path: std::path::PathBuf,
    name: String,
    /// Its coordinates are the source's own, in a coordinate system it doesn't say.
    unprojected: bool,
    /// Its coordinate system is one RustGIS can't read, so it was converted to longitude/latitude.
    in_lonlat: bool,
    /// The conversion dropped Z or M values (said as it ran), kept with the layer so an export
    /// says so too.
    zm_dropped: bool,
    origin: ConvertedOrigin,
}

#[cfg(not(target_arch = "wasm32"))]
enum ConvertedOrigin {
    Gdal(rustgis_map::source::GdalOrigin),
    Duckdb(rustgis_map::source::DuckdbOrigin),
}

/// Adds converted layers like the files they are (streamed when large), remembering where they came
/// from so a project can convert them again. `label` names the source in messages, `original` is
/// the layers' source.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn add_converted(s: &mut Session, p: &Value, label: &str, original: &str, converted: Vec<Converted>) -> CmdResult {
    let empty_before = super::map_is_empty(s);
    let parts = converted.len();
    let single = parts == 1;
    let (mut ids, mut features, mut streamed) = (Vec::new(), 0u64, false);
    let mut converted_lonlat: Vec<String> = Vec::new();
    let source_layer = |o: &ConvertedOrigin| match o {
        ConvertedOrigin::Gdal(g) => g.layer.clone(),
        ConvertedOrigin::Duckdb(d) => d.table.clone().unwrap_or_default(),
    };
    for c in converted {
        let mut q = json!({"name": c.name});
        if let Some(n) = ps(p, "name").filter(|_| single) {
            q["name"] = json!(n);
        }
        if let Some(v) = p.get("stream") {
            q["stream"] = v.clone();
        }
        let file = c.path.display().to_string();
        // One layer that can't be added doesn't stop the others.
        let added = match &c.origin {
            ConvertedOrigin::Gdal(_) => {
                rustgis_data::source::fgb::FgbSource::open_converted_path(&c.path, &c.name, Path::new(original), None)
                    .map_err(|e| format!("{original}: {e}"))
                    .and_then(|opened| add_source_with(s, &q, Origin::Path(&file), Some(Box::new(opened)), None))
            }
            ConvertedOrigin::Duckdb(d) => {
                use rustgis_data::source::{duckdb::Database, parquet::ParquetSource};
                let database = d.database.as_deref().map(Database::parse);
                let original = match &database {
                    Some(Database::File(path)) => Some(path.as_path()),
                    _ => None,
                };
                ParquetSource::open_converted_path(&c.path, &c.name, original, None)
                    .map_err(|e| format!("{file}: {e}"))
                    .and_then(|opened| add_source_with(s, &q, Origin::Path(&file), Some(Box::new(opened)), None))
            }
        };
        let r = match added {
            Ok(r) => r,
            Err(e) if !single => {
                s.notify(format!("{}: {e}", c.name), true);
                continue;
            }
            Err(e) => return Err(e),
        };
        features += r["features"].as_u64().unwrap_or(0);
        streamed |= r["streamed"].as_bool().unwrap_or(false);
        for id in r["layers"].as_array().into_iter().flatten().filter_map(Value::as_u64).filter_map(|i| u32::try_from(i).ok()) {
            if let Some(l) = s.map_mut().layer_mut(id) {
                l.dataset.source = Some(original.to_string());
                l.dataset.zm_dropped |= c.zm_dropped;
                if let Some(src) = &mut l.source {
                    match &c.origin {
                        ConvertedOrigin::Gdal(g) => src.gdal = Some(g.clone()),
                        ConvertedOrigin::Duckdb(d) => src.duckdb = Some(d.clone()),
                    }
                }
            }
            // The source couldn't be reprojected, or its coordinates go past longitude/latitude:
            // its coordinate system is unknown.
            let e = s.map().layer(id).map(|l| l.extent()).unwrap_or_default();
            let outside = !e.is_empty() && (e.min_x < -181.0 || e.max_x > 181.0 || e.min_y < -91.0 || e.max_y > 91.0);
            // Said once for the file below, naming each source layer once, not each part of it.
            if c.in_lonlat {
                let layer = source_layer(&c.origin);
                if !converted_lonlat.contains(&layer) {
                    converted_lonlat.push(layer);
                }
            }
            if outside || (c.unprojected && matches!(c.origin, ConvertedOrigin::Gdal(_))) {
                s.notify(
                    format!("{}: the data doesn't say its coordinate system; add it again with \"crs\" (such as EPSG:2263)", c.name),
                    true,
                );
            }
            ids.push(id);
        }
    }
    if !converted_lonlat.is_empty() {
        let names: Vec<&str> = converted_lonlat.iter().map(String::as_str).collect();
        s.notify(rustgis_data::source::gdal::in_lonlat_notes(&names), false);
    }
    if empty_before {
        s.zoom_to_layers(&ids, rustgis_map::ExtentOf::All);
    }
    if ids.is_empty() {
        return Err(format!("{label}: none of its layers could be added"));
    }
    if !single && let Some(name) = ps(p, "name") {
        s.notify_text(
            crate::notice_text::NoticeText::format(
                "Name '{name}' was not used: {parts} converted layer parts keep their generated names",
                [("name", crate::project::short_name(name).into()), ("parts", parts.to_string().into())],
            ),
            false,
        );
    }
    Ok(json!({"layer": ids.first(), "layers": ids, "features": features, "streamed": streamed}))
}

fn unique_name(s: &Session, base: &str) -> String {
    let mut name = base.to_string();
    let mut i = 1;
    while s.map().layer_by_name(&name).is_some() {
        i += 1;
        name = format!("{base} ({i})");
    }
    name
}

/// The active (or named) layer is streamed.
fn need_streamed(s: &Session) -> Option<&'static str> {
    match s.active_layer.and_then(|id| s.map().layer(id)) {
        Some(l) if l.is_streamed() => None,
        Some(_) => Some("this layer's features are already in memory"),
        None => Some("select a layer in the Layers pane first"),
    }
}

/// Reinterprets the layer's native coordinates in a replacement CRS. Validate the complete
/// replacement before mutating the layer; undo then restores geometry, metadata and caches.
fn define_projection(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let crs = super::parse_crs(s, s_req(p, "crs")?)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    if l.is_raster() || l.is_streamed() {
        return Err(
            "Define Projection needs a vector layer loaded into memory; import a Shapefile with a crs override for a streamed layer".into(),
        );
    }
    let ds = rustgis_data::io::define_projection(&l.dataset, crs).map_err(|e| e.to_string())?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    l.dataset = ds;
    l.touch_geometry();
    Ok(json!({"layer": id, "crs": crs.label()}))
}

/// Loads a streamed layer's features into memory, so it can be edited like any other layer.
fn load_all(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let (mut ds, unread) = super::whole::take_or_read(s, id)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    // Named as the layer is now: it may have been renamed while the read ran.
    ds.name = l.dataset.name.clone();
    let n = ds.features.len();
    unread.say(s);
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    l.dataset = ds;
    l.dataset.windowed = false;
    l.source = None;
    l.selection.clear();
    l.touch_geometry();
    Ok(json!({"layer": id, "features": n}))
}

/// Load All's read of the streamed layer `p` names, made before it runs ([`super::reads`]), on a
/// copy of the layer.
pub(crate) fn load_all_ahead(s: &Session, p: &Value) -> Option<super::reads::ReadWork> {
    super::whole::ahead(s, p, "Load All Features")
}

/// Adds a layer file: a layer with its data, symbology and labels.
fn add_layer_file(s: &mut Session, path: &str) -> CmdResult {
    let bytes = crate::project::read_file(Path::new(path), 512 << 20)?;
    let text = String::from_utf8(bytes).map_err(|_| format!("{path} isn't a RustGIS layer file"))?;
    add_layer_text(s, &text)
}

/// Adds a layer from a layer file's JSON.
pub(crate) fn add_layer_text(s: &mut Session, text: &str) -> CmdResult {
    crate::project::check_format(text, "layer file")?;
    let mut layer: rustgis_map::Layer = serde_json::from_str(text).map_err(|e| format!("not a RustGIS layer file: {e}"))?;
    if layer.dataset.source.as_deref().is_some_and(|source| source.starts_with("geolibre-pending:")) {
        return Err("a layer file cannot contain runtime GeoLibre download reservations".into());
    }
    for notice in crate::project::label_losses::layer_notices(text, &layer, true) {
        s.notify(notice, false);
    }
    for notice in crate::project::picture_notices([&layer], []) {
        s.notify_text(notice, false);
    }
    // A binding in a file is no authority to use a login here.
    crate::s3_connections::clear_bindings(layer.addresses_mut());
    crate::project::clear_login_bindings(layer.addresses_mut());
    // A fresh id and a unique name in this map; the rest goes through project repair.
    let base = layer.name.clone();
    let mut i = 1;
    while s.map().layer_by_name(&layer.name).is_some() {
        i += 1;
        layer.name = format!("{base} ({i})");
    }
    let id = s.map_mut().add_layer(layer);
    super::connections::clear_active();
    for notice in s.project.normalize_text() {
        s.notify_text(notice, false);
    }
    super::connections::restore_active(s);
    s.wait_in_place_reads();
    super::project::say_conversion_fallbacks(s);
    // Only the database the added layer reads can change here.
    let keys: Vec<_> = s.map().layer(id).and_then(|l| l.source.as_ref()).map(super::connections::layer_keys).unwrap_or_default();
    super::connections::resolve_keys(s, &keys);
    crate::s3_connections::resolve_project_s3(s);
    super::connections::note_waiting(s);
    s.active_layer = Some(id);
    // What reading its raster left out is said, as for a raster added (one still read in the
    // background isn't here yet).
    let lost: Vec<String> = s
        .map()
        .layer(id)
        .and_then(|l| Some((&l.name, l.raster.as_ref()?.data.as_ref()?)))
        .map(|(name, d)| d.lost.iter().map(|l| format!("{name}: {l}")).collect())
        .unwrap_or_default();
    for l in lost {
        s.notify(l, false);
    }
    Ok(json!({"layer": id}))
}

/// The most polylines added at once.
const MAX_POLYLINES: usize = 10_000;

/// Adds encoded polylines (routes from Google, OSRM or Valhalla) as a line layer.
fn add_encoded_polyline(s: &mut Session, p: &Value) -> CmdResult {
    use rustgis_data::{Dataset, Field, FieldType, Geometry, Value as V};
    let texts: Vec<&str> = match p.get("polyline") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).take(MAX_POLYLINES + 1).collect(),
        _ => return Err("polyline is required (text, or a list of texts)".into()),
    };
    if texts.len() > MAX_POLYLINES {
        return Err(format!("at most {MAX_POLYLINES} polylines at once"));
    }
    let precision = match f(p, "precision") {
        Some(v) if v.fract() == 0.0 && (1.0..=10.0).contains(&v) => v as u32,
        Some(v) => return Err(format!("precision {v}: give a whole number from 1 to 10 (5 for Google and OSRM, 6 for Valhalla)")),
        None => 5,
    };
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Polyline");
    let mut ds =
        Dataset::new(name, GeometryType::Polyline, vec![Field::new("index", FieldType::Integer), Field::new("points", FieldType::Integer)]);
    for (i, t) in texts.iter().enumerate() {
        let pts = rustgis_data::interop::decode_polyline(t, precision).map_err(|e| format!("polyline {}: {e}", i + 1))?;
        if pts.len() < 2 {
            return Err(format!("polyline {}: a line needs at least two points", i + 1));
        }
        let n = pts.len() as i64;
        ds.push_next(Some(Geometry::Line(vec![pts])), vec![V::Int(i as i64 + 1), V::Int(n)]);
    }
    add_read(s, p, (ds, rustgis_data::ReadReport::default()), None)
}

/// Adds a sheet of a spreadsheet (`"layer"` names it; the first by default), read like delimited
/// text with the same options.
pub(crate) fn add_sheet(s: &mut Session, p: &Value, file_name: &str, bytes: &[u8], zoom: Option<bool>) -> CmdResult {
    let read = read_sheet(p, file_name, bytes)?;
    add_read(s, p, read, zoom)
}

fn read_sheet(p: &Value, file_name: &str, bytes: &[u8]) -> Result<(rustgis_data::Dataset, rustgis_data::ReadReport), String> {
    let stem = Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("sheet");
    let opts = csv_options(p)?.unwrap_or_default();
    let sheet = ps(p, "layer").map(str::trim).filter(|l| !l.is_empty());
    let (dataset, mut report) = rustgis_data::sheets::read(bytes, stem, sheet, &opts).map_err(|e| format!("{file_name}: {e}"))?;
    if let (Some(text), Some(crs)) = (ps(p, "crs"), opts.crs)
        && let Some(note) = rustgis_data::raster::wkt::typed_crs_note(text, crs)
    {
        report.messages.push(format!("crs: {note}"));
    }
    Ok((dataset, report))
}

/// The `x`, `y`, `wkt`, `crs` and `delimiter` parameters for a delimited text file, when any is
/// given.
pub(crate) fn csv_options(p: &Value) -> Result<Option<rustgis_data::io::CsvOptions>, String> {
    let text = |k: &str| ps(p, k).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
    let (x, y, wkt, crs, delimiter) = (text("x"), text("y"), text("wkt"), text("crs"), ps(p, "delimiter"));
    if x.is_none() && y.is_none() && wkt.is_none() && crs.is_none() && delimiter.is_none() {
        return Ok(None);
    }
    let crs = crs.map(|c| rustgis_data::raster::Crs::parse(&c)).transpose()?;
    let delimiter = parse_delimiter(delimiter)?;
    Ok(Some(rustgis_data::io::CsvOptions { x, y, wkt, crs, delimiter }))
}

/// A `delimiter` parameter: one character, or `\t`/`tab`.
fn parse_delimiter(d: Option<&str>) -> Result<Option<u8>, String> {
    match d {
        None => Ok(None),
        Some("\\t" | "tab" | "\t") => Ok(Some(b'\t')),
        Some(d) if d.len() == 1 && d.is_ascii() && !d.starts_with(['"', '\n', '\r']) => Ok(d.bytes().next()),
        Some(d) => Err(format!("delimiter '{d}': give one character, such as ; or \\t")),
    }
}

fn file_columns(_s: &mut Session, p: &Value) -> CmdResult {
    columns_in_file(s_req(p, "path")?, p)
}

/// `info.fileColumns` for the file at `path` (the desktop app's Add Delimited Text).
pub fn columns_in_file(path: &str, p: &Value) -> CmdResult {
    use std::io::Read;
    if !Path::new(path).is_file() {
        return Err(format!("{path}: no such file"));
    }
    // Add Data's limits: a spreadsheet is read whole, delimited text within the text readers' limit.
    let sheet = rustgis_data::sheets::is_sheet_name(path);
    let limit = if sheet { super::raster::MAX_UPLOAD_BYTES as u64 } else { rustgis_data::io::max_bytes() };
    let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    if meta.len() > limit {
        return Err(rustgis_data::io::file_size_error(path, limit));
    }
    if rustgis_data::io::is_gzip_text_name(path) {
        let reader = rustgis_data::provenance::open(path).map_err(|e| format!("{path}: {e}"))?;
        let prefix = rustgis_data::io::gzip_text_prefix(reader, COLUMNS_PREFIX).map_err(|e| format!("{path}: {e}"))?;
        let plain = path.get(..path.len().saturating_sub(3)).unwrap_or(path);
        return gzip_columns(plain, &prefix, p);
    }
    let mut bytes = Vec::new();
    let mut file = rustgis_data::provenance::open(path).map_err(|e| format!("{path}: {e}"))?;
    let read = if sheet { file.read_to_end(&mut bytes) } else { (&file).take(COLUMNS_PREFIX as u64).read_to_end(&mut bytes) };
    read.map_err(|e| format!("{path}: {e}"))?;
    if sheet {
        rustgis_data::provenance::buffer(Path::new(path), &file, &bytes);
    }
    columns_of(path, &bytes, p)
}

/// Gzip import accepts UTF-8 only. A bounded picker prefix may end inside one character.
fn gzip_columns(plain: &str, prefix: &[u8], p: &Value) -> CmdResult {
    let text = match std::str::from_utf8(prefix) {
        Ok(text) => text,
        Err(e) if e.error_len().is_none() => std::str::from_utf8(prefix.get(..e.valid_up_to()).unwrap_or_default()).unwrap_or_default(),
        Err(_) => return Err(format!("{plain}.gz: gzip text must be UTF-8; decompress and add as .txt for another encoding")),
    };
    columns_of(plain, text.as_bytes(), p)
}

/// What `info.fileColumns` reads of delimited text: its header and first rows, not all of it.
const COLUMNS_PREFIX: usize = 8 << 20;

/// The columns of a delimited text file or a spreadsheet's sheet (`"layer"`), with the separator
/// (`"delimiter"`, or the one found) and the x/y or WKT columns found for `"crs"`:
/// `{"columns", "x", "y", "wkt", "delimiter"}`. `info.fileColumns` for a file's bytes (the web
/// app's Add Delimited Text).
pub fn columns_of(name: &str, bytes: &[u8], p: &Value) -> CmdResult {
    let err = |e: rustgis_data::DataError| format!("{name}: {e}");
    let crs = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty()).map(rustgis_data::raster::Crs::parse).transpose()?;
    let c = if rustgis_data::sheets::is_sheet_name(name) {
        let sheet = ps(p, "layer").map(str::trim).filter(|l| !l.is_empty());
        rustgis_data::sheets::columns(bytes, sheet, crs).map_err(err)?
    } else if rustgis_data::io::is_gzip_text_name(name) {
        let prefix = rustgis_data::io::gzip_text_prefix(bytes, COLUMNS_PREFIX).map_err(err)?;
        let plain = name.get(..name.len().saturating_sub(3)).unwrap_or(name);
        return gzip_columns(plain, &prefix, p);
    } else if is_delimited_name(name) {
        let bytes = bytes.get(..COLUMNS_PREFIX).unwrap_or(bytes);
        let text = match std::str::from_utf8(bytes) {
            Ok(text) => text.into(),
            // The part read may end inside a character.
            Err(e) if e.error_len().is_none() => {
                std::str::from_utf8(bytes.get(..e.valid_up_to()).unwrap_or_default()).unwrap_or_default().into()
            }
            // Another encoding (Latin-1, UTF-16), as reading the file takes it.
            Err(_) => rustgis_data::io::delimited_text(name, bytes).map_err(err)?.0,
        };
        rustgis_data::io::csv_columns(&text, parse_delimiter(ps(p, "delimiter"))?, crs).map_err(err)?
    } else {
        return Err(format!("{name}: not delimited text (CSV, TSV, TXT, DAT, or gzip-compressed text) or a spreadsheet"));
    };
    Ok(json!({
        "columns": c.names,
        "x": c.x,
        "y": c.y,
        "wkt": c.wkt,
        "delimiter": c.delimiter.map(|d| char::from(d).to_string()),
    }))
}

/// Whether a file name is delimited text (CSV, TSV, TXT or DAT).
fn is_delimited_name(name: &str) -> bool {
    rustgis_data::io::is_delimited_name(name)
}

/// Reads delimited text with the options `p` gives, or `None` when it gives none (or the file
/// isn't delimited text).
fn read_delimited(
    p: &Value,
    name: &str,
    bytes: impl FnOnce() -> Result<Vec<u8>, String>,
) -> Result<Option<(rustgis_data::Dataset, rustgis_data::ReadReport)>, String> {
    if !is_delimited_name(name) {
        return Ok(None);
    }
    if csv_options(p)?.is_none() {
        return Ok(None);
    }
    read_delimited_bytes(p, name, &bytes()?).map(Some)
}

/// Adds file context to a reader error. A size refusal already names the file it could not read,
/// which may be a sidecar of the requested file.
fn whole_read_error(path: &str, error: rustgis_data::DataError) -> String {
    let message = error.to_string();
    if rustgis_data::io::file_size_error_args(&message).is_some() || message.starts_with(&format!("{path}:")) {
        return message;
    }
    // A reader that names the file by its name alone (the delimited and gzip readers) is named
    // once, by the path given.
    let file = Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path);
    match message.strip_prefix(file).filter(|rest| rest.starts_with(':')) {
        Some(rest) => format!("{path}{rest}"),
        None => format!("{path}: {message}"),
    }
}

/// Reads local/uploaded delimited bytes with the same options and CRS notices on every path.
pub(crate) fn read_delimited_bytes(
    p: &Value,
    name: &str,
    bytes: &[u8],
) -> Result<(rustgis_data::Dataset, rustgis_data::ReadReport), String> {
    let opts = csv_options(p)?.unwrap_or_default();
    let (ds, mut report) = rustgis_data::io::read_delimited_bytes(name, bytes, &opts).map_err(|e| whole_read_error(name, e))?;
    if let (Some(text), Some(crs)) = (ps(p, "crs"), opts.crs)
        && let Some(note) = rustgis_data::raster::wkt::typed_crs_note(text, crs)
    {
        report.messages.push(format!("crs: {note}"));
    }
    Ok((ds, report))
}

/// The last segment of `url`'s path, without its query, control characters, backslashes or colons:
/// the file name [`url_file_name`] starts from.
fn url_last_segment(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let path = path.split_once("://").map_or(path, |(_, rest)| rest.split_once('/').map_or("", |(_, p)| p));
    path.rsplit('/').find(|p| !p.is_empty()).unwrap_or("").chars().filter(|c| !c.is_control() && !matches!(c, '\\' | ':')).collect()
}

/// The longest file name [`url_file_name`] keeps.
const MAX_URL_FILE_NAME: usize = 120;

/// The notice to give when [`url_file_name`] cut `url`'s file name, which names the layer unless
/// `p` gives a `name` (#2639); `command` is the command's title. None when it wasn't cut, a name
/// is given, or the download (`bytes`) is a zip, whose layers are named by the files in it. A
/// `.shp` address's zip is the one made of its parts, named from the address, so it counts.
pub(crate) fn url_name_cut(command: &str, url: &str, p: &Value, bytes: &[u8]) -> Option<String> {
    let full = url_last_segment(url);
    let shp = Path::new(&full).extension().is_some_and(|e| e.eq_ignore_ascii_case("shp"));
    let n = full.chars().count();
    // A short name first, so the common case doesn't read the download again.
    if n <= MAX_URL_FILE_NAME || ps(p, "name").is_some_and(|n| !n.trim().is_empty()) || (is_zip(bytes) && !shp) {
        return None;
    }
    // Decided by the stem the layer is named from: a long extension that isn't one (`a.xxx…`)
    // is dropped, not cut.
    let kept = url_file_name(url, bytes).ok()?;
    let stem = |name: &str| Path::new(name).file_stem().and_then(|s| s.to_str()).map_or(0, |s| s.chars().count());
    // A CityJSON model's ".city" is added, not kept from the address.
    let kept_stem = Path::new(&kept).file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let kept_stem = kept_stem.strip_suffix(".city").filter(|_| !full.to_ascii_lowercase().contains(".city.")).unwrap_or(kept_stem);
    (kept_stem.chars().count() < stem(&full)).then(|| {
        format!(
            "{command}: the address's file name is {n} characters, so the file name the layer is named from is cut to {MAX_URL_FILE_NAME} (give a name to choose it)"
        )
    })
}

/// The file name for data downloaded from `url`: its last path segment when that has an extension
/// RustGIS reads, else `<segment or "data">.<ext>` with the extension its contents suggest
/// (an error when they suggest none). The name is cut to 120 characters, keeping an extension
/// shorter than 60; a `.shp` address whose download is a zip of its parts is named `<stem>.zip`.
/// JPEG 2000 named or recognised by its contents is refused where GDAL can't run (the browser).
pub fn url_file_name(url: &str, bytes: &[u8]) -> Result<String, String> {
    let full = url_last_segment(url);
    let raw_ext = Path::new(&full).extension().and_then(|e| e.to_str());
    let ext = raw_ext.map(str::to_ascii_lowercase).unwrap_or_default();
    // At most 120 characters, keeping an extension shorter than 60 (a long `.shp` name is still one).
    let last: String = match raw_ext {
        Some(e) if full.chars().count() > MAX_URL_FILE_NAME && e.chars().count() < 60 => {
            format!("{}.{e}", full.chars().take(MAX_URL_FILE_NAME - 1 - e.chars().count()).collect::<String>())
        }
        _ => full.chars().take(MAX_URL_FILE_NAME).collect(),
    };
    // JPEG 2000 (Sentinel-2's imagery) is read through GDAL, which the browser can't run.
    let jpeg2000_only_on_desktop = |name: String| {
        if cfg!(target_arch = "wasm32") { Err(super::raster::JPEG2000_WEB_LIMITATION.to_string()) } else { Ok(name) }
    };
    if rustgis_data::raster::is_jpeg2000_name(&last) {
        return jpeg2000_only_on_desktop(last);
    }
    // A `.shp` address comes with its files fetched beside it, zipped ([`download_data`]).
    if ext == "shp" && is_zip(bytes) {
        return Ok(format!("{}.zip", Path::new(&last).file_stem().and_then(|s| s.to_str()).unwrap_or("layer")));
    }
    let known = rustgis_data::io::READ_EXTENSIONS.contains(&ext.as_str())
        || rustgis_data::raster::is_raster_name(&last)
        || matches!(ext.as_str(), "tsv" | "mbtiles" | "pmtiles");
    if (known && ext != "json") || rustgis_data::cityjson::is_cityjson_name(&last) {
        return Ok(last);
    }
    let stem = Path::new(&last).file_stem().and_then(|s| s.to_str()).filter(|s| !s.is_empty()).unwrap_or("data");
    // A CityJSON model at an address with no other format in its name: JSON, but not GeoJSON.
    if std::str::from_utf8(bytes).is_ok_and(rustgis_data::cityjson::looks_like_cityjson) {
        return Ok(format!("{stem}.city.json"));
    }
    match rustgis_data::io::sniff_extension(bytes) {
        Some(ext) => {
            let name = format!("{stem}.{ext}");
            if rustgis_data::raster::is_jpeg2000_name(&name) { jpeg2000_only_on_desktop(name) } else { Ok(name) }
        }
        None => {
            Err("not a data format RustGIS reads (GeoJSON, CSV, GeoParquet, FlatGeobuf, GeoPackage, zipped Shapefiles, KML, GPX, GeoTIFF)"
                .into())
        }
    }
}

/// Identifies a bounded data download using the same filename and companion rules as its reader.
/// Discovery, content-sniffed imports and external drivers keep conservative account checks.
pub(crate) fn fixed_download_urls(p: &Value) -> Option<Vec<String>> {
    let url = p.get("url")?.as_str()?;
    if url.is_empty()
        || url.trim() != url
        || ["service", "coverage"].iter().any(|key| p.get(key).is_some_and(|v| !v.is_null() && v.as_bool() != Some(false)))
        || ["geolibreProject", "geolibreLayer", "geolibreZarr"].iter().any(|key| p.get(key).is_some())
        || rustgis_data::time::is_template(url)
    {
        return None;
    }
    let name = url_file_name(url, &[]).ok()?;
    if rustgis_data::raster::is_jpeg2000_name(&name) {
        return None;
    }
    let mut urls = vec![url.to_string()];
    if let Some(parts) = rustgis_data::io::shapefile_part_urls(url) {
        urls.extend(parts.into_iter().map(|(_, url)| url));
    }
    Some(urls)
}

/// Downloads data from a web address and adds it (the command line, MCP and the assistant; the
/// app downloads off the frame and adds the bytes). In the browser, where nothing may wait, the
/// request is kept for the app ([`Session::request_url`]), which downloads and adds it as its own
/// Home › Add › Add Data › From URL does, so a plugin, a script or the assistant can add data by address too.
/// Its errors quote addresses without their keys: the assistant's and MCP's tools return them as
/// they are.
pub(crate) fn add_url(s: &mut Session, p: &Value) -> CmdResult {
    let owner = s.continuation()?;
    let url = ps(p, "url").map(str::trim).unwrap_or("");
    let out = add_url_as_given(s, p).map_err(|e| crate::sources::message_with_address(url, &e))?;
    s.check_continuation(&owner)?;
    Ok(out)
}

/// Parses the optional document origin (`"from"`, `"fromDocument"`) on a URL-loading command,
/// returning `(origin_for_address_check, public_document_reach)`.
pub fn request_origin(p: &Value) -> Result<(Option<&str>, bool), String> {
    let from = match p.get("from") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            let s = s.trim();
            (!s.is_empty()).then_some(s)
        }
        Some(_) => return Err("\"from\" must be a string".into()),
    };
    let from_document = match p.get("fromDocument") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return Err("\"fromDocument\" must be true or false".into()),
    };
    let from_public = from.is_some_and(|u| {
        crate::render::rustgis_tiles::address::reach(crate::render::rustgis_tiles::logins::inner(u))
            == crate::render::rustgis_tiles::address::Reach::Public
    });
    let public = from_document.unwrap_or(false) || from_public;
    let origin = if public && from.is_none() { Some("https://document.invalid/") } else { from };
    Ok((origin, public))
}

/// Selects observed provenance registered by native UI code for this exact asset and document.
/// JSON alone cannot grant that provenance; unregistered requests keep the conservative parser.
pub fn request_origin_for<'a>(s: &Session, p: &'a Value) -> Result<(Option<&'a str>, bool), String> {
    let parsed = request_origin(p)?;
    if s.document_assets.0 != s.opened {
        return Ok(parsed);
    }
    let Some(from) = p.get("from").and_then(Value::as_str).map(str::trim) else { return Ok(parsed) };
    let Some(url) = p.get("url").and_then(Value::as_str).map(str::trim) else { return Ok(parsed) };
    let key = (crate::render::rustgis_tiles::logins::inner(url.trim()).to_string(), from.to_string());
    let Some(origin) = s.document_assets.1.get(&key) else { return Ok(parsed) };
    let plain = crate::render::rustgis_tiles::logins::inner(url);
    let selected = if s.policy_user() && plain != url {
        p.get("connection")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(|id| crate::service_logins::bind_logged_in(s, id, plain).map(|bound| bound == url))
            .transpose()?
            .unwrap_or(false)
    } else {
        false
    };
    // Only a native user's explicit, live session login can replace the document's binding.
    // Its plain destination still has to pass the document's observed network reach.
    origin.check(if selected { plain } else { url })?;
    Ok((
        Some(from),
        origin.reach() == crate::render::rustgis_tiles::address::Reach::Public
            || p.get("fromDocument").and_then(Value::as_bool) == Some(true),
    ))
}

/// Binds an asset request without letting normalization invent a document's login selection.
/// Registered plain assets stay anonymous until the native user names a connection; already
/// bound document assets retain their verified binding.
pub fn asset_request_params<'a>(s: &mut Session, p: &'a Value) -> Result<std::borrow::Cow<'a, Value>, String> {
    let url = p.get("url").and_then(Value::as_str).map(str::trim);
    let from = p.get("from").and_then(Value::as_str).map(str::trim);
    let selected = p.get("connection").and_then(Value::as_str).is_some_and(|id| !id.trim().is_empty());
    let registered_plain = url.zip(from).is_some_and(|(url, from)| {
        let plain = crate::render::rustgis_tiles::logins::inner(url);
        s.document_assets.0 == s.opened && plain == url && s.document_assets.1.contains_key(&(plain.to_string(), from.to_string()))
    });
    if registered_plain && !selected {
        let mut anonymous = p.clone();
        anonymous["anonymous"] = json!(true);
        return crate::service_logins::request_params(s, &anonymous).map(|p| std::borrow::Cow::Owned(p.into_owned()));
    }
    crate::service_logins::request_params(s, p)
}

impl Session {
    /// Records an exact asset's transport-observed provenance from trusted native UI dispatch.
    /// Non-user ingress cannot populate this runtime-only table, and a full table fails closed.
    pub fn remember_document_asset(&mut self, url: &str, origin: crate::render::rustgis_tiles::address::Origin) -> Result<(), String> {
        if !self.policy_user() {
            return Err("only native UI dispatch records document provenance".into());
        }
        origin.check(url)?;
        if self.document_assets.0 != self.opened {
            self.document_assets = (self.opened, Default::default());
        }
        let key = (crate::render::rustgis_tiles::logins::inner(url.trim()).to_string(), origin.url().to_string());
        if self.document_assets.1.len() >= 16_384 && !self.document_assets.1.contains_key(&key) {
            return Err("too many document assets are open; reopen the project to register more".into());
        }
        self.document_assets.1.insert(key, origin);
        Ok(())
    }
}

/// Network reach ceiling implied by `(origin, public)` from [`request_origin`].
pub fn origin_ceiling(origin: Option<&str>, public: bool) -> crate::render::rustgis_tiles::address::Reach {
    use crate::render::rustgis_tiles::{address, logins};
    if origin.is_some_and(|u| address::reach(logins::inner(u)) == address::Reach::Forbidden) {
        address::Reach::Forbidden
    } else if public {
        address::Reach::Public
    } else {
        address::Reach::Local
    }
}

/// Records the reach a URL (and its Shapefile parts) was requested with: a document's request
/// restricts it to public addresses; a user's own request without an origin clears that again.
pub fn mark_url_reach(url: &str, origin: Option<&str>, public: bool) {
    use crate::render::rustgis_tiles::cache;
    if public {
        cache::restrict_to_public(url);
        if let Some(parts) = rustgis_data::io::shapefile_part_urls(url) {
            for (_, part) in parts {
                cache::restrict_to_public(&part);
            }
        }
    } else if origin.is_none() {
        cache::allow_named(url);
        if let Some(parts) = rustgis_data::io::shapefile_part_urls(url) {
            for (_, part) in parts {
                cache::allow_named(&part);
            }
        }
    }
}

fn add_url_as_given(s: &mut Session, p: &Value) -> CmdResult {
    let (origin, public) = request_origin_for(s, p)?;
    if (origin.is_some() || public)
        && let Some(raw) = ps(p, "url").map(str::trim).filter(|u| !u.is_empty())
    {
        let _ = web_address(raw)?;
        crate::render::rustgis_tiles::address::check_reach(raw, origin_ceiling(origin, public))?;
    }
    let bound = asset_request_params(s, p)?;
    let p = bound.as_ref();
    let (origin, public) = request_origin_for(s, p).map_err(|e| e.to_string()).map(|(o, pub_)| (o.or(origin), pub_ || public))?;
    if !s.import_completion(p) {
        no_geolibre_state(p)?;
    }
    // Marks a service's document, read and kept for `service.add`: as a download, no layer would come.
    if super::plugins::fetch::is_document(p) {
        return Err("\"document\" is internal to service.add: leave it out".into());
    }
    let url = ps(p, "url").map(str::trim).filter(|u| !u.is_empty()).ok_or("url is required")?;
    // A plugin command run again once the download it left to the app was added (#1202): what
    // adding it answered, as often as the run asks for it (the app downloaded and added it once).
    // Answering it changes nothing (the layer is its own undo step).
    let wanted = s.asked_for(p);
    // A window of the view is answered by the one asked in the view the command sees now.
    #[cfg(not(target_arch = "wasm32"))]
    let wanted = match p.get("bbox") {
        Some(b) if s.leave_downloads && is_view(Some(b)) => {
            let at = ps(p, "url").map(str::trim).unwrap_or_default();
            Session::with_view(&wanted, Some(window_settled(s, p, at, None)?.bbox))
        }
        _ => wanted,
    };
    if s.leave_downloads
        && let Some((answer, added)) = s.rerun_answer(&wanted)
    {
        s.request_only = true;
        // Added to another map than the one shown now: the command's next steps would act on this
        // one, where the layer's id isn't it. (A failed download added nothing: its error stands.)
        if added.added_any() && added.map != s.map().instance_id() {
            return Err(match s.project.maps.iter().find(|m| m.instance_id() == added.map) {
                Some(m) => format!(
                    "{url}: its download was added to map '{}', not the map shown now: show that map and run the command again",
                    m.name
                ),
                None => format!("{url}: its download was added to a map since taken away, not the map shown now: run the command again"),
            });
        }
        // What it added (a layer, a tile layer) may have been undone while the command waited,
        // and a layer added since may have its id: said, not named.
        let missing = answer
            .as_ref()
            .ok()
            .and_then(|v| v.get("layer"))
            .and_then(Value::as_u64)
            .is_some_and(|id| u32::try_from(id).ok().and_then(|id| s.map().layer(id)).is_none());
        if missing || added.gone(s.map()) {
            return Err(crate::sources::message_with_address(
                url,
                "the layer its download added was removed or undone before the command ran again",
            ));
        }
        if super::plugins::fetch::table_gone(&answer, &s.project) {
            return Err(crate::sources::message_with_address(
                url,
                "the table its download added was removed or undone before the command ran again",
            ));
        }
        return added.answer_now(s.map(), &answer);
    }
    // An address read through a saved S3 connection is judged by the address it wraps; the whole
    // address stays what is fetched and kept.
    let lower = web_address(url)?;
    if origin.is_some() || public {
        crate::render::rustgis_tiles::address::check_reach(url, origin_ceiling(origin, public))?;
    }
    if public || origin.is_some() || s.policy_user() {
        mark_url_reach(url, origin, public);
    }
    // A scan (`"as"`) keeps the pixels and none of the coordinates: the file is downloaded whole and
    // read as one (`add_named_bytes` checks it is an image), never read in place.
    let scan = super::raster::as_scan(p)?;
    // A byte range of a GRIB file (`"bytes"`, #2428): only that range is downloaded, and read as
    // the file. The browser refuses it ([`byte_range`]).
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(range) = byte_range(p, url)? {
        return add_url_range(s, p, url, range, public);
    }
    #[cfg(target_arch = "wasm32")]
    byte_range(p, url)?;
    // A window of a cloud-optimised GeoTIFF, read as a raster (opengeos/RustGIS#1204).
    let window = p.get("bbox").is_some_and(|v| !v.is_null());
    if !window && p.get("cellSize").is_some_and(|v| !v.is_null()) {
        return Err(format!("{url}: \"cellSize\" goes with \"bbox\": give a box, or leave it out"));
    }
    if window {
        window_checks(p, url, scan)?;
        // A plugin's command run by the app leaves what would wait to the app, off the frame
        // ([`read_cog_window`]); a box that can't be read is refused now, while the command runs.
        #[cfg(not(target_arch = "wasm32"))]
        if s.leave_downloads {
            let settled = window_settled(s, p, url, None)?;
            let mut asked = p.clone();
            asked["url"] = json!(url);
            // `"view"` is the view now: the app reads the window on a later frame, and a command
            // run again after a download sees the view it first ran with only while it runs. The
            // request keeps the box beside what was asked, by which a run again is answered.
            s.window_view = is_view(p.get("bbox")).then_some(settled.bbox);
            let queued = s.request_url(&asked);
            s.window_view = None;
            return queued;
        }
        return add_cog_window(s, p, url);
    }
    // A PMTiles archive is read a tile at a time, never downloaded whole.
    if lower.split(['?', '#']).next().is_some_and(|p| p.ends_with(".pmtiles")) {
        if scan {
            return Err(format!("{url}: a PMTiles archive is tiles, not a picture: it can't be opened as a scan"));
        }
        let mut q = json!({"url": url, "kind": "pmtiles", "top": true});
        if let Some(n) = ps(p, "name") {
            q["name"] = json!(n);
        }
        if let Some(connection) = p.get("connection") {
            q["connection"] = connection.clone();
        }
        return super::tiles::add(s, &q);
    }
    // A dated address (one file per date, `{date:YYYY}`) is read in place, following the time
    // slider: a GeoTIFF on the web, before anything fetches the address as it is.
    if rustgis_data::time::is_template(url) {
        if scan {
            return Err(format!("{url}: a dated address is read in place with its coordinates, so it can't be opened as a scan"));
        }
        let web = lower.starts_with("https://") || lower.starts_with("http://");
        return if is_tiff_url(url) && web {
            add_cog(s, p, url, false)
        } else {
            Err(format!(
                "{url}: dated addresses work for cloud-optimised GeoTIFFs on the web (http, https) here, and for tile layers (map.addTiles)"
            ))
        };
    }
    // A FlatGeobuf or GeoParquet: read in place by range when it has a spatial index (or a bbox
    // covering), otherwise downloaded whole.
    if rustgis_data::source::is_remote_name(url) && scan {
        return Err(format!("{url}: \"as\": \"scan\" opens a TIFF, PNG or JPEG"));
    }
    // A plugin's command run by the app (the desktop): what would wait (reading a header, a
    // probe, a download) is left to the app, which does it off the frame, as in the browser.
    #[cfg(not(target_arch = "wasm32"))]
    if s.leave_downloads {
        // A data cube's GeoTIFF (`"cube"`, as `stac.addCube` adds them) is a tile layer whose
        // tiles are read in place on later frames: nothing waits, so it is added now, its header
        // unread, as in the browser (#2418). Left to the app, no tile layer would come. One that
        // isn't cloud-optimised says so as the map draws it (its header's error, as any tile
        // layer's). The address is a web one (`web_address` above).
        let tiff = is_tiff_url(url) || b(p, "cog").unwrap_or(false);
        if b(p, "cube") == Some(true) && tiff && !scan && !b(p, "whole").unwrap_or(false) {
            return add_cog(s, p, url, false);
        }
        // The address as the app downloads it (trimmed): the request, its download and the
        // command run again all name it the same.
        let mut asked = p.clone();
        asked["url"] = json!(url);
        return s.request_url(&asked);
    }
    // An address the app found to be a GeoTIFF (`"cog"`) is one whatever its name says.
    if rustgis_data::source::is_remote_name(url) && !super::b(p, "whole").unwrap_or(false) && !super::b(p, "cog").unwrap_or(false) {
        // In the browser nothing may wait: the app has fetched what opening needs already.
        #[cfg(target_arch = "wasm32")]
        let opened = rustgis_data::source::open_url_now(url);
        #[cfg(not(target_arch = "wasm32"))]
        let opened = rustgis_data::source::open_url_blocking(url);
        match opened {
            Ok(src) => return add_remote(s, p, url, src),
            Err(e) if e.to_string().contains(rustgis_data::source::remote::READ_WHOLE) => {}
            Err(e) => {
                let e = e.to_string();
                return Err(if e.starts_with(url) { e } else { format!("{url}: {e}") });
            }
        }
    }
    // A GeoTIFF: read in place when it is cloud-optimised (tiled), otherwise downloaded whole. One
    // the app has probed already says so (`"cog"`).
    let tiff = is_tiff_url(url) || super::b(p, "cog").unwrap_or(false);
    #[cfg(target_arch = "wasm32")]
    {
        // Nothing may wait here: a GeoTIFF is taken to be cloud-optimised (the layer says if not).
        if tiff && !scan && !super::b(p, "whole").unwrap_or(false) {
            return add_cog(s, p, url, false);
        }
        // Downloaded by the app off the frame and added when it lands.
        s.request_url(p)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Redact the known address before composing any reason: a query value can contain
        // unescaped whitespace, so splitting an arbitrary error into URL words is not safe.
        let named = |e: String| crate::sources::message_with_address(url, &e);
        // A URL a web document restricted to public addresses stays so: a request from a local
        // origin (or none) doesn't lift the restriction its earlier request set.
        let public =
            public || crate::render::rustgis_tiles::cache::ceiling_for(url) == crate::render::rustgis_tiles::address::Reach::Public;
        // A service's features with no app to leave them to (the CLI, a script, a task): read in
        // pages as the app reads them, not as one answer cut at the service's record limit (#1144).
        // A GeoTIFF keeps its own path below, read in place when it can be.
        if super::b(p, "service").unwrap_or(false) && !tiff {
            let label = ps(p, "name").map(str::to_string).unwrap_or_else(|| crate::sources::address_in_message(url));
            let bounds = super::service::bounds_of(p.get("bounds"))?;
            // The cut judged from the bytes the layer is named from, as they are named.
            let mut cut = None;
            let file = |bytes: &[u8]| {
                cut = url_name_cut("Add Data From URL", url, p, bytes);
                url_file_name(url, bytes)
            };
            let r = super::service::add_service_layer(s, p, &label, url, bounds, public, b(p, "zoom"), file).map_err(named)?;
            if let Some(cut) = cut {
                s.notify(cut, false);
            }
            return Ok(r);
        }
        // An address without a file name may be a GeoTIFF too: its first bytes are read to see,
        // and anything that doesn't answer as one is downloaded whole, as before.
        let unnamed = !tiff && is_unnamed_url(url);
        if (tiff || unnamed) && !scan && !super::b(p, "whole").unwrap_or(false) {
            let cog = match crate::render::rustgis_tiles::cog::probe_blocking(url) {
                Ok(cog) => cog,
                Err(_) if unnamed => false,
                Err(e) => return Err(named(e)),
            };
            if cog {
                return add_cog(s, p, url, true);
            }
        }
        let bytes = if rustgis_data::io::shapefile_part_urls(url).is_some() {
            download_shapefile_blocking_with(url, public)?
        } else if public {
            crate::render::rustgis_tiles::cache::download_public_within(
                url,
                crate::render::rustgis_tiles::cache::MAX_DOWNLOAD,
                crate::render::rustgis_tiles::cache::BLOCKING_TIMEOUT,
            )
            .map_err(crate::render::rustgis_tiles::cache::large_file_hint)
            .map_err(named)?
        } else {
            crate::render::rustgis_tiles::cache::download_blocking(url).map_err(named)?
        };
        let name = url_file_name(url, &bytes).map_err(named)?;
        let left_out = parts_left_out(url, &bytes, p);
        let cut = url_name_cut("Add Data From URL", url, p, &bytes);
        // A WCS coverage's own system it was asked for in, for an answer that doesn't say it.
        let asked = if b(p, "coverage") == Some(true) { super::raster::coverage_system(p, url) } else { None };
        let r = super::raster::add_named_bytes_read(s, p, name, bytes, b(p, "zoom"), None, asked).map_err(named)?;
        if let Some(n) = left_out {
            s.notify(n, false);
        }
        if let Some(cut) = cut {
            s.notify(cut, false);
        }
        Ok(r)
    }
}

/// The most octets a `"bytes"` range may ask for (64 MB): a record of a model's file, not the file.
pub const MAX_RANGE: u64 = 64 << 20;

/// The range `layer.addUrl` is asked for (`"bytes": [first, last]`, octets counted from 0, both
/// included, as HTTP's `Range` writes them), checked: of a GRIB file only (by its address's name,
/// or `"format": "grib"`), at most [`MAX_RANGE`], and with nothing that reads an address another
/// way. `None` without `"bytes"`. The browser refuses it: a range from another site needs that
/// site's leave (CORS), which comes in a later part.
pub fn byte_range(p: &Value, url: &str) -> Result<Option<(u64, u64)>, String> {
    let grib = match p.get("format") {
        None | Some(Value::Null) => false,
        Some(Value::String(f)) if f.trim().eq_ignore_ascii_case("grib") => true,
        // Without a range, another format is left as before ranges (an unknown key, ignored).
        Some(_) if p.get("bytes").is_none_or(Value::is_null) => false,
        Some(_) => return Err("\"format\" is \"grib\", the one format a byte range is read as".into()),
    };
    let asked = match p.get("bytes") {
        None | Some(Value::Null) if grib => return Err("\"format\" goes with \"bytes\": give a range, or leave it out".into()),
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Array(v)) if v.len() == 2 => v[0].as_u64().zip(v[1].as_u64()),
        Some(_) => None,
    };
    let Some((first, last)) = asked else {
        return Err("\"bytes\" is [first, last]: two octet offsets counted from 0, both included".into());
    };
    if first > last {
        return Err(format!("\"bytes\": the first octet ({first}) comes after the last ({last})"));
    }
    if last - first >= MAX_RANGE {
        return Err(format!("\"bytes\": {} octets asked for, more than a range may have (64 MB)", (last - first).saturating_add(1)));
    }
    let path = url.split(['?', '#']).next().unwrap_or(url);
    if !rustgis_data::raster::grib::is_grib_name(path) {
        // An address naming another format is that format, whatever "format" says.
        if let Some(name) = url_file_name(url, &[]).ok().filter(|_| grib) {
            return Err(format!("\"bytes\": the address names {name}, not a GRIB file"));
        }
        if !grib {
            return Err("\"bytes\" reads a range of a GRIB file (an address ending in .grb, .grib, .grb2 or .grib2, or \"format\": \"grib\"): a range of another format isn't a file".into());
        }
    }
    let other = ["bbox", "cellSize", "as", "service", "coverage", "cube", "cog", "whole", "dates"];
    if let Some(k) = other.iter().find(|k| p.get(**k).is_some_and(|v| !v.is_null() && v.as_bool() != Some(false))) {
        return Err(format!("\"bytes\" doesn't go with \"{k}\": a range is downloaded and read as a GRIB file"));
    }
    if rustgis_data::time::is_template(url) {
        return Err("\"bytes\" doesn't go with a dated address: give the file's own address".into());
    }
    if cfg!(target_arch = "wasm32") {
        return Err("\"bytes\" works on the desktop and the command line for now: in the browser, a range from another site needs that site's leave (CORS)".into());
    }
    Ok(Some((first, last)))
}

/// The address a range is downloaded from: `url` with the range the fetchers send as `Range`
/// ([`cache::RANGE`]); its reach is the address's own.
///
/// [`cache::RANGE`]: crate::render::rustgis_tiles::cache::RANGE
pub fn ranged_address(url: &str, (first, last): (u64, u64)) -> String {
    format!("{url}{}{first}-{last}", crate::render::rustgis_tiles::cache::RANGE)
}

/// A range's answer is as long as the range and starts a GRIB message: a server answering fewer
/// octets (a range past the file's end), or a range that isn't a record (whatever its octets read
/// as, said), is refused, never read as the record. Other addresses pass.
pub fn check_range_answer(url: &str, bytes: &[u8]) -> Result<(), String> {
    match crate::render::rustgis_tiles::cache::split_range(url) {
        (_, Some((first, last))) if bytes.len() as u64 != (last - first).saturating_add(1) => Err(format!(
            "the server answered {} octets of the {} asked for (\"bytes\": [{first}, {last}]): check the range against the file's inventory",
            bytes.len(),
            (last - first).saturating_add(1)
        )),
        (_, Some(_)) if !bytes.starts_with(b"GRIB") => Err(match rustgis_data::io::sniff_extension(bytes) {
            Some(ext) => format!(
                "the range doesn't start a GRIB message: its octets read as a .{ext} file (check the range against the file's inventory)"
            ),
            None => "the range doesn't start a GRIB message (check the range against the file's inventory)".to_string(),
        }),
        _ => Ok(()),
    }
}

/// A range's download failed: one whose answer was longer than the range (a server answering the
/// whole file from octet 0) says why.
pub fn range_failed(url: &str, e: String) -> String {
    let (_, range) = crate::render::rustgis_tiles::cache::split_range(url);
    if range.is_some() && e.contains("larger than") {
        format!("{e}, more than the range asked for: {}", crate::render::rustgis_tiles::cache::NO_RANGES)
    } else {
        e
    }
}

/// Downloads a GRIB file's range ([`byte_range`]) and adds it (the command line); a plugin's
/// command on the desktop leaves it to the app, which downloads it off the frame. A server that
/// ignores the range is refused before the file comes ([`cache::NO_RANGES`]), or once its answer
/// is longer than the range.
///
/// [`cache::NO_RANGES`]: crate::render::rustgis_tiles::cache::NO_RANGES
#[cfg(not(target_arch = "wasm32"))]
fn add_url_range(s: &mut Session, p: &Value, url: &str, range: (u64, u64), public: bool) -> CmdResult {
    use crate::render::rustgis_tiles::cache;
    if s.leave_downloads {
        let mut asked = p.clone();
        asked["url"] = json!(url);
        return s.request_url(&asked);
    }
    let named = |e: String| crate::sources::message_with_address(url, &e);
    let ranged = ranged_address(url, range);
    let len = usize::try_from(range.1 - range.0 + 1).unwrap_or(usize::MAX);
    let public = public || cache::ceiling_for(url) == crate::render::rustgis_tiles::address::Reach::Public;
    let bytes = if public {
        cache::download_public_within(&ranged, len, cache::BLOCKING_TIMEOUT)
    } else {
        cache::download_blocking_within(&ranged, len, cache::BLOCKING_TIMEOUT)
    };
    // Without `large_file_hint`: a range timing out isn't helped by downloading the whole file.
    let bytes = bytes.map_err(|e| range_failed(&ranged, e)).map_err(named)?;
    check_range_answer(&ranged, &bytes).map_err(named)?;
    let name = url_file_name(&ranged, &bytes).map_err(named)?;
    super::raster::add_named_bytes(s, p, name, bytes, b(p, "zoom")).map_err(named)
}

/// Whether a Shapefile's part other than its `.dbf` can be done without: it isn't there (404 or
/// 410, or 403, which S3 answers for a missing object to those who can't list the bucket). A 403
/// to an address read with keys ([`keyed`]) fails instead, as they may not cover the part.
fn part_not_there(ext: &str, url: &str, e: &str) -> bool {
    ext != "dbf"
        && match crate::render::rustgis_tiles::cache::http_status(e) {
            Some(404 | 410) => true,
            Some(403) => !keyed(url),
            _ => false,
        }
}

/// The statuses a Shapefile's part answers when it isn't there ([`part_not_there`]): expected, so
/// they are noted once per server rather than recorded as failed requests.
fn part_absences(ext: &str, url: &str) -> &'static [u16] {
    match ext {
        "dbf" => &[],
        _ if keyed(url) => &[404, 410],
        _ => &[404, 410, 403],
    }
}

/// Whether an address is read with keys: one with a user and password, or whose query holds a key,
/// token or signature (what `sources::redact` takes out), or a saved S3 connection.
fn keyed(url: &str) -> bool {
    // A user and password, or a key, token or signature in the query, is what redaction takes out;
    // a query like `?v=2` carries none.
    // (Redaction drops a fragment too, which carries no key.)
    // (A trailing `?` or `&`, which redaction drops too, carries none either.)
    let bare = url.split('#').next().unwrap_or(url).trim_end_matches(['?', '&']);
    crate::sources::redact(bare) != bare || rustgis_data::s3_address::is_wrapper(url)
}

/// A part that arrived: itself, or `None` for an optional one that a web page answered for (an
/// error for the `.dbf`, which can't be done without).
fn page_part(ext: &str, b: Vec<u8>) -> Result<Option<Vec<u8>>, String> {
    match (is_web_page(&b), ext) {
        (false, _) => Ok(Some(b)),
        (true, "dbf") => Err("a web page answered, not a .dbf".into()),
        (true, _) => Ok(None),
    }
}

/// Whether downloaded bytes are a web page, not a Shapefile's part: a site that serves its page for
/// any address it doesn't have. A required part (`.shp`, `.dbf`) answered so fails; an optional
/// one is done without, as one that isn't there.
fn is_web_page(b: &[u8]) -> bool {
    b.iter().find(|c| !c.is_ascii_whitespace()) == Some(&b'<')
}

/// Whether downloaded bytes are a zip archive (its first local file header).
fn is_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

/// A Shapefile's `.shp` or part that failed, named by its address without its keys or signature
/// (the `.shp`'s query, which the parts carry too).
fn part_failed(url: &str, e: String) -> String {
    let why = status_hint(url, &e);
    format!("{}{why}", crate::sources::message_with_address(url, &e))
}

/// What a Shapefile's 403 may mean: S3 answers a signed request for a file that isn't there as it
/// answers a signature that doesn't cover it, and an unsigned one for a missing file so too.
fn status_hint(url: &str, e: &str) -> &'static str {
    match crate::render::rustgis_tiles::cache::http_status(e) {
        Some(403) if keyed(url) => {
            " (to an address read with keys, this may also mean the file isn't there, or that the keys or signature don't cover it: a presigned address signs one file, so a Shapefile's parts need addresses of their own; zip them and add the zip)"
        }
        Some(403) => " (S3 also answers this for a file that isn't there, to those who can't list its bucket)",
        _ => "",
    }
}

/// A part's download error, saying that a size limit is what was left of the one the Shapefile's
/// files share (a part is fetched within what the `.shp` and the parts before it left).
fn over_shared_limit(e: String) -> String {
    if e.starts_with("larger than ") { format!("{e}, what the .shp and the parts before it left of the download limit") } else { e }
}

/// What a Shapefile added by its `.shp` address (#1238) was read without, said: its `.shx`, `.prj`
/// or `.cpg` when they weren't found beside it. The address is said without its keys or signature;
/// a `crs` given (`p`) stands for the `.prj`.
fn parts_left_out(url: &str, bytes: &[u8], p: &Value) -> Option<String> {
    rustgis_data::io::shapefile_part_urls(url)?;
    let given_crs = ps(p, "crs").is_some_and(|c| !c.trim().is_empty());
    // Each Shapefile by its name: a zip the address answered with may hold several (the first
    // few named, the rest counted).
    const NAMED: usize = 5;
    // With `layer`, only the Shapefile added is said, picked as the read layers are (by its name
    // or its last word).
    let want = ps(p, "layer").map(str::trim).filter(|w| !w.is_empty());
    let picked = |shp: &str| {
        let stem = Path::new(shp).file_stem().and_then(|s| s.to_str()).unwrap_or(shp);
        want.is_none_or(|w| stem.eq_ignore_ascii_case(w) || stem.rsplit(' ').next().is_some_and(|l| l.eq_ignore_ascii_case(w)))
    };
    let lacking: Vec<_> = rustgis_data::io::shapefile_zip_lacks(bytes).into_iter().filter(|(shp, _)| picked(shp)).collect();
    let more = lacking.len().saturating_sub(NAMED);
    let mut said: Vec<String> = lacking
        .into_iter()
        .take(NAMED)
        // A name from the archive, without control characters and at most 80 characters.
        .map(|(shp, lacks)| {
            let shp: String = shp.chars().filter(|c| !c.is_control()).collect();
            (if shp.chars().count() > 80 { format!("{}…", shp.chars().take(79).collect::<String>()) } else { shp }, lacks)
        })
        .map(|(shp, lacks)| {
            let names = lacks.iter().map(|e| format!(".{e}")).collect::<Vec<_>>().join(", ");
            let crs = if lacks.contains(&"prj") && !given_crs { "; its coordinates are read as longitude/latitude" } else { "" };
            let them = if lacks.len() == 1 { "it" } else { "them" };
            format!(
                "{shp}: no {names} found or readable beside it ({} of 3 optional files missing), so it was read without {them}{crs}",
                lacks.len()
            )
        })
        .collect();
    if more > 0 {
        said.push(format!("and {more} more Shapefiles without some of theirs"));
    }
    (!said.is_empty()).then(|| format!("{}: {}", crate::sources::address_in_message(url), said.join("; ")))
}

/// A Shapefile by its `.shp` address, waited for (#1238): the `.shp`, then its `.dbf`, `.shx`,
/// `.prj` and `.cpg` beside it (each but the `.dbf` only when it is there), zipped, within one
/// download's budget together.
#[cfg(not(target_arch = "wasm32"))]
fn download_shapefile_blocking_with(url: &str, public: bool) -> Result<Vec<u8>, String> {
    use crate::render::rustgis_tiles::cache;
    let parts = rustgis_data::io::shapefile_part_urls(url).unwrap_or_default();
    if public {
        for (_, part) in &parts {
            crate::render::rustgis_tiles::address::check(part, Some(url)).map_err(|e| part_failed(part, e))?;
        }
    }
    // One wait for all the parts together, as for one download.
    let until = std::time::Instant::now() + cache::BLOCKING_TIMEOUT;
    // Past it, a part isn't asked for at all (a zero timeout may mean none), and a timeout says
    // where a large file goes instead.
    let within = |u: &str, max: usize, omit: &'static [u16]| match until.saturating_duration_since(std::time::Instant::now()) {
        w if w.is_zero() => Err(cache::large_file_hint("timed out".into())),
        w if public => cache::download_public_omitting_within(u, max, w, omit).map_err(cache::large_file_hint),
        w => cache::download_blocking_omitting_within(u, max, w, omit).map_err(cache::large_file_hint),
    };
    let shp = within(url, cache::MAX_DOWNLOAD, &[]).map_err(|e| part_failed(url, e))?;
    // Already zipped (a server answering the address with its parts): read as it is.
    if is_zip(&shp) {
        return Ok(shp);
    }
    if is_web_page(&shp) {
        return Err(part_failed(url, "a web page answered, not a .shp".into()));
    }
    let name = url_file_name(url, &shp).map_err(|e| part_failed(url, e))?;
    let mut left = cache::MAX_DOWNLOAD.saturating_sub(shp.len());
    let mut got = Vec::new();
    for (ext, u) in parts {
        match within(&u, left, part_absences(ext, &u)).and_then(|b| page_part(ext, b)) {
            Ok(None) => {}
            Ok(Some(b)) => {
                left = left.saturating_sub(b.len());
                got.push((ext, b));
            }
            Err(e) if part_not_there(ext, &u, &e) => {}
            Err(e) => return Err(part_failed(&u, over_shared_limit(e))),
        }
    }
    cache::check_still(&cache::current_still())?;
    zipped_within(&name, shp, got, left).map_err(|e| part_failed(url, e))
}

/// Downloads a file the user named by its address off the frame, as `cache::download_limited`
/// does, for the app to add when it lands: a Shapefile's `.shp` address brings its `.dbf`, `.shx`,
/// `.prj` and `.cpg` too, one after another, and `done` gets them zipped (#1238), within `max`
/// bytes together.
pub fn download_data(url: &str, max: usize, done: crate::render::rustgis_tiles::cache::Done) {
    download_data_from(url, false, max, done);
}

/// Like [`download_data`], restricting network connections to public internet addresses when `public`
/// is `true` (an address named by a web document rather than directly by the user).
pub fn download_data_from(url: &str, public: bool, max: usize, done: crate::render::rustgis_tiles::cache::Done) {
    use crate::render::rustgis_tiles::cache;
    let Some(parts) = rustgis_data::io::shapefile_part_urls(url) else {
        return if public { cache::download_public_limited(url, max, done) } else { cache::download_limited(url, max, done) };
    };
    if public {
        for (_, part) in &parts {
            if let Err(e) = crate::render::rustgis_tiles::address::check(part, Some(url)) {
                return done(Err(part_failed(part, e)));
            }
        }
    }
    let shp_url = url.to_string();
    let on_shp: cache::Done = Box::new(move |r| match r.and_then(|shp| url_file_name(&shp_url, &shp).map(|name| (name, shp))) {
        // Already zipped: as it is.
        Ok((_, shp)) if is_zip(&shp) => done(Ok(shp)),
        Ok((_, shp)) if is_web_page(&shp) => done(Err("a web page answered, not a .shp".into())),
        Ok((name, shp)) => {
            let left = max.saturating_sub(shp.len());
            download_parts(name, shp, parts.into_iter(), Vec::new(), left, public, done);
        }
        // Name the complete known address before the app or session redacts this again.
        Err(e) => {
            let why = status_hint(&shp_url, &e);
            done(Err(format!("{}{why}", crate::sources::message_with_address(&shp_url, &e))))
        }
    });
    if public {
        cache::download_public_limited(url, max, on_shp);
    } else {
        cache::download_limited(url, max, on_shp);
    }
}

/// A Shapefile's parts zipped, within the budget they were downloaded in (`left` being what is
/// still free): the archive's own records count too. It runs where the last part lands: on the
/// download thread natively, on the page's event loop in the web app (as the web app's loose
/// Shapefiles picked or dropped are zipped, #426).
fn zipped_within(name: &str, shp: Vec<u8>, got: Vec<(&'static str, Vec<u8>)>, left: usize) -> Result<Vec<u8>, String> {
    let max = got.iter().fold(shp.len().saturating_add(left), |n, (_, b)| n.saturating_add(b.len()));
    let zip = rustgis_data::io::zip_shapefile_parts(name, shp, got)?;
    if zip.len() > max {
        return Err(format!("larger than {max} bytes, zipped"));
    }
    Ok(zip)
}

/// The next of a Shapefile's parts for [`download_data`], then the zip once they are all in: the
/// steps of `download_shapefile_blocking_with` as callbacks, sharing its rules (`part_not_there`,
/// `part_failed`, `zipped_within`), so a change to one goes in both.
fn download_parts(
    name: String,
    shp: Vec<u8>,
    mut parts: std::vec::IntoIter<(&'static str, String)>,
    mut got: Vec<(&'static str, Vec<u8>)>,
    left: usize,
    public: bool,
    done: crate::render::rustgis_tiles::cache::Done,
) {
    let Some((ext, url)) = parts.next() else {
        done(zipped_within(&name, shp, got, left));
        return;
    };
    let next = url.clone();
    let omit = part_absences(ext, &url);
    let step: crate::render::rustgis_tiles::cache::Done = Box::new(move |r| match r.and_then(|b| page_part(ext, b)) {
        Ok(None) => download_parts(name, shp, parts, got, left, public, done),
        Ok(Some(b)) => {
            let left = left.saturating_sub(b.len());
            got.push((ext, b));
            download_parts(name, shp, parts, got, left, public, done);
        }
        Err(e) if part_not_there(ext, &url, &e) => download_parts(name, shp, parts, got, left, public, done),
        Err(e) => done(Err(part_failed(&url, over_shared_limit(e)))),
    });
    if public {
        crate::render::rustgis_tiles::cache::download_public_limited_omitting(&next, left, omit, step);
    } else {
        crate::render::rustgis_tiles::cache::download_limited_omitting(&next, left, omit, step);
    }
}

/// Adds a FlatGeobuf or GeoParquet by its address as a layer read in place: a view's features at
/// a time, by range. The project keeps the address.
fn add_remote(s: &mut Session, p: &Value, url: &str, src: Box<dyn rustgis_data::source::FeatureSource>) -> CmdResult {
    let info = src.info().clone();
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| info.name.clone());
    let empty_before = super::map_is_empty(s);
    let layer = rustgis_map::source::SourceLayer::from_path(url.to_string(), None, src);
    let unique = unique_name(s, &name);
    for n in &info.notes {
        s.say(n.clone().prefixed(&unique));
    }
    let id = s.map_mut().add_streamed(unique.clone(), layer);
    if let Some(l) = s.map_mut().layer_mut(id) {
        l.dataset.zm_dropped = info.zm_dropped;
    }
    s.active_layer = Some(id);
    let count = info.count.map(|c| c.to_string()).unwrap_or_else(|| "many".into());
    s.notify(format!("{unique}: {count} features, read in place by range from its address, a view at a time"), false);
    // Framed when it is the map's first layer, unless told to keep the view (a catalog's data for
    // the view, such as Overture's).
    if b(p, "zoom").unwrap_or(empty_before) && !info.extent.is_empty() {
        s.zoom_to(&info.extent);
    }
    Ok(json!({"layer": id, "layers": [id], "features": info.count, "streamed": true, "inPlace": true}))
}

/// The name of the layer the map's terrain takes its heights from, when it has one.
fn terrain_dem(s: &Session) -> Option<String> {
    match &s.map().view.terrain {
        Some(rustgis_map::view::Terrain { source: rustgis_map::view::TerrainSource::Layer(n), .. }) => Some(n.clone()),
        _ => None,
    }
}

/// Points the map's terrain at the layer called `name`.
fn set_terrain_dem(s: &mut Session, name: String) {
    if let Some(rustgis_map::view::Terrain { source: rustgis_map::view::TerrainSource::Layer(n), .. }) = &mut s.map_mut().view.terrain {
        *n = name;
    }
}

/// How many layers are called `name` (as layers are looked up, ignoring case).
fn named_layers(s: &Session, name: &str) -> usize {
    s.map().layers.iter().filter(|l| l.name.eq_ignore_ascii_case(name)).count()
}

/// Whether a web address names a GeoTIFF (by its path's extension).
pub fn is_tiff_url(url: &str) -> bool {
    let url = rustgis_data::s3_address::inner(crate::render::rustgis_tiles::logins::inner(url));
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    path.ends_with(".tif") || path.ends_with(".tiff")
}

/// Whether a web address's last path segment has no extension (an API answering with a file,
/// such as Vantor's `…/geotiff?…`): its first bytes tell whether it is a GeoTIFF to read in place.
pub fn is_unnamed_url(url: &str) -> bool {
    let url = rustgis_data::s3_address::inner(crate::render::rustgis_tiles::logins::inner(url));
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let Some((_, rest)) = path.split_once("://") else { return false };
    let Some((_, path)) = rest.split_once('/') else { return false };
    path.rsplit('/').find(|p| !p.is_empty()).is_some_and(|last| !last.contains('.'))
}

/// Adds a cloud-optimised GeoTIFF by its address as a layer read in place (its tiles by range,
/// as the view needs them), framed when its header is in and it is the first content on the map
/// (`"zoom"` says otherwise: `true` frames it, `false` keeps the view). `probed`: its header was
/// just read (natively), so it is waited for if something emptied the tile cache since (a login
/// ended): the layer would go unframed. One not read yet isn't waited for (a project opening its
/// files).
pub(crate) fn add_cog(s: &mut Session, p: &Value, url: &str, probed: bool) -> CmdResult {
    add_cog_with(s, p, url, true, probed)
}

/// [`add_cog`], a GeoTIFF that can't be read now added all the same (unless `strict`), showing
/// why: a project being imported keeps its layers.
pub(crate) fn add_cog_with(s: &mut Session, p: &Value, url: &str, strict: bool, probed: bool) -> CmdResult {
    let stem = cog_name(url);
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&stem);
    let empty_before = super::map_is_empty(s);
    // A GeoTIFF is a raster layer like a file's, read in place; a dated series, a data cube's file
    // (which follow the time slider) and AlphaEarth's quantised embeddings (which raster styles
    // don't dequantise) are tile layers.
    let (file, mut look) = crate::render::rustgis_tiles::cog::split(url);
    // A key given as `null` (a script's unused one) asks for nothing.
    let given = |k: &str| p.get(k).filter(|v| !v.is_null());
    // A data cube's file stays a tile layer, its project opened again too: its address says so.
    look.cube |= b(p, "cube") == Some(true);
    let dated = rustgis_data::time::is_template(file) || ["start", "end", "unit", "interval", "dates"].iter().any(|k| given(k).is_some());
    if !dated && !look.aef && !look.cube {
        return add_in_place(s, name, file, &look, strict, b(p, "zoom"), probed);
    }
    let url = crate::render::rustgis_tiles::cog::with_look(url, &look);
    let mut q = json!({"url": url, "kind": "cog", "name": name, "top": true});
    // A dated address's span and steps (see map.addTiles).
    for k in ["start", "end", "unit", "interval", "dates", "connection"] {
        if let Some(v) = given(k) {
            q[k] = v.clone();
        }
    }
    let mut r = super::tiles::add(s, &q)?;
    // A plugin's command in the app doesn't wait (it runs on the frame), as in the browser.
    let probed = probed && !s.leave_downloads;
    // A dated address is framed on the file of the date it shows; why its file couldn't be read.
    let (url, probed, mut why) = if r.get("dated").and_then(Value::as_bool) == Some(true) {
        // The public answer is redacted; metadata uses the installed tile's exact address.
        let index =
            r.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).ok_or("added GeoTIFF has no tile index")?;
        let dated = s.map().tiles.get(index).ok_or("added GeoTIFF tile is missing")?.url.clone();
        // Its header read now (natively) as an undated address's is before it is added.
        #[cfg(not(target_arch = "wasm32"))]
        let probe = if s.leave_downloads { Ok(false) } else { crate::render::rustgis_tiles::cog::probe_blocking(&dated) };
        #[cfg(target_arch = "wasm32")]
        let probe: Result<bool, String> = Ok(false);
        (dated, probe == Ok(true), probe.err())
    } else {
        (url.to_string(), probed, None)
    };
    #[cfg(not(target_arch = "wasm32"))]
    let info = if probed {
        crate::render::rustgis_tiles::cog::info_blocking(&url).map(Some)
    } else {
        crate::render::rustgis_tiles::cog::info(&url)
    };
    #[cfg(target_arch = "wasm32")]
    let info = crate::render::rustgis_tiles::cog::info(&url);
    let info = match info {
        Ok(info) => info,
        Err(e) => {
            why.get_or_insert(e);
            None
        }
    };
    if let Some(info) = info {
        let [w, so, e, n] = info.bounds;
        // A cloud-optimised GeoTIFF read in place is framed when it is the first content on the
        // map, as other data is; `"zoom"` says otherwise (`true`, or `false` to keep the view).
        if b(p, "zoom").unwrap_or(empty_before) {
            s.zoom_to(&rustgis_geom::Extent::new(w, so, e, n));
        }
        r["bounds"] = json!(info.bounds);
        // A tile layer's own name, as added (and a dated series' file); its header's losses said
        // now, and not again as the map draws (see `Session::notice_landed_rasters`).
        let index = r.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok());
        let mut shown = r.get("name").and_then(Value::as_str).unwrap_or(name).to_string();
        if let Some(t) = index.and_then(|i| s.map_mut().tiles.get_mut(i)) {
            shown = cog_tile_where(t, &t.url);
            let file = crate::render::rustgis_tiles::cog::split(&t.url).0.to_string();
            if !t.headers_said.contains(&file) {
                t.headers_said.push(file);
            }
        }
        for line in cog_tile_losses(&info) {
            s.notify(format!("{shown}: {line}"), false);
        }
    } else if probed || why.is_some() || r.get("dated").is_some() {
        // Added, but its (first date's) file couldn't be read: the view stays where it was.
        r["warning"] = json!(match why {
            Some(e) => crate::sources::message_with_address(&url, &format!("couldn't be read ({e}), so the map wasn't moved to it"),),
            None => crate::sources::message_with_address(&url, "couldn't be read yet, so the map wasn't moved to it"),
        });
    }
    r["inPlace"] = json!(true);
    Ok(r)
}

/// Where a cloud-optimised GeoTIFF tile layer's header losses are said: its name, and for a dated
/// series the file of `address`, one of its dates' (the last part of its path: the host, a login
/// wrapper, the query and the fragment stay out of what is said).
pub(crate) fn cog_tile_where(t: &rustgis_map::tiles::TileLayer, address: &str) -> String {
    if t.time_url.is_none() && t.time_urls.is_empty() {
        return t.name.clone();
    }
    let inner = rustgis_data::login_address::inner(rustgis_data::s3_address::inner(address));
    let address = inner.split(['?', '#']).next().unwrap_or(inner);
    // The path after the host (an address without a scheme is a path already); one without a
    // path names no file.
    let path = match address.split_once("://") {
        Some((_, rest)) => rest.split_once('/').map_or("", |(_, path)| path),
        None => address,
    };
    let whole = path.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut file: String = whole.chars().take(80).collect();
    if whole.chars().nth(80).is_some() {
        file.push('…');
    }
    if file.is_empty() { t.name.clone() } else { format!("{} ({file})", t.name) }
}

/// What a cloud-optimised GeoTIFF drawn as a tile layer leaves out of its header, a line each:
/// drawn in place, its tiles are drawn as values (a palette image's by its colour table), so what
/// the header holds beside them (band and class names, wavelengths) comes only with a window. Said
/// as the layer is added, or once its header lands ([`Session::notice_landed_rasters`]).
pub(crate) fn cog_tile_losses(info: &crate::render::rustgis_tiles::cog::Info) -> Vec<String> {
    let l = &info.layout;
    let named = l
        .described
        .as_ref()
        .is_some_and(|d| d.bands.iter().any(|b| b.description.is_some() || !b.categories.is_empty() || b.wavelength.is_some()));
    let mut said: Vec<String> = l.lost.clone();
    said.extend(l.described.iter().flat_map(|d| d.lost.iter().map(|x| format!("{}: {x}", rustgis_data::raster::pam::GDAL_METADATA))));
    if named {
        said.push("drawn in place without its band names, class names or wavelengths: a window (\"bbox\") reads them".into());
    }
    said
}

/// Adds a cloud-optimised GeoTIFF as a raster layer read in place: its overview held (read now
/// where the session waits for reads in place, failing the command when it can't be; landing as
/// the map draws on the desktop and in the browser, see [`Session::in_place_wait`]), the part in
/// view read as the map draws. A look at the address's end (`#bands=…&range=…`) is its style.
/// `zoom` is the adding command's `"zoom"`:
/// `Some(true)` frames it, `Some(false)` keeps the view, `None` frames it only as the first content
/// on an empty map. `probed` is [`add_cog`]'s.
fn add_in_place(
    s: &mut Session,
    name: &str,
    file: &str,
    look: &crate::render::rustgis_tiles::cog::Look,
    strict: bool,
    zoom: Option<bool>,
    probed: bool,
) -> CmdResult {
    use crate::render::rustgis_tiles::cog;
    // Judged as an address a project keeps is (a host, no spaces, not too long) before it is read.
    rustgis_map::tiles::check_url(file)?;
    let empty_before = super::map_is_empty(s);
    // Strict, where the session waits: read now, failing when it can't be. Strict on the desktop:
    // added now, its overview landing as the map draws (and failing there, said), so a slow host
    // doesn't hold the window. Otherwise read with the other layers added with it (see
    // `project::read_in_place`); in the browser, landing later.
    #[cfg(not(target_arch = "wasm32"))]
    let layer = if strict && s.in_place_wait {
        rustgis_map::raster::RasterLayer::in_place(cog::raster::overview_blocking(file)?, file)
    } else if strict {
        rustgis_map::raster::RasterLayer::opening_in_place(file)
    } else {
        rustgis_map::raster::RasterLayer::unread_in_place(file)
    };
    #[cfg(target_arch = "wasm32")]
    let layer = {
        let _ = strict;
        rustgis_map::raster::RasterLayer::opening_in_place(file)
    };
    let layer = layer.with_look(super::tiles::address_look(look));
    let landed = layer.data.is_some();
    let mut r = super::raster::add_raster_after(s, name, layer, zoom)?;
    // A header just read (natively) is waited for if something emptied the tile cache since (a
    // login ended): the layer would go unframed. A plugin's command in the app doesn't wait (it
    // runs on the frame), as in the browser.
    #[cfg(not(target_arch = "wasm32"))]
    let info = if probed && !s.leave_downloads { cog::info_blocking(file).map(Some) } else { cog::info(file) };
    #[cfg(target_arch = "wasm32")]
    let info = {
        let _ = probed;
        cog::info(file)
    };
    // Its header couldn't be read (a failure recorded and not tried again yet, or now): the view
    // stays where it was, and the answer says why (the layer says so too, once its read fails).
    if let Err(e) = &info {
        r["warning"] =
            json!(crate::sources::message_with_address(file, &format!("couldn't be read ({e}), so the map wasn't moved to it"),));
    }
    if let Ok(Some(info)) = info {
        let [w, so, e, n] = info.bounds;
        // Framed on the file's bounds when it is the first content on the map, as other data is,
        // or when asked to.
        if zoom.unwrap_or(empty_before) {
            s.zoom_to(&rustgis_geom::Extent::new(w, so, e, n));
        }
        r["bounds"] = json!(info.bounds);
        // Added before its overview is in: sized by the header as the answer would be once the
        // overview had landed (the level the layer holds, every band, and the full size when the
        // level isn't it).
        if !landed && let Some(level) = cog::raster::overview_level(&info) {
            let levels = &info.layout.levels;
            if let Some(l) = levels.get(level) {
                r["width"] = json!(l.width);
                r["height"] = json!(l.height);
                r["bands"] = json!(info.layout.bands);
            }
            if let Some(full) = levels.first().filter(|_| level > 0) {
                r["overviewOf"] = json!([full.width, full.height]);
            }
        }
    }
    if !landed {
        // Its overview is still being read (it lands as the map draws): a size the answer can't
        // give yet (the header not in, or no level small enough to hold) is null, not 0.
        r["reading"] = json!(true);
        if r["width"].as_u64() == Some(0) {
            for key in ["width", "height", "bands"] {
                r[key] = Value::Null;
            }
        }
    }
    r["inPlace"] = json!(true);
    r["kind"] = json!("raster");
    let added = r["layer"].as_u64().and_then(|id| u32::try_from(id).ok()).and_then(|id| s.map().layer(id));
    r["name"] = json!(added.map(|l| l.name.clone()));
    Ok(r)
}

/// The most values a window of a cloud-optimised GeoTIFF holds, all its bands together: one read's
/// most (256 MB of 32-bit values). Under test, 300,000, so the refusal is tried on a small file.
#[cfg(not(target_arch = "wasm32"))]
const MAX_WINDOW_VALUES: u64 = if cfg!(test) { 300_000 } else { crate::render::rustgis_tiles::cog::MAX_VALUES_READ };

/// The most bands a window reads, [`WINDOW_BATCH`] a read (8 under test, whose examples
/// have 16 bands at most).
#[cfg(not(target_arch = "wasm32"))]
const MAX_WINDOW_BANDS: usize = if cfg!(test) { 8 } else { 256 };

/// How long a window's bands are read for, all together: past it, no band more is asked for.
#[cfg(not(target_arch = "wasm32"))]
const WINDOW_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

/// The bands of a window read together, at most: their tiles asked for at once.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const WINDOW_BATCH: usize = 16;

/// How long a window's band may be waited for: a batch of bands has this much a band.
#[cfg(not(target_arch = "wasm32"))]
const BAND_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// The extension an address's file name has (lowercase, without the dot), if it has one: the
/// query and fragment aside.
pub(crate) fn address_extension(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    // The host isn't a file name (`https://data.example.com`): only what follows it is.
    let after_host = path.split_once("://").map_or(path, |(_, rest)| rest).split_once('/')?.1;
    let file = after_host.rsplit('/').next().filter(|f| !f.is_empty())?;
    let (stem, ext) = file.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty() && ext.len() <= 10 && ext.bytes().all(|c| c.is_ascii_alphanumeric()))
        .then(|| ext.to_ascii_lowercase())
}

/// `layer.addUrl` keys for other kinds of address, which a window (`"bbox"`) doesn't read.
const WINDOW_UNREAD: [&str; 17] = [
    "layer",
    "x",
    "y",
    "wkt",
    "crs",
    "delimiter",
    "paperSpace",
    "blocks",
    "stream",
    "start",
    "end",
    "unit",
    "interval",
    "dates",
    "service",
    "coverage",
    "bounds",
];

/// Metres in a degree, to compare a cell size in metres with a longitude/latitude file's pixels.
#[cfg(not(target_arch = "wasm32"))]
const METRES_PER_DEGREE: f64 = 111_320.0;

/// A web address's lowercase form (the address it wraps when read through a saved connection), or
/// why it isn't one.
fn web_address(url: &str) -> Result<String, String> {
    let lower = rustgis_data::s3_address::inner(crate::render::rustgis_tiles::logins::inner(url)).to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("s3://")) {
        return Err(format!("{url}: give an http://, https:// or s3:// address (Add Data opens files)"));
    }
    Ok(lower)
}

/// Refuses a GeoLibre project's download state, which only the app's import sets.
fn no_geolibre_state(p: &Value) -> Result<(), String> {
    if ["geolibreProject", "geolibreLayer", "geolibreZarr"].iter().any(|key| p.get(key).is_some()) {
        return Err("GeoLibre download state is internal; use project.importGeolibre".into());
    }
    Ok(())
}

/// Refuses what can't be a window of `url`, before anything is fetched.
fn window_checks(p: &Value, url: &str, scan: bool) -> Result<(), String> {
    no_geolibre_state(p)?;
    if scan || b(p, "whole") == Some(true) || rustgis_data::time::is_template(url) {
        return Err(format!(
            "{url}: \"bbox\" reads a window of one cloud-optimised GeoTIFF with its coordinates: leave out \"as\" and \"whole\", and give a single file's address"
        ));
    }
    // An address naming another kind of file is refused before anything is fetched (one
    // with no extension, or a GeoTIFF's, is read; `"cog": true` says it is one).
    if b(p, "cog") != Some(true)
        && let Some(ext) =
            address_extension(rustgis_data::s3_address::inner(url)).filter(|e| !matches!(e.as_str(), "tif" | "tiff" | "geotiff"))
    {
        return Err(format!(
            "{url}: \"bbox\" reads a window of a cloud-optimised GeoTIFF, and this address names a .{ext} file: leave out \"bbox\" (or give \"cog\": true for a GeoTIFF by another name)"
        ));
    }
    // What a window doesn't read is refused, not dropped: it keeps the file's coordinate system.
    // A data cube's file is a tile layer that follows the time slider, and a window is a raster:
    // `"cube": false` asks for nothing.
    let cube = (b(p, "cube") == Some(true)).then_some("cube");
    let unread: Vec<&str> = WINDOW_UNREAD.iter().copied().filter(|k| p.get(*k).is_some_and(|v| !v.is_null())).chain(cube).collect();
    if !unread.is_empty() {
        return Err(format!("{url}: a window (\"bbox\") doesn't read {}: leave it out", unread.join(", ")));
    }
    Ok(())
}

/// A window asked for, with its box settled (`"view"` is the view when it was asked for): what
/// [`read_cog_window`] needs, on any thread.
#[cfg(not(target_arch = "wasm32"))]
pub struct WindowAsked {
    url: String,
    bbox: [f64; 4],
    cell: Option<f64>,
    name: Option<String>,
    /// The bands the file must have, when the caller needs a number of them (checked from its
    /// header, before anything else is read).
    bands: Option<usize>,
    /// Whether `cell` was worked out by the caller (not asked for by the user): a window whose full
    /// image has finer cells than it is the intended read, not something to say.
    auto_cell: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl WindowAsked {
    /// A window of the file at `url` over `bbox` (west, south, east, north in degrees), with the
    /// coarsest level whose cells are at most `cell` (the full image without), as `layer.addUrl`
    /// with that box and `"cellSize"` reads it; a file without `bands` bands is refused from its
    /// header.
    pub(crate) fn of(url: &str, bbox: [f64; 4], cell: Option<f64>, bands: usize) -> WindowAsked {
        WindowAsked { url: url.to_string(), bbox, cell, name: None, bands: Some(bands), auto_cell: false }
    }
    /// The cell size is the caller's own choice, not the user's: a full image finer than it is
    /// read without saying so.
    pub(crate) fn automatic(mut self) -> WindowAsked {
        self.auto_cell = true;
        self
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
impl WindowAsked {
    pub(crate) fn bbox(&self) -> [f64; 4] {
        self.bbox
    }
}

/// A window read by [`read_cog_window`], waiting to be added to the project: its raster made and
/// its GeoTIFF written, so that adding it holds no frame for long.
#[cfg(not(target_arch = "wasm32"))]
pub struct CogWindow {
    name: String,
    layer: rustgis_map::RasterLayer,
    /// What to say of it, once the layer has its name.
    notes: Vec<String>,
    columns: [u32; 2],
    rows: [u32; 2],
    level: usize,
    cell_size: f64,
}

/// `p`'s box and cell size, read on the frame, which holds the view.
#[cfg(not(target_arch = "wasm32"))]
fn window_settled(s: &Session, p: &Value, url: &str, seen: Option<[f64; 4]>) -> Result<WindowAsked, String> {
    let named = |e: String| if e.starts_with(url) { e } else { format!("{url}: {e}") };
    let bbox = window_box(s, p, seen).map_err(named)?;
    let cell = match p.get("cellSize").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(v.as_f64().filter(|c| c.is_finite() && *c > 0.0).ok_or_else(|| named("cellSize is a size above 0".into()))?),
    };
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(String::from);
    Ok(WindowAsked { url: url.to_string(), bbox, cell, name, bands: None, auto_cell: false })
}

/// A window the app reads off the frame: `p`, a `layer.addUrl` with `"bbox"` and its address bound
/// to its login, checked as the command checks it. Errors quote the address without its keys.
#[cfg(not(target_arch = "wasm32"))]
pub fn window_asked(s: &Session, p: &Value) -> Result<WindowAsked, String> {
    window_asked_in(s, p, None)
}

/// [`window_asked`], `"bbox": "view"` being `seen` when given: the box the view was when a request
/// was queued ([`crate::UrlRequest::view_asked`]).
#[cfg(not(target_arch = "wasm32"))]
pub fn window_asked_in(s: &Session, p: &Value, seen: Option<[f64; 4]>) -> Result<WindowAsked, String> {
    let url = ps(p, "url").map(str::trim).filter(|u| !u.is_empty()).ok_or("url is required")?;
    let checked = web_address(url)
        .and_then(|_| super::raster::as_scan(p))
        .and_then(|scan| window_checks(p, url, scan))
        .and_then(|()| window_settled(s, p, url, seen));
    checked.map_err(|e| crate::sources::message_with_address(url, &e))
}

/// `layer.addUrl` with `"bbox"`, for those that wait: read here and added.
#[cfg(not(target_arch = "wasm32"))]
fn add_cog_window(s: &mut Session, p: &Value, url: &str) -> CmdResult {
    let read = read_cog_window(&window_settled(s, p, url, None)?)?;
    add_window(s, p, read)
}

/// `layer.addUrl` with `"bbox"` (opengeos/RustGIS#1204): a window of a cloud-optimised GeoTIFF,
/// read by range as a raster layer. The box (longitude/latitude, or the view) is turned into the
/// file's coordinate system and snapped outwards onto the pixels of the level read: the full image,
/// or with `"cellSize"` the coarsest overview whose cells are no larger. Only the tiles the window
/// takes are read, with the reader the data cubes use. The raster keeps the file's coordinate
/// system, no-data, colour table and band and class names, and is kept in the project (as a
/// GeoTIFF of the window), not read again from the address.
///
/// This waits for the file, and needs no [`Session`]: the app runs it on a thread and adds what it
/// reads when it lands (`layer.addDownload`, [`Session::download_landed_window`]; #1330). Errors
/// quote the address without its keys.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_cog_window(asked: &WindowAsked) -> Result<CogWindow, String> {
    read_cog_window_until(asked, &std::sync::atomic::AtomicBool::new(false))
}

/// The cell size to say is finer than the file's own: the one asked, when no level has cells that
/// small (`fits` false) and the user asked it (not `auto`, a size the caller worked out).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn finer_than_file(cell: Option<f64>, fits: bool, auto: bool) -> Option<f64> {
    cell.filter(|_| !fits && !auto)
}

/// [`read_cog_window`]'s raster, giving up within a moment of `stop` being set (the app's Cancel of
/// the download, or its project closed): in the wait on the header, before each batch of bands
/// and in the wait on their tiles. Making the raster, once the values are in, doesn't look.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_window_raster(asked: &WindowAsked, stop: &std::sync::atomic::AtomicBool) -> Result<WindowRaster, String> {
    use crate::render::rustgis_tiles::cog;
    use rustgis_data::raster::cog::Level;
    let url = asked.url.as_str();
    let named = |e: String| crate::sources::message_with_address(url, &e);
    let [west, south, east, north] = asked.bbox;
    let cell = asked.cell;
    // The five minutes count from the header's read.
    let started = std::time::Instant::now();
    let info = cog::info_until(url, stop).map_err(named)?;
    let layout = &info.layout;
    let (pw, ph) = (layout.transform.pixel_width, layout.transform.pixel_height);
    if !(pw.is_finite() && ph.is_finite() && pw != 0.0 && ph != 0.0) {
        return Err(named("its header gives no pixel size, so no window can be placed in it".into()));
    }
    if pw < 0.0 {
        return Err(named("it is stored east to west, which a window doesn't read yet".into()));
    }
    // Stored bottom-up (AlphaEarth's files): the rows are read south first, and turned over once
    // read, so the window is north up as any other.
    let bottom_up = ph > 0.0;
    // The box in the file's coordinates: its edges at 32 points each, and each piece halved until
    // it is straight in the file to within a quarter of its pixel. A projected file bends them (a
    // parallel in a conic projection is an arc whose lowest point can fall between two points).
    let tr = layout.crs.transformer().map_err(named)?;
    let place = |lon: f64, lat: f64| tr.from_lonlat(rustgis_geom::Coord::new(lon, lat));
    let tolerance = layout.transform.pixel_width.abs().min(layout.transform.pixel_height.abs()) / 4.0;
    let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    // Edge points the file's coordinate system has no place for, said once the window is in.
    let (mut points, mut unplaced) = (0usize, 0usize);
    // Bent pieces the depth or point limit left unhalved: said, as the window may then fall short
    // of a bent edge.
    let mut unrefined = 0usize;
    let mut pieces = Vec::new();
    for k in 0..32 {
        let (t, u) = (f64::from(k) / 32.0, f64::from(k + 1) / 32.0);
        let (lon, lat) = (|t: f64| west + (east - west) * t, |t: f64| south + (north - south) * t);
        pieces.extend([
            ((lon(t), south), (lon(u), south), 0u32),
            ((lon(t), north), (lon(u), north), 0),
            ((west, lat(t)), (west, lat(u)), 0),
            ((east, lat(t)), (east, lat(u)), 0),
        ]);
    }
    // The pieces' ends, 33 an edge.
    for k in 0..=32 {
        let t = f64::from(k) / 32.0;
        let (lon, lat) = (west + (east - west) * t, south + (north - south) * t);
        for (x, y) in [(lon, south), (lon, north), (west, lat), (east, lat)] {
            points += 1;
            match place(x, y) {
                Some(c) => (x0, y0, x1, y1) = (x0.min(c.x), y0.min(c.y), x1.max(c.x), y1.max(c.y)),
                None => unplaced += 1,
            }
        }
    }
    // Each piece's middle is placed, and the piece halved while the middle strays from the
    // straight line between its ends, or toward where the file's system stops placing points
    // (the edge of its projection's domain, between a point it places and one it doesn't).
    while let Some((a, b, depth)) = pieces.pop() {
        let m = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        points += 1;
        let c = place(m.0, m.1);
        match c {
            Some(c) => (x0, y0, x1, y1) = (x0.min(c.x), y0.min(c.y), x1.max(c.x), y1.max(c.y)),
            None => unplaced += 1,
        }
        // Bent: all three placed and the middle off the line. Toward a domain's edge (some placed,
        // some not) the halving stops at the depth limit by design, and the points unplaced are said.
        let (halve, bent) = match (place(a.0, a.1), place(b.0, b.1), c) {
            (Some(pa), Some(pb), Some(c)) => {
                let off = (c.x - (pa.x + pb.x) / 2.0).hypot(c.y - (pa.y + pb.y) / 2.0) > tolerance;
                (off, off)
            }
            (None, None, None) => (false, false),
            _ => (true, false),
        };
        if halve && depth < 12 && points < 200_000 {
            pieces.extend([(a, m, depth + 1), (m, b, depth + 1)]);
        } else if bent {
            unrefined += 1;
        }
    }
    if !(x0 < x1 && y0 < y1) {
        return Err(named("the box is outside the file's coordinate system".into()));
    }
    // A level's cell size, its longer side, in metres for a longitude/latitude file.
    let geographic = layout.crs.is_geographic();
    // A longitude/latitude file stored 0° to 360° (a climate grid) takes the box in its range.
    if geographic && let Some(full) = layout.levels.first() {
        let (fx0, pw) = (layout.transform.origin_x, layout.transform.pixel_width);
        (x0, x1) = box_in_file_longitudes(x0, x1, fx0, fx0 + f64::from(full.width) * pw).map_err(named)?;
    }
    let size_of = |l: &Level| {
        let lt = layout.transform_of(l);
        let px = lt.pixel_width.abs().max(lt.pixel_height.abs());
        if geographic { px * METRES_PER_DEGREE } else { px }
    };
    // A level's window: its columns and rows, snapped outwards and kept within the file.
    let window = |l: &Level| {
        let lt = layout.transform_of(l);
        let at = |v: f64, most: u32| v.clamp(0.0, f64::from(most)) as u32;
        let (c0, c1) =
            (at(((x0 - lt.origin_x) / lt.pixel_width).floor(), l.width), at(((x1 - lt.origin_x) / lt.pixel_width).ceil(), l.width));
        // The file's first row is its northern edge, or its southern one when stored bottom-up.
        let (first, last) = if bottom_up { (y0, y1) } else { (y1, y0) };
        let (r0, r1) = (
            at(((first - lt.origin_y) / lt.pixel_height).floor(), l.height),
            at(((last - lt.origin_y) / lt.pixel_height).ceil(), l.height),
        );
        (c0, c1, r0, r1)
    };
    let values = |l: &Level| {
        let (c0, c1, r0, r1) = window(l);
        u64::from(c1 - c0).saturating_mul(u64::from(r1 - r0)).saturating_mul(layout.bands.max(1) as u64)
    };
    // The full image, or the coarsest overview whose cells are no larger than the size asked. A
    // size finer than the full image's cells reads the full image, and says so.
    let fits =
        cell.and_then(|c| (0..layout.levels.len()).rev().find(|i| layout.levels.get(*i).is_some_and(|l| size_of(l) <= c * (1.0 + 1e-9))));
    let level = fits.unwrap_or(0);
    let finer = finer_than_file(cell, fits.is_some(), asked.auto_cell);
    let lv = layout.levels.get(level).ok_or_else(|| named("the file has no image".into()))?;
    let (c0, c1, r0, r1) = window(lv);
    if c1 <= c0 || r1 <= r0 {
        return Err(named("the box is outside the file".into()));
    }
    // The sides of the box past the file's edges, which the window is cut at (snapping outwards
    // onto the pixels isn't a cut).
    let cut: Vec<&str> = {
        let lt = layout.transform_of(lv);
        let (fx1, fy) = (lt.origin_x + f64::from(lv.width) * lt.pixel_width, lt.origin_y + f64::from(lv.height) * lt.pixel_height);
        let (south, north) = (fy.min(lt.origin_y), fy.max(lt.origin_y));
        [
            ("west", x0 < lt.origin_x - tolerance),
            ("east", x1 > fx1 + tolerance),
            ("south", y0 < south - tolerance),
            ("north", y1 > north + tolerance),
        ]
        .into_iter()
        .filter_map(|(side, past)| past.then_some(side))
        .collect()
    };
    let unit = if geographic { " m" } else { " (the file's units)" };
    if values(lv) > MAX_WINDOW_VALUES {
        // The finest overview the window fits in, offered, never read unasked.
        let take = match layout.levels.iter().skip(level).find(|l| values(l) <= MAX_WINDOW_VALUES) {
            Some(l) => format!("take \"cellSize\": {} or more, or a smaller box", size_of(l).ceil()),
            None => "take a smaller box".into(),
        };
        return Err(named(format!(
            "the window is {} × {} cells of {:.1}{unit}, {} band(s); a window holds {MAX_WINDOW_VALUES} values at most: {take}",
            c1 - c0,
            r1 - r0,
            size_of(lv),
            layout.bands.max(1)
        )));
    }
    let count = layout.bands.max(1);
    if let Some(want) = asked.bands.filter(|n| *n != count) {
        return Err(named(format!("it has {count} band(s), where {want} are needed")));
    }
    if count > MAX_WINDOW_BANDS {
        return Err(named(format!("the file has {count} bands; a window reads {MAX_WINDOW_BANDS} at most")));
    }
    // One read a batch of bands (their tiles asked for together, so a file of many bands stored
    // apart isn't read one band after another), each waiting up to two minutes a band
    // ([`batch_wait`]); no batch is started once five minutes are up (the header's read counted),
    // and none runs past two minutes more, as the last band started could when they were read
    // one at a time.
    // The values `f32` rounded (32- and 64-bit whole numbers and floats), a count a band, are said
    // when the layer is added, as for a file read whole.
    // Those past its range can't be held: left out (no value), and said apart.
    let (mut bands, mut rounded, mut overflowed) = (Vec::with_capacity(count), Vec::with_capacity(count), 0u64);
    let all: Vec<usize> = (0..count).collect();
    for batch in all.chunks(WINDOW_BATCH) {
        band_in_time(batch.first().copied().unwrap_or(0), count, started.elapsed(), WINDOW_WAIT).map_err(named)?;
        let wait = batch_wait(batch.len(), started.elapsed());
        for (values, changed, past) in
            cog::read_bands_counted_blocking(&info, url, level, batch, (c0, c1), (r0, r1), wait, stop).map_err(named)?
        {
            bands.push(values);
            rounded.push(changed);
            overflowed += past;
        }
    }
    let lt = layout.transform_of(lv);
    let mut transform = rustgis_data::raster::GeoTransform {
        origin_x: lt.origin_x + f64::from(c0) * lt.pixel_width,
        origin_y: lt.origin_y + f64::from(r0) * lt.pixel_height,
        ..lt
    };
    if bottom_up {
        let width = (c1 - c0) as usize;
        for band in &mut bands {
            let rows: Vec<f32> = band.chunks(width.max(1)).rev().flatten().copied().collect();
            *band = rows;
        }
        transform.origin_y = lt.origin_y + f64::from(r1) * lt.pixel_height;
        transform.pixel_height = -lt.pixel_height;
    }
    let color = match &layout.palette {
        Some(table) if bands.len() == 1 => rustgis_data::raster::ColorHint::Palette(table.clone()),
        _ => layout.color.clone(),
    };
    // A colour table is one band's: a window of more bands comes in without it, and says so.
    let palette_lost = (layout.palette.is_some() && bands.len() != 1)
        .then(|| format!("its colour table is left out: the window has {} bands, a palette has one", bands.len()));
    let name = asked.name.clone().unwrap_or_else(|| cog_name(url));
    let mut r =
        rustgis_data::raster::Raster::new(&name, c1 - c0, r1 - r0, bands, transform, layout.crs, layout.nodata, color, layout.data_type)
            .map_err(|e| named(e.to_string()))?;
    if rounded.iter().any(|n| *n > 0) {
        r.rounded = rounded;
    }
    if overflowed > 0 {
        r.lost.push(format!("{overflowed} value(s) past the range of 32-bit floats left out: those cells have no value"));
    }
    // What the header holds but isn't read (a colour table, names): said when the layer is added.
    r.lost.extend(layout.lost.iter().cloned());
    r.lost.extend(palette_lost);
    if let Some(described) = &layout.described {
        r.apply_pam(described, rustgis_data::raster::pam::GDAL_METADATA);
    }
    let size = size_of(lv);
    let of = if level == 0 { "the full image".to_string() } else { format!("overview {level} of {}", layout.levels.len() - 1) };
    let read = format!("a {} × {} window of {of}, cells of {size:.1}{unit}, read by range from its address", r.width, r.height);
    let finer = finer.map(|c| format!("cellSize {c} is finer than the file's finest cells ({size:.1}{unit}): the full image is read"));
    let mut edges = Vec::new();
    if !cut.is_empty() {
        edges.push(format!("the box reaches past the file's {} edge(s): the window is cut to the file", cut.join(", ")));
    }
    if unrefined > 0 {
        edges.push(format!(
            "{unrefined} piece(s) of the box's edges not followed to a quarter of a pixel: the window may fall short of a bent edge"
        ));
    }
    if unplaced > 0 {
        edges.push(format!(
            "{unplaced} of the {points} points tested along the box's edges have no place in the file's coordinate system: the window is placed from the others"
        ));
    }
    Ok(WindowRaster { raster: r, read, finer, edges, columns: [c0, c1], rows: [r0, r1], level, cell_size: size })
}

/// [`read_cog_window`], giving up within a moment of `stop` being set (the app's Cancel of the
/// download, or its project closed): in the wait on the header, before each batch of bands and in
/// the wait on their tiles. Making the raster and writing its GeoTIFF, once the values are in,
/// don't look.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_cog_window_until(asked: &WindowAsked, stop: &std::sync::atomic::AtomicBool) -> Result<CogWindow, String> {
    let named = |e: String| crate::sources::message_with_address(&asked.url, &e);
    let WindowRaster { raster: r, read, finer, edges, columns, rows, level, cell_size } = read_window_raster(asked, stop)?;
    let name = asked.name.clone().unwrap_or_else(|| cog_name(&asked.url));
    // Kept in the project as a GeoTIFF of the window: a cloud-optimised one, which keeps a colour
    // table; one it can't write (a palette with no index left for no-data) is kept plain, and said so.
    // Neither writes more bands than colour (or grey and alpha) takes: the others are said.
    let left_out = rustgis_data::raster::cogwrite::bands_written(&r, 0).map_or(0, |b| r.bands.len().saturating_sub(b.len()));
    let band_count = r.bands.len();
    let mut plain = None;
    let bytes = match rustgis_data::raster::cogwrite::write(&r, 0) {
        Ok(bytes) => bytes,
        Err(why) => {
            let bytes = rustgis_data::raster::geotiff::write(&r, 0).map_err(|e| named(e.to_string()))?;
            plain = Some(if matches!(r.color, rustgis_data::raster::ColorHint::Palette(_)) {
                format!(
                    "saved in the project as a plain GeoTIFF without its colour table ({why}): drawn in its colours now, it opens grey once the project is reopened"
                )
            } else {
                format!("saved in the project as a plain GeoTIFF, not a cloud-optimised one ({why})")
            });
            bytes
        }
    };
    let layer = rustgis_map::RasterLayer::from_bytes(r, format!("{name}.tif"), bytes);
    let mut notes = vec![read];
    notes.extend(plain);
    notes.extend(finer);
    if left_out > 0 {
        let kept = band_count - left_out;
        notes.push(format!("the project keeps {kept} of its {band_count} bands: {left_out} band(s) are left out once it is reopened"));
    }
    notes.extend(edges);
    Ok(CogWindow { name, layer, notes, columns, rows, level, cell_size })
}

/// A window's raster as [`read_window_raster`] reads it, every band kept, and what to say of the
/// read: its size and level, a cell size finer than the file's, and the box's edges (cut to the
/// file, not followed, unplaced).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct WindowRaster {
    pub(crate) raster: rustgis_data::raster::Raster,
    read: String,
    finer: Option<String>,
    edges: Vec<String>,
    columns: [u32; 2],
    rows: [u32; 2],
    level: usize,
    cell_size: f64,
}

#[cfg(not(target_arch = "wasm32"))]
impl WindowRaster {
    /// What to say of the read, in order, and what its raster left out (`lost`: what the header
    /// holds but isn't read, values past `f32`'s range), for a caller that makes another raster
    /// from it and so doesn't add this one (whose `lost` the layer would say).
    pub(crate) fn notes(&self) -> Vec<String> {
        self.read_notes().into_iter().chain(self.raster.lost.iter().cloned()).collect()
    }

    /// What to say of the read alone (its size and level, a cell size finer than the file's, the
    /// box's edges), without what the raster left out: the same for windows of one grid.
    pub(crate) fn read_notes(&self) -> Vec<String> {
        std::iter::once(self.read.clone()).chain(self.finer.clone()).chain(self.edges.iter().cloned()).collect()
    }
}

/// Adds a window read by [`read_cog_window`] and says what it read. `p` is the command adding it
/// (`layer.addUrl`, or `layer.addDownload` when the app read it off the frame).
#[cfg(not(target_arch = "wasm32"))]
fn add_window(s: &mut Session, p: &Value, read: CogWindow) -> CmdResult {
    let CogWindow { name, layer, notes, columns, rows, level, cell_size } = read;
    let mut out = super::raster::add_raster_after(s, &name, layer, b(p, "zoom"))?;
    let added = s.active_layer.and_then(|id| s.map().layer(id)).map(|l| l.name.clone()).unwrap_or(name);
    for note in notes {
        s.notify(format!("{added}: {note}"), false);
    }
    out["window"] = json!({"columns": columns, "rows": rows, "level": level, "cellSize": cell_size});
    // `"zoom": true` zooms to the window whatever the map held, as for any other address.
    if b(p, "zoom") == Some(true) {
        out["zoomed"] = json!(zoom_to_added(s, &out));
    }
    Ok(out)
}

/// Whether a window's band `band` (of `count`) may still be started, `elapsed` into a wait of
/// `wait` for them all: the first always is.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn band_in_time(band: usize, count: usize, elapsed: std::time::Duration, wait: std::time::Duration) -> Result<(), String> {
    if band > 0 && elapsed > wait {
        let minutes = wait.as_secs() / 60;
        return Err(format!(
            "{band} of {count} bands read in {minutes} minutes, so the window is given up: take a smaller box or a larger cellSize"
        ));
    }
    Ok(())
}

/// How long a batch of `bands` bands of a window may wait for its tiles, `elapsed` into the
/// window's read: two minutes a band, but no later than two minutes past the window's wait (when
/// the last batch may still start), and never less than one band's two minutes.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn batch_wait(bands: usize, elapsed: std::time::Duration) -> std::time::Duration {
    let left = (WINDOW_WAIT + BAND_WAIT).saturating_sub(elapsed).max(BAND_WAIT);
    BAND_WAIT.saturating_mul(u32::try_from(bands.max(1)).unwrap_or(u32::MAX)).min(left)
}

/// A window can't be read in the web app yet: nothing there may wait for the tiles.
#[cfg(target_arch = "wasm32")]
fn add_cog_window(_: &mut Session, _: &Value, url: &str) -> CmdResult {
    Err(format!(
        "{url}: a window of a cloud-optimised GeoTIFF (\"bbox\") is read on the desktop and the command line for now; here, leave out \"bbox\" to draw the file in place"
    ))
}

/// A longitude/latitude box's west and east, `x0..x1`, in the range of a file whose columns span
/// `fx0..fx1` degrees: shifted by 360° when that puts more of it over the file (a file stored 0°
/// to 360°, a box given −180° to 180°). A box across the file's seam, which would take columns at
/// both of its ends (one part as given and one shifted), is refused rather than cut at the seam.
/// A box of the whole world takes the world from the file's west edge: all of a 0° to 360° file.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn box_in_file_longitudes(x0: f64, x1: f64, fx0: f64, fx1: f64) -> Result<(f64, f64), String> {
    if x1 - x0 >= 360.0 - 1e-9 {
        return Ok((fx0, fx0 + 360.0));
    }
    let over = |a: f64, b: f64| (b.min(fx1) - a.max(fx0)).max(0.0);
    // The whole turns that bring the box nearest the file (its columns may start a turn or more
    // away, at 720°), and one either side of them.
    let near = (((fx0 + fx1) - (x0 + x1)) / 720.0).round() * 360.0;
    let turns = [0.0, near - 360.0, near, near + 360.0];
    let mut best = (x0, x1);
    for k in turns {
        if over(x0 + k, x1 + k) > over(best.0, best.1) {
            best = (x0 + k, x1 + k);
        }
    }
    let (a, b) = best;
    let mut on: Vec<f64> = turns.iter().filter(|k| over(x0 + **k, x1 + **k) > 0.0).map(|k| x0 + k).collect();
    on.dedup_by(|p, q| (*p - *q).abs() < 1e-9);
    let both = on.len() > 1;
    if both || (fx1 - fx0 >= 360.0 - 1e-6 && (a < fx0 - 1e-9 || b > fx1 + 1e-9)) {
        let seam = if on.iter().copied().fold(a, f64::min) < fx0 { fx0 } else { fx1 };
        return Err(format!("the box crosses the file's seam at {seam}° of longitude: give one box on each side of it"));
    }
    Ok(best)
}

/// A window's box: `[west, south, east, north]` in degrees, or `"view"`, the map's extent.
#[cfg(not(target_arch = "wasm32"))]
fn window_box(s: &Session, p: &Value, seen: Option<[f64; 4]>) -> Result<[f64; 4], String> {
    const NEED: &str = "bbox is [west, south, east, north] in degrees, or \"view\"";
    let [w, so, e, n] = match p.get("bbox") {
        Some(v) if is_view(Some(v)) => match seen {
            // The view a request was asked in ([`crate::UrlRequest::view_asked`]).
            Some(b) => b,
            None => {
                let e = super::workspace::view_extent(s);
                if e.is_empty() {
                    return Err("the map's extent isn't known; give bbox".into());
                }
                // A view past 180° (across the antimeridian, or the world more than once) isn't
                // cut to fit: what it shows past there would be left out.
                if e.min_x < -180.0 || e.max_x > 180.0 {
                    return Err("the view reaches past 180° of longitude; give bbox".into());
                }
                [e.min_x, e.min_y.max(-90.0), e.max_x, e.max_y.min(90.0)]
            }
        },
        Some(Value::Array(a)) => {
            let v: Option<Vec<f64>> = a.iter().map(|x| x.as_f64().filter(|x| x.is_finite())).collect();
            match v.as_deref() {
                Some([w, so, e, n]) => [*w, *so, *e, *n],
                _ => return Err(NEED.into()),
            }
        }
        _ => return Err(NEED.into()),
    };
    let (lon, lat) = (|x: f64| (-180.0..=180.0).contains(&x), |y: f64| (-90.0..=90.0).contains(&y));
    if w < e && so < n && lon(w) && lon(e) && lat(so) && lat(n) { Ok([w, so, e, n]) } else { Err(NEED.into()) }
}

/// A window's box as `layer.addUrl` settles it from `p`'s `"bbox"` (the view, or a box in degrees),
/// for another command that reads windows.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn window_box_of(s: &Session, p: &Value) -> Result<[f64; 4], String> {
    window_box(s, p, None)
}

/// `"bbox": "view"`, in any case or spacing.
#[cfg(not(target_arch = "wasm32"))]
fn is_view(bbox: Option<&Value>) -> bool {
    bbox.and_then(Value::as_str).is_some_and(|v| v.trim().eq_ignore_ascii_case("view"))
}

/// A GeoTIFF's name from its address: the file name without its extension, or for a dated
/// address (`…/landsat/{date:YYYY}.tif`) what is left of it once the dates are taken out, else its
/// folder's name.
fn cog_name(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let mut parts = path.rsplit('/');
    let file = parts.next().unwrap_or("GeoTIFF");
    let file = file.rsplit_once('.').map(|(a, _)| a).unwrap_or(file);
    if !rustgis_data::time::is_template(file) {
        return file.to_string();
    }
    let mut out = String::new();
    let mut depth = 0;
    for c in file.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = (depth - 1).max(0),
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let out = out.trim_matches(['_', '-', '.', ' ']).to_string();
    if out.is_empty() { parts.next().filter(|d| !d.is_empty() && !d.contains(':')).unwrap_or("GeoTIFF").to_string() } else { out }
}

/// Adds a download the app finished (its bytes handed to the session, not passed as text).
fn add_download(s: &mut Session, p: &Value) -> CmdResult {
    // The address may become the layer's source: a login wrapper another session owns is refused,
    // and one given with a connection is bound to that connection.
    let bound = if ps(p, "url").is_some_and(rustgis_data::login_address::is_wrapper) {
        crate::service_logins::request_params(s, p)?
    } else {
        std::borrow::Cow::Borrowed(p)
    };
    let p = bound.as_ref();
    let id = f(p, "id").filter(|v| *v >= 0.0 && v.fract() == 0.0).map(|v| v as u64).ok_or("id is required")?;
    let url = ps(p, "url").unwrap_or("data").to_string();
    // Read before the download is taken, so a bad one leaves it to be added again.
    let service = super::b(p, "service").unwrap_or(false);
    let coverage = super::b(p, "coverage").unwrap_or(false);
    if (service || coverage) && ps(p, "url").is_none_or(|url| url.trim().is_empty()) {
        return Err("url is required for a service or coverage download".into());
    }
    // A window the app read off the frame (#1330).
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(i) = s.landed_windows.iter().position(|(d, _)| *d == id) {
        let (_, read) = s.landed_windows.remove(i);
        let r = add_window(s, p, read)?;
        return added_download(s, p, r, false, &url);
    }
    // No window landed for it: a whole file isn't added in a window's place.
    if p.get("window").is_some_and(Value::is_object) {
        let shown = crate::sources::address_in_message(&url);
        return Err(format!("{shown}: \"window\" is for a window the app read off the frame: use layer.addUrl with \"bbox\""));
    }
    // A download read already (an ArcGIS layer's protocol buffer pages, joined): added as read,
    // with no axes to check against bounds.
    if let Some(i) = s.landed_reads.iter().position(|(d, _)| *d == id) {
        let (_, read) = s.landed_reads.remove(i);
        let r = add_read(s, p, *read, b(p, "zoom"))?;
        return added_download(s, p, r, service || coverage, &url);
    }
    let bounds = if service { super::service::bounds_of(p.get("bounds"))? } else { None };
    let i = s.downloaded.iter().position(|(d, _)| *d == id).ok_or("no such download (it was dropped when another project opened)")?;
    let (_, bytes) = s.downloaded.remove(i);
    if p.get("geolibreProject").and_then(Value::as_bool) == Some(true) {
        if bytes.len() > rustgis_map::geolibre::MAX_TEXT {
            return Err("GeoLibre project exceeds 64 MiB".into());
        }
        let text = String::from_utf8(bytes).map_err(|_| "GeoLibre project must be UTF-8 JSON")?;
        return s.run_part("project.importGeolibre", &json!({"text":text}));
    }
    if p.get("geolibreLayer").is_some() {
        let name = if b(p, "geolibreZarr") == Some(true) { "zarr.tif".into() } else { url_file_name(&url, &bytes)? };
        return super::geolibre::finish_layer(s, p, name, bytes);
    }
    // A service's features (Add From Service): its answer's axes (against its layer's bounds) and
    // paging.
    let bytes = if service {
        let label = ps(p, "name").map(str::to_string).unwrap_or_else(|| crate::sources::address_in_message(&url));
        super::service::features_answer(s, &label, &url, bytes, bounds)
    } else {
        bytes
    };
    // A WCS coverage (Add From Service): a GeoTIFF, or the service's error said as such.
    let name = if coverage {
        let label = ps(p, "name").map(str::to_string).unwrap_or_else(|| crate::sources::address_in_message(&url));
        super::service::coverage_check(&label, &bytes)?;
        "coverage.tif".to_string()
    } else {
        url_file_name(&url, &bytes).map_err(|e| crate::sources::message_with_address(&url, &e))?
    };
    let left_out = parts_left_out(&url, &bytes, p);
    // A coverage is named coverage.tif; a service's features are named from the address too.
    let cut = if coverage { None } else { url_name_cut("Add Downloaded Data", &url, p, &bytes) };
    // A PDF's page the app drew off the frame as it landed (#2336).
    #[cfg(not(target_arch = "wasm32"))]
    let drawn = s.landed_pages.iter().position(|(d, _)| *d == id).map(|i| s.landed_pages.remove(i).1);
    #[cfg(target_arch = "wasm32")]
    let drawn = None;
    let asked = if coverage { super::raster::coverage_system(p, &url) } else { None };
    let r = super::raster::add_named_bytes_read(s, p, name, bytes, b(p, "zoom"), drawn, asked)?;
    if let Some(n) = left_out {
        s.notify(n, false);
    }
    if let Some(cut) = cut {
        s.notify(cut, false);
    }
    added_download(s, p, r, service || coverage, &url)
}

/// A download added (`r`, the add's result): a service's features or coverage marked as read from
/// its address, and the map zoomed to them when asked.
fn added_download(s: &mut Session, p: &Value, mut r: Value, mark_source: bool, url: &str) -> CmdResult {
    // A service's features or coverage: known by the address it came from (Add From Service).
    if mark_source {
        super::service::mark_service_source(s, &r, url);
        // Drawn as its ArcGIS service draws it, when its description said how (#2080).
        if let Some(drawing) = p.get("drawingInfo").filter(|d| d.is_object()) {
            let name = ps(p, "name").map(str::to_string).unwrap_or_else(|| crate::sources::address_in_message(url));
            super::service::apply_drawing_info(s, &r, &name, drawing);
        }
    }
    // `"zoom": true` zooms to everything added, whatever the map held (a link's data); `zoomed`
    // says whether framing happened now, including the tile archive's own bounds handling. A photo result
    // says itself whether it framed the photos this download added (`zoom_to_added` would frame the
    // whole photo layer, earlier photos included), so one that doesn't wasn't framed.
    if b(p, "zoom") == Some(true) {
        let zoomed = r.get("zoomed").and_then(Value::as_bool).unwrap_or_else(|| r.get("photos").is_none() && zoom_to_added(s, &r));
        if let Some(o) = r.as_object_mut() {
            o.insert("zoomed".into(), json!(zoomed));
        }
    }
    Ok(r)
}

/// Zooms to the layers a command's result names (`layer`, `layers`), all of them together;
/// whether the view framed them (not when none had an extent, or the map's projection doesn't
/// reach it).
fn zoom_to_added(s: &mut Session, r: &Value) -> bool {
    let ids: Vec<u32> = r
        .get("layers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(r.get("layer"))
        .filter_map(Value::as_u64)
        .filter_map(|i| u32::try_from(i).ok())
        .collect();
    s.zoom_to_layers(&ids, rustgis_map::ExtentOf::All).unwrap_or(false)
}

pub(crate) fn add_data_text(s: &mut Session, p: &Value) -> CmdResult {
    add_data_text_after(s, p, None)
}

/// [`add_data_text`], with the adding command's `"zoom"` (layer.addUrl, layer.addDownload).
pub(crate) fn add_data_text_after(s: &mut Session, p: &Value, zoom: Option<bool>) -> CmdResult {
    let raw_name = s_req(p, "fileName")?;
    let name = super::raster::bytes_file_name(raw_name).ok_or("fileName is required")?;
    let text = s_req(p, "text")?;
    // A city model: all its layers, in the coordinate system given or with a layer picked. A DXF
    // drawing too, which takes its coordinate system and whether its blocks are drawn from here.
    if rustgis_data::dxf::is_dxf_name(name)
        || rustgis_data::cityjson::is_cityjson_name(name)
        || rustgis_data::io::is_citygml(name, text.as_bytes())
        || (["crs", "layer"].iter().any(|k| ps(p, k).is_some_and(|c| !c.trim().is_empty()))
            && rustgis_data::io::is_cityjson(name, text.as_bytes()))
    {
        return add_layers(s, p, name, text.as_bytes(), zoom, false);
    }
    if let Some(read) = read_delimited(p, name, || Ok(text.as_bytes().to_vec()))? {
        return add_read(s, p, read, zoom);
    }
    let read = rustgis_data::io::read_bytes(name, text.as_bytes()).map_err(|e| format!("{name}: {e}"))?;
    add_read(s, p, read, zoom)
}

/// Adds an imported dataset as a layer or table, forwarding reader notices for either result.
pub(crate) fn add_read(
    s: &mut Session,
    p: &Value,
    read: (rustgis_data::Dataset, rustgis_data::ReadReport),
    zoom: Option<bool>,
) -> CmdResult {
    let (mut ds, report) = read;
    if let Some(n) = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()) {
        ds.name = n.to_string();
    }
    for m in report.messages {
        s.say(m);
    }
    if ds.geometry_type == GeometryType::None {
        let name = ds.name.clone();
        let id = s.project.add_table(ds);
        return Ok(json!({"table": name, "id": id}));
    }
    let n = ds.features.len();
    let empty_before = super::map_is_empty(s);
    let synced = report.synced;
    let id = add_dataset(s, ds);
    if let Some(l) = s.map_mut().layer_mut(id) {
        l.synced = synced;
        if let Some(x) = report.extrusion {
            stand(l, &x);
        }
    }
    if zoom.unwrap_or(empty_before) {
        s.zoom_to_layers(&[id], rustgis_map::ExtentOf::All);
    }
    Ok(json!({"layer": id, "features": n}))
}

/// Extrudes a layer just added as its reader suggests (a city model's buildings and bridges): by
/// its height field, on its base field, each only when it is numeric. A layer that isn't of
/// polygons, or has no numeric height field, stays flat; the height where the field holds no number
/// is 0, so an object with none stays flat too.
fn stand(l: &mut rustgis_map::Layer, x: &rustgis_data::SuggestedExtrusion) {
    if l.dataset.geometry_type != GeometryType::Polygon {
        return;
    }
    let numeric = |n: &str| super::effects::numeric_field(&l.dataset, n).ok();
    let Some(field) = numeric(&x.height_field) else { return };
    let base_field = numeric(&x.base_field);
    l.extrusion = Some(rustgis_map::effects::Extrusion { field: Some(field), base_field, ..Default::default() });
}

fn add_sample(s: &mut Session, p: &Value) -> CmdResult {
    use crate::sample::*;
    let name = s_req(p, "name")?.to_ascii_lowercase();
    let (text, title) = match name.as_str() {
        "countries" => (countries(), "Countries"),
        "cities" => (CITIES, "Cities"),
        "rivers" => (RIVERS, "Rivers"),
        "lakes" => (LAKES, "Lakes"),
        _ => return Err(format!("no sample '{name}'")),
    };
    let (ds, _) = rustgis_data::io::read_geojson(text, title).map_err(|e| e.to_string())?;
    let id = add_dataset(s, ds);
    Ok(json!({"layer": id}))
}

/// Legends that leave out or label layer `old` of the active map, in the frames that show it,
/// follow it: to its new name, or (`None`, the layer deleted) by dropping it, so a later layer of
/// that name isn't left out or labelled unasked. Called once the layer is renamed or gone: while
/// another layer still bears `old` (names may repeat), the legends keep it for that one. Renamed
/// onto the name of another layer a legend lists, the layer is listed with it rather than taking
/// that one out, and under that one's label rather than giving it its own, which is said. A WMS or
/// WMTS layer's name counts as a layer's: legends list those too.
pub(crate) fn legends_follow_layer(s: &mut Session, old: &str, new: Option<&str>) {
    let named = |s: &Session, n: &str| {
        let m = s.map();
        m.layers.iter().map(|l| &l.name).chain(m.tiles.iter().map(|t| &t.name)).filter(|x| x.eq_ignore_ascii_case(n)).count()
    };
    if named(s, old) > 0 {
        return;
    }
    let shared = new.is_some_and(|n| named(s, n) > 1);
    let map = s.map().name.clone();
    let mut unlabelled = 0usize;
    for layout in &mut s.project.layouts {
        let frames: Vec<u32> = layout
            .elements
            .iter()
            .filter(|e| matches!(&e.kind, rustgis_map::layout::ElementKind::MapFrame { map: m, .. } if m.eq_ignore_ascii_case(&map)))
            .map(|e| e.id)
            .collect();
        for e in &mut layout.elements {
            if let rustgis_map::layout::ElementKind::Legend { frame, exclude, labels, .. } = &mut e.kind
                && frames.contains(frame)
            {
                // Its label (#1654), taken off the old name; onto the new one unless another layer
                // bears that name, whose label (or none) the renamed layer then shares.
                let keys: Vec<String> = labels.keys().filter(|n| n.eq_ignore_ascii_case(old)).cloned().collect();
                let label = keys.iter().filter_map(|k| labels.remove(k)).last();
                match (new, label, shared) {
                    (Some(n), Some(l), false) => {
                        labels.retain(|x, _| !x.eq_ignore_ascii_case(n));
                        labels.insert(n.to_string(), l);
                    }
                    (Some(_), Some(_), true) => unlabelled += 1,
                    _ => {}
                }
                match new {
                    // Another layer the legend lists bears the new name: carrying the exclusion
                    // over would take that one out too, so the renamed layer is listed with it.
                    Some(n) if shared && !exclude.iter().any(|x| x.eq_ignore_ascii_case(n)) => {
                        exclude.retain(|x| !x.eq_ignore_ascii_case(old));
                    }
                    Some(n) => {
                        exclude.iter_mut().filter(|x| x.eq_ignore_ascii_case(old)).for_each(|x| *x = n.to_string());
                        // Once each, should the new name have been left out already.
                        let mut seen: Vec<String> = Vec::new();
                        exclude.retain(|x| {
                            let first = !seen.iter().any(|y| y.eq_ignore_ascii_case(x));
                            if first {
                                seen.push(x.clone());
                            }
                            first
                        });
                    }
                    None => exclude.retain(|x| !x.eq_ignore_ascii_case(old)),
                }
            }
        }
    }
    // The text given for the layer goes while the layer stays: said. (A removed layer's goes
    // with its row.)
    if let Some(n) = new.filter(|_| unlabelled > 0) {
        s.notify(
            format!("'{old}', renamed '{n}', lost its label in {unlabelled} legend(s): another layer is called '{n}', and a legend labels layers by name"),
            false,
        );
    }
}

/// Carries typed references to a renamed layer along with it. Map-local references and layout
/// frames stay in their map. A name still present there keeps its references, as legends do;
/// project-wide references without a map stay unchanged when another map still has that name.
/// Script text, tool parameters and history are not rewritten.
fn references_follow_layer(s: &mut Session, old: &str, new: &str) {
    use rustgis_map::design::{Elevation, TopologyRule};
    use rustgis_map::layout::ElementKind;

    let case_only = old.eq_ignore_ascii_case(new);
    let old_present = !case_only && s.map().layers.iter().any(|l| l.name.eq_ignore_ascii_case(old));
    let mut old_left = 0usize;
    let collision = !case_only && s.map().layers.iter().filter(|l| l.name.eq_ignore_ascii_case(new)).count() > 1;
    let mut collision_left = 0usize;
    let mut destination_refs = 0usize;
    let mut replace = |name: &mut String| {
        if name.eq_ignore_ascii_case(old) {
            if old_present {
                old_left += 1;
            } else if collision {
                collision_left += 1;
            } else {
                *name = new.to_string();
            }
        } else if collision && name.eq_ignore_ascii_case(new) {
            destination_refs += 1;
        }
    };
    for layer in &mut s.map_mut().layers {
        if let Some(mask) = &mut layer.mask {
            replace(mask);
        }
    }
    // The terrain's water layer (#1915), kept by name as a mask is.
    if let Some(water) = s.map_mut().view.terrain.as_mut().and_then(|t| t.water.as_mut()) {
        replace(&mut water.layer);
    }
    // And its block's walls layer (#1914).
    if let Some(walls) = s.map_mut().view.terrain.as_mut().and_then(|t| t.block.as_mut()).and_then(|b| b.walls_layer.as_mut()) {
        replace(walls);
    }
    // And the polygons it is cut to (#2318).
    if let Some(rustgis_map::view::BlockShape::Polygon { layer, .. }) =
        s.map_mut().view.terrain.as_mut().and_then(|t| t.block.as_mut()).map(|b| &mut b.shape)
    {
        replace(layer);
    }
    // And the lines point labels are turned square to (#2263), and labels curved along (#2262).
    for layer in &mut s.map_mut().layers {
        for class in std::iter::once(&mut layer.labels.default).chain(layer.labels.classes.iter_mut()) {
            if let Some(to) = &mut class.square_to {
                replace(&mut to.layer);
            }
            if let Some(guide) = &mut class.guide {
                replace(&mut guide.layer);
            }
        }
    }
    // Elevation and Map Series selectors compare spellings exactly, even though lookup does not.
    if case_only {
        if s.map().layers.iter().filter(|l| l.name.eq_ignore_ascii_case(old)).count() == 1 {
            if let Elevation::Surface(layer) = &mut s.map_mut().editing.settings.elevation {
                replace(layer);
            }
            let map_name = s.map().name.clone();
            for layout in &mut s.project.layouts {
                if let Some(series) = &mut layout.series
                    && layout.elements.iter().any(|e| {
                        e.id == series.frame && matches!(&e.kind, ElementKind::MapFrame { map, .. } if map.eq_ignore_ascii_case(&map_name))
                    })
                {
                    replace(&mut series.layer);
                }
            }
        }
        return;
    }
    let map_name = s.map().name.clone();
    let editing = &mut s.map_mut().editing;
    for relation in &mut editing.relationships {
        replace(&mut relation.origin);
        replace(&mut relation.destination);
    }
    for rule in &mut editing.topology {
        match rule {
            TopologyRule::MustNotOverlap { layer }
            | TopologyRule::MustNotHaveGaps { layer }
            | TopologyRule::MustNotSelfIntersect { layer }
            | TopologyRule::MustNotHaveDangles { layer } => replace(layer),
            TopologyRule::MustBeCoveredBy { layer, by } => {
                replace(layer);
                replace(by);
            }
        }
    }
    if let Elevation::Surface(layer) = &mut editing.settings.elevation {
        replace(layer);
    }
    for layout in &mut s.project.layouts {
        let frames: Vec<u32> = layout
            .elements
            .iter()
            .filter_map(|e| match &e.kind {
                ElementKind::MapFrame { map, .. } if map.eq_ignore_ascii_case(&map_name) => Some(e.id),
                _ => None,
            })
            .collect();
        for element in &mut layout.elements {
            match &mut element.kind {
                ElementKind::Table { frame, layer, .. } | ElementKind::Chart { frame, layer, .. } if frames.contains(frame) => {
                    replace(layer)
                }
                ElementKind::Colorbar { frame, layer: Some(layer), .. } if frames.contains(frame) => replace(layer),
                _ => {}
            }
        }
        if let Some(series) = &mut layout.series
            && frames.contains(&series.frame)
        {
            replace(&mut series.layer);
        }
    }
    // These items do not record a map; changing an ambiguous name would redirect references
    // used with another map. Keep those spellings until the user chooses their intended layer.
    let shared = s.project.maps.iter().enumerate().any(|(i, map)| {
        i != s.project.active_map
            && map.layers.iter().any(|layer| layer.name.eq_ignore_ascii_case(old) || layer.name.eq_ignore_ascii_case(new))
    });
    let mut left = 0usize;
    let shared_new = s
        .project
        .maps
        .iter()
        .enumerate()
        .any(|(i, map)| i != s.project.active_map && map.layers.iter().any(|layer| layer.name.eq_ignore_ascii_case(new)));
    let mut project_destination_refs = 0usize;
    let mut project_reference = |name: &mut String| {
        if shared_new && name.eq_ignore_ascii_case(new) {
            project_destination_refs += 1;
        } else if shared && name.eq_ignore_ascii_case(old) {
            left += 1;
        } else {
            replace(name);
        }
    };
    for report in &mut s.project.reports {
        project_reference(&mut report.layer);
    }
    for model in &mut s.project.models {
        if let Some(graph) = &mut model.graph {
            for node in &mut graph.nodes {
                if let Some(layer) = &mut node.layer {
                    project_reference(layer);
                }
            }
        }
    }
    for workspace in &mut s.project.workspaces {
        for image in &mut workspace.images {
            project_reference(image);
        }
    }
    if left > 0 {
        s.notify_text(crate::notice_text::NoticeText::format(
            "{count} project reference(s) still name '{old}': another map has a layer of the old or new name; choose the intended layer in those items",
            [("count", left.to_string().into()), ("old", old.to_string().into())],
        ), false);
    }
    if old_left > 0 {
        s.notify_text(crate::notice_text::NoticeText::format(
            "{count} layer reference(s) still name '{old}': another layer in this map has that name; choose the intended layer in those items",
            [("count", old_left.to_string().into()), ("old", old.to_string().into())],
        ), false);
    }
    if collision_left > 0 {
        s.notify_text(crate::notice_text::NoticeText::format(
            "{count} layer reference(s) still name '{old}': two layers are now called '{new}'; give the layer a unique name and choose it in those items",
            [("count", collision_left.to_string().into()), ("old", old.to_string().into()), ("new", new.to_string().into())],
        ), false);
    }
    let destination_refs = destination_refs + project_destination_refs;
    if destination_refs > 0 {
        s.notify_text(crate::notice_text::NoticeText::format(
            "{count} layer or project reference(s) already name '{new}': the rename created duplicate layer names and may change which layer they use; give the renamed layer a unique name",
            [("count", destination_refs.to_string().into()), ("new", new.to_string().into())],
        ), false);
    }
}

/// Removes the layers `ids` from the active map, and legends that left them out forget them, as
/// `layer.remove` does. Paths that delete several layers at once go through here.
pub(crate) fn drop_layers(s: &mut Session, ids: &[u32]) {
    let m = s.map_mut();
    let names: Vec<String> = m.layers.iter().filter(|l| ids.contains(&l.id)).map(|l| l.name.clone()).collect();
    // The tile layers drawn over them keep their place.
    m.release_tiles(ids);
    m.layers.retain(|l| !ids.contains(&l.id));
    terrain_follows_removal(s, &names);
    square_labels_follow_removal(s, &names);
    for n in &names {
        legends_follow_layer(s, n, None);
    }
}

/// After layers called `names` are removed: point labels turned square to one of them (#2263),
/// and labels curved along one (#2262), keep its name, so a layer of that name brings their line
/// back, and are placed as auto, or as without a guide, meanwhile: said once a kind, naming the
/// layers, as for the terrain's water.
fn square_labels_follow_removal(s: &mut Session, names: &[String]) {
    let (mut squared, mut guided) = (Vec::new(), Vec::new());
    for n in names.iter().filter(|n| named_layers(s, n) == 0) {
        let classes = || s.map().layers.iter().flat_map(|l| l.labels.in_order());
        let squares = |c: &rustgis_map::LabelClass| {
            c.placement == rustgis_map::LabelPlacement::Perpendicular
                && c.square_to.as_ref().is_some_and(|t| t.layer.eq_ignore_ascii_case(n))
        };
        if classes().any(squares) {
            squared.push(format!("'{n}'"));
        }
        if classes().any(|c| c.guide.as_ref().is_some_and(|g| g.layer.eq_ignore_ascii_case(n))) {
            guided.push(format!("'{n}'"));
        }
    }
    if !squared.is_empty() {
        let (count, names) = (squared.len(), squared.join(", "));
        let layers = if count == 1 { "line layer" } else { "line layers" };
        s.notify(
            format!("Labels: {count} perpendicular labels' {layers} {names} removed, so their labels are placed as auto until a layer of that name comes back"),
            false,
        );
    }
    if !guided.is_empty() {
        let (count, names) = (guided.len(), guided.join(", "));
        s.notify(
            format!("Labels: {count} guide layer(s) {names} removed, so the labels they guided are placed as without them until a layer of that name comes back"),
            false,
        );
    }
}

/// After layers called `names` are removed: a terrain drawn from one of them would lose its
/// relief, or take another layer's heights by that name, so it is turned off, and said. Its water
/// layer removed (#1915), the water keeps the name, so a layer of that name brings it back, and
/// its lakes lie on the ground meanwhile: said. So for its block's walls layer (#1914), the walls
/// one colour meanwhile.
fn terrain_follows_removal(s: &mut Session, names: &[String]) {
    let water = s.map().view.terrain.as_ref().and_then(|t| t.water.as_ref()).map(|w| w.layer.clone());
    if let Some(w) = water.filter(|w| names.iter().any(|n| n.eq_ignore_ascii_case(w)) && named_layers(s, w) == 0) {
        s.notify(
            format!("Water: its layer '{w}' was removed, so its lakes lie on the ground until a layer of that name comes back"),
            false,
        );
    }
    let dem = terrain_dem(s).filter(|d| names.iter().any(|n| n.eq_ignore_ascii_case(d)));
    // So does the block's walls layer (#1914): its walls are their own colour meanwhile. Not when
    // the DEM goes too: the terrain, its block with it, is turned off below.
    let walls = s.map().view.terrain.as_ref().and_then(|t| t.block.as_ref()).and_then(|b| b.walls_layer.clone());
    if let Some(w) = walls.filter(|w| dem.is_none() && names.iter().any(|n| n.eq_ignore_ascii_case(w)) && named_layers(s, w) == 0) {
        s.notify(
            format!(
                "Terrain: the block's walls layer '{w}' was removed, so the walls are one colour until a layer of that name comes back"
            ),
            false,
        );
    }
    // And the layer of the polygons it is cut to (#2318): no block is drawn meanwhile.
    let cut = s.map().view.terrain.as_ref().and_then(|t| t.block.as_ref()).and_then(|b| match &b.shape {
        rustgis_map::view::BlockShape::Polygon { layer, .. } => Some(layer.clone()),
        _ => None,
    });
    if let Some(c) = cut.filter(|c| dem.is_none() && names.iter().any(|n| n.eq_ignore_ascii_case(c)) && named_layers(s, c) == 0) {
        s.notify(
            format!("Terrain: the layer '{c}' the block is cut to was removed, so no block is drawn until a layer of that name comes back"),
            false,
        );
        super::terrain_blocks::said_removed(s, &c);
    }
    let Some(dem) = dem else { return };
    s.map_mut().view.terrain = None;
    let said = if named_layers(s, &dem) > 0 {
        format!(
            "Terrain turned off: one of the layers called '{dem}' was removed, so which one gives the heights can't be told; turn Terrain on again"
        )
    } else {
        format!("Terrain turned off: its DEM '{dem}' was removed")
    };
    s.notify(said, false);
}

fn remove(s: &mut Session, p: &Value) -> CmdResult {
    let ids = layer_ids(s, p)?;
    let m = s.map_mut();
    // The layer that takes the place of the topmost one removed becomes the active layer.
    let at = ids.iter().filter_map(|id| m.layer_index(*id)).min().ok_or("no such layer")?;
    // The tile layers drawn over them keep their place.
    m.release_tiles(&ids);
    let mut names = Vec::new();
    for id in ids {
        if let Some(i) = m.layer_index(id) {
            names.push(m.layers.remove(i).name);
            m.set_group(id, None);
        }
    }
    let next = m.layers.get(at.min(m.layers.len().saturating_sub(1))).map(|l| l.id);
    s.set_layer_selection(next.into_iter().collect(), next);
    terrain_follows_removal(s, &names);
    square_labels_follow_removal(s, &names);
    // A legend that left it out forgets it.
    for name in names {
        legends_follow_layer(s, &name, None);
    }
    Ok(Value::Null)
}

/// The most layers one command takes in a list.
const MAX_LAYER_LIST: usize = 10_000;

/// The layers a command acts on: `layer` as a name or id, or a list of them; else the layer
/// selection (the active layer alone when nothing else is picked).
pub(crate) fn layer_ids(input: &impl super::InputSource, p: &Value) -> Result<Vec<u32>, String> {
    let context = input.input_context();
    let s = &context;
    match p.get("layer") {
        Some(Value::Array(list)) => {
            if list.is_empty() {
                return Err("name at least one layer (\"layer\")".into());
            }
            if list.len() > MAX_LAYER_LIST {
                return Err(format!("at most {MAX_LAYER_LIST} layers at a time"));
            }
            let mut ids = Vec::new();
            for v in list {
                if !(v.is_number() || v.is_string()) {
                    return Err("each layer is a name or an id".into());
                }
                let id = s.layer_id(&json!({"layer": v}))?;
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            Ok(ids)
        }
        Some(_) => Ok(vec![s.layer_id(p)?]),
        None => {
            let ids = s.selected_layers();
            if ids.is_empty() { Err("select a layer in the Layers pane first".into()) } else { Ok(ids) }
        }
    }
}

/// `layer.select`: the layers picked in the Layers pane, replaced, toggled or extended.
fn select(s: &mut Session, p: &Value) -> CmdResult {
    let ids = match p.get("layer") {
        Some(Value::Array(_)) => layer_ids(s, p)?,
        _ => vec![s.layer_id(p)?],
    };
    match ps(p, "mode").unwrap_or("replace") {
        "replace" => s.set_layer_selection(ids, None),
        "extend" => {
            let mut picked = s.selected_layers();
            picked.extend(ids.iter().copied());
            s.set_layer_selection(picked, ids.last().copied());
        }
        "toggle" => {
            let mut picked = s.selected_layers();
            let mut active = s.active_layer;
            for id in ids {
                if let Some(at) = picked.iter().position(|x| *x == id) {
                    picked.remove(at);
                    if active == Some(id) {
                        active = picked.last().copied();
                    }
                } else {
                    picked.push(id);
                    active = Some(id);
                }
            }
            s.set_layer_selection(picked, active);
        }
        other => return Err(format!("unknown mode '{other}' (replace, toggle or extend)")),
    }
    Ok(Value::Null)
}

fn move_layer(s: &mut Session, p: &Value, d: i32) -> CmdResult {
    let id = s.layer_id(p)?;
    let m = s.map_mut();
    let i = m.layer_index(id).ok_or("no such layer")?;
    let j = i as i64 + d as i64;
    if j < 0 || j as usize >= m.layers.len() {
        return Err("the layer can't move further".into());
    }
    m.move_layer(i, j as usize)?;
    Ok(Value::Null)
}

/// Moves a layer to a position in the drawing order: 0 is the top, anything past the end the
/// bottom. One command (and one undo step) however far the layer travels; none when the layers
/// are already where they were asked to go, which a script setting the order can't know beforehand.
fn move_to(s: &mut Session, p: &Value) -> CmdResult {
    let ids = layer_ids(s, p)?;
    let to = uint(p, "to").ok_or("to is required: the position in the drawing order, 0 at the top")?;
    let before = s.map().layer_mark();
    move_layers(s, ids, to)?;
    // The order is compared, not each layer's index: a move within a group can end where it
    // started, and a layer already in place leaves the order as it was.
    if s.map().layer_mark() == before {
        s.request_only = true;
    }
    Ok(Value::Null)
}

fn move_layers(s: &mut Session, ids: Vec<u32>, to: u64) -> Result<(), String> {
    if let [id] = ids[..] {
        let m = s.map_mut();
        let to = usize::try_from(to).unwrap_or(usize::MAX).min(m.layers.len().saturating_sub(1));
        return move_to_index(m, id, to);
    }
    // Several: to the top or the bottom, keeping their order among themselves. A layer in a group
    // stays in it, going to the top or the bottom of its group.
    let m = s.map_mut();
    let len = m.layers.len();
    let to = usize::try_from(to).unwrap_or(usize::MAX);
    let top = match to {
        0 => true,
        t if t >= len.saturating_sub(ids.len()) => false,
        _ => return Err("several layers move only to the top (to 0) or the bottom".into()),
    };
    let mut ids = ids;
    ids.sort_by_key(|id| m.layer_index(*id));
    if !top {
        ids.reverse();
    }
    // The layers out of groups first: moving those within a group shifts no other layer.
    let (loose, grouped): (Vec<u32>, Vec<u32>) = ids.into_iter().partition(|id| m.group_of(*id).is_none());
    for (k, id) in loose.iter().enumerate() {
        let t = if top { k } else { len.saturating_sub(1 + k) };
        move_to_index(m, *id, t)?;
    }
    let mut placed: Vec<(u32, usize)> = Vec::new();
    for id in grouped {
        let Some(g) = m.group_of(id).map(|g| g.id) else { continue };
        let members = m.group_layers(g);
        let run: Vec<usize> = m.layers.iter().enumerate().filter(|(_, l)| members.contains(&l.id)).map(|(i, _)| i).collect();
        let (Some(&lo), Some(&hi)) = (run.first(), run.last()) else { continue };
        let k = placed.iter().filter(|(x, _)| *x == g).count();
        placed.push((g, k));
        let t = if top { lo + k } else { hi.saturating_sub(k) };
        move_to_index(m, id, t)?;
    }
    Ok(())
}

/// Moves a layer to index `t` of the drawing order, if it isn't there already.
fn move_to_index(m: &mut rustgis_map::Map, id: u32, t: usize) -> Result<(), String> {
    let i = m.layer_index(id).ok_or("no such layer")?;
    if i != t {
        m.move_layer(i, t)?;
    }
    Ok(())
}

fn symbology(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let kind = ps(p, "type").unwrap_or("single").to_ascii_lowercase();
    // The layer's own ramp: kept when no other is named and the type stays, and found by name even
    // after a custom ramp has been deleted from the project.
    let current = s.map().layer(id).and_then(|l| match (&l.renderer, kind.as_str()) {
        (Renderer::Graduated { ramp, .. }, "graduated" | "graduatedcolors")
        | (Renderer::Unique { ramp, .. }, "unique" | "uniquevalues") => Some(ramp.clone()),
        _ => None,
    });
    let ramp = match ps(p, "ramp") {
        Some(n) => Some(s.ramp(n).or_else(|e| current.filter(|r| r.name().eq_ignore_ascii_case(n.trim())).ok_or(e))?),
        None => current,
    };
    // Manual bounds, as typed; "breaks" alone means manual.
    let typed = match p.get("breaks") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(a.iter().map(|v| v.as_f64().ok_or("breaks are numbers")).collect::<Result<Vec<f64>, _>>()?),
        Some(_) => return Err("breaks are a list of numbers".into()),
    };
    let method = match ps(p, "method").and_then(ClassMethod::from_name) {
        Some(m) if m != ClassMethod::Manual && typed.is_some() => {
            return Err(format!("breaks go with the manual method, not {}", m.label()));
        }
        Some(m) => m,
        None if typed.is_some() => ClassMethod::Manual,
        None => ClassMethod::default(),
    };
    let classes = f(p, "classes").map(|c| c.clamp(1.0, 12.0) as usize).unwrap_or(5);
    let l = s.map().layer(id).ok_or("no such layer")?;
    let g = l.geometry_type();
    // A new symbology after No Fill draws fills again (#1301).
    let hidden = l.no_fill;
    let base = l.renderer.base_symbol();
    let base = if matches!(l.renderer, Renderer::Graduated { .. } | Renderer::Unique { .. }) {
        rustgis_map::Symbol { ..rustgis_map::Symbol::default_for(g, id) }.with_fill(base.fill, g)
    } else {
        base
    };
    let base = match hidden {
        Some(NoFill::Symbols) if base.fill.a() == 0 => rustgis_map::Symbol { fill: base.fill.with_alpha(shown_fill_alpha()), ..base },
        _ => base,
    };
    // The field it classes by, or classed by before an expression (kept to come back to).
    let last_field = match &l.renderer {
        Renderer::Unique { field, .. } => Some(field.as_str()),
        r => r.field(),
    }
    .filter(|f| l.dataset.field_index(f).is_some());
    let field = |numeric_only: bool| -> Result<String, String> {
        if let Some(x) = ps(p, "field") {
            return Ok(x.to_string());
        }
        last_field
            .filter(|name| {
                !numeric_only || l.dataset.field_index(name).and_then(|i| l.dataset.fields.get(i)).is_some_and(|f| f.ty.is_numeric())
            })
            .map(str::to_string)
            .or_else(|| l.dataset.fields.iter().find(|f| !numeric_only || f.ty.is_numeric()).map(|f| f.name.clone()))
            .ok_or_else(|| {
                if numeric_only {
                    "no numeric field is available for this symbology".into()
                } else {
                    "no field is available for this symbology".into()
                }
            })
    };
    // Classes and sizes describe the rows the definition query lets through, taken once: changing
    // the query later keeps them until the symbology is applied again.
    let rows = || -> Result<Vec<bool>, String> {
        let rows = l.visible_rows();
        if !rows.is_empty() && !rows.contains(&true) {
            return Err("no feature passes the definition query".into());
        }
        Ok(rows)
    };
    // Unique Values by expression: the one given, or with no field given, the layer's own.
    let expression = match p.get("expression") {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) if t.trim().is_empty() => None,
        Some(Value::String(t)) => Some(t.trim().to_string()),
        Some(_) => return Err("expression is text".into()),
    };
    if expression.is_some() && ps(p, "field").is_some() {
        return Err("give a field or an expression, not both".into());
    }
    if expression.is_some() && !matches!(kind.as_str(), "unique" | "uniquevalues") {
        return Err("only Unique Values class by an expression".into());
    }
    // Applied again by the layer's own expression, as many classes as it has (an imported rule
    // style's may pass the first MAX_UNIQUE values).
    let mut keep = rustgis_map::symbology::MAX_UNIQUE;
    let expression = match (p.get("expression"), p.get("field")) {
        (None, None) => {
            if let Renderer::Unique { classes, expression: Some(_), .. } = &l.renderer {
                keep = keep.max(classes.len()).min(rustgis_map::symbology::MAX_EXPRESSION_CLASSES);
            }
            l.renderer.expression().map(str::to_string)
        }
        _ => expression,
    };
    // Features past the first values an expression's classes keep.
    let mut rest = rustgis_data::value::PastClasses::default();
    let mut r = match kind.as_str() {
        "single" | "singlesymbol" => Renderer::Single(base),
        "unique" | "uniquevalues" => match &expression {
            Some(e) => {
                let (mut r, n) = Renderer::unique_by_up_to(&l.dataset, &rows()?, e, ramp.unwrap_or(Ramp::Harbour), base, keep)?;
                rest = n;
                // (Clearing the expression goes back to the field it had.)
                if let (Renderer::Unique { field, .. }, Some(last)) = (&mut r, last_field) {
                    *field = last.to_string();
                }
                r
            }
            None => {
                let (r, n) = Renderer::unique_counted(&l.dataset, &rows()?, &field(false)?, ramp.unwrap_or(Ramp::Harbour), base)?;
                rest = n;
                r
            }
        },
        "graduated" | "graduatedcolors" => {
            let (field, ramp) = (field(true)?, ramp.unwrap_or(Ramp::Ember));
            // Manual with no bounds keeps the layer's current ones on the same field, so switching
            // to it changes nothing.
            let kept = match &l.renderer {
                Renderer::Graduated { field: f, breaks, .. } if *f == field => {
                    let mut b = breaks.clone();
                    // On a layer read in place, the last bound closed the classes at the largest
                    // value read: the open class takes its place, so the classes keep their number.
                    let open = rustgis_map::symbology::OPEN_ABOVE;
                    if l.is_streamed() && b.last().is_some_and(|v| *v < open) {
                        b.pop();
                        if b.is_empty() {
                            b.push(open);
                        }
                    }
                    Some(b)
                }
                _ => None,
            };
            match (method, typed.or(kept)) {
                // A layer read in place holds only the features of the last view read, so its
                // range is unknown: the classes take every value up to the first bound and past
                // the last.
                (ClassMethod::Manual, Some(b)) if l.is_streamed() => Renderer::graduated_manual_open(&l.dataset, &field, &b, ramp, base)?,
                (ClassMethod::Manual, Some(b)) => Renderer::graduated_manual(&l.dataset, &rows()?, &field, &b, ramp, base)?,
                // Manual with nothing to keep starts from the view's classes, open at both ends.
                (ClassMethod::Manual, None) if l.is_streamed() => {
                    let r = Renderer::graduated(&l.dataset, &rows()?, &field, classes, method, ramp, base).map_err(|e| {
                        // The file has values; the view read so far holds none.
                        if e == format!("'{field}' has no numeric values") {
                            format!("'{field}' has no values in the view read so far: give manual breaks, or read a view where it has some")
                        } else {
                            e
                        }
                    })?;
                    r.opened().unwrap_or(r)
                }
                _ => Renderer::graduated(&l.dataset, &rows()?, &field, classes, method, ramp, base)?,
            }
        }
        "proportional" | "proportionalsymbols" => {
            if g != GeometryType::Point {
                return Err("Proportional Symbols needs a point layer".into());
            }
            Renderer::proportional(&l.dataset, &rows()?, &field(true)?, base)?
        }
        other => return Err(format!("unknown symbology '{other}'")),
    };
    // Applied again on the same field, the classes keep the labels set by hand, and the colours
    // and outlines set by hand too while the colour scheme stays (#314). Closed classes made open
    // on a layer read in place class every feature as before, so their edits carry over too.
    let opened = match &r {
        Renderer::Graduated { breaks, .. } if l.is_streamed() && breaks.last() == Some(&rustgis_map::symbology::OPEN_ABOVE) => {
            l.renderer.opened()
        }
        _ => None,
    };
    r.carry_edits_from(opened.as_ref().unwrap_or(&l.renderer), g);
    // The same field and scheme again carry the classes' alpha, which No Fill made 0.
    if hidden == Some(NoFill::Symbols) {
        r.map_symbols(show_fill);
    }
    let name = l.name.clone();
    // Classes are the values given with no map scale: one given only at some scales never is.
    let by_scale = r.expression().is_some_and(rustgis_map::qgis::reads_scale);
    // Where the values past the classes come from, and in which order the first are kept.
    let values = match &r {
        Renderer::Unique { expression: Some(_), .. } => "the expression gives".to_string(),
        Renderer::Unique { field, .. } => format!("of {field}"),
        _ => String::new(),
    };
    let order = if rest.by_number { "by number" } else { "in text order" };
    let mut dropped = None;
    let out = with(s, p, |l| {
        l.renderer = r;
        dropped = dropped_by_no_fill(l);
        take_no_fill(l);
    })?;
    notify_dropped(s, dropped);
    if rest.features > 0 {
        let rest = rest.features;
        s.notify(
            format!(
                "{name}: {rest} features whose value is past the first {keep} values {values} ({order}) are drawn as <all other values>"
            ),
            false,
        );
    }
    if by_scale {
        s.notify(
            format!(
                "{name}: its classes are the values the expression gives without a map scale; values it gives only at some scales, if any, are drawn as <all other values>"
            ),
            false,
        );
    }
    Ok(out)
}

/// The class `value`, `index` or `other: true` name in a command's parameters, or `None` when
/// none does.
fn class_ref(p: &Value) -> Result<Option<ClassRef>, String> {
    let other = b(p, "other").unwrap_or(false);
    let index = match p.get("index") {
        Some(_) => Some(uint(p, "index").ok_or("index must be a whole number from 0")?),
        None => None,
    };
    Ok(Some(match (p.get("value"), index, other) {
        (None, None, false) => return Ok(None),
        (Some(v), None, false) => ClassRef::Value(match v {
            Value::String(t) => t.clone(),
            // As the data prints it (the classes' own keys): `1000.0` is the class "1000".
            Value::Number(n) => match (n.as_i64(), n.as_f64()) {
                (Some(i), _) => rustgis_data::Value::Int(i).to_string(),
                (None, Some(x)) => rustgis_data::Value::Float(x).to_string(),
                _ => n.to_string(),
            },
            _ => return Err("value must be a text or a number".into()),
        }),
        (None, Some(i), false) => ClassRef::Index(usize::try_from(i).map_err(|_| "index is too large")?),
        (None, None, true) => ClassRef::Other,
        _ => return Err("name one class: value (Unique Values), index (Graduated Colors) or other: true".into()),
    }))
}

/// `layer.classStyle`: one class's colour, outline colour and legend label (#314).
fn class_style(s: &mut Session, p: &Value) -> CmdResult {
    let class = class_ref(p)?.ok_or("name one class: value (Unique Values), index (Graduated Colors) or other: true")?;
    let label = match p.get("label") {
        None => None,
        Some(Value::String(t)) => Some(t.clone()),
        Some(_) => return Err("label must be a text (an empty one goes back to the value or range)".into()),
    };
    // A colour given as something other than text is refused, not taken as absent.
    for k in ["color", "outline"] {
        if p.get(k).is_some_and(|v| !v.is_string()) {
            return Err(format!("{k} must be #RRGGBB or #RRGGBBAA"));
        }
    }
    let preserve_fill_alpha = ps(p, "color").is_some_and(|hex| hex.trim().trim_start_matches('#').len() == 6);
    let style = ClassStyle { color: s.get_color(p, "color")?, outline: s.get_color(p, "outline")?, label, preserve_fill_alpha };
    if style == ClassStyle::default() {
        return Err("nothing to change: give a color, an outline or a label".into());
    }
    let id = s.layer_id(p)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    let g = l.geometry_type();
    let e = l.renderer.set_class_style(&class, &style, g)?;
    // A fill colour chosen while No Fill hides the fills is the one Restore Fill shows. The saved
    // opacities are only this class's while they still match the symbols, as Restore Fill needs.
    let at = symbol_position(&l.renderer, &class).filter(|_| style.color.is_some());
    let mut now = 0;
    l.renderer.map_symbols(|_| now += 1);
    if let Some(a) = at.and_then(|i| l.hidden_fills.as_mut().filter(|h| h.alphas.len() == now)?.alphas.get_mut(i)) {
        *a = e.symbol.fill.a();
    }
    Ok(json!({"label": e.label, "color": e.symbol.fill.hex(), "outline": e.symbol.stroke.hex()}))
}

/// These loaders interpret `layer` inside an external file/database, never as a map layer.
/// The selector remains frozen with its path/address, including the loader's empty default.
pub(crate) fn external_dataset_selector(command: &str) -> bool {
    matches!(
        command,
        "layer.addData"
            | "layer.addDataBytes"
            | "layer.addDatabase"
            | "info.fileColumns"
            | "layer.addUrl"
            | "layer.addDownload"
            | "layer.addDataText"
            | "service.add"
            | "service.coverage"
            | "identify.featureInfo"
    )
}

#[cfg(test)]
mod tests {
    use crate::Session;
    use serde_json::json;

    /// A GDAL conversion's parts say their Z or M values once for each kind: a part alone in a
    /// fresh conversion's words, several counted together; parts without any say nothing.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_conversion_says_its_parts_z_and_m_once_for_each_kind() {
        let part = |name: &str, zm_dropped: u64, zm_unknown: bool| rustgis_data::source::gdal::Converted {
            path: std::path::PathBuf::new(),
            name: name.into(),
            layer: "roads".into(),
            family: None,
            unprojected: false,
            in_lonlat: false,
            zm_dropped,
            zm_unknown,
            zm_declared: false,
        };
        let (lost, unknown) = super::zm_notices("f.kml", &[part("roads (Point)", 2, false), part("roads (Line)", 3, false)]);
        assert_eq!(
            lost.as_deref(),
            Some(
                "f.kml: GDAL: 2 layers ('roads (Point)', 'roads (Line)') contain Z or M coordinates on 5 source feature(s); conversion keeps only XY coordinates"
            )
        );
        assert_eq!(unknown, None);
        let (lost, unknown) =
            super::zm_notices("f.kml", &[part("a", 1, false), part("b", 0, true), part("c", 0, true), part("d", 0, false)]);
        assert!(lost.is_some_and(|t| t.starts_with("f.kml: GDAL: 'a' contains Z or M coordinates on 1 source feature(s)")));
        assert!(unknown.is_some_and(|t| t.starts_with("f.kml: GDAL: 2 layers ('b', 'c'): an unknown number")));
        assert_eq!(super::zm_notices("f.kml", &[part("d", 0, false)]), (None, None));
        // A part whose count failed is taken as having lost them when its layer declares them.
        let declared = rustgis_data::source::gdal::Converted { zm_declared: true, ..part("e", 0, true) };
        let parts = super::gdal_parts(vec![declared, part("f", 0, true), part("g", 4, false)], "f.kml", None);
        assert_eq!(parts.iter().map(|p| p.zm_dropped).collect::<Vec<_>>(), [true, false, true]);
    }

    /// A payload can start beyond the sniff budget; only root keys decide the route.
    #[test]
    fn json_head_stops_before_payloads() {
        for head in [
            br#"{"type":"FeatureCollection","features":["#.as_slice(),
            br#"{"metadata":{"type":"CityJSON","CityObjects":{}},"type":"FeatureCollection""#,
        ] {
            assert!(!super::json_city_model_head(head));
        }
        for head in [
            br#"{"type":"CityJSON","CityObjects":{"#.as_slice(),
            br#"{"CityObjects":{"#,
            br#"{"features":["#,
            br#"{"metadata":{"description":"unfinished"#,
        ] {
            assert!(super::json_city_model_head(head));
        }
    }

    /// A CityJSON extension named features cannot decide the format before its root type.
    #[test]
    fn json_head_preserves_cityjson_extension_members() {
        let text = r#"{"features":[],"type":"CityJSON","CityObjects":{},"version":"2.0","vertices":[]}"#;
        assert!(rustgis_data::cityjson::looks_like_cityjson(text));
        assert!(super::json_city_model_head(text.as_bytes()));
        let large_extension = format!("{{\"features\":[\"{}", "x".repeat(64 << 10));
        assert!(super::json_city_model_head(large_extension.as_bytes().get(..64 << 10).unwrap()));
    }

    /// Listing GeoJSON never reads its full payload merely to rule out a city model.
    #[test]
    fn listing_json_layers_only_reads_the_head() {
        struct Dir(std::path::PathBuf);
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = Dir(std::env::temp_dir().join(format!("rustgis-json-head-{}", std::process::id())));
        std::fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("points.json");
        let mut bytes = br#"{"type":"FeatureCollection","features":[]}"#.to_vec();
        bytes.resize(128 << 10, b' ');
        std::fs::write(&path, bytes).unwrap();
        let capture = rustgis_data::provenance::Capture::start();
        let mut s = crate::Session::new(Default::default());
        let out = s.run("info.fileLayers", &serde_json::json!({"path":path})).unwrap();
        assert_eq!(out["layers"], serde_json::json!(["points"]));
        let inputs = capture.inputs();
        assert_eq!(inputs.len(), 1);
        assert!(inputs[0].sha256.is_none(), "a whole-file read was captured");
    }

    /// File-wide OSM omissions survive selecting polygons, inline, uploaded or read ahead.
    #[test]
    fn named_osm_layers_keep_omission_notices() {
        use base64::Engine;
        use rustgis_data::GeometryType;
        struct Dir(std::path::PathBuf);
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = Dir(std::env::temp_dir().join(format!("rustgis-osm-picked-notices-{}", std::process::id())));
        std::fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("named.osm.pbf");
        for (bytes, lossy) in [
            (&include_bytes!("../../testdata/reading-losses/hole-clean-with-point.osm.pbf")[..], false),
            (&include_bytes!("../../testdata/reading-losses/hole-missing-member-with-point.osm.pbf")[..], true),
        ] {
            let native = rustgis_data::osm::read("named.osm.pbf", bytes).unwrap();
            assert_eq!(native.len(), 2);
            let expected = &native[1].0;
            assert_eq!(expected.geometry_type, GeometryType::Polygon);
            std::fs::write(&path, bytes).unwrap();
            for picked in [false, true] {
                for route in ["ahead", "path", "bytes"] {
                    let mut s = Session::new(crate::Project::default());
                    let mark = s.notice_mark();
                    let mut params = if route == "bytes" {
                        json!({"fileName": "named.osm.pbf", "data": base64::engine::general_purpose::STANDARD.encode(bytes)})
                    } else {
                        json!({"path": path})
                    };
                    if picked {
                        params["layer"] = json!(expected.name);
                    }
                    match route {
                        "ahead" => {
                            let ahead = crate::cmd::reads::read_ahead(&mut s, "layer.addData", &params).unwrap();
                            assert!(ahead.reads());
                            let read = ahead.run(&rustgis_analysis::control::Control::new()).unwrap();
                            crate::cmd::reads::read_land(&mut s, read).unwrap();
                        }
                        "path" => {
                            s.run("layer.addData", &params).unwrap();
                        }
                        _ => {
                            s.run("layer.addDataBytes", &params).unwrap();
                        }
                    }
                    assert_eq!(s.map().layers.len(), if picked { 1 } else { 2 });
                    let actual = &s.map().layers.iter().find(|l| l.dataset.geometry_type == GeometryType::Polygon).unwrap().dataset;
                    assert_eq!(actual.fields, expected.fields);
                    assert_eq!(actual.features.iter().collect::<Vec<_>>(), expected.features.iter().collect::<Vec<_>>());
                    let said: Vec<_> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
                    let expected_notice =
                        if lossy { vec!["1 OSM relation member way was omitted because it is unavailable"] } else { vec![] };
                    assert_eq!(said, expected_notice, "route={route}, picked={picked}");
                }
            }
        }
    }

    /// Overlays left out for their pictures' size are said, with the budget; none, nothing said.
    #[test]
    fn overlays_left_out_for_their_pictures_are_said_and_none_otherwise() {
        let mut s = Session::new(crate::Project::default());
        let mark = s.notice_mark();
        assert!(super::overlays_drawn(&mut s, "a.kmz", (Vec::new(), 0, 0), false).is_empty());
        assert!(s.notices_since(mark).is_empty(), "nothing left out, nothing said");
        let mark = s.notice_mark();
        super::overlays_drawn(&mut s, "a.kmz", (Vec::new(), 0, 2), false);
        let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert_eq!(
            said,
            ["a.kmz: 2 ground overlays left out: their pictures don't fit beside the archive in the 256 MiB a KMZ is expanded within"]
        );
    }

    /// Refuses a sparse KML before reading its bytes, with the same advice when listing its layers.
    #[test]
    fn oversized_kml_reports_limit_and_alternatives() {
        let dir = std::env::temp_dir().join(format!("rustgis-kml-limit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("permits.kml");
        // A KML is read as it is added, on the UI thread: within the floor, whatever the memory.
        let limit = rustgis_data::io::BYTES_FLOOR;
        std::fs::File::create(&path).unwrap().set_len(crate::cmd::raster::MAX_UPLOAD_BYTES as u64 + 1).unwrap();
        let params = json!({"path": path});
        let path = path.to_str().unwrap();
        let mut session = Session::new(crate::Project::default());
        let check = |error: String| {
            let limit = (limit >> 20).to_string();
            assert_eq!(
                rustgis_data::io::file_size_error_args(&error),
                Some((path, limit.as_str(), rustgis_data::io::FileSizeAdvice::Vector)),
                "{error}"
            );
            assert!(error.contains("Split it into smaller files") && error.contains("FlatGeobuf"), "{error}");
        };
        for command in ["layer.addData", "info.fileLayers"] {
            check(session.run(command, &params).unwrap_err());
        }
        #[cfg(not(target_arch = "wasm32"))]
        check(super::read_layers_file(&params, path).err().unwrap());
        assert!(session.map().layers.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A URL a web document restricted to public addresses stays restricted when a later request
    /// comes from a local origin: its blocking download goes through the public agent, which
    /// refuses a local address before connecting (#2055).
    #[test]
    fn a_public_restriction_holds_for_a_blocking_download_from_a_local_origin() {
        let url = "http://127.0.0.1:1/rustgis-restricted-download.dat";
        crate::render::rustgis_tiles::cache::restrict_to_public(url);
        let mut s = Session::new(crate::sample::world_project());
        let e = s
            .run("layer.addUrl", &json!({"url": url, "from": "http://127.0.0.1:9/doc", "fromDocument": false}))
            .expect_err("a public restriction must refuse the local address");
        assert!(e.contains("local network"), "{e}");
    }
    #[test]
    fn false_document_flag_cannot_widen_a_public_origin() {
        let params = json!({"from": "https://catalog.example/item", "fromDocument": false});
        let (origin, public) = super::request_origin(&params).unwrap();
        assert!(public);
        assert_eq!(super::origin_ceiling(origin, public), crate::render::rustgis_tiles::address::Reach::Public);
    }
    #[test]
    fn observed_local_asset_reach_cannot_be_granted_by_json() {
        use crate::render::rustgis_tiles::address::{Origin, Reach};
        let mut s = Session::new(crate::Project::default());
        let from = "https://catalog.corp.example/item.json";
        let asset = "http://10.0.0.5/scene.geojson";
        let p = json!({"url":asset,"from":from,"fromDocument":false});
        assert!(super::request_origin_for(&s, &p).unwrap().1);
        let origin = Origin::connected(from, "10.0.0.5".parse().unwrap()).unwrap();
        s.remember_document_asset(asset, origin).unwrap();
        let (origin, public) = super::request_origin_for(&s, &p).unwrap();
        assert!(!public);
        assert_eq!(super::origin_ceiling(origin, public), Reach::Local);
        let other = json!({"url":"http://10.0.0.6/other.geojson","from":from,"fromDocument":false});
        assert!(super::request_origin_for(&s, &other).unwrap().1);
        let public = json!({"url":asset,"from":from,"fromDocument":true});
        assert!(super::request_origin_for(&s, &public).unwrap().1);
    }
    #[test]
    fn observed_asset_allowances_do_not_cross_project_generations() {
        use crate::render::rustgis_tiles::address::Origin;
        let mut s = Session::new(crate::Project::default());
        let from = "https://project-scoped.corp.example/item.json";
        let asset = "http://10.0.0.5/project-scoped.geojson";
        let p = json!({"url":asset,"from":from,"fromDocument":false});
        let origin = Origin::connected(from, "10.0.0.5".parse().unwrap()).unwrap();
        s.remember_document_asset(asset, origin.clone()).unwrap();
        s.remember_document_asset("http://10.0.0.5/another.geojson", origin.clone()).unwrap();
        assert!(!super::request_origin_for(&s, &p).unwrap().1);
        s.run("project.new", &json!({})).unwrap();
        assert!(super::request_origin_for(&s, &p).unwrap().1);
        let normalized = super::asset_request_params(&mut s, &p).unwrap().into_owned();
        assert!(normalized.get("anonymous").is_none(), "stale registrations must not change normalization");
        assert!(s.run_leaving_downloads("layer.addUrl", &p).unwrap_err().contains("local network"));
        assert!(s.take_url_requests().is_empty());
        s.remember_document_asset(asset, origin).unwrap();
        assert_eq!(s.document_assets.0, s.opened);
        assert_eq!(s.document_assets.1.len(), 1, "a new project's registration releases the previous project's table and budget");
        assert!(!super::request_origin_for(&s, &p).unwrap().1);
    }

    #[test]
    fn registered_assets_accept_only_an_explicit_live_native_login() {
        use crate::render::rustgis_tiles::address::Origin;
        let mut s = Session::new(crate::Project::default());
        let from = "https://catalog.example/item.json";
        let asset = "https://assets.example/scene.geojson";
        s.remember_document_asset(asset, Origin::named(from)).unwrap();
        let spaced = json!({"url":format!("  {asset}  "),"from":from});
        assert!(super::request_origin_for(&s, &spaced).is_ok());
        let connection = s
            .run("connection.new", &json!({"kind":"ogcapi","address":"https://assets.example","auth":"header","keyName":"X-Key"}))
            .unwrap();
        s.run("connection.login", &json!({"id":connection["id"],"apiKey":"synthetic-registered-asset"})).unwrap();
        let plain = json!({"url":asset,"from":from,"connection":connection["id"]});
        assert!(super::request_origin_for(&s, &plain).is_ok());
        let mut implicit = plain.clone();
        implicit.as_object_mut().unwrap().remove("connection");
        assert!(super::request_origin_for(&s, &implicit).is_ok());
        let anonymous = super::asset_request_params(&mut s, &implicit).unwrap().into_owned();
        assert_eq!(anonymous["url"], implicit["url"], "normalization cannot invent a login selection");
        assert!(anonymous.get("connection").is_none());
        assert!(super::request_origin_for(&s, &anonymous).is_ok());
        let bound = super::asset_request_params(&mut s, &plain).unwrap().into_owned();
        assert_ne!(bound["url"], plain["url"]);
        assert!(super::request_origin_for(&s, &bound).is_ok());
        let mut unselected = bound.clone();
        unselected.as_object_mut().unwrap().remove("connection");
        assert!(super::request_origin_for(&s, &unselected).is_err());
        unselected["connection"] = json!("another-connection");
        assert!(super::request_origin_for(&s, &unselected).is_err());
        s.run("connection.logout", &json!({"id":connection["id"]})).unwrap();
        assert!(super::request_origin_for(&s, &bound).is_err());
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod geodatabase_tests {
    use super::LiveTable;
    use rustgis_data::source::geodatabase::{Item, ItemKind, Versioning};
    use rustgis_data::source::sqlserver::{GdbListing, Table};

    fn class(name: &str, shape: Option<&str>) -> Item {
        Item {
            name: format!("utility.gis.{name}"),
            schema: "gis".into(),
            table: name.to_uppercase(),
            kind: ItemKind::FeatureClass,
            dataset: None,
            alias: Some(format!("{name} (alias)")),
            shape_column: shape.map(str::to_string),
            field_aliases: Vec::new(),
            versioning: Versioning::Unversioned,
            definition_ok: true,
            family: None,
            archived: false,
            rowid_column: None,
            srid: None,
        }
    }

    fn spatial(table: &str, column: &str, several: bool) -> Table {
        Table {
            schema: "gis".into(),
            name: table.into(),
            column: column.into(),
            kind: "geometry".into(),
            current: false,
            several,
            alias: None,
            dataset: None,
            short: None,
        }
    }

    /// A class named with its column, as a table of several is labelled, is still the class.
    #[test]
    fn a_class_named_with_its_column_keeps_its_alias() {
        assert_eq!(super::split_column("gis.MAINS:SHAPE"), ("gis.MAINS", Some("SHAPE")));
        assert_eq!(super::split_column("gis.MAINS"), ("gis.MAINS", None));
        assert_eq!(super::split_column("gis.MAINS:"), ("gis.MAINS:", None));
        let item = class("Mains", Some("SHAPE"));
        let tables = [spatial("MAINS", "EXTRA", true), spatial("MAINS", "SHAPE", true)];
        for (column, expected) in [("shape", "SHAPE"), ("EXTRA", "EXTRA")] {
            let t = super::class_table(&item, &tables, Some(column)).unwrap();
            assert_eq!((t.column.as_str(), t.alias.as_deref(), t.short.as_deref()), (expected, Some("Mains (alias)"), Some("Mains")));
        }
        let e = super::class_table(&item, &tables, Some("NOPE")).unwrap_err().to_string();
        assert!(e.contains("utility.gis.Mains has no geometry or geography column 'NOPE'"), "{e}");
    }

    /// A class read from a column other than its registered shape column says so; one read from
    /// its shape column, or that registers none, says nothing.
    #[test]
    fn a_class_read_from_another_column_says_so() {
        let item = class("Mains", Some("SHAPE"));
        let t = super::class_table(&item, &[spatial("MAINS", "EXTRA", false)], None).unwrap();
        assert_eq!(t.column, "EXTRA");
        let entry = super::fallback(&item, &t);
        assert_eq!(entry.as_deref(), Some("utility.gis.Mains (EXTRA, not SHAPE)"));
        assert_eq!(super::fallback(&item, &spatial("MAINS", "shape", false)), None);
        assert_eq!(super::fallback(&class("Mains", None), &spatial("MAINS", "EXTRA", false)), None);
        // One notice for all of them, with a count, naming the first few.
        assert_eq!(super::fallbacks_note(&[]), None);
        assert_eq!(
            super::fallbacks_note(&entry.into_iter().collect::<Vec<_>>()).as_deref(),
            Some(
                "1 feature class is read from a spatial column other than the shape column it registers, which isn't a geometry or geography column: utility.gis.Mains (EXTRA, not SHAPE)"
            )
        );
        let many: Vec<String> = (0..7).map(|n| format!("C{n} (EXTRA, not SHAPE)")).collect();
        let note = super::fallbacks_note(&many).unwrap();
        assert!(note.starts_with("7 feature classes are read from a spatial column other than the shape column they register"), "{note}");
        assert!(note.ends_with(": C0 (EXTRA, not SHAPE), C1 (EXTRA, not SHAPE), C2 (EXTRA, not SHAPE), C3 (EXTRA, not SHAPE), C4 (EXTRA, not SHAPE) and 2 more"), "{note}");
    }

    /// What a geodatabase add leaves is part of the error when the add fails (a failed command's
    /// notices are withdrawn), and an error with nothing left is unchanged.
    #[test]
    fn a_failed_geodatabase_add_still_says_what_it_left() {
        assert_eq!(super::with_left("db: no tables with a geometry column".into(), &[]), "db: no tables with a geometry column");
        let left = [super::unadded_note(3), "1 feature class was left out: SDEBINARY: Orphans".to_string()];
        assert_eq!(
            super::with_left("db: no tables with a geometry column".into(), &left),
            "db: no tables with a geometry column; 3 tables not added; add one by name with \"layer\"; 1 feature class was left out: SDEBINARY: Orphans"
        );
    }

    /// A column asked for by name is the one wanted: only the class's own choice of another
    /// column than its shape column is said.
    #[test]
    fn a_column_asked_for_by_name_is_not_called_a_fallback() {
        let item = class("Mains", Some("SHAPE"));
        let extra = spatial("MAINS", "EXTRA", true);
        assert_eq!(super::named_fallback("gis.MAINS:EXTRA", &item, &extra), None);
        assert_eq!(super::named_fallback("Mains", &item, &extra).as_deref(), Some("utility.gis.Mains (EXTRA, not SHAPE)"));
        assert_eq!(super::named_fallback("Mains", &item, &spatial("MAINS", "SHAPE", true)), None);
    }

    /// A registered table added says how many rows it read, not none.
    #[test]
    fn a_registered_table_added_says_its_rows() {
        let result = super::table_result(&serde_json::json!(7), 50_000);
        assert_eq!(result["features"], 50_000);
        assert_eq!(result["tables"], serde_json::json!([7]));
        assert!(result["layer"].is_null() && result["layers"].as_array().is_some_and(Vec::is_empty));
    }

    /// A registered table's row limit is sized for the memory there is, within its bounds.
    #[test]
    fn a_registered_table_is_read_within_a_planned_row_limit() {
        let rows = super::table_rows();
        assert!((super::TABLE_ROWS_FLOOR..=rustgis_data::source::sqlserver::MAX_EDITABLE).contains(&rows), "{rows}");
    }

    /// Adding a whole geodatabase adds its readable classes and other spatial tables, counts its
    /// registered tables and names the classes it can't read; a clean one names none.
    #[test]
    fn a_whole_geodatabase_names_the_classes_it_leaves_out() {
        let table = Item { kind: ItemKind::Table, ..class("Readings", None) };
        let listing = GdbListing {
            datasets: Vec::new(),
            classes: vec![
                (class("Mains", Some("SHAPE")), Some(spatial("MAINS", "SHAPE", false))),
                (class("Orphans", Some("SHAPE")), None),
                (class("Valves", Some("SHAPE")), Some(spatial("VALVES", "EXTRA", false))),
            ],
            tables: vec![table],
            other: vec![spatial("PARCELS", "geom", false)],
            cut: false,
        };
        let (tables, left) = super::geodatabase_layers(listing);
        assert_eq!(tables.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["MAINS", "VALVES", "PARCELS"]);
        assert_eq!(left.tables, 1);
        assert_eq!(left.unread, ["utility.gis.Orphans"]);
        assert_eq!(left.fallbacks.len(), 1);
        assert_eq!(
            super::unread_note(&left.unread, false).as_deref(),
            Some(
                "1 feature class was left out: stored in a format RustGIS doesn't read (SDEBINARY or another non-native storage): utility.gis.Orphans"
            )
        );
        let many: Vec<String> = (0..7).map(|n| format!("C{n}")).collect();
        let note = super::unread_note(&many, false).unwrap();
        assert!(note.starts_with("7 feature classes were left out") && note.ends_with(": C0, C1, C2, C3, C4 and 2 more"), "{note}");

        let clean = GdbListing {
            datasets: Vec::new(),
            classes: vec![(class("Mains", Some("SHAPE")), Some(spatial("MAINS", "SHAPE", false)))],
            tables: Vec::new(),
            other: Vec::new(),
            cut: false,
        };
        let (tables, left) = super::geodatabase_layers(clean);
        assert_eq!(tables.len(), 1);
        assert!(left.unread.is_empty() && left.fallbacks.is_empty() && left.tables == 0 && left.cut.is_none());
        assert_eq!(super::unread_note(&left.unread, false), None);
    }

    /// When the database's list of spatial columns was cut, a class without one isn't called
    /// unreadable storage: the cut is said, by a whole add and by an add by name.
    #[test]
    fn a_cut_list_of_spatial_columns_is_said_not_taken_for_storage() {
        let listing = GdbListing {
            datasets: Vec::new(),
            classes: vec![(class("Orphans", Some("SHAPE")), None)],
            tables: Vec::new(),
            other: Vec::new(),
            cut: true,
        };
        let (_, left) = super::geodatabase_layers(listing);
        let cut = left.cut.clone().unwrap_or_default();
        assert!(cut.contains("only the first 10000 were read"), "{cut}");
        let note = super::unread_note(&left.unread, left.cut.is_some()).unwrap();
        assert!(!note.contains("SDEBINARY") && note.contains("no spatial column was found for it among those read"), "{note}");

        let mut full: Vec<Table> = (0..10_000).map(|n| spatial(&format!("T{n}"), "geom", false)).collect();
        let e = super::class_table(&class("Mains", Some("SHAPE")), &full, None).unwrap_err().to_string();
        assert!(!e.contains("SDEBINARY") && e.contains("only the first 10000 were read"), "{e}");
        // The cut may fall between the table's columns: another column isn't read in place of a
        // shape column that may be past the cut. One asked for by name still is, and a class
        // that registers no shape column still reads its table's first.
        full.push(spatial("MAINS", "EXTRA", false));
        let e = super::class_table(&class("Mains", Some("SHAPE")), &full, None).unwrap_err().to_string();
        assert!(e.contains("only the first 10000 were read"), "{e}");
        assert_eq!(super::class_table(&class("Mains", Some("SHAPE")), &full, Some("EXTRA")).unwrap().column, "EXTRA");
        assert_eq!(super::class_table(&class("Mains", None), &full, None).unwrap().column, "EXTRA");
    }

    /// A geodatabase class names its layer by its alias, else by its short name; a table the
    /// catalog doesn't register, by its label.
    #[test]
    fn geodatabase_class_layer_name_is_its_alias() {
        let table = |alias: Option<&str>, short: Option<&str>| Table {
            schema: "gis".into(),
            name: "MAINS".into(),
            column: "SHAPE".into(),
            kind: "geometry".into(),
            current: false,
            several: false,
            alias: alias.map(str::to_string),
            dataset: None,
            short: short.map(str::to_string),
        };
        assert_eq!(LiveTable::Sqlserver(table(Some("Water Mains"), Some("Mains"))).display(), "Water Mains");
        assert_eq!(LiveTable::Sqlserver(table(None, Some("Mains"))).display(), "Mains");
        assert_eq!(LiveTable::Sqlserver(table(None, None)).display(), "gis.MAINS");
    }

    /// A registered class reads its shape column, named as the catalog names it; one whose table
    /// has no native spatial column is refused, naming the storage.
    #[test]
    fn a_class_without_a_native_column_is_refused_by_its_storage() {
        use rustgis_data::source::geodatabase::{Item, ItemKind, Versioning};
        let item = Item {
            name: "utility.gis.Mains".into(),
            schema: "gis".into(),
            table: "MAINS".into(),
            kind: ItemKind::FeatureClass,
            dataset: Some("Water".into()),
            alias: None,
            shape_column: Some("SHAPE".into()),
            field_aliases: Vec::new(),
            versioning: Versioning::Unversioned,
            definition_ok: true,
            family: None,
            archived: false,
            rowid_column: None,
            srid: None,
        };
        let column = |column: &str| Table {
            schema: "gis".into(),
            name: "MAINS".into(),
            column: column.into(),
            kind: "geometry".into(),
            current: false,
            several: true,
            alias: None,
            dataset: None,
            short: None,
        };
        let t = super::class_table(&item, &[column("EXTRA"), column("SHAPE")], None).unwrap();
        assert_eq!((t.column.as_str(), t.short.as_deref(), t.dataset.as_deref()), ("SHAPE", Some("Mains"), Some("Water")));
        assert_eq!(LiveTable::Sqlserver(t).display(), "Mains");
        let e = super::class_table(&item, &[], None).unwrap_err().to_string();
        assert!(e.contains("utility.gis.Mains is stored in a format RustGIS doesn't read (SDEBINARY"), "{e}");
    }
}
