//! Ordinary Kriging spatial interpolation.
//!
//! Ordinary Kriging calculates the best linear unbiased estimator (BLUE) of a spatial process
//! across a raster grid from scattered point measurements. It models spatial autocorrelation
//! via a theoretical semivariogram (Spherical, Exponential, or Gaussian) and solves a local
//! linear system of Kriging equations for each grid cell.
//!
//! Output is a 2-band raster:
//! - Band 1: Predicted value surface
//! - Band 2: Estimation variance (uncertainty/error surface)

use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
use rustgis_geom::{Coord, Extent};

use crate::rasterops::{NODATA, max_pixels};

/// Maximum cell-point pairs Kriging computes on one thread (about a second's work); a run on several
/// threads may take as many times more, and one that can be cancelled any amount
/// ([`crate::parallel::work_cap`]).
pub const MAX_KRIGING_WORK: usize = 40_000_000;

/// Theoretical semivariogram models for spatial autocorrelation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VariogramModel {
    /// Spherical model: reaches the sill at range `a`. Most commonly used in GIS.
    Spherical,
    /// Exponential model: approaches sill asymptotically (95% at range `a`).
    Exponential,
    /// Gaussian model: parabolic behavior near the origin, suitable for extremely smooth phenomena.
    Gaussian,
}

impl std::str::FromStr for VariogramModel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "spherical" => Ok(Self::Spherical),
            "exponential" => Ok(Self::Exponential),
            "gaussian" => Ok(Self::Gaussian),
            _ => Err(()),
        }
    }
}

impl VariogramModel {
    pub fn parse(s: &str) -> Option<Self> {
        s.parse().ok()
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Spherical => "Spherical",
            Self::Exponential => "Exponential",
            Self::Gaussian => "Gaussian",
        }
    }
}

/// Parameters defining a theoretical semivariogram γ(h).
#[derive(Clone, Copy, Debug)]
pub struct VariogramParams {
    pub model: VariogramModel,
    /// Nugget effect c₀: micro-scale variance and measurement error at h -> 0.
    pub nugget: f64,
    /// Sill (c₀ + c): asymptotic variance at large distances.
    pub sill: f64,
    /// Range a: spatial correlation distance (lag), in the units of the grid's coordinate system.
    /// Semivariance reaches or approaches the sill at this distance.
    pub range: f64,
}

impl VariogramParams {
    /// Computes semivariance γ(h) for distance h.
    pub fn semivariance(&self, h: f64) -> f64 {
        if h <= 0.0 || !h.is_finite() {
            return 0.0;
        }
        let a = self.range.max(1e-9);
        let c0 = self.nugget.max(0.0);
        let c = (self.sill - c0).max(0.0);

        match self.model {
            VariogramModel::Spherical => {
                if h >= a {
                    c0 + c
                } else {
                    let hr = h / a;
                    c0 + c * (1.5 * hr - 0.5 * hr * hr * hr)
                }
            }
            VariogramModel::Exponential => {
                let hr = h / a;
                c0 + c * (1.0 - (-3.0 * hr).exp())
            }
            VariogramModel::Gaussian => {
                let hr = h / a;
                c0 + c * (1.0 - (-3.0 * hr * hr).exp())
            }
        }
    }
}

/// Preallocated reusable workspace buffers for solving the Kriging linear system.
#[derive(Clone, Debug)]
pub struct KrigingBuffers {
    a_mat: Vec<f64>,
    b_vec: Vec<f64>,
    solution: Vec<f64>,
}

impl Default for KrigingBuffers {
    fn default() -> Self {
        Self::new()
    }
}

impl KrigingBuffers {
    pub fn new() -> Self {
        const MAX_M: usize = 33;
        Self { a_mat: vec![0.0; MAX_M * MAX_M], b_vec: vec![0.0; MAX_M], solution: vec![0.0; MAX_M] }
    }
}

/// Estimates sensible default variogram parameters from sample points and extent.
pub fn auto_variogram(points: &[(Coord, f64)], extent: &Extent, model: VariogramModel) -> VariogramParams {
    let n = points.len();
    if n == 0 {
        return VariogramParams { model, nugget: 0.0, sill: 1.0, range: 1.0 };
    }
    let mean = points.iter().map(|(_, v)| *v).sum::<f64>() / (n as f64);
    let var = points.iter().map(|(_, v)| (v - mean).powi(2)).sum::<f64>() / ((n.max(2) - 1) as f64);
    let sill = if var.is_finite() && var > 1e-12 { var } else { 1.0 };
    let diag = (extent.width().powi(2) + extent.height().powi(2)).sqrt();
    let range = if diag.is_finite() && diag > 1e-6 { diag / 3.0 } else { 1.0 };
    VariogramParams { model, nugget: 0.0, sill, range }
}

/// Solves a linear system A * x = b of size `n` with Gaussian elimination and partial pivoting.
/// `a` is stored row-major with size `n * n`.
/// Returns false if the matrix is singular or ill-conditioned.
fn solve_linear_system(n: usize, a: &mut [f64], b: &mut [f64], x: &mut [f64]) -> bool {
    if n == 0 || a.len() < n * n || b.len() < n || x.len() < n {
        return false;
    }
    for col in 0..n {
        let mut pivot_row = col;
        let mut max_val = a.get(col * n + col).copied().unwrap_or(0.0).abs();
        for row in (col + 1)..n {
            let val = a.get(row * n + col).copied().unwrap_or(0.0).abs();
            if val > max_val {
                max_val = val;
                pivot_row = row;
            }
        }
        if !max_val.is_finite() || max_val < 1e-12 {
            return false;
        }
        if pivot_row != col {
            for j in 0..n {
                let idx1 = col * n + j;
                let idx2 = pivot_row * n + j;
                a.swap(idx1, idx2);
            }
            b.swap(col, pivot_row);
        }
        let pivot = match a.get(col * n + col).copied() {
            Some(p) if p.abs() >= 1e-12 => p,
            _ => return false,
        };
        for row in (col + 1)..n {
            let factor = a.get(row * n + col).copied().unwrap_or(0.0) / pivot;
            if !factor.is_finite() {
                return false;
            }
            for j in col..n {
                let col_val = a.get(col * n + j).copied().unwrap_or(0.0);
                if let Some(target) = a.get_mut(row * n + j) {
                    *target -= factor * col_val;
                }
            }
            let b_col = b.get(col).copied().unwrap_or(0.0);
            if let Some(b_target) = b.get_mut(row) {
                *b_target -= factor * b_col;
            }
        }
    }
    // Back substitution
    for i in (0..n).rev() {
        let mut sum = b.get(i).copied().unwrap_or(0.0);
        for j in (i + 1)..n {
            let a_val = a.get(i * n + j).copied().unwrap_or(0.0);
            let x_val = x.get(j).copied().unwrap_or(0.0);
            sum -= a_val * x_val;
        }
        let diag = a.get(i * n + i).copied().unwrap_or(0.0);
        if !diag.is_finite() || diag.abs() < 1e-12 {
            return false;
        }
        if let Some(x_target) = x.get_mut(i) {
            *x_target = sum / diag;
        }
    }
    true
}

/// Performs Ordinary Kriging interpolation for a query point given its nearest neighbors.
/// `near`: slice of `(distance, value, point_coord)` sorted or unsorted.
/// `kx`: cosine of query latitude for metric consistency across both system sides (1 in a
/// projected system).
/// `buffers`: reusable linear system buffers to eliminate per-pixel heap allocations.
/// Returns `(predicted_value, estimation_variance)`.
pub fn ordinary_kriging_point(
    query: Coord,
    near: &[(f64, f64, Coord)],
    params: &VariogramParams,
    kx: f64,
    buffers: &mut KrigingBuffers,
) -> (f64, f64) {
    if near.is_empty() {
        return (0.0, params.sill);
    }
    // Exact interpolator: if on or extremely close to a sample point, return its value exactly.
    // Check both metric distance and raw coordinate proximity so high latitudes don't false-trigger.
    if let Some((_, exact_val, _)) =
        near.iter().find(|(d, _, p)| *d < 1e-10 && (p.x - query.x).abs() < 1e-8 && (p.y - query.y).abs() < 1e-8)
    {
        return (*exact_val, 0.0);
    }

    let k = near.len().min(32);
    let m = k + 1; // system size (k weights + 1 Lagrange multiplier)

    // Solve with gamma / scale so the pivot tolerance does not depend on the data's units.
    // Weights are invariant; the Lagrange multiplier scales by `scale`.
    let scale = {
        let s = params.sill.max(params.nugget);
        if s.is_finite() && s > 0.0 { s } else { 1.0 }
    };

    for i in 0..k {
        let (di, _, pi) = match near.get(i) {
            Some(n) => *n,
            None => continue,
        };
        for j in 0..k {
            let (_, _, pj) = match near.get(j) {
                Some(n) => *n,
                None => continue,
            };
            let dx = (pi.x - pj.x) * kx;
            let dy = pi.y - pj.y;
            let hij = (dx * dx + dy * dy).sqrt();
            let gamma = params.semivariance(hij);
            if let Some(cell) = buffers.a_mat.get_mut(i * m + j) {
                *cell = gamma / scale;
            }
        }
        // Constraint: sum(w_i) = 1
        if let Some(cell) = buffers.a_mat.get_mut(i * m + k) {
            *cell = 1.0;
        }
        if let Some(cell) = buffers.a_mat.get_mut(k * m + i) {
            *cell = 1.0;
        }
        // Right hand side: gamma(h_i0) / scale
        let gamma_0 = params.semivariance(di);
        if let Some(b_val) = buffers.b_vec.get_mut(i) {
            *b_val = gamma_0 / scale;
        }
    }
    // A_mat[k, k] = 0.0
    if let Some(cell) = buffers.a_mat.get_mut(k * m + k) {
        *cell = 0.0;
    }
    // b_vec[k] = 1.0
    if let Some(b_val) = buffers.b_vec.get_mut(k) {
        *b_val = 1.0;
    }

    if solve_linear_system(m, &mut buffers.a_mat[..m * m], &mut buffers.b_vec[..m], &mut buffers.solution[..m]) {
        let mut est = 0.0;
        let mut variance = buffers.solution.get(k).copied().unwrap_or(0.0) * scale; // Lagrange multiplier μ

        for i in 0..k {
            let w = buffers.solution.get(i).copied().unwrap_or(0.0);
            let (_, val, _) = near.get(i).copied().unwrap_or((0.0, 0.0, query));
            est += w * val;
            let gamma_0 = params.semivariance(near.get(i).map(|(d, _, _)| *d).unwrap_or(0.0));
            variance += w * gamma_0;
        }

        // Clamp negative numerical rounding in variance
        let variance = if variance.is_finite() && variance > 0.0 { variance } else { 0.0 };
        (est, variance)
    } else {
        // Fallback to IDW when the matrix is singular (e.g. collinear points or duplicate coords)
        let nearest = near.iter().map(|(d, _, _)| *d).fold(f64::INFINITY, f64::min);
        let (num, den) = near.iter().fold((0.0, 0.0), |(n, d), (dist, val, _)| {
            let wgt = if *dist > 1e-12 { (nearest / dist).powi(2) } else { 1.0 };
            (n + wgt * val, d + wgt)
        });
        let fallback_est = if den > 0.0 { num / den } else { near.first().map(|(_, v, _)| *v).unwrap_or(0.0) };
        (fallback_est, params.sill)
    }
}

/// Ordinary Kriging interpolation of point values onto a raster grid in `crs`. The points, extent,
/// cell size and variogram range are in `crs`: degrees for longitude/latitude, where east-west
/// distances shrink with latitude, or the projected system's units, where they are measured as
/// they are. Returns a 2-band raster:
/// - Band 1: Prediction
/// - Band 2: Variance
pub fn kriging(
    points: &[(Coord, f64)],
    extent: &Extent,
    cell: f64,
    params: &VariogramParams,
    neighbours: usize,
    name: &str,
    crs: Crs,
) -> Result<Raster, String> {
    kriging_on(None, points, extent, cell, params, neighbours, name, crs)
}

/// [`kriging`] on `threads` threads, or as many as its work calls for.
#[allow(clippy::too_many_arguments)]
fn kriging_on(
    threads: Option<usize>,
    points: &[(Coord, f64)],
    extent: &Extent,
    cell: f64,
    params: &VariogramParams,
    neighbours: usize,
    name: &str,
    crs: Crs,
) -> Result<Raster, String> {
    if points.len() < 2 {
        return Err("kriging needs at least two points with values".into());
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
    let geographic = crs.is_geographic();
    let k = if neighbours == 0 { 12.min(points.len()) } else { neighbours.clamp(2, points.len().min(32)) };
    let k3 = (k * k * k) / 3;
    let work_per_cell = points.len().saturating_add(k3);
    let work = w.saturating_mul(h).saturating_mul(work_per_cell);

    // Each row on its own, on several threads when the work is large (#1252), and as many times
    // the work (#1291); a thread has its own distances and system buffers.
    let scratch_bytes = points.len().saturating_mul(std::mem::size_of::<(f64, f64, Coord)>());
    let threads = threads.unwrap_or_else(|| crate::parallel::threads_for_rows(h, work, scratch_bytes));
    let cap = crate::parallel::work_cap(MAX_KRIGING_WORK, threads);
    if work > cap {
        return Err(format!(
            "{w} × {h} cells from {} points is too much work, more than the {cap} this computer takes on {threads} thread(s); use a larger cell size{}",
            points.len(),
            crate::parallel::UNCAPPED_HINT
        ));
    }
    let scratch = || (Vec::<(f64, f64, Coord)>::with_capacity(points.len()), KrigingBuffers::new(), crate::control::Pace::default());
    let (mut pred_data, mut var_data) = (crate::rasterops::new_band(w, h, NODATA)?, crate::rasterops::new_band(w, h, NODATA)?);
    let rows = pred_data.chunks_mut(w).zip(var_data.chunks_mut(w));
    crate::control::begin(h);
    crate::parallel::for_each_row(rows, threads, scratch, |(near, buffers, pace), row, (pred, var): (&mut [f32], &mut [f32])| {
        // East-west degrees shrink with latitude; a projected system's units don't.
        let kx = if geographic { (extent.max_y - (row as f64 + 0.5) * cell).to_radians().cos().abs().max(1e-6) } else { 1.0 };
        for (col, (pred, var)) in pred.iter_mut().zip(var.iter_mut()).enumerate() {
            // A wide row of many points takes long without a cap: Cancel is looked at within it.
            if pace.stop(work_per_cell) {
                return;
            }
            let c = Coord::new(extent.min_x + (col as f64 + 0.5) * cell, extent.max_y - (row as f64 + 0.5) * cell);
            near.clear();
            near.extend(points.iter().map(|(p, v)| {
                let dx = (p.x - c.x) * kx;
                let dy = p.y - c.y;
                let dist = (dx * dx + dy * dy).sqrt();
                (dist, *v, *p)
            }));

            if k < near.len() {
                near.select_nth_unstable_by(k - 1, |a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                near.truncate(k);
            }

            let (val, var_est) = ordinary_kriging_point(c, near, params, kx, buffers);
            *pred = if val.is_finite() { val as f32 } else { NODATA };
            *var = if var_est.is_finite() { var_est as f32 } else { NODATA };
        }
    })?;

    let gt = GeoTransform { origin_x: extent.min_x, origin_y: extent.max_y, pixel_width: cell, pixel_height: -cell };
    Raster::new(name, w as u32, h as u32, vec![pred_data, var_data], gt, crs, Some(NODATA as f64), ColorHint::Gray, "Float32")
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run past one thread's work is refused where nothing can stop it, and taken on by a run
    /// that can be cancelled (#1291 step 2).
    #[test]
    fn kriging_past_the_cap_runs_only_when_it_can_be_cancelled() {
        // 500 × 500 cells from 300 points (and 12 neighbours): about 220 million units, past one
        // thread's 40 million.
        let pts: Vec<(Coord, f64)> =
            (0..300).map(|i| (Coord::new((i % 20) as f64 / 2.0, 40.0 + (i / 20) as f64 / 1.5), i as f64)).collect();
        let extent = Extent::new(0.0, 40.0, 10.0, 50.0);
        let params = auto_variogram(&pts, &extent, VariogramModel::Spherical);
        let e = kriging_on(Some(1), &pts, &extent, 0.02, &params, 12, "k", Crs::LonLat).unwrap_err();
        assert!(e.contains("too much work") && e.contains("on 1 thread(s)"), "{e}");
        let run = crate::control::Control::stoppable();
        run.cancel();
        let _installed = crate::control::install(Some(run));
        assert_eq!(kriging_on(Some(1), &pts, &extent, 0.02, &params, 12, "k", Crs::LonLat).unwrap_err(), crate::control::CANCELLED);
    }

    #[test]
    fn test_exact_interpolation_at_sample_points() {
        let sample_pts = vec![
            (Coord::new(0.0, 0.0), 10.0),
            (Coord::new(10.0, 0.0), 20.0),
            (Coord::new(0.0, 10.0), 30.0),
            (Coord::new(10.0, 10.0), 40.0),
            (Coord::new(5.0, 5.0), 25.0),
        ];
        let extent = Extent::new(0.0, 0.0, 10.0, 10.0);
        let params = auto_variogram(&sample_pts, &extent, VariogramModel::Spherical);
        let mut buffers = KrigingBuffers::new();

        for (pt, expected_val) in &sample_pts {
            let mut near: Vec<(f64, f64, Coord)> =
                sample_pts.iter().map(|(p, v)| (((p.x - pt.x).powi(2) + (p.y - pt.y).powi(2)).sqrt(), *v, *p)).collect();
            near.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

            let (val, var) = ordinary_kriging_point(*pt, &near, &params, 1.0, &mut buffers);
            assert!((val - expected_val).abs() < 1e-6, "Kriging at sample point {:?} gave {}, expected {}", pt, val, expected_val);
            assert!(var.abs() < 1e-6, "Variance at sample point {:?} was {}, expected 0.0", pt, var);
        }
    }

    #[test]
    fn test_planar_surface_reconstruction_accuracy() {
        // True function: z = 5.0 + 2.0*x + 3.0*y
        let f = |x: f64, y: f64| 5.0 + 2.0 * x + 3.0 * y;

        let mut pts = Vec::new();
        for i in 0..5 {
            for j in 0..5 {
                let x = i as f64 * 2.0;
                let y = j as f64 * 2.0;
                pts.push((Coord::new(x, y), f(x, y)));
            }
        }
        let _extent = Extent::new(0.0, 0.0, 8.0, 8.0);
        let params = VariogramParams { model: VariogramModel::Spherical, nugget: 0.0, sill: 50.0, range: 12.0 };
        let mut buffers = KrigingBuffers::new();

        // Test at intermediate points not in the training set
        let test_coords = vec![Coord::new(1.0, 1.0), Coord::new(3.0, 3.0), Coord::new(5.0, 1.0), Coord::new(3.0, 5.0)];

        for query in test_coords {
            let mut near: Vec<(f64, f64, Coord)> =
                pts.iter().map(|(p, v)| (((p.x - query.x).powi(2) + (p.y - query.y).powi(2)).sqrt(), *v, *p)).collect();
            near.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

            // With the 4 closest symmetric neighbors, Ordinary Kriging reproduces the plane with sub-millimeter precision
            let mut near_4 = near.clone();
            near_4.truncate(4);
            let (val_4, _) = ordinary_kriging_point(query, &near_4, &params, 1.0, &mut buffers);
            let expected = f(query.x, query.y);
            assert!((val_4 - expected).abs() < 1e-4, "4-neighbor query at {:?}: predicted {}, expected {}", query, val_4, expected);

            // With 12 neighbors, smoothing bias on an un-detrended ramp is within 1.5% relative error
            near.truncate(12);
            let (val, var) = ordinary_kriging_point(query, &near, &params, 1.0, &mut buffers);
            let error = (val - expected).abs();
            assert!(error < 0.75, "12-neighbor query at {:?}: predicted {}, expected {}, error {}", query, val, expected, error);
            assert!(var >= 0.0, "Variance should be non-negative: {}", var);
        }
    }

    /// Rows made on several threads give the same surfaces, bit for bit, as one thread (#1252).
    #[test]
    fn kriging_on_several_threads_is_kriging_on_one() {
        let pts: Vec<(Coord, f64)> = (0..300)
            .map(|i| {
                let (x, y) = ((i * 37 % 300) as f64 / 30.0, (i * 91 % 300) as f64 / 30.0);
                (Coord::new(x, 40.0 + y), (x * 1.3).sin() * 100.0 + y * 7.0)
            })
            .collect();
        let extent = Extent::new(0.0, 40.0, 10.0, 50.0);
        let params = auto_variogram(&pts, &extent, VariogramModel::Spherical);
        let one = kriging_on(Some(1), &pts, &extent, 0.2, &params, 12, "k", Crs::LonLat).unwrap();
        for threads in [2, 7, 16] {
            let many = kriging_on(Some(threads), &pts, &extent, 0.2, &params, 12, "k", Crs::LonLat).unwrap();
            for (a, b) in one.bands.iter().zip(&many.bands) {
                assert!(a.data.iter().map(|v| v.to_bits()).eq(b.data.iter().map(|v| v.to_bits())), "{threads} threads");
            }
        }
    }

    #[test]
    fn test_high_latitude_metric_consistency() {
        let lat = 60.0;
        let pts = vec![(Coord::new(0.0, lat), 10.0), (Coord::new(1.0, lat), 20.0), (Coord::new(0.0, lat + 1.0), 30.0)];
        let extent = Extent::new(0.0, lat, 1.0, lat + 1.0);
        let params = auto_variogram(&pts, &extent, VariogramModel::Spherical);
        let r = kriging(&pts, &extent, 0.25, &params, 3, "kriging_hi_lat", Crs::LonLat).unwrap();
        assert_eq!(r.bands.len(), 2);
        for band in &r.bands {
            for v in &band.data {
                assert!(r.is_valid(*v));
            }
        }
    }

    #[test]
    fn test_variogram_models_monotonicity_and_sill() {
        let models = [VariogramModel::Spherical, VariogramModel::Exponential, VariogramModel::Gaussian];
        for model in models {
            let p = VariogramParams { model, nugget: 1.0, sill: 10.0, range: 5.0 };
            assert_eq!(p.semivariance(0.0), 0.0);
            assert!(p.semivariance(1.0) > 0.0);
            assert!(p.semivariance(3.0) >= p.semivariance(1.0));
            // Beyond range
            let beyond = p.semivariance(10.0);
            assert!((beyond - 10.0).abs() < 0.1, "{:?} at 10.0 was {}", model, beyond);
        }
    }

    #[test]
    fn test_kriging_raster_output_has_two_bands() {
        let pts =
            vec![(Coord::new(0.0, 0.0), 10.0), (Coord::new(2.0, 0.0), 20.0), (Coord::new(0.0, 2.0), 15.0), (Coord::new(2.0, 2.0), 25.0)];
        let extent = Extent::new(0.0, 0.0, 2.0, 2.0);
        let params = auto_variogram(&pts, &extent, VariogramModel::Spherical);
        let r = kriging(&pts, &extent, 0.5, &params, 4, "test_kriging", Crs::LonLat).unwrap();
        assert_eq!(r.bands.len(), 2, "Kriging should produce Prediction and Variance bands");
        assert_eq!(r.width, 4);
        assert_eq!(r.height, 4);
        // All cells should be valid finite values
        for band in &r.bands {
            for v in &band.data {
                assert!(r.is_valid(*v));
            }
        }
    }

    #[test]
    fn test_hostile_inputs_never_panic() {
        let extent = Extent::new(0.0, 0.0, 2.0, 2.0);
        let params = VariogramParams { model: VariogramModel::Spherical, nugget: 0.0, sill: 1.0, range: 1.0 };
        // Less than 2 points
        assert!(kriging(&[], &extent, 0.5, &params, 4, "t", Crs::LonLat).is_err());
        assert!(kriging(&[(Coord::new(0.0, 0.0), 1.0)], &extent, 0.5, &params, 4, "t", Crs::LonLat).is_err());
        // Negative / invalid cell size
        let pts = vec![(Coord::new(0.0, 0.0), 1.0), (Coord::new(1.0, 1.0), 2.0)];
        assert!(kriging(&pts, &extent, -1.0, &params, 4, "t", Crs::LonLat).is_err());
        assert!(kriging(&pts, &extent, 0.0, &params, 4, "t", Crs::LonLat).is_err());
        assert!(kriging(&pts, &extent, f64::NAN, &params, 4, "t", Crs::LonLat).is_err());
        // Empty extent
        assert!(kriging(&pts, &Extent::EMPTY, 0.5, &params, 4, "t", Crs::LonLat).is_err());
    }

    #[test]
    fn test_kriging_scale_invariance_with_small_values() {
        let pts =
            vec![(Coord::new(0.0, 0.0), 10.0), (Coord::new(2.0, 0.0), 20.0), (Coord::new(0.0, 2.0), 15.0), (Coord::new(2.0, 2.0), 25.0)];
        let extent = Extent::new(0.0, 0.0, 2.0, 2.0);
        let params = auto_variogram(&pts, &extent, VariogramModel::Spherical);
        let r_orig = kriging(&pts, &extent, 0.5, &params, 4, "orig", Crs::LonLat).unwrap();

        let scale = 1e-6_f64;
        let scaled_pts: Vec<(Coord, f64)> = pts.iter().map(|(c, v)| (*c, v * scale)).collect();
        let scaled_params = auto_variogram(&scaled_pts, &extent, VariogramModel::Spherical);
        let r_scaled = kriging(&scaled_pts, &extent, 0.5, &scaled_params, 4, "scaled", Crs::LonLat).unwrap();

        for (v_orig, v_scaled) in r_orig.bands[0].data.iter().zip(&r_scaled.bands[0].data) {
            let expected = *v_orig * (scale as f32);
            assert!((*v_scaled - expected).abs() <= 1e-10, "scaled {v_scaled} vs expected {expected}");
        }

        let scale_sq = (scale * scale) as f32;
        for (v_orig, v_scaled) in r_orig.bands[1].data.iter().zip(&r_scaled.bands[1].data) {
            let expected = *v_orig * scale_sq;
            assert!((*v_scaled - expected).abs() <= 1e-12, "variance {v_scaled} vs expected {expected}");
        }
    }
}
