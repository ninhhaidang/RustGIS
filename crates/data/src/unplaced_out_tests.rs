//! What leaves the session from a layer with a place that can't be given in the system written
//! (#2032 C2f-4b): the feature is written without its geometry and said, rather than failing the
//! export; a layer whose places all convert writes as before and says nothing of it.

use crate::raster::Crs;
use crate::{Dataset, Field, FieldType, Geometry, Value};
use rustgis_geom::Coord;

fn utm31() -> Crs {
    Crs::from_epsg(32631).unwrap()
}

/// Lines held in UTM zone 31N: one at Paris and, with `far`, one reaching 1e9 m east, which has no
/// place in longitude/latitude.
fn lines(far: bool) -> Dataset {
    let mut ds = Dataset::new("lines", crate::GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
    let line = |a: (f64, f64), b: (f64, f64)| Some(Geometry::Line(vec![vec![Coord::new(a.0, a.1), Coord::new(b.0, b.1)]]));
    ds.push(line((451_000.0, 5_411_000.0), (452_000.0, 5_412_000.0)), vec![Value::Text("paris".into())]);
    if far {
        ds.push(line((451_500.0, 5_411_500.0), (1e9, 5_411_500.0)), vec![Value::Text("far east".into())]);
    }
    ds.held_in = utm31();
    ds.crs = Some(utm31());
    ds
}

fn dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("rustgis-unplaced-out-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_lonlat_copy_leaves_out_only_what_cant_be_placed() {
    let held = lines(true);
    let (copy, unplaced) = held.in_lonlat_leaving_out().unwrap();
    assert_eq!(unplaced, 1);
    assert_eq!(copy.coord_crs(), Crs::LonLat);
    assert_eq!(copy.features.len(), 2, "the feature keeps its attributes");
    assert!(copy.features[0].geometry.is_some());
    assert!(copy.features[1].geometry.is_none());
    // As in_lonlat gives it, for a layer whose places all convert.
    let clean = lines(false);
    let (copy, unplaced) = clean.in_lonlat_leaving_out().unwrap();
    assert_eq!(unplaced, 0);
    assert_eq!(*copy, *clean.in_lonlat().unwrap());
    assert!(lines(true).in_lonlat().is_err(), "the strict copy still refuses");
    assert_eq!(crate::unplaced_note("x", "lines", 0), None);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_export_writes_a_feature_it_cant_place_without_its_geometry_and_says_so() {
    let d = dir("files");
    let lonlat_note = "1 feature(s) of lines have a place that can't be given in WGS 84 (EPSG:4326), so their geometry is left out (and the feature, in a format that holds only features with geometry)";
    // GeoJSON (longitude/latitude): written, the feature without its geometry.
    let path = d.join("lines.geojson");
    let (left_out, _, notes) = crate::io::write_path_to(&lines(true), &path, Crs::LonLat, &Default::default()).unwrap();
    assert_eq!(left_out, 0);
    assert_eq!(notes, [format!("{}: {lonlat_note}", path.display())]);
    let (back, _) = crate::io::read_path(&path).unwrap();
    assert_eq!(back.features.len(), 2);
    assert!(back.features[1].geometry.is_none());
    // A FlatGeobuf holds only features with geometry: it leaves the feature out, as it says.
    let path = d.join("lines-lonlat.fgb");
    let (left_out, _, notes) = crate::io::write_path_to(&lines(true), &path, Crs::LonLat, &Default::default()).unwrap();
    assert_eq!((left_out, notes.len()), (1, 1), "{notes:?}");
    // In its own system nothing is converted: the feature is written whole, and nothing said.
    let path = d.join("lines-utm.fgb");
    let (left_out, _, notes) = crate::io::write_path_to(&lines(true), &path, utm31(), &Default::default()).unwrap();
    assert_eq!((left_out, notes), (0, Vec::<String>::new()));
    // A layer whose places all convert says nothing.
    let (_, _, notes) = crate::io::write_path_to(&lines(false), &d.join("clean.geojson"), Crs::LonLat, &Default::default()).unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    // The writer that gives no notes refuses it, naming the feature.
    let e = crate::io::write_path_in(&lines(true), &d.join("strict.geojson"), Crs::LonLat).unwrap_err().to_string();
    assert!(e.contains("feature 2"), "{e}");
    // CSV's own count reads such a layer too.
    let notes = crate::io::export_losses_notes(&lines(true), "csv", None, "lines.csv").unwrap();
    assert!(notes.iter().all(|n| !n.contains("can't be given")), "{notes:?}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_download_writes_a_feature_it_cant_place_without_its_geometry_and_says_so() {
    let (bytes, left_out, notes) = crate::io::write_bytes_noted(&lines(true), "geojson", Crs::LonLat, "lines.geojson").unwrap();
    assert_eq!(left_out, 0);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("lines.geojson: 1 feature(s) of lines have a place"), "{}", notes[0]);
    let (back, _) = crate::io::read_geojson(std::str::from_utf8(&bytes).unwrap(), "lines").unwrap();
    assert_eq!(back.features.len(), 2);
    assert!(back.features[1].geometry.is_none());
    let (_, _, notes) = crate::io::write_bytes_noted(&lines(false), "geojson", Crs::LonLat, "clean.geojson").unwrap();
    assert!(notes.is_empty());
    // The strict writer still refuses it, naming the feature.
    let e = crate::io::write_bytes_in(&lines(true), "geojson", Crs::LonLat).unwrap_err().to_string();
    assert!(e.contains("feature 2"), "{e}");
}

#[test]
fn a_missing_coordinate_stays_missing_and_isnt_counted() {
    // A vertex already missing (NaN, as GML keeps one) is no place that can't be given: the
    // export keeps the feature's geometry, the vertex missing as it was, and counts nothing.
    let mut ds = lines(false);
    ds.push(
        Some(Geometry::Line(vec![vec![
            Coord::new(451_500.0, 5_411_500.0),
            Coord::new(f64::NAN, f64::NAN),
            Coord::new(452_000.0, 5_411_000.0),
        ]])),
        vec![Value::Text("gap".into())],
    );
    let (out, unplaced) = crate::io::project_leaving_out(&ds, Crs::LonLat).unwrap();
    assert_eq!(unplaced, 0);
    let Some(Geometry::Line(parts)) = &out.features[1].geometry else { panic!("kept: {:?}", out.features[1].geometry) };
    let finite: Vec<bool> = parts[0].iter().map(|c| c.is_finite()).collect();
    assert_eq!(finite, [true, false, true]);
    // The far line beside it is still counted.
    ds.push(
        Some(Geometry::Line(vec![vec![Coord::new(451_500.0, 5_411_500.0), Coord::new(1e9, 5_411_500.0)]])),
        vec![Value::Text("far east".into())],
    );
    let (out, unplaced) = crate::io::project_leaving_out(&ds, Crs::LonLat).unwrap();
    assert_eq!(unplaced, 1);
    assert!(out.features[1].geometry.is_some() && out.features[2].geometry.is_none());
}

#[test]
fn an_infinite_coordinate_stays_as_it_is_and_isnt_placed() {
    // A latitude of +infinity has no place: Web Mercator would take it to 85.05°, so the export
    // keeps it as it is, beside a NaN, and counts nothing.
    let mut ds = Dataset::new("lines", crate::GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
    ds.push(
        Some(Geometry::Line(vec![vec![
            Coord::new(2.0, 48.0),
            Coord::new(f64::NAN, f64::NAN),
            Coord::new(40.0, f64::INFINITY),
            Coord::new(3.0, 49.0),
        ]])),
        vec![Value::Text("gaps".into())],
    );
    let (out, unplaced) = crate::io::project_leaving_out(&ds, Crs::WebMercator).unwrap();
    assert_eq!(unplaced, 0);
    let Some(Geometry::Line(parts)) = &out.features[0].geometry else { panic!("kept") };
    assert!(parts[0][1].x.is_nan());
    assert_eq!(parts[0][2].y, f64::INFINITY);
    assert!(parts[0][0].is_finite() && parts[0][3].is_finite());
}
