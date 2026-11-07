//! Conversions between RustGIS geometry and `geo` types, in the analysis plane
//! ([`rustgis_geom::lonlat_to_plane`]): Web Mercator meters within ±85.05°, carried on to the poles
//! beyond, so polar geometry isn't clamped onto 85.05° (#1033).

use geo::{Coord as GC, LineString, MultiLineString, MultiPoint, MultiPolygon, Point, Polygon};
use rustgis_data::Geometry;
use rustgis_geom::{Coord, lonlat_to_plane, plane_to_lonlat};

/// Projects without folding the longitude: a ring kept unwrapped across the antimeridian (as a
/// geodesic buffer is) stays one piece, rather than folding its far vertex across the world.
fn gc(c: Coord) -> GC<f64> {
    let m = lonlat_to_plane(c);
    GC { x: m.x, y: m.y }
}

fn back(c: &GC<f64>) -> Coord {
    plane_to_lonlat(Coord::new(c.x, c.y))
}

/// The poles something spanning latitudes `south` to `north` reaches (to within
/// [`rustgis_geom::POLE_TOLERANCE`]): the North Pole, then the South Pole.
pub(crate) fn reaches_poles(south: f64, north: f64) -> (bool, bool) {
    (north >= 90.0 - rustgis_geom::POLE_TOLERANCE, south <= -90.0 + rustgis_geom::POLE_TOLERANCE)
}

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
