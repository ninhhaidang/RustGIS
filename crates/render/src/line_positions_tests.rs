use super::*;
use crate::tests::label;
use rustgis_data::{Dataset, Field, FieldType, Value};
use rustgis_map::LabelPlacement;
use std::sync::Arc;

fn curved(line: &[(f32, f32)]) -> Candidate {
    let line = Arc::new(line_labels::Line::new(line).unwrap());
    let positions = line_labels::positions(line.length(), 40.0, 10.0);
    let (x, y) = line.anchor(line.length() / 2.0).unwrap();
    Candidate {
        label: PlacedLabel { x, y, ..label("road", x, None, 0.0) },
        priority: 1.0,
        row: 0,
        key: (1, 0, 0, 0),
        alternatives: Vec::new(),
        line: Some(LinePlaces::default()),
        curves: Some(label_places::Curves::new(
            Arc::new(label_places::CurveSpec {
                line,
                chars: "road".chars().map(|c| c.to_string()).collect(),
                advances: vec![10.0; 4],
                gap: 0.0,
                overrun: 10.0,
            }),
            positions,
            None,
        )),
    }
}

#[test]
fn curves_are_laid_out_only_until_a_place_fits() {
    let c = curved(&[(20.0, 100.0), (380.0, 100.0)]);
    label_places::LAYOUTS.with(|n| n.set(0));
    let p = place_labels(vec![c.clone()], Vec::new(), 400.0, 200.0, false);
    assert_eq!(p.keys[0].1, 0);
    label_places::LAYOUTS.with(|n| assert_eq!(n.get(), 1));
    let blocker = PlacedLabel { y: 100.0, ..label("BLOCKER", 200.0, None, 0.0) };
    label_places::LAYOUTS.with(|n| n.set(0));
    let p = place_labels(vec![c], vec![blocker], 400.0, 200.0, false);
    let won = p.keys[0].1;
    assert!(won > 0 && won < 8);
    label_places::LAYOUTS.with(|n| assert_eq!(n.get(), usize::from(won) + 1));
    assert_eq!(p.labels[1].glyphs.len(), 4);
}

#[test]
fn a_sharp_middle_tries_a_straight_stretch_and_a_lock_replays_that_index() {
    let c = curved(&[(0.0, 100.0), (200.0, 100.0), (200.0, 300.0)]);
    assert!(c.curves.as_ref().unwrap().place(0, &mut c.label.clone()).is_none());
    let p = place_labels(vec![c.clone()], Vec::new(), 400.0, 400.0, false);
    let won = usize::from(p.keys[0].1);
    assert!(won > 0);
    let mut locked = c;
    locked.curves.as_mut().unwrap().locked = Some(won);
    // Shift the map projection: the same place and its characters move together.
    turn_candidate(&mut locked, |x, y| Some((x + 12.0, y + 7.0)));
    label_places::LAYOUTS.with(|n| n.set(0));
    let q = place_labels(vec![locked], Vec::new(), 450.0, 450.0, true);
    assert_eq!(usize::from(q.keys[0].1), won);
    assert!((q.labels[0].x - p.labels[0].x - 12.0).abs() < 1e-4);
    assert!((q.labels[0].y - p.labels[0].y - 7.0).abs() < 1e-4);
    assert_eq!(q.labels[0].glyphs, p.labels[0].glyphs);
    label_places::LAYOUTS.with(|n| assert_eq!(n.get(), 1));
}

#[test]
fn all_curves_taken_are_unplaced_once_at_the_middle() {
    let c = curved(&[(20.0, 100.0), (380.0, 100.0)]);
    let blocker = PlacedLabel { y: 100.0, size: 200.0, ..label("XXXXXXXXXXXXXXXXXXXX", 200.0, None, 0.0) };
    let p = place_labels(vec![c], vec![blocker], 400.0, 200.0, false);
    assert_eq!(p.labels.len(), 1);
    assert_eq!(p.unplaced.len(), 1);
    assert_eq!((p.unplaced[0].x, p.unplaced[0].y), (200.0, 100.0));
}

#[test]
fn turned_and_tilted_curves_defer_their_glyphs_until_placement() {
    let c = curved(&[(20.0, 100.0), (380.0, 100.0)]);
    for tilt in [false, true] {
        label_places::LAYOUTS.with(|n| n.set(0));
        // A 45-degree turn: each glyph follows its own projected baseline.
        let at = |x, y| Some((200.0 + (x - y) * std::f32::consts::FRAC_1_SQRT_2, (x + y) * std::f32::consts::FRAC_1_SQRT_2));
        let mut c = c.clone();
        if tilt {
            c = tilt_candidate(c, &at).unwrap();
        } else {
            turn_candidate(&mut c, at);
        }
        label_places::LAYOUTS.with(|n| assert_eq!(n.get(), 0));
        let p = place_labels(vec![c], Vec::new(), 400.0, 400.0, false);
        let l = &p.labels[0];
        assert!((l.x - 270.7107).abs() < 1e-3 && (l.y - 212.132).abs() < 1e-3);
        assert!(l.glyphs.iter().all(|g| (g.angle - std::f32::consts::FRAC_PI_4).abs() < 1e-4));
        assert!(l.glyphs.windows(2).all(|w| w[1].dy > w[0].dy));
        label_places::LAYOUTS.with(|n| assert_eq!(n.get(), 1));
    }
}

fn map_with_line(blocked: bool) -> (Map, u32, View) {
    let mut map = Map::new("lines");
    map.basemap = rustgis_map::Basemap::Blank;
    let mut ds = Dataset::new("roads", GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
    ds.push(Some(Geometry::Line(vec![vec![Coord::new(-1.0, 0.0), Coord::new(1.0, 0.0)]])), vec![Value::Text("road".into())]);
    let id = map.add_dataset(ds);
    let layer = map.layer_mut(id).unwrap();
    layer.labels.enabled = true;
    layer.labels.default.field = "name".into();
    layer.labels.default.size = 10.0;
    layer.labels.default.placement = LabelPlacement::Parallel;
    if blocked {
        let mut ds = Dataset::new("blocker", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text("BLOCKER".into())]);
        let blocker = map.add_dataset(ds);
        let l = map.layer_mut(blocker).unwrap();
        l.labels.enabled = true;
        l.labels.default.field = "name".into();
        l.labels.default.placement = LabelPlacement::Center;
    }
    let camera = Camera { center: Coord::new(0.0, 0.0), res: 750.0, bearing: 0.0, pitch: 0.0 };
    map.camera = camera;
    (map, id, View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false })
}

#[test]
fn rendering_retries_only_opted_in_nonrepeated_line_labels() {
    for placement in [LabelPlacement::Parallel, LabelPlacement::Curved] {
        let (mut map, id, view) = map_with_line(true);
        map.layer_mut(id).unwrap().labels.default.placement = placement;
        let before = render(&mut map, &view).unwrap();
        assert!(before.labels.iter().all(|l| l.text != "road"));
        assert!(before.labels.iter().any(|l| l.text == "BLOCKER"), "{:?}", before.labels);
        let exported = render_complete(&mut map, &view).unwrap();
        assert!(exported.labels.iter().any(|l| l.text == "BLOCKER"), "export labels: {:?}", exported.labels);
        map.layer_mut(id).unwrap().labels.default.other_positions = true;
        for (bearing, pitch) in [(0.0, 0.0), (35.0, 0.0), (20.0, 30.0)] {
            let view = View { camera: Camera { bearing, pitch, ..view.camera }, ..view };
            let r = render(&mut map, &view).unwrap();
            assert!(r.labels.iter().any(|l| l.text == "road"), "{placement:?}, bearing {bearing}, pitch {pitch}");
        }
        // A saved lock selects the same alternative, including after a small pan.
        map.label_display.locked = true;
        let first = render(&mut map, &view).unwrap();
        let key = map.label_lock.as_ref().unwrap().iter().find(|(k, _)| k.0 == id).copied().unwrap();
        assert!(key.1 > 0);
        let panned = View { camera: Camera { center: Coord::new(7500.0, 0.0), ..view.camera }, ..view };
        let next = render(&mut map, &panned).unwrap();
        let x = |r: &Rendered| r.labels.iter().find(|l| l.text == "road").unwrap().x;
        assert!((x(&next) - x(&first) + 10.0).abs() < 0.05);
        assert!(map.label_lock.as_ref().unwrap().contains(&key));

        // Turning the option on changes neither an unblocked middle nor repeats.
        for repeat in [None, Some(45.0)] {
            let (mut clean, id, view) = map_with_line(false);
            let c = &mut clean.layer_mut(id).unwrap().labels.default;
            c.placement = placement;
            c.repeat = repeat;
            let before = render(&mut clean, &view).unwrap();
            clean.layer_mut(id).unwrap().labels.default.other_positions = true;
            let after = render(&mut clean, &view).unwrap();
            assert_eq!(before.labels, after.labels);
            assert_eq!(before.pixmap.data(), after.pixmap.data());
        }
    }
}

#[test]
fn dense_curved_labels_have_a_bounded_lazy_layout_budget() {
    label_places::LAYOUTS.with(|n| n.set(0));
    let c = curved(&[(20.0, 100.0), (380.0, 100.0)]);
    let candidates = (0..4000)
        .map(|i| {
            let mut c = c.clone();
            c.key.1 = i;
            c.priority = -(i as f64);
            c
        })
        .collect::<Vec<_>>();
    let p = place_labels(candidates, Vec::new(), 400.0, 200.0, false);
    assert!(!p.labels.is_empty());
    label_places::LAYOUTS.with(|n| assert!(n.get() <= 4000 * 8));
}

#[test]
fn terrain_and_roofs_translate_ready_and_deferred_places_without_changing_glyph_offsets() {
    let camera = Camera { center: Coord::new(0.0, 0.0), res: 600.0, bearing: 0.0, pitch: 45.0 };
    let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
    let mut hill = crate::tests::hill_map();
    hill.view.terrain =
        Some(rustgis_map::view::Terrain { source: rustgis_map::view::TerrainSource::Layer("dem".into()), exaggeration: 2.0 });
    let ground = terrain::Ground::of(&hill, &camera).unwrap();
    let mut blocks = Map::new("roofs");
    let mut ds = Dataset::new("block", GeometryType::Polygon, vec![]);
    ds.push(
        Some(Geometry::Polygon(vec![vec![vec![
            Coord::new(-0.1, -0.1),
            Coord::new(0.1, -0.1),
            Coord::new(0.0, 0.1),
            Coord::new(-0.1, -0.1),
        ]]])),
        vec![],
    );
    let id = blocks.add_dataset(ds);
    blocks.layer_mut(id).unwrap().extrusion = Some(rustgis_map::effects::Extrusion {
        field: None,
        height: 6000.0,
        factor: 1.0,
        face_culling: true,
        lighting: true,
        shadows: true,
    });
    for roof in [false, true] {
        let mut c = curved(&[(20.0, 150.0), (380.0, 150.0)]);
        c.key.0 = id;
        c.key.1 = blocks.layer(id).unwrap().dataset.features[0].id;
        let before = c.curves.as_ref().unwrap().place(0, &mut c.label.clone()).unwrap();
        c.alternatives.push(before.clone());
        let mut distant = before.clone();
        distant.y += 40.0;
        c.alternatives.push(distant.clone());
        let mut probe = c.clone();
        probe.curves = None;
        probe.alternatives.clear();
        (probe.label.x, probe.label.y) = (distant.x, distant.y);
        let mut candidates = vec![c, probe];
        if roof {
            raise_labels(&blocks, &view, &mut candidates);
        } else {
            lift_labels(&view, &ground, &mut candidates);
        }
        let c = &candidates[0];
        let after = c.curves.as_ref().unwrap().place(0, &mut c.label.clone()).unwrap();
        assert!(after.y < before.y - 0.1, "roof {roof}");
        assert_eq!(after.glyphs, before.glyphs);
        assert_eq!(c.alternatives[0], after);
        assert!((c.alternatives[1].x - candidates[1].label.x).abs() < 1e-4);
        assert!((c.alternatives[1].y - candidates[1].label.y).abs() < 1e-4);
    }
}

#[test]
fn terrain_lifts_line_places_at_their_own_height() {
    let camera = Camera { center: Coord::new(0.0, 0.0), res: 600.0, bearing: 0.0, pitch: 45.0 };
    let view = View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false };
    let mut map = crate::tests::hill_map();
    map.view.terrain =
        Some(rustgis_map::view::Terrain { source: rustgis_map::view::TerrainSource::Layer("dem".into()), exaggeration: 2.0 });
    let ground = terrain::Ground::of(&map, &camera).unwrap();
    let mut c = curved(&[(30.0, 150.0), (370.0, 150.0)]);
    let before: Vec<_> = (0..c.curves.as_ref().unwrap().positions.len())
        .filter_map(|i| c.curves.as_ref().unwrap().place(i, &mut c.label.clone()).map(|p| (i, p)))
        .collect();
    c.alternatives = before.iter().map(|(_, p)| p.clone()).collect();
    let mut candidates = vec![c];
    lift_labels(&view, &ground, &mut candidates);
    let c = &candidates[0];
    let mut shifts = Vec::new();
    for ((i, prior), ready) in before.iter().zip(&c.alternatives) {
        let after = c.curves.as_ref().unwrap().place(*i, &mut c.label.clone()).unwrap();
        let m = camera.try_to_map(f64::from(prior.x), f64::from(prior.y), view.width, view.height).unwrap();
        let (u, v) = camera.ground_px(m);
        let (x, y) = camera.project_raised(u, v, ground.up_px(m, &camera), view.width, view.height).unwrap();
        assert!((after.x - x as f32).abs() < 0.01 && (after.y - y as f32).abs() < 0.01);
        assert_eq!(&after, ready);
        assert_eq!(after.glyphs, prior.glyphs);
        shifts.push(after.y - prior.y);
    }
    assert!(shifts.iter().any(|v| (v - shifts[0]).abs() > 0.1), "places must sample different heights: {shifts:?}");
}

#[test]
fn a_rendered_curved_label_can_leave_a_sharp_middle_bend() {
    let (mut map, id, view) = map_with_line(false);
    let layer = map.layer_mut(id).unwrap();
    layer.dataset.features[0].geometry =
        Some(Geometry::Line(vec![vec![Coord::new(-1.0, -1.0), Coord::new(1.0, -1.0), Coord::new(1.0, 1.0)]]));
    layer.touch_geometry();
    layer.labels.default.placement = LabelPlacement::Curved;
    assert!(render(&mut map, &view).unwrap().labels.is_empty());
    map.layer_mut(id).unwrap().labels.default.other_positions = true;
    let r = render(&mut map, &view).unwrap();
    assert_eq!(r.labels.len(), 1);
    assert!(!r.labels[0].glyphs.is_empty());
}

#[test]
fn a_projected_anchor_does_not_keep_unprojectable_characters() {
    let c = curved(&[(20.0, 100.0), (380.0, 100.0)]);
    let c = tilt_candidate(c, &|x, y| (x > 195.0).then_some((x, y))).unwrap();
    let p = place_labels(vec![c], Vec::new(), 400.0, 200.0, false);
    assert_eq!(p.keys[0].1, 2, "middle anchor is visible, but its leftmost glyphs are not");
    assert!(p.labels[0].glyphs.iter().all(|g| p.labels[0].x + g.dx > 195.0));
}

#[test]
fn a_nearer_bands_unlayoutable_curve_does_not_claim_the_feature_key() {
    let mut bad = curved(&[(100.0, 100.0), (110.0, 100.0)]);
    // No overrun: four letters cannot fit on this band's short stretch.
    Arc::get_mut(&mut bad.curves.as_mut().unwrap().spec).unwrap().overrun = 0.0;
    let good = curved(&[(20.0, 100.0), (380.0, 100.0)]);
    let mut map = Map::new("bands");
    map.label_display.show_unplaced = true;
    let view = View {
        width: 400.0,
        height: 200.0,
        pixels_per_point: 1.0,
        camera: Camera { center: Coord::new(0.0, 0.0), res: 1000.0, bearing: 0.0, pitch: 30.0 },
        sky: false,
    };
    let p = place_tilted_labels(&map, &view, vec![bad.clone(), good.clone(), good.clone()], None);
    assert_eq!(p.labels.len(), 1);
    assert_eq!(p.labels[0].glyphs.len(), 4);
    assert!(p.unplaced.is_empty(), "a successful later band replaces the failed preview");
    // Every physical camera band remains available, with a hard bound on excess siblings.
    let max = rustgis_map::camera::MAX_TILT_BANDS;
    let candidates = std::iter::repeat_n(bad.clone(), max - 1).chain(std::iter::once(good.clone())).collect();
    let p = place_tilted_labels(&map, &view, candidates, None);
    assert_eq!(p.labels.len(), 1);
    label_places::LAYOUTS.with(|n| n.set(0));
    let candidates = std::iter::repeat_n(bad, 20_000).chain(std::iter::once(good)).collect();
    let p = place_tilted_labels(&map, &view, candidates, None);
    assert!(p.labels.is_empty());
    assert_eq!(p.unplaced.len(), 1);
    label_places::LAYOUTS.with(|n| assert!(n.get() <= max * 8));
}

#[test]
fn locked_line_anchors_survive_changes_to_the_visible_stretch() {
    for placement in [LabelPlacement::Parallel, LabelPlacement::Curved] {
        let (mut map, id, view) = map_with_line(true);
        let layer = map.layer_mut(id).unwrap();
        layer.dataset.features[0].geometry = Some(Geometry::Line(vec![vec![Coord::new(-3.0, 0.0), Coord::new(3.0, 0.0)]]));
        layer.touch_geometry();
        layer.labels.default.placement = placement;
        layer.labels.default.other_positions = true;
        map.label_display.locked = true;
        let first = render(&mut map, &view).unwrap();
        let first = first.labels.iter().find(|l| l.text == "road").unwrap();
        assert_eq!(map.label_lock_line_anchors.len(), 1);
        let moved = View { camera: Camera { center: Coord::new(60_000.0, 0.0), ..view.camera }, ..view };
        let next = render(&mut map, &moved).unwrap();
        let next = next.labels.iter().find(|l| l.text == "road").unwrap();
        assert!((next.x - first.x + 80.0).abs() < 0.1, "{placement:?}: {} -> {}", first.x, next.x);
        let zoom = View { camera: Camera { res: view.camera.res * 2.0, ..view.camera }, ..view };
        let zoomed = render(&mut map, &zoom).unwrap();
        let zoomed = zoomed.labels.iter().find(|l| l.text == "road").unwrap();
        assert!((zoomed.x - (200.0 + (first.x - 200.0) / 2.0)).abs() < 0.1);
        let outside = View { camera: Camera { center: Coord::new(300_000.0, 0.0), ..view.camera }, ..view };
        assert!(render(&mut map, &outside).unwrap().labels.iter().all(|l| l.text != "road"), "a lock never slides back into view");
        assert!(serde_json::to_value(&map).unwrap().get("label_lock_line_anchors").is_none());
    }
}

#[test]
fn a_later_parallel_band_can_supply_a_free_place() {
    let map = Map::new("bands");
    let view = View {
        width: 400.0,
        height: 200.0,
        pixels_per_point: 1.0,
        camera: Camera { center: Coord::new(0.0, 0.0), res: 1000.0, bearing: 0.0, pitch: 30.0 },
        sky: false,
    };
    let mut first = Candidate::from((PlacedLabel { y: 100.0, ..label("road", 100.0, None, 0.0) }, 1.0, (1, 0, 0, 0)));
    first.line = Some(LinePlaces::default());
    first.alternatives.push((110.0, 100.0).into());
    let mut second = first.clone();
    second.label.x = 300.0;
    second.alternatives = vec![(320.0, 100.0).into()];
    let blocker = Candidate::from((PlacedLabel { y: 100.0, ..label("BLOCKER", 100.0, None, 0.0) }, 9.0, (u32::MAX, 0, 0, 0)));
    let p = place_tilted_labels(&map, &view, vec![blocker, first, second], None);
    assert_eq!(p.labels.iter().filter(|l| l.text == "road").count(), 1);
    assert_eq!(p.labels.iter().find(|l| l.text == "road").unwrap().x, 300.0);
    assert!(p.unplaced.is_empty());
}

#[test]
fn turned_candidates_skip_failed_alternatives_without_renumbering_slots() {
    let mut c = Candidate::from((label("road", 100.0, None, 0.0), 1.0, (1, 0, 0, 0)));
    c.alternatives = vec![(200.0, 20.0).into(), (300.0, 20.0).into()];
    turn_candidate(&mut c, |x, y| (x != 200.0).then_some((x + 50.0, y)));
    let p = place_labels(vec![c], vec![label("BLOCKER", 150.0, None, 0.0)], 400.0, 200.0, false);
    assert_eq!(p.keys[0].1, 2);
    assert_eq!(p.labels[1].x, 350.0);
}

#[test]
fn end_candidates_fit_and_nearest_anchors_keep_overrun() {
    for total in [1.0, 10.0, 100.0, 1_000_000.0] {
        let line = line_labels::Line::new(&[(0.0, 0.0), (total, 0.0)]).unwrap();
        for width in [0.1, 1.0, 8.0, 20.0] {
            for overrun in [0.0, 0.5, 12.0] {
                if width > total + 2.0 * overrun {
                    continue;
                }
                let positions = line_labels::positions(total, width, overrun);
                assert!(positions.len() <= 8);
                assert!(positions.into_iter().all(|at| line.parallel(width, overrun, Some(at)).is_some()));
            }
        }
        assert!((line.nearest((-2.0, 0.0), 2.0).unwrap().1 + 2.0).abs() < 1e-4);
    }
}

#[test]
fn locked_multipart_lines_do_not_follow_a_distant_parts_extension() {
    let view = View {
        width: 400.0,
        height: 200.0,
        pixels_per_point: 1.0,
        camera: Camera { center: Coord::new(0.0, 0.0), res: 1.0, bearing: 0.0, pitch: 0.0 },
        sky: false,
    };
    let ctx = Ctx {
        view: &view,
        ppp: 1.0,
        wireframe: false,
        cull: Extent::EMPTY,
        globe: None,
        sprites: Default::default(),
        bursts: Default::default(),
    };
    let parts = vec![vec![Coord::new(-100.0, 0.0), Coord::new(-90.0, 0.0)], vec![Coord::new(-10.0, 0.0), Coord::new(10.0, 0.0)]];
    let (line, at) = line_near_anchor(&ctx, &parts, (200.0, 100.0), 5.0).unwrap();
    assert!((line.length() - 20.0).abs() < 1e-4 && (at - 10.0).abs() < 1e-4);
}

#[test]
fn auxiliary_drawings_preserve_locked_line_anchors() {
    let (mut map, id, view) = map_with_line(true);
    map.layer_mut(id).unwrap().labels.default.other_positions = true;
    map.label_display.locked = true;
    render(&mut map, &view).unwrap();
    let lock = map.label_lock.clone();
    let anchors = map.label_lock_line_anchors.clone();
    assert!(!anchors.is_empty());
    let tilted = View { camera: Camera { pitch: 30.0, ..view.camera }, ..view };
    assert!(!render_tilt_drape(&mut map, &tilted, 0.0, 256).is_empty());
    assert_eq!(map.label_lock, lock);
    assert_eq!(map.label_lock_line_anchors, anchors);
    assert!(!render_globe_drape(&mut map, &view, &[Extent::new(-1.0, -1.0, 1.0, 1.0)], 256).is_empty());
    assert_eq!(map.label_lock, lock);
    assert_eq!(map.label_lock_line_anchors, anchors);
    assert!(layout::frame_coverage(&mut map, Coord::new(0.0, 0.0), 1_000_000.0, 100.0, 100.0, 32).is_some());
    assert_eq!(map.label_lock, lock);
    assert_eq!(map.label_lock_line_anchors, anchors);
}

#[test]
fn colliding_band_groups_share_a_frame_layout_budget() {
    let mut c = curved(&[(20.0, 100.0), (380.0, 100.0)]);
    c.line.as_mut().unwrap().deduplicate = true;
    let blocker = PlacedLabel { y: 100.0, size: 200.0, ..label("XXXXXXXXXXXXXXXXXXXX", 200.0, None, 0.0) };
    // Layout itself succeeds at all eight positions; collision is the reason for retrying.
    assert!(c.curves.as_ref().unwrap().place(0, &mut c.label.clone()).is_some());
    let candidates = (0..2600)
        .map(|i| {
            let mut c = c.clone();
            c.key.1 = i;
            let siblings = vec![c.clone(); rustgis_map::camera::MAX_TILT_BANDS - 1];
            c.line.as_mut().unwrap().fallbacks = siblings;
            c
        })
        .collect::<Vec<_>>();
    label_places::LAYOUTS.with(|n| n.set(0));
    let p = place_labels(candidates, vec![blocker], 400.0, 200.0, false);
    // The budget only limits alternatives: once spent, each remaining label still lays out its
    // own place (one attempt per band), so the total stays within budget plus those.
    let budget = 20_000 * 8;
    label_places::LAYOUTS.with(|n| assert!((budget..=budget + 2600 * rustgis_map::camera::MAX_TILT_BANDS).contains(&n.get())));
    assert_eq!(p.labels.len(), 1);
    assert_eq!(p.unplaced.len(), 2000);
}

#[test]
fn successful_band_previews_do_not_consume_the_unplaced_cap() {
    let mut bad = Candidate::from((PlacedLabel { y: 100.0, ..label("road", 100.0, None, 0.0) }, 1.0, (1, 0, 0, 0)));
    bad.line = Some(LinePlaces { deduplicate: true, ..Default::default() });
    let mut good = bad.clone();
    good.label.x = 300.0;
    good.label.collide.text_allow_overlap = true;
    good.label.collide.text_ignore_placement = true;
    let mut candidates = Vec::new();
    for i in 0..2000 {
        let mut c = bad.clone();
        c.key.1 = i;
        let mut sibling = good.clone();
        sibling.key = c.key;
        c.line.as_mut().unwrap().fallbacks.push(sibling);
        candidates.push(c);
    }
    bad.key.1 = 2000;
    candidates.push(bad);
    let blocker = PlacedLabel { y: 100.0, ..label("BLOCKER", 100.0, None, 0.0) };
    let p = place_labels(candidates, vec![blocker], 400.0, 200.0, false);
    assert_eq!(p.labels.len(), 2001);
    assert_eq!(p.unplaced.len(), 1);
}

#[test]
fn hidden_unplaced_previews_do_not_change_placement() {
    let candidates: Vec<_> =
        (0..3000).map(|i| Candidate::from((PlacedLabel { y: 100.0, ..label("road", 100.0, None, 0.0) }, 1.0, (1, i, 0, 0)))).collect();
    let blocker = PlacedLabel { y: 100.0, ..label("BLOCKER", 100.0, None, 0.0) };
    let shown = place_labels_with_previews(candidates.clone(), vec![blocker.clone()], 400.0, 200.0, false, true);
    let hidden = place_labels_with_previews(candidates, vec![blocker], 400.0, 200.0, false, false);
    assert_eq!(shown.unplaced.len(), 2000);
    assert!(hidden.unplaced.is_empty());
    assert_eq!(shown.labels, hidden.labels);
    assert_eq!(shown.keys, hidden.keys);
}

#[test]
fn a_place_without_a_back_projected_anchor_is_never_locked() {
    // Locking is requested (anchors present) but the free place has none: it cannot be replayed,
    // so it is not taken and no key is recorded. The next place with an anchor is.
    let mut c = Candidate::from((PlacedLabel { y: 100.0, ..label("road", 100.0, None, 0.0) }, 1.0, (1, 0, 0, 0)));
    c.alternatives = vec![LabelPlace::from((300.0, 100.0))];
    c.line = Some(LinePlaces { anchors: vec![None, Some(Coord::new(2.0, 0.0))], ..Default::default() });
    let placed = place_labels(vec![c.clone()], Vec::new(), 400.0, 200.0, false);
    assert_eq!(placed.keys, vec![((1, 0, 0, 0), 1)]);
    assert_eq!(placed.line_anchors, vec![((1, 0, 0, 0), Coord::new(2.0, 0.0))]);
    c.alternatives.clear();
    c.line.as_mut().unwrap().anchors = vec![None];
    let placed = place_labels(vec![c], Vec::new(), 400.0, 200.0, false);
    assert!(placed.keys.is_empty() && placed.line_anchors.is_empty());
}

#[test]
fn invalid_straight_slots_keep_their_matching_anchor() {
    let mut c = Candidate::from((PlacedLabel { y: 100.0, ..label("road", 100.0, None, 0.0) }, 1.0, (1, 0, 0, 0)));
    c.alternatives = vec![LabelPlace::from((f32::NAN, f32::NAN)), LabelPlace::from((300.0, 100.0))];
    c.line = Some(LinePlaces { anchors: vec![Some(Coord::new(0.0, 0.0)), None, Some(Coord::new(2.0, 0.0))], ..Default::default() });
    let blocker = PlacedLabel { y: 100.0, ..label("BLOCKER", 100.0, None, 0.0) };
    let placed = place_labels(vec![c], vec![blocker], 400.0, 200.0, false);
    assert_eq!(placed.keys, vec![((1, 0, 0, 0), 2)]);
    assert_eq!(placed.line_anchors, vec![((1, 0, 0, 0), Coord::new(2.0, 0.0))]);
}

#[test]
fn tilted_icons_turn_once_for_both_ready_and_deferred_labels() {
    let at = |x, y| Some((300.0 - y, x));
    for deferred in [false, true] {
        let mut c = curved(&[(20.0, 100.0), (380.0, 100.0)]);
        if !deferred {
            c.curves = None;
        }
        c.label.icon =
            Some(PlacedIcon { image: Arc::new(Pixmap::new(2, 2).unwrap()), x: 200.0, y: 100.0, w: 2.0, h: 2.0, angle: Some(0.0) });
        let c = tilt_candidate(c, &at).unwrap();
        let icon = c.label.icon.unwrap();
        assert!((icon.angle.unwrap() - std::f32::consts::FRAC_PI_2).abs() < 1e-4);
        assert_eq!((icon.x, icon.y), (200.0, 200.0));
    }
}

#[test]
fn an_unplaced_tilted_curve_uses_the_visible_anchors_own_direction() {
    let mut c = curved(&[(20.0, 100.0), (220.0, 100.0), (220.0, 300.0)]);
    let curves = c.curves.as_mut().unwrap();
    curves.positions = vec![200.0, 300.0];
    // The text cannot fit: retain a preview without laying out glyphs. The midpoint is
    // hidden and the next position is on a vertical segment with a different direction.
    Arc::get_mut(&mut curves.spec).unwrap().advances = vec![500.0; 4];
    label_places::LAYOUTS.with(|n| n.set(0));
    let c = tilt_candidate(c, &|x, y| (y > 150.0).then_some((x + (y - 200.0) * 0.25, y))).unwrap();
    label_places::LAYOUTS.with(|n| assert_eq!(n.get(), 0));
    let p = place_labels(vec![c], Vec::new(), 500.0, 400.0, false);
    assert!(p.labels.is_empty());
    assert_eq!(p.unplaced.len(), 1);
    let preview = &p.unplaced[0];
    assert_eq!((preview.x, preview.y), (220.0, 200.0));
    assert!((preview.angle.unwrap() - 1.0_f32.atan2(0.25)).abs() < 1e-4);
}

#[test]
fn an_invalid_locked_straight_slot_never_invents_a_middle_preview() {
    let (mut map, id, view) = map_with_line(false);
    map.layer_mut(id).unwrap().labels.default.other_positions = true;
    map.label_display.locked = true;
    map.label_display.show_unplaced = true;
    render(&mut map, &view).unwrap();
    assert!(!map.label_lock.as_ref().unwrap().is_empty());
    let anchors = map.label_lock_line_anchors.clone();
    assert!(!anchors.is_empty());
    // A stale or malformed lock cannot name a real place. There is no accurate preview
    // to draw, even when View Unplaced is on; the valid lock's geographic anchor stays put.
    map.label_lock.as_mut().unwrap()[0].1 = u8::MAX;
    let r = render(&mut map, &view).unwrap();
    assert!(r.labels.is_empty());
    assert!(r.unplaced.is_empty());
    assert_eq!(map.label_lock_line_anchors, anchors);
}

#[test]
fn a_locked_line_without_its_saved_anchor_does_not_slide() {
    let (mut map, id, view) = map_with_line(false);
    map.layer_mut(id).unwrap().labels.default.other_positions = true;
    map.label_display.locked = true;
    render(&mut map, &view).unwrap();
    assert!(!map.label_lock.as_ref().unwrap().is_empty());
    assert!(!map.label_lock_line_anchors.is_empty());
    // The anchor is gone (a failed back-projection): the label is not re-placed on another stretch.
    map.label_lock_line_anchors.clear();
    let r = render(&mut map, &view).unwrap();
    assert!(r.labels.is_empty());
}

#[test]
fn an_invalid_locked_curved_slot_never_invents_a_middle_preview() {
    let (mut map, id, view) = map_with_line(false);
    let labels = &mut map.layer_mut(id).unwrap().labels;
    labels.default.other_positions = true;
    labels.default.placement = LabelPlacement::Curved;
    map.label_display.locked = true;
    map.label_display.show_unplaced = true;
    render(&mut map, &view).unwrap();
    assert!(!map.label_lock.as_ref().unwrap().is_empty());
    let anchors = map.label_lock_line_anchors.clone();
    map.label_lock.as_mut().unwrap()[0].1 = u8::MAX;
    let r = render(&mut map, &view).unwrap();
    assert!(r.labels.is_empty());
    assert!(r.unplaced.is_empty());
    assert_eq!(map.label_lock_line_anchors, anchors);
}

#[test]
fn rejected_curved_places_reuse_grapheme_allocations() {
    let c = curved(&[(20.0, 100.0), (380.0, 100.0)]);
    let curves = c.curves.as_ref().unwrap();
    let mut template = c.label.clone();
    template.glyphs = curves.place(0, &mut c.label.clone()).unwrap().glyphs;
    let allocations: Vec<_> = template.glyphs.iter().map(|g| g.text.as_ptr()).collect();
    let next = curves.place(1, &mut template).unwrap();
    assert_eq!(next.glyphs.iter().map(|g| g.text.as_ptr()).collect::<Vec<_>>(), allocations);
    assert_eq!(next.glyphs.iter().map(|g| g.text.as_str()).collect::<String>(), "road");
}
