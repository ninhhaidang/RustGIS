//! Coordinates from one coordinate system to another ([`Crs::to`]).
//!
//! A conversion goes through WGS 84 longitude/latitude, as every conversion in RustGIS has so
//! far: the source system's [`Transformer::to_lonlat`], then the target's
//! [`Transformer::from_lonlat`], with the checks each makes (a latitude past a pole, a place a
//! projection folds or can't bring back is refused). From or to longitude/latitude it is that one
//! step alone, so a conversion of longitude/latitude geometry gives exactly what the target's
//! transformer gives. Between two systems on the same datum other than WGS 84 the shift there and
//! back cancels only where both legs pick the same transformation; where the transformation chosen
//! depends on the place (a grid's edge, an area of use), a direct operation would differ, which
//! matters once layers keep their own coordinates (#2032 C2).

use super::crs::{Crs, Transformer};
use crate::Geometry;
use rustgis_geom::{Coord, Extent};

/// Converts coordinates from one system to another: [`Crs::to`].
#[derive(Clone, Debug)]
pub struct Reprojection {
    source: Crs,
    target: Crs,
    /// The source system's transformer, `None` for longitude/latitude (or the same system).
    unproject: Option<Transformer>,
    /// The target system's transformer, `None` for longitude/latitude (or the same system).
    project: Option<Transformer>,
}

impl Crs {
    /// The conversion of coordinates in this system to `other`'s. Between the same system it does
    /// nothing ([`Reprojection::is_identity`]). Fails as either system's [`Crs::transformer`]
    /// does. Each call asks both systems for their transformers again ([`Crs::transformer`], which
    /// keeps them for the run), so every conversion notes its systems for the missing-grid notice
    /// and waits for their grids, as asking for a transformer itself does.
    pub fn to(self, other: Crs) -> Result<Reprojection, String> {
        let same = self == other;
        let of = |crs: Crs| -> Result<Option<Transformer>, String> {
            if same || crs == Crs::LonLat { Ok(None) } else { crs.transformer().map(Some) }
        };
        Ok(Reprojection { source: self, target: other, unproject: of(self)?, project: of(other)? })
    }
}

impl Reprojection {
    /// The system it converts from.
    pub fn source(&self) -> Crs {
        self.source
    }

    /// The system it converts to.
    pub fn target(&self) -> Crs {
        self.target
    }

    /// Whether it converts a system to itself, leaving coordinates as they are.
    pub fn is_identity(&self) -> bool {
        self.source == self.target
    }

    /// A place in the target system, or `None` where the source system has no such place or the
    /// target can't hold it. Each system's transformer judges what it is given, as a call of its
    /// own did: Web Mercator takes a latitude past 85.05° (an infinite one too) as 85.05°, where
    /// it ends. Between the same system, or between longitude/latitude, the place itself when it
    /// is finite.
    pub fn point(&self, c: Coord) -> Option<Coord> {
        let lonlat = match &self.unproject {
            Some(t) => t.to_lonlat(c)?,
            None => c,
        };
        let out = match &self.project {
            Some(t) => t.from_lonlat(lonlat)?,
            None => lonlat,
        };
        out.is_finite().then_some(out)
    }

    /// A copy of `g` in the target system, its places that can't be converted NaN (which drawing
    /// skips; [`Reprojection::whole_geometry`] refuses them). Between the same system, `g` exactly
    /// as it is, a coordinate that isn't finite included.
    pub fn geometry(&self, g: &Geometry) -> Geometry {
        if self.is_identity() {
            return g.clone();
        }
        g.map_coords(&|c| self.point(c).unwrap_or(Coord::new(f64::NAN, f64::NAN)))
    }

    /// A copy of `g` in the target system, or `None` when any of its places can't be converted.
    pub fn whole_geometry(&self, g: &Geometry) -> Option<Geometry> {
        let out = self.geometry(g);
        let mut ok = true;
        out.for_each_coord(|c| ok &= c.is_finite());
        ok.then_some(out)
    }

    /// The box in the target system around `e`, from 17 by 17 places along its edges and inside
    /// it, as the map's `Flat::extent` boxes are found: a box a projection curves is
    /// followed between them, not exactly (a bulge between two samples may pass it). Places that
    /// don't convert are left out, so a box partly outside what the target holds gives the box of
    /// the part that converts; empty when none does. Between the same system, `e` as it is.
    pub fn extent(&self, e: &Extent) -> Extent {
        if self.is_identity() || e.is_empty() {
            return *e;
        }
        let mut out = Extent::EMPTY;
        for m in e.samples(16).filter_map(|c| self.point(c)) {
            out.expand(m);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// From longitude/latitude, a conversion gives what the target's transformer gives, and
    /// back what its inverse gives: Bern in LV95 (EPSG:2056), against PROJ 9.9.0, network off
    /// (`cs2cs -d 6 EPSG:4326 EPSG:2056 <<< "46.951082877 7.438632495"`: 2600000.006205,
    /// 1200000.012833).
    #[test]
    fn a_conversion_from_lonlat_is_the_targets_transformer() {
        let lv95 = Crs::Epsg(2056);
        let to = Crs::LonLat.to(lv95).unwrap();
        let t = lv95.transformer().unwrap();
        let bern = Coord::new(7.438_632_495, 46.951_082_877);
        let m = to.point(bern).unwrap();
        assert_eq!(Some(m), t.from_lonlat(bern));
        assert!((m.x - 2_600_000.006_205).abs() < 1e-3 && (m.y - 1_200_000.012_833).abs() < 1e-3, "{m:?}");
        let back = lv95.to(Crs::LonLat).unwrap().point(m).unwrap();
        assert_eq!(Some(back), t.to_lonlat(m));
        assert!((back.x - bern.x).abs() < 1e-7 && (back.y - bern.y).abs() < 1e-7, "{back:?}");
    }

    /// Every dataset is held in longitude/latitude, whatever system it was read in, so the map's
    /// conversion of its geometry is the identity and can't fail (the map draws none of a layer
    /// whose conversion fails: [`crate::Dataset::coord_crs`]).
    #[test]
    fn every_dataset_is_held_in_lonlat() {
        let mut ds = crate::Dataset::new("lv95", crate::GeometryType::Point, vec![]);
        ds.crs = Some(Crs::Epsg(2056));
        assert_eq!(ds.coord_crs(), Crs::LonLat);
        assert!(ds.coord_crs().to(Crs::LonLat).unwrap().is_identity());
    }

    /// Each system judges what it is given, as its transformer does: Web Mercator writes a latitude
    /// past where it ends, an infinite one too, at 85.05°; longitude/latitude refuses one that
    /// isn't finite.
    #[test]
    fn each_system_judges_its_input() {
        let merc = Crs::LonLat.to(Crs::WebMercator).unwrap();
        let t = Crs::WebMercator.transformer().unwrap();
        for c in [Coord::new(40.0, f64::INFINITY), Coord::new(10.0, 89.0), Coord::new(20.0, -90.0)] {
            assert_eq!(merc.point(c), t.from_lonlat(c), "{c:?}");
            assert!(merc.point(c).is_some(), "{c:?}");
        }
        let same = Crs::LonLat.to(Crs::LonLat).unwrap();
        assert_eq!(same.point(Coord::new(40.0, f64::INFINITY)), None);
    }

    /// Every conversion notes its systems for the missing-grid notice, as asking for a system's
    /// transformer does, the same conversion asked for again too (a second export).
    #[test]
    fn every_conversion_notes_its_systems() {
        let lv95 = Crs::Epsg(2056);
        let def = crate::raster::crs::epsg_proj4(2056).unwrap();
        for _ in 0..2 {
            let _ = crate::raster::crs::take_used();
            Crs::LonLat.to(lv95).unwrap();
            assert!(crate::raster::crs::take_used().iter().any(|(d, _)| d == def));
        }
    }

    /// Between two projected systems it goes through longitude/latitude, where PROJ goes
    /// directly: LV95 to UTM 32N at Bern, against PROJ 9.9.0, network off (`cs2cs -d 6 EPSG:2056
    /// EPSG:32632 <<< "2600000 1200000"`: 381188.815114, 5200911.299290).
    #[test]
    fn a_conversion_between_two_systems_goes_through_lonlat() {
        let (lv95, utm) = (Crs::Epsg(2056), Crs::Epsg(32632));
        let at = Coord::new(2_600_000.0, 1_200_000.0);
        let m = lv95.to(utm).unwrap().point(at).unwrap();
        assert!((m.x - 381_188.815_114).abs() < 1e-2 && (m.y - 5_200_911.299_290).abs() < 1e-2, "{m:?}");
    }

    /// The same system leaves geometry and boxes as they are; a place that can't be converted is
    /// NaN in a geometry and refuses the whole one.
    #[test]
    fn the_same_system_does_nothing_and_a_lost_place_is_nan() {
        let same = Crs::Epsg(2056).to(Crs::Epsg(2056)).unwrap();
        assert!(same.is_identity());
        let g = Geometry::Line(vec![vec![Coord::new(1.0, 2.0), Coord::new(3.0, 4.0)]]);
        assert_eq!(same.geometry(&g), g);
        let e = Extent::from_points(Coord::new(1.0, 2.0), Coord::new(3.0, 4.0));
        assert_eq!(same.extent(&e), e);
        assert_eq!(same.point(Coord::new(f64::NAN, 0.0)), None);
        // UTM 32N can't bring back a place on the equator 90° from its central meridian.
        let utm = Crs::LonLat.to(Crs::Epsg(32632)).unwrap();
        let far = Geometry::Line(vec![vec![Coord::new(9.0, 47.0), Coord::new(99.0, 0.0)]]);
        let out = utm.geometry(&far);
        let Geometry::Line(parts) = &out else { panic!("{out:?}") };
        assert!(parts[0][0].is_finite() && !parts[0][1].is_finite(), "{out:?}");
        assert_eq!(utm.whole_geometry(&far), None);
        assert!(utm.whole_geometry(&Geometry::Point(Coord::new(9.0, 47.0))).is_some());
    }

    /// A box follows its curved edges: in LV95 the middle of Switzerland's longitude/latitude
    /// box's south edge lies more than 500 m south of either southern corner, and is inside.
    #[test]
    fn a_box_holds_its_converted_edges() {
        let to = Crs::LonLat.to(Crs::Epsg(2056)).unwrap();
        let e = Extent::from_points(Coord::new(5.9, 45.8), Coord::new(10.5, 47.8));
        let out = to.extent(&e);
        let t = Crs::Epsg(2056).transformer().unwrap();
        for c in [Coord::new(5.9, 45.8), Coord::new(10.5, 47.8), Coord::new(8.2, 45.8)] {
            assert!(out.contains(t.from_lonlat(c).unwrap()), "{c:?} {out:?}");
        }
        // The curve itself: the south edge's middle lies well south of both southern corners, so
        // a box from the corners alone would miss it.
        let south = |lon: f64| t.from_lonlat(Coord::new(lon, 45.8)).unwrap().y;
        assert!(south(8.2) < south(5.9).min(south(10.5)) - 500.0, "{} {} {}", south(8.2), south(5.9), south(10.5));
        assert!(out.min_y <= south(8.2));
        assert!(Crs::LonLat.to(Crs::Epsg(2056)).unwrap().extent(&Extent::EMPTY).is_empty());
    }
}
