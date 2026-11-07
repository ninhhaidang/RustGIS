//! The map's right-click menu: the clicked coordinate (a click copies it), quick actions on the
//! place, Quick Analysis tools run there, and the place in Google Maps or Google Earth.

use egui::Ui;
use rustgis_engine::geom::Coord;
use serde_json::json;

use crate::widgets::{menu_row, submenu_row};
use crate::{CoordReadout, RustGisApp, i18n};

/// Decimal places of the coordinates shown, copied and sent to Google (~0.1 m).
const PRECISION: usize = 6;

/// Viewshed radii in metres: small enough to stay interactive.
const VIEWSHED_RADII: [f64; 3] = [2000.0, 5000.0, 15000.0];

/// The Buffer tool's distances on offer, in the units the Directions pane uses.
fn buffer_presets(imperial: bool) -> [(f64, &'static str); 3] {
    if imperial { [(0.25, "Miles"), (1.0, "Miles"), (5.0, "Miles")] } else { [(500.0, "Meters"), (1.0, "Kilometers"), (5.0, "Kilometers")] }
}

/// A distance with its unit's abbreviation ("500 m", "0.25 mi").
fn distance_label(d: f64, units: &str) -> String {
    let unit = match units {
        "Miles" => "mi",
        "Meters" => "m",
        _ => "km",
    };
    format!("{} {unit}", trim(d))
}

/// A viewshed radius in the units the Directions pane uses ("5 km", "3.1 mi").
fn radius_label(m: f64, imperial: bool) -> String {
    if imperial {
        format!("{} mi", trim((m / 1609.344 * 10.0).round() / 10.0))
    } else if m >= 1000.0 {
        format!("{} km", trim((m / 100.0).round() / 10.0))
    } else {
        format!("{} m", trim(m.round()))
    }
}

/// A number without trailing zeros (`1`, `0.25`).
fn trim(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// "lat, lon", as Google Maps writes a place.
fn lat_lon(c: Coord) -> String {
    format!("{:.p$}, {:.p$}", c.y, c.x, p = PRECISION)
}

/// The coordinate row: the text it shows and copies, in the readout's format, and whether it fell
/// back from it. Decimal degrees are `lat, lon` ([`lat_lon`]); north of 84° N or south of 80° S,
/// and on another body's map (`earth` false), UTM and MGRS fall back to them (and the row's hover
/// text says why).
fn coordinate_row(c: Coord, f: CoordReadout, earth: bool) -> (String, bool) {
    match f {
        CoordReadout::Decimal => (lat_lon(c), false),
        _ => match f.format(c, earth) {
            Some(text) => (text, false),
            None => (lat_lon(c), true),
        },
    }
}

/// The place as a GeoJSON point feature.
fn point_geojson(c: Coord) -> String {
    let f = json!({"type": "Feature", "geometry": {"type": "Point", "coordinates": [c.x, c.y]}, "properties": {}});
    serde_json::to_string_pretty(&f).unwrap_or_default()
}

/// A longitude within ±180 (a view can run past the antimeridian).
fn wrap_lon(lon: f64) -> f64 {
    (lon + 180.0).rem_euclid(360.0) - 180.0
}

/// The web map zoom level showing `ground_res` metres per pixel at latitude `lat`.
fn web_zoom(ground_res: f64, lat: f64) -> f64 {
    let z = (156_543.033_92 * lat.to_radians().cos().abs() / ground_res).log2();
    if z.is_finite() { z } else { 12.0 }
}

/// Google Maps with a pin on the place, at zoom `zoom`.
fn google_maps_url(c: Coord, zoom: f64) -> String {
    let (lat, lon) = (format!("{:.p$}", c.y.clamp(-90.0, 90.0), p = PRECISION), format!("{:.p$}", wrap_lon(c.x), p = PRECISION));
    format!("https://www.google.com/maps/place/{lat},{lon}/@{lat},{lon},{}z", trim(zoom.clamp(2.0, 21.0)))
}

/// Google Earth on the web looking down on the place from the height zoom `zoom` shows.
fn google_earth_url(c: Coord, zoom: f64) -> String {
    let lat = c.y.clamp(-90.0, 90.0);
    let distance = (248_500_000.0 * lat.to_radians().cos() / 2f64.powf(zoom)).clamp(150.0, 65_000_000.0).round();
    format!("https://earth.google.com/web/@{lat:.p$},{:.p$},0a,{distance}d,35y,0h,0t,0r", wrap_lon(c.x), p = PRECISION)
}

/// The elevation raster for a viewshed: the active layer when it is a one-band raster, else the
/// first one-band raster.
fn elevation_layer(app: &RustGisApp) -> Option<String> {
    let map = app.session.map();
    let one_band = |l: &&rustgis_engine::map::Layer| l.raster.as_ref().and_then(|r| r.data.as_ref()).is_some_and(|d| d.band_count() == 1);
    let active = app.session.active_layer.and_then(|id| map.layer(id)).filter(one_band);
    active.or_else(|| map.layers.iter().find(one_band)).map(|l| l.name.clone())
}

/// The clicked place as the commands take it: on a world copy past the antimeridian, the same
/// place within ±180.
fn place(c: Coord) -> Coord {
    Coord::new(wrap_lon(c.x), c.y)
}

/// Runs a menu action; a failure shows in the status bar (the menu closes either way).
fn run(app: &mut RustGisApp, id: &str, params: serde_json::Value) {
    if let Err(e) = app.run(id, params) {
        app.session.notify(e, true);
    }
}

/// The menu for the place `c` that was right-clicked.
pub fn show(app: &mut RustGisApp, ui: &mut Ui, c: Coord) {
    let c = place(c);
    ui.set_min_width(240.0);
    let format = app.map.coord_format;
    let earth = app.session.map().body().is_earth();
    let (coords, fell_back) = coordinate_row(c, format, earth);
    let hover = if fell_back && !earth {
        i18n::fmt(
            "Copy coordinates to the clipboard. {format} is a grid of the Earth, so latitude and longitude are copied",
            &[("format", format.label())],
        )
    } else if fell_back {
        i18n::fmt(
            "Copy coordinates to the clipboard. No {format} here: outside the UTM zones (80° S to 84° N), so latitude and longitude are copied",
            &[("format", format.label())],
        )
    } else {
        tl!("Copy coordinates to the clipboard").to_string()
    };
    if menu_row(ui, "locate", &coords, "", true).on_hover_text(hover).clicked() {
        ui.ctx().copy_text(coords);
        ui.close();
    }
    ui.separator();
    if let Some((layer, row)) = app.map.menu_text {
        if menu_row(ui, "g_text", tl!("Edit Text…"), "", true).clicked() {
            if let Err(e) = crate::graphicformat::edit(app, layer, row) {
                app.session.notify(e, true);
            }
            ui.close();
        }
        ui.separator();
    }
    ui.label(egui::RichText::new(tl!("Quick actions")).small().weak());
    if menu_row(ui, "copy", tl!("Copy As GeoJSON"), "", true).clicked() {
        ui.ctx().copy_text(point_geojson(c));
        ui.close();
    }
    if menu_row(ui, "goto_xy", tl!("Center Map Here"), "", true).clicked() {
        run(app, "map.goToXY", json!({"lon": c.x, "lat": c.y}));
        ui.close();
    }
    if menu_row(ui, "zoom_in", tl!("Zoom In Here"), "", true).clicked() {
        let scale = app.session.map().scale() / 2.0;
        run(app, "map.goToXY", json!({"lon": c.x, "lat": c.y, "scale": scale}));
        ui.close();
    }
    if menu_row(ui, "directions", tl!("Directions To Here"), "", true).clicked() {
        crate::navui::seed(app, None, c);
        ui.close();
    }
    if menu_row(ui, "directions", tl!("Directions From Here"), "", true).clicked() {
        crate::navui::seed(app, Some(0), c);
        ui.close();
    }
    ui.separator();
    submenu_row(ui, "tools", tl!("Quick Analysis"), |ui| quick_analysis(app, ui, c));
    ui.separator();
    let zoom = web_zoom(app.session.map().ground_res(), app.session.map().center_lonlat().y);
    if menu_row(ui, "map", tl!("View In Google Maps"), "", true).clicked() {
        app.ui.open_url = Some(google_maps_url(c, zoom));
        ui.close();
    }
    if menu_row(ui, "globe_view", tl!("View In Google Earth"), "", true).clicked() {
        app.ui.open_url = Some(google_earth_url(c, zoom));
        ui.close();
    }
}

/// Quick Analysis: the Buffer tool at a few distances, a viewshed and a line of sight on an
/// elevation raster in the map, and the Buffer tool's own pane.
fn quick_analysis(app: &mut RustGisApp, ui: &mut Ui, c: Coord) {
    ui.set_min_width(220.0);
    crate::navui::load(app);
    let imperial = app.ui.nav.imperial;
    for (d, units) in buffer_presets(imperial) {
        let dist = distance_label(d, units);
        if menu_row(ui, "buffer", &i18n::fmt("Buffer {distance} Here", &[("distance", &dist)]), "", true).clicked() {
            let name = i18n::fmt("Buffer {distance}", &[("distance", &dist)]);
            run(app, "gp.bufferPoint", json!({"lon": c.x, "lat": c.y, "distance": d, "units": units, "name": name}));
            ui.close();
        }
    }
    ui.separator();
    let dem = elevation_layer(app);
    let why = tl!("Add an elevation raster to the map first");
    for r in VIEWSHED_RADII {
        let radius = radius_label(r, imperial);
        let resp = menu_row(ui, "eye", &i18n::fmt("Viewshed {radius} Here", &[("radius", &radius)]), "", dem.is_some());
        if resp.clicked()
            && let Some(dem) = &dem
        {
            let name = i18n::fmt("Viewshed {radius}", &[("radius", &radius)]);
            // In the background on the desktop, its progress and Cancel in the Workflows pane.
            let p = json!({"layer": dem, "lon": c.x, "lat": c.y, "radius": r, "name": name});
            crate::analysispane::run_elsewhere(app, "visibility", "analysis.viewshed", p);
            ui.close();
        }
        resp.on_disabled_hover_text(why);
    }
    let resp = menu_row(ui, "line_tool", tl!("Line Of Sight From Here"), "", dem.is_some());
    if resp.clicked()
        && let Some(dem) = dem
    {
        // The Line Of Sight workflow with this observer; the next map click is the target.
        run(app, "ui.pane.workflows", json!({"section": "lineOfSight"}));
        app.workflow.layer = dem;
        app.workflow.pick = Some(crate::analysispane::Pick { purpose: "los", needed: 2, points: vec![c] });
        ui.close();
    }
    resp.on_disabled_hover_text(why);
    ui.separator();
    if menu_row(ui, "geoprocessing", tl!("Open In Geoprocessing…"), "", true).clicked() {
        run(app, "ui.gp.open", json!({"tool": "buffer"}));
        ui.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A place on a world copy reaches the commands within ±180 (they refuse it otherwise), and
    /// a failed action shows in the status bar instead of doing nothing.
    #[test]
    fn world_copies_wrap_and_failures_show() {
        let c = place(Coord::new(-83.9207 + 360.0, 35.9606));
        assert!((c.x - -83.9207).abs() < 1e-9 && (c.y - 35.9606).abs() < 1e-9);
        let mut app = RustGisApp::with_sample();
        assert!(app.run("gp.bufferPoint", json!({"lon": c.x, "lat": c.y, "distance": 500.0, "units": "Meters"})).is_ok());
        let before = app.session.notices.len();
        run(&mut app, "map.goToXY", json!({"lon": 500.0, "lat": 0.0}));
        assert!(app.session.notices.get(before..).is_some_and(|n| n.iter().any(|n| n.error)), "the error is shown");
    }

    #[test]
    fn labels_and_links() {
        assert_eq!(distance_label(0.25, "Miles"), "0.25 mi");
        assert_eq!(distance_label(500.0, "Meters"), "500 m");
        assert_eq!(radius_label(15000.0, false), "15 km");
        assert_eq!(radius_label(5000.0, true), "3.1 mi");
        let c = Coord::new(-83.9207, 35.9606);
        assert_eq!(lat_lon(c), "35.960600, -83.920700");
        assert_eq!(
            google_maps_url(Coord::new(c.x + 360.0, c.y), 12.0),
            "https://www.google.com/maps/place/35.960600,-83.920700/@35.960600,-83.920700,12z"
        );
        assert!(google_earth_url(c, 30.0).ends_with(",0a,150d,35y,0h,0t,0r"));
        assert!((web_zoom(156_543.033_92, 0.0)).abs() < 1e-9);
        assert!(point_geojson(c).contains("\"Point\""));
    }

    #[test]
    fn the_coordinate_row_copies_the_chosen_format() {
        let p = Coord::new(44.4, 33.3);
        assert_eq!(coordinate_row(p, CoordReadout::Decimal, true), ("33.300000, 44.400000".into(), false));
        assert_eq!(coordinate_row(p, CoordReadout::Dms, true), ("44°24′00.0″ E  33°18′00.0″ N".into(), false));
        assert_eq!(coordinate_row(p, CoordReadout::Utm, true), ("38north 444141 3684706".into(), false));
        assert_eq!(coordinate_row(p, CoordReadout::Mgrs, true), ("38S MB 44140 84706".into(), false));
        // What is copied as UTM or MGRS is found again by the search box, in both hemispheres.
        for q in [p, Coord::new(-58.4, -34.6), Coord::new(151.2, -33.87), Coord::new(3.0, -1e-7), Coord::new(-21.9, 64.15)] {
            for f in [CoordReadout::Utm, CoordReadout::Mgrs] {
                let (text, fell_back) = coordinate_row(q, f, true);
                assert!(!fell_back, "{text}");
                let at = rustgis_engine::cmd::search::read_grid_reference(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
                assert!((at.x - q.x).abs() < 1e-4 && (at.y - q.y).abs() < 1e-4, "{text} {at:?}");
            }
        }
        // Where UTM has no zone, the row falls back to latitude and longitude (and says so).
        let pole = Coord::new(10.0, 86.0);
        for f in [CoordReadout::Utm, CoordReadout::Mgrs] {
            assert_eq!(coordinate_row(pole, f, true), ("86.000000, 10.000000".into(), true), "{f:?}");
            // On another body's map as well: UTM and MGRS are the Earth's grids.
            assert_eq!(coordinate_row(p, f, false), ("33.300000, 44.400000".into(), true), "{f:?}");
        }
        assert!(!coordinate_row(pole, CoordReadout::Dms, true).1);
        assert_eq!(coordinate_row(p, CoordReadout::Dms, false), coordinate_row(p, CoordReadout::Dms, true));
    }
}
