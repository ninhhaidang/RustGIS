//! Exact-coordinate polygon moves use the ordinary design-rule and undo transaction.
use rustgis_engine::{
    Project, Session,
    data::{Dataset, Feature, Field, FieldType, Geometry, GeometryType, Value as Cell, VertexId},
    geom::Coord,
    map::design::{AttributeRule, RuleKind},
};
use serde_json::{Value, json};

/// Builds an explicitly closed rectangular polygon without import normalization.
fn rectangle(left: f64, bottom: f64, right: f64, top: f64) -> Geometry {
    Geometry::Polygon(vec![vec![vec![
        Coord::new(left, bottom),
        Coord::new(right, bottom),
        Coord::new(right, top),
        Coord::new(left, top),
        Coord::new(left, bottom),
    ]]])
}

/// Four parcels meet at zero, including two pairs with only a point in common.
fn fixture() -> (Session, u32) {
    let mut s = Session::new(Project::new("Shared vertex moves"));
    let id = add_layer(
        &mut s,
        "Parcels",
        vec![rectangle(0., 0., 1., 1.), rectangle(-1., 0., 0., 1.), rectangle(-1., -1., 0., 0.), rectangle(0., -1., 1., 0.)],
    );
    s.active_layer = Some(id);
    s.clear_history();
    (s, id)
}

/// Adds source geometries with non-row feature IDs and a field usable by calculation/constraint rules.
fn add_layer(s: &mut Session, name: &str, geometries: Vec<Geometry>) -> u32 {
    let mut data = Dataset::new(name, GeometryType::Polygon, vec![Field::new("n", FieldType::Integer)]);
    data.features = geometries
        .into_iter()
        .enumerate()
        .map(|(i, geometry)| Feature { id: 40 + i as u64, geometry: Some(geometry), attrs: vec![Cell::Int(i as i64)] })
        .collect::<Vec<_>>()
        .into();
    s.map_mut().add_dataset(data)
}

/// Reads full feature values so tests catch accidental attribute, ID or non-target changes.
fn features(s: &Session, id: u32) -> Vec<Feature> {
    s.map().layer(id).unwrap().dataset.features.iter().cloned().collect()
}

/// Runs the shared command at the fixture's first node; row is a position, not feature ID 40.
fn move_to(s: &mut Session, id: u32, at: Coord) -> Value {
    s.run("edit.moveSharedVertex", &json!({"layer":id,"row":0,"index":0,"lon":at.x,"lat":at.y})).unwrap()
}

/// Point-only neighbours and filtered, unselected rows move in one batch, with complete undo/redo.
#[test]
fn all_four_corners_move_without_selection_or_visibility_limits() {
    let (mut s, id) = fixture();
    let layer = s.map_mut().layer_mut(id).unwrap();
    layer.visible = false;
    layer.definition_query = Some("n = 0".into());
    layer.selection = vec![0];
    layer.design.archive = Some(Vec::new());
    let before = features(&s, id);
    let result = move_to(&mut s, id, Coord::new(0.2, 0.3));
    assert_eq!(result, json!({"part":0,"ring":0,"index":0,"modified":4}));
    let after = features(&s, id);
    for (old, new) in before.iter().zip(&after) {
        let mut expected = old.geometry.clone().unwrap();
        for (v, c) in old.geometry.as_ref().unwrap().vertices() {
            if c == Coord::new(0., 0.) {
                expected = expected.move_vertex(v, Coord::new(0.2, 0.3)).unwrap();
            }
        }
        assert_eq!(new.geometry.as_ref(), Some(&expected));
        assert_eq!((new.id, &new.attrs), (old.id, &old.attrs));
    }
    assert_eq!(s.map().layer(id).unwrap().design.archive.as_ref().unwrap().len(), 4);
    assert_eq!(s.edit_counts[&id].modified, 4);
    assert_eq!(s.undo_depth(), 1);
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(features(&s, id), before);
    assert!(s.map().layer(id).unwrap().design.archive.as_ref().unwrap().is_empty());
    assert_eq!(s.edit_counts[&id].modified, 4, "undo retains cumulative edit counts");
    s.run("edit.redo", &Value::Null).unwrap();
    assert_eq!(features(&s, id), after);
    assert_eq!(s.map().layer(id).unwrap().design.archive.as_ref().unwrap().len(), 4);
}

/// All original addresses in a feature move, including holes, other parts and repeated coordinates.
#[test]
fn multipart_holes_and_repeated_nodes_follow_the_original_coordinate() {
    let mut s = Session::new(Project::new("Multipart nodes"));
    let c = Coord::new(1., 1.);
    let mut geometry = rectangle(0., 0., 4., 4.);
    if let Geometry::Polygon(parts) = &mut geometry {
        parts[0].push(vec![c, Coord::new(2., 1.), Coord::new(2., 2.), c]);
        parts.push(vec![vec![c, Coord::new(3., 1.), c, Coord::new(3., 3.), c]]);
    }
    let id = add_layer(&mut s, "Parts", vec![geometry.clone()]);
    let at = Coord::new(1.1, 1.2);
    let result = s.run("edit.moveSharedVertex", &json!({"layer":id,"row":0,"part":0,"ring":1,"index":0,"lon":at.x,"lat":at.y})).unwrap();
    assert_eq!(result, json!({"part":0,"ring":1,"index":0,"modified":1}));
    let moved = features(&s, id)[0].geometry.clone().unwrap();
    for (v, old) in geometry.vertices() {
        assert_eq!(moved.vertex(v), Some(if old == c { at } else { old }));
    }
    if let Geometry::Polygon(parts) = &moved {
        for rings in parts {
            for ring in rings {
                assert_eq!(ring.first(), ring.last());
            }
        }
    }
}

/// Exact stored equality never captures nearby coordinates, non-polygons, or another layer.
#[test]
fn near_vertices_and_other_layers_remain_unchanged() {
    let (mut s, id) = fixture();
    let near = rectangle(f64::EPSILON, 0., 2., 2.);
    s.map_mut().layer_mut(id).unwrap().dataset.features.push(Feature { id: 99, geometry: Some(near.clone()), attrs: vec![Cell::Int(9)] });
    s.map_mut().layer_mut(id).unwrap().dataset.features.push(Feature {
        id: 100,
        geometry: Some(Geometry::Point(Coord::new(0., 0.))),
        attrs: vec![Cell::Int(10)],
    });
    s.map_mut().layer_mut(id).unwrap().dataset.features.push(Feature { id: 101, geometry: None, attrs: vec![Cell::Int(11)] });
    let unchanged = features(&s, id).into_iter().skip(4).collect::<Vec<_>>();
    let other = add_layer(&mut s, "Other", vec![rectangle(0., 0., 1., 1.)]);
    let untouched = features(&s, other);
    assert_eq!(move_to(&mut s, id, Coord::new(0.2, 0.3))["modified"], 4);
    assert_eq!(features(&s, id)[4].geometry.as_ref(), Some(&near));
    assert_eq!(&features(&s, id)[4..], unchanged.as_slice());
    assert_eq!(features(&s, other), untouched);
}

/// Frozen pre-optimization address loop, used only as a small-input closing-alias oracle.
fn addressed_move(geometry: &Geometry, original: Coord, at: Coord) -> Geometry {
    let mut moved = geometry.clone();
    geometry.for_each_vertex(|vertex, c| {
        if c == original {
            moved = moved.move_vertex(vertex, at).unwrap();
        }
    });
    moved
}

/// Single-copy rewriting agrees with vertex-address moves when destinations create closure too.
#[test]
fn coincident_rewrite_preserves_open_and_closed_ring_address_semantics() {
    let c = Coord::new(0., 0.);
    let a = Coord::new(1., 0.);
    let b = Coord::new(1., 1.);
    let d = Coord::new(0., 1.);
    let rings = [vec![c, a, b, c, d, c], vec![c, a, b, c, d], vec![a, c, b, d, c], vec![c, a, b, d, a], vec![a, b, c, d, a]];
    for ring in rings {
        let index = ring.iter().position(|point| *point == c).unwrap();
        for at in [c, a, Coord::new(-0.1, 0.2)] {
            let geometry = Geometry::Polygon(vec![vec![ring.clone()]]);
            let expected = addressed_move(&geometry, c, at);
            let mut s = Session::new(Project::new("Closing aliases"));
            let id = add_layer(&mut s, "Parcels", vec![geometry.clone()]);
            let result = s.run("edit.moveSharedVertex", &json!({"layer":id,"row":0,"index":index,"lon":at.x,"lat":at.y})).unwrap();
            assert_eq!(features(&s, id)[0].geometry.as_ref(), Some(&expected), "{ring:?} → {at:?}");
            assert_eq!(result["modified"], usize::from(expected != geometry));
        }
    }
}

/// Many stored matches rewrite together without changing row identity, attributes or one-step undo.
#[test]
fn densely_repeated_nodes_move_and_undo_as_one_feature() {
    let c = Coord::new(0., 0.);
    let at = Coord::new(-0.1, 0.2);
    let mut ring = vec![c; 32_768];
    ring.extend([Coord::new(1., 0.), Coord::new(1., 1.), Coord::new(0., 1.), c]);
    let geometry = Geometry::Polygon(vec![vec![ring.clone()]]);
    let expected = Geometry::Polygon(vec![vec![ring.into_iter().map(|point| if point == c { at } else { point }).collect()]]);
    let mut s = Session::new(Project::new("Repeated nodes"));
    let id = add_layer(&mut s, "Parcels", vec![geometry]);
    s.clear_history();
    let before = features(&s, id);
    assert_eq!(move_to(&mut s, id, at)["modified"], 1);
    let after = features(&s, id);
    assert_eq!(after[0].geometry.as_ref(), Some(&expected));
    assert_eq!((after[0].id, &after[0].attrs), (before[0].id, &before[0].attrs));
    assert_eq!(s.undo_depth(), 1);
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(features(&s, id), before);
    s.run("edit.redo", &Value::Null).unwrap();
    assert_eq!(features(&s, id), after);
}

/// Different segmentation is accepted: intermediate nodes are neither inserted nor remapped.
#[test]
fn differing_density_moves_existing_nodes_and_keeps_the_documented_limitation() {
    let mut s = Session::new(Project::new("Unequal density"));
    let a = rectangle(0., 0., 2., 2.);
    let b = Geometry::Polygon(vec![vec![vec![
        Coord::new(2., 0.),
        Coord::new(1., 0.),
        Coord::new(0., 0.),
        Coord::new(0., -2.),
        Coord::new(2., -2.),
        Coord::new(2., 0.),
    ]]]);
    let id = add_layer(&mut s, "Parcels", vec![a, b]);
    let result = s.run("edit.moveSharedVertex", &json!({"layer":id,"row":0,"index":1,"lon":2,"lat":0.5})).unwrap();
    assert_eq!(result["modified"], 2);
    let after = features(&s, id);
    assert_eq!(after[0].geometry.as_ref().unwrap().vertex(VertexId::new(0, 0, 1)), Some(Coord::new(2., 0.5)));
    assert_eq!(after[1].geometry.as_ref().unwrap().vertex(VertexId::new(0, 0, 0)), Some(Coord::new(2., 0.5)));
    assert_eq!(after[1].geometry.as_ref().unwrap().vertex(VertexId::new(0, 0, 1)), Some(Coord::new(1., 0.)));
    assert_eq!(after[1].geometry.as_ref().unwrap().vertices().len(), 5);
}

/// A later row's constraint rejection restores every member, archives, counts, dirty and redo.
#[test]
fn later_rule_rejection_rolls_back_the_whole_batch() {
    let (mut s, id) = fixture();
    let layer = s.map_mut().layer_mut(id).unwrap();
    layer.design.archive = Some(Vec::new());
    layer.design.rules.push(AttributeRule {
        name: "reject later parcel".into(),
        kind: RuleKind::Constraint,
        field: String::new(),
        expression: "n < 2".into(),
    });
    s.run("layer.rename", &json!({"layer":id,"name":"Renamed"})).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    let before = serde_json::to_value(&s.project).unwrap();
    let counts = s.edit_counts.clone();
    let dirty = s.dirty();
    assert!(s.can_redo());
    let error = s.run("edit.moveSharedVertex", &json!({"layer":id,"row":0,"index":0,"lon":0.2,"lat":0.3})).unwrap_err();
    assert!(error.contains("reject later parcel"));
    assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
    assert_eq!(s.edit_counts, counts);
    assert_eq!(s.dirty(), dirty);
    assert_eq!(s.undo_depth(), 0);
    assert!(s.can_redo());
}

/// Same-position moves retain ordinary rule/count/history semantics while reporting actual changes.
#[test]
fn same_position_runs_calculations_and_keeps_existing_undo_behaviour() {
    let (mut s, id) = fixture();
    assert_eq!(move_to(&mut s, id, Coord::new(0., 0.))["modified"], 0);
    assert_eq!(s.edit_counts[&id].modified, 4);
    assert_eq!(s.undo_depth(), 1);
    assert!(s.dirty());
    s.map_mut().layer_mut(id).unwrap().design.rules.push(AttributeRule {
        name: "increment".into(),
        kind: RuleKind::Calculation,
        field: "n".into(),
        expression: "n + 1".into(),
    });
    assert_eq!(move_to(&mut s, id, Coord::new(0., 0.))["modified"], 4);
    assert_eq!(s.edit_counts[&id].modified, 8);
    assert_eq!(s.undo_depth(), 2);
    assert_eq!(features(&s, id)[3].attrs[0], Cell::Int(4));
}

/// Hostile addresses and destinations fail without changing data or history, including closing aliases.
#[test]
fn malformed_inputs_and_wrong_geometry_are_rejected_without_mutation() {
    let (mut s, id) = fixture();
    for key in ["row", "part", "ring", "index", "lon", "lat"] {
        for bad in [Value::Null, json!(-1), json!(1.5), json!("junk"), json!([]), json!(u64::MAX)] {
            let mut p = json!({"layer":id,"row":0,"part":0,"ring":0,"index":0,"lon":0.2,"lat":0.3});
            p[key] = bad;
            if (key == "part" || key == "ring") && p[key].is_null() {
                continue;
            }
            if (key == "lon" || key == "lat") && p[key].as_f64().is_some_and(|v| v.abs() <= 90.) {
                continue;
            }
            let before = features(&s, id);
            assert!(s.run("edit.moveSharedVertex", &p).is_err(), "{p}");
            assert_eq!(features(&s, id), before);
            assert_eq!(s.undo_depth(), 0);
        }
    }
    for index in [4, 999] {
        assert!(s.run("edit.moveSharedVertex", &json!({"layer":id,"row":0,"index":index,"lon":0.2,"lat":0.3})).is_err());
    }
    s.map_mut().layer_mut(id).unwrap().dataset.features[0].geometry = Some(Geometry::Point(Coord::new(0., 0.)));
    assert!(s.run("edit.moveSharedVertex", &json!({"layer":id,"row":0,"index":0,"lon":0.2,"lat":0.3})).is_err());
}

/// Existing streamed-edit and editing-disabled guards reject the command before any row changes.
#[test]
fn streamed_and_disabled_editing_use_existing_guards() {
    let (mut s, id) = fixture();
    s.map_mut().editing.settings.enabled = false;
    let before = features(&s, id);
    assert!(s.run("edit.moveSharedVertex", &json!({"layer":id,"row":0,"index":0,"lon":1,"lat":1})).is_err());
    assert_eq!(features(&s, id), before);
    s.map_mut().editing.settings.enabled = true;
    let stream = s.map_mut().add_streamed("Stream", Default::default());
    let error = s.run("edit.moveSharedVertex", &json!({"layer":stream,"row":0,"index":0,"lon":1,"lat":1})).unwrap_err();
    assert!(error.contains("Load All Features"), "{error}");
    assert_eq!(s.undo_depth(), 0);
}
