use rustgis_engine::{
    Project, Session,
    data::{Dataset, Field, FieldType, Geometry, GeometryType, Value},
    geom::Coord,
};
use serde_json::json;

fn session(kind: GeometryType, geometries: Vec<Option<Geometry>>) -> (Session, u32) {
    rustgis_engine::plugins::store::set_dir(Some(std::env::temp_dir().join("rustgis-geometry-calc-test-plugins")));
    let mut s = Session::new(Project::default());
    let mut ds = Dataset::new("test", kind, vec![Field::new("result", FieldType::Double), Field::new("integer", FieldType::Integer)]);
    for g in geometries {
        ds.push(g, vec![Value::Float(99.0), Value::Int(99)]);
    }
    let id = s.map_mut().add_dataset(ds);
    s.active_layer = Some(id);
    (s, id)
}

#[test]
fn selected_units_overwrite_and_undo() {
    let line = |x| Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(x, 0.0)]]));
    let (mut s, id) = session(GeometryType::Polyline, vec![line(1.0), line(2.0), None]);
    s.map_mut().layer_mut(id).unwrap().selection = vec![1];
    let mut p = json!({"layer":id,"field":"result","property":"length","unit":"km","scope":"selected"});
    assert!(s.run("edit.calculateGeometry", &p).is_err());
    assert_eq!(s.map().layer(id).unwrap().dataset.value(1, 0).as_f64(), Some(99.0));
    p["overwrite"] = json!(true);
    assert_eq!(s.run("edit.calculateGeometry", &p).unwrap()["rows"], 1);
    let value = s.map().layer(id).unwrap().dataset.value(1, 0).as_f64().unwrap();
    assert!((222.6..222.7).contains(&value), "{value}"); // the ellipsoid: the sphere gives 222.39
    assert_eq!(s.map().layer(id).unwrap().dataset.value(0, 0).as_f64(), Some(99.0));
    s.run("edit.undo", &json!({})).unwrap();
    assert_eq!(s.map().layer(id).unwrap().dataset.value(1, 0).as_f64(), Some(99.0));
    p["create"] = json!(true);
    p["field"] = json!("metres");
    p["unit"] = json!("m");
    p["scope"] = json!("all");
    assert_eq!(s.run("edit.calculateGeometry", &p).unwrap()["nulls"], 1);
    let ds = &s.map().layer(id).unwrap().dataset;
    let fi = ds.field_index("metres").unwrap();
    assert!((ds.value(1, fi).as_f64().unwrap() / value - 1000.0).abs() < 1e-9);
    assert_eq!(ds.value(2, fi), &Value::Null);
    s.run("edit.undo", &json!({})).unwrap();
    assert!(s.map().layer(id).unwrap().dataset.field_index("metres").is_none());
    s.map_mut().layer_mut(id).unwrap().selection.clear();
    p["scope"] = json!("selected");
    assert!(s.run("edit.calculateGeometry", &p).is_err());
    assert!(s.map().layer(id).unwrap().dataset.field_index("metres").is_none());
}

#[test]
fn areas_are_on_the_wgs84_ellipsoid() {
    // An equator 1° × 1° box measured by GeographicLib itself (pyproj.Geod / Polygon.Compute on
    // WGS 84); the attribute table's Calculate Geometry values are checked against those (#939).
    let ring = vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0), Coord::new(1.0, 1.0), Coord::new(0.0, 1.0)];
    let (mut s, id) = session(GeometryType::Polygon, vec![Some(Geometry::Polygon(vec![vec![ring]]))]);
    s.run("edit.calculateGeometry", &json!({"layer":id,"field":"result","overwrite":true,"property":"area","unit":"m2","scope":"all"}))
        .unwrap();
    let actual = s.map().layer(id).unwrap().dataset.value(0, 0).as_f64().unwrap();
    assert!((actual - 12_308_778_361.469).abs() < 0.001, "{actual}");
    s.run("edit.calculateGeometry", &json!({"layer":id,"field":"result","overwrite":true,"property":"perimeter","unit":"m","scope":"all"}))
        .unwrap();
    let actual = s.map().layer(id).unwrap().dataset.value(0, 0).as_f64().unwrap();
    assert!((actual - 443_770.917).abs() < 0.001, "{actual}");
}

#[test]
fn polygon_holes_multipart_and_area_units() {
    let ring = |x: f64, size: f64| {
        vec![Coord::new(x, 0.0), Coord::new(x + size, 0.0), Coord::new(x + size, size), Coord::new(x, size), Coord::new(x, 0.0)]
    };
    let outer = ring(0.0, 0.01);
    let hole = ring(0.002, 0.002);
    let part = ring(0.02, 0.005);
    let expected = rustgis_engine::geom::wgs84_ring_perimeter_area_m2(&outer).1
        - rustgis_engine::geom::wgs84_ring_perimeter_area_m2(&hole).1
        + rustgis_engine::geom::wgs84_ring_perimeter_area_m2(&part).1;
    let (mut s, id) = session(GeometryType::Polygon, vec![Some(Geometry::Polygon(vec![vec![outer, hole], vec![part]]))]);
    for (unit, divisor) in [("m2", 1.0), ("km2", 1e6), ("ha", 1e4), ("ac", 4046.8564224)] {
        s.run("edit.calculateGeometry", &json!({"layer":id,"field":"result","overwrite":true,"property":"area","unit":unit,"scope":"all"}))
            .unwrap();
        let actual = s.map().layer(id).unwrap().dataset.value(0, 0).as_f64().unwrap();
        assert!((actual - expected / divisor).abs() < 1e-6);
    }
    assert!(
        s.run("edit.calculateGeometry", &json!({"layer":id,"field":"bad","create":true,"property":"length","unit":"m","scope":"all"}))
            .is_err()
    );
    assert!(s.map().layer(id).unwrap().dataset.field_index("bad").is_none());
}

#[test]
fn coordinates_projection_rounding_and_atomic_validation() {
    let (mut s, id) = session(GeometryType::Point, vec![Some(Geometry::Point(Coord::new(117.0, 3.0)))]);
    let mut p = json!({"layer":id,"field":"result","overwrite":true,"property":"x","unit":"deg","scope":"all"});
    s.run("edit.calculateGeometry", &p).unwrap();
    assert_eq!(s.map().layer(id).unwrap().dataset.value(0, 0).as_f64(), Some(117.0));
    p["unit"] = json!("projected");
    p["epsg"] = json!(32650);
    s.run("edit.calculateGeometry", &p).unwrap();
    assert!((s.map().layer(id).unwrap().dataset.value(0, 0).as_f64().unwrap() - 500000.0).abs() < 1.0);
    p["epsg"] = json!(4326);
    assert!(s.run("edit.calculateGeometry", &p).is_err());
    assert!((s.map().layer(id).unwrap().dataset.value(0, 0).as_f64().unwrap() - 500000.0).abs() < 1.0);
    p["unit"] = json!("deg");
    p["property"] = json!("y");
    p["field"] = json!("integer");
    s.run("edit.calculateGeometry", &p).unwrap();
    assert_eq!(s.map().layer(id).unwrap().dataset.value(0, 1), &Value::Int(3));
    s.map_mut().editing.settings.enabled = false;
    assert!(s.run("edit.calculateGeometry", &p).is_err());
}

#[test]
fn longitude_bounds_and_failed_calculation_preserve_data() {
    for x in [-180.0, 180.0] {
        let (mut s, id) = session(GeometryType::Point, vec![Some(Geometry::Point(Coord::new(x, 0.0)))]);
        s.run(
            "edit.calculateGeometry",
            &json!({
                "layer": id, "field": "result", "overwrite": true,
                "property": "x", "unit": "deg", "scope": "all"
            }),
        )
        .unwrap();
        assert_eq!(s.map().layer(id).unwrap().dataset.value(0, 0).as_f64(), Some(x));
    }

    for x in [-180.001, 180.001, -1e300, 1e300] {
        let (mut s, id) =
            session(GeometryType::Point, vec![Some(Geometry::Point(Coord::new(117.0, 3.0))), Some(Geometry::Point(Coord::new(x, 0.0)))]);
        let mut p = json!({
            "layer": id, "field": "result", "overwrite": true,
            "property": "x", "unit": "deg", "scope": "all"
        });

        let error = s.run("edit.calculateGeometry", &p).unwrap_err();
        assert!(error.contains("invalid coordinates"), "{error}");
        for row in 0..2 {
            assert_eq!(s.map().layer(id).unwrap().dataset.value(row, 0).as_f64(), Some(99.0),);
        }

        p["create"] = json!(true);
        p["field"] = json!("new_x");
        assert!(s.run("edit.calculateGeometry", &p).is_err());
        assert!(s.map().layer(id).unwrap().dataset.field_index("new_x").is_none());
    }
}

/// A buffer near the antimeridian runs past 180°: its area is still measured, while a point's
/// coordinate must stay a real longitude.
#[test]
fn polygons_across_the_antimeridian_are_measured() {
    let (mut s, id) = session(GeometryType::Point, vec![Some(Geometry::Point(Coord::new(179.99, 0.0)))]);
    let name = s.map().layer(id).unwrap().name.clone();
    s.run("gp.run", &json!({"tool": "buffer", "params": {"input": name, "distance": 10, "units": "Kilometers"}})).unwrap();
    let buffer = s.map().layers.iter().find(|l| l.geometry_type() == GeometryType::Polygon).map(|l| l.id).unwrap();
    let mut max_x = f64::MIN;
    for f in &s.map().layer(buffer).unwrap().dataset.features {
        f.geometry.as_ref().unwrap().for_each_coord(|c| max_x = max_x.max(c.x));
    }
    assert!(max_x > 180.0, "the buffer crosses the antimeridian: {max_x}");
    s.run(
        "edit.calculateGeometry",
        &json!({"layer": buffer, "field": "area", "create": true, "property": "area", "unit": "km2", "scope": "all"}),
    )
    .unwrap();
    let ds = &s.map().layer(buffer).unwrap().dataset;
    let area = ds.value(0, ds.field_index("area").unwrap()).as_f64().unwrap();
    assert!((300.0..330.0).contains(&area), "about π·10² km²: {area}");

    let ring = vec![Coord::new(179.0, 0.0), Coord::new(181.0, 0.0), Coord::new(181.0, 1.0), Coord::new(179.0, 1.0), Coord::new(179.0, 0.0)];
    let (mut s, id) = session(GeometryType::Polygon, vec![Some(Geometry::Polygon(vec![vec![ring]]))]);
    for property in ["area", "perimeter"] {
        let unit = if property == "area" { "km2" } else { "km" };
        let p = json!({"layer": id, "field": "result", "overwrite": true, "property": property, "unit": unit, "scope": "all"});
        s.run("edit.calculateGeometry", &p).unwrap();
        assert!(s.map().layer(id).unwrap().dataset.value(0, 0).as_f64().unwrap() > 0.0);
    }
}

#[test]
fn area_in_square_feet_is_43560_per_acre() {
    let ring = vec![Coord::new(0.0, 0.0), Coord::new(0.001, 0.0), Coord::new(0.001, 0.001), Coord::new(0.0, 0.001), Coord::new(0.0, 0.0)];
    let (mut s, id) = session(GeometryType::Polygon, vec![Some(Geometry::Polygon(vec![vec![ring]]))]);
    let mut p = json!({"layer": id, "field": "result", "property": "area", "unit": "ac", "scope": "all", "overwrite": true});
    s.run("edit.calculateGeometry", &p).unwrap();
    let acres = s.map().layer(id).unwrap().dataset.value(0, 0).as_f64().unwrap();
    p["unit"] = json!("ft2");
    s.run("edit.calculateGeometry", &p).unwrap();
    let feet = s.map().layer(id).unwrap().dataset.value(0, 0).as_f64().unwrap();
    assert!((feet / acres - 43_560.0).abs() < 1e-8);
    s.run("edit.undo", &json!({})).unwrap();
    assert_eq!(s.map().layer(id).unwrap().dataset.value(0, 0).as_f64(), Some(acres));
}

#[test]
fn area_in_square_miles_is_one_per_640_acres() {
    let ring = vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0), Coord::new(1.0, 1.0), Coord::new(0.0, 1.0), Coord::new(0.0, 0.0)];
    let (mut s, id) = session(GeometryType::Polygon, vec![Some(Geometry::Polygon(vec![vec![ring]]))]);
    let mut p = json!({"layer": id, "field": "result", "property": "area", "unit": "ac", "scope": "all", "overwrite": true});
    s.run("edit.calculateGeometry", &p).unwrap();
    let acres = s.map().layer(id).unwrap().dataset.value(0, 0).as_f64().unwrap();
    p["unit"] = json!("mi2");
    s.run("edit.calculateGeometry", &p).unwrap();
    let miles = s.map().layer(id).unwrap().dataset.value(0, 0).as_f64().unwrap();
    assert!((acres / miles - 640.0).abs() < 1e-10);
}

/// The rejected unit lists only the requested property's units, including coordinate choices.
#[test]
fn invalid_unit_lists_the_property_units() {
    for (kind, property, invalid, valid) in [
        (GeometryType::Polygon, "area", "sqft", "m2, km2, ha, ac, ft2, mi2"),
        (GeometryType::Polygon, "perimeter", "m2", "m, km, ft, mi"),
        (GeometryType::Polyline, "length", "m2", "m, km, ft, mi"),
        (GeometryType::Point, "x", "m", "deg, projected"),
    ] {
        let (mut s, id) = session(kind, vec![None]);
        let error = s
            .run(
                "edit.calculateGeometry",
                &json!({
                    "layer": id, "field": "result", "overwrite": true,
                    "property": property, "unit": invalid, "scope": "all"
                }),
            )
            .unwrap_err();
        assert_eq!(error, format!("unit does not match the property; valid units: {valid}"));
        assert_eq!(s.map().layer(id).unwrap().dataset.value(0, 0), &Value::Float(99.0));
    }
}
