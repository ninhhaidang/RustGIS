//! Units are interpreted at the command boundary; stored geometry stays in lon/lat.
use rustgis_engine::{
    Project, Session,
    data::Geometry,
    geom::{Coord, lonlat_to_merc},
};
use serde_json::json;

fn paths() -> Session {
    let mut s = Session::new(Project::default());
    s.run("layer.template", &json!({"template": "lines", "name": "Paths"})).unwrap();
    s.run("edit.addFeature", &json!({"geometry": {"type": "LineString", "coordinates": [[0, 0], [0, 0.001]]}})).unwrap();
    s.run("select.rows", &json!({"layer": "Paths", "rows": [0]})).unwrap();
    s
}

fn first(s: &Session) -> Coord {
    let Geometry::Line(parts) = s.map().layer_by_name("Paths").unwrap().dataset.features[0].geometry.as_ref().unwrap() else {
        panic!("line")
    };
    parts[0][0]
}

#[test]
fn move_feet_converts_before_ground_to_grid_and_undo_restores_geometry() {
    let mut s = paths();
    s.run("edit.groundToGrid", &json!({"scale": 1.5, "rotation": 0})).unwrap();
    s.run("edit.move", &json!({"dx": 500, "dy": -100, "units": "Feet"})).unwrap();
    let p = lonlat_to_merc(first(&s));
    assert!((p.x - 228.6).abs() < 1e-6 && (p.y + 45.72).abs() < 1e-6, "{p:?}");
    s.run("edit.undo", &json!({})).unwrap();
    assert_eq!(first(&s), Coord::new(0.0, 0.0));
    // Existing scripts keep metres when no unit is supplied.
    s.run("edit.groundToGrid", &json!({"on": false})).unwrap();
    s.run("edit.move", &json!({"dx": 500})).unwrap();
    assert!((lonlat_to_merc(first(&s)).x - 500.0).abs() < 1e-6);
}

#[test]
fn traverse_feet_and_metres_draw_the_same_line() {
    let mut feet = paths();
    let mut metres = paths();
    feet.run("edit.groundToGrid", &json!({"scale": 1.5, "rotation": 20})).unwrap();
    metres.run("edit.groundToGrid", &json!({"scale": 1.5, "rotation": 20})).unwrap();
    feet.run(
        "edit.traverse",
        &json!({"start": [-84, 36], "units": "feet", "legs": [{"distance": 500, "bearing": 90}, {"distance": 300, "bearing": 0}]}),
    )
    .unwrap();
    metres
        .run("edit.traverse", &json!({"start": [-84, 36], "legs": [{"distance": 152.4, "bearing": 90}, {"distance": 91.44, "bearing": 0}]}))
        .unwrap();
    let geometry = |s: &Session| s.map().layer_by_name("Paths").unwrap().dataset.features.last().unwrap().geometry.clone();
    assert_eq!(geometry(&feet), geometry(&metres));
    feet.run("edit.undo", &json!({})).unwrap();
    assert_eq!(feet.map().layer_by_name("Paths").unwrap().dataset.features.len(), 1);
}

#[test]
fn constant_elevation_feet_are_stored_in_metres() {
    let mut s = paths();
    s.run("edit.elevation", &json!({"mode": "constant", "z": 1000, "units": "Feet"})).unwrap();
    assert_eq!(s.map().editing.settings.elevation, rustgis_engine::map::design::Elevation::Constant(304.8));
    s.run("edit.elevation", &json!({"mode": "constant", "z": 1000})).unwrap();
    assert_eq!(s.map().editing.settings.elevation, rustgis_engine::map::design::Elevation::Constant(1000.0));
}

#[test]
fn select_by_location_feet_selects_only_features_within_the_radius() {
    use rustgis_engine::data::{Dataset, GeometryType};
    let mut s = paths();
    let mut points = Dataset::new("Parcels", GeometryType::Point, vec![]);
    // About 111 m and 222 m from the origin: only the first is within 500 ft.
    points.push(Some(Geometry::Point(Coord::new(0.001, 0.0))), vec![]);
    points.push(Some(Geometry::Point(Coord::new(0.002, 0.0))), vec![]);
    let id = s.map_mut().add_dataset(points);
    let mut center = Dataset::new("Site", GeometryType::Point, vec![]);
    center.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
    s.map_mut().add_dataset(center);
    let mut p = json!({"tool": "selectByLocation", "params": {"input": "Parcels", "select": "Site", "distance": 500, "units": "Feet"}});
    s.run("gp.run", &p).unwrap();
    assert_eq!(s.map().layer(id).unwrap().selection, vec![0]);
    p["params"]["distance"] = json!(0.1524);
    p["params"].as_object_mut().unwrap().remove("units");
    s.run("gp.run", &p).unwrap();
    assert_eq!(s.map().layer(id).unwrap().selection, vec![0], "omitted units still mean kilometres");
}

#[test]
fn invalid_editing_units_and_scaled_overflow_leave_the_project_unchanged() {
    for (command, params, overflow) in [
        ("edit.move", json!({"dx": 1, "dy": 1}), json!({"dx": f64::MAX, "units": "Miles"})),
        (
            "edit.traverse",
            json!({"start": [0, 0], "legs": [{"distance": 1, "bearing": 90}]}),
            json!({"start": [0, 0], "legs": [{"distance": f64::MAX, "bearing": 90}], "units": "Miles"}),
        ),
        ("edit.elevation", json!({"mode": "constant", "z": 1}), json!({"mode": "constant", "z": f64::MAX, "units": "Miles"})),
    ] {
        let mut s = paths();
        let before = serde_json::to_value(&s.project).unwrap();
        let undo = s.undo_label();
        let unchanged = |s: &Session| {
            assert_eq!(serde_json::to_value(&s.project).unwrap(), before, "{command} changed the project after an error");
            assert_eq!(s.undo_label(), undo, "{command} changed undo history after an error");
        };
        for units in [json!(7), json!(true), json!(["Feet"]), json!({"name": "Feet"}), json!("parsecs")] {
            let mut p = params.clone();
            p["units"] = units;
            let error = s.run(command, &p).unwrap_err();
            assert!(error.contains("units"), "{command}: {error}");
            unchanged(&s);
        }
        let error = s.run(command, &overflow).unwrap_err();
        assert!(error.contains("large") || error.contains("distance"), "{command}: {error}");
        unchanged(&s);
        assert!(s.run("info.project", &json!({})).is_ok(), "{command} left an unusable session");
    }
}
