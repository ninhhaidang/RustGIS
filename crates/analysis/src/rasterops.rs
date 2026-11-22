//! Raster operations: terrain (slope, aspect, hillshade), band arithmetic, remapping, resampling
//! onto another grid, weighted overlay, viewsheds, lines of sight, profiles, IDW interpolation,
//! k-means classification and change detection. Outputs are new pixel arrays on a raster's grid.

use std::ops::Range;

use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, Transformer};
use rustgis_geom::{Coord, Extent};

/// The no-data value written by these operations.
pub const NODATA: f32 = -3.4e38;
/// The fewest pixels [`max_pixels`] allows, and its limit in the browser.
pub const PIXELS_FLOOR: usize = 64_000_000;
/// The most [`max_pixels`] allows, however much memory the computer has (2 GB a band).
pub const PIXELS_CEILING: usize = 512_000_000;

/// Largest output raster built (pixels): on the desktop, as many as a quarter of the computer's
/// memory holds at 16 bytes each (a tool holds its input, its output and a working grid or two),
/// from [`PIXELS_FLOOR`] up to [`PIXELS_CEILING`] (#1291 step 2).
/// Less once RustGIS holds more than half the memory, never below the floor ([`rustgis_data::memory::planned`], #2431).
pub fn max_pixels() -> usize {
    rustgis_data::memory::planned(PIXELS_FLOOR, PIXELS_CEILING, 16)
}

/// Room for a grid of `w × h` cells of `T` (an output band, or a working grid of one), empty: a
/// grid the machine can't hold is an error saying so, instead of ending the program
/// ([`rustgis_data::raster::grid`]).
pub fn grid<T>(w: usize, h: usize) -> Result<Vec<T>, String> {
    rustgis_data::raster::grid(w, h).map_err(|e| e.to_string())
}

/// A `w × h` band filled with `fill` ([`grid`]).
pub fn new_band<T: Clone>(w: usize, h: usize, fill: T) -> Result<Vec<T>, String> {
    rustgis_data::raster::filled(w, h, fill).map_err(|e| e.to_string())
}

/// A new single- or multi-band raster on `r`'s grid.
pub fn derive(r: &Raster, name: &str, bands: Vec<Vec<f32>>) -> Result<Raster, String> {
    r.require_coordinates()?;
    Raster::new(name, r.width, r.height, bands, r.transform, r.crs, Some(NODATA as f64), ColorHint::Gray, "Float32")
        .map_err(|e| e.to_string())
}

/// [`derive`] for a [`hillshade`]'s pixels: one band of whole numbers 1–255 stored as bytes
/// (`UInt8`), with 0 as no-data, so it is written as 8-bit samples, not 32-bit floats.
pub fn derive_hillshade(r: &Raster, name: &str, mut shade: Vec<f32>) -> Result<Raster, String> {
    r.require_coordinates()?;
    for v in &mut shade {
        if !v.is_finite() || *v < 1.0 {
            *v = 0.0;
        }
    }
    Raster::new(name, r.width, r.height, vec![shade], r.transform, r.crs, Some(0.0), ColorHint::Gray, "UInt8").map_err(|e| e.to_string())
}

fn band(r: &Raster, b: usize) -> Result<&[f32], String> {
    r.bands.get(b).map(|b| b.data.as_slice()).ok_or_else(|| format!("{} has no band {}", r.name, b + 1))
}

/// Elevation at (col, row), clamped to the grid edge; `None` for no-data.
fn z(r: &Raster, data: &[f32], col: i64, row: i64) -> Option<f32> {
    let c = col.clamp(0, r.width as i64 - 1) as usize;
    let rr = row.clamp(0, r.height as i64 - 1) as usize;
    data.get(rr * r.width as usize + c).copied().filter(|v| r.is_valid(*v))
}

/// The surface gradient (dz/dx east, dz/dy north) at a cell (Horn's method), in metres per metre.
fn gradient(r: &Raster, data: &[f32], col: i64, row: i64, zf: f64, (cx, cy): (f64, f64)) -> Option<(f64, f64)> {
    let g = |dc: i64, dr: i64| z(r, data, col + dc, row + dr).or_else(|| z(r, data, col, row)).map(|v| v as f64 * zf);
    let (a, b, c) = (g(-1, -1)?, g(0, -1)?, g(1, -1)?);
    let (d, f) = (g(-1, 0)?, g(1, 0)?);
    let (gg, h, i) = (g(-1, 1)?, g(0, 1)?, g(1, 1)?);
    let dzdx = ((c + 2.0 * f + i) - (a + 2.0 * d + gg)) / (8.0 * cx);
    // In a north-up grid rows run south, so the difference is top minus bottom.
    let dzdy = ((a + 2.0 * b + c) - (gg + 2.0 * h + i)) / (8.0 * cy);
    // A grid stored bottom up (a positive pixel height) or right to left runs the other way.
    Some((dzdx * r.transform.pixel_width.signum(), dzdy * -r.transform.pixel_height.signum()))
}

/// `f` of each of `r`'s cells, no-data where it gives none. `cost` is a cell's work in
/// [`crate::parallel::MIN_ROW_WORK`]'s units (some 30 ns each).
fn per_cell(r: &Raster, cost: usize, f: impl Fn(i64, i64) -> Option<f64> + Sync) -> Result<Vec<f32>, String> {
    per_cell_on(None, r, cost, f)
}

/// [`per_cell`] on `threads` threads (none: as many as pay off). Each cell writes only itself, so
/// rows go on several threads (#1252).
fn per_cell_on(threads: Option<usize>, r: &Raster, cost: usize, f: impl Fn(i64, i64) -> Option<f64> + Sync) -> Result<Vec<f32>, String> {
    let (w, h) = (r.width as usize, r.height as usize);
    let mut out = new_band(w, h, NODATA)?;
    if w == 0 {
        return Ok(out);
    }
    let threads = threads.unwrap_or_else(|| crate::parallel::threads_for_rows(h, w.saturating_mul(h).saturating_mul(cost), 0));
    crate::control::begin(h);
    crate::parallel::for_each_row(
        out.chunks_mut(w),
        threads,
        || (),
        |_, row, line: &mut [f32]| {
            for (col, slot) in line.iter_mut().enumerate() {
                *slot = f(col as i64, row as i64).filter(|v| v.is_finite()).map(|v| v as f32).unwrap_or(NODATA);
            }
        },
    )?;
    Ok(out)
}

/// `f` of each of `n` cells by index, `chunk` cells at a time on as many threads as pay off, each
/// with its own `scratch()`: a cell writes only itself. `cost` is a cell's work as in
/// [`per_cell`]; the chunks count toward the run's progress, and a cancelled run stops (#1291).
pub fn per_index<S>(
    n: usize,
    chunk: usize,
    cost: usize,
    scratch: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, usize) -> f32 + Sync,
) -> Result<Vec<f32>, String> {
    let mut out = new_band(n, 1, NODATA)?;
    let chunk = chunk.max(1);
    let lines = n.div_ceil(chunk);
    let threads = crate::parallel::threads_for_rows(lines, n.saturating_mul(cost), 0);
    crate::control::begin(lines);
    crate::parallel::for_each_row(out.chunks_mut(chunk), threads, scratch, |s, line, cells: &mut [f32]| {
        for (k, slot) in cells.iter_mut().enumerate() {
            *slot = f(s, line * chunk + k);
        }
    })?;
    Ok(out)
}

/// The cells [`weighted_overlay`] makes at a time: its criteria carry no grid, so no rows.
const OVERLAY_CHUNK: usize = 1 << 14;

/// Draw the interior from three borrowed rows; edges and no-data keep the scalar path.
fn terrain(r: &Raster, data: &[f32], zf: f64, f: impl Fn(f64, f64) -> f64 + Sync) -> Result<Vec<f32>, String> {
    terrain_on(None, r, data, zf, f)
}

/// [`terrain`] on `threads` threads (none: as many as pay off). Each output row is made from the
/// three input rows around it and written where it lies, so rows go on several threads (#1252).
fn terrain_on(threads: Option<usize>, r: &Raster, data: &[f32], zf: f64, f: impl Fn(f64, f64) -> f64 + Sync) -> Result<Vec<f32>, String> {
    let (w, h) = (r.width as usize, r.height as usize);
    let cell = r.cell_size_m();
    let scalar = |c, row| {
        z(r, data, c, row)?;
        let (x, y) = gradient(r, data, c, row, zf, cell)?;
        Some(f(x, y))
    };
    let Some(len) = w.checked_mul(h) else { return per_cell(r, 1, scalar) };
    if w < 3 || h < 3 || data.len() != len {
        return per_cell(r, 1, scalar);
    }
    let value = |v: Option<f64>| v.filter(|v| v.is_finite()).map(|v| v as f32).unwrap_or(NODATA);
    let mut out = new_band(w, h, NODATA)?;
    // A cell's gradient and its function cost about one unit of the threshold's (some 30 ns).
    let threads = threads.unwrap_or_else(|| crate::parallel::threads_for_rows(h, len, 0));
    crate::control::begin(h);
    crate::parallel::for_each_row(
        out.chunks_mut(w),
        threads,
        || (),
        |_, row, line: &mut [f32]| {
            // The first and last rows, and each row's first and last cells, take the edge path.
            let edge = |col: usize| value(scalar(col as i64, row as i64));
            let (Some(top), Some(mid), Some(bottom)) = (
                row.checked_sub(1).and_then(|t| data.get(t * w..(t + 1) * w)),
                data.get(row * w..(row + 1) * w),
                data.get((row + 1) * w..(row + 2) * w),
            ) else {
                for (col, o) in line.iter_mut().enumerate() {
                    *o = edge(col);
                }
                return;
            };
            for (col, o) in line.iter_mut().enumerate() {
                *o = if col == 0 || col + 1 == w {
                    edge(col)
                } else {
                    let at = |s: &[f32], dc: usize| s.get(col + dc - 1).copied().unwrap_or(f32::NAN);
                    let ([a, b, c], [d, center, f0], [g, hh, i]) =
                        ([0, 1, 2].map(|k| at(top, k)), [0, 1, 2].map(|k| at(mid, k)), [0, 1, 2].map(|k| at(bottom, k)));
                    if !r.is_valid(center) {
                        NODATA
                    } else if [a, b, c, d, f0, g, hh, i].iter().all(|v| r.is_valid(*v)) {
                        let [a, b, c, d, f0, g, hh, i] = [a, b, c, d, f0, g, hh, i].map(|v| f64::from(v) * zf);
                        let x = ((c + 2.0 * f0 + i) - (a + 2.0 * d + g)) / (8.0 * cell.0) * r.transform.pixel_width.signum();
                        let y = ((a + 2.0 * b + c) - (g + 2.0 * hh + i)) / (8.0 * cell.1) * -r.transform.pixel_height.signum();
                        value(Some(f(x, y)))
                    } else {
                        edge(col)
                    }
                };
            }
        },
    )?;
    Ok(out)
}

/// Slope in degrees.
pub fn slope(r: &Raster, b: usize, zf: f64) -> Result<Vec<f32>, String> {
    let d = band(r, b)?;
    terrain(r, d, zf, |gx, gy| gx.hypot(gy).atan().to_degrees())
}

/// Slope as percent rise: 100 times the gradient magnitude, equivalent to 100 × tan(angle in radians).
/// Uses the same Horn gradient, Z factor and no-data handling as [`slope`], without rounding
/// through degree output first. Refuses output beyond Float32's range with a cell count.
pub fn slope_percent(r: &Raster, b: usize, zf: f64) -> Result<Vec<f32>, String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let d = band(r, b)?;
    let overflow = AtomicUsize::new(0);
    let pixels = terrain(r, d, zf, |gx, gy| {
        let value = 100.0 * gx.hypot(gy);
        if !value.is_finite() || value > f64::from(f32::MAX) {
            overflow.fetch_add(1, Ordering::Relaxed);
        }
        value
    })?;
    let n = overflow.load(Ordering::Relaxed);
    if n > 0 { Err(format!("{n} percent slope cells exceed the Float32 range")) } else { Ok(pixels) }
}

/// Aspect: the compass direction the slope faces, in degrees (flat cells are -1).
pub fn aspect(r: &Raster, b: usize) -> Result<Vec<f32>, String> {
    let d = band(r, b)?;
    terrain(r, d, 1.0, |gx, gy| if gx.abs() < 1e-12 && gy.abs() < 1e-12 { -1.0 } else { (-gx).atan2(-gy).to_degrees().rem_euclid(360.0) })
}

/// Hillshade (1–255) lit from `azimuth` and `altitude` degrees: a cell in full shadow is 1, so
/// that 0 is left for the no-data of the bytes [`derive_hillshade`] makes of it.
pub fn hillshade(r: &Raster, b: usize, azimuth: f64, altitude: f64, zf: f64) -> Result<Vec<f32>, String> {
    let d = band(r, b)?;
    let (zen, az) = ((90.0 - altitude).to_radians(), azimuth.to_radians());
    let (cos_zen, sin_zen) = (zen.cos(), zen.sin());
    terrain(r, d, zf, |gx, gy| {
        let slope = gx.hypot(gy).atan();
        let aspect = (-gx).atan2(-gy);
        let v = cos_zen * slope.cos() + sin_zen * slope.sin() * (az - aspect).cos();
        (v.max(0.0) * 255.0).round().max(1.0)
    })
}

/// A band arithmetic expression over `b1`, `b2`… (+ - * / and parentheses, numbers).
pub fn band_math(r: &Raster, expr: &str) -> Result<Vec<f32>, String> {
    band_math_on(None, r, expr)
}

/// [`band_math`] on `threads` threads (none: as many as pay off): each cell on its own, so rows go
/// on several threads (#1252), each with its own band values.
fn band_math_on(threads: Option<usize>, r: &Raster, expr: &str) -> Result<Vec<f32>, String> {
    let e = Expr::parse(expr, r.band_count())?;
    let (w, h) = (r.width as usize, r.height as usize);
    let mut out = new_band(w, h, NODATA)?;
    if w == 0 {
        return Ok(out);
    }
    // A cell reads every band and evaluates the expression: about a unit a band.
    let work = w.saturating_mul(h).saturating_mul(r.band_count().max(1));
    let threads = threads.unwrap_or_else(|| crate::parallel::threads_for_rows(h, work, 0));
    crate::control::begin(h);
    crate::parallel::for_each_row(
        out.chunks_mut(w),
        threads,
        || vec![0.0f64; r.band_count()],
        |vals, row, line: &mut [f32]| {
            for (col, slot) in line.iter_mut().enumerate() {
                let i = row * w + col;
                let mut ok = true;
                for (bi, b) in r.bands.iter().enumerate() {
                    let v = b.data.get(i).copied().unwrap_or(NODATA);
                    if !r.is_valid(v) {
                        ok = false;
                        break;
                    }
                    if let Some(slot) = vals.get_mut(bi) {
                        *slot = v as f64;
                    }
                }
                let v = if ok { e.eval(vals) } else { f64::NAN };
                *slot = if v.is_finite() { v as f32 } else { NODATA };
            }
        },
    )?;
    Ok(out)
}

/// A remap's cells, and its ranges whose low end isn't below the high end, which match nothing.
pub struct Remap {
    pub cells: Vec<f32>,
    pub empty: Vec<(f64, f64)>,
}

/// Remaps value ranges `[lo, hi)` to new values; other values become no-data unless `keep`.
/// A range whose low end isn't below its high end matches nothing (saved scripts and models
/// rely on what a range matches); it is listed in [`Remap::empty`] for [`empty_ranges_note`].
pub fn remap(r: &Raster, b: usize, ranges: &[(f64, f64, f64)], keep: bool) -> Result<Remap, String> {
    let d = band(r, b)?;
    let n = r.width as usize * r.height as usize;
    // A lookup in the ranges: about a unit a cell.
    let cells = per_index(
        n,
        r.width as usize,
        1,
        || (),
        |_, i| {
            let Some(v) = d.get(i).copied().filter(|v| r.is_valid(*v)) else { return NODATA };
            let x = v as f64;
            match ranges.iter().find(|(lo, hi, _)| x >= *lo && x < *hi) {
                Some((_, _, new)) => *new as f32,
                None if keep => v,
                None => NODATA,
            }
        },
    )?;
    let empty = ranges.iter().filter(|(lo, hi, _)| lo >= hi).map(|(lo, hi, _)| (*lo, *hi)).collect();
    Ok(Remap { cells, empty })
}

/// The one notice for remap ranges that match nothing, each with the raster it was given for,
/// or none when there are none. Where a range has equal whole ends (a class code), it shows the
/// range that picks that value.
pub fn empty_ranges_note(empty: &[(String, f64, f64)]) -> Option<String> {
    let mut by_raster: Vec<(&str, Vec<String>)> = Vec::new();
    for (name, lo, hi) in empty {
        let range = format!("[{lo}, {hi}]");
        match by_raster.iter_mut().find(|(n, _)| n == name) {
            Some((_, ranges)) => ranges.push(range),
            None => by_raster.push((name, vec![range])),
        }
    }
    let listed: Vec<String> = by_raster.iter().map(|(name, ranges)| format!("{} in {name}", ranges.join(", "))).collect();
    let what = match empty.len() {
        0 => return None,
        1 => "1 remap range matches".to_string(),
        n => format!("{n} remap ranges match"),
    };
    // Past 2^53 a Double has no whole number between `lo` and `lo + 1`.
    let pick = empty
        .iter()
        .find(|(_, lo, hi)| lo == hi && lo.fract() == 0.0 && lo.abs() < 9_007_199_254_740_992.0)
        .map(|(_, lo, _)| format!(", so [{lo}, {}] picks {lo}", lo + 1.0))
        .unwrap_or_default();
    Some(format!(
        "{what} nothing ({}): a range takes values from its low end up to, but not including, its high end{pick}",
        listed.join("; ")
    ))
}

/// Whether `cells`, rows of `width`, hold a cell with a value; looks at Cancel row by row, so a
/// large result with none still stops.
pub fn has_values(cells: &[f32], width: usize) -> Result<bool, String> {
    for row in cells.chunks(width.max(1)) {
        crate::control::check()?;
        if row.iter().any(|v| v.is_finite() && *v != NODATA) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `band`'s values resampled (nearest neighbour) onto `target`'s grid, through longitude/latitude.
pub fn resample_to(src: &Raster, b: usize, target: &Raster) -> Result<Vec<f32>, String> {
    src.require_coordinates()?;
    target.require_coordinates()?;
    band(src, b)?;
    if src.width == target.width && src.height == target.height && src.transform == target.transform && src.crs == target.crs {
        let d = band(src, b)?;
        let mut out = grid(src.width as usize, src.height as usize)?;
        out.extend(d.iter().map(|v| if src.is_valid(*v) { *v } else { NODATA }));
        return Ok(out);
    }
    // A cell goes to longitude/latitude and back onto the source: some ten units.
    per_cell(target, 10, |c, row| {
        let w = target.transform.to_world(c as f64 + 0.5, row as f64 + 0.5);
        let ll = target.transformer().to_lonlat(w)?;
        let (sc, sr) = src.pixel_at(ll)?;
        src.value(b, sc, sr).map(f64::from)
    })
}

/// A weighted sum of criteria (each already scored, e.g. 1–10) on one grid; no-data anywhere
/// gives no-data. Weights are normalised to sum to 1. Inputs mark no-data with [`NODATA`] (as
/// `resample_to`, `remap` and the other functions here do).
pub fn weighted_overlay(criteria: &[(Vec<f32>, f64)]) -> Result<Vec<f32>, String> {
    let n = criteria.first().map(|c| c.0.len()).ok_or("give at least one criterion")?;
    if criteria.iter().any(|c| c.0.len() != n) {
        return Err("criteria must share a grid".into());
    }
    let total: f64 = criteria.iter().map(|c| c.1.max(0.0)).sum();
    if total <= 0.0 {
        return Err("the weights must add up to more than zero".into());
    }
    per_index(
        n,
        OVERLAY_CHUNK,
        criteria.len(),
        || (),
        |_, i| {
            let mut s = 0.0;
            for (vals, w) in criteria {
                let v = vals.get(i).copied().unwrap_or(NODATA);
                if v == NODATA || !v.is_finite() {
                    return NODATA;
                }
                s += v as f64 * w.max(0.0) / total;
            }
            s as f32
        },
    )
}

/// Metres between two longitude/latitude points.
fn dist_m(a: Coord, b: Coord) -> f64 {
    rustgis_geom::haversine_m(a, b)
}

/// Elevation (bilinear) at a longitude/latitude point.
pub fn elevation_at(r: &Raster, b: usize, ll: Coord) -> Option<f64> {
    let w = r.transformer().from_lonlat(ll)?;
    let (x, y) = r.transform.to_pixel(w);
    let (x, y) = (x - 0.5, y - 0.5);
    let (c0, r0) = (x.floor() as i64, y.floor() as i64);
    let (fx, fy) = (x - c0 as f64, y - r0 as f64);
    let v = |c: i64, rr: i64| r.value(b, c, rr).map(f64::from);
    let (a, b2, c, d) = (v(c0, r0), v(c0 + 1, r0), v(c0, r0 + 1), v(c0 + 1, r0 + 1));
    match (a, b2, c, d) {
        (Some(a), Some(b2), Some(c), Some(d)) => {
            Some(a * (1.0 - fx) * (1.0 - fy) + b2 * fx * (1.0 - fy) + c * (1.0 - fx) * fy + d * fx * fy)
        }
        _ => v(x.round() as i64, y.round() as i64),
    }
}

/// An elevation profile along a path: (distance from the start in metres, point, elevation).
pub fn profile(r: &Raster, b: usize, path: &[Coord], samples: usize) -> Vec<(f64, Coord, Option<f64>)> {
    let samples = samples.clamp(2, 10_000);
    let legs: Vec<f64> = path
        .windows(2)
        .map(|w| match w {
            [a, c] => dist_m(*a, *c),
            _ => 0.0,
        })
        .collect();
    let total: f64 = legs.iter().sum();
    if total <= 0.0 {
        return path.first().map(|p| vec![(0.0, *p, elevation_at(r, b, *p))]).unwrap_or_default();
    }
    // Samples are in order along the path, so the legs are walked once.
    let mut out = Vec::with_capacity(samples);
    let (mut li, mut acc) = (0usize, 0.0);
    for i in 0..samples {
        let d = total * i as f64 / (samples - 1) as f64;
        while li + 1 < legs.len() && d > acc + legs.get(li).copied().unwrap_or(0.0) {
            acc += legs.get(li).copied().unwrap_or(0.0);
            li += 1;
        }
        let len = legs.get(li).copied().unwrap_or(0.0);
        let t = if len > 0.0 { ((d - acc) / len).clamp(0.0, 1.0) } else { 0.0 };
        let at = match (path.get(li), path.get(li + 1)) {
            (Some(a), Some(c)) => Coord::new(a.x + (c.x - a.x) * t, a.y + (c.y - a.y) * t),
            _ => path.last().copied().unwrap_or_default(),
        };
        out.push((d, at, elevation_at(r, b, at)));
    }
    out
}

/// Earth's radius as visible light sees it, bent by the standard atmospheric refraction of 1/7:
/// `gdal_viewshed`'s default curvature coefficient of 6/7. The 4/3 Earth is for radio waves.
const SIGHT_RADIUS_M: f64 = 6_371_000.0 * 7.0 / 6.0;

/// Whether `b` is visible from `a` (observer and target heights above the ground, in metres),
/// and each profile sample with whether it can be seen. Earth curvature is included, with the
/// refraction of visible light (see `SIGHT_RADIUS_M`), not the 4/3 Earth of radio links.
pub fn line_of_sight(
    r: &Raster,
    band: usize,
    a: Coord,
    b: Coord,
    ha: f64,
    hb: f64,
    samples: usize,
) -> Option<(bool, Vec<(f64, Coord, Option<f64>, bool)>)> {
    let prof = profile(r, band, &[a, b], samples);
    let (_, _, za) = *prof.first()?;
    let eye = za? + ha;
    let drop = |d: f64| d * d / (2.0 * SIGHT_RADIUS_M);
    let mut max_angle = f64::NEG_INFINITY;
    let mut out = Vec::with_capacity(prof.len());
    let mut target_visible = false;
    for (i, (d, p, zz)) in prof.iter().enumerate() {
        let last = i + 1 == prof.len();
        let h = zz.map(|z| z - drop(*d) + if last { hb } else { 0.0 });
        let visible = match h {
            Some(h) if *d > 0.0 => {
                let ang = (h - eye) / d;
                let v = ang >= max_angle;
                if !last || hb == 0.0 {
                    max_angle = max_angle.max(ang);
                }
                v
            }
            Some(_) => true,
            None => false,
        };
        if last {
            target_visible = visible;
        }
        out.push((*d, *p, *zz, visible));
    }
    Some((target_visible, out))
}

/// Most ray steps a viewshed takes, and cell-point pairs an interpolation weighs, on one thread
/// (each about a second or two); a run on several threads may take as many times more, and one
/// that can be cancelled any amount ([`crate::parallel::work_cap`]).
const MAX_VIEWSHED_WORK: usize = 400_000_000;
pub(crate) const MAX_IDW_WORK: usize = 400_000_000;

/// How a viewshed reads the ground between the observer and a cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Heights {
    /// The cell each point of the ray falls in, as the viewshed has read it (each point now exact on
    /// a cell's centre line, which can change a cell at a tie). On ground that bulges
    /// (a hill seen from its top), a cell nearer the observer than the point stands in for it and
    /// can hide cells just inside the horizon.
    #[default]
    Nearest,
    /// Between the cells' centres, in proportion to how near the point is to each (bilinear):
    /// smoother, and closer on bulging ground, though still an estimate. Where one of those cells
    /// has no data, the cell the point falls in, as [`Heights::Nearest`] reads it (counted:
    /// [`Viewshed::read_nearest`]).
    Bilinear,
}

impl Heights {
    /// The one named `name` (`nearest`, `bilinear`, in any case); `None` for any other.
    pub fn named(name: &str) -> Option<Heights> {
        if name.eq_ignore_ascii_case("nearest") {
            Some(Heights::Nearest)
        } else if name.eq_ignore_ascii_case("bilinear") {
            Some(Heights::Bilinear)
        } else {
            None
        }
    }

    /// The ground at a point of a ray, `(col, row)` in cells (whole numbers at cell centres), and
    /// whether it is the nearest cell's, read in place of cells between which a neighbour has
    /// no data. `None` where the point's own cell has no data: between cells, a neighbour weighed
    /// with no data falls back to that cell, so `None` there too only when it has none.
    fn at(self, r: &Raster, b: usize, col: f64, row: f64) -> Option<(f64, bool)> {
        let (nc, nr) = (col.round() as i64, row.round() as i64);
        let nearest = |fell_back: bool| r.value(b, nc, nr).map(|v| (f64::from(v), fell_back));
        if self == Heights::Nearest {
            return nearest(false);
        }
        let (c0, r0) = (col.floor(), row.floor());
        let (tx, ty) = (col - c0, row - r0);
        let (c0, r0) = (c0 as i64, r0 as i64);
        let mut sum = 0.0f64;
        // A neighbour weighing nothing (the point on its row or column) isn't read, so a point
        // on a cell's centre line needs only the two cells it lies between.
        for (dc, dr, wgt) in [(0, 0, (1.0 - tx) * (1.0 - ty)), (1, 0, tx * (1.0 - ty)), (0, 1, (1.0 - tx) * ty), (1, 1, tx * ty)] {
            if wgt == 0.0 {
                continue;
            }
            match r.value(b, c0 + dc, r0 + dr) {
                Some(v) => sum += wgt * f64::from(v),
                None => return nearest(true),
            }
        }
        Some((sum, false))
    }
}

/// A viewshed: each cell visible (1) or not (0), no-data outside the radius, and how many cells'
/// rays read the ground less than fully.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Viewshed {
    pub cells: Vec<f32>,
    /// Cells seen across ground with no data, taken as hiding nothing there.
    pub through_gaps: usize,
    /// Cells whose ray, read [`Heights::Bilinear`], took the nearest cell's height somewhere,
    /// a cell it would read between having no data.
    pub read_nearest: usize,
}

/// The ray steps a viewshed from `(oc, or)` takes over the window `cols` × `rows` (inclusive): for
/// each cell no farther than `radius_m` (cell centres, `cx` × `cy` metres apart, as the viewshed
/// measures them), its ray's steps, the larger of its column and row offsets. A row's cells within
/// the radius are a run of columns, whose sum has a closed form, so the count is one step a row.
/// It is the sum of the rays' full lengths, so the most steps the viewshed takes: a ray stops
/// where the ground first hides its cell, and a cell with no data walks none (finding out which
/// have none would read them all).
fn ray_steps(observer: (i64, i64), cols: (i64, i64), rows: (i64, i64), cell: (f64, f64), radius_m: f64) -> u128 {
    let ((oc, or), (left, right), (first, last), (cx, cy)) = (observer, cols, rows, cell);
    // Σ max(x, a) for x in 0..=n.
    let up_to = |n: i128, a: i128| -> i128 {
        if n < 0 {
            0
        } else if n <= a {
            (n + 1) * a
        } else {
            (a + 1) * a + (n * (n + 1) - a * (a + 1)) / 2
        }
    };
    let within = |dc: i64, dr: i64| (dc as f64 * cx).hypot(dr as f64 * cy) <= radius_m;
    let mut total: i128 = 0;
    for row in first..=last {
        let dr = row - or;
        if !within(0, dr) {
            continue;
        }
        // The farthest column offset within the radius on this row, at most the window's, worked
        // out in cells (so a radius near f64's range doesn't overflow when squared) and put right
        // at the edge: the corrections only undo rounding, and stay within the window's width.
        let span = (oc - left).max(right - oc);
        let (rc, yc) = (radius_m / cx, dr as f64 * cy / cx);
        let reach = ((rc - yc) * (rc + yc)).max(0.0).sqrt().floor();
        let mut k = reach.clamp(0.0, span as f64) as i64;
        while k < span && within(k + 1, dr) {
            k += 1;
        }
        while k > 0 && !within(k, dr) {
            k -= 1;
        }
        // The run's offsets from the observer's column, within the window: the observer is in
        // it, so the run takes in its column (offset 0, counted once).
        let (west, east) = (i128::from(oc - (oc - k).max(left)), i128::from((oc + k).min(right) - oc));
        let a = i128::from(dr.abs());
        total += up_to(west, a) + up_to(east, a) - a;
    }
    // (Never negative; were it, the cap refuses the run rather than letting it through.)
    u128::try_from(total).unwrap_or(u128::MAX)
}

/// Cells visible from an observer (1) or not (0) within `radius_m`; outside the radius no-data.
/// The Earth curves as visible light sees it (`SIGHT_RADIUS_M`). The ground along each ray is read
/// as `heights` says; ground with no data hides nothing, and the visible cells whose rays crossed
/// some are counted ([`Viewshed::through_gaps`]).
pub fn viewshed(r: &Raster, b: usize, observer: Coord, height: f64, radius_m: f64, heights: Heights) -> Result<Viewshed, String> {
    viewshed_on(None, r, b, observer, height, radius_m, heights)
}

/// [`viewshed`] on `threads` threads, or as many as its work calls for.
fn viewshed_on(
    threads: Option<usize>,
    r: &Raster,
    b: usize,
    observer: Coord,
    height: f64,
    radius_m: f64,
    heights: Heights,
) -> Result<Viewshed, String> {
    let (oc, or) = r.pixel_at(observer).ok_or("the observer is outside the raster")?;
    let z0 = r.value(b, oc, or).ok_or("the observer stands on no-data")? as f64 + height;
    let (cx, cy) = r.cell_size_m();
    if !(cx > 0.0 && cy > 0.0 && radius_m.is_finite() && radius_m > 0.0) {
        return Err("the raster's cells or the radius have no size".into());
    }
    // In cells (saturating: a radius of more cells than an i64 holds still reaches past the
    // raster). Only the radius's cells within the raster are worked out, so a radius reaching
    // past it (the whole DEM's diagonal, by default) costs no more than the raster, which is
    // already in memory; the time is bounded by the ray steps below.
    let rad_c = (radius_m / cx).ceil() as i64;
    let rad_r = (radius_m / cy).ceil() as i64;
    let (first, last) = (or.saturating_sub(rad_r).max(0), or.saturating_add(rad_r).min(r.height as i64 - 1));
    let (left, right) = (oc.saturating_sub(rad_c).max(0), oc.saturating_add(rad_c).min(r.width as i64 - 1));
    // A ray steps a whole cell along its longer axis, so a bilinear point lies on a cell's
    // centre line and reads the two cells it falls between ([`Heights::at`] skips a neighbour
    // that weighs nothing; next to no data, the nearest cell too) where a nearest one reads one:
    // weighed as two.
    let per_step = if heights == Heights::Bilinear { 2 } else { 1 };
    // Each cell within the radius walks a ray back to the observer, one step a cell along its
    // longer axis: the steps counted exactly, a row at a time, not estimated.
    let ray_work = usize::try_from(ray_steps((oc, or), (left, right), (first, last), (cx, cy), radius_m).saturating_mul(per_step as u128))
        .unwrap_or(usize::MAX);
    // Each cell walks its own ray and writes only itself, so the rows within the radius are made
    // on several threads when the work is large (#1252), and may take as many times the work.
    let rows = usize::try_from(last - first + 1).unwrap_or(0);
    let threads = threads.unwrap_or_else(|| crate::parallel::threads_for_rows(rows, ray_work, 0));
    let cap = crate::parallel::work_cap(MAX_VIEWSHED_WORK, threads);
    if ray_work > cap {
        // A radius reaching the raster's farthest corner covers all of it (as with no radius
        // given, the raster's diagonal): said as the whole raster, not as a radius to shorten.
        let corner = ((oc.max(r.width as i64 - 1 - oc)) as f64 * cx).hypot((or.max(r.height as i64 - 1 - or)) as f64 * cy);
        let (what, fix) = if radius_m >= corner {
            ("the whole raster", "give a radius to cover only part of it")
        } else {
            ("the radius", "use a smaller radius")
        };
        return Err(format!(
            "{what} takes up to {ray_work} ray steps{}, more than the {cap} this computer takes on {threads} thread(s); {fix}{}",
            if per_step > 1 { " (a step between cells counted as two)" } else { "" },
            crate::parallel::UNCAPPED_HINT
        ));
    }
    let w = r.width as usize;
    let mut out = new_band(w, r.height as usize, NODATA)?;
    let window = out.chunks_mut(w).skip(usize::try_from(first).unwrap_or(0)).take(rows);
    // Summed across threads: the same counts on any number.
    let (through_gaps, read_nearest) = (std::sync::atomic::AtomicUsize::new(0), std::sync::atomic::AtomicUsize::new(0));
    crate::control::begin(rows);
    crate::parallel::for_each_row(window, threads, crate::control::Pace::default, |pace, i, line: &mut [f32]| {
        let row = first + i as i64;
        for col in left..=right {
            let (dx, dy) = ((col - oc) as f64 * cx, (row - or) as f64 * cy);
            let dist = dx.hypot(dy);
            if dist > radius_m {
                continue;
            }
            let Some(zt) = r.value(b, col, row) else { continue };
            if dist == 0.0 {
                if let Some(o) = line.get_mut(col as usize) {
                    *o = 1.0;
                }
                continue;
            }
            let target = (zt as f64 - dist * dist / (2.0 * SIGHT_RADIUS_M) - z0) / dist;
            // Walk the ray from the observer; any cell higher than the line hides the target.
            let steps = ((col - oc).abs().max((row - or).abs())) as usize;
            // A wide window's row takes long without a cap: Cancel is looked at within it.
            if pace.stop(steps) {
                return;
            }
            let mut visible = true;
            let (mut gap, mut nearest) = (false, false);
            // Along the longer axis the ray steps a whole cell at a time; along the other it falls
            // between cells. Both are one division of whole numbers, so exact wherever the point
            // lies on a cell's centre line (and no neighbour is weighed by a rounding error).
            let (dc, dr) = (col - oc, row - or);
            let along = |o: i64, d: i64, s: usize| o as f64 + (d * s as i64) as f64 / steps as f64;
            for s in 1..steps {
                let t = s as f64 / steps as f64;
                let (c, rr) = (along(oc, dc, s), along(or, dr, s));
                let Some((zi, fell_back)) = heights.at(r, b, c, rr) else {
                    gap = true;
                    continue;
                };
                nearest |= fell_back;
                let di = dist * t;
                if (zi - di * di / (2.0 * SIGHT_RADIUS_M) - z0) / di > target {
                    visible = false;
                    break;
                }
            }
            // A hidden cell stays hidden whatever the missing ground is: only those seen count.
            if gap && visible {
                through_gaps.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            if nearest {
                read_nearest.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(o) = line.get_mut(col as usize) {
                *o = if visible { 1.0 } else { 0.0 };
            }
        }
    })?;
    Ok(Viewshed { cells: out, through_gaps: through_gaps.into_inner(), read_nearest: read_nearest.into_inner() })
}

/// Inverse distance weighted interpolation of point values onto a grid in `crs`, from each cell's
/// `neighbours` nearest points, or from every point (as QGIS does) when it is 0. The points,
/// extent and cell size are in `crs`: degrees for longitude/latitude, where east-west distances
/// shrink with latitude, or the projected system's units, where they are measured as they are.
pub fn idw(
    points: &[(Coord, f64)],
    extent: &Extent,
    cell: f64,
    power: f64,
    neighbours: usize,
    name: &str,
    crs: Crs,
) -> Result<Raster, String> {
    idw_on(None, points, extent, cell, power, neighbours, name, crs)
}

/// [`idw`] on `threads` threads, or as many as its work calls for.
fn idw_on(
    threads: Option<usize>,
    points: &[(Coord, f64)],
    extent: &Extent,
    cell: f64,
    power: f64,
    neighbours: usize,
    name: &str,
    crs: Crs,
) -> Result<Raster, String> {
    if points.len() < 2 {
        return Err("interpolation needs at least two points with values".into());
    }
    if !(cell.is_finite() && cell > 0.0) {
        return Err("the cell size must be positive".into());
    }
    if extent.is_empty() {
        return Err("the area to interpolate over is empty".into());
    }
    let (w, h) = ((extent.width() / cell).ceil().max(1.0) as usize, (extent.height() / cell).ceil().max(1.0) as usize);
    if w.saturating_mul(h) > max_pixels() / 4 {
        return Err(format!("{w} × {h} cells is too many, more than this computer's {}; use a larger cell size", max_pixels() / 4));
    }
    if !(power.is_finite() && power >= 0.0) {
        return Err("the distance coefficient must be zero or more".into());
    }
    let k = if neighbours == 0 { points.len() } else { neighbours.clamp(1, points.len()) };
    // At a power of 2 (the default) the weights are ratios of squared distances: no square root
    // or power per cell and point, as GDAL's IDW does it. Squaring keeps the distances' order, so
    // the nearest points are the same.
    let squared = power == 2.0;
    let geographic = crs.is_geographic();
    let exact = if squared { 1e-24 } else { 1e-12 };
    // Each row on its own, on several threads when the work is large (#1252); a thread's
    // distances go into its own buffer.
    let scratch_bytes = points.len().saturating_mul(std::mem::size_of::<(f64, f64)>());
    // Every cell weighs every point: as many times the work as there are threads (#1291).
    let work = w.saturating_mul(h).saturating_mul(points.len());
    let threads = threads.unwrap_or_else(|| crate::parallel::threads_for_rows(h, work, scratch_bytes));
    let cap = crate::parallel::work_cap(MAX_IDW_WORK, threads);
    if work > cap {
        return Err(format!(
            "{w} × {h} cells from {} points is too much work, more than the {cap} cell-point pairs this computer takes on {threads} thread(s); use a larger cell size{}",
            points.len(),
            crate::parallel::UNCAPPED_HINT
        ));
    }
    let mut data = new_band(w, h, NODATA)?;
    crate::control::begin(h);
    crate::parallel::for_each_row(
        data.chunks_mut(w),
        threads,
        || (Vec::with_capacity(points.len()), crate::control::Pace::default()),
        |(near, pace): &mut (Vec<(f64, f64)>, crate::control::Pace), row, out: &mut [f32]| {
            // East-west degrees shrink with latitude, which only changes by row; a projected
            // system's units don't.
            let kx = if geographic { (extent.max_y - (row as f64 + 0.5) * cell).to_radians().cos() } else { 1.0 };
            for (col, o) in out.iter_mut().enumerate() {
                // Each cell weighs every point, so a wide row of many points takes long without a
                // cap: Cancel is looked at within it.
                if pace.stop(points.len()) {
                    return;
                }
                let c = Coord::new(extent.min_x + (col as f64 + 0.5) * cell, extent.max_y - (row as f64 + 0.5) * cell);
                near.clear();
                let d2 = |p: &Coord| {
                    let dx = (p.x - c.x) * kx;
                    dx * dx + (p.y - c.y).powi(2)
                };
                // One loop or the other, each kept tight.
                if squared {
                    near.extend(points.iter().map(|(p, v)| (d2(p), *v)));
                } else {
                    near.extend(points.iter().map(|(p, v)| (d2(p).sqrt(), *v)));
                }
                if k < near.len() {
                    near.select_nth_unstable_by(k - 1, |a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                    near.truncate(k);
                }
                let v = if let Some(on) = near.iter().find(|(d, _)| *d < exact) {
                    on.1
                } else {
                    // Weights relative to the nearest point's: the same ratios as 1/dᵖ, without
                    // overflowing for a high power.
                    let nearest = near.iter().map(|(d, _)| *d).fold(f64::INFINITY, f64::min);
                    let (num, den) = near.iter().fold((0.0, 0.0), |(n, d), (dist, v)| {
                        let wgt = if squared { nearest / dist } else { (nearest / dist).powf(power) };
                        (n + wgt * v, d + wgt)
                    });
                    num / den
                };
                *o = if v.is_finite() { v as f32 } else { NODATA };
            }
        },
    )?;
    let gt = GeoTransform { origin_x: extent.min_x, origin_y: extent.max_y, pixel_width: cell, pixel_height: -cell };
    Raster::new(name, w as u32, h as u32, vec![data], gt, crs, Some(NODATA as f64), ColorHint::Gray, "Float32").map_err(|e| e.to_string())
}

/// Unsupervised k-means classes (1..=k) of the pixels over the given bands.
pub fn kmeans(r: &Raster, bands: &[usize], k: usize, iterations: usize) -> Result<Vec<f32>, String> {
    kmeans_on(None, r, bands, k, iterations)
}

/// [`kmeans`] on `threads` threads (none: as many as pay off). The centres are found on a sample
/// of the pixels; then each pixel takes its nearest, on its own, so rows go on several threads
/// (#1252).
fn kmeans_on(threads: Option<usize>, r: &Raster, bands: &[usize], k: usize, iterations: usize) -> Result<Vec<f32>, String> {
    let k = k.clamp(2, 50);
    let data: Vec<&[f32]> = bands.iter().map(|b| band(r, *b)).collect::<Result<_, _>>()?;
    if data.is_empty() {
        return Err("give at least one band".into());
    }
    let n = r.width as usize * r.height as usize;
    let pixel =
        |i: usize| -> Option<Vec<f64>> { data.iter().map(|d| d.get(i).copied().filter(|v| r.is_valid(*v)).map(f64::from)).collect() };
    // The same, into a buffer reused across pixels (the full passes touch every pixel).
    let pixel_into = |i: usize, buf: &mut Vec<f64>| -> bool {
        buf.clear();
        for d in &data {
            match d.get(i).copied().filter(|v| r.is_valid(*v)) {
                Some(v) => buf.push(f64::from(v)),
                None => return false,
            }
        }
        true
    };
    // Seeds spread across the value range of the first band.
    let valid: Vec<usize> = (0..n).step_by((n / 20_000).max(1)).filter(|i| pixel(*i).is_some()).collect();
    if valid.len() < k {
        return Err("too few valid pixels to classify".into());
    }
    let mut sorted = valid.clone();
    sorted.sort_by(|a, b| {
        let (va, vb) =
            (pixel(*a).and_then(|p| p.first().copied()).unwrap_or(0.0), pixel(*b).and_then(|p| p.first().copied()).unwrap_or(0.0));
        va.partial_cmp(&vb).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut centres: Vec<Vec<f64>> =
        (0..k).filter_map(|i| sorted.get((i * 2 + 1) * sorted.len() / (2 * k)).and_then(|j| pixel(*j))).collect();
    let nearest = |p: &[f64], cs: &[Vec<f64>]| -> usize {
        cs.iter()
            .enumerate()
            .map(|(i, c)| (i, c.iter().zip(p).map(|(a, b)| (a - b).powi(2)).sum::<f64>()))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map_or(0, |x| x.0)
    };
    for _ in 0..iterations.clamp(1, 50) {
        let mut sums = vec![vec![0.0; data.len()]; centres.len()];
        let mut counts = vec![0usize; centres.len()];
        let mut p = Vec::with_capacity(data.len());
        for i in &valid {
            if pixel_into(*i, &mut p) {
                let c = nearest(&p, &centres);
                if let (Some(s), Some(n)) = (sums.get_mut(c), counts.get_mut(c)) {
                    s.iter_mut().zip(&p).for_each(|(a, b)| *a += b);
                    *n += 1;
                }
            }
        }
        for ((c, s), n) in centres.iter_mut().zip(&sums).zip(&counts) {
            if *n > 0 {
                *c = s.iter().map(|v| v / *n as f64).collect();
            }
        }
    }
    // Classes numbered by the first band's centre, low to high.
    let mut order: Vec<usize> = (0..centres.len()).collect();
    order.sort_by(|a, b| {
        centres.get(*a).and_then(|c| c.first()).partial_cmp(&centres.get(*b).and_then(|c| c.first())).unwrap_or(std::cmp::Ordering::Equal)
    });
    let rank: Vec<usize> = (0..centres.len()).map(|c| order.iter().position(|o| *o == c).unwrap_or(0)).collect();
    let (w, h) = (r.width as usize, r.height as usize);
    let mut out = new_band(w, h, NODATA)?;
    if w == 0 {
        return Ok(out);
    }
    // A pixel's distance to every centre over every band, a few nanoseconds per pair: in
    // `MIN_ROW_WORK`'s units (some 30 ns), about one per eight pairs.
    let work = n.saturating_mul(centres.len().saturating_mul(data.len()) / 8 + 1);
    let threads = threads.unwrap_or_else(|| crate::parallel::threads_for_rows(h, work, 0));
    crate::control::begin(h);
    crate::parallel::for_each_row(
        out.chunks_mut(w),
        threads,
        || Vec::with_capacity(data.len()),
        |p, row, line: &mut [f32]| {
            for (col, o) in line.iter_mut().enumerate() {
                if pixel_into(row * w + col, p) {
                    *o = (rank.get(nearest(p, &centres)).copied().unwrap_or(0) + 1) as f32;
                }
            }
        },
    )?;
    Ok(out)
}

/// Minimum-distance classification: each pixel takes the class whose training mean is closest.
pub fn classify_min_distance(r: &Raster, bands: &[usize], means: &[(f32, Vec<f64>)]) -> Result<Vec<f32>, String> {
    let data: Vec<&[f32]> = bands.iter().map(|b| band(r, *b)).collect::<Result<_, _>>()?;
    if means.is_empty() {
        return Err("no training classes".into());
    }
    let n = r.width as usize * r.height as usize;
    // A distance to each class's mean in each band.
    let cost = means.len().saturating_mul(data.len()).max(1);
    per_index(
        n,
        r.width as usize,
        cost,
        || Vec::with_capacity(data.len()),
        |p: &mut Vec<f64>, i| {
            p.clear();
            for d in &data {
                match d.get(i).copied().filter(|v| r.is_valid(*v)) {
                    Some(v) => p.push(f64::from(v)),
                    None => return NODATA,
                }
            }
            means
                .iter()
                .map(|(c, m)| (*c, m.iter().zip(p.iter()).map(|(a, b)| (a - b).powi(2)).sum::<f64>()))
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map_or(NODATA, |x| x.0)
        },
    )
}

/// The mean of each band over the pixels whose centres fall in a polygon (lon/lat rings). Its rows
/// count toward the run's progress, and a run cancelled under a control stops between rows
/// (#1562); without one it runs to the end. The rows are summed in order on one thread, so the
/// means are the same on every machine.
pub fn zonal_means(r: &Raster, bands: &[usize], rings: &[Vec<Coord>]) -> Result<Option<(Vec<f64>, usize)>, crate::control::Cancelled> {
    crate::control::begin(r.height as usize);
    means_under(r, &ZoneIn::LonLat, bands, &[rings], |rows| {
        crate::control::check()?;
        crate::control::advance(rows);
        Ok(())
    })
}

/// [`zonal_means`] of a polygon's `parts` (each its rings, a pixel in two parts counted once) in
/// `from`: longitude/latitude, or the system its layer holds its coordinates in. A cell's centre is
/// converted into that system to be tested (none of it when it is the raster's own), by the direct
/// operation between two datums. Each part's rows count toward the progress.
pub fn zonal_means_in(r: &Raster, from: Crs, bands: &[usize], parts: &[Vec<Vec<Coord>>]) -> Result<Option<(Vec<f64>, usize)>, String> {
    let zone = ZoneIn::new(r, from)?;
    crate::control::begin(parts.len().saturating_mul(r.height as usize));
    let parts: Vec<&[Vec<Coord>]> = parts.iter().map(Vec::as_slice).collect();
    Ok(means_under(r, &zone, bands, &parts, |rows| {
        crate::control::check()?;
        crate::control::advance(rows);
        Ok(())
    })?)
}

/// The system a zone's rings are in, and how a raster's cells and the zone meet there: in
/// longitude/latitude through the raster's transformer, or in a layer's own system
/// ([`crate::tools::ToGrid`] both ways). A cell's centre goes into the zone's system as a point
/// ([`crate::tools::ToGrid::place`]): a cell centre the zone's system can't hold is outside the
/// zone, so it isn't counted. The zone's extent goes onto the raster's grid as a polygon's vertices do
/// ([`crate::tools::ToGrid::vertex`]), Web Mercator taking a latitude past 85.05° as 85.05°, as
/// it takes a place in longitude/latitude, so a zone reaching past it is still read within its
/// window.
pub(crate) enum ZoneIn {
    LonLat,
    Own { to_zone: Box<crate::tools::ToGrid>, to_grid: Box<crate::tools::ToGrid> },
}

impl ZoneIn {
    pub(crate) fn new(r: &Raster, from: Crs) -> Result<Self, String> {
        Ok(if from == Crs::LonLat {
            Self::LonLat
        } else {
            Self::Own {
                to_zone: Box::new(crate::tools::ToGrid::new(r.crs, from)?),
                to_grid: Box::new(crate::tools::ToGrid::new(from, r.crs)?),
            }
        })
    }

    /// A place in the raster's system in the zone's.
    fn zone(&self, r: &Raster, w: Coord) -> Option<Coord> {
        match self {
            Self::LonLat => r.transformer().to_lonlat(w),
            Self::Own { to_zone, .. } => to_zone.place(w),
        }
    }

    /// A place in the zone's system in the raster's.
    fn grid(&self, r: &Raster, c: Coord) -> Option<Coord> {
        match self {
            Self::LonLat => r.transformer().from_lonlat(c),
            Self::Own { to_grid, .. } => to_grid.vertex(c),
        }
    }
}

/// [`polygon_mask_with_failures`] for polygons in `from`: longitude/latitude, or the system their
/// layer holds its coordinates in, converted into the raster's (none of it when that is the
/// raster's own), by the direct operation between two datums.
pub fn polygon_mask_in(
    r: &Raster,
    from: Crs,
    polygons: &[Vec<Vec<Coord>>],
    failed_parts: &mut Vec<usize>,
) -> Result<Option<(Range<u32>, Range<u32>, Vec<bool>)>, String> {
    if from == Crs::LonLat {
        return polygon_mask_with_failures(r, polygons, failed_parts);
    }
    let to = crate::tools::ToGrid::new(from, r.crs)?;
    polygon_mask_on(ToWorld::Own(&to), &r.transform, (r.width, r.height), polygons, failed_parts)
}

/// [`zonal_means`] over a zone's `parts` (each its rings), `each_rows(n)` asked as the raster's
/// rows go by, `n` at a time (each row once per part, all of them): an error stops it.
///
/// A cell counts when its centre, in the zone's system (`zone`), lies in a part (inside its outer
/// ring and in none of its holes), once however many parts hold it. Each part's cells are tested in
/// its window ([`zone_window`]), not every cell of the raster, in the same row-major order, so for
/// one part the sums are those of testing them all.
fn means_under(
    r: &Raster,
    zone: &ZoneIn,
    bands: &[usize],
    parts: &[&[Vec<Coord>]],
    mut each_rows: impl FnMut(usize) -> Result<(), crate::control::Cancelled>,
) -> Result<Option<(Vec<f64>, usize)>, crate::control::Cancelled> {
    let boxes: Vec<Extent> = parts
        .iter()
        .map(|rings| {
            rings.iter().flatten().fold(Extent::EMPTY, |mut e, c| {
                e.expand(*c);
                e
            })
        })
        .collect();
    let in_part = |k: usize, ll: Coord| boxes[k].contains(ll) && rustgis_data::geometry::polygon_contains(parts[k], ll);
    // The earlier parts a cell of a part may also lie in are found by their boxes: a part whose box
    // meets no earlier one's tests none of them.
    let index = (parts.len() > 1).then(|| crate::tools::ExtentIndex::new(boxes.iter().copied()));
    let mut sums = vec![0.0; bands.len()];
    let mut n = 0usize;
    for (k, e) in boxes.iter().enumerate() {
        let earlier = index.as_ref().filter(|ix| ix.any_candidate(e, |j| j < k && boxes[j].intersects(e)));
        // A pole inside the zone's box, in a system other than the raster's: the outline bounds
        // nothing there (on a Web Mercator raster it lands south of the cells near the pole).
        let (cols, rows) = match zone {
            ZoneIn::Own { to_zone, .. } if to_zone.poles().iter().any(|p| e.contains(*p)) => (0..r.width, 0..r.height),
            ZoneIn::Own { .. } => zone_window(r, e, true, |c| zone.grid(r, c)),
            ZoneIn::LonLat => zone_window(r, e, false, |c| zone.grid(r, c)),
        };
        each_rows((r.height - (rows.end - rows.start)) as usize)?;
        for row in rows {
            each_rows(1)?;
            for col in cols.clone() {
                let w = r.transform.to_world(f64::from(col) + 0.5, f64::from(row) + 0.5);
                let Some(ll) = zone.zone(r, w) else { continue };
                // Counted for the first part that holds it, so parts that overlap count it once.
                if !in_part(k, ll)
                    || earlier.is_some_and(|ix| ix.any_candidate(&Extent::new(ll.x, ll.y, ll.x, ll.y), |j| j < k && in_part(j, ll)))
                {
                    continue;
                }
                let vals: Option<Vec<f64>> = bands.iter().map(|b| r.value(*b, i64::from(col), i64::from(row)).map(f64::from)).collect();
                if let Some(v) = vals {
                    sums.iter_mut().zip(&v).for_each(|(s, x)| *s += x);
                    n += 1;
                }
            }
        }
    }
    Ok((n > 0).then(|| (sums.into_iter().map(|s| s / n as f64).collect(), n)))
}

/// The columns and rows of `r` whose cell centres can fall in the extent `e`, in the zone's system
/// (`to_grid` places it in the raster's): its outline placed on the raster's grid, with two cells
/// to spare. Each side is sampled, and each stretch between samples near the raster is halved until
/// its ends are at most a cell apart, so a side that curves on the grid (a parallel on a polar
/// raster) bows only a fraction of a cell between them, whatever the zone's size. A stretch whose
/// ends are farther from the raster than the distance between them isn't halved: that takes the
/// side not to fold back within one stretch (a sixty-fourth of the side at first), which a
/// projection's smooth image of a parallel or meridian doesn't. The whole raster when a point can't
/// be placed; none when `e` is empty.
///
/// With `interior` (a zone held in a system other than longitude/latitude), places inside `e` are
/// placed too, a sixteenth of it apart: when one can't be, or lands outside the outline's window,
/// the zone's box holds a place the raster's grid can't take in one piece, and the whole raster is
/// read. (A box holding a pole, whose outline lands south of a Web Mercator raster's cells near
/// it, is read whole before this: `means_under`.)
fn zone_window(r: &Raster, e: &Extent, interior: bool, to_grid: impl Fn(Coord) -> Option<Coord>) -> (Range<u32>, Range<u32>) {
    if e.is_empty() {
        return (0..0, 0..0);
    }
    const SAMPLES: u32 = 64;
    // Halvings of a stretch: past 2^-40 of a side it is far below a cell.
    const DEPTH: u32 = 40;
    let whole = (0..r.width, 0..r.height);
    let (w, h) = (f64::from(r.width), f64::from(r.height));
    let place = |ll: Coord| {
        let (col, row) = r.transform.to_pixel(to_grid(ll)?);
        (col.is_finite() && row.is_finite()).then_some((col, row))
    };
    let sides: [(Coord, Coord); 4] = [
        (Coord::new(e.min_x, e.min_y), Coord::new(e.max_x, e.min_y)),
        (Coord::new(e.max_x, e.min_y), Coord::new(e.max_x, e.max_y)),
        (Coord::new(e.max_x, e.max_y), Coord::new(e.min_x, e.max_y)),
        (Coord::new(e.min_x, e.max_y), Coord::new(e.min_x, e.min_y)),
    ];
    let (mut c0, mut r0, mut c1, mut r1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut stretches = Vec::new();
    for (a, b) in sides {
        let at = |t: f64| Coord::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y));
        let mut last = None;
        for i in 0..=SAMPLES {
            let t = f64::from(i) / f64::from(SAMPLES);
            let Some(p) = place(at(t)) else { return whole };
            if let Some((t0, p0)) = last {
                stretches.push((t0, p0, t, p, 0));
            }
            last = Some((t, p));
        }
        while let Some((t0, p0, t1, p1, depth)) = stretches.pop() {
            for (col, row) in [p0, p1] {
                (c0, r0, c1, r1) = (c0.min(col), r0.min(row), c1.max(col), r1.max(row));
            }
            let len = (p1.0 - p0.0).hypot(p1.1 - p0.1);
            let far = p0.0.max(p1.0) + len < 0.0 || p0.0.min(p1.0) - len > w || p0.1.max(p1.1) + len < 0.0 || p0.1.min(p1.1) - len > h;
            if len <= 1.0 || depth >= DEPTH || far {
                continue;
            }
            let tm = (t0 + t1) / 2.0;
            let Some(pm) = place(at(tm)) else { return whole };
            stretches.push((t0, p0, tm, pm, depth + 1));
            stretches.push((tm, pm, t1, p1, depth + 1));
        }
    }
    let (c0, r0, c1, r1) = (c0.floor() - 2.0, r0.floor() - 2.0, c1.ceil() + 2.0, r1.ceil() + 2.0);
    if interior {
        const INSIDE: u32 = 16;
        for i in 1..INSIDE {
            for j in 1..INSIDE {
                let (u, v) = (f64::from(i) / f64::from(INSIDE), f64::from(j) / f64::from(INSIDE));
                let Some((col, row)) = place(Coord::new(e.min_x + u * (e.max_x - e.min_x), e.min_y + v * (e.max_y - e.min_y))) else {
                    return whole;
                };
                if !(c0..=c1).contains(&col) || !(r0..=r1).contains(&row) {
                    return whole;
                }
            }
        }
    }
    let clamp = |v: f64, n: u32| v.clamp(0.0, f64::from(n)) as u32;
    ((clamp(c0, r.width)..clamp(c1, r.width)), (clamp(r0, r.height)..clamp(r1, r.height)))
}

/// Each class's mean of `bands` over the pixels under its training polygons (lon/lat rings, each
/// with its class), and how many pixels: the classes in the order first met, one with no pixel
/// under its polygons left out. Each polygon's rows count toward the run's progress, and a
/// cancelled run stops (#1291).
pub fn class_means(r: &Raster, bands: &[usize], polygons: &[(String, Vec<Vec<Coord>>)]) -> Result<Vec<(String, Vec<f64>, usize)>, String> {
    crate::control::begin(polygons.len().saturating_mul(r.height as usize));
    let mut classes: Vec<(String, Vec<f64>, usize)> = Vec::new();
    for (key, rings) in polygons {
        let counted = |rows: usize| {
            crate::control::check()?;
            crate::control::advance(rows);
            Ok(())
        };
        let Some((m, n)) = means_under(r, &ZoneIn::LonLat, bands, &[rings.as_slice()], counted)? else { continue };
        match classes.iter_mut().find(|c| c.0 == *key) {
            Some(c) => {
                let total = c.2 + n;
                c.1 = c.1.iter().zip(&m).map(|(a, b)| (a * c.2 as f64 + b * n as f64) / total as f64).collect();
                c.2 = total;
            }
            None => classes.push((key.clone(), m, n)),
        }
    }
    Ok(classes)
}

/// `bands` bands of `images` laid onto a north-up grid in `crs` of `cols` × `rows` cells of `cell`
/// (in its units) from `extent`'s north-west corner: a cell takes the first image's value under
/// its centre, or with `mean` the mean of all of theirs; no-data where none has one. Rows go on
/// several threads and count toward the run's progress, and a cancelled run stops (#1291).
pub fn mosaic(
    images: &[&Raster],
    extent: &Extent,
    cell: f64,
    (cols, rows): (usize, usize),
    bands: usize,
    mean: bool,
    crs: Crs,
) -> Result<Vec<Vec<f32>>, String> {
    let mut out = (0..bands).map(|_| new_band(cols, rows, NODATA)).collect::<Result<Vec<_>, _>>()?;
    if cols == 0 {
        return Ok(out);
    }
    // A cell of a projected grid is found in an image in the same system by its coordinates, and
    // in any other through longitude/latitude.
    let to = if crs == Crs::LonLat { None } else { Some(crs.transformer()?) };
    let own = |r: &Raster, c: Coord| {
        let (cc, rr) = r.transform.to_pixel(c);
        let (cc, rr) = (r.wrap_col(cc).floor(), rr.floor());
        (cc >= 0.0 && rr >= 0.0 && cc < f64::from(r.width) && rr < f64::from(r.height)).then_some((cc as i64, rr as i64))
    };
    // Each row as a slice of every band, for one thread to fill.
    let mut by_band: Vec<_> = out.iter_mut().map(|b| b.chunks_mut(cols)).collect();
    let lines: Vec<Vec<&mut [f32]>> = (0..rows).map(|_| by_band.iter_mut().filter_map(Iterator::next).collect()).collect();
    // A cell is placed in each image (some ten units) and read in each band.
    let work = cols.saturating_mul(rows).saturating_mul(images.len().max(1)).saturating_mul(10 + bands);
    let threads = crate::parallel::threads_for_rows(rows, work, 0);
    crate::control::begin(rows);
    crate::parallel::for_each_row(
        lines.into_iter(),
        threads,
        || Vec::with_capacity(images.len()),
        |at: &mut Vec<Option<(i64, i64)>>, row, mut line: Vec<&mut [f32]>| {
            for col in 0..cols {
                let c = Coord::new(extent.min_x + (col as f64 + 0.5) * cell, extent.max_y - (row as f64 + 0.5) * cell);
                // The cell's longitude/latitude, worked out only for an image in another system.
                let mut lonlat: Option<Option<Coord>> = None;
                // Where the cell falls in each image, once for all bands.
                at.clear();
                for r in images {
                    at.push(match &to {
                        Some(_) if r.crs == crs => own(r, c),
                        Some(t) => (*lonlat.get_or_insert_with(|| t.to_lonlat(c))).and_then(|ll| r.pixel_at(ll)),
                        None => r.pixel_at(c),
                    });
                }
                for (bi, band) in line.iter_mut().enumerate() {
                    let vals = images.iter().zip(at.iter()).filter_map(|(r, a)| a.and_then(|(cc, rr)| r.value(bi, cc, rr)));
                    let v = if mean {
                        let (sum, k) = vals.fold((0.0f64, 0usize), |(s, k), v| (s + v as f64, k + 1));
                        (k > 0).then(|| (sum / k as f64) as f32)
                    } else {
                        vals.into_iter().next()
                    };
                    if let (Some(v), Some(slot)) = (v, band.get_mut(col)) {
                        *slot = v;
                    }
                }
            }
        },
    )?;
    Ok(out)
}

/// An error encountered while validating or transforming a polygon geometry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolygonTransformError {
    EmptyPolygon,
    InvalidExterior(String),
    InvalidHole { index: usize, reason: String },
}

impl std::fmt::Display for PolygonTransformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPolygon => write!(f, "polygon has no rings"),
            Self::InvalidExterior(r) => write!(f, "exterior ring is invalid: {r}"),
            Self::InvalidHole { index, reason } => write!(f, "hole {index} is invalid: {reason}"),
        }
    }
}

impl std::error::Error for PolygonTransformError {}

/// Transforms a single polygon's rings (exterior ring + optional holes) to WGS 84 lon/lat coordinates.
///
/// - The exterior ring must have at least 3 points and all points must project successfully.
/// - Every interior ring (hole) must have at least 3 points and all points must project successfully.
/// - If any hole fails or is invalid, the entire polygon is rejected with an error rather than
///   silently discarding the hole or altering the polygon geometry.
pub fn transform_polygon_to_lonlat(rings: &[Vec<Coord>], tr: Option<&Transformer>) -> Result<Vec<Vec<Coord>>, PolygonTransformError> {
    let exterior = rings.first().ok_or(PolygonTransformError::EmptyPolygon)?;
    let mut ext_lonlat = Vec::with_capacity(exterior.len());
    for &c in exterior {
        if !c.is_finite() {
            return Err(PolygonTransformError::InvalidExterior("non-finite coordinate".into()));
        }
        let ll = match tr {
            Some(t) => t.to_lonlat(c).ok_or_else(|| PolygonTransformError::InvalidExterior("coordinate transformation failed".into()))?,
            None => c,
        };
        if !ll.is_finite() {
            return Err(PolygonTransformError::InvalidExterior("transformed coordinate is non-finite".into()));
        }
        ext_lonlat.push(ll);
    }
    if ext_lonlat.len() < 3 {
        return Err(PolygonTransformError::InvalidExterior("fewer than 3 vertices".into()));
    }

    let mut out_rings = vec![ext_lonlat];
    for (idx, hole) in rings.iter().skip(1).enumerate() {
        let hole_num = idx + 1;
        let mut hole_lonlat = Vec::with_capacity(hole.len());
        for &c in hole {
            if !c.is_finite() {
                return Err(PolygonTransformError::InvalidHole { index: hole_num, reason: "non-finite coordinate".into() });
            }
            let ll = match tr {
                Some(t) => t.to_lonlat(c).ok_or_else(|| PolygonTransformError::InvalidHole {
                    index: hole_num,
                    reason: "coordinate transformation failed".into(),
                })?,
                None => c,
            };
            if !ll.is_finite() {
                return Err(PolygonTransformError::InvalidHole { index: hole_num, reason: "transformed coordinate is non-finite".into() });
            }
            hole_lonlat.push(ll);
        }
        if hole_lonlat.len() < 3 {
            return Err(PolygonTransformError::InvalidHole { index: hole_num, reason: "fewer than 3 vertices".into() });
        }
        out_rings.push(hole_lonlat);
    }

    Ok(out_rings)
}

/// Transforms and validates all polygon parts of a (multi-)polygon feature to WGS 84 lon/lat coordinates.
/// With no transformer (`tr` `None`), it only checks the parts and takes them apart into rings,
/// in whatever system they are in.
pub fn transform_feature_polygons_to_lonlat(
    polys: &[Vec<Vec<Coord>>],
    tr: Option<&Transformer>,
) -> Result<Vec<Vec<Vec<Coord>>>, PolygonTransformError> {
    if polys.is_empty() {
        return Err(PolygonTransformError::EmptyPolygon);
    }
    let mut out = Vec::with_capacity(polys.len());
    for (part_idx, rings) in polys.iter().enumerate() {
        let valid_poly = transform_polygon_to_lonlat(rings, tr).map_err(|e| match e {
            PolygonTransformError::InvalidHole { index, reason } => {
                PolygonTransformError::InvalidHole { index, reason: format!("part {part_idx}: {reason}") }
            }
            PolygonTransformError::InvalidExterior(reason) => PolygonTransformError::InvalidExterior(format!("part {part_idx}: {reason}")),
            other => other,
        })?;
        out.push(valid_poly);
    }
    Ok(out)
}

/// Validates a 1-based band number and returns its 0-based index.
pub(crate) fn band_from_f64(value: f64, total_bands: usize) -> Result<usize, String> {
    if !value.is_finite() {
        return Err("band number must be a finite number".into());
    }
    if value <= 0.0 {
        return Err("band numbers must be positive (1-based)".into());
    }
    if value.fract() != 0.0 {
        return Err("band numbers must be integers, not decimals".into());
    }
    // Check before casting, including on wasm32, where huge values would saturate.
    if value > total_bands as f64 || value >= usize::MAX as f64 {
        return Err(format!("band index {value} is invalid: raster has bands 1 to {total_bands}"));
    }
    Ok(value as usize - 1)
}

/// Parses a band specification string into 0-based band indices.
///
/// Supported inputs:
/// - `""` (empty string): defaults to band 1 (`vec![0]`)
/// - `"all"`: all bands `0..total_bands`
/// - comma, semicolon, or space-separated list of 1-based band numbers: `"1, 2, 3"`, `"[1, 2]"`, etc.
///
/// Validates that:
/// - Every entry is a valid, finite positive integer.
/// - Decimals (e.g. `"1.5"`), negative numbers (e.g. `"-1"`), zero (`"0"`), non-numeric (e.g. `"abc"`),
///   or non-finite values (e.g. `"NaN"`, `"Infinity"`) are strictly rejected.
/// - Every band number is within `1..=total_bands`.
/// - Empty parts (such as `"1,,2"` or `","`) are rejected.
pub fn parse_bands_str(s: &str, total_bands: usize) -> Result<Vec<usize>, String> {
    if total_bands == 0 {
        return Err("raster has no bands".into());
    }
    let txt = s.trim();
    if txt.is_empty() {
        return Ok(vec![0]);
    }
    if txt.eq_ignore_ascii_case("all") {
        return Ok((0..total_bands).collect());
    }
    let clean = txt.trim_matches(|c| c == '[' || c == ']');
    if clean.trim().is_empty() {
        return Err("band selection cannot be empty brackets".into());
    }
    let mut out = Vec::new();
    for part in clean.split([',', ';']) {
        let part = part.trim().trim_matches('"').trim();
        if part.is_empty() {
            return Err("band selection contains an empty band entry".into());
        }
        for subpart in part.split_whitespace() {
            let subpart = subpart.trim();
            if subpart.is_empty() {
                continue;
            }
            if subpart.eq_ignore_ascii_case("all") {
                for b in 0..total_bands {
                    if !out.contains(&b) {
                        out.push(b);
                    }
                }
                continue;
            }
            let f: f64 = subpart.parse().map_err(|_| format!("invalid band number '{subpart}'"))?;
            let b_idx = band_from_f64(f, total_bands)?;
            if !out.contains(&b_idx) {
                out.push(b_idx);
            }
        }
    }
    if out.is_empty() {
        return Err("no valid band numbers found in input".into());
    }
    Ok(out)
}

/// How polygons' places reach a grid's coordinate system: from longitude/latitude through its
/// transformer, or from a run's own system ([`crate::tools::ToGrid`]: as they are when the grid is
/// in it too).
#[derive(Clone, Copy)]
pub(crate) enum ToWorld<'a> {
    LonLat(&'a Transformer),
    Own(&'a crate::tools::ToGrid),
}

impl ToWorld<'_> {
    fn world(&self, c: Coord) -> Option<Coord> {
        match self {
            Self::LonLat(tr) => tr.from_lonlat(c),
            Self::Own(to) => to.vertex(c),
        }
    }
}

/// Projects a complete ring to the raster grid without dropping failed vertices.
fn project_ring(tr: ToWorld<'_>, t: &GeoTransform, ring: &[Coord]) -> Option<Vec<(f64, f64)>> {
    if ring.len() < 3 {
        return None;
    }
    ring.iter()
        .map(|&c| {
            if !c.is_finite() {
                return None;
            }
            let pt = t.to_pixel(tr.world(c)?);
            (pt.0.is_finite() && pt.1.is_finite()).then_some(pt)
        })
        .collect()
}

/// The cells of `r` whose centres fall inside any of `polygons` (each an exterior ring and its
/// holes, in longitude/latitude): the column and row window around the polygons, clamped to the
/// raster, and a row-major mask over it. `None` when no cell centre falls inside; an error when
/// the mask's memory can't be had ("not enough memory for a W × H raster").
///
/// The rings are projected onto the raster's own grid once and filled by scanline, so a large
/// raster costs a pass over the polygon edges per row rather than a point test per cell.
/// A malformed ring or a failed projection rejects the entire polygon part, including its holes.
pub fn polygon_mask(r: &Raster, polygons: &[Vec<Vec<Coord>>]) -> Result<Option<(Range<u32>, Range<u32>, Vec<bool>)>, String> {
    polygon_mask_with_failures(r, polygons, &mut Vec::new())
}

/// Builds a polygon mask and appends the zero-based indices of rejected parts to `failed_parts`.
/// Valid parts are retained even if another part cannot be projected onto the raster grid.
pub fn polygon_mask_with_failures(
    r: &Raster,
    polygons: &[Vec<Vec<Coord>>],
    failed_parts: &mut Vec<usize>,
) -> Result<Option<(Range<u32>, Range<u32>, Vec<bool>)>, String> {
    polygon_mask_on(ToWorld::LonLat(r.transformer()), &r.transform, (r.width, r.height), polygons, failed_parts)
}

/// [`polygon_mask_with_failures`] on a grid of `size` (columns, rows) cells placed by `t` in the
/// coordinate system of `tr`, with no raster on it yet.
pub(crate) fn polygon_mask_on(
    tr: ToWorld<'_>,
    t: &GeoTransform,
    size: (u32, u32),
    polygons: &[Vec<Vec<Coord>>],
    failed_parts: &mut Vec<usize>,
) -> Result<Option<(Range<u32>, Range<u32>, Vec<bool>)>, String> {
    let Some(placed) = Placed::new(tr, t, size, polygons, failed_parts) else { return Ok(None) };
    let (cols, rows) = (placed.cols.clone(), placed.rows.clone());
    let (w, h) = ((cols.end - cols.start) as usize, (rows.end - rows.start) as usize);
    let mut mask = new_band(w, h, false)?;
    // Trimmed to the cells inside: reprojected vertices on cell edges land a hair either side.
    let (mut tc0, mut tr0, mut tc1, mut tr1) = (usize::MAX, usize::MAX, 0, 0);
    placed.spans(|row, span| {
        let rr = (row - rows.start) as usize;
        let (c0, c1) = ((span.start - cols.start) as usize, (span.end - cols.start) as usize);
        if let Some(cells) = mask.get_mut(rr * w + c0..rr * w + c1) {
            cells.fill(true);
        }
        (tc0, tr0, tc1, tr1) = (tc0.min(c0), tr0.min(rr), tc1.max(c1), tr1.max(rr + 1));
    });
    if tc0 >= tc1 || tr0 >= tr1 {
        return Ok(None);
    }
    if (tc0, tr0, tc1, tr1) == (0, 0, w, h) {
        return Ok(Some((cols, rows, mask)));
    }
    let mut trimmed = grid(tc1 - tc0, tr1 - tr0)?;
    for rr in tr0..tr1 {
        trimmed.extend_from_slice(mask.get(rr * w + tc0..rr * w + tc1).unwrap_or_default());
    }
    let cols = cols.start + tc0 as u32..cols.start + tc1 as u32;
    let rows = rows.start + tr0 as u32..rows.start + tr1 as u32;
    Ok(Some((cols, rows, trimmed)))
}

/// Polygons (each an exterior ring and its holes, their places reaching the grid's system by a
/// [`ToWorld`]) projected to a grid's pixels, with the window of the grid around them.
pub(crate) struct Placed {
    parts: Vec<Vec<Vec<(f64, f64)>>>,
    width: u32,
    cols: Range<u32>,
    rows: Range<u32>,
}

/// An edge of a ring, from `a` to `b` in pixels, crossing the centres of rows `first..end`.
#[derive(Clone, Copy)]
struct Edge {
    first: u32,
    end: u32,
    a: (f64, f64),
    b: (f64, f64),
}

impl Placed {
    /// `None` when no polygon can be placed or none reaches the grid of `size` (columns, rows).
    /// The zero-based indices of the rejected parts are appended to `failed_parts`.
    pub(crate) fn new(
        tr: ToWorld<'_>,
        t: &GeoTransform,
        size: (u32, u32),
        polygons: &[Vec<Vec<Coord>>],
        failed_parts: &mut Vec<usize>,
    ) -> Option<Self> {
        let (width, height) = size;
        let parts: Vec<Vec<Vec<(f64, f64)>>> = polygons
            .iter()
            .enumerate()
            .filter_map(|(index, rings)| {
                // Reject a complete part if any ring fails, so a lost hole cannot enlarge it.
                let part =
                    if rings.is_empty() { None } else { rings.iter().map(|ring| project_ring(tr, t, ring)).collect::<Option<Vec<_>>>() };
                if part.is_none() {
                    failed_parts.push(index);
                }
                part
            })
            .collect();
        let (mut c0, mut r0, mut c1, mut r1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &(x, y) in parts.iter().flatten().flatten() {
            (c0, r0, c1, r1) = (c0.min(x), r0.min(y), c1.max(x), r1.max(y));
        }
        if !(c0.is_finite() && r0.is_finite() && c1.is_finite() && r1.is_finite()) {
            return None;
        }
        let clamp = |v: f64, n: u32| v.clamp(0.0, f64::from(n)) as u32;
        let cols = clamp(c0.floor(), width)..clamp(c1.ceil(), width);
        let rows = clamp(r0.floor(), height)..clamp(r1.ceil(), height);
        (!cols.is_empty() && !rows.is_empty()).then_some(Self { parts, width, cols, rows })
    }

    /// Calls `span` with each row's runs of cells whose centres lie inside a polygon: even-odd
    /// within each polygon (holes cut out), so polygons that overlap may give a cell twice. Each
    /// edge is visited only on the rows it crosses, so a polygon's edges cost by their length
    /// on the grid, not by the rows times all the edges.
    pub(crate) fn spans(&self, mut span: impl FnMut(u32, Range<u32>)) {
        let clamp = |v: f64| v.clamp(0.0, f64::from(self.width)) as u32;
        let (mut edges, mut active, mut xs) = (Vec::new(), Vec::new(), Vec::new());
        for rings in &self.parts {
            edges.clear();
            for ring in rings {
                for (i, &a) in ring.iter().enumerate() {
                    let b = ring.get(i + 1).or(ring.first()).copied().unwrap_or(a);
                    let (first, end) = (first_row_from(a.1.min(b.1), &self.rows), first_row_from(a.1.max(b.1), &self.rows));
                    if first < end {
                        edges.push(Edge { first, end, a, b });
                    }
                }
            }
            edges.sort_unstable_by_key(|e: &Edge| e.first);
            let (Some(start), Some(stop)) = (edges.first().map(|e| e.first), edges.iter().map(|e| e.end).max()) else { continue };
            let mut waiting = edges.iter().copied().peekable();
            active.clear();
            for row in start..stop {
                while let Some(e) = waiting.next_if(|e| e.first == row) {
                    active.push(e);
                }
                active.retain(|e: &Edge| e.end > row);
                let y = f64::from(row) + 0.5;
                xs.clear();
                xs.extend(active.iter().map(|e| e.a.0 + (y - e.a.1) * (e.b.0 - e.a.0) / (e.b.1 - e.a.1)));
                xs.sort_by(f64::total_cmp);
                for &[x0, x1] in xs.as_chunks::<2>().0 {
                    // Cells whose centre (col + 0.5) lies in [x0, x1).
                    let a = clamp((x0 - 0.5).ceil()).max(self.cols.start);
                    let b = clamp((x1 - 0.5).ceil()).min(self.cols.end);
                    if a < b {
                        span(row, a..b);
                    }
                }
            }
        }
    }
}

/// The first of `rows` whose centre (row + 0.5) is at or past `v`, or the end of them. An edge
/// crosses a row's centre when its lower end is at or before it and its upper end past it.
fn first_row_from(v: f64, rows: &Range<u32>) -> u32 {
    let centre = |r: u32| f64::from(r) + 0.5;
    let mut r = (v - 0.5).ceil().clamp(f64::from(rows.start), f64::from(rows.end)) as u32;
    // `v - 0.5` can round far from zero.
    while r > rows.start && centre(r - 1) >= v {
        r -= 1;
    }
    while r < rows.end && centre(r) < v {
        r += 1;
    }
    r
}

/// Summary statistics of raster cells within a zone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ZoneStats {
    /// Number of valid raster cells inside the zone.
    pub count: usize,
    /// Arithmetic sum of valid raster cells inside the zone.
    pub sum: Option<f64>,
    /// Arithmetic mean of valid raster cells inside the zone.
    pub mean: Option<f64>,
    /// Minimum value among valid raster cells inside the zone.
    pub min: Option<f64>,
    /// Maximum value among valid raster cells inside the zone.
    pub max: Option<f64>,
    /// Population standard deviation of valid raster cells inside the zone: sqrt(sum((x - mean)^2) / n).
    pub stddev: Option<f64>,
}

/// Numerically stable online accumulator using Welford's algorithm.
#[derive(Clone, Debug)]
pub struct Accumulator {
    count: usize,
    sum: f64,
    min: f64,
    max: f64,
    mean: f64,
    m2: f64,
}

impl Default for Accumulator {
    /// Constructs an empty accumulator with `min = INFINITY` and `max = NEG_INFINITY`.
    fn default() -> Self {
        Self::new()
    }
}

impl Accumulator {
    /// Constructs a new empty accumulator with `min = INFINITY` and `max = NEG_INFINITY`.
    pub fn new() -> Self {
        Self { count: 0, sum: 0.0, min: f64::INFINITY, max: f64::NEG_INFINITY, mean: 0.0, m2: 0.0 }
    }

    /// Feeds a valid finite measurement into the accumulator, updating count, sum, extrema,
    /// and running mean / variance using Welford's algorithm. Non-finite values are ignored.
    pub fn add(&mut self, val: f64) {
        if !val.is_finite() {
            return;
        }
        self.count += 1;
        self.sum += val;
        if val < self.min {
            self.min = val;
        }
        if val > self.max {
            self.max = val;
        }
        let delta = val - self.mean;
        self.mean += delta / (self.count as f64);
        let delta2 = val - self.mean;
        self.m2 += delta * delta2;
    }

    /// Number of valid finite samples accumulated so far.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Computes and returns the accumulated [`ZoneStats`].
    ///
    /// If no valid values were added (`count == 0`), returns `ZoneStats::default()`
    /// where `count` is 0 and all statistical metrics are `None`.
    pub fn stats(&self) -> ZoneStats {
        if self.count == 0 {
            ZoneStats::default()
        } else {
            let variance = (self.m2 / (self.count as f64)).max(0.0);
            ZoneStats {
                count: self.count,
                sum: Some(self.sum),
                mean: Some(self.mean),
                min: Some(self.min),
                max: Some(self.max),
                stddev: Some(variance.sqrt()),
            }
        }
    }
}

/// Computes summary statistics (count, sum, mean, min, max, stddev) for each requested band
/// over the raster cells whose centres fall inside `polygons` (exterior ring and holes in WGS 84 lon/lat).
///
/// Returns one `ZoneStats` per requested band. Cells with NoData, NaN, or non-finite values are
/// excluded. If no valid cells fall inside the polygon or if the polygon is outside the raster extent,
/// returns `ZoneStats` with `count = 0` and all numeric statistics set to `None`.
pub fn zonal_statistics(r: &Raster, bands: &[usize], polygons: &[Vec<Vec<Coord>>]) -> Result<Vec<ZoneStats>, String> {
    zonal_statistics_with_failures(r, bands, polygons, &mut Vec::new())
}

/// Computes zonal statistics, appending raster-side projection failures as zero-based part indices.
pub(crate) fn zonal_statistics_with_failures(
    r: &Raster,
    bands: &[usize],
    polygons: &[Vec<Vec<Coord>>],
    failed_parts: &mut Vec<usize>,
) -> Result<Vec<ZoneStats>, String> {
    zonal_statistics_from(r, ToWorld::LonLat(r.transformer()), bands, polygons, failed_parts)
}

/// [`zonal_statistics_with_failures`] for polygons whose places reach the raster's system by `to`.
pub(crate) fn zonal_statistics_from(
    r: &Raster,
    to: ToWorld<'_>,
    bands: &[usize],
    polygons: &[Vec<Vec<Coord>>],
    failed_parts: &mut Vec<usize>,
) -> Result<Vec<ZoneStats>, String> {
    r.require_coordinates()?;
    for &b in bands {
        if b >= r.band_count() {
            return Err(format!("band index {} is out of range: {} has {} bands", b + 1, r.name, r.band_count()));
        }
    }
    if bands.is_empty() {
        return Ok(Vec::new());
    }

    let Some((cols, rows, mask)) = polygon_mask_on(to, &r.transform, (r.width, r.height), polygons, failed_parts)? else {
        return Ok(vec![ZoneStats::default(); bands.len()]);
    };

    let w = (cols.end - cols.start) as usize;
    let mut accs: Vec<Accumulator> = (0..bands.len()).map(|_| Accumulator::new()).collect();

    for (ri, row) in rows.enumerate() {
        let line_offset = ri * w;
        let line = match mask.get(line_offset..line_offset + w) {
            Some(l) => l,
            None => continue,
        };
        for (ci, &inside) in line.iter().enumerate() {
            if !inside {
                continue;
            }
            let col = cols.start + ci as u32;
            for (acc_idx, &band) in bands.iter().enumerate() {
                if let Some(v) = r.value(band, col as i64, row as i64)
                    && let Some(acc) = accs.get_mut(acc_idx)
                {
                    acc.add(f64::from(v));
                }
            }
        }
    }

    Ok(accs.into_iter().map(|acc| acc.stats()).collect())
}

/// A tiny arithmetic expression over band values.
enum Expr {
    Num(f64),
    Band(usize),
    Neg(Box<Expr>),
    Bin(Box<Expr>, char, Box<Expr>),
}

impl Expr {
    fn parse(s: &str, bands: usize) -> Result<Expr, String> {
        let toks: Vec<char> = s.chars().filter(|c| !c.is_whitespace()).collect();
        if toks.len() > 2000 {
            return Err("the expression is too long".into());
        }
        let mut pos = 0;
        let e = Self::sum(&toks, &mut pos, bands, 0)?;
        if pos != toks.len() {
            return Err(format!("unexpected '{}' in the expression", toks.get(pos).copied().unwrap_or(' ')));
        }
        Ok(e)
    }
    fn sum(t: &[char], p: &mut usize, n: usize, depth: usize) -> Result<Expr, String> {
        let mut e = Self::product(t, p, n, depth)?;
        while let Some(op) = t.get(*p).copied().filter(|c| matches!(c, '+' | '-')) {
            *p += 1;
            e = Expr::Bin(Box::new(e), op, Box::new(Self::product(t, p, n, depth)?));
        }
        Ok(e)
    }
    fn product(t: &[char], p: &mut usize, n: usize, depth: usize) -> Result<Expr, String> {
        let mut e = Self::atom(t, p, n, depth)?;
        while let Some(op) = t.get(*p).copied().filter(|c| matches!(c, '*' | '/')) {
            *p += 1;
            e = Expr::Bin(Box::new(e), op, Box::new(Self::atom(t, p, n, depth)?));
        }
        Ok(e)
    }
    fn atom(t: &[char], p: &mut usize, n: usize, depth: usize) -> Result<Expr, String> {
        if depth > 50 {
            return Err("the expression nests too deeply".into());
        }
        match t.get(*p).copied() {
            Some('(') => {
                *p += 1;
                let e = Self::sum(t, p, n, depth + 1)?;
                if t.get(*p) != Some(&')') {
                    return Err("a parenthesis isn't closed".into());
                }
                *p += 1;
                Ok(e)
            }
            Some('-') => {
                *p += 1;
                Ok(Expr::Neg(Box::new(Self::atom(t, p, n, depth + 1)?)))
            }
            Some('b' | 'B') => {
                *p += 1;
                let start = *p;
                while t.get(*p).is_some_and(char::is_ascii_digit) {
                    *p += 1;
                }
                let num: String = t.get(start..*p).unwrap_or(&[]).iter().collect();
                let i: usize = num.parse().map_err(|_| "bands are written b1, b2…".to_string())?;
                if i == 0 || i > n {
                    return Err(format!("the raster has no band {i}"));
                }
                Ok(Expr::Band(i - 1))
            }
            Some(c) if c.is_ascii_digit() || c == '.' => {
                let start = *p;
                while t.get(*p).is_some_and(|c| c.is_ascii_digit() || *c == '.') {
                    *p += 1;
                }
                let num: String = t.get(start..*p).unwrap_or(&[]).iter().collect();
                num.parse().map(Expr::Num).map_err(|_| format!("'{num}' isn't a number"))
            }
            other => Err(format!("unexpected '{}' in the expression", other.unwrap_or(' '))),
        }
    }
    fn eval(&self, v: &[f64]) -> f64 {
        match self {
            Expr::Num(x) => *x,
            Expr::Band(i) => v.get(*i).copied().unwrap_or(f64::NAN),
            Expr::Neg(e) => -e.eval(v),
            Expr::Bin(a, op, b) => {
                let (a, b) = (a.eval(v), b.eval(v));
                match op {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    _ if b != 0.0 => a / b,
                    _ => f64::NAN,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grid the machine can't hold is an error naming its size, not the end of the program
    /// (#1291); one it can is the size asked for.
    #[test]
    fn a_grid_too_large_to_hold_is_an_error() {
        let huge = 1usize << 31;
        assert_eq!(grid::<f32>(huge, huge).unwrap_err(), format!("not enough memory for a {huge} × {huge} raster"));
        assert!(new_band(usize::MAX, 2, 0.0f32).is_err_and(|e| e.starts_with("not enough memory")));
        assert_eq!(new_band(3, 2, NODATA), Ok(vec![NODATA; 6]));
        assert!(grid::<f64>(4, 5).is_ok_and(|g| g.is_empty() && g.capacity() >= 20));
    }

    /// A radius reaching far past the raster (the whole DEM's diagonal, as the command takes by
    /// default) counts only the raster's cells: a strip 6,000 cells long, seen from one end, is
    /// worked out, though the square the radius spans holds 144 million cells. So is a radius of
    /// more cells than an `i64` holds.
    #[test]
    fn a_viewshed_radius_past_the_raster_counts_only_its_cells() {
        let transform = GeoTransform { origin_x: 0.0, origin_y: 0.001, pixel_width: 0.001, pixel_height: -0.001 };
        let strip = Raster::new("strip", 6_000, 1, vec![vec![100.0; 6_000]], transform, Crs::LonLat, None, ColorHint::Gray, "Float32")
            .unwrap_or_else(|e| panic!("{e}"));
        let (cx, _) = strip.cell_size_m();
        let vs = viewshed(&strip, 0, Coord::new(0.0005, 0.0005), 2.0, cx * 6_000.0, Heights::Nearest).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(vs.cells.len(), 6_000);
        // Every cell worked out (none left no-data): seen up to the horizon, hidden past it.
        assert!(vs.cells.iter().all(|v| *v == 1.0 || *v == 0.0), "every cell worked out");
        assert!(vs.cells.contains(&1.0) && vs.cells.contains(&0.0), "seen and hidden");
        // A radius of more cells than an i64 holds, on cells declared a millimetre wide.
        let tiny = GeoTransform { pixel_width: 1e-8, pixel_height: -1e-8, ..transform };
        let strip = Raster::new("strip", 50, 1, vec![vec![100.0; 50]], tiny, Crs::LonLat, None, ColorHint::Gray, "Float32")
            .unwrap_or_else(|e| panic!("{e}"));
        let vs = viewshed(&strip, 0, Coord::new(5e-9, 0.001 - 5e-9), 2.0, 1e300, Heights::Nearest).unwrap_or_else(|e| panic!("{e}"));
        assert!(vs.cells.iter().all(|v| *v == 1.0), "{:?}", vs.cells);
    }

    /// A bilinear ray step is weighed as two in the work cap (it reads the two cells it falls
    /// between), and a refusal says so: 1,000 × 1,000 cells from the middle on one thread take
    /// 333,333,500 nearest steps, under the cap, and twice that weighed bilinear, over it. A
    /// radius reaching every cell is refused as the whole raster; one short of the corners, as a
    /// radius, with the steps of the cells within it.
    #[test]
    fn a_bilinear_viewshed_step_is_weighed_as_two_in_the_work_cap() {
        let w = 1_000u32;
        let transform = GeoTransform { origin_x: 0.0, origin_y: 45.0, pixel_width: 0.0003, pixel_height: -0.0003 };
        let dem = Raster::new("dem", w, w, vec![vec![0.0; (w * w) as usize]], transform, Crs::LonLat, None, ColorHint::Gray, "Float32")
            .unwrap_or_else(|e| panic!("{e}"));
        let middle = Coord::new(500.5 * 0.0003, 45.0 - 500.5 * 0.0003);
        let e = viewshed_on(Some(1), &dem, 0, middle, 2.0, 1e9, Heights::Bilinear).unwrap_err();
        assert!(
            e.starts_with(&format!(
                "the whole raster takes up to 666667000 ray steps (a step between cells counted as two), more than the {MAX_VIEWSHED_WORK} this computer takes on 1 thread(s); give a radius to cover only part of it"
            )),
            "{e}"
        );
        // 600 of the longer cell side: past every edge (500 cells away), short of the corners
        // (about 707 cells of the side's mean away; the cells are longer north–south at 45°).
        let (cx, cy) = dem.cell_size_m();
        assert!(600.0 * cx.max(cy) < (500.0 * cx).hypot(500.0 * cy));
        let radius = 600.0 * cx.max(cy);
        let e = viewshed_on(Some(1), &dem, 0, middle, 2.0, radius, Heights::Bilinear).unwrap_err();
        let steps = 2 * brute_ray_steps((500, 500), (0, 999), (0, 999), (cx, cy), radius);
        assert!(
            steps < 666_667_000
                && e.starts_with(&format!("the radius takes up to {steps} ray steps"))
                && e.contains("; use a smaller radius"),
            "{e}"
        );
    }

    /// Each cell's ray steps within the radius, one at a time.
    fn brute_ray_steps(observer: (i64, i64), cols: (i64, i64), rows: (i64, i64), cell: (f64, f64), radius_m: f64) -> u128 {
        let mut n = 0u128;
        for row in rows.0..=rows.1 {
            for col in cols.0..=cols.1 {
                let (dc, dr) = (col - observer.0, row - observer.1);
                if (dc as f64 * cell.0).hypot(dr as f64 * cell.1) <= radius_m {
                    n += u128::from(dc.unsigned_abs().max(dr.unsigned_abs()));
                }
            }
        }
        n
    }

    /// The ray steps counted a row at a time are each cell's, counted one by one: an observer in
    /// the middle, at a corner, on an edge and outside the window's middle row; square and
    /// oblong cells; a radius inside the window, cutting its corners, past it, and on a cell's
    /// centre exactly. Cells and radii so large that the squared radius is past f64's range are
    /// counted as the rest (they once walked a column at a time towards the radius).
    #[test]
    fn viewshed_ray_steps_are_counted_exactly() {
        let window = ((0, 39), (0, 24));
        for observer in [(20, 12), (0, 0), (39, 7), (5, 24)] {
            for cell in [(10.0, 10.0), (7.0, 13.0), (30.0, 2.5)] {
                for radius in [0.5, 10.0, 35.0, 120.0, 260.0, 1e9, 1e300] {
                    assert_eq!(
                        ray_steps(observer, window.0, window.1, cell, radius),
                        brute_ray_steps(observer, window.0, window.1, cell, radius),
                        "{observer:?} {cell:?} {radius}"
                    );
                }
            }
        }
        assert_eq!(
            ray_steps((0, 0), (0, 49), (0, 0), (1e150, 1e150), 1e155),
            brute_ray_steps((0, 0), (0, 49), (0, 0), (1e150, 1e150), 1e155)
        );
        assert_eq!(
            ray_steps((0, 0), (0, 49), (0, 9), (1e182, 1e182), 1e200),
            brute_ray_steps((0, 0), (0, 49), (0, 9), (1e182, 1e182), 1e200)
        );
        for radius in [1.5e151, 1.5e154] {
            let (observer, cell) = ((25, 5), (1e150, 1e150));
            assert_eq!(
                ray_steps(observer, (0, 49), (0, 9), cell, radius),
                brute_ray_steps(observer, (0, 49), (0, 9), cell, radius),
                "{radius}"
            );
        }
    }

    /// Viewshed rows made on several threads see what one thread sees (#1252), with the radius
    /// inside the raster and reaching past its edges, with nearest and bilinear heights.
    #[test]
    fn viewshed_on_several_threads_is_viewshed_on_one() {
        let (w, h) = (90u32, 70u32);
        let data = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                if i % 97 == 5 { -9999.0 } else { 300.0 + 40.0 * (x * 0.21).sin() * (y * 0.17).cos() + 25.0 * ((x + y) * 0.4).sin() }
            })
            .collect();
        let transform = GeoTransform { origin_x: 0.0, origin_y: 45.0, pixel_width: 0.0003, pixel_height: -0.0003 };
        let dem = Raster::new("dem", w, h, vec![data], transform, Crs::LonLat, Some(-9999.0), ColorHint::Gray, "Float32")
            .unwrap_or_else(|e| panic!("{e}"));
        // Visible, hidden and no-data cells, as one thread gives them: a change to the
        // ray walk shows here, not only a difference between thread counts.
        for (at, radius, expected) in
            [(Coord::new(0.012, 44.990), 500.0, [207, 775, 5318]), (Coord::new(0.003, 44.985), 5_000.0, [185, 6050, 65])]
        {
            let one_run = viewshed_on(Some(1), &dem, 0, at, 2.0, radius, Heights::Nearest).unwrap_or_else(|e| panic!("{e}"));
            // Cells seen across the DEM's no-data cells are counted; nearest heights read no
            // neighbours, so none falls back.
            assert!(one_run.through_gaps > 0 && one_run.read_nearest == 0, "radius {radius}: {}", one_run.through_gaps);
            let one = &one_run.cells;
            let counts = [1.0, 0.0, NODATA].map(|k| one.iter().filter(|v| v.to_bits() == k.to_bits()).count());
            assert_eq!(counts, expected, "radius {radius}");
            // Bilinear heights read next to the no-data cells, so some cells take a nearest height.
            let bilinear = viewshed_on(Some(1), &dem, 0, at, 2.0, radius, Heights::Bilinear).unwrap_or_else(|e| panic!("{e}"));
            assert!(bilinear.through_gaps > 0 && bilinear.read_nearest > 0, "radius {radius}: {}", bilinear.read_nearest);
            for threads in [2, 7, 16] {
                for (heights, one_run) in [(Heights::Nearest, &one_run), (Heights::Bilinear, &bilinear)] {
                    let many = viewshed_on(Some(threads), &dem, 0, at, 2.0, radius, heights).unwrap_or_else(|e| panic!("{e}"));
                    let what = format!("{heights:?}, {threads} threads, radius {radius}");
                    assert_eq!((many.through_gaps, many.read_nearest), (one_run.through_gaps, one_run.read_nearest), "{what}");
                    let bits = |c: &[f32]| c.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
                    assert_eq!(bits(&one_run.cells), bits(&many.cells), "{what}");
                }
            }
        }
    }

    /// k-means classes made on several threads are those made on one (#1252), no-data pixels
    /// included.
    #[test]
    fn kmeans_on_several_threads_is_kmeans_on_one() {
        let (w, h) = (240u32, 180u32);
        let n = (w * h) as usize;
        let band = |k: usize| (0..n).map(|i| if i % 97 == 0 { -9999.0 } else { ((i * (5 + k) + i / 240 * 11) % 200) as f32 }).collect();
        let gt = GeoTransform { origin_x: 0.0, origin_y: 180.0, pixel_width: 1.0, pixel_height: -1.0 };
        let r =
            Raster::new("k", w, h, vec![band(0), band(1), band(2)], gt, Crs::LonLat, Some(-9999.0), ColorHint::Gray, "Float32").unwrap();
        let bits = |v: Vec<f32>| v.into_iter().map(f32::to_bits).collect::<Vec<_>>();
        let one = bits(kmeans_on(Some(1), &r, &[0, 1, 2], 7, 10).unwrap());
        assert_eq!(one, bits(kmeans_on(Some(4), &r, &[0, 1, 2], 7, 10).unwrap()));
        assert!(one.contains(&NODATA.to_bits()) && one.contains(&7.0f32.to_bits()));
        // A cancelled run gives no classes.
        let run = crate::control::Control::new();
        run.cancel();
        let _installed = crate::control::install(Some(run));
        assert_eq!(kmeans_on(Some(4), &r, &[0, 1, 2], 7, 10).unwrap_err(), crate::control::CANCELLED);
    }

    /// Zonal means test only the cells in their zone's window (#1252), and give what testing
    /// every cell gives, to the bit: on a longitude/latitude raster and a projected one, with a
    /// hole cut out (and one reaching past its outer ring, which counts nowhere), slanted edges,
    /// and edges through cell centres.
    #[test]
    fn zonal_means_are_those_of_testing_every_cell() {
        // Every cell tested, its centre in longitude/latitude, as zonal means did.
        let every_cell = |r: &Raster, rings: &[Vec<Coord>]| {
            let (mut sums, mut n) = ([0.0f64; 2], 0usize);
            for row in 0..r.height {
                for col in 0..r.width {
                    let Some(ll) = r.transformer().to_lonlat(r.transform.to_world(f64::from(col) + 0.5, f64::from(row) + 0.5)) else {
                        continue;
                    };
                    if rustgis_data::geometry::polygon_contains(rings, ll) {
                        for (b, s) in sums.iter_mut().enumerate() {
                            *s += f64::from(r.value(b, i64::from(col), i64::from(row)).unwrap());
                        }
                        n += 1;
                    }
                }
            }
            (n > 0).then(|| (sums.iter().map(|s| (s / n as f64).to_bits()).collect::<Vec<_>>(), n))
        };
        let ring = |pts: &[(f64, f64)]| pts.iter().map(|&(x, y)| Coord::new(x, y)).collect::<Vec<_>>();
        let (w, h) = (400u32, 300u32);
        let band = |k: u32| (0..w * h).map(|i| ((i * (3 + k) + i / w * 7) % 251) as f32 + 0.25 * k as f32).collect::<Vec<f32>>();
        let lonlat = GeoTransform { origin_x: 7.0, origin_y: 47.0, pixel_width: 0.0037, pixel_height: -0.0029 };
        let merc = GeoTransform { origin_x: 780_000.0, origin_y: 5_950_000.0, pixel_width: 410.0, pixel_height: -390.0 };
        let rasters = [
            Raster::new("lonlat", w, h, vec![band(0), band(1)], lonlat, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap(),
            Raster::new("merc", w, h, vec![band(0), band(1)], merc, Crs::WebMercator, None, ColorHint::Gray, "Float32").unwrap(),
        ];
        let zones = [
            vec![
                ring(&[(7.11, 46.31), (7.93, 46.45), (8.21, 46.88), (7.37, 46.97), (7.05, 46.62)]),
                ring(&[(7.51, 46.61), (7.77, 46.63), (7.71, 46.79), (7.49, 46.74)]),
            ],
            // A hole reaching past its outer ring.
            vec![ring(&[(7.2, 46.3), (7.8, 46.3), (7.8, 46.8), (7.2, 46.8)]), ring(&[(7.6, 46.6), (8.0, 46.6), (8.0, 46.9), (7.6, 46.9)])],
            // Edges through rows' and columns' centres (on the longitude/latitude grid).
            vec![ring(&[
                (7.0 + 0.0037 * 20.5, 47.0 - 0.0029 * 30.5),
                (7.0 + 0.0037 * 90.5, 47.0 - 0.0029 * 30.5),
                (7.0 + 0.0037 * 90.5, 47.0 - 0.0029 * 80.5),
                (7.0 + 0.0037 * 20.5, 47.0 - 0.0029 * 80.5),
            ])],
        ];
        for r in &rasters {
            for (z, rings) in zones.iter().enumerate() {
                let got = zonal_means(r, &[0, 1], rings).unwrap().map(|(m, n)| (m.iter().map(|v| v.to_bits()).collect::<Vec<_>>(), n));
                let want = every_cell(r, rings);
                assert!(want.as_ref().is_some_and(|(_, n)| *n > 100), "{} zone {z}: {want:?}", r.name);
                assert_eq!(got, want, "{} zone {z}", r.name);
            }
        }
        // A wide zone on a polar raster (EPSG:3031): its outer side, the parallel at 60° S, is
        // a circle on the grid, and the raster's 100 m cells straddle it where it reaches
        // furthest, between two of the side's first samples.
        let polar = Crs::Epsg(3031);
        let top = polar.transformer().unwrap().from_lonlat(Coord::new(-0.06, -60.0)).unwrap();
        let gt = GeoTransform { origin_x: top.x - 20_000.0, origin_y: top.y + 15_000.0, pixel_width: 100.0, pixel_height: -100.0 };
        let r = Raster::new("polar", w, h, vec![band(0), band(1)], gt, polar, None, ColorHint::Gray, "Float32").unwrap();
        let wide = vec![ring(&[(-61.0, -70.0), (59.0, -70.0), (59.0, -60.0), (-61.0, -60.0)])];
        let got = zonal_means(&r, &[0, 1], &wide).unwrap().map(|(m, n)| (m.iter().map(|v| v.to_bits()).collect::<Vec<_>>(), n));
        let want = every_cell(&r, &wide);
        assert!(want.as_ref().is_some_and(|(_, n)| *n > 1000 && *n < (w * h) as usize), "{want:?}");
        assert_eq!(got, want, "polar");
        // A zone off the raster has none.
        assert!(zonal_means(&rasters[0], &[0, 1], &[ring(&[(20.0, 10.0), (21.0, 10.0), (21.0, 11.0)])]).unwrap().is_none());
        // Under a control cancelled once it starts, a zone stops between rows (#1562).
        let control = crate::control::Control::cancelled_once_started();
        let res = crate::run_under("Zone", &control, || zonal_means(&rasters[0], &[0, 1], &zones[0]).map_err(String::from));
        assert!(res.is_err() && control.looked() > 0, "{res:?}, looked {}", control.looked());
    }

    /// The single passes the Analysis pane runs on a thread stop when their run is cancelled
    /// (#1291 step 3), and count their progress otherwise.
    #[test]
    fn single_passes_stop_when_cancelled() {
        let (w, h) = (300u32, 200u32);
        let data: Vec<f32> = (0..w * h).map(|i| (i % 97) as f32).collect();
        let gt = GeoTransform { origin_x: 10.0, origin_y: 46.0, pixel_width: 0.001, pixel_height: -0.001 };
        let r = Raster::new("r", w, h, vec![data.clone(), data], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let square = vec![vec![Coord::new(10.01, 45.9), Coord::new(10.05, 45.9), Coord::new(10.05, 45.95), Coord::new(10.01, 45.95)]];
        let polygons = vec![("a".to_string(), square.clone()), ("b".to_string(), square)];
        let means = [(1.0, vec![10.0, 10.0]), (2.0, vec![60.0, 60.0])];
        let ranges = [(0.0, 50.0, 1.0), (50.0, 100.0, 2.0)];
        let passes = |r: &Raster| -> Vec<Result<usize, String>> {
            vec![
                remap(r, 0, &ranges, false).map(|v| v.cells.len()),
                weighted_overlay(&[(r.bands[0].data.clone(), 1.0)]).map(|v| v.len()),
                classify_min_distance(r, &[0, 1], &means).map(|v| v.len()),
                class_means(r, &[0, 1], &polygons).map(|c| c.len()),
                mosaic(&[r], &r.extent(), 0.001, (w as usize, h as usize), 2, true, Crs::LonLat).map(|b| b.len()),
            ]
        };
        let run = crate::control::Control::new();
        {
            let _installed = crate::control::install(Some(run.clone()));
            for (i, done) in passes(&r).into_iter().enumerate() {
                assert!(done.is_ok(), "pass {i}: {done:?}");
                let (n, total) = run.progress().unwrap();
                assert!(total > 0 && n == total, "pass {i}: {n} of {total}");
            }
        }
        run.cancel();
        let _installed = crate::control::install(Some(run));
        for (i, done) in passes(&r).into_iter().enumerate() {
            assert_eq!(done, Err(crate::control::CANCELLED.to_string()), "pass {i}");
        }
        // The class means equal the zonal means they are made of.
        let _none = crate::control::install(None);
        let c = class_means(&r, &[0, 1], &polygons[..1]).unwrap();
        assert_eq!(Some((c[0].1.clone(), c[0].2)), zonal_means(&r, &[0, 1], &polygons[0].1).unwrap());
    }

    /// Band math and per-cell rows made on several threads give the same cells, bit for bit, as
    /// one thread (#1252): no-data, NaN and values that come out non-finite included.
    #[test]
    fn band_math_and_per_cell_on_several_threads_are_on_one() {
        let (w, h) = (173u32, 61u32);
        let band = |k: f32| -> Vec<f32> {
            (0..w * h)
                .map(|i| match i % 97 {
                    0 => NODATA,
                    1 => f32::NAN,
                    _ => ((i % w) as f32 * 0.31 + (i / w) as f32 * 0.7 + k).sin() * 50.0,
                })
                .collect()
        };
        let gt = GeoTransform { origin_x: 7.0, origin_y: 47.0, pixel_width: 0.001, pixel_height: -0.001 };
        let r =
            Raster::new("m", w, h, vec![band(0.0), band(1.5), band(3.0)], gt, Crs::LonLat, Some(NODATA as f64), ColorHint::Gray, "Float32")
                .unwrap();
        let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        for expr in ["(b1 - b2) / (b1 + b2)", "b3 * 2 + 1 / b1"] {
            let one = band_math_on(Some(1), &r, expr).unwrap();
            for threads in [2, 7, 16] {
                assert_eq!(bits(&band_math_on(Some(threads), &r, expr).unwrap()), bits(&one), "{expr}, {threads} threads");
            }
        }
        let f = |c: i64, row: i64| (c % 5 != 0).then(|| r.value(1, c, row).map(|v| f64::from(v) / ((c - row) as f64))).flatten();
        let one = per_cell_on(Some(1), &r, 1, f).unwrap();
        assert!(one.contains(&NODATA) && one.iter().any(|v| *v != NODATA));
        for threads in [2, 7, 16] {
            assert_eq!(bits(&per_cell_on(Some(threads), &r, 1, f).unwrap()), bits(&one), "{threads} threads");
        }
    }

    /// Over flat ground the horizon is where visible light puts it: √(2 · 7/6 R · h), 12.2 km
    /// for an eye 10 m up, as `gdal_viewshed` sees it. The radio waves' 4/3 Earth puts it at 13.0 km.
    #[test]
    fn viewshed_and_line_of_sight_see_the_horizon_of_visible_light() {
        // Sea level on the equator: three rows of 140 cells, 111 m wide.
        let (w, h) = (140u32, 3u32);
        let transform = GeoTransform { origin_x: 0.0, origin_y: 0.0015, pixel_width: 0.001, pixel_height: -0.001 };
        let flat = Raster::new("flat", w, h, vec![vec![0.0; (w * h) as usize]], transform, Crs::LonLat, None, ColorHint::Gray, "Float32")
            .unwrap_or_else(|e| panic!("{e}"));
        let eye = Coord::new(0.0005, 0.0);
        // √(2 · 7/6 · 6,371 km · 10 m), written out so that the test pins the radius.
        let horizon = 12_192.0;
        let vs = viewshed(&flat, 0, eye, 10.0, 20_000.0, Heights::Nearest).unwrap_or_else(|e| panic!("{e}")).cells;
        let row = &vs[w as usize..2 * w as usize];
        let last = row.iter().rposition(|v| *v == 1.0).unwrap_or(0);
        let (cx, _) = flat.cell_size_m();
        assert!((last as f64 * cx - horizon).abs() < cx, "last visible cell {last}, horizon {horizon} m");
        assert!(row[..=last].iter().all(|v| *v == 1.0) && row[last + 1..].iter().all(|v| *v == 0.0), "{row:?}");
        // The ground 11.5 km away is in view; 12.8 km away it is past the horizon.
        let seen = |lon: f64| line_of_sight(&flat, 0, eye, Coord::new(lon, 0.0), 10.0, 0.0, 200).map(|(v, _)| v);
        assert_eq!(seen(0.1035), Some(true));
        assert_eq!(seen(0.1155), Some(false));
    }

    /// Viewsheds on DEMs whose answer is known from their geometry: flat ground (the horizon at
    /// √(2·R′·h)), a bowl (every cell seen), a dome (seen up to where the line from the eye
    /// touches it, at √(h / k′)), and a wall lower than the eye (the cells in its shadow hidden,
    /// those beyond it seen again). The Earth's
    /// curvature for visible light (`SIGHT_RADIUS_M`) is part of each answer. Cells within a cell
    /// and a half of a boundary, where a grid can't say which side a cell is on, aren't counted.
    /// Bilinear heights get every cell counted right on all four; nearest heights hide some of the
    /// dome just inside its horizon, and are right on the other three.
    #[test]
    fn viewsheds_give_the_known_answers_of_flat_ground_a_bowl_a_dome_and_a_wall() {
        // 201 × 201 cells of 0.00027° (about 30 m) on the equator, the observer at the centre.
        let (n, half, step) = (201u32, 100i64, 0.00027);
        let transform = GeoTransform { origin_x: 0.0, origin_y: step * f64::from(n) / 2.0, pixel_width: step, pixel_height: -step };
        let blank = Raster::new("dem", n, n, vec![vec![0.0; (n * n) as usize]], transform, Crs::LonLat, None, ColorHint::Gray, "Float32")
            .unwrap_or_else(|e| panic!("{e}"));
        let (cx, cy) = blank.cell_size_m();
        let at = |col: i64, row: i64| ((col - half) as f64 * cx, (row - half) as f64 * cy);
        let dem = |z: &dyn Fn(f64, f64) -> f64| {
            let mut r = blank.clone();
            if let Some(b) = r.bands.get_mut(0) {
                for (i, v) in b.data.iter_mut().enumerate() {
                    let (x, y) = at(i as i64 % i64::from(n), i as i64 / i64::from(n));
                    *v = z(x, y) as f32;
                }
            }
            r
        };
        let eye = Coord::new(step * (half as f64 + 0.5), step * (f64::from(n) / 2.0 - half as f64 - 0.5));
        let radius = 3_000.0;
        let band = 1.5 * cx.max(cy);
        let drop = |d: f64| d * d / (2.0 * SIGHT_RADIUS_M);
        // Cells seen wrongly, and those counted, for `expected` (None: on a boundary, or outside).
        let wrong = |r: &Raster, h: f64, heights: Heights, expected: &dyn Fn(i64, i64, f64) -> Option<bool>| {
            let vs = viewshed(r, 0, eye, h, radius, heights).unwrap_or_else(|e| panic!("{e}")).cells;
            let (mut bad, mut counted) = (0usize, 0usize);
            for row in 0..i64::from(n) {
                for col in 0..i64::from(n) {
                    let (x, y) = at(col, row);
                    let d = x.hypot(y);
                    let Some(want) = (d > 0.0 && d <= radius).then(|| expected(col, row, d)).flatten() else { continue };
                    let got = vs.get((row * i64::from(n) + col) as usize).copied();
                    counted += 1;
                    bad += usize::from(got != Some(if want { 1.0 } else { 0.0 }));
                }
            }
            (bad, counted)
        };
        let before = |d: f64, edge: f64| if (d - edge).abs() <= band { None } else { Some(d < edge) };

        // Flat ground, an eye 0.5 m up: the horizon at √(2·R′·h), about 2.7 km.
        let flat = dem(&|_, _| 0.0);
        let horizon = (2.0 * SIGHT_RADIUS_M * 0.5).sqrt();
        for heights in [Heights::Nearest, Heights::Bilinear] {
            let (bad, counted) = wrong(&flat, 0.5, heights, &|_, _, d| before(d, horizon));
            assert!(bad == 0 && counted > 20_000, "flat, {heights:?}: {bad} of {counted} wrong");
        }

        // A bowl rising 4 m a kilometre squared (z = k·d², far steeper than the Earth curves
        // away): every cell is seen.
        let bowl = dem(&|x, y| 4e-6 * (x * x + y * y));
        for heights in [Heights::Nearest, Heights::Bilinear] {
            let (bad, counted) = wrong(&bowl, 2.0, heights, &|_, _, _| Some(true));
            assert!(bad == 0 && counted > 20_000, "bowl, {heights:?}: {bad} of {counted} wrong");
        }

        // A dome falling 10 m a kilometre squared from its top, the eye 10 m up there: with the
        // Earth's curve, the ground falls k′ = k + 1/(2R′) a metre squared, and the line from the
        // eye touches it at √(h / k′), about 1 km; beyond, the dome hides the ground.
        let k = 1e-5;
        let dome = dem(&|x, y| 500.0 - k * (x * x + y * y));
        let edge = (10.0 / (k + 1.0 / (2.0 * SIGHT_RADIUS_M))).sqrt();
        let (bad, counted) = wrong(&dome, 10.0, Heights::Bilinear, &|_, _, d| before(d, edge));
        assert!(bad == 0 && counted > 20_000, "dome, bilinear: {bad} of {counted} wrong");
        let (bad_nearest, _) = wrong(&dome, 10.0, Heights::Nearest, &|_, _, d| before(d, edge));
        assert!(bad_nearest > 0, "nearest heights hide some of the dome just inside its horizon");

        // A wall 1 m high, a column of cells 20 cells (about 600 m) east of an eye 2 m up: beyond it,
        // a cell is hidden while the line from the eye over the wall's top passes above it, and
        // seen again once the line has come down below the ground (at about 1.2 km on a flat
        // Earth, a little farther with its curve). Rays mostly east or west,
        // which cross the wall at a cell's centre line, are counted; others cross it between
        // columns, where it stands depends on how the ground is read.
        let wall_col = half + 20;
        let wall = dem(&|x, _| if (x - 20.0 * cx).abs() < cx / 2.0 { 1.0 } else { 0.0 });
        let z0 = 2.0;
        // How far the line over the wall's top passes above the cell (negative: below it).
        let over = |col: i64, d: f64| {
            let dw = d * (20.0 / (col - half) as f64);
            (1.0 - drop(dw) - z0) / dw * d - (-drop(d) - z0)
        };
        let shadow = |col: i64, row: i64, d: f64| -> Option<bool> {
            let (dc, dr) = (col - half, row - half);
            if col == wall_col || dr.abs() > dc.abs() {
                return None;
            }
            if col < wall_col {
                return Some(true);
            }
            let over = over(col, d);
            if over.abs() < 0.05 { None } else { Some(over < 0.0) }
        };
        // Both sides of the shadow's edge are counted, not only the shadow.
        let (mut hidden, mut seen_past) = (0usize, 0usize);
        for row in 0..i64::from(n) {
            for col in wall_col + 1..i64::from(n) {
                let (x, y) = at(col, row);
                let d = x.hypot(y);
                match (d <= radius).then(|| shadow(col, row, d)).flatten() {
                    Some(false) => hidden += 1,
                    Some(true) => seen_past += 1,
                    None => {}
                }
            }
        }
        assert!(hidden > 1_000 && seen_past > 1_000, "{hidden} hidden, {seen_past} seen past the wall");
        for heights in [Heights::Nearest, Heights::Bilinear] {
            let (bad, counted) = wrong(&wall, z0, heights, &shadow);
            assert!(bad == 0 && counted > 10_000, "wall, {heights:?}: {bad} of {counted} wrong");
        }
    }

    /// A run past one thread's work, on one thread, is refused, naming the cap and the threads.
    #[test]
    fn idw_past_one_threads_work_is_refused_on_one_naming_the_cap() {
        // 1000 × 1000 cells from 500 points: 500 million pairs, past one thread's 400 million.
        let pts: Vec<(Coord, f64)> = (0..500).map(|i| (Coord::new((i % 25) as f64 / 2.5, (i / 25) as f64 / 2.0), i as f64)).collect();
        let extent = Extent::new(0.0, 0.0, 10.0, 10.0);
        let e = idw_on(Some(1), &pts, &extent, 0.01, 2.0, 0, "idw", Crs::LonLat).unwrap_err();
        assert!(
            e.contains("too much work") && e.contains(&format!("{MAX_IDW_WORK} cell-point pairs")) && e.contains("on 1 thread(s)"),
            "{e}"
        );
        // A run that can be cancelled takes it on: cancelled before its first row, it says so.
        let run = crate::control::Control::stoppable();
        run.cancel();
        let _installed = crate::control::install(Some(run));
        assert_eq!(idw_on(Some(1), &pts, &extent, 0.01, 2.0, 0, "idw", Crs::LonLat).unwrap_err(), crate::control::CANCELLED);
    }

    /// Rows made on several threads give the same surface, bit for bit, as one thread (#1252),
    /// from every point and from the nearest few.
    #[test]
    fn idw_on_several_threads_is_idw_on_one() {
        let pts: Vec<(Coord, f64)> = (0..500)
            .map(|i| {
                let (x, y) = ((i * 37 % 500) as f64 / 50.0, (i * 91 % 500) as f64 / 50.0);
                (Coord::new(x, 40.0 + y), (x * 1.3).sin() * 100.0 + y * 7.0)
            })
            .collect();
        let extent = Extent::new(0.0, 40.0, 10.0, 50.0);
        // At a power of 2 (squared distances) and another (the general path).
        for (power, neighbours) in [(2.0, 0), (2.0, 8), (3.0, 0), (3.0, 8)] {
            let one = idw_on(Some(1), &pts, &extent, 0.1, power, neighbours, "idw", Crs::LonLat).unwrap_or_else(|e| panic!("{e}"));
            for threads in [2, 7, 16] {
                let many =
                    idw_on(Some(threads), &pts, &extent, 0.1, power, neighbours, "idw", Crs::LonLat).unwrap_or_else(|e| panic!("{e}"));
                let bits = |r: &Raster| r.bands[0].data.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
                assert_eq!(bits(&one), bits(&many), "power {power}, {threads} threads, {neighbours} neighbours");
            }
        }
    }

    /// At a power of 2 the weights come from squared distances, with no square root or power
    /// (#1252): the surface is the general path's to within rounding, from every point and from
    /// the nearest few, and a cell on a point still takes its value.
    #[test]
    fn idw_at_power_two_is_the_general_paths_surface() {
        let pts: Vec<(Coord, f64)> = (0..400)
            .map(|i| {
                let (x, y) = ((i * 37 % 400) as f64 / 40.0, (i * 91 % 400) as f64 / 40.0);
                (Coord::new(x, 40.0 + y), (x * 1.3).sin() * 100.0 + y * 7.0)
            })
            .chain([(Coord::new(0.05, 49.95), 1234.5)])
            .collect();
        let extent = Extent::new(0.0, 40.0, 10.0, 50.0);
        for neighbours in [0, 8] {
            let fast = idw_on(Some(1), &pts, &extent, 0.1, 2.0, neighbours, "idw", Crs::LonLat).unwrap_or_else(|e| panic!("{e}"));
            // The general path can't be asked for at a power of 2, and one near 2 has other
            // weights: the reference is made here, as the general path made it at 2.
            let k = if neighbours == 0 { pts.len() } else { neighbours };
            let (w, h) = (100usize, 100usize);
            for row in 0..h {
                let kx = (extent.max_y - (row as f64 + 0.5) * 0.1).to_radians().cos();
                for col in 0..w {
                    let c = Coord::new(extent.min_x + (col as f64 + 0.5) * 0.1, extent.max_y - (row as f64 + 0.5) * 0.1);
                    let mut near: Vec<(f64, f64)> = pts.iter().map(|(p, v)| (((p.x - c.x) * kx).hypot(p.y - c.y), *v)).collect();
                    near.sort_by(|a, b| a.0.total_cmp(&b.0));
                    // Points tied with the last one kept (as far as rounding goes): either may be
                    // the one picked, so each is a right answer.
                    let last = near[k - 1].0;
                    let tied: Vec<(f64, f64)> = near[k - 1..].iter().copied().filter(|(d, _)| (d - last).abs() <= last * 1e-12).collect();
                    let surface = |chosen: &[(f64, f64)]| {
                        (match chosen.iter().find(|(d, _)| *d < 1e-12) {
                            Some(on) => on.1,
                            None => {
                                let nearest = chosen.iter().map(|(d, _)| *d).fold(f64::INFINITY, f64::min);
                                let (n, d) = chosen.iter().fold((0.0, 0.0), |(n, d), (dist, v)| {
                                    let wgt = (nearest / dist).powf(2.0);
                                    (n + wgt * v, d + wgt)
                                });
                                n / d
                            }
                        }) as f32
                    };
                    let got = fast.bands[0].data[row * w + col];
                    let wants: Vec<f32> = tied
                        .iter()
                        .map(|t| {
                            let mut chosen = near[..k - 1].to_vec();
                            chosen.push(*t);
                            surface(&chosen)
                        })
                        .collect();
                    assert!(
                        wants.iter().any(|want| (got - want).abs() <= want.abs() * 1e-6 + 1e-6),
                        "({col}, {row}), {neighbours} neighbours: {got} vs {wants:?}"
                    );
                }
            }
            // The cell centred on the last point takes its value.
            assert_eq!(fast.bands[0].data[0], 1234.5, "{neighbours} neighbours");
        }
    }

    #[test]
    fn terrain_rows_match_scalar_edges_nodata_and_orientations() {
        for (w, h) in [(1, 1), (1, 5), (2, 3), (3, 2), (3, 3), (9, 7), (60, 41)] {
            for (pixel_width, pixel_height) in [(0.01, -0.01), (0.01, 0.01), (-0.01, -0.01), (-0.01, 0.01)] {
                for with_holes in [false, true] {
                    let values = (0..w * h)
                        .map(|i| {
                            if with_holes && i % 7 == 3 {
                                -9999.0
                            } else if with_holes && i % 11 == 4 {
                                f32::NAN
                            } else {
                                (i * 31 % 53) as f32
                            }
                        })
                        .collect();
                    let transform = GeoTransform { origin_x: 0.0, origin_y: 45.0, pixel_width, pixel_height };
                    let r = Raster::new("terrain", w, h, vec![values], transform, Crs::LonLat, Some(-9999.0), ColorHint::Gray, "Float32")
                        .unwrap_or_else(|e| panic!("{e}"));
                    let data = band(&r, 0).unwrap_or_default();
                    for zf in [0.0, 1.0, 2.5] {
                        let f = |x: f64, y: f64| x.hypot(y).atan().to_degrees();
                        let expected = per_cell(&r, 1, |col, row| {
                            z(&r, data, col, row)?;
                            let (x, y) = gradient(&r, data, col, row, zf, r.cell_size_m())?;
                            Some(f(x, y))
                        });
                        assert_eq!(
                            terrain(&r, data, zf, f),
                            expected,
                            "{w}x{h}, {pixel_width}, {pixel_height}, holes={with_holes}, zf={zf}"
                        );
                        // Rows made on several threads give the same, bit for bit (#1252).
                        let bits = |v: Result<Vec<f32>, String>| v.map(|v| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>());
                        for threads in [1, 2, 7, 16] {
                            assert_eq!(
                                bits(terrain_on(Some(threads), &r, data, zf, f)),
                                bits(expected.clone()),
                                "{w}x{h}, {threads} threads"
                            );
                        }
                    }
                }
            }
        }
    }

    /// A 20×20 lon/lat raster: a ramp rising 10 m per cell to the east, and a second band.
    fn ramp() -> Raster {
        let (w, h) = (20u32, 20u32);
        let a: Vec<f32> = (0..w * h).map(|i| (i % w) as f32 * 10.0).collect();
        let b: Vec<f32> = (0..w * h).map(|i| (i / w) as f32).collect();
        let gt = GeoTransform { origin_x: 0.0, origin_y: 0.02, pixel_width: 0.001, pixel_height: -0.001 };
        Raster::new("ramp", w, h, vec![a, b], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn terrain_keeps_edge_and_nodata_results() {
        let values = vec![0.0, 4.0, 3.0, 8.0, 2.0, -9999.0, 9.0, 7.0, 8.0, 5.0, 2.0, 1.0];
        let transform = GeoTransform { origin_x: 12.0, origin_y: 45.0, pixel_width: 0.002, pixel_height: -0.001 };
        let r = Raster::new("sample", 4, 3, vec![values], transform, Crs::LonLat, Some(-9999.0), ColorHint::Gray, "Float32")
            .unwrap_or_else(|e| panic!("{e}"));
        let expected_slope = [
            0.70946753, 1.3399616, 1.4611784, 0.93366396, 2.4284103, NODATA, 0.82372725, 2.1364474, 1.4713285, 0.75683224, 1.718825,
            2.4512568,
        ];
        let expected_aspect =
            [285.89474, 305.4469, 311.66492, 288.18884, 358.39075, NODATA, 160.65001, 180.0, 7.9960175, 97.375114, 163.86998, 172.00398];
        for (actual, expected) in
            [(slope(&r, 0, 1.5).unwrap_or_default(), expected_slope), (aspect(&r, 0).unwrap_or_default(), expected_aspect)]
        {
            assert_eq!(actual.len(), expected.len());
            assert!(actual.iter().zip(expected).all(|(a, e)| *a == e || (*a - e).abs() < 1e-4));
        }
        assert_eq!(
            hillshade(&r, 0, 315.0, 45.0, 1.5).unwrap_or_default(),
            [182.0, 184.0, 185.0, 183.0, 186.0, NODATA, 178.0, 175.0, 183.0, 178.0, 175.0, 174.0]
        );
    }

    /// A cell in full shadow is 1, not 0, so that the bytes made of a hillshade keep 0 for its
    /// no-data cells.
    #[test]
    fn hillshade_keeps_zero_for_no_data() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 0.001, pixel_height: -0.001 };
        let mut cells = vec![10.0; 25];
        cells[12] = -9999.0;
        let r = Raster::new("plane", 5, 5, vec![cells], gt, Crs::LonLat, Some(-9999.0), ColorHint::Gray, "Float32")
            .unwrap_or_else(|e| panic!("{e}"));
        // The sun on the horizon lights no flat ground.
        let shade = hillshade(&r, 0, 315.0, 0.0, 1.0).unwrap_or_default();
        assert_eq!((shade[0], shade[12]), (1.0, NODATA));
        let out = derive_hillshade(&r, "shade", shade).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((out.data_type.as_str(), out.nodata), ("UInt8", Some(0.0)));
        assert_eq!((out.value(0, 0, 0), out.value(0, 2, 2), out.bands[0].data[12]), (Some(1.0), None, 0.0));
        assert_eq!(out.bands[0].stats.count, 24);
    }

    #[test]
    fn polygon_mask_fills_polygons_and_cuts_holes() {
        let r = ramp();
        let sq = |x0: f64, y0: f64, x1: f64, y1: f64| {
            vec![Coord::new(x0, y0), Coord::new(x1, y0), Coord::new(x1, y1), Coord::new(x0, y1), Coord::new(x0, y0)]
        };
        // A square with a hole, and a second polygon in the south-west corner.
        let polys = vec![vec![sq(0.005, 0.005, 0.015, 0.015), sq(0.009, 0.009, 0.011, 0.011)], vec![sq(0.0, 0.0, 0.002, 0.002)]];
        let (cols, rows, mask) = polygon_mask(&r, &polys).unwrap().unwrap_or_else(|| panic!("no mask"));
        assert_eq!((cols.clone(), rows.clone()), (0..15, 5..20));
        assert_eq!(mask.iter().filter(|m| **m).count(), 100 - 4 + 4);
        let at = |col: u32, row: u32| mask[((row - rows.start) * 15 + col - cols.start) as usize];
        // Rows count down from the north edge (0.02): row 5 is the square's northern row.
        assert!(at(5, 5) && at(14, 14) && !at(4, 5) && !at(14, 15));
        assert!(!at(9, 9) && !at(10, 10), "the hole stays out");
        assert!(at(0, 19) && at(1, 18) && !at(2, 18));
        // Outside the raster, or too small to hold a cell centre: nothing.
        assert!(polygon_mask(&r, &[vec![sq(1.0, 1.0, 2.0, 2.0)]]).unwrap().is_none());
        assert!(polygon_mask(&r, &[vec![sq(0.0051, 0.0051, 0.0052, 0.0052)]]).unwrap().is_none());
    }

    /// The mask equals one found the plain way, every edge on every row, for polygons of many
    /// shapes: holes, several parts, edges on cell edges, crossing themselves, past the grid.
    #[test]
    fn polygon_mask_matches_a_scan_of_every_edge_on_every_row() {
        let plain = |parts: &[Vec<Vec<(f64, f64)>>], width: u32, height: u32| {
            let mut mask = vec![false; (width * height) as usize];
            for row in 0..height {
                let y = f64::from(row) + 0.5;
                for rings in parts {
                    let mut xs = Vec::new();
                    for ring in rings {
                        for (i, &(ax, ay)) in ring.iter().enumerate() {
                            let (bx, by) = ring.get(i + 1).or(ring.first()).copied().unwrap_or((ax, ay));
                            if (ay <= y) != (by <= y) {
                                xs.push(ax + (y - ay) * (bx - ax) / (by - ay));
                            }
                        }
                    }
                    xs.sort_by(f64::total_cmp);
                    for &[x0, x1] in xs.as_chunks::<2>().0 {
                        let at = |x: f64| (x - 0.5).ceil().clamp(0.0, f64::from(width)) as u32;
                        for col in at(x0)..at(x1) {
                            mask[(row * width + col) as usize] = true;
                        }
                    }
                }
            }
            mask
        };
        let (width, height) = (60u32, 40u32);
        let g = crate::warp::Grid { crs: Crs::LonLat, min_x: 0.0, max_y: 40.0, cell_x: 1.0, cell_y: 1.0, width: 60, height: 40 };
        let t = g.transform();
        let tr = Crs::LonLat.transformer().unwrap();
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move |n: f64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64 * n
        };
        for case in 0..400 {
            let mut polygons = Vec::new();
            for _ in 0..1 + case % 3 {
                let mut rings = Vec::new();
                for _ in 0..1 + case % 2 {
                    let mut ring: Vec<Coord> = (0..3 + case % 9)
                        .map(|_| {
                            // Whole numbers land on cell edges, halves on cell centres; the range
                            // goes past the grid.
                            let (x, y) = (next(80.0) - 10.0, next(60.0) - 10.0);
                            match case % 4 {
                                0 => Coord::new(x.floor(), y.floor()),
                                1 => Coord::new(x.floor() + 0.5, y.floor() + 0.5),
                                _ => Coord::new(x, y),
                            }
                        })
                        .collect();
                    ring.push(ring[0]);
                    rings.push(ring);
                }
                polygons.push(rings);
            }
            let found = polygon_mask_on(ToWorld::LonLat(&tr), &t, (width, height), &polygons, &mut Vec::new()).unwrap();
            let parts: Vec<Vec<Vec<(f64, f64)>>> = polygons
                .iter()
                .map(|rings| rings.iter().map(|ring| project_ring(ToWorld::LonLat(&tr), &t, ring).unwrap()).collect())
                .collect();
            let want = plain(&parts, width, height);
            let mut got = vec![false; want.len()];
            if let Some((cols, rows, mask)) = found {
                let w = (cols.end - cols.start) as usize;
                assert_eq!(mask.len(), w * (rows.end - rows.start) as usize);
                for (i, inside) in mask.iter().enumerate() {
                    let (c, r) = (cols.start as usize + i % w, rows.start as usize + i / w);
                    got[r * width as usize + c] = *inside;
                }
            }
            assert_eq!(got, want, "case {case}");
        }
    }

    /// A failed hole drops only its containing part and reports that part, even with no mask.
    #[test]
    fn polygon_mask_reports_rejected_parts_and_retains_valid_parts() {
        let r = ramp();
        let valid = vec![vec![Coord::new(0.0, 0.0), Coord::new(0.002, 0.0), Coord::new(0.002, 0.002), Coord::new(0.0, 0.002)]];
        let mut invalid = valid.clone();
        invalid.push(vec![Coord::new(f64::NAN, 0.0); 3]);
        let polys = vec![invalid.clone(), valid.clone()];
        let mut failures = Vec::new();
        let mask = polygon_mask_with_failures(&r, &polys, &mut failures);
        assert_eq!(mask, polygon_mask(&r, &[valid]));
        assert_eq!(failures, vec![0]);
        failures.clear();
        let stats = zonal_statistics_with_failures(&r, &[0], &polys, &mut failures).unwrap();
        assert_eq!(stats[0].count, 4);
        assert_eq!(failures, vec![0]);
        failures.clear();
        assert!(polygon_mask_with_failures(&r, &[invalid], &mut failures).unwrap().is_none());
        assert_eq!(failures, vec![0]);
    }

    #[test]
    fn terrain_faces_the_right_way_on_a_bottom_up_grid() {
        // Band 2 rises one per row. North up, rows run south, so the slope faces north; stored
        // bottom up (a positive pixel height, as some COGs are), rows run north and it faces south.
        let north_up = ramp();
        let mut gt = north_up.transform;
        (gt.origin_y, gt.pixel_height) = (0.0, 0.001);
        let bands = north_up.bands.iter().map(|b| b.data.clone()).collect();
        let bottom_up =
            Raster::new("ramp", 20, 20, bands, gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap_or_else(|e| panic!("{e}"));
        let a = aspect(&north_up, 1).unwrap_or_default();
        assert!(a[210] < 1.0 || a[210] > 359.0, "north up faces north: {}", a[210]);
        let a = aspect(&bottom_up, 1).unwrap_or_default();
        assert!((a[210] - 180.0).abs() < 1.0, "bottom up faces south: {}", a[210]);
        let lit_s = hillshade(&bottom_up, 1, 180.0, 45.0, 1.0).unwrap_or_default();
        let lit_n = hillshade(&bottom_up, 1, 0.0, 45.0, 1.0).unwrap_or_default();
        assert!(lit_s[210] > lit_n[210], "lit from the south the south-facing slope is brighter");
    }

    #[test]
    fn terrain_math_and_classes() {
        let r = ramp();
        let s = slope(&r, 0, 1.0).unwrap_or_default();
        // 10 m rise per ~111 m: about 5.1 degrees.
        let mid = s[10 * 20 + 10];
        assert!((mid - 5.1).abs() < 0.3, "{mid}");
        let a = aspect(&r, 0).unwrap_or_default();
        assert!((a[10 * 20 + 10] - 270.0).abs() < 1.0, "facing west: {}", a[210]);
        let hs = hillshade(&r, 0, 270.0, 45.0, 1.0).unwrap_or_default();
        let hs_e = hillshade(&r, 0, 90.0, 45.0, 1.0).unwrap_or_default();
        assert!(hs[210] > hs_e[210], "lit from the west the west-facing slope is brighter");
        let m = band_math(&r, "(b1 - b2) / (b1 + b2 + 1) * 2").unwrap_or_default();
        assert!((m[20 + 3] - ((30.0 - 1.0) / 32.0 * 2.0)).abs() < 1e-5);
        assert!(band_math(&r, "b3 + 1").is_err() && band_math(&r, "(b1").is_err() && band_math(&r, "b1 $ 2").is_err());
        let rm = remap(&r, 0, &[(0.0, 100.0, 1.0), (100.0, 1000.0, 2.0)], false).unwrap().cells;
        assert_eq!((rm[5], rm[15]), (1.0, 2.0));
        let o = weighted_overlay(&[(rm.clone(), 3.0), (vec![1.0; 400], 1.0)]).unwrap_or_default();
        assert!((o[15] - 1.75).abs() < 1e-6);
        let k = kmeans(&r, &[0], 2, 10).unwrap_or_default();
        assert_eq!((k[0], k[19]), (1.0, 2.0));
        // A point east of the ridge can't see over a wall.
        let mut wall = ramp();
        if let Some(b) = wall.bands.get_mut(0) {
            for (i, v) in b.data.iter_mut().enumerate() {
                *v = if i % 20 == 10 { 500.0 } else { 0.0 };
            }
        }
        let vs = viewshed(&wall, 0, Coord::new(0.0025, 0.0105), 2.0, 5_000.0, Heights::Nearest).unwrap_or_default().cells;
        assert_eq!(vs[10 * 20 + 5], 1.0);
        assert_eq!(vs[10 * 20 + 15], 0.0);
        let (seen, prof) =
            line_of_sight(&wall, 0, Coord::new(0.0025, 0.0105), Coord::new(0.0175, 0.0105), 2.0, 0.0, 50).unwrap_or((true, vec![]));
        assert!(!seen && !prof.is_empty());
        let g = idw(
            &[(Coord::new(0.0, 0.0), 0.0), (Coord::new(1.0, 0.0), 10.0)],
            &Extent::new(0.0, -0.5, 1.0, 0.5),
            0.1,
            2.0,
            8,
            "idw",
            Crs::LonLat,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((g.width, g.height), (10, 10));
        let row = &g.bands[0].data[50..60];
        assert!(row[0] < row[9] && row[0] > 0.0 && row[9] < 10.0, "{row:?}");
        // Every point (0), and a power high enough to overflow 1/dᵖ, still give finite values.
        let all = idw(
            &[(Coord::new(0.0, 0.0), 0.0), (Coord::new(1.0, 0.0), 10.0)],
            &Extent::new(0.0, -0.5, 1.0, 0.5),
            0.1,
            99.0,
            0,
            "idw",
            Crs::LonLat,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let row = &all.bands[0].data[50..60];
        assert!(row.iter().all(|v| (0.0..=10.0).contains(v)) && row[0] < 1e-3 && row[9] > 10.0 - 1e-3, "{row:?}");
        // With every point, a far one still pulls each cell off the two near ones' value.
        let pts = [(Coord::new(0.0, 0.0), 0.0), (Coord::new(1.0, 0.0), 0.0), (Coord::new(5.0, 0.0), 100.0)];
        let all = idw(&pts, &Extent::new(0.0, -0.5, 1.0, 0.5), 0.1, 1.0, 0, "idw", Crs::LonLat).unwrap_or_else(|e| panic!("{e}"));
        assert!(all.bands[0].data.iter().all(|v| *v > 0.0 && *v < 100.0), "{:?}", all.bands[0].data);
        let p = profile(&r, 0, &[Coord::new(0.0005, 0.01), Coord::new(0.0195, 0.01)], 20);
        assert_eq!(p.len(), 20);
        assert!(p.last().and_then(|x| x.2).unwrap_or(0.0) > p[0].2.unwrap_or(0.0) + 150.0);
    }

    #[test]
    fn accumulator_exact_math_and_numerical_stability() {
        // 1. Exact math on [10.0, 20.0, 30.0, 40.0]
        let mut acc = Accumulator::new();
        for &x in &[10.0, 20.0, 30.0, 40.0] {
            acc.add(x);
        }
        let st = acc.stats();
        assert_eq!(st.count, 4);
        assert_eq!(st.sum, Some(100.0));
        assert_eq!(st.mean, Some(25.0));
        assert_eq!(st.min, Some(10.0));
        assert_eq!(st.max, Some(40.0));
        let expected_stddev = 125.0f64.sqrt(); // sqrt(500 / 4)
        assert!((st.stddev.unwrap() - expected_stddev).abs() < 1e-12);

        // 2. Empty accumulator
        let empty_st = Accumulator::new().stats();
        assert_eq!(empty_st.count, 0);
        assert_eq!(empty_st.sum, None);
        assert_eq!(empty_st.mean, None);
        assert_eq!(empty_st.min, None);
        assert_eq!(empty_st.max, None);
        assert_eq!(empty_st.stddev, None);

        // 3. Single value
        let mut single_acc = Accumulator::new();
        single_acc.add(42.5);
        let single_st = single_acc.stats();
        assert_eq!(single_st.count, 1);
        assert_eq!(single_st.sum, Some(42.5));
        assert_eq!(single_st.mean, Some(42.5));
        assert_eq!(single_st.min, Some(42.5));
        assert_eq!(single_st.max, Some(42.5));
        assert_eq!(single_st.stddev, Some(0.0));

        // 4. Constant values
        let mut const_acc = Accumulator::new();
        for _ in 0..100 {
            const_acc.add(7.0);
        }
        let const_st = const_acc.stats();
        assert_eq!(const_st.count, 100);
        assert_eq!(const_st.mean, Some(7.0));
        assert_eq!(const_st.stddev, Some(0.0));

        // 5. Large magnitude values (Welford's algorithm avoids catastrophic cancellation)
        let base = 1e12;
        let mut large_acc = Accumulator::new();
        for &x in &[base + 1.0, base + 2.0, base + 3.0, base + 4.0] {
            large_acc.add(x);
        }
        let large_st = large_acc.stats();
        assert_eq!(large_st.count, 4);
        assert_eq!(large_st.mean, Some(base + 2.5));
        let std_large = large_st.stddev.unwrap();
        // Variance of [1, 2, 3, 4] is 1.25, stddev is sqrt(1.25) = 1.118033988749895
        assert!((std_large - 1.25f64.sqrt()).abs() < 1e-10, "large magnitude precision: {std_large}");

        // 6. Non-finite values (NaN and inf) are rejected
        let mut defensive_acc = Accumulator::new();
        defensive_acc.add(10.0);
        defensive_acc.add(f64::NAN);
        defensive_acc.add(f64::INFINITY);
        defensive_acc.add(f64::NEG_INFINITY);
        defensive_acc.add(20.0);
        let def_st = defensive_acc.stats();
        assert_eq!(def_st.count, 2);
        assert_eq!(def_st.sum, Some(30.0));
        assert_eq!(def_st.mean, Some(15.0));
    }

    #[test]
    fn accumulator_default_and_new_consistency_and_extrema() {
        // default() and new() produce identical initial empty state
        let d = Accumulator::default();
        let n = Accumulator::new();
        assert_eq!(d.count(), 0);
        assert_eq!(n.count(), 0);
        assert_eq!(d.stats(), ZoneStats::default());
        assert_eq!(n.stats(), ZoneStats::default());

        // Negative-only values: min must be -30, max must be -10 (not 0.0)
        let mut neg_acc = Accumulator::default();
        neg_acc.add(-30.0);
        neg_acc.add(-20.0);
        neg_acc.add(-10.0);
        let neg_st = neg_acc.stats();
        assert_eq!(neg_st.count, 3);
        assert_eq!(neg_st.min, Some(-30.0));
        assert_eq!(neg_st.max, Some(-10.0));
        assert_eq!(neg_st.sum, Some(-60.0));
        assert_eq!(neg_st.mean, Some(-20.0));

        // Positive-only values: min must be 10 (not 0.0), max must be 30
        let mut pos_acc = Accumulator::default();
        pos_acc.add(10.0);
        pos_acc.add(20.0);
        pos_acc.add(30.0);
        let pos_st = pos_acc.stats();
        assert_eq!(pos_st.count, 3);
        assert_eq!(pos_st.min, Some(10.0));
        assert_eq!(pos_st.max, Some(30.0));

        // Mixed positive and negative values
        let mut mixed_acc = Accumulator::default();
        mixed_acc.add(-5.0);
        mixed_acc.add(5.0);
        let mixed_st = mixed_acc.stats();
        assert_eq!(mixed_st.count, 2);
        assert_eq!(mixed_st.min, Some(-5.0));
        assert_eq!(mixed_st.max, Some(5.0));
        assert_eq!(mixed_st.sum, Some(0.0));
        assert_eq!(mixed_st.mean, Some(0.0));
    }

    #[test]
    fn zonal_statistics_known_cells_nodata_and_holes() {
        let r = ramp(); // 20x20 raster, band 0: (i % 20) * 10, band 1: i / 20.
        let sq = |x0: f64, y0: f64, x1: f64, y1: f64| {
            vec![Coord::new(x0, y0), Coord::new(x1, y0), Coord::new(x1, y1), Coord::new(x0, y1), Coord::new(x0, y0)]
        };

        // 1. Single known cell: col 5, row 5.
        // origin_x = 0.0, origin_y = 0.02, pixel_width = 0.001, pixel_height = -0.001
        // col 5 x in [0.005, 0.006], centre at 0.0055.
        // row 5 y in [0.014, 0.015], centre at 0.0145.
        let cell_poly = vec![vec![sq(0.005, 0.014, 0.006, 0.015)]];
        let stats = zonal_statistics(&r, &[0, 1], &cell_poly).unwrap();
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].count, 1);
        assert_eq!(stats[0].sum, Some(50.0)); // col 5 * 10 = 50
        assert_eq!(stats[0].mean, Some(50.0));
        assert_eq!(stats[0].min, Some(50.0));
        assert_eq!(stats[0].max, Some(50.0));
        assert_eq!(stats[0].stddev, Some(0.0));

        assert_eq!(stats[1].count, 1);
        assert_eq!(stats[1].sum, Some(5.0)); // row 5 = 5
        assert_eq!(stats[1].mean, Some(5.0));
        assert_eq!(stats[1].stddev, Some(0.0));

        // 2. Polygon outside raster: empty zone result
        let outside_poly = vec![vec![sq(1.0, 1.0, 2.0, 2.0)]];
        let outside_stats = zonal_statistics(&r, &[0], &outside_poly).unwrap();
        assert_eq!(outside_stats.len(), 1);
        assert_eq!(outside_stats[0], ZoneStats::default());

        // 3. Polygon with holes
        let poly_with_hole = vec![vec![sq(0.005, 0.005, 0.015, 0.015), sq(0.009, 0.009, 0.011, 0.011)]];
        let hole_stats = zonal_statistics(&r, &[0], &poly_with_hole).unwrap();
        // 100 cells - 4 hole cells = 96 cells
        assert_eq!(hole_stats[0].count, 96);
        assert!(hole_stats[0].sum.is_some());
        assert!(hole_stats[0].mean.is_some());

        // 4. MultiPolygon
        let multipoly = vec![
            vec![sq(0.001, 0.018, 0.003, 0.019)], // 2 cells
            vec![sq(0.005, 0.018, 0.007, 0.019)], // 2 cells
        ];
        let multi_stats = zonal_statistics(&r, &[0], &multipoly).unwrap();
        assert_eq!(multi_stats[0].count, 4);

        // 5. NoData handling: create a raster with NoData values
        let (w, h) = (4u32, 4u32);
        // cells:
        // [ 10,   20, -9999,   40 ]
        // [ 50,  NaN,   70,   80 ]
        // [ -9999, -9999, -9999, -9999 ]
        // [ 130, 140, 150, 160 ]
        let data: Vec<f32> =
            vec![10.0, 20.0, -9999.0, 40.0, 50.0, f32::NAN, 70.0, 80.0, -9999.0, -9999.0, -9999.0, -9999.0, 130.0, 140.0, 150.0, 160.0];
        let gt = GeoTransform { origin_x: 0.0, origin_y: 4.0, pixel_width: 1.0, pixel_height: -1.0 };
        let r_nodata = Raster::new("nd", w, h, vec![data], gt, Crs::LonLat, Some(-9999.0), ColorHint::Gray, "Float32").unwrap();

        // Zone covering entire row 0: cells are 10, 20, -9999 (NoData), 40
        // Valid values: 10, 20, 40 (count = 3, sum = 70, mean = 70/3, min = 10, max = 40)
        let row0_poly = vec![vec![sq(0.0, 3.0, 4.0, 4.0)]];
        let r0_stats = zonal_statistics(&r_nodata, &[0], &row0_poly).unwrap();
        assert_eq!(r0_stats[0].count, 3);
        assert_eq!(r0_stats[0].sum, Some(70.0));
        assert_eq!(r0_stats[0].min, Some(10.0));
        assert_eq!(r0_stats[0].max, Some(40.0));
        assert!((r0_stats[0].mean.unwrap() - 70.0 / 3.0).abs() < 1e-10);

        // Zone covering row 2 where ALL cells are NoData (-9999)
        let row2_poly = vec![vec![sq(0.0, 1.0, 4.0, 2.0)]];
        let r2_stats = zonal_statistics(&r_nodata, &[0], &row2_poly).unwrap();
        assert_eq!(r2_stats[0].count, 0);
        assert_eq!(r2_stats[0].sum, None);
        assert_eq!(r2_stats[0].mean, None);
        assert_eq!(r2_stats[0].min, None);
        assert_eq!(r2_stats[0].max, None);
        assert_eq!(r2_stats[0].stddev, None);

        // 6. Invalid band index returns Err
        assert!(zonal_statistics(&r, &[99], &cell_poly).is_err());

        // 7. Invalid exterior ring (< 3 points) with surviving hole does NOT promote hole to exterior
        let bad_ext_with_hole = vec![vec![
            vec![Coord::new(0.005, 0.005), Coord::new(0.015, 0.015)], // only 2 points!
            sq(0.009, 0.009, 0.011, 0.011),                           // 5 points (hole)
        ]];
        let bad_ext_stats = zonal_statistics(&r, &[0], &bad_ext_with_hole).unwrap();
        assert_eq!(bad_ext_stats[0], ZoneStats::default());
        assert!(polygon_mask(&r, &bad_ext_with_hole).unwrap().is_none());

        // 8. Exterior ring with non-finite/unprojectable coordinate is rejected
        let nan_ext_with_hole = vec![vec![
            vec![Coord::new(f64::NAN, 0.005), Coord::new(0.015, 0.005), Coord::new(0.015, 0.015), Coord::new(f64::NAN, 0.005)],
            sq(0.009, 0.009, 0.011, 0.011),
        ]];
        let nan_ext_stats = zonal_statistics(&r, &[0], &nan_ext_with_hole).unwrap();
        assert_eq!(nan_ext_stats[0], ZoneStats::default());
        assert!(polygon_mask(&r, &nan_ext_with_hole).unwrap().is_none());

        // 9. Polygon with defective hole (< 3 vertices or non-finite) is rejected rather than silently discarding the hole
        let bad_hole_poly = vec![vec![
            sq(0.005, 0.005, 0.015, 0.015),                           // valid exterior (5 points)
            vec![Coord::new(0.009, 0.009), Coord::new(0.011, 0.011)], // invalid hole: only 2 points!
        ]];
        assert!(polygon_mask(&r, &bad_hole_poly).unwrap().is_none(), "polygon with defective hole must be rejected");
        let bad_hole_stats = zonal_statistics(&r, &[0], &bad_hole_poly).unwrap();
        assert_eq!(bad_hole_stats[0], ZoneStats::default());

        let nan_hole_poly = vec![vec![
            sq(0.005, 0.005, 0.015, 0.015),
            vec![Coord::new(f64::NAN, 0.009), Coord::new(0.011, 0.009), Coord::new(0.011, 0.011), Coord::new(f64::NAN, 0.009)],
        ]];
        assert!(polygon_mask(&r, &nan_hole_poly).unwrap().is_none(), "polygon with non-finite hole must be rejected");
    }

    #[test]
    fn polygon_transformation_and_hole_validation_semantics() {
        let sq = |x0: f64, y0: f64, x1: f64, y1: f64| {
            vec![Coord::new(x0, y0), Coord::new(x1, y0), Coord::new(x1, y1), Coord::new(x0, y1), Coord::new(x0, y0)]
        };
        let ext = sq(0.0, 0.0, 10.0, 10.0);
        let hole1 = sq(2.0, 2.0, 4.0, 4.0);
        let hole2 = sq(6.0, 6.0, 8.0, 8.0);

        // Valid exterior and valid holes
        let valid_poly = vec![ext.clone(), hole1.clone(), hole2.clone()];
        let res = transform_polygon_to_lonlat(&valid_poly, None).unwrap();
        assert_eq!(res.len(), 3);
        assert_eq!(res[0].len(), ext.len());
        assert_eq!(res[1].len(), hole1.len());
        assert_eq!(res[2].len(), hole2.len());

        // Empty polygon
        let empty_poly: Vec<Vec<Coord>> = Vec::new();
        assert_eq!(transform_polygon_to_lonlat(&empty_poly, None), Err(PolygonTransformError::EmptyPolygon));

        // Invalid exterior: fewer than 3 vertices
        let bad_ext = vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]];
        assert!(matches!(transform_polygon_to_lonlat(&bad_ext, None), Err(PolygonTransformError::InvalidExterior(_))));

        // Invalid exterior: non-finite coordinate
        let nan_ext = vec![vec![Coord::new(0.0, 0.0), Coord::new(f64::NAN, 1.0), Coord::new(2.0, 2.0), Coord::new(0.0, 0.0)]];
        assert!(matches!(transform_polygon_to_lonlat(&nan_ext, None), Err(PolygonTransformError::InvalidExterior(_))));

        // Invalid hole 1: fewer than 3 vertices
        let bad_hole = vec![ext.clone(), vec![Coord::new(2.0, 2.0), Coord::new(3.0, 3.0)]];
        assert_eq!(
            transform_polygon_to_lonlat(&bad_hole, None),
            Err(PolygonTransformError::InvalidHole { index: 1, reason: "fewer than 3 vertices".into() })
        );

        // Invalid hole 2: non-finite coordinate
        let nan_hole = vec![ext.clone(), hole1.clone(), vec![Coord::new(6.0, 6.0), Coord::new(f64::INFINITY, 8.0), Coord::new(8.0, 8.0)]];
        assert!(matches!(transform_polygon_to_lonlat(&nan_hole, None), Err(PolygonTransformError::InvalidHole { index: 2, .. })));

        // Multipart features
        let multi_valid = vec![vec![ext.clone(), hole1.clone()], vec![sq(20.0, 20.0, 30.0, 30.0)]];
        let multi_res = transform_feature_polygons_to_lonlat(&multi_valid, None).unwrap();
        assert_eq!(multi_res.len(), 2);
        assert_eq!(multi_res[0].len(), 2);
        assert_eq!(multi_res[1].len(), 1);

        // Multipart with failing part
        let multi_bad = vec![vec![ext.clone()], vec![vec![Coord::new(1.0, 1.0)]]];
        assert!(transform_feature_polygons_to_lonlat(&multi_bad, None).is_err());
    }

    #[test]
    fn parse_bands_str_validation_and_boundaries() {
        let total = 3usize;

        // Valid formats
        assert_eq!(parse_bands_str("", total).unwrap(), vec![0]);
        assert_eq!(parse_bands_str("   ", total).unwrap(), vec![0]);
        assert_eq!(parse_bands_str("all", total).unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_bands_str("ALL", total).unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_bands_str("1", total).unwrap(), vec![0]);
        assert_eq!(parse_bands_str("3", total).unwrap(), vec![2]); // boundary: total_bands
        assert_eq!(parse_bands_str("1, 2", total).unwrap(), vec![0, 1]);
        assert_eq!(parse_bands_str("[1, 2, 3]", total).unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_bands_str("1; 3", total).unwrap(), vec![0, 2]);
        assert_eq!(parse_bands_str(" 1   2 ", total).unwrap(), vec![0, 1]);

        // De-duplication
        assert_eq!(parse_bands_str("1, 1, 2, 2, 3", total).unwrap(), vec![0, 1, 2]);

        // Boundary violations and negative
        assert!(parse_bands_str("0", total).is_err(), "0 is invalid (1-based)");
        assert!(parse_bands_str("-1", total).is_err(), "negative band is invalid");
        assert!(parse_bands_str("4", total).is_err(), "out of range band is invalid");
        assert!(parse_bands_str("999", total).is_err(), "out of range band is invalid");

        // Decimals and non-finite
        assert!(parse_bands_str("1.5", total).is_err(), "decimal band is invalid");
        assert_eq!(parse_bands_str("1.0", total).unwrap(), vec![0]); // 1.0 has fract == 0.0
        assert!(parse_bands_str("NaN", total).is_err(), "NaN is invalid");
        assert!(parse_bands_str("Infinity", total).is_err(), "Infinity is invalid");

        // Malformed strings
        assert!(parse_bands_str("abc", total).is_err(), "non-numeric string is invalid");
        assert!(parse_bands_str("[]", total).is_err(), "empty brackets are invalid");
        assert!(parse_bands_str("[   ]", total).is_err(), "whitespace brackets are invalid");
        assert!(parse_bands_str("1,,2", total).is_err(), "empty item between commas is invalid");
        assert!(parse_bands_str(",", total).is_err(), "standalone comma is invalid");
        assert!(parse_bands_str(";", total).is_err(), "standalone semicolon is invalid");
    }

    /// A polar zone held in its own system on a Web Mercator raster near the pole: the zone's
    /// outline lands south of the raster, its inside on it. Every cell whose centre is in the zone
    /// counts, as a scan of them all finds.
    #[test]
    fn a_polar_zone_counts_the_cells_inside_it_beyond_its_outline() {
        let merc = Crs::WebMercator.transformer().unwrap_or_else(|e| panic!("{e}"));
        let (Some(sw), Some(ne)) = (merc.from_lonlat(Coord::new(-10.0, 84.0)), merc.from_lonlat(Coord::new(10.0, 85.0))) else {
            panic!("the corners are in Web Mercator")
        };
        let cell = 5_000.0;
        let (w, h) = (((ne.x - sw.x) / cell).ceil() as u32, ((ne.y - sw.y) / cell).ceil() as u32);
        let gt = GeoTransform { origin_x: sw.x, origin_y: ne.y, pixel_width: cell, pixel_height: -cell };
        let r = Raster::new("polar", w, h, vec![vec![1.0; (w * h) as usize]], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32")
            .unwrap_or_else(|e| panic!("{e}"));
        // NSIDC Sea Ice Polar Stereographic North: squares of 1,000 km each way about the pole,
        // their sides near 77°N to 81°N.
        let polar = Crs::Epsg(3413);
        // Centred on the pole, and off it.
        let to_polar = polar.transformer().unwrap_or_else(|e| panic!("{e}"));
        // The last reaches far north and east: no sample inside it lands near the pole.
        for (x0, x1, y0, y1) in [(-1e6, 1e6, -1e6, 1e6), (-0.9e6, 1.1e6, -1.05e6, 0.95e6), (-1e6, 31e6, -1e6, 31e6)] {
            let ring = vec![Coord::new(x0, y0), Coord::new(x1, y0), Coord::new(x1, y1), Coord::new(x0, y1), Coord::new(x0, y0)];
            let mut inside = 0;
            for row in 0..h {
                for col in 0..w {
                    let c = r.transform.to_world(f64::from(col) + 0.5, f64::from(row) + 0.5);
                    let p = merc.to_lonlat(c).and_then(|ll| to_polar.from_lonlat(ll)).unwrap_or_else(|| panic!("{c:?} is in EPSG:3413"));
                    inside += usize::from(p.x > x0 && p.x < x1 && p.y > y0 && p.y < y1);
                }
            }
            assert!(inside > 1000, "{inside} cells are in the zone");
            let got = zonal_means_in(&r, polar, &[0], &[vec![ring]]).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(got, Some((vec![1.0], inside)), "{x0} {x1} {y0} {y1}");
        }
    }
}
