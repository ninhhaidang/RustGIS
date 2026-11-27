//! `layout.update`: an element's properties.

use super::*;

/// A north arrow or scale bar with the options in `p` over `kind`'s; `None` for other elements
/// or when `p` has none. A bad value is an error, leaving nothing changed.
pub(in crate::cmd::layout) fn restyle(kind: &ElementKind, p: &Value) -> Result<Option<ElementKind>, String> {
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
        ElementKind::ScaleBar {
            frame,
            units,
            divisions,
            style,
            color: c,
            fill,
            text_size,
            subdivisions,
            divisions_before_zero,
            label_position,
            unit_label,
            unit_label_position,
            decimals,
            fit,
        } => {
            if ["style", "units", "divisions", "color", "fill", "textSize", "decimals"]
                .iter()
                .chain(SCALE_BAR_KEYS.iter())
                .all(|k| p.get(k).is_none())
            {
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
            let text_size = text_size_param(p, "textSize", SCALE_BAR_TEXT_PT)?.unwrap_or(*text_size);
            // Its details, each read before any is set: one part is the division whole.
            let subdivisions =
                given_count(p, "subdivisions", MAX_SCALE_SUBDIVISIONS)?.map_or(*subdivisions, |n| if n == 1 { 0 } else { n });
            let divisions_before_zero = given_count(p, "divisionsBeforeZero", MAX_SCALE_BEFORE_ZERO)?.unwrap_or(*divisions_before_zero);
            // A bar draws at most ten divisions in all: more is refused, not quietly cut.
            if divisions.saturating_add(divisions_before_zero) > MAX_SCALE_DIVISIONS {
                return Err("divisions and divisionsBeforeZero are at most 10 together".into());
            }
            // As a chart's: "auto", the decimals each label needs, or that many with the thousands
            // apart, a whole number given as the counts are; null leaves them as they are.
            let decimals = match p.get("decimals").filter(|v| !v.is_null()) {
                None => *decimals,
                Some(v) if v.as_str().is_some_and(|t| t.trim().eq_ignore_ascii_case("auto")) => None,
                Some(_) => given_count(p, "decimals", MAX_SCALE_DECIMALS)
                    .ok()
                    .flatten()
                    .and_then(|n| u8::try_from(n).ok())
                    .map(Some)
                    .ok_or_else(|| format!("decimals is \"auto\" or a whole number from 0 to {MAX_SCALE_DECIMALS}"))?,
            };
            let label_position =
                given_choice(p, "labelPosition", ScaleLabelPosition::from_name, "above or below")?.unwrap_or(*label_position);
            let unit_label_position =
                given_choice(p, "unitLabelPosition", UnitLabelPosition::from_name, "afterLabels, afterBar, above or below")?
                    .unwrap_or(*unit_label_position);
            let fit = given_choice(p, "fit", ScaleBarFit::from_name, "adjustDivisionValue, adjustDivision or adjustWidth")?.unwrap_or(*fit);
            // Null gives back the unit's abbreviation; "" prints none.
            let unit_label = match p.get("unitLabel") {
                None => unit_label.clone(),
                Some(Value::Null) => None,
                Some(Value::String(t)) => {
                    let t = crate::cmd::within("unitLabel", "a scale bar's unit label", t, MAX_UNIT_LABEL_CHARS)?;
                    if t.contains(['\n', '\r']) {
                        return Err("unitLabel is one line".into());
                    }
                    Some(t)
                }
                Some(_) => return Err("unitLabel is text, or null for the unit's abbreviation".into()),
            };
            Ok(Some(ElementKind::ScaleBar {
                frame: *frame,
                units,
                divisions,
                style,
                color,
                fill,
                text_size,
                subdivisions,
                divisions_before_zero,
                label_position,
                unit_label,
                unit_label_position,
                decimals,
                fit,
            }))
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

pub(in crate::cmd::layout) fn update(s: &mut Session, p: &Value) -> CmdResult {
    let id = element_id(p)?;
    // The first frame's map as it is, to say what the texts lose only when it changes (#2853).
    let map_before = index(s, p).ok().and_then(|li| first_frame_map(s, li));
    // Checked before anything changes, so a bad value leaves the element as it was.
    let new_center = match p.get("center") {
        Some(c) => {
            let a = c.as_array().ok_or("center is [lon, lat]")?;
            match (a.first().and_then(Value::as_f64), a.get(1).and_then(Value::as_f64)) {
                // A pole too: the frame's map decides where it's drawn (#2684).
                (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Some(Coord::new(x.clamp(-540.0, 540.0), y.clamp(-90.0, 90.0))),
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
    let new_name = crate::cmd::name_given(p, "an element's name", 200)?;
    let new_text =
        ps(p, "text").map(|t| crate::cmd::within("text", "a text element's text", t, rustgis_map::layout::MAX_TEXT_CHARS)).transpose()?;
    let new_title = ps(p, "title").map(|t| crate::cmd::within("title", "a legend's, colorbar's or chart's title", t, 200)).transpose()?;
    let li = index(s, p)?;
    // What a publication's scale bar box was measured from, before this change.
    let basis = scale_box_basis(s, li);
    // Locked, it takes no change but being unlocked, which may come with other changes.
    // "dash": null makes a line solid, "background": null takes the background away, and
    // "border": null gives a map frame its default border,
    // "color": null gives a legend, colorbar, table or chart its default ink, "title",
    // "barColor", "max" or "barOutline": null a chart its given title, default bars, its bars'
    // own scale or no outline, and "unitLabel": null a scale bar its unit's abbreviation
    // ([`ElementKind::null_resets`]), so they are changes too.
    let current = s.project.layouts.get(li).and_then(|l| l.element(id)).map(|e| &e.kind);
    let bar = matches!(current, Some(ElementKind::ScaleBar { .. }));
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
    // A text's block alignment, line spacing and case, checked before anything changes.
    let new_block = match kind_now {
        Some(ElementKind::Text { valign, line_spacing, case, .. }) => Some(text_block_settings(p, (*valign, *line_spacing, *case))?),
        _ if gives_text_block(p) => return Err(ONLY_TEXT_BLOCK.into()),
        _ => None,
    };
    // "unitLabel": null too, since it means something on a scale bar.
    if !bar && (gives_scale_bar_details(p) || p.get("unitLabel").is_some()) {
        return Err(ONLY_SCALE_BAR_DETAILS.into());
    }
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
    let kind_of = s.project.layouts.get(li).and_then(|l| l.element(id)).map(|e| &e.kind);
    let is_legend = kind_of.is_some_and(|k| matches!(k, ElementKind::Legend { .. }));
    let sized = is_legend || kind_of.is_some_and(|k| matches!(k, ElementKind::ScaleBar { .. }));
    if p.get("textSize").is_some_and(|v| !v.is_null()) && !sized {
        return Err(ONLY_LAYOUT_TEXT_SIZE.into());
    }
    // A grid's label size is one of its options, given in "grid".
    if p.get("labelSize").is_some_and(|v| !v.is_null()) {
        return Err(ONLY_GRID_LABEL_SIZE.into());
    }
    // A legend's; a scale bar's is set with its other options.
    let new_text_size = if is_legend { text_size_param(p, "textSize", rustgis_map::layout::LEGEND_TEXT_PT)? } else { None };
    if p.get("showNoData").is_some_and(|v| !v.is_null()) && !is_legend {
        return Err(ONLY_LEGEND_NO_DATA.into());
    }
    if new_columns.is_some() && !is_legend {
        return Err(ONLY_LEGEND_COLUMNS.into());
    }
    let l = active(s, p)?;
    let e = l.element_mut(id).ok_or_else(|| format!("no element {id}"))?;
    // A text or scale bar with no ink isn't measured: given a colour that shows it, it is told
    // (#2346, #2581).
    let shown = col.is_some_and(|c| c.a() > 0)
        && matches!(&e.kind, ElementKind::Text { color, .. } | ElementKind::ScaleBar { color, .. } if color.a() == 0);
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
        ElementKind::Text {
            text,
            size,
            color,
            bold,
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
        } => {
            if let Some(((i, a, l), (f, n))) = new_text_style {
                (*italic, *align, *letter_spacing, *font, *font_name) = (i, a, l, f, n);
            }
            if let Some(r) = new_rotation {
                *rotation = r;
            }
            if let Some(w) = new_wrap {
                *wrap = w;
            }
            if let Some((v, l, c)) = new_block {
                (*valign, *line_spacing, *case) = (v, l, c);
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
            mirror,
            max,
            hide_labels,
            bar_outline,
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
                (*mirror, *max, *hide_labels, *bar_outline) = (o.mirror, o.max, o.hide_labels, o.bar_outline);
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
    // Given a maximum, it says the bars past it.
    if !chart_said.is_empty()
        || ((checked.is_some() || gives_chart_data(p) || b(p, "classes") == Some(true) || gives(p, "max")) && is_chart(s, li, id))
    {
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
        crate::cmd::say_crs_note(s, text);
    }
    // Its frame drawn at another scale or width, or the text under its bar changed: a
    // publication's scale bar box measured again.
    let given = |keys: &[&str]| keys.iter().any(|k| p.get(*k).is_some_and(|v| !v.is_null()));
    let refit = basis.filter(|basis| {
        // Anything that changes the bar's labels or their room, a text size or unit label given
        // back (`null`) included.
        let restyled = id == basis.bar_id && BAR_FIT_KEYS.iter().any(|k| p.get(*k).is_some());
        // Its frame's centre and map fill the text's tokens and set the bar's ground too.
        (id == basis.frame && given(&["scale", "w", "center", "map"]))
            || (id == basis.text_id && given(&["text", "size", "bold", "case"]))
            || restyled
    });
    // A map frame drawn at another view: its scale bars' lengths and labels change (#2581).
    let frame_view = given(&["scale", "center", "bearing", "pitch", "map"])
        && s.project.layouts.get(li).and_then(|l| l.element(id)).is_some_and(|e| matches!(e.kind, ElementKind::MapFrame { .. }));
    if frame_view {
        frame_view_changed(s, li, id, refit.as_ref());
    } else if let Some(basis) = &refit {
        refit_scale_box(s, li, basis);
    }
    // After the scale box is refitted: a text is measured in the box it is saved with, and a text
    // or scale bar moved, whose ink can then run off the page, too.
    if shown || TEXT_FIT_KEYS.iter().any(|k| p.get(*k).is_some_and(|v| !v.is_null())) || BAR_FIT_KEYS.iter().any(|k| p.get(*k).is_some()) {
        say_text_fit(s, li, id, true);
    } else if given(&["x", "y", "rotation", "valign"]) {
        // A move, a turn or a block alignment changes no line a text breaks into, but can take its
        // ink past the page's edge (#2703); a scale bar's labels move with it (#2581).
        say_text_fit(s, li, id, false);
    }
    if p.get("text").is_some_and(Value::is_string) {
        say_text_statistics(s, li, id);
    }
    // The first frame's map decides every text's figures (#2853): another map, which may lack a
    // layer or a field, says what the layout's texts lose.
    if given(&["map"])
        && s.project.layouts.get(li).is_some_and(|l| l.first_frame() == Some(id))
        && !same_map(first_frame_map(s, li).as_deref(), map_before.as_deref())
    {
        say_layout_text_statistics(s, li);
    }
    // A centre the frame's map can't place (a pole on Web Mercator) is said (#2684).
    if new_center.is_some() || given(&["map"]) {
        let said =
            s.project.layouts.get(li).and_then(|l| unplaced_notice(&l.name, &unplaced_center(s, l, id).into_iter().collect::<Vec<_>>()));
        if let Some(said) = said {
            s.notify(said, false);
        }
    }
    say_held(s, "Element Properties", held);
    if let Some(w) = font_told {
        return Ok(json!({"warning": w}));
    }
    Ok(Value::Null)
}

/// Validates a table/chart's effective layer and field before mutation; input binding uses this same choice.
pub(in crate::cmd::layout) fn checked_update(
    input: &impl crate::cmd::InputSource,
    p: &Value,
    li: usize,
    id: u32,
) -> Result<Option<(String, Option<String>)>, String> {
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
