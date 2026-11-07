//! Editing geometry: merging features, and topology checks (overlaps, gaps, self-intersections,
//! dangles, coverage). Computed in the analysis plane ([`rustgis_geom::lonlat_to_plane`]).

use geo::{Area, BooleanOps, BoundingRect, Contains, Intersects, Orient, orient::Direction};
use rustgis_data::{Dataset, Geometry};
use rustgis_geom::{Coord, MAX_LAT, WGS84_POLAR_RADIUS_M, lonlat_to_plane, plane_to_lonlat};

use crate::convert::{from_multipolygon, to_multipolygon};

/// A topology problem: the rows involved and where it is (longitude/latitude).
#[derive(Clone, Debug, PartialEq)]
pub struct TopologyError {
    pub rows: Vec<usize>,
    pub at: Coord,
}

/// A check's errors, whether it finished (a check stops at an error or work limit) with every part
/// measured as closely as it's meant to be, and how many polygons it had to leave out (see the
/// check for why).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Checked {
    pub errors: Vec<TopologyError>,
    pub complete: bool,
    pub skipped: usize,
}

impl Checked {
    fn done(errors: Vec<TopologyError>) -> Checked {
        Checked { errors, complete: true, skipped: 0 }
    }
    fn partial(errors: Vec<TopologyError>) -> Checked {
        Checked { errors, complete: false, skipped: 0 }
    }
}

/// Most errors reported per check.
const MAX_ERRORS: usize = 10_000;
/// Most pair tests one check spends, so a huge layer can't stall the app.
const MAX_WORK: usize = 50_000_000;

/// One geometry from several: polygons are unioned, lines and points gathered.
pub fn merge(geoms: &[Geometry]) -> Option<Geometry> {
    match geoms.first()? {
        Geometry::Polygon(_) => {
            let polys: Vec<geo::MultiPolygon<f64>> =
                geoms.iter().filter_map(to_multipolygon).map(|m| m.orient(Direction::Default)).collect();
            let all: Vec<geo::Polygon<f64>> = polys.into_iter().flat_map(|m| m.0).collect();
            from_multipolygon(&geo::unary_union(all.iter()))
        }
        Geometry::Line(_) => {
            let parts: Vec<Vec<Coord>> =
                geoms.iter().filter_map(|g| if let Geometry::Line(p) = g { Some(p.clone()) } else { None }).flatten().collect();
            (!parts.is_empty()).then_some(Geometry::Line(parts))
        }
        Geometry::Point(_) | Geometry::MultiPoint(_) => {
            let pts: Vec<Coord> = geoms
                .iter()
                .flat_map(|g| match g {
                    Geometry::Point(c) => vec![*c],
                    Geometry::MultiPoint(v) => v.clone(),
                    _ => Vec::new(),
                })
                .collect();
            (!pts.is_empty()).then_some(Geometry::MultiPoint(pts))
        }
    }
}

/// The parts of a multipart geometry as separate geometries.
pub fn explode(g: &Geometry) -> Vec<Geometry> {
    match g {
        Geometry::MultiPoint(v) => v.iter().map(|c| Geometry::Point(*c)).collect(),
        Geometry::Line(parts) => parts.iter().map(|p| Geometry::Line(vec![p.clone()])).collect(),
        Geometry::Polygon(polys) => polys.iter().map(|p| Geometry::Polygon(vec![p.clone()])).collect(),
        Geometry::Point(_) => vec![g.clone()],
    }
}

fn polygons(ds: &Dataset, rows: &[usize]) -> Vec<(usize, geo::MultiPolygon<f64>, geo::Rect<f64>)> {
    rows.iter()
        .filter_map(|r| {
            let mp = ds.features.get(*r)?.geometry.as_ref().and_then(to_multipolygon)?.orient(Direction::Default);
            let bb = mp.bounding_rect()?;
            Some((*r, mp, bb))
        })
        .collect()
}

fn centre(mp: &geo::MultiPolygon<f64>) -> Coord {
    use geo::InteriorPoint;
    mp.interior_point().map(|p| plane_to_lonlat(Coord::new(p.x(), p.y()))).unwrap_or_default()
}

fn rects_touch(a: &geo::Rect<f64>, b: &geo::Rect<f64>) -> bool {
    a.min().x <= b.max().x && b.min().x <= a.max().x && a.min().y <= b.max().y && b.min().y <= a.max().y
}

/// Pairs of polygons that overlap (share area, not just an edge).
pub fn overlaps(ds: &Dataset, rows: &[usize]) -> Checked {
    let mut p = polygons(ds, rows);
    // Sweep from west to east: only polygons whose boxes overlap in x are compared.
    p.sort_by(|a, b| a.2.min().x.partial_cmp(&b.2.min().x).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = Vec::new();
    let mut work = 0usize;
    crate::control::begin(p.len());
    for (i, (ra, a, ba)) in p.iter().enumerate() {
        if crate::control::cancelled() {
            return Checked::partial(out);
        }
        crate::control::reached(i);
        for (rb, b, bb) in p.iter().skip(i + 1) {
            if bb.min().x > ba.max().x {
                break;
            }
            work += 1;
            if work > MAX_WORK {
                return Checked::partial(out);
            }
            if !rects_touch(ba, bb) || !a.intersects(b) {
                continue;
            }
            let inter = a.intersection(b);
            // Shared edges leave slivers of floating-point area; a square metre is a real overlap.
            if inter.unsigned_area() > 1.0 {
                out.push(TopologyError { rows: vec![*ra, *rb], at: centre(&inter) });
                if out.len() >= MAX_ERRORS {
                    return Checked::partial(out);
                }
            }
        }
    }
    Checked::done(out)
}

/// Holes in the union of a layer's polygons that no polygon covers (the outer boundary isn't a
/// gap).
pub fn gaps(ds: &Dataset, rows: &[usize]) -> Checked {
    let all: Vec<geo::Polygon<f64>> = polygons(ds, rows).into_iter().flat_map(|(_, m, _)| m.0).collect();
    let u = geo::unary_union(all.iter());
    let mut out = Vec::new();
    for poly in &u.0 {
        for hole in poly.interiors() {
            let mp = geo::MultiPolygon::new(vec![geo::Polygon::new(hole.clone(), vec![])]);
            if mp.unsigned_area() > 1.0 {
                out.push(TopologyError { rows: Vec::new(), at: centre(&mp) });
                if out.len() >= MAX_ERRORS {
                    return Checked::partial(out);
                }
            }
        }
    }
    Checked::done(out)
}

fn segments_cross(a: (Coord, Coord), b: (Coord, Coord)) -> Option<Coord> {
    let d = (a.1.x - a.0.x) * (b.1.y - b.0.y) - (a.1.y - a.0.y) * (b.1.x - b.0.x);
    if d.abs() < 1e-12 {
        return None;
    }
    let t = ((b.0.x - a.0.x) * (b.1.y - b.0.y) - (b.0.y - a.0.y) * (b.1.x - b.0.x)) / d;
    let u = ((b.0.x - a.0.x) * (a.1.y - a.0.y) - (b.0.y - a.0.y) * (a.1.x - a.0.x)) / d;
    // Strictly inside both segments: touching at shared vertices isn't crossing.
    let e = 1e-9;
    (t > e && t < 1.0 - e && u > e && u < 1.0 - e).then(|| Coord::new(a.0.x + t * (a.1.x - a.0.x), a.0.y + t * (a.1.y - a.0.y)))
}

/// Where a path crosses itself (rings and lines; ring closures are allowed). Segments are swept
/// west to east, so only those overlapping in x are compared. A path too complex to finish
/// within the work budget is reported at its start, to be looked at, rather than passed.
fn self_crossing(path: &[Coord], closed: bool) -> Option<Coord> {
    let segs: Vec<(Coord, Coord)> = path.windows(2).filter_map(|w| Some((*w.first()?, *w.get(1)?))).collect();
    let n = segs.len();
    let minx = |s: &(Coord, Coord)| s.0.x.min(s.1.x);
    let maxx = |s: &(Coord, Coord)| s.0.x.max(s.1.x);
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| {
        let (sa, sb) = (segs.get(*a).map_or(0.0, minx), segs.get(*b).map_or(0.0, minx));
        sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut work = 0usize;
    for (k, &i) in order.iter().enumerate() {
        let Some(a) = segs.get(i) else { continue };
        for &j in order.iter().skip(k + 1) {
            let Some(b) = segs.get(j) else { continue };
            if minx(b) > maxx(a) {
                break;
            }
            work += 1;
            if work > MAX_WORK {
                return path.first().copied();
            }
            let (lo, hi) = (i.min(j), i.max(j));
            // Neighbouring segments share a vertex, as do a ring's first and last.
            if hi == lo + 1 || (closed && lo == 0 && hi == n - 1) {
                continue;
            }
            if let Some(c) = segments_cross(*a, *b) {
                return Some(c);
            }
        }
    }
    None
}

/// Lines and polygon rings that cross themselves.
pub fn self_intersections(ds: &Dataset, rows: &[usize]) -> Checked {
    let mut out = Vec::new();
    crate::control::begin(rows.len());
    for (i, r) in rows.iter().enumerate() {
        if crate::control::cancelled() {
            return Checked::partial(out);
        }
        crate::control::reached(i);
        let Some(g) = ds.features.get(*r).and_then(|f| f.geometry.as_ref()) else { continue };
        let paths: Vec<(Vec<Coord>, bool)> = match g {
            Geometry::Line(parts) => parts.iter().map(|p| (p.clone(), false)).collect(),
            Geometry::Polygon(polys) => polys.iter().flatten().map(|r| (r.clone(), true)).collect(),
            _ => Vec::new(),
        };
        for (p, closed) in paths {
            let m: Vec<Coord> = p.iter().map(|c| lonlat_to_plane(*c)).collect();
            if let Some(c) = self_crossing(&m, closed) {
                out.push(TopologyError { rows: vec![*r], at: plane_to_lonlat(c) });
                break;
            }
        }
        if out.len() >= MAX_ERRORS {
            return Checked::partial(out);
        }
    }
    Checked::done(out)
}

/// Line ends that touch no other line (within `tol_m` metres). Line parts are indexed on a grid,
/// so each end is measured only against parts near it; an end beyond 85.05° in a frame about its
/// pole (`PolarEnds`).
pub fn dangles(ds: &Dataset, rows: &[usize], tol_m: f64) -> Checked {
    let lines: Vec<(usize, Vec<Vec<Coord>>)> = rows
        .iter()
        .filter_map(|r| match ds.features.get(*r)?.geometry.as_ref()? {
            Geometry::Line(parts) => Some((*r, parts.iter().map(|p| p.iter().map(|c| lonlat_to_plane(*c)).collect()).collect())),
            _ => None,
        })
        .collect();
    // Within ±85.05°, tolerance in the plane's meters grows with latitude; index with the largest
    // it can be there.
    let max_tol = tol_m / MAX_LAT.to_radians().cos();
    let mut ext = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for c in lines.iter().flat_map(|(_, ps)| ps.iter().flatten()) {
        ext = (ext.0.min(c.x), ext.1.min(c.y), ext.2.max(c.x), ext.3.max(c.y));
    }
    let span = (ext.2 - ext.0).max(ext.3 - ext.1).max(1.0);
    let cells = Cells::new((ext.0, ext.1), (span / 512.0).max(max_tol.min(span)).max(1e-6), (ext.2, ext.3));
    // Each part's segments go into the cells their (widened) boxes cover, within the lines' extent:
    // only those are ever looked in, as every end lies in it.
    let mut grid: std::collections::HashMap<(i64, i64), Vec<(usize, usize)>> = std::collections::HashMap::new();
    let mut work = 0usize;
    crate::control::begin(lines.len());
    for (li, (_, parts)) in lines.iter().enumerate() {
        if crate::control::cancelled() {
            return Checked::partial(Vec::new());
        }
        crate::control::reached(li);
        for (pi, part) in parts.iter().enumerate() {
            for w in part.windows(2) {
                let (Some(a), Some(b)) = (w.first(), w.get(1)) else { continue };
                let (x0, y0) = cells.key(a.x.min(b.x) - max_tol, a.y.min(b.y) - max_tol);
                let (x1, y1) = cells.key(a.x.max(b.x) + max_tol, a.y.max(b.y) + max_tol);
                for gx in x0.max(0)..=x1.min(cells.last.0) {
                    for gy in y0.max(0)..=y1.min(cells.last.1) {
                        work += 1;
                        if work > MAX_WORK {
                            return Checked::partial(Vec::new());
                        }
                        let v = grid.entry((gx, gy)).or_default();
                        if v.last() != Some(&(li, pi)) {
                            v.push((li, pi));
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    crate::control::begin(lines.len());
    // Made at the first end beyond 85.05°.
    let mut polar: Option<PolarEnds> = None;
    for (li, (r, parts)) in lines.iter().enumerate() {
        if crate::control::cancelled() {
            return Checked::partial(out);
        }
        crate::control::reached(li);
        for (pi, part) in parts.iter().enumerate() {
            // A closed part has no ends.
            if part.len() < 2 || part.first() == part.last() {
                continue;
            }
            for (end, at_start) in [(part.first(), true), (part.last(), false)] {
                let Some(end) = end else { continue };
                let at = plane_to_lonlat(*end);
                let lat = at.y;
                let touches = if lat.abs() > MAX_LAT {
                    let polar = polar.get_or_insert_with(|| PolarEnds::new(&lines, &grid, cells));
                    match polar.meets((li, pi, at_start), (*end, at), tol_m, &mut work) {
                        Ok(touches) => touches,
                        Err(WorkLimit) => return Checked::partial(out),
                    }
                } else {
                    let tol = tol_m / lat.to_radians().cos().max(0.01);
                    let near = grid.get(&cells.key(end.x, end.y)).map(Vec::as_slice).unwrap_or(&[]);
                    // The end's own part counts too, past the stretch next to the end (a line
                    // curling back onto itself, like a 6, doesn't dangle there).
                    let own_rest = rest_of_part(part, at_start, 2.0 * tol);
                    !own_rest.is_empty() && rustgis_data::geometry::seg_dist2(&own_rest, *end) <= tol * tol
                        || near.iter().any(|&(oli, opi)| {
                            work += 1;
                            (oli != li || opi != pi)
                                && lines
                                    .get(oli)
                                    .and_then(|(_, ps)| ps.get(opi))
                                    .is_some_and(|op| op.len() >= 2 && rustgis_data::geometry::seg_dist2(op, *end) <= tol * tol)
                        })
                };
                if work > MAX_WORK {
                    return Checked::partial(out);
                }
                if !touches {
                    out.push(TopologyError { rows: vec![*r], at });
                }
            }
        }
        if out.len() >= MAX_ERRORS {
            return Checked::partial(out);
        }
    }
    // Every end was checked, but one measured coarsely may have been found wrongly.
    if polar.is_some_and(|p| p.coarse) { Checked::partial(out) } else { Checked::done(out) }
}

/// The most a piece of an edge may stray from the curve it stands for in [`edge_from`], as a share
/// of the tolerance; at least [`POLAR_STRAY_MIN_M`].
const POLAR_STRAY_SHARE: f64 = 1e-3;
/// The least that allowance, in meters.
const POLAR_STRAY_MIN_M: f64 = 1e-3;
/// The most pieces [`edge_from`] cuts one stretch into, and [`walk_edge`] an edge. Only one winding
/// round a pole many times needs more (a hostile edge thousands of degrees long): it's measured in
/// this many, which stray farther, and the check says it's incomplete.
const POLAR_PIECES_MAX: f64 = 65_536.0;
/// The longest piece, in radians of longitude, that [`walk_edge`] measures an edge in: its chords
/// fall short of the edge by under 0.006%.
const POLAR_LENGTH_STEP: f64 = 0.0175;
/// The most times [`walk_edge`] halves the piece it passes its distance in, to place it there (a
/// 2⁻⁶⁰th of the piece, below an f64's precision); it stops sooner, once the place is within
/// [`POLAR_STRAY_MIN_M`] past it, about 25 halvings for a piece of 1° at 2° from the pole.
const PLACE_HALVINGS: usize = 60;
/// How many cheap reads count as one unit of work: an edge's box found out of an end's reach, which
/// at worst costs about a quarter of a piece measured in [`edge_from`]. Reads add up across ends.
const READS_A_WORK: usize = 4;

/// The dangle check passed [`MAX_WORK`]: nothing more is compared.
struct WorkLimit;

/// Adds `n` to `work`, or says it passed [`MAX_WORK`].
fn charge(work: &mut usize, n: usize) -> Result<(), WorkLimit> {
    *work = work.saturating_add(n);
    if *work > MAX_WORK { Err(WorkLimit) } else { Ok(()) }
}

/// Counts a cheap read ([`READS_A_WORK`] to a unit of work).
fn read(reads: &mut usize, work: &mut usize) -> Result<(), WorkLimit> {
    *reads = reads.wrapping_add(1);
    if reads.is_multiple_of(READS_A_WORK) { charge(work, 1) } else { Ok(()) }
}

/// The dangle check's grid: which cell a point of the analysis plane falls in.
#[derive(Clone, Copy)]
struct Cells {
    /// The lowest corner of the lines' extent, a cell's width and height, and the last cell.
    origin: (f64, f64),
    size: f64,
    last: (i64, i64),
}

impl Cells {
    /// Cells of `size` from `origin` (the lowest corner) to `far` (the highest).
    fn new(origin: (f64, f64), size: f64, far: (f64, f64)) -> Self {
        let mut cells = Self { origin, size, last: (0, 0) };
        cells.last = cells.key(far.0, far.1);
        cells
    }

    fn key(&self, x: f64, y: f64) -> (i64, i64) {
        (((x - self.origin.0) / self.size).floor() as i64, ((y - self.origin.1) / self.size).floor() as i64)
    }
}

/// Where `c` (longitude/latitude) lies in the azimuthal equidistant projection centered on a pole
/// (the North Pole when `north`): its distance from the pole, at WGS 84's curvature there, in the
/// direction of its longitude. Near the pole that's the ground, within 0.01% along meridians and
/// 0.13% across them out to 85°, right up to the pole and across ±180°. (Not geom's exact `Aeqd`,
/// which solves a geodesic for each point: an edge can be measured in thousands of pieces.)
fn about_pole(c: Coord, north: bool) -> Coord {
    let rho = WGS84_POLAR_RADIUS_M * (90.0 - if north { c.y } else { -c.y }).to_radians();
    let (sin, cos) = c.x.to_radians().sin_cos();
    Coord::new(rho * sin, rho * cos)
}

/// What an end beyond 85.05° can reach in the analysis plane: every point within the tolerance of
/// it in the frame about its pole lies within `wy` of it north–south and, unless the tolerance
/// reaches the pole, within `wx` east–west, also across ±180° and any whole turns in stored longitudes.
struct Reach {
    end: Coord,
    wx: f64,
    wy: f64,
}

impl Reach {
    /// The reach of `end`, at latitude `lat`, for a tolerance of `tol_m`: within `tol_m` of the end
    /// in the frame, a point's distance from the pole is within `tol_m` of the end's, which the
    /// plane stretches by at most sec(85.05°) north–south, and its longitude within asin(tol/ρ).
    fn new(end: Coord, lat: f64, tol_m: f64) -> Self {
        let rho = WGS84_POLAR_RADIUS_M * (90.0 - lat.abs()).to_radians();
        let wy = tol_m / MAX_LAT.to_radians().cos() * 1.001;
        let wx = if tol_m < rho { rustgis_geom::MERCATOR_R * (tol_m / rho).asin() * 1.001 } else { f64::INFINITY };
        Self { end, wx, wy }
    }

    /// The window's copies nearest `center`, and a turn either way while it is narrower than
    /// the world. Stored parts can be any number of whole turns from the end.
    fn shifts(&self, center: f64) -> impl Iterator<Item = f64> {
        let round = std::f64::consts::TAU * rustgis_geom::MERCATOR_R;
        let shift = round * ((center - self.end.x) / round).round();
        [shift, shift + round, shift - round].into_iter().take(if self.wx.is_finite() { 3 } else { 1 })
    }

    /// Whether the box of the edge `a`–`b` meets the window or one of its copies: a cheap test
    /// before [`Reach::spans`].
    fn boxed(&self, a: &Coord, b: &Coord) -> bool {
        a.y.min(b.y) <= self.end.y + self.wy
            && a.y.max(b.y) >= self.end.y - self.wy
            && (self.wx.is_infinite()
                || (b.x - a.x).abs() >= std::f64::consts::TAU * rustgis_geom::MERCATOR_R
                || self
                    .shifts(a.x / 2.0 + b.x / 2.0)
                    .any(|s| a.x.min(b.x) <= self.end.x + s + self.wx && a.x.max(b.x) >= self.end.x + s - self.wx))
    }

    /// The stretches of the edge `a`–`b` (straight in the plane) within reach, one for the window
    /// and each of its copies, as ranges of the edge's parameter (0 at `a`, 1 at `b`); none when a
    /// coordinate is missing or infinite.
    fn spans(&self, a: &Coord, b: &Coord) -> [Option<(f64, f64)>; 3] {
        // Where p + (q − p)·t lies within [lo, hi], for t from 0 to 1.
        let clip = |lo: f64, hi: f64, p: f64, q: f64| {
            if !(p.is_finite() && q.is_finite()) {
                return None;
            }
            let d = q - p;
            if d.abs() > 0.0 {
                let (t0, t1) = ((lo - p) / d, (hi - p) / d);
                let (t0, t1) = (t0.min(t1).max(0.0), t0.max(t1).min(1.0));
                (t0 <= t1).then_some((t0, t1))
            } else {
                (lo <= p && p <= hi).then_some((0.0, 1.0))
            }
        };
        let mut spans = [None; 3];
        let Some((y0, y1)) = clip(self.end.y - self.wy, self.end.y + self.wy, a.y, b.y) else { return spans };
        // More than two turns can meet more than three copies. Measure the whole latitude
        // stretch instead; edge_from's piece and work limits still bound it and report coarseness.
        if self.wx.is_infinite() || (b.x - a.x).abs() > 2.0 * std::f64::consts::TAU * rustgis_geom::MERCATOR_R {
            spans[0] = (a.x.is_finite() && b.x.is_finite()).then_some((y0, y1));
            return spans;
        }
        for (span, s) in spans.iter_mut().zip(self.shifts(a.x / 2.0 + b.x / 2.0)) {
            *span = clip(self.end.x + s - self.wx, self.end.x + s + self.wx, a.x, b.x).and_then(|(x0, x1)| {
                let (t0, t1) = (x0.max(y0), x1.min(y1));
                (t0 <= t1).then_some((t0, t1))
            });
        }
        spans
    }
}

/// How far `e` (placed by [`about_pole`], about the North Pole when `north`) is, squared, from the
/// stretch `t` of the edge `a`–`b` (in the analysis plane). Beyond 85.05° the edge, straight in
/// the plane, is a spiral in the frame: it's measured in pieces short enough that none strays from
/// it by more than a thousandth of `tol_m`, at least a millimeter. Where the stretch reaches into
/// the band it isn't a spiral there, so that bound doesn't hold as such: the pieces are cut to
/// half the allowance, far finer than the edge bends there. The pieces count against `work`; a
/// stretch needing more than [`POLAR_PIECES_MAX`] is measured in that many and sets `coarse`. At
/// t = 0 and 1 the stretch ends at the edge's vertices `la` and `lb` (longitude/latitude),
/// converted as an end is (not at a + (b − a)·1), so an end on a vertex is placed on it.
#[allow(clippy::too_many_arguments)]
fn edge_from(
    e: Coord,
    (a, b): (Coord, Coord),
    (la, lb): (Coord, Coord),
    (t0, t1): (f64, f64),
    north: bool,
    tol_m: f64,
    work: &mut usize,
    coarse: &mut bool,
) -> Result<f64, WorkLimit> {
    let at = |t: f64| {
        if t <= 0.0 {
            la
        } else if t >= 1.0 {
            lb
        } else {
            plane_to_lonlat(Coord::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t))
        }
    };
    let (p, q) = (at(t0), at(t1));
    // Pieces short enough that none strays from the spiral by more than `stray`.
    let colat = |c: Coord| (90.0 - if north { c.y } else { -c.y }).to_radians();
    let rho = WGS84_POLAR_RADIUS_M * colat(p).max(colat(q));
    let drho = WGS84_POLAR_RADIUS_M * (colat(p) - colat(q)).abs();
    let dl = ((b.x - a.x) * (t1 - t0) / rustgis_geom::MERCATOR_R).abs();
    let reaches_band = p.y.abs() <= MAX_LAT || q.y.abs() <= MAX_LAT;
    let stray = (tol_m * POLAR_STRAY_SHARE).max(POLAR_STRAY_MIN_M) / if reaches_band { 2.0 } else { 1.0 };
    let n = (crate::geodesic::spiral_stray(rho, drho, dl) / stray).sqrt();
    // Past the cap, the cap; a count that overflows or isn't a number (a coordinate far out of
    // range, or missing), one piece. Either is coarse. Along a meridian, one piece, exactly.
    let pieces = if !n.is_finite() {
        *coarse = true;
        1.0
    } else if n > POLAR_PIECES_MAX {
        *coarse = true;
        POLAR_PIECES_MAX
    } else if n > 1.0 {
        n.ceil()
    } else {
        1.0
    };
    charge(work, pieces as usize)?;
    let (mut last, mut d2, mut k) = (about_pole(p, north), f64::INFINITY, 0.0);
    while k < pieces {
        k += 1.0;
        let t = if k >= pieces { t1 } else { t0 + (t1 - t0) * k / pieces };
        let next = about_pole(at(t), north);
        d2 = d2.min(rustgis_data::geometry::seg_dist2(&[last, next], e));
        last = next;
    }
    Ok(d2)
}

/// How far a walk along an edge from its start got: [`walk_edge`].
#[derive(Debug, PartialEq)]
enum Walked {
    /// It passed the distance asked for here, the edge's parameter (0 at its start, 1 at its end).
    Past(f64),
    /// It didn't: the edge is this long (meters).
    Short(f64),
}

/// Where the walk from `a` along the edge `a`–`b` (in the analysis plane), in the frame about a
/// pole (the North Pole when `north`), passes `skip` meters: in pieces of at most
/// [`POLAR_LENGTH_STEP`] of longitude, each counting against `work`, up to the one that passes it,
/// within which the place is found by halving the piece, to a millimeter, each halving counting
/// against `work` too. An edge whose length overflows or isn't a number (a coordinate far out of
/// range, or missing) is one piece, and sets `coarse`, as does one needing more than
/// [`POLAR_PIECES_MAX`], walked in that many, longer (their chords fall shorter, so the walk passes
/// later, never early).
fn walk_edge((a, b): (Coord, Coord), north: bool, skip: f64, work: &mut usize, coarse: &mut bool) -> Result<Walked, WorkLimit> {
    let n = ((b.x - a.x) / rustgis_geom::MERCATOR_R).abs() / POLAR_LENGTH_STEP;
    let pieces = if !n.is_finite() {
        *coarse = true;
        1.0
    } else if n > POLAR_PIECES_MAX {
        *coarse = true;
        POLAR_PIECES_MAX
    } else {
        n.ceil().max(1.0)
    };
    // At its ends, the edge's own vertices (a + (b − a)·t doesn't come back to them when b − a
    // overflows).
    let at = |t: f64| {
        let c = if t <= 0.0 {
            a
        } else if t >= 1.0 {
            b
        } else {
            Coord::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
        };
        about_pole(plane_to_lonlat(c), north)
    };
    let (mut last, mut length, mut k) = (at(0.0), 0.0, 0.0);
    while k < pieces {
        charge(work, 1)?;
        k += 1.0;
        let next = at(k / pieces);
        let piece = (next.x - last.x).hypot(next.y - last.y);
        if length + piece > skip {
            // Where in the piece it's `skip - length` from the piece's start, found by halving: its
            // parameter isn't in proportion to its length (a piece reaching from beyond 85.05° far
            // into the band stretches in the plane about 11.6 times more per meter beyond it). At
            // the piece's end when nothing is to be skipped, so a zero tolerance never measures
            // the end itself; at or just past the place, never before it.
            let (mut lo, mut hi) = ((k - 1.0) / pieces, k / pieces);
            if skip > 0.0 {
                let rest = skip - length;
                // How far past the place `hi` is; halved until it's within a millimeter, each
                // halving a unit of work.
                let (mut over, mut halvings) = (piece - rest, 0);
                while over > POLAR_STRAY_MIN_M && halvings < PLACE_HALVINGS {
                    let mid = 0.5 * (lo + hi);
                    if mid <= lo || mid >= hi {
                        break;
                    }
                    halvings += 1;
                    charge(work, 1)?;
                    let p = at(mid);
                    let d = (p.x - last.x).hypot(p.y - last.y);
                    if d < rest {
                        lo = mid;
                    } else {
                        hi = mid;
                        over = d - rest;
                    }
                }
            }
            return Ok(Walked::Past(hi));
        }
        length += piece;
        last = next;
    }
    Ok(Walked::Short(length))
}

/// The dangle check's ends beyond 85.05° (#1033). There the analysis plane stretches east–west by
/// sec(latitude) but north–south by a constant sec(85.05°), and a pole is a line, so an end is
/// measured in a frame about its pole ([`about_pole`]) instead, right up to the pole, where every
/// longitude meets, and across ±180°. The parts come from the grid's cells within the end's reach,
/// only those holding any (an end near a pole reaches every longitude, a whole row of cells).
struct PolarEnds<'a> {
    lines: &'a [(usize, Vec<Vec<Coord>>)],
    cells: Cells,
    /// Each row's cells that hold parts, by column, in order, with their parts.
    occupied: Vec<Vec<(i64, &'a [(usize, usize)])>>,
    /// Each line's first part's place among all the parts, and the end each part was last opened
    /// for (so a part in many cells is measured once an end).
    first: Vec<usize>,
    seen: Vec<usize>,
    end: usize,
    /// Cheap reads so far ([`READS_A_WORK`]).
    reads: usize,
    /// Whether a stretch was measured in [`POLAR_PIECES_MAX`] pieces, coarser than it needed: the
    /// check then says it's incomplete.
    coarse: bool,
}

impl<'a> PolarEnds<'a> {
    fn new(
        lines: &'a [(usize, Vec<Vec<Coord>>)],
        grid: &'a std::collections::HashMap<(i64, i64), Vec<(usize, usize)>>,
        cells: Cells,
    ) -> Self {
        let mut first = Vec::with_capacity(lines.len());
        let mut parts = 0usize;
        for (_, ps) in lines {
            first.push(parts);
            parts = parts.saturating_add(ps.len());
        }
        // The grid spans at most 513 rows and columns; a cell beyond them is never looked in. (The
        // cap on rows, well past 513, only guards against a grid built otherwise.)
        let rows = usize::try_from(cells.last.1.saturating_add(1)).unwrap_or(0).min(1024);
        let mut occupied: Vec<Vec<(i64, &'a [(usize, usize)])>> = vec![Vec::new(); rows];
        for (&(gx, gy), entries) in grid {
            if (0..=cells.last.0).contains(&gx)
                && let Some(row) = usize::try_from(gy).ok().and_then(|gy| occupied.get_mut(gy))
            {
                row.push((gx, entries.as_slice()));
            }
        }
        occupied.iter_mut().for_each(|row| row.sort_unstable_by_key(|(gx, _)| *gx));
        Self { lines, cells, occupied, first, seen: vec![0; parts], end: 0, reads: 0, coarse: false }
    }

    /// Whether `end` (in the analysis plane; `at` in longitude/latitude), an end of part `pi` of
    /// line `li` (its first point when `at_start`), comes within `tol_m` of a line: another part,
    /// or its own past the stretch within twice the tolerance of the end, as within the band. Only
    /// the stretches of edges within the end's reach ([`Reach`]) are measured. Each cell holding
    /// parts, entry, part opened, edge whose box meets the reach and piece measured counts against
    /// `work`, and edges out of reach count [`READS_A_WORK`] to a unit. The end's own part is read
    /// once an end, as within the band: its edges are measured for length (in pieces) until the
    /// walk passes twice the tolerance, and the rest, from that point on, as others' are.
    fn meets(
        &mut self,
        (li, pi, at_start): (usize, usize, bool),
        (end, at): (Coord, Coord),
        tol_m: f64,
        work: &mut usize,
    ) -> Result<bool, WorkLimit> {
        let north = at.y > 0.0;
        let e = about_pole(at, north);
        let lines = self.lines;
        let reach = Reach::new(end, at.y, tol_m);
        // Whether the edge `a`–`b`, from its parameter `from` on, comes within the tolerance.
        let mut near = |a: &Coord, b: &Coord, from: f64, work: &mut usize, coarse: &mut bool| -> Result<bool, WorkLimit> {
            if !reach.boxed(a, b) {
                return read(&mut self.reads, work).map(|()| false);
            }
            charge(work, 1)?;
            let mut lonlat = None;
            for (t0, t1) in reach.spans(a, b).into_iter().flatten() {
                let t0 = t0.max(from);
                if t0 > t1 {
                    continue;
                }
                let ends = *lonlat.get_or_insert_with(|| (plane_to_lonlat(*a), plane_to_lonlat(*b)));
                if edge_from(e, (*a, *b), ends, (t0, t1), north, tol_m, work, coarse)? <= tol_m * tol_m {
                    return Ok(true);
                }
            }
            Ok(false)
        };
        // Its own part, from the end on: the rest, past twice the tolerance along it, counts too (a
        // line curling back onto itself, like a 6, doesn't dangle there). The rest starts where the
        // walk passes that, inside an edge too: beyond 85.05° an edge is a spiral in the frame, and
        // one winding round the pole comes back past the end.
        let part = lines.get(li).and_then(|(_, ps)| ps.get(pi)).map(Vec::as_slice).unwrap_or(&[]);
        let n = part.len();
        let (mut walked, mut past) = (0.0, false);
        for i in 0..n.saturating_sub(1) {
            let ends = if at_start { (part.get(i), part.get(i + 1)) } else { (part.get(n - 1 - i), part.get(n - 2 - i)) };
            let (Some(a), Some(b)) = ends else { break };
            let skip = 2.0 * tol_m - walked;
            let from = if past {
                0.0
            } else if skip <= 0.0 && tol_m > 0.0 {
                // The edges before came to exactly twice the tolerance: the rest is all of this one.
                past = true;
                0.0
            } else {
                match walk_edge((*a, *b), north, skip, work, &mut self.coarse)? {
                    Walked::Short(length) => {
                        walked += length;
                        continue;
                    }
                    Walked::Past(from) => {
                        past = true;
                        from
                    }
                }
            };
            if near(a, b, from, work, &mut self.coarse)? {
                return Ok(true);
            }
        }
        let cells = self.cells;
        let rows = cells.key(end.x, end.y - reach.wy).1.max(0)..=cells.key(end.x, end.y + reach.wy).1.min(cells.last.1);
        // The window's columns and its copies', in order, each column once where they overlap.
        let mut columns = [(0, -1); 3];
        for (span, shift) in columns.iter_mut().zip(reach.shifts(cells.origin.0 + cells.size * cells.last.0 as f64 / 2.0)) {
            let left = cells.key(end.x + shift - reach.wx, end.y).0.max(0);
            *span = (left, cells.key(end.x + shift + reach.wx, end.y).0.min(cells.last.0));
        }
        // A grid wider than two turns can contain many copies. Scan its occupied cells once;
        // the edge tests use the nearest turn, and the existing work limit bounds the scan.
        if cells.size * (cells.last.0 as f64 + 1.0) > 2.0 * std::f64::consts::TAU * rustgis_geom::MERCATOR_R {
            columns = [(0, cells.last.0), (0, -1), (0, -1)];
        }
        columns.sort_unstable();
        let mut done = -1i64;
        for span in &mut columns {
            span.0 = span.0.max(done.saturating_add(1));
            done = done.max(span.1);
        }
        self.end = self.end.wrapping_add(1);
        for gy in rows {
            let Some(row) = usize::try_from(gy).ok().and_then(|gy| self.occupied.get(gy)) else { continue };
            for &(from, to) in &columns {
                let start = row.partition_point(|(gx, _)| *gx < from);
                for &(_, entries) in row.iter().skip(start).take_while(|(gx, _)| *gx <= to) {
                    charge(work, 1)?;
                    for &(oli, opi) in entries {
                        charge(work, 1)?;
                        let Some(seen) = self.first.get(oli).and_then(|f| self.seen.get_mut(f + opi)) else { continue };
                        if *seen == self.end || (oli, opi) == (li, pi) {
                            continue;
                        }
                        *seen = self.end;
                        charge(work, 1)?;
                        let other = lines.get(oli).and_then(|(_, ps)| ps.get(opi)).map(Vec::as_slice).unwrap_or(&[]);
                        for w in other.windows(2) {
                            let [a, b] = w else { continue };
                            if near(a, b, 0.0, work, &mut self.coarse)? {
                                return Ok(true);
                            }
                        }
                    }
                }
            }
        }
        Ok(false)
    }
}

/// A part without the stretch within `skip` metres (along the line) of one end: from the first
/// vertex further than that, to the other end.
fn rest_of_part(part: &[Coord], from_start: bool, skip: f64) -> Vec<Coord> {
    let pts: Vec<Coord> = if from_start { part.to_vec() } else { part.iter().rev().copied().collect() };
    let mut walked = 0.0;
    for (i, w) in pts.windows(2).enumerate() {
        let (Some(a), Some(b)) = (w.first(), w.get(1)) else { break };
        walked += ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
        if walked > skip {
            return pts.get(i + 1..).map(<[Coord]>::to_vec).unwrap_or_default();
        }
    }
    Vec::new()
}

/// Whether polygons (in the analysis plane, of one feature or several) hold the North Pole (the
/// South Pole when not `north`) inside them together, as the check counts the inside: on the
/// ground they surround it, so in the plane they run along that edge of the world a whole turn,
/// however their longitudes are written (from −180° to 180°, from 0° to 360°, or in parts
/// meeting or overlapping along meridians, written either way). They do if lines 0.13 m, 1 m,
/// 10 m, … 100 km off it in the plane (1.1 cm to 8.7 km on the ground; the nearest just past
/// [`rustgis_geom::POLE_TOLERANCE`], within which the overlays take a polygon to reach the pole)
/// lie inside them all the way around but for gaps of under a meter (9e-6° of longitude), as the
/// overlays' rounding leaves seams centimeters wide in the plane (nothing on the ground beside the
/// pole), from the nearest out to the first wholly outside them, where they end (#1033). A gap
/// reaching the pole (between polygons, in one's outline, or a hole no other polygon fills),
/// however narrow beside it, widens away from it, so it counts once it's wider than a meter on one
/// of the lines; one narrower all the way out to 100 km is taken for a seam. A gap near the pole
/// that doesn't reach it, crossing a line, counts too: the check leans to reporting. A polygon
/// whose crossings with a line can't be worked out (a coordinate missing or infinite there, or so
/// far around, a million turns, that a meter is lost in rounding) doesn't count. When the rest
/// don't surround the pole, or might not have with them (the lines end at one wholly outside the
/// rest that a polygon left out reaches), the rows (`polygons` come with their features' rows) of
/// those left out are returned with the answer, each once, for the check to say; otherwise those
/// left out changed nothing.
fn surrounds_pole<'a>(polygons: impl IntoIterator<Item = (usize, &'a geo::Polygon<f64>)>, north: bool) -> (bool, Vec<usize>) {
    /// The widest gap, in the plane's meters, taken for a seam.
    const SEAM_M: f64 = 1.0;
    let (rows, polygons): (Vec<usize>, Vec<&geo::Polygon<f64>>) = polygons.into_iter().unzip();
    let mut skipped = vec![false; polygons.len()];
    let round = std::f64::consts::TAU * rustgis_geom::MERCATOR_R;
    let inside = |gaps: &[(f64, f64)]| gaps.iter().all(|(a, b)| b - a <= SEAM_M);
    let ended = |gaps: &[(f64, f64)]| gaps.iter().any(|(a, b)| b - a >= round);
    let mut lines = [0.13, 1.0, 10.0, 100.0, 1e3, 1e4, 1e5].into_iter();
    let (mut surrounds, mut uncertain) = (false, false);
    if let Some(near) = lines.next() {
        surrounds = inside(&outside_line(&polygons, north, near, &mut skipped).0);
        for off in lines {
            if !surrounds {
                break;
            }
            let (gaps, left) = outside_line(&polygons, north, off, &mut skipped);
            if ended(&gaps) {
                // Where the polygons end, unless one left out reaches the line.
                uncertain = left;
                break;
            }
            surrounds = inside(&gaps);
        }
    }
    if surrounds && !uncertain {
        return (true, Vec::new());
    }
    let mut left_out: Vec<usize> = rows.iter().zip(&skipped).filter(|(_, s)| **s).map(|(r, _)| *r).collect();
    left_out.sort_unstable();
    left_out.dedup();
    (surrounds, left_out)
}

/// The stretches of the line `off` meters off the North Pole (the South Pole when not `north`) in
/// the analysis plane that lie outside `polygons`, within one turn from 0, a stretch across its end
/// taken whole (so it may run past a turn), for [`surrounds_pole`], and whether it left out a
/// polygon. A polygon that may reach the line but that it can't use is marked in `skipped`; one
/// whose latitudes miss the line plays no part, however its longitudes are written.
fn outside_line(polygons: &[&geo::Polygon<f64>], north: bool, off: f64, skipped: &mut [bool]) -> (Vec<(f64, f64)>, bool) {
    /// The farthest a polygon may lie east or west in the plane (meters, about a million turns),
    /// where it still rounds to about a centimeter, well under a seam.
    const FAR_M: f64 = 4.0e13;
    let round = std::f64::consts::TAU * rustgis_geom::MERCATOR_R;
    let pole = lonlat_to_plane(Coord::new(0.0, if north { 90.0 } else { -90.0 })).y;
    let y = if north { pole - off } else { pole + off };
    // Each polygon's stretches of the line inside it: inside its outline, less those inside its
    // holes (so a hole outside the outline takes nothing), between the places each ring crosses
    // the line (worked out exactly, where an overlay would round them to a grid); each brought
    // within one turn (split where it wraps).
    let mut inside: Vec<(f64, f64)> = Vec::new();
    let (mut outline, mut holes) = (Vec::new(), Vec::new());
    let mut left = false;
    for (i, polygon) in polygons.iter().enumerate() {
        let Some(r) = polygon.bounding_rect() else { continue };
        let mut skip = || {
            left = true;
            if let Some(s) = skipped.get_mut(i) {
                *s = true;
            }
        };
        let (y0, y1) = (r.min().y, r.max().y);
        if y0.is_finite() && y1.is_finite() && !(y0 <= y && y <= y1) {
            continue;
        }
        let usable = [r.min().x, y0, r.max().x, y1].iter().all(|v| v.is_finite()) && r.min().x >= -FAR_M && r.max().x <= FAR_M;
        if !usable {
            skip();
            continue;
        }
        outline.clear();
        holes.clear();
        // A coordinate missing would pair the crossings wrongly.
        if !ring_stretches(polygon.exterior(), y, &mut outline)
            || !polygon.interiors().iter().all(|hole| ring_stretches(hole, y, &mut holes))
        {
            skip();
            continue;
        }
        holes.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (lo, hi) in less(&outline, &holes) {
            if hi - lo >= round {
                return (Vec::new(), left);
            }
            let from = lo.rem_euclid(round);
            let to = from + (hi - lo);
            inside.push((from, to.min(round)));
            if to > round {
                inside.push((0.0, to - round));
            }
        }
    }
    // What lies between them, west to east, from the turn's start.
    inside.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut outside = Vec::new();
    let mut reached = 0.0;
    for (from, to) in inside {
        if from > reached {
            outside.push((reached, from));
        }
        reached = f64::max(reached, to);
    }
    if reached < round {
        outside.push((reached, round));
    }
    // A stretch running across the turn's end and on from its start is one.
    if let [first, .., last] = outside.as_mut_slice()
        && first.0 <= 0.0
        && last.1 >= round
    {
        last.1 = round + first.1;
        outside.remove(0);
    }
    (outside, left)
}

/// Adds to `out` the stretches of the line at `y` inside `ring`, between the places it crosses
/// the line, paired west to east; false when a crossing can't be worked out (a coordinate missing
/// or infinite).
fn ring_stretches(ring: &geo::LineString<f64>, y: f64, out: &mut Vec<(f64, f64)>) -> bool {
    let mut crossings: Vec<f64> = ring
        .lines()
        .filter(|e| (e.start.y > y) != (e.end.y > y))
        .map(|e| e.start.x + (y - e.start.y) * (e.end.x - e.start.x) / (e.end.y - e.start.y))
        .collect();
    if crossings.iter().any(|x| !x.is_finite()) {
        return false;
    }
    crossings.sort_by(f64::total_cmp);
    out.extend(crossings.as_chunks::<2>().0.iter().filter(|[lo, hi]| lo < hi).map(|&[lo, hi]| (lo, hi)));
    true
}

/// The parts of the stretches `of` (apart, west to east, as [`ring_stretches`] gives them) outside
/// every one of `holes` (sorted by where they start), in one pass along both: a ring crossing the
/// line a million times costs no more than its crossings.
fn less(of: &[(f64, f64)], holes: &[(f64, f64)]) -> Vec<(f64, f64)> {
    // The holes' stretches joined where they overlap, so they too lie apart, west to east.
    let mut apart: Vec<(f64, f64)> = Vec::with_capacity(holes.len());
    for &(lo, hi) in holes {
        match apart.last_mut() {
            Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
            _ => apart.push((lo, hi)),
        }
    }
    let mut out = Vec::new();
    let mut next = 0;
    for &(mut lo, hi) in of {
        // Holes ending before this stretch end before every later one too.
        while apart.get(next).is_some_and(|h| h.1 <= lo) {
            next += 1;
        }
        // Those overlapping it: the last of them may overlap the next stretch, so it's kept.
        for &(hole_lo, hole_hi) in apart.get(next..).unwrap_or_default() {
            if hole_lo >= hi {
                break;
            }
            if hole_lo > lo {
                out.push((lo, hole_lo));
            }
            lo = lo.max(hole_hi);
            if lo >= hi {
                break;
            }
        }
        if lo < hi {
            out.push((lo, hi));
        }
    }
    out
}

/// Points of `ds` that fall in no polygon of `by`. A point at a pole (to within
/// [`rustgis_geom::POLE_TOLERANCE`]) also falls in the polygons of `by` when together they surround
/// the pole (`surrounds_pole`); ones that only touch it, at a corner, hold it on their boundary,
/// as everywhere else (#1033). Features of `by` the test at a pole had to leave out (a coordinate
/// missing or infinite, or a million turns around) when the rest don't surround it, or might not
/// have with them, are counted in `skipped`, each once at either pole, for the check to say its
/// result there may be wrong.
pub fn not_covered(ds: &Dataset, rows: &[usize], by: &Dataset) -> Checked {
    let all: Vec<usize> = (0..by.features.len()).collect();
    let polys = polygons(by, &all);
    // Whether the polygons together surround each pole, found at the first point there, with the
    // features left out.
    type Holds = Option<(bool, Vec<usize>)>;
    let (mut holds_north, mut holds_south): (Holds, Holds) = (None, None);
    let with_skipped = |c: Checked, north: &Holds, south: &Holds| {
        let mut left_out: Vec<usize> = north.iter().chain(south).flat_map(|(_, rows)| rows.iter().copied()).collect();
        left_out.sort_unstable();
        left_out.dedup();
        Checked { skipped: left_out.len(), ..c }
    };
    let parts = || polys.iter().flat_map(|(r, mp, _)| mp.0.iter().map(move |p| (*r, p)));
    let mut out = Vec::new();
    crate::control::begin(rows.len());
    for (i, r) in rows.iter().enumerate() {
        if crate::control::cancelled() {
            return with_skipped(Checked::partial(out), &holds_north, &holds_south);
        }
        crate::control::reached(i);
        let pts: Vec<Coord> = match ds.features.get(*r).and_then(|f| f.geometry.as_ref()) {
            Some(Geometry::Point(c)) => vec![*c],
            Some(Geometry::MultiPoint(v)) => v.clone(),
            _ => continue,
        };
        for c in pts {
            let m = lonlat_to_plane(c);
            let p = geo::Point::new(m.x, m.y);
            let surrounded = match crate::convert::reaches_poles(c.y, c.y) {
                (true, _) => holds_north.get_or_insert_with(|| surrounds_pole(parts(), true)).0,
                (_, true) => holds_south.get_or_insert_with(|| surrounds_pole(parts(), false)).0,
                _ => false,
            };
            let covered = surrounded || polys.iter().any(|(_, mp, bb)| rects_touch(bb, &geo::Rect::new(p.0, p.0)) && mp.contains(&p));
            if !covered {
                out.push(TopologyError { rows: vec![*r], at: c });
            }
        }
        if out.len() >= MAX_ERRORS {
            return with_skipped(Checked::partial(out), &holds_north, &holds_south);
        }
    }
    with_skipped(Checked::done(out), &holds_north, &holds_south)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::GeometryType;

    #[test]
    fn holes_are_taken_from_an_outline_in_one_pass() {
        // Holes overlapping each other, one running across two stretches, one covering a stretch.
        let got = less(&[(0.0, 10.0), (20.0, 30.0), (40.0, 50.0)], &[(1.0, 5.0), (2.0, 3.0), (4.0, 8.0), (9.0, 25.0), (40.0, 50.0)]);
        assert_eq!(got, [(0.0, 1.0), (8.0, 9.0), (25.0, 30.0)]);
        // A million stretches and a million holes between them, as a ring zigzagging across the
        // line would make: one pass, not a million times a million.
        let at = |i: u32, d: f64| f64::from(i) * 4.0 + d;
        let of: Vec<(f64, f64)> = (0..1_000_000).map(|i| (at(i, 0.0), at(i, 2.0))).collect();
        let holes: Vec<(f64, f64)> = (0..1_000_000).map(|i| (at(i, 1.0), at(i, 3.0))).collect();
        let got = less(&of, &holes);
        assert!(got.len() == 1_000_000 && got.iter().all(|(lo, hi)| hi - lo == 1.0));
    }

    #[test]
    fn a_polygon_left_out_at_a_pole_is_counted() {
        // Around the South Pole, a cap from 20°E round to 10°E, and the 10° between them written a
        // million turns and more on (1e9° + 90° is 10°E): that one can't be used at the pole, so a
        // point there is reported, and the check counts the polygon it left out. A whole cap
        // covers the point without looking further, and counts none.
        let c = Coord::new;
        let ring = |w: f64, e: f64| Geometry::Polygon(vec![vec![vec![c(w, -85.0), c(e, -85.0), c(e, -90.0), c(w, -90.0), c(w, -85.0)]]]);
        let cap = || ring(-180.0, 180.0);
        let (open, far) = (ring(20.0, 370.0), ring(1e9 + 90.0, 1e9 + 100.0));
        let mut pts = Dataset::new("pts", GeometryType::Point, vec![]);
        pts.push(Some(Geometry::Point(c(0.0, -90.0))), vec![]);
        let layer = |gs: Vec<Geometry>| {
            let mut by = Dataset::new("by", GeometryType::Polygon, vec![]);
            for g in gs {
                by.push(Some(g), vec![]);
            }
            by
        };
        let checked = not_covered(&pts, &[0], &layer(vec![open, far]));
        assert!(checked.errors.len() == 1 && checked.skipped == 1, "{checked:?}");
        let checked = not_covered(&pts, &[0], &layer(vec![cap()]));
        assert!(checked.errors.is_empty() && checked.skipped == 0, "{checked:?}");
        // A polygon whose latitudes miss the pole's lines plays no part, however far around it's
        // written: with a whole cap, in either order, the point is covered and none is counted.
        let at = |w: f64, e: f64, y0: f64, y1: f64| vec![c(w, y0), c(e, y0), c(e, y1), c(w, y1), c(w, y0)];
        let band = Geometry::Polygon(vec![vec![at(1e9, 1e9 + 10.0, 0.0, 10.0)]]);
        for gs in [vec![band.clone(), cap()], vec![cap(), band.clone()]] {
            let checked = not_covered(&pts, &[0], &layer(gs));
            assert!(checked.errors.is_empty() && checked.skipped == 0, "{checked:?}");
        }
        // Left out at both poles, a feature of two parts at one and a feature at the other, beside
        // that band: two features, each once.
        let open = |y0: f64, y1: f64| Geometry::Polygon(vec![vec![at(20.0, 370.0, y0, y1)]]);
        let halves =
            |y0: f64, y1: f64| Geometry::Polygon(vec![vec![at(1e9 + 90.0, 1e9 + 95.0, y0, y1)], vec![at(1e9 + 95.0, 1e9 + 100.0, y0, y1)]]);
        let mut poles = Dataset::new("pts", GeometryType::Point, vec![]);
        poles.push(Some(Geometry::Point(c(0.0, -90.0))), vec![]);
        poles.push(Some(Geometry::Point(c(0.0, 90.0))), vec![]);
        let by = layer(vec![open(-85.0, -90.0), halves(-85.0, -90.0), band, open(85.0, 90.0), halves(85.0, 90.0)]);
        let checked = not_covered(&poles, &[0, 1], &by);
        assert!(checked.errors.len() == 2 && checked.skipped == 2, "{checked:?}");
        // A cap 500 m across about the South Pole covers it out to its lines within 87 m; the line
        // 860 m off lies wholly outside it, where the check ends. A polygon left out that reaches
        // that line might have changed the answer: the point is still covered, but it's counted.
        let small = Geometry::Polygon(vec![vec![at(-180.0, 180.0, -90.0, -89.9955)]]);
        let checked = not_covered(&pts, &[0], &layer(vec![small.clone()]));
        assert!(checked.errors.is_empty() && checked.skipped == 0, "{checked:?}");
        let across = Geometry::Polygon(vec![vec![at(1e9, 1e9 + 10.0, -89.995, -89.99)]]);
        let checked = not_covered(&pts, &[0], &layer(vec![small, across]));
        assert!(checked.errors.is_empty() && checked.skipped == 1, "{checked:?}");
    }

    fn square(x: f64, y: f64, s: f64) -> Geometry {
        Geometry::Polygon(vec![vec![vec![
            Coord::new(x, y),
            Coord::new(x + s, y),
            Coord::new(x + s, y + s),
            Coord::new(x, y + s),
            Coord::new(x, y),
        ]]])
    }

    #[test]
    fn a_polar_end_counts_its_work_and_stops_at_the_limit() {
        /// Whether line 0's end at 89°N meets a line, all in one cell, with the work it took
        /// (`None`: it passed the limit, starting from `work`).
        fn run(lines: &[(usize, Vec<Vec<Coord>>)], work: usize) -> (Option<bool>, usize) {
            let end = lonlat_to_plane(Coord::new(0.0, 89.0));
            let mut grid = std::collections::HashMap::new();
            grid.insert((0, 0), vec![(0, 0), (1, 0)]);
            let cells = Cells::new((end.x - 1e9, end.y - 1e9), 1e10, (end.x, end.y));
            let mut work = work;
            let met = PolarEnds::new(lines, &grid, cells).meets((0, 0, false), (end, plane_to_lonlat(end)), 10.0, &mut work);
            (met.ok(), work)
        }
        let p = |x: f64, y: f64| lonlat_to_plane(Coord::new(x, y));
        let own = vec![p(0.0, 88.0), p(0.0, 89.0)];
        // Line 1 meets it there.
        assert_eq!(run(&[(0, vec![own.clone()]), (1, vec![vec![p(1.0, 88.0), p(0.0, 89.0)]])], 0).0, Some(true));
        // Line 1 is 1,000 points at 86°N, far off but in the cell: the end measures its own edge's
        // length (one piece, 111 km, and 26 halvings placing twice the tolerance in it to a
        // millimeter) and the edge from there on (one unit), reads the cell and its two entries,
        // opens line 1 (one unit) and finds its 999 edges out of reach (one unit per READS_A_WORK).
        let apart = [(0, vec![own]), (1, vec![(0..1_000).map(|i| p(90.0 + f64::from(i) * 1e-4, 86.0)).collect()])];
        assert_eq!(run(&apart, 0), (Some(false), 1 + 26 + 1 + 1 + 2 + 1 + 999 / READS_A_WORK));
        // Starting short of the limit, it stops on the first unit past it: its own edge, a
        // halving, the cell, an entry, opening line 1, or reading line 1's edges.
        for start in [MAX_WORK, MAX_WORK - 1, MAX_WORK - 2, MAX_WORK - 4, MAX_WORK - 10] {
            assert_eq!(run(&apart, start), (None, MAX_WORK + 1), "{start}");
        }
        // Reads count across the ends: a second end reads line 1's edges on from the first's 999.
        let end = p(0.0, 89.0);
        let cells = Cells::new((end.x - 1e9, end.y - 1e9), 1e10, (end.x, end.y));
        let mut grid = std::collections::HashMap::new();
        grid.insert((0, 0), vec![(0, 0), (1, 0)]);
        let mut polar = PolarEnds::new(&apart, &grid, cells);
        let mut ends = [(None, 0), (None, 0)];
        for (met, work) in &mut ends {
            *met = polar.meets((0, 0, false), (end, plane_to_lonlat(end)), 10.0, work).ok();
        }
        // Each end: 6 units and its 26 halvings, as above, and the reads.
        let first = 6 + 26 + 999 / READS_A_WORK;
        assert_eq!(ends, [(Some(false), first), (Some(false), 6 + 26 + (2 * 999 / READS_A_WORK - 999 / READS_A_WORK))]);
        // The end's own part's edges out of reach, past twice the tolerance, are read too: nine
        // toward and along 86°N, with no other line, after its own first edge's piece (and its 26
        // halvings) and the edge from twice the tolerance on.
        let own: Vec<Coord> = (0..9).map(|i| p(90.0 + f64::from(i) * 1e-4, 86.0)).chain([p(0.0, 88.0), end]).collect();
        let (lone, mut work) = ([(0, vec![own])], 0);
        let met = PolarEnds::new(&lone, &std::collections::HashMap::new(), cells).meets(
            (0, 0, false),
            (end, plane_to_lonlat(end)),
            10.0,
            &mut work,
        );
        assert_eq!((met.ok(), work), (Some(false), 1 + 26 + 1 + 9 / READS_A_WORK));
    }

    #[test]
    fn a_small_layer_is_checked_at_a_large_tolerance() {
        // 2,000 lines 100 m long side by side across 89 m, checked at 1 km: the grid's cells are as
        // wide as the layer, and each segment goes only into those within it, so the check
        // finishes. It wouldn't if each segment filled every cell its widened box covers: about
        // 54,000 each, twice the work limit in all.
        let mut ds = Dataset::new("l", GeometryType::Polyline, vec![]);
        for i in 0..2_000 {
            let x = f64::from(i) * 4e-7;
            ds.push(Some(Geometry::Line(vec![vec![Coord::new(x, 0.0), Coord::new(x, 0.0009)]])), vec![]);
        }
        let checked = dangles(&ds, &(0..2_000).collect::<Vec<_>>(), 1_000.0);
        assert!(checked.complete && checked.errors.is_empty(), "complete {}, {} errors", checked.complete, checked.errors.len());
    }

    #[test]
    fn an_edge_with_a_coordinate_out_of_range_is_one_piece() {
        // Each is one piece, and coarse; so is a stretch of one in edge_from, a zero-length
        // stretch of an edge whose length overflows included (∞ × 0 is no number).
        let b = lonlat_to_plane(Coord::new(0.0, 89.0));
        for a in [Coord::new(f64::INFINITY, b.y), Coord::new(f64::NAN, b.y)] {
            let (mut work, mut coarse) = (0, false);
            let _ = walk_edge((a, b), true, 0.0, &mut work, &mut coarse);
            assert!(work == 1 && coarse, "{a:?}");
            let (mut work, mut coarse, ends) = (0, false, (plane_to_lonlat(a), plane_to_lonlat(b)));
            let e = about_pole(Coord::new(0.0, 89.0), true);
            assert!(edge_from(e, (a, b), ends, (0.0, 1.0), true, 1.0, &mut work, &mut coarse).is_ok() && coarse && work == 1, "{a:?}");
        }
        let (a, b) = (Coord::new(-f64::MAX, b.y), Coord::new(f64::MAX, b.y));
        let (mut work, mut coarse, ends) = (0, false, (plane_to_lonlat(a), plane_to_lonlat(b)));
        let e = about_pole(Coord::new(0.0, 89.0), true);
        assert!(edge_from(e, (a, b), ends, (0.5, 0.5), true, 1.0, &mut work, &mut coarse).is_ok() && coarse && work == 1);
    }

    #[test]
    fn a_long_edge_is_walked_only_as_far_as_it_needs() {
        // An edge winding round the North Pole about 194 times (70,000° of longitude), more than
        // POLAR_PIECES_MAX pieces of POLAR_LENGTH_STEP: it's walked in that many, and says so. The
        // walk from its start passes 20 m within its first piece, and stops there. A stretch of
        // it needing more than POLAR_PIECES_MAX pieces is measured in that many, and says so too.
        let (a, b) = (lonlat_to_plane(Coord::new(0.0, 88.0)), lonlat_to_plane(Coord::new(70_000.0, 88.0)));
        assert!(70_000f64.to_radians() / POLAR_LENGTH_STEP > POLAR_PIECES_MAX);
        let (mut work, mut coarse) = (0, false);
        let Ok(Walked::Past(t)) = walk_edge((a, b), true, 20.0, &mut work, &mut coarse) else { panic!("not past") };
        let passed = about_pole(plane_to_lonlat(Coord::new(a.x + (b.x - a.x) * t, a.y)), true);
        let start = about_pole(plane_to_lonlat(a), true);
        let past = (passed.x - start.x).hypot(passed.y - start.y) - 20.0;
        assert!(t < 1.0 / POLAR_PIECES_MAX && (0.0..=POLAR_STRAY_MIN_M).contains(&past), "{t} {past}");
        // One piece, and the halvings placing 20 m within it.
        let walked = work;
        assert!(coarse && walked > 1 && walked <= 1 + PLACE_HALVINGS, "{walked}");
        let ends = (plane_to_lonlat(a), plane_to_lonlat(b));
        coarse = false;
        assert!(edge_from(start, (a, b), ends, (0.0, 1.0), true, 1.0, &mut work, &mut coarse).is_ok());
        assert!(coarse && work == walked + POLAR_PIECES_MAX as usize, "{work}");
        // Under the cap (600° of longitude), the walk is as fine as it needs, and not coarse.
        let (mut work, mut coarse) = (0, false);
        let short = lonlat_to_plane(Coord::new(600.0, 88.0));
        assert!(matches!(walk_edge((a, short), true, 20.0, &mut work, &mut coarse), Ok(Walked::Past(_))) && !coarse);
    }

    #[test]
    fn an_edge_whose_piece_count_overflows_is_measured_as_one_coarse_piece() {
        // An edge at 88°N running 1e300 m across the plane (coordinates far out of range): its
        // piece count overflows. It's measured as one piece, for one unit of work, and is coarse,
        // not cut into POLAR_PIECES_MAX pieces that measure nothing better.
        let a = lonlat_to_plane(Coord::new(0.0, 88.0));
        let b = Coord::new(1e300, a.y);
        let (ends, mut work, mut coarse) = ((plane_to_lonlat(a), plane_to_lonlat(b)), 0, false);
        let e = about_pole(Coord::new(0.0, 88.0), true);
        assert!(edge_from(e, (a, b), ends, (0.0, 1.0), true, 1.0, &mut work, &mut coarse).is_ok());
        assert!(coarse && work == 1, "{work}");
    }

    #[test]
    fn a_stretch_measured_coarsely_leaves_the_check_incomplete_not_stopped() {
        // A line 11 m from the North Pole winding round it about 19,000 times (7,000,000° of
        // longitude): each end, within 20 m of the pole, reaches the whole of it, which needs about
        // a million pieces. It's measured in POLAR_PIECES_MAX, and the check says it's incomplete;
        // the other line's ends are still checked, and dangle.
        let c = Coord::new;
        let mut ds = Dataset::new("l", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![vec![c(0.0, 89.9999), c(7_000_000.0, 89.9999)]])), vec![]);
        ds.push(Some(Geometry::Line(vec![vec![c(0.0, 86.0), c(0.001, 86.0)]])), vec![]);
        let checked = dangles(&ds, &[0, 1], 20.0);
        let other: Vec<Coord> = checked.errors.iter().filter(|e| e.rows == [1]).map(|e| e.at).collect();
        assert!(!checked.complete && other.len() == 2, "complete {}, {other:?}", checked.complete);
        // Measured as closely as it needs, a line winding a few times is complete.
        ds.features.clear();
        ds.push(Some(Geometry::Line(vec![vec![c(0.0, 89.9999), c(2_000.0, 89.9999)]])), vec![]);
        assert!(dangles(&ds, &[0], 20.0).complete);
    }

    #[test]
    fn a_polar_end_meets_its_own_line_past_twice_the_tolerance_within_an_edge() {
        // A hairpin at 86°N: 14.8 m east from E, then back west through E for 3°. The line passes
        // E 29.6 m along it, past twice the 10 m tolerance, inside the second edge's first piece
        // (7.8 km): E doesn't dangle, only the far end does.
        let mut ds = Dataset::new("l", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 86.0), Coord::new(0.0019, 86.0), Coord::new(-3.0, 86.0)]])), vec![]);
        let checked = dangles(&ds, &[0], 10.0);
        let at: Vec<Coord> = checked.errors.iter().map(|e| e.at).collect();
        assert!(checked.complete && at.len() == 1 && at.iter().all(|c| (c.x + 3.0).abs() < 1e-9), "{at:?}");
    }

    #[test]
    fn a_polar_end_on_a_line_reaching_far_into_the_band_dangles() {
        // Lines down a meridian, or nearly, from beyond 85.05° far into the band: each is one
        // piece, along which the plane stretches about 11.6 times more per meter beyond 85.05°
        // than within it. Twice the tolerance is found on the ground, not in proportion to the
        // plane, so the polar end doesn't meet its own line a few meters on, and both ends dangle.
        let ends = [((0.0, 86.0), (0.0, 60.0)), ((0.0, -86.0), (0.0, -60.0)), ((0.0, 89.0), (0.0, 0.0)), ((0.0, 86.0), (0.5, 60.0))];
        for ((x0, y0), (x1, y1)) in ends {
            let mut ds = Dataset::new("l", GeometryType::Polyline, vec![]);
            ds.push(Some(Geometry::Line(vec![vec![Coord::new(x0, y0), Coord::new(x1, y1)]])), vec![]);
            let checked = dangles(&ds, &[0], 10.0);
            assert!(checked.complete && checked.errors.len() == 2, "({x0}, {y0}) to ({x1}, {y1}): {:?}", checked.errors);
        }
        // The walk passes 20 m from the end on the ground, in the frame about the pole.
        let (a, b) = (lonlat_to_plane(Coord::new(0.0, 86.0)), lonlat_to_plane(Coord::new(0.0, 60.0)));
        let (mut work, mut coarse) = (0, false);
        let Ok(Walked::Past(t)) = walk_edge((a, b), true, 20.0, &mut work, &mut coarse) else { panic!("not past") };
        let at = about_pole(plane_to_lonlat(Coord::new(a.x, a.y + (b.y - a.y) * t)), true);
        let start = about_pole(plane_to_lonlat(a), true);
        let past = (at.x - start.x).hypot(at.y - start.y) - 20.0;
        assert!((0.0..=POLAR_STRAY_MIN_M).contains(&past), "{t} {past}");
    }

    #[test]
    fn an_edge_whose_length_overflows_leaves_the_check_incomplete() {
        // An edge along 88°N from −9.2e302° to 9.2e302° of longitude: its length in the plane
        // overflows. It's walked as one piece between its own vertices, to a length that's a
        // number, and the check says it's incomplete rather than measuring it as nothing.
        let (w, e) = (Coord::new(-9.2e302, 88.0), Coord::new(9.2e302, 88.0));
        let (a, b) = (lonlat_to_plane(w), lonlat_to_plane(e));
        assert!(a.is_finite() && b.is_finite() && (b.x - a.x).is_infinite());
        let (mut work, mut coarse) = (0, false);
        let walked = walk_edge((a, b), true, f64::INFINITY, &mut work, &mut coarse);
        assert!(coarse && work == 1 && matches!(walked, Ok(Walked::Short(l)) if l.is_finite()), "{:?}", walked.ok());
        let mut ds = Dataset::new("l", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![vec![w, e]])), vec![]);
        assert!(!dangles(&ds, &[0], 10.0).complete);
    }

    #[test]
    fn a_polar_end_meets_its_own_line_from_exactly_twice_the_tolerance() {
        // The hairpin above at a tolerance of half its first edge's length: the walk comes to
        // twice the tolerance exactly at that edge's end, so all of the second edge counts, and it
        // passes back through E within its first piece. E doesn't dangle, only the far end does.
        let (e, f, g) = (Coord::new(0.0, 86.0), Coord::new(0.0019, 86.0), Coord::new(-3.0, 86.0));
        let (mut work, mut coarse) = (0, false);
        let first = (lonlat_to_plane(e), lonlat_to_plane(f));
        let Ok(Walked::Short(length)) = walk_edge(first, true, f64::INFINITY, &mut work, &mut coarse) else { panic!("not short") };
        let mut ds = Dataset::new("l", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![vec![e, f, g]])), vec![]);
        let checked = dangles(&ds, &[0], length / 2.0);
        let at: Vec<Coord> = checked.errors.iter().map(|e| e.at).collect();
        assert!(checked.complete && at.len() == 1 && at.iter().all(|c| (c.x + 3.0).abs() < 1e-9), "{at:?}");
    }

    #[test]
    fn an_edge_winding_round_a_pole_meets_its_own_end() {
        // One edge along 88°N through 400° of longitude passes back over both its ends (0° and,
        // a turn on, 40°): neither dangles, though each is far, along the edge, from where it
        // passes. Through 300° it doesn't come back, and both do.
        let line = |to: f64| {
            let mut ds = Dataset::new("l", GeometryType::Polyline, vec![]);
            ds.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 88.0), Coord::new(to, 88.0)]])), vec![]);
            dangles(&ds, &[0], 10.0)
        };
        let wound = line(400.0);
        assert!(wound.complete && wound.errors.is_empty(), "{:?}", wound.errors);
        let open = line(300.0);
        assert!(open.complete && open.errors.len() == 2, "{:?}", open.errors);
    }

    #[test]
    fn a_polar_end_reads_only_the_cells_holding_parts() {
        // An end 1.1 m from the North Pole reaches every longitude: a whole row of 512 cells, of
        // which only the last holds a part (line 1, far off). The end measures its own edge's
        // length (one piece, 11 km, and 24 halvings placing twice the tolerance in it to a
        // millimeter) and the edge from there on, and reads that cell, its entry and line 1 (a
        // unit each); the empty cells cost nothing, and cells beyond the grid's rows and columns
        // aren't looked in.
        let p = |x: f64, y: f64| lonlat_to_plane(Coord::new(x, y));
        let end = p(0.0, 90.0 - 1e-5);
        let lines = [(0, vec![vec![p(0.0, 89.9), end]]), (1, vec![vec![p(90.0, 86.0), p(90.1, 86.0)]])];
        let mut grid = std::collections::HashMap::new();
        for cell in [(511, 0), (-1, 0), (512, 0), (511, 1)] {
            grid.insert(cell, vec![(1, 0)]);
        }
        let cells = Cells::new((end.x - 1.0, end.y - 0.5e9), 1e9, (end.x - 1.0 + 511.5e9, end.y));
        assert_eq!(cells.last, (511, 0));
        let mut work = 0;
        let met = PolarEnds::new(&lines, &grid, cells).meets((0, 0, false), (end, plane_to_lonlat(end)), 10.0, &mut work);
        assert_eq!((met.ok(), work), (Some(false), 5 + 24));
    }

    #[test]
    fn a_polar_edge_is_measured_along_its_spiral() {
        // Straight in the plane from 86°N to 89°N, turning 120° about the pole: in the frame about
        // the pole it's a spiral, far from the chord between its ends.
        let (la, lb) = (Coord::new(0.0, 86.0), Coord::new(120.0, 89.0));
        let (a, b) = (lonlat_to_plane(la), lonlat_to_plane(lb));
        let mid = about_pole(plane_to_lonlat(Coord::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0)), true);
        let chord = rustgis_data::geometry::seg_dist2(&[about_pole(la, true), about_pole(lb, true)], mid).sqrt();
        assert!(chord > 10_000.0, "{chord}");
        // A 10 m tolerance allows pieces to stray 1 cm.
        let (mut work, mut coarse, ends) = (0, false, (plane_to_lonlat(a), plane_to_lonlat(b)));
        let d = edge_from(mid, (a, b), ends, (0.0, 1.0), true, 10.0, &mut work, &mut coarse).map(f64::sqrt).unwrap_or(f64::NAN);
        assert!(d < 0.01 && work > 1 && work <= POLAR_PIECES_MAX as usize && !coarse, "{d} m, {work} pieces");
        // A vertex is on its edge (within a micrometer), and a stretch is measured on its own.
        let at_b = edge_from(about_pole(ends.1, true), (a, b), ends, (0.0, 1.0), true, 10.0, &mut work, &mut coarse).unwrap_or(f64::NAN);
        assert!(at_b < 1e-12, "{at_b}");
        let first_half = edge_from(about_pole(ends.1, true), (a, b), ends, (0.0, 0.5), true, 10.0, &mut work, &mut coarse).unwrap_or(0.0);
        assert!(first_half.sqrt() > 10_000.0, "{first_half}");
    }

    #[test]
    fn reach_finds_the_stretches_of_an_edge_within_it() {
        let round = std::f64::consts::TAU * rustgis_geom::MERCATOR_R;
        let c = Coord::new;
        let reach = Reach { end: c(0.0, 0.0), wx: 10.0, wy: 5.0 };
        // Each edge, and its stretches within the window, a turn east of it and a turn west of it.
        let cases = [
            (c(-20.0, 0.0), c(20.0, 0.0), [Some((0.25, 0.75)), None, None]),
            (c(0.0, -10.0), c(20.0, 10.0), [Some((0.25, 0.5)), None, None]),
            (c(-20.0, 10.0), c(20.0, 10.0), [None, None, None]),
            (c(round - 20.0, 0.0), c(round, 0.0), [Some((0.5, 1.0)), None, None]),
            (c(-round, 0.0), c(round, 0.0), [Some((0.5, 0.5)), Some((1.0, 1.0)), Some((0.0, 0.0))]),
            (c(f64::NAN, 0.0), c(20.0, 0.0), [None, None, None]),
            (c(0.0, f64::NAN), c(20.0, 0.0), [None, None, None]),
        ];
        for (a, b, want) in cases {
            let got = reach.spans(&a, &b);
            let close = got.iter().zip(&want).all(|(g, w)| match (g, w) {
                (Some(g), Some(w)) => (g.0 - w.0).abs() < 1e-6 && (g.1 - w.1).abs() < 1e-6,
                (g, w) => g.is_none() && w.is_none(),
            });
            assert!(close, "{a:?}–{b:?}: {got:?}");
            // The box test never turns away an edge with a stretch in reach.
            assert!(got.iter().all(Option::is_none) || reach.boxed(&a, &b), "{a:?}–{b:?}");
        }
        // A tolerance reaching the pole reaches every longitude.
        let around = Reach { end: c(0.0, 0.0), wx: f64::INFINITY, wy: 5.0 };
        assert_eq!(around.spans(&c(-1e9, -10.0), &c(1e9, 10.0)), [Some((0.25, 0.75)), None, None]);
        assert!(Reach::new(c(0.0, 0.0), 89.999_99, 10.0).wx.is_infinite() && Reach::new(c(0.0, 0.0), 89.0, 10.0).wx.is_finite());
    }

    #[test]
    fn checks_find_problems() {
        let mut ds = Dataset::new("p", GeometryType::Polygon, vec![]);
        ds.push(Some(square(0.0, 0.0, 1.0)), vec![]);
        ds.push(Some(square(0.5, 0.5, 1.0)), vec![]); // overlaps the first
        ds.push(Some(square(1.0, 0.0, 1.0)), vec![]); // shares an edge with the first
        let rows: Vec<usize> = (0..3).collect();
        let o = overlaps(&ds, &rows).errors;
        assert!(o.iter().any(|e| e.rows == vec![0, 1]) && !o.iter().any(|e| e.rows == vec![0, 2]), "{o:?}");
        // A ring of four squares around a hole.
        let mut ring = Dataset::new("r", GeometryType::Polygon, vec![]);
        for (x, y) in [(0.0, 0.0), (1.0, 0.0), (2.0, 0.0), (0.0, 1.0), (2.0, 1.0), (0.0, 2.0), (1.0, 2.0), (2.0, 2.0)] {
            ring.push(Some(square(x, y, 1.0)), vec![]);
        }
        let g = gaps(&ring, &(0..8).collect::<Vec<_>>()).errors;
        assert_eq!(g.len(), 1);
        assert!((g[0].at.x - 1.5).abs() < 0.1 && (g[0].at.y - 1.5).abs() < 0.1, "{:?}", g[0].at);
        // A bow tie crosses itself.
        let mut lines = Dataset::new("l", GeometryType::Polyline, vec![]);
        lines.push(
            Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0), Coord::new(1.0, 0.0), Coord::new(0.0, 1.0)]])),
            vec![],
        );
        lines.push(Some(Geometry::Line(vec![vec![Coord::new(5.0, 5.0), Coord::new(6.0, 5.0)]])), vec![]);
        lines.push(Some(Geometry::Line(vec![vec![Coord::new(6.0, 5.0), Coord::new(6.0, 6.0)]])), vec![]);
        assert_eq!(self_intersections(&lines, &[0, 1, 2]).errors.len(), 1);
        // Line 1 starts and line 2 ends in the open: two dangles (they meet at 6,5).
        assert_eq!(dangles(&lines, &[1, 2], 1.0).errors.len(), 2);
        let mut pts = Dataset::new("pt", GeometryType::Point, vec![]);
        pts.push(Some(Geometry::Point(Coord::new(0.5, 0.5))), vec![]);
        pts.push(Some(Geometry::Point(Coord::new(9.0, 9.0))), vec![]);
        assert_eq!(not_covered(&pts, &[0, 1], &ds).errors.iter().map(|e| e.rows.clone()).collect::<Vec<_>>(), vec![vec![1]]);
        // Merging two overlapping squares makes one polygon.
        let m = merge(&[square(0.0, 0.0, 1.0), square(0.5, 0.0, 1.0)]).unwrap_or(Geometry::Point(Coord::default()));
        assert!(matches!(&m, Geometry::Polygon(p) if p.len() == 1));
        assert_eq!(explode(&Geometry::MultiPoint(vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)])).len(), 2);
    }

    #[test]
    fn a_point_across_the_antimeridian_is_covered_by_its_polygon() {
        // A polygon kept unwrapped (179.9° to 180.1°) covers a point at 180.05°, which is -179.95° once
        // folded: the point is covered, not reported as outside the polygon.
        let ring =
            vec![Coord::new(179.9, 0.0), Coord::new(180.1, 0.0), Coord::new(180.1, 1.0), Coord::new(179.9, 1.0), Coord::new(179.9, 0.0)];
        let mut ds = Dataset::new("p", GeometryType::Polygon, vec![]);
        ds.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
        let mut pts = Dataset::new("pt", GeometryType::Point, vec![]);
        pts.push(Some(Geometry::Point(Coord::new(180.05, 0.5))), vec![]);
        assert!(not_covered(&pts, &[0], &ds).errors.is_empty());
    }

    #[test]
    fn long_paths_are_checked_not_skipped() {
        // 10,000 segments zigzagging east without crossing...
        let mut path: Vec<Coord> = (0..10_001).map(|i| Coord::new(i as f64 * 0.0001, (i % 2) as f64 * 0.0001)).collect();
        let mut ds = Dataset::new("l", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![path.clone()])), vec![]);
        assert!(self_intersections(&ds, &[0]).errors.is_empty());
        // ...then back across the start.
        path.push(Coord::new(0.00015, -0.0001));
        path.push(Coord::new(0.00015, 0.0002));
        let mut ds = Dataset::new("l", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![path])), vec![]);
        let e = self_intersections(&ds, &[0]).errors;
        assert_eq!(e.len(), 1);
        assert!((e[0].at.x - 0.00015).abs() < 1e-6);
    }

    #[test]
    fn a_line_curling_onto_itself_does_not_dangle_there() {
        // A "6": the end comes back onto the line's own earlier part.
        let six =
            vec![Coord::new(0.0, 0.003), Coord::new(0.0, 0.0), Coord::new(0.001, 0.0), Coord::new(0.001, 0.001), Coord::new(0.0, 0.001)];
        let mut ds = Dataset::new("l", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![six])), vec![]);
        let d = dangles(&ds, &[0], 1.0);
        assert!(d.complete);
        assert_eq!(d.errors.len(), 1, "only the free top end dangles");
        assert!((d.errors[0].at.y - 0.003).abs() < 1e-9);
    }

    #[test]
    fn a_hairpin_ending_at_its_start_does_not_dangle() {
        // Three vertices: out, across and back to within a metre of the start.
        let hairpin = vec![Coord::new(0.0, 0.0), Coord::new(0.001, 0.0), Coord::new(0.000_005, 0.000_001)];
        let mut ds = Dataset::new("l", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![hairpin])), vec![]);
        assert!(dangles(&ds, &[0], 1.0).errors.is_empty());
    }
}
