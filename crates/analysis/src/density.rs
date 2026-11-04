//! Kernel density estimation for point vector layers.
//!
//! Generates a continuous magnitude-per-unit-area density raster from point features,
//! optionally weighted by a numeric attribute, using Quartic (biweight) or Gaussian kernels.
//! Calculations are performed in the linear units of a projected coordinate system (e.g. metres).

use std::f64::consts::PI;

use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
use rustgis_geom::{Coord, Extent};

/// Supported 2D kernel functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelType {
    /// 2D Quartic (biweight) compact-support kernel (default):
    /// K(u) = (3 / (π * h²)) * (1 - u²)² for u < 1, 0 otherwise.
    Quartic,
    /// 2D Gaussian kernel truncated at 3σ = h and renormalised to integrate to 1:
    /// K(d) = (1 / (2π * σ² * (1 - exp(-4.5)))) * exp(-d² / (2σ²)) for d <= h, 0 otherwise.
    Gaussian,
}

impl KernelType {
    /// Parses a kernel name case-insensitively. Accepts `"Quartic"` or `"Gaussian"`.
    /// When given empty text, defaults to [`KernelType::Quartic`].
    pub fn parse(s: &str) -> Result<Self, String> {
        let t = s.trim();
        if t.eq_ignore_ascii_case("quartic") || t.is_empty() {
            Ok(KernelType::Quartic)
        } else if t.eq_ignore_ascii_case("gaussian") {
            Ok(KernelType::Gaussian)
        } else {
            Err(format!("unknown kernel type '{t}': choose 'Quartic' or 'Gaussian'"))
        }
    }
}

/// The fewest output cells [`max_density_cells`] allows, and its limit in the browser.
pub const DENSITY_CELLS_FLOOR: usize = 16_000_000;

/// Upper limit on total output raster cells, to prevent memory exhaustion: on the desktop as many as
/// a quarter of the computer's memory holds at 16 bytes a cell (the sums, the output and the
/// GeoTIFF written from it), from [`DENSITY_CELLS_FLOOR`] up to the most any tool's output raster
/// takes ([`crate::rasterops::PIXELS_CEILING`]) (#1291 step 2).
pub fn max_density_cells() -> usize {
    rustgis_data::memory::scaled(rustgis_data::memory::physical(), DENSITY_CELLS_FLOOR, crate::rasterops::PIXELS_CEILING, 16)
}

/// Upper limit on aggregate cell visits across all points on one thread, to prevent CPU
/// exhaustion; a run on several threads may take as many times more
/// ([`crate::parallel::work_cap`]).
pub const MAX_DENSITY_WORK: usize = 50_000_000;

/// The points between two counts of a strip's progress and checks for a cancel.
const PROGRESS_POINTS: usize = 4096;
/// The cells the points' kernels reach (a few milliseconds of them) after which a strip counts its
/// progress and checks for a cancel sooner: a few points with large kernels.
const PROGRESS_CELLS: usize = 1 << 20;

/// Default target grid resolution along the longer side of the extent when pixel size is not specified.
const DEFAULT_GRID_STEPS: f64 = 250.0;

/// Computes an automatic bandwidth (search radius) for 2D point data using Silverman's rule of thumb.
///
/// h = 0.9 * min(StandardDistance, sqrt(1 / ln(2)) * MedianDistance) * N^(-0.2)
///
/// Takes point weights into account for weighted mean center, weighted standard distance, and
/// weighted median distance. Falls back safely and deterministically to extent-based scaling for
/// small samples or identical coordinates (span / 30.0 when extent > 0, or 1000.0 coordinate-space
/// units for single or coincident points).
pub fn silverman_bandwidth(points: &[(Coord, f64)], extent: &Extent) -> f64 {
    let n = points.len();
    if n < 2 {
        let span = extent.width().max(extent.height());
        return if span > 1e-4 { span / 30.0 } else { 1000.0 };
    }

    let (sum_x, sum_y, sum_w) = points.iter().fold((0.0, 0.0, 0.0), |(sx, sy, sw), (p, w)| {
        let weight = if w.is_finite() && *w > 0.0 { *w } else { 1.0 };
        (sx + p.x * weight, sy + p.y * weight, sw + weight)
    });
    if sum_w <= 0.0 || !sum_w.is_finite() {
        let span = extent.width().max(extent.height());
        return if span.is_finite() && span > 1e-4 { (span / 30.0).max(1e-4) } else { 1000.0 };
    }
    let mean_x = sum_x / sum_w;
    let mean_y = sum_y / sum_w;

    let sum_sq_dist = points.iter().fold(0.0, |acc, (p, w)| {
        let weight = if w.is_finite() && *w > 0.0 { *w } else { 1.0 };
        acc + weight * ((p.x - mean_x).powi(2) + (p.y - mean_y).powi(2))
    });
    let std_dist = (sum_sq_dist / sum_w).sqrt();

    let mut dists: Vec<(f64, f64)> = points
        .iter()
        .map(|(p, w)| {
            let weight = if w.is_finite() && *w > 0.0 { *w } else { 1.0 };
            (((p.x - mean_x).powi(2) + (p.y - mean_y).powi(2)).sqrt(), weight)
        })
        .collect();
    dists.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let half_w = sum_w * 0.5;
    let mut cum_w = 0.0;
    let mut median_dist = dists.first().map_or(0.0, |d| d.0);
    for &(d, w) in &dists {
        cum_w += w;
        if cum_w >= half_w {
            median_dist = d;
            break;
        }
    }

    // A = sqrt(1 / ln(2)) ≈ 1.2011224087864498
    let a = (1.0 / 2.0f64.ln()).sqrt();
    let term = std_dist.min(a * median_dist);

    if term > 1e-6 {
        let n_factor = (n as f64).powf(-0.2);
        let h = 0.9 * term * n_factor;
        if h.is_finite() && h > 1e-4 {
            return h;
        }
    }

    // Fallback: fraction of extent or fixed distance
    let span = extent.width().max(extent.height());
    if span.is_finite() && span > 1e-4 { (span / 30.0).max(1e-4) } else { 1000.0 }
}

/// Automatically calculates cell size so the grid spans approximately 250 cells across its maximum dimension.
pub fn auto_cell_size(extent: &Extent) -> f64 {
    let span = extent.width().max(extent.height());
    if !span.is_finite() || span <= 0.0 {
        return 1000.0;
    }
    (span / DEFAULT_GRID_STEPS).max(1e-4)
}

/// Computes the bounding extent of a slice of weighted points, ignoring non-finite coordinates.
pub fn points_extent(points: &[(Coord, f64)]) -> Option<Extent> {
    let mut ext = Extent::EMPTY;
    for (p, _) in points {
        if p.is_finite() {
            ext.expand(*p);
        }
    }
    if ext.is_empty() { None } else { Some(ext) }
}

/// Resolves the grid extent for kernel density estimation.
///
/// If `custom_extent` is provided and non-empty, validates and returns it.
/// Otherwise, expands `pt_extent` by `bandwidth`.
pub fn resolve_grid_extent(pt_extent: &Extent, bandwidth: f64, custom_extent: Option<&Extent>) -> Result<Extent, String> {
    let base_extent = match custom_extent {
        Some(e) if !e.is_empty() => *e,
        _ => {
            if pt_extent.is_empty() {
                return Err("points have empty extent".into());
            }
            Extent::new(pt_extent.min_x - bandwidth, pt_extent.min_y - bandwidth, pt_extent.max_x + bandwidth, pt_extent.max_y + bandwidth)
        }
    };
    if !base_extent.width().is_finite() || !base_extent.height().is_finite() || base_extent.width() <= 0.0 || base_extent.height() <= 0.0 {
        return Err("the extent must have a positive width and height".into());
    }
    Ok(base_extent)
}

/// Adds each point's kernel (`eval`) to the cells of `grid` it reaches, on `threads` threads. The
/// rows are cut into strips, a few per thread so that strips that cost more are shared out; each
/// strip takes the points that reach it in their order, so every cell adds them up as one thread
/// does (#1252). Progress counts each strip's points.
fn accumulate_density<F>(
    threads: usize,
    points: &[(Coord, f64)],
    grid: &mut [f64],
    w: usize,
    h_rows: usize,
    origin_x: f64,
    origin_y: f64,
    cell_size: f64,
    bandwidth: f64,
    h_sq: f64,
    eval: F,
) -> Result<(), crate::control::Cancelled>
where
    F: Fn(f64, f64) -> f64 + Sync,
{
    let strips = if threads <= 1 { 1 } else { h_rows.min(threads.saturating_mul(4)) };
    let strip_rows = h_rows.div_ceil(strips.max(1)).max(1);
    // The strips the rows make (fewer than asked for when they don't divide evenly).
    let strips = h_rows.div_ceil(strip_rows);
    // Each strip counts its points as it goes; `for_each_row` counts the strip itself once more.
    crate::control::begin(strips.saturating_mul(points.len().saturating_add(1)));
    crate::parallel::for_each_row(
        grid.chunks_mut(w * strip_rows),
        threads,
        || (),
        |_, s, strip: &mut [f64]| {
            let first = s * strip_rows;
            let last = first + strip.len() / w - 1;
            let (mut counted, mut cells) = (0, 0usize);
            for (i, &(p, wgt)) in points.iter().enumerate() {
                if i - counted == PROGRESS_POINTS || cells >= PROGRESS_CELLS {
                    if crate::control::cancelled() {
                        return;
                    }
                    crate::control::advance(i - counted);
                    (counted, cells) = (i, 0);
                }
                if wgt <= 0.0 || !wgt.is_finite() || !p.is_finite() {
                    continue;
                }

                let col_min = (((p.x - bandwidth - origin_x) / cell_size).floor() as isize).clamp(0, (w - 1) as isize) as usize;
                let col_max = (((p.x + bandwidth - origin_x) / cell_size).ceil() as isize).clamp(0, (w - 1) as isize) as usize;

                let row_min = (((origin_y - (p.y + bandwidth)) / cell_size).floor() as isize).clamp(0, (h_rows - 1) as isize) as usize;
                let row_max = (((origin_y - (p.y - bandwidth)) / cell_size).ceil() as isize).clamp(0, (h_rows - 1) as isize) as usize;

                let (top, bottom) = (row_min.max(first), row_max.min(last));
                cells = cells.saturating_add((bottom + 1).saturating_sub(top).saturating_mul((col_max + 1).saturating_sub(col_min)));
                for row in top..=bottom {
                    let y_cell = origin_y - (row as f64 + 0.5) * cell_size;
                    let dy = p.y - y_cell;
                    let dy_sq = dy * dy;
                    if dy_sq >= h_sq {
                        continue;
                    }
                    let rem_sq = h_sq - dy_sq;
                    let row_offset = (row - first) * w;

                    for col in col_min..=col_max {
                        let x_cell = origin_x + (col as f64 + 0.5) * cell_size;
                        let dx = p.x - x_cell;
                        let dx_sq = dx * dx;
                        if dx_sq >= rem_sq {
                            continue;
                        }

                        let d_sq = dx_sq + dy_sq;
                        let contrib = eval(d_sq, wgt);

                        if let Some(cell) = strip.get_mut(row_offset + col) {
                            *cell += contrib;
                        }
                    }
                }
            }
            crate::control::advance(points.len() - counted);
        },
    )?;
    // A strip stopped part of the way leaves the grid unfinished: it is not the result.
    crate::control::check()
}

/// Generates a continuous kernel density raster from weighted points in a projected CRS.
pub fn kernel_density(
    points: &[(Coord, f64)],
    custom_extent: Option<&Extent>,
    cell_size: f64,
    bandwidth: f64,
    kernel: KernelType,
    name: &str,
    crs: Crs,
) -> Result<Raster, String> {
    kernel_density_on(None, points, custom_extent, cell_size, bandwidth, kernel, name, crs)
}

/// [`kernel_density`] on `threads` threads (none: as many as pay off).
fn kernel_density_on(
    threads: Option<usize>,
    points: &[(Coord, f64)],
    custom_extent: Option<&Extent>,
    cell_size: f64,
    bandwidth: f64,
    kernel: KernelType,
    name: &str,
    crs: Crs,
) -> Result<Raster, String> {
    if points.is_empty() {
        return Err("kernel density requires at least one point".into());
    }
    if !bandwidth.is_finite() || bandwidth <= 0.0 {
        return Err("search radius (bandwidth) must be a positive number".into());
    }
    let h_sq = bandwidth * bandwidth;
    if !h_sq.is_normal() || h_sq <= 0.0 {
        return Err("search radius is out of range".into());
    }
    if !cell_size.is_finite() || cell_size <= 0.0 {
        return Err("pixel size must be a positive number".into());
    }
    if cell_size > bandwidth {
        return Err("pixel size must not be larger than the search radius (bandwidth)".into());
    }
    if crs.is_geographic() {
        return Err("kernel density requires a projected coordinate system with linear units (e.g. meters or feet), not geographic coordinates in degrees".into());
    }

    // Determine spatial extent
    let pt_extent = points_extent(points).ok_or("points have empty extent")?;
    let base_extent = resolve_grid_extent(&pt_extent, bandwidth, custom_extent)?;

    // Snap cell count to nearest integer if within floating-point epsilon to avoid spurious extra cells from division rounding.
    // When an extent dimension is not an exact multiple of cell_size, ceil() ensures the requested extent is fully covered.
    let snap_cells = |span: f64| -> usize {
        let raw = span / cell_size;
        let rounded = raw.round();
        if (raw - rounded).abs() < 1e-9 { rounded.max(1.0) as usize } else { raw.ceil().max(1.0) as usize }
    };
    let w = snap_cells(base_extent.width());
    let h_rows = snap_cells(base_extent.height());

    let total_cells = w.checked_mul(h_rows).ok_or("grid dimensions overflow")?;
    let max_cells = max_density_cells();
    if total_cells > max_cells {
        return Err(format!("{w} × {h_rows} cells is too large, more than this computer's {max_cells} cells; choose a larger pixel size"));
    }

    // Estimate total cell visits bounded by the point kernel window and output grid
    let cells_across = (2.0 * bandwidth / cell_size).ceil();
    let cells_per_point = if cells_across.is_finite() && cells_across < total_cells as f64 {
        let dim = (cells_across as usize).saturating_add(2);
        dim.saturating_mul(dim).min(total_cells)
    } else {
        total_cells
    };
    let total_work = points.len().saturating_mul(cells_per_point);
    // A cell visit is some 5 ns, a sixth of `MIN_ROW_WORK`'s unit.
    let threads = threads.unwrap_or_else(|| crate::parallel::threads_for_rows(h_rows, total_work / 6, 0));
    let cap = crate::parallel::work_cap(MAX_DENSITY_WORK, threads);
    if total_work > cap {
        return Err(format!(
            "{w} × {h_rows} cells from {} points is too much work, more than the {cap} cell visits this computer takes on {threads} thread(s); choose a larger pixel size or a smaller search radius",
            points.len()
        ));
    }

    let origin_x = base_extent.min_x;
    let origin_y = base_extent.max_y;
    let gt = GeoTransform { origin_x, origin_y, pixel_width: cell_size, pixel_height: -cell_size };

    // Accumulate in f64 to avoid precision loss when many point contributions overlap
    let mut grid = crate::rasterops::new_band(w, h_rows, 0.0f64)?;
    // (The output's room too, before the work: a grid that can't be held fails first.)
    let mut grid_f32 = crate::rasterops::grid(w, h_rows)?;

    match kernel {
        KernelType::Quartic => {
            let factor = 3.0 / (PI * h_sq);
            if !factor.is_normal() {
                return Err("search radius is out of range".into());
            }
            accumulate_density(threads, points, &mut grid, w, h_rows, origin_x, origin_y, cell_size, bandwidth, h_sq, |d_sq, wgt| {
                let val = 1.0 - d_sq / h_sq;
                wgt * factor * val * val
            })?;
        }
        KernelType::Gaussian => {
            // Truncate at 3σ = bandwidth and renormalise so the kernel integrates to 1 over the disk.
            let sigma = bandwidth / 3.0;
            let two_sigma_sq = 2.0 * sigma * sigma;
            if !two_sigma_sq.is_normal() {
                return Err("search radius is out of range".into());
            }
            let norm = 1.0 - (-4.5f64).exp();
            let factor = 1.0 / (PI * two_sigma_sq * norm);
            if !factor.is_normal() {
                return Err("search radius is out of range".into());
            }
            accumulate_density(threads, points, &mut grid, w, h_rows, origin_x, origin_y, cell_size, bandwidth, h_sq, |d_sq, wgt| {
                wgt * factor * (-d_sq / two_sigma_sq).exp()
            })?;
        }
    }

    for d in grid {
        let val = d as f32;
        if !val.is_finite() {
            return Err("calculated density values produced non-finite numbers; check weights and radius".into());
        }
        grid_f32.push(val);
    }

    Raster::new(name, w as u32, h_rows as u32, vec![grid_f32], gt, crs, None, ColorHint::Gray, "Float32").map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_point_quartic_density_shape() {
        let pt = Coord::new(100.0, 100.0);
        let points = vec![(pt, 1.0)];
        let h = 20.0;
        let cell = 2.0;
        let r = kernel_density(&points, None, cell, h, KernelType::Quartic, "test_density", Crs::WebMercator).unwrap();

        assert_eq!(r.bands.len(), 1);
        let band = &r.bands[0].data;

        // Peak should be at or near point center
        let (p_col, p_row) = r.transform.to_pixel(pt);
        let c_idx = (p_row.floor() as usize) * (r.width as usize) + (p_col.floor() as usize);
        let peak_val = band[c_idx];
        assert!(peak_val > 0.0, "peak value must be positive, got {peak_val}");

        // Outside radius h (e.g. corner of grid), density must be 0.0
        let corner_val = band[0];
        assert_eq!(corner_val, 0.0, "corner value outside bandwidth must be zero");

        // Verify values are strictly non-negative and finite
        for &v in band {
            assert!(v >= 0.0 && v.is_finite());
        }
    }

    #[test]
    fn weights_are_proportional() {
        let pt = Coord::new(500.0, 500.0);
        let r1 = kernel_density(&[(pt, 1.0)], None, 5.0, 50.0, KernelType::Quartic, "d1", Crs::WebMercator).unwrap();
        let r2 = kernel_density(&[(pt, 3.5)], None, 5.0, 50.0, KernelType::Quartic, "d2", Crs::WebMercator).unwrap();

        let b1 = &r1.bands[0].data;
        let b2 = &r2.bands[0].data;
        assert_eq!(b1.len(), b2.len());

        for (v1, v2) in b1.iter().zip(b2.iter()) {
            let expected = (*v1 as f64) * 3.5;
            let actual = *v2 as f64;
            assert!((actual - expected).abs() <= 1e-5 * expected.abs() + 1e-12, "weight scaling mismatch: {v1} vs {v2}");
        }
    }

    #[test]
    fn tiny_or_subnormal_radius_is_rejected() {
        let pt = Coord::new(100.0, 100.0);
        let err = kernel_density(&[(pt, 1.0)], None, 1.0, 1e-160, KernelType::Quartic, "tiny", Crs::WebMercator).unwrap_err();
        assert!(err.contains("search radius is out of range"), "{err}");
    }

    #[test]
    fn additive_point_contributions() {
        let pt1 = Coord::new(100.0, 100.0);
        let pt2 = Coord::new(110.0, 100.0);
        let h = 30.0;
        let cell = 5.0;

        let r_both = kernel_density(&[(pt1, 1.0), (pt2, 1.0)], None, cell, h, KernelType::Quartic, "both", Crs::WebMercator).unwrap();
        let r1 = kernel_density(
            &[(pt1, 1.0)],
            Some(&Extent::new(70.0, 70.0, 140.0, 130.0)),
            cell,
            h,
            KernelType::Quartic,
            "r1",
            Crs::WebMercator,
        )
        .unwrap();
        let r2 = kernel_density(
            &[(pt2, 1.0)],
            Some(&Extent::new(70.0, 70.0, 140.0, 130.0)),
            cell,
            h,
            KernelType::Quartic,
            "r2",
            Crs::WebMercator,
        )
        .unwrap();

        assert_eq!(r1.bands[0].data.len(), r2.bands[0].data.len());
        for (i, (&v1, &v2)) in r1.bands[0].data.iter().zip(r2.bands[0].data.iter()).enumerate() {
            let sum = v1 + v2;
            let actual = r_both.bands[0].data[i];
            assert!((sum - actual).abs() < 1e-5, "additive discrepancy at {i}: {sum} vs {actual}");
        }
    }

    #[test]
    fn bandwidth_spread_effect() {
        let pt = Coord::new(100.0, 100.0);
        let r_narrow = kernel_density(&[(pt, 1.0)], None, 2.0, 10.0, KernelType::Quartic, "narrow", Crs::WebMercator).unwrap();
        let r_wide = kernel_density(&[(pt, 1.0)], None, 2.0, 40.0, KernelType::Quartic, "wide", Crs::WebMercator).unwrap();

        // Narrow bandwidth has higher peak density and smaller spatial footprint
        let peak_narrow = r_narrow.bands[0].data.iter().copied().fold(0.0f32, f32::max);
        let peak_wide = r_wide.bands[0].data.iter().copied().fold(0.0f32, f32::max);
        assert!(peak_narrow > peak_wide, "narrow bandwidth peak ({peak_narrow}) should exceed wide bandwidth peak ({peak_wide})");
    }

    #[test]
    fn gaussian_kernel_works() {
        let pt = Coord::new(200.0, 200.0);
        let r = kernel_density(&[(pt, 2.0)], None, 5.0, 50.0, KernelType::Gaussian, "gauss", Crs::WebMercator).unwrap();
        assert_eq!(r.bands.len(), 1);
        let peak = r.bands[0].data.iter().copied().fold(0.0f32, f32::max);
        assert!(peak > 0.0);
    }

    #[test]
    fn silverman_bandwidth_is_deterministic_and_positive() {
        let pts = vec![
            (Coord::new(0.0, 0.0), 1.0),
            (Coord::new(100.0, 0.0), 1.0),
            (Coord::new(0.0, 100.0), 1.0),
            (Coord::new(100.0, 100.0), 1.0),
            (Coord::new(50.0, 50.0), 1.0),
        ];
        let ext = Extent::new(0.0, 0.0, 100.0, 100.0);
        let h1 = silverman_bandwidth(&pts, &ext);
        let h2 = silverman_bandwidth(&pts, &ext);
        assert_eq!(h1, h2, "bandwidth must be deterministic");
        assert!(h1 > 0.0 && h1.is_finite());

        // Coincident points fallback
        let coincident = vec![(Coord::new(50.0, 50.0), 1.0), (Coord::new(50.0, 50.0), 1.0)];
        let h_coincident = silverman_bandwidth(&coincident, &Extent::new(50.0, 50.0, 50.0, 50.0));
        assert!(h_coincident > 0.0 && h_coincident.is_finite());
    }

    #[test]
    fn geotiff_roundtrip_preserves_values_and_metadata() {
        let pt = Coord::new(500_000.0, 4_500_000.0); // UTM coordinates
        let crs = Crs::from_epsg(32618).unwrap();
        let r = kernel_density(&[(pt, 1.0)], None, 50.0, 500.0, KernelType::Quartic, "utm_density", crs).unwrap();

        let bytes = rustgis_data::raster::geotiff::write(&r, 0).unwrap();
        assert!(!bytes.is_empty());

        let reopened = rustgis_data::raster::geotiff::read(std::io::Cursor::new(&bytes), "utm_density").unwrap();
        assert_eq!(reopened.width, r.width);
        assert_eq!(reopened.height, r.height);
        assert_eq!(reopened.crs, crs);
        assert_eq!(reopened.transform, r.transform);

        assert_eq!(reopened.bands[0].data, r.bands[0].data, "GeoTIFF Float32 roundtrip must be bit-exact");
    }

    #[test]
    fn excessive_workload_is_rejected() {
        // 50,000 points in a 100x100 grid (10,000 cells <= DENSITY_CELLS_FLOOR) with large radius:
        // 500 million cell visits, more than one thread takes (several may take them).
        let pts: Vec<(Coord, f64)> = (0..50_000).map(|_| (Coord::new(500.0, 500.0), 1.0)).collect();
        let ext = Extent::new(0.0, 0.0, 1000.0, 1000.0);
        let err = kernel_density_on(Some(1), &pts, Some(&ext), 10.0, 1000.0, KernelType::Quartic, "huge", Crs::WebMercator).unwrap_err();
        assert!(
            err.contains("too much work") && err.contains(&format!("the {MAX_DENSITY_WORK} cell visits")) && err.contains("1 thread(s)"),
            "{err}"
        );
    }

    #[test]
    fn bandwidth_overflow_is_rejected() {
        let pts = vec![(Coord::new(0.0, 0.0), 1.0)];
        let err = kernel_density(&pts, None, 1.0, 1e200, KernelType::Quartic, "huge_bw", Crs::WebMercator).unwrap_err();
        assert!(err.contains("search radius is out of range"), "{err}");
    }

    #[test]
    fn geographic_crs_is_rejected() {
        let pts = vec![(Coord::new(10.0, 20.0), 1.0)];
        let err = kernel_density(&pts, None, 1.0, 10.0, KernelType::Quartic, "geo_crs", Crs::LonLat).unwrap_err();
        assert!(err.contains("requires a projected coordinate system"), "{err}");
    }

    #[test]
    fn non_finite_points_are_skipped() {
        let pts = vec![(Coord::new(f64::NAN, 100.0), 1.0), (Coord::new(100.0, f64::INFINITY), 1.0), (Coord::new(100.0, 100.0), 1.0)];
        let r = kernel_density(&pts, None, 2.0, 20.0, KernelType::Quartic, "finite_test", Crs::WebMercator).unwrap();
        for &val in &r.bands[0].data {
            assert!(val.is_finite());
            assert!(val >= 0.0);
        }
    }

    #[test]
    fn pixel_size_larger_than_bandwidth_is_rejected() {
        let pts = vec![(Coord::new(100.0, 100.0), 1.0)];
        let err = kernel_density(&pts, None, 50.0, 10.0, KernelType::Quartic, "test", Crs::WebMercator).unwrap_err();
        assert!(err.contains("pixel size must not be larger than"), "{err}");
    }

    #[test]
    fn gaussian_kernel_integrates_to_one() {
        let pt = Coord::new(100.0, 100.0);
        let cell = 0.5;
        let h = 30.0;
        let r = kernel_density(&[(pt, 1.0)], None, cell, h, KernelType::Gaussian, "gauss_int", Crs::WebMercator).unwrap();
        let total_mass: f64 = r.bands[0].data.iter().map(|&v| (v as f64) * cell * cell).sum();
        assert!((total_mass - 1.0).abs() < 0.02, "expected Gaussian integral ≈ 1.0, got {total_mass}");
    }

    #[test]
    fn quartic_kernel_integrates_to_one() {
        let pt = Coord::new(100.0, 100.0);
        let cell = 0.5;
        let h = 30.0;
        let r = kernel_density(&[(pt, 1.0)], None, cell, h, KernelType::Quartic, "quartic_int", Crs::WebMercator).unwrap();
        let total_mass: f64 = r.bands[0].data.iter().map(|&v| (v as f64) * cell * cell).sum();
        assert!((total_mass - 1.0).abs() < 0.02, "expected Quartic integral ≈ 1.0, got {total_mass}");
    }

    #[test]
    fn extent_helpers_work_as_expected() {
        let pts = vec![(Coord::new(10.0, 20.0), 1.0), (Coord::new(30.0, 40.0), 2.0)];
        let pe = points_extent(&pts).unwrap();
        assert_eq!(pe, Extent::new(10.0, 20.0, 30.0, 40.0));

        let ge_auto = resolve_grid_extent(&pe, 5.0, None).unwrap();
        assert_eq!(ge_auto, Extent::new(5.0, 15.0, 35.0, 45.0));

        let custom = Extent::new(0.0, 0.0, 100.0, 100.0);
        let ge_custom = resolve_grid_extent(&pe, 5.0, Some(&custom)).unwrap();
        assert_eq!(ge_custom, custom);
    }

    #[test]
    fn overflowing_f32_weight_is_rejected() {
        let pt = Coord::new(100.0, 100.0);
        let err = kernel_density(&[(pt, 1e42)], None, 5.0, 50.0, KernelType::Quartic, "huge_weight", Crs::WebMercator).unwrap_err();
        assert!(err.contains("calculated density values produced non-finite numbers"), "{err}");
    }

    /// A density made on several threads is the one made on one, bit for bit (#1252): each strip
    /// of rows adds the points that reach it in their order, as one thread adds them to a cell.
    #[test]
    fn kernel_density_on_several_threads_is_on_one() {
        // 3,000 points, weighed, clustered and spread, so kernels cross the strips' edges.
        let mut x = 12_345u64;
        let mut next = || {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (x >> 11) as f64 / (1u64 << 53) as f64
        };
        let points: Vec<(Coord, f64)> = (0..3000)
            .map(|i| {
                let (cx, cy) = if i % 3 == 0 { (2_000.0, 3_000.0) } else { (0.0, 0.0) };
                (Coord::new(cx + next() * 5_000.0, cy + next() * 4_000.0), 0.5 + next() * 3.0)
            })
            .collect();
        for kernel in [KernelType::Quartic, KernelType::Gaussian] {
            let make = |threads| kernel_density_on(Some(threads), &points, None, 20.0, 300.0, kernel, "d", Crs::WebMercator).unwrap();
            let (one, many) = (make(1), make(4));
            assert!(one.height > 100, "{}", one.height);
            let bits = |r: &Raster| r.bands[0].data.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
            assert_eq!(bits(&one), bits(&many), "{kernel:?}");
        }
    }

    /// Progress counts every strip's points to the end, also when the rows don't cut into as many
    /// strips as asked for (101 rows on 16 threads make 51 strips of 2, not 64).
    #[test]
    fn kernel_density_progress_ends_at_its_total() {
        let points: Vec<(Coord, f64)> = (0..5000).map(|i| (Coord::new(f64::from(i % 100) * 10.0, f64::from(i / 50) * 10.0), 1.0)).collect();
        let ext = Extent::new(0.0, 0.0, 1000.0, 1010.0);
        let run = crate::control::Control::new();
        let _installed = crate::control::install(Some(run.clone()));
        let r = kernel_density_on(Some(16), &points, Some(&ext), 10.0, 30.0, KernelType::Quartic, "p", Crs::WebMercator).unwrap();
        assert_eq!(r.height, 101);
        let (done, total) = run.progress().unwrap();
        assert_eq!((done, total), (total, 51 * 5001));
    }

    /// A cancelled run comes back as cancelled, on one thread and on several, not as a grid.
    #[test]
    fn a_cancelled_kernel_density_is_no_grid() {
        let points: Vec<(Coord, f64)> = (0..200).map(|i| (Coord::new(f64::from(i) * 5.0, f64::from(i % 20) * 5.0), 1.0)).collect();
        for threads in [1, 4] {
            let run = crate::control::Control::new();
            run.cancel();
            let _installed = crate::control::install(Some(run));
            let e = kernel_density_on(Some(threads), &points, None, 5.0, 50.0, KernelType::Quartic, "c", Crs::WebMercator).unwrap_err();
            assert_eq!(e, crate::control::CANCELLED, "{threads}");
        }
    }
}
