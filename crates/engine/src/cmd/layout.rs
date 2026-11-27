//! Home › Project (New Layout, Import Layout), Publish › Map Frames, Map Surrounds and Text, and
//! Publish › Output (Export Layout) and Save As (Layout File): page layouts.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use rustgis_geom::{Coord, Extent};
use rustgis_map::layout::{
    Arrange, ArrowStyle, Background, CHART_BAR, CHART_BARS, CHART_OUTLINE, ChartOrder, ChartOutline, CoordFormat, ElementKind, FontFamily,
    FrameShape, GRID_LABEL_PT, Grid, GridEdges, GridKind, GridStyle, Guides, LAYOUT_TEXT_SIZE, LINE_SPACING, Layout, LineDir, LineHead,
    MAX_CHART_BARS, MAX_CHART_DECIMALS, MAX_CHART_OUTLINE_WIDTH, MAX_COLORBAR_TICKS, MAX_GRID_WIDTH, MAX_GUIDES, MAX_LEGEND_COLUMNS,
    MAX_LEGEND_LABEL_CHARS, MAX_LEGEND_LABELS, MAX_LINE_SPACING, MAX_SCALE_BEFORE_ZERO, MAX_SCALE_DECIMALS, MAX_SCALE_DIVISIONS,
    MAX_SCALE_SUBDIVISIONS, MAX_TICK_MM, MAX_UNIT_LABEL_CHARS, MIN_ELEMENT_MM, MIN_LINE_SPACING, MapSeries, Orientation, PAGES,
    SCALE_BAR_TEXT_PT, ScaleBarFit, ScaleBarStyle, ScaleLabelPosition, ScaleUnits, TextAlign, TextCase, TextVAlign, TickSide,
    UnitLabelPosition, fit_scale, line_angle, line_at_angle, line_length,
};
use rustgis_map::report::StatKind;
use rustgis_map::symbology::{Dash, MAX_DASH_STEP, MAX_DASH_STEPS};
use rustgis_map::{Map, Rgba};
use serde_json::{Value, json};

use super::{b, f, numbered, ps};
use crate::{CmdResult, CommandSpec as C, Session};

mod chart;
use chart::*;
mod elements;
pub(crate) use elements::*;
mod export;
pub use export::*;
mod files;
pub(crate) use files::*;
mod frame;
#[cfg(test)]
pub(crate) use frame::scale_box_auto;
use frame::*;
mod inputs;
pub(crate) use inputs::*;
mod legend;
use legend::*;
mod page;
use page::*;
mod publication;
pub use publication::*;
mod series;
pub use series::*;
mod text;
pub(crate) use text::*;
mod update;
use update::*;

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
            concat!(
                r#"{"profile"?: "a4" (the default) | "letter"|"legal"|"tabloid"|"a5"|"a3"|"a2"|"a1"|"a0" (a sheet turned to portrait or landscape, whichever the area's map comes out larger on: its extent east to west against north to south, on the map's projection) | a size with its orientation: "a4-portrait"|"a4-landscape"|"letter-portrait"|"tabloid-landscape"|… for each of those sizes (a sheet: frame, title, subtitle, the map, a boxed footer, all scaled to the sheet)|"figure-1col" (90 mm)|"figure-1.5col" (140 mm)|"figure-2col" (190 mm)|"thesis-full" (160 × 225 mm)|"thesis-half" (160 × 110 mm) (a figure: the map edge to edge, its title and sources left to the caption), "#,
                r#""extent"?: [west, south, east, north] in degrees that must fit (default: the map view), "#,
                r#""name"?: string, "#,
                r#""title"?: text, "#,
                r#""subtitle"?: text, "#,
                r#""sources"?: text | [texts] (any of the texts may hold a layer's figure as the layout's texts do: {count:Layer}, {sum:Layer:field}, {mean:…}, {min:…}, {max:…}, filled in when the page is drawn), "#,
                r#""reference"?: text | [texts] (default: the projection, datum and graticule), "#,
                r#""note"?: text, "#,
                r#""author"?: text (default: "Made with RustGIS · {date}"; "" or [] for none), "#,
                r#""headings"?: [the sources' heading, the reference's heading, an editorial column's heading], "#,
                r#""locator"?: "globe" (a globe with the area outlined; the default on sheets, but for a map on the globe with no extent, the globe seen whole, which by default has no locator (one asked for by name is drawn) and no scale bar, the globe filling its frame) | "inset" (the surrounding region, the area outlined) | "none" (the default on figures), "#,
                r#""decimal"?: "." (the default) | "," (graticule, scale bar and colorbar numbers and {scale} with a decimal comma: 0,05 and 1:6.300.000), "#,
                r#""colorbar"?: "auto" (the default: the top visible raster with a colour scale, not drawn by classes, gets one, under the legend in its box; a raster drawn by classes has them in the legend's rows instead) | "none" (no colorbar, and the legend's box has no room for one), "#,
                r#""veil"?: true (the default: when no shape of the legend fits, a map that runs on past the extent, with a long legend, has what is outside the extent veiled and its surrounds grouped there, at up to half the scale on a sheet) | false (never veiled, and drawn at its scale; the map may still move aside), "#,
                r#""contextInLegend"?: false (the default: layers the placement takes as context around the data, polygons only outlined or, beside a layer drawn by classes or a raster, in one colour and running on past the frame, are left out of the legend, which a warning names) | true (the legend lists them), "#,
                r#""northArrow"?: "auto" (the default: none when the extent spans 20° or more on its longer side, where the graticule gives north, which a warning says) | "always" | "none" | {"show"?: "auto" | "always", "style"?, "fill"?, "outline"? as layout.update takes a north arrow's}, "#,
                r#""grid"?: "none" (no grid on the map frame, and no label strips inside it) | {a frame grid's options as layout.addElement takes them: type, crs, step, short, mgrs, color, width, labels, labelSize, style, tickLength, format, hemisphere, edges, labelOrientation; a graticule takes no crs, short or mgrs, a measured grid no format or hemisphere} over the default graticule (its step by the extent, labelled left and bottom, on a sheet all round); the label strips follow its labels, edges and labelSize (in points as given, not scaled with the sheet), and without a graticule the default reference names none and {grid} in a text reads "—", "#,
                r#""scaleBar"?: "none" | {a scale bar's options as layout.update takes them: style, units, divisions, color, fill, textSize, decimals, subdivisions, divisionsBeforeZero, labelPosition, unitLabel, unitLabelPosition, fit; textSize in points as given, not scaled with the sheet} (none on a globe shown whole, whatever is asked), "#,
                r#""legendBox"?: "auto" (the default: over the map, the legend has no box where the map shows nothing under it but its light plain paper or water, with no layer, context, basemap shape or graticule line there and no veil; the answer's "legendBox" says whether it has one) | "always", "#,
                r#""style"?: "classic" (the default: the title over the map, the footer under it) | "banner" (the title and subtitle in a band of the accent colour across the top) | "panel" (the map at the left, a panel at the right with the title, locator, legend, sources and reference) | "editorial" (the title large in serif, a column at the right with the note, the locator and the legend) | "modular" (cards on a light page: the map, the locator and the legend each in one, the sources and reference in tinted ones) | "poster" (the page in the accent colour, the title in light text, the map in a light card, the footer in darker blocks) | "edge" (the map from one side of the page to the other with no border, a kicker and the title over it, the footer with no boxes under a thin rule) (a sheet's arrangement; figures leave it to the caption), "#,
                r##""paper"?: "#RRGGBB" (an edge sheet's page colour, as layout.page's "color"; white by default; on a dark one its texts are light), "##,
                r#""kicker"?: text (an edge sheet's short line in spaced capitals over the title, in the accent colour), "#,
                r#""accent"?: a colour as #RRGGBB (the style's colour: its band, panel tint, headings and rules, a poster's page), "#,
                r#""arrangement"?: "best" (the default: the favourite, each arrangement by name and each saved with layout.arrangement laid out, the one moving the least area off its corners kept, then the one leaving the least over the data, the favourite on a tie) | "favourite" (layout.arrangement's) | an arrangement by name (built in or saved) or corner by corner, as layout.arrangement takes (the corners the locator, north arrow, legend and scale bar are at home in; one covering what the map draws there still moves), "#,
                r#""coverage"?: "*.png" (writes the coverage grid the placement searched, as it settled on it, three pixels a cell: what the map draws dark, the free cells white, both tinted blue in the strips the graticule's labels keep, the surrounds placed over the map outlined in red; the answer gives its path)} → a new layout of the active map at a round scale, its texts wrapped to their boxes; {projection}, {datum} and {grid} in the texts are filled from the map"#,
            ),
        ),
        C::new("layout.arrangement", "Favourite Arrangement", "Publish › Layout", arrangement).roles(&[]).effects(crate::policy::Effects::NONE).view().params(
            concat!(
                r#"{"favourite"?: "classic" (the locator top left, the north arrow top right, the legend bottom left, the scale bar bottom right) | "mirrored" | "legendRight" | "legendTop" | {"locator", "northArrow", "legend", "scaleBar": "top-left"|"top-right"|"bottom-left"|"bottom-right", each a corner of its own} | a saved arrangement's name, "#,
                r#""save"?: a name (1 to 40 characters, not a built-in one nor "best", "favourite", "custom" or "veil"; one saved under it already, case aside, is replaced) to save "arrangement" under (at most 20 saved), "#,
                r#""arrangement"?: what "save" saves: an arrangement by name, or a corner for each surround as "favourite" takes them (refused without "save"), "#,
                r#""remove"?: a saved arrangement's name (a favourite named after it keeps its corners, unless "favourite" gives another)} → the favourite arrangement of publication layouts and those saved by name (kept with the app's preferences), the favourite's corners, the arrangements by name and the saved ones' corners; a removal, then a save, then the favourite; with none of them given, they are only shown"#,
            ),
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
        // The order of the layout tabs and the Browser's layouts (#2802); the open one stays open.
        C::new("layout.move", "Move Layout", "Browser", |s, p| {
            let from = index(s, &by_name(p))?;
            let to = super::move_item(s, |s| &mut s.project.layouts, from, p)?;
            s.active_layout = s.active_layout.map(|a| super::after_move(a, from, to));
            Ok(Value::Null)
        }).inputs(named_object_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name"?: string, "layout"?: name|index (instead of name; default: the open layout), "to": index (0: first; past the end: last)}"#),
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
            l.renew_instance_id();
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
            r##"{"layout"?, "page"?: page id, "orientation"?: "landscape"|"portrait", "width"?: mm, "height"?: mm, "decimal"?: "."|"," (the decimal mark of the numbers its surrounds print), "color"?: "#RRGGBB" (the page's colour, painted under every element in the view and every export; opaque only; null or #FFFFFF: white, the default; left out: as it was)}"##,
        ),
        C::new("layout.addFrame", "Map Frame", "Publish › Map Frames", add_frame).inputs(add_frame_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layout"?, "shape"?: "rectangle"|"circle"|"ellipse"|"polygon"|"lasso", "points"?: [[u, v], …] (polygon/lasso, 0..1 across the frame), "map"?: name, "x"?, "y"?, "w"?, "h"?: mm}"#,
        ),
        C::new("layout.addElement", "Add Element", "Publish › Map Surrounds", add_element).inputs(element_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).params(
            concat!(
                r##"{"layout"?, "##,
                r##""kind": "northArrow"|"scaleBar"|"legend"|"colorbar"|"text"|"dynamicText"|"picture"|"table"|"chart"|"rectangle" (a neatline framing the page unless any of x, y, w, h is given)|"ellipse"|"circle"|"line"|"arrow"|"extentIndicator"|"grid", "##,
                r##""frame"?: element id (default: the first map frame), "##,
                r##""of"?: frame id (extentIndicator, which also takes "color" and "width" in points, default red and 2), "##,
                r##""text"? (a text's words; {count:Layer}, {sum:Layer:field}, {mean:Layer:field}, {min:Layer:field} and {max:Layer:field} show the layer's figure, as {scale} shows the scale: of the first map frame's map, over the features the layer's definition query keeps, with the layout's decimal mark; one with no figure shows as written and says why), "##,
                r##""size"?, "##,
                r##""color"?: "#RRGGBB"|"#RRGGBBAA" (text, dynamicText, line, arrow, rectangle, ellipse, circle: its outline; extentIndicator; grid; scaleBar; legend, colorbar, table, chart: the ink of its text and lines, default #222222, a chart's bars and a colorbar's colours keeping theirs, and a table drawn without its white cells; refused for other kinds), "##,
                r##""bold"?: bool (text), "##,
                r##""italic"?: bool, "##,
                r##""align"?: "left"|"center"|"right", "##,
                r##""letterSpacing"?: points, -5 to 50 (text, dynamicText), "##,
                r##""rotation"?: degrees clockwise about the box's centre, 90 reading top to bottom and -90 bottom to top, kept within -180 to 180 (text, dynamicText; refused for other kinds), "##,
                r##""wrap"?: bool (text, dynamicText: broken into lines at spaces to fit the box's width when drawn, after its dynamic text is filled in, a word wider than the box broken across lines; default false, lines breaking only at the text's own line breaks; refused for other kinds), "##,
                r##""valign"?: "top"|"middle"|"bottom" (text, dynamicText: where its block of lines sits down its box; default top; a block taller than its box is drawn from the top, its last lines left out, which a warning says; refused for other kinds), "##,
                r##""lineSpacing"?: 0.5 to 3 (text, dynamicText: the distance between its lines, a multiple of the usual; default 1; refused outside that range, and for other kinds), "##,
                r##""case"?: "none"|"upper"|"lower" (text, dynamicText: drawn in upper or lower case once its dynamic text is filled in, its text kept as typed; default none; refused for other kinds), "##,
                r##""font"?: "sans"|"serif"|an installed font family's name, as labels.fonts lists them (text, dynamicText; a family not installed here is drawn in its fallback, and a serif where none is installed in the sans, with a warning; refused for other kinds), "##,
                r##""fallback"?: "sans"|"serif" (a named font's, where it isn't installed; default its own class, else sans), "##,
                r##""title"?: legend title (default "Legend"); chart: its own title ("" for none; default: its field, or the statistic of its field by the group), "##,
                r##""allClasses"?: bool (legend: every class, not only those the frame draws), "##,
                r##""hideOther"?: bool (legend: leave out <all other values>), "##,
                r##""showNoData"?: bool (legend: include Graduated Colors’ missing-value symbol; default false), "##,
                r##""exclude"?: [layer names] (legend: layers it leaves out though the map draws them), "##,
                r##""columns"?: 1..3 (legend: the columns its rows are laid in; default 1), "##,
                r##""textSize"?: points, 4 to 100 (legend: its layers' names and classes' labels, its title, rows, swatches and gaps following in proportion and a point symbol keeping the size the map draws it at; default 9; scaleBar: its labels, the room over the bar and between them following, the bar keeping its size; default 8; refused outside that range, and for other kinds unless null, as showNoData is), "##,
                r##""orientation"?: "horizontal"|"vertical" (colorbar; chart: bars running across, the default, or columns standing up, each label centred under its column), "##,
                r##""ticks"?: 2..21 (colorbar: labelled values, ends included), "##,
                r##""tickSide"?: "below"|"above"|"right"|"left" (colorbar), "##,
                r##""reverse"?: bool (colorbar: high values first), "##,
                r##""fill"?: "#RRGGBBAA" (rectangle, ellipse, circle, northArrow; scaleBar: the alternating bar's light divisions, default white; refused for other kinds), "##,
                r##""width"?: outline width in points (rectangle, ellipse, circle; 0 for none) or line width in points (line, grid), "##,
                r##""radius"?: mm, 0 to 50 (rectangle: rounded corners), "##,
                r##""dash"?: [points, …] (line, arrow, and a rectangle's, ellipse's or circle's outline: dash, gap, … in turn, as layer.lineStrokes takes it, at most 8, each 0 to 500; an arrow's heads stay solid; [] or null for solid; refused, null too, for other kinds), "##,
                r##""dir"?: "horizontal"|"vertical"|"falling"|"rising" (line), "##,
                r##""head"?: "none"|"end"|"start"|"both" (line; default "end" for an arrow), "##,
                r##""angle"?: degrees anticlockwise from pointing right, centred on the page unless x or y is given (line, arrow; it sets the box and the direction, so w, h and dir are not used with it), "##,
                r##""length"?: mm (line, arrow), "##,
                r##""image"?: base64, "##,
                r##""path"?: picture file, "##,
                r##""layer"?: (table, chart; colorbar: a raster layer, default the top visible raster with a colour scale, not drawn by classes, which a legend lists), "##,
                r##""field"?: (chart: a numeric field, default the layer's first; any field when it groups and counts, default then the field it groups by), "##,
                r##""label"?: a field (chart: each bar's label when not grouped), "##,
                r##""group"?: a field (chart: a bar per value of this field, its statistic of field over that value's features; blank values make one "(blank)" bar; default none, a bar per feature), "##,
                r##""statistic"?: "sum" (the default)|"count" (features, numbers or not)|"mean"|"min"|"max" (chart, grouped), "##,
                r##""order"?: "descending" (the default: largest first)|"ascending"|"category" (by label, A to Z)|"layer" (as the features come) (chart), "##,
                r##""bars"?: 1..100 (chart: the most bars drawn, default 20), "##,
                r##""other"?: bool (chart: the bars past "bars" gathered in a last Other bar, its statistic over their features, their sum when not grouped, last whatever the order; default false: left out, which a warning says, as it says features with no number left out), "##,
                r##""stack"?: a field (chart, grouped by another field, summing or counting: each bar split by this field's values, a stretch per value in the key over the bars, the values its bars show by their total, largest first (A to Z when ordered by category), past 8 values 7 and a grey Other, which a warning counts; in the palette diagrams take, or with classes in the colour each value's features share on the map, a value with several keeping its palette colour, which a warning counts; values written are the bar's total; default none), "##,
                r##""values"?: bool (chart: each bar's value written past its end, its thousands apart with commas; default false), "##,
                r##""decimals"?: "auto" (the default: a chart's values up to 2, the zeros they end in dropped; a scale bar's labels each the decimals it needs, as before)|0..6 (chart: its values' decimals)|0..3 (scaleBar: its labels' decimals, fixed, with the thousands set apart), "##,
                r##""barColor"?: "#RRGGBB"|"#RRGGBBAA" (chart: its bars' colour, default #3B82C4), "##,
                r##""classes"?: bool (chart: each bar in the colour its features have on the map, by the layer's symbology classes: a bar per feature its feature's, a grouped bar the colour its features share; a bar whose features have several, which a warning counts, and an Other bar keep barColor; default false), "##,
                r##""labelAlign"?: "left" (the default)|"right" (chart: bars running across have their labels at the left of their column, or at its right against the bars), "##,
                r##""mirror"?: bool (chart: drawn as its mirror image, its text reading as ever: bars running across from the right, their labels at the right, or columns hanging from the top, their labels over them; beside a chart drawn the usual way the two face each other; default false), "##,
                r##""max"?: a number above 0|"auto" (chart: the value at the far end of its bars' room, so charts given the same one share a scale; a bar past it runs to that end, which a warning counts; default "auto", its bars' own), "##,
                r##""hideLabels"?: bool (chart: its labels left out, their room given to the bars; default false), "##,
                r##""barOutline"?: {"color"?: "#RRGGBB"|"#RRGGBBAA" (default #222222), "width"?: points, 0 to 5 (default 0.5; 0 for none)} (chart: a line round each bar, or each stretch of a stacked one; default none) (label, group, statistic, order, bars, other, stack, values, decimals, barColor, classes, labelAlign, mirror, max, hideLabels and barOutline refused for other kinds unless null, as showNoData is, a scaleBar's decimals aside), "##,
                r##""rows"?, "##,
                r##""step"?: degrees, or a measured grid's units (metres) (0 picks one), "##,
                r##""type"?: "graticule" (latitudes and longitudes, default) | "measured" (eastings and northings in a projected system), "##,
                r##""crs"?: a measured grid's coordinate system: EPSG:25832, WKT or a PROJ string ("" or none: the map's when projected, else the UTM zone at the frame's centre), "##,
                r##""short"?: bool (a measured grid's labels as their kilometres' last two digits, 43 for 543000), "##,
                r##""mgrs"?: bool (a measured grid labelled as MGRS: each 100 km square named with its grid zone designation, 32T MT, and the lines by their kilometres' last two digits; its crs a WGS 84 UTM zone by EPSG code, EPSG:32601–32660 or 32701–32760, or none: the map's when it is one, else the zone at the frame's centre), "##,
                r##""labels"?: bool (grid) | {"<layer>": "<label>"} (legend: what it shows instead of a layer's name, as in layout.update; a layer the map hasn't is refused), "##,
                r##""style"?: "lines"|"ticks"|"linesAndTicks" (grid) or "classic"|"compass"|"triangle"|"star" (northArrow) or "alternating"|"stepped"|"line" (scaleBar), "##,
                r##""tickLength"?: mm, "##,
                r##""format"?: "decimal"|"dms", "##,
                r##""hemisphere"?: bool (N/S/E/W rather than a sign), "##,
                r##""edges"?: ["top","bottom","left","right"] (labelled edges), "##,
                r##""labelOrientation"?: "horizontal"|"vertical", "##,
                r##""labelSize"?: points, 4 to 100 (its labels, their room from the frame's edge and MGRS squares' names following; default 8; null gives it back; refused outside that range, and for other kinds unless null), "##,
                r##""off"?: bool (grid; options not given keep the grid's), "##,
                r##""outline"?: "#RRGGBB" (northArrow, or a rectangle's, ellipse's or circle's outline instead of color; refused for other kinds), "##,
                r##""units"?: "auto"|"kilometers"|"meters"|"miles"|"feet", "##,
                r##""divisions"?: 1..10, at most 10 with divisionsBeforeZero together, more refused (scaleBar, whose "color" is its lines, dark divisions and labels, default #222222), "##,
                r##""subdivisions"?: 0..10 (scaleBar: parts the first division after zero is cut into, not labelled; 0 or 1 leaves it whole), "##,
                r##""divisionsBeforeZero"?: 0..2 (scaleBar: whole divisions left of zero, labelled with negatives, counted toward the ten in all with "divisions", more refused), "##,
                r##""labelPosition"?: "above"|"below" (scaleBar: its division labels, default above), "##,
                r##""unitLabel"?: text, one line, up to 100 characters (scaleBar: the unit's name as printed, default its abbreviation; "" prints none), "##,
                r##""unitLabelPosition"?: "afterLabels"|"afterBar"|"above"|"below" (scaleBar: default afterLabels, after the last label), "##,
                r##""fit"?: "adjustDivisionValue"|"adjustDivision"|"adjustWidth" (scaleBar: when round divisions don't fill the box: adjustDivisionValue, the default, divides the roundest length that fits, so a division may not be round; adjustDivision keeps a round division and adds or drops divisions, at most ten in all, the divisions before zero included, to fill the width; adjustWidth keeps the divisions and draws the bar only as wide as they need) (a value out of range or not one of the names is refused, and these are refused for other kinds), "##,
                r##""x"?, "##,
                r##""y"?, "##,
                r##""w"?, "##,
                r##""h"?: mm, "##,
                r##""background"?: {"fill"?: "#RRGGBB"|"#RRGGBBAA" (default white), "outline"?: colour (default #222222), "width"?: the outline's points (default 0, none), "radius"?: mm, its corners (0 to 50), "padding"?: mm it reaches past the box on every side (0 to 50)} | null (a box drawn behind the element, turned with a turned text; none by default; a map frame takes none, so it is refused for grid and extentIndicator)}"##,
            ),
        ),
        C::new("layout.update", "Element Properties", "Publish › Element", update).inputs(update_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).params(
            concat!(
                r##"{"layout"?, "##,
                r##""element": id, "##,
                r##""x"?, "##,
                r##""y"?, "##,
                r##""w"?, "##,
                r##""h"?: mm, "##,
                r##""name"?, "##,
                r##""visible"?, "##,
                r##""locked"?: bool (a locked element takes no other change until unlocked; "locked": false unlocks it and may come with changes), "##,
                r##""text"? (a text's words; {count:Layer}, {sum:Layer:field}, {mean:Layer:field}, {min:Layer:field} and {max:Layer:field} show the layer's figure, as {scale} shows the scale: of the first map frame's map, over the features the layer's definition query keeps, with the layout's decimal mark; one with no figure shows as written and says why), "##,
                r##""size"?, "##,
                r##""color"? (text, line, rectangle, ellipse: its outline; scaleBar: its lines, dark divisions and labels; legend, colorbar, table, chart: its ink, as in layout.addElement, null giving it the default again; refused for other elements), "##,
                r##""bold"?, "##,
                r##""italic"?, "##,
                r##""align"?, "##,
                r##""letterSpacing"?, "##,
                r##""rotation"?: degrees clockwise (text, as in layout.addElement; refused for other elements), "##,
                r##""wrap"?: bool (text, as in layout.addElement; refused for other elements), "##,
                r##""valign"?, "##,
                r##""lineSpacing"?, "##,
                r##""case"? (text, as in layout.addElement; null leaves each as it is; refused for other elements), "##,
                r##""font"?: "sans"|"serif"|an installed font family's name, "##,
                r##""fallback"?: "sans"|"serif" (text, as in layout.addElement; refused for other elements), "##,
                r##""title"? (legend, colorbar; chart: its own title ("" for none; null giving it the title it is given again)), "##,
                r##""allClasses"?: bool (legend), "##,
                r##""hideOther"?: bool (legend), "##,
                r##""showNoData"?: bool (legend; default false), "##,
                r##""exclude"?: [layer names] (legend; [] lists every layer again), "##,
                r##""labels"?: {"<layer>": "<label>"|null} (legend: what it shows instead of a layer's name, up to 200 characters, a longer one is refused; each entry replaces that layer's label, null or "" shows its name again, {} every layer's; a layer the map hasn't is refused), "##,
                r##""columns"?: 1..3 (legend), "##,
                r##""textSize"?: points, 4 to 100 (legend, scaleBar, as in layout.addElement, null giving each its default again; refused outside that range, and for other elements unless null), "##,
                r##""orientation"? (colorbar, chart), "##,
                r##""ticks"?, "##,
                r##""tickSide"?, "##,
                r##""reverse"?: (colorbar), "##,
                r##""style"?: (northArrow, scaleBar), "##,
                r##""fill"?, "##,
                r##""outline"? (northArrow, rectangle, ellipse, and a scaleBar's fill; refused for other elements; for a rectangle or ellipse, color sets it when outline is not given; a scaleBar's fill is the alternating bar's light divisions), "##,
                r##""radius"?: mm (rectangle: rounded corners), "##,
                r##""width"?: points (rectangle, ellipse, line), "##,
                r##""dash"?: [points, …]|[]|null (line, rectangle, ellipse, as in layout.addElement; [] or null makes it solid; refused, null too, for other elements), "##,
                r##""dir"?, "##,
                r##""head"?, "##,
                r##""angle"?, "##,
                r##""length"? (line: turned or sized about its centre, then moved by any x or y; dir does not come with angle or length), "##,
                r##""units"?, "##,
                r##""divisions"? (scaleBar; at most 10 with divisionsBeforeZero together, more refused), "##,
                r##""subdivisions"?, "##,
                r##""divisionsBeforeZero"?, "##,
                r##""labelPosition"?, "##,
                r##""unitLabel"? (null: the unit's abbreviation again), "##,
                r##""unitLabelPosition"?, "##,
                r##""fit"? (scaleBar, as in layout.addElement; refused for other elements, a null unitLabel too), "##,
                r##""map"?, "##,
                r##""scale"?, "##,
                r##""center"?: [lon, lat], "##,
                r##""bearing"?: degrees clockwise from north, "##,
                r##""pitch"?: degrees of tilt (0 to 85), "##,
                r##""grid"?: {grid options as in layout.addElement} | false (map frame; a grid's "labelSize" goes in it, and is refused outside it unless null), "##,
                r##""extentIndicator"?: {"color"?, "width"?: points} | false (map frame: its extent indicator's style, or removes it), "##,
                r##""border"?: {"color"?: "#RRGGBB"|"#RRGGBBAA", "width"?: points, 0 to 20, 0 for none} | null (map frame: the outline along its shape, default #303030 and 0.8, the keys not given keeping theirs; null gives the default back; refused for other elements), "##,
                r##""layer"?: (colorbar: "" for the top visible raster with a colour scale, not drawn by classes), "##,
                r##""field"?, "##,
                r##""label"?, "##,
                r##""group"?, "##,
                r##""statistic"?, "##,
                r##""order"?, "##,
                r##""bars"?, "##,
                r##""other"?, "##,
                r##""stack"? (chart, as in layout.addElement: "" for a label, group or stack takes it away; on a new layer a label, group or stack field it hasn't is let go, a stack the chart can no longer draw (ungrouped, grouped by the stack field, or not summing or counting) too, and a field not named that it no longer measures gives way to the first numeric field, which warnings say; refused for other elements), "##,
                r##""values"?: bool, "##,
                r##""decimals"?: 0..6|"auto" (a scaleBar's 0..3|"auto", as in layout.addElement), "##,
                r##""barColor"? (null giving the default again), "##,
                r##""classes"?: bool, "##,
                r##""labelAlign"?: "left"|"right", "##,
                r##""mirror"?: bool, "##,
                r##""max"?: a number above 0|"auto"|null (null or "auto" giving it its bars' own again), "##,
                r##""barOutline"?: {"color"?, "width"?: points, 0 to 5, a wider one held to 5, the keys not given keeping the outline's}|null (null or a width of 0 for none), "##,
                r##""hideLabels"?: bool (chart, as in layout.addElement: null leaves each other one as it is; refused for other elements unless null), "##,
                r##""rows"?, "##,
                r##""background"?: {"fill"?, "outline"?, "width"?, "radius"?, "padding"?, as in layout.addElement, the keys not given keeping theirs} | null (removes it); a map frame takes none}"##,
            ),

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
            let named = maps_layouts_name(&s.project);
            let l = active(s, p)?;
            // A frame's surrounds go with it: a locked one keeps the frame too.
            if let Some(locked) = l.locked_in_removal(id).map(|e| e.id) {
                unlocked(l, locked)?;
            }
            if !l.remove(id) {
                return Err(format!("no element {id}"));
            }
            say_map_texts(s, &named);
            Ok(Value::Null)
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
            let bytes = crate::project::layout_file_bytes(&l)?;
            rustgis_data::io::write_atomic(Path::new(path), &bytes).map_err(|e| format!("{path}: {e}"))?;
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
    let named = maps_layouts_name(&s.project);
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
    // The first map frame is the lowest: sending a frame back, or another forward, can change it.
    say_map_texts(s, &named);
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
            // The token, not the map's name as typed text: the title follows a rename of the map in
            // the first map frame, as the layout draws it.
            l.add("Title", (m, m, w - 2.0 * m, 14.0), ElementKind::text("{map}", 22.0, Rgba::rgb(0x1F, 0x29, 0x37), true));
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
