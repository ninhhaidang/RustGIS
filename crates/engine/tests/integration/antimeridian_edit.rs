//! Editing a shape kept unwrapped across 180° (as a geodesic buffer returns one) keeps it narrow (#940).

use rustgis_engine::{
    Project, Session,
    data::{Dataset, Feature, Field, FieldType, Geometry, GeometryType, Value},
    geom::Coord,
};
use serde_json::json;

/// A selected, editable polygon with unwrapped antimeridian coordinates.
fn across_180() -> (Session, u32) {
    let c = Coord::new;
    let ring = vec![c(179.6, 10.0), c(180.2, 10.0), c(180.2, 10.3), c(179.6, 10.3), c(179.6, 10.0)];
    let mut data = Dataset::new("Across", GeometryType::Polygon, vec![Field::new("n", FieldType::Integer)]);
    data.features = vec![Feature { id: 1, geometry: Some(Geometry::Polygon(vec![vec![ring]])), attrs: vec![Value::Int(0)] }].into();
    let mut s = Session::new(Project::new("Across 180"));
    let id = s.map_mut().add_dataset(data);
    s.active_layer = Some(id);
    s.map_mut().editing.settings.enabled = true;
    s.map_mut().layer_mut(id).unwrap().selection = vec![0];
    (s, id)
}

#[test]
fn small_edits_keep_a_polygon_across_180_narrow() {
    for (cmd, params) in
        [("edit.move", json!({"dx": 1000.0, "dy": 0.0})), ("edit.rotate", json!({"angle": 10.0})), ("edit.scale", json!({"factor": 1.01}))]
    {
        let (mut s, id) = across_180();
        let before = s.map().layer(id).unwrap().dataset.features[0].geometry.clone();
        s.run(cmd, &params).unwrap();
        assert_ne!(s.map().layer(id).unwrap().dataset.features[0].geometry, before, "{cmd} must change coordinates");
        let Some(Geometry::Polygon(p)) = &s.map().layer(id).unwrap().dataset.features[0].geometry else { panic!("{cmd}") };
        let (lo, hi) = p[0][0].iter().fold((f64::MAX, f64::MIN), |(a, b), c| (a.min(c.x), b.max(c.x)));
        assert!(hi - lo < 1.0, "{cmd}: the polygon is now {:.1}° wide", hi - lo);
    }
}
