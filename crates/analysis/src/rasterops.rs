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
pub fn max_pixels() -> usize {
    rustgis_data::memory::scaled(rustgis_data::memory::physical(), PIXELS_FLOOR, PIXELS_CEILING, 16)
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

/// Aspect: the compass direction the slope faces, in degrees (flat cells are -1).
pub fn aspect(r: &Raster, b: usize) -> Result<Vec<f32>, String> {
    let d = band(r, b)?;
    terrain(r, d, 1.0, |gx, gy| if gx.abs() < 1e-12 && gy.abs() < 1e-12 { -1.0 } else { (-gx).atan2(-gy).to_degrees().rem_euclid(360.0) })
}

/// Hillshade (0–255) lit from `azimuth` and `altitude` degrees.
pub fn hillshade(r: &Raster, b: usize, azimuth: f64, altitude: f64, zf: f64) -> Result<Vec<f32>, String> {
    let d = band(r, b)?;
    let (zen, az) = ((90.0 - altitude).to_radians(), azimuth.to_radians());
    let (cos_zen, sin_zen) = (zen.cos(), zen.sin());
    terrain(r, d, zf, |gx, gy| {
        let slope = gx.hypot(gy).atan();
        let aspect = (-gx).atan2(-gy);
        let v = cos_zen * slope.cos() + sin_zen * slope.sin() * (az - aspect).cos();
        (v.max(0.0) * 255.0).round()
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

/// Remaps value ranges `[lo, hi)` to new values; other values become no-data unless `keep`.
pub fn remap(r: &Raster, b: usize, ranges: &[(f64, f64, f64)], keep: bool) -> Result<Vec<f32>, String> {
    let d = band(r, b)?;
    let mut out = grid(r.width as usize, r.height as usize)?;
    out.extend(d.iter().map(|v| {
        if !r.is_valid(*v) {
            return NODATA;
        }
        let x = *v as f64;
        match ranges.iter().find(|(lo, hi, _)| x >= *lo && x < *hi) {
            Some((_, _, new)) => *new as f32,
            None if keep => *v,
            None => NODATA,
        }
    }));
    Ok(out)
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
    let mut out = grid(n, 1)?;
    out.extend((0..n).map(|i| {
        let mut s = 0.0;
        for (vals, w) in criteria {
            let v = vals.get(i).copied().unwrap_or(NODATA);
            if v == NODATA || !v.is_finite() {
                return NODATA;
            }
            s += v as f64 * w.max(0.0) / total;
        }
        s as f32
    }));
    Ok(out)
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

/// Whether `b` is visible from `a` (observer and target heights above the ground, in metres),
/// and each profile sample with whether it can be seen. Earth curvature is included.
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
    const R: f64 = 6_371_000.0 * 4.0 / 3.0; // refraction-adjusted Earth radius
    let drop = |d: f64| d * d / (2.0 * R);
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
/// (each about a second or two); a run on several threads may take as many times more
/// ([`crate::parallel::work_cap`]).
const MAX_VIEWSHED_WORK: usize = 400_000_000;
pub(crate) const MAX_IDW_WORK: usize = 400_000_000;

/// Cells visible from an observer (1) or not (0) within `radius_m`; outside the radius no-data.
pub fn viewshed(r: &Raster, b: usize, observer: Coord, height: f64, radius_m: f64) -> Result<Vec<f32>, String> {
    viewshed_on(None, r, b, observer, height, radius_m)
}

/// [`viewshed`] on `threads` threads, or as many as its work calls for.
fn viewshed_on(threads: Option<usize>, r: &Raster, b: usize, observer: Coord, height: f64, radius_m: f64) -> Result<Vec<f32>, String> {
    let (oc, or) = r.pixel_at(observer).ok_or("the observer is outside the raster")?;
    let z0 = r.value(b, oc, or).ok_or("the observer stands on no-data")? as f64 + height;
    let (cx, cy) = r.cell_size_m();
    if !(cx > 0.0 && cy > 0.0 && radius_m.is_finite() && radius_m > 0.0) {
        return Err("the raster's cells or the radius have no size".into());
    }
    // A tiny declared cell size would make the window overflow: bound it before converting.
    if radius_m / cx > 100_000.0 || radius_m / cy > 100_000.0 {
        return Err("the radius covers too many cells; use a smaller radius".into());
    }
    let rad_c = (radius_m / cx).ceil() as i64;
    let rad_r = (radius_m / cy).ceil() as i64;
    // Each cell walks a ray back to the observer: bound cells × ray length, not just cells.
    let cells = (2 * rad_c + 1).saturating_mul(2 * rad_r + 1);
    let ray_work = usize::try_from(cells.saturating_mul(rad_c.max(rad_r) / 2 + 1)).unwrap_or(usize::MAX);
    if cells > 25_000_000 {
        return Err("the radius covers too many cells; use a smaller radius".into());
    }
    // Each cell walks its own ray and writes only itself, so the rows within the radius are made
    // on several threads when the work is large (#1252), and may take as many times the work.
    let (first, last) = ((or - rad_r).max(0), (or + rad_r).min(r.height as i64 - 1));
    let rows = usize::try_from(last - first + 1).unwrap_or(0);
    let threads = threads.unwrap_or_else(|| crate::parallel::threads_for_rows(rows, ray_work, 0));
    let cap = crate::parallel::work_cap(MAX_VIEWSHED_WORK, threads);
    if ray_work > cap {
        return Err(format!(
            "the radius takes {ray_work} ray steps, more than the {cap} this computer takes on {threads} thread(s); use a smaller radius"
        ));
    }
    let w = r.width as usize;
    let mut out = new_band(w, r.height as usize, NODATA)?;
    const R: f64 = 6_371_000.0 * 4.0 / 3.0;
    let window = out.chunks_mut(w).skip(usize::try_from(first).unwrap_or(0)).take(rows);
    crate::control::begin(rows);
    crate::parallel::for_each_row(
        window,
        threads,
        || (),
        |_, i, line: &mut [f32]| {
            let row = first + i as i64;
            for col in (oc - rad_c).max(0)..=(oc + rad_c).min(r.width as i64 - 1) {
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
                let target = (zt as f64 - dist * dist / (2.0 * R) - z0) / dist;
                // Walk the ray from the observer; any cell higher than the line hides the target.
                let steps = ((col - oc).abs().max((row - or).abs())) as usize;
                let mut visible = true;
                for s in 1..steps {
                    let t = s as f64 / steps as f64;
                    let (c, rr) = ((oc as f64 + (col - oc) as f64 * t).round() as i64, (or as f64 + (row - or) as f64 * t).round() as i64);
                    let Some(zi) = r.value(b, c, rr) else { continue };
                    let di = dist * t;
                    if (zi as f64 - di * di / (2.0 * R) - z0) / di > target {
                        visible = false;
                        break;
                    }
                }
                if let Some(o) = line.get_mut(col as usize) {
                    *o = if visible { 1.0 } else { 0.0 };
                }
            }
        },
    )?;
    Ok(out)
}

/// Inverse distance weighted interpolation of point values onto a longitude/latitude grid, from
/// each cell's `neighbours` nearest points, or from every point (as QGIS does) when it is 0.
pub fn idw(points: &[(Coord, f64)], extent: &Extent, cell_deg: f64, power: f64, neighbours: usize, name: &str) -> Result<Raster, String> {
    idw_on(None, points, extent, cell_deg, power, neighbours, name)
}

/// [`idw`] on `threads` threads, or as many as its work calls for.
fn idw_on(
    threads: Option<usize>,
    points: &[(Coord, f64)],
    extent: &Extent,
    cell_deg: f64,
    power: f64,
    neighbours: usize,
    name: &str,
) -> Result<Raster, String> {
    if points.len() < 2 {
        return Err("interpolation needs at least two points with values".into());
    }
    if !(cell_deg.is_finite() && cell_deg > 0.0) {
        return Err("the cell size must be positive".into());
    }
    if extent.is_empty() {
        return Err("the area to interpolate over is empty".into());
    }
    let (w, h) = ((extent.width() / cell_deg).ceil().max(1.0) as usize, (extent.height() / cell_deg).ceil().max(1.0) as usize);
    if w.saturating_mul(h) > max_pixels() / 4 {
        return Err(format!("{w} × {h} cells is too many, more than this computer's {}; use a larger cell size", max_pixels() / 4));
    }
    if !(power.is_finite() && power >= 0.0) {
        return Err("the distance coefficient must be zero or more".into());
    }
    let k = if neighbours == 0 { points.len() } else { neighbours.clamp(1, points.len()) };
    // Each row on its own, on several threads when the work is large (#1252); a thread's
    // distances go into its own buffer.
    let scratch_bytes = points.len().saturating_mul(std::mem::size_of::<(f64, f64)>());
    // Every cell weighs every point: as many times the work as there are threads (#1291).
    let work = w.saturating_mul(h).saturating_mul(points.len());
    let threads = threads.unwrap_or_else(|| crate::parallel::threads_for_rows(h, work, scratch_bytes));
    let cap = crate::parallel::work_cap(MAX_IDW_WORK, threads);
    if work > cap {
        return Err(format!(
            "{w} × {h} cells from {} points is too much work, more than the {cap} cell-point pairs this computer takes on {threads} thread(s); use a larger cell size",
            points.len()
        ));
    }
    let mut data = new_band(w, h, NODATA)?;
    crate::control::begin(h);
    crate::parallel::for_each_row(
        data.chunks_mut(w),
        threads,
        || Vec::with_capacity(points.len()),
        |near: &mut Vec<(f64, f64)>, row, out: &mut [f32]| {
            // East-west degrees shrink with latitude, which only changes by row.
            let kx = (extent.max_y - (row as f64 + 0.5) * cell_deg).to_radians().cos();
            for (col, o) in out.iter_mut().enumerate() {
                let c = Coord::new(extent.min_x + (col as f64 + 0.5) * cell_deg, extent.max_y - (row as f64 + 0.5) * cell_deg);
                near.clear();
                near.extend(points.iter().map(|(p, v)| {
                    let dx = (p.x - c.x) * kx;
                    ((dx * dx + (p.y - c.y).powi(2)).sqrt(), *v)
                }));
                if k < near.len() {
                    near.select_nth_unstable_by(k - 1, |a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                    near.truncate(k);
                }
                let v = if let Some(exact) = near.iter().find(|(d, _)| *d < 1e-12) {
                    exact.1
                } else {
                    // Weights relative to the nearest point's: the same ratios as 1/dᵖ, without
                    // overflowing for a high power.
                    let nearest = near.iter().map(|(d, _)| *d).fold(f64::INFINITY, f64::min);
                    let (num, den) = near.iter().fold((0.0, 0.0), |(n, d), (dist, v)| {
                        let wgt = (nearest / dist).powf(power);
                        (n + wgt * v, d + wgt)
                    });
                    num / den
                };
                *o = if v.is_finite() { v as f32 } else { NODATA };
            }
        },
    )?;
    let gt = GeoTransform { origin_x: extent.min_x, origin_y: extent.max_y, pixel_width: cell_deg, pixel_height: -cell_deg };
    Raster::new(name, w as u32, h as u32, vec![data], gt, Crs::LonLat, Some(NODATA as f64), ColorHint::Gray, "Float32")
        .map_err(|e| e.to_string())
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
    let mut p: Vec<f64> = Vec::with_capacity(data.len());
    let mut out = grid(r.width as usize, r.height as usize)?;
    out.extend((0..n).map(|i| {
        p.clear();
        for d in &data {
            match d.get(i).copied().filter(|v| r.is_valid(*v)) {
                Some(v) => p.push(f64::from(v)),
                None => return NODATA,
            }
        }
        means
            .iter()
            .map(|(c, m)| (*c, m.iter().zip(&p).map(|(a, b)| (a - b).powi(2)).sum::<f64>()))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map_or(NODATA, |x| x.0)
    }));
    Ok(out)
}

/// The mean of each band over the pixels whose centres fall in a polygon (lon/lat rings).
pub fn zonal_means(r: &Raster, bands: &[usize], rings: &[Vec<Coord>]) -> Option<(Vec<f64>, usize)> {
    let mut e = Extent::EMPTY;
    for ring in rings {
        for c in ring {
            e.expand(*c);
        }
    }
    let mut sums = vec![0.0; bands.len()];
    let mut n = 0usize;
    for row in 0..r.height as i64 {
        for col in 0..r.width as i64 {
            let w = r.transform.to_world(col as f64 + 0.5, row as f64 + 0.5);
            let Some(ll) = r.transformer().to_lonlat(w) else { continue };
            if !e.contains(ll) || !rustgis_data::geometry::polygon_contains(rings, ll) {
                continue;
            }
            let vals: Option<Vec<f64>> = bands.iter().map(|b| r.value(*b, col, row).map(f64::from)).collect();
            if let Some(v) = vals {
                sums.iter_mut().zip(&v).for_each(|(s, x)| *s += x);
                n += 1;
            }
        }
    }
    (n > 0).then(|| (sums.into_iter().map(|s| s / n as f64).collect(), n))
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

/// Projects a complete ring to the raster grid without dropping failed vertices.
fn project_ring(r: &Raster, ring: &[Coord]) -> Option<Vec<(f64, f64)>> {
    if ring.len() < 3 {
        return None;
    }
    let tr = r.transformer();
    ring.iter()
        .map(|&c| {
            if !c.is_finite() {
                return None;
            }
            let pt = r.transform.to_pixel(tr.from_lonlat(c)?);
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
    let projected: Vec<Vec<Vec<(f64, f64)>>> = polygons
        .iter()
        .enumerate()
        .filter_map(|(index, rings)| {
            // Reject a complete part if any ring fails, so a lost hole cannot enlarge it.
            let part = if rings.is_empty() { None } else { rings.iter().map(|ring| project_ring(r, ring)).collect::<Option<Vec<_>>>() };
            if part.is_none() {
                failed_parts.push(index);
            }
            part
        })
        .collect();
    let (mut c0, mut r0, mut c1, mut r1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for &(x, y) in projected.iter().flatten().flatten() {
        (c0, r0, c1, r1) = (c0.min(x), r0.min(y), c1.max(x), r1.max(y));
    }
    if !(c0.is_finite() && r0.is_finite() && c1.is_finite() && r1.is_finite()) {
        return Ok(None);
    }
    let clamp = |v: f64, n: u32| v.clamp(0.0, f64::from(n)) as u32;
    let cols = clamp(c0.floor(), r.width)..clamp(c1.ceil(), r.width);
    let rows = clamp(r0.floor(), r.height)..clamp(r1.ceil(), r.height);
    if cols.is_empty() || rows.is_empty() {
        return Ok(None);
    }
    let w = (cols.end - cols.start) as usize;
    let mut mask = new_band(w, (rows.end - rows.start) as usize, false)?;
    let mut xs: Vec<f64> = Vec::new();
    for (ri, row) in rows.clone().enumerate() {
        let y = f64::from(row) + 0.5;
        let line = mask.get_mut(ri * w..(ri + 1) * w).unwrap_or_default();
        // Even-odd within each polygon (holes cut out), any polygon over the others.
        for rings in &projected {
            xs.clear();
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
                // Cells whose centre (col + 0.5) lies in [x0, x1).
                let a = clamp((x0 - 0.5).ceil(), r.width).max(cols.start);
                let b = clamp((x1 - 0.5).ceil(), r.width).min(cols.end);
                for col in a..b {
                    if let Some(m) = line.get_mut((col - cols.start) as usize) {
                        *m = true;
                    }
                }
            }
        }
    }
    // Trimmed to the cells inside: reprojected vertices on cell edges land a hair either side.
    let (mut tc0, mut tr0, mut tc1, mut tr1) = (usize::MAX, usize::MAX, 0, 0);
    for (i, _) in mask.iter().enumerate().filter(|(_, m)| **m) {
        let (c, rr) = (i % w, i / w);
        (tc0, tr0, tc1, tr1) = (tc0.min(c), tr0.min(rr), tc1.max(c + 1), tr1.max(rr + 1));
    }
    if tc0 >= tc1 || tr0 >= tr1 {
        return Ok(None);
    }
    let mut trimmed = grid(tc1 - tc0, tr1 - tr0)?;
    trimmed
        .extend((tr0..tr1).flat_map(|rr| (tc0..tc1).map(move |c| (rr, c))).map(|(rr, c)| mask.get(rr * w + c).copied().unwrap_or(false)));
    let cols = cols.start + tc0 as u32..cols.start + tc1 as u32;
    let rows = rows.start + tr0 as u32..rows.start + tr1 as u32;
    Ok(Some((cols, rows, trimmed)))
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
    r.require_coordinates()?;
    for &b in bands {
        if b >= r.band_count() {
            return Err(format!("band index {} is out of range: {} has {} bands", b + 1, r.name, r.band_count()));
        }
    }
    if bands.is_empty() {
        return Ok(Vec::new());
    }

    let Some((cols, rows, mask)) = polygon_mask_with_failures(r, polygons, failed_parts)? else {
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

    /// Viewshed rows made on several threads see what one thread sees (#1252), with the radius
    /// inside the raster and reaching past its edges.
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
            let one = viewshed_on(Some(1), &dem, 0, at, 2.0, radius).unwrap_or_else(|e| panic!("{e}"));
            let counts = [1.0, 0.0, NODATA].map(|k| one.iter().filter(|v| v.to_bits() == k.to_bits()).count());
            assert_eq!(counts, expected, "radius {radius}");
            for threads in [2, 7, 16] {
                let many = viewshed_on(Some(threads), &dem, 0, at, 2.0, radius).unwrap_or_else(|e| panic!("{e}"));
                assert!(one.iter().map(|v| v.to_bits()).eq(many.iter().map(|v| v.to_bits())), "{threads} threads, radius {radius}");
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

    /// A run past one thread's work, on one thread, is refused, naming the cap and the threads.
    #[test]
    fn idw_past_one_threads_work_is_refused_on_one_naming_the_cap() {
        // 1000 × 1000 cells from 500 points: 500 million pairs, past one thread's 400 million.
        let pts: Vec<(Coord, f64)> = (0..500).map(|i| (Coord::new((i % 25) as f64 / 2.5, (i / 25) as f64 / 2.0), i as f64)).collect();
        let extent = Extent::new(0.0, 0.0, 10.0, 10.0);
        let e = idw_on(Some(1), &pts, &extent, 0.01, 2.0, 0, "idw").unwrap_err();
        assert!(
            e.contains("too much work") && e.contains(&format!("{MAX_IDW_WORK} cell-point pairs")) && e.contains("on 1 thread(s)"),
            "{e}"
        );
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
        for neighbours in [0, 8] {
            let one = idw_on(Some(1), &pts, &extent, 0.1, 2.0, neighbours, "idw").unwrap_or_else(|e| panic!("{e}"));
            for threads in [2, 7, 16] {
                let many = idw_on(Some(threads), &pts, &extent, 0.1, 2.0, neighbours, "idw").unwrap_or_else(|e| panic!("{e}"));
                let bits = |r: &Raster| r.bands[0].data.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
                assert_eq!(bits(&one), bits(&many), "{threads} threads, {neighbours} neighbours");
            }
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
        let rm = remap(&r, 0, &[(0.0, 100.0, 1.0), (100.0, 1000.0, 2.0)], false).unwrap_or_default();
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
        let vs = viewshed(&wall, 0, Coord::new(0.0025, 0.0105), 2.0, 5_000.0).unwrap_or_default();
        assert_eq!(vs[10 * 20 + 5], 1.0);
        assert_eq!(vs[10 * 20 + 15], 0.0);
        let (seen, prof) =
            line_of_sight(&wall, 0, Coord::new(0.0025, 0.0105), Coord::new(0.0175, 0.0105), 2.0, 0.0, 50).unwrap_or((true, vec![]));
        assert!(!seen && !prof.is_empty());
        let g = idw(&[(Coord::new(0.0, 0.0), 0.0), (Coord::new(1.0, 0.0), 10.0)], &Extent::new(0.0, -0.5, 1.0, 0.5), 0.1, 2.0, 8, "idw")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((g.width, g.height), (10, 10));
        let row = &g.bands[0].data[50..60];
        assert!(row[0] < row[9] && row[0] > 0.0 && row[9] < 10.0, "{row:?}");
        // Every point (0), and a power high enough to overflow 1/dᵖ, still give finite values.
        let all = idw(&[(Coord::new(0.0, 0.0), 0.0), (Coord::new(1.0, 0.0), 10.0)], &Extent::new(0.0, -0.5, 1.0, 0.5), 0.1, 99.0, 0, "idw")
            .unwrap_or_else(|e| panic!("{e}"));
        let row = &all.bands[0].data[50..60];
        assert!(row.iter().all(|v| (0.0..=10.0).contains(v)) && row[0] < 1e-3 && row[9] > 10.0 - 1e-3, "{row:?}");
        // With every point, a far one still pulls each cell off the two near ones' value.
        let pts = [(Coord::new(0.0, 0.0), 0.0), (Coord::new(1.0, 0.0), 0.0), (Coord::new(5.0, 0.0), 100.0)];
        let all = idw(&pts, &Extent::new(0.0, -0.5, 1.0, 0.5), 0.1, 1.0, 0, "idw").unwrap_or_else(|e| panic!("{e}"));
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
}
