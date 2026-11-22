use rustgis_analysis::{Inputs, Output};
use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value};
use serde_json::json;
use std::collections::HashMap;

#[derive(Default)]
struct Layers {
    data: HashMap<String, Dataset>,
    selected: HashMap<String, Vec<usize>>,
}
impl Inputs for Layers {
    fn dataset(&self, name: &str) -> Option<&Dataset> {
        self.data.get(name)
    }
    fn selection(&self, name: &str) -> &[usize] {
        self.selected.get(name).map(Vec::as_slice).unwrap_or(&[])
    }
}
fn output(layers: &Layers, params: serde_json::Value) -> Dataset {
    match rustgis_analysis::run("merge", layers, &params).unwrap().output {
        Output::Layer(ds) => ds,
        _ => panic!("expected a vector layer"),
    }
}
fn point(x: f64) -> Option<Geometry> {
    Some(Geometry::Point(Coord::new(x, 88.0)))
}

#[test]
fn combines_schemas_without_converting_or_overwriting_conflicting_fields() {
    let mut a = Dataset::new(
        "A",
        GeometryType::Point,
        vec![Field::new("name", FieldType::Text), Field::new("value", FieldType::Integer), Field::new("value_2", FieldType::Text)],
    );
    a.fields[0].alias = Some("Title".into());
    a.push_next(point(1.0), vec![Value::Text("a".into()), Value::Int(i64::MAX), Value::Text("keep".into())]);
    let mut b = Dataset::new(
        "B",
        GeometryType::Point,
        vec![Field::new("NAME", FieldType::Text), Field::new("value", FieldType::Double), Field::new("extra", FieldType::Boolean)],
    );
    b.push_next(
        Some(Geometry::MultiPoint(vec![Coord::new(2.0, 88.0), Coord::new(3.0, 88.0)])),
        vec![Value::Text("b".into()), Value::Float(1.5), Value::Bool(true)],
    );
    let mut c = Dataset::new("C", GeometryType::Point, vec![Field::new("value", FieldType::Double)]);
    c.push_next(None, vec![Value::Float(2.5)]);
    let layers = Layers { data: HashMap::from([("A".into(), a.clone()), ("B".into(), b.clone()), ("C".into(), c)]), ..Default::default() };
    let ds = output(&layers, json!({"inputs":["A","B","C"],"output":"Combined"}));
    assert_eq!(ds.name, "Combined");
    assert_eq!(ds.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["name", "value", "value_2", "value_3", "extra"]);
    assert_eq!(ds.fields[0].alias.as_deref(), Some("Title"));
    assert_eq!(ds.value(0, 1), &Value::Int(i64::MAX));
    assert_eq!(ds.value(0, 2), &Value::Text("keep".into()));
    assert_eq!(ds.value(1, 0), &Value::Text("b".into()));
    assert_eq!(ds.value(1, 3), &Value::Float(1.5));
    assert_eq!(ds.value(2, 3), &Value::Float(2.5));
    assert_eq!(ds.value(2, 0), &Value::Null);
    assert_eq!(ds.features.iter().map(|f| f.id).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(ds.features[0].geometry, a.features[0].geometry);
    assert_eq!(ds.features[1].geometry, b.features[0].geometry);
    assert!(ds.features[2].geometry.is_none());
    assert_eq!(layers.data["A"], a, "inputs are unchanged");
}

#[test]
fn honors_each_selection_and_can_drop_attributes() {
    let mut layers = Layers::default();
    for name in ["A", "B"] {
        let mut ds = Dataset::new(name, GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
        for i in 0..3 {
            ds.push_next(point(i as f64), vec![Value::Int(i)]);
        }
        layers.data.insert(name.into(), ds);
    }
    layers.selected.insert("A".into(), vec![2, usize::MAX]);
    layers.selected.insert("B".into(), vec![1]);
    let ds = output(&layers, json!({"inputs":" A ; B ","attributes":false}));
    assert!(ds.fields.is_empty());
    assert_eq!(ds.features.len(), 2);
    assert_eq!(ds.features[0].geometry, point(2.0));
    assert_eq!(ds.features[1].geometry, point(1.0));
    assert!(ds.features.iter().all(|f| f.attrs.is_empty()));
}

#[test]
fn preserves_line_parts_and_polygon_holes_without_projection() {
    let ring = vec![Coord::new(0.0, 87.0), Coord::new(1.0, 87.0), Coord::new(1.0, 89.0), Coord::new(0.0, 87.0)];
    for (ty, g) in [
        (GeometryType::Polyline, Geometry::Line(vec![ring.clone(), vec![]])),
        (GeometryType::Polygon, Geometry::Polygon(vec![vec![ring.clone(), ring.clone()], vec![]])),
    ] {
        let mut ds = Dataset::new("A", ty, vec![]);
        ds.push_next(Some(g.clone()), vec![]);
        let layers = Layers { data: HashMap::from([("A".into(), ds)]), ..Default::default() };
        assert_eq!(output(&layers, json!({"inputs":["A"]})).features[0].geometry, Some(g));
    }
}

#[test]
fn rejects_bad_lists_tables_and_mixed_geometry_types() {
    let mut layers = Layers {
        data: HashMap::from([
            ("A".into(), Dataset::new("A", GeometryType::Point, vec![])),
            ("B".into(), Dataset::new("B", GeometryType::Polygon, vec![])),
            ("table".into(), Dataset::new("table", GeometryType::None, vec![])),
        ]),
        ..Default::default()
    };
    for p in [
        json!({"inputs":[]}),
        json!({"inputs":["A",42]}),
        json!({"inputs":[["A"]]}),
        json!({"inputs":";"}),
        json!({"inputs":"missing"}),
        json!({"inputs":"A;B"}),
        json!({"inputs":"table"}),
        json!({"inputs":"A","attributes":"yes"}),
    ] {
        assert!(rustgis_analysis::run("merge", &layers, &p).is_err(), "{p}");
    }
    layers.data.get_mut("A").unwrap().push_next(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0)]])), vec![]);
    assert!(rustgis_analysis::run("merge", &layers, &json!({"inputs":"A"})).is_err());
}
