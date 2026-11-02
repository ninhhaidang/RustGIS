//! QGIS projects and layer styles: `project.importQgis` opens a `.qgs` (or `.qgz`) as a new map,
//! with its layers, their order, visibility and styles, its coordinate system and its view;
//! `style.importQgis` gives a layer a `.qml` style. Both say what they couldn't carry over
//! (`rustgis_map::qgis::Report`).

use rustgis_geom::{Coord, Extent};
use rustgis_map::qgis::{self, NoteKind, Report, Source};
use serde_json::{Value, json};

use super::{need_layer, ps};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("project.importQgis", "Import QGIS Project", "Project › Import", import_project).params(
            r#"{"path"?: "*.qgs|*.qgz", "text"?: "a .qgs file's text", "data"?: "a .qgs or .qgz file, base64", "dir"?: "the folder its relative paths start from (default: the file's)"} → the layers added and notes on what wasn't carried over"#,
        ),
        C::new("style.importQgis", "Import QGIS Style", "Style › Styles", import_style).when(need_layer).params(
            r#"{"layer"?, "path"?: "*.qml", "text"?: "a .qml file's text", "data"?: "a .qml file, base64"} → notes on what wasn't carried over"#,
        ),
    ]
}

/// A QGIS file's text: a `.qgs` or `.qml` as it is, a `.qgz` (a zip archive) its `.qgs`.
fn qgis_text(p: &Value) -> Result<String, String> {
    let bytes = match (ps(p, "data"), ps(p, "text"), ps(p, "path")) {
        (Some(d), ..) => {
            // Base64 is 4 characters for 3 bytes: a payload past the file limit isn't decoded.
            if d.len() as u64 > MAX_FILE / 3 * 4 + 4 {
                return Err("data is too large for a QGIS file".into());
            }
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.decode(d.trim()).map_err(|_| "data must be base64".to_string())?
        }
        (None, Some(t), _) if t.len() > MAX_XML => return Err("text is too large for a QGIS project or style".into()),
        (None, Some(t), _) => return Ok(t.to_string()),
        (None, None, Some(path)) => read_file(path)?,
        _ => return Err("path, text or data is required".into()),
    };
    if bytes.starts_with(b"PK") {
        let entries = rustgis_data::zip::read_matching_within(&bytes, |n| n.to_ascii_lowercase().ends_with(".qgs"), MAX_XML)?;
        let (_, qgs) = entries.into_iter().next().ok_or("the .qgz holds no .qgs project")?;
        return String::from_utf8(qgs).map_err(|_| "the .qgz's project isn't text".to_string());
    }
    if bytes.len() > MAX_XML {
        return Err("too large for a QGIS project or style".into());
    }
    String::from_utf8(bytes).map_err(|_| "not a QGIS file (it isn't text)".to_string())
}

/// The most bytes of a QGIS file read (a `.qgz` can carry data files beside its project).
const MAX_FILE: u64 = 256 << 20;
/// The most bytes of a project's or style's XML parsed: its tree is several times larger than
/// the text, and real ones are a few megabytes.
const MAX_XML: usize = 64 << 20;

/// Reads at most [`MAX_FILE`] bytes of `path` (natively: the web app passes `text` or `data`).
fn read_file(path: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path).and_then(|f| f.take(MAX_FILE + 1).read_to_end(&mut bytes)).map_err(|e| format!("{path}: {e}"))?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(format!("{path} is too large for a QGIS project"));
    }
    Ok(bytes)
}

fn import_project(s: &mut Session, p: &Value) -> CmdResult {
    let text = qgis_text(p)?;
    // The folder relative paths start from: `dir` (a blank one is none, so they don't start
    // from the working directory), else the file's own (`.` for a file named without one).
    let dir = ps(p, "dir").filter(|d| !d.trim().is_empty()).map(str::to_string).or_else(|| {
        ps(p, "path").and_then(|path| std::path::Path::new(path).parent()).map(|d| match d.to_string_lossy() {
            d if d.is_empty() => ".".to_string(),
            d => d.into_owned(),
        })
    });
    let mut report = Report::default();
    let project = qgis::read_project(&text, dir.as_deref().unwrap_or_default(), &mut report)?;
    // A map of its own, named as the project.
    let name = if project.title.is_empty() { "QGIS project" } else { project.title.as_str() };
    s.run_part("map.new", &json!({"name": name}))?;
    // QGIS draws a project on its canvas alone: no basemap, no graticule.
    let m = s.map_mut();
    m.basemap = rustgis_map::Basemap::Blank;
    m.graticule = false;
    let degrees = project.crs.as_deref().and_then(Geographic::of);
    if let Some(crs) = &project.crs {
        let code = match crs.as_str() {
            "EPSG:3857" | "EPSG:900913" => "mercator",
            // QGIS draws a project in degrees as plate carrée: longitude and latitude as x and y.
            _ if degrees.is_some() => "equirectangular",
            other => other,
        };
        match s.run_part("map.projection", &json!({"type": code})) {
            Err(e) => report.approximated("", format!("the project's coordinate system {crs} ({e}), drawn in Web Mercator")),
            Ok(_) if degrees.is_some_and(|g| !g.wgs84 || !g.greenwich) => {
                let meridian = if degrees.is_some_and(|g| !g.greenwich) { ", its longitudes taken as from Greenwich" } else { "" };
                report.approximated("", format!("the project's coordinate system {crs}, drawn in Equirectangular on WGS 84{meridian}"));
            }
            Ok(_) => {}
        }
    }
    // Bottom first, so each is added on top of the ones below it.
    let mut added = Vec::new();
    for layer in project.layers.iter().rev() {
        match add_layer(s, layer, dir.is_some(), &mut report) {
            Ok(Some(name)) => added.push(name),
            Ok(None) => {}
            Err(e) => report.skipped(&layer.name, format!("the layer: {e}")),
        }
    }
    go_to_view(s, &project, degrees);
    let notes: Vec<Value> = report.notes.iter().map(note_json).collect();
    if !report.notes.is_empty() {
        let left = report.notes.iter().filter(|n| n.kind == NoteKind::Skipped).count();
        s.notify(
            format!(
                "Imported {} of {} QGIS layers; {} things drawn differently, {left} left out (see the import's notes)",
                added.len(),
                project.layers.len(),
                report.notes.len() - left
            ),
            false,
        );
    }
    Ok(json!({"map": name, "layers": added, "notes": notes}))
}

/// Adds one QGIS layer to the active map with its style: `Ok(Some(name))` when added, `Ok(None)`
/// when its source is one RustGIS doesn't read (noted). Without the project's folder (`has_dir`:
/// a project given as text or data with no `dir`), a relative path has nothing to start from and
/// is noted rather than looked for in the working directory.
fn add_layer(s: &mut Session, layer: &qgis::ProjectLayer, has_dir: bool, report: &mut Report) -> Result<Option<String>, String> {
    let name = layer.name.as_str();
    if let Source::File { path, .. } | Source::Csv { path, .. } = &layer.source {
        if !has_dir && qgis::is_relative(path) {
            report.skipped(name, format!("the file {path}, relative to the project's folder, which the import wasn't given (pass dir)"));
            return Ok(None);
        }
        // `C:roads.gpkg`: relative to the working directory on drive C, which no folder fixes.
        if qgis::is_drive_relative(path) {
            report.skipped(name, format!("the file {path}, relative to a drive's working directory (give a full path in QGIS)"));
            return Ok(None);
        }
    }
    match &layer.source {
        Source::File { path, layer: table, ignored, .. } => {
            if !ignored.is_empty() {
                // `layerid=2`: RustGIS opens a layer by name, so without one every layer of a file
                // that has several comes in; with one, the options are only not read.
                let why = if table.is_some() { "not read" } else { "not read: a file with several layers adds them all" };
                report.approximated(name, format!("its source options {} ({why})", ignored.join(", ")));
            }
            let mut q = json!({"path": path, "name": name});
            if let Some(t) = table {
                q["layer"] = json!(t);
            }
            s.run_part("layer.addData", &q)?;
        }
        Source::Csv { path, x, y, wkt, crs } => {
            let mut q = json!({"path": path, "name": name});
            for (k, v) in [("x", x), ("y", y), ("wkt", wkt), ("crs", crs)] {
                if let Some(v) = v {
                    q[k] = json!(v);
                }
            }
            s.run_part("layer.addData", &q)?;
        }
        Source::Xyz { url, zmin, zmax, vector } => {
            // An overlay, as a WMS layer is, so web layers keep the tree's order between them.
            let mut q = json!({"url": url, "name": name, "kind": if *vector { "vector" } else { "raster" }, "overlay": true});
            if let Some(z) = zmin {
                q["minZoom"] = json!(z);
            }
            if let Some(z) = zmax {
                q["maxZoom"] = json!(z);
            }
            let before = s.active_layer;
            let added = s.run_part("map.addTiles", &q)?;
            web_settings(s, &added, before, layer, report);
            return Ok(Some(name.to_string()));
        }
        Source::Wms { url, layer: wms, .. } => {
            let before = s.active_layer;
            let added = s.run_part("service.add", &json!({"url": url, "type": "wms", "layer": wms, "name": name}))?;
            web_settings(s, &added, before, layer, report);
            return Ok(Some(name.to_string()));
        }
        Source::Wfs { url, layer: typename } => {
            report.skipped(name, format!("the WFS layer {typename} at {url} (add it with Home › Add › Web Data)"));
            return Ok(None);
        }
        Source::Postgres { connection, table } => {
            report.skipped(name, format!("the PostGIS table {table} ({connection}): connect to the database to add it"));
            return Ok(None);
        }
        Source::Other { provider } => {
            // A layer of no provider (an annotation layer, a group layer) is told by its kind.
            let what = if provider.is_empty() { format!("{} layers", layer.kind) } else { format!("{provider} sources") };
            report.skipped(name, format!("the layer: RustGIS doesn't read QGIS's {what}"));
            return Ok(None);
        }
    }
    let id = s.active_layer.ok_or("the layer wasn't added")?;
    let Some(l) = s.map_mut().layer_mut(id) else { return Err("the layer wasn't added".into()) };
    let added_name = l.name.clone();
    l.visible = layer.visible;
    if l.raster.is_some() {
        // A raster takes its opacity, blending, scale range and colouring (when its bands are
        // there).
        if let Some(why) = l.raster.as_ref().and_then(|r| layer.style.raster_misfit(r)) {
            report.skipped(name, why);
        }
        layer.style.apply(l);
        return Ok(Some(added_name));
    }
    layer.style.apply(l);
    // What reads only against the layer's fields: its definition query, and what the style names.
    if let Some(q) = &layer.definition_query {
        match parse_imported(q, &l.dataset) {
            Ok(_) => l.definition_query = Some(q.clone()),
            Err(e) => report.skipped(name, format!("its filter `{q}` ({e})")),
        }
    }
    check_fields(l, name, &layer.style.rules, report);
    Ok(Some(added_name))
}

/// An imported QGIS expression read on `ds`. QGIS measures `$area`, `$perimeter` and `$length`
/// in the project's units, which aren't read yet, so an expression using them doesn't read.
fn parse_imported(text: &str, ds: &rustgis_data::Dataset) -> Result<rustgis_data::query::Expr, String> {
    if qgis::reads_measure(text) {
        return Err("it measures in the QGIS project's units, which RustGIS doesn't read yet".into());
    }
    rustgis_data::query::parse(text, ds)
}

/// Checks what a style names against the layer's fields: its label classes' clauses and fields
/// (a class that can't label is switched off) and the field a Unique Values or Graduated Colors
/// renderer classes by.
fn check_fields(l: &mut rustgis_map::Layer, name: &str, rules: &[qgis::RuleFilter], report: &mut Report) {
    for c in std::iter::once(&mut l.labels.default).chain(l.labels.classes.iter_mut()) {
        if let Some(w) = c.where_.clone()
            && let Err(e) = parse_imported(&w, &l.dataset)
        {
            report.skipped(name, format!("the label class \"{}\", whose clause `{w}` doesn't fit the layer ({e})", c.name));
            c.visible = false;
        }
        // Its data-defined text, size, colour and angle: an expression that doesn't read here is
        // left out, as the symbol's are below.
        for (what, slot) in [("text", &mut c.by.text), ("size", &mut c.by.size), ("colour", &mut c.by.color), ("angle", &mut c.by.angle)] {
            if let Some(text) = slot.as_deref()
                && let Err(e) = parse_imported(text, &l.dataset)
            {
                report.skipped(name, format!("the label {what} `{text}` of the class \"{}\" ({e})", c.name));
                *slot = None;
            }
        }
        // (A class with no field set labels nothing, and isn't noted; one whose text is an
        // expression needs none.)
        if c.visible && c.by.text.is_none() && !c.field.is_empty() && l.dataset.field_index(&c.field).is_none() {
            report.skipped(name, format!("labels from the field {}, which the layer hasn't", c.field));
            c.visible = false;
        }
    }
    // Classes by expression (the style's rule-based renderer's): a rule whose filter doesn't read
    // here is left out, the rest kept.
    if l.renderer.expression().is_some() && !rules.is_empty() {
        let kept = fit_rules(l, name, rules, report);
        note_overlapping_rules(l, name, &kept, report);
    }
    let field = match &l.renderer {
        rustgis_map::Renderer::Unique { expression: Some(_), .. } => None,
        rustgis_map::Renderer::Unique { field, .. } | rustgis_map::Renderer::Graduated { field, .. } => Some(field.clone()),
        _ => None,
    };
    if let Some(f) = field.filter(|f| l.dataset.field_index(f).is_none()) {
        report.approximated(name, format!("classes by the field {f}, which the layer hasn't: every feature draws as the rest"));
    }
    // The symbol's data-defined settings: an expression that doesn't read here (a QGIS function
    // or variable RustGIS lacks, a field the layer hasn't) is left out.
    let by = &mut l.symbol_by;
    for (what, slot) in [
        ("size", &mut by.size),
        ("fill colour", &mut by.fill),
        ("outline colour", &mut by.stroke),
        ("outline width", &mut by.stroke_width),
        ("angle", &mut by.angle),
    ] {
        if let Some(text) = slot.as_deref()
            && let Err(e) = parse_imported(text, &l.dataset)
        {
            report.skipped(name, format!("its data-defined symbol {what} `{text}` ({e})"));
            *slot = None;
        }
    }
}

/// Leaves the rules of a rule-based renderer (classed by expression) that don't read on `l`'s
/// fields out of its classes, each noted; none left: the first rule's symbol for every feature,
/// noted. The rules kept.
fn fit_rules(l: &mut rustgis_map::Layer, name: &str, rules: &[qgis::RuleFilter], report: &mut Report) -> Vec<qgis::RuleFilter> {
    let mut kept = Vec::new();
    // Those that don't fit, said once: how many, and the first.
    let mut unfit: Option<(String, usize)> = None;
    for r in rules {
        match parse_imported(&r.when, &l.dataset) {
            Ok(_) => kept.push(r.clone()),
            Err(e) => match &mut unfit {
                Some((_, n)) => *n += 1,
                None => unfit = Some((format!("\"{}\", whose filter `{}` doesn't fit the layer ({e})", r.key, r.condition), 1)),
            },
        }
    }
    if let Some((first, n)) = unfit {
        let what =
            if n == 1 { format!("the rule {first}") } else { format!("{n} rules whose filters don't fit the layer, the first {first}") };
        report.skipped(name, what);
    }
    if kept.len() == rules.len() {
        return kept;
    }
    let rustgis_map::Renderer::Unique { classes, class_strokes, class_fills, expression, .. } = &mut l.renderer else { return kept };
    if kept.is_empty() {
        // As before rules were classed: the first rule's symbol, strokes and fills.
        let first = rules.first().and_then(|r| classes.iter().find(|c| c.0 == r.key)).cloned();
        if let Some((key, sym)) = first {
            let strokes = class_strokes.get(&key).cloned().unwrap_or_default();
            let fills = class_fills.get(&key).cloned().unwrap_or_default();
            l.renderer = rustgis_map::Renderer::Single(sym);
            l.line_strokes = strokes;
            l.fill_layers = fills;
        }
        report.approximated(name, "its rules, none of whose filters fit the layer, drawn with the first rule's symbol");
        return kept;
    }
    let keys: std::collections::HashSet<&str> = kept.iter().map(|r| r.key.as_str()).collect();
    classes.retain(|c| keys.contains(c.0.as_str()));
    class_strokes.retain(|k, _| keys.contains(k.as_str()));
    class_fills.retain(|k, _| keys.contains(k.as_str()));
    *expression = Some(qgis::rules_expression(&kept));
    kept
}

/// The most rule checks counted (rules × features): past it, the features that several rules
/// pick are noted as a possibility.
const MAX_RULE_CHECKS: usize = 20_000_000;

/// Notes the features of `l` that more than one of a rule-based renderer's `rules` picks at some
/// scale: QGIS draws such a feature with each rule's symbol, RustGIS with the first's.
fn note_overlapping_rules(l: &rustgis_map::Layer, name: &str, rules: &[qgis::RuleFilter], report: &mut Report) {
    let ds = &l.dataset;
    let mut compiled: Vec<_> = rules.iter().filter_map(|r| Some((r, parse_imported(&r.condition, ds).ok()?))).collect();
    // Only rules some scale draws together can pick a feature together: in the order of where
    // they start zoomed in, a rule meets one before it when it starts within the farthest out
    // any of those reaches (a sweep over their scale ranges).
    compiled.sort_by(|a, b| a.0.inside.unwrap_or(0.0).total_cmp(&b.0.inside.unwrap_or(0.0)));
    if !together(compiled.iter().map(|c| c.0)) {
        return;
    }
    let what = "each with the first rule that picks it, where QGIS draws it with every one";
    // Rules read at no map scale can't be counted for one that reads it (an else rule beside a
    // rule with a scale range does too).
    let scaled = compiled.iter().any(|(r, _)| qgis::reads_scale(&r.condition));
    if scaled || compiled.len().saturating_mul(ds.features.len()) > MAX_RULE_CHECKS || l.source.is_some() {
        report.approximated(name, format!("features that more than one of its rules picks (if any), drawn {what}"));
        return;
    }
    let env = rustgis_data::query::Env::default();
    // (Only the features its definition query keeps are drawn.)
    let shown = l.visible_rows();
    let n = (0..ds.features.len())
        .filter(|row| shown.get(*row).copied().unwrap_or(false) && ds.features.get(*row).is_some_and(|f| f.geometry.is_some()))
        .filter(|row| together(compiled.iter().filter(|(_, e)| rustgis_data::query::matches(e, ds, *row, &env)).map(|c| c.0)))
        .count();
    if n > 0 {
        report.approximated(name, format!("{n} features that more than one of its rules picks, drawn {what}"));
    }
}

/// Whether two of `rules` (in the order of where they start zoomed in) are drawn at some scale
/// together: one starts within the farthest out any rule before it reaches.
fn together<'a>(rules: impl Iterator<Item = &'a qgis::RuleFilter>) -> bool {
    let mut reach: Option<f64> = None;
    // (A range that starts in beyond where it ends out is never drawn.)
    for r in rules.filter(|r| r.inside.unwrap_or(0.0) <= r.out.unwrap_or(f64::INFINITY)) {
        if reach.is_some_and(|out| r.inside.unwrap_or(0.0) <= out) {
            return true;
        }
        reach = Some(reach.unwrap_or(f64::NEG_INFINITY).max(r.out.unwrap_or(f64::INFINITY)));
    }
    false
}

/// Gives what a QGIS web layer was added as the layer's visibility, opacity and blending mode.
/// `added` is what `map.addTiles` or `service.add` answered: a tile layer's `index`, or no index
/// when the service offered the layer as features or a coverage, which come in as a map layer
/// (the active one, if it isn't `before` any more), which takes the scale range too. A tile layer
/// has no scale range, so one is noted.
pub(crate) fn web_settings(s: &mut Session, added: &Value, before: Option<u32>, layer: &qgis::ProjectLayer, report: &mut Report) {
    let name = layer.name.as_str();
    let st = &layer.style;
    let transparency = st.transparency.filter(|t| t.is_finite() && *t > 0.0);
    let index = added.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok());
    if index.is_none()
        && let Some(id) = s.active_layer.filter(|id| Some(*id) != before)
        && let Some(l) = s.map_mut().layer_mut(id)
    {
        l.visible = layer.visible;
        if let Some(tr) = transparency {
            l.transparency = tr.clamp(0.0, 100.0);
        }
        if let Some(blend) = st.blend {
            l.blend = blend;
        }
        if let Some(m) = st.min_scale {
            l.min_scale = m;
        }
        if let Some(m) = st.max_scale {
            l.max_scale = m;
        }
        return;
    }
    match index.and_then(|i| s.map_mut().tiles.get_mut(i)) {
        Some(t) => {
            t.visible = layer.visible;
            if let Some(blend) = st.blend {
                t.blend = blend;
            }
            if let Some(tr) = transparency {
                t.opacity = (1.0 - tr / 100.0).clamp(0.0, 1.0);
            }
        }
        None => {
            if !layer.visible {
                report.approximated(name, "a hidden web layer, added shown");
            }
            if transparency.is_some() {
                report.approximated(name, "its transparency: added opaque");
            }
            if st.blend.is_some_and(|b| b != rustgis_map::effects::Blend::default()) {
                report.approximated(name, "its blending mode: added as normal");
            }
        }
    }
    if st.min_scale.flatten().is_some() || st.max_scale.flatten().is_some() {
        report.approximated(name, "its scale range: a web layer draws at every scale");
    }
}

/// Goes to the project's saved view, or to its layers when that view shows none of them.
fn go_to_view(s: &mut Session, project: &qgis::Project, degrees: Option<Geographic>) {
    let data = s.map().full_extent();
    let saved = project.extent.and_then(|[x0, y0, x1, y1]| match degrees {
        // In degrees already (from Greenwich: the view of a project on another prime meridian
        // is left to its data).
        Some(g) if g.greenwich => Some(Extent::new(x0.max(-180.0), y0.max(-90.0), x1.min(180.0), y1.min(90.0))).filter(|e| !e.is_empty()),
        Some(_) => None,
        None => {
            let m = s.map();
            let corners = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|(x, y)| m.to_lonlat(Coord::new(x, y)));
            corners
                .iter()
                .flatten()
                .fold(None, |e: Option<Extent>, c| Some(e.unwrap_or(Extent::EMPTY).union(&Extent::new(c.x, c.y, c.x, c.y))))
        }
    });
    let target = match saved {
        Some(e) if !e.is_empty() && (data.is_empty() || e.intersects(&data)) => e,
        _ => data,
    };
    if !target.is_empty() {
        s.zoom_to(&target);
    }
}

/// A coordinate system in degrees (EPSG:4326, ETRS89, NAD83, …), which QGIS draws as plate carrée.
#[derive(Clone, Copy, Debug)]
struct Geographic {
    /// On WGS 84, or a datum PROJ takes as the same (no shift to WGS 84).
    wgs84: bool,
    /// Longitudes from Greenwich (not from another prime meridian, such as Paris's).
    greenwich: bool,
}

impl Geographic {
    /// `None` for a coordinate system that isn't in degrees, or isn't known: an `EPSG:` code,
    /// `OGC:CRS84`, or a PROJ string (a project QGIS saved without an authority code).
    fn of(crs: &str) -> Option<Geographic> {
        if crs.eq_ignore_ascii_case("OGC:CRS84") {
            return Some(Geographic { wgs84: true, greenwich: true });
        }
        let def = if crs.trim_start().starts_with('+') {
            crs
        } else {
            let code = crs.strip_prefix("EPSG:")?.trim().parse::<u16>().ok()?;
            rustgis_data::raster::crs::epsg_proj4(code)?
        };
        let has = |p: &str| def.split_whitespace().any(|t| t == p);
        ["+proj=longlat", "+proj=latlong", "+proj=lonlat", "+proj=latlon"].iter().any(|p| has(p)).then(|| Geographic {
            wgs84: has("+datum=WGS84") || has("+datum=NAD83") || has("+towgs84=0,0,0,0,0,0,0") || has("+towgs84=0,0,0"),
            // `+pm=` names a meridian or gives its offset in degrees (`0`, `0.0`: Greenwich).
            greenwich: def
                .split_whitespace()
                .filter_map(|t| t.strip_prefix("+pm="))
                .all(|pm| pm.eq_ignore_ascii_case("greenwich") || pm.parse::<f64>().is_ok_and(|d| d == 0.0)),
        })
    }
}

fn note_json(n: &qgis::Note) -> Value {
    json!({"layer": n.layer, "kind": n.kind, "text": n.text, "count": n.count})
}

fn import_style(s: &mut Session, p: &Value) -> CmdResult {
    let text = qgis_text(p)?;
    let id = s.layer_id(p)?;
    let name = s.map().layer(id).map(|l| l.name.clone()).ok_or("no such layer")?;
    let mut report = Report::default();
    let style = qgis::read_style(&text, &name, &mut report)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    if l.raster.is_some() {
        // A style that doesn't fit leaves the raster as it was, its opacity and scale range too.
        if let Some(why) = l.raster.as_ref().and_then(|r| style.raster_misfit(r)) {
            report.skipped(&name, why);
        } else {
            // Pixels still on their way (a remote raster) can't be checked: the style is applied,
            // and a band it reads that the file hasn't is drawn from the file's last band.
            if style.raster.is_some() && l.raster.as_ref().is_some_and(|r| r.data.is_none()) {
                report.approximated(
                    &name,
                    "its raster colouring, applied before the raster's bands could be checked (a band the file hasn't is drawn from its last)",
                );
            }
            if style.raster.is_none() && style.renderer.is_some() {
                report.skipped(
                    &name,
                    "a feature layer's symbols and labels, which a raster hasn't (only its opacity and scale range are taken)",
                );
            }
            style.apply(l);
        }
    } else {
        if style.raster.is_some() && style.renderer.is_none() {
            report.skipped(&name, "a raster's colouring, which a feature layer hasn't (only its opacity and scale range are taken)");
        }
        style.apply(l);
        check_fields(l, &name, &style.rules, &mut report);
    }
    Ok(json!({"notes": report.notes.iter().map(note_json).collect::<Vec<_>>()}))
}
