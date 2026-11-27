//! Source-addressed snapping for ordinary in-memory feature layers.
use rustgis_engine::{
    Project, Session,
    cmd::design::{snap_except, snap_vertex_except},
    data::{Dataset, Feature, Geometry, GeometryType, VertexId},
    geom::{Coord, merc_to_lonlat},
    map::{Projection, design::Snapping},
};
use serde_json::json;

/// Makes small fixtures in Mercator metres while storing the original longitude/latitude nodes.
fn c(x: f64, y: f64) -> Coord {
    merc_to_lonlat(Coord::new(x, y))
}

/// A public upstream session with one source geometry and a two-pixel snapping tolerance.
fn fixture(g: Geometry) -> (Session, u32) {
    let mut s = Session::new(Project::new("Source vertex snapping"));
    let mut data = Dataset::new("Paths", g.kind(), vec![]);
    data.features = vec![Feature { id: 1, geometry: Some(g), attrs: vec![] }].into();
    let id = s.map_mut().add_dataset(data);
    s.map_mut().camera.res = 1.0;
    s.map_mut().editing.snapping = Snapping { enabled: true, tolerance_px: 2.0, vertex: false, edge: true, end: false };
    (s, id)
}

/// Requires the expected candidate kind and coordinate, allowing only inverse-projection rounding.
fn hit(actual: Option<(Coord, &'static str)>, expected: Coord, kind: &str) {
    let (at, what) = actual.expect("eligible source target");
    assert_eq!(what, kind);
    assert!((at.x - expected.x).abs() < 1e-7 && (at.y - expected.y).abs() < 1e-7, "{at:?} != {expected:?}");
}

/// The legacy whole-feature exclusion demonstrates the limitation on the very same fixture.
/// Addressed exclusion keeps a different part and a distinct node at the same coordinate.
#[test]
fn another_part_and_a_coincident_node_remain_targets() {
    let (mut s, id) = fixture(Geometry::Line(vec![vec![c(0., 0.), c(0., 100.)], vec![c(0., 0.), c(-100., 0.)]]));
    let node = Some((id, 0, VertexId::new(0, 0, 0)));
    assert!(snap_except(&mut s, c(-50., 0.5), Some((id, 0))).is_none());
    hit(snap_vertex_except(&mut s, c(-50., 0.5), node), c(-50., 0.), "edge");
    assert!(snap_vertex_except(&mut s, c(0.5, 50.), node).is_none(), "the moving node's edge remains excluded");
    s.map_mut().editing.snapping.vertex = true;
    s.map_mut().editing.snapping.edge = false;
    hit(snap_vertex_except(&mut s, c(0.2, 0.1), node), c(0., 0.), "vertex");
}

/// Repeated coordinates within a line are distinct addresses, and a nonincident edge is eligible.
#[test]
fn repeated_coordinates_in_one_part_do_not_exclude_other_nodes() {
    let (mut s, id) = fixture(Geometry::Line(vec![vec![c(0., 0.), c(100., 0.), c(0., 0.), c(0., 100.)]]));
    let node = Some((id, 0, VertexId::new(0, 0, 0)));
    hit(snap_vertex_except(&mut s, c(0.5, 50.), node), c(0., 50.), "edge");
    hit(snap_vertex_except(&mut s, c(50., 0.5), node), c(50., 0.), "edge");
}

/// A polygon's first node and its stored closing copy exclude both incident edges, but neither
/// another polygon's coincident node nor a hole's unrelated boundary is removed.
#[test]
fn polygon_closing_aliases_and_hole_addresses_stay_local() {
    let outer = vec![c(0., 0.), c(100., 0.), c(100., 100.), c(0., 100.), c(0., 0.)];
    let hole = vec![c(20., 20.), c(40., 20.), c(40., 40.), c(20., 40.), c(20., 20.)];
    let other = vec![c(0., 0.), c(-100., 0.), c(-100., -100.), c(0., -100.), c(0., 0.)];
    let (mut s, id) = fixture(Geometry::Polygon(vec![vec![outer, hole], vec![other]]));
    let outer_node = Some((id, 0, VertexId::new(0, 0, 0)));
    assert!(snap_vertex_except(&mut s, c(50., 0.5), outer_node).is_none());
    assert!(snap_vertex_except(&mut s, c(0.5, 50.), outer_node).is_none());
    hit(snap_vertex_except(&mut s, c(30., 20.5), outer_node), c(30., 20.), "edge");
    let hole_node = Some((id, 0, VertexId::new(0, 1, 0)));
    assert!(snap_vertex_except(&mut s, c(30., 20.5), hole_node).is_none());
    hit(snap_vertex_except(&mut s, c(50., 0.5), hole_node), c(50., 0.), "edge");
    s.map_mut().editing.snapping.vertex = true;
    s.map_mut().editing.snapping.edge = false;
    hit(snap_vertex_except(&mut s, c(0.2, 0.1), outer_node), c(0., 0.), "vertex");
    let geometry = s.map().layer(id).unwrap().dataset.features[0].geometry.as_ref().unwrap();
    let moved = geometry.move_vertex(VertexId::new(0, 0, 0), c(-5., -5.)).unwrap();
    let Geometry::Polygon(polygons) = moved else { panic!("polygon") };
    assert_eq!(polygons[0][0].first(), Some(&c(-5., -5.)));
    assert_eq!(polygons[0][0].first(), polygons[0][0].last());
    assert_eq!(polygons[0][1].first(), Some(&c(20., 20.)));
}

/// Unlike polygon closure, a closed line has two independent end addresses. The unchanged
/// edit command moves only the requested end, and still makes one undoable edit.
#[test]
fn closed_line_endpoints_keep_the_upstream_edit_contract() {
    let (mut s, id) = fixture(Geometry::Line(vec![vec![c(0., 0.), c(100., 0.), c(100., 100.), c(0., 100.), c(0., 0.)]]));
    s.map_mut().editing.snapping = Snapping { edge: false, vertex: false, end: true, ..s.map().editing.snapping };
    for index in [0, 4] {
        hit(snap_vertex_except(&mut s, c(0.2, 0.1), Some((id, 0, VertexId::new(0, 0, index)))), c(0., 0.), "end");
    }
    s.clear_history();
    let at = c(-20., 0.);
    s.run("edit.vertex", &json!({"layer":id,"row":0,"action":"move","index":0,"lon":at.x,"lat":at.y})).unwrap();
    let geometry = s.map().layer(id).unwrap().dataset.features[0].geometry.as_ref().unwrap();
    assert_eq!(geometry.vertex(VertexId::new(0, 0, 0)), Some(at));
    assert_eq!(geometry.vertex(VertexId::new(0, 0, 4)), Some(c(0., 0.)));
    assert_eq!(s.undo_depth(), 1);
    s.run("edit.undo", &json!(null)).unwrap();
    let geometry = s.map().layer(id).unwrap().dataset.features[0].geometry.as_ref().unwrap();
    assert_eq!(geometry.vertex(VertexId::new(0, 0, 0)), Some(c(0., 0.)));
}

/// The existing point-before-edge choice, type switches, tolerance and disable switch survive.
#[test]
fn switches_tolerance_and_point_priority_remain_unchanged() {
    let (mut s, id) =
        fixture(Geometry::Line(vec![vec![c(0., 0.), c(0., 100.)], vec![c(100., 0.), c(100., 100.)], vec![c(80., 0.2), c(120., 0.2)]]));
    let node = Some((id, 0, VertexId::new(0, 0, 0)));
    s.map_mut().editing.snapping.vertex = true;
    hit(snap_vertex_except(&mut s, c(99., 0.), node), c(100., 0.), "vertex");
    s.map_mut().editing.snapping.vertex = false;
    hit(snap_vertex_except(&mut s, c(99., 0.), node), c(99., 0.2), "edge");
    s.map_mut().editing.snapping.edge = false;
    s.map_mut().editing.snapping.end = true;
    hit(snap_vertex_except(&mut s, c(98.01, 0.), node), c(100., 0.), "end");
    assert!(snap_vertex_except(&mut s, c(97.99, 0.), node).is_none());
    s.map_mut().editing.snapping.end = false;
    assert!(snap_vertex_except(&mut s, c(99., 0.), node).is_none());
    s.map_mut().editing.snapping.enabled = false;
    assert!(snap_vertex_except(&mut s, c(100., 0.), node).is_none());
}

/// No exclusion, feature exclusion and the existing Point/MultiPoint policy are unchanged.
/// Invalid source addresses are ignored and cannot mutate the project or add undo entries.
#[test]
fn legacy_policy_and_invalid_addresses_are_preserved() {
    for geometry in [Geometry::Point(c(0., 0.)), Geometry::MultiPoint(vec![c(0., 0.)]), Geometry::Line(vec![vec![c(0., 0.), c(100., 0.)]])]
    {
        let (mut s, id) = fixture(geometry);
        let expected = snap_except(&mut s, c(0., 0.), None);
        let before = serde_json::to_value(&s.project).unwrap();
        let revision = s.revision;
        for skip in [
            None,
            Some((u32::MAX, 0, VertexId::new(0, 0, 0))),
            Some((id, usize::MAX, VertexId::new(0, 0, 0))),
            Some((id, 0, VertexId::new(usize::MAX, usize::MAX, usize::MAX))),
        ] {
            assert_eq!(snap_vertex_except(&mut s, c(0., 0.), skip), expected);
        }
        assert!(snap_except(&mut s, c(0., 0.), Some((id, 0))).is_none());
        assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
        assert_eq!(s.revision, revision);
        assert_eq!(s.undo_depth(), 0);
    }
}

/// The addressed row keeps its place in the original layer/row tie order; hidden layers stay out.
#[test]
fn source_row_order_and_visibility_still_choose_the_first_tie() {
    let (mut s, id) = fixture(Geometry::Line(vec![vec![c(100., 100.), c(-1., 0.)]]));
    let layer = s.map_mut().layer_mut(id).unwrap();
    layer.dataset.features.push(Feature { id: 2, geometry: Some(Geometry::Line(vec![vec![c(200., 100.), c(1., 0.)]])), attrs: vec![] });
    s.map_mut().editing.snapping.vertex = true;
    s.map_mut().editing.snapping.edge = false;
    hit(snap_vertex_except(&mut s, c(0., 0.), Some((id, 1, VertexId::new(0, 0, 0)))), c(-1., 0.), "vertex");
    hit(snap_vertex_except(&mut s, c(0., 0.), Some((id, 0, VertexId::new(0, 0, 0)))), c(-1., 0.), "vertex");
    s.map_mut().layer_mut(id).unwrap().visible = false;
    assert!(snap_vertex_except(&mut s, c(0., 0.), Some((id, 0, VertexId::new(0, 0, 0)))).is_none());
}

/// World and regional map projections keep source identities and inverse-projected targets.
#[test]
fn world_and_regional_projections_use_source_nodes() {
    for code in ["equirectangular", "EPSG:4549"] {
        let at = Coord::new(120.001, 31.);
        let (mut s, id) =
            fixture(Geometry::Line(vec![vec![Coord::new(120., 31.), Coord::new(120., 31.01)], vec![at, Coord::new(120.001, 31.01)]]));
        s.map_mut().projection = Projection::from_code(code).unwrap();
        s.map_mut().camera.center = s.map().to_map(at).unwrap();
        s.map_mut().camera.res = 1.0;
        s.map_mut().editing.snapping.vertex = true;
        s.map_mut().editing.snapping.edge = false;
        let node = Some((id, 0, VertexId::new(0, 0, 0)));
        hit(snap_vertex_except(&mut s, at, node), at, "vertex");
        assert!(snap_vertex_except(&mut s, Coord::new(120., 31.), node).is_none());
    }
}

/// A world-edge cut adds rendered parts but cannot renumber source exclusions, invent node
/// targets at cut intersections, or create an edge across the middle of the world.
#[test]
fn projection_cuts_do_not_supply_source_addresses_or_bridge_the_world() {
    let (mut s, id) = fixture(Geometry::Line(vec![
        vec![Coord::new(80., -20.), Coord::new(100., -20.)],
        vec![Coord::new(-100., 20.), Coord::new(-110., 20.), Coord::new(-110., 30.)],
    ]));
    s.map_mut().projection = Projection::from_code("mollweide +lon_0=-90").unwrap();
    s.map_mut().camera.res = 1000.;
    s.map_mut().editing.snapping.vertex = true;
    s.map_mut().editing.snapping.edge = false;
    let node = Some((id, 0, VertexId::new(1, 0, 0)));
    hit(snap_vertex_except(&mut s, Coord::new(-110., 20.), node), Coord::new(-110., 20.), "vertex");
    assert!(snap_vertex_except(&mut s, Coord::new(-100., 20.), node).is_none());
    assert!(snap_vertex_except(&mut s, Coord::new(90., -20.), node).is_none());
    let Geometry::Line(parts) = s.map().layer(id).unwrap().projected_cache().get(0).unwrap() else { panic!("line") };
    assert!(parts.len() > 2, "the fixture must actually split at the world edge");
    s.map_mut().editing.snapping.vertex = false;
    s.map_mut().editing.snapping.edge = true;
    assert!(snap_vertex_except(&mut s, Coord::new(-90., -20.), node).is_none());
    hit(snap_vertex_except(&mut s, Coord::new(89., -20.), node), Coord::new(89., -20.), "edge");
    // This remains an ordinary in-memory feature layer.
    assert_eq!(s.map().layer(id).unwrap().geometry_type(), GeometryType::Polyline);
}

/// A vertex snapped to is the feature's own place, bit for bit, not the map's place for it brought
/// back, on Web Mercator and on a world projection: a vertex dragged onto another feature's lands
/// exactly on it, so the two share the place.
#[test]
fn a_snapped_vertex_is_the_stored_place_exactly() {
    let target = Coord::new(7.123456789012345, 46.98765432109876);
    let line = Geometry::Line(vec![vec![Coord::new(7.0, 46.9), target], vec![Coord::new(7.2, 47.1), Coord::new(7.3, 47.2)]]);
    for projection in [None, Some("robinson")] {
        let (mut s, id) = fixture(line.clone());
        if let Some(code) = projection {
            s.map_mut().projection = Projection::from_code(code).unwrap();
        }
        s.map_mut().camera.res = 1e-3;
        s.map_mut().editing.snapping = Snapping { enabled: true, tolerance_px: 50.0, vertex: true, edge: true, end: true };
        let near = Coord::new(target.x + 1e-8, target.y - 1e-8);
        let (at, kind) = snap_except(&mut s, near, None).expect("snaps");
        assert_eq!(kind, "end", "{projection:?}");
        assert_eq!((at.x.to_bits(), at.y.to_bits()), (target.x.to_bits(), target.y.to_bits()), "{projection:?}: {at:?}");
        // The other part's first vertex dragged onto it.
        s.run("edit.vertex", &json!({"layer": id, "row": 0, "part": 1, "action": "move", "index": 0, "lon": at.x, "lat": at.y})).unwrap();
        let Some(Geometry::Line(parts)) = &s.map().layer(id).unwrap().dataset.features[0].geometry else { panic!("a line") };
        assert_eq!(parts[1][0], parts[0][1], "{projection:?}");
    }
}

/// A stored vertex past 180° (a shape drawn across the antimeridian) is snapped to as stored,
/// where the map shows it: past the edge on Web Mercator, one turn round on a world projection.
#[test]
fn a_vertex_past_the_antimeridian_is_snapped_to_as_stored() {
    let places =
        (0..12).map(|i| Coord::new(180.0 + 7.123456789012345 * f64::from(i % 5 + 1) / 3.0, -60.0 + 11.987654321098765 * f64::from(i)));
    for (past, projection) in places.flat_map(|p| [(p, None), (p, Some("robinson"))]) {
        let (mut s, _) = fixture(Geometry::Line(vec![vec![Coord::new(170.0, 10.0), past]]));
        if let Some(code) = projection {
            s.map_mut().projection = Projection::from_code(code).unwrap();
        }
        s.map_mut().camera.res = 1e-3;
        s.map_mut().editing.snapping = Snapping { enabled: true, tolerance_px: 50.0, vertex: true, edge: true, end: true };
        let hits: Vec<_> =
            [past.x, past.x - 360.0].into_iter().filter_map(|x| snap_except(&mut s, Coord::new(x + 1e-8, past.y), None)).collect();
        assert!(!hits.is_empty(), "{projection:?} {past:?}");
        for (at, kind) in hits {
            assert_eq!(kind, "end");
            assert_eq!((at.x.to_bits(), at.y.to_bits()), (past.x.to_bits(), past.y.to_bits()), "{projection:?}: {at:?}");
        }
    }
}

/// Of two stored places a turn apart, the one the map draws at the hit is snapped to: on Web
/// Mercator, (0°, 0°) is drawn there and 360° a world away.
#[test]
fn a_place_one_turn_away_is_another_vertex() {
    let (mut s, _) = fixture(Geometry::MultiPoint(vec![Coord::new(360.0, 0.0), Coord::new(0.0, 0.0)]));
    s.map_mut().camera.res = 1e-3;
    s.map_mut().editing.snapping = Snapping { enabled: true, tolerance_px: 50.0, vertex: true, edge: true, end: true };
    let (at, kind) = snap_except(&mut s, Coord::new(1e-8, 1e-8), None).expect("snaps");
    assert_eq!((at, kind), (Coord::new(0.0, 0.0), "vertex"));
}
