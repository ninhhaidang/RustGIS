//! Home › Project (New Layout, Import Layout), Publish › Map Frames, Map Surrounds and Text, and
//! Publish › Output (Export Layout) and Save As (Layout File): page layouts.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use rustgis_geom::{Coord, Extent};
use rustgis_map::layout::{
    Arrange, ArrowStyle, Background, CHART_BAR, CHART_BARS, ChartOrder, CoordFormat, ElementKind, FontFamily, FrameShape, Grid, GridEdges,
    GridKind, GridStyle, Guides, LEGEND_TEXT_SIZE, Layout, LineDir, LineHead, MAX_CHART_BARS, MAX_CHART_DECIMALS, MAX_COLORBAR_TICKS,
    MAX_GRID_WIDTH, MAX_GUIDES, MAX_LEGEND_COLUMNS, MAX_LEGEND_LABEL_CHARS, MAX_LEGEND_LABELS, MAX_SCALE_DIVISIONS, MAX_TICK_MM,
    MIN_ELEMENT_MM, MapSeries, Orientation, PAGES, ScaleBarStyle, ScaleUnits, TextAlign, TickSide, fit_scale, line_angle, line_at_angle,
    line_length,
};
use rustgis_map::report::StatKind;
use rustgis_map::symbology::{Dash, MAX_DASH_STEP, MAX_DASH_STEPS};
use rustgis_map::{Map, Rgba};
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
            r#"{"profile"?: "a4" (the default) | "letter"|"legal"|"tabloid"|"a5"|"a3"|"a2"|"a1"|"a0" (a sheet turned to portrait or landscape, whichever the area's map comes out larger on: its extent east to west against north to south, on the map's projection) | a size with its orientation: "a4-portrait"|"a4-landscape"|"letter-portrait"|"tabloid-landscape"|… for each of those sizes (a sheet: frame, title, subtitle, the map, a boxed footer, all scaled to the sheet)|"figure-1col" (90 mm)|"figure-1.5col" (140 mm)|"figure-2col" (190 mm)|"thesis-full" (160 × 225 mm)|"thesis-half" (160 × 110 mm) (a figure: the map edge to edge, its title and sources left to the caption), "extent"?: [west, south, east, north] in degrees that must fit (default: the map view), "name"?: string, "title"?: text, "subtitle"?: text, "sources"?: text | [texts], "reference"?: text | [texts] (default: the projection, datum and graticule), "note"?: text, "author"?: text (default: "Made with RustGIS · {date}"; "" or [] for none), "headings"?: [the sources' heading, the reference's heading, an editorial column's heading], "locator"?: "globe" (a globe with the area outlined; the default on sheets, but for a map on the globe with no extent, the globe seen whole, which by default has no locator (one asked for by name is drawn) and no scale bar, the globe filling its frame) | "inset" (the surrounding region, the area outlined) | "none" (the default on figures), "decimal"?: "." (the default) | "," (graticule, scale bar and colorbar numbers and {scale} with a decimal comma: 0,05 and 1:6.300.000), "colorbar"?: "auto" (the default: the top visible raster with a colour scale, not drawn by classes, gets one, under the legend in its box; a raster drawn by classes has them in the legend's rows instead) | "none" (no colorbar, and the legend's box has no room for one), "veil"?: true (the default: when no shape of the legend fits, a map that runs on past the extent, with a long legend, has what is outside the extent veiled and its surrounds grouped there, at up to half the scale on a sheet) | false (never veiled, and drawn at its scale; the map may still move aside), "contextInLegend"?: false (the default: layers the placement takes as context around the data, polygons only outlined or, beside a layer drawn by classes or a raster, in one colour and running on past the frame, are left out of the legend, which a warning names) | true (the legend lists them), "northArrow"?: "auto" (the default: none when the extent spans 20° or more on its longer side, where the graticule gives north, which a warning says) | "always" | "none", "legendBox"?: "auto" (the default: over the map, the legend has no box where the map shows nothing under it but its light plain paper or water, with no layer, context, basemap shape or graticule line there and no veil; the answer's "legendBox" says whether it has one) | "always", "style"?: "classic" (the default: the title over the map, the footer under it) | "banner" (the title and subtitle in a band of the accent colour across the top) | "panel" (the map at the left, a panel at the right with the title, locator, legend, sources and reference) | "editorial" (the title large in serif, a column at the right with the note, the locator and the legend) | "modular" (cards on a light page: the map, the locator and the legend each in one, the sources and reference in tinted ones) | "poster" (the page in the accent colour, the title in light text, the map in a light card, the footer in darker blocks) (a sheet's arrangement; figures leave it to the caption), "accent"?: a colour as #RRGGBB (the style's colour: its band, panel tint, headings and rules, a poster's page), "arrangement"?: "best" (the default: the favourite, each arrangement by name and each saved with layout.arrangement laid out, the one moving the least area off its corners kept, then the one leaving the least over the data, the favourite on a tie) | "favourite" (layout.arrangement's) | an arrangement by name (built in or saved) or corner by corner, as layout.arrangement takes (the corners the locator, north arrow, legend and scale bar are at home in; one covering what the map draws there still moves), "coverage"?: "*.png" (writes the coverage grid the placement searched, as it settled on it, three pixels a cell: what the map draws dark, the free cells white, both tinted blue in the strips the graticule's labels keep, the surrounds placed over the map outlined in red; the answer gives its path)} → a new layout of the active map at a round scale, its texts wrapped to their boxes; {projection}, {datum} and {grid} in the texts are filled from the map"#,
        ),
        C::new("layout.arrangement", "Favourite Arrangement", "Publish › Layout", arrangement).roles(&[]).effects(crate::policy::Effects::NONE).view().params(
            r#"{"favourite"?: "classic" (the locator top left, the north arrow top right, the legend bottom left, the scale bar bottom right) | "mirrored" | "legendRight" | "legendTop" | {"locator", "northArrow", "legend", "scaleBar": "top-left"|"top-right"|"bottom-left"|"bottom-right", each a corner of its own} | a saved arrangement's name, "save"?: a name (1 to 40 characters, not a built-in one nor "best", "favourite", "custom" or "veil"; one saved under it already, case aside, is replaced) to save "arrangement" under (at most 20 saved), "arrangement"?: what "save" saves: an arrangement by name, or a corner for each surround as "favourite" takes them (refused without "save"), "remove"?: a saved arrangement's name (a favourite named after it keeps its corners, unless "favourite" gives another)} → the favourite arrangement of publication layouts and those saved by name (kept with the app's preferences), the favourite's corners, the arrangements by name and the saved ones' corners; a removal, then a save, then the favourite; with none of them given, they are only shown"#,
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
            let name = super::within("newName", "a layout's name", name, super::MAX_NAME)?;
            // Unique among the other layouts, so names keep finding one layout.
            let unique = numbered(&name, |n| s.project.layouts.iter().enumerate().any(|(j, l)| j != i && l.name.eq_ignore_ascii_case(n)));
            if let Some(l) = s.project.layouts.get_mut(i) {
                l.name = unique.clone();
            }
            super::say_name_cut(s, "Rename Layout", "the name given", &name, &unique);
            Ok(json!({"name": unique}))
        }).inputs(named_object_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name"?: string, "layout"?: name|index (instead of name; default: the open layout), "newName": string}"#),
        // A copy of a layout with all its elements (#288), right after it and open. Element ids
        // are per layout, so legends and scale bars stay on their frames, which show the same maps.
        C::new("layout.duplicate", "Duplicate Layout", "Browser", |s, p| {
            let i = index(s, &by_name(p))?;
            // A name given past the limit is refused, as layout.rename refuses it, not cut (#2639).
            let given = ps(p, "newName").map(str::trim).filter(|n| !n.is_empty());
            let given = given.map(|n| super::within("newName", "a layout's name", n, super::MAX_NAME)).transpose()?;
            let mut l = s.project.layouts.get(i).ok_or("open a layout first")?.clone();
            let (name, cut) = super::copy_name("Duplicate Layout", given.as_deref(), &l.name, |n| {
                s.project.layouts.iter().any(|x| x.name.eq_ignore_ascii_case(n))
            });
            l.name = name.clone();
            if s.project.layouts.len() >= crate::project::MAX_LAYOUTS {
                return Err(format!("a project holds at most {} layouts; remove one first", crate::project::MAX_LAYOUTS));
            }
            s.project.layouts.insert(i + 1, l);
            s.active_layout = Some(i + 1);
            if let Some(cut) = cut {
                s.notify(cut, false);
            }
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
            r##"{"layout"?, "kind": "northArrow"|"scaleBar"|"legend"|"colorbar"|"text"|"dynamicText"|"picture"|"table"|"chart"|"rectangle" (a neatline framing the page unless any of x, y, w, h is given)|"ellipse"|"circle"|"line"|"arrow"|"extentIndicator"|"grid", "frame"?: element id (default: the first map frame), "of"?: frame id (extentIndicator, which also takes "color" and "width" in points, default red and 2), "text"?, "size"?, "color"?: "#RRGGBB"|"#RRGGBBAA" (text, dynamicText, line, arrow, rectangle, ellipse, circle: its outline; extentIndicator; grid; scaleBar; legend, colorbar, table, chart: the ink of its text and lines, default #222222, a chart's bars and a colorbar's colours keeping theirs, and a table drawn without its white cells; refused for other kinds), "bold"?: bool (text), "italic"?: bool, "align"?: "left"|"center"|"right", "letterSpacing"?: points, -5 to 50 (text, dynamicText), "rotation"?: degrees clockwise about the box's centre, 90 reading top to bottom and -90 bottom to top, kept within -180 to 180 (text, dynamicText; refused for other kinds), "wrap"?: bool (text, dynamicText: broken into lines at spaces to fit the box's width when drawn, after its dynamic text is filled in, a word wider than the box broken across lines; default false, lines breaking only at the text's own line breaks; refused for other kinds), "font"?: "sans"|"serif"|an installed font family's name, as labels.fonts lists them (text, dynamicText; a family not installed here is drawn in its fallback, and a serif where none is installed in the sans, with a warning; refused for other kinds), "fallback"?: "sans"|"serif" (a named font's, where it isn't installed; default its own class, else sans), "title"?: legend title (default "Legend"); chart: its own title ("" for none; default: its field, or the statistic of its field by the group), "allClasses"?: bool (legend: every class, not only those the frame draws), "hideOther"?: bool (legend: leave out <all other values>), "showNoData"?: bool (legend: include Graduated Colors’ missing-value symbol; default false), "exclude"?: [layer names] (legend: layers it leaves out though the map draws them), "columns"?: 1..3 (legend: the columns its rows are laid in; default 1), "textSize"?: points, 4 to 100 (legend: its layers' names and classes' labels, its title, rows, swatches and gaps following in proportion and a point symbol keeping the size the map draws it at; default 9; refused outside that range, and for other kinds unless null, as showNoData is), "orientation"?: "horizontal"|"vertical" (colorbar; chart: bars running across, the default, or columns standing up, each label centred under its column), "ticks"?: 2..21 (colorbar: labelled values, ends included), "tickSide"?: "below"|"above"|"right"|"left" (colorbar), "reverse"?: bool (colorbar: high values first), "fill"?: "#RRGGBBAA" (rectangle, ellipse, circle, northArrow; scaleBar: the alternating bar's light divisions, default white; refused for other kinds), "width"?: outline width in points (rectangle, ellipse, circle; 0 for none) or line width in points (line, grid), "radius"?: mm, 0 to 50 (rectangle: rounded corners), "dash"?: [points, …] (line, arrow, and a rectangle's, ellipse's or circle's outline: dash, gap, … in turn, as layer.lineStrokes takes it, at most 8, each 0 to 500; an arrow's heads stay solid; [] or null for solid; refused, null too, for other kinds), "dir"?: "horizontal"|"vertical"|"falling"|"rising" (line), "head"?: "none"|"end"|"start"|"both" (line; default "end" for an arrow), "angle"?: degrees anticlockwise from pointing right, centred on the page unless x or y is given (line, arrow; it sets the box and the direction, so w, h and dir are not used with it), "length"?: mm (line, arrow), "image"?: base64, "path"?: picture file, "layer"?: (table, chart; colorbar: a raster layer, default the top visible raster with a colour scale, not drawn by classes, which a legend lists), "field"?: (chart: a numeric field, default the layer's first; any field when it groups and counts, default then the field it groups by), "label"?: a field (chart: each bar's label when not grouped), "group"?: a field (chart: a bar per value of this field, its statistic of field over that value's features; blank values make one "(blank)" bar; default none, a bar per feature), "statistic"?: "sum" (the default)|"count" (features, numbers or not)|"mean"|"min"|"max" (chart, grouped), "order"?: "descending" (the default: largest first)|"ascending"|"category" (by label, A to Z)|"layer" (as the features come) (chart), "bars"?: 1..100 (chart: the most bars drawn, default 20), "other"?: bool (chart: the bars past "bars" gathered in a last Other bar, its statistic over their features, their sum when not grouped, last whatever the order; default false: left out, which a warning says, as it says features with no number left out), "stack"?: a field (chart, grouped by another field, summing or counting: each bar split by this field's values, a stretch per value in the key over the bars, the values its bars show by their total, largest first (A to Z when ordered by category), past 8 values 7 and a grey Other, which a warning counts; in the palette diagrams take, or with classes in the colour each value's features share on the map, a value with several keeping its palette colour, which a warning counts; values written are the bar's total; default none), "values"?: bool (chart: each bar's value written past its end, its thousands apart with commas; default false), "decimals"?: "auto" (the default: up to 2, the zeros they end in dropped)|0..6 (chart: its values' decimals), "barColor"?: "#RRGGBB"|"#RRGGBBAA" (chart: its bars' colour, default #3B82C4), "classes"?: bool (chart: each bar in the colour its features have on the map, by the layer's symbology classes: a bar per feature its feature's, a grouped bar the colour its features share; a bar whose features have several, which a warning counts, and an Other bar keep barColor; default false), "labelAlign"?: "left" (the default)|"right" (chart: bars running across have their labels at the left of their column, or at its right against the bars) (label, group, statistic, order, bars, other, stack, values, decimals, barColor, classes and labelAlign refused for other kinds), "rows"?, "step"?: degrees, or a measured grid's units (metres) (0 picks one), "type"?: "graticule" (latitudes and longitudes, default) | "measured" (eastings and northings in a projected system), "crs"?: a measured grid's coordinate system: EPSG:25832, WKT or a PROJ string ("" or none: the map's when projected, else the UTM zone at the frame's centre), "short"?: bool (a measured grid's labels as their kilometres' last two digits, 43 for 543000), "mgrs"?: bool (a measured grid labelled as MGRS: each 100 km square named with its grid zone designation, 32T MT, and the lines by their kilometres' last two digits; its crs a WGS 84 UTM zone by EPSG code, EPSG:32601–32660 or 32701–32760, or none: the map's when it is one, else the zone at the frame's centre), "labels"?: bool (grid) | {"<layer>": "<label>"} (legend: what it shows instead of a layer's name, as in layout.update; a layer the map hasn't is refused), "style"?: "lines"|"ticks"|"linesAndTicks" (grid) or "classic"|"compass"|"triangle"|"star" (northArrow) or "alternating"|"stepped"|"line" (scaleBar), "tickLength"?: mm, "format"?: "decimal"|"dms", "hemisphere"?: bool (N/S/E/W rather than a sign), "edges"?: ["top","bottom","left","right"] (labelled edges), "labelOrientation"?: "horizontal"|"vertical", "off"?: bool (grid; options not given keep the grid's), "outline"?: "#RRGGBB" (northArrow, or a rectangle's, ellipse's or circle's outline instead of color; refused for other kinds), "units"?: "auto"|"kilometers"|"meters"|"miles"|"feet", "divisions"?: 1..10 (scaleBar, whose "color" is its lines, dark divisions and labels, default #222222), "x"?, "y"?, "w"?, "h"?: mm, "background"?: {"fill"?: "#RRGGBB"|"#RRGGBBAA" (default white), "outline"?: colour (default #222222), "width"?: the outline's points (default 0, none), "radius"?: mm, its corners (0 to 50), "padding"?: mm it reaches past the box on every side (0 to 50)} | null (a box drawn behind the element, turned with a turned text; none by default; a map frame takes none, so it is refused for grid and extentIndicator)}"##,
        ),
        C::new("layout.update", "Element Properties", "Publish › Element", update).inputs(update_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).params(
            r##"{"layout"?, "element": id, "x"?, "y"?, "w"?, "h"?: mm, "name"?, "visible"?, "locked"?: bool (a locked element takes no other change until unlocked; "locked": false unlocks it and may come with changes), "text"?, "size"?, "color"? (text, line, rectangle, ellipse: its outline; scaleBar: its lines, dark divisions and labels; legend, colorbar, table, chart: its ink, as in layout.addElement, null giving it the default again; refused for other elements), "bold"?, "italic"?, "align"?, "letterSpacing"?, "rotation"?: degrees clockwise (text, as in layout.addElement; refused for other elements), "wrap"?: bool (text, as in layout.addElement; refused for other elements), "font"?: "sans"|"serif"|an installed font family's name, "fallback"?: "sans"|"serif" (text, as in layout.addElement; refused for other elements), "title"? (legend, colorbar; chart: its own title ("" for none; null giving it the title it is given again)), "allClasses"?: bool (legend), "hideOther"?: bool (legend), "showNoData"?: bool (legend; default false), "exclude"?: [layer names] (legend; [] lists every layer again), "labels"?: {"<layer>": "<label>"|null} (legend: what it shows instead of a layer's name, up to 200 characters, a longer one is refused; each entry replaces that layer's label, null or "" shows its name again, {} every layer's; a layer the map hasn't is refused), "columns"?: 1..3 (legend), "textSize"?: points, 4 to 100 (legend, as in layout.addElement, null giving it the default again; refused outside that range, and for other elements unless null), "orientation"? (colorbar, chart), "ticks"?, "tickSide"?, "reverse"?: (colorbar), "style"?: (northArrow, scaleBar), "fill"?, "outline"? (northArrow, rectangle, ellipse, and a scaleBar's fill; refused for other elements; for a rectangle or ellipse, color sets it when outline is not given; a scaleBar's fill is the alternating bar's light divisions), "radius"?: mm (rectangle: rounded corners), "width"?: points (rectangle, ellipse, line), "dash"?: [points, …]|[]|null (line, rectangle, ellipse, as in layout.addElement; [] or null makes it solid; refused, null too, for other elements), "dir"?, "head"?, "angle"?, "length"? (line: turned or sized about its centre, then moved by any x or y; dir does not come with angle or length), "units"?, "divisions"? (scaleBar), "map"?, "scale"?, "center"?: [lon, lat], "bearing"?: degrees clockwise from north, "pitch"?: degrees of tilt (0 to 85), "grid"?: {grid options as in layout.addElement} | false (map frame), "extentIndicator"?: {"color"?, "width"?: points} | false (map frame: its extent indicator's style, or removes it), "border"?: {"color"?: "#RRGGBB"|"#RRGGBBAA", "width"?: points, 0 to 20, 0 for none} | null (map frame: the outline along its shape, default #303030 and 0.8, the keys not given keeping theirs; null gives the default back; refused for other elements), "layer"?: (colorbar: "" for the top visible raster with a colour scale, not drawn by classes), "field"?, "label"?, "group"?, "statistic"?, "order"?, "bars"?, "other"?, "stack"? (chart, as in layout.addElement: "" for a label, group or stack takes it away; on a new layer a label, group or stack field it hasn't is let go, a stack the chart can no longer draw (ungrouped, grouped by the stack field, or not summing or counting) too, and a field not named that it no longer measures gives way to the first numeric field, which warnings say; refused for other elements), "values"?: bool, "decimals"?: 0..6|"auto", "barColor"? (null giving the default again), "classes"?: bool, "labelAlign"?: "left"|"right" (chart, as in layout.addElement: null leaves each as it is; refused for other elements), "rows"?, "background"?: {"fill"?, "outline"?, "width"?, "radius"?, "padding"?, as in layout.addElement, the keys not given keeping theirs} | null (removes it); a map frame takes none}"##,
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
            .params(r#"{"layout"?, "path": "*.png|*.pdf|*.svg" (PNG/SVG: current page; SVG: each element a group of its own; text and shapes as SVG, map frames and surrounds as vectors, rasters and tiles in them as pictures at dpi), "dpi"?: 72..600, "allPages"?: bool (PDF only; default true: every map series page, up to 500; false: the current page), "georeference"?: bool (PDF only; default true: map frames carry their coordinates, a GeoPDF; false: none), "pdf"?: {"layers"?: bool (default true: each element, and in a map frame each layer and its labels, a PDF layer that vector editors open as a layer; false: none), "text"?: "fonts" (default: text in its faces, embedded as subsets, editable in a vector editor; a face that can't be is set in outlines, and said)|"outlines" (glyphs as outlines, as print shops ask)}}"#),
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

/// `base` (trimmed; "Layout" when blank), numbered when another layout has it, with the notice to
/// give when that cuts it ([`super::name_cut`]): `command` is the command's title and `whose` says
/// where the name came from.
fn unique_name(s: &Session, command: &str, whose: &str, base: &str) -> (String, Option<String>) {
    let base = base.trim();
    let base = if base.is_empty() { "Layout" } else { base };
    let name = numbered(base, |n| s.project.layouts.iter().any(|l| l.name.eq_ignore_ascii_case(n)));
    let cut = super::name_cut(command, whose, base, &name);
    (name, cut)
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
        border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
        border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
        bearing,
        pitch,
    }
}

/// Creates a template and returns its addressable elements without changing the existing layout keys.
pub(crate) fn new_layout(s: &mut Session, p: &Value) -> CmdResult {
    let (w, h) = page_size(
        &json!({"page": ps(p, "page").unwrap_or("letter"), "orientation": ps(p, "orientation").unwrap_or("landscape"), "width": p.get("width"), "height": p.get("height")}),
        None,
    )?;
    let given = super::within("name", "a layout's name", ps(p, "name").unwrap_or("Layout").trim(), super::MAX_NAME)?;
    let (name, cut) = unique_name(s, "New Layout", "the name given", &given);
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
                    show_no_data: false,
                    exclude: Vec::new(),
                    columns: 1,
                    labels: Default::default(),
                    color: None,
                    text_size: rustgis_map::layout::LEGEND_TEXT_PT,
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
    let elements = element_list(&l);
    s.project.layouts.push(l);
    let i = s.project.layouts.len() - 1;
    s.active_layout = Some(i);
    if let Some(cut) = cut {
        s.notify(cut, false);
    }
    Ok(json!({"layout": i, "name": name, "elements": elements}))
}

/// Stable element addresses shared by layout creation and project info, as publication uses.
pub(crate) fn element_list(l: &Layout) -> Vec<Value> {
    l.elements.iter().map(|e| json!({"element": e.id, "name": e.name, "kind": e.kind.code()})).collect()
}

/// One of `layout.publication`'s profiles.
#[derive(Clone, Copy, Debug)]
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

/// `layout.publication`'s figures, for a journal or a thesis: the map edge to edge, its title and
/// sources left to the caption.
const FIGURES: &[PublicationProfile] = &[
    PublicationProfile { id: "figure-1col", label: "Journal figure, 1 column (90 mm)", sheet: false, width: 90.0, height: 0.0 },
    PublicationProfile { id: "figure-1.5col", label: "Journal figure, 1.5 columns (140 mm)", sheet: false, width: 140.0, height: 0.0 },
    PublicationProfile { id: "figure-2col", label: "Journal figure, 2 columns (190 mm)", sheet: false, width: 190.0, height: 0.0 },
    PublicationProfile { id: "thesis-full", label: "Thesis figure, full page (160 × 225 mm)", sheet: false, width: 160.0, height: 225.0 },
    PublicationProfile { id: "thesis-half", label: "Thesis figure, half page (160 × 110 mm)", sheet: false, width: 160.0, height: 110.0 },
];

/// `layout.publication`'s profiles: a sheet for printing on every page size `layout.new` offers
/// ([`PAGES`], in its order), portrait then landscape (`a4-portrait`, "A4 portrait sheet"), then
/// the journal and thesis figures. Built once from the two lists, so a page size added to `PAGES`
/// is a sheet too; its names live for the program, as a constant's would.
pub static PUBLICATION_PROFILES: LazyLock<Vec<PublicationProfile>> = LazyLock::new(|| {
    let name = |text: String| -> &'static str { Box::leak(text.into_boxed_str()) };
    let sheets = PAGES.iter().flat_map(|&(size, label, w, h)| {
        [("portrait", w, h), ("landscape", h, w)].map(|(orientation, width, height)| PublicationProfile {
            id: name(format!("{size}-{orientation}")),
            label: name(format!("{label} {orientation} sheet")),
            sheet: true,
            width,
            height,
        })
    });
    sheets.chain(FIGURES.iter().copied()).collect()
});

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

/// Texts given as one or a list (up to 50), each of at most [`MAX_PUBLICATION_TEXT`] characters: a
/// longer one is refused, not cut.
fn texts_of(p: &Value, k: &str) -> Result<Option<Vec<String>>, String> {
    let within = |t: &str| super::within(k, "a publication layout's text", t, MAX_PUBLICATION_TEXT);
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(t)) => Ok(Some(vec![within(t)?])),
        Some(Value::Array(a)) if a.len() <= 50 => a
            .iter()
            .map(|v| v.as_str().ok_or_else(|| format!("{k} is a text or a list of texts")).and_then(within))
            .collect::<Result<_, _>>()
            .map(Some),
        Some(_) => Err(format!("{k} is a text or a list of up to 50 texts")),
    }
}

/// The extent's height over its width on the active map's projection, along its edges.
fn aspect(s: &Session, e: &Extent) -> Option<f64> {
    let ext = projected_extent(s, e);
    (ext.width() > 0.0 && ext.height() > 0.0).then(|| ext.height() / ext.width())
}

/// A lon/lat extent's bounds on the map's projection, from points along its edges.
fn projected_extent(s: &Session, e: &Extent) -> Extent {
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
    ext
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
    ElementKind::Text {
        text,
        size,
        color,
        bold,
        italic: false,
        align,
        letter_spacing: 0.0,
        font,
        font_name: None,
        rotation: 0.0,
        wrap: false,
    }
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
    ElementKind::Rectangle { fill, outline, width, radius, dash: None }
}

/// Where a page's visual centre sits down it, as a share of its height: a little above its middle,
/// where the eye takes a page's centre to be.
const VISUAL_CENTRE_DOWN: f64 = 0.45;

/// How far `at` (page mm) is from the visual centre of a page `page` (mm) wide and tall.
fn visual_centre_offset(at: (f64, f64), (w, h): (f64, f64)) -> f64 {
    (at.0 - w / 2.0).hypot(at.1 - h * VISUAL_CENTRE_DOWN)
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

/// The part of a coverage grid of a page `wide` (mm) that a frame `size` (mm) with its top left at
/// `at` (mm, from the wide page's top left) covers, to the nearest cell, as a grid of its own; a
/// frame reaching past the wide page is held inside it.
pub(crate) fn crop_cells(
    (gw, gh, cells): &(u32, u32, Vec<bool>),
    wide: (f64, f64),
    at: (f64, f64),
    size: (f64, f64),
) -> (u32, u32, Vec<bool>) {
    let (gw, gh) = ((*gw as usize).max(1), (*gh as usize).max(1));
    let (cell_w, cell_h) = (wide.0 / gw as f64, wide.1 / gh as f64);
    let (w, h) = (((size.0 / cell_w).round() as usize).clamp(1, gw), ((size.1 / cell_h).round() as usize).clamp(1, gh));
    let (c0, r0) = (((at.0 / cell_w).round().max(0.0) as usize).min(gw - w), ((at.1 / cell_h).round().max(0.0) as usize).min(gh - h));
    let out = (r0..r0 + h).flat_map(|r| (c0..c0 + w).map(move |c| cells.get(r * gw + c).copied().unwrap_or(false))).collect();
    (w as u32, h as u32, out)
}

/// The surrounds of a veiled map grouped in a band of its veil: the locator with the north arrow at
/// its right and the scale bar under them, beside the legend or over it, whichever the band's shape
/// suits, centred in it. Gives the legend's, locator's, arrow's and scale bar's places (the locator's
/// and the arrow's when there is one); `None` when they don't fit either way.
pub(crate) fn veil_panel(
    band: Rect,
    legend: (f64, f64),
    locator: Option<f64>,
    arrow: Option<(f64, f64)>,
    scale: (f64, f64),
    gap: f64,
) -> Option<(Rect, Option<Rect>, Option<Rect>, Rect)> {
    let (bx, by, bw, bh) = band;
    let (aw, ah) = arrow.unwrap_or((0.0, 0.0));
    // The locator and the arrow side by side, with a gap between them only when both are there.
    let between = if locator.is_some() && arrow.is_some() { gap } else { 0.0 };
    let top = (locator.map_or(0.0, |side| side + between) + aw, locator.unwrap_or(0.0).max(ah));
    // The gap over the scale bar only when something sits above it.
    let lead = if top.1 > 0.0 { gap } else { 0.0 };
    let group = (top.0.max(scale.0), top.1 + lead + scale.1);
    let beside = legend.0 + gap + group.0 <= bw && legend.1.max(group.1) <= bh;
    let over = legend.0.max(group.0) <= bw && legend.1 + gap + group.1 <= bh;
    if !beside && !over {
        return None;
    }
    let (legend_at, group_at) = if beside && (bw >= bh || !over) {
        let (x, y) = (bx + (bw - legend.0 - gap - group.0) / 2.0, by + (bh - legend.1.max(group.1)) / 2.0);
        ((x, y), (x + legend.0 + gap, y))
    } else {
        let (x, y) = (bx + (bw - legend.0.max(group.0)) / 2.0, by + (bh - legend.1 - gap - group.1) / 2.0);
        ((x, y + group.1 + gap), (x, y))
    };
    Some((
        (legend_at.0, legend_at.1, legend.0, legend.1),
        locator.map(|side| (group_at.0, group_at.1, side, side)),
        arrow.map(|(aw, ah)| (group_at.0 + group.0 - aw, group_at.1, aw, ah)),
        (group_at.0, group_at.1 + top.1 + lead, scale.0, scale.1),
    ))
}

/// A publication layout's colorbar's height, in mm, with a title of one line: its title, the bar and
/// its labels.
const COLORBAR_MM: f64 = 15.0;

/// A publication layout's colorbar's height, in mm, `width` mm wide: [`COLORBAR_MM`] and a line's
/// pitch more for each line its title wraps to past the first (#2493).
fn colorbar_mm(title: &str, width: f64) -> f64 {
    let lines = crate::render::layout::colorbar_title_lines(title, width).len();
    COLORBAR_MM + lines.saturating_sub(1) as f64 * crate::render::layout::line_pitch_mm(crate::render::layout::COLORBAR_TITLE_PT)
}

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

/// At most this many arrangements are saved by name, each name at most this many characters
/// (#1379).
pub const MAX_SAVED_ARRANGEMENTS: usize = 20;
pub const MAX_ARRANGEMENT_NAME: usize = 40;

/// Names an arrangement can't be saved under: those `"arrangement"` reads otherwise, and those
/// the answer gives.
const RESERVED_ARRANGEMENT_NAMES: [&str; 4] = ["best", "favourite", "custom", "veil"];

/// Two arrangement names the same but for case (beyond ASCII too).
fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// The arrangements saved by name (`layout.arrangement {"save": …}`), each its name and corners;
/// an entry that isn't an arrangement is passed over.
fn saved_arrangements(saved: &serde_json::Map<String, Value>) -> impl Iterator<Item = (&String, Homes)> {
    let none = serde_json::Map::new();
    saved
        .iter()
        .filter(|(_, corners)| corners.is_object())
        .filter_map(move |(name, corners)| parse_arrangement(corners, &none).ok().map(|h| (name, h)))
}

/// An arrangement: one of [`ARRANGEMENTS`] or of the `saved` ones by name, or a corner for each
/// surround (`{"locator": "top-left", "northArrow": "top-right", "legend": "bottom-left",
/// "scaleBar": "bottom-right"}`, every corner once). `Value::Null` (none given) is `classic`.
pub fn parse_arrangement(v: &Value, saved: &serde_json::Map<String, Value>) -> Result<Homes, String> {
    let names = || {
        let saved = saved_arrangements(saved).map(|(n, _)| n.as_str());
        ARRANGEMENTS.iter().map(|(n, _)| *n).chain(saved).map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ")
    };
    match v {
        Value::Null => Ok(ARRANGEMENTS[0].1),
        Value::String(name) => arrangement_named(name, saved)
            .map(|(_, h)| h)
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

/// A built-in or saved arrangement by `name` (case aside): its name as it is kept, and its corners.
fn arrangement_named(name: &str, saved: &serde_json::Map<String, Value>) -> Option<(String, Homes)> {
    let name = name.trim();
    let built_in = ARRANGEMENTS.iter().find(|(n, _)| same_name(n, name)).map(|(n, h)| ((*n).to_string(), *h));
    built_in.or_else(|| saved_arrangements(saved).find(|(n, _)| same_name(n, name)).map(|(n, h)| (n.clone(), h)))
}

/// What an arrangement given as `v` is called in the answer: its name; given corner by corner,
/// the name of the built-in or saved one with those corners, else `"custom"`.
fn arrangement_label(v: &Value, homes: Homes, saved: &serde_json::Map<String, Value>) -> String {
    let by_corners = || {
        ARRANGEMENTS
            .iter()
            .find(|(_, h)| *h == homes)
            .map(|(n, _)| (*n).to_string())
            .or_else(|| saved_arrangements(saved).find(|(_, h)| *h == homes).map(|(n, _)| n.clone()))
            .unwrap_or_else(|| "custom".into())
    };
    match v {
        Value::String(name) => arrangement_named(name, saved).map_or_else(by_corners, |(n, _)| n),
        _ => by_corners(),
    }
}

/// The corners of an arrangement, by surround.
fn corners_of(homes: Homes) -> Value {
    Value::Object(ARRANGEMENT_KEYS.iter().zip(homes).map(|(k, h)| ((*k).to_string(), json!(CORNER_NAMES[h]))).collect())
}

/// Refuses a name an arrangement can't be saved under: blank, longer than
/// [`MAX_ARRANGEMENT_NAME`], with control characters, or one `"arrangement"` already reads.
fn check_arrangement_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.trim() != name || name.chars().count() > MAX_ARRANGEMENT_NAME || name.chars().any(char::is_control) {
        return Err(format!("an arrangement's name is 1 to {MAX_ARRANGEMENT_NAME} characters, without control characters"));
    }
    if ARRANGEMENTS.iter().map(|(n, _)| *n).chain(RESERVED_ARRANGEMENT_NAMES).any(|n| same_name(n, name)) {
        return Err(format!("\"{name}\" names an arrangement already: choose another name"));
    }
    Ok(())
}

/// Whether this version would use a saved arrangement `name` with `corners`: a name
/// `layout.arrangement` saves, and a corner for each surround.
pub fn usable_saved_arrangement(name: &str, corners: &Value) -> bool {
    check_arrangement_name(name).is_ok() && corners.is_object() && parse_arrangement(corners, &serde_json::Map::new()).is_ok()
}

/// The arrangements saved by name as the preferences keep them (a JSON object of names and
/// corners), and the entries passed over, as they were: not an arrangement this version reads, a
/// name `layout.arrangement` wouldn't save, a name twice (case aside), or past
/// [`MAX_SAVED_ARRANGEMENTS`]. `None` for text that isn't such an object.
pub fn read_saved_arrangements(text: &str) -> Option<(serde_json::Map<String, Value>, serde_json::Map<String, Value>)> {
    let Ok(Value::Object(entries)) = serde_json::from_str::<Value>(text) else { return None };
    let (mut kept, mut passed) = (serde_json::Map::new(), serde_json::Map::new());
    for (name, corners) in entries {
        let usable =
            usable_saved_arrangement(&name, &corners) && !kept.keys().any(|k| same_name(k, &name)) && kept.len() < MAX_SAVED_ARRANGEMENTS;
        if usable {
            kept.insert(name, corners);
        } else {
            passed.insert(name, corners);
        }
    }
    Some((kept, passed))
}

/// The arrangements `layout.publication` tries for `"arrangement": asked` (#1379), each by the name
/// its answer gives it: for `"best"` (or none) the favourite first, so it wins a tie, then the
/// built-in ones and those saved by name, one of each set of corners.
pub(crate) fn arrangements_to_try(s: &Session, asked: &Value) -> Result<Vec<(String, Homes)>, String> {
    let saved = &s.layout_arrangements;
    let favourite = parse_arrangement(&s.layout_favourite, saved).unwrap_or(ARRANGEMENTS[0].1);
    let favourite = (arrangement_label(&s.layout_favourite, favourite, saved), favourite);
    Ok(match asked {
        Value::String(a) if a.trim().eq_ignore_ascii_case("favourite") => vec![favourite],
        Value::Null => arrangements_to_try(s, &json!("best"))?,
        Value::String(a) if a.trim().eq_ignore_ascii_case("best") => {
            let built_in = ARRANGEMENTS.iter().map(|(n, h)| ((*n).to_string(), *h));
            let mut all = vec![favourite];
            for (name, homes) in built_in.chain(saved_arrangements(saved).map(|(n, h)| (n.clone(), h))) {
                if !all.iter().any(|(_, h)| *h == homes) {
                    all.push((name, homes));
                }
            }
            all
        }
        v => {
            let homes = parse_arrangement(v, saved)?;
            vec![(arrangement_label(v, homes, saved), homes)]
        }
    })
}

/// Sets or shows the favourite arrangement of publication layouts (#1379): the one
/// `"arrangement": "favourite"` asks for, and the first `"best"` tries (it wins a tie). Saves
/// arrangements by name, which `"arrangement"` then takes as it takes the built-in ones and
/// `"best"` tries too, and removes them. All kept with the app's preferences, not the project.
/// A removal, then a save, then the favourite; checked before anything changes.
fn arrangement(s: &mut Session, p: &Value) -> CmdResult {
    let text = |key: &str, what: &str| match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) => Ok(Some(v.trim().to_string())),
        Some(_) => Err(format!("{key} is {what}")),
    };
    let (remove, save) = (text("remove", "the name of a saved arrangement")?, text("save", "a name for \"arrangement\"")?);
    // An arrangement given with nothing to save it under would do nothing.
    if save.is_none() && p.get("arrangement").is_some_and(|v| !v.is_null()) {
        return Err("\"arrangement\" is saved under a name given as \"save\"; the favourite is set with \"favourite\"".into());
    }
    let mut saved = s.layout_arrangements.clone();
    let mut warnings: Vec<String> = Vec::new();
    let mut favourite = s.layout_favourite.clone();
    if let Some(name) = remove {
        let key = saved.keys().find(|k| same_name(k, &name)).cloned().ok_or_else(|| format!("no arrangement is saved as \"{name}\""))?;
        let corners = saved.remove(&key).unwrap_or(Value::Null);
        // A favourite named after it keeps its corners, unless this call gives another.
        let replaced = p.get("favourite").is_some_and(|v| !v.is_null());
        if !replaced && favourite.as_str().is_some_and(|f| same_name(f.trim(), &key)) {
            favourite = corners;
            warnings.push(format!("the favourite was the arrangement \"{key}\": it keeps its corners"));
        }
    }
    if let Some(name) = save {
        let homes = match p.get("arrangement") {
            None | Some(Value::Null) => return Err("save takes \"arrangement\": a name, or a corner for each surround".into()),
            Some(v) => parse_arrangement(v, &saved)?,
        };
        check_arrangement_name(&name)?;
        match saved.keys().find(|k| same_name(k, &name)).cloned() {
            Some(old) => {
                saved.remove(&old);
                warnings.push(format!("the arrangement saved as \"{old}\" is replaced"));
            }
            None if saved.len() >= MAX_SAVED_ARRANGEMENTS => {
                return Err(format!("at most {MAX_SAVED_ARRANGEMENTS} arrangements are saved: remove one first"));
            }
            None => {}
        }
        saved.insert(name, corners_of(homes));
    }
    if let Some(v) = p.get("favourite").filter(|v| !v.is_null()) {
        parse_arrangement(v, &saved)?;
        favourite = v.clone();
    }
    let homes = parse_arrangement(&favourite, &saved)?;
    (s.layout_arrangements, s.layout_favourite) = (saved, favourite);
    let names: Vec<&str> =
        ARRANGEMENTS.iter().map(|(n, _)| *n).chain(saved_arrangements(&s.layout_arrangements).map(|(n, _)| n.as_str())).collect();
    Ok(
        json!({"favourite": s.layout_favourite, "corners": corners_of(homes), "arrangements": names, "saved": s.layout_arrangements, "warnings": warnings}),
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
///
/// It runs on the UI thread, with no Cancel, as it did before the veil: none of the background
/// paths (tools, exports, commands that make a raster, models and scripts on a copy) takes a
/// command that adds a layout, and a locator map, to the project yet. Its cost grows with the
/// map's features: placing the surrounds draws the frame once on a grid of 384 cells, and when no
/// shape of the legend fits, the search draws it at most four times more, once per scale it tries,
/// each time on a view wider than the frame that a streamed layer is read for, waited for here,
/// and where the projection stretches, up to four moved frames per scale on their own (in Web
/// Mercator, the two moved up and down)
/// (#1379; the whole command took about 0.7 s on a 14 MB geology map, 0.2 s before the search, and
/// 0.9 s with its layers read in place).
fn publication(s: &mut Session, p: &Value) -> CmdResult {
    let (auto, profile) = publication_profile(p)?;
    let (sheet, page_w, page_h) = (profile.sheet, profile.width, profile.height);
    let whole_globe = shows_whole_globe(s.map(), p);
    let extent = match p.get("extent").filter(|v| !v.is_null()) {
        // The view; on a globe the whole Earth, as geoprocessing takes it (the globe camera's flat
        // extent means nothing there).
        None if whole_globe => Extent::new(-180.0, -90.0, 180.0, 90.0),
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
    let locator = publication_locator(p, sheet, whole_globe)?;
    let colorbar = publication_colorbar(p)?;
    let may_veil = publication_veil(p)?;
    let context_in_legend = publication_flag(p, "contextInLegend")?.unwrap_or(false);
    let north_arrow = publication_north_arrow(p)?;
    let legend_box_always = publication_legend_box(p)?;
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
    let tries = arrangements_to_try(s, p.get("arrangement").unwrap_or(&Value::Null))?;
    // A translucent accent would leave a band, a poster's page or the headings faint or unseen.
    if accent.a() != 255 {
        return Err("accent is an opaque colour as #RRGGBB".into());
    }
    // Where the coverage grid the placement searches is written as an image (#1379).
    let coverage_path = match p.get("coverage") {
        None | Some(Value::Null) => None,
        Some(Value::String(path)) if path.trim().to_ascii_lowercase().ends_with(".png") => Some(path.trim().to_string()),
        Some(_) => return Err("coverage is the path of a .png file to write the coverage grid to".into()),
    };
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
    // A sheet's default locator would show the globe again, smaller; one asked for still comes,
    // and "none" asked for is no news.
    if sheet && whole_globe && p.get("locator").is_none_or(Value::is_null) {
        warnings.push("no locator: the map shows the whole globe; \"locator\": \"globe\" or \"inset\" adds one".into());
    }
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
    // The sources and the reference that have something in them, each its heading in `head_pt` and
    // lines in `body_pt`, `tw` wide. Each one's elements keep their names (`Sources`, `Reference`)
    // whatever its heading reads, the heading filled and wrapped like the rest.
    let sections = |tw: f64, head_pt: f64, body_pt: f64| -> Vec<(&'static str, Vec<String>, Vec<String>)> {
        [("Sources", heading(0, "Sources"), &sources), ("Reference", heading(1, "Cartographic reference"), &reference)]
            .into_iter()
            .filter(|(.., texts)| !texts.is_empty())
            .map(|(name, head, texts)| {
                // Filled first, so `{projection}` reads in capitals too; the live tokens left keep
                // their case (wrap_as fills again, which then finds nothing).
                let head = wrap_as(&capitals(&fill(&head)), tw, head_pt, true);
                (name, head, texts.iter().flat_map(|t| wrap(t, tw, body_pt)).collect::<Vec<_>>())
            })
            .collect()
    };
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
                    let columns = sections(tw, head_pt, body_pt);
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
                                        dash: None,
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
                                ElementKind::Line { color: accent, width: 1.0, dir: LineDir::Horizontal, head: LineHead::None, dash: None },
                            ));
                            y += 4.0 * k;
                        }
                        // The sources and the reference, stacked up from the panel's foot.
                        let mut yb = panel_bottom - ip;
                        for (name, head, lines) in sections(tw, head_pt, body_pt).into_iter().rev().filter(|_| !under) {
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
                            ElementKind::Line { color: accent, width: 1.5, dir: LineDir::Horizontal, head: LineHead::None, dash: None },
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
    let (inner_w, inner_h) = ((fw - strip_w).max(fw / 2.0), (fh - strip_h).max(fh / 2.0));
    let (center, scale) = if whole_globe {
        // The globe seen whole fills the frame, centred where the view looks (#2337): fitting the
        // whole Earth's flat footprint instead drew it a fraction of the frame, around 0°, 0°.
        // Its size at a scale is the same at any latitude, so it is worked out on the equator,
        // where the probe stays within the camera's resolutions.
        let view = s.map().center_lonlat();
        let scale = round_scale(globe_scale(s.map(), Coord::new(view.x, 0.0), inner_w.min(inner_h) * WHOLE_GLOBE_FILL));
        let center = drawable_globe_center(s.map(), view, scale);
        if center != view {
            warnings.push(format!(
                "the globe is centred at {:.1}° rather than the view's {:.1}° of latitude: nearer the pole, the frame can't draw it small enough to fit",
                center.y, view.y
            ));
        }
        (center, scale)
    } else {
        fit_scale(s.map(), &extent, inner_w, inner_h).ok_or("the extent can't be drawn on the map's projection")?
    };
    let scale = round_scale(scale);
    let grid = Grid { step, edges, ..Grid::default() };
    // A name given past the limit is refused, as layout.new refuses it, not cut (#2639).
    let given = super::within("name", "a layout's name", ps(p, "name").unwrap_or("Publication").trim(), super::MAX_NAME)?;
    let (name, name_cut) = unique_name(s, "New Publication Layout", "the name given", &given);
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
            border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
            border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
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
    let mut locator_cut = None;
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
                None => {
                    let name = numbered(&base, |n| s.project.maps.iter().any(|x| x.name.eq_ignore_ascii_case(n)));
                    locator_cut = super::name_cut("New Publication Layout", "the locator map's name", &base, &name);
                    name
                }
            };
            // A new map of what the globe draws, not a clone of the whole map (every layer's
            // features): its basemap (the land and borders around the area), graticule and body,
            // and the map's own polygon layers that show, unlabelled: the land at a glance, no
            // raster or streamed layer (read again) nor points and lines cluttering a small disc.
            // Not its web tiles (fetched and drawn over the basemap on the disc), swipe, time
            // slider, layer groups (their hidden layers left out here), bookmarks or animation.
            let mut m = rustgis_map::Map::new(name);
            (m.basemap, m.paper) = (src.basemap, src.paper);
            m.graticule = src.graticule;
            m.body = src.body.clone();
            m.set_projection(rustgis_map::Projection::Globe);
            // Sized on the equator and centred as near the area as the camera draws a globe that
            // small, as the whole globe's frame is (#2337): at the area's own latitude near a pole
            // the camera stops at its coarsest resolution, and the disc came out larger than its
            // circle, clipped. Further from the pole, the area is still on the disc and outlined.
            let globe = globe_scale(&m, Coord::new(center.x, 0.0), side * 0.92);
            let (at, whole) = globe_locator_center(&m, center, globe, side);
            if at != center {
                warnings.push(format!(
                    "the globe locator is centred at {:.1}° rather than the map frame's {:.1}° of latitude: nearer the pole, it can't draw the globe small enough to fit",
                    at.y, center.y
                ));
            }
            if !whole {
                warnings.push("the globe locator is too small to show the whole globe: its circle clips it".into());
            }
            // Each polygon simplified to a tenth of a millimetre on the disc (its scale in metres),
            // so the project keeps a sketch of the layers for it, not a second full copy of them.
            let tolerance_m = globe * 1e-4;
            // What Simplify left alone or changed beyond simplifying in what the sketch keeps (a
            // latitude past a pole taken as the pole, a polygon left as it was): said in the
            // warnings, one per kind (#1811).
            let notes = std::cell::Cell::new(rustgis_analysis::SimplifyNotes::default());
            // On the locator, sub-tolerance rings keep their outline rather than becoming triangles.
            let sketch = |g: &rustgis_data::Geometry| {
                let (mut simple, mut said) = rustgis_analysis::simplify_geometry(g, tolerance_m);
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
                            // The ring as it was: its vertices past a pole stay where they were.
                            said.past_pole = said.past_pole.saturating_sub(rustgis_analysis::past_pole(original.iter()));
                            *reduced = original.clone();
                        }
                    }
                }
                match &simple {
                    rustgis_data::Geometry::Polygon(polys) if polys.iter().flatten().all(|ring| ring.len() >= 4) => {
                        let mut all = notes.get();
                        all.add(&said);
                        notes.set(all);
                        simple
                    }
                    // The polygon as it was: nothing Simplify did is kept, but one it left as it was
                    // (a missing coordinate) is still said.
                    _ => {
                        let mut all = notes.get();
                        all.add(&rustgis_analysis::SimplifyNotes { broken: said.broken, capped: said.capped, ..Default::default() });
                        notes.set(all);
                        g.clone()
                    }
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
                    layer.synced = None;
                    // Sketched in longitude/latitude, whatever system the layer holds its
                    // coordinates in.
                    let lonlat = l.dataset.in_lonlat()?;
                    layer.dataset.held_in = rustgis_data::raster::Crs::LonLat;
                    layer.dataset.features = lonlat
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
                    Ok(layer)
                })
                .collect::<Result<_, String>>()?;
            // All but reaching beyond 85.05°, which any world map does (`without_polar`).
            warnings.extend(notes.get().without_polar().messages("the globe locator, simplifying its polygons"));
            let name = m.name.clone();
            match orphan.and_then(|i| s.project.maps.get_mut(i)) {
                Some(old) => {
                    *old = m;
                    warnings.push(format!("the map '{name}', which no layout showed, is replaced by this sheet's globe"));
                }
                None => s.project.maps.push(m),
            }
            Some((name, globe, FrameShape::Circle, at))
        }
        Locator::Inset => Some((s.map().name.clone(), round_scale(scale * fw / side * 4.0), FrameShape::Rectangle, center)),
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
    // Context around the data (neighbouring borders, the states around in one colour), which the
    // placement already sets aside, is left out of the legend too, unless "contextInLegend" (#1379).
    // It is judged again at each other scale the search below draws the map at, as the placement
    // judges it there, and the legend takes the one at the scale settled on.
    let context_at = |s: &Session, center: Coord, scale: f64| {
        if context_in_legend { Vec::new() } else { legend_context(s.map(), center, scale, (fw, fh)) }
    };
    let mut context = context_at(s, center, scale);
    let mut sizes = crate::render::layout::legend_sizes_mm(s.map_mut(), render_scale, false, false, (&context, &Default::default()));
    let (mut cw, mut ch) = match sizes {
        Some(sizes) => sizes[0],
        None => {
            // A complete source that can't be read: the export says why; the legend is sized by hand.
            warnings.push("a layer's source couldn't be read through, so the legend has a fixed size; exporting says why".into());
            (fw * 0.3, fh * 0.25)
        }
    };
    let listed = s.map().layers.iter().any(|l| s.map().shows(l) && crate::render::layout::legend_lists(l));
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
    let (sh, nh) = (if sheet { 8.0 * k } else { 6.0 }, pitch(small_pt) + 0.5);
    // The scale bar's box at a scale: the size of its bar and labels and of the scale under them,
    // with the padding (#1379). The bar is the one a box 30 % of the frame's width (25 mm at least)
    // would draw, which sets its length and divisions. The box is the narrowest that draws that same
    // bar: the renderer gives a bar at most 85 % of its box, so the box is at least the bar's length
    // over 0.85.
    let bar_kind = match ElementKind::scale_bar(frame) {
        ElementKind::ScaleBar { units, divisions, .. } => Some((units, divisions)),
        _ => None,
    };
    let scale_box = |ground_per_pt: f64, scale: f64| {
        let text = ("{scale}", small_pt, false);
        let w = bar_kind.map_or((fw * 0.3).max(25.0), |bar| scale_box_width(ground_per_pt, scale, fw, bar, comma, text));
        (w, sh + nh)
    };
    let ground_per_pt = |s: &Session, center: Coord, scale: f64| {
        s.map().ground_res_at(&rustgis_map::layout::frame_camera(s.map(), center, scale, 0.0, 0.0))
    };
    let pad = 2.0;
    // The surrounds to place: what each is and its size (the arrangement gives its corner).
    let mut surrounds: Vec<(Surround, f64, f64)> = Vec::new();
    if locator_map.is_some() && column.is_none() {
        surrounds.push((Surround::Locator, side, side));
    }
    // At a continental span the graticule's lines give north, and one arrow can't point north
    // across the whole frame: none, unless "northArrow": "always" (#1379). Left out so, it is said;
    // "northArrow": "none" asks for it, so isn't.
    let arrow = north_arrow.unwrap_or(extent_span_deg(&extent) < CONTINENTAL_DEG);
    if arrow {
        surrounds.push((Surround::Arrow, aw, ah));
    } else if north_arrow.is_none() {
        warnings.push(format!(
            "no north arrow: the extent spans {CONTINENTAL_DEG:.0}° or more, where the graticule gives north; \"northArrow\": \"always\" draws one"
        ));
    }
    // The topmost visible raster with a colour scale that the legend doesn't list by its classes
    // gets a colorbar, under the legend in its box (a map whose theme is a raster on a ramp has its
    // key there, which the legend's rows don't give): an RGB composite over a DEM leaves it to the
    // DEM. A raster drawn by classes has them in the legend's rows instead (#1679). `"colorbar":
    // "none"` leaves it out, and its room in the box with it (#1680).
    // One on top whose pixels aren't read yet has no scale to show: no colorbar, rather than the
    // scale of a raster under it under its name.
    let top_raster = colorbar.then(|| rustgis_map::layout::colorbar_layer(s.map(), None)).flatten();
    let scaled = |l: &&rustgis_map::Layer| l.raster.as_ref().is_some_and(|r| rustgis_map::layout::color_scale(r).is_some());
    let bar = top_raster.filter(scaled).map(|l| l.name.clone());
    if let Some(l) = top_raster.filter(|l| !scaled(l)) {
        let why = if l.raster.as_ref().is_some_and(|r| r.error.is_some()) { "couldn't be read" } else { "is still reading" };
        warnings.push(format!(
            "{} {why}, so its colour scale isn't known and the layout has no colorbar for it; add one with layout.addElement once it has read",
            l.name
        ));
    }
    // A colour table whose values can't be known (too many to list) gives the legend no class, and
    // the sheet no key for it; classes from an overview may miss a rare one: said, not left out.
    warnings.extend(raster_key_notices(s.map(), &[], false));
    // A page of cards gives the locator in its column a card of the column's width, and the legend
    // under it one as wide.
    let card = style == Style::Modular && column.is_some();
    let (card_r, card_pad) = (3.0 * k, 2.5 * k);
    // The colorbar's title is the raster's name, over the bar. The box is as wide as the title, up
    // to the legend's cap (in a side column, the column's width); a longer title wraps there, each
    // line past the first making the colorbar taller (#2493).
    let title_room = match column {
        Some(c) => c.w + if card { 2.0 * card_pad } else { 0.0 } - 2.0 * pad,
        None => fw * 0.45 - 2.0 * pad,
    };
    let base_w = if sheet { 50.0 * k } else { 40.0 };
    let bar_w = base_w.max(bar.as_deref().map_or(0.0, crate::render::layout::colorbar_title_width_mm).min(title_room));
    let bar_h = bar.as_deref().map_or(COLORBAR_MM, |title| colorbar_mm(title, bar_w.min(title_room)));
    let bar_part = if bar.is_some() { bar_h + if ch > 0.0 { 2.0 } else { 0.0 } } else { 0.0 };
    let (content_w, content_h) = (if bar.is_some() { cw.max(bar_w) } else { cw }, ch + bar_part);
    let legend_in_column = column.is_some();
    if content_h > 0.0 && !legend_in_column {
        surrounds.push((Surround::Legend, (content_w + 2.0 * pad).min(fw * 0.45), (content_h + 2.0 * pad).min(fh * 0.6)));
    }
    // Its width is set once the frame's centre and scale are settled, below; nothing reads it before.
    // A globe seen whole has none: its scale holds at its centre only, falling to nothing at its
    // rim, so a bar would read true nowhere else (#2337).
    if whole_globe {
        warnings.push("no scale bar: the map shows the whole globe, whose scale holds at its centre only".into());
    } else {
        surrounds.push((Surround::Scale, 0.0, sh + nh));
    }
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
    // What the map draws where, on a grid of the frame (its basemap left out). The map
    // itself is drawn for it, on purpose, as the screen draws it: a grid of 384 cells, a small part
    // of what exporting the sheet draws; a streamed layer draws what it holds and starts reading
    // the frame's area in the background (which the export reads anyway); web tiles aren't fetched.
    let coverage = crate::render::layout::frame_coverage(s.map_mut(), center, scale, fw, fh, 384);
    // The legend's whole box, around its rows and the colorbar under them.
    let whole = |(w, h): (f64, f64)| ((if bar.is_some() { w.max(bar_w) } else { w }) + 2.0 * pad, h + bar_part + 2.0 * pad);
    // The largest rectangle a coverage leaves free inside the graticule's margins, in mm.
    let free_room = |(gw, gh, cells): &(u32, u32, Vec<bool>)| {
        let (gw, gh) = (*gw as usize, *gh as usize);
        let col = |v: f64| ((v - fx) / fw * gw as f64).clamp(0.0, gw as f64) as usize;
        let row = |v: f64| ((v - fy) / fh * gh as f64).clamp(0.0, gh as f64) as usize;
        let (cw, ch) = largest_free_cells(gw, cells, (col(area.0), row(area.1), col(area.2), row(area.3)));
        (cw as f64 * fw / gw as f64, ch as f64 * fh / gh as f64)
    };
    // When no shape of the legend fits the room the map leaves, the map moves towards an edge, so
    // the room on both its sides joins, by part or all of the way the asked extent can go and stay
    // in the frame: of the moves that make room, the one keeping it nearest the page's visual
    // centre. Failing
    // that, on a continuous map (drawn over most of the extent and running on past it) with a long
    // legend (a short one sits over the map as well as ever), what is outside the extent is veiled,
    // and counts as room; and failing that, the map is drawn at up to half the scale, for a band of
    // it. Failing all those, it stays as it was (#1379).
    let (mut center, mut scale, mut coverage, mut map_shift) = (center, scale, coverage, (0.0, 0.0));
    let mut veiled: Option<Rect> = None;
    // The bands of the veil around an extent's place on the page, the widest first, short of the
    // extent by the gap the surrounds keep in a group.
    const GROUP_GAP: f64 = 3.0;
    let bands_around = |clip: Rect| {
        let (clip_right, clip_below) = (clip.0 + clip.2 + GROUP_GAP, clip.1 + clip.3 + GROUP_GAP);
        let mut bands = [
            (area.0, area.1, clip.0 - GROUP_GAP - area.0, area.3 - area.1),
            (clip_right, area.1, area.2 - clip_right, area.3 - area.1),
            (area.0, area.1, area.2 - area.0, clip.1 - GROUP_GAP - area.1),
            (area.0, clip_below, area.2 - area.0, area.3 - clip_below),
        ];
        bands.sort_by(|a, b| (b.2 * b.3).total_cmp(&(a.2 * a.3)));
        bands
    };
    let size_of = |kind: Surround| surrounds.iter().find(|x| x.0 == kind).map(|x| (x.1, x.2));
    let (locator_side, arrow_size) = (size_of(Surround::Locator).map(|x| x.0), size_of(Surround::Arrow));
    // The fewest columns from `from` up in which the surrounds group in a band around `clip`, with
    // their places; the scale bar's box `scale_bar` is the one at the scale tried.
    let grouped = |clip: Rect, from: usize, known: &[(f64, f64); MAX_LEGEND_COLUMNS], scale_bar: (f64, f64)| {
        (from..known.len()).find_map(|i| {
            bands_around(clip)
                .iter()
                .find_map(|band| veil_panel(*band, whole(known[i]), locator_side, arrow_size, scale_bar, GROUP_GAP))
                .map(|places| (i, places))
        })
    };
    if let Some(known) = sizes.filter(|_| column.is_none() && content_h > 0.0) {
        let need = |room: (f64, f64)| known.iter().position(|size| whole(*size).0 <= room.0 && whole(*size).1 <= room.1);
        let flat = s.map().flat();
        let asked = projected_extent(s, &extent);
        let at = flat.forward_checked(center).filter(|_| !asked.is_empty());
        if let Some(at) = at.filter(|_| coverage.as_ref().is_some_and(|c| need(free_room(c)).is_none())) {
            let (centred, asked_scale) = (center, scale);
            let continuous = coverage.as_ref().is_some_and(|(gw, gh, cells)| {
                let mm = 1000.0 * flat.ground_per_unit(asked.center()) * s.map().body().ratio() / asked_scale;
                let (half_w, half_h) = (asked.width() * mm / 2.0, asked.height() * mm / 2.0);
                let (gw, gh) = (*gw as usize, *gh as usize);
                let (mut inside, mut inside_drawn, mut outside, mut outside_drawn) = (0usize, 0usize, 0usize, 0usize);
                for (i, drawn) in cells.iter().enumerate() {
                    let x = ((i % gw) as f64 + 0.5) / gw as f64 * fw - fw / 2.0;
                    let y = ((i / gw) as f64 + 0.5) / gh as f64 * fh - fh / 2.0;
                    if x.abs() <= half_w && y.abs() <= half_h {
                        inside += 1;
                        inside_drawn += usize::from(*drawn);
                    } else {
                        outside += 1;
                        outside_drawn += usize::from(*drawn);
                    }
                }
                whole(known[0]).1 > fh * 0.25
                    && inside > 0
                    && inside_drawn * 5 >= inside * 4
                    && (outside == 0 || outside_drawn * 10 >= outside * 3)
            });
            // `"veil": false` keeps the map unveiled, and so at its scale.
            let continuous = continuous && may_veil;
            let passes = [(1.0, false), (1.0, true), (1.25, true), (1.5, true), (2.0, true)];
            // Each scale's frame is drawn once, wide enough for every move, and each move's frame
            // cut from it: a move is a translation on the page. So the search draws the map once per
            // scale it tries (at most four times), not once per place. Its legend is measured at
            // that scale, once, and those sizes are the ones laid out.
            type Drawn = (f64, Option<(u32, u32, Vec<bool>)>, Option<[(f64, f64); MAX_LEGEND_COLUMNS]>, Vec<String>);
            let mut drawn: Option<Drawn> = None;
            let (mut chosen, mut chosen_context) = (known, context.clone());
            let mut unknown_at: Option<f64> = None;
            // Moved frames drawn on their own (below), by scale and move: a pass with the veil
            // tries the same moves as the one before it.
            // With the legend's context judged there, and its sizes with that context.
            type Own = ((f64, f64, f64), (u32, u32, Vec<bool>), Vec<String>, [(f64, f64); MAX_LEGEND_COLUMNS]);
            let mut own_drawn: Vec<Own> = Vec::new();
            // A figure keeps its scale: its size is fixed, and a smaller scale costs it the most.
            for (factor, veil) in passes.into_iter().filter(|(factor, veil)| (!*veil || continuous) && (*factor == 1.0 || sheet)) {
                let tried = if factor == 1.0 { asked_scale } else { round_scale(asked_scale * factor) };
                let mm = 1000.0 * flat.ground_per_unit(asked.center()) * s.map().body().ratio() / tried;
                // How far the extent, centred, can go each way and stay inside that side's margin
                // (a figure labels only its left and bottom edges, so they differ).
                let (half_w, half_h) = (asked.width() * mm / 2.0, asked.height() * mm / 2.0);
                let to = |margin: f64, half_frame: f64, half_extent: f64| (half_frame - margin - half_extent).max(0.0);
                let (to_left, to_right) = (to(left, fw / 2.0, half_w), to(right, fw / 2.0, half_w));
                let (to_top, to_bottom) = (to(top, fh / 2.0, half_h), to(bottom, fh / 2.0, half_h));
                // The wide drawing is centred as the frame is, so it reaches the farther way on both.
                let (slack_x, slack_y) = (to_left.max(to_right), to_top.max(to_bottom));
                let wide = (fw + 2.0 * slack_x, fh + 2.0 * slack_y);
                if drawn.as_ref().is_none_or(|d| d.0 != tried) {
                    // A streamed layer has its features read for the wide view, at the asked scale
                    // too (the frame's read reaches past the frame by half its size in degrees,
                    // which the wide view outgrows where the projection stretches, as Web Mercator
                    // does far from the equator), and a smaller scale can draw more classes. While
                    // one is still reading there, or a legend picture still loading, what the map
                    // draws there isn't known: that scale isn't tried, which is said.
                    let render_scale = crate::render::layout::frame_render_scale(s.map(), centred, tried);
                    let view = crate::render::View {
                        width: wide.0 * rustgis_map::layout::PT_PER_MM,
                        height: wide.1 * rustgis_map::layout::PT_PER_MM,
                        pixels_per_point: 1.0,
                        camera: rustgis_map::layout::frame_camera(s.map(), centred, tried, 0.0, 0.0),
                        sky: false,
                    };
                    let lonlat = crate::render::view_lonlat(s.map(), &view);
                    let reading = s.map_mut().sync_sources(&lonlat, render_scale, true);
                    let here_context = if tried == asked_scale { context.clone() } else { context_at(s, centred, tried) };
                    let here =
                        if reading || (tried != asked_scale && crate::render::layout::tile_legends_pending(s.map(), render_scale, &[])) {
                            unknown_at.get_or_insert(tried);
                            None
                        } else if tried == asked_scale {
                            Some(known)
                        } else {
                            // A legend that can't be measured there (a source that can't be read
                            // through) leaves that scale untried too, which is said.
                            let measured = crate::render::layout::legend_sizes_mm(
                                s.map_mut(),
                                render_scale,
                                false,
                                false,
                                (&here_context, &Default::default()),
                            );
                            if measured.is_none() {
                                unknown_at.get_or_insert(tried);
                            }
                            measured
                        };
                    let cells = (384.0 * wide.0.max(wide.1) / fw.max(fh)).round() as u32;
                    // What is context is judged on the frame asked for, not the wide drawing, so
                    // every move's cut leaves out the same layers.
                    let whole_frame = here
                        .and_then(|_| crate::render::layout::frame_coverage_judged_on(s.map_mut(), centred, tried, wide, (fw, fh), cells));
                    drawn = Some((tried, whole_frame, here, here_context));
                }
                let Some((_, Some(whole_frame), Some(here), here_context)) = &drawn else { continue };
                let need = |sizes: &[(f64, f64); MAX_LEGEND_COLUMNS], room: (f64, f64)| {
                    sizes.iter().position(|size| whole(*size).0 <= room.0 && whole(*size).1 <= room.1)
                };
                // Of the places with the fewest columns, the one leaving the extent nearest the
                // page's visual centre (a little above its middle) is kept, a move away from that
                // centre as short as makes room, one towards it as far as stays nearest it; within a
                // millimetre of the nearest, the one with the most room (#1379).
                // Each place that fits: its columns, its distance from that centre, its room, and the
                // map's centre, coverage, move, veil and legend there.
                type Fit = (
                    usize,
                    f64,
                    f64,
                    Coord,
                    (u32, u32, Vec<bool>),
                    (f64, f64),
                    Option<Rect>,
                    [(f64, f64); MAX_LEGEND_COLUMNS],
                    Vec<String>,
                );
                let mut fits: Vec<Fit> = Vec::new();
                // Each way a quarter, half, three quarters and all of the way the extent can go, so
                // a move can stop short of the edge. A way with no room to go is left out (its moves
                // are the centred place, tried first).
                let moves = [(to_right, 0.0), (-to_left, 0.0), (0.0, to_bottom), (0.0, -to_top)]
                    .into_iter()
                    .filter(|&(x, y)| x != 0.0 || y != 0.0)
                    .flat_map(|(x, y)| [0.25, 0.5, 0.75, 1.0].map(|f| (x * f, y * f)));
                for (dx, dy) in std::iter::once((0.0, 0.0)).chain(moves) {
                    // Centred with no veil at the asked scale is what has no room; a shift of nothing
                    // is centred.
                    let still = dx == 0.0 && dy == 0.0;
                    if (still && !veil) || (!still && dx.abs() + dy.abs() < 1.0) {
                        continue;
                    }
                    let (cx, cy) = (at.x - dx / mm, at.y + dy / mm);
                    let there = if still { Some(centred) } else { flat.inverse(Coord::new(cx, cy)) };
                    let Some(there) = there else { continue };
                    // A frame is drawn at its own centre's ground scale, which a move changes where
                    // the projection stretches (Web Mercator's, with the latitude; a conic's, a
                    // little): the extent is placed on the page at the moved centre's scale, and a
                    // move that leaves it out of its margins there isn't made. The frame moved east
                    // by `dx` and south by `dy` is the wide one's part that far west and north of
                    // its middle while that scale moves a corner of it one cell of the coverage grid
                    // at most, which the cut is exact to anyway; past that, it is drawn on its own
                    // (what is context then judged on itself, for the legend too), once.
                    let here_mm = mm * flat.ground_per_unit(Coord::new(cx, cy)) / flat.ground_per_unit(asked.center());
                    let clip = (
                        fx + fw / 2.0 + (asked.min_x - cx) * here_mm,
                        fy + fh / 2.0 - (asked.max_y - cy) * here_mm,
                        asked.width() * here_mm,
                        asked.height() * here_mm,
                    );
                    let kept_whole = clip.0 >= fx + left - 0.5
                        && clip.1 >= fy + top - 0.5
                        && clip.0 + clip.2 <= fx + fw - right + 0.5
                        && clip.1 + clip.3 <= fy + fh - bottom + 0.5;
                    if !still && !kept_whole {
                        continue;
                    }
                    let drift = (1.0 - flat.ground_per_unit(Coord::new(cx, cy)) / flat.ground_per_unit(at)).abs() * fw.hypot(fh) / 2.0;
                    let (mut moved, legend_sizes, legend_context) = if drift <= fw.max(fh) / 384.0 {
                        (crop_cells(whole_frame, wide, (slack_x - dx, slack_y - dy), (fw, fh)), *here, here_context.clone())
                    } else if let Some((_, own, own_context, own_sizes)) = own_drawn.iter().find(|o| o.0 == (tried, dx, dy)) {
                        (own.clone(), *own_sizes, own_context.clone())
                    } else {
                        let Some(own) = crate::render::layout::frame_coverage(s.map_mut(), there, tried, fw, fh, 384) else { continue };
                        let own_context = context_at(s, there, tried);
                        let own_sizes = if own_context == *here_context {
                            *here
                        } else {
                            let at_there = crate::render::layout::frame_render_scale(s.map(), there, tried);
                            let measured = crate::render::layout::legend_sizes_mm(
                                s.map_mut(),
                                at_there,
                                false,
                                false,
                                (&own_context, &Default::default()),
                            );
                            // A legend that can't be measured there leaves that move untried, which is
                            // said, as at a scale.
                            let Some(measured) = measured else {
                                unknown_at.get_or_insert(tried);
                                continue;
                            };
                            measured
                        };
                        own_drawn.push(((tried, dx, dy), own.clone(), own_context.clone(), own_sizes));
                        (own, own_sizes, own_context)
                    };
                    if veil {
                        let (gw, gh) = (moved.0 as usize, moved.1 as usize);
                        for (i, cell) in moved.2.iter_mut().enumerate() {
                            let x = fx + ((i % gw) as f64 + 0.5) / gw as f64 * fw;
                            let y = fy + ((i / gw) as f64 + 0.5) / gh as f64 * fh;
                            *cell &= x >= clip.0 && x <= clip.0 + clip.2 && y >= clip.1 && y <= clip.1 + clip.3;
                        }
                    }
                    let room = free_room(&moved);
                    let Some(n) = need(&legend_sizes, room) else { continue };
                    // Under a veil the whole group has to fit, not the legend alone, the scale bar's box
                    // as drawn there.
                    let in_group = |s: &Session| grouped(clip, n, &legend_sizes, scale_box(ground_per_pt(s, there, tried), tried));
                    let Some(n) = (if veil { in_group(s).map(|(i, _)| i) } else { Some(n) }) else { continue };
                    let off = visual_centre_offset((clip.0 + clip.2 / 2.0, clip.1 + clip.3 / 2.0), (w, h));
                    fits.push((n, off, room.0 * room.1, there, moved, (dx, dy), veil.then_some(clip), legend_sizes, legend_context));
                }
                // The fewest columns; of those, the nearest that centre, and within a millimetre of
                // the nearest, the most room (the first tried on a tie).
                let Some(fewest) = fits.iter().map(|f| f.0).min() else { continue };
                let nearest = fits.iter().filter(|f| f.0 == fewest).map(|f| f.1).fold(f64::INFINITY, f64::min);
                let best = fits
                    .into_iter()
                    .filter(|f| f.0 == fewest && f.1 <= nearest + 1.0)
                    .reduce(|kept, f| if f.2 > kept.2 { f } else { kept });
                if let Some((_, _, _, there, moved, shift, clip, legend_sizes, legend_context)) = best {
                    (center, scale, coverage, map_shift) = (there, tried, Some(moved), shift);
                    veiled = clip;
                    (chosen, chosen_context) = (legend_sizes, legend_context);
                    break;
                }
            }
            if let Some(at_scale) = unknown_at {
                let or_veiled = if may_veil { " or veiled" } else { "" };
                warnings.push(format!(
                    "the map isn't moved{or_veiled} at 1:{at_scale:.0} for the legend's room: a layer there is still reading, a legend picture still loading, or a layer's source can't be read through (exporting says why)"
                ));
            }
            // A move changes what the frame shows past the extent, so it is said, as the veil and
            // the scale are.
            if map_shift != (0.0, 0.0) {
                let (dx, dy) = map_shift;
                let (by, way) =
                    if dx != 0.0 { (dx, if dx > 0.0 { "right" } else { "left" }) } else { (dy, if dy > 0.0 { "down" } else { "up" }) };
                warnings.push(format!(
                    "the map is moved {:.0} mm {way} in its frame (\"mapShift\"), the extent still whole, for the legend's room",
                    by.abs()
                ));
            }
            if scale != asked_scale {
                warnings.push(format!("the map is drawn at 1:{scale:.0}, not 1:{asked_scale:.0}, to leave the legend room"));
            }
            // The legend as measured where the map settled: at another scale, or moved to where
            // the context is judged apart.
            if scale != asked_scale || chosen_context != context {
                (sizes, context) = (Some(chosen), chosen_context);
                (cw, ch) = chosen[0];
                let (w, h) = whole(chosen[0]);
                if let Some(entry) = surrounds.iter_mut().find(|x| x.0 == Surround::Legend) {
                    (entry.1, entry.2) = (w.min(fw * 0.45), h.min(fh * 0.6));
                }
            }
        }
    }
    if !context.is_empty() {
        warnings.push(format!(
            "the legend leaves out {} context layer{} around the data ({}): \"contextInLegend\": true lists {}",
            context.len(),
            if context.len() == 1 { "" } else { "s" },
            context.join(", "),
            if context.len() == 1 { "it" } else { "them" }
        ));
    }
    // The scale bar's box as the frame is drawn, moved or at another scale or not.
    let bar_box = scale_box(ground_per_pt(s, center, scale), scale);
    if let Some(entry) = surrounds.iter_mut().find(|x| x.0 == Surround::Scale) {
        (entry.1, entry.2) = bar_box;
    }
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
        // A legend cut short of its neighbours loses rows, which costs half the frame, more than any
        // move, and ten times what it loses, so an arrangement that keeps them wins, also by moving
        // the legend itself; a rounding error costs nothing.
        let lost = legend_at.map_or(0.0, |(_, lw, lh)| legend.0 * legend.1 - lw * lh);
        let cut = if lost > 1.0 { fw * fh / 2.0 + 10.0 * lost } else { 0.0 };
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
        let mut laid: Vec<(Homes, Laid)> = tries.iter().map(|(_, h)| (*h, lay_out(*h, legend))).collect();
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
        let shown = |laid: &Laid, (w, h): (f64, f64)| laid.0.map_or(0.0, |(_, lw, lh)| (lw / w).min(1.0) * (lh / h).min(1.0));
        let long = whole(sizes[0]).1 > fh * 0.25;
        let mut score = (shown(&best.1, whole(sizes[0])), best.1.3.1);
        let wide = fw - left - right - 2.0 - arrow_size.map_or(0.0, |(w, _)| w);
        let free = coverage.as_ref().map(free_room);
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
    let (homes, (_, mut spots, mut moved, _)) = best;
    // A veiled map has its surrounds grouped in the widest band of the veil that takes them, in the
    // fewest columns that do, rather than each in a corner of its own. The search found that group
    // with these very sizes, so it is found again; the legend goes there too when the corners left
    // it no room.
    let mut arrangement_used =
        tries.iter().find(|(_, h)| *h == homes).map_or_else(|| arrangement_name(homes).to_string(), |(n, _)| n.clone());
    if let (Some(clip), Some(known)) = (veiled, sizes)
        && let Some((i, (legend_place, locator_place, arrow_place, scale_place))) =
            grouped(clip, 0, &known, scale_box(ground_per_pt(s, center, scale), scale))
    {
        legend_cols = i + 1;
        (cw, ch) = known[i];
        for (kind, place) in [
            (Surround::Legend, Some(legend_place)),
            (Surround::Locator, locator_place),
            (Surround::Arrow, arrow_place),
            (Surround::Scale, Some(scale_place)),
        ] {
            let Some(place) = place.filter(|_| surrounds.iter().any(|x| x.0 == kind)) else { continue };
            match spots.iter_mut().find(|(k, _)| *k == kind) {
                Some(spot) => spot.1 = place,
                None => spots.push((kind, place)),
            }
        }
        // "moved" names each surround off its corner, the group's places judged as any other.
        moved = spots
            .iter()
            .filter(|(kind, r)| {
                let home = at(homes[home_slot(*kind)], r.2, r.3);
                (home.0 - r.0).abs() > 0.5 || (home.1 - r.1).abs() > 0.5
            })
            .map(|(kind, _)| named(*kind))
            .collect();
        arrangement_used = "veil".into();
    }
    if let Some(ElementKind::MapFrame { center: at, scale: drawn_at, .. }) = l.element_mut(frame).map(|e| &mut e.kind) {
        (*at, *drawn_at) = (center, scale);
    }
    // The veil over the frame and under its surrounds, in four bands around the extent, and the
    // extent's outline: it covers what the map draws there, which is said. It stops at the
    // graticule's labels, drawn in the frame's own picture under it, so they stay as dark as on an
    // unveiled map; the map under them stays unveiled, which is said too.
    if let Some((x, y, w, h)) = veiled {
        warnings.push(
            "what the map draws outside the extent is veiled to make room for the legend (except the strips under the graticule's labels), and the surrounds are grouped there: \"veil\": false leaves it unveiled"
                .into(),
        );
        let (x0, y0) = (x.max(area.0), y.max(area.1));
        let (x1, y1) = ((x + w).min(area.2), (y + h).min(area.3));
        let veil = || pub_card(Rgba::new(255, 255, 255, 150), Rgba::new(0, 0, 0, 0), 0.0, 0.0);
        for (name, band) in [
            ("Veil Above", (area.0, area.1, area.2 - area.0, y0 - area.1)),
            ("Veil Below", (area.0, y1, area.2 - area.0, area.3 - y1)),
            ("Veil Left", (area.0, y0, x0 - area.0, y1 - y0)),
            ("Veil Right", (x1, y0, area.2 - x1, y1 - y0)),
        ] {
            // A band thinner than half a millimetre (a rounding) is left out.
            if band.2 > 0.5 && band.3 > 0.5 {
                l.add(name, band, veil());
            }
        }
        l.add("Extent Outline", (x0, y0, x1 - x0, y1 - y0), pub_card(Rgba::new(0, 0, 0, 0), Rgba::rgb(0x2B, 0x26, 0x20), 0.8, 0.0));
    }
    if surrounds.iter().any(|x| x.0 == Surround::Legend) && !spots.iter().any(|(kind, _)| *kind == Surround::Legend) {
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
    let surround_box = |fill: Rgba| if style.cards() { pub_card(fill, clear, 0.0, 2.0 * k) } else { pub_box(fill, clear, 0.0) };
    if let (Some((map, locator_scale, shape, locator_center)), Some(r)) = (&locator_map, locator_rect) {
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
                center: *locator_center,
                scale: *locator_scale,
                shape: shape.clone(),
                grid: None,
                extent_of: Some(frame),
                extent_color: rustgis_map::layout::INDICATOR_COLOR,
                extent_width: rustgis_map::layout::INDICATOR_WIDTH,
                border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
                border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
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
    // Over the map, where it shows nothing under the legend but its plain paper or water (no layer,
    // context, basemap shape or graticule line, and no veil), the legend needs no box to be read:
    // the box is kept, clear, unless "legendBox": "always" (#1379). A card is the style's look,
    // and stays.
    let legend_bare =
        legend_rect.filter(|_| column.is_none() && !style.cards() && veiled.is_none() && !legend_box_always).is_some_and(|r| {
            covered(r) == 0.0
                && !graticule_crosses(s, center, scale, (fw, fh), (r.0 - fx, r.1 - fy, r.2, r.3), step)
                && crate::render::layout::frame_plain_under(s.map_mut(), center, scale, (fw, fh), (r.0 - fx, r.1 - fy, r.2, r.3), 384)
        });
    if let Some(r) = legend_rect {
        let (lw, lh) = (r.2, r.3);
        l.add(
            "Legend Box",
            r,
            if card {
                pub_card(Rgba::rgb(0xFF, 0xFF, 0xFF), Rgba::new(0x2B, 0x26, 0x20, 60), 0.4, card_r)
            } else {
                surround_box(if legend_bare { clear } else { paper })
            },
        );
        // The colorbar at the bottom of the box, as tall as the box allows; the legend's rows in
        // what is left above it. Each says when it doesn't fit, rather than one blaming the other.
        // The colorbar's title wraps to the box as it is drawn (#2493), which may be wider than
        // the colorbar asked for (the rows in more columns): fewer lines leave the rows more room.
        let (ix, iy, iw, ih) = (r.0 + pad, r.1 + pad, lw - 2.0 * pad, lh - 2.0 * pad);
        let bar_need = bar.as_deref().map_or(0.0, |title| colorbar_mm(title, iw));
        let bar_fit = bar_need.min(ih);
        let rows_h = (ih - bar_fit - if bar.is_some() { bar_part - bar_h } else { 0.0 }).max(0.0);
        warnings.extend(legend_fit_warnings(legend_cols, (cw, ch), (iw, rows_h)).into_iter().map(String::from));
        // In a column the colorbar takes the column's width, as its rows do.
        if let Some(layer) = bar.as_deref().filter(|_| (base_w > iw + LEGEND_FIT_MM && !legend_in_column) || bar_need > ih + LEGEND_FIT_MM)
        {
            warnings.push(format!("the legend's box is smaller than the colorbar of '{layer}': it is drawn narrower or shorter"));
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
                    show_no_data: false,
                    exclude: context.clone(),
                    columns: legend_cols,
                    labels: Default::default(),
                    color: None,
                    text_size: rustgis_map::layout::LEGEND_TEXT_PT,
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
                    color: None,
                },
            );
        }
    }
    if let Some((sx, sy, w, h)) = spot_of(Surround::Scale) {
        placed.push(("scale bar", (sx, sy, w, h)));
        l.add("Scale Box", (sx, sy, w, sh + nh), surround_box(paper));
        l.add("Scale Bar", (sx, sy, w, sh), ElementKind::scale_bar(frame));
        l.add("Scale", (sx, sy + sh, w, nh), pub_text("{scale}".into(), small_pt, ink, false, TextAlign::Center));
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
    if let (None, Some(legend)) = (column, spot_of(Surround::Legend)) {
        align_footer(&mut around, legend, (fx, fw), &sections);
    }
    for (n, r, kind, _) in over {
        l.add(&n, r, kind);
    }
    for (n, r, kind) in around {
        l.add(&n, r, kind);
    }
    if s.project.layouts.len() >= crate::project::MAX_LAYOUTS {
        return Err(format!("a project holds at most {} layouts; remove one first", crate::project::MAX_LAYOUTS));
    }
    // The coverage grid as the placement searched it, with what was placed on it, for a look at
    // why a surround moved (#1379). The layout is made by then (nothing below refuses it): a file
    // that can't be written is said, and the layout kept.
    let mut coverage_written = Value::Null;
    if let Some(path) = &coverage_path {
        let rects: Vec<Rect> = placed.iter().map(|(_, r)| *r).collect();
        let png = coverage
            .as_ref()
            .ok_or_else(|| "the map couldn't be drawn for it".to_string())
            .and_then(|grid| coverage_png(grid, (fx, fy, fw, fh), area, &rects));
        match png.and_then(|bytes| rustgis_data::io::write_atomic(Path::new(path), &bytes).map_err(|e| format!("{path}: {e}"))) {
            Ok(()) => coverage_written = json!(path),
            Err(why) => warnings.push(format!("the coverage image isn't written: {why}")),
        }
    }
    // Every element by name and id, for `layout.update`.
    let elements = element_list(&l);
    s.project.layouts.push(l);
    let i = s.project.layouts.len() - 1;
    s.active_layout = Some(i);
    for cut in [name_cut, locator_cut].into_iter().flatten() {
        s.notify(cut, false);
    }
    let locator = locator_map.map(|(map, ..)| map);
    Ok(
        json!({"layout": i, "name": name, "profile": profile.id, "page": [w, h], "scale": scale, "grid": step, "locator": locator, "style": style.code(), "arrangement": arrangement_used, "legendColumns": legend_cols, "mapShift": [map_shift.0, map_shift.1], "veiled": veiled.is_some(), "legendBox": legend_rect.is_some() && !legend_bare, "elements": elements, "moved": moved, "coverage": coverage_written, "warnings": warnings}),
    )
}

/// The coverage grid of a publication's frame (`frame`, page mm) as a PNG, three pixels a cell
/// (#1379): a cell the map draws dark, a free one white, both tinted blue outside `area` (the
/// strips the grid's labels keep, which the search leaves out), and each surround placed over the
/// map (`placed`, page mm) outlined in red.
fn coverage_png(
    (gw, gh, cells): &(u32, u32, Vec<bool>),
    frame: Rect,
    area: (f64, f64, f64, f64),
    placed: &[Rect],
) -> Result<Vec<u8>, String> {
    const CELL_PX: u32 = 3;
    let (fx, fy, fw, fh) = frame;
    let mut pm = tiny_skia::Pixmap::new(gw * CELL_PX, gh * CELL_PX).ok_or("the coverage grid is empty")?;
    let paint = |r: u8, g: u8, b: u8| {
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(r, g, b, 255);
        paint
    };
    let (dark, free, dark_band, free_band) =
        (paint(0x33, 0x33, 0x33), paint(0xFF, 0xFF, 0xFF), paint(0x2E, 0x4A, 0x7A), paint(0xC9, 0xDD, 0xF2));
    for (i, drawn) in cells.iter().enumerate() {
        let (col, row) = ((i % *gw as usize) as f64, (i / *gw as usize) as f64);
        let (x, y) = (fx + (col + 0.5) / f64::from(*gw) * fw, fy + (row + 0.5) / f64::from(*gh) * fh);
        let band = x < area.0 || x > area.2 || y < area.1 || y > area.3;
        let colour = match (*drawn, band) {
            (true, false) => &dark,
            (false, false) => &free,
            (true, true) => &dark_band,
            (false, true) => &free_band,
        };
        let px = CELL_PX as f32;
        if let Some(cell) = tiny_skia::Rect::from_xywh(col as f32 * px, row as f32 * px, px, px) {
            pm.fill_rect(cell, colour, tiny_skia::Transform::identity(), None);
        }
    }
    let (sx, sy) = (f64::from(pm.width()) / fw, f64::from(pm.height()) / fh);
    let line = tiny_skia::Stroke { width: 2.0, ..tiny_skia::Stroke::default() };
    for (x, y, w, h) in placed {
        let r = tiny_skia::Rect::from_xywh(((x - fx) * sx) as f32, ((y - fy) * sy) as f32, (w * sx) as f32, (h * sy) as f32);
        if let Some(r) = r {
            pm.stroke_path(&tiny_skia::PathBuilder::from_rect(r), &paint(0xD6, 0x27, 0x28), &line, tiny_skia::Transform::identity(), None);
        }
    }
    pm.encode_png().map_err(|e| e.to_string())
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

/// A sheet's footer of two columns, its right one moved to start where a legend at the right of the
/// frame (`frame` its left edge and width, page mm) does, when that is between 45 % and 70 % of the
/// frame's width, so the two line up (#1379). Each column's heading and lines are wrapped again to
/// its new width (`sections`, as the footer wraps them); when they'd need a taller box, the
/// footer stays as it is.
pub(crate) fn align_footer(
    around: &mut [(String, Rect, ElementKind)],
    legend: Rect,
    (fx, fw): (f64, f64),
    sections: &dyn Fn(f64, f64, f64) -> Vec<(&'static str, Vec<String>, Vec<String>)>,
) {
    let at = (legend.0 - fx) / fw;
    if !(0.45..=0.70).contains(&at) || legend.0 + legend.2 < fx + 0.9 * fw {
        return;
    }
    let find = |name: &str| around.iter().position(|(n, ..)| n == name);
    let size = |i: Option<usize>| match i.and_then(|i| around.get(i)).map(|e| &e.2) {
        Some(ElementKind::Text { size, .. }) => Some(*size),
        _ => None,
    };
    let (Some(a), Some(b), Some(head_at)) = (find("Sources Box"), find("Reference Box"), find("Sources Heading")) else { return };
    let (Some(head_pt), Some(body_pt)) = (size(Some(head_at)), size(find("Sources")).or_else(|| size(find("Reference")))) else { return };
    let (Some(left), Some(right), Some(head)) = (around.get(a), around.get(b), around.get(head_at)) else { return };
    let (left, right, inner) = (left.1, right.1, head.1.0 - left.1.0);
    let gap = right.0 - (left.0 + left.2);
    let (lw, rw) = (legend.0 - gap - left.0, right.0 + right.2 - legend.0);
    // Side by side, and the move a real one.
    if (left.1 - right.1).abs() > 1e-6 || lw <= 2.0 * inner || rw <= 2.0 * inner || (legend.0 - right.0).abs() < 0.5 {
        return;
    }
    let pitch = crate::render::layout::line_pitch_mm;
    let column = |w: f64, name: &str| sections(w - 2.0 * inner, head_pt, body_pt).into_iter().find(|(n, ..)| *n == name);
    let (Some((_, l_head, l_lines)), Some((_, r_head, r_lines))) = (column(lw, "Sources"), column(rw, "Reference")) else { return };
    let head_h = l_head.len().max(r_head.len()) as f64 * pitch(head_pt);
    let rows = l_lines.len().max(r_lines.len());
    if 2.0 * inner + head_h + rows as f64 * pitch(body_pt) > left.3 + 1e-6 {
        return;
    }
    for (name, x, w, head, lines) in [("Sources", left.0, lw, l_head, l_lines), ("Reference", legend.0, rw, r_head, r_lines)] {
        let y = left.1;
        for (part, r, text) in [
            (format!("{name} Box"), (x, y, w, left.3), None),
            (format!("{name} Heading"), (x + inner, y + inner, w - 2.0 * inner, head.len() as f64 * pitch(head_pt)), Some(head.join("\n"))),
            (
                name.to_string(),
                (x + inner, y + inner + head_h, w - 2.0 * inner, lines.len() as f64 * pitch(body_pt)),
                Some(lines.join("\n")),
            ),
        ] {
            if let Some(e) = around.iter_mut().find(|(n, ..)| *n == part) {
                e.1 = r;
                if let (Some(new), ElementKind::Text { text, .. }) = (text, &mut e.2) {
                    *text = new;
                }
            }
        }
    }
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
        c.background = e.background;
    }
    super::say_name_cut(s, "Duplicate Element", "the copy's name", &e.name, &name);
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

/// Said when a map frame is given a background: it draws its map and its paper itself.
const NO_FRAME_BACKGROUND: &str = "a map frame takes no background: it draws its map and the paper behind it";
/// Said when a grid or an extent indicator is added with a background: they go on a map frame.
const NO_OVERLAY_BACKGROUND: &str = "a grid or an extent indicator goes on a map frame, which takes no background";
/// What an element's `background` object takes.
const BACKGROUND_KEYS: [&str; 5] = ["fill", "outline", "width", "radius", "padding"];

/// What a command's `background` asks for (#2257).
enum BackgroundParam {
    /// Not given: the element keeps what it has.
    Absent,
    /// `null`: none.
    Remove,
    /// The box to draw.
    Set(Background),
}

/// An element's `background` from `p` (#2257): `null` for none, else the box `{"fill"?, "outline"?,
/// "width"?, "radius"?, "padding"?}` over `current`, the keys it doesn't give keeping theirs (a new
/// one is white, with no outline, square corners and no padding). A negative number or a bad colour
/// is refused; a number past its limit is held to it, as a rectangle's radius is.
fn element_background(p: &Value, current: Option<Background>) -> Result<BackgroundParam, String> {
    let Some(v) = p.get("background") else { return Ok(BackgroundParam::Absent) };
    let o = match v {
        Value::Null => return Ok(BackgroundParam::Remove),
        Value::Object(o) => o,
        _ => return Err("background is an object of fill, outline, width, radius and padding, or null for none".into()),
    };
    if let Some(k) = o.keys().find(|k| !BACKGROUND_KEYS.contains(&k.as_str())) {
        return Err(format!("background takes fill, outline, width, radius and padding, not '{k}'"));
    }
    let base = current.unwrap_or(Background::filled(Rgba::rgb(0xFF, 0xFF, 0xFF)));
    let amount = |k: &str, d: f64, unit: &str| -> Result<f64, String> {
        match o.get(k).filter(|x| !x.is_null()) {
            None => Ok(d),
            Some(x) => x.as_f64().filter(|n| n.is_finite() && *n >= 0.0).ok_or_else(|| format!("a background's {k} is {unit}, 0 or more")),
        }
    };
    let colour = |k: &str, d: Rgba| color(v, k, d).map_err(|e| format!("a background's {e}"));
    Ok(BackgroundParam::Set(
        Background {
            fill: colour("fill", base.fill)?,
            outline: colour("outline", base.outline)?,
            width: amount("width", base.width, "points")?,
            radius: amount("radius", base.radius, "millimetres")?,
            padding: amount("padding", base.padding, "millimetres")?,
        }
        .clamped(),
    ))
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

/// The shape whose outline `width` (and a rectangle's `radius`) a layout command reads, by the
/// element's kind: what [`held_to_limits`] checks.
#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Rectangle,
    Ellipse,
    Line,
}

impl Shape {
    fn of_kind(kind: &ElementKind) -> Option<Shape> {
        match kind {
            ElementKind::Rectangle { .. } => Some(Shape::Rectangle),
            ElementKind::Ellipse { .. } => Some(Shape::Ellipse),
            ElementKind::Line { .. } => Some(Shape::Line),
            _ => None,
        }
    }

    fn of_name(kind: &str) -> Option<Shape> {
        match kind {
            "rectangle" => Some(Shape::Rectangle),
            "ellipse" | "circle" => Some(Shape::Ellipse),
            "line" | "arrow" => Some(Shape::Line),
            _ => None,
        }
    }
}

/// What a layout command holds to a limit instead of taking as given (#2639): a shape's outline
/// `width` (0 to 50 points; a line's from 0.1), a rectangle's `radius` (up to 50 mm), and a
/// background's width, radius and padding, each said with the value asked and the one kept. A
/// value the command refuses (not a number, negative where that isn't taken) isn't said here.
fn held_to_limits(p: &Value, shape: Option<Shape>) -> Vec<String> {
    use rustgis_map::layout::{MAX_BACKGROUND_PADDING, MAX_BACKGROUND_RADIUS, MAX_BACKGROUND_WIDTH};
    let mut said = Vec::new();
    let mut hold = |key: &str, asked: f64, kept: f64, range: &str| {
        if asked != kept {
            said.push(format!("{key} {asked} held to {kept} ({range})"));
        }
    };
    if let Some(shape) = shape {
        if let Some(w) = f(p, "width") {
            let least = if shape == Shape::Line { 0.1 } else { 0.0 };
            hold("width", w, w.clamp(least, 50.0), if shape == Shape::Line { "0.1 to 50 points" } else { "0 to 50 points" });
        }
        if shape == Shape::Rectangle
            && let Some(r) = p.get("radius").and_then(Value::as_f64).filter(|r| r.is_finite() && *r >= 0.0)
        {
            hold("radius", r, r.min(50.0), "up to 50 mm");
        }
    }
    if let Some(o) = p.get("background").and_then(Value::as_object) {
        for (k, max, unit) in
            [("width", MAX_BACKGROUND_WIDTH, "points"), ("radius", MAX_BACKGROUND_RADIUS, "mm"), ("padding", MAX_BACKGROUND_PADDING, "mm")]
        {
            if let Some(v) = o.get(k).and_then(Value::as_f64).filter(|v| v.is_finite() && *v >= 0.0) {
                hold(&format!("background {k}"), v, v.min(max), &format!("up to {max} {unit}"));
            }
        }
    }
    said
}

/// A frame's extent indicator's or border's `width` in `p` (that object) held to `range` points,
/// said as [`held_to_limits`] says it; `what` names the setting ("extent indicator width").
fn held_width(p: &Value, what: &str, range: (f64, f64)) -> Option<String> {
    let w = f(p, "width")?;
    let kept = w.clamp(range.0, range.1);
    (w != kept).then(|| format!("{what} {w} held to {kept} ({} to {} points)", range.0, range.1))
}

/// Says what [`held_to_limits`] found, in one notice for the command `title`.
fn say_held(s: &mut Session, title: &str, held: Vec<String>) {
    if !held.is_empty() {
        s.notify(format!("{title}: {}", held.join("; ")), false);
    }
}

/// The colour `p` gives as `k`, `None` when it gives none (absent or null). Any other value is
/// refused rather than ignored.
fn given_color(p: &Value, k: &str) -> Result<Option<Rgba>, String> {
    if gives(p, k) { color(p, k, Rgba::rgb(0, 0, 0)).map(Some) } else { Ok(None) }
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
/// Refuse the optional no-data key on an element that cannot display it.
const ONLY_LEGEND_NO_DATA: &str = "only legends take showNoData";
const ONLY_LEGEND_LABELS: &str = "only legends label layers";
const ONLY_LEGEND_TEXT_SIZE: &str = "only legends take textSize";

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

/// What a legend of `map` can't key, or keys only in part, for the rasters it shows by classes
/// (#1679), those `exclude` leaves out aside: one warning a kind, with how many and their names.
/// A colour table over more values than a legend can list (over 256, or not whole numbers) gives
/// no class and no key; classes taken from an overview's statistics (a COG read in place), or from
/// a sample of a large raster under a mask, can miss a rare one the map draws, unless
/// `all_classes` lists every class of the renderer.
fn raster_key_notices(map: &Map, exclude: &[String], all_classes: bool) -> Vec<String> {
    let shown: Vec<(&str, &rustgis_map::RasterLayer)> = map
        .layers
        .iter()
        .filter(|l| map.shows(l) && !exclude.iter().any(|n| n.eq_ignore_ascii_case(&l.name)))
        .filter_map(|l| Some((l.name.as_str(), l.raster.as_ref()?)))
        .collect();
    let names = |kind: &dyn Fn(&rustgis_map::RasterLayer) -> bool| -> Vec<&str> {
        shown.iter().filter(|(_, r)| kind(r)).map(|(n, _)| *n).collect()
    };
    let unlisted = names(&rustgis_map::RasterLayer::classes_unknown);
    let reduced = names(&|r| r.classes_from_overview(all_classes));
    let sampled = names(&|r| r.classes_from_sample(all_classes));
    let mut said = Vec::new();
    if !unlisted.is_empty() {
        said.push(format!(
            "{} raster(s) drawn by a colour table over more values than a legend can list (over 256, or not whole numbers) have no key in the legend: {}",
            unlisted.len(),
            unlisted.join(", ")
        ));
    }
    if !reduced.is_empty() {
        said.push(format!(
            "{} raster(s) drawn by classes were read through an overview, so the legend lists the classes found there and may miss a rare one the map draws: {}",
            reduced.len(),
            reduced.join(", ")
        ));
    }
    if !sampled.is_empty() {
        said.push(format!(
            "{} raster(s) drawn by classes under a mask have more cells than their masked statistics read, so the legend lists the classes of a sample and may miss a rare one the map draws: {}",
            sampled.len(),
            sampled.join(", ")
        ));
    }
    said
}

/// Says, a kind once, what the legend `id` of the layout `li` can't key ([`raster_key_notices`]),
/// as `layout.publication` does for its own: for a legend added, or told what it leaves out or
/// whether it lists every class.
fn say_raster_keys(s: &mut Session, li: usize, id: u32) {
    let Some(l) = s.project.layouts.get(li) else { return };
    let Some(ElementKind::Legend { frame, exclude, all_classes, .. }) = l.element(id).map(|e| &e.kind) else { return };
    let Some(ElementKind::MapFrame { map, .. }) = l.element(*frame).map(|f| &f.kind) else { return };
    let said = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)).map(|m| raster_key_notices(m, exclude, *all_classes));
    for said in said.unwrap_or_default() {
        s.warn(said);
    }
}

/// Whether a legend `kind`, after a command with `p`, says what it loses in its box (#2581): the
/// one rule `layout.addElement` and `layout.update` both take. Told when it is given what sets
/// what it needs of its box (its columns, its missing-value row, or its text size, `null`
/// included, which gives the default back) or labels (text chosen for the box), and, in columns
/// or at a text size of its own, when its box or what it lists changes. A move changes neither.
fn tells_legend_fit(kind: &ElementKind, p: &Value) -> bool {
    let ElementKind::Legend { columns, text_size, .. } = kind else { return false };
    let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
    let watched = *columns > 1 || *text_size != rustgis_map::layout::LEGEND_TEXT_PT;
    let refitted = ["w", "h", "title", "allClasses", "hideOther", "exclude", "labels"].iter().any(|k| given(k));
    ["columns", "showNoData", "labels"].iter().any(|k| given(k)) || p.get("textSize").is_some() || (watched && refitted)
}

/// Says what a legend loses in its box, as `layout.publication` does for its own, or that it can't
/// tell.
fn say_legend_fit(s: &mut Session, li: usize, id: u32) {
    let Some(l) = s.project.layouts.get(li) else { return };
    let Some(e) = l.element(id) else { return };
    let ElementKind::Legend { frame, title, all_classes, hide_other, show_no_data, exclude, columns, labels, text_size, .. } = &e.kind
    else {
        return;
    };
    let shown = match l.element(*frame).map(|f| &f.kind) {
        Some(ElementKind::MapFrame { map, center, scale, .. }) => Some((map.clone(), *center, *scale)),
        _ => None,
    };
    let (room_w, room_h) = (e.w, e.h - if title.is_empty() { 0.0 } else { crate::render::layout::legend_title_mm(*text_size) });
    let (all_classes, hide_other, show_no_data, exclude, columns, labels, text_size) =
        (*all_classes, *hide_other, *show_no_data, exclude.clone(), *columns, labels.clone(), *text_size);
    let found = shown
        .and_then(|(name, center, scale)| Some((s.project.maps.iter_mut().find(|m| m.name.eq_ignore_ascii_case(&name))?, center, scale)));
    let Some((map, center, scale)) = found else {
        s.warn("the legend's frame shows no map of this project, so the legend lists nothing");
        return;
    };
    let at = crate::render::layout::frame_render_scale(map, center, scale);
    let Some(sizes) = crate::render::layout::legend_sizes_with_no_data_mm(
        map,
        at,
        all_classes,
        hide_other,
        (show_no_data, text_size),
        (&exclude, &labels),
    ) else {
        s.warn("a layer's source couldn't be read through, so the legend's fit in its box wasn't checked; exporting says why");
        return;
    };
    let size = sizes[columns.clamp(1, MAX_LEGEND_COLUMNS) - 1];
    for said in legend_fit_warnings(columns, size, (room_w, room_h)) {
        s.warn(said);
    }
}

/// Says how many lines of the text `id` of the layout `li` don't fit its box (#2346), as the
/// layout view and an export count them: on the series' current page, measured at 300 dpi as a
/// wrapped text is broken.
fn say_text_fit(s: &mut Session, li: usize, id: u32) {
    let Some(l) = s.project.layouts.get(li) else { return };
    if !matches!(l.element(id).map(|e| &e.kind), Some(ElementKind::Text { .. })) {
        return;
    }
    let page = l.series.as_ref().map_or(0, |sr| sr.page);
    let series = series_info(s, li, page);
    let info = rustgis_render::layout::PageInfo {
        page: page + 1,
        pages: series.as_ref().map_or(1, |i| i.0),
        page_name: series.map(|i| i.1).unwrap_or_default(),
    };
    let said = rustgis_render::layout::text_loss(l, &s.project.maps, 300.0, &info, id)
        .as_ref()
        .and_then(rustgis_render::layout::lines_left_out_notice);
    if let Some(said) = said {
        s.warn(said);
    }
}

/// What a text is given that changes the lines it breaks into or the room they have, or shows it
/// (#2346): a move or a turn doesn't, nor a colour (the Format pane sets one at each step of a
/// pick), except one that shows a text without ink, which `update` tells apart.
const TEXT_FIT_KEYS: [&str; 12] =
    ["text", "size", "w", "h", "wrap", "font", "fallback", "bold", "italic", "letterSpacing", "align", "visible"];

/// The `"columns"` a legend is given, refused outside 1 to [`MAX_LEGEND_COLUMNS`].
fn legend_columns_param(p: &Value) -> Result<Option<usize>, String> {
    let Some(v) = p.get("columns").filter(|v| !v.is_null()) else { return Ok(None) };
    v.as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| (1..=MAX_LEGEND_COLUMNS).contains(n))
        .map(Some)
        .ok_or_else(|| format!("columns is a whole number from 1 to {MAX_LEGEND_COLUMNS}"))
}

/// A legend's `textSize` (#2581): its names' and labels' points, within [`LEGEND_TEXT_SIZE`];
/// `null` gives the default back, as for its ink. Not given, `None`; a size out of the range is
/// refused, as `columns` is, rather than drawn at another size than the one asked for.
fn legend_text_size_param(p: &Value) -> Result<Option<f64>, String> {
    match p.get("textSize") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(rustgis_map::layout::LEGEND_TEXT_PT)),
        Some(v) => v
            .as_f64()
            .filter(|v| LEGEND_TEXT_SIZE.contains(v))
            .map(Some)
            .ok_or_else(|| format!("textSize is a number of points, {} to {}", LEGEND_TEXT_SIZE.start(), LEGEND_TEXT_SIZE.end())),
    }
}

fn add_element(s: &mut Session, p: &Value) -> CmdResult {
    let kind = ps(p, "kind").ok_or("kind is required")?;
    if kind != "legend" && p.get("textSize").is_some_and(|v| !v.is_null()) {
        return Err(ONLY_LEGEND_TEXT_SIZE.into());
    }
    if kind != "legend" && p.get("showNoData").is_some_and(|v| !v.is_null()) {
        return Err(ONLY_LEGEND_NO_DATA.into());
    }
    if kind != "legend" && p.get("exclude").is_some_and(|v| !v.is_null()) {
        return Err("only legends leave layers out".into());
    }
    if kind != "legend" && p.get("columns").is_some_and(|v| !v.is_null()) {
        return Err(ONLY_LEGEND_COLUMNS.into());
    }
    // A grid's "labels" is a bool and a legend's an object (read with it); on any other kind, or as
    // anything else on a grid, it is refused rather than left unread.
    match p.get("labels").filter(|v| !v.is_null()) {
        Some(v) if kind == "grid" && !v.is_boolean() => return Err("a grid's \"labels\" is true or false".into()),
        Some(_) if kind != "legend" && kind != "grid" => return Err(ONLY_LEGEND_LABELS.into()),
        _ => {}
    }
    // A family is a text's: refused elsewhere, as a legend's left-out layers are, so a script
    // doesn't take it as applied.
    if !matches!(kind, "text" | "dynamicText") && gives_font(p) {
        return Err(ONLY_TEXT_FONT.into());
    }
    if kind != "rectangle" && p.get("radius").is_some_and(|v| !v.is_null()) {
        return Err(ONLY_RECTANGLE_RADIUS.into());
    }
    // `null` too: on a line it means solid, so elsewhere it is refused rather than ignored.
    if !matches!(kind, "line" | "arrow" | "rectangle" | "ellipse" | "circle") && p.get("dash").is_some() {
        return Err(ONLY_DASH.into());
    }
    if !matches!(kind, "text" | "dynamicText") && gives(p, "rotation") {
        return Err(ONLY_TEXT_ROTATION.into());
    }
    if !matches!(kind, "text" | "dynamicText") && gives(p, "wrap") {
        return Err(ONLY_TEXT_WRAP.into());
    }
    if kind != "chart" && gives_chart_options(p) {
        return Err(ONLY_CHART_OPTIONS.into());
    }
    if !matches!(
        kind,
        "text"
            | "dynamicText"
            | "rectangle"
            | "ellipse"
            | "circle"
            | "line"
            | "arrow"
            | "extentIndicator"
            | "grid"
            | "scaleBar"
            | "legend"
            | "colorbar"
            | "table"
            | "chart"
    ) && gives(p, "color")
    {
        return Err(ONLY_COLOR_ADDED.into());
    }
    // A legend's, colorbar's, table's or chart's ink (#2257), checked before anything is added:
    // none, absent or null, is the default dark grey. The other kinds read "color" as their own.
    let ink = given_color(p, "color")?;
    let shape = matches!(kind, "northArrow" | "rectangle" | "ellipse" | "circle");
    if gives(p, "fill") && !shape && kind != "scaleBar" {
        return Err(ONLY_FILL.into());
    }
    if gives(p, "outline") && !shape {
        return Err(ONLY_OUTLINE.into());
    }
    // The box the new element sits on (#2257). A grid or an extent indicator goes on a map frame,
    // which has none: refused rather than dropped.
    let background = match element_background(p, None)? {
        BackgroundParam::Set(_) if matches!(kind, "grid" | "extentIndicator") => return Err(NO_OVERLAY_BACKGROUND.into()),
        BackgroundParam::Set(b) => Some(b),
        BackgroundParam::Absent | BackgroundParam::Remove => None,
    };
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
            let title = super::within("title", "a legend's title", ps(p, "title").unwrap_or("Legend"), 200)?;
            let all_classes = b(p, "allClasses").unwrap_or(false);
            let hide_other = b(p, "hideOther").unwrap_or(false);
            let show_no_data = b(p, "showNoData").unwrap_or(false);
            let exclude = legend_exclude(s, l, frame, p)?.unwrap_or_default();
            let columns = legend_columns_param(p)?.unwrap_or(1);
            let labels = match legend_labels(s, l, frame, p)? {
                Some(given) => with_labels(&BTreeMap::new(), given)?,
                None => BTreeMap::new(),
            };
            let text_size = legend_text_size_param(p)?.unwrap_or(rustgis_map::layout::LEGEND_TEXT_PT);
            (
                "Legend",
                place(p, l, (pw - 70.0, 45.0, 60.0, 80.0))?,
                ElementKind::Legend {
                    frame,
                    title,
                    all_classes,
                    hide_other,
                    show_no_data,
                    exclude,
                    columns,
                    labels,
                    color: ink,
                    text_size,
                },
            )
        }
        "colorbar" => {
            let frame = frame_of(l, p)?;
            let layer = colorbar_layer(s, l, frame, ps(p, "layer"))?;
            let title = match ps(p, "title") {
                Some(t) => super::within("title", "a colorbar's title", t, 200)?,
                None => layer.clone().unwrap_or_default(),
            };
            let orientation = orientation(p)?.unwrap_or_default();
            let rect = match orientation {
                Orientation::Horizontal => (15.0, ph - 45.0, 80.0, 16.0),
                Orientation::Vertical => (pw - 40.0, 45.0, 25.0, 80.0),
            };
            let k = ElementKind::Colorbar {
                frame,
                // Kept only when named: otherwise (none, or "") it follows the map's top shown raster with a
                // colour scale, not drawn by classes.
                layer: ps(p, "layer").filter(|n| !n.trim().is_empty()).and(layer),
                title,
                orientation,
                ticks: ticks(p).unwrap_or(5),
                tick_side: tick_side(p)?.unwrap_or_default(),
                reverse: b(p, "reverse").unwrap_or(false),
                color: ink,
            };
            ("Colorbar", place(p, l, rect)?, k)
        }
        "text" | "dynamicText" => {
            let default = if kind == "text" { "Text" } else { "{map} · {scale} · {date}" };
            let text = super::within("text", "a text element's text", ps(p, "text").unwrap_or(default), 5000)?;
            let fonts = rustgis_map::graphics::FONT_SIZE;
            let size = f(p, "size").unwrap_or(12.0).clamp(*fonts.start(), *fonts.end());
            let (italic, align, letter_spacing) = text_style(p, false, TextAlign::Left, 0.0)?;
            let (font, font_name) = text_font(p, (FontFamily::Sans, None))?;
            let rotation = text_rotation(p)?.unwrap_or(0.0);
            let wrap = text_wrap(p)?.unwrap_or(false);
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
                rotation,
                wrap,
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
            ("Table Frame", place(p, l, (15.0, ph * 0.6, pw * 0.5, ph * 0.35))?, ElementKind::Table { frame, layer, rows, color: ink })
        }
        "chart" => {
            let frame = frame_of(l, p)?;
            let layer = frame_layer_name(s, l, frame, p)?;
            let ds = frame_dataset(s, l, frame, &layer).ok_or("no such layer")?;
            let mut o = chart_options(p, ChartOptions::default(), ds)?;
            // A new chart's stack is the one given, so one it can't draw is refused, never let go.
            if let Some(said) = stack_or_refuse(p, &mut o)? {
                return Err(said);
            }
            // Not given: the first numeric field, or for a count the field it groups by.
            let field = match ps(p, "field") {
                Some(fl) => ds.field_index(fl).and_then(|i| ds.fields.get(i)).ok_or_else(|| format!("no field '{fl}'"))?,
                None => ds
                    .fields
                    .iter()
                    .find(|f| f.ty.is_numeric())
                    .or_else(|| {
                        o.group.as_deref().filter(|_| o.statistic == StatKind::Count).and_then(|g| ds.fields.iter().find(|f| f.name == g))
                    })
                    .ok_or("the layer has no numeric field")?,
            };
            chart_measures(field, &o)?;
            let k = ElementKind::Chart {
                frame,
                layer,
                field: field.name.clone(),
                label: o.label,
                color: ink,
                group: o.group,
                statistic: o.statistic,
                order: o.order,
                bars: o.bars,
                other: o.other,
                orientation: o.orientation,
                values: o.values,
                decimals: o.decimals,
                title: o.title,
                bar_color: o.bar_color,
                label_align: o.label_align,
                classes: o.classes,
                stack: o.stack,
            };
            ("Chart Frame", place(p, l, (pw * 0.55, ph * 0.6, pw * 0.4, ph * 0.35))?, k)
        }
        "rectangle" => {
            let k = ElementKind::Rectangle {
                fill: color(p, "fill", Rgba::new(0, 0, 0, 0))?,
                outline: color(p, "outline", color(p, "color", Rgba::rgb(0x22, 0x22, 0x22))?)?,
                width: shape_width(p, 1.0)?.clamp(0.0, 50.0),
                radius: corner_radius(p, 0.0)?,
                dash: element_dash(p)?.flatten(),
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
                dash: element_dash(p)?.flatten(),
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
            let dash = element_dash(p)?.flatten();
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
            (name, rect, ElementKind::Line { color, width, dir, head, dash })
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
            let range = (rustgis_map::layout::MIN_INDICATOR_WIDTH, rustgis_map::layout::MAX_INDICATOR_WIDTH);
            say_held(s, "Add Element", held_width(p, "extent indicator width", range).into_iter().collect());
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
    // A colorbar with nothing to show says why, as a notice the app shows.
    let empty_colorbar = match &k {
        ElementKind::Colorbar { frame, layer: None, .. } => colorbar_without_raster(s, i, *frame),
        _ => None,
    };
    let l = s.project.layouts.get_mut(i).ok_or("no layout")?;
    let id = l.add(name, rect, k);
    if let Some(e) = l.element_mut(id) {
        e.background = background;
    }
    if l.element(id).is_some_and(|e| tells_legend_fit(&e.kind, p)) {
        say_legend_fit(s, i, id);
    }
    if kind == "legend" {
        say_raster_keys(s, i, id);
    }
    if kind == "chart" {
        say_chart_losses(s, i, id);
    }
    say_text_fit(s, i, id);
    if let Some(said) = empty_colorbar {
        s.warn(said);
    }
    say_held(s, "Add Element", held_to_limits(p, Shape::of_name(kind)));
    let mut out = json!({"element": id});
    if let Some(w) = warning {
        out["warning"] = json!(w);
    }
    Ok(out)
}

/// What a colorbar with no layer of its own on `frame` says when the frame's map shows no raster
/// with a colour scale for it but one drawn by classes (#1679): it draws an empty placeholder; the
/// legend lists those classes, unless a colour table has more values than it can list. `None`
/// otherwise. Said when the colorbar is added, or set back to no layer.
fn colorbar_without_raster(s: &Session, li: usize, frame: u32) -> Option<String> {
    let l = s.project.layouts.get(li)?;
    let Some(ElementKind::MapFrame { map, .. }) = l.element(frame).map(|f| &f.kind) else { return None };
    let m = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map))?;
    if rustgis_map::layout::colorbar_layer(m, None).is_some() {
        return None;
    }
    // Those the legend lists, and colour tables over more values than it can list, which nothing keys.
    let (mut listed, mut unlisted) = (Vec::new(), Vec::new());
    for l in m.layers.iter().filter(|l| m.shows(l)) {
        match l.raster.as_ref().filter(|r| r.style.renderer.draws_classes()) {
            Some(r) if r.classes_unknown() => unlisted.push(l.name.as_str()),
            Some(_) => listed.push(l.name.as_str()),
            None => {}
        }
    }
    let mut why = Vec::new();
    if !listed.is_empty() {
        why.push(format!(
            "{} is drawn by classes, which a legend lists rather than a colorbar (give \"layer\" to show its colours on one anyway)",
            listed.join(", ")
        ));
    }
    if !unlisted.is_empty() {
        why.push(format!(
            "{} draws a colour table over more values than a legend or a colorbar can list (over 256, or not whole numbers), so nothing keys it",
            unlisted.join(", ")
        ));
    }
    (!why.is_empty())
        .then(|| format!("the colorbar has no raster with a colour scale to show, so it draws an empty placeholder: {}", why.join("; ")))
}

/// The raster layer a colorbar on `frame` shows: `name` checked against the frame's map, or the
/// map's top visible raster with a colour scale, not drawn by classes
/// ([`rustgis_map::layout::colorbar_layer`]; `None` when the map has none yet).
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
    let current = match kind {
        Some(ElementKind::MapFrame { extent_color, extent_width, .. }) => (*extent_color, *extent_width),
        _ => (rustgis_map::layout::INDICATOR_COLOR, rustgis_map::layout::INDICATOR_WIDTH),
    };
    outline_style(p, current, (rustgis_map::layout::MIN_INDICATOR_WIDTH, rustgis_map::layout::MAX_INDICATOR_WIDTH))
}

/// A map frame's border colour and width (points, 0 for none) from `color` and `width` in `p`,
/// each keeping the frame's when not given (#2557), as [`indicator_style`] reads them.
fn border_style(p: &Value, kind: &ElementKind) -> Result<(Rgba, f64), String> {
    let current = match kind {
        ElementKind::MapFrame { border_color, border_width, .. } => (*border_color, *border_width),
        _ => (rustgis_map::layout::FRAME_BORDER_COLOR, rustgis_map::layout::FRAME_BORDER_WIDTH),
    };
    outline_style(p, current, (0.0, rustgis_map::layout::MAX_FRAME_BORDER_WIDTH))
}

/// An outline's colour and width (points, kept within `range`) from `color` and `width` in `p`,
/// each keeping `current`'s when not given.
fn outline_style(p: &Value, (c, w): (Rgba, f64), range: (f64, f64)) -> Result<(Rgba, f64), String> {
    let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
    let width = match (given("width"), f(p, "width")) {
        (_, Some(v)) => v.clamp(range.0, range.1),
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
            // (MGRS labels are a measured grid's; a graticule doesn't keep them.)
            if kind == GridKind::Graticule {
                g.mgrs = false;
            }
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
    if let Some(v) = b(p, "mgrs") {
        if v && g.kind != GridKind::Measured {
            return Err("mgrs labels a measured grid: give \"type\": \"measured\" with it".into());
        }
        g.mgrs = v;
    }
    // MGRS names a WGS 84 UTM zone's squares, so a measured grid with them is in one.
    if g.mgrs
        && let Some(c) = g.crs.as_deref().filter(|c| !c.trim().is_empty() && rustgis_map::layout::utm_zone_of(c).is_none())
    {
        return Err(format!(
            "crs: MGRS labels need a WGS 84 UTM zone by its EPSG code (EPSG:32601–32660 north, 32701–32760 south), not {}; give none for the map's own zone, else the one at the frame's centre",
            rustgis_map::graticule::short_crs(c)
        ));
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

/// What a `rotation` given for an element that isn't a text is told.
const ONLY_TEXT_ROTATION: &str = "only text elements take a rotation";

/// A text's `rotation` when given: degrees clockwise, kept within -180..=180 (450 is 90); refused
/// when not a number, as the other numbers of a layout are.
fn text_rotation(p: &Value) -> Result<Option<f64>, String> {
    Ok(given_num(p, "rotation", "a number of degrees, clockwise")?.map(rustgis_map::layout::rotation))
}

/// What a `wrap` given for an element that isn't a text is told.
const ONLY_TEXT_WRAP: &str = "only text elements wrap";

/// A text's `wrap` when given (#2346): true or false as [`super::bool_value`] reads it; refused
/// when neither, so a script isn't left with a text that silently doesn't wrap.
fn text_wrap(p: &Value) -> Result<Option<bool>, String> {
    match p.get("wrap").filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => super::bool_value(v).map(Some).ok_or_else(|| "wrap must be true or false".into()),
    }
}

/// What a chart's options given for another element are told.
const ONLY_CHART_OPTIONS: &str =
    "only charts take a label, group, statistic, order, bars, other, stack, values, decimals, barColor, classes or labelAlign";

/// The keys only a chart takes: what it draws, then how it draws it.
const CHART_DATA_KEYS: [&str; 7] = ["label", "group", "statistic", "order", "bars", "other", "stack"];
/// What a chart is given that changes the room its title and key leave its bars, beside its size.
const CHART_ROOM_KEYS: [&str; 3] = ["orientation", "title", "values"];
const CHART_LOOK_KEYS: [&str; 5] = ["values", "decimals", "barColor", "classes", "labelAlign"];

fn gives_chart_options(p: &Value) -> bool {
    gives_chart_data(p) || CHART_LOOK_KEYS.iter().any(|k| gives(p, k))
}

/// Whether `p` tells a chart what to draw, which it says its losses of again.
fn gives_chart_data(p: &Value) -> bool {
    CHART_DATA_KEYS.iter().any(|k| gives(p, k))
}

/// Whether `p` changes a chart's look: with a colorbar's and a legend's `orientation` and
/// `title`, and `null` too, which gives a chart's title or bar colour its default again.
fn changes_chart_look(p: &Value) -> bool {
    ["orientation", "title"].iter().chain(CHART_LOOK_KEYS.iter()).any(|k| p.get(*k).is_some())
}

/// A chart's options beyond its layer and field ([`ElementKind::Chart`], #2487).
struct ChartOptions {
    label: Option<String>,
    group: Option<String>,
    statistic: StatKind,
    order: ChartOrder,
    bars: usize,
    other: bool,
    orientation: Orientation,
    values: bool,
    decimals: Option<u8>,
    title: Option<String>,
    bar_color: Option<Rgba>,
    label_align: TextAlign,
    classes: bool,
    stack: Option<String>,
}

impl Default for ChartOptions {
    fn default() -> Self {
        ChartOptions {
            label: None,
            group: None,
            statistic: StatKind::default(),
            order: ChartOrder::default(),
            bars: CHART_BARS,
            other: false,
            orientation: Orientation::default(),
            values: false,
            decimals: None,
            title: None,
            bar_color: None,
            label_align: TextAlign::Left,
            classes: false,
            stack: None,
        }
    }
}

/// The chart options `p` gives over `base`, for a chart of `ds`: a label, group or stack field the
/// layer has (named as the layer names it; "" for none), a statistic, an order, a number of bars
/// and whether the rest go in an Other bar. Each one that isn't valid is refused, not dropped;
/// whether the chart can stack is [`stack_or_refuse`]'s.
fn chart_options(p: &Value, base: ChartOptions, ds: &rustgis_data::Dataset) -> Result<ChartOptions, String> {
    let field = |key: &str, what: &str| -> Result<Option<Option<String>>, String> {
        match p.get(key).filter(|v| !v.is_null()) {
            None => Ok(None),
            Some(v) => match v.as_str().map(str::trim) {
                Some("") => Ok(Some(None)),
                Some(n) => ds
                    .field_index(n)
                    .and_then(|i| ds.fields.get(i))
                    .map(|f| Some(Some(f.name.clone())))
                    .ok_or_else(|| format!("the layer has no field '{n}' to {what}")),
                None => Err(format!("{key} is a field's name, or \"\" for none")),
            },
        }
    };
    let label = field("label", "label the bars with")?.unwrap_or(base.label);
    let group = field("group", "group by")?.unwrap_or(base.group);
    let stack = field("stack", "stack by")?.unwrap_or(base.stack);
    let statistic = match p.get("statistic").filter(|v| !v.is_null()) {
        None => base.statistic,
        Some(v) => v.as_str().and_then(StatKind::from_code).ok_or("statistic is count, sum, mean, min or max")?,
    };
    let order = match p.get("order").filter(|v| !v.is_null()) {
        None => base.order,
        Some(v) => v.as_str().and_then(ChartOrder::from_code).ok_or("order is descending, ascending, category or layer")?,
    };
    let bars = match p.get("bars").filter(|v| !v.is_null()) {
        None => base.bars,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=MAX_CHART_BARS as u64).contains(n))
            .map(|n| n as usize)
            .ok_or_else(|| format!("bars is a whole number from 1 to {MAX_CHART_BARS}"))?,
    };
    let other = match p.get("other").filter(|v| !v.is_null()) {
        None => base.other,
        Some(v) => super::bool_value(v).ok_or("other must be true or false")?,
    };
    chart_look_options(p, ChartOptions { label, group, statistic, order, bars, other, stack, ..base })
}

/// The look options `p` gives a chart over `base` (#2487): none reads its layer, so a chart
/// whose layer is gone still takes them. Each one that isn't valid is refused, not dropped.
fn chart_look_options(p: &Value, base: ChartOptions) -> Result<ChartOptions, String> {
    let orientation = orientation(p)?.unwrap_or(base.orientation);
    let values = match p.get("values").filter(|v| !v.is_null()) {
        None => base.values,
        Some(v) => super::bool_value(v).ok_or("values must be true or false")?,
    };
    let decimals = match p.get("decimals").filter(|v| !v.is_null()) {
        None => base.decimals,
        Some(v) if v.as_str().is_some_and(|t| t.trim().eq_ignore_ascii_case("auto")) => None,
        Some(v) => Some(
            v.as_u64()
                .filter(|n| *n <= MAX_CHART_DECIMALS as u64)
                .map(|n| n as u8)
                .ok_or_else(|| format!("decimals is \"auto\" or a whole number from 0 to {MAX_CHART_DECIMALS}"))?,
        ),
    };
    // Null gives it back the title it is given, as it gives the bars their default colour.
    let title = match p.get("title") {
        None => base.title,
        Some(Value::Null) => None,
        Some(v) => Some(super::within(
            "title",
            "a chart's title",
            v.as_str().ok_or("a chart's title is a text, \"\" for none, or null for the title it is given")?,
            200,
        )?),
    };
    let bar_color = match p.get("barColor") {
        None => base.bar_color,
        Some(Value::Null) => None,
        Some(_) => Some(color(p, "barColor", CHART_BAR)?),
    };
    // Columns centre their labels whatever it says, so a centre has nothing to set.
    let label_align = match p.get("labelAlign").filter(|v| !v.is_null()) {
        None => base.label_align,
        Some(v) => v.as_str().and_then(TextAlign::from_name).filter(|a| *a != TextAlign::Center).ok_or("labelAlign is left or right")?,
    };
    // Coloured by classes, it reads its layer's colours when drawn, not now.
    let classes = match p.get("classes").filter(|v| !v.is_null()) {
        None => base.classes,
        Some(v) => super::bool_value(v).ok_or("classes must be true or false")?,
    };
    Ok(ChartOptions { orientation, values, decimals, title, bar_color, label_align, classes, ..base })
}

/// A stack field a chart of `o` can't draw ([`rustgis_map::layout::chart_stack_refusal`], #2487
/// part 3b): refused when `p` gives it, let go when it is kept from before and `p` changes what it
/// needs, which is returned to be said.
fn stack_or_refuse(p: &Value, o: &mut ChartOptions) -> Result<Option<String>, String> {
    let Some(st) = o.stack.as_deref() else { return Ok(None) };
    let Some(why) = rustgis_map::layout::chart_stack_refusal(o.group.as_deref(), o.statistic, st) else { return Ok(None) };
    if gives(p, "stack") {
        return Err(format!("the chart can't stack by '{st}': {why} (it stacks grouped by another field, summing or counting)"));
    }
    let said = format!("the chart no longer stacks by '{st}': {why}");
    o.stack = None;
    Ok(Some(said))
}

/// Whether a chart of these options charts `field`: a number, or any field when it groups and
/// counts, which reads none (ungrouped, its statistic isn't read).
fn chart_takes(field: &rustgis_data::Field, o: &ChartOptions) -> bool {
    field.ty.is_numeric() || o.group.is_some() && o.statistic == StatKind::Count
}

/// Refuses a chart field that [`chart_takes`] doesn't.
fn chart_measures(field: &rustgis_data::Field, o: &ChartOptions) -> Result<(), String> {
    if chart_takes(field, o) {
        return Ok(());
    }
    Err(format!("'{}' isn't a numeric field: a chart measures a number, unless it groups and counts", field.name))
}

/// The features of `layer` in the map of the frame `frame`, as a chart on it draws them.
fn frame_dataset<'a>(s: &'a Session, l: &Layout, frame: u32, layer: &str) -> Option<&'a rustgis_data::Dataset> {
    frame_layer(s, l, frame, layer).map(|l| &l.dataset)
}

/// The layer `layer` in the map of the frame `frame`, with its symbology for a chart coloured by it.
fn frame_layer<'a>(s: &'a Session, l: &Layout, frame: u32, layer: &str) -> Option<&'a rustgis_map::Layer> {
    let Some(ElementKind::MapFrame { map, .. }) = l.element(frame).map(|e| &e.kind) else { return None };
    s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map))?.layer_by_name(layer)
}

fn is_chart(s: &Session, li: usize, id: u32) -> bool {
    matches!(s.project.layouts.get(li).and_then(|l| l.element(id)).map(|e| &e.kind), Some(ElementKind::Chart { .. }))
}

/// A chart's options after `layout.update`, the field it now measures when it changes, and what it
/// says.
type ChartChange = (ChartOptions, Option<String>, Vec<String>);

/// A chart's options after `layout.update` (`p`) with the layer and field it will chart
/// (`checked`, from [`checked_update`]), and what it says of the label or group it loses on a
/// new layer without them; `None` when `p` changes neither. A field `p` doesn't name that the
/// chart no longer measures (ungrouped, or no longer counting) gives way to the layer's first
/// numeric field, which is said and returned. Look options alone read nothing of its layer.
/// Chart options given for another element are refused.
fn chart_update(
    s: &Session,
    p: &Value,
    li: usize,
    id: u32,
    checked: Option<&(String, Option<String>)>,
) -> Result<Option<ChartChange>, String> {
    let l = s.project.layouts.get(li).ok_or("no layout")?;
    let e = l.element(id).ok_or_else(|| format!("no element {id}"))?;
    let ElementKind::Chart {
        frame,
        layer,
        field,
        label,
        group,
        statistic,
        order,
        bars,
        other,
        orientation,
        values,
        decimals,
        title,
        bar_color,
        label_align,
        classes,
        stack,
        ..
    } = &e.kind
    else {
        return if gives_chart_options(p) { Err(ONLY_CHART_OPTIONS.into()) } else { Ok(None) };
    };
    if checked.is_none() && !gives_chart_options(p) && !changes_chart_look(p) {
        return Ok(None);
    }
    let look = ChartOptions {
        label: label.clone(),
        group: group.clone(),
        statistic: *statistic,
        order: *order,
        bars: *bars,
        other: *other,
        orientation: *orientation,
        values: *values,
        decimals: *decimals,
        title: title.clone(),
        bar_color: *bar_color,
        label_align: *label_align,
        classes: *classes,
        stack: stack.clone(),
    };
    // Told only how to draw, it reads nothing of its layer, which may be gone.
    if checked.is_none() && !gives_chart_data(p) {
        return Ok(Some((chart_look_options(p, look)?, None, Vec::new())));
    }
    let stored = field.as_str();
    let (layer, field) = match checked {
        Some((name, f)) => (name.as_str(), f.as_deref().unwrap_or(stored)),
        None => (layer.as_str(), stored),
    };
    let ds = frame_dataset(s, l, *frame, layer).ok_or_else(|| format!("no feature layer '{layer}' in the frame's map"))?;
    // On a new layer, a label, group or stack field it hasn't is let go, and said.
    let mut said = Vec::new();
    let mut keep = |name: &Option<String>, what: &str| {
        let n = name.as_deref()?;
        let kept = ds.field_index(n).and_then(|i| ds.fields.get(i)).map(|f| f.name.clone());
        if kept.is_none() {
            said.push(format!("{layer} has no field '{n}': the chart no longer {what}"));
        }
        kept
    };
    // A label, group or stack the update itself sets or takes away isn't said to be lost.
    let base = ChartOptions {
        label: if gives(p, "label") { None } else { keep(label, "labels its bars with it") },
        group: if gives(p, "group") { None } else { keep(group, "groups by it") },
        stack: if gives(p, "stack") { None } else { keep(stack, "stacks by it") },
        ..look
    };
    let mut o = chart_options(p, base, ds)?;
    said.extend(stack_or_refuse(p, &mut o)?);
    let f = ds.field_index(field).and_then(|i| ds.fields.get(i)).ok_or_else(|| format!("no field '{field}' in {layer}"))?;
    if gives(p, "field") || chart_takes(f, &o) {
        chart_measures(f, &o)?;
        // A field picked for it (on a new layer without one it can measure) is said too.
        if !gives(p, "field") && field != stored {
            said.push(format!("{layer} has no field '{stored}' the chart can measure: it now measures {field}"));
        }
        return Ok(Some((o, None, said)));
    }
    let to = ds.fields.iter().find(|f| f.ty.is_numeric()).ok_or_else(|| {
        format!("'{field}' isn't a numeric field and {layer} has none: a chart measures a number, unless it groups and counts")
    })?;
    said.push(format!("'{field}' isn't a number: the chart now measures {}", to.name));
    Ok(Some((o, Some(to.name.clone()), said)))
}

/// What a chart leaves out of what it is told to draw (an existing loss made said): the bars
/// past its limit, and the features with no number in its field; coloured by classes, the bars
/// that keep its bar colour, their features having several, and stacked, the values of its key
/// gathered in its Other or keeping their palette colour. It reads each feature of the
/// chart's layer once, on the command's thread, as drawing the chart does on every draw of the
/// page: run only when a chart is added, told what to draw or coloured by classes, not when it
/// is moved; resized or restyled, a stacked chart reads it for [`say_chart_crowded`] alone.
fn say_chart_losses(s: &mut Session, li: usize, id: u32) {
    say_chart(s, li, id, false);
}

/// [`say_chart_losses`]' warning of a chart whose title and key leave its bars no room, alone.
fn say_chart_crowded(s: &mut Session, li: usize, id: u32) {
    say_chart(s, li, id, true);
}

/// [`say_chart_losses`], or `crowded_only` [`say_chart_crowded`].
fn say_chart(s: &mut Session, li: usize, id: u32, crowded_only: bool) {
    let said = {
        let Some(l) = s.project.layouts.get(li) else { return };
        let Some(e) = l.element(id) else { return };
        let ElementKind::Chart { frame, layer, field, .. } = &e.kind else { return };
        // Only a stacked chart can be crowded, so for that alone an unstacked one reads nothing.
        if crowded_only && e.kind.chart_stack().is_none() {
            return;
        }
        // Read as the page draws it: at the frame's scale and on the series page shown.
        let Some((m, env)) = rustgis_render::layout::frame_page_env(l, &s.project.maps, *frame) else { return };
        let Some(data) = m.layer_by_name(layer).and_then(|fl| e.kind.chart_data_of(fl, &env)) else { return };
        let mut said = if crowded_only { Vec::new() } else { chart_losses(&e.kind, &data, layer, field) };
        if rustgis_render::layout::chart_crowded(&data, &e.kind.chart_look().unwrap_or_default(), e.w, e.h) {
            said.push("the chart's title and key leave its bars no room: it draws as a placeholder until its box is taller".to_string());
        }
        said
    };
    for w in said {
        s.warn(w);
    }
}

/// What [`say_chart_losses`] says of the chart `kind` of `layer` and `field`, drawing `data`.
fn chart_losses(kind: &ElementKind, data: &rustgis_map::layout::ChartData, layer: &str, field: &str) -> Vec<String> {
    let mut said = Vec::new();
    if data.left_out > 0 {
        let drawn = data.bars.len();
        said.push(format!(
            "the chart draws {drawn} of {} bars: {} left out (\"bars\" draws up to {MAX_CHART_BARS}, \"other\": true gathers the rest in an Other bar)",
            drawn + data.left_out,
            data.left_out
        ));
    }
    if data.out_of_range > 0 {
        let n = data.out_of_range;
        said.push(format!(
            "{n} bar{} of the chart left out: {} statistic is past a number's range",
            if n == 1 { "" } else { "s" },
            if n == 1 { "its" } else { "their" }
        ));
    }
    if data.no_number > 0 {
        let n = data.no_number;
        said.push(format!("{n} feature{} of {layer} with no number in {field} left out of the chart", if n == 1 { "" } else { "s" }));
    }
    // Coloured by classes, the bars that keep the bar colour all the same.
    for (n, why) in [(data.mixed, "have several colours"), (data.unshown, "show no colour")] {
        if n > 0 {
            let (plural, its) = if n == 1 { ("", "its") } else { ("s", "their") };
            let keep = if n == 1 { "keeps" } else { "keep" };
            said.push(format!("{n} bar{plural} {keep} the chart's bar colour: {its} features {why} on the map"));
        }
    }
    // Stacked, the values gathered in its key's Other, and coloured by classes, those that keep
    // their palette colour.
    if data.key_gathered > 0 {
        let n = data.key_gathered;
        let named = data.key.len().saturating_sub(1);
        let stack = kind.chart_stack().unwrap_or_default();
        said.push(format!(
            "the chart's key names {named} of the {} values of {stack} on its bars: {n} {} gathered in its grey Other",
            named + n,
            if n == 1 { "is" } else { "are" }
        ));
    }
    if data.key_kept > 0 {
        let n = data.key_kept;
        let (plural, its, keep) = if n == 1 { ("", "its", "keeps") } else { ("s", "their", "keep") };
        said.push(format!(
            "{n} value{plural} of the chart's key {keep} {its} palette colour: {its} features have several colours on the map, or show none"
        ));
    }
    if data.stack_gone {
        let stack = kind.chart_stack().unwrap_or_default();
        said.push(format!("the chart draws its bars whole: {layer} has no field '{stack}' to stack them by"));
    }
    if data.whole > 0 {
        let n = data.whole;
        let (plural, is, its) = if n == 1 { ("", "is", "its") } else { ("s", "are", "their") };
        said.push(format!("{n} bar{plural} of the chart {is} drawn whole, not stacked: {its} stretches add up past the largest number"));
    }
    said
}

/// What a `font` given for an element that isn't a text is told.
const ONLY_TEXT_FONT: &str = "only text elements take a font or a fallback";

/// What a `dash` given for an element that draws no line or outline is told.
const ONLY_DASH: &str = "only lines, arrows, rectangles, ellipses and circles take a dash";

/// A line's or an outline's `dash` when given, read as a line layer's stroke reads it
/// (`layer.lineStrokes`): `Some(None)` for solid (`null` or `[]`), `Some(Some(_))` for a pattern,
/// `None` when not given; refused when it isn't one.
fn element_dash(p: &Value) -> Result<Option<Option<Dash>>, String> {
    let bad = || {
        format!(
            "dash is a list of 1 to {MAX_DASH_STEPS} lengths in points (dash, gap, …), each 0 to {MAX_DASH_STEP}, not all 0; [] or null for solid"
        )
    };
    match p.get("dash") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::Array(a)) if a.is_empty() => Ok(Some(None)),
        Some(Value::Array(a)) => {
            let steps: Vec<f32> = a.iter().map(|v| v.as_f64().map(|x| x.clamp(-1e6, 1e6) as f32)).collect::<Option<_>>().ok_or_else(bad)?;
            Dash::new(&steps).map(|d| Some(Some(d))).ok_or_else(bad)
        }
        Some(_) => Err(bad()),
    }
}

/// Rounded corners are a rectangle's: refused elsewhere, as a family is, so a script doesn't take
/// them as applied.
const ONLY_RECTANGLE_RADIUS: &str = "only a rectangle rounds its corners";

/// Colours are refused for the elements that don't draw them, as a family is, so a script doesn't
/// take them as applied: "color" is a text's, a line's, an arrow's, a scale bar's or a shape's
/// outline (a rectangle's or an ellipse's), a legend's, colorbar's, table's or chart's ink (#2257),
/// and, when adding, also an extent indicator's or a grid's.
const ONLY_COLOR_ADDED: &str = "only text, lines, arrows, rectangles, ellipses, circles, extent indicators, grids, scale bars, \
legends, colorbars, tables and charts take a color";
const ONLY_COLOR_UPDATED: &str =
    "only text, lines, arrows, rectangles, ellipses, circles, scale bars, legends, colorbars, tables and charts take a color";
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
    let Some(v) = p.get("exclude").filter(|v| !v.is_null()) else { return Ok(None) };
    let names = v.as_array().ok_or("exclude is a list of layer names")?;
    if names.len() > rustgis_map::layout::MAX_LEGEND_EXCLUDED {
        return Err(format!("a legend leaves out at most {} layers", rustgis_map::layout::MAX_LEGEND_EXCLUDED));
    }
    let map = legend_map(input, l, frame)?;
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let n = n.as_str().ok_or("exclude is a list of layer names")?;
        let name = legend_layer(map, n)?;
        if !out.iter().any(|o| o.eq_ignore_ascii_case(name)) {
            out.push(name.clone());
        }
    }
    Ok(Some(out))
}

/// The map a legend's frame shows.
fn legend_map<'m>(input: &'m impl super::InputSource, l: &Layout, frame: u32) -> Result<&'m Map, String> {
    let context = input.input_context();
    let s = &context;
    let map_name = match l.element(frame).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { map, .. }) => map,
        _ => return Err("the legend's frame isn't a map frame".into()),
    };
    s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map_name)).ok_or_else(|| format!("no map '{map_name}'"))
}

/// The name of the layer `n` (any case, ends trimmed) of a legend's `map`, as the map spells it:
/// one of its layers, or a WMS or WMTS layer's (whose legend is its service's picture). One the
/// map hasn't is an error.
fn legend_layer<'m>(map: &'m Map, n: &str) -> Result<&'m String, String> {
    map.layer_by_name(n.trim())
        .map(|l| &l.name)
        .or_else(|| map.tiles.iter().find(|t| t.name.eq_ignore_ascii_case(n.trim())).map(|t| &t.name))
        .ok_or_else(|| format!("no layer '{n}' in map '{}'", map.name))
}

/// The shape a legend's `labels` takes.
const LABELS_SHAPE: &str = r#"labels is an object of layer names and labels: {"<layer>": "<label>" | null}"#;

/// A legend's `labels` from `p` (#1654): each layer's name, as its frame's map spells it, with the
/// label to show for it, or `None` to show its name again (a `null` or blank label). `None` when
/// not given; an empty list for `{}`, which shows every layer's name again. Not an object of
/// labels, a name the map hasn't, or a layer named twice is an error.
fn legend_labels(
    input: &impl super::InputSource,
    l: &Layout,
    frame: u32,
    p: &Value,
) -> Result<Option<Vec<(String, Option<String>)>>, String> {
    let Some(v) = p.get("labels").filter(|v| !v.is_null()) else { return Ok(None) };
    let given = v.as_object().ok_or(LABELS_SHAPE)?;
    if given.len() > MAX_LEGEND_LABELS {
        return Err(format!("a legend can label at most {MAX_LEGEND_LABELS} layers"));
    }
    let map = legend_map(input, l, frame)?;
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for (n, label) in given {
        let label = match label {
            Value::Null => None,
            Value::String(t) if t.trim().is_empty() => None,
            // Refused rather than cut, so a label is never shown other than as given.
            Value::String(t) if t.chars().count() > MAX_LEGEND_LABEL_CHARS => {
                return Err(format!("a legend's label is at most {MAX_LEGEND_LABEL_CHARS} characters ('{n}' has {})", t.chars().count()));
            }
            Value::String(t) => Some(t.clone()),
            _ => return Err(LABELS_SHAPE.into()),
        };
        let name = legend_layer(map, n)?;
        // Which of two spellings of a name counts is not the caller's to guess: refused.
        if out.iter().any(|(o, _)| o.eq_ignore_ascii_case(name)) {
            return Err(format!("labels names layer '{name}' twice"));
        }
        out.push((name.clone(), label));
    }
    Ok(Some(out))
}

/// A legend's `labels` with those `given` ([`legend_labels`]) put in: each layer's label replaced,
/// or dropped for `None`; none at all for an empty list. More than a legend keeps is an error.
fn with_labels(labels: &BTreeMap<String, String>, given: Vec<(String, Option<String>)>) -> Result<BTreeMap<String, String>, String> {
    let mut out = if given.is_empty() { BTreeMap::new() } else { labels.clone() };
    for (name, label) in given {
        out.retain(|n, _| !n.eq_ignore_ascii_case(&name));
        if let Some(t) = label {
            out.insert(name, t);
        }
    }
    if out.len() > MAX_LEGEND_LABELS {
        return Err(format!("a legend can label at most {MAX_LEGEND_LABELS} layers"));
    }
    Ok(out)
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
        ElementKind::Rectangle { fill, outline, width, dash, .. } | ElementKind::Ellipse { fill, outline, width, dash } => {
            if ["fill", "outline", "color", "width", "radius", "dash"].iter().all(|k| p.get(k).is_none()) {
                return Ok(None);
            }
            let dash = element_dash(p)?.unwrap_or(*dash);
            // The outline's colour is "outline", or "color" as when adding one.
            let (fill, outline) = (color(p, "fill", *fill)?, color(p, "outline", color(p, "color", *outline)?)?);
            let width = shape_width(p, *width)?.clamp(0.0, 50.0);
            Ok(Some(match kind {
                ElementKind::Rectangle { radius, .. } => {
                    ElementKind::Rectangle { fill, outline, width, radius: corner_radius(p, *radius)?, dash }
                }
                _ => ElementKind::Ellipse { fill, outline, width, dash },
            }))
        }
        ElementKind::Line { color: c, width, dir, head, dash } => {
            if ["color", "width", "dir", "head", "dash"].iter().all(|k| p.get(k).is_none()) {
                return Ok(None);
            }
            let dash = element_dash(p)?.unwrap_or(*dash);
            let dir = line_dir(p, *dir)?;
            let head = line_head(p, *head)?;
            let width = shape_width(p, *width)?.clamp(0.1, 50.0);
            Ok(Some(ElementKind::Line { color: color(p, "color", *c)?, width, dir, head, dash }))
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
    // A name, text or title over its limit is refused, not cut.
    let new_name = super::name_given(p, "an element's name", 200)?;
    let new_text = ps(p, "text").map(|t| super::within("text", "a text element's text", t, 5000)).transpose()?;
    let new_title = ps(p, "title").map(|t| super::within("title", "a legend's, colorbar's or chart's title", t, 200)).transpose()?;
    let li = index(s, p)?;
    // What a publication's scale bar box was measured from, before this change.
    let basis = scale_box_basis(s, li);
    // Locked, it takes no change but being unlocked, which may come with other changes.
    // "dash": null makes a line solid, "background": null takes the background away, and
    // "border": null gives a map frame its default border,
    // "color": null gives a legend, colorbar, table or chart its default ink, and "title" or
    // "barColor": null a chart its given title or default bars ([`ElementKind::null_resets`]), so
    // they are changes too.
    let current = s.project.layouts.get(li).and_then(|l| l.element(id)).map(|e| &e.kind);
    let null_changes = |k: &str| matches!(k, "dash" | "background" | "border") || current.is_some_and(|kind| kind.null_resets(k));
    let changes = p.as_object().is_some_and(|o| {
        o.iter().any(|(k, v)| (!v.is_null() || null_changes(k.as_str())) && !matches!(k.as_str(), "layout" | "element" | "locked"))
    });
    if changes && b(p, "locked") != Some(false) {
        unlocked(s.project.layouts.get(li).ok_or("no layout")?, id)?;
    }
    // A new layer (tables and charts) or field (charts) must exist in the frame's map.
    let checked = checked_update(s, p, li, id)?;
    // A chart's options, read over its own on the layer it will chart.
    let (new_chart, chart_field, chart_said) = match chart_update(s, p, li, id, checked.as_ref())? {
        Some((o, f, said)) => (Some(o), f, said),
        None => (None, None, Vec::new()),
    };
    // A colorbar's new layer must be a raster in its frame's map; empty follows the top visible
    // raster with a colour scale, not drawn by classes.
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
    // Set back to no layer: it follows the map's default, which may have nothing to show (#1679).
    let emptied_colorbar = matches!(&colorbar, Some((Some(None), _, _)));
    // A north arrow's or scale bar's new look, and a map frame's grid: `"grid": false` removes it,
    // `true` adds the default one, an object changes the options it names, and null (not set)
    // leaves it as it is.
    let held;
    let (restyled, new_grid, new_indicator, new_border) = {
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
        // `"border": null` gives the frame the default border back; an object changes the colour
        // and width it names, a width of 0 drawing none (#2557).
        let border = match (kind, p.get("border")) {
            (_, None) => None,
            (ElementKind::MapFrame { .. }, Some(Value::Null)) => {
                Some((rustgis_map::layout::FRAME_BORDER_COLOR, rustgis_map::layout::FRAME_BORDER_WIDTH))
            }
            (ElementKind::MapFrame { .. }, Some(v @ Value::Object(_))) => Some(border_style(v, kind)?),
            (ElementKind::MapFrame { .. }, Some(_)) => {
                return Err("border is an object of color and width (points, 0 for none), or null for the default".into());
            }
            (_, Some(_)) => return Err("only map frames have a border".into()),
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
        let mut said = held_to_limits(p, Shape::of_kind(kind));
        if let (ElementKind::MapFrame { .. }, Some(Some(_))) = (kind, &indicator)
            && let Some(v) = p.get("extentIndicator")
        {
            let range = (rustgis_map::layout::MIN_INDICATOR_WIDTH, rustgis_map::layout::MAX_INDICATOR_WIDTH);
            said.extend(held_width(v, "extent indicator width", range));
        }
        if let (ElementKind::MapFrame { .. }, Some(v @ Value::Object(_))) = (kind, p.get("border")) {
            said.extend(held_width(v, "border width", (0.0, rustgis_map::layout::MAX_FRAME_BORDER_WIDTH)));
        }
        held = said;
        (restyle(kind, p)?, grid, indicator, border)
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
        && !kind_now.is_some_and(ElementKind::takes_ink)
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
    // A colour that isn't text is refused, as `color` does when adding.
    let col = given_color(p, "color")?;
    // A text element's italic, alignment and spacing, checked before anything changes.
    let new_text_style = match s.project.layouts.get(li).and_then(|l| l.element(id)).map(|e| &e.kind) {
        Some(ElementKind::Text { italic, align, letter_spacing, font, font_name, .. }) => {
            Some((text_style(p, *italic, *align, *letter_spacing)?, text_font(p, (*font, font_name.clone()))?))
        }
        _ if gives_font(p) => return Err(ONLY_TEXT_FONT.into()),
        _ => None,
    };
    let new_rotation = match text_rotation(p)? {
        Some(_) if !matches!(kind_now, Some(ElementKind::Text { .. })) => return Err(ONLY_TEXT_ROTATION.into()),
        r => r,
    };
    let new_wrap = match text_wrap(p)? {
        Some(_) if !matches!(kind_now, Some(ElementKind::Text { .. })) => return Err(ONLY_TEXT_WRAP.into()),
        w => w,
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
    // `null` too: it makes a line solid, so elsewhere it is refused rather than ignored.
    if p.get("dash").is_some()
        && !matches!(
            s.project.layouts.get(li).and_then(|l| l.element(id)).map(|e| &e.kind),
            Some(ElementKind::Line { .. } | ElementKind::Rectangle { .. } | ElementKind::Ellipse { .. })
        )
    {
        return Err(ONLY_DASH.into());
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
    // And its labels, put in over those it has.
    let new_labels = {
        let l = s.project.layouts.get(li).ok_or("no layout")?;
        match l.element(id).map(|e| &e.kind) {
            Some(ElementKind::Legend { frame, labels, .. }) => match legend_labels(s, l, *frame, p)? {
                Some(given) => Some(with_labels(labels, given)?),
                None => None,
            },
            // A map frame's grid takes its labels under "grid", as its other options: said where.
            Some(ElementKind::MapFrame { .. }) if p.get("labels").is_some_and(|v| !v.is_null()) => {
                return Err(r#"a map frame's grid labels go under "grid": {"grid": {"labels": false}}"#.into());
            }
            _ if p.get("labels").is_some_and(|v| !v.is_null()) => return Err(ONLY_LEGEND_LABELS.into()),
            _ => None,
        }
    };
    // Its background (#2257): the keys given over those it has, or none with null.
    let new_background = {
        let e = s.project.layouts.get(li).and_then(|l| l.element(id)).ok_or_else(|| format!("no element {id}"))?;
        match element_background(p, e.background)? {
            BackgroundParam::Set(_) if !e.kind.takes_background() => return Err(NO_FRAME_BACKGROUND.into()),
            BackgroundParam::Set(b) => Some(Some(b)),
            BackgroundParam::Remove => Some(None),
            BackgroundParam::Absent => None,
        }
    };
    let new_columns = legend_columns_param(p)?;
    let is_legend = s.project.layouts.get(li).and_then(|l| l.element(id)).is_some_and(|e| matches!(e.kind, ElementKind::Legend { .. }));
    if p.get("textSize").is_some_and(|v| !v.is_null()) && !is_legend {
        return Err(ONLY_LEGEND_TEXT_SIZE.into());
    }
    let new_text_size = legend_text_size_param(p)?;
    if p.get("showNoData").is_some_and(|v| !v.is_null()) && !is_legend {
        return Err(ONLY_LEGEND_NO_DATA.into());
    }
    if new_columns.is_some() && !is_legend {
        return Err(ONLY_LEGEND_COLUMNS.into());
    }
    let l = active(s, p)?;
    let e = l.element_mut(id).ok_or_else(|| format!("no element {id}"))?;
    // A text with no ink isn't measured: given a colour that shows it, it is told (#2346).
    let shown = col.is_some_and(|c| c.a() > 0) && matches!(&e.kind, ElementKind::Text { color, .. } if color.a() == 0);
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
    if let Some(n) = new_name {
        e.name = n;
    }
    if let Some(v) = b(p, "visible") {
        e.visible = v;
    }
    if let Some(v) = b(p, "locked") {
        e.locked = v;
    }
    if let Some(v) = new_background {
        e.background = v;
    }
    // A legend's, colorbar's, table's or chart's ink (#2257); null gives it the default again.
    if p.get("color").is_some()
        && let Some(ink) = e.kind.ink_mut()
    {
        *ink = col;
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
        ElementKind::Text { text, size, color, bold, italic, align, letter_spacing, font, font_name, rotation, wrap } => {
            if let Some(((i, a, l), (f, n))) = new_text_style {
                (*italic, *align, *letter_spacing, *font, *font_name) = (i, a, l, f, n);
            }
            if let Some(r) = new_rotation {
                *rotation = r;
            }
            if let Some(w) = new_wrap {
                *wrap = w;
            }
            if let Some(t) = new_text {
                *text = t;
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
        ElementKind::Legend { title, all_classes, hide_other, show_no_data, exclude, columns, labels, text_size, .. } => {
            if let Some(names) = new_exclude {
                *exclude = names;
            }
            if let Some(pt) = new_text_size {
                *text_size = pt;
            }
            if let Some(given) = new_labels {
                *labels = given;
            }
            if let Some(n) = new_columns {
                *columns = n;
            }
            if let Some(t) = new_title {
                *title = t;
            }
            if let Some(v) = b(p, "allClasses") {
                *all_classes = v;
            }
            if let Some(v) = b(p, "hideOther") {
                *hide_other = v;
            }
            if let Some(v) = b(p, "showNoData") {
                *show_no_data = v;
            }
        }
        ElementKind::Colorbar { layer, title, orientation, ticks: n, tick_side, reverse, .. } => {
            if let Some(t) = new_title {
                *title = t;
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
        ElementKind::MapFrame {
            map,
            center,
            scale,
            grid,
            bearing,
            pitch,
            extent_of,
            extent_color,
            extent_width,
            border_color,
            border_width,
            ..
        } => {
            match new_indicator {
                Some(None) => *extent_of = None,
                Some(Some((c, w))) => (*extent_color, *extent_width) = (c, w),
                None => {}
            }
            if let Some((c, w)) = new_border {
                (*border_color, *border_width) = (c, w);
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
        ElementKind::Chart {
            field,
            layer,
            label,
            group,
            statistic,
            order,
            bars,
            other,
            orientation,
            values,
            decimals,
            title,
            bar_color,
            label_align,
            classes,
            stack,
            ..
        } => {
            if let Some((l, f)) = &checked {
                *layer = l.clone();
                if let Some(f) = f {
                    *field = f.clone();
                }
            }
            if let Some(o) = new_chart {
                (*label, *group, *statistic, *order, *bars, *other) = (o.label, o.group, o.statistic, o.order, o.bars, o.other);
                (*orientation, *values, *decimals, *title, *bar_color, *label_align) =
                    (o.orientation, o.values, o.decimals, o.title, o.bar_color, o.label_align);
                (*classes, *stack) = (o.classes, o.stack);
            }
            if let Some(f) = chart_field {
                *field = f;
            }
        }
        _ => {}
    }
    // Told what it leaves out, or whether it lists every class, it says again which rasters it
    // can't key.
    let excluding =
        matches!(&e.kind, ElementKind::Legend { .. }) && (p.get("exclude").is_some_and(|v| !v.is_null()) || b(p, "allClasses").is_some());
    let fit = tells_legend_fit(&e.kind, p);
    if fit {
        say_legend_fit(s, li, id);
    }
    if excluding {
        say_raster_keys(s, li, id);
    }
    // A chart told what to draw, or coloured by classes, says what it leaves out of it.
    if !chart_said.is_empty() || ((checked.is_some() || gives_chart_data(p) || b(p, "classes") == Some(true)) && is_chart(s, li, id)) {
        for said in chart_said {
            s.warn(said);
        }
        say_chart_losses(s, li, id);
    } else if (gives(p, "w") || gives(p, "h") || CHART_ROOM_KEYS.iter().any(|k| p.get(*k).is_some())) && is_chart(s, li, id) {
        // Resized, retitled (`null` too, its given title back), turned or given values, it says
        // only whether its bars still have room.
        say_chart_crowded(s, li, id);
    }
    if emptied_colorbar {
        let frame = s.project.layouts.get(li).and_then(|l| match l.element(id).map(|e| &e.kind) {
            Some(ElementKind::Colorbar { frame, layer: None, .. }) => Some(*frame),
            _ => None,
        });
        if let Some(said) = frame.and_then(|frame| colorbar_without_raster(s, li, frame)) {
            s.warn(said);
        }
    }
    if let Some(text) = p.get("grid").and_then(|grid| grid.get("crs")).and_then(Value::as_str) {
        super::say_crs_note(s, text);
    }
    // Its frame drawn at another scale or width, or the text under its bar changed: a
    // publication's scale bar box measured again.
    if let Some(basis) = basis {
        let given = |keys: &[&str]| keys.iter().any(|k| p.get(*k).is_some_and(|v| !v.is_null()));
        let restyled = id == basis.bar_id && given(&["units", "divisions"]);
        if (id == basis.frame && given(&["scale", "w"])) || (id == basis.text_id && given(&["text", "size", "bold"])) || restyled {
            refit_scale_box(s, li, &basis);
        }
    }
    // After the scale box is refitted: a text is measured in the box it is saved with.
    if shown || TEXT_FIT_KEYS.iter().any(|k| p.get(*k).is_some_and(|v| !v.is_null())) {
        say_text_fit(s, li, id);
    }
    say_held(s, "Element Properties", held);
    if let Some(w) = font_told {
        return Ok(json!({"warning": w}));
    }
    Ok(Value::Null)
}

/// The width (mm) of a publication's scale bar box (#1379), for a frame `frame_w` mm wide at
/// `scale` and `ground_per_pt` ground metres a point: the bar a box 30 % of the frame's width (25
/// mm at least) would draw sets its length and divisions, and the box is the narrowest that draws
/// that same bar (at least its length over the 85 % of a box the renderer gives a bar), its last
/// label included, and holds the text written under it: `text` (its `{scale}` filled), in
/// `text_pt` points, bold or not.
fn scale_box_width(ground_per_pt: f64, scale: f64, frame_w_mm: f64, bar: (ScaleUnits, usize), comma: bool, text: (&str, f64, bool)) -> f64 {
    let (text, text_pt, bold) = text;
    let (widest, pt_per_mm) = ((frame_w_mm * 0.3).max(25.0), rustgis_map::layout::PT_PER_MM);
    let bar_w = crate::render::layout::scale_bar_width_pt(ground_per_pt, widest * pt_per_mm, bar, comma, 1.5 * pt_per_mm)
        .map_or(widest, |w| w / pt_per_mm);
    let written = text.replace("{scale}", &crate::render::layout::scale_ratio_text(scale, comma));
    bar_w.max(scale_text_mm(&written, text_pt, bold)).min(widest)
}

/// The width (mm) a scale bar box gives the text under its bar, its tokens filled: its longest
/// line in `text_pt` points, bold or not, and a millimetre each side.
fn scale_text_mm(text: &str, text_pt: f64, bold: bool) -> f64 {
    text.lines().map(|line| crate::render::report::width_mm(line, text_pt as f32, bold)).fold(0.0, f64::max) + 2.0
}

/// What a publication places over its map, which a scale bar box measured again must stay off.
const PLACED_OVER_MAP: [&str; 3] = ["Legend Box", "Locator", "North Arrow"];

/// What a publication's scale bar box is measured from: its frame (id, centre, scale, width), its
/// bar (id, units, divisions) and the text under the bar (id, text, size, boldness).
#[derive(Clone)]
struct BoxBasis {
    frame: u32,
    center: Coord,
    scale: f64,
    frame_w: f64,
    bar_id: u32,
    units: ScaleUnits,
    divisions: usize,
    text_id: u32,
    text: String,
    size: f64,
    bold: bool,
}

/// The text under a publication's scale bar as the page draws it from `b`: `{scale}` filled as
/// `layout.publication` fills it, the other tokens from the frame's map.
fn basis_text(l: &Layout, m: &rustgis_map::Map, b: &BoxBasis) -> String {
    let info = crate::render::layout::PageInfo { page: 1, pages: 1, page_name: String::new() };
    let cam = rustgis_map::layout::frame_camera(m, b.center, b.scale, 0.0, 0.0);
    let text = b.text.replace("{scale}", &crate::render::layout::scale_ratio_text(b.scale, l.decimal_comma));
    crate::render::layout::expand(&text, l, &info, Some((m, cam)))
}

/// The automatic width (mm) of a publication's scale bar box measured from `b`: the one
/// `layout.publication` gives it for that frame, bar and text.
fn basis_width(l: &Layout, m: &rustgis_map::Map, b: &BoxBasis) -> f64 {
    let ground_per_pt = m.ground_res_at(&rustgis_map::layout::frame_camera(m, b.center, b.scale, 0.0, 0.0));
    let filled = basis_text(l, m, b);
    scale_box_width(ground_per_pt, b.scale, b.frame_w, (b.units, b.divisions), l.decimal_comma, (&filled, b.size, b.bold))
}

/// The automatic width (mm) of layout `li`'s publication scale bar box as things are now.
#[cfg(test)]
pub(crate) fn scale_box_auto(s: &Session, li: usize) -> Option<f64> {
    let b = scale_box_basis(s, li)?;
    let l = s.project.layouts.get(li)?;
    let ElementKind::MapFrame { map, .. } = &l.element(b.frame)?.kind else { return None };
    let m = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map))?;
    Some(basis_width(l, m, &b))
}

/// The basis of a layout's publication scale bar box as it is now (#1379); `None` without one.
fn scale_box_basis(s: &Session, li: usize) -> Option<BoxBasis> {
    let l = s.project.layouts.get(li)?;
    let named = |name: &str| l.elements.iter().find(|e| e.name == name);
    let (bar, written) = (named("Scale Bar")?, named("Scale")?);
    let (ElementKind::ScaleBar { frame, units, divisions, .. }, ElementKind::Text { text, size, bold, .. }) = (&bar.kind, &written.kind)
    else {
        return None;
    };
    let fr = l.element(*frame)?;
    let ElementKind::MapFrame { center, scale, .. } = &fr.kind else { return None };
    Some(BoxBasis {
        frame: *frame,
        center: *center,
        scale: *scale,
        frame_w: fr.w,
        bar_id: bar.id,
        units: *units,
        divisions: *divisions,
        text_id: written.id,
        text: text.clone(),
        size: *size,
        bold: *bold,
    })
}

/// A publication's scale bar box measured again after its frame's scale or width changed (#1379):
/// the "Scale Box", the "Scale Bar" of `frame` and the "Scale" under it, as a publication lays them
/// out (one place, one width, none locked), take the width that bar and the text under it (its
/// tokens filled as the page draws them, measured as drawn) now need, kept on the side of the
/// frame they are nearer. A layout without them, with them moved apart, with that text spaced out
/// or in another font, naming a map series page (which differs from page to page), or of a frame
/// other than the first (whose map the page fills the text from) is left as it is; so is a text
/// longer than the widest box a publication gives. Only a box as wide as `before` (what it was
/// measured from before the change: its frame, bar and text) is measured again: one set
/// by hand keeps its width. A change to the text under the bar or to the bar's units or divisions
/// measures it again too. A box that would then cover the legend, locator or north arrow, which
/// it didn't, stays as it was, which is said.
fn refit_scale_box(s: &mut Session, li: usize, before: &BoxBasis) {
    let frame = before.frame;
    let Some(now) = scale_box_basis(s, li).filter(|b| b.frame == frame) else { return };
    let Some(l) = s.project.layouts.get(li) else { return };
    let Some(fr) = l.element(frame) else { return };
    let ElementKind::MapFrame { map, .. } = &fr.kind else { return };
    let Some(m) = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)) else { return };
    let named = |name: &str| l.elements.iter().find(|e| e.name == name);
    let (Some(boxed), Some(bar), Some(text)) = (named("Scale Box"), named("Scale Bar"), named("Scale")) else { return };
    let (ElementKind::ScaleBar { frame: of, .. }, ElementKind::Text { text: written, letter_spacing, font, font_name, .. }) =
        (&bar.kind, &text.kind)
    else {
        return;
    };
    if *letter_spacing != 0.0
        || *font != FontFamily::Sans
        || font_name.is_some()
        || (l.series.is_some() && written.contains("{page"))
        || l.first_frame() != Some(frame)
    {
        return;
    }
    // Its tokens filled as the page draws them.
    let filled = basis_text(l, m, &now);
    let together = [bar, text].iter().all(|e| (e.x - boxed.x).abs() < 1e-6 && (e.w - boxed.w).abs() < 1e-6 && !e.locked);
    if *of != frame || !together || boxed.locked {
        return;
    }
    // A text longer than the widest box a publication gives: a width set by hand for it is kept.
    if scale_text_mm(&filled, now.size, now.bold) > (fr.w * 0.3).max(25.0) {
        return;
    }
    // The width the box had from its basis before the change: another one was set by hand.
    if (boxed.w - basis_width(l, m, before)).abs() > 0.01 {
        return;
    }
    let w = basis_width(l, m, &now);
    let x = if boxed.x + boxed.w / 2.0 > fr.x + fr.w / 2.0 { boxed.x + boxed.w - w } else { boxed.x };
    let covers = |(x, w): (f64, f64), e: &rustgis_map::layout::Element| {
        x < e.x + e.w && e.x < x + w && boxed.y < e.y + e.h && e.y < boxed.y + boxed.h
    };
    let newly = l
        .elements
        .iter()
        .filter(|e| PLACED_OVER_MAP.contains(&e.name.as_str()) && e.visible)
        .find(|e| covers((x, w), e) && !covers((boxed.x, boxed.w), e))
        .map(|e| e.name.to_lowercase().replace(" box", ""));
    if let Some(what) = newly {
        s.notify(
            format!("the scale bar's box would now cover the {what}, so it is left as it was: move or widen it by hand, or make the layout again with layout.publication"),
            false,
        );
        return;
    }
    let ids = [boxed.id, bar.id, text.id];
    if let Some(l) = s.project.layouts.get_mut(li) {
        for id in ids {
            if let Some(e) = l.element_mut(id) {
                (e.x, e.w) = (x, w);
            }
        }
    }
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
    let basis = scale_box_basis(s, li);
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
    if let Some(basis) = basis.filter(|b| b.frame == frame) {
        refit_scale_box(s, li, &basis);
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
    Some((0..layer.dataset.features.len()).map(|r| rustgis_map::layout::series_page_name(layer, fi, r)).collect())
}

/// A map series' page count and the name of `page`, without naming every page.
pub fn series_info(s: &Session, li: usize, page: usize) -> Option<(usize, String)> {
    let (layer, fi) = series_layer(s, li)?;
    let n = layer.dataset.features.len();
    Some((n, if page < n { rustgis_map::layout::series_page_name(layer, fi, page) } else { String::new() }))
}

/// A map series' index layer and its name field.
fn series_layer(input: &impl super::InputSource, li: usize) -> Option<(&rustgis_map::Layer, Option<usize>)> {
    let context = input.input_context();
    let s = &context;
    s.project.layouts.get(li)?.series_index(&s.project.maps)
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
    let e = layer.rows_extent([page]);
    if !e.is_empty() {
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
    layout_bytes_with(s, li, pdf, dpi, all, true, rustgis_render::pdf::Options::default())
}

/// [`layout_bytes`], a PDF's map frames georeferenced only when `georeference` (#1191), written
/// as `options` say (its layers and text, #1815). Returns an actionable error if a linked archive changes
/// while the pages are drawn.
pub fn layout_bytes_with(
    s: &mut Session,
    li: usize,
    pdf: bool,
    dpi: f64,
    all: bool,
    georeference: bool,
    options: rustgis_render::pdf::Options,
) -> Result<Vec<u8>, String> {
    let layout = s.project.layouts.get(li).ok_or("no layout")?;
    let revisions = rustgis_render::layout::archive_revisions(layout, &s.project.maps);
    let result = layout_bytes_inner(s, li, pdf, dpi, all, georeference, options);
    let layout = s.project.layouts.get(li).ok_or("no layout")?;
    revisions.finish_revision(
        rustgis_render::layout::archive_revisions(layout, &s.project.maps),
        Some(result),
        "could not draw the page",
    )?
}

/// Layout `li`'s current page as a PNG at `dpi`, or only its part in `region` (`[x, y, w, h]` in
/// millimetres from the page's top left), drawn at that `dpi` rather than cut out of the whole
/// page: what the assistant's `view_map` shows of a layout. It says what exporting the page says
/// of it: texts cut off, named fonts still being read, frames drawn without their cast shadows.
pub fn layout_png(s: &mut Session, li: usize, dpi: f64, region: Option<[f64; 4]>) -> Result<Vec<u8>, String> {
    let layout = s.project.layouts.get(li).ok_or("no layout")?.clone();
    if let Some(note) = named_fonts_unread(&layout) {
        s.warn(note);
    }
    let page = layout.series.as_ref().map_or(0, |sr| sr.page);
    let series = series_info(s, li, page);
    let info = rustgis_render::layout::PageInfo {
        page: page + 1,
        pages: series.as_ref().map_or(1, |i| i.0),
        page_name: series.map(|i| i.1).unwrap_or_default(),
    };
    let mut clipped = rustgis_render::layout::clipped_text(&layout, &s.project.maps, dpi, &info);
    // A region says the texts cut off in it, not those elsewhere on the page it doesn't show.
    if let Some(r) = region {
        clipped.retain(|(id, _)| layout.element(*id).is_some_and(|e| rustgis_render::layout::meets(e, r, 0.0)));
    }
    let img = super::share::saying_shadows(s, "The layout", |s| match region {
        Some(r) => rustgis_render::layout::render_region(&layout, &mut s.project.maps, dpi, &info, r),
        None => rustgis_render::layout::render(&layout, &mut s.project.maps, dpi, &info),
    })?;
    if let Some(note) = rustgis_render::layout::clipped_text_notice(&layout, &clipped) {
        s.warn(note);
    }
    img.encode_png().map_err(|e| e.to_string())
}

/// What `view_map` draws when it is given a `layout`: the layout's frames' maps and their tiles,
/// as exporting it reads them, rather than the active map's. `None` without a `layout`.
pub(crate) fn view_inputs(input: &super::InputContext<'_>, p: &Value) -> Option<Result<super::Inputs, String>> {
    p.get("layout").filter(|v| !v.is_null()).map(|_| layout_inputs(input, p))
}

/// The layout `p` names (`layout`, a name or an index), as Export Layout finds it.
pub fn named(s: &Session, p: &Value) -> Result<usize, String> {
    index(s, p)
}

fn layout_bytes_inner(
    s: &mut Session,
    li: usize,
    pdf: bool,
    dpi: f64,
    all: bool,
    georeference: bool,
    options: rustgis_render::pdf::Options,
) -> Result<Vec<u8>, String> {
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
    let mut clipped = BTreeMap::new();
    let render_pages = || -> Result<(Vec<rustgis_render::pdf::Page>, Option<Vec<u8>>), String> {
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
            clipped.extend(rustgis_render::layout::clipped_text(layout, &s.project.maps, dpi, &info));
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
    // Layers its frames drew without their cast shadows (read in place, too large for the
    // browser, no memory, still being worked out on the interface's thread): said, as the map's
    // export says them.
    let (result, said) = rustgis_render::shadows_said(render_pages);
    if result.is_ok() {
        for w in super::share::shadow_warnings("The layout", &said) {
            s.warn(w);
        }
    }
    // Stepping through the series moved the frame; put the layout back however that went.
    if stepping && let Some(cur) = s.project.layouts.get_mut(li) {
        *cur = saved.clone();
    }
    let (pages, png) = result?;
    if let Some(note) = rustgis_render::layout::clipped_text_notice(&saved, &clipped.into_iter().collect::<Vec<_>>()) {
        s.warn(note);
    }
    pictured_notices(s, &frames_pictured, &pages_pictured);
    georeference_notices(s, &left_out, &by_corners, pages.len());
    if !pdf {
        return png.ok_or_else(|| "nothing to draw".to_string());
    }
    let (bytes, outlined) = rustgis_render::pdf::write_with(&pages, &saved.name, options);
    outlined_notice(s, &outlined);
    Ok(bytes)
}

/// Says which faces a PDF set in outlines though its text was to be in fonts (#1815), with why:
/// their text is drawings, not text, in a vector editor. One notice, the faces counted.
pub(crate) fn outlined_notice(s: &mut Session, outlined: &[rustgis_render::pdf::Outlined]) {
    let n = outlined.len();
    if n == 0 {
        return;
    }
    let mut each: Vec<String> = outlined.iter().take(10).map(|o| format!("{} ({})", o.face, o.why)).collect();
    if n > 10 {
        each.push(format!("and {} more", n - 10));
    }
    s.warn(format!(
        "The PDF sets the text of {n} face{} as outlines, not as text a vector editor can edit: {}",
        if n == 1 { "" } else { "s" },
        each.join(", ")
    ));
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
                "The PDF georeferences {n} map frame{} by {} corners in longitude/latitude, as {} can't be written as WKT: on a 9 × 9 grid over each, it puts places within 0.5 mm of where they are drawn, not proven between them ({})",
                plural(n),
                if n == 1 { "its" } else { "their" },
                if n == 1 { "its coordinate system" } else { "their coordinate systems" },
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

/// The keys `layout.export`'s `"pdf"` takes.
const PDF_KEYS: &[&str] = &["layers", "text"];

/// How `layout.export`'s `"pdf": {…}` asks a PDF to be written (#1815): with its layers unless
/// `"layers": false`, its text in its fonts unless `"text": "outlines"`. It is checked whatever
/// the export writes, a PNG or SVG too, so a script's mistake shows on its first run; a key it
/// doesn't know is refused, with the keys it takes.
fn pdf_options(p: &Value) -> Result<rustgis_render::pdf::Options, String> {
    use rustgis_render::pdf::Text;
    let mut options = rustgis_render::pdf::Options::default();
    let Some(pdf) = p.get("pdf").filter(|v| !v.is_null()) else { return Ok(options) };
    let Some(pdf) = pdf.as_object() else { return Err(r#"pdf is an object: {"layers": true|false, "text": "fonts"|"outlines"}"#.into()) };
    if let Some(key) = pdf.keys().find(|k| !PDF_KEYS.contains(&k.as_str())) {
        return Err(format!("pdf has no key \"{key}\": its keys are {}", PDF_KEYS.join(", ")));
    }
    match pdf.get("layers") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(on)) => options.layers = *on,
        Some(_) => return Err("pdf.layers is true or false".into()),
    }
    options.text = match pdf.get("text") {
        None | Some(Value::Null) => Text::Fonts,
        Some(v) if v == "fonts" => Text::Fonts,
        Some(v) if v == "outlines" => Text::Outlines,
        Some(_) => return Err(r#"pdf.text is "fonts" or "outlines""#.into()),
    };
    Ok(options)
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
    let options = pdf_options(p)?;
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
                layout_bytes_with(&mut copy, li, pdf, dpi, all, georeference, options)?
            };
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            rustgis_data::io::write_atomic(Path::new(&path), &bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!({"path": path, "bytes": bytes.len()}))
        })();
        // What drawing it said (frames drawn as pictures, fonts it lacks), as the command says it
        // here.
        let said = super::exports::copy_notices(&mut copy, &path);
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
    let clipped = rustgis_render::layout::clipped_text(layout, &s.project.maps, dpi, &info);
    let note = rustgis_render::layout::clipped_text_notice(layout, &clipped);
    let (text, pictured) = rustgis_render::svg::layout_svg(layout, &mut s.project.maps, dpi, &info)?;
    if let Some(note) = note {
        s.warn(note);
    }
    pictured_notices(s, &pictured, &[]);
    Ok(text)
}

pub(crate) fn import(s: &mut Session, text: &str) -> CmdResult {
    if text.len() as u64 > MAX_LAYOUT {
        return Err("the layout file is too large".into());
    }
    // A newer format, or a capability this release doesn't know, is refused by name rather than
    // read without what it adds (#2277). A file without a header is a legacy layout.
    crate::project::check_format(text, "layout")?;
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
    let (name, cut) = unique_name(s, "Import Layout", "the layout file's name", &l.name);
    l.name = name;
    if let Some(cut) = cut {
        s.notify(cut, false);
    }
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
    if kind == "legend" && legend_exclude(input, layout, frame, p)?.is_none() && legend_labels(input, layout, frame, p)?.is_none() {
        let mut inputs = object_inputs(input, p)?;
        inputs.state["frame"] = json!(frame);
        return Ok(inputs);
    }
    let map = frame_map(input, layout, frame)?;
    let mut inputs = super::Inputs::layers(std::iter::empty());
    if matches!(kind, "table" | "chart") {
        let name = frame_layer_name(input, layout, frame, p)?;
        inputs.extend(super::Inputs::named(map, [name.as_str()])?);
    } else if kind == "colorbar" {
        if let Some(name) = colorbar_layer(input, layout, frame, ps(p, "layer"))? {
            inputs.extend(super::Inputs::named(map, [name.as_str()])?);
        }
    } else {
        legend_inputs(&mut inputs, map, legend_exclude(input, layout, frame, p)?, legend_labels(input, layout, frame, p)?);
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

/// Whether the publication gives the top shown raster with a colour scale a colorbar: `"auto"` (the
/// default) or `"none"`.
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

/// The names a publication's legend leaves out as context around the data: the layers the placement
/// takes as context in a frame `size` (mm) showing `map` at 1:`scale` around `center`
/// ([`crate::render::layout::context_layers`], #1379). The legend leaves layers out by name, so a
/// name is left out only when every layer it would list under that name is context, and no web
/// tile layer has it.
fn legend_context(map: &Map, center: Coord, scale: f64, size: (f64, f64)) -> Vec<String> {
    let ids = crate::render::layout::context_layers(map, center, scale, size);
    let shown: Vec<&rustgis_map::Layer> = map.layers.iter().filter(|l| map.shows(l) && crate::render::layout::legend_lists(l)).collect();
    let mut names: Vec<String> = Vec::new();
    for l in shown.iter().filter(|l| ids.contains(&l.id)) {
        let same = |name: &str| name.eq_ignore_ascii_case(&l.name);
        let all = shown.iter().filter(|o| same(&o.name)).all(|o| ids.contains(&o.id));
        if all && !map.tiles.iter().any(|t| same(&t.name)) && !names.iter().any(|n| same(n)) {
            names.push(l.name.clone());
        }
    }
    names
}

/// A publication's `key` given as `true` or `false`; `None` when it isn't given.
fn publication_flag(p: &Value, key: &str) -> Result<Option<bool>, String> {
    match p.get(key).filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or_else(|| format!("{key} is true or false")),
    }
}

/// A publication's north arrow: `Some(true)` always drawn ("always"), `Some(false)` never ("none"),
/// `None` left to the extent ("auto", the default: none at a continental span, #1379).
fn publication_north_arrow(p: &Value) -> Result<Option<bool>, String> {
    match p.get("northArrow").filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => match v.as_str().map(str::trim) {
            Some("auto") => Ok(None),
            Some("always") => Ok(Some(true)),
            Some("none") => Ok(Some(false)),
            _ => Err("northArrow is \"auto\", \"always\" or \"none\"".into()),
        },
    }
}

/// The span in degrees, on its longer side, from which a publication's extent is continental: its
/// graticule's meridians then turn too far across the frame for one north arrow to point north
/// everywhere, and they give north themselves (#1379).
pub(crate) const CONTINENTAL_DEG: f64 = 20.0;

/// An extent's span in degrees on its longer side, its width as long at its middle latitude.
pub(crate) fn extent_span_deg(e: &Extent) -> f64 {
    let mid = ((e.min_y + e.max_y) / 2.0).to_radians();
    e.height().max(e.width() * mid.cos())
}

/// Whether a publication's legend always has its box: `true` for "always", `false` for "auto"
/// (the default: none where the map shows nothing under it but its plain background, #1379).
fn publication_legend_box(p: &Value) -> Result<bool, String> {
    match p.get("legendBox").filter(|v| !v.is_null()) {
        None => Ok(false),
        Some(v) => match v.as_str().map(str::trim) {
            Some("auto") => Ok(false),
            Some("always") => Ok(true),
            _ => Err("legendBox is \"auto\" or \"always\"".into()),
        },
    }
}

/// Whether a line of a graticule every `step` degrees crosses `under` (mm from the top left of a
/// frame `size` mm wide and tall showing the active map at 1:`scale` around `center`): the
/// rectangle's longitudes or latitudes, read on a grid of points over it, run past a multiple of
/// the step. A point off the projection, or longitudes over half the globe apart, count as
/// crossed.
fn graticule_crosses(s: &Session, center: Coord, scale: f64, (fw, fh): (f64, f64), under: Rect, step: f64) -> bool {
    const N: usize = 8;
    let flat = s.map().flat();
    let Some(at) = flat.forward_checked(center) else { return true };
    let paper_mm_per_unit = 1000.0 * flat.ground_per_unit(at) * s.map().body().ratio() / scale;
    let (mut lon, mut lat) = ((f64::INFINITY, f64::NEG_INFINITY), (f64::INFINITY, f64::NEG_INFINITY));
    for i in 0..=N {
        for j in 0..=N {
            let x = under.0 + under.2 * i as f64 / N as f64 - fw / 2.0;
            let y = under.1 + under.3 * j as f64 / N as f64 - fh / 2.0;
            let Some(ll) = flat.inverse(Coord::new(at.x + x / paper_mm_per_unit, at.y - y / paper_mm_per_unit)) else { return true };
            (lon, lat) = ((lon.0.min(ll.x), lon.1.max(ll.x)), (lat.0.min(ll.y), lat.1.max(ll.y)));
        }
    }
    let crossed = |(lo, hi): (f64, f64)| (lo / step).floor() != (hi / step).floor() || (lo / step).fract() == 0.0;
    !step.is_finite() || step <= 0.0 || lon.1 - lon.0 > 180.0 || crossed(lon) || crossed(lat)
}

/// Whether the publication may veil what lies outside the extent when nothing else leaves its
/// legend room: `true` (the default) or `false`.
fn publication_veil(p: &Value) -> Result<bool, String> {
    match p.get("veil").filter(|v| !v.is_null()) {
        None => Ok(true),
        Some(v) => v.as_bool().ok_or_else(|| "veil is true or false".into()),
    }
}

/// Whether a publication of `map` shows the whole globe: the map is on the globe and no extent is
/// given, so the sheet's map is the globe seen whole (#2337).
fn shows_whole_globe(map: &rustgis_map::Map, p: &Value) -> bool {
    map.projection.is_globe() && p.get("extent").is_none_or(Value::is_null)
}

/// How much of the frame, inside its graticule's label strips, a globe seen whole spans: a little
/// less than all of it, for the atmosphere's glow at its rim.
const WHOLE_GLOBE_FILL: f64 = 0.94;

/// Where a frame of `map` at 1:`scale` centres the globe seen whole: at `view`, or as near it on
/// its meridian as the frame's camera can draw the globe that small. Towards a pole the camera's
/// resolution for a scale grows as one over the cosine of the latitude, and past its coarsest
/// ([`rustgis_map::camera::MAX_RES`]) the globe would be drawn larger than the frame (#2337).
fn drawable_globe_center(map: &Map, view: Coord, scale: f64) -> Coord {
    let fits =
        |lat: f64| rustgis_map::layout::frame_camera(map, Coord::new(view.x, lat), scale, 0.0, 0.0).res < rustgis_map::camera::MAX_RES;
    if fits(view.y) {
        return view;
    }
    // The latitude in between that the camera still draws at the scale, found by halving.
    let (mut fit, mut over) = (0.0f64, view.y.abs());
    for _ in 0..40 {
        let mid = (fit + over) / 2.0;
        if fits(mid.copysign(view.y)) { fit = mid } else { over = mid }
    }
    Coord::new(view.x, fit.copysign(view.y))
}

/// Centres the globe locator as close to the area as its circle can hold the rendered disc.
/// If even the equator does not fit, or the area would be behind the rim, keep the locator
/// centred on the area and let its circle clip the globe (#2394).
pub(crate) fn globe_locator_center(map: &Map, area: Coord, scale: f64, side_mm: f64) -> (Coord, bool) {
    let side = side_mm * rustgis_map::layout::PT_PER_MM;
    let globe = |at| {
        let camera = rustgis_map::layout::frame_camera(map, at, scale, 0.0, 0.0);
        rustgis_map::globe::Globe::new(&camera, side, side)
    };
    let fits = |at| 2.0 * globe(at).disc_radius() <= side;
    if fits(area) {
        return (area, true);
    }
    let equator = Coord::new(area.x, 0.0);
    if !fits(equator) {
        return (area, false);
    }
    // At a fixed scale the disc only grows towards the pole, even after resolution is clamped.
    let (mut inside, mut outside) = (equator, area);
    for _ in 0..40 {
        let mid = Coord::new(area.x, (inside.y + outside.y) / 2.0);
        if fits(mid) {
            inside = mid;
        } else {
            outside = mid;
        }
    }
    if globe(inside).forward(area).is_some() { (inside, true) } else { (area, false) }
}

/// Parses the publication's locator choice with its default: a globe on a sheet, none on a figure
/// or where the map is itself the globe seen whole (it would only show it again, smaller).
fn publication_locator(p: &Value, sheet: bool, whole_globe: bool) -> Result<Locator, String> {
    Ok(match p.get("locator").filter(|v| !v.is_null()) {
        None => {
            if sheet && !whole_globe {
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
    let locator = publication_locator(p, profile.sheet, shows_whole_globe(s.map(), p))?;
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
    // A sheet's size alone ("a4", the default, or any page size `layout.new` takes): its
    // orientation chosen below, from the area's shape, portrait until then.
    let auto = PAGES.iter().any(|(size, ..)| *size == id).then(|| id.clone());
    let id = match &auto {
        Some(size) => format!("{size}-portrait"),
        None => id,
    };
    let profile = PUBLICATION_PROFILES.iter().find(|pr| pr.id == id).ok_or_else(|| {
        let sizes: Vec<&str> = PAGES.iter().map(|(size, ..)| *size).collect();
        let names: Vec<&str> = PUBLICATION_PROFILES.iter().map(|pr| pr.id).collect();
        format!("unknown profile '{id}': {} (turned to the area's shape), {}", sizes.join(", "), names.join(", "))
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
                // A chart that groups and counts (or is told to) charts any field; the others a
                // number.
                let count = match ps(p, "statistic") {
                    Some(st) => StatKind::from_code(st) == Some(StatKind::Count),
                    None => matches!(e.kind, ElementKind::Chart { statistic: StatKind::Count, .. }),
                } && match p.get("group").filter(|v| !v.is_null()) {
                    Some(g) => g.as_str().is_some_and(|g| !g.trim().is_empty()),
                    None => matches!(e.kind, ElementKind::Chart { group: Some(_), .. }),
                };
                // Only charts have a field.
                let field = match nf.filter(|_| matches!(e.kind, ElementKind::Chart { .. })) {
                    Some(fl) => Some(
                        target
                            .dataset
                            .field_index(fl)
                            .and_then(|i| target.dataset.fields.get(i))
                            .filter(|f| count || f.ty.is_numeric())
                            .map(|f| f.name.clone())
                            .ok_or_else(|| format!("no numeric field '{fl}' in {}", target.name))?,
                    ),
                    // A chart moved to another layer keeps its field only if that layer has it as a
                    // number (any field, counting); otherwise it charts the layer's first numeric field,
                    // or, counting a layer without one, its first field.
                    None => match &e.kind {
                        ElementKind::Chart { field, .. } if nl.is_some() => {
                            let numeric = |f: &&rustgis_data::Field| count || f.ty.is_numeric();
                            let keep = target.dataset.field_index(field).and_then(|i| target.dataset.fields.get(i)).filter(numeric);
                            let f = keep
                                .or_else(|| target.dataset.fields.iter().find(|f| f.ty.is_numeric()))
                                .or_else(|| target.dataset.fields.first().filter(|_| count))
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
            let (names, labels) = (legend_exclude(input, l, *frame, p)?, legend_labels(input, l, *frame, p)?);
            if names.is_some() || labels.is_some() {
                legend_inputs(&mut inputs, frame_map(input, l, *frame)?, names, labels);
            }
        }
        _ => {}
    }
    inputs.state = json!({"layout":li,"element":id,"kind":l.element(id).map(|e|e.kind.code()),"stored":l.element(id).and_then(|e|element_selectors(&e.kind)),"selectors":inputs.state});
    Ok(inputs)
}

/// The layers a legend's `exclude` and `labels` name, as inputs. Tile layers' names are stored
/// selector text, not invented layer/table identities.
fn legend_inputs(inputs: &mut super::Inputs, map: &Map, names: Option<Vec<String>>, labels: Option<Vec<(String, Option<String>)>>) {
    let named = names.iter().flatten().chain(labels.iter().flatten().map(|(n, _)| n));
    inputs.layers.extend(named.filter_map(|n| map.layer_by_name(n)).map(|l| (map.instance_id(), l.id)));
    inputs.state = json!({"exclude": names, "labels": labels});
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
        ElementKind::Chart { frame, layer, field, label, group, stack, .. } => {
            json!({"frame":frame,"layer":layer,"field":field,"label":label,"group":group,"stack":stack})
        }
        ElementKind::Legend { frame, exclude, labels, .. } => json!({"frame":frame,"exclude":exclude,"labels":labels}),
        ElementKind::Colorbar { frame, layer, .. } => json!({"frame":frame,"layer":layer}),
        ElementKind::NorthArrow { frame, .. } | ElementKind::ScaleBar { frame, .. } => json!({"frame":frame}),
        _ => return None,
    })
}
