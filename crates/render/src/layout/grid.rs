//! A map frame's grid: graticule, measured and MGRS lines, and their labels.

use super::*;

/// A graticule label as [`Grid::label`] writes it, with plain `'` and `"` for minutes and seconds
/// when the label font has no prime marks.
pub(super) fn grid_label(g: &Grid, v: f64, lon: bool, step: f64) -> String {
    let s = g.label(v, lon, step);
    if text::covers(&s, false) { s } else { s.replace('′', "'").replace('″', "\"") }
}

/// A graticule's lines over `ext` (longitude/latitude), each with its label.
pub(super) fn graticule_lines(g: &Grid, ext: Extent, comma: bool, lines: &mut Vec<(Vec<Coord>, String, bool)>) {
    if ext.is_empty() {
        return;
    }
    let span = ext.width().max(ext.height());
    let step = if g.step > 0.0 {
        g.step.max(0.001)
    } else {
        *[0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 45.0].iter().find(|s| span / **s <= 8.0).unwrap_or(&60.0)
    };
    // Lines run a little past the frame so they cross its edges rather than end on them.
    let pad = span * 0.02;
    let (lat0, lat1) = ((ext.min_y - pad).max(-89.9), (ext.max_y + pad).min(89.9));
    let (lon0, lon1) = (ext.min_x - pad, ext.max_x + pad);
    let mut n = 0;
    let mut lon = (ext.min_x / step).floor() * step;
    while lon <= ext.max_x + step * 1e-9 && n < 200 {
        let pts = (0..=48).map(|i| Coord::new(lon, lat0 + (lat1 - lat0) * i as f64 / 48.0)).collect();
        lines.push((pts, decimal(grid_label(g, lon, true, step), comma), true));
        lon += step;
        n += 1;
    }
    let mut lat = (ext.min_y / step).floor() * step;
    while lat <= ext.max_y + step * 1e-9 && n < 400 {
        if lat.abs() <= 90.0 {
            let pts = (0..=48).map(|i| Coord::new(lon0 + (lon1 - lon0) * i as f64 / 48.0, lat)).collect();
            lines.push((pts, decimal(grid_label(g, lat, false, step), comma), false));
        }
        lat += step;
        n += 1;
    }
}

/// The most lines a measured grid draws each way: past them its step is widened to draw at most
/// [`WIDENED_LINES`].
pub(super) const MAX_MEASURED_LINES: f64 = 100.0;
/// About how many lines a step too fine for the frame is widened to.
pub(super) const WIDENED_LINES: f64 = 50.0;

/// A measured grid's lines over the frame whose edges are `lonlats`, in the coordinate system of
/// `tr`: eastings (north–south) and northings, each back in longitude/latitude, with its label.
pub(crate) fn measured_lines(
    g: &Grid,
    tr: &rustgis_data::raster::Transformer,
    lonlats: &[Coord],
    comma: bool,
    lines: &mut Vec<(Vec<Coord>, String, bool)>,
) {
    let mut ext = Extent::EMPTY;
    for c in lonlats.iter().filter_map(|c| tr.from_lonlat(*c)) {
        ext.expand(c);
    }
    if ext.is_empty() || !(ext.width().is_finite() && ext.height().is_finite()) {
        return;
    }
    let span = ext.width().max(ext.height());
    let given = g.step.clamp(0.0, rustgis_map::layout::MAX_MEASURED_STEP);
    let step = if given > 0.0 {
        given.max(rustgis_map::layout::MIN_MEASURED_STEP)
    } else if g.mgrs {
        // (At most 100 km apart, so MGRS's squares are drawn whole.)
        nice_step(span, rustgis_map::layout::MIN_MEASURED_STEP).min(100_000.0)
    } else {
        nice_step(span, rustgis_map::layout::MIN_MEASURED_STEP)
    };
    // A step too fine for the frame (a 1 m grid over a country) is widened, not drawn as a blur.
    let step = if span / step > MAX_MEASURED_LINES { nice_step(span * 8.0 / WIDENED_LINES, step) } else { step };
    let pad = span * 0.02;
    let (x0, x1, y0, y1) = (ext.min_x - pad, ext.max_x + pad, ext.min_y - pad, ext.max_y + pad);
    let back = |x: f64, y: f64| tr.to_lonlat(Coord::new(x, y));
    // Counted too: at coordinates far larger than the step, adding it may not move a line on.
    let most = 2 * MAX_MEASURED_LINES as usize;
    let first = |min: f64| (min / step).floor() * step;
    for (i, x) in
        std::iter::successors(Some(first(ext.min_x)), |x| Some(x + step)).take_while(|x| *x <= ext.max_x + step * 1e-9).enumerate()
    {
        if i >= most {
            break;
        }
        // (A point that doesn't convert is left out.)
        let pts: Vec<Coord> = (0..=48).filter_map(|i| back(x, y0 + (y1 - y0) * f64::from(i) / 48.0)).collect();
        lines.push((pts, decimal(g.measured_label(x, step), comma), true));
    }
    for (i, y) in
        std::iter::successors(Some(first(ext.min_y)), |y| Some(y + step)).take_while(|y| *y <= ext.max_y + step * 1e-9).enumerate()
    {
        if i >= most {
            break;
        }
        let pts: Vec<Coord> = (0..=48).filter_map(|i| back(x0 + (x1 - x0) * f64::from(i) / 48.0, y)).collect();
        lines.push((pts, decimal(g.measured_label(y, step), comma), false));
    }
}

/// Where a polyline (in pixels) crosses the edges of a `w` × `h` frame, as (edge, position along
/// it): the top and bottom edges give x, the left and right ones y.
pub(super) fn edge_crossings(pts: &[(f32, f32)], w: f32, h: f32) -> Vec<(Edge, f32)> {
    let mut out = Vec::new();
    for pair in pts.windows(2) {
        let &[(x0, y0), (x1, y1)] = pair else { continue };
        for (edge, a, b, at) in [(Edge::Top, y0, y1, 0.0), (Edge::Bottom, y0, y1, h), (Edge::Left, x0, x1, 0.0), (Edge::Right, x0, x1, w)] {
            let (da, db) = (a - at, b - at);
            // Counted once where a vertex sits on the edge, and never along it.
            if !((da <= 0.0 && db > 0.0) || (da > 0.0 && db <= 0.0)) {
                continue;
            }
            let t = da / (da - db);
            let (along, limit) = match edge {
                Edge::Top | Edge::Bottom => (x0 + (x1 - x0) * t, w),
                Edge::Left | Edge::Right => (y0 + (y1 - y0) * t, h),
            };
            if along.is_finite() && (0.0..=limit).contains(&along) {
                out.push((edge, along));
            }
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// The coordinate system of a measured grid on `map`: the grid's own, else the map's when it is
/// projected, else the WGS 84 UTM zone at `center` (longitude/latitude). With MGRS labels, only a
/// WGS 84 UTM zone: the one named, else the map's when it is one, else MGRS's zone at `center`.
/// `None` for one that can't be read or isn't projected.
pub fn measured_crs(g: &Grid, map: &Map, center: Coord) -> Option<rustgis_data::raster::Crs> {
    use rustgis_data::raster::Crs;
    use rustgis_map::layout::utm_zone_of;
    let utm = |(zone, north): (u8, bool)| Crs::from_epsg(if north { 32_600 } else { 32_700 } + u32::from(zone));
    let named = g.crs.as_deref().map(str::trim).filter(|c| !c.is_empty());
    let crs = match named {
        // (One longer than a command keeps, from a crafted project, isn't read each frame.)
        Some(c) if c.chars().count() > rustgis_map::layout::MAX_GRID_CRS => return None,
        Some(c) if g.mgrs => utm(utm_zone_of(c)?)?,
        Some(c) => Crs::parse(c).ok()?,
        // The map's own: its code (a preset, EPSG:, ESRI: or local:) as its PROJ definition.
        None => match &map.projection {
            rustgis_map::Projection::Custom(code) if g.mgrs => utm_zone_of(code).and_then(utm),
            rustgis_map::Projection::Custom(code) => rustgis_map::projection::lookup(code)
                .ok()
                .and_then(|(def, _)| Crs::parse(&def).ok())
                .or_else(|| Crs::parse(code).ok())
                .filter(|c| !c.is_geographic()),
            _ => None,
        }
        .map_or_else(
            || {
                // MGRS's zone at the centre keeps southern Norway's and Svalbard's wider zones.
                let c = Coord::new((center.x + 180.0).rem_euclid(360.0) - 180.0, center.y);
                match rustgis_geom::utm::utm_zone(c).filter(|_| g.mgrs) {
                    Some(zone) => utm((zone, center.y >= 0.0)),
                    None => Crs::from_epsg(rustgis_map::layout::utm_epsg(center.x, center.y)),
                }
            },
            Some,
        )?,
    };
    (!crs.is_geographic()).then_some(crs)
}

/// A step that draws about eight lines across `span`: 1, 2 or 5 times a power of ten.
pub(crate) fn nice_step(span: f64, smallest: f64) -> f64 {
    let rough = (span / 8.0).max(smallest);
    let p = 10f64.powf(rough.log10().floor());
    [1.0, 2.0, 5.0, 10.0].iter().map(|k| k * p).find(|s| *s >= rough).unwrap_or(10.0 * p)
}

/// A grid over a frame: lines across it, ticks along its edges or both, labelled at the edges the
/// grid names. A graticule's lines are latitudes and longitudes; a measured grid's are eastings and
/// northings in its coordinate system, bent through it onto the map. Gives the labels drawn: each
/// one's edge and its box in points (left, top, right, bottom).
pub(super) fn draw_grid(pm: &mut Canvas, map: &Map, view: &View, g: &Grid, comma: bool) -> Vec<(Edge, [f32; 4])> {
    let ppp = view.pixels_per_point as f32;
    // Its labels' size against the default (#2581): their room from the edge and between them,
    // and MGRS squares' names, follow in proportion, a point at the default being `unit` pixels.
    let k = layout_text_scale(g.label_size, GRID_LABEL_PT) as f32;
    let unit = ppp * k;
    let (w, h) = (view.width, view.height);
    // On a globe, through the globe as the frame draws it (#2337): through the flat camera the
    // lines would be laid out as a flat world around it.
    let globe = map.projection.is_globe().then(|| rustgis_map::globe::Globe::new(&view.camera, w, h));
    let lonlat_at = |sx: f64, sy: f64| crate::mapgrid::at(map, view, globe.as_ref(), sx, sy);
    // The frame's edges in longitude/latitude: its corners and points along each side.
    let mut edge_points = Vec::new();
    for i in 0..=8 {
        let t = f64::from(i) / 8.0;
        edge_points.extend([(w * t, 0.0), (w * t, h), (0.0, h * t), (w, h * t)]);
    }
    let mut lonlats: Vec<Coord> = edge_points.iter().filter_map(|&(sx, sy)| lonlat_at(sx, sy)).collect();
    // A frame with an edge past the globe's rim sees the globe's limb: its graticule is the whole
    // Earth's, the lines behind the globe left out where they are drawn. A measured grid takes its
    // bounds from the part of the globe inside the frame too, the edges missing it.
    let rim = globe.is_some() && lonlats.len() < edge_points.len();
    if rim {
        for i in 0..=16 {
            for j in 0..=16 {
                lonlats.extend(lonlat_at(w * f64::from(i) / 16.0, h * f64::from(j) / 16.0));
            }
        }
    }
    let world = Extent::new(-180.0, -90.0, 180.0, 90.0);
    let mut ext = if rim { world } else { Extent::EMPTY };
    for c in &lonlats {
        ext.expand(*c);
    }
    if ext.is_empty() {
        return Vec::new();
    }
    // Each line: its points in longitude/latitude, its label, and whether it runs north–south
    // (labelled along the top and bottom first).
    let mut lines: Vec<(Vec<Coord>, String, bool)> = Vec::new();
    // A measured grid's MGRS squares: each one's name and middle, in the frame's points.
    let mut squares = Vec::new();
    match g.kind {
        GridKind::Graticule => {
            // Over the extent of the frame's corners and midpoints, as it always was (its step and
            // lines unchanged for the layouts made before measured grids).
            let mut corners = if rim { world } else { Extent::EMPTY };
            for (sx, sy) in [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h), (w / 2.0, 0.0), (w / 2.0, h), (0.0, h / 2.0), (w, h / 2.0)] {
                if let Some(c) = lonlat_at(sx, sy) {
                    corners.expand(c);
                }
            }
            graticule_lines(g, corners, comma, &mut lines);
        }
        GridKind::Measured => {
            let center = lonlat_at(w / 2.0, h / 2.0).unwrap_or_else(|| ext.center());
            let Some(crs) = measured_crs(g, map, center) else { return Vec::new() };
            let Ok(tr) = crs.transformer() else { return Vec::new() };
            measured_lines(g, &tr, &lonlats, comma, &mut lines);
            if g.mgrs
                && g.labels
                && let Some((zone, _)) = rustgis_map::layout::utm_zone_of(&crs.epsg().to_string())
            {
                let size = |s: &str| (text::measure(s, MGRS_LABEL_PT * k, true), MGRS_LABEL_PT * k * 1.33 * 1.2);
                squares = mgrs_squares(map, view, globe.as_ref(), zone, &tr, size);
            }
        }
    }
    // A tilted frame's lines break where they pass behind the eye, a globe's behind its horizon.
    let to_px = |c: Coord| {
        match &globe {
            Some(on) => on.forward(c),
            None => map.to_map(c).and_then(|m| view.camera.try_to_screen(m, w, h)),
        }
        .map(|(x, y)| (x as f32 * ppp, y as f32 * ppp))
        .filter(|(x, y)| x.is_finite() && y.is_finite())
    };
    let (pw, ph) = (pm.width() as f32, pm.height() as f32);
    let paint_line = paint(g.color, 1.0);
    let stroke = Stroke { width: g.width.max(0.0) as f32 * ppp, ..Stroke::default() };
    let tick = if g.style.ticks() { (g.tick_length * PT_PER_MM) as f32 * ppp } else { 0.0 };
    let label_px = GRID_LABEL_PT as f32 * 1.33 * unit;
    let label_color = Rgba::rgb(0x20, 0x20, 0x20);
    let vertical = g.label_orientation == Orientation::Vertical;
    let mut pb = PathBuilder::new();
    // Each label's edge, place along it, text, and whether it is a meridian's.
    let mut labels: Vec<(Edge, f32, String, bool)> = Vec::new();
    let mut draw_line = |pb: &mut PathBuilder, pts: Vec<Coord>, label: String, lon: bool| {
        let px: Vec<Option<(f32, f32)>> = pts.into_iter().map(to_px).collect();
        // Runs of points that projected, so a line breaks where its projection does.
        for run in px.split(|p| p.is_none()) {
            let run: Vec<(f32, f32)> = run.iter().flatten().copied().collect();
            if g.style.lines() {
                for (i, (x, y)) in run.iter().enumerate() {
                    if i == 0 { pb.move_to(*x, *y) } else { pb.line_to(*x, *y) }
                }
            }
            for (edge, at) in edge_crossings(&run, pw, ph) {
                if tick > 0.0 {
                    let (a, b) = match edge {
                        Edge::Top => ((at, 0.0), (at, tick)),
                        Edge::Bottom => ((at, ph), (at, ph - tick)),
                        Edge::Left => ((0.0, at), (tick, at)),
                        Edge::Right => ((pw, at), (pw - tick, at)),
                    };
                    pb.move_to(a.0, a.1);
                    pb.line_to(b.0, b.1);
                }
                let shown = match edge {
                    Edge::Top => g.edges.top,
                    Edge::Bottom => g.edges.bottom,
                    Edge::Left => g.edges.left,
                    Edge::Right => g.edges.right,
                };
                if g.labels && shown {
                    labels.push((edge, at, label.clone(), lon));
                }
            }
        }
    };
    for (pts, label, lon) in lines {
        draw_line(&mut pb, pts, label, lon);
    }
    if let Some(p) = pb.finish() {
        pm.stroke_path(&p, &paint_line, &stroke, Transform::identity(), None);
    }
    for (s, x, y) in squares {
        text::text_at(pm, &s, x * ppp, y * ppp, MGRS_LABEL_PT * 1.33 * unit, label_color, true, text::Align::Center, true);
    }
    // Labels sit just inside the frame, past the ticks.
    let inset = tick + 3.0 * unit;
    let along = |edge: Edge, s: &str| match edge {
        Edge::Left | Edge::Right if !vertical => label_px,
        _ => text::measure(s, label_px / 1.33, false),
    };
    // How far a label reaches in from its edge: past the inset, its height, or a side's flat
    // label its width.
    let across = |edge: Edge, s: &str| {
        inset
            + match edge {
                Edge::Left | Edge::Right if !vertical => text::measure(s, label_px / 1.33, false),
                _ => label_px,
            }
    };
    let mut drawn = Vec::new();
    for (edge, at, s) in kept_labels(labels, (pw, ph), (along, across), label_px, unit) {
        let tw = text::measure(&s, label_px / 1.33, false);
        // Its centre, and half its size across and down (its halo and descenders included).
        let (flat, upright) = ((tw / 2.0, label_px * 0.6), (label_px * 0.6, tw / 2.0));
        let (cx, cy, (hw, hh)) = match edge {
            Edge::Top => (at, inset + label_px * 0.4, flat),
            Edge::Bottom => (at, ph - inset - label_px * 0.4, flat),
            Edge::Left if vertical => (inset + label_px * 0.4, at, upright),
            Edge::Right if vertical => (pw - inset - label_px * 0.4, at, upright),
            Edge::Left => (inset + tw / 2.0, at, flat),
            Edge::Right => (pw - inset - tw / 2.0, at, flat),
        };
        if matches!(edge, Edge::Left | Edge::Right) && vertical {
            rotated_text(pm, &s, cx, cy, label_px, label_color);
        } else {
            text::text_at(pm, &s, cx, cy, label_px, label_color, false, text::Align::Center, true);
        }
        drawn.push((edge, [(cx - hw) / ppp, (cy - hh) / ppp, (cx + hw) / ppp, (cy + hh) / ppp]));
    }
    drawn
}

/// The size of a measured grid's MGRS square labels, in points.
pub(super) const MGRS_LABEL_PT: f32 = 10.0;
/// The places across and down a frame where [`mgrs_squares`] looks which square shows.
pub(super) const MGRS_SAMPLES: u32 = 32;

/// Where a measured grid in UTM zone `zone` (`tr` its transformer) names its MGRS 100 km squares on
/// a frame showing `map` in `view` (through `globe` on a globe, as its lines): each square of the
/// zone, and of each latitude band, that shows, by its grid zone designation and letters
/// (`32T MT`), at the middle of what shows of it, in the frame's points. Only where all of the
/// label, `size(text)` points across and down, stays in its square and well inside the frame,
/// clear of the edge labels. Squares the zone's eastings reach past its edges (a frame across two
/// zones) are named in the other zone, so not here.
pub(super) fn mgrs_squares(
    map: &Map,
    view: &View,
    globe: Option<&rustgis_map::globe::Globe>,
    zone: u8,
    tr: &rustgis_data::raster::Transformer,
    size: impl Fn(&str) -> (f32, f32),
) -> Vec<(String, f32, f32)> {
    use rustgis_geom::utm;
    let (w, h) = (view.width, view.height);
    // The square, and band, a place on the frame is in: none outside the zone. On a globe, the
    // place the globe draws there, as the grid's lines (#2337).
    let square = |sx: f64, sy: f64| -> Option<(i64, i64, char)> {
        let c = crate::mapgrid::at(map, view, globe, sx, sy)?;
        // (A copy of the world past ±180° is the same place.)
        let c = Coord::new((c.x + 180.0).rem_euclid(360.0) - 180.0, c.y);
        if utm::utm_zone(c)? != zone {
            return None;
        }
        let band = utm::latitude_band(c.y)?;
        let p = tr.from_lonlat(c)?;
        // Across the Equator from the grid's hemisphere, the northing MGRS gives there: the other
        // hemisphere's, 10,000 km apart.
        let y = match (c.y < 0.0, p.y) {
            (true, y) if y < 0.0 => y + 10_000_000.0,
            (false, y) if y >= 10_000_000.0 => y - 10_000_000.0,
            (_, y) => y,
        };
        let (e, n) = ((p.x / 100_000.0).floor(), (y / 100_000.0).floor());
        (e.is_finite() && n.is_finite()).then_some((e as i64, n as i64, band))
    };
    // Each square's samples: their sum across and down, and how many.
    let mut seen: std::collections::BTreeMap<(i64, i64, char), (f64, f64, u32)> = std::collections::BTreeMap::new();
    for i in 0..MGRS_SAMPLES {
        for j in 0..MGRS_SAMPLES {
            let (sx, sy) = (w * (f64::from(i) + 0.5) / f64::from(MGRS_SAMPLES), h * (f64::from(j) + 0.5) / f64::from(MGRS_SAMPLES));
            if let Some(k) = square(sx, sy) {
                let e = seen.entry(k).or_default();
                *e = (e.0 + sx, e.1 + sy, e.2 + 1);
            }
        }
    }
    let mut out = Vec::new();
    for ((e, n, band), (sx, sy, count)) in seen {
        let mid = |k: i64| (k as f64 + 0.5) * 100_000.0;
        let Some((column, row)) = utm::square_letters(zone, mid(e), mid(n)) else { continue };
        let text = format!("{zone:02}{band} {column}{row}");
        let (tw, th) = size(&text);
        let (cx, cy) = (sx / f64::from(count), sy / f64::from(count));
        let (hw, hh) = (f64::from(tw) / 2.0 + 2.0, f64::from(th) / 2.0 + 2.0);
        // Clear of the edges' labels: three label heights in from each edge.
        let margin = 3.0 * f64::from(th);
        if cx - hw < margin || cx + hw > w - margin || cy - hh < margin || cy + hh > h - margin {
            continue;
        }
        let corners = [(cx - hw, cy - hh), (cx + hw, cy - hh), (cx - hw, cy + hh), (cx + hw, cy + hh)];
        if corners.iter().all(|&(x, y)| square(x, y) == Some((e, n, band))) {
            out.push((text, cx as f32, cy as f32));
        }
    }
    out
}

/// The graticule labels drawn, of those at each edge crossing (`labels`: edge, place along it,
/// text, a meridian's or not) on a frame `size` pixels, `label_h` tall: away from the corners (all
/// of a label, `along` its edge, fits, and none at a side's ends by a top or bottom label), within
/// the frame across (how far it reaches in from its edge, `across`, at most the frame's other side:
/// one reaching further in is left out, at any size, rather than cut by the frame), and none over
/// another. Meridians are labelled along the top and bottom and parallels along the sides first; a
/// line that leaves the frame through the other edges (a slanting meridian on a projected map) is
/// labelled there only well clear of the labels already set on that edge. Their room from the
/// corners and between them is in `unit`s, a point at the labels' default size in pixels, so it
/// follows their size.
/// A label left out for want of room is thinning, as on any map, not a loss to report: the grid
/// still shows its lines, and the user guide says labels go where there is room.
pub(super) fn kept_labels(
    mut labels: Vec<(Edge, f32, String, bool)>,
    (pw, ph): (f32, f32),
    (along, across): (impl Fn(Edge, &str) -> f32, impl Fn(Edge, &str) -> f32),
    label_h: f32,
    unit: f32,
) -> Vec<(Edge, f32, String)> {
    let own_edge = |edge: Edge, lon: bool| matches!(edge, Edge::Top | Edge::Bottom) == lon;
    labels.sort_by_key(|(edge, _, _, lon)| !own_edge(*edge, *lon));
    let mut taken: Vec<(Edge, f32, f32)> = Vec::new();
    let mut kept = Vec::new();
    for (edge, at, s, lon) in labels {
        let length = along(edge, &s);
        let side = matches!(edge, Edge::Left | Edge::Right);
        let (limit, room) = if side { (ph, pw) } else { (pw, ph) };
        if across(edge, &s) > room {
            continue;
        }
        // A side's labels keep two label heights from the corners, below the top's labels and
        // above the bottom's, so a corner holds no two.
        let margin = (length / 2.0 + 3.0 * unit).max(12.0 * unit).max(if side { 2.0 * label_h } else { 0.0 });
        if at < margin || at > limit - margin {
            continue;
        }
        let (a, z) = (at - length / 2.0 - 2.0 * unit, at + length / 2.0 + 2.0 * unit);
        // Off its own edge, a label keeps a further length of its own clear of the others, so
        // it doesn't read as one of theirs.
        let clear = if own_edge(edge, lon) { 0.0 } else { length };
        if taken.iter().any(|(e, ta, tz)| *e == edge && a - clear < *tz && *ta < z + clear) {
            continue;
        }
        taken.push((edge, a, z));
        kept.push((edge, at, s));
    }
    kept
}

/// One line of haloed text centred on (`cx`, `cy`), turned to read from bottom to top.
pub(super) fn rotated_text(pm: &mut Canvas, s: &str, cx: f32, cy: f32, px: f32, color: Rgba) {
    // Whole pixels throughout, so the quarter turn moves pixels without resampling them.
    let tw = (text::measure(s, px / 1.33, false) + px * 0.4).ceil().max(1.0);
    let th = (px * 1.6).ceil().max(1.0);
    let (x, y) = ((cx - th / 2.0).round(), (cy + tw / 2.0).round());
    let t = Transform::from_row(0.0, -1.0, 1.0, 0.0, x, y);
    if pm.is_recording() {
        pm.with_transform(t, |c| text::text_at(c, s, tw / 2.0, th / 2.0, px, color, false, text::Align::Center, true));
        return;
    }
    let Some(mut tmp) = Canvas::new(tw as u32, th as u32) else { return };
    text::text_at(&mut tmp, s, tw / 2.0, th / 2.0, px, color, false, text::Align::Center, true);
    pm.draw_pixmap(
        0,
        0,
        tmp.pixmap().as_ref(),
        &PixmapPaint { quality: tiny_skia::FilterQuality::Nearest, ..PixmapPaint::default() },
        t,
        None,
    );
}
