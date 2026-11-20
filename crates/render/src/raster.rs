//! Drawing raster layers.
//!
//! Each screen pixel is traced back to the raster's grid: Web Mercator → the raster's own
//! coordinate system → pixel position. The full transformation runs on a coarse grid of screen
//! points and is interpolated in between, which is exact for Web Mercator and lon/lat rasters
//! and well under a pixel off for projected ones. Values are then sampled (nearest or bilinear),
//! optionally re-stretched from the pixels in view (DRA), and coloured.

use rustgis_data::raster::{BandStats, ColorHint, Raster};
use rustgis_geom::{Coord, Extent, extent_to_merc, lonlat_to_merc};
use rustgis_map::Globe;
use rustgis_map::projection::Flat;
use rustgis_map::raster::{Colorizer, RasterMask, RasterRenderer, RasterStyle, Resampling, Rock, hillshade, slope};
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

use crate::View;
use crate::terrain::Water;

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

/// The grids a Shaded Relief layer is lit with, worked out off the frame, one value per cell: the
/// light its cast shadows leave (see [`rustgis_map::raster::RasterLayer::cast_shadows`]) and the
/// share of the sky it sees ([`rustgis_map::raster::RasterLayer::sky_view`]); and the terrain's
/// water (#1915), whose cells are drawn at their level with the even light of flat ground.
#[derive(Clone, Copy, Default)]
pub(crate) struct Lights<'a> {
    pub shadow: Option<&'a [u8]>,
    pub sky: Option<&'a [u8]>,
    pub water: Option<&'a Water>,
}

/// Draws a raster layer onto `pm` at `opacity`. `stats` are the renderer bands' statistics
/// without the masked pixels, when the style has a mask. `light` is what it is lit with, for
/// Shaded Relief with shadows or sky.
pub(crate) fn draw(
    pm: &mut Pixmap,
    r: &Raster,
    style: &RasterStyle,
    stats: Option<&[BandStats]>,
    view: &View,
    opacity: f32,
    full_res: bool,
    place: Place,
    light: Lights,
) {
    let threads = threads_for(pm.width() as usize * pm.height() as usize);
    draw_with(pm, r, style, stats, view, opacity, full_res, place, threads, light);
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
/// map shows what it has, an export refuses it. `light` is as [`draw`] takes it.
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
    light: Lights,
) -> Result<bool, String> {
    let bands = style.renderer.bands();
    let own: Vec<BandStats> = match stats {
        Some(_) => Vec::new(),
        None => bands.iter().map(|b| r.bands.get(*b).map(|b| b.stats.clone()).unwrap_or_default()).collect(),
    };
    let stats = stats.unwrap_or(&own);
    // Its cast shadows are only worked out when the overview is the whole image (see
    // `RasterLayer::cast_shadows`), so only it is drawn with them.
    let whole = |pm: &mut Pixmap| draw(pm, r, style, Some(stats), view, opacity, full_res, place, light);
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
                draw(pm, part, &own_style, Some(stats), view, 1.0, full_res, place, Lights::default());
            }
        } else if let Some(mut both) = Pixmap::new(pm.width(), pm.height()) {
            draw(&mut both, r, style, Some(stats), view, 1.0, full_res, place, Lights::default());
            for part in &parts {
                draw(&mut both, part, &own_style, Some(stats), view, 1.0, full_res, place, Lights::default());
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
            draw(pm, part, &own_style, Some(stats), view, opacity, full_res, place, Lights::default());
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
    light: Lights,
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

    // Exact source positions on a coarse grid, its rows spread over the threads (a projected
    // raster's transformation is most of a frame's work on one, #1792): each position is worked
    // out alone, so the grid is the same on any number.
    let (gw, gh) = (bw / STEP + 2, bh / STEP + 2);
    let mut grid = vec![None; gw * gh];
    // (A few points aren't worth a thread.)
    let grid_threads = if gw * gh < 1024 { 1 } else { threads.clamp(1, gh) };
    let grid_rows = gh.div_ceil(grid_threads);
    each_chunk(grid_threads, grid.chunks_mut(grid_rows * gw).enumerate(), |(i, chunk)| {
        for (j, cell) in chunk.iter_mut().enumerate() {
            let (gx, gy) = (j % gw, i * grid_rows + j / gw);
            *cell = source(r, view, place, k, (x0 + gx * STEP) as f64, (y0 + gy * STEP) as f64);
        }
    });

    // Pass 1: band values (and hillshade) for each screen pixel.
    let bands = style.renderer.bands();
    let nb = bands.len().max(1);
    let relief = match style.renderer {
        RasterRenderer::ShadedRelief { azimuth, altitude, z_factor, .. } => Some((azimuth, altitude, z_factor)),
        _ => None,
    };
    // Cast shadows darken the direct light by their strength; the sky a cell sees, the ambient.
    let cells = r.width as usize * r.height as usize;
    let (shadow, sky) = match style.renderer {
        RasterRenderer::ShadedRelief { shadow, sky, .. } => (lit_by(light.shadow, shadow, cells), lit_by(light.sky, sky, cells)),
        _ => (None, None),
    };
    // The fill light's strength (0 to 1); 0 has none.
    let fill = match style.renderer {
        RasterRenderer::ShadedRelief { fill, .. } if fill > 0.0 => fill.min(1.0),
        _ => 0.0,
    };
    // Steep slopes tinted toward rock, before the light.
    let rock = match style.renderer {
        RasterRenderer::ShadedRelief { rock, .. } => rock,
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
        shadow,
        sky,
        fill,
        rock,
        water: light.water.filter(|_| relief.is_some()).map(|w| WaterCells::new(w, r, &grid)),
        resampling,
        alpha_band,
        alpha_max: rustgis_data::raster::alpha_max(&r.data_type),
        cell: r.cell_size_m(),
        east_north: (if r.transform.pixel_width < 0.0 { -1 } else { 1 }, if r.transform.pixel_height > 0.0 { -1 } else { 1 }),
        mask: &style.mask,
    };
    let mut vals = vec![f32::NAN; bw * bh * nb];
    let mut shade = if relief.is_some() { vec![0u8; bw * bh] } else { Vec::new() };
    let mut alpha = if alpha_band.is_some() { vec![0u8; bw * bh] } else { Vec::new() };
    let mut ambient = if sky.is_some() { vec![255u8; bw * bh] } else { Vec::new() };
    let mut filled = if relief.is_some() && fill > 0.0 { vec![0u8; bw * bh] } else { Vec::new() };
    let mut rocky = if relief.is_some() && rock.is_some() { vec![0u8; bw * bh] } else { Vec::new() };
    let threads = threads.clamp(1, bh);
    let rows = bh.div_ceil(threads.max(1));
    each_chunk(
        threads,
        vals.chunks_mut(rows * bw * nb)
            .zip(chunks_or_empty(&mut shade, rows * bw))
            .zip(chunks_or_empty(&mut alpha, rows * bw))
            .zip(chunks_or_empty(&mut ambient, rows * bw))
            .zip(chunks_or_empty(&mut filled, rows * bw))
            .zip(chunks_or_empty(&mut rocky, rows * bw))
            .enumerate(),
        |(i, (((((v, sh), al), am), fl), ro))| pass1.rows(y0 + i * rows, v, sh, al, am, fl, ro),
    );

    // Dynamic range adjustment: statistics of the values in view.
    let view_stats: Option<Vec<BandStats>> =
        style.dra.then(|| (0..nb).map(|k| BandStats::from_values(&vals.iter().skip(k).step_by(nb).copied().collect::<Vec<_>>())).collect());
    let colorizer = Colorizer::new(style, r, view_stats.as_deref().or(stats));

    // Pass 2: colours, premultiplied, into a pixmap the size of the raster's screen box.
    let Some(mut lay) = Pixmap::new(bw as u32, bh as u32) else { return };
    let (vals, shade, alpha, ambient, filled, rocky) = (&vals, &shade, &alpha, &ambient, &filled, &rocky);
    each_chunk(threads, lay.data_mut().chunks_mut(rows * bw * 4).enumerate(), |(ci, out)| {
        let first = ci * rows * bw;
        // Neighbouring pixels often share their values (zoomed in, or classes): colour once.
        let mut last: Option<(&[f32], [Option<u8>; 5], Option<[u8; 4]>)> = None;
        for (j, px) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let i = first + j;
            let Some(v) = vals.get(i * nb..(i + 1) * nb) else { continue };
            let (sh, al, am) = (shade.get(i).copied(), alpha.get(i).copied(), ambient.get(i).copied());
            let (fl, ro) = (filled.get(i).copied(), rocky.get(i).copied());
            let c = match last {
                Some((lv, l, c)) if l == [sh, al, am, fl, ro] && same(lv, v) => c,
                _ => {
                    let c = colorizer.color_lit(v, sh, am, fl, ro).map(|mut c| {
                        if let Some(a) = al {
                            c[3] = ((c[3] as u16 * a as u16 + 127) / 255) as u8;
                        }
                        let a = c[3] as u16;
                        let pm = |x: u8| ((x as u16 * a + 127) / 255) as u8;
                        [pm(c[0]), pm(c[1]), pm(c[2]), c[3]]
                    });
                    last = Some((v, [sh, al, am, fl, ro], c));
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
        crate::blend_over(&mut pm.as_mut(), &lay, x0 as i64, y0 as i64);
        return;
    }
    let quality = if style.resampling == Resampling::Bilinear { FilterQuality::Bilinear } else { FilterQuality::Nearest };
    let at = Transform::from_row(k as f32, 0.0, 0.0, k as f32, (x0 as f64 * k) as f32, (y0 as f64 * k) as f32);
    // The rows it covers on screen.
    let rows = ((y0 as f64 * k).floor().max(0.0) as usize, (((y0 + bh) as f64 * k).ceil() as usize).min(pm.height() as usize));
    draw_scaled(pm, &lay, &PixmapPaint { opacity, quality, ..PixmapPaint::default() }, at, rows, threads);
}

/// A light `grid` drawn at `strength` (up to 1), unless that is 0 or the grid isn't `cells` long.
fn lit_by(grid: Option<&[u8]>, strength: f64, cells: usize) -> Option<(&[u8], f64)> {
    grid.filter(|g| strength > 0.0 && g.len() == cells).map(|g| (g, strength.min(1.0)))
}

/// Rows of the screen drawn as one band when a raster is scaled up (see [`draw_scaled`]).
const BAND: usize = 64;

/// Draws `src` onto `pm` through `at` as tiny-skia's `draw_pixmap` does, on `threads` threads.
/// The screen rows `rows` (where `src` lands) are drawn in bands of [`BAND`] rows, each on its
/// own as the same rows of `pm` with `at` moved up by the band's top, and the threads take runs
/// of whole bands: the bands are the same on any number of threads, so the pixels are too.
/// (Moving `at` rounds a bilinear sample a unit differently here and there from drawing the
/// whole at once.) Scaling a frame's raster up to a high-density screen on one thread took more
/// than half the frame (#1792).
fn draw_scaled(pm: &mut Pixmap, src: &Pixmap, paint: &PixmapPaint, at: Transform, (top, bottom): (usize, usize), threads: usize) {
    let (w, stride) = (pm.width(), pm.width() as usize * 4);
    let bottom = bottom.min(pm.height() as usize);
    let Some(covered) = pm.data_mut().get_mut(top.min(bottom) * stride..bottom * stride) else { return };
    let bands = (bottom.saturating_sub(top)).div_ceil(BAND);
    if bands == 0 {
        return;
    }
    let threads = threads.clamp(1, bands);
    let run = bands.div_ceil(threads) * BAND;
    each_chunk(threads, covered.chunks_mut(run * stride).enumerate(), |(i, data)| {
        for (j, band) in data.chunks_mut(BAND * stride).enumerate() {
            let h = (band.len() / stride) as u32;
            let first = top + i * run + j * BAND;
            if let Some(mut part) = tiny_skia::PixmapMut::from_bytes(band, w, h) {
                part.draw_pixmap(0, 0, src.as_ref(), paint, at.post_translate(0.0, -(first as f32)), None);
            }
        }
    });
}

/// The most relief cells in view whose water levels a draw keeps (16 MB): past that a cell is
/// smaller than a pixel, so few pixels share one and each is looked up as it comes.
const MAX_WATER_CELLS: usize = 1 << 22;

/// A cell's water level not looked up yet ([`WaterCells`]).
const UNREAD: u32 = u32::MAX;

/// The terrain's water over a Shaded Relief's cells for one draw (#1915): each cell's level is
/// looked up once, at its centre, however many pixels and lights read it (on a projected DEM each
/// lookup projects the centre to Web Mercator).
struct WaterCells<'a> {
    water: &'a Water,
    /// The first column and row of the cells kept, and how many columns a row.
    origin: (i64, i64),
    cols: i64,
    /// Each kept cell's level (`f32` bits), NaN where it is dry, [`UNREAD`] until looked up.
    levels: Vec<std::sync::atomic::AtomicU32>,
}

impl<'a> WaterCells<'a> {
    /// The water over the cells of `r` that the source positions in `grid` reach, with a margin of
    /// two cells (a light's neighbours); none kept when they are more than [`MAX_WATER_CELLS`].
    fn new(water: &'a Water, r: &Raster, grid: &[Option<(f64, f64)>]) -> WaterCells<'a> {
        let (mut c0, mut r0, mut c1, mut r1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (c, row) in grid.iter().flatten() {
            (c0, r0, c1, r1) = (c0.min(*c), r0.min(*row), c1.max(*c), r1.max(*row));
        }
        let clamp = |v: f64, n: u32| if v.is_finite() { (v.floor() as i64).clamp(0, i64::from(n)) } else { 0 };
        let (c0, c1) = (clamp(c0 - 2.0, r.width), clamp(c1 + 3.0, r.width));
        let (r0, r1) = (clamp(r0 - 2.0, r.height), clamp(r1 + 3.0, r.height));
        let (cols, rows) = ((c1 - c0).max(0), (r1 - r0).max(0));
        let n = usize::try_from(cols.saturating_mul(rows)).unwrap_or(usize::MAX);
        let levels = if n <= MAX_WATER_CELLS { (0..n).map(|_| std::sync::atomic::AtomicU32::new(UNREAD)).collect() } else { Vec::new() };
        WaterCells { water, origin: (c0, r0), cols: if levels.is_empty() { 0 } else { cols }, levels }
    }

    /// The level (metres) of the water at the centre of the cell `(ci, ri)` of `r`, when a lake
    /// holds it; as an `f32`, kept or not.
    fn level(&self, r: &Raster, (ci, ri): (i64, i64)) -> Option<f64> {
        let (dc, dr) = (ci - self.origin.0, ri - self.origin.1);
        let slot = (dc >= 0 && dr >= 0 && dc < self.cols)
            .then(|| usize::try_from(dr * self.cols + dc).ok())
            .flatten()
            .and_then(|i| self.levels.get(i));
        let read = |slot: Option<&std::sync::atomic::AtomicU32>| {
            let lonlat = r.transformer().to_lonlat(r.transform.to_world(ci as f64 + 0.5, ri as f64 + 0.5));
            let level = lonlat.and_then(|ll| self.water.level(lonlat_to_merc(ll))).map_or(f32::NAN, |l| l as f32);
            if let Some(slot) = slot {
                slot.store(level.to_bits(), std::sync::atomic::Ordering::Relaxed);
            }
            level
        };
        let level = match slot.map(|s| s.load(std::sync::atomic::Ordering::Relaxed)) {
            Some(bits) if bits != UNREAD => f32::from_bits(bits),
            _ => read(slot),
        };
        (!level.is_nan()).then_some(f64::from(level))
    }
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
    /// The light cast shadows leave on each cell, and how dark they are drawn (0 to 1).
    shadow: Option<(&'a [u8], f64)>,
    /// The share of the sky each cell sees, and how much it scales the ambient light (0 to 1).
    sky: Option<(&'a [u8], f64)>,
    /// The fill light's strength (0 to 1; 0, none), see [`FILL_ALTITUDE`].
    fill: f64,
    /// Shaded Relief's rock on steep slopes, when it has one.
    rock: Option<Rock>,
    /// The terrain's water, for Shaded Relief: its cells take its level and flat ground's light.
    water: Option<WaterCells<'a>>,
    resampling: Resampling,
    alpha_band: Option<usize>,
    /// The alpha band's opaque value (its pixel type's: 65535 for 16 bits).
    alpha_max: f32,
    cell: (f64, f64),
    /// The grid steps (columns, rows) that lead east and north: -1 for a grid stored right to left
    /// or bottom up, which the hillshade's north-up window must undo.
    east_north: (i64, i64),
    /// Values drawn as no data.
    mask: &'a RasterMask,
}

impl Pass1<'_> {
    /// Fills the band values (and hillshade, alpha, ambient and fill light and share of rock, when
    /// kept) of the screen rows from `py0` on, as many as `vals` holds.
    fn rows(
        &self,
        py0: usize,
        vals: &mut [f32],
        shade: &mut [u8],
        alpha: &mut [u8],
        ambient: &mut [u8],
        filled: &mut [u8],
        rocky: &mut [u8],
    ) {
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
            // Bilinear relief: the last four cells' lights, by the first's column and row; and the
            // last pixel that fell in the grid, with its source position.
            let mut quad: Option<((i64, i64), [Option<Lit>; 4])> = None;
            let mut prev: Option<(usize, f64, f64)> = None;
            // The source position of the pixel in column `dx` of this row, its column wrapped here,
            // not in `source`: the grid's columns run on across a 0..360° seam.
            let pos_at = |dx: usize| {
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
                pos.map(|(c, row)| (r.wrap_col(c), row))
            };
            for dx in 0..bw {
                let Some((c, row)) = pos_at(dx) else { continue };
                if c < 0.0 || row < 0.0 || c >= r.width as f64 || row >= r.height as f64 {
                    continue;
                }
                let i = dy * bw + dx;
                // How many cells one screen pixel spans here, along the row: from the last pixel in
                // the grid, or the next one on the row's first. Across a whole turn's seam the columns
                // are counted the short way round: 35.95 to 0.05 is a tenth of a cell, not 35.9.
                let last_drawn = prev.replace((dx, c, row));
                let cols_apart = |other: f64| {
                    let d = (c - other).abs();
                    self.wrap_width.map_or(d, |w| d.min(w as f64 - d))
                };
                let span = || match last_drawn {
                    Some((pdx, pc, pr)) => cols_apart(pc).hypot(row - pr) / (dx - pdx) as f64,
                    None => pos_at(dx + 1).map_or(0.0, |(nc, nr)| cols_apart(nc).hypot(nr - row)),
                };
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
                // On the water the relief's colour is the lake's level's, as even as its light: a
                // cell whose centre is in a lake, as `light` has it.
                if let Some(level) = self.water_level(cell)
                    && let Some(slot) = row_vals.get_mut(dx * nb)
                    && !slot.is_nan()
                {
                    *slot = level as f32;
                }
                if let (Some(relief), Some(&b)) = (self.relief, self.bands.first()) {
                    // Bilinear: the light blended between the four cells around the point, as the
                    // band values are (#1880). Nearest, or no cell around it lit: its own cell's.
                    // So too where a cell is a pixel across or less: the blend can't be seen there,
                    // and each pixel would work out four new lights.
                    let blended = (self.resampling == Resampling::Bilinear && span() < 1.0).then(|| {
                        let (x, y) = (c - 0.5, row - 0.5);
                        let at = (x.floor() as i64, y.floor() as i64);
                        let cell_light = |i, j| self.cell_light(b, relief, (i, j));
                        let lights = match quad {
                            Some((q, lights)) if q == at => lights,
                            // One cell east or west: two of the lights are the last four's.
                            Some(((qi, qj), [_, ne, _, se])) if qj == at.1 && qi + 1 == at.0 => {
                                [ne, cell_light(at.0 + 1, at.1), se, cell_light(at.0 + 1, at.1 + 1)]
                            }
                            Some(((qi, qj), [nw, _, sw, _])) if qj == at.1 && qi - 1 == at.0 => {
                                [cell_light(at.0, at.1), nw, cell_light(at.0, at.1 + 1), sw]
                            }
                            _ => self.quad_lights(b, relief, at),
                        };
                        quad = Some((at, lights));
                        blend(lights, x - x.floor(), y - y.floor())
                    });
                    let light = blended.flatten().unwrap_or_else(|| {
                        let centre = value(r, self.mask, b, cell.0, cell.1, self.wrap_width).unwrap_or(0.0);
                        self.light(b, relief, cell, f64::from(centre))
                    });
                    if let Some(s) = shade.get_mut(i) {
                        *s = light.direct.round().clamp(0.0, 255.0) as u8;
                    }
                    if self.sky.is_some()
                        && let Some(a) = ambient.get_mut(i)
                    {
                        *a = light.ambient.round().clamp(0.0, 255.0) as u8;
                    }
                    if let Some(f) = filled.get_mut(i) {
                        *f = light.fill.round().clamp(0.0, 255.0) as u8;
                    }
                    if let Some(ro) = rocky.get_mut(i) {
                        *ro = light.rock.round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
    }

    /// The level (metres) of the terrain's water at the centre of the cell `(ci, ri)`, when a
    /// lake holds it.
    fn water_level(&self, cell: (i64, i64)) -> Option<f64> {
        self.water.as_ref()?.level(self.r, cell)
    }

    /// The lights on the relief's cell `(ci, ri)`, from 0 to 255 and not rounded: the direct
    /// light, the hillshade of its 3 × 3 window times what the cast shadows leave on it; and the
    /// ambient light, scaled by the sky the cell sees; the fill light (see [`FILL_ALTITUDE`]), which
    /// nothing shadows; and its share of rock (see [`Rock::share`]), from the ground's own slope in
    /// the same window. `centre` is the cell's height, and stands in for a neighbour that is no
    /// data, masked or outside the grid. Every light a cell gets is worked out here, so that
    /// [`blend`] smooths each of them.
    fn light(&self, band: usize, (az, alt, zf): (f64, f64, f64), (ci, ri): (i64, i64), centre: f64) -> Lit {
        let r = self.r;
        // A water cell (#1915): flat ground's own light, with no cast shadow, the whole sky and no
        // fill or rock (both are nothing on flat ground).
        if self.water_level((ci, ri)).is_some() {
            let direct = f64::from(hillshade(&[0.0; 9], self.cell.0, self.cell.1, az, alt, zf));
            return Lit { direct, ambient: 255.0, fill: 0.0, rock: 0.0 };
        }
        let mut z = [centre; 9];
        for (k, (dx, dy)) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (0, 0), (1, 0), (-1, 1), (0, 1), (1, 1)].into_iter().enumerate() {
            let (dx, dy) = (dx * self.east_north.0, dy * self.east_north.1);
            z[k] = value(r, self.mask, band, ci + dx, ri + dy, self.wrap_width).map(f64::from).unwrap_or(centre);
        }
        let at = ri as usize * r.width as usize + ci as usize;
        // What a grid of 0 (none) to 255 (all) leaves of a light, at `strength`.
        let left = |grid: Option<(&[u8], f64)>| {
            grid.and_then(|(grid, strength)| grid.get(at).map(|v| 1.0 - strength * (1.0 - f64::from(*v) / 255.0)))
        };
        let mut direct = f64::from(hillshade(&z, self.cell.0, self.cell.1, az, alt, zf));
        if let Some(left) = left(self.shadow) {
            direct *= left;
        }
        // The fill light lights only what turns to it more than flat ground does, so it opens the
        // slopes away from the sun without lifting the whole map.
        let fill = if self.fill > 0.0 {
            let toward = f64::from(hillshade(&z, self.cell.0, self.cell.1, (az + 180.0) % 360.0, FILL_ALTITUDE, zf));
            self.fill * (toward - FILL_FLAT).max(0.0)
        } else {
            0.0
        };
        let rock = self.rock.map_or(0.0, |rock| rock.share(slope(&z, self.cell.0, self.cell.1)) * 255.0);
        Lit { direct, ambient: left(self.sky).map_or(255.0, |left| left * 255.0), fill, rock }
    }

    /// The lights of the cell `(i, j)`, or none when it is no data, masked or outside the grid; a
    /// column past a whole turn's edge wraps.
    fn cell_light(&self, band: usize, relief: (f64, f64, f64), (i, j): (i64, i64)) -> Option<Lit> {
        let ci = self.wrap_width.map_or(i, |w| i.rem_euclid(w));
        if ci < 0 || j < 0 || ci >= i64::from(self.r.width) || j >= i64::from(self.r.height) {
            return None;
        }
        let centre = value(self.r, self.mask, band, ci, j, self.wrap_width)?;
        Some(self.light(band, relief, (ci, j), f64::from(centre)))
    }

    /// The lights of the four cells whose centres are around a point, from the cell `(i, j)` at
    /// its top left: `(i, j)`, `(i + 1, j)`, `(i, j + 1)` and `(i + 1, j + 1)`.
    fn quad_lights(&self, band: usize, relief: (f64, f64, f64), (i, j): (i64, i64)) -> [Option<Lit>; 4] {
        [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(di, dj)| self.cell_light(band, relief, (i + di, j + dj)))
    }
}

/// How high a Shaded Relief's fill light is, in degrees: it comes from the side opposite the sun,
/// high enough not to draw long gradients of its own.
const FILL_ALTITUDE: f64 = 55.0;

/// The fill light's hillshade (0–255) on flat ground, rounded as [`hillshade`] rounds it: the
/// share of it nothing gains.
const FILL_FLAT: f64 = 209.0;

/// A relief cell's lights, from 0 to 255 and not rounded (see [`Pass1::light`]).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Lit {
    direct: f64,
    ambient: f64,
    fill: f64,
    /// Its share of rock, 0 to 255: not a light, but blended as the lights are.
    rock: f64,
}

/// The four cells' lights of [`Pass1::quad_lights`] blended bilinearly at `(fx, fy)` (0 to 1)
/// from the first, each light on its own. A cell without lights is left out and the others'
/// weights scaled up: the light is clamped at the grid's edge, and a clipped or masked edge isn't
/// darkened. At a cell's centre this is that cell's lights exactly. `None` when none of the four
/// with a weight has lights.
fn blend(lights: [Option<Lit>; 4], fx: f64, fy: f64) -> Option<Lit> {
    let weights = [(1.0 - fx) * (1.0 - fy), fx * (1.0 - fy), (1.0 - fx) * fy, fx * fy];
    let (mut direct, mut ambient, mut fill, mut rock, mut total) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (light, w) in lights.into_iter().zip(weights) {
        if let Some(light) = light {
            direct += w * light.direct;
            ambient += w * light.ambient;
            fill += w * light.fill;
            rock += w * light.rock;
            total += w;
        }
    }
    (total > 0.0).then(|| Lit { direct: direct / total, ambient: ambient / total, fill: fill / total, rock: rock / total })
}

/// Whether two pixels' band values are the same (NaN, no data, equal to itself).
fn same(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// `v` in chunks of `n`, or endless empty chunks when it is empty (a plane not kept).
fn chunks_or_empty(v: &mut [u8], n: usize) -> Box<dyn Iterator<Item = &mut [u8]> + Send + '_> {
    if v.is_empty() { Box::new(std::iter::repeat_with(|| &mut [][..])) } else { Box::new(v.chunks_mut(n.max(1))) }
}

/// The most threads a raster is drawn on. A frame's raster takes milliseconds, and #331 measured
/// its speed-up up to 8 threads; whether more pays off past the cost of starting them is
/// untested, so this keeps the cap it was tuned at, below the shared count.
const MAX_DRAW_THREADS: usize = 8;

/// How many threads to draw `pixels` with: one for a small image, and always one in the browser;
/// the shared count ([`rustgis_data::threads::max`]) up to [`MAX_DRAW_THREADS`] for a large one.
fn threads_for(pixels: usize) -> usize {
    if cfg!(target_arch = "wasm32") || pixels < 64 * 1024 {
        return 1;
    }
    rustgis_data::threads::max().min(MAX_DRAW_THREADS)
}

/// Runs `f` on each item, spread over `threads` threads (in turn on this one when it is one). An
/// item whose thread the system refuses is run on this one: it costs speed, not the drawing.
fn each_chunk<T: Send>(threads: usize, items: impl Iterator<Item = T>, f: impl Fn(T) + Sync) {
    if threads <= 1 {
        items.for_each(f);
        return;
    }
    let f = &f;
    // Each item in a slot of its own, so one whose thread wasn't started is still at hand.
    let slots: Vec<std::sync::Mutex<Option<T>>> = items.map(|item| std::sync::Mutex::new(Some(item))).collect();
    let take = |slot: &std::sync::Mutex<Option<T>>| slot.lock().ok().and_then(|mut s| s.take());
    std::thread::scope(|scope| {
        for slot in &slots {
            let started = std::thread::Builder::new().spawn_scoped(scope, move || {
                if let Some(item) = take(slot) {
                    f(item);
                }
            });
            if started.is_err()
                && let Some(item) = take(slot)
            {
                f(item);
            }
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

    /// A Shaded Relief without a ramp or rock, lit from `azimuth` and `altitude`.
    fn grey_relief(azimuth: f64, altitude: f64, z_factor: f64, shadow: f64, sky: f64) -> RasterRenderer {
        RasterRenderer::ShadedRelief {
            band: 0,
            ramp: None,
            azimuth,
            altitude,
            z_factor,
            shadow,
            sky,
            fill: 0.0,
            rock: None,
            ambient: None,
            balanced: false,
        }
    }

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

    /// A grid's stored direction doesn't change where the light comes from: bottom up and right to
    /// left rasters shade like the north-up one.
    #[test]
    fn shaded_relief_follows_the_grids_orientation() {
        let view = View {
            width: 1.0,
            height: 1.0,
            pixels_per_point: 1.0,
            camera: Camera { center: lonlat_to_merc(Coord::new(0.05, 0.05)), res: 1000.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        // Terrain `height(east, north)` on a 10 x 10 grid of 0.01 degree cells, stored in each direction.
        let shade = |right_to_left: bool, bottom_up: bool, azimuth: f64, height: &dyn Fn(usize, usize) -> f32| {
            let gt = GeoTransform {
                origin_x: if right_to_left { 0.1 } else { 0.0 },
                origin_y: if bottom_up { 0.0 } else { 0.1 },
                pixel_width: if right_to_left { -0.01 } else { 0.01 },
                pixel_height: if bottom_up { 0.01 } else { -0.01 },
            };
            let data = (0..10usize)
                .flat_map(|row| (0..10usize).map(move |col| (col, row)))
                .map(|(col, row)| height(if right_to_left { 9 - col } else { col }, if bottom_up { row } else { 9 - row }))
                .collect();
            let r = Raster::new("relief", 10, 10, vec![data], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
            let mut style = RasterStyle::default_for(&r);
            style.renderer = RasterRenderer::ShadedRelief {
                band: 0,
                ramp: None,
                azimuth,
                altitude: 45.0,
                z_factor: 1.0,
                shadow: 0.0,
                sky: 0.0,
                fill: 0.0,
                rock: None,
                ambient: None,
                balanced: false,
            };
            style.resampling = Resampling::Nearest;
            let mut pm = Pixmap::new(1, 1).unwrap();
            draw(&mut pm, &r, &style, None, &view, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"), Lights::default());
            let p = pm.pixel(0, 0).unwrap();
            [p.red(), p.green(), p.blue(), p.alpha()]
        };
        let north = |_: usize, n: usize| n as f32 * 300.0;
        let flat = |_: usize, _: usize| 0.0;
        let diagonal = |e: usize, n: usize| (e + 2 * n) as f32 * 300.0;
        // Terrain rising to the north, lit from the north: darker than flat ground, in every direction.
        let lit = shade(false, false, 0.0, &north);
        assert_eq!(lit[0], 159);
        assert!(lit[0] < shade(false, false, 0.0, &flat)[0]);
        for (right_to_left, bottom_up) in [(false, true), (true, false), (true, true)] {
            assert_eq!(shade(right_to_left, bottom_up, 0.0, &north), lit, "north, rtl {right_to_left}, bottom up {bottom_up}");
            for azimuth in [45.0, 135.0, 225.0, 315.0] {
                let want = shade(false, false, azimuth, &diagonal);
                assert_eq!(
                    shade(right_to_left, bottom_up, azimuth, &diagonal),
                    want,
                    "{azimuth} deg, rtl {right_to_left}, bottom up {bottom_up}"
                );
            }
        }
    }

    #[test]
    fn regional_relief_keeps_the_centre_fallback_at_its_edge() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 90.0, pixel_width: 10.0, pixel_height: -10.0 };
        let data = [0.0, 1_000_000.0, 2_000_000.0, 3_000_000.0].repeat(18);
        let r = Raster::new("regional relief", 4, 18, vec![data], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let mut style = RasterStyle::default_for(&r);
        style.renderer = grey_relief(270.0, 45.0, 1.0, 0.0, 0.0);
        style.resampling = Resampling::Nearest;
        let view = View {
            width: 1.0,
            height: 1.0,
            pixels_per_point: 1.0,
            camera: Camera { center: lonlat_to_merc(Coord::new(5.0, 0.0)), res: 1000.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let mut pm = Pixmap::new(1, 1).unwrap();
        draw(&mut pm, &r, &style, None, &view, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"), Lights::default());
        let z = [0.0, 0.0, 1_000_000.0, 0.0, 0.0, 1_000_000.0, 0.0, 0.0, 1_000_000.0];
        let (cell_x, cell_y) = r.cell_size_m();
        let shade = hillshade(&z, cell_x, cell_y, 270.0, 45.0, 1.0);
        let expected = Colorizer::new(&style, &r, None).color(&[0.0], Some(shade)).unwrap();
        let actual = pm.pixel(0, 0).unwrap();
        assert_eq!([actual.red(), actual.green(), actual.blue(), actual.alpha()], expected);
    }

    /// Cast shadows darken the hillshade by their strength, the ambient light left; a shadow of 0
    /// draws today's pixels, with a light grid or without.
    #[test]
    fn cast_shadows_darken_the_direct_light_by_their_strength() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 90.0, pixel_width: 10.0, pixel_height: -10.0 };
        let data = [0.0, 1_000_000.0, 2_000_000.0, 3_000_000.0].repeat(18);
        let r = Raster::new("relief", 4, 18, vec![data], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let view = View {
            width: 1.0,
            height: 1.0,
            pixels_per_point: 1.0,
            camera: Camera { center: lonlat_to_merc(Coord::new(5.0, 0.0)), res: 1000.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let z = [0.0, 0.0, 1_000_000.0, 0.0, 0.0, 1_000_000.0, 0.0, 0.0, 1_000_000.0];
        let (cell_x, cell_y) = r.cell_size_m();
        let lit = hillshade(&z, cell_x, cell_y, 270.0, 45.0, 1.0);
        let dark = vec![0u8; 4 * 18];
        let drawn = |shadow: f64, light: Option<&[u8]>| {
            let mut style = RasterStyle::default_for(&r);
            style.renderer = grey_relief(270.0, 45.0, 1.0, shadow, 0.0);
            style.resampling = Resampling::Nearest;
            let mut pm = Pixmap::new(1, 1).unwrap();
            draw(
                &mut pm,
                &r,
                &style,
                None,
                &view,
                1.0,
                false,
                Place::Flat(&Flat::Mercator, "mercator"),
                Lights { shadow: light, sky: None, water: None },
            );
            let p = pm.pixel(0, 0).unwrap();
            let want = |shade: u8| Colorizer::new(&style, &r, None).color(&[0.0], Some(shade)).unwrap();
            ([p.red(), p.green(), p.blue(), p.alpha()], want(lit), want(0), want((f64::from(lit) * 0.5).round() as u8))
        };
        let none = Some(dark.as_slice());
        // Without shadows, today's pixels, a grid or not; with them, the direct light by their strength.
        for (shadow, light, which) in [(0.0, None, 0), (0.0, none, 0), (1.0, None, 0), (1.0, none, 1), (0.5, none, 2)] {
            let (pixel, lit, unlit, half) = drawn(shadow, light);
            assert_eq!(pixel, [lit, unlit, half][which], "shadow {shadow}, grid {}", light.is_some());
        }
        // In full shadow the relief keeps its ambient light (a quarter of its colour), not black.
        let (pixel, _, _, _) = drawn(1.0, none);
        assert_eq!(pixel, [64, 64, 64, 255]);
        // A grid of another size isn't read.
        let (pixel, lit, _, _) = drawn(1.0, Some(&dark[..10]));
        assert_eq!(pixel, lit);
    }

    /// The sky a cell sees scales its ambient light by the sky's strength, the direct light left
    /// as it is; a sky of 0 draws today's pixels, with a sky grid or without.
    #[test]
    fn the_sky_scales_the_ambient_light_by_its_strength() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 90.0, pixel_width: 10.0, pixel_height: -10.0 };
        let data = [0.0, 1_000_000.0, 2_000_000.0, 3_000_000.0].repeat(18);
        let r = Raster::new("relief", 4, 18, vec![data], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let view = View {
            width: 1.0,
            height: 1.0,
            pixels_per_point: 1.0,
            camera: Camera { center: lonlat_to_merc(Coord::new(5.0, 0.0)), res: 1000.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let z = [0.0, 0.0, 1_000_000.0, 0.0, 0.0, 1_000_000.0, 0.0, 0.0, 1_000_000.0];
        let (cell_x, cell_y) = r.cell_size_m();
        let lit = hillshade(&z, cell_x, cell_y, 270.0, 45.0, 1.0);
        let (none, dark) = (vec![0u8; 4 * 18], vec![0u8; 4 * 18]);
        let drawn = |shadow: f64, sky: f64, light: Lights| {
            let mut style = RasterStyle::default_for(&r);
            style.renderer = grey_relief(270.0, 45.0, 1.0, shadow, sky);
            style.resampling = Resampling::Nearest;
            let mut pm = Pixmap::new(1, 1).unwrap();
            draw(&mut pm, &r, &style, None, &view, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"), light);
            let p = pm.pixel(0, 0).unwrap();
            [p.red(), p.green(), p.blue(), p.alpha()]
        };
        let today = drawn(0.0, 0.0, Lights::default());
        let hidden = Lights { shadow: None, sky: Some(&none), water: None };
        assert_eq!(drawn(0.0, 0.0, hidden), today, "a sky of 0");
        assert_eq!(drawn(0.0, 1.0, Lights::default()), today, "no grid");
        // No sky at all: the direct light alone.
        let mut style = RasterStyle::default_for(&r);
        style.renderer = grey_relief(270.0, 45.0, 1.0, 0.0, 1.0);
        let want = Colorizer::new(&style, &r, None).color_lit(&[0.0], Some(lit), Some(0), None, None).unwrap();
        assert_eq!(drawn(0.0, 1.0, hidden), want);
        assert!(want[0] < today[0]);
        // In full shadow only the ambient light is left: a quarter of the colour, scaled by the
        // sky by its strength.
        let half_seen = vec![128u8; 4 * 18];
        for (sky, grid, want) in [(1.0, &dark, 0), (0.5, &dark, 32), (1.0, &half_seen, 32), (0.0, &dark, 64)] {
            let light = Lights { shadow: Some(&dark), sky: Some(grid.as_slice()), water: None };
            assert_eq!(drawn(1.0, sky, light), [want, want, want, 255], "sky {sky}, seen {}", grid[0]);
        }
    }

    /// The fill light (#1912), from the side opposite a western sun, 55° up: a fill of 0 draws
    /// today's pixels; flat ground gains nothing; a slope turned east, toward the fill, gains
    /// `fill × (L − sin 55°)` of its colour, L its Lambert term toward the fill light.
    #[test]
    fn the_fill_light_opens_only_the_slopes_turned_toward_it() {
        assert_eq!(f64::from(hillshade(&[0.0; 9], 100.0, 100.0, 90.0, FILL_ALTITUDE, 1.0)), FILL_FLAT);
        // Three columns of 100 m cells, their heights `east` from west to east: the middle cell's red.
        let drawn = |east: [f64; 3], fill: f64| {
            let gt = GeoTransform { origin_x: 0.0, origin_y: 300.0, pixel_width: 100.0, pixel_height: -100.0 };
            let data = east.map(|h| h as f32).repeat(3);
            let r = Raster::new("relief", 3, 3, vec![data], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32").unwrap();
            let mut style = RasterStyle::default_for(&r);
            let (azimuth, altitude) = (270.0, 45.0);
            style.renderer = RasterRenderer::ShadedRelief {
                band: 0,
                ramp: None,
                azimuth,
                altitude,
                z_factor: 1.0,
                shadow: 0.0,
                sky: 0.0,
                fill,
                rock: None,
                ambient: None,
                balanced: false,
            };
            style.resampling = Resampling::Nearest;
            let view = View {
                width: 1.0,
                height: 1.0,
                pixels_per_point: 1.0,
                camera: Camera { center: Coord::new(150.0, 150.0), res: 1.0, bearing: 0.0, pitch: 0.0 },
                sky: false,
            };
            let mut pm = Pixmap::new(1, 1).unwrap();
            draw(&mut pm, &r, &style, None, &view, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"), Lights::default());
            pm.pixel(0, 0).unwrap().red()
        };
        // Today's colour (a quarter ambient, three quarters the sun's hillshade), plus the fill.
        let want = |east: [f64; 3], fill: f64| {
            let z: [f64; 9] = std::array::from_fn(|k| east[k % 3]);
            let shade = f64::from(hillshade(&z, 100.0, 100.0, 270.0, 45.0, 1.0));
            let toward = f64::from(hillshade(&z, 100.0, 100.0, 90.0, FILL_ALTITUDE, 1.0));
            (255.0 * 0.25 + 0.75 * shade + (fill * (toward - FILL_FLAT).max(0.0)).round()).round() as u8
        };
        let (flat, to_sun, to_fill) = ([0.0; 3], [100.0, 200.0, 300.0], [300.0, 200.0, 100.0]);
        for terrain in [flat, to_sun, to_fill] {
            assert_eq!(drawn(terrain, 0.0), want(terrain, 0.0), "a fill of 0 draws today's pixels, {terrain:?}");
            assert_eq!(drawn(terrain, 0.3), want(terrain, 0.3), "{terrain:?}");
        }
        assert_eq!(drawn(flat, 1.0), drawn(flat, 0.0), "flat ground gains nothing");
        assert_eq!(drawn(to_sun, 1.0), drawn(to_sun, 0.0), "nor a slope turned to the sun");
        assert!(drawn(to_fill, 0.3) > drawn(to_fill, 0.0) && drawn(to_fill, 1.0) > drawn(to_fill, 0.3), "a slope turned to the fill");
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
            style.renderer = grey_relief(270.0, 45.0, 1.0, 0.0, 0.0);
            style.resampling = Resampling::Nearest;
            let mut pm = Pixmap::new(80, 40).unwrap();
            draw(&mut pm, &r, &style, None, &view, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"), Lights::default());
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
            draw(&mut pm, &r, style, stats, &view, 1.0, false, flat, Lights::default());
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
        draw(&mut pm, &r, &style, None, &view, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"), Lights::default());
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
                _ => grey_relief(315.0, 45.0, 1.0, 0.0, 0.0),
            };
            s.dra = true;
            s.resampling = Resampling::Bilinear;
            draw(&mut pm, &r, &s, None, &view, 0.5, false, Place::Flat(&Flat::Mercator, "mercator"), Lights::default());
        }
        // High-density screens draw at logical resolution, scaled up.
        let hi = View { pixels_per_point: 2.0, ..view };
        let mut pm2 = Pixmap::new(400, 400).unwrap();
        draw(&mut pm2, &r, &style, None, &hi, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"), Lights::default());
        assert_eq!(pm2.pixel(200, 200).map(|p| p.alpha()), Some(255));
        assert_eq!(pm2.pixel(4, 4).map(|p| p.alpha()), Some(0));
        // Spread over threads, every pixel comes out as drawn on one, for each renderer.
        let wide = View { width: 420.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
        for kind in ["stretch", "rgb", "shadedRelief"] {
            let mut s = style.clone();
            s.renderer = match kind {
                "rgb" => RasterRenderer::Rgb { red: 0, green: 0, blue: 0 },
                "shadedRelief" => grey_relief(315.0, 45.0, 1.0, 0.0, 0.0),
                _ => s.renderer,
            };
            let mut one = Pixmap::new(420, 300).unwrap();
            let mut many = Pixmap::new(420, 300).unwrap();
            draw_with(&mut one, &r, &s, None, &wide, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"), 1, Lights::default());
            draw_with(&mut many, &r, &s, None, &wide, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"), 7, Lights::default());
            assert!(one.data() == many.data(), "{kind}");
            assert!(one.pixels().iter().any(|p| p.alpha() == 255), "{kind} draws");
        }
        let far = View { width: 3.0, height: 1.0, pixels_per_point: 2.0, camera: Camera::world(), sky: false };
        let mut tiny = Pixmap::new(6, 2).unwrap();
        draw(&mut tiny, &r, &style, None, &far, 1.0, true, Place::Flat(&Flat::Mercator, "mercator"), Lights::default());
    }

    /// #1792: a projected raster scaled up to a high-density screen, its reprojection grid and
    /// its scaling spread over threads, comes out as drawn on one, pixel for pixel, at any
    /// scale, opacity and resampling, partly in view or filling it.
    #[test]
    fn scaled_up_on_threads_as_on_one() {
        // 200 × 200 cells of 5 m in Swiss LV95, valued in a ramp with a few holes.
        let gt = GeoTransform { origin_x: 2_696_000.0, origin_y: 1_262_000.0, pixel_width: 5.0, pixel_height: -5.0 };
        let data: Vec<f32> = (0..200 * 200).map(|i| if i % 97 == 0 { -9999.0 } else { ((i % 200) + (i / 200)) as f32 }).collect();
        let r =
            Raster::new("dem", 200, 200, vec![data], gt, Crs::from_epsg(2056).unwrap(), Some(-9999.0), ColorHint::Gray, "Float32").unwrap();
        let centre = lonlat_to_merc(Coord::new(8.7205, 47.4985));
        for (res, ppp) in [(2.0, 1.5), (2.0, 2.0), (0.7, 1.5), (0.7, 1.75), (6.0, 2.0)] {
            for (opacity, resampling) in [(1.0, Resampling::Bilinear), (0.6, Resampling::Nearest), (1.0, Resampling::Nearest)] {
                let mut style = RasterStyle::default_for(&r);
                style.resampling = resampling;
                let camera = Camera { center: centre, res, bearing: 0.0, pitch: 0.0 };
                let view = View { width: 300.0, height: 200.0, pixels_per_point: ppp, camera, sky: false };
                let size = ((300.0 * ppp).ceil() as u32, (200.0 * ppp).ceil() as u32);
                let mut one = Pixmap::new(size.0, size.1).unwrap();
                one.fill(tiny_skia::Color::from_rgba8(20, 120, 40, 255));
                let mut many = one.clone();
                draw_with(
                    &mut one,
                    &r,
                    &style,
                    None,
                    &view,
                    opacity,
                    false,
                    Place::Flat(&Flat::Mercator, "mercator"),
                    1,
                    Lights::default(),
                );
                draw_with(
                    &mut many,
                    &r,
                    &style,
                    None,
                    &view,
                    opacity,
                    false,
                    Place::Flat(&Flat::Mercator, "mercator"),
                    7,
                    Lights::default(),
                );
                assert!(one.data() == many.data(), "res {res} ppp {ppp} opacity {opacity} {resampling:?}");
                assert!(one.pixels().iter().any(|p| p.green() != 120), "res {res} ppp {ppp}: the raster is drawn");
            }
        }
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
        draw(&mut pm, &r, &style, None, &view, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"), Lights::default());
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
            draw(&mut pm, &r, &style, None, &view, 1.0, false, Place::Flat(&flat, "robinson +lon_0=150"), Lights::default());
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

    /// A row of 600 screen pixels across a relief of 6 × 3 cells of 100 m whose slope grows
    /// eastward, lit from the west, its middle row drawn: cell `k` covers pixels `100 k - 1` to
    /// `100 k + 98`, its centre on `100 k + 49`, and the last pixel is past the raster. The red
    /// channel of each pixel, or `None` where it isn't drawn. The cast shadows and sky of `light`,
    /// when given, are drawn at full strength; a `mirrored` grid is stored right to left.
    fn relief_row(resampling: Resampling, nodata: Option<f64>, mask: &[f64], light: Lights, mirrored: bool) -> Vec<Option<u8>> {
        let (origin_x, pixel_width) = if mirrored { (600.0, -100.0) } else { (0.0, 100.0) };
        let gt = GeoTransform { origin_x, origin_y: 300.0, pixel_width, pixel_height: -100.0 };
        let mut heights = [0.0, 0.0, 20.0, 60.0, 120.0, 200.0];
        if mirrored {
            heights.reverse();
        }
        let data = heights.repeat(3);
        let r = Raster::new("relief", 6, 3, vec![data], gt, Crs::WebMercator, nodata, ColorHint::Gray, "Float32").unwrap();
        let mut style = RasterStyle::default_for(&r);
        let full = |grid: Option<&[u8]>| if grid.is_some() { 1.0 } else { 0.0 };
        let (shadow, sky) = (full(light.shadow), full(light.sky));
        style.renderer = grey_relief(270.0, 45.0, 1.0, shadow, sky);
        style.resampling = resampling;
        style.mask = RasterMask { values: mask.to_vec(), ..RasterMask::default() };
        let view = View {
            width: 600.0,
            height: 1.0,
            pixels_per_point: 1.0,
            camera: Camera { center: Coord::new(300.5, 150.0), res: 1.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let mut pm = Pixmap::new(600, 1).unwrap();
        draw(&mut pm, &r, &style, None, &view, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"), light);
        (0..600).map(|x| pm.pixel(x, 0).filter(|p| p.alpha() > 0).map(|p| p.red())).collect()
    }

    /// The middle row of a white relief rising eastward (cells 100 m, slopes of 0, 5.7, 16.7, 26.6,
    /// 35 and 21.8 degrees), lit from `azimuth` with `rock`, `z_factor` and `fill`, as each pixel's
    /// colour.
    fn rock_row(resampling: Resampling, rock: Option<Rock>, z_factor: f64, (azimuth, fill): (f64, f64)) -> Vec<[u8; 3]> {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 300.0, pixel_width: 100.0, pixel_height: -100.0 };
        let data = [0.0, 0.0, 20.0, 60.0, 120.0, 200.0].repeat(3);
        let r = Raster::new("relief", 6, 3, vec![data], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32").unwrap();
        let mut style = RasterStyle::default_for(&r);
        style.renderer = RasterRenderer::ShadedRelief {
            band: 0,
            ramp: None,
            azimuth,
            altitude: 45.0,
            z_factor,
            shadow: 0.0,
            sky: 0.0,
            fill,
            rock,
            ambient: None,
            balanced: false,
        };
        style.resampling = resampling;
        let view = View {
            width: 600.0,
            height: 1.0,
            pixels_per_point: 1.0,
            camera: Camera { center: Coord::new(300.5, 150.0), res: 1.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let mut pm = Pixmap::new(600, 1).unwrap();
        draw(&mut pm, &r, &style, None, &view, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"), Lights::default());
        (0..600).map(|x| pm.pixel(x, 0).map_or([0; 3], |p| [p.red(), p.green(), p.blue()])).collect()
    }

    /// Rock on steep slopes (#1913) tints by the ground's own slope before the light: the cells
    /// below `from` keep today's pixels, those past `to` are the rock colour lit, those between
    /// lean toward it; a Z factor changes the light, not the tint; and Bilinear blends the tint
    /// between cell centres as it blends the light.
    #[test]
    fn rock_tints_steep_slopes_before_the_light() {
        const WEST: (f64, f64) = (270.0, 0.0);
        let rock = Rock { from: 10.0, to: 30.0, color: rustgis_map::Rgba::rgb(200, 100, 50) };
        let plain = rock_row(Resampling::Nearest, None, 1.0, WEST);
        let rocky = rock_row(Resampling::Nearest, Some(rock), 1.0, WEST);
        let centre = |row: &[[u8; 3]], k: usize| row[100 * k + 49];
        // Green over red: 1 on the white relief, 0.5 on the rock, whatever the light.
        let tint = |c: [u8; 3]| f64::from(c[1]) / f64::from(c[0]);
        for k in [0, 1] {
            assert_eq!(centre(&rocky, k), centre(&plain, k), "cell {k} is below from");
        }
        assert!((tint(centre(&rocky, 4)) - 0.5).abs() < 0.02, "cell 4 is past to: {:?}", centre(&rocky, 4));
        for k in [2, 3, 5] {
            let t = tint(centre(&rocky, k));
            assert!(0.52 < t && t < 0.98, "cell {k} leans toward rock: {t}");
        }
        assert!(tint(centre(&rocky, 2)) > tint(centre(&rocky, 3)), "steeper, more rock");
        // Three times the Z factor: other lights, the same tints.
        let steeper = rock_row(Resampling::Nearest, Some(rock), 3.0, WEST);
        for k in 1..6 {
            assert_ne!(centre(&steeper, k), centre(&rocky, k), "cell {k}: the light changes");
            assert!((tint(centre(&steeper, k)) - tint(centre(&rocky, k))).abs() < 0.02, "cell {k}: the tint doesn't");
        }
        // Bilinear: the tint runs from cell 2's centre to cell 3's, not in a step.
        let bilinear = rock_row(Resampling::Bilinear, Some(rock), 1.0, WEST);
        let tints = |row: &[[u8; 3]]| row[249..=349].iter().map(|c| (tint(*c) * 1000.0).round() as i64).collect::<Vec<_>>();
        let mut seen = tints(&bilinear);
        // (Each colour channel is rounded to a byte: a tint can step back by a hundredth.)
        assert!(seen.windows(2).all(|w| w[1] <= w[0] + 10), "toward more rock all the way: {seen:?}");
        seen.dedup();
        let mut stepped = tints(&rocky);
        stepped.dedup();
        assert!(seen.len() > 20 && stepped.len() == 2, "{} tints blended, {} nearest", seen.len(), stepped.len());
    }

    /// Rock and the fill light (#1912) together: the slopes turned from the sun take the rock
    /// colour, and the fill lightens it as it lightens the ramp, keeping its tint; flat ground
    /// gets neither.
    #[test]
    fn the_fill_light_lightens_the_rock_too() {
        let rock = Rock { from: 10.0, to: 30.0, color: rustgis_map::Rgba::rgb(200, 100, 50) };
        // The sun from the east: the slopes rising eastward face away from it, toward the fill.
        let unfilled = rock_row(Resampling::Nearest, Some(rock), 1.0, (90.0, 0.0));
        let filled = rock_row(Resampling::Nearest, Some(rock), 1.0, (90.0, 1.0));
        let plain = rock_row(Resampling::Nearest, None, 1.0, (90.0, 1.0));
        let centre = |row: &[[u8; 3]], k: usize| row[100 * k + 49];
        let tint = |c: [u8; 3]| f64::from(c[1]) / f64::from(c[0]);
        let (dim, lit) = (centre(&unfilled, 4), centre(&filled, 4));
        assert!(lit.iter().zip(dim).all(|(l, d)| *l > d), "cell 4 (35 degrees) lightened: {dim:?} to {lit:?}");
        assert!((tint(lit) - 0.5).abs() < 0.02 && (tint(dim) - 0.5).abs() < 0.02, "the rock's tint kept: {dim:?} {lit:?}");
        assert!(tint(centre(&plain, 4)) > 0.98, "no rock: the white relief, filled");
        assert_eq!(centre(&filled, 0), centre(&unfilled, 0), "flat: no fill");
        assert_eq!(centre(&filled, 0), centre(&plain, 0), "flat: no rock");
    }

    /// Bilinear relief blends the four cells' light (#1880): the same as Nearest at each cell's
    /// centre and in the outer half of the edge cells, and in between from one centre's light to
    /// the next, not in steps. Nearest keeps one light per cell.
    #[test]
    fn bilinear_relief_blends_the_light_between_cell_centres() {
        let nearest = relief_row(Resampling::Nearest, None, &[], Lights::default(), false);
        let bilinear = relief_row(Resampling::Bilinear, None, &[], Lights::default(), false);
        // Stored right to left, the cells are read westward: the same pixels (the last one, on
        // the raster's edge, falls on the other side of it).
        assert_eq!(relief_row(Resampling::Bilinear, None, &[], Lights::default(), true)[..599], bilinear[..599]);
        let centre = |k: usize| nearest[100 * k + 49].unwrap();
        for k in 0..6 {
            assert!(nearest[100 * k..100 * k + 98].iter().all(|v| *v == Some(centre(k))), "nearest, cell {k}");
            assert_eq!(bilinear[100 * k + 49], Some(centre(k)), "bilinear at the centre of cell {k}");
        }
        assert!(bilinear[..49].iter().all(|v| *v == Some(centre(0))) && bilinear[550..599].iter().all(|v| *v == Some(centre(5))));
        for k in 1..4 {
            let (a, b) = (centre(k), centre(k + 1));
            assert!(b > a + 5, "the slope grows toward the sun: {a} to {b}");
            let between: Vec<u8> = bilinear[100 * k + 49..=100 * k + 149].iter().map(|v| v.unwrap()).collect();
            assert!(between.windows(2).all(|w| w[0] <= w[1] && w[1] - w[0] <= 2), "cells {k} to {}: {between:?}", k + 1);
            let middle = between[50];
            assert!(a < middle && middle < b, "cells {k} to {}: {middle} between {a} and {b}", k + 1);
        }
    }

    /// A no-data or masked cell is left out of the blend, the others' weights scaled up: next to
    /// it the relief keeps its own cell's light, rather than darkening toward the hole.
    #[test]
    fn bilinear_relief_leaves_a_hidden_cell_out_of_the_blend() {
        for (nodata, mask) in [(Some(120.0), &[][..]), (None, &[120.0][..])] {
            let nearest = relief_row(Resampling::Nearest, nodata, mask, Lights::default(), false);
            let bilinear = relief_row(Resampling::Bilinear, nodata, mask, Lights::default(), false);
            // Cell 4 is hidden; cell 3's light runs on to its edge, and cell 5's from its own.
            assert!(bilinear[399..499].iter().all(Option::is_none), "{nodata:?} {mask:?}");
            assert!(bilinear[349..399].iter().all(|v| v.is_some() && *v == nearest[349]), "{nodata:?} {mask:?}");
            assert!(bilinear[499..599].iter().all(|v| v.is_some() && *v == nearest[549]), "{nodata:?} {mask:?}");
        }
    }

    /// The light cast shadows leave is blended with the hillshade: from a lit cell to a shadowed
    /// one, Bilinear dims gradually where Nearest steps.
    #[test]
    fn bilinear_relief_blends_the_cast_shadows_too() {
        // Cell 2 in full shadow in every row, the others lit.
        let light: Vec<u8> = [255, 255, 0, 255, 255, 255].repeat(3);
        let nearest = relief_row(Resampling::Nearest, None, &[], Lights { shadow: Some(&light), sky: None, water: None }, false);
        let bilinear = relief_row(Resampling::Bilinear, None, &[], Lights { shadow: Some(&light), sky: None, water: None }, false);
        let (lit, dark) = (nearest[149].unwrap(), nearest[249].unwrap());
        assert!(dark < lit && nearest[198] == Some(lit) && nearest[199] == Some(dark), "{lit} {dark}");
        assert_eq!((bilinear[149], bilinear[249]), (Some(lit), Some(dark)));
        let middle = bilinear[199].unwrap();
        assert!(dark < middle && middle < lit, "{middle} between {dark} and {lit}");
    }

    /// The ambient light the sky leaves is blended too: from a cell that sees the whole sky to one
    /// that sees none of it, Bilinear dims gradually where Nearest steps.
    #[test]
    fn bilinear_relief_blends_the_sky_too() {
        // Cell 2 sees no sky in every row, the others all of it.
        let sky: Vec<u8> = [255, 255, 0, 255, 255, 255].repeat(3);
        let light = Lights { shadow: None, sky: Some(&sky), water: None };
        let nearest = relief_row(Resampling::Nearest, None, &[], light, false);
        let bilinear = relief_row(Resampling::Bilinear, None, &[], light, false);
        let (open, hidden) = (nearest[149].unwrap(), nearest[249].unwrap());
        assert!(hidden < open && nearest[198] == Some(open) && nearest[199] == Some(hidden), "{open} {hidden}");
        assert_eq!((bilinear[149], bilinear[249]), (Some(open), Some(hidden)));
        let middle = bilinear[199].unwrap();
        assert!(hidden < middle && middle < open, "{middle} between {hidden} and {open}");
    }

    /// Where a cell is a pixel across or less, Bilinear relief draws each cell's own light, as
    /// before #1880: the blend couldn't be seen, and would work out four lights for each pixel.
    #[test]
    fn bilinear_relief_keeps_the_cells_light_when_a_cell_is_a_pixel_or_less() {
        // 40 x 3 cells of 100 m with uneven heights, seen 24 pixels wide at `res` m a pixel.
        let row = |resampling: Resampling, res: f64| {
            let gt = GeoTransform { origin_x: 0.0, origin_y: 300.0, pixel_width: 100.0, pixel_height: -100.0 };
            let data = (0..120).map(|k| (k * 37 % 11) as f32 * 10.0).collect();
            let r = Raster::new("relief", 40, 3, vec![data], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32").unwrap();
            let mut style = RasterStyle::default_for(&r);
            style.renderer = grey_relief(270.0, 45.0, 1.0, 0.0, 0.0);
            style.resampling = resampling;
            let view = View {
                width: 24.0,
                height: 1.0,
                pixels_per_point: 1.0,
                camera: Camera { center: Coord::new(2000.0, 150.0), res, bearing: 0.0, pitch: 0.0 },
                sky: false,
            };
            let mut pm = Pixmap::new(24, 1).unwrap();
            draw(&mut pm, &r, &style, None, &view, 1.0, false, Place::Flat(&Flat::Mercator, "mercator"), Lights::default());
            (0..24).map(|x| pm.pixel(x, 0).map(|p| p.red())).collect::<Vec<_>>()
        };
        for res in [100.0, 150.0] {
            assert_eq!(row(Resampling::Bilinear, res), row(Resampling::Nearest, res), "{res} m a pixel");
        }
        // Two pixels to a cell: blended.
        assert_ne!(row(Resampling::Bilinear, 50.0), row(Resampling::Nearest, 50.0));
    }

    /// A grid spanning one whole turn blends its light across the 0°/360° seam as anywhere else:
    /// the cells on either side of it are neighbours, and a pixel just across it is a fraction of
    /// a cell from the last one, not a whole turn.
    #[test]
    fn bilinear_relief_blends_across_a_whole_turns_seam() {
        // 36 x 18 cells of 10 degrees, flat; the cast shadows leave no light on column 0 (0 to
        // 10 degrees east) and all of it on the others.
        let gt = GeoTransform { origin_x: 0.0, origin_y: 90.0, pixel_width: 10.0, pixel_height: -10.0 };
        let r = Raster::new("turn", 36, 18, vec![vec![0.0; 36 * 18]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let light: Vec<u8> = (0..36 * 18).map(|k| if k % 36 == 0 { 0 } else { 255 }).collect();
        // 80 pixels of half a degree across the seam, on row 8's centre (5 degrees north).
        let view = View {
            width: 80.0,
            height: 1.0,
            pixels_per_point: 1.0,
            camera: Camera { center: lonlat_to_merc(Coord::new(0.0, 5.0)), res: 1_113_194.9 / 20.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let row = |resampling: Resampling| {
            let mut style = RasterStyle::default_for(&r);
            style.renderer = grey_relief(315.0, 45.0, 1.0, 1.0, 0.0);
            style.resampling = resampling;
            let mut pm = Pixmap::new(80, 1).unwrap();
            draw(
                &mut pm,
                &r,
                &style,
                None,
                &view,
                1.0,
                false,
                Place::Flat(&Flat::Mercator, "mercator"),
                Lights { shadow: Some(&light), sky: None, water: None },
            );
            (0..80).map(|x| pm.pixel(x, 0).unwrap().red()).collect::<Vec<_>>()
        };
        let (nearest, bilinear) = (row(Resampling::Nearest), row(Resampling::Bilinear));
        // Column 35's centre (5 degrees west) is on pixel 29.5, column 0's on 49.5; the seam lies
        // between pixels 39 and 40.
        let (lit, dark) = (nearest[29], nearest[50]);
        assert!(dark + 20 < lit && nearest[39] == lit && nearest[40] == dark, "{nearest:?}");
        let across = &bilinear[30..=49];
        assert!(across.windows(2).all(|w| w[0] >= w[1]), "darkens steadily across the seam: {across:?}");
        for x in [39, 40] {
            assert!(dark + 10 < bilinear[x] && bilinear[x] + 10 < lit, "pixel {x} blends: {across:?}");
        }
    }
}
