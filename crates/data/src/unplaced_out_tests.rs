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
    let (bytes, left_out, notes, _) = crate::io::write_bytes_noted(&lines(true), "geojson", Crs::LonLat, "lines.geojson").unwrap();
    assert_eq!(left_out, 0);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("lines.geojson: 1 feature(s) of lines have a place"), "{}", notes[0]);
    let (back, _) = crate::io::read_geojson(std::str::from_utf8(&bytes).unwrap(), "lines").unwrap();
    assert_eq!(back.features.len(), 2);
    assert!(back.features[1].geometry.is_none());
    let (_, _, notes, _) = crate::io::write_bytes_noted(&lines(false), "geojson", Crs::LonLat, "clean.geojson").unwrap();
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

/// A Shapefile whose `.prj` gives a shape no place in longitude/latitude keeps it as the file holds
/// it, says so, and leaves the layer whole (#2032 C2f-4b): nothing is lost, so Define Projection
/// stays open. When no shape has one, its system is said not to fit.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_shapefile_keeps_a_shape_with_no_longitude_latitude_place() {
    let d = dir("kept-shp");
    for (far_only, file) in [(false, "some.shp"), (true, "all.shp")] {
        let mut raw = Dataset::new("points", crate::GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        if !far_only {
            raw.push(Some(Geometry::Point(Coord::new(451_000.0, 5_411_000.0))), vec![Value::Text("paris".into())]);
        }
        raw.push(Some(Geometry::Point(Coord::new(1e12, 1e12))), vec![Value::Text("far".into())]);
        let path = d.join(file);
        crate::io::write_path(&raw, &path).unwrap();
        std::fs::write(path.with_extension("prj"), crate::raster::wkt::to_wkt(utm31()).unwrap()).unwrap();
        let (ds, report) = crate::io::read_path(&path).unwrap();
        assert_eq!(ds.coord_crs(), utm31());
        assert!(!ds.crs_incomplete);
        let far = ds.features.rows().last().and_then(|r| r.geometry().cloned());
        assert_eq!(far, Some(Geometry::Point(Coord::new(1e12, 1e12))), "kept as the file holds it");
        let said: Vec<&str> = report.messages.texts().collect();
        let expected = if far_only {
            "none of the shapes has a place in longitude/latitude from EPSG:32631"
        } else {
            "one of the shapes has a place in EPSG:32631"
        };
        assert!(said.iter().any(|m| m.contains(expected) && m.contains("kept as the file holds")), "{said:?}");
        assert_eq!(ds.in_lonlat_leaving_out().unwrap().1, 1);
    }
}

/// A delimited text file's rows with no place in longitude/latitude from the system given are
/// kept as the file holds them, and said (#2032 C2f-4b).
#[test]
fn a_csv_keeps_a_row_with_no_longitude_latitude_place() {
    let text = "name,x,y\nparis,451000,5411000\nfar,1e12,1e12\n";
    let opts = crate::io::CsvOptions { x: Some("x".into()), y: Some("y".into()), crs: Some(utm31()), ..Default::default() };
    let (ds, report) = crate::io::read_csv_with(text, "pts", &opts).unwrap();
    assert_eq!(ds.features.len(), 2);
    assert_eq!(ds.features.rows().last().and_then(|r| r.geometry().cloned()), Some(Geometry::Point(Coord::new(1e12, 1e12))));
    let said: Vec<&str> = report.messages.texts().collect();
    assert!(
        said.iter().any(|m| m.contains("one of the rows has a place in EPSG:32631") && m.contains("kept as the file holds it")),
        "{said:?}"
    );
    assert_eq!(report.skipped, 0);
}

/// A Shapefile read in place keeps a shape with no place in longitude/latitude as the file holds
/// it, as one read whole does, and says so once (#2032 C2f-4b).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_streamed_shapefile_keeps_what_a_whole_read_keeps() {
    crate::source::shp::use_temporary_index_dir();
    let d = dir("kept-streamed-shp");
    let mut raw = Dataset::new("points", crate::GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    raw.push(Some(Geometry::Point(Coord::new(451_000.0, 5_411_000.0))), vec![Value::Text("paris".into())]);
    raw.push(Some(Geometry::Point(Coord::new(1e12, 1e12))), vec![Value::Text("far".into())]);
    let path = d.join("points.shp");
    crate::io::write_path(&raw, &path).unwrap();
    std::fs::write(path.with_extension("prj"), crate::raster::wkt::to_wkt(utm31()).unwrap()).unwrap();
    let (whole, _) = crate::io::read_path(&path).unwrap();
    let src = crate::source::shp::ShpSource::open_path(&path, "points").unwrap();
    let (streamed, report) = crate::source::materialize(&src).unwrap();
    assert_eq!(streamed.features, whole.features);
    assert_eq!(streamed.coord_crs(), utm31());
    assert!(!streamed.crs_incomplete);
    let said: Vec<&str> = report.messages.texts().collect();
    assert_eq!(said.iter().filter(|m| m.contains("one of the features has a place in EPSG:32631")).count(), 1, "{said:?}");
    // A read of a box in longitude/latitude leaves it out; it meets none.
    let paris = rustgis_geom::Extent { min_x: 2.0, min_y: 48.5, max_x: 2.7, max_y: 49.2 };
    let view = crate::source::FeatureSource::read(&src, &crate::source::Request { extent: Some(paris), limit: 10, own: None }).unwrap();
    assert_eq!(view.features.len(), 1);
}

/// An export converts each place once and takes the layer's box from that conversion (#2032):
/// the box is [`Dataset::lonlat_extent`]'s exactly, a feature left out for a place it can't give
/// still counting its others, and a layer large enough for several threads is written as one
/// thread writes it, its threads stopping on Cancel.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_export_takes_its_box_from_its_one_conversion_on_any_number_of_threads() {
    use crate::progress::tests::Counted;
    let mut ds = lines(false);
    for i in 0..30_000u32 {
        let x = 300_000.0 + f64::from(i % 997) * 400.0;
        let y = 4_000_000.0 + f64::from(i / 997) * 30_000.0;
        let mut part = vec![Coord::new(x, y), Coord::new(x + 50.0, y + 75.0)];
        match i % 1000 {
            // A place with no longitude/latitude: the feature is left out, its other place kept in the box.
            7 => part.push(Coord::new(1e9, y)),
            // A missing place, kept as it is.
            11 => part.push(Coord::new(f64::NAN, f64::NAN)),
            _ => {}
        }
        ds.push(Some(Geometry::Line(vec![part])), vec![Value::Text(i.to_string())]);
    }
    let to = ds.lonlat_conversion().unwrap();
    // Several threads wherever the computer has them, so the threaded path is what is checked.
    assert!(crate::threads::cores() == 1 || crate::threads::max() > 1, "threads: {}", crate::threads::max());
    let mut threaded = ds.features.to_vec();
    let (unplaced, placed) = crate::io::project_each_leaving_out(&mut threaded, &to, true).unwrap();
    assert_eq!(unplaced, 30, "one feature in a thousand has a place it can't give");
    assert_eq!(placed, ds.lonlat_extent(), "the box lonlat_extent gives, from the one conversion");
    // The one-thread path, a piece too small for threads at a time, gives the same bit for bit.
    let mut one = ds.features.to_vec();
    let mut by_one = (0, rustgis_geom::Extent::EMPTY);
    for piece in one.chunks_mut(1000) {
        let (n, e) = crate::io::project_each_leaving_out(piece, &to, false).unwrap();
        by_one = (by_one.0 + n, by_one.1.union(&e));
    }
    assert_eq!(by_one, (unplaced, placed));
    assert_eq!(format!("{threaded:?}"), format!("{one:?}"));
    // Cancel reaches the threads: the run stops after the pieces under way, short of the end.
    let probe = Counted::cancelling_after(1);
    let _g = crate::progress::install(Some(probe.clone()));
    crate::progress::begin(ds.features.len());
    let mut cancelled = ds.features.to_vec();
    let e = crate::io::project_each_leaving_out(&mut cancelled, &to, true).unwrap_err();
    assert!(matches!(e, crate::DataError::Cancelled), "{e}");
    let done = probe.done.load(std::sync::atomic::Ordering::Relaxed);
    assert!(done < ds.features.len() as u64, "stopped short of the end: {done}");
    // A Cancel once every piece is taken still stops the run: with one feature left (the threads
    // look between features) and, on several threads, after the last (looked at once they finish).
    let n = ds.features.len() as u64;
    let after = if crate::threads::max() > 1 { vec![n - 1, n] } else { vec![n - 1] };
    for after in after {
        let probe = Counted::cancelling_after(after);
        let _g = crate::progress::install(Some(probe.clone()));
        crate::progress::begin(ds.features.len());
        let mut cancelled = ds.features.to_vec();
        let e = crate::io::project_each_leaving_out(&mut cancelled, &to, true).unwrap_err();
        assert!(matches!(e, crate::DataError::Cancelled), "cancelled after {after}: {e}");
    }
}

/// The features a threaded export converts first on its own thread run up to the first with a
/// place converted, past those that never reach the conversion's first-place check: an empty
/// geometry, none, one only missing places and one only places that can't be converted (#2032).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_threaded_export_converts_first_up_to_the_first_place_it_can_convert() {
    let mut ds = Dataset::new("lines", crate::GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
    ds.held_in = utm31();
    ds.crs = Some(utm31());
    ds.push(Some(Geometry::Line(vec![])), vec![Value::Text("empty".into())]);
    ds.push(None, vec![Value::Text("none".into())]);
    ds.push(Some(Geometry::Line(vec![vec![Coord::new(f64::NAN, f64::NAN)]])), vec![Value::Text("missing".into())]);
    ds.push(Some(Geometry::Line(vec![vec![Coord::new(1e9, 4e6)]])), vec![Value::Text("far".into())]);
    for i in 0..10_000u32 {
        let x = 300_000.0 + f64::from(i % 997) * 400.0;
        let part = vec![Coord::new(x, 4_000_000.0), Coord::new(x + 50.0, 4_000_075.0)];
        ds.push(Some(Geometry::Line(vec![part])), vec![Value::Text(i.to_string())]);
    }
    let to = ds.lonlat_conversion().unwrap();
    let mut head = ds.features.to_vec();
    let (first, unplaced, placed) = crate::io::project_one_by_one(&mut head, &to, false, true).unwrap();
    assert_eq!((first, unplaced), (5, 1), "up to and including the first feature with a place converted");
    assert!(!placed.is_empty());
    // With nothing to convert, all of them, and no box.
    let mut none = ds.features.to_vec();
    none.truncate(4);
    let (first, _, placed) = crate::io::project_one_by_one(&mut none, &to, false, true).unwrap();
    assert_eq!(first, 4);
    assert!(placed.is_empty());
    // On several threads, as on one.
    let mut threaded = ds.features.to_vec();
    let got = crate::io::project_each_leaving_out(&mut threaded, &to, false).unwrap();
    let mut one = ds.features.to_vec();
    let (_, n, e) = crate::io::project_one_by_one(&mut one, &to, false, false).unwrap();
    assert_eq!(got, (n, e));
    assert_eq!(format!("{threaded:?}"), format!("{one:?}"));
}
