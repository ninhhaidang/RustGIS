//! Map frames: their picture, outline and georeferenced viewports.

use super::*;

/// A map frame's map, `w` × `h` points at `ppp` pixels a point, labels and attribution included:
/// what a frame shows before its grid, indicator and outline. It waits for the map's tiles, as a
/// page needs them all. Refuses a linked archive changed during the complete frame.
pub fn frame_map(map: &mut Map, camera: rustgis_map::Camera, w: f64, h: f64, ppp: f64) -> Result<Pixmap, String> {
    let revisions = crate::archive_revisions([&*map]);
    let mut failed_source = None;
    let view = View { width: w, height: h, pixels_per_point: ppp, camera, sky: false };
    let result = frame_canvas(map, &view, false, &mut false, None, &mut failed_source, &Env::default()).map(Canvas::into_pixmap);
    revisions.finish(map, failed_source, result, "could not draw the map frame")
}

/// [`frame_map`] on a canvas: one that records (see [`crate::canvas`]) when `record` and the
/// frame can be kept as vectors (flat, north up, without view effects), else its pixels; with
/// `grid` (and whether the layout writes a decimal comma) drawn on it under the attribution.
/// `pending` is set when some of its tiles, or a read of its streamed layers or rasters, were
/// still on their way (in the browser, which doesn't wait for them).
pub(super) fn frame_canvas(
    map: &mut Map,
    view: &View,
    record: bool,
    pending: &mut bool,
    grid: Option<(&Grid, bool)>,
    failed_source: &mut Option<u64>,
    env: &Env,
) -> Option<Canvas> {
    // Why its layers were drawn without their cast shadows is kept for the page's caller (see
    // `crate::shadows_said`).
    let (img, shadows) = crate::frame_with_shadows_to_come(|| crate::complete(map, view, record, failed_source, env));
    let img = img?;
    *pending |= img.tiles_pending || img.data_pending || shadows;
    Some(finish_frame(img, view.pixels_per_point, grid.map(|(g, comma)| (&*map, view, g, comma))))
}

/// [`frame_map`] without waiting for tiles still on their way, for a preview drawn while the
/// interface waits: a pan's preview, whose size is more than the frame's for the overscan slid
/// under the pointer.
pub fn frame_map_preview(map: &mut Map, camera: rustgis_map::Camera, w: f64, h: f64, ppp: f64) -> Option<Pixmap> {
    frame_map_preview_with(map, camera, w, h, ppp, &Env::default())
}

/// Map preview during a layout pan, using the same expression context as its page.
pub fn frame_map_preview_with(map: &mut Map, camera: rustgis_map::Camera, w: f64, h: f64, ppp: f64, env: &Env) -> Option<Pixmap> {
    let view = View { width: w, height: h, pixels_per_point: ppp, camera, sky: false };
    crate::render_cpu(map, &view, false, false, &mut None, env).map(|img| finish_frame(img, ppp, None).into_pixmap())
}

/// A rendered frame map with its labels, its `grid` (drawn on `map` in `view`, with a decimal
/// comma or not) and its attribution drawn on it: on its recording when it was recorded, else on
/// its pixels. The attribution keeps clear of the grid's labels.
pub(super) fn finish_frame(img: crate::Rendered, ppp: f64, grid: Option<(&Map, &View, &Grid, bool)>) -> Canvas {
    let crate::Rendered { pixmap, recorded, labels, unplaced, label_layers, attribution, .. } = img;
    let mut pm = recorded.unwrap_or_else(|| Canvas::from_pixmap(pixmap));
    crate::draw_labels(&mut pm, (&unplaced, &labels, &label_layers), ppp as f32);
    // The grid is a layer of its own (#1815).
    pm.layer(grid.is_some().then_some("Grid"));
    let grid_labels = grid.map(|(map, view, g, comma)| draw_grid(&mut pm, map, view, g, comma)).unwrap_or_default();
    pm.layer(None);
    let (w, h) = (pm.width() as f32 / ppp as f32, pm.height() as f32 / ppp as f32);
    let inset = crate::attribution_text(&attribution).map_or((0.0, 0.0), |(_, tw)| attribution_inset((w, h), tw, &grid_labels));
    crate::draw_attribution(&mut pm, &attribution, ppp as f32, inset);
    pm
}

/// How far further in (up from the bottom, in from the right, in points) the attribution `tw`
/// points wide goes on a frame `w` × `h` points, so it covers none of the grid's bottom and right
/// `labels` (each one's edge, and its box in points: left, top, right, bottom). It goes above a
/// bottom label under it. Beside a right label, it goes left of it when it still fits in the frame
/// there, else above it; when neither fits, as far left as the frame holds it. Where none is in
/// the way, it stays in the corner.
pub(super) fn attribution_inset((w, h): (f32, f32), tw: f32, labels: &[(Edge, [f32; 4])]) -> (f32, f32) {
    let size = crate::ATTRIBUTION_SIZE;
    // Half the credit's height, its halo included, and the gap it keeps from a label.
    let (half, gap) = (size * 0.8, 2.0);
    let (mut bottom, mut right) = (0.0f32, 0.0f32);
    // The credit's foot set `gap` above a label's top `y`: how far up from the bottom that is.
    let above = |y: f32| h - size * 0.9 + half - y + gap;
    // Each move clears the label for good (the credit only goes up and left), so each label is
    // met once at most; the last resort ends the search.
    for _ in 0..=labels.len() {
        // Its whole width, where it is drawn: a credit wider than the frame runs past both edges.
        let (cx, cy) = (crate::attribution_center(w, tw, right), h - bottom - size * 0.9);
        let (x0, x1, y0, y1) = (cx - tw / 2.0, cx + tw / 2.0, cy - half, cy + half);
        let hit = labels
            .iter()
            .filter(|(e, _)| matches!(e, Edge::Bottom | Edge::Right))
            .find(|(_, b)| x0 < b[2] && b[0] < x1 && y0 < b[3] && b[1] < y1);
        match hit {
            // Left of the label, its right end `gap` short of it, when all of it stays in.
            Some((Edge::Right, b)) if b[0] - gap - tw >= 0.0 => right += x1 - (b[0] - gap),
            // Above the label, when all of it stays in.
            Some((Edge::Right, b)) if b[1] - gap - 2.0 * half >= 0.0 => bottom = above(b[1]),
            // Neither: as far left as the frame holds it (a credit wider than the frame stays put).
            Some((Edge::Right, _)) => {
                right += x0.max(0.0);
                break;
            }
            Some((_, b)) => bottom = above(b[1]),
            None => break,
        }
    }
    (bottom, right)
}

pub(super) fn shape_path(shape: &FrameShape, r: Rect) -> Option<tiny_skia::Path> {
    match shape {
        FrameShape::Rectangle => Some(PathBuilder::from_rect(r)),
        FrameShape::Circle => {
            let d = r.width().min(r.height());
            PathBuilder::from_oval(Rect::from_xywh(r.x() + (r.width() - d) / 2.0, r.y() + (r.height() - d) / 2.0, d, d)?)
        }
        FrameShape::Ellipse => PathBuilder::from_oval(r),
        FrameShape::Polygon(_) => {
            let mut pb = PathBuilder::new();
            for (i, (u, v)) in shape.outline().into_iter().enumerate() {
                let (px, py) = (r.x() + u as f32 * r.width(), r.y() + v as f32 * r.height());
                if i == 0 { pb.move_to(px, py) } else { pb.line_to(px, py) }
            }
            pb.close();
            pb.finish()
        }
    }
}

/// The lon/lat outline of what a frame shows, `None` where an edge has no place on the Earth (off
/// the disc of a frame on the globe, or outside a custom projection's domain): the outline breaks
/// there rather than cutting across the map it is drawn on, and reaches the place where the edge
/// leaves it (#2369).
pub(super) fn frame_outline_lonlat(map: &Map, cam: &rustgis_map::Camera, w: f64, h: f64) -> Vec<Option<Coord>> {
    let n = 16;
    let edge = |t: f64, side: usize| match side {
        0 => (t * w, 0.0),
        1 => (w, t * h),
        2 => ((1.0 - t) * w, h),
        _ => (0.0, (1.0 - t) * h),
    };
    // A frame on the globe is read through the globe as it draws it (#2348), with a gap where an
    // edge runs off the disc: one that shows the globe whole has no edge on it, and no outline.
    let globe = map.projection.is_globe().then(|| rustgis_map::globe::Globe::new(cam, w, h));
    let at = |(sx, sy): (f64, f64)| match &globe {
        Some(g) => g.inverse(sx, sy),
        None => map.to_lonlat(cam.to_map(sx, sy, w, h)),
    };
    // A frame on the globe has its longitudes kept continuous, a turn added or taken where they
    // cross ±180°, so on a flat frame the outline doesn't jump across the map there. A flat
    // frame's are drawn where they are.
    let mut last: Option<f64> = None;
    let mut ring = Vec::with_capacity(4 * n + 8);
    let mut push = |c: Option<Coord>| {
        let c = match (globe.is_some(), c, last) {
            (true, Some(c), Some(p)) => Some(Coord::new(c.x + 360.0 * ((p - c.x) / 360.0).round(), c.y)),
            _ => c,
        };
        if let Some(c) = c {
            last = Some(c.x);
        }
        ring.push(c);
    };
    let mut first = None;
    let mut prev: Option<((f64, f64), Option<Coord>)> = None;
    // Every side's samples, and the first again to close the ring.
    for i in 0..=4 * n {
        let p = edge((i % n) as f64 / n as f64, (i / n) % 4);
        let c = match first {
            Some(f) if i == 4 * n => f,
            _ => at(p),
        };
        first.get_or_insert(c);
        // Between a sample with a place and one without, the place where the edge leaves the
        // Earth, found by bisecting (24 halvings take a sixteenth of the edge to well under a
        // millionth of it): without it the outline stops up to a sample short of it.
        if let Some((q, b)) = prev
            && b.is_some() != c.is_some()
        {
            let (mut inside, mut outside, mut found) = if b.is_some() { (q, p, b) } else { (p, q, c) };
            for _ in 0..24 {
                let mid = ((inside.0 + outside.0) / 2.0, (inside.1 + outside.1) / 2.0);
                match at(mid) {
                    Some(m) => (inside, found) = (mid, Some(m)),
                    None => outside = mid,
                }
            }
            push(found);
        }
        push(c);
        prev = Some((p, c));
    }
    ring
}

/// The map frames of a page georeferenced for a PDF ([`frame_viewports`], #1191).
#[derive(Debug, Default)]
pub struct Georeferenced {
    /// The georeferenced frames, in drawing order, each in the rectangles no frame drawn over it
    /// covers (`uncovered`): no two overlap, so a reader never has two to choose from.
    pub viewports: Vec<crate::pdf::Viewport>,
    /// The frames that couldn't be, by id and name, with why.
    pub left_out: Vec<(u32, String, &'static str)>,
    /// The frames whose coordinate system can't be written as WKT (a grid shift, or a projection
    /// WKT1 has no name for), by id and name:
    /// georeferenced by their corners in longitude/latitude, which put the places of a 9 × 9 grid
    /// over the frame within [`MAX_CORNER_FIT_MM`] of where they are drawn (`corner_fit_mm`).
    pub by_corners: Vec<(u32, String)>,
}

/// How far, on paper, a frame georeferenced by its corners in longitude/latitude may put a place
/// from where the frame draws it: past it, the frame is left out (#1191).
pub const MAX_CORNER_FIT_MM: f64 = 0.5;

/// Each visible map frame of `layout` as a PDF viewport ([`crate::pdf::Viewport`]): its place on
/// the page as a page drawn at `dpi` places it, its corners' latitude and longitude, and its
/// map's coordinate system as WKT. A frame on the globe or tilted shows no flat map to measure
/// on, so it is left out, as is one on another body than the Earth, one whose map is missing,
/// one with a corner off the Earth, one across its projection's edge (the 180° meridian) and one
/// whose system has no WKT and whose corners alone would misplace it. A frame drawn over another,
/// left out or not, is cut out of the other's viewport, so that the other's coordinates don't show
/// through it: a viewport without coordinates would do it by the standard (the last viewport
/// holding a point is taken, ISO 32000-1, 12.9.2), but Acrobat Reader doesn't take one.
pub fn frame_viewports(layout: &Layout, maps: &[Map], dpi: f64) -> Georeferenced {
    let mut out = Georeferenced::default();
    // A frame is drawn from a whole pixel of the page ([`draw_page`]), whose pixels are stretched
    // over its paper: the viewport is that box, in millimetres.
    let (wpx, hpx, k) = page_geometry(layout, dpi);
    let (sx, sy) = (layout.width / f64::from(wpx.max(1)), layout.height / f64::from(hpx.max(1)));
    let frames: Vec<_> = layout
        .elements
        .iter()
        .filter(|e| e.visible && matches!(e.kind, ElementKind::MapFrame { .. }))
        // Rounded as `draw_page` rounds them, in single precision, so the box is the drawn one.
        .map(|e| (e, [f64::from(((e.x * k) as f32).round()) * sx, f64::from(((e.y * k) as f32).round()) * sy, e.w * k * sx, e.h * k * sy]))
        .collect();
    for (i, (e, placed)) in frames.iter().enumerate() {
        let ElementKind::MapFrame { map, center, scale, bearing, pitch, shape, .. } = &e.kind else { continue };
        // A viewport is a rectangle: one for a shaped frame over another would give its places
        // where the page shows the other's map, around the shape. (It still cuts the other.)
        let over =
            |[x, y, w, h]: [f64; 4]| x < placed[0] + placed[2] && placed[0] < x + w && y < placed[1] + placed[3] && placed[1] < y + h;
        if *shape != rustgis_map::layout::FrameShape::Rectangle && frames[..i].iter().any(|(_, r)| over(*r)) {
            out.left_out.push((
                e.id,
                e.name.clone(),
                "it isn't a rectangle and is drawn over another frame, whose map shows around its shape",
            ));
            continue;
        }
        let Some(m) = map_index(maps, map).and_then(|i| maps.get(i)) else {
            out.left_out.push((e.id, e.name.clone(), "its map is missing"));
            continue;
        };
        if !placed.iter().all(|v| v.is_finite()) {
            out.left_out.push((e.id, e.name.clone(), "its place on the page isn't a number"));
            continue;
        }
        // What the frames drawn over it leave of it.
        let mut shown = vec![*placed];
        for (_, over) in &frames[i + 1..] {
            shown = shown.into_iter().flat_map(|r| uncovered(r, *over)).collect();
            // Past the most, it is left out: cutting on would only make more.
            if shown.len() > MAX_VIEWPORTS_A_FRAME {
                break;
            }
        }
        if shown.is_empty() {
            out.left_out.push((e.id, e.name.clone(), "the frames drawn over it cover all of it"));
            continue;
        }
        if shown.len() > MAX_VIEWPORTS_A_FRAME {
            out.left_out.push((e.id, e.name.clone(), "too many frames are drawn over it"));
            continue;
        }
        match frame_viewport(e, *placed, &shown, m, rustgis_map::layout::frame_camera(m, *center, *scale, *bearing, *pitch)) {
            Ok((v, exact)) => {
                if !exact {
                    out.by_corners.push((e.id, e.name.clone()));
                }
                out.viewports.extend(v);
            }
            Err(why) => out.left_out.push((e.id, e.name.clone(), why)),
        }
    }
    out
}

/// The most rectangles a frame's viewport is cut into by the frames drawn over it; past it, the
/// frame is left out.
pub(super) const MAX_VIEWPORTS_A_FRAME: usize = 64;

/// A strip narrower than this, in millimetres, left of a frame beside one drawn over it, is
/// dropped: no one points into it.
pub(super) const MIN_STRIP_MM: f64 = 0.01;

/// Rectangle `r` (x, y, width, height) without `cut`: up to four rectangles, the bands above and
/// below the cut, and beside it between them.
pub(super) fn uncovered(r: [f64; 4], cut: [f64; 4]) -> Vec<[f64; 4]> {
    let (x0, y0, x1, y1) = (r[0], r[1], r[0] + r[2], r[1] + r[3]);
    let (cx0, cy0, cx1, cy1) = (cut[0].max(x0), cut[1].max(y0), (cut[0] + cut[2]).min(x1), (cut[1] + cut[3]).min(y1));
    if cx0 >= cx1 || cy0 >= cy1 {
        return vec![r];
    }
    [(x0, y0, x1, cy0), (x0, cy1, x1, y1), (x0, cy0, cx0, cy1), (cx1, cy0, x1, cy1)]
        .into_iter()
        .filter(|(a, b, c, d)| c - a >= MIN_STRIP_MM && d - b >= MIN_STRIP_MM)
        .map(|(a, b, c, d)| [a, b, c - a, d - b])
        .collect()
}

/// How far, in millimetres on paper, a reader that fits a frame's corners in longitude/latitude
/// (`corners`, latitude then longitude, clockwise from the top-left) puts the places on a 9 × 9
/// grid over it, its edges included, from where the frame, `w` × `h` points seen by `cam`, draws
/// them: the worst of them, the fit being exact at the corners. `None` when one of them isn't on
/// the map.
pub(super) fn corner_fit_mm(corners: &[(f64, f64); 4], map: &Map, cam: &rustgis_map::Camera, w: f64, h: f64) -> Option<f64> {
    let [tl, tr, br, bl] = *corners;
    let mut worst: f64 = 0.0;
    // A 9 × 9 grid over the frame, its edges included (the corners are exact).
    let grid = (0..=8).flat_map(|i| (0..=8).map(move |j| (f64::from(i) / 8.0, f64::from(j) / 8.0)));
    for (u, t) in grid {
        // Between the corners, as the unit square's (`/LPTS`) is filled: bilinearly.
        let mix = |a: f64, b: f64, c: f64, d: f64| (1.0 - u) * (1.0 - t) * a + u * (1.0 - t) * b + u * t * c + (1.0 - u) * t * d;
        let (lat, lon) = (mix(tl.0, tr.0, br.0, bl.0), mix(tl.1, tr.1, br.1, bl.1));
        let at = map.to_map(Coord::new((lon + 540.0).rem_euclid(360.0) - 180.0, lat))?;
        let (x, y) = cam.try_to_screen(at, w, h)?;
        // `f64::max` ignores NaN: a sample that isn't a number is no fit.
        let off = (x - u * w).hypot(y - t * h);
        if !off.is_finite() {
            return None;
        }
        worst = worst.max(off);
    }
    (worst.is_finite()).then_some(worst / PT_PER_MM)
}

/// Frame `e` of `map`, placed at `placed` (millimetres: x, y, width, height) and seen by `cam`, as
/// a viewport for each rectangle of it in `shown`, and whether it is in the map's own coordinate
/// system (else in longitude/latitude, by its corners).
pub(super) fn frame_viewport(
    e: &rustgis_map::layout::Element,
    placed: [f64; 4],
    shown: &[[f64; 4]],
    map: &Map,
    cam: rustgis_map::Camera,
) -> Result<(Vec<crate::pdf::Viewport>, bool), &'static str> {
    use rustgis_data::raster::{crs, wkt};
    if map.projection.is_globe() {
        return Err("it shows the globe");
    }
    // Its coordinates are another body's, which an Earth system would misplace.
    if !map.body().is_earth() {
        return Err("it is on another body than the Earth");
    }
    if cam.pitch.abs() > 1e-9 {
        return Err("it is tilted");
    }
    // The map's coordinate system: its WKT and EPSG code, and its PROJ definition (whose datum
    // the corners are given on).
    let (system, def) = match &map.projection {
        rustgis_map::Projection::Custom(code) => {
            let (def, label) = rustgis_map::projection::lookup(code).map_err(|_| "its projection can't be read")?;
            let lower = code.trim().to_ascii_lowercase();
            let digits = lower.strip_prefix("epsg").map_or(lower.as_str(), |r| r.trim_start_matches([':', ' ']));
            let epsg = digits.parse::<u16>().ok().filter(|_| digits.bytes().all(|b| b.is_ascii_digit()));
            let text = match epsg {
                Some(c) => wkt::to_wkt(crs::Crs::Epsg(c)),
                None => wkt::proj4_to_wkt(&def, &label),
            };
            (text.map(|t| (t, epsg)), Some(def))
        }
        _ => (wkt::to_wkt(crs::Crs::WebMercator).map(|t| (t, Some(3857))), None),
    };
    let (w, h) = (e.w * PT_PER_MM, e.h * PT_PER_MM);
    // The latitude and longitude of the frame's point `(sx, sy)` (points from its top-left).
    let place = |sx: f64, sy: f64| -> Result<(f64, f64), &'static str> {
        let m = cam.try_to_map(sx, sy, w, h).ok_or("a corner is off the Earth")?;
        let c = map.to_lonlat(m).ok_or("a corner is off the Earth")?;
        // A corner past the projection's edge (Web Mercator's world repeated across 180°) is at a
        // longitude past ±180°, or comes back elsewhere: a reader, which takes longitudes within
        // ±180°, would put it on the other side of the world. (In longitude/latitude, below, the
        // corners are kept within half a turn of each other instead.)
        let back = map.to_map(c).ok_or("a corner is off the Earth")?;
        let elsewhere = (back.x - m.x).abs() > 1e-6 * m.x.abs().max(1.0) || (back.y - m.y).abs() > 1e-6 * m.y.abs().max(1.0);
        if elsewhere || (system.is_some() && c.x.abs() > 180.0) {
            return Err("it crosses its projection's edge (the 180° meridian)");
        }
        // On the system's own datum, as a reader that takes its WKT reads them.
        let c = match (&system, &def) {
            (Some(_), Some(def)) => crs::lonlat_on_datum_of(def, c).ok_or("its corners can't be moved onto its datum")?,
            _ => c,
        };
        // A value no PDF number can be (from a hostile project) is no place.
        if !(c.x.is_finite() && c.y.is_finite()) {
            return Err("a corner is off the Earth");
        }
        Ok((c.y, c.x))
    };
    // Clockwise from the top-left, as `/LPTS` takes them.
    let corners_of = |[x0, y0, x1, y1]: [f64; 4]| -> Result<[(f64, f64); 4], &'static str> {
        Ok([place(x0, y0)?, place(x1, y0)?, place(x1, y1)?, place(x0, y1)?])
    };
    let mut corners = corners_of([0.0, 0.0, w, h])?;
    let exact = system.is_some();
    let (wkt, epsg) = match &system {
        Some((text, code)) => (text.clone(), *code),
        None => {
            // In longitude/latitude the corners are fitted as they are: across 180° they are kept
            // within half a turn of the first, so that the frame isn't taken to span the world.
            let first = corners[0].1;
            for c in &mut corners {
                c.1 = first + ((c.1 - first + 540.0).rem_euclid(360.0) - 180.0);
            }
            // A reader is right at the corners only: the frame is written if, between them, it
            // puts no place more than half a millimetre on paper from where the frame draws it.
            match corner_fit_mm(&corners, map, &cam, w, h) {
                Some(off) if off <= MAX_CORNER_FIT_MM => {}
                Some(_) => {
                    return Err(
                        "its coordinate system can't be written as WKT, and its corners alone would put places more than 0.5 mm off",
                    );
                }
                None => return Err("its coordinate system can't be written as WKT, and a place between its corners is off the map"),
            }
            (wkt::to_wkt(crs::Crs::LonLat).ok_or("its projection can't be read")?, Some(4326))
        }
    };
    let first = corners[0].1;
    let mut viewports = Vec::with_capacity(shown.len());
    for r in shown {
        // The rectangle's corners in the frame's points, the frame being `placed` on the page.
        let to = |mm: f64, from: f64, size: f64, pts: f64| (mm - from) / size * pts;
        let (x0, y0) = (to(r[0], placed[0], placed[2], w), to(r[1], placed[1], placed[3], h));
        let (x1, y1) = (to(r[0] + r[2], placed[0], placed[2], w), to(r[1] + r[3], placed[1], placed[3], h));
        let mut corners = if *r == placed { corners } else { corners_of([x0, y0, x1, y1])? };
        if !exact {
            for c in &mut corners {
                c.1 = first + ((c.1 - first + 540.0).rem_euclid(360.0) - 180.0);
            }
        }
        let measure = crate::pdf::GeoMeasure { corners, wkt: wkt.clone(), projected: exact, epsg };
        viewports.push(crate::pdf::Viewport { name: e.name.clone(), rect_mm: *r, measure });
    }
    Ok((viewports, exact))
}

/// A ring (longitude, latitude), broken at each `None`, placed on a flat map for drawing: where its
/// points go on the map's world, whose middle (its camera's centre) is at `mid`. `from_globe`: the
/// ring is a frame on the globe's, its longitudes run on across ±180° rather than kept in a world.
///
/// A frame on the globe has its whole ring shifted a turn east or west if that brings the middle
/// of its longitudes nearer the map's middle, as they run on from its first point (#2348), which
/// can be on the other side of ±180° from it: one shift for all keeps it in one piece on a map
/// that doesn't wrap. A flat frame's ring is placed where it is. Then a ring that a map can't show in one piece
/// is cut at the map's edge as the map cuts features (#209), each piece brought into the map's
/// world and running to the edge (#2369): any ring on a world projection, which wraps at its edge
/// (on Robinson centred on 150°E at -30°, with its datum's margin), and on Web Mercator, which
/// doesn't, a globe frame's around a pole, which runs a whole turn, at ±180°.
pub(super) fn ring_on_flat(map: &Map, mid: Option<Coord>, ring: &[Option<Coord>], from_globe: bool) -> Vec<Option<Coord>> {
    use rustgis_data::Geometry;
    let (west, east) = ring.iter().flatten().fold((f64::INFINITY, f64::NEG_INFINITY), |(w, e), c| (w.min(c.x), e.max(c.x)));
    let around_pole = from_globe && east - west >= 360.0 - 1e-6;
    let shift = match (from_globe && !around_pole && west <= east, mid) {
        (true, Some(mid)) => 360.0 * ((mid.x - (west + east) / 2.0) / 360.0).round(),
        _ => 0.0,
    };
    let shifted = ring.iter().map(|c| c.map(|c| Coord::new(c.x + shift, c.y)));
    let flat = map.flat();
    let edge = match &flat {
        rustgis_map::projection::Flat::Custom(p) => p.edge().copied(),
        rustgis_map::projection::Flat::Mercator => None,
    };
    if edge.is_none() && !around_pole {
        return shifted.collect();
    }
    let shifted: Vec<Option<Coord>> = shifted.collect();
    let runs = shifted.split(Option::is_none).map(|r| r.iter().flatten().copied().collect()).collect();
    let (lon0, margin) = edge.map_or((0.0, None), |e| (e.lon - 180.0, Some(e)));
    let margin = |lat: f64| margin.map_or(0.0, |e| e.stray_at(lat));
    // A line cut is a line: the `else` is never taken.
    let Geometry::Line(pieces) = rustgis_map::antimeridian::cut_with_margin(&Geometry::Line(runs), lon0, &margin) else {
        return shifted;
    };
    pieces.into_iter().flat_map(|p| p.into_iter().map(Some).chain([None])).collect()
}

/// Strokes the ring `ring` (longitude, latitude) on a frame's map `width` points wide: another
/// frame's extent, as an extent indicator shows it, broken at each `None`. `from_globe`: the ring
/// is a frame on the globe's, its longitudes run on across ±180° rather than kept in a world.
pub(super) fn outline_lonlat(pm: &mut Canvas, map: &Map, view: &View, ring: &[Option<Coord>], from_globe: bool, color: Rgba, width: f32) {
    let ppp = view.pixels_per_point as f32;
    // On a globe, through the globe as the frame draws it, breaking behind its horizon (#2348):
    // through the flat camera the outline lands off the area once it is away from the centre.
    let globe = map.projection.is_globe().then(|| rustgis_map::globe::Globe::new(&view.camera, view.width, view.height));
    let placed;
    let ring = match globe {
        Some(_) => ring,
        None => {
            placed = ring_on_flat(map, map.to_lonlat(view.camera.center), ring, from_globe);
            &placed[..]
        }
    };
    let project = |c: Coord| match &globe {
        Some(g) => g.forward(c),
        // A tilted frame's outline breaks where it passes behind the eye, and any frame's where
        // its projection can't show the place (#2369).
        None => map.to_map(c).and_then(|m| view.camera.try_to_screen(m, view.width, view.height)),
    };
    let mut pb = PathBuilder::new();
    let mut first = true;
    let mut push = |at: Option<(f64, f64)>| {
        let Some((x, y)) = at else {
            first = true;
            return;
        };
        let (x, y) = (x as f32 * ppp, y as f32 * ppp);
        if first {
            pb.move_to(x, y);
            first = false;
        } else {
            pb.line_to(x, y);
        }
    };
    let mut previous: Option<(Coord, Option<(f64, f64)>)> = None;
    for &c in ring {
        let at = c.and_then(project);
        if globe.is_none()
            && let (Some((last, last_at)), Some(c)) = (previous, c)
            && last_at.is_some() != at.is_some()
        {
            // Reach the overview's projection boundary, rather than stop at the last sample
            // inside it (#2433). Keep the valid side of 24 halvings, as frame_outline_lonlat
            // does for the source frame's boundary. An explicit gap never enters this search.
            let (mut inside, mut outside, mut found) = if last_at.is_some() { (last, c, last_at) } else { (c, last, at) };
            for _ in 0..24 {
                // Samples can straddle ±180° on a regional projection without a wrapping edge:
                // follow their short arc rather than bisect across the other side of the map.
                let lon = inside.x + ((outside.x - inside.x + 540.0).rem_euclid(360.0) - 180.0) / 2.0;
                let mid = Coord::new(lon, (inside.y + outside.y) / 2.0);
                match project(mid) {
                    Some(point) => (inside, found) = (mid, Some(point)),
                    None => outside = mid,
                }
            }
            push(found);
        }
        push(at);
        previous = c.map(|c| (c, at));
    }
    if let Some(p) = pb.finish() {
        pm.stroke_path(&p, &paint(color, 1.0), &Stroke { width: width * ppp, ..Stroke::default() }, Transform::identity(), None);
    }
}
