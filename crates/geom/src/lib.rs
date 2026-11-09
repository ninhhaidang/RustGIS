//! L0: coordinates, extents, the Web Mercator projection, geodesic measurement, map scale, and
//! UTM and MGRS coordinates ([`utm`]).
//!
//! Everything here is plain math with no I/O. Longitude/latitude are WGS 84 degrees; projected
//! coordinates are Web Mercator meters (EPSG:3857), which [`lonlat_to_plane`] continues beyond
//! 85.05° to the poles for analysis.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod body;
pub mod eqc;
pub mod sinusoidal;
pub mod triangulate;
pub mod utm;

pub use body::{BODIES, Body};
pub use eqc::EquidistantCylindrical;
pub use sinusoidal::Sinusoidal;
pub use utm::{GridError, Mgrs, Utm, format_mgrs, format_utm, parse_mgrs, parse_utm, to_mgrs, to_utm};

use std::sync::LazyLock;

use geographiclib_rs::{DirectGeodesic, Geodesic, InverseGeodesic, PolygonArea, Winding};
use serde::{Deserialize, Serialize};

/// Mean Earth radius used by the haversine formula (metres).
pub const EARTH_RADIUS_M: f64 = 6_371_008.8;
/// The smallest radius of curvature of the WGS 84 ellipsoid, b²/a, along the meridian at the
/// equator (metres). Every path on the ellipsoid is at least as long as on a sphere of this radius
/// through the same longitudes and latitudes, so a reach worked out on that sphere is never short.
pub const WGS84_MIN_RADIUS_M: f64 = 6_335_439.327_293;
/// The WGS 84 ellipsoid's radius of curvature at the poles, a²/b = a/(1 − f) (meters): near a pole,
/// distances along and across meridians are about this radius times the angle.
pub const WGS84_POLAR_RADIUS_M: f64 = body::EARTH.a / (1.0 - 1.0 / body::EARTH.inv_f);
/// Web Mercator sphere radius (metres).
pub const MERCATOR_R: f64 = 6_378_137.0;
/// Latitude limit of Web Mercator.
pub const MAX_LAT: f64 = 85.051_128_78;
/// How near ±90° (degrees) a latitude counts as the pole: about a centimeter, more than geometry
/// operations' rounding moves a pole vertex (around 1e-8°). The engine's polar edits
/// (`cmd/design.rs`) and analysis's topology, conversion and network code use it to find the
/// features meeting at a pole (#1033).
pub const POLE_TOLERANCE: f64 = 1e-7;
/// Screen pixel size assumed when converting resolution to a map scale (0.28 mm, OGC).
pub const PIXEL_M: f64 = 0.000_28;

/// Metres per international foot (not a US survey foot).
pub const METRES_PER_FOOT: f64 = 0.3048;
/// Metres per international mile.
pub const METRES_PER_MILE: f64 = 1609.344;
/// Square metres per international square foot.
pub const SQUARE_METRES_PER_SQUARE_FOOT: f64 = 0.09290304;
/// Square metres per international square mile.
pub const SQUARE_METRES_PER_SQUARE_MILE: f64 = 2_589_988.110336;
/// Square metres per international acre.
pub const SQUARE_METRES_PER_ACRE: f64 = 4046.8564224;

/// Metres per unit for a lowercase distance unit name. Callers choose defaults and normalize input.
/// Feet are international feet, not US survey feet.
pub fn distance_unit_metres(unit: &str) -> Option<f64> {
    match unit {
        "meters" => Some(1.0),
        "feet" => Some(METRES_PER_FOOT),
        "kilometers" => Some(1000.0),
        "miles" => Some(METRES_PER_MILE),
        _ => None,
    }
}

/// A 2-D coordinate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Coord {
    pub x: f64,
    pub y: f64,
}

impl Coord {
    pub const fn new(x: f64, y: f64) -> Self {
        Coord { x, y }
    }
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

/// An axis-aligned rectangle. An "empty" extent has `min > max`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Extent {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl Default for Extent {
    fn default() -> Self {
        Extent::EMPTY
    }
}

impl Extent {
    pub const EMPTY: Extent = Extent { min_x: f64::INFINITY, min_y: f64::INFINITY, max_x: f64::NEG_INFINITY, max_y: f64::NEG_INFINITY };
    /// The whole world in longitude/latitude.
    pub const WORLD_LONLAT: Extent = Extent { min_x: -180.0, min_y: -85.0, max_x: 180.0, max_y: 85.0 };

    pub const fn new(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Self {
        Extent { min_x, min_y, max_x, max_y }
    }
    pub fn from_points(a: Coord, b: Coord) -> Self {
        Extent { min_x: a.x.min(b.x), min_y: a.y.min(b.y), max_x: a.x.max(b.x), max_y: a.y.max(b.y) }
    }
    pub fn is_empty(&self) -> bool {
        !(self.min_x <= self.max_x && self.min_y <= self.max_y)
    }
    pub fn width(&self) -> f64 {
        if self.is_empty() { 0.0 } else { self.max_x - self.min_x }
    }
    pub fn height(&self) -> f64 {
        if self.is_empty() { 0.0 } else { self.max_y - self.min_y }
    }
    pub fn center(&self) -> Coord {
        Coord::new((self.min_x + self.max_x) / 2.0, (self.min_y + self.max_y) / 2.0)
    }
    pub fn expand(&mut self, c: Coord) {
        if !c.is_finite() {
            return;
        }
        self.min_x = self.min_x.min(c.x);
        self.min_y = self.min_y.min(c.y);
        self.max_x = self.max_x.max(c.x);
        self.max_y = self.max_y.max(c.y);
    }
    pub fn union(&self, o: &Extent) -> Extent {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        Extent::new(self.min_x.min(o.min_x), self.min_y.min(o.min_y), self.max_x.max(o.max_x), self.max_y.max(o.max_y))
    }
    pub fn intersects(&self, o: &Extent) -> bool {
        !self.is_empty()
            && !o.is_empty()
            && self.min_x <= o.max_x
            && o.min_x <= self.max_x
            && self.min_y <= o.max_y
            && o.min_y <= self.max_y
    }
    pub fn contains(&self, c: Coord) -> bool {
        c.x >= self.min_x && c.x <= self.max_x && c.y >= self.min_y && c.y <= self.max_y
    }
    /// Grows every side by `f` of the size (0.1 = 10 %), with a minimum pad for points.
    pub fn padded(&self, f: f64, min_pad: f64) -> Extent {
        if self.is_empty() {
            return *self;
        }
        let px = (self.width() * f).max(min_pad);
        let py = (self.height() * f).max(min_pad);
        Extent::new(self.min_x - px, self.min_y - py, self.max_x + px, self.max_y + py)
    }
}

/// Longitude/latitude (degrees) → Web Mercator metres, without folding the longitude: one past
/// ±180° lands past the world's edge (±20,037,508.34 m), so a shape kept unwrapped across the
/// antimeridian (as `geodesic_interpolate` and `Aeqd::inverse` keep one, and a geodesic buffer
/// returns) stays the sliver it is, and `merc_to_lonlat` returns the longitude it was given. Edits,
/// overlays and hulls rely on that. Latitude is clamped to ±85.05°. A caller that needs one point
/// inside the world uses [`lonlat_to_merc_folded`] (#940).
pub fn lonlat_to_merc(c: Coord) -> Coord {
    let lat = c.y.clamp(-MAX_LAT, MAX_LAT).to_radians();
    Coord::new(MERCATOR_R * c.x.to_radians(), MERCATOR_R * (std::f64::consts::FRAC_PI_4 + lat / 2.0).tan().ln())
}

/// Like [`lonlat_to_merc`], but a longitude outside −180..180 folds into that range, so a single
/// point projects onto the world, never metres past its edge. Do not project the vertices of a
/// shape with it: an edge from 179.9° to 180.1° would become one across the whole map. A
/// non-finite longitude stays broken rather than quietly becoming the prime meridian.
pub fn lonlat_to_merc_folded(c: Coord) -> Coord {
    let lon = if !c.x.is_finite() || (c.x >= -180.0 && c.x <= 180.0) { c.x } else { wrap_lon(c.x % 360.0) };
    lonlat_to_merc(Coord::new(lon, c.y))
}

/// Web Mercator metres → longitude/latitude (degrees).
pub fn merc_to_lonlat(c: Coord) -> Coord {
    let lon = (c.x / MERCATOR_R).to_degrees();
    let lat = (2.0 * (c.y / MERCATOR_R).exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees();
    Coord::new(lon, lat)
}

/// Web Mercator's northing at [`MAX_LAT`] north and south, each as [`lonlat_to_merc`] rounds it
/// (the two differ by a rounding), and how fast it grows there (meters a radian of latitude,
/// R·sec [`MAX_LAT`]): where [`lonlat_to_plane`] carries it on to the poles.
static MERC_EDGE: LazyLock<(f64, f64, f64)> = LazyLock::new(|| {
    let edge = |lat: f64| lonlat_to_merc(Coord::new(0.0, lat)).y;
    (edge(MAX_LAT), edge(-MAX_LAT), MERCATOR_R / MAX_LAT.to_radians().cos())
});

/// Longitude/latitude (degrees) → the plane the analysis tools work in (meters): Web Mercator
/// within ±[`MAX_LAT`], exactly as [`lonlat_to_merc`] gives it, and beyond, where Web Mercator
/// stops, continued straight in latitude at the rate it reaches there, so the two meet smoothly and
/// the poles lie at ±26,424 km. Nothing within ±90° is clamped (a latitude past one is taken as
/// the pole), so a polar feature keeps its shape through an overlay. Beyond 85.05°, where the plane
/// is linear in latitude, an edge straight in the plane is straight in longitude and latitude too
/// (Web Mercator's view still draws it clamped to its edge).
pub fn lonlat_to_plane(c: Coord) -> Coord {
    let lat = c.y.clamp(-90.0, 90.0);
    // False for NaN, which then stays NaN below.
    if lat.abs() <= MAX_LAT {
        return lonlat_to_merc(c);
    }
    let (north, south, rate) = *MERC_EDGE;
    let beyond = (lat.abs() - MAX_LAT).to_radians() * rate;
    Coord::new(MERCATOR_R * c.x.to_radians(), if lat > 0.0 { north + beyond } else { south - beyond })
}

/// The analysis plane's meters → longitude/latitude (degrees), undoing [`lonlat_to_plane`]:
/// [`merc_to_lonlat`] within Web Mercator's square, and beyond it back along the continuation, to
/// ±90° at most.
pub fn plane_to_lonlat(p: Coord) -> Coord {
    let (north, south, rate) = *MERC_EDGE;
    let beyond = if p.y > north {
        p.y - north
    } else if p.y < south {
        south - p.y
    } else {
        // Within the square, or NaN, which merc_to_lonlat keeps NaN.
        return merc_to_lonlat(p);
    };
    let lat = (MAX_LAT + (beyond / rate).to_degrees()).min(90.0);
    Coord::new((p.x / MERCATOR_R).to_degrees(), lat.copysign(p.y))
}

/// Projects a lon/lat extent to Web Mercator, preserving unwrapped longitudes for spans below
/// 360°. A span of a full turn or more maps to the standard world bounds.
pub fn extent_to_merc(e: &Extent) -> Extent {
    if e.is_empty() {
        return *e;
    }
    // Preserve an unwrapped antimeridian extent instead of folding its corners separately.
    // A full turn or more covers the world, independent of the starting longitude.
    let (sw, ne) = (lonlat_to_merc(Coord::new(e.min_x, e.min_y)), lonlat_to_merc(Coord::new(e.max_x, e.max_y)));
    let world = MERCATOR_R * std::f64::consts::PI;
    let (west, east) =
        if e.max_x.is_finite() && e.min_x.is_finite() && e.max_x - e.min_x >= 360.0 { (-world, world) } else { (sw.x, ne.x) };
    Extent::from_points(Coord::new(west, sw.y), Coord::new(east, ne.y))
}

/// Great-circle distance (metres) from the latitudes' cosines and the lat/lon differences (radians).
#[inline]
fn haversine_from_trig(dlat: f64, dlon: f64, cos1: f64, cos2: f64) -> f64 {
    let h = (dlat / 2.0).sin().powi(2) + cos1 * cos2 * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().clamp(0.0, 1.0).asin()
}

/// Great-circle distance between two lon/lat points (metres); NaN for non-finite coordinates.
pub fn haversine_m(a: Coord, b: Coord) -> f64 {
    let (la1, la2) = (a.y.to_radians(), b.y.to_radians());
    haversine_from_trig(la2 - la1, (b.x - a.x).to_radians(), la1.cos(), la2.cos())
}

/// Length of a lon/lat polyline along great circles (metres).
pub fn geodesic_length_m(pts: &[Coord]) -> f64 {
    let Some((first, rest)) = pts.split_first() else { return 0.0 };
    let mut total = 0.0;
    let mut prev_la = first.y.to_radians();
    let mut prev_cos = prev_la.cos();
    let mut prev_x = first.x;

    for pt in rest {
        let cur_la = pt.y.to_radians();
        let cur_cos = cur_la.cos();
        total += haversine_from_trig(cur_la - prev_la, (pt.x - prev_x).to_radians(), prev_cos, cur_cos);

        prev_la = cur_la;
        prev_cos = cur_cos;
        prev_x = pt.x;
    }
    total
}

/// A lon/lat point as a unit vector from the Earth's centre.
fn unit_vector(c: Coord) -> [f64; 3] {
    let (lon, lat) = (c.x.to_radians(), c.y.to_radians());
    [lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin()]
}

/// A vector from the Earth's centre as lon/lat (degrees); `None` for the zero vector.
fn from_vector(v: [f64; 3]) -> Option<Coord> {
    let [x, y, z] = v;
    let h = x.hypot(y);
    (h.hypot(z) > 1e-12).then(|| Coord::new(y.atan2(x).to_degrees(), z.atan2(h).to_degrees()))
}

/// The point a fraction `t` of the way from `a` to `b` along the great circle between them.
pub fn geodesic_interpolate(a: Coord, b: Coord, t: f64) -> Coord {
    let (va, vb) = (unit_vector(a), unit_vector(b));
    let dot = (va[0] * vb[0] + va[1] * vb[1] + va[2] * vb[2]).clamp(-1.0, 1.0);
    let w = dot.acos();
    if w < 1e-12 || w.sin().abs() < 1e-12 {
        // The same point, or antipodes (no single great circle): straight in lon/lat.
        return Coord::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
    }
    let (ka, kb) = (((1.0 - t) * w).sin() / w.sin(), (t * w).sin() / w.sin());
    let v = [ka * va[0] + kb * vb[0], ka * va[1] + kb * vb[1], ka * va[2] + kb * vb[2]];
    match from_vector(v) {
        // Keep the longitude next to `a`'s, so a path across the antimeridian stays continuous.
        Some(c) => Coord::new(a.x + wrap_lon(c.x - a.x), c.y),
        None => a,
    }
}

/// A longitude (degrees) folded into −180..180, unchanged when it is already there. The remainder
/// is exact, as GeographicLib takes it, so a longitude far out of range still names the meridian
/// GeographicLib measures from. One that isn't finite is NaN (where `wrap_lon`, which folds
/// differences, gives 0).
pub fn fold_lon(x: f64) -> f64 {
    let r = x % 360.0;
    if r > 180.0 {
        r - 360.0
    } else if r < -180.0 {
        r + 360.0
    } else {
        r
    }
}

/// A longitude difference (degrees) folded into −180..180.
fn wrap_lon(d: f64) -> f64 {
    let w = (d + 180.0).rem_euclid(360.0) - 180.0;
    if w.is_finite() { w } else { 0.0 }
}

/// The centre of lon/lat points on the sphere (their mean direction from the Earth's centre),
/// or `None` when there are none or they balance out (such as two antipodes).
pub fn spherical_centre(pts: &[Coord]) -> Option<Coord> {
    let mut s = [0.0; 3];
    for v in pts.iter().filter(|c| c.is_finite()).map(|c| unit_vector(*c)) {
        s = [s[0] + v[0], s[1] + v[1], s[2] + v[2]];
    }
    from_vector(s)
}

/// The center on the sphere of points taken one at a time, each with a weight (such as stretches
/// of lines by their lengths): their weighted mean direction from the Earth's center.
#[derive(Clone, Copy, Debug, Default)]
pub struct SphericalMean {
    sum: [f64; 3],
    weight: f64,
    /// How many times the totals were scaled down by 2⁵¹² to keep them finite.
    shrunk: i32,
}

impl SphericalMean {
    /// Takes in a point; a non-finite one is left out.
    pub fn add(&mut self, c: Coord) {
        self.add_weighted(c, 1.0);
    }

    /// Takes in a point counting `weight` times (as a stretch of a line does, by its length); a
    /// non-finite point, or a weight that isn't a positive normal number (zero, negative,
    /// subnormal, missing or infinite), is left out: a subnormal one rounds its direction away.
    pub fn add_weighted(&mut self, c: Coord, weight: f64) {
        if c.is_finite() && weight.is_normal() && weight > 0.0 {
            // Totals that would overflow are scaled down by a power of two, as is every weight
            // after, which leaves the mean as it was (a weight tiny beside them drops to nothing,
            // as it would anyway).
            let shrink = 0.5f64.powi(512);
            let mut weight = weight * 0.5f64.powi(self.shrunk.saturating_mul(512));
            if self.weight + weight > 1e300 {
                self.sum = self.sum.map(|s| s * shrink);
                self.weight *= shrink;
                self.shrunk = self.shrunk.saturating_add(1);
                weight *= shrink;
            }
            let (s, v) = (self.sum, unit_vector(c));
            self.sum = [s[0] + v[0] * weight, s[1] + v[1] * weight, s[2] + v[2] * weight];
            self.weight += weight;
        }
    }

    /// The center of the points so far, or `None` when there are none or they balance out (such as
    /// two antipodes) to within rounding of their total weight.
    pub fn center(&self) -> Option<Coord> {
        if self.weight <= 0.0 {
            return None;
        }
        // Their mean direction, as a vector no longer than 1 (the threshold is then relative).
        from_vector(self.sum.map(|s| s / self.weight))
    }
}

/// The point `dist_m` metres from `c` along the geodesic leaving it at `azimuth_deg` (clockwise
/// from north): the direct geodesic problem on the WGS 84 ellipsoid.
pub fn geodesic_destination(c: Coord, azimuth_deg: f64, dist_m: f64) -> Coord {
    let (s, co) = azimuth_deg.to_radians().sin_cos();
    Aeqd::new(c).inverse(Coord::new(dist_m * s, dist_m * co))
}

/// The geodesic distance between two lon/lat points on the WGS 84 ellipsoid (metres): the inverse
/// geodesic problem, solved by GeographicLib.
pub fn wgs84_distance_m(a: Coord, b: Coord) -> f64 {
    // A broken coordinate is no place (see [`haversine_m`]): the clamp below would otherwise turn a
    // latitude of infinity into a pole.
    if !a.is_finite() || !b.is_finite() {
        return f64::NAN;
    }
    let (s12, _): (f64, f64) = WGS84.inverse(a.y.clamp(-90.0, 90.0), a.x, b.y.clamp(-90.0, 90.0), b.x);
    s12
}

/// The geodesic from `a` to `b` on the WGS 84 ellipsoid: its length in meters, as
/// [`wgs84_distance_m`] gives it, and its azimuths at `a` and at `b` (degrees clockwise from north).
/// A coordinate that isn't finite is no place: all three are NaN, as that distance is.
pub fn wgs84_inverse(a: Coord, b: Coord) -> (f64, f64, f64) {
    if !a.is_finite() || !b.is_finite() {
        return (f64::NAN, f64::NAN, f64::NAN);
    }
    let (s12, azi1, azi2, _): (f64, f64, f64, f64) = WGS84.inverse(a.y.clamp(-90.0, 90.0), a.x, b.y.clamp(-90.0, 90.0), b.x);
    (s12, azi1, azi2)
}

/// The point `dist_m` meters from `c` along the geodesic leaving it at `azimuth_deg` (clockwise
/// from north) on the WGS 84 ellipsoid, and the geodesic's azimuth there. The longitude is in
/// −180..180 exactly as GeographicLib gives it, not moved next to `c`'s, which would round it. A
/// start, azimuth or distance that isn't finite leads nowhere: NaN.
pub fn wgs84_direct(c: Coord, azimuth_deg: f64, dist_m: f64) -> (Coord, f64) {
    if !c.is_finite() || !azimuth_deg.is_finite() || !dist_m.is_finite() {
        return (Coord::new(f64::NAN, f64::NAN), f64::NAN);
    }
    let (lat, lon, azi2): (f64, f64, f64) = WGS84.direct(c.y.clamp(-90.0, 90.0), c.x, azimuth_deg, dist_m);
    (Coord::new(lon, lat), azi2)
}

/// Length of a lon/lat polyline along the WGS 84 ellipsoid's geodesics (metres): each segment is
/// an inverse geodesic solve, so it is exact, unlike [`geodesic_length_m`]'s great circles. Long
/// lines of many vertices cost a solve per segment (a few microseconds each).
pub fn wgs84_length_m(pts: &[Coord]) -> f64 {
    pts.windows(2)
        .map(|w| match w {
            [a, b] => wgs84_distance_m(*a, *b),
            _ => 0.0,
        })
        .sum()
}

/// Whether a ring through both poles winds beyond one complete surface.
///
/// Such a ring has no single enclosed region: closing its winding at either pole gives areas a
/// whole globe apart. The whole-world rectangle remains valid. Roundoff within 64 ulps of the
/// complete surface is not an extra winding.
pub fn ring_area_is_ambiguous(ring: &[Coord]) -> bool {
    if !ring.iter().all(|c| c.is_finite() && c.y.abs() <= 90.0)
        || !ring.iter().any(|c| at_pole(c.y) && c.y > 0.0)
        || !ring.iter().any(|c| at_pole(c.y) && c.y < 0.0)
    {
        return false;
    }
    let steps = ring_lon_steps(ring);
    let full = steps.iter().any(|&(_, full)| full);
    let sphere_surface = 4.0 * std::f64::consts::PI * EARTH_RADIUS_M * EARTH_RADIUS_M;
    let wgs84_surface = 2.0 * std::f64::consts::PI * WGS84_A * WGS84_A * authalic_q(90.0);
    let beyond = |area: f64, surface: f64| area > surface * (1.0 + 64.0 * f64::EPSILON);
    beyond(sphere_ring_area_m2(ring, steps.iter().map(|&(d, _)| d), full), sphere_surface)
        || (full && beyond(authalic_ring_area_m2(ring), wgs84_surface))
}

/// The perimeter (metres) and the unsigned enclosed area (square metres) of a lon/lat ring on the
/// WGS 84 ellipsoid. The ring is closed along a geodesic to its first point, whatever its winding,
/// so the perimeter and area come from one pass over the ring, not two. A ring gives the
/// region it traces (so polar rings and globe-wide bands follow their own edges, as the sphere's
/// [`geodesic_ring_area_m2`] does; a ring with a full-turn edge, see [`ring_lon_steps`], is
/// measured on the ellipsoid by its edges' paths in longitude and latitude, each full turn a whole
/// parallel); reversing the ring gives the same magnitude. An edge along the ±180° meridian (a
/// polygon split at the antimeridian) is a seam, measured as the meridian it runs along.
/// An ambiguous winding ([`ring_area_is_ambiguous`]) has NaN area and keeps its perimeter.
pub fn wgs84_ring_perimeter_area_m2(ring: &[Coord]) -> (f64, f64) {
    // A broken vertex is no place: NaN, never a plausible number (see [`haversine_m`]).
    if !ring.iter().all(|c| c.is_finite()) {
        return (f64::NAN, f64::NAN);
    }
    if ring.len() < 2 {
        return (0.0, 0.0);
    }
    // A two-vertex ring is a segment there and back: twice its length, and no area.
    if let [a, b] = ring {
        return (2.0 * wgs84_distance_m(*a, *b), 0.0);
    }
    // An edge of a full turn of longitude (-180° to 180° along a parallel, as a globe-wide band or a
    // cap closed along the pole has) joins two coordinates that name the same place, which a
    // geodesic cannot tell from no edge at all: such a ring is measured by its edges' paths in
    // longitude and latitude, on the ellipsoid (see [`authalic_ring_area_m2`]), and each full turn
    // counts the whole parallel towards the perimeter. A seam edge needs none of this: GeographicLib
    // takes it as the meridian it is.
    let steps = ring_lon_steps(ring);
    if steps.iter().any(|&(_, full)| full) {
        let perimeter = ring
            .iter()
            .zip(ring.iter().cycle().skip(1))
            .zip(&steps)
            .map(|((p, r), &(dlon, full))| if full { wgs84_parallel_m((p.y + r.y) / 2.0, dlon) } else { wgs84_distance_m(*p, *r) })
            .sum();
        return (perimeter, if ring_area_is_ambiguous(ring) { f64::NAN } else { authalic_ring_area_m2(ring) });
    }
    let mut polygon = PolygonArea::new(&WGS84, Winding::Clockwise);
    for c in ring {
        polygon.add_point(c.y.clamp(-90.0, 90.0), c.x);
    }
    // `compute` reduces the signed area to at most half the ellipsoid, so a ring enclosing more
    // than half of it comes back as the smaller complement: the sphere's area says which one the
    // ring traces.
    let (perimeter, area, _) = polygon.compute(true);
    let area = area.abs();
    let sphere = sphere_ring_area_m2(ring, steps.iter().map(|&(d, _)| d), false);
    // Compare a sphere area with its own hemisphere; the ellipsoid's hemisphere differs.
    let hemisphere = 2.0 * std::f64::consts::PI * EARTH_RADIUS_M * EARTH_RADIUS_M;
    (
        perimeter,
        if ring_area_is_ambiguous(ring) {
            f64::NAN
        } else if sphere > hemisphere {
            WGS84_AREA_M2 - area
        } else {
            area
        },
    )
}

/// The length (metres) of `dlon_deg` degrees of longitude along the WGS 84 parallel at `lat_deg`:
/// N cos φ per radian, with N = a / √(1 − e² sin² φ) the prime vertical radius of curvature
/// (Snyder 1987, eqs. 4-20 and 4-21).
fn wgs84_parallel_m(lat_deg: f64, dlon_deg: f64) -> f64 {
    let (s, c) = lat_deg.clamp(-90.0, 90.0).to_radians().sin_cos();
    WGS84_A / (1.0 - WGS84_E2 * s * s).sqrt() * c.abs() * dlon_deg.to_radians().abs()
}

/// Coordinates closer than this (degrees, about 11 m, half a unit in the fourth decimal) are on one
/// parallel or meridian, for [`ring_lon_steps`]: a band written with coordinates rounded to four
/// decimals or more keeps its full turns.
const NEAR_DEG: f64 = 1e-4;

/// Whether a latitude is at a pole, within [`NEAR_DEG`].
fn at_pole(lat: f64) -> bool {
    lat.is_finite() && lat.abs() >= 90.0 - NEAR_DEG
}

/// The sine of latitude an edge's step in longitude is taken at, for an edge to or from a pole in a
/// ring with a full turn: such an edge runs along a meridian, and its step in longitude is taken at
/// the pole, where the sine is ±1. An edge from one pole to the other turns half at each, 0. `None`
/// for an edge that touches no pole, or with a latitude that isn't finite (which stays NaN).
fn pole_sine(lat1: f64, lat2: f64) -> Option<f64> {
    if !(lat1.is_finite() && lat2.is_finite()) {
        return None;
    }
    match (at_pole(lat1), at_pole(lat2)) {
        (true, true) => Some((lat1.signum() + lat2.signum()) / 2.0),
        (true, false) => Some(lat1.signum()),
        (false, true) => Some(lat2.signum()),
        (false, false) => None,
    }
}

/// Each edge of a lon/lat ring, closed back to its first vertex, as its step in longitude
/// (degrees) and whether that step is a full turn around the globe.
///
/// A step of less than a full turn is taken the short way (179° to -179° is 2° east), as the
/// antimeridian convention of GeoJSON (RFC 7946 §3.1.9) has it. An edge of 360° joins two
/// coordinates on the same meridian, ±180°, and means one of two things:
///
/// - a **full turn**, the whole parallel, as a globe-wide band (`[-180, -10]` to `[180, -10]`) or
///   a polar cap closed along the pole writes it;
/// - a **seam**, a step along the ±180° meridian (or none at all), as a polygon split at the
///   antimeridian writes it when one ring has vertices on both 180° and -180°.
///
/// The rule: a full turn runs along a parallel (both ends at one latitude: a path that winds the
/// globe while changing latitude is a spiral, which no format writes as a polygon edge), and the
/// ring comes to it along a meridian and leaves it along a meridian back the way it came, north
/// and then south or south and then north, with no step in longitude. That is how a band, a cap
/// and the whole globe turn: down the -180° meridian, round the parallel, up the 180° meridian. A
/// ring that steps east or west into the edge or out of it is travelling across the antimeridian
/// there, and one that goes on north (or south) past it is following the seam, so the edge is a
/// seam: taken as a turn, it would double back over the parallel or the meridian the ring was just
/// following. An edge to or from a pole runs along a meridian whatever the pole's longitude.
///
/// The full turns must also not make the ring wind further round the globe than it does with them
/// as seams: a polygon's ring winds 0° in longitude, or 360° round a pole it encloses. A band's two
/// turns cancel, and a cap's turn along the pole closes the turn its coastline makes; a turn that
/// would wind the ring once more is a seam. A ring through a pole can turn there by any amount, so
/// it keeps its full turns, and the step of its first edge to or from a pole is the one that closes
/// its winding (a pole vertex's longitude names no place). So a band, a cap and the whole globe keep
/// their full turns, and a 1° cell with vertices at 180° and -180° is the cell. A seam's step is 0. Steps of
/// more than a full turn (longitudes beyond ±180°, which no antimeridian split writes) are always
/// kept as written, as full turns, as before.
fn ring_lon_steps(ring: &[Coord]) -> Vec<(f64, bool)> {
    let n = ring.len();
    let edge = |i: usize| ring.get(i).zip(ring.get((i + 1) % n));
    // Most rings have no 360° edge: their steps are the short way round, with nothing to decide.
    let short = |d: f64| if d.abs() > 180.0 { wrap_lon(d) } else { d };
    if ring.iter().zip(ring.iter().cycle().skip(1)).all(|(a, b)| (b.x - a.x).abs() < 360.0 - NEAR_DEG) {
        return ring.iter().zip(ring.iter().cycle().skip(1)).map(|(a, b)| (short(b.x - a.x), false)).collect();
    }
    // An edge's step north (+) or south (-), and whether it moves in longitude, other than by whole
    // turns. An edge to or from a pole is a meridian.
    let dlat = |i: usize| edge(i).map_or(0.0, |(a, b)| b.y - a.y);
    let moves = |i: usize| edge(i).is_some_and(|(a, b)| !at_pole(a.y) && !at_pole(b.y) && wrap_lon(b.x - a.x).abs() > NEAR_DEG);
    // An edge from a vertex to a copy of itself (a closing vertex repeated) is no neighbour: look
    // past it to the next edge that goes somewhere, `step` edges at a time (1 forward, n - 1 back).
    let same = |a: &Coord, b: &Coord| a.x == b.x && a.y == b.y;
    let neighbour = |i: usize, step: usize| {
        let mut j = (i + step) % n;
        for _ in 0..n {
            if !edge(j).is_some_and(|(a, b)| same(a, b)) {
                break;
            }
            j = (j + step) % n;
        }
        j
    };
    // Each edge's step as a seam, and the step it would take as a full turn: `Some` for a 360° edge
    // that may be one.
    let candidates: Vec<(f64, Option<f64>)> = (0..n)
        .map(|i| {
            let Some((p, r)) = edge(i) else { return (0.0, None) };
            let d = r.x - p.x;
            if d.is_nan() || d.abs() < 360.0 - NEAR_DEG {
                return (short(d), None);
            }
            if d.abs() > 360.0 + NEAR_DEG {
                return (d, None);
            }
            let (before, after) = (neighbour(i, n - 1), neighbour(i, 1));
            let full = (r.y - p.y).abs() <= NEAR_DEG && !moves(before) && !moves(after) && dlat(before) * dlat(after) < 0.0;
            (0.0, full.then_some(d))
        })
        .collect();
    let seams: f64 = candidates.iter().map(|(d, _)| d).sum();
    let turns: f64 = candidates.iter().map(|(d, t)| t.unwrap_or(*d)).sum();
    let through_pole = ring.iter().any(|c| at_pole(c.y));
    let keep = through_pole || turns.abs() <= seams.abs() + NEAR_DEG;
    let mut steps: Vec<(f64, bool)> = candidates
        .into_iter()
        .map(|(d, t)| match t {
            Some(t) if keep => (t, true),
            _ => (d, d.abs() > 360.0 + NEAR_DEG),
        })
        .collect();
    // A ring through both poles could close its winding at either, an area apart by the whole
    // globe's worth of sin ξ: no step is the right one there, so its steps stay as they are, the
    // same from whichever vertex the ring starts.
    let poles = ring.iter().filter(|c| at_pole(c.y)).map(|c| c.y.signum());
    let one_pole = poles.clone().all(|s| s > 0.0) || poles.clone().all(|s| s < 0.0);
    if through_pole && one_pole && steps.iter().any(|&(_, full)| full) {
        let turns = (steps.iter().map(|(d, _)| d).sum::<f64>() / 360.0).round();
        let first = (0..n).find(|&i| edge(i).is_some_and(|(a, b)| pole_sine(a.y, b.y).is_some_and(|s| s != 0.0)));
        if let Some(step) = first.and_then(|i| steps.get_mut(i))
            && turns.is_finite()
            && turns != 0.0
        {
            step.0 -= 360.0 * turns;
        }
    }
    steps
}

/// The perimeter (metres) of a lon/lat ring, closed back to its first vertex, on the sphere:
/// edges along great circles ([`haversine_m`]), except a full turn around the globe (see
/// [`ring_lon_steps`]), which is its whole parallel. The sphere's counterpart of
/// [`wgs84_ring_perimeter_area_m2`]'s perimeter; [`geodesic_length_m`] of the closed ring counts
/// a full turn as nothing, the distance between its two ends.
pub fn geodesic_ring_perimeter_m(ring: &[Coord]) -> f64 {
    if ring.len() < 2 {
        return 0.0;
    }
    // A two-vertex ring is a segment there and back, as on the ellipsoid.
    if let [a, b] = ring {
        return 2.0 * haversine_m(*a, *b);
    }
    ring.iter()
        .zip(ring.iter().cycle().skip(1))
        .zip(ring_lon_steps(ring))
        .map(|((p, r), (dlon, full))| {
            if full { EARTH_RADIUS_M * ((p.y + r.y) / 2.0).to_radians().cos().abs() * dlon.to_radians().abs() } else { haversine_m(*p, *r) }
        })
        .sum()
}

/// The WGS 84 ellipsoid's equatorial radius (metres).
const WGS84_A: f64 = 6_378_137.0;

/// The WGS 84 ellipsoid's first eccentricity squared, e² = f(2 − f).
const WGS84_E2: f64 = 0.006_694_379_990_141_316;

/// q(φ) = (1 − e²) [sin φ / (1 − e² sin² φ) + atanh(e sin φ) / e], Snyder (1987), *Map Projections:
/// A Working Manual*, USGS Professional Paper 1395, eq. 3-12. The authalic latitude ξ has
/// sin ξ = q(φ) / q(90°) (eq. 3-11).
fn authalic_q(lat_deg: f64) -> f64 {
    let e = WGS84_E2.sqrt();
    let s = lat_deg.clamp(-90.0, 90.0).to_radians().sin();
    (1.0 - WGS84_E2) * (s / (1.0 - WGS84_E2 * s * s) + (e * s).atanh() / e)
}

/// The area (square metres, unsigned) of a lon/lat ring on the WGS 84 ellipsoid, for rings whose
/// edges are paths in longitude and latitude. The area between a ring and the equator is
/// R_q² ∮ sin ξ dλ, with ξ the authalic latitude and R_q = a √(q(90°) / 2) the authalic radius
/// (Snyder 1987, eqs. 3-11 to 3-13), which is Green's theorem on the equal-area sphere. Along a
/// parallel (no change of latitude) the integral is sin ξ × Δλ, in closed form; an edge that
/// changes latitude is straight in longitude and latitude, integrated by Simpson's rule. Steps of
/// longitude are [`ring_lon_steps`]': the short way, a full turn kept, a seam none.
fn authalic_ring_area_m2(ring: &[Coord]) -> f64 {
    let a = WGS84_A;
    let q_pole = authalic_q(90.0);
    let rq2 = a * a * q_pole / 2.0;
    let sin_xi = |lat: f64| authalic_q(lat) / q_pole;
    let terms = ring.iter().zip(ring.iter().cycle().skip(1)).zip(ring_lon_steps(ring)).map(|((p, r), (dlon, _))| {
        let dlat = r.y - p.y;
        // An edge to or from a pole runs along a meridian ([`pole_sine`]); sin ξ is ±1 there too.
        let mean = if let Some(s) = pole_sine(p.y, r.y) {
            s
        } else if dlat.abs() < 1e-12 {
            sin_xi(p.y)
        } else {
            const N: usize = 32;
            let mut sum = sin_xi(p.y) + sin_xi(r.y);
            for i in 1..N {
                let w = if i % 2 == 1 { 4.0 } else { 2.0 };
                sum += w * sin_xi(p.y + dlat * i as f64 / N as f64);
            }
            sum / (3.0 * N as f64)
        };
        dlon.to_radians() * mean
    });
    (compensated_sum(terms) * rq2).abs()
}

/// Surface area 2πa²(1 + (1 − e²) atanh(e) / e) for WGS 84 (square metres).
const WGS84_AREA_M2: f64 = 510_065_621_724_079.0;

/// The WGS 84 ellipsoid's geodesic solver.
static WGS84: LazyLock<Geodesic> = LazyLock::new(Geodesic::wgs84);

/// The azimuthal equidistant projection on the WGS 84 ellipsoid, centred on a lon/lat point:
/// distances and directions from the centre are true, so a buffer drawn in it is geodesic near
/// the centre. Coordinates are metres east and north of the centre.
///
/// It is Karney's ellipsoidal form (Karney 2013, "Algorithms for geodesics"): a point's
/// distance and azimuth from the centre are the inverse geodesic problem, and back again the
/// direct one, both solved by GeographicLib to a few nanometres.
#[derive(Clone, Copy, Debug)]
pub struct Aeqd {
    lon0: f64,
    lat0: f64,
}

impl Aeqd {
    pub fn new(centre: Coord) -> Self {
        Aeqd { lon0: centre.x, lat0: centre.y.clamp(-90.0, 90.0) }
    }

    /// Lon/lat (degrees) → metres from the centre.
    pub fn forward(&self, c: Coord) -> Coord {
        let (s12, azi1, _, _): (f64, f64, f64, f64) = WGS84.inverse(self.lat0, self.lon0, c.y.clamp(-90.0, 90.0), c.x);
        let (sa, ca) = azi1.to_radians().sin_cos();
        Coord::new(s12 * sa, s12 * ca)
    }

    /// Metres from the centre → lon/lat (degrees). The longitude is within 180° of the centre's,
    /// not folded into −180..180, so a shape across the antimeridian stays in one piece. Metres
    /// that aren't finite give no place (both coordinates NaN).
    pub fn inverse(&self, p: Coord) -> Coord {
        if !(p.x.is_finite() && p.y.is_finite()) {
            return Coord::new(f64::NAN, f64::NAN);
        }
        // At a pole the azimuth is taken from the meridian `lon0`, as `forward` gives it.
        let (lat, lon): (f64, f64) = WGS84.direct(self.lat0, self.lon0, p.x.atan2(p.y).to_degrees(), p.x.hypot(p.y));
        Coord::new(self.lon0 + wrap_lon(lon - self.lon0), lat)
    }

    /// [`Aeqd::inverse`], with the azimuth (degrees clockwise from north) the geodesic from the
    /// center arrives there with: a direction carried along it turns as much as that differs from
    /// the azimuth it left with, `p.x.atan2(p.y)`.
    pub fn inverse_heading(&self, p: Coord) -> (Coord, f64) {
        if !(p.x.is_finite() && p.y.is_finite()) {
            return (Coord::new(f64::NAN, f64::NAN), f64::NAN);
        }
        let (lat, lon, azi2): (f64, f64, f64) = WGS84.direct(self.lat0, self.lon0, p.x.atan2(p.y).to_degrees(), p.x.hypot(p.y));
        (Coord::new(self.lon0 + wrap_lon(lon - self.lon0), lat), azi2)
    }
}

/// Area of a lon/lat ring on the sphere (square metres, always positive).
///
/// Uses the spherical-excess approximation from Chamberlain & Duquette (JPL, 2007).
/// Returns NaN for an ambiguous winding ([`ring_area_is_ambiguous`]).
pub fn geodesic_ring_area_m2(ring: &[Coord]) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    if ring_area_is_ambiguous(ring) {
        return f64::NAN;
    }
    // An edge that jumps across the antimeridian (179° to -179°) is a short hop east, not 358°
    // west. A full turn (-180° to 180° along a parallel, as a world-wide rectangle has) is kept as
    // it is, and a seam along ±180° is no step in longitude ([`ring_lon_steps`]). A ring without a
    // 360° edge needs none of that: its steps are the short way round.
    if ring.iter().zip(ring.iter().cycle().skip(1)).all(|(a, b)| (b.x - a.x).abs() < 360.0 - NEAR_DEG) {
        let short = |(a, b): (&Coord, &Coord)| {
            let d = b.x - a.x;
            if d.abs() > 180.0 { wrap_lon(d) } else { d }
        };
        return sphere_ring_area_m2(ring, ring.iter().zip(ring.iter().cycle().skip(1)).map(short), false);
    }
    let steps = ring_lon_steps(ring);
    let full = steps.iter().any(|&(_, full)| full);
    sphere_ring_area_m2(ring, steps.into_iter().map(|(d, _)| d), full)
}

/// [`geodesic_ring_area_m2`] with each edge's step in longitude (degrees) given. In a ring with a
/// full turn (`full`), an edge to or from a pole takes its step at the pole ([`pole_sine`]), as the
/// ellipsoid's [`authalic_ring_area_m2`] does; other rings keep each edge straight in longitude and
/// latitude.
fn sphere_ring_area_m2(ring: &[Coord], steps: impl Iterator<Item = f64>, full: bool) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    let sin = |c: &Coord| c.y.to_radians().sin();
    let total = compensated_sum(ring.iter().zip(ring.iter().cycle().skip(1)).zip(steps).map(|((p1, p2), dlon)| {
        let sines = match pole_sine(p1.y, p2.y).filter(|_| full) {
            Some(s) => 2.0 * s,
            None => sin(p1) + sin(p2),
        };
        dlon.to_radians() * (2.0 + sines)
    }));
    (total * EARTH_RADIUS_M * EARTH_RADIUS_M / 2.0).abs()
}

/// Kahan compensated summation prevents long valid rings crossing the winding tolerance from
/// accumulated addition roundoff: Kahan (1965), https://doi.org/10.1145/363707.363723.
fn compensated_sum(terms: impl IntoIterator<Item = f64>) -> f64 {
    let (mut total, mut lost) = (0.0, 0.0);
    for term in terms {
        let corrected = term - lost;
        let next = total + corrected;
        lost = (next - total) - corrected;
        total = next;
    }
    total
}

/// Map scale denominator (1:N) for a Web Mercator resolution (metres per screen pixel) at a latitude.
pub fn scale_denominator(merc_m_per_px: f64, lat_deg: f64) -> f64 {
    merc_m_per_px * lat_deg.to_radians().cos().abs() / PIXEL_M
}

/// Rounds a distance down to a "nice" 1, 2 or 5 × 10ⁿ for scale bars.
pub fn nice_distance(d: f64) -> f64 {
    if d <= 0.0 || !d.is_finite() {
        return 0.0;
    }
    let p = 10f64.powf(d.log10().floor());
    let f = d / p;
    let n = if f >= 5.0 {
        5.0
    } else if f >= 2.0 {
        2.0
    } else {
        1.0
    };
    n * p
}

/// Formats a distance in metres as `0.2 mm` / `5 mm` / `20 cm` / `850 m` / `12.5 km`: under a
/// metre in centimetres or millimetres, so a scale bar on a site plan (#902) doesn't read `0 m`.
/// A negative distance keeps its sign and picks its unit by magnitude (-12.5 km). Non-finite
/// values display as `—`.
pub fn format_distance(m: f64) -> String {
    if !m.is_finite() {
        return "—".to_string();
    }
    // Each unit is picked, and shown, by the value rounded as it shows: formatting alone rounds
    // halves to even.
    let (mm, cm) = ((m * 1000.0).round(), (m * 100.0).round());
    if m != 0.0 && mm == 0.0 {
        // Under half a millimetre, which the closest zoom spreads over pixels: tenths, then a
        // bound rather than 0.
        let tenths = (m * 10_000.0).round() / 10.0;
        return if tenths != 0.0 {
            format!("{tenths:.1} mm")
        } else if m < 0.0 {
            "> -0.1 mm".to_string()
        } else {
            "< 0.1 mm".to_string()
        };
    }
    if m != 0.0 && mm.abs() < 10.0 {
        format!("{mm:.0} mm")
    } else if m != 0.0 && cm.abs() < 100.0 {
        format!("{cm:.0} cm")
    } else if m.abs().round() < 1000.0 {
        format!("{m:.0} m")
    } else if (m.abs() / 100.0).round() < 1000.0 {
        format!("{:.1} km", m / 1000.0)
    } else {
        format!("{:.0} km", m / 1000.0)
    }
}

/// Formats an area in square metres as `km²` or `m²`; `—` for non-finite values.
/// A negative area keeps its sign and picks its unit by magnitude (-1.0 km²).
pub fn format_area(m2: f64) -> String {
    if !m2.is_finite() {
        return "—".to_string();
    }
    if m2.abs().round() < 1.0e6 {
        // A tiny negative area rounds to `-0`; it reads `0 m²`.
        let whole = format!("{m2:.0}");
        format!("{} m²", if whole == "-0" { "0" } else { &whole })
    } else {
        format!("{:.1} km²", m2 / 1.0e6)
    }
}

/// Formats a lon/lat pair in decimal degrees with hemispheres: `12.345° E 45.678° N`.
/// Displays `—` if either coordinate is non-finite.
pub fn format_lonlat(c: Coord) -> String {
    if !c.is_finite() {
        return "—".to_string();
    }
    let ew = if c.x < 0.0 { 'W' } else { 'E' };
    let ns = if c.y < 0.0 { 'S' } else { 'N' };
    format!("{:.4}° {ew}  {:.4}° {ns}", c.x.abs(), c.y.abs())
}

/// Formats a lon/lat pair as degrees-minutes-seconds; `—` if either coordinate is non-finite.
pub fn format_dms(c: Coord) -> String {
    fn dms(v: f64) -> String {
        let tenths = (v.abs() * 36_000.0).round();
        let d = (tenths / 36_000.0).floor();
        let m = ((tenths % 36_000.0) / 600.0).floor();
        let s = (tenths % 600.0) / 10.0;
        format!("{d:.0}°{m:02.0}′{s:04.1}″")
    }
    if !c.is_finite() {
        return "—".to_string();
    }
    let ew = if c.x < 0.0 { 'W' } else { 'E' };
    let ns = if c.y < 0.0 { 'S' } else { 'N' };
    format!("{} {ew}  {} {ns}", dms(c.x), dms(c.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_weighted_point_counts_as_that_many() {
        // Weight 2 is the point taken twice; a weight of 0, below 0 or missing, and a point
        // missing a coordinate, are left out.
        let (a, b) = (Coord::new(0.0, 0.0), Coord::new(90.0, 0.0));
        let (mut twice, mut weighted) = (SphericalMean::default(), SphericalMean::default());
        [a, a, b].into_iter().for_each(|c| twice.add(c));
        weighted.add_weighted(a, 2.0);
        weighted.add(b);
        for (c, w) in [(b, 0.0), (b, -1.0), (b, f64::NAN), (Coord::new(f64::NAN, 0.0), 1.0)] {
            weighted.add_weighted(c, w);
        }
        let (t, w) = (twice.center().unwrap(), weighted.center().unwrap());
        assert!((t.x - w.x).abs() < 1e-12 && (t.y - w.y).abs() < 1e-12, "{t:?} {w:?}");
        // Antipodes weighing a million each balance out, to within their rounding.
        let mut balanced = SphericalMean::default();
        balanced.add_weighted(a, 1e6);
        balanced.add_weighted(Coord::new(180.0, 0.0), 1e6);
        assert_eq!(balanced.center(), None);
    }

    #[test]
    fn any_weight_gives_the_mean() {
        // A tiny weight alone is still a center, and the largest weights don't overflow the
        // totals: a and b at the largest weight each meet halfway, and a third point weighing
        // the same pulls the center towards it, as at any weight.
        let (a, b) = (Coord::new(0.0, 0.0), Coord::new(90.0, 0.0));
        let mut tiny = SphericalMean::default();
        tiny.add_weighted(b, 1e-13);
        assert_eq!(tiny.center(), Some(b));
        let (mut huge, mut unit) = (SphericalMean::default(), SphericalMean::default());
        for c in [a, b, a] {
            huge.add_weighted(c, f64::MAX);
            unit.add_weighted(c, 1.0);
        }
        let mut half = SphericalMean::default();
        half.add_weighted(a, f64::MAX);
        half.add_weighted(b, f64::MAX);
        let mid = half.center().unwrap();
        assert!((mid.x - 45.0).abs() < 1e-9 && mid.y.abs() < 1e-9, "{mid:?}");
        // Totals scaled down once they hold a point: the second 1e300 passes the limit.
        let mut grown = SphericalMean::default();
        grown.add_weighted(a, 1e300);
        grown.add_weighted(b, 1e300);
        let mid = grown.center().unwrap();
        assert!((mid.x - 45.0).abs() < 1e-9 && mid.y.abs() < 1e-9, "{mid:?}");
        // A subnormal weight is left out, as a zero one is: alone, no center.
        let mut sub = SphericalMean::default();
        sub.add_weighted(b, 5e-324);
        assert_eq!(sub.center(), None);
        let (h, u) = (huge.center().unwrap(), unit.center().unwrap());
        assert!((h.x - u.x).abs() < 1e-9 && (h.y - u.y).abs() < 1e-9, "{h:?} {u:?}");
    }

    #[test]
    fn a_geodesic_arrives_turned_as_far_as_it_has_gone_around_the_pole() {
        // From 88°N, east 300 km: near a pole the geodesic turns away from it by about as much
        // longitude as it covers (53° here), arriving heading south of east; a step of a
        // trillionth of a meter doesn't turn at all.
        let (at, heading) = Aeqd::new(Coord::new(0.0, 88.0)).inverse_heading(Coord::new(300_000.0, 0.0));
        assert!(((heading - 90.0) - at.x).abs() < 1.0 && at.x > 45.0, "{at:?} {heading}");
        let (_, heading) = Aeqd::new(Coord::new(0.0, 88.0)).inverse_heading(Coord::new(1e-12, 0.0));
        assert!((heading - 90.0).abs() < 1e-9, "{heading}");
    }

    #[test]
    fn mercator_roundtrip() {
        for (x, y) in [(0.0, 0.0), (12.5, 41.9), (-122.4, 37.8), (179.0, -60.0)] {
            let m = lonlat_to_merc(Coord::new(x, y));
            let back = merc_to_lonlat(m);
            assert!((back.x - x).abs() < 1e-9 && (back.y - y).abs() < 1e-9);
        }
    }

    #[test]
    fn wgs84_rings_keep_full_turn_edges_and_large_areas() {
        let c = Coord::new;
        // A globe-wide band: its -180° to 180° edges are parallels, not zero-length hops. Measured
        // on the sphere: 2πR²(sin 10° − sin −10°) ≈ 8.85e13 m².
        let band = [c(-180.0, -10.0), c(180.0, -10.0), c(180.0, 10.0), c(-180.0, 10.0)];
        let (_, area) = wgs84_ring_perimeter_area_m2(&band);
        assert!(area > 8.5e13 && area < 9.2e13, "{area}");
        // A ring of 36 vertices at 60° N, closed by a wrapped 10° edge (no full-turn edge): it traces
        // the southern cap of latitude < 60°, more than half the Earth, so it must not come back as
        // the northern cap. Its area agrees with the sphere's, as the ring's own edges define it.
        let big: Vec<Coord> = (0..36).map(|i| c(-180.0 + f64::from(i) * 10.0, 60.0)).collect();
        let area = wgs84_ring_perimeter_area_m2(&big).1;
        assert!(area > WGS84_AREA_M2 / 2.0, "{area}");
        assert!((area / geodesic_ring_area_m2(&big) - 1.0).abs() < 0.01, "{area}");
        // A broken latitude is no pole.
        assert!(wgs84_length_m(&[c(0.0, 0.0), c(0.0, f64::INFINITY)]).is_nan());
        assert!(wgs84_distance_m(c(0.0, f64::NEG_INFINITY), c(0.0, 0.0)).is_nan());
    }

    /// The WGS 84 band between 10° S and 10° N, 2π a² q(10°) (Snyder 1987, eq. 3-12), and its
    /// ring's perimeter: two whole parallels at 10°, 2π N cos φ each, and the ±180° meridian from
    /// 10° S to 10° N twice, the meridian arc ∫ a(1 − e²)/(1 − e² sin² φ)^1.5 dφ. Worked out apart
    /// from the code.
    const BAND_10_M2: f64 = 88_187_925_166_399.86;
    const BAND_10_PERIMETER_M: f64 = 2.0 * 39_470_171.064_535 + 2.0 * 2_211_709.666_469;

    #[test]
    fn a_band_counts_its_parallels_and_a_split_cell_is_the_cell() {
        let c = Coord::new;
        // #1598. A globe-wide band: each -180° to 180° edge is a whole parallel.
        let band = [c(-180.0, -10.0), c(180.0, -10.0), c(180.0, 10.0), c(-180.0, 10.0)];
        let mut closed = band.to_vec();
        closed.push(band[0]);
        for ring in [band.to_vec(), closed] {
            for reverse in [false, true] {
                let mut ring = ring.clone();
                if reverse {
                    ring.reverse();
                }
                let (perimeter, area) = wgs84_ring_perimeter_area_m2(&ring);
                assert!((area / BAND_10_M2 - 1.0).abs() < 1e-9, "{area}");
                assert!((perimeter / BAND_10_PERIMETER_M - 1.0).abs() < 1e-9, "{perimeter}");
                // The sphere's: 2π R cos 10° twice, and 20° of meridian twice.
                let r = EARTH_RADIUS_M;
                let sphere = 4.0 * std::f64::consts::PI * r * 10f64.to_radians().cos() + 2.0 * r * 20f64.to_radians();
                assert!((geodesic_ring_perimeter_m(&ring) / sphere - 1.0).abs() < 1e-9);
            }
        }
        // A band whose coordinates were rounded to 7 decimals at one end of a full turn is the band.
        for off in [1e-7, 5e-5] {
            for rounded in [
                [c(-180.0, -10.0), c(180.0, -10.0 - off), c(180.0, 10.0), c(-180.0, 10.0)],
                [c(-180.0, -10.0), c(180.0, -10.0), c(180.0, 10.0), c(-180.0 - off, 10.0)],
                [c(-180.0, -10.0), c(180.0, -10.0), c(180.0 - off, 10.0), c(-180.0, 10.0)],
            ] {
                let (perimeter, area) = wgs84_ring_perimeter_area_m2(&rounded);
                assert!((area / BAND_10_M2 - 1.0).abs() < 1e-5, "{off}: {area}");
                assert!((perimeter / BAND_10_PERIMETER_M - 1.0).abs() < 1e-5, "{off}: {perimeter}");
            }
        }
        // The sphere's band: 2πR²(sin 10° − sin −10°).
        let sphere = 4.0 * std::f64::consts::PI * EARTH_RADIUS_M * EARTH_RADIUS_M * 10f64.to_radians().sin();
        assert!((geodesic_ring_area_m2(&band) / sphere - 1.0).abs() < 1e-9);
        // A cap closed at the pole by one vertex (whose longitude names no place): the full turn along
        // 80° N is kept, and the edges to the pole are meridians.
        let tip = [c(-180.0, 80.0), c(180.0, 80.0), c(0.0, 90.0)];
        let area = wgs84_ring_perimeter_area_m2(&tip).1;
        assert!((area / CAP_80N_M2 - 1.0).abs() < 1e-9, "{area}");
        // On the sphere, 2πR²(1 − sin 80°), not half of it.
        let sphere_cap = 2.0 * std::f64::consts::PI * EARTH_RADIUS_M * EARTH_RADIUS_M * (1.0 - 80f64.to_radians().sin());
        assert!((geodesic_ring_area_m2(&tip) / sphere_cap - 1.0).abs() < 1e-9, "{}", geodesic_ring_area_m2(&tip));
        // A band 0.00005° high, and one with a vertex halfway up each meridian, are bands.
        let h = 5e-5;
        let narrow = [c(-180.0, 0.0), c(180.0, 0.0), c(180.0, h), c(-180.0, h)];
        let narrow_sphere = 2.0 * std::f64::consts::PI * EARTH_RADIUS_M * EARTH_RADIUS_M * h.to_radians().sin();
        assert!((geodesic_ring_area_m2(&narrow) / narrow_sphere - 1.0).abs() < 1e-6);
        let dense = [c(-180.0, -10.0), c(180.0, -10.0), c(180.0, 0.0), c(180.0, 10.0), c(-180.0, 10.0), c(-180.0, 0.0)];
        assert!((wgs84_ring_perimeter_area_m2(&dense).1 / BAND_10_M2 - 1.0).abs() < 1e-9);
        // Whatever longitude the pole vertex is given, or within 1e-4° of the pole, it is the cap.
        for tip in [[c(-180.0, 80.0), c(180.0, 80.0), c(90.0, 90.0)], [c(-180.0, 80.0), c(180.0, 80.0), c(0.0, 89.9999)]] {
            for reverse in [false, true] {
                let mut tip = tip.to_vec();
                if reverse {
                    tip.reverse();
                }
                let area = wgs84_ring_perimeter_area_m2(&tip).1;
                assert!((area / CAP_80N_M2 - 1.0).abs() < 1e-5, "{tip:?}: {area}");
                assert!((geodesic_ring_area_m2(&tip) / sphere_cap - 1.0).abs() < 1e-5, "{tip:?}");
            }
        }
        // A ring through a pole with no full turn keeps its edges straight in longitude and latitude
        // on the sphere, as before: πR²(1 − sin 80°)/3 for this one.
        let sector = [c(0.0, 80.0), c(120.0, 80.0), c(0.0, 90.0)];
        let straight = std::f64::consts::PI * EARTH_RADIUS_M * EARTH_RADIUS_M * (1.0 - 80f64.to_radians().sin()) / 3.0;
        assert!((geodesic_ring_area_m2(&sector) / straight - 1.0).abs() < 1e-9);
        // An edge from one pole to the other measures the same both ways round.
        let poles = [c(-180.0, -90.0), c(180.0, -90.0), c(0.0, 90.0), c(-180.0, 90.0)];
        let mut back = poles.to_vec();
        back.reverse();
        assert!((geodesic_ring_area_m2(&poles) - geodesic_ring_area_m2(&back)).abs() < 1.0);
        assert!((wgs84_ring_perimeter_area_m2(&poles).1 - wgs84_ring_perimeter_area_m2(&back).1).abs() < 1.0);
        // A ring through both poles measures the same from whichever vertex it starts.
        let both = [c(-180.0, -90.0), c(180.0, -90.0), c(-90.0, 90.0), c(90.0, 90.0)];
        let (sphere0, wgs0) = (geodesic_ring_area_m2(&both), wgs84_ring_perimeter_area_m2(&both).1);
        for k in 1..both.len() {
            let mut turned = both.to_vec();
            turned.rotate_left(k);
            assert!((geodesic_ring_area_m2(&turned) - sphere0).abs() < 1.0, "{k}");
            assert!((wgs84_ring_perimeter_area_m2(&turned).1 - wgs0).abs() < 1.0, "{k}");
        }
        // A latitude that isn't finite next to a pole stays NaN.
        assert!(geodesic_ring_area_m2(&[c(0.0, f64::NAN), c(90.0, 90.0), c(180.0, 90.0)]).is_nan());
        assert!(geodesic_ring_area_m2(&[c(-180.0, f64::NAN), c(180.0, 80.0), c(180.0, 90.0), c(-180.0, 90.0)]).is_nan());
        // A band 5e-10° high: its meridian edges are short, not repeated vertices.
        let h = 5e-10;
        let hair = [c(-180.0, 0.0), c(180.0, 0.0), c(180.0, h), c(-180.0, h)];
        let hair_sphere = 2.0 * std::f64::consts::PI * EARTH_RADIUS_M * EARTH_RADIUS_M * h.to_radians().sin();
        assert!((geodesic_ring_area_m2(&hair) / hair_sphere - 1.0).abs() < 1e-3);
        // A two-vertex ring is a segment there and back on the sphere, as on the ellipsoid.
        let two = [c(-180.0, 10.0), c(180.0, 10.0)];
        assert!(geodesic_ring_perimeter_m(&two) < 1e-3);
        assert!(wgs84_ring_perimeter_area_m2(&two).0 < 1e-3);
        // GeographicLib's measure of a ring wholly on one side of the antimeridian.
        let reference = |ring: &[Coord]| {
            let mut polygon = PolygonArea::new(&WGS84, Winding::Clockwise);
            for p in ring {
                polygon.add_point(p.y, p.x);
            }
            let (perimeter, area, _) = polygon.compute(true);
            (perimeter, area.abs())
        };
        // A 1° cell east of the antimeridian whose edge along it is written from 180° to -180°: a
        // seam, the meridian from 0° to 1°, not a turn around the globe (once near-hemisphere).
        let cell = [c(179.0, 0.0), c(180.0, 0.0), c(-180.0, 1.0), c(179.0, 1.0)];
        let (want_p, want_a) = reference(&[c(179.0, 0.0), c(180.0, 0.0), c(180.0, 1.0), c(179.0, 1.0)]);
        // A 1° cell across the antimeridian with vertices on both 180° and -180° at each latitude:
        // the 180° to -180° edges along its parallels are seams of no length.
        let across =
            [c(179.5, 0.0), c(180.0, 0.0), c(-180.0, 0.0), c(-179.5, 0.0), c(-179.5, 1.0), c(-180.0, 1.0), c(180.0, 1.0), c(179.5, 1.0)];
        let (across_p, across_a) = reference(&[c(179.5, 0.0), c(180.0, 0.0), c(180.5, 0.0), c(180.5, 1.0), c(180.0, 1.0), c(179.5, 1.0)]);
        for (ring, (want_p, want_a)) in [(cell.to_vec(), (want_p, want_a)), (across.to_vec(), (across_p, across_a))] {
            assert!(want_a > 1.2e10 && want_a < 1.3e10, "{want_a}");
            let (perimeter, area) = wgs84_ring_perimeter_area_m2(&ring);
            assert!((area / want_a - 1.0).abs() < 1e-9, "{area} vs {want_a}");
            assert!((perimeter / want_p - 1.0).abs() < 1e-9, "{perimeter} vs {want_p}");
            // The sphere's area and perimeter agree to the ellipsoid's flattening.
            assert!((geodesic_ring_area_m2(&ring) / want_a - 1.0).abs() < 0.01, "{}", geodesic_ring_area_m2(&ring));
            assert!((geodesic_ring_perimeter_m(&ring) / want_p - 1.0).abs() < 0.01);
        }
        // A 1° × 2° polygon east of the antimeridian with its vertex at 0° written on both sides: the
        // 180° to -180° edge between two meridians is a lone turn that would wind the ring once
        // round the globe, so it is a seam of no length, not the equator.
        let spike = [c(180.0, -1.0), c(180.0, 0.0), c(-180.0, 0.0), c(-180.0, 1.0), c(-179.0, 1.0), c(-179.0, -1.0)];
        let (want_p, want_a) = reference(&[c(180.0, -1.0), c(180.0, 0.0), c(180.0, 1.0), c(181.0, 1.0), c(181.0, -1.0)]);
        let (perimeter, area) = wgs84_ring_perimeter_area_m2(&spike);
        assert!((area / want_a - 1.0).abs() < 1e-9, "{area} vs {want_a}");
        assert!((perimeter / want_p - 1.0).abs() < 1e-9, "{perimeter} vs {want_p}");
        assert!((geodesic_ring_perimeter_m(&spike) / want_p - 1.0).abs() < 0.01);
        // Up the seam with a zero-length crossing at 0° and another back at 1°: both edges go on north
        // along the meridian, so they are seams, not a band from 0° to 1° (their turns would cancel).
        let zigzag =
            [c(180.0, -1.0), c(180.0, 0.0), c(-180.0, 0.0), c(-180.0, 1.0), c(180.0, 1.0), c(180.0, 2.0), c(179.0, 2.0), c(179.0, -1.0)];
        let (want_p, want_a) = reference(&[c(180.0, -1.0), c(180.0, 0.0), c(180.0, 1.0), c(180.0, 2.0), c(179.0, 2.0), c(179.0, -1.0)]);
        let (perimeter, area) = wgs84_ring_perimeter_area_m2(&zigzag);
        assert!((area / want_a - 1.0).abs() < 1e-9, "{area} vs {want_a}");
        assert!((perimeter / want_p - 1.0).abs() < 1e-9, "{perimeter} vs {want_p}");
        // The cell closed with its first vertex repeated is still the cell.
        let mut closed = across.to_vec();
        closed.push(across[0]);
        assert!((wgs84_ring_perimeter_area_m2(&closed).1 / across_a - 1.0).abs() < 1e-9);
        // A thin band of 1° along the full turns, from meridians on both sides: still a band.
        let thin = [c(180.0, 0.0), c(-180.0, 0.0), c(-180.0, 1.0), c(180.0, 1.0)];
        assert!(wgs84_ring_perimeter_area_m2(&thin).1 > 4e12);
    }

    /// The WGS 84 zone between latitude 80° N and the pole, 2π R_q² (1 − sin ξ) in square metres
    /// (Snyder 1987, eqs. 3-11 to 3-13), worked out apart from the code: 3,908,572.76 km².
    const CAP_80N_M2: f64 = 3.908_572_761_8e12;

    #[test]
    fn a_cap_closed_along_the_pole_is_measured_on_the_ellipsoid() {
        let c = Coord::new;
        // Cut at ±180° and closed along the pole: one edge along 80° N, one along the pole. The
        // sphere's area for it is 3.875 M km², 0.9% low.
        let cap = [c(-180.0, 80.0), c(180.0, 80.0), c(180.0, 90.0), c(-180.0, 90.0), c(-180.0, 80.0)];
        // A vertex every degree along 80° N, then up the meridian and back along the pole.
        let mut dense: Vec<Coord> = (-180..=180).map(|x| c(f64::from(x), 80.0)).collect();
        dense.extend([c(180.0, 90.0), c(-180.0, 90.0)]);
        for ring in [cap.to_vec(), dense] {
            for reverse in [false, true] {
                let mut ring = ring.clone();
                if reverse {
                    ring.reverse();
                }
                let (_, area) = wgs84_ring_perimeter_area_m2(&ring);
                assert!((area / CAP_80N_M2 - 1.0).abs() < 1e-9, "reverse={reverse}: {area}");
                assert!((geodesic_ring_area_m2(&ring) / CAP_80N_M2 - 1.0).abs() > 0.005, "the sphere is the 0.9% low one");
            }
        }
    }

    #[test]
    fn rings_without_a_full_turn_edge_keep_geographiclib_areas() {
        let c = Coord::new;
        let ring = [c(10.0, 50.0), c(12.0, 50.0), c(12.0, 52.0), c(10.0, 52.0)];
        let mut polygon = PolygonArea::new(&WGS84, Winding::Clockwise);
        for p in &ring {
            polygon.add_point(p.y, p.x);
        }
        let (perimeter, area, _) = polygon.compute(true);
        let measured = wgs84_ring_perimeter_area_m2(&ring);
        assert_eq!(measured, (perimeter, area.abs()));
    }

    #[test]
    fn the_authalic_area_of_the_whole_globe_is_the_ellipsoids() {
        // Two hemispheres' worth of band: a ring with 360° edges at the poles, -90° to 90°.
        let c = Coord::new;
        let globe = [c(-180.0, -90.0), c(180.0, -90.0), c(180.0, 90.0), c(-180.0, 90.0)];
        let area = authalic_ring_area_m2(&globe);
        assert!((area / WGS84_AREA_M2 - 1.0).abs() < 1e-9, "{area}");
    }

    #[test]
    fn an_extent_across_the_antimeridian_stays_a_sliver() {
        // A geodesic buffer near 180° comes back unwrapped (179.9° to 180.1°): about 22 km wide, not
        // the whole world.
        let e = extent_to_merc(&Extent::new(179.9, 0.0, 180.1, 1.0));
        assert!(e.width() > 20_000.0 && e.width() < 25_000.0, "{}", e.width());
        // Inside the world nothing changes; a wider-than-world extent is the world.
        let plain = extent_to_merc(&Extent::new(-10.0, 0.0, 10.0, 1.0));
        assert!((plain.width() - 2.0 * MERCATOR_R * 10f64.to_radians()).abs() < 1e-6);
        let all = extent_to_merc(&Extent::new(-540.0, 0.0, 540.0, 1.0));
        assert!((all.width() - 2.0 * MERCATOR_R * std::f64::consts::PI).abs() < 1e-6);
        // A full turn from Greenwich is the world too, at the world's edges, not 0 to twice the edge.
        let turn = extent_to_merc(&Extent::new(0.0, 0.0, 360.0, 1.0));
        let world = MERCATOR_R * std::f64::consts::PI;
        assert!((turn.min_x + world).abs() < 1e-6 && (turn.max_x - world).abs() < 1e-6, "{turn:?}");
    }

    #[test]
    fn mercator_keeps_longitudes_outside_the_world_unless_folded() {
        // Unwrapped, 185° round-trips (an edit of a shape across the antimeridian stays narrow).
        let past = lonlat_to_merc(Coord::new(185.0, 0.0));
        assert!((merc_to_lonlat(past).x - 185.0).abs() < 1e-9);
        assert!(past.x > MERCATOR_R * std::f64::consts::PI);
        // Folded, 185° is -175° on the world: the same place, never past its edge.
        let folded = lonlat_to_merc_folded(Coord::new(185.0, 0.0));
        let there = lonlat_to_merc(Coord::new(-175.0, 0.0));
        assert!((folded.x - there.x).abs() < 1e-6 && folded.y == there.y);
        assert!(folded.x.abs() <= MERCATOR_R * std::f64::consts::PI, "{}", folded.x);
        // Reduce the longitude before adding 180 degrees: at 2^60 that addition loses bits.
        for lon in [2f64.powi(60), -2f64.powi(60), f64::MAX, -f64::MAX, -540.0, -180.0, 180.0, 540.0] {
            let reduced = lon % 360.0;
            let expected = if reduced > 180.0 {
                reduced - 360.0
            } else if reduced < -180.0 {
                reduced + 360.0
            } else {
                reduced
            };
            let actual = lonlat_to_merc_folded(Coord::new(lon, 0.0));
            // Either sign of 180 degrees denotes the seam.
            let delta = (merc_to_lonlat(actual).x - expected).abs();
            assert!(delta < 1e-9 || (delta - 360.0).abs() < 1e-9, "{lon}: {actual:?}");
            assert!(actual.x.abs() <= MERCATOR_R * std::f64::consts::PI);
        }
        // A broken longitude stays broken, never a plausible place near Greenwich.
        assert!(lonlat_to_merc_folded(Coord::new(f64::NAN, 0.0)).x.is_nan());
        assert!(lonlat_to_merc_folded(Coord::new(f64::INFINITY, 0.0)).x.is_infinite());
    }

    #[test]
    fn wgs84_near_hemisphere_rings_keep_the_spherical_side() {
        // Both sides of the equator: the spherical and ellipsoidal hemisphere areas differ.
        // Compare each model with its own hemisphere, independently of winding.
        for latitude in [-0.00001, 0.00001] {
            let ring: Vec<_> = (0..36).map(|i| Coord::new(-180.0 + f64::from(i) * 10.0, latitude)).collect();
            let (perimeter, area) = wgs84_ring_perimeter_area_m2(&ring);
            assert_eq!(area > WGS84_AREA_M2 / 2.0, latitude > 0.0, "{latitude}: {area}");
            let reversed: Vec<_> = ring.iter().rev().copied().collect();
            let (reverse_perimeter, reverse_area) = wgs84_ring_perimeter_area_m2(&reversed);
            assert!((reverse_area - area).abs() < 1.0);
            assert!((reverse_perimeter - perimeter).abs() < 1e-6);
        }
    }

    #[test]
    fn polar_winding_with_only_a_seam_is_ambiguous() {
        let c = Coord::new;
        let ring = [
            c(-180.0, 90.0),
            c(180.0, 0.0),
            c(-60.0, 10.0),
            c(60.0, 20.0),
            c(180.0, 30.0),
            c(-60.0, 40.0),
            c(60.0, 50.0),
            c(180.0, 60.0),
            c(180.0, -90.0),
            c(90.0, -80.0),
        ];
        assert!(ring_area_is_ambiguous(&ring));
        assert!(geodesic_ring_area_m2(&ring).is_nan());
        let (perimeter, area) = wgs84_ring_perimeter_area_m2(&ring);
        assert!(perimeter.is_finite() && area.is_nan());
    }

    #[test]
    fn ambiguous_polar_winding_has_no_single_area() {
        let c = Coord::new;
        let ring = [c(-180.0, 90.0), c(180.0, 90.0), c(-90.0, -90.0), c(90.0, -80.0)];
        for reversed in [false, true] {
            for start in 0..ring.len() {
                let mut shape = ring.to_vec();
                if reversed {
                    shape.reverse();
                }
                shape.rotate_left(start);
                for closed in [false, true] {
                    let mut points = shape.clone();
                    if closed {
                        points.push(points[0]);
                    }
                    let area = geodesic_ring_area_m2(&points);
                    assert!(area.is_nan(), "{points:?}: no single spherical region, got {area}");
                    let (perimeter, area) = wgs84_ring_perimeter_area_m2(&points);
                    assert!(perimeter.is_finite() && perimeter > 0.0);
                    assert!(area.is_nan(), "{points:?}: no single ellipsoidal region");
                }
            }
        }
        // The explicitly whole-world rectangle remains a well-defined area.
        let world = [c(-180.0, -90.0), c(180.0, -90.0), c(180.0, 90.0), c(-180.0, 90.0)];
        let globe = 4.0 * std::f64::consts::PI * EARTH_RADIUS_M * EARTH_RADIUS_M;
        assert!((geodesic_ring_area_m2(&world) / globe - 1.0).abs() < 1e-12);
        assert!(wgs84_ring_perimeter_area_m2(&world).1.is_finite());
    }

    #[test]
    fn densified_world_boundaries_do_not_become_ambiguous_from_roundoff() {
        let corners = [
            Coord::new(-180.0, -90.0),
            Coord::new(180.0, -90.0),
            Coord::new(180.0, 90.0),
            Coord::new(-180.0, 90.0),
            Coord::new(-180.0, -90.0),
        ];
        for keep_full in [false, true] {
            let mut ring = Vec::new();
            for (edge, pair) in corners.windows(2).enumerate() {
                let steps = if keep_full && edge == 0 { 1 } else { 10_000 };
                for index in 0..steps {
                    let t = index as f64 / steps as f64;
                    ring.push(Coord::new(pair[0].x + (pair[1].x - pair[0].x) * t, pair[0].y + (pair[1].y - pair[0].y) * t));
                }
            }
            for reverse in [false, true] {
                if reverse {
                    ring.reverse();
                }
                assert!(!ring_area_is_ambiguous(&ring), "full={keep_full}, reverse={reverse}");
                let surface = 4.0 * std::f64::consts::PI * EARTH_RADIUS_M * EARTH_RADIUS_M;
                let area = geodesic_ring_area_m2(&ring);
                assert!(area.is_finite() && (area / surface - 1.0).abs() < 1e-12, "{area}");
                assert!(wgs84_ring_perimeter_area_m2(&ring).1.is_finite());
            }
        }
    }

    #[test]
    fn wgs84_measures_match_geographiclib() {
        // References from GeographicLib (pyproj.Geod / Polygon.Compute on WGS 84), cross-checked
        // against `geo`'s GeodesicArea: a 1° × 1° box at the equator, and the same box stretched
        // across the antimeridian (unwrapped, 179° to 181°).
        let equator = &[Coord::new(0.0, 0.0), Coord::new(0.0, 1.0), Coord::new(1.0, 1.0), Coord::new(1.0, 0.0)];
        let (perimeter, area) = wgs84_ring_perimeter_area_m2(equator);
        assert!((perimeter - 443_770.917).abs() < 0.001, "{perimeter}");
        assert!((area - 12_308_778_361.469).abs() < 0.001, "{area}");
        // The ring traced the other way around gives the same magnitude.
        let (perimeter_reversed, area_reversed) = wgs84_ring_perimeter_area_m2(&equator.iter().rev().copied().collect::<Vec<_>>());
        assert!((perimeter - perimeter_reversed).abs() < 1e-6);
        assert!((area - area_reversed).abs() < 1e-6);
        // Unwrapped longitudes past the antimeridian are one ring, not a world-sized complement.
        let across = &[Coord::new(179.0, 0.0), Coord::new(181.0, 0.0), Coord::new(181.0, 1.0), Coord::new(179.0, 1.0)];
        let (perimeter_across, area_across) = wgs84_ring_perimeter_area_m2(across);
        assert!((perimeter_across - 666_393.055).abs() < 0.001, "{perimeter_across}");
        assert!((area_across - 24_619_443_759.277).abs() < 0.001, "{area_across}");
        // Segment lengths are inverse geodesic solves, unwrapped or folded alike.
        assert!((wgs84_length_m(&[Coord::new(0.0, 0.0), Coord::new(3.0, 0.0)]) - 3.0 * 111_319.490_8).abs() < 0.01);
        assert!((wgs84_distance_m(Coord::new(179.0, 0.0), Coord::new(-179.0, 0.0)) - 222_638.981_6).abs() < 0.001);
        // A broken coordinate is no length and no area (see `haversine_m`'s guard).
        assert!(wgs84_length_m(&[Coord::new(0.0, 0.0), Coord::new(f64::NAN, 0.0)]).is_nan());
        let (perimeter_broken, area_broken) =
            wgs84_ring_perimeter_area_m2(&[Coord::new(0.0, 0.0), Coord::new(0.0, 1.0), Coord::new(f64::NAN, 0.0)]);
        assert!(perimeter_broken.is_nan() && area_broken.is_nan());
        // A single vertex keeps its zero; two vertices are a segment there and back (2d, no area).
        assert_eq!(wgs84_ring_perimeter_area_m2(&[Coord::new(0.0, 0.0)]), (0.0, 0.0));
        let (perimeter_two, area_two) = wgs84_ring_perimeter_area_m2(&[Coord::new(0.0, 0.0), Coord::new(1.0, 0.0)]);
        assert!((perimeter_two - 2.0 * 111_319.490_8).abs() < 0.01, "{perimeter_two}");
        assert_eq!(area_two, 0.0);
        // Degenerate rings keep their zero, unless a vertex is broken: that is NaN, even in a short ring.
        let (perimeter_short, area_short) = wgs84_ring_perimeter_area_m2(&[Coord::new(f64::NAN, f64::NAN), Coord::new(0.0, 0.0)]);
        assert!(perimeter_short.is_nan() && area_short.is_nan());
    }

    #[test]
    fn a_non_finite_coordinate_is_no_distance_no_area_and_no_place() {
        // A NaN vertex once measured as a quarter of the Earth's circumference (π·R = 20 015 114
        // m) through `h.sqrt().min(1.0)`: `f64::min` ignores NaN. It must be no distance at all, so
        // sums and panels stay visibly broken and comparisons exclude it (NaN <= r is false).
        let nan = f64::NAN;
        let origin = Coord::new(0.0, 0.0);
        for near in [Coord::new(nan, 0.0), Coord::new(0.0, nan), Coord::new(f64::INFINITY, 0.0)] {
            assert!(haversine_m(origin, near).is_nan());
            assert!(geodesic_length_m(&[origin, near, origin]).is_nan());
        }
        // A finite edge next to a broken one keeps measuring, through its own points.
        assert!((geodesic_length_m(&[origin, Coord::new(1.0, 0.0)]) - 111_195.080).abs() < 0.01);
        // Ring area: a NaN absorbing match on the older behaviour, then anything broken.
        assert!(geodesic_ring_area_m2(&[origin, Coord::new(1.0, 0.0), Coord::new(1.0, 1.0), Coord::new(nan, 1.0)]).is_nan());
        assert!(geodesic_ring_area_m2(&[origin, Coord::new(1.0, 0.0), Coord::new(1.0, f64::INFINITY)]).is_nan());
        // Heading away from a broken place is no place either.
        assert!(geodesic_destination(Coord::new(10.0, 50.0), 90.0, f64::NAN).x.is_nan());
    }

    #[test]
    fn geodesic_length_matches_summed_haversine_across_the_antimeridian() {
        let poly = [Coord::new(0.0, 0.0), Coord::new(10.0, 20.0), Coord::new(179.0, -30.0), Coord::new(-179.0, -31.0)];
        let per_segment: f64 = poly.windows(2).map(|w| haversine_m(w[0], w[1])).sum();
        assert_eq!(geodesic_length_m(&poly), per_segment);
    }

    #[test]
    fn azimuthal_equidistant_keeps_distances_from_its_centre() {
        // Distances and azimuths on WGS 84 from GeographicLib (pyproj.Geod(ellps="WGS84").inv).
        for (centre, (x, y), s12, azi1) in [
            ((10.0, 60.0), (10.0, 70.0), 1_114_907.908_280, 0.0),
            ((10.0, 60.0), (-170.0, 80.0), 4_464_718.767_198, 0.0),
            ((10.0, 60.0), (40.0, 20.0), 5_037_117.406_299, 138.557_618_275_686),
            ((10.0, 60.0), (179.5, -30.0), 16_578_966.423_682, 17.827_025_777_178),
            ((2.3522, 48.8566), (-0.1276, 51.5072), 343_896.891_267, -30.048_593_801_903),
        ] {
            let (centre, c) = (Coord::new(centre.0, centre.1), Coord::new(x, y));
            let p = Aeqd::new(centre);
            let m = p.forward(c);
            assert!((m.x.hypot(m.y) - s12).abs() < 1e-5, "{c:?}: {m:?}");
            assert!((m.x.atan2(m.y).to_degrees() - azi1).abs() < 1e-9, "{c:?}: {m:?}");
            let back = p.inverse(m);
            assert!((back.x - x).abs() < 1e-9 && (back.y - y).abs() < 1e-9, "{c:?} → {back:?}");
        }
        assert_eq!(Aeqd::new(Coord::new(10.0, 60.0)).forward(Coord::new(10.0, 60.0)), Coord::new(0.0, 0.0));
        assert!((wgs84_distance_m(Coord::new(2.3522, 48.8566), Coord::new(-0.1276, 51.5072)) - 343_896.891_267).abs() < 1e-5);
        // A tenth of a degree north from the equator is shorter than on the mean sphere, but not
        // than on the sphere of the smallest radius of curvature.
        let (a, b) = (Coord::new(0.0, 0.0), Coord::new(0.0, 0.1));
        assert!(wgs84_distance_m(a, b) < haversine_m(a, b));
        assert!(wgs84_distance_m(a, b) >= haversine_m(a, b) * WGS84_MIN_RADIUS_M / EARTH_RADIUS_M);
        // Metres that aren't finite are no place, not half of one.
        for m in [Coord::new(f64::NAN, 0.0), Coord::new(f64::INFINITY, 0.0), Coord::new(0.0, f64::NEG_INFINITY)] {
            let c = Aeqd::new(Coord::new(10.0, 45.0)).inverse(m);
            assert!(c.x.is_nan() && c.y.is_nan(), "{m:?} → {c:?}");
        }
        // The inverse stays next to the centre's longitude rather than folding into −180..180.
        assert!((Aeqd::new(Coord::new(179.0, 0.0)).inverse(Coord::new(300_000.0, 0.0)).x - 181.694_945_852_359).abs() < 1e-9);
        // Centred on a pole, the azimuth is taken from the centre's meridian, both ways.
        for pole in [90.0, -90.0] {
            let p = Aeqd::new(Coord::new(25.0, pole));
            for c in [Coord::new(25.0, pole * 0.9), Coord::new(-100.0, pole * 0.8), Coord::new(170.0, pole * 0.95)] {
                let back = p.inverse(p.forward(c));
                assert!((wrap_lon(back.x - c.x)).abs() < 1e-9 && (back.y - c.y).abs() < 1e-9, "{c:?} → {back:?}");
            }
        }
    }

    #[test]
    fn geodesic_destination_and_interpolation() {
        // Destinations on WGS 84 from GeographicLib (pyproj.Geod(ellps="WGS84").fwd): 10 km from
        // the equator and from 46.5° and 60°N (#888), 1,000 km from 60°N, and from either pole.
        for (lon, lat, az, d, lon2, lat2) in [
            (0.0, 0.0, 0.0, 10_000.0, 0.0, 0.090_436_946_951),
            (0.0, 0.0, 90.0, 10_000.0, 0.089_831_528_412, 0.0),
            (10.5, 46.5, 0.0, 10_000.0, 10.5, 46.589_958_829_711),
            (10.5, 46.5, 90.0, 10_000.0, 10.630_271_666_436, 46.499_925_816_591),
            (10.5, 46.5, 225.0, 10_000.0, 10.407_991_242_765, 46.436_351_607_689),
            (30.0, 60.0, 90.0, 10_000.0, 30.179_211_026_165, 59.999_878_434_646),
            (10.0, 60.0, 0.0, 1_000_000.0, 10.0, 68.969_947_795_748),
            (10.0, 60.0, 300.0, 1_000_000.0, -7.552_217_029_942, 63.435_692_055_327),
            (0.0, 90.0, 0.0, 1_000_000.0, 180.0, 81.046_232_815_951),
            (0.0, -90.0, 90.0, 1_000_000.0, 90.0, -81.046_232_815_951),
        ] {
            let p = geodesic_destination(Coord::new(lon, lat), az, d);
            // 1e-11° is about a micrometre.
            assert!(wrap_lon(p.x - lon2).abs() < 1e-11 && (p.y - lat2).abs() < 1e-11, "{lon} {lat} {az}: {p:?}");
        }
        let (a, b) = (Coord::new(2.3522, 48.8566), Coord::new(-0.1276, 51.5072));
        let mid = geodesic_interpolate(a, b, 0.5);
        assert!((haversine_m(a, mid) - haversine_m(mid, b)).abs() < 1e-3);
        // Across the antimeridian, the path stays continuous.
        assert!((geodesic_interpolate(Coord::new(179.0, 0.0), Coord::new(-179.0, 0.0), 0.5).x - 180.0).abs() < 1e-9);
        assert_eq!(spherical_centre(&[Coord::new(179.0, 0.0), Coord::new(-179.0, 0.0)]).map(|c| c.x.abs().round()), Some(180.0));
        assert!(spherical_centre(&[]).is_none());
    }

    #[test]
    fn the_analysis_plane_is_web_mercator_within_its_square_and_reaches_the_poles() {
        // Within ±MAX_LAT, to the bit what the tools used before, both ways, past ±180° too.
        for i in -40..=40 {
            let lat = MAX_LAT * f64::from(i) / 40.0;
            for lon in [-540.0, -180.0, -0.5, 0.0, 33.3, 179.999, 180.0, 541.25] {
                let c = Coord::new(lon, lat);
                let (m, p) = (lonlat_to_merc(c), lonlat_to_plane(c));
                assert!(m.x.to_bits() == p.x.to_bits() && m.y.to_bits() == p.y.to_bits(), "{c:?}");
                let (a, b) = (merc_to_lonlat(m), plane_to_lonlat(m));
                assert!(a.x.to_bits() == b.x.to_bits() && a.y.to_bits() == b.y.to_bits(), "{c:?}");
            }
        }
        // Beyond, nothing is clamped: latitude climbs on to the poles, about 26,424 km out, meeting
        // Web Mercator at its edge with the same slope (R·sec 85.05° a radian), and comes back.
        let y = |lat: f64| lonlat_to_plane(Coord::new(10.0, lat)).y;
        assert!((y(90.0) - 26_423_582.9).abs() < 1.0 && (y(-90.0) + y(90.0)).abs() < 1e-6, "{}", y(90.0));
        let h = 1e-6;
        let (below, above) = ((y(MAX_LAT) - y(MAX_LAT - h)) / h, (y(MAX_LAT + h) - y(MAX_LAT)) / h);
        assert!((above / below - 1.0).abs() < 1e-5, "{below} {above}");
        let mut last = y(MAX_LAT);
        for i in 1..=100 {
            let lat = MAX_LAT + (90.0 - MAX_LAT) * f64::from(i) / 100.0;
            assert!(y(lat) > last, "{lat}");
            last = y(lat);
            for sign in [1.0, -1.0] {
                let back = plane_to_lonlat(lonlat_to_plane(Coord::new(-170.25, sign * lat)));
                assert!((back.y - sign * lat).abs() < 1e-9 && (back.x + 170.25).abs() < 1e-12, "{lat}: {back:?}");
            }
        }
        // Junk stays junk or lands on a pole, never panics or goes past one.
        assert!(lonlat_to_plane(Coord::new(0.0, f64::NAN)).y.is_nan() && plane_to_lonlat(Coord::new(0.0, f64::NAN)).y.is_nan());
        assert_eq!(lonlat_to_plane(Coord::new(0.0, f64::INFINITY)).y, y(90.0));
        assert_eq!(lonlat_to_plane(Coord::new(0.0, -1e300)).y, y(-90.0));
        assert_eq!((plane_to_lonlat(Coord::new(0.0, 1e300)).y, plane_to_lonlat(Coord::new(0.0, f64::NEG_INFINITY)).y), (90.0, -90.0));
    }

    #[test]
    fn wgs84_direct_and_inverse_agree_with_geographiclib() {
        // GeographicLib's destinations (pyproj.Geod(ellps="WGS84").fwd), as in the test above.
        for (lon, lat, az, d, lon2, lat2) in [
            (0.0, 0.0, 90.0, 10_000.0, 0.089_831_528_412, 0.0),
            (10.5, 46.5, 225.0, 10_000.0, 10.407_991_242_765, 46.436_351_607_689),
            (10.0, 60.0, 300.0, 1_000_000.0, -7.552_217_029_942, 63.435_692_055_327),
        ] {
            let (a, b) = (Coord::new(lon, lat), Coord::new(lon2, lat2));
            let (p, azi2) = wgs84_direct(a, az, d);
            assert!((p.x - lon2).abs() < 1e-11 && (p.y - lat2).abs() < 1e-11, "{lon} {lat} {az}: {p:?}");
            // Back again: the same length and azimuths, the one at `b` as the direct problem gave it.
            let (s12, azi1, back) = wgs84_inverse(a, b);
            assert!((s12 - d).abs() < 1e-6 && (azi1 - az).rem_euclid(360.0).min((az - azi1).rem_euclid(360.0)) < 1e-9, "{s12} {azi1}");
            assert!((back - azi2).abs() < 1e-9 && (s12 - wgs84_distance_m(a, b)).abs() < 1e-9, "{back} {azi2}");
        }
        // Folded: in range exactly as given; out of range by an exact remainder.
        for (x, folded) in
            [(0.01_f64, 0.01_f64), (-180.0, -180.0), (180.0, 180.0), (190.0, -170.0), (-190.0, 170.0), (900.25, 180.25 - 360.0)]
        {
            assert_eq!(fold_lon(x).to_bits(), folded.to_bits(), "{x}");
        }
        assert!((-180.0..=180.0).contains(&fold_lon(1e308)) && fold_lon(f64::INFINITY).is_nan());
        // Heading east along the equator, still east; across the antimeridian the longitude is
        // GeographicLib's, in -180..180 (one degree is 111,319.49 m there).
        let (p, azi2) = wgs84_direct(Coord::new(179.5, 0.0), 90.0, 111_319.490_793);
        assert!((p.x + 179.5).abs() < 1e-9 && p.y.abs() < 1e-12 && (azi2 - 90.0).abs() < 1e-12, "{p:?} {azi2}");
        // A coordinate that isn't finite is no place, not a pole the clamp would make of it.
        let (o, inf) = (Coord::new(0.0, 0.0), Coord::new(0.0, f64::INFINITY));
        let (s, a1, a2) = wgs84_inverse(o, inf);
        assert!(s.is_nan() && a1.is_nan() && a2.is_nan() && wgs84_distance_m(o, inf).is_nan());
        for (c, azimuth, dist) in [(inf, 0.0, 1.0), (o, f64::NAN, 1.0), (o, 0.0, f64::INFINITY)] {
            let (p, azi2) = wgs84_direct(c, azimuth, dist);
            assert!(!p.is_finite() && azi2.is_nan(), "{c:?} {azimuth} {dist}");
        }
    }

    #[test]
    fn wgs84_keeps_its_precision_at_the_antimeridian_and_the_poles() {
        // GeographicLib's own values (Python geographiclib 2.1) where coordinates are hardest: a few
        // centimeters across the antimeridian and across a pole, near either pole, from a pole and
        // nearly to the antipode. Lengths agree to a nanometer, places to 1e-12° (0.1 µm).
        let inverse = [
            (179.999_999_9, 0.0, -179.999_999_9, 0.0, 0.022_263_896_836_887_686, 90.0, 90.0),
            (0.0, 89.999_999_9, 180.0, 89.999_999_9, 0.022_338_794_585_611_93, 0.0, 180.0),
            (10.0, 89.99, 100.0, 89.99, 1_579.591_403_108_320_3, 45.000_000_436_332_31, 134.999_999_563_667_7),
            (-179.99, -89.999, 179.99, -89.999, 0.038_988_553_759_428_43, -90.009_999_999_998_47, -89.990_000_000_001_53),
            (0.0, 0.0, 179.9999, 0.0001, 20_003_920.400_274_97, 0.009_500_228_497_032_422, 179.990_499_771_502_95),
        ];
        for (lon1, lat1, lon2, lat2, s12, azi1, azi2) in inverse {
            let (s, a1, a2) = wgs84_inverse(Coord::new(lon1, lat1), Coord::new(lon2, lat2));
            assert!((s - s12).abs() < 1e-9 && (a1 - azi1).abs() < 1e-9 && (a2 - azi2).abs() < 1e-9, "{lon1} {lat1}: {s} {a1} {a2}");
        }
        let direct = [
            (179.9999, -60.0, 80.0, 50.0, -179.999_217_557_877_2, -59.999_922_066_609_36, 79.999_235_783_004_34),
            (45.0, 89.9999, 225.0, 30.0, 11.771_982_732_812_425, 89.999_653_408_535_92, -168.228_017_267_021_84),
            (0.0, 90.0, 30.0, 1000.0, 150.0, 89.991_046_965_968_72, 180.0),
            (-180.0, -89.999_999_9, 0.0, 0.05, -180.0, -89.999_999_452_348_3, 0.0),
        ];
        for (lon1, lat1, azi1, s12, lon2, lat2, azi2) in direct {
            let (p, a2) = wgs84_direct(Coord::new(lon1, lat1), azi1, s12);
            // The same meridian: on the antimeridian, the Rust port says 180 where Python says -180.
            let dlon = wrap_lon(p.x - lon2).abs();
            assert!(dlon < 1e-12 && (p.y - lat2).abs() < 1e-12 && (a2 - azi2).abs() < 1e-9, "{lon1} {lat1}: {p:?} {a2}");
        }
    }

    #[test]
    fn nonfinite_coordinates_do_not_become_valid_distances() {
        let c = Coord::new;
        let origin = c(0.0, 0.0);
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for invalid in [c(value, 0.0), c(0.0, value)] {
                assert!(haversine_m(origin, invalid).is_nan());
                assert!(haversine_m(invalid, origin).is_nan());
                assert!(geodesic_length_m(&[origin, invalid, c(1.0, 0.0)]).is_nan());
                assert!(geodesic_ring_area_m2(&[origin, invalid, c(1.0, 1.0)]).is_nan());
            }
        }
        assert_eq!(haversine_m(origin, origin), 0.0);
        let antipodal = haversine_m(origin, c(180.0, 0.0));
        assert!((antipodal - std::f64::consts::PI * EARTH_RADIUS_M).abs() < 1e-6);
        let across = haversine_m(c(179.0, 0.0), c(-179.0, 0.0));
        let unwrapped = haversine_m(c(179.0, 0.0), c(181.0, 0.0));
        assert!((across - unwrapped).abs() < 1e-6);
    }

    #[test]
    fn haversine_paris_london() {
        let d = haversine_m(Coord::new(2.3522, 48.8566), Coord::new(-0.1276, 51.5072));
        assert!((d - 343_500.0).abs() < 2_000.0, "{d}");
    }

    #[test]
    fn nonfinite_measurements_display_as_unavailable() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(format_distance(value), "—");
            assert_eq!(format_area(value), "—");
            for coord in [Coord::new(value, 50.0), Coord::new(12.0, value), Coord::new(value, value)] {
                assert_eq!(format_lonlat(coord), "—");
                assert_eq!(format_dms(coord), "—");
            }
        }
        assert_eq!(format_area(0.0), "0 m²");
        assert_eq!(format_lonlat(Coord::new(-12.5, 45.25)), "12.5000° W  45.2500° N");
        assert_eq!(format_lonlat(Coord::new(0.0, 0.0)), "0.0000° E  0.0000° N");
    }

    #[test]
    fn distances_and_areas_switch_unit_when_they_round_up() {
        assert_eq!(format_distance(850.0), "850 m");
        assert_eq!(format_distance(0.0), "0 m");
        assert_eq!(format_distance(0.005), "5 mm");
        assert_eq!(format_distance(0.0096), "1 cm");
        assert_eq!(format_distance(0.2), "20 cm");
        assert_eq!(format_distance(0.996), "1 m");
        // Ties round away from zero, as the unit is picked; signs stay.
        assert_eq!(format_distance(0.0045), "5 mm");
        assert_eq!(format_distance(0.0095), "1 cm");
        assert_eq!(format_distance(0.995), "1 m");
        assert_eq!(format_distance(-0.005), "-5 mm");
        assert_eq!(format_distance(-0.2), "-20 cm");
        // Under half a millimetre, never 0 (#923).
        assert_eq!(format_distance(0.0002), "0.2 mm");
        assert_eq!(format_distance(-0.0002), "-0.2 mm");
        assert_eq!(format_distance(0.00001), "< 0.1 mm");
        assert_eq!(format_distance(-0.00001), "> -0.1 mm");
        assert_eq!(format_distance(999.4), "999 m");
        assert_eq!(format_distance(999.6), "1.0 km");
        assert_eq!(format_distance(12_500.0), "12.5 km");
        assert_eq!(format_distance(99_960.0), "100 km");
        assert_eq!(format_distance(99_949.0), "99.9 km");
        assert_eq!(format_distance(1_000.0), "1.0 km");
        assert_eq!(format_area(999_999.4), "999999 m²");
        assert_eq!(format_area(999_999.6), "1.0 km²");
        assert_eq!(format_area(1.0e6), "1.0 km²");
        // Negative distances and areas also switch units based on magnitude.
        assert_eq!(format_distance(-850.0), "-850 m");
        assert_eq!(format_distance(-999.4), "-999 m");
        assert_eq!(format_distance(-999.6), "-1.0 km");
        assert_eq!(format_distance(-12_500.0), "-12.5 km");
        assert_eq!(format_distance(-99_949.0), "-99.9 km");
        assert_eq!(format_distance(-99_960.0), "-100 km");
        assert_eq!(format_area(-0.3), "0 m²");
        assert_eq!(format_area(-0.0), "0 m²");
        assert_eq!(format_area(-0.6), "-1 m²");
        assert_eq!(format_area(-999_999.4), "-999999 m²");
        assert_eq!(format_area(-999_999.6), "-1.0 km²");
        assert_eq!(format_area(-1.0e6), "-1.0 km²");
        assert_eq!(format_area(-5.0e6), "-5.0 km²");
    }

    #[test]
    fn destination_lies_at_its_distance_and_bearing() {
        let from = Coord::new(10.0, 60.0);
        for az in [0.0, 45.0, 90.0, 180.0, 270.0, 333.0] {
            let to = geodesic_destination(from, az, 500_000.0);
            let m = Aeqd::new(from).forward(to);
            assert!((m.x.hypot(m.y) - 500_000.0).abs() < 1e-6, "{az}: {to:?}");
            assert!(wrap_lon(m.x.atan2(m.y).to_degrees() - az).abs() < 1e-9, "{az}: {to:?}");
        }
        // North and east go where they say: 60° to 61°N is 111,420.728 m on WGS 84 (GeographicLib).
        assert!((geodesic_destination(from, 0.0, 111_420.727_870).y - 61.0).abs() < 1e-9);
        assert!(geodesic_destination(from, 90.0, 1000.0).x > 10.0);
        // Longitudes stay continuous across the antimeridian, so the points around one centre
        // form one shape (Tissot's indicatrix draws its circles with them).
        assert!(geodesic_destination(Coord::new(179.9, 0.0), 90.0, 100_000.0).x > 180.0);
    }

    #[test]
    fn ring_area_one_degree_square_at_equator() {
        let ring = [Coord::new(0.0, 0.0), Coord::new(1.0, 0.0), Coord::new(1.0, 1.0), Coord::new(0.0, 1.0)];
        let a = geodesic_ring_area_m2(&ring);
        assert!((a - 1.2364e10).abs() / 1.2364e10 < 0.01, "{a}");
    }

    #[test]
    fn ring_area_across_the_antimeridian() {
        let c = Coord::new;
        let across = geodesic_ring_area_m2(&[c(179.0, 0.0), c(-179.0, 0.0), c(-179.0, 2.0), c(179.0, 2.0)]);
        let unwrapped = geodesic_ring_area_m2(&[c(179.0, 0.0), c(181.0, 0.0), c(181.0, 2.0), c(179.0, 2.0)]);
        let reversed = geodesic_ring_area_m2(&[c(179.0, 2.0), c(-179.0, 2.0), c(-179.0, 0.0), c(179.0, 0.0)]);
        assert!((across - unwrapped).abs() / unwrapped < 1e-9, "{across} vs {unwrapped}");
        assert!((reversed - unwrapped).abs() / unwrapped < 1e-9, "{reversed}");
        // The same box away from the antimeridian has the same area to within the latitude band.
        let plain = geodesic_ring_area_m2(&[c(0.0, 0.0), c(2.0, 0.0), c(2.0, 2.0), c(0.0, 2.0)]);
        assert!((across - plain).abs() / plain < 1e-9, "{across} vs {plain}");
        // A full -180° to 180° edge is not collapsed: a world-wide band keeps its area.
        let band = geodesic_ring_area_m2(&[c(-180.0, -10.0), c(180.0, -10.0), c(180.0, 10.0), c(-180.0, 10.0)]);
        assert!(band > 1.0e13, "{band}");
    }

    #[test]
    fn nice_distances() {
        assert_eq!(nice_distance(730.0), 500.0);
        assert_eq!(nice_distance(2_400.0), 2_000.0);
        assert_eq!(nice_distance(0.0), 0.0);
        assert_eq!(nice_distance(f64::NAN), 0.0);
    }

    #[test]
    fn dms_carries_rounded_seconds() {
        assert_eq!(format_dms(Coord::new(12.5, -45.25)), "12°30′00.0″ E  45°15′00.0″ S");
        // 59.96″ rounds to 60.0″, which carries into the minutes and then the degrees.
        assert_eq!(format_dms(Coord::new(10.99999, -45.99999)), "11°00′00.0″ E  46°00′00.0″ S");
        assert_eq!(format_dms(Coord::new(-0.5 - 59.97 / 3600.0, 0.0)), "0°31′00.0″ W  0°00′00.0″ N");
    }

    #[test]
    fn extent_ops() {
        let mut e = Extent::EMPTY;
        assert!(e.is_empty());
        e.expand(Coord::new(1.0, 2.0));
        e.expand(Coord::new(f64::NAN, 0.0));
        e.expand(Coord::new(-1.0, 5.0));
        assert_eq!(e, Extent::new(-1.0, 2.0, 1.0, 5.0));
        assert!(e.intersects(&Extent::new(0.0, 0.0, 3.0, 3.0)));
        assert!(!e.intersects(&Extent::EMPTY));
    }
}
