//! Public tool dispatch covers multipart boundaries, selections and endpoint graph semantics.
use rustgis_analysis::{Inputs, Output, ToolResult};
use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value};
use serde_json::json;

struct Layer {
    ds: Dataset,
    selected: Vec<usize>,
}
impl Inputs for Layer {
    fn dataset(&self, name: &str) -> Option<&Dataset> {
        (name == "input").then_some(&self.ds)
    }
    fn selection(&self, _: &str) -> &[usize] {
        &self.selected
    }
}
/// A layer with a named attribute so joins cannot silently choose among conflicting values.
fn layer(ty: GeometryType) -> Layer {
    Layer { ds: Dataset::new("input", ty, vec![Field::new("name", FieldType::Text)]), selected: vec![] }
}
/// Coordinates stay unprojected even above the Web Mercator latitude limit.
fn coords(xs: &[f64]) -> Vec<Coord> {
    xs.iter().map(|x| Coord::new(*x, 89.0)).collect()
}
/// Inserts one multipart line with the given shared attribute.
fn line(l: &mut Layer, parts: &[&[f64]], attr: &str) {
    l.ds.push_next(Some(Geometry::Line(parts.iter().map(|xs| coords(xs)).collect())), vec![Value::Text(attr.into())]);
}
/// Uses the same parameter resolution and dispatch as gp.run.
fn run(tool: &str, l: &Layer) -> ToolResult {
    rustgis_analysis::run(tool, l, &json!({"input":"input", "output":"converted"})).unwrap()
}
/// Requires a vector layer, including an intentionally empty result.
fn dataset(r: ToolResult) -> Dataset {
    let Output::Layer(ds) = r.output else { panic!("expected a layer") };
    ds
}

#[test]
fn polygon_boundaries_keep_holes_parts_attributes_and_close_open_rings() {
    let mut l = layer(GeometryType::Polygon);
    let outer = vec![Coord::new(0., 0.), Coord::new(4., 0.), Coord::new(4., 4.), Coord::new(0., 0.)];
    let hole = vec![Coord::new(1., 1.), Coord::new(2., 1.), Coord::new(2., 2.)];
    l.ds.crs = Some(rustgis_data::raster::Crs::Epsg(26915));
    l.ds.crs_incomplete = true;
    l.ds.zm_dropped = true;
    l.ds.push_next(Some(Geometry::Polygon(vec![vec![outer.clone(), hole.clone()], vec![outer.clone()]])), vec![Value::Text("keep".into())]);
    let r = run("polygonToLine", &l);
    assert!(r.messages.is_empty());
    let ds = dataset(r);
    let mut closed = hole;
    closed.push(Coord::new(1., 1.));
    assert_eq!(ds.features[0].geometry, Some(Geometry::Line(vec![outer.clone(), closed, outer])));
    assert_eq!(ds.features[0].attrs, l.ds.features[0].attrs);
    assert_eq!(ds.fields, l.ds.fields);
    assert_eq!(ds.crs, l.ds.crs);
    assert!(ds.crs_incomplete);
    assert!(ds.zm_dropped);
    assert_eq!(ds.name, "converted");
}

#[test]
fn split_honors_selection_parts_repeated_vertices_and_original_coordinates() {
    let mut l = layer(GeometryType::Polyline);
    line(&mut l, &[&[0., 1.]], "unselected");
    line(&mut l, &[&[2., 3., 3.], &[8., 9.]], "selected");
    l.selected = vec![1, usize::MAX];
    let r = run("splitLineAtVertices", &l);
    assert!(r.messages.is_empty());
    let ds = dataset(r);
    assert_eq!(ds.features.len(), 3);
    for (f, xs) in ds.features.iter().zip([[2., 3.], [3., 3.], [8., 9.]]) {
        assert_eq!(f.geometry, Some(Geometry::Line(vec![coords(&xs)])));
        assert_eq!(f.attrs, vec![Value::Text("selected".into())]);
    }
    assert_eq!(ds.features.iter().map(|f| f.id).collect::<Vec<_>>(), [1, 2, 3]);
}

#[test]
fn conversions_pad_sparse_attributes_and_reject_values_without_fields() {
    for tool in ["polygonToLine", "splitLineAtVertices", "featureEnvelopeToPolygon", "unsplitLine"] {
        let polygon = tool == "polygonToLine";
        let mut l = layer(if polygon { GeometryType::Polygon } else { GeometryType::Polyline });
        let vertices = vec![Coord::new(0., 0.), Coord::new(2., 1.), Coord::new(0., 2.), Coord::new(0., 0.)];
        let geometry = if polygon { Geometry::Polygon(vec![vec![vertices]]) } else { Geometry::Line(vec![vertices]) };
        l.ds.push_next(Some(geometry), vec![]);
        l.ds.features[0].attrs.clear();
        let r = run(tool, &l);
        assert!(r.messages.is_empty());
        let ds = dataset(r);
        assert!(!ds.features.is_empty());
        assert!(ds.features.iter().all(|f| f.attrs == vec![Value::Null]));
        assert_eq!(ds.fields, l.ds.fields);
        l.ds.features[0].attrs = vec![Value::Text("retained".into()), Value::Text("extra".into())];
        let error = rustgis_analysis::run(tool, &l, &json!({"input":"input"})).unwrap_err();
        assert!(error.contains("feature 1 has attribute count 2 exceeding schema width 1"), "{tool}: {error}");
    }
}

#[test]
fn envelopes_bound_all_parts_and_report_zero_area_and_missing_geometries() {
    let mut l = layer(GeometryType::Point);
    l.ds.push_next(Some(Geometry::MultiPoint(vec![Coord::new(-3., 86.), Coord::new(7., 89.)])), vec![]);
    let r = run("featureEnvelopeToPolygon", &l);
    assert!(r.messages.is_empty());
    let ds = dataset(r);
    assert_eq!(
        ds.features[0].geometry,
        Some(Geometry::Polygon(vec![vec![vec![
            Coord::new(-3., 86.),
            Coord::new(7., 86.),
            Coord::new(7., 89.),
            Coord::new(-3., 89.),
            Coord::new(-3., 86.)
        ]]]))
    );
    l.ds.push_next(Some(Geometry::Point(Coord::new(1., 2.))), vec![]);
    l.ds.push_next(None, vec![]);
    let r = run("featureEnvelopeToPolygon", &l);
    assert_eq!(r.messages.len(), 1);
    assert!(r.messages[0].contains("skipped 2"));
    l.selected = vec![1, 2];
    assert!(rustgis_analysis::run("featureEnvelopeToPolygon", &l, &json!({"input":"input"})).is_err());
}

#[test]
fn unsplit_walks_reversed_out_of_order_chains_and_cycles_once() {
    let mut l = layer(GeometryType::Polyline);
    line(&mut l, &[&[2., 1.], &[3., 4.]], "road");
    line(&mut l, &[&[3., 2.]], "road");
    line(&mut l, &[&[10., 11.], &[12., 11.], &[12., 10.]], "loop");
    let r = run("unsplitLine", &l);
    assert!(r.messages.is_empty());
    let ds = dataset(r);
    assert_eq!(ds.features.len(), 2);
    assert_eq!(ds.features[0].geometry, Some(Geometry::Line(vec![coords(&[1., 2., 3., 4.])])));
    assert_eq!(ds.features[1].geometry, Some(Geometry::Line(vec![coords(&[10., 11., 12., 10.])])));
    assert_eq!(ds.features[0].attrs, vec![Value::Text("road".into())]);
}

/// Missing trailing values and explicit nulls have the same schema meaning at a join.
/// Disconnected parts still copy the common text into each output chain.
#[test]
fn unsplit_joins_sparse_and_explicit_null_attributes_and_keeps_disjoint_text() {
    let mut l = layer(GeometryType::Polyline);
    l.ds.fields.push(Field::new("optional", FieldType::Integer));
    let text = "road".repeat(1024);
    line(&mut l, &[&[0., 1.], &[10., 11.]], &text);
    line(&mut l, &[&[1., 2.]], &text);
    l.ds.features[0].attrs.pop();
    let r = run("unsplitLine", &l);
    assert!(r.messages.is_empty());
    let ds = dataset(r);
    assert_eq!(ds.features.len(), 2);
    assert_eq!(ds.features[0].geometry, Some(Geometry::Line(vec![coords(&[0., 1., 2.])])));
    assert_eq!(ds.features[1].geometry, Some(Geometry::Line(vec![coords(&[10., 11.])])));
    for f in ds.features.iter() {
        assert_eq!(f.attrs, vec![Value::Text(text.clone()), Value::Null]);
    }
}

#[test]
fn unsplit_stops_at_branches_attribute_changes_and_near_endpoints() {
    let mut l = layer(GeometryType::Polyline);
    line(&mut l, &[&[0., 1.], &[1., 2.], &[1., 3.]], "branch");
    line(&mut l, &[&[5., 6.]], "a");
    line(&mut l, &[&[6., 7.]], "b");
    line(&mut l, &[&[7.0000000001, 8.]], "b");
    assert_eq!(dataset(run("unsplitLine", &l)).features.len(), 6);
    l.selected = vec![0];
    assert_eq!(dataset(run("unsplitLine", &l)).features.len(), 3);
}

#[test]
fn empty_parts_are_counted_nonfinite_and_wrong_types_are_errors() {
    for tool in ["splitLineAtVertices", "unsplitLine"] {
        let mut l = layer(GeometryType::Polyline);
        line(&mut l, &[&[0., 1.], &[], &[3.]], "keep");
        l.ds.push_next(None, vec![]);
        let r = run(tool, &l);
        assert_eq!(r.messages.len(), 1);
        assert!(r.messages[0].contains("skipped 3"));
        line(&mut l, &[&[f64::NAN, 1.]], "bad");
        assert!(rustgis_analysis::run(tool, &l, &json!({"input":"input"})).is_err());
    }
    let l = layer(GeometryType::Point);
    for tool in ["polygonToLine", "splitLineAtVertices", "unsplitLine"] {
        assert!(rustgis_analysis::run(tool, &l, &json!({"input":"input"})).is_err());
    }
}

/// Empty polygons and discarded rings count once, without counting their parent a second time.
#[test]
fn polygon_boundary_loss_counts_only_discarded_pieces() {
    let mut l = layer(GeometryType::Polygon);
    l.ds.push_next(Some(Geometry::Polygon(vec![vec![coords(&[0., 1., 0.])]])), vec![]);
    l.ds.push_next(Some(Geometry::Polygon(vec![vec![vec![]]])), vec![]);
    let r = run("polygonToLine", &l);
    assert_eq!(r.messages.len(), 1);
    assert!(r.messages[0].contains("skipped 1"));
    assert_eq!(dataset(r).features.len(), 1);
    l.ds.push_next(Some(Geometry::Polygon(vec![vec![], vec![coords(&[2., 3., 2.])]])), vec![]);
    let r = run("polygonToLine", &l);
    assert!(r.messages[0].contains("skipped 2"));
    assert_eq!(dataset(r).features.len(), 2);
}
