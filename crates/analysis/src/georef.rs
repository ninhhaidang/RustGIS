//! Control-point fits for georeferencing: transforms from pixel positions (column, row) to map
//! coordinates and back, fitted to control points, with each point's residual.
//!
//! Residuals follow QGIS's georeferencer: in pixels, a point's map position is taken back
//! through the inverse fit and compared with its pixel; in map units, its pixel is taken through
//! the forward fit and compared with its map position. Similarity and projective fits invert
//! exactly; affine, polynomial and spline fits are fitted in each direction, as GDAL does.
//! Coordinates are centred and scaled before fitting, so a scan tens of thousands of pixels wide
//! placed in state plane feet stays well conditioned.
//!
//! Inside this module the image side is (column, −row), the way QGIS stores source points, so
//! the image and the map turn the same way and a similarity fit needs no mirror.

/// Most control points a fit takes.
pub const MAX_POINTS: usize = 1000;
/// Most points switched on for which the spline's leave-one-out error is worked out: each point
/// refits two splines, which past this takes seconds.
pub const MAX_SPLINE_LEAVE_ONE_OUT: usize = 250;
/// A transform whose leave-one-out error is within this fraction of the best is as good as the
/// best, and the simplest such one is recommended.
pub const RECOMMEND_TOLERANCE: f64 = 0.1;

/// The transforms, simplest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Shift, rotation and one scale (Helmert): shapes and angles are kept.
    Similarity,
    /// Adds shear and separate scales (first-order polynomial): straight and parallel lines are kept.
    Affine,
    /// A tilted plane: straight lines are kept, parallel ones may meet.
    Projective,
    /// Second-order polynomial: gentle bending.
    Polynomial2,
    /// Third-order polynomial: more bending.
    Polynomial3,
    /// Thin plate spline: passes through every point and bends between them (rubber sheeting).
    Spline,
}

impl Kind {
    /// Every transform, simplest first (the order [`compare`] recommends in).
    pub const ALL: [Kind; 6] = [Kind::Similarity, Kind::Affine, Kind::Projective, Kind::Polynomial2, Kind::Polynomial3, Kind::Spline];

    /// A transform by its id, or by the name QGIS or GDAL gives it (helmert, polynomial1, tps).
    pub fn parse(s: &str) -> Option<Kind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "similarity" | "helmert" => Some(Kind::Similarity),
            "affine" | "polynomial1" => Some(Kind::Affine),
            "projective" => Some(Kind::Projective),
            "polynomial2" => Some(Kind::Polynomial2),
            "polynomial3" => Some(Kind::Polynomial3),
            "spline" | "tps" | "thinplatespline" => Some(Kind::Spline),
            _ => None,
        }
    }

    /// The id commands take and return.
    pub fn id(self) -> &'static str {
        match self {
            Kind::Similarity => "similarity",
            Kind::Affine => "affine",
            Kind::Projective => "projective",
            Kind::Polynomial2 => "polynomial2",
            Kind::Polynomial3 => "polynomial3",
            Kind::Spline => "spline",
        }
    }

    /// Fewest points that fix the transform. These are QGIS's counts, except the spline, which
    /// needs three points off a line for its affine part.
    pub fn min_points(self) -> usize {
        match self {
            Kind::Similarity => 2,
            Kind::Affine => 3,
            Kind::Projective => 4,
            Kind::Polynomial2 => 6,
            Kind::Polynomial3 => 10,
            Kind::Spline => 3,
        }
    }
}

/// A control point: a pixel position (column, row; rows go down) and its map coordinates.
/// Points switched off are left out of the fit but still get residuals.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gcp {
    pub col: f64,
    pub row: f64,
    pub x: f64,
    pub y: f64,
    pub on: bool,
}

/// A fitted transform, both ways.
#[derive(Clone, Debug, PartialEq)]
pub struct Fit {
    pub kind: Kind,
    forward: Model,
    inverse: Model,
}

impl Fit {
    /// Map coordinates of a pixel position.
    pub fn to_map(&self, col: f64, row: f64) -> Option<(f64, f64)> {
        self.forward.apply((col, -row))
    }

    /// Pixel position (column, row) of map coordinates.
    pub fn to_pixel(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        self.inverse.apply((x, y)).map(|(c, w)| (c, -w))
    }

    /// A similarity or affine fit as GDAL's geotransform: x at the origin, x per column, x per
    /// row, y at the origin, y per column, y per row (a world file's numbers, reordered).
    pub fn affine(&self) -> Option<[f64; 6]> {
        match (self.kind, &self.forward) {
            (Kind::Similarity | Kind::Affine, Model::Matrix(m)) => Some([m[2], m[0], -m[1], m[5], m[3], -m[4]]),
            _ => None,
        }
    }
}

/// How far a point's fit misses it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Residual {
    /// The map position taken back to the image, minus the point's pixel (columns, rows down).
    pub pixels: (f64, f64),
    /// The pixel taken to the map, minus the point's map position (map units).
    pub map: (f64, f64),
}

/// A fit and how well it holds: residuals for every point, and errors over the points switched on.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub fit: Fit,
    /// Points switched on, and the fewest the transform needs.
    pub used: usize,
    pub minimum: usize,
    /// One per point given (`None` where a position can't be taken through the fit).
    pub residuals: Vec<Option<Residual>>,
    /// Root mean square of the residuals over the points switched on: (pixels, map units).
    pub rmse: (f64, f64),
    /// QGIS's mean error: the same sum over (points used − the fewest needed), zero at the minimum.
    pub mean_error: (f64, f64),
    /// Each point's error when it is left out of the fit, (pixels, map units); `None` for points
    /// switched off, or where the fit without it can't be made.
    pub leave_one_out: Option<Vec<Option<(f64, f64)>>>,
    /// Root mean square of those errors.
    pub leave_one_out_rmse: Option<(f64, f64)>,
    /// Why there is no leave-one-out error, when there isn't one.
    pub leave_one_out_note: Option<String>,
}

/// Fits a transform to the points switched on.
pub fn fit(kind: Kind, points: &[Gcp]) -> Result<Fit, String> {
    if points.len() > MAX_POINTS {
        return Err(format!("a fit takes at most {MAX_POINTS} control points"));
    }
    let on: Vec<&Gcp> = points.iter().filter(|g| g.on).collect();
    if on.len() < kind.min_points() {
        return Err(format!("a {} fit needs at least {} points switched on, not {}", kind.id(), kind.min_points(), on.len()));
    }
    if on.iter().any(|g| ![g.col, g.row, g.x, g.y].iter().all(|v| v.is_finite())) {
        return Err("control point coordinates must be finite numbers".into());
    }
    let src: Vec<(f64, f64)> = on.iter().map(|g| (g.col, -g.row)).collect();
    let dst: Vec<(f64, f64)> = on.iter().map(|g| (g.x, g.y)).collect();
    fit_pairs(kind, &src, &dst)
        .ok_or_else(|| format!("the points can't fix a {} fit: some sit on top of each other or too many fall on a line", kind.id()))
}

/// Fits a transform and works out its residuals, errors and (when asked) leave-one-out errors.
pub fn assess(kind: Kind, points: &[Gcp], leave_one_out: bool) -> Result<Report, String> {
    assess_on(None, kind, points, leave_one_out)
}

/// [`assess`] on `threads` threads (none: as many as pay off).
fn assess_on(threads: Option<usize>, kind: Kind, points: &[Gcp], leave_one_out: bool) -> Result<Report, String> {
    let fit = fit(kind, points)?;
    let residuals: Vec<Option<Residual>> = points.iter().map(|g| residual(&fit, g)).collect();
    let used = points.iter().filter(|g| g.on).count();
    let minimum = kind.min_points();
    let (mut sp, mut sm, mut n) = (0.0, 0.0, 0usize);
    for (g, r) in points.iter().zip(&residuals) {
        if let (true, Some(r)) = (g.on, r) {
            sp += r.pixels.0.powi(2) + r.pixels.1.powi(2);
            sm += r.map.0.powi(2) + r.map.1.powi(2);
            n += 1;
        }
    }
    let rmse = if n > 0 { ((sp / n as f64).sqrt(), (sm / n as f64).sqrt()) } else { (0.0, 0.0) };
    let mean_error =
        if used > minimum { ((sp / (used - minimum) as f64).sqrt(), (sm / (used - minimum) as f64).sqrt()) } else { (0.0, 0.0) };
    let mut report =
        Report { fit, used, minimum, residuals, rmse, mean_error, leave_one_out: None, leave_one_out_rmse: None, leave_one_out_note: None };
    if !leave_one_out {
        return Ok(report);
    }
    if used <= minimum {
        report.leave_one_out_note = Some(format!("leaving a point out needs at least {} points switched on", minimum + 1));
        return Ok(report);
    }
    if kind == Kind::Spline && used > MAX_SPLINE_LEAVE_ONE_OUT {
        report.leave_one_out_note =
            Some(format!("the spline's leave-one-out error is worked out for up to {MAX_SPLINE_LEAVE_ONE_OUT} points switched on"));
        return Ok(report);
    }
    // One refit a point left out, each on its own, so they go on several threads when the work is
    // large (#1252), each thread switching points off in its own copy. A fit weighs every point,
    // and the spline solves a system of them (n³).
    let n = points.len();
    let per_fit = if kind == Kind::Spline { n.saturating_mul(n).saturating_mul(n) / 4 } else { n.saturating_mul(16) };
    let threads = threads
        .unwrap_or_else(|| crate::parallel::threads_for_rows(n, n.saturating_mul(per_fit), n.saturating_mul(std::mem::size_of::<Gcp>())));
    let indices: Vec<usize> = (0..n).collect();
    crate::control::begin(n);
    let errors = crate::parallel::try_map_on(
        &indices,
        threads,
        || points.to_vec(),
        |rest, _, &i| {
            let Some(g) = points.get(i).copied().filter(|g| g.on) else { return Ok::<_, String>(None) };
            if let Some(r) = rest.get_mut(i) {
                r.on = false;
            }
            let e = fit_loose(kind, rest).and_then(|f| residual(&f, &g)).map(|r| (r.pixels.0.hypot(r.pixels.1), r.map.0.hypot(r.map.1)));
            if let Some(r) = rest.get_mut(i) {
                r.on = true;
            }
            Ok(e)
        },
    )?;
    // A point the fit can't do without is the one that says most about it: when leaving any
    // point out breaks the fit, there is no overall error, rather than one over the easy points.
    let missing: Vec<String> =
        points.iter().zip(&errors).enumerate().filter(|(_, (g, e))| g.on && e.is_none()).map(|(i, _)| (i + 1).to_string()).collect();
    if missing.is_empty() {
        let kept: Vec<(f64, f64)> = errors.iter().flatten().copied().collect();
        let k = kept.len().max(1) as f64;
        report.leave_one_out_rmse =
            Some(((kept.iter().map(|e| e.0 * e.0).sum::<f64>() / k).sqrt(), (kept.iter().map(|e| e.1 * e.1).sum::<f64>() / k).sqrt()));
    } else {
        let shown = missing.iter().take(10).cloned().collect::<Vec<_>>().join(", ");
        let more = if missing.len() > 10 { format!(" and {} more", missing.len() - 10) } else { String::new() };
        report.leave_one_out_note = Some(format!("the fit can't be made without point {shown}{more}, so it can't be checked against them"));
    }
    report.leave_one_out = Some(errors);
    Ok(report)
}

/// Assesses every transform, and recommends the simplest whose leave-one-out error (in pixels) is
/// within [`RECOMMEND_TOLERANCE`] of the best. More bending always lowers the error at the
/// points; it has to lower the error at points it wasn't fitted to. A transform the points can't
/// fix comes back with the reason.
pub fn compare(points: &[Gcp]) -> (Vec<(Kind, Result<Report, String>)>, Option<Kind>) {
    let reports: Vec<(Kind, Result<Report, String>)> = Kind::ALL.iter().map(|k| (*k, assess(*k, points, true))).collect();
    let loo = |r: &Result<Report, String>| r.as_ref().ok().and_then(|r| r.leave_one_out_rmse).map(|e| e.0);
    let best = reports.iter().filter_map(|(_, r)| loo(r)).fold(f64::INFINITY, f64::min);
    let recommended = best
        .is_finite()
        .then(|| reports.iter().find(|(_, r)| loo(r).is_some_and(|e| e <= best * (1.0 + RECOMMEND_TOLERANCE) + 1e-9)))
        .flatten()
        .map(|(k, _)| *k);
    (reports, recommended)
}

fn fit_loose(kind: Kind, points: &[Gcp]) -> Option<Fit> {
    fit(kind, points).ok()
}

fn residual(fit: &Fit, g: &Gcp) -> Option<Residual> {
    let (c, r) = fit.to_pixel(g.x, g.y)?;
    let (x, y) = fit.to_map(g.col, g.row)?;
    let res = Residual { pixels: (c - g.col, r - g.row), map: (x - g.x, y - g.y) };
    [res.pixels.0, res.pixels.1, res.map.0, res.map.1].iter().all(|v| v.is_finite()).then_some(res)
}

fn fit_pairs(kind: Kind, src: &[(f64, f64)], dst: &[(f64, f64)]) -> Option<Fit> {
    let (forward, inverse) = match kind {
        Kind::Similarity => {
            let m = similarity(src, dst)?;
            (Model::Matrix(m), Model::Matrix(invert3(&m)?))
        }
        Kind::Affine => (Model::Matrix(affine(src, dst)?), Model::Matrix(affine(dst, src)?)),
        Kind::Projective => {
            let m = projective(src, dst)?;
            (Model::Matrix(m), Model::Matrix(invert3(&m)?))
        }
        Kind::Polynomial2 => (poly(2, src, dst)?, poly(2, dst, src)?),
        Kind::Polynomial3 => (poly(3, src, dst)?, poly(3, dst, src)?),
        Kind::Spline => (spline(src, dst)?, spline(dst, src)?),
    };
    Some(Fit { kind, forward, inverse })
}

/// One direction of a fit.
#[derive(Clone, Debug, PartialEq)]
enum Model {
    /// A 3×3 matrix on homogeneous coordinates (row-major): similarity, affine and projective.
    Matrix([f64; 9]),
    /// Polynomial terms for x and y, on centred and scaled coordinates.
    Poly { order: u8, from: Norm, to: Norm, cx: Vec<f64>, cy: Vec<f64> },
    /// A thin plate spline on centred and scaled coordinates: a weight per centre, then the
    /// constant, x and y terms, for each output.
    Spline { from: Norm, to: Norm, centres: Vec<(f64, f64)>, wx: Vec<f64>, wy: Vec<f64> },
}

impl Model {
    fn apply(&self, p: (f64, f64)) -> Option<(f64, f64)> {
        let out = match self {
            Model::Matrix(m) => {
                let w = m[6] * p.0 + m[7] * p.1 + m[8];
                if w.abs() < 1e-12 {
                    return None;
                }
                ((m[0] * p.0 + m[1] * p.1 + m[2]) / w, (m[3] * p.0 + m[4] * p.1 + m[5]) / w)
            }
            Model::Poly { order, from, to, cx, cy } => {
                let (u, v) = from.to(p);
                let t = terms(*order, u, v);
                let dot = |c: &[f64]| c.iter().zip(&t).map(|(a, b)| a * b).sum::<f64>();
                to.back((dot(cx), dot(cy)))
            }
            Model::Spline { from, to, centres, wx, wy } => {
                let (u, v) = from.to(p);
                let n = centres.len();
                let (mut x, mut y) = (0.0, 0.0);
                for (i, c) in centres.iter().enumerate() {
                    let k = kernel((u - c.0).powi(2) + (v - c.1).powi(2));
                    x += wx.get(i)? * k;
                    y += wy.get(i)? * k;
                }
                x += wx.get(n)? + wx.get(n + 1)? * u + wx.get(n + 2)? * v;
                y += wy.get(n)? + wy.get(n + 1)? * u + wy.get(n + 2)? * v;
                to.back((x, y))
            }
        };
        (out.0.is_finite() && out.1.is_finite()).then_some(out)
    }
}

/// Centring and scaling of a set of points: (p − mean) / scale, the scale being the root mean
/// square distance from the mean.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Norm {
    mx: f64,
    my: f64,
    s: f64,
}

impl Norm {
    fn of(pts: &[(f64, f64)]) -> Norm {
        let n = pts.len().max(1) as f64;
        let mx = pts.iter().map(|p| p.0).sum::<f64>() / n;
        let my = pts.iter().map(|p| p.1).sum::<f64>() / n;
        let rms = (pts.iter().map(|p| (p.0 - mx).powi(2) + (p.1 - my).powi(2)).sum::<f64>() / n).sqrt();
        Norm { mx, my, s: if rms.is_normal() { rms } else { 1.0 } }
    }

    fn to(&self, p: (f64, f64)) -> (f64, f64) {
        ((p.0 - self.mx) / self.s, (p.1 - self.my) / self.s)
    }

    fn back(&self, p: (f64, f64)) -> (f64, f64) {
        (p.0 * self.s + self.mx, p.1 * self.s + self.my)
    }

    /// The matrix that centres and scales.
    fn matrix(&self) -> [f64; 9] {
        [1.0 / self.s, 0.0, -self.mx / self.s, 0.0, 1.0 / self.s, -self.my / self.s, 0.0, 0.0, 1.0]
    }

    /// The matrix that undoes it.
    fn inverse(&self) -> [f64; 9] {
        [self.s, 0.0, self.mx, 0.0, self.s, self.my, 0.0, 0.0, 1.0]
    }
}

/// A matrix fitted on centred and scaled coordinates, applied to the originals.
fn denormalise(m: &[f64; 9], from: &Norm, to: &Norm) -> [f64; 9] {
    mul3(&to.inverse(), &mul3(m, &from.matrix()))
}

fn normalised(src: &[(f64, f64)], dst: &[(f64, f64)]) -> (Norm, Norm, Vec<(f64, f64)>, Vec<(f64, f64)>) {
    let (from, to) = (Norm::of(src), Norm::of(dst));
    (from, to, src.iter().map(|p| from.to(*p)).collect(), dst.iter().map(|p| to.to(*p)).collect())
}

/// x = a·u − b·v + tx, y = b·u + a·v + ty, by least squares.
fn similarity(src: &[(f64, f64)], dst: &[(f64, f64)]) -> Option<[f64; 9]> {
    let (from, to, s, d) = normalised(src, dst);
    let mut a = Vec::with_capacity(s.len() * 8);
    let mut b = Vec::with_capacity(s.len() * 2);
    for ((u, v), (x, y)) in s.iter().zip(&d) {
        a.extend_from_slice(&[*u, -v, 1.0, 0.0, *v, *u, 0.0, 1.0]);
        b.extend_from_slice(&[*x, *y]);
    }
    let c = lstsq(&a, s.len() * 2, 4, &b, 1)?;
    let [ca, cb, tx, ty] = c.as_slice() else { return None };
    let m = [*ca, -cb, *tx, *cb, *ca, *ty, 0.0, 0.0, 1.0];
    invertible(&m).then(|| denormalise(&m, &from, &to))
}

/// x = a0·u + a1·v + a2, y = b0·u + b1·v + b2, by least squares.
fn affine(src: &[(f64, f64)], dst: &[(f64, f64)]) -> Option<[f64; 9]> {
    let (from, to, s, d) = normalised(src, dst);
    let a: Vec<f64> = s.iter().flat_map(|(u, v)| [*u, *v, 1.0]).collect();
    let b: Vec<f64> = d.iter().flat_map(|(x, y)| [*x, *y]).collect();
    let c = lstsq(&a, s.len(), 3, &b, 2)?;
    let [a0, b0, a1, b1, a2, b2] = c.as_slice() else { return None };
    Some(denormalise(&[*a0, *a1, *a2, *b0, *b1, *b2, 0.0, 0.0, 1.0], &from, &to))
}

/// x = (h0·u + h1·v + h2) / (h6·u + h7·v + 1), and y likewise, by least squares on the
/// equations multiplied out.
fn projective(src: &[(f64, f64)], dst: &[(f64, f64)]) -> Option<[f64; 9]> {
    let (from, to, s, d) = normalised(src, dst);
    let mut a = Vec::with_capacity(s.len() * 16);
    let mut b = Vec::with_capacity(s.len() * 2);
    for ((u, v), (x, y)) in s.iter().zip(&d) {
        a.extend_from_slice(&[*u, *v, 1.0, 0.0, 0.0, 0.0, -u * x, -v * x]);
        a.extend_from_slice(&[0.0, 0.0, 0.0, *u, *v, 1.0, -u * y, -v * y]);
        b.extend_from_slice(&[*x, *y]);
    }
    let h = lstsq(&a, s.len() * 2, 8, &b, 1)?;
    let [h0, h1, h2, h3, h4, h5, h6, h7] = h.as_slice() else { return None };
    // With the last term fixed at 1, three points on a line still give a full-rank system but a
    // matrix that folds the plane flat: refuse it rather than return a fit that only looks exact.
    let m = [*h0, *h1, *h2, *h3, *h4, *h5, *h6, *h7, 1.0];
    invertible(&m).then(|| denormalise(&m, &from, &to))
}

/// Whether a matrix fitted on centred and scaled coordinates is safely invertible: its
/// determinant against the cube of its size (scale-free), far from zero.
fn invertible(m: &[f64; 9]) -> bool {
    let [a, b, c, d, e, f, g, h, i] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    let size = m.iter().map(|v| v * v).sum::<f64>().sqrt();
    size.is_normal() && det.is_finite() && det.abs() / size.powi(3) >= 1e-10
}

/// The terms of a polynomial in u and v, constant first.
fn terms(order: u8, u: f64, v: f64) -> Vec<f64> {
    let mut t = vec![1.0, u, v, u * u, u * v, v * v];
    if order >= 3 {
        t.extend_from_slice(&[u * u * u, u * u * v, u * v * v, v * v * v]);
    }
    t
}

fn poly(order: u8, src: &[(f64, f64)], dst: &[(f64, f64)]) -> Option<Model> {
    let (from, to, s, d) = normalised(src, dst);
    let nt = terms(order, 0.0, 0.0).len();
    let a: Vec<f64> = s.iter().flat_map(|(u, v)| terms(order, *u, *v)).collect();
    let b: Vec<f64> = d.iter().flat_map(|(x, y)| [*x, *y]).collect();
    let c = lstsq(&a, s.len(), nt, &b, 2)?;
    let cx = c.iter().step_by(2).copied().collect();
    let cy = c.iter().skip(1).step_by(2).copied().collect();
    Some(Model::Poly { order, from, to, cx, cy })
}

/// The thin plate spline's radial function of a squared distance: r² ln r.
fn kernel(r2: f64) -> f64 {
    if r2 > 0.0 { 0.5 * r2 * r2.ln() } else { 0.0 }
}

/// A thin plate spline through every point: weights on the radial function at each point, plus
/// an affine part, with the weights summing (and balancing) to zero.
fn spline(src: &[(f64, f64)], dst: &[(f64, f64)]) -> Option<Model> {
    let (from, to, s, d) = normalised(src, dst);
    let n = s.len();
    let size = n + 3;
    let mut a = vec![0.0; size * size];
    let mut b = vec![0.0; size * 2];
    for (i, p) in s.iter().enumerate() {
        for (j, q) in s.iter().enumerate() {
            a[i * size + j] = kernel((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2));
        }
        for (k, t) in [1.0, p.0, p.1].iter().enumerate() {
            a[i * size + n + k] = *t;
            a[(n + k) * size + i] = *t;
        }
    }
    for (i, q) in d.iter().enumerate() {
        b[i * 2] = q.0;
        b[i * 2 + 1] = q.1;
    }
    let (a0, b0) = (a.clone(), b.clone());
    solve(&mut a, size, &mut b, 2)?;
    // Points a hair apart with different map positions solve without a zero pivot but lose the
    // fit: check that the spline still passes through its points.
    for (row, want) in a0.chunks_exact(size).zip(b0.as_chunks::<2>().0) {
        for (k, w) in want.iter().enumerate() {
            let got: f64 = row.iter().enumerate().map(|(j, v)| v * b.get(j * 2 + k).copied().unwrap_or(f64::NAN)).sum();
            if !got.is_finite() || (got - w).abs() > 1e-6 {
                return None;
            }
        }
    }
    let wx = b.iter().step_by(2).copied().collect();
    let wy = b.iter().skip(1).step_by(2).copied().collect();
    Some(Model::Spline { from, to, centres: s, wx, wy })
}

fn mul3(a: &[f64; 9], b: &[f64; 9]) -> [f64; 9] {
    let mut out = [0.0; 9];
    for r in 0..3 {
        for c in 0..3 {
            out[r * 3 + c] = (0..3).map(|k| a[r * 3 + k] * b[k * 3 + c]).sum();
        }
    }
    out
}

fn invert3(m: &[f64; 9]) -> Option<[f64; 9]> {
    let [a, b, c, d, e, f, g, h, i] = *m;
    let co = [e * i - f * h, -(d * i - f * g), d * h - e * g];
    let det = a * co[0] + b * co[1] + c * co[2];
    if !det.is_normal() {
        return None;
    }
    let adj = [co[0], -(b * i - c * h), b * f - c * e, co[1], a * i - c * g, -(a * f - c * d), co[2], -(a * h - b * g), a * e - b * d];
    let out = adj.map(|v| v / det);
    out.iter().all(|v| v.is_finite()).then_some(out)
}

/// Least squares by Householder QR: the `n × k` solution minimising |A·X − B| for an `m × n`
/// matrix A (m ≥ n) and `m × k` right-hand sides, row-major. `None` when A's columns are
/// dependent (points on a line, or on top of each other).
fn lstsq(a: &[f64], m: usize, n: usize, b: &[f64], k: usize) -> Option<Vec<f64>> {
    if m < n || n == 0 || a.len() != m * n || b.len() != m * k {
        return None;
    }
    let (mut a, mut b) = (a.to_vec(), b.to_vec());
    let mut diag = vec![0.0; n];
    for j in 0..n {
        let norm = (j..m).map(|i| a[i * n + j].powi(2)).sum::<f64>().sqrt();
        if !norm.is_normal() {
            return None;
        }
        let alpha = if a[j * n + j] > 0.0 { -norm } else { norm };
        a[j * n + j] -= alpha;
        let vv = (j..m).map(|i| a[i * n + j].powi(2)).sum::<f64>();
        if !vv.is_normal() {
            return None;
        }
        for c in j + 1..n {
            let f = 2.0 * (j..m).map(|i| a[i * n + j] * a[i * n + c]).sum::<f64>() / vv;
            for i in j..m {
                a[i * n + c] -= f * a[i * n + j];
            }
        }
        for c in 0..k {
            let f = 2.0 * (j..m).map(|i| a[i * n + j] * b[i * k + c]).sum::<f64>() / vv;
            for i in j..m {
                b[i * k + c] -= f * a[i * n + j];
            }
        }
        diag[j] = alpha;
    }
    let big = diag.iter().fold(0.0f64, |acc, d| acc.max(d.abs()));
    if diag.iter().any(|d| d.abs() <= big * 1e-10) {
        return None;
    }
    let mut x = vec![0.0; n * k];
    for c in 0..k {
        for i in (0..n).rev() {
            let s = b[i * k + c] - (i + 1..n).map(|j| a[i * n + j] * x[j * k + c]).sum::<f64>();
            x[i * k + c] = s / diag[i];
        }
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

/// Solves the square system A·X = B in place (B becomes X) by Gaussian elimination with partial
/// pivoting. `None` when A is singular.
fn solve(a: &mut [f64], n: usize, b: &mut [f64], k: usize) -> Option<()> {
    if n == 0 || a.len() != n * n || b.len() != n * k {
        return None;
    }
    let scale = a.iter().fold(0.0f64, |acc, v| acc.max(v.abs()));
    if !scale.is_normal() {
        return None;
    }
    for col in 0..n {
        let piv = (col..n).max_by(|&i, &j| a[i * n + col].abs().total_cmp(&a[j * n + col].abs()))?;
        if a[piv * n + col].abs() <= scale * 1e-13 {
            return None;
        }
        if piv != col {
            for c in 0..n {
                a.swap(piv * n + c, col * n + c);
            }
            for c in 0..k {
                b.swap(piv * k + c, col * k + c);
            }
        }
        let d = a[col * n + col];
        for r in col + 1..n {
            let f = a[r * n + col] / d;
            if f == 0.0 {
                continue;
            }
            for c in col..n {
                a[r * n + c] -= f * a[col * n + c];
            }
            for c in 0..k {
                b[r * k + c] -= f * b[col * k + c];
            }
        }
    }
    for c in 0..k {
        for r in (0..n).rev() {
            let s = b[r * k + c] - (r + 1..n).map(|j| a[r * n + j] * b[j * k + c]).sum::<f64>();
            b[r * k + c] = s / a[r * n + r];
        }
    }
    b.iter().all(|v| v.is_finite()).then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 6 × 5 grid of pixels over a 30,000 × 20,000 scan, placed by `place`.
    fn grid(place: impl Fn(f64, f64) -> (f64, f64)) -> Vec<Gcp> {
        let mut out = Vec::new();
        for i in 0..6 {
            for j in 0..5 {
                let (col, row) = (500.0 + 5800.0 * f64::from(i), 400.0 + 4800.0 * f64::from(j));
                let (x, y) = place(col, row);
                out.push(Gcp { col, row, x, y, on: true });
            }
        }
        out
    }

    /// Leave-one-out refits made on several threads give the same report, bit for bit, as on one
    /// (#1252), for every model, with points switched off among them.
    #[test]
    fn assess_on_several_threads_is_assess_on_one() {
        let points: Vec<Gcp> = (0..80)
            .map(|i| {
                let (col, row) = (f64::from(i % 10) * 3000.0 + 200.0, f64::from(i / 10) * 2400.0 + 150.0);
                let (x, y) = (2_600_000.0 + col * 0.5 + wobble(i as usize) * 3.0, 1_200_000.0 - row * 0.5 + wobble(i as usize + 99) * 3.0);
                Gcp { col, row, x, y, on: i % 5 != 2 }
            })
            .collect();
        for kind in [Kind::Similarity, Kind::Affine, Kind::Projective, Kind::Polynomial2, Kind::Polynomial3, Kind::Spline] {
            let one = assess_on(Some(1), kind, &points, true).unwrap();
            assert!(one.leave_one_out.as_ref().is_some_and(|e| e.iter().any(Option::is_some)), "{kind:?}");
            for threads in [2, 7, 16] {
                assert_eq!(assess_on(Some(threads), kind, &points, true).unwrap(), one, "{kind:?}, {threads} threads");
            }
        }
    }

    /// A small, repeatable wobble in [−1, 1].
    fn wobble(i: usize) -> f64 {
        ((i as f64 * 12.9898).sin() * 43758.5453).fract() * 2.0 - 1.0
    }

    fn close(a: (f64, f64), b: (f64, f64), tol: f64) -> bool {
        (a.0 - b.0).abs() <= tol && (a.1 - b.1).abs() <= tol
    }

    /// Every transform recovers points made by itself: the map residuals vanish and a position
    /// between the points goes there and back. A polynomial's inverse is a polynomial fitted the
    /// other way, not an exact inverse (as in GDAL), so its pixel residuals are small, not zero.
    #[test]
    fn exact_fits() {
        // State plane feet, a scan turned 20° and scaled: the size that strains conditioning.
        let (s, t) = (2.5, 20f64.to_radians());
        let sim = |c: f64, r: f64| (2_271_490.0 + s * (c * t.cos() + r * t.sin()), 720_873.0 + s * (c * t.sin() - r * t.cos()));
        let aff = |c: f64, r: f64| (500_000.0 + 2.0 * c + 0.3 * r, 4_000_000.0 + 0.1 * c - 1.7 * r);
        let proj = |c: f64, r: f64| {
            let w = 1.0 + 2e-6 * c + 3e-6 * r;
            ((10.0 + 1.5 * c + 0.2 * r) / w, (20.0 - 0.1 * c - 1.4 * r) / w)
        };
        // Bends of up to about 90 pixels over the scan.
        let p2 = |c: f64, r: f64| (c + 1e-7 * c * c - 2e-8 * c * r, -r + 3e-8 * r * r + 1e-8 * c * c);
        let p3 = |c: f64, r: f64| (c + 1e-12 * c * c * c, -r - 2e-12 * r * r * r + 1e-8 * c * r);
        for (kind, place, inverse_tol) in [
            (Kind::Similarity, &sim as &dyn Fn(f64, f64) -> (f64, f64), 1e-6),
            (Kind::Affine, &aff, 1e-6),
            (Kind::Projective, &proj, 1e-6),
            (Kind::Polynomial2, &p2, 1.0),
            (Kind::Polynomial3, &p3, 1.0),
        ] {
            let pts = grid(place);
            let r = assess(kind, &pts, true).unwrap();
            assert!(r.rmse.1 < 1e-6, "{kind:?} {:?}", r.rmse);
            assert!(r.rmse.0 < inverse_tol, "{kind:?} {:?}", r.rmse);
            let mid = place(12_345.0, 6_789.0);
            assert!(close(r.fit.to_map(12_345.0, 6_789.0).unwrap(), mid, 1e-4), "{kind:?}");
            assert!(close(r.fit.to_pixel(mid.0, mid.1).unwrap(), (12_345.0, 6_789.0), inverse_tol.max(1e-4)), "{kind:?}");
            let loo = r.leave_one_out_rmse.unwrap();
            assert!(loo.1 < 1e-4 && loo.0 < inverse_tol.max(1e-4), "{kind:?} predicts the points it wasn't given: {loo:?}");
        }
        // The spline passes through every point, whatever they are.
        let pts = grid(|c, r| (c + 30.0 * wobble(c as usize), -r + 30.0 * wobble(r as usize)));
        let r = assess(Kind::Spline, &pts, true).unwrap();
        assert!(r.rmse.0 < 1e-6 && r.rmse.1 < 1e-6, "{:?}", r.rmse);
        assert!(r.leave_one_out_rmse.unwrap().0 > 1.0, "but can't predict a wobble it wasn't given");
    }

    #[test]
    fn affine_geotransform() {
        let pts = grid(|c, r| (500_000.0 + 2.0 * c + 0.3 * r, 4_000_000.0 + 0.1 * c - 1.7 * r));
        let g = fit(Kind::Affine, &pts).unwrap().affine().unwrap();
        for (a, b) in g.iter().zip([500_000.0, 2.0, 0.3, 4_000_000.0, 0.1, -1.7]) {
            assert!((a - b).abs() < 1e-6, "{g:?}");
        }
        assert!(fit(Kind::Polynomial2, &pts).unwrap().affine().is_none());
    }

    /// A point pulled off the fit shows in its residual, both ways; switched off, it no longer
    /// pulls the fit but keeps a residual.
    #[test]
    fn residuals_and_switching_off() {
        let mut pts = grid(|c, r| (c, -r));
        pts[7].x += 10.0;
        let r = assess(Kind::Affine, &pts, false).unwrap();
        let worst = r.residuals.iter().enumerate().max_by(|a, b| a.1.unwrap().map.0.abs().total_cmp(&b.1.unwrap().map.0.abs())).unwrap();
        assert_eq!(worst.0, 7);
        assert!(r.residuals[7].unwrap().map.0 < 0.0, "the fit falls short of the moved point");
        pts[7].on = false;
        let r = assess(Kind::Affine, &pts, false).unwrap();
        assert!(r.rmse.1 < 1e-6);
        assert!(close(r.residuals[7].unwrap().map, (-10.0, 0.0), 1e-6));
        assert!(close(r.residuals[7].unwrap().pixels, (10.0, 0.0), 1e-6), "a map shift of +10 is 10 pixels right");
        assert_eq!(r.used, 29);
    }

    /// QGIS's mean error divides by the points beyond the minimum; at the minimum it is zero.
    #[test]
    fn mean_error() {
        let mut pts = grid(|c, r| (c, -r));
        for (i, p) in pts.iter_mut().enumerate() {
            p.x += wobble(i);
        }
        let r = assess(Kind::Affine, &pts, false).unwrap();
        let ratio = r.mean_error.1 / r.rmse.1;
        assert!((ratio - (30.0f64 / 27.0).sqrt()).abs() < 1e-9, "{ratio}");
        // Three points off a line (the grid's first three share a column).
        let mut three: Vec<Gcp> = pts.iter().take(3).copied().collect();
        three[2].col += 1000.0;
        let r = assess(Kind::Affine, &three, true).unwrap();
        assert_eq!(r.mean_error, (0.0, 0.0));
        assert!(r.leave_one_out.is_none() && r.leave_one_out_note.is_some());
    }

    /// On points from an affine map with a little noise, the extra bending of polynomials and
    /// the spline lowers the error at the points but not at points left out, so affine is
    /// recommended; a similarity can't take the shear.
    #[test]
    fn recommends_the_simplest_that_holds() {
        let mut pts = grid(|c, r| (2.0 * c + 0.3 * r, 0.1 * c - 1.7 * r));
        for (i, p) in pts.iter_mut().enumerate() {
            p.x += 3.0 * wobble(i);
            p.y += 3.0 * wobble(i + 100);
        }
        let (reports, best) = compare(&pts);
        assert_eq!(best, Some(Kind::Affine));
        let rmse = |k: Kind| reports.iter().find(|(kk, _)| *kk == k).unwrap().1.as_ref().unwrap().rmse.1;
        assert!(rmse(Kind::Polynomial3) < rmse(Kind::Affine), "more bending fits the points better");
        assert!(rmse(Kind::Similarity) > 10.0 * rmse(Kind::Affine));
    }

    #[test]
    fn degenerate_points_are_refused() {
        let line: Vec<Gcp> = (0..5).map(|i| Gcp { col: f64::from(i), row: f64::from(i), x: f64::from(i), y: 0.0, on: true }).collect();
        assert!(fit(Kind::Affine, &line).is_err());
        assert!(fit(Kind::Spline, &line).is_err());
        let mut twice = grid(|c, r| (c, -r));
        twice[1] = twice[0];
        twice[1].x += 5.0;
        assert!(fit(Kind::Spline, &twice).is_err(), "one pixel, two places");
        assert!(fit(Kind::Polynomial3, &twice[..9]).is_err_and(|e| e.contains("at least 10")));
        let mut bad = grid(|c, r| (c, -r));
        bad[0].x = f64::NAN;
        assert!(fit(Kind::Affine, &bad).is_err());
        assert!(fit(Kind::Similarity, &[]).is_err());
        let many = vec![Gcp { col: 0.0, row: 0.0, x: 0.0, y: 0.0, on: true }; MAX_POINTS + 1];
        assert!(fit(Kind::Similarity, &many).is_err_and(|e| e.contains("at most")));
    }

    /// Three of four points on a line leave the projective system full rank, but the fitted
    /// matrix folds the plane flat: it must be refused, not returned with zero map residuals.
    #[test]
    fn projective_refuses_points_on_a_line() {
        let place = |c: f64, r: f64| (2_271_490.0 + 2.5 * c, 720_873.0 - 2.5 * r);
        let pts: Vec<Gcp> = [(0.0, 0.0), (5000.0, 0.0), (10_000.0, 0.0), (5000.0, 8000.0)]
            .iter()
            .map(|&(col, row)| {
                let (x, y) = place(col, row);
                Gcp { col, row, x, y, on: true }
            })
            .collect();
        assert!(fit(Kind::Projective, &pts).is_err());
        let mut five = pts.clone();
        five.push({
            let (x, y) = place(2000.0, 3000.0);
            Gcp { col: 2000.0, row: 3000.0, x, y, on: true }
        });
        assert!(fit(Kind::Projective, &five).is_ok(), "a fifth point off the line fixes it");
    }

    /// Four points along one image row and one off it: leaving out the one off the row leaves
    /// the affine fit undetermined, so its leave-one-out error is withheld, not averaged over the
    /// four easy points, and compare can't recommend it.
    #[test]
    fn leave_one_out_needs_every_point() {
        let mut pts: Vec<Gcp> = (0..4).map(|i| f64::from(i) * 1000.0).map(|col| Gcp { col, row: 0.0, x: col, y: 0.0, on: true }).collect();
        pts.push(Gcp { col: 1500.0, row: 2000.0, x: 1500.0, y: -2000.0, on: true });
        for (i, p) in pts.iter_mut().enumerate() {
            p.x += wobble(i);
        }
        let r = assess(Kind::Affine, &pts, true).unwrap();
        assert!(r.leave_one_out_rmse.is_none(), "{:?}", r.leave_one_out_rmse);
        assert!(r.leave_one_out_note.as_deref().is_some_and(|n| n.contains("point 5")), "{:?}", r.leave_one_out_note);
        let (reports, best) = compare(&pts);
        assert_ne!(best, Some(Kind::Affine));
        assert!(reports.iter().any(|(k, r)| *k == Kind::Polynomial3 && r.is_err()), "a transform the points can't fix is reported");
    }

    /// Two points a hair apart with different map positions: the spline would solve but miss its
    /// own points, so it is refused.
    #[test]
    fn spline_refuses_a_lost_solve() {
        let mut pts = grid(|c, r| (c, -r));
        pts[1] = Gcp { col: pts[0].col + 1e-5, row: pts[0].row, x: pts[0].x + 5.0, y: pts[0].y, on: true };
        assert!(fit(Kind::Spline, &pts).is_err());
    }
}
