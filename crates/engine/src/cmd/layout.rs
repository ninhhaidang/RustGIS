//! Home › Project (New Layout, Import Layout), Publish › Map Frames, Map Surrounds and Text, and
//! Publish › Output (Export Layout) and Save As (Layout File): page layouts.

use std::path::Path;

use rustgis_geom::{Coord, Extent};
use rustgis_map::Rgba;
use rustgis_map::layout::{
    Arrange, ArrowStyle, CoordFormat, ElementKind, FontFamily, FrameShape, Grid, GridEdges, GridKind, GridStyle, Guides, Layout, LineDir,
    LineHead, MAX_COLORBAR_TICKS, MAX_GRID_WIDTH, MAX_GUIDES, MAX_LEGEND_COLUMNS, MAX_SCALE_DIVISIONS, MAX_TICK_MM, MIN_ELEMENT_MM,
    MapSeries, Orientation, PAGES, ScaleBarStyle, ScaleUnits, TextAlign, TickSide, fit_scale, line_angle, line_at_angle, line_length,
};
use serde_json::{Value, json};

use super::{b, f, numbered, ps};
use crate::{CmdResult, CommandSpec as C, Session};

/// Most compressed page data a PDF export holds (less in the browser).
const PDF_BUDGET: usize = if cfg!(target_arch = "wasm32") { 256 << 20 } else { 1 << 30 };

/// Layout files: JSON with this extension.
pub const LAYOUT_EXTENSION: &str = "rgislayout";
/// Largest layout file read (64 MiB, pictures included).
const MAX_LAYOUT: u64 = 64 << 20;

pub fn commands() -> Vec<C> {
    vec![
        C::new("layout.new", "New Layout", "Home › Project", new_layout).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"name"?: string, "page"?: "letter"|"legal"|"tabloid"|"a5"|"a4"|"a3"|"a2"|"a1"|"a0", "orientation"?: "landscape"|"portrait", "width"?: mm, "height"?: mm, "template"?: "map" (frame, title, legend, north arrow, scale bar; the default)|"blank"}"#,
        ),
        C::new("layout.publication", "New Publication Layout", "Publish › Layout", publication).inputs(publication_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"profile"?: "a4" (the default) | "a3" | "a2" (a sheet turned to portrait or landscape, whichever the area's map comes out larger on: its extent east to west against north to south, on the map's projection) | "a4-portrait"|"a4-landscape"|"a3-portrait"|"a3-landscape"|"a2-portrait"|"a2-landscape" (a sheet: frame, title, subtitle, the map, a boxed footer)|"figure-1col" (90 mm)|"figure-1.5col" (140 mm)|"figure-2col" (190 mm)|"thesis-full" (160 × 225 mm)|"thesis-half" (160 × 110 mm) (a figure: the map edge to edge, its title and sources left to the caption), "extent"?: [west, south, east, north] in degrees that must fit (default: the map view), "name"?: string, "title"?: text, "subtitle"?: text, "sources"?: text | [texts], "reference"?: text | [texts] (default: the projection, datum and graticule), "note"?: text, "author"?: text (default: "Made with RustGIS · {date}"; "" or [] for none), "headings"?: [the sources' heading, the reference's heading, an editorial column's heading], "locator"?: "globe" (a globe with the area outlined; the default on sheets) | "inset" (the surrounding region, the area outlined) | "none" (the default on figures), "decimal"?: "." (the default) | "," (graticule, scale bar and colorbar numbers and {scale} with a decimal comma: 0,05 and 1:6.300.000), "colorbar"?: "auto" (the default: the top visible raster with a colour scale gets one, under the legend in its box) | "none" (no colorbar, and the legend's box has no room for one), "style"?: "classic" (the default: the title over the map, the footer under it) | "banner" (the title and subtitle in a band of the accent colour across the top) | "panel" (the map at the left, a panel at the right with the title, locator, legend, sources and reference) | "editorial" (the title large in serif, a column at the right with the note, the locator and the legend) | "modular" (cards on a light page: the map, the locator and the legend each in one, the sources and reference in tinted ones) | "poster" (the page in the accent colour, the title in light text, the map in a light card, the footer in darker blocks) (a sheet's arrangement; figures leave it to the caption), "accent"?: a colour as #RRGGBB (the style's colour: its band, panel tint, headings and rules, a poster's page), "arrangement"?: "best" (the default: the favourite and each arrangement by name laid out, the one moving the least area off its corners kept, then the one leaving the least over the data, the favourite on a tie) | "favourite" (layout.arrangement's) | an arrangement by name or corner by corner, as layout.arrangement takes (the corners the locator, north arrow, legend and scale bar are at home in; one covering what the map draws there still moves)} → a new layout of the active map at a round scale, its texts wrapped to their boxes; {projection}, {datum} and {grid} in the texts are filled from the map"#,
        ),
        C::new("layout.arrangement", "Favourite Arrangement", "Publish › Layout", arrangement).roles(&[]).effects(crate::policy::Effects::NONE).view().params(
            r#"{"favourite"?: "classic" (the locator top left, the north arrow top right, the legend bottom left, the scale bar bottom right) | "mirrored" | "legendRight" | "legendTop" | {"locator", "northArrow", "legend", "scaleBar": "top-left"|"top-right"|"bottom-left"|"bottom-right", each a corner of its own}} → the favourite arrangement of publication layouts (kept with the app's preferences), its corners and the arrangements by name; with no favourite given, it is only shown"#,
        ),
        C::new("layout.activate", "Open Layout", "Browser", |s, p| {
            let i = index(s, &by_name(p))?;
            s.active_layout = Some(i);
            Ok(json!({"layout": i}))
        }).inputs(named_object_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .params(r#"{"name": string | "layout": name|index}"#),
        C::new("layout.close", "Show Map", "Browser", |s, _| {
            s.active_layout = None;
            Ok(Value::Null)
        }).inputs(close_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        C::new("layout.remove", "Delete Layout", "Browser", |s, p| {
            let i = index(s, &by_name(p))?;
            s.project.layouts.remove(i);
            // Deleting another layout keeps the one open (a layout tab's close button).
            s.active_layout = match s.active_layout {
                Some(a) if a > i => Some(a - 1),
                Some(a) if a < i => Some(a),
                _ => None,
            };
            Ok(Value::Null)
        }).inputs(named_object_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name"?: string, "layout"?: name|index (instead of name; default: the open layout)}"#),
        C::new("layout.rename", "Rename Layout", "Browser", |s, p| {
            let name = ps(p, "newName").map(str::trim).filter(|n| !n.is_empty()).ok_or("newName is required")?;
            let i = index(s, &by_name(p))?;
            // Unique among the other layouts, so names keep finding one layout.
            let unique = numbered(name, |n| s.project.layouts.iter().enumerate().any(|(j, l)| j != i && l.name.eq_ignore_ascii_case(n)));
            if let Some(l) = s.project.layouts.get_mut(i) {
                l.name = unique.clone();
            }
            Ok(json!({"name": unique}))
        }).inputs(named_object_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name"?: string, "layout"?: name|index (instead of name; default: the open layout), "newName": string}"#),
        // A copy of a layout with all its elements (#288), right after it and open. Element ids
        // are per layout, so legends and scale bars stay on their frames, which show the same maps.
        C::new("layout.duplicate", "Duplicate Layout", "Browser", |s, p| {
            let i = index(s, &by_name(p))?;
            let mut l = s.project.layouts.get(i).ok_or("open a layout first")?.clone();
            l.name = super::copy_name(ps(p, "newName"), &l.name, |n| s.project.layouts.iter().any(|x| x.name.eq_ignore_ascii_case(n)));
            let name = l.name.clone();
            if s.project.layouts.len() >= crate::project::MAX_LAYOUTS {
                return Err(format!("a project holds at most {} layouts; remove one first", crate::project::MAX_LAYOUTS));
            }
            s.project.layouts.insert(i + 1, l);
            s.active_layout = Some(i + 1);
            Ok(json!({"name": name, "layout": i + 1}))
        }).inputs(named_object_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name"?: string, "layout"?: name|index (instead of name; default: the open layout), "newName"?: string (default: "<layout> copy")}"#),
        C::new("layout.page", "Page Size", "Publish › Page Setup", page).inputs(object_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layout"?, "page"?: page id, "orientation"?: "landscape"|"portrait", "width"?: mm, "height"?: mm, "decimal"?: "."|"," (the decimal mark of the numbers its surrounds print)}"#,
        ),
        C::new("layout.addFrame", "Map Frame", "Publish › Map Frames", add_frame).inputs(add_frame_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layout"?, "shape"?: "rectangle"|"circle"|"ellipse"|"polygon"|"lasso", "points"?: [[u, v], …] (polygon/lasso, 0..1 across the frame), "map"?: name, "x"?, "y"?, "w"?, "h"?: mm}"#,
        ),
        C::new("layout.addElement", "Add Element", "Publish › Map Surrounds", add_element).inputs(element_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).params(
            r##"{"layout"?, "kind": "northArrow"|"scaleBar"|"legend"|"colorbar"|"text"|"dynamicText"|"picture"|"table"|"chart"|"rectangle" (a neatline framing the page unless any of x, y, w, h is given)|"ellipse"|"circle"|"line"|"arrow"|"extentIndicator"|"grid", "frame"?: element id (default: the first map frame), "of"?: frame id (extentIndicator, which also takes "color" and "width" in points, default red and 2), "text"?, "size"?, "color"?: "#RRGGBB"|"#RRGGBBAA" (text, dynamicText, line, arrow, rectangle, ellipse, circle: its outline; extentIndicator; grid; scaleBar; refused for other kinds), "bold"?: bool (text), "italic"?: bool, "align"?: "left"|"center"|"right", "letterSpacing"?: points, -5 to 50 (text, dynamicText), "font"?: "sans"|"serif"|an installed font family's name, as labels.fonts lists them (text, dynamicText; a family not installed here is drawn in its fallback, and a serif where none is installed in the sans, with a warning; refused for other kinds), "fallback"?: "sans"|"serif" (a named font's, where it isn't installed; default its own class, else sans), "title"?: legend title (default "Legend"), "allClasses"?: bool (legend: every class, not only those the frame draws), "hideOther"?: bool (legend: leave out <all other values>), "exclude"?: [layer names] (legend: layers it leaves out though the map draws them), "columns"?: 1..3 (legend: the columns its rows are laid in; default 1), "orientation"?: "horizontal"|"vertical" (colorbar), "ticks"?: 2..21 (colorbar: labelled values, ends included), "tickSide"?: "below"|"above"|"right"|"left" (colorbar), "reverse"?: bool (colorbar: high values first), "fill"?: "#RRGGBBAA" (rectangle, ellipse, circle, northArrow; scaleBar: the alternating bar's light divisions, default white; refused for other kinds), "width"?: outline width in points (rectangle, ellipse, circle; 0 for none) or line width in points (line, grid), "radius"?: mm, 0 to 50 (rectangle: rounded corners), "dir"?: "horizontal"|"vertical"|"falling"|"rising" (line), "head"?: "none"|"end"|"start"|"both" (line; default "end" for an arrow), "angle"?: degrees anticlockwise from pointing right, centred on the page unless x or y is given (line, arrow; it sets the box and the direction, so w, h and dir are not used with it), "length"?: mm (line, arrow), "image"?: base64, "path"?: picture file, "layer"?: (table, chart; colorbar: a raster layer, default the top visible one), "field"?, "label"?, "rows"?, "step"?: degrees, or a measured grid's units (metres) (0 picks one), "type"?: "graticule" (latitudes and longitudes, default) | "measured" (eastings and northings in a projected system, as ArcGIS Pro's measured grid), "crs"?: a measured grid's coordinate system: EPSG:25832, WKT or a PROJ string ("" or none: the map's when projected, else the UTM zone at the frame's centre), "short"?: bool (a measured grid's labels as their kilometres' last two digits, 43 for 543000), "labels"?: bool, "style"?: "lines"|"ticks"|"linesAndTicks" (grid) or "classic"|"compass"|"triangle"|"star" (northArrow) or "alternating"|"stepped"|"line" (scaleBar), "tickLength"?: mm, "format"?: "decimal"|"dms", "hemisphere"?: bool (N/S/E/W rather than a sign), "edges"?: ["top","bottom","left","right"] (labelled edges), "labelOrientation"?: "horizontal"|"vertical", "off"?: bool (grid; options not given keep the grid's), "outline"?: "#RRGGBB" (northArrow, or a rectangle's, ellipse's or circle's outline instead of color; refused for other kinds), "units"?: "auto"|"kilometers"|"meters"|"miles"|"feet", "divisions"?: 1..10 (scaleBar, whose "color" is its lines, dark divisions and labels, default #222222), "x"?, "y"?, "w"?, "h"?: mm}"##,
        ),
        C::new("layout.update", "Element Properties", "Publish › Element", update).inputs(update_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).params(
            r##"{"layout"?, "element": id, "x"?, "y"?, "w"?, "h"?: mm, "name"?, "visible"?, "locked"?: bool (a locked element takes no other change until unlocked; "locked": false unlocks it and may come with changes), "text"?, "size"?, "color"? (text, line, rectangle, ellipse: its outline; scaleBar: its lines, dark divisions and labels; refused for other elements), "bold"?, "italic"?, "align"?, "letterSpacing"?, "font"?: "sans"|"serif"|an installed font family's name, "fallback"?: "sans"|"serif" (text, as in layout.addElement; refused for other elements), "title"?, "allClasses"?: bool (legend), "hideOther"?: bool (legend), "exclude"?: [layer names] (legend; [] lists every layer again), "columns"?: 1..3 (legend), "orientation"?, "ticks"?, "tickSide"?, "reverse"?: (colorbar), "style"?: (northArrow, scaleBar), "fill"?, "outline"? (northArrow, rectangle, ellipse, and a scaleBar's fill; refused for other elements; for a rectangle or ellipse, color sets it when outline is not given; a scaleBar's fill is the alternating bar's light divisions), "radius"?: mm (rectangle: rounded corners), "width"?: points (rectangle, ellipse, line), "dir"?, "head"?, "angle"?, "length"? (line: turned or sized about its centre, then moved by any x or y; dir does not come with angle or length), "units"?, "divisions"? (scaleBar), "map"?, "scale"?, "center"?: [lon, lat], "bearing"?: degrees clockwise from north, "pitch"?: degrees of tilt (0 to 85), "grid"?: {grid options as in layout.addElement} | false (map frame), "extentIndicator"?: {"color"?, "width"?: points} | false (map frame: its extent indicator's style, or removes it), "layer"?: (colorbar: "" for the top visible raster), "field"?, "rows"?}"##,
        ),
        C::new("layout.guides", "Guides", "Publish › Page Setup", guides).inputs(object_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layout"?, "x"?: [mm] (vertical guides, from the page's left edge), "y"?: [mm] (horizontal guides, from its top edge), "clear"?: bool (remove every guide first)}"#,
        ),
        C::new("layout.duplicateElement", "Duplicate Element", "Publish › Element", duplicate_element).inputs(object_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layout"?, "element": id, "dx"?, "dy"?: mm (offset of the copy, default 5)}"#,
        ),
        C::new("layout.arrange", "Arrange Element", "Publish › Element", arrange).inputs(object_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layout"?, "element": id, "to": "front"|"forward"|"backward"|"back" (later elements are drawn over earlier ones: front puts it over every other, forward over the one above it, backward under the one below, back under every other)}"#,
        ),
        C::new("layout.removeElement", "Delete Element", "Publish › Element", |s, p| {
            let id = element_id(p)?;
            let l = active(s, p)?;
            // A frame's surrounds go with it: a locked one keeps the frame too.
            if let Some(locked) = l.locked_in_removal(id).map(|e| e.id) {
                unlocked(l, locked)?;
            }
            if l.remove(id) { Ok(Value::Null) } else { Err(format!("no element {id}")) }
        }).inputs(object_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"layout"?, "element": id}"#),
        C::new("layout.frameZoom", "Zoom Frame", "Publish › Map Frame", frame_zoom).inputs(frame_zoom_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layout"?, "element"?: frame id, "frame"?: (as element), "to"?: "view" (the map view's extent, bearing and pitch; the default)|"full"|"selection"|"layer", "layer"?}"#,
        ),
        C::new("layout.series", "Map Series", "Publish › Map Frames", series).inputs(series_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layout"?, "layer": index layer, "nameField"?: string, "frame"?: id, "margin"?: 0..1, "off"?: bool}"#,
        ),
        C::new("layout.seriesPage", "Map Series Page", "Publish › Map Frames", series_page).inputs(series_page_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layout"?, "page": number|"next"|"previous"}"#),
        C::new("layout.seriesRefresh", "Refresh Map Series", "Publish › Map Frames", |s, p| series_page(s, &json!({"layout": p.get("layout"), "page": "current"}))).inputs(series_page_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"layout"?}"#),
        C::new("layout.export", "Export Layout", "Publish › Output", |s, p| super::exports::write_here(s, p, "layout.export")).inputs(layout_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"layout"?, "path": "*.png|*.pdf|*.svg" (PNG/SVG: current page; SVG: each element a group of its own; text and shapes as SVG, map frames and surrounds as vectors, rasters and tiles in them as pictures at dpi), "dpi"?: 72..600, "allPages"?: bool (PDF only; default true: every map series page, up to 500; false: the current page), "georeference"?: bool (PDF only; default true: map frames carry their coordinates, a GeoPDF; false: none)}"#),
        C::new("layout.save", "Save Layout File", "Publish › Save As", |s, p| {
            let path = ps(p, "path").ok_or("path is required")?;
            let l = active(s, p)?.clone();
            let text = serde_json::to_string_pretty(&l).map_err(|e| e.to_string())?;
            rustgis_data::io::write_atomic(Path::new(path), text.as_bytes()).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!({"path": path}))
        }).inputs(object_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .view()
        .params(r#"{"layout"?, "path": "*.rgislayout"}"#),
        C::new("layout.import", "Import Layout", "Home › Project", |s, p| {
            let text = match (ps(p, "text"), ps(p, "path")) {
                (Some(t), _) => t.to_string(),
                (None, Some(path)) => {
                    let bytes = crate::project::read_file(std::path::Path::new(path), MAX_LAYOUT)?;
                    String::from_utf8(bytes).map_err(|_| format!("{path} isn't a RustGIS layout"))?
                }
                (None, None) => return Err("path or text is required".into()),
            };
            import(s, &text)
        }).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .params(r#"{"path"?: "*.rgislayout", "text"?: layout JSON}"#),
    ]
}

/// The layout named or numbered by `layout`, else the active one, else the only one.
fn index(input: &impl super::InputSource, p: &Value) -> Result<usize, String> {
    let context = input.input_context();
    let s = &context;
    let ls = &s.project.layouts;
    // A `layout` that is neither a name nor an index is an error, never the open layout.
    match p.get("layout") {
        None | Some(Value::Null) => {}
        Some(Value::String(n)) => {
            return ls.iter().position(|l| l.name.eq_ignore_ascii_case(n)).ok_or_else(|| format!("no layout '{n}'"));
        }
        Some(_) => {
            let i = super::uint(p, "layout").ok_or("layout must be a layout's name or index")?;
            return usize::try_from(i).ok().filter(|i| *i < ls.len()).ok_or_else(|| format!("no layout {i}"));
        }
    }
    s.active_layout
        .filter(|i| *i < ls.len())
        .or(if ls.len() == 1 { Some(0) } else { None })
        .ok_or_else(|| "open a layout first".to_string())
}

/// Parameters where `name` names the layout (Open, Delete and Rename Layout) as `layout`.
fn by_name(p: &Value) -> Value {
    match (ps(p, "name"), p.get("layout")) {
        (Some(n), None) => json!({"layout": n}),
        _ => p.clone(),
    }
}

fn active<'a>(s: &'a mut Session, p: &Value) -> Result<&'a mut Layout, String> {
    let i = index(s, p)?;
    s.project.layouts.get_mut(i).ok_or_else(|| "open a layout first".to_string())
}

fn element_id(p: &Value) -> Result<u32, String> {
    p.get("element").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()).ok_or_else(|| "element is required".to_string())
}

/// Moves an element in the layout's drawing order: a legend brought to the front is drawn over
/// the map frame under it, and is the one a click there picks.
fn arrange(s: &mut Session, p: &Value) -> CmdResult {
    let id = element_id(p)?;
    let to = ps(p, "to").and_then(Arrange::from_name).ok_or("to is front, forward, backward or back")?;
    let l = active(s, p)?;
    if l.element(id).is_none() {
        return Err(format!("no element {id}"));
    }
    unlocked(l, id)?;
    let before = l.draw_order(id).map(|(i, _)| i);
    let index = l.arrange(id, to).ok_or_else(|| format!("no element {id}"))?;
    let of = l.elements.len();
    // A move that changes nothing (front for the element already on top) is no step to undo.
    if before == Some(index) {
        s.request_only = true;
    }
    Ok(json!({"element": id, "index": index, "of": of}))
}

/// Refuses to change a locked element (#536).
fn unlocked(l: &Layout, id: u32) -> Result<(), String> {
    match l.element(id) {
        Some(e) if e.locked => Err(format!("'{}' is locked: unlock it first", e.name)),
        _ => Ok(()),
    }
}

fn unique_name(s: &Session, base: &str) -> String {
    let base = base.trim();
    let base = if base.is_empty() { "Layout" } else { base };
    numbered(base, |n| s.project.layouts.iter().any(|l| l.name.eq_ignore_ascii_case(n)))
}

/// (width, height) in mm from page/orientation/width/height parameters.
pub(crate) fn page_size(p: &Value, current: Option<(f64, f64)>) -> Result<(f64, f64), String> {
    let (mut w, mut h) = match ps(p, "page") {
        Some(id) => PAGES
            .iter()
            .find(|(pid, label, ..)| pid.eq_ignore_ascii_case(id) || label.eq_ignore_ascii_case(id))
            .map(|(_, _, w, h)| (*w, *h))
            .ok_or_else(|| format!("unknown page size '{id}'"))?,
        None => current.unwrap_or((215.9, 279.4)),
    };
    if let Some(v) = f(p, "width") {
        w = v;
    }
    if let Some(v) = f(p, "height") {
        h = v;
    }
    match ps(p, "orientation") {
        Some("landscape") if h > w => std::mem::swap(&mut w, &mut h),
        Some("portrait") if w > h => std::mem::swap(&mut w, &mut h),
        Some("landscape" | "portrait") | None => {}
        Some(o) => return Err(format!("unknown orientation '{o}'")),
    }
    if !(20.0..=5000.0).contains(&w) || !(20.0..=5000.0).contains(&h) {
        return Err("pages are 20 to 5000 mm on a side".into());
    }
    Ok((w, h))
}

/// The active map's view as a frame centre and scale for a frame `w_mm` wide.
fn view_frame(s: &Session, w_mm: f64) -> (Coord, f64) {
    let m = s.map();
    let ground_w = s.view_size.0 * m.ground_res();
    (m.center_lonlat(), (ground_w / (w_mm / 1000.0)).clamp(rustgis_map::layout::MIN_FRAME_SCALE, rustgis_map::layout::MAX_FRAME_SCALE))
}

/// The active map's bearing and pitch, for a frame showing its view.
fn view_turn(s: &Session) -> (f64, f64) {
    let m = s.map();
    (m.camera.bearing, m.camera.pitch)
}

fn frame_kind(s: &Session, map: &str, w: f64, shape: FrameShape) -> ElementKind {
    let (center, scale) = view_frame(s, w);
    let (bearing, pitch) = view_turn(s);
    ElementKind::MapFrame {
        map: map.to_string(),
        center,
        scale,
        shape,
        grid: None,
        extent_of: None,
        extent_color: rustgis_map::layout::INDICATOR_COLOR,
        extent_width: rustgis_map::layout::INDICATOR_WIDTH,
        bearing,
        pitch,
    }
}

pub(crate) fn new_layout(s: &mut Session, p: &Value) -> CmdResult {
    let (w, h) = page_size(
        &json!({"page": ps(p, "page").unwrap_or("letter"), "orientation": ps(p, "orientation").unwrap_or("landscape"), "width": p.get("width"), "height": p.get("height")}),
        None,
    )?;
    let name = unique_name(s, ps(p, "name").unwrap_or("Layout"));
    let mut l = Layout::new(name.clone(), w, h);
    match ps(p, "template").unwrap_or("map") {
        "blank" => {}
        "map" => {
            let map = s.map().name.clone();
            let m = 10.0;
            let fw = (w - 2.0 * m) * 0.72;
            let fh = h - 2.0 * m - 18.0;
            let frame = l.add("Map Frame", (m, m + 16.0, fw, fh), frame_kind(s, &map, fw, FrameShape::Rectangle));
            let side = m + fw + 6.0;
            let sw = w - side - m;
            l.add("Title", (m, m, w - 2.0 * m, 14.0), ElementKind::text(map.clone(), 22.0, Rgba::rgb(0x1F, 0x29, 0x37), true));
            l.add(
                "Legend",
                (side, m + 16.0, sw, fh * 0.6),
                ElementKind::Legend {
                    frame,
                    title: "Legend".into(),
                    all_classes: false,
                    hide_other: false,
                    exclude: Vec::new(),
                    columns: 1,
                },
            );
            l.add("North Arrow", (side + sw / 2.0 - 8.0, m + 16.0 + fh * 0.62, 16.0, 24.0), ElementKind::north_arrow(frame));
            l.add("Scale Bar", (side, m + 16.0 + fh * 0.62 + 28.0, sw, 12.0), ElementKind::scale_bar(frame));
            l.add(
                "Credits",
                (side, h - m - 10.0, sw, 10.0),
                ElementKind::text("{scale} · {date}", 8.0, Rgba::rgb(0x55, 0x5B, 0x66), false),
            );
        }
        t => return Err(format!("unknown template '{t}'")),
    }
    if s.project.layouts.len() >= crate::project::MAX_LAYOUTS {
        return Err(format!("a project holds at most {} layouts; remove one first", crate::project::MAX_LAYOUTS));
    }
    s.project.layouts.push(l);
    let i = s.project.layouts.len() - 1;
    s.active_layout = Some(i);
    Ok(json!({"layout": i, "name": name}))
}

/// One of `layout.publication`'s profiles.
pub struct PublicationProfile {
    pub id: &'static str,
    pub label: &'static str,
    /// A sheet (a frame, title and boxed footer), else a figure (the map edge to edge, its title
    /// and sources left to the caption).
    pub sheet: bool,
    /// The page, in mm. A height of 0 follows the map's shape.
    pub width: f64,
    pub height: f64,
}

/// `layout.publication`'s profiles: sheets for printing, figures for a journal or a thesis.
pub const PUBLICATION_PROFILES: &[PublicationProfile] = &[
    PublicationProfile { id: "a4-portrait", label: "A4 portrait sheet", sheet: true, width: 210.0, height: 297.0 },
    PublicationProfile { id: "a4-landscape", label: "A4 landscape sheet", sheet: true, width: 297.0, height: 210.0 },
    PublicationProfile { id: "a3-portrait", label: "A3 portrait sheet", sheet: true, width: 297.0, height: 420.0 },
    PublicationProfile { id: "a3-landscape", label: "A3 landscape sheet", sheet: true, width: 420.0, height: 297.0 },
    PublicationProfile { id: "a2-portrait", label: "A2 portrait sheet", sheet: true, width: 420.0, height: 594.0 },
    PublicationProfile { id: "a2-landscape", label: "A2 landscape sheet", sheet: true, width: 594.0, height: 420.0 },
    PublicationProfile { id: "figure-1col", label: "Journal figure, 1 column (90 mm)", sheet: false, width: 90.0, height: 0.0 },
    PublicationProfile { id: "figure-1.5col", label: "Journal figure, 1.5 columns (140 mm)", sheet: false, width: 140.0, height: 0.0 },
    PublicationProfile { id: "figure-2col", label: "Journal figure, 2 columns (190 mm)", sheet: false, width: 190.0, height: 0.0 },
    PublicationProfile { id: "thesis-full", label: "Thesis figure, full page (160 × 225 mm)", sheet: false, width: 160.0, height: 225.0 },
    PublicationProfile { id: "thesis-half", label: "Thesis figure, half page (160 × 110 mm)", sheet: false, width: 160.0, height: 110.0 },
];

/// `layout.publication`'s styles for a sheet, by the name `"style"` takes, with their names in
/// English: the default first.
pub const PUBLICATION_STYLES: &[(&str, &str)] = &[
    ("classic", "Classic"),
    ("banner", "Banner"),
    ("panel", "Panel"),
    ("editorial", "Editorial"),
    ("modular", "Modular"),
    ("poster", "Poster"),
];

/// A figure's text size, in points at its printed size: journals ask for 6 to 8.
const FIGURE_PT: f64 = 7.0;

/// The graticule steps a publication layout picks from, in degrees.
const GRID_STEPS: &[f64] = &[0.01, 0.02, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0];

/// The strips a frame's graticule labels take inside its edges, in mm: beside the left and right
/// edges (a latitude such as `10°S` across), and along the top and bottom ones.
const LABEL_ROOM_SIDE: f64 = 9.0;
const LABEL_ROOM_END: f64 = 6.0;

/// The longest text a publication layout takes in one field, in characters.
const MAX_PUBLICATION_TEXT: usize = 2000;

/// A scale denominator rounded up to two significant figures (1:1,234,567 to 1:1,300,000), so the
/// extent fitted at it still fits.
pub(crate) fn round_scale(scale: f64) -> f64 {
    if !(scale.is_finite() && scale > 0.0) {
        return scale;
    }
    let unit = 10f64.powf(scale.log10().floor() - 1.0);
    ((scale / unit) - 1e-9).ceil() * unit
}

/// The graticule step for an extent: the smallest that draws at most five lines across its longer
/// side.
pub(crate) fn grid_step(e: &Extent) -> f64 {
    let span = e.width().max(e.height());
    GRID_STEPS.iter().copied().find(|step| span / step <= 5.0).unwrap_or(30.0)
}

/// A graticule step as printed: degrees, else minutes, else seconds of arc.
pub(crate) fn step_text(step: f64) -> String {
    let minutes = step * 60.0;
    if step >= 1.0 {
        format!("{step}°")
    } else if (minutes - minutes.round()).abs() < 1e-9 {
        format!("{}′", minutes.round())
    } else {
        // Minutes and seconds (1′12″), or seconds alone under a minute.
        let seconds = (step * 3600.0).round();
        let (m, sec) = ((seconds / 60.0).floor(), seconds % 60.0);
        if m > 0.0 { format!("{m}′{sec}″") } else { format!("{sec}″") }
    }
}

/// The map's projection and datum for a layout's texts: an EPSG name's two halves (`SIRGAS 2000 /
/// UTM zone 23S`), else the projection's name and the datum its definition names (a preset's
/// `+datum=NAD83`), WGS 84 when it names none.
fn projection_and_datum(s: &Session) -> (String, String) {
    let projection = &s.map().projection;
    let label = projection.label();
    if let Some((datum, name)) = label.split_once(" / ") {
        return (name.trim().to_string(), datum.trim().to_string());
    }
    // A UTM zone's EPSG code, named: its PROJ definition gives only an ellipsoid (SIRGAS 2000's
    // zones have +ellps=GRS80), and its label only the code.
    let code = label.strip_prefix("EPSG:").and_then(|n| n.parse::<u32>().ok());
    if let Some((datum, zone, north)) = code.and_then(rustgis_data::raster::wkt::utm_of_code) {
        return (format!("UTM zone {zone}{}", if north { "N" } else { "S" }), datum.to_string());
    }
    let datum = rustgis_map::projection::lookup(&projection.code()).ok().and_then(|(def, _)| datum_of(&def));
    (label, datum.unwrap_or_else(|| "WGS 84".to_string()))
}

/// The datum a PROJ definition names: its `+datum`, else its `+ellps`, else a sphere for `+R`.
pub(crate) fn datum_of(def: &str) -> Option<String> {
    let param = |k: &str| def.split_whitespace().find_map(|t| t.strip_prefix(k).map(str::to_string));
    if let Some(d) = param("+datum=") {
        return Some(if d == "WGS84" { "WGS 84".into() } else { d });
    }
    if let Some(e) = param("+ellps=") {
        return Some(match e.as_str() {
            "WGS84" => "WGS 84".into(),
            "GRS80" => "GRS 1980 ellipsoid".into(),
            _ => format!("{e} ellipsoid"),
        });
    }
    param("+R=").map(|_| "a sphere".into())
}

/// Texts given as one or a list (up to 50), each cut to [`MAX_PUBLICATION_TEXT`] characters.
fn texts_of(p: &Value, k: &str) -> Result<Option<Vec<String>>, String> {
    let cut = |t: &str| t.chars().take(MAX_PUBLICATION_TEXT).collect::<String>();
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(t)) => Ok(Some(vec![cut(t)])),
        Some(Value::Array(a)) if a.len() <= 50 => a
            .iter()
            .map(|v| v.as_str().map(cut).ok_or_else(|| format!("{k} is a text or a list of texts")))
            .collect::<Result<_, _>>()
            .map(Some),
        Some(_) => Err(format!("{k} is a text or a list of up to 50 texts")),
    }
}

/// The extent's height over its width on the active map's projection, along its edges.
fn aspect(s: &Session, e: &Extent) -> Option<f64> {
    let flat = s.map().flat();
    let mut ext = Extent::EMPTY;
    for i in 0..=32 {
        let t = f64::from(i) / 32.0;
        let (x, y) = (e.min_x + e.width() * t, e.min_y + e.height() * t);
        for c in [Coord::new(x, e.min_y), Coord::new(x, e.max_y), Coord::new(e.min_x, y), Coord::new(e.max_x, y)] {
            if let Some(q) = flat.forward_checked(c) {
                ext.expand(q);
            }
        }
    }
    (ext.width() > 0.0 && ext.height() > 0.0).then(|| ext.height() / ext.width())
}

/// `t` in capitals, its `{tokens}` as they are: the layout fills a token (`{date}`) when it draws
/// only in its own case. A token is a closed `{name}` of letters and digits; a lone brace is text.
pub(crate) fn capitals(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut rest = t;
    while let Some(c) = rest.chars().next() {
        let token =
            (c == '{').then(|| rest.find('}')).flatten().filter(|&end| end > 1 && rest[1..end].chars().all(|c| c.is_ascii_alphanumeric()));
        match token {
            Some(end) => {
                out.push_str(&rest[..=end]);
                rest = &rest[end + 1..];
            }
            None => {
                out.extend(c.to_uppercase());
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}

/// A text element in a publication layout's style: upright, sans, in `align`.
fn pub_text(text: String, size: f64, color: Rgba, bold: bool, align: TextAlign) -> ElementKind {
    pub_text_in(text, size, color, bold, align, FontFamily::Sans)
}

/// [`pub_text`] in `font`.
fn pub_text_in(text: String, size: f64, color: Rgba, bold: bool, align: TextAlign, font: FontFamily) -> ElementKind {
    ElementKind::Text { text, size, color, bold, italic: false, align, letter_spacing: 0.0, font, font_name: None }
}

/// How a publication sheet is arranged around its map.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Style {
    /// The title over the map, the footer's boxes under it.
    Classic,
    /// The title and subtitle in a band of the accent colour across the top.
    Banner,
    /// The map at the left; a panel at the right with the title, the locator, the legend, the
    /// sources and the reference.
    Panel,
    /// A magazine's page: the title large in serif, a column at the right with the note, the
    /// locator and the legend.
    Editorial,
    /// Cards on a light page: the map in one, the locator and the legend in one each beside it,
    /// the sources and the reference in tinted ones under them.
    Modular,
    /// The page in the accent colour: the title and subtitle in light text across the top, the
    /// map in a light card, the footer in darker blocks.
    Poster,
}

impl Style {
    fn code(self) -> &'static str {
        match self {
            Style::Classic => "classic",
            Style::Banner => "banner",
            Style::Panel => "panel",
            Style::Editorial => "editorial",
            Style::Modular => "modular",
            Style::Poster => "poster",
        }
    }

    /// The accent colour when none is given: a deep blue, green or brown (a poster's page).
    fn accent(self) -> Rgba {
        match self {
            Style::Classic => Rgba::rgb(0x2B, 0x26, 0x20),
            Style::Banner => Rgba::rgb(0x4A, 0x34, 0x26),
            Style::Panel => Rgba::rgb(0x2E, 0x5E, 0x3A),
            Style::Editorial => Rgba::rgb(0x1F, 0x2D, 0x4D),
            Style::Modular => Rgba::rgb(0x2F, 0x6B, 0x4F),
            Style::Poster => Rgba::rgb(0x3A, 0x26, 0x18),
        }
    }

    /// Whether its surrounds sit on cards with rounded corners.
    fn cards(self) -> bool {
        matches!(self, Style::Modular | Style::Poster)
    }
}

/// How a style draws its footer: its boxes' fill, outline and corners, and its texts' colours.
#[derive(Clone, Copy)]
struct Look {
    head: Rgba,
    body: Rgba,
    small: Rgba,
    fill: Rgba,
    outline: Rgba,
    width: f64,
    radius: f64,
}

/// Where a style sets the locator and the legend (under it) beside the map rather than over it:
/// the room left for them there, in page millimetres.
#[derive(Clone, Copy, Debug)]
struct Column {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// A rectangle with no outline (`width` 0) or no fill (`fill` transparent).
fn pub_box(fill: Rgba, outline: Rgba, width: f64) -> ElementKind {
    pub_card(fill, outline, width, 0.0)
}

/// [`pub_box`] with its corners rounded by `radius` millimetres.
fn pub_card(fill: Rgba, outline: Rgba, width: f64, radius: f64) -> ElementKind {
    ElementKind::Rectangle { fill, outline, width, radius }
}

/// The surrounds a publication layout places in the map frame's corners.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Surround {
    Locator,
    Arrow,
    Legend,
    Scale,
}

/// The least a publication layout's legend is cut to, each way, by the surrounds beside and above
/// or below it, in mm.
const MIN_LEGEND_MM: f64 = 10.0;

/// A publication layout's legend, `w` × `h` in its corner, cut short of the surround beside it
/// (`beside`: what it is and its width) and the one above or below it (`over`: what it is and its
/// height), in a frame with `across` and `down` mm of room (its margins and the gap between
/// surrounds taken off). Beside the arrow or the scale bar the legend keeps [`MIN_LEGEND_MM`] each
/// way, as a layout without a locator always has; the locator leaving it less, `None`: left out,
/// rather than drawn over the locator.
pub(crate) fn legend_room(
    w: f64,
    h: f64,
    across: f64,
    down: f64,
    beside: Option<(Surround, f64)>,
    over: Option<(Surround, f64)>,
) -> Option<(f64, f64)> {
    let room = |other: Option<(Surround, f64)>, total: f64| other.map_or(f64::INFINITY, |(_, size)| total - size);
    let (room_w, room_h) = (room(beside, across), room(over, down));
    let locator_in_the_way =
        |room: f64, other: Option<(Surround, f64)>| room < MIN_LEGEND_MM && other.is_some_and(|(kind, _)| kind == Surround::Locator);
    if locator_in_the_way(room_w, beside) || locator_in_the_way(room_h, over) {
        return None;
    }
    Some((w.min(room_w.max(MIN_LEGEND_MM)), h.min(room_h.max(MIN_LEGEND_MM))))
}

/// Whether a publication legend laid out in more columns beats it in fewer, each as the share of it
/// shown and the area (mm²) the surrounds leave over the drawing: it shows more of itself, or as
/// much and covers at least a quarter less, as a low wide box does above a map's data (#1379).
pub(crate) fn more_columns_win((shown, over): (f64, f64), (best_shown, best_over): (f64, f64)) -> bool {
    shown > best_shown + 1e-6 || (shown > best_shown - 1e-6 && over < 0.75 * best_over)
}

/// The width and height, in cells, of the largest rectangle with no drawn cell in
/// `(c0, r0, c1, r1)` of a grid `gw` cells wide, its cells row by row: where a legend's shape can
/// go (#1379).
pub(crate) fn largest_free_cells(gw: usize, cells: &[bool], (c0, r0, c1, r1): (usize, usize, usize, usize)) -> (usize, usize) {
    let mut heights = vec![0usize; c1.saturating_sub(c0) + 1];
    let (mut best, mut stack) = ((0, 0), Vec::<usize>::new());
    for r in r0..r1 {
        for c in c0..c1 {
            let free = !cells.get(r * gw + c).copied().unwrap_or(true);
            heights[c - c0] = if free { heights[c - c0] + 1 } else { 0 };
        }
        // Each column's height from this row up: the widest run at least that high, by a stack of
        // rising heights. The last entry stays 0, so every run ends.
        stack.clear();
        for i in 0..heights.len() {
            while let Some(&top) = stack.last().filter(|&&top| heights[top] >= heights[i]) {
                stack.pop();
                let wide = stack.last().map_or(i, |&left| i - left - 1);
                if wide * heights[top] > best.0 * best.1 {
                    best = (wide, heights[top]);
                }
            }
            stack.push(i);
        }
    }
    best
}

/// A publication layout's colorbar's height, in mm: its title, the bar and its labels.
const COLORBAR_MM: f64 = 15.0;

/// The share of what the map draws a surround may cover before the layout warns about it, when a
/// corner freer by at least as much went to another surround.
const OVER_DATA: f64 = 0.25;

/// The share of what the map draws a surround may cover in the corner it was given before it
/// leaves the corner for the freest place in the frame ([`free_spot`], #1379).
const LEAK: f64 = 0.005;

/// The step of the search for a place off the corners, in mm.
const STEP_MM: f64 = 1.0;

/// The room kept between surrounds placed off the corners, in mm.
const CLEAR_MM: f64 = 2.0;

/// A rectangle (x, y, w, h) in page millimetres.
type Rect = (f64, f64, f64, f64);

/// Where a surround that covers what the map draws in its corner goes instead (#1379): the place
/// in `area` (x0, y0, x1, y1: the frame inside the graticule labels' margins) at least
/// [`CLEAR_MM`] from each of `others`, on a [`STEP_MM`] step and on the lines through the area's
/// edges and centre and the others' edges, costing least: what it covers of the drawing (a share,
/// from `covered`) times 1000, plus its distance from `home` times 0.05, plus its distance from the
/// nearest of those lines, up to 4 mm each way, times 0.25. So it goes where the map is free, near
/// its corner, and in line with the frame or another surround (the scale bar under the locator's
/// left edge, the legend on the scale bar's baseline). The first of the cheapest, scanning row by
/// row, so a frame always gives the same place; `None` when nothing fits clear of the others.
pub(crate) fn free_spot(home: Rect, area: (f64, f64, f64, f64), others: &[Rect], covered: impl Fn(Rect) -> f64) -> Option<Rect> {
    let (hx, hy, w, h) = home;
    let (x0, y0, x1, y1) = area;
    let (xmax, ymax) = (x1 - w, y1 - h);
    if xmax < x0 || ymax < y0 {
        return None;
    }
    let mut gx = vec![x0, x1, (x0 + x1) / 2.0];
    let mut gy = vec![y0, y1, (y0 + y1) / 2.0];
    for o in others {
        gx.extend([o.0, o.0 + o.2]);
        gy.extend([o.1, o.1 + o.3]);
    }
    // Every step, then where an edge or the centre falls on a line.
    let along = |lo: f64, hi: f64, size: f64, lines: &[f64]| -> Vec<f64> {
        let steps = ((hi - lo) / STEP_MM).floor() as usize;
        let mut v: Vec<f64> = (0..=steps).map(|i| lo + i as f64 * STEP_MM).collect();
        v.extend(lines.iter().flat_map(|g| [*g, g - size, g - size / 2.0]).filter(|p| (lo..=hi).contains(p)));
        v
    };
    let off = |p: f64, size: f64, lines: &[f64]| {
        lines.iter().map(|g| (p - g).abs().min((p + size - g).abs()).min((p + size / 2.0 - g).abs())).fold(4.0_f64, f64::min)
    };
    let clear = |r: Rect| {
        others.iter().all(|o| {
            r.0 >= o.0 + o.2 + CLEAR_MM || o.0 >= r.0 + r.2 + CLEAR_MM || r.1 >= o.1 + o.3 + CLEAR_MM || o.1 >= r.1 + r.3 + CLEAR_MM
        })
    };
    // Each place along an axis with how far it is off the nearest line, worked out once per axis.
    let xs: Vec<(f64, f64)> = along(x0, xmax, w, &gx).into_iter().map(|x| (x, off(x, w, &gx))).collect();
    let ys: Vec<(f64, f64)> = along(y0, ymax, h, &gy).into_iter().map(|y| (y, off(y, h, &gy))).collect();
    let mut best: Option<(f64, Rect)> = None;
    for &(y, off_y) in &ys {
        for &(x, off_x) in &xs {
            let r = (x, y, w, h);
            if !clear(r) {
                continue;
            }
            let cost = 1000.0 * covered(r) + 0.05 * (x - hx).hypot(y - hy) + 0.25 * (off_x + off_y);
            if best.is_none_or(|(c, _)| cost < c - 1e-9) {
                best = Some((cost, r));
            }
        }
    }
    best.map(|(_, r)| r)
}

/// Where an arrangement puts each surround of a publication layout: the corner the locator, the
/// north arrow, the legend and the scale bar are each at home in (0 top left, 1 top right, 2 bottom
/// left, 3 bottom right), all four different (#1379).
pub type Homes = [usize; 4];

/// The arrangements by name. `classic` is the one layouts had before there was a choice.
pub const ARRANGEMENTS: [(&str, Homes); 4] =
    [("classic", [0, 1, 2, 3]), ("mirrored", [1, 0, 3, 2]), ("legendRight", [0, 1, 3, 2]), ("legendTop", [2, 1, 0, 3])];

/// The keys of an arrangement given surround by surround, in [`Homes`]'s order.
const ARRANGEMENT_KEYS: [&str; 4] = ["locator", "northArrow", "legend", "scaleBar"];

/// The corners by name, in [`Homes`]'s numbering.
const CORNER_NAMES: [&str; 4] = ["top-left", "top-right", "bottom-left", "bottom-right"];

/// A surround's place in [`Homes`].
fn home_slot(kind: Surround) -> usize {
    match kind {
        Surround::Locator => 0,
        Surround::Arrow => 1,
        Surround::Legend => 2,
        Surround::Scale => 3,
    }
}

/// An arrangement: one of [`ARRANGEMENTS`] by name, or a corner for each surround
/// (`{"locator": "top-left", "northArrow": "top-right", "legend": "bottom-left", "scaleBar":
/// "bottom-right"}`, every corner once). `Value::Null` (none given) is `classic`.
pub fn parse_arrangement(v: &Value) -> Result<Homes, String> {
    let names = || ARRANGEMENTS.iter().map(|(n, _)| format!("\"{n}\"")).collect::<Vec<_>>().join(", ");
    match v {
        Value::Null => Ok(ARRANGEMENTS[0].1),
        Value::String(name) => ARRANGEMENTS
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name.trim()))
            .map(|(_, h)| *h)
            .ok_or_else(|| format!("arrangement is {}, or a corner for each surround, not \"{name}\"", names())),
        Value::Object(o) => {
            if let Some(k) = o.keys().find(|k| !ARRANGEMENT_KEYS.contains(&k.as_str())) {
                return Err(format!(
                    "an arrangement gives \"locator\", \"northArrow\", \"legend\" and \"scaleBar\" a corner each, not \"{k}\""
                ));
            }
            let mut homes = [0usize; 4];
            for (i, key) in ARRANGEMENT_KEYS.iter().enumerate() {
                let corner = o.get(*key).and_then(Value::as_str).map(str::trim).unwrap_or("");
                homes[i] = CORNER_NAMES
                    .iter()
                    .position(|c| c.eq_ignore_ascii_case(corner))
                    .ok_or_else(|| format!("arrangement: \"{key}\" is \"top-left\", \"top-right\", \"bottom-left\" or \"bottom-right\""))?;
            }
            if (0..4).any(|i| homes[..i].contains(&homes[i])) {
                return Err("an arrangement puts each surround in a corner of its own".into());
            }
            Ok(homes)
        }
        _ => Err(format!("arrangement is {}, or a corner for each surround", names())),
    }
}

/// The area (mm²) the rectangles cover twice, pair by pair.
pub(crate) fn overlap_area(rects: &[Rect]) -> f64 {
    let mut total = 0.0;
    for (i, a) in rects.iter().enumerate() {
        for b in &rects[i + 1..] {
            let (w, h) = ((a.0 + a.2).min(b.0 + b.2) - a.0.max(b.0), (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1));
            total += w.max(0.0) * h.max(0.0);
        }
    }
    total
}

/// Which arrangement `best` takes, from each one's (area moved, area left over the data) in mm², in
/// the order tried (#1379): of those moving within a square millimetre of the least, the one
/// leaving the least over the data by more than a square millimetre, the first tried on a tie.
/// Both bands are measured from the best value, so they don't add up across tries. `None` when no
/// score is a number.
pub(crate) fn pick_arrangement(scores: &[(f64, f64)]) -> Option<usize> {
    let least = scores.iter().map(|s| s.0).fold(f64::INFINITY, f64::min);
    let near: Vec<usize> = (0..scores.len()).filter(|&i| scores[i].0 <= least + 1.0).collect();
    let fewest = near.iter().map(|&i| scores[i].1).fold(f64::INFINITY, f64::min);
    near.into_iter().find(|&i| scores[i].1 <= fewest + 1.0)
}

/// The name of an arrangement, or `"custom"` for one given corner by corner.
fn arrangement_name(homes: Homes) -> &'static str {
    ARRANGEMENTS.iter().find(|(_, h)| *h == homes).map_or("custom", |(n, _)| n)
}

/// Sets or shows the favourite arrangement of publication layouts (#1379): the one
/// `"arrangement": "favourite"` asks for, and the first `"best"` tries (it wins a tie). Kept with
/// the app's preferences, not the project.
fn arrangement(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(v) = p.get("favourite").filter(|v| !v.is_null()) {
        parse_arrangement(v)?;
        s.layout_favourite = v.clone();
    }
    let homes = parse_arrangement(&s.layout_favourite)?;
    let corners: serde_json::Map<String, Value> =
        ARRANGEMENT_KEYS.iter().zip(homes).map(|(k, h)| ((*k).to_string(), json!(CORNER_NAMES[h]))).collect();
    Ok(
        json!({"favourite": s.layout_favourite, "corners": corners, "arrangements": ARRANGEMENTS.iter().map(|(n, _)| *n).collect::<Vec<_>>()}),
    )
}

/// A `"decimal"` parameter: `","` for a decimal comma, `"."` for a point; `None` when not given.
fn decimal_mark(p: &Value) -> Result<Option<bool>, String> {
    match p.get("decimal").filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => match v.as_str().map(str::trim) {
            Some(",") => Ok(Some(true)),
            Some(".") => Ok(Some(false)),
            _ => Err("decimal is \".\" or \",\"".into()),
        },
    }
}

/// What a publication layout locates its area on.
#[derive(Clone, Copy, PartialEq)]
enum Locator {
    Globe,
    Inset,
    None,
}

/// The scale at which `map` on the globe, centred on `center`, draws a globe `d_mm` across in a
/// frame: the globe's visible disc as the renderer lays it out for the frame's camera at a probe
/// scale, its size going as one over the scale.
fn globe_scale(map: &rustgis_map::Map, center: Coord, d_mm: f64) -> f64 {
    const PROBE: f64 = 1e8;
    let cam = rustgis_map::layout::frame_camera(map, center, PROBE, 0.0, 0.0);
    let side = d_mm.max(1.0) * rustgis_map::layout::PT_PER_MM;
    let radius = rustgis_map::globe::Globe::new(&cam, side, side).disc_radius();
    if radius.is_finite() && radius > 0.0 {
        (PROBE * 2.0 * radius / side).clamp(rustgis_map::layout::MIN_FRAME_SCALE, rustgis_map::layout::MAX_FRAME_SCALE)
    } else {
        PROBE
    }
}

/// Whether a sheet `short` × `long` mm suits an area `aspect` times as tall as it is wide better
/// turned to landscape: the map comes out clearly wider there (by more than 3 %, portrait winning
/// a near tie), as near as the sheet's texts can be told before they are laid out (a header and a
/// footer of about 70 mm on A4 between them, a panel's title in its panel; a style with a column
/// leaves the map two thirds of the width).
fn landscape_suits(aspect: f64, short: f64, long: f64, style: Style) -> bool {
    let k = short / 210.0;
    let map_w = |w: f64, h: f64| {
        let texts = if style == Style::Panel { 50.0 } else { 100.0 } * k;
        let (iw, ih) = (w - 30.0 * k, h - texts);
        let fw = if matches!(style, Style::Panel | Style::Editorial | Style::Modular) { 0.68 * iw } else { iw };
        let (fw, fh) = (fw - 2.0 * LABEL_ROOM_SIDE, ih - 2.0 * LABEL_ROOM_END);
        fw.min(fh / aspect.max(1e-6))
    };
    map_w(long, short) > 1.03 * map_w(short, long)
}

/// `layout.publication`: a finished sheet or figure of the active map, its extent fitted at a
/// round scale, with a graticule, legend, scale bar and north arrow; a sheet adds a frame, title,
/// subtitle and a footer of two boxed columns (sources, cartographic reference) with a note and
/// authorship under them, each text wrapped to its box with the layout's own font.
fn publication(s: &mut Session, p: &Value) -> CmdResult {
    let (auto, profile) = publication_profile(p)?;
    let (sheet, page_w, page_h) = (profile.sheet, profile.width, profile.height);
    let extent = match p.get("extent").filter(|v| !v.is_null()) {
        // The view; on a globe the whole Earth, as geoprocessing takes it (the globe camera's flat
        // extent means nothing there).
        None if s.map().projection.is_globe() => Extent::new(-180.0, -90.0, 180.0, 90.0),
        None => {
            let e = super::workspace::view_extent(s);
            Extent::new(e.min_x.max(-180.0), e.min_y.max(-90.0), e.max_x.min(180.0), e.max_y.min(90.0))
        }
        Some(v) => {
            let b: Option<Vec<f64>> =
                v.as_array().filter(|a| a.len() == 4).and_then(|a| a.iter().map(|x| x.as_f64().filter(|x| x.is_finite())).collect());
            match b.as_deref() {
                Some([w, so, e, n]) if w < e && so < n && *w >= -180.0 && *e <= 180.0 && *so >= -90.0 && *n <= 90.0 => {
                    Extent::new(*w, *so, *e, *n)
                }
                _ => return Err("extent is [west, south, east, north] in degrees".into()),
            }
        }
    };
    if !(extent.width() > 0.0 && extent.height() > 0.0) {
        return Err("the map's extent isn't known; give extent".into());
    }
    let one = |k: &str| texts_of(p, k).map(|t| t.map(|t| t.join(" ")).filter(|t| !t.trim().is_empty()));
    let (title, subtitle, note) = (one("title")?, one("subtitle")?, one("note")?);
    let sources = texts_of(p, "sources")?.unwrap_or_default();
    let reference =
        texts_of(p, "reference")?.unwrap_or_else(|| vec!["Projection: {projection}".into(), "Datum: {datum} · Graticule: {grid}".into()]);
    // An empty author leaves the line out; none given, the default.
    let author = match texts_of(p, "author")? {
        None => Some("Made with RustGIS · {date}".to_string()),
        Some(t) => Some(t.join(" ")).filter(|t| !t.trim().is_empty()),
    };
    let headings = texts_of(p, "headings")?.unwrap_or_default();
    let comma = decimal_mark(p)?.unwrap_or(false);
    let locator = publication_locator(p, sheet)?;
    let colorbar = publication_colorbar(p)?;
    // A style or an accent that isn't text is refused, as an unknown one is, rather than left out.
    for k in ["style", "accent"] {
        if p.get(k).is_some_and(|v| !v.is_null() && !v.is_string()) {
            return Err(format!("{k} is text: {}", if k == "style" { "a style's name, such as \"panel\"" } else { "a colour as #RRGGBB" }));
        }
    }
    let style = match ps(p, "style").map(str::trim) {
        None | Some("classic") => Style::Classic,
        Some("banner") => Style::Banner,
        Some("panel") => Style::Panel,
        Some("editorial") => Style::Editorial,
        Some("modular") => Style::Modular,
        Some("poster") => Style::Poster,
        Some(x) => {
            return Err(format!("style is \"classic\", \"banner\", \"panel\", \"editorial\", \"modular\" or \"poster\", not \"{x}\""));
        }
    };
    let accent = color(p, "accent", style.accent())?;
    // The arrangements to try (#1379): the favourite first, so it wins a tie.
    let favourite = parse_arrangement(&s.layout_favourite).unwrap_or(ARRANGEMENTS[0].1);
    let tries: Vec<Homes> = match p.get("arrangement").filter(|v| !v.is_null()) {
        None => std::iter::once(favourite).chain(ARRANGEMENTS.iter().map(|(_, h)| *h).filter(|h| *h != favourite)).collect(),
        Some(Value::String(a)) if a.trim().eq_ignore_ascii_case("best") => {
            std::iter::once(favourite).chain(ARRANGEMENTS.iter().map(|(_, h)| *h).filter(|h| *h != favourite)).collect()
        }
        Some(Value::String(a)) if a.trim().eq_ignore_ascii_case("favourite") => vec![favourite],
        Some(v) => vec![parse_arrangement(v)?],
    };
    // A translucent accent would leave a band, a poster's page or the headings faint or unseen.
    if accent.a() != 255 {
        return Err("accent is an opaque colour as #RRGGBB".into());
    }
    // No orientation asked: the one the area's map comes out larger on, from its extents east to
    // west and north to south on the map's projection and the room the style leaves the map.
    let profile = match &auto {
        Some(size) if aspect(s, &extent).is_some_and(|a| landscape_suits(a, page_w.min(page_h), page_w.max(page_h), style)) => {
            let id = format!("{size}-landscape");
            PUBLICATION_PROFILES.iter().find(|pr| pr.id == id).unwrap_or(profile)
        }
        _ => profile,
    };
    let (page_w, page_h) = (profile.width, profile.height);
    let heading = |i: usize, or: &str| headings.get(i).cloned().unwrap_or_else(|| or.to_string());
    let (projection, datum) = projection_and_datum(s);
    let step = grid_step(&extent);
    let fill = |t: &str| t.replace("{projection}", &projection).replace("{datum}", &datum).replace("{grid}", &step_text(step));
    // Text is measured as it is drawn: bold as bold, and the tokens the layout fills only when it
    // draws (`{date}`, `{scale}`…) as wide as a typical value of theirs.
    let (map_name, crs) = (s.map().name.clone(), s.map().projection.label());
    let drawn = move |t: &str| {
        // Most text has no token: measured as it is.
        if !t.contains('{') {
            return t.to_string();
        }
        [
            ("{date}", "0000-00-00"),
            ("{time}", "00:00"),
            ("{scale}", "1:10,000,000"),
            ("{pages}", "00"),
            ("{page}", "00"),
            ("{layers}", "00"),
            ("{center}", "00.000°S, 000.000°W"),
            ("{map}", map_name.as_str()),
            ("{crs}", crs.as_str()),
        ]
        .iter()
        .fold(t.to_string(), |t, (token, like)| t.replace(token, like))
    };
    let wrap_as = |t: &str, w: f64, pt: f64, bold: bool| {
        crate::render::report::wrap_with(&fill(t), w.max(1.0), |x| crate::render::report::width_mm(&drawn(x), pt as f32, bold))
    };
    let wrap = |t: &str, w: f64, pt: f64| wrap_as(t, w, pt, false);
    let pitch = crate::render::layout::line_pitch_mm;
    let ink = Rgba::rgb(0x2B, 0x26, 0x20);
    let soft = Rgba::rgb(0x5E, 0x55, 0x47);
    let clear = Rgba::new(0, 0, 0, 0);
    let mut warnings: Vec<String> = Vec::new();
    // Texts and boxes around the map, placed while its frame is worked out.
    let mut around: Vec<(String, (f64, f64, f64, f64), ElementKind)> = Vec::new();
    // What a style draws under the map and its surrounds: a page's colour, a card or a panel.
    let mut behind: Vec<(String, (f64, f64, f64, f64), ElementKind)> = Vec::new();
    // What spans the whole page (a sheet's neatline, a poster's colour), laid on the full page
    // whatever a pass below frees, so never moved: over the map (`true`) or under everything,
    // each before the rest of its side.
    let mut page_wide: Vec<(String, (f64, f64, f64, f64), ElementKind, bool)> = Vec::new();
    // A sheet's sizes follow its short side (A4's 210 mm is 1); a figure keeps its texts at their
    // printed size, whatever its width.
    let k = if sheet { page_w.min(page_h) / 210.0 } else { 1.0 };
    // A panel's or an editorial page's map is as wide as its column: where the area is less tall
    // than the frame the sheet leaves it, the sheet is laid out again with the frame the area's
    // height (and a little room), and the whole put in the middle of the page, rather than the map
    // showing more of its surroundings.
    let page_full = page_h;
    let mut freed = 0.0f64;
    let (mut target, mut passes): (Option<f64>, u32) = (None, 0);
    // A later pass failing where an earlier one held (its shorter page leaving the texts too
    // little room) settles on the last that held: only the first pass's failure is the request's.
    let (mut held, mut settled) = (0.0f64, false);
    let warned = warnings.len();
    let (w, h, (fx, fy, fw, fh), column) = loop {
        around.clear();
        behind.clear();
        page_wide.clear();
        warnings.truncate(warned);
        let page_h = page_full - freed;
        let attempt = (|| -> Result<_, String> {
            Ok(if sheet {
                let (m, pad) = (10.0 * k, 5.0 * k);
                let (x0, iw) = (m + pad, page_w - 2.0 * (m + pad));
                let (title_pt, sub_pt, head_pt, body_pt, small_pt) = (18.0 * k, 10.5 * k, 7.5 * k, 7.0 * k, 6.5 * k);
                // A neatline round the sheet, but on a poster's coloured page or a page of cards.
                if !style.cards() {
                    page_wide.push(("Frame".into(), (m, m, page_w - 2.0 * m, page_full - 2.0 * m), pub_box(clear, ink, 0.8), true));
                }
                // The sources and the reference that have something in them, each its heading and lines
                // `tw` wide. Each one's elements keep their names (`Sources`, `Reference`) whatever its
                // heading reads, the heading filled and wrapped like the rest.
                let sections = |tw: f64| -> Vec<(&'static str, Vec<String>, Vec<String>)> {
                    [("Sources", heading(0, "Sources"), &sources), ("Reference", heading(1, "Cartographic reference"), &reference)]
                        .into_iter()
                        .filter(|(.., texts)| !texts.is_empty())
                        .map(|(name, head, texts)| {
                            // Filled first, so `{projection}` reads in capitals too; the live tokens left
                            // keep their case (wrap_as fills again, which then finds nothing).
                            let head = wrap_as(&capitals(&fill(&head)), tw, head_pt, true);
                            (name, head, texts.iter().flat_map(|t| wrap(t, tw, body_pt)).collect::<Vec<_>>())
                        })
                        .collect()
                };
                // The footer, from the bottom up across the sheet: authorship, the note (unless the style
                // sets it beside the map), then the sources and reference boxed side by side (unless it
                // stacks them in its panel), as tall as the longer column. Its top.
                type Around = Vec<(String, (f64, f64, f64, f64), ElementKind)>;
                let footer = |around: &mut Around, look: &Look, note_here: bool, boxes_here: bool| -> f64 {
                    let bottom = page_h - m - pad;
                    let author_lines = author.as_deref().map(|t| wrap(t, iw, small_pt)).unwrap_or_default();
                    let author_h = author_lines.len() as f64 * pitch(small_pt);
                    let author_y = bottom - author_h;
                    if !author_lines.is_empty() {
                        around.push((
                            "Author".into(),
                            (x0, author_y, iw, author_h),
                            pub_text(author_lines.join("\n"), small_pt, look.small, false, TextAlign::Right),
                        ));
                    }
                    let mut foot = author_y;
                    let note_lines = note.as_deref().filter(|_| note_here).map(|t| wrap(t, iw, body_pt)).unwrap_or_default();
                    if !note_lines.is_empty() {
                        let nh = note_lines.len() as f64 * pitch(body_pt);
                        foot -= nh + 1.5 * k;
                        around.push((
                            "Note".into(),
                            (x0, foot, iw, nh),
                            pub_text(note_lines.join("\n"), body_pt, look.small, false, TextAlign::Left),
                        ));
                    }
                    if !boxes_here {
                        return foot;
                    }
                    // The columns side by side (one alone takes the width).
                    let (gap, inner) = (4.0 * k, 2.5 * k);
                    let n = [!sources.is_empty(), !reference.is_empty()].iter().filter(|x| **x).count().max(1) as f64;
                    let cw = (iw - gap * (n - 1.0)) / n;
                    let tw = cw - 2.0 * inner;
                    let columns = sections(tw);
                    let heads = columns.iter().map(|(_, h, _)| h.len()).max().unwrap_or(1);
                    let rows = columns.iter().map(|(.., l)| l.len()).max().unwrap_or(0);
                    if rows > 0 {
                        let head_h = heads as f64 * pitch(head_pt);
                        let bh = 2.0 * inner + head_h + rows as f64 * pitch(body_pt);
                        foot -= bh + 2.0 * k;
                        for (i, (name, head, lines)) in columns.into_iter().enumerate() {
                            let bx = x0 + i as f64 * (cw + gap);
                            around.push((
                                format!("{name} Box"),
                                (bx, foot, cw, bh),
                                pub_card(look.fill, look.outline, look.width, look.radius),
                            ));
                            let tx = bx + inner;
                            around.push((
                                format!("{name} Heading"),
                                (tx, foot + inner, tw, head.len() as f64 * pitch(head_pt)),
                                pub_text(head.join("\n"), head_pt, look.head, true, TextAlign::Left),
                            ));
                            if !lines.is_empty() {
                                let body = (tx, foot + inner + head_h, tw, lines.len() as f64 * pitch(body_pt));
                                around.push((
                                    name.to_string(),
                                    body,
                                    pub_text(lines.join("\n"), body_pt, look.body, false, TextAlign::Left),
                                ));
                            }
                        }
                    }
                    foot
                };
                // The map's frame between `top` and `bottom`, at least 40 mm tall (on A4).
                let room = |top: f64, bottom: f64| {
                    if bottom - top < 40.0 * k {
                        Err("the texts leave the map no room: shorten the footer, the note or the subtitle".to_string())
                    } else {
                        Ok(bottom - top)
                    }
                };
                let white = Rgba::rgb(0xFF, 0xFF, 0xFF);
                let plain = Look {
                    head: ink,
                    body: soft,
                    small: soft,
                    fill: clear,
                    outline: Rgba::new(0x2B, 0x26, 0x20, 140),
                    width: 0.5,
                    radius: 0.0,
                };
                let (card_r, card_pad) = (3.0 * k, 2.5 * k);
                let card_line = Rgba::new(0x2B, 0x26, 0x20, 60);
                match style {
                    Style::Classic => {
                        let mut top = m + pad;
                        if let Some(t) = &title {
                            let lines = wrap_as(t, iw, title_pt, true);
                            let th = lines.len() as f64 * pitch(title_pt) + 1.5 * k;
                            around.push((
                                "Title".into(),
                                (x0, top, iw, th),
                                pub_text(lines.join("\n"), title_pt, ink, true, TextAlign::Center),
                            ));
                            top += th;
                        }
                        if let Some(t) = &subtitle {
                            let lines = wrap(t, iw, sub_pt);
                            let sh = lines.len() as f64 * pitch(sub_pt) + 1.0 * k;
                            around.push((
                                "Subtitle".into(),
                                (x0, top, iw, sh),
                                pub_text(lines.join("\n"), sub_pt, soft, false, TextAlign::Center),
                            ));
                            top += sh;
                        }
                        let foot = footer(&mut around, &plain, true, true);
                        let (ft, fb) = (top + 3.0 * k, foot - 4.0 * k);
                        (page_w, page_h, (x0, ft, iw, room(ft, fb)?), None)
                    }
                    Style::Banner | Style::Poster => {
                        // A band in the accent colour across the top: the title at its left in white, the
                        // subtitle beside it past a thin rule. With neither, no band. A poster's whole page
                        // is that colour, its map in a light card and its footer in darker blocks.
                        let poster = style == Style::Poster;
                        if poster {
                            page_wide.push(("Background".into(), (0.0, 0.0, page_w, page_full), pub_box(accent, clear, 0.0), false));
                        }
                        let mut top = m + pad;
                        if title.is_some() || subtitle.is_some() {
                            let big = title_pt * 1.2;
                            let t_max = if subtitle.is_some() { 0.42 * iw } else { iw };
                            let t_lines = title.as_deref().map(|t| wrap_as(t, t_max, big, true)).unwrap_or_default();
                            let t_w = t_lines
                                .iter()
                                .map(|x| crate::render::report::width_mm(&drawn(x), big as f32, true))
                                .fold(0.0, f64::max)
                                .min(t_max)
                                .ceil();
                            let rule = if t_lines.is_empty() { 0.0 } else { 6.0 * k };
                            let s_x = x0 + t_w + rule;
                            let s_w = (x0 + iw - s_x).max(20.0 * k);
                            let s_lines = subtitle.as_deref().map(|t| wrap(t, s_w, sub_pt)).unwrap_or_default();
                            let (t_h, s_h) = (t_lines.len() as f64 * pitch(big), s_lines.len() as f64 * pitch(sub_pt));
                            let band_h = t_h.max(s_h) + 2.0 * pad;
                            let mid = m + band_h / 2.0;
                            if !poster {
                                around.push(("Banner".into(), (m, m, page_w - 2.0 * m, band_h), pub_box(accent, clear, 0.0)));
                            }
                            if !t_lines.is_empty() {
                                around.push((
                                    "Title".into(),
                                    (x0, mid - t_h / 2.0, t_w, t_h),
                                    pub_text(t_lines.join("\n"), big, white, true, TextAlign::Left),
                                ));
                            }
                            if !t_lines.is_empty() && !s_lines.is_empty() {
                                around.push((
                                    "Banner Rule".into(),
                                    (x0 + t_w + rule / 2.0 - 0.5, mid - t_h.max(s_h) / 2.0, 1.0, t_h.max(s_h)),
                                    ElementKind::Line {
                                        color: Rgba::new(0xFF, 0xFF, 0xFF, 170),
                                        width: 0.75,
                                        dir: LineDir::Vertical,
                                        head: LineHead::None,
                                    },
                                ));
                            }
                            if !s_lines.is_empty() {
                                around.push((
                                    "Subtitle".into(),
                                    (s_x, mid - s_h / 2.0, s_w, s_h),
                                    pub_text(s_lines.join("\n"), sub_pt, Rgba::rgb(0xF2, 0xEE, 0xE6), false, TextAlign::Left),
                                ));
                            }
                            top = m + band_h;
                        }
                        // A poster's footer: blocks a shade lighter than its page, amber headings, light text.
                        let shade = |c: u8| c.saturating_add(((255 - u16::from(c)) / 8) as u8);
                        let look = if poster {
                            Look {
                                head: Rgba::rgb(0xE8, 0xB0, 0x4B),
                                body: Rgba::rgb(0xED, 0xE6, 0xDA),
                                small: Rgba::rgb(0xCF, 0xC6, 0xB8),
                                fill: Rgba::rgb(shade(accent.r()), shade(accent.g()), shade(accent.b())),
                                outline: clear,
                                width: 0.0,
                                radius: card_r,
                            }
                        } else {
                            plain
                        };
                        let foot = footer(&mut around, &look, true, true);
                        let (ft, fb) = (top + 4.0 * k, foot - 4.0 * k);
                        let h = room(ft, fb)?;
                        if poster {
                            behind.push(("Map Card".into(), (x0, ft, iw, h), pub_card(Rgba::rgb(0xF6, 0xF1, 0xE7), clear, 0.0, card_r)));
                            (page_w, page_h, (x0 + card_pad, ft + card_pad, iw - 2.0 * card_pad, h - 2.0 * card_pad), None)
                        } else {
                            (page_w, page_h, (x0, ft, iw, h), None)
                        }
                    }
                    Style::Modular => {
                        // Cards on a light page: the title and subtitle at the top left over a short bar of
                        // the accent colour; the map in a card at the left; at the right the locator and
                        // the legend, each in a card; the sources and reference in tinted cards side by
                        // side under them, the note and authorship under all.
                        let mut y = m + pad;
                        let tw = 0.75 * iw;
                        if let Some(t) = &title {
                            let big = title_pt * 1.3;
                            let lines = wrap_as(t, tw, big, true);
                            let th = lines.len() as f64 * pitch(big);
                            around.push(("Title".into(), (x0, y, tw, th), pub_text(lines.join("\n"), big, ink, true, TextAlign::Left)));
                            y += th;
                        }
                        if let Some(t) = &subtitle {
                            let lines = wrap(t, tw, sub_pt);
                            let sh = lines.len() as f64 * pitch(sub_pt);
                            around.push((
                                "Subtitle".into(),
                                (x0, y, tw, sh),
                                pub_text(lines.join("\n"), sub_pt, soft, false, TextAlign::Left),
                            ));
                            y += sh;
                        }
                        if y > m + pad {
                            around.push(("Title Rule".into(), (x0, y + 1.5 * k, 14.0 * k, 1.2 * k), pub_box(accent, clear, 0.0)));
                            y += 5.0 * k;
                        }
                        let look = Look {
                            head: accent,
                            body: soft,
                            small: soft,
                            fill: Rgba::new(accent.r(), accent.g(), accent.b(), 0x1A),
                            outline: clear,
                            width: 0.0,
                            radius: card_r,
                        };
                        let foot = footer(&mut around, &look, true, true);
                        let bottom = foot - 4.0 * k;
                        let h = room(y, bottom)?;
                        let gap = 5.0 * k;
                        let cw = (0.3 * iw).max(48.0 * k).min(0.45 * iw);
                        let (mw, cx) = (iw - cw - gap, x0 + iw - cw);
                        behind.push(("Map Card".into(), (x0, y, mw, h), pub_card(white, card_line, 0.4, card_r)));
                        let frame = (x0 + card_pad, y + card_pad, mw - 2.0 * card_pad, h - 2.0 * card_pad);
                        let column = Column { x: cx + card_pad, y: y + card_pad, w: cw - 2.0 * card_pad, h: h - 2.0 * card_pad };
                        (page_w, page_h, frame, Some(column))
                    }
                    Style::Panel => {
                        // The map at the left, as tall as the sheet allows; a pale panel at the right with
                        // the title and subtitle, then the locator and the legend, and the sources and the
                        // reference at its foot; the note and authorship across the bottom.
                        let gap = 5.0 * k;
                        let pw = (0.3 * iw).max(48.0 * k).min(0.45 * iw);
                        let (mw, px) = (iw - pw - gap, x0 + iw - pw);
                        let top = m + pad;
                        // With its frame cut to the area, the sources and reference go under the map, in
                        // the room the frame left, and the panel holds the title, locator and legend.
                        let under = freed > 0.0;
                        let foot = footer(&mut around, &plain, true, under);
                        let panel_bottom = foot - 4.0 * k;
                        let tint = Rgba::new(accent.r(), accent.g(), accent.b(), 0x18);
                        behind.push(("Panel Box".into(), (px, top, pw, panel_bottom - top), pub_box(tint, clear, 0.0)));
                        let ip = 3.0 * k;
                        let (ix, tw) = (px + ip, pw - 2.0 * ip);
                        let mut y = top + ip;
                        if let Some(t) = &title {
                            let lines = wrap_as(t, tw, title_pt, true);
                            let th = lines.len() as f64 * pitch(title_pt);
                            around.push((
                                "Title".into(),
                                (ix, y, tw, th),
                                pub_text(lines.join("\n"), title_pt, ink, true, TextAlign::Left),
                            ));
                            y += th + 1.0 * k;
                        }
                        if let Some(t) = &subtitle {
                            let lines = wrap(t, tw, sub_pt);
                            let sh = lines.len() as f64 * pitch(sub_pt);
                            around.push((
                                "Subtitle".into(),
                                (ix, y, tw, sh),
                                pub_text(lines.join("\n"), sub_pt, soft, false, TextAlign::Left),
                            ));
                            y += sh;
                        }
                        if y > top + ip {
                            around.push((
                                "Panel Rule".into(),
                                (ix, y + 1.5 * k, tw, 1.0),
                                ElementKind::Line { color: accent, width: 1.0, dir: LineDir::Horizontal, head: LineHead::None },
                            ));
                            y += 4.0 * k;
                        }
                        // The sources and the reference, stacked up from the panel's foot.
                        let mut yb = panel_bottom - ip;
                        for (name, head, lines) in sections(tw).into_iter().rev().filter(|_| !under) {
                            let (hh, bh) = (head.len() as f64 * pitch(head_pt), lines.len() as f64 * pitch(body_pt));
                            yb -= hh + bh;
                            around.push((
                                format!("{name} Heading"),
                                (ix, yb, tw, hh),
                                pub_text(head.join("\n"), head_pt, accent, true, TextAlign::Left),
                            ));
                            if !lines.is_empty() {
                                around.push((
                                    name.to_string(),
                                    (ix, yb + hh, tw, bh),
                                    pub_text(lines.join("\n"), body_pt, soft, false, TextAlign::Left),
                                ));
                            }
                            yb -= 3.0 * k;
                        }
                        if yb - y < 30.0 * k {
                            return Err(
                            "the panel's texts leave no room for the locator and the legend: shorten the title, the sources or the reference"
                                .into(),
                        );
                        }
                        (page_w, page_h, (x0, top, mw, room(top, panel_bottom)?), Some(Column { x: ix, y, w: tw, h: yb - y }))
                    }
                    Style::Editorial => {
                        // A magazine's page: the title large in serif at the top left in the accent colour,
                        // the subtitle under it, the map below; a column at the right, under a rule, with
                        // its heading (the third of `headings`), the note (the map's story), the locator
                        // and the legend; the sources and reference boxed under both.
                        // Its title and subtitle are serif: with none installed, drawn in the sans, said.
                        if (title.is_some() || subtitle.is_some())
                            && let Some(w) = font_warning(FontFamily::Serif, None, false, (false, false))
                        {
                            warnings.push(format!("the editorial title and subtitle: {w}"));
                        }
                        let gap = 6.0 * k;
                        let cw = (0.3 * iw).max(45.0 * k).min(0.4 * iw);
                        let (mw, cx) = (iw - cw - gap, x0 + iw - cw);
                        let serif_wrap = |t: &str, w: f64, pt: f64, bold: bool| {
                            crate::render::report::wrap_with(&fill(t), w.max(1.0), |x| {
                                crate::render::report::serif_width_mm(&drawn(x), pt as f32, bold)
                            })
                        };
                        let (big, sub) = (30.0 * k, 13.0 * k);
                        let mut y = m + pad;
                        if let Some(t) = &title {
                            let lines = serif_wrap(t, mw, big, true);
                            let th = lines.len() as f64 * pitch(big);
                            around.push((
                                "Title".into(),
                                (x0, y, mw, th),
                                pub_text_in(lines.join("\n"), big, accent, true, TextAlign::Left, FontFamily::Serif),
                            ));
                            y += th + 1.0 * k;
                        }
                        if let Some(t) = &subtitle {
                            let lines = serif_wrap(t, mw, sub, false);
                            let sh = lines.len() as f64 * pitch(sub);
                            around.push((
                                "Subtitle".into(),
                                (x0, y, mw, sh),
                                pub_text_in(lines.join("\n"), sub, soft, false, TextAlign::Left, FontFamily::Serif),
                            ));
                            y += sh;
                        }
                        let foot = footer(&mut around, &plain, false, true);
                        let fb = foot - 4.0 * k;
                        let mut cy = m + pad;
                        around.push((
                            "Column Rule".into(),
                            (cx, cy, cw, 1.0),
                            ElementKind::Line { color: accent, width: 1.5, dir: LineDir::Horizontal, head: LineHead::None },
                        ));
                        cy += 3.0 * k;
                        if let Some(hd) = headings.get(2).filter(|h| !h.trim().is_empty()) {
                            let pt = head_pt * 1.2;
                            let lines = wrap_as(&capitals(&fill(hd)), cw, pt, true);
                            let hh = lines.len() as f64 * pitch(pt);
                            around.push((
                                "Column Heading".into(),
                                (cx, cy, cw, hh),
                                pub_text(lines.join("\n"), pt, accent, true, TextAlign::Left),
                            ));
                            cy += hh + 2.0 * k;
                        }
                        if let Some(t) = &note {
                            let pt = body_pt * 1.15;
                            let lines = wrap(t, cw, pt);
                            let nh = lines.len() as f64 * pitch(pt);
                            around.push(("Note".into(), (cx, cy, cw, nh), pub_text(lines.join("\n"), pt, soft, false, TextAlign::Left)));
                            cy += nh + 4.0 * k;
                        }
                        if fb - cy < 30.0 * k {
                            warnings
                                .push("the column's note leaves the locator and the legend little room under it: shorten the note".into());
                        }
                        let ft = y + 4.0 * k;
                        (page_w, page_h, (x0, ft, mw, room(ft, fb)?), Some(Column { x: cx, y: cy, w: cw, h: (fb - cy).max(0.0) }))
                    }
                }
            } else {
                if style != Style::Classic {
                    warnings.push(format!(
                        "style \"{}\" arranges a sheet's texts, which a figure leaves to its caption: left out",
                        style.code()
                    ));
                }
                // A figure: the map edge to edge, a line of the projection and datum under it.
                // Every text a figure leaves to its caption, given anyway, is said to be left out.
                let asked = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
                for (field, given) in [
                    ("title", title.is_some()),
                    ("subtitle", subtitle.is_some()),
                    ("sources", !sources.is_empty()),
                    ("note", note.is_some()),
                    // An empty author asks for none: nothing left out.
                    ("author", asked("author") && author.is_some()),
                    ("headings", !headings.is_empty()),
                ] {
                    if given {
                        warnings.push(format!("{field} goes in the figure's caption, not on it: left out"));
                    }
                }
                let line = reference.iter().map(|t| fill(t)).collect::<Vec<_>>().join(" · ");
                // A millimetre in from the paper's edges, which a journal may trim.
                let lines = wrap(&line, page_w - 2.0, FIGURE_PT);
                if lines.len() > 1 {
                    warnings.push("the reference is wider than the figure: it wraps under the map".into());
                }
                let note_h = lines.len() as f64 * pitch(FIGURE_PT) + 1.0;
                let fh = if page_h > 0.0 {
                    page_h - note_h
                } else {
                    let shape = aspect(s, &extent).unwrap_or_else(|| {
                        warnings.push("the extent's shape on the map's projection isn't known: the figure is 4 wide by 3 tall".into());
                        0.75
                    });
                    (page_w * shape).clamp(0.45 * page_w, 1.3 * page_w).round()
                };
                if fh < 40.0 {
                    return Err("the reference leaves the map no room: shorten it".into());
                }
                let h = if page_h > 0.0 { page_h } else { (fh + note_h).ceil() };
                around.push((
                    "Reference".into(),
                    (1.0, fh + 1.0, page_w - 2.0, note_h - 1.0),
                    pub_text(lines.join("\n"), FIGURE_PT, ink, false, TextAlign::Left),
                ));
                (page_w, h, (0.0, 0.0, page_w, fh), None)
            })
        })();
        let laid = match attempt {
            Ok(laid) => laid,
            Err(e) if passes == 0 || settled => return Err(e),
            Err(_) => {
                (freed, settled) = (held, true);
                continue;
            }
        };
        let (_, _, (_, _, lw, lh), _) = laid;
        // The frame's height for the area at its width, with a seventh more of its surroundings;
        // worked out on the first pass (the width doesn't change), aimed at on the others, whose
        // footer can grow (a panel's sources move under the map) and so need a pass or two more.
        if passes == 0 {
            target = (sheet && matches!(style, Style::Panel | Style::Editorial))
                .then(|| aspect(s, &extent))
                .flatten()
                .map(|a| (((lw - 2.0 * LABEL_ROOM_SIDE).max(1.0) * a + 2.0 * LABEL_ROOM_END) * 1.15).max(40.0 * k))
                // Only a frame clearly too tall is cut.
                .filter(|n| *n < 0.97 * lh);
        }
        passes += 1;
        match target {
            Some(n) if !settled && (lh - n).abs() > 0.5 && passes < 5 => (held, freed) = (freed, (freed + lh - n).max(0.0)),
            _ => break laid,
        }
    };
    // The content in the middle of the page: what the sheet laid out on its shorter page moves down
    // by half of what it freed (what spans the whole page was laid on it, and stays).
    for (_, r, _) in around.iter_mut().chain(behind.iter_mut()) {
        r.1 += freed / 2.0;
    }
    let (fy, column, h) = if freed > 0.0 && sheet {
        (fy + freed / 2.0, column.map(|c| Column { y: c.y + freed / 2.0, ..c }), page_full)
    } else {
        (fy, column, h)
    };
    // The extent fits within the graticule's label strips (drawn inside the frame along its
    // labelled edges), on both sides so it stays centred, and its labels cover none of it.
    let edges = GridEdges { top: sheet, bottom: true, left: true, right: sheet };
    let strip_w = if edges.left || edges.right { 2.0 * LABEL_ROOM_SIDE } else { 0.0 };
    let strip_h = if edges.top || edges.bottom { 2.0 * LABEL_ROOM_END } else { 0.0 };
    if fw - strip_w < fw / 2.0 || fh - strip_h < fh / 2.0 {
        warnings.push("the frame is too small to keep its graticule's labels off the map's edges: they may cover them".into());
    }
    let (center, scale) = fit_scale(s.map(), &extent, (fw - strip_w).max(fw / 2.0), (fh - strip_h).max(fh / 2.0))
        .ok_or("the extent can't be drawn on the map's projection")?;
    let scale = round_scale(scale);
    let grid = Grid { step, edges, ..Grid::default() };
    let name = unique_name(s, ps(p, "name").unwrap_or("Publication"));
    let mut l = Layout::new(name.clone(), w, h);
    l.decimal_comma = comma;
    let (over, under): (Vec<_>, Vec<_>) = page_wide.into_iter().partition(|e| e.3);
    for (n, r, kind, _) in under {
        l.add(&n, r, kind);
    }
    for (n, r, kind) in behind {
        l.add(&n, r, kind);
    }
    let frame = l.add(
        "Map Frame",
        (fx, fy, fw, fh),
        ElementKind::MapFrame {
            map: s.map().name.clone(),
            center,
            scale,
            shape: FrameShape::Rectangle,
            grid: Some(grid),
            extent_of: None,
            extent_color: rustgis_map::layout::INDICATOR_COLOR,
            extent_width: rustgis_map::layout::INDICATOR_WIDTH,
            bearing: 0.0,
            pitch: 0.0,
        },
    );
    // Over the map, clear of the graticule's labels (drawn inside the frame along its labelled
    // edges): the north arrow, the legend sized to what it lists, the scale bar with the numeric
    // scale under it, and the locator, each on a pale box. Each takes the corner where it covers
    // the least of what the map draws; by default (and when it makes no difference) the locator at
    // the top left, the arrow at the top right, the legend at the bottom left and the scale at the
    // bottom right.
    let paper = Rgba::new(0xFF, 0xFF, 0xFF, 225);
    let (e, small_pt) = if sheet { (4.0 * k, 6.5 * k) } else { (2.0, 6.0) };
    let room = |labelled: bool, side: bool| {
        e + if !labelled {
            0.0
        } else if side {
            LABEL_ROOM_SIDE
        } else {
            LABEL_ROOM_END
        }
    };
    let (left, right, top, bottom) = (room(edges.left, true), room(edges.right, true), room(edges.top, false), room(edges.bottom, false));
    let (aw, ah) = if sheet { (8.0 * k, 12.0 * k) } else { (5.0, 7.5) };
    // The locator: a globe (a copy of the map's polygon layers on the globe, its diameter nearly
    // the frame's) or the surrounding region on the map itself, the area outlined on either by
    // the map frame's extent indicator.
    let side = match column {
        // In a column, its width up to an A4 portrait column's (a landscape sheet's wider
        // column doesn't make it larger), leaving the legend most of the height under it.
        Some(c) => c.w.min(52.0 * k).min(0.45 * c.h).max(0.0),
        None if sheet => (0.24 * fw.min(fh)).clamp(25.0 * k, 70.0 * k),
        None => (0.3 * fw.min(fh)).clamp(18.0, 45.0),
    };
    let locator_map = match locator {
        Locator::None => None,
        Locator::Globe => {
            let src = s.map();
            let base = format!("{} Locator", src.name);
            // A locator an earlier sheet made that no layout shows any more (its layout deleted)
            // is replaced, so sheets made again and again don't leave a trail of them; one still
            // shown is kept, and the new one numbered beside it.
            let orphan = locator_orphan(s);
            let name = match orphan.and_then(|i| s.project.maps.get(i)) {
                Some(x) => x.name.clone(),
                None => numbered(&base, |n| s.project.maps.iter().any(|x| x.name.eq_ignore_ascii_case(n))),
            };
            // A new map of what the globe draws, not a clone of the whole map (every layer's
            // features): its basemap (the land and borders around the area), graticule and body,
            // and the map's own polygon layers that show, unlabelled: the land at a glance, no
            // raster or streamed layer (read again) nor points and lines cluttering a small disc.
            // Not its web tiles (fetched and drawn over the basemap on the disc), swipe, time
            // slider, layer groups (their hidden layers left out here), bookmarks or animation.
            let mut m = rustgis_map::Map::new(name);
            m.basemap = src.basemap;
            m.graticule = src.graticule;
            m.body = src.body.clone();
            m.set_projection(rustgis_map::Projection::Globe);
            let globe = globe_scale(&m, center, side * 0.92);
            // Each polygon simplified to a tenth of a millimetre on the disc (its scale in metres),
            // so the project keeps a sketch of the layers for it, not a second full copy of them.
            let tolerance_m = globe * 1e-4;
            // On the locator, sub-tolerance rings keep their outline rather than becoming triangles.
            let sketch = |g: &rustgis_data::Geometry| {
                let mut simple = rustgis_analysis::simplify_geometry(g, tolerance_m);
                if let (rustgis_data::Geometry::Polygon(original), rustgis_data::Geometry::Polygon(reduced)) = (g, &mut simple) {
                    for (original, reduced) in original.iter().flatten().zip(reduced.iter_mut().flatten()) {
                        let mut extent = Extent::EMPTY;
                        // Keep the extent continuous across the antimeridian.
                        let mut longitude = original.iter().find(|c| c.is_finite()).map_or(0.0, |c| c.x);
                        for &c in original.iter().filter(|c| c.is_finite()) {
                            longitude += (c.x - longitude + 180.0).rem_euclid(360.0) - 180.0;
                            extent.expand(Coord::new(longitude, c.y));
                        }
                        let projected = rustgis_geom::extent_to_merc(&extent);
                        let size_m =
                            projected.width().hypot(projected.height()) * extent.center().y.clamp(-85.0, 85.0).to_radians().cos().max(0.01);
                        if size_m < tolerance_m {
                            *reduced = original.clone();
                        }
                    }
                }
                match &simple {
                    rustgis_data::Geometry::Polygon(polys) if polys.iter().flatten().all(|ring| ring.len() >= 4) => simple,
                    _ => g.clone(),
                }
            };
            m.layers = src
                .layers
                .iter()
                .filter(|l| {
                    src.shows(l) && !l.is_raster() && l.source.is_none() && l.geometry_type() == rustgis_data::GeometryType::Polygon
                })
                .map(|l| {
                    // (Sharing the rows: `Features` is copy-on-write, replaced just below.)
                    let mut layer = l.clone();
                    layer.renew_instance_id();
                    // Simplified sketches aren't the source's rows: saving them would overwrite it.
                    layer.gpkg = None;
                    layer.postgis = None;
                    layer.dataset.features = l
                        .dataset
                        .features
                        .iter()
                        .map(|f| {
                            let mut f = f.clone();
                            f.geometry = f.geometry.as_ref().map(sketch);
                            f
                        })
                        .collect();
                    layer.touch_geometry();
                    layer.selection.clear();
                    layer.labels.enabled = false;
                    layer
                })
                .collect();
            let name = m.name.clone();
            match orphan.and_then(|i| s.project.maps.get_mut(i)) {
                Some(old) => {
                    *old = m;
                    warnings.push(format!("the map '{name}', which no layout showed, is replaced by this sheet's globe"));
                }
                None => s.project.maps.push(m),
            }
            Some((name, globe, FrameShape::Circle))
        }
        Locator::Inset => Some((s.map().name.clone(), round_scale(scale * fw / side * 4.0), FrameShape::Rectangle)),
    };
    // The legend lists what the frame draws: its map synced to the frame's view first, as an
    // export does, so a streamed layer has its features there. (This changes the session's map,
    // so it comes after the last refusal: nothing below fails.) While one is still reading, the
    // classes can't be known: the legend gets a fixed size, to fit by hand. Nothing drawn in the
    // frame, no legend.
    let render_scale = crate::render::layout::frame_render_scale(s.map(), center, scale);
    let view = crate::render::View {
        width: fw * rustgis_map::layout::PT_PER_MM,
        height: fh * rustgis_map::layout::PT_PER_MM,
        pixels_per_point: 1.0,
        camera: rustgis_map::layout::frame_camera(s.map(), center, scale, 0.0, 0.0),
        sky: false,
    };
    let lonlat = crate::render::view_lonlat(s.map(), &view);
    let reading = s.map_mut().sync_sources(&lonlat, render_scale, true);
    let mut sizes = crate::render::layout::legend_sizes_mm(s.map_mut(), render_scale, false, false, &[]);
    let (mut cw, mut ch) = match sizes {
        Some(sizes) => sizes[0],
        None => {
            // A complete source that can't be read: the export says why; the legend is sized by hand.
            warnings.push("a layer's source couldn't be read through, so the legend has a fixed size; exporting says why".into());
            (fw * 0.3, fh * 0.25)
        }
    };
    let listed = s.map().layers.iter().any(|l| s.map().shows(l) && !l.graphics && !l.is_raster());
    if reading && listed {
        (cw, ch) = (cw.max(fw * 0.3), ch.max(fh * 0.25));
        sizes = None;
        warnings.push("the legend's classes aren't known yet (a layer still reading): it has a fixed size to fit by hand".into());
    } else if crate::render::layout::tile_legends_pending(s.map(), render_scale, &[]) {
        // Not waited for: a slow legend server would hold the command up.
        (cw, ch) = (cw.max(fw * 0.3), ch.max(fh * 0.25));
        sizes = None;
        warnings.push("a WMS or WMTS layer's legend picture is still loading: the legend has a fixed size to fit by hand".into());
    }
    let (sw, sh, nh) = ((fw * 0.3).max(25.0), if sheet { 8.0 * k } else { 6.0 }, pitch(small_pt) + 0.5);
    let pad = 2.0;
    // What the map draws where, on a grid of the frame (its basemap left out). The map
    // itself is drawn for it, on purpose, as the screen draws it: a grid of 384 cells, a small part
    // of what exporting the sheet draws; a streamed layer draws what it holds and starts reading
    // the frame's area in the background (which the export reads anyway); web tiles aren't fetched.
    let coverage = crate::render::layout::frame_coverage(s.map_mut(), center, scale, fw, fh, 384);
    // Its cells summed from the top left (a summed-area table), so a rectangle's count is four
    // lookups, however large: the search off the corners tries thousands.
    let sums: Option<(usize, usize, Vec<u32>)> = coverage.as_ref().map(|(gw, gh, cells)| {
        let (gw, gh) = (*gw as usize, *gh as usize);
        let mut t = vec![0u32; (gw + 1) * (gh + 1)];
        for r in 0..gh {
            for c in 0..gw {
                let v = u32::from(cells.get(r * gw + c).copied().unwrap_or(false));
                t[(r + 1) * (gw + 1) + c + 1] = v + t[r * (gw + 1) + c + 1] + t[(r + 1) * (gw + 1) + c] - t[r * (gw + 1) + c];
            }
        }
        (gw, gh, t)
    });
    // The share of a rectangle (in page mm) over the frame's drawn cells.
    let covered = |(x, y, w, h): (f64, f64, f64, f64)| -> f64 {
        let Some((gw, gh, t)) = &sums else { return 0.0 };
        let (gw, gh) = (*gw, *gh);
        let col = |v: f64| ((v - fx) / fw * gw as f64).floor().clamp(0.0, gw as f64) as usize;
        let row = |v: f64| ((v - fy) / fh * gh as f64).floor().clamp(0.0, gh as f64) as usize;
        let (c0, r0) = (col(x).min(gw.saturating_sub(1)), row(y).min(gh.saturating_sub(1)));
        let (c1, r1) = (col(x + w).max(c0 + 1).min(gw), row(y + h).max(r0 + 1).min(gh));
        let n = (c1 - c0) * (r1 - r0);
        let at = |r: usize, c: usize| f64::from(t[r * (gw + 1) + c]);
        let drawn = at(r1, c1) - at(r0, c1) - at(r1, c0) + at(r0, c0);
        if n == 0 { 0.0 } else { drawn / n as f64 }
    };
    // The surrounds to place: what each is and its size (the arrangement gives its corner).
    let mut surrounds: Vec<(Surround, f64, f64)> = Vec::new();
    if locator_map.is_some() && column.is_none() {
        surrounds.push((Surround::Locator, side, side));
    }
    surrounds.push((Surround::Arrow, aw, ah));
    // The topmost visible raster drawn on a ramp gets a colorbar, under the legend in its box (a
    // map whose theme is a raster has its key there, which the legend's rows don't give).
    // `"colorbar": "none"` leaves it out, and its room in the box with it (#1680).
    let bar = rustgis_map::layout::colorbar_layer(s.map(), None)
        .filter(|l| colorbar && l.raster.as_ref().and_then(rustgis_map::layout::color_scale).is_some())
        .map(|l| l.name.clone());
    let (bar_w, bar_h) = (if sheet { 50.0 * k } else { 40.0 }, COLORBAR_MM);
    let bar_part = if bar.is_some() { bar_h + if ch > 0.0 { 2.0 } else { 0.0 } } else { 0.0 };
    let (content_w, content_h) = (if bar.is_some() { cw.max(bar_w) } else { cw }, ch + bar_part);
    let legend_in_column = column.is_some();
    if content_h > 0.0 && !legend_in_column {
        surrounds.push((Surround::Legend, (content_w + 2.0 * pad).min(fw * 0.45), (content_h + 2.0 * pad).min(fh * 0.6)));
    }
    surrounds.push((Surround::Scale, sw, sh + nh));
    // A corner's rectangle for a surround `w` × `h`.
    let at = |corner: usize, w: f64, h: f64| {
        let x = if corner.is_multiple_of(2) { fx + left } else { fx + fw - right - w };
        let y = if corner < 2 { fy + top } else { fy + fh - bottom - h };
        (x, y, w, h)
    };
    // What each surround is called in the answer and in warnings.
    let named = |kind: Surround| match kind {
        Surround::Locator => "locator",
        Surround::Arrow => "north arrow",
        Surround::Legend => "legend",
        Surround::Scale => "scale bar",
    };
    let area = (fx + left, fy + top, fx + fw - right, fy + fh - bottom);
    // An arrangement laid out (#1379), the legend's box `legend` at most: each surround over the map
    // in its own corner, the legend short of the surrounds beside and above or below it (`None` when
    // the locator leaves it too small, so it is left out); then, the largest first, each one covering
    // what the map draws there leaves it for the freest place in the frame, clear of those placed
    // before it ([`free_spot`]), when that covers less, and one whose corner a larger one took leaves
    // it in any case. Data everywhere alike leaves nowhere better: it stays. Gives the legend's corner
    // and size, each surround's place, the ones moved, and the area (mm²) of those moved and of what
    // still covers the drawing.
    type Laid = (Option<(usize, f64, f64)>, Vec<(Surround, Rect)>, Vec<&'static str>, (f64, f64));
    let lay_out = |homes: Homes, legend: (f64, f64)| -> Laid {
        let corner_of = |what: Surround| {
            surrounds.iter().find(|(kind, ..)| *kind == what).map(|&(kind, w, h)| {
                let (w, h) = if kind == Surround::Legend { legend } else { (w, h) };
                (homes[home_slot(kind)], w, h)
            })
        };
        let legend_at = corner_of(Surround::Legend).and_then(|(corner, w, h)| {
            let other = |c: usize| surrounds.iter().find(|&&(kind, ..)| homes[home_slot(kind)] == c).copied();
            let beside = other(corner ^ 1).map(|(kind, ow, _)| (kind, ow));
            let over = other(corner ^ 2).map(|(kind, _, oh)| (kind, oh));
            legend_room(w, h, fw - left - right - 2.0, fh - top - bottom - e, beside, over).map(|(lw, lh)| (corner, lw, lh))
        });
        let mut spots: Vec<(Surround, Rect)> = [Surround::Locator, Surround::Arrow, Surround::Legend, Surround::Scale]
            .into_iter()
            .filter_map(|kind| match kind {
                Surround::Legend => legend_at.map(|(c, lw, lh)| (kind, at(c, lw, lh))),
                _ => corner_of(kind).map(|(c, w, h)| (kind, at(c, w, h))),
            })
            .collect();
        // The largest first, the legend as a rule: it has the most to lose, so it takes the freest
        // place, and a smaller surround whose corner it took moves out of its way.
        let mut order: Vec<usize> = (0..spots.len()).collect();
        order.sort_by(|&a, &b| (spots[b].1.2 * spots[b].1.3).total_cmp(&(spots[a].1.2 * spots[a].1.3)));
        let mut moved: Vec<&'static str> = Vec::new();
        let mut settled: Vec<Rect> = Vec::new();
        for i in order {
            let (kind, home) = spots[i];
            let here = covered(home);
            let overlaps = |r: Rect| settled.iter().any(|o| r.0 < o.0 + o.2 && o.0 < r.0 + r.2 && r.1 < o.1 + o.3 && o.1 < r.1 + r.3);
            let taken = overlaps(home);
            if here > LEAK || taken {
                // Out of a corner taken by a larger one whatever it covers; off the data only when
                // that covers less.
                if let Some(r) = free_spot(home, area, &settled, covered).filter(|r| taken || covered(*r) < here - LEAK) {
                    spots[i].1 = r;
                    moved.push(named(kind));
                }
            }
            settled.push(spots[i].1);
        }
        // Named in the order the surrounds are listed, whatever order they were placed in.
        moved.sort_by_key(|m| spots.iter().position(|(kind, _)| named(*kind) == *m));
        let shifted: f64 = spots.iter().filter(|(kind, _)| moved.contains(&named(*kind))).map(|(_, r)| r.2 * r.3).sum();
        let over: f64 = spots.iter().map(|(_, r)| covered(*r) * r.2 * r.3).sum();
        // A legend left out costs the whole frame, more than any move, so an arrangement that keeps
        // it always wins over one that drops it.
        let dropped = if surrounds.iter().any(|x| x.0 == Surround::Legend) && legend_at.is_none() { fw * fh } else { 0.0 };
        // A legend cut short of its neighbours costs ten times what it loses, so one that keeps its
        // rows beats moving another surround.
        let cut = legend_at.map_or(0.0, |(_, lw, lh)| 10.0 * (legend.0 * legend.1 - lw * lh).max(0.0));
        // Surrounds left over each other (nowhere clear for one a larger one's corner took) cost ten
        // times their overlap, so an arrangement that keeps them apart wins.
        let clash = 10.0 * overlap_area(&spots.iter().map(|(_, r)| *r).collect::<Vec<_>>());
        (legend_at, spots, moved, (shifted + dropped + cut + clash, over))
    };
    // Each arrangement tried laid out, and one picked ([`pick_arrangement`]): the least area moved
    // off its corners (so a north arrow moved costs less than a legend), then the least left over
    // the drawing, the first tried (the favourite) on a tie. Every arrangement fills the same four
    // corners, so it is what goes where that differs. A single arrangement asked for is the one
    // laid out.
    let arranged = |legend: (f64, f64)| -> (Homes, Laid) {
        let mut laid: Vec<(Homes, Laid)> = tries.iter().map(|h| (*h, lay_out(*h, legend))).collect();
        let chosen = pick_arrangement(&laid.iter().map(|(_, l)| l.3).collect::<Vec<_>>()).map(|i| laid.swap_remove(i));
        chosen.unwrap_or_else(|| (ARRANGEMENTS[0].1, lay_out(ARRANGEMENTS[0].1, legend)))
    };
    let at_most_one = surrounds.iter().find(|x| x.0 == Surround::Legend).map_or((0.0, 0.0), |x| (x.1, x.2));
    let mut legend_cols = 1;
    let mut best = arranged(at_most_one);
    // The legend takes the fewest columns that fit the largest rectangle the map leaves free
    // (#1379). Past those, or when none fits, a long one is tried in more, up to the width beside
    // the north arrow, when in fewer it is cut or over what the map draws. One in a side column
    // stays in one: that column is about as wide as its rows.
    if let Some(sizes) = sizes.filter(|_| column.is_none() && content_h > 0.0) {
        let whole = |(w, h): (f64, f64)| ((if bar.is_some() { w.max(bar_w) } else { w }) + 2.0 * pad, h + bar_part + 2.0 * pad);
        let shown = |laid: &Laid, (w, h): (f64, f64)| laid.0.map_or(0.0, |(_, lw, lh)| (lw / w).min(1.0) * (lh / h).min(1.0));
        let long = whole(sizes[0]).1 > fh * 0.25;
        let mut score = (shown(&best.1, whole(sizes[0])), best.1.3.1);
        let wide = fw - left - right - 2.0 - aw;
        let free = coverage.as_ref().map(|(gw, gh, cells)| {
            let (gw, gh) = (*gw as usize, *gh as usize);
            let col = |v: f64| ((v - fx) / fw * gw as f64).clamp(0.0, gw as f64) as usize;
            let row = |v: f64| ((v - fy) / fh * gh as f64).clamp(0.0, gh as f64) as usize;
            let (cw, ch) = largest_free_cells(gw, cells, (col(area.0), row(area.1), col(area.2), row(area.3)));
            (cw as f64 * fw / gw as f64, ch as f64 * fh / gh as f64)
        });
        let fits = free.and_then(|(rw, rh)| sizes.iter().position(|size| whole(*size).0 <= rw && whole(*size).1 <= rh));
        // One column too, when the rectangle holds what the box a legend starts with cuts.
        if let Some(i) = fits.filter(|i| *i > 0 || whole(sizes[0]).0 > at_most_one.0 + 1e-6 || whole(sizes[0]).1 > at_most_one.1 + 1e-6) {
            let (w, h) = whole(sizes[i]);
            let more = arranged((w.min(wide), h));
            let new = (shown(&more.1, (w, h)), more.1.3.1);
            if more.1.0.is_some() && new.0 >= score.0 - 1e-6 {
                (legend_cols, best, score) = (i + 1, more, new);
            }
        }
        for (i, size) in sizes.iter().enumerate().skip(legend_cols) {
            if score.0 > 1.0 - 1e-6 && (!long || score.1 <= LEAK * fw * fh) {
                break;
            }
            let (w, h) = whole(*size);
            let more = arranged((w.min(wide), h.min(fh * 0.6)));
            let new = (shown(&more.1, (w, h)), more.1.3.1);
            if more.1.0.is_some() && more_columns_win(new, score) {
                (legend_cols, best, score) = (i + 1, more, new);
            }
        }
        (cw, ch) = sizes[legend_cols - 1];
    }
    let (homes, (legend_at, spots, moved, _)) = best;
    if surrounds.iter().any(|x| x.0 == Surround::Legend) && legend_at.is_none() {
        warnings.push(
            "the frame leaves the legend no room beside the locator, so it is left out: give the map more room, or set \"locator\": \"none\""
                .into(),
        );
    }
    let mut placed: Vec<(&'static str, (f64, f64, f64, f64))> = Vec::new();
    let spot_of = |what: Surround| spots.iter().find(|(kind, _)| *kind == what).map(|(_, r)| *r);
    // In a column, at its top, centred across it; otherwise in its corner over the map.
    let locator_rect = match column {
        Some(c) => Some((c.x + (c.w - side) / 2.0, c.y, side, side)),
        None => spot_of(Surround::Locator).inspect(|&r| placed.push(("locator", r))),
    };
    // A page of cards gives the locator in its column a card of the column's width.
    let card = style == Style::Modular && column.is_some();
    let (card_r, card_pad) = (3.0 * k, 2.5 * k);
    let surround_box = |fill: Rgba| if style.cards() { pub_card(fill, clear, 0.0, 2.0 * k) } else { pub_box(fill, clear, 0.0) };
    if let (Some((map, locator_scale, shape)), Some(r)) = (&locator_map, locator_rect) {
        if let (true, Some(c)) = (card, column) {
            l.add(
                "Locator Card",
                (c.x - card_pad, r.1 - card_pad, c.w + 2.0 * card_pad, r.3 + 2.0 * card_pad),
                pub_card(Rgba::rgb(0xFF, 0xFF, 0xFF), Rgba::new(0x2B, 0x26, 0x20, 60), 0.4, card_r),
            );
        }
        l.add(
            "Locator",
            r,
            ElementKind::MapFrame {
                map: map.clone(),
                center,
                scale: *locator_scale,
                shape: shape.clone(),
                grid: None,
                extent_of: Some(frame),
                extent_color: rustgis_map::layout::INDICATOR_COLOR,
                extent_width: rustgis_map::layout::INDICATOR_WIDTH,
                bearing: 0.0,
                pitch: 0.0,
            },
        );
    }
    if let Some(r) = spot_of(Surround::Arrow) {
        placed.push(("north arrow", r));
        l.add("North Arrow", r, ElementKind::north_arrow(frame));
    }
    // In a panel, the legend under the locator, as tall as it needs within what is left.
    let legend_rect = match column {
        Some(c) if content_h > 0.0 => {
            let ly = c.y + if locator_map.is_some() { side + 3.0 * k + if card { 2.0 * card_pad } else { 0.0 } } else { 0.0 };
            let room_h = c.y + c.h - ly;
            if room_h < MIN_LEGEND_MM {
                warnings.push("the side column leaves the legend no room under the locator, so it is left out: shorten its texts".into());
                None
            } else {
                // On a page of cards, a card as wide as the locator's.
                let (x, w) = if card { (c.x - card_pad, c.w + 2.0 * card_pad) } else { (c.x, c.w) };
                Some((x, ly, w, (content_h + 2.0 * pad).min(room_h)))
            }
        }
        Some(_) => None,
        None => spot_of(Surround::Legend).inspect(|&r| placed.push(("legend", r))),
    };
    if let Some(r) = legend_rect {
        let (lw, lh) = (r.2, r.3);
        l.add(
            "Legend Box",
            r,
            if card { pub_card(Rgba::rgb(0xFF, 0xFF, 0xFF), Rgba::new(0x2B, 0x26, 0x20, 60), 0.4, card_r) } else { surround_box(paper) },
        );
        // The colorbar at the bottom of the box, as tall as the box allows; the legend's rows in
        // what is left above it. Each says when it doesn't fit, rather than one blaming the other.
        let (ix, iy, iw, ih) = (r.0 + pad, r.1 + pad, lw - 2.0 * pad, lh - 2.0 * pad);
        let bar_fit = if bar.is_some() { bar_h.min(ih) } else { 0.0 };
        let rows_h = (ih - bar_fit - if bar.is_some() { bar_part - bar_h } else { 0.0 }).max(0.0);
        warnings.extend(legend_fit_warnings(legend_cols, (cw, ch), (iw, rows_h)).into_iter().map(String::from));
        // In a column the colorbar takes the column's width, as its rows do.
        if bar.is_some() && ((bar_w > iw + LEGEND_FIT_MM && !legend_in_column) || bar_h > ih + LEGEND_FIT_MM) {
            warnings.push("the legend's box is smaller than the colorbar: it is drawn narrower or shorter".into());
        }
        if ch > 0.0 && rows_h >= 1.0 {
            l.add(
                "Legend",
                (ix, iy, iw, rows_h),
                ElementKind::Legend {
                    frame,
                    title: String::new(),
                    all_classes: false,
                    hide_other: false,
                    exclude: Vec::new(),
                    columns: legend_cols,
                },
            );
        }
        if let Some(layer) = &bar {
            l.add(
                "Colorbar",
                (ix, iy + ih - bar_fit, iw, bar_fit),
                ElementKind::Colorbar {
                    frame,
                    layer: Some(layer.clone()),
                    title: layer.clone(),
                    orientation: Orientation::Horizontal,
                    ticks: 5,
                    tick_side: TickSide::End,
                    reverse: false,
                },
            );
        }
    }
    if let Some((sx, sy, w, h)) = spot_of(Surround::Scale) {
        placed.push(("scale bar", (sx, sy, w, h)));
        l.add("Scale Box", (sx, sy, sw, sh + nh), surround_box(paper));
        l.add("Scale Bar", (sx, sy, sw, sh), ElementKind::scale_bar(frame));
        l.add("Scale", (sx, sy + sh, sw, nh), pub_text("{scale}".into(), small_pt, ink, false, TextAlign::Center));
    }
    // What placing couldn't avoid: a surround over the map's drawing while a clearly freer corner
    // went to another (more surrounds than free corners), or two overlapping (a frame too small
    // for them all). Data under every corner alike (a context layer filling the frame) leaves
    // nothing to move, so it isn't said. One warning names them all.
    let crowded: Vec<&str> = placed
        .iter()
        .filter(|(_, r)| {
            let share = covered(*r);
            let freest = (0..4).map(|c| covered(at(c, r.2, r.3))).fold(f64::INFINITY, f64::min);
            share > OVER_DATA && share - freest > OVER_DATA
        })
        .map(|(what, _)| *what)
        .collect();
    if let Some((last, rest)) = crowded.split_last() {
        let (names, verb) = match rest {
            [] => (format!("the {last}"), "sits"),
            _ => (format!("the {} and the {last}", rest.join(", the ")), "sit"),
        };
        warnings
            .push(format!("{names} {verb} on what the map shows, the freer places having gone to the others: move by hand what matters"));
    }
    for (i, (a, ra)) in placed.iter().enumerate() {
        for (b, rb) in placed.iter().skip(i + 1) {
            if ra.0 < rb.0 + rb.2 && rb.0 < ra.0 + ra.2 && ra.1 < rb.1 + rb.3 && rb.1 < ra.1 + ra.3 {
                warnings.push(format!("the {a} and the {b} overlap: the frame is too small for both"));
            }
        }
    }
    for (n, r, kind, _) in over {
        l.add(&n, r, kind);
    }
    for (n, r, kind) in around {
        l.add(&n, r, kind);
    }
    // Every element by name and id, for `layout.update`.
    let elements: Vec<Value> = l.elements.iter().map(|e| json!({"element": e.id, "name": e.name, "kind": e.kind.code()})).collect();
    if s.project.layouts.len() >= crate::project::MAX_LAYOUTS {
        return Err(format!("a project holds at most {} layouts; remove one first", crate::project::MAX_LAYOUTS));
    }
    s.project.layouts.push(l);
    let i = s.project.layouts.len() - 1;
    s.active_layout = Some(i);
    let locator = locator_map.map(|(map, ..)| map);
    Ok(
        json!({"layout": i, "name": name, "profile": profile.id, "page": [w, h], "scale": scale, "grid": step, "locator": locator, "style": style.code(), "arrangement": arrangement_name(homes), "legendColumns": legend_cols, "elements": elements, "moved": moved, "warnings": warnings}),
    )
}

fn page(s: &mut Session, p: &Value) -> CmdResult {
    let i = index(s, p)?;
    let cur = s.project.layouts.get(i).map(|l| (l.width, l.height));
    let (w, h) = page_size(p, cur)?;
    let comma = decimal_mark(p)?;
    let l = s.project.layouts.get_mut(i).ok_or("no layout")?;
    l.width = w;
    l.height = h;
    if let Some(comma) = comma {
        l.decimal_comma = comma;
    }
    Ok(json!({"width": w, "height": h, "decimal": if l.decimal_comma { "," } else { "." }}))
}

/// Sets a layout's ruler guides: the lists given replace those there.
fn guides(s: &mut Session, p: &Value) -> CmdResult {
    let read = |k: &str| -> Result<Option<Vec<f64>>, String> {
        let bad = || format!("{k} is a list of positions in mm");
        match p.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Array(a)) if a.len() <= MAX_GUIDES => a
                .iter()
                .map(|v| v.as_f64().filter(|v| v.is_finite() && v.abs() <= 5000.0).ok_or_else(bad))
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Some(Value::Array(_)) => Err(format!("a layout has at most {MAX_GUIDES} guides each way")),
            Some(_) => Err(bad()),
        }
    };
    // Checked before anything changes.
    let (x, y) = (read("x")?, read("y")?);
    let l = active(s, p)?;
    if b(p, "clear") == Some(true) {
        l.guides = Guides::default();
    }
    if let Some(x) = x {
        l.guides.x = x;
    }
    if let Some(y) = y {
        l.guides.y = y;
    }
    Ok(json!({"x": l.guides.x, "y": l.guides.y}))
}

/// Copies an element, offset a little so the copy shows, on top of the others.
fn duplicate_element(s: &mut Session, p: &Value) -> CmdResult {
    let id = element_id(p)?;
    let (dx, dy) = (f(p, "dx").unwrap_or(5.0), f(p, "dy").unwrap_or(5.0));
    if dx.abs() > 5000.0 || dy.abs() > 5000.0 {
        return Err("the copy's offset is up to 5000 mm".into());
    }
    let l = active(s, p)?;
    let e = l.element(id).cloned().ok_or_else(|| format!("no element {id}"))?;
    let name = numbered(&e.name, |n| l.elements.iter().any(|x| x.name == n));
    let copy = l.add(name.clone(), (e.x + dx, e.y + dy, e.w, e.h), e.kind);
    if let Some(c) = l.element_mut(copy) {
        c.visible = e.visible;
    }
    Ok(json!({"element": copy, "name": name}))
}

/// A shape's "width" in points: `default` when it's not given (or null), an error when it's not a
/// number.
fn shape_width(p: &Value, default: f64) -> Result<f64, String> {
    match p.get("width").filter(|v| !v.is_null()) {
        Some(_) => f(p, "width").ok_or_else(|| "width is a number of points".to_string()),
        None => Ok(default),
    }
}

/// A line's "head": `default` when it's not given (or null), an error when it's not an
/// arrowhead's name.
fn line_head(p: &Value, default: LineHead) -> Result<LineHead, String> {
    match p.get("head").filter(|v| !v.is_null()) {
        Some(v) => v.as_str().and_then(LineHead::from_name).ok_or_else(|| format!("unknown arrowhead {v} (none, end, start or both)")),
        None => Ok(default),
    }
}

/// A number given as `k`, when it's given (and not null): an error when it's not a number.
fn given_num(p: &Value, k: &str, what: &str) -> Result<Option<f64>, String> {
    match p.get(k).filter(|v| !v.is_null()) {
        Some(_) => f(p, k).map(Some).ok_or_else(|| format!("{k} is {what}")),
        None => Ok(None),
    }
}

/// A line's "dir": `default` when it's not given (or null), an error when it's not a direction's
/// name.
fn line_dir(p: &Value, default: LineDir) -> Result<LineDir, String> {
    match p.get("dir").filter(|v| !v.is_null()) {
        Some(v) => v
            .as_str()
            .and_then(LineDir::from_name)
            .ok_or_else(|| format!("unknown line direction {v} (horizontal, vertical, falling or rising)")),
        None => Ok(default),
    }
}

/// The element's box: given values, else a default inside the page.
fn place(p: &Value, l: &Layout, (dx, dy, dw, dh): (f64, f64, f64, f64)) -> Result<(f64, f64, f64, f64), String> {
    let v = |k: &str, d: f64| f(p, k).unwrap_or(d);
    let (x, y, w, h) = (v("x", dx), v("y", dy), v("w", dw.min(l.width - 10.0).max(5.0)), v("h", dh.min(l.height - 10.0).max(5.0)));
    if w <= 0.0 || h <= 0.0 || w > 5000.0 || h > 5000.0 || x.abs() > 5000.0 || y.abs() > 5000.0 {
        return Err("element sizes are more than 0 and up to 5000 mm".into());
    }
    Ok((x, y, w, h))
}

fn add_frame(s: &mut Session, p: &Value) -> CmdResult {
    let shape = match ps(p, "shape").unwrap_or("rectangle") {
        "rectangle" => FrameShape::Rectangle,
        "circle" => FrameShape::Circle,
        "ellipse" => FrameShape::Ellipse,
        "polygon" | "lasso" => {
            let pts: Vec<(f64, f64)> = p
                .get("points")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .take(10_000)
                        .filter_map(|v| Some((v.get(0)?.as_f64()?, v.get(1)?.as_f64()?)))
                        .filter(|(u, v)| u.is_finite() && v.is_finite())
                        .map(|(u, v)| (u.clamp(0.0, 1.0), v.clamp(0.0, 1.0)))
                        .collect()
                })
                .unwrap_or_else(|| vec![(0.5, 0.0), (1.0, 0.38), (0.81, 1.0), (0.19, 1.0), (0.0, 0.38)]);
            if pts.len() < 3 {
                return Err("a polygon frame needs at least three points".into());
            }
            FrameShape::Polygon(pts)
        }
        o => return Err(format!("unknown frame shape '{o}'")),
    };
    let map = added_frame_map(s, p)?.name.clone();
    let i = index(s, p)?;
    let l = s.project.layouts.get(i).ok_or("no layout")?;
    let n = l.elements.len() as f64;
    let rect = place(p, l, (15.0 + n * 5.0, 25.0 + n * 5.0, l.width * 0.5, l.height * 0.5))?;
    let kind = frame_kind(s, &map, rect.2, shape);
    let l = s.project.layouts.get_mut(i).ok_or("no layout")?;
    let id = l.add("Map Frame", rect, kind);
    Ok(json!({"element": id}))
}

fn frame_of(l: &Layout, p: &Value) -> Result<u32, String> {
    match p.get("frame").and_then(Value::as_u64) {
        Some(id) => {
            let id = u32::try_from(id).map_err(|_| "no such frame")?;
            match l.element(id).map(|e| &e.kind) {
                Some(ElementKind::MapFrame { .. }) => Ok(id),
                _ => Err(format!("element {id} isn't a map frame")),
            }
        }
        None => l.first_frame().ok_or_else(|| "add a map frame first".to_string()),
    }
}

/// A rectangle's `radius`: its corners rounded by up to 50 mm; `d` when not given.
fn corner_radius(p: &Value, d: f64) -> Result<f64, String> {
    match p.get("radius").filter(|v| !v.is_null()) {
        None => Ok(d),
        Some(v) => {
            v.as_f64().filter(|r| r.is_finite() && *r >= 0.0).map(|r| r.min(50.0)).ok_or_else(|| "radius is millimetres, 0 or more".into())
        }
    }
}

/// The colour `p` gives as `k`, or `d` when it gives none (absent or null). Any other value is
/// refused rather than ignored.
pub(crate) fn color(p: &Value, k: &str, d: Rgba) -> Result<Rgba, String> {
    let bad = || format!("{k} must be #RRGGBB or #RRGGBBAA");
    match p.get(k).filter(|v| !v.is_null()) {
        Some(v) => v.as_str().and_then(Rgba::from_hex).ok_or_else(bad),
        None => Ok(d),
    }
}

const ONLY_LEGEND_COLUMNS: &str = "only legends have columns";

/// A box sized to its content, less the padding again, can come out a rounding error short of it:
/// that still fits.
const LEGEND_FIT_MM: f64 = 1e-6;

/// What a legend's rows `size` in `columns` columns lose in a box `room`, in mm, each said once.
fn legend_fit_warnings(columns: usize, (w, h): (f64, f64), (room_w, room_h): (f64, f64)) -> Vec<&'static str> {
    // With no rows there is nothing to lose, in a box shorter than its title too.
    let (too_wide, too_tall) = (h > 0.0 && w > room_w + LEGEND_FIT_MM, h > 0.0 && h > room_h + LEGEND_FIT_MM);
    let mut said = Vec::new();
    if columns > 1 {
        // Its columns close up rather than leave one out.
        if too_wide {
            said.push("the legend's columns are wider than its box: they close up, long labels run into the next, and the last column's past the box");
        }
        if too_tall {
            said.push("the legend lists more than fits its box: its last rows are left out");
        }
    } else if too_wide || too_tall {
        said.push("the legend lists more than fits its box: its last rows or the ends of long labels are left out");
    }
    said
}

/// Says what a legend loses in its box, as `layout.publication` does for its own, or that it can't
/// tell.
fn say_legend_fit(s: &mut Session, li: usize, id: u32) {
    let Some(l) = s.project.layouts.get(li) else { return };
    let Some(e) = l.element(id) else { return };
    let ElementKind::Legend { frame, title, all_classes, hide_other, exclude, columns } = &e.kind else { return };
    let shown = match l.element(*frame).map(|f| &f.kind) {
        Some(ElementKind::MapFrame { map, center, scale, .. }) => Some((map.clone(), *center, *scale)),
        _ => None,
    };
    let (room_w, room_h) = (e.w, e.h - if title.is_empty() { 0.0 } else { crate::render::layout::legend_title_mm() });
    let (all_classes, hide_other, exclude, columns) = (*all_classes, *hide_other, exclude.clone(), *columns);
    let found = shown
        .and_then(|(name, center, scale)| Some((s.project.maps.iter_mut().find(|m| m.name.eq_ignore_ascii_case(&name))?, center, scale)));
    let Some((map, center, scale)) = found else {
        s.warn("the legend's frame shows no map of this project, so the legend lists nothing");
        return;
    };
    let at = crate::render::layout::frame_render_scale(map, center, scale);
    let Some(sizes) = crate::render::layout::legend_sizes_mm(map, at, all_classes, hide_other, &exclude) else {
        s.warn("a layer's source couldn't be read through, so the legend's fit in its box wasn't checked; exporting says why");
        return;
    };
    let size = sizes[columns.clamp(1, MAX_LEGEND_COLUMNS) - 1];
    for said in legend_fit_warnings(columns, size, (room_w, room_h)) {
        s.warn(said);
    }
}

/// The `"columns"` a legend is given, refused outside 1 to [`MAX_LEGEND_COLUMNS`].
fn legend_columns_param(p: &Value) -> Result<Option<usize>, String> {
    let Some(v) = p.get("columns").filter(|v| !v.is_null()) else { return Ok(None) };
    v.as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| (1..=MAX_LEGEND_COLUMNS).contains(n))
        .map(Some)
        .ok_or_else(|| format!("columns is a whole number from 1 to {MAX_LEGEND_COLUMNS}"))
}

fn add_element(s: &mut Session, p: &Value) -> CmdResult {
    let kind = ps(p, "kind").ok_or("kind is required")?;
    if kind != "legend" && p.get("exclude").is_some_and(|v| !v.is_null()) {
        return Err("only legends leave layers out".into());
    }
    if kind != "legend" && p.get("columns").is_some_and(|v| !v.is_null()) {
        return Err(ONLY_LEGEND_COLUMNS.into());
    }
    // A family is a text's: refused elsewhere, as a legend's left-out layers are, so a script
    // doesn't take it as applied.
    if !matches!(kind, "text" | "dynamicText") && gives_font(p) {
        return Err(ONLY_TEXT_FONT.into());
    }
    if kind != "rectangle" && p.get("radius").is_some_and(|v| !v.is_null()) {
        return Err(ONLY_RECTANGLE_RADIUS.into());
    }
    if !matches!(
        kind,
        "text" | "dynamicText" | "rectangle" | "ellipse" | "circle" | "line" | "arrow" | "extentIndicator" | "grid" | "scaleBar"
    ) && gives(p, "color")
    {
        return Err(ONLY_COLOR_ADDED.into());
    }
    let shape = matches!(kind, "northArrow" | "rectangle" | "ellipse" | "circle");
    if gives(p, "fill") && !shape && kind != "scaleBar" {
        return Err(ONLY_FILL.into());
    }
    if gives(p, "outline") && !shape {
        return Err(ONLY_OUTLINE.into());
    }
    let i = index(s, p)?;
    let image = if kind == "picture" { Some(super::graphics::picture(p)?) } else { None };
    let l = s.project.layouts.get(i).ok_or("no layout")?;
    let (pw, ph) = (l.width, l.height);
    let (name, rect, k) = match kind {
        "northArrow" => {
            let frame = frame_of(l, p)?;
            let k = restyle(&ElementKind::north_arrow(frame), p)?.unwrap_or(ElementKind::north_arrow(frame));
            ("North Arrow", place(p, l, (pw - 30.0, 15.0, 16.0, 24.0))?, k)
        }
        "scaleBar" => {
            let frame = frame_of(l, p)?;
            let k = restyle(&ElementKind::scale_bar(frame), p)?.unwrap_or(ElementKind::scale_bar(frame));
            ("Scale Bar", place(p, l, (15.0, ph - 25.0, 70.0, 12.0))?, k)
        }
        "legend" => {
            let frame = frame_of(l, p)?;
            let title = ps(p, "title").unwrap_or("Legend").chars().take(200).collect();
            let all_classes = b(p, "allClasses").unwrap_or(false);
            let hide_other = b(p, "hideOther").unwrap_or(false);
            let exclude = legend_exclude(s, l, frame, p)?.unwrap_or_default();
            let columns = legend_columns_param(p)?.unwrap_or(1);
            (
                "Legend",
                place(p, l, (pw - 70.0, 45.0, 60.0, 80.0))?,
                ElementKind::Legend { frame, title, all_classes, hide_other, exclude, columns },
            )
        }
        "colorbar" => {
            let frame = frame_of(l, p)?;
            let layer = colorbar_layer(s, l, frame, ps(p, "layer"))?;
            let title = ps(p, "title").map(|t| t.chars().take(200).collect()).unwrap_or_else(|| layer.clone().unwrap_or_default());
            let orientation = orientation(p)?.unwrap_or_default();
            let rect = match orientation {
                Orientation::Horizontal => (15.0, ph - 45.0, 80.0, 16.0),
                Orientation::Vertical => (pw - 40.0, 45.0, 25.0, 80.0),
            };
            let k = ElementKind::Colorbar {
                frame,
                // Kept only when named: otherwise (none, or "") it follows the map's top visible raster.
                layer: ps(p, "layer").filter(|n| !n.trim().is_empty()).and(layer),
                title,
                orientation,
                ticks: ticks(p).unwrap_or(5),
                tick_side: tick_side(p)?.unwrap_or_default(),
                reverse: b(p, "reverse").unwrap_or(false),
            };
            ("Colorbar", place(p, l, rect)?, k)
        }
        "text" | "dynamicText" => {
            let default = if kind == "text" { "Text" } else { "{map} · {scale} · {date}" };
            let text: String = ps(p, "text").unwrap_or(default).chars().take(5000).collect();
            let fonts = rustgis_map::graphics::FONT_SIZE;
            let size = f(p, "size").unwrap_or(12.0).clamp(*fonts.start(), *fonts.end());
            let (italic, align, letter_spacing) = text_style(p, false, TextAlign::Left, 0.0)?;
            let (font, font_name) = text_font(p, (FontFamily::Sans, None))?;
            let k = ElementKind::Text {
                text,
                size,
                color: color(p, "color", Rgba::rgb(0x1F, 0x29, 0x37))?,
                bold: b(p, "bold").unwrap_or(false),
                italic,
                align,
                letter_spacing,
                font,
                font_name,
            };
            (if kind == "text" { "Text" } else { "Dynamic Text" }, place(p, l, (15.0, 12.0, 120.0, (size * 0.5).max(8.0)))?, k)
        }
        "picture" => {
            ("Picture", place(p, l, (pw - 60.0, ph - 50.0, 45.0, 35.0))?, ElementKind::Picture { image: image.unwrap_or_default() })
        }
        "table" => {
            let frame = frame_of(l, p)?;
            let layer = frame_layer_name(s, l, frame, p)?;
            let rows = p.get("rows").and_then(Value::as_u64).unwrap_or(12).clamp(1, 500) as usize;
            ("Table Frame", place(p, l, (15.0, ph * 0.6, pw * 0.5, ph * 0.35))?, ElementKind::Table { frame, layer, rows })
        }
        "chart" => {
            let frame = frame_of(l, p)?;
            let layer = frame_layer_name(s, l, frame, p)?;
            let (_, m) = chart_field_layer(s, &layer).ok_or("no such layer")?;
            let field = match ps(p, "field") {
                Some(fl) => m
                    .dataset
                    .field_index(fl)
                    .and_then(|i| m.dataset.fields.get(i))
                    .map(|f| f.name.clone())
                    .ok_or_else(|| format!("no field '{fl}'"))?,
                None => {
                    m.dataset.fields.iter().find(|f| f.ty.is_numeric()).map(|f| f.name.clone()).ok_or("the layer has no numeric field")?
                }
            };
            let label = ps(p, "label").and_then(|n| m.dataset.field_index(n)).and_then(|i| m.dataset.fields.get(i)).map(|f| f.name.clone());
            ("Chart Frame", place(p, l, (pw * 0.55, ph * 0.6, pw * 0.4, ph * 0.35))?, ElementKind::Chart { frame, layer, field, label })
        }
        "rectangle" => {
            let k = ElementKind::Rectangle {
                fill: color(p, "fill", Rgba::new(0, 0, 0, 0))?,
                outline: color(p, "outline", color(p, "color", Rgba::rgb(0x22, 0x22, 0x22))?)?,
                width: shape_width(p, 1.0)?.clamp(0.0, 50.0),
                radius: corner_radius(p, 0.0)?,
            };
            // Without a box it frames the page, as a neatline does; with any part of one it's a shape.
            if ["x", "y", "w", "h"].iter().any(|k| p.get(k).is_some_and(|v| !v.is_null())) {
                ("Rectangle", place(p, l, (pw * 0.4, ph * 0.4, pw * 0.2, ph * 0.2))?, k)
            } else {
                ("Neatline", place(p, l, (5.0, 5.0, pw - 10.0, ph - 10.0))?, k)
            }
        }
        "ellipse" | "circle" => {
            let k = ElementKind::Ellipse {
                fill: color(p, "fill", Rgba::new(0, 0, 0, 0))?,
                outline: color(p, "outline", color(p, "color", Rgba::rgb(0x22, 0x22, 0x22))?)?,
                width: shape_width(p, 1.0)?.clamp(0.0, 50.0),
            };
            // A circle is an ellipse in a square box.
            let d = (pw.min(ph) * 0.25).max(5.0);
            let (w, h) = if kind == "circle" { (d, d) } else { (d * 1.5, d) };
            let name = if kind == "circle" { "Circle" } else { "Ellipse" };
            (name, place(p, l, ((pw - w) / 2.0, (ph - h) / 2.0, w, h))?, k)
        }
        "line" | "arrow" => {
            let arrow = kind == "arrow";
            let head = line_head(p, if arrow { LineHead::End } else { LineHead::None })?;
            let color = color(p, "color", Rgba::rgb(0x22, 0x22, 0x22))?;
            let width = shape_width(p, if arrow { 1.5 } else { 1.0 })?.clamp(0.1, 50.0);
            let len = (pw.min(ph) * 0.4).max(10.0);
            let name = if head == LineHead::None { "Line" } else { "Arrow" };
            // At an angle, centred on the page unless x or y moves it; else the box is the
            // direction's, with room either side of a level or upright line to pick it up by.
            let (rect, dir, head) = match given_num(p, "angle", "a number of degrees")? {
                Some(angle) => {
                    // The angle sets the direction, so a "dir" as well would be overruled.
                    if p.get("dir").is_some_and(|v| !v.is_null()) {
                        return Err("give a line a dir or an angle, not both".into());
                    }
                    let length = given_num(p, "length", "a number of mm")?.unwrap_or(len);
                    if length <= 0.0 {
                        return Err("length is more than 0 mm".into());
                    }
                    // The box is exact for the angle and length: place() would resize it.
                    let ((x, y, w, h), dir, head) = line_at_angle((pw / 2.0, ph / 2.0), length, angle, head);
                    let (x, y) = (f(p, "x").unwrap_or(x), f(p, "y").unwrap_or(y));
                    if x.abs() > 5000.0 || y.abs() > 5000.0 {
                        return Err("element positions are up to 5000 mm".into());
                    }
                    ((x, y, w, h), dir, head)
                }
                None => {
                    let dir = line_dir(p, LineDir::Horizontal)?;
                    let (w, h) = match dir {
                        LineDir::Horizontal => (len, 6.0),
                        LineDir::Vertical => (6.0, len),
                        LineDir::Falling | LineDir::Rising => (len, len * 0.6),
                    };
                    (place(p, l, ((pw - w) / 2.0, (ph - h) / 2.0, w, h))?, dir, head)
                }
            };
            (name, rect, ElementKind::Line { color, width, dir, head })
        }
        "extentIndicator" => {
            let frame = frame_of(l, p)?;
            let of =
                p.get("of").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()).ok_or("of (the frame to outline) is required")?;
            if of == frame || !matches!(l.element(of).map(|e| &e.kind), Some(ElementKind::MapFrame { .. })) {
                return Err("of must be another map frame".into());
            }
            unlocked(l, frame)?;
            let (color, width) = indicator_style(p, l.element(frame).map(|e| &e.kind))?;
            let l = active(s, p)?;
            if let Some(ElementKind::MapFrame { extent_of, extent_color, extent_width, .. }) = l.element_mut(frame).map(|e| &mut e.kind) {
                *extent_of = Some(of);
                *extent_color = color;
                *extent_width = width;
            }
            return Ok(json!({"element": frame}));
        }
        "grid" => {
            let frame = frame_of(l, p)?;
            unlocked(l, frame)?;
            // Options not given keep the frame's grid's, or the defaults for a new grid.
            let base = match l.element(frame).map(|e| &e.kind) {
                Some(ElementKind::MapFrame { grid: Some(g), .. }) => g.clone(),
                _ => Grid::default(),
            };
            let new = if b(p, "off").unwrap_or(false) { None } else { Some(grid_params(p, base)?) };
            let l = active(s, p)?;
            if let Some(ElementKind::MapFrame { grid, .. }) = l.element_mut(frame).map(|e| &mut e.kind) {
                *grid = new;
            }
            if !b(p, "off").unwrap_or(false)
                && let Some(text) = ps(p, "crs")
            {
                super::say_crs_note(s, text);
            }
            return Ok(json!({"element": frame}));
        }
        o => return Err(format!("unknown element kind '{o}'")),
    };
    let warning = match &k {
        ElementKind::Text { font, font_name, bold, italic, .. } => {
            font_warning(*font, font_name.as_deref(), p.get("fallback").is_some_and(|v| !v.is_null()), (*bold, *italic))
        }
        _ => None,
    };
    let l = s.project.layouts.get_mut(i).ok_or("no layout")?;
    let id = l.add(name, rect, k);
    if p.get("columns").is_some_and(|v| !v.is_null()) {
        say_legend_fit(s, i, id);
    }
    let mut out = json!({"element": id});
    if let Some(w) = warning {
        out["warning"] = json!(w);
    }
    Ok(out)
}

/// The raster layer a colorbar on `frame` shows: `name` checked against the frame's map, or the
/// map's top visible raster (`None` when the map has none yet).
fn colorbar_layer(input: &impl super::InputSource, l: &Layout, frame: u32, name: Option<&str>) -> Result<Option<String>, String> {
    let context = input.input_context();
    let s = &context;
    let map = match l.element(frame).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { map, .. }) => map.clone(),
        _ => return Err("no such frame".into()),
    };
    let m = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(&map)).ok_or_else(|| format!("no map '{map}'"))?;
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => match rustgis_map::layout::colorbar_layer(m, Some(n)) {
            Some(layer) => Ok(Some(layer.name.clone())),
            None => Err(format!("no raster layer '{n}' in {map}")),
        },
        None => Ok(rustgis_map::layout::colorbar_layer(m, None).map(|l| l.name.clone())),
    }
}

fn orientation(p: &Value) -> Result<Option<Orientation>, String> {
    orientation_of(p, "orientation")
}

fn orientation_of(p: &Value, key: &str) -> Result<Option<Orientation>, String> {
    match ps(p, key).map(|o| o.trim().to_ascii_lowercase()) {
        None => Ok(None),
        Some(o) if o == "horizontal" => Ok(Some(Orientation::Horizontal)),
        Some(o) if o == "vertical" => Ok(Some(Orientation::Vertical)),
        Some(o) => Err(format!("unknown orientation '{o}' (horizontal or vertical)")),
    }
}

fn tick_side(p: &Value) -> Result<Option<TickSide>, String> {
    match ps(p, "tickSide") {
        None => Ok(None),
        Some(t) => TickSide::from_name(t).map(Some).ok_or_else(|| format!("unknown tickSide '{t}' (below, above, right or left)")),
    }
}

fn ticks(p: &Value) -> Option<usize> {
    f(p, "ticks").filter(|v| v.is_finite()).map(|v| v.round().clamp(2.0, MAX_COLORBAR_TICKS as f64) as usize)
}

/// An extent indicator's outline colour and width (points) from `color` and `width` in `p`, each
/// keeping the frame's (`kind`) when not given; one given but not a colour or a number is an
/// error, not left as it was.
fn indicator_style(p: &Value, kind: Option<&ElementKind>) -> Result<(Rgba, f64), String> {
    let (c, w) = match kind {
        Some(ElementKind::MapFrame { extent_color, extent_width, .. }) => (*extent_color, *extent_width),
        _ => (rustgis_map::layout::INDICATOR_COLOR, rustgis_map::layout::INDICATOR_WIDTH),
    };
    let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
    let width = match (given("width"), f(p, "width")) {
        (_, Some(v)) => v.clamp(rustgis_map::layout::MIN_INDICATOR_WIDTH, rustgis_map::layout::MAX_INDICATOR_WIDTH),
        (true, None) => return Err("width must be a number of points".into()),
        (false, None) => w,
    };
    if given("color") && ps(p, "color").is_none() {
        return Err("color must be #RRGGBB or #RRGGBBAA".into());
    }
    Ok((color(p, "color", c)?, width))
}

/// A grid with the options in `p` over `base`'s; a bad value is an error, leaving nothing changed.
fn grid_params(p: &Value, base: Grid) -> Result<Grid, String> {
    let mut g = base;
    // A graticule or a measured grid; a change of type without a step picks one, as degrees and
    // metres don't carry over.
    if let Some(v) = ps(p, "type") {
        let kind = GridKind::from_name(v).ok_or_else(|| format!("unknown grid type '{v}' (graticule or measured)"))?;
        if kind != g.kind {
            g.kind = kind;
            g.step = 0.0;
        }
    }
    // A measured grid's coordinate system, projected ("" or null: the map's, else the UTM zone).
    match p.get("crs") {
        None => {}
        Some(Value::Null) => g.crs = None,
        Some(Value::String(c)) if c.trim().is_empty() => g.crs = None,
        Some(Value::String(c)) if c.trim().chars().count() > rustgis_map::layout::MAX_GRID_CRS => {
            return Err(format!("crs: at most {} characters", rustgis_map::layout::MAX_GRID_CRS));
        }
        Some(Value::String(c)) => {
            let crs = rustgis_data::raster::Crs::parse(c.trim()).map_err(|e| format!("crs: {e}"))?;
            if crs.is_geographic() {
                return Err(format!(
                    "crs: {} is geographic; a measured grid needs a projected system (a graticule draws degrees)",
                    c.trim()
                ));
            }
            g.crs = Some(c.trim().to_string());
        }
        Some(_) => return Err("crs must be a coordinate system (EPSG:25832, WKT or a PROJ string), or \"\"".into()),
    }
    if let Some(v) = b(p, "short") {
        g.short = v;
    }
    if let Some(v) = f(p, "step") {
        g.step = match g.kind {
            GridKind::Graticule => v.clamp(0.0, rustgis_map::layout::MAX_GRATICULE_STEP),
            // (0 picks one; a step below a metre is a metre.)
            GridKind::Measured if v <= 0.0 => 0.0,
            GridKind::Measured => v.clamp(rustgis_map::layout::MIN_MEASURED_STEP, rustgis_map::layout::MAX_MEASURED_STEP),
        };
    }
    g.color = color(p, "color", g.color)?;
    if let Some(v) = b(p, "labels") {
        g.labels = v;
    }
    if let Some(v) = ps(p, "style") {
        g.style = GridStyle::from_name(v).ok_or_else(|| format!("unknown grid style '{v}' (lines, ticks or linesAndTicks)"))?;
    }
    if let Some(v) = f(p, "tickLength") {
        g.tick_length = v.clamp(0.0, MAX_TICK_MM);
    }
    if let Some(v) = f(p, "width") {
        g.width = v.clamp(0.0, MAX_GRID_WIDTH);
    }
    if let Some(v) = ps(p, "format") {
        g.format = CoordFormat::from_name(v).ok_or_else(|| format!("unknown format '{v}' (decimal or dms)"))?;
    }
    if let Some(v) = b(p, "hemisphere") {
        g.hemisphere = v;
    }
    if let Some(v) = p.get("edges") {
        g.edges = edges(v)?;
    }
    if let Some(o) = orientation_of(p, "labelOrientation")? {
        g.label_orientation = o;
    }
    Ok(g)
}

/// Labelled edges from `["top", "bottom", "left", "right"]` (any of them) or `"bottom,left"`.
fn edges(v: &Value) -> Result<GridEdges, String> {
    let names: Vec<String> = match v {
        Value::Array(a) => a.iter().map(|n| n.as_str().map(str::to_string).ok_or("edges are names")).collect::<Result<_, _>>()?,
        Value::String(t) => t.split(',').map(str::to_string).collect(),
        _ => return Err("edges is a list of top, bottom, left and right".into()),
    };
    let mut e = GridEdges { top: false, bottom: false, left: false, right: false };
    for n in names.iter().map(|n| n.trim().to_ascii_lowercase()).filter(|n| !n.is_empty()) {
        match n.as_str() {
            "top" => e.top = true,
            "bottom" => e.bottom = true,
            "left" => e.left = true,
            "right" => e.right = true,
            o => return Err(format!("unknown edge '{o}' (top, bottom, left or right)")),
        }
    }
    Ok(e)
}

/// A text element's italic, alignment and letter spacing (points) from `italic`, `align` and
/// `letterSpacing` in `p`, each keeping the current one when not given; one given but not of its
/// kind is an error.
pub(crate) fn text_style(p: &Value, italic: bool, align: TextAlign, spacing: f64) -> Result<(bool, TextAlign, f64), String> {
    let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
    let italic = match p.get("italic") {
        None | Some(Value::Null) => italic,
        Some(Value::Bool(v)) => *v,
        Some(Value::String(t)) if ["true", "yes", "1", "on"].contains(&t.trim()) => true,
        Some(Value::String(t)) if ["false", "no", "0", "off"].contains(&t.trim()) => false,
        Some(_) => return Err("italic must be true or false".into()),
    };
    let align = match ps(p, "align") {
        Some(a) => TextAlign::from_name(a).ok_or_else(|| format!("unknown alignment '{a}' (left, center or right)"))?,
        None if given("align") => return Err("align must be left, center or right".into()),
        None => align,
    };
    let spacing = match (given("letterSpacing"), f(p, "letterSpacing")) {
        (_, Some(v)) => v.clamp(rustgis_map::layout::MIN_LETTER_SPACING, rustgis_map::layout::MAX_LETTER_SPACING),
        (true, None) => return Err("letterSpacing must be a number of points".into()),
        (false, None) => spacing,
    };
    Ok((italic, align, spacing))
}

/// What a `font` given for an element that isn't a text is told.
const ONLY_TEXT_FONT: &str = "only text elements take a font or a fallback";

/// Rounded corners are a rectangle's: refused elsewhere, as a family is, so a script doesn't take
/// them as applied.
const ONLY_RECTANGLE_RADIUS: &str = "only a rectangle rounds its corners";

/// Colours are refused for the elements that don't draw them, as a family is, so a script doesn't
/// take them as applied: "color" is a text's, a line's, an arrow's, a scale bar's or a shape's
/// outline (a rectangle's or an ellipse's), and, when adding, also an extent indicator's or a grid's.
const ONLY_COLOR_ADDED: &str =
    "only text, lines, arrows, rectangles, ellipses, circles, extent indicators, grids and scale bars take a color";
const ONLY_COLOR_UPDATED: &str = "only text, lines, arrows, rectangles, ellipses, circles and scale bars take a color";
const ONLY_FILL: &str = "only north arrows, rectangles, ellipses, circles and scale bars take a fill";
const ONLY_OUTLINE: &str = "only north arrows, rectangles, ellipses and circles take an outline";

/// Whether `p` gives `k`: `null` is not given.
fn gives(p: &Value, k: &str) -> bool {
    p.get(k).is_some_and(|v| !v.is_null())
}

/// Whether `p` gives a `font` or a `fallback`, which only text elements take.
fn gives_font(p: &Value) -> bool {
    ["font", "fallback"].iter().any(|k| p.get(*k).is_some_and(|v| !v.is_null()))
}

/// A text element's family from `font` and `fallback` in `p`, the current ones when not given:
/// `font` is "sans" or "serif" (#517), or an installed family's name (#762), kept as the font names
/// it where it is installed (its case), else as given. A named family is drawn in `fallback`
/// ("sans" or "serif") where it isn't installed: by default the one it has when it is named again,
/// else its own class (a serif's is the serif), else the sans.
pub(crate) fn text_font(p: &Value, current: (FontFamily, Option<String>)) -> Result<(FontFamily, Option<String>), String> {
    let fallback = match (ps(p, "fallback"), p.get("fallback").is_some_and(|v| !v.is_null())) {
        (Some(n), _) => Some(FontFamily::from_name(n).ok_or("fallback must be sans or serif")?),
        (None, true) => return Err("fallback must be sans or serif".into()),
        (None, false) => None,
    };
    let Some(font) = p.get("font").filter(|v| !v.is_null()) else {
        return match (fallback, current) {
            (Some(_), (_, None)) => Err("fallback goes with a font family's name".into()),
            (Some(f), (_, name)) => Ok((f, name)),
            (None, cur) => Ok(cur),
        };
    };
    let n = font.as_str().ok_or("font must be sans, serif or an installed font family's name")?;
    // Kept as a label's font is (trimmed, control characters out), then read as a generic name
    // or a family's.
    let name = rustgis_map::symbology::font_name(n).ok_or("font must be sans, serif or an installed font family's name")?;
    if let Some(generic) = FontFamily::from_name(&name) {
        return match fallback {
            Some(_) => Err("fallback goes with a font family's name, not sans or serif".into()),
            None => Ok((generic, None)),
        };
    }
    let installed = installed_family(&name);
    let kept = installed.map_or(name, |f| f.name.clone());
    let fallback = fallback.unwrap_or_else(|| default_fallback(installed, &kept, &current));
    Ok((fallback, Some(kept)))
}

/// The installed family named `name` (in any case, as the index compares names): none while the
/// app is still indexing them (a command runs on its interface thread, which mustn't wait).
fn installed_family(name: &str) -> Option<&'static rustgis_render::families::Family> {
    use rustgis_render::families;
    families::available()?.get(usize::from(families::find_ready(name)?))
}

/// Whether the face of `family` a text in `style` (bold, italic) draws in can be read (a file past
/// the size RustGIS reads, or a damaged one, can't): that face only, read once and kept, as
/// drawing it would.
fn readable(family: &rustgis_render::families::Family, (bold, italic): (bool, bool)) -> bool {
    use rustgis_render::families;
    families::pick(family, bold, italic).and_then(|(face, _)| families::bytes(face)).is_some()
}

/// A named family's fallback when none is given: the one it has when it is named again, else its
/// own class (a serif's is the serif), else, where the family isn't known (not installed, or
/// the fonts not indexed yet), a name that says "Serif" without "Sans" (Noto Serif), else the
/// sans.
fn default_fallback(
    installed: Option<&rustgis_render::families::Family>,
    name: &str,
    current: &(FontFamily, Option<String>),
) -> FontFamily {
    if current.1.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(name)) {
        return current.0;
    }
    let lower = name.to_lowercase();
    match installed {
        Some(f) if f.serif => FontFamily::Serif,
        None if lower.contains("serif") && !lower.contains("sans") => FontFamily::Serif,
        _ => FontFamily::Sans,
    }
}

/// What a text is told when it isn't drawn as asked: a named family this computer hasn't is drawn
/// in its fallback, and a serif asked for where none is installed (the web app, a bare Linux box)
/// in the sans, so an export never differs from what was asked unsaid. While the app is still
/// indexing the installed fonts (its first seconds), a family named with no `fallback` given is
/// told that its fallback came from its name, not the font.
fn font_warning(font: FontFamily, name: Option<&str>, fallback_given: bool, style: (bool, bool)) -> Option<String> {
    let serif = rustgis_render::fonts::serif_available();
    let drawn = if font == FontFamily::Serif && serif { "serif" } else { "sans-serif" };
    let indexed = rustgis_render::families::available().is_some();
    match name {
        Some(n) if !indexed && !fallback_given => Some(format!(
            "the installed fonts are still being read, so {n}'s fallback ({drawn}) comes from its name, not the font: give \"fallback\" to choose it"
        )),
        // Installed, but its face can't be loaded (a file too large or damaged, or past the font
        // bytes RustGIS keeps in a session): drawn in its fallback too.
        Some(n) if indexed && installed_family(n).is_some_and(|f| !readable(f, style)) => Some(format!(
            "{n} is installed but its font can't be loaded (a file too large or damaged, or past the fonts RustGIS keeps in memory), so the text is drawn in the {drawn}"
        )),
        Some(n) if indexed && rustgis_render::families::find_ready(n).is_none() => {
            Some(format!("{n} is not installed on this computer, so the text is drawn in the {drawn}"))
        }
        Some(_) => None,
        None => {
            (font == FontFamily::Serif && !serif).then(|| "no serif font is installed here, so the text is drawn in the sans-serif".into())
        }
    }
}

/// A legend's `exclude` from `p`: the names of layers in its frame's map, as the map spells them;
/// `None` when not given. Not a list of names, or a name the map hasn't, is an error.
fn legend_exclude(input: &impl super::InputSource, l: &Layout, frame: u32, p: &Value) -> Result<Option<Vec<String>>, String> {
    let context = input.input_context();
    let s = &context;
    let Some(v) = p.get("exclude").filter(|v| !v.is_null()) else { return Ok(None) };
    let names = v.as_array().ok_or("exclude is a list of layer names")?;
    if names.len() > rustgis_map::layout::MAX_LEGEND_EXCLUDED {
        return Err(format!("a legend leaves out at most {} layers", rustgis_map::layout::MAX_LEGEND_EXCLUDED));
    }
    let map_name = match l.element(frame).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { map, .. }) => map.clone(),
        _ => return Err("the legend's frame isn't a map frame".into()),
    };
    let map = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(&map_name)).ok_or_else(|| format!("no map '{map_name}'"))?;
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let n = n.as_str().ok_or("exclude is a list of layer names")?;
        // A layer of the map's, or a WMS or WMTS layer's (whose legend is its service's picture).
        let name = map
            .layer_by_name(n.trim())
            .map(|l| &l.name)
            .or_else(|| map.tiles.iter().find(|t| t.name.eq_ignore_ascii_case(n.trim())).map(|t| &t.name))
            .ok_or_else(|| format!("no layer '{n}' in map '{map_name}'"))?;
        if !out.iter().any(|o| o.eq_ignore_ascii_case(name)) {
            out.push(name.clone());
        }
    }
    Ok(Some(out))
}

/// A north arrow or scale bar with the options in `p` over `kind`'s; `None` for other elements
/// or when `p` has none. A bad value is an error, leaving nothing changed.
fn restyle(kind: &ElementKind, p: &Value) -> Result<Option<ElementKind>, String> {
    match kind {
        ElementKind::NorthArrow { frame, style, fill, outline } => {
            if ["style", "fill", "outline"].iter().all(|k| p.get(k).is_none()) {
                return Ok(None);
            }
            let style = match ps(p, "style") {
                Some(v) => ArrowStyle::from_name(v)
                    .ok_or_else(|| format!("unknown north arrow style '{v}' (classic, compass, triangle or star)"))?,
                None => *style,
            };
            Ok(Some(ElementKind::NorthArrow {
                frame: *frame,
                style,
                fill: color(p, "fill", *fill)?,
                outline: color(p, "outline", *outline)?,
            }))
        }
        ElementKind::ScaleBar { frame, units, divisions, style, color: c, fill } => {
            if ["style", "units", "divisions", "color", "fill"].iter().all(|k| p.get(k).is_none()) {
                return Ok(None);
            }
            let style = match ps(p, "style") {
                Some(v) => {
                    ScaleBarStyle::from_name(v).ok_or_else(|| format!("unknown scale bar style '{v}' (alternating, stepped or line)"))?
                }
                None => *style,
            };
            let units = match ps(p, "units") {
                Some(v) => {
                    ScaleUnits::from_name(v).ok_or_else(|| format!("unknown units '{v}' (auto, kilometers, meters, miles or feet)"))?
                }
                None => *units,
            };
            let divisions = match p.get("divisions") {
                Some(_) => f(p, "divisions").ok_or("divisions is a number")?.round().clamp(1.0, MAX_SCALE_DIVISIONS as f64) as usize,
                None => *divisions,
            };
            let (color, fill) = (color(p, "color", *c)?, color(p, "fill", *fill)?);
            Ok(Some(ElementKind::ScaleBar { frame: *frame, units, divisions, style, color, fill }))
        }
        ElementKind::Rectangle { fill, outline, width, .. } | ElementKind::Ellipse { fill, outline, width } => {
            if ["fill", "outline", "color", "width", "radius"].iter().all(|k| p.get(k).is_none()) {
                return Ok(None);
            }
            // The outline's colour is "outline", or "color" as when adding one.
            let (fill, outline) = (color(p, "fill", *fill)?, color(p, "outline", color(p, "color", *outline)?)?);
            let width = shape_width(p, *width)?.clamp(0.0, 50.0);
            Ok(Some(match kind {
                ElementKind::Rectangle { radius, .. } => {
                    ElementKind::Rectangle { fill, outline, width, radius: corner_radius(p, *radius)? }
                }
                _ => ElementKind::Ellipse { fill, outline, width },
            }))
        }
        ElementKind::Line { color: c, width, dir, head } => {
            if ["color", "width", "dir", "head"].iter().all(|k| p.get(k).is_none()) {
                return Ok(None);
            }
            let dir = line_dir(p, *dir)?;
            let head = line_head(p, *head)?;
            let width = shape_width(p, *width)?.clamp(0.1, 50.0);
            Ok(Some(ElementKind::Line { color: color(p, "color", *c)?, width, dir, head }))
        }
        _ => Ok(None),
    }
}

fn update(s: &mut Session, p: &Value) -> CmdResult {
    let id = element_id(p)?;
    // Checked before anything changes, so a bad value leaves the element as it was.
    let new_center = match p.get("center") {
        Some(c) => {
            let a = c.as_array().ok_or("center is [lon, lat]")?;
            match (a.first().and_then(Value::as_f64), a.get(1).and_then(Value::as_f64)) {
                (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Some(Coord::new(x.clamp(-540.0, 540.0), y.clamp(-85.0, 85.0))),
                _ => return Err("center is [lon, lat]".into()),
            }
        }
        None => None,
    };
    for k in ["bearing", "pitch"] {
        if p.get(k).is_some_and(|v| !v.is_null()) && f(p, k).is_none() {
            return Err(format!("{k} must be a number of degrees"));
        }
    }
    let li = index(s, p)?;
    // Locked, it takes no change but being unlocked, which may come with other changes.
    let changes =
        p.as_object().is_some_and(|o| o.iter().any(|(k, v)| !v.is_null() && !matches!(k.as_str(), "layout" | "element" | "locked")));
    if changes && b(p, "locked") != Some(false) {
        unlocked(s.project.layouts.get(li).ok_or("no layout")?, id)?;
    }
    // A new layer (tables and charts) or field (charts) must exist in the frame's map.
    let checked = checked_update(s, p, li, id)?;
    // A colorbar's new layer must be a raster in its frame's map; empty follows the top raster.
    let colorbar = {
        let l = s.project.layouts.get(li).ok_or("no layout")?;
        match l.element(id).map(|e| &e.kind) {
            Some(ElementKind::Colorbar { frame, .. }) => {
                let layer = match ps(p, "layer") {
                    Some(n) if n.trim().is_empty() => Some(None),
                    Some(n) => Some(colorbar_layer(s, l, *frame, Some(n))?),
                    None => None,
                };
                Some((layer, orientation(p)?, tick_side(p)?))
            }
            _ => None,
        }
    };
    // A north arrow's or scale bar's new look, and a map frame's grid: `"grid": false` removes it,
    // `true` adds the default one, an object changes the options it names, and null (not set)
    // leaves it as it is.
    let (restyled, new_grid, new_indicator) = {
        let l = s.project.layouts.get(li).ok_or("no layout")?;
        let kind = &l.element(id).ok_or_else(|| format!("no element {id}"))?.kind;
        // `"extentIndicator": false` takes the frame's indicator away; an object restyles it.
        let indicator = match (kind, p.get("extentIndicator").filter(|v| !v.is_null())) {
            (ElementKind::MapFrame { .. }, Some(Value::Bool(false))) => Some(None),
            (ElementKind::MapFrame { extent_of: Some(_), .. }, Some(v @ Value::Object(_))) => Some(Some(indicator_style(v, Some(kind))?)),
            (ElementKind::MapFrame { .. }, Some(Value::Object(_))) => {
                return Err("this frame has no extent indicator: add one with layout.addElement (kind extentIndicator)".into());
            }
            (ElementKind::MapFrame { .. }, Some(_)) => return Err("extentIndicator is an object of color and width, or false".into()),
            (_, Some(_)) => return Err("only map frames have an extent indicator".into()),
            _ => None,
        };
        let grid = match (kind, p.get("grid").filter(|v| !v.is_null())) {
            (ElementKind::MapFrame { grid, .. }, Some(v)) => Some(match v {
                Value::Bool(false) => None,
                Value::Bool(true) => Some(grid.clone().unwrap_or_default()),
                Value::Object(_) => Some(grid_params(v, grid.clone().unwrap_or_default())?),
                _ => return Err("grid is an object of grid options, or false to remove it".into()),
            }),
            (_, Some(_)) => return Err("only map frames have a grid".into()),
            _ => None,
        };
        (restyle(kind, p)?, grid, indicator)
    };
    // A line turned to an "angle" or made a "length" long, about its centre: its new box,
    // direction and heads.
    let (angle, length) = (given_num(p, "angle", "a number of degrees")?, given_num(p, "length", "a number of mm")?);
    let new_line = match (angle, length) {
        (None, None) => None,
        _ => {
            let e = s.project.layouts.get(li).and_then(|l| l.element(id)).ok_or_else(|| format!("no element {id}"))?;
            let ElementKind::Line { dir, head, .. } = &e.kind else {
                return Err("only lines and arrows take an angle or a length".into());
            };
            // The angle and length set the direction, so a "dir" as well would be overruled.
            if p.get("dir").is_some_and(|v| !v.is_null()) {
                return Err("give a line a dir or an angle and length, not both".into());
            }
            let r = (e.x, e.y, e.w, e.h);
            let length = length.unwrap_or_else(|| line_length(r, *dir));
            if length <= 0.0 {
                return Err("length is more than 0 mm".into());
            }
            // The angle the line points now, read with its current heads; it turns with the new.
            let angle = angle.unwrap_or_else(|| line_angle(r, *dir, *head));
            let head = match &restyled {
                Some(ElementKind::Line { head, .. }) => *head,
                _ => *head,
            };
            Some(line_at_angle((e.x + e.w / 2.0, e.y + e.h / 2.0), length, angle, head))
        }
    };
    let map_name = match ps(p, "map") {
        Some(n) => Some(
            s.project
                .maps
                .iter()
                .find(|m| m.name.eq_ignore_ascii_case(n))
                .map(|m| m.name.clone())
                .ok_or_else(|| format!("no map '{n}'"))?,
        ),
        None => None,
    };
    let kind_now = s.project.layouts.get(li).and_then(|l| l.element(id)).map(|e| &e.kind);
    if gives(p, "color")
        && !matches!(
            kind_now,
            Some(
                ElementKind::Text { .. }
                    | ElementKind::Line { .. }
                    | ElementKind::ScaleBar { .. }
                    | ElementKind::Rectangle { .. }
                    | ElementKind::Ellipse { .. }
            )
        )
    {
        return Err(ONLY_COLOR_UPDATED.into());
    }
    let shape = matches!(kind_now, Some(ElementKind::NorthArrow { .. } | ElementKind::Rectangle { .. } | ElementKind::Ellipse { .. }));
    if gives(p, "fill") && !shape && !matches!(kind_now, Some(ElementKind::ScaleBar { .. })) {
        return Err(ONLY_FILL.into());
    }
    if gives(p, "outline") && !shape {
        return Err(ONLY_OUTLINE.into());
    }
    // A colour that isn't text is refused, as `color` does when adding (the default is unused).
    let col = match p.get("color").filter(|v| !v.is_null()) {
        Some(_) => Some(color(p, "color", Rgba::rgb(0, 0, 0))?),
        None => None,
    };
    // A text element's italic, alignment and spacing, checked before anything changes.
    let new_text_style = match s.project.layouts.get(li).and_then(|l| l.element(id)).map(|e| &e.kind) {
        Some(ElementKind::Text { italic, align, letter_spacing, font, font_name, .. }) => {
            Some((text_style(p, *italic, *align, *letter_spacing)?, text_font(p, (*font, font_name.clone()))?))
        }
        _ if gives_font(p) => return Err(ONLY_TEXT_FONT.into()),
        _ => None,
    };
    // A family asked for that isn't drawn as asked: said whenever the call asks for one (again
    // too, as `layout.addElement` says it), so a script is told what it gets; a call that changes
    // something else on such a text isn't told again.
    let bold_now = match s.project.layouts.get(li).and_then(|l| l.element(id)).map(|e| &e.kind) {
        Some(ElementKind::Text { bold, .. }) => b(p, "bold").unwrap_or(*bold),
        _ => false,
    };
    let font_told = match &new_text_style {
        Some(((italic, _, _), (f, n))) if gives_font(p) => {
            font_warning(*f, n.as_deref(), p.get("fallback").is_some_and(|v| !v.is_null()), (bold_now, *italic))
        }
        _ => None,
    };
    if p.get("radius").is_some_and(|v| !v.is_null())
        && !matches!(s.project.layouts.get(li).and_then(|l| l.element(id)).map(|e| &e.kind), Some(ElementKind::Rectangle { .. }))
    {
        return Err(ONLY_RECTANGLE_RADIUS.into());
    }
    // A legend's left-out layers, checked against its frame's map before anything changes.
    let new_exclude = {
        let l = s.project.layouts.get(li).ok_or("no layout")?;
        match l.element(id).map(|e| &e.kind) {
            Some(ElementKind::Legend { frame, .. }) => legend_exclude(s, l, *frame, p)?,
            _ if p.get("exclude").is_some_and(|v| !v.is_null()) => return Err("only legends leave layers out".into()),
            _ => None,
        }
    };
    let new_columns = legend_columns_param(p)?;
    if new_columns.is_some()
        && !matches!(s.project.layouts.get(li).and_then(|l| l.element(id)).map(|e| &e.kind), Some(ElementKind::Legend { .. }))
    {
        return Err(ONLY_LEGEND_COLUMNS.into());
    }
    let l = active(s, p)?;
    let e = l.element_mut(id).ok_or_else(|| format!("no element {id}"))?;
    let num = |k: &str, lo: f64, hi: f64| f(p, k).map(|v| v.clamp(lo, hi));
    if let Some(v) = num("x", -5000.0, 5000.0) {
        e.x = v;
    }
    if let Some(v) = num("y", -5000.0, 5000.0) {
        e.y = v;
    }
    if let Some(v) = num("w", MIN_ELEMENT_MM, 5000.0) {
        e.w = v;
    }
    if let Some(v) = num("h", MIN_ELEMENT_MM, 5000.0) {
        e.h = v;
    }
    if let Some(n) = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()) {
        e.name = n.chars().take(200).collect();
    }
    if let Some(v) = b(p, "visible") {
        e.visible = v;
    }
    if let Some(v) = b(p, "locked") {
        e.locked = v;
    }
    // A colorbar turned keeps its size: its length and thickness swap, unless given.
    if let (ElementKind::Colorbar { orientation, .. }, Some((_, Some(o), _))) = (&e.kind, &colorbar)
        && o != orientation
        && f(p, "w").is_none()
        && f(p, "h").is_none()
    {
        std::mem::swap(&mut e.w, &mut e.h);
    }
    if let Some(k) = restyled {
        e.kind = k;
    }
    // The turned line's box; an "x" or "y" in the same call still moves it.
    if let Some(((x, y, w, h), d, hd)) = new_line {
        (e.x, e.y, e.w, e.h) = (num("x", -5000.0, 5000.0).unwrap_or(x), num("y", -5000.0, 5000.0).unwrap_or(y), w, h);
        if let ElementKind::Line { dir, head, .. } = &mut e.kind {
            (*dir, *head) = (d, hd);
        }
    }
    match &mut e.kind {
        ElementKind::Text { text, size, color, bold, italic, align, letter_spacing, font, font_name } => {
            if let Some(((i, a, l), (f, n))) = new_text_style {
                (*italic, *align, *letter_spacing, *font, *font_name) = (i, a, l, f, n);
            }
            if let Some(t) = ps(p, "text") {
                *text = t.chars().take(5000).collect();
            }
            if let Some(v) = num("size", *rustgis_map::graphics::FONT_SIZE.start(), *rustgis_map::graphics::FONT_SIZE.end()) {
                *size = v;
            }
            if let Some(c) = col {
                *color = c;
            }
            if let Some(v) = b(p, "bold") {
                *bold = v;
            }
        }
        ElementKind::Legend { title, all_classes, hide_other, exclude, columns, .. } => {
            if let Some(names) = new_exclude {
                *exclude = names;
            }
            if let Some(n) = new_columns {
                *columns = n;
            }
            if let Some(t) = ps(p, "title") {
                *title = t.chars().take(200).collect();
            }
            if let Some(v) = b(p, "allClasses") {
                *all_classes = v;
            }
            if let Some(v) = b(p, "hideOther") {
                *hide_other = v;
            }
        }
        ElementKind::Colorbar { layer, title, orientation, ticks: n, tick_side, reverse, .. } => {
            if let Some(t) = ps(p, "title") {
                *title = t.chars().take(200).collect();
            }
            if let Some((new_layer, new_orientation, new_side)) = colorbar {
                if let Some(nl) = new_layer {
                    *layer = nl;
                }
                if let Some(o) = new_orientation {
                    *orientation = o;
                }
                if let Some(t) = new_side {
                    *tick_side = t;
                }
            }
            if let Some(v) = ticks(p) {
                *n = v;
            }
            if let Some(v) = b(p, "reverse") {
                *reverse = v;
            }
        }
        ElementKind::MapFrame { map, center, scale, grid, bearing, pitch, extent_of, extent_color, extent_width, .. } => {
            match new_indicator {
                Some(None) => *extent_of = None,
                Some(Some((c, w))) => (*extent_color, *extent_width) = (c, w),
                None => {}
            }
            if let Some(m) = map_name {
                *map = m;
            }
            if let Some(v) = f(p, "bearing") {
                *bearing = rustgis_map::camera::normalize_bearing(v);
            }
            if let Some(v) = f(p, "pitch") {
                *pitch = rustgis_map::camera::normalize_pitch(v);
            }
            if let Some(g) = new_grid {
                *grid = g;
            }
            if let Some(v) = num("scale", rustgis_map::layout::MIN_FRAME_SCALE, rustgis_map::layout::MAX_FRAME_SCALE) {
                *scale = v;
            }
            if let Some(c) = new_center {
                *center = c;
            }
        }
        ElementKind::Table { rows, layer, .. } => {
            if let Some(v) = p.get("rows").and_then(Value::as_u64) {
                *rows = v.clamp(1, 500) as usize;
            }
            if let Some((l, _)) = &checked {
                *layer = l.clone();
            }
        }
        ElementKind::Chart { field, layer, .. } => {
            if let Some((l, f)) = &checked {
                *layer = l.clone();
                if let Some(f) = f {
                    *field = f.clone();
                }
            }
        }
        _ => {}
    }
    // A legend in columns is told again when its box or what it lists changes; a move changes neither.
    let in_columns = matches!(&e.kind, ElementKind::Legend { columns, .. } if *columns > 1);
    let refitted = f(p, "w").is_some()
        || f(p, "h").is_some()
        || ps(p, "title").is_some()
        || b(p, "allClasses").is_some()
        || b(p, "hideOther").is_some()
        || p.get("exclude").is_some_and(|v| !v.is_null());
    if new_columns.is_some() || (in_columns && refitted) {
        say_legend_fit(s, li, id);
    }
    if let Some(text) = p.get("grid").and_then(|grid| grid.get("crs")).and_then(Value::as_str) {
        super::say_crs_note(s, text);
    }
    if let Some(w) = font_told {
        return Ok(json!({"warning": w}));
    }
    Ok(Value::Null)
}

/// Points a frame at an extent.
fn set_frame(s: &mut Session, li: usize, frame: u32, e: &Extent) -> Result<(), String> {
    let l = s.project.layouts.get(li).ok_or("no layout")?;
    let el = l.element(frame).ok_or("no such frame")?;
    let (w, h) = (el.w, el.h);
    let ElementKind::MapFrame { map, .. } = &el.kind else { return Err("that isn't a map frame".into()) };
    let m = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)).ok_or_else(|| format!("no map '{map}'"))?;
    let (c, sc) = fit_scale(m, e, w, h).ok_or("nothing to zoom to")?;
    if let Some(ElementKind::MapFrame { center, scale, .. }) =
        s.project.layouts.get_mut(li).and_then(|l| l.element_mut(frame)).map(|e| &mut e.kind)
    {
        *center = c;
        *scale = sc;
    }
    Ok(())
}

fn frame_zoom(s: &mut Session, p: &Value) -> CmdResult {
    let li = index(s, p)?;
    let l = s.project.layouts.get(li).ok_or("no layout")?;
    let frame = zoom_frame(l, p)?;
    unlocked(l, frame)?;
    let m = frame_map(s, l, frame)?;
    match ps(p, "to").unwrap_or("view") {
        "view" => {
            let w = l.element(frame).map(|e| e.w).unwrap_or(100.0);
            let ground_w = s.view_size.0 * m.ground_res();
            let (c, sc) = (
                m.center_lonlat(),
                (ground_w / (w / 1000.0)).clamp(rustgis_map::layout::MIN_FRAME_SCALE, rustgis_map::layout::MAX_FRAME_SCALE),
            );
            let (b, t) = view_turn(s);
            if let Some(ElementKind::MapFrame { center, scale, bearing, pitch, .. }) =
                s.project.layouts.get_mut(li).and_then(|l| l.element_mut(frame)).map(|e| &mut e.kind)
            {
                *center = c;
                *scale = sc;
                (*bearing, *pitch) = (b, t);
            }
        }
        "full" => {
            let e = m.shown_extent();
            set_frame(s, li, frame, &e)?;
        }
        "selection" => {
            let e = m.selection_extent();
            set_frame(s, li, frame, &e)?;
        }
        "layer" => {
            let n = ps(p, "layer").ok_or("layer is required")?;
            let e = m.layer_by_name(n).ok_or_else(|| format!("no layer '{n}'"))?.shown_extent();
            set_frame(s, li, frame, &e)?;
        }
        o => return Err(format!("unknown zoom '{o}'")),
    }
    Ok(json!({"element": frame}))
}

fn series(s: &mut Session, p: &Value) -> CmdResult {
    let li = index(s, p)?;
    if b(p, "off").unwrap_or(false) {
        if let Some(l) = s.project.layouts.get_mut(li) {
            l.series = None;
        }
        return Ok(Value::Null);
    }
    let l = s.project.layouts.get(li).ok_or("no layout")?;
    let frame = frame_of(l, p)?;
    let map = match l.element(frame).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { map, .. }) => map.clone(),
        _ => return Err("no such frame".into()),
    };
    let n = ps(p, "layer").ok_or("layer is required")?;
    let m = frame_map(s, l, frame)?;
    let layer = m.layer_by_name(n).filter(|l| !l.is_raster()).ok_or_else(|| format!("no feature layer '{n}' in {map}"))?;
    if layer.dataset.features.is_empty() {
        return Err("the index layer has no features".into());
    }
    let name_field = match ps(p, "nameField") {
        Some(fl) => layer
            .dataset
            .field_index(fl)
            .and_then(|i| layer.dataset.fields.get(i))
            .map(|f| f.name.clone())
            .ok_or_else(|| format!("no field '{fl}'"))?,
        None => layer.labels.default.field.clone(),
    };
    let layer = layer.name.clone();
    let margin = f(p, "margin").unwrap_or(0.1).clamp(0.0, 5.0);
    if let Some(l) = s.project.layouts.get_mut(li) {
        l.series = Some(MapSeries { frame, layer, name_field, margin, page: 0 });
    }
    series_page(s, &json!({"layout": li, "page": 0}))
}

/// The pages of a layout's map series: (count, page name of each row).
pub fn series_pages(s: &Session, li: usize) -> Option<Vec<String>> {
    let (layer, fi) = series_layer(s, li)?;
    Some((0..layer.dataset.features.len()).map(|r| page_name(layer, fi, r)).collect())
}

/// A map series' page count and the name of `page`, without naming every page.
pub fn series_info(s: &Session, li: usize, page: usize) -> Option<(usize, String)> {
    let (layer, fi) = series_layer(s, li)?;
    let n = layer.dataset.features.len();
    Some((n, if page < n { page_name(layer, fi, page) } else { String::new() }))
}

/// A map series' index layer and its name field.
fn series_layer(input: &impl super::InputSource, li: usize) -> Option<(&rustgis_map::Layer, Option<usize>)> {
    let context = input.input_context();
    let s = &context;
    let l = s.project.layouts.get(li)?;
    let sr = l.series.as_ref()?;
    let map = match l.element(sr.frame).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { map, .. }) => map,
        _ => return None,
    };
    let layer = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map))?.layer_by_name(&sr.layer)?;
    Some((layer, layer.dataset.field_index(&sr.name_field)))
}

fn page_name(layer: &rustgis_map::Layer, fi: Option<usize>, row: usize) -> String {
    fi.map(|i| layer.dataset.value(row, i).to_string()).unwrap_or_else(|| (row + 1).to_string())
}

fn series_page(s: &mut Session, p: &Value) -> CmdResult {
    let li = index(s, p)?;
    let (n, _) = series_info(s, li, 0).ok_or("the layout has no map series")?;
    let l = s.project.layouts.get(li).ok_or("no layout")?;
    let sr = l.series.clone().ok_or("the layout has no map series")?;
    let page = match p.get("page") {
        Some(Value::String(t)) if t == "next" => (sr.page + 1).min(n.saturating_sub(1)),
        Some(Value::String(t)) if t == "previous" => sr.page.saturating_sub(1),
        Some(Value::String(t)) if t == "current" => sr.page.min(n.saturating_sub(1)),
        Some(v) => v
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .filter(|v| *v < n)
            .ok_or_else(|| format!("page must be 0 to {}", n.saturating_sub(1)))?,
        None => return Err("page is required".into()),
    };
    // The page's feature extent, with the series margin.
    let map = match l.element(sr.frame).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { map, .. }) => map.clone(),
        _ => return Err("the map series frame is gone".into()),
    };
    let layer = s
        .project
        .maps
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(&map))
        .and_then(|m| m.layer_by_name(&sr.layer))
        .ok_or("the index layer is gone")?;
    // A feature without geometry (common in CSV and GeoJSON) leaves the frame where it was.
    let e = layer.dataset.features.get(page).and_then(|f| f.geometry.as_ref()).map(|g| g.extent()).filter(|e| !e.is_empty());
    if let Some(e) = e {
        set_frame(s, li, sr.frame, &e.padded(sr.margin, 0.01))?;
    }
    if let Some(sr) = s.project.layouts.get_mut(li).and_then(|l| l.series.as_mut()) {
        sr.page = page;
    }
    let name = series_info(s, li, page).map(|x| x.1).unwrap_or_default();
    Ok(json!({"page": page, "pages": n, "name": name}))
}

/// What an export of a layout whose texts name installed families is told while the app is still
/// indexing the fonts (its first seconds; the app's exports don't wait for them): those texts
/// are drawn in their fallbacks. A script or the command line indexes them at once.
fn named_fonts_unread(layout: &Layout) -> Option<&'static str> {
    let named = layout.elements.iter().any(|e| matches!(&e.kind, ElementKind::Text { font_name: Some(_), .. }));
    (named && rustgis_render::families::available().is_none()).then_some(
        "the installed fonts are still being read, so texts in a named font family are drawn in their fallback: export again in a moment",
    )
}

/// Layout `li` as a PDF (kept as vectors; every map series page when `all`; its map frames
/// georeferenced) or a PNG of the current page, at `dpi`.
pub fn layout_bytes(s: &mut Session, li: usize, pdf: bool, dpi: f64, all: bool) -> Result<Vec<u8>, String> {
    layout_bytes_with(s, li, pdf, dpi, all, true)
}

/// [`layout_bytes`], a PDF's map frames georeferenced only when `georeference` (#1191).
/// Returns an actionable error if a linked archive changes while the pages are drawn.
pub fn layout_bytes_with(s: &mut Session, li: usize, pdf: bool, dpi: f64, all: bool, georeference: bool) -> Result<Vec<u8>, String> {
    let layout = s.project.layouts.get(li).ok_or("no layout")?;
    let revisions = rustgis_render::layout::archive_revisions(layout, &s.project.maps);
    let result = layout_bytes_inner(s, li, pdf, dpi, all, georeference);
    let layout = s.project.layouts.get(li).ok_or("no layout")?;
    revisions.finish_revision(
        rustgis_render::layout::archive_revisions(layout, &s.project.maps),
        Some(result),
        "could not draw the page",
    )?
}

fn layout_bytes_inner(s: &mut Session, li: usize, pdf: bool, dpi: f64, all: bool, georeference: bool) -> Result<Vec<u8>, String> {
    let saved = s.project.layouts.get(li).ok_or("no layout")?.clone();
    if let Some(note) = named_fonts_unread(&saved) {
        s.warn(note);
    }
    let dpi = if dpi.is_finite() { dpi.clamp(36.0, 600.0) } else { 150.0 };
    let current = saved.series.as_ref().map_or(0, |sr| sr.page);
    // Only a whole-series PDF names every page.
    let info = series_info(s, li, current);
    let names = if all && pdf { series_pages(s, li) } else { None };
    let total = info.as_ref().map_or(1, |i| i.0);
    // Every page of a map series (PDF), or just the layout as it is now.
    let pages: Vec<usize> = match (&names, all && pdf) {
        (Some(n), true) => {
            if n.len() > 500 {
                s.warn(format!("The map series has {} pages; the PDF holds the first 500", n.len()));
            }
            (0..n.len().min(500)).collect()
        }
        _ => vec![current],
    };
    let stepping = names.is_some() && pages.len() > 1;
    // What the PDF draws as pictures because its vectors would grow too large: frames, and
    // whole pages (their surrounds).
    let (mut frames_pictured, mut pages_pictured) = (Vec::new(), Vec::new());
    // The map frames a PDF can't georeference, and those it georeferences by their corners only,
    // each with the page (from 1) it is on.
    let (mut left_out, mut by_corners): (Vec<(usize, u32, String, &'static str)>, Vec<(usize, u32, String)>) = (Vec::new(), Vec::new());
    let mut render_pages = || -> Result<(Vec<rustgis_render::pdf::Page>, Option<Vec<u8>>), String> {
        let mut out = Vec::new();
        let mut png = None;
        let mut held = 0usize;
        // How far a series' pages have come, when the export runs with progress.
        if pages.len() > 1 {
            rustgis_data::progress::begin(pages.len());
        }
        for page in &pages {
            if stepping {
                series_page(s, &json!({"layout": li, "page": page}))?;
            }
            let layout = s.project.layouts.get(li).ok_or("no layout")?;
            let info = rustgis_render::layout::PageInfo {
                page: page + 1,
                pages: total,
                page_name: match &names {
                    Some(n) => n.get(*page).cloned().unwrap_or_default(),
                    None => info.as_ref().map(|i| i.1.clone()).unwrap_or_default(),
                },
            };
            // Each page is kept as vectors (its pictures compressed) as soon as it's drawn, within
            // a memory budget.
            if pdf {
                let (w, h) = (layout.width, layout.height);
                // Where its map frames show, for readers to locate and measure in: a GeoPDF (#1191).
                let geo = if georeference {
                    rustgis_render::layout::frame_viewports(layout, &s.project.maps, dpi)
                } else {
                    rustgis_render::layout::Georeferenced::default()
                };
                left_out.extend(geo.left_out.into_iter().map(|(id, n, why)| (page + 1, id, n, why)));
                by_corners.extend(geo.by_corners.into_iter().map(|(id, n)| (page + 1, id, n)));
                let (drawing, pictured) = rustgis_render::layout::render_drawing(layout, &mut s.project.maps, dpi, &info)?;
                if drawing.overflowed() {
                    pages_pictured.push(page + 1);
                    let layout = s.project.layouts.get(li).ok_or("no layout")?;
                    let img = rustgis_render::layout::render(layout, &mut s.project.maps, dpi, &info)?;
                    out.push(rustgis_render::pdf::Page::new(&img, w, h).with_viewports(geo.viewports));
                } else {
                    frames_pictured.extend(pictured);
                    out.push(rustgis_render::pdf::Page::drawing(drawing, w, h).with_viewports(geo.viewports));
                }
                held += out.last().map_or(0, rustgis_render::pdf::Page::len);
                if held > PDF_BUDGET {
                    return Err(format!(
                        "the PDF passed {} MB at page {}; export at a lower dpi, or fewer pages",
                        PDF_BUDGET >> 20,
                        page + 1
                    ));
                }
            } else {
                let img = rustgis_render::layout::render(layout, &mut s.project.maps, dpi, &info)?;
                png = Some(img.encode_png().map_err(|e| e.to_string())?);
            }
            // A cancelled export stops here, its layout put back below.
            rustgis_data::progress::step(1).map_err(|e| e.to_string())?;
        }
        Ok((out, png))
    };
    let result = render_pages();
    // Stepping through the series moved the frame; put the layout back however that went.
    if stepping && let Some(cur) = s.project.layouts.get_mut(li) {
        *cur = saved.clone();
    }
    let (pages, png) = result?;
    pictured_notices(s, &frames_pictured, &pages_pictured);
    georeference_notices(s, &left_out, &by_corners, pages.len());
    if pdf { Ok(rustgis_render::pdf::write(&pages, &saved.name)) } else { png.ok_or_else(|| "nothing to draw".to_string()) }
}

/// Says which map frames a PDF doesn't georeference, with why, and which it georeferences by their
/// corners only (#1191): one notice for each, with a count. Each entry has the page (from 1) it
/// is on, of the `pages` exported. A frame is counted once however many pages of a series it is
/// on (by its id: two frames sharing a name are two), with the first page's reason, and named with
/// its pages when it is so on some of them only: a series moves each frame, which may cross the
/// 180° meridian on one page and not the next. Information, not warnings: nothing wrong is
/// written (a frame left out carries no coordinates, and one placed by its corners is checked to
/// within half a millimetre).
pub(crate) fn georeference_notices(
    s: &mut Session,
    left_out: &[(usize, u32, String, &'static str)],
    by_corners: &[(usize, u32, String)],
    pages: usize,
) {
    let listed = |items: Vec<String>| {
        let more = items.len().saturating_sub(10);
        let mut text = items.into_iter().take(10).collect::<Vec<_>>().join(", ");
        if more > 0 {
            text.push_str(&format!(" and {more} more"));
        }
        text
    };
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    // Each frame once, in the order first met: its name, the first page's reason and its pages.
    let frames = |entries: Vec<(usize, u32, &str, &'static str)>| {
        let mut frames: Vec<(u32, String, &'static str, Vec<usize>)> = Vec::new();
        for (page, id, name, why) in entries {
            match frames.iter_mut().find(|f| f.0 == id) {
                Some(f) if !f.3.contains(&page) => f.3.push(page),
                Some(_) => {}
                None => frames.push((id, name.to_string(), why, vec![page])),
            }
        }
        frames
    };
    // A frame's name, with its pages when it is so on some of the pages only.
    let named = |name: &str, on: &[usize]| {
        if pages < 2 || on.len() >= pages {
            return name.to_string();
        }
        let mut shown: Vec<String> = on.iter().map(usize::to_string).collect();
        let more = shown.len().saturating_sub(10);
        shown.truncate(10);
        let listed = match (shown.split_last(), more) {
            (Some((last, rest)), 0) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
            (_, 0) => shown.join(", "),
            (_, more) => format!("{} and {more} more", shown.join(", ")),
        };
        format!("{name} on page{} {listed}", plural(on.len()))
    };
    let out: Vec<String> = frames(left_out.iter().map(|(p, id, n, why)| (*p, *id, n.as_str(), *why)).collect())
        .into_iter()
        .map(|(_, n, why, on)| format!("{}: {why}", named(&n, &on)))
        .collect();
    if !out.is_empty() {
        let n = out.len();
        s.notify(format!("The PDF doesn't georeference {n} map frame{} ({})", plural(n), listed(out)), false);
    }
    let corners: Vec<String> = frames(by_corners.iter().map(|(p, id, n)| (*p, *id, n.as_str(), "")).collect())
        .into_iter()
        .map(|(_, n, _, on)| named(&n, &on))
        .collect();
    if !corners.is_empty() {
        let n = corners.len();
        s.notify(
            format!(
                "The PDF georeferences {n} map frame{} by its corners in longitude/latitude, as its coordinate system can't be written as WKT: on a 9 × 9 grid over each, it puts places within 0.5 mm of where they are drawn, not proven between them ({})",
                plural(n),
                listed(corners)
            ),
            false,
        );
    }
}

/// Says which layout elements, map frames and surrounds (by name), and pages (by number) an export
/// draws as pictures because their
/// vectors would pass the size a page's content may come to: one notice for each, with a count.
pub(crate) fn pictured_notices(s: &mut Session, frames: &[(u32, String)], pages: &[usize]) {
    let mb = rustgis_render::vector::MAX_COST >> 20;
    // The first of a list, and how many more there are.
    let listed = |items: Vec<String>, most: usize| {
        let more = items.len().saturating_sub(most);
        let mut text = items.into_iter().take(most).collect::<Vec<_>>().join(", ");
        if more > 0 {
            text.push_str(&format!(" and {more} more"));
        }
        text
    };
    // Each element once (a frame on every page of a series is one frame), by its id and name: two
    // elements sharing a name are two.
    let mut elements: Vec<(u32, String)> = frames.to_vec();
    elements.sort_unstable();
    elements.dedup();
    let names: Vec<String> = elements.into_iter().map(|(_, name)| name).collect();
    if !names.is_empty() {
        let n = names.len();
        s.warn(format!(
            "The export draws {n} layout element{} as pictures, not vectors: their vectors would pass {mb} MB ({})",
            if n == 1 { "" } else { "s" },
            listed(names, 10)
        ));
    }
    if !pages.is_empty() {
        let n = pages.len();
        s.warn(format!(
            "The export draws {n} page{} as pictures, not vectors: their vectors would pass {mb} MB (page {})",
            if n == 1 { "" } else { "s" },
            listed(pages.iter().map(usize::to_string).collect(), 20)
        ));
    }
}

/// `layout.export`: a layout to a PNG, PDF or SVG. It draws a copy of the project, so the project
/// can change meanwhile (a map series steps through its pages on the copy); a cancelled export
/// stops at the next page, or its map's wait for tiles, and writes nothing.
pub(crate) fn export_work(s: &Session, p: &Value) -> Result<super::exports::Work, String> {
    let path = ps(p, "path").ok_or("path is required")?.to_string();
    let li = index(s, p)?;
    let lower = path.to_ascii_lowercase();
    let (pdf, svg) = (lower.ends_with(".pdf"), lower.ends_with(".svg"));
    if !pdf && !svg && !lower.ends_with(".png") {
        return Err("export to a .png, .pdf or .svg file".into());
    }
    let (dpi, all, georeference) = (f(p, "dpi").unwrap_or(150.0), b(p, "allPages").unwrap_or(true), b(p, "georeference").unwrap_or(true));
    let mut project = s.project.clone();
    for m in &mut project.maps {
        m.detach_reads();
    }
    Ok(Box::new(move || {
        let mut copy = Session::new(project);
        let written = (|| {
            let bytes = if svg {
                layout_svg(&mut copy, li, dpi)?.into_bytes()
            } else {
                layout_bytes_with(&mut copy, li, pdf, dpi, all, georeference)?
            };
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            rustgis_data::io::write_atomic(Path::new(&path), &bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!({"path": path, "bytes": bytes.len()}))
        })();
        // What drawing it said (frames drawn as pictures, fonts it lacks), as the command says it
        // here; past the notices a session keeps, how many earlier ones there were.
        let kept = std::mem::take(&mut copy.notices);
        let earlier = copy.notices_given().saturating_sub(kept.len() as u64);
        let mut said: Vec<_> = Vec::new();
        if earlier > 0 {
            said.push((
                rustgis_data::diagnostics::Severity::Warning,
                format!("{path}: {earlier} earlier notices from drawing it aren't shown"),
            ));
        }
        said.extend(kept.into_iter().map(|n| (n.severity, n.text)));
        super::exports::drawn(&copy.project.maps, said, written, path)
    }))
}

/// The layout `li` as it is now (a map series at its current page) as SVG, kept as vectors with
/// pictures at `dpi`; a frame too large for vectors is a picture, and the export says so.
fn layout_svg(s: &mut Session, li: usize, dpi: f64) -> Result<String, String> {
    let dpi = if dpi.is_finite() { dpi.clamp(36.0, 600.0) } else { 150.0 };
    let current = s.project.layouts.get(li).ok_or("no layout")?.series.as_ref().map_or(0, |sr| sr.page);
    let series = series_info(s, li, current);
    let info = rustgis_render::layout::PageInfo {
        page: current + 1,
        pages: series.as_ref().map_or(1, |i| i.0),
        page_name: series.map(|i| i.1).unwrap_or_default(),
    };
    // Rendering borrows the current layout; it does not step or modify the series page.
    let layout = s.project.layouts.get(li).ok_or("no layout")?;
    let (text, pictured) = rustgis_render::svg::layout_svg(layout, &mut s.project.maps, dpi, &info)?;
    pictured_notices(s, &pictured, &[]);
    Ok(text)
}

pub(crate) fn import(s: &mut Session, text: &str) -> CmdResult {
    if text.len() as u64 > MAX_LAYOUT {
        return Err("the layout file is too large".into());
    }
    let mut l: Layout = serde_json::from_str(text).map_err(|e| format!("not a RustGIS layout: {e}"))?;
    // Refused before anything is said of it.
    if s.project.layouts.len() >= crate::project::MAX_LAYOUTS {
        return Err(format!("a project holds at most {} layouts; remove one first", crate::project::MAX_LAYOUTS));
    }
    let repairs = l.clamp();
    if !repairs.is_empty() {
        for notice in crate::project::layout_notices(&l.name, "while importing the layout file", 0, &[(l.name.clone(), repairs)]) {
            s.notify(notice, false);
        }
    }
    l.name = unique_name(s, &l.name);
    // Frames showing maps this project lacks show the active map instead.
    let fallback = s.map().name.clone();
    let names: Vec<String> = s.project.maps.iter().map(|m| m.name.to_ascii_lowercase()).collect();
    let mut swapped = 0usize;
    for e in &mut l.elements {
        if let ElementKind::MapFrame { map, .. } = &mut e.kind
            && !names.contains(&map.to_ascii_lowercase())
        {
            *map = fallback.clone();
            swapped = swapped.saturating_add(1);
        }
    }
    if swapped > 0 {
        s.notify(
            format!(
                "{}: {swapped} map frame(s) showing a map this project lacks show \"{}\" instead",
                crate::project::short_name(&l.name),
                crate::project::short_name(&fallback)
            ),
            false,
        );
    }
    let name = l.name.clone();
    if s.project.layouts.len() >= crate::project::MAX_LAYOUTS {
        return Err(format!("a project holds at most {} layouts; remove one first", crate::project::MAX_LAYOUTS));
    }
    s.project.layouts.push(l);
    let i = s.project.layouts.len() - 1;
    s.active_layout = Some(i);
    Ok(json!({"layout": i, "name": name}))
}

/// Selects the explicit or first feature layer in the stored frame's map.
fn frame_layer_name(input: &impl super::InputSource, l: &Layout, frame: u32, p: &Value) -> Result<String, String> {
    let context = input.input_context();
    let s = &context;
    let map = match l.element(frame).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { map, .. }) => map.clone(),
        _ => return Err("no such frame".into()),
    };
    let m = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(&map)).ok_or_else(|| format!("no map '{map}'"))?;
    let found = match ps(p, "layer") {
        Some(n) => m.layer_by_name(n),
        None => m.layers.iter().find(|l| !l.is_raster() && !l.graphics),
    };
    found.map(|l| l.name.clone()).ok_or_else(|| "no such layer in the frame's map".to_string())
}

/// Resolves a frame's stored map literally; missing frame maps are errors here.
fn frame_map<'a>(input: &'a impl super::InputSource, l: &Layout, frame: u32) -> Result<&'a rustgis_map::Map, String> {
    let context = input.input_context();
    let s = &context;
    let map = match l.element(frame).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { map, .. }) => map,
        _ => return Err("no such frame".into()),
    };
    s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)).ok_or_else(|| format!("no map '{map}'"))
}
/// Binds the chosen frame map and the layers whose extent this zoom consumes.
fn frame_zoom_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let layout = s.project.layouts.get(index(input, p)?).ok_or("no layout")?;
    let frame = zoom_frame(layout, p)?;
    let map = frame_map(input, layout, frame)?;
    let mut inputs = match ps(p, "to").unwrap_or("view") {
        "layer" => super::Inputs::named(map, [ps(p, "layer").ok_or("layer is required")?])?,
        "full" | "selection" => super::Inputs::layers(map.layers.iter().map(|l| (map, l))),
        _ => super::Inputs::layers(std::iter::empty()),
    };
    inputs.maps.push(map.instance_id());
    inputs.state = json!({"layout":index(input,p)?,"frame":frame});
    Ok(inputs)
}
/// Binds a new series to its actual frame map, unless the call only turns it off.
fn series_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    if b(p, "off") == Some(true) {
        return Ok(super::Inputs::layers(std::iter::empty()));
    }
    let layout = s.project.layouts.get(index(input, p)?).ok_or("no layout")?;
    let frame = frame_of(layout, p)?;
    let map = frame_map(input, layout, frame)?;
    let mut inputs = super::Inputs::named(map, [ps(p, "layer").ok_or("layer is required")?])?;
    inputs.maps.push(map.instance_id());
    inputs.state = json!({"layout":index(input,p)?,"frame":frame});
    Ok(inputs)
}
/// Binds the stored series frame/layer before changing its current page.
fn series_page_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let li = index(input, p)?;
    let (layer, _) = series_layer(input, li).ok_or("the layout has no map series")?;
    let mut inputs =
        super::Inputs::layers(s.project.maps.iter().flat_map(|m| m.layers.iter().filter(|l| std::ptr::eq(*l, layer)).map(move |l| (m, l))));
    inputs.state = json!({"layout":li,"series":s.project.layouts.get(li).and_then(|l|l.series.as_ref())});
    Ok(inputs)
}
/// Collects only the selectors the requested surround validates, including chart field precedence.
fn element_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let kind = ps(p, "kind").ok_or("kind is required")?;
    if !matches!(kind, "table" | "chart" | "colorbar" | "legend") {
        let mut inputs = object_inputs(input, p)?;
        if matches!(kind, "northArrow" | "scaleBar" | "extentIndicator" | "grid") {
            let layout = s.project.layouts.get(index(input, p)?).ok_or("no layout")?;
            inputs.state["frame"] = json!(frame_of(layout, p)?);
        }
        return Ok(inputs);
    }
    let layout = s.project.layouts.get(index(input, p)?).ok_or("no layout")?;
    let frame = frame_of(layout, p)?;
    if kind == "legend" && legend_exclude(input, layout, frame, p)?.is_none() {
        let mut inputs = object_inputs(input, p)?;
        inputs.state["frame"] = json!(frame);
        return Ok(inputs);
    }
    let map = frame_map(input, layout, frame)?;
    let mut inputs = super::Inputs::layers(std::iter::empty());
    if matches!(kind, "table" | "chart") {
        let name = frame_layer_name(input, layout, frame, p)?;
        inputs.extend(super::Inputs::named(map, [name.as_str()])?);
        if kind == "chart"
            && let Some((m, l)) = chart_field_layer(input, &name)
        {
            inputs.layers.push((m.instance_id(), l.id));
        }
    } else if kind == "colorbar" {
        if let Some(name) = colorbar_layer(input, layout, frame, ps(p, "layer"))? {
            inputs.extend(super::Inputs::named(map, [name.as_str()])?);
        }
    } else if let Some(names) = legend_exclude(input, layout, frame, p)? {
        inputs.layers.extend(names.iter().filter_map(|name| map.layer_by_name(name)).map(|l| (map.instance_id(), l.id)));
        // Tile exclusions are stored selector text, not invented layer/table identities.
        inputs.state = json!(names);
    }
    inputs.maps.push(map.instance_id());
    inputs.state = json!({"layout":index(input,p)?,"frame":frame,"selectors":inputs.state});
    Ok(inputs)
}
/// Resolves the same active/name/index layout used by export before its dependency walk.
fn layout_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    Ok(render_inputs_at(input, index(input, p)?))
}
/// Collects only maps actually referenced by renderer frames; never renders or opens a source.
pub(crate) fn render_inputs_at(input: &impl super::InputSource, li: usize) -> super::Inputs {
    let context = input.input_context();
    let s = &context;
    let Some(layout) = s.project.layouts.get(li) else { return super::Inputs::unresolved() };
    let mut inputs = super::Inputs::layers(std::iter::empty());
    for element in &layout.elements {
        // A missing stored map renders a placeholder, and consumes no map/layer source.
        if let ElementKind::MapFrame { map, .. } = &element.kind
            && let Some(map) = rustgis_render::layout::map_index(&s.project.maps, map).and_then(|i| s.project.maps.get(i))
        {
            inputs.extend(super::tiles::render_inputs(map));
        }
    }
    inputs.state = json!({"layout":li,"name":layout.name,"selectors":layout.elements.iter().filter_map(|e| element_selectors(&e.kind).map(|v|(e.id,v))).collect::<Vec<_>>(),"series":layout.series,"tiles":inputs.state});
    inputs
}

/// Charts find fields in the first matching project layer, separately from the frame's layer.
fn chart_field_layer<'a>(input: &'a impl super::InputSource, name: &str) -> Option<(&'a rustgis_map::Map, &'a rustgis_map::Layer)> {
    let context = input.input_context();
    let s = &context;
    s.project.maps.iter().find_map(|map| map.layer_by_name(name).map(|layer| (map, layer)))
}

/// Finds only the generated, unreferenced globe map that publication may replace.
fn locator_orphan(input: &impl super::InputSource) -> Option<usize> {
    let context = input.input_context();
    let s = &context;
    let src = s.map();
    let base = format!("{} Locator", src.name);
    let shown = |n: &str| {
        s.project
            .layouts
            .iter()
            .flat_map(|l| &l.elements)
            .any(|e| matches!(&e.kind, ElementKind::MapFrame { map, .. } if map.eq_ignore_ascii_case(n)))
    };
    let ours = |n: &str| {
        n.strip_prefix(base.as_str())
            .is_some_and(|rest| rest.is_empty() || rest.strip_prefix(' ').is_some_and(|k| k.parse::<u32>().is_ok()))
    };
    // And made as this command makes them: no web tiles, only unlabelled polygon layers,
    // so a globe map made by hand under that name is left alone.
    let made_here = |x: &rustgis_map::Map| {
        x.tiles.is_empty() && x.layers.iter().all(|l| l.geometry_type() == rustgis_data::GeometryType::Polygon && !l.labels.enabled)
    };
    s.project.maps.iter().position(|x| x.projection.is_globe() && ours(&x.name) && !shown(&x.name) && x.name != src.name && made_here(x))
}

/// Whether the publication gives the top visible raster a colorbar: `"auto"` (the default) or
/// `"none"`.
fn publication_colorbar(p: &Value) -> Result<bool, String> {
    match p.get("colorbar").filter(|v| !v.is_null()) {
        None => Ok(true),
        Some(v) => match v.as_str().map(str::trim) {
            Some("auto") => Ok(true),
            Some("none") => Ok(false),
            _ => Err("colorbar is \"auto\" or \"none\"".into()),
        },
    }
}

/// Parses the publication's locator choice with the sheet-specific default.
fn publication_locator(p: &Value, sheet: bool) -> Result<Locator, String> {
    Ok(match p.get("locator").filter(|v| !v.is_null()) {
        None => {
            if sheet {
                Locator::Globe
            } else {
                Locator::None
            }
        }
        Some(v) => match v.as_str().map(str::trim) {
            Some("globe") => Locator::Globe,
            Some("inset") => Locator::Inset,
            Some("none") => Locator::None,
            _ => return Err("locator is \"globe\", \"inset\" or \"none\"".into()),
        },
    })
}

/// Binds the active shown inputs and the optional generated map replacement.
fn publication_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let (_, profile) = publication_profile(p)?;
    let locator = publication_locator(p, profile.sheet)?;
    let map = s.map();
    let mut inputs = super::Inputs::layers(map.layers.iter().filter(|l| map.shows(l)).map(|l| (map, l)));
    inputs.maps.push(map.instance_id());
    if matches!(locator, Locator::Globe) {
        if let Some(old) = locator_orphan(input).and_then(|i| s.project.maps.get(i)) {
            inputs.maps.push(old.instance_id());
            inputs.layers.extend(old.layers.iter().map(|l| (old.instance_id(), l.id)));
        } else {
            inputs.state = json!({"locator":"new map"});
        }
    }
    Ok(inputs)
}

/// Resolves the profile and automatic sheet orientation without laying out or rendering.
fn publication_profile(p: &Value) -> Result<(Option<String>, &'static PublicationProfile), String> {
    let id = match p.get("profile").filter(|v| !v.is_null()) {
        None => "a4".to_string(),
        Some(v) => v.as_str().map(|t| t.trim().to_ascii_lowercase()).ok_or("profile is a profile's name, such as \"a4-portrait\"")?,
    };
    // A sheet's size alone ("a4", the default): its orientation chosen below, from the area's
    // shape, portrait until then.
    let auto = matches!(id.as_str(), "a4" | "a3" | "a2").then(|| id.clone());
    let id = match &auto {
        Some(size) => format!("{size}-portrait"),
        None => id,
    };
    let profile = PUBLICATION_PROFILES.iter().find(|pr| pr.id == id).ok_or_else(|| {
        let names: Vec<&str> = PUBLICATION_PROFILES.iter().map(|pr| pr.id).collect();
        format!("unknown profile '{id}': a4, a3 or a2 (turned to the area's shape), {}", names.join(", "))
    })?;
    Ok((auto, profile))
}

/// Validates a table/chart's effective layer and field before mutation; input binding uses this same choice.
fn checked_update(input: &impl super::InputSource, p: &Value, li: usize, id: u32) -> Result<Option<(String, Option<String>)>, String> {
    let context = input.input_context();
    let s = &context;
    Ok({
        let l = s.project.layouts.get(li).ok_or("no layout")?;
        let e = l.element(id).ok_or_else(|| format!("no element {id}"))?;
        match (&e.kind, ps(p, "layer"), ps(p, "field")) {
            (ElementKind::Table { frame, layer, .. } | ElementKind::Chart { frame, layer, .. }, nl, nf) if nl.is_some() || nf.is_some() => {
                let map = match l.element(*frame).map(|f| &f.kind) {
                    Some(ElementKind::MapFrame { map, .. }) => map.clone(),
                    _ => return Err("the element's map frame is gone".into()),
                };
                let m = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(&map)).ok_or_else(|| format!("no map '{map}'"))?;
                let target = m
                    .layer_by_name(nl.unwrap_or(layer))
                    .filter(|l| !l.is_raster())
                    .ok_or_else(|| format!("no feature layer '{}' in {map}", nl.unwrap_or(layer)))?;
                // Only charts have a field.
                let field = match nf.filter(|_| matches!(e.kind, ElementKind::Chart { .. })) {
                    Some(fl) => Some(
                        target
                            .dataset
                            .field_index(fl)
                            .and_then(|i| target.dataset.fields.get(i))
                            .filter(|f| f.ty.is_numeric())
                            .map(|f| f.name.clone())
                            .ok_or_else(|| format!("no numeric field '{fl}' in {}", target.name))?,
                    ),
                    // A chart moved to another layer keeps its field only if that layer has it as a
                    // number; otherwise it charts the layer's first numeric field.
                    None => match &e.kind {
                        ElementKind::Chart { field, .. } if nl.is_some() => {
                            let numeric = |f: &&rustgis_data::Field| f.ty.is_numeric();
                            let keep = target.dataset.field_index(field).and_then(|i| target.dataset.fields.get(i)).filter(numeric);
                            let f = keep
                                .or_else(|| target.dataset.fields.iter().find(numeric))
                                .ok_or_else(|| format!("{} has no numeric field to chart", target.name))?;
                            Some(f.name.clone())
                        }
                        _ => None,
                    },
                };
                Some((target.name.clone(), field))
            }
            _ => None,
        }
    })
}

/// An element key overrides frame even when null, using frame_of's existing fallback semantics.
fn zoom_frame(l: &Layout, p: &Value) -> Result<u32, String> {
    match p.get("element") {
        Some(_) => frame_of(l, &json!({"frame":p.get("element")})),
        None => frame_of(l, p),
    }
}
/// String names select another map; other values retain the active-map convention.
fn added_frame_map<'a>(input: &'a impl super::InputSource, p: &Value) -> Result<&'a rustgis_map::Map, String> {
    let context = input.input_context();
    let s = &context;
    match ps(p, "map") {
        Some(n) => s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(n)).ok_or_else(|| format!("no map '{n}'")),
        None => Ok(s.map()),
    }
}
/// Records the map whose current view initializes the new frame.
fn add_frame_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let mut inputs = object_inputs(input, p)?;
    inputs.maps.push(added_frame_map(input, p)?.instance_id());
    Ok(inputs)
}
/// Records only stored references actually validated by this element update.
fn update_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let li = index(input, p)?;
    let id = element_id(p)?;
    let l = s.project.layouts.get(li).ok_or("no layout")?;
    let mut inputs = super::Inputs::layers(std::iter::empty());
    if let Some((name, _)) = checked_update(input, p, li, id)? {
        let frame = match l.element(id).map(|e| &e.kind) {
            Some(ElementKind::Table { frame, .. } | ElementKind::Chart { frame, .. }) => *frame,
            _ => return Ok(inputs),
        };
        inputs.extend(super::Inputs::named(frame_map(input, l, frame)?, [name.as_str()])?);
    }
    match l.element(id).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { .. }) if ps(p, "map").is_some() => inputs.maps.push(added_frame_map(input, p)?.instance_id()),
        Some(ElementKind::Colorbar { frame, .. }) if ps(p, "layer").is_some_and(|n| !n.trim().is_empty()) => {
            if let Some(name) = colorbar_layer(input, l, *frame, ps(p, "layer"))? {
                inputs.extend(super::Inputs::named(frame_map(input, l, *frame)?, [name.as_str()])?);
            }
        }
        Some(ElementKind::Legend { frame, .. }) => {
            if let Some(names) = legend_exclude(input, l, *frame, p)? {
                let map = frame_map(input, l, *frame)?;
                inputs.layers.extend(names.iter().filter_map(|n| map.layer_by_name(n)).map(|l| (map.instance_id(), l.id)));
                inputs.state = json!(names);
            }
        }
        _ => {}
    }
    inputs.state = json!({"layout":li,"element":id,"kind":l.element(id).map(|e|e.kind.code()),"stored":l.element(id).and_then(|e|element_selectors(&e.kind)),"selectors":inputs.state});
    Ok(inputs)
}

/// Reference-only operations bind the actual chosen layout, without reading its stored maps.
fn object_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let i = index(input, p)?;
    let layout = s.project.layouts.get(i).ok_or("no layout")?;
    let mut inputs = super::Inputs::layers(std::iter::empty());
    inputs.state = json!({"layout":i,"name":layout.name});
    Ok(inputs)
}
/// Browser name-based actions use their existing name-to-layout parameter projection.
fn named_object_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    object_inputs(input, &by_name(p))
}
/// Closing the layout view succeeds even when none is open.
fn close_inputs(input: &super::InputContext<'_>, _: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let mut inputs = super::Inputs::layers(std::iter::empty());
    inputs.state = json!({"activeLayout":s.active_layout});
    Ok(inputs)
}

/// Stored surround references consumed by rendering or validation, kept as private selector text.
fn element_selectors(kind: &ElementKind) -> Option<Value> {
    Some(match kind {
        ElementKind::MapFrame { map, extent_of, .. } => json!({"map":map,"extentOf":extent_of}),
        ElementKind::Table { frame, layer, .. } => json!({"frame":frame,"layer":layer}),
        ElementKind::Chart { frame, layer, field, label } => json!({"frame":frame,"layer":layer,"field":field,"label":label}),
        ElementKind::Legend { frame, exclude, .. } => json!({"frame":frame,"exclude":exclude}),
        ElementKind::Colorbar { frame, layer, .. } => json!({"frame":frame,"layer":layer}),
        ElementKind::NorthArrow { frame, .. } | ElementKind::ScaleBar { frame, .. } => json!({"frame":frame}),
        _ => return None,
    })
}
