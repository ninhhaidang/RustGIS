//! Coordinates from one coordinate system to another ([`Crs::to`]).
//!
//! From or to WGS 84 longitude/latitude, a conversion is the other system's transformer alone
//! ([`Transformer::to_lonlat`] or [`Transformer::from_lonlat`]), so a conversion of
//! longitude/latitude geometry gives exactly what that transformer gives. Between two other
//! systems it is the direct operation between them, as PROJ finds it (`cs2cs A B`) but never a
//! ballpark: no datum shift between two systems on the same datum, and the registry's
//! transformation between two datums (DHDN to ETRS89), where going through WGS 84 would take
//! each datum's own shift there and back and may be metres off. Whether a place is one is still
//! judged through WGS 84, with the checks each transformer makes (a latitude past a pole, a place
//! a projection folds or can't bring back is refused), and so is the edge or side of the
//! antimeridian it lies on. Where the direct operation can't convert a place, or the datums have
//! no transformation between them, the place through WGS 84 is taken: one geometry's vertices may
//! then be converted by two transformations, as PROJ's choice by place does near an area's edge.
//! The direct operation's own grids are noted and downloaded as a system's are
//! ([`super::crs::missing_grids_between`]).

use super::crs::{Crs, DirectPair, Transformer};
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
    /// The direct operation between the two, when neither is longitude/latitude or Web Mercator
    /// and terrio-crs has one.
    direct: Option<std::sync::Arc<DirectPair>>,
    /// Two conversions one after the other, through a system between ([`Crs::to_via`]); the
    /// fields above are then unused.
    through: Option<std::sync::Arc<(Reprojection, Reprojection)>>,
}

impl Crs {
    /// The conversion of coordinates in this system to `other`'s. Between the same system it does
    /// nothing ([`Reprojection::is_identity`]). Fails as either system's [`Crs::transformer`]
    /// does. Each call asks both systems for their transformers again ([`Crs::transformer`], which
    /// keeps them for the run), so every conversion notes its systems for the missing-grid notice
    /// and waits for their grids, as asking for a transformer itself does; and the direct
    /// operation between them, kept for the run too, which notes the pair and waits for its grids
    /// as a system's are.
    pub fn to(self, other: Crs) -> Result<Reprojection, String> {
        let same = self == other;
        let of = |crs: Crs| -> Result<Option<Transformer>, String> {
            if same || crs == Crs::LonLat { Ok(None) } else { crs.transformer().map(Some) }
        };
        let (unproject, project) = (of(self)?, of(other)?);
        let direct = match (&unproject, &project) {
            (Some(a), Some(b)) => DirectPair::new(self, other, a, b).map(std::sync::Arc::new),
            _ => None,
        };
        Ok(Reprojection { source: self, target: other, unproject, project, direct, through: None })
    }

    /// The conversion of coordinates in this system to `other`'s through `via`: this system's to
    /// `via` ([`Crs::to`], the direct operation where there is one), then `via`'s to `other`. A
    /// map in `via` shows a layer in this system by the first, and gives its places in
    /// longitude/latitude by the second, so a place converted this way and drawn on that map
    /// lands where the layer's own place is drawn. Where `via` is either end, or the two ends are
    /// the same system, it is [`Crs::to`].
    pub fn to_via(self, via: Crs, other: Crs) -> Result<Reprojection, String> {
        if via == self || via == other || self == other {
            return self.to(other);
        }
        let (first, second) = (self.to(via)?, via.to(other)?);
        Ok(Reprojection {
            source: self,
            target: other,
            unproject: None,
            project: None,
            direct: None,
            through: Some(std::sync::Arc::new((first, second))),
        })
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

    /// Whether it takes the direct operation between its two systems where it can (see the
    /// module's notes), not only the way through WGS 84.
    pub fn is_direct(&self) -> bool {
        self.direct.is_some()
    }

    /// A place in the target system, or `None` where the source system has no such place or the
    /// target can't hold it. Each system's transformer judges what it is given, as a call of its
    /// own did: Web Mercator takes a latitude past 85.05° (an infinite one too) as 85.05°, where
    /// it ends. Between the same system, or between longitude/latitude, the place itself when it
    /// is finite. Between two other systems, the direct operation's coordinates of a place the way
    /// through WGS 84 converts, else those through WGS 84 (see the module's notes).
    pub fn point(&self, c: Coord) -> Option<Coord> {
        if let Some(through) = &self.through {
            return through.1.point(through.0.point(c)?);
        }
        let lonlat = match &self.unproject {
            Some(t) => t.to_lonlat(c)?,
            None => c,
        };
        let out = match &self.project {
            Some(t) => t.from_lonlat(lonlat)?,
            None => lonlat,
        };
        if !out.is_finite() {
            return None;
        }
        Some(self.direct.as_ref().and_then(|d| d.point(c, out)).unwrap_or(out))
    }

    /// The place `c` in the target system when `via` is where the way through WGS 84 puts it in
    /// the target, as a caller with its own checks there found it (the map's projection): the
    /// direct operation's coordinates where it has one that lands near `via`, else `via`.
    pub fn point_near(&self, c: Coord, via: Coord) -> Coord {
        self.direct.as_ref().and_then(|d| d.point(c, via)).unwrap_or(via)
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

    /// [`Reprojection::extent`] for a feature's box, as a frame tests each feature's against the
    /// view distance: a point's box by its one place, a small box by its corners and edges'
    /// middles (3 by 3 places, not 17 by 17). A box whose converted size is more than
    /// [`FEATURE_EXTENT_FINE`] in either axis (about 10 km in degrees), where edges bend enough to
    /// matter, is converted as [`Reprojection::extent`] converts it. Empty where none of the
    /// places converts.
    pub fn feature_extent(&self, e: &Extent) -> Extent {
        if self.is_identity() || e.is_empty() {
            return *e;
        }
        let corner = Coord::new(e.min_x, e.min_y);
        if e.max_x == e.min_x && e.max_y == e.min_y {
            return self.point(corner).map_or(Extent::EMPTY, |m| Extent::from_points(m, m));
        }
        let mut out = Extent::EMPTY;
        for m in e.samples(2).filter_map(|c| self.point(c)) {
            out.expand(m);
        }
        if out.width() > FEATURE_EXTENT_FINE || out.height() > FEATURE_EXTENT_FINE { self.extent(e) } else { out }
    }
}

/// The converted size above which [`Reprojection::feature_extent`] samples a box as finely as
/// [`Reprojection::extent`]: 0.1, about 10 km in degrees. Below it, a box's edges bend by well
/// under a millimetre between 3 places a side (a 10 km box in UTM: within 1e-8°).
pub const FEATURE_EXTENT_FINE: f64 = 0.1;

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

    /// A feature's box converted from 3 by 3 places is the 17 by 17 one's within a millimetre for
    /// a 10 km box in UTM 32N (in degrees, about 1e-8); a point's is its one place.
    #[test]
    fn a_features_box_converts_from_few_places() {
        let to = Crs::Epsg(32632).to(Crs::LonLat).unwrap();
        let e = Extent::new(690_000.0, 5_390_000.0, 700_000.0, 5_400_000.0);
        let (fine, rough) = (to.extent(&e), to.feature_extent(&e));
        for (a, b) in [(fine.min_x, rough.min_x), (fine.min_y, rough.min_y), (fine.max_x, rough.max_x), (fine.max_y, rough.max_y)] {
            assert!((a - b).abs() < 1e-8, "{fine:?} vs {rough:?}");
        }
        // A 500 km box is sampled finely.
        let large = Extent::new(300_000.0, 5_000_000.0, 800_000.0, 5_500_000.0);
        assert_eq!(to.feature_extent(&large), to.extent(&large));
        let p = Coord::new(695_000.0, 5_395_000.0);
        let at = to.point(p).unwrap();
        assert_eq!(to.feature_extent(&Extent::from_points(p, p)), Extent::from_points(at, at));
        assert!(to.feature_extent(&Extent::EMPTY).is_empty());
    }

    /// A dataset made in code is held in longitude/latitude, whatever system it says it was read
    /// in, until held in another ([`crate::Dataset::hold_in`]), so the map's conversion of its
    /// geometry is the identity and can't fail.
    #[test]
    fn a_dataset_made_in_code_is_held_in_lonlat() {
        let mut ds = crate::Dataset::new("lv95", crate::GeometryType::Point, vec![]);
        ds.crs = Some(Crs::Epsg(2056));
        assert_eq!(ds.coord_crs(), Crs::LonLat);
        assert!(ds.coord_crs().to(Crs::LonLat).unwrap().is_identity());
        // An edit reads and writes its geometry as it is: borrowed, not copied, and a place that
        // isn't one (NaN) kept, as the edit checks it.
        let g = Geometry::Line(vec![vec![Coord::new(7.0, 46.0), Coord::new(f64::NAN, 47.0)]]);
        let read = ds.geometry_to_lonlat(&g).unwrap();
        assert!(matches!(read, std::borrow::Cow::Borrowed(_)));
        let held = crate::HeldPlaces::default();
        assert_eq!(format!("{:?}", ds.edit_from_lonlat(read.into_owned(), &held).unwrap()), format!("{g:?}"));
        // So its longitude/latitude box is its geometry's.
        ds.push(Some(Geometry::Point(Coord::new(2_600_000.0, 1_200_000.0))), vec![]);
        ds.push(Some(Geometry::Point(Coord::new(7.0, 46.0))), vec![]);
        assert_eq!(ds.lonlat_extent(), ds.extent());
        assert_eq!(ds.extent_to_lonlat(&Extent::EMPTY), Extent::EMPTY);
        // And a longitude/latitude box asked of it (a selection, a view) is the same box.
        let asked = Extent::from_points(Coord::new(5.9, 45.8), Coord::new(10.5, 47.8));
        assert_eq!(ds.extent_from_lonlat(&asked), asked);
        assert_eq!(ds.extent_from_lonlat(&Extent::EMPTY), Extent::EMPTY);
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

    /// Between two projected systems on WGS 84's ensemble it gives what PROJ gives: LV95 to UTM 32N
    /// at Bern, against PROJ 9.9.0, network off (`cs2cs -d 6 EPSG:2056 EPSG:32632 <<< "2600000
    /// 1200000"`: 381188.815114, 5200911.299290).
    #[test]
    fn a_conversion_between_two_systems_is_projs() {
        let (lv95, utm) = (Crs::Epsg(2056), Crs::Epsg(32632));
        let at = Coord::new(2_600_000.0, 1_200_000.0);
        let m = lv95.to(utm).unwrap().point(at).unwrap();
        assert!((m.x - 381_188.815_114).abs() < 1e-3 && (m.y - 5_200_911.299_290).abs() < 1e-3, "{m:?}");
    }

    /// The place through WGS 84, which the direct operation's coordinates replace.
    fn through_wgs84(from: Crs, to: Crs, c: Coord) -> Coord {
        to.transformer().unwrap().from_lonlat(from.transformer().unwrap().to_lonlat(c).unwrap()).unwrap()
    }

    /// Between two datums the registry's transformation is taken, as PROJ takes it, not each
    /// datum's own to WGS 84 there and back: Gauss-Krüger zone 3 on DHDN to UTM 32N on ETRS89 by
    /// DHDN to ETRS89's Helmert (its best, by grid, missing), 0.44 m from the way through WGS 84
    /// (PROJ 9.9.0, network off, `cs2cs -d 6 EPSG:31467 EPSG:25832 <<< "5400000 3500000"`,
    /// northing first: 499924.543645, 5398279.544875); and LV03 to LV95 without its grid by
    /// CH1903's and CH1903+'s Helmerts to CHTRS95, which cancel, so exactly the 2,000,000 and
    /// 1,000,000 m apart the two are defined to be at Bern (`cs2cs -d 6 EPSG:21781 EPSG:2056 <<<
    /// "600000 200000"`: 2600000.000000, 1200000.000000), where through WGS 84 is a millimetre
    /// off.
    #[test]
    fn a_conversion_between_two_datums_takes_their_transformation() {
        let (gk3, utm) = (Crs::Epsg(31467), Crs::Epsg(25832));
        let at = Coord::new(3_500_000.0, 5_400_000.0);
        let m = gk3.to(utm).unwrap().point(at).unwrap();
        assert!((m.x - 499_924.543_645).abs() < 1e-3 && (m.y - 5_398_279.544_875).abs() < 1e-3, "{m:?}");
        let via = through_wgs84(gk3, utm, at);
        assert!((m.x - via.x).hypot(m.y - via.y) > 0.4, "{m:?} {via:?}");

        let (lv03, lv95) = (Crs::Epsg(21781), Crs::Epsg(2056));
        let at = Coord::new(600_000.0, 200_000.0);
        let m = lv03.to(lv95).unwrap().point(at).unwrap();
        assert!((m.x - 2_600_000.0).abs() < 1e-4 && (m.y - 1_200_000.0).abs() < 1e-4, "{m:?}");
        let via = through_wgs84(lv03, lv95, at);
        assert!((m.x - via.x).hypot(m.y - via.y) > 5e-4, "{m:?} {via:?}");
    }

    /// Between two systems on the same datum no shift is applied: UTM 32N to LAEA Europe, both
    /// on ETRS89 (PROJ 9.9.0, network off, `cs2cs -d 6 EPSG:25832 EPSG:3035 <<< "500000
    /// 5400000"`, northing first: 2849353.439746, 4247444.683891). The conversion back lands where
    /// it started; a place the way through WGS 84 refuses stays refused.
    #[test]
    fn a_conversion_on_one_datum_shifts_nothing() {
        let (utm, laea) = (Crs::Epsg(25832), Crs::Epsg(3035));
        let at = Coord::new(500_000.0, 5_400_000.0);
        let m = utm.to(laea).unwrap().point(at).unwrap();
        assert!((m.x - 4_247_444.683_891).abs() < 1e-3 && (m.y - 2_849_353.439_746).abs() < 1e-3, "{m:?}");
        let back = laea.to(utm).unwrap().point(m).unwrap();
        assert!((back.x - at.x).abs() < 1e-6 && (back.y - at.y).abs() < 1e-6, "{back:?}");
        // UTM 32N can't bring back a place on the equator 90° from its central meridian, nor
        // take one there: LAEA's coordinates of 99° E on the equator.
        let far = Crs::LonLat.to(laea).unwrap().point(Coord::new(99.0, 0.0)).unwrap();
        assert_eq!(laea.to(utm).unwrap().point(far), None);
    }

    /// To a geographic system, a longitude on the antimeridian is put on the side the way through
    /// WGS 84 puts it, and the place is where it was: NAD83 (EPSG:4269) to Alaska's Albers
    /// (EPSG:3338, on NAD83) and back, with no shift, where the way through WGS 84 is nearly a
    /// metre off.
    #[test]
    fn a_geographic_target_keeps_its_longitudes_turn() {
        let (albers, nad83) = (Crs::Epsg(3338), Crs::Epsg(4269));
        for lon in [179.9, -179.9, 180.0, -180.0] {
            let at = nad83.to(albers).unwrap().point(Coord::new(lon, 52.0)).unwrap();
            let m = albers.to(nad83).unwrap().point(at).unwrap();
            let via = through_wgs84(albers, nad83, at);
            assert!((m.x - via.x).abs() < 1e-4 && (m.y - via.y).abs() < 1e-4, "{lon}: {m:?} {via:?}");
            if lon.abs() < 180.0 {
                assert!((m.x - lon).abs() < 1e-9 && (m.y - 52.0).abs() < 1e-9, "{lon}: {m:?}");
            }
        }
    }

    /// A place on a world projection's edge stays on that edge: Natural Earth's coordinates of
    /// 180° E a hair past its edge (terrio-crs's inverse gives 180° W, which the way through WGS 84
    /// refuses and takes back onto the edge, #1925), in Robinson, are on Robinson's east edge,
    /// not its west one.
    #[test]
    fn a_world_projections_edge_stays_its_side() {
        let (natearth, robinson) = (Crs::parse("ESRI:54077").unwrap(), Crs::parse("ESRI:54030").unwrap());
        let east = natearth.transformer().unwrap().from_lonlat(Coord::new(180.0, 30.0)).unwrap();
        let at = Coord::new(east.x * (1.0 + 1e-9), east.y);
        let m = natearth.to(robinson).unwrap().point(at).unwrap();
        let edge = robinson.transformer().unwrap().from_lonlat(Coord::new(180.0, 30.0)).unwrap();
        assert!((m.x - edge.x).abs() < 1.0 && (m.y - edge.y).abs() < 1.0, "{m:?} {edge:?}");
    }

    /// A conversion between two systems whose direct operation's best transformation needs a grid
    /// RustGIS doesn't have notes the pair, by both names, for the missing-grid notice, and lists
    /// the grid for download: LV03 to LV95 by swisstopo's CHENyx06, which neither system's own
    /// conversion to WGS 84 needs.
    #[test]
    fn a_pairs_missing_grid_is_noted_and_listed() {
        let (lv03, lv95) = (Crs::Epsg(21781), Crs::Epsg(2056));
        assert!(crate::raster::crs::missing_grids(lv03).is_empty() && crate::raster::crs::missing_grids(lv95).is_empty());
        let grids = crate::raster::crs::missing_grids_between(lv03, lv95);
        assert!(grids.iter().any(|g| g.name.contains("CHENyx06") && g.url.starts_with("https://cdn.proj.org/")), "{grids:?}");
        let _ = crate::raster::crs::take_used();
        lv03.to(lv95).unwrap();
        let used = crate::raster::crs::take_used();
        let (key, label) = used.iter().find(|(_, l)| l.contains(" to ")).expect("the pair noted");
        assert_eq!(label, "EPSG:21781 to EPSG:2056");
        assert!(crate::raster::crs::needs_a_missing_grid(key));
        assert_eq!(crate::raster::crs::pair_systems(key), Some((lv03, lv95)));
        // On one datum, nothing to download.
        assert!(crate::raster::crs::missing_grids_between(Crs::Epsg(25832), Crs::Epsg(3035)).is_empty());
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

    /// A box partly outside what the target holds gives the box of the part that converts, finite;
    /// one wholly outside gives an empty box (what `Dataset::extent_from_lonlat` asks of a layer
    /// in its own system): UTM 32N can't bring back a place on the equator 90° from its central
    /// meridian, a corner of the first box and the whole of the second.
    #[test]
    fn a_box_partly_outside_its_target_keeps_the_part_that_converts() {
        let utm = Crs::LonLat.to(Crs::Epsg(32632)).unwrap();
        let t = Crs::Epsg(32632).transformer().unwrap();
        let lost = Coord::new(99.0, 0.0);
        assert!(t.from_lonlat(lost).is_none());
        let partly = utm.extent(&Extent::from_points(Coord::new(3.0, 0.0), lost));
        assert!(!partly.is_empty() && partly.min_x.is_finite() && partly.max_x.is_finite(), "{partly:?}");
        // The meridian at 9° E, well inside the zone, is held; the box's own corners are too.
        assert!(partly.contains(t.from_lonlat(Coord::new(9.0, 0.0)).unwrap()), "{partly:?}");
        assert!(partly.contains(t.from_lonlat(Coord::new(3.0, 0.0)).unwrap()), "{partly:?}");
        assert!(utm.extent(&Extent::from_points(lost, lost)).is_empty());
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
