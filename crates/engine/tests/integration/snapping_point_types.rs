//! Point targets obey the same switches without changing feature exclusion or snap ordering.
use rustgis_engine::{
    Project, Session,
    cmd::design::{snap_at, snap_except},
    data::{Dataset, Feature, Geometry},
    geom::{Coord, merc_to_lonlat},
    map::{LayerGroup, Projection, design::Snapping},
};
use serde_json::json;

/// Stores metre-sized fixtures in the longitude/latitude coordinates used by feature layers.
fn c(x: f64, y: f64) -> Coord {
    merc_to_lonlat(Coord::new(x, y))
}

/// Adds one ordinary feature without importing any external data or application-specific state.
fn add(s: &mut Session, geometry: Geometry) -> u32 {
    let mut data = Dataset::new("Targets", geometry.kind(), vec![]);
    data.features = vec![Feature { id: 1, geometry: Some(geometry), attrs: vec![] }].into();
    s.map_mut().add_dataset(data)
}

/// Uses a two-pixel tolerance at one Mercator metre per pixel, with only vertices enabled.
fn fixture(geometry: Geometry) -> (Session, u32) {
    let mut s = Session::new(Project::new("Point target types"));
    let id = add(&mut s, geometry);
    s.map_mut().camera.res = 1.;
    s.map_mut().editing.snapping = Snapping { enabled: true, tolerance_px: 2., vertex: true, edge: false, end: false };
    (s, id)
}

/// Checks both candidate kind and inverse-projected position, allowing projection rounding only.
fn hit(actual: Option<(Coord, &'static str)>, at: Coord, kind: &str) {
    let (found, what) = actual.expect("eligible snap target");
    assert_eq!(what, kind);
    assert!((found.x - at.x).abs() < 1e-9 && (found.y - at.y).abs() < 1e-9, "{found:?} != {at:?}");
}

/// A Point must not override the user's edge-only choice, even inside the tolerance.
#[test]
fn point_requires_vertex() {
    let (mut s, _) = fixture(Geometry::Point(c(0., 0.)));
    s.map_mut().editing.snapping.vertex = false;
    s.map_mut().editing.snapping.edge = true;
    assert!(snap_at(&mut s, c(0.5, 0.)).is_none());
}

/// Every MultiPoint member is a vertex target, and the nearest member wins over an earlier one.
#[test]
fn multipoint_members_are_vertices() {
    let (mut s, _) = fixture(Geometry::MultiPoint(vec![c(0., 0.), c(3., 0.)]));
    hit(snap_at(&mut s, c(2.5, 0.)), c(3., 0.), "vertex");
}

/// Both point representations obey all eight type-switch combinations and the master switch.
/// MultiPoint members never form implicit connecting edges or supply line-end candidates.
#[test]
fn both_point_types_obey_every_switch_combination() {
    for geometry in [Geometry::Point(c(0., 0.)), Geometry::MultiPoint(vec![c(0., 0.)])] {
        let (mut s, _) = fixture(geometry);
        for enabled in [false, true] {
            for bits in 0..8 {
                s.map_mut().editing.snapping =
                    Snapping { enabled, tolerance_px: 2., vertex: bits & 1 != 0, edge: bits & 2 != 0, end: bits & 4 != 0 };
                assert_eq!(snap_at(&mut s, c(0.5, 0.)).is_some(), enabled && bits & 1 != 0, "enabled={enabled}, bits={bits}");
            }
        }
    }
    let (mut s, _) = fixture(Geometry::MultiPoint(vec![c(-10., 0.), c(10., 0.)]));
    s.map_mut().editing.snapping.edge = true;
    s.map_mut().editing.snapping.end = true;
    assert!(snap_at(&mut s, c(0., 0.)).is_none(), "no edge joins MultiPoint members");
    let (mut s, _) = fixture(Geometry::MultiPoint(vec![]));
    assert!(snap_at(&mut s, c(0., 0.)).is_none());
}

/// Point targets keep the original point-before-edge priority and layer/row/member tie order.
/// Existing line-end and polygon-vertex switches continue to work independently.
#[test]
fn mixed_targets_preserve_priority_and_ties() {
    let (mut s, id) = fixture(Geometry::MultiPoint(vec![c(-1., 0.), c(1., 0.), c(-1., 0.)]));
    s.map_mut().layer_mut(id).unwrap().dataset.features.push(Feature { id: 2, geometry: Some(Geometry::Point(c(1., 0.))), attrs: vec![] });
    hit(snap_at(&mut s, c(0., 0.)), c(-1., 0.), "vertex");
    add(&mut s, Geometry::Point(c(1., 0.)));
    add(&mut s, Geometry::Line(vec![vec![c(-10., 0.), c(10., 0.)]]));
    s.map_mut().editing.snapping.edge = true;
    hit(snap_at(&mut s, c(0., 0.)), c(1., 0.), "vertex");
    s.map_mut().layers.reverse();
    hit(snap_at(&mut s, c(0., 0.)), c(-1., 0.), "vertex");
    s.map_mut().editing.snapping.vertex = false;
    hit(snap_at(&mut s, c(0., 0.)), c(0., 0.), "edge");
    s.map_mut().editing.snapping.end = true;
    hit(snap_at(&mut s, c(9., 0.)), c(10., 0.), "end");
    let (mut s, _) = fixture(Geometry::Point(c(1., 0.)));
    add(&mut s, Geometry::MultiPoint(vec![c(-1., 0.)]));
    hit(snap_at(&mut s, c(0., 0.)), c(-1., 0.), "vertex");
    let (mut s, _) = fixture(Geometry::Polygon(vec![vec![vec![c(0., 0.), c(100., 0.), c(100., 100.), c(0., 0.)]]]));
    hit(snap_at(&mut s, c(0.5, 0.)), c(0., 0.), "vertex");
    s.map_mut().editing.snapping.vertex = false;
    s.map_mut().editing.snapping.end = true;
    assert!(snap_at(&mut s, c(0.5, 0.)).is_none(), "polygon nodes are not line ends");
}

/// Feature exclusion still removes every member of an addressed MultiPoint and only that row.
/// Direct and command queries return the same target without changing geometry or undo state.
#[test]
fn whole_feature_exclusion_and_read_only_queries_remain_intact() {
    for geometry in [Geometry::Point(c(0., 0.)), Geometry::MultiPoint(vec![c(0., 0.), c(1., 0.)])] {
        let (mut s, id) = fixture(geometry);
        assert!(snap_except(&mut s, c(0.5, 0.), Some((id, 0))).is_none());
        s.map_mut().layer_mut(id).unwrap().dataset.features.push(Feature {
            id: 2,
            geometry: Some(Geometry::Point(c(1.5, 0.))),
            attrs: vec![],
        });
        hit(snap_except(&mut s, c(0.5, 0.), Some((id, 0))), c(1.5, 0.), "vertex");
        let before = serde_json::to_value(&s.project).unwrap();
        let revision = s.revision;
        let expected = snap_at(&mut s, c(0.5, 0.)).unwrap();
        assert_eq!(snap_except(&mut s, c(0.5, 0.), None), Some(expected));
        assert_eq!(s.revision, revision, "direct previews do not invalidate the view");
        let query = c(0.5, 0.);
        let out = s.run("edit.snap", &json!({"lon":query.x,"lat":query.y})).unwrap();
        assert_eq!(out["snapped"], true);
        assert_eq!(out["kind"], expected.1);
        assert_eq!(out["lon"], expected.0.x);
        assert_eq!(out["lat"], expected.0.y);
        assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
        assert_eq!(s.revision, revision.wrapping_add(1), "commands retain the existing view-invalidation revision");
        assert_eq!(s.undo_depth(), 0);
        s.map_mut().editing.snapping.enabled = false;
        let out = s.run("edit.snap", &json!({"lon":query.x,"lat":query.y})).unwrap();
        assert_eq!(out, json!({"lon":query.x,"lat":query.y,"snapped":false}));
    }
}

/// Pixel tolerance scales with map resolution, with inclusive boundary and outside rejection.
/// Hidden, out-of-scale and grouped-off point layers cannot supply candidates.
#[test]
fn tolerance_and_layer_eligibility_are_unchanged() {
    for geometry in [Geometry::Point(c(0., 0.)), Geometry::MultiPoint(vec![c(0., 0.)])] {
        let (mut s, id) = fixture(geometry);
        hit(snap_at(&mut s, c(2., 0.)), c(0., 0.), "vertex");
        assert!(snap_at(&mut s, c(2.01, 0.)).is_none());
        s.map_mut().camera.res = 2.;
        hit(snap_at(&mut s, c(3., 0.)), c(0., 0.), "vertex");
        s.map_mut().layer_mut(id).unwrap().visible = false;
        assert!(snap_at(&mut s, c(0., 0.)).is_none());
        s.map_mut().layer_mut(id).unwrap().visible = true;
        s.map_mut().layer_mut(id).unwrap().min_scale = Some(1.);
        assert!(snap_at(&mut s, c(0., 0.)).is_none());
        s.map_mut().layer_mut(id).unwrap().min_scale = None;
        s.map_mut().layer_mut(id).unwrap().graphics = true;
        assert!(snap_at(&mut s, c(0., 0.)).is_none());
        s.map_mut().layer_mut(id).unwrap().graphics = false;
        let mut group = LayerGroup::new("Hidden targets");
        group.children.push(id);
        group.visible = false;
        s.map_mut().add_group(group);
        assert!(snap_at(&mut s, c(0., 0.)).is_none());
    }
}

/// World and regional projections retain valid members and ignore unprojectable coordinates.
#[test]
fn projected_multipoints_keep_valid_members_only() {
    for code in ["equirectangular", "EPSG:4549", "mollweide +lon_0=-90"] {
        let at = if code.starts_with("mollweide") { Coord::new(-110., 20.) } else { Coord::new(120., 31.) };
        let (mut s, _) = fixture(Geometry::MultiPoint(vec![Coord::new(f64::NAN, 0.), at, Coord::new(0., f64::INFINITY)]));
        s.map_mut().projection = Projection::from_code(code).unwrap();
        s.map_mut().camera.center = s.map().to_map(at).unwrap();
        hit(snap_at(&mut s, at), at, "vertex");
        assert!(snap_at(&mut s, Coord::new(f64::NAN, 0.)).is_none());
    }
    let (mut s, _) = fixture(Geometry::MultiPoint(vec![Coord::new(0., 95.), Coord::new(120., 31.)]));
    s.map_mut().projection = Projection::from_code("EPSG:4549").unwrap();
    hit(snap_at(&mut s, Coord::new(120., 31.)), Coord::new(120., 31.), "vertex");
}
