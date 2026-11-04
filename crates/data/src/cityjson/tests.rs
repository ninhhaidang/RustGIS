use serde_json::json;

use super::*;

/// A document in longitude/latitude (unit transform), with the given vertices and objects.
fn doc(vertices: Json, objects: Json) -> Vec<u8> {
    json!({
        "type": "CityJSON",
        "version": "2.0",
        "transform": {"scale": [1.0, 1.0, 1.0], "translate": [0.0, 0.0, 0.0]},
        "metadata": {"referenceSystem": "https://www.opengis.net/def/crs/EPSG/0/4326"},
        "vertices": vertices,
        "CityObjects": objects,
    })
    .to_string()
    .into_bytes()
}

/// A 10-wide square at height `z`, as vertices.
fn square(z: i64) -> Vec<Json> {
    [[0, 0], [10, 0], [10, 10], [0, 10]].iter().map(|[x, y]| json!([x, y, z])).collect()
}

fn box_vertices() -> Json {
    Json::Array([square(0), square(5)].concat())
}

/// A box's shell: floor, roof and a wall, as a solid's boundaries.
fn box_shell() -> Json {
    json!([[[[0, 3, 2, 1]], [[4, 5, 6, 7]], [[0, 1, 5, 4]]]])
}

fn messages_of(out: &[(Dataset, ReadReport)]) -> Vec<String> {
    out.iter().flat_map(|(_, r)| r.messages.clone()).collect()
}

fn says(out: &[(Dataset, ReadReport)], text: &str) -> bool {
    messages_of(out).iter().any(|m| m.contains(text))
}

fn value(ds: &Dataset, row: usize, field: &str) -> crate::Value {
    let at = ds.field_index(field).unwrap();
    ds.features.get(row).unwrap().attrs[at].clone()
}

fn area(g: Option<&Geometry>) -> f64 {
    let Some(Geometry::Polygon(polys)) = g else { return 0.0 };
    polys.iter().filter_map(|p| p.first()).map(|r| crate::geometry::ring_area(r).abs()).sum()
}

fn geometry(ds: &Dataset, row: usize) -> Option<&Geometry> {
    ds.features.get(row).unwrap().geometry.as_ref()
}

#[test]
fn it_is_known_by_its_root_type_wherever_that_comes() {
    let late = format!(r#"{{"CityObjects": {{}}, "vertices": [], "padding": "{}", "type": "CityJSON"}}"#, "x".repeat(100_000));
    assert!(looks_like_cityjson(&late), "the root type past any prefix");
    assert!(looks_like_cityjson(r#"{"type": "CityJSON", "CityObjects": {}}"#));
    // A GeoJSON whose property says so, an array, and a document without its objects aren't.
    assert!(!looks_like_cityjson(r#"{"type": "FeatureCollection", "x": {"type": "CityJSON", "CityObjects": {}}, "features": []}"#));
    assert!(!looks_like_cityjson(r#"["CityJSON", "CityObjects"]"#));
    assert!(!looks_like_cityjson(r#"{"type": "CityJSON"}"#));
    assert!(!looks_like_cityjson(r#"{"type": "CityJSON", "CityObjects": "#), "broken");
    assert!(is_cityjson_name("Delft.CITY.json") && !is_cityjson_name("delft.json"));
    let bytes = doc(json!([[0, 0, 0]]), json!({"a": {"type": "Road", "geometry": [{"type": "MultiPoint", "boundaries": [0]}]}}));
    assert_eq!(read("Delft.CITY.json", &bytes, None).unwrap()[0].0.name, "Delft", "named without its suffix");
}

/// A solid is its footprint with its heights: the notice says so, and the walls show nothing.
#[test]
fn a_solid_is_its_footprint_with_its_heights() {
    let bytes = doc(
        box_vertices(),
        json!({"b1": {"type": "Building", "attributes": {"name": "Town hall", "floors": 2},
            "geometry": [{"type": "Solid", "lod": "2", "boundaries": box_shell()}]}}),
    );
    let out = read("model.city.json", &bytes, None).unwrap();
    assert_eq!(out.len(), 1);
    let (ds, _) = &out[0];
    assert_eq!(ds.name, "model", "the only layer takes the file's name");
    assert_eq!(ds.geometry_type, GeometryType::Polygon);
    let names: Vec<&str> = ds.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["id", "type", "parent", "lod", "floors", "name", "z_min", "z_max", "height"]);
    assert_eq!(value(ds, 0, "id"), crate::Value::Text("b1".into()));
    assert_eq!(value(ds, 0, "lod"), crate::Value::Text("2".into()));
    assert_eq!(value(ds, 0, "floors"), crate::Value::Int(2));
    assert_eq!(value(ds, 0, "z_min"), crate::Value::Float(0.0));
    assert_eq!(value(ds, 0, "height"), crate::Value::Float(5.0));
    assert!((area(geometry(ds, 0)) - 100.0).abs() < 1e-6, "{:?}", geometry(ds, 0));
    let said = messages_of(&out);
    assert_eq!(said.len(), 1, "a clean solid says only that: {said:?}");
    assert!(said[0].starts_with("1 3D city objects are drawn as their footprints"), "{said:?}");
}

/// A clean 2D model says nothing.
#[test]
fn a_flat_model_gives_no_notice() {
    let bytes = doc(
        Json::Array(square(0)),
        json!({"park": {"type": "LandUse", "geometry": [{"type": "MultiSurface", "lod": "1", "boundaries": [[[0, 1, 2, 3]]]}]}}),
    );
    let out = read("flat.city.json", &bytes, None).unwrap();
    assert_eq!(messages_of(&out), Vec::<String>::new());
    assert_eq!(out[0].1.skipped, 0);
    assert_eq!(value(&out[0].0, 0, "height"), crate::Value::Float(0.0));
}

/// LoD 0 comes first, then `GroundSurface`, then the union of the highest LoD.
#[test]
fn a_footprint_comes_from_lod_0_then_the_ground_then_the_highest_lod() {
    // A big roof over a small ground, and a smaller LoD 0.
    let vertices = json!([
        [0, 0, 5],
        [20, 0, 5],
        [20, 20, 5],
        [0, 20, 5],
        [5, 5, 0],
        [15, 5, 0],
        [15, 15, 0],
        [5, 15, 0],
        [8, 8, 0],
        [12, 8, 0],
        [12, 12, 0],
        [8, 12, 0],
    ]);
    let roof_and_ground = json!({"type": "MultiSurface", "lod": "2",
        "boundaries": [[[0, 1, 2, 3]], [[4, 5, 6, 7]]],
        "semantics": {"surfaces": [{"type": "RoofSurface"}, {"type": "GroundSurface"}], "values": [0, 1]}});
    let unmarked = json!({"type": "MultiSurface", "lod": "1", "boundaries": [[[0, 1, 2, 3]], [[4, 5, 6, 7]]]});
    let lod0 = json!({"type": "MultiSurface", "lod": "0", "boundaries": [[[8, 9, 10, 11]]]});
    let bytes = doc(
        vertices,
        json!({
            "a": {"type": "Building", "geometry": [roof_and_ground, lod0]},
            "b": {"type": "Building", "geometry": [roof_and_ground]},
            "c": {"type": "Building", "geometry": [unmarked]},
        }),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    let ds = &out[0].0;
    assert!((area(geometry(ds, 0)) - 16.0).abs() < 1e-6, "LoD 0: {:?}", geometry(ds, 0));
    assert_eq!(value(ds, 0, "lod"), crate::Value::Text("0".into()));
    assert!((area(geometry(ds, 1)) - 100.0).abs() < 1e-6, "the ground: {:?}", geometry(ds, 1));
    assert_eq!(value(ds, 1, "lod"), crate::Value::Text("2".into()));
    assert!((area(geometry(ds, 2)) - 400.0).abs() < 1e-6, "the union: {:?}", geometry(ds, 2));
    // The flat outline's heights still reach its other geometry's.
    assert_eq!(value(ds, 0, "z_max"), crate::Value::Float(5.0));
}

/// A building's solids in its parts: it stands as tall as they do, its own geometry a flat outline
/// or nothing, and a part's `parent` names it. One with nothing is kept, and the notice says so.
#[test]
fn a_parents_heights_take_in_its_childrens() {
    let flat = json!({"type": "MultiSurface", "lod": "0", "boundaries": [[[0, 3, 2, 1]]]});
    let bytes = doc(
        box_vertices(),
        json!({
            "outlined": {"type": "Building", "children": ["p1"], "geometry": [flat]},
            "empty": {"type": "Building", "children": ["p2"], "attributes": {"name": "kept"}},
            "p1": {"type": "BuildingPart", "parents": ["outlined"], "geometry": [{"type": "Solid", "lod": "2", "boundaries": box_shell()}]},
            "p2": {"type": "BuildingPart", "parents": ["empty"], "geometry": [{"type": "Solid", "lod": "2", "boundaries": box_shell()}]},
        }),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    assert_eq!(out.iter().map(|(d, _)| d.name.as_str()).collect::<Vec<_>>(), ["m Building", "m BuildingPart"]);
    let (buildings, parts) = (&out[0].0, &out[1].0);
    // In the order of their ids.
    assert_eq!(value(buildings, 0, "id"), crate::Value::Text("empty".into()));
    assert!(geometry(buildings, 0).is_none());
    assert_eq!(value(buildings, 0, "name"), crate::Value::Text("kept".into()));
    assert_eq!(value(buildings, 0, "height"), crate::Value::Float(5.0));
    assert!(geometry(buildings, 1).is_some());
    assert_eq!(value(buildings, 1, "height"), crate::Value::Float(5.0), "from its part");
    assert_eq!(value(parts, 0, "parent"), crate::Value::Text("outlined".into()));
    assert!(says(&out, "1 city objects have no geometry of their own"), "{:?}", messages_of(&out));
}

/// A cycle of parents ends.
#[test]
fn a_cycle_of_parents_ends() {
    let solid = json!([{"type": "Solid", "lod": "2", "boundaries": box_shell()}]);
    let bytes = doc(
        box_vertices(),
        json!({"a": {"type": "Building", "parents": ["b"], "geometry": solid}, "b": {"type": "Building", "parents": ["a"], "geometry": solid}}),
    );
    assert_eq!(read("m.city.json", &bytes, None).unwrap()[0].0.features.len(), 2);
}

/// Layers with something to draw come first: a file named `.json` adds only the first, and a model
/// whose buildings keep their solids in their parts must not open as an empty table.
#[test]
fn a_layer_with_something_to_draw_comes_first() {
    let bytes = doc(
        box_vertices(),
        json!({
            "b": {"type": "Building", "children": ["p"]},
            "p": {"type": "BuildingPart", "parents": ["b"], "geometry": [{"type": "Solid", "lod": "2", "boundaries": box_shell()}]},
        }),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    assert_eq!(out.iter().map(|(d, _)| d.name.as_str()).collect::<Vec<_>>(), ["m BuildingPart", "m Building"]);
    assert_eq!(out[1].0.geometry_type, GeometryType::None);
    let (ds, report) = crate::io::read_bytes("m.json", &bytes).unwrap();
    assert_eq!(ds.name, "m BuildingPart");
    assert!(report.messages.iter().any(|m| m.contains("it also holds m Building")), "{report:?}");
}

/// Each family of a type has its layer, and `+GenericCityObject` is `GenericCityObject`.
#[test]
fn each_type_and_family_has_its_layer() {
    let bytes = doc(
        box_vertices(),
        json!({
            "a": {"type": "Road", "geometry": [
                {"type": "MultiLineString", "lod": "0", "boundaries": [[0, 1, 2]]},
                {"type": "MultiSurface", "lod": "1", "boundaries": [[[0, 1, 2, 3]]]},
            ]},
            "b": {"type": "+GenericCityObject", "geometry": [{"type": "MultiPoint", "lod": "1", "boundaries": [0, 1]}]},
            "c": {"type": "GenericCityObject", "geometry": [{"type": "MultiPoint", "lod": "1", "boundaries": [2]}]},
        }),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    assert_eq!(out.iter().map(|(d, _)| d.name.as_str()).collect::<Vec<_>>(), ["m GenericCityObject", "m Road", "m Road lines"]);
    assert_eq!(out[0].0.geometry_type, GeometryType::Point);
    assert_eq!(out[0].0.features.len(), 2);
    assert!(matches!(geometry(&out[0].0, 0), Some(Geometry::MultiPoint(p)) if p.len() == 2));
    assert!(matches!(geometry(&out[0].0, 1), Some(Geometry::Point(_))));
    assert_eq!(out[2].0.geometry_type, GeometryType::Polyline);
}

/// Instances are points at their reference point, and `TINRelief`s aren't drawn: both are said.
#[test]
fn instances_and_tin_reliefs_are_said() {
    let bytes = doc(
        box_vertices(),
        json!({
            "tree": {"type": "SolitaryVegetationObject", "geometry": [
                {"type": "GeometryInstance", "template": 0, "boundaries": [4], "transformationMatrix": []}]},
            "ground": {"type": "TINRelief", "geometry": [{"type": "CompositeSurface", "lod": "1", "boundaries": [[[0, 1, 2]]]}]},
        }),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    assert_eq!(out.len(), 1);
    assert!(matches!(geometry(&out[0].0, 0), Some(Geometry::Point(_))));
    assert!(says(&out, "1 geometry instances are drawn as a point"), "{:?}", messages_of(&out));
    // Its heights are in its template, which isn't read: unknown, not none.
    assert_eq!(value(&out[0].0, 0, "height"), crate::Value::Null);
    assert!(says(&out, "1 TINRelief objects aren't drawn"), "{:?}", messages_of(&out));
    // Nothing but those: an error, which says why.
    let only = doc(box_vertices(), json!({"g": {"type": "TINRelief", "geometry": []}}));
    assert!(read("m.city.json", &only, None).unwrap_err().to_string().contains("TINRelief"));
}

/// An attribute named as one of the reader's fields keeps its value, under a name that says so.
#[test]
fn an_attribute_named_as_a_field_is_renamed_and_said() {
    let bytes = doc(
        box_vertices(),
        json!({"b": {"type": "Building", "attributes": {"height": 99, "Type": "x"},
            "geometry": [{"type": "Solid", "lod": "2", "boundaries": box_shell()}]}}),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    let ds = &out[0].0;
    assert_eq!(value(ds, 0, "height_1"), crate::Value::Int(99));
    assert_eq!(value(ds, 0, "height"), crate::Value::Float(5.0));
    assert_eq!(value(ds, 0, "Type_1"), crate::Value::Text("x".into()));
    assert!(says(&out, "attributes renamed with a number, as these fields are the reader's own: Type, height"), "{:?}", messages_of(&out));
    // Renamed, it keeps clear of the object's other attributes.
    let bytes = doc(
        box_vertices(),
        json!({"b": {"type": "Building", "attributes": {"height": 99, "height_1": 5},
            "geometry": [{"type": "Solid", "lod": "2", "boundaries": box_shell()}]}}),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    assert_eq!(value(&out[0].0, 0, "height_1"), crate::Value::Int(5));
    assert_eq!(value(&out[0].0, 0, "height_2"), crate::Value::Int(99));
}

/// The compound systems city models name read by their horizontal part: The Hague's RD New.
#[test]
fn a_compound_system_is_read_by_its_horizontal_part() {
    let mut v: Json = serde_json::from_slice(&doc(
        json!([[85000, 447000, 0], [85100, 447000, 0], [85100, 447100, 0]]),
        json!({"a": {"type": "LandUse", "geometry": [{"type": "MultiSurface", "lod": "1", "boundaries": [[[0, 1, 2]]]}]}}),
    ))
    .unwrap();
    for urn in ["https://www.opengis.net/def/crs/EPSG/0/7415", "urn:ogc:def:crs:EPSG::7415"] {
        v["metadata"]["referenceSystem"] = json!(urn);
        let out = read("m.city.json", v.to_string().as_bytes(), None).unwrap();
        let ds = &out[0].0;
        let Some(Geometry::Polygon(p)) = geometry(ds, 0) else { panic!("{:?}", geometry(ds, 0)) };
        let c = p[0][0][0];
        assert!((c.x - 4.32).abs() < 0.05 && (c.y - 52.0).abs() < 0.05, "{urn}: {c:?}");
        assert_eq!(ds.crs, Crs::from_epsg(28992));
    }
    assert_eq!(horizontal_epsg(2056), 2056, "a plain system is itself");
}

/// With no system named: longitude/latitude if it can be (and said), else an error, unless given.
#[test]
fn a_file_with_no_system_is_not_placed_by_a_guess() {
    let object = json!({"a": {"type": "LandUse", "geometry": [{"type": "MultiSurface", "lod": "1", "boundaries": [[[0, 1, 2]]]}]}});
    let mut v: Json = serde_json::from_slice(&doc(json!([[1, 1, 0], [2, 1, 0], [2, 2, 0]]), object)).unwrap();
    v.as_object_mut().unwrap().remove("metadata");
    let out = read("m.city.json", v.to_string().as_bytes(), None).unwrap();
    assert!(says(&out, "names no coordinate system"), "{:?}", messages_of(&out));

    v["vertices"] = json!([[85000, 447000, 0], [85100, 447000, 0], [85100, 447100, 0]]);
    let e = read("m.city.json", v.to_string().as_bytes(), None).unwrap_err().to_string();
    assert!(e.contains("names no coordinate system") && e.contains("crs"), "{e}");
    let out = read("m.city.json", v.to_string().as_bytes(), Crs::from_epsg(28992)).unwrap();
    assert_eq!(out[0].0.crs, Crs::from_epsg(28992));
    assert_eq!(messages_of(&out), Vec::<String>::new(), "given, so nothing was assumed");

    v["metadata"] = json!({"referenceSystem": "https://www.opengis.net/def/crs/EPSG/0/99999"});
    let e = read("m.city.json", v.to_string().as_bytes(), None).unwrap_err().to_string();
    assert!(e.contains("99999"), "{e}");
}

#[test]
fn other_versions_and_broken_documents_are_refused() {
    let mut v: Json = serde_json::from_slice(&doc(json!([]), json!({}))).unwrap();
    v["version"] = json!("1.0");
    let e = read("m.city.json", v.to_string().as_bytes(), None).unwrap_err().to_string();
    assert!(e.contains("1.0") && e.contains("cjio"), "{e}");
    for fine in ["1.1", "1.1.3", "2.0", "2.0.1"] {
        v["version"] = json!(fine);
        // Past the version, to its having nothing to draw.
        assert!(read("m.city.json", v.to_string().as_bytes(), None).unwrap_err().to_string().contains("nothing to draw"), "{fine}");
    }
    v["version"] = json!("2.0");
    v.as_object_mut().unwrap().remove("transform");
    assert!(read("m.city.json", v.to_string().as_bytes(), None).unwrap_err().to_string().contains("transform"));
    assert!(read("m.city.json", br#"{"type": "FeatureCollection"}"#, None).is_err());
    assert!(read("m.city.json", b"not json", None).is_err());
    assert!(read("m.city.json", &[0xFF, 0xFE], None).is_err());
}

/// Real files hold rings such as `[6038, 6038, 6038]`: left out and counted, and the object drawn.
#[test]
fn a_ring_with_no_area_is_left_out_and_counted() {
    let bytes = doc(
        box_vertices(),
        json!({"b": {"type": "Building", "geometry": [{"type": "Solid", "lod": "2", "boundaries": [[[[0, 3, 2, 1]], [[4, 4, 4]], [[0, 1, 0]]]]}]}}),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    assert!((area(geometry(&out[0].0, 0)) - 100.0).abs() < 1e-6);
    assert!(says(&out, "2 rings with fewer than three different points"), "{:?}", messages_of(&out));
}

/// A hole of a face is kept, and a lone face from LoD 0 is the footprint as it is, in the browser too.
#[test]
fn a_lone_lod_0_face_keeps_its_hole() {
    let bytes = doc(
        json!([[0, 0, 0], [10, 0, 0], [10, 10, 0], [0, 10, 0], [3, 3, 0], [7, 3, 0], [7, 7, 0], [3, 7, 0]]),
        json!({"b": {"type": "Building", "geometry": [{"type": "MultiSurface", "lod": "0", "boundaries": [[[0, 1, 2, 3], [4, 7, 6, 5]]]}]}}),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    let Some(Geometry::Polygon(p)) = geometry(&out[0].0, 0) else { panic!() };
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].len(), 2);
    assert_eq!(p[0][0].first(), p[0][0].last(), "closed");
}

/// Walls alone, or one wall in a straight line.
#[test]
fn walls_are_an_outline_or_said_to_have_none() {
    let walls = json!([[[[0, 1, 5, 4]], [[1, 2, 6, 5]]]]);
    let wall = json!([[[[0, 1, 5, 4]]]]);
    let bytes = doc(
        box_vertices(),
        json!({
            "l": {"type": "Building", "geometry": [{"type": "Solid", "lod": "1", "boundaries": walls}]},
            "s": {"type": "Building", "geometry": [{"type": "Solid", "lod": "1", "boundaries": wall}]},
        }),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    assert!((area(geometry(&out[0].0, 0)) - 50.0).abs() < 1e-6, "{:?}", geometry(&out[0].0, 0));
    assert!(geometry(&out[0].0, 1).is_none());
    assert!(says(&out, "1 city objects with no faces seen from above"), "{:?}", messages_of(&out));
    assert!(says(&out, "1 city objects have a shape that can't be drawn"), "{:?}", messages_of(&out));
}

/// Hostile geometry never panics: what can't be read is left out and counted, the rest stays.
#[test]
fn hostile_geometry_is_left_out_and_counted() {
    let good = json!({"type": "MultiSurface", "lod": "1", "boundaries": [[[0, 1, 2, 3]]]});
    for bad in [
        json!({"type": "Solid", "lod": "1", "boundaries": [[[[0, 1, 99999]]]]}),
        json!({"type": "Solid", "lod": "1", "boundaries": [[[[-1, 1, 2]]]]}),
        json!({"type": "Solid", "lod": "1", "boundaries": [[[0, 1, 2]]]}),
        json!({"type": "Solid", "lod": "1", "boundaries": 7}),
        json!({"type": "Solid", "lod": "1"}),
        json!({"type": "MultiSurface", "boundaries": [[["a"]]]}),
        json!({"type": "MultiPoint", "boundaries": [99999]}),
        json!({"type": "MultiLineString", "boundaries": [[99999]]}),
        json!({"type": "GeometryInstance", "boundaries": []}),
    ] {
        let bytes =
            doc(box_vertices(), json!({"bad": {"type": "Building", "geometry": [bad]}, "good": {"type": "Building", "geometry": [good]}}));
        let out = read("m.city.json", &bytes, None).unwrap_or_else(|e| panic!("{bad}: {e}"));
        assert!(says(&out, "1 geometries of city objects have vertex indexes"), "{bad}: {:?}", messages_of(&out));
        assert!(geometry(&out[0].0, 1).is_some(), "{bad}: the good one stays");
    }
    // Objects that aren't objects, an unknown geometry type, odd semantics and attributes.
    let bytes = doc(
        box_vertices(),
        json!({
            "n": 7,
            "t": {"geometry": []},
            "u": {"type": "Building", "geometry": [{"type": "Blob", "boundaries": []}]},
            "s": {"type": "Building", "attributes": 3, "geometry": [{"type": "MultiSurface", "lod": [], "boundaries": [[[0, 1, 2, 3]]],
                "semantics": {"surfaces": 9, "values": "x"}}]},
            "m": {"type": "Building", "geometry": [{"type": "MultiSurface", "lod": "2", "boundaries": [[[0, 1, 2, 3]]],
                "semantics": {"surfaces": [], "values": [99]}}]},
        }),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    assert_eq!(out[0].1.skipped, 2, "{:?}", out[0].1);
    assert!(says(&out, "2 city objects couldn't be read"), "{:?}", messages_of(&out));
    assert!(says(&out, "1 city objects have attributes that aren't names and values"), "{:?}", messages_of(&out));
    assert!(says(&out, "geometries of unknown types are left out: Blob"), "{:?}", messages_of(&out));
    // Vertices that aren't three numbers, or a number past what a float holds: the file is broken.
    let e = read("m.city.json", &doc(json!([[0, 0]]), json!({})), None).unwrap_err().to_string();
    assert!(e.contains("invalid length"), "{e}");
    let far = br#"{"type":"CityJSON","version":"2.0","transform":{"scale":[1,1,1],"translate":[0,0,0]},"vertices":[[1e999,0,0]],"CityObjects":{}}"#;
    let e = read("m.city.json", far, None).unwrap_err().to_string();
    assert!(e.contains("out of range"), "{e}");
}

/// A transform that overflows: a height makes the object its hull, an X leaves it no footprint, and
/// both are said.
#[test]
fn coordinates_a_transform_overflows_are_said() {
    let object = json!({"b": {"type": "Building", "geometry": [{"type": "Solid", "lod": "2", "boundaries": box_shell()}]}});
    let mut v: Json = serde_json::from_slice(&doc(box_vertices(), object)).unwrap();
    v["transform"]["scale"] = json!([1.0, 1.0, 1e308]);
    v["vertices"] = json!([[0, 0, 0], [10, 0, 0], [10, 10, 0], [0, 10, 0], [0, 0, 100], [10, 0, 100], [10, 10, 100], [0, 10, 100]]);
    let out = read("m.city.json", v.to_string().as_bytes(), None).unwrap();
    assert!(geometry(&out[0].0, 0).is_some());
    assert!(says(&out, "whose faces couldn't be merged") || cfg!(target_arch = "wasm32"), "{:?}", messages_of(&out));
    v["transform"]["scale"] = json!([1e308, 1.0, 1.0]);
    let out = read("m.city.json", v.to_string().as_bytes(), Crs::from_epsg(4326)).unwrap();
    assert!(geometry(&out[0].0, 0).is_none());
    assert!(says(&out, "have a shape that can't be drawn"), "{:?}", messages_of(&out));
}

/// Geometries of the same level of detail are one footprint; a level with nothing but degenerate rings
/// gives way to one below; a number is as good a level as text.
#[test]
fn a_footprint_takes_every_geometry_of_its_level() {
    let vertices = json!([[0, 0, 0], [10, 0, 0], [10, 10, 0], [0, 10, 0], [20, 0, 0], [30, 0, 0], [30, 10, 0], [20, 10, 0],]);
    let surface = |lod: Json, ring: Json| json!({"type": "MultiSurface", "lod": lod, "boundaries": [[ring]]});
    let bytes = doc(
        vertices,
        json!({
            "a": {"type": "Building", "geometry": [surface(json!("1"), json!([0, 1, 2, 3])), surface(json!("1"), json!([4, 5, 6, 7]))]},
            "b": {"type": "Building", "geometry": [surface(json!("2"), json!([4, 4, 4])), surface(json!("1"), json!([0, 1, 2, 3]))]},
            "c": {"type": "Building", "geometry": [surface(json!(2), json!([0, 1, 2, 3]))]},
        }),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    let ds = &out[0].0;
    assert!((area(geometry(ds, 0)) - 200.0).abs() < 1e-6, "{:?}", geometry(ds, 0));
    assert!((area(geometry(ds, 1)) - 100.0).abs() < 1e-6, "{:?}", geometry(ds, 1));
    assert_eq!(value(ds, 1, "lod"), crate::Value::Text("1".into()));
    assert_eq!(value(ds, 2, "lod"), crate::Value::Text("2".into()));
}

/// The ground in a solid, the way real files mark it: the first shell's only, as past it a shell is a
/// cavity, whose floor is no footprint. Multi-solids are read two levels deeper.
#[test]
fn the_ground_of_a_solid_is_its_outer_shells() {
    let vertices = json!([
        [0, 0, 5],
        [20, 0, 5],
        [20, 20, 5],
        [0, 20, 5],
        [5, 5, 0],
        [15, 5, 0],
        [15, 15, 0],
        [5, 15, 0],
        [40, 0, 0],
        [50, 0, 0],
        [50, 10, 0],
        [40, 10, 0],
    ]);
    let ground = json!([{"type": "GroundSurface"}]);
    let outer = json!({"type": "Solid", "lod": "2", "boundaries": [[[[0, 1, 2, 3]], [[4, 5, 6, 7]]]],
        "semantics": {"surfaces": ground, "values": [[null, 0]]}});
    let cavity = json!({"type": "Solid", "lod": "2", "boundaries": [[[[0, 1, 2, 3]]], [[[4, 5, 6, 7]]]],
        "semantics": {"surfaces": ground, "values": [[null], [0]]}});
    let solids = json!({"type": "MultiSolid", "lod": "2", "boundaries": [[[[[0, 1, 2, 3]]]], [[[[8, 9, 10, 11]]]]]});
    let bytes = doc(
        vertices,
        json!({
            "a": {"type": "Building", "geometry": [outer]},
            "b": {"type": "Building", "geometry": [cavity]},
            "c": {"type": "Building", "geometry": [solids]},
        }),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    let ds = &out[0].0;
    assert!((area(geometry(ds, 0)) - 100.0).abs() < 1e-6, "its ground: {:?}", geometry(ds, 0));
    assert!((area(geometry(ds, 1)) - 400.0).abs() < 1e-6, "not its cavity's: {:?}", geometry(ds, 1));
    assert!((area(geometry(ds, 2)) - 500.0).abs() < 1e-6, "both solids: {:?}", geometry(ds, 2));
}

/// A level 0 face seen edge on is no footprint as it stands: it goes the way of any wall.
#[test]
fn a_lone_face_seen_edge_on_has_no_footprint() {
    let bytes = doc(
        box_vertices(),
        json!({"w": {"type": "Building", "geometry": [{"type": "MultiSurface", "lod": "0", "boundaries": [[[0, 1, 5, 4]]]}]}}),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    assert!(geometry(&out[0].0, 0).is_none(), "{:?}", geometry(&out[0].0, 0));
    assert!(says(&out, "1 city objects have a shape that can't be drawn"), "{:?}", messages_of(&out));
}

/// Lines and points show the level of detail they came from, not the footprint's. Their own heights
/// differing is more than `z_min` and `z_max` keep, which is said; all at one height, it isn't.
#[test]
fn lines_and_points_keep_their_level_and_say_when_heights_go() {
    let line = |ring: Json| json!({"type": "MultiLineString", "lod": "0", "boundaries": [ring]});
    let surface = json!({"type": "MultiSurface", "lod": "1", "boundaries": [[[0, 1, 2, 3]]]});
    let bytes = doc(box_vertices(), json!({"a": {"type": "Road", "geometry": [line(json!([0, 1, 5])), surface]}}));
    let out = read("m.city.json", &bytes, None).unwrap();
    assert_eq!(out.iter().map(|(d, _)| d.name.as_str()).collect::<Vec<_>>(), ["m Road", "m Road lines"]);
    assert_eq!(value(&out[0].0, 0, "lod"), crate::Value::Text("1".into()));
    assert_eq!(value(&out[1].0, 0, "lod"), crate::Value::Text("0".into()));
    assert!(out[1].0.zm_dropped);
    assert!(out[1].1.messages.iter().any(|m| m.contains("1 feature had Z or M values")), "{:?}", out[1].1);

    let bytes = doc(
        box_vertices(),
        json!({
            "l": {"type": "Road", "geometry": [line(json!([0, 1, 2]))]},
            "p": {"type": "Tree", "geometry": [{"type": "MultiPoint", "lod": "1", "boundaries": [0, 1]}]},
        }),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    assert_eq!(messages_of(&out), Vec::<String>::new());
    assert!(!out[0].0.zm_dropped && !out[1].0.zm_dropped);
    assert_eq!(value(&out[1].0, 0, "lod"), crate::Value::Text("1".into()));
}

/// Objects that come to nothing are said, whichever way they do.
#[test]
fn objects_with_empty_geometries_are_said() {
    let bytes = doc(
        box_vertices(),
        json!({
            "a": {"type": "Building", "geometry": [{"type": "MultiSurface", "lod": "1", "boundaries": []}]},
            "b": {"type": "Building", "geometry": [{"type": "MultiPoint", "boundaries": []}]},
            "c": {"type": "Building", "address": {"City": "Delft"}, "geometry": [{"type": "MultiSurface", "lod": "1", "boundaries": [[[0, 1, 2, 3]]]}]},
        }),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    assert!(says(&out, "1 city objects have a shape that can't be drawn"), "{:?}", messages_of(&out));
    assert!(says(&out, "1 city objects have no geometry of their own"), "{:?}", messages_of(&out));
    assert!(says(&out, "the addresses of 1 city objects are left out"), "{:?}", messages_of(&out));
}

/// A file names a coordinate system by its code: its text is never a new definition, a geocentric
/// one isn't a map's, and coordinates that can't be what it says are said.
#[test]
fn what_a_file_says_of_its_coordinate_system_is_checked() {
    let object = json!({"a": {"type": "LandUse", "geometry": [{"type": "MultiSurface", "lod": "1", "boundaries": [[[0, 1, 2]]]}]}});
    let mut v: Json = serde_json::from_slice(&doc(json!([[85000, 447000, 0], [85100, 447000, 0], [85100, 447100, 0]]), object)).unwrap();
    v["metadata"]["referenceSystem"] = json!("+proj=longlat +datum=WGS84");
    let e = read("m.city.json", v.to_string().as_bytes(), None).unwrap_err().to_string();
    assert!(e.contains("doesn't know"), "{e}");
    v["metadata"]["referenceSystem"] = json!("https://www.opengis.net/def/crs/EPSG/0/4978");
    let e = read("m.city.json", v.to_string().as_bytes(), None).unwrap_err().to_string();
    assert!(e.contains("geocentric"), "{e}");
    v["metadata"]["referenceSystem"] = json!("https://www.opengis.net/def/crs/EPSG/0/4326");
    let out = read("m.city.json", v.to_string().as_bytes(), None).unwrap();
    assert!(geometry(&out[0].0, 0).is_some(), "kept, so that defining its projection can place it");
    assert!(says(&out, "coordinates are not longitude/latitude"), "{:?}", messages_of(&out));
}

/// A geometry the coordinate system can't convert is lost, which defining the projection again can't
/// bring back, so the layer says so. With none converted the system is wrong: an error.
#[test]
fn objects_that_cant_be_converted_mark_the_layer() {
    let triangle =
        |a: i64| json!({"type": "LandUse", "geometry": [{"type": "MultiSurface", "lod": "1", "boundaries": [[[a, a + 1, a + 2]]]}]});
    let near = [[0.0, 0.0, 0.0], [1000.0, 0.0, 0.0], [1000.0, 1000.0, 0.0]];
    let far = [[1e12, 0.0, 0.0], [1e12 + 1000.0, 0.0, 0.0], [1e12 + 1000.0, 1000.0, 0.0]];
    let vertices: Vec<[f64; 3]> = near.iter().chain(&far).copied().collect();
    let bytes = doc(json!(vertices), json!({"a": triangle(0), "b": triangle(3)}));
    let out = read("m.city.json", &bytes, Some(Crs::WebMercator)).unwrap();
    assert!(out[0].0.crs_incomplete);
    assert!(says(&out, "1 city objects have coordinates outside their coordinate system"), "{:?}", messages_of(&out));
    let bytes = doc(json!(vertices), json!({"a": triangle(0)}));
    let out = read("m.city.json", &bytes, Some(Crs::WebMercator)).unwrap();
    assert!(!out[0].0.crs_incomplete);
    // Only that the system given goes over the one the file names (`doc`'s EPSG:4326).
    assert_eq!(messages_of(&out), vec!["the file names EPSG:4326, and is read in EPSG:3857, the coordinate system given".to_string()]);
    let bytes = doc(json!(vertices), json!({"b": triangle(3)}));
    let e = read("m.city.json", &bytes, Some(Crs::WebMercator)).unwrap_err().to_string();
    assert!(e.contains("no object could be converted"), "{e}");
}

/// An object is counted once in a notice, however many of its geometry families it loses.
#[test]
fn an_object_losing_several_families_is_counted_once() {
    let near = [[0.0, 0.0, 0.0], [1000.0, 0.0, 0.0], [1000.0, 1000.0, 0.0]];
    let far = [[1e12, 0.0, 0.0], [1e12 + 1000.0, 0.0, 0.0], [1e12 + 1000.0, 1000.0, 0.0]];
    let vertices: Vec<[f64; 3]> = near.iter().chain(&far).copied().collect();
    let surface = |a: i64| json!({"type": "MultiSurface", "lod": "1", "boundaries": [[[a, a + 1, a + 2]]]});
    let line = json!({"type": "MultiLineString", "lod": "1", "boundaries": [[3, 4, 5]]});
    let bytes = doc(
        json!(vertices),
        json!({
            "a": {"type": "LandUse", "geometry": [surface(0)]},
            "b": {"type": "LandUse", "geometry": [surface(3), line]},
        }),
    );
    let out = read("m.city.json", &bytes, Some(Crs::WebMercator)).unwrap();
    assert!(says(&out, "1 city objects have coordinates outside their coordinate system"), "{:?}", messages_of(&out));
}

/// A byte order mark doesn't hide a model, and a file named only `.city.json` still names its layer.
#[test]
fn a_byte_order_mark_and_a_bare_name() {
    let mut bytes = "\u{feff}".as_bytes().to_vec();
    bytes.extend(doc(box_vertices(), json!({"a": {"type": "Road", "geometry": [{"type": "MultiPoint", "boundaries": [0]}]}})));
    assert!(looks_like_cityjson(std::str::from_utf8(&bytes).unwrap()));
    assert_eq!(read(".city.json", &bytes, None).unwrap()[0].0.name, "city model");
}

/// A level of detail that isn't a number is the lowest, not one that hides the others.
#[test]
fn a_level_that_isnt_a_number_is_the_lowest() {
    let bytes = doc(
        box_vertices(),
        json!({"b": {"type": "Building", "geometry": [
            {"type": "Solid", "lod": "2", "boundaries": box_shell()},
            {"type": "MultiSurface", "lod": "nan", "boundaries": [[[0, 1, 2, 3]]]},
        ]}}),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    assert_eq!(value(&out[0].0, 0, "lod"), crate::Value::Text("2".into()));
    assert!((area(geometry(&out[0].0, 0)) - 100.0).abs() < 1e-6);
}

/// Extension types make the number of layers the file's to choose: the rest are said.
#[test]
fn layers_past_the_cap_are_said() {
    let objects: serde_json::Map<String, Json> = (0..MAX_LAYERS + 3)
        .map(|i| (format!("o{i}"), json!({"type": format!("+T{i:03}"), "geometry": [{"type": "MultiPoint", "boundaries": [0]}]})))
        .collect();
    let out = read("m.city.json", &doc(box_vertices(), Json::Object(objects)), None).unwrap();
    assert_eq!(out.len(), MAX_LAYERS);
    assert!(says(&out, "3 of 67 layers are left out"), "{:?}", messages_of(&out));
}

/// Through the general readers: a `.city.json` has all its layers, a `.json` its first, naming the rest.
#[test]
fn the_general_readers_find_it() {
    let bytes = doc(
        box_vertices(),
        json!({
            "a": {"type": "Bridge", "geometry": [{"type": "MultiSurface", "lod": "1", "boundaries": [[[0, 1, 2, 3]]]}]},
            "b": {"type": "Road", "geometry": [{"type": "MultiSurface", "lod": "1", "boundaries": [[[0, 1, 2, 3]]]}]},
        }),
    );
    assert!(crate::io::is_multi_layer_name("delft.city.json"));
    assert!(!crate::io::is_multi_layer_name("delft.json"));
    assert_eq!(crate::io::read_layers("m.city.json", &bytes).unwrap().len(), 2);
    assert_eq!(crate::io::read_layers("m.json", &bytes).unwrap().len(), 2);
    let (ds, report) = crate::io::read_bytes("m.json", &bytes).unwrap();
    assert_eq!(ds.name, "m Bridge");
    assert!(report.messages.iter().any(|m| m.contains("it also holds m Road")), "{report:?}");
    // Other `.json` files aren't taken for it.
    let (ds, _) = crate::io::read_bytes("m.json", br#"{"type": "FeatureCollection", "features": []}"#).unwrap();
    assert_eq!(ds.features.len(), 0);
}

/// A document with the given root members over `doc`'s.
fn doc_with(vertices: Json, objects: Json, extra: Json) -> Vec<u8> {
    let mut root: Json = serde_json::from_slice(&doc(vertices, objects)).unwrap();
    for (k, v) in extra.as_object().unwrap() {
        root[k] = v.clone();
    }
    root.to_string().into_bytes()
}

fn one_point() -> Json {
    json!({"a": {"type": "Road", "geometry": [{"type": "MultiPoint", "boundaries": [0]}]}})
}

/// Appearance isn't read, and that is said with how much; an empty one has nothing to say.
#[test]
fn appearance_left_out_is_said() {
    let appearance = json!({"appearance": {"materials": [{"name": "brick"}, {"name": "roof"}], "textures": [{"type": "PNG"}]}});
    let out = read("m.city.json", &doc_with(json!([[0, 0, 0]]), one_point(), appearance), None).unwrap();
    assert!(says(&out, "2 materials and 1 textures are left out"), "{:?}", messages_of(&out));
    let empty = json!({"appearance": {"materials": [], "textures": []}});
    for bytes in [doc_with(json!([[0, 0, 0]]), one_point(), empty), doc(json!([[0, 0, 0]]), one_point())] {
        let out = read("m.city.json", &bytes, None).unwrap();
        assert!(!says(&out, "appearance"), "{:?}", messages_of(&out));
    }
    // One that isn't what the specification says doesn't stop the model, and is said.
    let out = read("m.city.json", &doc_with(json!([[0, 0, 0]]), one_point(), json!({"appearance": 5})), None).unwrap();
    assert!(says(&out, "appearance isn't read"), "{:?}", messages_of(&out));
}

/// Types that differ only past the characters a layer's name shows stay apart.
#[test]
fn long_types_stay_apart() {
    let (a, b) = (format!("+{}A", "x".repeat(70)), format!("+{}B", "x".repeat(70)));
    let geometry = json!([{"type": "MultiPoint", "boundaries": [0]}]);
    let objects = json!({"a": {"type": a, "geometry": geometry}, "b": {"type": b, "geometry": geometry}});
    let out = read("m.city.json", &doc(json!([[0, 0, 0]]), objects), None).unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].0.name.chars().count(), "m ".len() + MAX_TYPE_IN_NAME);
    assert_eq!(value(&out[0].0, 0, "type"), crate::Value::Text(a));
    assert_eq!(value(&out[1].0, 0, "type"), crate::Value::Text(b));
}

/// However many numbered names are taken, a renamed attribute finds a free one and loses nothing.
#[test]
fn a_renamed_attribute_always_finds_a_free_name() {
    let mut attributes: serde_json::Map<String, Json> = (1..=120).map(|k| (format!("id_{k}"), json!("taken"))).collect();
    attributes.insert("id".to_string(), json!("mine"));
    attributes.insert("id_attribute".to_string(), json!("taken"));
    let objects = json!({"a": {"type": "Road", "attributes": attributes, "geometry": [{"type": "MultiPoint", "boundaries": [0]}]}});
    let out = read("m.city.json", &doc(json!([[0, 0, 0]]), objects), None).unwrap();
    let ds = &out[0].0;
    assert_eq!(value(ds, 0, "id_121"), crate::Value::Text("mine".to_string()));
    assert_eq!(value(ds, 0, "id_attribute"), crate::Value::Text("taken".to_string()));
    assert_eq!(value(ds, 0, "id_120"), crate::Value::Text("taken".to_string()));
    assert_eq!(value(ds, 0, "id"), crate::Value::Text("a".to_string()));

    // Spellings of one name take numbers of their own.
    let objects = json!({"a": {"type": "Road", "attributes": {"ID": "upper", "id": "lower"}, "geometry": [{"type": "MultiPoint", "boundaries": [0]}]}});
    let out = read("m.city.json", &doc(json!([[0, 0, 0]]), objects), None).unwrap();
    assert_eq!(value(&out[0].0, 0, "ID_1"), crate::Value::Text("upper".to_string()));
    assert_eq!(value(&out[0].0, 0, "id_2"), crate::Value::Text("lower".to_string()));
}

/// A height the transform takes past what a number holds is left out, and said; others stay.
#[test]
fn heights_that_are_not_numbers_are_said() {
    let scale = json!({"transform": {"scale": [1.0, 1.0, 10.0], "translate": [0.0, 0.0, 0.0]}});
    let objects = json!({"a": {"type": "Road", "geometry": [{"type": "MultiPoint", "boundaries": [0, 1]}]}});
    let bytes = doc_with(json!([[0, 0, 1e308], [1, 1, 2]]), objects.clone(), scale.clone());
    let out = read("m.city.json", &bytes, None).unwrap();
    assert!(says(&out, "1 city objects have heights that aren't finite numbers"), "{:?}", messages_of(&out));
    assert_eq!(value(&out[0].0, 0, "z_max"), crate::Value::Float(20.0));
    let out = read("m.city.json", &doc_with(json!([[0, 0, 3], [1, 1, 2]]), objects, scale), None).unwrap();
    assert!(!says(&out, "finite numbers"), "{:?}", messages_of(&out));
}

/// `count` objects, each the parent of the next in the order they're visited, with heights only at
/// the far end: the worst order, one level a round.
fn chain(count: usize) -> Json {
    let mut objects = serde_json::Map::new();
    for i in 0..count {
        let mut object = json!({"type": "Building"});
        if i > 0 {
            object["parents"] = json!([format!("o{:03}", i - 1)]);
        }
        if i + 1 == count {
            object["geometry"] = json!([{"type": "MultiPoint", "boundaries": [0, 1]}]);
        }
        objects.insert(format!("o{i:03}"), object);
    }
    Json::Object(objects)
}

/// Heights reach the top of a chain past the two levels of the specification, without a notice.
#[test]
fn heights_reach_the_top_of_a_long_chain() {
    let out = read("m.city.json", &doc(json!([[0, 0, 1], [1, 1, 9]]), chain(40)), None).unwrap();
    let ds = out.iter().map(|(ds, _)| ds).find(|ds| ds.features.len() > 1).unwrap();
    assert_eq!(value(ds, 0, "id"), crate::Value::Text("o000".to_string()));
    assert_eq!(value(ds, 0, "height"), crate::Value::Float(8.0));
    assert!(!says(&out, "parent chains"), "{:?}", messages_of(&out));
}

fn built(id: &str, parent: Option<&str>, z: Option<(f64, f64)>) -> Built {
    Built {
        id: id.into(),
        ty: "Building".into(),
        parent_ids: parent.into_iter().map(String::from).collect(),
        z,
        solid: false,
        attributes: Vec::new(),
    }
}

/// Past what the visits allow it stops and says after how many rounds; a cycle settles.
#[test]
fn raising_parents_is_bounded() {
    let mut objects: Vec<Built> = (0..20)
        .map(|i| {
            let parent = (i > 0).then(|| format!("o{}", i - 1));
            built(&format!("o{i}"), parent.as_deref(), (i == 19).then_some((0.0, 5.0)))
        })
        .collect();
    assert_eq!(raise_parents(&mut objects, 0), Some(MIN_PARENT_ROUNDS));
    assert_eq!(objects[0].z, None, "the top isn't reached");
    assert_eq!(raise_parents(&mut objects, MAX_PARENT_VISITS), None, "with room, it settles");
    assert_eq!(objects[0].z, Some((0.0, 5.0)));
    // The visits pay for twelve rounds of a fresh chain (39 a round): it stops there, short of the top.
    let mut fresh: Vec<Built> = (0..20)
        .map(|i| {
            let parent = (i > 0).then(|| format!("o{}", i - 1));
            built(&format!("o{i}"), parent.as_deref(), (i == 19).then_some((0.0, 5.0)))
        })
        .collect();
    assert_eq!(raise_parents(&mut fresh, 39 * 12), Some(12));
    assert_eq!(fresh[0].z, None);
    let mut cycle = vec![built("a", Some("b"), Some((1.0, 2.0))), built("b", Some("a"), Some((0.0, 1.0)))];
    assert_eq!(raise_parents(&mut cycle, MAX_PARENT_VISITS), None);
    assert_eq!((cycle[0].z, cycle[1].z), (Some((0.0, 2.0)), Some((0.0, 2.0))));
}

/// Heights in a system of feet are said, as extrusions read metres; a system in metres says nothing.
#[test]
fn heights_not_in_metres_are_said() {
    let named = |code: u32| json!({"metadata": {"referenceSystem": format!("https://www.opengis.net/def/crs/EPSG/0/{code}")}});
    let points = json!([[1000, 1000, 0], [1010, 1010, 30]]);
    let objects = json!({"a": {"type": "Road", "geometry": [{"type": "MultiPoint", "boundaries": [0, 1]}]}});
    let out = read("m.city.json", &doc_with(points.clone(), objects.clone(), named(2263)), None).unwrap();
    assert!(says(&out, "measures in us-ft"), "{:?}", messages_of(&out));
    assert_eq!(value(&out[0].0, 0, "z_max"), crate::Value::Float(30.0), "kept as the file has them");
    let out = read("m.city.json", &doc_with(points.clone(), objects, named(28992)), None).unwrap();
    assert!(!says(&out, "measures in"), "{:?}", messages_of(&out));
    // The file's system given is said too, and so is a vertical unit, in a geographic system as well.
    let given = doc(points, json!({"a": {"type": "Road", "geometry": [{"type": "MultiPoint", "boundaries": [0, 1]}]}}));
    assert!(says(&read("m.city.json", &given, Crs::from_epsg(2263)).unwrap(), "measures in us-ft"));
    let unit = |crs: Option<Crs>| crs.unwrap().height_unit_not_metre();
    assert_eq!(unit(Crs::from_epsg(7406)).as_deref(), Some("us-ft"), "+proj=longlat with +vunits=us-ft");
    assert_eq!(unit(Crs::from_epsg(2263)).as_deref(), Some("us-ft"));
    assert_eq!(unit(Crs::from_epsg(28992)), None);
    assert_eq!(unit(Crs::from_epsg(4326)), None);
    assert_eq!(unit(Crs::from_epsg(3857)), None);
    assert_eq!(unit(Crs::from_epsg(4267)), None, "geographic, with no vertical unit");
    let typed = |d: &str| Crs::parse(d).unwrap().height_unit_not_metre();
    assert_eq!(typed("+proj=utm +zone=18 +datum=NAD83 +to_meter=0.3048 +no_defs").as_deref(), Some("0.3048 m"));
    assert_eq!(typed("+proj=utm +zone=18 +datum=NAD83 +units=m +vunits=us-ft +no_defs").as_deref(), Some("us-ft"));
    assert_eq!(typed("+proj=utm +zone=18 +datum=NAD83 +units=us-ft +vunits=m +no_defs"), None, "the vertical unit goes first");
    assert_eq!(typed("+proj=utm +zone=18 +datum=NAD83 +units=m +no_defs"), None);
}

/// In a zip a model adds every layer, under the system given.
#[test]
fn a_zipped_model_has_its_layers_and_takes_the_system_given() {
    let objects = json!({
        "a": {"type": "Bridge", "geometry": [{"type": "MultiPoint", "boundaries": [0]}]},
        "b": {"type": "Road", "geometry": [{"type": "MultiPoint", "boundaries": [0]}]},
    });
    let mut root: Json = serde_json::from_slice(&doc(json!([[1000, 1000, 0]]), objects)).unwrap();
    root.as_object_mut().unwrap().remove("metadata");
    let zip = crate::zip::write(&[("d/m.city.json".to_string(), root.to_string().into_bytes())]).unwrap();
    let out = crate::io::read_zip_in("x.zip", &zip, Crs::from_epsg(28992)).unwrap();
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|(ds, _)| ds.source.as_deref() == Some("x.zip/d/m.city.json")));
    assert!(out.iter().all(|(ds, _)| ds.crs == Crs::from_epsg(28992)));
    // Without one, it is said why the member is left out.
    assert!(crate::io::read_zip_in("x.zip", &zip, None).unwrap_err().to_string().contains("names no coordinate system"));

    // The system given is for what can't say its own: a model naming one it knows keeps it...
    root["metadata"] = json!({"referenceSystem": "https://www.opengis.net/def/crs/EPSG/0/28992"});
    let zip = crate::zip::write(&[("m.city.json".to_string(), root.to_string().into_bytes())]).unwrap();
    let out = crate::io::read_zip_in("x.zip", &zip, Crs::from_epsg(32631)).unwrap();
    assert!(out.iter().all(|(ds, _)| ds.crs == Crs::from_epsg(28992)));
    // ...and one naming a system RustGIS doesn't know takes it.
    root["metadata"] = json!({"referenceSystem": "https://www.opengis.net/def/crs/EPSG/0/999999"});
    let zip = crate::zip::write(&[("m.city.json".to_string(), root.to_string().into_bytes())]).unwrap();
    let out = crate::io::read_zip_in("x.zip", &zip, Crs::from_epsg(32631)).unwrap();
    assert!(out.iter().all(|(ds, _)| ds.crs == Crs::from_epsg(32631)));
    // Outside an archive the system given goes first, as the command asks.
    let bare = read("m.city.json", root.to_string().as_bytes(), Crs::from_epsg(32631)).unwrap();
    assert!(bare.iter().all(|(ds, _)| ds.crs == Crs::from_epsg(32631)));
}

/// A model past the zip's layer limit: its first layers are kept, and those left out are named.
#[test]
fn a_zipped_model_past_the_layer_limit_is_said() {
    let count = crate::io::MAX_ZIP_LAYERS + 2;
    let objects: serde_json::Map<String, Json> = (0..count)
        .map(|i| (format!("o{i:02}"), json!({"type": format!("T{i:02}"), "geometry": [{"type": "MultiPoint", "boundaries": [0]}]})))
        .collect();
    let zip = crate::zip::write(&[("m.city.json".to_string(), doc(json!([[0, 0, 0]]), Json::Object(objects)))]).unwrap();
    let out = crate::io::read_zip_in("x.zip", &zip, None).unwrap();
    assert_eq!(out.len(), crate::io::MAX_ZIP_LAYERS);
    assert!(says(&out, "layer limit"), "{:?}", messages_of(&out));
    assert!(says(&out, "m T33"), "{:?}", messages_of(&out));
}

/// Lines and points keep only their highest level of detail, as surfaces do, under its label.
#[test]
fn lines_and_points_keep_their_highest_lod() {
    let vertices = json!([[0, 0, 0], [1, 0, 0], [5, 5, 0], [6, 5, 0]]);
    let objects = json!({"a": {"type": "Road", "geometry": [
        {"type": "MultiLineString", "lod": "1", "boundaries": [[0, 1]]},
        {"type": "MultiLineString", "lod": "2", "boundaries": [[2, 3]]},
        {"type": "MultiPoint", "lod": "2", "boundaries": [0]},
        {"type": "MultiPoint", "lod": "1", "boundaries": [3]},
        {"type": "MultiPoint", "lod": "2", "boundaries": [1]},
    ]}});
    let out = read("m.city.json", &doc(vertices, objects), None).unwrap();
    let lines = out.iter().map(|(ds, _)| ds).find(|ds| ds.name.ends_with("lines")).unwrap();
    assert_eq!(geometry(lines, 0), Some(&Geometry::Line(vec![vec![Coord::new(5.0, 5.0), Coord::new(6.0, 5.0)]])));
    assert_eq!(value(lines, 0, "lod"), crate::Value::Text("2".into()));
    let points = out.iter().map(|(ds, _)| ds).find(|ds| ds.name.ends_with("points")).unwrap();
    assert_eq!(geometry(points, 0), Some(&Geometry::MultiPoint(vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0)])));
    assert_eq!(value(points, 0, "lod"), crate::Value::Text("2".into()));
}

/// A height too large to be a number is said, and left empty; others are kept and say nothing.
#[test]
fn a_height_too_large_to_be_a_number_is_said() {
    let objects = json!({"a": {"type": "Road", "geometry": [{"type": "MultiPoint", "boundaries": [0, 1]}]}});
    let out = read("m.city.json", &doc(json!([[0, 0, -1e308], [1, 1, 1e308]]), objects.clone()), None).unwrap();
    assert!(says(&out, "1 city objects have a lowest and highest Z too far apart"), "{:?}", messages_of(&out));
    assert_eq!(value(&out[0].0, 0, "height"), crate::Value::Null);
    assert_eq!(value(&out[0].0, 0, "z_max"), crate::Value::Float(1e308));
    let out = read("m.city.json", &doc(json!([[0, 0, -10], [1, 1, 20]]), objects), None).unwrap();
    assert!(!says(&out, "too far apart"), "{:?}", messages_of(&out));
}

/// A line of one point at a higher level doesn't take the place of a line drawn at a lower one, and
/// an instance at a lower level, left out, isn't said to be drawn.
#[test]
fn what_draws_nothing_or_is_left_out_takes_no_place() {
    let vertices = json!([[0, 0, 0], [1, 0, 0], [5, 5, 0]]);
    let objects = json!({"a": {"type": "Road", "geometry": [
        {"type": "MultiLineString", "lod": "1", "boundaries": [[0, 1]]},
        {"type": "MultiLineString", "lod": "2", "boundaries": [[2]]},
    ]}});
    let out = read("m.city.json", &doc(vertices.clone(), objects), None).unwrap();
    assert_eq!(geometry(&out[0].0, 0), Some(&Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0)]])));
    assert_eq!(value(&out[0].0, 0, "lod"), crate::Value::Text("1".into()));
    let instance = |lod: &str| json!({"type": "GeometryInstance", "lod": lod, "template": 0, "boundaries": [2], "transformationMatrix": [1,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,0,1]});
    let objects = json!({"a": {"type": "SolitaryVegetationObject", "geometry": [instance("1"), {"type": "MultiPoint", "lod": "2", "boundaries": [0]}]}});
    let out = read("m.city.json", &doc(vertices.clone(), objects), None).unwrap();
    assert!(!says(&out, "geometry instances"), "{:?}", messages_of(&out));
    let objects = json!({"a": {"type": "SolitaryVegetationObject", "geometry": [instance("2"), {"type": "MultiPoint", "lod": "1", "boundaries": [0]}]}});
    let out = read("m.city.json", &doc(vertices, objects), None).unwrap();
    assert!(says(&out, "1 geometry instances are drawn"), "{:?}", messages_of(&out));
}

/// A system given over the one the file names is said; one that is the same, or a file naming none,
/// says nothing of it.
#[test]
fn a_system_given_over_the_one_named_is_said() {
    let named = json!({"metadata": {"referenceSystem": "https://www.opengis.net/def/crs/EPSG/0/7415"}});
    let points = json!([[85000, 447000, 0], [85010, 447010, 5]]);
    let objects = json!({"a": {"type": "Road", "geometry": [{"type": "MultiPoint", "boundaries": [0, 1]}]}});
    let bytes = doc_with(points.clone(), objects.clone(), named);
    let out = read("m.city.json", &bytes, Crs::from_epsg(32631)).unwrap();
    assert!(says(&out, "the coordinate system given"), "{:?}", messages_of(&out));
    assert!(out.iter().all(|(ds, _)| ds.crs == Crs::from_epsg(32631)));
    let out = read("m.city.json", &bytes, Crs::from_epsg(28992)).unwrap();
    assert!(!says(&out, "the coordinate system given"), "{:?}", messages_of(&out));
    let mut root: Json = serde_json::from_slice(&doc(points, objects)).unwrap();
    root.as_object_mut().unwrap().remove("metadata");
    let out = read("m.city.json", root.to_string().as_bytes(), Crs::from_epsg(28992)).unwrap();
    assert!(!says(&out, "the coordinate system given"), "{:?}", messages_of(&out));
    // One RustGIS doesn't know is said by its name too, since the system given takes its place...
    root["metadata"] = json!({"referenceSystem": "https://www.opengis.net/def/crs/EPSG/0/999999"});
    let out = read("m.city.json", root.to_string().as_bytes(), Crs::from_epsg(28992)).unwrap();
    assert!(
        says(
            &out,
            "the file names a coordinate system RustGIS doesn't know (https://www.opengis.net/def/crs/EPSG/0/999999), and is read in"
        ),
        "{:?}",
        messages_of(&out)
    );
    assert!(out.iter().all(|(ds, _)| ds.crs == Crs::from_epsg(28992)));
    // ...and without one given, it is an error, not a note.
    let e = read("m.city.json", root.to_string().as_bytes(), None).unwrap_err().to_string();
    assert!(e.contains("it names a coordinate system RustGIS doesn't know"), "{e}");
}

/// One building whose LoD 2 geometry passes the face cap: a face with a courtyard first (marked
/// `GroundSurface` when `ground_first`), then walls, past the cap by one, then (`late_ground`) a
/// ground face beside it, 10 wide, left out. `lower_ground`: a 1-wide ground at LoD 1 too.
fn courtyard_past_the_cap(ground_first: bool, late_ground: bool, lower_ground: bool) -> Vec<u8> {
    let mut boundaries = String::from("[[[0,1,2,3],[4,7,6,5]]");
    let mut values = String::from(if ground_first { "[0" } else { "[null" });
    for _ in 0..MAX_FACES {
        boundaries.push_str(",[[0,1,9,8]]");
        values.push_str(",null");
    }
    if late_ground {
        boundaries.push_str(",[[1,10,11,2]]");
        values.push_str(",0");
    }
    boundaries.push(']');
    values.push(']');
    let lower = if lower_ground {
        r#",{"type":"MultiSurface","lod":"1","boundaries":[[[12,13,14,15]]],"semantics":{"surfaces":[{"type":"GroundSurface"}],"values":[0]}}"#
    } else {
        ""
    };
    format!(
        r#"{{"type":"CityJSON","version":"2.0","transform":{{"scale":[1,1,1],"translate":[0,0,0]}},
        "metadata":{{"referenceSystem":"https://www.opengis.net/def/crs/EPSG/0/4326"}},
        "vertices":[[0,0,0],[10,0,0],[10,10,0],[0,10,0],[3,3,0],[7,3,0],[7,7,0],[3,7,0],[0,0,5],[10,0,5],
            [20,0,0],[20,10,0],[0,0,0],[1,0,0],[1,1,0],[0,1,0]],
        "CityObjects":{{"b":{{"type":"Building","geometry":[{{"type":"MultiSurface","lod":"2","boundaries":{boundaries},
        "semantics":{{"surfaces":[{{"type":"GroundSurface"}}],"values":{values}}}}}{lower}]}}}}}}"#
    )
    .into_bytes()
}

/// Past the face cap, a ground footprint keeps its courtyard when only walls were left out (#1374).
/// A ground face left out makes it the outline of the ground's points, those left out included,
/// said; so does a highest LoD whose ground was all left out, over a lower LoD's ground.
#[test]
fn walls_past_the_face_cap_leave_the_ground_whole() {
    let polygon = |out: &[(Dataset, ReadReport)]| match geometry(&out[0].0, 0) {
        Some(Geometry::Polygon(p)) => p.clone(),
        other => panic!("{other:?}"),
    };
    let out = read("m.city.json", &courtyard_past_the_cap(true, false, false), None).unwrap();
    let p = polygon(&out);
    assert_eq!((p.len(), p[0].len()), (1, 2), "the ground with its courtyard: {p:?}");
    assert!(!says(&out, "outline of their points"), "{:?}", messages_of(&out));
    // Faces past the cap, said as such: not as faces that couldn't be merged.
    let capped = |out: &[(Dataset, ReadReport)]| {
        says(out, "1 city objects have a geometry of more than 200000 faces") && !says(out, "couldn't be merged")
    };

    let out = read("m.city.json", &courtyard_past_the_cap(true, true, false), None).unwrap();
    let p = polygon(&out);
    assert_eq!((p.len(), p[0].len()), (1, 1), "the outline, without the courtyard: {p:?}");
    assert!((area(geometry(&out[0].0, 0)) - 200.0).abs() < 1e-6, "over the ground left out too: {p:?}");
    assert!(capped(&out), "{:?}", messages_of(&out));

    let out = read("m.city.json", &courtyard_past_the_cap(false, true, true), None).unwrap();
    assert!((area(geometry(&out[0].0, 0)) - 100.0).abs() < 1e-6, "LoD 2's ground left out: {:?}", geometry(&out[0].0, 0));
    assert_eq!(value(&out[0].0, 0, "lod"), crate::Value::Text("2".into()));
    assert!(capped(&out), "{:?}", messages_of(&out));
}

/// Types that read the same once cut, the same but for case, or a type named as another's lines,
/// still give layers of their own names (#1375); the rest keep theirs, and a name a type already
/// has isn't given again.
#[test]
fn layers_of_types_cut_to_one_name_are_told_apart() {
    let long = "+".to_string() + &"A".repeat(MAX_TYPE_IN_NAME);
    let cut: String = long.chars().take(MAX_TYPE_IN_NAME).collect();
    let face = json!([{"type": "MultiSurface", "lod": "1", "boundaries": [[[0, 1, 2, 3]]]}]);
    let bytes = doc(
        box_vertices(),
        json!({
            "a": {"type": format!("{long}x"), "geometry": face},
            "b": {"type": format!("{long}y"), "geometry": face},
            "c": {"type": "+Road", "geometry": [
                {"type": "MultiSurface", "lod": "1", "boundaries": [[[0, 1, 2, 3]]]},
                {"type": "MultiLineString", "lod": "0", "boundaries": [[0, 1]]},
            ]},
            "d": {"type": "+Road lines", "geometry": face},
            "e": {"type": "+Shed", "geometry": face},
            "f": {"type": "+shed", "geometry": face},
            "g": {"type": "+shed (2)", "geometry": face},
            "h": {"type": "Building", "geometry": face},
        }),
    );
    let out = read("m.city.json", &bytes, None).unwrap();
    let names: Vec<&str> = out.iter().map(|(d, _)| d.name.as_str()).collect();
    let (one, two) = (format!("m {cut}"), format!("m {cut} (2)"));
    assert_eq!(
        names,
        [
            one.as_str(),
            two.as_str(),
            "m +Road",
            "m +Road lines",
            "m +Road lines (2)",
            "m +Shed",
            "m +shed (3)",
            "m +shed (2)",
            "m Building"
        ]
    );
    assert!(out.iter().all(|(d, _)| d.features.len() == 1));
    assert_eq!(unique_names(vec!["x".into(), "y".into()]), ["x", "y"], "names that differ are kept");
}
