//! Conversions between RustGIS geometry and `geo` types, in the plane a run works in
//! ([`Plane`]): for a run in longitude/latitude, the analysis plane
//! ([`rustgis_geom::lonlat_to_plane`]), Web Mercator meters within ±85.05°, carried on to the poles
//! beyond, so polar geometry isn't clamped onto 85.05° (#1033); for a run in its first input's own
//! projected system (#2032 C2e), that system's own coordinates, as they are.

use geo::{Coord as GC, LineString, MultiLineString, MultiPoint, MultiPolygon, Point, Polygon};
use rustgis_data::Geometry;
use rustgis_geom::{Coord, lonlat_to_plane, plane_to_lonlat};

/// The plane a run's geometry is worked on in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Plane {
    /// Longitude/latitude, worked on in the analysis plane ([`rustgis_geom::lonlat_to_plane`]),
    /// with the poles and the antimeridian where they are in it.
    #[default]
    Analysis,
    /// A projected system's own coordinates, worked on as they are: a plane with no poles or
    /// antimeridian in it (#2032 C2e).
    Own,
}

/// The plane of a run and the length of its unit on the ground: what [`install`] puts in place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunPlane {
    pub plane: Plane,
    /// Metres per unit of a projected system's own plane ([`Plane::Own`]): 1 for one in metres,
    /// 0.3048 for one in feet. 1 in the analysis plane, whose metres are Web Mercator's.
    pub unit_m: f64,
    /// Which way a projected system's x and y point: the (east, north) of a step of one along
    /// each ([`rustgis_data::raster::Crs::axis_directions`]); east, then north, otherwise.
    pub axes: [(f64, f64); 2],
}

/// East, then north: the axes of a plane whose x points east and y north.
pub const EAST_NORTH: [(f64, f64); 2] = [(1.0, 0.0), (0.0, 1.0)];

thread_local! {
    static PLANE: std::cell::Cell<RunPlane> =
        const { std::cell::Cell::new(RunPlane { plane: Plane::Analysis, unit_m: 1.0, axes: EAST_NORTH }) };
}

/// The plane of the run on this thread ([`install`]): [`Plane::Analysis`] unless a run in a
/// projected system installed [`Plane::Own`]. The threads of [`crate::parallel`] take it over from
/// the thread that starts them.
pub fn plane() -> Plane {
    run_plane().plane
}

/// The plane of the run on this thread with its unit ([`install`]).
pub fn run_plane() -> RunPlane {
    PLANE.with(std::cell::Cell::get)
}

/// A distance on the ground in metres as a length in a projected system's own plane, for a run
/// there ([`Plane::Own`]): measured flat, as in a projected system tools measure (#2032 C2e-3).
/// `None` in a run in longitude/latitude, which measures on the ellipsoid.
pub fn own_length(metres: f64) -> Option<f64> {
    own_unit_m().map(|u| metres / u)
}

/// Metres per unit of a projected system's own plane, for a run there ([`Plane::Own`]).
pub fn own_unit_m() -> Option<f64> {
    let r = run_plane();
    (r.plane == Plane::Own).then_some(r.unit_m)
}

/// Puts the plane back as it was when dropped.
pub struct PlaneInstalled(RunPlane);

impl Drop for PlaneInstalled {
    fn drop(&mut self) {
        PLANE.with(|p| p.set(self.0));
    }
}

/// Makes `run` the plane of the work on this thread until the guard is dropped.
pub fn install(run: RunPlane) -> PlaneInstalled {
    PlaneInstalled(PLANE.with(|p| p.replace(run)))
}

/// A step `(dx, dy)` in a projected system's own plane as (east, north), by the order and
/// direction of its axes ([`RunPlane::axes`]): for a direction from grid north.
pub fn own_east_north(dx: f64, dy: f64) -> (f64, f64) {
    let [(xe, xn), (ye, yn)] = run_plane().axes;
    (dx * xe + dy * ye, dx * xn + dy * yn)
}

/// A place of a run's geometry in its plane ([`plane`]): in the analysis plane for
/// longitude/latitude, as it is in a projected system's own.
pub fn to_plane(c: Coord) -> Coord {
    match plane() {
        Plane::Analysis => lonlat_to_plane(c),
        Plane::Own => c,
    }
}

/// A place in the run's plane back in its geometry's coordinates, undoing [`to_plane`].
pub fn from_plane(c: Coord) -> Coord {
    match plane() {
        Plane::Analysis => plane_to_lonlat(c),
        Plane::Own => c,
    }
}

/// Projects without folding the longitude: a ring kept unwrapped across the antimeridian (as a
/// geodesic buffer is) stays one piece, rather than folding its far vertex across the world.
fn gc(c: Coord) -> GC<f64> {
    let m = to_plane(c);
    GC { x: m.x, y: m.y }
}

fn back(c: &GC<f64>) -> Coord {
    from_plane(Coord::new(c.x, c.y))
}

/// The poles something spanning latitudes `south` to `north` reaches (to within
/// [`rustgis_geom::POLE_TOLERANCE`]): the North Pole, then the South Pole. None in a projected
/// system's own plane ([`Plane::Own`]), whose y is no latitude.
pub(crate) fn reaches_poles(south: f64, north: f64) -> (bool, bool) {
    if plane() == Plane::Own {
        return (false, false);
    }
    (north >= 90.0 - rustgis_geom::POLE_TOLERANCE, south <= -90.0 + rustgis_geom::POLE_TOLERANCE)
}

/// A run of coordinates as a `LineString` in the run's plane: a ring's (closed when its coordinates
/// close it) or a line's, which it doesn't close.
pub fn ring(r: &[Coord]) -> LineString<f64> {
    LineString::new(r.iter().map(|c| gc(*c)).collect())
}

pub fn to_multipolygon(g: &Geometry) -> Option<MultiPolygon<f64>> {
    match g {
        Geometry::Polygon(polys) => Some(MultiPolygon::new(
            polys
                .iter()
                .filter_map(|rings| {
                    let mut it = rings.iter();
                    let outer = ring(it.next()?);
                    Some(Polygon::new(outer, it.map(|r| ring(r)).collect()))
                })
                .collect(),
        )),
        _ => None,
    }
}

pub fn to_multiline(g: &Geometry) -> Option<MultiLineString<f64>> {
    match g {
        Geometry::Line(parts) => Some(MultiLineString::new(parts.iter().map(|p| ring(p)).collect())),
        _ => None,
    }
}

pub fn to_multipoint(g: &Geometry) -> Option<MultiPoint<f64>> {
    match g {
        Geometry::Point(c) => Some(MultiPoint::new(vec![Point(gc(*c))])),
        Geometry::MultiPoint(v) => Some(MultiPoint::new(v.iter().map(|c| Point(gc(*c))).collect())),
        _ => None,
    }
}

pub fn to_geo(g: &Geometry) -> geo::Geometry<f64> {
    match g {
        Geometry::Point(c) => geo::Geometry::Point(Point(gc(*c))),
        Geometry::MultiPoint(_) => {
            to_multipoint(g).map(geo::Geometry::MultiPoint).unwrap_or(geo::Geometry::MultiPoint(MultiPoint::new(vec![])))
        }
        Geometry::Line(_) => {
            to_multiline(g).map(geo::Geometry::MultiLineString).unwrap_or(geo::Geometry::MultiLineString(MultiLineString::new(vec![])))
        }
        Geometry::Polygon(_) => {
            to_multipolygon(g).map(geo::Geometry::MultiPolygon).unwrap_or(geo::Geometry::MultiPolygon(MultiPolygon::new(vec![])))
        }
    }
}

pub fn from_multipolygon(mp: &MultiPolygon<f64>) -> Option<Geometry> {
    let polys: Vec<Vec<Vec<Coord>>> =
        mp.0.iter()
            .map(|p| std::iter::once(p.exterior()).chain(p.interiors().iter()).map(|r| r.0.iter().map(back).collect()).collect())
            .collect();
    (!polys.is_empty()).then_some(Geometry::Polygon(polys))
}

pub fn from_line(ml: &MultiLineString<f64>) -> Option<Geometry> {
    let parts: Vec<Vec<Coord>> = ml.0.iter().filter(|l| l.0.len() >= 2).map(|l| l.0.iter().map(back).collect()).collect();
    (!parts.is_empty()).then_some(Geometry::Line(parts))
}

pub fn from_point(p: &Point<f64>) -> Geometry {
    Geometry::Point(back(&p.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Makes `plane` the plane of the work on this thread until the guard is dropped, its unit a
    /// metre and its axes east and north, as a run installs its own ([`install`]).
    fn install_plane(plane: Plane) -> PlaneInstalled {
        install(RunPlane { plane, unit_m: 1.0, axes: EAST_NORTH })
    }

    /// In a projected system's own plane a place is worked on as it is, and nothing reaches a
    /// pole however large its y; the analysis plane is put back after.
    #[test]
    fn a_projected_systems_plane_is_its_own_coordinates() {
        let c = Coord::new(683_944.533_251, 5_397_122.834_867);
        {
            let _own = install_plane(Plane::Own);
            assert_eq!((to_plane(c), from_plane(c)), (c, c));
            assert_eq!(reaches_poles(-10_000_000.0, 10_000_000.0), (false, false));
        }
        assert_eq!(plane(), Plane::Analysis);
        assert_eq!(reaches_poles(-90.0, 90.0), (true, true));
        assert_ne!(to_plane(Coord::new(10.0, 10.0)), Coord::new(10.0, 10.0));
    }

    #[test]
    fn an_antimeridian_ring_stays_a_sliver_through_geo() {
        // Unwrapped across the antimeridian (179.9° to 180.1°): about 22 km wide in Mercator, not
        // a band folded to the far west, and it comes back to the same longitudes.
        let ring =
            vec![Coord::new(179.9, 0.0), Coord::new(180.1, 0.0), Coord::new(180.1, 1.0), Coord::new(179.9, 1.0), Coord::new(179.9, 0.0)];
        let mp = to_multipolygon(&Geometry::Polygon(vec![vec![ring.clone()]])).unwrap();
        let xs: Vec<f64> = mp.0.iter().flat_map(|p| p.exterior().0.iter().map(|c| c.x)).collect();
        let width = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max) - xs.iter().copied().fold(f64::INFINITY, f64::min);
        assert!(width > 20_000.0 && width < 25_000.0, "{width}");
        let back: Vec<Coord> = mp.0.iter().flat_map(|p| p.exterior().0.iter().map(|c| plane_to_lonlat(Coord::new(c.x, c.y)))).collect();
        for (a, b) in ring.iter().zip(back.iter()) {
            assert!((a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9, "{a:?} → {b:?}");
        }
    }
}
