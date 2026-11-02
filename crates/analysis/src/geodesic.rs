//! Geodesic buffers: a distance measured on the Earth, wherever the feature is and however far it
//! spans (#394).
//!
//! A feature is buffered in an azimuthal equidistant projection centred on it, where distances
//! from the centre are true and the error elsewhere grows with the square of the distance from it.
//! So a small feature is buffered in one piece, and a long line or a large polygon's outline in
//! stretches of a few hundred kilometres, each in its own projection, and the pieces are joined.
//!
//! The outline keeps its distance between its vertices too, not only at them (#1159): an edge
//! drawn straight in the plane is split before it comes back to lon/lat wherever a straight edge
//! there would stray from it ([`OUTLINE_TOLERANCE`]), so up to 85° of latitude the sides are
//! within 0.012% of the distance (or 1 cm) wherever the feature is within 150 km of its
//! projection's centre: the projection's own error, under 0.0093% there, and the edges' 0.002%.
//! A winding line or outline buffered in stretches can have vertices up to [`STRETCH_M`] from a
//! stretch's centre, where the sides are within 0.05%. A ring gains at most [`MAX_ADDED`] points,
//! which only a ring of thousands of long edges uses up; past that, its edges are left whole.
//!
//! Arcs use a 2.8125° step: 128 edges to a point's circle and 64 to a semicircular cap.
//! Between vertices, those edges cut at most 0.031% of the distance inside the circle in the
//! projection plane. A short corner can have a step approaching 5.625° when the backend rounds
//! its segment count down, cutting at most 0.125% inside, on top of the projection error.

use geo::{
    BooleanOps, Buffer, Contains, Coord as GC, CoordsIter, LineString, MultiLineString, MultiPolygon, Orient, Point, Polygon,
    algorithm::buffer::{BufferStyle, LineCap, LineJoin},
    orient::Direction,
};
use rustgis_data::{Coord, Geometry};
use rustgis_geom::{Aeqd, geodesic_interpolate, haversine_m, lonlat_to_merc, merc_to_lonlat, spherical_centre};

/// A feature within this distance of its centre is buffered in one projection (the error is
/// under 0.01%).
const ONE_PIECE_M: f64 = 150_000.0;
/// A stretch stays within this distance of its first vertex (the error is under 0.05%).
const STRETCH_M: f64 = 300_000.0;
/// Edges are split to this length before buffering in stretches, so the pieces and the polygon
/// they join meet along the same path.
const DENSIFY_M: f64 = 10_000.0;
/// At most this many vertices are added to one line or ring (four times round the Earth at 10 km).
const MAX_ADDED: f64 = 16_000.0;
/// Back in lon/lat, an outline's straight edges stay within this fraction of the distance of the
/// edges drawn in the plane, or [`MIN_TOLERANCE_M`], whichever is more, on top of the projection's
/// own error (under 0.0093% at [`ONE_PIECE_M`]).
const OUTLINE_TOLERANCE: f64 = 2e-5;
/// The smallest tolerance (metres): finer only adds vertices.
const MIN_TOLERANCE_M: f64 = 0.01;
/// An edge is halved at most this many times on its way back to lon/lat (4096 pieces): enough for
/// the longest edges a buffer has (about 300 km) to come within 1 cm where they stray most.
const MAX_SPLITS: u32 = 12;
/// How far, per square metre of an edge's length, a straight edge in lon/lat can stray from the
/// same edge straight in the plane: measured at 2e-8 in mid-latitudes (the long side of a 2 km
/// buffer of a 96 km line strayed 190 m, 2e-8 × 96,000²) and 1.9e-7 at worst, along a 298 km
/// line at 84°N, with a margin. An edge too short to stray as far as the tolerance isn't
/// checked, so an arc's short edges cost nothing.
const STRAY_PER_M2: f64 = 3e-7;
/// How much [`may_stray`] overstates how far an edge can stray at its latitude: the margin
/// [`STRAY_PER_M2`] has over the worst stray measured.
const STRAY_MARGIN: f64 = 1.6;
/// How far, against ρ / R, an edge straight in the plane at ρ from its centre turns from the
/// geodesic, for [`may_stray`]: about a third on the sphere (the plane's scale x / sin x changes
/// by 1/x − cot x per radian across it), so two leaves a wide margin;
/// [`tests::an_edge_let_pass_by_its_latitude_does_not_stray`] fails at a tenth.
const PLANE_TURN: f64 = 2.0;
/// The Earth's mean radius (metres), for [`may_stray`]'s sphere: the radius `haversine_m` and the
/// planes use.
const EARTH_R_M: f64 = rustgis_geom::EARTH_RADIUS_M;
/// What [`may_stray`] adds to tan φ for the ellipsoid and near the equator, where tan φ is near 0:
/// a tenth of the turn at 45°, as at about 5.7° of latitude.
const FLAT_ALLOWANCE: f64 = 0.1;

/// A circle's area deficit is under 0.041% with 128 edges: less than half the reference's
/// 0.1% bound, leaving room for projection error.
const ARC_STEP: f64 = std::f64::consts::PI / 64.0;

pub(crate) const VERTEX_BUDGET_ERROR: &str = "Buffer exceeds its vertex budget; split large features or select fewer features";

/// Round arcs at the same density for points, line caps and polygon corners, including stretches.
fn buffer_style(metres: f64) -> BufferStyle<f64> {
    BufferStyle::new(metres).line_cap(LineCap::Round(ARC_STEP)).line_join(LineJoin::Round(ARC_STEP))
}

/// A feature's buffer at `metres` (negative shrinks a polygon), in lon/lat; `None` when nothing
/// is left.
pub fn geodesic_buffer(g: &Geometry, metres: f64) -> Option<Geometry> {
    geodesic_buffer_with_limit(g, metres, usize::MAX).ok().and_then(|b| b.geometry)
}

/// A buffered outline and its budget charge: at least the retained coordinate count, or the
/// pre-union disc total for a multipoint, whichever is larger.
pub(crate) struct BudgetedBuffer {
    pub geometry: Option<Geometry>,
    pub charged_vertices: usize,
}

/// A buffer that must fit the caller's remaining vertex budget. Multipoint discs are counted
/// before collecting them for union, and their charge is returned for the caller to accumulate.
/// Other geometries are checked before conversion to the returned geometry; this bounds retained
/// outlines, not all temporary allocations inside the polygon overlay backend.
pub(crate) fn geodesic_buffer_with_limit(g: &Geometry, metres: f64, max_vertices: usize) -> Result<BudgetedBuffer, &'static str> {
    if !metres.is_finite() {
        return Ok(BudgetedBuffer { geometry: None, charged_vertices: 0 });
    }
    let mut pre_union_vertices = 0;
    let mp = match g {
        Geometry::Point(c) => point(*c, metres),
        Geometry::MultiPoint(v) => {
            let mut remaining = max_vertices;
            let mut discs = Vec::new();
            for c in v {
                let disc = point(*c, metres);
                if disc.0.is_empty() {
                    continue;
                }
                remaining = remaining.checked_sub(disc.coords_count()).ok_or(VERTEX_BUDGET_ERROR)?;
                discs.push(disc);
            }
            pre_union_vertices = max_vertices - remaining;
            union(discs)
        }
        Geometry::Line(parts) => lines(&finite(parts), metres),
        Geometry::Polygon(polys) => polygons(&polys.iter().map(|rings| finite(rings)).collect::<Vec<_>>(), metres),
    };
    if mp.coords_count() > max_vertices {
        return Err(VERTEX_BUDGET_ERROR);
    }
    let charged_vertices = pre_union_vertices.max(mp.coords_count());
    Ok(BudgetedBuffer { geometry: to_geometry(&mp), charged_vertices })
}

/// Buffers of this many features or more are made on several threads.
pub(crate) const PARALLEL_FEATURES: usize = 256;

/// The buffers of `geoms` at `metres`, in their order, as [`geodesic_buffer`] makes them: on
/// several threads when there are many (one in the browser), since each is made alone.
pub fn geodesic_buffers(geoms: &[&Geometry], metres: f64) -> Vec<Option<Geometry>> {
    geodesic_buffers_with_limit(geoms, metres, usize::MAX).unwrap_or_default()
}

/// Ordered parallel buffers sharing an outline budget, including pre-union multipoint discs.
/// `usize::MAX` disables cumulative charging for the public unlimited API. Each buffer checks the
/// allowance left before it is made, then is charged to it before it is kept. In-flight
/// operations may allocate temporary geometry on each thread (one in the browser).
pub(crate) fn geodesic_buffers_with_limit(geoms: &[&Geometry], metres: f64, limit: usize) -> Result<Vec<Option<Geometry>>, &'static str> {
    buffers_on(geoms, metres, crate::parallel::threads_for(geoms.len(), PARALLEL_FEATURES), limit)
}

/// [`geodesic_buffers_with_limit`] on `threads` threads, through [`crate::parallel::try_map_on`]:
/// a buffer past the budget stops the others.
fn buffers_on(geoms: &[&Geometry], metres: f64, threads: usize, limit: usize) -> Result<Vec<Option<Geometry>>, &'static str> {
    let remaining = std::sync::atomic::AtomicUsize::new(limit);
    crate::control::begin(geoms.len());
    crate::parallel::try_map_on(
        geoms,
        threads,
        || (),
        |_, _, g| {
            let buffered = geodesic_buffer_with_limit(g, metres, remaining.load(std::sync::atomic::Ordering::Relaxed))?;
            charge(&remaining, limit, buffered.charged_vertices)?;
            Ok(buffered.geometry)
        },
    )
}

/// Takes `vertices` from the `remaining` budget that threads share, or says it is spent; nothing
/// when the `limit` is `usize::MAX`.
pub(crate) fn charge(remaining: &std::sync::atomic::AtomicUsize, limit: usize, vertices: usize) -> Result<(), &'static str> {
    use std::sync::atomic::Ordering;
    if limit == usize::MAX {
        return Ok(());
    }
    let mut left = remaining.load(Ordering::Relaxed);
    loop {
        let next = left.checked_sub(vertices).ok_or(VERTEX_BUDGET_ERROR)?;
        match remaining.compare_exchange_weak(left, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ok(()),
            Err(current) => left = current,
        }
    }
}

/// An azimuthal equidistant plane whose centre's longitude is within 180° of `reference`, a
/// longitude of the coordinates it buffers as written, so the buffer comes back in lon/lat next to
/// them and to the polygon it may be joined to, even at the antimeridian.
struct Plane(Aeqd);

impl Plane {
    fn new(centre: Coord, reference: f64) -> Self {
        let lon = reference + wrap(centre.x - reference);
        Plane(Aeqd::new(Coord::new(if lon.is_finite() { lon } else { centre.x }, centre.y)))
    }

    fn line(&self, pts: &[Coord]) -> LineString<f64> {
        LineString::new(pts.iter().map(|c| self.0.forward(*c)).map(|p| GC { x: p.x, y: p.y }).collect())
    }

    /// A buffer at `metres` drawn in the plane, in lon/lat degrees.
    fn back(&self, mp: &MultiPolygon<f64>, metres: f64) -> MultiPolygon<f64> {
        let tolerance = (metres.abs() * OUTLINE_TOLERANCE).max(MIN_TOLERANCE_M);
        let ring = |r: &LineString<f64>| self.ring(r, tolerance);
        MultiPolygon::new(mp.0.iter().map(|p| Polygon::new(ring(p.exterior()), p.interiors().iter().map(ring).collect())).collect())
    }

    /// A point of the plane in lon/lat.
    fn to_lonlat(&self, p: GC<f64>) -> Coord {
        self.0.inverse(Coord::new(p.x, p.y))
    }

    /// A ring drawn in the plane, in lon/lat degrees, its edges split as [`Plane::split`] needs, by
    /// at most [`MAX_ADDED`] points in all, however many edges stray. Each vertex is taken back to
    /// lon/lat once, where most of a buffer's time goes.
    fn ring(&self, r: &LineString<f64>, tolerance: f64) -> LineString<f64> {
        let mut both = Vec::with_capacity(r.0.len());
        let mut budget = MAX_ADDED as usize;
        let vertices: Vec<(GC<f64>, Coord)> = r.0.iter().map(|p| (*p, self.to_lonlat(*p))).collect();
        for w in vertices.windows(2) {
            let [a, b] = w else { continue };
            both.push(*a);
            self.split(*a, *b, tolerance, 0, &mut budget, &mut both);
        }
        both.extend(vertices.last());
        let (dense, lonlat): (Vec<GC<f64>>, Vec<Coord>) = both.into_iter().unzip();
        let dense = &LineString::new(dense);
        let around = |lat: f64| {
            let pole = self.0.forward(Coord::new(0.0, lat));
            Polygon::new(dense.clone(), vec![]).contains(&Point::new(pole.x, pole.y))
        };
        if around(90.0) {
            return LineString::new(self.capped(dense, &lonlat, 90.0));
        }
        if around(-90.0) {
            return LineString::new(self.capped(dense, &lonlat, -90.0));
        }
        let mut out: Vec<GC<f64>> = Vec::with_capacity(lonlat.len());
        for c in lonlat {
            // Each longitude within 180° of the one before, so the ring doesn't jump at ±180°.
            let x = out.last().map_or(c.x, |prev| prev.x + wrap(c.x - prev.x));
            out.push(GC { x: if x.is_finite() { x } else { c.x }, y: c.y });
        }
        LineString::new(out)
    }

    /// The points to add between `a` and `b`, an edge straight in the plane, so that back in lon/lat
    /// straight edges between them stay within `tolerance` metres of it. The edge's quarter points
    /// and midpoint are compared with the same points of the straight edge in Web Mercator, the
    /// plane the tools draw edges in (one point alone misses a stray that changes side, as along a
    /// line crossing the equator); an edge that strays is halved, and each half checked again, up
    /// to [`MAX_SPLITS`] times, while the ring's `budget` of added points lasts. Each point, the
    /// ends and those added to `out`, comes with its place in lon/lat.
    fn split(
        &self,
        (a, la): (GC<f64>, Coord),
        (b, lb): (GC<f64>, Coord),
        tolerance: f64,
        depth: u32,
        budget: &mut usize,
        out: &mut Vec<(GC<f64>, Coord)>,
    ) {
        if *budget == 0 || depth >= MAX_SPLITS || !may_stray(a, b, la, lb, tolerance) {
            return;
        }
        if !self.strays(a, b, la, lb, tolerance) {
            return;
        }
        let mid = GC { x: (a.x + b.x) / 2.0, y: (a.y + b.y) / 2.0 };
        let mid = (mid, self.to_lonlat(mid));
        self.split((a, la), mid, tolerance, depth + 1, budget, out);
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        out.push(mid);
        self.split(mid, (b, lb), tolerance, depth + 1, budget, out);
    }

    /// Whether the edge from `a` to `b` (straight in the plane, `la` and `lb` in lon/lat) strays
    /// from the same edge straight in Web Mercator by more than `tolerance` metres at its quarter
    /// points or midpoint.
    fn strays(&self, a: GC<f64>, b: GC<f64>, la: Coord, lb: Coord, tolerance: f64) -> bool {
        let at = |t: f64| self.0.inverse(Coord::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y)));
        let beyond = |c: Coord| c.y.abs() >= rustgis_geom::MAX_LAT;
        match (beyond(la), beyond(lb)) {
            // Beyond Web Mercator's 85.05° there's no straight edge to compare with.
            (true, true) => false,
            // Across that edge: halved until each piece is on one side of it or too short to stray.
            (true, false) | (false, true) => true,
            (false, false) => {
                // `b` on `a`'s side of the antimeridian, so the straight edge is the short one.
                let near_b = Coord::new(la.x + wrap(lb.x - la.x), lb.y);
                let (ma, mb) = (lonlat_to_merc(la), lonlat_to_merc(near_b));
                [0.25, 0.5, 0.75].into_iter().any(|t| {
                    let p = at(t);
                    let chord = merc_to_lonlat(Coord::new(ma.x + t * (mb.x - ma.x), ma.y + t * (mb.y - ma.y)));
                    haversine_m(p, Coord::new(p.x + wrap(chord.x - p.x), chord.y)) > tolerance
                })
            }
        }
    }

    /// A ring round a pole, which goes once round the globe in longitude, as a lon/lat ring: cut at
    /// the antimeridian and closed along the pole, so in lon/lat it holds the cap between the ring and
    /// the top (or bottom) of the map.
    fn capped(&self, planar: &LineString<f64>, ring: &[Coord], pole: f64) -> Vec<GC<f64>> {
        let mut v: Vec<Coord> = ring.iter().map(|c| Coord::new(wrap(c.x), c.y)).collect();
        if v.len() > 1 && v.first() == v.last() {
            v.pop();
        }
        let n = v.len();
        // The edge that crosses the antimeridian: its ends are more than 180° apart.
        let seam = (0..n).find(|k| match (v.get((k + n - 1) % n), v.get(*k)) {
            (Some(a), Some(b)) => (b.x - a.x).abs() > 180.0,
            _ => false,
        });
        let found = seam.and_then(|k| Some((k, v.get((k + n - 1) % n)?, planar.0.get((k + n - 1) % n)?, planar.0.get(k)?)));
        let Some((k, a, pa, pb)) = found else {
            return ring.iter().map(|c| GC { x: c.x, y: c.y }).collect();
        };
        // Where the edge from `a` meets ±180° (`edge`, on `a`'s side), found along the edge in the
        // plane, where it's straight.
        let edge = if a.x > 0.0 { 180.0 } else { -180.0 };
        let at = |t: f64| self.0.inverse(Coord::new(pa.x + t * (pb.x - pa.x), pa.y + t * (pb.y - pa.y)));
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..50 {
            let mid = (lo + hi) / 2.0;
            // Before the crossing a longitude is short of `edge`, after it past (wrapped round).
            if wrap(at(mid).x - edge) * edge < 0.0 { lo = mid } else { hi = mid }
        }
        let y = at((lo + hi) / 2.0).y;
        let mut out = vec![GC { x: -edge, y }];
        out.extend((0..n).filter_map(|i| v.get((k + i) % n)).map(|c| GC { x: c.x, y: c.y }));
        out.extend([GC { x: edge, y }, GC { x: edge, y: pole }, GC { x: -edge, y: pole }, GC { x: -edge, y }]);
        out
    }
}

/// Whether the edge from `a` to `b` in the plane (`la` and `lb` in lon/lat) can stray as far as
/// `tolerance` from the same edge straight in Web Mercator, so that [`Plane::strays`] must look.
/// Straight in Web Mercator is a rhumb line, which runs off the geodesic by about L² tan φ / 8R
/// at latitude φ, the heading of a geodesic turning by L tan φ / R along it: 2.0e-8 per square
/// metre at 46°, 1.9e-7 at 84°, as measured for [`STRAY_PER_M2`], plus [`FLAT_ALLOWANCE`] for the
/// ellipsoid and near the equator. Straight in the plane is the
/// geodesic only through the plane's centre, and turns from it as the edge lies farther out, by
/// about ρ / R at ρ from the centre. The latitude is the edge's farthest from the equator plus its
/// length as an angle (L / R), and ρ its end farthest from the centre plus L; both overstate, so an edge
/// this passes doesn't stray, and shorter edges, edges nearer the centre and lower latitudes skip
/// the look that most of a buffer's time went to.
fn may_stray(a: GC<f64>, b: GC<f64>, la: Coord, lb: Coord, tolerance: f64) -> bool {
    let length2 = (b.x - a.x).powi(2) + (b.y - a.y).powi(2);
    if STRAY_PER_M2 * length2 <= tolerance {
        return false;
    }
    let length = length2.sqrt();
    let lat = la.y.abs().max(lb.y.abs()).to_radians() + length / EARTH_R_M;
    let rho = a.x.hypot(a.y).max(b.x.hypot(b.y)) + length;
    if !(lat.is_finite() && rho.is_finite()) || lat >= 89f64.to_radians() {
        return true;
    }
    STRAY_MARGIN * (lat.tan() + FLAT_ALLOWANCE + PLANE_TURN * rho / EARTH_R_M) / (8.0 * EARTH_R_M) * length2 > tolerance
}

/// A longitude difference folded into −180..180.
fn wrap(d: f64) -> f64 {
    (d + 180.0).rem_euclid(360.0) - 180.0
}

/// A point's buffer: a circle drawn in a projection centred on the point.
fn point(c: Coord, metres: f64) -> MultiPolygon<f64> {
    if !c.is_finite() {
        return MultiPolygon::new(vec![]);
    }
    let plane = Plane::new(c, c.x);
    plane.back(&Point::new(0.0, 0.0).buffer_with_style(buffer_style(metres)), metres)
}

/// A line's buffer: in one projection centred on it when it fits, else in stretches.
fn lines(parts: &[Vec<Coord>], metres: f64) -> MultiPolygon<f64> {
    let all: Vec<Coord> = parts.iter().flatten().copied().collect();
    let Some(centre) = spherical_centre(&all) else { return MultiPolygon::new(vec![]) };
    if fits(&all, centre) {
        let plane = Plane::new(centre, all.first().map_or(centre.x, |c| c.x));
        return plane
            .back(&MultiLineString::new(parts.iter().map(|p| plane.line(p)).collect()).buffer_with_style(buffer_style(metres)), metres);
    }
    union(stretched(parts, metres))
}

/// A polygon's buffer: in one projection centred on it when it fits, else the polygon with its
/// outline's buffer (in stretches) added or, inward, taken away.
fn polygons(polys: &[Vec<Vec<Coord>>], metres: f64) -> MultiPolygon<f64> {
    let all: Vec<Coord> = polys.iter().flatten().flatten().copied().collect();
    let Some(centre) = spherical_centre(&all) else { return MultiPolygon::new(vec![]) };
    let shape = |ring: &dyn Fn(&[Coord]) -> LineString<f64>| {
        MultiPolygon::new(
            polys
                .iter()
                .filter_map(|rings| {
                    let mut it = rings.iter();
                    let outer = ring(it.next()?);
                    Some(Polygon::new(outer, it.map(|r| ring(r)).collect()).orient(Direction::Default))
                })
                .collect(),
        )
    };
    if fits(&all, centre) {
        let plane = Plane::new(centre, all.first().map_or(centre.x, |c| c.x));
        return plane.back(&shape(&|r| plane.line(r)).buffer_with_style(buffer_style(metres)), metres);
    }
    // A buffer distributes over a union: the polygon's buffer is the polygon with its outline's
    // buffer added (or, inward, taken away). The polygon stays in lon/lat, along the same densified
    // edges as the outline's pieces.
    let lonlat = shape(&|r| LineString::new(densify(r).iter().map(|c| GC { x: c.x, y: c.y }).collect()));
    let rings: Vec<Vec<Coord>> = polys.iter().flatten().cloned().collect();
    let outline = union(stretched(&rings, metres.abs()));
    if metres >= 0.0 { lonlat.union(&outline) } else { lonlat.difference(&outline) }
}

/// Lines and rings without their non-finite coordinates.
fn finite(parts: &[Vec<Coord>]) -> Vec<Vec<Coord>> {
    parts.iter().map(|p| p.iter().copied().filter(|c| c.is_finite()).collect::<Vec<_>>()).filter(|p| !p.is_empty()).collect()
}

fn fits(pts: &[Coord], centre: Coord) -> bool {
    pts.iter().all(|c| haversine_m(centre, *c) <= ONE_PIECE_M)
}

/// The buffers of lines cut into stretches, each buffered in its own plane, in lon/lat.
fn stretched(parts: &[Vec<Coord>], metres: f64) -> Vec<MultiPolygon<f64>> {
    let mut out = Vec::new();
    for part in parts {
        let d = densify(part);
        let near = |a: usize, b: usize| d.get(a).zip(d.get(b)).is_some_and(|(a, b)| haversine_m(*a, *b) <= STRETCH_M);
        let mut start = 0;
        loop {
            let mut end = start + 1;
            while end + 1 < d.len() && near(start, end + 1) {
                end += 1;
            }
            let Some(stretch) = d.get(start..=end.min(d.len().saturating_sub(1))) else { break };
            if let (Some(centre), Some(first)) = (spherical_centre(stretch), stretch.first()) {
                let plane = Plane::new(centre, first.x);
                out.push(plane.back(&plane.line(stretch).buffer_with_style(buffer_style(metres)), metres));
            }
            // Consecutive stretches share a vertex, so their buffers overlap.
            if end + 1 >= d.len() {
                break;
            }
            start = end;
        }
    }
    out
}

/// A line with every edge split along its geodesic on WGS 84 into pieces of at most [`DENSIFY_M`], or
/// longer ones for a line so long it would gain more than [`MAX_ADDED`] vertices.
fn densify(pts: &[Coord]) -> Vec<Coord> {
    let step = DENSIFY_M.max(rustgis_geom::geodesic_length_m(pts) / MAX_ADDED);
    let mut out: Vec<Coord> = pts.first().copied().into_iter().collect();
    for w in pts.windows(2) {
        let [a, b] = w else { continue };
        let n = (haversine_m(*a, *b) / step).ceil().clamp(1.0, MAX_ADDED) as usize;
        out.extend((1..n).map(|i| on_wgs84(*a, *b, i as f64 / n as f64)));
        out.push(*b);
    }
    out
}

/// The point a fraction `t` of the way from `a` to `b` along the geodesic between them on WGS 84,
/// the path the rest of the buffer follows (a great circle runs tens of metres from it over a
/// few hundred kilometres). Coordinates that aren't on the Earth (a latitude beyond ±90°, say)
/// have no geodesic, so they get the great circle, which stays finite.
fn on_wgs84(a: Coord, b: Coord, t: f64) -> Coord {
    let on_earth = |c: Coord| c.x.is_finite() && (-90.0..=90.0).contains(&c.y);
    if on_earth(a) && on_earth(b) {
        let p = geo::InterpolatePoint::point_at_ratio_between(&geo::Geodesic, geo::Point::new(a.x, a.y), geo::Point::new(b.x, b.y), t);
        let p = Coord::new(a.x + wrap(p.x() - a.x), p.y());
        if p.is_finite() {
            return p;
        }
    }
    geodesic_interpolate(a, b, t)
}

fn union(pieces: Vec<MultiPolygon<f64>>) -> MultiPolygon<f64> {
    if pieces.len() == 1 {
        return pieces.into_iter().next().unwrap_or_else(|| MultiPolygon::new(vec![]));
    }
    let oriented: Vec<Polygon<f64>> = pieces.iter().flat_map(|m| m.0.iter()).map(|p| p.orient(Direction::Default)).collect();
    geo::unary_union(oriented.iter())
}

fn to_geometry(mp: &MultiPolygon<f64>) -> Option<Geometry> {
    let ring = |r: &LineString<f64>| r.0.iter().map(|c| Coord::new(c.x, c.y)).collect::<Vec<_>>();
    let polys: Vec<Vec<Vec<Coord>>> =
        mp.0.iter().map(|p| std::iter::once(p.exterior()).chain(p.interiors().iter()).map(ring).collect()).collect();
    (!polys.is_empty()).then_some(Geometry::Polygon(polys))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::BATCH;

    /// Samples the actual projected outlines: shallow turns exercise a rounded-down join,
    /// while larger turns would fail this bound at the old default arc density.
    #[test]
    fn round_polygon_corners_and_line_joins_keep_their_error_bound() {
        for degrees in [0.25_f64, 2.8, 2.9, 5.5, 8.3, 89.0, 179.0] {
            let angle = degrees.to_radians();
            let line = LineString::from(vec![(-100.0, 0.0), (0.0, 0.0), (100.0 * angle.cos(), 100.0 * angle.sin())]);
            for shape in [geo::Geometry::LineString(line.clone()), geo::Geometry::Polygon(Polygon::new(line, vec![]))] {
                let buffered = shape.buffer_with_style(buffer_style(1.0));
                let mut probes = 0;
                for edge in buffered.0.iter().flat_map(|p| p.exterior().0.windows(2)) {
                    let [a, b] = edge else { continue };
                    if (a.x.hypot(a.y) - 1.0).abs() > 1e-5 || (b.x.hypot(b.y) - 1.0).abs() > 1e-5 {
                        continue;
                    }
                    for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
                        let distance = (a.x + t * (b.x - a.x)).hypot(a.y + t * (b.y - a.y));
                        assert!((1.0 - 0.00125 - 1e-5..=1.0 + 1e-5).contains(&distance), "{degrees}°: {distance}");
                        probes += 1;
                    }
                }
                assert!(probes > 0, "no corner samples at {degrees}°");
            }
        }
    }

    #[test]
    fn a_ring_gains_at_most_max_added_points() {
        // 2,000 edges of about 100 km zigzagging across the plane, each needing dozens of splits.
        let plane = Plane::new(Coord::new(10.0, 46.0), 10.0);
        let zigzag: Vec<GC<f64>> =
            (0..=2000).map(|i| GC { x: i as f64 * 100.0 - 100_000.0, y: if i % 2 == 0 { -50_000.0 } else { 50_000.0 } }).collect();
        let n = zigzag.len();
        let ring = plane.ring(&LineString::new(zigzag), MIN_TOLERANCE_M);
        assert_eq!(ring.0.len(), n + MAX_ADDED as usize);
    }

    #[test]
    fn an_edge_let_pass_by_its_latitude_does_not_stray() {
        // Wherever `may_stray` lets an edge pass without looking, looking agrees: planes from 88°S
        // to 88°N (past Web Mercator's 85.05° too), edges of 100 m to 1,000 km in eight directions,
        // up to 5,000 km from the centre to the side or at 45°, at the tolerances of buffers from
        // 100 m to 5,000 km (the largest Buffer makes).
        let mut passed = 0;
        for lat in (-84..=84).step_by(12).chain([-88, -86, 86, 88]) {
            let plane = Plane::new(Coord::new(10.0, f64::from(lat)), 10.0);
            for len in [100.0, 500.0, 1_000.0, 2_000.0, 5_000.0, 10_000.0, 30_000.0, 100_000.0, 300_000.0, 1_000_000.0] {
                for k in 0..8 {
                    let (sin, cos) = (f64::from(k) * std::f64::consts::PI / 8.0).sin_cos();
                    // The edge's centre off it to the side (where the plane turns it most) or obliquely.
                    for (off, skew) in [-3_000_000.0, -300_000.0, -50_000.0, 0.0, 150_000.0, 1_000_000.0, 5_000_000.0]
                        .into_iter()
                        .flat_map(|off| [(off, 0.0), (off, 1.0)])
                    {
                        let (s, c) = (skew * std::f64::consts::FRAC_PI_4 + std::f64::consts::FRAC_PI_2).sin_cos();
                        let (cx, cy) = ((cos * c - sin * s) * off, (sin * c + cos * s) * off);
                        let a = GC { x: cx - cos * len / 2.0, y: cy - sin * len / 2.0 };
                        let b = GC { x: cx + cos * len / 2.0, y: cy + sin * len / 2.0 };
                        let (la, lb) = (plane.to_lonlat(a), plane.to_lonlat(b));
                        for metres in [100.0, 1_000.0, 5_000.0, 50_000.0, 500_000.0, 2_000_000.0, 5_000_000.0] {
                            let tolerance = (metres * OUTLINE_TOLERANCE).max(MIN_TOLERANCE_M);
                            if STRAY_PER_M2 * len * len > tolerance && !may_stray(a, b, la, lb, tolerance) {
                                passed += 1;
                                assert!(!plane.strays(a, b, la, lb, tolerance), "{len} m at {lat}°, {off} m off, {metres} m");
                            }
                        }
                    }
                }
            }
        }
        // Most edges a buffer has at mid latitudes are let pass.
        assert!(passed > 1_000, "{passed}");
    }

    #[test]
    fn buffers_made_on_several_threads_are_the_ones_made_alone_in_order() {
        let points: Vec<Geometry> =
            (0..PARALLEL_FEATURES * 3).map(|i| Geometry::Point(Coord::new(-170.0 + (i % 340) as f64, -80.0 + (i % 161) as f64))).collect();
        let refs: Vec<&Geometry> = points.iter().collect();
        let alone: Vec<Option<Geometry>> = refs.iter().map(|g| geodesic_buffer(g, 5_000.0)).collect();
        assert_eq!(geodesic_buffers(&refs, 5_000.0), alone);
        let vertices: usize = alone.iter().flatten().map(Geometry::coord_count).sum();
        // The exact shared limit succeeds; one vertex less fails on every thread count.
        for threads in [1, 3, 16] {
            assert_eq!(buffers_on(&refs, 5_000.0, threads, vertices).unwrap(), alone);
            assert_eq!(buffers_on(&refs, 5_000.0, threads, vertices - 1).unwrap_err(), VERTEX_BUDGET_ERROR);
            assert_eq!(buffers_on(&refs, 5_000.0, threads, usize::MAX).unwrap(), alone, "{threads} threads");
            assert_eq!(buffers_on(&refs[..3], 5_000.0, threads, usize::MAX).unwrap(), alone[..3], "{threads} threads, 3 features");
        }
        assert!(buffers_on(&[], 5_000.0, 3, 0).unwrap().is_empty());
        // Overlapping multipoint unions stay small, but every disc is charged across workers.
        let multipoint = Geometry::MultiPoint(vec![Coord::new(0.0, 0.0); 2]);
        let refs = vec![&multipoint; 2 * BATCH];
        let disc_vertices = geodesic_buffer(&Geometry::Point(Coord::new(0.0, 0.0)), 5_000.0).unwrap().coord_count();
        let charge = refs.len() * 2 * disc_vertices;
        for threads in [1, 3, 16] {
            assert!(buffers_on(&refs, 5_000.0, threads, charge).is_ok());
            assert_eq!(buffers_on(&refs, 5_000.0, threads, charge - 1).unwrap_err(), VERTEX_BUDGET_ERROR);
        }
    }
}
