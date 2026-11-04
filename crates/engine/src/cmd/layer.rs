//! Home › Add (Add Data), Layers pane actions, and the Feature Layer / Labeling / Data tabs.

use std::path::Path;

use rustgis_data::{GeometryType, photo};
use rustgis_map::symbology::{ClassRef, ClassStyle, LABEL_SIZE, MAX_STROKES};
use rustgis_map::{
    ClassMethod, Glyph, LineCap, LineJoin, LineMark, LineMarkers, LineStroke, Marker, NoFill, Placements, Ramp, Renderer, Rgba,
};
use serde_json::{Value, json};

use super::{b, f, need_feature_layer, need_layer, ps, uint};
use crate::{CmdResult, CommandSpec as C, Session};

/// Registers layer commands shared by the UI, CLI and command search.
pub fn commands() -> Vec<C> {
    vec![
        C::new("layer.addData", "Add Data", "Home › Add", add_data)
            .params(r#"{"path": string, "name"?: string, "layer"?: "GeoPackage, GDAL, DuckDB or DXF layer", "paperSpace"?: "bool: a DXF's layout entities too", "stream"?: bool, "as"?: "scan" (a TIFF, PNG or JPEG, even a photo or a GeoTIFF, as a picture with no coordinates), "crs"?: "EPSG:… (or WKT) for Shapefile coordinates (overrides .prj), a CSV, a CityJSON model, or a GDAL or DuckDB file that doesn't say", "x"?: "CSV x/longitude column", "y"?: "CSV y/latitude column", "wkt"?: "CSV WKT geometry column", "delimiter"?: ","|";"|"\\t"|"|"}"#),
        C::new("layer.addPhotos", "Add Photos", "Home › Add", add_photos).params(
            r#"{"paths"?: [JPEG photo files], "folder"?: "a folder of photos (every JPEG in it)", "name"?: "the layer (default Photos; photos join it when it exists)"}"#,
        ),
        C::new("info.fileLayers", "List Layers in File", "Home › Add › Add Data", file_layers)
            .view()
            .params(r#"{"path": string, "crs"?: "EPSG:… for a .city.json that doesn't say (as layer.addData)"}"#),
        C::new("info.fileColumns", "List Columns in File", "Home › Add › Add Data › Delimited Text", file_columns).view().params(
            r#"{"path": "a delimited text file (CSV, TSV, TXT) or a spreadsheet", "delimiter"?: ","|";"|"\\t"|"|", "layer"?: "a spreadsheet's sheet (the first by default)"}"#,
        ),
        C::new("layer.addSql", "Add Data From SQL", "Home › Add", add_sql).params(
            r#"{"sql": "one DuckDB SELECT; DuckDB Spatial's ST_ functions, read_parquet('…'), web addresses", "name"?: string, "database"?: "a .duckdb file or a PostgreSQL connection (postgres:host=… dbname=…) whose tables the query names", "password"?: "the connection's, kept for this session only", "crs"?: "EPSG:… of a geometry that doesn't say", "stream"?: bool, "ran"?: "a result the app ran (the SQL pane)"}"#,
        ),
        C::new("layer.addDatabase", "Add Data From Database", "Home › Add › Add Data", add_database).params(
            r#"{"database": "postgres:host=… dbname=… user=… (or a postgresql:// address; PostGIS tables are read live), a mysql://user@host:port/database address (MySQL or MariaDB, read live; mariadb:// too), an mssql://user@host:1433/database address (SQL Server or Azure SQL, read live; ?encrypt=strict|false, ?trust-server-certificate=true for a self-signed certificate, ?instance=NAME; on Windows DOMAIN\\user or no user signs in with Windows), or a .duckdb file", "password"?: "kept for this session only, never saved", "layer"?: "a table, as table or schema.table (table:column for one of several geometry columns; default: every table with a geometry column)", "crs"?: "EPSG:… of a geometry that doesn't say", "name"?: string, "stream"?: bool}"#,
        ),
        C::new("info.databaseTables", "List Database Tables", "Home › Add › Add Data › Database", database_tables)
            .view()
            .params(r#"{"database": "as layer.addDatabase", "password"?: string}"#),
        C::new("layer.defineProjection", "Define Projection", "Layers", define_projection)
            .when(need_feature_layer)
            .params(r#"{"layer"?: name|id, "crs": "EPSG:… or WKT; reinterpret native coordinates, without rewriting the source file"}"#),
        C::new("layer.loadAll", "Load All Features", "Layers", load_all).when(need_streamed).params(r#"{"layer"?: name|id}"#),
        C::new("layer.addUrl", "Add Data From URL", "Home › Add › Add Data", add_url).params(
            r#"{"url": "https://…/data.geojson|.csv|.parquet|.fgb|.gpkg|.zip|.shp (its .dbf fetched beside it, and its .shx, .prj and .cpg when there)|.kml|.kmz|.gpx|.tif|.jp2 (JPEG 2000: desktop, through GDAL) (the format from the address, else the contents) | a .pmtiles archive", "name"?: text, "layer"?: "one layer of a zip, KML or GPX", "zoom"?: "bool: true frames everything added; false keeps the view, even on an empty map; omitted, the first content on an empty map is framed (a GeoParquet, FlatGeobuf or cloud-optimised GeoTIFF read in place likewise; a PMTiles or MBTiles archive ignores zoom and is framed on its bounds unless it covers the whole world)", "x"?, "y"?, "wkt"?, "crs"?, "delimiter"?, "paperSpace"?, "stream"?, "as"? (as layer.addData), "whole"?: "bool: download a cloud-optimised GeoTIFF whole (as a raster layer) instead of reading it in place (an address without a file name is read in place when its first bytes are a cloud-optimised GeoTIFF)", "cog"?: "bool: the address is a cloud-optimised GeoTIFF whatever its name says (the app's probe found it so)", "bbox"?: "[west, south, east, north] in degrees | "view": a cloud-optimised GeoTIFF's window as a raster layer (desktop and command line), read by range at full resolution, in the file's coordinate system, with its colour table and class names; at most 64 million values (all bands)", "cellSize"?: "with bbox: the coarsest overview whose cells are at most this size is read (the file's units; metres for longitude/latitude, a degree taken as 111.32 km)", "start"?, "end"?, "unit"?, "interval"?, "dates"?: "a GeoTIFF address with date placeholders ({date:YYYY}, {YYYY}{MM}{DD}), one file per date: the dates it spans and steps through (as time.set), turning the time slider on"}"#,
        ),
        C::new("layer.addDownload", "Add Downloaded Data", "Home › Add", add_download)
            .params(r#"{"id": a download the app finished (Home › Add › Add Data › From URL), "url": its address, "name"?: text, "layer"?: text, "zoom"?: bool, "x"?, "y"?, "wkt"?, "crs"?, "delimiter"?, "paperSpace"?, "stream"?, "as"? (as layer.addData), "service"?: bool (a service's features: their axes (when they came as GeoJSON) and paging are checked, and the address is kept as the layer's source), "bounds"?: [west, south, east, north] (with service: the layer's longitude/latitude bounds from its capabilities, which its axes are checked against; without bounds they are left as they come; ignored without service), "coverage"?: bool (a WCS coverage: a GeoTIFF, else the service's error, and its request kept as the layer's source), "geolibreProject"?: bool (internal: downloaded project JSON), "geolibreLayer"?: {"token", "record"} (internal: reserved imported layer and its saved style), "geolibreZarr"?: bool (internal: an imported Zarr slice as GeoTIFF)}"#),
        C::new("layer.addEncodedPolyline", "Add Encoded Polyline", "Home › Add", add_encoded_polyline).params(
            r#"{"polyline": "encoded polyline text" | [texts] (one line each), "precision"?: 5 (Google) | 6 (OSRM, Valhalla), "name"?: text}"#,
        ),
        C::new("layer.addDataText", "Add Data (from text)", "Home › Add", add_data_text).params(r#"{"fileName": "name.geojson|.csv|.tsv", "text": string, "name"?: text, "x"?, "y"?, "wkt"?, "crs"?, "delimiter"? (as layer.addData, for delimited text), "layer"?, "paperSpace"? (as layer.addData, for a file of several layers)}"#),
        C::new("layer.addSample", "Add Sample Data", "Home › Add › Add Data", add_sample).params(r#"{"name": "countries"|"cities"|"rivers"|"lakes"}"#),
        C::new("layer.remove", "Remove", "Layers", remove)
            .key("Delete")
            .when(need_layer)
            .params(r#"{"layer"?: name|id | [name|id] (default: the layer selection)}"#),
        C::new("layer.select", "Select Layer", "Layers", select)
            .params(r#"{"layer"?: name|id | [name|id] (default: the active layer; the last of a list becomes the active layer), "mode"?: "replace" (only these; the default) | "toggle" (each in or out of the layer selection; the last one out leaves no active layer) | "extend" (added to it)}"#)
            .view(),
        C::new("layer.rename", "Rename", "Layers", |s, p| {
            let n = s_req(p, "name")?.trim().chars().take(200).collect::<String>();
            if n.is_empty() {
                return Err("name is required".into());
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
            // Layers masked by this one follow the new name.
            for l in &mut s.map_mut().layers {
                if l.mask.as_deref().is_some_and(|m| m.eq_ignore_ascii_case(&old)) {
                    l.mask = Some(n.clone());
                }
            }
            // So do legends that leave it out.
            legends_follow_layer(s, &old, Some(&n));
            Ok(r)
        }).params(r#"{"layer"?, "name": string}"#)
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
        }).params(r#"{"layer"?: name|id | [name|id] (default: the layer selection), "visible"?: bool (toggles when omitted: all take the opposite of the active layer's state)}"#)
        .when(need_layer),
        C::new("layer.moveUp", "Move Up", "Layers", |s, p| move_layer(s, p, -1)).params(r#"{"layer"?: name|id (default: the active layer)}"#).when(need_layer),
        C::new("layer.moveDown", "Move Down", "Layers", |s, p| move_layer(s, p, 1)).params(r#"{"layer"?: name|id (default: the active layer)}"#).when(need_layer),
        C::new("layer.move", "Move", "Layers", move_to)
            .when(need_layer)
            .params(r#"{"layer"?: name|id | [name|id] (default: the layer selection), "to": position (0 = top; past the end, the bottom). Several keep their order, the topmost landing at "to" as one layer would, and go only to the top or the bottom (one in a group, to its group's): with 4 layers and 2 of them, 2 or more is the bottom, 1 is refused}"#),
        C::new("layer.zoomTo", "Zoom To Layer", "Layers", |s, p| {
            let ids = layer_ids(s, p)?;
            let e = ids.iter().filter_map(|id| s.map().layer(*id)).fold(rustgis_geom::Extent::EMPTY, |e, l| e.union(&l.shown_extent()));
            if e.is_empty() {
                return Err(if ids.len() > 1 { "the layers have no features" } else { "the layer has no features" }.into());
            }
            s.zoom_to(&e);
            Ok(Value::Null)
        }).params(r#"{"layer"?: name|id | [name|id] (default: the layer selection; zooms to all of them)}"#)
        .view()
        .when(need_layer),
        C::new("layer.symbology", "Symbology", "Style › Drawing", symbology)
            .when(need_feature_layer)
            .params(r#"{"layer"?, "type": "single"|"unique"|"graduated"|"proportional", "field"?, "expression"?: text|null (unique: class by this expression's values instead of a field; blank or null: by a field; with neither key, Unique Values keep the layer's expression), "classes"?: 1..12, "method"?: "naturalBreaks"|"equalInterval"|"quantile"|"geometricInterval"|"standardDeviation"|"arithmeticProgression"|"headTailBreaks"|"manual", "breaks"?: [numbers] (manual: the classes' upper bounds, closed by the field's largest value), "ramp"?: string}"#),
        C::new("layer.ramp", "Color Scheme", "Style › Drawing", |s, p| {
            let r = s.ramp(s_req(p, "ramp")?)?;
            with(s, p, |l| match &mut l.raster {
                Some(rl) => rl.style.set_ramp(r),
                None => {
                    let g = l.geometry_type();
                    l.renderer.apply_ramp(r, g);
                    // New colours after No Fill are drawn (#1301).
                    if take_no_fill(l) == Some(NoFill::Symbols) {
                        l.renderer.map_symbols(show_fill);
                    }
                }
            })
        }).params(r#"{"layer"?, "ramp": colour ramp name}"#)
        .when(need_layer),
        C::new("layer.classStyle", "Class Style", "Style › Drawing", class_style)
            .when(need_feature_layer)
            .params(
                r##"{"layer"?, "value"?: text|number (a Unique Values class), "index"?: number (a Graduated Colors class, 0 = the lowest), "other"?: true (Unique Values' <all other values>), "color"?: "#RRGGBB" (preserves polygon translucency, minimum alpha 150)|"#RRGGBBAA" (sets alpha exactly), "outline"?: "#RRGGBB"|"#RRGGBBAA", "label"?: text (the legend label; "" goes back to the value or range)}"##,
            ),
        C::new("layer.color", "Symbol Color", "Style › Drawing", |s, p| {
            let hex = s_req(p, "color")?;
            let c = Rgba::from_hex(hex).ok_or("color must be #RRGGBB or #RRGGBBAA")?;
            // `#RRGGBBAA` sets the fill's alpha exactly; `#RRGGBB` keeps the layer's translucency.
            let explicit_alpha = hex.trim().trim_start_matches('#').len() == 8;
            with(s, p, |l| {
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
                take_no_fill(l);
            })
        })
        .when(need_feature_layer)
        .params(r##"{"layer"?, "color": "#RRGGBB"|"#RRGGBBAA" (the fill; a polygon's outline is left as it is, set it with layer.outline)}"##),
        C::new("layer.noFill", "No Fill", "Style › Drawing", |s, p| {
            with(s, p, |l| {
                if l.source.is_some() || l.has_geometry(GeometryType::Point) || l.has_geometry(GeometryType::Polyline) {
                    // Symbols are shared by geometry families. A polygon-only fill override
                    // leaves point markers and line strokes (including their markers) intact.
                    // A streamed layer may have other families outside the loaded window.
                    l.no_fill = Some(NoFill::Override);
                    l.fill_layers = vec![no_fill_override()];
                    match &mut l.renderer {
                        Renderer::Unique { class_fills, other_fills, .. } => {
                            class_fills.clear();
                            other_fills.clear();
                        }
                        Renderer::Graduated { class_fills, .. } => class_fills.clear(),
                        _ => {}
                    }
                } else {
                    l.renderer.map_symbols(|sym| sym.fill = sym.fill.with_alpha(0));
                    l.no_fill = Some(NoFill::Symbols);
                }
            })
        })
        .when(need_polygon_features)
        .params(r#"{"layer"?: name|id (default: the active layer containing polygons; hides polygon fills, keeping outlines and point/line symbols)}"#),
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
        })
        .when(need_feature_layer)
        .params(r##"{"layer"?, "color"?: "#RRGGBB"|"#RRGGBBAA", "width"?: number}"##),
        C::new("layer.lineStrokes", "Line Strokes", "Style › Drawing", line_strokes)
            .when(need_lines_or_points)
            .params(
                r##"{"layer"?, "strokes": [{"color"?: "#RRGGBB"|"#RRGGBBAA"|null (null: the renderer's line colour), "width"?: points|null (null: the renderer's width), "offset"?: points (positive: right of the line's direction, negative: left), "cap"?: "round"|"flat"|"square", "join"?: "round"|"miter"|"bevel", "level"?: 0..255|null (with layer.strokeLevels: the level it draws at; null: its place in the stack), "dash"?: [points, …]|null (dash, gap, … in turn, at most 8; null: solid), "markers"?: {"mark"?: "circle"|"square"|"triangle"|"diamond"|"star"|"character"|"hash" (a mark across the line), "char"?: text (a character of a font as the marker instead of a shape), "font"?: text (the installed family it is drawn from; none: the label font), "size"?: points (a marker's size, a hash mark's length; default 4), "at"?: "interval"|"vertices"|"innerVertices"|"first"|"last"|"middle"|"segmentMiddles" or a list of them (default "interval"), "interval"?: points (default 12), "offsetAlong"?: points (from the start, back from the end, on from the middle), "rotate"?: bool (turned with the line; default true), "averageAngle"?: points (the stretch of line a marker at an interval turns with; 0: its segment), "angle"?: degrees clockwise (a hash mark at 0 crosses the line square), "outline"?: "#RRGGBB"|"#RRGGBBAA" (the markers' outline, as wide as the stroke)}|null (markers or hash marks along the line in the stroke's colour, instead of a line; cap, join and dash don't apply)}] (drawn bottom first; [] goes back to the single stroke, or for a class to the layer's strokes; on a point layer they are its marks: each stroke's markers drawn at the point in place of the symbol's, "offsetAlong" shifting a mark right and "offset" down, both turned with its "angle", and "rotate" turning it by layer.markerRotation's field), "value"?: text|number (a Unique Values class's own strokes), "index"?: number (a Graduated Colors class's, 0 = the lowest), "other"?: true (Unique Values' <all other values>)}"##,
            ),
        C::new("layer.fillLayers", "Fill Layers", "Style › Drawing", fill_layers)
            .when(need_polygon_features)
            .params(
                r##"{"layer"?, "value"?|"index"?|"other"?: a class of Unique Values (its value, or other: true) or Graduated Colors (its index) to set its own fills (none: the layer's), "fills": [{"color"?: "#RRGGBB"|"#RRGGBBAA"|null (null: the feature's fill colour, so the fills follow the classes), "pattern"?: "solid" (default)|"hatch" (parallel lines)|"dots" (markers on a grid)|"gradient" (from color to color2 across the feature)|"shapeburst" (from color at the edge to color2 inside), "angle"?: degrees anticlockwise from east (hatch), "spacing"?: points between lines (hatch, 1 to 500; default 8) or points across and down between markers, a number or [across, down] (dots), "width"?: a hatch line's width in points (0 to 50; default 1), "offset"?: points across the lines (hatch) or [across, down] (dots), "marker"?: "circle"|"square"|"triangle"|"diamond"|"star" (dots), "char"?: text (dots: a character of a font as the marker instead of a shape), "font"?: text (the installed family it is drawn from; none: the label font), "size"?: a marker's size in points (default 3), "outline"?: a marker's outline colour or null, "outlineWidth"?: points, "displacement"?: [across, down] points every other row and column are shifted by (dots), "color2"?: "#RRGGBB"|"#RRGGBBAA" (gradient, shapeburst: the second colour; default white), "stops"?: [[0..1, "#RRGGBB"|"#RRGGBBAA"], …] (gradient, shapeburst: colours between the two, at most 16), "discrete"?: bool (each colour held until the next stop), "shape"?: "linear" (default)|"radial"|"conical" (gradient), "from"?: [across, down]|"centroid" (gradient: where color is, 0..1 in the feature's box; default [0.5, 0], the top middle), "to"?: [across, down]|"centroid" (where color2 is; default [0.5, 1]), "spread"?: "pad" (default)|"reflect"|"repeat" (gradient: past its points), "angle"?: degrees anticlockwise (gradient: turned round its box's middle), "inView"?: bool (gradient: from and to in the map view's box, not the feature's), "distance"?: points|null (shapeburst: how deep it shades; null or none: to the point deepest inside), "blur"?: 0..17 (shapeburst: pixels), "ignoreRings"?: bool (shapeburst: holes' edges left out)}] (bottom first, at most 8; [] for the symbol's single fill; the outline is drawn over them)}"##,
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
        })
        .when(need_points)
        .params(r#"{"layer"?, "field": numeric field name|null (each point's marker turned by the angle in it, in degrees; null: not turned), "counterclockwise"?: bool (the angles run counterclockwise, as in mathematics; default clockwise, as on a compass)}"#),
        C::new("layer.symbolBy", "Symbol by Expression", "Style › Drawing", symbol_by)
            .when(need_feature_layer)
            .params(r#"{"layer"?, "size"?: expression|null (a point's marker size in points), "fill"?: expression|null (the fill colour: '#RRGGBB', a CSS colour name or 'r,g,b[,a]' as color_rgb gives it), "stroke"?: expression|null (the outline colour, a line's colour), "strokeWidth"?: expression|null (the outline's width in points, a line's width), "angle"?: expression|null (point layers: each marker turned by it, in degrees clockwise, in place of layer.markerRotation's field)} (each expression in the language of queries, per feature, @map_scale the map's scale; a key left out keeps its expression, null or "" removes it; where an expression gives null the symbol's own value is drawn)"#),
        C::new("layer.strokeLevels", "Stroke Levels", "Style › Drawing", |s, p| {
            let on = b(p, "on").ok_or("on is true or false")?;
            let id = s.layer_id(p)?;
            let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
            if l.geometry_type() != GeometryType::Polyline || l.graphics {
                return Err("this works on line layers".into());
            }
            l.stroke_levels = on;
            Ok(json!({"on": on}))
        })
        .when(need_lines)
        .params(r#"{"layer"?, "on": bool (draw every feature's stacked strokes of one level before the next, so casings join under the cores)}"#),
        C::new("layer.marker", "Marker", "Style › Drawing", |s, p| {
            let shape = ps(p, "shape").map(|n| Marker::ALL.into_iter().find(|m| m.label().eq_ignore_ascii_case(n)).ok_or("unknown marker shape")).transpose()?;
            let size = f(p, "size").map(|v| v.clamp(1.0, 80.0) as f32);
            let (ch, font) = (character(p)?, ps(p, "font"));
            if shape.is_some() && ch.is_some() {
                return Err("give a shape or a char, not both".into());
            }
            with(s, p, |l| {
                l.renderer.map_symbols(|sym| {
                    if let Some(m) = shape {
                        sym.marker = m;
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
        })
        .when(need_feature_layer)
        .params(r#"{"layer"?, "shape"?: "circle"|"square"|"triangle"|"diamond"|"star", "char"?: text (a character of a font as the marker, its first; sized by the font's em), "font"?: text (the installed family the character is drawn from; "" or one not installed: the label font), "size"?: number}"#),
        C::new("layer.transparency", "Transparency", "Style › Effects", |s, p| {
            let t = f(p, "value").ok_or("value (0–100) is required")?.clamp(0.0, 100.0) as f32;
            with(s, p, |l| l.transparency = t)
        })
        .when(need_layer)
        .params(r#"{"layer"?, "value": 0..100}"#),
        C::new("layer.scaleRange", "Visibility Range", "Style › Visibility Range", |s, p| {
            let min = f(p, "min").filter(|v| *v > 0.0);
            let max = f(p, "max").filter(|v| *v > 0.0);
            with(s, p, |l| {
                l.min_scale = min;
                l.max_scale = max;
            })
        })
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
        })
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
        }).params(r#"{"layer"?: name|id | [name|id] (default: the layer selection), "on"?: bool (toggles when omitted: all take the opposite of the active layer's state)}"#)
        .when(need_feature_layer),
        C::new("layer.expand", "Expand Legend", "Layers", |s, p| {
            let on = b(p, "on");
            with(s, p, |l| l.expanded = on.unwrap_or(!l.expanded))
        }).params(r#"{"layer"?, "on"?: bool (toggles when omitted)}"#)
        .view()
        .when(need_layer),
        C::new("labels.enable", "Label", "Labeling › Labels", |s, p| {
            let on = b(p, "on");
            with(s, p, |l| l.labels.enabled = on.unwrap_or(!l.labels.enabled))
        })
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
        }).params(r#"{"layer"?, "class"?: name (none: the Default class, by whatever name it has), "field": name}"#)
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
        })
        .when(need_feature_layer)
        .params(r##"{"layer"?, "class"?: name (none: the Default class, by whatever name it has), "size"?: number, "color"?: "#RRGGBB"|"#RRGGBBAA", "halo"?: bool, "haloColor"?: "#RRGGBB"|"#RRGGBBAA"|"" (""/null: white or dark, whichever stands out from the text), "haloWidth"?: points 0-10|null (null: the thin halo), "spacing"?: points between letters (-5 to 50), "bold"?: bool, "italic"?: bool, "font"?: installed family name (labels.fonts lists them; "" is the label font), "background"?: false|null (none) | true (white, a thin dark outline) | {"fill"?: "#RRGGBB"|"#RRGGBBAA"|"" ("": none), "stroke"?: colour|"" (the outline; "": none), "strokeWidth"?: points 0-10, "padding"?: [across, up and down] points 0-50, "radius"?: corner radius in points 0-50} (a rectangle behind the text; what isn't given is kept, or the default's), "wrapChar"?: text (a line break wherever it occurs, left out; a space puts each word on its own line; "" for none), "wrapLength"?: characters (a longest line, broken between words; 0 for none; at most 500), "lineAlign"?: "left"|"center"|"right" (how a label's lines line up)}"##),
        C::new("layer.definitionQuery", "Definition Query", "Data › Definition Query", |s, p| {
            let q = ps(p, "where").map(str::trim).unwrap_or("").to_string();
            let id = s.layer_id(p)?;
            if !q.is_empty()
                && let Some(l) = s.map().layer(id)
            {
                rustgis_data::query::parse(&q, &l.dataset)?;
            }
            with(s, p, |l| l.definition_query = if q.is_empty() { None } else { Some(q.clone()) })
        })
        .when(need_feature_layer)
        .params(r#"{"layer"?, "where": "SQL clause, empty to clear"}"#),
        C::new("table.open", "Attribute Table", "Data › Table", |s, p| {
            s.table_layer = Some(s.layer_id(p)?);
            Ok(Value::Null)
        }).params(r#"{"layer"?: name|id (default: the active layer)}"#)
        .view()
        .key("Ctrl+T")
        .when(need_feature_layer),
        C::new("table.close", "Close Table", "Data › Table", |s, _| {
            s.table_layer = None;
            Ok(Value::Null)
        })
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
        })
        .params(r#"{"table": name|id (a standalone table; of tables sharing a name, the name is the first)} (the old position, "at", is refused)"#),
        C::new("layer.fields", "Fields", "Data › Data Design", |s, p| {
            let id = s.layer_id(p)?;
            let l = s.map().layer(id).ok_or("no layer")?;
            Ok(json!(l.dataset.fields.iter().map(|f| json!({"name": f.name, "type": f.ty.label(), "alias": f.alias})).collect::<Vec<_>>()))
        }).params(r#"{"layer"?: name|id (default: the active layer)}"#)
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
    let mut strokes = Vec::with_capacity(list.len());
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
        let markers = match o.get("markers") {
            None | Some(Value::Null) => None,
            Some(m) => Some(line_markers(m).map_err(|e| format!("stroke {}: markers: {e}", i + 1))?),
        };
        strokes.push(
            LineStroke { color, width: num("width")?, offset: num("offset")?.unwrap_or(0.0), cap, join, level, dash, markers }.clamped(),
        );
    }
    let id = s.layer_id(p)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    let points = l.geometry_type() == GeometryType::Point;
    if !(points || l.geometry_type() == GeometryType::Polyline) || l.graphics {
        return Err("this works on line and point layers".into());
    }
    // A point draws marks only: a line stroke would draw nothing there.
    if let Some(i) = strokes.iter().position(|st| st.markers.is_none()).filter(|_| points) {
        return Err(format!("stroke {}: a point layer's strokes are markers", i + 1));
    }
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

/// Turns No Fill off, dropping its override if it laid one, and says how it was on. The caller
/// shows the symbols' fills on the other path, where only it knows which.
fn take_no_fill(l: &mut rustgis_map::Layer) -> Option<NoFill> {
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
fn show_fill(sym: &mut rustgis_map::Symbol) {
    if sym.fill.a() == 0 {
        sym.fill = sym.fill.with_alpha(shown_fill_alpha());
    }
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
    let mut fills = Vec::with_capacity(list.len());
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
        let pattern = match o.get("pattern").filter(|v| !v.is_null()).map(|v| v.as_str()) {
            None | Some(Some("solid")) => FillPattern::Solid,
            Some(Some("hatch")) => FillPattern::Hatch {
                angle: num("angle", 0.0)?,
                spacing: num("spacing", 8.0)?,
                width: num("width", 1.0)?,
                offset: num("offset", 0.0)?,
            },
            Some(Some("dots")) => {
                let marker = match o.get("marker") {
                    None | Some(Value::Null) => Marker::Circle,
                    Some(Value::String(m)) => Marker::ALL
                        .into_iter()
                        .find(|s| s.label().eq_ignore_ascii_case(m.trim()))
                        .ok_or_else(|| format!("fill {n}: unknown marker '{m}'"))?,
                    Some(_) => return Err(format!("fill {n}: marker is \"circle\", \"square\", \"triangle\", \"diamond\" or \"star\"")),
                };
                // A character of a font instead of a shape, as `layer.marker` takes it.
                let marker = match character(v).map_err(|e| format!("fill {n}: {e}"))? {
                    Some(_) if o.get("marker").is_some_and(|m| !m.is_null()) => {
                        return Err(format!("fill {n}: give a marker or a char, not both"));
                    }
                    Some(c) => Marker::Char(Glyph::new(c, o.get("font").and_then(Value::as_str).unwrap_or_default())),
                    None => marker,
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
    let id = s.layer_id(p)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    if !l.has_geometry(GeometryType::Polygon) || l.graphics || l.is_raster() {
        return Err("this works on layers containing polygons".into());
    }
    // A fill in the symbol's colour, or the symbol's own fill ([]), asked for after No Fill: the
    // symbols' fills are shown again, or they would draw nothing (#1301). A class's fills show
    // that class's alone.
    let in_symbol_colour = fills.is_empty() || fills.iter().any(|f| f.color.is_none());
    let hidden = l.no_fill;
    let count = fills.len();
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
                l.no_fill = None;
            }
            // Fills set here replace No Fill's own, even transparent ones.
            if hidden == Some(NoFill::Override) {
                l.no_fill = None;
            }
            l.fill_layers = fills;
        }
    }
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

/// A stroke's `markers`: what it draws along the line instead of a line, and where.
fn line_markers(v: &Value) -> Result<LineMarkers, String> {
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
    let mark = match o.get("mark") {
        None | Some(Value::Null) => d.mark,
        Some(Value::String(m)) if m.trim().eq_ignore_ascii_case("hash") => LineMark::Hash,
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
        Some(_) => return Err("mark is \"circle\", \"square\", \"triangle\", \"diamond\", \"star\", \"character\" or \"hash\"".into()),
    };
    // A character of a font instead of a shape.
    let mark = match (character(v)?, o.get("font")) {
        (Some(_), _) if mark == LineMark::Hash => return Err("hash marks take no char".into()),
        // As `layer.marker`: a shape named with a char is a mistake, not the char.
        (Some(_), _) if o.get("mark").and_then(Value::as_str).is_some_and(|m| !m.trim().eq_ignore_ascii_case("character")) => {
            return Err("give a shape or a char, not both".into());
        }
        (Some(c), font) => LineMark::Marker(Marker::Char(Glyph::new(c, font.and_then(Value::as_str).unwrap_or_default()))),
        (None, _) => mark,
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
        s.notify(m.clone(), false);
    }
    if ds.features.is_empty() {
        return Err(report.messages.first().cloned().unwrap_or_else(|| "no photo with a GPS position".into()));
    }
    // The reader took no more than there is room for; `take` keeps that so if it ever changes.
    let added = ds.features.len().min(room);
    let extent =
        ds.features.iter().take(added).filter_map(|f| f.geometry.as_ref()).fold(rustgis_geom::Extent::EMPTY, |e, g| e.union(&g.extent()));
    let id = match existing {
        Some(id) => {
            let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
            for f in ds.features.into_iter().take(added) {
                l.dataset.push(f.geometry, f.attrs);
            }
            l.touch_geometry();
            s.active_layer = Some(id);
            id
        }
        None => add_dataset(s, ds),
    };
    if zoom.unwrap_or(empty_before) {
        s.zoom_to(&extent);
    }
    Ok(json!({"layer": id, "photos": added, "skipped": report.skipped}))
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
                    Ok(_) => match std::fs::read(path) {
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
            None => s.notify(format!("{path}: {table} not read: {why} (GDAL, when installed, reads it)"), true),
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
fn add_data(s: &mut Session, p: &Value) -> CmdResult {
    let path = s_req(p, "path")?;
    let path = &*gdb_path(path);
    // `"as": "scan"` opens a TIFF, PNG or JPEG as a picture with no coordinates, a photo too.
    let scan = super::raster::wants_scan(p, path)?;
    // A photo: a point in the photo layer (one without a GPS position is named and left out). In the
    // browser `add_photos` asks for an upload, so a JPEG isn't taken for a scan there either.
    if !scan && rustgis_data::photo::is_photo_name(path) {
        let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or(PHOTOS).to_string();
        return add_photos(s, &json!({"paths": [path], "name": name}));
    }
    if rustgis_data::raster::is_raster_name(path) {
        return super::raster::add_path(s, path, ps(p, "name").map(str::trim).filter(|n| !n.is_empty()), scan);
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
    // A CityJSON model named .json, in the coordinate system given or with a layer picked: as a
    // .city.json, all its layers.
    if path.to_ascii_lowercase().ends_with(".json")
        && ["crs", "layer"].iter().any(|k| ps(p, k).is_some_and(|c| !c.trim().is_empty()))
        && std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= rustgis_data::io::MAX_BYTES)
    {
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        if rustgis_data::io::is_cityjson(path, &bytes) {
            let full = std::path::absolute(path).map(|p| p.display().to_string()).unwrap_or_else(|_| path.to_string());
            return add_layers(s, p, &full, &bytes, None);
        }
    }
    // A large GeoJSON FeatureCollection (any, with `"stream": true`) is read in place, streamed
    // when it holds many features; another GeoJSON document, or one that can't be read so, is
    // read whole below.
    #[cfg(not(target_arch = "wasm32"))]
    if is_geojson(path)
        && !rustgis_data::cityjson::is_cityjson_name(path)
        && b(p, "stream").unwrap_or_else(|| {
            std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > rustgis_data::source::geojson::IN_PLACE_ABOVE)
        })
    {
        let name = Path::new(path).file_stem().and_then(|n| n.to_str()).unwrap_or("layer");
        match rustgis_data::source::geojson::GeoJsonSource::open_path(Path::new(path), name) {
            Ok(src) => return add_source_with(s, p, Origin::Path(path), Some(Box::new(src)), None),
            // Not a FeatureCollection: read whole below.
            Err(rustgis_data::DataError::Unsupported(_)) => {}
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
        let crs = rustgis_data::raster::Crs::parse(crs)?;
        let name = Path::new(path).file_stem().and_then(|n| n.to_str()).unwrap_or("layer");
        let stream = b(p, "stream").unwrap_or_else(|| {
            rustgis_data::source::shp::record_count(Path::new(path)).is_some_and(|n| n > rustgis_map::source::STREAM_ABOVE)
        });
        if stream {
            let src = rustgis_data::source::shp::ShpSource::open_path_in(Path::new(path), name, Some(crs)).map_err(|e| e.to_string())?;
            return add_source_with(s, p, Origin::Path(path), Some(Box::new(src)), None);
        }
        let mut read = rustgis_data::io::read_shapefile_in(Path::new(path), name, Some(crs)).map_err(|e| e.to_string())?;
        read.0.source = Some(path.to_string());
        return add_read(s, p, read, None);
    }
    // A large Shapefile is read in place, as a GeoPackage is; smaller ones are loaded whole.
    #[cfg(not(target_arch = "wasm32"))]
    if path.to_ascii_lowercase().ends_with(".shp")
        && b(p, "stream").unwrap_or_else(|| {
            rustgis_data::source::shp::record_count(Path::new(path)).is_some_and(|n| n > rustgis_map::source::STREAM_ABOVE)
        })
    {
        return add_source(s, p, Origin::Path(path), None);
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
        let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        if meta.len() > super::raster::MAX_UPLOAD_BYTES as u64 {
            return Err(format!("{path} is too large to open"));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        // Read from disk: the table keeps the workbook's path, so an export can't replace it.
        let mut read = read_sheet(p, path, &bytes)?;
        read.0.source = Some(std::path::absolute(path).map_or_else(|_| path.to_string(), |a| a.display().to_string()));
        return add_read(s, p, read, None);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if large_osm(path) {
        return add_gdal(s, p, path);
    }
    if rustgis_data::io::is_multi_layer_name(path) {
        let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        if meta.len() > super::raster::MAX_UPLOAD_BYTES as u64 {
            return Err(format!("{path} is too large to open"));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let full = std::path::absolute(path).map(|p| p.display().to_string()).unwrap_or_else(|_| path.to_string());
        return add_layers(s, p, &full, &bytes, None);
    }
    let delimited = read_delimited(p, path, || {
        let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        // The limit the text readers keep without options too.
        if meta.len() > rustgis_data::io::MAX_BYTES {
            return Err(format!("{path} is too large to open"));
        }
        std::fs::read(path).map_err(|e| format!("{path}: {e}"))
    })?;
    let read = match delimited {
        Some(read) => read,
        None => rustgis_data::io::read_path(Path::new(path)).map_err(|e| format!("{path}: {e}"))?,
    };
    add_read(s, p, read, None)
}

/// Adds every layer of a file that holds several: the Shapefiles (and other files) of a zip, a KML
/// or KMZ's points, lines and polygons drawn in its styles, a GPX's waypoints, routes, tracks and
/// track points. `"layer"` picks one by name; a name given applies to the first.
pub(crate) fn add_layers(s: &mut Session, p: &Value, file_name: &str, bytes: &[u8], zoom: Option<bool>) -> CmdResult {
    // A LandXML file in a coordinate system it doesn't say.
    let crs = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty());
    let landxml = rustgis_data::landxml::is_landxml_name(file_name)
        || (file_name.to_ascii_lowercase().ends_with(".xml")
            && rustgis_data::landxml::looks_like_landxml(&String::from_utf8_lossy(bytes.get(..bytes.len().min(4096)).unwrap_or(bytes))));
    let lower = file_name.to_ascii_lowercase();
    let kml = lower.ends_with(".kml") || lower.ends_with(".kmz");
    // A KML's ground overlays (pictures) are raster layers, added after its placemarks.
    let overlays = if kml && ps(p, "layer").is_none() { ground_overlays(s, file_name, bytes) } else { Vec::new() };
    // A DXF drawing: its coordinate system (it never says) and one of its own layers.
    let dxf = rustgis_data::dxf::is_dxf_name(file_name);
    let mut reads = match crs.filter(|_| landxml) {
        _ if lower.ends_with(".zip") && crs.is_some() => {
            let crs = crs.map(rustgis_data::raster::Crs::parse).transpose()?;
            rustgis_data::io::read_zip_in(file_name, bytes, crs)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|(dataset, report)| rustgis_data::io::LayerRead { dataset, report, styles: Vec::new() })
                .collect()
        }
        // A CityJSON model that names no coordinate system, or names one it doesn't have.
        _ if crs.is_some() && rustgis_data::io::is_cityjson(file_name, bytes) => {
            let crs = crs.map(rustgis_data::raster::Crs::parse).transpose()?;
            rustgis_data::cityjson::read(file_name, bytes, crs)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|(mut dataset, report)| {
                    dataset.source.get_or_insert_with(|| file_name.to_string());
                    rustgis_data::io::LayerRead { dataset, report, styles: Vec::new() }
                })
                .collect()
        }
        _ if dxf => {
            let opts = rustgis_data::dxf::Options {
                crs: crs.map(rustgis_data::raster::Crs::parse).transpose()?,
                layer: ps(p, "layer").map(str::trim).filter(|l| !l.is_empty()),
                paper_space: b(p, "paperSpace").unwrap_or(false),
            };
            rustgis_data::dxf::read(file_name, bytes, &opts)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|(dataset, report)| rustgis_data::io::LayerRead { dataset, report, styles: Vec::new() })
                .collect()
        }
        Some(c) => {
            let crs = rustgis_data::raster::Crs::parse(c)?;
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
    if let Some(want) = ps(p, "layer").map(str::trim).filter(|t| !t.is_empty())
        && !dxf
    {
        let names: Vec<String> = reads.iter().map(|r| r.dataset.name.clone()).collect();
        // By its full name or its last word ("tracks" for "walk tracks").
        let pick = |n: &str| n.eq_ignore_ascii_case(want) || n.rsplit(' ').next().is_some_and(|w| w.eq_ignore_ascii_case(want));
        // A CityJSON model's first layer holds what the file as a whole says, after its own one
        // notice: that its heights were dropped (it has no other). In a zip, a model is the one
        // member that gives several layers, and it says so for its own layers only.
        let whole = rustgis_data::io::is_cityjson(file_name, bytes);
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
                said.messages.drain(..usize::from(r.dataset.zm_dropped).min(said.messages.len()));
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
/// and said, the others are still added. A name given goes to the first layer added.
fn add_reads(
    s: &mut Session,
    p: &Value,
    file_name: &str,
    reads: Vec<rustgis_data::io::LayerRead>,
    overlays: Vec<(String, rustgis_data::raster::Raster)>,
    zoom: Option<bool>,
) -> CmdResult {
    let mut added = Vec::new();
    let mut failed = Vec::new();
    let mut q = p.clone();
    for read in reads {
        let name = read.dataset.name.clone();
        let styles = read.styles;
        // A layer that can't be added is skipped and said; the others are still added. A name
        // given goes to the first layer actually added.
        match add_read(s, &q, (read.dataset, read.report), zoom) {
            Ok(r) => {
                if let Some(id) = r.get("layer").and_then(Value::as_u64).and_then(|i| u32::try_from(i).ok()) {
                    apply_styles(s, id, &styles);
                }
                added.push(r);
                if let Some(o) = q.as_object_mut() {
                    o.remove("name");
                }
            }
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    for (name, raster) in overlays {
        match super::raster::add_overlay(s, &name, raster, zoom) {
            Ok(r) => added.push(r),
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
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
fn ground_overlays(s: &mut Session, file_name: &str, bytes: &[u8]) -> Vec<(String, rustgis_data::raster::Raster)> {
    let (found, undrawn) = match rustgis_data::kml::ground_overlays(file_name, bytes) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut notes = Vec::new();
    let dir = Path::new(file_name).parent().filter(|d| d.is_absolute());
    for (g, picture) in found {
        let href = g.href.trim();
        let picture = picture.or_else(|| {
            // Beside the document: no web addresses, absolute paths or climbing out of its folder.
            let rel = Path::new(href);
            let safe = !href.contains("://")
                    && rel.components().next().is_some()
                    // Only names (no root, drive prefix or `..`), so it stays in the folder on
                    // Windows too (`C:x`, `\\x`).
                    && rel.components().all(|c| matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir));
            if !safe {
                return None;
            }
            let p = dir?.join(rel);
            crate::project::read_file(&p, (256 << 20) - 1).ok()
        });
        let Some(picture) = picture else {
            notes.push(format!("{} (its picture {href} isn't in the file or beside it)", g.name));
            continue;
        };
        match rustgis_data::raster::from_image(&g.name, &picture, g.bbox, g.rotation) {
            Ok(r) => out.push((g.name.clone(), r)),
            Err(e) => notes.push(format!("{} ({e})", g.name)),
        }
    }
    if undrawn > 0 {
        notes.push(format!("{undrawn} ground overlays without a picture or a box (LatLonBox)"));
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
/// layers, or the one layer of other files. `layer.addData` takes a name with `"layer"`.
fn file_layers(_s: &mut Session, p: &Value) -> CmdResult {
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
            return Err(format!("{path} is too large to open"));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
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
        if meta.len() > rustgis_data::io::MAX_BYTES {
            return Err(format!("{path} is too large to open"));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let layers = rustgis_data::dxf::layers(path, &bytes).map_err(err)?;
        return Ok(json!({"count": layers.len(), "layers": layers}));
    }
    // A CityJSON model named .json: all its layers, as Add Data adds them by name.
    let crs = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty());
    let model_json = path.to_ascii_lowercase().ends_with(".json")
        && std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= rustgis_data::io::MAX_BYTES);
    if rustgis_data::io::is_multi_layer_name(path) || model_json {
        let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        if meta.len() > super::raster::MAX_UPLOAD_BYTES as u64 {
            return Err(format!("{path} is too large to open"));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let zip = path.to_ascii_lowercase().ends_with(".zip");
        let layers: Vec<String> = match crs.filter(|_| zip || rustgis_data::io::is_cityjson(path, &bytes)) {
            Some(crs) => {
                let crs = Some(rustgis_data::raster::Crs::parse(crs)?);
                let read =
                    if zip { rustgis_data::io::read_zip_in(path, &bytes, crs) } else { rustgis_data::cityjson::read(path, &bytes, crs) };
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
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Postgis(&'a rustgis_map::source::PostgisOrigin),
    /// A MySQL or MariaDB table, always opened by the caller (desktop only).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Mysql(&'a rustgis_map::source::MysqlOrigin),
    /// A SQL Server table, always opened by the caller (desktop only).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
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
    mut opened: Option<Box<dyn rustgis_data::source::FeatureSource>>,
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
    let tables: Vec<Option<String>> = match (ps(p, "layer").map(str::trim).filter(|t| !t.is_empty()), &origin) {
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
        // (The desktop opens uploads with SQLite, one temporary copy per table, so it adds the
        // first; files on disk add them all.)
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
        return Err(format!("{label}: no feature tables"));
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
            .map_err(err)?,
        };
        let info = src.info().clone();
        // A point cloud is always read in place, so a project keeps its file's path, not millions
        // of points; its colours come from a sample of it.
        let lidar = info.format == "LiDAR";
        let style = if lidar { lidar_renderer(src.as_ref(), s.map().layers.len() as u32) } else { None };
        let stream = b(p, "stream").unwrap_or(lidar || info.count.is_none_or(|c| c > rustgis_map::source::STREAM_ABOVE));
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
                s.notify(format!("{unique}: {n}"), false);
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
            let (mut ds, report) = source::materialize(src.as_ref()).map_err(err)?;
            ds.name = name;
            ds.source = Some(label.clone());
            features += ds.features.len() as u64;
            let id = add_dataset(s, ds);
            if let (Some(r), Some(l)) = (style.clone(), s.map_mut().layer_mut(id)) {
                l.renderer = r;
            }
            ids.push(id);
            for m in report.messages {
                s.notify(m, false);
            }
        }
    }
    if zoom.unwrap_or(empty_before) {
        let e = ids.iter().filter_map(|id| s.map().layer(*id)).fold(rustgis_geom::Extent::EMPTY, |e, l| e.union(&l.extent()));
        if !e.is_empty() {
            s.zoom_to(&e);
        }
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
    let sample = src.read(&rustgis_data::source::Request { extent: None, limit: 100_000 }).ok()?;
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
/// whole-file reader takes, when they don't say whether to stream; `None` otherwise.
#[cfg(not(target_arch = "wasm32"))]
fn past_cap_shapefile(path: &str, p: &Value) -> Option<Value> {
    let shp = Path::new(path);
    let past = |f: &Path| std::fs::metadata(f).is_ok_and(|m| m.len() > rustgis_data::io::MAX_BYTES);
    if !path.to_ascii_lowercase().ends_with(".shp") || b(p, "stream").is_some() || !(past(shp) || past(&shp.with_extension("dbf"))) {
        return None;
    }
    let mut q = p.clone();
    q.as_object_mut()?.insert("stream".into(), json!(true));
    Some(q)
}

/// An OpenStreetMap extract past what the native reader takes, opened through GDAL instead when it
/// is installed (country-size extracts).
#[cfg(not(target_arch = "wasm32"))]
fn large_osm(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".pbf")
        && std::fs::metadata(path).is_ok_and(|m| m.len() > rustgis_data::io::MAX_BYTES)
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
    let original = std::path::absolute(path).map(|p| p.display().to_string()).unwrap_or_else(|_| path.to_string());
    let parts = converted
        .into_iter()
        .map(|c| Converted {
            path: c.path,
            name: c.name,
            unprojected: c.unprojected,
            origin: ConvertedOrigin::Gdal(rustgis_map::source::GdalOrigin {
                path: original.clone(),
                layer: c.layer,
                family: c.family,
                crs: crs.map(str::to_string),
            }),
        })
        .collect();
    add_converted(s, p, path, &original, parts)
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
        let layers: Vec<String> = match LiveDb::connect(&db) {
            Some(live) => {
                let live = live.map_err(|e| format!("{}: {e}", db.short()))?;
                live.tables().map_err(|e| format!("{}: {e}", db.short()))?.iter().map(LiveTable::label).collect()
            }
            None => rustgis_data::source::duckdb::tables_of(&db)
                .map_err(|e| format!("{}: {e}", db.short()))?
                .iter()
                .map(|t| t.label())
                .collect(),
        };
        Ok(json!({"count": layers.len(), "layers": layers}))
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
        use rustgis_data::source::{mysql, postgis, sqlserver};
        Ok(match self {
            LiveDb::Postgis(c) => LiveTable::Postgis(postgis::find_table(c, name)?),
            LiveDb::Mysql(c) => LiveTable::Mysql(mysql::find_table(c, name)?),
            LiveDb::Sqlserver(c) => LiveTable::Sqlserver(sqlserver::find_table(c, name)?),
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
    let mut tables = match ps(p, "layer").map(str::trim).filter(|l| !l.is_empty()) {
        Some(l) => vec![live.find(l).map_err(err)?],
        None => live.tables().map_err(err)?,
    };
    if tables.is_empty() {
        return Err(format!("{label}: no tables with a geometry column"));
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
                return Err(e);
            }
            s.notify(format!("{label}: {}: {e}", t.label()), true);
            Ok(())
        };
        let families = match live.families(&t) {
            Ok(f) => f,
            Err(e) => {
                fail(s, e.to_string())?;
                continue;
            }
        };
        let mixed = families.len() > 1;
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
                None if mixed => format!("{} ({})", t.label(), family.label()),
                None => t.label(),
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
        return Err(if empty > 0 && single {
            format!("{label}: the table is empty")
        } else if empty > 0 {
            format!("{label}: the tables with a geometry column are empty")
        } else {
            format!("{label}: none of its tables could be added")
        });
    }
    if empty_before {
        let e = ids.iter().filter_map(|id| s.map().layer(*id)).fold(rustgis_geom::Extent::EMPTY, |e, l| e.union(&l.extent()));
        if !e.is_empty() {
            s.zoom_to(&e);
        }
    }
    // Empty tables (common in a PostGIS database: its TIGER geocoder's) are left out quietly.
    if empty > 0 {
        s.notify(format!("{label}: {empty} empty tables were left out"), false);
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
/// geometry types.
#[cfg(not(target_arch = "wasm32"))]
fn duckdb_parts(
    s: &mut Session,
    name: &str,
    out: rustgis_data::source::duckdb::Output,
    origin: impl Fn(rustgis_data::source::Family) -> rustgis_map::source::DuckdbOrigin,
) -> Vec<Converted> {
    if out.skipped > 0 {
        s.notify(format!("{name}: {} rows without a geometry were left out", out.skipped), false);
    }
    let mixed = out.parts.len() > 1;
    out.parts
        .into_iter()
        .map(|part| Converted {
            path: part.path,
            name: if mixed { format!("{name} ({})", part.family.label()) } else { name.to_string() },
            // A geometry that says no coordinate system is usually longitude/latitude: only
            // coordinates past it are pointed out (in add_converted).
            unprojected: false,
            origin: ConvertedOrigin::Duckdb(origin(part.family)),
        })
        .collect()
}

/// A layer another program (GDAL, DuckDB) converted into the cache.
#[cfg(not(target_arch = "wasm32"))]
struct Converted {
    path: std::path::PathBuf,
    name: String,
    /// Its coordinates are the source's own, in a coordinate system it doesn't say.
    unprojected: bool,
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
fn add_converted(s: &mut Session, p: &Value, label: &str, original: &str, converted: Vec<Converted>) -> CmdResult {
    let empty_before = super::map_is_empty(s);
    let single = converted.len() == 1;
    let (mut ids, mut features, mut streamed) = (Vec::new(), 0u64, false);
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
        let r = match add_source(s, &q, Origin::Path(&file), None) {
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
            if outside || (c.unprojected && matches!(c.origin, ConvertedOrigin::Gdal(_))) {
                s.notify(
                    format!("{}: the data doesn't say its coordinate system; add it again with \"crs\" (such as EPSG:2263)", c.name),
                    true,
                );
            }
            ids.push(id);
        }
    }
    if empty_before {
        let e = ids.iter().filter_map(|id| s.map().layer(*id)).fold(rustgis_geom::Extent::EMPTY, |e, l| e.union(&l.extent()));
        if !e.is_empty() {
            s.zoom_to(&e);
        }
    }
    if ids.is_empty() {
        return Err(format!("{label}: none of its layers could be added"));
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
    let crs = rustgis_data::raster::Crs::parse(s_req(p, "crs")?)?;
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
    let l = s.map().layer(id).ok_or("no such layer")?;
    let ds = l.load_all()?;
    let n = ds.features.len();
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    l.dataset = ds;
    l.dataset.windowed = false;
    l.source = None;
    l.selection.clear();
    l.touch_geometry();
    Ok(json!({"layer": id, "features": n}))
}

/// Adds a layer file: a layer with its data, symbology and labels.
fn add_layer_file(s: &mut Session, path: &str) -> CmdResult {
    let bytes = crate::project::read_file(Path::new(path), 512 << 20)?;
    let text = String::from_utf8(bytes).map_err(|_| format!("{path} isn't a RustGIS layer file"))?;
    add_layer_text(s, &text)
}

/// Adds a layer from a layer file's JSON.
pub(crate) fn add_layer_text(s: &mut Session, text: &str) -> CmdResult {
    let mut layer: rustgis_map::Layer = serde_json::from_str(text).map_err(|e| format!("not a RustGIS layer file: {e}"))?;
    if layer.dataset.source.as_deref().is_some_and(|source| source.starts_with("geolibre-pending:")) {
        return Err("a layer file cannot contain runtime GeoLibre download reservations".into());
    }
    // A binding in a file is no authority to use a login here.
    crate::s3_connections::clear_bindings(layer.addresses_mut());
    // A fresh id and a unique name in this map; the rest goes through project repair.
    let base = layer.name.clone();
    let mut i = 1;
    while s.map().layer_by_name(&layer.name).is_some() {
        i += 1;
        layer.name = format!("{base} ({i})");
    }
    let id = s.map_mut().add_layer(layer);
    super::connections::clear_active();
    s.project.normalize();
    super::connections::restore_active(s);
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
    rustgis_data::sheets::read(bytes, stem, sheet, &opts).map_err(|e| format!("{file_name}: {e}"))
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
    let limit = if sheet { super::raster::MAX_UPLOAD_BYTES as u64 } else { rustgis_data::io::MAX_BYTES };
    let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    if meta.len() > limit {
        return Err(format!("{path} is too large to open"));
    }
    let mut bytes = Vec::new();
    let read = if sheet {
        std::fs::File::open(path).and_then(|mut f| f.read_to_end(&mut bytes))
    } else {
        std::fs::File::open(path).and_then(|f| f.take(COLUMNS_PREFIX as u64).read_to_end(&mut bytes))
    };
    read.map_err(|e| format!("{path}: {e}"))?;
    columns_of(path, &bytes, p)
}

/// What `info.fileColumns` reads of delimited text: its header and first rows, not all of it.
const COLUMNS_PREFIX: usize = 8 << 20;

/// The columns of a delimited text file or a spreadsheet's sheet (`"layer"`), with the separator
/// (`"delimiter"`, or the one found) and the x/y or WKT columns found by name:
/// `{"columns", "x", "y", "wkt", "delimiter"}`. `info.fileColumns` for a file's bytes (the web
/// app's Add Delimited Text).
pub fn columns_of(name: &str, bytes: &[u8], p: &Value) -> CmdResult {
    let err = |e: rustgis_data::DataError| format!("{name}: {e}");
    let c = if rustgis_data::sheets::is_sheet_name(name) {
        let sheet = ps(p, "layer").map(str::trim).filter(|l| !l.is_empty());
        rustgis_data::sheets::columns(bytes, sheet).map_err(err)?
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
        rustgis_data::io::csv_columns(&text, parse_delimiter(ps(p, "delimiter"))?).map_err(err)?
    } else {
        return Err(format!("{name}: not delimited text (CSV, TSV, TXT) or a spreadsheet"));
    };
    Ok(json!({
        "columns": c.names,
        "x": c.x,
        "y": c.y,
        "wkt": c.wkt,
        "delimiter": c.delimiter.map(|d| char::from(d).to_string()),
    }))
}

/// Whether a file name is delimited text (CSV, TSV, TXT).
fn is_delimited_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".csv") || n.ends_with(".tsv") || n.ends_with(".txt")
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
    let Some(opts) = csv_options(p)? else { return Ok(None) };
    let bytes = bytes()?;
    let (text, note) = rustgis_data::io::delimited_text(name, &bytes).map_err(|e| e.to_string())?;
    let stem = Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or("layer");
    let (ds, mut report) = rustgis_data::io::read_csv_with(&text, stem, &opts).map_err(|e| format!("{name}: {e}"))?;
    report.messages.extend(note);
    Ok(Some((ds, report)))
}

/// The file name for data downloaded from `url`: its last path segment when that has an extension
/// RustGIS reads, else `<segment or "data">.<ext>` with the extension its contents suggest
/// (an error when they suggest none). The name is cut to 120 characters, keeping an extension
/// shorter than 60; a `.shp` address whose download is a zip of its parts is named `<stem>.zip`.
/// JPEG 2000 named or recognised by its contents is refused where GDAL can't run (the browser).
pub fn url_file_name(url: &str, bytes: &[u8]) -> Result<String, String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let path = path.split_once("://").map_or(path, |(_, rest)| rest.split_once('/').map_or("", |(_, p)| p));
    let full: String =
        path.rsplit('/').find(|p| !p.is_empty()).unwrap_or("").chars().filter(|c| !c.is_control() && !matches!(c, '\\' | ':')).collect();
    let raw_ext = Path::new(&full).extension().and_then(|e| e.to_str());
    let ext = raw_ext.map(str::to_ascii_lowercase).unwrap_or_default();
    // At most 120 characters, keeping an extension shorter than 60 (a long `.shp` name is still one).
    let last: String = match raw_ext {
        Some(e) if full.chars().count() > 120 && e.chars().count() < 60 => {
            format!("{}.{e}", full.chars().take(119 - e.chars().count()).collect::<String>())
        }
        _ => full.chars().take(120).collect(),
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

/// Downloads data from a web address and adds it (the command line, MCP and the assistant; the
/// app downloads off the frame and adds the bytes). In the browser, where nothing may wait, the
/// request is kept for the app ([`Session::request_url`]), which downloads and adds it as its own
/// Home › Add › Add Data › From URL does, so a plugin, a script or the assistant can add data by address too.
/// Its errors quote addresses without their keys: the assistant's and MCP's tools return them as
/// they are.
pub(crate) fn add_url(s: &mut Session, p: &Value) -> CmdResult {
    add_url_as_given(s, p).map_err(|e| crate::sources::redact_text(&e))
}

fn add_url_as_given(s: &mut Session, p: &Value) -> CmdResult {
    if ["geolibreProject", "geolibreLayer", "geolibreZarr"].iter().any(|key| p.get(key).is_some()) {
        return Err("GeoLibre download state is internal; use project.importGeolibre".into());
    }
    let url = ps(p, "url").map(str::trim).filter(|u| !u.is_empty()).ok_or("url is required")?;
    // An address read through a saved S3 connection is judged by the address it wraps; the whole
    // address stays what is fetched and kept.
    let lower = rustgis_data::s3_address::inner(url).to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("s3://")) {
        return Err(format!("{url}: give an http://, https:// or s3:// address (Add Data opens files)"));
    }
    // A scan (`"as"`) keeps the pixels and none of the coordinates: the file is downloaded whole and
    // read as one (`add_named_bytes` checks it is an image), never read in place.
    let scan = super::raster::as_scan(p)?;
    // A window of a cloud-optimised GeoTIFF, read as a raster (opengeos/RustGIS#1204).
    let window = p.get("bbox").is_some_and(|v| !v.is_null());
    if !window && p.get("cellSize").is_some_and(|v| !v.is_null()) {
        return Err(format!("{url}: \"cellSize\" goes with \"bbox\": give a box, or leave it out"));
    }
    if window {
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
        let unread: Vec<&str> = WINDOW_UNREAD.iter().copied().filter(|k| p.get(*k).is_some_and(|v| !v.is_null())).collect();
        if !unread.is_empty() {
            return Err(format!("{url}: a window (\"bbox\") doesn't read {}: leave it out", unread.join(", ")));
        }
        // A plugin's command run by the app leaves what would wait to the app, off the frame; a
        // window waits for its header and tiles, which the app doesn't take over yet.
        #[cfg(not(target_arch = "wasm32"))]
        if s.leave_downloads {
            return Err(format!(
                "{url}: a window (\"bbox\") waits for its tiles, which a plugin's command can't do in the app yet: run layer.addUrl from the command line or a script, or leave out \"bbox\" to draw the file in place"
            ));
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
            add_cog(s, p, url)
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
        return s.request_url(p);
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
            return add_cog(s, p, url);
        }
        // Downloaded by the app off the frame and added when it lands.
        s.request_url(p)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Errors name the address without its keys, and any address they quote (a redirect's)
        // without its keys too; those about the address itself name it already.
        let shown = crate::sources::redact(url);
        let named = |e: String| match e.strip_prefix(url).or_else(|| e.strip_prefix(shown.as_str())) {
            Some(rest) => format!("{shown}{}", crate::sources::redact_text(rest)),
            None => format!("{shown}: {}", crate::sources::redact_text(&e)),
        };
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
                return add_cog(s, p, url);
            }
        }
        let bytes = if rustgis_data::io::shapefile_part_urls(url).is_some() {
            download_shapefile_blocking(url)?
        } else {
            crate::render::rustgis_tiles::cache::download_blocking(url).map_err(named)?
        };
        let name = url_file_name(url, &bytes).map_err(named)?;
        let left_out = parts_left_out(url, &bytes, p);
        let r = super::raster::add_named_bytes(s, p, name, bytes, b(p, "zoom")).map_err(named)?;
        if let Some(n) = left_out {
            s.notify(n, false);
        }
        Ok(r)
    }
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
    let e = e.strip_prefix(url).map(|r| r.trim_start_matches(": ").to_string()).unwrap_or(e);
    format!("{}: {}{}", crate::sources::redact(url), crate::sources::redact_text(&e), status_hint(url, &e))
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
    (!said.is_empty()).then(|| format!("{}: {}", crate::sources::redact(url), said.join("; ")))
}

/// A Shapefile by its `.shp` address, waited for (#1238): the `.shp`, then its `.dbf`, `.shx`,
/// `.prj` and `.cpg` beside it (each but the `.dbf` only when it is there), zipped, within one
/// download's budget together.
#[cfg(not(target_arch = "wasm32"))]
fn download_shapefile_blocking(url: &str) -> Result<Vec<u8>, String> {
    use crate::render::rustgis_tiles::cache;
    let parts = rustgis_data::io::shapefile_part_urls(url).unwrap_or_default();
    // One wait for all the parts together, as for one download.
    let until = std::time::Instant::now() + cache::BLOCKING_TIMEOUT;
    // Past it, a part isn't asked for at all (a zero timeout may mean none), and a timeout says
    // where a large file goes instead.
    let within = |u: &str, max: usize, omit: &'static [u16]| match until.saturating_duration_since(std::time::Instant::now()) {
        w if w.is_zero() => Err(cache::large_file_hint("timed out".into())),
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
    zipped_within(&name, shp, got, left).map_err(|e| part_failed(url, e))
}

/// Downloads a file the user named by its address off the frame, as `cache::download_limited`
/// does, for the app to add when it lands: a Shapefile's `.shp` address brings its `.dbf`, `.shx`,
/// `.prj` and `.cpg` too, one after another, and `done` gets them zipped (#1238), within `max`
/// bytes together.
pub fn download_data(url: &str, max: usize, done: crate::render::rustgis_tiles::cache::Done) {
    use crate::render::rustgis_tiles::cache;
    let Some(parts) = rustgis_data::io::shapefile_part_urls(url) else {
        return cache::download_limited(url, max, done);
    };
    let shp_url = url.to_string();
    cache::download_limited(
        url,
        max,
        Box::new(move |r| match r.and_then(|shp| url_file_name(&shp_url, &shp).map(|name| (name, shp))) {
            // Already zipped: as it is.
            Ok((_, shp)) if is_zip(&shp) => done(Ok(shp)),
            Ok((_, shp)) if is_web_page(&shp) => done(Err("a web page answered, not a .shp".into())),
            Ok((name, shp)) => {
                let left = max.saturating_sub(shp.len());
                download_parts(name, shp, parts.into_iter(), Vec::new(), left, done);
            }
            // The app names the address it asked for (redacted), as for any download.
            Err(e) => {
                let why = status_hint(&shp_url, &e);
                done(Err(format!("{}{why}", crate::sources::redact_text(&e))))
            }
        }),
    );
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
/// steps of `download_shapefile_blocking` as callbacks, sharing its rules (`part_not_there`,
/// `part_failed`, `zipped_within`), so a change to one goes in both.
fn download_parts(
    name: String,
    shp: Vec<u8>,
    mut parts: std::vec::IntoIter<(&'static str, String)>,
    mut got: Vec<(&'static str, Vec<u8>)>,
    left: usize,
    done: crate::render::rustgis_tiles::cache::Done,
) {
    let Some((ext, url)) = parts.next() else {
        done(zipped_within(&name, shp, got, left));
        return;
    };
    let next = url.clone();
    crate::render::rustgis_tiles::cache::download_limited_omitting(
        &next,
        left,
        part_absences(ext, &url),
        Box::new(move |r| match r.and_then(|b| page_part(ext, b)) {
            Ok(None) => download_parts(name, shp, parts, got, left, done),
            Ok(Some(b)) => {
                let left = left.saturating_sub(b.len());
                got.push((ext, b));
                download_parts(name, shp, parts, got, left, done);
            }
            Err(e) if part_not_there(ext, &url, &e) => download_parts(name, shp, parts, got, left, done),
            Err(e) => done(Err(part_failed(&url, over_shared_limit(e)))),
        }),
    );
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
        s.notify(format!("{unique}: {n}"), false);
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
    let url = rustgis_data::s3_address::inner(url);
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    path.ends_with(".tif") || path.ends_with(".tiff")
}

/// Whether a web address's last path segment has no extension (an API answering with a file,
/// such as Vantor's `…/geotiff?…`): its first bytes tell whether it is a GeoTIFF to read in place.
pub fn is_unnamed_url(url: &str) -> bool {
    let url = rustgis_data::s3_address::inner(url);
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let Some((_, rest)) = path.split_once("://") else { return false };
    let Some((_, path)) = rest.split_once('/') else { return false };
    path.rsplit('/').find(|p| !p.is_empty()).is_some_and(|last| !last.contains('.'))
}

/// Adds a cloud-optimised GeoTIFF by its address as a layer read in place (its tiles by range,
/// as the view needs them), framed when its header is in.
pub(crate) fn add_cog(s: &mut Session, p: &Value, url: &str) -> CmdResult {
    let stem = cog_name(url);
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&stem);
    let empty_before = super::map_is_empty(s);
    let mut q = json!({"url": url, "kind": "cog", "name": name, "top": true});
    // A dated address's span and steps (see map.addTiles).
    for k in ["start", "end", "unit", "interval", "dates"] {
        if let Some(v) = p.get(k) {
            q[k] = v.clone();
        }
    }
    let mut r = super::tiles::add(s, &q)?;
    // A dated address is framed on the file of the date it shows.
    let url = match r.get("url").and_then(Value::as_str) {
        Some(dated) => {
            // Its header read now (natively) as an undated address's is before it is added.
            #[cfg(not(target_arch = "wasm32"))]
            let _ = crate::render::rustgis_tiles::cog::probe_blocking(dated);
            dated.to_string()
        }
        None => url.to_string(),
    };
    if let Ok(Some(info)) = crate::render::rustgis_tiles::cog::info(&url) {
        let [w, so, e, n] = info.bounds;
        // A cloud-optimised GeoTIFF read in place is framed when it is the first content on the
        // map, as other data is; `"zoom"` says otherwise (`true`, or `false` to keep the view).
        if b(p, "zoom").unwrap_or(empty_before) {
            s.zoom_to(&rustgis_geom::Extent::new(w, so, e, n));
        }
        r["bounds"] = json!(info.bounds);
        // Drawn in place, the tiles are drawn as values: what the header holds beside them (a
        // colour table, band and class names) comes only with a window, and is said.
        let l = &info.layout;
        let named = l.described.as_ref().is_some_and(|d| d.bands.iter().any(|b| b.description.is_some() || !b.categories.is_empty()));
        let mut said: Vec<String> = l.lost.clone();
        said.extend(l.described.iter().flat_map(|d| d.lost.iter().map(|x| format!("{}: {x}", rustgis_data::raster::pam::GDAL_METADATA))));
        // Grey only for a palette file: an RGB file with band names is drawn in its colours.
        match (l.palette.is_some(), named) {
            (true, true) => {
                said.push("drawn in place in grey, without its colour table and band or class names: a window (\"bbox\") reads them".into())
            }
            (true, false) => said.push("drawn in place in grey, without its colour table: a window (\"bbox\") reads it".into()),
            (false, true) => said.push("drawn in place without its band or class names: a window (\"bbox\") reads them".into()),
            (false, false) => {}
        }
        // A tile layer's own name, as added.
        let shown = r.get("name").and_then(Value::as_str).unwrap_or(name).to_string();
        for line in said {
            s.notify(format!("{shown}: {line}"), false);
        }
    } else if r.get("dated").is_some() {
        // Added, but its first date's file couldn't be read: the view stays where it was.
        r["warning"] = json!(format!("{url} couldn't be read yet, so the map wasn't moved to it"));
    }
    r["inPlace"] = json!(true);
    Ok(r)
}

/// The most values a window of a cloud-optimised GeoTIFF holds, all its bands together: one read's
/// most (256 MB of 32-bit values). Under test, 300,000, so the refusal is tried on a small file.
#[cfg(not(target_arch = "wasm32"))]
const MAX_WINDOW_VALUES: u64 = if cfg!(test) { 300_000 } else { crate::render::rustgis_tiles::cog::MAX_VALUES_READ };

/// The most bands a window reads: each band is a read of its own (8 under test, whose examples
/// have 16 bands at most).
#[cfg(not(target_arch = "wasm32"))]
const MAX_WINDOW_BANDS: usize = if cfg!(test) { 8 } else { 256 };

/// How long a window's bands are read for, all together: past it, no band more is asked for.
#[cfg(not(target_arch = "wasm32"))]
const WINDOW_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

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
const WINDOW_UNREAD: [&str; 13] =
    ["layer", "x", "y", "wkt", "crs", "delimiter", "paperSpace", "stream", "start", "end", "unit", "interval", "dates"];

/// Metres in a degree, to compare a cell size in metres with a longitude/latitude file's pixels.
#[cfg(not(target_arch = "wasm32"))]
const METRES_PER_DEGREE: f64 = 111_320.0;

/// `layer.addUrl` with `"bbox"` (opengeos/RustGIS#1204): a window of a cloud-optimised GeoTIFF,
/// read by range as a raster layer. The box (longitude/latitude, or the view) is turned into the
/// file's coordinate system and snapped outwards onto the pixels of the level read: the full image,
/// or with `"cellSize"` the coarsest overview whose cells are no larger. Only the tiles the window
/// takes are read, with the reader the data cubes use. The raster keeps the file's coordinate
/// system, no-data, colour table and band and class names, and is kept in the project (as a
/// GeoTIFF of the window), not read again from the address.
#[cfg(not(target_arch = "wasm32"))]
fn add_cog_window(s: &mut Session, p: &Value, url: &str) -> CmdResult {
    use crate::render::rustgis_tiles::cog;
    use rustgis_data::raster::cog::Level;
    let named = |e: String| if e.starts_with(url) { e } else { format!("{url}: {e}") };
    let [west, south, east, north] = window_box(s, p).map_err(named)?;
    let cell = match p.get("cellSize").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(v.as_f64().filter(|c| c.is_finite() && *c > 0.0).ok_or_else(|| named("cellSize is a size above 0".into()))?),
    };
    // The five minutes count from the header's read.
    let started = std::time::Instant::now();
    let info = cog::info_blocking(url).map_err(named)?;
    let layout = &info.layout;
    let (pw, ph) = (layout.transform.pixel_width, layout.transform.pixel_height);
    if !(pw.is_finite() && ph.is_finite() && pw != 0.0 && ph != 0.0) {
        return Err(named("its header gives no pixel size, so no window can be placed in it".into()));
    }
    if ph > 0.0 || pw < 0.0 {
        return Err(named("it isn't stored north up, which a window doesn't read yet".into()));
    }
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
        let (r0, r1) =
            (at(((y1 - lt.origin_y) / lt.pixel_height).floor(), l.height), at(((y0 - lt.origin_y) / lt.pixel_height).ceil(), l.height));
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
    let finer = cell.filter(|_| fits.is_none());
    let lv = layout.levels.get(level).ok_or_else(|| named("the file has no image".into()))?;
    let (c0, c1, r0, r1) = window(lv);
    if c1 <= c0 || r1 <= r0 {
        return Err(named("the box is outside the file".into()));
    }
    // The sides of the box past the file's edges, which the window is cut at (snapping outwards
    // onto the pixels isn't a cut).
    let cut: Vec<&str> = {
        let lt = layout.transform_of(lv);
        let (fx1, fy0) = (lt.origin_x + f64::from(lv.width) * lt.pixel_width, lt.origin_y + f64::from(lv.height) * lt.pixel_height);
        [
            ("west", x0 < lt.origin_x - tolerance),
            ("east", x1 > fx1 + tolerance),
            ("south", y0 < fy0 - tolerance),
            ("north", y1 > lt.origin_y + tolerance),
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
    if count > MAX_WINDOW_BANDS {
        return Err(named(format!("the file has {count} bands; a window reads {MAX_WINDOW_BANDS} at most")));
    }
    // One read a band, each waiting up to two minutes; no band is started once five minutes are
    // up (the header's read counted), so the last one started may run two minutes more.
    // The values `f32` rounded (32- and 64-bit whole numbers and floats), a count a band, are said
    // when the layer is added, as for a file read whole.
    // Those past its range can't be held: left out (no value), and said apart.
    let (mut bands, mut rounded, mut overflowed) = (Vec::with_capacity(count), Vec::with_capacity(count), 0u64);
    for band in 0..count {
        band_in_time(band, count, started.elapsed(), WINDOW_WAIT).map_err(named)?;
        let (values, changed, past) = cog::read_values_counted_blocking(&info, url, level, band, (c0, c1), (r0, r1)).map_err(named)?;
        bands.push(values);
        rounded.push(changed);
        overflowed += past;
    }
    let lt = layout.transform_of(lv);
    let transform = rustgis_data::raster::GeoTransform {
        origin_x: lt.origin_x + f64::from(c0) * lt.pixel_width,
        origin_y: lt.origin_y + f64::from(r0) * lt.pixel_height,
        ..lt
    };
    let color = match &layout.palette {
        Some(table) if bands.len() == 1 => rustgis_data::raster::ColorHint::Palette(table.clone()),
        _ => layout.color.clone(),
    };
    // A colour table is one band's: a window of more bands comes in without it, and says so.
    let palette_lost = (layout.palette.is_some() && bands.len() != 1)
        .then(|| format!("its colour table is left out: the window has {} bands, a palette has one", bands.len()));
    let stem = cog_name(url);
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&stem).to_string();
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
    let (width, height, size) = (r.width, r.height, size_of(lv));
    let layer = rustgis_map::RasterLayer::from_bytes(r, format!("{name}.tif"), bytes);
    let mut out = super::raster::add_raster_after(s, &name, layer, b(p, "zoom"))?;
    let of = if level == 0 { "the full image".to_string() } else { format!("overview {level} of {}", layout.levels.len() - 1) };
    let added = s.active_layer.and_then(|id| s.map().layer(id)).map(|l| l.name.clone()).unwrap_or(name);
    s.notify(format!("{added}: a {width} × {height} window of {of}, cells of {size:.1}{unit}, read by range from its address"), false);
    if let Some(plain) = plain {
        s.notify(format!("{added}: {plain}"), false);
    }
    if let Some(c) = finer {
        s.notify(format!("{added}: cellSize {c} is finer than the file's finest cells ({size:.1}{unit}): the full image is read"), false);
    }
    if left_out > 0 {
        let kept = band_count - left_out;
        s.notify(
            format!("{added}: the project keeps {kept} of its {band_count} bands: {left_out} band(s) are left out once it is reopened"),
            false,
        );
    }
    if !cut.is_empty() {
        s.notify(format!("{added}: the box reaches past the file's {} edge(s): the window is cut to the file", cut.join(", ")), false);
    }
    if unrefined > 0 {
        s.notify(
            format!("{added}: {unrefined} piece(s) of the box's edges not followed to a quarter of a pixel: the window may fall short of a bent edge"),
            false,
        );
    }
    if unplaced > 0 {
        s.notify(
            format!("{added}: {unplaced} of the {points} points tested along the box's edges have no place in the file's coordinate system: the window is placed from the others"),
            false,
        );
    }
    out["window"] = json!({"columns": [c0, c1], "rows": [r0, r1], "level": level, "cellSize": size});
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
fn window_box(s: &Session, p: &Value) -> Result<[f64; 4], String> {
    const NEED: &str = "bbox is [west, south, east, north] in degrees, or \"view\"";
    let [w, so, e, n] = match p.get("bbox") {
        Some(Value::String(v)) if v.trim().eq_ignore_ascii_case("view") => {
            let e = super::workspace::view_extent(s);
            if e.is_empty() {
                return Err("the map's extent isn't known; give bbox".into());
            }
            // A view past 180° (across the antimeridian, or the world more than once) isn't cut
            // to fit: what it shows past there would be left out.
            if e.min_x < -180.0 || e.max_x > 180.0 {
                return Err("the view reaches past 180° of longitude; give bbox".into());
            }
            [e.min_x, e.min_y.max(-90.0), e.max_x, e.max_y.min(90.0)]
        }
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
    let id = f(p, "id").filter(|v| *v >= 0.0 && v.fract() == 0.0).map(|v| v as u64).ok_or("id is required")?;
    let url = ps(p, "url").unwrap_or("data").to_string();
    // Read before the download is taken, so a bad one leaves it to be added again.
    let service = super::b(p, "service").unwrap_or(false);
    let coverage = super::b(p, "coverage").unwrap_or(false);
    if (service || coverage) && ps(p, "url").is_none_or(|url| url.trim().is_empty()) {
        return Err("url is required for a service or coverage download".into());
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
        let label = ps(p, "name").unwrap_or(&url).to_string();
        super::service::features_answer(s, &label, bytes, bounds)
    } else {
        bytes
    };
    // A WCS coverage (Add From Service): a GeoTIFF, or the service's error said as such.
    let name = if coverage {
        super::service::coverage_check(ps(p, "name").unwrap_or(&url), &bytes)?;
        "coverage.tif".to_string()
    } else {
        url_file_name(&url, &bytes).map_err(|e| format!("{url}: {e}"))?
    };
    let left_out = parts_left_out(&url, &bytes, p);
    let r = super::raster::add_named_bytes(s, p, name, bytes, b(p, "zoom"))?;
    if let Some(n) = left_out {
        s.notify(n, false);
    }
    added_download(s, p, r, service || coverage, &url)
}

/// A download added (`r`, the add's result): a service's features or coverage marked as read from
/// its address, and the map zoomed to them when asked.
fn added_download(s: &mut Session, p: &Value, mut r: Value, mark_source: bool, url: &str) -> CmdResult {
    // A service's features or coverage: known by the address it came from (Add From Service).
    if mark_source {
        super::service::mark_service_source(s, &r, url);
    }
    // `"zoom": true` zooms to everything added, whatever the map held (a link's data); `zoomed`
    // says whether framing happened now, including the tile archive's own bounds handling. A photo result
    // was already framed on the photos this download added; `zoom_to_added` would frame the whole
    // photo layer, earlier photos included.
    if b(p, "zoom") == Some(true) {
        let zoomed =
            r.get("zoomed").and_then(Value::as_bool).unwrap_or_else(|| if r.get("photos").is_some() { true } else { zoom_to_added(s, &r) });
        if let Some(o) = r.as_object_mut() {
            o.insert("zoomed".into(), json!(zoomed));
        }
    }
    Ok(r)
}

/// Zooms to the layers a command's result names (`layer`, `layers`), all of them together;
/// whether any had an extent.
fn zoom_to_added(s: &mut Session, r: &Value) -> bool {
    let ids = r.get("layers").and_then(Value::as_array).into_iter().flatten().chain(r.get("layer"));
    let e = ids
        .filter_map(Value::as_u64)
        .filter_map(|i| u32::try_from(i).ok())
        .filter_map(|id| s.map().layer(id).map(|l| l.extent()))
        .filter(|e| !e.is_empty())
        .fold(rustgis_geom::Extent::EMPTY, |a, e| a.union(&e));
    if e.is_empty() {
        return false;
    }
    s.zoom_to(&e);
    true
}

pub(crate) fn add_data_text(s: &mut Session, p: &Value) -> CmdResult {
    add_data_text_after(s, p, None)
}

/// [`add_data_text`], with the adding command's `"zoom"` (layer.addUrl, layer.addDownload).
pub(crate) fn add_data_text_after(s: &mut Session, p: &Value, zoom: Option<bool>) -> CmdResult {
    let name = s_req(p, "fileName")?;
    let text = s_req(p, "text")?;
    // A CityJSON model: all its layers, in the coordinate system given or with a layer picked.
    if rustgis_data::cityjson::is_cityjson_name(name)
        || (["crs", "layer"].iter().any(|k| ps(p, k).is_some_and(|c| !c.trim().is_empty()))
            && rustgis_data::io::is_cityjson(name, text.as_bytes()))
    {
        return add_layers(s, p, name, text.as_bytes(), zoom);
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
        s.notify(m, false);
    }
    if ds.geometry_type == GeometryType::None {
        let name = ds.name.clone();
        let id = s.project.add_table(ds);
        return Ok(json!({"table": name, "id": id}));
    }
    let n = ds.features.len();
    let empty_before = super::map_is_empty(s);
    let id = add_dataset(s, ds);
    if zoom.unwrap_or(empty_before)
        && let Some(e) = s.map().layer(id).map(|l| l.extent())
    {
        s.zoom_to(&e);
    }
    Ok(json!({"layer": id, "features": n}))
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

/// Legends that leave out layer `old` of the active map, in the frames that show it, follow it:
/// to its new name, or (`None`, the layer deleted) by dropping it, so a later layer of that name
/// isn't left out unasked. Called once the layer is renamed or gone: while another layer still
/// bears `old` (names may repeat), the legends keep it for that one. Renamed onto the name of
/// another layer a legend lists, the layer is listed with it rather than taking that one out.
/// A WMS or WMTS layer's name counts as a layer's: legends list those too.
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
    for layout in &mut s.project.layouts {
        let frames: Vec<u32> = layout
            .elements
            .iter()
            .filter(|e| matches!(&e.kind, rustgis_map::layout::ElementKind::MapFrame { map: m, .. } if m.eq_ignore_ascii_case(&map)))
            .map(|e| e.id)
            .collect();
        for e in &mut layout.elements {
            if let rustgis_map::layout::ElementKind::Legend { frame, exclude, .. } = &mut e.kind
                && frames.contains(frame)
            {
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
    for n in &names {
        legends_follow_layer(s, n, None);
    }
}

/// After layers called `names` are removed: a terrain drawn from one of them would lose its
/// relief, or take another layer's heights by that name, so it is turned off, and said.
fn terrain_follows_removal(s: &mut Session, names: &[String]) {
    let Some(dem) = terrain_dem(s).filter(|d| names.iter().any(|n| n.eq_ignore_ascii_case(d))) else { return };
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
pub(crate) fn layer_ids(s: &Session, p: &Value) -> Result<Vec<u32>, String> {
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
/// bottom. One command (and one undo step) however far the layer travels.
fn move_to(s: &mut Session, p: &Value) -> CmdResult {
    let ids = layer_ids(s, p)?;
    let to = uint(p, "to").ok_or("to is required: the position in the drawing order, 0 at the top")?;
    if let [id] = ids[..] {
        return move_one(s, id, to);
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
    Ok(Value::Null)
}

/// Moves a layer to index `t` of the drawing order, if it isn't there already.
fn move_to_index(m: &mut rustgis_map::Map, id: u32, t: usize) -> Result<(), String> {
    let i = m.layer_index(id).ok_or("no such layer")?;
    if i != t {
        m.move_layer(i, t)?;
    }
    Ok(())
}

fn move_one(s: &mut Session, id: u32, to: u64) -> CmdResult {
    let m = s.map_mut();
    let i = m.layer_index(id).ok_or("no such layer")?;
    let to = usize::try_from(to).unwrap_or(usize::MAX).min(m.layers.len().saturating_sub(1));
    if to == i {
        return Err("the layer is already there".into());
    }
    m.move_layer(i, to)?;
    Ok(Value::Null)
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
                Renderer::Graduated { field: f, breaks, .. } if *f == field => Some(breaks.clone()),
                _ => None,
            };
            match (method, typed.or(kept)) {
                (ClassMethod::Manual, Some(b)) => Renderer::graduated_manual(&l.dataset, &rows()?, &field, &b, ramp, base)?,
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
    // and outlines set by hand too while the colour scheme stays (#314).
    r.carry_edits_from(&l.renderer, g);
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
    let out = with(s, p, |l| {
        l.renderer = r;
        take_no_fill(l);
    })?;
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
    Ok(json!({"label": e.label, "color": e.symbol.fill.hex(), "outline": e.symbol.stroke.hex()}))
}
