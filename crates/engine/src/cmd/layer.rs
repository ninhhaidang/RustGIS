//! Home › Add (Add Data), Layers pane actions, and the Feature Layer / Labeling / Data tabs.

use std::path::Path;

use rustgis_data::{GeometryType, photo};
use rustgis_map::symbology::{ClassRef, ClassStyle, LABEL_SIZE, MAX_STROKES};
use rustgis_map::{ClassMethod, LineCap, LineJoin, LineStroke, Marker, Ramp, Renderer, Rgba};
use serde_json::{Value, json};

use super::{b, f, need_feature_layer, need_layer, ps, uint};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("layer.addData", "Add Data", "Home › Add", add_data)
            .params(r#"{"path": string, "name"?: string, "layer"?: "GeoPackage, GDAL, DuckDB or DXF layer", "paperSpace"?: "bool: a DXF's layout entities too", "stream"?: bool, "as"?: "scan" (a TIFF, PNG or JPEG, even a photo or a GeoTIFF, as a picture with no coordinates), "crs"?: "EPSG:… (or WKT) for a CSV's coordinates, or a GDAL or DuckDB file that doesn't say", "x"?: "CSV x/longitude column", "y"?: "CSV y/latitude column", "wkt"?: "CSV WKT geometry column", "delimiter"?: ","|";"|"\\t"|"|"}"#),
        C::new("layer.addPhotos", "Add Photos", "Home › Add", add_photos).params(
            r#"{"paths"?: [JPEG photo files], "folder"?: "a folder of photos (every JPEG in it)", "name"?: "the layer (default Photos; photos join it when it exists)"}"#,
        ),
        C::new("info.fileLayers", "List Layers in File", "Home › Add › Add Data", file_layers)
            .view()
            .params(r#"{"path": string}"#),
        C::new("info.fileColumns", "List Columns in File", "Home › Add › Delimited Text", file_columns).view().params(
            r#"{"path": "a delimited text file (CSV, TSV, TXT) or a spreadsheet", "delimiter"?: ","|";"|"\\t"|"|", "layer"?: "a spreadsheet's sheet (the first by default)"}"#,
        ),
        C::new("layer.addSql", "Add Data From SQL", "Home › Add", add_sql).params(
            r#"{"sql": "one DuckDB SELECT; DuckDB Spatial's ST_ functions, read_parquet('…'), web addresses", "name"?: string, "database"?: "a .duckdb file or a PostgreSQL connection (postgres:host=… dbname=…) whose tables the query names", "password"?: "the connection's, kept for this session only", "crs"?: "EPSG:… of a geometry that doesn't say", "stream"?: bool, "ran"?: "a result the app ran (the SQL pane)"}"#,
        ),
        C::new("layer.addDatabase", "Add Data From Database", "Home › Add", add_database).params(
            r#"{"database": "postgres:host=… dbname=… user=… (or a postgresql:// address; PostGIS tables are read live), a mysql://user@host:port/database address (MySQL or MariaDB, read live; mariadb:// too), an mssql://user@host:1433/database address (SQL Server or Azure SQL, read live; ?encrypt=strict|false, ?trust-server-certificate=true for a self-signed certificate, ?instance=NAME; on Windows DOMAIN\\user or no user signs in with Windows), or a .duckdb file", "password"?: "kept for this session only, never saved", "layer"?: "a table, as table or schema.table (table:column for one of several geometry columns; default: every table with a geometry column)", "crs"?: "EPSG:… of a geometry that doesn't say", "name"?: string, "stream"?: bool}"#,
        ),
        C::new("info.databaseTables", "List Database Tables", "Home › Add › Add Data From Database", database_tables)
            .view()
            .params(r#"{"database": "as layer.addDatabase", "password"?: string}"#),
        C::new("layer.loadAll", "Load All Features", "Layers", load_all).when(need_streamed).params(r#"{"layer"?: name|id}"#),
        C::new("layer.addUrl", "Add Data From URL", "Home › Add", add_url).params(
            r#"{"url": "https://…/data.geojson|.csv|.parquet|.fgb|.gpkg|.zip|.kml|.kmz|.gpx|.tif|.jp2 (JPEG 2000: desktop, through GDAL) (the format from the address, else the contents) | a .pmtiles archive", "name"?: text, "layer"?: "one layer of a zip, KML or GPX", "zoom"?: "bool: true zooms to what was added; false keeps the view of a GeoParquet or FlatGeobuf read in place (zoomed to only when the map was empty)", "x"?, "y"?, "wkt"?, "crs"?, "delimiter"?, "paperSpace"?, "stream"?, "as"? (as layer.addData), "whole"?: "bool: download a cloud-optimised GeoTIFF whole (as a raster layer) instead of reading it in place", "start"?, "end"?, "unit"?, "interval"?, "dates"?: "a GeoTIFF address with date placeholders ({date:YYYY}, {YYYY}{MM}{DD}), one file per date: the dates it spans and steps through (as time.set), turning the time slider on"}"#,
        ),
        C::new("layer.addDownload", "Add Downloaded Data", "Home › Add", add_download)
            .params(r#"{"id": a download the app finished (Add Data › From URL), "url": its address, "name"?: text, "layer"?: text, "zoom"?: bool, "x"?, "y"?, "wkt"?, "crs"?, "delimiter"?, "paperSpace"?, "stream"?, "as"? (as layer.addData), "service"?: bool (a service's features: their axes and paging are checked), "coverage"?: bool (a WCS coverage: a GeoTIFF, else the service's error)}"#),
        C::new("layer.addEncodedPolyline", "Add Encoded Polyline", "Home › Add", add_encoded_polyline).params(
            r#"{"polyline": "encoded polyline text" | [texts] (one line each), "precision"?: 5 (Google) | 6 (OSRM, Valhalla), "name"?: text}"#,
        ),
        C::new("layer.addDataText", "Add Data (from text)", "Home › Add", add_data_text).params(r#"{"fileName": "name.geojson|.csv|.tsv", "text": string, "name"?: text, "x"?, "y"?, "wkt"?, "crs"?, "delimiter"? (as layer.addData, for delimited text)}"#),
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
            let old = s.map().layer(s.layer_id(p)?).map(|l| l.name.clone()).unwrap_or_default();
            let r = with(s, p, |l| {
                l.name = n.clone();
                l.dataset.name = n.clone();
            })?;
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
            .params(r#"{"layer"?, "type": "single"|"unique"|"graduated"|"proportional", "field"?, "classes"?: 1..12, "method"?: "naturalBreaks"|"equalInterval"|"quantile", "ramp"?: string}"#),
        C::new("layer.ramp", "Color Scheme", "Style › Drawing", |s, p| {
            let r = s.ramp(s_req(p, "ramp")?)?;
            with(s, p, |l| match &mut l.raster {
                Some(rl) => rl.style.set_ramp(r),
                None => {
                    let g = l.geometry_type();
                    l.renderer.apply_ramp(r, g)
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
            })
        })
        .when(need_feature_layer)
        .params(r##"{"layer"?, "color": "#RRGGBB"|"#RRGGBBAA" (the fill; a polygon's outline is left as it is, set it with layer.outline)}"##),
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
            .when(need_lines)
            .params(
                r##"{"layer"?, "strokes": [{"color"?: "#RRGGBB"|"#RRGGBBAA"|null (null: the renderer's line colour), "width"?: points|null (null: the renderer's width), "offset"?: points (positive: right of the line's direction, negative: left), "cap"?: "round"|"flat"|"square", "join"?: "round"|"miter"|"bevel"}] (drawn bottom first; [] goes back to the single stroke)}"##,
            ),
        C::new("layer.marker", "Marker", "Style › Drawing", |s, p| {
            let shape = ps(p, "shape").map(|n| Marker::ALL.into_iter().find(|m| m.label().eq_ignore_ascii_case(n)).ok_or("unknown marker shape")).transpose()?;
            let size = f(p, "size").map(|v| v.clamp(1.0, 80.0) as f32);
            with(s, p, |l| {
                l.renderer.map_symbols(|sym| {
                    if let Some(m) = shape {
                        sym.marker = m;
                    }
                    if let Some(sz) = size {
                        sym.size = sz;
                    }
                })
            })
        })
        .when(need_feature_layer)
        .params(r#"{"layer"?, "shape"?: "circle"|"square"|"triangle"|"diamond"|"star", "size"?: number}"#),
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
        C::new("labels.enable", "Label", "Style › Labels", |s, p| {
            let on = b(p, "on");
            with(s, p, |l| l.labels.enabled = on.unwrap_or(!l.labels.enabled))
        })
        .when(need_feature_layer)
        .params(r#"{"layer"?, "on"?: bool}"#),
        C::new("labels.field", "Label Field", "Style › Label Class", |s, p| {
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
        C::new("labels.style", "Text Symbol", "Style › Text Symbol", |s, p| {
            let size = f(p, "size").map(|v| v.clamp(*LABEL_SIZE.start(), *LABEL_SIZE.end()) as f32);
            let color = s.get_color(p, "color")?;
            let (halo, bold) = (b(p, "halo"), b(p, "bold"));
            super::labels::with_class(s, p, |c| {
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
            })
        })
        .when(need_feature_layer)
        .params(r##"{"layer"?, "class"?: name (none: the Default class, by whatever name it has), "size"?: number, "color"?: "#RRGGBB"|"#RRGGBBAA", "halo"?: bool, "bold"?: bool}"##),
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

/// A line layer is active.
fn need_lines(s: &Session) -> Option<&'static str> {
    match s.active_layer.and_then(|id| s.map().layer(id)) {
        Some(l) if l.geometry_type() == GeometryType::Polyline && !l.graphics => None,
        Some(_) => Some("this works on line layers"),
        None => Some("select a layer in the Layers pane first"),
    }
}

/// `layer.lineStrokes`: sets a line layer's stacked strokes.
fn line_strokes(s: &mut Session, p: &Value) -> CmdResult {
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
        strokes.push(LineStroke { color, width: num("width")?, offset: num("offset")?.unwrap_or(0.0), cap, join }.clamped());
    }
    let id = s.layer_id(p)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    if l.geometry_type() != GeometryType::Polyline || l.graphics {
        return Err("this works on line layers".into());
    }
    l.line_strokes = strokes;
    Ok(json!({"strokes": l.line_strokes.len()}))
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
/// and named in a notice.
pub(crate) fn add_photos_to(s: &mut Session, name: &str, feed: impl FnOnce(&mut photo::Reader)) -> CmdResult {
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
    s.zoom_to(&extent);
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
        add_photos_to(s, &name, |reader| {
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
    match add_reads(s, p, path, reads, Vec::new()) {
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
        return add_source(s, p, Origin::Path(path));
    }
    // A large GeoJSON FeatureCollection (any, with `"stream": true`) is read in place, streamed
    // when it holds many features; another GeoJSON document, or one that can't be read so, is
    // read whole below.
    #[cfg(not(target_arch = "wasm32"))]
    if is_geojson(path)
        && b(p, "stream").unwrap_or_else(|| {
            std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > rustgis_data::source::geojson::IN_PLACE_ABOVE)
        })
    {
        let name = Path::new(path).file_stem().and_then(|n| n.to_str()).unwrap_or("layer");
        match rustgis_data::source::geojson::GeoJsonSource::open_path(Path::new(path), name) {
            Ok(src) => return add_source_with(s, p, Origin::Path(path), Some(Box::new(src))),
            // Not a FeatureCollection: read whole below.
            Err(rustgis_data::DataError::Unsupported(_)) => {}
            Err(e) => return Err(format!("{path}: {e}")),
        }
    }
    // A large Shapefile is read in place, as a GeoPackage is; smaller ones are loaded whole.
    #[cfg(not(target_arch = "wasm32"))]
    if path.to_ascii_lowercase().ends_with(".shp")
        && b(p, "stream").unwrap_or_else(|| {
            rustgis_data::source::shp::record_count(Path::new(path)).is_some_and(|n| n > rustgis_map::source::STREAM_ABOVE)
        })
    {
        return add_source(s, p, Origin::Path(path));
    }
    // A PMTiles archive is a tile layer (raster or vector, as its header says).
    if path.to_ascii_lowercase().ends_with(".pmtiles") {
        if cfg!(target_arch = "wasm32") {
            return Err(format!("{path}: the web app reads PMTiles archives from web addresses (Home › Add › Basemap › Add Tile Layer)"));
        }
        let full = std::path::absolute(path).map(|p| p.display().to_string()).map_err(|e| format!("{path}: {e}"))?;
        let mut q = json!({"url": full, "kind": "pmtiles"});
        if let Some(n) = ps(p, "name") {
            q["name"] = json!(n);
        }
        return super::tiles::add(s, &q);
    }
    // So is an MBTiles archive, read in place a tile at a time.
    if rustgis_data::mbtiles::is_mbtiles_name(path) {
        if cfg!(target_arch = "wasm32") {
            return Err(format!("{path}: in the web app, add the MBTiles file from this computer (Home › Add Data)"));
        }
        let full = std::path::absolute(path).map(|p| p.display().to_string()).map_err(|e| format!("{path}: {e}"))?;
        // Read now, so a file that isn't an archive is said at once rather than when it draws.
        crate::render::rustgis_tiles::mbtiles::info(&full)?;
        let mut q = json!({"url": full, "kind": "mbtiles"});
        if let Some(n) = ps(p, "name") {
            q["name"] = json!(n);
        }
        return super::tiles::add(s, &q);
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
        return add_sheet(s, p, path, &bytes);
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
        return add_layers(s, p, &full, &bytes);
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
    add_read(s, p, read)
}

/// Adds every layer of a file that holds several: the Shapefiles (and other files) of a zip, a KML
/// or KMZ's points, lines and polygons drawn in its styles, a GPX's waypoints, routes, tracks and
/// track points. `"layer"` picks one by name; a name given applies to the first.
pub(crate) fn add_layers(s: &mut Session, p: &Value, file_name: &str, bytes: &[u8]) -> CmdResult {
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
        reads.retain(|r| pick(&r.dataset.name));
        if reads.is_empty() {
            return Err(format!("{file_name}: no layer '{want}' (layers: {})", names.join(", ")));
        }
    }
    add_reads(s, p, file_name, reads, overlays)
}

/// Adds the layers a file gave and its ground overlays: a layer that can't be added is skipped
/// and said, the others are still added. A name given goes to the first layer added.
fn add_reads(
    s: &mut Session,
    p: &Value,
    file_name: &str,
    reads: Vec<rustgis_data::io::LayerRead>,
    overlays: Vec<(String, rustgis_data::raster::Raster)>,
) -> CmdResult {
    let mut added = Vec::new();
    let mut failed = Vec::new();
    let mut q = p.clone();
    for read in reads {
        let name = read.dataset.name.clone();
        let styles = read.styles;
        // A layer that can't be added is skipped and said; the others are still added. A name
        // given goes to the first layer actually added.
        match add_read(s, &q, (read.dataset, read.report)) {
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
        match super::raster::add_overlay(s, &name, raster) {
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
            (std::fs::metadata(&p).ok()?.len() < 256 << 20).then(|| std::fs::read(p).ok()).flatten()
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
    if rustgis_data::io::is_multi_layer_name(path) {
        let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        if meta.len() > super::raster::MAX_UPLOAD_BYTES as u64 {
            return Err(format!("{path} is too large to open"));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let layers: Vec<String> = rustgis_data::io::read_layers(path, &bytes).map_err(err)?.into_iter().map(|l| l.dataset.name).collect();
        return Ok(json!({"count": layers.len(), "layers": layers}));
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
pub(crate) fn add_source(s: &mut Session, p: &Value, origin: Origin<'_>) -> CmdResult {
    add_source_with(s, p, origin, None)
}

/// [`add_source`], with the source already open (its one table).
fn add_source_with(
    s: &mut Session,
    p: &Value,
    origin: Origin<'_>,
    mut opened: Option<Box<dyn rustgis_data::source::FeatureSource>>,
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
    let empty_before = s.map().layers.is_empty();
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
            let layer = match &origin {
                Origin::Path(path) => {
                    let full = std::path::absolute(path).map(|p| p.display().to_string()).unwrap_or_else(|_| path.to_string());
                    rustgis_map::source::SourceLayer::from_path(full, table.clone(), src)
                }
                Origin::Bytes(file, bytes) => rustgis_map::source::SourceLayer::from_bytes(*file, bytes.clone(), table.clone(), src),
                Origin::Postgis(o) => rustgis_map::source::SourceLayer::from_postgis((*o).clone(), src),
                Origin::Mysql(o) => rustgis_map::source::SourceLayer::from_mysql((*o).clone(), src),
                Origin::Sqlserver(o) => rustgis_map::source::SourceLayer::from_sqlserver((*o).clone(), src),
            };
            let unique = unique_name(s, &name);
            for n in &info.notes {
                s.notify(format!("{unique}: {n}"), false);
            }
            let id = s.map_mut().add_streamed(unique.clone(), layer);
            if let (Some(r), Some(l)) = (style.clone(), s.map_mut().layer_mut(id)) {
                l.renderer = r;
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
    if empty_before {
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
            Err("SQL queries don't run on MySQL, MariaDB or SQL Server databases: add their tables with Add Data › Database…".into())
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

/// The geometry tables of a live database server, with the schema, name, column and type of
/// each (`connection.browse` groups them by schema).
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn database_table_details(p: &Value) -> Result<Vec<Value>, String> {
    let db = database_param(p).ok_or("database is required")?;
    let live = LiveDb::connect(&db).ok_or("not a database server")?.map_err(|e| format!("{}: {e}", db.short()))?;
    Ok(live.tables().map_err(|e| format!("{}: {e}", db.short()))?.iter().map(LiveTable::detail).collect())
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

    /// The table as a browser lists it: its label for `layer.addDatabase`, and what it is made of.
    fn detail(&self) -> Value {
        let (schema, name, column, kind) = match self {
            LiveTable::Postgis(t) => (&t.schema, &t.name, &t.column, &t.kind),
            LiveTable::Mysql(t) => (&t.schema, &t.name, &t.column, &t.kind),
            LiveTable::Sqlserver(t) => (&t.schema, &t.name, &t.column, &t.kind),
        };
        json!({"label": self.label(), "schema": schema, "name": name, "column": column, "kind": kind})
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
    let empty_before = s.map().layers.is_empty();
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
            let r = match add_source_with(s, &q, origin, Some(src)) {
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
    let empty_before = s.map().layers.is_empty();
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
        let r = match add_source(s, &q, Origin::Path(&file)) {
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
    let len = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?.len();
    if len > 512 << 20 {
        return Err("the layer file is too large".into());
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    add_layer_text(s, &text)
}

/// Adds a layer from a layer file's JSON.
pub(crate) fn add_layer_text(s: &mut Session, text: &str) -> CmdResult {
    let mut layer: rustgis_map::Layer = serde_json::from_str(text).map_err(|e| format!("not a RustGIS layer file: {e}"))?;
    // A fresh id and a unique name in this map; the rest goes through project repair.
    let base = layer.name.clone();
    let mut i = 1;
    while s.map().layer_by_name(&layer.name).is_some() {
        i += 1;
        layer.name = format!("{base} ({i})");
    }
    let id = s.map_mut().add_layer(layer);
    s.project.normalize();
    s.active_layer = Some(id);
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
    add_read(s, p, (ds, rustgis_data::ReadReport::default()))
}

/// Adds a sheet of a spreadsheet (`"layer"` names it; the first by default), read like delimited
/// text with the same options.
pub(crate) fn add_sheet(s: &mut Session, p: &Value, file_name: &str, bytes: &[u8]) -> CmdResult {
    let stem = Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("sheet");
    let opts = csv_options(p)?.unwrap_or_default();
    let sheet = ps(p, "layer").map(str::trim).filter(|l| !l.is_empty());
    let read = rustgis_data::sheets::read(bytes, stem, sheet, &opts).map_err(|e| format!("{file_name}: {e}"))?;
    add_read(s, p, read)
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
            Ok(text) => text,
            // The part read may end inside a character.
            Err(e) if e.error_len().is_none() => std::str::from_utf8(bytes.get(..e.valid_up_to()).unwrap_or_default()).unwrap_or_default(),
            Err(_) => return Err(format!("{name} is not UTF-8 text")),
        };
        rustgis_data::io::csv_columns(text, parse_delimiter(ps(p, "delimiter"))?).map_err(err)?
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
    let text = std::str::from_utf8(&bytes).map_err(|_| format!("{name} is not UTF-8 text"))?;
    let stem = Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or("layer");
    rustgis_data::io::read_csv_with(text, stem, &opts).map(Some).map_err(|e| format!("{name}: {e}"))
}

/// The file name for data downloaded from `url`: its last path segment when that has an extension
/// RustGIS reads, else `<segment or "data">.<ext>` with the extension its contents suggest
/// (an error when they suggest none). JPEG 2000 named or recognised by its contents is refused
/// where GDAL can't run (the browser).
pub fn url_file_name(url: &str, bytes: &[u8]) -> Result<String, String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let path = path.split_once("://").map_or(path, |(_, rest)| rest.split_once('/').map_or("", |(_, p)| p));
    let last: String = path
        .rsplit('/')
        .find(|p| !p.is_empty())
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '\\' | ':'))
        .take(120)
        .collect();
    let ext = Path::new(&last).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    // JPEG 2000 (Sentinel-2's imagery) is read through GDAL, which the browser can't run.
    let jpeg2000_only_on_desktop = |name: String| {
        if cfg!(target_arch = "wasm32") { Err(super::raster::JPEG2000_WEB_LIMITATION.to_string()) } else { Ok(name) }
    };
    if rustgis_data::raster::is_jpeg2000_name(&last) {
        return jpeg2000_only_on_desktop(last);
    }
    let known = rustgis_data::io::READ_EXTENSIONS.contains(&ext.as_str()) || rustgis_data::raster::is_raster_name(&last) || ext == "tsv";
    if known && ext != "json" {
        return Ok(last);
    }
    let stem = Path::new(&last).file_stem().and_then(|s| s.to_str()).filter(|s| !s.is_empty()).unwrap_or("data");
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
/// Add Data › From URL does, so a plugin, a script or the assistant can add data by address too.
pub(crate) fn add_url(s: &mut Session, p: &Value) -> CmdResult {
    let url = ps(p, "url").map(str::trim).filter(|u| !u.is_empty()).ok_or("url is required")?;
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("s3://")) {
        return Err(format!("{url}: give an http://, https:// or s3:// address (Add Data opens files)"));
    }
    // A scan (`"as"`) keeps the pixels and none of the coordinates: the file is downloaded whole and
    // read as one (`add_named_bytes` checks it is an image), never read in place.
    let scan = super::raster::as_scan(p)?;
    // A PMTiles archive is read a tile at a time, never downloaded whole.
    if lower.split(['?', '#']).next().is_some_and(|p| p.ends_with(".pmtiles")) {
        if scan {
            return Err(format!("{url}: a PMTiles archive is tiles, not a picture: it can't be opened as a scan"));
        }
        let mut q = json!({"url": url, "kind": "pmtiles"});
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
    if rustgis_data::source::is_remote_name(url) && !super::b(p, "whole").unwrap_or(false) {
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
    // A GeoTIFF: read in place when it is cloud-optimised (tiled), otherwise downloaded whole.
    let tiff = is_tiff_url(url);
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
        // Errors about the address itself name it already.
        let named = |e: String| if e.starts_with(url) { e } else { format!("{url}: {e}") };
        if tiff
            && !scan
            && !super::b(p, "whole").unwrap_or(false)
            && crate::render::rustgis_tiles::cog::probe_blocking(url).map_err(named)?
        {
            return add_cog(s, p, url);
        }
        let bytes = crate::render::rustgis_tiles::cache::download_blocking(url).map_err(named)?;
        let name = url_file_name(url, &bytes).map_err(named)?;
        super::raster::add_named_bytes(s, p, name, bytes).map_err(named)
    }
}

/// Adds a FlatGeobuf or GeoParquet by its address as a layer read in place: a view's features at
/// a time, by range. The project keeps the address.
fn add_remote(s: &mut Session, p: &Value, url: &str, src: Box<dyn rustgis_data::source::FeatureSource>) -> CmdResult {
    let info = src.info().clone();
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| info.name.clone());
    let empty_before = s.map().layers.is_empty();
    let layer = rustgis_map::source::SourceLayer::from_path(url.to_string(), None, src);
    let unique = unique_name(s, &name);
    for n in &info.notes {
        s.notify(format!("{unique}: {n}"), false);
    }
    let id = s.map_mut().add_streamed(unique.clone(), layer);
    s.active_layer = Some(id);
    let count = info.count.map(|c| c.to_string()).unwrap_or_else(|| "many".into());
    s.notify(format!("{unique}: {count} features, read in place by range from its address, a view at a time"), false);
    // Framed when it is the map's first layer, unless told to keep the view (a catalog's data for
    // the view, such as Overture's).
    if (empty_before || b(p, "zoom") == Some(true)) && b(p, "zoom") != Some(false) && !info.extent.is_empty() {
        s.zoom_to(&info.extent);
    }
    Ok(json!({"layer": id, "layers": [id], "features": info.count, "streamed": true, "inPlace": true}))
}

/// Whether a web address names a GeoTIFF (by its path's extension).
pub fn is_tiff_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    path.ends_with(".tif") || path.ends_with(".tiff")
}

/// Adds a cloud-optimised GeoTIFF by its address as a layer read in place (its tiles by range,
/// as the view needs them), framed when its header is in.
pub(crate) fn add_cog(s: &mut Session, p: &Value, url: &str) -> CmdResult {
    let stem = cog_name(url);
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&stem);
    let mut q = json!({"url": url, "kind": "cog", "name": name});
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
        s.zoom_to(&rustgis_geom::Extent::new(w, so, e, n));
        r["bounds"] = json!(info.bounds);
    } else if r.get("dated").is_some() {
        // Added, but its first date's file couldn't be read: the view stays where it was.
        r["warning"] = json!(format!("{url} couldn't be read yet, so the map wasn't moved to it"));
    }
    r["inPlace"] = json!(true);
    Ok(r)
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
    let i = s.downloaded.iter().position(|(d, _)| *d == id).ok_or("no such download (it was dropped when another project opened)")?;
    let (_, bytes) = s.downloaded.remove(i);
    // A service's features (Add From Service): its answer's axes and paging.
    let bytes = if super::b(p, "service").unwrap_or(false) {
        let label = ps(p, "name").unwrap_or(&url).to_string();
        super::service::features_answer(s, &label, bytes)
    } else {
        bytes
    };
    // A WCS coverage (Add From Service): a GeoTIFF, or the service's error said as such.
    let name = if super::b(p, "coverage").unwrap_or(false) {
        super::service::coverage_check(ps(p, "name").unwrap_or(&url), &bytes)?;
        "coverage.tif".to_string()
    } else {
        url_file_name(&url, &bytes).map_err(|e| format!("{url}: {e}"))?
    };
    let mut r = super::raster::add_named_bytes(s, p, name, bytes)?;
    // `"zoom": true` zooms to everything added, whatever the map held (a link's data); `zoomed`
    // says whether there was anything to zoom to (a tile archive adds no layer).
    if b(p, "zoom") == Some(true) {
        let zoomed = zoom_to_added(s, &r);
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
    let name = s_req(p, "fileName")?;
    let text = s_req(p, "text")?;
    if let Some(read) = read_delimited(p, name, || Ok(text.as_bytes().to_vec()))? {
        return add_read(s, p, read);
    }
    let read = rustgis_data::io::read_bytes(name, text.as_bytes()).map_err(|e| format!("{name}: {e}"))?;
    add_read(s, p, read)
}

pub(crate) fn add_read(s: &mut Session, p: &Value, read: (rustgis_data::Dataset, rustgis_data::ReadReport)) -> CmdResult {
    let (mut ds, report) = read;
    if let Some(n) = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()) {
        ds.name = n.to_string();
    }
    if ds.geometry_type == GeometryType::None {
        let name = ds.name.clone();
        let id = s.project.add_table(ds);
        return Ok(json!({"table": name, "id": id}));
    }
    let n = ds.features.len();
    let empty_before = s.map().layers.is_empty();
    let id = add_dataset(s, ds);
    for m in report.messages {
        s.notify(m, false);
    }
    if empty_before && let Some(e) = s.map().layer(id).map(|l| l.extent()) {
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
pub(crate) fn legends_follow_layer(s: &mut Session, old: &str, new: Option<&str>) {
    if s.map().layers.iter().any(|l| l.name.eq_ignore_ascii_case(old)) {
        return;
    }
    let shared = new.is_some_and(|n| s.map().layers.iter().filter(|l| l.name.eq_ignore_ascii_case(n)).count() > 1);
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
    m.layers.retain(|l| !ids.contains(&l.id));
    for n in &names {
        legends_follow_layer(s, n, None);
    }
}

fn remove(s: &mut Session, p: &Value) -> CmdResult {
    let ids = layer_ids(s, p)?;
    let m = s.map_mut();
    // The layer that takes the place of the topmost one removed becomes the active layer.
    let at = ids.iter().filter_map(|id| m.layer_index(*id)).min().ok_or("no such layer")?;
    let mut names = Vec::new();
    for id in ids {
        if let Some(i) = m.layer_index(id) {
            names.push(m.layers.remove(i).name);
            m.set_group(id, None);
        }
    }
    let next = m.layers.get(at.min(m.layers.len().saturating_sub(1))).map(|l| l.id);
    s.set_layer_selection(next.into_iter().collect(), next);
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
        let run: Vec<usize> =
            m.layers.iter().enumerate().filter(|(_, l)| m.group_of(l.id).is_some_and(|x| x.id == g)).map(|(i, _)| i).collect();
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
    let method = ps(p, "method").and_then(ClassMethod::from_name).unwrap_or_default();
    let classes = f(p, "classes").map(|c| c.clamp(1.0, 12.0) as usize).unwrap_or(5);
    let l = s.map().layer(id).ok_or("no such layer")?;
    let g = l.geometry_type();
    let base = l.renderer.base_symbol();
    let base = if matches!(l.renderer, Renderer::Graduated { .. } | Renderer::Unique { .. }) {
        rustgis_map::Symbol { ..rustgis_map::Symbol::default_for(g, id) }.with_fill(base.fill, g)
    } else {
        base
    };
    let field = || -> Result<String, String> {
        if let Some(x) = ps(p, "field") {
            return Ok(x.to_string());
        }
        l.renderer
            .field()
            .map(str::to_string)
            .or_else(|| l.dataset.fields.iter().find(|f| f.ty.is_numeric()).map(|f| f.name.clone()))
            .ok_or_else(|| "field is required".into())
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
    let mut r = match kind.as_str() {
        "single" | "singlesymbol" => Renderer::Single(base),
        "unique" | "uniquevalues" => Renderer::unique(&l.dataset, &rows()?, &field()?, ramp.unwrap_or(Ramp::Harbour), base)?,
        "graduated" | "graduatedcolors" => {
            Renderer::graduated(&l.dataset, &rows()?, &field()?, classes, method, ramp.unwrap_or(Ramp::Ember), base)?
        }
        "proportional" | "proportionalsymbols" => {
            if g != GeometryType::Point {
                return Err("Proportional Symbols needs a point layer".into());
            }
            Renderer::proportional(&l.dataset, &rows()?, &field()?, base)?
        }
        other => return Err(format!("unknown symbology '{other}'")),
    };
    // Applied again on the same field, the classes keep the labels set by hand, and the colours
    // and outlines set by hand too while the colour scheme stays (#314).
    r.carry_edits_from(&l.renderer, g);
    with(s, p, |l| l.renderer = r)
}

/// `layer.classStyle`: one class's colour, outline colour and legend label (#314).
fn class_style(s: &mut Session, p: &Value) -> CmdResult {
    let other = b(p, "other").unwrap_or(false);
    let index = match p.get("index") {
        Some(_) => Some(uint(p, "index").ok_or("index must be a whole number from 0")?),
        None => None,
    };
    let class = match (p.get("value"), index, other) {
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
    };
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
