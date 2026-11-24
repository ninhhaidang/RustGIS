//! A read given its box in the source's own system ([`Request::own`]) tests each feature's own box
//! against it, converting nothing; without one, or with one in another system, it reads as before.

use super::*;
use crate::raster::Crs;
use crate::{Dataset, Field, FieldType, Geometry, Value};
use rustgis_geom::Coord;
use std::path::PathBuf;

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("rustgis-own-box-tests-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn utm31() -> Crs {
    Crs::from_epsg(32631).unwrap()
}

/// Lines in UTM zone 31N: one at Paris, one from Paris to 1e9 m east (which has no place in
/// longitude/latitude), and one 400 km south.
fn lines() -> Dataset {
    let mut ds = Dataset::new("lines", GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
    let line = |a: (f64, f64), b: (f64, f64)| Some(Geometry::Line(vec![vec![Coord::new(a.0, a.1), Coord::new(b.0, b.1)]]));
    ds.push(line((451_000.0, 5_411_000.0), (452_000.0, 5_412_000.0)), vec![Value::Text("paris".into())]);
    ds.push(line((451_500.0, 5_411_500.0), (1e9, 5_411_500.0)), vec![Value::Text("far east".into())]);
    ds.push(line((451_000.0, 5_011_000.0), (452_000.0, 5_012_000.0)), vec![Value::Text("south".into())]);
    ds.held_in = utm31();
    ds
}

fn names(b: &Batch) -> Vec<String> {
    let mut out: Vec<String> = b
        .features
        .iter()
        .filter_map(|f| match f.attrs.first() {
            Some(Value::Text(t)) => Some(t.clone()),
            _ => None,
        })
        .collect();
    out.sort();
    out
}

/// Around Paris: in longitude/latitude, and the same place in UTM 31N.
fn around_paris() -> (Extent, Extent) {
    (Extent::new(2.2, 48.75, 2.5, 48.95), Extent::new(440_000.0, 5_400_000.0, 470_000.0, 5_420_000.0))
}

fn read_three_ways(src: &dyn FeatureSource, what: &str) {
    let (lonlat, utm) = around_paris();
    assert_eq!(src.coord_crs(), utm31(), "{what}");
    // In longitude/latitude, as before: the line with no place there is left out.
    let plain = src.read(&Request { extent: Some(lonlat), limit: 100, own: None }).unwrap();
    assert_eq!(names(&plain), ["paris"], "{what}");
    // In its own system: the line reaching past where longitude/latitude goes is kept, as its box
    // meets the view's; the one to the south still isn't.
    let own = src.read(&Request { extent: Some(lonlat), limit: 100, own: Some((utm31(), utm)) }).unwrap();
    assert_eq!(names(&own), ["far east", "paris"], "{what}");
    assert!(own.complete, "{what}");
    let line = own.features.iter().find(|f| matches!(f.attrs.first(), Some(Value::Text(t)) if t == "paris")).unwrap();
    assert_eq!(line.geometry.as_ref().map(Geometry::extent), Some(Extent::new(451_000.0, 5_411_000.0, 452_000.0, 5_412_000.0)), "{what}");
    // A box in another system is one this source can't take: read as without it.
    let swiss = Extent::new(2_600_000.0, 1_200_000.0, 2_700_000.0, 1_300_000.0);
    let other = src.read(&Request { extent: Some(lonlat), limit: 100, own: Some((Crs::from_epsg(2056).unwrap(), swiss)) }).unwrap();
    assert_eq!(names(&other), ["paris"], "{what}");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_read_given_its_box_in_the_files_own_system_tests_features_there() {
    let d = dir();
    for ext in ["shp", "fgb", "gpkg", "parquet"] {
        let p = d.join(format!("lines.{ext}"));
        crate::io::write_path_in(&lines(), &p, utm31()).unwrap();
        let read = || {
            let src = open_path(&p, None).unwrap();
            read_three_ways(src.as_ref(), ext);
        };
        if ext == "shp" {
            shp::with_test_cache(d.join("indexes"), read);
        } else {
            read();
        }
        // The browser's GeoPackage reader too.
        if ext == "gpkg" {
            let bytes = Shared(std::sync::Arc::new(std::fs::read(&p).unwrap()));
            let src = gpkg_mem::GpkgMemSource::open_shared(bytes, None).unwrap();
            read_three_ways(&src, "gpkg in memory");
        }
    }
}

#[test]
fn a_geojson_read_in_place_given_its_box_in_its_own_system_tests_features_there() {
    let p = dir().join("lines.geojson");
    let feature = |name: &str, a: (f64, f64), b: (f64, f64)| {
        format!(
            r#"{{"type":"Feature","properties":{{"name":"{name}"}},"geometry":{{"type":"LineString","coordinates":[[{},{}],[{},{}]]}}}}"#,
            a.0, a.1, b.0, b.1
        )
    };
    let text = format!(
        r#"{{"type":"FeatureCollection","crs":{{"type":"name","properties":{{"name":"urn:ogc:def:crs:EPSG::32631"}}}},"features":[{},{}]}}"#,
        feature("paris", (451_000.0, 5_411_000.0), (452_000.0, 5_412_000.0)),
        feature("south", (451_000.0, 5_011_000.0), (452_000.0, 5_012_000.0)),
    );
    std::fs::write(&p, text).unwrap();
    let src = geojson::GeoJsonSource::open_path(&p, "lines").unwrap();
    assert_eq!(src.coord_crs(), utm31());
    let (lonlat, utm) = around_paris();
    for own in [None, Some((utm31(), utm)), Some((Crs::from_epsg(2056).unwrap(), utm))] {
        let b = src.read(&Request { extent: Some(lonlat), limit: 100, own }).unwrap();
        assert_eq!(names(&b), ["paris"], "{own:?}");
    }
    // A box in its own system that misses Paris, though the longitude/latitude extent holds it:
    // the box decides.
    let elsewhere = Extent::new(600_000.0, 5_400_000.0, 620_000.0, 5_420_000.0);
    assert!(src.read(&Request { extent: Some(lonlat), limit: 100, own: Some((utm31(), elsewhere)) }).unwrap().features.is_empty());
}

#[test]
fn a_box_in_its_own_system_is_tested_without_converting() {
    let mut notes = Vec::new();
    let r = Reproject::from_crs(utm31(), &mut notes);
    let (lonlat, utm) = around_paris();
    let far = Geometry::Line(vec![vec![Coord::new(451_500.0, 5_411_500.0), Coord::new(1e9, 5_411_500.0)]]);
    // No place in longitude/latitude: kept as the file holds it (#2032 C2f-4b), outside a box
    // there, within a whole read, and tested by its own box with one in its own system.
    let kept = r.place(far.clone(), &Request { extent: Some(lonlat), limit: 1, own: None }).unwrap();
    assert!(!kept.inside && kept.geometry == far);
    assert!(r.place(far.clone(), &Request { extent: None, limit: 1, own: None }).unwrap().inside);
    let fitted = r.place(far.clone(), &Request { extent: Some(lonlat), limit: 1, own: Some((utm31(), utm)) }).unwrap();
    assert!(fitted.inside);
    assert_eq!(fitted.geometry, far);
    assert_eq!(r.request_box(&Request { extent: Some(lonlat), limit: 1, own: Some((utm31(), utm)) }), Some(utm));
    // A source in longitude/latitude reads by the extent, whatever box comes with it.
    let lonlat_source = Reproject::from_crs(Crs::LonLat, &mut notes);
    let req = Request { extent: Some(lonlat), limit: 1, own: Some((Crs::LonLat, Extent::new(0.0, 0.0, 1.0, 1.0))) };
    assert_eq!(lonlat_source.request_box(&req), Some(lonlat));
    assert!(lonlat_source.place(Geometry::Point(Coord::new(2.3, 48.85)), &req).unwrap().inside);
    assert!(notes.is_empty(), "{notes:?}");
}

#[test]
fn a_coordinate_that_isnt_a_number_is_left_out_in_its_own_system_too() {
    let mut notes = Vec::new();
    let r = Reproject::from_crs(utm31(), &mut notes);
    let (lonlat, utm) = around_paris();
    let req = Request { extent: Some(lonlat), limit: 1, own: Some((utm31(), utm)) };
    let missing = Geometry::Line(vec![vec![Coord::new(451_500.0, 5_411_500.0), Coord::new(f64::NAN, 5_411_500.0)]]);
    // Left out for the reader to count, as without the box.
    assert!(r.place(missing.clone(), &req).is_none());
    assert!(r.place(missing, &Request { own: None, ..req }).is_none());
    let clean = Geometry::Line(vec![vec![Coord::new(451_500.0, 5_411_500.0), Coord::new(452_000.0, 5_411_500.0)]]);
    assert!(r.place(clean, &req).unwrap().inside);
}

/// A box over the whole plane holds every shape, one with no place (a polygon of no parts)
/// included, as a read without a box keeps it (#2788).
#[test]
fn the_whole_plane_holds_a_shape_with_no_place() {
    let utm = Crs::from_epsg(32631).unwrap();
    let reproject = Reproject::known(Some((utm, utm.transformer().unwrap())));
    let empty = Geometry::Polygon(Vec::new());
    let whole = Request { extent: None, limit: usize::MAX, own: Some((utm, whole_plane())) };
    let none = Request { extent: None, limit: usize::MAX, own: None };
    let small = Request { extent: None, limit: usize::MAX, own: Some((utm, Extent::new(0.0, 0.0, 1.0, 1.0))) };
    assert!(reproject.place(empty.clone(), &whole).is_some_and(|f| f.inside), "the whole plane");
    assert!(reproject.place(empty.clone(), &none).is_some_and(|f| f.inside), "no box");
    assert!(reproject.place(empty, &small).is_some_and(|f| !f.inside), "a box of somewhere");
}

/// A source read by its rows (MySQL, SQL Server) keeps a geometry with no place in
/// longitude/latitude as the table holds it (#2032 C2f-4b): within a whole read, outside any
/// extent. One with a coordinate that isn't a number is left out for the reader to count.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_row_with_no_place_in_longitude_latitude_is_kept_outside_any_extent() {
    let mut notes = Vec::new();
    let r = Reproject::from_crs(utm31(), &mut notes);
    let (lonlat, _) = around_paris();
    let far = Geometry::Line(vec![vec![Coord::new(451_500.0, 5_411_500.0), Coord::new(1e9, 5_411_500.0)]]);
    let kept = r.read(far.clone()).unwrap();
    assert_eq!(kept.geometry, far);
    assert_eq!(kept.lonlat, None);
    assert!(Reproject::within(kept.lonlat.as_ref(), None), "a whole read");
    assert!(!Reproject::within(kept.lonlat.as_ref(), Some(&lonlat)), "a read of a box");
    let paris = r.read(Geometry::Point(Coord::new(451_500.0, 5_411_500.0))).unwrap();
    assert!(Reproject::within(paris.lonlat.as_ref(), Some(&lonlat)));
    assert!(r.read(Geometry::Point(Coord::new(f64::NAN, 5_411_500.0))).is_none());
    assert!(notes.is_empty(), "{notes:?}");
}
