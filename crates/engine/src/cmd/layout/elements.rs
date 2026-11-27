//! Adding and duplicating elements, and the style shapes share.

use super::*;

/// Copies an element, offset a little so the copy shows, on top of the others.
pub(in crate::cmd::layout) fn duplicate_element(s: &mut Session, p: &Value) -> CmdResult {
    let id = element_id(p)?;
    let (dx, dy) = (f(p, "dx").unwrap_or(5.0), f(p, "dy").unwrap_or(5.0));
    if dx.abs() > 5000.0 || dy.abs() > 5000.0 {
        return Err("the copy's offset is up to 5000 mm".into());
    }
    let li = index(s, p)?;
    let l = active(s, p)?;
    let e = l.element(id).cloned().ok_or_else(|| format!("no element {id}"))?;
    let name = numbered(&e.name, |n| l.elements.iter().any(|x| x.name == n));
    let copy = l.add(name.clone(), (e.x + dx, e.y + dy, e.w, e.h), e.kind);
    if let Some(c) = l.element_mut(copy) {
        c.visible = e.visible;
        c.background = e.background;
    }
    crate::cmd::say_name_cut(s, "Duplicate Element", "the copy's name", &e.name, &name);
    // Its offset can take a text's ink past the page's edge, as a move does (#2703); the lines it
    // loses are the original's, said when it was set.
    say_text_fit(s, li, copy, false);
    Ok(json!({"element": copy, "name": name}))
}

/// A shape's "width" in points: `default` when it's not given (or null), an error when it's not a
/// number.
pub(in crate::cmd::layout) fn shape_width(p: &Value, default: f64) -> Result<f64, String> {
    match p.get("width").filter(|v| !v.is_null()) {
        Some(_) => f(p, "width").ok_or_else(|| "width is a number of points".to_string()),
        None => Ok(default),
    }
}

/// A line's "head": `default` when it's not given (or null), an error when it's not an
/// arrowhead's name.
pub(in crate::cmd::layout) fn line_head(p: &Value, default: LineHead) -> Result<LineHead, String> {
    match p.get("head").filter(|v| !v.is_null()) {
        Some(v) => v.as_str().and_then(LineHead::from_name).ok_or_else(|| format!("unknown arrowhead {v} (none, end, start or both)")),
        None => Ok(default),
    }
}

/// A number given as `k`, when it's given (and not null): an error when it's not a number.
pub(in crate::cmd::layout) fn given_num(p: &Value, k: &str, what: &str) -> Result<Option<f64>, String> {
    match p.get(k).filter(|v| !v.is_null()) {
        Some(_) => f(p, k).map(Some).ok_or_else(|| format!("{k} is {what}")),
        None => Ok(None),
    }
}

/// A line's "dir": `default` when it's not given (or null), an error when it's not a direction's
/// name.
pub(in crate::cmd::layout) fn line_dir(p: &Value, default: LineDir) -> Result<LineDir, String> {
    match p.get("dir").filter(|v| !v.is_null()) {
        Some(v) => v
            .as_str()
            .and_then(LineDir::from_name)
            .ok_or_else(|| format!("unknown line direction {v} (horizontal, vertical, falling or rising)")),
        None => Ok(default),
    }
}

/// The element's box: given values, else a default inside the page.
pub(in crate::cmd::layout) fn place(p: &Value, l: &Layout, (dx, dy, dw, dh): (f64, f64, f64, f64)) -> Result<(f64, f64, f64, f64), String> {
    let v = |k: &str, d: f64| f(p, k).unwrap_or(d);
    let (x, y, w, h) = (v("x", dx), v("y", dy), v("w", dw.min(l.width - 10.0).max(5.0)), v("h", dh.min(l.height - 10.0).max(5.0)));
    if w <= 0.0 || h <= 0.0 || w > 5000.0 || h > 5000.0 || x.abs() > 5000.0 || y.abs() > 5000.0 {
        return Err("element sizes are more than 0 and up to 5000 mm".into());
    }
    Ok((x, y, w, h))
}

/// Said when a map frame is given a background: it draws its map and its paper itself.
pub(in crate::cmd::layout) const NO_FRAME_BACKGROUND: &str = "a map frame takes no background: it draws its map and the paper behind it";
/// Said when a grid or an extent indicator is added with a background: they go on a map frame.
pub(in crate::cmd::layout) const NO_OVERLAY_BACKGROUND: &str =
    "a grid or an extent indicator goes on a map frame, which takes no background";
/// What an element's `background` object takes.
pub(in crate::cmd::layout) const BACKGROUND_KEYS: [&str; 5] = ["fill", "outline", "width", "radius", "padding"];

/// What a command's `background` asks for (#2257).
pub(in crate::cmd::layout) enum BackgroundParam {
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
pub(in crate::cmd::layout) fn element_background(p: &Value, current: Option<Background>) -> Result<BackgroundParam, String> {
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
pub(in crate::cmd::layout) fn corner_radius(p: &Value, d: f64) -> Result<f64, String> {
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
pub(in crate::cmd::layout) enum Shape {
    Rectangle,
    Ellipse,
    Line,
}

impl Shape {
    pub(in crate::cmd::layout) fn of_kind(kind: &ElementKind) -> Option<Shape> {
        match kind {
            ElementKind::Rectangle { .. } => Some(Shape::Rectangle),
            ElementKind::Ellipse { .. } => Some(Shape::Ellipse),
            ElementKind::Line { .. } => Some(Shape::Line),
            _ => None,
        }
    }

    pub(in crate::cmd::layout) fn of_name(kind: &str) -> Option<Shape> {
        match kind {
            "rectangle" => Some(Shape::Rectangle),
            "ellipse" | "circle" => Some(Shape::Ellipse),
            "line" | "arrow" => Some(Shape::Line),
            _ => None,
        }
    }
}

/// What a layout command holds to a limit instead of taking as given (#2639): a shape's outline
/// `width` (0 to 50 points; a line's from 0.1), a rectangle's `radius` (up to 50 mm), a chart's
/// `barOutline` width (0 to 5 points) and a background's width, radius and padding, each said with
/// the value asked and the one kept. A value the command refuses (not a number, negative where that
/// isn't taken) isn't said here.
pub(in crate::cmd::layout) fn held_to_limits(p: &Value, shape: Option<Shape>) -> Vec<String> {
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
    if let Some(w) = p.get("barOutline").and_then(|o| f(o, "width")) {
        hold("barOutline width", w, w.clamp(0.0, MAX_CHART_OUTLINE_WIDTH), &format!("0 to {MAX_CHART_OUTLINE_WIDTH} points"));
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
pub(in crate::cmd::layout) fn held_width(p: &Value, what: &str, range: (f64, f64)) -> Option<String> {
    let w = f(p, "width")?;
    let kept = w.clamp(range.0, range.1);
    (w != kept).then(|| format!("{what} {w} held to {kept} ({} to {} points)", range.0, range.1))
}

/// Says what [`held_to_limits`] found, in one notice for the command `title`.
pub(in crate::cmd::layout) fn say_held(s: &mut Session, title: &str, held: Vec<String>) {
    if !held.is_empty() {
        s.notify(format!("{title}: {}", held.join("; ")), false);
    }
}

/// The colour `p` gives as `k`, `None` when it gives none (absent or null). Any other value is
/// refused rather than ignored.
pub(in crate::cmd::layout) fn given_color(p: &Value, k: &str) -> Result<Option<Rgba>, String> {
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

pub(in crate::cmd::layout) fn add_element(s: &mut Session, p: &Value) -> CmdResult {
    let kind = ps(p, "kind").ok_or("kind is required")?;
    if kind != "legend" && kind != "scaleBar" && p.get("textSize").is_some_and(|v| !v.is_null()) {
        return Err(ONLY_LAYOUT_TEXT_SIZE.into());
    }
    if kind != "grid" && p.get("labelSize").is_some_and(|v| !v.is_null()) {
        return Err(ONLY_GRID_LABEL_SIZE.into());
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
    if !matches!(kind, "text" | "dynamicText") && gives_text_block(p) {
        return Err(ONLY_TEXT_BLOCK.into());
    }
    if kind != "chart" && gives_chart_options_to(p, kind == "scaleBar") {
        return Err(ONLY_CHART_OPTIONS.into());
    }
    // "unitLabel": null too, as layout.update refuses it, since it means something on a scale bar.
    if kind != "scaleBar" && (gives_scale_bar_details(p) || p.get("unitLabel").is_some()) {
        return Err(ONLY_SCALE_BAR_DETAILS.into());
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
    let image = if kind == "picture" { Some(crate::cmd::graphics::picture(p)?) } else { None };
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
            let title = crate::cmd::within("title", "a legend's title", ps(p, "title").unwrap_or("Legend"), 200)?;
            let all_classes = b(p, "allClasses").unwrap_or(false);
            let hide_other = b(p, "hideOther").unwrap_or(false);
            let show_no_data = b(p, "showNoData").unwrap_or(false);
            let exclude = legend_exclude(s, l, frame, p)?.unwrap_or_default();
            let columns = legend_columns_param(p)?.unwrap_or(1);
            let labels = match legend_labels(s, l, frame, p)? {
                Some(given) => with_labels(&BTreeMap::new(), given)?,
                None => BTreeMap::new(),
            };
            let text_size =
                text_size_param(p, "textSize", rustgis_map::layout::LEGEND_TEXT_PT)?.unwrap_or(rustgis_map::layout::LEGEND_TEXT_PT);
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
                Some(t) => crate::cmd::within("title", "a colorbar's title", t, 200)?,
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
            let text =
                crate::cmd::within("text", "a text element's text", ps(p, "text").unwrap_or(default), rustgis_map::layout::MAX_TEXT_CHARS)?;
            let fonts = rustgis_map::graphics::FONT_SIZE;
            let size = f(p, "size").unwrap_or(12.0).clamp(*fonts.start(), *fonts.end());
            let (italic, align, letter_spacing) = text_style(p, false, TextAlign::Left, 0.0)?;
            let (font, font_name) = text_font(p, (FontFamily::Sans, None))?;
            let rotation = text_rotation(p)?.unwrap_or(0.0);
            let wrap = text_wrap(p)?.unwrap_or(false);
            let (valign, line_spacing, case) = text_block_settings(p, (TextVAlign::Top, LINE_SPACING, TextCase::None))?;
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
                valign,
                line_spacing,
                case,
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
                mirror: o.mirror,
                max: o.max,
                hide_labels: o.hide_labels,
                bar_outline: o.bar_outline,
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
            only_add_keys(p, "an extent indicator", ADD_EXTENT_INDICATOR_KEYS)?;
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
            only_add_keys(p, "a grid", ADD_GRID_KEYS)?;
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
                crate::cmd::say_crs_note(s, text);
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
    say_text_fit(s, i, id, true);
    say_text_statistics(s, i, id);
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

pub(in crate::cmd::layout) fn orientation(p: &Value) -> Result<Option<Orientation>, String> {
    orientation_of(p, "orientation")
}

pub(in crate::cmd::layout) fn orientation_of(p: &Value, key: &str) -> Result<Option<Orientation>, String> {
    match ps(p, key).map(|o| o.trim().to_ascii_lowercase()) {
        None => Ok(None),
        Some(o) if o == "horizontal" => Ok(Some(Orientation::Horizontal)),
        Some(o) if o == "vertical" => Ok(Some(Orientation::Vertical)),
        Some(o) => Err(format!("unknown orientation '{o}' (horizontal or vertical)")),
    }
}

pub(in crate::cmd::layout) fn tick_side(p: &Value) -> Result<Option<TickSide>, String> {
    match ps(p, "tickSide") {
        None => Ok(None),
        Some(t) => TickSide::from_name(t).map(Some).ok_or_else(|| format!("unknown tickSide '{t}' (below, above, right or left)")),
    }
}

pub(in crate::cmd::layout) fn ticks(p: &Value) -> Option<usize> {
    f(p, "ticks").filter(|v| v.is_finite()).map(|v| v.round().clamp(2.0, MAX_COLORBAR_TICKS as f64) as usize)
}

/// An extent indicator's outline colour and width (points) from `color` and `width` in `p`, each
/// keeping the frame's (`kind`) when not given; one given but not a colour or a number is an
/// error, not left as it was.
pub(in crate::cmd::layout) fn indicator_style(p: &Value, kind: Option<&ElementKind>) -> Result<(Rgba, f64), String> {
    let current = match kind {
        Some(ElementKind::MapFrame { extent_color, extent_width, .. }) => (*extent_color, *extent_width),
        _ => (rustgis_map::layout::INDICATOR_COLOR, rustgis_map::layout::INDICATOR_WIDTH),
    };
    outline_style(p, current, (rustgis_map::layout::MIN_INDICATOR_WIDTH, rustgis_map::layout::MAX_INDICATOR_WIDTH))
}

/// A map frame's border colour and width (points, 0 for none) from `color` and `width` in `p`,
/// each keeping the frame's when not given (#2557), as [`indicator_style`] reads them.
pub(in crate::cmd::layout) fn border_style(p: &Value, kind: &ElementKind) -> Result<(Rgba, f64), String> {
    let current = match kind {
        ElementKind::MapFrame { border_color, border_width, .. } => (*border_color, *border_width),
        _ => (rustgis_map::layout::FRAME_BORDER_COLOR, rustgis_map::layout::FRAME_BORDER_WIDTH),
    };
    outline_style(p, current, (0.0, rustgis_map::layout::MAX_FRAME_BORDER_WIDTH))
}

/// An outline's colour and width (points, kept within `range`) from `color` and `width` in `p`,
/// each keeping `current`'s when not given.
pub(in crate::cmd::layout) fn outline_style(p: &Value, (c, w): (Rgba, f64), range: (f64, f64)) -> Result<(Rgba, f64), String> {
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

/// What a `dash` given for an element that draws no line or outline is told.
pub(in crate::cmd::layout) const ONLY_DASH: &str = "only lines, arrows, rectangles, ellipses and circles take a dash";

/// A line's or an outline's `dash` when given, read as a line layer's stroke reads it
/// (`layer.lineStrokes`): `Some(None)` for solid (`null` or `[]`), `Some(Some(_))` for a pattern,
/// `None` when not given; refused when it isn't one.
pub(in crate::cmd::layout) fn element_dash(p: &Value) -> Result<Option<Option<Dash>>, String> {
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
pub(in crate::cmd::layout) const ONLY_RECTANGLE_RADIUS: &str = "only a rectangle rounds its corners";

/// Colours are refused for the elements that don't draw them, as a family is, so a script doesn't
/// take them as applied: "color" is a text's, a line's, an arrow's, a scale bar's or a shape's
/// outline (a rectangle's or an ellipse's), a legend's, colorbar's, table's or chart's ink (#2257),
/// and, when adding, also an extent indicator's or a grid's.
pub(in crate::cmd::layout) const ONLY_COLOR_ADDED: &str = "only text, lines, arrows, rectangles, ellipses, circles, extent indicators, grids, scale bars, \
legends, colorbars, tables and charts take a color";
pub(in crate::cmd::layout) const ONLY_COLOR_UPDATED: &str =
    "only text, lines, arrows, rectangles, ellipses, circles, scale bars, legends, colorbars, tables and charts take a color";
pub(in crate::cmd::layout) const ONLY_FILL: &str = "only north arrows, rectangles, ellipses, circles and scale bars take a fill";
pub(in crate::cmd::layout) const ONLY_OUTLINE: &str = "only north arrows, rectangles, ellipses and circles take an outline";

/// Whether `p` gives `k`: `null` is not given.
pub(in crate::cmd::layout) fn gives(p: &Value, k: &str) -> bool {
    p.get(k).is_some_and(|v| !v.is_null())
}

/// Keys read by `layout.addElement` for an extent indicator (#2890).
pub(in crate::cmd::layout) const ADD_EXTENT_INDICATOR_KEYS: &[&str] = &["layout", "kind", "frame", "of", "color", "width"];

/// Keys read by `layout.addElement` for a grid (#2890).
pub(in crate::cmd::layout) const ADD_GRID_KEYS: &[&str] = &[
    "layout",
    "kind",
    "frame",
    "type",
    "crs",
    "step",
    "short",
    "mgrs",
    "color",
    "width",
    "labels",
    "labelSize",
    "style",
    "tickLength",
    "format",
    "hemisphere",
    "edges",
    "labelOrientation",
    "off",
];

/// Refuses any non-null key in `p` that `what` does not read, naming it and what `what` reads.
pub(in crate::cmd::layout) fn only_add_keys(p: &Value, what: &str, allowed: &[&str]) -> Result<(), String> {
    let Some(obj) = p.as_object() else { return Ok(()) };
    if let Some(key) = obj.keys().find(|k| !obj.get(*k).is_none_or(Value::is_null) && !allowed.contains(&k.as_str())) {
        return Err(format!("layout.addElement: {what} takes no \"{key}\" (it reads {})", allowed.join(", ")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rustgis_map::Rgba;
    use rustgis_map::layout::{CoordFormat, ElementKind, GridEdges, GridKind, GridStyle, Orientation};
    use serde_json::{Value, json};

    use crate::{Project, Session};

    fn layout_session() -> (Session, u32) {
        let mut s = Session::new(Project::default());
        s.run("layout.new", &json!({"name": "L", "template": "map"})).unwrap();
        let frame = s.project.layouts[0].first_frame().unwrap();
        (s, frame)
    }

    #[test]
    fn add_element_grid_refuses_unread_parameters_without_mutating() {
        let (mut s, frame) = layout_session();
        let before_layout = s.project.layouts[0].clone();
        let before_undo = s.undo_depth();

        for (key, val) in [
            ("x", json!(10)),
            ("y", json!(10)),
            ("w", json!(50)),
            ("h", json!(50)),
            ("text", json!("hi")),
            ("angle", json!(30)),
            ("divisions", json!(4)),
            ("of", json!(frame)),
            ("bold", json!(true)),
            ("units", json!("meters")),
            ("rows", json!(5)),
        ] {
            let err = s.run("layout.addElement", &json!({"kind": "grid", "step": 20, key: val})).unwrap_err();
            assert!(err.starts_with(&format!("layout.addElement: a grid takes no \"{key}\" (it reads ")), "{key}: {err}");
            assert!(err.contains("layout, kind, frame, type, crs, step"), "{key}: {err}");
            assert_eq!(s.project.layouts[0], before_layout, "{key} mutated layout");
            assert_eq!(s.undo_depth(), before_undo, "{key} pushed an undo step");
        }

        // Exact issue #2890 reproduction payload (BTreeMap checks keys in alphabetical order: "angle" first).
        let err = s
            .run(
                "layout.addElement",
                &json!({"kind": "grid", "step": 20, "x": 10, "y": 10, "w": 50, "h": 50, "text": "hi", "angle": 30, "divisions": 4}),
            )
            .unwrap_err();
        assert!(err.starts_with("layout.addElement: a grid takes no \"angle\" (it reads "), "{err}");
        assert_eq!(s.project.layouts[0], before_layout);
        assert_eq!(s.undo_depth(), before_undo);
    }

    #[test]
    fn add_element_extent_indicator_refuses_unread_parameters_without_mutating() {
        let (mut s, frame) = layout_session();
        let inset = s.run("layout.addFrame", &json!({"x": 150, "y": 150, "w": 40, "h": 40})).unwrap()["element"].as_u64().unwrap() as u32;
        let before_layout = s.project.layouts[0].clone();
        let before_undo = s.undo_depth();

        for (key, val) in [
            ("x", json!(10)),
            ("y", json!(10)),
            ("w", json!(50)),
            ("h", json!(50)),
            ("text", json!("hi")),
            ("step", json!(5)),
            ("bold", json!(true)),
            ("type", json!("measured")),
            ("off", json!(true)),
            ("angle", json!(30)),
            ("divisions", json!(4)),
        ] {
            let err = s.run("layout.addElement", &json!({"kind": "extentIndicator", "frame": inset, "of": frame, key: val})).unwrap_err();
            assert_eq!(
                err,
                format!("layout.addElement: an extent indicator takes no \"{key}\" (it reads layout, kind, frame, of, color, width)"),
                "{key}: {err}"
            );
            assert_eq!(s.project.layouts[0], before_layout, "{key} mutated layout");
            assert_eq!(s.undo_depth(), before_undo, "{key} pushed an undo step");
        }

        // Exact issue #2890 reproduction payload (BTreeMap checks keys in alphabetical order: "bold" first).
        let err = s
            .run(
                "layout.addElement",
                &json!({"kind": "extentIndicator", "frame": inset, "of": frame, "x": 10, "w": 50, "text": "hi", "step": 5, "bold": true}),
            )
            .unwrap_err();
        assert_eq!(err, "layout.addElement: an extent indicator takes no \"bold\" (it reads layout, kind, frame, of, color, width)");
        assert_eq!(s.project.layouts[0], before_layout);
        assert_eq!(s.undo_depth(), before_undo);
    }

    #[test]
    fn add_element_grid_and_extent_indicator_allow_null_unread_keys_and_all_valid_keys_silently() {
        let (mut s, frame) = layout_session();
        let inset = s.run("layout.addFrame", &json!({"x": 150, "y": 150, "w": 40, "h": 40})).unwrap()["element"].as_u64().unwrap() as u32;

        // Explicit null values for unread keys count as not given and emit no notice.
        let mark = s.notice_mark();
        s.run(
            "layout.addElement",
            &json!({"kind": "grid", "step": 20, "x": null, "y": null, "w": null, "h": null, "text": null, "angle": null, "divisions": null, "of": null}),
        )
        .unwrap();
        s.run(
            "layout.addElement",
            &json!({"kind": "extentIndicator", "frame": inset, "of": frame, "x": null, "w": null, "text": null, "step": null, "bold": null}),
        )
        .unwrap();
        assert!(s.notices_since(mark).is_empty(), "{:?}", s.notices_since(mark));

        // Clean calls covering every allowed key for grid (graticule + measured/MGRS + off) and extentIndicator.
        let mark = s.notice_mark();
        s.run(
            "layout.addElement",
            &json!({
                "layout": 0,
                "kind": "grid",
                "frame": frame,
                "type": "graticule",
                "step": 10,
                "color": "#334455",
                "width": 1.0,
                "labels": true,
                "labelSize": 10,
                "style": "linesAndTicks",
                "tickLength": 2.5,
                "format": "dms",
                "hemisphere": true,
                "edges": ["top", "bottom", "left", "right"],
                "labelOrientation": "vertical",
                "off": false
            }),
        )
        .unwrap();
        assert!(s.notices_since(mark).is_empty(), "{:?}", s.notices_since(mark));
        let Some(ElementKind::MapFrame { grid: Some(g), .. }) = s.project.layouts[0].element(frame).map(|e| &e.kind) else {
            panic!("expected grid on frame");
        };
        assert_eq!(g.kind, GridKind::Graticule);
        assert_eq!(g.step, 10.0);
        assert_eq!(g.color, Rgba::rgb(0x33, 0x44, 0x55));
        assert_eq!(g.width, 1.0);
        assert!(g.labels);
        assert_eq!(g.label_size, 10.0);
        assert_eq!(g.style, GridStyle::LinesAndTicks);
        assert_eq!(g.tick_length, 2.5);
        assert_eq!(g.format, CoordFormat::Dms);
        assert!(g.hemisphere);
        assert_eq!(g.edges, GridEdges { top: true, bottom: true, left: true, right: true });
        assert_eq!(g.label_orientation, Orientation::Vertical);

        let mark = s.notice_mark();
        s.run(
            "layout.addElement",
            &json!({
                "layout": "L",
                "kind": "grid",
                "frame": frame,
                "type": "measured",
                "crs": "EPSG:32632",
                "step": 1000,
                "short": true,
                "mgrs": true
            }),
        )
        .unwrap();
        assert!(s.notices_since(mark).is_empty(), "{:?}", s.notices_since(mark));
        let Some(ElementKind::MapFrame { grid: Some(g), .. }) = s.project.layouts[0].element(frame).map(|e| &e.kind) else {
            panic!("expected measured grid on frame");
        };
        assert_eq!(g.kind, GridKind::Measured);
        assert_eq!(g.crs.as_deref(), Some("EPSG:32632"));
        assert_eq!(g.step, 1000.0);
        assert!(g.short);
        assert!(g.mgrs);

        let mark = s.notice_mark();
        s.run("layout.addElement", &json!({"layout": 0, "kind": "grid", "frame": frame, "off": true})).unwrap();
        assert!(s.notices_since(mark).is_empty(), "{:?}", s.notices_since(mark));
        assert!(matches!(s.project.layouts[0].element(frame).map(|e| &e.kind), Some(ElementKind::MapFrame { grid: None, .. })));

        let mark = s.notice_mark();
        s.run(
            "layout.addElement",
            &json!({"layout": 0, "kind": "extentIndicator", "frame": inset, "of": frame, "color": "#1F4E9A", "width": 1.5}),
        )
        .unwrap();
        assert!(s.notices_since(mark).is_empty(), "{:?}", s.notices_since(mark));
        assert!(matches!(
            s.project.layouts[0].element(inset).map(|e| &e.kind),
            Some(ElementKind::MapFrame { extent_of: Some(of), extent_color, extent_width, .. })
                if *of == frame && *extent_color == Rgba::rgb(0x1F, 0x4E, 0x9A) && *extent_width == 1.5
        ));

        // Earlier kind-specific checks keep their existing messages, including intentional null refusals.
        assert_eq!(s.run("layout.addElement", &json!({"kind": "grid", "fill": "#FFFFFF"})).unwrap_err(), super::ONLY_FILL);
        assert_eq!(
            s.run("layout.addElement", &json!({"kind": "extentIndicator", "frame": inset, "of": frame, "outline": "#FFFFFF"})).unwrap_err(),
            super::ONLY_OUTLINE
        );
        assert_eq!(
            s.run("layout.addElement", &json!({"kind": "grid", "background": {"fill": "#FFFFFF"}})).unwrap_err(),
            super::NO_OVERLAY_BACKGROUND
        );
        assert_eq!(
            s.run("layout.addElement", &json!({"kind": "extentIndicator", "frame": inset, "of": frame, "background": {"fill": "#FFFFFF"}}))
                .unwrap_err(),
            super::NO_OVERLAY_BACKGROUND
        );
        assert_eq!(s.run("layout.addElement", &json!({"kind": "grid", "dash": Value::Null})).unwrap_err(), super::ONLY_DASH);
        assert_eq!(
            s.run("layout.addElement", &json!({"kind": "grid", "unitLabel": Value::Null})).unwrap_err(),
            super::ONLY_SCALE_BAR_DETAILS
        );
    }
}
