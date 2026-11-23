//! The status bar's coordinate readout: a click steps map → decimal → DMS → UTM → MGRS → layer, the choice
//! is remembered as a preference, and the readout shows the pointer in it.

use std::sync::{Arc, Mutex};

use egui_kittest::kittest::Queryable;
use rustgis_ui_egui::{CoordReadout, MapSystem, RustGisApp, Services};
use serde_json::{Value, json};

fn harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
    egui_kittest::Harness::builder().with_size(egui::vec2(1600.0, 900.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    )
}

#[test]
fn the_format_cycles_persists_and_is_restored() {
    let saved = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let writes = saved.clone();
    let mut app = RustGisApp::with_sample().with_services(Services {
        save_pref: Some(Box::new(move |k, v| writes.lock().unwrap_or_else(|e| e.into_inner()).push((k.to_string(), v.to_string())))),
        ..Services::default()
    });
    assert_eq!(app.map.coord_format, CoordReadout::Map);
    let mut seen = Vec::new();
    for _ in 0..6 {
        app.run("ui.coordinateFormat", Value::Null).unwrap();
        seen.push(app.map.coord_format.code());
    }
    assert_eq!(seen, ["decimal", "dms", "utm", "mgrs", "layer", "map"]);
    let writes: Vec<String> = saved.lock().unwrap().iter().filter(|(k, _)| k == "coordinate_format").map(|(_, v)| v.clone()).collect();
    assert_eq!(writes, ["decimal", "dms", "utm", "mgrs", "layer", "map"]);
    app.run("ui.coordinateFormat", json!({"format": "MGRS"})).unwrap();
    assert_eq!(app.map.coord_format, CoordReadout::Mgrs);
    for bad in [json!({"format": "ups"}), json!({"format": 3}), json!({"format": ""})] {
        assert!(app.run("ui.coordinateFormat", bad.clone()).is_err(), "{bad}");
        assert_eq!(app.map.coord_format, CoordReadout::Mgrs, "{bad}");
    }
    // A saved choice comes back, with nothing to say.
    for (pref, want) in [
        ("utm", CoordReadout::Utm),
        ("mgrs", CoordReadout::Mgrs),
        ("decimal", CoordReadout::Decimal),
        ("map", CoordReadout::Map),
        ("layer", CoordReadout::Layer),
    ] {
        let restored = RustGisApp::with_sample().with_services(Services {
            load_pref: Some(Box::new(move |k| (k == "coordinate_format").then(|| pref.to_string()))),
            ..Services::default()
        });
        assert_eq!(restored.map.coord_format, want, "{pref}");
        assert!(!said_unknown(&restored), "{pref}");
    }
}

fn said_unknown(app: &RustGisApp) -> bool {
    app.session.notices.iter().any(|n| n.text.contains("saved coordinate format isn't known"))
}

#[test]
fn an_unknown_saved_format_is_said_and_kept_until_the_user_picks_one() {
    let saved = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let writes = saved.clone();
    let app = RustGisApp::with_sample().with_services(Services {
        load_pref: Some(Box::new(|k| (k == "coordinate_format").then(|| "lambert".to_string()))),
        save_pref: Some(Box::new(move |k, v| writes.lock().unwrap_or_else(|e| e.into_inner()).push((k.to_string(), v.to_string())))),
        ..Services::default()
    });
    let format_writes =
        || saved.lock().unwrap().iter().filter(|(k, _)| k == "coordinate_format").map(|(_, v)| v.clone()).collect::<Vec<_>>();
    // Read in the map's coordinates, and said once.
    assert_eq!(app.map.coord_format, CoordReadout::Map);
    assert_eq!(app.session.notices.iter().filter(|n| n.text.contains("saved coordinate format isn't known")).count(), 1);
    // Frames drawn and another setting saved leave the stored format alone.
    let mut h = harness(app);
    hover_map(&mut h);
    h.state_mut().run("ui.measurementUnits", json!({"units": "imperial"})).unwrap();
    hover_map(&mut h);
    assert!(saved.lock().unwrap().iter().any(|(k, _)| k == "measurement_units"));
    assert!(format_writes().is_empty(), "{:?}", format_writes());
    // A click on the readout is the user's choice: it is saved.
    let shown = map_readout(&h);
    h.get_by_label(&shown).click();
    hover_map(&mut h);
    assert_eq!(h.state().map.coord_format, CoordReadout::Decimal);
    assert_eq!(format_writes(), ["decimal"]);
}

/// The readout in the map's coordinates of where the pointer is.
fn map_readout(h: &egui_kittest::Harness<'_, RustGisApp>) -> String {
    let app = h.state();
    let at = app.map.pointer_lonlat.expect("the pointer is over the map");
    CoordReadout::Map.readout(Some(at), true, MapSystem::of(app.session.map()).as_ref())
}

/// Points the mouse at the middle of the window, over the map, for a few frames.
fn hover_map(h: &mut egui_kittest::Harness<'_, RustGisApp>) {
    for _ in 0..3 {
        h.input_mut().events.push(egui::Event::PointerMoved(egui::pos2(800.0, 450.0)));
        h.step();
    }
}

#[test]
fn the_status_bar_reads_the_pointer_in_the_chosen_format_and_a_click_switches() {
    let mut h = harness(RustGisApp::with_sample());
    // A view about 4 km across around Baghdad (38S MB 44140 84706, at 38north 444141 3684706).
    h.state_mut().run("map.goToXY", json!({"lon": 44.4, "lat": 33.3, "scale": 10000})).unwrap();
    hover_map(&mut h);
    let at = h.state().map.pointer_lonlat.expect("the pointer is over the map");
    assert!((at.x - 44.4).abs() < 0.05 && (at.y - 33.3).abs() < 0.05, "{at:?}");
    // First in the map's own coordinates: Web Mercator's metres, about 4.94 million east.
    let shown = map_readout(&h);
    assert!(shown.starts_with("494") && shown.ends_with(" mN"), "{shown}");
    h.get_by_label(&shown).click();
    hover_map(&mut h);
    assert_eq!(h.state().map.coord_format, CoordReadout::Decimal);
    let at = h.state().map.pointer_lonlat.expect("the pointer is over the map");
    let decimal = rustgis_engine::geom::format_lonlat(at);
    h.get_by_label(&decimal).click();
    hover_map(&mut h);
    assert_eq!(h.state().map.coord_format, CoordReadout::Dms);
    let at = h.state().map.pointer_lonlat.unwrap();
    h.get_by_label(&rustgis_engine::geom::format_dms(at));
    h.get_by_label(&rustgis_engine::geom::format_dms(at)).click();
    hover_map(&mut h);
    assert_eq!(h.state().map.coord_format, CoordReadout::Utm);
    let utm = rustgis_engine::geom::format_utm(h.state().map.pointer_lonlat.unwrap()).unwrap();
    assert!(utm.starts_with("38north 44"), "{utm}");
    h.get_by_label(&utm).click();
    hover_map(&mut h);
    assert_eq!(h.state().map.coord_format, CoordReadout::Mgrs);
    let mgrs = rustgis_engine::geom::format_mgrs(h.state().map.pointer_lonlat.unwrap()).unwrap();
    assert!(mgrs.starts_with("38S MB 4"), "{mgrs}");
    h.get_by_label(&mgrs).click();
    hover_map(&mut h);
    assert_eq!(h.state().map.coord_format, CoordReadout::Layer);
    // The sample's layers are in longitude and latitude: layer coordinates read in degrees.
    let at = h.state().map.pointer_lonlat.unwrap();
    let layer = CoordReadout::Layer.readout(
        Some(at),
        true,
        CoordReadout::Layer.system(h.state().session.map(), h.state().session.active_layer).as_ref(),
    );
    assert_eq!(layer, rustgis_engine::geom::format_lonlat(at));
    h.get_by_label(&layer).click();
    hover_map(&mut h);
    assert_eq!(h.state().map.coord_format, CoordReadout::Map);
    // Where UTM has no zone the readout gives the polar grid, UPS (MGRS without a zone number).
    h.state_mut().run("ui.coordinateFormat", json!({"format": "mgrs"})).unwrap();
    h.state_mut().run("map.goToXY", json!({"lon": 10.0, "lat": 84.5, "scale": 10000})).unwrap();
    hover_map(&mut h);
    let at = h.state().map.pointer_lonlat.unwrap();
    assert!(at.y > 84.0, "{at:?}");
    let polar = rustgis_engine::geom::format_mgrs(at).unwrap();
    assert!(polar.starts_with("Z "), "{polar}");
    h.get_by_label(&polar);
    // On another body's map UTM and MGRS, the Earth's grids, read nothing either.
    h.state_mut().run("map.body", json!({"name": "mars", "basemap": false})).unwrap();
    h.state_mut().run("map.goToXY", json!({"lon": 44.4, "lat": 33.3, "scale": 10000})).unwrap();
    hover_map(&mut h);
    assert!(h.state().map.pointer_lonlat.is_some_and(|c| (c.y - 33.3).abs() < 0.05));
    h.get_by_label("No MGRS here");
    h.state_mut().run("ui.coordinateFormat", json!({"format": "decimal"})).unwrap();
    hover_map(&mut h);
    h.get_by_label(&rustgis_engine::geom::format_lonlat(h.state().map.pointer_lonlat.unwrap()));
    // There the map's coordinates, which its projection would give on the Earth's figure, read
    // as degrees.
    h.state_mut().run("ui.coordinateFormat", json!({"format": "map"})).unwrap();
    hover_map(&mut h);
    h.get_by_label(&rustgis_engine::geom::format_lonlat(h.state().map.pointer_lonlat.unwrap()));
}

/// A map in the Swiss grid (LV95) reads the pointer in its eastings and northings, in metres, as
/// does the right-click menu's first row; the globe reads degrees.
#[test]
fn a_map_in_lv95_reads_eastings_and_northings() {
    let mut h = harness(RustGisApp::with_sample());
    h.state_mut().run("map.projection", json!({"type": "EPSG:2056"})).unwrap();
    h.state_mut().run("map.goToXY", json!({"lon": 7.4386, "lat": 46.9511, "scale": 10000})).unwrap();
    hover_map(&mut h);
    let shown = map_readout(&h);
    let (e, n) = shown.split_once("  ").expect("an easting and a northing");
    let e: f64 = e.strip_suffix(" mE").and_then(|v| v.parse().ok()).expect("metres east");
    let n: f64 = n.strip_suffix(" mN").and_then(|v| v.parse().ok()).expect("metres north");
    assert!((e - 2_600_000.0).abs() < 2_000.0 && (n - 1_200_000.0).abs() < 2_000.0, "{shown}");
    h.get_by_label(&shown);
    h.state_mut().run("map.projection", json!({"type": "globe"})).unwrap();
    h.state_mut().run("map.goToXY", json!({"lon": 7.4386, "lat": 46.9511, "scale": 10000})).unwrap();
    hover_map(&mut h);
    h.get_by_label(&rustgis_engine::geom::format_lonlat(h.state().map.pointer_lonlat.expect("the pointer is over the globe")));
}

/// Moves the mouse onto the status bar, off the map, for a few frames.
fn hover_status_bar(h: &mut egui_kittest::Harness<'_, RustGisApp>) {
    for _ in 0..3 {
        h.input_mut().events.push(egui::Event::PointerMoved(egui::pos2(800.0, 892.0)));
        h.step();
    }
}

fn readout_at_center(h: &mut egui_kittest::Harness<'_, RustGisApp>) -> String {
    h.state_mut().run("ui.coordinateFormat", json!({"format": "decimal"})).unwrap();
    h.state_mut().run("map.goToXY", json!({"lon": 44.4, "lat": 33.3, "scale": 10000})).unwrap();
    hover_map(h);
    rustgis_engine::geom::format_lonlat(h.state().map.pointer_lonlat.expect("the pointer is over the map"))
}

#[test]
fn the_readout_is_a_dash_until_the_pointer_has_been_over_the_map() {
    let mut h = harness(RustGisApp::with_sample());
    h.step();
    h.get_by_label("—");
    hover_status_bar(&mut h);
    h.get_by_label("—");
    assert!(h.state().map.pointer_lonlat.is_none());
}

#[test]
fn the_position_stays_when_the_pointer_leaves_the_map_and_a_click_switches_format() {
    let mut h = harness(RustGisApp::with_sample());
    let decimal = readout_at_center(&mut h);
    hover_status_bar(&mut h);
    assert!(h.state().map.pointer_lonlat.is_none());
    h.get_by_label(&decimal);
    // The pointer in the window's corner, or gone altogether, keeps it too.
    h.input_mut().events.push(egui::Event::PointerGone);
    h.step();
    h.get_by_label(&decimal);
    // A click on the readout shows the same position in the next format.
    h.get_by_label(&decimal).click();
    h.step();
    h.step();
    assert_eq!(h.state().map.coord_format, CoordReadout::Dms);
    let at = h.state().map.pointer_lonlat;
    assert!(at.is_none());
    h.get_by_label_contains("44°");
    // The pointer back over the map follows it again.
    hover_map(&mut h);
    assert!(h.state().map.pointer_lonlat.is_some());
}

#[test]
fn the_position_is_dropped_on_another_map_or_project() {
    let mut h = harness(RustGisApp::with_sample());
    let decimal = readout_at_center(&mut h);
    hover_status_bar(&mut h);
    h.get_by_label(&decimal);
    // Another map becoming the active one.
    h.state_mut().run("map.new", json!({"name": "Second Map"})).unwrap();
    h.step();
    h.step();
    assert!(h.query_by_label(&decimal).is_none());
    h.get_by_label("—");
    // Back on the first map it is not remembered from before.
    h.state_mut().run("map.activate", json!({"name": "World Atlas"})).ok();
    h.step();
    h.step();
    h.get_by_label("—");
    // A new position, then another project replacing this one.
    let decimal = readout_at_center(&mut h);
    hover_status_bar(&mut h);
    h.get_by_label(&decimal);
    h.state_mut().run("project.new", json!({})).unwrap();
    h.step();
    h.step();
    assert!(h.query_by_label(&decimal).is_none());
    h.get_by_label("—");
}

/// Layer coordinates read in the projected system the active layer was read in, in its unit, as
/// `cs2cs` gives them; in degrees for a layer in degrees, with no layer active and on another
/// body's map; and say so where the system doesn't reach.
#[test]
fn layer_coordinates_read_in_the_active_layers_system() {
    use rustgis_engine::data::{Dataset, GeometryType, raster::Crs};
    let mut app = RustGisApp::with_sample();
    fn add_to(app: &mut RustGisApp, crs: Option<Crs>) -> u32 {
        let mut ds = Dataset::new("held", GeometryType::Point, Vec::new());
        ds.crs = crs;
        app.session.map_mut().add_dataset(ds)
    }
    let (laea, texas, merc, lonlat, nad27) = (
        add_to(&mut app, Some(Crs::from_epsg(3035).unwrap())),
        add_to(&mut app, Some(Crs::from_epsg(2277).unwrap())),
        add_to(&mut app, Some(Crs::WebMercator)),
        add_to(&mut app, None),
        add_to(&mut app, Some(Crs::from_epsg(4267).unwrap())),
    );
    let read = |app: &RustGisApp, id: Option<u32>, at: rustgis_engine::geom::Coord| {
        CoordReadout::Layer.readout(
            Some(at),
            app.session.map().body().is_earth(),
            CoordReadout::Layer.system(app.session.map(), id).as_ref(),
        )
    };
    let number =
        |part: &str, suffix: &str| -> f64 { part.trim().strip_suffix(suffix).unwrap_or_else(|| panic!("{part}")).parse().unwrap() };
    // ETRS89 / LAEA Europe: `echo "49.85 15.3" | cs2cs EPSG:4326 EPSG:3035` gives N 2984581.1, E 4701839.1.
    let p = rustgis_engine::geom::Coord::new(15.3, 49.85);
    let r = read(&app, Some(laea), p);
    let (e, n) = r.split_once("  ").unwrap();
    assert!((number(e, " mE") - 4_701_839.1).abs() < 1.0 && (number(n, " mN") - 2_984_581.1).abs() < 1.0, "{r}");
    assert_eq!(CoordReadout::Layer.system(app.session.map(), Some(laea)).unwrap().name(), "EPSG:3035");
    // Texas Central in US survey feet.
    let r = read(&app, Some(texas), rustgis_engine::geom::Coord::new(-97.74, 30.27));
    assert!(r.contains(" ftUSE  ") && r.ends_with(" ftUSN"), "{r}");
    // Web Mercator in its metres.
    let r = read(&app, Some(merc), rustgis_engine::geom::Coord::new(0.0, 0.0));
    assert_eq!(r, "0 mE  0 mN");
    assert_eq!(CoordReadout::Layer.system(app.session.map(), Some(merc)).unwrap().name(), "Web Mercator");
    // In degrees: a layer in longitude/latitude, one in another datum's degrees, none active.
    for id in [Some(lonlat), Some(nad27), None] {
        assert!(CoordReadout::Layer.system(app.session.map(), id).is_none(), "{id:?}");
        assert_eq!(read(&app, id, p), rustgis_engine::geom::format_lonlat(p), "{id:?}");
    }
    // A westing and southing (South Africa's Lo29, `+axis=wsu`) would be mislabelled as an
    // easting and northing: degrees instead.
    let lo29 = Crs::parse("+proj=tmerc +lat_0=0 +lon_0=29 +k=1 +x_0=0 +y_0=0 +axis=wsu +ellps=WGS84 +units=m +no_defs").unwrap();
    assert_ne!(lo29.axis_directions(), [(1.0, 0.0), (0.0, 1.0)]);
    let lo29 = add_to(&mut app, Some(lo29));
    assert!(CoordReadout::Layer.system(app.session.map(), Some(lo29)).is_none());
    assert_eq!(
        read(&app, Some(lo29), rustgis_engine::geom::Coord::new(29.0, -26.0)),
        rustgis_engine::geom::format_lonlat(rustgis_engine::geom::Coord::new(29.0, -26.0))
    );
    // The hover names the format.
    assert!(CoordReadout::Layer.hover().contains("active layer"));
    // On another body's map, degrees.
    app.run("map.body", json!({"name": "mars", "basemap": false})).unwrap();
    assert!(CoordReadout::Layer.system(app.session.map(), Some(laea)).is_none());
}
