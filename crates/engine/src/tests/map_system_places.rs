//! Places in the map's or a layer's own system (#2032 C2f-8): Go To XY and Convert Position take
//! an easting and a northing in a named system (`crs`), and a typed position reads them as the
//! map's right-click menu copies them (`2600000 mE  1200000 mN  EPSG:2056`), in the search box
//! too; Georeference's Fit To Map View places the raster in the map's own system.

use rustgis_geom::Coord;
use serde_json::{Value, json};

use super::{out_raster, scan_layer, world};
use crate::Session;

/// Bern's old observatory, the Swiss grid's origin: E 2600000, N 1200000 in LV95 (EPSG:2056).
const BERN: Coord = Coord { x: 7.438_632, y: 46.951_082 };

fn near_bern(s: &Session) -> bool {
    let c = s.map().center_lonlat();
    (c.x - BERN.x).abs() < 2e-3 && (c.y - BERN.y).abs() < 2e-3
}

fn away(s: &mut Session) {
    s.run("map.goToXY", &json!({"lon": 2.35, "lat": 48.85})).unwrap();
    assert!(!near_bern(s));
}

#[test]
fn go_to_xy_takes_an_easting_and_northing_in_a_named_system() {
    let mut s = world();
    // On the Web Mercator map, in LV95 by its code, as numbers or text.
    for (x, y) in [(json!(2_600_000), json!(1_200_000)), (json!("2600000"), json!(" 1200000 "))] {
        away(&mut s);
        s.run("map.goToXY", &json!({"x": x, "y": y, "crs": "EPSG:2056"})).unwrap();
        assert!(near_bern(&s), "{:?}", s.map().center_lonlat());
    }
    // In the map's own system: Web Mercator's metres here, then LV95's on a map drawn in it.
    let merc = rustgis_geom::lonlat_to_merc(BERN);
    away(&mut s);
    s.run("map.goToXY", &json!({"x": merc.x, "y": merc.y, "crs": "map"})).unwrap();
    assert!(near_bern(&s));
    s.run("map.projection", &json!({"type": "EPSG:2056"})).unwrap();
    away(&mut s);
    s.run("map.goToXY", &json!({"x": 2_600_000, "y": 1_200_000, "crs": "MAP"})).unwrap();
    assert!(near_bern(&s));
    // In a layer's system: the sample's Cities are in degrees.
    away(&mut s);
    s.run("map.goToXY", &json!({"x": BERN.x, "y": BERN.y, "crs": "Cities"})).unwrap();
    assert!(near_bern(&s));
    // A system in degrees in a typed position: refused, as degrees are typed latitude first
    // without one, and this form takes the easting first.
    for system in ["EPSG:4326", "EPSG:4258"] {
        let e = s.run("map.goToXY", &json!({"position": format!("40.7128, -74.006 {system}")})).unwrap_err();
        assert!(e.contains("counts in degrees"), "{system}: {e}");
    }
    // Both names of an axis: refused, not one passed over.
    let e = s.run("map.goToXY", &json!({"x": 2_600_000, "lon": 7.4, "y": 1_200_000, "crs": "EPSG:2056"})).unwrap_err();
    assert!(e.contains("not both"), "{e}");
    // Convert Position gives the same place.
    let v = s.run("map.convertPosition", &json!({"x": 2_600_000, "y": 1_200_000, "crs": "EPSG:2056"})).unwrap();
    assert!((v["lon"].as_f64().unwrap() - BERN.x).abs() < 2e-3 && (v["lat"].as_f64().unwrap() - BERN.y).abs() < 2e-3, "{v}");
}

#[test]
fn a_position_with_its_system_reads_as_the_menu_copies_it() {
    let mut s = world();
    for text in
        ["2600000 mE  1200000 mN  EPSG:2056", "2600000 1200000 EPSG:2056", "2600000, 1200000, epsg:2056", "2600000 E 1200000 N EPSG:2056"]
    {
        away(&mut s);
        s.run("map.goToXY", &json!({"position": text})).unwrap_or_else(|e| panic!("{text}: {e}"));
        assert!(near_bern(&s), "{text}: {:?}", s.map().center_lonlat());
    }
    // Web Mercator by the name the readout copies, and in feet.
    let merc = rustgis_geom::lonlat_to_merc(BERN);
    away(&mut s);
    s.run("map.goToXY", &json!({"position": format!("{:.2} mE  {:.2} mN  Web Mercator", merc.x, merc.y)})).unwrap();
    assert!(near_bern(&s));
    let v = s.run("map.convertPosition", &json!({"position": "2300000 ftUSE 10070000 ftUSN EPSG:2277"})).unwrap();
    let (lon, lat) = (v["lon"].as_f64().unwrap_or(0.0), v["lat"].as_f64().unwrap_or(0.0));
    assert!((-100.5..-95.0).contains(&lon) && (29.5..31.5).contains(&lat), "Texas Central, near Austin: {v}");
    // The search box finds it.
    let hits = s.run("map.search", &json!({"query": "2600000 mE  1200000 mN  EPSG:2056"})).unwrap();
    let found = hits.to_string();
    assert!(found.contains("7.43") && found.contains("46.95"), "{found}");
}

#[test]
fn places_in_a_system_are_refused_with_a_reason() {
    let mut s = world();
    let err = |s: &mut Session, p: Value| s.run("map.goToXY", &p).expect_err(&p.to_string());
    assert!(err(&mut s, json!({"x": 1, "y": 2, "crs": "nowhere"})).contains("neither"));
    assert!(err(&mut s, json!({"x": "a", "y": 2, "crs": "EPSG:2056"})).contains("number"));
    assert!(err(&mut s, json!({"position": "2600000 1200000 EPSG:2056", "crs": "EPSG:2056"})).contains("position"));
    assert!(err(&mut s, json!({"x": 1, "y": 2, "crs": 5})).contains("crs"));
    // Far beyond the system's reach, no place.
    assert!(err(&mut s, json!({"x": 1e9, "y": 1e9, "crs": "EPSG:2056"})).contains("no place"));
    // The globe has no flat system of its own.
    s.run("map.projection", &json!({"type": "globe"})).unwrap();
    assert!(err(&mut s, json!({"x": 1, "y": 2, "crs": "map"})).contains("globe"));
    // A unit the system doesn't count in names no place, and the search says why.
    let v = s.run("map.search", &json!({"query": "2600000 ftE 1200000 ftN EPSG:2056"})).unwrap();
    assert_eq!(v["hits"], json!([]), "{v}");
    assert!(v["positionError"].as_str().is_some_and(|e| e.contains("counts in m, not ft")), "{v}");
    let e = s.run("map.search", &json!({"query": "2600000 ftE 1200000 ftN EPSG:2056", "go": true})).unwrap_err();
    assert!(e.contains("as a position") && e.contains("not ft"), "{e}");
    assert!(s.run("map.search", &json!({"query": "2600000 mE 1200000 mN EPSG:2056"})).unwrap().get("positionError").is_none());
}

/// On another body's map, a code names an Earth system: refused, not read as that body's
/// degrees, by `crs` as by a typed position.
#[test]
fn an_earth_system_names_no_place_on_another_body() {
    let mut s = world();
    let err = |s: &mut Session, p: Value| s.run("map.goToXY", &p).expect_err(&p.to_string());
    s.run("map.body", &json!({"name": "mars", "basemap": false})).unwrap();
    for p in [json!({"x": 2_600_000, "y": 1_200_000, "crs": "EPSG:2056"}), json!({"position": "2600000 1200000 EPSG:2056"})] {
        assert!(err(&mut s, p.clone()).contains("system of the Earth"), "{p}");
        let e = s.run("map.convertPosition", &p).expect_err(&p.to_string());
        assert!(e.contains("system of the Earth"), "{p}: {e}");
    }
    // Nor does a layer held in a projection of the Earth, named as the system.
    let mut swiss = rustgis_data::Dataset::new("Swiss", rustgis_data::GeometryType::Point, vec![]);
    swiss.crs = Some(rustgis_data::raster::Crs::Epsg(2056));
    s.map_mut().add_dataset(swiss);
    let p = json!({"x": 2_600_000, "y": 1_200_000, "crs": "Swiss"});
    assert!(err(&mut s, p.clone()).contains("a system of the Earth"), "{p}");
    let e = s.run("map.convertPosition", &p).expect_err(&p.to_string());
    assert!(e.contains("a system of the Earth"), "{e}");
    // A projection of Mars, given by its PROJ text as `crs`, is that map's own: it names a place
    // there. (A typed position names its system by a name, which starts with a letter.)
    let p = json!({"x": 100_000, "y": 200_000, "crs": "+proj=eqc +a=3396190 +b=3396190 +units=m +no_defs"});
    let v = s.run("map.convertPosition", &p).unwrap_or_else(|e| panic!("{p}: {e}"));
    // 100 km east along Mars's equator: 100000 / 3396190 rad.
    let lon = v["lon"].as_f64().unwrap();
    assert!((lon - (100_000.0f64 / 3_396_190.0).to_degrees()).abs() < 1e-6, "{v}");
}

/// Degrees typed before a projected system's code are refused as before `map`; with units written
/// they are its units.
#[test]
fn degrees_typed_with_a_systems_code_are_refused() {
    let mut s = world();
    let e = s.run("map.goToXY", &json!({"position": "46.95 7.44 EPSG:2056"})).unwrap_err();
    assert!(e.contains("reads as degrees") && e.contains("46.95 mE"), "{e}");
    let v = s.run("map.search", &json!({"query": "46.95 7.44 EPSG:2056"})).unwrap();
    assert!(v["positionError"].as_str().is_some_and(|e| e.contains("reads as degrees")), "{v}");
}

/// Another body's projection names no place on the Earth's map: its inverse gives that body's
/// degrees, not the Earth's.
#[test]
fn another_bodys_projection_names_no_place_on_the_earth() {
    let mut s = world();
    let p = json!({"x": 100_000, "y": 200_000, "crs": "+proj=eqc +a=3396190 +b=3396190 +units=m +no_defs"});
    let e = s.run("map.goToXY", &p).unwrap_err();
    assert!(e.contains("a system of another body"), "{e}");
    let mut mars = rustgis_data::Dataset::new("Mars plan", rustgis_data::GeometryType::Point, vec![]);
    mars.crs = Some(rustgis_data::raster::Crs::parse("+proj=eqc +a=3396190 +b=3396190 +units=m +no_defs").unwrap());
    s.map_mut().add_dataset(mars);
    let e = s.run("map.convertPosition", &json!({"x": 100_000, "y": 200_000, "crs": "Mars plan"})).unwrap_err();
    assert!(e.contains("a system of another body"), "{e}");
}

/// A raster whose pixels couldn't be read names no system: refused with the reason, not taken
/// for longitude and latitude.
#[test]
fn a_raster_with_no_pixels_names_no_system() {
    let mut s = world();
    scan_layer(&mut s, "relief");
    let id = s.map().layer_by_name("relief").unwrap().id;
    s.map_mut().layer_mut(id).unwrap().raster.as_mut().unwrap().data = None;
    let e = s.run("map.goToXY", &json!({"x": 40, "y": 30, "crs": "relief"})).unwrap_err();
    assert!(e.contains("has no pixels loaded"), "{e}");
}

/// One body's projection names no place on another body's map either: Mars's inverse gives no
/// place on the Moon. The Moon's own projection does.
#[test]
fn a_projection_of_mars_names_no_place_on_the_moon() {
    let mut s = world();
    s.run("map.body", &json!({"name": "moon", "basemap": false})).unwrap();
    let p = json!({"x": 100_000, "y": 200_000, "crs": "+proj=eqc +a=3396190 +b=3396190 +units=m +no_defs"});
    let e = s.run("map.goToXY", &p).unwrap_err();
    assert!(e.contains("a system of Mars"), "{e}");
    let mut mars = rustgis_data::Dataset::new("Mars plan", rustgis_data::GeometryType::Point, vec![]);
    mars.crs = Some(rustgis_data::raster::Crs::parse("+proj=eqc +a=3396190 +b=3396190 +units=m +no_defs").unwrap());
    s.map_mut().add_dataset(mars);
    let e = s.run("map.convertPosition", &json!({"x": 100_000, "y": 200_000, "crs": "Mars plan"})).unwrap_err();
    assert!(e.contains("a system of Mars"), "{e}");
    let moon = json!({"x": 100_000, "y": 200_000, "crs": "+proj=eqc +a=1737400 +b=1737400 +units=m +no_defs"});
    let v = s.run("map.convertPosition", &moon).unwrap_or_else(|e| panic!("{e}"));
    assert!((v["lon"].as_f64().unwrap() - (100_000.0f64 / 1_737_400.0).to_degrees()).abs() < 1e-6, "{v}");
}

/// Degrees typed before `map` (latitude first, as without a system) are refused with the reason,
/// not read as metres a few steps from the map's origin; in the search box too.
#[test]
fn degrees_typed_with_the_maps_name_are_refused() {
    let mut s = world();
    for text in ["40.7, -74.0 map", "-74.0 40.7 map"] {
        let e = s.run("map.goToXY", &json!({"position": text})).unwrap_err();
        assert!(e.contains("reads as degrees"), "{text}: {e}");
        let v = s.run("map.search", &json!({"query": text})).unwrap();
        assert!(v["positionError"].as_str().is_some_and(|e| e.contains("reads as degrees")), "{text}: {v}");
    }
    // Metres in the map's own system still go.
    let merc = rustgis_geom::lonlat_to_merc(BERN);
    away(&mut s);
    s.run("map.goToXY", &json!({"position": format!("{:.0} {:.0} map", merc.x, merc.y)})).unwrap();
    assert!(near_bern(&s));
    // Small numbers with a unit written say they are metres: taken as such, by the map's origin.
    s.run("map.goToXY", &json!({"position": "50 mE 30 mN map"})).unwrap();
    let c = s.map().center_lonlat();
    assert!(c.x.abs() < 1e-3 && c.y.abs() < 1e-3, "{c:?}");
}

/// A position in Web Mercator by a name only data's systems take is in metres, as Web Mercator
/// is: a unit of metres written with it is its own.
#[test]
fn web_mercator_by_any_name_counts_in_metres() {
    let mut s = world();
    let merc = rustgis_geom::lonlat_to_merc(BERN);
    // Names a map projection doesn't take, so they come by data's systems.
    for name in ["EPSG:102100", "EPSG:102113"] {
        assert_eq!(rustgis_data::raster::Crs::parse(name).ok(), Some(rustgis_data::raster::Crs::WebMercator), "{name}");
        assert!(rustgis_map::Projection::from_code(name).is_err(), "{name}");
        away(&mut s);
        let text = format!("{:.2} mE  {:.2} mN  {name}", merc.x, merc.y);
        s.run("map.goToXY", &json!({"position": text})).unwrap_or_else(|e| panic!("{text}: {e}"));
        assert!(near_bern(&s), "{name}");
    }
}

/// Fit To Map View on a projected map places the raster in the map's own system over the view's
/// rectangle, not over a box of degrees.
#[test]
fn fit_to_view_places_the_raster_in_the_maps_system() {
    let mut s = world();
    s.view_size = (800.0, 600.0);
    scan_layer(&mut s, "scan");
    s.run("map.projection", &json!({"type": "EPSG:2056"})).unwrap();
    s.run("map.goToXY", &json!({"lon": BERN.x, "lat": BERN.y, "scale": 50_000})).unwrap();
    let v = s.run("raster.georeference", &json!({"layer": "scan", "extent": "view"})).unwrap();
    let r = out_raster(&s, &v);
    assert_eq!(r.crs, rustgis_data::raster::Crs::Epsg(2056));
    let view = s.map().camera.extent(800.0, 600.0);
    let t = r.transform;
    let (right, bottom) = (t.origin_x + t.pixel_width * f64::from(r.width), t.origin_y + t.pixel_height * f64::from(r.height));
    for (a, b) in [(t.origin_x, view.min_x), (bottom, view.min_y), (right, view.max_x), (t.origin_y, view.max_y)] {
        assert!((a - b).abs() < 1e-6, "{t:?} against the view {view:?}");
    }
    // On Web Mercator, in its metres.
    s.run("map.projection", &json!({"type": "mercator"})).unwrap();
    let v = s.run("raster.georeference", &json!({"layer": "scan", "extent": "view"})).unwrap();
    assert_eq!(out_raster(&s, &v).crs, rustgis_data::raster::Crs::WebMercator);
    // A world copy past the antimeridian is fitted where it is within the world.
    s.run("map.goToXY", &json!({"lon": 170.0, "lat": 0.0, "scale": 50_000})).unwrap();
    let world = 2.0 * std::f64::consts::PI * rustgis_geom::MERCATOR_R;
    s.map_mut().camera.center.x += world;
    let v = s.run("raster.georeference", &json!({"layer": "scan", "extent": "view"})).unwrap();
    let t = out_raster(&s, &v).transform;
    assert!((t.origin_x - (s.map().camera.center.x - world)).abs() < 50_000.0, "{t:?}");
    // A turned or tilted view, or control points with a system but no transform, are refused.
    s.run("map.setBearing", &json!({"bearing": 30})).unwrap();
    let e = s.run("raster.georeference", &json!({"layer": "scan", "extent": "view"})).unwrap_err();
    assert!(e.contains("turned or tilted"), "{e}");
    let e = s
        .run("raster.georeference", &json!({"layer": "scan", "points": [[0, 0, 7, 47], [10, 10, 8, 46]], "crs": "EPSG:2056"}))
        .unwrap_err();
    assert!(e.contains("transform"), "{e}");
    // Four numbers in a named system; degrees without one.
    let v = s
        .run("raster.georeference", &json!({"layer": "scan", "extent": [2_599_000, 1_199_000, 2_601_000, 1_201_000], "crs": "EPSG:2056"}))
        .unwrap();
    assert_eq!(out_raster(&s, &v).crs, rustgis_data::raster::Crs::Epsg(2056));
    let v = s.run("raster.georeference", &json!({"layer": "scan", "extent": [7.0, 46.0, 8.0, 47.0]})).unwrap();
    assert_eq!(out_raster(&s, &v).crs, rustgis_data::raster::Crs::LonLat);
    assert!(s.run("raster.georeference", &json!({"layer": "scan", "extent": "view", "crs": "EPSG:2056"})).is_err());
    // What an extent doesn't use is refused, not passed over: control points' system, and an
    // extent beside a transform.
    let e =
        s.run("raster.georeference", &json!({"layer": "scan", "extent": [7.0, 46.0, 8.0, 47.0], "pointsCrs": "EPSG:2056"})).unwrap_err();
    assert!(e.contains("pointsCrs goes with control points"), "{e}");
    let e = s
        .run(
            "raster.georeference",
            &json!({"layer": "scan", "extent": [7.0, 46.0, 8.0, 47.0], "transform": "affine", "points": [[0, 0, 7.0, 47.0], [10, 0, 8.0, 47.0], [0, 10, 7.0, 46.0]]}),
        )
        .unwrap_err();
    assert!(e.contains("not both"), "{e}");
    // Control points beside an extent, with no transform: refused, the layer as it was.
    let before = s.map().layer_by_name("scan").unwrap().raster.as_ref().unwrap().data.as_ref().unwrap().transform;
    let e = s
        .run(
            "raster.georeference",
            &json!({"layer": "scan", "extent": [7.0, 46.0, 8.0, 47.0], "points": [[0, 0, 7.0, 47.0], [10, 10, 8.0, 46.0]]}),
        )
        .unwrap_err();
    assert!(e.contains("an extent or control points, not both"), "{e}");
    // What only a warp uses is refused without a transform, the layer placed nowhere new.
    for (key, value) in
        [("name", json!("aligned")), ("path", json!("aligned.tif")), ("cellSize", json!(0.01)), ("resampling", json!("cubic"))]
    {
        let mut p = json!({"layer": "scan", "extent": [7.0, 46.0, 8.0, 47.0]});
        p[key] = value;
        let e = s.run("raster.georeference", &p).unwrap_err();
        assert!(e.contains(&format!("{key} goes with a transform")), "{key}: {e}");
    }
    assert!(s.map().layer_by_name("aligned").is_none());
    assert_eq!(s.map().layer_by_name("scan").unwrap().raster.as_ref().unwrap().data.as_ref().unwrap().transform, before);
    // An entry that isn't a number is refused, not passed over, and nothing is placed.
    let before = s.map().layers.len();
    for extent in [json!([0, 0, null, 10, 10]), json!([7, 46, 8, 47, "x"]), json!([7, 46, 8, "47"])] {
        let e = s.run("raster.georeference", &json!({"layer": "scan", "extent": extent})).unwrap_err();
        assert!(e.contains("[minX, minY, maxX, maxY]"), "{extent}: {e}");
    }
    assert_eq!(s.map().layers.len(), before);
}

/// A layer named as a system means the layer; `map` after a position is the map's own system; a
/// unit that isn't the system's is refused rather than read as metres.
#[test]
fn names_and_units_mean_what_they_say() {
    let mut s = world();
    // The sample's Cities, in degrees, renamed as LV95's bare code.
    s.run("layer.rename", &json!({"layer": "Cities", "name": "2056"})).unwrap();
    away(&mut s);
    s.run("map.goToXY", &json!({"x": BERN.x, "y": BERN.y, "crs": "2056"})).unwrap();
    assert!(near_bern(&s), "the layer, not EPSG:2056: {:?}", s.map().center_lonlat());
    s.run("map.projection", &json!({"type": "EPSG:2056"})).unwrap();
    away(&mut s);
    s.run("map.goToXY", &json!({"position": "2600000 mE 1200000 mN map"})).unwrap();
    assert!(near_bern(&s), "{:?}", s.map().center_lonlat());
    let e = s.run("map.goToXY", &json!({"position": "2600000 ftE 1200000 ftN EPSG:2056"})).unwrap_err();
    assert!(e.contains("counts in m, not ft"), "{e}");
}

/// A raster layer's system is its pixels' (a raster names none on its features): named in `crs`,
/// an LV95 raster takes an easting and a northing in LV95.
#[test]
fn a_raster_layers_system_is_its_pixels() {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
    let mut s = world();
    let gt = GeoTransform { origin_x: 2_599_000.0, origin_y: 1_201_000.0, pixel_width: 100.0, pixel_height: -100.0 };
    let r = Raster::new("relief", 20, 20, vec![vec![1.0; 400]], gt, Crs::Epsg(2056), None, ColorHint::Gray, "Float32").unwrap();
    let bytes = geotiff::write(&r, 0).unwrap();
    crate::cmd::raster::add_raster(&mut s, "relief", rustgis_map::RasterLayer::from_bytes(r, "relief.tif".to_string(), bytes)).unwrap();
    away(&mut s);
    s.run("map.goToXY", &json!({"x": 2_600_000, "y": 1_200_000, "crs": "relief"})).unwrap();
    assert!(near_bern(&s));
}

/// X and Y are an easting and a northing: a system counted west, then south (as South Africa's
/// Lo systems are) would take them mirrored, so it is refused with the reason.
#[test]
fn a_system_counted_other_than_east_then_north_is_refused() {
    let mut s = world();
    let lo29 = "+proj=tmerc +lat_0=0 +lon_0=29 +k=1 +x_0=0 +y_0=0 +axis=wsu +ellps=WGS84 +units=m +no_defs";
    let e = s.run("map.goToXY", &json!({"x": 10_000, "y": 3_000_000, "crs": lo29})).unwrap_err();
    assert!(e.contains("counts a westing, then a southing"), "{e}");
    // By its EPSG code too (a code a map projection also takes), and as a typed position.
    let e = s.run("map.goToXY", &json!({"x": 10_000, "y": 3_000_000, "crs": "EPSG:22275"})).unwrap_err();
    assert!(e.contains("counts a westing, then a southing"), "{e}");
    let e = s.run("map.goToXY", &json!({"position": "10000 3000000 EPSG:22275"})).unwrap_err();
    assert!(e.contains("counts a westing, then a southing"), "{e}");
    // A system whose EPSG definition names the northing first (LAEA Europe) still takes an
    // easting, then a northing, as RustGIS holds and writes its coordinates.
    s.run("map.goToXY", &json!({"position": "4200000 2600000 EPSG:3035"})).unwrap();
}

/// A place in Robinson, whose inverse iterates (to about 1.5e-5°, some 1.6 m), is taken: its
/// round trip comes back within a millionth of the coordinate's size.
#[test]
fn a_place_in_robinson_is_taken() {
    let mut s = world();
    let robinson = rustgis_data::raster::Crs::parse("ESRI:54030").unwrap();
    let at = robinson.transformer().unwrap().from_lonlat(rustgis_geom::Coord::new(20.0, 30.0)).unwrap();
    let v = s.run("map.convertPosition", &json!({"x": at.x, "y": at.y, "crs": "ESRI:54030"})).unwrap();
    let (lon, lat) = (v["lon"].as_f64().unwrap(), v["lat"].as_f64().unwrap());
    assert!((lon - 20.0).abs() < 1e-4 && (lat - 30.0).abs() < 1e-4, "{v}");
}
