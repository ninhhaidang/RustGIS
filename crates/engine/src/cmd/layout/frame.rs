//! Map frames: their grid, view and scale box.

use super::*;

pub(in crate::cmd::layout) fn add_frame(s: &mut Session, p: &Value) -> CmdResult {
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
    let first = l.first_frame();
    let id = l.add("Map Frame", rect, kind);
    // The first frame decides the figures of the texts (#2853): a layout that had none reads them
    // from this one now, and what its map lacks is said.
    if first.is_none() {
        say_layout_text_statistics(s, i);
    }
    Ok(json!({"element": id}))
}

pub(in crate::cmd::layout) fn frame_of(l: &Layout, p: &Value) -> Result<u32, String> {
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

pub(in crate::cmd::layout) const ONLY_GRID_LABEL_SIZE: &str = "only grids take labelSize (a map frame's in its \"grid\" options)";

/// A grid with the options in `p` over `base`'s; a bad value is an error, leaving nothing changed.
pub(in crate::cmd::layout) fn grid_params(p: &Value, base: Grid) -> Result<Grid, String> {
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
    if let Some(pt) = text_size_param(p, "labelSize", GRID_LABEL_PT)? {
        g.label_size = pt;
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
pub(in crate::cmd::layout) fn edges(v: &Value) -> Result<GridEdges, String> {
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

/// The width (mm) of a publication's scale bar box (#1379), for a frame `frame_w` mm wide at
/// `scale` and `ground_per_pt` ground metres a point: the bar a box 30 % of the frame's width (25
/// mm at least) would draw sets its length and divisions, and the box is the narrowest that draws
/// that same bar (at least its length over the 85 % of a box the renderer gives a bar), its last
/// label included at the bar's text size, with every setting the bar has, as the renderer draws it
/// ([`crate::render::layout::scale_bar_box_width_pt`]), and holds the text written under it:
/// `text` (its `{scale}` filled), in `text_pt` points, bold or not.
pub(in crate::cmd::layout) fn scale_box_width(
    ground_per_pt: f64,
    scale: f64,
    frame_w_mm: f64,
    bar: &ElementKind,
    comma: bool,
    text: (&str, f64, bool),
) -> f64 {
    let (text, text_pt, bold) = text;
    let (widest, pt_per_mm) = ((frame_w_mm * 0.3).max(25.0), rustgis_map::layout::PT_PER_MM);
    let bar_w = crate::render::layout::scale_bar_box_width_pt(ground_per_pt, widest * pt_per_mm, bar, comma, 1.5 * pt_per_mm)
        .map_or(widest, |w| w / pt_per_mm);
    let written = text.replace("{scale}", &crate::render::layout::scale_ratio_text(scale, comma));
    bar_w.max(scale_text_mm(&written, text_pt, bold)).min(widest)
}

/// The width (mm) a scale bar box gives the text under its bar, its tokens filled: its longest
/// line in `text_pt` points, bold or not, and a millimetre each side.
pub(in crate::cmd::layout) fn scale_text_mm(text: &str, text_pt: f64, bold: bool) -> f64 {
    text.lines().map(|line| crate::render::report::width_mm(line, text_pt as f32, bold)).fold(0.0, f64::max) + 2.0
}

/// What a publication places over its map, which a scale bar box measured again must stay off.
pub(in crate::cmd::layout) const PLACED_OVER_MAP: [&str; 3] = ["Legend Box", "Locator", "North Arrow"];

/// What a publication's scale bar box is measured from: its frame (id, map, centre, scale, width),
/// its bar (id, and the bar as it is set: every setting changes its labels or their room) and the
/// text under the bar (id, text, size, boldness, case).
#[derive(Clone)]
pub(in crate::cmd::layout) struct BoxBasis {
    pub(in crate::cmd::layout) frame: u32,
    pub(in crate::cmd::layout) map: String,
    pub(in crate::cmd::layout) center: Coord,
    pub(in crate::cmd::layout) scale: f64,
    pub(in crate::cmd::layout) frame_w: f64,
    pub(in crate::cmd::layout) bar_id: u32,
    pub(in crate::cmd::layout) bar: ElementKind,
    pub(in crate::cmd::layout) text_id: u32,
    pub(in crate::cmd::layout) text: String,
    pub(in crate::cmd::layout) size: f64,
    pub(in crate::cmd::layout) bold: bool,
    pub(in crate::cmd::layout) case: TextCase,
}

/// The text under a publication's scale bar as the page draws it from `b`: `{scale}` filled as
/// `layout.publication` fills it, the other tokens from the frame's map, then in its case.
pub(in crate::cmd::layout) fn basis_text(l: &Layout, m: &rustgis_map::Map, b: &BoxBasis) -> String {
    let info = crate::render::layout::PageInfo { page: 1, pages: 1, page_name: String::new() };
    let cam = rustgis_map::layout::frame_camera(m, b.center, b.scale, 0.0, 0.0);
    let text = b.text.replace("{scale}", &crate::render::layout::scale_ratio_text(b.scale, l.decimal_comma));
    b.case.kept().apply(crate::render::layout::expand(&text, l, &info, Some((m, cam))))
}

/// The automatic width (mm) of a publication's scale bar box measured from `b`: the one
/// `layout.publication` gives it for that frame, bar and text.
pub(in crate::cmd::layout) fn basis_width(l: &Layout, m: &rustgis_map::Map, b: &BoxBasis) -> f64 {
    let ground_per_pt = m.ground_res_at(&rustgis_map::layout::frame_camera(m, b.center, b.scale, 0.0, 0.0));
    let filled = basis_text(l, m, b);
    scale_box_width(ground_per_pt, b.scale, b.frame_w, &b.bar, l.decimal_comma, (&filled, b.size, b.bold))
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
pub(in crate::cmd::layout) fn scale_box_basis(s: &Session, li: usize) -> Option<BoxBasis> {
    let l = s.project.layouts.get(li)?;
    let named = |name: &str| l.elements.iter().find(|e| e.name == name);
    let (bar, written) = (named("Scale Bar")?, named("Scale")?);
    let (ElementKind::ScaleBar { frame, .. }, ElementKind::Text { text, size, bold, case, .. }) = (&bar.kind, &written.kind) else {
        return None;
    };
    let fr = l.element(*frame)?;
    let ElementKind::MapFrame { map, center, scale, .. } = &fr.kind else { return None };
    Some(BoxBasis {
        frame: *frame,
        map: map.clone(),
        center: *center,
        scale: *scale,
        frame_w: fr.w,
        bar_id: bar.id,
        bar: bar.kind.clone(),
        text_id: written.id,
        text: text.clone(),
        size: *size,
        bold: *bold,
        case: *case,
    })
}

/// A map frame `frame` of the layout `li` drawn at another view: a publication's scale bar box of
/// it measured again from `basis` ([`refit_scale_box`], #1379), then the scale bars of the frame
/// whose labels no longer fit told, one notice a kind (#2581), as their lengths and labels follow
/// the frame's scale.
pub(in crate::cmd::layout) fn frame_view_changed(s: &mut Session, li: usize, frame: u32, basis: Option<&BoxBasis>) {
    if let Some(basis) = basis.filter(|b| b.frame == frame) {
        refit_scale_box(s, li, basis);
    }
    let bars: Vec<u32> = s.project.layouts.get(li).map_or_else(Vec::new, |l| {
        l.elements.iter().filter(|e| matches!(e.kind, ElementKind::ScaleBar { frame: f, .. } if f == frame)).map(|e| e.id).collect()
    });
    let losses: Vec<_> = bars.into_iter().filter_map(|bar| text_fit_loss(s, li, bar)).collect();
    for said in rustgis_render::layout::scale_bar_fit_notices(&losses) {
        s.warn(said);
    }
}

/// A publication's scale bar box measured again after its frame's scale, width, centre or map
/// changed (#1379, #2581), each measured on its own map and centre:
/// the "Scale Box", the "Scale Bar" of `frame` and the "Scale" under it, as a publication lays
/// them out (one place, one width, none locked), take the width that bar and the text under it
/// (its tokens filled as the page draws them, measured as drawn) now need, kept on the side of the
/// frame they are nearer, the text measured in its case. A layout without them, with them moved
/// apart, with that text spaced out or in another font, naming a map series page (which differs
/// from page to page), or of a frame other than the first (whose map the page fills the text
/// from) is left as it is; so is a text longer than the widest box a publication gives. Only a
/// box as wide as `before` (what it was measured from before the change: its frame, bar and text)
/// is measured again: one set by hand keeps its width. A change to the text under the bar or to
/// anything of the bar's that changes its labels or their room (its units, divisions, text size,
/// subdivisions, divisions before zero, labels, unit label and where it sits, decimals or fit)
/// measures it again too, with every setting the bar has. A box that would then cover the legend,
/// locator or north arrow, which it didn't, stays as it was, which is said.
pub(in crate::cmd::layout) fn refit_scale_box(s: &mut Session, li: usize, before: &BoxBasis) {
    let frame = before.frame;
    let Some(now) = scale_box_basis(s, li).filter(|b| b.frame == frame) else { return };
    let Some(l) = s.project.layouts.get(li) else { return };
    let Some(fr) = l.element(frame) else { return };
    // Each basis measured on its own map: a frame given another map fills the text from it.
    let map_of = |b: &BoxBasis| s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(&b.map));
    let (Some(m), Some(was)) = (map_of(&now), map_of(before)) else { return };
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
    if (boxed.w - basis_width(l, was, before)).abs() > 0.01 {
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
pub(in crate::cmd::layout) fn set_frame(s: &mut Session, li: usize, frame: u32, e: &Extent) -> Result<(), String> {
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

pub(in crate::cmd::layout) fn frame_zoom(s: &mut Session, p: &Value) -> CmdResult {
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
    frame_view_changed(s, li, frame, basis.as_ref());
    Ok(json!({"element": frame}))
}

/// An element key overrides frame even when null, using frame_of's existing fallback semantics.
pub(in crate::cmd::layout) fn zoom_frame(l: &Layout, p: &Value) -> Result<u32, String> {
    match p.get("element") {
        Some(_) => frame_of(l, &json!({"frame":p.get("element")})),
        None => frame_of(l, p),
    }
}
