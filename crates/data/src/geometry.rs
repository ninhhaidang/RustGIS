//! Geometry: points, polylines and polygons (each possibly multi-part), plus hit testing.

use rustgis_geom::{Coord, Extent, wgs84_length_m, wgs84_ring_perimeter_area_m2};
use serde::{Deserialize, Serialize};

/// Subdivisions of the label fallback grid; both endpoints are sampled.
const LABEL_GRID_STEPS: usize = 64;
// Prefix construction can shift by steps + 1, which must also fit in u128.
const _: () = assert!(LABEL_GRID_STEPS > 0 && LABEL_GRID_STEPS < (u128::BITS - 1) as usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GeometryType {
    Point,
    Polyline,
    Polygon,
    /// A table with no geometry.
    None,
}

impl GeometryType {
    pub fn label(self) -> &'static str {
        match self {
            GeometryType::Point => "Point",
            GeometryType::Polyline => "Polyline",
            GeometryType::Polygon => "Polygon",
            GeometryType::None => "Table",
        }
    }
}

/// What [`Geometry::wgs84_measure`] measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measure {
    /// A polygon's area, in square metres; holes subtract.
    Area,
    /// The length of a polygon's rings, holes included, in metres.
    Perimeter,
    /// A line's length, in metres.
    Length,
}

impl Measure {
    /// The measure named `key` (`area`, `perimeter`, `length`, any case).
    pub fn from_key(key: &str) -> Option<Measure> {
        [("area", Measure::Area), ("perimeter", Measure::Perimeter), ("length", Measure::Length)]
            .into_iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, m)| m)
    }

    /// The units it can be given in, by key, with the square metres or metres in one.
    pub fn units(self) -> &'static [(&'static str, f64)] {
        match self {
            Measure::Area => &[
                ("m2", 1.0),
                ("km2", 1e6),
                ("ha", 1e4),
                ("ac", rustgis_geom::SQUARE_METRES_PER_ACRE),
                ("ft2", rustgis_geom::SQUARE_METRES_PER_SQUARE_FOOT),
                ("mi2", rustgis_geom::SQUARE_METRES_PER_SQUARE_MILE),
            ],
            Measure::Perimeter | Measure::Length => {
                &[("m", 1.0), ("km", 1e3), ("ft", rustgis_geom::METRES_PER_FOOT), ("mi", rustgis_geom::METRES_PER_MILE)]
            }
        }
    }

    /// The square metres or metres in the unit `key` (any case), if it is one of [`Self::units`].
    pub fn unit(self, key: &str) -> Option<f64> {
        self.units().iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|u| u.1)
    }

    /// The kind of layer it measures.
    pub fn geometry_type(self) -> GeometryType {
        match self {
            Measure::Area | Measure::Perimeter => GeometryType::Polygon,
            Measure::Length => GeometryType::Polyline,
        }
    }
}

/// A geometry. Lines hold parts; polygons hold polygons of rings (first ring = exterior).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Geometry {
    Point(Coord),
    MultiPoint(Vec<Coord>),
    Line(Vec<Vec<Coord>>),
    Polygon(Vec<Vec<Vec<Coord>>>),
}

impl Geometry {
    pub fn kind(&self) -> GeometryType {
        match self {
            Geometry::Point(_) | Geometry::MultiPoint(_) => GeometryType::Point,
            Geometry::Line(_) => GeometryType::Polyline,
            Geometry::Polygon(_) => GeometryType::Polygon,
        }
    }

    /// Calls `f` with every coordinate.
    pub fn for_each_coord(&self, mut f: impl FnMut(Coord)) {
        match self {
            Geometry::Point(c) => f(*c),
            Geometry::MultiPoint(v) => v.iter().copied().for_each(f),
            Geometry::Line(parts) => parts.iter().flatten().copied().for_each(f),
            Geometry::Polygon(polys) => polys.iter().flatten().flatten().copied().for_each(f),
        }
    }

    /// `m` of this longitude/latitude geometry on the WGS 84 ellipsoid: what Calculate Geometry
    /// stores and `$area`, `$perimeter` and `$length` read. `None` for another kind of geometry,
    /// an empty one, or one whose coordinates aren't longitudes and latitudes (a shape drawn or
    /// buffered across the antimeridian may run to ±360°).
    pub fn wgs84_measure(&self, m: Measure) -> Option<f64> {
        let mut valid = self.coord_count() > 0;
        self.for_each_coord(|c| valid &= c.is_finite() && c.x.abs() <= 360.0 && c.y.abs() <= 90.0);
        if !valid {
            return None;
        }
        match (self, m) {
            // Full-turn and polar rings retain the documented exceptions to geoprocessing parity:
            // docs/user-guide/analysis.md (Calculate Geometry Attributes).
            (Geometry::Polygon(polys), Measure::Area) => Some(
                polys
                    .iter()
                    .map(|rings| {
                        rings
                            .iter()
                            .enumerate()
                            .map(|(i, r)| wgs84_ring_perimeter_area_m2(r).1 * if i == 0 { 1.0 } else { -1.0 })
                            .sum::<f64>()
                    })
                    .sum(),
            ),
            (Geometry::Polygon(polys), Measure::Perimeter) => Some(polys.iter().flatten().map(|r| wgs84_ring_perimeter_area_m2(r).0).sum()),
            (Geometry::Line(parts), Measure::Length) => Some(parts.iter().map(|r| wgs84_length_m(r)).sum()),
            _ => None,
        }
    }

    pub fn coord_count(&self) -> usize {
        let mut n = 0;
        self.for_each_coord(|_| n += 1);
        n
    }

    pub fn extent(&self) -> Extent {
        let mut e = Extent::EMPTY;
        self.for_each_coord(|c| e.expand(c));
        e
    }

    /// A copy with every coordinate transformed.
    pub fn map_coords(&self, f: &impl Fn(Coord) -> Coord) -> Geometry {
        match self {
            Geometry::Point(c) => Geometry::Point(f(*c)),
            Geometry::MultiPoint(v) => Geometry::MultiPoint(v.iter().map(|c| f(*c)).collect()),
            Geometry::Line(parts) => Geometry::Line(parts.iter().map(|p| p.iter().map(|c| f(*c)).collect()).collect()),
            Geometry::Polygon(polys) => {
                Geometry::Polygon(polys.iter().map(|rings| rings.iter().map(|r| r.iter().map(|c| f(*c)).collect()).collect()).collect())
            }
        }
    }

    /// Whether `p` hits this geometry within `tol` (same units as the coordinates).
    pub fn hit(&self, p: Coord, tol: f64) -> bool {
        match self {
            Geometry::Point(c) => dist2(*c, p) <= tol * tol,
            Geometry::MultiPoint(v) => v.iter().any(|c| dist2(*c, p) <= tol * tol),
            Geometry::Line(parts) => parts.iter().any(|part| seg_dist2(part, p) <= tol * tol),
            Geometry::Polygon(polys) => {
                polys.iter().any(|rings| polygon_contains(rings, p) || rings.iter().any(|r| seg_dist2(r, p) <= tol * tol))
            }
        }
    }

    /// Whether any part of the geometry lies inside the rectangle (box selection).
    pub fn intersects_extent(&self, e: &Extent) -> bool {
        if !self.extent().intersects(e) {
            return false;
        }
        let mut any = false;
        self.for_each_coord(|c| any |= e.contains(c));
        if any {
            return true;
        }
        // A polygon that contains the whole box, or a box edge intersecting a polygon ring.
        if let Geometry::Polygon(polys) = self {
            if polys.iter().any(|rings| polygon_contains(rings, e.center())) {
                return true;
            }
            return polys.iter().any(|rings| {
                rings.iter().any(|ring| {
                    let n = ring.len();
                    (0..n).any(|i| {
                        let (Some(a), Some(b)) = (ring.get(i), ring.get((i + 1) % n)) else { return false };
                        segment_intersects_extent(*a, *b, e)
                    })
                })
            });
        }
        // A line crossing the box without a vertex inside it.
        if let Geometry::Line(parts) = self {
            return parts.iter().any(|part| {
                part.windows(2).any(|w| match w {
                    [a, b] => segment_intersects_extent(*a, *b, e),
                    _ => false,
                })
            });
        }
        false
    }

    /// A point for a label or a pop-up: the point itself, the middle vertex of the longest line
    /// part, or the centroid of the largest valid polygon (moved inside it if the centroid falls out).
    ///
    /// Returns `None` if the geometry is empty or if all polygon parts contain non-finite coordinates.
    pub fn label_point(&self) -> Option<Coord> {
        match self {
            Geometry::Point(c) => Some(*c),
            Geometry::MultiPoint(v) => v.first().copied(),
            Geometry::Line(parts) => {
                let part = parts.iter().max_by_key(|p| p.len())?;
                part.get(part.len() / 2).copied()
            }
            Geometry::Polygon(polys) => {
                let rings = largest_part(polys)?;
                let outer = rings.first()?;
                let c = ring_centroid(outer)?;
                if polygon_contains(rings, c) {
                    return Some(c);
                }
                // Scan a horizontal line through the centroid for the widest inside span.
                let e = {
                    let mut e = Extent::EMPTY;
                    outer.iter().for_each(|c| e.expand(*c));
                    e
                };
                let steps = LABEL_GRID_STEPS;
                let mut best: Option<(f64, Coord)> = None;
                for i in 1..steps {
                    let y = grid_coord(e.min_y, e.max_y, i);
                    // Evaluate the same 65 grid points together. Each crossing toggles a
                    // prefix of x samples, replacing 65 repeated edge walks per scanline.
                    let mut inside = rings.first().map_or(0, |ring| label_grid_ring(ring, y, &e));
                    for hole in rings.iter().skip(1) {
                        inside &= !label_grid_ring(hole, y, &e);
                    }
                    let mut run_start: Option<f64> = None;
                    for j in 0..=steps {
                        let x = grid_coord(e.min_x, e.max_x, j);
                        match (inside & (1u128 << j) != 0, run_start) {
                            (true, None) => run_start = Some(x),
                            (false, Some(s)) => {
                                // Half widths and a halved midpoint stay finite for spans past
                                // f64::MAX; halving is exact, so ordinary runs rank the same.
                                let w = x * 0.5 - s * 0.5;
                                if best.is_none_or(|(bw, _)| w > bw) {
                                    let mid = (s + x) / 2.0;
                                    let mid = if mid.is_finite() { mid } else { s * 0.5 + x * 0.5 };
                                    best = Some((w, Coord::new(mid, y)));
                                }
                                run_start = None;
                            }
                            _ => {}
                        }
                    }
                }
                best.map(|(_, c)| c).or(Some(c))
            }
        }
    }
}

/// The `k`th of the label grid's sample positions between `min` and `max`. A span whose samples
/// would overflow `f64` is interpolated instead, so the samples stay finite; ordinary spans
/// keep the original expression and its exact bits.
fn grid_coord(min: f64, max: f64, k: usize) -> f64 {
    let span = max - min;
    if (span * LABEL_GRID_STEPS as f64).is_finite() {
        return min + span * k as f64 / LABEL_GRID_STEPS as f64;
    }
    let t = k as f64 / LABEL_GRID_STEPS as f64;
    min * (1.0 - t) + max * t
}

/// Exact even-odd results for the label algorithm's existing 0..=64 x samples.
/// Search uses the original floating-point expression and strict comparison, preserving boundaries.
fn label_grid_ring(ring: &[Coord], y: f64, extent: &Extent) -> u128 {
    if ring.len() < 3 {
        return 0;
    }
    let mut mask = 0u128;
    let Some(mut previous) = ring.last() else { return 0 };
    for point in ring {
        if (point.y > y) != (previous.y > y) {
            let crossing = crossing_x(*point, *previous, y);
            let (mut lo, mut hi) = (0usize, LABEL_GRID_STEPS + 1);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let x = grid_coord(extent.min_x, extent.max_x, mid);
                if x < crossing {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            mask ^= (1u128 << lo) - 1;
        }
        previous = point;
    }
    mask
}

/// One vertex of a geometry: its part (a line's part, a polygon's polygon; 0 for points), its
/// ring (a polygon's ring; 0 otherwise) and its place along the part or ring (a multipoint's
/// points are one part). A closed ring's repeated last point is not a vertex of its own: it
/// follows the first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VertexId {
    pub part: usize,
    pub ring: usize,
    pub index: usize,
}

impl VertexId {
    pub fn new(part: usize, ring: usize, index: usize) -> VertexId {
        VertexId { part, ring, index }
    }
}

/// How many vertices a path has: a closed ring's repeated last point doesn't count.
fn open_len(path: &[Coord], ring: bool) -> usize {
    match (path.first(), path.last()) {
        (Some(a), Some(b)) if ring && path.len() > 1 && a == b => path.len() - 1,
        _ => path.len(),
    }
}

impl Geometry {
    /// Calls `f` with each path that vertices lie on: its part and ring, and whether it is a polygon
    /// ring. Nothing is allocated, so a geometry of many parts costs no memory to walk.
    fn for_each_path(&self, mut f: impl FnMut(usize, usize, &[Coord], bool)) {
        match self {
            Geometry::Point(c) => f(0, 0, std::slice::from_ref(c), false),
            Geometry::MultiPoint(v) => f(0, 0, v.as_slice(), false),
            Geometry::Line(parts) => {
                for (i, p) in parts.iter().enumerate() {
                    f(i, 0, p.as_slice(), false);
                }
            }
            Geometry::Polygon(polys) => {
                for (i, rings) in polys.iter().enumerate() {
                    for (j, r) in rings.iter().enumerate() {
                        f(i, j, r.as_slice(), true);
                    }
                }
            }
        }
    }

    /// The path a vertex lies on, and whether it is a polygon ring. It picks out the same path that
    /// [`Geometry::for_each_path`] passes to its callback for these indices, so the two must change
    /// together.
    fn path(&self, part: usize, ring: usize) -> Option<(&[Coord], bool)> {
        match self {
            Geometry::Point(c) => (part == 0 && ring == 0).then_some((std::slice::from_ref(c), false)),
            Geometry::MultiPoint(v) => (part == 0 && ring == 0).then_some((v.as_slice(), false)),
            Geometry::Line(parts) => parts.get(part).filter(|_| ring == 0).map(|p| (p.as_slice(), false)),
            Geometry::Polygon(polys) => polys.get(part).and_then(|rings| rings.get(ring)).map(|r| (r.as_slice(), true)),
        }
    }

    /// Calls `f` with every vertex, a closed ring's repeated last point left out.
    pub fn for_each_vertex(&self, mut f: impl FnMut(VertexId, Coord)) {
        self.for_each_path(|part, ring, path, closed| {
            for (index, c) in path.iter().take(open_len(path, closed)).enumerate() {
                f(VertexId { part, ring, index }, *c);
            }
        });
    }

    /// Every vertex, a closed ring's repeated last point left out.
    pub fn vertices(&self) -> Vec<(VertexId, Coord)> {
        let mut out = Vec::new();
        self.for_each_vertex(|v, c| out.push((v, c)));
        out
    }

    /// Calls `f` with every edge of a line or polygon: the vertex it starts at and its two ends
    /// (a ring's last edge comes back to its first vertex). A vertex inserted on an edge goes
    /// after its start.
    pub fn for_each_edge(&self, mut f: impl FnMut(VertexId, Coord, Coord)) {
        if matches!(self, Geometry::Point(_) | Geometry::MultiPoint(_)) {
            return;
        }
        self.for_each_path(|part, ring, path, closed| {
            let n = open_len(path, closed);
            let count = if closed && n > 2 { n } else { n.saturating_sub(1) };
            for index in 0..count {
                if let (Some(a), Some(b)) = (path.get(index), path.get((index + 1) % n.max(1))) {
                    f(VertexId { part, ring, index }, *a, *b);
                }
            }
        });
    }

    /// Every edge of a line or polygon (see [`Geometry::for_each_edge`]).
    pub fn edges(&self) -> Vec<(VertexId, Coord, Coord)> {
        let mut out = Vec::new();
        self.for_each_edge(|v, a, b| out.push((v, a, b)));
        out
    }

    /// The vertex's position, if it has one.
    pub fn vertex(&self, v: VertexId) -> Option<Coord> {
        let (path, closed) = self.path(v.part, v.ring)?;
        (v.index < open_len(path, closed)).then(|| path.get(v.index).copied()).flatten()
    }

    /// The path a vertex lies on, mutably, and whether it is a polygon ring.
    fn path_mut(&mut self, part: usize, ring: usize) -> Option<(&mut Vec<Coord>, bool)> {
        match self {
            Geometry::Point(_) => None,
            Geometry::MultiPoint(v) => (part == 0 && ring == 0).then_some((v, false)),
            Geometry::Line(parts) => parts.get_mut(part).filter(|_| ring == 0).map(|p| (p, false)),
            Geometry::Polygon(polys) => polys.get_mut(part).and_then(|rings| rings.get_mut(ring)).map(|r| (r, true)),
        }
    }

    /// A copy with a vertex moved to `c` (a ring's closing point goes with its first vertex).
    pub fn move_vertex(&self, v: VertexId, c: Coord) -> Option<Geometry> {
        self.vertex(v)?;
        let mut g = self.clone();
        if let Geometry::Point(p) = &mut g {
            *p = c;
            return Some(g);
        }
        let (path, ring) = g.path_mut(v.part, v.ring)?;
        let closed = ring && open_len(path, true) < path.len();
        *path.get_mut(v.index)? = c;
        if closed && v.index == 0 {
            *path.last_mut()? = c;
        }
        Some(g)
    }

    /// A copy with a vertex at `c` inserted after `after`, on the edge it starts (lines and
    /// polygons only).
    pub fn insert_vertex(&self, after: VertexId, c: Coord) -> Option<Geometry> {
        if matches!(self, Geometry::Point(_) | Geometry::MultiPoint(_)) {
            return None;
        }
        self.vertex(after)?;
        let mut g = self.clone();
        let (path, _) = g.path_mut(after.part, after.ring)?;
        path.insert(after.index + 1, c);
        Some(g)
    }

    /// A copy without a vertex, refused when a line would keep fewer than two vertices, a
    /// polygon ring fewer than three or a multipoint none.
    pub fn delete_vertex(&self, v: VertexId) -> Result<Geometry, &'static str> {
        self.vertex(v).ok_or("no such vertex")?;
        let mut g = self.clone();
        let kind = g.kind();
        let multi = matches!(g, Geometry::MultiPoint(_));
        let Some((path, ring)) = g.path_mut(v.part, v.ring) else {
            return Err("a point has only one vertex");
        };
        let n = open_len(path, ring);
        let closed = ring && n < path.len();
        match kind {
            GeometryType::Point if multi && n <= 1 => return Err("a multipoint keeps at least one point"),
            GeometryType::Polyline if n <= 2 => return Err("a line keeps at least two vertices"),
            GeometryType::Polygon if n <= 3 => return Err("a polygon ring keeps at least three vertices"),
            _ => {}
        }
        path.remove(v.index);
        if closed && v.index == 0 {
            let first = *path.first().ok_or("no such vertex")?;
            *path.last_mut().ok_or("no such vertex")? = first;
        }
        Ok(g)
    }
}

fn dist2(a: Coord, b: Coord) -> f64 {
    (a.x - b.x).powi(2) + (a.y - b.y).powi(2)
}

/// Squared distance from `p` to the closest segment of a polyline.
pub fn seg_dist2(line: &[Coord], p: Coord) -> f64 {
    let mut best = f64::INFINITY;
    for w in line.windows(2) {
        let [a, b] = w else { continue };
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 { (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
        best = best.min(dist2(p, Coord::new(a.x + t * dx, a.y + t * dy)));
    }
    if line.len() == 1
        && let Some(a) = line.first()
    {
        best = dist2(*a, p);
    }
    best
}

/// Whether a line segment intersects an extent (including crossing through it or touching boundaries).
///
/// Returns `false` if `e` is empty, or if either coordinate is non-finite (`NaN` or `Infinity`)
/// unless an endpoint is already contained within `e`. Non-finite endpoints outside the extent
/// are treated as invalid and do not intersect.
pub fn segment_intersects_extent(p1: Coord, p2: Coord, e: &Extent) -> bool {
    if e.is_empty() {
        return false;
    }
    if e.contains(p1) || e.contains(p2) {
        return true;
    }
    if !p1.is_finite() || !p2.is_finite() {
        return false;
    }
    let min_x = p1.x.min(p2.x);
    let max_x = p1.x.max(p2.x);
    let min_y = p1.y.min(p2.y);
    let max_y = p1.y.max(p2.y);
    if min_x > e.max_x || max_x < e.min_x || min_y > e.max_y || max_y < e.min_y {
        return false;
    }

    let halve_axis = |v1: f64, v2: f64, min_e: f64, max_e: f64| {
        if v1.abs().max(v2.abs()) > f64::MAX / 2.0 { (v1 * 0.5, v2 * 0.5, min_e * 0.5, max_e * 0.5) } else { (v1, v2, min_e, max_e) }
    };
    let (p1x, p2x, min_x, max_x) = halve_axis(p1.x, p2.x, e.min_x, e.max_x);
    let (p1y, p2y, min_y, max_y) = halve_axis(p1.y, p2.y, e.min_y, e.max_y);

    let dx = p2x - p1x;
    let dy = p2y - p1y;
    let mut t0 = 0.0f64;
    let mut t1 = 1.0f64;

    let mut clip = |p: f64, q: f64| -> bool {
        if p == 0.0 {
            return q >= 0.0;
        }
        let r = q / p;
        if p < 0.0 {
            if r > t1 {
                return false;
            }
            if r > t0 {
                t0 = r;
            }
        } else {
            if r < t0 {
                return false;
            }
            if r < t1 {
                t1 = r;
            }
        }
        true
    };

    clip(-dx, p1x - min_x) && clip(dx, max_x - p1x) && clip(-dy, p1y - min_y) && clip(dy, max_y - p1y) && t0 <= t1
}

/// Even-odd point-in-ring test.
pub fn ring_contains(ring: &[Coord], p: Coord) -> bool {
    let mut inside = false;
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        let (Some(a), Some(b)) = (ring.get(i), ring.get(j)) else { break };
        if (a.y > p.y) != (b.y > p.y) && p.x < crossing_x(*a, *b, p.y) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Where the edge `a`–`b` crosses the horizontal line at `y`. When the plain formula overflows
/// (coordinates near `f64::MAX`), it is recomputed fraction first, halving only the axis whose
/// differences overflow, so huge rings still give a finite crossing and subnormal values on the
/// other axis keep their precision.
fn crossing_x(a: Coord, b: Coord, y: f64) -> f64 {
    let x = (b.x - a.x) * (y - a.y) / (b.y - a.y) + a.x;
    // An overflowed denominator can still give a finite (wrong) quotient, so it is checked too.
    if x.is_finite() && (b.y - a.y).is_finite() {
        return x;
    }
    let (ny, dy) = (y - a.y, b.y - a.y);
    let t = if ny.is_finite() && dy.is_finite() { ny / dy } else { (y * 0.5 - a.y * 0.5) / (b.y * 0.5 - a.y * 0.5) };
    let dx = b.x - a.x;
    if dx.is_finite() { dx * t + a.x } else { ((b.x * 0.5 - a.x * 0.5) * t + a.x * 0.5) * 2.0 }
}

/// Inside the exterior ring and outside every hole.
pub fn polygon_contains(rings: &[Vec<Coord>], p: Coord) -> bool {
    let mut it = rings.iter();
    match it.next() {
        Some(outer) if ring_contains(outer, p) => !it.any(|h| ring_contains(h, p)),
        _ => false,
    }
}

/// Signed planar area (shoelace; positive = counter-clockwise).
pub fn ring_area(ring: &[Coord]) -> f64 {
    let n = ring.len();
    if n < 3 {
        return 0.0;
    }
    let mut s = 0.0;
    for i in 0..n {
        let (Some(a), Some(b)) = (ring.get(i), ring.get((i + 1) % n)) else { break };
        s += a.x * b.y - b.x * a.y;
    }
    s / 2.0
}

/// A frame for ring arithmetic that cannot overflow: an axis whose coordinates exceed 1e100 is
/// scaled to about 1 and shifted so the first vertex sits at 0, keeping shoelace terms small.
/// Ordinary axes are left as they are, so their results keep their exact bits.
struct RingFrame {
    sx: f64,
    sy: f64,
    ox: f64,
    oy: f64,
}

impl RingFrame {
    /// The frame for `ring`, or `None` when it is empty or has a non-finite coordinate.
    fn new(ring: &[Coord]) -> Option<RingFrame> {
        let first = ring.first()?;
        let (mut max_x, mut max_y) = (0.0f64, 0.0f64);
        for c in ring {
            if !c.is_finite() {
                return None;
            }
            max_x = max_x.max(c.x.abs());
            max_y = max_y.max(c.y.abs());
        }
        let sx = if max_x > 1e100 { 1.0 / max_x } else { 1.0 };
        let sy = if max_y > 1e100 { 1.0 / max_y } else { 1.0 };
        let ox = if sx < 1.0 { first.x * sx } else { 0.0 };
        let oy = if sy < 1.0 { first.y * sy } else { 0.0 };
        Some(RingFrame { sx, sy, ox, oy })
    }

    /// `c` in frame units.
    fn local(&self, c: &Coord) -> (f64, f64) {
        (c.x * self.sx - self.ox, c.y * self.sy - self.oy)
    }

    /// A frame point back in coordinate units.
    fn world(&self, x: f64, y: f64) -> Coord {
        Coord::new((x + self.ox) / self.sx, (y + self.oy) / self.sy)
    }

    /// Signed unhalved shoelace sum of `ring` in frame units.
    fn shoelace_sum(&self, ring: &[Coord]) -> f64 {
        let n = ring.len();
        let mut a = 0.0f64;
        for i in 0..n {
            let (Some(p), Some(q)) = (ring.get(i), ring.get((i + 1) % n)) else { break };
            let ((px, py), (qx, qy)) = (self.local(p), self.local(q));
            a += px * qy - qx * py;
        }
        a
    }

    /// Signed shoelace area of `ring` in frame units.
    fn area(&self, ring: &[Coord]) -> f64 {
        self.shoelace_sum(ring) / 2.0
    }
}

/// The polygon part with the largest outer ring, skipping parts with no outer ring or with a
/// non-finite coordinate in any ring. Each part's area is computed once in its [`RingFrame`]. When
/// any part needs a scaled frame (coordinates above 1e100), parts are ranked by the logarithm of
/// their area, so huge or far-offset rings rank by their true size rather than an overflowed or
/// cancelled shoelace sum; otherwise by the plain shoelace area, bit for bit. Ties keep the last part.
fn largest_part(polys: &[Vec<Vec<Coord>>]) -> Option<&Vec<Vec<Coord>>> {
    let parts: Vec<(&Vec<Vec<Coord>>, RingFrame, f64, f64)> = polys
        .iter()
        .filter(|p| p.iter().all(|r| r.iter().all(|c| c.is_finite())))
        .filter_map(|p| {
            let outer = p.first()?;
            let f = RingFrame::new(outer)?;
            let sum = f.shoelace_sum(outer).abs();
            let a = sum / 2.0;
            Some((p, f, sum, a))
        })
        .collect();
    let scaled = parts.iter().any(|(_, f, _, _)| f.sx != 1.0 || f.sy != 1.0);
    let key = |(_, f, sum, a): &(&Vec<Vec<Coord>>, RingFrame, f64, f64)| {
        if scaled { sum.ln() - 2.0f64.ln() - f.sx.ln() - f.sy.ln() } else { *a }
    };
    parts.iter().max_by(|x, y| key(x).partial_cmp(&key(y)).unwrap_or(std::cmp::Ordering::Equal)).map(|(p, _, _, _)| *p)
}

/// Planar centroid of a ring (vertex average when degenerate).
///
/// Returns `None` if the ring contains non-finite coordinates, has no vertices, or produces a non-finite centroid.
/// Axes with coordinates above 1e100 are computed in a scaled frame centred on the first vertex,
/// so they neither overflow nor lose a small ring to cancellation.
/// For degenerate rings with zero or near-zero planar area, returns the unweighted average
/// of vertices, excluding the duplicate closing vertex for closed rings.
pub fn ring_centroid(ring: &[Coord]) -> Option<Coord> {
    let frame = RingFrame::new(ring)?;
    let n = ring.len();
    let pts = if n > 1 && ring.first() == ring.last() { ring.get(..n - 1).unwrap_or(ring) } else { ring };
    let a = frame.area(ring);

    // The plain 1e-12 threshold, in frame units. It may underflow to 0 for huge rings, which then
    // count as degenerate only at an exact zero area.
    if a == 0.0 || a.abs() < 1e-12 * frame.sx * frame.sy {
        let count = pts.len() as f64;
        let (sx, sy) = pts.iter().fold((0.0, 0.0), |(x, y), c| {
            let (lx, ly) = frame.local(c);
            (x + lx, y + ly)
        });
        let c = frame.world(sx / count, sy / count);
        return c.is_finite().then_some(c);
    }

    let (mut cx, mut cy) = (0.0, 0.0);
    for i in 0..n {
        let (Some(p), Some(q)) = (ring.get(i), ring.get((i + 1) % n)) else { break };
        let ((px, py), (qx, qy)) = (frame.local(p), frame.local(q));
        let f = px * qy - qx * py;
        cx += (px + qx) * f;
        cy += (py + qy) * f;
    }
    let c = frame.world(cx / (6.0 * a), cy / (6.0 * a));
    c.is_finite().then_some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: f64, y0: f64, s: f64) -> Vec<Coord> {
        vec![Coord::new(x0, y0), Coord::new(x0 + s, y0), Coord::new(x0 + s, y0 + s), Coord::new(x0, y0 + s), Coord::new(x0, y0)]
    }

    #[test]
    fn polygon_with_hole() {
        let g = Geometry::Polygon(vec![vec![square(0.0, 0.0, 10.0), square(4.0, 4.0, 2.0)]]);
        assert!(g.hit(Coord::new(1.0, 1.0), 0.0));
        assert!(!g.hit(Coord::new(5.0, 5.0), 0.0));
        assert!(g.hit(Coord::new(5.0, 5.0), 1.5)); // near the hole's edge
        let lp = g.label_point().unwrap();
        assert!(polygon_contains(&[square(0.0, 0.0, 10.0), square(4.0, 4.0, 2.0)], lp));
    }

    #[test]
    fn line_hit_and_box() {
        let g = Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(10.0, 0.0)]]);
        assert!(g.hit(Coord::new(5.0, 0.5), 1.0));
        assert!(!g.hit(Coord::new(5.0, 2.0), 1.0));
        assert!(g.intersects_extent(&Extent::new(4.0, -1.0, 6.0, 1.0)));
        assert!(!g.intersects_extent(&Extent::new(4.0, 1.0, 6.0, 2.0)));

        // Polygon crossing the box whose center is outside and vertices outside
        let poly = Geometry::Polygon(vec![vec![vec![
            Coord::new(-10.0, 7.0),
            Coord::new(20.0, 7.0),
            Coord::new(20.0, 20.0),
            Coord::new(-10.0, 20.0),
        ]]]);
        // Box is [0, 10] x [0, 10], center is (5, 5) which is outside y in [7, 20]
        assert!(poly.intersects_extent(&Extent::new(0.0, 0.0, 10.0, 10.0)));

        // Diagonal segment crossing through box corners
        assert!(segment_intersects_extent(Coord::new(-5.0, -5.0), Coord::new(15.0, 15.0), &Extent::new(0.0, 0.0, 10.0, 10.0)));
        // Segment collinear with box top edge
        assert!(segment_intersects_extent(Coord::new(-5.0, 10.0), Coord::new(15.0, 10.0), &Extent::new(0.0, 0.0, 10.0, 10.0)));
        // Segment outside box
        assert!(!segment_intersects_extent(Coord::new(-5.0, 12.0), Coord::new(15.0, 12.0), &Extent::new(0.0, 0.0, 10.0, 10.0)));
        // An endpoint inside the box intersects even if the other endpoint is non-finite
        assert!(segment_intersects_extent(Coord::new(5.0, 5.0), Coord::new(f64::NAN, 5.0), &Extent::new(0.0, 0.0, 10.0, 10.0)));
        assert!(segment_intersects_extent(Coord::new(f64::INFINITY, 0.0), Coord::new(5.0, 5.0), &Extent::new(0.0, 0.0, 10.0, 10.0)));
        // Non-finite segment outside the box or empty extent never intersects
        assert!(!segment_intersects_extent(Coord::new(f64::NAN, -1.0), Coord::new(5.0, 15.0), &Extent::new(0.0, 0.0, 10.0, 10.0)));
        assert!(!segment_intersects_extent(Coord::new(-5.0, -5.0), Coord::new(f64::INFINITY, 15.0), &Extent::new(0.0, 0.0, 10.0, 10.0)));
        assert!(!segment_intersects_extent(Coord::new(-5.0, 5.0), Coord::new(f64::INFINITY, 5.0), &Extent::new(0.0, 0.0, 10.0, 10.0)));
        // Huge coordinates exceeding normal span: non-crossing returns false, crossing returns true
        assert!(!segment_intersects_extent(Coord::new(-1.7e308, 20.0), Coord::new(1.7e308, 20.0), &Extent::new(0.0, 0.0, 10.0, 10.0)));
        // Passes bounding-box check; dx = 3.4e308 would overflow without halving and wrongly report a hit
        assert!(!segment_intersects_extent(Coord::new(-1.7e308, 5.0), Coord::new(1.7e308, 20.0), &Extent::new(0.0, 0.0, 10.0, 10.0)));
        assert!(segment_intersects_extent(Coord::new(-1.7e308, 5.0), Coord::new(1.7e308, 5.0), &Extent::new(0.0, 0.0, 10.0, 10.0)));
        // Subnormal values preserve precision without collapsing to zero
        assert!(!segment_intersects_extent(Coord::new(0.0, 5e-324), Coord::new(5e-324, 0.0), &Extent::new(0.0, 0.0, 0.0, 0.0)));
        assert!(segment_intersects_extent(Coord::new(1e301, 0.0), Coord::new(-1e301, 1e-323), &Extent::new(0.0, 0.0, 10.0, 5e-324)));
        assert!(!segment_intersects_extent(Coord::new(1.7e308, 5e-324), Coord::new(-1.7e308, 5e-324), &Extent::new(0.0, 0.0, 0.0, 0.0)));
        assert!(!segment_intersects_extent(Coord::new(0.0, 0.0), Coord::new(10.0, 10.0), &Extent::EMPTY));
    }

    #[test]
    fn degenerate_ring_centroid_is_rotation_independent_and_ignores_duplicate_closing_vertex() {
        let closed_a = vec![Coord::new(0.0, 0.0), Coord::new(10.0, 0.0), Coord::new(0.0, 0.0)];
        let closed_b = vec![Coord::new(10.0, 0.0), Coord::new(0.0, 0.0), Coord::new(10.0, 0.0)];
        assert_eq!(ring_centroid(&closed_a), Some(Coord::new(5.0, 0.0)));
        assert_eq!(ring_centroid(&closed_b), Some(Coord::new(5.0, 0.0)));
        let open = vec![Coord::new(0.0, 0.0), Coord::new(10.0, 0.0)];
        assert_eq!(ring_centroid(&open), Some(Coord::new(5.0, 0.0)));
        let non_finite = vec![Coord::new(f64::NAN, 0.0), Coord::new(10.0, 0.0), Coord::new(f64::NAN, 0.0)];
        assert_eq!(ring_centroid(&non_finite), None);
        assert_eq!(ring_centroid(&[]), None);
        let huge_ring = vec![Coord::new(1e160, 0.0), Coord::new(0.0, 1e160), Coord::new(-1e160, 0.0), Coord::new(1e160, 0.0)];
        let c = ring_centroid(&huge_ring).unwrap();
        assert!(c.x.abs() < 1e145 && (c.y - 1e160 / 3.0).abs() / 1e160 < 1e-12);
        let max_ring = vec![Coord::new(f64::MAX, 0.0), Coord::new(f64::MAX, 0.0), Coord::new(f64::MAX, 0.0)];
        assert_eq!(ring_centroid(&max_ring), Some(Coord::new(f64::MAX, 0.0)));
        let subnormal_ring = vec![Coord::new(5e-324, 0.0), Coord::new(0.0, 5e-324), Coord::new(-5e-324, 0.0), Coord::new(5e-324, 0.0)];
        assert!(ring_centroid(&subnormal_ring).is_some());
        let mixed_ring = vec![Coord::new(1e200, 1e-200), Coord::new(1e200, 2e-200), Coord::new(1e200, 3e-200), Coord::new(1e200, 1e-200)];
        let c_mixed = ring_centroid(&mixed_ring).unwrap();
        assert!((c_mixed.x - 1e200).abs() / 1e200 < 1e-12);
        assert!((c_mixed.y - 2e-200).abs() / 2e-200 < 1e-12);
    }

    #[test]
    fn ring_centroid_scale_threshold_continuity() {
        let p_below =
            vec![Coord::new(0.0, 0.0), Coord::new(1e99, 0.0), Coord::new(1e99, 1e99), Coord::new(0.0, 1e99), Coord::new(0.0, 0.0)];
        let p_above =
            vec![Coord::new(0.0, 0.0), Coord::new(1e101, 0.0), Coord::new(1e101, 1e101), Coord::new(0.0, 1e101), Coord::new(0.0, 0.0)];
        let c_below = ring_centroid(&p_below).unwrap();
        let c_above = ring_centroid(&p_above).unwrap();
        assert!((c_below.x - 5e98).abs() / 5e98 < 1e-12);
        assert!((c_above.x - 5e100).abs() / 5e100 < 1e-12);
    }

    #[test]
    fn huge_rings_keep_precision_and_area_weighting() {
        // A ring of size 1e90 sitting near 1e101: the scaled frame is centred on the first vertex,
        // so the shoelace terms do not cancel to noise.
        let c = ring_centroid(&square(1e101, 1e101, 1e90)).unwrap();
        let want = 1e101 + 5e89;
        assert!((c.x - want).abs() < 1e88 && (c.y - want).abs() < 1e88, "{c:?}");
        // A tiny but nonzero scaled area is still area-weighted, not a vertex average.
        let kite = vec![Coord::new(0.0, 0.0), Coord::new(1e-110, 0.0), Coord::new(1e200, 1e200), Coord::new(0.0, 1e-110)];
        let c = ring_centroid(&kite).unwrap();
        assert!((c.x / (1e200 / 3.0) - 1.0).abs() < 1e-3 && (c.y / (1e200 / 3.0) - 1.0).abs() < 1e-3, "{c:?}");
    }

    #[test]
    fn ring_contains_handles_huge_coordinates() {
        let ring = vec![Coord::new(1e200, 1e200), Coord::new(0.0, 0.0), Coord::new(0.0, 1e200), Coord::new(1e200, 1e200)];
        assert!(!ring_contains(&ring, Coord::new(2e200, 5e199)));
        assert!(ring_contains(&ring, Coord::new(1e199, 5e199)));
        // Halving only the overflowing axis keeps a subnormal y fraction exact.
        let (s, m) = (f64::from_bits(1), f64::MAX);
        let mixed = vec![Coord::new(m, 4.0 * s), Coord::new(-m, 0.0), Coord::new(m, 0.0)];
        assert!(!ring_contains(&mixed, Coord::new(-0.75 * m, s)));
    }

    #[test]
    fn concave_huge_polygon_labels_inside() {
        // A U shape near 1e200 whose centroid falls in the notch, so the scanline picks the label.
        let u = [(0.0, 0.0), (3.0, 0.0), (3.0, 3.0), (2.0, 3.0), (2.0, 1.0), (1.0, 1.0), (1.0, 3.0), (0.0, 3.0), (0.0, 0.0)];
        let ring: Vec<Coord> = u.iter().map(|&(x, y)| Coord::new(x * 1e200, y * 1e200)).collect();
        assert!(!ring_contains(&ring, ring_centroid(&ring).unwrap()));
        let c = Geometry::Polygon(vec![vec![ring.clone()]]).label_point().unwrap();
        assert!(ring_contains(&ring, c), "{c:?}");
    }

    #[test]
    fn far_offset_small_part_does_not_outrank_a_larger_one() {
        // At 1e150 the plain shoelace terms are about 1e300, so its sum is rounding noise.
        let far = vec![square(1e150, 1e150, 1.0)];
        let near = vec![square(0.0, 0.0, 1e10)];
        for parts in [vec![far.clone(), near.clone()], vec![near, far]] {
            let c = Geometry::Polygon(parts).label_point().unwrap();
            assert!(c.x < 1e11 && c.y < 1e11, "{c:?}");
        }
    }

    #[test]
    fn label_grid_spanning_beyond_f64_max_stays_inside() {
        // A U shape spanning -1.5e308..1.5e308 on x: its width overflows f64, and its centroid falls in the notch.
        let m = 1.5e308;
        let u = [(-m, 0.0), (m, 0.0), (m, 3.0), (m / 3.0, 3.0), (m / 3.0, 1.0), (-m / 3.0, 1.0), (-m / 3.0, 3.0), (-m, 3.0), (-m, 0.0)];
        let ring: Vec<Coord> = u.iter().map(|&(x, y)| Coord::new(x, y)).collect();
        assert!(!ring_contains(&ring, ring_centroid(&ring).unwrap()));
        let c = Geometry::Polygon(vec![vec![ring.clone()]]).label_point().unwrap();
        assert!(ring_contains(&ring, c), "{c:?}");
    }

    #[test]
    fn huge_label_grid_and_crossings_stay_finite_and_inside() {
        // Span 2e307 is finite, but span * k overflows for later samples.
        let m = 1e307;
        let u = [(-m, 0.0), (m, 0.0), (m, 3.0), (m / 3.0, 3.0), (m / 3.0, 1.0), (-m / 3.0, 1.0), (-m / 3.0, 3.0), (-m, 3.0), (-m, 0.0)];
        let ring: Vec<Coord> = u.iter().map(|&(x, y)| Coord::new(x, y)).collect();
        let c = Geometry::Polygon(vec![vec![ring.clone()]]).label_point().unwrap();
        assert!(c.is_finite() && ring_contains(&ring, c), "{c:?}");
        // A hole leaves a run whose endpoints sum past f64::MAX.
        let outer = vec![
            Coord::new(-1.5e308, 0.0),
            Coord::new(1.5e308, 0.0),
            Coord::new(1.5e308, 3.0),
            Coord::new(-1.5e308, 3.0),
            Coord::new(-1.5e308, 0.0),
        ];
        let hole = vec![
            Coord::new(-1.4e308, 0.01),
            Coord::new(1e308, 0.01),
            Coord::new(1e308, 2.99),
            Coord::new(-1.4e308, 2.99),
            Coord::new(-1.4e308, 0.01),
        ];
        let rings = vec![outer, hole];
        let c = Geometry::Polygon(vec![rings.clone()]).label_point().unwrap();
        assert!(c.is_finite() && polygon_contains(&rings, c), "{c:?}");
        // An edge whose y difference overflows: the crossing at y = 0 is x = 0.5, not 0.
        let tri = vec![Coord::new(0.0, -f64::MAX), Coord::new(1.0, f64::MAX), Coord::new(1.0, 0.0)];
        assert!(!ring_contains(&tri, Coord::new(0.25, 0.0)));
        assert!(ring_contains(&tri, Coord::new(0.75, 0.0)));
    }

    #[test]
    fn ringless_part_does_not_shadow_a_valid_part() {
        let flat = vec![vec![Coord::new(0.0, 0.0), Coord::new(10.0, 0.0), Coord::new(0.0, 0.0)]];
        assert_eq!(Geometry::Polygon(vec![flat.clone(), vec![]]).label_point(), Some(Coord::new(5.0, 0.0)));
        assert_eq!(Geometry::Polygon(vec![flat, vec![vec![]]]).label_point(), Some(Coord::new(5.0, 0.0)));
    }

    #[test]
    fn largest_part_wins_when_its_plain_area_overflows() {
        let small = vec![square(0.0, 0.0, 10.0)];
        let huge = vec![square(1e160, 1e160, 1e160)];
        assert!(!ring_area(&square(1e160, 1e160, 1e160)).is_finite());
        for parts in [vec![small.clone(), huge.clone()], vec![huge, small]] {
            let c = Geometry::Polygon(parts).label_point().unwrap();
            assert!(c.x > 1e160 && c.y > 1e160, "{c:?}");
        }
    }

    #[test]
    fn largest_part_with_tiny_framed_shoelace_sum_does_not_underflow() {
        let triangle = vec![vec![Coord::new(0.0, 0.0), Coord::new(1e308, 0.0), Coord::new(0.0, f64::from_bits(1)), Coord::new(0.0, 0.0)]];
        let tiny_square = vec![square(0.0, 0.0, 1e-8)];
        let polys = vec![tiny_square.clone(), triangle.clone()];
        let best = largest_part(&polys);
        assert_eq!(best, Some(&triangle));

        let polys_rev = vec![triangle.clone(), tiny_square];
        let best_rev = largest_part(&polys_rev);
        assert_eq!(best_rev, Some(&triangle));
    }

    #[test]
    fn polygon_with_non_finite_vertex_returns_none_label() {
        // Polygons with non-finite vertices cannot compute a valid centroid or label point
        let poly = Geometry::Polygon(vec![vec![vec![
            Coord::new(0.0, 0.0),
            Coord::new(10.0, 0.0),
            Coord::new(f64::NAN, 5.0),
            Coord::new(10.0, 10.0),
            Coord::new(0.0, 10.0),
            Coord::new(0.0, 0.0),
        ]]]);
        assert_eq!(poly.label_point(), None);
    }

    #[test]
    fn multipolygon_with_one_corrupt_part_labels_valid_part() {
        let valid_square = vec![square(0.0, 0.0, 10.0)];
        let corrupt_poly = vec![vec![Coord::new(0.0, 0.0), Coord::new(f64::NAN, 0.0), Coord::new(10.0, 10.0), Coord::new(0.0, 0.0)]];
        let mp1 = Geometry::Polygon(vec![valid_square.clone(), corrupt_poly.clone()]);
        let mp2 = Geometry::Polygon(vec![corrupt_poly, valid_square.clone()]);
        assert_eq!(mp1.label_point(), Some(Coord::new(5.0, 5.0)));
        assert_eq!(mp2.label_point(), Some(Coord::new(5.0, 5.0)));

        // A polygon with a corrupt hole is also rejected in favor of a valid polygon
        let poly_corrupt_hole = vec![square(0.0, 0.0, 20.0), vec![Coord::new(5.0, 5.0), Coord::new(f64::NAN, 5.0), Coord::new(5.0, 5.0)]];
        let mp_hole = Geometry::Polygon(vec![poly_corrupt_hole, valid_square]);
        assert_eq!(mp_hole.label_point(), Some(Coord::new(5.0, 5.0)));
    }

    #[test]
    fn vertex_editing() {
        // A closed square ring: four vertices, the closing point following the first.
        let g = Geometry::Polygon(vec![vec![square(0.0, 0.0, 10.0), square(4.0, 4.0, 2.0)]]);
        assert_eq!(g.vertices().len(), 8);
        assert_eq!(g.edges().len(), 8);
        let first = VertexId::new(0, 0, 0);
        let moved = g.move_vertex(first, Coord::new(-1.0, -1.0)).unwrap();
        let Geometry::Polygon(p) = &moved else { panic!("not a polygon") };
        assert_eq!(p[0][0][0], Coord::new(-1.0, -1.0));
        assert_eq!(p[0][0][4], Coord::new(-1.0, -1.0), "the ring stays closed");
        // Inserting on the ring's last edge (back to the first vertex) keeps it closed.
        let ins = g.insert_vertex(VertexId::new(0, 1, 3), Coord::new(3.0, 5.0)).unwrap();
        let Geometry::Polygon(p) = &ins else { panic!("not a polygon") };
        assert_eq!(p[0][1].len(), 6);
        assert_eq!(p[0][1][4], Coord::new(3.0, 5.0));
        assert_eq!(p[0][1].first(), p[0][1].last());
        // Deleting the first vertex re-closes the ring; a triangle can't lose one.
        let del = g.delete_vertex(first).unwrap();
        let Geometry::Polygon(p) = &del else { panic!("not a polygon") };
        assert_eq!(p[0][0].len(), 4);
        assert_eq!(p[0][0].first(), p[0][0].last());
        assert!(del.delete_vertex(first).is_err());
        // Lines keep two vertices; points can't lose theirs.
        let line = Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0), Coord::new(2.0, 0.0)]]);
        assert_eq!(line.edges().len(), 2);
        let two = line.delete_vertex(VertexId::new(0, 0, 1)).unwrap();
        assert!(two.delete_vertex(VertexId::new(0, 0, 0)).is_err());
        assert!(Geometry::Point(Coord::new(0.0, 0.0)).delete_vertex(VertexId::new(0, 0, 0)).is_err());
        assert!(Geometry::Point(Coord::new(0.0, 0.0)).insert_vertex(VertexId::new(0, 0, 0), Coord::new(1.0, 1.0)).is_none());
        let mp = Geometry::MultiPoint(vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]);
        assert_eq!(mp.delete_vertex(VertexId::new(0, 0, 1)).unwrap(), Geometry::MultiPoint(vec![Coord::new(0.0, 0.0)]));
        // Vertices that aren't there.
        for v in [VertexId::new(0, 0, 9), VertexId::new(3, 0, 0), VertexId::new(0, 0, 4), VertexId::new(0, 5, 0)] {
            assert!(g.move_vertex(v, Coord::new(0.0, 0.0)).is_none(), "{v:?}");
            assert!(g.insert_vertex(v, Coord::new(0.0, 0.0)).is_none(), "{v:?}");
            assert!(g.delete_vertex(v).is_err(), "{v:?}");
        }
        assert!(line.move_vertex(VertexId::new(0, 1, 0), Coord::new(0.0, 0.0)).is_none());
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        assert!(!ring_contains(&[], Coord::new(0.0, 0.0)));
        assert_eq!(ring_area(&[Coord::new(1.0, 1.0)]), 0.0);
        assert!(Geometry::Polygon(vec![]).label_point().is_none());
        assert!(Geometry::Line(vec![vec![]]).label_point().is_none());
    }
}

#[cfg(test)]
mod label_scanline_tests {
    use super::*;

    /// Compare every grid sample against the existing even-odd point-in-polygon predicate.
    #[test]
    fn label_scanlines_match_every_legacy_grid_test_including_holes_and_large_origins() {
        for (offset, scale) in [(0.0, 1.0), (41_000_000.0, 1.0), (41_000_000.0, 1e-4), (-80.0, 10000.0)] {
            for variant in 0..24 {
                let mut outer = Vec::new();
                for i in 0..19 {
                    let a = i as f64 * std::f64::consts::TAU / 19.0;
                    let r = if (i + variant) % 3 == 0 { 0.23 } else { 1.0 };
                    outer.push(Coord::new(offset + a.cos() * r * scale, offset + a.sin() * r * scale));
                }
                if variant % 2 == 0 {
                    outer.push(outer[0]);
                }
                let hole = |x: f64| {
                    vec![
                        Coord::new(offset + x * scale, offset - 0.2 * scale),
                        Coord::new(offset + (x + 0.3) * scale, offset - 0.2 * scale),
                        Coord::new(offset + (x + 0.3) * scale, offset + 0.3 * scale),
                        Coord::new(offset + x * scale, offset + 0.3 * scale),
                    ]
                };
                let rings = vec![outer, hole(-0.2), hole(0.0)];
                let mut extent = Extent::EMPTY;
                for p in &rings[0] {
                    extent.expand(*p);
                }
                for i in 1..64 {
                    let y = extent.min_y + extent.height() * i as f64 / 64.0;
                    let mut bits = label_grid_ring(&rings[0], y, &extent);
                    for ring in rings.iter().skip(1) {
                        bits &= !label_grid_ring(ring, y, &extent);
                    }
                    for j in 0..=64 {
                        let x = extent.min_x + extent.width() * j as f64 / 64.0;
                        assert_eq!(
                            bits & (1u128 << j) != 0,
                            polygon_contains(&rings, Coord::new(x, y)),
                            "origin={offset} scale={scale} variant={variant} y={i} x={j}"
                        );
                    }
                }
            }
        }
        assert_eq!(label_grid_ring(&[], 0.0, &Extent::EMPTY), 0);
    }

    /// Frozen 64-step oracle; keep its constants independent of the optimized implementation.
    fn reference_label_point(geometry: &Geometry) -> Option<Coord> {
        match geometry {
            Geometry::Point(c) => Some(*c),
            Geometry::MultiPoint(v) => v.first().copied(),
            Geometry::Line(parts) => {
                let part = parts.iter().max_by_key(|p| p.len())?;
                part.get(part.len() / 2).copied()
            }
            Geometry::Polygon(polys) => {
                let rings = polys.iter().max_by(|a, b| {
                    let aa = a.first().map(|r| ring_area(r).abs()).unwrap_or(0.0);
                    let bb = b.first().map(|r| ring_area(r).abs()).unwrap_or(0.0);
                    aa.partial_cmp(&bb).unwrap_or(std::cmp::Ordering::Equal)
                })?;
                let outer = rings.first()?;
                let c = ring_centroid(outer)?;
                if polygon_contains(rings, c) {
                    return Some(c);
                }
                // Scan a horizontal line through the centroid for the widest inside span.
                let e = {
                    let mut e = Extent::EMPTY;
                    outer.iter().for_each(|c| e.expand(*c));
                    e
                };
                let steps = 64;
                let mut best: Option<(f64, Coord)> = None;
                for i in 1..steps {
                    let y = e.min_y + e.height() * i as f64 / steps as f64;
                    let mut run_start: Option<f64> = None;
                    for j in 0..=steps {
                        let x = e.min_x + e.width() * j as f64 / steps as f64;
                        let inside = polygon_contains(rings, Coord::new(x, y));
                        match (inside, run_start) {
                            (true, None) => run_start = Some(x),
                            (false, Some(s)) => {
                                let w = x - s;
                                if best.is_none_or(|(bw, _)| w > bw) {
                                    best = Some((w, Coord::new((s + x) / 2.0, y)));
                                }
                                run_start = None;
                            }
                            _ => {}
                        }
                    }
                }
                best.map(|(_, c)| c).or(Some(c))
            }
        }
    }

    /// Compare the full public label result, not just the scanline's membership bits.
    #[test]
    fn label_points_match_reference_bits() {
        let ring = |v: &[(f64, f64)]| v.iter().map(|&(x, y)| Coord::new(x, y)).collect::<Vec<_>>();
        let square = ring(&[(-4.0, -4.0), (4.0, -4.0), (4.0, 4.0), (-4.0, 4.0)]);
        let hole = ring(&[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]);
        let concave = ring(&[(0.0, 0.0), (8.0, 0.0), (8.0, 8.0), (6.0, 8.0), (6.0, 2.0), (2.0, 2.0), (2.0, 8.0), (0.0, 8.0)]);
        assert!(!polygon_contains(std::slice::from_ref(&concave), ring_centroid(&concave).unwrap()));
        assert!(!polygon_contains(&[square.clone(), hole.clone()], ring_centroid(&square).unwrap()));
        let mut cases = vec![
            Geometry::Point(Coord::new(-0.0, 0.0)),
            Geometry::MultiPoint(vec![]),
            Geometry::MultiPoint(vec![Coord::new(1.0, 2.0)]),
            Geometry::Line(vec![]),
            Geometry::Line(vec![vec![], ring(&[(0.0, 0.0), (1.0, 2.0), (3.0, 4.0)])]),
            Geometry::Polygon(vec![]),
            Geometry::Polygon(vec![vec![]]),
            Geometry::Polygon(vec![vec![vec![]]]),
        ];
        let shifted: Vec<_> = square.iter().map(|c| Coord::new(c.x + 20.0, c.y)).collect();
        cases.push(Geometry::Polygon(vec![vec![square.clone(), hole.clone()], vec![shifted.clone()]]));
        cases.push(Geometry::Polygon(vec![vec![shifted], vec![square.clone(), hole.clone()]]));
        let fixtures = vec![
            vec![square.clone()],
            vec![concave],
            vec![square.clone(), hole.clone()],
            vec![square.clone(), hole.clone(), hole.clone()],
            vec![square.clone(), square.clone()], // No inside samples: retain the centroid fallback.
            vec![ring(&[(0.0, 0.0)])],
            vec![ring(&[(0.0, 0.0), (1.0, 0.0), (2.0, 0.0)])],
            vec![ring(&[(-0.0, 0.0), (0.0, -0.0), (0.0, 0.0)])],
        ];
        for (offset, scale) in [(0.0, 1.0), (41_000_000.0, 1.0), (41_000_000.0, 1e-4), (-80.0, 10000.0)] {
            for fixture in &fixtures {
                for reversed in [false, true] {
                    for closed in [false, true] {
                        let rings: Vec<Vec<Coord>> = fixture
                            .iter()
                            .map(|r| {
                                let mut r: Vec<_> = r.iter().map(|c| Coord::new(offset + c.x * scale, offset + c.y * scale)).collect();
                                if reversed {
                                    r.reverse();
                                }
                                if closed && let Some(&first) = r.first() {
                                    r.push(first);
                                }
                                r
                            })
                            .collect();
                        cases.push(Geometry::Polygon(vec![rings.clone()]));
                        // Equal-area polygons retain the legacy selection order; a smaller polygon precedes them.
                        cases.push(Geometry::Polygon(vec![vec![hole.clone()], rings.clone(), rings]));
                    }
                }
            }
        }
        for variant in 0..24 {
            let outer: Vec<_> = (0..19)
                .map(|i| {
                    let a = i as f64 * std::f64::consts::TAU / 19.0;
                    let r = if (i + variant) % 3 == 0 { 0.23 } else { 1.0 };
                    Coord::new(a.cos() * r, a.sin() * r)
                })
                .collect();
            cases.push(Geometry::Polygon(vec![vec![outer, hole.iter().map(|c| Coord::new(c.x * 0.2, c.y * 0.2)).collect()]]));
        }
        let mut fast = 0;
        let mut fallback = 0;
        for (case, geometry) in cases.iter().enumerate() {
            if let Geometry::Polygon(polys) = geometry
                && let Some(rings) = polys.iter().max_by(|a, b| {
                    let area = |r: &[Vec<Coord>]| r.first().map_or(0.0, |r| ring_area(r).abs());
                    area(a).partial_cmp(&area(b)).unwrap_or(std::cmp::Ordering::Equal)
                })
                && let Some(c) = rings.first().and_then(|r| ring_centroid(r))
            {
                if polygon_contains(rings, c) {
                    fast += 1;
                } else {
                    fallback += 1;
                }
            }
            let bits = |c: Option<Coord>| c.map(|c| (c.x.to_bits(), c.y.to_bits()));
            assert_eq!(bits(geometry.label_point()), bits(reference_label_point(geometry)), "case={case}");
        }
        assert!(fast > 0 && fallback > 0, "both centroid and scanline paths must execute");
        println!("label cases: {}, centroid={fast}, scanline={fallback}", cases.len());
    }
}
