use super::*;
use crate::{Extent, View};
use rustgis_map::{Camera, Rgba};
use tiny_skia::Pixmap;

#[test]
fn cached_pixels_equal_sequential_strokes_including_overlap_and_alpha() {
    for ppp in [1.0f32, 1.5, 2.0] {
        let view = View {
            width: 600.0,
            height: 400.0,
            pixels_per_point: f64::from(ppp),
            camera: Camera { center: Coord::new(0.0, 0.0), res: 1.0, bearing: 0.0, pitch: 0.0 },
            sky: false,
        };
        let ctx = Ctx {
            view: &view,
            ppp,
            wireframe: false,
            cull: Extent::EMPTY,
            globe: None,
            sprites: Default::default(),
            bursts: Default::default(),
        };
        let mut expected = Pixmap::new((600.0 * ppp) as u32, (400.0 * ppp) as u32).unwrap();
        let mut actual = expected.clone();
        let symbol = Symbol::default_for(rustgis_data::GeometryType::Polyline, 0);
        for join in [rustgis_map::LineJoin::Round, rustgis_map::LineJoin::Miter, rustgis_map::LineJoin::Bevel] {
            for cap in [rustgis_map::LineCap::Round, rustgis_map::LineCap::Square, rustgis_map::LineCap::Butt] {
                expected.fill(tiny_skia::Color::WHITE);
                actual.fill(tiny_skia::Color::WHITE);
                let strokes = [
                    LineStroke {
                        offset: 2.0,
                        width: Some(3.0),
                        color: Some(Rgba::new(40, 70, 180, 150)),
                        join,
                        cap,
                        level: None,
                        dash: None,
                        markers: None,
                    },
                    LineStroke {
                        offset: -2.0,
                        width: Some(1.0),
                        color: Some(Rgba::new(220, 30, 40, 200)),
                        join,
                        cap,
                        level: None,
                        dash: None,
                        markers: None,
                    },
                    LineStroke {
                        offset: 2.0,
                        width: Some(1.0),
                        color: Some(Rgba::rgb(30, 40, 50)),
                        join,
                        cap,
                        level: None,
                        dash: None,
                        markers: None,
                    },
                ];
                let mut batch = Paths::default();
                for i in 0..180 {
                    let x = f64::from(i % 8) * 70.1 - 245.0;
                    let y = f64::from(i / 8) * 60.3 - 165.0;
                    let parts = vec![vec![Coord::new(x, y), Coord::new(x + 7.5, y + 0.25), Coord::new(x + 5.0, y + 9.0)]];
                    let mut styled = symbol;
                    if i % 11 == 0 {
                        styled.stroke_width = 5.0;
                    }
                    crate::canvas::with_canvas(&mut expected, |c| ctx.stacked_line(c, &parts, &styled, &strokes));
                    crate::canvas::with_canvas(&mut actual, |c| batch.draw(&ctx, c, &parts, &styled, &strokes));
                    // Repeated features verify unchanged overlap compositing order.
                    if i % 13 == 0 {
                        crate::canvas::with_canvas(&mut expected, |c| ctx.stacked_line(c, &parts, &styled, &strokes));
                        crate::canvas::with_canvas(&mut actual, |c| batch.draw(&ctx, c, &parts, &styled, &strokes));
                    }
                }
                let mismatch = actual.data().iter().zip(expected.data()).position(|(a, b)| a != b);
                assert!(mismatch.is_none(), "byte={mismatch:?} ppp={ppp} join={join:?} cap={cap:?}");
            }
        }
    }
}

#[test]
fn path_storage_is_reused_and_large_paths_are_not_retained() {
    let view = View {
        width: 600.0,
        height: 400.0,
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
    let mut image = Pixmap::new(600, 400).unwrap();
    let symbol = Symbol::default_for(rustgis_data::GeometryType::Polyline, 0);
    let strokes = [LineStroke { offset: 0.5, width: Some(0.5), ..Default::default() }];
    let mut paths = Paths::default();
    let mut allocation = None;
    for i in 0..200 {
        let x = f64::from(i % 20) * 10.0;
        crate::canvas::with_canvas(&mut image, |c| {
            paths.draw(&ctx, c, &[vec![Coord::new(x, 0.0), Coord::new(x + 1.0, 1.0)]], &symbol, &strokes)
        });
        assert!(paths.cached.as_ref().is_some_and(|path| path.points().len() <= CACHE_POINTS));
        let current = paths.cached.as_ref().unwrap().points().as_ptr();
        if let Some(previous) = allocation {
            assert_eq!(current, previous, "the next feature should reuse the point allocation");
        }
        allocation = Some(current);
    }
    let mut large = PathBuilder::new();
    large.move_to(0.0, 0.0);
    for i in 0..=CACHE_POINTS {
        large.line_to(i as f32, 1.0);
    }
    paths.remember(large.finish().unwrap());
    assert!(paths.cached.is_none(), "oversized buffers must be released after use");
    // Empty and invalid paths must neither replay stale geometry nor poison later reuse.
    for parts in [vec![], vec![vec![]], vec![vec![Coord::new(f64::NAN, 0.0)]]] {
        let before = image.data().to_vec();
        crate::canvas::with_canvas(&mut image, |c| paths.draw(&ctx, c, &parts, &symbol, &strokes));
        assert_eq!(image.data(), before);
        assert!(paths.cached.is_none());
    }
    crate::canvas::with_canvas(&mut image, |c| paths.draw(&ctx, c, &[vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]], &symbol, &strokes));
    assert!(paths.cached.is_some());
}
