use super::*;
use rustgis_data::source::{Batch, FeatureSource, Request, SourceInfo};
use rustgis_data::{Dataset, Feature, Field, FieldType, Value};
use rustgis_map::{Layer, Renderer, source::SourceLayer};

thread_local! {
    /// Complete chunks drawing from display geometry, and chunks retaining a full projection.
    pub(super) static LOD_PATHS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

struct Chunks {
    info: SourceInfo,
    ds: Dataset,
    size: usize,
}

impl FeatureSource for Chunks {
    fn info(&self) -> &SourceInfo {
        &self.info
    }
    fn read(&self, _: &Request) -> rustgis_data::Result<Batch> {
        Ok(Batch {
            features: self.ds.features.iter().take(1).cloned().collect(),
            matched: self.ds.features.len() as u64,
            complete: false,
            skipped: 0,
        })
    }
    fn streams_chunks(&self) -> bool {
        true
    }
    fn visit_chunks(
        &self,
        _: Option<Extent>,
        _: usize,
        visit: &mut dyn FnMut(Vec<Feature>) -> rustgis_data::Result<()>,
    ) -> rustgis_data::Result<Batch> {
        if self.size == 0 {
            visit(self.ds.features.iter().take(1).cloned().collect())?;
            return Err(rustgis_data::DataError::Format("cancelled after a source failure".into()));
        }
        for chunk in self.ds.features.chunks(self.size) {
            visit(chunk.to_vec())?;
        }
        Ok(Batch { complete: self.size != usize::MAX, matched: self.ds.features.len() as u64, ..Default::default() })
    }
}
fn streamed(layer: &Layer, size: usize) -> Layer {
    streamed_kind(layer, size, false)
}

/// Lets the streamed layers' background reads of `view` land first, as the screen does between
/// frames. A render then finds the view read and starts no read of its own, which could land
/// mid-test (on a busy machine) and replace the owner's rows.
fn settle(map: &mut Map, view: &View) {
    for _ in 0..2000 {
        let pending = if view.camera.pitch > 0.0 {
            sync_tilted_view(map, view)
        } else {
            let scale = map.ground_res_at(&view.camera) / rustgis_geom::PIXEL_M;
            map.sync_sources(&view_lonlat(map, view), scale, false)
        };
        if !pending {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("the background reads did not land");
}

fn streamed_kind(layer: &Layer, size: usize, native: bool) -> Layer {
    let ds = layer.dataset.clone();
    let info = SourceInfo {
        name: ds.name.clone(),
        format: "test",
        geometry_type: ds.geometry_type,
        fields: ds.fields.clone(),
        extent: ds.extent(),
        count: Some(ds.features.len() as u64),
        crs: if native { "EPSG:3857".into() } else { "EPSG:4326".into() },
        indexed: true,
        notes: Vec::new(),
        zm_dropped: false,
    };
    let mut l = layer.clone();
    l.dataset.features = ds.features.iter().take(1).cloned().collect::<Vec<_>>().into();
    l.source = Some(SourceLayer::from_path("test.shp", None, Box::new(Chunks { info, ds, size })));
    l
}

#[test]
fn complete_chunks_preserve_painter_order_filters_transparency_labels_and_selection() {
    // Keep selected interaction rows authoritative even if source synchronization completes
    // during this test. Complete traversal still emits the original file order.
    struct SelectedSample(Chunks);
    impl FeatureSource for SelectedSample {
        fn info(&self) -> &SourceInfo {
            self.0.info()
        }
        fn read(&self, _: &Request) -> rustgis_data::Result<Batch> {
            Ok(Batch {
                features: [5, 3].iter().filter_map(|row| self.0.ds.features.get(*row).cloned()).collect(),
                matched: self.0.ds.features.len() as u64,
                complete: false,
                skipped: 0,
            })
        }
        fn streams_chunks(&self) -> bool {
            true
        }
        fn visit_chunks(
            &self,
            extent: Option<Extent>,
            bytes: usize,
            visit: &mut dyn FnMut(Vec<Feature>) -> rustgis_data::Result<()>,
        ) -> rustgis_data::Result<Batch> {
            self.0.visit_chunks(extent, bytes, visit)
        }
    }
    let mut ds =
        Dataset::new("overlap", GeometryType::Polygon, vec![Field::new("name", FieldType::Text), Field::new("value", FieldType::Integer)]);
    for i in 0..37 {
        let x = (i % 7) as f64 * 0.015 - 0.05;
        let y = (i % 5) as f64 * 0.015 - 0.03;
        ds.push(
            Some(Geometry::Polygon(vec![vec![vec![
                Coord::new(x, y),
                Coord::new(x + 0.04, y),
                Coord::new(x + 0.04, y + 0.04),
                Coord::new(x, y + 0.04),
                Coord::new(x, y),
            ]]])),
            vec![Value::Text(format!("p{i}")), Value::Int(i)],
        );
    }
    let mut m = Map::new("test");
    m.add_dataset(ds);
    m.camera = Camera::new(rustgis_geom::lonlat_to_merc(Coord::new(0.02, 0.02)), 80.0);
    let l = &mut m.layers[0];
    l.transparency = 0.35;
    l.selection = vec![5, 3];
    l.definition_query = Some("value >= 3".into());
    l.labels.enabled = true;
    l.labels.default.field = "name".into();
    l.renderer = Renderer::Single(Symbol { fill: Rgba::new(210, 60, 25, 120), ..Symbol::default_for(GeometryType::Polygon, l.id) });
    let view = View { width: 320.0, height: 240.0, pixels_per_point: 1.0, camera: m.camera, sky: false };
    for extrusion in [None, Some(true), Some(false)] {
        m.layers[0].extrusion = extrusion.map(|shadows| rustgis_map::effects::Extrusion {
            height: 1500.0,
            field: None,
            factor: 1.0,
            face_culling: true,
            lighting: true,
            shadows,
        });
        let expected = render(&mut m.clone(), &view).unwrap();
        for size in [1, 3, 11, 100] {
            let mut actual = m.clone();
            actual.layers[0] = streamed(&m.layers[0], size);
            // Selected source rows are deliberately ordered differently from file order.
            actual.layers[0].dataset.features =
                [5, 3].iter().map(|row| m.layers[0].dataset.features[*row].clone()).collect::<Vec<_>>().into();
            actual.layers[0].selection = vec![0, 1];
            let info = actual.layers[0].source.as_ref().unwrap().info().unwrap().clone();
            actual.layers[0].source = Some(SourceLayer::from_path(
                "selected.shp",
                None,
                Box::new(SelectedSample(Chunks { info, ds: m.layers[0].dataset.clone(), size })),
            ));

            let Some(RenderOutput::Cpu(got)) = render_with_backend(&mut actual, &view, true, Backend::Cpu) else {
                panic!("complete selected frame");
            };
            assert!(got.pixmap.data() == expected.pixmap.data(), "chunk size {size} changed pixels");
            assert_eq!(got.labels.iter().map(|l| &l.text).collect::<Vec<_>>(), expected.labels.iter().map(|l| &l.text).collect::<Vec<_>>());
        }
    }
}

#[test]
fn complete_chunks_accumulate_point_bins_across_boundaries() {
    let mut ds = Dataset::new("points", GeometryType::Point, vec![]);
    for i in 0..211 {
        ds.push(Some(Geometry::Point(Coord::new((i % 13) as f64 * 0.002, (i % 7) as f64 * 0.002))), vec![]);
    }
    let mut m = Map::new("test");
    m.add_dataset(ds);
    m.camera = Camera::new(Coord::new(0.0, 0.0), 30.0);
    m.layers[0].aggregation = Some(rustgis_map::effects::Aggregation { size: 18.0, hexagons: false });
    m.layers[0].selection = vec![0];
    let view = View { width: 320.0, height: 240.0, pixels_per_point: 1.0, camera: m.camera, sky: false };
    let expected = render(&mut m.clone(), &view).unwrap();
    for size in [1, 17, 300] {
        let mut actual = m.clone();
        actual.layers[0] = streamed(&m.layers[0], size);

        let got = render(&mut actual, &view).unwrap();
        assert!(
            got.pixmap.data() == expected.pixmap.data(),
            "aggregation changed across chunks: {:?}",
            got.pixmap.data().iter().zip(expected.pixmap.data()).position(|(a, b)| a != b)
        );
    }
}

#[test]
fn complete_chunks_use_the_entire_mask_source() {
    let rectangle = |x: f64, y: f64, w: f64, h: f64| {
        Geometry::Polygon(vec![vec![vec![
            Coord::new(x, y),
            Coord::new(x + w, y),
            Coord::new(x + w, y + h),
            Coord::new(x, y + h),
            Coord::new(x, y),
        ]]])
    };
    let mut m = Map::new("mask");
    let mut ds = Dataset::new("ink", GeometryType::Polygon, vec![]);
    ds.push(Some(rectangle(-0.2, -0.15, 0.4, 0.3)), vec![]);
    let ink = m.add_dataset(ds);
    let mut ds = Dataset::new("mask", GeometryType::Polygon, vec![]);
    for i in 0..5 {
        ds.push(Some(rectangle(i as f64 * 0.05 - 0.12, -0.08, 0.025, 0.16)), vec![]);
    }
    let mask = m.add_dataset(ds);
    m.layer_mut(mask).unwrap().visible = false;
    m.layer_mut(ink).unwrap().mask = Some("mask".into());
    m.camera = Camera::new(Coord::new(0.0, 0.0), 100.0);
    let view = View { width: 320.0, height: 240.0, pixels_per_point: 1.0, camera: m.camera, sky: false };
    let expected = render(&mut m.clone(), &view).unwrap();
    for size in [1, 3, 10] {
        let mut actual = m.clone();
        *actual.layer_mut(mask).unwrap() = streamed(m.layer(mask).unwrap(), size);

        let got = render(&mut actual, &view).unwrap();
        assert!(got.pixmap.data() == expected.pixmap.data(), "a mask lost features across chunks");
    }
}

#[test]
fn complete_chunks_preserve_reused_stroke_paths_with_overlap_and_alpha() {
    let mut ds = Dataset::new("overlapping lines", GeometryType::Polyline, vec![Field::new("value", FieldType::Integer)]);
    for i in 0..37 {
        let y = (i % 5) as f64 * 0.006 - 0.012;
        ds.push(
            Some(Geometry::Line(vec![
                vec![Coord::new(-0.06, y), Coord::new(0.0, y + 0.02), Coord::new(0.06, y)],
                vec![Coord::new(-0.04, -y), Coord::new(0.04, -y)],
            ])),
            vec![Value::Int(i)],
        );
    }
    let mut base = Map::new("stroke paths");
    base.basemap = rustgis_map::Basemap::Blank;
    base.add_dataset(ds);
    let layer = &mut base.layers[0];
    layer.definition_query = Some("value < 34".into());
    layer.selection = vec![0];
    layer.line_strokes = vec![
        LineStroke { offset: 3.0, width: Some(6.0), color: Some(Rgba::new(20, 40, 180, 120)), ..Default::default() },
        LineStroke { offset: 3.0, width: Some(2.0), color: Some(Rgba::new(220, 30, 40, 180)), ..Default::default() },
        LineStroke { offset: -2.0, width: Some(1.5), color: Some(Rgba::new(40, 180, 80, 200)), ..Default::default() },
    ];
    for transparency in [0.0, 0.35] {
        base.layers[0].transparency = transparency;
        for ppp in [1.0, 1.5, 2.0] {
            let view =
                View { width: 240.0, height: 180.0, pixels_per_point: ppp, camera: Camera::new(Coord::new(0.0, 0.0), 80.0), sky: false };
            let expected = render(&mut base.clone(), &view).unwrap();
            for size in [1, 3, 11, 100] {
                let mut actual = base.clone();
                actual.layers[0] = streamed(&base.layers[0], size);
                settle(&mut actual, &view);
                let owner = actual.layers[0].dataset.features.clone();
                let got = render(&mut actual, &view).unwrap();
                assert!(got.pixmap.data() == expected.pixmap.data(), "size={size} ppp={ppp} transparency={transparency}");
                assert_eq!(actual.layers[0].dataset.features, owner);
                assert_eq!(render_png(&mut actual, &view).unwrap(), render_png(&mut base.clone(), &view).unwrap());
            }
        }
    }
}

#[test]
fn complete_chunks_preserve_label_classes_projection_rotation_and_png_export() {
    let mut ds =
        Dataset::new("lines", GeometryType::Polyline, vec![Field::new("name", FieldType::Text), Field::new("value", FieldType::Integer)]);
    for i in 0..23 {
        let y = i as f64 * 0.004 - 0.04;
        ds.push(
            Some(Geometry::Line(vec![vec![Coord::new(-0.06, y), Coord::new(0.0, y + 0.006), Coord::new(0.06, y)]])),
            vec![Value::Text(format!("line {i}")), Value::Int(i)],
        );
    }
    let mut base = Map::new("classes");
    base.add_dataset(ds);
    let l = &mut base.layers[0];
    l.labels.enabled = true;
    l.labels.default.field = "name".into();
    let mut class = l.labels.default.clone();
    class.name = "large".into();
    class.where_ = Some("value >= 10".into());
    class.size = 18.0;
    class.color = Rgba::rgb(180, 20, 30);
    class.bold = true;
    l.labels.classes.push(class);
    l.display_filters.push(rustgis_map::effects::DisplayFilter {
        name: "subset".into(),
        min_scale: None,
        max_scale: None,
        where_: "value < 20".into(),
    });
    l.selection = vec![0];
    l.line_strokes = vec![
        rustgis_map::LineStroke { color: Some(Rgba::rgb(20, 20, 20)), width: Some(5.0), offset: 3.0, ..Default::default() },
        rustgis_map::LineStroke { color: Some(Rgba::rgb(240, 180, 40)), width: Some(2.0), offset: 3.0, ..Default::default() },
    ];
    let view = View { width: 320.0, height: 240.0, pixels_per_point: 1.0, camera: Camera::new(Coord::new(0.0, 0.0), 80.0), sky: false };
    for projection in [rustgis_map::Projection::default(), rustgis_map::Projection::Custom("robinson".into())] {
        base.projection = projection;
        for bearing in [0.0, 25.0] {
            let view = View { camera: Camera { bearing, ..view.camera }, ..view };
            let expected = render(&mut base.clone(), &view).unwrap();
            assert!(!expected.labels.is_empty());
            assert!(expected.labels.iter().any(|l| l.bold));
            for size in [1, 3, 11, 100] {
                let mut actual = base.clone();
                actual.layers[0] = streamed(&base.layers[0], size);
                let got = render(&mut actual, &view).unwrap();
                assert_eq!(got.labels, expected.labels, "label class or placement changed");
                assert!(got.pixmap.data() == expected.pixmap.data(), "line rendering changed");
                assert_eq!(render_png(&mut actual, &view).unwrap(), render_png(&mut base.clone(), &view).unwrap(), "PNG export changed");
            }
        }
    }
}

#[test]
fn complete_chunks_fail_without_publishing_a_partial_frame() {
    let mut ds = Dataset::new("failure", GeometryType::Point, vec![]);
    ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
    let mut map = Map::new("failure");
    map.add_dataset(ds);
    map.layers[0] = streamed(&map.layers[0], 0);
    let view = View { width: 320.0, height: 240.0, pixels_per_point: 1.0, camera: map.camera, sky: false };
    assert!(render(&mut map, &view).is_none());
    assert!(map.layers[0].source.as_ref().unwrap().error.as_ref().unwrap().contains("cancelled"));
}

/// Complete drawing clears the chunk source and uses its display budget independently
/// of the interaction window, even when that window can retain only one byte.
#[test]
fn complete_chunks_use_whole_layer_lod_decisions() {
    let mut ds = Dataset::new("detailed", GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
    for row in 0..11 {
        let points = (0..20_001).map(|i| Coord::new(-0.1 + i as f64 * 0.00001, row as f64 * 0.004 + (i % 2) as f64 * 0.000001)).collect();
        ds.push(Some(Geometry::Line(vec![points])), vec![Value::Text(format!("line {row}"))]);
    }
    let mut base = Map::new("LOD");
    base.add_dataset(ds);
    let view = View { width: 320.0, height: 240.0, pixels_per_point: 1.0, camera: Camera::new(Coord::new(0.0, 0.0), 80.0), sky: false };
    for labelled in [false, true] {
        let mut variant = base.clone();
        variant.layers[0].labels.enabled = labelled;
        variant.layers[0].labels.default.field = "name".into();
        let expected = render(&mut variant.clone(), &view).unwrap();
        if labelled {
            assert!(!expected.labels.is_empty(), "the exact-consumer case must place real labels");
        }
        for size in [1, 3, 20] {
            for budget in [0, 1] {
                let mut actual = variant.clone();
                actual.layers[0] = streamed(&variant.layers[0], size);
                actual.layers[0].source.as_mut().unwrap().byte_budget = budget;
                LOD_PATHS.set((0, 0));
                let got = render(&mut actual, &view).unwrap();
                let chunks = 11usize.div_ceil(size);
                let paths = if labelled { (0, chunks) } else { (chunks, 0) };
                assert_eq!(LOD_PATHS.get(), paths, "label consumers must select exact projection, plain chunks the display path");
                assert_eq!(got.labels, expected.labels);
                assert!(
                    got.pixmap.data() == expected.pixmap.data(),
                    "whole-layer LOD changed at chunk size {size}, interaction budget {budget}, labelled {labelled}"
                );
            }
        }
    }
}

#[test]
fn perspective_uses_complete_isolated_fallback() {
    let mut ds = Dataset::new("upright", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    for i in 0..13 {
        ds.push(Some(Geometry::Point(Coord::new((i % 5) as f64 * 0.01, (i / 5) as f64 * 0.01))), vec![Value::Text(format!("point {i}"))]);
    }
    let mut base = Map::new("perspective");
    base.add_dataset(ds);
    base.layers[0].labels.enabled = true;
    base.layers[0].labels.default.field = "name".into();
    base.layers[0].selection = vec![0];
    let view = View {
        width: 320.0,
        height: 240.0,
        pixels_per_point: 1.0,
        camera: Camera { pitch: 35.0, ..Camera::new(Coord::new(0.0, 0.0), 80.0) },
        sky: false,
    };
    let expected = render(&mut base.clone(), &view).unwrap();
    for size in [1, 3, 20] {
        let mut actual = base.clone();
        actual.layers[0] = streamed(&base.layers[0], size);
        settle(&mut actual, &view);
        let before = actual.layers[0].dataset.features.clone();
        let got = render(&mut actual, &view).unwrap();
        assert_eq!(got.labels, expected.labels);
        assert!(got.pixmap.data() == expected.pixmap.data());
        // The complete chunks never reach the interaction rows. Compared by value, not by
        // allocation: the layer's background read can finish mid-render and install the same
        // one-row sample again as a new allocation.
        assert_eq!(actual.layers[0].dataset.features, before);
        assert_eq!(actual.layers[0].selection, vec![0]);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn actual_shapefile_export_is_complete_despite_one_row_interaction_limit() {
    // Keep this Shapefile's index out of the user's cache (#1230).
    rustgis_data::source::shp::use_temporary_index_dir();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("complete.shp");
    let mut ds =
        Dataset::new("complete", GeometryType::Point, vec![Field::new("name", FieldType::Text), Field::new("value", FieldType::Integer)]);
    for i in 0..29 {
        ds.push(
            Some(Geometry::Point(Coord::new((i % 7) as f64 * 0.02 - 0.06, (i / 7) as f64 * 0.02 - 0.04))),
            vec![Value::Text(format!("p{i}")), Value::Int(i)],
        );
    }
    for (ext, bytes) in rustgis_data::io::shapefile_parts(&ds).unwrap().0 {
        std::fs::write(path.with_extension(ext), bytes).unwrap();
    }
    let source = rustgis_data::source::shp::ShpSource::open_path(&path, "complete").unwrap();
    ds.fields = source.info().fields.clone();
    ds.features = source.read(&Request::all()).unwrap().features.into();
    let mut base = Map::new("complete");
    base.add_dataset(ds);
    base.layers[0].labels.enabled = true;
    base.layers[0].labels.default.field = "name".into();
    base.layers[0].selection = vec![0];
    let symbol = base.layers[0].symbol(0);
    let renderers = vec![
        Renderer::Single(symbol),
        Renderer::unique(&base.layers[0].dataset, &[], "value", rustgis_map::Ramp::Ember, symbol).unwrap(),
        Renderer::graduated(
            &base.layers[0].dataset,
            &[],
            "value",
            4,
            rustgis_map::ClassMethod::EqualInterval,
            rustgis_map::Ramp::Ember,
            symbol,
        )
        .unwrap(),
        Renderer::proportional(&base.layers[0].dataset, &[], "value", symbol).unwrap(),
    ];
    let view = View { width: 320.0, height: 240.0, pixels_per_point: 1.0, camera: Camera::new(Coord::new(0.0, 0.0), 100.0), sky: false };
    for renderer in renderers {
        base.layers[0].renderer = renderer;
        let expected = render(&mut base.clone(), &view).unwrap();
        assert!(expected.labels.len() > 1);
        let mut actual = base.clone();
        actual.layers[0].dataset.features = base.layers[0].dataset.features.iter().take(1).cloned().collect::<Vec<_>>().into();
        let mut source = SourceLayer::from_path(
            path.to_string_lossy(),
            None,
            Box::new(rustgis_data::source::shp::ShpSource::open_path(&path, "complete").unwrap()),
        );
        source.limit = 1;
        actual.layers[0].source = Some(source);
        let bytes = render_png(&mut actual, &view).unwrap();
        assert_eq!(bytes, render_png(&mut base.clone(), &view).unwrap());
        assert_eq!(actual.layers[0].dataset.features.len(), 1, "export must not replace the interaction window");
        if let Ok(path) = std::env::var("RUSTGIS_CHUNK_TEST_PNG") {
            std::fs::write(path, bytes).unwrap();
        }
    }
}

#[test]
fn partial_source_failure_and_incomplete_summary_reach_unchanged_owner() {
    let mut ds = Dataset::new("failure", GeometryType::Point, vec![]);
    ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
    let mut base = Map::new("failure");
    base.add_dataset(ds);
    for (size, message) in [(0, "cancelled"), (usize::MAX, "incomplete")] {
        let mut map = base.clone();
        map.layers[0] = streamed(&base.layers[0], size);
        let view = View { width: 320.0, height: 240.0, pixels_per_point: 1.0, camera: map.camera, sky: false };
        settle(&mut map, &view);
        let mut worker = map.clone();
        assert!(render(&mut worker, &view).is_none());
        // The error below can only come from the worker.
        assert!(map.layers[0].source.as_ref().unwrap().error.is_none());
        map.adopt_caches(&worker);
        assert!(map.layers[0].source.as_ref().unwrap().error.as_ref().unwrap().contains(message));
        map.layers[0].source.as_mut().unwrap().error = None;
        map.layers[0].touch_geometry();
        map.adopt_caches(&worker);
        assert!(map.layers[0].source.as_ref().unwrap().error.is_none());
    }
}

#[test]
fn layout_export_keeps_all_drawn_legend_classes() {
    use rustgis_map::layout::{ElementKind, FrameShape, INDICATOR_COLOR, INDICATOR_WIDTH, Layout};
    let mut ds = Dataset::new("classes", GeometryType::Point, vec![Field::new("value", FieldType::Integer)]);
    for i in 0..12 {
        ds.push(Some(Geometry::Point(Coord::new((i % 4) as f64 * 0.02, (i / 4) as f64 * 0.02))), vec![Value::Int(i)]);
    }
    let mut base = Map::new("legend");
    base.add_dataset(ds);
    let layer = &mut base.layers[0];
    layer.renderer = Renderer::unique(&layer.dataset, &[], "value", rustgis_map::Ramp::Ember, layer.symbol(0)).unwrap();
    layer.definition_query = Some("value >= 2".into());
    let mut page = Layout::new("page", 140.0, 100.0);
    let frame = page.add(
        "map",
        (0.0, 0.0, 85.0, 95.0),
        ElementKind::MapFrame {
            map: base.name.clone(),
            center: Coord::new(0.02, 0.02),
            scale: 300000.0,
            shape: FrameShape::Rectangle,
            grid: None,
            extent_of: None,
            extent_color: INDICATOR_COLOR,
            extent_width: INDICATOR_WIDTH,
            bearing: 0.0,
            pitch: 0.0,
        },
    );
    page.add(
        "legend",
        (90.0, 0.0, 45.0, 95.0),
        ElementKind::Legend { frame, title: "classes".into(), all_classes: false, hide_other: false, exclude: vec![], columns: 1 },
    );
    let expected = layout::render(&page, &mut [base.clone()], 96.0, &layout::PageInfo::default()).unwrap();
    for size in [1, 3, 20] {
        let mut actual = base.clone();
        actual.layers[0] = streamed(&base.layers[0], size);
        let got = layout::render(&page, &mut [actual], 96.0, &layout::PageInfo::default()).unwrap();
        assert!(got.data() == expected.data(), "layout/legend changed at chunk size {size}");
    }
    let mut failed = base.clone();
    failed.layers[0] = streamed(&base.layers[0], 0);
    assert!(layout::render(&page, &mut [failed], 96.0, &layout::PageInfo::default()).is_err());
}

#[test]
fn chunked_label_compaction_keeps_late_priorities_stable_ties_and_nan_order() {
    let candidates: Vec<Candidate> = (0..50_003)
        .map(|i| {
            (
                PlacedLabel {
                    text: format!("label {i}"),
                    x: 100.0,
                    y: 100.0,
                    size: 10.0,
                    color: Rgba::rgb(0, 0, 0),
                    halo: None,
                    halo_width: None,
                    bold: false,
                    icon: None,
                    align: 0.0,
                    framed: false,
                    italic: false,
                    spacing: 0.0,
                    font: None,
                    line_align: TextAlign::Left,
                    angle: None,
                    collide: Default::default(),
                    glyphs: Vec::new(),
                    background: None,
                },
                if i % 17 == 0 { f64::NAN } else { (i % 7) as f64 },
                (1, i, 0, 0),
            )
                .into()
        })
        .collect();
    let expected = place_labels(candidates.clone(), vec![], 320.0, 240.0, false);
    let mut compacted = Vec::new();
    for batch in candidates.chunks(1001) {
        compacted.extend_from_slice(batch);
        compact_label_candidates(&mut compacted);
    }
    let got = place_labels(compacted, vec![], 320.0, 240.0, false);
    assert_eq!(got.labels, expected.labels);
    assert_eq!(got.unplaced, expected.unplaced);
    assert_eq!(got.keys, expected.keys);
}

#[test]
fn perspective_does_not_read_unused_hidden_sources() {
    let mut ds = Dataset::new("visible", GeometryType::Point, vec![]);
    ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
    let mut base = Map::new("hidden");
    base.add_dataset(ds.clone());
    let hidden = base.add_dataset(ds);
    base.layer_mut(hidden).unwrap().visible = false;
    let view = View {
        width: 320.0,
        height: 240.0,
        pixels_per_point: 1.0,
        camera: Camera { pitch: 35.0, ..Camera::new(Coord::new(0.0, 0.0), 80.0) },
        sky: false,
    };
    let expected = render(&mut base.clone(), &view).unwrap();
    let mut actual = base.clone();
    for layer in &mut actual.layers {
        *layer = streamed(layer, if layer.id == hidden { 0 } else { 1 });
    }
    let got = render(&mut actual, &view).unwrap();
    assert!(got.pixmap.data() == expected.pixmap.data());
    assert!(actual.layer(hidden).unwrap().source.as_ref().unwrap().error.is_none());
}

#[test]
fn extrusion_spool_failures_retain_the_actual_source_diagnostic() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            effects::SPOOL_FAILURE.with(|s| s.set(None));
        }
    }
    let _reset = Reset;
    let mut ds = Dataset::new("extrusion failure", GeometryType::Polygon, vec![]);
    ds.push(
        Some(Geometry::Polygon(vec![vec![vec![
            Coord::new(0.0, 0.0),
            Coord::new(0.01, 0.0),
            Coord::new(0.01, 0.01),
            Coord::new(0.0, 0.0),
        ]]])),
        vec![],
    );
    let mut base = Map::new("spool failure");
    base.add_dataset(ds);
    base.layers[0].extrusion = Some(rustgis_map::effects::Extrusion {
        height: 100.0,
        field: None,
        factor: 1.0,
        face_culling: true,
        lighting: true,
        shadows: true,
    });
    let view = View { width: 120.0, height: 90.0, pixels_per_point: 1.0, camera: Camera::new(Coord::new(0.0, 0.0), 80.0), sky: false };
    for stage in ["create", "insert", "draw"] {
        let mut map = base.clone();
        map.layers[0] = streamed(&base.layers[0], 1);
        effects::SPOOL_FAILURE.with(|s| s.set(Some(stage)));
        assert!(render(&mut map, &view).is_none());
        assert!(map.layers[0].source.as_ref().unwrap().error.as_ref().unwrap().contains(&format!("spool {stage} failure")));
        effects::SPOOL_FAILURE.with(|s| s.set(None));
        assert!(render(&mut map, &view).is_some());
        assert!(map.layers[0].source.as_ref().unwrap().error.is_none());
    }
}

#[test]
fn perspective_reports_shapefile_open_failure() {
    let mut ds = Dataset::new("visible", GeometryType::Point, vec![]);
    ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
    let mut map = Map::new("open failure");
    map.add_dataset(ds);
    let view = View {
        width: 120.0,
        height: 90.0,
        pixels_per_point: 1.0,
        camera: Camera { pitch: 35.0, ..Camera::new(Coord::new(0.0, 0.0), 80.0) },
        sky: false,
    };
    let dir = tempfile::tempdir().unwrap();
    let mut source = SourceLayer::default();
    source.path = Some(dir.path().join("missing.shp").display().to_string());
    map.layers[0].source = Some(source);
    assert!(render(&mut map, &view).is_none());
    assert!(map.layers[0].source.as_ref().unwrap().error.is_some());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn pitched_mixed_sources_keep_windows_and_pending_on_the_owner() {
    use std::sync::{Arc, Condvar, Mutex};
    struct WindowSource {
        chunks: Chunks,
        gate: Arc<(Mutex<bool>, Condvar)>,
    }
    impl FeatureSource for WindowSource {
        fn info(&self) -> &SourceInfo {
            &self.chunks.info
        }
        fn read(&self, _: &Request) -> rustgis_data::Result<Batch> {
            let (lock, wake) = &*self.gate;
            let mut ready = lock.lock().unwrap();
            while !*ready {
                ready = wake.wait(ready).unwrap();
            }
            Ok(Batch {
                features: self.chunks.ds.features.to_vec(),
                matched: self.chunks.ds.features.len() as u64,
                complete: true,
                skipped: 0,
            })
        }
    }
    struct Release(Arc<(Mutex<bool>, Condvar)>);
    impl Drop for Release {
        fn drop(&mut self) {
            *self.0.0.lock().unwrap() = true;
            self.0.1.notify_all();
        }
    }
    let mut ds = Dataset::new("mixed", GeometryType::Point, vec![]);
    ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
    ds.push(Some(Geometry::Point(Coord::new(0.01, 0.01))), vec![]);
    let mut base = Map::new("mixed");
    base.add_dataset(ds.clone());
    base.add_dataset(ds);
    let view = View {
        width: 120.0,
        height: 90.0,
        pixels_per_point: 1.0,
        camera: Camera { pitch: 35.0, ..Camera::new(Coord::new(0.0, 0.0), 80.0) },
        sky: false,
    };
    for (backend, total) in [(Backend::Cpu, Some(2)), (Backend::Gpu, Some(2)), (Backend::Cpu, None), (Backend::Gpu, None)] {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Release(gate.clone());
        let mut map = base.clone();
        map.layers[0] = streamed(&base.layers[0], 1);
        let mut info = map.layers[0].source.as_ref().unwrap().info().unwrap().clone();
        info.count = total;
        map.layers[0].source = Some(SourceLayer::from_path(
            "test.shp",
            None,
            Box::new(Chunks { info: info.clone(), ds: base.layers[0].dataset.clone(), size: 1 }),
        ));
        map.layers[1].dataset.features = Vec::new().into();
        map.layers[1].source = Some(SourceLayer::from_path(
            "mixed.gpkg",
            None,
            Box::new(WindowSource { chunks: Chunks { info, ds: base.layers[1].dataset.clone(), size: 1 }, gate }),
        ));
        let output = render_with_backend(&mut map, &view, false, backend).unwrap();
        let pending = match output {
            RenderOutput::Cpu(r) => r.data_pending,
            RenderOutput::Gpu(s) => s.data_pending,
        };
        assert!(pending);
        assert!(map.layers[1].source.as_ref().unwrap().is_pending());
        drop(release);
        // Wait for the same owning request, then verify the next frame retains its window.
        let bands = view.camera.tilt_bands(view.width, view.height);
        sync_tilted(&mut map, &view, &bands, true);
        assert_eq!(map.layers[1].dataset.features.len(), 2);
        assert!(map.layers[1].source.as_ref().unwrap().reads > 0);
        let output = render_with_backend(&mut map, &view, true, backend).unwrap();
        let pending = match output {
            RenderOutput::Cpu(r) => r.data_pending,
            RenderOutput::Gpu(s) => s.data_pending,
        };
        assert!(!pending);
        assert_eq!(map.layers[1].dataset.features.len(), 2);
    }
}

#[test]
fn perspective_snapshot_budget_counts_all_layers_capacity_and_nested_values() {
    let row = Feature {
        id: 1,
        geometry: Some(Geometry::MultiPoint(vec![Coord::new(0.0, 0.0); 8])),
        attrs: vec![Value::Text("large attribute".repeat(16))],
    };
    let row_bytes = rustgis_data::source::chunk_memory::feature_bytes(&row) + std::mem::size_of::<Feature>();
    let mut budget = SnapshotBudget::new(row_bytes);
    let mut first = Vec::new();
    budget.append(&mut first, vec![row.clone()]).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(budget.used, row_bytes);
    let mut second = Vec::new();
    assert!(budget.append(&mut second, vec![row.clone()]).is_err());
    assert!(second.is_empty());
    assert!(budget.append(&mut first, vec![row]).is_err());
    assert_eq!(first.len(), 1);
    assert_eq!(budget.used, row_bytes);
}

#[test]
fn perspective_snapshot_budget_failure_reaches_source_without_partial_output() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            SNAPSHOT_TEST_LIMIT.with(|s| s.set(None));
        }
    }
    let _reset = Reset;
    let mut ds = Dataset::new("snapshot", GeometryType::Point, vec![]);
    ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
    let mut base = Map::new("snapshot");
    base.add_dataset(ds);
    let view = View {
        width: 120.0,
        height: 90.0,
        pixels_per_point: 1.0,
        camera: Camera { pitch: 35.0, ..Camera::new(Coord::new(0.0, 0.0), 80.0) },
        sky: false,
    };
    for backend in [Backend::Cpu, Backend::Gpu] {
        let mut map = base.clone();
        map.layers[0] = streamed(&base.layers[0], 1);
        SNAPSHOT_TEST_LIMIT.with(|s| s.set(Some(0)));
        assert!(render_with_backend(&mut map, &view, true, backend).is_none());
        assert!(map.layers[0].source.as_ref().unwrap().error.as_ref().unwrap().contains("snapshot exceeds"));
        assert!(map.layers[0].source.as_ref().unwrap().chunk_source().is_some());
        SNAPSHOT_TEST_LIMIT.with(|s| s.set(None));
        assert!(render_with_backend(&mut map, &view, true, backend).is_some());
        assert!(map.layers[0].source.as_ref().unwrap().error.is_none());
    }
}

#[test]
fn masks_skip_undrawn_targets_and_reuse_one_complete_source_with_distinct_swipes() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Counted {
        chunks: Chunks,
        calls: Arc<AtomicUsize>,
    }
    impl FeatureSource for Counted {
        fn info(&self) -> &SourceInfo {
            self.chunks.info()
        }
        fn read(&self, request: &Request) -> rustgis_data::Result<Batch> {
            self.chunks.read(request)
        }
        fn streams_chunks(&self) -> bool {
            true
        }
        fn visit_chunks(
            &self,
            extent: Option<Extent>,
            bytes: usize,
            visit: &mut dyn FnMut(Vec<Feature>) -> rustgis_data::Result<()>,
        ) -> rustgis_data::Result<Batch> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.chunks.visit_chunks(extent, bytes, visit)
        }
    }
    let mut ds = Dataset::new("target", GeometryType::Polygon, vec![]);
    ds.push(
        Some(Geometry::Polygon(vec![vec![vec![
            Coord::new(-0.02, -0.02),
            Coord::new(0.02, -0.02),
            Coord::new(0.02, 0.02),
            Coord::new(-0.02, 0.02),
            Coord::new(-0.02, -0.02),
        ]]])),
        vec![],
    );
    let mut base = Map::new("shared masks");
    let first = base.add_dataset(ds.clone());
    let second = base.add_dataset(ds.clone());
    ds.name = "mask".into();
    let mask = base.add_dataset(ds.clone());
    base.layer_mut(mask).unwrap().visible = false;
    for id in [first, second] {
        base.layer_mut(id).unwrap().mask = Some("mask".into());
    }
    base.swipe = Some((first, 0.35));
    let view = View { width: 120.0, height: 90.0, pixels_per_point: 1.0, camera: Camera::new(Coord::new(0.0, 0.0), 80.0), sky: false };
    let ctx = Ctx {
        view: &view,
        ppp: 1.0,
        wireframe: false,
        cull: Extent::EMPTY,
        globe: None,
        sprites: Default::default(),
        bursts: Default::default(),
    };
    let flat = base.flat();
    let scale = base.ground_res_at(&view.camera) / rustgis_geom::PIXEL_M;
    let expected = layer_masks(&mut base.clone(), &ctx, 120, 90, &flat, "test", scale).unwrap();
    let info = streamed(base.layer(mask).unwrap(), 1).source.unwrap().info().unwrap().clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut actual = base.clone();
    actual.layer_mut(mask).unwrap().source =
        Some(SourceLayer::from_path("mask.shp", None, Box::new(Counted { chunks: Chunks { info, ds, size: 1 }, calls: calls.clone() })));
    let got = layer_masks(&mut actual, &ctx, 120, 90, &flat, "test", scale).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(got.len(), 2);
    for ((id, got), (expected_id, expected)) in got.iter().zip(&expected) {
        assert_eq!(id, expected_id);
        assert_eq!(got.data(), expected.data());
    }
    assert_ne!(got[0].1.data(), got[1].1.data(), "swipe remains target-specific");
    for mode in 0..5 {
        let mut map = base.clone();
        *map.layer_mut(mask).unwrap() = streamed(base.layer(mask).unwrap(), 0);
        for id in [first, second] {
            match mode {
                0 => map.layer_mut(id).unwrap().visible = false,
                1 => map.layer_mut(id).unwrap().max_scale = Some(scale * 2.0),
                2 | 3 => {
                    let mut group = rustgis_map::LayerGroup::new(format!("group {id}"));
                    group.id = id;
                    group.children = vec![id];
                    if mode == 2 {
                        group.visible = false;
                    } else {
                        group.max_scale = Some(scale * 2.0);
                    }
                    map.groups.push(group);
                }
                _ => {}
            }
        }
        let got = layer_masks(&mut map, &ctx, 120, 90, &flat, "test", scale);
        if mode == 4 {
            assert!(got.is_none(), "a failed required mask must still fail the frame");
        } else {
            assert!(got.unwrap().is_empty());
            assert!(map.layer(mask).unwrap().source.as_ref().unwrap().error.is_none());
        }
    }
}

#[test]
fn pitched_plain_chunks_bypass_depth_budget_alone_and_with_upright_markers() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            SNAPSHOT_TEST_LIMIT.with(|s| s.set(None));
        }
    }
    let _reset = Reset;
    let view = View {
        width: 120.0,
        height: 90.0,
        pixels_per_point: 1.0,
        camera: Camera { pitch: 35.0, ..Camera::new(Coord::new(0.0, 0.0), 80.0) },
        sky: false,
    };
    for kind in [GeometryType::Polyline, GeometryType::Polygon] {
        let mut ds = Dataset::new("ground", kind, vec![Field::new("text", FieldType::Text)]);
        for i in 0..5 {
            let x = i as f64 * 0.005 - 0.01;
            let ring = vec![
                Coord::new(x, -0.01),
                Coord::new(x + 0.004, -0.01),
                Coord::new(x + 0.004, 0.01),
                Coord::new(x, 0.01),
                Coord::new(x, -0.01),
            ];
            let g = if kind == GeometryType::Polyline { Geometry::Line(vec![ring]) } else { Geometry::Polygon(vec![vec![ring]]) };
            ds.push(Some(g), vec![Value::Text("attribute".repeat(256))]);
        }
        let mut base = Map::new("depth eligibility");
        let ground = base.add_dataset(ds);
        base.layer_mut(ground).unwrap().selection = vec![0];
        base.layer_mut(ground).unwrap().transparency = 0.35;
        for mixed in [false, true] {
            let mut full = base.clone();
            if mixed {
                let mut ds = Dataset::new("upright", GeometryType::Point, vec![]);
                ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
                full.add_dataset(ds);
            }
            for backend in [Backend::Cpu, Backend::Gpu] {
                let pixels = |output: RenderOutput| match output {
                    RenderOutput::Cpu(r) => (r.pixmap.data().to_vec(), r.labels),
                    RenderOutput::Gpu(_) => panic!("complete chunk painter uses the CPU fallback"),
                };
                // Always use a CPU reference; a plain in-memory ground layer may be GPU eligible.
                let expected_cpu = render_with_backend(&mut full.clone(), &view, true, Backend::Cpu).unwrap();
                let (expected_pixels, expected_labels) = pixels(expected_cpu);
                let mut map = full.clone();
                for layer in &mut map.layers {
                    *layer = streamed(layer, 1);
                }
                SNAPSHOT_TEST_LIMIT.with(|s| s.set(Some(std::mem::size_of::<Feature>() * 2)));
                let got = render_with_backend(&mut map, &view, true, backend).unwrap();
                let (got_pixels, got_labels) = pixels(got);
                assert_eq!(got_pixels, expected_pixels, "kind {kind:?}, mixed {mixed}, backend {backend:?}");
                assert_eq!(got_labels, expected_labels);

                let mut failed = full.clone();
                for layer in &mut failed.layers {
                    *layer = streamed(layer, if layer.id == ground { 0 } else { 1 });
                }
                let lock = Some(vec![((ground, 0, 0, 0), rustgis_map::LockedPlace::default())]);
                failed.label_lock = lock.clone();
                assert!(render_with_backend(&mut failed, &view, true, backend).is_none());
                assert_eq!(failed.label_lock, lock);
                assert!(failed.layer(ground).unwrap().source.as_ref().unwrap().error.as_ref().unwrap().contains("cancelled"));
                SNAPSHOT_TEST_LIMIT.with(|s| s.set(None));
            }
        }
    }
}

#[test]
fn pitched_large_or_unknown_point_sources_use_total_rows_not_the_interaction_window() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            SNAPSHOT_TEST_LIMIT.with(|s| s.set(None));
        }
    }
    let _reset = Reset;
    let mut ds = Dataset::new("many points", GeometryType::Point, vec![]);
    for i in 0..=MAX_UPRIGHT_POINTS {
        let c = if i < 3 { Coord::new(i as f64 * 0.005, 0.0) } else { Coord::new(179.0, 80.0) };
        ds.push(Some(Geometry::Point(c)), vec![]);
    }
    let mut full = Map::new("source total");
    full.add_dataset(ds.clone());
    let view = View {
        width: 96.0,
        height: 64.0,
        pixels_per_point: 1.0,
        camera: Camera { pitch: 35.0, ..Camera::new(Coord::new(0.0, 0.0), 80.0) },
        sky: false,
    };
    // Exercise the direct GPU path as well as the selected CPU fallback.
    for selected in [false, true] {
        full.layers[0].selection = if selected { vec![0] } else { Vec::new() };
        let expected = render(&mut full.clone(), &view).unwrap();
        assert!(!stands_up(&full, &full.layers[0]));
        for total in [Some(MAX_UPRIGHT_POINTS as u64 + 1), None] {
            for window in [1, 3] {
                for backend in [Backend::Cpu, Backend::Gpu] {
                    let mut map = full.clone();
                    map.layers[0] = streamed(&full.layers[0], 2048);
                    map.layers[0].dataset.features = ds.features.iter().take(window).cloned().collect::<Vec<_>>().into();
                    let mut info = map.layers[0].source.as_ref().unwrap().info().unwrap().clone();
                    info.count = total;
                    map.layers[0].source =
                        Some(SourceLayer::from_path("points.shp", None, Box::new(Chunks { info, ds: ds.clone(), size: 2048 })));
                    assert!(!stands_up(&map, &map.layers[0]));
                    SNAPSHOT_TEST_LIMIT.with(|s| s.set(Some(0)));
                    match render_with_backend(&mut map, &view, true, backend).expect("complete large-source render") {
                        RenderOutput::Cpu(got) => {
                            assert!(got.pixmap.data() == expected.pixmap.data());
                            assert_eq!(got.labels, expected.labels);
                            assert!(!got.data_pending);
                        }
                        RenderOutput::Gpu(scene) => {
                            assert_eq!(backend, Backend::Gpu);
                            let Some(RenderOutput::Gpu(reference)) = render_with_backend(&mut full.clone(), &view, true, backend) else {
                                panic!("the resident GPU reference should use the same ground-marker path");
                            };
                            let count = |scene: &gpu_scene::GpuScene| {
                                scene
                                    .cmds
                                    .iter()
                                    .map(|cmd| match cmd {
                                        gpu_scene::DrawCmd::Points { instances, .. } => instances.len(),
                                        _ => 0,
                                    })
                                    .sum::<usize>()
                            };
                            assert!(scene.tilt.is_some());
                            assert!(scene.clips.iter().all(|clip| !clip.upright));
                            assert!(count(&scene) > 0);
                            assert_eq!(count(&scene), count(&reference), "the GPU scene must include all visible source markers");
                            assert_eq!(scene.labels, reference.labels);
                            assert_eq!(scene.clear, reference.clear);
                            assert!(!scene.data_pending);
                        }
                    }
                    assert!(!stands_up(&map, &map.layers[0]));
                    assert!(map.layers[0].source.as_ref().unwrap().error.is_none());
                }
            }
        }
    }
}

#[test]
fn layout_failure_policy_only_requires_active_complete_sources_and_masks() {
    let mut ds = Dataset::new("plain", GeometryType::Point, vec![]);
    ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
    let mut map = Map::new("layout policy");
    let plain = map.add_dataset(ds);
    let mut ds = Dataset::new("mask", GeometryType::Polygon, vec![]);
    ds.push(
        Some(Geometry::Polygon(vec![vec![vec![
            Coord::new(-0.01, -0.01),
            Coord::new(0.01, -0.01),
            Coord::new(0.01, 0.01),
            Coord::new(-0.01, -0.01),
        ]]])),
        vec![],
    );
    let mask = map.add_dataset(ds);
    *map.layer_mut(mask).unwrap() = streamed(map.layer(mask).unwrap(), 1);
    map.layer_mut(mask).unwrap().visible = false;
    let view = View { width: 120.0, height: 90.0, pixels_per_point: 1.0, camera: Camera::new(Coord::new(0.0, 0.0), 80.0), sky: false };
    assert!(!layout::frame_requires_complete(&map, &view), "an unused hidden source cannot change ordinary frame failure behavior");
    map.layer_mut(plain).unwrap().mask = Some("mask".into());
    assert!(layout::frame_requires_complete(&map, &view), "a needed hidden complete mask is authoritative");
    map.layer_mut(plain).unwrap().visible = false;
    assert!(!layout::frame_requires_complete(&map, &view));
    map.layer_mut(mask).unwrap().visible = true;
    assert!(layout::frame_requires_complete(&map, &view));
    let mut group = rustgis_map::LayerGroup::new("hidden group");
    group.visible = false;
    group.children = vec![mask];
    map.groups.push(group);
    assert!(!layout::frame_requires_complete(&map, &view));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn layout_export_rejects_unopened_local_shapefiles_and_required_masks() {
    use rustgis_map::layout::{ElementKind, FrameShape, INDICATOR_COLOR, INDICATOR_WIDTH, Layout};
    let dir = tempfile::tempdir().unwrap();
    let mut base = Map::new("persisted sources");
    let mut points = Dataset::new("points", GeometryType::Point, vec![]);
    points.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
    let point = base.add_dataset(points);
    let mask = base.add_dataset(Dataset::new("mask", GeometryType::Polygon, vec![]));
    base.layer_mut(mask).unwrap().visible = false;
    for pitch in [0.0, 35.0] {
        let mut page = Layout::new("page", 30.0, 20.0);
        page.add(
            "map",
            (0.0, 0.0, 30.0, 20.0),
            ElementKind::MapFrame {
                map: base.name.clone(),
                center: Coord::new(0.0, 0.0),
                scale: 300000.0,
                shape: FrameShape::Rectangle,
                grid: None,
                extent_of: None,
                extent_color: INDICATOR_COLOR,
                extent_width: INDICATOR_WIDTH,
                bearing: 0.0,
                pitch,
            },
        );
        for masked in [false, true] {
            let mut map = base.clone();
            let owner = if masked { mask } else { point };
            let mut source = SourceLayer::default();
            source.path = Some(dir.path().join("missing.SHP").display().to_string());
            map.layer_mut(owner).unwrap().source = Some(source);
            if masked {
                map.layer_mut(point).unwrap().mask = Some("MASK".into());
            }
            assert!(
                layout::render(&page, std::slice::from_mut(&mut map), 96.0, &layout::PageInfo::default()).is_err(),
                "missing complete source must fail the page: pitch {pitch}, masked {masked}"
            );
            assert!(map.layer(owner).unwrap().source.as_ref().unwrap().error.is_some());
        }
        let mut unused = base.clone();
        let mut source = SourceLayer::default();
        source.path = Some(dir.path().join("unused.shp").display().to_string());
        unused.layer_mut(mask).unwrap().source = Some(source);
        assert!(
            layout::render(&page, &mut [unused], 96.0, &layout::PageInfo::default()).is_ok(),
            "an unused hidden Shapefile must not fail export"
        );

        let mut ordinary = base.clone();
        let mut source = SourceLayer::default();
        source.path = Some(dir.path().join("missing.gpkg").display().to_string());
        ordinary.layer_mut(point).unwrap().source = Some(source);
        assert!(
            layout::render(&page, &mut [ordinary], 96.0, &layout::PageInfo::default()).is_ok(),
            "ordinary sources retain upstream best-effort behavior"
        );
    }
}

#[test]
fn gpu_mesh_limit_spans_a_streamed_layers_batches() {
    // 72,900 triangles about a pixel across, streamed in batches that each mesh well under
    // MAX_LAYER_VERTICES but together pass it: the batches past the limit draw on the CPU.
    let mut ds = Dataset::new("dense", GeometryType::Polygon, vec![]);
    for i in 0..270 {
        for j in 0..270 {
            let (x, y) = (i as f64 * 0.01, j as f64 * 0.01);
            let ring = vec![Coord::new(x, y), Coord::new(x + 0.008, y), Coord::new(x, y + 0.008)];
            ds.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
        }
    }
    let mut map = Map::new("dense");
    map.add_dataset(ds);
    map.layers[0] = streamed(&map.layers[0], 72_900 / 8 + 1);
    let camera = Camera { center: lonlat_to_merc(Coord::new(1.35, 1.35)), res: 1000.0, bearing: 0.0, pitch: 0.0 };
    let view = View { width: 400.0, height: 400.0, pixels_per_point: 1.0, camera, sky: false };
    let Some(RenderOutput::Gpu(scene)) = render_backend(&mut map, &view, Backend::Gpu) else { panic!("gpu scene") };
    let meshed: Vec<usize> = scene
        .cmds
        .iter()
        .filter_map(|c| match c {
            gpu_scene::DrawCmd::Mesh { vertices, .. } => Some(vertices.len()),
            _ => None,
        })
        .collect();
    assert!(!meshed.is_empty(), "the first batches still mesh");
    assert!(meshed.iter().sum::<usize>() <= mesh::MAX_LAYER_VERTICES, "the layer's meshes: {meshed:?}");
    assert!(scene.cmds.iter().any(|c| matches!(c, gpu_scene::DrawCmd::Image { .. })), "the rest drawn on the CPU");
}

/// Counts full render traversals independently of interaction-window reads.
struct CountedLod {
    chunks: Chunks,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl FeatureSource for CountedLod {
    /// Preserves the source metadata used to choose the streamed render path.
    fn info(&self) -> &SourceInfo {
        self.chunks.info()
    }
    /// Interaction-window reads do not count as full render traversals.
    fn read(&self, request: &Request) -> rustgis_data::Result<Batch> {
        self.chunks.read(request)
    }
    /// Advertises the same complete traversal capability as the wrapped source.
    fn streams_chunks(&self) -> bool {
        true
    }
    /// Counts each complete render request, including failed traversals.
    fn visit_chunks(
        &self,
        extent: Option<Extent>,
        bytes: usize,
        visit: &mut dyn FnMut(Vec<Feature>) -> rustgis_data::Result<()>,
    ) -> rustgis_data::Result<Batch> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.chunks.visit_chunks(extent, bytes, visit)
    }
}

/// Wraps the whole dataset while leaving only its interaction sample on the layer.
fn counted_lod(layer: &Layer, size: usize) -> (Layer, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let ds = layer.dataset.clone();
    let mut layer = streamed(layer, size);
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let info = layer.source.as_ref().unwrap().info().unwrap().clone();
    let chunks = Chunks { info, ds, size };
    layer.source = Some(SourceLayer::from_path("counted.shp", None, Box::new(CountedLod { chunks, calls: calls.clone() })));
    (layer, calls)
}

/// Independent worker snapshots share both useful and ineffective thinning decisions while panning.
#[test]
fn streamed_lod_reuses_decisions_across_render_snapshots_and_pans() {
    use std::sync::atomic::Ordering;
    for detailed in [false, true] {
        let mut ds = Dataset::new("LOD visits", GeometryType::Polyline, vec![]);
        let n = if detailed { 210_001 } else { 101 };
        ds.push(Some(Geometry::Line(vec![(0..n).map(|i| Coord::new(i as f64 / n as f64 * 0.2 - 0.1, 0.0)).collect()])), vec![]);
        let mut base = Map::new("LOD visits");
        base.basemap = rustgis_map::Basemap::Blank;
        base.add_dataset(ds);
        let (layer, calls) = counted_lod(&base.layers[0], 1);
        let mut actual = base.clone();
        actual.layers[0] = layer;
        let mut view =
            View { width: 120.0, height: 90.0, pixels_per_point: 1.0, camera: Camera::new(Coord::new(0.0, 0.0), 80.0), sky: false };
        for frame in 0..3 {
            // A fresh worker snapshot each time must still reuse the source's decision.
            let got = render(&mut actual.clone(), &view).unwrap();
            let expected = render(&mut base.clone(), &view).unwrap();
            assert_eq!(got.pixmap.data(), expected.pixmap.data());
            assert_eq!(calls.load(Ordering::Relaxed), frame + 2, "detailed={detailed} frame={frame}");
            view.camera.center.x += 100.0;
        }
    }
}

/// Density and projection changes select separate decisions; refresh and replacement invalidate them.
#[test]
fn streamed_lod_separates_levels_projections_and_source_resets() {
    use std::sync::atomic::Ordering;
    let mut ds = Dataset::new("LOD invalidation", GeometryType::Polyline, vec![]);
    ds.push(Some(Geometry::Line(vec![vec![Coord::new(-0.1, 0.0), Coord::new(0.1, 0.0)]])), vec![]);
    let mut base = Map::new("LOD invalidation");
    base.basemap = rustgis_map::Basemap::Blank;
    base.add_dataset(ds);
    let (layer, calls) = counted_lod(&base.layers[0], 1);
    let mut actual = base.clone();
    actual.layers[0] = layer;
    let mut view = View { width: 120.0, height: 90.0, pixels_per_point: 1.0, camera: Camera::new(Coord::new(0.0, 0.0), 80.0), sky: false };
    let mut total = 0;
    // Resolutions 80 and 90 share a level. Density 2 selects the next finer level;
    // a different projection needs its own counts, even at the same tolerance.
    for (res, ppp, projection, visits) in [
        (80.0, 1.0, rustgis_map::Projection::default(), 2),
        (90.0, 1.0, rustgis_map::Projection::default(), 1),
        (80.0, 2.0, rustgis_map::Projection::default(), 2),
        (80.0, 2.0, rustgis_map::Projection::Custom("robinson".into()), 2),
        (80.0, 2.0, rustgis_map::Projection::default(), 1),
    ] {
        view.camera.res = res;
        view.pixels_per_point = ppp;
        base.projection = projection.clone();
        actual.projection = projection;
        let got = render(&mut actual, &view).unwrap();
        let expected = render(&mut base.clone(), &view).unwrap();
        assert_eq!(got.pixmap.data(), expected.pixmap.data());
        total += visits;
        assert_eq!(calls.load(Ordering::Relaxed), total);
    }
    // Re-evaluate after a refresh even if the open handle and projection stay the same.
    actual.layers[0].source.as_mut().unwrap().reset();
    assert!(render(&mut actual, &view).is_some());
    total += 2;
    assert_eq!(calls.load(Ordering::Relaxed), total);
    // A newly opened source at the same path must not inherit the old decision.
    let (replacement, replacement_calls) = counted_lod(&base.layers[0], 1);
    actual.layers[0].replace_source(replacement.source.unwrap());
    assert!(render(&mut actual, &view).is_some());
    assert_eq!(replacement_calls.load(Ordering::Relaxed), 2);
}

/// Failed fills stay retryable, negative results are cached, and LRU hits refresh recency.
#[test]
fn streamed_lod_retries_failed_decisions_and_bounds_retained_levels() {
    let mut ds = Dataset::new("cache", GeometryType::Polyline, vec![]);
    ds.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]])), vec![]);
    let (layer, _) = counted_lod(&Layer::new(1, ds), 1);
    let source = layer.source.as_ref().unwrap();
    for _ in 0..2 {
        assert!(source.cached_thinning("mercator", 0, || Err(rustgis_data::DataError::Format("incomplete".into()))).is_err());
    }
    assert!(!source.cached_thinning("mercator", 0, || Ok(false)).unwrap());
    assert!(!source.cached_thinning("mercator", 0, || panic!("negative decisions must be cached")).unwrap());
    for level in 1..=rustgis_map::lod::LEVELS_KEPT as i32 {
        assert!(source.cached_thinning("mercator", level, || Ok(true)).unwrap());
    }
    // The oldest negative entry was evicted; recomputation may now return a different value.
    assert!(source.cached_thinning("mercator", 0, || Ok(true)).unwrap());
    // Keep recently reused levels, rather than evicting in insertion order.
    assert!(source.cached_thinning("mercator", 2, || panic!("recent level should remain")).unwrap());
    assert!(source.cached_thinning("mercator", 4, || Ok(true)).unwrap());
    assert!(source.cached_thinning("mercator", 2, || panic!("hit must refresh recency")).unwrap());
}

/// A source panic must not disable caching for later successful frames in the same session.
#[test]
fn streamed_lod_recovers_caching_after_an_escaped_source_panic() {
    let ds = Dataset::new("panic recovery", GeometryType::Polyline, vec![]);
    let (layer, _) = counted_lod(&Layer::new(1, ds), 1);
    let source = layer.source.as_ref().unwrap();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        source.cached_thinning("mercator", 0, || panic!("injected source panic"))
    }));
    assert!(failed.is_err());
    assert!(source.cached_thinning("mercator", 0, || Ok(true)).unwrap());
    assert!(source.cached_thinning("mercator", 0, || panic!("recovered decision must be cached")).unwrap());
}

/// A streamed extruded layer's charts go over its extrusions, which its spool draws once every
/// chunk is read: none is covered by a roof (#1596).
#[test]
fn streamed_extrusions_keep_their_charts_on_top() {
    let magenta = Rgba::rgb(255, 0, 255);
    let mut ds = Dataset::new("blocks", GeometryType::Polygon, vec![Field::new("n", FieldType::Double)]);
    for i in 0..12 {
        let x = (i % 4) as f64 * 0.05 - 0.1;
        let y = (i / 4) as f64 * 0.05 - 0.07;
        ds.push(
            Some(Geometry::Polygon(vec![vec![vec![
                Coord::new(x, y),
                Coord::new(x + 0.03, y),
                Coord::new(x + 0.03, y + 0.03),
                Coord::new(x, y + 0.03),
                Coord::new(x, y),
            ]]])),
            vec![Value::Float(5.0)],
        );
    }
    let mut m = Map::new("test");
    m.basemap = rustgis_map::Basemap::Blank;
    m.add_dataset(ds);
    m.camera = Camera::new(rustgis_geom::lonlat_to_merc(Coord::new(-0.03, -0.02)), 60.0);
    let l = &mut m.layers[0];
    l.extrusion = Some(rustgis_map::effects::Extrusion {
        height: 1500.0,
        field: None,
        factor: 1.0,
        face_culling: true,
        lighting: true,
        shadows: false,
    });
    l.diagram = Some(rustgis_map::DiagramSettings {
        kind: rustgis_map::DiagramKind::Pie,
        categories: vec![rustgis_map::DiagramCategory { field: "n".into(), colour: magenta }],
        size: rustgis_map::DiagramSize::Fixed(8.0),
        start_angle: 0.0,
        background: None,
        stroke: rustgis_map::DiagramSettings::default_stroke(),
        stroke_width: 0.75,
        labels: rustgis_map::DiagramLabel::None,
    });
    let view = View { width: 320.0, height: 240.0, pixels_per_point: 1.0, camera: m.camera, sky: false };
    let magentas =
        |pm: &Pixmap| pm.pixels().iter().filter(|p| p.red() > 200 && p.green() < 60 && p.blue() > 200 && p.alpha() > 200).count();
    let whole = magentas(&render(&mut m.clone(), &view).unwrap().pixmap);
    assert!(whole > 100, "the resident layer's charts: {whole} px");
    for size in [1, 5] {
        let mut actual = m.clone();
        actual.layers[0] = streamed(&m.layers[0], size);
        let Some(RenderOutput::Cpu(got)) = render_with_backend(&mut actual, &view, true, Backend::Cpu) else {
            panic!("complete frame");
        };
        let n = magentas(&got.pixmap);
        assert!(n * 10 >= whole * 9, "chunks of {size}: {n} chart px, {whole} resident");
    }
}
