//! Drawing raster layers.
//!
//! Each screen pixel is traced back to the raster's grid: Web Mercator → the raster's own
//! coordinate system → pixel position. The full transformation runs on a coarse grid of screen
//! points and is interpolated in between, which is exact for Web Mercator and lon/lat rasters
//! and well under a pixel off for projected ones. Values are then sampled (nearest or bilinear),
//! optionally re-stretched from the pixels in view (DRA), and coloured.

use rustgis_data::raster::{BandStats, ColorHint, Raster};
use rustgis_geom::{Coord, Extent, extent_to_merc};
use rustgis_map::Globe;
use rustgis_map::projection::Flat;
use rustgis_map::raster::{Colorizer, RasterMask, RasterRenderer, RasterStyle, Resampling, hillshade};
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

use crate::View;

/// Screen pixels between exactly transformed points.
const STEP: usize = 8;

/// The raster pixel position under a work pixel (`k` device pixels), or `None` off the
/// projection.
/// Where a raster is drawn: on the globe, or on the flat map in a projection (with its code).
#[derive(Clone, Copy)]
pub(crate) enum Place<'a> {
    Globe(&'a Globe),
    Flat(&'a Flat, &'a str),
}

fn source(r: &Raster, view: &View, place: Place, k: f64, dx: f64, dy: f64) -> Option<(f64, f64)> {
    let ppp = view.pixels_per_point / k;
    let (sx, sy) = (dx / ppp, dy / ppp);
    let n = match place {
        Place::Globe(g) => r.transformer().from_lonlat(g.inverse(sx, sy)?)?,
        Place::Flat(Flat::Mercator, _) => r.transformer().from_merc(view.camera.to_map(sx, sy, view.width, view.height))?,
        Place::Flat(flat, key) => {
            let m = view.camera.to_map(sx, sy, view.width, view.height);
            // A raster already in the map's coordinate system needs no reprojection.
            if key.strip_prefix("EPSG:").and_then(|c| c.parse::<u16>().ok()) == Some(r.crs.epsg()) {
                m
            } else {
                r.transformer().from_lonlat(flat.inverse(m)?)?
            }
        }
    };
    let (c, row) = r.transform.to_pixel(n);
    (c.is_finite() && row.is_finite()).then_some((c, row))
}

/// A whole geographic turn has exactly this many columns. Compare the grid's span rather
/// than truncating `360 / pixel_width`: a regional grid can end partway through that column.
pub(crate) fn full_turn_width(r: &Raster) -> Option<i64> {
    let span = f64::from(r.width) * r.transform.pixel_width.abs();
    (r.transformer().is_geographic() && (span - 360.0).abs() <= 360.0 * 16.0 * f64::EPSILON).then_some(i64::from(r.width))
}

/// The value of `band` at a pixel, unless it is no data, masked or outside the grid.
fn value(r: &Raster, mask: &RasterMask, band: usize, col: i64, row: i64, wrap_width: Option<i64>) -> Option<f32> {
    let col = wrap_width.map_or(col, |w| if col < 0 || col >= w { col.rem_euclid(w) } else { col });
    r.value(band, col, row).filter(|v| !mask.hides(*v))
}

fn sample(r: &Raster, mask: &RasterMask, band: usize, c: f64, row: f64, resampling: Resampling, wrap_width: Option<i64>) -> f32 {
    let nearest = || value(r, mask, band, c.floor() as i64, row.floor() as i64, wrap_width).unwrap_or(f32::NAN);
    if resampling == Resampling::Nearest {
        return nearest();
    }
    // Bilinear between the four pixel centres around the point.
    let (x, y) = (c - 0.5, row - 0.5);
    let (i, j) = (x.floor(), y.floor());
    let (fx, fy) = ((x - i) as f32, (y - j) as f32);
    let (i, j) = (i as i64, j as i64);
    let v = |i, j| value(r, mask, band, i, j, wrap_width);
    match (v(i, j), v(i + 1, j), v(i, j + 1), v(i + 1, j + 1)) {
        (Some(a), Some(b), Some(cc), Some(d)) => {
            let top = a + (b - a) * fx;
            let bottom = cc + (d - cc) * fx;
            top + (bottom - top) * fy
        }
        _ => nearest(),
    }
}

fn place_is_flat(p: &Place) -> bool {
    matches!(p, Place::Flat(..))
}

/// Draws a raster layer onto `pm` at `opacity`. `stats` are the renderer bands' statistics
/// without the masked pixels, when the style has a mask.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    pm: &mut Pixmap,
    r: &Raster,
    style: &RasterStyle,
    stats: Option<&[BandStats]>,
    view: &View,
    opacity: f32,
    full_res: bool,
    place: Place,
) {
    let threads = threads_for(pm.width() as usize * pm.height() as usize);
    draw_with(pm, r, style, stats, view, opacity, full_res, place, threads);
}

/// Screen points a side sampled to find the part of a raster read in place that is in view, and
/// how finely.
const WINDOW_GRID: usize = 17;
/// Why a part in view was drawn from a coarser level than it needs.
const TOO_LARGE: &str = "the view is too large a part of the image to read at the resolution it needs";
/// Why a turned layer is drawn from its overview.
const TURNED: &str = "turned, it is drawn from its overview, not at the resolution the view needs";

/// Whether `rotation` (degrees) turns the raster: anything but a whole number of turns. A hair
/// either side of none (`-1e-7` as much as `1e-7`) isn't one.
fn turned(rotation: f64) -> bool {
    let r = rotation.rem_euclid(360.0);
    r.min(360.0 - r) > 1e-6
}

/// Why a see-through layer with tiles to come is drawn from its overview alone: no memory for
/// the image that blends the part in view with it.
const UNBLENDED: &str = "no memory to blend the part in view with its overview, so it is drawn from its overview";

/// Draws a raster layer read in place (a cloud-optimised GeoTIFF by its address, see
/// [`rustgis_map::raster::RasterLayer::in_place`]) as [`draw`] draws one held whole: its overview
/// `r` (the layer's pixels), or, zoomed in past it, the part in view read from the level the view
/// needs, drawn over the overview until its tiles arrive. `stats` are the renderer's bands'
/// statistics (the overview's when not given), so the part in view is stretched as the whole is.
/// Returns whether tiles are still to come (the map is drawn again as they land), or why it drew
/// less than the view needs (the overview, or other levels for tiles that couldn't be read): the
/// map shows what it has, an export refuses it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_in_place(
    pm: &mut Pixmap,
    address: &str,
    r: &Raster,
    style: &RasterStyle,
    stats: Option<&[BandStats]>,
    view: &View,
    opacity: f32,
    full_res: bool,
    place: Place,
) -> Result<bool, String> {
    let bands = style.renderer.bands();
    let own: Vec<BandStats> = match stats {
        Some(_) => Vec::new(),
        None => bands.iter().map(|b| r.bands.get(*b).map(|b| b.stats.clone()).unwrap_or_default()).collect(),
    };
    let stats = stats.unwrap_or(&own);
    let whole = |pm: &mut Pixmap| draw(pm, r, style, Some(stats), view, opacity, full_res, place);
    let full = r.overview.unwrap_or((r.width, r.height));
    if full.0 <= r.width {
        whole(pm);
        return Ok(false);
    }
    // A rotated raster turns about the whole image's centre, not the part in view's: the
    // overview is drawn, and zoomed in past it (a screen pixel under 0.8 of its pixels) that is
    // less than the view needs: said, and refused by exports.
    if turned(style.rotation) {
        whole(pm);
        let zoomed_in = in_view(r, view, place, pm.width(), pm.height(), full_res).is_some_and(|w| w.per_px < 0.8);
        return if zoomed_in { Err(TURNED.into()) } else { Ok(false) };
    }
    let info = match rustgis_tiles::cog::info(address) {
        Ok(Some(info)) => info,
        Ok(None) => {
            whole(pm);
            return Ok(true);
        }
        Err(e) => {
            whole(pm);
            return Err(e);
        }
    };
    let Some(want) = in_view(r, view, place, pm.width(), pm.height(), full_res) else {
        whole(pm);
        return Ok(false);
    };
    // The coarsest level at least as fine as the view (within a quarter); zoomed in past the full
    // image, the full image.
    let levels = &info.layout.levels;
    let full_px = want.per_px * f64::from(full.0) / f64::from(r.width.max(1));
    let fine_enough = |l: &rustgis_data::raster::cog::Level| f64::from(full.0) / f64::from(l.width.max(1)) <= full_px * 1.25;
    let mut level = levels.iter().rposition(fine_enough).unwrap_or(0);
    let needed = level;
    // The renderer's bands, then the image's alpha band (again, when a colour draws it too), read
    // as the window's own bands, as `draw` reads a raster held whole.
    let mut read = bands.clone();
    let alpha = match (&style.renderer, &r.color) {
        (RasterRenderer::Rgb { .. }, ColorHint::Rgb { alpha: true }) if r.band_count() >= 4 => Some(3),
        (RasterRenderer::Stretch { band: 0, .. } | RasterRenderer::ShadedRelief { band: 0, .. }, ColorHint::GrayAlpha) => Some(1),
        _ => None,
    };
    read.extend(alpha);
    // The strips in view: one, or two across a 0–360° grid's seam.
    let strips: Vec<(f64, f64)> = std::iter::once(want.cols).chain(want.cols2).collect();
    let mut window = None;
    while let Some(l) = levels.get(level) {
        // The part in view, in the level's pixels, a pixel past each side (for bilinear sampling).
        let (sx, sy) = (f64::from(l.width) / f64::from(r.width.max(1)), f64::from(l.height) / f64::from(r.height.max(1)));
        let edge = |v: f64, k: f64, max: u32| ((v * k).floor() as i64).clamp(0, i64::from(max)) as u32;
        let cols: Vec<(u32, u32)> = strips
            .iter()
            .map(|c| (edge(c.0, sx, l.width).saturating_sub(1), edge(c.1, sx, l.width).saturating_add(2).min(l.width)))
            .collect();
        let rows = (edge(want.rows.0, sy, l.height).saturating_sub(1), edge(want.rows.1, sy, l.height).saturating_add(2).min(l.height));
        if cols.iter().all(|cols| rustgis_tiles::cog::raster::fits(address, level, *cols, rows, &read)) {
            window = Some((level, cols, rows));
            break;
        }
        // Too large a part for the tiles kept (a view far wider than the screen, a file of many
        // bands stored together): a coarser level.
        level += 1;
    }
    let Some((level, cols, rows)) = window else {
        whole(pm);
        return Err(TOO_LARGE.into());
    };
    // The image at the overview's level is the overview: reached from a finer level the view
    // needs (too large a part for the tiles kept), it is drawn but refused, as a coarser level is
    // below.
    if levels.get(level).is_some_and(|l| l.width <= r.width) {
        whole(pm);
        return if level > needed { Err(TOO_LARGE.into()) } else { Ok(false) };
    }
    let color = match (&style.renderer, &r.color) {
        (_, ColorHint::Palette(p)) => ColorHint::Palette(p.clone()),
        (RasterRenderer::Rgb { .. }, _) => ColorHint::Rgb { alpha: alpha.is_some() },
        _ if alpha.is_some() => ColorHint::GrayAlpha,
        _ => ColorHint::Gray,
    };
    // Each strip read from the tiles kept, the missing ones asked for.
    let (mut parts, mut pending, mut failed) = (Vec::new(), false, false);
    for cols in cols {
        match rustgis_tiles::cog::raster::window(address, level, cols, rows, &read, color.clone()) {
            Ok(w) => {
                pending |= w.pending;
                failed |= w.failed;
                parts.extend(w.raster);
            }
            Err(e) => {
                whole(pm);
                return Err(e);
            }
        }
    }
    if parts.is_empty() {
        whole(pm);
        return Ok(pending);
    }
    let mut own_style = style.clone();
    own_style.renderer = renumbered(&style.renderer);
    // While it has gaps (tiles to come, or that couldn't be read and no level kept stands in for),
    // the part in view goes over the overview: both drawn opaque, then blended once, so a
    // see-through layer isn't darker where they overlap.
    if pending || failed {
        if opacity >= 1.0 {
            whole(pm);
            for part in &parts {
                draw(pm, part, &own_style, Some(stats), view, 1.0, full_res, place);
            }
        } else if let Some(mut both) = Pixmap::new(pm.width(), pm.height()) {
            draw(&mut both, r, style, Some(stats), view, 1.0, full_res, place);
            for part in &parts {
                draw(&mut both, part, &own_style, Some(stats), view, 1.0, full_res, place);
            }
            pm.draw_pixmap(0, 0, both.as_ref(), &PixmapPaint { opacity, ..PixmapPaint::default() }, Transform::identity(), None);
        } else {
            // No image to blend in: the overview alone, and said (the map shows it, an export
            // refuses it), rather than nothing.
            whole(pm);
            return Err(UNBLENDED.into());
        }
    } else {
        for part in &parts {
            draw(pm, part, &own_style, Some(stats), view, opacity, full_res, place);
        }
    }
    match (pending, failed) {
        (true, _) => Ok(true),
        (false, true) => Err("some of its tiles couldn't be read".into()),
        // Read coarser than the view needs (too large a part for the tiles kept): the map shows
        // it, an export refuses it.
        (false, false) if level > needed => Err(TOO_LARGE.into()),
        (false, false) => Ok(false),
    }
}

/// A renderer drawing the same bands of a raster holding only those, in its order (see
/// [`RasterRenderer::bands`]).
fn renumbered(renderer: &RasterRenderer) -> RasterRenderer {
    let mut out = renderer.clone();
    match &mut out {
        RasterRenderer::Stretch { band, .. }
        | RasterRenderer::Classify { band, .. }
        | RasterRenderer::UniqueValues { band, .. }
        | RasterRenderer::ShadedRelief { band, .. } => *band = 0,
        RasterRenderer::Rgb { red, green, blue } => (*red, *green, *blue) = (0, 1, 2),
        RasterRenderer::Colormap => {}
    }
    out
}

/// The part of a raster in view: its columns and rows (half-open, in its own pixels, possibly
/// fractional), and how many of its pixels a pixel drawn spans.
#[derive(Debug, PartialEq)]
struct InView {
    cols: (f64, f64),
    /// A second run of columns: a geographic grid stored from 0 to 360° seen across its seam has
    /// the places west of 0° at its far columns, so the part in view is two strips, not the width.
    cols2: Option<(f64, f64)>,
    rows: (f64, f64),
    per_px: f64,
}

/// [`InView`] of a raster drawn into a `width × height` pixmap, from a grid of screen points;
/// `None` when it isn't in view.
fn in_view(r: &Raster, view: &View, place: Place, width: u32, height: u32, full_res: bool) -> Option<InView> {
    let k = if view.pixels_per_point >= 1.5 && !full_res { view.pixels_per_point } else { 1.0 };
    let (vw, vh) = (f64::from(width) / k, f64::from(height) / k);
    let n = WINDOW_GRID;
    let at = |i: usize, j: usize| source(r, view, place, k, vw * i as f64 / (n - 1) as f64, vh * j as f64 / (n - 1) as f64);
    let grid: Vec<Option<(f64, f64)>> = (0..n * n).map(|p| at(p % n, p / n)).collect();
    let (mut c0, mut c1, mut r0, mut r1) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
    let (rw, rh) = (f64::from(r.width), f64::from(r.height));
    // A geographic grid stored from 0 to 360° has the places west of 0° at its far columns (see
    // `Raster::wrap_col`): seen across that seam, the part in view is two strips, one at each end.
    let (mut w0, mut w1) = (f64::INFINITY, f64::NEG_INFINITY);
    for (c, row) in grid.iter().flatten() {
        let col = r.wrap_col(*c);
        if col != *c {
            w0 = w0.min(col.clamp(0.0, rw));
            w1 = w1.max(col.clamp(0.0, rw));
        } else {
            c0 = c0.min(col.clamp(0.0, rw));
            c1 = c1.max(col.clamp(0.0, rw));
        }
        r0 = r0.min(row.clamp(0.0, rh));
        r1 = r1.max(row.clamp(0.0, rh));
    }
    let mut cols2 = None;
    if w0 < w1 {
        if c0 < c1 {
            // Both sides: the seam is in view, so each strip runs to it (the sample points stop
            // short of it by up to a grid step).
            cols2 = Some((w0, rw));
            c0 = 0.0;
        } else {
            (c0, c1) = (w0, w1);
        }
    }
    if !(c0 < c1 && r0 < r1) {
        return None;
    }
    // The raster's pixels a screen pixel spans: the smallest step between neighbouring points,
    // where the view is finest (a tilted or curved view is finer near its centre).
    let step = (vw / (n - 1) as f64).max(1.0);
    let mut per_px = f64::INFINITY;
    for j in 0..n {
        for i in 0..n - 1 {
            if let (Some(Some(a)), Some(Some(b))) = (grid.get(j * n + i), grid.get(j * n + i + 1)) {
                let d = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt() / step;
                if d > 0.0 {
                    per_px = per_px.min(d);
                }
            }
        }
    }
    // In the pixels drawn (a work pixel is `k` device pixels).
    per_px.is_finite().then_some(InView { cols: (c0, c1), cols2, rows: (r0, r1), per_px })
}

/// [`draw`], its rows spread over `threads` threads.
#[allow(clippy::too_many_arguments)]
fn draw_with(
    pm: &mut Pixmap,
    r: &Raster,
    style: &RasterStyle,
    stats: Option<&[BandStats]>,
    view: &View,
    opacity: f32,
    full_res: bool,
    place: Place,
    threads: usize,
) {
    // On high-density screens, compute at logical-pixel resolution and scale up (unless asked
    // for full resolution, as exports are): a quarter of the work at 2×.
    let k = if view.pixels_per_point >= 1.5 && !full_res { view.pixels_per_point } else { 1.0 };
    let (w, h) = ((pm.width() as f64 / k).ceil(), (pm.height() as f64 / k).ceil());
    let ppp = view.pixels_per_point / k;
    // The raster's bounds on screen, clipped to the view.
    let ((ax, ay), (bx, by)) = match place {
        // On the globe: the box around the visible part of a grid of points over the extent.
        Place::Globe(g) => {
            let e = r.extent();
            let mut b = Extent::EMPTY;
            for i in 0..=24 {
                for j in 0..=24 {
                    let c = Coord::new(e.min_x + e.width() * i as f64 / 24.0, e.min_y + e.height() * j as f64 / 24.0);
                    if let Some((x, y)) = g.forward(c) {
                        b.expand(Coord::new(x, y));
                    }
                }
            }
            // A raster under the centre of view may reach past every sample point: take the disc.
            if e.contains(view.camera.center_lonlat()) {
                let (cx, cy) = g.center();
                let (rx, ry) = (g.disc_radius(), g.disc_height());
                ((cx - rx, cy - ry), (cx + rx, cy + ry))
            } else if b.is_empty() {
                return;
            } else {
                // A margin for the parts between the sample points.
                let m = (b.width().max(b.height()) / 24.0).max(2.0);
                ((b.min_x - m, b.min_y - m), (b.max_x + m, b.max_y + m))
            }
        }
        Place::Flat(Flat::Mercator, _) => {
            let e = extent_to_merc(&r.extent());
            (view.to_screen(Coord::new(e.min_x, e.max_y)), view.to_screen(Coord::new(e.max_x, e.min_y)))
        }
        Place::Flat(flat, _) => {
            let e = flat.extent(&r.extent());
            if e.is_empty() {
                return;
            }
            (view.to_screen(Coord::new(e.min_x, e.max_y)), view.to_screen(Coord::new(e.max_x, e.min_y)))
        }
    };
    let clip = |v: f64, max: f64| if v.is_finite() { v.clamp(0.0, max) } else { 0.0 };
    // A rotated raster turns about its centre: its pixels may land anywhere in view.
    let rot = style.rotation.rem_euclid(360.0);
    let rotated = rot.abs() > 1e-6 && place_is_flat(&place);
    let centre = (((ax + bx) / 2.0) * ppp, ((ay + by) / 2.0) * ppp);
    let (sin, cos) = (-rot.to_radians()).sin_cos();
    let (x0, x1, y0, y1) = if rotated {
        (0, w as usize, 0, h as usize)
    } else {
        (
            clip((ax.min(bx) * ppp).floor(), w) as usize,
            clip((ax.max(bx) * ppp).ceil(), w) as usize,
            clip((ay.min(by) * ppp).floor(), h) as usize,
            clip((ay.max(by) * ppp).ceil(), h) as usize,
        )
    };
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let (bw, bh) = (x1 - x0, y1 - y0);

    // Exact source positions on a coarse grid.
    let (gw, gh) = (bw / STEP + 2, bh / STEP + 2);
    let mut grid = Vec::with_capacity(gw * gh);
    for gy in 0..gh {
        for gx in 0..gw {
            grid.push(source(r, view, place, k, (x0 + gx * STEP) as f64, (y0 + gy * STEP) as f64));
        }
    }

    // Pass 1: band values (and hillshade) for each screen pixel.
    let bands = style.renderer.bands();
    let nb = bands.len().max(1);
    let relief = match style.renderer {
        RasterRenderer::ShadedRelief { azimuth, altitude, z_factor, .. } => Some((azimuth, altitude, z_factor)),
        _ => None,
    };
    // Blending neighbours would invent values that aren't classes or palette entries.
    let resampling = match style.renderer {
        RasterRenderer::UniqueValues { .. } | RasterRenderer::Colormap => Resampling::Nearest,
        _ => style.resampling,
    };
    // RGBA images carry their transparency in band 4.
    let alpha_band = match (&style.renderer, &r.color) {
        (RasterRenderer::Rgb { .. }, ColorHint::Rgb { alpha: true }) if r.band_count() >= 4 => Some(3),
        (RasterRenderer::Stretch { band: 0, .. } | RasterRenderer::ShadedRelief { band: 0, .. }, ColorHint::GrayAlpha) => Some(1),
        _ => None,
    };
    let pass1 = Pass1 {
        r,
        view,
        place,
        k,
        rotated: rotated.then_some((centre, sin, cos)),
        x0,
        y0,
        bw,
        grid: &grid,
        gw,
        // Half a turn of a geographic grid's columns.
        seam: if r.transformer().is_geographic() { 180.0 / r.transform.pixel_width.abs() } else { f64::INFINITY },
        wrap_width: full_turn_width(r),
        bands: &bands,
        data: bands.iter().map(|b| r.bands.get(*b).map(|b| b.data.as_slice())).collect(),
        relief,
        resampling,
        alpha_band,
        alpha_max: rustgis_data::raster::alpha_max(&r.data_type),
        cell: r.cell_size_m(),
        mask: &style.mask,
    };
    let mut vals = vec![f32::NAN; bw * bh * nb];
    let mut shade = if relief.is_some() { vec![0u8; bw * bh] } else { Vec::new() };
    let mut alpha = if alpha_band.is_some() { vec![0u8; bw * bh] } else { Vec::new() };
    let threads = threads.clamp(1, bh);
    let rows = bh.div_ceil(threads.max(1));
    each_chunk(
        threads,
        vals.chunks_mut(rows * bw * nb).zip(chunks_or_empty(&mut shade, rows * bw)).zip(chunks_or_empty(&mut alpha, rows * bw)).enumerate(),
        |(i, ((v, sh), al))| pass1.rows(y0 + i * rows, v, sh, al),
    );

    // Dynamic range adjustment: statistics of the values in view.
    let view_stats: Option<Vec<BandStats>> =
        style.dra.then(|| (0..nb).map(|k| BandStats::from_values(&vals.iter().skip(k).step_by(nb).copied().collect::<Vec<_>>())).collect());
    let colorizer = Colorizer::new(style, r, view_stats.as_deref().or(stats));

    // Pass 2: colours, premultiplied, into a pixmap the size of the raster's screen box.
    let Some(mut lay) = Pixmap::new(bw as u32, bh as u32) else { return };
    let (vals, shade, alpha) = (&vals, &shade, &alpha);
    each_chunk(threads, lay.data_mut().chunks_mut(rows * bw * 4).enumerate(), |(ci, out)| {
        let first = ci * rows * bw;
        // Neighbouring pixels often share their values (zoomed in, or classes): colour once.
        let mut last: Option<(&[f32], Option<u8>, Option<u8>, Option<[u8; 4]>)> = None;
        for (j, px) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let i = first + j;
            let Some(v) = vals.get(i * nb..(i + 1) * nb) else { continue };
            let (sh, al) = (shade.get(i).copied(), alpha.get(i).copied());
            let c = match last {
                Some((lv, ls, la, c)) if ls == sh && la == al && same(lv, v) => c,
                _ => {
                    let c = colorizer.color(v, sh).map(|mut c| {
                        if let Some(a) = al {
                            c[3] = ((c[3] as u16 * a as u16 + 127) / 255) as u8;
                        }
                        let a = c[3] as u16;
                        let pm = |x: u8| ((x as u16 * a + 127) / 255) as u8;
                        [pm(c[0]), pm(c[1]), pm(c[2]), c[3]]
                    });
                    last = Some((v, sh, al, c));
                    c
                }
            };
            if let Some(c) = c {
                px.copy_from_slice(&c);
            }
        }
    });
    // Drawn at its own size and fully opaque: a plain copy over, much cheaper than the
    // rasteriser's general image drawing.
    if k == 1.0 && opacity >= 1.0 {
        crate::blend_over(pm, &lay, x0 as i64, y0 as i64);
        return;
    }
    let quality = if style.resampling == Resampling::Bilinear { FilterQuality::Bilinear } else { FilterQuality::Nearest };
    let at = Transform::from_row(k as f32, 0.0, 0.0, k as f32, (x0 as f64 * k) as f32, (y0 as f64 * k) as f32);
    pm.draw_pixmap(0, 0, lay.as_ref(), &PixmapPaint { opacity, quality, ..PixmapPaint::default() }, at, None);
}

/// What pass 1 of [`draw`] reads: where each screen pixel falls in the raster, and how to
/// sample it there.
struct Pass1<'a> {
    r: &'a Raster,
    view: &'a View,
    place: Place<'a>,
    k: f64,
    /// A rotated raster's centre on screen and the sine and cosine of its back-rotation.
    rotated: Option<((f64, f64), f64, f64)>,
    x0: usize,
    y0: usize,
    bw: usize,
    /// Exact source positions every `STEP` pixels, `gw` to a row.
    grid: &'a [Option<(f64, f64)>],
    gw: usize,
    /// Columns between neighbouring grid points further apart than this straddle a geographic
    /// grid's jump at the antimeridian (or the map's edge): not interpolated across.
    seam: f64,
    /// Cyclic neighbours only for a geographic grid spanning exactly one turn.
    wrap_width: Option<i64>,
    bands: &'a [usize],
    /// The renderer's bands' values.
    data: Vec<Option<&'a [f32]>>,
    relief: Option<(f64, f64, f64)>,
    resampling: Resampling,
    alpha_band: Option<usize>,
    /// The alpha band's opaque value (its pixel type's: 65535 for 16 bits).
    alpha_max: f32,
    cell: (f64, f64),
    /// Values drawn as no data.
    mask: &'a RasterMask,
}

impl Pass1<'_> {
    /// Fills the band values (and hillshade and alpha, when kept) of the screen rows from `py0`
    /// on, as many as `vals` holds.
    fn rows(&self, py0: usize, vals: &mut [f32], shade: &mut [u8], alpha: &mut [u8]) {
        let (r, bw, nb) = (self.r, self.bw, self.bands.len().max(1));
        let lerp = |p: f64, q: f64, t: f64| p + (q - p) * t;
        // Each grid column's source position at this row, interpolated down the grid.
        let mut cols: Vec<Option<(f64, f64)>> = vec![None; self.gw];
        for (dy, row_vals) in vals.chunks_mut(bw * nb).enumerate() {
            let py = py0 + dy;
            let v = (py - self.y0) as f64 + 0.5;
            let gy = (v / STEP as f64) as usize;
            let fy = v / STEP as f64 - gy as f64;
            let g = |x: usize, y: usize| self.grid.get(y * self.gw + x).copied().flatten();
            for (gx, c) in cols.iter_mut().enumerate() {
                *c = match (g(gx, gy), g(gx, gy + 1)) {
                    (Some(a), Some(b)) if (a.0 - b.0).abs() <= self.seam => Some((lerp(a.0, b.0, fy), lerp(a.1, b.1, fy))),
                    _ => None,
                };
            }
            // Nearest sampling: the last cell read, and the pixel it was read for.
            let mut last: Option<((i64, i64), usize)> = None;
            for dx in 0..bw {
                let px = self.x0 + dx;
                let pos = match self.rotated {
                    // Back-rotate the screen pixel about the centre, then find its source exactly.
                    Some((centre, sin, cos)) => {
                        let (ddx, ddy) = (px as f64 + 0.5 - centre.0, py as f64 + 0.5 - centre.1);
                        source(r, self.view, self.place, self.k, centre.0 + ddx * cos - ddy * sin, centre.1 + ddx * sin + ddy * cos)
                    }
                    None => {
                        let u = dx as f64 + 0.5;
                        let gx = (u / STEP as f64) as usize;
                        let fx = u / STEP as f64 - gx as f64;
                        match (cols.get(gx).copied().flatten(), cols.get(gx + 1).copied().flatten()) {
                            (Some(a), Some(b)) if (a.0 - b.0).abs() <= self.seam => Some((lerp(a.0, b.0, fx), lerp(a.1, b.1, fx))),
                            // Near the edge of the projection's valid area, or across a seam:
                            // transform this pixel exactly.
                            _ => source(r, self.view, self.place, self.k, px as f64 + 0.5, py as f64 + 0.5),
                        }
                    }
                };
                let Some((c, row)) = pos else { continue };
                // Wrapped here, not in `source`: the grid's columns run on across a 0..360° seam.
                let c = r.wrap_col(c);
                if c < 0.0 || row < 0.0 || c >= r.width as f64 || row >= r.height as f64 {
                    continue;
                }
                let i = dy * bw + dx;
                // Truncating is flooring here (both are at least 0), and much cheaper.
                let cell = (c as i64, row as i64);
                // The same cell as the last pixel, sampled nearest: the same values.
                if self.resampling == Resampling::Nearest && self.relief.is_none() {
                    if let Some((lc, ldx)) = last
                        && lc == cell
                    {
                        if let Some(&la) = alpha.get(dy * bw + ldx)
                            && let Some(a) = alpha.get_mut(i)
                        {
                            *a = la;
                        }
                        row_vals.copy_within(ldx * nb..(ldx + 1) * nb, dx * nb);
                        continue;
                    }
                    last = Some((cell, dx));
                }
                if let (Some(ab), Some(a)) = (self.alpha_band, alpha.get_mut(i)) {
                    *a = r.value(ab, cell.0, cell.1).map(|v| ((v / self.alpha_max).clamp(0.0, 1.0) * 255.0).round() as u8).unwrap_or(0);
                    if *a == 0 {
                        continue;
                    }
                }
                if self.resampling == Resampling::Nearest {
                    // The cell is in the grid (checked above): read each band at one index.
                    let at = cell.1 as usize * r.width as usize + cell.0 as usize;
                    for (bi, data) in self.data.iter().enumerate() {
                        if let (Some(slot), Some(v)) = (row_vals.get_mut(dx * nb + bi), data.and_then(|d| d.get(at))) {
                            *slot = if r.is_valid(*v) && !self.mask.hides(*v) { *v } else { f32::NAN };
                        }
                    }
                } else {
                    for (bi, b) in self.bands.iter().enumerate() {
                        if let Some(slot) = row_vals.get_mut(dx * nb + bi) {
                            *slot = sample(r, self.mask, *b, c, row, self.resampling, self.wrap_width);
                        }
                    }
                }
                if let (Some((az, alt, zf)), Some(b)) = (self.relief, self.bands.first()) {
                    let (ci, ri) = cell;
                    let centre = value(r, self.mask, *b, ci, ri, self.wrap_width).unwrap_or(0.0) as f64;
                    let mut z = [centre; 9];
                    for (k, (dx, dy)) in
                        [(-1, -1), (0, -1), (1, -1), (-1, 0), (0, 0), (1, 0), (-1, 1), (0, 1), (1, 1)].into_iter().enumerate()
                    {
                        z[k] = value(r, self.mask, *b, ci + dx, ri + dy, self.wrap_width).map(f64::from).unwrap_or(centre);
                    }
                    if let Some(s) = shade.get_mut(i) {
                        *s = hillshade(&z, self.cell.0, self.cell.1, az, alt, zf);
                    }
                }
            }
        }
    }
}

/// Whether two pixels' band values are the same (NaN, no data, equal to itself).
fn same(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// `v` in chunks of `n`, or endless empty chunks when it is empty (a plane not kept).
fn chunks_or_empty(v: &mut [u8], n: usize) -> Box<dyn Iterator<Item = &mut [u8]> + Send + '_> {
    if v.is_empty() { Box::new(std::iter::repeat_with(|| &mut [][..])) } else { Box::new(v.chunks_mut(n.max(1))) }
}

/// How many threads to draw `pixels` with: one for a small image, and always one in the browser.
fn threads_for(pixels: usize) -> usize {
    if cfg!(target_arch = "wasm32") || pixels < 64 * 1024 {
        return 1;
    }
    std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8)
}

/// Runs `f` on each item, spread over `threads` threads (in turn on this one when it is one).
fn each_chunk<T: Send>(threads: usize, items: impl Iterator<Item = T>, f: impl Fn(T) + Sync) {
    if threads <= 1 {
        items.for_each(f);
        return;
    }
    let f = &f;
    std::thread::scope(|scope| {
        for item in items {
            scope.spawn(move || f(item));
        }
    });
}

#[cfg(test)]
mod tests {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform};
    use rustgis_geom::lonlat_to_merc;
    use rustgis_map::Camera;
    use rustgis_map::raster::RasterStyle;

    use super::*;

    /// A raster turned by a hair either side of none isn't turned: it is drawn at the view's
    /// resolution, not from its overview.
    #[test]
    fn a_hair_of_rotation_either_way_isnt_a_turn() {
        for r in [0.0, 1e-7, -1e-7, 360.0, -360.0, 720.0 + 1e-7, f64::NAN] {
            assert!(!turned(r), "{r}");
        }
        for r in [1.0, -1.0, 359.0, 180.0, 1e-5, -1e-5] {
            assert!(turned(r), "{r}");
        }
    }

    fn sampling_grid(pixel_width: f64, crs: Crs, nodata: Option<f64>) -> Raster {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 90.0, pixel_width, pixel_height: -60.0 };
        let data = [0.0, 10.0, 20.0, 30.0].repeat(3);
        Raster::new("sampling", 4, 3, vec![data], gt, crs, nodata, ColorHint::Gray, "Float32").unwrap()
    }

    #[test]
    fn bilinear_samples_blend_across_both_ends_of_a_full_turn() {
        for crs in [Crs::LonLat, Crs::from_epsg(4269).unwrap()] {
            for pixel_width in [90.0, -90.0] {
                let r = sampling_grid(pixel_width, crs, None);
                let mask = RasterMask::default();
                let wrap_width = full_turn_width(&r);
                for (col, expected) in [(0.0, 15.0), (0.25, 7.5), (3.75, 22.5)] {
                    assert_eq!(sample(&r, &mask, 0, col, 1.5, Resampling::Bilinear, wrap_width), expected);
                }
                assert_eq!(sample(&r, &mask, 0, 0.25, 1.5, Resampling::Nearest, wrap_width), 0.0);
                // Rows do not wrap: the original nearest fallback still applies.
                assert_eq!(sample(&r, &mask, 0, 0.25, 0.25, Resampling::Bilinear, wrap_width), 0.0);
                assert!(sample(&r, &mask, 0, 0.25, -0.25, Resampling::Bilinear, wrap_width).is_nan());
            }
        }
    }

    #[test]
    fn bilinear_neighbours_keep_regional_projected_and_nodata_edges() {
        let mask = RasterMask::default();
        // Neither a partial column short of a turn nor an overlapping grid is cyclic.
        for (pixel_width, crs) in [(10.0, Crs::LonLat), (89.9, Crs::LonLat), (90.1, Crs::LonLat), (90.0, Crs::WebMercator)] {
            let r = sampling_grid(pixel_width, crs, None);
            let wrap_width = full_turn_width(&r);
            for (col, expected) in [(0.25, 0.0), (3.75, 30.0)] {
                assert_eq!(sample(&r, &mask, 0, col, 1.5, Resampling::Bilinear, wrap_width), expected);
            }
        }
        let r = sampling_grid(90.0, Crs::LonLat, None);
        let masked = RasterMask { values: vec![30.0], ..RasterMask::default() };
        assert_eq!(sample(&r, &masked, 0, 0.25, 1.5, Resampling::Bilinear, full_turn_width(&r)), 0.0);
        assert!(sample(&r, &masked, 0, 3.75, 1.5, Resampling::Bilinear, full_turn_width(&r)).is_nan());
        let nodata = sampling_grid(90.0, Crs::LonLat, Some(30.0));
        assert_eq!(sample(&nodata, &mask, 0, 0.25, 1.5, Resampling::Bilinear, full_turn_width(&nodata)), 0.0);
        assert!(sample(&nodata, &mask, 0, 3.75, 1.5, Resampling::Bilinear, full_turn_width(&nodata)).is_nan());
    }

    #[test]
    fn regional_relief_keeps_the_centre_fallback_at_its_edge() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 90.0, pixel_width: 10.0, pixel_height: -10.0 };
        let data = [0.0, 1_000_000.0, 2_000_000.0, 3_000_000.0].repeat(18);
        let r = Raster::new("regional relief", 4, 18, vec![data], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let mut style = RasterStyle::default_for(&r);
        style.renderer = RasterRenderer::ShadedRelief { band: 0, ramp: None, azimuth: 270.0, altitude: 45.0, z_factor: 1.0 };
        style.resampling = Resampling::Nearest;
        let view = View {
            width: 1.0,
            height: 1.0,
            pixels_per_point: 1.0,
            camera: Camera { center: lonlat_to_merc(Coord::new(5.0, 0.0)), res: 1000.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let mut pm = Pixmap::new(1, 1).unwrap();
        draw(&mut pm, &r, &style, None, &view, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"));
        let z = [0.0, 0.0, 1_000_000.0, 0.0, 0.0, 1_000_000.0, 0.0, 0.0, 1_000_000.0];
        let (cell_x, cell_y) = r.cell_size_m();
        let shade = hillshade(&z, cell_x, cell_y, 270.0, 45.0, 1.0);
        let expected = Colorizer::new(&style, &r, None).color(&[0.0], Some(shade)).unwrap();
        let actual = pm.pixel(0, 0).unwrap();
        assert_eq!([actual.red(), actual.green(), actual.blue(), actual.alpha()], expected);
    }

    /// A 0–360° grid seen across its seam: the part in view is two strips, one at each end of
    /// the grid, not the whole width.
    #[test]
    fn a_view_across_the_seam_of_a_full_turn_grid_is_two_strips() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 90.0, pixel_width: 10.0, pixel_height: -10.0 };
        let r = Raster::new("turn", 36, 18, vec![vec![1.0; 36 * 18]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        // About 90° to each side of 0°: the east strip from column 0, the west one to column 36.
        let view = View {
            width: 400.0,
            height: 200.0,
            pixels_per_point: 1.0,
            camera: Camera { center: Coord::new(0.0, 0.0), res: 50_000.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let w = in_view(&r, &view, Place::Flat(&Flat::Mercator, "mercator"), 400, 200, true).unwrap();
        assert!(w.cols.0 == 0.0 && w.cols.1 > 5.0 && w.cols.1 < 18.0, "{:?}", w.cols);
        let (a, b) = w.cols2.unwrap();
        assert!(a > 18.0 && a < 31.0 && b == 36.0, "{:?}", (a, b));
        // East of 0° only: one strip.
        let east =
            View { camera: Camera { center: lonlat_to_merc(Coord::new(100.0, 0.0)), res: 50_000.0, bearing: 0.0, pitch: 0.0 }, ..view };
        let w = in_view(&r, &east, Place::Flat(&Flat::Mercator, "mercator"), 400, 200, true).unwrap();
        assert!(w.cols2.is_none() && w.cols.0 > 0.0, "{:?}", (w.cols, w.cols2));
    }

    #[test]
    fn shaded_relief_is_unchanged_when_the_geographic_seam_moves() {
        let elevations: Vec<f32> = (0..36).map(|c| (((c as f64 + 0.5) * 10.0).to_radians().sin() * 1_000_000.0) as f32).collect();
        let view = View {
            width: 80.0,
            height: 40.0,
            pixels_per_point: 1.0,
            camera: Camera { center: Coord::new(0.0, 0.0), res: 50_000.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let render = |origin_x, shift: usize| {
            let gt = GeoTransform { origin_x, origin_y: 90.0, pixel_width: 10.0, pixel_height: -10.0 };
            let data: Vec<f32> = (0..36 * 18).map(|i| elevations[(i % 36 + shift) % 36]).collect();
            let r = Raster::new("relief", 36, 18, vec![data], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
            let mut style = RasterStyle::default_for(&r);
            style.renderer = RasterRenderer::ShadedRelief { band: 0, ramp: None, azimuth: 270.0, altitude: 45.0, z_factor: 1.0 };
            style.resampling = Resampling::Nearest;
            let mut pm = Pixmap::new(80, 40).unwrap();
            draw(&mut pm, &r, &style, None, &view, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"));
            pm
        };
        let seam = render(0.0, 0);
        let interior = render(-180.0, 18);
        assert!(seam.pixels().iter().all(|p| p.alpha() == 255));
        assert_eq!(seam.data(), interior.data(), "the first and last columns have the same slopes as interior columns");
    }

    /// #348: masked values draw as no data, are never blended into their neighbours, and leave
    /// the stretch to the values shown.
    #[test]
    fn masked_values_draw_as_no_data() {
        // 100 × 100 cells of 1 km in UTM 17N around 81°W, 42°N: a -9999 background the file
        // doesn't flag in the west half, 49 down to 0 by column in the east.
        let gt = GeoTransform { origin_x: 450_000.0, origin_y: 4_700_000.0, pixel_width: 1000.0, pixel_height: -1000.0 };
        let data: Vec<f32> = (0..10_000).map(|i| if i % 100 < 50 { -9999.0 } else { (99 - i % 100) as f32 }).collect();
        let r = Raster::new("utm", 100, 100, vec![data], gt, Crs::from_epsg(32617).unwrap(), None, ColorHint::Gray, "Float32").unwrap();
        let camera = Camera { center: lonlat_to_merc(Coord::new(-81.0, 42.0)), res: 250.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let flat = Place::Flat(&Flat::Mercator, "mercator");
        let mut style = RasterStyle::default_for(&r);
        style.resampling = Resampling::Bilinear;
        let draw_it = |style: &RasterStyle, stats: Option<&[BandStats]>| {
            let mut pm = Pixmap::new(200, 200).unwrap();
            draw(&mut pm, &r, style, stats, &view, 1.0, false, flat);
            pm
        };
        let px = |pm: &Pixmap, x: u32| pm.pixel(x, 100).map(|p| (p.alpha(), p.red())).unwrap_or((0, 0));
        let plain = draw_it(&style, None);
        assert_eq!(px(&plain, 70).0, 255, "unmasked, the background is drawn");

        style.mask = RasterMask { values: vec![-9999.0], ..RasterMask::default() };
        let stats = [style.mask.stats(&r, 0)];
        assert!(stats[0].min == 0.0 && stats[0].max == 49.0, "{:?}", stats[0]);
        let masked = draw_it(&style, Some(&stats));
        assert_eq!(px(&masked, 70).0, 0, "the background is hidden");
        // Across the edge: drawn or hidden, never a blend of -9999 and the 49 beside it, which the
        // stretch (0–49, light to dark) would show black.
        let first = (100..200).find(|x| px(&masked, *x).0 > 0).unwrap();
        assert!(px(&masked, first).1 > 200, "the edge is the light high end: {:?}", px(&masked, first));
        // Stretched over the values shown, 0–49, not with the file's statistics (-9999–49).
        let spread = |pm: &Pixmap| px(pm, first).1.abs_diff(px(pm, 199).1);
        let file_stats = draw_it(&style, None);
        assert!(spread(&masked) > spread(&file_stats) + 40, "{} vs {}", spread(&masked), spread(&file_stats));

        // A range: values outside 35–45 are hidden; with `inside`, those within.
        style.mask = RasterMask { min: Some(35.0), max: Some(45.0), ..RasterMask::default() };
        let outside = draw_it(&style, None);
        style.mask.inside = true;
        let inside = draw_it(&style, None);
        let shown = |pm: &Pixmap| (0..200).filter(|x| px(pm, *x).0 > 0).count();
        assert!(shown(&outside) > 0 && shown(&inside) > 0);
        assert!(shown(&outside) < shown(&plain) && shown(&inside) < shown(&plain));
        for x in 0..200 {
            assert!(!(px(&outside, x).0 > 0 && px(&inside, x).0 > 0), "column {x} is in and out");
        }
    }

    #[test]
    fn draws_a_utm_raster_where_it_belongs() {
        // 100 × 100 cells of 1 km in UTM 17N around 81°W, 42°N, valued by column.
        let gt = GeoTransform { origin_x: 450_000.0, origin_y: 4_700_000.0, pixel_width: 1000.0, pixel_height: -1000.0 };
        let data: Vec<f32> = (0..10_000).map(|i| (i % 100) as f32).collect();
        let r = Raster::new("utm", 100, 100, vec![data], gt, Crs::from_epsg(32617).unwrap(), None, ColorHint::Gray, "Float32").unwrap();
        let style = RasterStyle::default_for(&r);
        let camera = Camera { center: lonlat_to_merc(Coord::new(-81.0, 42.0)), res: 2000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 200.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let mut pm = Pixmap::new(200, 200).unwrap();
        draw(&mut pm, &r, &style, None, &view, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"));
        let alpha = |x: u32, y: u32| pm.pixel(x, y).map(|p| p.alpha()).unwrap_or(0);
        assert_eq!(alpha(100, 100), 255, "the centre is covered");
        assert_eq!(alpha(2, 2), 0, "the corner of the view is outside the raster");
        // West is low (dark), east high (light).
        let lum = |x: u32| pm.pixel(x, 100).map(|p| p.red()).unwrap_or(0);
        assert!(lum(70) < lum(130), "{} vs {}", lum(70), lum(130));
        // Every renderer draws without panicking, at odd view sizes too.
        for kind in ["rgb", "classify", "unique", "shadedRelief"] {
            let mut s = style.clone();
            s.renderer = match kind {
                "rgb" => RasterRenderer::Rgb { red: 0, green: 0, blue: 0 },
                "classify" => rustgis_map::raster::classify(&r, 0, 5, Default::default(), Default::default()),
                "unique" => rustgis_map::raster::unique_values(&r, 0, Default::default()),
                _ => RasterRenderer::ShadedRelief { band: 0, ramp: None, azimuth: 315.0, altitude: 45.0, z_factor: 1.0 },
            };
            s.dra = true;
            s.resampling = Resampling::Bilinear;
            draw(&mut pm, &r, &s, None, &view, 0.5, false, Place::Flat(&Flat::Mercator, "mercator"));
        }
        // High-density screens draw at logical resolution, scaled up.
        let hi = View { pixels_per_point: 2.0, ..view };
        let mut pm2 = Pixmap::new(400, 400).unwrap();
        draw(&mut pm2, &r, &style, None, &hi, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"));
        assert_eq!(pm2.pixel(200, 200).map(|p| p.alpha()), Some(255));
        assert_eq!(pm2.pixel(4, 4).map(|p| p.alpha()), Some(0));
        // Spread over threads, every pixel comes out as drawn on one, for each renderer.
        let wide = View { width: 420.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
        for kind in ["stretch", "rgb", "shadedRelief"] {
            let mut s = style.clone();
            s.renderer = match kind {
                "rgb" => RasterRenderer::Rgb { red: 0, green: 0, blue: 0 },
                "shadedRelief" => RasterRenderer::ShadedRelief { band: 0, ramp: None, azimuth: 315.0, altitude: 45.0, z_factor: 1.0 },
                _ => s.renderer,
            };
            let mut one = Pixmap::new(420, 300).unwrap();
            let mut many = Pixmap::new(420, 300).unwrap();
            draw_with(&mut one, &r, &s, None, &wide, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"), 1);
            draw_with(&mut many, &r, &s, None, &wide, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"), 7);
            assert!(one.data() == many.data(), "{kind}");
            assert!(one.pixels().iter().any(|p| p.alpha() == 255), "{kind} draws");
        }
        let far = View { width: 3.0, height: 1.0, pixels_per_point: 2.0, camera: Camera::world(), sky: false };
        let mut tiny = Pixmap::new(6, 2).unwrap();
        draw(&mut tiny, &r, &style, None, &far, 1.0, true, Place::Flat(&Flat::Mercator, "mercator"));
    }

    /// #1109: a lon/lat grid stored from 0 to 360° draws west of Greenwich too, from its columns
    /// past 180°, with no smear where the grid's last column meets its first.
    #[test]
    fn draws_a_0_to_360_grid_on_both_sides_of_greenwich() {
        // 36 × 18 cells of 10°, valued by column: 0 at 0–10°E, 35 at 350–360° (10–0°W).
        let gt = GeoTransform { origin_x: 0.0, origin_y: 90.0, pixel_width: 10.0, pixel_height: -10.0 };
        let data: Vec<f32> = (0..36 * 18).map(|i| (i % 36) as f32).collect();
        let r = Raster::new("world", 36, 18, vec![data], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let style = RasterStyle::default_for(&r);
        let camera = Camera { center: Coord::new(0.0, 0.0), res: 100_000.0, bearing: 0.0, pitch: 0.0 };
        let view = View { width: 400.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
        let mut pm = Pixmap::new(400, 200).unwrap();
        draw(&mut pm, &r, &style, None, &view, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"));
        let at = |lon: f64| {
            let (x, y) = view.to_screen(lonlat_to_merc(Coord::new(lon, 10.0)));
            pm.pixel(x as u32, y as u32).map(|p| (p.alpha(), p.red())).unwrap_or((0, 0))
        };
        for lon in [-175.0, -100.0, -5.0, 5.0, 80.0, 175.0] {
            assert_eq!(at(lon).0, 255, "{lon}° is drawn");
        }
        // 100°W is column 26 (260°), lighter than column 8 (80°E); 5°W is the last column, 5°E the first.
        assert!(at(-100.0).1 > at(80.0).1, "{:?} vs {:?}", at(-100.0), at(80.0));
        assert!(at(-5.0).1 > at(-100.0).1 && at(5.0).1 < at(80.0).1, "{:?} {:?}", at(-5.0), at(5.0));
    }

    /// A lon/lat grid on a map centred on the Pacific: across 180°, where the source longitude
    /// jumps from one end of the grid to the other, no strip of values from elsewhere shows.
    #[test]
    fn no_smear_where_the_longitudes_jump() {
        for x0 in [0.0, -180.0] {
            let gt = GeoTransform { origin_x: x0, origin_y: 90.0, pixel_width: 10.0, pixel_height: -10.0 };
            // Valued by longitude: 17 at 170–180°, 18 at 180–190° (-180 to -170°).
            let data: Vec<f32> = (0..36 * 18).map(|i| ((x0 / 10.0) as i32 + i % 36).rem_euclid(36) as f32).collect();
            let r = Raster::new("world", 36, 18, vec![data], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
            let style = RasterStyle::default_for(&r);
            let flat = Flat::Custom(std::sync::Arc::new(rustgis_map::projection::Projector::new("robinson +lon_0=150").unwrap()));
            let camera = Camera { center: Coord::new(0.0, 0.0), res: 100_000.0, bearing: 0.0, pitch: 0.0 };
            let view = View { width: 400.0, height: 200.0, pixels_per_point: 1.0, camera, sky: false };
            let mut pm = Pixmap::new(400, 200).unwrap();
            draw(&mut pm, &r, &style, None, &view, 1.0, false, Place::Flat(&flat, "robinson +lon_0=150"));
            let screen = |lon: f64| view.to_screen(flat.forward(Coord::new(lon, 10.0)).unwrap());
            let red = |x: f64, y: f64| pm.pixel(x as u32, y as u32).map(|p| (p.alpha(), p.red())).unwrap_or((0, 0));
            let ((xa, y), (xb, _)) = (screen(170.5), screen(-170.5));
            let (lo, hi) = (red(xa, y).1.min(red(xb, y).1), red(xa, y).1.max(red(xb, y).1));
            assert!(xa < xb && hi > lo, "{x0}: {xa} {xb} {lo} {hi}");
            for x in xa as u32..=xb as u32 {
                let (a, v) = red(f64::from(x), y);
                assert!(a == 255 && (lo..=hi).contains(&v), "{x0}: {v} at x {x}, outside {lo}–{hi}");
            }
        }
    }
}
